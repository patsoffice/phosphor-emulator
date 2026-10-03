//! Atari Space Duel (1982).
//!
//! # Schematics
//!
//! | Drawing | Source | Pages |
//! |---|---|---|
//! | `Space Duel` PCB schematic, SP-181, 2nd printing | `arcarc.xmission.com/PDF_Arcade_Atari_Kee/Space_Duel/Space_Duel_SP-181_2nd_Printing.pdf` | 4A clock and watchdog, 4B decoder and IRQ, 5B I/O and audio, and the CAT-box memory map; PDF pages 7, 8, 10 and 18 |
//!
//! Transcribed in
//! [`docs/schematics/space-duel-audio-output.md`](../../docs/schematics/space-duel-audio-output.md).
//!
//! # The board
//!
//! The first of Atari's color vector conversion class, on the board model in
//! [`crate::atari_color_vector_conversions`] with Space Duel's own decode
//! ([`Decode::SpaceDuel`]): 1K of RAM, the I/O page at $0800, the POKEYs at
//! $1000 and $1400, 6K of vector ROM and 20K of program ROM. The IRQ counter,
//! watchdog, output latch, EAROM and audio topology are the same circuits as
//! Gravitar's and Black Widow's.
//!
//! What is Space Duel's own, and in this file:
//!
//! - **Controls**, on the two LS251 muxes at $0900-$0907 (sheet 5B). N9's
//!   D0-D6 are SHIELDS 1, SHIELDS 2, ROT LEFT 1, ROT LEFT 2, THRUST 1, THRUST
//!   2 and GAME SELECT; L9's D0-D4 are FIRE 1, FIRE 2, ROT RIGHT 1, ROT RIGHT 2
//!   and START. Pin D7 of N9 is the cabinet, and L9's D5-D7 the P10/11
//!   option jumpers. M9's D5 is a real DIAG STEP switch.
//! - **Switches**: D4 (C/D3's ALLPOT) is the game options, B4 (B3's) coinage,
//!   and the P10/11 jumpers charge-by, 2-credit minimum and 1-player only.
//! - **Audio**: Gravitar's values under SP-181's refdes. R45 10k is B3's leg
//!   into the sum (Gravitar's R46), C27 0.001 uF is the gain-of-10 stage's
//!   capacitor, and C32 0.22 uF sits across C/D3's R50 (Gravitar's C34).
//! - **Display**: a 540 x 400 field. The monitor's pots set this on
//!   hardware and no drawing gives it.
//!
//! What is not modeled: the coin counters, the coin lockout and the
//! start/select lamps on the latch, and the cocktail cabinet (the jumper
//! stays open, which the program reads as upright).

