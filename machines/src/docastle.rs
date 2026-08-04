//! Universal "Mr. Do's Castle" board family (1983-84).
//!
//! Three games share one PCB, differing only in their memory maps, DIP
//! semantics and cabinet orientation:
//!
//! | CLI name   | Title               | Map                  | Monitor |
//! |------------|---------------------|----------------------|---------|
//! | `docastle` | Mr. Do's Castle     | contiguous 0000-7FFF | ROT270  |
//! | `dorunrun` | Do! Run Run         | split 0000-1FFF + 4000-9FFF | ROT0 |
//! | `dowild`   | Mr. Do's Wild Ride  | split (as `dorunrun`) | ROT0   |
//!
//! Hardware:
//! - **Two Z80s @ 4 MHz.** `main` runs the game; `sub` owns the inputs and the
//!   sound chips. They talk through a single bidirectional latch that asserts
//!   the main CPU's `WAIT` input on every access, which is what keeps the two
//!   `LDIR` block transfers in lockstep (see [`DocastleBoard::tick`]).
//! - A third Z80 on the real board is a pass-through doorway in front of sprite
//!   RAM; it copies 0x200 bytes from the main CPU to the sprite chip unmodified,
//!   so this emulation has the main CPU write sprite RAM directly.
//! - **Sound:** 4× SN76489A @ 4 MHz driven by the sub CPU. Their `READY`
//!   outputs are wired to the sub's `WAIT`, stalling it after each write.
//! - **Inputs:** two TMS1025 8-way multiplexers (low and high nibble) behind an
//!   LS273 that latches the select lines from the *address* of the read, so a
//!   read returns the port selected by the *previous* read.
//! - **Video:** one 32×32 tilemap of 8×8 4bpp tiles plus 16×16 4bpp sprites,
//!   composed tilemap → sprites → tilemap-again for the priority half of the
//!   pens. 240×192 visible.
//! - **Interrupts:** an HD6845S CRTC drives VSYNC to the main CPU's IRQ and a
//!   memory-address bit to the sub CPU's IRQ; both are approximated here from
//!   the raster position (see [`SUB_IRQ_FIRST_LINE`]).

use phosphor_core::audio::AudioResampler;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    ActionRole, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, DipSwitches, Direction,
    InputConfigurable, InputControl, InputEvent, InputId, InputKind, MachineCore, Orientation,
    SaveState,
};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, ClockDivider, TimingConfig};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::z80::Z80;
use phosphor_core::device::sn76489::Sn76489a;
use phosphor_core::gfx;
use phosphor_core::gfx::decode::{GfxLayout, decode_gfx};
use phosphor_core::{bus_split, core::machine::DefaultBinding};
use phosphor_macros::{BusDebug, MemoryRegion};

use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::gfx_registry::GfxRegion;
use crate::input_defaults as ind;
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_low;

// ---------------------------------------------------------------------------
// Memory map region IDs
// ---------------------------------------------------------------------------

/// Main CPU (Z80) regions. The shared latch, watchdog and sub-NMI trigger are
/// decoded directly in the [`Bus`] impl; their addresses vary per variant.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum MainRegion {
    /// docastle: 0x0000-0x7FFF. dorunrun/dowild: 0x0000-0x1FFF.
    RomLow = 1,
    /// docastle: 0x8000-0x97FF. dorunrun/dowild: 0x2000-0x37FF.
    WorkRam = 2,
    /// docastle: 0x9800-0x99FF. dorunrun/dowild: 0x3800-0x39FF. 128 sprites × 4.
    SpriteRam = 3,
    /// dorunrun/dowild only: 0x4000-0x9FFF.
    RomHigh = 4,
    /// 0xB000-0xB3FF — 32×32 tile codes.
    VideoRam = 5,
    /// 0xB400-0xB7FF — 32×32 tile colour/bank attributes.
    ColorRam = 6,
}

/// Sub CPU (Z80) regions. The latch, the four PSG ports and the input mux are
/// decoded in the [`Bus`] impl.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum SubRegion {
    Rom = 1, // 0x0000-0x3FFF
    Ram = 2, // 0x8000-0x87FF
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------
// Both Z80s run from a 4 MHz crystal. Video comes off a separate 9.828 MHz
// crystal: pixel clock 9.828/2 = 4.914 MHz, HTOTAL 312, VTOTAL 264, giving
// 59.66 Hz. That works out to 4e6 × 312 / 4.914e6 ≈ 254 CPU cycles per
// scanline, so a frame is 254 × 264 = 67056 cycles ⇒ 59.65 Hz.

pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 4_000_000,
    cycles_per_scanline: 254,
    total_scanlines: 264,
    // Native (pre-orientation) framebuffer. Rotation is declared per variant
    // and applied centrally by the frontend.
    display_width: VISIBLE_WIDTH as u32,   // 240
    display_height: VISIBLE_HEIGHT as u32, // 192
    // Overridden per variant in `Renderable::display_aspect` — a rotated
    // cabinet presents the same 4:3 tube as 3:4.
    display_aspect: Some((4, 3)),
};

/// Native visible raster. The CRTC is programmed for 32 characters, but its
/// display-enable output is gated so the first and last 8 pixels of every line
/// are blanked: x 8..247 of a 256-pixel line, y 0..191.
pub const VISIBLE_WIDTH: usize = 240;
pub const VISIBLE_HEIGHT: usize = 192;

/// Left edge of the visible window inside the 256-pixel raster line.
const VISIBLE_X_ORIGIN: i32 = 8;

/// The tilemap is 32×32 tiles of 8×8 pixels and sits 32 pixels above the top of
/// the visible raster, so screen row 0 samples tilemap row 32.
const TILEMAP_Y_OFFSET: usize = 32;

/// Scanline at which VSYNC asserts the main CPU's IRQ (start of vertical blank).
const VBLANK_IRQ_LINE: u64 = VISIBLE_HEIGHT as u64;

// The sub CPU's IRQ comes from bit 6 of the CRTC's memory-address counter
// sampled on each HSYNC. With 32 displayed characters per row that counter
// advances 32 per 8-scanline character row, so bit 6 toggles every two rows and
// rises every four — 8 times across the 33 character rows of a frame. We model
// that directly as a rising edge every 32 scanlines rather than emulating a
// full HD6845S.
const SUB_IRQ_FIRST_LINE: u64 = 8;
const SUB_IRQ_LINE_PERIOD: u64 = 32;
const SUB_IRQ_LAST_LINE: u64 = SUB_IRQ_FIRST_LINE + 7 * SUB_IRQ_LINE_PERIOD; // 232

/// All four PSGs share the CPU's 4 MHz clock.
const SOUND_CLOCK: u32 = TIMING.cpu_clock_hz as u32;
const OUTPUT_SAMPLE_RATE: u64 = 44_100;

/// Palette pens: 32 colour codes × 16 pens. Pen bit 3 is the transparency /
/// priority flag rather than a colour bit, so pens `n` and `n | 8` share an RGB
/// value (see [`docastle_palette_rgb`]).
const PALETTE_LEN: usize = 512;

const TILE_COUNT: usize = 512; // 0x4000 bytes / 32 bytes per 8×8 4bpp tile
const SPRITE_COUNT: usize = 256; // 0x8000 bytes / 128 bytes per 16×16 4bpp sprite
const SPRITE_RAM_LEN: usize = 0x200;

// ---------------------------------------------------------------------------
// GFX bit-plane layouts
// ---------------------------------------------------------------------------
// Both regions are "packed MSB": all four planes of a pixel live in one nibble,
// high nibble first. `plane_offsets` is LSB-first (plane 0 = pixel bit 0), so
// the nibble's most significant bit is the last entry.

/// 8×8 4bpp tiles — 32 bytes each.
pub static DOCASTLE_TILE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[3, 2, 1, 0],
    x_offsets: &[0, 4, 8, 12, 16, 20, 24, 28],
    y_offsets: &[0, 32, 64, 96, 128, 160, 192, 224],
    char_increment: 8 * 8 * 4,
};

/// 16×16 4bpp sprites — 128 bytes each.
pub static DOCASTLE_SPRITE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[3, 2, 1, 0],
    x_offsets: &[0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 52, 56, 60],
    y_offsets: &[
        0, 64, 128, 192, 256, 320, 384, 448, 512, 576, 640, 704, 768, 832, 896, 960,
    ],
    char_increment: 16 * 16 * 4,
};

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// Decode the 256-byte colour PROM into the 512-entry pen table.
///
/// Each PROM byte drives a resistor DAC: red from bits 7-5 and green from bits
/// 4-2 through 200/390/820 Ω, blue from bits 1-0 through 200/390 Ω. The ladder
/// weights are 0x91/0x4b/0x23 for the three-bit guns and 0xad/0x52 for blue.
///
/// Graphics are 4bpp with the top pen bit used for transparency and sprite
/// priority rather than colour, so each PROM entry is written to two pens —
/// `…|0x00` and `…|0x08` — and the renderer can ignore bit 3 when resolving a
/// colour.
fn docastle_palette_rgb(prom: &[u8]) -> [(u8, u8, u8); PALETTE_LEN] {
    let mut out = [(0u8, 0u8, 0u8); PALETTE_LEN];
    for i in 0..256 {
        let byte = prom.get(i).copied().unwrap_or(0);
        let bit = |n: u8| u16::from((byte >> n) & 1);
        let r = (0x23 * bit(5) + 0x4b * bit(6) + 0x91 * bit(7)) as u8;
        let g = (0x23 * bit(2) + 0x4b * bit(3) + 0x91 * bit(4)) as u8;
        let b = (0x52 * bit(0) + 0xad * bit(1)) as u8;

        // PROM index i = colour code (i >> 3) with low pen bits (i & 7).
        let base = ((i & 0xf8) << 1) | (i & 0x07);
        out[base] = (r, g, b);
        out[base | 0x08] = (r, g, b);
    }
    out
}

// ---------------------------------------------------------------------------
// Variant configuration
// ---------------------------------------------------------------------------

/// Which of the three games this board is wired as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocastleVariant {
    Docastle,
    Dorunrun,
    Dowild,
}

/// Address decode differences between the two board revisions.
struct MapConfig {
    /// Program ROM at the bottom of the map (`0x0000..rom_low_end`).
    rom_low_end: u16,
    /// Second program ROM window, or `None` on the contiguous docastle map.
    rom_high: Option<(u16, u16)>,
    work_ram_start: u16,
    sprite_ram_start: u16,
    /// Main-CPU address whose write pulses the sub CPU's NMI.
    sub_nmi_addr: u16,
    /// Sub-CPU base of the shared latch window (0x800 bytes wide).
    sub_latch_base: u16,
    /// Sub-CPU base of the four PSG ports (spaced 0x400 apart).
    sn_base: u16,
    /// Tile pens drawn *behind* sprites. Bit `n` set = pen `n` is transparent
    /// in the front tilemap pass, so it stays wherever the opaque pass put it.
    fg_transmask: u16,
    /// Video/colour RAM is mirrored 0x800 higher on the docastle map.
    video_ram_mirror: bool,
}

const DOCASTLE_MAP: MapConfig = MapConfig {
    rom_low_end: 0x8000,
    rom_high: None,
    work_ram_start: 0x8000,
    sprite_ram_start: 0x9800,
    sub_nmi_addr: 0xE000,
    sub_latch_base: 0xA000,
    sn_base: 0xE000,
    fg_transmask: 0x00FF,
    video_ram_mirror: true,
};

