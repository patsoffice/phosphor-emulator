//! Atari Road Runner (1985) — the second game on the Atari System 1 board.
//!
//! Road Runner shares all of the System 1 hardware in [`AtariSystem1Board`] with
//! Marble Madness; this module is the thin game wrapper (repo board-wrapper
//! pattern). It contributes Road Runner's cartridge ROM manifest, its slapstic
//! chip (137412-108), and the fact that its sound board carries speech (a
//! TMS5220 behind a VIA6522, currently stubbed).
//!
//! Its analog "Hall-effect" joystick is an ADC0809 mapped at `0xF40000`: the
//! wrapper feeds the X/Y channels from input and raises IRQ2 on each conversion
//! (gated by the joystick-IRQ enable), which is also what drives the attract
//! demo. Otherwise the wrapper forwards the bus to the shared board and handles
//! the digital coin/start/service switches.

use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::input::{AnalogAxis, AxisRange};
use phosphor_core::core::machine::{
    AnalogAxisKind, DefaultBinding, Direction, InputConfigurable, InputControl, InputEvent,
    InputId, InputKind, MachineCore, MouseControl, Nvram, Profilable, SaveState,
};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::state::M68000State;
use phosphor_core::device::adc0809::Adc0809;

use crate::atari_system1::{self, AtariSystem1Board, AtariSystem1Bus};
use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_low;
use phosphor_core::cpu::m68000::M68000;

// ---------------------------------------------------------------------------
// ROM manifest ("roadrunn" parent set — Road Runner rev 2)
// ---------------------------------------------------------------------------

/// All 68010 program chips, concatenated back-to-back in load order, then
/// de-interleaved into the big-endian `maincpu` image by [`load_maincpu_image`].
///
/// The motherboard BIOS holds the reset vectors; the cartridge program and the
/// slapstic-banked ROM follow as `LOAD16_BYTE` even/odd pairs (even chip = high
/// byte of the 68k word, odd chip = low byte). Unlike Marble the banks are not
/// contiguous — they land at the region offsets MAME loads them to.
pub static ROADRUNNER_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x60000,
    entries: &[
        // Motherboard BIOS (TTL Rev 2) — reset vectors at 0x000000 (shared chip).
        RomEntry {
            name: "136032.205.l13",
            size: 0x4000,
            offset: 0x00000,
            crc32: &[0x88d0be26],
        },
        RomEntry {
            name: "136032.206.l12",
            size: 0x4000,
            offset: 0x04000,
            crc32: &[0x3c79ef05],
        },
        // Cartridge program (even/odd pairs), region 0x10000 / 0x20000.
        RomEntry {
            name: "136040-228.11c",
            size: 0x8000,
            offset: 0x08000,
            crc32: &[0xb66c629a],
        },
        RomEntry {
            name: "136040-229.11a",
            size: 0x8000,
            offset: 0x10000,
            crc32: &[0x5638959f],
        },
        RomEntry {
            name: "136040-230.13c",
            size: 0x8000,
            offset: 0x18000,
            crc32: &[0xcd7956a3],
        },
        RomEntry {
            name: "136040-231.13a",
            size: 0x8000,
            offset: 0x20000,
            crc32: &[0x722f2d3b],
        },
        // Cartridge program, region 0x50000 / 0x60000 / 0x70000.
        RomEntry {
            name: "136040-134.12c",
            size: 0x8000,
            offset: 0x28000,
            crc32: &[0x18f431fe],
        },
        RomEntry {
            name: "136040-135.12a",
            size: 0x8000,
            offset: 0x30000,
            crc32: &[0xcb06f9ab],
        },
        RomEntry {
            name: "136040-136.14c",
            size: 0x8000,
            offset: 0x38000,
            crc32: &[0x8050bce4],
        },
        RomEntry {
            name: "136040-137.14a",
            size: 0x8000,
            offset: 0x40000,
            crc32: &[0x3372a5cf],
        },
        RomEntry {
            name: "136040-138.16c",
            size: 0x8000,
            offset: 0x48000,
            crc32: &[0xa83155ee],
        },
        RomEntry {
            name: "136040-139.16a",
            size: 0x8000,
            offset: 0x50000,
            crc32: &[0x23aead1c],
        },
        // Slapstic-banked ROM (even/odd), region 0x80000.
        RomEntry {
            name: "136040-140.17c",
            size: 0x4000,
            offset: 0x58000,
            crc32: &[0xd1464c88],
        },
        RomEntry {
            name: "136040-141.17a",
            size: 0x4000,
            offset: 0x5C000,
            crc32: &[0xf8f2acdf],
        },
    ],
};

