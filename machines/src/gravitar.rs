//! Atari Gravitar (1982), versions 3, 2 and 1 and the later Lunar Battle
//! prototype, all on the one board.
//!
//! The board is [`crate::atari_color_vector_conversions`], shared with Black
//! Widow; this file
//! is what is Gravitar's own: its ROMs, its controls, its switch tables and
//! the four audio part values that differ from Black Widow's.
//!
//! - **Controls**, from SP-206 sheet 11A: player 1 on $8000 and player 2 on
//!   $8800, each as thrust, rotate left, rotate right, fire and shields on
//!   bits 4-0, with the starts on $8800 bits 5 and 6. All active low. Player
//!   2's set is the cocktail cabinet's.
//! - **Switches**: D4 (C/D3's ALLPOT) is the game options and B4 (B3's)
//!   coinage, the other way round from Black Widow; TM-206 tables 1-3 and
//!   1-1.
//! - **Audio**: sheet 5A fits C27 at 0.001 uF, R46 at 10k and C34 at 0.22 uF.
//! - **Display**: a 420 x 400 field. The monitor's pots set this on
//!   hardware and no drawing gives it.
//!
//! Lunar Battle's later prototype (`lunarbat`) runs on this board without the
//! R7 vector ROM. Its earlier prototype (`lunarba1`) runs on Space Duel's
//! memory map and is not carried here (`phosphor-emulator-quwu.6`).

use crate::atari_avg;
use crate::atari_color_vector_conversions::{
    AtariColorVectorConversionsBoard, AudioParts, ConversionRomConfig, timing,
};
use crate::rom_loader::{RomLoadError, RomRegion, RomSet};
use crate::{choice, option, set_bit_active_low};
use phosphor_core::audio::SampleRing;
use phosphor_core::core::machine::{
    ActionRole, DefaultBinding, DipSwitchBank, InputConfigurable, InputControl, InputEvent,
    InputId, InputKind, KeyId, MachineCore, Nvram, PadButton, PadControl, Profilable, Renderable,
    SaveState, TimingConfig,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::device::dvg::{VectorLine, raster_size_for_field};
use phosphor_macros::{BusDebug, Saveable};

// ---------------------------------------------------------------------------
// ROM sets
// ---------------------------------------------------------------------------

static VECTOR_V3: RomRegion = RomRegion {
    size: 0x3800,
    entries: &[
        rom!("136010-210.l7", 0x0800, 0x0000, 0xdebcb243),
        rom!("136010-207.mn7", 0x1000, 0x0800, 0x4135629a),
        rom!("136010-208.np7", 0x1000, 0x1800, 0x358f25d9),
        rom!("136010-309.r7", 0x1000, 0x2800, 0x4ac78df4),
    ],
};

static PROGRAM_V3: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        rom!("136010-301.d1", 0x1000, 0x0000, 0xa2a55013),
        rom!("136010-302.ef1", 0x1000, 0x1000, 0xd3700b3c),
        rom!("136010-303.h1", 0x1000, 0x2000, 0x8e12e3e0),
        rom!("136010-304.j1", 0x1000, 0x3000, 0x467ad5da),
        rom!("136010-305.kl1", 0x1000, 0x4000, 0x840603af),
        rom!("136010-306.m1", 0x1000, 0x5000, 0x3f3805ad),
    ],
};

/// Version 2 shares version 3's first three vector ROMs.
static VECTOR_V2: RomRegion = RomRegion {
    size: 0x3800,
    entries: &[
        rom!("136010-210.l7", 0x0800, 0x0000, 0xdebcb243),
        rom!("136010-207.mn7", 0x1000, 0x0800, 0x4135629a),
        rom!("136010-208.np7", 0x1000, 0x1800, 0x358f25d9),
        rom!("136010-209.r7", 0x1000, 0x2800, 0x37034287),
    ],
};

