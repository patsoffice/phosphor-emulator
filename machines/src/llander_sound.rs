//! Lunar Lander (1979) discrete sound, built on the [`DiscreteCircuit`]
//! framework. Models MAME's `llander_discrete` netlist (asteroid.cpp): a shared
//! 12 kHz LFSR noise source drives both the band-passed rocket thrust and the
//! crash explosion (which reuses the 3-bit thrust value as its volume), plus
//! two fixed alert tones (3 kHz / 6 kHz). The board talks to it with hardware
//! intent (`write_sound_register`, `pulse_noise_reset`).

use phosphor_core::core::debug::{DebugRegister, Debuggable};
use phosphor_core::core::save_state::{SaveError, StateReader, StateWriter};
use phosphor_core::device::{
    CustomComponent, DataInputId, DiscreteCircuit, DiscreteCircuitBuilder, LogicInputId, NodeId,
    OutputGain, PulseInputId,
};
use phosphor_macros::Saveable;

use crate::atari_dvg::TIMING;

// ---------------------------------------------------------------------------
// Shared 12 kHz LFSR noise (custom escape-hatch component)
// ---------------------------------------------------------------------------

/// MAME `llander_lfsr`: 16-bit XNOR LFSR (taps 6 and 14), clocked at 12 kHz,
/// output taken from bit 14, resettable by the noise-reset pulse. It feeds both
/// the thrust and explosion paths, so it lives on the framework's `Custom`
/// escape hatch (the built-in `lfsr_noise` node can't be reset). Input:
/// `[noise_reset 0/1]`.
struct LanderNoise {
    lfsr: u16,
    clock_acc: f64,
}

impl LanderNoise {
    // Reset value 0, matching MAME. (For an XNOR LFSR the *all-ones* state is the
    // lock state; 0 is the natural running seed.)
    const SEED: u16 = 0;
}

