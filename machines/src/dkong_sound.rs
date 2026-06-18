//! Donkey Kong (TKG-04) discrete sound, built on the [`DiscreteCircuit`]
//! framework. The I8035 DAC stream enters the circuit as an external source and
//! is summed with the three discrete analog effects (walk, jump, stomp) inside
//! the circuit — replacing the old "add the effect sample to the resampled DAC
//! as finished PCM" path. This puts the DAC + effects mixing inside the
//! framework so the shared analog stages (mixer, filters, discharge) have a home.
//!
//! The walk/jump/stomp models are ported from the previous `DkongDiscrete`
//! (closed-form 555 VCO + RC envelopes + LFSR noise); fidelity is unchanged. The
//! board talks to it with hardware intent (`write_sound_bit`, `feed_dac`,
//! `set_discharge`).

use phosphor_core::core::debug::{DebugRegister, Debuggable};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::device::{
    CustomComponent, DiscreteCircuit, DiscreteCircuitBuilder, ExternalSourceId, FilterMode,
    LogicInputId, NodeId, OutputGain,
};

/// Output sample rate. The circuit is built board = sim = output = this rate, so
/// `tick(1)` advances exactly one simulation step; the board drives one step per
/// box-filtered DAC sample.
const SAMPLE_RATE: u64 = 44_100;

// Per-effect output low-pass cutoffs (Hz) and gains, calibrated against MAME
// dkong captures (tools/sound-reference). MAME runs each effect through RC
// integrators and a coupling filter, so the raw squares/noise are darkened by
// the low-passes and balanced relative to each other as MAME does (walk subtle,
// jump moderate, stomp prominent). The absolute gains are then scaled up from
// MAME's levels because Phosphor's DAC plays at full scale while MAME's music
// sits ~5x lower (VR2 + mixer): matching MAME's effect/music *ratio* keeps the
// effects audible over the music rather than buried under it.
// The DAC (music) is attenuated to leave output headroom for the effects, the
// way MAME's VR2 volume pot + mixer do. At full scale the music consumed the
// entire range, so any effect loud enough to hear over it clipped against the
// output clamp. With headroom, the effects sit clearly above the music.
const DAC_GAIN: f64 = 0.55;
const WALK_LP_HZ: f64 = 900.0;
const WALK_GAIN: f64 = 1.0;
const JUMP_LP_HZ: f64 = 1_400.0;
const JUMP_GAIN: f64 = 1.1;
const STOMP_LP_HZ: f64 = 340.0;
const STOMP_GAIN: f64 = 7.0;

/// 555 astable frequency with external control voltage (DK sound board):
/// R1 = 47 kΩ (charge), R2 = 27 kΩ (discharge), Vcc = 5 V.
fn vco_freq(cap_nf: f64, cv: f64) -> f64 {
    const VCC: f64 = 5.0;
    const R1: f64 = 47_000.0;
    const R2: f64 = 27_000.0;
    let c = cap_nf * 1e-9;
    let t_charge = (R1 + R2) * c * ((VCC - cv * 0.5) / (VCC - cv)).ln();
    let t_discharge = R2 * c * 2.0_f64.ln();
    1.0 / (t_charge + t_discharge)
}

// ---------------------------------------------------------------------------
// Effect components (custom escape hatch)
// ---------------------------------------------------------------------------

/// Walk: a ~1 Hz inverter-oscillator LFO modulates a 555 VCO (~430 Hz) while the
/// enable is held. Input: `[walk_en]`.
struct DkongWalk {
    lfo_phase: f64,
    vco_phase: f64,
}

impl CustomComponent for DkongWalk {
    fn reset(&mut self) {
        self.lfo_phase = 0.0;
        self.vco_phase = 0.0;
    }
    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        if inputs[0] < 0.5 {
            return 0.0;
        }
        self.lfo_phase += dt; // ~1 Hz LFO
        self.lfo_phase -= self.lfo_phase.floor();
        let lfo = (self.lfo_phase * std::f64::consts::TAU).sin();
        let cv = 3.15 + 0.65 * lfo;
        let freq = vco_freq(33.0, cv);
        self.vco_phase += freq * dt;
        self.vco_phase -= self.vco_phase.floor();
        let wave = if self.vco_phase < 0.5 { 1.0 } else { -1.0 };
        wave * 0.12
    }
    fn save_state(&self, w: &mut StateWriter) {
        w.write_f64_le(self.lfo_phase);
        w.write_f64_le(self.vco_phase);
    }
    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.lfo_phase = r.read_f64_le()?;
        self.vco_phase = r.read_f64_le()?;
        Ok(())
    }
}

