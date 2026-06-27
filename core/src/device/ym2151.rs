//! Yamaha YM2151 (OPM) 8-voice FM synthesiser — **timer/IRQ stub**.
//!
//! This models only the parts a sound program needs to *run*: the address/data
//! register port, the readable status register, and the two interval timers
//! (A and B) that drive the chip's IRQ line. Many Atari sound CPUs sit in an
//! IRQ-driven main loop clocked by timer A, so without working timers the sound
//! program never services its command queue. The FM voices themselves are not
//! synthesised — [`Ym2151::drain_audio`] returns silence — but register writes
//! are accepted and stored so a real FM core can drop in behind this same API.
//!
//! Timer A is a 10-bit down-counter that overflows every `64·(1024−A)` chip
//! clocks; timer B is 8-bit and overflows every `1024·(256−B)`. Each overflow
//! sets its status flag and, when the matching IRQ-enable bit in the control
//! register is set, asserts the IRQ line until the flag is reset.

use crate::core::save_state::{SaveError, Saveable, StateReader, StateWriter};

// Control register (0x14) bit assignments.
const CTRL_LOAD_A: u8 = 0x01;
const CTRL_LOAD_B: u8 = 0x02;
const CTRL_IRQEN_A: u8 = 0x04;
const CTRL_IRQEN_B: u8 = 0x08;
const CTRL_RESET_A: u8 = 0x10;
const CTRL_RESET_B: u8 = 0x20;

const STATUS_TIMER_A: u8 = 0x01;
const STATUS_TIMER_B: u8 = 0x02;

const REG_TIMER_A_HI: usize = 0x10; // bits 9-2 of timer A
const REG_TIMER_A_LO: usize = 0x11; // bits 1-0 of timer A
const REG_TIMER_B: usize = 0x12;
const REG_CONTROL: usize = 0x14;

/// YM2151 register/timer/IRQ stub (no FM synthesis).
pub struct Ym2151 {
    regs: [u8; 256],
    /// Latched register address (written via the address port).
    address: u8,
    /// Timer A / B down-counters, in chip clocks.
    timer_a: u32,
    timer_b: u32,
    /// Overflow flags (bit 0 = timer A, bit 1 = timer B).
    status: u8,
}

impl Ym2151 {
    pub fn new() -> Self {
        Self {
            regs: [0; 256],
            address: 0,
            timer_a: 0,
            timer_b: 0,
            status: 0,
        }
    }

    pub fn reset(&mut self) {
        self.regs = [0; 256];
        self.address = 0;
        self.timer_a = 0;
        self.timer_b = 0;
        self.status = 0;
    }

    fn timer_a_period(&self) -> u32 {
        let ta = ((self.regs[REG_TIMER_A_HI] as u32) << 2) | (self.regs[REG_TIMER_A_LO] as u32 & 3);
        64 * (1024 - ta)
    }

    fn timer_b_period(&self) -> u32 {
        1024 * (256 - self.regs[REG_TIMER_B] as u32)
    }

    /// Read the status register. Both port addresses return it; the BUSY bit
    /// (bit 7) is always clear here since no FM write actually takes time.
    pub fn read(&self, _offset: u16) -> u8 {
        self.status
    }

    /// Write the address port (even offset) or the data port (odd offset).
    pub fn write(&mut self, offset: u16, data: u8) {
        if offset & 1 == 0 {
            self.address = data;
        } else {
            self.write_reg(self.address, data);
        }
    }

    fn write_reg(&mut self, reg: u8, data: u8) {
        let prev = self.regs[reg as usize];
        self.regs[reg as usize] = data;
        if reg as usize == REG_CONTROL {
            // Reset bits clear the overflow flags (and so drop the IRQ).
            if data & CTRL_RESET_A != 0 {
                self.status &= !STATUS_TIMER_A;
            }
            if data & CTRL_RESET_B != 0 {
                self.status &= !STATUS_TIMER_B;
            }
            // A rising LOAD edge (re)loads the counter from the period registers.
            if data & CTRL_LOAD_A != 0 && prev & CTRL_LOAD_A == 0 {
                self.timer_a = self.timer_a_period();
            }
            if data & CTRL_LOAD_B != 0 && prev & CTRL_LOAD_B == 0 {
                self.timer_b = self.timer_b_period();
            }
        }
    }

    /// Advance the running timers by `cycles` chip clocks, raising the overflow
    /// flags (and the IRQ, via [`Self::irq`]) as they expire and reload.
    pub fn tick(&mut self, cycles: u32) {
        let ctrl = self.regs[REG_CONTROL];
        if ctrl & CTRL_LOAD_A != 0 {
            if self.timer_a <= cycles {
                self.status |= STATUS_TIMER_A;
                self.timer_a = self.timer_a_period();
            } else {
                self.timer_a -= cycles;
            }
        }
        if ctrl & CTRL_LOAD_B != 0 {
            if self.timer_b <= cycles {
                self.status |= STATUS_TIMER_B;
                self.timer_b = self.timer_b_period();
            } else {
                self.timer_b -= cycles;
            }
        }
    }

