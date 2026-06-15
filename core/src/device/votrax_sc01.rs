//! Votrax SC-01 speech synthesizer.
//!
//! 64-phoneme formant synthesis chip used in Gottlieb arcade games (Q*Bert,
//! Reactor, etc.). Parameters for each phoneme are stored in a 512-byte
//! internal ROM. The synthesis pipeline models the analog circuit using
//! a glottal pulse generator, noise source, and 4 cascaded IIR formant
//! filters with switched-capacitor coefficients.
//!
//! # Interface
//!
//! - `write_phoneme(data)`: 6-bit phoneme code (bits 0-5)
//! - `set_inflection(data)`: 2-bit inflection (bits 0-1), modulates pitch
//! - `ar_output()`: A/R (Articulate/Request) pin — `true` = ready
//!
//! # Clock
//!
//! Typical main clock: 720 kHz (Gottlieb boards).
//! - sclock = main / 18 ≈ 40 kHz (audio sample rate)
//! - cclock = main / 36 ≈ 20 kHz (switched-capacitor filter clock)
//!
//! # References
//!
//! - MAME `src/devices/sound/votrax.cpp` (Olivier Galibert, BSD-3-Clause)
//! - US Patent 4,433,210 (switched capacitor filter technology)

use crate::audio::AudioResampler;
use crate::core::debug::{DebugRegister, Debuggable};
use phosphor_macros::Saveable;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Glottal pulse waveform from the SC-01's transistor resistor ladder.
///
/// Index 0 = middle value, index 1 = 0V, indices 2-8 = descending ladder
/// from peak. Indices 9+ map back to the middle value (0.0).
const GLOTTAL_WAVE: [f64; 9] = [
    0.0,
    -4.0 / 7.0,
    1.0,
    6.0 / 7.0,
    5.0 / 7.0,
    4.0 / 7.0,
    3.0 / 7.0,
    2.0 / 7.0,
    1.0 / 7.0,
];

/// Phoneme names for debug display.
#[allow(dead_code)]
const PHONE_NAMES: [&str; 64] = [
    "EH3", "EH2", "EH1", "PA0", "DT", "A1", "A2", "ZH", "AH2", "I3", "I2", "I1", "M", "N", "B",
    "V", "CH", "SH", "Z", "AW1", "NG", "AH1", "OO1", "OO", "L", "K", "J", "H", "G", "F", "D", "S",
    "A", "AY", "Y1", "UH3", "AH", "P", "O", "I", "U", "Y", "T", "R", "E", "W", "AE", "AE1", "AW2",
    "UH2", "UH1", "UH", "O2", "O1", "IU", "U1", "THV", "TH", "ER", "EH", "E1", "AW", "PA1", "STOP",
];

const OUTPUT_SAMPLE_RATE: u64 = 44_100;

// ---------------------------------------------------------------------------
// VotraxSc01
// ---------------------------------------------------------------------------

/// Votrax SC-01 speech synthesizer.
#[derive(Saveable)]
#[save_version(1)]
pub struct VotraxSc01 {
    // --- Inputs ---
    phone: u8,
    inflection: u8,

    // --- Outputs ---
    ar_state: bool,

    // --- ROM-extracted parameters for current phoneme ---
    rom_duration: u8,
    rom_vd: u8,
    rom_cld: u8,
    rom_fa: u8,
    rom_fc: u8,
    rom_va: u8,
    rom_f1: u8,
    rom_f2: u8,
    rom_f2q: u8,
    rom_f3: u8,
    rom_closure: bool,
    rom_pause: bool,

    // --- Interpolation registers (8-bit, exponential approach) ---
    cur_fa: u8,
    cur_fc: u8,
    cur_va: u8,
    cur_f1: u8,
    cur_f2: u8,
    cur_f2q: u8,
    cur_f3: u8,

    // --- Committed filter parameter values ---
    filt_fa: u8,
    filt_fc: u8,
    filt_va: u8,
    filt_f1: u8,
    filt_f2: u8, // 5 bits (cur_f2 >> 3)
    filt_f2q: u8,
    filt_f3: u8,

    // --- Timing counters ---
    phonetick: u16,
    ticks: u8,
    update_counter: u8,
    pitch: u8,
    closure: u8,
    sample_count: u32,
    main_divider: u8,
    cur_closure: bool,

    // --- Noise generator ---
    noise: u16,
    cur_noise: bool,

    // --- Commit/end-of-phone scheduling ---
    commit_pending: bool,
    commit_countdown: u32,
    end_phone_pending: bool,
    end_phone_countdown: u32,

    // --- Filter coefficients (IIR biquads) ---
    f1_a: [f64; 4],
    f1_b: [f64; 4],
    f2v_a: [f64; 4],
    f2v_b: [f64; 4],
    f2n_a: [f64; 2],
    f2n_b: [f64; 2],
    f3_a: [f64; 4],
    f3_b: [f64; 4],
    f4_a: [f64; 4],
    f4_b: [f64; 4],
    fx_a: [f64; 1],
    fx_b: [f64; 2],
    fn_a: [f64; 3],
    fn_b: [f64; 3],

    // --- Signal path histories ---
    voice_1: [f64; 4],
    voice_2: [f64; 4],
    voice_3: [f64; 4],
    noise_1: [f64; 3],
    noise_2: [f64; 3],
    noise_3: [f64; 2],
    noise_4: [f64; 2],
    vn_1: [f64; 4],
    vn_2: [f64; 4],
    vn_3: [f64; 4],
    vn_4: [f64; 4],
    vn_5: [f64; 2],
    vn_6: [f64; 2],

    // --- Non-serialized fields ---
    #[save_skip]
    rom: Vec<u8>,
    #[save_skip]
    main_clock_hz: u64,
    #[save_skip]
    sclock: f64,
    #[save_skip]
    cclock: f64,
    #[save_skip]
    resampler: AudioResampler<f32>,
}

impl VotraxSc01 {
    /// Create a new SC-01 with the given main clock frequency.
    ///
    /// Typical clocks: 720,000 Hz (Gottlieb), 722,534 Hz (others).
    pub fn new(main_clock_hz: u64) -> Self {
        let sclock = main_clock_hz as f64 / 18.0;
        Self {
            phone: 0x3F, // STOP
            inflection: 0,
            ar_state: true,
            rom_duration: 0,
            rom_vd: 0,
            rom_cld: 0,
            rom_fa: 0,
            rom_fc: 0,
            rom_va: 0,
            rom_f1: 0,
            rom_f2: 0,
            rom_f2q: 0,
            rom_f3: 0,
            rom_closure: false,
            rom_pause: false,
            cur_fa: 0,
            cur_fc: 0,
            cur_va: 0,
            cur_f1: 0,
            cur_f2: 0,
            cur_f2q: 0,
            cur_f3: 0,
            filt_fa: 0,
            filt_fc: 0,
            filt_va: 0,
            filt_f1: 0,
            filt_f2: 0,
            filt_f2q: 0,
            filt_f3: 0,
            phonetick: 0,
            ticks: 0,
            update_counter: 0,
            pitch: 0,
            closure: 0,
            sample_count: 0,
            main_divider: 0,
            cur_closure: true,
            noise: 0,
            cur_noise: false,
            commit_pending: false,
            commit_countdown: 0,
            end_phone_pending: false,
            end_phone_countdown: 0,
            f1_a: [0.0; 4],
            f1_b: [0.0; 4],
            f2v_a: [0.0; 4],
            f2v_b: [0.0; 4],
            f2n_a: [0.0; 2],
            f2n_b: [0.0; 2],
            f3_a: [0.0; 4],
            f3_b: [0.0; 4],
            f4_a: [0.0; 4],
            f4_b: [0.0; 4],
            fx_a: [0.0; 1],
            fx_b: [0.0; 2],
            fn_a: [0.0; 3],
            fn_b: [0.0; 3],
            voice_1: [0.0; 4],
            voice_2: [0.0; 4],
            voice_3: [0.0; 4],
            noise_1: [0.0; 3],
            noise_2: [0.0; 3],
            noise_3: [0.0; 2],
            noise_4: [0.0; 2],
            vn_1: [0.0; 4],
            vn_2: [0.0; 4],
            vn_3: [0.0; 4],
            vn_4: [0.0; 4],
            vn_5: [0.0; 2],
            vn_6: [0.0; 2],
            rom: vec![0u8; 512],
            main_clock_hz,
            sclock,
            cclock: main_clock_hz as f64 / 36.0,
            resampler: AudioResampler::new(sclock as u64, OUTPUT_SAMPLE_RATE),
        }
    }

