//! MOS 6522 Versatile Interface Adapter (VIA) — register-file stub.
//!
//! This models enough of the 6522 for a host CPU to drive the two 8-bit ports
//! (each with a data-direction register) and to read/write the control and timer
//! registers without stalling. The interval timers (T1/T2), shift register, and
//! interrupt logic are **not** emulated — those offsets read back as a plain
//! register file, and [`irq`](Via6522::irq) is always low.
//!
//! That is sufficient for the Atari System 1 sound board's speech path, where
//! the 6522 is only a parallel bridge to the TMS5220: Port A carries the data /
//! status byte, and Port B carries the `/WS`, `/RS` strobes plus the chip's
//! `/READY` and `/INT` status. Board logic drives the input pins via
//! [`set_pa_input`](Via6522::set_pa_input) / [`set_pb_input`](Via6522::set_pb_input)
//! and reads the driven outputs via [`read_output_a`](Via6522::read_output_a) /
//! [`read_output_b`](Via6522::read_output_b). Full timer/IRQ behavior is a
//! follow-up if a game needs it.

use phosphor_macros::Saveable;

// 6522 register offsets (RS3:RS0).
const ORB: usize = 0x0; // Output/Input Register B
const ORA: usize = 0x1; // Output/Input Register A (with handshake)
const DDRB: usize = 0x2; // Data Direction Register B
const DDRA: usize = 0x3; // Data Direction Register A
const IFR: usize = 0xD; // Interrupt Flag Register
const IER: usize = 0xE; // Interrupt Enable Register
const ORA_NH: usize = 0xF; // Register A, no handshake

/// MOS 6522 VIA (register-file stub). Two ports with direction registers plus a
/// passive register file for the timer/control/interrupt space.
#[derive(Saveable)]
#[save_version(1)]
pub struct Via6522 {
    /// Raw register file: the port output latches (ORB/ORA), direction registers
    /// (DDRB/DDRA), and the timer/SR/ACR/PCR/IFR/IER registers (stored, not acted
    /// on).
    regs: [u8; 16],
    /// External input pins driven onto Port A / Port B by board logic; the bits
    /// where the matching DDR is 0 read back from here.
    input_a: u8,
    input_b: u8,
    /// Set when the CPU writes ORB — a one-shot notify for board logic that wants
    /// to react to a Port B command.
    port_b_written: bool,
}

impl Via6522 {
    pub fn new() -> Self {
        Self {
            regs: [0; 16],
            input_a: 0,
            input_b: 0,
            port_b_written: false,
        }
    }

    /// Reset to power-on: all registers cleared, all pins input.
    pub fn reset(&mut self) {
        self.regs = [0; 16];
        self.input_a = 0;
        self.input_b = 0;
        self.port_b_written = false;
    }

    /// Drive the Port A input pins (bits where DDRA=0 read back from here).
    pub fn set_pa_input(&mut self, data: u8) {
        self.input_a = data;
    }

    /// Drive the Port B input pins (bits where DDRB=0 read back from here).
    pub fn set_pb_input(&mut self, data: u8) {
        self.input_b = data;
    }

    /// The Port A output pins the CPU is driving (ORA masked by DDRA).
    pub fn read_output_a(&self) -> u8 {
        self.regs[ORA] & self.regs[DDRA]
    }

    /// The Port B output pins the CPU is driving (ORB masked by DDRB).
    pub fn read_output_b(&self) -> u8 {
        self.regs[ORB] & self.regs[DDRB]
    }

    /// Whether ORB was written since the last check (one-shot; clears on read).
    pub fn take_port_b_written(&mut self) -> bool {
        let written = self.port_b_written;
        self.port_b_written = false;
        written
    }

    /// IRQ output. The stub emulates no interrupt sources (no timers/SR/CxN),
    /// so this stays low unless a flag is forced into IFR directly.
    pub fn irq(&self) -> bool {
        self.regs[IFR] & self.regs[IER] & 0x7F != 0
    }

    /// Read a register (offset 0-15). Port reads mix the input pins (DDR=0 bits)
    /// with the output latch (DDR=1 bits); every other offset returns the raw
    /// register file.
    pub fn read(&mut self, offset: u16) -> u8 {
        match offset as usize & 0x0F {
            ORB => (self.input_b & !self.regs[DDRB]) | (self.regs[ORB] & self.regs[DDRB]),
            ORA | ORA_NH => (self.input_a & !self.regs[DDRA]) | (self.regs[ORA] & self.regs[DDRA]),
            IER => self.regs[IER] | 0x80, // bit 7 always reads set
            other => self.regs[other],
        }
    }

    /// Write a register (offset 0-15).
    pub fn write(&mut self, offset: u16, data: u8) {
        match offset as usize & 0x0F {
            ORB => {
                self.regs[ORB] = data;
                self.port_b_written = true;
            }
            ORA_NH => self.regs[ORA] = data, // aliases the handshake register
            IER => {
                // Bit 7 selects set (1) vs. clear (0) for the enables in bits 0-6.
                if data & 0x80 != 0 {
                    self.regs[IER] |= data & 0x7F;
                } else {
                    self.regs[IER] &= !(data & 0x7F);
                }
            }
            IFR => self.regs[IFR] &= !(data & 0x7F), // writing 1 clears a flag
            other => self.regs[other] = data,
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
    fn ifr_write_clears_flags_and_irq_stays_low() {
        let mut via = Via6522::new();
        // No emulated sources, so the IRQ line is low out of reset.
        assert!(!via.irq());
        // A forced flag + matching enable would assert; writing 1 to IFR clears it.
        via.write(IER as u16, 0x80 | 0x40);
        via.write(IFR as u16, 0x40); // clear (no-op here, but must not panic)
        assert!(!via.irq());
    }

    #[test]
    fn timer_and_control_offsets_are_a_plain_register_file() {
        let mut via = Via6522::new();
        for off in [0x4u16, 0x5, 0x6, 0x7, 0x8, 0x9, 0xA, 0xB, 0xC] {
            via.write(off, 0x30 + off as u8);
            assert_eq!(
                via.read(off),
                0x30 + off as u8,
                "offset {off:#x} round-trips"
            );
        }
    }

    #[test]
    fn save_load_round_trips() {
        let mut via = Via6522::new();
        via.write(DDRB as u16, 0xFF);
        via.write(ORB as u16, 0x99);
        via.set_pb_input(0x12);
        via.write(0xB, 0x40); // ACR

        let mut w = StateWriter::new();
        via.save_state(&mut w);
        let bytes = w.into_vec();

        let mut via2 = Via6522::new();
        let mut r = StateReader::new(&bytes);
        via2.load_state(&mut r).unwrap();
        assert_eq!(via2.read_output_b(), 0x99);
        assert_eq!(via2.read(0xB), 0x40);
    }
}