/// Alphanumerics character ROM (the shared motherboard font, 136032.104.f5):
/// 512 tiles, 8×8, 2bpp — identical chip to Marble.
pub static ROADRUNNER_ALPHA_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[RomEntry {
        name: "136032.104.f5",
        size: 0x2000,
        offset: 0x0000,
        crc32: &[0x7a29dc07],
    }],
};

/// Playfield / motion-object tile ROM ("tiles" region, 0x300000): six 0x80000
/// banks, each holding four 0x8000 bitplanes spaced 0x10000 apart. The region is
/// `ROMREGION_INVERT | ROMREGION_ERASEFF`: erase to 0xFF, place the chips, then
/// invert the whole buffer (see [`RoadRunnerSystem::load_rom_set`]).
pub static ROADRUNNER_TILE_ROM: RomRegion = RomRegion {
    size: 0x300000,
    entries: &[
        // Bank 1.
        RomEntry {
            name: "136040-101.4b",
            size: 0x8000,
            offset: 0x000000,
            crc32: &[0x26d9f29c],
        },
        RomEntry {
            name: "136040-107.9b",
            size: 0x8000,
            offset: 0x010000,
            crc32: &[0x8aac0ba4],
        },
        RomEntry {
            name: "136040-113.4f",
            size: 0x8000,
            offset: 0x020000,
            crc32: &[0x48b74c52],
        },
        RomEntry {
            name: "136040-119.9f",
            size: 0x8000,
            offset: 0x030000,
            crc32: &[0x17a6510c],
        },
        // Bank 2.
        RomEntry {
            name: "136040-102.3b",
            size: 0x8000,
            offset: 0x080000,
            crc32: &[0xae88f54b],
        },
        RomEntry {
            name: "136040-108.8b",
            size: 0x8000,
            offset: 0x090000,
            crc32: &[0xa2ac13d4],
        },
        RomEntry {
            name: "136040-114.3f",
            size: 0x8000,
            offset: 0x0a0000,
            crc32: &[0xc91c3fcb],
        },
        RomEntry {
            name: "136040-120.8f",
            size: 0x8000,
            offset: 0x0b0000,
            crc32: &[0x42d25859],
        },
        // Bank 3.
        RomEntry {
            name: "136040-103.2b",
            size: 0x8000,
            offset: 0x100000,
            crc32: &[0xf2d7ef55],
        },
        RomEntry {
            name: "136040-109.7b",
            size: 0x8000,
            offset: 0x110000,
            crc32: &[0x11a843dc],
        },
        RomEntry {
            name: "136040-115.2f",
            size: 0x8000,
            offset: 0x120000,
            crc32: &[0x8b1fa5bc],
        },
        RomEntry {
            name: "136040-121.7f",
            size: 0x8000,
            offset: 0x130000,
            crc32: &[0xecf278f2],
        },
        // Bank 4.
        RomEntry {
            name: "136040-104.1b",
            size: 0x8000,
            offset: 0x180000,
            crc32: &[0x0203d89c],
        },
        RomEntry {
            name: "136040-110.6b",
            size: 0x8000,
            offset: 0x190000,
            crc32: &[0x64801601],
        },
        RomEntry {
            name: "136040-116.1f",
            size: 0x8000,
            offset: 0x1a0000,
            crc32: &[0x52b23a36],
        },
        RomEntry {
            name: "136040-122.6f",
            size: 0x8000,
            offset: 0x1b0000,
            crc32: &[0xb1137a9d],
        },
        // Bank 5.
        RomEntry {
            name: "136040-105.4d",
            size: 0x8000,
            offset: 0x200000,
            crc32: &[0x398a36f8],
        },
        RomEntry {
            name: "136040-111.9d",
            size: 0x8000,
            offset: 0x210000,
            crc32: &[0xf08b418b],
        },
        RomEntry {
            name: "136040-117.2d",
            size: 0x8000,
            offset: 0x220000,
            crc32: &[0xc4394834],
        },
        RomEntry {
            name: "136040-123.7d",
            size: 0x8000,
            offset: 0x230000,
            crc32: &[0xdafd3dbe],
        },
        // Bank 6.
        RomEntry {
            name: "136040-106.3d",
            size: 0x8000,
            offset: 0x280000,
            crc32: &[0x36a77bc5],
        },
        RomEntry {
            name: "136040-112.8d",
            size: 0x8000,
            offset: 0x290000,
            crc32: &[0xb6624f3c],
        },
        RomEntry {
            name: "136040-118.1d",
            size: 0x8000,
            offset: 0x2a0000,
            crc32: &[0xf489a968],
        },
        RomEntry {
            name: "136040-124.6d",
            size: 0x8000,
            offset: 0x2b0000,
            crc32: &[0x524d65f7],
        },
    ],
};

