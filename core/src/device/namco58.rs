//! Namco 58XX custom I/O chip, behind a 16XX on the Mappy board.
//!
//! # What this is, and what it is not
//!
//! The 58XX is a Fujitsu MB8843 four-bit MCU with a mask ROM, and **that ROM
//! has never been dumped**. So unlike the 51XX here, which has a low-level
//! model next to its behavioral one, there is no program to run: this is a
//! model of the chip's command modes as they have been inferred from what the
//! game programs write to it and what they expect back. Every mode below is
//! that inference. Nothing in it was read off a schematic, because the
//! schematic shows the chip as a box.
//!
//! What the schematic does give is the pinout and the plumbing: four 4-bit
//! input ports (A on pins 38-41, B on 22-25, C on 26-29, D on 30-33), two
//! 4-bit output ports (A on 13-16, B on 17-20), a reset, and a 16-nibble
//! window of the 16XX's buffer RAM that is its whole interface to the CPU.
//!
//! # The interface
//!
//! The CPU reads and writes the 16 nibbles at any time; the chip acts on them
//! once per frame when the board runs it ([`Namco58::run`]). Nibble 8 is the
//! command; 9-15 are its arguments; the chip writes its results into 0-7.
//!
//! ```text
//!   mode 1  read switches: 4-7 = ports A-D, outputs A/B = nibbles 9/10
//!   mode 2  set coinage: 9-12 = coins/credit and credits/coin per slot
//!   mode 3  credit mode: coin and start handling (see `handle_coins`)
//!   mode 4  read multiplexed switches: output A pin 13 low, then high,
//!           sampling all four ports each time into the even, then odd,
//!           nibbles
//!   mode 5  boot check: a 7-bit LFSR over nibbles 9-15, results in 1-7
//! ```
//!
//! The ports are active-low at the pins; the chip stores them inverted, so a
//! pressed switch reads back as a set bit.
//!
//! # Against the 56XX
//!
//! The command set differs in numbering, not in kind: 58XX mode 3 is 56XX
//! mode 4 with the credit nibbles swapped (0/1 <-> 2/3), 58XX mode 4 is 56XX
//! mode 9, and the boot check (58XX mode 5) is an LFSR where the 56XX sums.
//! The two chips share [`InPort`].

use crate::device::namco56::InPort;
use phosphor_macros::Saveable;

#[derive(Saveable)]
#[save_version(1)]
pub struct Namco58 {
    /// The 16-nibble window of the 16XX buffer RAM this chip owns.
    ram: [u8; 16],
    /// The reset input is asserted. A chip in reset does not run.
    in_reset: bool,
    /// Output port A (pins 13-16) and B (pins 17-20).
    out: [u8; 2],
    credits: u8,
    coins: [u8; 2],
    coins_per_credit: [u8; 2],
    credits_per_coin: [u8; 2],
    /// Last sample of port A (coins) and port D (buttons), active-high, for
    /// edge detection.
    last_coins: u8,
    last_buttons: u8,
}

impl Default for Namco58 {
    fn default() -> Self {
        Self::new()
    }
}

impl Namco58 {
    pub fn new() -> Self {
        let mut chip = Self {
            ram: [0; 16],
            in_reset: false,
            out: [0; 2],
            credits: 0,
            coins: [0; 2],
            coins_per_credit: [1; 2],
            credits_per_coin: [1; 2],
            last_coins: 0,
            last_buttons: 0,
        };
        chip.set_reset(true);
        chip
    }

    /// CPU read of nibble `offset` (0-15). The RAM is four bits wide and the
    /// upper half of the data bus floats high.
    pub fn read(&self, offset: u16) -> u8 {
        0xF0 | self.ram[(offset & 0x0F) as usize]
    }

    /// CPU write of nibble `offset` (0-15). Only the low four bits are stored.
    pub fn write(&mut self, offset: u16, data: u8) {
        self.ram[(offset & 0x0F) as usize] = data & 0x0F;
    }

    /// Drive the reset input. Asserting it clears the chip's credit and
    /// coinage state; it does not clear the shared RAM, which is the 16XX's.
    pub fn set_reset(&mut self, asserted: bool) {
        self.in_reset = asserted;
        if asserted {
            self.credits = 0;
            self.coins = [0; 2];
            self.coins_per_credit = [1; 2];
            self.credits_per_coin = [1; 2];
        }
    }

    pub fn in_reset(&self) -> bool {
        self.in_reset
    }

    /// Output port A (pins 13-16).
    pub fn out_a(&self) -> u8 {
        self.out[0]
    }

    /// Output port B (pins 17-20).
    pub fn out_b(&self) -> u8 {
        self.out[1]
    }

