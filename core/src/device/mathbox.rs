//! Atari Mathbox coprocessor
//!
//! A hardware math coprocessor used in Battlezone, Red Baron, and Tempest
//! for 2D rotation (matrix multiply), division, and distance approximation.
//!
//! The CPU writes a data byte to one of 32 command registers ($00–$1F).
//! Each write triggers an immediate computation; the result is available
//! instantly via `lo_r()` / `hi_r()` reads, and `status_r()` always
//! returns 0 (done).
//!
//! # Reference
//!
//! - <https://6502disassembly.com/va-battlezone/mathbox.html>
//! - <https://github.com/historicalsource/battlezone/blob/main/MBUDOC.DOC>

use crate::core::debug::{DebugRegister, Debuggable};
use phosphor_macros::Saveable;

/// Atari Mathbox coprocessor.
#[derive(Saveable)]
#[save_version(1)]
pub struct Mathbox {
    /// 16 signed 16-bit working registers.
    reg: [i16; 16],
    /// Result of the last computation (signed 16-bit).
    result: i16,
}

impl Mathbox {
    pub fn new() -> Self {
        Self {
            reg: [0; 16],
            result: 0,
        }
    }

    /// Status read — always returns 0 (instantaneous completion).
    pub fn status_r(&self) -> u8 {
        0x00
    }

    /// Read result low byte.
    pub fn lo_r(&self) -> u8 {
        (self.result & 0xFF) as u8
    }

    /// Read result high byte.
    pub fn hi_r(&self) -> u8 {
        ((self.result >> 8) & 0xFF) as u8
    }

    /// Write to a command register, triggering computation.
    ///
    /// `offset` is the 5-bit register address (0x00–0x1F).
    /// `data` is the 8-bit value written by the CPU.
    #[allow(clippy::identity_op)]
    pub fn go_w(&mut self, offset: u8, data: u8) {
        let d = data as i16;
        match offset & 0x1F {
            0x00 => {
                self.reg[0] = (self.reg[0] & !0xFF) | (d & 0xFF);
                self.result = self.reg[0];
            }
            0x01 => {
                self.reg[0] = (self.reg[0] & 0xFF) | (d << 8);
                self.result = self.reg[0];
            }
            0x02 => {
                self.reg[1] = (self.reg[1] & !0xFF) | (d & 0xFF);
                self.result = self.reg[1];
            }
            0x03 => {
                self.reg[1] = (self.reg[1] & 0xFF) | (d << 8);
                self.result = self.reg[1];
            }
            0x04 => {
                self.reg[2] = (self.reg[2] & !0xFF) | (d & 0xFF);
                self.result = self.reg[2];
            }
            0x05 => {
                self.reg[2] = (self.reg[2] & 0xFF) | (d << 8);
                self.result = self.reg[2];
            }
            0x06 => {
                self.reg[3] = (self.reg[3] & !0xFF) | (d & 0xFF);
                self.result = self.reg[3];
            }
            0x07 => {
                self.reg[3] = (self.reg[3] & 0xFF) | (d << 8);
                self.result = self.reg[3];
            }
            0x08 => {
                self.reg[4] = (self.reg[4] & !0xFF) | (d & 0xFF);
                self.result = self.reg[4];
            }
            0x09 => {
                self.reg[4] = (self.reg[4] & 0xFF) | (d << 8);
                self.result = self.reg[4];
            }
            0x0A => {
                self.reg[5] = (self.reg[5] & !0xFF) | (d & 0xFF);
                self.result = self.reg[5];
            }
            0x0B => {
                self.reg[5] = (self.reg[5] & 0xFF) | (d << 8);
                self.do_rotation(true);
            }
            0x0C => {
                self.reg[6] = d;
                self.result = self.reg[6];
            }
            0x0D => {
                self.reg[0xA] = (self.reg[0xA] & !0xFF) | (d & 0xFF);
                self.result = self.reg[0xA];
            }
            0x0E => {
                self.reg[0xA] = (self.reg[0xA] & 0xFF) | (d << 8);
                self.result = self.reg[0xA];
            }
            0x0F => {
                self.reg[0xB] = (self.reg[0xB] & !0xFF) | (d & 0xFF);
                self.result = self.reg[0xB];
            }
            0x10 => {
                self.reg[0xB] = (self.reg[0xB] & 0xFF) | (d << 8);
                self.result = self.reg[0xB];
            }
            0x11 => {
                self.reg[5] = (self.reg[5] & 0xFF) | (d << 8);
                self.do_rotation(false);
            }
            0x12 => self.do_rotation_part2(),
            0x13 => {
                let (r9, r8) = (self.reg[9], self.reg[8]);
                self.do_division(r9, r8);
            }
            0x14 => {
                let (ra, rb) = (self.reg[0xA], self.reg[0xB]);
                self.do_division(ra, rb);
            }
            0x15 => {
                self.reg[7] = (self.reg[7] & !0xFF) | (d & 0xFF);
                self.result = self.reg[7];
            }
            0x16 => {
                self.reg[7] = (self.reg[7] & 0xFF) | (d << 8);
                self.result = self.reg[7];
            }
            0x17 => self.result = self.reg[7],
            0x18 => self.result = self.reg[9],
            0x19 => self.result = self.reg[8],
            0x1A => {
                self.reg[8] = (self.reg[8] & !0xFF) | (d & 0xFF);
                self.result = self.reg[8];
            }
            0x1B => {
                self.reg[8] = (self.reg[8] & 0xFF) | (d << 8);
                self.result = self.reg[8];
            }
            0x1C => {
                self.reg[5] = (self.reg[5] & 0xFF) | (d << 8);
                self.do_window_test();
            }
            0x1D => {
                self.reg[3] = (self.reg[3] & 0xFF) | (d << 8);
                self.reg[2] = self.reg[2].wrapping_sub(self.reg[0]);
                if self.reg[2] < 0 {
                    self.reg[2] = -self.reg[2];
                }
                self.reg[3] = self.reg[3].wrapping_sub(self.reg[1]);
                if self.reg[3] < 0 {
                    self.reg[3] = -self.reg[3];
                }
                self.do_distance();
            }
            0x1E => self.do_distance(),
            // 0x1F: self-test stub
            0x1F => {}
            _ => {}
        }
    }

