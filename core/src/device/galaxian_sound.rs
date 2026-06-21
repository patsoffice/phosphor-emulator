//! Galaxian custom discrete sound device (Namco, 1979).
//!
//! The Tier-1 Galaxian sound board is an all-analog circuit driven by three CPU
//! register ports. It is expressed here on the shared
//! [`discrete`](crate::device::discrete) netlist framework rather than as a
//! hand-rolled mixer, so the topology reads like the schematic and reuses the
//! framework's filters, oscillators, and resampler.
//!
//! Register interface (matches MAME's `galaxian_sound_device`):
//!   * [`pitch_w`](GalaxianSound::pitch_w) — 8-bit background-melody pitch; the
//!     note clock is `SOUND_CLOCK / (256 - pitch)`, decoded into three square
//!     tones (the 74393 QA/QC/QD taps).
//!   * [`lfo_freq_w`](GalaxianSound::lfo_freq_w) — 4 lines forming the
//!     background DAC value that sweeps the three background oscillators (the
//!     "wolf-whistle").
//!   * [`sound_w`](GalaxianSound::sound_w) — a 74LS259 latch: lines 0-2 enable
//!     the background oscillators FS1-FS3, line 3 the HIT/explosion, line 5 the
//!     FIRE/shoot, lines 6-7 the VOL1/VOL2 mixer-volume switches.
//!
//! The four audible voices the issue calls out — background tone, shoot/noise,
//! hit, and the wolf-whistle LFO — are each a small sub-graph below.
//!
//! APPROXIMATION (flagged per the issue): MAME models the exact NE555 VCOs,
//! op-amp band-pass, and CD4066 switch impedances component-by-component. The
//! discrete framework has no 555/op-amp primitives, so the oscillators and
//! envelopes here are framework primitives (square/triangle + RC envelope +
//! state-variable band-pass + 1-pole RC filters) whose levels and filter
//! cutoffs were calibrated against a MAME `galaxian` capture using the
//! `tools/sound-reference` rig (`analyze_wav.py --galaxian`). Per-voice
//! spectral centroids and RMS levels track MAME closely for the tune and
//! wolf-whistle; the fire and hit are close but a touch less bright / less dark
//! respectively, the residual limit of 1-pole filters versus the real op-amp
//! network. The register *interface* and voice *structure* are faithful.

use crate::core::debug::{DebugRegister, Debuggable};
use crate::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use crate::device::Device;
use crate::device::discrete::{
    ClockDomain, CustomComponent, DataInputId, DiscreteCircuit, DiscreteCircuitBuilder, LfsrSpec,
    LogicInputId, OutputGain,
};

/// Galaxian master clock is 18.432 MHz; the sound section runs at /6/2.
const SOUND_CLOCK: f64 = 18_432_000.0 / 6.0 / 2.0; // 1.536 MHz
/// Main-CPU clock; [`GalaxianSound::tick`] is driven in these cycles.
pub const CPU_CLOCK_HZ: u64 = 3_072_000;
/// Internal simulation rate. High enough for the few-kHz tones and the
/// band-pass filters; the output is resampled down from here.
const SIM_RATE: u64 = 192_000;
/// Noise flip-flop sample rate (`2V` = 60·264/2 Hz on the real board).
const NOISE_RATE: f64 = 60.0 * 264.0 / 2.0; // 7920 Hz

// ---------------------------------------------------------------------------
// pitch → note-clock frequency (no divide primitive in the framework)
// ---------------------------------------------------------------------------

/// Converts the 8-bit pitch latch into the 74393 note-clock frequency
/// `SOUND_CLOCK / (256 - pitch)`, clamped to keep the derived square tones
/// below the simulation Nyquist.
struct PitchToFreq;

impl CustomComponent for PitchToFreq {
    fn reset(&mut self) {}

    fn step(&mut self, inputs: &[f64], _dt: f64) -> f64 {
        let pitch = inputs[0].round().clamp(0.0, 255.0);
        let freq = SOUND_CLOCK / (256.0 - pitch);
        // Cap so even the /2 tap stays well under SIM_RATE/2.
        freq.min(40_000.0)
    }

    fn save_state(&self, _w: &mut StateWriter) {}
    fn load_state(&mut self, _r: &mut StateReader) -> Result<(), SaveError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// GalaxianSound
// ---------------------------------------------------------------------------

/// The Galaxian custom sound board as a discrete circuit plus its register
/// latches.
pub struct GalaxianSound {
    circuit: DiscreteCircuit,

