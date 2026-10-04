//! Atari Major Havoc (1983): two 6502s, the Major Havoc AVG and a quad POKEY.
//!
//! # Schematics
//!
//! | Drawing | Source | Pages |
//! |---|---|---|
//! | `MAJOR HAVOC Main PCB`, SP-252, 2nd printing | `arcarc.xmission.com/PDF_Arcade_Atari_Kee/Major_Havoc/Major_Havoc_SP-252_2nd_Printing.pdf` | 19 pages; sheet 1A contents on p1, 7A vector generator oscillator p13, 10A gamma I/O and audio p19 |
//! | `MAJOR HAVOC Regulator/Audio II PCB`, SP-252 sheet 2A | same | p3 |
//! | `MAJOR HAVOC Main Wiring Diagram`, SP-252 sheet 1B | same | p2 |
//!
//! The audio output is transcribed in
//! [`docs/schematics/majorhavoc-audio-output.md`](../../docs/schematics/majorhavoc-audio-output.md).
//!
//! # The board
//!
//! - **Alpha**, a 6502 at 10 MHz / 4 = 2.5 MHz: the game. 32K of fixed program
//!   ROM, 32K more paged into 0x2000-0x3FFF 8K at a time, 3K of RAM paged
//!   between two banks, the vector generator and its RAM and ROM, color RAM,
//!   and a latch pair to gamma.
//! - **Gamma**, a 6502 at 10 MHz / 8 = 1.25 MHz: sound and I/O. The quad POKEY,
//!   the buttons, the roller, the switches at 8S, a 2804 EEPROM for scores and
//!   settings, and the other end of the latch pair. A write to gamma's latch
//!   pulls gamma's NMI.
//! - **The AVG**, clocked from its own LC oscillator, labeled 12M on sheet 7A.
//! - **Interrupts**: a 5 kHz clock (10 MHz / 16 / 16 / 8) drives an LS161 for
//!   each CPU. Alpha's raises IRQ on its twelfth count and stops until
//!   acknowledged; gamma's follows its counter's bit 3.
//!
//! What is not modeled: the watchdog (the reference emulator leaves it off and
//! nothing here has been checked against the hardware's timeout), the X/Y
//! inversion bits at 0x1600, and the beta processor, which shipping boards do
//! not fit.

use crate::atari_dvg::rasterize_vectors;
use crate::atari_regulator_audio::{C9_MHAVOC, RegulatorAudioII};
use crate::rom_loader::{RomLoadError, RomRegion, RomSet};
use crate::{choice, option, set_bit_active_low};
use phosphor_core::audio::{DcBlocker, SampleRing};
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind, DebugTraceBuffer};
use phosphor_core::core::display::display_settings;
use phosphor_core::core::input::{DrainPolicy, RelativeCounter};
use phosphor_core::core::machine::{
    ActionRole, AnalogAxisKind, AudioSource, DefaultBinding, DipSwitchBank, DipSwitches,
    InputConfigurable, InputControl, InputEvent, InputId, InputKind, MachineCore, MouseControl,
    Nvram, Profilable, Renderable, SaveState, TimingConfig,
};
use phosphor_core::core::watchpoint::DebugAccessSource;
use phosphor_core::core::{AccessKind, AddressSpace16, Bus, BusMaster, ClockTree, DomainId};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::device::avg::{Avg, AvgVariant, VectorMemory};
use phosphor_core::device::dvg::{VectorLine, raster_size_for_field};
use phosphor_core::device::pokey::PokeyLoad;
use phosphor_core::device::quad_pokey::QuadPokey;
use phosphor_core::device::tms5220::{Tms52xxVariant, Tms5220};
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion, Saveable};

// ---------------------------------------------------------------------------
// ROM sets
// ---------------------------------------------------------------------------

/// Where one Major Havoc ROM set's chips go.
pub struct MhavocConfig {
    /// The machine id, shared by every revision of one machine.
    id: &'static str,
    /// The MAME set name, reported by `MachineCore::revision`.
    set: &'static str,
    /// Alpha's fixed program ROM, 0x8000-0xFFFF.
    alpha: &'static RomRegion,
    /// Alpha's paged program ROM, four 8K pages at 0x2000-0x3FFF.
    alpha_paged: &'static RomRegion,
    /// Alpha's vector ROM: the first 4K at 0x5000, the second at 0x6000 and
    /// mirrored at 0x7000.
    vector: &'static RomRegion,
    /// The AVG's own paged vector ROM, four 8K pages the generator alone sees.
    avg_paged: &'static RomRegion,
    /// Gamma's program ROM, 16K at 0x8000, mirrored at 0xC000 for the vectors.
    gamma: &'static RomRegion,
    /// DSW1 and DSW2; the prototype lays DSW1 out differently.
    dip_banks: &'static [DipSwitchBank],
    /// Return to Vax fits the TMS5220 that production boards leave off.
    speech: bool,
}

/// The AVG state PROM at 6C, common to every set.
static AVG_PROM: RomRegion = RomRegion {
    size: 0x100,
    entries: &[rom!("136002-125.6c", 0x100, 0, 0x5903af03)],
};

static AVG_PAGED: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        rom!("136025.106", 0x4000, 0x0000, 0x2ca83c76),
        rom!("136025.107", 0x4000, 0x4000, 0x5f81c5f3),
    ],
};
static GAMMA: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[rom!("136025.108", 0x4000, 0, 0x93faf210)],
};

static MHAVOC_ALPHA: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        rom!("136025.216", 0x4000, 0x0000, 0x522a9cc0),
        rom!("136025.217", 0x4000, 0x4000, 0xea3d6877),
    ],
};
static MHAVOC_PAGED: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        rom!("136025.215", 0x4000, 0x0000, 0xa4d380ca),
        rom!("136025.318", 0x4000, 0x4000, 0xba935067),
    ],
};
static MHAVOC_VECTOR: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[rom!("136025.210", 0x2000, 0, 0xc67284ca)],
};

static MHAVOC2_ALPHA: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        rom!("136025.103", 0x4000, 0x0000, 0xbf192284),
        rom!("136025.104", 0x4000, 0x4000, 0x833c5d4e),
    ],
};
static MHAVOC2_PAGED: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        rom!("136025.101", 0x4000, 0x0000, 0x2b3b591f),
        rom!("136025.109", 0x4000, 0x4000, 0x4d766827),
    ],
};
static MHAVOC2_VECTOR: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[rom!("136025.110", 0x2000, 0, 0x16eef583)],
};

static MHAVOCRV_ALPHA: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        rom!("136025.916", 0x4000, 0x0000, 0x1255bd7f),
        rom!("136025.917", 0x4000, 0x4000, 0x21889079),
    ],
};
static MHAVOCRV_PAGED: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        rom!("136025.915", 0x4000, 0x0000, 0x4c7235dc),
        rom!("136025.918", 0x4000, 0x4000, 0x84735445),
    ],
};
static MHAVOCRV_AVG_PAGED: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        rom!("136025.106", 0x4000, 0x0000, 0x2ca83c76),
        rom!("136025.907", 0x4000, 0x4000, 0x4deea2c9),
    ],
};
static MHAVOCRV_GAMMA: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[rom!("136025.908", 0x4000, 0, 0xc52ec664)],
};

static MHAVOCP_ALPHA: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        rom!("136025.016", 0x4000, 0x0000, 0x94caf6c0),
        rom!("136025.017", 0x4000, 0x4000, 0x05cba70a),
    ],
};
static MHAVOCP_PAGED: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        rom!("136025.015", 0x4000, 0x0000, 0xc567c11b),
        rom!("136025.018", 0x4000, 0x4000, 0xa8c35ccd),
    ],
};
static MHAVOCP_VECTOR: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[rom!("136025.010", 0x2000, 0, 0x3050c0e6)],
};
static MHAVOCP_AVG_PAGED: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        rom!("136025.006", 0x4000, 0x0000, 0xe272ed41),
        rom!("136025.007", 0x4000, 0x4000, 0xe152c9d8),
    ],
};
static MHAVOCP_GAMMA: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[rom!("136025.008", 0x4000, 0, 0x22ea7399)],
};