    /// The credit count the chip is holding (0-99 in practice).
    pub fn credits(&self) -> u8 {
        self.credits
    }

    /// Execute the command in nibble 8, once. Does nothing while in reset.
    ///
    /// `read(port, out_a)` returns the raw (active-low) level on one input
    /// port, given what this chip is presenting on output port A at that
    /// moment. The board needs the second argument because the Mappy board
    /// hangs a 74LS157 on the second chip's port A, selected by pin 13, to
    /// read eight DIP switches through four pins.
    pub fn run<F: FnMut(InPort, u8) -> u8>(&mut self, mut read: F) {
        if self.in_reset {
            return;
        }
        let mut sample = |chip: &Self, port| !read(port, chip.out[0]) & 0x0F;
        match self.ram[8] {
            1 => {
                for (i, port) in [InPort::A, InPort::B, InPort::C, InPort::D]
                    .into_iter()
                    .enumerate()
                {
                    self.ram[4 + i] = sample(self, port);
                }
                self.out = [self.ram[9], self.ram[10]];
            }
            2 => {
                self.coins_per_credit = [self.ram[9], self.ram[11]];
                self.credits_per_coin = [self.ram[10], self.ram[12]];
            }
            3 => self.handle_coins(&mut sample),
            4 => {
                for pin13 in 0..2u8 {
                    self.out[0] = pin13;
                    for (i, port) in [InPort::A, InPort::B, InPort::C, InPort::D]
                        .into_iter()
                        .enumerate()
                    {
                        self.ram[i * 2 + pin13 as usize] = sample(self, port);
                    }
                }
            }
            5 => self.boot_check(),
            // 0 is idle. Any other code is one no program on this board is
            // known to send.
            _ => {}
        }
    }

    /// Mode 3: count coins into credits, debit credits on a start press, and
    /// report the joystick and button ports.
    ///
    /// Results: 2-3 the BCD credit count, 0 a credit-added flag, 1 a
    /// credit-spent flag (both left for the CPU to clear), 4 and 6 ports B and
    /// C, and 5 and 7 the four buttons of port D as level-and-edge pairs.
    /// This is the 56XX credit mode with the credit nibbles swapped (0/1 <->
    /// 2/3).
    fn handle_coins<F: FnMut(&Self, InPort) -> u8>(&mut self, sample: &mut F) {
        let coins = sample(self, InPort::A);
        let toggled = coins ^ self.last_coins;
        self.last_coins = coins;

        let mut credit_add = 0u8;
        for slot in 0..2 {
            if coins & toggled & (1 << slot) == 0 {
                continue;
            }
            let per = self.coins_per_credit[slot] & 7;
            self.coins[slot] += 1;
            if self.coins[slot] >= per {
                credit_add =
                    self.credits_per_coin[slot].wrapping_sub(self.coins_per_credit[slot] >> 3);
                self.coins[slot] -= per;
            } else if self.coins_per_credit[slot] & 8 != 0 {
                credit_add = 1;
            }
        }
        // The service coin always adds one.
        if coins & toggled & 0x08 != 0 {
            credit_add = 1;
        }

        let buttons = sample(self, InPort::D);
        let toggled = buttons ^ self.last_buttons;
        self.last_buttons = buttons;

        let mut credit_sub = 0u8;
        // Start is honored only while the game leaves nibble 9 at zero.
        if self.ram[9] == 0 {
            if buttons & toggled & 0x04 != 0 {
                if self.credits >= 1 {
                    credit_sub = 1;
                }
            } else if buttons & toggled & 0x08 != 0 && self.credits >= 2 {
                credit_sub = 2;
            }
        }

        self.credits = self
            .credits
            .wrapping_add(credit_add)
            .wrapping_sub(credit_sub);
        self.ram[2] = (self.credits / 10) & 0x0F;
        self.ram[3] = (self.credits % 10) & 0x0F;
        if credit_add != 0 {
            self.ram[0] = credit_add & 0x0F;
        }
        if credit_sub != 0 {
            self.ram[1] = credit_sub & 0x0F;
        }
        self.ram[4] = sample(self, InPort::B);
        self.ram[5] = (((buttons & 0x05) << 1) | (buttons & toggled & 0x05)) & 0x0F;
        self.ram[6] = sample(self, InPort::C);
        self.ram[7] = ((buttons & 0x0A) | ((buttons & toggled & 0x0A) >> 1)) & 0x0F;
    }