impl CustomComponent for LanderNoise {
    fn reset(&mut self) {
        self.lfsr = Self::SEED;
        self.clock_acc = 0.0;
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        if inputs[0] > 0.5 {
            self.lfsr = 0;
        }
        self.clock_acc += 12_000.0 * dt;
        while self.clock_acc >= 1.0 {
            self.clock_acc -= 1.0;
            let fb = !(((self.lfsr >> 6) ^ (self.lfsr >> 14)) & 1) & 1;
            self.lfsr = (self.lfsr << 1) | fb;
        }
        if (self.lfsr >> 14) & 1 != 0 {
            1.0
        } else {
            -1.0
        }
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.write_u16_le(self.lfsr);
        w.write_f64_le(self.clock_acc);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.lfsr = r.read_u16_le()?;
        self.clock_acc = r.read_f64_le()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Typed input handles + node ids for debug
// ---------------------------------------------------------------------------

struct LunarLanderInputs {
    thrust_data: DataInputId,
    tone3k_en: LogicInputId,
    tone6k_en: LogicInputId,
    explod_en: LogicInputId,
    noise_reset: PulseInputId,
    mix: NodeId,
}

// ---------------------------------------------------------------------------
// Circuit construction
// ---------------------------------------------------------------------------

/// MAME relative mix levels (`llander_discrete`): tones 9.2 each, explosion
/// 1000, thrust 600 (the explosion path multiplies the shared noise*throttle by
/// `LVL_EXPLOSION / LVL_THRUST`).
const LVL_TONE: f64 = 9.2;
const LVL_THRUST: f64 = 600.0;
const LVL_EXPLOSION: f64 = 1000.0;

// Gains tuned to captured llander references (tools/sound-reference) for the
// thrust/explosion/tone RMS balance and overall level. THRUST_IN_GAIN sets the
// noise·throttle amplitude feeding the unfiltered explosion path; the thrust
// band-pass instead takes the normalized throttle·noise, and THRUST_OUT_GAIN
// supplies its post-filter make-up + trim.
const THRUST_IN_GAIN: f64 = 2400.0; // explosion noise * throttle amplitude
const THRUST_OUT_GAIN: f64 = 18.7; // post-band-pass make-up + trim
const OUTPUT_GAIN: f64 = 1.0 / 14347.0;

fn build_circuit() -> (DiscreteCircuit, LunarLanderInputs) {
    let mut b = DiscreteCircuitBuilder::new(
        TIMING.cpu_clock_hz,
        phosphor_core::audio::host_sample_rate() as u64,
    );

    // --- Board-facing inputs ---
    let thrust_data = b.data_input("THRUST_DATA", 1.0); // normalized 0..1 (data/7)
    let tone3k_en = b.logic_input("TONE3K_EN");
    let tone6k_en = b.logic_input("TONE6K_EN");
    let explod_en = b.logic_input("EXPLOD_EN");
    let noise_reset = b.pulse_input("NOISE_RESET");

    // --- Shared noise -> RC low-pass (~71 Hz) ---
    let noise = b.custom(
        "NOISE",
        vec![noise_reset.into()],
        Box::new(LanderNoise {
            lfsr: LanderNoise::SEED,
            clock_acc: 0.0,
        }),
    );
    let noise_rc = b.rc_low_pass("NOISE_RC", noise, 2_247.0, 1e-6);

    // --- Thrust: noise scaled by the 3-bit throttle, through a resonant op-amp
    // multiple-feedback band-pass (R_in 1.17k / R_f 270k / C 0.1uF -> fc ~89.5 Hz,
    // Q ~7.6). The band-pass's component-set gain (center Rf/2·Rin ≈ 115) provides
    // the resonant make-up, so it takes the *normalized* throttle·noise — the
    // amplified `thrust_amp` below would slam the op-amp into its rails. ---
    let thrust_throttle = b.multiply("THRUST_THROTTLE", noise_rc, thrust_data);
    let thrust_bp = b.op_amp_band_pass(
        "THRUST_BP",
        thrust_throttle,
        &[1_170.0],
        270_000.0,
        0.1e-6,
        0.1e-6,
        0.0,
        -12.0,
        12.0,
    );
    let thrust_path = b.gain("THRUST_PATH", thrust_bp, THRUST_OUT_GAIN);

    // --- Explosion: the same noise*throttle, scaled up (unfiltered) and gated ---
    let thrust_in = b.gain("THRUST_IN", thrust_data, THRUST_IN_GAIN);
    let thrust_amp = b.multiply("THRUST_AMP", noise_rc, thrust_in);
    let explod_scaled = b.gain("EXPLOD_SCALED", thrust_amp, LVL_EXPLOSION / LVL_THRUST);
    let explod_gated = b.multiply("EXPLOD_GATE", explod_scaled, explod_en);

    // Sum thrust + explosion, then a shared output low-pass (560 Hz).
    let te_sum = b.add("TE_SUM", &[thrust_path, explod_gated]);
    let thrust_explod = b.low_pass_hz("THRUST_EXPLOD", te_sum, 560.0);

    // --- Alert tones (quiet relative to thrust/explosion) ---
    let tone3k = b.fixed_square("TONE3K", 3_000.0);
    let tone3k_g = b.multiply("TONE3K_G", tone3k, tone3k_en);
    let tone3k_out = b.gain("TONE3K_OUT", tone3k_g, LVL_TONE);

    let tone6k = b.fixed_square("TONE6K", 6_000.0);
    let tone6k_g = b.multiply("TONE6K_G", tone6k, tone6k_en);
    let tone6k_out = b.gain("TONE6K_OUT", tone6k_g, LVL_TONE);

    // --- Final mix ---
    let mix = b.add("MIX", &[tone3k_out, tone6k_out, thrust_explod]);
    b.output(mix, OutputGain::linear(OUTPUT_GAIN));

    let circuit = b.build();
    (
        circuit,
        LunarLanderInputs {
            thrust_data,
            tone3k_en,
            tone6k_en,
            explod_en,
            noise_reset,
            mix,
        },
    )
}

// ---------------------------------------------------------------------------
// LunarLanderDiscreteSound — board-facing wrapper
// ---------------------------------------------------------------------------

/// Concrete Lunar Lander sound device. Wraps a [`DiscreteCircuit`] and exposes
/// hardware-intent methods for the board's bus writes.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct LunarLanderDiscreteSound {
    #[save(id = 2)]
    circuit: DiscreteCircuit,
    /// Input handles, fixed when the circuit is built.
    #[save_skip]
    ids: LunarLanderInputs,
    /// Last value written to the 0x3C00 sound register (for debug/save).
    #[save(id = 1)]
    sound_reg: u8,
}

impl LunarLanderDiscreteSound {
    pub fn new() -> Self {
        let (circuit, ids) = build_circuit();
        Self {
            circuit,
            ids,
            sound_reg: 0,
        }
    }

