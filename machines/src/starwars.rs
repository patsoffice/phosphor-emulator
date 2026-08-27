//! Atari Star Wars (1983) — color-vector cockpit game.
//!
//! Hardware (MAME `src/mame/atari/starwars.cpp`):
//! - Main CPU: MC6809E @ 1.512 MHz (12.096 MHz / 8)
//! - Sound CPU: MC6809E @ 1.512 MHz
//! - Video: Atari AVG (Star Wars variant) color vector display
//! - 3D math: the [`StarWarsMath`] Matrix Processor + divider + PRNG
//! - Sound: 4× POKEY + TMS5220 speech via a MOS6532 RIOT (wired in a later step)
//!
//! This module implements the board (both CPUs, their 64 KB address spaces, ROM
//! banking, the watchdog, the periodic IRQ, the main/sound mailbox latches, the
//! AVG/matrix wiring, the POKEY/RIOT/TMS5220 sound, the ADC0809 flight yoke, and
//! the X2212 NVRAM), loads the ROM set, and registers the machine.

use phosphor_core::audio::SampleRing;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind, DebugTraceBuffer};
use phosphor_core::core::display::display_settings;
use phosphor_core::core::input::{AnalogAxis, AxisRange};
use phosphor_core::core::machine::{
    ActionRole, AnalogAxisKind, DefaultBinding, DipApplyTiming, DipChoice, DipOption,
    DipSwitchBank, DipSwitches, Direction, FrontendMachine, InputConfigurable, InputControl,
    InputEvent, InputId, InputKind, MachineCore, MouseControl, Nvram, PadAxis, PadControl,
    Profilable, SaveState,
};
use phosphor_core::core::watchpoint::DebugAccessSource;
use phosphor_core::core::{
    AccessKind, AddressSpace16, Bus, BusMaster, ClockDomainName as Clk, ClockTree, DomainId,
    TimingConfig,
};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::m6809::M6809;
use phosphor_core::device::adc0809::Adc0809;
use phosphor_core::device::avg::{Avg, AvgVariant, VectorMemory};
use phosphor_core::device::dvg::{VectorLine, raster_size_for_field};
use phosphor_core::device::pokey::Pokey;
use phosphor_core::device::riot6532::Riot6532;
use phosphor_core::device::slapstic::Slapstic;
use phosphor_core::device::starwars_math::StarWarsMath;
use phosphor_core::device::tms5220::{Tms52xxVariant, Tms5220};
use phosphor_core::device::x2212::X2212;
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion, Saveable};

use crate::atari_dvg::rasterize_vectors;
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_low;

// ---------------------------------------------------------------------------
// Input controls (digital buttons + coins; the analog flight yoke is added
// with the ADC in a follow-on step)
// ---------------------------------------------------------------------------

const INPUT_COIN1: u8 = 0;
const INPUT_COIN2: u8 = 1;
const INPUT_SERVICE: u8 = 2;
const INPUT_FIRE1: u8 = 3; // top-left trigger (BUTTON1)
const INPUT_FIRE2: u8 = 4; // top-right trigger (BUTTON2)
const INPUT_FIRE3: u8 = 5; // bottom-left thumb (BUTTON3)
const INPUT_FIRE4: u8 = 6; // bottom-right thumb (BUTTON4)
// Digital yoke deflection (keyboard); indices into `yoke_keys`.
const INPUT_YOKE_UP: u8 = 7;
const INPUT_YOKE_DOWN: u8 = 8;
const INPUT_YOKE_LEFT: u8 = 9;
const INPUT_YOKE_RIGHT: u8 = 10;
// Analog yoke axes (mouse / pad).
const CTRL_YOKE_X: InputId = InputId(20); // yaw
const CTRL_YOKE_Y: InputId = InputId(21); // pitch

// AD_STICK electrical range: full 8-bit, spring-centered.
const STICK_MIN: i32 = 0x00;
const STICK_MAX: i32 = 0xFF;
const STICK_CENTER: i32 = 0x80;

// ADC channels (MAME: channel 0 = STICKY pitch, channel 1 = STICKX yaw).
const ADC_PITCH_CH: usize = 0;
const ADC_YAW_CH: usize = 1;

const STARWARS_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_COIN1 as u16),
        stable_name: "coin",
        label: "Coin",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(INPUT_COIN2 as u16),
        stable_name: "coin2",
        label: "Coin 2",
        kind: InputKind::Coin,
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
        id: InputId(INPUT_FIRE1 as u16),
        stable_name: "fire1",
        label: "Fire (top-left)",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_FIRE2 as u16),
        stable_name: "fire2",
        label: "Fire (top-right)",
        kind: InputKind::Action(ActionRole::Secondary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_FIRE3 as u16),
        stable_name: "fire3",
        label: "Fire (bottom-left)",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_FIRE4 as u16),
        stable_name: "fire4",
        label: "Fire (bottom-right)",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: &[],
    },
    // Flight yoke — digital deflection (keyboard) and analog axes (mouse).
    InputControl {
        id: InputId(INPUT_YOKE_UP as u16),
        stable_name: "yoke_up",
        label: "Yoke Up (climb)",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_UP,
    },
    InputControl {
        id: InputId(INPUT_YOKE_DOWN as u16),
        stable_name: "yoke_down",
        label: "Yoke Down (dive)",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_DOWN,
    },
    InputControl {
        id: InputId(INPUT_YOKE_LEFT as u16),
        stable_name: "yoke_left",
        label: "Yoke Left",
        kind: InputKind::DigitalDirection {
            direction: Direction::Left,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(INPUT_YOKE_RIGHT as u16),
        stable_name: "yoke_right",
        label: "Yoke Right",
        kind: InputKind::DigitalDirection {
            direction: Direction::Right,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
    InputControl {
        id: CTRL_YOKE_X,
        stable_name: "yoke_x",
        label: "Yoke X (yaw)",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::X,
        },
        player: Some(1),
        // Right stick, not left: the shared direction defaults (P1_LEFT/RIGHT)
        // already bind LeftX as signed digital, and those assign the yoke
        // absolutely — one stick driving both would fight itself.
        default_bindings: &[
            DefaultBinding::Mouse(MouseControl::AxisX),
            DefaultBinding::Pad(PadControl::FullAxis(PadAxis::RightX)),
        ],
    },
    InputControl {
        id: CTRL_YOKE_Y,
        stable_name: "yoke_y",
        label: "Yoke Y (pitch)",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::Y,
        },
        player: Some(1),
        default_bindings: &[
            DefaultBinding::Mouse(MouseControl::AxisY),
            DefaultBinding::Pad(PadControl::FullAxis(PadAxis::RightY)),
        ],
    },
];

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------

// Master clock 12.096 MHz; CPUs run at master/8 = 1.512 MHz.
// Frame rate = 3 kHz / 12 / 6 = master / 4096 / 12 / 6 ≈ 41.02 Hz.
// Cycles per frame at 1.512 MHz ≈ 36864.
pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 1_512_000,
    cycles_per_scanline: 36_864, // whole frame (vector display has no scanlines)
    total_scanlines: 1,
    // Native beam field: the AVG deflects ±160 from center, so a 320-wide
    // visible area (xcenter = 160) frames it exactly. The real cabinet stretches
    // this ~square field horizontally onto its 4:3 tube — done at presentation
    // time via display_aspect, not baked into the vertices.
    display_width: 320,
    display_height: 330,
    display_aspect: Some((4, 3)),
};

/// The board's crystal and everything divided out of it.
///
/// One 12.096 MHz crystal: the AVG runs off it directly, both 6809Es and the
/// POKEYs through a divide-by-eight, and the TMS5220 through /2/9. The AVG
/// division is what [`AVG_CYCLES_PER_CPU_CYCLE`] is, and
/// `avg_step_matches_the_declared_crystals` checks the constant against it.
pub fn clock_tree() -> phosphor_core::core::ClockTree {
    use phosphor_core::core::{ClockDomainName as Clk, ClockTree, RootId};
    let mut t = ClockTree::new(12_096_000);
    let cpu = t.add_domain(Clk::Cpu, RootId::MAIN, 1, 8); // 1.512 MHz 6809E
    t.add_domain(Clk::SoundCpu, RootId::MAIN, 1, 8); // sound 6809E, same rate
    t.add_domain(Clk::Vector, RootId::MAIN, 1, 1); // 12.096 MHz AVG
    t.add_domain(Clk::Speech, RootId::MAIN, 1, 18); // TMS5220 at 672 kHz
    t.set_step_domain(cpu);
    // No raster derivation: a vector board has no dot clock.
    t
}

/// Periodic IRQ: 3 kHz / 12 ≈ 246.09 Hz → every 6144 CPU cycles.
const IRQ_PERIOD_CYCLES: u64 = 6144;

/// AVG master-clock cycles per CPU cycle. The 12.096 MHz crystal drives the
/// vector generator directly and the 6809E through a divide-by-8.
const AVG_CYCLES_PER_CPU_CYCLE: u32 = 8;

/// POKEY / sound-CPU clock: master / 8 = 1.512 MHz.
const SOUND_CLOCK_HZ: u32 = 1_512_000;
// The TMS5220's 672 kHz (master / 2 / 9) is no longer a constant here: it comes
// off the speech domain in `clock_tree()`, which is also what steps it.
/// Host audio output rate.
fn audio_sample_rate_hz() -> u32 {
    phosphor_core::audio::host_sample_rate() as u32
}

/// Watchdog timeout ≈ 3 kHz / 128 ≈ 23 Hz. Reset if not pet within this many
/// CPU cycles (1.512 MHz / 23 ≈ 65_700).
const WATCHDOG_CYCLES: u64 = 65_536;

// ---------------------------------------------------------------------------
// Memory maps
// ---------------------------------------------------------------------------

/// Main CPU (MC6809E) 64 KB address space.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum MainRegion {
    /// $0000–$2FFF work RAM (also the vector display list the AVG reads).
    Ram = 1,
    /// $3000–$3FFF vector ROM (upper 4 KB of the AVG's address space).
    VectorRom = 2,
    /// $4000–$47FF memory-mapped I/O (sub-decoded in `bus_read`/`bus_write`).
    Io = 3,
    /// $4800–$4FFF CPU + math scratch RAM.
    MathRamLo = 4,
    /// $5000–$5FFF shared Math RAM operated on by the Matrix Processor.
    MathRam = 5,
    /// $6000–$7FFF banked ROM — active bank 0 (LS259 bit 4 = 0).
    BankLow = 6,
    /// $8000–$FFFF fixed program ROM (Star Wars only; Empire Strikes Back
    /// replaces this window with the slapstic window + bank 2 below).
    ProgramRom = 7,
    /// Banked ROM bank 1 backing (mapped into $6000–$7FFF when bit 4 = 1).
    BankHigh = 8,
    /// ESB slapstic window backing: four 8 KB banks (32 KB) the Slapstic chip
    /// selects between at $8000–$9FFF.
    SlapsticWindow = 9,
    /// ESB bank 2 backing: two 24 KB entries (48 KB) mapped into $A000–$FFFF,
    /// switched together with bank 1 by LS259 bit 4.
    Bank2 = 10,
}

/// Sound CPU (MC6809E) 64 KB address space.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum SoundRegion {
    /// $0000–$1FFF mailbox latches + RIOT + POKEY (sub-decoded in the bus).
    Io = 1,
    /// $2000–$27FF program RAM.
    Ram = 2,
    /// $4000–$7FFF sound ROMs.
    RomLo = 3,
    /// $B000–$FFFF sound ROMs (incl. the reset vector).
    RomHi = 4,
}

fn build_main_map() -> AddressSpace16 {
    use MainRegion::*;
    let mut map = AddressSpace16::new();
    map.region(
        Ram,
        "Work/Vector RAM",
        0x0000,
        0x3000,
        AccessKind::ReadWrite,
    )
    .region(
        VectorRom,
        "Vector ROM",
        0x3000,
        0x1000,
        AccessKind::ReadOnly,
    )
    .region(Io, "I/O", 0x4000, 0x0800, AccessKind::Io)
    .region(
        MathRamLo,
        "Math RAM (lo)",
        0x4800,
        0x0800,
        AccessKind::ReadWrite,
    )
    .region(MathRam, "Math RAM", 0x5000, 0x1000, AccessKind::ReadWrite)
    .region(
        BankLow,
        "Banked ROM 0",
        0x6000,
        0x2000,
        AccessKind::ReadOnly,
    )
    .region(
        ProgramRom,
        "Program ROM",
        0x8000,
        0x8000,
        AccessKind::ReadOnly,
    )
    .backing_region(BankHigh, "Banked ROM 1", 0x2000);
    map
}

