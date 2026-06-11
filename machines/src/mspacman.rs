use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{InputReceiver, MachineCore, SaveState};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_macros::Saveable;

use crate::namco_pac::{self, NamcoPacBoard};
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};

// ---------------------------------------------------------------------------
// Ms. Pac-Man ROM definitions ("mspacman" MAME set)
// ---------------------------------------------------------------------------

/// Program ROM: same four Pac-Man base ROMs.
static MSPACMAN_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "pacman.6e",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xc1e6ab10],
        },
        RomEntry {
            name: "pacman.6f",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x1a6fb2d4],
        },
        RomEntry {
            name: "pacman.6h",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0xbcdd1beb],
        },
        RomEntry {
            name: "pacman.6j",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0x817d94e3],
        },
    ],
};

/// Auxiliary board ROMs (encrypted). Laid out matching MAME's address offsets
/// relative to 0x8000: U5 at +0x0000, U6 at +0x1000, U7 at +0x3000.
static MSPACMAN_AUX_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "u5",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0xf45fbbcd],
        },
        RomEntry {
            name: "u6",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0xa90e7000],
        },
        RomEntry {
            name: "u7",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0xc82cd714],
        },
    ],
};

/// GFX ROM: Ms. Pac-Man specific character/sprite graphics.
static MSPACMAN_GFX_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "5e",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x5c281d01],
        },
        RomEntry {
            name: "5f",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x615af909],
        },
    ],
};

// Color PROMs and sound PROM are identical to Pac-Man — reuse pacman:: statics.

// ---------------------------------------------------------------------------
// Decode latch trap addresses
// ---------------------------------------------------------------------------

/// Decode latch clear addresses (disable decode → return to Pac-Man code).
/// Any memory access within these 8-byte-aligned regions clears the latch.
const DECODE_DISABLE_TRAPS: &[u16] = &[0x0038, 0x03B0, 0x1600, 0x2120, 0x3FF0, 0x8000, 0x97F0];

/// Decode latch set address (enable decode → Ms. Pac-Man code).
const DECODE_ENABLE_TRAP: u16 = 0x3FF8;

// ---------------------------------------------------------------------------
// Ms. Pac-Man system
// ---------------------------------------------------------------------------

/// Ms. Pac-Man Arcade System (Midway / General Computer Corp., 1981)
///
/// Pac-Man base hardware with auxiliary daughter card containing three encrypted
/// ROMs (U5, U6, U7) and decode latch copy protection. The daughter card patches
/// the base Pac-Man code with new maze layouts, character graphics, and intermissions.
#[derive(Saveable)]
pub struct MsPacmanSystem {
    pub board: NamcoPacBoard,

    /// Decode latch state. When true, reads from ROM addresses return decoded
    /// Ms. Pac-Man code; when false, original Pac-Man code is returned.
    decode_enabled: bool,

    /// Fully decoded Ms. Pac-Man ROM (64KB: patched Pac-Man base + auxiliary code).
    #[save_skip]
    decoded_rom: Vec<u8>,

    /// Undecoded ROM bank (64KB: original Pac-Man code + mirrors at 0x8000).
    #[save_skip]
    undecoded_rom: Vec<u8>,
}

