//! Atari Lunar Battle (1982), the earlier prototype, registered as `lunarba1`.
//!
//! The board is [`crate::atari_color_vector_conversions`] with
//! [`Decode::LunarBa1`]: Space Duel's I/O, RAM and vector ROM with six
//! program pages from $4000 instead of five. The later prototype (`lunarbat`)
//! runs on the Gravitar board and is carried as a Gravitar revision; this one
//! runs on Space Duel's memory map and is its own machine because nothing
//! else about it matches Space Duel: one player instead of two, no readable
//! switch banks, and a 500 x 440 field instead of 540 x 400.
//!
//! - **Controls**, from the reference driver's input ports: the driver's
//!   player-1 bits sit in the same positions as Space Duel's, so they drive
//!   the same N9/L9 mux pins: rotate left, rotate right, fire, thrust and
//!   shields with two starts.
//! - **Switches**: none. The reference driver marks both switch banks unread,
//!   so no banks are exposed; the board's switch bytes stay at their defaults
//!   and the POKEYs' ALLPOT reads them there.
//! - **Audio**: Space Duel's SP-181 part values, assumed. No schematic is
//!   available for the prototype board.
//! - **Display**: a 500 x 440 field, from the reference driver's visible area.
//!   The monitor's pots set this on hardware and no drawing gives it.
//!
//! Everything below $4000 is SP-181; only the sixth program page is modeled
//! from the reference driver alone (`phosphor-emulator-quwu.6`). The driver
//! marks this game working, which is what makes that modeling trustworthy;
//! the Black Widow prototype (`bwidowp`), which it marks not working, stays
//! unmodeled.

use crate::atari_avg;
use crate::atari_color_vector_conversions::{
    AtariColorVectorConversionsBoard, AudioParts, ConversionRomConfig, Decode, timing,
};
use crate::rom_loader::{RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_low;
use phosphor_core::audio::SampleRing;
use phosphor_core::core::machine::{
    ActionRole, AudioSource, InputConfigurable, InputControl, InputEvent, InputId, InputKind,
    MachineCore, Nvram, Profilable, Renderable, SaveState, TimingConfig,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::device::dvg::{VectorLine, raster_size_for_field};
use phosphor_macros::{BusDebug, Saveable};

// ---------------------------------------------------------------------------
// ROM sets
// ---------------------------------------------------------------------------

/// Vector ROM: 2K at AVG address $800-$FFF (CPU $2800-$2FFF), 4K at
/// $1000-$1FFF (CPU $3000-$3FFF). Same layout as Space Duel's.
static VECTOR_ROM: RomRegion = RomRegion {
    size: 0x1800,
    entries: &[
        rom!("vrom1.bin", 0x0800, 0x0000, 0xc60634d9),
        rom!("vrom2.bin", 0x1000, 0x0800, 0x53d9a8a2),
    ],
};

/// Program ROM: six pages from $4000, the sixth at $9000 reloaded through
/// $A000-$FFFF for the reset vectors.
static PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        rom!("rom0.bin", 0x1000, 0x0000, 0xcc4691c6),
        rom!("rom1.bin", 0x1000, 0x1000, 0x4df71d07),
        rom!("rom2.bin", 0x1000, 0x2000, 0xc6ff04cb),
        rom!("rom3.bin", 0x1000, 0x3000, 0xa7dc9d1b),
        rom!("rom4.bin", 0x1000, 0x4000, 0x788bf976),
        rom!("rom5.bin", 0x1000, 0x5000, 0x16121e13),
    ],
};

/// Lunar Battle, the earlier prototype.
pub static LUNARBA1_CONFIG: ConversionRomConfig = ConversionRomConfig {
    set: "lunarba1",
    vector: &VECTOR_ROM,
    program: &PROGRAM_ROM,
};

const ALL_CONFIGS: &[&ConversionRomConfig] = &[&LUNARBA1_CONFIG];

// ---------------------------------------------------------------------------
// The machine
// ---------------------------------------------------------------------------

const TIMING: TimingConfig = timing(500, 440);

/// Space Duel's sheet 5B part values, assumed for the prototype board.
const AUDIO: AudioParts = AudioParts {
    c27_farads: 1e-9,
    r46_ohms: 10_000.0,
    c34_farads: Some(0.22e-6),
};