/// Major Havoc, revision 3.
pub static MHAVOC: MhavocConfig = MhavocConfig {
    id: "majorhavoc",
    set: "mhavoc",
    alpha: &MHAVOC_ALPHA,
    alpha_paged: &MHAVOC_PAGED,
    vector: &MHAVOC_VECTOR,
    avg_paged: &AVG_PAGED,
    gamma: &GAMMA,
    dip_banks: &[DSW1, DSW2],
    speech: false,
};
/// Major Havoc, revision 2.
pub static MHAVOC2: MhavocConfig = MhavocConfig {
    id: "majorhavoc",
    set: "mhavoc2",
    alpha: &MHAVOC2_ALPHA,
    alpha_paged: &MHAVOC2_PAGED,
    vector: &MHAVOC2_VECTOR,
    avg_paged: &AVG_PAGED,
    gamma: &GAMMA,
    dip_banks: &[DSW1, DSW2],
    speech: false,
};
/// Major Havoc: Return to Vax, a later hack that adds levels and speech.
pub static MHAVOCRV: MhavocConfig = MhavocConfig {
    id: "majorhavocreturntovax",
    set: "mhavocrv",
    alpha: &MHAVOCRV_ALPHA,
    alpha_paged: &MHAVOCRV_PAGED,
    vector: &MHAVOC_VECTOR,
    avg_paged: &MHAVOCRV_AVG_PAGED,
    gamma: &MHAVOCRV_GAMMA,
    dip_banks: &[DSW1, DSW2],
    speech: true,
};
/// Major Havoc, prototype.
pub static MHAVOCP: MhavocConfig = MhavocConfig {
    id: "majorhavoc",
    set: "mhavocp",
    alpha: &MHAVOCP_ALPHA,
    alpha_paged: &MHAVOCP_PAGED,
    vector: &MHAVOCP_VECTOR,
    avg_paged: &MHAVOCP_AVG_PAGED,
    gamma: &MHAVOCP_GAMMA,
    dip_banks: &[DSW1_PROTOTYPE, DSW2],
    speech: false,
};

// ---------------------------------------------------------------------------
// Timing and clocks
// ---------------------------------------------------------------------------

/// The 10 MHz crystal both CPUs and the POKEYs divide from.
const CPU_CRYSTAL_HZ: u32 = 10_000_000;
/// The vector generator's oscillator, sheet 7A: an LC oscillator whose output
/// is labeled `12M`. Its parts as printed (L2 100 uH, C19 100 pF, C20 39 pF)
/// do not come to 12 MHz, and the box is cropped at the edge of the scan, so
/// the label is what is taken. It is not a crystal and was presumably trimmed.
const AVG_OSCILLATOR_HZ: u32 = 12_000_000;

/// Alpha at 2.5 MHz, one frame per 1/60 s.
///
/// The hardware has no frame rate: a vector monitor redraws whenever the game
/// finishes a picture, and this game's rate varies with what is on screen.
/// Over 30 s of attract mode the gap between pictures was most often 8 of
/// alpha's interrupts (6,144 cycles each, about 51 Hz) but 11 or 12 nearly as
/// often (about 34 Hz), and anywhere from 4 to 21. So the frame is purely how
/// often the latest picture is presented (see `MhavocBoard::present`), and
/// 60 Hz is the host display's rate; the unevenness left is the game's own.
const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 2_500_000,
    cycles_per_scanline: 41_667,
    total_scanlines: 1,
    display_width: 300,
    display_height: 260,
    display_aspect: Some((4, 3)),
};

/// The board's two oscillators and everything divided from them.
pub fn clock_tree() -> ClockTree {
    use phosphor_core::core::{ClockDomainName as Clk, RootId};
    let mut t = ClockTree::new(CPU_CRYSTAL_HZ);
    let alpha = t.add_domain(Clk::Cpu, RootId::MAIN, 1, 4);
    t.add_domain(Clk::SoundCpu, RootId::MAIN, 1, 8);
    t.add_domain(Clk::Pokey, RootId::MAIN, 1, 8);
    // Return to Vax's TMS5220: MAME's 10 MHz / 2 / 9.
    t.add_domain(Clk::Speech, RootId::MAIN, 1, 18);
    let avg_root = t.add_root(AVG_OSCILLATOR_HZ);
    t.add_domain(Clk::Vector, avg_root, 1, 1);
    t.set_step_domain(alpha);
    t
}

/// Alpha cycles per tick of the 5 kHz interrupt clock: 10 MHz / 16 / 16 / 8
/// against 10 MHz / 4.
const IRQ_CLOCK_ALPHA_CYCLES: u64 = 512;

/// A completed vector pass this long or shorter that draws nothing is the
/// game's beam-parking run, not a picture. The park list is 6 vectors in
/// every phase probed across all four ROMs (tens of thousands of passes,
/// zero variance); the smallest lit completion seen is 218 vectors, so 16
/// sits well clear of both.
const PARK_PASS_MAX_VECTORS: usize = 16;

// ---------------------------------------------------------------------------
// Address maps
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
enum AlphaRegion {
    Ram = 1,
    PagedRam = 2,
    Io = 3,
    BetaRam = 4,
    PagedRom = 5,
    VectorRam = 6,
    VectorRom = 7,
    ProgramRom = 8,
    Ram0800 = 9,
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
enum GammaRegion {
    Ram = 1,
    Io = 2,
    Eeprom = 3,
    ProgramRom = 4,
}

/// Size of one page of alpha's paged RAM, which appears at 0x0200 and again at
/// 0x0A00.
const RAM_PAGE: u32 = 0x600;

fn alpha_map() -> AddressSpace16 {
    let mut map = AddressSpace16::new();
    // Both pages of the paged RAM in one region, so the save state carries
    // both; the windows are pointed at one page or the other.
    map.region(
        AlphaRegion::PagedRam,
        "Paged RAM",
        0x0200,
        2 * RAM_PAGE,
        AccessKind::ReadWrite,
    )
    .region(
        AlphaRegion::Ram,
        "RAM",
        0x0000,
        0x0200,
        AccessKind::ReadWrite,
    )
    .region(AlphaRegion::Io, "I/O", 0x1000, 0x0800, AccessKind::Io)
    .region(
        AlphaRegion::BetaRam,
        "Beta RAM",
        0x1800,
        0x0800,
        AccessKind::ReadWrite,
    )
    .backing_region(AlphaRegion::PagedRom, "Paged Program ROM", 0x8000)
    .region(
        AlphaRegion::VectorRam,
        "Vector RAM",
        0x4000,
        0x1000,
        AccessKind::ReadWrite,
    )
    .region(
        AlphaRegion::VectorRom,
        "Vector ROM",
        0x5000,
        0x2000,
        AccessKind::ReadOnly,
    )
    .mirror(0x7000, 0x6000, 0x1000)
    .region(
        AlphaRegion::ProgramRom,
        "Program ROM",
        0x8000,
        0x8000,
        AccessKind::ReadOnly,
    )
    // 0x0800-0x09FF is plain RAM between the two windows onto the paged
    // RAM, laid over the pages the paged region's declaration covered.
    .region(
        AlphaRegion::Ram0800,
        "RAM 0800",
        0x0800,
        0x0200,
        AccessKind::ReadWrite,
    );
    map
}

/// Point both windows onto alpha's paged RAM at page `page`, 0 or 1.
fn select_ram_page(map: &mut AddressSpace16, page: u8) {
    let base = u32::from(page & 1) * RAM_PAGE;
    map.remap_pages(0x02, 6, AlphaRegion::PagedRam, base);
    map.remap_pages(0x0A, 6, AlphaRegion::PagedRam, base);
}

/// Point 0x2000-0x3FFF at page `page`, 0 to 3, of the paged program ROM.
fn select_rom_page(map: &mut AddressSpace16, page: u8) {
    map.remap_pages(
        0x20,
        0x20,
        AlphaRegion::PagedRom,
        u32::from(page & 3) * 0x2000,
    );
}

// ---------------------------------------------------------------------------
// The board
// ---------------------------------------------------------------------------

/// Gamma's side of the board: its address space, the quad POKEY, and the
/// inputs only it reads.
#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct GammaSide {
    /// Gamma's RAM, EEPROM and ROM.
    #[debug_map(cpu = 1)]
    #[save(id = 1)]
    map: AddressSpace16,
    /// The quad POKEY at 13Q. OUT1-3 share one node and OUT4 has its own; see
    /// [`MhavocBoard::mix_audio`].
    #[save(id = 2)]
    quad: QuadPokey,
    /// Return to Vax's TMS5220; idle on the other sets.
    ///
    /// Silent in practice, and correctly so. Over a minute of attract mode and
    /// a recorded game, gamma sent the chip one 0x60 (Speak External) and
    /// thereafter only 0xFF and 0x00, energy codes that stop or mute it; the
    /// reference emulator's gamma sent the same bytes in the same proportions
    /// (28,809 0xFF, 17 0x00, one 0x60 in a minute), and plays nothing either.
    /// Whatever events make this set speak, neither run reached them.
    #[save(id = 3)]
    tms: Tms5220,
    /// The byte gamma last latched for the speech chip at 0x5800.
    #[save(id = 4)]
    speech_latch: u8,
    /// Buttons at 0x2800 bits 4-7, active low.
    #[save(id = 5)]
    buttons: u8,
    /// The roller, read at 0x3800.
    #[save(id = 6)]
    roller: RelativeCounter,
    /// DSW1 at 13/14S, read through chip 0's ALLPOT, and DSW2 at 8S, read at
    /// 0x4000. Board configuration, so not saved.
    #[save_skip]
    dsw1: u8,
    #[save_skip]
    dsw2: u8,
}

