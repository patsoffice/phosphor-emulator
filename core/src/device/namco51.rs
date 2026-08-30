use phosphor_macros::Saveable;

/// Namco 51XX custom chip — input multiplexer with credit management.
///
/// In hardware, this is a Fujitsu MB8843 MCU running firmware that handles
/// coin counting, credit management, joystick remapping, and input
/// multiplexing. We emulate the external behavior directly.
///
/// I/O ports (active-low, directly from cabinet switches):
///
/// ```text
/// IN0[3:0] = P1 joystick {Left, Down, Right, Up}
/// IN0[7:4] = P2 joystick {Left, Down, Right, Up}
/// IN1[0]   = P1 Fire
/// IN1[1]   = P2 Fire
/// IN1[2]   = Start1
/// IN1[3]   = Start2
/// IN1[4]   = Coin1
/// IN1[5]   = Coin2
/// IN1[6]   = Service
/// IN1[7]   = Test/Service Mode
/// ```
///
/// Commands (written via 06xx):
///   0x00: nop
///   0x01 + 4 args: set coinage (`coins_per_credit[0]`, `creds_per_coin[0]`,
///                                `coins_per_credit[1]`, `creds_per_coin[1]`)
///   0x02: enter credit mode
///   0x03: disable joystick remapping
///   0x04: enable joystick remapping
///   0x05: enter switch mode (raw inputs)
///   0x06-0x07: nop
#[derive(Saveable)]
#[save_version(1)]
pub struct Namco51 {
    /// Operating mode: false = switch, true = credit.
    pub credit_mode: bool,
    /// Credit counter (0-99).
    pub credits: u8,
    /// Coinage: coins needed per credit (slot 0/1).
    coins_per_credit: [u8; 2],
    /// Coinage: credits awarded per N coins (slot 0/1).
    creds_per_coin: [u8; 2],
    /// Partial coin accumulator (slot 0/1).
    coin_count: [u8; 2],
    /// Read sequence counter (0-2, wraps mod 3).
    pub read_index: u8,
    /// Previous IN1 state for coin/start edge detection.
    last_coins: u8,
    /// Previous fire button state for P1/P2 edge detection.
    last_buttons: u8,
    /// Joystick remapping enabled (command 03/04).
    remap_joy: bool,
    /// Credit mode sub-state: 1 = waiting for start, 2 = game active.
    credit_state: u8,
    /// Command write state machine.
    pub write_state: u8,
    /// Coinage argument counter (0..coinage_total during command 01).
    coinage_arg: u8,
    /// Number of argument bytes the coinage command (01) consumes. Normally 4;
    /// Xevious has a firmware quirk where it is 6 (the first two are ignored),
    /// so a trailing "enter credit mode" (02) command is not swallowed.
    coinage_total: u8,
    /// Enables the Xevious 6-argument coinage quirk (set by the machine wrapper).
    xevious_coinage_kludge: bool,
}

/// Joystick direction remapping table.
/// Index is the raw active-low 4-bit joystick value: bit3=L, bit2=D, bit1=R, bit0=U.
/// Output is a direction code: 0=up, 1=up-right, 2=right, 3=down-right,
/// 4=down, 5=down-left, 6=left, 7=up-left, 8=center, 9+=invalid.
const JOY_MAP: [u8; 16] = [
    0xf, 0xe, 0xd, 0x5, 0xc, 0x9, 0x7, 0x6, 0xb, 0x3, 0xa, 0x4, 0x1, 0x2, 0x0, 0x8,
];

impl Namco51 {
    pub fn new() -> Self {
        Self {
            credit_mode: false,
            credits: 0,
            coins_per_credit: [1, 1],
            creds_per_coin: [1, 1],
            coin_count: [0, 0],
            read_index: 0,
            last_coins: 0xFF, // all buttons released (active-low)
            last_buttons: 0xFF,
            remap_joy: true,
            credit_state: 1,
            write_state: 0,
            coinage_arg: 0,
            coinage_total: 4,
            xevious_coinage_kludge: false,
        }
    }

    /// Enable/disable the Xevious coinage quirk (6-argument command 01).
    pub fn set_xevious_coinage_kludge(&mut self, on: bool) {
        self.xevious_coinage_kludge = on;
    }