/// Empire Strikes Back main map: identical to Star Wars below $8000, but the
/// fixed program ROM is replaced by the Slapstic-banked window ($8000–$9FFF,
/// four 8 KB banks) and bank 2 ($A000–$FFFF, two 24 KB entries). Both upper
/// windows are backing-only regions paged in by [`StarWarsBoard::reset`] and the
/// banking logic — the slapstic window follows the chip, bank 2 follows LS259
/// bit 4. The reset vector at $FFFE lives in bank 2.
fn build_esb_main_map() -> AddressSpace16 {
    use MainRegion::*;
    let mut map = AddressSpace16::new();
    map.region(
        Ram,
        "Work/Vector RAM",
        0x0000,
        0x3000,
        AccessKind::ReadWrite,
    )
    .region(
        VectorRom,
        "Vector ROM",
        0x3000,
        0x1000,
        AccessKind::ReadOnly,
    )
    .region(Io, "I/O", 0x4000, 0x0800, AccessKind::Io)
    .region(
        MathRamLo,
        "Math RAM (lo)",
        0x4800,
        0x0800,
        AccessKind::ReadWrite,
    )
    .region(MathRam, "Math RAM", 0x5000, 0x1000, AccessKind::ReadWrite)
    .region(
        BankLow,
        "Banked ROM 0",
        0x6000,
        0x2000,
        AccessKind::ReadOnly,
    )
    .backing_region(BankHigh, "Banked ROM 1", 0x2000)
    .backing_region(SlapsticWindow, "Slapstic Window", 0x8000)
    .backing_region(Bank2, "Bank 2", 0xC000);
    // Page in the power-on windows: slapstic bank 3 ($6000 into the 32 KB image)
    // at $8000–$9FFF, bank 2 entry 0 at $A000–$FFFF.
    map.remap_pages(0x80, 0x20, SlapsticWindow, 3 * 0x2000);
    map.remap_pages(0xA0, 0x60, Bank2, 0);
    map
}

fn build_sound_map() -> AddressSpace16 {
    use SoundRegion::*;
    let mut map = AddressSpace16::new();
    map.region(Io, "Sound I/O", 0x0000, 0x2000, AccessKind::Io)
        .region(Ram, "Sound RAM", 0x2000, 0x0800, AccessKind::ReadWrite)
        .region(
            RomLo,
            "Sound ROM (lo)",
            0x4000,
            0x4000,
            AccessKind::ReadOnly,
        )
        .region(
            RomHi,
            "Sound ROM (hi)",
            0xB000,
            0x5000,
            AccessKind::ReadOnly,
        );
    map
}

// ---------------------------------------------------------------------------
// ROM manifest (MAME `starwars` parent set)
// ---------------------------------------------------------------------------

/// Fixed program ROM at CPU $8000–$FFFF (four contiguous 8 KB ROMs).
static SW_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        RomEntry {
            name: "136021.102.1hj",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xf725e344],
        },
        RomEntry {
            name: "136021.203.1jk",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0xf6da0a00],
        },
        RomEntry {
            name: "136021.104.1kl",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x7e406703],
        },
        RomEntry {
            name: "136021.206.1m",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0xc7e51237],
        },
    ],
};

/// Banked ROM: one 16 KB ROM whose first 8 KB is bank 0 ($6000–$7FFF, LS259
/// bit 4 = 0) and second 8 KB is bank 1 — loaded whole here and split by
/// [`StarWarsBoard::load_rom_set`].
static SW_BANK_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[RomEntry {
        name: "136021.214.1f",
        size: 0x4000,
        offset: 0x0000,
        crc32: &[0x04f1876e],
    }],
};

/// Vector ROM at CPU $3000–$3FFF (the upper 4 KB of the AVG address space).
static SW_VECTOR_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "136021-105.1l",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0x538e7d2f],
    }],
};

/// Sound ROMs at sound $4000–$7FFF (two 8 KB ROMs).
static SW_SOUND_LO: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "136021-107.1jk",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xdbf3aea2],
        },
        RomEntry {
            name: "136021-208.1h",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0xe38070a8],
        },
    ],
};

/// Sound ROMs mirrored at sound $B000–$FFFF (the two sound ROMs reloaded above a
/// 4 KB gap; the reset vector lives in the second one at $FFFE).
static SW_SOUND_HI: RomRegion = RomRegion {
    size: 0x5000,
    entries: &[
        RomEntry {
            name: "136021-107.1jk",
            size: 0x2000,
            offset: 0x1000,
            crc32: &[0xdbf3aea2],
        },
        RomEntry {
            name: "136021-208.1h",
            size: 0x2000,
            offset: 0x3000,
            crc32: &[0xe38070a8],
        },
    ],
};

/// AVG state-machine PROM (256×4, 4B). This is the sequencer that decides how
/// many states each vector instruction takes, and therefore how long the
/// generator holds VG_HALT low — the flag the game polls before touching the
/// display list.
static SW_AVG_PROM: RomRegion = RomRegion {
    size: 0x100,
    entries: &[RomEntry {
        name: "136021-109.4b",
        size: 0x100,
        offset: 0x0000,
        crc32: &[0x82fc3eb2],
    }],
};

/// Matrix Processor microcode PROMs (four 1 K×4 PROMs → the 4 KB `user2` image).
static SW_MATHBOX_PROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[
        RomEntry {
            name: "136021-110.7h",
            size: 0x400,
            offset: 0x0000,
            crc32: &[0x810e040e],
        },
        RomEntry {
            name: "136021-111.7j",
            size: 0x400,
            offset: 0x0400,
            crc32: &[0xae69881c],
        },
        RomEntry {
            name: "136021-112.7k",
            size: 0x400,
            offset: 0x0800,
            crc32: &[0xecf22628],
        },
        RomEntry {
            name: "136021-113.7l",
            size: 0x400,
            offset: 0x0C00,
            crc32: &[0x83febfde],
        },
    ],
};

// ---------------------------------------------------------------------------
// ROM manifest (MAME `esb` set — The Empire Strikes Back)
// ---------------------------------------------------------------------------
//
// ESB reuses the Star Wars board but reshapes the upper 32 KB of the main CPU
// map: the $6000 window keeps the two-bank ROM, $8000–$9FFF becomes the
// Slapstic-banked window (four 8 KB banks), and $A000–$FFFF becomes bank 2 (two
// 24 KB entries switched with bank 1). Each 16 KB ESB ROM is loaded whole here
// and sliced into banks by [`StarWarsBoard::load_esb_rom_set`].

/// ESB $6000 banked ROM (bank 1): one 16 KB ROM split into two 8 KB banks.
static ESB_BANK1_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[RomEntry {
        name: "136031-101.1f",
        size: 0x4000,
        offset: 0x0000,
        crc32: &[0xef1e3ae5],
    }],
};

/// ESB bank 2 source: three 16 KB ROMs, concatenated. Their low halves form
/// bank-2 entry 0 ($A000–$FFFF), their high halves entry 1.
static ESB_BANK2_ROM: RomRegion = RomRegion {
    size: 0xC000,
    entries: &[
        RomEntry {
            name: "136031-102.1jk",
            size: 0x4000,
            offset: 0x0000,
            crc32: &[0x62ce5c12],
        },
        RomEntry {
            name: "136031-203.1kl",
            size: 0x4000,
            offset: 0x4000,
            crc32: &[0x27b0889b],
        },
        RomEntry {
            name: "136031-104.1m",
            size: 0x4000,
            offset: 0x8000,
            crc32: &[0xfd5c725e],
        },
    ],
};

/// ESB Slapstic window ROM: two 16 KB ROMs → the 32 KB, four-bank image
/// (`105.3u` = banks 0/1, `106.2u` = banks 2/3).
static ESB_SLAPSTIC_ROM: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        RomEntry {
            name: "136031-105.3u",
            size: 0x4000,
            offset: 0x0000,
            crc32: &[0xea9e4dce],
        },
        RomEntry {
            name: "136031-106.2u",
            size: 0x4000,
            offset: 0x4000,
            crc32: &[0x76d07f59],
        },
    ],
};

/// ESB vector ROM at CPU $3000–$3FFF.
static ESB_VECTOR_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "136031-111.1l",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0xb1f9bd12],
    }],
};

/// ESB sound ROM source: two 16 KB ROMs, concatenated. Their low halves fill
/// sound $4000–$7FFF, their high halves sound $C000/$E000 (in the $B000 region).
static ESB_SOUND_ROM: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        RomEntry {
            name: "136031-113.1jk",
            size: 0x4000,
            offset: 0x0000,
            crc32: &[0x24ae3815],
        },
        RomEntry {
            name: "136031-112.1h",
            size: 0x4000,
            offset: 0x4000,
            crc32: &[0xca72d341],
        },
    ],
};

/// ESB Matrix Processor microcode PROMs (four 1 K×4 PROMs → the 4 KB image).
/// ESB ships its own microcode, distinct from the Star Wars PROMs.
static ESB_MATHBOX_PROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[
        RomEntry {
            name: "136031-110.7h",
            size: 0x400,
            offset: 0x0000,
            crc32: &[0xb8d0f69d],
        },
        RomEntry {
            name: "136031-109.7j",
            size: 0x400,
            offset: 0x0400,
            crc32: &[0x6a2a4d98],
        },
        RomEntry {
            name: "136031-108.7k",
            size: 0x400,
            offset: 0x0800,
            crc32: &[0x6a76138f],
        },
        RomEntry {
            name: "136031-107.7l",
            size: 0x400,
            offset: 0x0C00,
            crc32: &[0xafbf6e01],
        },
    ],
};

// ---------------------------------------------------------------------------
// Board
// ---------------------------------------------------------------------------

/// Star Wars board: two MC6809E CPUs, the AVG vector generator, the Matrix
/// Processor, ROM banking, watchdog, periodic IRQ, and the main↔sound mailbox.
#[derive(BusDebug, DebugTrace, Saveable)]
#[save_version(1)]
#[save_tlv]
#[save_after_load(resync_tms_clock)]
pub(crate) struct StarWarsBoard {
    #[debug_device("AVG")]
    #[save(id = 1)]
    pub(crate) avg: Avg,
    #[debug_device("SW-MATRIX")]
    #[save(id = 2)]
    pub(crate) math: StarWarsMath,
    /// The X2212 is saved whole rather than through its NVRAM bytes, so its
    /// SRAM and EEPROM halves come back as they stood rather than both from one
    /// copy of the bytes.
    #[save(id = 3)]
    pub(crate) novram: X2212,

    /// Empire Strikes Back only: the 137412-101 Slapstic banking the $8000–$9FFF
    /// window. `None` on Star Wars (where that window is fixed program ROM).
    ///
    /// An `Option` field is on the wire exactly when it is fitted.
    #[save(id = 4)]
    pub(crate) slapstic: Option<Slapstic>,

    // Sound board: four POKEYs (quad-decoded at $1800–$183F on the sound CPU).
    #[save(id = 5)]
    pub(crate) pokey: [Pokey; 4],
    /// MOS6532 RIOT ($1000–$109F): bridges the TMS5220 and raises the sound IRQ.
    #[save(id = 6)]
    pub(crate) riot: Riot6532,
    /// TMS5220 speech synthesizer, driven through the RIOT ports.
    #[save(id = 7)]
    pub(crate) tms: Tms5220,
    /// The board's clock tree, as [`clock_tree`] declares it. Only the speech
    /// domain is stepped here; the rest is the derivation it rides on.
    #[debug_device("Clocks")]
    #[save(id = 8)]
    pub(crate) clocks: ClockTree,
    /// A handle into the clock tree, which is itself saved.
    #[save_skip]
    pub(crate) tms_dom: DomainId,

    /// The address spaces persist their own writable regions and their page
    /// tables, so the ROM bank and the Slapstic window come back mapped where
    /// they were rather than being replayed from `bank` on load.
    #[debug_map(cpu = 0)]
    #[save(id = 9)]
    pub(crate) main_map: AddressSpace16,
    #[debug_map(cpu = 1)]
    #[save(id = 10)]
    pub(crate) sound_map: AddressSpace16,

    // ROM banking (LS259 bit 4).
    #[save(id = 11)]
    pub(crate) bank: u8,

    // Main↔sound mailbox (generic 8-bit latches with "pending" flags).
    #[save(id = 12)]
    pub(crate) soundlatch: u8, // main → sound
    #[save(id = 13)]
    pub(crate) soundlatch_pending: bool,
    #[save(id = 14)]
    pub(crate) mainlatch: u8, // sound → main
    #[save(id = 15)]
    pub(crate) mainlatch_pending: bool,
    /// Set by $46E0; the sound CPU is reset on the next `tick` (where a bus is
    /// available to read its reset vector). A hand-off inside one tick, which a
    /// save is never taken part way through.
    #[save_skip(default)]
    pub(crate) sound_reset_pending: bool,

