//! AY-8910 Programmable Sound Generator — 3-channel square wave + noise + envelope.
//!
//! Used across many arcade, home computer, and console systems including Midway
//! MCR (via SSIO sound board), MSX, ZX Spectrum 128, Amstrad CPC, and more.
//! The chip provides three independently controllable square-wave tone generators,
//! a noise generator using a 17-bit LFSR, and a single shared envelope generator.
//!
//! Internal clock: chip_clock / 8. Each tone/noise/envelope counter operates at
//! this divided rate. Register interface uses an address latch / data port protocol.
//!
//! # Register map
//!
//! | Reg | Name      | Description                                          |
//! |-----|-----------|------------------------------------------------------|
//! | R0  | TONE_A_LO | Channel A tone period, low 8 bits                    |
//! | R1  | TONE_A_HI | Channel A tone period, high 4 bits                   |
//! | R2  | TONE_B_LO | Channel B tone period, low 8 bits                    |
//! | R3  | TONE_B_HI | Channel B tone period, high 4 bits                   |
//! | R4  | TONE_C_LO | Channel C tone period, low 8 bits                    |
//! | R5  | TONE_C_HI | Channel C tone period, high 4 bits                   |
//! | R6  | NOISE_PER | Noise period (5 bits)                                |
//! | R7  | MIXER     | Mixer control (active-low enable) + I/O direction    |
//! | R8  | AMP_A     | Channel A amplitude (4-bit) or envelope mode (bit 4) |
//! | R9  | AMP_B     | Channel B amplitude                                  |
//! | R10 | AMP_C     | Channel C amplitude                                  |
//! | R11 | ENV_LO    | Envelope period, low 8 bits                          |
//! | R12 | ENV_HI    | Envelope period, high 8 bits                         |
//! | R13 | ENV_SHAPE | Envelope shape (4 bits)                              |
//! | R14 | PORT_A    | I/O Port A data                                      |
//! | R15 | PORT_B    | I/O Port B data                                      |

use crate::audio::{AudioResampler, host_sample_rate};
use phosphor_macros::Saveable;

/// AY-8910 DAC volume table (logarithmic, ~3 dB per step).
///
/// Level 0 produces silence (zero_is_off characteristic of the AY-8910).
/// Each subsequent level is approximately √2 (3.01 dB) louder than the previous.
/// Scaled so three channels at maximum volume use ~75% of i16 range.
const VOLUME_TABLE: [i32; 16] = [
    0, 64, 91, 128, 181, 256, 362, 512, 724, 1024, 1448, 2048, 2896, 4096, 5793, 8192,
];

/// AY-8910 Programmable Sound Generator.
///
/// # One output or three
///
/// By default the chip delivers the sum of its three channels through
/// [`Self::fill_audio`], which is what a board that ties A, B and C together
/// gets. A board that treats the channels differently (filters one, pans them,
/// meets them at different gains) calls [`Self::enable_channel_outputs`] once
/// and drains each channel with [`Self::fill_channel_audio`] instead. The two
/// modes are exclusive, so a chip only ever resamples what its board reads: a
/// buffer nobody drains would fill and count overruns.
///
/// # Resampled a group at a time
///
/// The generators update every eighth chip clock, so the output holds for
/// eight clocks at a stretch and changes between updates only when a register
/// or a channel gain is written. [`Self::tick`] records those stretches as
/// segments rather than feeding the resampler every clock, and flushes them
/// through [`AudioResampler::tick_repeat`] when the next group starts. That is
/// exactly the samples feeding every clock produced, and it only ever emits at
/// a group boundary, so two chips ticked together still emit on the same tick:
/// boards that pair their chips' samples tick by tick (SSIO, Konami) rely on
/// that. The cost is up to eight clocks of latency, about 5 us.
#[derive(Saveable)]
#[save_version(3)]
pub struct Ay8910 {
    registers: [u8; 16],
    address_latch: u8,

    // Tone generators (3 channels: A, B, C)
    tone_counters: [u16; 3],
    tone_outputs: [bool; 3],

