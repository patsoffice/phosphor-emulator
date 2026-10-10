//! Namco 15XX: the eight-voice wavetable generator of the Super Pac-Man and
//! Mappy boards.
//!
//! The same idea as the three-voice WSG in [`super::namco_wsg`] (a 20-bit phase
//! accumulator per voice indexing a 32-sample, 4-bit waveform out of a 256x4
//! PROM), with two structural differences that decide how a board wires it:
//!
//! - **Its registers are RAM.** The chip has no register file of its own on the
//!   CPU side. It reads its voice parameters out of the first 64 bytes of the
//!   1 KB sound RAM that the main and sound CPUs share, eight bytes per voice.
//!   So a board keeps the RAM and forwards writes in that window here, and a
//!   read of the window is a read of the RAM.
//! - **It has no DAC.** It streams a voice's 4-bit sample and 4-bit volume out
//!   on separate lines, time-multiplexed, and the board does the multiply: an
//!   LS273 and a 4066-switched resistor network on Super Pac-Man, the 99XX on
//!   Mappy. That is why the voices leave through [`Namco15xx::voices`] as two
//!   codes rather than as a mixed sample.
//!
//! Register layout, per voice `v` at `v * 8` (bytes, as the 6809s write them):
//!
//! ```text
//!   +2   bits 0-4  overwrite the top five bits of the phase accumulator
//!   +3   bits 0-3  volume
//!   +4   bits 0-7  frequency bits 0-7
//!   +5   bits 0-7  frequency bits 8-15
//!   +6   bits 0-3  frequency bits 16-19
//!        bits 4-6  waveform select
//! ```
//!
//! Offsets +0, +1 and +7 are ordinary RAM as far as anything known goes. The
//! +2 write is how Grobda plays its speech sample, holding a voice at zero
//! frequency and stepping its waveform position directly; Super Pac-Man does
//! not depend on it, but it costs nothing to carry.
//!
//! The layout and the +2 behavior are inferred from what the programs write:
//! the chip has not been decapped and its datasheet, if there was one, is not
//! available.
//!
//! # Clock
//!
//! [`Namco15xx::tick`] is one voice update, 24 kHz on the Super Pac-Man board,
//! in which every voice advances once. The rate is the board's: the chip reads
//! the sound RAM while 2H is high, with the voice picked by H bits 4-6 and the
//! half of its eight-byte record by the chip's own pin 6, so a full pass over
//! all eight voices' records takes 256 of the 6.144 MHz dot clocks.
//!
//! The phase accumulator carries 15 fractional bits above the 5-bit waveform
//! position, so a voice's waveform repeats at `frequency * 24000 / 2^20` Hz.
//! That is the same arithmetic as the three-voice WSG's, whose 96 kHz clock
//! also adds the frequency once per tick into 15 fractional bits.

use crate::prelude::Saveable;

/// Number of voices.
pub const VOICES: usize = 8;

/// Fractional bits of the phase accumulator, below the 5-bit waveform position.
const FRAC_BITS: u32 = 15;

/// One voice's decoded parameters and its phase.
#[derive(Clone, Copy, Default, Saveable)]
struct Voice {
    /// 20-bit phase increment per tick.
    frequency: u32,
    /// Phase accumulator: waveform position in bits 15-19.
    counter: u32,
    /// Volume code, 0-15.
    volume: u8,
    /// Waveform select, 0-7.
    waveform: u8,
}

/// The 15XX's voices and the copy of the register window it decodes them from.
#[derive(Saveable)]
#[save_version(1)]
pub struct Namco15xx {
    voices: [Voice; VOICES],
    /// The last value written to each byte of the 64-byte register window, so
    /// a write that changes nothing changes nothing (the +2 phase write in
    /// particular must not re-fire on a repeat).
    regs: [u8; 64],
    /// 8 waveforms x 32 samples, low nibble only. The sound PROM; not state.
    #[save_skip]
    waveform_rom: [u8; 256],
    /// `SOUND ON`: the board clears its output latch when this is low, so the
    /// voices still run but nothing reaches the DAC.
    sound_enabled: bool,
}

impl Default for Namco15xx {
    fn default() -> Self {
        Self::new()
    }
}

impl Namco15xx {
    pub fn new() -> Self {
        Self {
            voices: [Voice::default(); VOICES],
            regs: [0; 64],
            waveform_rom: [0; 256],
            sound_enabled: false,
        }
    }

    /// Load the 256x4 waveform PROM. Only the low nibble of each byte is wired.
    pub fn load_waveform_rom(&mut self, data: &[u8]) {
        let len = data.len().min(256);
        self.waveform_rom[..len].copy_from_slice(&data[..len]);
    }

    /// Drive `SOUND ON`.
    pub fn set_sound_enabled(&mut self, enabled: bool) {
        self.sound_enabled = enabled;
    }

    pub fn sound_enabled(&self) -> bool {
        self.sound_enabled
    }

