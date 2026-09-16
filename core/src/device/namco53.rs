use phosphor_macros::Saveable;

/// Namco 53XX custom chip — DIP switch reader.
///
/// In hardware, this is a Fujitsu MB8843 MCU that reads DIP switch
/// settings and returns them as a sequence of nibbles. We emulate the
/// external behavior directly.
///
/// Returns 2 bytes per read cycle:
///   [DSWA, DSWB]
///
/// The real MB8843 firmware reads R0-R3 (DIP switch nibbles) and packs
/// pairs into full bytes via the O port. Each Z80 read returns one
/// complete DIP switch byte, cycling between DSWA and DSWB.
#[derive(Saveable)]
#[save_version(1)]
pub struct Namco53 {
    /// Byte sequence counter (0-1).
    pub read_index: u8,
}

impl Namco53 {
    pub fn new() -> Self {
        Self { read_index: 0 }
    }

    /// Read the next DIP switch nibble.
    /// `dswa` and `dswb` are the current DIP switch byte values.
    pub fn read(&mut self, dswa: u8, dswb: u8) -> u8 {
        let idx = self.read_index;
        self.read_index = (self.read_index + 1) % 2;

        // The real MB8843 firmware packs two R-port nibbles per IRQ:
        //   IRQ 0: R0 (low) | R1 (high) << 4 = DSWA
        //   IRQ 1: R2 (low) | R3 (high) << 4 = DSWB
        match idx {
            0 => dswa,
            1 => dswb,
            _ => unreachable!(),
        }
    }

    pub fn reset(&mut self) {
        self.read_index = 0;
    }
}

impl Default for Namco53 {
    fn default() -> Self {
        Self::new()
    }
}

impl super::Device for Namco53 {
    fn name(&self) -> &'static str {
        "Namco 53XX"
    }
    fn reset(&mut self) {
        self.reset();
    }
}

use crate::core::debug::{DebugRegister, Debuggable};

impl Debuggable for Namco53 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![DebugRegister {
            name: "READ_IDX",
            value: self.read_index as u64,
            width: 2,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successive_reads_alternate_between_the_two_dip_banks() {
        // The Z80 sees one byte per read and the chip cycles between them, so
        // a board that read once would get DSWA forever and never see DSWB.
        let mut c = Namco53::new();
        assert_eq!(c.read(0xA1, 0xB2), 0xA1);
        assert_eq!(c.read(0xA1, 0xB2), 0xB2);
        assert_eq!(c.read(0xA1, 0xB2), 0xA1, "the sequence wraps after two");
        assert_eq!(c.read(0xA1, 0xB2), 0xB2);
    }

    #[test]
    fn the_values_are_read_live_rather_than_latched() {
        // The MCU samples its R ports each cycle, so a DIP changed between
        // reads is visible on the next one. Latching at reset would freeze the
        // settings a service menu is meant to be able to change.
        let mut c = Namco53::new();
        assert_eq!(c.read(0x11, 0x22), 0x11);
        assert_eq!(c.read(0x33, 0x44), 0x44, "bank B, with the new value");
        assert_eq!(c.read(0x55, 0x66), 0x55);
    }

    #[test]
    fn the_sequence_index_is_the_whole_of_the_state() {
        let mut c = Namco53::new();
        assert_eq!(c.read_index, 0);
        c.read(0, 0);
        assert_eq!(c.read_index, 1);
        c.read(0, 0);
        assert_eq!(c.read_index, 0, "two reads is a full cycle");
    }

    #[test]
    fn reset_returns_the_sequence_to_the_first_bank() {
        // A reset mid-sequence has to resume at DSWA, or every later read is
        // off by one and the two banks are swapped for the rest of the run.
        let mut c = Namco53::new();
        c.read(0xA1, 0xB2);
        assert_eq!(c.read_index, 1);
        c.reset();
        assert_eq!(c.read_index, 0);
        assert_eq!(c.read(0xA1, 0xB2), 0xA1);
    }

    #[test]
    fn it_powers_up_ready_to_return_the_first_bank() {
        let mut c = Namco53::default();
        assert_eq!(c.read(0xA1, 0xB2), 0xA1);
    }
}