use crate::atari_avg;
use crate::atari_color_vector_conversions::{
    AtariColorVectorConversionsBoard, AudioParts, ConversionRomConfig, Decode, timing,
};
use crate::rom_loader::{RomLoadError, RomRegion, RomSet};
use crate::{choice, option, set_bit_active_low};
use phosphor_core::audio::SampleRing;
use phosphor_core::core::machine::{
    ActionRole, AudioSource, DefaultBinding, DipSwitchBank, InputConfigurable, InputControl,
    InputEvent, InputId, InputKind, KeyId, MachineCore, Nvram, PadButton, PadControl, Profilable,
    Renderable, SaveState, TimingConfig,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::device::dvg::{VectorLine, raster_size_for_field};
use phosphor_macros::{BusDebug, Saveable};

// ---------------------------------------------------------------------------
// ROM sets
// ---------------------------------------------------------------------------

/// Vector ROM: 2K at AVG address $800-$FFF (CPU $2800-$2FFF), 4K at
/// $1000-$1FFF (CPU $3000-$3FFF).
static VECTOR_ROM: RomRegion = RomRegion {
    size: 0x1800,
    entries: &[
        rom!("136006-106.r7", 0x0800, 0x0000, 0x691122fe),
        rom!("136006-107.np7", 0x1000, 0x0800, 0xd8dd0461),
    ],
};

/// Program ROM, version 2.
static PROGRAM_V2: RomRegion = RomRegion {
    size: 0x5000,
    entries: &[
        rom!("136006-201.r1", 0x1000, 0x0000, 0xf4037b6e),
        rom!("136006-102.np1", 0x1000, 0x1000, 0x4c451e8a),
        rom!("136006-103.m1", 0x1000, 0x2000, 0xee72da63),
        rom!("136006-104.kl1", 0x1000, 0x3000, 0xe41b38a3),
        rom!("136006-105.j1", 0x1000, 0x4000, 0x5652710f),
    ],
};

/// Program ROM, version 1. Only the $4000 page differs from version 2.
static PROGRAM_V1: RomRegion = RomRegion {
    size: 0x5000,
    entries: &[
        rom!("136006-101.r1", 0x1000, 0x0000, 0xcd239e6c),
        rom!("136006-102.np1", 0x1000, 0x1000, 0x4c451e8a),
        rom!("136006-103.m1", 0x1000, 0x2000, 0xee72da63),
        rom!("136006-104.kl1", 0x1000, 0x3000, 0xe41b38a3),
        rom!("136006-105.j1", 0x1000, 0x4000, 0x5652710f),
    ],
};

/// Space Duel, version 2.
pub static SPACEDUEL_CONFIG: ConversionRomConfig = ConversionRomConfig {
    vector: &VECTOR_ROM,
    program: &PROGRAM_V2,
};

/// Space Duel, version 1. The vector ROM is common to both.
pub static SPACEDUEL1_CONFIG: ConversionRomConfig = ConversionRomConfig {
    vector: &VECTOR_ROM,
    program: &PROGRAM_V1,
};

/// Newest revision first: a set matches the first config whose files it has.
const ALL_CONFIGS: &[&ConversionRomConfig] = &[&SPACEDUEL_CONFIG, &SPACEDUEL1_CONFIG];

// ---------------------------------------------------------------------------
// The machine
// ---------------------------------------------------------------------------

const TIMING: TimingConfig = timing(540, 400);

/// Sheet 5B's part values, named for the board model's (Gravitar's) refdes.
const AUDIO: AudioParts = AudioParts {
    c27_farads: 1e-9,
    r46_ohms: 10_000.0,
    c34_farads: Some(0.22e-6),
};

/// Atari Space Duel, both ROM revisions on the one board.
#[derive(BusDebug, Saveable)]
#[save_version(2)]
#[save_tlv]
pub struct SpaceduelSystem {
    #[debug_cpu("M6502")]
    #[save(id = 1)]
    cpu: M6502,
    #[debug_bus]
    #[save(id = 2)]
    board: AtariColorVectorConversionsBoard,
    #[save_skip(default)]
    audio_buffer: SampleRing<i16>,
}

impl SpaceduelSystem {
    pub fn new() -> Self {
        let mut board = AtariColorVectorConversionsBoard::new(Decode::SpaceDuel, &TIMING, &AUDIO);
        board.dsw_d4 = DSW_D4_DEFAULT;
        board.dsw_b4 = DSW_B4_DEFAULT;
        board.options = OPTIONS_DEFAULT;
        Self {
            cpu: M6502::new(),
            board,
            audio_buffer: SampleRing::with_capacity(2048),
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        self.load_roms(rom_set, &SPACEDUEL_CONFIG)
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

    /// Write the CPU-facing bus, side effects and all. See [`Self::bus_read`].
    pub fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.board.write(master, addr, data)
    }
}

impl Default for SpaceduelSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------

const INPUT_COIN1: u8 = 0;
const INPUT_COIN2: u8 = 1;
const INPUT_P1_LEFT: u8 = 2;
const INPUT_P1_RIGHT: u8 = 3;
const INPUT_P1_FIRE: u8 = 4;
const INPUT_P1_THRUST: u8 = 5;
const INPUT_P1_SHIELD: u8 = 6;
const INPUT_P1_START: u8 = 7;
const INPUT_SELECT: u8 = 8;
const INPUT_P2_LEFT: u8 = 9;
const INPUT_P2_RIGHT: u8 = 10;
const INPUT_P2_FIRE: u8 = 11;
const INPUT_P2_THRUST: u8 = 12;
const INPUT_P2_SHIELD: u8 = 13;
const INPUT_SERVICE: u8 = 14;
const INPUT_DIAG: u8 = 15;

const SPACEDUEL_CONTROLS: &[InputControl] = &[
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
        label: "P1 Shield",
        kind: InputKind::Action(ActionRole::Tertiary),
        player: Some(1),
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
        id: InputId(INPUT_SELECT as u16),
        stable_name: "select",
        label: "Select (2P Start)",
        kind: InputKind::Start,
        player: Some(1),
        default_bindings: crate::input_defaults::P2_START,
    },
    // Player 2 plays along, so every control needs a default. Fire rides
    // Primary (RShift, the one role the ladder differentiates per player);
    // thrust and shield stay plain Buttons with explicit keys because
    // Secondary and Tertiary share Space and LCtrl across both players.
    // The pads mirror P1's A/B/X, slot-scoped to player 2.
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
        label: "P2 Shield",
        kind: InputKind::Button,
        player: Some(2),
        default_bindings: &[
            DefaultBinding::Key(KeyId::RCtrl),
            DefaultBinding::Pad(PadControl::Button(PadButton::X)),
        ],
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
        id: InputId(INPUT_DIAG as u16),
        stable_name: "diag_step",
        label: "Diagnostic Step",
        kind: InputKind::Service,
        player: None,
        default_bindings: &[],
    },
];