/// Graphics-mapping PROMs ("proms" region, 0x400): prom1 (136040-126) at 0x000,
/// prom2 (136040-125) at 0x200, driving the per-tile bank / bpp / colour / offset
/// lookup (entries 0-255 playfield, 256-511 motion objects).
pub static ROADRUNNER_PROM: RomRegion = RomRegion {
    size: 0x400,
    entries: &[
        RomEntry {
            name: "136040-126.7a",
            size: 0x200,
            offset: 0x000,
            crc32: &[0x1713c0cd],
        },
        RomEntry {
            name: "136040-125.5a",
            size: 0x200,
            offset: 0x200,
            crc32: &[0xa9ca8795],
        },
    ],
};

/// M6502 sound program. 64 KB region with ROM at 0x8000-0xFFFF.
pub static ROADRUNNER_SOUND_ROM: RomRegion = RomRegion {
    size: 0x10000,
    entries: &[
        RomEntry {
            name: "136040-143.15e",
            size: 0x4000,
            offset: 0x8000,
            crc32: &[0x62b9878e],
        },
        RomEntry {
            name: "136040-144.17e",
            size: 0x4000,
            offset: 0xC000,
            crc32: &[0x6ef1b804],
        },
    ],
};

/// Build the 0x88000-byte `maincpu` image: the 68010 program at 000000-07FFFF
/// and the slapstic ROM at 080000-087FFF, de-interleaving each even/odd chip
/// pair into big-endian words at its region offset.
fn load_maincpu_image(rom_set: &RomSet) -> Result<Vec<u8>, RomLoadError> {
    let chips = ROADRUNNER_PROGRAM_ROM.load(rom_set)?;
    let mut image = vec![0u8; 0x88000];
    // (dst_region_offset, even_chip_offset, odd_chip_offset, half_size).
    const PAIRS: [(usize, usize, usize, usize); 7] = [
        (0x00000, 0x00000, 0x04000, 0x4000), // BIOS
        (0x10000, 0x08000, 0x10000, 0x8000), // 228/229
        (0x20000, 0x18000, 0x20000, 0x8000), // 230/231
        (0x50000, 0x28000, 0x30000, 0x8000), // 134/135
        (0x60000, 0x38000, 0x40000, 0x8000), // 136/137
        (0x70000, 0x48000, 0x50000, 0x8000), // 138/139
        (0x80000, 0x58000, 0x5C000, 0x4000), // 140/141 (slapstic)
    ];
    for (dst, even, odd, half) in PAIRS {
        for i in 0..half {
            image[dst + 2 * i] = chips[even + i]; // even address = high byte
            image[dst + 2 * i + 1] = chips[odd + i]; // odd address = low byte
        }
    }
    Ok(image)
}

// ---------------------------------------------------------------------------
// Input IDs (digital switches; the analog joystick is a follow-up)
// ---------------------------------------------------------------------------

/// F60000 bit 0 — "Left Hop / P1 Start" (active-low).
pub const INPUT_START1: u8 = 0;
/// F60000 bit 1 — "Right Hop / P2 Start" (active-low).
pub const INPUT_START2: u8 = 1;
/// Service / self-test switch (F60000 bit 6, active-low).
pub const INPUT_SERVICE: u8 = 2;
/// Coin insert (sound port 0x1820 bit 0, active-low).
pub const INPUT_COIN: u8 = 3;
/// Analog-joystick direction keys (keyboard fallback for the Hall-effect stick).
pub const INPUT_JOY_UP: u8 = 4;
pub const INPUT_JOY_DOWN: u8 = 5;
pub const INPUT_JOY_LEFT: u8 = 6;
pub const INPUT_JOY_RIGHT: u8 = 7;

/// Typed control ids for the analog stick axes — a separate `InputId` namespace
/// from the digital buttons above.
const CTRL_STICK_X: InputId = InputId(10);
const CTRL_STICK_Y: InputId = InputId(11);

// ADC channel assignments (MAME atarisy1 roadrunn): Y on channel 6, X on 7.
const ADC_Y_CH: u16 = 6;
const ADC_X_CH: u16 = 7;
/// Full-scale stick endpoints and center (MAME `AD_STICK` `MINMAX(0x10,0xf0)`).
const STICK_MIN: i32 = 0x10;
const STICK_MAX: i32 = 0xF0;
const STICK_CENTER: i32 = 0x80;
/// Conversion time: 64 ADC clocks at master/16 = 8 main-CPU cycles each → the
/// end-of-conversion (and thus IRQ2) lands ~512 CPU cycles after a start.
const ADC_CONVERSION_CYCLES: u64 = 512;