    // Noise generator
    noise_counter: u16,
    noise_prescaler: bool,
    noise_lfsr: u32, // 17-bit LFSR

    // Envelope generator
    envelope_counter: u16,
    envelope_step: i16,
    envelope_attack: u8, // 0x00 or 0x0F (XOR mask for volume inversion)
    envelope_alternate: bool,
    envelope_hold: bool,
    envelope_holding: bool,
    envelope_volume: u8,

    // I/O ports (directly settable for arcade use)
    port_a_in: u8,
    port_b_in: u8,

    // Clock prescaler (chip_clock / 8)
    prescaler_count: u8,

    resampler: AudioResampler<i16>,

    // Per-channel gain for external volume modulation (0–255, 255 = full)
    channel_gain: [u8; 3],

    /// One resampler per channel, run instead of `resampler` once
    /// [`Self::enable_channel_outputs`] is called.
    channel_resamplers: [AudioResampler<i16>; 3],

    /// The current group's output so far, as segments of constant levels:
    /// `seg_levels[i]` held for `seg_lens[i]` clocks, the last one still
    /// open. A group is eight clocks, so eight segments always suffice.
    seg_levels: [[i32; 3]; 8],
    seg_lens: [u8; 8],
    seg_count: u8,

    /// Which of the two outputs this chip feeds. Configuration fixed by the
    /// board at construction, not state, so it is not saved.
    #[save_skip]
    channel_outputs: bool,
}

impl Ay8910 {
    /// The chip clock this AY was built with, which sets its tone pitch.
    ///
    /// Exposed so a board can be checked to be clocking the chip at the same
    /// rate it steps it at, rather than the two being separately written down.
    pub fn chip_clock_hz(&self) -> u64 {
        self.resampler.input_rate()
    }

    /// Create a new AY-8910 with the given chip clock rate.
    ///
    /// The internal generator rate is chip_clock / 8. For Midway SSIO,
    /// the chip clock is typically 2 MHz.
    pub fn new(chip_clock_hz: u64) -> Self {
        Self {
            registers: [0; 16],
            address_latch: 0,
            tone_counters: [0; 3],
            tone_outputs: [false; 3],
            noise_counter: 0,
            noise_prescaler: false,
            noise_lfsr: 1, // Must be non-zero for LFSR
            envelope_counter: 0,
            envelope_step: 0x0F,
            envelope_attack: 0,
            envelope_alternate: false,
            envelope_hold: false,
            envelope_holding: false,
            envelope_volume: 0x0F,
            port_a_in: 0,
            port_b_in: 0,
            prescaler_count: 0,
            resampler: AudioResampler::new(chip_clock_hz, host_sample_rate() as u64),
            channel_gain: [255; 3],
            channel_resamplers: std::array::from_fn(|_| {
                AudioResampler::new(chip_clock_hz, host_sample_rate() as u64)
            }),
            // One open, empty segment at silence: every volume is zero.
            seg_levels: [[0; 3]; 8],
            seg_lens: [0; 8],
            seg_count: 1,
            channel_outputs: false,
        }
    }

    /// Deliver the three channels separately, through
    /// [`Self::fill_channel_audio`], instead of their sum through
    /// [`Self::fill_audio`]. Call once, when the board is built.
    pub fn enable_channel_outputs(&mut self) {
        self.channel_outputs = true;
    }

    /// Latch a register address (0–15). Subsequent data_write/data_read
    /// operations target this register.
    pub fn address_write(&mut self, data: u8) {
        self.address_latch = data & 0x0F;
    }

    /// Write data to the currently latched register.
    pub fn data_write(&mut self, data: u8) {
        let r = self.address_latch as usize;

        // Apply register-specific bit masks
        let masked = match r {
            1 | 3 | 5 => data & 0x0F, // Tone high: 4 bits
            6 => data & 0x1F,         // Noise period: 5 bits
            8..=10 => data & 0x1F,    // Amplitude: 5 bits (bit 4 = envelope mode)
            13 => data & 0x0F,        // Envelope shape: 4 bits
            _ => data,
        };
        self.registers[r] = masked;

        // Writing R13 always resets the envelope generator
        if r == 13 {
            self.set_envelope_shape(masked);
        }
        // The output can change from the next clock on.
        self.open_segment();
    }

