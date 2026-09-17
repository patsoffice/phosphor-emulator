//! Namco WSG (Waveform Sound Generator) — 3-voice wavetable synthesizer.
//!
//! Used in Pac-Man, Pengo, Dig Dug, and other early Namco arcade games.
//! Each voice reads through a 32-sample, 4-bit waveform at a programmable
//! frequency and volume. The waveform data comes from a PROM containing
//! 8 selectable waveforms.
//!
//! Clock: master_clock / 6 / 32 = 96 KHz for 18.432 MHz master.
//! Register interface: 32 nibble-wide registers written at 0x5040–0x505F.

use crate::audio::{AudioResampler, host_sample_rate};
use crate::prelude::Saveable;

/// 3-voice Namco WSG wavetable synthesizer.
#[derive(Saveable)]
#[save_version(1)]
pub struct NamcoWsg {
    voices: [WsgVoice; 3],
    sound_regs: [u8; 32],

    /// 8 waveforms × 32 samples, 4 bits per sample (only low nibble used).
    /// Loaded from the sound PROM (82s126.1m for Pac-Man, 256 bytes).
    #[save_skip]
    waveform_rom: [u8; 256],

    sound_enabled: bool,

    resampler: AudioResampler<i16>,
}

#[derive(Default, Saveable)]
struct WsgVoice {
    frequency: u32,
    counter: u32,
    volume: u8,
    waveform_select: u8,
}

/// Fractional bits for the frequency counter.
///
/// The WSG input clock is master / 6 / 32 = 96 kHz for 18.432 MHz.
/// MAME doubles this to 192 kHz and uses f_fracbits = clock_multiple + 15
/// = 1 + 15 = 16 for its internal stream rate.
///
/// We advance the counter at the CPU clock rate (3.072 MHz) instead of
/// 192 kHz, which is 16× faster. To compensate, we add 4 extra fractional
/// bits: 16 + 4 = 20. This yields identical waveform rates:
///   MAME:  freq × 192000 / 2^(16+5) = freq × 192000 / 2^21
///   Ours:  freq × 3072000 / 2^(20+5) = freq × 3072000 / 2^25 = freq × 192000 / 2^21
const F_FRACBITS: u32 = 20;

impl NamcoWsg {
    /// Create a new WSG with the given CPU clock rate (e.g., 3_072_000).
    pub fn new(cpu_clock_hz: u64) -> Self {
        Self {
            voices: [
                WsgVoice::default(),
                WsgVoice::default(),
                WsgVoice::default(),
            ],
            sound_regs: [0; 32],
            waveform_rom: [0; 256],
            sound_enabled: false,
            resampler: AudioResampler::new(cpu_clock_hz, host_sample_rate() as u64),
        }
    }

    /// Load the waveform PROM data (256 bytes, only low 4 bits of each byte used).
    pub fn load_waveform_rom(&mut self, data: &[u8]) {
        let len = data.len().min(256);
        self.waveform_rom[..len].copy_from_slice(&data[..len]);
    }

    /// Enable or disable sound output.
    pub fn set_sound_enabled(&mut self, enabled: bool) {
        self.sound_enabled = enabled;
    }

