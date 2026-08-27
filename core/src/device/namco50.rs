use crate::core::debug::{DebugRegister, Debuggable};
use phosphor_macros::Saveable;

/// Namco 50XX custom chip — HLE (high-level emulation) of the score /
/// protection device.
///
/// In hardware this is a Fujitsu MB8842 MCU that keeps each player's running
/// score in BCD, tracks bonus/high-score thresholds, and returns the score
/// plus status flags when read. The main CPU drives it through the 06XX with a
/// small command set (reset, select player, increment/decrement mode, add a
/// fixed amount, set bonus/high-score thresholds). We model that behaviour
/// directly rather than running the MB8842 firmware.
///
/// Communication is byte-oriented through the 06XX: each command byte is
/// written to the data port with the 50XX selected in write mode, and the
/// response is four bytes read back with the 50XX selected in read mode:
///   Byte 0: status flags (0x80 high score, 0x40 first bonus, 0x20 interval)
///   Byte 1: BCD score digits 10^4..10^5
///   Byte 2: BCD score digits 10^2..10^3
///   Byte 3: BCD score digits 10^0..10^1
///
/// Xevious uses it only for a periodic protection check: reset, add 5, read,
/// and verify the low byte reads back 5.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
#[save_after_load(clamp_indices)]
pub struct Namco50 {
    /// Per-player running score (decimal; converted to BCD on read).
    #[save(id = 1)]
    score: [u32; 2],
    /// High-score threshold (decimal) for the status flag / set via command 5x.
    #[save(id = 2)]
    high_score: u32,
    /// Currently selected player (0 or 1).
    #[save(id = 3)]
    player: u8,
    /// True = add on the increment commands, false = subtract.
    #[save(id = 4)]
    increment: bool,
    /// Status flags for first / interval bonus.
    #[save(id = 5)]
    first_bonus: bool,
    #[save(id = 6)]
    interval_bonus: bool,
    /// Read byte position, cycles 0..3 across a four-byte response.
    #[save(id = 7)]
    read_index: u8,
    /// Remaining operand bytes for a multi-byte command (2x/3x/5x), and which
    /// command is collecting them.
    #[save(id = 8)]
    pending_cmd: u8,
    #[save(id = 9)]
    pending_bytes: u8,
    #[save(id = 10)]
    pending_data: [u8; 3],
}

/// Decimal amount added/subtracted by each 0x80-0xFF increment command.
fn increment_amount(cmd: u8) -> u32 {
    // Base tables for the 8x, Bx and Ex command groups; 9x/Ax scale 8x by
    // 10/100, Cx/Dx scale Bx, and Fx scales Ex by 10.
    const T8: [u32; 16] = [
        5, 10, 15, 20, 25, 30, 40, 50, 60, 70, 80, 90, 100, 200, 300, 500,
    ];
    const TB: [u32; 16] = [
        10, 20, 30, 40, 50, 60, 80, 100, 120, 140, 160, 180, 200, 400, 600, 1000,
    ];
    const TE: [u32; 16] = [
        15, 30, 45, 60, 75, 90, 120, 150, 180, 210, 240, 270, 300, 600, 900, 1500,
    ];
    let lo = (cmd & 0x0F) as usize;
    match cmd >> 4 {
        0x8 => T8[lo],
        0x9 => T8[lo] * 10,
        0xA => T8[lo] * 100,
        0xB => TB[lo],
        0xC => TB[lo] * 10,
        0xD => TB[lo] * 100,
        0xE => TE[lo],
        0xF => TE[lo] * 10,
        _ => 0,
    }
}

/// Pack two decimal digits of `n` (the `place`..`place`*100 pair) into a BCD byte.
fn bcd_pair(n: u32, place: u32) -> u8 {
    let pair = (n / place) % 100;
    (((pair / 10) << 4) | (pair % 10)) as u8
}

/// Decode a 3-byte BCD operand (as sent for set-bonus / set-high-score) to
/// decimal.
fn bcd3_to_dec(b: &[u8; 3]) -> u32 {
    let mut v = 0u32;
    for &byte in b {
        v = v * 100 + (byte >> 4) as u32 * 10 + (byte & 0x0F) as u32;
    }
    v
}

impl Default for Namco50 {
    fn default() -> Self {
        Self {
            score: [0; 2],
            high_score: 0,
            player: 0,
            // Power-on / post-reset default is increment mode, player 1.
            increment: true,
            first_bonus: false,
            interval_bonus: false,
            read_index: 0,
            pending_cmd: 0,
            pending_bytes: 0,
            pending_data: [0; 3],
        }
    }
}

impl Namco50 {
    pub fn new() -> Self {
        Self::default()
    }