impl InputConfigurable for SpaceduelSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        SPACEDUEL_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let b = &mut self.board;
        match event {
            InputEvent::Button { id, pressed } => match id.0 as u8 {
                // M9 at $0800.
                INPUT_COIN2 => set_bit_active_low(&mut b.in0, 0, pressed),
                INPUT_COIN1 => set_bit_active_low(&mut b.in0, 1, pressed),
                INPUT_SERVICE => set_bit_active_low(&mut b.in0, 4, pressed),
                INPUT_DIAG => set_bit_active_low(&mut b.in0, 5, pressed),
                // N9's pins.
                INPUT_P1_SHIELD => set_bit_active_low(&mut b.n9, 0, pressed),
                INPUT_P2_SHIELD => set_bit_active_low(&mut b.n9, 1, pressed),
                INPUT_P1_LEFT => set_bit_active_low(&mut b.n9, 2, pressed),
                INPUT_P2_LEFT => set_bit_active_low(&mut b.n9, 3, pressed),
                INPUT_P1_THRUST => set_bit_active_low(&mut b.n9, 4, pressed),
                INPUT_P2_THRUST => set_bit_active_low(&mut b.n9, 5, pressed),
                INPUT_SELECT => set_bit_active_low(&mut b.n9, 6, pressed),
                // L9's pins.
                INPUT_P1_FIRE => set_bit_active_low(&mut b.l9, 0, pressed),
                INPUT_P2_FIRE => set_bit_active_low(&mut b.l9, 1, pressed),
                INPUT_P1_RIGHT => set_bit_active_low(&mut b.l9, 2, pressed),
                INPUT_P2_RIGHT => set_bit_active_low(&mut b.l9, 3, pressed),
                INPUT_P1_START => set_bit_active_low(&mut b.l9, 4, pressed),
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

impl Renderable for SpaceduelSystem {
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

impl AudioSource for SpaceduelSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.audio_buffer.pop_front_into(buffer)
    }

    fn audio_sample_rate(&self) -> u32 {
        phosphor_core::audio::host_sample_rate()
    }
}

crate::impl_board_debug!(SpaceduelSystem, board, TIMING);
crate::impl_board_debug_trace!(SpaceduelSystem, board);

impl MachineCore for SpaceduelSystem {
    crate::machine_core_metadata!("spaceduel", TIMING, atari_avg::clock_tree);

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

impl SaveState for SpaceduelSystem {
    crate::machine_save_state!();
}

impl Nvram for SpaceduelSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.earom.snapshot())
    }

    fn load_nvram(&mut self, data: &[u8]) {
        self.board.earom.load_from(data);
    }
}

impl Profilable for SpaceduelSystem {}

// ---------------------------------------------------------------------------
// DIP switches
// ---------------------------------------------------------------------------

/// 3 ships, normal difficulty, English, 10K bonus.
const DSW_D4_DEFAULT: u8 = 0x01;
/// 1 coin 1 credit, both mechanisms x1, no bonus coins.
const DSW_B4_DEFAULT: u8 = 0x00;
/// The P10/11 jumpers, all open.
const OPTIONS_DEFAULT: u8 = 0x07;

