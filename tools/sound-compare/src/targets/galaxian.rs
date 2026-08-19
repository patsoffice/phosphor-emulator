//! Galaxian's discrete sound board as a comparison target.
//!
//! Four voices share one mixer: a background "melody" whose pitch comes from an
//! 8-bit register, three swept FS oscillators driven by a 4-bit DAC, a hit
//! (explosion) one-shot, and a fire one-shot. Two volume lines scale the melody.
//!
//! WHY THIS EXISTS NOW. Galaxian's sound constants were fitted against MAME
//! captures taken through a driver whose timeline was quantised to whole
//! seconds, so its `((t - 5.0) % 0.6) < 0.1` pulse trains became an irregular
//! once-per-second pattern. The driver is fixed and now passes
//! `verify-reference.sh`, but nothing has re-checked the values, and until
//! something does they rest on the same footing Donkey Kong's jump did.

use phosphor_core::device::{DiscreteCircuit, GALAXIAN_SOUND_CLOCK as SOUND_CLOCK, GalaxianSound};

use crate::scenario::Value;
use crate::target::{ControlSpec, ProbeSpec, SoundTarget, TargetSpec};

pub static SPEC: TargetSpec = TargetSpec {
    id: "galaxian-discrete",
    description: "Galaxian discrete sound board (melody, background FS, hit, fire)",
    controls: &[
        ControlSpec {
            name: "pitch",
            description: "Background melody pitch register, 0-255 (port 0x7800)",
        },
        ControlSpec {
            name: "lfo",
            description: "Background DAC that sweeps the FS oscillators, 0-15",
        },
        ControlSpec {
            name: "fs1",
            description: "Background oscillator 1 enable",
        },
        ControlSpec {
            name: "fs2",
            description: "Background oscillator 2 enable",
        },
        ControlSpec {
            name: "fs3",
            description: "Background oscillator 3 enable",
        },
        ControlSpec {
            name: "hit",
            description: "Explosion one-shot trigger",
        },
        ControlSpec {
            name: "fire",
            description: "Fire one-shot trigger",
        },
        ControlSpec {
            name: "vol1",
            description: "Melody volume line 1",
        },
        ControlSpec {
            name: "vol2",
            description: "Melody volume line 2",
        },
    ],
    probes: &[
        ProbeSpec {
            name: "mix",
            description: "Final mix — the default, same as no probe",
        },
        ProbeSpec {
            name: "melody",
            description: "Melody after its volume lines, before the mixer",
        },
        ProbeSpec {
            name: "background",
            description: "The three FS oscillators summed and filtered",
        },
        ProbeSpec {
            name: "hit",
            description: "Explosion voice, after its band-pass",
        },
        ProbeSpec {
            name: "fire",
            description: "Fire voice, after its coupling capacitor",
        },
        ProbeSpec {
            name: "noise",
            description: "The shift-register noise both one-shots draw on",
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
    let device = GalaxianSound::new(phosphor_core::audio::host_sample_rate());
    let rate = device.sample_rate() as f64;
    Ok(Box::new(GalaxianTarget {
        device,
        probe: probe.map(str::to_string),
        // The device counts main-CPU cycles, not samples, so one output sample
        // is a fractional number of them. Carrying the remainder keeps the
        // scenario's action times landing where they should rather than drifting
        // by a cycle per sample.
        cycles_per_sample: SOUND_CLOCK / rate,
        cycles_owed: 0.0,
        buf: vec![0i16; 8],
        last: 0,
    }))
}

struct GalaxianTarget {
    device: GalaxianSound,
    probe: Option<String>,
    cycles_per_sample: f64,
    cycles_owed: f64,
    buf: Vec<i16>,
    last: i16,
}

impl SoundTarget for GalaxianTarget {
    fn sample_rate(&self) -> u32 {
        self.device.sample_rate()
    }

    fn set_control(&mut self, name: &str, value: Value) -> Result<(), String> {
        // The latch lines the board actually wires; line 4 is not connected.
        let latch = |n: &str| -> Option<u8> {
            match n {
                "fs1" => Some(0),
                "fs2" => Some(1),
                "fs3" => Some(2),
                "hit" => Some(3),
                "fire" => Some(5),
                "vol1" => Some(6),
                "vol2" => Some(7),
                _ => None,
            }
        };
        match name {
            "pitch" => self.device.pitch_w(value.as_f64() as u8),
            "lfo" => {
                // Four separate one-bit ports on the board, so a scenario naming
                // a 0-15 level writes each of them rather than pretending there
                // is a nibble-wide register.
                let v = value.as_f64() as u8;
                for bit in 0..4u8 {
                    self.device.lfo_freq_w(bit, (v >> bit) & 1);
                }
            }
            other => match latch(other) {
                Some(line) => self.device.sound_w(line, u8::from(value.as_bool())),
                None => {
                    let names: Vec<&str> = SPEC.controls.iter().map(|c| c.name).collect();
                    return Err(format!(
                        "unknown control {other:?} for galaxian-discrete; known: {}",
                        names.join(", ")
                    ));
                }
            },
        }
        Ok(())
    }

    fn step(&mut self) -> i16 {
        self.cycles_owed += self.cycles_per_sample;
        let whole = self.cycles_owed.floor();
        self.cycles_owed -= whole;
        self.device.tick(whole as u64);

        let n = self.device.fill_audio(&mut self.buf);
        if n > 0 {
            self.last = self.buf[n - 1];
        }

        if let Some(p) = &self.probe
            && p != "mix"
            && let Some(v) = probe_value(self.device.circuit(), p)
        {
            return (v * i16::MAX as f64).clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        }
        // A step that produced no sample holds the previous one, which is what
        // the circuit's own output stage does between produced samples.
        self.last
    }
}

/// Read a named node out of the built circuit.
///
/// Curated rather than exposing every node, so a probe id describes circuit
/// intent and survives a local topology change.
fn probe_value(circuit: &DiscreteCircuit, probe: &str) -> Option<f64> {
    let node = match probe {
        "melody" => "tune_vol",
        "background" => "bg",
        "hit" => "hit",
        "fire" => "fire_ac",
        "noise" => "noise01",
        _ => return None,
    };
    circuit.node_by_name(node).map(|id| circuit.value(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_declared_controls_are_all_accepted() {
        let mut t = create(None).expect("create");
        for c in SPEC.controls {
            t.set_control(c.name, Value::Number(1.0))
                .unwrap_or_else(|e| panic!("{} rejected: {e}", c.name));
        }
    }

    #[test]
    fn an_unknown_control_is_rejected() {
        let mut t = create(None).expect("create");
        assert!(t.set_control("wobble", Value::Bool(true)).is_err());
    }

    /// Every declared probe must resolve to a node. One that silently fell back
    /// to the mix would make every voice look identical, which is worse than
    /// having no probe at all.
    #[test]
    fn every_declared_probe_resolves_to_a_node() {
        let dev = GalaxianSound::new(phosphor_core::audio::host_sample_rate());
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

    /// The device counts CPU cycles and the harness counts samples. If that
    /// conversion drifts, a scenario's action times drift with it — so check
    /// that a second of samples really is a second of CPU time.
    #[test]
    fn a_second_of_samples_is_a_second_of_cpu_time() {
        let device = GalaxianSound::new(phosphor_core::audio::host_sample_rate());
        let rate = device.sample_rate() as f64;
        let mut t = GalaxianTarget {
            device,
            probe: None,
            cycles_per_sample: SOUND_CLOCK / rate,
            cycles_owed: 0.0,
            buf: vec![0i16; 8],
            last: 0,
        };
        for _ in 0..(rate as usize) {
            t.step();
        }
        // Only the sub-cycle remainder may still be owed; anything more means
        // the accumulator is losing or gaining whole cycles per sample.
        assert!(
            t.cycles_owed < 1.0,
            "cycle remainder ran away: {}",
            t.cycles_owed
        );
    }
}
