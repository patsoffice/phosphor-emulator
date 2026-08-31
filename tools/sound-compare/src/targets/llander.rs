//! Lunar Lander (1979) discrete sound.
//!
//! Four voices out of one board: a rocket thrust, a crash explosion, and two
//! fixed alert tones. They reach the board through a single write address, so
//! the controls here are named for the effects rather than for `0x3C00`'s bits.
//!
//! WHY THIS EXISTS NOW. This device has never been compared against anything.
//! It was transcribed from a netlist rather than built from the drawing, and it
//! carries three constants with no part behind them: `THRUST_IN_GAIN` 2400,
//! `THRUST_OUT_GAIN` 18.7, and an output divisor of 14347. Per the standing rule
//! that a fitted value is the usual disguise for a missing stage, all three are
//! suspect, and the per-stage probes below are what makes it possible to find
//! out which stage each is standing in for.
//!
//! The schematic is now transcribed at `docs/schematics/llander-audio-output.md`,
//! and it names two mechanisms this model does not have. Both are visible from
//! the probes here: the thrust volume and the noise low-pass corner are the SAME
//! three resistors, so quieter thrust is also darker thrust; and the two board
//! outputs are differential for thrust and explosion but single-ended for the
//! tones, so the tones sit 6 dB lower against the rest than a summed mix says.

use phosphor_core::device::DiscreteCircuit;
use phosphor_machines::atari_dvg::TIMING;
use phosphor_machines::llander_sound::LunarLanderDiscreteSound;

use crate::scenario::Value;
use crate::target::{ControlSpec, ProbeSpec, SoundTarget, TargetSpec};

