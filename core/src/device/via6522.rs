//! MOS 6522 Versatile Interface Adapter (VIA).
//!
//! Two 8-bit ports with data-direction registers, the two interval timers T1 and
//! T2, the interrupt flag/enable registers, and the CA1/CA2/CB1/CB2 handshake
//! inputs. [`tick`](Via6522::tick) is one phase-2 clock, so a board calls it once
//! per cycle of the CPU the VIA shares its bus with.
//!
//! The shift register is stored but not clocked: SR reads and writes round-trip
//! and the SR interrupt flag is never raised on its own. Nothing in the registry
//! shifts through a 6522, and a shift register that silently produced the wrong
//! bits would be worse than one that visibly does nothing.
//!
//! # Timer counts
//!
//! Both counters decrement once per phase-2 clock, and the interrupt flag is
//! raised by the decrement that takes the counter from `0x0000` to `0xFFFF`.
//! Loading `N` therefore raises the flag `N + 1` cycles after the load, which is
//! the whole-cycle reading of the part's documented `N + 1.5`.
//!
//! T1 in free-run mode reloads from its latch on the cycle *after* the underflow
//! rather than on the underflow itself, so the counter spends one cycle at
//! `0xFFFF` and the repeat period is `N + 2`, not `N + 1`. That extra cycle is
//! the reload, and it is why a free-running T1 divides phase 2 by `N + 2`.
//!
//! # Ports
//!
//! Board logic drives the input pins with [`set_pa_input`](Via6522::set_pa_input)
//! and [`set_pb_input`](Via6522::set_pb_input) and reads the driven outputs with
//! [`read_output_a`](Via6522::read_output_a) and
//! [`read_output_b`](Via6522::read_output_b). On the Atari System 1 sound board's
//! speech path the 6522 is a parallel bridge to the TMS5220: Port A carries the
//! data/status byte and Port B the `/WS` and `/RS` strobes plus the chip's
//! `/READY` and `/INT` status.

use phosphor_macros::Saveable;

// 6522 register offsets (RS3:RS0).
const ORB: usize = 0x0; // Output/Input Register B
const ORA: usize = 0x1; // Output/Input Register A (with handshake)
const DDRB: usize = 0x2; // Data Direction Register B
const DDRA: usize = 0x3; // Data Direction Register A
const T1CL: usize = 0x4; // T1 counter low / latch low
const T1CH: usize = 0x5; // T1 counter high (write starts the timer)
const T1LL: usize = 0x6; // T1 latch low
const T1LH: usize = 0x7; // T1 latch high
const T2CL: usize = 0x8; // T2 counter low / latch low
const T2CH: usize = 0x9; // T2 counter high (write starts the timer)
const SR: usize = 0xA; // Shift Register
const ACR: usize = 0xB; // Auxiliary Control Register
const PCR: usize = 0xC; // Peripheral Control Register
const IFR: usize = 0xD; // Interrupt Flag Register
const IER: usize = 0xE; // Interrupt Enable Register
const ORA_NH: usize = 0xF; // Register A, no handshake

/// Interrupt flag and enable bits, shared by IFR and IER.
pub const INT_CA2: u8 = 0x01;
/// See [`INT_CA2`].
pub const INT_CA1: u8 = 0x02;
/// See [`INT_CA2`].
pub const INT_SR: u8 = 0x04;
/// See [`INT_CA2`].
pub const INT_CB2: u8 = 0x08;
/// See [`INT_CA2`].
pub const INT_CB1: u8 = 0x10;
/// See [`INT_CA2`].
pub const INT_T2: u8 = 0x20;
/// See [`INT_CA2`].
pub const INT_T1: u8 = 0x40;

// ACR bits.
const ACR_T2_COUNT_PB6: u8 = 0x20; // 0: count phase 2, 1: count PB6 falling edges
const ACR_T1_FREE_RUN: u8 = 0x40; // 0: one-shot, 1: reload and repeat
const ACR_T1_PB7: u8 = 0x80; // T1 drives PB7

/// MOS 6522 VIA: two ports, two interval timers, and the interrupt logic.
#[derive(Saveable)]
#[save_version(2)]
pub struct Via6522 {
    /// Port output latches and direction registers.
    or_a: u8,
    or_b: u8,
    ddr_a: u8,
    ddr_b: u8,
    /// External input pins driven onto Port A / Port B by board logic; the bits
    /// where the matching DDR is 0 read back from here.
    input_a: u8,
    input_b: u8,

