//! Dig Dug II (Namco, 1985).
//!
//! No schematics of this game are known. It runs on Mappy hardware (the
//! scrolling playfield gives it away): the same CPUs, maps, video pipeline,
//! sound chain and latch, with its own ROMs, a 56XX in place of the second
//! 58XX, twice the sprite ROM, and the program ROM filling 0x8000-0xFFFF.
//! Everything shared lives in [`crate::namco_mappy`], parameterized by
//! [`MappyVariant::DigDug2`]; this file holds only the ROMs, DIPs, controls,
//! system glue, and tests. MAME's `namco/mappy.cpp` (`digdug2`) is the
//! behavioral reference throughout, transcribed to
//! `docs/schematics/digdug2-board.md`.

use phosphor_core::core::machine::{
    ActionRole, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, Direction, InputConfigurable,
    InputControl, InputEvent, InputId, InputKind, MachineCore, Nvram, Profilable, SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::m6809::M6809;
use phosphor_core::cpu::{Cpu, CpuStateTrait};
use phosphor_macros::{BusDebug, Saveable};

use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::namco_mappy::{MappyBoard, MappyVariant, TIMING};
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};

// Input button IDs.
pub const INPUT_COIN1: u8 = 0;
pub const INPUT_COIN2: u8 = 1;
pub const INPUT_SERVICE: u8 = 2;
pub const INPUT_P1_START: u8 = 3;
pub const INPUT_P2_START: u8 = 4;
pub const INPUT_P1_UP: u8 = 5;
pub const INPUT_P1_RIGHT: u8 = 6;
pub const INPUT_P1_DOWN: u8 = 7;
pub const INPUT_P1_LEFT: u8 = 8;
pub const INPUT_P1_PUMP: u8 = 9;
pub const INPUT_P1_DRILL: u8 = 10;
pub const INPUT_P2_UP: u8 = 11;
pub const INPUT_P2_RIGHT: u8 = 12;
pub const INPUT_P2_DOWN: u8 = 13;
pub const INPUT_P2_LEFT: u8 = 14;
pub const INPUT_P2_PUMP: u8 = 15;
pub const INPUT_P2_DRILL: u8 = 16;
pub const INPUT_SERVICE_MODE: u8 = 17;

// ---------------------------------------------------------------------------
// ROM definitions: the `digdug2` set (New Ver.)
// ---------------------------------------------------------------------------

pub static DIGDUG2_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        RomEntry {
            name: "d23_3.1d",
            size: 0x4000,
            offset: 0x0000,
            crc32: &[0xcc155338],
        },
        RomEntry {
            name: "d23_1.1b",
            size: 0x4000,
            offset: 0x4000,
            crc32: &[0x40e46af8],
        },
    ],
};

pub static DIGDUG2_SOUND_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[RomEntry {
        name: "d21_4.1k",
        size: 0x2000,
        offset: 0x0000,
        crc32: &[0x737443b1],
    }],
};

pub static DIGDUG2_TILE_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "d21_5.3b",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0xafcb4509],
    }],
};

/// The two sprite ROMs back-to-back (16K each), de-interleaved even/odd on
/// load into 256 16x16 4bpp codes.
pub static DIGDUG2_SPRITE_ROM: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        RomEntry {
            name: "d21_6.3m",
            size: 0x4000,
            offset: 0x0000,
            crc32: &[0xdf1f4ad8],
        },
        RomEntry {
            name: "d21_7.3n",
            size: 0x4000,
            offset: 0x4000,
            crc32: &[0xccadb3ea],
        },
    ],
};

/// Palette (5B), char lookup (4C), sprite lookup (5K): same layout as Mappy.
pub static DIGDUG2_COLOR_PROMS: RomRegion = RomRegion {
    size: 0x0220,
    entries: &[
        RomEntry {
            name: "d21-5.5b",
            size: 0x0020,
            offset: 0x0000,
            crc32: &[0x9b169db5],
        },
        RomEntry {
            name: "d21-6.4c",
            size: 0x0100,
            offset: 0x0020,
            crc32: &[0x55a88695],
        },
        RomEntry {
            name: "d21-7.5k",
            size: 0x0100,
            offset: 0x0120,
            crc32: &[0x9c55feda],
        },
    ],
};