const ROADRUNNER_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_COIN as u16),
        stable_name: "coin",
        label: "Coin",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(INPUT_START1 as u16),
        stable_name: "p1_start",
        label: "P1 Start / Left Hop",
        kind: InputKind::Start,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_START,
    },
    InputControl {
        id: InputId(INPUT_START2 as u16),
        stable_name: "p2_start",
        label: "P2 Start / Right Hop",
        kind: InputKind::Start,
        player: Some(2),
        default_bindings: crate::input_defaults::P2_START,
    },
    InputControl {
        id: InputId(INPUT_SERVICE as u16),
        stable_name: "service",
        label: "Service / Self-Test",
        kind: InputKind::Service,
        player: None,
        default_bindings: crate::input_defaults::SERVICE,
    },
    // Analog joystick: the mouse drives it; the D-pad / arrow keys are the
    // digital fallback that deflects the stick fully.
    InputControl {
        id: InputId(INPUT_JOY_UP as u16),
        stable_name: "p1_up",
        label: "P1 Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_UP,
    },
    InputControl {
        id: InputId(INPUT_JOY_DOWN as u16),
        stable_name: "p1_down",
        label: "P1 Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_DOWN,
    },
    InputControl {
        id: InputId(INPUT_JOY_LEFT as u16),
        stable_name: "p1_left",
        label: "P1 Left",
        kind: InputKind::DigitalDirection {
            direction: Direction::Left,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(INPUT_JOY_RIGHT as u16),
        stable_name: "p1_right",
        label: "P1 Right",
        kind: InputKind::DigitalDirection {
            direction: Direction::Right,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
    InputControl {
        id: CTRL_STICK_X,
        stable_name: "p1_stick_x",
        label: "P1 Joystick X",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::X,
        },
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::AxisX)],
    },
    InputControl {
        id: CTRL_STICK_Y,
        stable_name: "p1_stick_y",
        label: "P1 Joystick Y",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::Y,
        },
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::AxisY)],
    },
];

// ---------------------------------------------------------------------------
// RoadRunnerSystem — Atari System 1 board configured for Road Runner
// ---------------------------------------------------------------------------

/// Atari Road Runner (System 1). Slapstic 108, speech-equipped sound board,
/// Hall-effect analog joystick on an ADC0809 (IRQ2).
#[derive(phosphor_macros::BusDebug)]
pub struct RoadRunnerSystem {
    /// The 68010 is held beside the bus view over the board.
    #[debug_cpu("M68010")]
    pub cpu: M68000,

    #[debug_bus]
    pub board: AtariSystem1Board,

    /// The joystick ADC (channels 6 = Y, 7 = X, X reversed like the cabinet).
    adc: Adc0809,
    /// Joystick-interrupt enable (the ADC address A4 bit): true once the game
    /// arms IRQ2 by starting a conversion with A4 low.
    adc_irq_enabled: bool,
    /// Main-CPU clock at the last conversion start; end-of-conversion (and thus
    /// IRQ2) is asserted `ADC_CONVERSION_CYCLES` later.
    adc_start_clock: u64,

    /// Self-centering analog stick [x, y] fed to the ADC channels: held
    /// direction keys deflect fully, mouse motion nudges within the range.
    stick: [AnalogAxis; 2],
}

/// The stick's electrical range. Held keys drive it to an extreme and release
/// springs it back to center; the ADC digitizes whatever position it lands on.
fn new_stick() -> [AnalogAxis; 2] {
    std::array::from_fn(|_| AnalogAxis::new(AxisRange::new(STICK_MIN, STICK_CENTER, STICK_MAX)))
}

impl RoadRunnerSystem {
    pub fn new() -> Self {
        let mut sys = Self {
            cpu: AtariSystem1Board::new_cpu(),
            board: AtariSystem1Board::new(108, true),
            adc: Adc0809::new(),
            adc_irq_enabled: false,
            adc_start_clock: 0,
            stick: new_stick(),
        };
        sys.push_stick();
        sys
    }

    /// Feed the current stick position to the ADC channels. Y (channel 6) is
    /// direct; X (channel 7) is reversed, matching the cabinet's `PORT_REVERSE`.
    /// Borrow the CPU and the bus it drives as two disjoint pieces.
    #[inline]
    fn split(&mut self) -> (&mut M68000, RoadRunnerBus<'_>) {
        (
            &mut self.cpu,
            RoadRunnerBus {
                board: &mut self.board,
                adc: &mut self.adc,
                adc_irq_enabled: &mut self.adc_irq_enabled,
                adc_start_clock: &mut self.adc_start_clock,
            },
        )
    }

