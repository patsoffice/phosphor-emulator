//! Zaxxon (Sega IC Board A 834-0214) discrete sound.
//!
//! Eleven analog voices gated by twelve active-low bits of the i8255 at `U23`,
//! meeting at one passive summing node. See `machines/src/zaxxon_sound.rs` and
//! the transcription in `docs/schematics/zaxxon-discrete-sound.md`.
//!
//! **There is no reference to compare a capture against**, and registering this
//! adapter does not claim one. The reference emulator plays recorded WAV samples
//! for this board, so a comparison against it would measure whoever made the
//! recordings rather than the circuit. What these scenarios are for is the other
//! comparison `sndcmp` supports and this board badly needs: **one revision of
//! this device against the next**. Every correction to this file so far has been
//! a topology read off the sheet, and the way to tell a correction from a
//! regression is to capture the same voice before and after and listen to both.
//!
//! Two things here differ from the other targets.
//!
//! The gate bits are **active low**, and the board's pull-ups hold every one of
//! them high at power-on. A scenario therefore writes `true` to *ask for* a
//! voice and this adapter inverts; `false` is the resting state, which is how
//! the board sits when the program has written nothing.
//!
//! And `ship-level` is not a gate at all. `PA0` and `PA1` drive a resistor
//! ladder through two 7406 sections, so they set a two-bit level whose code is
//! `3 - (PA0*2 + PA1)`. The control takes that code, 0 to 3, with 3 the loudest,
//! and this adapter turns it back into the two bits so a scenario never has to
//! know that the board runs the pair backwards.

use phosphor_core::device::DiscreteCircuit;
use phosphor_machines::sega_zaxxon::TIMING;
use phosphor_machines::zaxxon_sound::ZaxxonSound;

use crate::scenario::Value;
use crate::target::{ControlSpec, ProbeSpec, SoundTarget, TargetSpec};

