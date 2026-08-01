//! Texas Instruments SN76489A programmable sound generator (PSG).
//!
//! Three square-wave tone channels plus one noise channel, each with a 4-bit
//! logarithmic attenuator (2 dB/step, 0 = loudest, 15 = silent). Modelled on
//! MAME's `sound/sn76496.cpp` (the SN76489A variant — 15-bit LFSR with noise
//! taps at 0x04/0x08 and feedback into 0x10000, `/8` internal clock divider).
//!
//! ## Clocking
//! The chip's tone/noise counters run at `chip_clock / 16`. To keep this device
//! tickable from a slower main-CPU loop (Congo's SN76489As run at 4 MHz and
//! 1 MHz against a ~3 MHz Z80), [`tick`](Sn76489a::tick) advances the generators
//! by **one counter step**; the owning board drives it at
//! [`generator_clock_hz`](Sn76489a::generator_clock_hz) and resamples
//! [`output`](Sn76489a::output) to the audio rate.
//!
//! ## Register interface
//! A single write port. A byte with bit 7 set is a *latch/data* byte (bits 6-5
//! select the channel, bit 4 selects volume vs. tone, bits 3-0 are data); a byte
//! with bit 7 clear is a *data* byte supplying the upper 6 frequency bits of the
//! last-latched tone register.

use phosphor_macros::Saveable;

use crate::core::debug::{DebugRegister, Debuggable};

// SN76489A noise LFSR configuration. MAME implements the (hardware) shift-left
// register as a shift-right with the taps and feedback bit reversed.
const FEEDBACK_MASK: u32 = 0x1_0000;
const NOISE_TAP1: u32 = 0x04;
const NOISE_TAP2: u32 = 0x08;

/// Counter clock = chip clock / 16 (the `/8` stream divider applied to a `/2`
/// internal clock). Tone frequency works out to `chip_clock / (32 * period)`.
const GENERATOR_DIVISOR: u32 = 16;

/// A write pulls the chip's `READY` output low while the byte is transferred
/// into the register file, for 32 chip-clock cycles. Boards that wire `READY`
/// to their CPU's `WAIT` input stall the CPU for that long after each write.
const READY_BUSY_CLOCKS: u16 = 32;

/// SN76489A programmable sound generator.
#[derive(Saveable)]
pub struct Sn76489a {
    /// Chip input clock (Hz). Config only — see [`Sn76489a::generator_clock_hz`].
    #[save_skip]
    clock_hz: u32,

    /// Raw register file: 0/2/4 = 10-bit tone period, 1/3/5/7 = 4-bit volume,
    /// 6 = 3-bit noise control.
    registers: [u16; 8],
    /// Index (0-7) of the register a bare data byte updates.
    last_register: u8,

    /// Per-channel counter reload value and live down-counter.
    period: [u16; 4],
    count: [i16; 4],
    /// Per-channel square-wave output bit (0/1).
    output_bit: [u8; 4],
    /// Per-channel attenuator output level (from the volume table).
    volume: [i16; 4],

    /// Noise linear-feedback shift register.
    rng: u32,

    /// Chip clocks remaining before `READY` returns high after a write. Not
    /// saved: it spans at most 32 chip clocks, so a restore that lands inside
    /// the window simply comes back ready.
    #[save_skip(default)]
    ready_countdown: u16,

    /// Precomputed 2 dB/step attenuation table (index 15 = silence). Config only.
    #[save_skip]
    vol_table: [i16; 16],
}

impl Sn76489a {
    /// Create an SN76489A with the given chip input clock (e.g. 4_000_000).
    pub fn new(clock_hz: u32) -> Self {
        // Volume table: max output split across four channels, 2 dB per step.
        let max_per_channel = (0x7fff / 4) as f64;
        let mut vol_table = [0i16; 16];
        let mut out = max_per_channel;
        for v in vol_table.iter_mut().take(15) {
            *v = out.min(max_per_channel) as i16;
            out /= 1.258_925_412; // 10^(2/20) = 2 dB
        }
        vol_table[15] = 0;

        let mut chip = Self {
            clock_hz,
            registers: [0; 8],
            last_register: 0,
            period: [0; 4],
            count: [0; 4],
            output_bit: [0; 4],
            volume: [0; 4],
            rng: FEEDBACK_MASK,
            ready_countdown: 0,
            vol_table,
        };
        chip.reset_state();
        chip
    }

