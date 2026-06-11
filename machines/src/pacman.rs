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
// Pac-Man ROM definitions ("pacman" Midway set)
// ---------------------------------------------------------------------------

/// Program ROM: 16KB at 0x0000-0x3FFF (four 4KB chips).
pub static PACMAN_PROGRAM_ROM: RomRegion = RomRegion {
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

/// GFX ROM: 8KB (tiles at 0x0000-0x0FFF, sprites at 0x1000-0x1FFF).
pub static PACMAN_GFX_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "pacman.5e",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x0c944964],
        },
        RomEntry {
            name: "pacman.5f",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x958fedf9],
        },
    ],
};

/// Palette PROM (32 bytes) + color lookup table PROM (256 bytes).
pub static PACMAN_COLOR_PROMS: RomRegion = RomRegion {
    size: 0x0120,
    entries: &[
        RomEntry {
            name: "82s123.7f",
            size: 0x0020,
            offset: 0x0000,
            crc32: &[0x2fc650bd],
        },
        RomEntry {
            name: "82s126.4a",
            size: 0x0100,
            offset: 0x0020,
            crc32: &[0x3eb3a8e4],
        },
    ],
};

/// Sound waveform PROM (256 bytes — 8 waveforms × 32 samples × 4 bits).
pub static PACMAN_SOUND_PROM: RomRegion = RomRegion {
    size: 0x0100,
    entries: &[RomEntry {
        name: "82s126.1m",
        size: 0x0100,
        offset: 0x0000,
        crc32: &[0xa9cc86bf],
    }],
};

// ---------------------------------------------------------------------------
// PacmanSystem — Pac-Man game wrapper around NamcoPacBoard
// ---------------------------------------------------------------------------

/// Pac-Man Arcade System (Namco/Midway, 1980)
///
/// Hardware: Zilog Z80 @ 3.072 MHz, Namco WSG 3-voice wavetable sound.
/// Video: 36×28 tile playfield + 8 sprites, 2bpp, PROM-based palette.
/// Screen: 288×224 displayed rotated 90° CCW on vertical monitor.
#[derive(Saveable)]
pub struct PacmanSystem {
    pub board: NamcoPacBoard,
}

impl PacmanSystem {
    pub fn new() -> Self {
        Self {
            board: NamcoPacBoard::new(),
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let rom_data = PACMAN_PROGRAM_ROM.load(rom_set)?;
        self.board.load_program_rom(&rom_data);

        let gfx_data = PACMAN_GFX_ROM.load(rom_set)?;
        self.board.load_gfx_rom(&gfx_data);

        let color_data = PACMAN_COLOR_PROMS.load(rom_set)?;
        self.board.load_color_proms(&color_data);

        let sound_data = PACMAN_SOUND_PROM.load(rom_set)?;
        self.board.load_sound_prom(&sound_data);

        Ok(())
    }

    pub fn get_cpu_state(&self) -> phosphor_core::cpu::state::Z80State {
        self.board.get_cpu_state()
    }

    pub fn clock(&self) -> u64 {
        self.board.clock()
    }
}

impl Default for PacmanSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus implementation
// ---------------------------------------------------------------------------

impl Bus for PacmanSystem {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        // A15 not connected: 0x8000-0xFFFF mirrors 0x0000-0x7FFF
        let addr = addr & 0x7FFF;
        self.board.bus_read_common(addr)
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        let addr = addr & 0x7FFF;
        self.board.bus_write_common(addr, data);
    }

    fn io_read(&mut self, _master: BusMaster, _addr: u16) -> u8 {
        0xFF // No I/O read ports used on Pac-Man
    }

    fn io_write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        // Port 0x00: set interrupt vector byte (used by Z80 IM2)
        if addr & 0xFF == 0x00 {
            self.board.interrupt_vector = data;
        }
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false // No DMA hardware on Pac-Man
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            nmi: false,
            irq: self.board.vblank_irq_pending && self.board.irq_enabled,
            firq: false,
            irq_vector: self.board.interrupt_vector,
        }
    }
}

// ---------------------------------------------------------------------------
// Trait implementations
// ---------------------------------------------------------------------------

crate::impl_board_delegation!(PacmanSystem, board, namco_pac::TIMING);

impl InputReceiver for PacmanSystem {
    fn set_input(&mut self, button: u8, pressed: bool) {
        self.board.handle_input(button, pressed);
    }

    fn input_map(&self) -> &[phosphor_core::core::machine::InputButton] {
        namco_pac::NAMCO_PAC_INPUT_MAP
    }
}

impl MachineCore for PacmanSystem {
    crate::machine_core_metadata!("pacman", namco_pac::TIMING);

    fn run_frame(&mut self) {
        bus_split!(self, bus => {
            for _ in 0..namco_pac::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });
    }

    fn reset(&mut self) {
        self.board.reset_board();
        bus_split!(self, bus => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
        });
    }
}

impl SaveState for PacmanSystem {
    crate::machine_save_state!();
}

crate::impl_default_frontend_capabilities!(PacmanSystem);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::Machine>, RomLoadError> {
    let mut sys = PacmanSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("pacman", &["pacman"], create_machine)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namco_pac::Region;
    use phosphor_core::cpu::CpuStateTrait;

    #[test]
    fn save_load_round_trip() {
        let mut sys = PacmanSystem::new();

        // Set known state
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

        // Save
        let data = sys.save_state().expect("save_state should return Some");
        let cpu_snap = sys.board.cpu.snapshot();

        // Mutate everything
        let mut sys2 = PacmanSystem::new();
        sys2.board.map.region_data_mut(Region::VideoRam)[0x100] = 0xFF;
        sys2.board.in0 = 0x00;
        sys2.board.clock = 999;

        // Load
        sys2.load_state(&data).unwrap();

        // Verify CPU
        assert_eq!(sys2.board.cpu.snapshot(), cpu_snap);

        // Verify memory
        assert_eq!(sys2.board.map.region_data(Region::VideoRam)[0x100], 0xAA);
        assert_eq!(sys2.board.map.region_data(Region::ColorRam)[0x200], 0xBB);
        assert_eq!(sys2.board.map.region_data(Region::Ram)[0x300], 0xCC);
        assert_eq!(sys2.board.sprite_coords[5], 0xDD);

        // Verify I/O and control state
        assert_eq!(sys2.board.in0, 0xEE);
        assert_eq!(sys2.board.in1, 0x77);
        assert!(sys2.board.irq_enabled);
        assert!(sys2.board.sound_enabled);
        assert!(sys2.board.flip_screen);
        assert_eq!(sys2.board.interrupt_vector, 0xCF);
        assert!(sys2.board.vblank_irq_pending);
        assert_eq!(sys2.board.clock, 100_000);
        assert_eq!(sys2.board.watchdog_counter, 99);
    }

    #[test]
    fn save_does_not_include_rom() {
        let mut sys = PacmanSystem::new();
        sys.board.map.region_data_mut(Region::Rom)[0] = 0xDE;
        sys.board.tile_cache.set_pixel(0, 0, 0, 3);

        let data = sys.save_state().unwrap();

        // Load into a fresh system (ROMs are zeroed)
        let mut sys2 = PacmanSystem::new();
        sys2.load_state(&data).unwrap();

        // ROMs and GFX caches should remain at their default, not overwritten
        assert_eq!(sys2.board.map.region_data(Region::Rom)[0], 0x00);
        assert_eq!(sys2.board.tile_cache.pixel(0, 0, 0), 0);
    }
}