    /// Read from the currently latched register.
    pub fn data_read(&self) -> u8 {
        match self.address_latch {
            14 => {
                // Port A: return external input if direction = input (R7 bit 6 = 0)
                if self.registers[7] & 0x40 == 0 {
                    self.port_a_in
                } else {
                    self.registers[14]
                }
            }
            15 => {
                // Port B: return external input if direction = input (R7 bit 7 = 0)
                if self.registers[7] & 0x80 == 0 {
                    self.port_b_in
                } else {
                    self.registers[15]
                }
            }
            r => self.registers[r as usize],
        }
    }

    /// Advance the PSG by one chip clock cycle.
    ///
    /// Call at the chip clock rate (e.g. 2 MHz for SSIO). The internal
    /// tone/noise/envelope generators advance every 8th call.
    pub fn tick(&mut self) {
        self.prescaler_count += 1;
        if self.prescaler_count >= 8 {
            self.prescaler_count = 0;
            // The last group is complete: resample it, then start this one,
            // whose first clock already sees the generators' new state.
            self.flush_group();
            self.clock_generators();
            self.open_segment();
        }
        self.seg_lens[self.seg_count as usize - 1] += 1;
    }

    /// Start a segment at the levels the chip outputs now, or re-level the
    /// open one if it has not held for a clock yet.
    fn open_segment(&mut self) {
        let levels = self.channel_levels();
        let open = self.seg_count as usize - 1;
        if self.seg_lens[open] == 0 {
            self.seg_levels[open] = levels;
        } else if self.seg_levels[open] != levels {
            debug_assert!(open + 1 < self.seg_levels.len(), "a group is eight clocks");
            self.seg_levels[open + 1] = levels;
            self.seg_lens[open + 1] = 0;
            self.seg_count += 1;
        }
    }

    /// Resample the group's segments and leave one empty segment open.
    fn flush_group(&mut self) {
        for i in 0..self.seg_count as usize {
            let (levels, n) = (self.seg_levels[i], self.seg_lens[i] as u64);
            if self.channel_outputs {
                for (r, &level) in self.channel_resamplers.iter_mut().zip(&levels) {
                    r.tick_repeat(level as i16, n);
                }
            } else {
                self.resampler
                    .tick_repeat(levels.iter().sum::<i32>() as i16, n);
            }
        }
        self.seg_count = 1;
        self.seg_lens[0] = 0;
    }

    /// Set the external input value for I/O Port A.
    pub fn set_port_a(&mut self, data: u8) {
        self.port_a_in = data;
    }

    /// Set the external input value for I/O Port B.
    pub fn set_port_b(&mut self, data: u8) {
        self.port_b_in = data;
    }

    /// The currently latched register address (0–15). Lets a host board tell
    /// which register a `data_read`/`data_write` will target (e.g. to ack an
    /// IRQ when the command-latch port is read).
    pub fn latched_register(&self) -> u8 {
        self.address_latch
    }

    /// Read the current Port A output register value.
    pub fn port_a_read(&self) -> u8 {
        self.registers[14]
    }

    /// Read the current Port B output register value.
    pub fn port_b_read(&self) -> u8 {
        self.registers[15]
    }

    /// Set per-channel gain (0–255, 255 = full volume).
    ///
    /// Used by the SSIO sound board for duty-cycle volume modulation.
    pub fn set_channel_gain(&mut self, ch: usize, gain: u8) {
        if ch < 3 && self.channel_gain[ch] != gain {
            self.channel_gain[ch] = gain;
            self.open_segment();
        }
    }

