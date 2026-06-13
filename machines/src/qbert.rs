//! Q*Bert (1982, Gottlieb) — Gottlieb System 80 (GG-III) platform.
//!
//! Thin wrapper around `GottliebBoard` providing game-specific ROM loading,
//! input wiring, and `Bus` implementation for the main I8088's memory map.

use std::time::Instant;

use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    DefaultBinding, Direction, FrontendMachine, InputConfigurable, InputControl, InputEvent,
    InputId, InputKind, KeyId, MachineCore, Nvram, Profilable, ProfileSpan, SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_macros::Saveable;

use crate::gottlieb::{self, GottliebBoard};
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_high;

// ---------------------------------------------------------------------------
// ROM definitions (from MAME gottlieb.cpp — qbert parent set)
// ---------------------------------------------------------------------------

static QBERT_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x6000, // 24KB (3 × 8KB)
    entries: &[
        RomEntry {
            name: "qb-rom2.bin",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xfe434526],
        },
        RomEntry {
            name: "qb-rom1.bin",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x55635447],
        },
        RomEntry {
            name: "qb-rom0.bin",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x8e318641],
        },
    ],
};

static QBERT_SOUND_ROM: RomRegion = RomRegion {
    size: 0x2000, // 8KB (2 × 2KB, loaded at end of region)
    entries: &[
        RomEntry {
            name: "qb-snd1.bin",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0x15787c07],
        },
        RomEntry {
            name: "qb-snd2.bin",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0x58437508],
        },
    ],
};

static QBERT_TILE_ROM: RomRegion = RomRegion {
    size: 0x2000, // 8KB (2 × 4KB)
    entries: &[
        RomEntry {
            name: "qb-bg0.bin",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x7a9ba824],
        },
        RomEntry {
            name: "qb-bg1.bin",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x22e5b891],
        },
    ],
};

static QBERT_SPRITE_ROM: RomRegion = RomRegion {
    size: 0x8000, // 32KB (4 × 8KB — one per bitplane)
    entries: &[
        RomEntry {
            name: "qb-fg3.bin",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xdd436d3a],
        },
        RomEntry {
            name: "qb-fg2.bin",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0xf69b9483],
        },
        RomEntry {
            name: "qb-fg1.bin",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x224e8356],
        },
        RomEntry {
            name: "qb-fg0.bin",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0x2f695b85],
        },
    ],
};

/// Votrax SC-01A internal phoneme ROM (optional — speech works only if present).
static QBERT_VOTRAX_ROM: RomRegion = RomRegion {
    size: 0x200, // 512 bytes (64 entries × 8 bytes LE-64)
    entries: &[RomEntry {
        name: "sc01a.bin",
        size: 0x200,
        offset: 0x0000,
        crc32: &[0xfc416227],
    }],
};

// ---------------------------------------------------------------------------
// Input definitions
// ---------------------------------------------------------------------------

// IN1: start/coin (active-high except service)
const INPUT_START1: u8 = 0;
const INPUT_START2: u8 = 1;
const INPUT_COIN1: u8 = 2;
const INPUT_COIN2: u8 = 3;
const INPUT_SERVICE: u8 = 4;

// IN4: joystick (active-high)
const INPUT_RIGHT: u8 = 10;
const INPUT_LEFT: u8 = 11;
const INPUT_UP: u8 = 12;
const INPUT_DOWN: u8 = 13;

