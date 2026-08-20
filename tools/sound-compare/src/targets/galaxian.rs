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

use phosphor_core::device::{DiscreteCircuit, GALAXIAN_CPU_CLOCK, GalaxianSound};

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
            name: "hit-rc",
            description: "Explosion's gated discharge, before the band-pass",
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
    Ok(Box::new(GalaxianTarget::new(probe)))
}

struct GalaxianTarget {
    device: GalaxianSound,
    probe: Option<String>,
    cycles_per_sample: f64,
    cycles_owed: f64,
    buf: Vec<i16>,
    last: i16,
}

impl GalaxianTarget {
    /// One construction site, so a test can hold a concrete target and check the
    /// clock conversion rather than restating it.
    fn new(probe: Option<&str>) -> Self {
        let device = GalaxianSound::new(phosphor_core::audio::host_sample_rate());
        let rate = device.sample_rate() as f64;
        Self {
            device,
            probe: probe.map(str::to_string),
            // The device counts main-CPU cycles, not samples, so one output
            // sample is a fractional number of them. Carrying the remainder
            // keeps the scenario's action times landing where they should
            // rather than drifting by a cycle per sample.
            //
            // MAIN-CPU cycles, not the 1.536 MHz sound clock the melody counter
            // divides. This read the sound clock, so every capture ran the board
            // at half speed and every voice measured an octave low with its
            // decay twice as long, which was the whole of what the first
            // Galaxian comparison reported.
            cycles_per_sample: GALAXIAN_CPU_CLOCK as f64 / rate,
            cycles_owed: 0.0,
            buf: vec![0i16; 8],
            last: 0,
        }
    }
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
///
/// The scale divides the node's value before it is written as PCM. These are all
/// circuit nodes carrying VOLTS, which run to the 5 V supply and would clip a
/// full-scale sample flat; only the logic-level noise line is already in range.
/// Without this every stage probe here rendered as a clipped square, so anything
/// measured off one was the clipping and not the circuit.
fn probe_value(circuit: &DiscreteCircuit, probe: &str) -> Option<f64> {
    let (node, scale) = match probe {
        "melody" => ("melody", 5.0),
        "background" => ("bg", 5.0),
        "hit" => ("hit", 5.0),
        "hit-rc" => ("hit_rc", 5.0),
        "fire" => ("fire_ac", 5.0),
        "noise" => ("noise01", 1.0),
        _ => return None,
    };
    circuit
        .node_by_name(node)
        .map(|id| circuit.value(id) / scale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::device::GALAXIAN_SOUND_CLOCK as SOUND_CLOCK;

    fn new_target() -> GalaxianTarget {
        GalaxianTarget::new(None)
    }

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
        let mut t = new_target();
        let rate = t.sample_rate() as f64;
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

    /// The harness must advance the device at the rate the *board* advances it.
    /// The board calls `sound.tick(1)` once per main-CPU cycle, so a second of
    /// harness stepping owes the device a second of `TIMING.cpu_clock_hz`.
    ///
    /// This existed only in the form above, which built its target from the same
    /// expression it was checking, so it passed while the harness was feeding
    /// the 1.536 MHz sound clock instead of the 3.072 MHz CPU clock and running
    /// every capture at half speed. Comparing against the machine's own timing
    /// constant is what makes it an actual check rather than a restatement.
    #[test]
    fn the_harness_clocks_the_device_at_the_boards_rate() {
        let t = new_target();
        let per_second = t.cycles_per_sample * t.sample_rate() as f64;
        let board = phosphor_machines::galaxian::TIMING.cpu_clock_hz as f64;
        assert!(
            (per_second - board).abs() < 1.0,
            "harness runs the sound device at {per_second} Hz, board runs it at {board} Hz"
        );
    }

    /// The melody's lowest tap is arithmetic off the schematic: the note clock
    /// is `SOUND_CLOCK / (256 - pitch)` and QD divides it by 16, so at pitch
    /// 0xB0 it is exactly 1200 Hz, and no part of that depends on the harness.
    /// Measuring it through the harness therefore pins the harness's clock:
    /// stepping the device too slowly halves this, which is exactly the error
    /// that shipped.
    #[test]
    fn the_melody_tap_lands_on_its_schematic_frequency() {
        let mut t = new_target();
        t.set_control("pitch", Value::Number(0xB0 as f64)).unwrap();
        t.set_control("vol1", Value::Bool(true)).unwrap();
        t.set_control("vol2", Value::Bool(true)).unwrap();
        t.probe = Some("melody".to_string());

        let rate = t.sample_rate() as f64;
        // Settle first: the probe reads a live node, and the first steps carry
        // the circuit's power-on transient.
        for _ in 0..(rate as usize / 10) {
            t.step();
        }
        let samples: Vec<f64> = (0..(rate as usize / 4)).map(|_| t.step() as f64).collect();

        // QA (4800 Hz) and QC (2400 Hz) are whole multiples of QD, so the summed
        // waveform repeats at QD's 1200 Hz. Autocorrelation finds that period
        // without being pulled to the faster taps the way zero crossings are.
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        let ac: Vec<f64> = samples.iter().map(|s| s - mean).collect();
        let lag_lo = (rate / 4000.0) as usize; // skip the trivial peak at 0
        let lag_hi = (rate / 300.0) as usize;
        let corr = |lag: usize| -> f64 {
            ac[..ac.len() - lag]
                .iter()
                .zip(&ac[lag..])
                .map(|(x, y)| x * y)
                .sum::<f64>()
        };
        let best = (lag_lo..lag_hi)
            .max_by(|&a, &b| corr(a).total_cmp(&corr(b)))
            .unwrap();
        let measured = rate / best as f64;
        let expected = SOUND_CLOCK / (256.0 - 0xB0 as f64) / 16.0; // 1200 Hz
        assert!(
            (measured / expected - 1.0).abs() < 0.02,
            "melody QD tap measured {measured:.1} Hz, schematic says {expected:.1} Hz"
        );
    }
}
