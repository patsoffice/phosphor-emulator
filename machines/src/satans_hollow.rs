use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    FrontendMachine, InputButton, InputReceiver, MachineCore, Nvram, Profilable, SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_macros::Saveable;

use crate::mcr2::{self, Mcr2Board};
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_low;

// ---------------------------------------------------------------------------
// ROM definitions (from MAME mcr.cpp — shollow parent set)
// ---------------------------------------------------------------------------

static SHOLLOW_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0xC000, // 48KB
    entries: &[
        RomEntry {
            name: "sh-pro.00",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x95e2b800],
        },
        RomEntry {
            name: "sh-pro.01",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0xb99f6ff8],
        },
        RomEntry {
            name: "sh-pro.02",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x1202c7b2],
        },
        RomEntry {
            name: "sh-pro.03",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0x0a64afb9],
        },
        RomEntry {
            name: "sh-pro.04",
            size: 0x2000,
            offset: 0x8000,
            crc32: &[0x22fa9175],
        },
        RomEntry {
            name: "sh-pro.05",
            size: 0x2000,
            offset: 0xA000,
            crc32: &[0x1716e2bb],
        },
    ],
};

static SHOLLOW_SOUND_ROM: RomRegion = RomRegion {
    size: 0x4000, // 16KB (12KB data + 4KB padding)
    entries: &[
        RomEntry {
            name: "sh-snd.01",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x55a297cc],
        },
        RomEntry {
            name: "sh-snd.02",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x46fc31f6],
        },
        RomEntry {
            name: "sh-snd.03",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0xb1f4a6a8],
        },
    ],
};

static SHOLLOW_BG_ROM: RomRegion = RomRegion {
    size: 0x4000, // 16KB (2 × 8KB)
    entries: &[
        RomEntry {
            name: "sh-bg.00",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x3e2b333c],
        },
        RomEntry {
            name: "sh-bg.01",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0xd1d70cc4],
        },
    ],
};

static SHOLLOW_FG_ROM: RomRegion = RomRegion {
    size: 0x8000, // 32KB (4 × 8KB)
    entries: &[
        RomEntry {
            name: "sh-fg.00",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x33f4554e],
        },
        RomEntry {
            name: "sh-fg.01",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0xba1a38b4],
        },
        RomEntry {
            name: "sh-fg.02",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x6b57f6da],
        },
        RomEntry {
            name: "sh-fg.03",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0x37ea9d07],
        },
    ],
};

// ---------------------------------------------------------------------------
// Input definitions
// ---------------------------------------------------------------------------

// IP0: coin/start (active-low)
const INPUT_COIN1: u8 = 0;
const INPUT_COIN2: u8 = 1;
const INPUT_START1: u8 = 2;
const INPUT_START2: u8 = 3;
const INPUT_TILT: u8 = 4;
const INPUT_SERVICE: u8 = 5;

// IP1: player controls (active-low)
const INPUT_LEFT: u8 = 6;
const INPUT_RIGHT: u8 = 7;
const INPUT_SHIELD: u8 = 8;
const INPUT_FIRE: u8 = 9;

const SHOLLOW_INPUT_MAP: &[InputButton] = &[
    InputButton {
        id: INPUT_COIN1,
        name: "Coin 1",
    },
    InputButton {
        id: INPUT_COIN2,
        name: "Coin 2",
    },
    InputButton {
        id: INPUT_START1,
        name: "P1 Start",
    },
    InputButton {
        id: INPUT_START2,
        name: "P2 Start",
    },
    InputButton {
        id: INPUT_TILT,
        name: "Tilt",
    },
    InputButton {
        id: INPUT_SERVICE,
        name: "Service",
    },
    InputButton {
        id: INPUT_LEFT,
        name: "P1 Left",
    },
    InputButton {
        id: INPUT_RIGHT,
        name: "P1 Right",
    },
    InputButton {
        id: INPUT_SHIELD,
        name: "P1 Jump",
    },
    InputButton {
        id: INPUT_FIRE,
        name: "P1 Fire",
    },
];