    /// Drain accumulated audio samples, the three channels summed, into the
    /// provided buffer. Returns the number of samples written. Produces nothing
    /// once [`Self::enable_channel_outputs`] has been called.
    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.resampler.fill_audio(buffer)
    }

    /// Drain one channel's samples (0 = A, 1 = B, 2 = C) into `buffer`.
    /// Returns the number written, which is the same for all three since they
    /// share a clock. Produces nothing unless [`Self::enable_channel_outputs`]
    /// has been called.
    pub fn fill_channel_audio(&mut self, ch: usize, buffer: &mut [i16]) -> usize {
        self.channel_resamplers[ch].fill_audio(buffer)
    }

    /// Reset the PSG to initial state.
    pub fn reset(&mut self) {
        self.registers = [0; 16];
        self.address_latch = 0;
        self.tone_counters = [0; 3];
        self.tone_outputs = [false; 3];
        self.noise_counter = 0;
        self.noise_prescaler = false;
        self.noise_lfsr = 1;
        self.envelope_counter = 0;
        self.envelope_step = 0x0F;
        self.envelope_attack = 0;
        self.envelope_alternate = false;
        self.envelope_hold = false;
        self.envelope_holding = false;
        self.envelope_volume = 0x0F;
        self.port_a_in = 0;
        self.port_b_in = 0;
        self.prescaler_count = 0;
        self.resampler.reset();
        self.channel_gain = [255; 3];
        self.channel_resamplers
            .iter_mut()
            .for_each(AudioResampler::reset);
        // The pending group goes with the resamplers' state; the chip is silent.
        self.seg_levels = [[0; 3]; 8];
        self.seg_lens = [0; 8];
        self.seg_count = 1;
    }

    // -- Private methods -------------------------------------------------------

    /// Advance all generators by one internal clock tick (chip_clock / 8).
    fn clock_generators(&mut self) {
        // Tone generators: count up, toggle output on period expiry
        for ch in 0..3 {
            self.tone_counters[ch] += 1;
            let period = self.tone_period(ch).max(1);
            if self.tone_counters[ch] >= period {
                self.tone_counters[ch] = 0;
                self.tone_outputs[ch] = !self.tone_outputs[ch];
            }
        }

        // Noise generator: count up, prescale by 2, then advance LFSR
        self.noise_counter += 1;
        let noise_period = (self.registers[6] & 0x1F).max(1) as u16;
        if self.noise_counter >= noise_period {
            self.noise_counter = 0;
            self.noise_prescaler = !self.noise_prescaler;
            if self.noise_prescaler {
                // Advance 17-bit LFSR: XOR taps at bits 0 and 3
                let bit0 = self.noise_lfsr & 1;
                let bit3 = (self.noise_lfsr >> 3) & 1;
                self.noise_lfsr = (self.noise_lfsr >> 1) | ((bit0 ^ bit3) << 16);
            }
        }

        // Envelope generator: count up, step envelope on period expiry
        self.envelope_counter += 1;
        let env_period = self.envelope_period().max(1);
        if self.envelope_counter >= env_period {
            self.envelope_counter = 0;
            self.step_envelope();
        }
    }

    /// Each channel's output level.
    fn channel_levels(&self) -> [i32; 3] {
        let mixer = self.registers[7];
        let noise_out = (self.noise_lfsr & 1) != 0;

        let mut output = [0i32; 3];

        for (ch, out) in output.iter_mut().enumerate() {
            // R7 enable bits are active-low: 1 = disabled
            let tone_disable = (mixer >> ch) & 1 != 0;
            let noise_disable = (mixer >> (ch + 3)) & 1 != 0;

            // AY-8910 mixing: (tone_output | tone_disable) & (noise_output | noise_disable)
            let enabled = (self.tone_outputs[ch] || tone_disable) && (noise_out || noise_disable);

            if enabled {
                let amp_reg = self.registers[8 + ch];
                let volume = if amp_reg & 0x10 != 0 {
                    self.envelope_volume
                } else {
                    amp_reg & 0x0F
                };
                let level = VOLUME_TABLE[volume as usize];
                *out = (level * self.channel_gain[ch] as i32) / 255;
            }
        }

        output
    }

    /// Initialize envelope generator from shape register value.
    fn set_envelope_shape(&mut self, shape: u8) {
        self.envelope_attack = if shape & 0x04 != 0 { 0x0F } else { 0x00 };

        if shape & 0x08 == 0 {
            // Continue = 0: single cycle then hold
            self.envelope_hold = true;
            self.envelope_alternate = self.envelope_attack != 0;
        } else {
            self.envelope_hold = shape & 0x01 != 0;
            self.envelope_alternate = shape & 0x02 != 0;
        }

        self.envelope_step = 0x0F;
        self.envelope_holding = false;
        self.envelope_volume = (self.envelope_step as u8) ^ self.envelope_attack;
        self.envelope_counter = 0;
    }

    /// Advance envelope by one step (called when envelope counter expires).
    ///
    /// AY-8910 uses a step value of 2 (giving 8 distinct levels per cycle)
    /// with a 4-bit mask (0x0F). The envelope_attack XOR mask inverts the
    /// volume for attack-mode shapes.
    fn step_envelope(&mut self) {
        if self.envelope_holding {
            return;
        }

        self.envelope_step -= 2;
        if self.envelope_step < 0 {
            if self.envelope_hold {
                if self.envelope_alternate {
                    self.envelope_attack ^= 0x0F;
                }
                self.envelope_holding = true;
                self.envelope_step = 0;
            } else {
                if self.envelope_alternate && (self.envelope_step & 0x10) != 0 {
                    self.envelope_attack ^= 0x0F;
                }
                self.envelope_step &= 0x0F;
            }
        }

        self.envelope_volume = (self.envelope_step as u8) ^ self.envelope_attack;
    }

    /// Get the 12-bit tone period for channel ch (0=A, 1=B, 2=C).
    fn tone_period(&self, ch: usize) -> u16 {
        let lo = self.registers[ch * 2] as u16;
        let hi = (self.registers[ch * 2 + 1] & 0x0F) as u16;
        (hi << 8) | lo
    }

    /// Get the 16-bit envelope period.
    fn envelope_period(&self) -> u16 {
        let lo = self.registers[11] as u16;
        let hi = self.registers[12] as u16;
        (hi << 8) | lo
    }
}