static PROGRAM_V2: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        rom!("136010-201.d1", 0x1000, 0x0000, 0x167315e4),
        rom!("136010-202.ef1", 0x1000, 0x1000, 0xaaa9e62c),
        rom!("136010-203.h1", 0x1000, 0x2000, 0xae437253),
        rom!("136010-204.j1", 0x1000, 0x3000, 0x5d6bc29e),
        rom!("136010-205.kl1", 0x1000, 0x4000, 0x0db1ff34),
        rom!("136010-206.m1", 0x1000, 0x5000, 0x4521ca48),
    ],
};

static VECTOR_V1: RomRegion = RomRegion {
    size: 0x3800,
    entries: &[
        rom!("136010-110.l7", 0x0800, 0x0000, 0x1da0d845),
        rom!("136010-107.mn7", 0x1000, 0x0800, 0x650ba31e),
        rom!("136010-108.np7", 0x1000, 0x1800, 0x5119c0b2),
        rom!("136010-109.r7", 0x1000, 0x2800, 0xdefa8cbc),
    ],
};

static PROGRAM_V1: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        rom!("136010-101.d1", 0x1000, 0x0000, 0xacbc0e2c),
        rom!("136010-102.ef1", 0x1000, 0x1000, 0x88f98f8f),
        rom!("136010-103.h1", 0x1000, 0x2000, 0x68a85703),
        rom!("136010-104.j1", 0x1000, 0x3000, 0x33d19ef6),
        rom!("136010-105.kl1", 0x1000, 0x4000, 0x032b5806),
        rom!("136010-106.m1", 0x1000, 0x5000, 0x47fe97a0),
    ],
};

/// Lunar Battle has no R7 vector ROM; that socket reads as zeros.
static VECTOR_LUNARBAT: RomRegion = RomRegion {
    size: 0x3800,
    entries: &[
        rom!("136010-010.l7", 0x0800, 0x0000, 0x48fd38aa),
        rom!("136010-007.mn7", 0x1000, 0x0800, 0x9754830e),
        rom!("136010-008.np7", 0x1000, 0x1800, 0x084aa8db),
    ],
};

static PROGRAM_LUNARBAT: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        rom!("136010-001.d1", 0x1000, 0x0000, 0xcd7e1780),
        rom!("136010-002.ef1", 0x1000, 0x1000, 0xdc813a54),
        rom!("136010-003.h1", 0x1000, 0x2000, 0x8e1fecd3),
        rom!("136010-004.j1", 0x1000, 0x3000, 0xc407764f),
        rom!("136010-005.kl1", 0x1000, 0x4000, 0x4feb6f81),
        rom!("136010-006.m1", 0x1000, 0x5000, 0xf8ad139d),
    ],
};

/// Gravitar, version 3.
pub static GRAVITAR_CONFIG: ConversionRomConfig = ConversionRomConfig {
    vector: &VECTOR_V3,
    program: &PROGRAM_V3,
};

/// Gravitar, version 2.
pub static GRAVITAR2_CONFIG: ConversionRomConfig = ConversionRomConfig {
    vector: &VECTOR_V2,
    program: &PROGRAM_V2,
};

/// Gravitar, version 1.
pub static GRAVITAR1_CONFIG: ConversionRomConfig = ConversionRomConfig {
    vector: &VECTOR_V1,
    program: &PROGRAM_V1,
};

/// Lunar Battle, the later prototype.
pub static LUNARBAT_CONFIG: ConversionRomConfig = ConversionRomConfig {
    vector: &VECTOR_LUNARBAT,
    program: &PROGRAM_LUNARBAT,
};

/// Newest revision first: a set matches the first config whose files it has.
const ALL_CONFIGS: &[&ConversionRomConfig] = &[
    &GRAVITAR_CONFIG,
    &GRAVITAR2_CONFIG,
    &GRAVITAR1_CONFIG,
    &LUNARBAT_CONFIG,
];

// ---------------------------------------------------------------------------
// The machine
// ---------------------------------------------------------------------------

const TIMING: TimingConfig = timing(420, 400);

