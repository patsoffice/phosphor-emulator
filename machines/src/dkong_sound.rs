//! Donkey Kong (TKG-04) discrete sound, built on the [`DiscreteCircuit`]
//! framework. The I8035 DAC stream enters the circuit as an external source and
//! is summed with the three discrete analog effects (walk, jump, stomp) inside
//! the circuit — replacing the old "add the effect sample to the resampled DAC
//! as finished PCM" path. This puts the DAC + effects mixing inside the
//! framework so the shared analog stages (mixer, filters, discharge) have a home.
//!
//! Walk and jump are voltage-controlled 555 astables (R1 = 47 kΩ / R2 = 27 kΩ,
//! C = 33 nF walk / 47 nF jump), built on the framework's [`ne555_astable`]
//! primitive driven by a control-voltage node — a slow LFO for the walk wobble,
//! an exponential pitch-sweep envelope for the jump. This replaces the old
//! closed-form `vco_freq()` + phase-accumulator squares, so the cap integration
//! (and its ~73 % duty, harmonics) comes from the real 555 model. Stomp stays on
//! its LFSR-noise model — on hardware it is an LS164 noise source + LS161
//! counter, not a 555. The board talks to the device with hardware intent
//! (`write_sound_bit`, `feed_dac`, `set_discharge`).
//!
//! [`ne555_astable`]: phosphor_core::device::DiscreteCircuitBuilder::ne555_astable

use phosphor_core::core::debug::{DebugRegister, Debuggable};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::device::{
    CustomComponent, DiscreteCircuit, DiscreteCircuitBuilder, ExternalSourceId, FilterMode,
    LogicInputId, NodeId, Output555, OutputGain,
};

/// Output sample rate. The circuit is built board = sim = output = this rate, so
/// `tick(1)` advances exactly one simulation step; the board drives one step per
/// box-filtered DAC sample.
fn sample_rate() -> u64 {
    phosphor_core::audio::host_sample_rate() as u64
}

// Per-effect output low-pass cutoffs (Hz) and gains, calibrated against captured
// hardware references (tools/sound-reference). On the board each effect passes
// through RC integrators and a coupling filter, so the raw squares/noise are
// darkened by the low-passes and balanced against each other (walk subtle, jump
// moderate, stomp prominent). The absolute gains are scaled up relative to the
// reference because Phosphor's DAC plays at full scale while the board's music
// sits ~5x lower (the VR2 volume pot + mixer): matching the effect/music *ratio*
// keeps the effects audible over the music rather than buried under it.
// The DAC (music) is attenuated to leave output headroom for the effects, the
// way the VR2 volume pot + mixer do on hardware. At full scale the music
// consumed the entire range, so any effect loud enough to hear over it clipped
// against the output clamp. With headroom, the effects sit clearly above.
const DAC_GAIN: f64 = 0.55;
// I8035 DAC reconstruction filter: a Sallen-Key low-pass on the board (R = 5.6 kΩ
// ×2, C = 22 nF / 10 nF) gives f ≈ 1916 Hz, Q ≈ 0.74. It rolls off the DAC step
// edges and sample brightness so the music/effects sit warm rather than hashy.
const DAC_LP_HZ: f64 = 1_916.0;
const DAC_LP_Q: f64 = 0.74;
const WALK_LP_HZ: f64 = 700.0;
const WALK_GAIN: f64 = 0.14;
const JUMP_LP_HZ: f64 = 1_400.0;
const JUMP_GAIN: f64 = 0.25;
const STOMP_LP_HZ: f64 = 340.0;
const STOMP_GAIN: f64 = 7.0;

// Shared 555 astable values for the walk/jump VCOs: R1 charges through 47 kΩ,
// R2 discharges through 27 kΩ, Vcc = 5 V. The control voltage on pin 5 sets
// threshold = CV, trigger = CV/2, so a higher CV is a slower charge and a lower
// frequency.
const VCC: f64 = 5.0;
const R1: f64 = 47_000.0;
const R2: f64 = 27_000.0;
const WALK_C: f64 = 33e-9;
const JUMP_C: f64 = 47e-9;
/// 555 output-high level. The absolute value is folded into the per-effect
/// gains; 1.0 keeps the post-DC-block square at ±~0.5 before calibration.
const OUT_HIGH: f64 = 1.0;
/// DC-blocking high-pass matching the walk path's coupling network (11.2 kΩ,
/// 4.7 µF, ≈3 Hz). The 555 square's duty (and so its DC offset) shifts with CV,
/// so AC-couple before filtering rather than subtracting a fixed mean.
const AC_R: f64 = 11_200.0;
const AC_C: f64 = 4.7e-6;
/// Jump pitch-sweep / amplitude decay time constant (seconds).
const JUMP_TAU: f64 = 0.36;