/// Jump: a one-shot 555 VCO sweep (220→700 Hz, τ ≈ 360 ms) triggered on the
/// rising edge of the enable. Input: `[jump_en]`.
struct DkongJump {
    active: bool,
    timer: f64,
    vco_phase: f64,
    last_en: bool,
}

impl CustomComponent for DkongJump {
    fn reset(&mut self) {
        self.active = false;
        self.timer = 0.0;
        self.vco_phase = 0.0;
        self.last_en = false;
    }
    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let en = inputs[0] > 0.5;
        if en && !self.last_en {
            self.active = true;
            self.timer = 0.0;
            self.vco_phase = 0.0;
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
        let t = self.timer;
        let cv = 1.0 + 3.0 * (-t / 0.36).exp();
        let freq = vco_freq(47.0, cv);
        let amp = (-t / 0.36).exp();
        self.vco_phase += freq * dt;
        self.vco_phase -= self.vco_phase.floor();
        let wave = if self.vco_phase < 0.5 { 1.0 } else { -1.0 };
        wave * amp * 0.15
    }
    fn save_state(&self, w: &mut StateWriter) {
        w.write_bool(self.active);
        w.write_f64_le(self.timer);
        w.write_f64_le(self.vco_phase);
        w.write_bool(self.last_en);
    }
    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.active = r.read_bool()?;
        self.timer = r.read_f64_le()?;
        self.vco_phase = r.read_f64_le()?;
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
    let mut b = DiscreteCircuitBuilder::new(SAMPLE_RATE, SAMPLE_RATE);

    let dac = b.external_source("DAC"); // normalized I8035 DAC stream
    let walk_en = b.logic_input("WALK_EN");
    let jump_en = b.logic_input("JUMP_EN");
    let stomp_en = b.logic_input("STOMP_EN");
    // Discharge control line — wired for the board-level mute/discharge path,
    // currently inert (the TKG-04 model does not drive it yet).
    let discharge = b.logic_input("DISCHARGE");

    // Each effect: raw oscillator/noise -> output low-pass (darken, as MAME's RC
    // integrators do) -> level gain.
    let walk_raw = b.custom(
        "WALK",
        vec![walk_en.into()],
        Box::new(DkongWalk {
            lfo_phase: 0.0,
            vco_phase: 0.0,
        }),
    );
    let walk_lp = b.second_order("WALK_LP", walk_raw, FilterMode::LowPass, WALK_LP_HZ, 0.707);
    let walk = b.gain("WALK_OUT", walk_lp, WALK_GAIN);

    let jump_raw = b.custom(
        "JUMP",
        vec![jump_en.into()],
        Box::new(DkongJump {
            active: false,
            timer: 0.0,
            vco_phase: 0.0,
            last_en: false,
        }),
    );
    let jump_lp = b.second_order("JUMP_LP", jump_raw, FilterMode::LowPass, JUMP_LP_HZ, 0.707);
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

    // Op-amp summing mixer: the DAC (full scale) plus the three filtered effects.
    let mix = b.add("MIX", &[dac.into(), walk, jump, stomp]);
    b.output(mix, OutputGain::unity());

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

    #[test]
    fn dac_passes_through_the_mix() {
        let mut s = DkongDiscreteSound::new();
        run(&mut s, 10_000, 100);
        let mut buf = vec![0i16; 256];
        let n = s.fill_audio(&mut buf);
        assert!(n > 0);
        // With no effects active, the output is the (attenuated) DAC value.
        let expected = (10_000.0 * DAC_GAIN) as i16;
        assert!(buf[..n].iter().all(|&v| v == expected));
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