    /// Full rotation: REG4 -= REG2, REG5 -= REG3, then matrix multiply.
    fn do_rotation(&mut self, full: bool) {
        if full {
            self.reg[0xF] = -1; // REGf = 0xFFFF
            self.reg[4] = self.reg[4].wrapping_sub(self.reg[2]);
            self.reg[5] = self.reg[5].wrapping_sub(self.reg[3]);
        } else {
            self.reg[0xF] = 0;
        }
        self.do_rotation_part1();
    }

    /// First half of rotation: compute REG7 from REG0*REG4 - REG1*REG5.
    fn do_rotation_part1(&mut self) {
        let reg = &mut self.reg;

        // REG0 * REG4
        let mb_temp = (reg[0] as i32) * (reg[4] as i32);
        reg[0xC] = (mb_temp >> 16) as i16;
        reg[0xE] = (mb_temp & 0xFFFF) as i16;

        // -REG1 * REG5
        let mb_temp2 = (-(reg[1] as i32)) * (reg[5] as i32);
        reg[7] = (mb_temp2 >> 16) as i16;
        let mb_q = (mb_temp2 & 0xFFFF) as i16;

        reg[7] = reg[7].wrapping_add(reg[0xC]);

        // Rounding
        reg[0xE] = (reg[0xE] >> 1) & 0x7FFF;
        reg[0xC] = (mb_q >> 1) & 0x7FFF;
        let sum = reg[0xC].wrapping_add(reg[0xE]);
        if sum < 0 {
            reg[7] = reg[7].wrapping_add(1);
        }

        self.result = reg[7];

        if reg[0xF] < 0 {
            return; // one-step mode or first half of full rotation
        }

        reg[7] = reg[7].wrapping_add(reg[2]);
        self.do_rotation_part2();
    }

    /// Second half of rotation: compute REG8 from REG1*REG4 + REG0*REG5.
    fn do_rotation_part2(&mut self) {
        let reg = &mut self.reg;

        // REG1 * REG4
        let mb_temp = (reg[1] as i32) * (reg[4] as i32);
        reg[0xC] = (mb_temp >> 16) as i16;
        reg[9] = (mb_temp & 0xFFFF) as i16;

        // REG0 * REG5
        let mb_temp2 = (reg[0] as i32) * (reg[5] as i32);
        reg[8] = (mb_temp2 >> 16) as i16;
        let mb_q = (mb_temp2 & 0xFFFF) as i16;

        reg[8] = reg[8].wrapping_add(reg[0xC]);

        // Rounding
        reg[9] = (reg[9] >> 1) & 0x7FFF;
        reg[0xC] = (mb_q >> 1) & 0x7FFF;
        reg[9] = reg[9].wrapping_add(reg[0xC]);
        if reg[9] < 0 {
            reg[8] = reg[8].wrapping_add(1);
        }
        reg[9] <<= 1;

        self.result = reg[8];

        if reg[0xF] < 0 {
            return;
        }

        reg[8] = reg[8].wrapping_add(reg[3]);
        reg[9] &= !0xFF; // reg[9] &= 0xFF00

        // Fall through to division using REG7/REG8/REG9
        let r9 = reg[9];
        let r8 = reg[8];
        self.do_division(r9, r8);
    }