    /// Mode 5: the boot check. A 7-bit LFSR seeded from nibbles 9-10 picks
    /// which of the remaining arguments to XOR into each of the seven result
    /// nibbles. Only the low four bits of the accumulator survive, so the
    /// whole computation fits in a byte.
    fn boot_check(&mut self) {
        fn next(n: u8) -> u8 {
            (if n & 1 != 0 { n ^ 0x90 } else { n }) >> 1
        }
        let warmup = (self.ram[9] << 4) | self.ram[10];
        let mut seed = 0x22u8;
        for _ in 0..warmup & 0x7F {
            seed = next(seed);
        }
        // The arguments XOR in the fixed order 11, 10, 9, 15, 14, 13, 12.
        let args = [11, 10, 9, 15, 14, 13, 12].map(|i| !self.ram[i] & 0x0F);
        for i in 1..8 {
            let mut rng = seed;
            let mut n = 0u8;
            for (k, &arg) in args.iter().enumerate() {
                if rng & 1 != 0 {
                    n ^= arg;
                }
                rng = next(rng);
                if k == 0 {
                    seed = rng;
                }
            }
            self.ram[i] = !n & 0x0F;
        }
        self.ram[0] = 0x0;
        // MAME parity for Gaplus, which runs the check with all-0xF
        // arguments and expects nibble 0 set. Mappy never sends that.
        if self.ram[9] == 0xF {
            self.ram[0] = 0xF;
        }
    }

    /// Power-on: RAM cleared, outputs low, reset asserted until the board
    /// releases it.
    pub fn reset(&mut self) {
        *self = Self::new();
    }
}

impl crate::device::Device for Namco58 {
    fn name(&self) -> &'static str {
        "Namco 58XX"
    }
    fn reset(&mut self) {
        self.reset();
    }
    fn read(&mut self, offset: u16) -> u8 {
        Namco58::read(self, offset)
    }
    fn write(&mut self, offset: u16, data: u8) {
        Namco58::write(self, offset, data);
    }
}

use crate::core::debug::{DebugRegister, Debuggable};

