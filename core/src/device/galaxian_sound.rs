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
//! This is a component-level port of MAME's `galaxian_discrete` netlist
//! (`mame/src/mame/galaxian/galaxian_a.cpp`) built on the framework's NE555 and
//! op-amp primitives with the real `GAL_R*`/`GAL_C*` schematic values, so the
//! oscillator waveforms and filter responses match the hardware rather than
//! approximating them. The wolf-whistle is a 555 constant-current VCO driving
//! three CV-modulated 555 astables; the HIT is a noise-gated op-amp
//! multiple-feedback band-pass; the FIRE is a noise-jittered 555 VCO gating an
//! RC discharge. Documented simplifications vs. MAME: the 555 sub-sample
//! threshold-crossing loop is dropped (faithful at the 192 kHz sim rate); the
//! per-tap pitch counter keeps the validated [`TuneVoice`] custom component
//! rather than a discrete `DISCRETE_NOTE`; and the VOL1/VOL2 switched-resistor
//! melody mix is modelled as a melody-level gain (the combined `TuneVoice`
//! output cannot be split per QC/QD tap). The output level is calibrated to
//! MAME's `32767/5` scale.

use crate::core::debug::{DebugRegister, Debuggable};
use crate::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use crate::device::Device;
use crate::device::discrete::{
    CustomComponent, DataInputId, DiscreteCircuit, DiscreteCircuitBuilder, LfsrSpec, LogicInputId,
    Output555, OutputGain,
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
// Background-melody voice (the pitched 74393 tap chord)
// ---------------------------------------------------------------------------

/// Frequencies above this (Hz) are inaudible (and would alias if synthesized at
/// the sim rate), so taps above it are muted rather than capped. This matters at
/// power-on / attract: the game parks the pitch latch near 0xFF, which makes the
/// note clock ~MHz and every tap ultrasonic — silent on real hardware. Capping
/// the clock instead would fold that into a constant audible tone.
const AUDIBLE_CEILING: f64 = 16_000.0;

/// The background melody: a note clock `SOUND_CLOCK / (256 - pitch)` feeding a
/// 74393 counter whose QA(/2), QC(/8), QD(/16) taps are summed (QC weighted
/// twice, matching MAME's mixer). Synthesized directly so ultrasonic taps can be
/// muted (see [`AUDIBLE_CEILING`]) instead of aliased.
#[derive(Default)]
struct TuneVoice {
    phase: [f64; 3],
}

impl TuneVoice {
    const TAPS: [f64; 3] = [0.5, 0.125, 0.0625]; // QA, QC, QD divisors
    const WEIGHTS: [f64; 3] = [1.0, 2.0, 1.0]; // QC mixed twice
}

impl CustomComponent for TuneVoice {
    fn reset(&mut self) {
        self.phase = [0.0; 3];
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let pitch = inputs[0].round().clamp(0.0, 255.0);
        let f_count = SOUND_CLOCK / (256.0 - pitch);
        let mut out = 0.0;
        for i in 0..3 {
            let f = f_count * Self::TAPS[i];
            if f <= AUDIBLE_CEILING {
                self.phase[i] = (self.phase[i] + f * dt).fract();
                let square = if self.phase[i] < 0.5 { 1.0 } else { -1.0 };
                out += square * Self::WEIGHTS[i];
            }
            // else: ultrasonic tap is inaudible — contributes nothing.
        }
        out
    }

    fn save_state(&self, w: &mut StateWriter) {
        for p in self.phase {
            w.write_f64_le(p);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        for p in self.phase.iter_mut() {
            *p = r.read_f64_le()?;
        }
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
        // taps (11, 0) give the full maximal 2^17-1 period for this Fibonacci
        // structure — true white noise. (Many "obvious" 17-bit pairs, e.g.
        // (16, 13), collapse to a tiny cycle here and buzz like a tone.)
        // MAME's NODE_150/152: the LFSR latched by the 2V D-flip-flop.
        let noise_pm = b.lfsr_noise(
            "noise",
            NOISE_RATE,
            // Taps chosen against the toward-zero shift, so they keep it.
            LfsrSpec::toward_zero(17, (11, 0), 0x1_FFFF),
        );
        // The HIT/FIRE stages gate on the noise as a logic level, so map the
        // ±1 LFSR to the 0/1 line the D-flip-flop (NODE_152) presents.
        let noise_half = b.gain("noise_half", noise_pm, 0.5);
        let noise_bias = b.constant("noise_bias", 0.5);
        let noise = b.add("noise01", &[noise_half, noise_bias]);

        // --- Background / wolf-whistle (NODE_100..120) ----------------------
        // R-1 ladder DAC (galaxian_bck_dac): each bit switches its resistor
        // between TTL_OUT and ground; a bias branch and a ground branch fix the
        // operating point. Every resistor is always connected, so the Millman
        // denominator is constant and the node voltage is affine in the code:
        // V = offset + Σ bit_i·w_i.
        let r_dac = [1.0e6, 470.0e3, 220.0e3, 100.0e3]; // R18,R17,R16,R15 (bit0..3)
        let r_bias = 15.0e3; // R20
        let r_gnd = 330.0e3; // R19
        let v_bias = 4.4;
        let v_on = 4.0; // TTL_OUT
        let dac_denom = r_dac.iter().map(|r| 1.0 / r).sum::<f64>() + 1.0 / r_bias + 1.0 / r_gnd;
        let dac_weights: Vec<f64> = r_dac.iter().map(|r| v_on / (r * dac_denom)).collect();
        let dac_bits = b.dac_weighted("bg_dac_bits", bg_dac_in, &dac_weights);
        let dac_off = b.constant("bg_dac_off", v_bias / (r_bias * dac_denom));
        let bg_dac_v = b.add("bg_dac_v", &[dac_bits, dac_off]); // NODE_100

        // 555 constant-current VCO (galaxian_bck_vco): a slow (<10 Hz) sawtooth
        // on C15, the wolf-whistle's sweep source. Output is the cap voltage.
        let bg_vco = b.ne555_cc(
            "bg_vco",
            bg_dac_v,
            100.0e3, // R21
            1.0e-6,  // C15
            5.0,
            5.0,
            0.7, // vcc, v_cc_source, Q2 junction
            Output555::Capacitor,
        ); // NODE_105

        // Op-amp mult/add (NODE_110): v·gain + offset, derived from R31/R32/R33.
        let (r31, r32, r33) = (47.0e3, 47.0e3, 10.0e3);
        let r3par = 1.0 / (1.0 / r31 + 1.0 / r32 + 1.0 / r33);
        let bg_ma_gain = b.gain("bg_ma_gain", bg_vco, r33 / r3par);
        let bg_ma_off = b.constant("bg_ma_off", -5.0 * r33 / r31);
        let bg_ma = b.add("bg_ma", &[bg_ma_gain, bg_ma_off]);
        let bg_cv = b.clamp("bg_cv", bg_ma, 0.0, 5.0); // NODE_111, the FS control voltage

        // Three CV-modulated 555 astables (galaxian_555_vco_desc, energy output
        // ≡ square once the sub-sample x_time is dropped; v_out_high = 4.5 V).
        // Frequencies set by (R22/R23/C17), (R25/R26/C18), (R28/R29/C19).
        let fs_rc = [
            (100.0e3, 470.0e3, 0.01e-6), // FS1
            (100.0e3, 330.0e3, 0.01e-6), // FS2
            (100.0e3, 220.0e3, 0.01e-6), // FS3
        ];
        let mut fs_taps = Vec::with_capacity(3);
        for (i, &(ra, rb, c)) in fs_rc.iter().enumerate() {
            let osc = b.ne555_astable(
                "fs_osc",
                Some(bg_cv),
                ra,
                rb,
                c,
                5.0,
                4.5,
                Output555::Square,
            );
            // The astable's enable input (FSi) gates its contribution.
            let gated = b.multiply("fs_gated", osc, fs_in[i]);
            fs_taps.push((gated, 10.0e3)); // R24/R27/R30 mixer resistors
        }
        // bck mixer (R24/R27/R30) with the C20 output cap low-passing the stack.
        let bg_mix = b.resistor_mixer("bg_mix", &fs_taps, None);
        let bg = b.rc_low_pass("bg", bg_mix, 10.0e3 / 3.0, 0.1e-6); // NODE_120

        // --- Pitch / melody (NODE_132/133): the validated TuneVoice ---------
        let tune = b.custom(
            "tune",
            vec![pitch_in.into()],
            Box::new(TuneVoice::default()),
        );

        // --- HIT (NODE_155/157): noise-gated op-amp band-pass ----------------
        // RCDISC5 gated by the noise (enable) with the HIT TTL level as input,
        // then the op-amp multiple-feedback band-pass (~168 Hz).
        let hit4 = b.gain("hit4", hit_in, v_on); // GAL_INP_HIT = TTL_OUT·hit
        let hit_rc = b.rc_disc5("hit_rc", hit4, noise, 150.0e3 + 22.0e3, 2.2e-6); // R35+R36, C21
        let hit_vref = 5.0 * 22.0e3 / (33.0e3 + 22.0e3); // 5·R39/(R38+R39) = 2.0
        let hit_bp = b.op_amp_band_pass(
            "hit_bp",
            hit_rc,
            &[150.0e3, 22.0e3], // R35, R36
            470.0e3,            // R37
            0.01e-6,
            0.01e-6, // C22, C23
            hit_vref,
            0.0,
            5.0, // vRef, vN, vP (galaxian_bandpass_desc)
        ); // NODE_157
        // The band-pass output sits on the op-amp's vRef rail; the downstream
        // output coupling (cAmp) removes the DC, so strip it here for mixing.
        let hit_vref_off = b.constant("hit_vref_off", -hit_vref);
        let hit_ac = b.add("hit_ac", &[hit_bp, hit_vref_off]);
        // Calibration trim: MAME's own netlist flags the hit as too quiet ("op-amp
        // band filter" note, R40 0.6 trim); the capture confirms our band-pass
        // output runs ~1.25x low, so lift it to match the explosion's punch.
        let hit = b.gain("hit", hit_ac, 1.25);

        // --- FIRE (NODE_170..182): noise-jittered 555 VCO gating an RC -------
        let fire4 = b.gain("fire4", fire_in, v_on); // NODE_171 = TTL_OUT·fire
        // NODE_172 = TTL_OUT·(1-fire), low-passed by R47/C28 -> NODE_173 bias.
        let fire_inv_g = b.gain("fire_inv_g", fire_in, -v_on);
        let fire_inv_b = b.constant("fire_inv_b", v_on);
        let fire_inv = b.add("fire_inv", &[fire_inv_g, fire_inv_b]);
        let fire_bias = b.rc_low_pass("fire_bias", fire_inv, 2.2e3, 47.0e-6); // R47, C28
        // NODE_178 control voltage: noise·c1 + bias·c2 (the R46/R48 summing net).
        let (r46, r48) = (10.0e3, 2.2e3);
        let r2par_fire = 1.0 / (1.0 / r46 + 1.0 / r48);
        let fire_cv_n = b.gain("fire_cv_n", noise, r2par_fire * v_on / r46);
        let fire_cv_b = b.gain("fire_cv_b", fire_bias, r2par_fire / r48);
        let fire_cv = b.add("fire_cv", &[fire_cv_n, fire_cv_b]); // NODE_178
        // 555 fire VCO (galaxian_555_fire_vco_desc, v_out_high = 1.0 logic).
        let fire_vco = b.ne555_astable(
            "fire_vco",
            Some(fire_cv),
            10.0e3,
            22.0e3,
            0.01e-6, // R44, R45, C27
            5.0,
            1.0,
            Output555::Square,
        ); // NODE_181
        // The VCO square gates the fire envelope through RCDISC5 (R41/C25).
        let fire = b.rc_disc5("fire", fire4, fire_vco, 100.0e3, 1.0e-6); // NODE_182

        // --- Pre-mix: melody + background, VOL1/VOL2 switched gain (NODE_279) -
        // The CD4066 VOL switches add parallel melody resistors (R49/R52). With
        // the combined TuneVoice we model this as a melody-level gain rather
        // than per-tap conductance switching.
        let vol_base = b.constant("vol_base", 0.25);
        let vol1_g = b.gain("vol1_g", vol_in[0], 0.3);
        let vol2_g = b.gain("vol2_g", vol_in[1], 0.3);
        let vol_gain = b.add("vol_gain", &[vol_base, vol1_g, vol2_g]);
        let tune_vol = b.multiply("tune_vol", tune, vol_gain);
        // The melody mixes through R51 (QA) and R50 (QC), plus R49 (QC) and R52
        // (QD) switched in by VOL1/VOL2 — both on gives ~4.1k in parallel. Using
        // that parallel value (with vol_gain modelling the switch level) keeps
        // the melody-vs-background balance MAME has; a lone R51 buries the tune.
        let r_melody = 1.0 / (1.0 / 33.0e3 + 1.0 / 22.0e3 + 1.0 / 10.0e3 + 1.0 / 15.0e3);
        let pre = b.resistor_mixer("pre", &[(tune_vol, r_melody), (bg, 5.1e3)], None);

        // --- Final mix (NODE_280): R34/R40/R43 into the R91 load -------------
        let (r34, r40, r43, r91) = (5.1e3, 2.2e3 * 0.6, 2.2e3, 10.0e3); // R40 has a 0.6 volume trim
        // Fire channel is AC-coupled by C26 (≈8.8 kHz with R43‖R91) before mixing.
        let fire_couple_r = 1.0 / (1.0 / r43 + 1.0 / r91);
        let fire_ac = b.rc_high_pass("fire_ac", fire, fire_couple_r, 0.01e-6); // C26
        let mix = b.resistor_mixer("mix", &[(pre, r34), (hit, r40), (fire_ac, r43)], Some(r91));
        // Output coupling cap (cAmp = C46) — MAME models it as a ≈16 Hz DC block
        // against a 100 k final-stage impedance.
        let out = b.rc_high_pass("out", mix, 100.0e3, 0.1e-6); // C46
        // Calibrated to MAME's 32767/5 output scale.
        b.output(out, OutputGain::linear(0.8));

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
    fn ultrasonic_pitch_is_silent() {
        // At power-on / attract the game parks the pitch latch high (256-pitch
        // ≈ 1), making every note tap ultrasonic — silent on real hardware.
        // Capping the note clock instead would alias it into a constant audible
        // tone, which is the regression this guards.
        let mut hi = GalaxianSound::new(RATE);
        hi.pitch_w(0xFF);
        hi.sound_w(6, 1); // VOL1
        hi.sound_w(7, 1); // VOL2
        let silent = rms(&render(&mut hi, 80));
        assert!(
            silent < 50.0,
            "ultrasonic pitch should be silent, rms={silent:.0}"
        );

        // An audible pitch at the same volume produces real output.
        let mut lo = GalaxianSound::new(RATE);
        lo.pitch_w(0xB0);
        lo.sound_w(6, 1);
        lo.sound_w(7, 1);
        let audible = rms(&render(&mut lo, 80));
        assert!(
            audible > 300.0,
            "audible pitch should sound, rms={audible:.0}"
        );
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