    /// Write a command or argument byte (from 06xx data write).
    pub fn write(&mut self, data: u8) {
        if self.write_state == 1 {
            // Receiving coinage arguments for command 01. Only the last four of
            // `coinage_total` bytes carry values; any leading bytes (Xevious's
            // 6-argument quirk) are consumed and discarded.
            let base = self.coinage_total.saturating_sub(4);
            if self.coinage_arg >= base {
                match self.coinage_arg - base {
                    0 => self.coins_per_credit[0] = data & 0x0F,
                    1 => self.creds_per_coin[0] = data & 0x0F,
                    2 => self.coins_per_credit[1] = data & 0x0F,
                    _ => self.creds_per_coin[1] = data & 0x0F,
                }
            }
            self.coinage_arg += 1;
            if self.coinage_arg >= self.coinage_total {
                self.write_state = 0; // done with args
            }
            return;
        }

        // Process command byte.
        let cmd = data & 0x07;
        match cmd {
            0x01 => {
                // Set coinage: normally 4 argument nibbles, 6 for Xevious.
                self.write_state = 1;
                self.coinage_arg = 0;
                self.coinage_total = if self.xevious_coinage_kludge { 6 } else { 4 };
                self.credits = 0;
                self.coin_count = [0, 0];
                if self.xevious_coinage_kludge {
                    self.remap_joy = true;
                }
            }
            0x02 => {
                // Enter credit mode.
                self.credit_mode = true;
                self.credit_state = 1; // waiting for start
                self.read_index = 0;
            }
            0x03 => {
                // Disable joystick remapping.
                self.remap_joy = false;
            }
            0x04 => {
                // Enable joystick remapping.
                self.remap_joy = true;
            }
            0x05 => {
                // Enter switch mode (raw inputs).
                self.credit_mode = false;
                self.read_index = 0;
            }
            _ => {} // 0, 6, 7: nop
        }
    }

    /// Read the next output byte (from 06xx data read).
    /// `in0` and `in1` are the current active-low input port values.
    pub fn read(&mut self, in0: u8, in1: u8) -> u8 {
        let idx = self.read_index;
        self.read_index = (self.read_index + 1) % 3;

        if self.credit_mode {
            self.read_credit_mode(idx, in0, in1)
        } else {
            self.read_switch_mode(idx, in0, in1)
        }
    }

    /// Switch mode: return raw input bytes.
    fn read_switch_mode(&self, idx: u8, in0: u8, in1: u8) -> u8 {
        match idx {
            0 => in0,
            1 => in1,
            _ => 0,
        }
    }

    /// Credit mode: process coins, return credit count and player inputs.
    fn read_credit_mode(&mut self, idx: u8, in0: u8, in1: u8) -> u8 {
        match idx {
            0 => self.read_credits(in1),
            1 => self.read_player_input(in0 & 0x0F, in1 & 1, 0),
            2 => self.read_player_input((in0 >> 4) & 0x0F, (in1 >> 1) & 1, 1),
            _ => 0,
        }
    }

    /// Read nibble 0: handle coin counting and return BCD credit count.
    fn read_credits(&mut self, in1: u8) -> u8 {
        // Test mode: IN1 bit 7 active-low → when 0, test mode active.
        if in1 & 0x80 == 0 {
            return 0xBB;
        }

        // Invert to active-high for edge detection.
        let active = !in1;
        let prev_active = !self.last_coins;
        let toggle = active ^ prev_active;
        self.last_coins = in1;

        // Coin 1 (IN1 bit 4): rising edge detection.
        if toggle & active & 0x10 != 0 && self.coins_per_credit[0] > 0 {
            self.coin_count[0] += 1;
            if self.coin_count[0] >= self.coins_per_credit[0] {
                self.credits = self.credits.saturating_add(self.creds_per_coin[0]).min(99);
                self.coin_count[0] -= self.coins_per_credit[0];
            }
        }

        // Coin 2 (IN1 bit 5): rising edge detection.
        if toggle & active & 0x20 != 0 && self.coins_per_credit[1] > 0 {
            self.coin_count[1] += 1;
            if self.coin_count[1] >= self.coins_per_credit[1] {
                self.credits = self.credits.saturating_add(self.creds_per_coin[1]).min(99);
                self.coin_count[1] -= self.coins_per_credit[1];
            }
        }

        // Service coin (IN1 bit 6): rising edge.
        if toggle & active & 0x40 != 0 {
            self.credits = self.credits.saturating_add(1).min(99);
        }

        // Free play: if coins_per_credit[0] == 0, credits pinned to 100.
        if self.coins_per_credit[0] == 0 {
            self.credits = 100;
        }

        // Start button handling (only in credit_state 1 = waiting for start).
        if self.credit_state == 1 {
            // Start 1 (IN1 bit 2): needs 1 credit.
            if toggle & active & 0x04 != 0 && self.credits >= 1 {
                self.credits -= 1;
                self.credit_state = 2;
            }
            // Start 2 (IN1 bit 3): needs 2 credits.
            else if toggle & active & 0x08 != 0 && self.credits >= 2 {
                self.credits -= 2;
                self.credit_state = 2;
            }
        }

        // Return BCD credit count.
        let tens = (self.credits / 10) & 0x0F;
        let ones = self.credits % 10;
        (tens << 4) | ones
    }

