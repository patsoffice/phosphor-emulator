//! `sndcmp` — drive a sound device through a scenario and capture the result.
//!
//! The harness half of the discrete-sound fidelity work
//! (`docs/designs/discrete-sound-fidelity.md`). It replaces the
//! `machines/examples/*_capture.rs` binaries and the timeline that used to be
//! duplicated across a MAME Lua driver, a Rust example and a Python analyzer.
//!
//! Comparison itself lives in `disasm audiodiff`, which already owns the metrics
//! and the WAV I/O; this produces the captures that go into it.
//!
//! ```text
//! sndcmp targets
//! sndcmp scenarios [TARGET]
//! sndcmp capture dkong/stomp --out /tmp/stomp.wav [--probe walk]
//! ```
//!
//! MAME stays outside this boundary on purpose: `sndcmp` knows nothing of Lua,
//! device tags or memory spaces, so the same scenario can be compared against a
//! MAME capture, a hardware recording, another emulator, or a previous revision
//! of this one.

mod scenario;
mod target;
mod targets;
mod verify;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use scenario::Scenario;

#[derive(Parser)]
#[command(
    name = "sndcmp",
    about = "Drive a discrete sound device through a scenario and capture it",
    long_about = None
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List the sound targets that can be driven, with their controls and probes.
    Targets,
    /// List the available scenarios, optionally for one target.
    Scenarios {
        /// Only scenarios for this target id.
        target: Option<String>,
    },
    /// Prove a scenario drives what it claims before trusting a comparison.
    ///
    /// Runs it with no actions (must be silent) and with the timeline moved
    /// 30 ms (must change). A scenario that passes neither is capturing
    /// something it does not drive.
    Verify {
        /// Scenario id (`dkong/stomp`) or a path to a scenario file. Omit to
        /// check every registered scenario.
        scenario: Option<String>,
    },
    /// Run a scenario and write a WAV.
    Capture {
        /// Scenario id (`dkong/stomp`) or a path to a scenario file.
        scenario: String,
        /// Output WAV path.
        #[arg(short, long)]
        out: PathBuf,
        /// Render one voice on its own instead of the mix.
        #[arg(long)]
        probe: Option<String>,
        /// Write the whole run rather than just the analysis window.
        #[arg(long)]
        full: bool,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("sndcmp: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run(cmd: Command) -> Result<String, String> {
    match cmd {
        Command::Targets => Ok(list_targets()),
        Command::Scenarios { target } => list_scenarios(target.as_deref()),
        Command::Verify { scenario } => verify_cmd(scenario.as_deref()),
        Command::Capture {
            scenario,
            out,
            probe,
            full,
        } => capture(&scenario, &out, probe.as_deref(), full),
    }
}

/// Verify one scenario, or every registered one.
///
/// Checking all of them is the useful default after touching a driver or an
/// adapter: the failure this guards against is silent, so it is only caught by
/// asking, and asking is cheap.
fn verify_cmd(id: Option<&str>) -> Result<String, String> {
    let ids: Vec<String> = match id {
        Some(one) => vec![one.to_string()],
        None => all_scenarios()?.into_iter().map(|(_, sc)| sc.id).collect(),
    };

    let mut out = String::new();
    let mut failures = 0usize;
    for id in &ids {
        match verify::verify(id, &resolve(id)) {
            Ok(report) => out.push_str(&report),
            Err(report) => {
                failures += 1;
                out.push_str(&report);
                out.push('\n');
            }
        }
    }

    if failures == 0 {
        Ok(out)
    } else {
        Err(format!(
            "{out}\n{failures} of {} scenarios failed verification",
            ids.len()
        ))
    }
}

fn list_targets() -> String {
    use std::fmt::Write;
    let mut s = String::new();
    for t in target::all() {
        let _ = writeln!(s, "{}\n  {}", t.id, t.description);
        let _ = writeln!(s, "  controls:");
        for c in t.controls {
            let _ = writeln!(s, "    {:<12} {}", c.name, c.description);
        }
        let _ = writeln!(s, "  probes:");
        for p in t.probes {
            let _ = writeln!(s, "    {:<12} {}", p.name, p.description);
        }
    }
    s
}

/// Where the committed scenarios live.
fn scenarios_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scenarios")
}

/// Resolve `dkong/stomp` to its file, or take a path as given.
fn resolve(id_or_path: &str) -> PathBuf {
    let direct = PathBuf::from(id_or_path);
    if direct.is_file() {
        return direct;
    }
    scenarios_dir().join(format!("{id_or_path}.toml"))
}

/// A scenario together with the file it came from.
type LoadedScenario = (PathBuf, Scenario);

