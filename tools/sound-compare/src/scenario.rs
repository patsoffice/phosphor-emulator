//! Scenarios: what to do to a sound device, described as hardware intent.
//!
//! A scenario says "assert walk at 0.5 s, release it at 2.5 s, run for 3.5 s".
//! It does not say which register that is, which emulator method sets it, or
//! what MAME calls it. That separation is the point: the same file describes the
//! effect whether it is driven through this crate's adapter or produced by
//! someone else's capture rig, and it survives the device being refactored.
//!
//! # Why one file instead of three
//!
//! The rig this replaces kept the timeline in three places — a MAME Lua driver,
//! a Rust capture example, and the Python analyzer's segment list — with nothing
//! checking them against each other. A window that drifted in one of them
//! silently compared our explosion against MAME's thump. Here the timeline
//! exists once and the analysis window is part of it.
//!
//! # Isolated effects
//!
//! One scenario is one effect. Sustained effects get a clear assert and release;
//! one-shots are triggered *once* and allowed to decay completely. The old DK
//! captures re-pulsed their one-shots every 0.5 s, which is fine for listening
//! and useless for measurement: overlapping decays hide the envelope and the
//! integrated energy of a single event, so neither the decay nor the level of
//! one stomp can be recovered from them.

use serde::Deserialize;
use std::path::Path;

/// A control change at a point in time.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Action {
    /// When, in seconds from the start of the run.
    pub at_s: f64,
    /// Which control, by the adapter's stable name.
    pub control: String,
    /// The value to set. Booleans cover latch lines; a number covers a level.
    #[serde(default)]
    pub value: Value,
    /// This action puts the board into the state the measurement assumes,
    /// rather than being the stimulus under test.
    ///
    /// Some voices free-run. Galaxian's melody oscillator is always going, so
    /// isolating its fire or hit means parking the melody somewhere inaudible
    /// first — and that parking is not part of what is being measured. The
    /// distinction matters to `sndcmp verify`, whose null check removes the
    /// stimulus and requires silence: without it, any scenario on a board with a
    /// free-running voice could never pass, and the check would have to be
    /// weakened to the point of proving nothing.
    #[serde(default)]
    pub setup: bool,
}

/// A control value: a latch line or a level.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Value {
    Bool(bool),
    Number(f64),
}

impl Default for Value {
    fn default() -> Self {
        Self::Bool(true)
    }
}

impl Value {
    /// Interpret as a latch line. A number is on when non-zero, so a scenario
    /// may write either spelling.
    pub fn as_bool(self) -> bool {
        match self {
            Self::Bool(b) => b,
            Self::Number(n) => n != 0.0,
        }
    }

    pub fn as_f64(self) -> f64 {
        match self {
            Self::Bool(b) => {
                if b {
                    1.0
                } else {
                    0.0
                }
            }
            Self::Number(n) => n,
        }
    }
}

/// The window of a capture that describes the effect.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Analysis {
    /// Start of the measured window, seconds.
    pub start_s: f64,
    /// End of the measured window, seconds.
    pub end_s: f64,
}

/// One isolated effect.
#[derive(Debug, Clone, Deserialize)]
pub struct Scenario {
    /// Schema version, so a future change is a clear error rather than a
    /// silently misread field.
    pub schema: u32,
    /// Stable id, conventionally `<target>/<effect>`.
    pub id: String,
    /// Which adapter drives this.
    pub target: String,
    /// Total run length, seconds.
    pub duration_s: f64,
    /// What the effect is, for `sndcmp scenarios`.
    #[serde(default)]
    pub description: String,
    /// Timed control changes.
    #[serde(default, rename = "action")]
    pub actions: Vec<Action>,
    /// The window to measure.
    pub analysis: Analysis,
}

/// The schema version this build understands.
pub const SCHEMA: u32 = 1;

#[derive(Debug)]
pub enum ScenarioError {
    Io(String),
    Parse(String),
    Invalid(String),
}

impl std::fmt::Display for ScenarioError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(m) | Self::Parse(m) | Self::Invalid(m) => write!(f, "{m}"),
        }
    }
}