    // Periodic IRQ (main CPU) and watchdog.
    #[save(id = 16)]
    pub(crate) irq_pending: bool,
    #[save(id = 17)]
    pub(crate) irq_counter: u64,
    #[save(id = 18)]
    pub(crate) watchdog_counter: u64,
    /// Latched for the frame loop to act on, and cleared by a load so a restored
    /// machine does not reset itself on its first frame.
    #[save_skip(default)]
    pub(crate) watchdog_tripped: bool,

    // Digital inputs (active-low: a released control reads 1). IN0 is the full
    // port byte; IN1 holds only its button bits (2,4,5) — bits 6/7 are computed.
    // Live input and operator configuration, which keep their previous
    // treatment of surviving a load.
    #[save_skip]
    pub(crate) in0: u8,
    #[save_skip]
    pub(crate) in1_buttons: u8,

    // Flight yoke: ADC0809 + current stick position [x=yaw, y=pitch] and the
    // held digital-deflection keys [up, down, left, right].
    #[save(id = 19)]
    pub(crate) adc: Adc0809,
    /// Saved whole rather than through `position()`, so the held-key flags come
    /// back with the position they belong to.
    #[save(id = 20)]
    pub(crate) stick: [AnalogAxis; 2],

    // Operator DIP switches (defaults; full DIP support added later).
    #[save_skip]
    pub(crate) dsw0: u8,
    #[save_skip]
    pub(crate) dsw1: u8,

    #[save(id = 21)]
    pub(crate) clock: u64,

    /// Vector display list (AVG output, refreshed when a pass over the list
    /// finishes, which for Star Wars is the HALT that ends it). Emptied by a
    /// load, so the next frame is drawn from the AVG's restored state rather
    /// than resumed part way through.
    #[save_skip(default)]
    pub(crate) display_list: Vec<VectorLine>,

    /// Samples already mixed and waiting for the frontend to drain, which the
    /// next frame refills.
    #[save_skip(default)]
    pub(crate) audio_buffer: SampleRing<i16>,
    /// One-pole DC-block history, previous input and previous output.
    ///
    /// The same filter [`DcBlocker`](phosphor_core::audio::DcBlocker) runs, but
    /// with its pole hard-coded here rather than derived from a corner
    /// frequency, so the two are not interchangeable without moving this
    /// board's output. Two fields rather than a tuple because a tuple is not a
    /// shape the save-state derive encodes.
    #[save(id = 22)]
    pub(crate) audio_dc_prev_in: f32,
    #[save(id = 23)]
    pub(crate) audio_dc_prev_out: f32,

    // Debug event ring (observer state — never saved in save states).
    #[save_skip]
    #[debug_events]
    pub(crate) debug_trace: DebugTraceBuffer,
}

/// IN0 bit 5: unused, and wired active-high rather than active-low like the
/// rest of the port, so it reads 0 at rest. Every other bit is a control that
/// reads 1 when released.
const IN0_UNUSED_BIT: u8 = 0x20;

/// `BankSwitch` event details for the four slapstic banks, indexed by the new
/// bank. Static strings because `DebugEvent::detail` holds one.
const SLAPSTIC_BANK_DETAIL: [&str; 4] = [
    "slapstic bank 0 at $8000-$9FFF",
    "slapstic bank 1 at $8000-$9FFF",
    "slapstic bank 2 at $8000-$9FFF",
    "slapstic bank 3 at $8000-$9FFF",
];

/// The yoke's electrical range [yaw, pitch]. Held keys deflect fully and
/// release springs back to center; the ADC digitizes the resulting position.
fn new_yoke() -> [AnalogAxis; 2] {
    std::array::from_fn(|_| AnalogAxis::new(AxisRange::new(STICK_MIN, STICK_CENTER, STICK_MAX)))
}

impl StarWarsBoard {
    /// Star Wars board (fixed program ROM at $8000–$FFFF, no slapstic).
    pub(crate) fn new() -> Self {
        Self::with_variant(false)
    }

    /// Empire Strikes Back board: the slapstic-banked $8000 window + bank 2, and
    /// ESB's operator-DIP defaults.
    pub(crate) fn new_esb() -> Self {
        Self::with_variant(true)
    }

    fn with_variant(esb: bool) -> Self {
        let clocks = clock_tree();
        let tms_dom = clocks.find(Clk::Speech).expect("declared speech domain");
        // The TMS's rate and the ratio it is stepped at are now one derivation,
        // read from the domain the crystal declares.
        let tms_hz = clocks.hz(tms_dom) as u32;
        Self {
            avg: Avg::with_variant(
                AvgVariant::StarWars,
                TIMING.display_width as i32,
                TIMING.display_height as i32,
            ),
            math: StarWarsMath::new(),
            novram: X2212::new(),
            slapstic: esb.then(|| Slapstic::for_chip(101)),
            pokey: std::array::from_fn(|_| {
                Pokey::with_clock(SOUND_CLOCK_HZ, audio_sample_rate_hz())
            }),
            riot: Riot6532::new(),
            tms: Tms5220::with_variant(Tms52xxVariant::Tms5220, tms_hz),
            clocks,
            tms_dom,
            main_map: if esb {
                build_esb_main_map()
            } else {
                build_main_map()
            },
            sound_map: build_sound_map(),
            bank: 0,
            soundlatch: 0,
            soundlatch_pending: false,
            mainlatch: 0,
            mainlatch_pending: false,
            sound_reset_pending: false,
            irq_pending: false,
            irq_counter: 0,
            watchdog_counter: 0,
            watchdog_tripped: false,
            in0: 0xFF,
            in1_buttons: 0xFF,
            adc: Adc0809::new(),
            stick: new_yoke(),
            // DSW0 factory defaults. Star Wars: 6 shields, Hard, 1 bonus, demo
            // sounds on, Freeze OFF (bit 7 = 1). Empire Strikes Back reshapes
            // this bank — 4 shields, Hard, Jedi-letter Increment, music on,
            // Freeze OFF ($03|$00|$30|$40|$80 = $F3).
            dsw0: if esb { 0xF3 } else { 0x98 },
            dsw1: 0x02, // coinage default: 1 coin / 1 credit
            clock: 0,
            display_list: Vec::with_capacity(2048),
            audio_buffer: SampleRing::new(),
            audio_dc_prev_in: 0.0,
            audio_dc_prev_out: 0.0,
            debug_trace: DebugTraceBuffer::new(),
        }
    }

    /// Load a Star Wars ROM set into the board's memory regions and the Matrix
    /// Processor PROMs.
    pub(crate) fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let prog = SW_PROGRAM_ROM.load(rom_set)?;
        self.main_map.load_region(MainRegion::ProgramRom, &prog);

        // The 16 KB bank ROM splits into two 8 KB banks for the $6000 window.
        let bank = SW_BANK_ROM.load(rom_set)?;
        self.main_map
            .load_region(MainRegion::BankLow, &bank[..0x2000]);
        self.main_map
            .load_region(MainRegion::BankHigh, &bank[0x2000..]);

        let vrom = SW_VECTOR_ROM.load(rom_set)?;
        self.main_map.load_region(MainRegion::VectorRom, &vrom);

        let sound_lo = SW_SOUND_LO.load(rom_set)?;
        self.sound_map.load_region(SoundRegion::RomLo, &sound_lo);
        let sound_hi = SW_SOUND_HI.load(rom_set)?;
        self.sound_map.load_region(SoundRegion::RomHi, &sound_hi);

        let proms = SW_MATHBOX_PROM.load(rom_set)?;
        self.math.load_proms(&proms);

