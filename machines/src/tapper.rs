//! Tapper (1983, Bally Midway), on the 91490 CPU board. See `mcr_91490.rs` for
//! the hardware.
//!
//! The set is the Budweiser release with program ROMs labeled 1/27/84, which is
//! the newest of the dated revisions. The others (12/9/83, 1/12/84, the
//! alternate-graphics set, Suntory and Root Beer) are not in the catalog.

use phosphor_core::core::machine::{
    ActionRole, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, DipSwitches, Direction,
    InputConfigurable, InputControl, InputEvent, InputId, InputKind, MachineCore, Nvram,
    Profilable, SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_macros::Saveable;

use crate::mcr_91490::{self, Mcr91490Board};
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_low;

// ---------------------------------------------------------------------------
// ROM definitions
// ---------------------------------------------------------------------------

static TAPPER_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0xE000, // 3 × 16KB + 8KB
    entries: &[
        RomEntry {
            name: "tapper_c.p.u._pg_0_1c_1-27-84.1c",
            size: 0x4000,
            offset: 0x0000,
            crc32: &[0xbb060bb0],
        },
        RomEntry {
            name: "tapper_c.p.u._pg_1_2c_1-27-84.2c",
            size: 0x4000,
            offset: 0x4000,
            crc32: &[0xfd9acc22],
        },
        RomEntry {
            name: "tapper_c.p.u._pg_2_3c_1-27-84.3c",
            size: 0x4000,
            offset: 0x8000,
            crc32: &[0xb3755d41],
        },
        RomEntry {
            name: "tapper_c.p.u._pg_3_4c_1-27-84.4c",
            size: 0x2000,
            offset: 0xC000,
            crc32: &[0x77273096],
        },
    ],
};

static TAPPER_SOUND_ROM: RomRegion = RomRegion {
    size: 0x4000, // 4 × 4KB
    entries: &[
        RomEntry {
            name: "tapper_sound_snd_0_a7_12-7-83.a7",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x0e8bb9d5],
        },
        RomEntry {
            name: "tapper_sound_snd_1_a8_12-7-83.a8",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x0cf0e29b],
        },
        RomEntry {
            name: "tapper_sound_snd_2_a9_12-7-83.a9",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0x31eb6dc6],
        },
        RomEntry {
            name: "tapper_sound_snd_3_a10_12-7-83.a10",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0x01a9be6a],
        },
    ],
};

/// Background tiles: BG 1 at 6F is the first half, BG 0 at 5F the second.
static TAPPER_BG_ROM: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        RomEntry {
            name: "tapper_c.p.u._bg_1_6f_12-7-83.6f",
            size: 0x4000,
            offset: 0x0000,
            crc32: &[0x2a30238c],
        },
        RomEntry {
            name: "tapper_c.p.u._bg_0_5f_12-7-83.5f",
            size: 0x4000,
            offset: 0x4000,
            crc32: &[0x394ab576],
        },
    ],
};

/// Objects: eight ROMs on the 91464, paired odd-then-even into four quarters.
static TAPPER_FG_ROM: RomRegion = RomRegion {
    size: 0x20000,
    entries: &[
        RomEntry {
            name: "tapper_video_fg_1_a7_12-7-83.a7",
            size: 0x4000,
            offset: 0x00000,
            crc32: &[0x32509011],
        },
        RomEntry {
            name: "tapper_video_fg_0_a8_12-7-83.a8",
            size: 0x4000,
            offset: 0x04000,
            crc32: &[0x8412c808],
        },
        RomEntry {
            name: "tapper_video_fg_3_a5_12-7-83.a5",
            size: 0x4000,
            offset: 0x08000,
            crc32: &[0x818fffd4],
        },
        RomEntry {
            name: "tapper_video_fg_2_a6_12-7-83.a6",
            size: 0x4000,
            offset: 0x0C000,
            crc32: &[0x67e37690],
        },
        RomEntry {
            name: "tapper_video_fg_5_a3_12-7-83.a3",
            size: 0x4000,
            offset: 0x10000,
            crc32: &[0x800f7c8a],
        },
        RomEntry {
            name: "tapper_video_fg_4_a4_12-7-83.a4",
            size: 0x4000,
            offset: 0x14000,
            crc32: &[0x32674ee6],
        },
        RomEntry {
            name: "tapper_video_fg_7_a1_12-7-83.a1",
            size: 0x4000,
            offset: 0x18000,
            crc32: &[0x070b4c81],
        },
        RomEntry {
            name: "tapper_video_fg_6_a2_12-7-83.a2",
            size: 0x4000,
            offset: 0x1C000,
            crc32: &[0xa37aef36],
        },
    ],
};

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

