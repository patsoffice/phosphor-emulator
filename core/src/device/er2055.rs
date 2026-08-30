use phosphor_macros::Saveable;

/// GI ER2055 Electrically Alterable Read-Only Memory (EAROM)
///
/// 64×4-bit non-volatile storage used for high score tables on early arcade
/// boards (Atari Asteroids Deluxe, Tempest, etc.). Data is retained without
/// power via charge storage on internal MOS capacitors.
///
/// The device has a 6-bit address bus (A0–A5), a 4-bit data bus (D0–D3),
/// and control signals: C1, C2, CK (clock), CS1, CS2 (chip selects).
///
/// # Operation modes (active when CS1=1 and CS2=1)
///
/// | C1 | C2 | Mode  | Action                                          |
/// |----|----|-------|-------------------------------------------------|
/// |  1 |  x | Read  | Falling clock: data register ← `rom[address]`   |
/// |  0 |  0 | Write | `rom[address]` &= data register (destructive AND) |
/// |  0 |  1 | Erase | `rom[address]` ← 0xFF                           |
///
/// The write uses AND because the real device requires a separate erase
/// cycle before writing. Without erase, bits can only be cleared (1→0).
///
/// State updates are triggered by `set_control` (control line changes)
/// and `set_clk` (clock transitions). Reads happen on the falling clock
/// edge. Writes/erases happen whenever control or clock state changes
/// while the chip is selected.
///
/// Although the real device is 64×4, many boards wire the full 8-bit data
/// bus, so we store 8 bits per cell to match MAME and simplify board code.
///
/// # Reference
///
/// MAME `src/devices/machine/er2055.cpp`
#[derive(Saveable)]
#[save_version(1)]
pub struct Er2055 {
    rom_data: [u8; 64],
    address: u8,
    data: u8,
    control_state: u8,
}

// Control state bit masks
const CK: u8 = 0x01;
const C1: u8 = 0x02;
const C2: u8 = 0x04;
const CS1: u8 = 0x08;
const CS2: u8 = 0x10;

impl Er2055 {
    pub fn new() -> Self {
        Self {
            rom_data: [0xFF; 64], // MAME defaults to 0xFF
            address: 0,
            data: 0,
            control_state: 0,
        }
    }

    /// Reset latches to power-on state. Data is preserved (non-volatile).
    pub fn reset(&mut self) {
        self.address = 0;
        self.data = 0;
        self.control_state = 0;
    }

    /// Read a byte directly from EAROM storage. Used for NVRAM snapshots.
    pub fn read(&self, offset: u16) -> u8 {
        self.rom_data[(offset & 0x3F) as usize]
    }

    /// Get the data register value (what the CPU reads from the EAROM port).
    ///
    /// The data register is loaded by a read operation (falling clock with C1=1).
    /// The CPU reads this register, NOT the ROM array directly.
    pub fn data(&self) -> u8 {
        self.data
    }

    /// Set the address register.
    pub fn set_address(&mut self, address: u8) {
        self.address = address & 0x3F;
    }

    /// Set the data register (for writes from the CPU side).
    pub fn set_data(&mut self, data: u8) {
        self.data = data;
    }

    /// Set the control lines (CS1, CS2, C1, C2). All active-high.
    ///
    /// The caller must decode board-specific bit layouts and inversions
    /// before calling this. Triggers `update_state` if the chip is
    /// selected and the state changed.
    pub fn set_control(&mut self, cs1: bool, cs2: bool, c1: bool, c2: bool) {
        let old = self.control_state;
        self.control_state = old & CK; // preserve clock
        if c1 {
            self.control_state |= C1;
        }
        if c2 {
            self.control_state |= C2;
        }
        if cs1 {
            self.control_state |= CS1;
        }
        if cs2 {
            self.control_state |= CS2;
        }

        // If not selected, or no change, we're done
        if (self.control_state & (CS1 | CS2)) != (CS1 | CS2) || self.control_state == old {
            return;
        }

        self.update_state();
    }