pub static SPEC: TargetSpec = TargetSpec {
    id: "zaxxon-discrete",
    description: "Zaxxon 834-0214 discrete sound: eleven voices on one summing node",
    controls: &[
        ControlSpec {
            name: "ship-level",
            description: "Engine level 0-3 from PA0/PA1's ladder, 3 loudest (not a gate)",
        },
        ControlSpec {
            name: "ship-tone-a",
            description: "Engine tone A, 723 Hz Sallen-Key (U32 Y0: PA2 and PA3 both low)",
        },
        ControlSpec {
            name: "ship-tone-b",
            description: "Engine tone B, 482 Hz Sallen-Key (U32 Y1: PA2 high, PA3 low)",
        },
        ControlSpec {
            name: "homing-missile",
            description: "Homing missile (PA4): a 555 swept by noise plus an envelope",
        },
        ControlSpec {
            name: "base-missile",
            description: "Base missile (PA5): 151 ms one-shot into a 482 Hz noise band",
        },
        ControlSpec {
            name: "laser",
            description: "Laser / force field (PA6): a 5.31 Hz repeat gating a tone",
        },
        ControlSpec {
            name: "battleship",
            description: "Battleship (PA7): a 122 Hz square through a 4016B switch",
        },
        ControlSpec {
            name: "s-exp",
            description: "Small (enemy) explosion (PB4): 10 ms one-shot, 321 Hz noise band",
        },
        ControlSpec {
            name: "m-exp",
            description: "Medium (ship) explosion (PB5): 43 ms one-shot, 226 Hz noise band",
        },
        ControlSpec {
            name: "cannon",
            description: "Player's gun (PB7): a bridged-T sweeping 7.3 kHz down to 1.8 kHz",
        },
        ControlSpec {
            name: "shot",
            description: "Enemy fire (PC0): an 11 ms one-shot into the U18 555 chain",
        },
        ControlSpec {
            name: "alarm2",
            description: "Alarm 2, target lock (PC2): 132 ms of 1QD's 1268 Hz",
        },
        ControlSpec {
            name: "alarm3",
            description: "Alarm 3, low fuel (PC3): 132 ms of 1QC's 2535 Hz",
        },
    ],
    probes: &[
        ProbeSpec {
            name: "mix",
            description: "Final mix at the speaker: the default, same as no probe",
        },
        ProbeSpec {
            name: "ship-a",
            description: "Engine tone A alone, at its mixer leg",
        },
        ProbeSpec {
            name: "ship-b",
            description: "Engine tone B alone, at its mixer leg",
        },
        ProbeSpec {
            name: "homing-missile",
            description: "Homing missile alone, at its mixer leg",
        },
        ProbeSpec {
            name: "base-missile",
            description: "Base missile alone, at its mixer leg",
        },
        ProbeSpec {
            name: "laser",
            description: "Laser alone, at its mixer leg",
        },
        ProbeSpec {
            name: "battleship",
            description: "Battleship alone, at its mixer leg",
        },
        ProbeSpec {
            name: "s-exp",
            description: "Small explosion alone, at its mixer leg",
        },
        ProbeSpec {
            name: "m-exp",
            description: "Medium explosion alone, at its mixer leg",
        },
        ProbeSpec {
            name: "cannon",
            description: "Cannon alone, at its mixer leg",
        },
        ProbeSpec {
            name: "shot",
            description: "Shot alone, at its mixer leg",
        },
        ProbeSpec {
            name: "alarms",
            description: "Both alarms alone; they share one leg",
        },
        ProbeSpec {
            name: "sj",
            description: "SJ itself, the passive node all eleven legs sum into",
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
    let device = ZaxxonSound::new(TIMING.cpu_clock_hz);
    let cycles_per_sample = TIMING.cpu_clock_hz as f64 / device.sample_rate() as f64;
    let mut t = ZaxxonTarget {
        device,
        cycles_per_sample,
        cycle_debt: 0.0,
        pending: std::collections::VecDeque::new(),
        buf: vec![0i16; 64],
        ports: RESTING_PORTS,
        probe: probe.map(str::to_string),
    };
    // The board rests with every gate line pulled high, and the device is only
    // told that when the latches are pushed, so push them once before the run.
    t.device.set_ports(t.ports.0, t.ports.1, t.ports.2);
    Ok(Box::new(t))
}

/// What `RP1` and `RP2` hold the fourteen lines at when nothing has written:
/// every gate off, and the engine level at the bottom of its ladder.
const RESTING_PORTS: (u8, u8, u8) = (0xFF, 0xFF, 0xFF);

struct ZaxxonTarget {
    device: ZaxxonSound,
    cycles_per_sample: f64,
    cycle_debt: f64,
    pending: std::collections::VecDeque<i16>,
    buf: Vec<i16>,
    ports: (u8, u8, u8),
    probe: Option<String>,
}

impl ZaxxonTarget {
    /// Clear or set one active-low gate bit. `on` is the voice being asked for,
    /// so it clears the bit.
    fn gate(&mut self, port: usize, mask: u8, on: bool) {
        let p = match port {
            0 => &mut self.ports.0,
            1 => &mut self.ports.1,
            _ => &mut self.ports.2,
        };
        if on {
            *p &= !mask;
        } else {
            *p |= mask;
        }
    }
}

impl SoundTarget for ZaxxonTarget {
    fn sample_rate(&self) -> u32 {
        self.device.sample_rate()
    }

    fn set_control(&mut self, name: &str, value: Value) -> Result<(), String> {
        let on = value.as_bool();
        match name {
            // The ladder, not a gate. `3 - code` puts the two bits back the way
            // the 7406 sections leave them, so the scenario says "loudest" and
            // this says 0b00.
            "ship-level" => {
                let code = (value.as_f64().round() as i64).clamp(0, 3) as u8;
                let bits = 3 - code;
                self.ports.0 = (self.ports.0 & !0x03) | ((bits >> 1) & 0x01) | ((bits & 0x01) << 1);
            }
            // U32's two decoded outputs. Y0 needs PA2 and PA3 both low, Y1 needs
            // PA2 high and PA3 low, so these two are not independent bits and
            // asking for either releases the other.
            "ship-tone-a" => {
                self.ports.0 = if on {
                    self.ports.0 & !0x0C
                } else {
                    self.ports.0 | 0x0C
                };
            }
            "ship-tone-b" => {
                self.ports.0 = if on {
                    (self.ports.0 | 0x04) & !0x08
                } else {
                    self.ports.0 | 0x0C
                };
            }
            "homing-missile" => self.gate(0, 0x10, on),
            "base-missile" => self.gate(0, 0x20, on),
            "laser" => self.gate(0, 0x40, on),
            "battleship" => self.gate(0, 0x80, on),
            "s-exp" => self.gate(1, 0x10, on),
            "m-exp" => self.gate(1, 0x20, on),
            "cannon" => self.gate(1, 0x80, on),
            "shot" => self.gate(2, 0x01, on),
            "alarm2" => self.gate(2, 0x04, on),
            "alarm3" => self.gate(2, 0x08, on),
            other => {
                let names: Vec<&str> = SPEC.controls.iter().map(|c| c.name).collect();
                return Err(format!(
                    "unknown control {other:?} for zaxxon-discrete; known: {}",
                    names.join(", ")
                ));
            }
        }
        self.device
            .set_ports(self.ports.0, self.ports.1, self.ports.2);
        Ok(())
    }

    fn step(&mut self) -> i16 {
        // The device is clocked in main-CPU cycles and resamples internally, so
        // one output sample is not a whole number of cycles. Carrying the
        // fraction keeps the capture's length exact over a long run rather than
        // drifting by the rounding every sample.
        self.cycle_debt += self.cycles_per_sample;
        let whole = self.cycle_debt.floor();
        self.cycle_debt -= whole;
        self.device.tick(whole as u64);

        let n = self.device.fill_audio(&mut self.buf);
        self.pending.extend(&self.buf[..n]);

        if let Some(p) = &self.probe
            && p != "mix"
            && let Some(v) = probe_value(self.device.circuit(), p)
        {
            // Drained anyway, so the mix and a probe advance the device
            // identically and a probe capture lines up with the mix sample for
            // sample.
            self.pending.pop_front();
            return (v * i16::MAX as f64).clamp(i16::MIN as f64, i16::MAX as f64) as i16;
        }
        self.pending.pop_front().unwrap_or(0)
    }
}

/// Read a named node out of the built circuit.
///
/// The legs are millivolt-scale at `SJ`'s side of a 51 kOhm common, three orders
/// below the mix that `U11`'s gain of -8.2 and the power amplifier make of them,
/// so they are scaled up to be audible rather than left at their circuit value.
/// The scale is the same for all eleven, which is the part that matters: the
/// board's whole balance is the eleven legs' relative size, and a per-probe
/// scale would destroy exactly that.
fn probe_value(circuit: &DiscreteCircuit, probe: &str) -> Option<f64> {
    const LEG: f64 = 0.02;
    let (node, scale) = match probe {
        "ship-a" => ("SHIP_A_LEG", LEG),
        "ship-b" => ("SHIP_B_LEG", LEG),
        "homing-missile" => ("HOMING_LEG", LEG),
        "base-missile" => ("BASE_MISSILE_LEG", LEG),
        "laser" => ("LASER_LEG", LEG),
        "battleship" => ("BATTLESHIP_LEG", LEG),
        "s-exp" => ("S_EXP_LEG", LEG),
        "m-exp" => ("M_EXP_LEG", LEG),
        "cannon" => ("CANNON_LEG", LEG),
        "shot" => ("SHOT_LEG", LEG),
        "alarms" => ("ALARM_LEG", LEG),
        "sj" => ("SJ", LEG),
        _ => return None,
    };
    circuit
        .node_by_name(node)
        .map(|id| circuit.value(id) / scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    impl ZaxxonTarget {
        /// The ladder's control voltage, after letting the circuit evaluate.
        /// A `set_data` is a pending value until a step consumes it, so reading
        /// the node without advancing first returns the value from before.
        fn ladder_volts(&mut self) -> f64 {
            self.device.tick(128);
            let id = self
                .device
                .circuit()
                .node_by_name("SHIP_LEVEL")
                .expect("the device exposes its ladder node");
            self.device.circuit().value(id)
        }
    }

    /// A target whose port latches can be read back, which the trait cannot do.
    fn bare() -> ZaxxonTarget {
        ZaxxonTarget {
            device: ZaxxonSound::new(TIMING.cpu_clock_hz),
            cycles_per_sample: 1.0,
            cycle_debt: 0.0,
            pending: std::collections::VecDeque::new(),
            buf: vec![0i16; 8],
            ports: RESTING_PORTS,
            probe: None,
        }
    }

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
        assert!(err.contains("cannon"), "{err}");
    }

    #[test]
    fn every_declared_probe_resolves_to_a_node() {
        let dev = ZaxxonSound::new(TIMING.cpu_clock_hz);
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

    /// The gates are active low and the scenarios are not, which is the one
    /// place this adapter could invert the whole board without failing anything
    /// else. A voice asked for must clear its bit, and releasing it must leave
    /// the port exactly where the pull-ups had it.
    #[test]
    fn asking_for_a_voice_clears_its_bit_and_releasing_restores_the_port() {
        let mut t = bare();
        t.set_control("cannon", Value::Bool(true)).expect("set");
        assert_eq!(t.ports.1, 0x7F, "cannon is PB7, active low");
        t.set_control("cannon", Value::Bool(false)).expect("clear");
        assert_eq!(t.ports, RESTING_PORTS);
    }

    /// `PA0` is the ladder's more significant bit and the level falls as the
    /// bits rise, so a scenario's "3" has to come out as `0b00`. Getting this
    /// backwards is audible but entirely plausible, which is why it is pinned.
    /// `PA0` is the ladder's more significant bit and the board's level FALLS as
    /// the two bits rise, so this adapter's 0-to-3 has to come out inverted and
    /// bit-swapped. Checked through the device rather than against a bit pattern:
    /// what a scenario is promised is that 3 is the loudest, and the port bits
    /// are how that is delivered rather than what is being claimed.
    #[test]
    fn the_engine_level_rises_with_the_control_and_falls_with_the_bits() {
        let mut t = bare();
        let mut last = f64::NEG_INFINITY;
        for code in 0..=3 {
            t.set_control("ship-level", Value::Number(f64::from(code)))
                .expect("set");
            let v = t.ladder_volts();
            assert!(v > last, "level {code} gave {v} V, not above {last} V");
            last = v;
        }
        // 3 is the top of the ladder, which the board reaches with both bits low.
        assert_eq!(t.ports.0 & 0x03, 0b00);
        // And the level never disturbs the four gate bits above it.
        assert_eq!(t.ports.0 & !0x03, RESTING_PORTS.0 & !0x03);
    }

    /// The resting state is not an arbitrary starting point: `RP1` and `RP2`
    /// hold all fourteen lines high, which for the level pair is the BOTTOM of
    /// the ladder with `PC1`'s LED dark. A target that started anywhere else
    /// would make every scenario's first moment a transient.
    #[test]
    fn the_resting_port_is_the_quietest_level_and_no_gate() {
        let mut t = bare();
        t.device
            .set_ports(RESTING_PORTS.0, RESTING_PORTS.1, RESTING_PORTS.2);
        let resting = t.ladder_volts();
        t.set_control("ship-level", Value::Number(0.0))
            .expect("set");
        assert_eq!(t.ladder_volts(), resting);
        assert_eq!(t.ports, RESTING_PORTS);
    }
}