// ---------------------------------------------------------------------------
// Effect components (custom escape hatch)
// ---------------------------------------------------------------------------

/// Jump amplitude/pitch envelope: a one-shot exponential decay (τ = [`JUMP_TAU`])
/// triggered on the rising edge of the enable and held for 0.5 s. Drives both the
/// 555 control voltage (`1 + 3·env`, so the pitch sweeps up as it decays) and the
/// output amplitude. This is envelope shaping only — the oscillator itself is the
/// [`ne555_astable`](phosphor_core::device::DiscreteCircuitBuilder::ne555_astable)
/// node. Input: `[jump_en]`.
struct DkongJumpEnv {
    active: bool,
    timer: f64,
    last_en: bool,
}

impl CustomComponent for DkongJumpEnv {
    fn reset(&mut self) {
        self.active = false;
        self.timer = 0.0;
        self.last_en = false;
    }
    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let en = inputs[0] > 0.5;
        if en && !self.last_en {
            self.active = true;
            self.timer = 0.0;
        }
        self.last_en = en;
        if !self.active {
            return 0.0;
        }
        self.timer += dt;
        if self.timer > 0.5 {
            self.active = false;
            return 0.0;
        }
        (-self.timer / JUMP_TAU).exp()
    }
    fn save_state(&self, w: &mut StateWriter) {
        w.write_bool(self.active);
        w.write_f64_le(self.timer);
        w.write_bool(self.last_en);
    }
    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.active = r.read_bool()?;
        self.timer = r.read_f64_le()?;
        self.last_en = r.read_bool()?;
        Ok(())
    }
}

/// Stomp: a 24-bit LFSR noise burst (4 kHz clock) with τ ≈ 50 ms amplitude
/// decay, triggered on the rising edge of the enable. Input: `[stomp_en]`.
struct DkongStomp {
    active: bool,
    timer: f64,
    lfsr: u32,
    lfsr_clock: f64,
    last_en: bool,
}

impl DkongStomp {
    const SEED: u32 = 0x1A_CFFC;
}

impl CustomComponent for DkongStomp {
    fn reset(&mut self) {
        self.active = false;
        self.timer = 0.0;
        self.lfsr = Self::SEED;
        self.lfsr_clock = 0.0;
        self.last_en = false;
    }
    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let en = inputs[0] > 0.5;
        if en && !self.last_en {
            self.active = true;
            self.timer = 0.0;
        }
        self.last_en = en;
        if !self.active {
            return 0.0;
        }
        self.timer += dt;
        if self.timer > 0.25 {
            self.active = false;
            return 0.0;
        }
        self.lfsr_clock += 4000.0 * dt;
        while self.lfsr_clock >= 1.0 {
            self.lfsr_clock -= 1.0;
            let bit = ((self.lfsr >> 10) ^ (self.lfsr >> 23)) & 1;
            self.lfsr = (self.lfsr >> 1) | (bit << 23);
        }
        let noise = if self.lfsr & 1 != 0 { 1.0 } else { -1.0 };
        let amp = (-self.timer / 0.05).exp();
        noise * amp * 0.12
    }
    fn save_state(&self, w: &mut StateWriter) {
        w.write_bool(self.active);
        w.write_f64_le(self.timer);
        w.write_u32_le(self.lfsr);
        w.write_f64_le(self.lfsr_clock);
        w.write_bool(self.last_en);
    }
    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.active = r.read_bool()?;
        self.timer = r.read_f64_le()?;
        self.lfsr = r.read_u32_le()?;
        self.lfsr_clock = r.read_f64_le()?;
        self.last_en = r.read_bool()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Circuit + wrapper
// ---------------------------------------------------------------------------

struct DkongInputs {
    dac: ExternalSourceId,
    walk_en: LogicInputId,
    jump_en: LogicInputId,
    stomp_en: LogicInputId,
    discharge: LogicInputId,
    mix: NodeId,
}