// ---------------------------------------------------------------------------
// SatansHollowSystem
// ---------------------------------------------------------------------------

/// Satan's Hollow (1982, Bally Midway) — MCR II platform.
///
/// Thin wrapper around `Mcr2Board` providing game-specific ROM loading,
/// input wiring, and `Bus` implementation for the main Z80's memory/IO map.
#[derive(Saveable)]
pub struct SatansHollowSystem {
    pub board: Mcr2Board,
}

impl SatansHollowSystem {
    pub fn new() -> Self {
        Self {
            board: Mcr2Board::new(),
        }
    }

    fn overlay_stats_impl(&self) -> Option<String> {
        let total = mcr2::TILE_ROWS * mcr2::TILE_COLS;
        Some(format!(
            "tile dirty: {}/{}",
            self.board.tiles_redrawn, total
        ))
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        // Program ROM
        let prog_data = SHOLLOW_PROGRAM_ROM.load(rom_set)?;
        self.board.map.load_region(mcr2::Region::Rom, &prog_data);

        // Sound ROM (loaded into SSIO)
        let sound_data = SHOLLOW_SOUND_ROM.load(rom_set)?;
        self.board.ssio.load_rom(&sound_data);

        // GFX ROMs
        let bg_data = SHOLLOW_BG_ROM.load(rom_set)?;
        let fg_data = SHOLLOW_FG_ROM.load(rom_set)?;
        self.board.decode_gfx(&bg_data, &fg_data);

        // Set IP3 to active-high idle (0x00 instead of default 0xFF)
        self.board.ssio.set_input_port(3, 0x00);

        // DIP switches: MAME defaults = 0x81 (Free Play OFF, coinage 1C/1C)
        self.board.ssio.set_dip_switches(0x81);

        Ok(())
    }
}

impl Default for SatansHollowSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus — MCR II main CPU memory and I/O map
// ---------------------------------------------------------------------------

impl Bus for SatansHollowSystem {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let data = match self.board.map.page(addr).region_id {
            mcr2::Region::ROM
            | mcr2::Region::NVRAM
            | mcr2::Region::SPRITE_RAM
            | mcr2::Region::VIDEO_RAM => self.board.map.read_backing(addr),
            _ => 0xFF,
        };
        self.board.map.watch_read(0, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.board.map.watch_write(0, master, addr, data);
        match self.board.map.page(addr).region_id {
            mcr2::Region::NVRAM | mcr2::Region::SPRITE_RAM => {
                self.board.map.write_backing(addr, data);
            }
            mcr2::Region::VIDEO_RAM => {
                self.board.map.write_backing(addr, data);
                let offset = (addr & 0x7FF) as usize;
                if (offset & 0x780) == 0x780 {
                    self.board.update_palette_from_vram(offset, data);
                    self.board.tile_dirty.mark_all();
                } else {
                    self.board.mark_tile_dirty(offset);
                }
            }
            _ => {} // ROM and unmapped: writes ignored
        }
    }