    /// Retune the master clock frequency.
    ///
    /// On the Gottlieb sound/speech board the SC-01's clock is a VCO driven
    /// by the speech-clock DAC, so the host retunes it at runtime. Phoneme
    /// durations are paced by the rate at which [`tick`](Self::tick) is called
    /// (the host's responsibility), while this updates the derived sample
    /// (`sclock`) and capacitor (`cclock`) clocks, the resampler input rate,
    /// and the switched-capacitor filters so formant pitch tracks the clock.
    pub fn set_clock(&mut self, main_clock_hz: u64) {
        if main_clock_hz == 0 || main_clock_hz == self.main_clock_hz {
            return;
        }
        self.main_clock_hz = main_clock_hz;
        self.sclock = main_clock_hz as f64 / 18.0;
        self.cclock = main_clock_hz as f64 / 36.0;
        self.resampler.set_input_rate(self.sclock as u64);
        // Rebuild every filter so the new cclock is reflected in the
        // switched-capacitor coefficients.
        self.filters_commit(true);
    }

    /// Load the phoneme ROM (512 bytes, 64 entries × 8 bytes, little-endian).
    pub fn load_rom(&mut self, data: &[u8]) {
        let len = data.len().min(512);
        self.rom[..len].copy_from_slice(&data[..len]);
    }

    /// Write a 6-bit phoneme code. Sets A/R low and schedules a commit.
    pub fn write_phoneme(&mut self, data: u8) {
        self.phone = data & 0x3F;
        self.ar_state = false;

        // Schedule commit at 72 main clock ticks (~0.1 ms).
        // Overrides a pending end-of-phone but not an existing commit.
        if !self.commit_pending {
            self.commit_pending = true;
            self.commit_countdown = 72;
            self.end_phone_pending = false;
        }
    }

    /// Set the 2-bit inflection value (modulates pitch).
    pub fn set_inflection(&mut self, data: u8) {
        self.inflection = data & 0x03;
    }

    /// Read the A/R (Articulate/Request) pin.
    ///
    /// `true` = ready for next phoneme, `false` = busy playing.
    pub fn ar_output(&self) -> bool {
        self.ar_state
    }