impl MsPacmanSystem {
    pub fn new() -> Self {
        Self {
            board: NamcoPacBoard::new(),
            decode_enabled: true, // MAME sets bank 1 (decoded) at init
            decoded_rom: vec![0u8; 0x10000],
            undecoded_rom: vec![0u8; 0x10000],
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        // Load base Pac-Man program ROMs
        let rom_data = MSPACMAN_PROGRAM_ROM.load(rom_set)?;
        self.board.load_program_rom(&rom_data);

        // Load encrypted auxiliary board ROMs
        let aux_data = MSPACMAN_AUX_ROM.load(rom_set)?;

        // Build decoded and undecoded ROM banks
        self.build_rom_banks(&rom_data, &aux_data);

        // Ms. Pac-Man specific graphics (different from Pac-Man)
        let gfx_data = MSPACMAN_GFX_ROM.load(rom_set)?;
        self.board.load_gfx_rom(&gfx_data);

        // Color and sound PROMs are identical to Pac-Man
        let color_data = crate::pacman::PACMAN_COLOR_PROMS.load(rom_set)?;
        self.board.load_color_proms(&color_data);

        let sound_data = crate::pacman::PACMAN_SOUND_PROM.load(rom_set)?;
        self.board.load_sound_prom(&sound_data);

        Ok(())
    }

    /// Build the decoded and undecoded ROM banks from base + auxiliary ROMs.
    ///
    /// Follows MAME's `init_mspacman()` exactly:
    /// - Decoded bank: Pac-Man base with decrypted auxiliary code overlaid + patches
    /// - Undecoded bank: Original Pac-Man code mirrored at 0x8000-0xBFFF
    fn build_rom_banks(&mut self, rom_data: &[u8], aux_data: &[u8]) {
        // aux_data layout (matching MAME offsets relative to 0x8000):
        //   [0x0000..0x07FF] = U5 raw  (corresponds to MAME ROM[0x8000..0x87FF])
        //   [0x1000..0x1FFF] = U6 raw  (corresponds to MAME ROM[0x9000..0x9FFF])
        //   [0x3000..0x3FFF] = U7 raw  (corresponds to MAME ROM[0xB000..0xBFFF])

        let drom = &mut self.decoded_rom;

        // Copy Pac-Man base ROMs (6e, 6f, 6h) into decoded bank as-is
        drom[0x0000..0x3000].copy_from_slice(&rom_data[0x0000..0x3000]);

        // Decrypt U7 → decoded[0x3000..0x3FFF] (replaces pacman.6j)
        for i in 0..0x1000 {
            let src_addr = bitswap::<12>(i, [11, 3, 7, 9, 10, 8, 6, 5, 4, 2, 1, 0]);
            drom[0x3000 + i] = bitswap_data(aux_data[0x3000 + src_addr]);
        }

        // Decrypt U5 → decoded[0x8000..0x87FF]
        for i in 0..0x800 {
            let src_addr = bitswap::<11>(i, [8, 7, 5, 9, 10, 6, 3, 4, 2, 1, 0]);
            drom[0x8000 + i] = bitswap_data(aux_data[src_addr]);
        }

        // Decrypt U6 high half → decoded[0x8800..0x8FFF]
        for i in 0..0x800 {
            let src_addr = bitswap::<11>(i, [3, 7, 9, 10, 8, 6, 5, 4, 2, 1, 0]);
            drom[0x8800 + i] = bitswap_data(aux_data[0x1800 + src_addr]);
        }

        // Decrypt U6 low half → decoded[0x9000..0x97FF]
        for i in 0..0x800 {
            let src_addr = bitswap::<11>(i, [3, 7, 9, 10, 8, 6, 5, 4, 2, 1, 0]);
            drom[0x9000 + i] = bitswap_data(aux_data[0x1000 + src_addr]);
        }

        // Mirrors of Pac-Man ROMs in upper decoded bank
        drom[0x9800..0xA000].copy_from_slice(&rom_data[0x1800..0x2000]); // pacman.6f high
        drom[0xA000..0xB000].copy_from_slice(&rom_data[0x2000..0x3000]); // pacman.6h
        drom[0xB000..0xC000].copy_from_slice(&rom_data[0x3000..0x4000]); // pacman.6j

        // Apply 40 eight-byte patches from decoded auxiliary code into base
        Self::apply_patches(drom);

        // Build undecoded bank: original Pac-Man + mirrors at 0x8000
        let urom = &mut self.undecoded_rom;
        urom[0x0000..0x4000].copy_from_slice(rom_data);
        urom[0x8000..0x9000].copy_from_slice(&rom_data[0x0000..0x1000]);
        urom[0x9000..0xA000].copy_from_slice(&rom_data[0x1000..0x2000]);
        urom[0xA000..0xB000].copy_from_slice(&rom_data[0x2000..0x3000]);
        urom[0xB000..0xC000].copy_from_slice(&rom_data[0x3000..0x4000]);
    }

    /// Apply the 40 eight-byte patches from decoded auxiliary ROM into the
    /// Pac-Man base code region (0x0000-0x2FFF). Each patch typically inserts
    /// a jump to new code above 0x8000.
    ///
    /// Source: MAME `mspacman_install_patches()`.
    fn apply_patches(drom: &mut [u8]) {
        const PATCHES: &[(u16, u16)] = &[
            (0x0410, 0x8008),
            (0x08E0, 0x81D8),
            (0x0A30, 0x8118),
            (0x0BD0, 0x80D8),
            (0x0C20, 0x8120),
            (0x0E58, 0x8168),
            (0x0EA8, 0x8198),
            (0x1000, 0x8020),
            (0x1008, 0x8010),
            (0x1288, 0x8098),
            (0x1348, 0x8048),
            (0x1688, 0x8088),
            (0x16B0, 0x8188),
            (0x16D8, 0x80C8),
            (0x16F8, 0x81C8),
            (0x19A8, 0x80A8),
            (0x19B8, 0x81A8),
            (0x2060, 0x8148),
            (0x2108, 0x8018),
            (0x21A0, 0x81A0),
            (0x2298, 0x80A0),
            (0x23E0, 0x80E8),
            (0x2418, 0x8000),
            (0x2448, 0x8058),
            (0x2470, 0x8140),
            (0x2488, 0x8080),
            (0x24B0, 0x8180),
            (0x24D8, 0x80C0),
            (0x24F8, 0x81C0),
            (0x2748, 0x8050),
            (0x2780, 0x8090),
            (0x27B8, 0x8190),
            (0x2800, 0x8028),
            (0x2B20, 0x8100),
            (0x2B30, 0x8110),
            (0x2BF0, 0x81D0),
            (0x2CC0, 0x80D0),
            (0x2CD8, 0x80E0),
            (0x2CF0, 0x81E0),
            (0x2D60, 0x8160),
        ];

        for &(dest, src) in PATCHES {
            for i in 0..8 {
                drom[dest as usize + i] = drom[src as usize + i];
            }
        }
    }

    /// Check if the given address triggers the decode latch.
    /// Called at the start of every Bus::read and Bus::write.
    fn check_decode_latch(&mut self, addr: u16) {
        let aligned = addr & !0x07;
        if aligned == DECODE_ENABLE_TRAP {
            self.decode_enabled = true;
        } else {
            for &trap in DECODE_DISABLE_TRAPS {
                if aligned == trap {
                    self.decode_enabled = false;
                    return;
                }
            }
        }
    }
}

impl Default for MsPacmanSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus implementation
// ---------------------------------------------------------------------------

impl Bus for MsPacmanSystem {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        // Check decode latch trap addresses — latch toggles BEFORE data is returned
        self.check_decode_latch(addr);

