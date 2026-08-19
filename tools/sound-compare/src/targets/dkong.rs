//! Donkey Kong (TKG-04) discrete sound.
//!
//! The first target, because its effects are the ones being corrected and it
//! exercises all three shapes: a sustained voice (walk), a pitched transient
//! (jump) and a noise transient (stomp).
//!
//! The device is driven directly with its DAC held at silence. That is not a
//! simplification — it is the isolation the measurement needs. `Tkg04Board`
//! box-filters the I8035 DAC and feeds each sample in, so a full-machine capture
//! carries the music on top of the effects and no per-effect measurement can be
//! recovered from the sum. Holding the DAC at zero exercises the same modelled
//! analog path with only the effect in it.
//!
//! The full machine is still needed to prove the board *drives* this correctly —
//! bus wiring, real pulse widths, effect-to-music balance — which is what the
//! ROM-gated tests are for. This is the level at which a component value can be
//! measured; that is the level at which the board can be.

use phosphor_core::device::DiscreteCircuit;
use phosphor_machines::dkong_sound::DkongDiscreteSound;

use crate::scenario::Value;
use crate::target::{ControlSpec, ProbeSpec, SoundTarget, TargetSpec};

pub static SPEC: TargetSpec = TargetSpec {
    id: "dkong-discrete",
    description: "Donkey Kong TKG-04 discrete walk/jump/stomp, DAC held silent",
    controls: &[
        ControlSpec {
            name: "walk",
            description: "Walk 555 VCO enable (sustained; latch bit 0)",
        },
        ControlSpec {
            name: "jump",
            description: "Jump pitch-sweep one-shot (edge-triggered; latch bit 1)",
        },
        ControlSpec {
            name: "stomp",
            description: "Stomp noise burst one-shot (edge-triggered; latch bit 2)",
        },
        ControlSpec {
            name: "dac",
            description: "I8035 DAC level fed into the mix; 0 isolates the effects",
        },
        ControlSpec {
            name: "discharge",
            description: "Effect discharge line",
        },
    ],
    probes: &[
        ProbeSpec {
            name: "walk",
            description: "Walk voice alone, after its output low-pass",
        },
        ProbeSpec {
            name: "jump",
            description: "Jump voice alone, after its output low-pass",
        },
        ProbeSpec {
            name: "stomp",
            description: "Stomp voice alone, after its output low-pass",
        },
        ProbeSpec {
            name: "dac",
            description: "Filtered DAC alone, without any effect",
        },
        ProbeSpec {
            name: "mix",
            description: "Final mix — the default, same as no probe",
        },
        // Stage probes along the jump chain. The final mix cannot say which
        // stage is wrong: the control voltage, the oscillator it drives, the
        // envelope that gates it and the follower that buffers it all move the
        // same bands, so a spectral difference at the output is consistent with
        // an error in any of them.
        ProbeSpec {
            name: "stomp-noise",
            description: "Stomp shift-register output, before the divider (±1)",
        },
        ProbeSpec {
            name: "stomp-div",
            description: "Stomp counter output: the divided rumble (0/1)",
        },
        ProbeSpec {
            name: "stomp-lid",
            description: "Stomp envelope capacitor, before its diode (volts)",
        },
        ProbeSpec {
            name: "stomp-mix",
            description: "Stomp diode-mixer node: envelope and rumble combined (volts)",
        },
        ProbeSpec {
            name: "stomp-int",
            description: "Stomp emitter follower output, before the divider (volts)",
        },
        ProbeSpec {
            name: "jump-cv",
            description: "Jump 555 control voltage, after the slew capacitor (volts)",
        },
        ProbeSpec {
            name: "jump-555",
            description: "Jump 555 square, before the envelope (volts)",
        },
        ProbeSpec {
            name: "jump-lid",
            description: "Jump envelope capacitor, before its diode (volts)",
        },
        ProbeSpec {
            name: "jump-mix",
            description: "Jump diode-mixer node: envelope and square combined (volts)",
        },
        ProbeSpec {
            name: "jump-int",
            description: "Jump emitter follower output, before the divider (volts)",
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
    Ok(Box::new(DkongTarget {
        device: DkongDiscreteSound::new(),
        dac: 0,
        probe: probe.map(str::to_string),
        buf: vec![0i16; 8],
    }))
}

struct DkongTarget {
    device: DkongDiscreteSound,
    dac: i16,
    probe: Option<String>,
    buf: Vec<i16>,
}

/// Latch bit for a control name.
fn bit_of(name: &str) -> Option<u8> {
    match name {
        "walk" => Some(0),
        "jump" => Some(1),
        "stomp" => Some(2),
        _ => None,
    }
}

impl SoundTarget for DkongTarget {
    fn sample_rate(&self) -> u32 {
        phosphor_core::audio::host_sample_rate()
    }

    fn set_control(&mut self, name: &str, value: Value) -> Result<(), String> {
        match name {
            "dac" => self.dac = value.as_f64() as i16,
            "discharge" => self.device.set_discharge(value.as_bool()),
            other => match bit_of(other) {
                Some(bit) => self.device.write_sound_bit(bit, value.as_bool()),
                None => {
                    let names: Vec<&str> = SPEC.controls.iter().map(|c| c.name).collect();
                    return Err(format!(
                        "unknown control {other:?} for dkong-discrete; known: {}",
                        names.join(", ")
                    ));
                }
            },
        }
        Ok(())
    }

    fn step(&mut self) -> i16 {
        // The device runs board = sim = output rate, so one fed DAC sample is
        // exactly one simulation step and one output sample.
        self.device.feed_dac(self.dac);
        let n = self.device.fill_audio(&mut self.buf);

        // A probe reads the named node's held value instead of the mix. The
        // circuit has already stepped, so this is that step's value.
        if let Some(p) = &self.probe
            && p != "mix"
            && let Some(v) = probe_value(self.device.circuit(), p)
        {
            // Nodes carry circuit-internal volts; scale to PCM the way the
            // output stage does so a probe and the mix are comparable.
            return (v * i16::MAX as f64).clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        }
        if n > 0 { self.buf[0] } else { 0 }
    }
}

/// Read a named node out of the built circuit.
///
/// Node names are the ones the circuit constructor assigned. Kept as a small
/// curated map rather than exposing every node, so a probe id describes circuit
/// intent and survives a local topology change.
/// The scale divides the node's value before it is written as PCM. Voice
/// outputs are already calibrated to the mix's range and need none; a stage
/// probe carries circuit volts, which run to the 5 V supply and would saturate
/// a full-scale sample, so those are scaled by the supply.
fn probe_value(circuit: &DiscreteCircuit, probe: &str) -> Option<f64> {
    let (node, scale) = match probe {
        "walk" => ("WALK_OUT", 1.0),
        "jump" => ("JUMP_OUT", 1.0),
        "stomp" => ("STOMP_OUT", 1.0),
        "dac" => ("DAC_LP", 1.0),
        "stomp-noise" => ("STOMP_NOISE", 1.0),
        "stomp-div" => ("STOMP_DIV", 1.0),
        "stomp-lid" => ("STOMP_LID", 5.0),
        "stomp-mix" => ("STOMP_MIX", 5.0),
        "stomp-int" => ("STOMP_INT", 5.0),
        "jump-cv" => ("JUMP_CV", 5.0),
        "jump-555" => ("JUMP_555", 5.0),
        "jump-lid" => ("JUMP_LID", 5.0),
        "jump-mix" => ("JUMP_MIX", 5.0),
        "jump-int" => ("JUMP_INT", 5.0),
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
        assert!(create(Some("stomp")).is_ok());
    }

    /// Every declared probe must actually resolve to a node. A probe that
    /// silently returned the mix would be worse than no probe at all — it would
    /// look like every voice was identical.
    #[test]
    fn every_declared_probe_resolves_to_a_node() {
        let t = create(None).expect("create");
        // Reach the circuit through a concrete target to read node names.
        let dev = DkongDiscreteSound::new();
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
        drop(t);
    }

    /// The point of solo render: one voice's capture must differ from another's.
    ///
    /// Measured as AC energy after letting the circuit settle, because neither
    /// assumption the obvious version makes holds on this board.
    ///
    /// An idle voice is not at zero: jump and stomp are diode-mixed under an
    /// envelope resting *above* their source, so each sits at a steady ~4 V that
    /// the amplifier's coupling capacitors remove much further downstream.
    /// Summing raw samples would call that silence loud.
    ///
    /// And an idle voice is not immediately quiet from reset: those envelope
    /// capacitors start discharged and take most of a second to charge past
    /// their source's peak, so every free-running source leaks until they do.
    /// That is the board's power-on noise, and it is why the capture scenarios
    /// carry a 2 s pre-roll. Measuring from reset measures the transient.
    #[test]
    fn probes_render_different_voices() {
        // Long enough for the slowest envelope (stomp's 363 ms, jump's 517 ms)
        // to close over its source.
        const SETTLE_STEPS: usize = 88_200;

        let render = |probe: &str, bit: u8| -> f64 {
            let mut t = create(Some(probe)).expect("create");
            for _ in 0..SETTLE_STEPS {
                t.step();
            }
            t.set_control(if bit == 0 { "walk" } else { "stomp" }, Value::Bool(true))
                .unwrap();
            let samples: Vec<f64> = (0..20_000).map(|_| t.step() as f64).collect();
            let mean = samples.iter().sum::<f64>() / samples.len() as f64;
            samples.iter().map(|s| (s - mean) * (s - mean)).sum()
        };
        // With only walk asserted, the walk probe carries energy and the stomp
        // probe carries none.
        let walk_on_walk = render("walk", 0);
        let stomp_on_walk = render("stomp", 0);
        assert!(
            walk_on_walk > stomp_on_walk * 10.0,
            "walk probe {walk_on_walk} vs stomp probe {stomp_on_walk} with only walk on"
        );
    }
}