    /// Rate at which the board should call [`tick`](Self::tick): `chip_clock / 16`.
    pub fn generator_clock_hz(&self) -> u32 {
        self.clock_hz / GENERATOR_DIVISOR
    }

    fn reset_state(&mut self) {
        self.registers = [0; 8];
        self.last_register = 0;
        for i in 0..4 {
            self.output_bit[i] = 0;
            self.count[i] = 0;
            // Tone periods power on at max; the noise period at 0. Volume
            // registers power on at 0, i.e. full volume (the SN76489A "pop").
            self.period[i] = if i == 3 { 0 } else { 0x400 };
            self.volume[i] = self.vol_table[0];
        }
        self.rng = FEEDBACK_MASK;
        self.output_bit[3] = (self.rng & 1) as u8;
        self.ready_countdown = 0;
    }

    /// Reset to power-on state (configuration preserved).
    pub fn reset(&mut self) {
        self.reset_state();
    }

    /// Is `READY` high, i.e. is the chip willing to accept another write?
    ///
    /// Boards that tie `READY` to a CPU `WAIT` input stall that CPU while this
    /// is false. Boards that ignore `READY` can ignore this entirely.
    pub fn is_ready(&self) -> bool {
        self.ready_countdown == 0
    }

    /// Advance the `READY` busy countdown by one chip clock.
    ///
    /// Separate from [`tick`](Self::tick), which runs at `chip_clock / 16`:
    /// the busy window is short enough that it needs full chip-clock
    /// resolution.
    pub fn tick_ready(&mut self) {
        self.ready_countdown = self.ready_countdown.saturating_sub(1);
    }

    /// Write a byte to the chip's single data port.
    pub fn write(&mut self, data: u8) {
        self.ready_countdown = READY_BUSY_CLOCKS;

        let r = if data & 0x80 != 0 {
            // Latch/data byte: select register and load its low 4 bits.
            let r = ((data >> 4) & 0x07) as usize;
            self.last_register = r as u8;
            self.registers[r] = (self.registers[r] & 0x3f0) | (data & 0x0f) as u16;
            r
        } else {
            self.last_register as usize
        };

        let c = r >> 1; // channel for this register pair
        match r {
            // Tone frequency (low nibble already loaded above for a latch byte;
            // a data byte supplies the upper 6 bits).
            0 | 2 | 4 => {
                if data & 0x80 == 0 {
                    self.registers[r] = (self.registers[r] & 0x0f) | (((data & 0x3f) as u16) << 4);
                }
                self.period[c] = if self.registers[r] != 0 {
                    self.registers[r]
                } else {
                    0x400
                };
                // Noise tracks tone 2 when in "tone 3 output" mode.
                if r == 4 && (self.registers[6] & 0x03) == 0x03 {
                    self.period[3] = self.period[2] << 1;
                }
            }
            // Volume / attenuation.
            1 | 3 | 5 | 7 => {
                self.volume[c] = self.vol_table[(data & 0x0f) as usize];
                if data & 0x80 == 0 {
                    self.registers[r] = (self.registers[r] & 0x3f0) | (data & 0x0f) as u16;
                }
            }
            // Noise control: shift rate (bits 0-1) and white/periodic (bit 2).
            6 => {
                if data & 0x80 == 0 {
                    self.registers[r] = (self.registers[r] & 0x3f0) | (data & 0x0f) as u16;
                }
                let n = self.registers[6];
                self.period[3] = if (n & 3) == 3 {
                    self.period[2] << 1
                } else {
                    1 << (5 + (n & 3))
                };
                self.rng = FEEDBACK_MASK; // any noise-control write resets the LFSR
            }
            _ => unreachable!(),
        }
    }