/// The 15XX's waveforms (3M on the CPU board).
pub static DIGDUG2_SOUND_PROM: RomRegion = RomRegion {
    size: 0x0100,
    entries: &[RomEntry {
        name: "d21-3.3m",
        size: 0x0100,
        offset: 0x0000,
        crc32: &[0xe0074ee2],
    }],
};

// ---------------------------------------------------------------------------
// DIP switches. Values are pin levels: the MCUs invert what they read, so a
// switch that is ON reads 0 here. MAME-sourced throughout (no manual is
// known); see `docs/schematics/digdug2-board.md`.
// ---------------------------------------------------------------------------

const DSW1_OPTIONS: &[DipOption] = &[
    DipOption {
        name: "Service Mode",
        mask: 0x01,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "Off",
                value: 0x01,
            },
            DipChoice {
                label: "On",
                value: 0x00,
            },
        ],
        conditional: &[],
    },
    DipOption {
        name: "Lives",
        mask: 0x02,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "3",
                value: 0x02,
            },
            DipChoice {
                label: "5",
                value: 0x00,
            },
        ],
        conditional: &[],
    },
    DipOption {
        name: "Coinage",
        mask: 0x0C,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "1C/1C",
                value: 0x0C,
            },
            DipChoice {
                label: "2C/1C",
                value: 0x08,
            },
            DipChoice {
                label: "1C/2C",
                value: 0x04,
            },
            DipChoice {
                label: "3C/1C",
                value: 0x00,
            },
        ],
        conditional: &[],
    },
    DipOption {
        name: "Bonus Life",
        mask: 0x30,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "30k 80k and ...",
                value: 0x30,
            },
            DipChoice {
                label: "30k 100k and ...",
                value: 0x20,
            },
            DipChoice {
                label: "30k 120k and ...",
                value: 0x10,
            },
            DipChoice {
                label: "30k 150k and ...",
                value: 0x00,
            },
        ],
        conditional: &[],
    },
    DipOption {
        name: "Level Select",
        mask: 0x40,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "Off",
                value: 0x40,
            },
            DipChoice {
                label: "On",
                value: 0x00,
            },
        ],
        conditional: &[],
    },
    DipOption {
        name: "Freeze",
        mask: 0x80,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "Off",
                value: 0x80,
            },
            DipChoice {
                label: "On",
                value: 0x00,
            },
        ],
        conditional: &[],
    },
];

const DSW0_OPTIONS: &[DipOption] = &[DipOption {
    name: "Cabinet",
    mask: 0x04,
    apply: DipApplyTiming::Immediate,
    choices: &[
        DipChoice {
            label: "Upright",
            value: 0x04,
        },
        DipChoice {
            label: "Cocktail",
            value: 0x00,
        },
    ],
    conditional: &[],
}];

/// Bank 2 (SW3) is entirely unused on this game and has no bank here; the
/// field stays at its 0xFF default.
pub(crate) const DIGDUG2_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "SW2",
        options: DSW1_OPTIONS,
    },
    DipSwitchBank {
        name: "Cabinet",
        options: DSW0_OPTIONS,
    },
];

// ---------------------------------------------------------------------------
// Inputs: 4-way sticks, pump and drill per player, and a service-mode button
// beside the usual service coin.
// ---------------------------------------------------------------------------

pub const DIGDUG2_CONTROLS: &[InputControl] = &[
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
        id: InputId(INPUT_P1_PUMP as u16),
        stable_name: "p1_pump",
        label: "P1 Pump",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P1_DRILL as u16),
        stable_name: "p1_drill",
        label: "P1 Drill",
        kind: InputKind::Action(ActionRole::Secondary),
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
        id: InputId(INPUT_P2_PUMP as u16),
        stable_name: "p2_pump",
        label: "P2 Pump",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(2),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P2_DRILL as u16),
        stable_name: "p2_drill",
        label: "P2 Drill",
        kind: InputKind::Action(ActionRole::Secondary),
        player: Some(2),
        default_bindings: &[],
    },
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
        id: InputId(INPUT_SERVICE as u16),
        stable_name: "service",
        label: "Service Coin",
        kind: InputKind::Service,
        player: None,
        default_bindings: crate::input_defaults::SERVICE,
    },
    InputControl {
        id: InputId(INPUT_SERVICE_MODE as u16),
        stable_name: "service_mode",
        label: "Service Mode",
        kind: InputKind::Service,
        player: None,
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P1_START as u16),
        stable_name: "p1_start",
        label: "P1 Start",
        kind: InputKind::Start,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_START,
    },
    InputControl {
        id: InputId(INPUT_P2_START as u16),
        stable_name: "p2_start",
        label: "P2 Start",
        kind: InputKind::Start,
        player: Some(2),
        default_bindings: crate::input_defaults::P2_START,
    },
];