const DORUNRUN_MAP: MapConfig = MapConfig {
    rom_low_end: 0x2000,
    rom_high: Some((0x4000, 0xA000)),
    work_ram_start: 0x2000,
    sprite_ram_start: 0x3800,
    sub_nmi_addr: 0xB800,
    sub_latch_base: 0xE000,
    sn_base: 0xA000,
    fg_transmask: 0xFF00,
    video_ram_mirror: false,
};

/// Main-CPU base of the shared latch window — the same on both revisions.
const MAIN_LATCH_BASE: u16 = 0xA000;
const LATCH_WINDOW_LEN: u16 = 0x800;

const WORK_RAM_LEN: u32 = 0x1800;
const VIDEO_RAM_BASE: u16 = 0xB000;
const COLOR_RAM_BASE: u16 = 0xB400;
const TILE_RAM_LEN: u32 = 0x400;

impl DocastleVariant {
    /// CLI / save-state identifier.
    pub fn id(self) -> &'static str {
        match self {
            Self::Docastle => "docastle",
            Self::Dorunrun => "dorunrun",
            Self::Dowild => "dowild",
        }
    }

    fn map(self) -> &'static MapConfig {
        match self {
            Self::Docastle => &DOCASTLE_MAP,
            Self::Dorunrun | Self::Dowild => &DORUNRUN_MAP,
        }
    }

    /// Mr. Do's Castle stands its monitor on end; the other two are landscape.
    pub fn orientation(self) -> Orientation {
        match self {
            Self::Docastle => Orientation::ROT270,
            Self::Dorunrun | Self::Dowild => Orientation::NORMAL,
        }
    }

    /// Tube aspect as viewed, i.e. after the frontend applies the rotation.
    pub fn display_aspect(self) -> Option<(u32, u32)> {
        match self {
            Self::Docastle => Some((3, 4)),
            Self::Dorunrun | Self::Dowild => Some((4, 3)),
        }
    }

    fn dip_banks(self) -> &'static [DipSwitchBank] {
        match self {
            Self::Docastle => DOCASTLE_DIP_BANKS,
            Self::Dorunrun => DORUNRUN_DIP_BANKS,
            Self::Dowild => DOWILD_DIP_BANKS,
        }
    }

    fn roms(self) -> &'static VariantRoms {
        match self {
            Self::Docastle => &DOCASTLE_ROMS,
            Self::Dorunrun => &DORUNRUN_ROMS,
            Self::Dowild => &DOWILD_ROMS,
        }
    }
}

// ---------------------------------------------------------------------------
// ROM definitions
// ---------------------------------------------------------------------------

/// The five ROM regions every variant loads.
struct VariantRoms {
    main: &'static RomRegion,
    sub: &'static RomRegion,
    tiles: &'static RomRegion,
    sprites: &'static RomRegion,
    prom: &'static RomRegion,
}

static DOCASTLE_MAIN_ROM: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        RomEntry {
            name: "01p_a1.bin",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x17c6fc24],
        },
        RomEntry {
            name: "01n_a2.bin",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x1d2fc7f4],
        },
        RomEntry {
            name: "01l_a3.bin",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x71a70ba9],
        },
        RomEntry {
            name: "01k_a4.bin",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0x479a745e],
        },
    ],
};

static DOCASTLE_SUB_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[RomEntry {
        name: "07n_a0.bin",
        size: 0x4000,
        offset: 0x0000,
        crc32: &[0xf23b5cdb],
    }],
};

static DOCASTLE_TILE_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[RomEntry {
        name: "03a_a5.bin",
        size: 0x4000,
        offset: 0x0000,
        crc32: &[0x0636b8f4],
    }],
};

static DOCASTLE_SPRITE_ROM: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        RomEntry {
            name: "04m_a6.bin",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x3bbc9b26],
        },
        RomEntry {
            name: "04l_a7.bin",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x3dfaa9d1],
        },
        RomEntry {
            name: "04j_a8.bin",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x9afb16e9],
        },
        RomEntry {
            name: "04h_a9.bin",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0xaf24bce0],
        },
    ],
};

/// Colour PROM. Mr. Do's Castle fits a 512-byte device but only the first 256
/// entries are addressed.
static DOCASTLE_PALETTE_PROM: RomRegion = RomRegion {
    size: 0x200,
    entries: &[RomEntry {
        name: "09c.bin",
        size: 0x200,
        offset: 0x0000,
        crc32: &[0x066f52bc],
    }],
};

static DOCASTLE_ROMS: VariantRoms = VariantRoms {
    main: &DOCASTLE_MAIN_ROM,
    sub: &DOCASTLE_SUB_ROM,
    tiles: &DOCASTLE_TILE_ROM,
    sprites: &DOCASTLE_SPRITE_ROM,
    prom: &DOCASTLE_PALETTE_PROM,
};

static DORUNRUN_MAIN_ROM: RomRegion = RomRegion {
    size: 0xA000,
    entries: &[
        RomEntry {
            name: "2764.p1",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x95c86f8e],
        },
        RomEntry {
            name: "2764.l1",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0xe9a65ba7],
        },
        RomEntry {
            name: "2764.k1",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0xb1195d3d],
        },
        RomEntry {
            name: "2764.n1",
            size: 0x2000,
            offset: 0x8000,
            crc32: &[0x6a8160d1],
        },
    ],
};

static DORUNRUN_SUB_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[RomEntry {
        name: "27128.p7",
        size: 0x4000,
        offset: 0x0000,
        crc32: &[0x8b06d461],
    }],
};

static DORUNRUN_TILE_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[RomEntry {
        name: "27128.a3",
        size: 0x4000,
        offset: 0x0000,
        crc32: &[0x4be96dcf],
    }],
};

static DORUNRUN_SPRITE_ROM: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        RomEntry {
            name: "2764.m4",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x4bb231a0],
        },
        RomEntry {
            name: "2764.l4",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x0c08508a],
        },
        RomEntry {
            name: "2764.j4",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x79287039],
        },
        RomEntry {
            name: "2764.h4",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0x523aa999],
        },
    ],
};

static DORUNRUN_PALETTE_PROM: RomRegion = RomRegion {
    size: 0x100,
    entries: &[RomEntry {
        name: "dorunrun.clr",
        size: 0x100,
        offset: 0x0000,
        crc32: &[0xd5bab5d5],
    }],
};

static DORUNRUN_ROMS: VariantRoms = VariantRoms {
    main: &DORUNRUN_MAIN_ROM,
    sub: &DORUNRUN_SUB_ROM,
    tiles: &DORUNRUN_TILE_ROM,
    sprites: &DORUNRUN_SPRITE_ROM,
    prom: &DORUNRUN_PALETTE_PROM,
};

static DOWILD_MAIN_ROM: RomRegion = RomRegion {
    size: 0xA000,
    entries: &[
        RomEntry {
            name: "w1",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x097de78b],
        },
        RomEntry {
            name: "w3",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0xfc6a1cbb],
        },
        RomEntry {
            name: "w4",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0x8aac1d30],
        },
        RomEntry {
            name: "w2",
            size: 0x2000,
            offset: 0x8000,
            crc32: &[0x0914ab69],
        },
    ],
};

static DOWILD_SUB_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[RomEntry {
        name: "w10",
        size: 0x4000,
        offset: 0x0000,
        crc32: &[0xd1f37fba],
    }],
};

static DOWILD_TILE_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[RomEntry {
        name: "w5",
        size: 0x4000,
        offset: 0x0000,
        crc32: &[0xb294b151],
    }],
};

static DOWILD_SPRITE_ROM: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        RomEntry {
            name: "w6",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x57e0208b],
        },
        RomEntry {
            name: "w7",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x5001a6f7],
        },
        RomEntry {
            name: "w8",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0xec503251],
        },
        RomEntry {
            name: "w9",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0xaf7bd7eb],
        },
    ],
};

static DOWILD_PALETTE_PROM: RomRegion = RomRegion {
    size: 0x100,
    entries: &[RomEntry {
        name: "dowild.clr",
        size: 0x100,
        offset: 0x0000,
        crc32: &[0xa703dea5],
    }],
};

static DOWILD_ROMS: VariantRoms = VariantRoms {
    main: &DOWILD_MAIN_ROM,
    sub: &DOWILD_SUB_ROM,
    tiles: &DOWILD_TILE_ROM,
    sprites: &DOWILD_SPRITE_ROM,
    prom: &DOWILD_PALETTE_PROM,
};

// ---------------------------------------------------------------------------
// Input multiplexer (2× TMS1025)
// ---------------------------------------------------------------------------

/// The eight multiplexer inputs, of which five are wired on this board.
/// `S0` leaves the outputs high-impedance, so a read there returns whatever the
/// buffer last held.
const MUX_DSW2: u8 = 1;
const MUX_DSW1: u8 = 2;
const MUX_JOYS: u8 = 3;
const MUX_BUTTONS: u8 = 5;
const MUX_SYSTEM: u8 = 7;

/// Two TMS1025s (one per nibble) behind an LS273 select latch.
///
/// The latch is clocked by the address decode of the *current* access, so the
/// data returned belongs to the port selected by the *previous* access. Reading
/// a port is therefore a two-step dance, and it is what makes the boot-time DIP
/// reads on this board so sensitive to CPU timing.
#[derive(Default)]
struct InputMux {
    /// Latched S0-S2 select lines (0-7).
    select: u8,
    /// Last value driven onto the H outputs, held while `select == 0`.
    hold: u8,
    joys: u8,
    buttons: u8,
    system: u8,
    dsw1: u8,
    dsw2: u8,
}

impl InputMux {
    fn new() -> Self {
        Self {
            select: 0,
            hold: 0,
            joys: 0xFF,
            buttons: 0xFF,
            system: 0xFF,
            dsw1: DOCASTLE_DSW1_DEFAULT,
            dsw2: DSW2_DEFAULT,
        }
    }

    /// Value the currently selected port drives onto the H lines.
    fn selected(&self) -> u8 {
        match self.select {
            MUX_DSW2 => self.dsw2,
            MUX_DSW1 => self.dsw1,
            MUX_JOYS => self.joys,
            MUX_BUTTONS => self.buttons,
            MUX_SYSTEM => self.system,
            // S0 tri-states the outputs; the unwired ports read back as zero.
            0 => self.hold,
            _ => 0,
        }
    }

    /// Perform one read of the mux window. `offset` is the low 8 bits of the
    /// address: bits 0-2 latch the next select, bit 7 drives flipscreen.
    fn read(&mut self, offset: u8) -> u8 {
        self.hold = self.selected();
        self.select = offset & 0x07;
        self.hold
    }
}

// ---------------------------------------------------------------------------
// DocastleBoard
// ---------------------------------------------------------------------------

#[derive(BusDebug)]
pub struct DocastleBoard {
    #[debug_cpu("Z80 Main")]
    pub(crate) main_cpu: Z80,
    #[debug_cpu("Z80 Sub")]
    pub(crate) sub_cpu: Z80,

    #[debug_map(cpu = 0)]
    pub(crate) main_map: AddressSpace16,
    #[debug_map(cpu = 1)]
    pub(crate) sub_map: AddressSpace16,

    pub(crate) variant: DocastleVariant,