    fn io_read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        let port = addr as u8;
        match port {
            // SSIO range (bits 5-7 clear): mirror(0x18) means bits 3-4 are don't-care
            0x00..=0x1F => {
                let base = port & 0x07;
                match base {
                    0x00..=0x04 => self.board.ssio.input_port(base as usize),
                    0x07 => self.board.ssio.status_read(),
                    _ => 0xFF,
                }
            }
            0xF0..=0xF3 => self.board.ctc.read(port - 0xF0),
            _ => 0xFF,
        }
    }

    fn io_write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        let port = addr as u8;
        match port {
            // SSIO output ports (no custom outputs for Satan's Hollow)
            0x00..=0x07 => {}
            0x1C..=0x1F => self.board.ssio.latch_write(port - 0x1C, data),
            0xE0 => self.board.watchdog_counter = 0,
            0xE8 => {} // nop write (MAME: map(0xe8, 0xe8).nopw())
            0xF0..=0xF3 => self.board.ctc.write(port - 0xF0, data),
            _ => {}
        }
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        match target {
            BusMaster::Cpu(0) => {
                if self.board.ctc.interrupt_pending() {
                    let vector = self.board.ctc.interrupt_vector();
                    self.board.ctc_vector_latch = vector;
                    self.board.ctc_ack_needed = true;
                    InterruptState {
                        irq: true,
                        irq_vector: vector,
                        ..Default::default()
                    }
                } else {
                    // Return latched vector for INTA cycle (Z80 reads irq_vector
                    // during interrupt acknowledge regardless of irq flag)
                    InterruptState {
                        irq_vector: self.board.ctc_vector_latch,
                        ..Default::default()
                    }
                }
            }
            _ => InterruptState::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Machine trait implementations
// ---------------------------------------------------------------------------

crate::impl_board_delegation!(SatansHollowSystem, board, mcr2::TIMING, overlay_stats);

impl InputReceiver for SatansHollowSystem {
    fn set_input(&mut self, button: u8, pressed: bool) {
        let (port, bit) = match button {
            // IP0: coin/start/service (active-low, bits 0-7)
            INPUT_COIN1 => (0usize, 0u8),
            INPUT_COIN2 => (0, 1),
            INPUT_START1 => (0, 2),
            INPUT_START2 => (0, 3),
            INPUT_TILT => (0, 5),
            INPUT_SERVICE => (0, 7),
            // IP1: player controls (active-low)
            INPUT_LEFT => (1, 0),
            INPUT_RIGHT => (1, 1),
            INPUT_SHIELD => (1, 2),
            INPUT_FIRE => (1, 3),
            _ => return,
        };
        let mut val = self.board.ssio.input_port(port);
        set_bit_active_low(&mut val, bit, pressed);
        self.board.ssio.set_input_port(port, val);
    }

    fn input_map(&self) -> &[InputButton] {
        SHOLLOW_INPUT_MAP
    }
}

impl MachineCore for SatansHollowSystem {
    crate::machine_core_metadata!("shollow", mcr2::TIMING);

    fn run_frame(&mut self) {
        bus_split!(self, bus => {
            for _ in 0..mcr2::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });
        self.board.render_frame_internal();
    }

    fn reset(&mut self) {
        self.board.reset_board();
        bus_split!(self, bus => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
        });
        // Re-initialize IP3 to active-high idle
        self.board.ssio.set_input_port(3, 0x00);
    }
}

impl SaveState for SatansHollowSystem {
    crate::machine_save_state!();
}

impl Nvram for SatansHollowSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.map.region_data(mcr2::Region::Nvram))
    }

    fn load_nvram(&mut self, data: &[u8]) {
        let nvram = self.board.map.region_data_mut(mcr2::Region::Nvram);
        let len = data.len().min(nvram.len());
        nvram[..len].copy_from_slice(&data[..len]);
    }
}

impl Profilable for SatansHollowSystem {}

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