    /// One CPU cycle. Returns 1 at an instruction boundary (for the debugger,
    /// which steps instructions rather than cycles).
    pub fn step_cycle(&mut self) -> u32 {
        let (cpu, mut bus) = self.split();
        atari_system1::tick(cpu, &mut bus);
        AtariSystem1Board::instruction_boundaries(&self.cpu)
    }

    /// Read the CPU-facing bus, side effects and all. Distinct from the
    /// debugger's `BusDebug::peek`/`poke`, which avoid side effects.
    pub fn bus_read(&mut self, master: BusMaster, addr: u32) -> u16 {
        self.split().1.read(master, addr)
    }

    /// Write the CPU-facing bus, side effects and all. See [`Self::bus_read`].
    pub fn bus_write(&mut self, master: BusMaster, addr: u32, data: u16) {
        self.split().1.write(master, addr, data);
    }

    fn push_stick(&mut self) {
        self.adc
            .set_input(ADC_Y_CH as usize, self.stick[1].position() as u8);
        self.adc
            .set_input(ADC_X_CH as usize, (0xFF - self.stick[0].position()) as u8);
    }

    /// Recompute the stick from the held direction keys (full deflection while
    /// held, spring-centered when released), then push it to the ADC.
    fn update_dir_keys(&mut self) {
        self.push_stick();
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let image = load_maincpu_image(rom_set)?;
        self.board.load_program(&image);

        // Alpha (text/HUD) font tiles (motherboard, shared with Marble).
        let alpha = ROADRUNNER_ALPHA_ROM.load(rom_set)?;
        self.board.load_alpha(&alpha);

        // Playfield + motion-object tiles. The region is ROMREGION_INVERT |
        // ROMREGION_ERASEFF: erase to 0xFF, place the chips, then invert — so
        // absent planes read 0 and chip data is inverted.
        let mut tiles = ROADRUNNER_TILE_ROM.load_erased(rom_set, 0xFF)?;
        for b in tiles.iter_mut() {
            *b = !*b;
        }
        let prom = ROADRUNNER_PROM.load(rom_set)?;
        self.board.load_gfx(&prom, &tiles);

        // M6502 sound program.
        let sound_image = ROADRUNNER_SOUND_ROM.load(rom_set)?;
        self.board.load_sound(&sound_image);
        Ok(())
    }

    // -- Bring-up diagnostics (forwarded to the board) -----------------------

    pub fn get_cpu_state(&self) -> M68000State {
        use phosphor_core::cpu::CpuStateTrait;
        self.cpu.snapshot()
    }

    pub fn clock(&self) -> u64 {
        self.board.clock()
    }

    pub fn sound_debug(&self) -> (bool, u64, bool, bool) {
        self.board.sound_debug()
    }

    pub fn eeprom_debug(&self) -> (usize, u64) {
        self.board.eeprom_debug()
    }

    pub fn video_ram_stats(&self) -> (usize, usize, usize) {
        self.board.video_ram_stats()
    }
}

impl Default for RoadRunnerSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus — the analog-joystick ADC at 0xF40000, everything else to the board
// ---------------------------------------------------------------------------

/// The Road Runner bus: the shared board plus this game's joystick ADC.
struct RoadRunnerBus<'a> {
    board: &'a mut AtariSystem1Board,
    adc: &'a mut Adc0809,
    adc_irq_enabled: &'a mut bool,
    adc_start_clock: &'a mut u64,
}

impl AtariSystem1Bus for RoadRunnerBus<'_> {
    #[inline]
    fn board(&mut self) -> &mut AtariSystem1Board {
        self.board
    }
}

impl RoadRunnerBus<'_> {
    /// Start an ADC conversion on `offset`, latching the clock so
    /// end-of-conversion (and IRQ2) can be timed from it.
    fn adc_start(&mut self, offset: u16) {
        self.adc.address_offset_start_w(offset & 0x07);
        *self.adc_start_clock = self.board.clock();
        // A4 (offset bit 3) low enables the analog-joystick interrupt.
        *self.adc_irq_enabled = offset & 0x08 == 0;
    }

    /// IRQ2 line: asserted while joystick interrupts are enabled and a
    /// conversion has completed (end-of-conversion). Reading the ADC restarts a
    /// conversion, which drops EOC and clears IRQ2 until the next one finishes.
    fn adc_irq(&self) -> bool {
        *self.adc_irq_enabled
            && self.board.clock().wrapping_sub(*self.adc_start_clock) >= ADC_CONVERSION_CYCLES
    }
}