// IP0: coins, starts, tilt, service (active-low)
const INPUT_COIN1: u8 = 0;
const INPUT_COIN2: u8 = 1;
const INPUT_START1: u8 = 2;
const INPUT_START2: u8 = 3;
const INPUT_TILT: u8 = 4;
const INPUT_SERVICE: u8 = 5;

// IP1: player 1 (active-low). IP2 carries player 2 in the same layout, for
// the cocktail cabinet; an upright alternates players on the one handle.
const INPUT_P1_RIGHT: u8 = 6;
const INPUT_P1_LEFT: u8 = 7;
const INPUT_P1_DOWN: u8 = 8;
const INPUT_P1_UP: u8 = 9;
const INPUT_P1_TAP: u8 = 10;
const INPUT_P2_RIGHT: u8 = 11;
const INPUT_P2_LEFT: u8 = 12;
const INPUT_P2_DOWN: u8 = 13;
const INPUT_P2_UP: u8 = 14;
const INPUT_P2_TAP: u8 = 15;

const TAPPER_CONTROLS: &[InputControl] = &[
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
        default_bindings: &[],
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
        id: InputId(INPUT_TILT as u16),
        stable_name: "tilt",
        label: "Tilt",
        kind: InputKind::Button,
        player: None,
        default_bindings: &[],
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
        id: InputId(INPUT_P1_UP as u16),
        stable_name: "p1_up",
        label: "P1 Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_UP,
    },
    InputControl {
        id: InputId(INPUT_P1_DOWN as u16),
        stable_name: "p1_down",
        label: "P1 Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_DOWN,
    },
    InputControl {
        id: InputId(INPUT_P1_LEFT as u16),
        stable_name: "p1_left",
        label: "P1 Left",
        kind: InputKind::DigitalDirection {
            direction: Direction::Left,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(INPUT_P1_RIGHT as u16),
        stable_name: "p1_right",
        label: "P1 Right",
        kind: InputKind::DigitalDirection {
            direction: Direction::Right,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
    InputControl {
        id: InputId(INPUT_P1_TAP as u16),
        stable_name: "p1_tap",
        label: "P1 Tap",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P2_UP as u16),
        stable_name: "p2_up",
        label: "P2 Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(2),
        default_bindings: crate::input_defaults::P2_UP,
    },
    InputControl {
        id: InputId(INPUT_P2_DOWN as u16),
        stable_name: "p2_down",
        label: "P2 Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(2),
        default_bindings: crate::input_defaults::P2_DOWN,
    },
    InputControl {
        id: InputId(INPUT_P2_LEFT as u16),
        stable_name: "p2_left",
        label: "P2 Left",
        kind: InputKind::DigitalDirection {
            direction: Direction::Left,
        },
        player: Some(2),
        default_bindings: crate::input_defaults::P2_LEFT,
    },
    InputControl {
        id: InputId(INPUT_P2_RIGHT as u16),
        stable_name: "p2_right",
        label: "P2 Right",
        kind: InputKind::DigitalDirection {
            direction: Direction::Right,
        },
        player: Some(2),
        default_bindings: crate::input_defaults::P2_RIGHT,
    },
    InputControl {
        id: InputId(INPUT_P2_TAP as u16),
        stable_name: "p2_tap",
        label: "P2 Tap",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(2),
        default_bindings: &[],
    },
];

// ---------------------------------------------------------------------------
// TapperSystem
// ---------------------------------------------------------------------------

#[derive(Saveable, phosphor_macros::BusDebug)]
pub struct TapperSystem {
    /// The Z80 is held beside the board, which is its bus.
    #[debug_cpu("Z80 Main")]
    pub cpu: phosphor_core::cpu::z80::Z80,

    #[debug_bus]
    pub board: Mcr91490Board,
}

impl TapperSystem {
    pub fn new() -> Self {
        let mut sys = Self {
            cpu: phosphor_core::cpu::z80::Z80::new(),
            board: Mcr91490Board::new(),
        };
        // Switch positions are set once, here, and kept through a reset.
        sys.board.ssio.set_input_port(3, TAPPER_IP3_POWER_ON);
        // Switch 3 at D14 on the Sound I/O board, read by the sound CPU. The
        // card gives it only diagnostic modes, all off for play; with SW#1 off
        // the other three have no effect. Every switch is open here.
        sys.board.ssio.set_dip_switches(0xFF);
        sys
    }

    /// One CPU cycle. Returns 1 at an instruction boundary.
    pub fn step_cycle(&mut self) -> u32 {
        mcr_91490::tick(&mut self.cpu, &mut self.board);
        Mcr91490Board::instruction_boundaries(&self.cpu)
    }

    /// Read the CPU-facing bus, side effects and all.
    pub fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        Bus::read(&mut self.board, master, addr)
    }

    /// Write the CPU-facing bus, side effects and all.
    pub fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        Bus::write(&mut self.board, master, addr, data);
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let program = TAPPER_PROGRAM_ROM.load(rom_set)?;
        self.board.map.load_region(mcr_91490::Region::Rom, &program);

        let sound = TAPPER_SOUND_ROM.load(rom_set)?;
        self.board.ssio.load_rom(&sound);

        let bg = TAPPER_BG_ROM.load(rom_set)?;
        let fg = TAPPER_FG_ROM.load(rom_set)?;
        self.board.decode_gfx(&bg, &fg);
        Ok(())
    }
}

