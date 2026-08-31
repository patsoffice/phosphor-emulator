//! Sound targets: the seam between a scenario and a concrete device.
//!
//! One small adapter per sound device, rather than a universal "write register"
//! trait. A universal trait would erase the intent a scenario is written in
//! (`walk`, not `latch bit 0`) and still fail to cover the external DAC and PSG
//! streams these devices mix, which are not register writes at all.
//!
//! The adapter owns the board's timing. Donkey Kong advances one simulation
//! step per output sample and holds its DAC at silence; a Williams board would
//! translate elapsed time into CPU cycles instead. The scenario layer knows none
//! of that, and never imports a concrete machine type.

use crate::scenario::{Scenario, Value};

/// A control a scenario may address, by stable name.
pub struct ControlSpec {
    pub name: &'static str,
    pub description: &'static str,
}

/// A node inside the device that can be rendered on its own.
///
/// Per-voice solo render is the diagnostic that turns "our Donkey Kong sounds
/// wrong" into "our walk voice's decay is too fast". A mixed sum cannot say
/// which voice is at fault, and no amount of spectral analysis of the sum
/// recovers that — the voices overlap in frequency.
pub struct ProbeSpec {
    pub name: &'static str,
    pub description: &'static str,
}

/// A device that a scenario can drive.
pub trait SoundTarget {
    /// Output sample rate.
    fn sample_rate(&self) -> u32;
    /// Set a control by its stable name.
    fn set_control(&mut self, name: &str, value: Value) -> Result<(), String>;
    /// Advance one output sample and return it.
    ///
    /// One sample per call rather than a buffer, because a scenario's actions
    /// land on exact sample indices and a buffered API would quantise them to
    /// the buffer size.
    fn step(&mut self) -> i16;
}

/// Everything the CLI needs to know about a target without building one.
pub struct TargetSpec {
    pub id: &'static str,
    pub description: &'static str,
    pub controls: &'static [ControlSpec],
    pub probes: &'static [ProbeSpec],
    /// Build the device, optionally rendering one probe instead of the mix.
    pub create: CreateFn,
}

/// Construct a target, optionally rendering one probe node instead of the mix.
pub type CreateFn = fn(probe: Option<&str>) -> Result<Box<dyn SoundTarget>, String>;

/// Every registered target.
///
/// A hand-written list rather than an inventory registry: there are a handful of
/// these, they are only reachable from this binary, and a plain slice is what
/// makes the coverage tests trivially exhaustive.
static ALL: &[&TargetSpec] = &[
    &crate::targets::asteroids::SPEC,
    &crate::targets::dkong::SPEC,
    &crate::targets::dkongjr::SPEC,
    &crate::targets::galaxian::SPEC,
    &crate::targets::llander::SPEC,
    &crate::targets::mariobros::SPEC,
];

pub fn all() -> &'static [&'static TargetSpec] {
    ALL
}

pub fn find(id: &str) -> Option<&'static TargetSpec> {
    all().iter().copied().find(|t| t.id == id)
}

/// Run a scenario against a target and return the captured samples.
///
/// Actions land on exact sample indices, so a trigger edge is where the
/// scenario says it is regardless of any frame rate — which is what makes two
/// captures of the same scenario comparable in the first place.
pub fn run(scenario: &Scenario, target: &mut dyn SoundTarget) -> Result<Vec<i16>, String> {
    let rate = target.sample_rate() as f64;
    let total = (scenario.duration_s * rate) as usize;
    let mut out = Vec::with_capacity(total);
    let mut next_action = 0usize;

    for i in 0..total {
        let t = i as f64 / rate;
        // Apply every action whose time has arrived. Comparing against the
        // sample's own timestamp (rather than a running clock) keeps the edge
        // exact at any rate.
        while next_action < scenario.actions.len() && scenario.actions[next_action].at_s <= t {
            let a = &scenario.actions[next_action];
            target.set_control(&a.control, a.value)?;
            next_action += 1;
        }
        out.push(target.step());
    }
    Ok(out)
}