// ---------------------------------------------------------------------------
// The machine
// ---------------------------------------------------------------------------

/// Dig Dug II: two 6809s beside a Mappy board running the Dig Dug II variant.
#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct DigDug2System {
    #[debug_cpu("M6809 Main")]
    #[save(id = 1)]
    pub main: M6809,
    #[debug_cpu("M6809 Sound")]
    #[save(id = 2)]
    pub sub: M6809,
    #[debug_bus]
    #[save(id = 3)]
    pub board: MappyBoard,
}

impl DigDug2System {
    pub fn new() -> Self {
        Self {
            main: M6809::new(),
            sub: M6809::new(),
            board: MappyBoard::new(MappyVariant::DigDug2),
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        self.board
            .load_program_rom(&DIGDUG2_PROGRAM_ROM.load(rom_set)?);
        self.board.load_sub_rom(&DIGDUG2_SOUND_ROM.load(rom_set)?);
        self.board.load_tile_rom(&DIGDUG2_TILE_ROM.load(rom_set)?);
        self.board
            .load_sprite_rom(&DIGDUG2_SPRITE_ROM.load(rom_set)?);
        self.board.load_proms(&DIGDUG2_COLOR_PROMS.load(rom_set)?);
        self.board
            .load_sound_prom(&DIGDUG2_SOUND_PROM.load(rom_set)?);
        Ok(())
    }

    /// Active-low: pressing clears the bit.
    fn apply_input(&mut self, button: u8, pressed: bool) {
        let b = &mut self.board;
        match button {
            INPUT_COIN1 => crate::set_bit_active_low(&mut b.in_coins, 0, pressed),
            INPUT_COIN2 => crate::set_bit_active_low(&mut b.in_coins, 1, pressed),
            INPUT_SERVICE => crate::set_bit_active_low(&mut b.in_coins, 3, pressed),
            INPUT_P1_UP => crate::set_bit_active_low(&mut b.in_p1, 0, pressed),
            INPUT_P1_RIGHT => crate::set_bit_active_low(&mut b.in_p1, 1, pressed),
            INPUT_P1_DOWN => crate::set_bit_active_low(&mut b.in_p1, 2, pressed),
            INPUT_P1_LEFT => crate::set_bit_active_low(&mut b.in_p1, 3, pressed),
            INPUT_P2_UP => crate::set_bit_active_low(&mut b.in_p2, 0, pressed),
            INPUT_P2_RIGHT => crate::set_bit_active_low(&mut b.in_p2, 1, pressed),
            INPUT_P2_DOWN => crate::set_bit_active_low(&mut b.in_p2, 2, pressed),
            INPUT_P2_LEFT => crate::set_bit_active_low(&mut b.in_p2, 3, pressed),
            INPUT_P1_PUMP => crate::set_bit_active_low(&mut b.in_buttons, 0, pressed),
            INPUT_P2_PUMP => crate::set_bit_active_low(&mut b.in_buttons, 1, pressed),
            INPUT_P1_START => crate::set_bit_active_low(&mut b.in_buttons, 2, pressed),
            INPUT_P2_START => crate::set_bit_active_low(&mut b.in_buttons, 3, pressed),
            INPUT_P1_DRILL => crate::set_bit_active_low(&mut b.in_buttons2, 0, pressed),
            INPUT_P2_DRILL => crate::set_bit_active_low(&mut b.in_buttons2, 1, pressed),
            INPUT_SERVICE_MODE => b.in_service_mode = pressed,
            _ => {}
        }
    }

    pub fn get_cpu_state(&self) -> phosphor_core::cpu::state::M6809State {
        self.main.snapshot()
    }

    /// One cycle, returning the instruction-boundary mask.
    pub fn step_cycle(&mut self) -> u32 {
        crate::namco_mappy::tick(&mut self.main, &mut self.sub, &mut self.board);
        self.board.instruction_boundaries(&self.main, &self.sub)
    }

    /// The CPU-facing bus, side effects and all.
    pub fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        self.board.read(master, addr)
    }