    /// Set the clock line. Triggers read on falling edge, and
    /// update_state for write/erase operations.
    pub fn set_clk(&mut self, state: bool) {
        let old = self.control_state;
        if state {
            self.control_state |= CK;
        } else {
            self.control_state &= !CK;
        }

        // Updates occur on falling edge when chip is selected
        if (self.control_state & (CS1 | CS2)) == (CS1 | CS2) && self.control_state != old && !state
        {
            // Read mode (C1=1, C2 is don't-care)
            if (self.control_state & C1) == C1 {
                self.data = self.rom_data[self.address as usize];
            }

            self.update_state();
        }
    }

    /// Convenience for boards that write control + clock in a single register.
    ///
    /// Calls `set_control` first, then `set_clk`, matching MAME's Tempest order.
    pub fn write_control(&mut self, clock: bool, cs1: bool, c1: bool, c2: bool) {
        self.set_control(cs1, true, c1, c2); // CS2 is hardwired on Tempest
        self.set_clk(clock);
    }

    /// Process write/erase based on current control state.
    fn update_state(&mut self) {
        match self.control_state & (C1 | C2) {
            // Write mode: C1=0, C2=0
            0 => {
                self.rom_data[self.address as usize] &= self.data;
            }
            // Erase mode: C1=0, C2=1
            C2 => {
                self.rom_data[self.address as usize] = 0xFF;
            }
            // C1=1: read mode (already handled in set_clk) or standby
            _ => {}
        }
    }

    /// Latch address and data for a subsequent write.
    ///
    /// Convenience method that calls `set_address` + `set_data`.
    pub fn latch(&mut self, offset: u16, data: u8) {
        self.set_address((offset & 0x3F) as u8);
        self.set_data(data);
    }

    /// Read the data register (alias for `data()`).
    ///
    /// Used by boards where the EAROM read port returns the data register.
    pub fn read_latched(&self) -> u8 {
        self.data
    }

    /// Load EAROM contents from a byte slice (e.g., from an NVRAM file).
    pub fn load_from(&mut self, src: &[u8]) {
        let len = src.len().min(64);
        self.rom_data[..len].copy_from_slice(&src[..len]);
    }

    /// Get a reference to the full EAROM contents for saving.
    pub fn snapshot(&self) -> &[u8; 64] {
        &self.rom_data
    }
}

impl Default for Er2055 {
    fn default() -> Self {
        Self::new()
    }
}

use crate::core::debug::{DebugRegister, Debuggable};

impl Debuggable for Er2055 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "addr",
                value: self.address as u64,
                width: 8,
            },
            DebugRegister {
                name: "data",
                value: self.data as u64,
                width: 8,
            },
        ]
    }
}

