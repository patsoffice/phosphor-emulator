//! Donkey Kong Jr. (TKG-04) discrete sound.
//!
//! Four voices where Donkey Kong has three, built from entirely different parts:
//! five 74LS629 oscillator halves, a 4020 ripple counter with an LS157 selecting
//! its taps, three LS123 one-shots and a 16-bit LFSR. See
//! `machines/src/dkongjr_sound.rs`.
//!
//! As with the Donkey Kong target, the device is driven directly with its DAC
//! held at silence. That is the isolation the measurement needs rather than a
//! simplification: the board box-filters the I8035 DAC into the same mix, so a
//! full-machine capture carries the music on top of the effects and no
//! per-effect measurement survives the sum.
//!
//! One control here has no Donkey Kong counterpart. `walk-pitch` is 6H bit 7,
//! which picks which pair of 4020 taps the walking voice uses, so walking has
//! two scenarios rather than one — the same trigger really does make two
//! different sounds, and a comparison against only one of them would miss half
//! the voice.

use phosphor_core::device::DiscreteCircuit;
use phosphor_machines::dkongjr_sound::DkongJrDiscreteSound;

use crate::scenario::Value;
use crate::target::{ControlSpec, ProbeSpec, SoundTarget, TargetSpec};

pub static SPEC: TargetSpec = TargetSpec {
    id: "dkongjr-discrete",
    description: "Donkey Kong Jr. TKG-04 discrete walk/jump/climb/fall, DAC held silent",
    controls: &[
        ControlSpec {
            name: "walk",
            description: "Walking one-shot (edge-triggered; 6H bit 0)",
        },
        ControlSpec {
            name: "jump",
            description: "Jump one-shot (edge-triggered; 6H bit 1)",
        },
        ControlSpec {
            name: "climb",
            description: "Climbing noise one-shot (edge-triggered; 6H bit 2)",
        },
        ControlSpec {
            name: "walk-pitch",
            description: "Walking counter-tap select (level; 6H bit 7)",
        },
        ControlSpec {
            name: "fall",
            description: "Falling enable (LEVEL, not a trigger; 5H bit 1)",
        },
        ControlSpec {
            name: "dac",
            description: "I8035 DAC level fed into the mix; 0 isolates the effects",
        },
        ControlSpec {
            name: "discharge",
            description: "DAC signal-decay line",
        },
    ],
    probes: &[
        ProbeSpec {
            name: "mix",
            description: "Final mix — the default, same as no probe",
        },
        ProbeSpec {
            name: "walk",
            description: "Walking voice alone, at the mixer leg",
        },
        ProbeSpec {
            name: "jump",
            description: "Jump voice alone, after its output low-pass",
        },
        ProbeSpec {
            name: "climb",
            description: "Climbing voice alone, after its output low-pass",
        },
        ProbeSpec {
            name: "fall",
            description: "Falling voice alone, at the mixer leg",
        },
        ProbeSpec {
            name: "dac",
            description: "Filtered DAC alone, without any effect",
        },
        // Stage probes. An output comparison blames the stage nearest the
        // output and cannot localise, which is why each voice's source, its
        // control node and its shaping are separately readable.
        ProbeSpec {
            name: "walk-shot",
            description: "Walking one-shot output (0/1)",
        },
        ProbeSpec {
            name: "walk-fc",
            description: "5K pin 7 control node, after its slew capacitor (volts)",
        },
        ProbeSpec {
            name: "walk-vco",
            description: "5K pin 7 oscillator rate (Hz)",
        },
        ProbeSpec {
            name: "walk-clock",
            description: "5K pin 10 counter-clock rate (Hz)",
        },
        ProbeSpec {
            name: "walk-count",
            description: "The 4020's count",
        },
        ProbeSpec {
            name: "jump-fc",
            description: "8L control node before the pin divider, the two legs mixed (volts)",
        },
        ProbeSpec {
            name: "jump-vco",
            description: "8L oscillator rate (Hz)",
        },
        ProbeSpec {
            name: "jump-q3",
            description: "Q3 collector, the chopped RC network (volts)",
        },
        ProbeSpec {
            name: "climb-noise",
            description: "LFSR output, the raw noise (0/1)",
        },
        ProbeSpec {
            name: "climb-clock",
            description: "7P pin 10 noise-clock rate (Hz)",
        },
        ProbeSpec {
            name: "climb-q2",
            description: "Q2 collector, the chopped RC network (volts)",
        },
        ProbeSpec {
            name: "fall-vco",
            description: "7P pin 7 oscillator rate (Hz), which sweeps while the enable is held",
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
    Ok(Box::new(DkongJrTarget {
        device: DkongJrDiscreteSound::new(),
        dac: 0,
        probe: probe.map(str::to_string),
        buf: vec![0i16; 8],
    }))
}

struct DkongJrTarget {
    device: DkongJrDiscreteSound,
    dac: i16,
    probe: Option<String>,
    buf: Vec<i16>,
}

impl SoundTarget for DkongJrTarget {
    fn sample_rate(&self) -> u32 {
        phosphor_core::audio::host_sample_rate()
    }

    fn set_control(&mut self, name: &str, value: Value) -> Result<(), String> {
        match name {
            "dac" => self.dac = value.as_f64() as i16,
            "discharge" => self.device.set_discharge(value.as_bool()),
            "walk" => self.device.write_sound_bit(0, value.as_bool()),
            "jump" => self.device.write_sound_bit(1, value.as_bool()),
            "climb" => self.device.write_sound_bit(2, value.as_bool()),
            "walk-pitch" => self.device.write_sound_bit(7, value.as_bool()),
            "fall" => self.device.write_latch_5h_bit(1, value.as_bool()),
            other => {
                let names: Vec<&str> = SPEC.controls.iter().map(|c| c.name).collect();
                return Err(format!(
                    "unknown control {other:?} for dkongjr-discrete; known: {}",
                    names.join(", ")
                ));
            }
        }
        Ok(())
    }

    fn step(&mut self) -> i16 {
        // One fed DAC sample is one output sample. The circuit runs several
        // simulation steps per sample internally, which is invisible here.
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

/// Read a named node out of the built circuit.
///
/// The scale divides the node's value before it is written as PCM. A voice
/// output is already in the mixer's volts and needs the supply's scale; a rate
/// probe carries hertz, which would saturate at any scale, so those are divided
/// by a round number that keeps the interesting range on screen and is
/// documented rather than meaningful.
fn probe_value(circuit: &DiscreteCircuit, probe: &str) -> Option<f64> {
    let (node, scale) = match probe {
        "walk" => ("WALK_OUT", 5.0),
        "jump" => ("JUMP_OUT", 5.0),
        "climb" => ("CLIMB_OUT", 5.0),
        "fall" => ("FALL_OUT", 5.0),
        "dac" => ("DAC_LP", 5.0),
        "walk-shot" => ("WALK_SHOT", 1.0),
        "walk-fc" => ("FC_5K_A", 5.0),
        "walk-vco" => ("VCO_5K_A", 20_000.0),
        "walk-clock" => ("VCO_5K_B", 65_536.0),
        "walk-count" => ("COUNTER_6L", 16_384.0),
        "jump-fc" => ("FC_8L_RAW", 5.0),
        "jump-vco" => ("VCO_8L", 2_048.0),
        "jump-q3" => ("Q3", 5.0),
        "climb-noise" => ("NOISE_3J_4J", 1.0),
        "climb-clock" => ("VCO_7P_B", 1_024.0),
        "climb-q2" => ("Q2", 5.0),
        "fall-vco" => ("VCO_7P_A", 4_096.0),
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
        assert!(err.contains("walk"), "{err}");
    }

    #[test]
    fn an_unknown_probe_is_rejected_at_construction() {
        assert!(create(Some("nope")).is_err());
        assert!(create(Some("climb")).is_ok());
    }

    /// Every declared probe must resolve to a node. A probe that silently fell
    /// back to the mix would be worse than no probe: it would make every voice
    /// look identical and confirm whatever it was pointed at.
    #[test]
    fn every_declared_probe_resolves_to_a_node() {
        let dev = DkongJrDiscreteSound::new();
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