/// Everything both CPUs talk to. Alpha's bus is this struct; gamma's is
/// [`GammaView`] over it, because the latch pair between them is shared.
#[derive(BusDebug, DebugTrace, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct MhavocBoard {
    #[debug_map(cpu = 0)]
    #[save(id = 1)]
    map: AddressSpace16,
    #[debug_device("AVG")]
    #[save(id = 2)]
    avg: Avg,
    /// Color RAM at 0x1400-0x141F: 32 entries, the second half used by
    /// sparkle.
    #[save(id = 3)]
    color_ram: [u8; 32],
    /// The AVG's own paged vector ROM. ROM, so not saved.
    #[save_skip]
    avg_rom: Vec<u8>,
    #[save(id = 4)]
    ram_page: u8,
    #[save(id = 5)]
    rom_page: u8,

    // The latch pair. `alpha_data` is what alpha wrote for gamma; `gamma_data`
    // what gamma wrote for alpha. Each side's flags are what the other reads.
    #[save(id = 6)]
    alpha_data: u8,
    #[save(id = 7)]
    alpha_rcvd: bool,
    #[save(id = 8)]
    alpha_xmtd: bool,
    #[save(id = 9)]
    gamma_data: u8,
    #[save(id = 10)]
    gamma_rcvd: bool,
    #[save(id = 11)]
    gamma_xmtd: bool,
    /// Alpha's write to gamma's latch pulls gamma's NMI; this carries the
    /// pulse until gamma next samples its interrupt lines.
    #[save(id = 12)]
    gamma_nmi: bool,

    /// Alpha's 0x1600 bit 5: which input port 0x1200 bits 6-7 read.
    #[save(id = 13)]
    player_1: bool,
    /// Alpha's 0x1600 bit 3, low: gamma held in reset.
    #[save(id = 14)]
    gamma_reset_held: bool,
    /// Set when alpha releases gamma's reset, so the machine resets gamma's
    /// CPU on its next cycle.
    #[save(id = 15)]
    gamma_reset_pending: bool,

    // The 5 kHz interrupt clock and the two LS161s it drives.
    #[save(id = 16)]
    irq_clock_phase: u64,
    #[save(id = 17)]
    alpha_irq_clock: u8,
    #[save(id = 18)]
    alpha_irq_clock_enable: bool,
    #[save(id = 19)]
    alpha_irq: bool,
    #[save(id = 20)]
    gamma_irq_clock: u8,

    /// Alpha cycles since power-on; bit 9 is the 2.4 kHz input at 0x1200.
    #[save(id = 21)]
    clock: u64,
    /// Coins at 0x1200 bits 6-7 while `player_1` is clear, active low.
    #[save(id = 22)]
    coins: u8,
    /// Test and the credit option at 0x1200 bits 6-7 while `player_1` is set.
    #[save(id = 23)]
    service: u8,
    /// Aux coin and diagnostic step, 0x1200 bits 5 and 4, active low.
    #[save(id = 24)]
    in0_switches: u8,
    /// The address alpha last put on its bus: A0-A2 seed the sparkle LFSR.
    #[save_skip]
    last_addr: u16,

    #[debug_bus]
    #[save(id = 25)]
    gamma: GammaSide,

    /// What the tube shows: the latest pass over the vector list that drew
    /// anything. Redrawn after a load rather than restored.
    ///
    /// The game runs two lists alternately: the picture, about 3,100 vectors
    /// every 49,400 alpha cycles, and a few thousand cycles later a pass of six
    /// that draws nothing visible and parks the beam. Showing whichever pass
    /// finished last blanked the screen on every parking pass; showing every
    /// pass that finished in the frame drew two pictures on top of each other
    /// whenever two fell in one frame, which flashed. The latest picture is
    /// neither.
    #[save_skip(default)]
    display_list: Vec<VectorLine>,
    /// The latest accepted pass, waiting for the frame's end.
    #[save_skip(default)]
    latest_picture: Vec<VectorLine>,

    // Audio: see `mix_audio`.
    #[save(id = 26)]
    node_a_coupling: DcBlocker,
    #[save(id = 27)]
    node_a_t: DcBlocker,
    #[save(id = 28)]
    node_a_feedback: DcBlocker,
    #[save(id = 29)]
    node_b_coupling: DcBlocker,
    #[save(id = 30)]
    node_b_t: DcBlocker,
    #[save(id = 31)]
    node_b_feedback: DcBlocker,
    #[save(id = 32)]
    output_filter: DcBlocker,
    #[save(id = 33)]
    amp: RegulatorAudioII,
    #[save_skip(default = SampleRing::with_capacity(2048))]
    audio_buffer: SampleRing<i16>,
    #[save_skip]
    speech: bool,

    #[debug_events]
    #[save_skip]
    debug_trace: DebugTraceBuffer,
}

/// A one-pole low-pass as the input less a one-pole high-pass at the same
/// corner, which is the low-pass exactly.
fn low_pass(stage: &mut DcBlocker, x: f32) -> f32 {
    x - stage.process(x)
}

fn corner(ohms: f64, farads: f64) -> f32 {
    (1.0 / (std::f64::consts::TAU * ohms * farads)) as f32
}

/// Each quad POKEY node is a zero-ohm virtual ground at +5V AUD.
const NODE_LOAD: PokeyLoad = PokeyLoad::VirtualGround {
    series_ohms: 0.0,
    reference_v: 5.0,
};

/// Weight of the three-chip node against the lone chip's at the mixer: R145
/// 10k over R144 22k, against R145 over R139 10k.
const NODE_B_WEIGHT: f32 = 10.0 / 22.0;

/// The TMS5220 joins node B's second stage through a resistor the drawing
/// marks with an asterisk and production boards leave out, so its weight is
/// on no sheet. Return to Vax is a later hack; this is a level for it, not a
/// reading.
const SPEECH_WEIGHT: f32 = 0.5;