fn create_machine(rom_set: &RomSet) -> Result<Box<dyn FrontendMachine>, RomLoadError> {
    let mut sys = SatansHollowSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("shollow", &["shollow"], create_machine)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::cpu::CpuStateTrait;

    #[test]
    fn save_load_round_trip() {
        let mut sys = SatansHollowSystem::new();

        // Set known state
        sys.board.map.region_data_mut(mcr2::Region::Nvram)[0x100] = 0xAA;
        sys.board.map.region_data_mut(mcr2::Region::VideoRam)[0x50] = 0xBB;
        sys.board.map.region_data_mut(mcr2::Region::SpriteRam)[0x10] = 0xCC;
        sys.board.palette_ram[0] = 0x55;
        sys.board.clock = 50_000;
        sys.board.watchdog_counter = 42;

        // Save
        let data = sys.save_state().expect("save_state should return Some");

        // Capture CPU snapshot
        let cpu_snap = sys.board.cpu.snapshot();

        // Load into fresh system
        let mut sys2 = SatansHollowSystem::new();
        sys2.load_state(&data).unwrap();

        // Verify
        assert_eq!(sys2.board.cpu.snapshot(), cpu_snap);
        assert_eq!(sys2.board.map.region_data(mcr2::Region::Nvram)[0x100], 0xAA);
        assert_eq!(
            sys2.board.map.region_data(mcr2::Region::VideoRam)[0x50],
            0xBB
        );
        assert_eq!(
            sys2.board.map.region_data(mcr2::Region::SpriteRam)[0x10],
            0xCC
        );
        assert_eq!(sys2.board.palette_ram[0], 0x55);
        assert_eq!(sys2.board.clock, 50_000);
        assert_eq!(sys2.board.watchdog_counter, 42);
    }

    #[test]
    fn save_does_not_include_rom() {
        let mut sys = SatansHollowSystem::new();
        sys.board.map.region_data_mut(mcr2::Region::Rom)[0] = 0xDE;

        let data = sys.save_state().unwrap();

        // Load into system with different ROM
        let mut sys2 = SatansHollowSystem::new();
        sys2.board.map.region_data_mut(mcr2::Region::Rom)[0] = 0x11;
        sys2.load_state(&data).unwrap();

        assert_eq!(
            sys2.board.map.region_data(mcr2::Region::Rom)[0],
            0x11,
            "ROM should be untouched"
        );
    }

    #[test]
    fn palette_9bit_decode() {
        let mut board = Mcr2Board::new();

        // Entry 0: val9 = 0x1FF (all bits set)
        // R = pal3bit(0x1FF >> 6) = pal3bit(7) = 255
        // G = pal3bit(0x1FF & 7) = pal3bit(7) = 255
        // B = pal3bit((0x1FF >> 3) & 7) = pal3bit(7) = 255
        board.palette_ram[0] = 0xFF; // low byte
        board.palette_ram[1] = 0x01; // high byte (bit 0 = 1)
        board.rebuild_palette();
        assert_eq!(board.palette_rgb[0], (255, 255, 255));

        // Entry 1: val9 = 0x000
        board.palette_ram[2] = 0x00;
        board.palette_ram[3] = 0x00;
        board.rebuild_palette();
        assert_eq!(board.palette_rgb[1], (0, 0, 0));

        // Entry 2: val9 = 0x049 = 0b001_001_001
        // R = pal3bit(1) = 36, G = pal3bit(1) = 36, B = pal3bit(1) = 36
        board.palette_ram[4] = 0x49;
        board.palette_ram[5] = 0x00;
        board.rebuild_palette();
        assert_eq!(board.palette_rgb[2], (36, 36, 36));
    }

    #[test]
    fn palette_from_vram_write() {
        let mut sys = SatansHollowSystem::new();

        // Palette is in video RAM at offset 0x780-0x7FF (CPU addr 0xEF80-0xEFFF).
        // Entry 0 = offsets 0x780 (even) and 0x781 (odd).
        // Write to odd byte: val9 = 0xFF | (1 << 8) = 0x1FF → all white
        Bus::write(&mut sys, BusMaster::Cpu(0), 0xEF81, 0xFF);
        assert_eq!(sys.board.palette_rgb[0], (255, 255, 255));

        // Write to even byte: val9 = 0x00 | (0 << 8) = 0x000 → all black
        Bus::write(&mut sys, BusMaster::Cpu(0), 0xEF80, 0x00);
        assert_eq!(sys.board.palette_rgb[0], (0, 0, 0));

        // Entry 1 via mirror at 0xF800 range: 0xF800 + 0x782 = 0xFF82
        // val9 = 0x49 | (0 << 8) = 0x49 → R=1, G=1, B=1 → (36, 36, 36)
        Bus::write(&mut sys, BusMaster::Cpu(0), 0xFF82, 0x49);
        assert_eq!(sys.board.palette_rgb[1], (36, 36, 36));
    }

    #[test]
    fn memory_map_mirrors() {
        let mut sys = SatansHollowSystem::new();

        // NVRAM mirror: 0xC000 and 0xD000 map to same byte
        Bus::write(&mut sys, BusMaster::Cpu(0), 0xC042, 0xAA);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0xD042), 0xAA);

        // Sprite RAM mirror: 0xE000 and 0xF000 map to same byte
        Bus::write(&mut sys, BusMaster::Cpu(0), 0xE010, 0xBB);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0xF010), 0xBB);
        // Also mirrored within 0xE000-0xE7FF (bit 9)
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0xE210), 0xBB);

        // Video RAM mirror: 0xE800 and 0xF800 map to same byte
        Bus::write(&mut sys, BusMaster::Cpu(0), 0xE900, 0xCC);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0xF900), 0xCC);
    }

    #[test]
    fn input_active_low() {
        let mut sys = SatansHollowSystem::new();

        // Initially all inputs are idle (0xFF for active-low ports)
        assert_eq!(sys.board.ssio.input_port(0), 0xFF);
        assert_eq!(sys.board.ssio.input_port(1), 0xFF);

        // Press coin
        sys.set_input(INPUT_COIN1, true);
        assert_eq!(sys.board.ssio.input_port(0), 0xFE); // bit 0 cleared

        // Release coin
        sys.set_input(INPUT_COIN1, false);
        assert_eq!(sys.board.ssio.input_port(0), 0xFF); // bit 0 set again

        // Press fire
        sys.set_input(INPUT_FIRE, true);
        assert_eq!(sys.board.ssio.input_port(1), 0xF7); // bit 3 cleared
    }

    #[test]
    fn io_read_ssio_input_ports() {
        let mut sys = SatansHollowSystem::new();
        sys.board.ssio.set_input_port(0, 0xAA);
        sys.board.ssio.set_input_port(1, 0xBB);

        // Base addresses
        assert_eq!(Bus::io_read(&mut sys, BusMaster::Cpu(0), 0x00), 0xAA);
        assert_eq!(Bus::io_read(&mut sys, BusMaster::Cpu(0), 0x01), 0xBB);

        // Mirror with bit 3 set (0x08 offset)
        assert_eq!(Bus::io_read(&mut sys, BusMaster::Cpu(0), 0x08), 0xAA);
        assert_eq!(Bus::io_read(&mut sys, BusMaster::Cpu(0), 0x09), 0xBB);

        // Mirror with bit 4 set (0x10 offset)
        assert_eq!(Bus::io_read(&mut sys, BusMaster::Cpu(0), 0x10), 0xAA);
        assert_eq!(Bus::io_read(&mut sys, BusMaster::Cpu(0), 0x11), 0xBB);

        // Mirror with bits 3+4 set (0x18 offset)
        assert_eq!(Bus::io_read(&mut sys, BusMaster::Cpu(0), 0x18), 0xAA);

        // Status read at base 0x07 and mirror 0x0F
        assert_eq!(Bus::io_read(&mut sys, BusMaster::Cpu(0), 0x07), 0x00);
        assert_eq!(Bus::io_read(&mut sys, BusMaster::Cpu(0), 0x0F), 0x00);
    }

    #[test]
    fn io_write_ctc() {
        let mut sys = SatansHollowSystem::new();

        // Write vector base to CTC channel 0
        Bus::io_write(&mut sys, BusMaster::Cpu(0), 0xF0, 0xE0);
        assert_eq!(sys.board.ctc.read(0), 0); // counter value (vector base isn't readable this way)
    }

    #[test]
    fn memory_map_rom_read() {
        let mut sys = SatansHollowSystem::new();
        sys.board.map.region_data_mut(mcr2::Region::Rom)[0] = 0x42;
        sys.board.map.region_data_mut(mcr2::Region::Rom)[0xBFFF] = 0x77;

        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x0000), 0x42);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0xBFFF), 0x77);
    }

    #[test]
    fn memory_map_nvram_read_write() {
        let mut sys = SatansHollowSystem::new();

        Bus::write(&mut sys, BusMaster::Cpu(0), 0xC000, 0x55);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0xC000), 0x55);
        assert_eq!(sys.board.map.region_data(mcr2::Region::Nvram)[0], 0x55);
    }
}