impl Debuggable for Namco58 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "MODE",
                value: self.ram[8] as u64,
                width: 8,
            },
            DebugRegister {
                name: "RESET",
                value: self.in_reset as u64,
                width: 8,
            },
            DebugRegister {
                name: "CREDITS",
                value: self.credits as u64,
                width: 8,
            },
            DebugRegister {
                name: "OUT_A",
                value: self.out[0] as u64,
                width: 8,
            },
            DebugRegister {
                name: "OUT_B",
                value: self.out[1] as u64,
                width: 8,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port levels as the pins see them (active-low), one nibble per port.
    #[derive(Clone, Copy)]
    struct Pins([u8; 4]);

    impl Pins {
        fn idle() -> Self {
            Self([0x0F; 4])
        }
        fn read(&self, port: InPort) -> u8 {
            self.0[port as usize]
        }
    }

    fn running() -> Namco58 {
        let mut c = Namco58::new();
        c.set_reset(false);
        c
    }

    fn command(c: &mut Namco58, mode: u8, args: &[u8]) {
        for (i, &a) in args.iter().enumerate() {
            c.write(9 + i as u16, a);
        }
        c.write(8, mode);
    }

    #[test]
    fn reads_return_the_nibble_with_the_upper_bus_floating() {
        let mut c = running();
        c.write(3, 0xA5);
        assert_eq!(c.read(3), 0xF5);
        assert_eq!(c.read(0x13), 0xF5, "the window is sixteen nibbles");
    }

    #[test]
    fn a_chip_in_reset_does_not_run() {
        let mut c = Namco58::new();
        assert!(c.in_reset(), "power-on holds the chip in reset");
        command(&mut c, 5, &[3, 6, 5, 0xF, 0xA, 0xC, 0xE]);
        c.run(|p, _| Pins::idle().read(p));
        assert_eq!(c.read(1) & 0xF, 0);
        c.set_reset(false);
        c.run(|p, _| Pins::idle().read(p));
        assert_ne!(c.read(1) & 0xF, 0);
    }

    #[test]
    fn the_boot_check_matches_what_the_mappy_program_waits_for() {
        // Both 58XX chips get arguments 3 6 5 f a c e (ROM table at $F855)
        // and the program spins until nibbles 1-7 read 8 4 6 e d 9 d (ROM
        // table at $F85C).
        let mut c = running();
        command(&mut c, 5, &[3, 6, 5, 0xF, 0xA, 0xC, 0xE]);
        c.run(|p, _| Pins::idle().read(p));
        let got: Vec<u8> = (0..8).map(|i| c.read(i) & 0xF).collect();
        assert_eq!(got, vec![0x0, 0x8, 0x4, 0x6, 0xE, 0xD, 0x9, 0xD]);
    }

    #[test]
    fn mode_one_reads_the_ports_inverted_into_nibbles_four_to_seven() {
        let mut c = running();
        command(&mut c, 1, &[0x3, 0xC]);
        let pins = Pins([0x0E, 0x0D, 0x0B, 0x07]);
        c.run(|p, _| pins.read(p));
        let got: Vec<u8> = (4..8).map(|i| c.read(i) & 0xF).collect();
        assert_eq!(got, vec![0x1, 0x2, 0x4, 0x8]);
        assert_eq!((c.out_a(), c.out_b()), (0x3, 0xC));
    }

    #[test]
    fn mode_four_samples_each_port_with_pin_thirteen_low_then_high() {
        // A multiplexer on port A that shows one nibble with pin 13 low and
        // another with it high, which is how the board reads its DIPs.
        let mut c = running();
        command(&mut c, 4, &[]);
        let mut order = Vec::new();
        c.run(|p, out_a| {
            order.push((p, out_a & 1));
            match (p, out_a & 1) {
                (InPort::A, 0) => 0x0E,
                (InPort::A, _) => 0x07,
                _ => 0x0F,
            }
        });
        assert_eq!(c.read(0) & 0xF, 0x1, "port A, pin 13 low");
        assert_eq!(c.read(1) & 0xF, 0x8, "port A, pin 13 high");
        // Pin 13 is low for all four reads, then high for all four.
        let pins: Vec<u8> = order.iter().map(|&(_, pin)| pin).collect();
        assert_eq!(pins, vec![0, 0, 0, 0, 1, 1, 1, 1]);
    }

    /// Run mode 3 once per call with port A (coins) and D (buttons) as given.
    fn frame(c: &mut Namco58, coins: u8, buttons: u8) {
        let pins = Pins([coins, 0x0F, 0x0F, buttons]);
        c.run(|p, _| pins.read(p));
    }

    #[test]
    fn a_coin_counts_on_its_press_edge_only() {
        let mut c = running();
        command(&mut c, 2, &[1, 1, 1, 1]);
        c.run(|p, _| Pins::idle().read(p));
        command(&mut c, 3, &[0]);
        frame(&mut c, 0x0E, 0x0F); // coin 1 down
        frame(&mut c, 0x0E, 0x0F); // still down: no second credit
        frame(&mut c, 0x0F, 0x0F); // released
        assert_eq!(c.credits(), 1);
        assert_eq!(c.read(3) & 0xF, 1, "BCD units live in nibble 3");
        assert_eq!(c.read(0) & 0xF, 1, "credit-added flag lives in nibble 0");
        frame(&mut c, 0x0E, 0x0F);
        assert_eq!(c.credits(), 2);
    }

    #[test]
    fn two_coins_per_credit_needs_two_presses() {
        let mut c = running();
        command(&mut c, 2, &[2, 1, 1, 1]);
        c.run(|p, _| Pins::idle().read(p));
        command(&mut c, 3, &[0]);
        let mut credits = Vec::new();
        for _ in 0..4 {
            frame(&mut c, 0x0E, 0x0F);
            frame(&mut c, 0x0F, 0x0F);
            credits.push(c.credits());
        }
        assert_eq!(credits, vec![0, 1, 1, 2]);
    }

    #[test]
    fn start_spends_a_credit_only_when_the_game_allows_it() {
        let mut c = running();
        command(&mut c, 3, &[1]); // nibble 9 nonzero: start ignored
        frame(&mut c, 0x0E, 0x0F);
        frame(&mut c, 0x0F, 0x0B); // start 1
        assert_eq!(c.credits(), 1);
        c.write(9, 0);
        frame(&mut c, 0x0F, 0x0F);
        frame(&mut c, 0x0F, 0x0B);
        assert_eq!(c.credits(), 0);
        assert_eq!(c.read(1) & 0xF, 1, "credit-spent flag lives in nibble 1");
    }

    #[test]
    fn buttons_report_level_and_edge() {
        let mut c = running();
        command(&mut c, 3, &[1]);
        frame(&mut c, 0x0F, 0x0E); // button 1 (port D bit 0) pressed
        assert_eq!(c.read(5) & 0xF, 0b0011, "level in bit 1, edge in bit 0");
        frame(&mut c, 0x0F, 0x0E);
        assert_eq!(c.read(5) & 0xF, 0b0010, "held: level without the edge");
    }

    #[test]
    fn asserting_reset_forgets_the_credits_but_not_the_ram() {
        let mut c = running();
        command(&mut c, 3, &[0]);
        frame(&mut c, 0x0E, 0x0F);
        assert_eq!(c.credits(), 1);
        c.write(12, 0x7);
        c.set_reset(true);
        assert_eq!(c.credits(), 0);
        assert_eq!(c.read(12) & 0xF, 0x7);
    }
}