/// Sheet 5A's part values.
const AUDIO: AudioParts = AudioParts {
    c27_farads: 1e-9,
    r46_ohms: 10_000.0,
    c34_farads: Some(0.22e-6),
};

#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct GravitarSystem {
    #[debug_cpu("M6502")]
    #[save(id = 1)]
    cpu: M6502,
    #[debug_bus]
    #[save(id = 2)]
    board: AtariColorVectorConversionsBoard,
    #[save_skip(default)]
    audio_buffer: SampleRing<i16>,
}

impl GravitarSystem {
    pub fn new() -> Self {
        let mut board = AtariColorVectorConversionsBoard::new(&TIMING, &AUDIO);
        board.dsw_d4 = DSW_D4_DEFAULT;
        board.dsw_b4 = DSW_B4_DEFAULT;
        Self {
            cpu: M6502::new(),
            board,
            audio_buffer: SampleRing::with_capacity(2048),
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        self.load_roms(rom_set, &GRAVITAR_CONFIG)
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

impl Default for GravitarSystem {
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
const INPUT_P1_LEFT: u8 = 6;
const INPUT_P1_RIGHT: u8 = 7;
const INPUT_P1_FIRE: u8 = 8;
const INPUT_P1_THRUST: u8 = 9;
const INPUT_P1_SHIELD: u8 = 10;
const INPUT_P2_LEFT: u8 = 11;
const INPUT_P2_RIGHT: u8 = 12;
const INPUT_P2_FIRE: u8 = 13;
const INPUT_P2_THRUST: u8 = 14;
const INPUT_P2_SHIELD: u8 = 15;

const GRAVITAR_CONTROLS: &[InputControl] = &[
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
    InputControl {
        id: InputId(INPUT_P1_LEFT as u16),
        stable_name: "p1_left",
        label: "P1 Rotate Left",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(INPUT_P1_RIGHT as u16),
        stable_name: "p1_right",
        label: "P1 Rotate Right",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
    InputControl {
        id: InputId(INPUT_P1_FIRE as u16),
        stable_name: "p1_fire",
        label: "P1 Fire",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P1_THRUST as u16),
        stable_name: "p1_thrust",
        label: "P1 Thrust",
        kind: InputKind::Action(ActionRole::Secondary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P1_SHIELD as u16),
        stable_name: "p1_shield",
        label: "P1 Shields / Tractor Beam",
        kind: InputKind::Action(ActionRole::Tertiary),
        player: Some(1),
        default_bindings: &[],
    },
    // Player 2's controls are the cocktail cabinet's. Fire rides Primary
    // (the one role the ladder differentiates per player); thrust and shield
    // take explicit keys, as Space Duel's player 2 does.
    InputControl {
        id: InputId(INPUT_P2_LEFT as u16),
        stable_name: "p2_left",
        label: "P2 Rotate Left",
        kind: InputKind::Button,
        player: Some(2),
        default_bindings: crate::input_defaults::P2_LEFT,
    },
    InputControl {
        id: InputId(INPUT_P2_RIGHT as u16),
        stable_name: "p2_right",
        label: "P2 Rotate Right",
        kind: InputKind::Button,
        player: Some(2),
        default_bindings: crate::input_defaults::P2_RIGHT,
    },
    InputControl {
        id: InputId(INPUT_P2_FIRE as u16),
        stable_name: "p2_fire",
        label: "P2 Fire",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(2),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P2_THRUST as u16),
        stable_name: "p2_thrust",
        label: "P2 Thrust",
        kind: InputKind::Button,
        player: Some(2),
        default_bindings: &[
            DefaultBinding::Key(KeyId::Enter),
            DefaultBinding::Pad(PadControl::Button(PadButton::B)),
        ],
    },
    InputControl {
        id: InputId(INPUT_P2_SHIELD as u16),
        stable_name: "p2_shield",
        label: "P2 Shields / Tractor Beam",
        kind: InputKind::Button,
        player: Some(2),
        default_bindings: &[
            DefaultBinding::Key(KeyId::RCtrl),
            DefaultBinding::Pad(PadControl::Button(PadButton::X)),
        ],
    },
];

impl InputConfigurable for GravitarSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        GRAVITAR_CONTROLS
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
                // L9 at $8000: player 1.
                INPUT_P1_SHIELD => set_bit_active_low(&mut b.in1, 0, pressed),
                INPUT_P1_FIRE => set_bit_active_low(&mut b.in1, 1, pressed),
                INPUT_P1_RIGHT => set_bit_active_low(&mut b.in1, 2, pressed),
                INPUT_P1_LEFT => set_bit_active_low(&mut b.in1, 3, pressed),
                INPUT_P1_THRUST => set_bit_active_low(&mut b.in1, 4, pressed),
                // N9 at $8800: player 2 and the starts.
                INPUT_P2_SHIELD => set_bit_active_low(&mut b.in2, 0, pressed),
                INPUT_P2_FIRE => set_bit_active_low(&mut b.in2, 1, pressed),
                INPUT_P2_RIGHT => set_bit_active_low(&mut b.in2, 2, pressed),
                INPUT_P2_LEFT => set_bit_active_low(&mut b.in2, 3, pressed),
                INPUT_P2_THRUST => set_bit_active_low(&mut b.in2, 4, pressed),
                INPUT_P1_START => set_bit_active_low(&mut b.in2, 5, pressed),
                INPUT_P2_START => set_bit_active_low(&mut b.in2, 6, pressed),
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

impl Renderable for GravitarSystem {
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

impl phosphor_core::core::machine::AudioSource for GravitarSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.audio_buffer.pop_front_into(buffer)
    }

    fn audio_sample_rate(&self) -> u32 {
        phosphor_core::audio::host_sample_rate()
    }
}

crate::impl_board_debug!(GravitarSystem, board, TIMING);
crate::impl_board_debug_trace!(GravitarSystem, board);

impl MachineCore for GravitarSystem {
    crate::machine_core_metadata!("gravitar", TIMING, atari_avg::clock_tree);

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

impl SaveState for GravitarSystem {
    crate::machine_save_state!();
}

impl Nvram for GravitarSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.earom.snapshot())
    }