    /// 0x3C00 sound register: bits 0-2 thrust volume, bit 3 explosion enable,
    /// bit 4 3 kHz tone enable, bit 5 6 kHz tone enable.
    pub fn write_sound_register(&mut self, data: u8) {
        self.sound_reg = data;
        self.circuit
            .set_data(self.ids.thrust_data, (data & 0x07) as f64 / 7.0);
        self.circuit.set_logic(self.ids.explod_en, data & 0x08 != 0);
        self.circuit.set_logic(self.ids.tone3k_en, data & 0x10 != 0);
        self.circuit.set_logic(self.ids.tone6k_en, data & 0x20 != 0);
    }

    /// 0x3E00: noise reset pulse.
    pub fn pulse_noise_reset(&mut self) {
        self.circuit.pulse(self.ids.noise_reset);
    }

    /// Advance the circuit by `board_cycles` of CPU-clock time.
    pub fn tick(&mut self, board_cycles: u64) {
        self.circuit.tick(board_cycles);
    }

    /// Drain produced mono `i16` samples. Returns the number written.
    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize {
        self.circuit.fill_audio(out)
    }

    /// Output sample rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.circuit.sample_rate()
    }

    pub fn reset(&mut self) {
        self.circuit.reset();
        self.sound_reg = 0;
    }
}

impl Default for LunarLanderDiscreteSound {
    fn default() -> Self {
        Self::new()
    }
}