    /// Take all buffered audio samples (f32, centered around 0).
    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.resampler.drain_audio()
    }

    // -----------------------------------------------------------------------
    // Timing engine
    // -----------------------------------------------------------------------

    /// One step of exponential interpolation (matches MAME `interpolate`).
    ///
    /// `reg = reg - (reg >> 3) + (target << 1)`
    ///
    /// Converges to `target * 16` in approximately 8 steps. The 4-bit ROM
    /// targets are scaled to the 8-bit interpolation register range; the
    /// committed filter values (`filt_*`) are derived by right-shifting
    /// the interpolated result back to 4 or 5 bits.
    fn interpolate(reg: &mut u8, target: u8) {
        *reg = *reg - (*reg >> 3) + (target << 1);
    }

    /// Commit interpolated values to filter parameters.
    ///
    /// Always updates FA, FC, VA (amplitude params). Only updates formant
    /// filter coefficients (F1, F2, F2Q, F3) when they actually change,
    /// unless `force` is true (e.g., on reset).
    fn filters_commit(&mut self, force: bool) {
        self.filt_fa = self.cur_fa >> 4;
        self.filt_fc = self.cur_fc >> 4;
        self.filt_va = self.cur_va >> 4;

        let new_f1 = self.cur_f1 >> 4;
        if force || self.filt_f1 != new_f1 {
            self.filt_f1 = new_f1;
            Self::build_standard_filter(
                &mut self.f1_a,
                &mut self.f1_b,
                self.sclock,
                self.cclock,
                11247.0,
                11797.0,
                949.0,
                52067.0,
                2280.0
                    + Self::bits_to_caps(self.filt_f1 as u32, &[2546.0, 4973.0, 9861.0, 19724.0]),
                166272.0,
            );
        }

        let new_f2 = self.cur_f2 >> 3; // 5 bits — extra precision
        let new_f2q = self.cur_f2q >> 4;
        if force || self.filt_f2 != new_f2 || self.filt_f2q != new_f2q {
            self.filt_f2 = new_f2;
            self.filt_f2q = new_f2q;

            let f2q_caps = 829.0
                + Self::bits_to_caps(self.filt_f2q as u32, &[1390.0, 2965.0, 5875.0, 11297.0]);
            let f2_caps = 2352.0
                + Self::bits_to_caps(
                    self.filt_f2 as u32,
                    &[833.0, 1663.0, 3164.0, 6327.0, 12654.0],
                );

            Self::build_standard_filter(
                &mut self.f2v_a,
                &mut self.f2v_b,
                self.sclock,
                self.cclock,
                24840.0,
                29154.0,
                f2q_caps,
                38180.0,
                f2_caps,
                34270.0,
            );
            Self::build_injection_filter(
                &mut self.f2n_a,
                &mut self.f2n_b,
                self.sclock,
                self.cclock,
                29154.0,
                f2q_caps,
                38180.0,
                f2_caps,
                34270.0,
            );
        }

        let new_f3 = self.cur_f3 >> 4;
        if force || self.filt_f3 != new_f3 {
            self.filt_f3 = new_f3;
            Self::build_standard_filter(
                &mut self.f3_a,
                &mut self.f3_b,
                self.sclock,
                self.cclock,
                0.0,
                17594.0,
                868.0,
                18828.0,
                8480.0
                    + Self::bits_to_caps(self.filt_f3 as u32, &[2226.0, 4485.0, 9056.0, 18111.0]),
                50019.0,
            );
        }

        if force {
            Self::build_standard_filter(
                &mut self.f4_a,
                &mut self.f4_b,
                self.sclock,
                self.cclock,
                0.0,
                28810.0,
                1165.0,
                21457.0,
                8558.0,
                7289.0,
            );
            Self::build_lowpass_filter(
                &mut self.fx_a,
                &mut self.fx_b,
                self.sclock,
                self.cclock,
                1122.0,
                23131.0,
            );
            Self::build_noise_shaper_filter(
                &mut self.fn_a,
                &mut self.fn_b,
                self.sclock,
                self.cclock,
                15500.0,
                14854.0,
                8450.0,
                9523.0,
                14083.0,
            );
        }
    }

    // -----------------------------------------------------------------------
    // Filter coefficient calculation
    // -----------------------------------------------------------------------

    /// Sum capacitor values for bits that are set in `value`.
    ///
    /// Each bit position selects a capacitor from the array. Capacitor
    /// values are in square micrometers (proportional to capacitance in
    /// the switched-capacitor design).
    fn bits_to_caps(value: u32, caps: &[f64]) -> f64 {
        let mut total = 0.0;
        let mut v = value;
        for &cap in caps {
            if v & 1 != 0 {
                total += cap;
            }
            v >>= 1;
        }
        total
    }

    /// Build a standard 2nd-order bandpass IIR filter (4 coefficients each).
    ///
    /// Models the SC-01's switched-capacitor op-amp circuit:
    /// - Input through parallel R1||C1 (c1t unswitched, c1b switched)
    /// - Feedback through parallel R2||C2 (c2t unswitched, c2b switched)
    /// - Inter-stage cap C3 and output feedback cap C4
    ///
    /// Transfer function: H(s) = (1 + k0·s) / (1 + k1·s + k2·s²)
    ///
    /// Uses bilinear z-transform with frequency pre-warping at the
    /// estimated peak frequency.
    #[allow(clippy::too_many_arguments)]
    fn build_standard_filter(
        a: &mut [f64; 4],
        b: &mut [f64; 4],
        sclock: f64,
        cclock: f64,
        c1t: f64,
        c1b: f64,
        c2t: f64,
        c2b: f64,
        c3: f64,
        c4: f64,
    ) {
        let k0 = c1t / (cclock * c1b);
        let k1 = c4 * c2t / (cclock * c1b * c3);
        let k2 = c4 * c2b / (cclock * cclock * c1b * c3);

        let fpeak = (k0 * k1 - k2).abs().sqrt() / (2.0 * std::f64::consts::PI * k2);
        let zc = 2.0 * std::f64::consts::PI * fpeak / (std::f64::consts::PI * fpeak / sclock).tan();

        let m0 = zc * k0;
        let m1 = zc * k1;
        let m2 = zc * zc * k2;

        a[0] = 1.0 + m0;
        a[1] = 3.0 + m0;
        a[2] = 3.0 - m0;
        a[3] = 1.0 - m0;
        b[0] = 1.0 + m1 + m2;
        b[1] = 3.0 + m1 - m2;
        b[2] = 3.0 - m1 - m2;
        b[3] = 1.0 - m1 + m2;
    }

    /// Build a 1st-order lowpass IIR filter (1 a-coeff, 2 b-coeffs).
    ///
    /// Simple RC lowpass: H(s) = 1 / (1 + k·s)
    ///
    /// The raw cap values place the cutoff at ~150 Hz, but recordings
    /// show ~4 kHz is correct — a 150/4000 fudge factor is applied
    /// (matching MAME).
    fn build_lowpass_filter(
        a: &mut [f64; 1],
        b: &mut [f64; 2],
        sclock: f64,
        cclock: f64,
        c1t: f64,
        c1b: f64,
    ) {
        let k = c1b / (cclock * c1t) * (150.0 / 4000.0);
        let fpeak = 1.0 / (2.0 * std::f64::consts::PI * k);
        let zc = 2.0 * std::f64::consts::PI * fpeak / (std::f64::consts::PI * fpeak / sclock).tan();
        let m = zc * k;

        a[0] = 1.0;
        b[0] = 1.0 + m;
        b[1] = 1.0 - m;
    }

    /// Build a 2nd-order bandpass noise shaper filter (3 coefficients each).
    ///
    /// Transfer function: H(s) = k0·s / (1 + k1·s + k2·s²)
    ///
    /// This shapes the white noise into a band-limited signal before
    /// it enters the formant filters.
    #[allow(clippy::too_many_arguments)]
    fn build_noise_shaper_filter(
        a: &mut [f64; 3],
        b: &mut [f64; 3],
        sclock: f64,
        cclock: f64,
        c1: f64,
        c2t: f64,
        c2b: f64,
        c3: f64,
        c4: f64,
    ) {
        let k0 = c2t * c3 * c2b / c4;
        let k1 = c2t * (cclock * c2b);
        let k2 = c1 * c2t * c3 / (cclock * c4);

        let fpeak = (1.0 / k2).sqrt() / (2.0 * std::f64::consts::PI);
        let zc = 2.0 * std::f64::consts::PI * fpeak / (std::f64::consts::PI * fpeak / sclock).tan();

        let m0 = zc * k0;
        let m1 = zc * k1;
        let m2 = zc * zc * k2;

        a[0] = m0;
        a[1] = 0.0;
        a[2] = -m0;
        b[0] = 1.0 + m1 + m2;
        b[1] = 2.0 - 2.0 * m2;
        b[2] = 1.0 - m1 + m2;
    }

    /// Build a 1st-order injection filter for F2 noise path (2 coefficients each).
    ///
    /// Transfer function: H(s) = (k0 + k2·s) / (k1 - k2·s)
    ///
    /// MAME notes this filter is numerically unstable, so it is
    /// neutralized (zeroed out). Retained for documentation and future
    /// investigation.
    #[allow(clippy::too_many_arguments)]
    fn build_injection_filter(
        a: &mut [f64; 2],
        b: &mut [f64; 2],
        _sclock: f64,
        _cclock: f64,
        _c1b: f64,
        _c2t: f64,
        _c2b: f64,
        _c3: f64,
        _c4: f64,
    ) {
        // Neutralized per MAME — numerically unstable filter.
        a[0] = 0.0;
        a[1] = 0.0;
        b[0] = 1.0;
        b[1] = 0.0;
    }

    // -----------------------------------------------------------------------
    // Analog signal path
    // -----------------------------------------------------------------------

    /// Shift a history buffer right by one and insert a new value at the front.
    fn shift_hist(val: f64, hist: &mut [f64]) {
        for i in (1..hist.len()).rev() {
            hist[i] = hist[i - 1];
        }
        hist[0] = val;
    }

    /// Apply an IIR filter. `a` coefficients weight inputs `x`, `b` coefficients
    /// weight previous outputs `y`. Returns `(Σ x[i]*a[i] - Σ y[i-1]*b[i]) / b[0]`.
    fn apply_filter(x: &[f64], y: &[f64], a: &[f64], b: &[f64]) -> f64 {
        let mut total = 0.0;
        for i in 0..a.len() {
            total += x[i] * a[i];
        }
        for i in 1..b.len() {
            total -= y[i - 1] * b[i];
        }
        total / b[0]
    }

    /// Compute one audio sample from the 13-stage analog signal path.
    ///
    /// Called at sclock rate (~40 kHz). The signal flows through:
    /// voice path (glottal → VA → F1 → F2v), noise path (LFSR → FA →
    /// shaper → FC → F2n), then mixed (F3 → noise inject → F4 →
    /// closure → lowpass → output).
    fn analog_calc(&mut self) -> f64 {
        // --- Voice path ---

        // 1. Glottal pulse lookup (9-entry waveform, indexed by pitch >> 3)
        let v = if self.pitch >= 9 << 3 {
            0.0
        } else {
            GLOTTAL_WAVE[(self.pitch >> 3) as usize]
        };

        // 2. Voice amplitude scaling (linear, 4-bit)
        let v = v * self.filt_va as f64 / 15.0;
        Self::shift_hist(v, &mut self.voice_1);

        // 3. F1 bandpass filter
        let v = Self::apply_filter(&self.voice_1, &self.voice_2, &self.f1_a, &self.f1_b);
        Self::shift_hist(v, &mut self.voice_2);

        // 4. F2 voice bandpass filter
        let v = Self::apply_filter(&self.voice_2, &self.voice_3, &self.f2v_a, &self.f2v_b);
        Self::shift_hist(v, &mut self.voice_3);

        // --- Noise path ---

        // 5. Noise gate: LFSR output gated by pitch bit 6, scaled by FA
        let noise_bit = if self.pitch & 0x40 != 0 {
            self.cur_noise
        } else {
            false
        };
        let n = 1e4 * if noise_bit { 1.0 } else { -1.0 };
        let n = n * self.filt_fa as f64 / 15.0;
        Self::shift_hist(n, &mut self.noise_1);

        // 6. Noise shaper bandpass filter
        let n = Self::apply_filter(&self.noise_1, &self.noise_2, &self.fn_a, &self.fn_b);
        Self::shift_hist(n, &mut self.noise_2);

        // 7. Noise cutoff scaling by FC
        let n2 = n * self.filt_fc as f64 / 15.0;
        Self::shift_hist(n2, &mut self.noise_3);

        // 8. F2 noise bandpass filter (injection filter, currently neutralized)
        let n2 = Self::apply_filter(&self.noise_3, &self.noise_4, &self.f2n_a, &self.f2n_b);
        Self::shift_hist(n2, &mut self.noise_4);

        // --- Mixed path ---

        // 9. Voice + noise mix
        let vn = v + n2;
        Self::shift_hist(vn, &mut self.vn_1);

        // 10. F3 bandpass filter
        let vn = Self::apply_filter(&self.vn_1, &self.vn_2, &self.f3_a, &self.f3_b);
        Self::shift_hist(vn, &mut self.vn_2);

        // 11. Secondary noise injection (inverse FC weighting)
        let vn = vn + n * (5.0 + (15 ^ self.filt_fc) as f64) / 20.0;
        Self::shift_hist(vn, &mut self.vn_3);

        // 12. F4 bandpass filter (fixed frequency ~3.4 kHz)
        let vn = Self::apply_filter(&self.vn_3, &self.vn_4, &self.f4_a, &self.f4_b);
        Self::shift_hist(vn, &mut self.vn_4);

        // 13. Glottal closure amplitude envelope (linear, 3-bit)
        let vn = vn * (7 ^ (self.closure >> 2)) as f64 / 7.0;
        Self::shift_hist(vn, &mut self.vn_5);

        // 14. Output lowpass filter (fixed ~3.5 kHz)
        let vn = Self::apply_filter(&self.vn_5, &self.vn_6, &self.fx_a, &self.fx_b);
        Self::shift_hist(vn, &mut self.vn_6);

        vn * 0.35
    }

    // -----------------------------------------------------------------------
    // Timing engine
    // -----------------------------------------------------------------------

    /// Main timing engine, called at cclock rate (~20 kHz).
    ///
    /// Manages phonetick/ticks counters, interpolation at 208/625 Hz,
    /// pitch counter, noise LFSR, and closure state.
    fn chip_update(&mut self) {
        // Phone tick counter. Stopped when ticks reach 16.
        if self.ticks != 0x10 {
            self.phonetick += 1;
            // Comparator with duration << 2, one-tick delay in path
            if self.phonetick == ((self.rom_duration as u16) << 2) | 1 {
                self.phonetick = 0;
                self.ticks += 1;
                if self.ticks == self.rom_cld {
                    self.cur_closure = self.rom_closure;
                }
            }
        }

        // Update timing counters: divide by 16 (625 Hz) and by 48 (208 Hz),
        // phased so the 208 Hz tick falls exactly between two 625 Hz ticks.
        self.update_counter += 1;
        if self.update_counter == 0x30 {
            self.update_counter = 0;
        }

        let tick_625 = (self.update_counter & 0x0F) == 0;
        let tick_208 = self.update_counter == 0x28;

        // Formant update at 208 Hz.
        // Die bug: FC is interpolated here instead of VA.
        // Formants frozen during pause unless both voice and noise volumes are zero.
        if tick_208 && (!self.rom_pause || (self.filt_fa == 0 && self.filt_va == 0)) {
            Self::interpolate(&mut self.cur_fc, self.rom_fc);
            Self::interpolate(&mut self.cur_f1, self.rom_f1);
            Self::interpolate(&mut self.cur_f2, self.rom_f2);
            Self::interpolate(&mut self.cur_f2q, self.rom_f2q);
            Self::interpolate(&mut self.cur_f3, self.rom_f3);
        }

        // Non-formant (amplitude) update at 625 Hz.
        // Die bug: VA is interpolated here instead of FC.
        if tick_625 {
            if self.ticks >= self.rom_vd {
                Self::interpolate(&mut self.cur_fa, self.rom_fa);
            }
            if self.ticks >= self.rom_cld {
                Self::interpolate(&mut self.cur_va, self.rom_va);
            }
        }

        // Closure counter: ramps 0→28 when closure active, reset otherwise.
        if !self.cur_closure && (self.filt_fa != 0 || self.filt_va != 0) {
            self.closure = 0;
        } else if self.closure != 7 << 2 {
            self.closure += 1;
        }

        // Pitch counter: 8-bit, wraps at threshold derived from inflection and F1.
        self.pitch = self.pitch.wrapping_add(1);
        let pitch_limit = (0xE0u8 ^ (self.inflection << 5) ^ (self.filt_f1 << 1)).wrapping_add(2);
        if self.pitch == pitch_limit {
            self.pitch = 0;
        }

        // Filters commit when pitch is in index 1 of pitch wave
        // (matches 4 consecutive values where bits [2:1] vary).
        if (self.pitch & 0xF9) == 0x08 {
            self.filters_commit(false);
        }

        // Noise shift register: 15-bit LFSR with NXOR feedback on bits 13-14.
        // The `1 || filt_fa` in MAME is always true (likely intended as `0 ||`).
        let inp = self.cur_noise && (self.noise != 0x7FFF);
        self.noise = ((self.noise << 1) & 0x7FFE) | u16::from(inp);
        self.cur_noise = ((self.noise >> 14) ^ (self.noise >> 13)) & 1 == 0;
    }

    // -----------------------------------------------------------------------
    // ROM parsing
    // -----------------------------------------------------------------------

    /// Extract bits from a 64-bit value at the given positions (MSB first).
    ///
    /// Mirrors MAME's `bitswap()` template: the first position becomes the
    /// MSB of the result, the last becomes the LSB.
    fn extract_bits(val: u64, positions: &[u8]) -> u8 {
        let n = positions.len();
        let mut result = 0u8;
        for (i, &pos) in positions.iter().enumerate() {
            result |= (((val >> pos) & 1) as u8) << (n - 1 - i);
        }
        result
    }

    /// Commit the current phoneme: search the ROM and extract parameters.
    fn phone_commit(&mut self) {
        self.phonetick = 0;
        self.ticks = 0;

        for i in 0..64 {
            let offset = i * 8;
            if offset + 8 > self.rom.len() {
                break;
            }

            let val = u64::from_le_bytes(self.rom[offset..offset + 8].try_into().unwrap());

            if self.phone != ((val >> 56) & 0x3F) as u8 {
                continue;
            }

            // Interleaved 4-bit parameters (MSB-first bit positions)
            self.rom_f1 = Self::extract_bits(val, &[0, 7, 14, 21]);
            self.rom_va = Self::extract_bits(val, &[1, 8, 15, 22]);
            self.rom_f2 = Self::extract_bits(val, &[2, 9, 16, 23]);
            self.rom_fc = Self::extract_bits(val, &[3, 10, 17, 24]);
            self.rom_f2q = Self::extract_bits(val, &[4, 11, 18, 25]);
            self.rom_f3 = Self::extract_bits(val, &[5, 12, 19, 26]);
            self.rom_fa = Self::extract_bits(val, &[6, 13, 20, 27]);

            // CLD and VD have inverted bit orders (prototype miswiring
            // compensated in ROM)
            self.rom_cld = Self::extract_bits(val, &[34, 32, 30, 28]);
            self.rom_vd = Self::extract_bits(val, &[35, 33, 31, 29]);

            self.rom_closure = ((val >> 36) & 1) != 0;
            self.rom_duration = Self::extract_bits(!val, &[37, 38, 39, 40, 41, 42, 43]);

            // Hard-wired pause detection (not part of ROM data)
            self.rom_pause = self.phone == 0x03 || self.phone == 0x3E;

            if self.rom_cld == 0 {
                self.cur_closure = self.rom_closure;
            }

            // Schedule end-of-phone: A/R returns high after full duration
            let duration_ticks = 16 * (self.rom_duration as u32 * 4 + 1) * 4 * 9 + 2;
            self.end_phone_pending = true;
            self.end_phone_countdown = duration_ticks;

            return;
        }
    }
}