    fn load_nvram(&mut self, data: &[u8]) {
        self.board.earom.load_from(data);
    }
}

impl Profilable for GravitarSystem {}

// ---------------------------------------------------------------------------
// DIP switches
// ---------------------------------------------------------------------------

/// TM-206's recommended settings: 3 ships, easy, a bonus ship every 10,000.
const DSW_D4_DEFAULT: u8 = 0x10;
/// TM-206's recommended settings: 1 coin 1 credit, both mechanisms x1, no
/// bonus coins.
const DSW_B4_DEFAULT: u8 = 0x00;

/// D4, TM-206 table 1-3. Switch 1 is bit 7 and switch 8 bit 0; on reads 1.
/// Switches 3, 7 and 8 (bits 5, 1 and 0) are not used.
const DSW_D4: DipSwitchBank = DipSwitchBank {
    name: "D4 (Game)",
    options: &[
        option(
            "Ships",
            0x0C,
            &[
                choice("3", 0x00),
                choice("4", 0x04),
                choice("5", 0x08),
                choice("6", 0x0C),
            ],
        ),
        // Switch 4 on is easy, per the manual.
        option(
            "Difficulty",
            0x10,
            &[choice("Hard", 0x00), choice("Easy", 0x10)],
        ),
        option(
            "Bonus Ship",
            0xC0,
            &[
                choice("10000", 0x00),
                choice("20000", 0x40),
                choice("30000", 0x80),
                choice("None", 0xC0),
            ],
        ),
    ],
};

/// B4, TM-206 table 1-1. The manual lists no setting for bonus-coin code
/// 0x20, so it is not offered.
const DSW_B4: DipSwitchBank = DipSwitchBank {
    name: "B4 (Price)",
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
                choice("None", 0x00),
                choice("1 each 4", 0x40),
                choice("2 each 4", 0x60),
                choice("1 each 5", 0x80),
                choice("1 each 3", 0xA0),
                choice("None (0xC0)", 0xC0),
                choice("None (0xE0)", 0xE0),
            ],
        ),
    ],
};