    /// A CPU wrote `data` to byte `offset` of the sound RAM. Offsets past the
    /// 64-byte register window are not the chip's and are ignored, so a board
    /// can forward every sound-RAM write without decoding it first.
    pub fn write(&mut self, offset: u16, data: u8) {
        let offset = offset as usize;
        if offset >= self.regs.len() || self.regs[offset] == data {
            return;
        }
        self.regs[offset] = data;

        let ch = offset >> 3;
        let base = ch * 8;
        let voice = &mut self.voices[ch];
        match offset & 7 {
            2 => {
                let frac = voice.counter & ((1 << FRAC_BITS) - 1);
                voice.counter = frac | (u32::from(data & 0x1F) << FRAC_BITS);
            }
            3 => voice.volume = data & 0x0F,
            4..=6 => {
                if offset & 7 == 6 {
                    voice.waveform = (data >> 4) & 7;
                }
                voice.frequency = u32::from(self.regs[base + 4])
                    | (u32::from(self.regs[base + 5]) << 8)
                    | (u32::from(self.regs[base + 6] & 0x0F) << 16);
            }
            _ => {}
        }
    }

    /// One period of the sample clock: every voice advances by its frequency.
    ///
    /// Every voice advances whether or not its volume is zero. The three-voice
    /// WSG here freezes a silent voice's phase and documents that as a
    /// divergence; for this chip neither behavior has been established, and
    /// the counters are internal to it, so the simpler one is taken. The
    /// difference is a phase offset on the first period after a voice is
    /// unmuted.
    pub fn tick(&mut self) {
        for voice in &mut self.voices {
            voice.counter = voice.counter.wrapping_add(voice.frequency) & 0xF_FFFF;
        }
    }

    /// What each voice presents to the board's latch right now: its 4-bit
    /// waveform sample as a signed value (-8..+7, code 8 being zero) and its
    /// volume code.
    ///
    /// Sound disabled reports `(0, 0)` for every voice, because `SOUND ON` is
    /// the latch's clear: the board zeroes sample and volume together rather
    /// than muting anything downstream. Same convention as
    /// [`NamcoWsg::tick_voices`](super::namco_wsg::NamcoWsg::tick_voices).
    pub fn voices(&self) -> [(i32, u8); VOICES] {
        let mut out = [(0, 0); VOICES];
        if !self.sound_enabled {
            return out;
        }
        for (slot, voice) in out.iter_mut().zip(&self.voices) {
            let pos = ((voice.counter >> FRAC_BITS) & 0x1F) as usize;
            let code = self.waveform_rom[(voice.waveform as usize) * 32 + pos] & 0x0F;
            *slot = (i32::from(code) - 8, voice.volume);
        }
        out
    }

    pub fn reset(&mut self) {
        self.voices = [Voice::default(); VOICES];
        self.regs = [0; 64];
        self.sound_enabled = false;
    }
}

impl super::Device for Namco15xx {
    fn name(&self) -> &'static str {
        "Namco 15XX"
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

impl Debuggable for Namco15xx {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        const FREQ: [&str; VOICES] = [
            "FREQ0", "FREQ1", "FREQ2", "FREQ3", "FREQ4", "FREQ5", "FREQ6", "FREQ7",
        ];
        const VOL: [&str; VOICES] = [
            "VOL0", "VOL1", "VOL2", "VOL3", "VOL4", "VOL5", "VOL6", "VOL7",
        ];
        const WAVE: [&str; VOICES] = [
            "WAVE0", "WAVE1", "WAVE2", "WAVE3", "WAVE4", "WAVE5", "WAVE6", "WAVE7",
        ];
        let mut regs = vec![DebugRegister {
            name: "ENABLED",
            value: self.sound_enabled as u64,
            width: 8,
        }];
        for (i, v) in self.voices.iter().enumerate() {
            regs.push(DebugRegister {
                name: FREQ[i],
                value: v.frequency as u64,
                width: 24,
            });
            regs.push(DebugRegister {
                name: VOL[i],
                value: v.volume as u64,
                width: 8,
            });
            regs.push(DebugRegister {
                name: WAVE[i],
                value: v.waveform as u64,
                width: 8,
            });
        }
        regs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A chip whose PROM makes every sample identify itself: waveform `w`,
    /// position `p` holds `(w + p) & 0x0F`, with garbage in the high nibble to
    /// catch a decode that forgets the mask.
    fn chip() -> Namco15xx {
        let mut c = Namco15xx::new();
        let mut rom = [0u8; 256];
        for w in 0..8 {
            for p in 0..32 {
                rom[w * 32 + p] = 0xF0 | (((w + p) & 0x0F) as u8);
            }
        }
        c.load_waveform_rom(&rom);
        c.set_sound_enabled(true);
        c
    }

