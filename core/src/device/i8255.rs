//! Intel 8255 Programmable Peripheral Interface (PPI).
//!
//! Three 8-bit ports — A, B, and C (the latter splittable into independently
//! directioned upper/lower nibbles) — plus a control register at offset 3.
//! Modelled on MAME's `machine/i8255.cpp`, but only **mode 0** (simple,
//! unlatched I/O) is implemented: that is all the SN76489A-era Sega boards
//! (Congo Bongo's sound CPU) use. Mode 1/2 handshaking is intentionally absent.
//!
//! ## Register map (offset, 2 bits)
//! | 0 | Port A | 1 | Port B | 2 | Port C | 3 | Control |
//!
//! ## Control word (bit 7 set = mode select)
//! - bit 4: Port A direction (1 = input)
//! - bit 3: Port C upper-nibble direction
//! - bit 1: Port B direction
//! - bit 0: Port C lower-nibble direction
//! - bits 6-5 / bit 2: group A / B mode — only mode 0 is honored here
//!
//! A control byte with bit 7 clear is a Port C bit set/reset (BSR): bits 3-1
//! select the bit, bit 0 is the new value.

use phosphor_macros::Saveable;

use crate::core::debug::{DebugRegister, Debuggable};

const MODE_SET: u8 = 0x80;
const PORT_A_INPUT: u8 = 0x10;
const PORT_C_UPPER_INPUT: u8 = 0x08;
const PORT_B_INPUT: u8 = 0x02;
const PORT_C_LOWER_INPUT: u8 = 0x01;

/// Reset control word: all ports input, mode 0 (the 8255 power-on state).
const RESET_CONTROL: u8 = 0x9b;

/// Intel 8255 PPI (mode 0 only).
#[derive(Saveable)]
pub struct I8255 {
    /// Mode-control word.
    control: u8,
    /// Output latches for ports A, B, C.
    output: [u8; 3],
    /// External input values for ports A, B, C (driven by the board).
    input: [u8; 3],
}

impl Default for I8255 {
    fn default() -> Self {
        Self::new()
    }
}

impl I8255 {
    pub fn new() -> Self {
        Self {
            control: RESET_CONTROL,
            output: [0; 3],
            input: [0; 3],
        }
    }

    /// Reset to the power-on state: all ports configured as inputs.
    pub fn reset(&mut self) {
        self.control = RESET_CONTROL;
        self.output = [0; 3];
        self.input = [0; 3];
    }

    fn port_a_is_input(&self) -> bool {
        self.control & PORT_A_INPUT != 0
    }
    fn port_b_is_input(&self) -> bool {
        self.control & PORT_B_INPUT != 0
    }
    fn port_c_lower_is_input(&self) -> bool {
        self.control & PORT_C_LOWER_INPUT != 0
    }
    fn port_c_upper_is_input(&self) -> bool {
        self.control & PORT_C_UPPER_INPUT != 0
    }

    /// Read a register by offset (0-3).
    pub fn read(&self, offset: u16) -> u8 {
        match offset & 0x03 {
            0 => {
                if self.port_a_is_input() {
                    self.input[0]
                } else {
                    self.output[0]
                }
            }
            1 => {
                if self.port_b_is_input() {
                    self.input[1]
                } else {
                    self.output[1]
                }
            }
            2 => {
                // Each nibble independently reads its input pin or output latch.
                let lower = if self.port_c_lower_is_input() {
                    self.input[2]
                } else {
                    self.output[2]
                } & 0x0f;
                let upper = if self.port_c_upper_is_input() {
                    self.input[2]
                } else {
                    self.output[2]
                } & 0xf0;
                upper | lower
            }
            _ => self.control,
        }
    }

    /// Write a register by offset (0-3).
    pub fn write(&mut self, offset: u16, data: u8) {
        match offset & 0x03 {
            // Data ports latch only when configured for output.
            0 => {
                if !self.port_a_is_input() {
                    self.output[0] = data;
                }
            }
            1 => {
                if !self.port_b_is_input() {
                    self.output[1] = data;
                }
            }
            2 => self.output[2] = data,
            _ => {
                if data & MODE_SET != 0 {
                    // Mode select: latch the control word; outputs reset to 0.
                    self.control = data;
                    self.output = [0; 3];
                } else {
                    // Port C bit set/reset.
                    let bit = (data >> 1) & 0x07;
                    if data & 1 != 0 {
                        self.output[2] |= 1 << bit;
                    } else {
                        self.output[2] &= !(1 << bit);
                    }
                }
            }
        }
    }

    /// Drive Port A's input pins (used when Port A is an input).
    pub fn set_port_a_input(&mut self, data: u8) {
        self.input[0] = data;
    }
    /// Drive Port B's input pins.
    pub fn set_port_b_input(&mut self, data: u8) {
        self.input[1] = data;
    }
    /// Drive Port C's input pins.
    pub fn set_port_c_input(&mut self, data: u8) {
        self.input[2] = data;
    }

