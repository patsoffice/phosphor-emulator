use phosphor_macros::Saveable;

/// Xicor X2212 256×4-bit "NOVRAM" — static RAM shadowed by non-volatile EEPROM.
///
/// Used for the high-score table on Atari I, Robot. The device is a 256-nibble
/// static RAM that the CPU reads and writes directly, backed by a parallel
/// 256-nibble EEPROM array. Two control lines move data between the two:
///
/// - **RECALL** copies the EEPROM into the live SRAM (load saved data).
/// - **STORE** copies the live SRAM into the EEPROM (commit data to NVRAM).
///
/// Only the low nibble of each cell is significant; the high nibble reads back
/// as open bus (0xF on this board). Both edges are modelled as rising-edge
/// triggered, matching MAME `src/devices/machine/x2212.cpp`.
///
/// For NVRAM file persistence the emulator snapshots the live SRAM (I, Robot
/// wires the X2212 with auto-save, so the saved image always reflects SRAM);
/// [`load_nvram`](Self::load_nvram) restores it into both arrays so a later
/// RECALL keeps the data.
#[derive(Saveable)]
#[save_version(1)]
pub struct X2212 {
    /// Live static RAM (low nibble significant).
    sram: [u8; 256],
    /// Non-volatile shadow array.
    eeprom: [u8; 256],
    /// Last STORE line state (rising edge triggers a store).
    store_line: bool,
    /// Last RECALL line state (rising edge triggers a recall).
    recall_line: bool,
}

impl Default for X2212 {
    fn default() -> Self {
        Self::new()
    }
}

impl X2212 {
    pub fn new() -> Self {
        Self {
            sram: [0xFF; 256],
            eeprom: [0xFF; 256],
            store_line: false,
            recall_line: false,
        }
    }

    /// Reset the control lines to their power-on state. Stored data is
    /// non-volatile and preserved.
    pub fn reset(&mut self) {
        self.store_line = false;
        self.recall_line = false;
    }

    /// CPU read: low nibble from SRAM, high nibble floats to open bus (0xF).
    /// The X2212 is a 256x**4** device: it drives D0-D3 only, and D4-D7 are
    /// left to whatever the bus floats to. That is modelled as 0, matching
    /// MAME's `(m_sram[offset] & 0x0f) | (space.unmap() & 0xf0)` with the
    /// default unmap value the Atari drivers use.
    ///
    /// The upper nibble is not cosmetic: Star Wars reads the NVRAM and shifts
    /// the value left, so forcing D4-D7 high (as this used to) changes the
    /// result the game computes rather than being masked away.
    pub fn read(&self, offset: u16) -> u8 {
        self.sram[(offset & 0xFF) as usize] & 0x0F
    }

    /// CPU write: only the low nibble is stored.
    pub fn write(&mut self, offset: u16, data: u8) {
        self.sram[(offset & 0xFF) as usize] = data & 0x0F;
    }

    /// Drive the STORE line. A rising edge commits live SRAM to the EEPROM.
    pub fn store(&mut self, state: bool) {
        if state && !self.store_line {
            self.eeprom = self.sram;
        }
        self.store_line = state;
    }

    /// Drive the RECALL line. A rising edge loads the EEPROM into live SRAM.
    pub fn recall(&mut self, state: bool) {
        if state && !self.recall_line {
            self.sram = self.eeprom;
        }
        self.recall_line = state;
    }

    /// Battery-backed contents for an NVRAM snapshot (the live SRAM).
    pub fn nvram(&self) -> &[u8] {
        &self.sram
    }

    /// Restore battery-backed contents from a previous snapshot into both the
    /// live SRAM and the EEPROM shadow.
    pub fn load_nvram(&mut self, data: &[u8]) {
        let len = data.len().min(self.sram.len());
        self.sram[..len].copy_from_slice(&data[..len]);
        self.eeprom[..len].copy_from_slice(&data[..len]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The device is 256x4: a write keeps only D0-D3, and a read drives only
    /// D0-D3, leaving D4-D7 to the bus. This used to assert `0xF5` — the
    /// upper nibble forced high — which is not what the part does and which
    /// changed what Star Wars computed from its NVRAM (it shifts the value
    /// left rather than masking it).
    #[test]
    fn write_keeps_only_low_nibble_and_read_drives_only_low_nibble() {
        let mut nv = X2212::new();
        nv.write(0x10, 0xA5);
        assert_eq!(nv.read(0x10), 0x05);
    }

    #[test]
    fn store_then_recall_round_trips_through_eeprom() {
        let mut nv = X2212::new();
        nv.write(0x00, 0x0C);
        // Rising edge commits SRAM -> EEPROM.
        nv.store(true);
        // Clobber live SRAM.
        nv.write(0x00, 0x03);
        assert_eq!(nv.read(0x00) & 0x0F, 0x03);
        // Rising edge of RECALL restores EEPROM -> SRAM.
        nv.recall(false);
        nv.recall(true);
        assert_eq!(nv.read(0x00) & 0x0F, 0x0C);
    }

    #[test]
    fn store_requires_a_rising_edge() {
        let mut nv = X2212::new();
        nv.store(true); // commit 0xF nibbles
        nv.write(0x05, 0x07);
        // Line already high: no new store, EEPROM still holds 0xF.
        nv.store(true);
        nv.recall(false);
        nv.recall(true);
        assert_eq!(nv.read(0x05) & 0x0F, 0x0F);
    }

    #[test]
    fn load_nvram_populates_sram_and_eeprom() {
        let mut nv = X2212::new();
        let data: Vec<u8> = (0..=255).map(|i| (i & 0x0F) as u8).collect();
        nv.load_nvram(&data);
        assert_eq!(nv.read(0x42) & 0x0F, 0x02);
        // The shadow copy is loaded too: clobber SRAM, recall restores it.
        nv.write(0x42, 0x0F);
        nv.recall(false);
        nv.recall(true);
        assert_eq!(nv.read(0x42) & 0x0F, 0x02);
    }
}
