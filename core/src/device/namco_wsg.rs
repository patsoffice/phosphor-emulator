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

        // Sum contributions from all voices
        let mut mixed: i32 = 0;
        for voice in &mut self.voices {
            if voice.volume == 0 {
                continue;
            }

            // Advance counter by frequency
            voice.counter = voice.counter.wrapping_add(voice.frequency);

            // Look up waveform sample (4-bit signed: 0-15 mapped to -8..+7)
            let pos = ((voice.counter >> F_FRACBITS) & 0x1F) as usize;
            let wave_offset = (voice.waveform_select as usize) * 32 + pos;
            let sample = (self.waveform_rom[wave_offset] & 0x0F) as i32 - 8;

            mixed += sample * voice.volume as i32;
        }

        // Scale to i16 range. Each voice max: 7 * 15 = 105. Three voices: 315.
        // Scale so max output uses ~75% of i16 range.
        self.resampler.tick((mixed * 80) as i16);
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
