use crate::core::debug::{DebugRegister, Debuggable};
use crate::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use crate::cpu::mb88xx::{Mb88xx, Mb88xxVariant};

/// Namco 50XX custom chip — LLE (low-level emulation) using an MB8842 MCU.
///
/// The 50XX is a Fujitsu MB8842 4-bit microcontroller (2048-byte ROM,
/// 128-nibble RAM) programmed as a score-keeping / protection device. The
/// main CPU issues single-byte commands (reset scores, set bonus thresholds,
/// increment/decrement the current player's score by fixed amounts, select
/// the active player) and reads back the running BCD score plus status flags.
/// Xevious drives it only for a start-up protection handshake; Bosconian uses
/// its full scoring command set.
///
/// The chip sits behind the 06XX bus arbiter. Communication is nibble-oriented
/// across the MCU's I/O ports:
///   K port  ← command bits 7-4 (high nibble of the byte from the Z80)
///   R0 port ← command bits 3-0 (low nibble)
///   R2 port ← R/W line (bit 0): 1 = read cycle, 0 = write cycle
///   O port  → answer byte returned to the Z80
///
/// The 06XX asserts the MCU's /IRQ (via `set_chip_select`) once per transfer;
/// the firmware services the interrupt, reads the command off K/R0, and either
/// latches it or drives the answer onto the O port.
pub struct Namco50 {
    /// The MB8842 MCU running the 50XX firmware.
    pub mcu: Mb88xx,
}

impl Namco50 {
    pub fn new() -> Self {
        Self {
            mcu: Mb88xx::new(Mb88xxVariant::Mb8842),
        }
    }

    /// Load the 50XX firmware ROM (2048 bytes).
    pub fn load_rom(&mut self, data: &[u8]) {
        self.mcu.load_rom(data);
    }

    /// Advance the MCU by one machine cycle (call at the MCU's machine-cycle
    /// rate: external clock / 6).
    pub fn tick(&mut self) {
        self.mcu.execute_cycle();
    }

    /// Read the O port output — the answer byte for the Z80 via the 06XX.
    pub fn read(&self) -> u8 {
        self.mcu.read_o()
    }

    /// Latch a command byte from the Z80 (06XX data write with the 50XX
    /// selected). The high nibble appears on K, the low nibble on R0, matching
    /// how the firmware samples the two ports when it services the interrupt.
    pub fn write(&mut self, data: u8) {
        self.mcu.set_k(data >> 4);
        self.mcu.set_r_input(0, data & 0x0F);
    }

    /// Update the R/W line on R2. `read` is true for a read cycle (Z80 fetching
    /// an answer) and false for a write cycle (Z80 issuing a command).
    pub fn set_rw(&mut self, read: bool) {
        self.mcu.set_r_input(2, read as u8);
    }

    /// Drive the /IRQ (chip-select) line. The 06XX pulses this active for the
    /// selected chip on each timer toggle; the rising edge starts a transfer.
    pub fn set_chip_select(&mut self, active: bool) {
        self.mcu.set_irq(active);
    }

    /// Reset the MCU to power-on state. ROM content is preserved.
    pub fn reset(&mut self) {
        self.mcu.reset();
    }
}

impl Default for Namco50 {
    fn default() -> Self {
        Self::new()
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
        self.mcu.debug_registers()
    }
}

impl Saveable for Namco50 {
    fn save_state(&self, w: &mut StateWriter) {
        self.mcu.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.mcu.load_state(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_mb8842_core() {
        // The 50XX is a 2048-byte-ROM MB8842.
        let n50 = Namco50::new();
        assert_eq!(n50.mcu.variant(), Mb88xxVariant::Mb8842);
        assert_eq!(n50.mcu.peek_rom(0x7FF), 0); // full 2K address space
    }

    #[test]
    fn command_splits_across_k_and_r0() {
        // A command byte arrives high-nibble-on-K, low-nibble-on-R0.
        let mut n50 = Namco50::new();
        n50.write(0xAB);
        assert_eq!(n50.mcu.k_input, 0x0A);
        assert_eq!(n50.mcu.r_input[0], 0x0B);
    }

    #[test]
    fn rw_line_drives_r2() {
        let mut n50 = Namco50::new();
        n50.set_rw(true);
        assert_eq!(n50.mcu.r_input[2], 1);
        n50.set_rw(false);
        assert_eq!(n50.mcu.r_input[2], 0);
    }

    #[test]
    fn reset_clears_mcu_but_keeps_rom() {
        let mut n50 = Namco50::new();
        n50.load_rom(&[0x12; 2048]);
        n50.reset();
        assert_eq!(n50.mcu.pc, 0);
        assert_eq!(n50.mcu.peek_rom(0), 0x12); // ROM survives reset
    }
}
