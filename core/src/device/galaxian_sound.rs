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
//! 1-pole RC filters). Their levels, decays, and filter cutoffs were tuned with
//! the `tools/sound-reference` rig: the tune/wolf-whistle against a MAME
//! `galaxian` capture, and the fire and hit (shoot / explosion) against the
//! original recorded MAME samples `shot.wav` / `death.wav` — a bright ~0.6 s
//! noise burst and a dark (~630 Hz), ~2.5 s noise rumble. The register
//! *interface* and voice *structure* are faithful.

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
        let noise = b.lfsr_noise(
            "noise",
            NOISE_RATE,
            LfsrSpec {
                width: 17,
                taps: (11, 0),
                seed: 0x1_FFFF,
            },
        );

        // --- Background tone: pitched 74393 tap chord ------------------------
        let tune = b.custom(
            "tune",
            vec![pitch_in.into()],
            Box::new(TuneVoice::default()),
        );
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

        // --- HIT / explosion: a dark broadband noise burst ------------------
        // MAME's hit is a loud, dark (~1 kHz centroid) but *noisy* rumble. A
        // resonant band-pass would ring like a bell, so instead we shape the
        // enveloped noise with two gentle 1-pole filters: a low-pass to darken
        // the hiss and a high-pass to drop the sub-bass, leaving a broad,
        // explosive band rather than a single tone. Soft (~4 ms) attack avoids
        // click transients; ~0.3 s decay.
        // The recorded explosion (death.wav) is a long (~2.5 s) dark
        // (centroid ~630 Hz) broadband-noise rumble, so use a slow decay and
        // heavy low-passing.
        let hit_env = b.rc_envelope("hit_env", hit_in, 0.008, 0.95);
        let hit_noise = b.multiply("hit_noise", noise, hit_env);
        // Three cascaded 1-pole low-passes (-18 dB/oct) darken the hiss into a
        // deep boom matching the sample's ~630 Hz centroid.
        let hit_lp1 = b.low_pass_hz("hit_lp1", hit_noise, 520.0);
        let hit_lp2 = b.low_pass_hz("hit_lp2", hit_lp1, 520.0);
        let hit_lp3 = b.low_pass_hz("hit_lp3", hit_lp2, 520.0);
        // High-pass ~120 Hz (tau = R·C) to remove the rumble's sub-bass.
        let hit_hp = b.rc_high_pass("hit_hp", hit_lp3, 13_000.0, 1e-7);
        let hit_out = b.gain("hit_lvl", hit_hp, 3.4);

        // --- FIRE / shoot: bright noise burst -------------------------------
        // MAME's fire is a bright (~5.5 kHz centroid), very *noisy* (high
        // spectral flatness) burst: a 555 VCO whose control voltage is driven by
        // the LFSR noise. We make it noise-dominated (a high-passed raw-noise
        // bed plus a lighter noise-FM'd VCO for the descending zap character),
        // enveloped with a ~80 ms decay.
        // The recorded shot (shot.wav) is a ~0.6 s bright (centroid ~4.9 kHz)
        // broadband-noise burst with a smooth attack, so use a softer attack and
        // a longer decay than a quick blip.
        let fire_env = b.rc_envelope("fire_env", fire_in, 0.010, 0.18);
        let fire_base = b.constant("fire_base", 2400.0);
        let fire_sweep = b.gain("fire_sweep", fire_env, 1400.0);
        // Heavy noise FM makes the VCO jump pitch every noise clock, which is
        // what gives the shot its broadband, noise-like character (the raw LFSR
        // noise is band-limited too low to survive the high-pass).
        let fire_fm = b.gain("fire_fm", noise, 3200.0);
        let fire_freq_raw = b.add("fire_freq_raw", &[fire_base, fire_sweep, fire_fm]);
        let fire_freq = b.clamp("fire_freq", fire_freq_raw, 300.0, 18000.0);
        let fire_osc = b.variable_square("fire_osc", fire_freq);
        let fire_tone = b.gain("fire_tone", fire_osc, 0.52);
        let fire_grit = b.gain("fire_grit", noise, 0.13);
        let fire_src = b.add("fire_src", &[fire_tone, fire_grit]);
        // High-pass (~4.4 kHz) for the bright noisy "pew" (sample centroid ~4.9 kHz).
        let fire_hp = b.rc_high_pass("fire_hp", fire_src, 3_600.0, 1e-8);
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