    /// T1 counter, its reload latch, and its state.
    t1_counter: u16,
    t1_latch_lo: u8,
    t1_latch_hi: u8,
    /// One-shot arming. A one-shot T1 raises its flag once per T1C-H write; the
    /// counter keeps running afterwards so a later read still returns a value.
    t1_active: bool,
    /// The free-run reload takes the cycle after the underflow, which is what
    /// makes the repeat period `N + 2`.
    t1_reload: bool,
    /// The square wave T1 puts on PB7 when ACR bit 7 is set.
    t1_pb7: bool,

    /// T2 counter, its reload latch, and its one-shot arming. T2 has no
    /// free-run mode: it raises its flag once per T2C-H write in either the
    /// timed or the pulse-counting mode.
    t2_counter: u16,
    t2_latch_lo: u8,
    t2_latch_hi: u8,
    t2_active: bool,

    /// Stored, never clocked. See the module comment.
    sr: u8,
    acr: u8,
    pcr: u8,
    /// Interrupt flags, bits 0-6. Bit 7 is not stored: it is the OR of the
    /// enabled flags and is computed when IFR is read.
    ifr: u8,
    ier: u8,

    /// Last seen level of each handshake input, for edge detection.
    ca1: bool,
    ca2: bool,
    cb1: bool,
    cb2: bool,

    /// Set when the CPU writes ORB: a one-shot notify for board logic that wants
    /// to react to a Port B command.
    port_b_written: bool,
}

impl Via6522 {
    pub fn new() -> Self {
        Self {
            or_a: 0,
            or_b: 0,
            ddr_a: 0,
            ddr_b: 0,
            input_a: 0,
            input_b: 0,
            t1_counter: 0,
            t1_latch_lo: 0,
            t1_latch_hi: 0,
            t1_active: false,
            t1_reload: false,
            t1_pb7: false,
            t2_counter: 0,
            t2_latch_lo: 0,
            t2_latch_hi: 0,
            t2_active: false,
            sr: 0,
            acr: 0,
            pcr: 0,
            ifr: 0,
            ier: 0,
            ca1: false,
            ca2: false,
            cb1: false,
            cb2: false,
            port_b_written: false,
        }
    }

    /// Reset to power-on: all registers cleared, all pins input, both timers
    /// stopped.
    ///
    /// The handshake inputs come back low rather than high. Nothing holds them
    /// either way with no board attached, so a board that enables a CA1/CB1
    /// interrupt should drive the line to its idle level before doing so, or the
    /// first drive looks like an edge.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    // --- Ports ---

    /// Drive the Port A input pins (bits where DDRA=0 read back from here).
    pub fn set_pa_input(&mut self, data: u8) {
        self.input_a = data;
    }

    /// Drive the Port B input pins (bits where DDRB=0 read back from here).
    ///
    /// A falling edge on PB6 clocks T2 when it is in pulse-counting mode, so
    /// this is the entry point for that mode as well as an ordinary port write.
    pub fn set_pb_input(&mut self, data: u8) {
        let was_high = self.input_b & 0x40 != 0;
        self.input_b = data;
        if was_high && data & 0x40 == 0 {
            self.count_pb6();
        }
    }

    /// The Port A output pins the CPU is driving (ORA masked by DDRA).
    pub fn read_output_a(&self) -> u8 {
        self.or_a & self.ddr_a
    }

    /// The Port B output pins the CPU is driving (ORB masked by DDRB).
    ///
    /// With ACR bit 7 set, T1 owns PB7 and drives it regardless of DDRB.
    pub fn read_output_b(&self) -> u8 {
        let driven = self.or_b & self.ddr_b;
        if self.acr & ACR_T1_PB7 != 0 {
            (driven & 0x7F) | if self.t1_pb7 { 0x80 } else { 0 }
        } else {
            driven
        }
    }

    /// Whether ORB was written since the last check (one-shot; clears on read).
    pub fn take_port_b_written(&mut self) -> bool {
        let written = self.port_b_written;
        self.port_b_written = false;
        written
    }

    // --- Handshake inputs ---

    /// Drive CA1. An edge in the direction PCR bit 0 selects raises the CA1 flag.
    pub fn set_ca1(&mut self, state: bool) {
        if self.ca1 == state {
            return;
        }
        self.ca1 = state;
        if state == (self.pcr & 0x01 != 0) {
            self.ifr |= INT_CA1;
        }
    }