impl Debuggable for LunarLanderDiscreteSound {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "THRUST_DATA",
                value: (self.sound_reg & 0x07) as u64,
                width: 8,
            },
            DebugRegister {
                name: "EXPLODE",
                value: (self.sound_reg & 0x08 != 0) as u64,
                width: 8,
            },
            DebugRegister {
                name: "TONE_3K",
                value: (self.sound_reg & 0x10 != 0) as u64,
                width: 8,
            },
            DebugRegister {
                name: "TONE_6K",
                value: (self.sound_reg & 0x20 != 0) as u64,
                width: 8,
            },
            DebugRegister {
                name: "MIX",
                value: (self.circuit.value(self.ids.mix).clamp(-1.0, 1.0) * 32767.0) as i16 as u16
                    as u64,
                width: 16,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::save_state::Saveable as _;
    fn run_frame(s: &mut LunarLanderDiscreteSound) {
        s.tick(TIMING.cycles_per_frame());
    }

    fn ac_rms(s: &mut LunarLanderDiscreteSound) -> f64 {
        let mut buf = vec![0i16; 16384];
        let n = s.fill_audio(&mut buf);
        if n == 0 {
            return 0.0;
        }
        let mean = buf[..n].iter().map(|&v| v as f64).sum::<f64>() / n as f64;
        let ac: f64 = buf[..n].iter().map(|&v| (v as f64 - mean).powi(2)).sum();
        (ac / n as f64).sqrt()
    }

    #[test]
    fn sound_register_maps_all_fields() {
        let mut s = LunarLanderDiscreteSound::new();
        s.write_sound_register(0b0011_1101); // thrust 5, explode, 3k, 6k
        assert_eq!(s.sound_reg & 0x07, 0x05);
        let regs = s.debug_registers();
        let get = |name: &str| regs.iter().find(|r| r.name == name).unwrap().value;
        assert_eq!(get("THRUST_DATA"), 5);
        assert_eq!(get("EXPLODE"), 1);
        assert_eq!(get("TONE_3K"), 1);
        assert_eq!(get("TONE_6K"), 1);
    }

    #[test]
    fn thrust_turns_off_when_data_clears() {
        // Regression: the resonant thrust band-pass must not latch "on". Drive
        // full thrust, then clear it, and confirm the output decays to silence.
        let mut s = LunarLanderDiscreteSound::new();
        let mut discard = vec![0i16; 16384];
        s.write_sound_register(0x07); // full thrust
        for _ in 0..30 {
            run_frame(&mut s);
        }
        let on = ac_rms(&mut s);
        assert!(on > 100.0, "thrust should be audible while on, rms={on:.0}");

        // Release, let the resonant filter ring down, then measure the steady
        // state ~0.5 s later.
        while s.fill_audio(&mut discard) > 0 {}
        s.write_sound_register(0x00);
        for _ in 0..30 {
            run_frame(&mut s);
        }
        while s.fill_audio(&mut discard) > 0 {} // discard the ring-down transient
        for _ in 0..30 {
            run_frame(&mut s);
        }
        let off = ac_rms(&mut s);
        assert!(
            off < 20.0,
            "thrust should be ~silent after release, rms={off:.0} (was {on:.0})"
        );
    }

    #[test]
    fn tone_only_is_deterministic_and_non_silent() {
        let mut a = LunarLanderDiscreteSound::new();
        let mut b = LunarLanderDiscreteSound::new();
        a.write_sound_register(0x10); // 3 kHz tone only
        b.write_sound_register(0x10);
        run_frame(&mut a);
        run_frame(&mut b);
        let mut ba = vec![0i16; 8192];
        let mut bb = vec![0i16; 8192];
        let na = a.fill_audio(&mut ba);
        let nb = b.fill_audio(&mut bb);
        assert!(na > 0 && na == nb);
        assert_eq!(ba[..na], bb[..nb], "tone output must be deterministic");
        assert!(ba[..na].iter().any(|&v| v != 0), "3 kHz tone non-silent");
    }

    #[test]
    fn thrust_and_explosion_are_audible() {
        // Thrust: full throttle, no explosion.
        let mut s = LunarLanderDiscreteSound::new();
        s.write_sound_register(0x07);
        for _ in 0..6 {
            run_frame(&mut s);
        }
        assert!(ac_rms(&mut s) > 150.0, "thrust should be audible");

        // Explosion reuses the thrust value as its volume (MAME), so drive both.
        let mut s = LunarLanderDiscreteSound::new();
        s.write_sound_register(0x07 | 0x08);
        for _ in 0..6 {
            run_frame(&mut s);
        }
        assert!(ac_rms(&mut s) > 150.0, "explosion should be audible");
    }

    #[test]
    fn noise_reset_pulses_without_panicking() {
        let mut s = LunarLanderDiscreteSound::new();
        s.write_sound_register(0x07);
        s.pulse_noise_reset();
        run_frame(&mut s);
        let mut buf = vec![0i16; 4096];
        assert!(s.fill_audio(&mut buf) > 0);
    }

    #[test]
    fn save_load_round_trip() {
        let mut s1 = LunarLanderDiscreteSound::new();
        s1.write_sound_register(0x07 | 0x10);
        run_frame(&mut s1);
        let mut discard = vec![0i16; 8192];
        while s1.fill_audio(&mut discard) > 0 {}

        let mut w = StateWriter::new();
        s1.save_state(&mut w);
        let data = w.into_vec();

        let mut s2 = LunarLanderDiscreteSound::new();
        let mut r = StateReader::new(&data);
        s2.load_state(&mut r).unwrap();

        assert_eq!(s2.sound_reg, s1.sound_reg);
        assert_eq!(s2.circuit.value(s2.ids.mix), s1.circuit.value(s1.ids.mix));

        run_frame(&mut s1);
        run_frame(&mut s2);
        let mut a = vec![0i16; 8192];
        let mut b = vec![0i16; 8192];
        let na = s1.fill_audio(&mut a);
        let nb = s2.fill_audio(&mut b);
        assert_eq!(na, nb);
        assert_eq!(a[..na], b[..nb]);
    }
}