        let avg_prom = SW_AVG_PROM.load(rom_set)?;
        self.avg.load_state_prom(&avg_prom);
        Ok(())
    }

    /// Load an Empire Strikes Back ROM set: the $6000 bank ROM, the 32 KB
    /// four-bank slapstic window, the interleaved bank 2, the vector ROM, the
    /// split sound ROMs, and ESB's Matrix Processor PROMs.
    pub(crate) fn load_esb_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        // $6000 window: 16 KB ROM split into two 8 KB banks (as on Star Wars).
        let bank1 = ESB_BANK1_ROM.load(rom_set)?;
        self.main_map
            .load_region(MainRegion::BankLow, &bank1[..0x2000]);
        self.main_map
            .load_region(MainRegion::BankHigh, &bank1[0x2000..]);

        // Slapstic window: four contiguous 8 KB banks.
        let slap = ESB_SLAPSTIC_ROM.load(rom_set)?;
        self.main_map.load_region(MainRegion::SlapsticWindow, &slap);

        // Bank 2: entry 0 = the three ROMs' low halves, entry 1 = their high
        // halves (the ROM_CONTINUE split in MAME's flat image).
        let src = ESB_BANK2_ROM.load(rom_set)?; // 102@0, 203@0x4000, 104@0x8000
        let mut bank2 = vec![0u8; 0xC000];
        for (i, &off) in [0x0000usize, 0x4000, 0x8000].iter().enumerate() {
            let lo = i * 0x2000;
            let hi = 0x6000 + i * 0x2000;
            bank2[lo..lo + 0x2000].copy_from_slice(&src[off..off + 0x2000]);
            bank2[hi..hi + 0x2000].copy_from_slice(&src[off + 0x2000..off + 0x4000]);
        }
        self.main_map.load_region(MainRegion::Bank2, &bank2);

        let vrom = ESB_VECTOR_ROM.load(rom_set)?;
        self.main_map.load_region(MainRegion::VectorRom, &vrom);

        // Sound ROMs: low halves fill $4000–$7FFF; high halves land at $C000 and
        // $E000 within the $B000 region (offsets 0x1000 and 0x3000).
        let snd = ESB_SOUND_ROM.load(rom_set)?; // 113@0, 112@0x4000
        let mut lo = vec![0u8; 0x4000];
        lo[0x0000..0x2000].copy_from_slice(&snd[0x0000..0x2000]); // 113 low
        lo[0x2000..0x4000].copy_from_slice(&snd[0x4000..0x6000]); // 112 low
        self.sound_map.load_region(SoundRegion::RomLo, &lo);
        let mut hi = vec![0u8; 0x5000];
        hi[0x1000..0x3000].copy_from_slice(&snd[0x2000..0x4000]); // 113 high @ $C000
        hi[0x3000..0x5000].copy_from_slice(&snd[0x6000..0x8000]); // 112 high @ $E000
        self.sound_map.load_region(SoundRegion::RomHi, &hi);

        let proms = ESB_MATHBOX_PROM.load(rom_set)?;
        self.math.load_proms(&proms);

        let avg_prom = SW_AVG_PROM.load(rom_set)?;
        self.avg.load_state_prom(&avg_prom);
        Ok(())
    }

    /// Feed one main-CPU bus access to the Slapstic (ESB only) and re-page the
    /// $8000–$9FFF window if the chip switched banks. The real PAL snoops every
    /// address the CPU drives, so this is called from both read and write paths.
    fn feed_slapstic(&mut self, addr: u16) {
        if let Some(sl) = self.slapstic.as_mut() {
            let before = sl.current_bank();
            let state_before = sl.state_label();
            sl.test(addr as u32);
            let after = sl.current_bank();
            let state_after = sl.state_label();
            // A bank-select address only switches from `active`; from `idle`
            // it is ignored. So a switch that should not have happened (or one
            // that should have) is only explicable from the state, never from
            // the committed bank.
            if !std::ptr::eq(state_before, state_after) && self.debug_trace.enabled() {
                self.debug_trace.record(DebugEvent {
                    cpu_index: Some(0),
                    pc: self.main_map.latched_pc(),
                    addr: Some(addr as u32),
                    region: Some("Slapstic window"),
                    detail: Some(state_after),
                    ..DebugEvent::new(
                        self.clock,
                        DebugAccessSource::Cpu(0),
                        DebugEventKind::Message,
                    )
                });
            }
            if before != after {
                self.main_map.remap_pages(
                    0x80,
                    0x20,
                    MainRegion::SlapsticWindow,
                    after as u32 * 0x2000,
                );
                // The chip is driven by a *sequence* of addresses, so the access
                // that completes a switch is rarely the one a reader would guess
                // from the disassembly. Recording the address that tipped it,
                // and the bank either side, is what makes a wrong-bank return
                // traceable at all.
                self.trace_bank_switch(addr, after);
            }
        }
    }

    /// Record a slapstic bank change as a `BankSwitch` event. `value` and
    /// `detail` both carry the *new* bank; the old one is the previous event's.
    fn trace_bank_switch(&mut self, addr: u16, after: u8) {
        if !self.debug_trace.enabled() {
            return;
        }
        self.debug_trace.record(DebugEvent {
            cpu_index: Some(0),
            pc: self.main_map.latched_pc(),
            addr: Some(addr as u32),
            value: Some(after as u32),
            width: 1,
            region: Some("Slapstic window"),
            detail: Some(SLAPSTIC_BANK_DETAIL[after as usize & 3]),
            ..DebugEvent::new(
                self.clock,
                DebugAccessSource::Cpu(0),
                DebugEventKind::BankSwitch,
            )
        });
    }

    /// Decode the four-POKEY address scramble at $1800–$183F into
    /// `(pokey_index, register)`. Matches MAME `quad_pokeyn_w`.
    fn quad_pokey_decode(offset: u16) -> (usize, u16) {
        let pokey = ((offset >> 3) & !0x04) as usize;
        let control = (offset & 0x20) >> 2;
        let reg = (offset % 8) | control;
        (pokey, reg)
    }

    // --- Main CPU bus ------------------------------------------------------

    fn main_read(&mut self, addr: u16) -> u8 {
        let value = self.main_read_value(addr);
        // The slapstic snoops the access *after* the byte has been fetched, so
        // a read that switches banks still returns the OLD bank's byte and the
        // new window takes effect from the next access. MAME's read tap has
        // this order (`m_next->read()` then the tap, emumem_het.cpp), and it is
        // load-bearing on ESB, whose bank-select stubs live inside the switched
        // window: feeding the chip first flips the window one access early, so
        // a cross-bank `JSR` returns into the wrong bank's copy of a routine.
        // The write path keeps feeding first, which is the order MAME's write
        // tap uses.
        self.feed_slapstic(addr);
        value
    }

    fn main_read_value(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x3FFF | 0x4800..=0x7FFF | 0x8000..=0xFFFF => self.main_map.read_backing(addr),
            0x4300..=0x431F => self.in0(),
            0x4320..=0x433F => self.in1(),
            0x4340..=0x435F => self.dsw0,
            0x4360..=0x437F => self.dsw1,
            0x4380..=0x439F => self.adc.data_r(), // analog yoke conversion result
            0x4400 => {
                self.mainlatch_pending = false;
                self.mainlatch
            }
            0x4401 => {
                ((self.soundlatch_pending as u8) << 7) | ((self.mainlatch_pending as u8) << 6)
            }
            0x4500..=0x45FF => self.novram.read(addr & 0xFF),
            0x4700 => self.math.div_reh_r(),
            0x4701 => self.math.div_rel_r(),
            0x4703 => self.math.prng_r(),
            _ => 0xFF,
        }
    }

    fn main_write(&mut self, addr: u16, data: u8) {
        self.feed_slapstic(addr);
        match addr {
            0x0000..=0x2FFF | 0x4800..=0x5FFF => self.main_map.write_backing(addr, data),
            0x4400 => {
                self.soundlatch = data;
                self.soundlatch_pending = true;
            }
            0x4500..=0x45FF => self.novram.write(addr & 0xFF, data),
            0x4600..=0x461F => self.trigger_avg(),
            0x4620..=0x463F => self.avg.reset(),
            0x4640..=0x465F => self.pet_watchdog(),
            0x4660..=0x467F => self.irq_pending = false, // IRQ acknowledge
            0x4680..=0x469F => self.outlatch_w(addr, data),
            0x46A0..=0x46BF => {
                // X2212 nstore: STORE pulse (0→1→0).
                self.novram.store(false);
                self.novram.store(true);
                self.novram.store(false);
            }
            0x46C0..=0x46C3 => {
                // Select an ADC channel and start a conversion; feed the current
                // yoke position so the read returns a fresh value.
                self.push_stick();
                self.adc.address_offset_start_w(addr & 0x03);
            }
            0x46E0 => self.sound_reset(),
            0x4700..=0x4707 => {
                let offset = (addr & 0x07) as u8;
                // Disjoint field borrows: the Matrix Processor operates on the
                // Math RAM region while it runs.
                self.math.math_w(
                    offset,
                    data,
                    self.main_map.region_data_mut(MainRegion::MathRam),
                );
            }
            _ => {}
        }
    }

    /// LS259 addressable output latch (9L/M): D7 is latched into bit `addr & 7`.
    fn outlatch_w(&mut self, addr: u16, data: u8) {
        let bit = (addr & 0x07) as u8;
        let val = (data >> 7) & 1;
        match bit {
            4 => {
                // ROM bank select ($6000–$7FFF). On ESB the same line also
                // switches bank 2 ($A000–$FFFF) between its two 24 KB entries.
                self.bank = val;
                let region = if val == 0 {
                    MainRegion::BankLow
                } else {
                    MainRegion::BankHigh
                };
                self.main_map.remap_pages(0x60, 0x20, region, 0);
                if self.slapstic.is_some() {
                    self.main_map
                        .remap_pages(0xA0, 0x60, MainRegion::Bank2, val as u32 * 0x6000);
                }
                if self.debug_trace.enabled() {
                    self.debug_trace.record(DebugEvent {
                        cpu_index: Some(0),
                        pc: self.main_map.latched_pc(),
                        addr: Some(addr as u32),
                        value: Some(val as u32),
                        width: 1,
                        region: Some("ROM Bank"),
                        detail: Some(if val == 0 {
                            "LS259 bit 4 = 0: bank low at $6000 (ESB: bank 2 entry 0)"
                        } else {
                            "LS259 bit 4 = 1: bank high at $6000 (ESB: bank 2 entry 1)"
                        }),
                        ..DebugEvent::new(
                            self.clock,
                            DebugAccessSource::Cpu(0),
                            DebugEventKind::BankSwitch,
                        )
                    });
                }
            }
            7 => self.novram.recall(val == 0), // NVRAM array recall (active low)
            // bits 0/1 coin counters, 2/3/6 LEDs, 5 PRNG reset — no board state.
            _ => {}
        }
    }

    fn in0(&self) -> u8 {
        // Coin/service/tilt/button1/button4, all active-low, so an unpressed
        // control reads 1. Bit 5 is the exception: it is unused and wired
        // active-*high*, so it reads 0 at rest rather than 1.
        self.in0 & !IN0_UNUSED_BIT
    }

    fn in1(&self) -> u8 {
        // Active-low button bits (2,4,5); bit 6 = AVG done (VG_HALT, active
        // high), bit 7 = MATH_RUN (active high).
        let mut v = self.in1_buttons & 0x34;
        if self.avg.is_halted() {
            v |= 0x40;
        }
        if self.math.math_run() {
            v |= 0x80;
        }
        v
    }

    fn pet_watchdog(&mut self) {
        self.watchdog_counter = 0;
        if self.debug_trace.enabled() {
            self.debug_trace.record(DebugEvent {
                cpu_index: Some(0),
                pc: self.main_map.latched_pc(),
                addr: Some(0x4640),
                detail: Some("watchdog cleared"),
                ..DebugEvent::new(
                    self.clock,
                    DebugAccessSource::Cpu(0),
                    DebugEventKind::Watchdog,
                )
            });
        }
    }

    /// Feed the current yoke position to the ADC channels.
    fn push_stick(&mut self) {
        self.adc
            .set_input(ADC_PITCH_CH, self.stick[1].position() as u8);
        self.adc
            .set_input(ADC_YAW_CH, self.stick[0].position() as u8);
        self.adc.set_input(2, 0); // thrust (unused)
    }

    /// Recompute the yoke from the held keys (full deflection while held,
    /// spring-centered when released).
    pub(crate) fn update_yoke_keys(&mut self) {
        self.push_stick();
    }

    /// Nudge an analog yoke axis by a relative (mouse) delta.
    pub(crate) fn move_stick(&mut self, axis: usize, delta: i32) {
        self.stick[axis].move_relative(delta as f32);
        self.push_stick();
    }

    /// Set an analog yoke axis to an absolute position (pad stick, −1.0..=1.0).
    pub(crate) fn set_stick(&mut self, axis: usize, value: f32) {
        // Both directions scale by the *upper* half-span, even though center is
        // not the midpoint of 0x00..=0xFF. AnalogAxis::set_absolute scales each
        // side by its own span, which would move full-left from 0x01 to 0x00.
        let v = STICK_CENTER + (value.clamp(-1.0, 1.0) * (STICK_MAX - STICK_CENTER) as f32) as i32;
        self.stick[axis].set_position(v);
        self.push_stick();
    }

    fn sound_reset(&mut self) {
        // Acknowledge both mailbox latches and request a sound-CPU reset (done on
        // the next tick, where a bus is available for the reset-vector fetch).
        self.soundlatch_pending = false;
        self.mainlatch_pending = false;
        self.sound_reset_pending = true;
    }

    // --- Sound CPU bus -----------------------------------------------------

    fn sound_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0800..=0x0FFF => {
                self.soundlatch_pending = false;
                self.soundlatch
            }
            0x1000..=0x107F => self.riot.read_ram((addr & 0x7F) as u8),
            0x1080..=0x109F => {
                let off = (addr & 0x1F) as u8;
                // Port B (data reg) reads the TMS status byte.
                if off == 0x02 {
                    self.riot.set_pb_input(self.tms.status_r());
                }
                self.riot.read_io(off)
            }
            0x1800..=0x183F => {
                let (p, reg) = Self::quad_pokey_decode(addr & 0x3F);
                self.pokey[p].read(reg)
            }
            0x2000..=0x27FF | 0x4000..=0x7FFF | 0xB000..=0xFFFF => {
                self.sound_map.read_backing(addr)
            }
            _ => 0x00,
        }
    }

    fn sound_write(&mut self, addr: u16, data: u8) {
        match addr {
            0x0000..=0x07FF => {
                self.mainlatch = data;
                self.mainlatch_pending = true;
            }
            0x1000..=0x107F => self.riot.write_ram((addr & 0x7F) as u8, data),
            0x1080..=0x109F => {
                let off = (addr & 0x1F) as u8;
                self.riot.write_io(off, data);
                // A Port B write presents a speech-data byte to the TMS (latched
                // on the /WS strobe on real hardware).
                if off == 0x02 {
                    self.tms.data_w(data);
                }
            }
            0x1800..=0x183F => {
                let (p, reg) = Self::quad_pokey_decode(addr & 0x3F);
                self.pokey[p].write(reg, data);
            }
            0x2000..=0x27FF => self.sound_map.write_backing(addr, data),
            _ => {}
        }
    }

    /// Refresh the RIOT Port A input pins the TMS/mailbox drive: PA2 = TMS
    /// `/READY` (active low), PA4 = 1 (not self-test), PA6 = main-latch pending,
    /// PA7 = sound-latch pending.
    fn refresh_riot_pa(&mut self) {
        let mut pa = 0x10; // PA4 tied high
        if !self.tms.ready() {
            pa |= 0x04; // /READY high when the TMS cannot accept data
        }
        if self.mainlatch_pending {
            pa |= 0x40;
        }
        if self.soundlatch_pending {
            pa |= 0x80;
        }
        self.riot.set_pa_input(pa);
    }

    // --- Bus dispatch (routed by master) -----------------------------------

    pub(crate) fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        if master == BusMaster::Cpu(1) {
            let data = self.sound_read(addr);
            self.sound_map.watch_read(1, master, addr, data);
            data
        } else {
            let data = self.main_read(addr);
            self.main_map.watch_read(0, master, addr, data);
            data
        }
    }

    pub(crate) fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        if master == BusMaster::Cpu(1) {
            self.sound_map.watch_write(1, master, addr, data);
            self.sound_write(addr, data);
        } else {
            self.main_map.watch_write(0, master, addr, data);
            self.main_write(addr, data);
        }
    }

    pub(crate) fn bus_is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    pub(crate) fn bus_check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        match target {
            BusMaster::Cpu(0) => InterruptState {
                irq: self.irq_pending,
                ..Default::default()
            },
            // Sound CPU IRQ is the RIOT timer/edge interrupt.
            BusMaster::Cpu(1) => InterruptState {
                irq: self.riot.irq_active(),
                ..Default::default()
            },
            _ => InterruptState::default(),
        }
    }

    // --- Vector generator --------------------------------------------------

    /// VGGO: restart the vector generator at the top of the list.
    ///
    /// Star Wars' list ends in HALT rather than looping, so the frame is
    /// delimited by the halt: the display list is taken when the generator
    /// actually reaches it, which is now a real moment in emulated time rather
    /// than the instant of the GO write.
    pub(crate) fn trigger_avg(&mut self) {
        self.avg.go();
        self.avg.take_display_list();
    }

    /// Advance the vector generator alongside the CPUs.
    ///
    /// The 12.096 MHz crystal drives the generator directly and the 6809E
    /// through a divide-by-8, so one CPU cycle is eight AVG cycles. VG_HALT
    /// (IN1 bit 6) then reads busy for exactly as long as the generator is
    /// still walking the list, which is what the game polls; there is no
    /// separate countdown to keep in step with it any more.
    fn step_avg(&mut self) {
        let mem = VectorMemory::split(
            self.main_map.region_data(MainRegion::Ram),
            self.main_map.region_data(MainRegion::VectorRom),
            0x3000,
        );
        // Star Wars has no color RAM: its colors come from the color111 index.
        if self.avg.step(AVG_CYCLES_PER_CPU_CYCLE, &mem, &[0u8; 16]) {
            self.display_list = self.avg.take_display_list();
        }
    }

    // --- Frame execution ---------------------------------------------------

    /// Board work before the CPUs' cycle: the periodic IRQ divider, the AVG
    /// busy countdown and the watchdog.
    pub(crate) fn begin_cycle(&mut self, cpu: &M6809) {
        // Compare before incrementing, so the divider fires on cycle
        // `IRQ_PERIOD_CYCLES` rather than one earlier. Incrementing first makes
        // the counter reach the period on cycle N-1, and an interrupt asserted
        // one cycle early is invisible until an instruction boundary happens to
        // fall in that one-cycle window — whereupon the CPU takes the interrupt
        // an instruction sooner than the hardware would.
        if self.irq_counter >= IRQ_PERIOD_CYCLES {
            self.irq_counter = 0;
            self.irq_pending = true;
        }
        self.irq_counter += 1;

        self.step_avg();

        if self.watchdog_counter >= WATCHDOG_CYCLES {
            // Record the edge, not the level: the flag stays set until
            // `take_watchdog_trip` clears it, and one event per cycle until
            // then would bury the trip that matters.
            if !self.watchdog_tripped && self.debug_trace.enabled() {
                self.debug_trace.record(DebugEvent {
                    cpu_index: Some(0),
                    pc: Some(cpu.pc as u32),
                    detail: Some("watchdog expired — board reset"),
                    ..DebugEvent::new(
                        self.clock,
                        DebugAccessSource::Cpu(0),
                        DebugEventKind::Watchdog,
                    )
                });
            }
            self.watchdog_tripped = true;
        }
        self.watchdog_counter += 1;

        // Latch the executing CPU's PC + cycle so any watchpoint hit taken via
        // bus_read/bus_write this cycle carries usable debugger metadata, and
        // so bus-driven trace events (the slapstic bank switch) can name the
        // instruction that caused them. Only when an observer is armed, so
        // normal runs pay nothing.
        if self.main_map.has_any_watchpoints() || self.debug_trace.enabled() {
            self.main_map
                .latch_access_context(self.clock, Some(cpu.pc as u32));
        }
    }

    /// Latch the sound CPU's PC before its cycle, so a watchpoint hit it takes
    /// carries the instruction that caused it.
    pub(crate) fn latch_sound_pc(&mut self, sound_cpu: &M6809) {
        if self.sound_map.has_any_watchpoints() {
            self.sound_map
                .latch_access_context(self.clock, Some(sound_cpu.pc as u32));
        }
    }

    /// Board work after the CPUs' cycle: the matrix processor, the sound
    /// board, and the clock advance.
    pub(crate) fn end_cycle(&mut self) {
        self.math.tick();
        for p in &mut self.pokey {
            p.tick();
        }

        // Drive the RIOT input pins, then clock it and the TMS (672 kHz).
        self.refresh_riot_pa();
        self.riot.tick();
        if self.clocks.tick(self.tms_dom) {
            self.tms.tick();
        }

        self.clock += 1;
    }

    /// True (and cleared) if the watchdog expired since the last check.
    pub(crate) fn take_watchdog_trip(&mut self) -> bool {
        let tripped = self.watchdog_tripped;
        self.watchdog_tripped = false;
        tripped
    }

    pub(crate) fn reset(&mut self) {
        self.avg.reset();
        self.math.reset();
        self.novram.reset();
        for p in &mut self.pokey {
            p.reset();
        }
        self.riot.reset();
        self.tms.reset();
        self.clocks.reset();
        self.adc.reset();
        self.stick = new_yoke();
        self.push_stick();
        self.audio_buffer.clear();
        self.audio_dc_prev_in = 0.0;
        self.audio_dc_prev_out = 0.0;
        self.bank = 0;
        self.main_map
            .remap_pages(0x60, 0x20, MainRegion::BankLow, 0);
        // ESB: power the slapstic back to its start bank and re-page the
        // slapstic window ($8000) and bank 2 ($A000) to their reset entries.
        if let Some(sl) = self.slapstic.as_mut() {
            sl.reset();
            let bank = sl.current_bank() as u32;
            self.main_map
                .remap_pages(0x80, 0x20, MainRegion::SlapsticWindow, bank * 0x2000);
            self.main_map.remap_pages(0xA0, 0x60, MainRegion::Bank2, 0);
        }
        self.soundlatch = 0;
        self.soundlatch_pending = false;
        self.mainlatch = 0;
        self.mainlatch_pending = false;
        self.irq_pending = false;
        self.irq_counter = 0;
        self.watchdog_counter = 0;
        self.watchdog_tripped = false;
        self.clock = 0;
        self.display_list.clear();
    }

    // --- Frontend hooks ----------------------------------------------------

    pub(crate) fn render_frame(&self, buffer: &mut [u8]) {
        // flip_y: the AVG emits a Y-up display list; a normal (ROT0) monitor maps
        // that to screen Y-down.
        let field = TIMING.display_size();
        let (rw, rh) = raster_size_for_field(field.0, field.1);
        rasterize_vectors(
            &self.display_list,
            buffer,
            rw,
            rh,
            field,
            true,
            // The viewer's settings, minus the glow this path cannot afford.
            &display_settings().without_halation(),
        );
    }

    pub(crate) fn vector_display_list(&self) -> Option<&[VectorLine]> {
        Some(&self.display_list)
    }

    /// Drain the four POKEYs, mix (with a one-pole DC-blocking high-pass), and
    /// queue signed-16-bit samples for the frontend. Called once per frame.
    pub(crate) fn end_frame_audio(&mut self) {
        const POKEY_GAIN: f32 = 0.20; // MAME per-POKEY route gain
        const SPEECH_GAIN: f32 = 0.50; // MAME TMS5220 route gain
        let chans: [Vec<f32>; 4] = std::array::from_fn(|i| self.pokey[i].drain_audio());
        let speech = self.tms.drain_audio();
        let n = chans
            .iter()
            .map(Vec::len)
            .chain(std::iter::once(speech.len()))
            .max()
            .unwrap_or(0);

        let (mut x1, mut y1) = (self.audio_dc_prev_in, self.audio_dc_prev_out);
        for i in 0..n {
            let pokey = POKEY_GAIN
                * chans
                    .iter()
                    .map(|c| c.get(i).copied().unwrap_or(0.0))
                    .sum::<f32>();
            let x = pokey + SPEECH_GAIN * speech.get(i).copied().unwrap_or(0.0);
            // DC block (cutoff ≈ 35 Hz) then scale/clamp to i16.
            let y = x - x1 + 0.995 * y1;
            x1 = x;
            y1 = y;
            self.audio_buffer
                .push((y * 2.0 * 32767.0).clamp(-32767.0, 32767.0) as i16);
        }
        self.audio_dc_prev_in = x1;
        self.audio_dc_prev_out = y1;
    }

    /// Copy pending audio into the frontend's buffer.
    pub(crate) fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.audio_buffer.pop_front_into(buffer)
    }

    /// Hand the TMS5220 its clock back from the tree after a load.
    ///
    /// The domain comes back at whatever rate it was running at, because the
    /// tree saves its live ratio, but the device save-skips its own clock as
    /// configuration, so it has to be told again.
    fn resync_tms_clock(&mut self) {
        let hz = self.clocks.hz(self.tms_dom) as u32;
        self.tms.set_clock(hz);
    }

    /// Whether the main CPU is at an instruction boundary. It lives on the
    /// machine, which passes it back in.
    pub(crate) fn instruction_boundaries(cpu: &M6809) -> u32 {
        cpu.at_instruction_boundary() as u32
    }
}

