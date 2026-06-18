//! Asteroids (1979) discrete sound, built on the [`DiscreteCircuit`] framework.
//!
//! Approximates the seven effect paths of MAME's `asteroid_a.cpp` discrete
//! netlist — explosion, thump, saucer, saucer-fire, ship-fire, thrust, and life
//! — summed into one mono output. This is a behaviorally faithful model, not a
//! bit-exact port: relative mix levels follow MAME, while the chirp/warble
//! shapes are reasonable approximations. The board talks to it with hardware
//! intent (`write_explosion`, `write_thump`, `write_audio_latch_bit`,
//! `pulse_noise_reset`) and never sees internal node ids.

use phosphor_core::core::debug::{DebugRegister, Debuggable};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::device::{
    CustomComponent, DataInputId, DiscreteCircuit, DiscreteCircuitBuilder, FilterMode, LfsrSpec,
    LogicInputId, NodeId, OutputGain, PulseInputId,
};

use crate::atari_dvg::TIMING;

/// MAME's pitch-divider mapping for the explosion path: register bits 6-7 select
/// the noise re-clock divider (`12 kHz / divider`).
fn explosion_divider(reg: u8) -> f64 {
    match (reg >> 6) & 0x03 {
        0 => 12.0,
        1 => 6.0,
        2 => 3.0,
        _ => 5.0,
    }
}

// ---------------------------------------------------------------------------
// Explosion noise generator (custom escape-hatch component)
// ---------------------------------------------------------------------------

/// 16-bit XNOR LFSR re-clocked at `12 kHz / divider`, scaled by volume. This is
/// circuit-specific (variable re-clock rate + register-cleared reset), so it
/// rides the framework's `Custom` escape hatch rather than a shared primitive.
///
/// Inputs: `[volume 0..1, divider, noise_reset 0/1]`.
struct ExplosionNoise {
    lfsr: u16,
    clock_acc: f64,
}

impl ExplosionNoise {
    // Reset/seed value 0, matching MAME. For an XNOR LFSR the *all-ones* state is
    // the lock state (it feeds back ones forever); 0 is the natural running seed.
    const SEED: u16 = 0;
}