    /// Drive CA2. Ignored while PCR bit 3 makes CA2 an output.
    pub fn set_ca2(&mut self, state: bool) {
        if self.ca2 == state {
            return;
        }
        self.ca2 = state;
        if self.pcr & 0x08 != 0 {
            return;
        }
        let edge = if state { 0x04 } else { 0x00 };
        if self.pcr & 0x0C == edge {
            self.ifr |= INT_CA2;
        }
    }

    /// Drive CB1. An edge in the direction PCR bit 4 selects raises the CB1 flag.
    pub fn set_cb1(&mut self, state: bool) {
        if self.cb1 == state {
            return;
        }
        self.cb1 = state;
        if state == (self.pcr & 0x10 != 0) {
            self.ifr |= INT_CB1;
        }
    }

    /// Drive CB2. Ignored while PCR bit 7 makes CB2 an output.
    pub fn set_cb2(&mut self, state: bool) {
        if self.cb2 == state {
            return;
        }
        self.cb2 = state;
        if self.pcr & 0x80 != 0 {
            return;
        }
        let edge = if state { 0x40 } else { 0x00 };
        if self.pcr & 0xC0 == edge {
            self.ifr |= INT_CB2;
        }
    }

    // --- Timers ---

    /// Advance both timers by one phase-2 clock.
    pub fn tick(&mut self) {
        self.tick_t1();
        if self.acr & ACR_T2_COUNT_PB6 == 0 {
            self.count_t2();
        }
    }

    fn t1_latch(&self) -> u16 {
        u16::from(self.t1_latch_hi) << 8 | u16::from(self.t1_latch_lo)
    }

    fn t2_latch(&self) -> u16 {
        u16::from(self.t2_latch_hi) << 8 | u16::from(self.t2_latch_lo)
    }

    fn tick_t1(&mut self) {
        // The free-run reload occupies its own cycle, so the counter is not
        // decremented on it. This is the +2 in the N+2 repeat period.
        if self.t1_reload {
            self.t1_reload = false;
            self.t1_counter = self.t1_latch();
            return;
        }

        let underflow = self.t1_counter == 0;
        self.t1_counter = self.t1_counter.wrapping_sub(1);
        if !underflow {
            return;
        }

        if self.acr & ACR_T1_FREE_RUN != 0 {
            self.t1_pb7 = !self.t1_pb7;
            self.t1_reload = true;
            self.ifr |= INT_T1;
        } else if self.t1_active {
            // One-shot: the flag is raised once per T1C-H write. The counter is
            // left running so a later read of T1C-L returns a live value.
            self.t1_active = false;
            self.t1_pb7 = true;
            self.ifr |= INT_T1;
        }
    }

    /// One T2 decrement, from either phase 2 or a PB6 edge.
    fn count_t2(&mut self) {
        let underflow = self.t2_counter == 0;
        self.t2_counter = self.t2_counter.wrapping_sub(1);
        if underflow && self.t2_active {
            self.t2_active = false;
            self.ifr |= INT_T2;
        }
    }

    fn count_pb6(&mut self) {
        if self.acr & ACR_T2_COUNT_PB6 != 0 {
            self.count_t2();
        }
    }

    // --- Interrupts ---

    /// IRQ output: any flag whose enable is set.
    pub fn irq(&self) -> bool {
        self.ifr & self.ier & 0x7F != 0
    }

    /// Clear CA1, and CA2 unless PCR puts it in independent-interrupt mode,
    /// where the flag survives a port access.
    fn clear_pa_int(&mut self) {
        let independent = self.pcr & 0x0A == 0x02;
        self.ifr &= !(INT_CA1 | if independent { 0 } else { INT_CA2 });
    }

    /// Clear CB1, and CB2 unless PCR puts it in independent-interrupt mode.
    fn clear_pb_int(&mut self) {
        let independent = self.pcr & 0xA0 == 0x20;
        self.ifr &= !(INT_CB1 | if independent { 0 } else { INT_CB2 });
    }

    // --- Register access ---

    fn port_a_value(&self) -> u8 {
        (self.input_a & !self.ddr_a) | (self.or_a & self.ddr_a)
    }