    // GFX ROMs + decoded pixel caches.
    pub(crate) tile_rom: [u8; 0x4000],
    pub(crate) sprite_rom: [u8; 0x8000],
    pub(crate) tile_cache: gfx::GfxCache,   // 512 × 8×8 4bpp
    pub(crate) sprite_cache: gfx::GfxCache, // 256 × 16×16 4bpp
    pub(crate) palette_prom: [u8; 0x100],
    pub(crate) palette_rgb: [(u8, u8, u8); PALETTE_LEN],

    // Dual-CPU handshake. `main_wait` gates the main CPU's clock; `wait_toggle`
    // distinguishes a stalled latch read from the retry that follows it, and
    // `retry` tells `tick` to rewind the CPU so the read happens after the
    // stall rather than before it.
    pub(crate) shared_latch: u8,
    pub(crate) main_wait: bool,
    pub(crate) main_wait_toggle: bool,
    pub(crate) main_retry: bool,
    pub(crate) main_read_stalled: bool,
    pub(crate) sub_nmi_pending: bool,

    // Video.
    pub(crate) flipscreen: bool,

    inputs: InputMux,

    // Sound: four PSGs summed to mono. Their READY outputs are OR'd into the
    // sub CPU's WAIT input.
    #[debug_device("SN76489A")]
    pub(crate) sn: [Sn76489a; 4],
    pub(crate) sn_clock: ClockDivider,
    pub(crate) audio: AudioResampler<i16>,

    // Timing / interrupts.
    pub(crate) clock: u64,
    pub(crate) main_irq_pending: bool,
    pub(crate) sub_irq_pending: bool,
}

impl DocastleBoard {
    pub fn new(variant: DocastleVariant) -> Self {
        let mut inputs = InputMux::new();
        inputs.dsw1 = match variant {
            DocastleVariant::Docastle => DOCASTLE_DSW1_DEFAULT,
            DocastleVariant::Dorunrun => DORUNRUN_DSW1_DEFAULT,
            DocastleVariant::Dowild => DOWILD_DSW1_DEFAULT,
        };
        Self {
            main_cpu: Z80::new(),
            sub_cpu: Z80::new(),
            main_map: Self::build_main_map(variant),
            sub_map: Self::build_sub_map(),
            variant,
            tile_rom: [0; 0x4000],
            sprite_rom: [0; 0x8000],
            tile_cache: gfx::GfxCache::new(0, 8, 8),
            sprite_cache: gfx::GfxCache::new(0, 16, 16),
            palette_prom: [0; 0x100],
            palette_rgb: [(0, 0, 0); PALETTE_LEN],
            shared_latch: 0,
            main_wait: false,
            main_wait_toggle: false,
            main_retry: false,
            main_read_stalled: false,
            sub_nmi_pending: false,
            flipscreen: false,
            inputs,
            sn: [
                Sn76489a::new(SOUND_CLOCK),
                Sn76489a::new(SOUND_CLOCK),
                Sn76489a::new(SOUND_CLOCK),
                Sn76489a::new(SOUND_CLOCK),
            ],
            sn_clock: ClockDivider::new(SOUND_CLOCK / 16, TIMING.cpu_clock_hz as u32),
            audio: AudioResampler::new(TIMING.cpu_clock_hz, OUTPUT_SAMPLE_RATE),
            clock: 0,
            main_irq_pending: false,
            sub_irq_pending: false,
        }
    }

    fn build_main_map(variant: DocastleVariant) -> AddressSpace16 {
        use MainRegion::*;
        let cfg = variant.map();
        let mut map = AddressSpace16::new();
        map.region(
            RomLow,
            "Program ROM",
            0x0000,
            cfg.rom_low_end as u32,
            AccessKind::ReadOnly,
        )
        .region(
            WorkRam,
            "Work RAM",
            cfg.work_ram_start,
            WORK_RAM_LEN,
            AccessKind::ReadWrite,
        )
        .region(
            SpriteRam,
            "Sprite RAM",
            cfg.sprite_ram_start,
            SPRITE_RAM_LEN as u32,
            AccessKind::ReadWrite,
        )
        .region(
            VideoRam,
            "Video RAM",
            VIDEO_RAM_BASE,
            TILE_RAM_LEN,
            AccessKind::ReadWrite,
        )
        .region(
            ColorRam,
            "Color RAM",
            COLOR_RAM_BASE,
            TILE_RAM_LEN,
            AccessKind::ReadWrite,
        );

        if let Some((start, end)) = cfg.rom_high {
            map.region(
                RomHigh,
                "Program ROM (high)",
                start,
                (end - start) as u32,
                AccessKind::ReadOnly,
            );
        }
        if cfg.video_ram_mirror {
            map.mirror(VIDEO_RAM_BASE + 0x800, VIDEO_RAM_BASE, TILE_RAM_LEN)
                .mirror(COLOR_RAM_BASE + 0x800, COLOR_RAM_BASE, TILE_RAM_LEN);
        }
        map
    }

    fn build_sub_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(
            SubRegion::Rom,
            "Sub ROM",
            0x0000,
            0x4000,
            AccessKind::ReadOnly,
        )
        .region(
            SubRegion::Ram,
            "Sub RAM",
            0x8000,
            0x0800,
            AccessKind::ReadWrite,
        );
        map
    }

    // -----------------------------------------------------------------------
    // ROM-derived state
    // -----------------------------------------------------------------------

    pub fn decode_gfx_roms(&mut self) {
        self.tile_cache = decode_gfx(&self.tile_rom, 0, TILE_COUNT, &DOCASTLE_TILE_LAYOUT);
        self.sprite_cache = decode_gfx(&self.sprite_rom, 0, SPRITE_COUNT, &DOCASTLE_SPRITE_LAYOUT);
    }

    pub fn build_palette(&mut self) {
        self.palette_rgb = docastle_palette_rgb(&self.palette_prom);
    }

    // -----------------------------------------------------------------------
    // Latch helpers (called from the `Bus` impl)
    // -----------------------------------------------------------------------

    /// Main CPU reads the shared latch.
    ///
    /// The access always asserts WAIT first: the first attempt stalls, and only
    /// the retry — after the sub CPU has touched the latch and released WAIT —
    /// samples the data. `main_retry` asks [`tick`](Self::tick) to rewind the
    /// CPU so the stalled attempt has no effect.
    fn main_read_latch(&mut self) -> u8 {
        self.main_wait_toggle = !self.main_wait_toggle;
        if self.main_wait_toggle {
            self.main_wait = true;
            self.main_retry = true;
        }
        self.shared_latch
    }

    /// Main CPU writes the shared latch: the byte lands, then WAIT asserts.
    fn main_write_latch(&mut self, data: u8) {
        self.shared_latch = data;
        self.main_wait = true;
    }

    /// Any sub-CPU access to the latch releases the main CPU's WAIT.
    fn sub_read_latch(&mut self) -> u8 {
        self.main_wait = false;
        self.shared_latch
    }

    fn sub_write_latch(&mut self, data: u8) {
        self.shared_latch = data;
        self.main_wait = false;
    }

    // -----------------------------------------------------------------------
    // Core tick
    // -----------------------------------------------------------------------

    /// Run one main-CPU T-state, rewinding it if the access hit the latch and
    /// asserted WAIT.
    fn step_main(&mut self, bus: &mut dyn Bus<Address = u16, Data = u8>) {
        let snapshot = self.main_cpu.clone();
        self.main_retry = false;
        let iff1_before = self.main_cpu.iff1;
        self.main_cpu.execute_cycle(bus, BusMaster::Cpu(0));
        if self.main_retry {
            self.main_cpu = snapshot;
            self.main_retry = false;
            self.main_read_stalled = true;
        } else if self.main_irq_pending && iff1_before && !self.main_cpu.iff1 {
            // HOLD_LINE auto-clear: the CPU acknowledged, observed as IFF1
            // dropping at an instruction boundary with the IRQ asserted.
            self.main_irq_pending = false;
        }
    }

    /// Advance the board by one 4 MHz cycle, stepping both Z80s in lockstep.
    ///
    /// Both CPUs are clocked 1:1 and both bus accesses resolve inside the same
    /// tick, so the WAIT gates take effect at T-state granularity — which is
    /// what the latch handshake needs.
    pub fn tick(&mut self, bus: &mut dyn Bus<Address = u16, Data = u8>) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();
        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let line = frame_cycle / TIMING.cycles_per_scanline;
            if line == VBLANK_IRQ_LINE {
                self.main_irq_pending = true;
            }
            if (SUB_IRQ_FIRST_LINE..=SUB_IRQ_LAST_LINE).contains(&line)
                && (line - SUB_IRQ_FIRST_LINE).is_multiple_of(SUB_IRQ_LINE_PERIOD)
            {
                self.sub_irq_pending = true;
            }
        }

        if self.main_map.debug_active() {
            let pc = self
                .main_cpu
                .at_instruction_boundary()
                .then_some(self.main_cpu.pc as u32);
            self.main_map.latch_access_context(self.clock, pc);
        }
        if self.sub_map.debug_active() {
            let pc = self
                .sub_cpu
                .at_instruction_boundary()
                .then_some(self.sub_cpu.pc as u32);
            self.sub_map.latch_access_context(self.clock, pc);
        }

        // Main CPU. A stalled latch read rewinds the CPU so the T-state runs
        // again once the sub CPU releases WAIT — the chip holds the address on
        // the bus and latches data only after WAIT goes away.
        if !self.main_wait {
            self.step_main(bus);
        }

        // Sub CPU, stalled while any PSG holds READY low.
        if self.sn.iter().all(Sn76489a::is_ready) {
            let iff1_before = self.sub_cpu.iff1;
            self.sub_cpu.execute_cycle(bus, BusMaster::Cpu(1));
            if self.sub_irq_pending && iff1_before && !self.sub_cpu.iff1 {
                self.sub_irq_pending = false;
            }
        }

        // A read that stalled cost the main CPU the cycle it was rewound out
        // of; if the sub CPU released WAIT during this same cycle, hand it
        // straight back by running the retry now. Without that the two `LDIR`s
        // drift a cycle apart per byte and the main CPU starts sampling the
        // latch one sub-write too late.
        if self.main_read_stalled && !self.main_wait {
            self.main_read_stalled = false;
            self.step_main(bus);
        }

        for chip in &mut self.sn {
            chip.tick_ready();
        }

        // PSG generators run at chip_clock / 16; box-filter the summed output
        // down to the audio rate, one input sample per CPU cycle.
        if self.sn_clock.tick() {
            for chip in &mut self.sn {
                chip.tick();
            }
        }
        let mix = self
            .sn
            .iter()
            .map(|c| i32::from(c.output()))
            .sum::<i32>()
            .clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        if let Some(avg) = self.audio.tick_sample(mix) {
            self.audio.push_sample(avg);
        }

        self.clock += 1;
    }

    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.audio.fill_audio(buffer)
    }

    // -----------------------------------------------------------------------
    // Video
    // -----------------------------------------------------------------------

    /// Render one frame into `buffer` (240×192 RGB24, pre-rotation).
    ///
    /// Composition order matches the hardware: the tilemap is laid down opaque,
    /// sprites go over it, then the tilemap's "priority" half of the pens
    /// (selected by the variant's transparency mask) is drawn again on top.
    /// There are no mid-frame raster effects on this board, so rendering the
    /// whole frame at once is equivalent to per-scanline output.
    pub fn render_frame(&self, buffer: &mut [u8]) {
        if self.tile_cache.count() == 0 {
            buffer.fill(0);
            return;
        }

        let videoram = self.main_map.region_data(MainRegion::VideoRam);
        let colorram = self.main_map.region_data(MainRegion::ColorRam);
        let spriteram = self.main_map.region_data(MainRegion::SpriteRam);
        let transmask = self.variant.map().fg_transmask;
        let flip = self.flipscreen;

        let mut pen = vec![0u16; VISIBLE_WIDTH * VISIBLE_HEIGHT];
        // Tile pixel value per screen pixel, kept for the second tilemap pass.
        let mut tile_val = vec![0u8; VISIBLE_WIDTH * VISIBLE_HEIGHT];
        let mut tile_pen = vec![0u16; VISIBLE_WIDTH * VISIBLE_HEIGHT];

        // Opaque tilemap pass.
        for ny in 0..VISIBLE_HEIGHT {
            for nx in 0..VISIBLE_WIDTH {
                let ax = nx + VISIBLE_X_ORIGIN as usize;
                // Cocktail flip mirrors the tilemap about the visible centre;
                // sampling the mirrored coordinate is the same thing.
                let (tx, ty) = if flip {
                    (255 - ax, 255 - (ny + TILEMAP_Y_OFFSET))
                } else {
                    (ax, ny + TILEMAP_Y_OFFSET)
                };
                let idx = (ty / 8) * 32 + tx / 8;
                let attr = colorram[idx];
                // Attribute bit 5 is the 256-tile bank select.
                let code = videoram[idx] as usize + 8 * (attr as usize & 0x20);
                let color = (attr & 0x1f) as u16;
                let val = self.tile_cache.pixel(code, tx & 7, ty & 7);
                let i = ny * VISIBLE_WIDTH + nx;
                tile_val[i] = val;
                tile_pen[i] = color * 16 + val as u16;
                pen[i] = tile_pen[i];
            }
        }

        self.draw_sprites(spriteram, &mut pen);

        // Front tilemap pass: pens whose transparency bit is clear are redrawn
        // over the sprites.
        for i in 0..pen.len() {
            if (transmask >> tile_val[i]) & 1 == 0 {
                pen[i] = tile_pen[i];
            }
        }

        for (i, &p) in pen.iter().enumerate() {
            let (r, g, b) = self.palette_rgb[p as usize];
            buffer[i * 3] = r;
            buffer[i * 3 + 1] = g;
            buffer[i * 3 + 2] = b;
        }
    }

    /// Draw the 128 sprites, highest entry first.
    ///
    /// Only pens 8-15 are non-transparent. Pens 8-14 paint; pen 15 paints
    /// nothing but still claims the pixel, so it punches sprite-shaped holes
    /// that later (lower-numbered, higher-priority) sprites cannot draw into.
    fn draw_sprites(&self, spriteram: &[u8], pen: &mut [u16]) {
        let mut claimed = vec![false; VISIBLE_WIDTH * VISIBLE_HEIGHT];

        for offs in (0..SPRITE_RAM_LEN).step_by(4).rev() {
            let mut sy = spriteram[offs] as i32 - 32;
            let mut sx = ((spriteram[offs + 1] as i32 + 8) & 0xff) - 8;
            let attr = spriteram[offs + 2];
            let code = spriteram[offs + 3] as usize;
            let color = (attr & 0x1f) as u16;
            let mut flipx = attr & 0x40 != 0;
            let mut flipy = attr & 0x80 != 0;

            if self.flipscreen {
                sx = 240 - sx;
                sy = 176 - sy;
                flipx = !flipx;
                flipy = !flipy;
            }

            for py in 0..16i32 {
                let ny = sy + py;
                if !(0..VISIBLE_HEIGHT as i32).contains(&ny) {
                    continue;
                }
                let ry = if flipy { 15 - py } else { py } as usize;
                for px in 0..16i32 {
                    let nx = sx + px - VISIBLE_X_ORIGIN;
                    if !(0..VISIBLE_WIDTH as i32).contains(&nx) {
                        continue;
                    }
                    let rx = if flipx { 15 - px } else { px } as usize;
                    let val = self.sprite_cache.pixel(code, rx, ry);
                    if val < 8 {
                        continue;
                    }
                    let i = ny as usize * VISIBLE_WIDTH + nx as usize;
                    if val != 15 && !claimed[i] {
                        pen[i] = color * 16 + val as u16;
                    }
                    claimed[i] = true;
                }
            }
        }
    }

    pub fn orientation(&self) -> Orientation {
        self.variant.orientation()
    }

    // -----------------------------------------------------------------------
    // Reset / interrupts
    // -----------------------------------------------------------------------

    pub fn reset(&mut self) {
        self.clock = 0;
        self.main_irq_pending = false;
        self.sub_irq_pending = false;
        self.sub_nmi_pending = false;
        self.shared_latch = 0;
        self.main_wait = false;
        self.main_wait_toggle = false;
        self.main_retry = false;
        self.main_read_stalled = false;
        self.flipscreen = false;
        // The select latch is cleared by the same LS273 reset that clears
        // flipscreen.
        self.inputs.select = 0;
        self.inputs.hold = 0;
        for chip in &mut self.sn {
            chip.reset();
        }
        self.sn_clock.reset();
        self.audio.reset();
        self.main_map.region_data_mut(MainRegion::WorkRam).fill(0);
        self.main_map.region_data_mut(MainRegion::SpriteRam).fill(0);
        self.main_map.region_data_mut(MainRegion::VideoRam).fill(0);
        self.main_map.region_data_mut(MainRegion::ColorRam).fill(0);
        self.sub_map.region_data_mut(SubRegion::Ram).fill(0);
    }

    pub fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        match target {
            BusMaster::Cpu(0) => InterruptState {
                irq: self.main_irq_pending,
                ..Default::default()
            },
            BusMaster::Cpu(1) => {
                // The NMI is a pulse: present it for one interrupt check so the
                // CPU's edge detector sees a rising edge, then drop it.
                let nmi = self.sub_nmi_pending;
                self.sub_nmi_pending = false;
                InterruptState {
                    irq: self.sub_irq_pending,
                    nmi,
                    ..Default::default()
                }
            }
            _ => InterruptState::default(),
        }
    }

    pub fn debug_tick_boundaries(&self) -> u32 {
        u32::from(self.main_cpu.at_instruction_boundary())
            + u32::from(self.sub_cpu.at_instruction_boundary())
    }
}