// ---------------------------------------------------------------------------
// Device trait
// ---------------------------------------------------------------------------

impl crate::device::Device for VotraxSc01 {
    fn name(&self) -> &'static str {
        "Votrax SC-01"
    }

    fn reset(&mut self) {
        self.phone = 0x3F;
        self.inflection = 0;
        self.ar_state = true;
        self.sample_count = 0;

        // Commit STOP phoneme to initialize ROM parameters
        self.phone_commit();

        // Clear interpolation state
        self.cur_fa = 0;
        self.cur_fc = 0;
        self.cur_va = 0;
        self.cur_f1 = 0;
        self.cur_f2 = 0;
        self.cur_f2q = 0;
        self.cur_f3 = 0;

        // Clear committed filter values
        self.filt_fa = 0;
        self.filt_fc = 0;
        self.filt_va = 0;
        self.filt_f1 = 0;
        self.filt_f2 = 0;
        self.filt_f2q = 0;
        self.filt_f3 = 0;

        // Clear timing state
        self.pitch = 0;
        self.closure = 0;
        self.update_counter = 0;
        self.cur_closure = true;
        self.noise = 0;
        self.cur_noise = false;
        self.main_divider = 0;
        self.commit_pending = false;
        self.commit_countdown = 0;
        self.end_phone_pending = false;
        self.end_phone_countdown = 0;

        // Clear signal histories
        self.voice_1 = [0.0; 4];
        self.voice_2 = [0.0; 4];
        self.voice_3 = [0.0; 4];
        self.noise_1 = [0.0; 3];
        self.noise_2 = [0.0; 3];
        self.noise_3 = [0.0; 2];
        self.noise_4 = [0.0; 2];
        self.vn_1 = [0.0; 4];
        self.vn_2 = [0.0; 4];
        self.vn_3 = [0.0; 4];
        self.vn_4 = [0.0; 4];
        self.vn_5 = [0.0; 2];
        self.vn_6 = [0.0; 2];

        // Rebuild all filter coefficients from zero state
        self.filters_commit(true);

        self.resampler.reset();
    }

    fn tick(&mut self) {
        // Handle pending phoneme commit (72 main clock ticks after write)
        if self.commit_pending {
            if self.commit_countdown == 0 {
                self.commit_pending = false;
                self.phone_commit();
            } else {
                self.commit_countdown -= 1;
            }
        }

        // Handle end-of-phone (A/R goes high when phoneme duration expires)
        if self.end_phone_pending {
            if self.end_phone_countdown == 0 {
                self.end_phone_pending = false;
                self.ar_state = true;
            } else {
                self.end_phone_countdown -= 1;
            }
        }

        // Divide main clock by 18 → sclock (~40 kHz audio sample rate)
        self.main_divider += 1;
        if self.main_divider >= 18 {
            self.main_divider = 0;

            self.sample_count += 1;

            // Every other sclock tick = cclock (~20 kHz): run timing engine
            if self.sample_count & 1 != 0 {
                self.chip_update();
            }

            // Analog signal path runs at sclock (~40 kHz)
            let sample = self.analog_calc();
            self.resampler.tick(sample as f32);
        }
    }
}

// ---------------------------------------------------------------------------
// Debuggable
// ---------------------------------------------------------------------------

