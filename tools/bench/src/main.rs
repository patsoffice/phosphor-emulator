//! Headless emulation throughput benchmark.
//!
//! Answers one question: how fast does a machine run when nothing is waiting on
//! it? Boots a registered machine through the shared [`Harness`], advances a
//! fixed number of frames with no window and no throttle, and reports the
//! wall-clock cost of each per-frame phase.
//!
//! This exists so performance claims are checkable. The interactive profiler
//! (`frontend/src/profile.rs`) measures one session on one machine and produces
//! nothing comparable across commits; this produces a number you can put in a
//! commit message and diff against later.
//!
//! ```text
//! cargo run --release -p phosphor-bench -- --roms ~/ws/mame-runtime/roms
//! cargo run --release -p phosphor-bench -- --machine galaga --frames 2000 --reps 5
//! ```
//!
//! ROM-gated: with no ROM directory it explains how to point at one and exits
//! 0, so it is safe to invoke from a script that may run without ROMs.

use std::path::Path;
use std::time::{Duration, Instant};

use clap::Parser;
use phosphor_harness::{Harness, roms_dir};

/// Boards chosen to span the shapes that cost different amounts:
/// a single-CPU raster board, a three-CPU board sharing one bus, a vector
/// board with a coprocessor, a 68000 board, and a two-CPU board with a
/// blitter and two address spaces.
const DEFAULT_MACHINES: &[&str] = &["pacman", "galaga", "tempest", "marble", "joust"];

/// Audio drain buffer, matching the frontend's own scratch size.
const AUDIO_SCRATCH: usize = 2048;

#[derive(Parser)]
#[command(
    about = "Measure headless emulation throughput for one or more machines",
    long_about = "Boots each machine, runs a fixed number of frames with no window and no \
                  throttle, and reports per-frame cost split into emulation, render, and \
                  audio.\n\n\
                  Numbers are only comparable when the build profile, machine, frame count, \
                  and warmup all match, so prefer changing one thing at a time. Run under \
                  --release; a debug build measures a different program."
)]
struct Args {
    /// Machine to benchmark; repeatable. Defaults to a representative set.
    #[arg(long = "machine", value_name = "NAME")]
    machines: Vec<String>,

    /// ROM set directory or ZIP. Defaults to $PHOSPHOR_ROMS, then
    /// ~/ws/mame-runtime/roms.
    #[arg(long, value_name = "PATH")]
    roms: Option<String>,

    /// Frames to measure per repetition.
    #[arg(long, default_value_t = 600)]
    frames: u64,

    /// Untimed frames run before measuring, to warm caches and branch
    /// predictors. Note this is far short of most machines' power-on
    /// self-test, so by default you are measuring self-test code rather than
    /// gameplay. That is fine for A/B comparison (both sides measure the same
    /// thing) but raise it if you want in-game numbers.
    #[arg(long, default_value_t = 120)]
    warmup: u64,

    /// Repetitions per machine. The fastest is reported, with how far the
    /// slowest lagged, so host noise is visible rather than hidden.
    #[arg(long, default_value_t = 5)]
    reps: usize,

    /// Skip the render phase (emulation and audio only).
    #[arg(long)]
    no_render: bool,
}

/// Per-frame wall-clock cost of one repetition, split by phase.
struct Rep {
    emulate: Duration,
    render: Duration,
    audio: Duration,
}

impl Rep {
    fn total(&self) -> Duration {
        self.emulate + self.render + self.audio
    }
}

/// Everything one machine's benchmark produced.
struct MachineResult {
    name: String,
    /// Native frame rate, for the realtime multiple.
    frame_rate_hz: f64,
    reps: Vec<Rep>,
}

impl MachineResult {
    /// Frames per second for a repetition, from its total per-frame cost.
    fn fps(&self, rep: &Rep, frames: u64) -> f64 {
        frames as f64 / rep.total().as_secs_f64()
    }

