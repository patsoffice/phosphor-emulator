//! Atari Black Widow (1983), registered as `blackwidow` (ROM set `bwidow`).
//!
//! The board is [`crate::atari_color_vector_conversions`], shared with
//! Gravitar (Black Widow was sold as a conversion of it); this file is
//! what is Black Widow's own: its ROMs, its twin joysticks, its switch tables
//! and the four audio part values that differ from Gravitar's.
//!
//! - **Controls**, from SP-234 sheet 3A: the move stick on $8000 bits 3-0
//!   (up, down, left, right) and the fire stick on $8800 bits 3-0 in the same
//!   order, with the starts on $8800 bits 5 and 6. All active low.
//! - **Switches**: D4 (C/D3's ALLPOT) is coinage and B4 (B3's) the game
//!   options, TM-234 tables 1-2 and 1-4.
//! - **Audio**: sheet 7A fits C27 at 100 pF (handwritten on the drawing) and
//!   R46 at 22k, and no C34.
//! - **Display**: a 480 x 440 field. The monitor's pots set this on
//!   hardware and no drawing gives it.
//!
//! The prototype `bwidowp` runs on a different memory map and is not carried
//! here (`phosphor-emulator-quwu.6`).

use crate::atari_avg;
use crate::atari_color_vector_conversions::{
    AtariColorVectorConversionsBoard, AudioParts, ConversionRomConfig, Decode, timing,
};
use crate::rom_loader::{RomLoadError, RomRegion, RomSet};
use crate::{choice, option, set_bit_active_low};
use phosphor_core::audio::SampleRing;
use phosphor_core::core::machine::{
    DipSwitchBank, Direction, InputConfigurable, InputControl, InputEvent, InputId, InputKind,
    MachineCore, Nvram, Profilable, Renderable, SaveState, TimingConfig,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::device::dvg::{VectorLine, raster_size_for_field};
use phosphor_macros::{BusDebug, Saveable};

// ---------------------------------------------------------------------------
// ROM sets
// ---------------------------------------------------------------------------

static VECTOR_ROM: RomRegion = RomRegion {
    size: 0x3800,
    entries: &[
        rom!("136017-107.l7", 0x0800, 0x0000, 0x97f6000c),
        rom!("136017-108.mn7", 0x1000, 0x0800, 0x3da354ed),
        rom!("136017-109.np7", 0x1000, 0x1800, 0x2fc4ce79),
        rom!("136017-110.r7", 0x1000, 0x2800, 0x0dd52987),
    ],
};

static PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        rom!("136017-101.d1", 0x1000, 0x0000, 0xfe3febb7),
        rom!("136017-102.ef1", 0x1000, 0x1000, 0x10ad0376),
        rom!("136017-103.h1", 0x1000, 0x2000, 0x8a1430ee),
        rom!("136017-104.j1", 0x1000, 0x3000, 0x44f9943f),
        rom!("136017-105.kl1", 0x1000, 0x4000, 0x1fdf801c),
        rom!("136017-106.m1", 0x1000, 0x5000, 0xccc9b26c),
    ],
};

/// Black Widow, the production release.
pub static BWIDOW_CONFIG: ConversionRomConfig = ConversionRomConfig {
    set: "bwidow",
    vector: &VECTOR_ROM,
    program: &PROGRAM_ROM,
};

const ALL_CONFIGS: &[&ConversionRomConfig] = &[&BWIDOW_CONFIG];

// ---------------------------------------------------------------------------
// The machine
// ---------------------------------------------------------------------------

const TIMING: TimingConfig = timing(480, 440);

/// Sheet 7A's part values.
const AUDIO: AudioParts = AudioParts {
    c27_farads: 100e-12,
    r46_ohms: 22_000.0,
    c34_farads: None,
};

#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct BlackWidowSystem {
    #[debug_cpu("M6502")]
    #[save(id = 1)]
    cpu: M6502,
    #[debug_bus]
    #[save(id = 2)]
    board: AtariColorVectorConversionsBoard,
    #[save_skip(default)]
    audio_buffer: SampleRing<i16>,
}

impl BlackWidowSystem {
    pub fn new() -> Self {
        let mut board =
            AtariColorVectorConversionsBoard::new(Decode::GravitarBlackWidow, &TIMING, &AUDIO);
        board.dsw_d4 = DSW_D4_DEFAULT;
        board.dsw_b4 = DSW_B4_DEFAULT;
        Self {
            cpu: M6502::new(),
            board,
            audio_buffer: SampleRing::with_capacity(2048),
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        self.load_roms(rom_set, &BWIDOW_CONFIG)
    }

    fn load_roms(
        &mut self,
        rom_set: &RomSet,
        config: &ConversionRomConfig,
    ) -> Result<(), RomLoadError> {
        self.board.load_roms(rom_set, config)
    }

    /// Advance one CPU cycle, returning the instruction-boundary mask.
    pub fn step_cycle(&mut self) -> u32 {
        self.board.tick(&mut self.cpu);
        u32::from(self.cpu.at_instruction_boundary())
    }

    /// Read the CPU-facing bus, side effects and all. Distinct from the
    /// debugger's `BusDebug::peek`/`poke`, which avoid side effects.
    pub fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        self.board.read(master, addr)
    }

