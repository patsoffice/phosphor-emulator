//! Asteroids (1979) discrete sound.
//!
//! Seven voices sum into one mono node: explosion, thrust, thump, saucer,
//! ship-fire, saucer-fire and life. The board reaches them through four write
//! addresses, so the controls here are named for the effects rather than for
//! `0x3600` and `0x3C00 + n`.
//!
//! WHY THIS EXISTS NOW. Every level in this device is a fitted constant. Thrust
//! and thump carry output gains whose comments say "calibrated against the
//! reference", the thump control voltage carries a 1.027 trim to "land the
//! reference pitch", both fire paths carry calibrated gains, and the saucer is
//! built from frequency literals rather than from its netlist at all. None of
//! those has a part behind it. Per the standing rule that a fitted value is the
//! usual disguise for a missing stage, they are all suspect, and the per-stage
//! probes below are what makes it possible to find out which stage each one is
//! standing in for.
//!
//! The starting symptom is thrust: its band centre matches the reference while
//! its low end does not, which is the "one metric agrees, another does not"
//! signature that has meant a missing mechanism every time.

use phosphor_core::device::DiscreteCircuit;
use phosphor_machines::asteroids_sound::AsteroidsDiscreteSound;
use phosphor_machines::atari_dvg::TIMING;

use crate::scenario::Value;
use crate::target::{ControlSpec, ProbeSpec, SoundTarget, TargetSpec};