/// D4: lives, difficulty, language, bonus life.
const DSW_D4: DipSwitchBank = DipSwitchBank {
    name: "DSW0 (D4)",
    options: &[
        option(
            "Lives",
            0x03,
            &[
                choice("4", 0x00),
                choice("3", 0x01),
                choice("6", 0x02),
                choice("5", 0x03),
            ],
        ),
        option(
            "Difficulty",
            0x0C,
            &[
                choice("Normal", 0x00),
                choice("Easy", 0x04),
                choice("Hard", 0x08),
                choice("Medium", 0x0C),
            ],
        ),
        option(
            "Language",
            0x30,
            &[
                choice("English", 0x00),
                choice("German", 0x10),
                choice("French", 0x20),
                choice("Spanish", 0x30),
            ],
        ),
        option(
            "Bonus Life",
            0xC0,
            &[
                choice("10000", 0x00),
                choice("15000", 0x40),
                choice("None", 0x80),
                choice("8000", 0xC0),
            ],
        ),
    ],
};

/// B4: coinage.
const DSW_B4: DipSwitchBank = DipSwitchBank {
    name: "DSW1 (B4)",
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
                choice("1 each 2", 0x20),
                choice("1 each 4", 0x40),
                choice("1 each 5", 0x80),
                choice("1 each 3", 0xA0),
                choice("2 each 4", 0x60),
            ],
        ),
    ],
};

/// The P10/11 option jumpers on L9's D5-D7, read through the mux rather than
/// a POKEY. Only three connect. A closed jumper grounds its pin.
const DSW2: DipSwitchBank = DipSwitchBank {
    name: "DSW2 (P10/11)",
    options: &[
        option(
            "Charge By",
            0x01,
            &[choice("Game", 0x00), choice("Player", 0x01)],
        ),
        option(
            "2-Credit Minimum",
            0x02,
            &[choice("On", 0x00), choice("Off", 0x02)],
        ),
        option(
            "1-Player Game Only",
            0x04,
            &[choice("On", 0x00), choice("Off", 0x04)],
        ),
    ],
};

const SPACEDUEL_DIP_BANKS: &[DipSwitchBank] = &[DSW_D4, DSW_B4, DSW2];

crate::impl_dip_switches!(
    SpaceduelSystem,
    SPACEDUEL_DIP_BANKS,
    board.dsw_d4,
    board.dsw_b4,
    board.options
);

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