    /// Write the CPU-facing bus, side effects and all.
    pub fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.board.write(master, addr, data)
    }
}

impl Default for BlackWidowSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------

const INPUT_COIN1: u8 = 0;
const INPUT_COIN2: u8 = 1;
const INPUT_COIN_AUX: u8 = 2;
const INPUT_SERVICE: u8 = 3;
const INPUT_P1_START: u8 = 4;
const INPUT_P2_START: u8 = 5;
const INPUT_MOVE_UP: u8 = 6;
const INPUT_MOVE_DOWN: u8 = 7;
const INPUT_MOVE_LEFT: u8 = 8;
const INPUT_MOVE_RIGHT: u8 = 9;
const INPUT_FIRE_UP: u8 = 10;
const INPUT_FIRE_DOWN: u8 = 11;
const INPUT_FIRE_LEFT: u8 = 12;
const INPUT_FIRE_RIGHT: u8 = 13;

const fn direction(
    id: u8,
    name: &'static str,
    label: &'static str,
    dir: Direction,
) -> InputControl {
    InputControl {
        id: InputId(id as u16),
        stable_name: name,
        label,
        kind: InputKind::DigitalDirection { direction: dir },
        player: Some(1),
        default_bindings: match dir {
            Direction::Up => crate::input_defaults::P1_UP,
            Direction::Down => crate::input_defaults::P1_DOWN,
            Direction::Left => crate::input_defaults::P1_LEFT,
            Direction::Right => crate::input_defaults::P1_RIGHT,
        },
    }
}

const BWIDOW_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_COIN1 as u16),
        stable_name: "coin1",
        label: "Coin 1 (Left)",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(INPUT_COIN2 as u16),
        stable_name: "coin2",
        label: "Coin 2 (Right)",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_COIN_AUX as u16),
        stable_name: "coin_aux",
        label: "Coin Aux",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_SERVICE as u16),
        stable_name: "service",
        label: "Self-Test",
        kind: InputKind::Service,
        player: None,
        default_bindings: crate::input_defaults::SERVICE,
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
    direction(INPUT_MOVE_UP, "move_up", "Move Up", Direction::Up),
    direction(INPUT_MOVE_DOWN, "move_down", "Move Down", Direction::Down),
    direction(INPUT_MOVE_LEFT, "move_left", "Move Left", Direction::Left),
    direction(
        INPUT_MOVE_RIGHT,
        "move_right",
        "Move Right",
        Direction::Right,
    ),
    InputControl {
        id: InputId(INPUT_FIRE_UP as u16),
        stable_name: "fire_up",
        label: "Fire Up",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_FIRE_UP,
    },
    InputControl {
        id: InputId(INPUT_FIRE_DOWN as u16),
        stable_name: "fire_down",
        label: "Fire Down",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_FIRE_DOWN,
    },
    InputControl {
        id: InputId(INPUT_FIRE_LEFT as u16),
        stable_name: "fire_left",
        label: "Fire Left",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_FIRE_LEFT,
    },
    InputControl {
        id: InputId(INPUT_FIRE_RIGHT as u16),
        stable_name: "fire_right",
        label: "Fire Right",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_FIRE_RIGHT,
    },
];

impl InputConfigurable for BlackWidowSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        BWIDOW_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let b = &mut self.board;
        match event {
            InputEvent::Button { id, pressed } => match id.0 as u8 {
                // M9 at $7800.
                INPUT_COIN2 => set_bit_active_low(&mut b.in0, 0, pressed),
                INPUT_COIN1 => set_bit_active_low(&mut b.in0, 1, pressed),
                INPUT_COIN_AUX => set_bit_active_low(&mut b.in0, 2, pressed),
                INPUT_SERVICE => set_bit_active_low(&mut b.in0, 4, pressed),
                // L9 at $8000: the move stick.
                INPUT_MOVE_RIGHT => set_bit_active_low(&mut b.l9, 0, pressed),
                INPUT_MOVE_LEFT => set_bit_active_low(&mut b.l9, 1, pressed),
                INPUT_MOVE_DOWN => set_bit_active_low(&mut b.l9, 2, pressed),
                INPUT_MOVE_UP => set_bit_active_low(&mut b.l9, 3, pressed),
                // N9 at $8800: the fire stick and the starts.
                INPUT_FIRE_RIGHT => set_bit_active_low(&mut b.n9, 0, pressed),
                INPUT_FIRE_LEFT => set_bit_active_low(&mut b.n9, 1, pressed),
                INPUT_FIRE_DOWN => set_bit_active_low(&mut b.n9, 2, pressed),
                INPUT_FIRE_UP => set_bit_active_low(&mut b.n9, 3, pressed),
                INPUT_P1_START => set_bit_active_low(&mut b.n9, 5, pressed),
                INPUT_P2_START => set_bit_active_low(&mut b.n9, 6, pressed),
                _ => {}
            },
            InputEvent::Relative { .. } => {}
            InputEvent::Absolute { .. } => {}
        }
    }

    fn release_all_inputs(&mut self) {
        phosphor_core::core::machine::release_all_controls(self);
    }
}