impl Saveable for DocastleBoard {
    fn save_state(&self, w: &mut StateWriter) {
        self.main_cpu.save_state(w);
        self.sub_cpu.save_state(w);
        w.write_bytes(self.main_map.region_data(MainRegion::WorkRam));
        w.write_bytes(self.main_map.region_data(MainRegion::SpriteRam));
        w.write_bytes(self.main_map.region_data(MainRegion::VideoRam));
        w.write_bytes(self.main_map.region_data(MainRegion::ColorRam));
        w.write_bytes(self.sub_map.region_data(SubRegion::Ram));
        w.write_u8(self.shared_latch);
        w.write_bool(self.main_wait);
        w.write_bool(self.main_wait_toggle);
        w.write_bool(self.sub_nmi_pending);
        w.write_bool(self.flipscreen);
        w.write_u8(self.inputs.select);
        w.write_u8(self.inputs.hold);
        w.write_u8(self.inputs.joys);
        w.write_u8(self.inputs.buttons);
        w.write_u8(self.inputs.system);
        w.write_u8(self.inputs.dsw1);
        w.write_u8(self.inputs.dsw2);
        for chip in &self.sn {
            chip.save_state(w);
        }
        self.sn_clock.save_state(w);
        self.audio.save_state(w);
        w.write_u64_le(self.clock);
        w.write_bool(self.main_irq_pending);
        w.write_bool(self.sub_irq_pending);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.main_cpu.load_state(r)?;
        self.sub_cpu.load_state(r)?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::WorkRam))?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::SpriteRam))?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::VideoRam))?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::ColorRam))?;
        r.read_bytes_into(self.sub_map.region_data_mut(SubRegion::Ram))?;
        self.shared_latch = r.read_u8()?;
        self.main_wait = r.read_bool()?;
        self.main_wait_toggle = r.read_bool()?;
        self.sub_nmi_pending = r.read_bool()?;
        self.flipscreen = r.read_bool()?;
        self.inputs.select = r.read_u8()?;
        self.inputs.hold = r.read_u8()?;
        self.inputs.joys = r.read_u8()?;
        self.inputs.buttons = r.read_u8()?;
        self.inputs.system = r.read_u8()?;
        self.inputs.dsw1 = r.read_u8()?;
        self.inputs.dsw2 = r.read_u8()?;
        for chip in &mut self.sn {
            chip.load_state(r)?;
        }
        self.sn_clock.load_state(r)?;
        self.audio.load_state(r)?;
        self.clock = r.read_u64_le()?;
        self.main_irq_pending = r.read_bool()?;
        self.sub_irq_pending = r.read_bool()?;
        self.main_retry = false;
        self.main_read_stalled = false;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// DocastleSystem wrapper
// ---------------------------------------------------------------------------

/// One of the three Mr. Do's Castle family games, selected by
/// [`DocastleVariant`].
pub struct DocastleSystem {
    pub board: DocastleBoard,
}

impl DocastleSystem {
    pub fn new(variant: DocastleVariant) -> Self {
        Self {
            board: DocastleBoard::new(variant),
        }
    }

    pub fn variant(&self) -> DocastleVariant {
        self.board.variant
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let variant = self.board.variant;
        let roms = variant.roms();
        let cfg = variant.map();

        let prog = roms.main.load(rom_set)?;
        self.board.main_map.load_region_at(
            MainRegion::RomLow,
            0,
            &prog[..cfg.rom_low_end as usize],
        );
        if let Some((start, end)) = cfg.rom_high {
            self.board.main_map.load_region_at(
                MainRegion::RomHigh,
                0,
                &prog[start as usize..end as usize],
            );
        }

        let sub = roms.sub.load(rom_set)?;
        self.board.sub_map.load_region_at(SubRegion::Rom, 0, &sub);

        self.board
            .tile_rom
            .copy_from_slice(&roms.tiles.load(rom_set)?);
        self.board
            .sprite_rom
            .copy_from_slice(&roms.sprites.load(rom_set)?);
        // Only the first 256 entries of the colour PROM are addressed.
        let prom = roms.prom.load(rom_set)?;
        self.board.palette_prom.copy_from_slice(&prom[..0x100]);

        self.board.decode_gfx_roms();
        self.board.build_palette();
        Ok(())
    }
}

impl Saveable for DocastleSystem {
    fn save_state(&self, w: &mut StateWriter) {
        self.board.save_state(w);
    }
    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.board.load_state(r)
    }
}