impl MhavocBoard {
    fn new(config: &MhavocConfig) -> Self {
        let rate = phosphor_core::audio::host_sample_rate();
        let mut map = alpha_map();
        select_ram_page(&mut map, 0);
        select_rom_page(&mut map, 0);
        let mut quad = QuadPokey::with_clock(CPU_CRYSTAL_HZ / 8, rate);
        // OUT1-OUT3 (chips 0-2) tied onto node 0, OUT4 (chip 3) alone on 1.
        quad.set_nodes([0, 0, 0, 1]);
        quad.set_node_load(0, NODE_LOAD);
        quad.set_node_load(1, NODE_LOAD);
        let mut gamma_map = gamma_map();
        gamma_map.region_data_mut(GammaRegion::Eeprom).fill(0xFF);
        Self {
            map,
            avg: Avg::with_variant(
                AvgVariant::MajorHavoc,
                TIMING.display_width as i32,
                TIMING.display_height as i32,
            ),
            color_ram: [0; 32],
            avg_rom: vec![0; 0x8000],
            ram_page: 0,
            rom_page: 0,
            alpha_data: 0,
            alpha_rcvd: false,
            alpha_xmtd: false,
            gamma_data: 0,
            gamma_rcvd: false,
            gamma_xmtd: false,
            gamma_nmi: false,
            player_1: false,
            gamma_reset_held: false,
            gamma_reset_pending: false,
            irq_clock_phase: 0,
            alpha_irq_clock: 0,
            alpha_irq_clock_enable: true,
            alpha_irq: false,
            gamma_irq_clock: 0,
            clock: 0,
            coins: 0x03,
            service: 0x03,
            in0_switches: 0x30,
            last_addr: 0,
            gamma: GammaSide {
                map: gamma_map,
                quad,
                tms: Tms5220::with_variant(Tms52xxVariant::Tms5220, CPU_CRYSTAL_HZ / 18),
                speech_latch: 0,
                buttons: 0xF0,
                roller: new_roller(),
                dsw1: 0x00,
                dsw2: 0xFF,
            },
            display_list: Vec::with_capacity(4096),
            latest_picture: Vec::with_capacity(4096),
            node_a_coupling: DcBlocker::with_cutoff(corner(39_000.0, 0.22e-6), rate),
            // T network: C67 against R136 and R137 in parallel.
            node_a_t: DcBlocker::with_cutoff(corner(39_000.0 * 18_000.0 / 57_000.0, 0.01e-6), rate),
            node_a_feedback: DcBlocker::with_cutoff(corner(39_000.0, 0.01e-6), rate),
            node_b_coupling: DcBlocker::with_cutoff(corner(39_000.0, 0.22e-6), rate),
            node_b_t: DcBlocker::with_cutoff(
                corner(39_000.0 * 18_000.0 / 57_000.0, 0.001e-6),
                rate,
            ),
            node_b_feedback: DcBlocker::with_cutoff(corner(39_000.0, 0.001e-6), rate),
            // R148 2.2k into C92 0.01 uF, loaded by the amplifier's input
            // divider (R14 10k and R27 1k, 11k to ground).
            output_filter: DcBlocker::with_cutoff(
                corner(2_200.0 * 11_000.0 / 13_200.0, 0.01e-6),
                rate,
            ),
            amp: RegulatorAudioII::new(C9_MHAVOC, rate),
            audio_buffer: SampleRing::with_capacity(2048),
            speech: config.speech,
            debug_trace: DebugTraceBuffer::new(),
        }
    }

    /// Alpha's input port at 0x1200.
    fn in0(&self) -> u8 {
        let mut v = self.in0_switches & 0x30;
        v |= u8::from(self.avg.is_halted());
        // 2.4 kHz (sheet 4B: 625 kHz / 256): bit 9 of alpha's cycle
        // count, high for the first half.
        v |= u8::from(self.clock & 0x200 == 0) << 1;
        v |= u8::from(self.gamma_xmtd) << 2;
        v |= u8::from(self.gamma_rcvd) << 3;
        let upper = if self.player_1 {
            self.service
        } else {
            self.coins
        };
        v | ((upper & 0x03) << 6)
    }

    /// Alpha's control latch at 0x1600.
    fn out_0(&mut self, data: u8) {
        self.player_1 = data & 0x20 != 0;
        let held = data & 0x08 == 0;
        if held {
            // Holding gamma in reset clears both sides' flags and a queued
            // NMI with them (sheet 9B: RESETγ holds the flag flops' CLR).
            self.alpha_rcvd = false;
            self.alpha_xmtd = false;
            self.gamma_rcvd = false;
            self.gamma_xmtd = false;
            self.gamma_nmi = false;
        } else if self.gamma_reset_held {
            self.gamma_reset_pending = true;
        }
        self.gamma_reset_held = held;
    }

    /// Clock the 5 kHz interrupt clock if its period has come round.
    fn clock_interrupts(&mut self) {
        self.irq_clock_phase += 1;
        if self.irq_clock_phase < IRQ_CLOCK_ALPHA_CYCLES {
            return;
        }
        self.irq_clock_phase = 0;
        // Alpha's LS161 counts while enabled and raises IRQ at 12, then stops
        // until the acknowledge at 0x1700 clears and restarts it.
        if self.alpha_irq_clock_enable {
            self.alpha_irq_clock = self.alpha_irq_clock.wrapping_add(1);
            if self.alpha_irq_clock & 0x0C == 0x0C {
                self.alpha_irq = true;
                self.alpha_irq_clock_enable = false;
            }
        }
        // Gamma's follows bit 3 of its count.
        self.gamma_irq_clock = self.gamma_irq_clock.wrapping_add(1);
    }

    /// Run the vector generator for `cycles` of its own clock.
    fn step_avg(&mut self, cycles: u32) {
        if cycles == 0 {
            return;
        }
        self.avg.set_host_address(self.last_addr);
        let mem = VectorMemory::split(
            self.map.region_data(AlphaRegion::VectorRam),
            &self.map.region_data(AlphaRegion::VectorRom)[..0x1000],
            0x1000,
        )
        .with_banked(&self.avg_rom);
        if self.avg.step(cycles, &mem, &self.color_ram) {
            let pass = self.avg.take_display_list();
            self.file_pass(pass);
        }
    }

    /// File a completed vector pass: anything lit is the new picture, and
    /// so is a dark pass longer than the parking pass, which means the
    /// picture itself went dark. Short dark passes are the game's
    /// beam-parking runs and are dropped so they never blank a picture.
    fn file_pass(&mut self, pass: Vec<VectorLine>) {
        let lit = pass.iter().any(|l| l.intensity > 0);
        if lit || pass.len() > PARK_PASS_MAX_VECTORS {
            self.latest_picture = pass;
        }
    }

    /// Drain the quad POKEY's two nodes (and the speech chip) and carry them to
    /// the speaker, as sheet 10A does.
    ///
    /// Each node is a zero-ohm virtual ground, so its output is the current
    /// its chips sink; one chip with every device on is 1.0. Both second
    /// stages have the same passband gain, -0.684, which is a scale. Node A
    /// (OUT4) is low-passed by its T network and feedback at about 1.3 kHz and
    /// 408 Hz, node B (OUT1-3) at about 12.9 kHz and 4.08 kHz, and the mixer
    /// weights them 1 : 10/22. Every device on all four chips is scaled to 1.0.
    fn mix_audio(&mut self) {
        let a = self.gamma.quad.drain_node(1);
        let b = self.gamma.quad.drain_node(0);
        let speech = if self.speech {
            self.gamma.tms.drain_audio()
        } else {
            Vec::new()
        };
        let full = NODE_LOAD.full_scale_of_chips(1) as f32;
        let scale = 1.0 / (1.0 + 3.0 * NODE_B_WEIGHT);
        let len = a.len().min(b.len());
        for i in 0..len {
            let va = self.node_a_coupling.process(a[i] / full);
            let va = low_pass(&mut self.node_a_t, va);
            let va = low_pass(&mut self.node_a_feedback, va);
            let vb = self.node_b_coupling.process(b[i] / full);
            let vb = low_pass(&mut self.node_b_t, vb);
            let mut vb = low_pass(&mut self.node_b_feedback, vb);
            if let Some(s) = speech.get(i) {
                vb += SPEECH_WEIGHT * s;
            }
            let mixed = (va + NODE_B_WEIGHT * vb) * scale;
            let out = self.amp.process(low_pass(&mut self.output_filter, mixed));
            self.audio_buffer
                .push((out * 32767.0).clamp(i16::MIN as f32, i16::MAX as f32) as i16);
        }
    }

    /// End a presentation frame: show the latest pass the generator
    /// finished, if it finished one during the frame, or keep the last one.
    /// The parking pass never displaces a picture, but a picture that goes
    /// dark does reach the screen.
    fn present(&mut self) {
        if !self.latest_picture.is_empty() {
            std::mem::swap(&mut self.display_list, &mut self.latest_picture);
            self.latest_picture.clear();
        }
    }