pub static SPEC: TargetSpec = TargetSpec {
    id: "asteroids-discrete",
    description: "Asteroids discrete sound board (explosion, thrust, thump, saucer, fires, life)",
    controls: &[
        ControlSpec {
            name: "explosion-vol",
            description: "Explosion noise volume, 0-15 (0x3600 bits 2-5)",
        },
        ControlSpec {
            name: "explosion-pitch",
            description: "Explosion noise re-clock divider select, 0-3 (0x3600 bits 6-7)",
        },
        ControlSpec {
            name: "noise-reset",
            description: "Clear the explosion shift register (0x3E00); a pulse, not a level",
        },
        ControlSpec {
            name: "thump",
            description: "Thump enable (0x3A00 bit 4)",
        },
        ControlSpec {
            name: "thump-data",
            description: "Thump 4-bit DAC code, 0-15; higher is lower pitched (0x3A00 bits 0-3)",
        },
        ControlSpec {
            name: "saucer",
            description: "Saucer warble tone enable (latch line 0)",
        },
        ControlSpec {
            name: "saucer-fire",
            description: "Saucer fire chirp (latch line 1)",
        },
        ControlSpec {
            name: "saucer-sel",
            description: "Saucer size select: off is the small saucer, on the large (latch line 2)",
        },
        ControlSpec {
            name: "thrust",
            description: "Thrust rumble enable (latch line 3)",
        },
        ControlSpec {
            name: "ship-fire",
            description: "Ship fire chirp (latch line 4)",
        },
        ControlSpec {
            name: "life",
            description: "Extra-life 3 kHz tone enable (latch line 5)",
        },
    ],
    probes: &[
        ProbeSpec {
            name: "mix",
            description: "Final mix, the default, same as no probe",
        },
        // One per voice, at the point it enters the mixer.
        ProbeSpec {
            name: "explosion",
            description: "Explosion voice alone, at its mix level",
        },
        ProbeSpec {
            name: "thrust",
            description: "Thrust voice alone, at its mix level",
        },
        ProbeSpec {
            name: "thump",
            description: "Thump voice alone, at its mix level",
        },
        ProbeSpec {
            name: "saucer",
            description: "Saucer warble tone alone, at its mix level",
        },
        ProbeSpec {
            name: "ship-fire",
            description: "Ship fire chirp alone, at its mix level",
        },
        ProbeSpec {
            name: "saucer-fire",
            description: "Saucer fire chirp alone, at its mix level",
        },
        ProbeSpec {
            name: "life",
            description: "Extra-life tone alone, at its mix level",
        },
        // Thrust, stage by stage. This is the chain under investigation, and the
        // five stages all move the same bands at the output: the noise source,
        // its pre-filter, the gate, the resonant band-pass that sets the rumble,
        // and the output low-pass. An output comparison cannot separate them,
        // and it will accuse the last one.
        ProbeSpec {
            name: "thrust-noise",
            description: "Thrust 12 kHz shift-register noise, before any filtering (+/-1)",
        },
        ProbeSpec {
            name: "thrust-rc",
            description: "Thrust noise after its RC pre-filter (+/-1)",
        },
        ProbeSpec {
            name: "thrust-gate",
            description: "Thrust noise after the enable gate (+/-1)",
        },
        ProbeSpec {
            name: "thrust-bp",
            description: "Thrust resonant band-pass output, where the rumble is made (volts)",
        },
        ProbeSpec {
            name: "thrust-lp",
            description: "Thrust output low-pass, the last stage before its gain (volts)",
        },
        // Thump, stage by stage: a DAC voltage, the VCO it steers, and the two
        // couplings between that and the mixer.
        ProbeSpec {
            name: "thump-cv",
            description: "Thump DAC control voltage steering the VCO (volts)",
        },
        ProbeSpec {
            name: "thump-555",
            description: "Thump VCO square on pin 3, the board's tap (volts)",
        },
        ProbeSpec {
            name: "thump-rc",
            description: "Thump square after R74/C64's 482 Hz low-pass, before the gate (volts)",
        },
        // Explosion: the noise source and its one filter.
        ProbeSpec {
            name: "explosion-noise",
            description: "Explosion shift-register noise, scaled by volume (+/-1)",
        },
        ProbeSpec {
            name: "explosion-lp",
            description: "Explosion noise after its low-pass (+/-1)",
        },
        // Saucer: the tone and the warble that sweeps it, separately, because a
        // wrong warble rate and a wrong tone centre look alike in a spectrum.
        ProbeSpec {
            name: "saucer-tone",
            description: "Saucer tone oscillator, before its gate (+/-1)",
        },
        ProbeSpec {
            name: "saucer-lfo",
            description: "Saucer warble oscillator sweeping the tone (+/-1)",
        },
        // The two fire chains, stage by stage. Both are walked because the
        // pitch and the amplitude come from two separate capacitors on
        // different time constants, and at the output a slow sweep and a slow
        // decay are hard to tell apart.
        ProbeSpec {
            name: "ship-cv",
            description: "Ship fire pitch capacitor C47, rising 5 V to ~11 V (volts/12)",
        },
        ProbeSpec {
            name: "ship-555",
            description: "Ship fire 555 pin 3, the square CR8 reads (volts/5)",
        },
        ProbeSpec {
            name: "ship-node",
            description: "Ship fire summing node, pulled below +5 V (volts/5)",
        },
        ProbeSpec {
            name: "saucer-fire-cv",
            description: "Saucer fire pitch capacitor C38, rising 5 V to ~11 V (volts/12)",
        },
        ProbeSpec {
            name: "saucer-fire-555",
            description: "Saucer fire 555 pin 3, the square CR6 reads (volts/5)",
        },
        ProbeSpec {
            name: "saucer-fire-node",
            description: "Saucer fire summing node, pulled below +5 V (volts/5)",
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
    Ok(Box::new(AsteroidsTarget::new(probe)))
}

struct AsteroidsTarget {
    device: AsteroidsDiscreteSound,
    probe: Option<String>,
    /// The 0x3600 and 0x3A00 registers, held because each packs two controls
    /// and the device is written a whole byte at a time. A scenario that sets
    /// only the volume must not silently zero the pitch select.
    explosion_reg: u8,
    thump_reg: u8,
    cycles_per_sample: f64,
    cycles_owed: f64,
    buf: Vec<i16>,
    last: i16,
}

impl AsteroidsTarget {
    /// One construction site, so a test can hold a concrete target and check
    /// the clock conversion rather than restating it.
    fn new(probe: Option<&str>) -> Self {
        let device = AsteroidsDiscreteSound::new();
        let rate = device.sample_rate() as f64;
        Self {
            device,
            probe: probe.map(str::to_string),
            explosion_reg: 0,
            thump_reg: 0,
            // The device counts board CPU cycles, not samples, so one output
            // sample is a fractional number of them. Carrying the remainder
            // keeps a scenario's action times landing where they should rather
            // than drifting by a cycle per sample.
            cycles_per_sample: TIMING.cpu_clock_hz as f64 / rate,
            cycles_owed: 0.0,
            buf: vec![0i16; 8],
            last: 0,
        }
    }
}

impl SoundTarget for AsteroidsTarget {
    fn sample_rate(&self) -> u32 {
        self.device.sample_rate()
    }

    fn set_control(&mut self, name: &str, value: Value) -> Result<(), String> {
        // 74LS259 lines, as wired on the board. Line 6 and 7 are not audio.
        let latch = |n: &str| -> Option<u8> {
            match n {
                "saucer" => Some(0),
                "saucer-fire" => Some(1),
                "saucer-sel" => Some(2),
                "thrust" => Some(3),
                "ship-fire" => Some(4),
                "life" => Some(5),
                _ => None,
            }
        };
        match name {
            "explosion-vol" => {
                let v = (value.as_f64() as u8) & 0x0F;
                self.explosion_reg = (self.explosion_reg & 0xC0) | (v << 2);
                self.device.write_explosion(self.explosion_reg);
            }
            "explosion-pitch" => {
                let v = (value.as_f64() as u8) & 0x03;
                self.explosion_reg = (self.explosion_reg & 0x3F) | (v << 6);
                self.device.write_explosion(self.explosion_reg);
            }
            // A pulse, not a level: the board's write to 0x3E00 clears the
            // register and the address carries no data. Expressed as an action
            // whose value is ignored, so a scenario writes it once rather than
            // asserting and releasing something that was never a line.
            "noise-reset" => self.device.pulse_noise_reset(),
            "thump" => {
                self.thump_reg = (self.thump_reg & 0x0F) | if value.as_bool() { 0x10 } else { 0 };
                self.device.write_thump(self.thump_reg);
            }
            "thump-data" => {
                self.thump_reg = (self.thump_reg & 0x10) | ((value.as_f64() as u8) & 0x0F);
                self.device.write_thump(self.thump_reg);
            }
            other => match latch(other) {
                Some(line) => self.device.write_audio_latch_bit(line, value.as_bool()),
                None => {
                    let names: Vec<&str> = SPEC.controls.iter().map(|c| c.name).collect();
                    return Err(format!(
                        "unknown control {other:?} for asteroids-discrete; known: {}",
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
/// The scale divides the node's value before it is written as PCM. A voice
/// output is already at its mix level and needs none. A stage carrying circuit
/// VOLTS would clip a full-scale sample flat, and anything measured off a
/// clipped probe is the clipping and not the circuit, so those are divided by
/// the rail they swing against: 5 V for the 555 paths, 12 V for the thrust
/// band-pass, which is the rail it is actually built with.
fn probe_value(circuit: &DiscreteCircuit, probe: &str) -> Option<f64> {
    let (node, scale) = match probe {
        "explosion" => ("EXPLODE", 1.0),
        "thrust" => ("THRUST", 1.0),
        "thump" => ("THUMP", 1.0),
        "saucer" => ("SAUCER", 1.0),
        "ship-fire" => ("SHIP_FIRE_OUT", 1.0),
        "saucer-fire" => ("SAUCER_FIRE_OUT", 1.0),
        "life" => ("LIFE", 1.0),
        "thrust-noise" => ("THRUST_NOISE", 1.0),
        "thrust-rc" => ("THRUST_RC", 1.0),
        "thrust-gate" => ("THRUST_GATE", 1.0),
        "thrust-bp" => ("THRUST_BP", 12.0),
        "thrust-lp" => ("THRUST_LP", 12.0),
        "thump-cv" => ("THUMP_CV", 5.0),
        "thump-555" => ("THUMP_555", 5.0),
        "thump-rc" => ("THUMP_RC", 5.0),
        "explosion-noise" => ("EXPLODE_NOISE", 1.0),
        "explosion-lp" => ("EXPLODE_LP", 1.0),
        "saucer-tone" => ("SAUCER_TONE", 1.0),
        "saucer-lfo" => ("SAUCER_LFO", 1.0),
        // The fire chains, stage by stage. The control voltage swings over the
        // whole 5..11 V span between the analog switch and the source
        // transistor's saturation, so it is scaled by 12 rather than 5.
        "ship-cv" => ("SHIP_FIRE_CV", 12.0),
        "ship-555" => ("SHIP_FIRE_555", 5.0),
        "ship-node" => ("SHIP_FIRE_NODE", 5.0),
        "saucer-fire-cv" => ("SAUCER_FIRE_CV", 12.0),
        "saucer-fire-555" => ("SAUCER_FIRE_555", 5.0),
        "saucer-fire-node" => ("SAUCER_FIRE_NODE", 5.0),
        _ => return None,
    };
    circuit
        .node_by_name(node)
        .map(|id| circuit.value(id) / scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_target() -> AsteroidsTarget {
        AsteroidsTarget::new(None)
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
    fn an_unknown_control_names_the_known_ones() {
        let mut t = create(None).expect("create");
        let err = t.set_control("wobble", Value::Bool(true)).unwrap_err();
        assert!(err.contains("thrust"), "{err}");
    }

    #[test]
    fn an_unknown_probe_is_rejected_at_construction() {
        assert!(create(Some("nope")).is_err());
        assert!(create(Some("thrust-bp")).is_ok());
    }

    /// Every declared probe must resolve to a node. One that silently fell back
    /// to the mix would make every stage look identical, which is worse than
    /// having no probe at all.
    #[test]
    fn every_declared_probe_resolves_to_a_node() {
        let dev = AsteroidsDiscreteSound::new();
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

    /// The two packed registers must behave as separate controls. Writing the
    /// volume rebuilds the whole 0x3600 byte, so a naive implementation clears
    /// the pitch select and the scenario silently measures a different divider
    /// than the one it asked for.
    #[test]
    fn packed_register_controls_do_not_clear_each_other() {
        let mut t = new_target();
        t.set_control("explosion-pitch", Value::Number(2.0))
            .unwrap();
        t.set_control("explosion-vol", Value::Number(15.0)).unwrap();
        assert_eq!(t.explosion_reg, 0x80 | (0x0F << 2));

        t.set_control("thump-data", Value::Number(15.0)).unwrap();
        t.set_control("thump", Value::Bool(true)).unwrap();
        assert_eq!(t.thump_reg, 0x1F);
        t.set_control("thump", Value::Bool(false)).unwrap();
        assert_eq!(t.thump_reg, 0x0F, "releasing the enable ate the DAC code");
    }

    /// The device counts board cycles and the harness counts samples. If that
    /// conversion drifts, a scenario's action times drift with it.
    #[test]
    fn a_second_of_samples_is_a_second_of_board_time() {
        let mut t = new_target();
        let rate = t.sample_rate() as f64;
        for _ in 0..(rate as usize) {
            t.step();
        }
        assert!(
            t.cycles_owed < 1.0,
            "cycle remainder ran away: {}",
            t.cycles_owed
        );
    }

    /// The harness must advance the device at the rate the board advances it.
    ///
    /// Checked against the hardware number rather than against `TIMING`, which
    /// the device is also built from and which would therefore agree with any
    /// mistake made here. Asteroids divides a 12.096 MHz crystal by 8.
    #[test]
    fn the_harness_clocks_the_device_at_the_boards_rate() {
        let t = new_target();
        let per_second = t.cycles_per_sample * t.sample_rate() as f64;
        let board = 12_096_000.0 / 8.0;
        assert!(
            (per_second - board).abs() < 1.0,
            "harness runs the sound device at {per_second} Hz, board runs it at {board} Hz"
        );
    }

    /// The life tone is a 3 kHz literal in the circuit, so measuring it through
    /// the harness pins the harness's clock rather than the circuit's: stepping
    /// the device too slowly halves it. That is the error that shipped on
    /// Galaxian and made every voice read an octave low, and nothing about a
    /// capture looks wrong when it happens.
    #[test]
    fn the_life_tone_lands_on_its_declared_frequency() {
        let mut t = new_target();
        t.set_control("life", Value::Bool(true)).unwrap();
        t.probe = Some("life".to_string());

        let rate = t.sample_rate() as f64;
        // Settle first: the probe reads a live node and the first steps carry
        // the circuit's power-on transient.
        for _ in 0..(rate as usize / 10) {
            t.step();
        }
        let samples: Vec<f64> = (0..(rate as usize / 4)).map(|_| t.step() as f64).collect();

        // A gated square, so zero crossings of the mean-removed signal count its
        // period directly; there are no faster harmonics to be pulled onto the
        // way an autocorrelation would need guarding against.
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        let crossings = samples
            .windows(2)
            .filter(|w| (w[0] - mean) <= 0.0 && (w[1] - mean) > 0.0)
            .count();
        let measured = crossings as f64 * rate / samples.len() as f64;
        assert!(
            (measured / 3_000.0 - 1.0).abs() < 0.02,
            "life tone measured {measured:.1} Hz, the circuit declares 3000 Hz"
        );
    }
}