impl Bus for DocastleSystem {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let cfg = self.board.variant.map();
        let data = match master {
            BusMaster::Cpu(0) => {
                if (MAIN_LATCH_BASE..MAIN_LATCH_BASE + LATCH_WINDOW_LEN).contains(&addr) {
                    // The stalled first attempt must leave no trace in the
                    // watch/trace rings — the retry records the real access.
                    return self.board.main_read_latch();
                }
                match addr {
                    a if a < cfg.rom_low_end => self.board.main_map.read_backing(a),
                    a if (cfg.work_ram_start..cfg.work_ram_start + WORK_RAM_LEN as u16)
                        .contains(&a) =>
                    {
                        self.board.main_map.read_backing(a)
                    }
                    a if (cfg.sprite_ram_start..cfg.sprite_ram_start + SPRITE_RAM_LEN as u16)
                        .contains(&a) =>
                    {
                        self.board.main_map.read_backing(a)
                    }
                    a if cfg.rom_high.is_some_and(|(s, e)| (s..e).contains(&a)) => {
                        self.board.main_map.read_backing(a)
                    }
                    a if main_tile_ram_addr(cfg, a).is_some() => {
                        self.board.main_map.read_backing(a)
                    }
                    _ => 0xFF,
                }
            }
            BusMaster::Cpu(1) => match addr {
                0x0000..=0x3FFF | 0x8000..=0x87FF => self.board.sub_map.read_backing(addr),
                a if (cfg.sub_latch_base..cfg.sub_latch_base + LATCH_WINDOW_LEN).contains(&a) => {
                    self.board.sub_read_latch()
                }
                0xC000..=0xC0FF => {
                    // Reading the mux window also clocks the flipscreen latch.
                    self.board.flipscreen = addr & 0x80 != 0;
                    self.board.inputs.read(addr as u8)
                }
                _ => 0xFF,
            },
            _ => return 0xFF,
        };

        match master {
            BusMaster::Cpu(0) => self.board.main_map.watch_read(0, master, addr, data),
            _ => self.board.sub_map.watch_read(1, master, addr, data),
        };
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        let cfg = self.board.variant.map();
        match master {
            BusMaster::Cpu(0) => {
                self.board.main_map.watch_write(0, master, addr, data);
                if (MAIN_LATCH_BASE..MAIN_LATCH_BASE + LATCH_WINDOW_LEN).contains(&addr) {
                    self.board.main_write_latch(data);
                    return;
                }
                if addr == cfg.sub_nmi_addr {
                    self.board.sub_nmi_pending = true;
                    return;
                }
                match addr {
                    a if (cfg.work_ram_start..cfg.work_ram_start + WORK_RAM_LEN as u16)
                        .contains(&a) =>
                    {
                        self.board.main_map.write_backing(a, data)
                    }
                    a if (cfg.sprite_ram_start..cfg.sprite_ram_start + SPRITE_RAM_LEN as u16)
                        .contains(&a) =>
                    {
                        self.board.main_map.write_backing(a, data)
                    }
                    a if main_tile_ram_addr(cfg, a).is_some() => {
                        self.board.main_map.write_backing(a, data)
                    }
                    // ROM, watchdog reset and unmapped space.
                    _ => {}
                }
            }
            BusMaster::Cpu(1) => {
                self.board.sub_map.watch_write(1, master, addr, data);
                if (cfg.sub_latch_base..cfg.sub_latch_base + LATCH_WINDOW_LEN).contains(&addr) {
                    self.board.sub_write_latch(data);
                    return;
                }
                match addr {
                    0x8000..=0x87FF => self.board.sub_map.write_backing(addr, data),
                    0xC000..=0xC0FF => self.board.flipscreen = addr & 0x80 != 0,
                    // Four PSG ports, 0x400 apart.
                    a if (cfg.sn_base..cfg.sn_base + 0x1000).contains(&a)
                        && (a & 0x3FF) == 0
                        && a.wrapping_sub(cfg.sn_base) < 0x1000 =>
                    {
                        let idx = ((a - cfg.sn_base) >> 10) as usize;
                        self.board.sn[idx].write(data);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        self.board.check_interrupts(target)
    }
}

/// Is `addr` inside the tile-code / colour RAM window (including the docastle
/// map's 0x800 mirror)?
fn main_tile_ram_addr(cfg: &MapConfig, addr: u16) -> Option<u16> {
    let len = if cfg.video_ram_mirror { 0x1000 } else { 0x800 };
    (VIDEO_RAM_BASE..VIDEO_RAM_BASE + len)
        .contains(&addr)
        .then_some(addr)
}

impl phosphor_core::core::machine::Renderable for DocastleSystem {
    fn display_size(&self) -> (u32, u32) {
        TIMING.display_size()
    }
    fn display_aspect(&self) -> Option<(u32, u32)> {
        self.variant().display_aspect()
    }
    fn render_frame(&self, buffer: &mut [u8]) {
        self.board.render_frame(buffer);
    }
    fn orientation(&self) -> Orientation {
        self.board.orientation()
    }
}

crate::impl_board_audio!(DocastleSystem, board);
crate::impl_board_debug!(DocastleSystem, board, crate::docastle::TIMING);

impl MachineCore for DocastleSystem {
    fn frame_rate_hz(&self) -> f64 {
        TIMING.frame_rate_hz()
    }

    fn machine_id(&self) -> &str {
        self.variant().id()
    }

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
        bus_split!(self, bus => {
            for _ in 0..crate::docastle::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });
    }

    fn reset(&mut self) {
        self.board.reset();
        bus_split!(self, bus => {
            self.board.main_cpu.reset(bus, BusMaster::Cpu(0));
            self.board.sub_cpu.reset(bus, BusMaster::Cpu(1));
        });
    }
}

impl SaveState for DocastleSystem {
    crate::machine_save_state!();
}

impl phosphor_core::core::machine::Nvram for DocastleSystem {}
impl phosphor_core::core::machine::Profilable for DocastleSystem {}
crate::impl_map_debug_trace!(DocastleSystem, board.main_map);

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------
// JOYS:    b0-b3 = P1 right/up/left/down, b4-b7 = P2 (cocktail).
// BUTTONS: b0/b1 = P1 fire/jump, b3 = Start 1, b4/b5 = P2 fire/jump,
//          b7 = Start 2.
// SYSTEM:  b0 = tilt, b1 = test, b2 = service, b3 = freeze, b4 = coin 2,
//          b5 = coin 1.
// All active-low.

const INPUT_P1_RIGHT: u16 = 0;
const INPUT_P1_LEFT: u16 = 1;
const INPUT_P1_UP: u16 = 2;
const INPUT_P1_DOWN: u16 = 3;
const INPUT_P1_FIRE: u16 = 4;
const INPUT_P1_JUMP: u16 = 5;
const INPUT_P2_RIGHT: u16 = 6;
const INPUT_P2_LEFT: u16 = 7;
const INPUT_P2_UP: u16 = 8;
const INPUT_P2_DOWN: u16 = 9;
const INPUT_P2_FIRE: u16 = 10;
const INPUT_P2_JUMP: u16 = 11;
const INPUT_P1_START: u16 = 12;
const INPUT_P2_START: u16 = 13;
const INPUT_COIN1: u16 = 14;
const INPUT_COIN2: u16 = 15;
const INPUT_SERVICE: u16 = 16;

#[allow(clippy::too_many_arguments)]
const fn dir(
    id: u16,
    name: &'static str,
    label: &'static str,
    direction: Direction,
    player: u8,
    bindings: &'static [DefaultBinding],
) -> InputControl {
    InputControl {
        id: InputId(id),
        stable_name: name,
        label,
        kind: InputKind::DigitalDirection { direction },
        player: Some(player),
        default_bindings: bindings,
    }
}

const fn action(
    id: u16,
    name: &'static str,
    label: &'static str,
    role: ActionRole,
    player: u8,
    bindings: &'static [DefaultBinding],
) -> InputControl {
    InputControl {
        id: InputId(id),
        stable_name: name,
        label,
        kind: InputKind::Action(role),
        player: Some(player),
        default_bindings: bindings,
    }
}

const fn simple(
    id: u16,
    name: &'static str,
    label: &'static str,
    kind: InputKind,
    player: Option<u8>,
    bindings: &'static [DefaultBinding],
) -> InputControl {
    InputControl {
        id: InputId(id),
        stable_name: name,
        label,
        kind,
        player,
        default_bindings: bindings,
    }
}

const DOCASTLE_CONTROLS: &[InputControl] = &[
    dir(
        INPUT_P1_RIGHT,
        "p1_right",
        "P1 Right",
        Direction::Right,
        1,
        ind::P1_RIGHT,
    ),
    dir(
        INPUT_P1_LEFT,
        "p1_left",
        "P1 Left",
        Direction::Left,
        1,
        ind::P1_LEFT,
    ),
    dir(INPUT_P1_UP, "p1_up", "P1 Up", Direction::Up, 1, ind::P1_UP),
    dir(
        INPUT_P1_DOWN,
        "p1_down",
        "P1 Down",
        Direction::Down,
        1,
        ind::P1_DOWN,
    ),
    action(
        INPUT_P1_FIRE,
        "p1_fire",
        "P1 Fire",
        ActionRole::Primary,
        1,
        &[],
    ),
    action(
        INPUT_P1_JUMP,
        "p1_jump",
        "P1 Jump",
        ActionRole::Secondary,
        1,
        &[],
    ),
    dir(
        INPUT_P2_RIGHT,
        "p2_right",
        "P2 Right",
        Direction::Right,
        2,
        ind::P2_RIGHT,
    ),
    dir(
        INPUT_P2_LEFT,
        "p2_left",
        "P2 Left",
        Direction::Left,
        2,
        ind::P2_LEFT,
    ),
    dir(INPUT_P2_UP, "p2_up", "P2 Up", Direction::Up, 2, ind::P2_UP),
    dir(
        INPUT_P2_DOWN,
        "p2_down",
        "P2 Down",
        Direction::Down,
        2,
        ind::P2_DOWN,
    ),
    action(
        INPUT_P2_FIRE,
        "p2_fire",
        "P2 Fire",
        ActionRole::Primary,
        2,
        &[],
    ),
    action(
        INPUT_P2_JUMP,
        "p2_jump",
        "P2 Jump",
        ActionRole::Secondary,
        2,
        &[],
    ),
    simple(
        INPUT_P1_START,
        "p1_start",
        "P1 Start",
        InputKind::Start,
        Some(1),
        ind::P1_START,
    ),
    simple(
        INPUT_P2_START,
        "p2_start",
        "P2 Start",
        InputKind::Start,
        Some(2),
        ind::P2_START,
    ),
    simple(
        INPUT_COIN1,
        "coin1",
        "Coin 1",
        InputKind::Coin,
        None,
        ind::COIN,
    ),
    simple(INPUT_COIN2, "coin2", "Coin 2", InputKind::Coin, None, &[]),
    simple(
        INPUT_SERVICE,
        "service",
        "Service",
        InputKind::Service,
        None,
        ind::SERVICE,
    ),
];

impl InputConfigurable for DocastleSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        DOCASTLE_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let InputEvent::Button { id, pressed } = event else {
            return;
        };
        let inp = &mut self.board.inputs;
        match id.0 {
            INPUT_P1_RIGHT => set_bit_active_low(&mut inp.joys, 0, pressed),
            INPUT_P1_UP => set_bit_active_low(&mut inp.joys, 1, pressed),
            INPUT_P1_LEFT => set_bit_active_low(&mut inp.joys, 2, pressed),
            INPUT_P1_DOWN => set_bit_active_low(&mut inp.joys, 3, pressed),
            INPUT_P2_RIGHT => set_bit_active_low(&mut inp.joys, 4, pressed),
            INPUT_P2_UP => set_bit_active_low(&mut inp.joys, 5, pressed),
            INPUT_P2_LEFT => set_bit_active_low(&mut inp.joys, 6, pressed),
            INPUT_P2_DOWN => set_bit_active_low(&mut inp.joys, 7, pressed),
            INPUT_P1_FIRE => set_bit_active_low(&mut inp.buttons, 0, pressed),
            INPUT_P1_JUMP => set_bit_active_low(&mut inp.buttons, 1, pressed),
            INPUT_P1_START => set_bit_active_low(&mut inp.buttons, 3, pressed),
            INPUT_P2_FIRE => set_bit_active_low(&mut inp.buttons, 4, pressed),
            INPUT_P2_JUMP => set_bit_active_low(&mut inp.buttons, 5, pressed),
            INPUT_P2_START => set_bit_active_low(&mut inp.buttons, 7, pressed),
            INPUT_SERVICE => set_bit_active_low(&mut inp.system, 2, pressed),
            INPUT_COIN2 => set_bit_active_low(&mut inp.system, 4, pressed),
            INPUT_COIN1 => set_bit_active_low(&mut inp.system, 5, pressed),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// DIP switches
// ---------------------------------------------------------------------------

/// docastle DSW1: Easy, rack test off, diamond bonus credit on, EXTRA easy,
/// upright, 3 lives.
const DOCASTLE_DSW1_DEFAULT: u8 = 0xDF;
/// dorunrun DSW1: Easy, demo sounds on, flip off, EXTRA easy, upright, special
/// given, 3 lives.
const DORUNRUN_DSW1_DEFAULT: u8 = 0xDF;
/// dowild DSW1: Easy, rack test off, flip off, EXTRA easy, upright, special
/// given, 3 lives.
const DOWILD_DSW1_DEFAULT: u8 = 0xDF;
/// DSW2: 1 coin / 1 credit on both slots.
const DSW2_DEFAULT: u8 = 0xFF;

const fn choice(label: &'static str, value: u8) -> DipChoice {
    DipChoice { label, value }
}

/// Coinage choices, shared by Coin B (low nibble, `shift = 0`) and Coin A
/// (high nibble, `shift = 4`).
const fn coinage(shift: u8) -> [DipChoice; 11] {
    [
        choice("4 Coins/1 Credit", 0x06 << shift),
        choice("3 Coins/1 Credit", 0x08 << shift),
        choice("2 Coins/1 Credit", 0x0a << shift),
        choice("3 Coins/2 Credits", 0x07 << shift),
        choice("1 Coin/1 Credit", 0x0f << shift),
        choice("2 Coins/3 Credits", 0x09 << shift),
        choice("1 Coin/2 Credits", 0x0e << shift),
        choice("1 Coin/3 Credits", 0x0d << shift),
        choice("1 Coin/4 Credits", 0x0c << shift),
        choice("1 Coin/5 Credits", 0x0b << shift),
        choice("Free Play", 0x00 << shift),
    ]
}

const COIN_B_CHOICES: [DipChoice; 11] = coinage(0);
const COIN_A_CHOICES: [DipChoice; 11] = coinage(4);

const DIFFICULTY_OPTION: DipOption = DipOption {
    name: "Difficulty",
    mask: 0x03,
    apply: DipApplyTiming::Immediate,
    choices: &[
        choice("Easy", 0x03),
        choice("Medium", 0x02),
        choice("Hard", 0x01),
        choice("Hardest", 0x00),
    ],
};

const EXTRA_OPTION: DipOption = DipOption {
    name: "Difficulty of EXTRA",
    mask: 0x10,
    apply: DipApplyTiming::Immediate,
    choices: &[choice("Easy", 0x10), choice("Difficult", 0x00)],
};

const CABINET_OPTION: DipOption = DipOption {
    name: "Cabinet",
    mask: 0x20,
    apply: DipApplyTiming::Immediate,
    choices: &[choice("Upright", 0x00), choice("Cocktail", 0x20)],
};

const RACK_TEST_OPTION: DipOption = DipOption {
    name: "Rack Test (Cheat)",
    mask: 0x04,
    apply: DipApplyTiming::Immediate,
    choices: &[choice("Off", 0x04), choice("On", 0x00)],
};

const FLIP_SCREEN_OPTION: DipOption = DipOption {
    name: "Flip Screen",
    mask: 0x08,
    apply: DipApplyTiming::Immediate,
    choices: &[choice("Off", 0x08), choice("On", 0x00)],
};

const SPECIAL_OPTION: DipOption = DipOption {
    name: "Special",
    mask: 0x40,
    apply: DipApplyTiming::Immediate,
    choices: &[choice("Given", 0x40), choice("Not Given", 0x00)],
};

const LIVES_3_5_OPTION: DipOption = DipOption {
    name: "Lives",
    mask: 0x80,
    apply: DipApplyTiming::Immediate,
    choices: &[choice("3", 0x80), choice("5", 0x00)],
};

const COINAGE_BANK: DipSwitchBank = DipSwitchBank {
    name: "DSW2",
    options: &[
        DipOption {
            name: "Coin B",
            mask: 0x0f,
            apply: DipApplyTiming::Immediate,
            choices: &COIN_B_CHOICES,
        },
        DipOption {
            name: "Coin A",
            mask: 0xf0,
            apply: DipApplyTiming::Immediate,
            choices: &COIN_A_CHOICES,
        },
    ],
};

const DOCASTLE_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSW1",
        options: &[
            DIFFICULTY_OPTION,
            RACK_TEST_OPTION,
            DipOption {
                name: "Bonus Credit for Diamond",
                mask: 0x08,
                apply: DipApplyTiming::Immediate,
                choices: &[choice("Yes", 0x08), choice("No", 0x00)],
            },
            EXTRA_OPTION,
            CABINET_OPTION,
            DipOption {
                name: "Lives",
                mask: 0xc0,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    choice("2", 0x00),
                    choice("3", 0xc0),
                    choice("4", 0x80),
                    choice("5", 0x40),
                ],
            },
        ],
    },
    COINAGE_BANK,
];