    fn render(&self, buffer: &mut [u8]) {
        let field = TIMING.display_size();
        let (rw, rh) = raster_size_for_field(field.0, field.1);
        rasterize_vectors(
            &self.display_list,
            buffer,
            rw,
            rh,
            field,
            true,
            &display_settings().without_halation(),
        );
    }

    fn trace_write(&mut self, detail: &'static str) {
        if self.debug_trace.enabled() {
            self.debug_trace.record(DebugEvent {
                cpu_index: Some(0),
                pc: self.map.latched_pc(),
                device: Some("AVG"),
                detail: Some(detail),
                ..DebugEvent::new(
                    self.clock,
                    DebugAccessSource::Cpu(0),
                    DebugEventKind::DeviceWrite,
                )
            });
        }
    }
}

fn new_roller() -> RelativeCounter {
    // An 8-bit counter read backwards, as the reference reads it.
    RelativeCounter::new(0xFF, 4, true, DrainPolicy::ClampCarry { max_step: 32 })
}

// ---------------------------------------------------------------------------
// Alpha's bus
// ---------------------------------------------------------------------------

impl Bus for MhavocBoard {
    type Address = u16;
    type Data = u8;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        self.last_addr = addr;
        let data = match self.map.page(addr).region_id {
            AlphaRegion::IO => match addr {
                0x1000 => {
                    // Reading gamma's latch.
                    self.alpha_rcvd = true;
                    self.gamma_xmtd = false;
                    self.gamma_data
                }
                0x1200 => self.in0(),
                0x1400..=0x141F => self.color_ram[usize::from(addr & 0x1F)],
                _ => 0,
            },
            id if id == AlphaRegion::PagedRom.into() => self.map.read_backing(addr),
            phosphor_core::core::UNMAPPED => 0,
            _ => self.map.read_backing(addr),
        };
        self.map.watch_read(0, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.last_addr = addr;
        self.map.watch_write(0, master, addr, data);
        let region = self.map.page(addr).region_id;
        if region == AlphaRegion::IO {
            match addr {
                0x1400..=0x141F => self.color_ram[usize::from(addr & 0x1F)] = data,
                0x1600 => self.out_0(data),
                0x1640 => {
                    self.trace_write("vector generator start");
                    self.avg.go();
                    self.avg.take_display_list();
                }
                0x1680 => {} // watchdog: not modeled
                0x16C0 => {
                    self.trace_write("vector generator reset");
                    self.avg.reset();
                }
                0x1700 => {
                    self.alpha_irq = false;
                    self.alpha_irq_clock = 0;
                    self.alpha_irq_clock_enable = true;
                }
                0x1740 => {
                    self.rom_page = data & 3;
                    select_rom_page(&mut self.map, self.rom_page);
                }
                0x1780 => {
                    self.ram_page = data & 1;
                    select_ram_page(&mut self.map, self.ram_page);
                }
                0x17C0 => {
                    // Writing gamma's latch, with an NMI to say so. While
                    // gamma is held in reset the data still latches (the
                    // LS374 has no clear) but the flag flops cannot set, so
                    // no flags and no NMI.
                    self.alpha_data = data;
                    if !self.gamma_reset_held {
                        self.gamma_rcvd = false;
                        self.alpha_xmtd = true;
                        self.gamma_nmi = true;
                    }
                }
                _ => {}
            }
        } else if region == AlphaRegion::RAM
            || region == AlphaRegion::PAGED_RAM
            || region == AlphaRegion::RAM0800
            || region == AlphaRegion::BETA_RAM
            || region == AlphaRegion::VECTOR_RAM
        {
            self.map.write_backing(addr, data);
        }
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            irq: self.alpha_irq,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Gamma's bus
// ---------------------------------------------------------------------------

/// Gamma's view of the board.
pub struct GammaView<'a>(&'a mut MhavocBoard);

impl GammaView<'_> {
    /// The quad POKEY's decode: A0-A2 are the register's low bits, A3-A4 the
    /// chip, and A5 the register's bit 3. Chip `n` is the LS139 at 11Q's `Yn`,
    /// `CS(n+1)`.
    fn quad_decode(offset: u16) -> (usize, u16) {
        let chip = usize::from((offset >> 3) & 3);
        let reg = (offset & 7) | ((offset & 0x20) >> 2);
        (chip, reg)
    }
}