/// Typed logical controls. `InputId`s reuse the `INPUT_*` numbering; default
/// bindings mirror the legacy name-matched defaults.
const QBERT_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_COIN1 as u16),
        stable_name: "coin1",
        label: "Coin 1",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(INPUT_COIN2 as u16),
        stable_name: "coin2",
        label: "Coin 2",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[DefaultBinding::Key(KeyId::Num5)],
    },
    InputControl {
        id: InputId(INPUT_START1 as u16),
        stable_name: "p1_start",
        label: "P1 Start",
        kind: InputKind::Start,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_START,
    },
    InputControl {
        id: InputId(INPUT_START2 as u16),
        stable_name: "p2_start",
        label: "P2 Start",
        kind: InputKind::Start,
        player: Some(2),
        default_bindings: crate::input_defaults::P2_START,
    },
    InputControl {
        id: InputId(INPUT_SERVICE as u16),
        stable_name: "service",
        label: "Service",
        kind: InputKind::Service,
        player: None,
        default_bindings: crate::input_defaults::SERVICE,
    },
    InputControl {
        id: InputId(INPUT_RIGHT as u16),
        stable_name: "p1_right",
        label: "P1 Right",
        kind: InputKind::DigitalDirection {
            direction: Direction::Right,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
    InputControl {
        id: InputId(INPUT_LEFT as u16),
        stable_name: "p1_left",
        label: "P1 Left",
        kind: InputKind::DigitalDirection {
            direction: Direction::Left,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(INPUT_UP as u16),
        stable_name: "p1_up",
        label: "P1 Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_UP,
    },
    InputControl {
        id: InputId(INPUT_DOWN as u16),
        stable_name: "p1_down",
        label: "P1 Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_DOWN,
    },
];

// ---------------------------------------------------------------------------
// QbertSystem
// ---------------------------------------------------------------------------

/// Q*Bert (1982, Gottlieb) on the Gottlieb System 80 (GG-III) platform.
///
/// Wraps `GottliebBoard` with Q*Bert-specific ROM loading, input mapping,
/// and `Bus<Address = u32>` implementation for the I8088 main CPU.
#[derive(Saveable)]
pub struct QbertSystem {
    pub board: GottliebBoard,
}

impl QbertSystem {
    pub fn new() -> Self {
        let mut board = GottliebBoard::new();
        // IN1 default: service bit 6 is active-LOW (idle high)
        board.input_ports[0] = 0x40;
        Self { board }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        // Program ROM (24KB, loaded at end of 0x6000-0xFFFF region → 0xA000-0xFFFF)
        let prog_data = QBERT_PROGRAM_ROM.load(rom_set)?;
        self.board.load_program_rom(&prog_data);

        // Sound ROM (8KB, loaded into sound board)
        let sound_data = QBERT_SOUND_ROM.load(rom_set)?;
        self.board.load_sound_rom(&sound_data);

        // Votrax SC-01A phoneme ROM (optional — speech disabled if missing)
        if let Ok(votrax_data) = QBERT_VOTRAX_ROM.load(rom_set) {
            self.board.load_votrax_rom(&votrax_data);
        }

        // GFX ROMs
        let tile_data = QBERT_TILE_ROM.load(rom_set)?;
        let sprite_data = QBERT_SPRITE_ROM.load(rom_set)?;
        self.board.decode_gfx(&tile_data, &sprite_data);

        // Q*Bert uses ROM tiles for all codes (init_romtiles)
        self.board.gfxcharlo = true;
        self.board.gfxcharhi = true;

        // IN1 default: service bit 6 is active-LOW (idle high)
        self.board.input_ports[0] = 0x40;

        Ok(())
    }
}

impl Default for QbertSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus — I8088 main CPU memory map (20-bit address masked to 16-bit)
// ---------------------------------------------------------------------------

impl Bus for QbertSystem {
    type Address = u32;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u32) -> u8 {
        let addr16 = (addr & 0xFFFF) as u16;
        let data = match addr16 {
            // NVRAM: 0x0000-0x0FFF
            0x0000..=0x0FFF => self.board.map.read_backing(addr16),

            // RAM: 0x1000-0x2FFF
            0x1000..=0x2FFF => self.board.map.read_backing(addr16),

            // Sprite RAM: 0x3000-0x37FF (256 bytes mirrored)
            0x3000..=0x37FF => {
                let offset = addr16 & 0xFF;
                self.board.map.read_backing(0x3000 + offset)
            }

            // Video RAM: 0x3800-0x3FFF (1KB mirrored)
            0x3800..=0x3FFF => {
                let offset = addr16 & 0x3FF;
                self.board.map.read_backing(0x3800 + offset)
            }

            // Char RAM: 0x4000-0x4FFF
            0x4000..=0x4FFF => self.board.map.read_backing(addr16),

            // Palette RAM: 0x5000-0x57FF (32 bytes mirrored)
            0x5000..=0x57FF => {
                let offset = (addr16 & 0x1F) as usize;
                self.board.palette_ram[offset]
            }

            // I/O ports: 0x5800-0x5FFF (3-bit decode)
            0x5800..=0x5FFF => self.board.io_port_read(addr16 as u8),

            // Program ROM: 0x6000-0xFFFF
            0x6000..=0xFFFF => self.board.map.read_backing(addr16),
        };
        self.board.map.watch_read(0, master, addr16, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u32, data: u8) {
        let addr16 = (addr & 0xFFFF) as u16;
        self.board.map.watch_write(0, master, addr16, data);
        match addr16 {
            // NVRAM: 0x0000-0x0FFF
            0x0000..=0x0FFF => self.board.map.write_backing(addr16, data),

            // RAM: 0x1000-0x2FFF
            0x1000..=0x2FFF => self.board.map.write_backing(addr16, data),

            // Sprite RAM: 0x3000-0x37FF (256 bytes mirrored)
            0x3000..=0x37FF => {
                let offset = addr16 & 0xFF;
                self.board.map.write_backing(0x3000 + offset, data);
            }

            // Video RAM: 0x3800-0x3FFF (1KB mirrored)
            0x3800..=0x3FFF => {
                let offset = addr16 & 0x3FF;
                self.board.map.write_backing(0x3800 + offset, data);
            }

            // Char RAM: 0x4000-0x4FFF
            0x4000..=0x4FFF => {
                let offset = (addr16 - 0x4000) as usize;
                self.board.charram_write(offset, data);
            }

            // Palette RAM: 0x5000-0x57FF (32 bytes mirrored)
            0x5000..=0x57FF => {
                let offset = (addr16 & 0x1F) as usize;
                self.board.update_palette(offset, data);
            }

            // I/O ports: 0x5800-0x5FFF
            0x5800..=0x5FFF => self.board.io_port_write(addr16 as u8, data),

            _ => {} // ROM and unmapped: writes ignored
        }
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        match target {
            BusMaster::Cpu(0) => {
                // VBLANK NMI: asserted during blanking period (scanlines 240-255)
                let scanline = self.board.clock / gottlieb::TIMING.cycles_per_scanline
                    % gottlieb::TIMING.total_scanlines;
                let in_vblank = scanline >= gottlieb::VISIBLE_LINES;
                InterruptState {
                    nmi: in_vblank,
                    ..Default::default()
                }
            }
            _ => InterruptState::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Machine traits (MachineCore + capabilities)
// ---------------------------------------------------------------------------

crate::impl_board_delegation!(QbertSystem, board, gottlieb::TIMING, bus_addr: u32);

impl InputConfigurable for QbertSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        QBERT_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let InputEvent::Button { id, pressed } = event else {
            return;
        };
        match id.0 as u8 {
            // IN1: start/coin (active-high, bits 0-3; service active-low, bit 6)
            INPUT_START1 => set_bit_active_high(&mut self.board.input_ports[0], 0, pressed),
            INPUT_START2 => set_bit_active_high(&mut self.board.input_ports[0], 1, pressed),
            INPUT_COIN1 => set_bit_active_high(&mut self.board.input_ports[0], 2, pressed),
            INPUT_COIN2 => set_bit_active_high(&mut self.board.input_ports[0], 3, pressed),
            // Active-LOW: clear on press, set on release
            INPUT_SERVICE => crate::set_bit_active_low(&mut self.board.input_ports[0], 6, pressed),
            // IN4: joystick (active-high, bits 0-3)
            INPUT_RIGHT => set_bit_active_high(&mut self.board.input_ports[3], 0, pressed),
            INPUT_LEFT => set_bit_active_high(&mut self.board.input_ports[3], 1, pressed),
            INPUT_UP => set_bit_active_high(&mut self.board.input_ports[3], 2, pressed),
            INPUT_DOWN => set_bit_active_high(&mut self.board.input_ports[3], 3, pressed),
            _ => {}
        }
    }
}

impl MachineCore for QbertSystem {
    crate::machine_core_metadata!("qbert", gottlieb::TIMING);

    fn run_frame(&mut self) {
        let t0 = self.board.profiling.then(Instant::now);

        bus_split!(self, bus : u32 => {
            for _ in 0..gottlieb::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });

        let t1 = self.board.profiling.then(Instant::now);

        self.board.render_frame_internal();

        if let (Some(t0), Some(t1)) = (t0, t1) {
            let t2 = Instant::now();
            self.board.profile_spans.clear();
            self.board.profile_spans.push(ProfileSpan {
                name: "cpu",
                duration: t1 - t0,
            });
            self.board.profile_spans.push(ProfileSpan {
                name: "gfx",
                duration: t2 - t1,
            });
        }
    }

    fn reset(&mut self) {
        self.board.reset_board();
        bus_split!(self, bus : u32 => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
        });
        // Re-initialize IN1 idle state
        self.board.input_ports[0] = 0x40;
    }
}

impl SaveState for QbertSystem {
    crate::machine_save_state!();
}

impl Nvram for QbertSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.map.region_data(gottlieb::Region::Nvram))
    }

    fn load_nvram(&mut self, data: &[u8]) {
        let nvram = self.board.map.region_data_mut(gottlieb::Region::Nvram);
        let len = data.len().min(nvram.len());
        nvram[..len].copy_from_slice(&data[..len]);
    }
}