// ---------------------------------------------------------------------------
// System wrapper
// ---------------------------------------------------------------------------

/// Star Wars / Empire Strikes Back machine: the board plus the registry id the
/// two variants report (`machine_id` distinguishes them for save-state and NVRAM
/// paths since both share this wrapper type).
#[derive(phosphor_macros::Saveable, phosphor_macros::BusDebug)]
pub(crate) struct StarWarsSystem {
    /// Both 6809s are held beside the board, which is their bus.
    #[debug_cpu("M6809 Main")]
    pub(crate) cpu: M6809,
    #[debug_cpu("M6809 Sound")]
    pub(crate) sound_cpu: M6809,

    #[debug_bus]
    pub(crate) board: StarWarsBoard,
    #[save_skip]
    pub(crate) machine_id: &'static str,
}

impl StarWarsSystem {
    pub(crate) fn new() -> Self {
        Self {
            cpu: M6809::new(),
            sound_cpu: M6809::new(),
            board: StarWarsBoard::new(),
            machine_id: "starwars",
        }
    }

    /// The Empire Strikes Back variant (slapstic-banked window + bank 2).
    pub(crate) fn new_esb() -> Self {
        Self {
            cpu: M6809::new(),
            sound_cpu: M6809::new(),
            board: StarWarsBoard::new_esb(),
            machine_id: "esb",
        }
    }

    /// Which game this system is running. The board's ESB-specific state (the
    /// slapstic) is the variant flag — `machine_id` is `#[save_skip]` and so is
    /// not restored by a save state, while the slapstic is.
    fn is_esb(&self) -> bool {
        self.board.slapstic.is_some()
    }

    /// Service a pending sound-CPU reset ($46E0) before a cycle-stepped tick.
    ///
    /// `run_frame` does this once per frame, before its cycle loop. The
    /// debugger and the headless
    /// `trace --cpu`/`--break-pc` loops call `debug_tick` instead and never go
    /// through `run_frame`, so without this the request would sit pending
    /// forever and the sound CPU would keep running the pre-reset code — the
    /// machine would behave differently under the debugger than at full speed,
    /// which is the one thing a debugger must not do.
    /// Service a pending sound-CPU reset ($46E0).
    ///
    /// The reset fetches a vector through the bus, which is why it is deferred
    /// out of the $46E0 write: the CPU and the bus are separate fields here, so
    /// this is now a plain call rather than something needing a borrow split.
    fn service_sound_reset(&mut self) {
        if self.board.sound_reset_pending {
            self.board.sound_reset_pending = false;
            self.sound_cpu.reset(&mut self.board, BusMaster::Cpu(1));
        }
    }

    /// One CPU cycle: board work, both 6809s, then the sound board.
    pub(crate) fn step_cycle(&mut self) -> u32 {
        self.service_sound_reset();
        self.tick_once();
        StarWarsBoard::instruction_boundaries(&self.cpu)
    }

    /// One cycle without the deferred-reset check (the frame loop does that
    /// once per frame, as the hardware's reset line would settle).
    fn tick_once(&mut self) {
        self.board.begin_cycle(&self.cpu);
        self.cpu.execute_cycle(&mut self.board, BusMaster::Cpu(0));
        self.board.latch_sound_pc(&self.sound_cpu);
        self.sound_cpu
            .execute_cycle(&mut self.board, BusMaster::Cpu(1));
        self.board.end_cycle();
    }
}

// The board is the bus: Star Wars and Empire share it, and the variant
// differences (the slapstic window, the bank) are board state already.
impl Bus for StarWarsBoard {
    type Address = u16;
    type Data = u8;

    #[inline]
    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        self.bus_read(master, addr)
    }

    #[inline]
    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.bus_write(master, addr, data);
    }

    #[inline]
    fn is_halted_for(&self, master: BusMaster) -> bool {
        self.bus_is_halted_for(master)
    }

    #[inline]
    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        self.bus_check_interrupts(target)
    }
}

crate::impl_board_renderable!(StarWarsSystem, board, TIMING, vector_field, vectors);
crate::impl_board_audio!(StarWarsSystem, board);