    // Input handles.
    pitch_in: DataInputId,
    bg_dac_in: DataInputId,
    fs_in: [LogicInputId; 3],
    hit_in: LogicInputId,
    fire_in: LogicInputId,
    vol_in: [LogicInputId; 2],

    // Shadowed latch state (for debug views, save state, and the lfo
    // read-modify-write).
    pitch: u8,
    lfo_val: u8,
    sound_latch: u8,
}

impl GalaxianSound {
    /// Build the device producing audio at `output_sample_rate` Hz.
    pub fn new(output_sample_rate: u32) -> Self {
        let mut b = DiscreteCircuitBuilder::new(CPU_CLOCK_HZ, output_sample_rate as u64)
            .with_sim_rate(SIM_RATE);

        // --- Inputs ---------------------------------------------------------
        let pitch_in = b.data_input("pitch", 1.0);
        let bg_dac_in = b.data_input("bg_dac", 1.0); // 0..15 wolf-whistle DAC
        let fs_in = [
            b.logic_input("fs1"),
            b.logic_input("fs2"),
            b.logic_input("fs3"),
        ];
        let hit_in = b.logic_input("hit");
        let fire_in = b.logic_input("fire");
        let vol_in = [b.logic_input("vol1"), b.logic_input("vol2")];

        // --- Shared noise source (17-bit LFSR sampled by the 2V flip-flop) --
        let noise = b.lfsr_noise(
            "noise",
            NOISE_RATE,
            LfsrSpec {
                width: 17,
                taps: (16, 13),
                seed: 0x1_FFFF,
            },
        );

        // --- Background tone: pitched 74393 tap chord ------------------------
        // Note clock, then the QA(/2), QC(/8), QD(/16) decoded square taps.
        let note_clk = b.custom("note_clk", vec![pitch_in.into()], Box::new(PitchToFreq));
        let f_qa = b.gain("f_qa", note_clk, 0.5);
        let f_qc = b.gain("f_qc", note_clk, 0.125);
        let f_qd = b.gain("f_qd", note_clk, 0.0625);
        let qa = b.variable_square("qa", f_qa);
        let qc = b.variable_square("qc", f_qc);
        let qd = b.variable_square("qd", f_qd);
        // MAME mixes QA, QC, QC, QD (QC weighted twice).
        let tune = b.add("tune", &[qa, qc, qc, qd]);
        let tune_lvl = b.gain("tune_lvl", tune, 0.16);

        // --- Wolf-whistle: three background oscillators swept by the DAC -----
        // Each FS oscillator's frequency rises with the 0..15 DAC value; the
        // game ramps the DAC to produce the rising/falling whistle. Bases follow
        // the FS1<FS2<FS3 ordering of the schematic's astable resistors. The
        // schematic's 555 astables are heavily filtered by the bck mixer cap, so
        // we use triangle waves (few harmonics) + a low-pass to match MAME's
        // dark (~2 kHz centroid) whistle rather than a harsh square stack.
        let bg_bases = [150.0, 190.0, 270.0];
        let mut bg_taps = Vec::with_capacity(3);
        for i in 0..3 {
            let base = b.constant("fs_base", bg_bases[i]);
            // freq = base * (1 + dac/5)  →  up to ~4× at dac=15.
            let swept = b.gain("fs_swept", bg_dac_in, bg_bases[i] / 5.0);
            let freq = b.add("fs_freq", &[base, swept]);
            let osc = b.variable_triangle("fs_osc", freq);
            let gated = b.multiply("fs_gated", osc, fs_in[i]);
            bg_taps.push(gated);
        }
        let bg = b.add("bg", &bg_taps);
        let bg_filt = b.low_pass_hz("bg_lp", bg, 2000.0);
        let bg_lvl = b.gain("bg_lvl", bg_filt, 0.27);

        // Pre-mix of melody + background, scaled by the VOL1/VOL2 switches.
        let pre = b.add("pre", &[tune_lvl, bg_lvl]);
        let vol_base = b.constant("vol_base", 0.4);
        let vol1_g = b.gain("vol1_g", vol_in[0], 0.3);
        let vol2_g = b.gain("vol2_g", vol_in[1], 0.3);
        let vol_gain = b.add("vol_gain", &[vol_base, vol1_g, vol2_g]);
        let pre_vol = b.multiply("pre_vol", pre, vol_gain);

        // --- HIT / explosion: enveloped noise, band-pass then low-pass -------
        // MAME's hit is a loud, dark (~1.5 kHz centroid) rumble; the op-amp band
        // filter + mixer caps roll off the noise hiss, so we band-pass the
        // enveloped noise and low-pass the result.
        // A soft (~4 ms) attack avoids click transients that would brighten the
        // rumble; ~0.3 s decay.
        let hit_env = b.rc_envelope("hit_env", hit_in, 0.004, 0.30);
        let hit_noise = b.multiply("hit_noise", noise, hit_env);
        let hit_bp = b.band_pass("hit_bp", hit_noise, 160.0, 1.2);
        let hit_lp = b.low_pass_hz("hit_lp", hit_bp, 520.0);
        let hit_out = b.gain("hit_lvl", hit_lp, 5.4);

        // --- FIRE / shoot: bright noise-FM zap -------------------------------
        // MAME's fire is a very bright (~12 kHz centroid) noise burst: a 555 VCO
        // (~2.6 kHz) whose control voltage is driven by the LFSR noise, so the
        // pitch jumps around and spreads energy high. We mirror that with a
        // high VCO heavily frequency-modulated by noise, plus raw noise, then
        // high-pass to keep it bright, enveloped with a ~80 ms decay.
        let fire_env = b.rc_envelope("fire_env", fire_in, 0.0005, 0.08);
        let fire_base = b.constant("fire_base", 3400.0);
        let fire_sweep = b.gain("fire_sweep", fire_env, 1400.0);
        let fire_fm = b.gain("fire_fm", noise, 5000.0);
        let fire_freq_raw = b.add("fire_freq_raw", &[fire_base, fire_sweep, fire_fm]);
        let fire_freq = b.clamp("fire_freq", fire_freq_raw, 300.0, 18000.0);
        let fire_osc = b.variable_square("fire_osc", fire_freq);
        let fire_tone = b.gain("fire_tone", fire_osc, 0.46);
        let fire_grit = b.gain("fire_grit", noise, 0.46);
        let fire_src = b.add("fire_src", &[fire_tone, fire_grit]);
        // High-pass (~8 kHz) to brighten toward MAME's ~12 kHz centroid.
        let fire_hp = b.rc_high_pass("fire_hp", fire_src, 2_000.0, 1e-8);
        let fire_out = b.multiply("fire_out", fire_hp, fire_env);

        // --- Final passive resistor mix (schematic R34/R40/R43, load R91) ----
        let mix = b.resistor_mixer(
            "mix",
            &[
                (pre_vol, 5_100.0),  // R34 background/melody
                (hit_out, 2_200.0),  // R40 hit
                (fire_out, 2_200.0), // R43 fire
            ],
            Some(10_000.0), // R91 load
        );
        b.output(mix, OutputGain::linear(1.6));

        // The oscillators/filters run at the simulation rate; the resampler
        // tap is per output sample.
        b.set_domain(mix, ClockDomain::BoardCycle);

        Self {
            circuit: b.build(),
            pitch_in,
            bg_dac_in,
            fs_in,
            hit_in,
            fire_in,
            vol_in,
            pitch: 0,
            lfo_val: 0,
            sound_latch: 0,
        }
    }