    /// Read nibble 1/2: player joystick direction + fire state.
    /// `joy_raw` = raw 4-bit active-low joystick bits.
    /// `fire_raw` = raw active-low fire bit (0 = pressed).
    /// `player` = 0 for P1, 1 for P2.
    fn read_player_input(&mut self, joy_raw: u8, fire_raw: u8, player: u8) -> u8 {
        let dir = if self.remap_joy {
            JOY_MAP[(joy_raw & 0x0F) as usize]
        } else {
            joy_raw & 0x0F
        };

        // Fire button: active-low → 0 = pressed.
        let fire_pressed = fire_raw == 0;
        let prev_mask = 1 << player;
        let was_pressed = self.last_buttons & prev_mask == 0;

        // Update last state for this player.
        if fire_pressed {
            self.last_buttons &= !prev_mask;
        } else {
            self.last_buttons |= prev_mask;
        }

        let toggle = fire_pressed && !was_pressed;

        // Output uses active-low: 0 = pressed/toggled, 1 = released/no-toggle.
        dir | ((!toggle as u8) << 4) | ((!fire_pressed as u8) << 5)
    }

    pub fn reset(&mut self) {
        self.credit_mode = false;
        self.credits = 0;
        self.coin_count = [0, 0];
        self.read_index = 0;
        self.last_coins = 0xFF;
        self.last_buttons = 0xFF;
        self.remap_joy = true;
        self.credit_state = 1;
        self.write_state = 0;
        self.coinage_arg = 0;
        self.coinage_total = 4;
        // xevious_coinage_kludge is a machine-configuration flag, not runtime
        // state, so it survives a reset.
    }
}

impl Default for Namco51 {
    fn default() -> Self {
        Self::new()
    }
}

impl super::Device for Namco51 {
    fn name(&self) -> &'static str {
        "Namco 51XX"
    }
    fn reset(&mut self) {
        self.reset();
    }
}

use crate::core::debug::{DebugRegister, Debuggable};

impl Debuggable for Namco51 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "MODE",
                value: self.credit_mode as u64,
                width: 1,
            },
            DebugRegister {
                name: "CREDITS",
                value: self.credits as u64,
                width: 8,
            },
            DebugRegister {
                name: "READ_IDX",
                value: self.read_index as u64,
                width: 2,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Standard coinage protocol (Galaga/Dig Dug): command 01 consumes exactly
    /// four argument bytes, so a following 02 enters credit mode.
    #[test]
    fn default_coinage_consumes_four_args() {
        let mut n = Namco51::new();
        n.write(0x01); // set coinage
        for _ in 0..4 {
            n.write(0x03); // four coinage argument nibbles
        }
        assert!(!n.credit_mode, "still switch mode after coinage command");
        n.write(0x02); // enter credit mode
        assert!(n.credit_mode, "02 enters credit mode after 4 coinage args");
        assert_eq!(n.coins_per_credit[0], 3);
        assert_eq!(n.creds_per_coin[1], 3);
    }

    /// Xevious drives command 01 with six argument bytes (a firmware quirk).
    /// Without the kludge the trailing 02 is swallowed as an argument and the
    /// chip never leaves switch mode; with it, credit mode is entered.
    #[test]
    fn xevious_kludge_consumes_six_args() {
        // Exact command stream Xevious emits: 01 then 6×01 then 02.
        let stream = [0x01u8, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02];

        // Without the kludge: the 02 lands mid-arguments, no credit mode.
        let mut plain = Namco51::new();
        for &b in &stream {
            plain.write(b);
        }
        assert!(
            !plain.credit_mode,
            "without kludge the 02 is swallowed by a re-triggered coinage command"
        );

        // With the kludge: 01 eats six args, so the 02 is a real command.
        let mut xev = Namco51::new();
        xev.set_xevious_coinage_kludge(true);
        for &b in &stream[..7] {
            xev.write(b); // command + 6 swallowed arguments
        }
        assert!(
            !xev.credit_mode,
            "still consuming the six coinage arguments"
        );
        xev.write(stream[7]); // 0x02
        assert!(xev.credit_mode, "kludge: 02 enters credit mode");
    }

    /// In credit mode with no credits and idle inputs, the first response byte
    /// is the BCD credit count (0x00), matching the reference hardware.
    #[test]
    fn credit_mode_first_byte_is_credit_count() {
        let mut n = Namco51::new();
        n.set_xevious_coinage_kludge(true);
        for b in [0x01u8, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x02] {
            n.write(b);
        }
        assert!(n.credit_mode);
        // Idle inputs are all-ones (active low); first read = 0 credits (BCD).
        assert_eq!(n.read(0xFF, 0xFF), 0x00);
    }
}