fn build_circuit() -> (DiscreteCircuit, DkongInputs) {
    let mut b = DiscreteCircuitBuilder::new(sample_rate(), sample_rate());

    let dac = b.external_source("DAC"); // normalized I8035 DAC stream
    let walk_en = b.logic_input("WALK_EN");
    let jump_en = b.logic_input("JUMP_EN");
    let stomp_en = b.logic_input("STOMP_EN");
    // Discharge control line — wired for the board-level mute/discharge path,
    // currently inert (the TKG-04 model does not drive it yet).
    let discharge = b.logic_input("DISCHARGE");

    // Walk: a ~1 Hz LFO wobbles the 555 control voltage around 3.15 ±0.65 V; the
    // 555 oscillates near ~430 Hz. AC-couple to drop the duty-dependent DC, gate
    // by the enable, then darken with the output low-pass and level it.
    let walk_lfo = b.triangle("WALK_LFO", 1.0); // ±1
    let walk_lfo_s = b.gain("WALK_LFO_S", walk_lfo, 0.65);
    let walk_cv_off = b.constant("WALK_CV_OFF", 3.15);
    let walk_cv = b.add("WALK_CV", &[walk_lfo_s, walk_cv_off]);
    let walk_555 = b.ne555_astable(
        "WALK_555",
        Some(walk_cv),
        R1,
        R2,
        WALK_C,
        VCC,
        OUT_HIGH,
        Output555::Square,
    );
    let walk_ac = b.rc_high_pass("WALK_AC", walk_555, AC_R, AC_C);
    let walk_gated = b.multiply("WALK_GATED", walk_ac, walk_en);
    let walk_lp = b.second_order(
        "WALK_LP",
        walk_gated,
        FilterMode::LowPass,
        WALK_LP_HZ,
        0.707,
    );
    let walk = b.gain("WALK_OUT", walk_lp, WALK_GAIN);

    // Jump: a one-shot exponential envelope drives both the 555 control voltage
    // (CV = 1 + 3·env, so the pitch sweeps up as it decays) and the output
    // amplitude. AC-couple the square, apply the amplitude envelope, then filter.
    let jump_env = b.custom(
        "JUMP_ENV",
        vec![jump_en.into()],
        Box::new(DkongJumpEnv {
            active: false,
            timer: 0.0,
            last_en: false,
        }),
    );
    let jump_cv_s = b.gain("JUMP_CV_S", jump_env, 3.0);
    let jump_cv_off = b.constant("JUMP_CV_OFF", 1.0);
    let jump_cv = b.add("JUMP_CV", &[jump_cv_s, jump_cv_off]);
    let jump_555 = b.ne555_astable(
        "JUMP_555",
        Some(jump_cv),
        R1,
        R2,
        JUMP_C,
        VCC,
        OUT_HIGH,
        Output555::Square,
    );
    let jump_ac = b.rc_high_pass("JUMP_AC", jump_555, AC_R, AC_C);
    let jump_amp = b.multiply("JUMP_AMP", jump_ac, jump_env);
    let jump_lp = b.second_order("JUMP_LP", jump_amp, FilterMode::LowPass, JUMP_LP_HZ, 0.707);
    let jump = b.gain("JUMP_OUT", jump_lp, JUMP_GAIN);

    let stomp_raw = b.custom(
        "STOMP",
        vec![stomp_en.into()],
        Box::new(DkongStomp {
            active: false,
            timer: 0.0,
            lfsr: DkongStomp::SEED,
            lfsr_clock: 0.0,
            last_en: false,
        }),
    );
    let stomp_lp = b.second_order(
        "STOMP_LP",
        stomp_raw,
        FilterMode::LowPass,
        STOMP_LP_HZ,
        0.707,
    );
    let stomp = b.gain("STOMP_OUT", stomp_lp, STOMP_GAIN);

    // DAC reconstruction: a Sallen-Key low-pass darkens the raw DAC steps before
    // the mix, matching the board's filter so the sampled music isn't hashy.
    let dac_lp = b.second_order("DAC_LP", dac, FilterMode::LowPass, DAC_LP_HZ, DAC_LP_Q);

    // Op-amp summing mixer: the filtered DAC plus the three filtered effects.
    let mix = b.add("MIX", &[dac_lp, walk, jump, stomp]);

    // Output coupling capacitor.
    //
    // Walk and jump are AC-coupled individually above, but the I8035 DAC is
    // unipolar — its codes span 0..Vref, so its rest level is a pedestal, not
    // zero — and nothing was removing that from the summed output. It reached
    // the speaker as a slow drift that carried 94 % of the board's measured
    // energy below 150 Hz, with a spectral centroid of 32 Hz: content no
    // cabinet speaker reproduces, swamping the walk/jump/stomp voices this
    // circuit exists to model.
    //
    // The corner matches the per-effect couplings already here (11.2 kΩ /
    // 4.7 µF ≈ 3 Hz), which is the same part value the board uses on its
    // output stage.
    let out_ac = b.rc_high_pass("OUT_AC", mix, AC_R, AC_C);
    b.output(out_ac, OutputGain::unity());

    let circuit = b.build();
    (
        circuit,
        DkongInputs {
            dac,
            walk_en,
            jump_en,
            stomp_en,
            discharge,
            mix,
        },
    )
}

