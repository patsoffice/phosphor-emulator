//! Prove a scenario actually drives the thing it claims to drive.
//!
//! A capture that ignores its own timeline is worse than no capture, because it
//! looks like evidence. That is not hypothetical: every MAME reference driver in
//! this repo once read the clock as an integer-seconds field, so a 50 ms pulse
//! was really a one-second assertion and the release edge never happened. The
//! captures looked entirely reasonable — a plausible note, of plausible length,
//! starting when it should — and hours went into reconciling a model against a
//! control voltage the board never holds.
//!
//! What caught it was not analysis but two experiments on the reference itself,
//! and both are cheap enough to run every time:
//!
//! 1. **Null.** Run with every action removed. The analysis window must be
//!    silent. A scenario that sounds the same with nothing driving it is
//!    capturing something else — power-on noise, attract music, a neighbouring
//!    voice.
//! 2. **Sensitivity.** Move the timeline and confirm the capture changes.
//!    Identical output from two different stimuli means the stimulus is not
//!    reaching the circuit.
//!
//! The sensitivity perturbation is deliberately SUB-SECOND. The bug that
//! motivated this quantised everything to whole seconds, so a check that shifted
//! an action by a second would have passed happily while the defect sat
//! underneath it. Anything measuring in tens of milliseconds — which is most
//! arcade envelopes — needs a probe finer than the failure it is looking for.

use crate::scenario::Scenario;
use crate::target;

/// How far to move the timeline for the sensitivity check.
///
/// Small enough that a driver quantising to whole seconds cannot absorb it, and
/// large enough to change a capture whose envelopes run in tens of milliseconds.
const NUDGE_S: f64 = 0.030;

/// Root-mean-square of a capture, in dBFS. `-inf` is reported as -200.
fn rms_dbfs(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return -200.0;
    }
    let sum: f64 = samples
        .iter()
        .map(|s| {
            let v = *s as f64 / i16::MAX as f64;
            v * v
        })
        .sum();
    let rms = (sum / samples.len() as f64).sqrt();
    if rms <= 0.0 {
        -200.0
    } else {
        20.0 * rms.log10()
    }
}

/// Anything quieter than this in the null run counts as silence. Well below any
/// effect worth measuring, and above the numerical floor of an idle circuit that
/// still carries a DC level its couplings have not finished removing.
const SILENCE_DBFS: f64 = -70.0;

/// One check's outcome, kept separate from its formatting so the caller decides
/// how loud to be.
struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

/// Run both checks against a scenario. `Ok` iff every check passed.
pub fn verify(id: &str, path: &std::path::Path) -> Result<String, String> {
    use std::fmt::Write;

    let sc = Scenario::load(path).map_err(|e| e.to_string())?;
    let spec = target::find(&sc.target).ok_or_else(|| {
        let known: Vec<&str> = target::all().iter().map(|t| t.id).collect();
        format!(
            "scenario {} names target {:?}, which is not registered; known: {}",
            sc.id,
            sc.target,
            known.join(", ")
        )
    })?;

    let run = |s: &Scenario| -> Result<Vec<i16>, String> {
        let mut device = (spec.create)(None)?;
        let rate = device.sample_rate();
        let samples = target::run(s, device.as_mut())?;
        Ok(target::analysis_window(s, &samples, rate))
    };

    let baseline = run(&sc)?;
    let baseline_rms = rms_dbfs(&baseline);

    // --- Null: the same scenario with the STIMULUS removed. -----------------
    //
    // Setup actions stay. They put the board into the state the measurement
    // assumes — parking a free-running voice somewhere inaudible, typically —
    // and removing them would test whether the board is silent at reset, which
    // is a different and less useful question. What must be silent is the window
    // with nothing *triggered*.
    let mut null_sc = sc.clone();
    null_sc.actions.retain(|a| a.setup);
    let null = run(&null_sc)?;
    let null_rms = rms_dbfs(&null);
    let null_check = Check {
        name: "null",
        passed: null_rms < SILENCE_DBFS,
        detail: if null_rms < SILENCE_DBFS {
            format!("silent at {null_rms:.1} dBFS untriggered (driven: {baseline_rms:.1})")
        } else {
            format!(
                "NOT silent — {null_rms:.1} dBFS with nothing triggered, against {baseline_rms:.1} driven. \
                 Either the window captures something this scenario does not drive, or a \
                 free-running voice needs parking with a `setup = true` action."
            )
        },
    };

    // --- Sensitivity: move the timeline and require the capture to change. --
    //
    // Compared byte for byte rather than by level. Two runs differing only in
    // when a trigger fires should differ *somewhere*; identical output is proof
    // the trigger is not landing, and no summary statistic makes that as plain.
    let mut nudged_sc = sc.clone();
    for a in &mut nudged_sc.actions {
        a.at_s += NUDGE_S;
    }
    let nudged = run(&nudged_sc)?;
    let identical = nudged == baseline;
    let sens_check = Check {
        name: "sensitivity",
        passed: !identical,
        detail: if identical {
            format!(
                "IDENTICAL capture with every action moved {:.0} ms later. \
                 The stimulus is not reaching the circuit.",
                NUDGE_S * 1000.0
            )
        } else {
            format!(
                "capture changes when every action moves {:.0} ms later",
                NUDGE_S * 1000.0
            )
        },
    };

    let checks = [null_check, sens_check];
    let mut out = String::new();
    let _ = writeln!(out, "{} ({})", sc.id, sc.target);
    for c in &checks {
        let _ = writeln!(
            out,
            "  {} {:<12} {}",
            if c.passed { "ok  " } else { "FAIL" },
            c.name,
            c.detail
        );
    }

    if checks.iter().all(|c| c.passed) {
        Ok(out)
    } else {
        // The message carries the whole report: a failure here invalidates any
        // comparison made against this scenario, so it should be readable
        // without re-running anything.
        Err(format!(
            "{out}\n{id}: the scenario does not drive what it claims. \
             Any measurement taken against it is unverified."
        ))
    }
}