const DORUNRUN_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSW1",
        options: &[
            DIFFICULTY_OPTION,
            DipOption {
                name: "Demo Sounds",
                mask: 0x04,
                apply: DipApplyTiming::Immediate,
                choices: &[choice("On", 0x04), choice("Off", 0x00)],
            },
            FLIP_SCREEN_OPTION,
            EXTRA_OPTION,
            CABINET_OPTION,
            SPECIAL_OPTION,
            LIVES_3_5_OPTION,
        ],
    },
    COINAGE_BANK,
];

const DOWILD_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSW1",
        options: &[
            DIFFICULTY_OPTION,
            RACK_TEST_OPTION,
            FLIP_SCREEN_OPTION,
            EXTRA_OPTION,
            CABINET_OPTION,
            SPECIAL_OPTION,
            LIVES_3_5_OPTION,
        ],
    },
    COINAGE_BANK,
];

impl DipSwitches for DocastleSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        self.variant().dip_banks()
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        match bank {
            0 => self.board.inputs.dsw1,
            1 => self.board.inputs.dsw2,
            _ => 0,
        }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        match bank {
            0 => self.board.inputs.dsw1 = value,
            1 => self.board.inputs.dsw2 = value,
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

fn create_variant(
    variant: DocastleVariant,
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = DocastleSystem::new(variant);
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
MachineEntry::new("docastle", &["docastle"], |rs| create_variant(DocastleVariant::Docastle, rs), DOCASTLE_CONTROLS) }
inventory::submit! {
MachineEntry::new("dorunrun", &["dorunrun"], |rs| create_variant(DocastleVariant::Dorunrun, rs), DOCASTLE_CONTROLS) }
inventory::submit! {
MachineEntry::new("dowild", &["dowild"], |rs| create_variant(DocastleVariant::Dowild, rs), DOCASTLE_CONTROLS) }

inventory::submit! {
    DisasmRegion {
        machine: "docastle",
        region: "main",
        cpu: DisasmCpu::Z80,
        org: 0x0000,
        size: DOCASTLE_MAIN_ROM.size as u32,
        load: |rs| DOCASTLE_MAIN_ROM.load(rs),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "docastle",
        region: "sub",
        cpu: DisasmCpu::Z80,
        org: 0x0000,
        size: DOCASTLE_SUB_ROM.size as u32,
        load: |rs| DOCASTLE_SUB_ROM.load(rs),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "dorunrun",
        region: "main",
        cpu: DisasmCpu::Z80,
        org: 0x0000,
        size: DORUNRUN_MAIN_ROM.size as u32,
        load: |rs| DORUNRUN_MAIN_ROM.load(rs),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "dorunrun",
        region: "sub",
        cpu: DisasmCpu::Z80,
        org: 0x0000,
        size: DORUNRUN_SUB_ROM.size as u32,
        load: |rs| DORUNRUN_SUB_ROM.load(rs),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "dowild",
        region: "main",
        cpu: DisasmCpu::Z80,
        org: 0x0000,
        size: DOWILD_MAIN_ROM.size as u32,
        load: |rs| DOWILD_MAIN_ROM.load(rs),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "dowild",
        region: "sub",
        cpu: DisasmCpu::Z80,
        org: 0x0000,
        size: DOWILD_SUB_ROM.size as u32,
        load: |rs| DOWILD_SUB_ROM.load(rs),
    }
}

// ---------------------------------------------------------------------------
// GFX viewer regions
// ---------------------------------------------------------------------------

fn variant_gfx_palette(
    prom: &'static RomRegion,
    rom_set: &RomSet,
) -> Result<Vec<(u8, u8, u8)>, RomLoadError> {
    let data = prom.load(rom_set)?;
    Ok(docastle_palette_rgb(&data[..0x100]).to_vec())
}

macro_rules! gfx_regions {
    ($machine:literal, $tiles:ident, $sprites:ident, $prom:ident) => {
        inventory::submit! {
            GfxRegion {
                machine: $machine,
                region: "tiles",
                count: TILE_COUNT as u32,
                width: 8,
                height: 8,
                layout: &DOCASTLE_TILE_LAYOUT,
                load: |rs| $tiles.load(rs),
                palette: Some(|rs| variant_gfx_palette(&$prom, rs)),
            }
        }
        inventory::submit! {
            GfxRegion {
                machine: $machine,
                region: "sprites",
                count: SPRITE_COUNT as u32,
                width: 16,
                height: 16,
                layout: &DOCASTLE_SPRITE_LAYOUT,
                load: |rs| $sprites.load(rs),
                palette: Some(|rs| variant_gfx_palette(&$prom, rs)),
            }
        }
    };
}

gfx_regions!(
    "docastle",
    DOCASTLE_TILE_ROM,
    DOCASTLE_SPRITE_ROM,
    DOCASTLE_PALETTE_PROM
);
gfx_regions!(
    "dorunrun",
    DORUNRUN_TILE_ROM,
    DORUNRUN_SPRITE_ROM,
    DORUNRUN_PALETTE_PROM
);
gfx_regions!(
    "dowild",
    DOWILD_TILE_ROM,
    DOWILD_SPRITE_ROM,
    DOWILD_PALETTE_PROM
);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::Renderable;

    const ALL: [DocastleVariant; 3] = [
        DocastleVariant::Docastle,
        DocastleVariant::Dorunrun,
        DocastleVariant::Dowild,
    ];

    #[test]
    fn machines_are_registered() {
        for v in ALL {
            assert!(
                crate::registry::find(v.id()).is_some(),
                "{} not registered",
                v.id()
            );
        }
    }

    #[test]
    fn disasm_regions_registered() {
        for v in ALL {
            assert!(crate::disasm_registry::find(v.id(), "main").is_some());
            assert!(crate::disasm_registry::find(v.id(), "sub").is_some());
        }
    }

    #[test]
    fn gfx_regions_registered_with_expected_geometry() {
        for v in ALL {
            let regions = crate::gfx_registry::regions_for(v.id());
            assert_eq!(regions.len(), 2, "{}", v.id());
            let tiles = regions.iter().find(|r| r.region == "tiles").unwrap();
            assert_eq!((tiles.count, tiles.width, tiles.height), (512, 8, 8));
            let spr = regions.iter().find(|r| r.region == "sprites").unwrap();
            assert_eq!((spr.count, spr.width, spr.height), (256, 16, 16));
        }
    }

    #[test]
    fn timing_is_sane() {
        assert_eq!(TIMING.cpu_clock_hz, 4_000_000);
        let hz = TIMING.frame_rate_hz();
        assert!((59.0..60.5).contains(&hz), "frame rate {hz} out of range");
        assert_eq!(TIMING.display_size(), (240, 192));
    }

    #[test]
    fn orientation_and_aspect_follow_the_variant() {
        let castle = DocastleSystem::new(DocastleVariant::Docastle);
        assert_eq!(castle.orientation(), Orientation::ROT270);
        assert_eq!(castle.display_aspect(), Some((3, 4)));

        let runrun = DocastleSystem::new(DocastleVariant::Dorunrun);
        assert_eq!(runrun.orientation(), Orientation::NORMAL);
        assert_eq!(runrun.display_aspect(), Some((4, 3)));
    }

    #[test]
    fn variant_maps_decode_program_rom_windows() {
        // docastle: contiguous ROM through 0x7FFF, RAM from 0x8000.
        let mut castle = DocastleSystem::new(DocastleVariant::Docastle);
        castle
            .board
            .main_map
            .load_region_at(MainRegion::RomLow, 0x7FFF, &[0xAB]);
        assert_eq!(castle.read(BusMaster::Cpu(0), 0x7FFF), 0xAB);
        castle.write(BusMaster::Cpu(0), 0x8000, 0x5A);
        assert_eq!(castle.read(BusMaster::Cpu(0), 0x8000), 0x5A);

        // dorunrun: ROM is split around a RAM/sprite hole at 0x2000-0x3FFF.
        let mut runrun = DocastleSystem::new(DocastleVariant::Dorunrun);
        runrun
            .board
            .main_map
            .load_region_at(MainRegion::RomHigh, 0, &[0xCD]);
        assert_eq!(runrun.read(BusMaster::Cpu(0), 0x4000), 0xCD);
        runrun.write(BusMaster::Cpu(0), 0x2000, 0x33);
        assert_eq!(runrun.read(BusMaster::Cpu(0), 0x2000), 0x33);
        runrun.write(BusMaster::Cpu(0), 0x3800, 0x44);
        assert_eq!(
            runrun.board.main_map.region_data(MainRegion::SpriteRam)[0],
            0x44
        );
    }

    #[test]
    fn docastle_video_ram_is_mirrored() {
        let mut sys = DocastleSystem::new(DocastleVariant::Docastle);
        sys.write(BusMaster::Cpu(0), 0xB000, 0x77);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xB800), 0x77);
        sys.write(BusMaster::Cpu(0), 0xBC00, 0x99);
        assert_eq!(
            sys.board.main_map.region_data(MainRegion::ColorRam)[0],
            0x99,
            "0xBC00 mirrors colour RAM"
        );
    }

    #[test]
    fn gfx_decode_produces_populated_caches() {
        let mut board = DocastleBoard::new(DocastleVariant::Docastle);
        // Packed MSB: byte 0 holds pixels 0 and 1 as high and low nibble.
        board.tile_rom[0] = 0x9C;
        board.sprite_rom[0] = 0x9C;
        board.decode_gfx_roms();
        assert_eq!(board.tile_cache.count(), 512);
        assert_eq!(board.sprite_cache.count(), 256);
        assert_eq!(board.tile_cache.pixel(0, 0, 0), 0x9);
        assert_eq!(board.tile_cache.pixel(0, 1, 0), 0xC);
        assert_eq!(board.sprite_cache.pixel(0, 0, 0), 0x9);
        assert_eq!(board.sprite_cache.pixel(0, 1, 0), 0xC);
    }

    #[test]
    fn palette_duplicates_each_prom_entry_across_the_mask_bit() {
        let mut prom = [0u8; 0x100];
        // Full red (bits 7-5), no green, full blue (bits 1-0).
        prom[0] = 0b1110_0011;
        let pal = docastle_palette_rgb(&prom);
        assert_eq!(pal.len(), 512);
        let expected = (0x23 + 0x4b + 0x91, 0, 0x52 + 0xad);
        assert_eq!(pal[0x000], (expected.0 as u8, 0, expected.2 as u8));
        // Pen bit 3 is the transparency flag, not a colour bit.
        assert_eq!(pal[0x008], pal[0x000]);
        // PROM entry 8 is colour code 1, pen 0 → palette index 0x10.
        prom[8] = 0b0001_1100;
        let pal = docastle_palette_rgb(&prom);
        assert_eq!(pal[0x010], pal[0x018]);
        assert_ne!(pal[0x010], (0, 0, 0));
    }

    #[test]
    fn tilemap_renders_through_the_palette() {
        let mut sys = DocastleSystem::new(DocastleVariant::Docastle);
        // Tile 0, every pixel value 0xF.
        sys.board.tile_rom[..32].fill(0xFF);
        sys.board.decode_gfx_roms();
        // Colour code 1, pen 0xF → palette index 1*16 + 15 = 0x1F.
        sys.board.palette_rgb[0x1F] = (10, 20, 30);
        // Visible pixel (0, 0) is raster (8, 0) → tilemap (8, 32) → tile
        // index row 4, column 1.
        let idx = 4 * 32 + 1;
        sys.board.main_map.region_data_mut(MainRegion::VideoRam)[idx] = 0;
        sys.board.main_map.region_data_mut(MainRegion::ColorRam)[idx] = 0x01;

        let mut buf = vec![0u8; VISIBLE_WIDTH * VISIBLE_HEIGHT * 3];
        sys.board.render_frame(&mut buf);
        assert_eq!((buf[0], buf[1], buf[2]), (10, 20, 30));
    }

    #[test]
    fn sprite_pen_15_masks_without_drawing() {
        let mut sys = DocastleSystem::new(DocastleVariant::Docastle);
        // Sprite 0 = solid pen 0xE (drawn), sprite 1 = solid pen 0xF (mask).
        sys.board.sprite_rom[..128].fill(0xEE);
        sys.board.sprite_rom[128..256].fill(0xFF);
        sys.board.decode_gfx_roms();
        sys.board.palette_rgb[0x0E] = (1, 2, 3);
        sys.board.palette_rgb[0x0F] = (4, 5, 6);

        {
            let spr = sys.board.main_map.region_data_mut(MainRegion::SpriteRam);
            // Entry 0 (drawn last, on top): pen-14 sprite at raster (8, 0).
            spr[0] = 32; // sy = 0
            spr[1] = 8; // sx = 8
            spr[2] = 0x00; // colour 0
            spr[3] = 0x00; // code 0
            // Entry 1 (drawn first): pen-15 mask over the same spot.
            spr[4] = 32;
            spr[5] = 8;
            spr[6] = 0x00;
            spr[7] = 0x01;
        }

        let mut buf = vec![0u8; VISIBLE_WIDTH * VISIBLE_HEIGHT * 3];
        sys.board.render_frame(&mut buf);
        // The mask claimed the pixel first, so the pen-14 sprite behind it does
        // not paint — and pen 15 never paints either.
        assert_eq!((buf[0], buf[1], buf[2]), (0, 0, 0));

        // Remove the mask: now the visible sprite paints.
        sys.board.main_map.region_data_mut(MainRegion::SpriteRam)[4] = 0;
        sys.board.render_frame(&mut buf);
        assert_eq!((buf[0], buf[1], buf[2]), (1, 2, 3));
    }

    #[test]
    fn front_tilemap_pass_uses_the_variant_transmask() {
        // docastle draws tile pens 8-15 in front of sprites; dorunrun draws
        // pens 0-7 instead.
        for (variant, front_val, front_pen) in [
            (DocastleVariant::Docastle, 0xFFu8, 0x0Fusize),
            (DocastleVariant::Dorunrun, 0x77u8, 0x07usize),
        ] {
            let mut sys = DocastleSystem::new(variant);
            sys.board.tile_rom[..32].fill(front_val);
            sys.board.sprite_rom[..128].fill(0xEE);
            sys.board.decode_gfx_roms();
            sys.board.palette_rgb[front_pen] = (200, 0, 0);
            sys.board.palette_rgb[0x0E] = (0, 200, 0);

            let idx = 4 * 32 + 1;
            sys.board.main_map.region_data_mut(MainRegion::ColorRam)[idx] = 0x00;
            {
                let spr = sys.board.main_map.region_data_mut(MainRegion::SpriteRam);
                spr[0] = 32;
                spr[1] = 8;
                spr[2] = 0x00;
                spr[3] = 0x00;
            }

            let mut buf = vec![0u8; VISIBLE_WIDTH * VISIBLE_HEIGHT * 3];
            sys.board.render_frame(&mut buf);
            assert_eq!(
                (buf[0], buf[1], buf[2]),
                (200, 0, 0),
                "{} tile should sit in front of the sprite",
                variant.id()
            );
        }
    }

    #[test]
    fn input_mux_returns_the_previously_selected_port() {
        let mut sys = DocastleSystem::new(DocastleVariant::Docastle);
        sys.board.inputs.dsw1 = 0xA5;
        sys.board.inputs.dsw2 = 0x5A;

        // First read selects DSW2 but returns the (tri-stated) held value.
        sys.read(BusMaster::Cpu(1), 0xC001);
        // Second read returns DSW2 and selects DSW1.
        assert_eq!(sys.read(BusMaster::Cpu(1), 0xC002), 0x5A);
        // Third read returns DSW1.
        assert_eq!(sys.read(BusMaster::Cpu(1), 0xC000), 0xA5);
    }

    #[test]
    fn input_window_bit7_drives_flipscreen() {
        let mut sys = DocastleSystem::new(DocastleVariant::Docastle);
        assert!(!sys.board.flipscreen);
        sys.read(BusMaster::Cpu(1), 0xC083);
        assert!(sys.board.flipscreen, "address bit 7 sets flip");
        sys.read(BusMaster::Cpu(1), 0xC003);
        assert!(!sys.board.flipscreen, "address bit 7 clear releases flip");
    }

    #[test]
    fn joystick_and_coin_bits_are_active_low() {
        let mut sys = DocastleSystem::new(DocastleVariant::Docastle);
        assert_eq!(sys.board.inputs.joys, 0xFF);
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_P1_RIGHT),
            pressed: true,
        });
        assert_eq!(sys.board.inputs.joys & 0x01, 0);
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_COIN1),
            pressed: true,
        });
        assert_eq!(sys.board.inputs.system & 0x20, 0);
    }

    #[test]
    fn sub_psg_writes_land_on_the_variant_ports() {
        // docastle: 0xE000/0xE400/0xE800/0xEC00.
        let mut castle = DocastleSystem::new(DocastleVariant::Docastle);
        for (i, addr) in [0xE000u16, 0xE400, 0xE800, 0xEC00].into_iter().enumerate() {
            castle.write(BusMaster::Cpu(1), addr, 0x80 | 0x05);
            assert!(!castle.board.sn[i].is_ready(), "PSG {i} took the write");
        }
        // dorunrun: 0xA000/0xA400/0xA800/0xAC00.
        let mut runrun = DocastleSystem::new(DocastleVariant::Dorunrun);
        for (i, addr) in [0xA000u16, 0xA400, 0xA800, 0xAC00].into_iter().enumerate() {
            runrun.write(BusMaster::Cpu(1), addr, 0x80 | 0x05);
            assert!(!runrun.board.sn[i].is_ready(), "PSG {i} took the write");
        }
    }

    #[test]
    fn psg_ready_stalls_the_sub_cpu() {
        let mut sys = DocastleSystem::new(DocastleVariant::Docastle);
        sys.write(BusMaster::Cpu(1), 0xE000, 0x9F);
        let pc = sys.board.sub_cpu.pc;
        bus_split!(&mut sys, bus => {
            for _ in 0..8 {
                sys.board.tick(bus);
            }
        });
        assert_eq!(sys.board.sub_cpu.pc, pc, "sub CPU held while READY is low");
    }

    #[test]
    fn main_latch_write_stalls_until_the_sub_cpu_reads() {
        let mut sys = DocastleSystem::new(DocastleVariant::Docastle);
        sys.write(BusMaster::Cpu(0), 0xA000, 0x42);
        assert!(sys.board.main_wait, "a latch write asserts main WAIT");
        assert_eq!(sys.board.shared_latch, 0x42);
        assert_eq!(sys.read(BusMaster::Cpu(1), 0xA000), 0x42);
        assert!(!sys.board.main_wait, "the sub CPU released WAIT");
    }

    #[test]
    fn main_latch_read_stalls_then_samples_the_sub_cpu_value() {
        let mut sys = DocastleSystem::new(DocastleVariant::Docastle);
        sys.board.shared_latch = 0x11;

        // First attempt: stalls and asks to be retried.
        let stale = sys.read(BusMaster::Cpu(0), 0xA000);
        assert_eq!(stale, 0x11);
        assert!(sys.board.main_wait);
        assert!(sys.board.main_retry, "the stalled read must be re-run");

        // The sub CPU supplies the byte and releases WAIT.
        sys.write(BusMaster::Cpu(1), 0xA000, 0x99);
        assert!(!sys.board.main_wait);

        // The retry samples the value the sub CPU just wrote.
        sys.board.main_retry = false;
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xA000), 0x99);
        assert!(!sys.board.main_wait, "the retry does not re-stall");
        assert!(!sys.board.main_retry);
    }

    #[test]
    fn stalled_main_cpu_makes_no_progress_until_the_sub_cpu_answers() {
        let mut sys = DocastleSystem::new(DocastleVariant::Docastle);
        // LD A,(0xA000) at reset — the read stalls mid-instruction.
        sys.board
            .main_map
            .load_region_at(MainRegion::RomLow, 0, &[0x3A, 0x00, 0xA0, 0x76]);
        sys.reset();

        bus_split!(&mut sys, bus => {
            for _ in 0..64 {
                sys.board.tick(bus);
            }
        });
        assert!(sys.board.main_wait, "main CPU is parked on the latch read");
        let parked_pc = sys.board.main_cpu.pc;

        bus_split!(&mut sys, bus => {
            for _ in 0..64 {
                sys.board.tick(bus);
            }
        });
        assert_eq!(sys.board.main_cpu.pc, parked_pc, "no forward progress");

        // Answer from the sub side; the main CPU resumes and reads that byte.
        sys.board.sub_write_latch(0x7E);
        bus_split!(&mut sys, bus => {
            for _ in 0..64 {
                sys.board.tick(bus);
            }
        });
        assert_eq!(sys.board.main_cpu.a, 0x7E);
    }

    /// The real boot handshake: the main CPU pulses the sub CPU's NMI, then
    /// `LDIR`s nine bytes into the latch and nine back out, while the sub CPU's
    /// NMI handler `LDIR`s nine out and nine in. Every byte has to survive in
    /// both directions — the two block copies only stay paired if each CPU
    /// spends exactly 21 cycles per byte, so a single cycle of drift on either
    /// side drops half the transfer.
    #[test]
    fn dual_ldir_block_transfer_survives_in_both_directions() {
        const SENT: [u8; 9] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99];
        const REPLY: [u8; 9] = [0xA1, 0xB2, 0xC3, 0xD4, 0xE5, 0xF6, 0x07, 0x18, 0x29];

        let mut sys = DocastleSystem::new(DocastleVariant::Docastle);

        #[rustfmt::skip]
        let main_prog = [
            0x3E, 0x00,             // LD   A,$00
            0x32, 0x00, 0xE0,       // LD   ($E000),A   ; pulse the sub CPU's NMI
            0x21, 0x00, 0x90,       // LD   HL,$9000
            0x11, 0x00, 0xA0,       // LD   DE,$A000
            0x01, 0x09, 0x00,       // LD   BC,$0009
            0xED, 0xB0,             // LDIR             ; main -> latch
            0x21, 0x00, 0xA0,       // LD   HL,$A000
            0x11, 0x10, 0x90,       // LD   DE,$9010
            0x01, 0x09, 0x00,       // LD   BC,$0009
            0xED, 0xB0,             // LDIR             ; latch -> main
            0x76,                   // HALT
        ];
        sys.board
            .main_map
            .load_region_at(MainRegion::RomLow, 0, &main_prog);

        #[rustfmt::skip]
        let sub_reset = [
            0x31, 0x00, 0x88,       // LD   SP,$8800
            0xC3, 0x00, 0x01,       // JP   $0100
        ];
        #[rustfmt::skip]
        let sub_nmi = [
            0x21, 0x00, 0xA0,       // LD   HL,$A000
            0x11, 0x00, 0x80,       // LD   DE,$8000
            0x01, 0x09, 0x00,       // LD   BC,$0009
            0xED, 0xB0,             // LDIR             ; latch -> sub
            0x21, 0x00, 0x81,       // LD   HL,$8100
            0x11, 0x00, 0xA0,       // LD   DE,$A000
            0x01, 0x09, 0x00,       // LD   BC,$0009
            0xED, 0xB0,             // LDIR             ; sub -> latch
            0xED, 0x45,             // RETN
        ];
        sys.board
            .sub_map
            .load_region_at(SubRegion::Rom, 0, &sub_reset);
        sys.board
            .sub_map
            .load_region_at(SubRegion::Rom, 0x66, &sub_nmi);
        sys.board
            .sub_map
            .load_region_at(SubRegion::Rom, 0x100, &[0x18, 0xFE]); // JR $ (spin)

        sys.reset();
        sys.board.main_map.region_data_mut(MainRegion::WorkRam)[0x1000..0x1009]
            .copy_from_slice(&SENT); // 0x9000
        sys.board.sub_map.region_data_mut(SubRegion::Ram)[0x100..0x109].copy_from_slice(&REPLY);

        bus_split!(&mut sys, bus => {
            for _ in 0..4000 {
                sys.board.tick(bus);
            }
        });

        assert!(sys.board.main_cpu.halted, "main CPU never finished");
        assert_eq!(
            &sys.board.sub_map.region_data(SubRegion::Ram)[..9],
            &SENT,
            "main -> sub bytes"
        );
        assert_eq!(
            &sys.board.main_map.region_data(MainRegion::WorkRam)[0x1010..0x1019],
            &REPLY,
            "sub -> main bytes"
        );
    }

    #[test]
    fn nmi_trigger_address_pulses_the_sub_cpu() {
        for (variant, addr) in [
            (DocastleVariant::Docastle, 0xE000u16),
            (DocastleVariant::Dorunrun, 0xB800),
        ] {
            let mut sys = DocastleSystem::new(variant);
            assert!(!sys.board.sub_nmi_pending);
            sys.write(BusMaster::Cpu(0), addr, 0x00);
            assert!(sys.board.sub_nmi_pending, "{}", variant.id());
            // The pulse is consumed by a single interrupt check.
            assert!(sys.check_interrupts(BusMaster::Cpu(1)).nmi);
            assert!(!sys.check_interrupts(BusMaster::Cpu(1)).nmi);
        }
    }

    #[test]
    fn sub_irq_fires_eight_times_per_frame() {
        let mut sys = DocastleSystem::new(DocastleVariant::Docastle);
        let mut count = 0;
        bus_split!(&mut sys, bus => {
            for _ in 0..TIMING.cycles_per_frame() {
                sys.board.tick(bus);
                // Stand in for the CPU acknowledging the auto-clearing line, so
                // each assertion shows up as its own edge.
                if sys.board.sub_irq_pending {
                    sys.board.sub_irq_pending = false;
                    count += 1;
                }
            }
        });
        assert_eq!(count, 8, "one rising edge per CRTC MA6 transition");
    }

    #[test]
    fn sound_produces_non_silent_audio() {
        let mut sys = DocastleSystem::new(DocastleVariant::Docastle);
        for byte in [0x8Eu8, 0x0F, 0x90] {
            sys.write(BusMaster::Cpu(1), 0xE000, byte);
        }
        sys.run_frame();
        let mut buf = vec![0i16; 2048];
        let n = sys.board.fill_audio(&mut buf);
        assert!(n > 0, "resampler produced no samples for a frame");
        assert!(buf[..n].iter().any(|&s| s != 0), "expected audible output");
    }

    #[test]
    fn dip_banks_are_valid() {
        crate::assert_dip_banks_valid(DOCASTLE_DIP_BANKS, &[DOCASTLE_DSW1_DEFAULT, DSW2_DEFAULT]);
        crate::assert_dip_banks_valid(DORUNRUN_DIP_BANKS, &[DORUNRUN_DSW1_DEFAULT, DSW2_DEFAULT]);
        crate::assert_dip_banks_valid(DOWILD_DIP_BANKS, &[DOWILD_DSW1_DEFAULT, DSW2_DEFAULT]);
    }

    #[test]
    fn dip_defaults_match_the_factory_settings() {
        let sys = DocastleSystem::new(DocastleVariant::Docastle);
        // Easy difficulty, upright cabinet, 3 lives, 1 coin / 1 credit.
        assert_eq!(sys.dip_bank_value(0) & 0x03, 0x03);
        assert_eq!(sys.dip_bank_value(0) & 0x20, 0x00);
        assert_eq!(sys.dip_bank_value(0) & 0xc0, 0xc0);
        assert_eq!(sys.dip_bank_value(1), 0xFF);

        let wild = DocastleSystem::new(DocastleVariant::Dowild);
        // Bit 7 alone is Lives on this variant; 3 lives is the default.
        assert_eq!(wild.dip_bank_value(0) & 0x80, 0x80);
    }

    #[test]
    fn boots_and_runs_frames_without_panicking() {
        for v in ALL {
            let mut sys = DocastleSystem::new(v);
            sys.reset();
            for _ in 0..3 {
                sys.run_frame();
            }
            assert_eq!(
                sys.display_size(),
                (VISIBLE_WIDTH as u32, VISIBLE_HEIGHT as u32)
            );
            let mut buf = vec![0u8; VISIBLE_WIDTH * VISIBLE_HEIGHT * 3];
            sys.board.render_frame(&mut buf);
        }
    }
}