/// Donkey Kong discrete sound device: the DAC stream summed with the walk, jump,
/// and stomp effects inside a [`DiscreteCircuit`].
pub struct DkongDiscreteSound {
    circuit: DiscreteCircuit,
    ids: DkongInputs,
    /// 74LS259 sound control latch bits 0-2 (walk/jump/stomp).
    latch: u8,
    discharge: bool,
}

impl DkongDiscreteSound {
    pub fn new() -> Self {
        let (circuit, ids) = build_circuit();
        Self {
            circuit,
            ids,
            latch: 0,
            discharge: false,
        }
    }

    /// Set a sound-control latch bit (0 = walk, 1 = jump, 2 = stomp).
    pub fn write_sound_bit(&mut self, bit: u8, value: bool) {
        if value {
            self.latch |= 1 << bit;
        } else {
            self.latch &= !(1 << bit);
        }
        match bit {
            0 => self.circuit.set_logic(self.ids.walk_en, value),
            1 => self.circuit.set_logic(self.ids.jump_en, value),
            2 => self.circuit.set_logic(self.ids.stomp_en, value),
            _ => {}
        }
    }

    /// Feed one box-filtered I8035 DAC sample (signed i16) and advance the
    /// circuit by one step, producing one output sample.
    pub fn feed_dac(&mut self, sample: i16) {
        self.circuit
            .set_external(self.ids.dac, sample as f64 / 32767.0 * DAC_GAIN);
        self.circuit.tick(1);
    }

    /// Set the discharge/mute control line.
    pub fn set_discharge(&mut self, value: bool) {
        self.discharge = value;
        self.circuit.set_logic(self.ids.discharge, value);
    }

    /// Drain produced mono `i16` samples. Returns the number written.
    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize {
        self.circuit.fill_audio(out)
    }

    pub fn reset(&mut self) {
        self.circuit.reset();
        self.latch = 0;
        self.discharge = false;
    }
}

impl Default for DkongDiscreteSound {
    fn default() -> Self {
        Self::new()
    }
}

impl phosphor_core::device::Device for DkongDiscreteSound {
    fn name(&self) -> &'static str {
        "DK Discrete"
    }
    fn reset(&mut self) {
        self.reset();
    }
}

impl Saveable for DkongDiscreteSound {
    fn save_state(&self, w: &mut StateWriter) {
        w.write_version(1);
        w.write_u8(self.latch);
        w.write_bool(self.discharge);
        self.circuit.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        r.read_version(1)?;
        self.latch = r.read_u8()?;
        self.discharge = r.read_bool()?;
        self.circuit.load_state(r)?;
        Ok(())
    }
}