        // ROM range: select bank based on decode latch state
        if self.decode_enabled {
            match addr {
                0x0000..=0x3FFF | 0x8000..=0x97FF => {
                    let data = self.decoded_rom[addr as usize];
                    self.board.map.watch_read(0, master, addr, data);
                    return data;
                }
                _ => {}
            }
        } else {
            match addr {
                0x0000..=0x3FFF | 0x8000..=0xBFFF => {
                    let data = self.undecoded_rom[addr as usize];
                    self.board.map.watch_read(0, master, addr & 0x7FFF, data);
                    return data;
                }
                _ => {}
            }
        }

        // I/O, VRAM, CRAM, RAM regions (with A15 mirror for addresses above 0x8000)
        let effective = addr & 0x7FFF;
        self.board.bus_read_common(effective)
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        // Check decode latch trap addresses on writes too
        self.check_decode_latch(addr);

        let effective = addr & 0x7FFF;
        self.board.bus_write_common(effective, data);
    }

    fn io_read(&mut self, _master: BusMaster, _addr: u16) -> u8 {
        0xFF
    }

    fn io_write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        if addr & 0xFF == 0x00 {
            self.board.interrupt_vector = data;
        }
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            nmi: false,
            irq: self.board.vblank_irq_pending && self.board.irq_enabled,
            firq: false,
            irq_vector: self.board.interrupt_vector,
            irq_level: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Trait implementations
// ---------------------------------------------------------------------------

crate::impl_board_delegation!(MsPacmanSystem, board, namco_pac::TIMING);

impl InputReceiver for MsPacmanSystem {
    fn set_input(&mut self, button: u8, pressed: bool) {
        self.board.handle_input(button, pressed);
    }

    fn input_map(&self) -> &[phosphor_core::core::machine::InputButton] {
        namco_pac::NAMCO_PAC_INPUT_MAP
    }
}

impl MachineCore for MsPacmanSystem {
    crate::machine_core_metadata!("mspacman", namco_pac::TIMING);

    fn run_frame(&mut self) {
        bus_split!(self, bus => {
            for _ in 0..namco_pac::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });
    }

    fn reset(&mut self) {
        self.board.reset_board();
        self.decode_enabled = true; // Latch defaults to enabled (Ms. Pac-Man code)
        bus_split!(self, bus => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
        });
    }
}