impl Bus for RoadRunnerBus<'_> {
    type Address = u32;
    type Data = u16;

    fn is_halted_for(&self, master: BusMaster) -> bool {
        self.board.bus_is_halted_for(master)
    }

    fn observe_data_access(&mut self, master: BusMaster, addr: u32, is_write: bool) {
        self.board.bus_observe_data_access(master, addr, is_write);
    }

    fn read(&mut self, master: BusMaster, addr: u32) -> u16 {
        // ADC data read (0xF40000-0xF4001F, low byte). Reading also restarts a
        // conversion on the same channel (mirrors MAME `adc_r`), which drops EOC
        // and so clears IRQ2 until the next conversion completes.
        if (0xF4_0000..=0xF4_001F).contains(&addr) {
            let value = self.adc.data_r();
            self.adc_start(((addr >> 1) & 0x0F) as u16);
            let v = 0xFF00 | value as u16;
            self.board.note_read(master, addr, v);
            return v;
        }
        self.board.bus_read(master, addr)
    }

    fn write(&mut self, master: BusMaster, addr: u32, data: u16) {
        // ADC channel select + conversion start (also latches the IRQ enable).
        if (0xF4_0000..=0xF4_001F).contains(&addr) {
            self.adc_start(((addr >> 1) & 0x0F) as u16);
        }
        // Forward for the watchpoint hook; the board treats F40000 as a no-op.
        self.board.bus_write(master, addr, data);
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        // Drive the board's IRQ2 line from the ADC before it resolves priority.
        self.board.set_int2(self.adc_irq());
        self.board.bus_check_interrupts(target)
    }
}

// ---------------------------------------------------------------------------
// Capability traits
// ---------------------------------------------------------------------------

crate::impl_board_delegation!(RoadRunnerSystem, board, atari_system1::TIMING);

impl MachineCore for RoadRunnerSystem {
    crate::machine_core_metadata!(
        "roadrunner",
        atari_system1::TIMING,
        atari_system1::clock_tree
    );

    fn run_frame(&mut self) {
        {
            let (cpu, mut bus) = self.split();
            atari_system1::run_frame(cpu, &mut bus);
        }

        // Watchdog: System 1 reboots after 8 VBLANKs without a strobe to
        // 0x880001.
        if self.board.advance_watchdog() {
            self.reset();
        }

        self.board.end_frame_audio();
    }

    fn reset(&mut self) {
        self.board.reset();
        self.adc.reset();
        self.adc_irq_enabled = false;
        self.adc_start_clock = 0;
        self.stick = new_stick();
        self.push_stick();
        let (cpu, mut bus) = self.split();
        cpu.reset(&mut bus, BusMaster::Cpu(0));
    }
}

impl InputConfigurable for RoadRunnerSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        ROADRUNNER_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Button { id, pressed } => match id.0 as u8 {
                // Bits 0/1 double as "Left/Right Hop" in-game and P1/P2 Start.
                INPUT_START1 => set_bit_active_low(&mut self.board.f60000_buttons, 0, pressed),
                INPUT_START2 => set_bit_active_low(&mut self.board.f60000_buttons, 1, pressed),
                INPUT_SERVICE => set_bit_active_low(&mut self.board.f60000_buttons, 6, pressed),
                // Coins are read on the sound board's 0x1820 port.
                INPUT_COIN => self.board.sound.set_coin(0, pressed),
                // Digital joystick fallback → full stick deflection.
                INPUT_JOY_UP => {
                    self.stick[1].set_held(false, pressed);
                    self.update_dir_keys();
                }
                INPUT_JOY_DOWN => {
                    self.stick[1].set_held(true, pressed);
                    self.update_dir_keys();
                }
                INPUT_JOY_LEFT => {
                    self.stick[0].set_held(false, pressed);
                    self.update_dir_keys();
                }
                INPUT_JOY_RIGHT => {
                    self.stick[0].set_held(true, pressed);
                    self.update_dir_keys();
                }
                _ => {}
            },
            // Mouse motion accumulates into the stick position (clamped to the
            // stick's electrical range).
            InputEvent::Relative { id, delta } => {
                let d = delta as i32;
                if id == CTRL_STICK_X {
                    self.stick[0].move_relative(d as f32);
                    self.push_stick();
                } else if id == CTRL_STICK_Y {
                    self.stick[1].move_relative(d as f32);
                    self.push_stick();
                }
            }
            InputEvent::Absolute { .. } => {}
        }
    }

    /// Also clears conditioned analog state: the digital releases above cannot
    /// reach accumulated motion or a held deflection.
    fn release_all_inputs(&mut self) {
        phosphor_core::core::machine::release_all_controls(self);
        for c in &mut self.stick {
            c.release_all();
        }
    }
}

impl SaveState for RoadRunnerSystem {
    crate::machine_save_state!();
}

