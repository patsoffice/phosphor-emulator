use phosphor_core::core::BusMaster;
use phosphor_core::core::machine::{MachineCore, SaveState};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::z80::Z80;
use phosphor_macros::{BusDebug, Saveable};

use crate::namco_pac::{self, NamcoPacBoard};
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
///
/// The Z80 and the board it drives are separate fields: `self.cpu` and
/// `self.board` are disjoint borrows, so the CPU's bus accesses go straight to
/// [`NamcoPacBoard`]'s `Bus` impl with no trait object in between.
#[derive(Saveable, BusDebug)]
pub struct PacmanSystem {
    #[debug_cpu("Z80")]
    pub cpu: Z80,

    #[debug_bus]
    pub board: NamcoPacBoard,
}

impl PacmanSystem {
    pub fn new() -> Self {
        Self {
            cpu: Z80::new(),
            board: NamcoPacBoard::new(),
        }
    }

    /// One CPU cycle. Returns 1 at an instruction boundary (for the debugger's
    /// single-step, which counts instructions rather than cycles).
    pub fn step_cycle(&mut self) -> u32 {
        namco_pac::tick(&mut self.cpu, &mut self.board);
        u32::from(self.cpu.at_instruction_boundary())
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
        use phosphor_core::cpu::CpuStateTrait;
        self.cpu.snapshot()
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
// Trait implementations
// ---------------------------------------------------------------------------

// Pac-Man adds nothing to the base board's address decoding, so `NamcoPacBoard`
// *is* the bus — see its `Bus` impl in namco_pac.rs.

crate::impl_board_delegation!(PacmanSystem, board, namco_pac::TIMING, orientation);

impl MachineCore for PacmanSystem {
    crate::machine_core_metadata!("pacman", namco_pac::TIMING);

    fn gfx_sheets(&self) -> Vec<phosphor_core::core::machine::GfxSheet<'_>> {
        use phosphor_core::core::machine::GfxSheet;
        vec![
            GfxSheet {
                name: "tiles",
                cache: &self.board.tile_cache,
                palette: &self.board.palette_rgb,
            },
            GfxSheet {
                name: "sprites",
                cache: &self.board.sprite_cache,
                palette: &self.board.palette_rgb,
            },
        ]
    }

    fn run_frame(&mut self) {
        namco_pac::run_frame(&mut self.cpu, &mut self.board);
    }

    fn reset(&mut self) {
        self.board.reset_board();
        self.cpu.reset(&mut self.board, BusMaster::Cpu(0));
    }
}

impl SaveState for PacmanSystem {
    crate::machine_save_state!();
}

impl phosphor_core::core::machine::Nvram for PacmanSystem {}
impl phosphor_core::core::machine::InputConfigurable for PacmanSystem {
    fn input_controls(&self) -> &'static [phosphor_core::core::machine::InputControl] {
        namco_pac::NAMCO_PAC_CONTROLS
    }

    fn handle_input(&mut self, event: phosphor_core::core::machine::InputEvent) {
        if let phosphor_core::core::machine::InputEvent::Button { id, pressed } = event {
            self.board.handle_input(id.0 as u8, pressed);
        }
    }
}
impl phosphor_core::core::machine::Profilable for PacmanSystem {}
crate::impl_dip_switches!(PacmanSystem, namco_pac::DIP_BANKS, board.dip_switches);
crate::impl_board_debug_trace!(PacmanSystem, board);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

crate::register_machine!(
    PacmanSystem,
    "pacman",
    &["pacman"],
    namco_pac::NAMCO_PAC_CONTROLS
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namco_pac::Region;
    use phosphor_core::core::machine::DipSwitches;
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
        let cpu_snap = sys.cpu.snapshot();

        // Mutate everything
        let mut sys2 = PacmanSystem::new();
        sys2.board.map.region_data_mut(Region::VideoRam)[0x100] = 0xFF;
        sys2.board.in0 = 0x00;
        sys2.board.clock = 999;

        // Load
        sys2.load_state(&data).unwrap();

        // Verify CPU
        assert_eq!(sys2.cpu.snapshot(), cpu_snap);

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

    // -----------------------------------------------------------------------
    // DIP switches
    // -----------------------------------------------------------------------

    #[test]
    fn set_dip_option_stray_bits_are_filtered_to_the_mask() {
        // The generated suite covers every defined choice; this pins the
        // behaviour for a value that is not one — Lives is option index 1
        // (mask 0x0C), and 0xFF must land as 0x0C, not 0xFF.
        let mut sys = PacmanSystem::new();
        sys.set_dip_option(0, 1, 0xFF);
        assert_eq!(sys.dip_bank_value(0) & 0x0C, 0x0C);
        assert_eq!(sys.dip_bank_value(0) & !0x0C, 0xC9 & !0x0C);
    }
}

// Historical power-on byte: 1C/1C, 3 lives, 10000 bonus, normal difficulty,
// normal ghosts.
#[cfg(test)]
crate::dip_test_suite!(PacmanSystem, &[0xC9]);