    #[test]
    fn each_voice_decodes_its_own_eight_bytes() {
        let mut c = chip();
        for v in 0..VOICES as u16 {
            c.write(v * 8 + 3, v as u8 + 1);
            c.write(v * 8 + 6, ((7 - v as u8) << 4) | 0x0A);
            c.write(v * 8 + 5, 0x30 + v as u8);
            c.write(v * 8 + 4, 0x40 + v as u8);
        }
        for (v, voice) in c.voices.iter().enumerate() {
            assert_eq!(voice.volume, v as u8 + 1, "voice {v} volume");
            assert_eq!(voice.waveform, 7 - v as u8, "voice {v} waveform");
            assert_eq!(
                voice.frequency,
                0xA_0000 | ((0x30 + v as u32) << 8) | (0x40 + v as u32),
                "voice {v} frequency"
            );
        }
    }

    #[test]
    fn the_waveform_nibble_does_not_leak_into_the_frequency() {
        // +6 carries both: waveform in the high nibble, frequency bits 16-19 in
        // the low. A decode that took the whole byte would put the waveform at
        // bits 20-23 of a 20-bit frequency.
        let mut c = chip();
        c.write(6, 0x7F);
        assert_eq!(c.voices[0].frequency, 0xF_0000);
        assert_eq!(c.voices[0].waveform, 7);
    }

    #[test]
    fn the_volume_keeps_only_four_bits() {
        let mut c = chip();
        c.write(3, 0xF5);
        assert_eq!(c.voices[0].volume, 5);
    }

    #[test]
    fn writes_past_the_register_window_are_not_the_chips() {
        let mut c = chip();
        c.write(0x40 + 3, 9);
        c.write(0x3FF, 9);
        assert!(c.voices.iter().all(|v| v.volume == 0));
    }

    #[test]
    fn the_counter_advances_by_the_frequency_per_tick_in_sequence() {
        // The sequence, not the total: a step that skipped a tick and then
        // doubled would land on the same final value.
        let mut c = chip();
        c.write(4, 0x23);
        for n in 1..=6u32 {
            c.tick();
            assert_eq!(c.voices[0].counter, 0x23 * n, "after tick {n}");
        }
    }

    #[test]
    fn a_waveform_period_is_two_to_the_twentieth_over_the_frequency() {
        // frequency 0x8000 (2^15) moves one position per tick, so 32 ticks are
        // one period and the counter is back where it started.
        let mut c = chip();
        c.write(3, 1);
        c.write(5, 0x80);
        let mut positions = Vec::new();
        for _ in 0..32 {
            positions.push(c.voices()[0].0 + 8);
            c.tick();
        }
        let want: Vec<i32> = (0..32).map(|p| p & 0x0F).collect();
        assert_eq!(positions, want, "waveform 0 read position by position");
        assert_eq!(c.voices[0].counter, 0, "the 20-bit counter wraps");
    }

    #[test]
    fn the_sample_is_the_proms_low_nibble_biased_to_signed() {
        let mut c = chip();
        c.write(3, 1);
        c.write(6, 0x70); // waveform 7: position 0 holds 7
        assert_eq!(c.voices()[0], (-1, 1));
        c.write(6, 0x00); // waveform 0: position 0 holds 0
        assert_eq!(c.voices()[0], (-8, 1));
    }

    #[test]
    fn the_phase_write_sets_the_waveform_position_and_keeps_the_fraction() {
        let mut c = chip();
        c.write(3, 1);
        c.write(4, 0x01);
        c.tick(); // a fractional phase of 1
        c.write(2, 0x05);
        assert_eq!(c.voices[0].counter, (5 << FRAC_BITS) | 1);
        assert_eq!(c.voices()[0].0 + 8, 5, "waveform 0 position 5");
        // Only five bits reach the counter.
        c.write(2, 0xFF);
        assert_eq!(c.voices[0].counter >> FRAC_BITS, 0x1F);
    }

    #[test]
    fn a_repeated_write_is_not_a_second_write() {
        // The phase write moves the counter, so it is the one register where
        // the cache of the last value written is observable.
        let mut c = chip();
        c.write(2, 0x03);
        c.write(4, 0x10);
        c.tick();
        let before = c.voices[0].counter;
        c.write(2, 0x03);
        assert_eq!(c.voices[0].counter, before);
    }

    #[test]
    fn sound_off_clears_every_voices_latch_but_not_its_phase() {
        let mut c = chip();
        c.write(3, 15);
        c.write(6, 0x70);
        c.write(4, 0x55);
        assert_ne!(c.voices()[0], (0, 0));
        c.set_sound_enabled(false);
        assert_eq!(c.voices(), [(0, 0); VOICES]);
        let before = c.voices[0].counter;
        c.tick();
        assert_ne!(c.voices[0].counter, before, "the voices keep running");
    }

    #[test]
    fn reset_clears_voices_and_registers_but_keeps_the_prom() {
        let mut c = chip();
        let rom = c.waveform_rom;
        for o in 0..64u16 {
            c.write(o, 0xFF);
        }
        c.tick();
        c.reset();
        assert_eq!(c.regs, [0; 64]);
        assert!(!c.sound_enabled);
        assert!(c.voices.iter().all(|v| v.counter == 0 && v.frequency == 0));
        assert_eq!(c.waveform_rom, rom);
    }
}