    /// Current Port A output latch.
    pub fn read_output_a(&self) -> u8 {
        self.output[0]
    }
    /// Current Port B output latch.
    pub fn read_output_b(&self) -> u8 {
        self.output[1]
    }
    /// Current Port C output latch.
    pub fn read_output_c(&self) -> u8 {
        self.output[2]
    }
}

impl super::Device for I8255 {
    fn name(&self) -> &'static str {
        "i8255 PPI"
    }
    fn reset(&mut self) {
        self.reset();
    }
    fn read(&mut self, offset: u16) -> u8 {
        I8255::read(self, offset)
    }
    fn write(&mut self, offset: u16, data: u8) {
        I8255::write(self, offset, data);
    }
}

impl Debuggable for I8255 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "CTRL",
                value: self.control as u64,
                width: 8,
            },
            DebugRegister {
                name: "PA",
                value: self.read(0) as u64,
                width: 8,
            },
            DebugRegister {
                name: "PB",
                value: self.read(1) as u64,
                width: 8,
            },
            DebugRegister {
                name: "PC",
                value: self.read(2) as u64,
                width: 8,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::save_state::{Saveable, StateReader, StateWriter};

    /// Congo's PPI setup: A input, B output, C output (control 0x90).
    fn congo_config() -> I8255 {
        let mut ppi = I8255::new();
        ppi.write(3, 0x90);
        ppi
    }

    #[test]
    fn reset_is_all_inputs() {
        let ppi = I8255::new();
        assert_eq!(ppi.read(3), RESET_CONTROL);
        assert!(ppi.port_a_is_input() && ppi.port_b_is_input());
        assert!(ppi.port_c_lower_is_input() && ppi.port_c_upper_is_input());
    }

    #[test]
    fn input_port_reads_external_pins() {
        let mut ppi = congo_config();
        ppi.set_port_a_input(0x5a);
        assert_eq!(ppi.read(0), 0x5a, "port A returns the driven input");
        // Writing an input-configured port is ignored.
        ppi.write(0, 0xff);
        assert_eq!(ppi.read(0), 0x5a);
    }

    #[test]
    fn output_port_latches_and_reads_back() {
        let mut ppi = congo_config();
        ppi.write(1, 0x3c);
        assert_eq!(ppi.read(1), 0x3c, "output latch reads back");
        assert_eq!(ppi.read_output_b(), 0x3c);
        ppi.write(2, 0x0f);
        assert_eq!(ppi.read_output_c(), 0x0f);
    }

    #[test]
    fn port_c_bit_set_reset() {
        let mut ppi = congo_config();
        ppi.write(2, 0x00);
        // BSR control byte = (bit << 1) | state.
        ppi.write(3, 0b0000_0111); // set bit 3
        ppi.write(3, 0b0000_0001); // set bit 0
        assert_eq!(ppi.read_output_c(), 0b0000_1001);
        ppi.write(3, 0b0000_0110); // reset bit 3
        assert_eq!(ppi.read_output_c(), 0b0000_0001);
    }

    #[test]
    fn port_c_split_direction() {
        let mut ppi = I8255::new();
        // A out, B out, C lower input, C upper output (control: bit0=1 only).
        ppi.write(3, MODE_SET | PORT_C_LOWER_INPUT);
        ppi.set_port_c_input(0x0a); // lower nibble pins = 0xA
        ppi.write(2, 0x50); // latch (upper nibble 5 drives out; lower ignored on read)
        let v = ppi.read(2);
        assert_eq!(v & 0xf0, 0x50, "upper nibble from output latch");
        assert_eq!(v & 0x0f, 0x0a, "lower nibble from input pins");
    }

    #[test]
    fn mode_set_clears_outputs() {
        let mut ppi = congo_config();
        ppi.write(1, 0xff);
        ppi.write(2, 0xff);
        ppi.write(3, 0x90); // re-issue mode word
        assert_eq!(ppi.read_output_b(), 0);
        assert_eq!(ppi.read_output_c(), 0);
    }

    #[test]
    fn save_load_round_trip() {
        let mut ppi = congo_config();
        ppi.set_port_a_input(0x42);
        ppi.write(1, 0x99);
        ppi.write(2, 0x5a);

        let mut w = StateWriter::new();
        ppi.save_state(&mut w);
        let bytes = w.into_vec();

        let mut restored = I8255::new();
        let mut r = StateReader::new(&bytes);
        restored.load_state(&mut r).unwrap();

        assert_eq!(restored.read(3), ppi.read(3));
        assert_eq!(restored.read(0), 0x42);
        assert_eq!(restored.read_output_b(), 0x99);
        assert_eq!(restored.read_output_c(), 0x5a);
    }
}