// ---------------------------------------------------------------------------
// Machine traits
// ---------------------------------------------------------------------------

impl Renderable for BlackWidowSystem {
    fn display_size(&self) -> (u32, u32) {
        let (w, h) = TIMING.display_size();
        raster_size_for_field(w, h)
    }

    fn vector_field_size(&self) -> Option<(u32, u32)> {
        Some(TIMING.display_size())
    }

    fn display_aspect(&self) -> Option<(u32, u32)> {
        TIMING.display_aspect()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        self.board.render(buffer);
    }

    fn vector_display_list(&self) -> Option<&[VectorLine]> {
        Some(&self.board.display_list)
    }
}

impl phosphor_core::core::machine::AudioSource for BlackWidowSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.audio_buffer.pop_front_into(buffer)
    }

    fn audio_sample_rate(&self) -> u32 {
        phosphor_core::audio::host_sample_rate()
    }
}

crate::impl_board_debug!(BlackWidowSystem, board, TIMING);
crate::impl_board_debug_trace!(BlackWidowSystem, board);

impl MachineCore for BlackWidowSystem {
    crate::machine_core_metadata!("blackwidow", TIMING, atari_avg::clock_tree);

    fn run_frame(&mut self) {
        for _ in 0..TIMING.cycles_per_frame() {
            self.board.tick(&mut self.cpu);
        }
        self.board.mix_audio(&mut self.audio_buffer);
    }

    fn reset(&mut self) {
        self.audio_buffer.clear();
        self.board.reset(&mut self.cpu);
    }
}

impl SaveState for BlackWidowSystem {
    crate::machine_save_state!();
}

impl Nvram for BlackWidowSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.earom.snapshot())
    }

    fn load_nvram(&mut self, data: &[u8]) {
        self.board.earom.load_from(data);
    }
}

impl Profilable for BlackWidowSystem {}

// ---------------------------------------------------------------------------
// DIP switches
// ---------------------------------------------------------------------------

/// TM-234's recommended settings: 1 coin 1 credit, both mechanisms x1, no
/// bonus coins.
const DSW_D4_DEFAULT: u8 = 0x00;
/// TM-234's recommended settings: start up to level 21, 3 spiders, medium,
/// a bonus spider every 20,000.
const DSW_B4_DEFAULT: u8 = 0x11;

/// D4, TM-234 table 1-2. Switch 1 is bit 7 and switch 8 bit 0; on reads 1.
const DSW_D4: DipSwitchBank = DipSwitchBank {
    name: "D4 (Price)",
    options: &[
        option(
            "Coinage",
            0x03,
            &[
                choice("1 Coin/1 Credit", 0x00),
                choice("2 Coins/1 Credit", 0x01),
                choice("Free Play", 0x02),
                choice("1 Coin/2 Credits", 0x03),
            ],
        ),
        option(
            "Right Coin Mechanism",
            0x0C,
            &[
                choice("x1", 0x00),
                choice("x4", 0x04),
                choice("x5", 0x08),
                choice("x6", 0x0C),
            ],
        ),
        option(
            "Left Coin Mechanism",
            0x10,
            &[choice("x1", 0x00), choice("x2", 0x10)],
        ),
        option(
            "Bonus Coins",
            0xE0,
            &[
                choice("None (0)", 0x00),
                choice("1 each 2", 0x20),
                choice("1 each 4", 0x40),
                choice("2 each 4", 0x60),
                choice("1 each 5", 0x80),
                choice("1 each 3", 0xA0),
                choice("None (6)", 0xC0),
                choice("None (7)", 0xE0),
            ],
        ),
    ],
};