    fn port_b_value(&self) -> u8 {
        let value = (self.input_b & !self.ddr_b) | (self.or_b & self.ddr_b);
        if self.acr & ACR_T1_PB7 != 0 {
            (value & 0x7F) | if self.t1_pb7 { 0x80 } else { 0 }
        } else {
            value
        }
    }

    /// Read a register (offset 0-15). Port reads mix the input pins (DDR=0 bits)
    /// with the output latch (DDR=1 bits).
    pub fn read(&mut self, offset: u16) -> u8 {
        match offset as usize & 0x0F {
            ORB => {
                self.clear_pb_int();
                self.port_b_value()
            }
            ORA => {
                self.clear_pa_int();
                self.port_a_value()
            }
            // The no-handshake alias reads the same pins without touching the
            // CA1/CA2 flags, which is the whole point of the second address.
            ORA_NH => self.port_a_value(),
            DDRB => self.ddr_b,
            DDRA => self.ddr_a,
            T1CL => {
                self.ifr &= !INT_T1;
                self.t1_counter as u8
            }
            T1CH => (self.t1_counter >> 8) as u8,
            T1LL => self.t1_latch_lo,
            T1LH => self.t1_latch_hi,
            T2CL => {
                self.ifr &= !INT_T2;
                self.t2_counter as u8
            }
            T2CH => (self.t2_counter >> 8) as u8,
            SR => self.sr,
            ACR => self.acr,
            PCR => self.pcr,
            IFR => self.ifr | if self.irq() { 0x80 } else { 0 },
            IER => self.ier | 0x80, // bit 7 always reads set
            _ => unreachable!("offset is masked to 0-15"),
        }
    }

    /// Write a register (offset 0-15).
    pub fn write(&mut self, offset: u16, data: u8) {
        match offset as usize & 0x0F {
            ORB => {
                self.or_b = data;
                self.port_b_written = true;
                self.clear_pb_int();
            }
            ORA => {
                self.or_a = data;
                self.clear_pa_int();
            }
            ORA_NH => self.or_a = data,
            DDRB => self.ddr_b = data,
            DDRA => self.ddr_a = data,
            // A write to the counter-low address goes to the latch: the value
            // reaches the counter when the high byte is written.
            T1CL | T1LL => self.t1_latch_lo = data,
            T1LH => {
                self.t1_latch_hi = data;
                self.ifr &= !INT_T1;
            }
            T1CH => {
                self.t1_latch_hi = data;
                self.t1_counter = self.t1_latch();
                self.t1_reload = false;
                self.t1_active = true;
                self.t1_pb7 = false;
                self.ifr &= !INT_T1;
            }
            T2CL => self.t2_latch_lo = data,
            T2CH => {
                self.t2_latch_hi = data;
                self.t2_counter = self.t2_latch();
                self.t2_active = true;
                self.ifr &= !INT_T2;
            }
            SR => self.sr = data,
            ACR => self.acr = data,
            PCR => self.pcr = data,
            IFR => self.ifr &= !(data & 0x7F), // writing 1 clears a flag
            IER => {
                // Bit 7 selects set (1) vs. clear (0) for the enables in bits 0-6.
                if data & 0x80 != 0 {
                    self.ier |= data & 0x7F;
                } else {
                    self.ier &= !(data & 0x7F);
                }
            }
            _ => unreachable!("offset is masked to 0-15"),
        }
    }
}