impl CustomComponent for ExplosionNoise {
    fn reset(&mut self) {
        self.lfsr = Self::SEED;
        self.clock_acc = 0.0;
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let volume = inputs[0];
        let divider = inputs[1].max(1.0);
        if inputs[2] > 0.5 {
            // Noise reset clears the register (XNOR feedback recovers from 0).
            self.lfsr = 0;
        }
        let freq = 12_000.0 / divider;
        self.clock_acc += freq * dt;
        while self.clock_acc >= 1.0 {
            self.clock_acc -= 1.0;
            let fb = !(((self.lfsr >> 6) ^ (self.lfsr >> 14)) & 1) & 1;
            self.lfsr = ((self.lfsr << 1) | fb) & 0xFFFF;
        }
        let level = if self.lfsr & 1 != 0 { 1.0 } else { -1.0 };
        level * volume
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
// Fire chirp generator (custom escape-hatch component)
// ---------------------------------------------------------------------------

/// 555-VCO "pew": while the enable input is held high, the frequency ramps
/// linearly from `f_hi` to `f_lo` over `ramp_time` seconds and the amplitude
/// decays exponentially (`amp_tau`). The ramp restarts on each rising edge of
/// the enable. The framework has no triggered frequency ramp, so this rides the
/// `Custom` escape hatch. Input: `[enable 0/1]`.
struct FireChirp {
    f_hi: f64,
    f_lo: f64,
    ramp_time: f64,
    amp_tau: f64,
    active: bool,
    elapsed: f64,
    phase: f64,
}

impl FireChirp {
    fn new(f_hi: f64, f_lo: f64, ramp_time: f64, amp_tau: f64) -> Self {
        Self {
            f_hi,
            f_lo,
            ramp_time,
            amp_tau,
            active: false,
            elapsed: 0.0,
            phase: 0.0,
        }
    }
}

impl CustomComponent for FireChirp {
    fn reset(&mut self) {
        self.active = false;
        self.elapsed = 0.0;
        self.phase = 0.0;
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        if inputs[0] < 0.5 {
            self.active = false;
            return 0.0;
        }
        if !self.active {
            // Rising edge: (re)start the ramp.
            self.active = true;
            self.elapsed = 0.0;
            self.phase = 0.0;
        }
        let frac = (self.elapsed / self.ramp_time).min(1.0);
        let freq = self.f_hi - (self.f_hi - self.f_lo) * frac;
        let amp = (-self.elapsed / self.amp_tau).exp();
        self.phase += freq * dt;
        self.phase -= self.phase.floor();
        let square = if self.phase < 0.5 { 1.0 } else { -1.0 };
        self.elapsed += dt;
        square * amp
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.write_bool(self.active);
        w.write_f64_le(self.elapsed);
        w.write_f64_le(self.phase);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.active = r.read_bool()?;
        self.elapsed = r.read_f64_le()?;
        self.phase = r.read_f64_le()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Typed input handles + node ids for debug
// ---------------------------------------------------------------------------

struct AsteroidsDiscreteInputs {
    explosion_vol: DataInputId,
    explosion_pitch: DataInputId,
    noise_reset: PulseInputId,
    thump_en: LogicInputId,
    thump_data: DataInputId,
    saucer_en: LogicInputId,
    saucer_fire: LogicInputId,
    saucer_sel: LogicInputId,
    thrust_en: LogicInputId,
    ship_fire: LogicInputId,
    life_en: LogicInputId,
    mix: NodeId,
}

// ---------------------------------------------------------------------------
// Circuit construction
// ---------------------------------------------------------------------------

/// Relative mix levels from MAME's final adder (`asteroid_a.cpp`). Normalized so
/// the summed output stays within [-1, 1].
const LVL_EXPLOSION: f64 = 1000.0;
const LVL_THRUST: f64 = 600.0;
const LVL_LIFE: f64 = 100.0;
const LVL_THUMP: f64 = 131.6;
const LVL_SAUCER: f64 = 76.1;
const LVL_SHIP_FIRE: f64 = 53.0;
const LVL_SAUCER_FIRE: f64 = 49.5;
const LVL_TOTAL: f64 = LVL_EXPLOSION
    + LVL_THRUST
    + LVL_LIFE
    + LVL_THUMP
    + LVL_SAUCER
    + LVL_SHIP_FIRE
    + LVL_SAUCER_FIRE;

/// Make-up gain ahead of the thrust mix. The ~90 Hz band-pass passes only a
/// sliver of the noise's energy, so (as MAME does with its `600*7.6` pre-gain)
/// the path needs a boost — but only enough to reach full scale, not to slam
/// the resonant filter into hard clipping.
const THRUST_MAKEUP: f64 = 12.0;

fn build_circuit() -> (DiscreteCircuit, AsteroidsDiscreteInputs) {
    let mut b = DiscreteCircuitBuilder::new(TIMING.cpu_clock_hz, 44_100);

    // --- Board-facing inputs ---
    let explosion_vol = b.data_input("EXPLODE_VOL", 1.0);
    let explosion_pitch = b.data_input("EXPLODE_PITCH", 1.0);
    let noise_reset = b.pulse_input("NOISE_RESET");
    let thump_en = b.logic_input("THUMP_EN");
    let thump_data = b.data_input("THUMP_DATA", 1.0);
    let saucer_en = b.logic_input("SAUCER_EN");
    let saucer_fire = b.logic_input("SAUCER_FIRE");
    let saucer_sel = b.logic_input("SAUCER_SEL");
    let thrust_en = b.logic_input("THRUST_EN");
    let ship_fire = b.logic_input("SHIP_FIRE");
    let life_en = b.logic_input("LIFE_EN");

    // --- Explosion: pitched LFSR noise -> RC low-pass ---
    let expl_noise = b.custom(
        "EXPLODE_NOISE",
        vec![
            explosion_vol.into(),
            explosion_pitch.into(),
            noise_reset.into(),
        ],
        Box::new(ExplosionNoise {
            lfsr: ExplosionNoise::SEED,
            clock_acc: 0.0,
        }),
    );
    let expl_lp = b.rc_low_pass("EXPLODE_LP", expl_noise, 3_042.0, 1e-6);
    let expl = b.gain("EXPLODE", expl_lp, LVL_EXPLOSION / LVL_TOTAL);

    // --- Thrust: 12 kHz LFSR noise -> RC LP -> gate -> band-pass -> low-pass,
    // with a large make-up gain to compensate the narrow band-pass loss ---
    let thrust_noise = b.lfsr_noise(
        "THRUST_NOISE",
        12_000.0,
        LfsrSpec {
            width: 16,
            taps: (6, 14),
            seed: 0xACE1,
        },
    );
    // Pre-filter the noise (RC ~72 Hz as in MAME), resonate the ~82 Hz band
    // (MAME's measured thrust pitch), then a steep 2nd-order low-pass keeps it a
    // deep rumble with no high-frequency hiss.
    let thrust_rc = b.rc_low_pass("THRUST_RC", thrust_noise, 2_200.0, 1e-6);
    let thrust_gated = b.multiply("THRUST_GATE", thrust_rc, thrust_en);
    let thrust_bp = b.band_pass("THRUST_BP", thrust_gated, 82.0, 4.0);
    let thrust_lp = b.second_order("THRUST_LP", thrust_bp, FilterMode::LowPass, 130.0, 0.707);
    let thrust = b.gain("THRUST", thrust_lp, THRUST_MAKEUP * LVL_THRUST / LVL_TOTAL);

    // --- Thump: 4-bit DAC sets a low VCO (~20-55 Hz, MAME peaks ~53 Hz at max
    // data), smoothed, gated ---
    let thump_dac = b.dac_r2r("THUMP_DAC", thump_data, 4, 1.0); // 0..1
    let thump_sweep = b.gain("THUMP_SWEEP", thump_dac, 35.0);
    let thump_base = b.constant("THUMP_BASE", 20.0);
    let thump_freq = b.add("THUMP_FREQ", &[thump_sweep, thump_base]);
    let thump_vco = b.variable_square("THUMP_VCO", thump_freq);
    let thump_lp = b.low_pass_hz("THUMP_LP", thump_vco, 480.0);
    let thump_gated = b.multiply("THUMP_G", thump_lp, thump_en);
    let thump = b.gain("THUMP", thump_gated, LVL_THUMP / LVL_TOTAL);

    // --- Saucer (MAME asteroid_a.cpp): a triangle warble LFO (8.25 Hz small /
    // 5.75 Hz large) sweeps a triangle tone VCO. SAUCER_SEL shifts both the
    // warble rate and the tone centre: ~750-1670 Hz small, ~500-1420 Hz large.
    // The mellow triangle (not a square) avoids the harsh upper harmonics.
    let warble_base = b.constant("SAUCER_WBASE", 8.25);
    let warble_sel = b.gain("SAUCER_WSEL", saucer_sel, -2.5); // 8.25 small / 5.75 large
    let warble_rate = b.add("SAUCER_WRATE", &[warble_base, warble_sel]);
    let warble_lfo = b.variable_triangle("SAUCER_LFO", warble_rate); // ±1 triangle
    let warble_dev = b.gain("SAUCER_DEV", warble_lfo, 460.0); // ±460 Hz sweep
    let saucer_base = b.constant("SAUCER_BASE", 1_210.0);
    let saucer_seloff = b.gain("SAUCER_SELOFF", saucer_sel, -250.0); // large saucer lower
    let saucer_freq = b.add("SAUCER_FREQ", &[saucer_base, warble_dev, saucer_seloff]);
    let saucer_tone = b.variable_triangle("SAUCER_TONE", saucer_freq);
    let saucer_gated = b.multiply("SAUCER_G", saucer_tone, saucer_en);
    let saucer = b.gain("SAUCER", saucer_gated, LVL_SAUCER / LVL_TOTAL);

    // --- Fire paths: descending-frequency "pew" while the enable is held ---
    let ship = b.custom(
        "SHIP_FIRE",
        vec![ship_fire.into()],
        Box::new(FireChirp::new(820.0, 110.0, 0.28, 0.081)),
    );
    let ship_out = b.gain("SHIP_FIRE_OUT", ship, LVL_SHIP_FIRE / LVL_TOTAL);

    let sfire = b.custom(
        "SAUCER_FIRE",
        vec![saucer_fire.into()],
        Box::new(FireChirp::new(830.0, 630.0, 0.28, 0.3)),
    );
    let sfire_out = b.gain("SAUCER_FIRE_OUT", sfire, LVL_SAUCER_FIRE / LVL_TOTAL);

    // --- Life: fixed 3 kHz tone, gated ---
    let life_tone = b.fixed_square("LIFE_TONE", 3_000.0);
    let life_gated = b.multiply("LIFE_G", life_tone, life_en);
    let life = b.gain("LIFE", life_gated, LVL_LIFE / LVL_TOTAL);

    // --- Final mix ---
    let mix = b.add(
        "MIX",
        &[expl, thrust, thump, saucer, ship_out, sfire_out, life],
    );
    b.output(mix, OutputGain::unity());

    let circuit = b.build();
    (
        circuit,
        AsteroidsDiscreteInputs {
            explosion_vol,
            explosion_pitch,
            noise_reset,
            thump_en,
            thump_data,
            saucer_en,
            saucer_fire,
            saucer_sel,
            thrust_en,
            ship_fire,
            life_en,
            mix,
        },
    )
}

// ---------------------------------------------------------------------------
// AsteroidsDiscreteSound — board-facing wrapper
// ---------------------------------------------------------------------------

/// Concrete Asteroids sound device. Wraps a [`DiscreteCircuit`] and exposes
/// hardware-intent methods for the board's bus writes.
pub struct AsteroidsDiscreteSound {
    circuit: DiscreteCircuit,
    ids: AsteroidsDiscreteInputs,
    /// 0x3600 explosion register (volume bits 2-5, pitch bits 6-7).
    explosion_reg: u8,
    /// 0x3A00 thump register (bit 4 enable, low nibble DAC data).
    thump_reg: u8,
    /// 0x3C00-0x3C07 addressable audio latch (74LS259).
    audio_latch: u8,
}

impl AsteroidsDiscreteSound {
    pub fn new() -> Self {
        let (circuit, ids) = build_circuit();
        Self {
            circuit,
            ids,
            explosion_reg: 0,
            thump_reg: 0,
            audio_latch: 0,
        }
    }

    /// 0x3600: explosion. Bits 2-5 are volume (0-15); bits 6-7 the pitch divider.
    pub fn write_explosion(&mut self, data: u8) {
        self.explosion_reg = data;
        let volume = ((data >> 2) & 0x0F) as f64 / 15.0;
        self.circuit.set_data(self.ids.explosion_vol, volume);
        self.circuit
            .set_data(self.ids.explosion_pitch, explosion_divider(data));
    }

    /// 0x3A00: thump. Bit 4 enables; the low nibble is the 4-bit DAC code.
    pub fn write_thump(&mut self, data: u8) {
        self.thump_reg = data;
        self.circuit.set_logic(self.ids.thump_en, data & 0x10 != 0);
        self.circuit
            .set_data(self.ids.thump_data, (data & 0x0F) as f64);
    }

    /// 0x3C00-0x3C07: addressable audio latch (74LS259). `bit` selects the line.
    pub fn write_audio_latch_bit(&mut self, bit: u8, value: bool) {
        crate::set_bit_active_high(&mut self.audio_latch, bit, value);
        match bit {
            0 => self.circuit.set_logic(self.ids.saucer_en, value),
            1 => self.circuit.set_logic(self.ids.saucer_fire, value),
            2 => self.circuit.set_logic(self.ids.saucer_sel, value),
            3 => self.circuit.set_logic(self.ids.thrust_en, value),
            4 => self.circuit.set_logic(self.ids.ship_fire, value),
            5 => self.circuit.set_logic(self.ids.life_en, value),
            _ => {}
        }
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
        self.explosion_reg = 0;
        self.thump_reg = 0;
        self.audio_latch = 0;
    }
}

impl Default for AsteroidsDiscreteSound {
    fn default() -> Self {
        Self::new()
    }
}

impl Saveable for AsteroidsDiscreteSound {
    fn save_state(&self, w: &mut StateWriter) {
        w.write_version(1);
        w.write_u8(self.explosion_reg);
        w.write_u8(self.thump_reg);
        w.write_u8(self.audio_latch);
        self.circuit.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        r.read_version(1)?;
        self.explosion_reg = r.read_u8()?;
        self.thump_reg = r.read_u8()?;
        self.audio_latch = r.read_u8()?;
        self.circuit.load_state(r)?;
        Ok(())
    }
}

impl Debuggable for AsteroidsDiscreteSound {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        let bit = |b: u8| ((self.audio_latch >> b) & 1) as u64;
        vec![
            DebugRegister {
                name: "SAUCER",
                value: bit(0),
                width: 8,
            },
            DebugRegister {
                name: "SAUCER_FIRE",
                value: bit(1),
                width: 8,
            },
            DebugRegister {
                name: "SAUCER_SEL",
                value: bit(2),
                width: 8,
            },
            DebugRegister {
                name: "THRUST",
                value: bit(3),
                width: 8,
            },
            DebugRegister {
                name: "SHIP_FIRE",
                value: bit(4),
                width: 8,
            },
            DebugRegister {
                name: "LIFE",
                value: bit(5),
                width: 8,
            },
            DebugRegister {
                name: "THUMP_EN",
                value: (self.thump_reg & 0x10 != 0) as u64,
                width: 8,
            },
            DebugRegister {
                name: "THUMP_DATA",
                value: (self.thump_reg & 0x0F) as u64,
                width: 8,
            },
            DebugRegister {
                name: "EXPLODE_DATA",
                value: ((self.explosion_reg >> 2) & 0x0F) as u64,
                width: 8,
            },
            DebugRegister {
                name: "EXPLODE_PITCH",
                value: explosion_divider(self.explosion_reg) as u64,
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

    fn run_frame(s: &mut AsteroidsDiscreteSound) {
        s.tick(TIMING.cycles_per_frame());
    }

    fn rms(s: &mut AsteroidsDiscreteSound) -> f64 {
        let mut buf = vec![0i16; 4096];
        let n = s.fill_audio(&mut buf);
        if n == 0 {
            return 0.0;
        }
        let sum: f64 = buf[..n].iter().map(|&v| (v as f64).powi(2)).sum();
        (sum / n as f64).sqrt()
    }

    #[test]
    fn explosion_register_maps_volume_and_pitch() {
        let mut s = AsteroidsDiscreteSound::new();
        // bits 2-5 = 0b1111 (vol 15), bits 6-7 = 0b10 -> divider 3.
        s.write_explosion(0b1011_1100);
        assert_eq!((s.explosion_reg >> 2) & 0x0F, 0x0F);
        assert_eq!(explosion_divider(s.explosion_reg), 3.0);
        // Each pitch-select code maps as MAME documents.
        assert_eq!(explosion_divider(0b0000_0000), 12.0);
        assert_eq!(explosion_divider(0b0100_0000), 6.0);
        assert_eq!(explosion_divider(0b1000_0000), 3.0);
        assert_eq!(explosion_divider(0b1100_0000), 5.0);
    }

    #[test]
    fn thump_register_maps_enable_and_data() {
        let mut s = AsteroidsDiscreteSound::new();
        s.write_thump(0x1A); // enable (bit 4) + data 0x0A
        assert_eq!(s.thump_reg & 0x10, 0x10);
        assert_eq!(s.thump_reg & 0x0F, 0x0A);
    }

    #[test]
    fn audio_latch_bits_map_to_effects() {
        let mut s = AsteroidsDiscreteSound::new();
        for bit in 0..6u8 {
            s.write_audio_latch_bit(bit, true);
        }
        assert_eq!(s.audio_latch & 0x3F, 0x3F);
        let regs = s.debug_registers();
        // All six latch-driven effect registers should now read 1.
        for name in [
            "SAUCER",
            "SAUCER_FIRE",
            "SAUCER_SEL",
            "THRUST",
            "SHIP_FIRE",
            "LIFE",
        ] {
            let r = regs.iter().find(|r| r.name == name).unwrap();
            assert_eq!(r.value, 1, "{name} should be enabled");
        }
        s.write_audio_latch_bit(3, false);
        assert_eq!(s.audio_latch & 0x08, 0);
    }

    #[test]
    fn audio_drains_after_a_frame_when_active() {
        let mut s = AsteroidsDiscreteSound::new();
        // Silent at power-on (all effects off).
        run_frame(&mut s);
        assert!(rms(&mut s) < 1.0, "should be ~silent with no effects");

        // Enable the life tone -> non-silent, and audio drains.
        s.write_audio_latch_bit(5, true);
        let mut buf = vec![0i16; 8192];
        run_frame(&mut s);
        let n = s.fill_audio(&mut buf);
        assert!(n > 0, "expected samples after a frame");
    }

    #[test]
    fn explosion_produces_output() {
        let mut s = AsteroidsDiscreteSound::new();
        s.write_explosion(0b1011_1100); // max volume, divider 3
        run_frame(&mut s);
        assert!(rms(&mut s) > 1.0, "explosion should be audible");
    }

    #[test]
    fn every_effect_is_audible_when_enabled() {
        // Each effect, enabled in isolation, must produce a clearly non-silent
        // signal. Guards against silent paths (e.g. the D7 latch bug) and
        // under-gained filters (the thrust band-pass make-up).
        for effect in [
            "explosion",
            "thrust",
            "thump",
            "saucer",
            "ship_fire",
            "saucer_fire",
            "life",
        ] {
            let mut s = AsteroidsDiscreteSound::new();
            match effect {
                "explosion" => s.write_explosion(0b1011_1100), // max vol, divider 3
                "thrust" => s.write_audio_latch_bit(3, true),
                "thump" => s.write_thump(0x1F), // enable + max DAC
                "saucer" => s.write_audio_latch_bit(0, true),
                "ship_fire" => s.write_audio_latch_bit(4, true),
                "saucer_fire" => s.write_audio_latch_bit(1, true),
                "life" => s.write_audio_latch_bit(5, true),
                _ => unreachable!(),
            }
            for _ in 0..6 {
                s.tick(TIMING.cycles_per_frame());
            }
            let mut buf = vec![0i16; 16384];
            let n = s.fill_audio(&mut buf);
            assert!(n > 0, "{effect} produced no samples");
            // AC RMS (mean removed) so a stuck-DC signal can't false-pass.
            let mean = buf[..n].iter().map(|&v| v as f64).sum::<f64>() / n as f64;
            let ac: f64 = buf[..n].iter().map(|&v| (v as f64 - mean).powi(2)).sum();
            let rms = (ac / n as f64).sqrt();
            let peak = buf[..n]
                .iter()
                .map(|&v| v.unsigned_abs())
                .max()
                .unwrap_or(0);
            assert!(rms > 150.0, "{effect} should be audible, rms={rms:.0}");
            assert!(peak < 32_760, "{effect} should not hard-clip, peak={peak}");
        }
    }

    #[test]
    fn noise_reset_pulses_without_panicking() {
        let mut s = AsteroidsDiscreteSound::new();
        s.write_explosion(0b0011_1100);
        s.pulse_noise_reset();
        run_frame(&mut s);
        // Just exercising the path; output should still drain.
        let mut buf = vec![0i16; 4096];
        assert!(s.fill_audio(&mut buf) > 0);
    }

    #[test]
    fn save_load_round_trip() {
        let mut s1 = AsteroidsDiscreteSound::new();
        s1.write_explosion(0b0110_1100);
        s1.write_thump(0x15);
        s1.write_audio_latch_bit(3, true); // thrust
        s1.write_audio_latch_bit(5, true); // life
        run_frame(&mut s1);
        let mut discard = vec![0i16; 8192];
        while s1.fill_audio(&mut discard) > 0 {}

        let mut w = StateWriter::new();
        s1.save_state(&mut w);
        let data = w.into_vec();

        let mut s2 = AsteroidsDiscreteSound::new();
        let mut r = StateReader::new(&data);
        s2.load_state(&mut r).unwrap();

        assert_eq!(s2.explosion_reg, s1.explosion_reg);
        assert_eq!(s2.thump_reg, s1.thump_reg);
        assert_eq!(s2.audio_latch, s1.audio_latch);
        assert_eq!(s2.circuit.value(s2.ids.mix), s1.circuit.value(s1.ids.mix));

        // Lock-step audio afterward.
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