    // -----------------------------------------------------------------------
    // Register interface
    // -----------------------------------------------------------------------

    /// Write the 8-bit background-melody pitch (port `0x7800`).
    pub fn pitch_w(&mut self, data: u8) {
        self.pitch = data;
        self.circuit.set_data(self.pitch_in, data as f64);
    }

    /// Write one of the four LFO/background-DAC lines (ports `0x6004-0x6007`).
    /// `offset` selects the bit; `data` bit 0 is its value.
    pub fn lfo_freq_w(&mut self, offset: u8, data: u8) {
        let bit = offset & 3;
        let new = (self.lfo_val & !(1 << bit)) | ((data & 1) << bit);
        if new != self.lfo_val {
            self.lfo_val = new;
            self.circuit.set_data(self.bg_dac_in, new as f64);
        }
    }

    /// Write the 74LS259 sound latch (ports `0x6800-0x6807`). `offset` selects
    /// the line; `data` bit 0 is its value.
    pub fn sound_w(&mut self, offset: u8, data: u8) {
        let line = offset & 7;
        let on = data & 1 != 0;
        if on {
            self.sound_latch |= 1 << line;
        } else {
            self.sound_latch &= !(1 << line);
        }
        match line {
            0..=2 => self.circuit.set_logic(self.fs_in[line as usize], on),
            3 => self.circuit.set_logic(self.hit_in, on),
            4 => {} // not connected
            5 => self.circuit.set_logic(self.fire_in, on),
            6..=7 => self.circuit.set_logic(self.vol_in[(line - 6) as usize], on),
            _ => unreachable!(),
        }
    }

