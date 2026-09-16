use phosphor_macros::Saveable;

/// 74LS259 8-bit addressable latch.
///
/// Address lines A0-A2 select which output bit to set/clear.
/// One data line (typically D0) provides the value. The caller extracts
/// the relevant data bit before calling [`write()`](OutputLatch::write).
#[derive(Default, Saveable)]
#[save_version(1)]
pub struct OutputLatch {
    value: u8,
}

impl OutputLatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Read the full 8-bit latch state.
    pub fn value(&self) -> u8 {
        self.value
    }

    /// Set or clear output `bit`. Returns the previous latch state (useful for
    /// edge detection on specific bits).
    ///
    /// The index is taken modulo eight, because the part has three address
    /// lines and cannot see a fourth. That is the hardware's own behavior and
    /// it also removes a panic: `1 << bit` on a `u8` overflows for an index of
    /// eight or more, which is a debug-build abort and a silent zero in
    /// release. A caller derives this index from an address, so a decode that
    /// let a higher bit through would have found that edge rather than the
    /// mirror the board actually implements.
    pub fn write(&mut self, bit: u8, data: bool) -> u8 {
        let old = self.value;
        let mask = 1u8 << (bit & 7);
        if data {
            self.value |= mask;
        } else {
            self.value &= !mask;
        }
        old
    }

    /// Test whether output `bit` is set, under the same three-line decode.
    pub fn bit(&self, n: u8) -> bool {
        self.value & (1u8 << (n & 7)) != 0
    }

    /// Reset all outputs to zero (active-low clear).
    pub fn reset(&mut self) {
        self.value = 0;
    }
}

impl super::Device for OutputLatch {
    fn name(&self) -> &'static str {
        "74LS259"
    }
    fn reset(&mut self) {
        self.reset();
    }
}

use crate::core::debug::{DebugRegister, Debuggable};

impl Debuggable for OutputLatch {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![DebugRegister {
            name: "VALUE",
            value: self.value as u64,
            width: 8,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_address_selects_its_own_output() {
        let mut l = OutputLatch::new();
        for bit in 0..8u8 {
            l.write(bit, true);
            assert_eq!(l.value(), 1 << bit, "bit {bit} alone");
            assert!(l.bit(bit));
            l.write(bit, false);
            assert_eq!(l.value(), 0);
        }
    }

    #[test]
    fn writing_one_output_leaves_the_others_alone() {
        // The point of an addressable latch: each write addresses one bit and
        // the other seven hold. A device that rewrote the whole byte would
        // pass a single-bit test and fail here.
        let mut l = OutputLatch::new();
        l.write(0, true);
        l.write(3, true);
        l.write(7, true);
        assert_eq!(l.value(), 0b1000_1001);
        l.write(3, false);
        assert_eq!(l.value(), 0b1000_0001);
    }

    #[test]
    fn a_write_returns_the_state_before_it() {
        // Callers use this for edge detection on a specific line, so it has to
        // be the value from before the write and not after.
        let mut l = OutputLatch::new();
        assert_eq!(l.write(2, true), 0b0000_0000);
        assert_eq!(
            l.write(2, true),
            0b0000_0100,
            "an idempotent write still reports"
        );
        assert_eq!(l.write(5, true), 0b0000_0100);
        assert_eq!(l.value(), 0b0010_0100);
    }

    #[test]
    fn the_address_is_three_bits_wide() {
        // A0-A2 is all the part has, so index 8 is index 0 again. Before this
        // was masked, `1 << 8` on a u8 panicked in a debug build.
        let mut l = OutputLatch::new();
        l.write(8, true);
        assert_eq!(l.value(), 0b0000_0001, "bit 8 mirrors bit 0");
        assert!(l.bit(8));
        l.write(0xFF, true);
        assert_eq!(l.value(), 0b1000_0001, "bit 255 mirrors bit 7");
    }

    #[test]
    fn reset_clears_every_output() {
        let mut l = OutputLatch::new();
        for bit in 0..8u8 {
            l.write(bit, true);
        }
        assert_eq!(l.value(), 0xFF);
        l.reset();
        assert_eq!(l.value(), 0, "the clear line takes every output low");
        for bit in 0..8u8 {
            assert!(!l.bit(bit));
        }
    }

    #[test]
    fn it_powers_up_with_every_output_low() {
        assert_eq!(OutputLatch::new().value(), 0);
    }
}