    /// Advance the tone and noise counters by one step (`chip_clock / 16`).
    pub fn tick(&mut self) {
        for i in 0..3 {
            self.count[i] -= 1;
            if self.count[i] <= 0 {
                self.output_bit[i] ^= 1;
                self.count[i] = self.period[i] as i16;
            }
        }

        self.count[3] -= 1;
        if self.count[3] <= 0 {
            // White noise (mode bit set) XORs both taps; periodic noise uses tap1
            // alone, giving a single circulating bit.
            let noise_mode = self.registers[6] & 0x04 != 0;
            let tap1 = self.rng & NOISE_TAP1 != 0;
            let tap2 = self.rng & NOISE_TAP2 != 0;
            let feedback = tap1 != (tap2 && noise_mode);
            self.rng >>= 1;
            if feedback {
                self.rng |= FEEDBACK_MASK;
            }
            self.output_bit[3] = (self.rng & 1) as u8;
            self.count[3] = self.period[3] as i16;
        }
    }

    /// Current mixed output of all four channels as a signed 16-bit sample.
    pub fn output(&self) -> i16 {
        let mut sum = 0i32;
        for i in 0..4 {
            if self.output_bit[i] != 0 {
                sum += self.volume[i] as i32;
            }
        }
        sum as i16
    }
}

impl super::Device for Sn76489a {
    fn name(&self) -> &'static str {
        "SN76489A"
    }
    fn reset(&mut self) {
        self.reset();
    }
    fn write(&mut self, _offset: u16, data: u8) {
        self.write(data);
    }
    fn tick(&mut self) {
        self.tick();
    }
}

