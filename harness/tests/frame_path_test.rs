//! Frame-path equivalence: `run_frame()` versus `cycles_per_frame()` ticks of
//! `debug_tick()`.
//!
//! The take path advances whole frames through `run_frame()` while the
//! debugger path steps the same frame cycle-by-cycle. This suite boots each
//! machine once, snapshots it, then runs the same window three ways from
//! that snapshot: twice through `run_frame` (the determinism control) and
//! once through raw ticks with the harness's own scheduled-input framing.
//! The gate pins what the paths must share: the determinism control agrees,
//! and every CPU ends the window at the same program counter on both
//! paths, meaning both executed the same code to the same point.
//!
//! # What the gate does not compare
//!
//! `run_frame()` also does frame housekeeping that per-cycle stepping
//! deliberately does not: advancing and mixing sound, watchdog accounting,
//! clearing NMI edges, servicing pending sound-CPU resets. Those live in
//! `run_frame` by design (stepping one cycle must not mix a frame of audio
//! or pet the watchdog), so `save_state()` bytes and the drained audio
//! legitimately differ between the paths. The suite still measures both
//! diffs and prints them per machine, so a change in the scaffolding shows
//! up as a changed count; but only a program-counter divergence or a
//! nondeterministic control fails the run.
//!
//! A control mismatch (the two `run_frame` runs disagree) means the machine
//! is nondeterministic over this window, which fails loudly: re-running
//! the comparison cannot distinguish the paths there.
//!
//! # Gating
//!
//! ROM-gated like the other suites: no ROM directory means every machine
//! skips and the empty-registry guard keeps the test honest. One revision
//! per machine (the first whose archive is present) is enough: revisions
//! differ by ROM dump, while the frame scaffolding under test is machine
//! code shared across them.
//!
//! # Sensitivity
//!
//! `PHOSPHOR_FRAME_PATH_DIVERGE=1` drops one tick from the middle of the
//! tick path. The suite must fail with it set; that run is the proof the
//! comparison bites rather than compares two copies of the same bytes.

mod common;

use phosphor_harness::{Harness, roms_dir};
use phosphor_machines::registry;

/// Frames to run from power-on before snapshotting. Attract mode is not
/// required: both paths start from the same snapshot, so the window only
/// needs live CPUs, video, and sound.
const BOOT_FRAMES: usize = 600;

/// Frames compared per path from the snapshot.
const COMPARE_FRAMES: usize = 120;

/// Resolve the ROM directory, or announce the skip and return `None`.
fn roms() -> Option<std::path::PathBuf> {
    let dir = roms_dir();
    if dir.is_none() {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
    }
    dir
}

/// Boot the first revision of `machine` whose archive is present, or `None`.
fn boot_first_present(dir: &std::path::Path, machine: &str) -> Option<Harness> {
    let entry = registry::find(machine).unwrap_or_else(|| panic!("{machine} is not registered"));
    let name = entry
        .revisions
        .iter()
        .flat_map(|r| r.names.iter())
        .find(|n| dir.join(format!("{n}.zip")).exists())?;
    match Harness::build(
        machine,
        dir.to_str().unwrap(),
        Some(name),
        None,
        None,
        &[],
        &[],
    ) {
        Ok(harness) => Some(harness),
        Err(e) => {
            eprintln!("skipping {machine}: {e}");
            None
        }
    }
}

/// Run `COMPARE_FRAMES` through `Harness::run_frame`, draining audio.
fn run_framed(harness: &mut Harness, audio: &mut Vec<i16>) {
    let mut buf = [0i16; 8192];
    for _ in 0..COMPARE_FRAMES {
        harness.run_frame();
        let n = harness.machine_mut().fill_audio(&mut buf);
        audio.extend_from_slice(&buf[..n]);
    }
}

/// Run `COMPARE_FRAMES` cycle-by-cycle: the harness's own scheduled-input
/// framing around `cycles_per_frame()` ticks of `debug_tick()`.
///
/// With `PHOSPHOR_FRAME_PATH_DIVERGE=1`, drops the last tick of the middle
/// frame: a deliberate divergence the comparison must catch.
fn run_ticked(harness: &mut Harness, audio: &mut Vec<i16>) {
    let diverge = std::env::var_os("PHOSPHOR_FRAME_PATH_DIVERGE").is_some();
    let mut buf = [0i16; 8192];
    for frame in 0..COMPARE_FRAMES {
        harness.apply_scheduled_input();
        let mut ticks = harness.machine().cycles_per_frame();
        if diverge && frame == COMPARE_FRAMES / 2 {
            ticks = ticks.saturating_sub(1);
        }
        for _ in 0..ticks {
            harness.machine_mut().debug_tick();
        }
        harness.advance_frame();
        let n = harness.machine_mut().fill_audio(&mut buf);
        audio.extend_from_slice(&buf[..n]);
    }
}