impl Bus for GammaView<'_> {
    type Address = u16;
    type Data = u8;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let b = &mut *self.0;
        let g = &mut b.gamma;
        let data = match g.map.page(addr).region_id {
            GammaRegion::IO => match addr & 0xF800 {
                0x2000 => {
                    let (chip, reg) = Self::quad_decode(addr & 0x3F);
                    if chip == 0 && reg == 8 {
                        // Chip 0's ALLPOT reads DSW1 at 13/14S.
                        g.dsw1
                    } else {
                        g.quad.chip_mut(chip).read(reg)
                    }
                }
                0x2800 => {
                    let mut v = g.buttons & 0xF0;
                    v |= u8::from(b.alpha_xmtd);
                    v |= u8::from(b.alpha_rcvd) << 1;
                    // Speech ready (active low) where the chip is fitted,
                    // otherwise floating high; bit 3 is not connected.
                    let not_ready = !b.speech || !g.tms.ready();
                    v |= u8::from(not_ready) << 2;
                    v | 0x08
                }
                0x3000 => {
                    // Reading alpha's latch.
                    b.gamma_rcvd = true;
                    b.alpha_xmtd = false;
                    b.alpha_data
                }
                0x3800 => g.roller.counter(),
                0x4000 => g.dsw2,
                _ => 0xFF,
            },
            phosphor_core::core::UNMAPPED => 0,
            _ => g.map.read_backing(addr),
        };
        g.map.watch_read(1, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        let b = &mut *self.0;
        let g = &mut b.gamma;
        g.map.watch_write(1, master, addr, data);
        let region = g.map.page(addr).region_id;
        if region == GammaRegion::IO {
            match addr & 0xF800 {
                0x2000 => {
                    let (chip, reg) = Self::quad_decode(addr & 0x3F);
                    g.quad.chip_mut(chip).write(reg, data);
                }
                0x4000 => {
                    // IRQ acknowledge: clears the line and the count.
                    b.gamma_irq_clock = 0;
                }
                0x4800 => {} // coin counters
                0x5000 => {
                    // Writing alpha's latch.
                    b.alpha_rcvd = false;
                    b.gamma_xmtd = true;
                    b.gamma_data = data;
                }
                0x5800 if b.speech => {
                    if addr & 0x0100 == 0 {
                        g.speech_latch = data;
                    } else {
                        g.tms.data_w(g.speech_latch);
                    }
                }
                _ => {}
            }
        } else if region == GammaRegion::RAM || region == GammaRegion::EEPROM {
            g.map.write_backing(addr, data);
        }
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        let b = &mut *self.0;
        let nmi = b.gamma_nmi;
        b.gamma_nmi = false;
        InterruptState {
            irq: b.gamma_irq_clock & 0x08 != 0,
            nmi,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// The machine
// ---------------------------------------------------------------------------

/// Atari Major Havoc and its variants.
#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct MhavocSystem {
    #[debug_cpu("M6502 Alpha")]
    #[save(id = 1)]
    alpha: M6502,
    #[debug_cpu("M6502 Gamma")]
    #[save(id = 2)]
    gamma: M6502,
    #[debug_bus]
    #[save(id = 3)]
    board: MhavocBoard,
    /// The clock tree, stepped in alpha cycles.
    #[save(id = 4)]
    clocks: ClockTree,
    #[save_skip]
    gamma_dom: DomainId,
    #[save_skip]
    pokey_dom: DomainId,
    #[save_skip]
    speech_dom: DomainId,
    #[save_skip]
    avg_dom: DomainId,
    #[save_skip]
    config: &'static MhavocConfig,
    /// MAME set name this machine loaded (e.g. "mhavocp"), reported by
    /// `MachineCore::revision`. Loading a save restores state, never ROMs,
    /// so this keeps its value across loads.
    #[save_skip]
    loaded_revision: &'static str,
}

impl MhavocSystem {
    pub fn new() -> Self {
        use phosphor_core::core::ClockDomainName as Clk;
        let clocks = clock_tree();
        let find = |name| clocks.find(name).expect("declared domain");
        let (gamma_dom, pokey_dom, speech_dom, avg_dom) = (
            find(Clk::SoundCpu),
            find(Clk::Pokey),
            find(Clk::Speech),
            find(Clk::Vector),
        );
        Self {
            alpha: M6502::new(),
            gamma: M6502::new(),
            board: MhavocBoard::new(&MHAVOC),
            clocks,
            gamma_dom,
            pokey_dom,
            speech_dom,
            avg_dom,
            config: &MHAVOC,
            loaded_revision: "",
        }
    }
}

impl Default for MhavocSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl MhavocSystem {
    /// A system fixed to `config`: the single-revision path, which leaves
    /// the revision unreported. `new` plus [`load_roms`](Self::load_roms) is
    /// the multi-revision path, which reports the loaded set.
    pub fn with_config(config: &'static MhavocConfig) -> Self {
        let mut sys = Self::new();
        sys.config = config;
        sys.board.speech = config.speech;
        sys
    }

    /// Load with the preset config, leaving the revision unreported: the
    /// single-revision path.
    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        self.load_regions(rom_set)
    }

    pub fn load_roms(
        &mut self,
        rom_set: &RomSet,
        config: &'static MhavocConfig,
    ) -> Result<(), RomLoadError> {
        // Recorded before the first ROM read so a blank-set load still
        // carries the attempted revision. `create` builds a fresh instance
        // per attempt, so a failed attempt cannot poison a later success.
        self.config = config;
        self.loaded_revision = config.set;
        self.board.speech = config.speech;
        self.load_regions(rom_set)
    }

    fn load_regions(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let c = self.config;
        let b = &mut self.board;
        b.map
            .load_region(AlphaRegion::ProgramRom, &c.alpha.load(rom_set)?);
        b.map
            .load_region(AlphaRegion::PagedRom, &c.alpha_paged.load(rom_set)?);
        b.map
            .load_region(AlphaRegion::VectorRom, &c.vector.load(rom_set)?);
        b.avg_rom = c.avg_paged.load(rom_set)?;
        b.gamma
            .map
            .load_region(GammaRegion::ProgramRom, &c.gamma.load(rom_set)?);
        b.avg.load_state_prom(&AVG_PROM.load(rom_set)?);
        Ok(())
    }

    /// One alpha cycle, and whatever else falls due in it.
    fn tick(&mut self) {
        let b = &mut self.board;
        b.clock_interrupts();
        if b.map.has_any_watchpoints() || b.debug_trace.enabled() {
            let pc = self
                .alpha
                .at_instruction_boundary()
                .then_some(self.alpha.pc as u32);
            b.map.latch_access_context(b.clock, pc);
        }
        let avg_cycles = self.clocks.advance(self.avg_dom);
        b.step_avg(avg_cycles);
        self.alpha.execute_cycle(&mut self.board, BusMaster::Cpu(0));

        if self.clocks.tick(self.gamma_dom) {
            let b = &mut self.board;
            if b.gamma_reset_pending {
                b.gamma_reset_pending = false;
                self.gamma.reset(&mut GammaView(b), BusMaster::Cpu(1));
            } else if !b.gamma_reset_held {
                self.gamma
                    .execute_cycle(&mut GammaView(&mut self.board), BusMaster::Cpu(1));
            }
        }
        if self.clocks.tick(self.pokey_dom) {
            self.board.gamma.quad.tick();
        }
        if self.clocks.tick(self.speech_dom) && self.board.speech {
            self.board.gamma.tms.tick();
        }
        self.board.clock += 1;
    }

    /// Advance one alpha cycle, returning which CPUs are at an instruction
    /// boundary (bit 0 alpha, bit 1 gamma).
    pub fn step_cycle(&mut self) -> u32 {
        self.tick();
        u32::from(self.alpha.at_instruction_boundary())
            | (u32::from(self.gamma.at_instruction_boundary()) << 1)
    }
}

// ---------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------

const INPUT_COIN_LEFT: u8 = 0;
const INPUT_COIN_RIGHT: u8 = 1;
const INPUT_COIN_AUX: u8 = 2;
const INPUT_FIRE: u8 = 3;
const INPUT_SHIELD: u8 = 4;
const INPUT_P2_FIRE: u8 = 5;
const INPUT_P2_SHIELD: u8 = 6;
const INPUT_ROLL_LEFT: u8 = 7;
const INPUT_ROLL_RIGHT: u8 = 8;
const INPUT_TEST: u8 = 9;
const CTRL_ROLLER: InputId = InputId(10);

const MHAVOC_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_COIN_LEFT as u16),
        stable_name: "coin1",
        label: "Coin (left)",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(INPUT_COIN_RIGHT as u16),
        stable_name: "coin2",
        label: "Coin (right)",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_COIN_AUX as u16),
        stable_name: "coin3",
        label: "Coin (aux)",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_FIRE as u16),
        stable_name: "fire",
        label: "Fire / Jump",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_SHIELD as u16),
        stable_name: "shield",
        label: "Shield",
        kind: InputKind::Action(ActionRole::Secondary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P2_FIRE as u16),
        stable_name: "p2_fire",
        label: "P2 Fire / Jump",
        kind: InputKind::Button,
        player: Some(2),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P2_SHIELD as u16),
        stable_name: "p2_shield",
        label: "P2 Shield",
        kind: InputKind::Button,
        player: Some(2),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_ROLL_LEFT as u16),
        stable_name: "roll_left",
        label: "Roll Left",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(INPUT_ROLL_RIGHT as u16),
        stable_name: "roll_right",
        label: "Roll Right",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
    InputControl {
        id: InputId(INPUT_TEST as u16),
        stable_name: "test",
        label: "Test",
        kind: InputKind::Service,
        player: None,
        default_bindings: crate::input_defaults::SERVICE,
    },
    InputControl {
        id: CTRL_ROLLER,
        stable_name: "roller",
        label: "Roller",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::X,
        },
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::AxisX)],
    },
];

impl InputConfigurable for MhavocSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        MHAVOC_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let b = &mut self.board;
        match event {
            InputEvent::Button { id, pressed } => match id.0 as u8 {
                INPUT_COIN_LEFT => set_bit_active_low(&mut b.coins, 0, pressed),
                INPUT_COIN_RIGHT => set_bit_active_low(&mut b.coins, 1, pressed),
                INPUT_COIN_AUX => set_bit_active_low(&mut b.in0_switches, 5, pressed),
                INPUT_TEST => set_bit_active_low(&mut b.service, 1, pressed),
                INPUT_FIRE => set_bit_active_low(&mut b.gamma.buttons, 7, pressed),
                INPUT_SHIELD => set_bit_active_low(&mut b.gamma.buttons, 6, pressed),
                INPUT_P2_FIRE => set_bit_active_low(&mut b.gamma.buttons, 5, pressed),
                INPUT_P2_SHIELD => set_bit_active_low(&mut b.gamma.buttons, 4, pressed),
                INPUT_ROLL_LEFT => b.gamma.roller.set_held(false, pressed),
                INPUT_ROLL_RIGHT => b.gamma.roller.set_held(true, pressed),
                _ => {}
            },
            InputEvent::Relative { id, delta } => {
                if id == CTRL_ROLLER {
                    b.gamma.roller.add_delta(delta);
                }
            }
            InputEvent::Absolute { .. } => {}
        }
    }

    fn release_all_inputs(&mut self) {
        phosphor_core::core::machine::release_all_controls(self);
        self.board.gamma.roller.release_all();
    }
}

// ---------------------------------------------------------------------------
// Machine traits
// ---------------------------------------------------------------------------

impl Renderable for MhavocSystem {
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

impl AudioSource for MhavocSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.board.audio_buffer.pop_front_into(buffer)
    }

    fn audio_sample_rate(&self) -> u32 {
        phosphor_core::audio::host_sample_rate()
    }
}