impl Debuggable for Sn76489a {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "TONE0",
                value: self.period[0] as u64,
                width: 16,
            },
            DebugRegister {
                name: "TONE1",
                value: self.period[1] as u64,
                width: 16,
            },
            DebugRegister {
                name: "TONE2",
                value: self.period[2] as u64,
                width: 16,
            },
            DebugRegister {
                name: "NOISE",
                value: (self.registers[6] & 0x0f) as u64,
                width: 8,
            },
            DebugRegister {
                name: "VOL0",
                value: (self.registers[1] & 0x0f) as u64,
                width: 8,
            },
            DebugRegister {
                name: "VOL1",
                value: (self.registers[3] & 0x0f) as u64,
                width: 8,
            },
            DebugRegister {
                name: "VOL2",
                value: (self.registers[5] & 0x0f) as u64,
                width: 8,
            },
            DebugRegister {
                name: "VOL3",
                value: (self.registers[7] & 0x0f) as u64,
                width: 8,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::save_state::{Saveable, StateReader, StateWriter};

    /// Latch tone channel `c`'s 10-bit period and silence its volume's siblings.
    fn set_tone(chip: &mut Sn76489a, c: u8, period: u16) {
        let r = c << 1;
        chip.write(0x80 | (r << 4) | (period & 0x0f) as u8); // latch: low nibble
        chip.write(((period >> 4) & 0x3f) as u8); // data: high 6 bits
    }

    fn set_volume(chip: &mut Sn76489a, c: u8, atten: u8) {
        let r = (c << 1) | 1;
        chip.write(0x80 | (r << 4) | (atten & 0x0f));
    }

    fn silence_all(chip: &mut Sn76489a) {
        for c in 0..4 {
            set_volume(chip, c, 0x0f);
        }
    }

    #[test]
    fn tone_period_latch_and_data_bytes() {
        let mut chip = Sn76489a::new(4_000_000);
        // Low nibble = 0x5, high 6 bits = 0x2A → period 0x2A5.
        chip.write(0x80 | 0x05); // tone 0 latch, low nibble 5
        chip.write(0x2a); // data byte, high 6 bits
        assert_eq!(chip.period[0], 0x2a5);
        // A zero period maps to 0x400 (the longest period).
        set_tone(&mut chip, 1, 0);
        assert_eq!(chip.period[1], 0x400);
    }

    #[test]
    fn volume_table_is_monotonic_and_silent_at_15() {
        let chip = Sn76489a::new(4_000_000);
        assert_eq!(chip.vol_table[15], 0);
        assert!(chip.vol_table[0] > chip.vol_table[1]);
        assert!(chip.vol_table[1] > chip.vol_table[7]);
        // Four channels at max must not overflow i16.
        assert!((chip.vol_table[0] as i32) * 4 <= i16::MAX as i32);
    }

    #[test]
    fn tone_output_toggles_at_period_rate() {
        let mut chip = Sn76489a::new(4_000_000);
        silence_all(&mut chip);
        set_tone(&mut chip, 0, 3);
        set_volume(&mut chip, 0, 0); // loudest
        let v = chip.vol_table[0];

        // From count 0: first toggle on the next tick, then every `period` ticks.
        chip.tick();
        assert_eq!(chip.output(), v, "toggled high");
        for _ in 0..3 {
            chip.tick();
        }
        assert_eq!(chip.output(), 0, "toggled low after one period");
    }

    #[test]
    fn noise_lfsr_advances_and_produces_transitions() {
        let mut chip = Sn76489a::new(4_000_000);
        silence_all(&mut chip);
        set_volume(&mut chip, 3, 0); // noise channel loudest
        chip.write(0x80 | (6 << 4) | 0x04); // white noise, fastest shift (n=0 → period 32)

        let start = chip.rng;
        let mut seen_high = false;
        let mut seen_low = false;
        for _ in 0..4000 {
            chip.tick();
            if chip.output_bit[3] != 0 {
                seen_high = true;
            } else {
                seen_low = true;
            }
        }
        assert_ne!(chip.rng, start, "LFSR advanced");
        assert!(seen_high && seen_low, "noise output transitions");
    }

    #[test]
    fn output_mixes_active_channels() {
        let mut chip = Sn76489a::new(4_000_000);
        silence_all(&mut chip);
        // Force two channels high at max volume; the other two silent.
        chip.output_bit = [1, 1, 0, 0];
        chip.volume = [chip.vol_table[0], chip.vol_table[0], chip.vol_table[0], 0];
        assert_eq!(chip.output(), chip.vol_table[0] * 2);
    }

    #[test]
    fn generator_clock_is_chip_clock_over_16() {
        assert_eq!(Sn76489a::new(4_000_000).generator_clock_hz(), 250_000);
        assert_eq!(Sn76489a::new(1_000_000).generator_clock_hz(), 62_500);
    }

    #[test]
    fn ready_goes_low_for_32_chip_clocks_after_a_write() {
        let mut chip = Sn76489a::new(4_000_000);
        assert!(chip.is_ready(), "idle chip is ready");

        chip.write(0x80 | 0x05);
        assert!(!chip.is_ready(), "a write pulls READY low");
        for _ in 0..31 {
            chip.tick_ready();
            assert!(!chip.is_ready());
        }
        chip.tick_ready();
        assert!(chip.is_ready(), "READY returns high after 32 chip clocks");

        // Further clocking must not underflow back into the busy state.
        chip.tick_ready();
        assert!(chip.is_ready());
    }

    #[test]
    fn reset_restores_power_on_state() {
        let mut chip = Sn76489a::new(4_000_000);
        set_tone(&mut chip, 0, 0x123);
        set_volume(&mut chip, 0, 0x0f);
        chip.tick();
        chip.reset();
        assert_eq!(chip.period[0], 0x400);
        assert_eq!(chip.registers, [0; 8]);
        assert_eq!(chip.volume[0], chip.vol_table[0]);
        assert_eq!(chip.rng, FEEDBACK_MASK);
    }

    #[test]
    fn save_load_round_trip() {
        let mut chip = Sn76489a::new(4_000_000);
        set_tone(&mut chip, 1, 0x0aa);
        set_volume(&mut chip, 2, 0x03);
        chip.write(0x80 | (6 << 4) | 0x05); // noise control
        for _ in 0..100 {
            chip.tick();
        }

        let mut w = StateWriter::new();
        chip.save_state(&mut w);
        let bytes = w.into_vec();

        let mut restored = Sn76489a::new(4_000_000);
        let mut r = StateReader::new(&bytes);
        restored.load_state(&mut r).unwrap();

        assert_eq!(restored.period, chip.period);
        assert_eq!(restored.registers, chip.registers);
        assert_eq!(restored.count, chip.count);
        assert_eq!(restored.rng, chip.rng);
        assert_eq!(restored.output(), chip.output());
    }
}