impl Debuggable for VotraxSc01 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "PHONE",
                value: self.phone as u64,
                width: 6,
            },
            DebugRegister {
                name: "INFLECT",
                value: self.inflection as u64,
                width: 2,
            },
            DebugRegister {
                name: "AR",
                value: u64::from(self.ar_state),
                width: 1,
            },
            DebugRegister {
                name: "TICKS",
                value: self.ticks as u64,
                width: 5,
            },
            DebugRegister {
                name: "PITCH",
                value: self.pitch as u64,
                width: 8,
            },
            DebugRegister {
                name: "DURATION",
                value: self.rom_duration as u64,
                width: 7,
            },
        ]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::Device;

    const TEST_CLOCK: u64 = 720_000;

    /// Build one 8-byte ROM entry encoding the given phoneme parameters.
    ///
    /// The entry is stored in the interleaved bit format expected by
    /// `phone_commit()`, matching the SC-01 ROM layout.
    #[allow(clippy::too_many_arguments)]
    fn build_rom_entry(
        phone: u8,
        f1: u8,
        va: u8,
        f2: u8,
        fc: u8,
        f2q: u8,
        f3: u8,
        fa: u8,
        cld: u8,
        vd: u8,
        closure: bool,
        duration: u8,
    ) -> [u8; 8] {
        let mut val: u64 = 0;

        // Phone code in bits 56-61
        val |= (phone as u64 & 0x3F) << 56;

        // Helper to set a single bit
        let set = |v: &mut u64, pos: u8, bit: u8| {
            if bit & 1 != 0 {
                *v |= 1u64 << pos;
            }
        };

        // Interleaved parameters: position arrays match extract_bits calls.
        // Each parameter is 4 bits (MSB to LSB stored at the given positions).
        let params: [(u8, &[u8]); 7] = [
            (f1, &[0, 7, 14, 21]),
            (va, &[1, 8, 15, 22]),
            (f2, &[2, 9, 16, 23]),
            (fc, &[3, 10, 17, 24]),
            (f2q, &[4, 11, 18, 25]),
            (f3, &[5, 12, 19, 26]),
            (fa, &[6, 13, 20, 27]),
        ];
        for (param, positions) in &params {
            let n = positions.len();
            for (i, &pos) in positions.iter().enumerate() {
                set(&mut val, pos, param >> (n - 1 - i));
            }
        }

        // CLD: bits 34, 32, 30, 28
        for (i, &pos) in [34u8, 32, 30, 28].iter().enumerate() {
            set(&mut val, pos, cld >> (3 - i));
        }
        // VD: bits 35, 33, 31, 29
        for (i, &pos) in [35u8, 33, 31, 29].iter().enumerate() {
            set(&mut val, pos, vd >> (3 - i));
        }

        // Closure: bit 36
        if closure {
            val |= 1u64 << 36;
        }

        // Duration: extracted via extract_bits(!val, [37..43]), so store
        // inverted bits in val.
        for (i, &pos) in [37u8, 38, 39, 40, 41, 42, 43].iter().enumerate() {
            let bit = (duration >> (6 - i)) & 1;
            // Invert: extract_bits uses !val
            if bit == 0 {
                val |= 1u64 << pos;
            }
        }

        val.to_le_bytes()
    }

    /// Build a 512-byte ROM containing one phoneme entry at index 0
    /// (remaining entries zeroed).
    fn build_test_rom(entry: &[u8; 8]) -> Vec<u8> {
        let mut rom = vec![0u8; 512];
        rom[..8].copy_from_slice(entry);
        rom
    }

    #[test]
    fn initial_state() {
        let v = VotraxSc01::new(TEST_CLOCK);
        assert_eq!(v.phone, 0x3F);
        assert_eq!(v.inflection, 0);
        assert!(v.ar_output());
        assert_eq!(v.sclock, TEST_CLOCK as f64 / 18.0);
        assert_eq!(v.cclock, TEST_CLOCK as f64 / 36.0);
    }

    #[test]
    fn set_clock_recomputes_derived_clocks() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        let new_clock = TEST_CLOCK + 200_000;
        v.set_clock(new_clock);
        assert_eq!(v.main_clock_hz, new_clock);
        assert_eq!(v.sclock, new_clock as f64 / 18.0);
        assert_eq!(v.cclock, new_clock as f64 / 36.0);

        // Idempotent: re-applying the same clock is a no-op.
        v.set_clock(new_clock);
        assert_eq!(v.sclock, new_clock as f64 / 18.0);

        // A zero clock is ignored (guards against divide-by-zero downstream).
        v.set_clock(0);
        assert_eq!(v.main_clock_hz, new_clock);
    }

    #[test]
    fn write_phoneme_sets_state() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.write_phoneme(0x05); // A1
        assert_eq!(v.phone, 0x05);
        assert!(!v.ar_output()); // busy
        assert!(v.commit_pending);
        assert_eq!(v.commit_countdown, 72);
    }

    #[test]
    fn write_phoneme_masks_to_6_bits() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.write_phoneme(0xFF);
        assert_eq!(v.phone, 0x3F);
    }

    #[test]
    fn set_inflection_masks_to_2_bits() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.set_inflection(0xFF);
        assert_eq!(v.inflection, 0x03);
    }

    #[test]
    fn extract_bits_basic() {
        // bit0=1, bit7=0, bit14=1, bit21=0
        let val: u64 = (1 << 0) | (1 << 14);
        // Positions [0, 7, 14, 21] → MSB-first → result = 1010 = 0xA
        assert_eq!(VotraxSc01::extract_bits(val, &[0, 7, 14, 21]), 0b1010);

        // All bits set
        let val2: u64 = (1 << 0) | (1 << 7) | (1 << 14) | (1 << 21);
        assert_eq!(VotraxSc01::extract_bits(val2, &[0, 7, 14, 21]), 0b1111);
    }

    #[test]
    fn extract_bits_single() {
        let val: u64 = 1 << 36;
        assert_eq!(VotraxSc01::extract_bits(val, &[36]), 1);
        assert_eq!(VotraxSc01::extract_bits(val, &[35]), 0);
    }

    #[test]
    fn rom_round_trip() {
        // Build a ROM entry with known parameters and verify extraction
        let entry = build_rom_entry(
            0x05, // phone = A1
            0x0A, // f1
            0x0C, // va
            0x07, // f2
            0x03, // fc
            0x0F, // f2q
            0x06, // f3
            0x09, // fa
            0x02, // cld
            0x05, // vd
            true, // closure
            0x1A, // duration
        );
        let rom = build_test_rom(&entry);

        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.load_rom(&rom);
        v.phone = 0x05;
        v.phone_commit();

        assert_eq!(v.rom_f1, 0x0A);
        assert_eq!(v.rom_va, 0x0C);
        assert_eq!(v.rom_f2, 0x07);
        assert_eq!(v.rom_fc, 0x03);
        assert_eq!(v.rom_f2q, 0x0F);
        assert_eq!(v.rom_f3, 0x06);
        assert_eq!(v.rom_fa, 0x09);
        assert_eq!(v.rom_cld, 0x02);
        assert_eq!(v.rom_vd, 0x05);
        assert!(v.rom_closure);
        assert_eq!(v.rom_duration, 0x1A);
        assert!(!v.rom_pause); // A1 is not a pause phone
    }

    #[test]
    fn rom_pause_detection() {
        // Phone 0x03 = PA0 (pause)
        let entry = build_rom_entry(0x03, 0, 0, 0, 0, 0, 0, 0, 0, 0, false, 0);
        let rom = build_test_rom(&entry);
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.load_rom(&rom);
        v.phone = 0x03;
        v.phone_commit();
        assert!(v.rom_pause);

        // Phone 0x3E = PA1 (pause)
        let entry = build_rom_entry(0x3E, 0, 0, 0, 0, 0, 0, 0, 0, 0, false, 0);
        let rom = build_test_rom(&entry);
        v.load_rom(&rom);
        v.phone = 0x3E;
        v.phone_commit();
        assert!(v.rom_pause);
    }

    #[test]
    fn rom_cld_zero_sets_closure() {
        // When rom_cld == 0, cur_closure is set immediately from rom_closure
        let entry = build_rom_entry(0x10, 0, 0, 0, 0, 0, 0, 0, 0, 0, true, 0x20);
        let rom = build_test_rom(&entry);
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.load_rom(&rom);
        v.cur_closure = false;
        v.phone = 0x10;
        v.phone_commit();
        assert!(v.cur_closure); // Set immediately because cld == 0
    }

    #[test]
    fn commit_fires_after_72_ticks() {
        let entry = build_rom_entry(0x05, 0x0A, 0, 0, 0, 0, 0, 0, 0, 0, false, 0x10);
        let rom = build_test_rom(&entry);
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.load_rom(&rom);
        v.write_phoneme(0x05);

        // Tick 72 times — commit should fire on the 73rd tick
        for _ in 0..72 {
            assert!(v.commit_pending);
            v.tick();
        }

        // After 72 ticks the countdown reaches 0, commit fires on next tick
        v.tick();
        assert!(!v.commit_pending);
        assert_eq!(v.rom_f1, 0x0A); // Parameters were extracted
    }

    #[test]
    fn end_of_phone_restores_ar() {
        let duration = 0x01u8;
        let entry = build_rom_entry(0x05, 0, 0, 0, 0, 0, 0, 0, 0, 0, false, duration);
        let rom = build_test_rom(&entry);
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.load_rom(&rom);
        v.write_phoneme(0x05);

        // Tick past commit (72 + 1 ticks)
        for _ in 0..73 {
            v.tick();
        }
        assert!(!v.ar_output()); // Still busy

        // End-of-phone countdown: 16 * (duration * 4 + 1) * 4 * 9 + 2
        let end_ticks = 16 * (duration as u32 * 4 + 1) * 4 * 9 + 2;
        for _ in 0..end_ticks {
            v.tick();
        }
        // After the countdown the next tick fires end-of-phone
        v.tick();
        assert!(v.ar_output()); // Ready again
    }

    #[test]
    fn write_during_end_of_phone_cancels_it() {
        let entry = build_rom_entry(0x05, 0, 0, 0, 0, 0, 0, 0, 0, 0, false, 0x7F);
        let rom = build_test_rom(&entry);
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.load_rom(&rom);

        // Start first phoneme and tick past commit
        v.write_phoneme(0x05);
        for _ in 0..73 {
            v.tick();
        }
        assert!(v.end_phone_pending);

        // Write a new phoneme — cancels end-of-phone
        v.write_phoneme(0x05);
        assert!(!v.end_phone_pending);
        assert!(v.commit_pending);
    }

    #[test]
    fn reset_clears_state() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.phone = 0x10;
        v.inflection = 3;
        v.ar_state = false;
        v.pitch = 0x42;
        v.noise = 0x1234;

        v.reset();

        assert_eq!(v.phone, 0x3F);
        assert_eq!(v.inflection, 0);
        assert!(v.ar_state);
        assert_eq!(v.pitch, 0);
        assert_eq!(v.noise, 0);
        assert!(v.cur_closure);
    }

    #[test]
    fn debug_registers_populated() {
        let v = VotraxSc01::new(TEST_CLOCK);
        let regs = v.debug_registers();
        assert!(!regs.is_empty());
        assert_eq!(regs[0].name, "PHONE");
        assert_eq!(regs[0].value, 0x3F);
    }

    #[test]
    fn save_load_round_trip() {
        use crate::core::save_state::{StateReader, StateWriter};
        use crate::prelude::Saveable;

        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.phone = 0x15;
        v.inflection = 2;
        v.ar_state = false;
        v.pitch = 0x42;
        v.noise = 0x5678;
        v.f1_a[0] = 1.234;

        let mut w = StateWriter::new();
        v.save_state(&mut w);
        let data = w.into_vec();

        let mut v2 = VotraxSc01::new(TEST_CLOCK);
        let mut r = StateReader::new(&data);
        v2.load_state(&mut r).unwrap();

        assert_eq!(v2.phone, 0x15);
        assert_eq!(v2.inflection, 2);
        assert!(!v2.ar_state);
        assert_eq!(v2.pitch, 0x42);
        assert_eq!(v2.noise, 0x5678);
        assert!((v2.f1_a[0] - 1.234).abs() < f64::EPSILON);
    }

    // -- Phase 2: Timing engine tests --

    #[test]
    fn interpolate_converges_to_target() {
        // Target 15 → steady state = 15 * 16 = 240 = 0xF0
        let mut reg = 0u8;
        for _ in 0..40 {
            VotraxSc01::interpolate(&mut reg, 15);
        }
        // Should converge to 240 (within 1 due to truncation)
        assert!((239..=241).contains(&reg), "expected ~240, got {reg}");
        // After >> 4, should equal the 4-bit target
        assert_eq!(reg >> 4, 15);
    }

    #[test]
    fn interpolate_converges_downward() {
        // Start high, target 0 → should decay toward 0
        let mut reg = 240u8;
        for _ in 0..80 {
            VotraxSc01::interpolate(&mut reg, 0);
        }
        // Should reach a small value (truncation prevents exact 0)
        assert!(reg < 8, "expected near 0, got {reg}");
    }

    #[test]
    fn interpolate_midrange() {
        // Target 8 → steady state ~128
        let mut reg = 0u8;
        for _ in 0..40 {
            VotraxSc01::interpolate(&mut reg, 8);
        }
        assert!((127..=129).contains(&reg), "expected ~128, got {reg}");
        assert_eq!(reg >> 4, 8);
    }

    #[test]
    fn chip_update_phonetick_wraps_at_duration() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.rom_duration = 3; // wrap at (3 << 2) | 1 = 13
        v.ticks = 0;

        // Run 13 chip_update calls — phonetick should wrap once
        for _ in 0..13 {
            v.chip_update();
        }
        assert_eq!(v.phonetick, 0);
        assert_eq!(v.ticks, 1);
    }

    #[test]
    fn chip_update_ticks_stop_at_16() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.rom_duration = 0; // wrap at (0 << 2) | 1 = 1 (every other tick)
        v.ticks = 15;

        // One more wrap should bring ticks to 16
        v.chip_update(); // phonetick = 1 == 1, wraps, ticks = 16
        assert_eq!(v.ticks, 16);

        // Now ticks should stay at 16 — phonetick counter is frozen
        let prev_phonetick = v.phonetick;
        v.chip_update();
        assert_eq!(v.ticks, 16);
        // phonetick doesn't advance when ticks == 0x10
        assert_eq!(v.phonetick, prev_phonetick);
    }

    #[test]
    fn chip_update_closure_at_cld() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.rom_duration = 0; // wrap every other tick
        v.rom_cld = 3;
        v.rom_closure = true;
        v.cur_closure = false;

        // Run until ticks reaches 3 (= rom_cld)
        // Each wrap takes 1 chip_update (phonetick goes 0→1, wraps)
        for _ in 0..3 {
            v.chip_update(); // ticks: 0→1→2→3
        }
        assert_eq!(v.ticks, 3);
        assert!(v.cur_closure);
    }

    #[test]
    fn update_counter_mod_48() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.update_counter = 0x2F; // 47

        v.chip_update();
        assert_eq!(v.update_counter, 0); // wraps at 0x30 (48)
    }

    #[test]
    fn tick_625_fires_every_16_updates() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.rom_vd = 0; // always interpolate FA
        v.rom_cld = 0; // always interpolate VA
        v.rom_fa = 15;
        v.rom_va = 15;
        v.ticks = 1; // past both delays

        // Run 48 chip_updates (one full update_counter cycle)
        let mut tick_625_count = 0;
        for i in 0..48 {
            let before_fa = v.cur_fa;
            v.chip_update();
            if v.cur_fa != before_fa {
                tick_625_count += 1;
            }
            // 625 Hz fires when update_counter (after increment) & 0x0F == 0
            // That's at counter values: 0x10, 0x20, 0x30→0 = 0, so ticks at 16, 32, 0
            let _ = i;
        }
        // Should fire 3 times per 48-cycle period (at 0, 16, 32)
        assert_eq!(tick_625_count, 3, "625 Hz should fire 3× per 48 cycles");
    }

    #[test]
    fn tick_208_fires_at_counter_40() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.rom_f1 = 10;
        v.update_counter = 0x27; // next increment → 0x28 → 208 Hz fires

        let before = v.cur_f1;
        v.chip_update();
        assert_ne!(v.cur_f1, before, "208 Hz should have interpolated F1");
    }

    #[test]
    fn pitch_wraps_at_threshold() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.inflection = 0;
        v.filt_f1 = 0;
        // Threshold = (0xE0 ^ 0 ^ 0) + 2 = 0xE2 = 226
        v.pitch = 225; // next increment = 226

        v.chip_update();
        assert_eq!(v.pitch, 0, "pitch should wrap at 226");
    }

    #[test]
    fn pitch_wrap_with_inflection() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.inflection = 2; // inflection << 5 = 0x40
        v.filt_f1 = 5; // filt_f1 << 1 = 0x0A
        // Threshold = (0xE0 ^ 0x40 ^ 0x0A) + 2 = 0xAA + 2 = 0xAC = 172
        v.pitch = 171;

        v.chip_update();
        assert_eq!(v.pitch, 0, "pitch should wrap at 172");
    }

    #[test]
    fn noise_lfsr_advances() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.noise = 0x0001;
        v.cur_noise = true;

        // After one update, noise should shift
        v.chip_update();
        // inp = true && (0x0001 != 0x7FFF) = true
        // noise = ((0x0001 << 1) & 0x7FFE) | 1 = 0x0002 | 1 = 0x0003
        assert_eq!(v.noise, 0x0003);
    }

    #[test]
    fn noise_lfsr_lockup_prevention() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.noise = 0x7FFF; // all 1s
        v.cur_noise = true;

        v.chip_update();
        // inp = true && (0x7FFF != 0x7FFF) = false (lockup prevention)
        // noise = ((0x7FFF << 1) & 0x7FFE) | 0 = 0x7FFE
        assert_eq!(v.noise, 0x7FFE);
    }

    #[test]
    fn noise_doesnt_lock_up_over_long_run() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.noise = 1;
        v.cur_noise = true;

        // Run many updates and verify noise keeps changing
        let mut seen_values = std::collections::HashSet::new();
        for _ in 0..1000 {
            v.chip_update();
            seen_values.insert(v.noise);
        }
        // Should visit many different states
        assert!(
            seen_values.len() > 100,
            "LFSR should visit many states, got {}",
            seen_values.len()
        );
    }

    #[test]
    fn closure_counter_ramps_to_28() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.cur_closure = true;
        v.filt_fa = 1; // nonzero so closure doesn't reset
        v.filt_va = 0;
        v.closure = 0;

        for _ in 0..30 {
            v.chip_update();
        }
        // Closure counter maxes at 7 << 2 = 28
        assert_eq!(v.closure, 28);
    }

    #[test]
    fn closure_counter_resets_when_not_active() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.cur_closure = false; // not closed
        v.filt_fa = 1; // nonzero
        v.closure = 20;

        v.chip_update();
        assert_eq!(
            v.closure, 0,
            "closure resets when cur_closure=false and volume nonzero"
        );
    }

    #[test]
    fn filters_commit_derives_filt_from_cur() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.cur_fa = 0xF0;
        v.cur_fc = 0x80;
        v.cur_va = 0x50;
        v.cur_f1 = 0xA0;
        v.cur_f2 = 0xC8; // >> 3 = 25
        v.cur_f2q = 0x70;
        v.cur_f3 = 0x30;

        v.filters_commit(true);

        assert_eq!(v.filt_fa, 0x0F); // 0xF0 >> 4
        assert_eq!(v.filt_fc, 0x08); // 0x80 >> 4
        assert_eq!(v.filt_va, 0x05); // 0x50 >> 4
        assert_eq!(v.filt_f1, 0x0A); // 0xA0 >> 4
        assert_eq!(v.filt_f2, 0x19); // 0xC8 >> 3 = 25
        assert_eq!(v.filt_f2q, 0x07); // 0x70 >> 4
        assert_eq!(v.filt_f3, 0x03); // 0x30 >> 4
    }

    #[test]
    fn clock_division_fires_chip_update() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.rom_f1 = 10; // nonzero target so interpolation has effect
        v.update_counter = 0x27; // next chip_update → 0x28 → 208 Hz fires

        // Need 18 main ticks for one sclock, and chip_update fires on odd sclock
        // First sclock tick: sample_count goes from 0 to 1 (odd) → chip_update fires
        for _ in 0..18 {
            v.tick();
        }
        assert_eq!(v.sample_count, 1);
        // 208 Hz should have fired and interpolated F1
        assert_ne!(v.cur_f1, 0);
    }

    #[test]
    fn formants_frozen_during_pause() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.rom_pause = true;
        v.rom_f1 = 10;
        v.filt_fa = 1; // nonzero volume → formants stay frozen
        v.filt_va = 0;
        v.update_counter = 0x27; // next → 208 Hz tick

        v.chip_update();
        assert_eq!(
            v.cur_f1, 0,
            "formants should be frozen during pause with nonzero volume"
        );
    }

    #[test]
    fn formants_unfreeze_during_pause_at_zero_volume() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.rom_pause = true;
        v.rom_f1 = 10;
        v.filt_fa = 0; // zero volume → formants can update
        v.filt_va = 0;
        v.update_counter = 0x27; // next → 208 Hz tick

        v.chip_update();
        assert_ne!(
            v.cur_f1, 0,
            "formants should update during pause when volume is zero"
        );
    }

    // -- Phase 3: Filter coefficient tests --

    #[test]
    fn bits_to_caps_sums_selected() {
        // Value 0b1010 → selects indices 1 and 3
        let caps = [100.0, 200.0, 400.0, 800.0];
        assert_eq!(VotraxSc01::bits_to_caps(0b1010, &caps), 200.0 + 800.0);
    }

    #[test]
    fn bits_to_caps_all_zero() {
        assert_eq!(
            VotraxSc01::bits_to_caps(0, &[100.0, 200.0, 400.0, 800.0]),
            0.0
        );
    }

    #[test]
    fn bits_to_caps_all_set() {
        let caps = [100.0, 200.0, 400.0, 800.0];
        assert_eq!(VotraxSc01::bits_to_caps(0b1111, &caps), 1500.0);
    }

    #[test]
    fn build_standard_filter_produces_valid_coefficients() {
        let sclock = TEST_CLOCK as f64 / 18.0;
        let cclock = TEST_CLOCK as f64 / 36.0;
        let mut a = [0.0f64; 4];
        let mut b = [0.0f64; 4];

        // F1 filter with filt_f1 = 0 (minimum capacitance)
        VotraxSc01::build_standard_filter(
            &mut a, &mut b, sclock, cclock, 11247.0, 11797.0, 949.0, 52067.0, 2280.0, 166272.0,
        );

        // b[0] should be nonzero (used as divisor in apply_filter)
        assert!(b[0].abs() > 0.0, "b[0] must be nonzero for filter division");
        // All coefficients should be finite
        for i in 0..4 {
            assert!(a[i].is_finite(), "a[{i}] is not finite: {}", a[i]);
            assert!(b[i].is_finite(), "b[{i}] is not finite: {}", b[i]);
        }
    }

    #[test]
    fn build_standard_filter_varies_with_capacitance() {
        let sclock = TEST_CLOCK as f64 / 18.0;
        let cclock = TEST_CLOCK as f64 / 36.0;
        let mut a_lo = [0.0f64; 4];
        let mut b_lo = [0.0f64; 4];
        let mut a_hi = [0.0f64; 4];
        let mut b_hi = [0.0f64; 4];

        // F1 with min vs max capacitance
        let c3_lo = 2280.0;
        let c3_hi = 2280.0 + 2546.0 + 4973.0 + 9861.0 + 19724.0;

        VotraxSc01::build_standard_filter(
            &mut a_lo, &mut b_lo, sclock, cclock, 11247.0, 11797.0, 949.0, 52067.0, c3_lo, 166272.0,
        );
        VotraxSc01::build_standard_filter(
            &mut a_hi, &mut b_hi, sclock, cclock, 11247.0, 11797.0, 949.0, 52067.0, c3_hi, 166272.0,
        );

        // Different capacitance must produce different coefficients
        assert_ne!(a_lo, a_hi, "F1 coefficients should differ with capacitance");
        assert_ne!(b_lo, b_hi, "F1 coefficients should differ with capacitance");
    }

    #[test]
    fn build_lowpass_filter_valid() {
        let sclock = TEST_CLOCK as f64 / 18.0;
        let cclock = TEST_CLOCK as f64 / 36.0;
        let mut a = [0.0f64; 1];
        let mut b = [0.0f64; 2];

        VotraxSc01::build_lowpass_filter(&mut a, &mut b, sclock, cclock, 1122.0, 23131.0);

        assert!(b[0].abs() > 0.0, "b[0] must be nonzero");
        assert!(a[0].is_finite());
        assert!(b[0].is_finite());
        assert!(b[1].is_finite());
    }

    #[test]
    fn build_noise_shaper_filter_valid() {
        let sclock = TEST_CLOCK as f64 / 18.0;
        let cclock = TEST_CLOCK as f64 / 36.0;
        let mut a = [0.0f64; 3];
        let mut b = [0.0f64; 3];

        VotraxSc01::build_noise_shaper_filter(
            &mut a, &mut b, sclock, cclock, 15500.0, 14854.0, 8450.0, 9523.0, 14083.0,
        );

        assert!(b[0].abs() > 0.0, "b[0] must be nonzero");
        // Noise shaper is bandpass: a[1] should be 0
        assert_eq!(a[1], 0.0, "noise shaper a[1] should be 0 (bandpass)");
        // a[0] and a[2] should be equal magnitude, opposite sign
        assert!(
            (a[0] + a[2]).abs() < 1e-10,
            "a[0] and a[2] should be opposite: {} vs {}",
            a[0],
            a[2]
        );
        for i in 0..3 {
            assert!(a[i].is_finite(), "a[{i}] is not finite");
            assert!(b[i].is_finite(), "b[{i}] is not finite");
        }
    }

    #[test]
    fn build_injection_filter_neutralized() {
        let sclock = TEST_CLOCK as f64 / 18.0;
        let cclock = TEST_CLOCK as f64 / 36.0;
        let mut a = [0.0f64; 2];
        let mut b = [0.0f64; 2];

        VotraxSc01::build_injection_filter(
            &mut a, &mut b, sclock, cclock, 29154.0, 829.0, 38180.0, 2352.0, 34270.0,
        );

        // Neutralized: output is always zero
        assert_eq!(a[0], 0.0);
        assert_eq!(a[1], 0.0);
        assert_eq!(b[0], 1.0);
        assert_eq!(b[1], 0.0);
    }

    #[test]
    fn filters_commit_builds_f1_coefficients() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.cur_f1 = 0xA0; // filt_f1 = 0x0A

        v.filters_commit(true);

        // F1 filter coefficients should be nonzero after commit
        assert!(
            v.f1_a[0] != 0.0 || v.f1_a[1] != 0.0,
            "F1 a-coefficients should be populated"
        );
        assert!(
            v.f1_b[0] != 0.0,
            "F1 b[0] should be nonzero (filter divisor)"
        );
    }

    #[test]
    fn filters_commit_skips_unchanged_formants() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.cur_f1 = 0xA0;

        // First commit builds the filter
        v.filters_commit(true);
        let saved_a = v.f1_a;

        // Manually corrupt the coefficients
        v.f1_a = [999.0; 4];

        // Commit again with same filt_f1 — should NOT rebuild
        v.filters_commit(false);
        assert_eq!(v.f1_a, [999.0; 4], "should not rebuild unchanged filter");

        // But changing the interpolation register should rebuild
        v.cur_f1 = 0xB0; // filt_f1 changes from 0x0A to 0x0B
        v.filters_commit(false);
        assert_ne!(v.f1_a, [999.0; 4], "should rebuild when filt_f1 changes");
        // Should also differ from original (different filt_f1 value)
        assert_ne!(v.f1_a, saved_a);
    }

    #[test]
    fn filters_commit_force_builds_fixed_filters() {
        let mut v = VotraxSc01::new(TEST_CLOCK);

        // Without force, fixed filters stay at zero
        v.filters_commit(false);
        assert_eq!(v.f4_a, [0.0; 4], "fixed F4 not built without force");

        // With force, fixed filters are built
        v.filters_commit(true);
        assert!(v.f4_b[0] != 0.0, "F4 should be built on force commit");
        assert!(v.fx_a[0] != 0.0, "FX should be built on force commit");
        assert!(
            v.fn_b[0] != 0.0,
            "noise shaper should be built on force commit"
        );
    }

    #[test]
    fn filters_commit_f2_rebuilds_on_q_change() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.cur_f2 = 0x80;
        v.cur_f2q = 0x30;
        v.filters_commit(true);
        let saved = v.f2v_a;

        // Change Q only (not F2)
        v.cur_f2q = 0x70;
        v.filters_commit(false);
        assert_ne!(v.f2v_a, saved, "F2 should rebuild when Q changes");
    }

    #[test]
    fn reset_builds_all_filters() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.reset();

        // All fixed filters should be built after reset
        assert!(v.f4_b[0] != 0.0, "F4 should be built after reset");
        assert!(v.fx_a[0] != 0.0, "FX should be built after reset");
        assert!(v.fn_b[0] != 0.0, "noise shaper should be built after reset");
    }

    // -- Phase 4: Analog signal path tests --

    #[test]
    fn shift_hist_inserts_at_front() {
        let mut hist = [0.0, 0.0, 0.0, 0.0];
        VotraxSc01::shift_hist(1.0, &mut hist);
        assert_eq!(hist, [1.0, 0.0, 0.0, 0.0]);

        VotraxSc01::shift_hist(2.0, &mut hist);
        assert_eq!(hist, [2.0, 1.0, 0.0, 0.0]);

        VotraxSc01::shift_hist(3.0, &mut hist);
        assert_eq!(hist, [3.0, 2.0, 1.0, 0.0]);

        VotraxSc01::shift_hist(4.0, &mut hist);
        assert_eq!(hist, [4.0, 3.0, 2.0, 1.0]);

        // Fifth push evicts oldest
        VotraxSc01::shift_hist(5.0, &mut hist);
        assert_eq!(hist, [5.0, 4.0, 3.0, 2.0]);
    }

    #[test]
    fn shift_hist_size_two() {
        let mut hist = [0.0, 0.0];
        VotraxSc01::shift_hist(10.0, &mut hist);
        assert_eq!(hist, [10.0, 0.0]);
        VotraxSc01::shift_hist(20.0, &mut hist);
        assert_eq!(hist, [20.0, 10.0]);
    }

    #[test]
    fn apply_filter_passthrough() {
        // Identity-like filter: a=[1], b=[1] → output = input
        let x = [42.0, 0.0];
        let y = [0.0, 0.0];
        let a = [1.0];
        let b = [1.0];
        let result = VotraxSc01::apply_filter(&x, &y, &a, &b);
        assert!((result - 42.0).abs() < 1e-10);
    }

    #[test]
    fn apply_filter_feedback() {
        // a=[1,0], b=[1, -0.5] → y[n] = x[n] + 0.5*y[n-1]
        let x = [1.0, 0.0];
        let y = [2.0, 0.0]; // previous output was 2.0
        let a = [1.0, 0.0];
        let b = [1.0, -0.5];
        // total = x[0]*1 + x[1]*0 = 1
        // total -= y[0]*(-0.5) = 1 + 1 = 2
        // result = 2 / 1 = 2
        let result = VotraxSc01::apply_filter(&x, &y, &a, &b);
        assert!((result - 2.0).abs() < 1e-10);
    }

    #[test]
    fn analog_calc_silent_at_zero_amplitude() {
        // With filters built but zero amplitude, output should be silent
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.reset(); // builds filters
        v.filt_va = 0;
        v.filt_fa = 0;

        // Run several samples to let filter histories stabilize
        for _ in 0..100 {
            v.analog_calc();
        }
        let sample = v.analog_calc();
        assert!(
            sample.abs() < 1e-10,
            "should be silent with zero amplitude, got {sample}"
        );
    }

    #[test]
    fn analog_calc_produces_output_after_reset() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.reset(); // builds all filters

        // Set up voice amplitude and a valid pitch for glottal lookup
        v.filt_va = 15; // max voice amplitude
        v.pitch = 8; // index 1 of glottal wave (nonzero value)

        let sample = v.analog_calc();
        // With voice amplitude and glottal wave active, some output expected
        // (may be zero on first call due to filter warm-up, but history will be populated)
        assert!(
            v.voice_1[0] != 0.0,
            "voice_1 history should be populated after analog_calc"
        );
        let _ = sample;
    }

    #[test]
    fn analog_calc_glottal_zero_above_index_8() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.reset();
        v.filt_va = 15;
        v.pitch = 9 << 3; // index 9 = out of range → glottal = 0

        v.analog_calc();
        assert_eq!(
            v.voice_1[0], 0.0,
            "glottal wave should be 0 for pitch >= 72"
        );
    }

    #[test]
    fn analog_calc_noise_gated_by_pitch_bit6() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.reset();
        v.filt_fa = 15; // max noise amplitude
        v.cur_noise = true;

        // Pitch bit 6 clear → noise gate outputs false → always -1
        v.pitch = 0x00;
        v.analog_calc();
        let noise_off = v.noise_1[0];

        // Pitch bit 6 set → noise gate passes cur_noise (true) → +1
        v.pitch = 0x40;
        v.analog_calc();
        let noise_on = v.noise_1[0];

        // The signs should differ
        assert!(
            noise_off < 0.0,
            "noise should be negative when gate is closed"
        );
        assert!(
            noise_on > 0.0,
            "noise should be positive when gate is open and cur_noise=true"
        );
    }

    #[test]
    fn analog_calc_closure_attenuates() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.reset();
        v.filt_va = 15;
        v.pitch = 8; // valid glottal index

        // Open glottis (closure = 0 → attenuation factor = 7/7 = 1.0)
        v.closure = 0;
        // Run several samples to let filter histories build up
        for _ in 0..100 {
            v.analog_calc();
        }
        let open_sample = v.analog_calc();

        // Closed glottis (closure = 28 → factor = (7^7)/7 = 0/7 = 0)
        v.closure = 28; // 7 << 2
        let closed_sample = v.analog_calc();

        assert!(
            closed_sample.abs() < open_sample.abs() || open_sample.abs() < 1e-15,
            "closed glottis should attenuate: open={open_sample}, closed={closed_sample}"
        );
    }

    #[test]
    fn tick_produces_audio_samples() {
        let entry = build_rom_entry(0x05, 10, 12, 7, 3, 8, 6, 9, 0, 0, false, 0x10);
        let rom = build_test_rom(&entry);
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.load_rom(&rom);
        v.reset();
        v.write_phoneme(0x05);

        // Tick enough to commit the phoneme and generate audio
        // 73 ticks for commit + many more for audio generation
        for _ in 0..5000 {
            v.tick();
        }

        let samples = v.drain_audio();
        assert!(
            !samples.is_empty(),
            "should produce audio samples after ticking"
        );
    }

    #[test]
    fn drain_audio_clears_buffer() {
        let mut v = VotraxSc01::new(TEST_CLOCK);
        v.reset();

        // Tick enough for at least one output sample
        // sclock = 40kHz, output = 44.1kHz, so ~1 output per input
        for _ in 0..18 {
            v.tick();
        }

        let first = v.drain_audio();
        let second = v.drain_audio();
        assert!(!first.is_empty(), "first drain should have samples");
        assert!(second.is_empty(), "second drain should be empty");
    }
}