impl SaveState for MsPacmanSystem {
    crate::machine_save_state!();
}

crate::impl_default_frontend_capabilities!(MsPacmanSystem);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = MsPacmanSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("mspacman", &["mspacman"], create_machine)
}

// ---------------------------------------------------------------------------
// Bitswap helpers for ROM decryption
// ---------------------------------------------------------------------------

/// Rearrange address bits according to a permutation.
/// `perm[0]` is the source bit for the MSB of the result.
/// N is the output width (number of bits).
fn bitswap<const N: usize>(val: usize, perm: [usize; N]) -> usize {
    let mut result = 0;
    for (i, &src_bit) in perm.iter().enumerate() {
        if (val >> src_bit) & 1 != 0 {
            result |= 1 << (N - 1 - i);
        }
    }
    result
}

/// Data bitswap: bitswap<8>(d, 0,4,5,7,6,3,2,1) — same for all three aux ROMs.
/// Rearranges data bits for the auxiliary board's copy protection.
fn bitswap_data(val: u8) -> u8 {
    bitswap::<8>(val as usize, [0, 4, 5, 7, 6, 3, 2, 1]) as u8
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namco_pac::Region;
    use phosphor_core::cpu::CpuStateTrait;

    #[test]
    fn save_load_round_trip() {
        let mut sys = MsPacmanSystem::new();

        // Set known board state
        sys.board.map.region_data_mut(Region::VideoRam)[0x100] = 0xAA;
        sys.board.map.region_data_mut(Region::ColorRam)[0x200] = 0xBB;
        sys.board.map.region_data_mut(Region::Ram)[0x300] = 0xCC;
        sys.board.sprite_coords[5] = 0xDD;
        sys.board.in0 = 0xEE;
        sys.board.in1 = 0x77;
        sys.board.irq_enabled = true;
        sys.board.sound_enabled = true;
        sys.board.flip_screen = true;
        sys.board.interrupt_vector = 0xCF;
        sys.board.vblank_irq_pending = true;
        sys.board.clock = 100_000;
        sys.board.watchdog_counter = 99;

        // Set Ms. Pac-Man specific state
        sys.decode_enabled = false;

        // Save
        let data = sys.save_state().expect("save_state should return Some");
        let cpu_snap = sys.board.cpu.snapshot();

        // Load into fresh system
        let mut sys2 = MsPacmanSystem::new();
        sys2.load_state(&data).unwrap();

        // Verify CPU
        assert_eq!(sys2.board.cpu.snapshot(), cpu_snap);

        // Verify board state
        assert_eq!(sys2.board.map.region_data(Region::VideoRam)[0x100], 0xAA);
        assert_eq!(sys2.board.map.region_data(Region::ColorRam)[0x200], 0xBB);
        assert_eq!(sys2.board.map.region_data(Region::Ram)[0x300], 0xCC);
        assert_eq!(sys2.board.sprite_coords[5], 0xDD);
        assert_eq!(sys2.board.in0, 0xEE);
        assert_eq!(sys2.board.in1, 0x77);
        assert!(sys2.board.irq_enabled);
        assert!(sys2.board.sound_enabled);
        assert!(sys2.board.flip_screen);
        assert_eq!(sys2.board.interrupt_vector, 0xCF);
        assert!(sys2.board.vblank_irq_pending);
        assert_eq!(sys2.board.clock, 100_000);
        assert_eq!(sys2.board.watchdog_counter, 99);

        // Verify Ms. Pac-Man specific state
        assert!(!sys2.decode_enabled);
    }

    #[test]
    fn decode_latch_disable_addresses() {
        let mut sys = MsPacmanSystem::new();
        assert!(sys.decode_enabled); // starts enabled

        // Each disable trap should clear the latch
        for &trap in DECODE_DISABLE_TRAPS {
            sys.decode_enabled = true;
            sys.check_decode_latch(trap);
            assert!(
                !sys.decode_enabled,
                "trap at 0x{trap:04X} should disable decode"
            );

            // Also verify the last byte in the 8-byte range triggers it
            sys.decode_enabled = true;
            sys.check_decode_latch(trap + 7);
            assert!(
                !sys.decode_enabled,
                "trap at 0x{:04X} should disable decode",
                trap + 7
            );
        }
    }

    #[test]
    fn decode_latch_enable_address() {
        let mut sys = MsPacmanSystem::new();
        sys.decode_enabled = false;

        sys.check_decode_latch(DECODE_ENABLE_TRAP);
        assert!(sys.decode_enabled, "0x3FF8 should enable decode");

        sys.decode_enabled = false;
        sys.check_decode_latch(DECODE_ENABLE_TRAP + 7);
        assert!(sys.decode_enabled, "0x3FFF should enable decode");
    }

    #[test]
    fn decode_latch_boundary() {
        let mut sys = MsPacmanSystem::new();

        // 0x3FF7 is in the 0x3FF0 disable range, NOT the 0x3FF8 enable range
        sys.decode_enabled = true;
        sys.check_decode_latch(0x3FF7);
        assert!(
            !sys.decode_enabled,
            "0x3FF7 should be in 0x3FF0 disable range"
        );

        // 0x3FF8 should enable
        sys.check_decode_latch(0x3FF8);
        assert!(sys.decode_enabled, "0x3FF8 should enable decode");
    }

    #[test]
    fn bus_read_uses_decode_latch() {
        let mut sys = MsPacmanSystem::new();

        // Put different values in the decoded and undecoded banks
        sys.decoded_rom[0x0100] = 0xAA;
        sys.undecoded_rom[0x0100] = 0xBB;

        sys.decode_enabled = true;
        let val = sys.read(BusMaster::Cpu(0), 0x0100);
        assert_eq!(val, 0xAA, "should read from decoded bank when enabled");

        sys.decode_enabled = false;
        let val = sys.read(BusMaster::Cpu(0), 0x0100);
        assert_eq!(val, 0xBB, "should read from undecoded bank when disabled");
    }

    #[test]
    fn bus_read_auxiliary_rom_range() {
        let mut sys = MsPacmanSystem::new();

        // Use addresses outside the 8-byte trap regions:
        //   0x8000-0x8007 is a disable trap, so use 0x8008+
        //   0x97F0-0x97F7 is a disable trap, so use 0x97EF
        sys.decoded_rom[0x8008] = 0xCD;
        sys.decoded_rom[0x97EF] = 0xEF;
        sys.undecoded_rom[0x8008] = 0x12;

        sys.decode_enabled = true;
        assert_eq!(sys.read(BusMaster::Cpu(0), 0x8008), 0xCD);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0x97EF), 0xEF);

        sys.decode_enabled = false;
        assert_eq!(sys.read(BusMaster::Cpu(0), 0x8008), 0x12);
    }

    #[test]
    fn bitswap_data_matches_mame() {
        // bitswap<8>(data, 0,4,5,7,6,3,2,1) means:
        //   result bit 7 = source bit 0
        //   result bit 6 = source bit 4
        //   result bit 5 = source bit 5
        //   result bit 4 = source bit 7
        //   result bit 3 = source bit 6
        //   result bit 2 = source bit 3
        //   result bit 1 = source bit 2
        //   result bit 0 = source bit 1
        assert_eq!(bitswap_data(0b00000001), 0b10000000); // bit 0 → bit 7
        assert_eq!(bitswap_data(0b00000010), 0b00000001); // bit 1 → bit 0
        assert_eq!(bitswap_data(0b00000100), 0b00000010); // bit 2 → bit 1
        assert_eq!(bitswap_data(0b00001000), 0b00000100); // bit 3 → bit 2
        assert_eq!(bitswap_data(0b00010000), 0b01000000); // bit 4 → bit 6
        assert_eq!(bitswap_data(0b00100000), 0b00100000); // bit 5 → bit 5
        assert_eq!(bitswap_data(0b01000000), 0b00001000); // bit 6 → bit 3
        assert_eq!(bitswap_data(0b10000000), 0b00010000); // bit 7 → bit 4
    }

    #[test]
    fn bitswap_address_known_values() {
        // Verify address bitswap<11>(i, 8,7,5,9,10,6,3,4,2,1,0) for U5
        // Input 0x001 (bit 0 set) → output bit 0 set (perm[10]=0, result bit 0)
        assert_eq!(
            bitswap::<11>(0x001, [8, 7, 5, 9, 10, 6, 3, 4, 2, 1, 0]),
            0x001
        );
        // Input 0x100 (bit 8 set) → result bit 10 set (perm[0]=8, result bit 10)
        assert_eq!(
            bitswap::<11>(0x100, [8, 7, 5, 9, 10, 6, 3, 4, 2, 1, 0]),
            0x400
        );
    }
}