    pub fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.board.write(master, addr, data);
    }
}

impl Default for DigDug2System {
    fn default() -> Self {
        Self::new()
    }
}

crate::impl_board_delegation!(DigDug2System, board, TIMING, orientation);

impl MachineCore for DigDug2System {
    crate::machine_core_metadata!("digdug2", TIMING, crate::namco_mappy::clock_tree);

    fn gfx_sheets(&self) -> Vec<phosphor_core::core::machine::GfxSheet<'_>> {
        use phosphor_core::core::machine::GfxSheet;
        let v = &self.board.video;
        vec![
            GfxSheet {
                name: "chars",
                cache: v.tile_cache(),
                palette: v.char_pens(),
            },
            GfxSheet {
                name: "sprites",
                cache: v.sprite_cache(),
                palette: v.sprite_pens(),
            },
        ]
    }

    fn run_frame(&mut self) {
        crate::namco_mappy::run_frame(&mut self.main, &mut self.sub, &mut self.board);
    }

    fn reset(&mut self) {
        self.board.reset_board();
        self.main.reset(&mut self.board, BusMaster::Cpu(0));
        // The sound CPU sits in reset until the main CPU raises SUB RESET.
    }
}

impl SaveState for DigDug2System {
    crate::machine_save_state!();
}

impl Nvram for DigDug2System {}
impl Profilable for DigDug2System {}

impl InputConfigurable for DigDug2System {
    fn input_controls(&self) -> &'static [InputControl] {
        DIGDUG2_CONTROLS
    }
    fn handle_input(&mut self, event: InputEvent) {
        if let InputEvent::Button { id, pressed } = event {
            self.apply_input(id.0 as u8, pressed);
        }
    }
}

crate::impl_dip_switches!(DigDug2System, DIGDUG2_DIP_BANKS, board.dsw1, board.dsw0);

crate::impl_board_debug_trace!(DigDug2System, board);

crate::register_machine!(DigDug2System, "digdug2", &["digdug2"], DIGDUG2_CONTROLS);