    /// Fastest rep, and the number reported.
    ///
    /// The minimum rather than the mean or median because the emulated workload
    /// is deterministic — there is no RNG and no wall clock in the tick path, so
    /// every rep executes exactly the same instructions. All run-to-run
    /// variation is therefore host noise (scheduling, frequency scaling, cache
    /// pressure from other processes), and noise can only ever *add* time. The
    /// fastest rep is the one least contaminated by it, which makes it both the
    /// most accurate estimate and the most stable one to compare across
    /// commits. [`spread_pct`](Self::spread_pct) is what tells you how noisy the
    /// sample was.
    fn best_rep(&self) -> &Rep {
        self.reps
            .iter()
            .min_by_key(|r| r.total())
            .expect("at least one rep")
    }

    /// How far the slowest rep fell behind the fastest, as a percentage. This is
    /// the host-noise indicator: near zero means a quiet machine and a
    /// trustworthy number; a large value means something else was competing for
    /// the CPU and the result deserves a re-run. A single rep has nothing to
    /// compare.
    fn spread_pct(&self, frames: u64) -> Option<f64> {
        if self.reps.len() < 2 {
            return None;
        }
        let best = self.fps(self.best_rep(), frames);
        let worst = self
            .reps
            .iter()
            .map(|r| self.fps(r, frames))
            .fold(f64::INFINITY, f64::min);
        Some((best - worst) / best * 100.0)
    }
}

/// Run one machine for `reps` repetitions, rebooting between them so each is
/// independent.
fn bench_machine(name: &str, roms: &Path, args: &Args) -> Result<MachineResult, String> {
    let roms = roms.to_str().ok_or("ROM path is not valid UTF-8")?;
    let mut reps = Vec::with_capacity(args.reps);
    let mut frame_rate_hz = 60.0;

    for _ in 0..args.reps {
        let mut harness = Harness::build(name, roms, None, None, &[], &[])?;
        let machine = harness.machine_mut();
        frame_rate_hz = machine.frame_rate_hz();

        let (w, h) = machine.display_size();
        let mut framebuffer = vec![0u8; w as usize * h as usize * 3];
        let mut audio = vec![0i16; AUDIO_SCRATCH];

        for _ in 0..args.warmup {
            harness.run_frame();
            // Drain during warmup too, or the resampler's backlog grows and the
            // first measured drain pays for every warmup frame at once.
            harness.machine_mut().fill_audio(&mut audio);
        }

        let mut emulate = Duration::ZERO;
        let mut render = Duration::ZERO;
        let mut audio_time = Duration::ZERO;

        for _ in 0..args.frames {
            let t0 = Instant::now();
            harness.run_frame();
            let t1 = Instant::now();

            if !args.no_render {
                harness.machine_mut().render_frame(&mut framebuffer);
            }
            let t2 = Instant::now();

            harness.machine_mut().fill_audio(&mut audio);
            let t3 = Instant::now();

            emulate += t1 - t0;
            render += t2 - t1;
            audio_time += t3 - t2;
        }

        reps.push(Rep {
            emulate,
            render,
            audio: audio_time,
        });
    }

    Ok(MachineResult {
        name: name.to_string(),
        frame_rate_hz,
        reps,
    })
}

/// Milliseconds per frame for a phase.
fn ms_per_frame(d: Duration, frames: u64) -> f64 {
    d.as_secs_f64() * 1000.0 / frames as f64
}