// `MachineDebug` is hand-written rather than macro-generated because this
// board overrides `set_debug_entropy`, which the delegation macro has no arm
// for. Everything else is the same delegation the macro emits.
impl phosphor_core::core::machine::MachineDebug for StarWarsSystem {
    fn debug_bus(&self) -> Option<&dyn phosphor_core::core::debug::BusDebug> {
        // The machine, not the board: it owns the CPUs and merges the board's
        // devices and maps through `#[debug_bus]`.
        Some(self)
    }

    fn debug_bus_mut(&mut self) -> Option<&mut dyn phosphor_core::core::debug::BusDebug> {
        Some(self)
    }

    fn cycles_per_frame(&self) -> u64 {
        TIMING.cycles_per_frame()
    }

    fn debug_tick(&mut self) -> u32 {
        self.step_cycle()
    }

    /// The board's only entropy source is the Matrix Processor PRNG at $4703.
    fn set_debug_entropy(&mut self, values: &[u8]) -> usize {
        self.board.math.set_prng_replay(values)
    }
}

impl MachineCore for StarWarsSystem {
    fn frame_rate_hz(&self) -> f64 {
        TIMING.frame_rate_hz()
    }

    fn machine_id(&self) -> &str {
        self.machine_id
    }

    crate::machine_clock_declaration!(TIMING, crate::starwars::clock_tree);

    fn run_frame(&mut self) {
        // A pending sound-CPU reset ($46E0) is serviced once per frame; the
        // cycle-stepped debugger paths do it per step in `step_cycle`.
        self.service_sound_reset();
        for _ in 0..TIMING.cycles_per_frame() {
            self.tick_once();
        }
        self.board.end_frame_audio();
        if self.board.take_watchdog_trip() {
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.board.reset();
        self.cpu.reset(&mut self.board, BusMaster::Cpu(0));
        self.sound_cpu.reset(&mut self.board, BusMaster::Cpu(1));
    }
}

impl SaveState for StarWarsSystem {
    crate::machine_save_state!();
}

impl InputConfigurable for StarWarsSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        STARWARS_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let b = &mut self.board;
        match event {
            InputEvent::Button { id, pressed } => match id.0 as u8 {
                // IN0 (active-low)
                INPUT_COIN2 => set_bit_active_low(&mut b.in0, 0, pressed),
                INPUT_COIN1 => set_bit_active_low(&mut b.in0, 1, pressed),
                INPUT_SERVICE => set_bit_active_low(&mut b.in0, 4, pressed),
                INPUT_FIRE4 => set_bit_active_low(&mut b.in0, 6, pressed), // BUTTON4
                INPUT_FIRE1 => set_bit_active_low(&mut b.in0, 7, pressed), // BUTTON1
                // IN1 button bits (active-low)
                INPUT_FIRE3 => set_bit_active_low(&mut b.in1_buttons, 4, pressed), // BUTTON3
                INPUT_FIRE2 => set_bit_active_low(&mut b.in1_buttons, 5, pressed), // BUTTON2
                // Digital yoke deflection [up, down, left, right].
                INPUT_YOKE_UP => {
                    b.stick[1].set_held(true, pressed);
                    b.update_yoke_keys();
                }
                INPUT_YOKE_DOWN => {
                    b.stick[1].set_held(false, pressed);
                    b.update_yoke_keys();
                }
                INPUT_YOKE_LEFT => {
                    b.stick[0].set_held(false, pressed);
                    b.update_yoke_keys();
                }
                INPUT_YOKE_RIGHT => {
                    b.stick[0].set_held(true, pressed);
                    b.update_yoke_keys();
                }
                _ => {}
            },
            // Mouse motion nudges the analog yoke.
            InputEvent::Relative { id, delta } => {
                if id == CTRL_YOKE_X {
                    b.move_stick(0, delta as i32);
                } else if id == CTRL_YOKE_Y {
                    b.move_stick(1, delta as i32);
                }
            }
            // Pad stick sets an absolute yoke position.
            InputEvent::Absolute { id, value } => {
                if id == CTRL_YOKE_X {
                    b.set_stick(0, value);
                } else if id == CTRL_YOKE_Y {
                    b.set_stick(1, value);
                }
            }
        }
    }

    /// Also clears the conditioned yoke: the digital releases above cannot
    /// reach a deflection the mouse or pad set.
    fn release_all_inputs(&mut self) {
        phosphor_core::core::machine::release_all_controls(self);
        for axis in &mut self.board.stick {
            axis.release_all();
        }
        self.board.push_stick();
    }
}

// ---------------------------------------------------------------------------
// Operator DIP switches
// ---------------------------------------------------------------------------
//
// Two banks on the main board: DSW0 at 10D (read at $4340-$435F) and DSW1 at
// 10EF (read at $4360-$437F). Both are read live on every access, so every
// option applies immediately.
//
// DSW1 is identical on both games; DSW0 is reshaped by Empire Strikes Back,
// which keeps only the bit-7 Freeze switch in place. That is why this impl is
// hand-written rather than `impl_dip_switches!`: the macro returns one static
// bank table, and the correct table here depends on which game is running.

/// Freeze (10D:8) — the same switch on both games, so both DSW0 tables end
/// with it.
const DSW0_FREEZE: DipOption = DipOption {
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
};

/// Star Wars DSW0 (10D): shields, difficulty, bonus shields, demo sounds.
const SW_DSW0_OPTIONS: &[DipOption] = &[
    DipOption {
        name: "Starting Shields",
        mask: 0x03,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "6",
                value: 0x00,
            },
            DipChoice {
                label: "7",
                value: 0x01,
            },
            DipChoice {
                label: "8",
                value: 0x02,
            },
            DipChoice {
                label: "9",
                value: 0x03,
            },
        ],
    },
    DipOption {
        name: "Difficulty",
        mask: 0x0C,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "Easy",
                value: 0x00,
            },
            DipChoice {
                label: "Moderate",
                value: 0x04,
            },
            DipChoice {
                label: "Hard",
                value: 0x08,
            },
            DipChoice {
                label: "Hardest",
                value: 0x0C,
            },
        ],
    },
    DipOption {
        name: "Bonus Shields",
        mask: 0x30,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "0",
                value: 0x00,
            },
            DipChoice {
                label: "1",
                value: 0x10,
            },
            DipChoice {
                label: "2",
                value: 0x20,
            },
            DipChoice {
                label: "3",
                value: 0x30,
            },
        ],
    },
    DipOption {
        name: "Demo Sounds",
        mask: 0x40,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "On",
                value: 0x00,
            },
            DipChoice {
                label: "Off",
                value: 0x40,
            },
        ],
    },
    DSW0_FREEZE,
];

/// Empire Strikes Back DSW0 (10D). The shield and difficulty encodings are not
/// merely relabelled — the bit patterns are permuted (shields count *up* as
/// 3,2,5,4 across 0..3), so the tables cannot be shared.
const ESB_DSW0_OPTIONS: &[DipOption] = &[
    DipOption {
        name: "Starting Shields",
        mask: 0x03,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "3",
                value: 0x00,
            },
            DipChoice {
                label: "2",
                value: 0x01,
            },
            DipChoice {
                label: "5",
                value: 0x02,
            },
            DipChoice {
                label: "4",
                value: 0x03,
            },
        ],
    },
    DipOption {
        name: "Difficulty",
        mask: 0x0C,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "Hard",
                value: 0x00,
            },
            DipChoice {
                label: "Hardest",
                value: 0x04,
            },
            DipChoice {
                label: "Easy",
                value: 0x08,
            },
            DipChoice {
                label: "Moderate",
                value: 0x0C,
            },
        ],
    },
    DipOption {
        name: "Jedi-Letter Mode",
        mask: 0x30,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "Level Only",
                value: 0x00,
            },
            DipChoice {
                label: "Level",
                value: 0x10,
            },
            DipChoice {
                label: "Increment Only",
                value: 0x20,
            },
            DipChoice {
                label: "Increment",
                value: 0x30,
            },
        ],
    },
    DipOption {
        name: "Demo Sounds",
        mask: 0x40,
        apply: DipApplyTiming::Immediate,
        choices: &[
            // Inverted relative to Star Wars: on ESB the switch is labelled
            // "Music In Attract Mode", and *off* (bit set) is the on state.
            DipChoice {
                label: "Off",
                value: 0x00,
            },
            DipChoice {
                label: "On",
                value: 0x40,
            },
        ],
    },
    DSW0_FREEZE,
];

/// DSW1 (10EF), shared by both games: coinage, the two coin multipliers, and
/// the bonus-coin adder.
///
/// The adder leaves 0xC0 and 0xE0 undefined — the manual lists six settings for
/// three switches — so those two patterns have no label and are simply not
/// offered.
const DSW1_OPTIONS: &[DipOption] = &[
    DipOption {
        name: "Coinage",
        mask: 0x03,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "Free Play",
                value: 0x00,
            },
            DipChoice {
                label: "1 Coin/2 Credits",
                value: 0x01,
            },
            DipChoice {
                label: "1 Coin/1 Credit",
                value: 0x02,
            },
            DipChoice {
                label: "2 Coins/1 Credit",
                value: 0x03,
            },
        ],
    },
    DipOption {
        name: "Right Coin Mechanism",
        mask: 0x0C,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "×1",
                value: 0x00,
            },
            DipChoice {
                label: "×4",
                value: 0x04,
            },
            DipChoice {
                label: "×5",
                value: 0x08,
            },
            DipChoice {
                label: "×6",
                value: 0x0C,
            },
        ],
    },
    DipOption {
        name: "Left Coin Mechanism",
        mask: 0x10,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "×1",
                value: 0x00,
            },
            DipChoice {
                label: "×2",
                value: 0x10,
            },
        ],
    },
    DipOption {
        name: "Bonus Coin Adder",
        mask: 0xE0,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "None",
                value: 0x00,
            },
            DipChoice {
                label: "2 gives 1",
                value: 0x20,
            },
            DipChoice {
                label: "4 gives 1",
                value: 0x40,
            },
            DipChoice {
                label: "4 gives 2",
                value: 0x60,
            },
            DipChoice {
                label: "5 gives 1",
                value: 0x80,
            },
            DipChoice {
                label: "3 gives 1",
                value: 0xA0,
            },
        ],
    },
];

const STARWARS_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSW0",
        options: SW_DSW0_OPTIONS,
    },
    DipSwitchBank {
        name: "DSW1",
        options: DSW1_OPTIONS,
    },
];

const ESB_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSW0",
        options: ESB_DSW0_OPTIONS,
    },
    DipSwitchBank {
        name: "DSW1",
        options: DSW1_OPTIONS,
    },
];

impl DipSwitches for StarWarsSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        if self.is_esb() {
            ESB_DIP_BANKS
        } else {
            STARWARS_DIP_BANKS
        }
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        match bank {
            0 => self.board.dsw0,
            1 => self.board.dsw1,
            _ => 0,
        }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        match bank {
            0 => self.board.dsw0 = value,
            1 => self.board.dsw1 = value,
            _ => {}
        }
    }
}

impl Nvram for StarWarsSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.novram.nvram())
    }

    fn load_nvram(&mut self, data: &[u8]) {
        self.board.novram.load_nvram(data);
    }
}

impl Profilable for StarWarsSystem {}

crate::impl_board_debug_trace!(StarWarsSystem, board);

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

fn create_machine(rom_set: &RomSet) -> Result<Box<dyn FrontendMachine>, RomLoadError> {
    let mut sys = StarWarsSystem::new();
    sys.board.load_rom_set(rom_set)?;
    sys.reset();
    Ok(Box::new(sys))
}

fn create_esb_machine(rom_set: &RomSet) -> Result<Box<dyn FrontendMachine>, RomLoadError> {
    let mut sys = StarWarsSystem::new_esb();
    sys.board.load_esb_rom_set(rom_set)?;
    sys.reset();
    Ok(Box::new(sys))
}

// The ROM-less counterparts keep the variant distinction — ESB adds the
// slapstic-banked ROM window, so the two are different hardware even with no
// ROMs in them.
fn create_bare() -> Box<dyn FrontendMachine> {
    let mut sys = StarWarsSystem::new();
    let _ = sys.board.load_rom_set(&RomSet::blank());
    sys.reset();
    Box::new(sys)
}

fn create_esb_bare() -> Box<dyn FrontendMachine> {
    let mut sys = StarWarsSystem::new_esb();
    let _ = sys.board.load_esb_rom_set(&RomSet::blank());
    sys.reset();
    Box::new(sys)
}

inventory::submit! {
MachineEntry::new("starwars", &["starwars", "starwars1", "starwarso"], create_machine, create_bare, STARWARS_CONTROLS) }