    /// Division using the step counter in REG6.
    fn do_division(&mut self, regc_init: i16, mb_q_init: i16) {
        let reg = &mut self.reg;

        let rege = reg[7] ^ mb_q_init; // save sign of result

        let mut regd: i16;
        let mut mb_q: i16;

        if mb_q_init >= 0 {
            regd = mb_q_init;
            mb_q = regc_init;
        } else {
            regd = (-mb_q_init).wrapping_sub(1);
            mb_q = (-regc_init).wrapping_sub(1);
            if mb_q < 0 && mb_q.wrapping_add(1) < 0 {
                regd = regd.wrapping_add(1);
            }
            mb_q = mb_q.wrapping_add(1);
        }

        // abs(REG7)
        let regc = if reg[7] >= 0 { reg[7] } else { -reg[7] };

        let mut regf = reg[6]; // step counter

        loop {
            regd = regd.wrapping_sub(regc);
            let msb = if (mb_q as u16) & 0x8000 != 0 {
                1_i16
            } else {
                0
            };
            mb_q <<= 1;
            if regd >= 0 {
                mb_q = mb_q.wrapping_add(1);
            } else {
                regd = regd.wrapping_add(regc);
            }
            regd <<= 1;
            regd = regd.wrapping_add(msb);

            regf = regf.wrapping_sub(1);
            if regf < 0 {
                break;
            }
        }

        self.result = if rege >= 0 { mb_q } else { -mb_q };
    }

    /// Window test computation.
    fn do_window_test(&mut self) {
        let reg = &mut self.reg;
        loop {
            let rege = (reg[4].wrapping_add(reg[7])) >> 1;
            let regf = (reg[5].wrapping_add(reg[8])) >> 1;
            if reg[0xB] < rege && regf < rege && rege.wrapping_add(regf) >= 0 {
                reg[7] = rege;
                reg[8] = regf;
            } else {
                reg[4] = rege;
                reg[5] = regf;
            }
            reg[6] = reg[6].wrapping_sub(1);
            if reg[6] < 0 {
                break;
            }
        }
        self.result = reg[8];
    }

    /// Distance approximation: max(a, b) + 3/8 * min(a, b).
    fn do_distance(&mut self) {
        let reg = &mut self.reg;
        let (regc, regd) = if reg[3] >= reg[2] {
            (reg[2], reg[3])
        } else {
            (reg[3], reg[2])
        };
        let regc_shifted = regc >> 2;
        let regd_sum = regd.wrapping_add(regc_shifted);
        let regc_half = regc_shifted >> 1;
        self.result = regc_half.wrapping_add(regd_sum);
        reg[0xD] = self.result;
    }

    /// Reset all registers and result.
    pub fn reset(&mut self) {
        self.reg = [0; 16];
        self.result = 0;
    }
}

impl Debuggable for Mathbox {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![DebugRegister {
            name: "RESULT",
            value: self.result as u16 as u64,
            width: 16,
        }]
    }
}

impl super::Device for Mathbox {
    fn name(&self) -> &'static str {
        "MATHBOX"
    }
    fn reset(&mut self) {
        self.reset();
    }
}

impl Default for Mathbox {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_zeroed() {
        let mb = Mathbox::new();
        assert_eq!(mb.result, 0);
        assert!(mb.reg.iter().all(|&r| r == 0));
    }

    #[test]
    fn status_always_zero() {
        let mb = Mathbox::new();
        assert_eq!(mb.status_r(), 0x00);
    }

    #[test]
    fn register_load_low_high() {
        let mut mb = Mathbox::new();
        mb.go_w(0x00, 0x34); // REG0 low = 0x34
        mb.go_w(0x01, 0x12); // REG0 high = 0x12
        assert_eq!(mb.reg[0], 0x1234);
        assert_eq!(mb.lo_r(), 0x34);
        assert_eq!(mb.hi_r(), 0x12);
    }

    #[test]
    fn distance_approximation() {
        let mut mb = Mathbox::new();
        // REG0 = 10, REG1 = 20, REG2 = 50, REG3 high loaded via 0x1D
        // REG2 -= REG0 → |50-10| = 40
        // REG3 = (data << 8) | REG3_low, then REG3 -= REG1
        mb.go_w(0x00, 10); // REG0 low
        mb.go_w(0x01, 0); // REG0 high
        mb.go_w(0x02, 20); // REG1 low
        mb.go_w(0x03, 0); // REG1 high
        mb.go_w(0x04, 50); // REG2 low
        mb.go_w(0x05, 0); // REG2 high
        mb.go_w(0x06, 80); // REG3 low
        // Now trigger distance with REG3 high = 0
        mb.go_w(0x1D, 0);
        // REG2 = |50 - 10| = 40
        // REG3 = |80 - 20| = 60
        // max=60, min=40
        // result = 60 + 40/4 + 40/8 = 60 + 10 + 5 = 75
        assert_eq!(mb.result, 75);
    }

    #[test]
    fn reset_clears_state() {
        let mut mb = Mathbox::new();
        mb.go_w(0x00, 0xFF);
        mb.go_w(0x01, 0x7F);
        mb.reset();
        assert_eq!(mb.result, 0);
        assert!(mb.reg.iter().all(|&r| r == 0));
    }

    #[test]
    fn result_read_split() {
        let mut mb = Mathbox::new();
        mb.result = -1; // 0xFFFF
        assert_eq!(mb.lo_r(), 0xFF);
        assert_eq!(mb.hi_r(), 0xFF);

        mb.result = 0x1234;
        assert_eq!(mb.lo_r(), 0x34);
        assert_eq!(mb.hi_r(), 0x12);
    }
}