/// The verdict for one machine: what matched, or why it was skipped.
///
/// `Match` carries the scaffolding diffs as diagnostics: they legitimately
/// differ (see above) but a change in either count means something moved.
enum Verdict {
    Match {
        state_bytes: usize,
        audio_samples: usize,
    },
    Nondeterministic,
    PcMismatch {
        framed: Vec<u32>,
        ticked: Vec<u32>,
    },
    Skipped(&'static str),
}

/// Elements differing between two runs (plus any length gap).
fn diff_count<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    a.iter().zip(b.iter()).filter(|(x, y)| x != y).count() + a.len().abs_diff(b.len())
}

/// Boot `machine`, snapshot it, run the window three ways, and compare.
fn check_machine(name: &str, dir: &std::path::Path) -> (String, Verdict) {
    let label = name.to_string();
    let Some(mut boot) = boot_first_present(dir, name) else {
        return (label, Verdict::Skipped("no ROM set"));
    };
    for _ in 0..BOOT_FRAMES {
        boot.run_frame();
    }
    if boot.machine().cycles_per_frame() == 0 {
        return (
            label,
            Verdict::Skipped("cycles_per_frame is 0: no tick path"),
        );
    }
    let cpus_visible = boot
        .machine()
        .debug_bus()
        .map(|bus| !bus.cpus().is_empty())
        .unwrap_or(false);
    if !cpus_visible {
        return (label, Verdict::Skipped("no CPUs visible on the debug bus"));
    }
    let Some(snap) = boot.machine().save_state() else {
        return (label, Verdict::Skipped("no save-state support"));
    };

    // End-of-window program counters per CPU, for the diagnosis: matching
    // PCs mean both paths executed the same code to the same point, and
    // whatever save-state bytes differ are peripheral, not execution.
    fn end_pcs(h: &Harness) -> Vec<u32> {
        h.machine()
            .debug_bus()
            .map(|bus| bus.cpus().iter().map(|(_, cpu)| cpu.debug_pc()).collect())
            .unwrap_or_default()
    }

    // Three runs from the identical snapshot: two framed (the determinism
    // control) and one ticked.
    let mut runs: Vec<(Vec<u8>, Vec<i16>, Vec<u32>)> = Vec::with_capacity(3);
    for ticked in [false, false, true] {
        let mut h = boot_first_present(dir, name).expect("reboot for a run");
        h.machine_mut()
            .load_state(&snap)
            .expect("snapshot produced seconds ago must load");
        let mut audio = Vec::new();
        if ticked {
            run_ticked(&mut h, &mut audio);
        } else {
            run_framed(&mut h, &mut audio);
        }
        runs.push((
            h.machine().save_state().expect("save-state support"),
            audio,
            end_pcs(&h),
        ));
    }

    if runs[0].0 != runs[1].0 || runs[0].1 != runs[1].1 {
        return (label, Verdict::Nondeterministic);
    }
    if runs[0].2 != runs[2].2 {
        return (
            label,
            Verdict::PcMismatch {
                framed: runs[0].2.clone(),
                ticked: runs[2].2.clone(),
            },
        );
    }
    (
        label,
        Verdict::Match {
            state_bytes: diff_count(&runs[0].0, &runs[2].0),
            audio_samples: diff_count(&runs[0].1, &runs[2].1),
        },
    )
}

/// Every machine's tick path agrees with its frame path over the window.
#[test]
fn frame_path_equivalence() {
    let Some(dir) = roms() else { return };

    let names: Vec<&str> = registry::all().iter().map(|e| e.name).collect();
    assert!(!names.is_empty(), "the_registry_is_not_empty");
    let results = common::map_parallel(&names, |&name| check_machine(name, &dir));

    let mut matched = Vec::new();
    let mut nondeterministic = Vec::new();
    let mut pc_mismatched = Vec::new();
    let mut skipped = Vec::new();
    for (label, verdict) in results {
        match verdict {
            Verdict::Match {
                state_bytes,
                audio_samples,
            } => {
                matched.push(format!(
                    "{label} ({state_bytes} state bytes, {audio_samples} audio samples differ)"
                ));
            }
            Verdict::Nondeterministic => nondeterministic.push(label),
            Verdict::PcMismatch { framed, ticked } => {
                pc_mismatched.push(format!(
                    "{label} (framed {framed:02X?}, ticked {ticked:02X?})"
                ));
            }
            Verdict::Skipped(why) => skipped.push(format!("{label}: {why}")),
        }
    }

    eprintln!(
        "frame-path: {} match, {} nondeterministic, {} PC mismatch, {} skipped",
        matched.len(),
        nondeterministic.len(),
        pc_mismatched.len(),
        skipped.len(),
    );
    for line in matched.iter().chain(skipped.iter()) {
        eprintln!("  {line}");
    }
    assert!(
        nondeterministic.is_empty(),
        "nondeterministic over the window (control runs disagree), so the paths \
         cannot be compared here: {nondeterministic:?}"
    );
    assert!(
        pc_mismatched.is_empty(),
        "CPUs end the window at different PCs on the two paths (file a bug per \
         machine with the diagnosis): {pc_mismatched:?}"
    );
    assert!(
        !matched.is_empty(),
        "the ROM directory {} exists but no machine could be compared — this \
         test would otherwise pass having checked nothing",
        dir.display()
    );
}