impl Profilable for QbertSystem {
    fn set_profiling(&mut self, enabled: bool) {
        self.board.profiling = enabled;
    }

    fn frame_profile_spans(&self) -> &[ProfileSpan] {
        &self.board.profile_spans
    }
}

impl phosphor_core::core::debug_trace::DebugTrace for QbertSystem {}

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

fn create_machine(rom_set: &RomSet) -> Result<Box<dyn FrontendMachine>, RomLoadError> {
    let mut sys = QbertSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("qbert", &["qbert"], create_machine)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_load_round_trip() {
        let mut sys = QbertSystem::new();

        // Set known state
        sys.board.map.region_data_mut(gottlieb::Region::Nvram)[0x100] = 0xAA;
        sys.board.map.region_data_mut(gottlieb::Region::Ram)[0x50] = 0xBB;
        sys.board.map.region_data_mut(gottlieb::Region::VideoRam)[0x10] = 0xCC;
        sys.board.palette_ram[0] = 0x55;
        sys.board.clock = 50_000;
        sys.board.watchdog_counter = 42;
        sys.board.video_control = 1;
        sys.board.sprite_bank = 2;

        // Save
        let data = sys.save_state().expect("save_state should return Some");

        // Load into fresh system
        let mut sys2 = QbertSystem::new();
        sys2.load_state(&data).unwrap();

        // Verify
        assert_eq!(
            sys2.board.map.region_data(gottlieb::Region::Nvram)[0x100],
            0xAA
        );
        assert_eq!(
            sys2.board.map.region_data(gottlieb::Region::Ram)[0x50],
            0xBB
        );
        assert_eq!(
            sys2.board.map.region_data(gottlieb::Region::VideoRam)[0x10],
            0xCC
        );
        assert_eq!(sys2.board.palette_ram[0], 0x55);
        assert_eq!(sys2.board.clock, 50_000);
        assert_eq!(sys2.board.watchdog_counter, 42);
        assert_eq!(sys2.board.video_control, 1);
        assert_eq!(sys2.board.sprite_bank, 2);
    }

    #[test]
    fn input_active_high_joystick() {
        let mut sys = QbertSystem::new();

        // Initially joystick is idle (0x00)
        assert_eq!(sys.board.input_ports[3], 0x00);

        // Press right
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_RIGHT) as u16),
            pressed: true,
        });
        assert_eq!(sys.board.input_ports[3], 0x01); // bit 0 set

        // Release right
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_RIGHT) as u16),
            pressed: false,
        });
        assert_eq!(sys.board.input_ports[3], 0x00);

        // Press up
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_UP) as u16),
            pressed: true,
        });
        assert_eq!(sys.board.input_ports[3], 0x04); // bit 2 set
    }

    #[test]
    fn input_coin_and_start() {
        let mut sys = QbertSystem::new();

        // IN1 starts with service bit 6 idle high
        assert_eq!(sys.board.input_ports[0], 0x40);

        // Press coin 1 (active-high, bit 2)
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_COIN1) as u16),
            pressed: true,
        });
        assert_eq!(sys.board.input_ports[0], 0x44);

        // Press service (active-low, bit 6 → clear)
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_SERVICE) as u16),
            pressed: true,
        });
        assert_eq!(sys.board.input_ports[0], 0x04); // bit 6 cleared

        // Release service (bit 6 → set)
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_SERVICE) as u16),
            pressed: false,
        });
        assert_eq!(sys.board.input_ports[0], 0x44);
    }

    #[test]
    fn palette_rgb_decode() {
        let mut sys = QbertSystem::new();

        // Write palette entry 0: even byte G=0xF B=0x0, odd byte R=0xF
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x5000, 0xF0); // G=15, B=0
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x5001, 0x0F); // R=15

        assert_eq!(sys.board.palette_rgb[0], (255, 255, 0)); // R=255, G=255, B=0
    }

    #[test]
    fn palette_resistor_weighted() {
        let mut sys = QbertSystem::new();

        // Value 4 (0100): resistor DAC = 70, not linear 68
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x5000, 0x40); // G=4, B=0
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x5001, 0x04); // R=4
        assert_eq!(sys.board.palette_rgb[0], (70, 70, 0));

        // Value 12 (1100): resistor DAC = 206, not linear 204
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x5002, 0xC0); // G=12, B=0
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x5003, 0x0C); // R=12
        assert_eq!(sys.board.palette_rgb[1], (206, 206, 0));
    }

    #[test]
    fn palette_mirror() {
        let mut sys = QbertSystem::new();

        // Write through mirror (0x5020 maps to same as 0x5000)
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x5020, 0xAB);
        assert_eq!(sys.board.palette_ram[0], 0xAB);
    }

    #[test]
    fn memory_map_ram_read_write() {
        let mut sys = QbertSystem::new();

        Bus::write(&mut sys, BusMaster::Cpu(0), 0x1000, 0x55);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x1000), 0x55);
    }

    #[test]
    fn sprite_ram_mirror() {
        let mut sys = QbertSystem::new();

        Bus::write(&mut sys, BusMaster::Cpu(0), 0x3010, 0xBB);
        // Mirror: 0x3110 maps to 0x3010 (offset 0x10)
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x3110), 0xBB);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x3210), 0xBB);
    }

    #[test]
    fn video_ram_mirror() {
        let mut sys = QbertSystem::new();

        Bus::write(&mut sys, BusMaster::Cpu(0), 0x3900, 0xCC);
        // Mirror: 0x3D00 maps to 0x3900 (offset 0x100, bit 10 don't-care)
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x3D00), 0xCC);
    }

    #[test]
    fn address_wraps_to_16_bit() {
        let mut sys = QbertSystem::new();

        // I8088 physical address 0x10042 should wrap to 0x0042 (NVRAM)
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x10042, 0xDD);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x0042), 0xDD);
    }
}