/// Every scenario file under `scenarios/`, sorted by id.
fn all_scenarios() -> Result<Vec<LoadedScenario>, String> {
    let root = scenarios_dir();
    let mut found = Vec::new();
    let mut dirs = vec![root.clone()];
    while let Some(dir) = dirs.pop() {
        let entries =
            std::fs::read_dir(&dir).map_err(|e| format!("reading {}: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                dirs.push(p);
            } else if p.extension().is_some_and(|e| e == "toml") {
                let s = Scenario::load(&p).map_err(|e| e.to_string())?;
                found.push((p, s));
            }
        }
    }
    found.sort_by(|a, b| a.1.id.cmp(&b.1.id));
    Ok(found)
}

fn list_scenarios(target_filter: Option<&str>) -> Result<String, String> {
    use std::fmt::Write;
    let mut s = String::new();
    for (_, sc) in all_scenarios()? {
        if target_filter.is_some_and(|t| t != sc.target) {
            continue;
        }
        let _ = writeln!(
            s,
            "{:<16} {:<16} {:.1}s  analysis {:.2}-{:.2}s  {}",
            sc.id, sc.target, sc.duration_s, sc.analysis.start_s, sc.analysis.end_s, sc.description
        );
    }
    if s.is_empty() {
        s.push_str("no scenarios matched\n");
    }
    Ok(s)
}

fn capture(id: &str, out: &Path, probe: Option<&str>, full: bool) -> Result<String, String> {
    let path = resolve(id);
    let sc = Scenario::load(&path).map_err(|e| e.to_string())?;
    let spec = target::find(&sc.target).ok_or_else(|| {
        let known: Vec<&str> = target::all().iter().map(|t| t.id).collect();
        format!(
            "scenario {} names target {:?}, which is not registered; known: {}",
            sc.id,
            sc.target,
            known.join(", ")
        )
    })?;

    let mut device = (spec.create)(probe)?;
    let rate = device.sample_rate();
    let samples = target::run(&sc, device.as_mut())?;

    // Default to the analysis window: that is the span the scenario declares as
    // describing the effect, so a capture and a measurement cannot disagree
    // about which part of the run mattered.
    let written = if full {
        samples.clone()
    } else {
        target::analysis_window(&sc, &samples, rate)
    };

    write_wav(out, &written, rate).map_err(|e| format!("writing {}: {e}", out.display()))?;
    Ok(format!(
        "{} ({}{}) -> {} — {} samples at {rate} Hz, {:.3} s{}\n",
        sc.id,
        sc.target,
        probe.map(|p| format!(", probe {p}")).unwrap_or_default(),
        out.display(),
        written.len(),
        written.len() as f64 / rate as f64,
        if full {
            " (full run)"
        } else {
            " (analysis window)"
        }
    ))
}

/// Write 16-bit mono PCM as a WAV.
fn write_wav(path: &Path, samples: &[i16], rate: u32) -> std::io::Result<()> {
    use std::io::Write;
    let data_len = (samples.len() * 2) as u32;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 2).to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    f.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every committed scenario parses, validates, and names a real target and
    /// real controls. This is the check that keeps the scenario directory from
    /// drifting away from the adapters — the failure the old three-copy timeline
    /// had no way to catch.
    #[test]
    fn every_scenario_is_valid_and_addresses_its_target() {
        let scenarios = all_scenarios().expect("scenarios should load");
        assert!(!scenarios.is_empty(), "no scenarios found");
        for (path, sc) in scenarios {
            let spec = target::find(&sc.target).unwrap_or_else(|| {
                panic!(
                    "{}: target {:?} is not registered",
                    path.display(),
                    sc.target
                )
            });
            for a in &sc.actions {
                assert!(
                    spec.controls.iter().any(|c| c.name == a.control),
                    "{}: control {:?} is not one of {}'s",
                    path.display(),
                    a.control,
                    sc.target
                );
            }
        }
    }

    /// Scenario ids are how everything else refers to them, so a duplicate would
    /// make one of the two unreachable.
    #[test]
    fn scenario_ids_are_unique_and_match_their_path() {
        let mut seen = std::collections::HashSet::new();
        for (path, sc) in all_scenarios().expect("load") {
            assert!(
                seen.insert(sc.id.clone()),
                "duplicate scenario id {}",
                sc.id
            );
            assert_eq!(
                resolve(&sc.id).canonicalize().ok(),
                path.canonicalize().ok(),
                "scenario {} does not live at the path its id implies",
                sc.id
            );
        }
    }

    /// One-shot scenarios must trigger exactly once. Re-pulsing is what made the
    /// old Donkey Kong captures useless for measurement: overlapping decays hide
    /// the envelope and the integrated energy of a single event.
    #[test]
    fn one_shot_scenarios_trigger_exactly_once() {
        for (path, sc) in all_scenarios().expect("load") {
            for control in ["jump", "stomp"] {
                let rises = sc
                    .actions
                    .iter()
                    .filter(|a| a.control == control && a.value.as_bool())
                    .count();
                assert!(
                    rises <= 1,
                    "{}: {control} rises {rises} times; a one-shot must fire once so its \
                     decay can be measured",
                    path.display()
                );
            }
        }
    }
}