crate::impl_board_debug!(MhavocSystem, board, TIMING);
crate::impl_board_debug_trace!(MhavocSystem, board);

impl MachineCore for MhavocSystem {
    fn frame_rate_hz(&self) -> f64 {
        TIMING.frame_rate_hz()
    }

    fn machine_id(&self) -> &str {
        self.config.id
    }

    fn revision(&self) -> &str {
        self.loaded_revision
    }

    crate::machine_clock_declaration!(TIMING, clock_tree);

    fn run_frame(&mut self) {
        self.board.gamma.roller.update();
        for _ in 0..TIMING.cycles_per_frame() {
            self.tick();
        }
        self.board.present();
        self.board.mix_audio();
    }

    fn reset(&mut self) {
        let b = &mut self.board;
        b.avg.reset();
        b.display_list.clear();
        b.latest_picture.clear();
        b.ram_page = 0;
        b.rom_page = 0;
        select_ram_page(&mut b.map, 0);
        select_rom_page(&mut b.map, 0);
        b.alpha_data = 0;
        b.alpha_rcvd = false;
        b.alpha_xmtd = false;
        b.gamma_data = 0;
        b.gamma_rcvd = false;
        b.gamma_xmtd = false;
        b.gamma_nmi = false;
        b.player_1 = false;
        b.gamma_reset_held = false;
        b.gamma_reset_pending = false;
        b.irq_clock_phase = 0;
        b.alpha_irq_clock = 0;
        b.alpha_irq_clock_enable = true;
        b.alpha_irq = false;
        b.gamma_irq_clock = 0;
        b.gamma.quad.reset();
        b.gamma.tms.reset();
        b.gamma.roller = new_roller();
        b.audio_buffer.clear();
        self.clocks.reset();
        self.alpha.reset(&mut self.board, BusMaster::Cpu(0));
        self.gamma
            .reset(&mut GammaView(&mut self.board), BusMaster::Cpu(1));
    }
}

impl SaveState for MhavocSystem {
    crate::machine_save_state!();
}

impl Nvram for MhavocSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.gamma.map.region_data(GammaRegion::Eeprom))
    }

    fn load_nvram(&mut self, data: &[u8]) {
        let eeprom = self.board.gamma.map.region_data_mut(GammaRegion::Eeprom);
        let n = data.len().min(eeprom.len());
        eeprom[..n].copy_from_slice(&data[..n]);
    }
}

impl Profilable for MhavocSystem {}

// ---------------------------------------------------------------------------
// DIP switches
// ---------------------------------------------------------------------------

/// DSW1 at 13/14S, read through chip 0's ALLPOT.
const DSW1: DipSwitchBank = DipSwitchBank {
    name: "DSW1 (13/14S)",
    options: &[
        option(
            "Adaptive Difficulty",
            0x01,
            &[choice("On", 0x00), choice("Off", 0x01)],
        ),
        option(
            "Demo Sounds",
            0x02,
            &[choice("On", 0x00), choice("Off", 0x02)],
        ),
        option(
            "Bonus Life",
            0x0C,
            &[
                choice("100000", 0x00),
                choice("200000", 0x04),
                choice("None", 0x08),
                choice("50000", 0x0C),
            ],
        ),
        option(
            "Difficulty",
            0x30,
            &[
                choice("Medium", 0x00),
                choice("Easy", 0x10),
                choice("Demo", 0x20),
                choice("Hard", 0x30),
            ],
        ),
        option(
            "Lives",
            0xC0,
            &[
                choice("3 (2 in Free Play)", 0x00),
                choice("6 (5 in Free Play)", 0x40),
                choice("5 (4 in Free Play)", 0x80),
                choice("4 (3 in Free Play)", 0xC0),
            ],
        ),
    ],
};

/// The prototype's DSW1: lives on bits 0-1 instead.
const DSW1_PROTOTYPE: DipSwitchBank = DipSwitchBank {
    name: "DSW1 (13/14S)",
    options: &[option(
        "Lives",
        0x03,
        &[
            choice("1", 0x00),
            choice("2", 0x01),
            choice("3", 0x02),
            choice("4", 0x03),
        ],
    )],
};

/// DSW2 at 8S, read by gamma at 0x4000.
const DSW2: DipSwitchBank = DipSwitchBank {
    name: "DSW2 (8S)",
    options: &[
        option(
            "Coinage",
            0x03,
            &[
                choice("1 Coin/2 Credits", 0x00),
                choice("Free Play", 0x01),
                choice("2 Coins/1 Credit", 0x02),
                choice("1 Coin/1 Credit", 0x03),
            ],
        ),
        option(
            "Right Coin Mechanism",
            0x0C,
            &[
                choice("x6", 0x00),
                choice("x5", 0x04),
                choice("x4", 0x08),
                choice("x1", 0x0C),
            ],
        ),
        option(
            "Left Coin Mechanism",
            0x10,
            &[choice("x2", 0x00), choice("x1", 0x10)],
        ),
        option(
            "Bonus Credits",
            0xE0,
            &[
                choice("1 each 3", 0x40),
                choice("1 each 5", 0x60),
                choice("2 each 4", 0x80),
                choice("1 each 4", 0xA0),
                choice("None", 0xE0),
            ],
        ),
    ],
};