pub static SPEC: TargetSpec = TargetSpec {
    id: "llander-discrete",
    description: "Lunar Lander discrete sound board (thrust, explosion, 3 kHz and 6 kHz tones)",
    controls: &[
        ControlSpec {
            name: "thrust-data",
            description: "Thrust volume, 0-7 (0x3C00 bits 0-2); also the explosion's volume",
        },
        ControlSpec {
            name: "explosion",
            description: "Explosion enable (0x3C00 bit 3); silent unless thrust-data is non-zero",
        },
        ControlSpec {
            name: "tone-3k",
            description: "3 kHz alert tone enable (0x3C00 bit 4)",
        },
        ControlSpec {
            name: "tone-6k",
            description: "6 kHz alert tone enable (0x3C00 bit 5)",
        },
        ControlSpec {
            name: "noise-reset",
            description: "Clear the noise shift register (0x3E00); a pulse, not a level",
        },
    ],
    probes: &[
        ProbeSpec {
            name: "mix",
            description: "Final mix, the default, same as no probe",
        },
        // One per voice, at the point it enters the mixer.
        ProbeSpec {
            name: "thrust",
            description: "Thrust voice alone, at its mix level",
        },
        ProbeSpec {
            name: "explosion",
            description: "Explosion voice alone, at its mix level",
        },
        ProbeSpec {
            name: "tone-3k",
            description: "3 kHz tone alone, at its mix level",
        },
        ProbeSpec {
            name: "tone-6k",
            description: "6 kHz tone alone, at its mix level",
        },
        ProbeSpec {
            name: "thrust-explod",
            description: "Thrust and explosion summed, after the shared 560 Hz low-pass",
        },
        // The thrust chain, stage by stage. Four stages all move the same bands
        // at the output -- the noise source, its pre-filter, the volume
        // multiply, and the resonant band-pass that makes the rumble -- so an
        // output comparison cannot separate them and will accuse the last one.
        ProbeSpec {
            name: "noise",
            description: "12 kHz shift-register noise, before any filtering (+/-1)",
        },
        ProbeSpec {
            name: "noise-rc",
            description: "Noise after its 71 Hz RC pre-filter (+/-1)",
        },
        ProbeSpec {
            name: "thrust-throttle",
            description: "Noise scaled by the throttle, the band-pass's input (+/-1)",
        },
        ProbeSpec {
            name: "thrust-bp",
            description: "Thrust resonant band-pass output, where the rumble is made (volts)",
        },
        ProbeSpec {
            name: "explosion-noise",
            description: "The explosion's unfiltered noise leg, before its gate",
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
    Ok(Box::new(LlanderTarget::new(probe)))
}

struct LlanderTarget {
    device: LunarLanderDiscreteSound,
    probe: Option<String>,
    /// The 0x3C00 register, held because all four level controls pack into one
    /// byte and the device is written a whole byte at a time. A scenario that
    /// enables the explosion must not silently zero the thrust volume it shares
    /// its level with.
    sound_reg: u8,
    cycles_per_sample: f64,
    cycles_owed: f64,
    buf: Vec<i16>,
    last: i16,
}

impl LlanderTarget {
    /// One construction site, so a test can hold a concrete target and check the
    /// clock conversion rather than restating it.
    fn new(probe: Option<&str>) -> Self {
        let device = LunarLanderDiscreteSound::new();
        let rate = device.sample_rate() as f64;
        Self {
            device,
            probe: probe.map(str::to_string),
            sound_reg: 0,
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

    /// Rewrite one field of the packed register and send the whole byte.
    fn write_field(&mut self, mask: u8, value: u8) {
        self.sound_reg = (self.sound_reg & !mask) | (value & mask);
        self.device.write_sound_register(self.sound_reg);
    }
}

impl SoundTarget for LlanderTarget {
    fn sample_rate(&self) -> u32 {
        self.device.sample_rate()
    }

    fn set_control(&mut self, name: &str, value: Value) -> Result<(), String> {
        match name {
            "thrust-data" => self.write_field(0x07, value.as_f64() as u8),
            "explosion" => self.write_field(0x08, if value.as_bool() { 0x08 } else { 0 }),
            "tone-3k" => self.write_field(0x10, if value.as_bool() { 0x10 } else { 0 }),
            "tone-6k" => self.write_field(0x20, if value.as_bool() { 0x20 } else { 0 }),
            // A pulse, not a level: the board's write to 0x3E00 clears the
            // register and the address carries no data. Expressed as an action
            // whose value is ignored, so a scenario writes it once rather than
            // asserting and releasing something that was never a line.
            "noise-reset" => self.device.pulse_noise_reset(),
            other => {
                let names: Vec<&str> = SPEC.controls.iter().map(|c| c.name).collect();
                return Err(format!(
                    "unknown control {other:?} for llander-discrete; known: {}",
                    names.join(", ")
                ));
            }
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
/// The scale divides the node's value before it is written as PCM. A stage
/// already at its mix level is divided by the mixer's own full scale, so a probe
/// reads at the share of the output that voice actually has; dividing by
/// anything else would measure the probe's normalization instead. The band-pass
/// carries circuit VOLTS and would clip a full-scale sample flat, so it is
/// divided by the rail it swings against.
fn probe_value(circuit: &DiscreteCircuit, probe: &str) -> Option<f64> {
    const MIX: f64 = LunarLanderDiscreteSound::MIX_FULL_SCALE;
    let (node, scale) = match probe {
        "thrust" => ("THRUST_PATH", MIX),
        "explosion" => ("EXPLOD_GATE", MIX),
        "tone-3k" => ("TONE3K_OUT", MIX),
        "tone-6k" => ("TONE6K_OUT", MIX),
        "thrust-explod" => ("THRUST_EXPLOD", MIX),
        "explosion-noise" => ("EXPLOD_SCALED", MIX),
        "noise" => ("NOISE", 1.0),
        "noise-rc" => ("NOISE_RC", 1.0),
        "thrust-throttle" => ("THRUST_THROTTLE", 1.0),
        "thrust-bp" => ("THRUST_BP", 12.0),
        _ => return None,
    };
    circuit
        .node_by_name(node)
        .map(|id| circuit.value(id) / scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_target() -> LlanderTarget {
        LlanderTarget::new(None)
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
        assert!(err.contains("thrust-data"), "{err}");
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
        let dev = LunarLanderDiscreteSound::new();
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

    /// All five controls share one byte, so a naive implementation that rebuilds
    /// the register per control silently clears the others. The explosion is the
    /// case that matters: it reuses the thrust value as its volume, so a scenario
    /// that set the volume and then enabled the explosion would measure silence.
    #[test]
    fn packed_register_controls_do_not_clear_each_other() {
        let mut t = new_target();
        t.set_control("thrust-data", Value::Number(7.0)).unwrap();
        t.set_control("explosion", Value::Bool(true)).unwrap();
        assert_eq!(t.sound_reg, 0x0F, "enabling the explosion ate the volume");

        t.set_control("tone-3k", Value::Bool(true)).unwrap();
        t.set_control("tone-6k", Value::Bool(true)).unwrap();
        assert_eq!(t.sound_reg, 0x3F);

        t.set_control("explosion", Value::Bool(false)).unwrap();
        assert_eq!(t.sound_reg, 0x37, "releasing the explosion ate a tone");

        t.set_control("thrust-data", Value::Number(0.0)).unwrap();
        assert_eq!(t.sound_reg, 0x30, "clearing the volume ate the tones");
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
    /// mistake made here. Lunar Lander shares Asteroids' board and divides a
    /// 12.096 MHz crystal by 8.
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

    /// Both alert tones are fixed-frequency squares in the circuit, so measuring
    /// them through the harness pins the HARNESS's clock rather than the
    /// circuit's: stepping the device too slowly halves them. That is the error
    /// that shipped on Galaxian and made every voice read an octave low, and
    /// nothing about a capture looks wrong when it happens.
    ///
    /// These two are also the only quantities on this board computable from the
    /// drawing alone, which is what makes them the right first check before any
    /// comparison is read as evidence about the circuit.
    #[test]
    fn the_alert_tones_land_on_their_declared_frequencies() {
        for (control, probe, declared) in [
            ("tone-3k", "tone-3k", 3_000.0),
            ("tone-6k", "tone-6k", 6_000.0),
        ] {
            let mut t = new_target();
            t.set_control(control, Value::Bool(true)).unwrap();
            t.probe = Some(probe.to_string());

            let rate = t.sample_rate() as f64;
            // Settle first: the probe reads a live node and the first steps
            // carry the circuit's power-on transient.
            for _ in 0..(rate as usize / 10) {
                t.step();
            }
            let samples: Vec<f64> = (0..(rate as usize / 4)).map(|_| t.step() as f64).collect();

            // A gated square, so zero crossings of the mean-removed signal count
            // its period directly; there are no faster harmonics to be pulled
            // onto the way an autocorrelation would need guarding against.
            let mean = samples.iter().sum::<f64>() / samples.len() as f64;
            let crossings = samples
                .windows(2)
                .filter(|w| (w[0] - mean) <= 0.0 && (w[1] - mean) > 0.0)
                .count();
            let measured = crossings as f64 * rate / samples.len() as f64;
            assert!(
                (measured / declared - 1.0).abs() < 0.02,
                "{control} measured {measured:.1} Hz, the circuit declares {declared} Hz"
            );
        }
    }

    /// The explosion takes its volume from the thrust field, which is the one
    /// piece of this board's wiring a scenario can get wrong and still produce a
    /// plausible-looking capture: enabling it with the volume at zero gives
    /// digital silence, not a quiet explosion.
    #[test]
    fn the_explosion_is_silent_without_a_thrust_value() {
        let quiet = {
            let mut t = new_target();
            t.set_control("explosion", Value::Bool(true)).unwrap();
            let rate = t.sample_rate() as usize;
            (0..rate / 4)
                .map(|_| t.step().unsigned_abs())
                .max()
                .unwrap()
        };
        let loud = {
            let mut t = new_target();
            t.set_control("thrust-data", Value::Number(7.0)).unwrap();
            t.set_control("explosion", Value::Bool(true)).unwrap();
            let rate = t.sample_rate() as usize;
            (0..rate / 4)
                .map(|_| t.step().unsigned_abs())
                .max()
                .unwrap()
        };
        assert!(
            loud > quiet * 4,
            "explosion peak with no thrust value {quiet}, with full thrust {loud}; \
             the board takes the explosion's volume from the thrust bits"
        );
    }
}