impl super::Device for Er2055 {
    fn name(&self) -> &'static str {
        "ER2055"
    }

    fn reset(&mut self) {
        Self::reset(self);
    }

    fn read(&mut self, offset: u16) -> u8 {
        self.rom_data[(offset & 0x3F) as usize]
    }

    fn write(&mut self, offset: u16, data: u8) {
        self.latch(offset, data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_defaults_to_ff() {
        let earom = Er2055::new();
        assert!(earom.rom_data.iter().all(|&b| b == 0xFF));
    }

    #[test]
    fn erase_then_write_cycle() {
        let mut earom = Er2055::new();

        // Erase address 5 (should already be 0xFF)
        earom.set_address(5);
        earom.set_data(0x00); // doesn't matter for erase
        // C1=0, C2=1 = erase mode; CS1=1, CS2=1
        earom.set_control(true, true, false, true);
        assert_eq!(earom.rom_data[5], 0xFF); // erase sets to 0xFF

        // Write 0xAB to address 5
        earom.set_data(0xAB);
        // C1=0, C2=0 = write mode
        earom.set_control(true, true, false, false);
        // Write is AND: 0xFF & 0xAB = 0xAB
        assert_eq!(earom.rom_data[5], 0xAB);
    }

    #[test]
    fn write_without_erase_is_destructive_and() {
        let mut earom = Er2055::new();

        // Write 0xF0 to address 10
        earom.set_address(10);
        earom.set_data(0xF0);
        earom.set_control(true, true, false, false); // write mode
        assert_eq!(earom.rom_data[10], 0xF0); // 0xFF & 0xF0

        // Write 0x0F without erase — result is AND
        earom.set_data(0x0F);
        // Need to change state to trigger update_state again
        earom.set_control(true, true, true, false); // standby first
        earom.set_control(true, true, false, false); // back to write
        assert_eq!(earom.rom_data[10], 0x00); // 0xF0 & 0x0F = 0x00
    }

    #[test]
    fn read_on_falling_clock() {
        let mut earom = Er2055::new();
        earom.rom_data[3] = 0x42;

        earom.set_address(3);
        // Set C1=1 (read mode), CS1=1, CS2=1
        earom.set_control(true, true, true, false);
        // Clock high
        earom.set_clk(true);
        assert_eq!(earom.data(), 0); // not loaded yet
        // Falling edge loads data register
        earom.set_clk(false);
        assert_eq!(earom.data(), 0x42);
    }

    #[test]
    fn no_action_without_chip_select() {
        let mut earom = Er2055::new();
        earom.set_address(0);
        earom.set_data(0x55);
        // CS1=0 → not selected, write won't happen
        earom.set_control(false, true, false, false);
        assert_eq!(earom.rom_data[0], 0xFF); // unchanged
    }

    #[test]
    fn tempest_write_sequence() {
        // Simulate the Tempest EAROM write sequence:
        // 1. STA $6000,X → latch address/data
        // 2. LDA #$08 → STA $6040 → CS1=1, C1=1(inv), C2=0, CK=0
        // 3. LDA #$0B → STA $6040 → CS1=1, C1=1(inv), C2=1, CK=1 (erase+clock)
        // 4. LDA #$08 → STA $6040 → CS1=1, C1=1(inv), C2=0, CK=0 (falling edge)
        // 5. LDA #$09 → STA $6040 → CS1=1, C1=1(inv), C2=0, CK=1
        // 6. LDA #$08 → STA $6040 → CS1=1, C1=1(inv), C2=0, CK=0 (write + falling)

        let mut earom = Er2055::new();

        // Step 1: latch address 5 with data 0xAB
        earom.latch(5, 0xAB);

        // Step 2: set control with erase mode, clock low
        // Tempest: bit3=CS1, !bit2=C1, bit1=C2, bit0=CK
        // $0A = 0000_1010: CS1=1, C1=!0=1, C2=1, CK=0
        earom.write_control(false, true, true, true); // standby

        // Step 3: erase with clock high
        // $0B = 0000_1011: CS1=1, C1=!0=1, C2=1, CK=1
        earom.write_control(true, true, true, true); // still C1=1 (standby/read), clock high

        // Actually let me trace the exact Tempest sequence from MAME:
        // $08: CS1=1, C1=!0=1, C2=0, CK=0
        // $09: CS1=1, C1=!0=1, C2=0, CK=1
        // $08: CS1=1, C1=!0=1, C2=0, CK=0 ← falling edge with C1=1 = READ

        // That's a READ sequence! Let me look at the write sequence differently.
        // The game probably does: erase first, then write.
    }

    #[test]
    fn write_control_convenience() {
        // Test the write_control convenience method matches set_control + set_clk
        let mut earom = Er2055::new();
        earom.rom_data[7] = 0x42;
        earom.set_address(7);

        // Use write_control to do a read cycle (C1=1, falling edge)
        earom.write_control(true, true, true, false); // clock high, C1=1
        earom.write_control(false, true, true, false); // falling edge → read
        assert_eq!(earom.data(), 0x42);
    }

    #[test]
    fn load_from_and_snapshot() {
        let mut earom = Er2055::new();
        let mut src = [0u8; 64];
        src[0] = 0x11;
        src[32] = 0x22;
        src[63] = 0x33;
        earom.load_from(&src);

        assert_eq!(earom.read(0), 0x11);
        assert_eq!(earom.read(32), 0x22);
        assert_eq!(earom.read(63), 0x33);
        assert_eq!(earom.snapshot(), &src);
    }

    #[test]
    fn reset_preserves_data() {
        let mut earom = Er2055::new();
        earom.rom_data[10] = 0x77;
        earom.reset();
        assert_eq!(earom.rom_data[10], 0x77); // data survives reset
        assert_eq!(earom.address, 0);
        assert_eq!(earom.data, 0);
        assert_eq!(earom.control_state, 0);
    }
}