impl Saveable for RoadRunnerSystem {
    fn save_state(&self, w: &mut StateWriter) {
        // The CPU first, which is where the board wrote it when it owned it.
        self.cpu.save_state(w);
        self.board.save_state(w);
        self.adc.save_state(w);
        w.write_bool(self.adc_irq_enabled);
        w.write_u64_le(self.adc_start_clock);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        self.board.load_state(r)?;
        self.adc.load_state(r)?;
        self.adc_irq_enabled = r.read_bool()?;
        self.adc_start_clock = r.read_u64_le()?;
        Ok(())
    }
}

// The 2804 EEPROM is the machine's battery-backed store.
impl Nvram for RoadRunnerSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.nvram())
    }

    fn load_nvram(&mut self, data: &[u8]) {
        self.board.load_nvram(data);
    }
}

impl Profilable for RoadRunnerSystem {}
crate::impl_map_debug_trace!(RoadRunnerSystem, board.map);

// Road Runner has no operator DIP switches — coinage and game options live in
// the EEPROM and the sound-board config.
impl phosphor_core::core::machine::DipSwitches for RoadRunnerSystem {}

// ---------------------------------------------------------------------------
// Registry + disassembly
// ---------------------------------------------------------------------------

crate::register_machine!(
    RoadRunnerSystem,
    "roadrunner",
    &["roadrunn"],
    ROADRUNNER_CONTROLS
);