inventory::submit! {
    DisasmRegion {
        machine: "digdug2",
        region: "main",
        cpu: DisasmCpu::M6809,
        org: 0x8000,
        size: DIGDUG2_PROGRAM_ROM.size as u32,
        load: |rs| DIGDUG2_PROGRAM_ROM.load(rs),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "digdug2",
        region: "sound",
        cpu: DisasmCpu::M6809,
        org: 0xE000,
        size: DIGDUG2_SOUND_ROM.size as u32,
        load: |rs| DIGDUG2_SOUND_ROM.load(rs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::namco_mappy::{DEFAULT_DSW0, DEFAULT_DSW1};
    use phosphor_core::core::machine::{Orientation, Renderable};

    crate::dip_test_suite!(DigDug2System, &[DEFAULT_DSW1, DEFAULT_DSW0]);

    /// A system whose two CPUs each spin on `BRA *`, so nothing the test does
    /// is disturbed by a program running out of zeroed memory.
    fn parked() -> DigDug2System {
        let mut sys = DigDug2System::new();
        let mut rom = vec![0u8; 0x8000];
        rom[0] = 0x20; // BRA *
        rom[1] = 0xFE;
        rom[0x7FFE] = 0x80; // reset vector 0x8000
        sys.board.load_program_rom(&rom);
        let mut snd = vec![0u8; 0x2000];
        snd[0] = 0x20;
        snd[1] = 0xFE;
        snd[0x1FFE] = 0xE0; // reset vector 0xE000
        sys.board.load_sub_rom(&snd);
        sys.reset();
        sys
    }

    /// The LS259 address for output `line` taking value `bit`.
    fn latch_addr(base: u16, line: u16, bit: bool) -> u16 {
        base | (line << 1) | u16::from(bit)
    }

    #[test]
    fn machine_identity_and_geometry() {
        let sys = DigDug2System::new();
        assert_eq!(sys.machine_id(), "digdug2");
        assert_eq!(sys.display_size(), (288, 224));
        assert_eq!(sys.orientation(), Orientation::ROT90);
        assert_eq!(TIMING.cycles_per_frame(), 25_344);
        assert_eq!(
            sys.board.variant,
            MappyVariant::DigDug2,
            "the board runs the Dig Dug II variant"
        );
    }

    #[test]
    fn the_program_rom_fills_0x8000_through_0xffff() {
        let mut sys = parked();
        let rom: Vec<u8> = (0..0x8000).map(|i| (i ^ (i >> 8)) as u8).collect();
        sys.board.load_program_rom(&rom);
        for a in [0x000u16, 0x123, 0x3FFF] {
            assert_eq!(
                sys.bus_read(BusMaster::Cpu(0), 0x8000 + a),
                rom[a as usize],
                "low half at {a:#X}"
            );
            assert_eq!(
                sys.bus_read(BusMaster::Cpu(0), 0xC000 + a),
                rom[0x4000 + a as usize],
                "high half at {a:#X}"
            );
        }
        // The halves differ, so this is a full 32K rather than a mirror.
        assert_ne!(
            sys.bus_read(BusMaster::Cpu(0), 0x8000),
            sys.bus_read(BusMaster::Cpu(0), 0xC000)
        );
    }

    #[test]
    fn the_56xx_pair_reads_the_dips_and_the_live_port_d() {
        let mut sys = parked();
        sys.board.dsw1 = 0x3C;
        sys.board.dsw2 = 0xA5;
        sys.board.dsw0 = 0x04;
        // Chip 1 (56XX) in mode 9, chip 0 (58XX) in mode 1.
        sys.bus_write(BusMaster::Cpu(0), 0x4818, 9);
        sys.bus_write(BusMaster::Cpu(0), 0x4808, 1);
        sys.board.run_io();
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x4810), 0xF0, "in reset");
        sys.bus_write(BusMaster::Cpu(0), latch_addr(0x5000, 4, true), 0);
        sys.board.run_io();
        let n = |sys: &mut DigDug2System, a| sys.bus_read(BusMaster::Cpu(0), a) & 0x0F;
        // SW3 through the mux, SW2 on ports B and C.
        assert_eq!(n(&mut sys, 0x4810), !0x05u8 & 0x0F);
        assert_eq!(n(&mut sys, 0x4811), !0x0Au8 & 0x0F);
        assert_eq!(n(&mut sys, 0x4812), !0x0Cu8 & 0x0F);
        assert_eq!(n(&mut sys, 0x4814), !0x03u8 & 0x0F);
        // Port D with nothing pressed: drills and service high, upright.
        assert_eq!(n(&mut sys, 0x4816), !0x0Fu8 & 0x0F);
        // P1 drill and service mode pressed: live bits 0 and 3 go low while
        // the cabinet DIP on bit 2 stays.
        sys.apply_input(INPUT_P1_DRILL, true);
        sys.apply_input(INPUT_SERVICE_MODE, true);
        sys.board.run_io();
        assert_eq!(n(&mut sys, 0x4816), !0x06u8 & 0x0F);
    }

    #[test]
    fn the_sticks_are_4_way_and_reach_chip_0() {
        let mut sys = parked();
        sys.bus_write(BusMaster::Cpu(0), 0x4808, 1);
        sys.bus_write(BusMaster::Cpu(0), latch_addr(0x5000, 4, true), 0);
        // Up and down are wired here (unlike Mappy's 2-way sticks).
        sys.apply_input(INPUT_P1_UP, true);
        sys.apply_input(INPUT_P1_DOWN, true);
        sys.board.run_io();
        // Chip 0, mode 1: P1 is port B at nibble 5, inverted.
        assert_eq!(
            sys.bus_read(BusMaster::Cpu(0), 0x4805) & 0x0F,
            !0x0Au8 & 0x0F
        );
    }

    #[test]
    fn the_sprite_rom_decodes_256_codes() {
        let mut sys = parked();
        sys.board.load_sprite_rom(&[0x33; 0x8000]);
        assert_eq!(sys.board.video.sprite_cache().count(), 256);
    }
}
