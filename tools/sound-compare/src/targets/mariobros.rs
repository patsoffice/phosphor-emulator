//! Mario Bros. (TMA1) discrete sound.
//!
//! Three voices and the music, mixed at one summing node. See
//! `machines/src/mario_sound.rs`.
//!
//! Two things here differ from the other targets and both come from the board.
//!
//! The footstep controls are STROBES, not levels: setting one to true fires a
//! footstep and there is nothing to set back. The scenarios therefore carry a
//! single action where the others carry a pair, and a `false` would be a second
//! footstep rather than a release.
//!
//! And the DAC is held at silence, as on the Donkey Kong targets, but here that
//! excludes rather more: this board's music passes two LM3900 sections into the
//! same summing node the voices use, so the filtering is in the circuit under
//! test even when the music is not.

use phosphor_core::device::DiscreteCircuit;
use phosphor_machines::mario_sound::MarioDiscreteSound;

use crate::scenario::Value;
use crate::target::{ControlSpec, ProbeSpec, SoundTarget, TargetSpec};

pub static SPEC: TargetSpec = TargetSpec {
    id: "mariobros-discrete",
    description: "Mario Bros. TMA1 discrete walk/walk/skid, DAC held silent",
    controls: &[
        ControlSpec {
            name: "walk1",
            description: "Mario footstep (STROBE: any write fires it; 0x7C00)",
        },
        ControlSpec {
            name: "walk2",
            description: "Luigi footstep (STROBE; 0x7C80)",
        },
        ControlSpec {
            name: "skid",
            description: "Skid enable (level; 0x7F07 bit 0)",
        },
        ControlSpec {
            name: "dac",
            description: "M58715 DAC level fed into the mix; 0 isolates the voices",
        },
    ],
    probes: &[
        ProbeSpec {
            name: "mix",
            description: "Final mix — the default, same as no probe",
        },
        ProbeSpec {
            name: "walk1",
            description: "Mario's voice alone, at its mixer leg",
        },
        ProbeSpec {
            name: "walk2",
            description: "Luigi's voice alone, at its mixer leg",
        },
        ProbeSpec {
            name: "skid",
            description: "Skid alone, at its mixer leg",
        },
        ProbeSpec {
            name: "dac",
            description: "The music after both LM3900 sections, without any voice",
        },
        ProbeSpec {
            name: "walk1-shot",
            description: "Mario's one-shot (0/1), which is the whole envelope",
        },
        ProbeSpec {
            name: "walk1-fc",
            description: "Mario's shared oscillator control node (volts)",
        },
        ProbeSpec {
            name: "walk1-osc-a",
            description: "Mario's 3.9 nF oscillator rate (Hz)",
        },
        ProbeSpec {
            name: "walk1-osc-b",
            description: "Mario's 22 nF oscillator rate (Hz)",
        },
        ProbeSpec {
            name: "skid-osc-a",
            description: "The skid's 22 nF oscillator rate (Hz)",
        },
        ProbeSpec {
            name: "skid-clock",
            description: "The skid's 4.7 nF counter-clock rate (Hz)",
        },
        ProbeSpec {
            name: "skid-count",
            description: "The 4020's count",
        },
    ],
    create,
};

fn create(probe: Option<&str>) -> Result<Box<dyn SoundTarget>, String> {
    if let Some(p) = probe
        && !SPEC.probes.iter().any(|s| s.name == p)
    {
        let names: Vec<&str> = SPEC.probes.iter().map(|s| s.name).collect();
        return Err(format!("unknown probe {p:?}; known: {}", names.join(", ")));
    }
    Ok(Box::new(MarioTarget {
        device: MarioDiscreteSound::new(),
        dac: 0,
        probe: probe.map(str::to_string),
        buf: vec![0i16; 8],
    }))
}

struct MarioTarget {
    device: MarioDiscreteSound,
    dac: i16,
    probe: Option<String>,
    buf: Vec<i16>,
}

impl SoundTarget for MarioTarget {
    fn sample_rate(&self) -> u32 {
        phosphor_core::audio::host_sample_rate()
    }

    fn set_control(&mut self, name: &str, value: Value) -> Result<(), String> {
        match name {
            "dac" => self.dac = value.as_f64() as i16,
            // A strobe: only the asserting edge is an event. A scenario that
            // wrote `false` here would be asking for a second footstep, so the
            // false case is dropped rather than passed on.
            "walk1" => {
                if value.as_bool() {
                    self.device.strobe_walk(0)
                }
            }
            "walk2" => {
                if value.as_bool() {
                    self.device.strobe_walk(1)
                }
            }
            "skid" => self.device.set_skid(value.as_bool()),
            other => {
                let names: Vec<&str> = SPEC.controls.iter().map(|c| c.name).collect();
                return Err(format!(
                    "unknown control {other:?} for mariobros-discrete; known: {}",
                    names.join(", ")
                ));
            }
        }
        Ok(())
    }

    fn step(&mut self) -> i16 {
        self.device.feed_dac(self.dac);
        let n = self.device.fill_audio(&mut self.buf);

        if let Some(p) = &self.probe
            && p != "mix"
            && let Some(v) = probe_value(self.device.circuit(), p)
        {
            return (v * i16::MAX as f64).clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        }
        if n > 0 { self.buf[0] } else { 0 }
    }
}

/// Read a named node out of the built circuit. The scale divides the node's
/// value before it becomes PCM: voice outputs carry circuit volts, and a rate
/// probe carries hertz, which needs a divisor that keeps the interesting range
/// on screen and is documented rather than meaningful.
fn probe_value(circuit: &DiscreteCircuit, probe: &str) -> Option<f64> {
    let (node, scale) = match probe {
        "walk1" => ("WALK1_OUT", 5.0),
        "walk2" => ("WALK2_OUT", 5.0),
        "skid" => ("SKID_OUT", 5.0),
        "dac" => ("DAC_OUT", 5.0),
        "walk1-shot" => ("WALK1_SHOT", 1.0),
        "walk1-fc" => ("WALK1_FC", 5.0),
        "walk1-osc-a" => ("WALK1_OSC_A", 131_072.0),
        "walk1-osc-b" => ("WALK1_OSC_B", 32_768.0),
        "skid-osc-a" => ("SKID_OSC_A", 32_768.0),
        "skid-clock" => ("SKID_OSC_B", 131_072.0),
        "skid-count" => ("COUNTER_3H", 16_384.0),
        _ => return None,
    };
    circuit
        .node_by_name(node)
        .map(|id| circuit.value(id) / scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_declared_controls_are_all_accepted() {
        let mut t = create(None).expect("create");
        for c in SPEC.controls {
            t.set_control(c.name, Value::Bool(true))
                .unwrap_or_else(|e| panic!("{} rejected: {e}", c.name));
        }
    }

    #[test]
    fn an_unknown_control_names_the_known_ones() {
        let mut t = create(None).expect("create");
        let err = t.set_control("wobble", Value::Bool(true)).unwrap_err();
        assert!(err.contains("walk1"), "{err}");
    }

    #[test]
    fn every_declared_probe_resolves_to_a_node() {
        let dev = MarioDiscreteSound::new();
        for p in SPEC.probes {
            if p.name == "mix" {
                continue;
            }
            assert!(
                probe_value(dev.circuit(), p.name).is_some(),
                "probe {:?} does not resolve to a circuit node",
                p.name
            );
        }
    }
}