impl Default for Via6522 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::save_state::{Saveable, StateReader, StateWriter};

    /// Write T1's latch and start it, returning the VIA ready to tick.
    fn start_t1(via: &mut Via6522, n: u16) {
        via.write(T1CL as u16, n as u8);
        via.write(T1CH as u16, (n >> 8) as u8);
    }

    fn start_t2(via: &mut Via6522, n: u16) {
        via.write(T2CL as u16, n as u8);
        via.write(T2CH as u16, (n >> 8) as u8);
    }

    /// Tick until the given flag appears, returning the cycle count. `None` if
    /// it never does within the budget.
    fn cycles_to_flag(via: &mut Via6522, flag: u8, budget: u32) -> Option<u32> {
        for n in 1..=budget {
            via.tick();
            if via.read(IFR as u16) & flag != 0 {
                return Some(n);
            }
        }
        None
    }

    #[test]
    fn port_reads_mix_input_pins_and_output_latch_by_ddr() {
        let mut via = Via6522::new();
        // DDRA: bits 0-3 output, 4-7 input.
        via.write(DDRA as u16, 0x0F);
        // CPU drives 0xA5 onto ORA; only the low nibble reaches the pins.
        via.write(ORA as u16, 0xA5);
        // Board drives 0x5A onto the input pins; only the high nibble is read.
        via.set_pa_input(0x5A);
        // Read = (input & !ddr) | (ora & ddr) = (0x5A & 0xF0) | (0xA5 & 0x0F).
        assert_eq!(via.read(ORA as u16), 0x50 | 0x05);
        // The no-handshake alias reads the same port.
        assert_eq!(via.read(ORA_NH as u16), 0x55);
        // read_output_a exposes just the driven bits.
        assert_eq!(via.read_output_a(), 0x05);
    }

    #[test]
    fn port_b_write_sets_the_one_shot_notify() {
        let mut via = Via6522::new();
        assert!(!via.take_port_b_written());
        via.write(ORB as u16, 0x42);
        assert!(
            via.take_port_b_written(),
            "ORB write flags a Port B command"
        );
        assert!(!via.take_port_b_written(), "flag is one-shot");
    }

    #[test]
    fn ier_sets_and_clears_by_bit7_and_reads_bit7_high() {
        let mut via = Via6522::new();
        via.write(IER as u16, 0x80 | 0x21); // set enables 0 and 5
        assert_eq!(via.read(IER as u16), 0x80 | 0x21);
        via.write(IER as u16, 0x01); // clear enable 0 (bit7=0)
        assert_eq!(via.read(IER as u16), 0x80 | 0x20);
    }

    #[test]
    fn t1_one_shot_flags_once_at_n_plus_one_cycles() {
        let mut via = Via6522::new();
        start_t1(&mut via, 100);
        // The flag lands on the decrement that takes the counter past zero,
        // which is the 101st tick after the load, not the 100th.
        assert_eq!(cycles_to_flag(&mut via, INT_T1, 200), Some(101));

        // One-shot: no second flag, however long it runs. Clear and wait out
        // more than another full period.
        via.write(IFR as u16, INT_T1);
        assert_eq!(
            cycles_to_flag(&mut via, INT_T1, 300),
            None,
            "a one-shot T1 flags once per T1C-H write"
        );

        // Rewriting T1C-H rearms it, and it fires on the same schedule.
        via.write(T1CH as u16, 0);
        assert_eq!(cycles_to_flag(&mut via, INT_T1, 200), Some(101));
    }

    #[test]
    fn t1_free_run_repeats_every_n_plus_two_cycles_and_toggles_pb7() {
        let mut via = Via6522::new();
        // ACR bit 6 free-run, bit 7 puts the square wave on PB7.
        via.write(ACR as u16, ACR_T1_FREE_RUN | ACR_T1_PB7);
        start_t1(&mut via, 10);

        // First flag on the same N+1 schedule as the one-shot.
        assert_eq!(cycles_to_flag(&mut via, INT_T1, 50), Some(11));
        let first_pb7 = via.read_output_b() & 0x80;

        // Every flag after that is one reload cycle further apart: N+2.
        for round in 0..4 {
            via.write(IFR as u16, INT_T1);
            assert_eq!(
                cycles_to_flag(&mut via, INT_T1, 50),
                Some(12),
                "free-run period is N+2 on repeat {round}"
            );
        }

        // Four more underflows since `first_pb7`, so PB7 is back where it was.
        // Checking the parity rather than each toggle is the point: the square
        // wave's period is two underflows, or 2(N+2) cycles of phase 2.
        assert_eq!(
            via.read_output_b() & 0x80,
            first_pb7,
            "an even number of underflows returns PB7 to the same level"
        );
        via.write(IFR as u16, INT_T1);
        assert_eq!(cycles_to_flag(&mut via, INT_T1, 50), Some(12));
        assert_ne!(
            via.read_output_b() & 0x80,
            first_pb7,
            "and an odd number inverts it, so PB7 toggles once per underflow"
        );
    }

    #[test]
    fn t1_counter_reads_back_while_running() {
        let mut via = Via6522::new();
        start_t1(&mut via, 0x0300);
        for _ in 0..0x100 {
            via.tick();
        }
        let lo = via.read(T1CL as u16);
        let hi = via.read(T1CH as u16);
        assert_eq!(u16::from(hi) << 8 | u16::from(lo), 0x0200);
        // The latch is readable separately and still holds the load value.
        assert_eq!(via.read(T1LL as u16), 0x00);
        assert_eq!(via.read(T1LH as u16), 0x03);
    }

    #[test]
    fn t2_one_shot_flags_once_and_rearms_only_on_a_t2ch_write() {
        let mut via = Via6522::new();
        start_t2(&mut via, 50);
        assert_eq!(cycles_to_flag(&mut via, INT_T2, 200), Some(51));

        via.write(IFR as u16, INT_T2);
        assert_eq!(
            cycles_to_flag(&mut via, INT_T2, 200),
            None,
            "T2 has no free-run mode"
        );

        via.write(T2CH as u16, 0);
        assert_eq!(cycles_to_flag(&mut via, INT_T2, 200), Some(51));
    }

    #[test]
    fn t2_pulse_mode_counts_pb6_edges_and_ignores_phase_two() {
        let mut via = Via6522::new();
        via.write(ACR as u16, ACR_T2_COUNT_PB6);
        via.set_pb_input(0x40); // PB6 idle high
        start_t2(&mut via, 3);

        // Phase 2 no longer moves it.
        for _ in 0..100 {
            via.tick();
        }
        assert_eq!(
            via.read(IFR as u16) & INT_T2,
            0,
            "phase 2 does not clock T2"
        );

        // Four falling edges: three to reach zero, the fourth to underflow.
        for edge in 1..=4 {
            via.set_pb_input(0x00);
            via.set_pb_input(0x40);
            let flagged = via.read(IFR as u16) & INT_T2 != 0;
            assert_eq!(
                flagged,
                edge == 4,
                "N=3 flags on the 4th falling edge, not the {edge}th"
            );
        }

        // A rising edge alone does nothing.
        via.write(IFR as u16, INT_T2);
        start_t2(&mut via, 1);
        via.set_pb_input(0x00);
        via.set_pb_input(0x40);
        via.set_pb_input(0x40);
        assert_eq!(
            via.read(IFR as u16) & INT_T2,
            0,
            "only falling edges clock T2"
        );
    }

    #[test]
    fn irq_follows_the_enable_and_the_flag_together() {
        let mut via = Via6522::new();
        assert!(!via.irq(), "no sources out of reset");

        // Flag with no enable: no IRQ, but the flag is visible in IFR.
        start_t1(&mut via, 5);
        assert_eq!(cycles_to_flag(&mut via, INT_T1, 20), Some(6));
        assert!(!via.irq(), "a flag without its enable does not drive IRQ");
        assert_eq!(
            via.read(IFR as u16) & 0x80,
            0,
            "IFR bit 7 mirrors the IRQ line, so it is clear too"
        );

        // Enabling it asserts, without the flag being touched.
        via.write(IER as u16, 0x80 | INT_T1);
        assert!(via.irq());
        assert_eq!(via.read(IFR as u16) & 0x80, 0x80, "IFR bit 7 follows");

        // Writing a 1 to the flag clears it and releases the line.
        via.write(IFR as u16, INT_T1);
        assert!(!via.irq());
    }

    #[test]
    fn reading_the_counter_low_byte_clears_the_timer_flag() {
        let mut via = Via6522::new();
        via.write(IER as u16, 0x80 | INT_T1 | INT_T2);

        start_t1(&mut via, 2);
        assert_eq!(cycles_to_flag(&mut via, INT_T1, 20), Some(3));
        assert!(via.irq());
        via.read(T1CL as u16);
        assert!(!via.irq(), "reading T1C-L clears the T1 flag");

        start_t2(&mut via, 2);
        assert_eq!(cycles_to_flag(&mut via, INT_T2, 20), Some(3));
        assert!(via.irq());
        via.read(T2CL as u16);
        assert!(!via.irq(), "reading T2C-L clears the T2 flag");
    }

    #[test]
    fn ca1_and_cb1_flag_on_the_edge_pcr_selects() {
        let mut via = Via6522::new();
        // PCR bit 0 = 0: CA1 is negative-edge. Bit 4 = 1: CB1 is positive-edge.
        via.write(PCR as u16, 0x10);

        via.set_ca1(true); // rising: wrong direction
        assert_eq!(via.read(IFR as u16) & INT_CA1, 0);
        via.set_ca1(false); // falling: flags
        assert_eq!(via.read(IFR as u16) & INT_CA1, INT_CA1);

        via.set_cb1(true); // rising: flags
        assert_eq!(via.read(IFR as u16) & INT_CB1, INT_CB1);
        via.write(IFR as u16, INT_CB1);
        via.set_cb1(false); // falling: wrong direction
        assert_eq!(via.read(IFR as u16) & INT_CB1, 0);

        // A level that does not change is not an edge.
        via.write(IFR as u16, 0x7F);
        via.set_ca1(false);
        via.set_ca1(false);
        assert_eq!(via.read(IFR as u16) & INT_CA1, 0);
    }

    #[test]
    fn a_port_a_access_clears_ca1_but_leaves_an_independent_ca2() {
        let mut via = Via6522::new();
        // PCR = 0x00: CA1 and CA2 both negative-edge inputs, CA2 not
        // independent. Both lines come back low from reset, so idle them high
        // first: driving a line to the level it already holds is not an edge,
        // which is the caveat on `reset`.
        via.set_ca1(true);
        via.set_ca2(true);
        assert_eq!(
            via.read(IFR as u16) & (INT_CA1 | INT_CA2),
            0,
            "the idle-high drive is the wrong direction for either flag"
        );
        via.set_ca1(false);
        via.set_ca2(false);
        assert_eq!(
            via.read(IFR as u16) & (INT_CA1 | INT_CA2),
            INT_CA1 | INT_CA2
        );
        via.read(ORA as u16);
        assert_eq!(
            via.read(IFR as u16) & (INT_CA1 | INT_CA2),
            0,
            "a handshake port read clears both"
        );

        // PCR = 0x02: CA2 independent negative edge. The flag now survives.
        let mut via = Via6522::new();
        via.write(PCR as u16, 0x02);
        via.set_ca1(true);
        via.set_ca2(true);
        via.set_ca1(false);
        via.set_ca2(false);
        via.read(ORA as u16);
        assert_eq!(
            via.read(IFR as u16) & (INT_CA1 | INT_CA2),
            INT_CA2,
            "independent CA2 survives a port access; CA1 still clears"
        );

        // And the no-handshake address clears nothing at all.
        let mut via = Via6522::new();
        via.set_ca1(true);
        via.set_ca1(false);
        via.read(ORA_NH as u16);
        assert_eq!(via.read(IFR as u16) & INT_CA1, INT_CA1);
    }

    #[test]
    fn ca2_and_cb2_are_ignored_while_pcr_makes_them_outputs() {
        let mut via = Via6522::new();
        // PCR bit 3 = CA2 output, bit 7 = CB2 output.
        via.write(PCR as u16, 0x88);
        via.set_ca2(false);
        via.set_cb2(false);
        assert_eq!(
            via.read(IFR as u16) & (INT_CA2 | INT_CB2),
            0,
            "an output pin does not raise an input flag"
        );
    }

    #[test]
    fn shift_register_round_trips_without_being_clocked() {
        let mut via = Via6522::new();
        via.write(SR as u16, 0x5A);
        assert_eq!(via.read(SR as u16), 0x5A);
        for _ in 0..1000 {
            via.tick();
        }
        assert_eq!(via.read(SR as u16), 0x5A, "the SR is stored, not shifted");
        assert_eq!(
            via.read(IFR as u16) & INT_SR,
            0,
            "and never raises its own flag"
        );
    }

    #[test]
    fn save_load_round_trips_a_timer_in_flight() {
        let mut via = Via6522::new();
        via.write(DDRB as u16, 0xFF);
        via.write(ORB as u16, 0x99);
        via.set_pb_input(0x12);
        via.write(ACR as u16, ACR_T1_FREE_RUN);
        via.write(IER as u16, 0x80 | INT_T1);
        start_t1(&mut via, 60);
        for _ in 0..20 {
            via.tick();
        }

        let mut w = StateWriter::new();
        via.save_state(&mut w);
        let bytes = w.into_vec();

        let mut via2 = Via6522::new();
        let mut r = StateReader::new(&bytes);
        via2.load_state(&mut r).unwrap();
        assert_eq!(via2.read_output_b(), 0x99);
        assert_eq!(via2.read(ACR as u16), ACR_T1_FREE_RUN);
        // The restored timer is mid-flight and fires on the cycle the original
        // would have: 41 more ticks to reach the 61st.
        assert_eq!(cycles_to_flag(&mut via2, INT_T1, 100), Some(41));
    }
}