impl Default for TapperSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Machine traits
// ---------------------------------------------------------------------------

crate::impl_board_delegation!(TapperSystem, board, mcr_91490::TIMING);

impl InputConfigurable for TapperSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        TAPPER_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let InputEvent::Button { id, pressed } = event else {
            return;
        };
        let (port, bit) = match id.0 as u8 {
            INPUT_COIN1 => (0usize, 0u8),
            INPUT_COIN2 => (0, 1),
            INPUT_START1 => (0, 2),
            INPUT_START2 => (0, 3),
            INPUT_TILT => (0, 5),
            INPUT_SERVICE => (0, 7),
            INPUT_P1_RIGHT => (1, 0),
            INPUT_P1_LEFT => (1, 1),
            INPUT_P1_DOWN => (1, 2),
            INPUT_P1_UP => (1, 3),
            INPUT_P1_TAP => (1, 4),
            INPUT_P2_RIGHT => (2, 0),
            INPUT_P2_LEFT => (2, 1),
            INPUT_P2_DOWN => (2, 2),
            INPUT_P2_UP => (2, 3),
            INPUT_P2_TAP => (2, 4),
            _ => return,
        };
        let mut val = self.board.ssio.input_port(port);
        set_bit_active_low(&mut val, bit, pressed);
        self.board.ssio.set_input_port(port, val);
    }
}

impl MachineCore for TapperSystem {
    crate::machine_core_metadata!("tapper", mcr_91490::TIMING, mcr_91490::clock_tree);

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
        // The board renders on the frame's last cycle, so the debugger's
        // single-step path refreshes the picture too.
        mcr_91490::run_frame(&mut self.cpu, &mut self.board);
    }

    fn reset(&mut self) {
        self.board.reset_board();
        self.cpu.reset(&mut self.board, BusMaster::Cpu(0));
    }
}

impl SaveState for TapperSystem {
    crate::machine_save_state!();
}

impl Nvram for TapperSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.map.region_data(mcr_91490::Region::Nvram))
    }

    fn load_nvram(&mut self, data: &[u8]) {
        let nvram = self.board.map.region_data_mut(mcr_91490::Region::Nvram);
        let len = data.len().min(nvram.len());
        nvram[..len].copy_from_slice(&data[..len]);
    }
}

impl Profilable for TapperSystem {}

/// IP3 at power-on: every switch of Switch 1 open, which is the factory
/// setting for both switches the game reads.
const TAPPER_IP3_POWER_ON: u8 = 0xFF;