#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct Lunarba1System {
    #[debug_cpu("M6502")]
    #[save(id = 1)]
    cpu: M6502,
    #[debug_bus]
    #[save(id = 2)]
    board: AtariColorVectorConversionsBoard,
    #[save_skip(default)]
    audio_buffer: SampleRing<i16>,
}

impl Lunarba1System {
    pub fn new() -> Self {
        let board = AtariColorVectorConversionsBoard::new(Decode::LunarBa1, &TIMING, &AUDIO);
        Self {
            cpu: M6502::new(),
            board,
            audio_buffer: SampleRing::with_capacity(2048),
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        self.load_roms(rom_set, &LUNARBA1_CONFIG)
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

impl Default for Lunarba1System {
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
const INPUT_P2_START: u8 = 8;
const INPUT_SERVICE: u8 = 9;
const INPUT_DIAG: u8 = 10;

const LUNARBA1_CONTROLS: &[InputControl] = &[
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
        id: InputId(INPUT_P2_START as u16),
        stable_name: "p2_start",
        label: "P2 Start",
        kind: InputKind::Start,
        player: Some(1),
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
        id: InputId(INPUT_DIAG as u16),
        stable_name: "diag_step",
        label: "Diagnostic Step",
        kind: InputKind::Service,
        player: None,
        default_bindings: &[],
    },
];

impl InputConfigurable for Lunarba1System {
    fn input_controls(&self) -> &'static [InputControl] {
        LUNARBA1_CONTROLS
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
                // N9's pins, as Space Duel's player 1.
                INPUT_P1_SHIELD => set_bit_active_low(&mut b.n9, 0, pressed),
                INPUT_P1_LEFT => set_bit_active_low(&mut b.n9, 2, pressed),
                INPUT_P1_THRUST => set_bit_active_low(&mut b.n9, 4, pressed),
                INPUT_P2_START => set_bit_active_low(&mut b.n9, 6, pressed),
                // L9's pins.
                INPUT_P1_FIRE => set_bit_active_low(&mut b.l9, 0, pressed),
                INPUT_P1_RIGHT => set_bit_active_low(&mut b.l9, 2, pressed),
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

impl Renderable for Lunarba1System {
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

impl AudioSource for Lunarba1System {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.audio_buffer.pop_front_into(buffer)
    }

    fn audio_sample_rate(&self) -> u32 {
        phosphor_core::audio::host_sample_rate()
    }
}

crate::impl_board_debug!(Lunarba1System, board, TIMING);
crate::impl_board_debug_trace!(Lunarba1System, board);

impl MachineCore for Lunarba1System {
    crate::machine_core_metadata!("lunarba1", TIMING, atari_avg::clock_tree);

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

impl SaveState for Lunarba1System {
    crate::machine_save_state!();
}

impl Nvram for Lunarba1System {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.earom.snapshot())
    }

    fn load_nvram(&mut self, data: &[u8]) {
        self.board.earom.load_from(data);
    }
}

impl Profilable for Lunarba1System {}

// The program never reads the switch banks, so the trait's empty defaults
// stand and no banks are exposed.
impl phosphor_core::core::machine::DipSwitches for Lunarba1System {}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

crate::register_machine!(
    Lunarba1System,
    "lunarba1",
    &[crate::registry::Revision {
        names: &["lunarba1"],
        nvram_group: None
    }],
    LUNARBA1_CONTROLS,
    configs = ALL_CONFIGS
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atari_color_vector_conversions::ConversionRegion;
    use phosphor_core::core::machine::DipSwitches;

    fn read(sys: &mut Lunarba1System, addr: u16) -> u8 {
        sys.bus_read(BusMaster::Cpu(0), addr)
    }

    fn press(sys: &mut Lunarba1System, id: u8, pressed: bool) {
        sys.handle_input(InputEvent::Button {
            id: InputId(id as u16),
            pressed,
        });
    }

    /// The muxes spread the one player's controls over $0900-$0907: offset k
    /// reads N9's Dk on bit 7 and L9's Dk on bit 6, inverted by the W outputs.
    #[test]
    fn muxes_spread_the_single_players_controls() {
        let mut sys = Lunarba1System::new();
        for k in 0..8 {
            assert_eq!(read(&mut sys, 0x0900 + k), 0x00, "offset {k}");
        }
        press(&mut sys, INPUT_P1_FIRE, true);
        press(&mut sys, INPUT_P1_SHIELD, true);
        press(&mut sys, INPUT_P1_LEFT, true);
        press(&mut sys, INPUT_P1_THRUST, true);
        press(&mut sys, INPUT_P1_START, true);
        press(&mut sys, INPUT_P2_START, true);
        assert_eq!(
            read(&mut sys, 0x0900),
            0xC0,
            "shield on N9 D0, fire on L9 D0"
        );
        assert_eq!(read(&mut sys, 0x0902), 0x80, "rotate left on N9 D2");
        assert_eq!(
            read(&mut sys, 0x0904),
            0xC0,
            "thrust on N9 D4, start on L9 D4"
        );
        assert_eq!(read(&mut sys, 0x0906), 0x80, "P2 start on N9 D6");
    }

    /// Coins and self-test land on M9's bits, as on Space Duel's PCB.
    #[test]
    fn coin_door_lands_on_m9() {
        let mut sys = Lunarba1System::new();
        press(&mut sys, INPUT_COIN1, true);
        assert_eq!(sys.board.in0 & 0x1F, 0x1D);
        press(&mut sys, INPUT_COIN2, true);
        press(&mut sys, INPUT_SERVICE, true);
        assert_eq!(sys.board.in0 & 0x1F, 0x0C);
    }

    /// 1K of RAM with no bank select; the sixth program page sits at $9000
    /// where Space Duel reloads $8000, and the reset vectors read it through
    /// the $A000-$FFFF reload.
    #[test]
    fn ram_and_six_page_rom_mirror() {
        let mut sys = Lunarba1System::new();
        sys.bus_write(BusMaster::Cpu(0), 0x0010, 0x11);
        sys.bus_write(BusMaster::Cpu(0), 0x0C00, 0x04);
        assert_eq!(read(&mut sys, 0x0010), 0x11, "D2 is no bank select here");
        assert_eq!(read(&mut sys, 0x0410), 0x00, "nothing above 1K");
        // Offsets into the program ROM region from $4000.
        sys.board.map.region_data_mut(ConversionRegion::ProgramRom)[0x4FFC] = 0x34;
        sys.board.map.region_data_mut(ConversionRegion::ProgramRom)[0x5FFC] = 0x56;
        assert_eq!(read(&mut sys, 0x8FFC), 0x34);
        assert_eq!(
            read(&mut sys, 0x9FFC),
            0x56,
            "the sixth page, not a reload of $8000"
        );
        assert_eq!(read(&mut sys, 0xFFFC), 0x56, "the reset vectors' page");
    }

    /// The switch banks exist on the PCB but the program never reads them, so
    /// none are exposed. ALLPOT still reads the bytes sitting underneath.
    #[test]
    fn no_switch_banks_are_exposed() {
        let sys = Lunarba1System::new();
        assert!(
            sys.dip_banks().is_empty(),
            "unread switches must not appear in the menu"
        );
        crate::assert_dip_banks_valid(sys.dip_banks(), &[]);
    }

    /// Save/load carries RAM, inputs, the counters and the EAROM.
    #[test]
    fn save_load_round_trip() {
        let mut sys = Lunarba1System::new();
        sys.bus_write(BusMaster::Cpu(0), 0x0100, 0xAA);
        sys.bus_write(BusMaster::Cpu(0), 0x2200, 0xBB);
        sys.board.n9 = 0x2A;
        sys.board.clock = 75_000;
        sys.board.irq_count = 9;
        sys.board.earom.load_from(&{
            let mut d = [0u8; 64];
            d[0] = 0x42;
            d
        });
        let data = sys.save_state().expect("save_state should return Some");

        let mut sys2 = Lunarba1System::new();
        sys2.load_state(&data).unwrap();
        assert_eq!(sys2.bus_read(BusMaster::Cpu(0), 0x0100), 0xAA);
        assert_eq!(sys2.bus_read(BusMaster::Cpu(0), 0x2200), 0xBB);
        assert_eq!(sys2.board.n9, 0x2A);
        assert_eq!(sys2.board.clock, 75_000);
        assert_eq!(sys2.board.irq_count, 9);
        assert_eq!(sys2.board.earom.read(0), 0x42);
    }
}