    /// The IRQ line: asserted while a timer's flag is set and its IRQ-enable bit
    /// in the control register is on.
    pub fn irq(&self) -> bool {
        let ctrl = self.regs[REG_CONTROL];
        (self.status & STATUS_TIMER_A != 0 && ctrl & CTRL_IRQEN_A != 0)
            || (self.status & STATUS_TIMER_B != 0 && ctrl & CTRL_IRQEN_B != 0)
    }

    /// FM audio output — silent until a real synthesis core replaces this stub.
    pub fn drain_audio(&mut self) -> Vec<f32> {
        Vec::new()
    }
}

impl Default for Ym2151 {
    fn default() -> Self {
        Self::new()
    }
}

impl Saveable for Ym2151 {
    fn save_state(&self, w: &mut StateWriter) {
        w.write_bytes(&self.regs);
        w.write_u8(self.address);
        w.write_u32_le(self.timer_a);
        w.write_u32_le(self.timer_b);
        w.write_u8(self.status);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        r.read_bytes_into(&mut self.regs)?;
        self.address = r.read_u8()?;
        self.timer_a = r.read_u32_le()?;
        self.timer_b = r.read_u32_le()?;
        self.status = r.read_u8()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Write a register through the address/data ports.
    fn poke(ym: &mut Ym2151, reg: u8, data: u8) {
        ym.write(0, reg); // address port
        ym.write(1, data); // data port
    }

    #[test]
    fn status_starts_clear_and_not_busy() {
        let ym = Ym2151::new();
        assert_eq!(ym.read(0), 0);
        assert!(!ym.irq());
    }

    #[test]
    fn timer_a_overflow_sets_flag_and_irq() {
        let mut ym = Ym2151::new();
        // Timer A = 1023 → period 64. Enable load + IRQ.
        poke(&mut ym, 0x10, 0xFF); // bits 9-2 = 0xFF
        poke(&mut ym, 0x11, 0x03); // bits 1-0 = 3 → A = 1023
        poke(&mut ym, 0x14, CTRL_LOAD_A | CTRL_IRQEN_A);
        assert!(!ym.irq(), "no overflow yet");

        ym.tick(64);
        assert_eq!(ym.read(0) & STATUS_TIMER_A, STATUS_TIMER_A, "flag set");
        assert!(ym.irq(), "IRQ asserted while flag + enable");

        // Reset the flag → IRQ drops.
        poke(&mut ym, 0x14, CTRL_LOAD_A | CTRL_IRQEN_A | CTRL_RESET_A);
        assert!(!ym.irq(), "reset clears the flag and IRQ");
    }

    #[test]
    fn timer_a_irq_masked_without_enable() {
        let mut ym = Ym2151::new();
        poke(&mut ym, 0x10, 0xFF);
        poke(&mut ym, 0x11, 0x03);
        poke(&mut ym, 0x14, CTRL_LOAD_A); // load but no IRQ enable
        ym.tick(64);
        assert_eq!(
            ym.read(0) & STATUS_TIMER_A,
            STATUS_TIMER_A,
            "flag still set"
        );
        assert!(!ym.irq(), "IRQ masked without the enable bit");
    }

    #[test]
    fn timer_reloads_and_fires_periodically() {
        let mut ym = Ym2151::new();
        poke(&mut ym, 0x10, 0xFF);
        poke(&mut ym, 0x11, 0x03); // period 64
        poke(&mut ym, 0x14, CTRL_LOAD_A | CTRL_IRQEN_A);

        ym.tick(64);
        assert!(ym.irq());
        poke(&mut ym, 0x14, CTRL_LOAD_A | CTRL_IRQEN_A | CTRL_RESET_A); // ack
        assert!(!ym.irq());
        // Re-arm (clear the reset bit) and it fires again after another period.
        poke(&mut ym, 0x14, CTRL_LOAD_A | CTRL_IRQEN_A);
        ym.tick(64);
        assert!(ym.irq(), "timer reloaded and fired again");
    }

    #[test]
    fn drain_audio_is_silent() {
        let mut ym = Ym2151::new();
        assert!(ym.drain_audio().is_empty());
    }

    #[test]
    fn save_load_round_trips() {
        let mut ym = Ym2151::new();
        poke(&mut ym, 0x10, 0x55);
        poke(&mut ym, 0x14, CTRL_LOAD_A | CTRL_IRQEN_A);
        ym.tick(10);

        let mut w = StateWriter::new();
        ym.save_state(&mut w);
        let bytes = w.into_vec();

        let mut ym2 = Ym2151::new();
        let mut r = StateReader::new(&bytes);
        ym2.load_state(&mut r).unwrap();
        assert_eq!(ym2.regs[0x10], 0x55);
        assert_eq!(ym2.regs[0x14], CTRL_LOAD_A | CTRL_IRQEN_A);
        assert_eq!(ym2.timer_a, ym.timer_a);
    }
}