/// B4, TM-234 table 1-4.
const DSW_B4: DipSwitchBank = DipSwitchBank {
    name: "B4 (Game)",
    options: &[
        option(
            "Maximum Start Level",
            0x03,
            &[
                choice("13", 0x00),
                choice("21", 0x01),
                choice("37", 0x02),
                choice("53", 0x03),
            ],
        ),
        option(
            "Spiders",
            0x0C,
            &[
                choice("3", 0x00),
                choice("4", 0x04),
                choice("5", 0x08),
                choice("6", 0x0C),
            ],
        ),
        option(
            "Difficulty",
            0x30,
            &[
                choice("Easy", 0x00),
                choice("Medium", 0x10),
                choice("Hard", 0x20),
                choice("Demonstration", 0x30),
            ],
        ),
        option(
            "Bonus Spider",
            0xC0,
            &[
                choice("20000", 0x00),
                choice("30000", 0x40),
                choice("40000", 0x80),
                choice("None", 0xC0),
            ],
        ),
    ],
};

const BWIDOW_DIP_BANKS: &[DipSwitchBank] = &[DSW_D4, DSW_B4];

crate::impl_dip_switches!(
    BlackWidowSystem,
    BWIDOW_DIP_BANKS,
    board.dsw_d4,
    board.dsw_b4
);

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

crate::register_machine!(
    BlackWidowSystem,
    "blackwidow",
    &[crate::registry::Revision {
        names: &["bwidow"],
        nvram_group: None
    }],
    BWIDOW_CONTROLS,
    configs = ALL_CONFIGS
);

#[cfg(test)]
mod tests {
    use super::*;

    fn press(sys: &mut BlackWidowSystem, id: u8, pressed: bool) {
        sys.handle_input(InputEvent::Button {
            id: InputId(id as u16),
            pressed,
        });
    }

    /// The move stick lands on $8000 and the fire stick on $8800, each as
    /// up, down, left, right on bits 3-0, active low, under the open option
    /// jumpers and cabinet line.
    #[test]
    fn the_two_sticks_land_on_their_buffers() {
        let mut sys = BlackWidowSystem::new();
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x8000), 0xFF);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x8800), 0xFF);
        press(&mut sys, INPUT_MOVE_UP, true);
        press(&mut sys, INPUT_MOVE_RIGHT, true);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x8000), 0xF6);
        press(&mut sys, INPUT_FIRE_LEFT, true);
        press(&mut sys, INPUT_P2_START, true);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x8800), 0xBD);
        press(&mut sys, INPUT_MOVE_UP, false);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x8000), 0xFE);
    }

    /// Coins and self-test land on M9's bits: right on 0, left on 1, aux on
    /// 2, self-test on 4.
    #[test]
    fn coin_door_lands_on_m9() {
        let mut sys = BlackWidowSystem::new();
        press(&mut sys, INPUT_COIN1, true);
        assert_eq!(sys.board.in0 & 0x1F, 0x1D);
        press(&mut sys, INPUT_COIN2, true);
        press(&mut sys, INPUT_SERVICE, true);
        assert_eq!(sys.board.in0 & 0x1F, 0x0C);
    }

    /// D4 is coinage at $6000's ALLPOT and B4 the game options at $6800's.
    #[test]
    fn switch_banks_reach_their_pokeys() {
        let mut sys = BlackWidowSystem::new();
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x6008), DSW_D4_DEFAULT);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x6808), DSW_B4_DEFAULT);
    }

    /// Save/load carries RAM, the latch, the counters, inputs and the EAROM.
    #[test]
    fn save_load_round_trip() {
        let mut sys = BlackWidowSystem::new();
        sys.bus_write(BusMaster::Cpu(0), 0x0123, 0xAA);
        sys.bus_write(BusMaster::Cpu(0), 0x2200, 0xBB);
        sys.bus_write(BusMaster::Cpu(0), 0x8800, 0x30);
        sys.board.irq_count = 7;
        sys.board.watchdog_count = 9;
        sys.board.l9 = 0x15;
        sys.board.clock = 75_000;
        sys.board.earom.load_from(&{
            let mut d = [0u8; 64];
            d[5] = 0x42;
            d
        });
        let data = sys.save_state().expect("save_state should return Some");

        let mut sys2 = BlackWidowSystem::new();
        sys2.load_state(&data).unwrap();
        assert_eq!(sys2.bus_read(BusMaster::Cpu(0), 0x0123), 0xAA);
        assert_eq!(sys2.bus_read(BusMaster::Cpu(0), 0x2200), 0xBB);
        assert_eq!(sys2.board.latch, 0x30);
        assert_eq!(sys2.board.irq_count, 7);
        assert_eq!(sys2.board.watchdog_count, 9);
        assert_eq!(sys2.board.l9, 0x15);
        assert_eq!(sys2.board.clock, 75_000);
        assert_eq!(sys2.board.earom.read(5), 0x42);
    }
}

#[cfg(test)]
crate::dip_test_suite!(BlackWidowSystem, &[DSW_D4_DEFAULT, DSW_B4_DEFAULT]);