impl Scenario {
    /// Load and validate a scenario file.
    pub fn load(path: &Path) -> Result<Self, ScenarioError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| ScenarioError::Io(format!("reading {}: {e}", path.display())))?;
        let s: Scenario = toml::from_str(&text)
            .map_err(|e| ScenarioError::Parse(format!("parsing {}: {e}", path.display())))?;
        s.validate()
            .map_err(|e| ScenarioError::Invalid(format!("{}: {e}", path.display())))?;
        Ok(s)
    }

    /// Check the things that would otherwise produce a confidently wrong
    /// measurement rather than an error.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != SCHEMA {
            return Err(format!(
                "schema {} but this build understands {SCHEMA}",
                self.schema
            ));
        }
        if self.duration_s <= 0.0 {
            return Err("duration_s must be positive".into());
        }
        if self.analysis.start_s >= self.analysis.end_s {
            return Err(format!(
                "analysis window {}..{} is empty",
                self.analysis.start_s, self.analysis.end_s
            ));
        }
        // An analysis window past the end of the run would measure silence and
        // report it as the effect.
        if self.analysis.end_s > self.duration_s {
            return Err(format!(
                "analysis window ends at {} s but the run is only {} s",
                self.analysis.end_s, self.duration_s
            ));
        }
        for a in &self.actions {
            if a.at_s < 0.0 || a.at_s > self.duration_s {
                return Err(format!(
                    "action on {:?} at {} s falls outside the {} s run",
                    a.control, a.at_s, self.duration_s
                ));
            }
        }
        // Actions are applied in file order at their sample index, so an
        // out-of-order file would behave differently from how it reads.
        if self.actions.windows(2).any(|w| w[1].at_s < w[0].at_s) {
            return Err("actions must be listed in ascending at_s order".into());
        }
        Ok(())
    }

    /// Actions falling in `[from_s, to_s)`.
    #[cfg(test)]
    pub fn actions_at(&self, from_s: f64, to_s: f64) -> impl Iterator<Item = &Action> {
        self.actions
            .iter()
            .filter(move |a| a.at_s >= from_s && a.at_s < to_s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Scenario, String> {
        let sc: Scenario = toml::from_str(s).map_err(|e| e.to_string())?;
        sc.validate()?;
        Ok(sc)
    }

    const GOOD: &str = r#"
schema = 1
id = "dkong/stomp"
target = "dkong-discrete"
duration_s = 3.0
description = "one stomp, decaying fully"

[[action]]
at_s = 0.5
control = "stomp"
value = true

[[action]]
at_s = 0.55
control = "stomp"
value = false

[analysis]
start_s = 0.45
end_s = 3.0
"#;

    #[test]
    fn a_valid_scenario_round_trips() {
        let s = parse(GOOD).expect("should parse");
        assert_eq!(s.id, "dkong/stomp");
        assert_eq!(s.target, "dkong-discrete");
        assert_eq!(s.actions.len(), 2);
        assert_eq!(s.actions[0].value, Value::Bool(true));
        assert_eq!(s.analysis.end_s, 3.0);
    }

    /// A window past the end of the run would measure silence and report it as
    /// the effect — the quietest possible wrong answer.
    #[test]
    fn an_analysis_window_past_the_end_is_rejected() {
        let bad = GOOD.replace("end_s = 3.0", "end_s = 5.0");
        let err = parse(&bad).unwrap_err();
        assert!(err.contains("only 3"), "{err}");
    }

    #[test]
    fn an_empty_analysis_window_is_rejected() {
        let bad = GOOD.replace("start_s = 0.45", "start_s = 3.0");
        assert!(parse(&bad).unwrap_err().contains("empty"));
    }

    #[test]
    fn an_action_past_the_end_is_rejected() {
        let bad = GOOD.replace("at_s = 0.5\n", "at_s = 9.0\n");
        assert!(parse(&bad).unwrap_err().contains("outside"));
    }

    /// Actions are applied in order, so a file that reads one way and behaves
    /// another is a trap.
    #[test]
    fn out_of_order_actions_are_rejected() {
        let bad = GOOD.replace("at_s = 0.55", "at_s = 0.25");
        assert!(parse(&bad).unwrap_err().contains("ascending"));
    }

    #[test]
    fn a_future_schema_is_an_error_not_a_misread() {
        let bad = GOOD.replace("schema = 1", "schema = 2");
        assert!(parse(&bad).unwrap_err().contains("schema 2"));
    }

    /// Either spelling of a latch value works, so a scenario can say `true` or
    /// `1` without changing meaning.
    #[test]
    fn bool_and_number_values_agree() {
        assert!(Value::Bool(true).as_bool());
        assert!(Value::Number(1.0).as_bool());
        assert!(!Value::Number(0.0).as_bool());
        assert_eq!(Value::Bool(true).as_f64(), 1.0);
    }

    #[test]
    fn actions_are_selected_by_time_window() {
        let s = parse(GOOD).unwrap();
        let due: Vec<&Action> = s.actions_at(0.5, 0.52).collect();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].at_s, 0.5);
        assert_eq!(s.actions_at(0.0, 0.5).count(), 0);
        assert_eq!(s.actions_at(0.0, 1.0).count(), 2);
    }
}