impl super::Device for Ay8910 {
    fn name(&self) -> &'static str {
        "AY-8910"
    }
    fn reset(&mut self) {
        self.reset();
    }
    fn tick(&mut self) {
        self.tick();
    }
}

// -- Debug support -----------------------------------------------------------

use crate::core::debug::{DebugRegister, Debuggable};

impl Debuggable for Ay8910 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "ADDR",
                value: self.address_latch as u64,
                width: 8,
            },
            DebugRegister {
                name: "TONE_A",
                value: self.tone_period(0) as u64,
                width: 16,
            },
            DebugRegister {
                name: "TONE_B",
                value: self.tone_period(1) as u64,
                width: 16,
            },
            DebugRegister {
                name: "TONE_C",
                value: self.tone_period(2) as u64,
                width: 16,
            },
            DebugRegister {
                name: "NOISE",
                value: (self.registers[6] & 0x1F) as u64,
                width: 8,
            },
            DebugRegister {
                name: "MIXER",
                value: self.registers[7] as u64,
                width: 8,
            },
            DebugRegister {
                name: "AMP_A",
                value: self.registers[8] as u64,
                width: 8,
            },
            DebugRegister {
                name: "AMP_B",
                value: self.registers[9] as u64,
                width: 8,
            },
            DebugRegister {
                name: "AMP_C",
                value: self.registers[10] as u64,
                width: 8,
            },
            DebugRegister {
                name: "ENV_PER",
                value: self.envelope_period() as u64,
                width: 16,
            },
            DebugRegister {
                name: "ENV_SHAPE",
                value: self.registers[13] as u64,
                width: 8,
            },
            DebugRegister {
                name: "ENV_VOL",
                value: self.envelope_volume as u64,
                width: 8,
            },
        ]
    }
}