const GRAVITAR_DIP_BANKS: &[DipSwitchBank] = &[DSW_D4, DSW_B4];

crate::impl_dip_switches!(
    GravitarSystem,
    GRAVITAR_DIP_BANKS,
    board.dsw_d4,
    board.dsw_b4
);

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

// One board, four ROM revisions: the registry tries each config in
// ALL_CONFIGS order and the first whose files the set has wins.
crate::register_machine!(
    GravitarSystem,
    "gravitar",
    &["gravitar", "gravitar2", "gravitar1", "lunarbat"],
    GRAVITAR_CONTROLS,
    configs = ALL_CONFIGS
);

#[cfg(test)]
mod tests {
    use super::*;

    fn press(sys: &mut GravitarSystem, id: u8, pressed: bool) {
        sys.handle_input(InputEvent::Button {
            id: InputId(id as u16),
            pressed,
        });
    }

    /// Each player's five controls land on their own buffer in sheet 11A's
    /// order: shields, fire, rotate right, rotate left, thrust on bits 0-4.
    #[test]
    fn both_players_land_on_their_buffers() {
        let mut sys = GravitarSystem::new();
        press(&mut sys, INPUT_P1_SHIELD, true);
        press(&mut sys, INPUT_P1_THRUST, true);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x8000), 0xEE);
        press(&mut sys, INPUT_P2_FIRE, true);
        press(&mut sys, INPUT_P2_LEFT, true);
        press(&mut sys, INPUT_P1_START, true);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x8800), 0xD5);
        press(&mut sys, INPUT_P1_RIGHT, true);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x8000), 0xEA);
    }

    /// D4 is the game options at $6000's ALLPOT, B4 coinage at $6800's.
    #[test]
    fn switch_banks_reach_their_pokeys() {
        let mut sys = GravitarSystem::new();
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x6008), DSW_D4_DEFAULT);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x6808), DSW_B4_DEFAULT);
    }

    /// Every player 2 control has a default binding, through the role ladder
    /// or inline, so the cocktail side is playable out of the box.
    #[test]
    fn p2_controls_all_have_defaults() {
        let sys = GravitarSystem::new();
        for name in ["p2_left", "p2_right", "p2_fire", "p2_thrust", "p2_shield"] {
            let control = sys
                .input_controls()
                .iter()
                .find(|c| c.stable_name == name)
                .unwrap_or_else(|| panic!("{name} control exists"));
            let role: &[DefaultBinding] = match control.kind {
                InputKind::Action(role) => role.default_bindings(control.player),
                _ => &[],
            };
            assert!(
                !role.is_empty() || !control.default_bindings.is_empty(),
                "{name} has no default binding"
            );
        }
    }

    /// Save/load carries RAM, the latch and the counters.
    #[test]
    fn save_load_round_trip() {
        let mut sys = GravitarSystem::new();
        sys.bus_write(BusMaster::Cpu(0), 0x0456, 0xA5);
        sys.bus_write(BusMaster::Cpu(0), 0x8800, 0xC4);
        sys.board.irq_count = 11;
        let data = sys.save_state().expect("save_state should return Some");
        let mut sys2 = GravitarSystem::new();
        sys2.load_state(&data).unwrap();
        assert_eq!(sys2.board.latch, 0xC4);
        assert_eq!(sys2.board.irq_count, 11);
        // The byte went in with BANK SEL clear; the restored latch has it
        // set, so it reads back through the other half.
        assert_eq!(sys2.bus_read(BusMaster::Cpu(0), 0x0056), 0xA5);
    }
}

#[cfg(test)]
crate::dip_test_suite!(GravitarSystem, &[DSW_D4_DEFAULT, DSW_B4_DEFAULT]);