impl DipSwitches for MhavocSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        self.config.dip_banks
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        match bank {
            0 => self.board.gamma.dsw1,
            1 => self.board.gamma.dsw2,
            _ => 0,
        }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        match bank {
            0 => self.board.gamma.dsw1 = value,
            1 => self.board.gamma.dsw2 = value,
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

crate::register_machine!(
    MhavocSystem,
    "majorhavoc",
    &[
        crate::registry::Revision {
            names: &["mhavoc"],
            nvram_group: None
        },
        crate::registry::Revision {
            names: &["mhavoc2"],
            nvram_group: None
        },
        crate::registry::Revision {
            names: &["mhavocp"],
            nvram_group: Some("mhavocp")
        },
    ],
    MHAVOC_CONTROLS,
    configs = &[&MHAVOC, &MHAVOC2, &MHAVOCP],
    former_names = &["mhavoc", "mhavoc2", "mhavocp"]
);
crate::register_machine!(
    new = MhavocSystem::with_config(&MHAVOCRV),
    "majorhavocreturntovax",
    &["mhavocrv"],
    MHAVOC_CONTROLS,
    former_names = &["mhavocrv"]
);

fn gamma_map() -> AddressSpace16 {
    let mut map = AddressSpace16::new();
    map.region(
        GammaRegion::Ram,
        "RAM",
        0x0000,
        0x0800,
        AccessKind::ReadWrite,
    )
    .mirror(0x0800, 0x0000, 0x0800)
    .mirror(0x1000, 0x0000, 0x0800)
    .mirror(0x1800, 0x0000, 0x0800)
    .region(GammaRegion::Io, "I/O", 0x2000, 0x4000, AccessKind::Io)
    .region(
        GammaRegion::Eeprom,
        "EEPROM",
        0x6000,
        0x0200,
        AccessKind::ReadWrite,
    )
    .region(
        GammaRegion::ProgramRom,
        "Program ROM",
        0x8000,
        0x4000,
        AccessKind::ReadOnly,
    )
    .mirror(0xC000, 0x8000, 0x4000);
    // The 2804 ignores A9 and above, so it repeats every 512 bytes.
    for mirror in (0x6200..0x8000).step_by(0x200) {
        map.mirror(mirror as u16, 0x6000, 0x200);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both variants' tables are valid against the power-on switch bytes, and
    /// the prototype's DSW1 is its own table.
    #[test]
    fn dip_tables_are_valid_for_each_variant() {
        for config in [&MHAVOC, &MHAVOC2, &MHAVOCRV, &MHAVOCP] {
            let mut sys = MhavocSystem::new();
            let _ = sys.load_roms(&RomSet::blank(), config);
            crate::assert_dip_banks_valid(sys.dip_banks(), &[0x00, 0xFF]);
        }
        let mut p = MhavocSystem::new();
        let _ = p.load_roms(&RomSet::blank(), &MHAVOCP);
        assert_eq!(p.dip_banks()[0].options[0].name, "Lives");
        assert_eq!(p.dip_banks()[0].options[0].mask, 0x03);
    }

    /// The folded revisions share one machine id and report their own set;
    /// Return to Vax stays its own single-revision machine and reports none.
    #[test]
    fn every_revision_reports_one_machine_id_and_its_own_set() {
        for (config, set) in [
            (&MHAVOC, "mhavoc"),
            (&MHAVOC2, "mhavoc2"),
            (&MHAVOCP, "mhavocp"),
        ] {
            let mut sys = MhavocSystem::new();
            let _ = sys.load_roms(&RomSet::blank(), config);
            assert_eq!(sys.machine_id(), "majorhavoc");
            assert_eq!(sys.revision(), set);
        }

        let mut rv = MhavocSystem::with_config(&MHAVOCRV);
        let _ = rv.load_rom_set(&RomSet::blank());
        assert_eq!(rv.machine_id(), "majorhavocreturntovax");
        assert_eq!(rv.revision(), "", "single-revision machines report none");
    }

    /// Alpha's write to gamma's latch sets alpha's transmitted flag, clears
    /// gamma's received flag and raises gamma's NMI exactly once; gamma's read
    /// returns the byte and swaps the flags back. The reverse direction is the
    /// same with the roles exchanged.
    #[test]
    fn the_latch_pair_hands_a_byte_each_way() {
        let mut sys = MhavocSystem::new();
        let b = &mut sys.board;
        b.write(BusMaster::Cpu(0), 0x17C0, 0x5A);
        assert!(b.alpha_xmtd && !b.gamma_rcvd);
        assert_eq!(GammaView(b).read(BusMaster::Cpu(1), 0x2800) & 0x03, 0x01);
        assert!(GammaView(b).check_interrupts(BusMaster::Cpu(1)).nmi);
        assert!(!GammaView(b).check_interrupts(BusMaster::Cpu(1)).nmi);
        assert_eq!(GammaView(b).read(BusMaster::Cpu(1), 0x3000), 0x5A);
        assert!(!b.alpha_xmtd && b.gamma_rcvd);

        // Gamma's transmitted flag (0x1200 bit 2) rises on its write and falls
        // on alpha's read; its received flag (bit 3) still stands from above.
        GammaView(b).write(BusMaster::Cpu(1), 0x5000, 0xA5);
        assert_eq!(b.read(BusMaster::Cpu(0), 0x1200) & 0x0C, 0x0C);
        assert_eq!(b.read(BusMaster::Cpu(0), 0x1000), 0xA5);
        assert_eq!(b.read(BusMaster::Cpu(0), 0x1200) & 0x0C, 0x08);
        assert!(b.alpha_rcvd);
    }

    /// Asserting gamma's reset hold clears a queued NMI with the latch
    /// flags, and a latch write during the hold latches data but sets no
    /// flags and queues no NMI (sheet 9B: RESETγ holds the flag flops'
    /// CLR, while the data latch has no clear). Releasing the hold resets
    /// gamma with no NMI pending.
    #[test]
    fn gamma_reset_hold_clears_and_blocks_the_nmi() {
        let mut sys = MhavocSystem::new();
        let b = &mut sys.board;
        b.write(BusMaster::Cpu(0), 0x17C0, 0x5A);
        assert!(b.gamma_nmi, "a latch write queues an NMI");
        b.write(BusMaster::Cpu(0), 0x1600, 0x00);
        assert!(!b.gamma_nmi, "asserting the hold clears the queued NMI");
        assert!(!b.alpha_xmtd, "the hold clears the flags with it");
        b.write(BusMaster::Cpu(0), 0x17C0, 0xA5);
        assert_eq!(b.alpha_data, 0xA5, "data still latches during the hold");
        assert!(!b.gamma_nmi, "no NMI queues while held");
        assert!(!b.alpha_xmtd, "no flags set while held");
        b.write(BusMaster::Cpu(0), 0x1600, 0x08);
        assert!(b.gamma_reset_pending, "release schedules gamma's reset");
        assert!(!GammaView(b).check_interrupts(BusMaster::Cpu(1)).nmi);
    }

    /// The 0x1200 bit 1 input is the 2.4 kHz clock (sheet 4B: 625 kHz /
    /// 256): bit 9 of alpha's cycle count, high for the first half of
    /// each 1024-cycle period.
    #[test]
    fn in0_bit_1_is_the_2_4_khz_clock() {
        let mut sys = MhavocSystem::new();
        let b = &mut sys.board;
        for (clock, high) in [
            (0, true),
            (511, true),
            (512, false),
            (1023, false),
            (1024, true),
        ] {
            b.clock = clock;
            assert_eq!(
                b.read(BusMaster::Cpu(0), 0x1200) & 0x02 != 0,
                high,
                "clock {clock}"
            );
        }
    }

    /// A lit pass is always the new picture; a short dark pass is the
    /// beam-parking run and is dropped; a dark pass longer than the parking
    /// pass means the picture itself went dark and shows.
    #[test]
    fn only_parking_short_dark_passes_are_dropped() {
        fn line(intensity: u8) -> VectorLine {
            VectorLine {
                x0: 0.0,
                y0: 0.0,
                x1: 1.0,
                y1: 1.0,
                intensity,
                r: 0xFF,
                g: 0xFF,
                b: 0xFF,
                beam_cycles: 8,
                dwell_cycles: 0,
            }
        }
        let dark = |n: usize| vec![line(0); n];
        let mut sys = MhavocSystem::new();
        let b = &mut sys.board;
        b.file_pass(vec![line(8)]);
        b.present();
        assert_eq!(b.display_list.len(), 1);
        b.file_pass(dark(PARK_PASS_MAX_VECTORS));
        b.file_pass(dark(PARK_PASS_MAX_VECTORS));
        b.present();
        assert_eq!(
            b.display_list[0].intensity, 8,
            "parking passes change nothing, however many in a row"
        );
        b.file_pass(dark(PARK_PASS_MAX_VECTORS + 1));
        b.present();
        assert_eq!(b.display_list[0].intensity, 0, "a dark picture shows");
        b.file_pass(vec![line(8)]);
        b.present();
        assert_eq!(b.display_list[0].intensity, 8, "light returns");
    }

    /// The RAM page register moves both windows, 0x0200 and 0x0A00, together;
    /// the ROM page register chooses which 8K of the paged ROM is at 0x2000.
    #[test]
    fn paging_moves_both_ram_windows_and_the_rom_window() {
        let mut sys = MhavocSystem::new();
        let rom: Vec<u8> = (0..0x8000u32).map(|i| (i / 0x2000) as u8 + 1).collect();
        sys.board.map.load_region(AlphaRegion::PagedRom, &rom);
        let b = &mut sys.board;
        b.write(BusMaster::Cpu(0), 0x0200, 0x11);
        b.write(BusMaster::Cpu(0), 0x1780, 1);
        assert_eq!(b.read(BusMaster::Cpu(0), 0x0200), 0x00, "page 1 is fresh");
        b.write(BusMaster::Cpu(0), 0x0A00, 0x22);
        b.write(BusMaster::Cpu(0), 0x1780, 0);
        assert_eq!(b.read(BusMaster::Cpu(0), 0x0200), 0x11);
        assert_eq!(
            b.read(BusMaster::Cpu(0), 0x0A00),
            0x11,
            "one page, two windows"
        );
        b.write(BusMaster::Cpu(0), 0x1780, 1);
        assert_eq!(b.read(BusMaster::Cpu(0), 0x0200), 0x22);
        for page in 0..4u8 {
            b.write(BusMaster::Cpu(0), 0x1740, page);
            assert_eq!(
                b.read(BusMaster::Cpu(0), 0x3FFF),
                page + 1,
                "ROM page {page}"
            );
        }
        // 0x0800 is its own RAM between the windows.
        b.write(BusMaster::Cpu(0), 0x0800, 0x33);
        assert_eq!(b.read(BusMaster::Cpu(0), 0x0800), 0x33);
        assert_eq!(b.read(BusMaster::Cpu(0), 0x0200), 0x22);
    }
}