inventory::submit! {
MachineEntry::new("esb", &["esb"], create_esb_machine, create_esb_bare, STARWARS_CONTROLS) }

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// `AVG_CYCLES_PER_CPU_CYCLE` is a hand-derived integer sitting in a
    /// comment. It is the ratio between two domains of the declared tree, so
    /// derive it there and hold the constant to it.
    #[test]
    fn avg_step_matches_the_declared_crystals() {
        let t = clock_tree();
        let avg = t
            .find(phosphor_core::core::ClockDomainName::Vector)
            .expect("the tree declares an AVG domain");
        let (num, den) = t.domain(avg).step_ratio();
        assert_eq!(den, 1, "the AVG is a whole multiple of the CPU clock");
        assert_eq!(
            num, AVG_CYCLES_PER_CPU_CYCLE,
            "AVG_CYCLES_PER_CPU_CYCLE says {AVG_CYCLES_PER_CPU_CYCLE}, but the \
             declared crystals divide out to {num}"
        );
    }

    /// Encode a 16-bit word into main RAM (or region backing) big-endian in the
    /// order the Star Wars AVG reads (op/high byte first, no XOR swap).
    fn put_avg_word(map: &mut AddressSpace16, addr: u16, word: u16) {
        let ram = map.region_data_mut(MainRegion::Ram);
        ram[addr as usize] = (word >> 8) as u8;
        ram[addr as usize + 1] = (word & 0xFF) as u8;
    }

    #[test]
    fn banking_swaps_the_6000_window() {
        let mut board = StarWarsBoard::new();
        // Distinct sentinel bytes at $6000 in each bank.
        board.main_map.region_data_mut(MainRegion::BankLow)[0] = 0xA1;
        board.main_map.region_data_mut(MainRegion::BankHigh)[0] = 0xB2;

        // Default bank 0.
        board
            .main_map
            .remap_pages(0x60, 0x20, MainRegion::BankLow, 0);
        assert_eq!(board.bus_read(BusMaster::Cpu(0), 0x6000), 0xA1);

        // LS259 bit 4 = 1 (write to $4684, D7 = 1) selects bank 1.
        board.bus_write(BusMaster::Cpu(0), 0x4684, 0x80);
        assert_eq!(board.bank, 1);
        assert_eq!(board.bus_read(BusMaster::Cpu(0), 0x6000), 0xB2);

        // Back to bank 0.
        board.bus_write(BusMaster::Cpu(0), 0x4684, 0x00);
        assert_eq!(board.bus_read(BusMaster::Cpu(0), 0x6000), 0xA1);
    }

    #[test]
    fn matrix_processor_runs_via_the_bus() {
        // Reuse the C1 datapath check but drive it through the $4700 registers
        // and the $5000 Math RAM region. Program: A<-RAM[0], B<-RAM[1],
        // C<-RAM[2] (MAC), store ACC to RAM[3], halt.
        const LDA: u8 = 0x80;
        const LDB: u8 = 0x40;
        const LDC: u8 = 0x20;
        const READ_ACC: u8 = 0x02;
        const CLEAR_ACC: u8 = 0x10;
        const M_HALT: u8 = 0x04;

        // Build a PROM image (4 × 0x400 nibble planes) with 5 steps.
        let mut prom = vec![0u8; 0x1000];
        let mut set = |step: usize, strobe: u8, am: u8, mas: u8| {
            prom[step] = (strobe >> 4) & 0xf;
            prom[0x400 + step] = strobe & 0xf;
            prom[0x800 + step] = ((am & 1) << 3) | ((mas >> 4) & 0x7);
            prom[0xc00 + step] = mas & 0xf;
        };
        set(0, CLEAR_ACC | LDA, 1, 0);
        set(1, LDB, 1, 1);
        set(2, LDC, 1, 2);
        set(3, READ_ACC, 1, 3);
        set(4, M_HALT, 1, 0);

        let mut board = StarWarsBoard::new();
        board.math.load_proms(&prom);

        // Operands in Math RAM ($5000): A=256, B=0, C=256 (big-endian words).
        let mr = board.main_map.region_data_mut(MainRegion::MathRam);
        mr[0] = 0x01;
        mr[1] = 0x00; // A
        mr[4] = 0x01;
        mr[5] = 0x00; // C

        board.bus_write(BusMaster::Cpu(0), 0x4700, 0x00); // mw0: run

        // ACC = (256 * 256) * 4 = 0x40000; upper 16 bits = 0x0004 at word 3.
        let mr = board.main_map.region_data(MainRegion::MathRam);
        assert_eq!(((mr[6] as u16) << 8) | mr[7] as u16, 0x0004);
        // MATH_RUN asserted immediately after a run (IN1 bit 7).
        assert_ne!(board.in1() & 0x80, 0);
    }

    #[test]
    fn avg_go_produces_a_display_list() {
        let mut board = StarWarsBoard::new();
        // STAT (intensity/color), CNTR, VCTR, HALT — a minimal lit vector.
        put_avg_word(&mut board.main_map, 0x0000, 0x61F0); // STAT: intensity=0xF0,color=1
        put_avg_word(&mut board.main_map, 0x0002, 0x8000); // CNTR
        put_avg_word(&mut board.main_map, 0x0004, 0x0100); // VCTR word0
        put_avg_word(&mut board.main_map, 0x0006, 0x2100); // VCTR word1
        put_avg_word(&mut board.main_map, 0x0008, 0x2000); // HALT

        board.bus_write(BusMaster::Cpu(0), 0x4600, 0x00); // AVG GO

        // The generator runs on its own clock, so GO starts it rather than
        // drawing the list: nothing is published until it reaches the HALT.
        assert!(
            board.vector_display_list().is_some_and(|l| l.is_empty()),
            "GO starts the generator, it does not draw the list"
        );
        assert!(!board.avg.is_halted(), "and it is running");

        // VG_HALT stays busy while it walks the list, and the list appears when
        // it gets to the HALT. One CPU cycle is eight AVG cycles, and this list
        // is a handful of states plus one vector's beam time.
        for _ in 0..0x4000 {
            if board.avg.is_halted() {
                break;
            }
            assert_eq!(board.in1() & 0x40, 0, "VG_HALT reads busy while running");
            board.step_avg();
        }

        assert!(board.avg.is_halted(), "the HALT was reached");
        assert_ne!(board.in1() & 0x40, 0, "VG_HALT reads done once it parks");
        assert!(
            board.vector_display_list().is_some_and(|l| !l.is_empty()),
            "the finished pass was published"
        );
    }

    #[test]
    fn mailbox_latches_track_pending() {
        let mut board = StarWarsBoard::new();
        // Main writes a sound command → soundlatch pending (IN $4401 bit 7).
        board.bus_write(BusMaster::Cpu(0), 0x4400, 0x5A);
        assert_ne!(board.main_read(0x4401) & 0x80, 0);
        // Sound CPU reads it → pending clears.
        assert_eq!(board.sound_read(0x0800), 0x5A);
        assert_eq!(board.main_read(0x4401) & 0x80, 0);

        // Sound writes a response → mainlatch pending (IN $4401 bit 6).
        board.sound_write(0x0000, 0x3C);
        assert_ne!(board.main_read(0x4401) & 0x40, 0);
        assert_eq!(board.main_read(0x4400), 0x3C);
        assert_eq!(board.main_read(0x4401) & 0x40, 0);
    }

    #[test]
    fn quad_pokey_decode_selects_chip_and_register() {
        // pokey_num = (off>>3) & ~4; reg = (off%8) | ((off&0x20)>>2).
        assert_eq!(StarWarsBoard::quad_pokey_decode(0x00), (0, 0));
        assert_eq!(StarWarsBoard::quad_pokey_decode(0x08), (1, 0));
        assert_eq!(StarWarsBoard::quad_pokey_decode(0x18), (3, 0));
        assert_eq!(StarWarsBoard::quad_pokey_decode(0x07), (0, 7));
        // Bit 5 of the offset adds 8 to the register (control lines).
        assert_eq!(StarWarsBoard::quad_pokey_decode(0x28), (1, 8));
    }

    #[test]
    fn pokey_writes_produce_audio() {
        let mut board = StarWarsBoard::new();
        // Program POKEY 0 channel 1: a mid frequency at full volume/tone.
        board.sound_write(0x1800, 0x40); // AUDF1
        board.sound_write(0x1801, 0xAF); // AUDC1: volume 0xF, pure tone
        // Tick a frame's worth of sound cycles, then drain.
        for _ in 0..TIMING.cycles_per_frame() {
            for p in &mut board.pokey {
                p.tick();
            }
        }
        board.end_frame_audio();
        assert!(
            !board.audio_buffer.is_empty(),
            "an active POKEY channel should generate audio samples"
        );
    }

    #[test]
    fn tms5220_receives_speech_bytes_via_the_riot() {
        let mut board = StarWarsBoard::new();
        // Configure RIOT Port B as output (DDR B = 0xFF), then SPEAK EXTERNAL
        // followed by a couple of data bytes — all through the RIOT PB data reg.
        board.sound_write(0x1083, 0xFF); // DDR B = all output
        board.sound_write(0x1082, 0x60); // TMS SPEAK EXTERNAL command
        board.sound_write(0x1082, 0xAA); // speech data
        board.sound_write(0x1082, 0x55); // speech data

        // The TMS should now be talking / no longer idle: reading its status via
        // Port B returns a live status byte, and readyq feeds RIOT PA2.
        board.sound_write(0x1081, 0x00); // DDR A = all input (read strobes/status)
        let pa = board.sound_read(0x1080);
        // PA4 is tied high in every read.
        assert_ne!(pa & 0x10, 0, "PA4 (not-self-test) reads high");
        // Status read path is wired (does not panic and returns a byte).
        let _status = board.sound_read(0x1082);
    }

    #[test]
    fn riot_timer_raises_the_sound_irq() {
        let mut board = StarWarsBoard::new();
        // Start the RIOT timer with the shortest prescale (÷1) and IRQ enabled:
        // io offset 0x1C = A4|A3|A2 (timer, IE) with prescale bits 0. Write 1
        // count, then clock past it.
        board.sound_write(0x109C, 0x01);
        assert!(!board.bus_check_interrupts(BusMaster::Cpu(1)).irq);
        for _ in 0..8 {
            board.riot.tick();
        }
        assert!(
            board.bus_check_interrupts(BusMaster::Cpu(1)).irq,
            "RIOT timer underflow should raise the sound-CPU IRQ"
        );
    }

    #[test]
    fn rom_manifest_places_regions_and_splits_bank() {
        use crate::rom_loader::RomSet;

        // Synthetic ROM set: each file sized correctly, with sentinel bytes we
        // can trace to their region offsets. CRCs are skipped here (the real
        // CRC-validated load is exercised by the boot-check example).
        let mut f214 = vec![0u8; 0x4000];
        f214[0] = 0xA0; // start of bank 0
        f214[0x2000] = 0xB1; // start of bank 1
        let mut f107 = vec![0u8; 0x2000];
        f107[0] = 0x77;
        let mut f208 = vec![0u8; 0x2000];
        f208[0x1FFE] = 0xEE; // near the reset vector
        let f2000 = vec![0u8; 0x2000];
        let f1000 = vec![0u8; 0x1000];
        let f400 = vec![0u8; 0x400];
        let rs = RomSet::from_slices(&[
            ("136021.102.1hj", &f2000),
            ("136021.203.1jk", &f2000),
            ("136021.104.1kl", &f2000),
            ("136021.206.1m", &f2000),
            ("136021.214.1f", &f214),
            ("136021-105.1l", &f1000),
            ("136021-107.1jk", &f107),
            ("136021-208.1h", &f208),
            ("136021-110.7h", &f400),
            ("136021-111.7j", &f400),
            ("136021-112.7k", &f400),
            ("136021-113.7l", &f400),
        ]);

        // Bank ROM splits into two 8 KB banks.
        let bank = SW_BANK_ROM.load_skip_checksums(&rs).unwrap();
        assert_eq!(bank.len(), 0x4000);
        assert_eq!(bank[0], 0xA0);
        assert_eq!(bank[0x2000], 0xB1);

        // Program ROM is the four 8 KB ROMs, contiguous.
        assert_eq!(
            SW_PROGRAM_ROM.load_skip_checksums(&rs).unwrap().len(),
            0x8000
        );

        // Sound-hi region has a 4 KB leading gap, then the two ROMs; the reset
        // vector byte lands at $FFFE = region offset 0x4FFE ($FFFE − $B000).
        let shi = SW_SOUND_HI.load_skip_checksums(&rs).unwrap();
        assert_eq!(shi.len(), 0x5000);
        assert_eq!(shi[0], 0x00); // gap
        assert_eq!(shi[0x1000], 0x77); // 107 at $C000
        assert_eq!(shi[0x4FFE], 0xEE); // 208 near $FFFE
    }

    #[test]
    fn analog_yoke_feeds_adc_channels() {
        let mut sys = StarWarsSystem::new();

        // Keyboard: full pitch-up. Select channel 0 (pitch), start, read.
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_YOKE_UP as u16),
            pressed: true,
        });
        sys.board.bus_write(BusMaster::Cpu(0), 0x46C0, 0x00);
        assert_eq!(sys.board.bus_read(BusMaster::Cpu(0), 0x4380), 0xFF);

        // Yaw channel (1) has no input yet → spring-centered.
        sys.board.bus_write(BusMaster::Cpu(0), 0x46C1, 0x00);
        assert_eq!(sys.board.bus_read(BusMaster::Cpu(0), 0x4380), 0x80);

        // Pad stick: absolute full-right yaw.
        sys.handle_input(InputEvent::Absolute {
            id: CTRL_YOKE_X,
            value: 1.0,
        });
        sys.board.bus_write(BusMaster::Cpu(0), 0x46C1, 0x00);
        assert_eq!(sys.board.bus_read(BusMaster::Cpu(0), 0x4380), 0xFF);

        // Releasing the pitch key re-centers.
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_YOKE_UP as u16),
            pressed: false,
        });
        sys.board.bus_write(BusMaster::Cpu(0), 0x46C0, 0x00);
        assert_eq!(sys.board.bus_read(BusMaster::Cpu(0), 0x4380), 0x80);
    }

    #[test]
    fn nvram_round_trips_through_the_x2212() {
        let mut sys = StarWarsSystem::new();
        let len = sys.save_nvram().expect("nvram present").len();
        assert!(len > 0);
        // X2212 cells are 4-bit; mask to the storable nibble.
        let data: Vec<u8> = (0..len).map(|i| (i as u8) & 0x0F).collect();
        sys.load_nvram(&data);
        assert_eq!(sys.save_nvram().unwrap(), &data[..]);
    }

    #[test]
    fn watchdog_trips_without_petting() {
        let mut system = StarWarsSystem::new();
        // Enough frames of no watchdog writes to exceed the timeout.
        for _ in 0..3 {
            system.run_frame();
        }
        // run_frame resets on trip; the board should be alive (clock advancing)
        // and the watchdog flag consumed.
        assert!(!system.board.watchdog_tripped);
    }

    // -- Empire Strikes Back -------------------------------------------------

    #[test]
    fn esb_map_decodes_slapstic_window_and_bank2() {
        let board = StarWarsBoard::new_esb();
        // $8000–$9FFF is the slapstic window, $A000–$FFFF is bank 2 — both
        // backing regions, not the fixed program ROM the Star Wars map uses.
        assert_eq!(
            board.main_map.region_at(0x8000).unwrap().id,
            MainRegion::SlapsticWindow.into()
        );
        assert_eq!(
            board.main_map.region_at(0xA000).unwrap().id,
            MainRegion::Bank2.into()
        );
        assert_eq!(
            board.main_map.region_at(0xFFFE).unwrap().id,
            MainRegion::Bank2.into()
        );
        // The $6000 window still banks as on Star Wars.
        assert_eq!(
            board.main_map.region_at(0x6000).unwrap().id,
            MainRegion::BankLow.into()
        );
        assert!(board.slapstic.is_some());
    }

    #[test]
    fn esb_slapstic_banks_the_8000_window_through_the_bus() {
        let mut board = StarWarsBoard::new_esb();
        // Distinct marker at offset 0 of each 8 KB slapstic bank.
        for b in 0..4u8 {
            board.main_map.region_data_mut(MainRegion::SlapsticWindow)[b as usize * 0x2000] =
                0xB0 + b;
        }
        // Power-on bank is 3; reading the window base ($8000) arms the chip and
        // returns bank 3's marker.
        assert_eq!(board.bus_read(BusMaster::Cpu(0), 0x8000), 0xB3);
        assert_eq!(board.slapstic.as_ref().unwrap().current_bank(), 3);
        // Direct-select bank 0 (window offset 0x80), then read its marker.
        board.bus_read(BusMaster::Cpu(0), 0x8080);
        assert_eq!(board.slapstic.as_ref().unwrap().current_bank(), 0);
        assert_eq!(board.bus_read(BusMaster::Cpu(0), 0x8000), 0xB0);
    }

    #[test]
    fn esb_bank2_switches_with_ls259_bit4() {
        let mut board = StarWarsBoard::new_esb();
        // Marker at the base of each bank-2 entry: entry 0 at region offset 0,
        // entry 1 at region offset 0x6000.
        board.main_map.region_data_mut(MainRegion::Bank2)[0x0000] = 0xC0;
        board.main_map.region_data_mut(MainRegion::Bank2)[0x6000] = 0xC1;

        // Default bit 4 = 0 → entry 0.
        assert_eq!(board.bus_read(BusMaster::Cpu(0), 0xA000), 0xC0);
        // LS259 bit 4 = 1 ($4684, D7 = 1) switches bank 1 *and* bank 2.
        board.bus_write(BusMaster::Cpu(0), 0x4684, 0x80);
        assert_eq!(board.bank, 1);
        assert_eq!(board.bus_read(BusMaster::Cpu(0), 0xA000), 0xC1);
        // Back to entry 0.
        board.bus_write(BusMaster::Cpu(0), 0x4684, 0x00);
        assert_eq!(board.bus_read(BusMaster::Cpu(0), 0xA000), 0xC0);
    }

    #[test]
    fn esb_rom_manifest_splits_banks_and_interleaves_bank2() {
        use crate::rom_loader::RomSet;

        // Synthetic ESB ROM set: correct sizes, sentinel bytes we can trace.
        let mk = |first: u8, second: u8| {
            let mut v = vec![0u8; 0x4000];
            v[0] = first;
            v[0x2000] = second;
            v
        };
        let f101 = mk(0xA0, 0xB1); // bank1 low/high
        let f102 = mk(0x02, 0x12);
        let f203 = mk(0x03, 0x13);
        let f104 = mk(0x04, 0x14);
        let f105 = mk(0x50, 0x51); // slapstic banks 0,1
        let f106 = mk(0x62, 0x63); // slapstic banks 2,3
        let f111 = vec![0u8; 0x1000];
        let f113 = mk(0x70, 0x71); // sound 0 low/high
        let f112 = mk(0x80, 0x81); // sound 1 low/high
        let f400 = vec![0u8; 0x400];
        let rs = RomSet::from_slices(&[
            ("136031-101.1f", &f101),
            ("136031-102.1jk", &f102),
            ("136031-203.1kl", &f203),
            ("136031-104.1m", &f104),
            ("136031-105.3u", &f105),
            ("136031-106.2u", &f106),
            ("136031-111.1l", &f111),
            ("136031-113.1jk", &f113),
            ("136031-112.1h", &f112),
            ("136031-110.7h", &f400),
            ("136031-109.7j", &f400),
            ("136031-108.7k", &f400),
            ("136031-107.7l", &f400),
        ]);

        // Slapstic image: four contiguous 8 KB banks 105.lo/105.hi/106.lo/106.hi.
        let slap = ESB_SLAPSTIC_ROM.load_skip_checksums(&rs).unwrap();
        assert_eq!(slap.len(), 0x8000);
        assert_eq!(
            [slap[0], slap[0x2000], slap[0x4000], slap[0x6000]],
            [0x50, 0x51, 0x62, 0x63]
        );

        // Bank-2 source concatenates the three 16 KB ROMs; the loader interleaves
        // low halves into entry 0 and high halves into entry 1.
        let src = ESB_BANK2_ROM.load_skip_checksums(&rs).unwrap();
        assert_eq!(src.len(), 0xC000);
        assert_eq!([src[0], src[0x4000], src[0x8000]], [0x02, 0x03, 0x04]);
        assert_eq!([src[0x2000], src[0x6000], src[0xA000]], [0x12, 0x13, 0x14]);
    }

    #[test]
    fn esb_save_state_round_trips_slapstic_and_bank2() {
        let mut sys = StarWarsSystem::new_esb();
        // Mark a RAM byte, drive the slapstic to bank 0, and switch bank 2.
        sys.board.main_map.region_data_mut(MainRegion::Ram)[0x40] = 0x5A;
        sys.board
            .main_map
            .region_data_mut(MainRegion::SlapsticWindow)[0] = 0xB0;
        sys.board.main_map.region_data_mut(MainRegion::Bank2)[0x6000] = 0xC1;
        sys.board.bus_read(BusMaster::Cpu(0), 0x8000); // arm
        sys.board.bus_read(BusMaster::Cpu(0), 0x8080); // direct-select bank 0
        sys.board.bus_write(BusMaster::Cpu(0), 0x4684, 0x80); // bank 2 → entry 1
        assert_eq!(sys.board.slapstic.as_ref().unwrap().current_bank(), 0);

        let data = SaveState::save_state(&sys).expect("save");
        let mut sys2 = StarWarsSystem::new_esb();
        SaveState::load_state(&mut sys2, &data).unwrap();
        sys2.board
            .main_map
            .region_data_mut(MainRegion::SlapsticWindow)[0] = 0xB0;
        sys2.board.main_map.region_data_mut(MainRegion::Bank2)[0x6000] = 0xC1;

        assert_eq!(sys2.board.slapstic.as_ref().unwrap().current_bank(), 0);
        assert_eq!(sys2.board.main_map.region_data(MainRegion::Ram)[0x40], 0x5A);
        // Restored paging: slapstic window presents bank 0, bank 2 presents entry 1.
        assert_eq!(sys2.board.main_map.read_backing(0x8000), 0xB0);
        assert_eq!(sys2.board.main_map.read_backing(0xA000), 0xC1);
    }

    // -- DIP switches ------------------------------------------------------
    //
    // `dip_test_suite!` below covers the Star Wars tables (it builds the
    // machine with `new()`); these cover what is variant-specific.

    #[test]
    fn esb_dip_defaults_and_metadata() {
        let sys = StarWarsSystem::new_esb();
        assert_eq!(sys.dip_bank_value(0), 0xF3, "ESB DSW0 power-on byte");
        assert_eq!(sys.dip_bank_value(1), 0x02, "DSW1 power-on byte");
        crate::assert_dip_banks_valid(sys.dip_banks(), &[0xF3, 0x02]);
        assert_eq!(sys.dip_bank_value(2), 0, "out-of-range bank must read 0");
    }

    /// The two games present different DSW0 tables from the same byte, and the
    /// choice is driven by the board, not by `machine_id` (which a save state
    /// does not restore).
    #[test]
    fn dsw0_table_follows_the_variant() {
        let sw = StarWarsSystem::new();
        let esb = StarWarsSystem::new_esb();
        assert_eq!(sw.dip_banks()[0].options[2].name, "Bonus Shields");
        assert_eq!(esb.dip_banks()[0].options[2].name, "Jedi-Letter Mode");
        // DSW1 is the same bank on both.
        assert_eq!(sw.dip_banks()[1].options[0].name, "Coinage");
        assert_eq!(esb.dip_banks()[1].options[0].name, "Coinage");

        // Shields 0x03 means 9 on Star Wars and 4 on ESB — the same bits, a
        // different meaning, which is the whole reason for two tables.
        assert_eq!(sw.dip_banks()[0].options[0].choices[3].label, "9");
        assert_eq!(esb.dip_banks()[0].options[0].choices[3].label, "4");
    }

    /// Free Play is what makes the headless ESB repro scriptable, so pin that
    /// the option reaches the byte the game reads at $4360.
    #[test]
    fn free_play_reaches_the_dsw1_port() {
        let mut sys = StarWarsSystem::new_esb();
        assert_eq!(sys.board.bus_read(BusMaster::Cpu(0), 0x4360), 0x02);
        sys.set_dip_option(1, 0, 0x00);
        assert_eq!(sys.dip_bank_value(1) & 0x03, 0x00);
        assert_eq!(sys.board.bus_read(BusMaster::Cpu(0), 0x4360), 0x00);
    }
}

// Star Wars power-on bytes — DSW0: 6 shields, Hard, 1 bonus shield, demo
// sounds on, Freeze off. DSW1: 1 coin/1 credit, both mechanisms ×1, no adder.
#[cfg(test)]
crate::dip_test_suite!(StarWarsSystem, &[0x98, 0x02]);