    // -----------------------------------------------------------------------
    // Audio
    // -----------------------------------------------------------------------

    /// Advance the circuit by `cpu_cycles` of main-CPU time.
    pub fn tick(&mut self, cpu_cycles: u64) {
        self.circuit.tick(cpu_cycles);
    }

    /// Drain produced mono `i16` samples. Returns the number written.
    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize {
        self.circuit.fill_audio(out)
    }

    /// The audio output rate in Hz.
    pub fn sample_rate(&self) -> u32 {
        self.circuit.sample_rate()
    }

    fn reset_impl(&mut self) {
        self.circuit.reset();
        self.pitch = 0;
        self.lfo_val = 0;
        self.sound_latch = 0;
    }
}

// ---------------------------------------------------------------------------
// Device / Debuggable / Saveable
// ---------------------------------------------------------------------------

impl Device for GalaxianSound {
    fn name(&self) -> &'static str {
        "Galaxian Sound"
    }

    fn reset(&mut self) {
        self.reset_impl();
    }
}

impl Debuggable for GalaxianSound {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "PITCH",
                value: self.pitch as u64,
                width: 8,
            },
            DebugRegister {
                name: "LFO",
                value: self.lfo_val as u64,
                width: 4,
            },
            DebugRegister {
                name: "LATCH",
                value: self.sound_latch as u64,
                width: 8,
            },
        ]
    }
}

impl Saveable for GalaxianSound {
    fn save_state(&self, w: &mut StateWriter) {
        self.circuit.save_state(w);
        w.write_u8(self.pitch);
        w.write_u8(self.lfo_val);
        w.write_u8(self.sound_latch);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.circuit.load_state(r)?;
        self.pitch = r.read_u8()?;
        self.lfo_val = r.read_u8()?;
        self.sound_latch = r.read_u8()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 44_100;

    /// Run the device for `ms` milliseconds of CPU time and return the produced
    /// samples.
    fn render(dev: &mut GalaxianSound, ms: u64) -> Vec<i16> {
        let cycles = CPU_CLOCK_HZ * ms / 1000;
        dev.tick(cycles);
        let mut buf = vec![0i16; (RATE as u64 * (ms + 2) / 1000) as usize];
        let n = dev.fill_audio(&mut buf);
        buf.truncate(n);
        buf
    }

    fn rms(samples: &[i16]) -> f64 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
        (sum / samples.len() as f64).sqrt()
    }

    #[test]
    fn produces_samples_at_output_rate() {
        let mut dev = GalaxianSound::new(RATE);
        assert_eq!(dev.sample_rate(), RATE);
        let out = render(&mut dev, 100);
        // ~44100 samples/s over 100 ms ≈ 4410 (allow resampler slack).
        assert!(
            (4000..4900).contains(&out.len()),
            "expected ~4410 samples, got {}",
            out.len()
        );
        assert!(out.iter().all(|&s| (-32768..=32767).contains(&(s as i32))));
    }

    #[test]
    fn fire_adds_energy_over_baseline() {
        let mut quiet = GalaxianSound::new(RATE);
        let baseline = rms(&render(&mut quiet, 60));

        let mut dev = GalaxianSound::new(RATE);
        dev.sound_w(5, 1); // FIRE on
        let fired = rms(&render(&mut dev, 60));
        assert!(
            fired > baseline + 100.0,
            "fire should raise RMS: baseline={baseline:.0} fired={fired:.0}"
        );
    }