impl Debuggable for DkongDiscreteSound {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        let bit = |b: u8| ((self.latch >> b) & 1) as u64;
        let sample = |v: f64| (v.clamp(-1.0, 1.0) * 32767.0) as i16 as u16 as u64;
        vec![
            DebugRegister {
                name: "LATCH",
                value: self.latch as u64,
                width: 8,
            },
            DebugRegister {
                name: "WALK",
                value: bit(0),
                width: 8,
            },
            DebugRegister {
                name: "JUMP",
                value: bit(1),
                width: 8,
            },
            DebugRegister {
                name: "STOMP",
                value: bit(2),
                width: 8,
            },
            DebugRegister {
                name: "DAC",
                value: sample(self.circuit.value(self.ids.dac)),
                width: 16,
            },
            DebugRegister {
                name: "DISCHARGE",
                value: self.discharge as u64,
                width: 8,
            },
            DebugRegister {
                name: "MIX_OUT",
                value: sample(self.circuit.value(self.ids.mix)),
                width: 16,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run `n` output samples with a fixed DAC value.
    fn run(s: &mut DkongDiscreteSound, dac: i16, n: usize) {
        for _ in 0..n {
            s.feed_dac(dac);
        }
    }

    fn drain_rms(s: &mut DkongDiscreteSound) -> f64 {
        let mut buf = vec![0i16; 8192];
        let n = s.fill_audio(&mut buf);
        if n == 0 {
            return 0.0;
        }
        let sum: f64 = buf[..n].iter().map(|&v| (v as f64).powi(2)).sum();
        (sum / n as f64).sqrt()
    }

    /// The DAC reaches the output, and its *DC* does not.
    ///
    /// This used to assert that a held DAC value settled to a constant at the
    /// output, which is exactly the defect the output coupling capacitor now
    /// removes: the I8035 DAC is unipolar, so a held code is a pedestal, and
    /// letting it through put 94 % of the board's energy below 150 Hz and
    /// swamped the walk/jump/stomp voices. A coupling capacitor passes the step
    /// and then decays to zero, which is what a real one does and what this now
    /// checks.
    #[test]
    fn the_dac_reaches_the_output_but_its_dc_does_not() {
        let mut s = DkongDiscreteSound::new();
        // The coupling network is 11.2 kΩ / 4.7 µF, so τ ≈ 53 ms. Hold the code
        // for a second — about twenty time constants — so the decay is fully
        // resolved rather than caught mid-slope.
        run(&mut s, 10_000, 44_100);
        let mut buf = vec![0i16; 65_536];
        let n = s.fill_audio(&mut buf);
        assert!(n > 1000, "expected about a second of audio, got {n}");

        // The step itself gets through: the output moves when the DAC does.
        let peak = buf[..n].iter().map(|v| v.abs()).max().unwrap();
        assert!(
            peak > (10_000.0 * DAC_GAIN * 0.5) as i16,
            "the DAC step should reach the output, peak was {peak}"
        );

        // And the held level decays away rather than sitting there as a pedestal.
        let tail_slice = &buf[n - 200..n];
        let tail = (tail_slice.iter().map(|&v| (v as f64).powi(2)).sum::<f64>()
            / tail_slice.len() as f64)
            .sqrt();
        assert!(
            tail < peak as f64 * 0.05,
            "a held DAC code must not survive as DC: peak {peak}, tail RMS {tail:.1}"
        );
    }

    #[test]
    fn effects_add_on_top_of_dac() {
        // The stomp is one of the louder effects (walk is intentionally near
        // inaudible, matching real DK), so use it to check effects reach the mix.
        let mut s = DkongDiscreteSound::new();
        s.write_sound_bit(2, true); // stomp
        run(&mut s, 0, 200);
        assert!(
            drain_rms(&mut s) > 100.0,
            "stomp should be audible over silence"
        );
    }

    #[test]
    fn jump_and_stomp_are_edge_triggered_one_shots() {
        for bit in [1u8, 2u8] {
            let mut s = DkongDiscreteSound::new();
            s.write_sound_bit(bit, true); // rising edge triggers
            run(&mut s, 0, 1000);
            assert!(drain_rms(&mut s) > 50.0, "bit {bit} one-shot should sound");
        }
    }

    #[test]
    fn debug_registers_reflect_latch() {
        let mut s = DkongDiscreteSound::new();
        s.write_sound_bit(1, true);
        let regs = s.debug_registers();
        let get = |n: &str| regs.iter().find(|r| r.name == n).unwrap().value;
        assert_eq!(get("LATCH"), 0x02);
        assert_eq!(get("JUMP"), 1);
        assert_eq!(get("WALK"), 0);
    }

    #[test]
    fn save_load_round_trip() {
        let mut s1 = DkongDiscreteSound::new();
        s1.write_sound_bit(0, true);
        s1.write_sound_bit(1, true);
        run(&mut s1, 5_000, 300);
        let mut discard = vec![0i16; 8192];
        while s1.fill_audio(&mut discard) > 0 {}

        let mut w = StateWriter::new();
        s1.save_state(&mut w);
        let data = w.into_vec();

        let mut s2 = DkongDiscreteSound::new();
        let mut r = StateReader::new(&data);
        s2.load_state(&mut r).unwrap();
        assert_eq!(s2.latch, s1.latch);
        assert_eq!(s2.circuit.value(s2.ids.mix), s1.circuit.value(s1.ids.mix));

        run(&mut s1, 5_000, 100);
        run(&mut s2, 5_000, 100);
        let mut a = vec![0i16; 8192];
        let mut b = vec![0i16; 8192];
        let na = s1.fill_audio(&mut a);
        let nb = s2.fill_audio(&mut b);
        assert_eq!(na, nb);
        assert_eq!(a[..na], b[..nb]);
    }
}