// Save state support: derived via #[derive(Saveable)] on the struct.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::save_state::{Saveable, StateReader, StateWriter};

    #[test]
    fn initial_state_is_silent() {
        let mut ay = Ay8910::new(2_000_000);
        // Run a few ticks to generate samples
        for _ in 0..1000 {
            ay.tick();
        }
        let mut buf = [0i16; 100];
        let n = ay.fill_audio(&mut buf);
        assert!(n > 0);
        // All volumes are 0, so output should be silent
        for &s in &buf[..n] {
            assert_eq!(s, 0);
        }
    }

    #[test]
    fn tone_produces_output() {
        let mut ay = Ay8910::new(2_000_000);
        // Set channel A: tone period = 100, volume = 15, tone enabled
        ay.address_write(0);
        ay.data_write(100); // R0 = tone period low
        ay.address_write(1);
        ay.data_write(0); // R1 = tone period high
        ay.address_write(7);
        ay.data_write(0b0011_1110); // Enable tone A only (bit 0 = 0)
        ay.address_write(8);
        ay.data_write(0x0F); // Volume = max

        // Run enough ticks to get samples
        for _ in 0..2000 {
            ay.tick();
        }

        let mut buf = [0i16; 100];
        let n = ay.fill_audio(&mut buf);
        assert!(n > 0);
        // Should have non-zero samples (square wave)
        let has_nonzero = buf[..n].iter().any(|&s| s != 0);
        assert!(has_nonzero, "tone output should produce non-zero samples");
    }

    #[test]
    fn noise_lfsr_advances() {
        let mut ay = Ay8910::new(2_000_000);
        let initial_lfsr = ay.noise_lfsr;

        // Enable noise on channel A with volume
        ay.address_write(6);
        ay.data_write(1); // Noise period = 1 (fastest)
        ay.address_write(7);
        ay.data_write(0b0011_0111); // Enable noise A (bit 3=0), disable tones
        ay.address_write(8);
        ay.data_write(0x0F);

        // Run enough ticks for the LFSR to advance
        for _ in 0..100 {
            ay.tick();
        }

        assert_ne!(ay.noise_lfsr, initial_lfsr, "LFSR should have advanced");
    }

    #[test]
    fn envelope_decay_shape() {
        let mut ay = Ay8910::new(2_000_000);
        // Set envelope period = 1 (fastest), shape = 0 (decay, hold at 0)
        ay.address_write(11);
        ay.data_write(1);
        ay.address_write(12);
        ay.data_write(0);
        ay.address_write(13);
        ay.data_write(0); // Shape 0: \___

        // Initial volume should be 15 (step=15, attack=0, volume = 15^0 = 15)
        assert_eq!(ay.envelope_volume, 15);

        // Run enough internal clocks to step through the envelope
        // Each internal clock = 8 chip clocks, envelope steps when counter reaches period
        for _ in 0..8 {
            ay.tick();
        }
        // After 1 internal clock, envelope should have stepped: 15 -> 13
        assert_eq!(ay.envelope_volume, 13);

        // Run more to complete the envelope cycle (8 steps to reach 0)
        for _ in 0..(7 * 8) {
            ay.tick();
        }
        // Should be holding at 0
        assert_eq!(ay.envelope_volume, 0);
        assert!(ay.envelope_holding);
    }

    #[test]
    fn channel_gain_modulates_output() {
        let mut ay = Ay8910::new(2_000_000);
        // Set up channel A with tone, max volume
        ay.address_write(0);
        ay.data_write(1); // Period = 1 (fast)
        ay.address_write(7);
        ay.data_write(0b0011_1110); // Enable tone A only
        ay.address_write(8);
        ay.data_write(0x0F); // Max volume

        // Run with full gain
        for _ in 0..2000 {
            ay.tick();
        }
        let mut buf_full = [0i16; 100];
        let n1 = ay.fill_audio(&mut buf_full);

        // Now set half gain and regenerate
        ay.set_channel_gain(0, 128);
        for _ in 0..2000 {
            ay.tick();
        }
        let mut buf_half = [0i16; 100];
        let n2 = ay.fill_audio(&mut buf_half);

        assert!(n1 > 0 && n2 > 0);

        // Max samples with half gain should be roughly half
        let max_full = buf_full[..n1]
            .iter()
            .map(|s| s.unsigned_abs())
            .max()
            .unwrap_or(0);
        let max_half = buf_half[..n2]
            .iter()
            .map(|s| s.unsigned_abs())
            .max()
            .unwrap_or(0);
        assert!(
            max_half < max_full,
            "half gain ({max_half}) should be less than full ({max_full})"
        );
    }

    /// Program tone B only, loud, on a chip in either mode, and run it.
    fn tone_b_only(channel_outputs: bool) -> Ay8910 {
        let mut ay = Ay8910::new(2_000_000);
        if channel_outputs {
            ay.enable_channel_outputs();
        }
        for (reg, value) in [(2, 50), (3, 0), (7, 0b0011_1101), (9, 0x0F)] {
            ay.address_write(reg);
            ay.data_write(value);
        }
        for _ in 0..20_000 {
            ay.tick();
        }
        ay
    }

    #[test]
    fn a_channel_appears_on_its_own_output_only() {
        let mut ay = tone_b_only(true);
        let mut bufs = [[0i16; 400]; 3];
        let counts: Vec<usize> = (0..3)
            .map(|ch| ay.fill_channel_audio(ch, &mut bufs[ch]))
            .collect();
        assert!(
            counts[0] > 0 && counts.iter().all(|&n| n == counts[0]),
            "{counts:?}"
        );
        assert!(bufs[0].iter().all(|&s| s == 0), "channel A is silent");
        assert!(bufs[2].iter().all(|&s| s == 0), "channel C is silent");
        assert!(
            bufs[1].iter().any(|&s| s != 0),
            "channel B carries the tone"
        );
        let mut mixed = [0i16; 400];
        assert_eq!(ay.fill_audio(&mut mixed), 0, "the summed output is not fed");
    }

    /// The three outputs add up to what the chip delivers summed, to within
    /// each resampler rounding once.
    #[test]
    fn the_channel_outputs_sum_to_the_mixed_output() {
        let program = |ay: &mut Ay8910| {
            for (reg, value) in [
                (0, 40),
                (2, 57),
                (4, 91),
                (7, 0b0011_1000),
                (8, 0x0F),
                (9, 0x0B),
                (10, 0x08),
            ] {
                ay.address_write(reg);
                ay.data_write(value);
            }
        };
        let mut mixed = Ay8910::new(2_000_000);
        let mut split = Ay8910::new(2_000_000);
        split.enable_channel_outputs();
        program(&mut mixed);
        program(&mut split);
        for _ in 0..40_000 {
            mixed.tick();
            split.tick();
        }
        let mut m = [0i16; 1000];
        let n = mixed.fill_audio(&mut m);
        let mut chans = [[0i16; 1000]; 3];
        for (ch, buf) in chans.iter_mut().enumerate() {
            assert_eq!(split.fill_channel_audio(ch, buf), n);
        }
        for i in 0..n {
            let sum: i32 = chans.iter().map(|c| c[i] as i32).sum();
            assert!(
                (sum - m[i] as i32).abs() <= 2,
                "sample {i}: channels sum to {sum}, mixed is {}",
                m[i]
            );
        }
        assert!(m[..n].iter().any(|&s| s != 0));
    }

    /// Resampling a group at a time is exactly what feeding the resampler every
    /// clock produced, with register writes and channel gains changing between
    /// any two clocks, in both output modes. The reference is a resampler fed
    /// the chip's own levels on every clock, which is what `tick` used to do.
    #[test]
    fn a_group_at_a_time_is_exactly_every_clock() {
        for channel_outputs in [false, true] {
            let mut ay = Ay8910::new(2_000_000);
            if channel_outputs {
                ay.enable_channel_outputs();
            }
            let rate = host_sample_rate() as u64;
            let mut reference: [AudioResampler<i16>; 4] =
                std::array::from_fn(|_| AudioResampler::new(2_000_000, rate));
            let mut x: u32 = 0x9E37_79B9;
            let mut next = || {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                x >> 8
            };
            for _ in 0..200_000 {
                // Now and then, between clocks, write a register or a gain.
                match next() % 64 {
                    0 => {
                        ay.address_write((next() % 14) as u8);
                        ay.data_write(next() as u8);
                    }
                    1 => ay.set_channel_gain((next() % 3) as usize, next() as u8),
                    _ => {}
                }
                ay.tick();
                let levels = ay.channel_levels();
                reference[3].tick(levels.iter().sum::<i32>() as i16);
                for (r, &l) in reference.iter_mut().zip(&levels) {
                    r.tick(l as i16);
                }
            }
            let outputs: Vec<(Vec<i16>, Vec<i16>)> = if channel_outputs {
                (0..3)
                    .map(|ch| {
                        let mut buf = vec![0i16; 20_000];
                        let n = ay.fill_channel_audio(ch, &mut buf);
                        buf.truncate(n);
                        (buf, reference[ch].drain_audio())
                    })
                    .collect()
            } else {
                let mut buf = vec![0i16; 20_000];
                let n = ay.fill_audio(&mut buf);
                buf.truncate(n);
                vec![(buf, reference[3].drain_audio())]
            };
            for (got, want) in outputs {
                // The chip may still hold its last group: at most one sample.
                assert!(
                    want.len() - got.len() <= 1,
                    "{} of {}",
                    got.len(),
                    want.len()
                );
                assert!(got.iter().any(|&s| s != 0), "the walk made some sound");
                assert_eq!(got, want[..got.len()], "channel outputs {channel_outputs}");
            }
        }
    }

    #[test]
    fn a_mixed_chip_feeds_no_channel_output() {
        let mut ay = tone_b_only(false);
        let mut buf = [0i16; 400];
        assert_eq!(ay.fill_channel_audio(1, &mut buf), 0);
        assert!(ay.fill_audio(&mut buf) > 0);
    }

    #[test]
    fn register_read_write_round_trip() {
        let mut ay = Ay8910::new(2_000_000);

        // Write and read back tone period
        ay.address_write(0);
        ay.data_write(0xAB);
        ay.address_write(0);
        assert_eq!(ay.data_read(), 0xAB);

        // High nibble should be masked to 4 bits
        ay.address_write(1);
        ay.data_write(0xFF);
        ay.address_write(1);
        assert_eq!(ay.data_read(), 0x0F);

        // Noise period masked to 5 bits
        ay.address_write(6);
        ay.data_write(0xFF);
        ay.address_write(6);
        assert_eq!(ay.data_read(), 0x1F);
    }

    #[test]
    fn port_a_reads_external_input() {
        let mut ay = Ay8910::new(2_000_000);
        ay.set_port_a(0x42);

        // R7 bit 6 = 0 means port A is input (default)
        ay.address_write(14);
        assert_eq!(ay.data_read(), 0x42);

        // Set port A to output mode
        ay.address_write(7);
        ay.data_write(0x40); // Bit 6 = 1
        ay.address_write(14);
        ay.data_write(0x99);
        ay.address_write(14);
        assert_eq!(ay.data_read(), 0x99); // Returns register value, not input
    }

    #[test]
    fn save_load_round_trip() {
        let mut ay = Ay8910::new(2_000_000);

        // Set some state
        ay.address_write(0);
        ay.data_write(0x50);
        ay.address_write(8);
        ay.data_write(0x0A);
        for _ in 0..500 {
            ay.tick();
        }

        // Save
        let mut w = StateWriter::new();
        ay.save_state(&mut w);
        let data = w.into_vec();

        // Create fresh instance and load
        let mut ay2 = Ay8910::new(2_000_000);
        let mut r = StateReader::new(&data);
        ay2.load_state(&mut r).unwrap();

        // Verify key fields match
        assert_eq!(ay2.registers, ay.registers);
        assert_eq!(ay2.tone_counters, ay.tone_counters);
        assert_eq!(ay2.noise_lfsr, ay.noise_lfsr);
        assert_eq!(ay2.envelope_volume, ay.envelope_volume);
        assert_eq!(ay2.channel_gain, [255; 3]);
    }
}