    #[test]
    fn hit_adds_energy_over_baseline() {
        let mut quiet = GalaxianSound::new(RATE);
        let baseline = rms(&render(&mut quiet, 60));

        let mut dev = GalaxianSound::new(RATE);
        dev.sound_w(3, 1); // HIT on
        let hit = rms(&render(&mut dev, 60));
        assert!(
            hit > baseline + 100.0,
            "hit should raise RMS: baseline={baseline:.0} hit={hit:.0}"
        );
    }

    #[test]
    fn background_oscillators_need_enable() {
        // With the background DAC swept but no FS enable, the wolf-whistle
        // oscillators are gated off; enabling FS1 adds energy.
        let mut off = GalaxianSound::new(RATE);
        off.lfo_freq_w(0, 1);
        off.lfo_freq_w(1, 1);
        let off_rms = rms(&render(&mut off, 60));

        let mut on = GalaxianSound::new(RATE);
        on.lfo_freq_w(0, 1);
        on.lfo_freq_w(1, 1);
        on.sound_w(0, 1); // FS1 enable
        on.sound_w(1, 1); // FS2 enable
        on.sound_w(2, 1); // FS3 enable
        let on_rms = rms(&render(&mut on, 60));
        assert!(
            on_rms > off_rms + 50.0,
            "FS enable should add background energy: off={off_rms:.0} on={on_rms:.0}"
        );
    }

    #[test]
    fn lfo_latch_accumulates_bits() {
        let mut dev = GalaxianSound::new(RATE);
        dev.lfo_freq_w(0, 1);
        dev.lfo_freq_w(2, 1);
        assert_eq!(dev.lfo_val, 0b0101);
        dev.lfo_freq_w(0, 0);
        assert_eq!(dev.lfo_val, 0b0100);
    }

    #[test]
    fn sound_latch_tracks_lines() {
        let mut dev = GalaxianSound::new(RATE);
        dev.sound_w(0, 1);
        dev.sound_w(5, 1);
        assert_eq!(dev.sound_latch, 0b0010_0001);
        dev.sound_w(0, 0);
        assert_eq!(dev.sound_latch, 0b0010_0000);
    }

    #[test]
    fn pitch_changes_background_spectrum() {
        // Two different pitches should yield measurably different output (the
        // melody tone frequency tracks pitch).
        let mut lo = GalaxianSound::new(RATE);
        lo.pitch_w(0xA0);
        let a = render(&mut lo, 80);

        let mut hi = GalaxianSound::new(RATE);
        hi.pitch_w(0xF0);
        let b = render(&mut hi, 80);

        // Different pitch ⇒ different waveform ⇒ different sample stream.
        assert_ne!(a, b);
    }

    #[test]
    fn reset_clears_latches() {
        let mut dev = GalaxianSound::new(RATE);
        dev.pitch_w(0x80);
        dev.lfo_freq_w(1, 1);
        dev.sound_w(3, 1);
        Device::reset(&mut dev);
        assert_eq!(dev.pitch, 0);
        assert_eq!(dev.lfo_val, 0);
        assert_eq!(dev.sound_latch, 0);
    }

    #[test]
    fn save_load_round_trip() {
        let mut dev = GalaxianSound::new(RATE);
        dev.pitch_w(0xC4);
        dev.lfo_freq_w(2, 1);
        dev.sound_w(5, 1);
        dev.tick(5000);
        // Drain any buffered output before saving: the resampler's pending
        // output queue is deliberately not serialized, so a clean comparison
        // requires both devices to start from an empty buffer.
        let mut drain = vec![0i16; 4096];
        while dev.fill_audio(&mut drain) > 0 {}

        let mut w = StateWriter::new();
        dev.save_state(&mut w);
        let bytes = w.into_vec();

        let mut dev2 = GalaxianSound::new(RATE);
        let mut r = StateReader::new(&bytes);
        dev2.load_state(&mut r).unwrap();
        assert_eq!(dev2.pitch, 0xC4);
        assert_eq!(dev2.lfo_val, 0b0100);
        assert_eq!(dev2.sound_latch, 0b0010_0000);

        // Both continue identically from the restored state.
        let mut a = vec![0i16; 256];
        let mut bbuf = vec![0i16; 256];
        dev.tick(2000);
        dev2.tick(2000);
        let na = dev.fill_audio(&mut a);
        let nb = dev2.fill_audio(&mut bbuf);
        assert_eq!(na, nb);
        assert_eq!(a[..na], bbuf[..nb]);
    }
}