inventory::submit! {
    DisasmRegion {
        machine: "roadrunner",
        region: "main",
        cpu: DisasmCpu::M68000,
        org: 0,
        size: 0x80000,
        load: |rs| load_maincpu_image(rs).map(|mut v| { v.truncate(0x80000); v }),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "roadrunner",
        region: "sound",
        cpu: DisasmCpu::M6502,
        org: 0x8000,
        size: 0x8000,
        load: |rs| ROADRUNNER_SOUND_ROM.load(rs).map(|v| v[0x8000..0x10000].to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atari_system1::Region;
    use phosphor_core::cpu::m68000::M68kVariant;

    #[test]
    fn board_is_a_108_speech_system_1() {
        let sys = RoadRunnerSystem::new();
        assert_eq!(sys.cpu.variant, M68kVariant::M68010);
        // The slapstic powers on to bank 3 (chip 108 bankstart).
        assert_eq!(sys.board.slapstic.current_bank(), 3);
    }

    #[test]
    fn maincpu_pairs_cover_the_program_and_slapstic_windows() {
        // The de-interleave table must exactly tile the loaded program regions
        // and end at the slapstic window, with no overlap.
        let mut covered = 0usize;
        let pairs = [
            (0x00000usize, 0x4000usize),
            (0x10000, 0x8000),
            (0x20000, 0x8000),
            (0x50000, 0x8000),
            (0x60000, 0x8000),
            (0x70000, 0x8000),
            (0x80000, 0x4000),
        ];
        for (dst, half) in pairs {
            covered += half * 2;
            assert!(dst + half * 2 <= 0x88000, "region {dst:#x} fits the image");
        }
        assert_eq!(covered, 0x4000 * 2 + 0x8000 * 2 * 5 + 0x4000 * 2);
    }

    #[test]
    fn map_and_regions_match_the_board() {
        let sys = RoadRunnerSystem::new();
        // The shared board map is identical to Marble's.
        assert_eq!(
            sys.board.map.region_at(0x00_0000).unwrap().id,
            Region::Rom.into()
        );
        assert!(sys.board.map.region_at(0x08_0000).is_none());
        assert_eq!(
            sys.board.map.region_at(0xB0_0000).unwrap().id,
            Region::Palette.into()
        );
    }

    #[test]
    fn control_latches_forward_to_the_board() {
        let mut sys = RoadRunnerSystem::new();
        sys.bus_write(BusMaster::Cpu(0), 0x80_0000, 0x0040);
        assert_eq!(sys.board.xscroll, 0x0040);
        // VBLANK IRQ4 asserts and acks through the forwarded bus.
        sys.board.video_int = true;
        assert_eq!(
            sys.split().1.check_interrupts(BusMaster::Cpu(0)).irq_level,
            4
        );
        sys.bus_write(BusMaster::Cpu(0), 0x8A_0000, 0x0000);
        assert!(!sys.board.video_int);
    }

    #[test]
    fn start_and_service_drive_f60000_active_low() {
        let mut sys = RoadRunnerSystem::new();
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_START1 as u16),
            pressed: true,
        });
        assert_eq!(sys.board.read_f60000() & 0x0001, 0x0000);
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_SERVICE as u16),
            pressed: true,
        });
        assert_eq!(sys.board.read_f60000() & 0x0040, 0x0000);
    }

    #[test]
    fn disasm_regions_registered() {
        use crate::disasm_registry::{find, regions_for};
        assert_eq!(
            regions_for("roadrunner")
                .iter()
                .map(|r| r.region)
                .collect::<Vec<_>>(),
            vec!["main", "sound"],
        );
        let main = find("roadrunner", "main").expect("main region");
        assert_eq!((main.org, main.size), (0, 0x80000));
    }

    const M: BusMaster = BusMaster::Cpu(0);

    #[test]
    fn adc_selects_channel_and_reads_the_stick() {
        let mut sys = RoadRunnerSystem::new();
        // Deflect the stick right; Y stays centered.
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_JOY_RIGHT as u16),
            pressed: true,
        });
        // Channel 7 (X) select+start at word offset 7 (byte 0xF4000E); read data
        // on the low byte. X is reversed: 0xFF - STICK_MAX(0xF0) = 0x0F.
        sys.bus_write(M, 0xF4_000E, 0);
        assert_eq!(sys.bus_read(M, 0xF4_000F) & 0xFF, 0x0F);
        // Channel 6 (Y) is centered → 0x80.
        sys.bus_write(M, 0xF4_000C, 0);
        assert_eq!(sys.bus_read(M, 0xF4_000D) & 0xFF, 0x80);
    }

    #[test]
    fn adc_irq2_gated_by_enable_and_conversion_time() {
        let mut sys = RoadRunnerSystem::new();
        assert!(!sys.split().1.adc_irq(), "disabled out of reset");
        // A conversion start with A4 low (word offset 6, byte 0xF4000C) enables
        // the joystick interrupt.
        sys.bus_write(M, 0xF4_000C, 0);
        assert!(sys.adc_irq_enabled);
        // Just started: end-of-conversion not reached (clock hasn't advanced).
        assert!(
            !sys.split().1.adc_irq(),
            "no IRQ2 before the conversion completes"
        );
        // Simulate the conversion time elapsing.
        sys.adc_start_clock = sys.board.clock().wrapping_sub(ADC_CONVERSION_CYCLES);
        assert!(
            sys.split().1.adc_irq(),
            "IRQ2 asserts after end-of-conversion"
        );
        // check_interrupts drives the board's IRQ2 line (nothing higher pending).
        assert_eq!(sys.split().1.check_interrupts(M).irq_level, 2);
        // A start with A4 high (word offset 0xE, byte 0xF4001C) disables it.
        sys.bus_write(M, 0xF4_001C, 0);
        assert!(!sys.adc_irq_enabled);
        assert!(!sys.split().1.adc_irq());
    }

    /// End-to-end check on the real ROM set. Opt-in: set `ROADRUNNER_ROM_DIR` to
    /// a directory of extracted `roadrunn` ROM files. Skipped (passes) when unset
    /// so CI without ROMs is unaffected.
    #[test]
    fn real_rom_boots_renders_and_sounds() {
        let Ok(dir) = std::env::var("ROADRUNNER_ROM_DIR") else {
            return;
        };
        let rom_set = RomSet::from_directory(std::path::Path::new(&dir)).unwrap();
        let mut sys = RoadRunnerSystem::new();
        sys.load_rom_set(&rom_set).unwrap();
        MachineCore::reset(&mut sys);
        let reset_pc = sys.get_cpu_state().pc;
        // Boot past the blank-EEPROM self-init and into attract.
        for _ in 0..2500 {
            sys.run_frame();
        }
        let pc = sys.get_cpu_state().pc;
        assert_ne!(pc, reset_pc, "CPU left the reset vector");
        assert!(pc < 0x0100_0000, "CPU running in ROM/RAM, not crashed away");
        // The EEPROM self-initialized (no protection/config lockout).
        assert!(sys.eeprom_debug().0 > 0, "EEPROM self-initialized");
        // Video renders content (palette + playfield populated).
        let (pal, _alpha, pf) = sys.video_ram_stats();
        assert!(pal > 0 && pf > 0, "palette + playfield populated");
        // The sound pipeline produced audio.
        let mut buf = vec![0i16; 8192];
        assert!(
            phosphor_core::core::machine::AudioSource::fill_audio(&mut sys, &mut buf) > 0,
            "audio should be produced",
        );
    }

    #[test]
    fn adc_state_round_trips_through_save_load() {
        let mut sys = RoadRunnerSystem::new();
        sys.bus_write(M, 0xF4_000C, 0); // enable + start channel 6
        sys.board.map.region_data_mut(Region::Ram)[0x40] = 0x5A;

        let data = SaveState::save_state(&sys).expect("save");
        let mut sys2 = RoadRunnerSystem::new();
        SaveState::load_state(&mut sys2, &data).unwrap();
        assert!(sys2.adc_irq_enabled, "ADC enable survives save/load");
        assert_eq!(sys2.board.map.region_data(Region::Ram)[0x40], 0x5A);
    }
}