    /// Write a command byte (main CPU → 50XX via the 06XX data port).
    pub fn write(&mut self, cmd: u8) {
        // Collect the operand bytes for a pending multi-byte command first.
        if self.pending_bytes > 0 {
            let idx = 3 - self.pending_bytes as usize;
            self.pending_data[idx] = cmd;
            self.pending_bytes -= 1;
            if self.pending_bytes == 0 {
                let value = bcd3_to_dec(&self.pending_data);
                match self.pending_cmd {
                    0x2 => self.first_bonus = value != 0,
                    0x3 => self.interval_bonus = value != 0,
                    0x5 => self.high_score = value,
                    _ => {}
                }
            }
            return;
        }

        match cmd >> 4 {
            0x0 => {} // nop
            0x1 => {
                // reset scores (also restores player 1 / increment mode)
                self.score = [0; 2];
                self.player = 0;
                self.increment = true;
            }
            0x2 | 0x3 | 0x5 => {
                // set first bonus / interval bonus / high score (3 more bytes)
                self.pending_cmd = cmd >> 4;
                self.pending_bytes = 3;
            }
            0x4 => {} // unknown/unused
            0x6 => self.player = u8::from(cmd == 0x68),
            0x7 => self.increment = cmd == 0x70,
            0x8..=0xF => {
                let amount = increment_amount(cmd);
                let s = &mut self.score[self.player as usize];
                if self.increment {
                    *s = (*s + amount) % 100_000_000;
                } else {
                    *s = s.saturating_sub(amount);
                }
            }
            _ => unreachable!(),
        }
    }

    /// Bring the two indices back into range after a load.
    ///
    /// `player` selects one of two scores and `read_index` one of four response
    /// bytes, and both are masked wherever the chip sets them, so nothing this
    /// writer emits is out of range. The masks are here because a save is an
    /// input, and `player` indexes an array.
    fn clamp_indices(&mut self) {
        self.player &= 1;
        self.read_index &= 0x03;
    }

    /// Read the next response byte (50XX → main CPU via the 06XX data port).
    /// Cycles through the four-byte score/flags response.
    pub fn read(&mut self) -> u8 {
        let b = self.response_byte(self.read_index);
        self.read_index = (self.read_index + 1) & 0x03;
        b
    }

    fn response_byte(&self, index: u8) -> u8 {
        let score = self.score[self.player as usize];
        match index & 0x03 {
            0 => {
                let mut flags = 0u8;
                if score >= self.high_score {
                    flags |= 0x80;
                }
                if self.first_bonus {
                    flags |= 0x40;
                }
                if self.interval_bonus {
                    flags |= 0x20;
                }
                flags
            }
            1 => bcd_pair(score, 10_000),
            2 => bcd_pair(score, 100),
            _ => bcd_pair(score, 1),
        }
    }

    /// Reset to power-on state (scores cleared, first player, increment mode).
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

impl super::Device for Namco50 {
    fn name(&self) -> &'static str {
        "Namco 50XX"
    }
    fn reset(&mut self) {
        self.reset();
    }
}

impl Debuggable for Namco50 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read the full four-byte response.
    fn response(n50: &mut Namco50) -> [u8; 4] {
        n50.read_index = 0;
        [n50.read(), n50.read(), n50.read(), n50.read()]
    }

    #[test]
    fn xevious_protection_check() {
        // The game's periodic check: reset, add 5, expect a low byte of 5.
        let mut n50 = Namco50::new();
        n50.write(0x10); // reset scores
        n50.write(0x80); // increment by 5 (default increment mode)
        assert_eq!(response(&mut n50), [0x80, 0x00, 0x00, 0x05]);
    }

    #[test]
    fn increment_amounts_and_bcd() {
        let mut n50 = Namco50::new();
        n50.write(0x10);
        n50.write(0x87); // +50
        n50.write(0x8C); // +100
        // score = 150 -> BCD bytes 00 01 50
        assert_eq!(response(&mut n50), [0x80, 0x00, 0x01, 0x50]);
    }

    #[test]
    fn player_select_is_independent() {
        let mut n50 = Namco50::new();
        n50.write(0x10);
        n50.write(0x80); // player 1 += 5
        n50.write(0x68); // select player 2
        n50.write(0x81); // player 2 += 10
        assert_eq!(response(&mut n50)[3], 0x10); // player 2 score = 10
        n50.write(0x60); // back to player 1
        assert_eq!(response(&mut n50)[3], 0x05); // player 1 score = 5
    }

    #[test]
    fn read_cycles_four_bytes() {
        let mut n50 = Namco50::new();
        n50.write(0x10);
        n50.write(0x80);
        // Four sequential reads then wrap to byte 0 again.
        let seq = [n50.read(), n50.read(), n50.read(), n50.read(), n50.read()];
        assert_eq!(seq, [0x80, 0x00, 0x00, 0x05, 0x80]);
    }
}