/// Tapper's option switches, from the option switch card `M051-00A11-A007`
/// (Parts and Operating Manual, Dec 1983, PDF p5).
///
/// COINAGE IS NOT ON A SWITCH. The card says the common options are set in the
/// MACHINE SETUP part of self-test, so they live in NVRAM.
///
/// Switch 1 is the 10-position DIP at B3 on the Sound I/O board, read through
/// the SSIO's IP3. A closed (ON) switch reads 0 and switch `n` is bit `n - 1`.
/// SW#3 is attract-mode sounds and SW#7 the cabinet; every other switch up to
/// SW#9 is marked not used, and the factory setting has both used switches
/// off. SW#10, freeze video, is not modeled: a tenth switch does not fit IP3.
const TAPPER_DIP_BANKS: &[DipSwitchBank] = &[DipSwitchBank {
    name: "Switch 1 (B3)",
    options: &[
        DipOption {
            name: "Attract Mode Sounds",
            mask: 0x04,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "On",
                    value: 0x00,
                },
                DipChoice {
                    label: "Off",
                    value: 0x04,
                },
            ],
            conditional: &[],
        },
        DipOption {
            name: "Cabinet",
            mask: 0x40,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "Cocktail",
                    value: 0x00,
                },
                DipChoice {
                    label: "Upright",
                    value: 0x40,
                },
            ],
            conditional: &[],
        },
    ],
}];

impl DipSwitches for TapperSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        TAPPER_DIP_BANKS
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        match bank {
            0 => self.board.ssio.input_port(3),
            _ => 0,
        }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        if bank == 0 {
            self.board.ssio.set_input_port(3, value);
        }
    }
}
crate::impl_map_debug_trace!(TapperSystem, board.map);

crate::register_machine!(TapperSystem, "tapper", &["tapper"], TAPPER_CONTROLS);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::cpu::CpuStateTrait;

    #[test]
    fn save_load_round_trip() {
        let mut sys = TapperSystem::new();
        sys.board.map.region_data_mut(mcr_91490::Region::Nvram)[0x100] = 0xAA;
        sys.board.map.region_data_mut(mcr_91490::Region::VideoRam)[0x50] = 0xBB;
        sys.board.map.region_data_mut(mcr_91490::Region::SpriteRam)[0x10] = 0xCC;
        sys.bus_write(BusMaster::Cpu(0), 0xF805, 0x49);
        sys.board.clock = 50_000;

        let data = sys.save_state().expect("save_state should return Some");
        let cpu_snap = sys.cpu.snapshot();

        let mut sys2 = TapperSystem::new();
        sys2.load_state(&data).unwrap();
        assert_eq!(sys2.cpu.snapshot(), cpu_snap);
        assert_eq!(
            sys2.board.map.region_data(mcr_91490::Region::Nvram)[0x100],
            0xAA
        );
        assert_eq!(
            sys2.board.map.region_data(mcr_91490::Region::VideoRam)[0x50],
            0xBB
        );
        assert_eq!(
            sys2.board.map.region_data(mcr_91490::Region::SpriteRam)[0x10],
            0xCC
        );
        assert_eq!(sys2.board.palette_words[2], 0x149);
        assert_eq!(sys2.board.palette_rgb, sys.board.palette_rgb);
        assert_eq!(sys2.board.clock, 50_000);
    }

    #[test]
    fn inputs_are_active_low_on_the_documented_bits() {
        let mut sys = TapperSystem::new();
        for (id, port, mask) in [
            (INPUT_COIN1, 0, 0x01),
            (INPUT_START2, 0, 0x08),
            (INPUT_SERVICE, 0, 0x80),
            (INPUT_P1_UP, 1, 0x08),
            (INPUT_P1_TAP, 1, 0x10),
            (INPUT_P2_LEFT, 2, 0x02),
        ] {
            sys.handle_input(InputEvent::Button {
                id: InputId(u16::from(id)),
                pressed: true,
            });
            assert_eq!(sys.board.ssio.input_port(port), !mask);
            sys.handle_input(InputEvent::Button {
                id: InputId(u16::from(id)),
                pressed: false,
            });
            assert_eq!(sys.board.ssio.input_port(port), 0xFF);
        }
    }

    /// A switch keeps its position through a reset.
    #[test]
    fn a_reset_keeps_the_switch_positions() {
        let mut sys = TapperSystem::new();
        sys.set_dip_option(0, 1, 0x00); // Cabinet: cocktail
        MachineCore::reset(&mut sys);
        assert_eq!(sys.dip_bank_value(0), 0xBF);
    }
}

#[cfg(test)]
crate::dip_test_suite!(TapperSystem, &[TAPPER_IP3_POWER_ON]);