fn main() {
    let args = Args::parse();

    if cfg!(debug_assertions) {
        eprintln!(
            "warning: this is a debug build; the numbers below measure a different \
             program than the one you ship. Re-run with --release."
        );
    }

    let roms = match args.roms.as_deref() {
        Some(p) => std::path::PathBuf::from(p),
        None => match roms_dir() {
            Some(p) => p,
            None => {
                eprintln!(
                    "no ROM directory: pass --roms <path>, set PHOSPHOR_ROMS, or place \
                     ROMs in ~/ws/mame-runtime/roms"
                );
                return;
            }
        },
    };

    let machines: Vec<String> = if args.machines.is_empty() {
        DEFAULT_MACHINES.iter().map(|s| s.to_string()).collect()
    } else {
        args.machines.clone()
    };

    println!(
        "{} frames x {} reps, {} warmup, roms {}\n",
        args.frames,
        args.reps,
        args.warmup,
        roms.display()
    );
    println!(
        "{:<12} {:>10} {:>10} {:>10} {:>10} {:>10} {:>8}",
        "machine", "emul ms/f", "rend ms/f", "aud ms/f", "total ms/f", "fps", "x rt"
    );
    println!("{}", "-".repeat(76));

    let mut failures = Vec::new();

    for name in &machines {
        match bench_machine(name, &roms, &args) {
            Ok(result) => {
                let rep = result.best_rep();
                let f = args.frames;
                let fps = result.fps(rep, f);
                let spread = result
                    .spread_pct(f)
                    .map(|s| format!("+/-{s:.1}%"))
                    .unwrap_or_default();
                println!(
                    "{:<12} {:>10.3} {:>10.3} {:>10.3} {:>10.3} {:>10.1} {:>7.2}x  {}",
                    result.name,
                    ms_per_frame(rep.emulate, f),
                    ms_per_frame(rep.render, f),
                    ms_per_frame(rep.audio, f),
                    ms_per_frame(rep.total(), f),
                    fps,
                    fps / result.frame_rate_hz,
                    spread,
                );
            }
            Err(e) => {
                println!("{name:<12} {:>10}  {e}", "-");
                failures.push(name.clone());
            }
        }
    }

    if !failures.is_empty() {
        eprintln!(
            "\n{} machine(s) could not be benchmarked: {}",
            failures.len(),
            failures.join(", ")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(ms: u64) -> Rep {
        Rep {
            emulate: Duration::from_millis(ms),
            render: Duration::ZERO,
            audio: Duration::ZERO,
        }
    }

    fn result(mss: &[u64]) -> MachineResult {
        MachineResult {
            name: "test".into(),
            frame_rate_hz: 60.0,
            reps: mss.iter().map(|&m| rep(m)).collect(),
        }
    }

    #[test]
    fn best_rep_is_the_fastest_since_noise_only_adds_time() {
        let r = result(&[100, 10, 50]);
        assert_eq!(r.best_rep().total(), Duration::from_millis(10));
    }

    #[test]
    fn single_rep_has_no_spread_to_report() {
        assert!(result(&[42]).spread_pct(600).is_none());
    }

    #[test]
    fn spread_is_zero_when_every_rep_agrees() {
        let s = result(&[50, 50, 50]).spread_pct(600).unwrap();
        assert!(s.abs() < 1e-9, "expected no spread, got {s}");
    }

    #[test]
    fn spread_measures_how_far_the_slowest_rep_lagged_the_fastest() {
        // 50 ms best, 100 ms worst: the slow rep achieved half the fps, so it
        // lagged the best by 50%.
        let s = result(&[50, 100]).spread_pct(600).unwrap();
        assert!((s - 50.0).abs() < 1e-6, "expected 50%, got {s}");
    }

    #[test]
    fn spread_is_unaffected_by_rep_order() {
        let a = result(&[40, 50, 60]).spread_pct(600).unwrap();
        let b = result(&[60, 40, 50]).spread_pct(600).unwrap();
        assert!((a - b).abs() < 1e-9, "{a} vs {b}");
    }

    #[test]
    fn fps_is_derived_from_total_not_emulation_alone() {
        let r = MachineResult {
            name: "test".into(),
            frame_rate_hz: 60.0,
            reps: vec![Rep {
                emulate: Duration::from_millis(500),
                render: Duration::from_millis(300),
                audio: Duration::from_millis(200),
            }],
        };
        // 1000 ms total for 600 frames => 600 fps.
        let fps = r.fps(&r.reps[0], 600);
        assert!((fps - 600.0).abs() < 1e-6, "got {fps}");
    }

    #[test]
    fn ms_per_frame_divides_by_frame_count() {
        let v = ms_per_frame(Duration::from_millis(1200), 600);
        assert!((v - 2.0).abs() < 1e-9, "got {v}");
    }
}