    /// Write a nibble register (offset 0x00–0x1F, only low 4 bits of data used).
    ///
    /// Register map (from MAME namco.cpp):
    ///   0x05:       Ch 0 waveform select
    ///   0x0A:       Ch 1 waveform select
    ///   0x0F:       Ch 2 waveform select
    ///   0x10:       Ch 0 extra frequency bits (20-bit total)
    ///   0x11-0x14:  Ch 0 frequency nibbles
    ///   0x15:       Ch 0 volume
    ///   0x16-0x19:  Ch 1 frequency nibbles
    ///   0x1A:       Ch 1 volume
    ///   0x1B-0x1E:  Ch 2 frequency nibbles
    ///   0x1F:       Ch 2 volume
    pub fn write(&mut self, offset: u16, data: u8) {
        let offset = (offset & 0x1F) as usize;
        let data = data & 0x0F;

        if self.sound_regs[offset] == data {
            return;
        }
        self.sound_regs[offset] = data;

        // Determine which channel this register affects
        let ch = if offset < 0x10 {
            (offset.wrapping_sub(5)) / 5
        } else if offset == 0x10 {
            0
        } else {
            (offset - 0x11) / 5
        };

        if ch >= 3 {
            return;
        }

        let voice = &mut self.voices[ch];
        let reg_in_ch = offset - ch * 5;

        match reg_in_ch {
            0x05 => {
                voice.waveform_select = data & 7;
            }
            0x10..=0x14 => {
                // Channel 0 has 20-bit frequency, channels 1-2 have 16-bit
                let regs = &self.sound_regs;
                voice.frequency = if ch == 0 { regs[0x10] as u32 } else { 0 };
                voice.frequency += (regs[ch * 5 + 0x11] as u32) << 4;
                voice.frequency += (regs[ch * 5 + 0x12] as u32) << 8;
                voice.frequency += (regs[ch * 5 + 0x13] as u32) << 12;
                voice.frequency += (regs[ch * 5 + 0x14] as u32) << 16;
            }
            0x15 => {
                voice.volume = data;
            }
            _ => {}
        }
    }

    /// Advance the WSG by one CPU clock cycle. Call at the CPU clock rate.
    ///
    /// The WSG counter advances every 32 CPU clocks on real hardware.
    /// We accumulate at CPU rate — the fractional bits handle the division.
    pub fn tick(&mut self) {
        if !self.sound_enabled {
            self.resampler.tick(0);
            return;
        }
        let mixed = self.mix();
        // Scale to i16 range. Each voice max: 7 * 15 = 105. Three voices: 315.
        // Scale so max output uses ~75% of i16 range.
        self.resampler.tick((mixed * 80) as i16);
    }

    /// Advance every voice one step and return their summed, volume-scaled
    /// sample, before the output scaling and the resampler.
    ///
    /// Split out of [`Self::tick`] so the synthesis can be asserted directly.
    /// Reading it back through `fill_audio` instead would put a windowed-sinc
    /// filter and its group delay between the test and the thing under test,
    /// which is a poor way to ask what sample a waveform position holds.
    fn mix(&mut self) -> i32 {
        self.step_voices()
            .iter()
            .map(|(sample, volume)| sample * *volume as i32)
            .sum()
    }

    /// Advance every voice one step and return each one's waveform sample
    /// (4-bit signed, -8..+7) and volume code (0-15) separately.
    ///
    /// A voice whose volume is zero reports `(0, 0)` and does not advance,
    /// which is what [`Self::mix`] has always done.
    fn step_voices(&mut self) -> [(i32, u8); 3] {
        let mut out = [(0, 0); 3];
        for (slot, voice) in self.voices.iter_mut().enumerate() {
            if voice.volume == 0 {
                continue;
            }

            // Advance counter by frequency
            voice.counter = voice.counter.wrapping_add(voice.frequency);

            // Look up waveform sample (4-bit signed: 0-15 mapped to -8..+7)
            let pos = ((voice.counter >> F_FRACBITS) & 0x1F) as usize;
            let wave_offset = (voice.waveform_select as usize) * 32 + pos;
            let sample = (self.waveform_rom[wave_offset] & 0x0F) as i32 - 8;

            out[slot] = (sample, voice.volume);
        }
        out
    }

    /// Advance one CPU clock and return what the board's sample-and-volume
    /// latch holds for each voice, for a board that performs the multiply in
    /// its own analog stage instead of taking [`Self::tick`]'s summed output.
    ///
    /// The multiply is not always the chip's to do. On the Pac-Man board the
    /// two four-bit fields leave the 74LS273 as eight separate lines and are
    /// multiplied by two switched resistor networks, and neither network is an
    /// exact binary ladder, so the product this chip computes is the one thing
    /// that board never forms. See `docs/schematics/pacman-audio-output.md`.
    ///
    /// **A board calls this or [`Self::tick`], never both**: each advances the
    /// voices, and the internal resampler this one does not feed is the one
    /// [`Self::fill_audio`] drains.
    ///
    /// Sound disabled reports `(0, 0)` for every voice, because `SOUND ON` is
    /// the latch's CLR: the board zeroes the sample and the volume together at
    /// the latch rather than muting anything downstream.
    pub fn tick_voices(&mut self) -> [(i32, u8); 3] {
        if !self.sound_enabled {
            return [(0, 0); 3];
        }
        self.step_voices()
    }