// One board, two ROM revisions: the registry tries each config in
// ALL_CONFIGS order and the first whose files the set has wins.
crate::register_machine!(
    SpaceduelSystem,
    "spaceduel",
    &["spacduel", "spacduel1"],
    SPACEDUEL_CONTROLS,
    configs = ALL_CONFIGS
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atari_color_vector_conversions::ConversionRegion;
    use phosphor_core::cpu::CpuStateTrait;

    fn read(sys: &mut SpaceduelSystem, addr: u16) -> u8 {
        sys.bus_read(BusMaster::Cpu(0), addr)
    }

    fn press(sys: &mut SpaceduelSystem, id: u8, pressed: bool) {
        sys.handle_input(InputEvent::Button {
            id: InputId(id as u16),
            pressed,
        });
    }

    /// The muxes spread both players, the option jumpers and the cabinet
    /// over eight reads at $0900-$0907: offset k reads N9's Dk on bit 7 and
    /// L9's Dk on bit 6, inverted by the W outputs.
    #[test]
    fn muxes_spread_players_options_and_cabinet() {
        let mut sys = SpaceduelSystem::new();
        // Nothing pressed, jumpers open, cabinet jumper absent: all clear.
        for k in 0..8 {
            assert_eq!(read(&mut sys, 0x0900 + k), 0x00, "offset {k}");
        }
        press(&mut sys, INPUT_P1_FIRE, true);
        press(&mut sys, INPUT_P2_SHIELD, true);
        press(&mut sys, INPUT_P1_LEFT, true);
        press(&mut sys, INPUT_P2_THRUST, true);
        assert_eq!(read(&mut sys, 0x0900), 0x40, "P1 fire on L9 D0");
        assert_eq!(read(&mut sys, 0x0901), 0x80, "P2 shield on N9 D1");
        assert_eq!(read(&mut sys, 0x0902), 0x80, "P1 rotate left on N9 D2");
        assert_eq!(read(&mut sys, 0x0905), 0x80, "P2 thrust on N9 D5");

        // Closing all three jumpers grounds L9's D5-D7.
        sys.board.options = 0x00;
        assert_eq!(read(&mut sys, 0x0905), 0xC0, "P2 thrust plus charge-by");
        assert_eq!(read(&mut sys, 0x0906), 0x40, "2-credit minimum");
        assert_eq!(read(&mut sys, 0x0907), 0x40, "1-player only, cabinet clear");
    }

    /// Thrust and shield land on the playtest-confirmed lines: thrust on
    /// offsets 4 and 5, shield on offsets 0 and 1, both on bit 7.
    #[test]
    fn thrust_and_shield_land_on_the_playtest_confirmed_bits() {
        let mut sys = SpaceduelSystem::new();
        press(&mut sys, INPUT_P1_THRUST, true);
        assert_eq!(read(&mut sys, 0x0904) & 0x80, 0x80, "P1 thrust");
        press(&mut sys, INPUT_P1_SHIELD, true);
        assert_eq!(read(&mut sys, 0x0900) & 0x80, 0x80, "P1 shield");
        press(&mut sys, INPUT_P2_THRUST, true);
        assert_eq!(read(&mut sys, 0x0905) & 0x80, 0x80, "P2 thrust");
        press(&mut sys, INPUT_P2_SHIELD, true);
        assert_eq!(read(&mut sys, 0x0901) & 0x80, 0x80, "P2 shield");
        press(&mut sys, INPUT_P1_START, true);
        press(&mut sys, INPUT_SELECT, true);
        assert_eq!(read(&mut sys, 0x0904), 0xC0, "P1 start beside thrust");
        assert_eq!(read(&mut sys, 0x0906) & 0x80, 0x80, "game select");
    }

    /// P1 shield rides the Tertiary rung (LCtrl): the third ranked action
    /// after fire (Primary) and thrust (Secondary).
    #[test]
    fn p1_shield_rides_the_tertiary_ladder() {
        let sys = SpaceduelSystem::new();
        let shield = sys
            .input_controls()
            .iter()
            .find(|c| c.stable_name == "p1_shield")
            .expect("p1_shield control exists");
        assert_eq!(shield.kind, InputKind::Action(ActionRole::Tertiary));
    }

    /// Both players can play out of the box: every P2 control resolves to
    /// at least one physical default, through the role ladder or inline.
    #[test]
    fn p2_controls_all_have_defaults() {
        let sys = SpaceduelSystem::new();
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

    /// Both POKEYs' ALLPOT registers read their switch bank, through the
    /// windows' mirrors.
    #[test]
    fn allpot_reads_the_switch_banks() {
        let mut sys = SpaceduelSystem::new();
        sys.board.dsw_d4 = 0xA5;
        sys.board.dsw_b4 = 0x5A;
        assert_eq!(read(&mut sys, 0x1008), 0xA5);
        assert_eq!(read(&mut sys, 0x1408), 0x5A);
        assert_eq!(read(&mut sys, 0x1308), 0xA5);
        assert_eq!(read(&mut sys, 0x1708), 0x5A);
    }

    /// The EAROM writes through $0F00 and reads back at $0A00, through the
    /// same K2 control bits as the later boards.
    #[test]
    fn earom_write_read() {
        let mut sys = SpaceduelSystem::new();
        sys.bus_write(BusMaster::Cpu(0), 0x0F05, 0xAB);
        sys.bus_write(BusMaster::Cpu(0), 0x0E80, 0x0F); // erase
        sys.bus_write(BusMaster::Cpu(0), 0x0E80, 0x0E);
        sys.bus_write(BusMaster::Cpu(0), 0x0E80, 0x0D); // write
        sys.bus_write(BusMaster::Cpu(0), 0x0E80, 0x0C);
        sys.bus_write(BusMaster::Cpu(0), 0x0E80, 0x09); // read
        sys.bus_write(BusMaster::Cpu(0), 0x0E80, 0x08);
        assert_eq!(read(&mut sys, 0x0A00), 0xAB);
    }

    /// Space Duel's strobes reach the shared counters: $0E00 acknowledges
    /// the IRQ and $0D00 clears the watchdog.
    #[test]
    fn strobes_reach_the_shared_counters() {
        let mut sys = SpaceduelSystem::new();
        sys.board.irq_count = 12;
        sys.board.watchdog_count = 100;
        sys.bus_write(BusMaster::Cpu(0), 0x0E00, 0);
        sys.bus_write(BusMaster::Cpu(0), 0x0D00, 0);
        assert_eq!(sys.board.irq_count, 0);
        assert_eq!(sys.board.watchdog_count, 0);
    }

    /// 1K of RAM with no bank select, and the $8000 page at $F000.
    #[test]
    fn ram_and_rom_mirror() {
        let mut sys = SpaceduelSystem::new();
        sys.bus_write(BusMaster::Cpu(0), 0x0010, 0x11);
        sys.bus_write(BusMaster::Cpu(0), 0x0C00, 0x04);
        assert_eq!(read(&mut sys, 0x0010), 0x11, "D2 is no bank select here");
        assert_eq!(read(&mut sys, 0x0410), 0x00, "nothing above 1K");
        // $8FFC is offset 0x4FFC of the program ROM region from $4000.
        sys.board.map.region_data_mut(ConversionRegion::ProgramRom)[0x4FFC] = 0x34;
        assert_eq!(read(&mut sys, 0x8FFC), 0x34);
        assert_eq!(read(&mut sys, 0xFFFC), 0x34, "the reset vectors' page");
    }

    /// Save/load carries RAM, inputs, the counters and the EAROM.
    #[test]
    fn save_load_round_trip() {
        let mut sys = SpaceduelSystem::new();
        sys.bus_write(BusMaster::Cpu(0), 0x0100, 0xAA);
        sys.bus_write(BusMaster::Cpu(0), 0x2200, 0xBB);
        sys.board.n9 = 0x2A;
        sys.board.clock = 75_000;
        sys.board.irq_count = 9;
        sys.board.earom.load_from(&{
            let mut d = [0u8; 64];
            d[0] = 0x42;
            d[63] = 0xEF;
            d
        });
        let data = sys.save_state().expect("save_state should return Some");
        let cpu_snap = sys.cpu.snapshot();

        let mut sys2 = SpaceduelSystem::new();
        sys2.load_state(&data).unwrap();
        assert_eq!(sys2.cpu.snapshot(), cpu_snap);
        assert_eq!(read(&mut sys2, 0x0100), 0xAA);
        assert_eq!(read(&mut sys2, 0x2200), 0xBB);
        assert_eq!(sys2.board.n9, 0x2A);
        assert_eq!(sys2.board.clock, 75_000);
        assert_eq!(sys2.board.irq_count, 9);
        assert_eq!(sys2.board.earom.read(0), 0x42);
        assert_eq!(sys2.board.earom.read(63), 0xEF);
    }

    /// A vector drawn up the display list lands up the screen: the
    /// end-to-end Y-sign statement, as on Tempest.
    #[test]
    fn a_vector_drawn_up_the_display_list_lands_up_the_screen() {
        let (fw, fh) = (TIMING.display_width as f32, TIMING.display_height as f32);
        let mut sys = SpaceduelSystem::new();
        sys.board.display_list = vec![VectorLine {
            x0: fw / 2.0,
            y0: fh / 2.0,
            x1: fw / 2.0,
            y1: fh * 0.9,
            intensity: 15,
            r: 255,
            g: 255,
            b: 255,
            beam_cycles: 0,
            dwell_cycles: 0,
        }];

        let (w, h) = sys.display_size();
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.render_frame(&mut buf);

        let row_light = |row: u32| -> u64 {
            (0..w)
                .map(|x| buf[((row * w + x) * 3) as usize] as u64)
                .sum()
        };
        let above: u64 = (0..h / 2).map(row_light).sum();
        let below: u64 = (h / 2..h).map(row_light).sum();
        assert!(
            above > below * 4,
            "drawn up the list, it should land up the screen: {above} above, {below} below"
        );
    }
}

#[cfg(test)]
crate::dip_test_suite!(
    SpaceduelSystem,
    &[DSW_D4_DEFAULT, DSW_B4_DEFAULT, OPTIONS_DEFAULT]
);