/// Trim a capture to a scenario's analysis window.
pub fn analysis_window(scenario: &Scenario, samples: &[i16], rate: u32) -> Vec<i16> {
    let idx = |t: f64| ((t * rate as f64) as usize).min(samples.len());
    samples[idx(scenario.analysis.start_s)..idx(scenario.analysis.end_s)].to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A target that records what it was told, so the runner's timing can be
    /// checked without a real device.
    struct Spy {
        rate: u32,
        sample: u64,
        events: Vec<(u64, String, bool)>,
    }

    impl SoundTarget for Spy {
        fn sample_rate(&self) -> u32 {
            self.rate
        }
        fn set_control(&mut self, name: &str, value: Value) -> Result<(), String> {
            self.events
                .push((self.sample, name.to_string(), value.as_bool()));
            Ok(())
        }
        fn step(&mut self) -> i16 {
            self.sample += 1;
            0
        }
    }

    fn scenario(src: &str) -> Scenario {
        let s: Scenario = toml::from_str(src).expect("parse");
        s.validate().expect("validate");
        s
    }

    const SRC: &str = r#"
schema = 1
id = "t/x"
target = "spy"
duration_s = 1.0

[[action]]
at_s = 0.25
control = "go"
value = true

[[action]]
at_s = 0.5
control = "go"
value = false

[analysis]
start_s = 0.2
end_s = 1.0
"#;

    /// The property that makes two captures comparable: a trigger lands on the
    /// sample the scenario names, not on a frame or buffer boundary.
    #[test]
    fn actions_land_on_exact_sample_indices() {
        let sc = scenario(SRC);
        let mut spy = Spy {
            rate: 1000,
            sample: 0,
            events: Vec::new(),
        };
        let out = run(&sc, &mut spy).expect("run");
        assert_eq!(out.len(), 1000);
        assert_eq!(spy.events.len(), 2);
        assert_eq!(spy.events[0], (250, "go".to_string(), true));
        assert_eq!(spy.events[1], (500, "go".to_string(), false));
    }

    /// The same scenario at a different rate puts the edges at the same
    /// *times*, which is the whole point of expressing them in seconds.
    #[test]
    fn edges_stay_at_the_same_time_across_rates() {
        let sc = scenario(SRC);
        for rate in [8_000u32, 44_100, 48_000] {
            let mut spy = Spy {
                rate,
                sample: 0,
                events: Vec::new(),
            };
            run(&sc, &mut spy).expect("run");
            let t = spy.events[0].0 as f64 / rate as f64;
            assert!((t - 0.25).abs() < 1.0 / rate as f64, "rate {rate} gave {t}");
        }
    }

    #[test]
    fn the_analysis_window_is_trimmed_to_the_declared_span() {
        let sc = scenario(SRC);
        let samples: Vec<i16> = (0..1000).map(|i| i as i16).collect();
        let w = analysis_window(&sc, &samples, 1000);
        assert_eq!(w.len(), 800);
        assert_eq!(w[0], 200);
    }

    #[test]
    fn an_unknown_control_is_an_error_not_a_silent_no_op() {
        let sc = scenario(&SRC.replace("control = \"go\"", "control = \"nope\""));
        struct Strict;
        impl SoundTarget for Strict {
            fn sample_rate(&self) -> u32 {
                1000
            }
            fn set_control(&mut self, name: &str, _v: Value) -> Result<(), String> {
                Err(format!("unknown control {name:?}"))
            }
            fn step(&mut self) -> i16 {
                0
            }
        }
        assert!(run(&sc, &mut Strict).is_err());
    }

    #[test]
    fn every_registered_target_has_controls_and_a_unique_id() {
        let mut seen = std::collections::HashSet::new();
        for t in all() {
            assert!(seen.insert(t.id), "duplicate target id {}", t.id);
            assert!(!t.controls.is_empty(), "{} declares no controls", t.id);
        }
    }
}