    /// Drain audio samples into the provided buffer. Returns number of samples written.
    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.resampler.fill_audio(buffer)
    }

    /// Reset the WSG to initial state.
    pub fn reset(&mut self) {
        for voice in &mut self.voices {
            voice.frequency = 0;
            voice.counter = 0;
            voice.volume = 0;
            voice.waveform_select = 0;
        }
        self.sound_regs = [0; 32];
        self.sound_enabled = false;
        self.resampler.reset();
    }
}

impl super::Device for NamcoWsg {
    fn name(&self) -> &'static str {
        "Namco WSG"
    }
    fn reset(&mut self) {
        self.reset();
    }
    fn write(&mut self, offset: u16, data: u8) {
        self.write(offset, data);
    }
    fn tick(&mut self) {
        self.tick();
    }
}

use crate::core::debug::{DebugRegister, Debuggable};

impl Debuggable for NamcoWsg {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "ENABLED",
                value: self.sound_enabled as u64,
                width: 8,
            },
            DebugRegister {
                name: "FREQ0",
                value: self.voices[0].frequency as u64,
                width: 16,
            },
            DebugRegister {
                name: "VOL0",
                value: self.voices[0].volume as u64,
                width: 8,
            },
            DebugRegister {
                name: "WAVE0",
                value: self.voices[0].waveform_select as u64,
                width: 8,
            },
            DebugRegister {
                name: "FREQ1",
                value: self.voices[1].frequency as u64,
                width: 16,
            },
            DebugRegister {
                name: "VOL1",
                value: self.voices[1].volume as u64,
                width: 8,
            },
            DebugRegister {
                name: "WAVE1",
                value: self.voices[1].waveform_select as u64,
                width: 8,
            },
            DebugRegister {
                name: "FREQ2",
                value: self.voices[2].frequency as u64,
                width: 16,
            },
            DebugRegister {
                name: "VOL2",
                value: self.voices[2].volume as u64,
                width: 8,
            },
            DebugRegister {
                name: "WAVE2",
                value: self.voices[2].waveform_select as u64,
                width: 8,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WSG with a waveform PROM whose eight tables are each a recognizable
    /// ramp, so a sample's value says which waveform and which position it came
    /// from.
    ///
    /// Waveform `w`, position `p`, holds `(w + p) & 0x0F`. The nibble mask is
    /// the PROM's own: only the low four bits of each byte are wired.
    fn wsg() -> NamcoWsg {
        let mut w = NamcoWsg::new(3_072_000);
        let mut rom = [0u8; 256];
        for wave in 0..8 {
            for pos in 0..32 {
                // The high nibble is deliberately garbage, to catch a decode
                // that reads the whole byte instead of masking.
                rom[wave * 32 + pos] = 0xF0 | (((wave + pos) & 0x0F) as u8);
            }
        }
        w.load_waveform_rom(&rom);
        w
    }

    /// Write a channel's four frequency nibbles, low first.
    fn set_freq(w: &mut NamcoWsg, ch: usize, nibbles: [u8; 4]) {
        let base = 0x11 + ch * 5;
        for (i, n) in nibbles.iter().enumerate() {
            w.write((base + i) as u16, *n);
        }
    }

    // --- The register contract ------------------------------------------

    #[test]
    fn each_channel_takes_its_volume_from_its_own_register() {
        let mut w = wsg();
        w.write(0x15, 3);
        w.write(0x1A, 7);
        w.write(0x1F, 11);
        assert_eq!(w.voices[0].volume, 3);
        assert_eq!(w.voices[1].volume, 7);
        assert_eq!(w.voices[2].volume, 11);
    }

    #[test]
    fn each_channel_takes_its_waveform_from_its_own_register() {
        let mut w = wsg();
        w.write(0x05, 1);
        w.write(0x0A, 2);
        w.write(0x0F, 3);
        assert_eq!(w.voices[0].waveform_select, 1);
        assert_eq!(w.voices[1].waveform_select, 2);
        assert_eq!(w.voices[2].waveform_select, 3);
    }

    #[test]
    fn the_waveform_select_keeps_only_three_bits() {
        // Eight waveforms live in a 256-byte PROM, so a fourth bit would index
        // past the end of it.
        let mut w = wsg();
        w.write(0x05, 0x0F);
        assert_eq!(w.voices[0].waveform_select, 7);
    }

    #[test]
    fn channel_zero_has_twenty_frequency_bits_and_the_others_sixteen() {
        // This is the asymmetry the register map exists to express: channel 0
        // gets an extra low nibble at 0x10 that channels 1 and 2 do not have,
        // so the same four nibbles mean a value sixteen times larger on the
        // other two channels.
        let mut w = wsg();
        set_freq(&mut w, 0, [1, 0, 0, 0]);
        set_freq(&mut w, 1, [1, 0, 0, 0]);
        assert_eq!(w.voices[0].frequency, 0x10);
        assert_eq!(w.voices[1].frequency, 0x10);

        // ... and only channel 0 responds to 0x10 at all.
        w.write(0x10, 0x0F);
        assert_eq!(w.voices[0].frequency, 0x1F);
        assert_eq!(w.voices[1].frequency, 0x10, "0x10 is channel 0's alone");
    }

    #[test]
    fn the_frequency_nibbles_stack_into_one_value() {
        let mut w = wsg();
        w.write(0x10, 0x1);
        set_freq(&mut w, 0, [0x2, 0x3, 0x4, 0x5]);
        assert_eq!(w.voices[0].frequency, 0x54321);

        set_freq(&mut w, 2, [0xF, 0xE, 0xD, 0xC]);
        assert_eq!(w.voices[2].frequency, 0xCDEF0);
    }

    #[test]
    fn a_write_keeps_only_the_low_nibble() {
        // The bus is four bits wide; the high nibble of a byte written here is
        // not connected to anything.
        let mut w = wsg();
        w.write(0x15, 0xF3);
        assert_eq!(w.voices[0].volume, 3);
        w.write(0x10, 0xFA);
        assert_eq!(w.voices[0].frequency, 0x0A);
    }

    #[test]
    fn the_offset_wraps_into_the_thirty_two_register_window() {
        // The register file is 32 nibbles and the board does not decode above
        // it, so a mirrored address reaches the same register.
        let mut w = wsg();
        w.write(0x15, 5);
        assert_eq!(w.voices[0].volume, 5);
        w.write(0x35, 9);
        assert_eq!(w.voices[0].volume, 9, "0x35 mirrors 0x15");
    }

    #[test]
    fn registers_belonging_to_no_channel_disturb_no_voice() {
        // 0x00 through 0x04 and the gaps at 0x06-0x09 and 0x0B-0x0E are not
        // voice registers. The channel arithmetic in `write` has to reject them
        // rather than fold them onto a voice, which is the kind of thing an
        // off-by-one in that arithmetic would do silently.
        let mut w = wsg();
        w.write(0x15, 5);
        w.write(0x05, 2);
        set_freq(&mut w, 0, [1, 2, 3, 4]);
        let before = (
            w.voices[0].volume,
            w.voices[0].waveform_select,
            w.voices[0].frequency,
        );
        for offset in [0x00, 0x01, 0x02, 0x03, 0x04, 0x06, 0x09, 0x0B, 0x0E] {
            w.write(offset, 0x0F);
        }
        let after = (
            w.voices[0].volume,
            w.voices[0].waveform_select,
            w.voices[0].frequency,
        );
        assert_eq!(before, after, "a non-voice register moved a voice");
        assert_eq!(w.voices[1].volume, 0);
        assert_eq!(w.voices[2].volume, 0);
    }

    // --- The waveform PROM ----------------------------------------------

    #[test]
    fn a_sample_is_the_proms_low_nibble_biased_to_signed() {
        // The PROM holds an unsigned nibble and the DAC treats it as signed
        // around its midpoint, so 0 is the most negative step and 15 the most
        // positive. Reading it unbiased would put silence at a hard offset.
        let mut w = wsg();
        w.set_sound_enabled(true);
        w.write(0x15, 1); // channel 0 at volume 1, so mixed == the sample
        w.write(0x05, 0); // waveform 0, whose position 0 holds 0
        assert_eq!(w.mix(), -8, "nibble 0 is the bottom of the range");

        w.write(0x05, 7); // waveform 7, whose position 0 holds 7
        assert_eq!(w.mix(), -1);
    }

    #[test]
    fn the_waveform_select_picks_a_different_table() {
        let mut w = wsg();
        w.set_sound_enabled(true);
        w.write(0x15, 1);
        // The fixture's waveform w at position 0 holds w, so the sample is
        // w - 8 and each selection is distinguishable from the others.
        for wave in 0..8u8 {
            w.write(0x05, wave);
            w.voices[0].counter = 0;
            assert_eq!(w.mix(), i32::from(wave) - 8, "waveform {wave}");
        }
    }

    // --- The frequency accumulator --------------------------------------

    #[test]
    fn the_counter_advances_by_the_frequency_each_tick() {
        let mut w = wsg();
        w.set_sound_enabled(true);
        w.write(0x15, 1);
        w.write(0x10, 0x3);
        set_freq(&mut w, 0, [0x2, 0x0, 0x0, 0x0]);
        assert_eq!(w.voices[0].frequency, 0x23);

        for n in 1..=5u32 {
            w.tick();
            assert_eq!(w.voices[0].counter, 0x23 * n);
        }
    }

    #[test]
    fn the_waveform_position_is_the_top_five_bits_of_the_counter() {
        // F_FRACBITS of the counter are fractional; the next five index the
        // 32-sample table, and everything above that wraps.
        let mut w = wsg();
        w.set_sound_enabled(true);
        w.write(0x15, 1);
        w.write(0x05, 0);

        // Park the counter just below each position boundary and step over it.
        for pos in 0..32u32 {
            w.voices[0].counter = pos << F_FRACBITS;
            // Waveform 0 position p holds p & 0x0F, biased by -8.
            let want = ((pos & 0x0F) as i32) - 8;
            assert_eq!(w.mix(), want, "position {pos}");
        }

        // Position 32 is position 0 again: the table is 32 samples long.
        w.voices[0].counter = 32 << F_FRACBITS;
        assert_eq!(w.mix(), -8);
    }

    #[test]
    fn a_voice_at_volume_zero_contributes_nothing() {
        let mut w = wsg();
        w.set_sound_enabled(true);
        w.write(0x05, 7); // a waveform whose samples are not zero
        w.write(0x15, 0);
        assert_eq!(w.mix(), 0);
        w.write(0x15, 1);
        assert_eq!(w.mix(), -1);
    }

    #[test]
    fn volume_scales_the_sample_linearly() {
        let mut w = wsg();
        w.set_sound_enabled(true);
        w.write(0x05, 7); // position 0 holds 7, so the sample is -1
        for vol in 1..=15u8 {
            w.write(0x15, vol);
            assert_eq!(w.mix(), -i32::from(vol), "volume {vol}");
        }
    }

    #[test]
    fn the_three_voices_sum() {
        let mut w = wsg();
        w.set_sound_enabled(true);
        // Waveform 7 at position 0 is -1, waveform 6 is -2, waveform 5 is -3.
        w.write(0x05, 7);
        w.write(0x0A, 6);
        w.write(0x0F, 5);
        w.write(0x15, 1);
        w.write(0x1A, 1);
        w.write(0x1F, 1);
        assert_eq!(w.mix(), -6);
    }

    // --- The sound-enable gate -------------------------------------------

    #[test]
    fn the_sound_enable_gate_silences_every_voice() {
        let mut w = wsg();
        w.write(0x05, 7);
        w.write(0x15, 15);
        w.write(0x10, 0x8);
        set_freq(&mut w, 0, [0, 0, 0, 0x8]);

        // Disabled from reset: nothing comes out however loud the voice is.
        assert!(!w.sound_enabled);
        let mut buf = [0i16; 256];
        for _ in 0..8000 {
            w.tick();
        }
        let n = w.fill_audio(&mut buf);
        assert!(n > 0, "the resampler produced no samples to judge");
        assert!(
            buf[..n].iter().all(|&s| s == 0),
            "a disabled WSG emitted a nonzero sample"
        );

        // Enabled, the same voice reaches the output.
        w.set_sound_enabled(true);
        for _ in 0..8000 {
            w.tick();
        }
        let n = w.fill_audio(&mut buf);
        assert!(
            buf[..n].iter().any(|&s| s != 0),
            "an enabled WSG with a loud voice emitted only silence"
        );
    }

    // --- Reset ------------------------------------------------------------

    #[test]
    fn reset_clears_every_voice_and_the_register_file() {
        let mut w = wsg();
        w.set_sound_enabled(true);
        for offset in 0..0x20u16 {
            w.write(offset, 0x0F);
        }
        for _ in 0..100 {
            w.tick();
        }
        assert!(w.voices[0].counter > 0, "the fixture did not actually run");

        w.reset();
        assert!(!w.sound_enabled);
        assert_eq!(w.sound_regs, [0; 32]);
        for (i, v) in w.voices.iter().enumerate() {
            assert_eq!(v.frequency, 0, "voice {i} frequency");
            assert_eq!(v.counter, 0, "voice {i} counter");
            assert_eq!(v.volume, 0, "voice {i} volume");
            assert_eq!(v.waveform_select, 0, "voice {i} waveform");
        }
    }

    #[test]
    fn reset_leaves_the_waveform_prom_alone() {
        // The PROM is a part on the board, not state: a reset line does not
        // erase it, and `#[save_skip]` on it says the same thing about a save
        // state. A reset that cleared it would silence the machine until
        // something reloaded the ROM, which nothing does.
        let mut w = wsg();
        let before = w.waveform_rom;
        w.reset();
        assert_eq!(w.waveform_rom, before);
    }

    // --- A divergence worth having written down ---------------------------

    #[test]
    fn a_silenced_voice_freezes_its_counter_rather_than_running_on() {
        // THIS DOCUMENTS CURRENT BEHAVIOR AND IS NOT AN ENDORSEMENT OF IT.
        //
        // `tick` skips a voice whose volume is zero, so its phase accumulator
        // stops. On the board the counters are clocked continuously and the
        // volume is applied downstream at the DAC, so a voice that is silenced
        // and then brought back should resume at the phase it would have
        // reached, not the one it left.
        //
        // The audible difference is a phase discontinuity where the hardware
        // has none. **It has been priced and it is inaudible on every machine
        // that uses the part**, so this stays as it is; see
        // phosphor-emulator-hszj for the survey.
        //
        // A phase discontinuity can only be heard across a gap short enough
        // that the ear joins the two segments into one tone, which is at most a
        // waveform period or two. Over the five committed movies (Pac-Man,
        // Ms. Pac-Man, Dig Dug, Galaga, Xevious; 12,505 frames, about 208
        // seconds of attract, a coin and real play) there were 689 resumes from
        // volume zero and the shortest gap of any kind was 7.53 ms. Only 78
        // resumes kept the same frequency and waveform across the gap, which is
        // the only case where the board's phase would line up with anything,
        // and the shortest of those was 40.88 ms: tens of periods of silence,
        // heard as an articulation rather than a glitch, and re-onset from
        // silence in either model.
        //
        // Advancing muted voices also costs 1 to 2 percent of emulation time on
        // these machines (phosphor-bench, release, best of 9: Galaga 0.957 to
        // 0.976 ms/frame, Xevious 1.216 to 1.227, Pac-Man 0.467 to 0.473), so
        // the trade is a measurable slowdown for no audible gain.
        let mut w = wsg();
        w.set_sound_enabled(true);
        w.write(0x10, 0x8);
        w.write(0x15, 1);
        for _ in 0..10 {
            w.tick();
        }
        let running = w.voices[0].counter;
        assert!(running > 0);

        w.write(0x15, 0);
        for _ in 0..10 {
            w.tick();
        }
        assert_eq!(
            w.voices[0].counter, running,
            "a muted voice's counter moved, so this test is stale and the \
             behavior it documents has changed"
        );
    }
}
