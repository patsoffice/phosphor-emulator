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
    LogicInputId, NodeId, Output555, OutputGain, PulseInputId,
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
            self.lfsr = (self.lfsr << 1) | fb;
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
// Fire chirp envelope (custom escape-hatch component)
// ---------------------------------------------------------------------------

/// Triggered envelope for the fire "pew". On each rising edge of the enable it
/// restarts a timer and, while the enable is held, outputs one of two shapes:
///
/// - **Linear** (`exponential = false`): the sweep position `min(elapsed/span, 1)`,
///   rising 0→1 over `span` seconds. It drives the [`ne555_cc`] control voltage;
///   because that VCO's frequency is *linear* in its CV, a linear CV ramp yields
///   a linear frequency sweep (the descending "pew").
/// - **Exponential** (`exponential = true`): the amplitude decay `exp(-elapsed/span)`,
///   which multiplies the oscillator output.
///
/// The oscillator itself is the `ne555_cc` node, so this component carries no
/// phase — only the envelopes the framework can't express as a triggered ramp.
/// Input: `[enable 0/1]`.
///
/// [`ne555_cc`]: phosphor_core::device::DiscreteCircuitBuilder::ne555_cc
struct FireEnvelope {
    span: f64,
    exponential: bool,
    active: bool,
    elapsed: f64,
    last_en: bool,
}

impl FireEnvelope {
    fn linear(span: f64) -> Self {
        Self {
            span,
            exponential: false,
            active: false,
            elapsed: 0.0,
            last_en: false,
        }
    }
    fn exp(span: f64) -> Self {
        Self {
            span,
            exponential: true,
            active: false,
            elapsed: 0.0,
            last_en: false,
        }
    }
}

impl CustomComponent for FireEnvelope {
    fn reset(&mut self) {
        self.active = false;
        self.elapsed = 0.0;
        self.last_en = false;
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let en = inputs[0] > 0.5;
        if !en {
            // Silent while released; the sweep restarts on the next rising edge.
            self.active = false;
            self.last_en = false;
            return 0.0;
        }
        if !self.last_en {
            self.active = true;
            self.elapsed = 0.0;
        }
        self.last_en = true;
        let e = self.elapsed;
        self.elapsed += dt;
        if self.exponential {
            (-e / self.span).exp()
        } else {
            (e / self.span).min(1.0)
        }
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.write_bool(self.active);
        w.write_f64_le(self.elapsed);
        w.write_bool(self.last_en);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.active = r.read_bool()?;
        self.elapsed = r.read_f64_le()?;
        self.last_en = r.read_bool()?;
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

/// Output gain after the thrust band-pass. The multiple-feedback band-pass
/// already provides the resonant make-up (its center gain is `Rf / 2·Rin`), so
/// this only trims the path to its mix level — calibrated against the reference
/// thrust rumble rather than the hand-tuned pre-gain it replaces.
const THRUST_GAIN: f64 = 0.12;

/// Output gain after the thump 555/RC chain, calibrated to the reference thump
/// level (replaces the old `LVL_THUMP / LVL_TOTAL` weight on the square VCO).
const THUMP_GAIN: f64 = 0.135;

// Fire chirp constant-current 555 VCO. The frequency is linear in the control
// voltage, `f = (Vcc_src − Vbe − Vcv) / ((Vcc/3)·C·R)`, so a linear CV ramp gives
// a linear frequency sweep. R·C is sized so the stall edge (cap just reaches the
// ⅔·Vcc threshold) sits above the top of the sweep (~910 Hz), leaving the
// 110–830 Hz pew range comfortably oscillating.
const FIRE_C: f64 = 0.01e-6;
const FIRE_R: f64 = 110e3;
const FIRE_VCC: f64 = 5.0;
const FIRE_VCC_SRC: f64 = 5.0;
const FIRE_JUNCTION: f64 = 0.7;
/// Per-fire output gains, calibrated to the reference pew levels.
const SHIP_FIRE_GAIN: f64 = 0.104;
const SAUCER_FIRE_GAIN: f64 = 0.064;

/// Control voltage that makes the fire 555 oscillate at `freq` Hz (inverse of the
/// linear CC-VCO frequency law above).
fn fire_cv(freq: f64) -> f64 {
    FIRE_VCC_SRC - FIRE_JUNCTION - freq * (FIRE_VCC / 3.0) * FIRE_C * FIRE_R
}

/// Build one fire "pew": a linear CV ramp (`f_hi`→`f_lo` over `ramp` s) sweeps a
/// constant-current 555 VCO, AC-coupled and shaped by an exponential amplitude
/// envelope (`amp_tau`). Returns the leveled output node.
#[allow(clippy::too_many_arguments)]
fn build_fire(
    b: &mut DiscreteCircuitBuilder,
    enable: LogicInputId,
    name: &str,
    f_hi: f64,
    f_lo: f64,
    ramp: f64,
    amp_tau: f64,
    gain: f64,
) -> NodeId {
    let sweep = b.custom(
        &format!("{name}_SWEEP"),
        vec![enable.into()],
        Box::new(FireEnvelope::linear(ramp)),
    );
    let (v0, v1) = (fire_cv(f_hi), fire_cv(f_lo));
    let vin_g = b.gain(&format!("{name}_VIN_G"), sweep, v1 - v0);
    let vin_b = b.constant(&format!("{name}_VIN0"), v0);
    let vin = b.add(&format!("{name}_VIN"), &[vin_g, vin_b]);
    let osc = b.ne555_cc(
        &format!("{name}_555"),
        vin,
        FIRE_R,
        FIRE_C,
        FIRE_VCC,
        FIRE_VCC_SRC,
        FIRE_JUNCTION,
        Output555::Capacitor,
    );
    let ac = b.rc_high_pass(&format!("{name}_AC"), osc, 16e3, 1e-6);
    let amp = b.custom(
        &format!("{name}_AMP"),
        vec![enable.into()],
        Box::new(FireEnvelope::exp(amp_tau)),
    );
    let env = b.multiply(&format!("{name}_ENV"), ac, amp);
    b.gain(&format!("{name}_FIRE_OUT"), env, gain)
}

fn build_circuit() -> (DiscreteCircuit, AsteroidsDiscreteInputs) {
    let mut b = DiscreteCircuitBuilder::new(
        TIMING.cpu_clock_hz,
        phosphor_core::audio::host_sample_rate() as u64,
    );

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

    // --- Thrust: 12 kHz noise -> RC pre-filter (~72 Hz) -> gate -> resonant
    // op-amp multiple-feedback band-pass (~89.5 Hz, Q ~7.6) -> 160 Hz output
    // low-pass. The band-pass R/C set fc, Q and the make-up gain together
    // (center gain Rf/2·Rin ≈ 115), so the path no longer needs a hand-tuned
    // pre-gain. Deep rumble, no high-frequency hiss. ---
    let thrust_noise = b.lfsr_noise(
        "THRUST_NOISE",
        12_000.0,
        LfsrSpec {
            width: 16,
            taps: (6, 14),
            seed: 0xACE1,
        },
    );
    let thrust_rc = b.rc_low_pass("THRUST_RC", thrust_noise, 2_200.0, 1e-6);
    let thrust_gated = b.multiply("THRUST_GATE", thrust_rc, thrust_en);
    // R_in 1.17 kΩ / R_f 270 kΩ / C 0.1 µF give fc ≈ 89.5 Hz, Q ≈ 7.6 (matching
    // the reference). Rails wide enough to stay linear over the noise drive.
    let thrust_bp = b.op_amp_band_pass(
        "THRUST_BP",
        thrust_gated,
        &[1_170.0],
        270_000.0,
        0.1e-6,
        0.1e-6,
        0.0,
        -12.0,
        12.0,
    );
    // Steep 2nd-order output low-pass to keep it a deep rumble (the resonant
    // band-pass skirts otherwise leave audible upper noise).
    let thrust_lp = b.second_order("THRUST_LP", thrust_bp, FilterMode::LowPass, 120.0, 0.707);
    let thrust = b.gain("THRUST", thrust_lp, THRUST_GAIN);

    // --- Thump: a 4-bit R-1 DAC sets the control voltage of a constant-current
    // 555 VCO (the cap sawtooth), AC-coupled, RC-smoothed and gated. Higher data
    // raises the CV, lowering the charge current and the pitch (~200 Hz at data 0
    // down to ~55 Hz at full data). ---
    // DAC node voltage = (Σ bit·Von/R + Vbias/Rbias) / (Σ1/R + 1/Rbias + 1/Rgnd),
    // with R = 220k/100k/47k/22k (bits 0-3), Von 3.5, Vbias 4.3, Rbias 6.8k,
    // Rgnd 47k.
    let thump_denom =
        1.0 / 220e3 + 1.0 / 100e3 + 1.0 / 47e3 + 1.0 / 22e3 + 1.0 / 6.8e3 + 1.0 / 47e3;
    let thump_weights: Vec<f64> = [220e3, 100e3, 47e3, 22e3]
        .iter()
        .map(|r| 3.5 / (r * thump_denom))
        .collect();
    let thump_dac_bits = b.dac_weighted("THUMP_DAC", thump_data, &thump_weights);
    let thump_dac_off = b.constant("THUMP_DAC_OFF", 4.3 / (6.8e3 * thump_denom));
    let thump_cv_raw = b.add("THUMP_CV_RAW", &[thump_dac_bits, thump_dac_off]);
    // The constant-current VCO's frequency is steep near the charge limit (a few
    // % of CV moves the pitch ~20 %), so trim the modeled DAC voltage slightly to
    // land the reference pitch (~53 Hz at full data).
    let thump_cv = b.gain("THUMP_CV", thump_cv_raw, 1.027);
    let thump_555 = b.ne555_cc(
        "THUMP_555",
        thump_cv,
        22e3,
        0.22e-6, // R, C
        5.0,
        5.0,
        0.8, // vcc, v_cc_source, 2N3906 junction
        // The cap sawtooth is the audible tap; the framework's CC square is a
        // near-100 %-duty pulse (instant discharge) that doesn't model this VCO.
        Output555::Capacitor,
    );
    let thump_ac = b.rc_high_pass("THUMP_AC", thump_555, 16e3, 1e-6); // strip the cap-voltage DC
    let thump_rc = b.rc_low_pass("THUMP_RC", thump_ac, 3.3e3, 0.1e-6); // ~482 Hz coupling filter
    let thump_gated = b.multiply("THUMP_G", thump_rc, thump_en);
    let thump = b.gain("THUMP", thump_gated, THUMP_GAIN);

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

    // --- Fire paths: a constant-current 555 VCO swept by a linear CV ramp gives
    // the descending-frequency "pew"; an exponential envelope sets the amplitude.
    // Ship: 820 -> 110 Hz over 0.28 s, fast decay (τ 81 ms). ---
    let ship_out = build_fire(
        &mut b,
        ship_fire,
        "SHIP",
        820.0,
        110.0,
        0.28,
        0.081,
        SHIP_FIRE_GAIN,
    );
    // Saucer: a higher, narrower 830 -> 630 Hz sweep with a slower decay (τ 0.3 s).
    let sfire_out = build_fire(
        &mut b,
        saucer_fire,
        "SAUCER",
        830.0,
        630.0,
        0.28,
        0.3,
        SAUCER_FIRE_GAIN,
    );

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
