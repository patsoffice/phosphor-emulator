//! Sega Congo Bongo (1983) — Zaxxon-family hardware.
//!
//! Hardware (per MAME `src/mame/sega/zaxxon.cpp`, the `congo` set):
//! - Main CPU: Z80 @ MASTER_CLOCK/16 = 48.66 MHz / 16 ≈ 3.041 MHz
//! - Sound CPU: Z80 @ 4 MHz, with a ~244 Hz periodic IRQ (`SOUND_CLOCK/16/16/16/4`)
//! - Video: 256×224 raster, ROT90 (portrait 224×256), VBlank IRQ on the main CPU
//! - Foreground: 32×32 8×8 2bpp tilemap + per-tile color RAM, fg-bank/color-bank latches
//! - Background: 8×8 3bpp `tilemap_dat` map, 11-bit scroll with an isometric skew
//! - Sprites: 32×32 3bpp, moved into a 256-byte sprite RAM by a custom DMA engine
//! - Sound: 2× SN76489A + i8255 PPI driving 5 synthesized percussion voices
//!
//! Status: the main CPU, memory maps, GFX decode/palette, and the full video
//! pipeline (scrolling background → sprites → foreground tilemap) are
//! implemented. Still to come: refined input + DIP options (issue `.8`) and the
//! entire sound path — sound Z80, 2× SN76489A, i8255 PPI, and discrete
//! percussion (issues `.9`–`.10`).

use phosphor_core::audio::AudioResampler;
use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::DebugTraceBuffer;
use phosphor_core::core::machine::{
    ActionRole, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, DipSwitches, Direction,
    InputConfigurable, InputControl, InputEvent, InputId, InputKind, MachineCore, Nvram, SaveState,
};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, ClockDivider, TimingConfig};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::z80::Z80;
use phosphor_core::device::i8255::I8255;
use phosphor_core::device::sn76489::Sn76489a;
use phosphor_core::gfx;
use phosphor_core::gfx::decode::{GfxLayout, decode_gfx};
use phosphor_core::gfx::resistor::{combine_weights, compute_resistor_weights};
use phosphor_core::gfx::sprite::{SpriteClip, draw_sprite_row};
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion, Saveable};

use crate::congo_sound::CongoSound;
use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::gfx_registry::GfxRegion;
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_high;

// ---------------------------------------------------------------------------
// Memory map region IDs
// ---------------------------------------------------------------------------

/// Main CPU (Z80) address space regions.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum MainRegion {
    Rom = 1,      // 0x0000-0x7FFF (32KB program ROM)
    Ram = 2,      // 0x8000-0x8FFF (4KB work RAM)
    VideoRam = 3, // 0xA000-0xA3FF (1KB tilemap RAM, mirrored at 0xA800)
    ColorRam = 4, // 0xA400-0xA7FF (1KB color RAM, mirrored at 0xAC00)
    Io = 5,       // 0xC000-0xDFFF (I/O ports, heavily mirrored)
}

/// Sound CPU (Z80) address space regions.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum SoundRegion {
    Rom = 1, // 0x0000-0x1FFF (8KB sound program ROM)
    Ram = 2, // 0x4000-0x47FF (2KB work RAM, mirrored at 0x4800)
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------
// Master clock 48.66 MHz; main Z80 = /16 ≈ 3.041 MHz; pixel clock = /8 ≈ 6.083 MHz.
// HTOTAL 384 px → 192 main-CPU cycles/scanline (pixel clock is 2× the CPU clock).
// VTOTAL 264 lines; visible Y 16..239 (224 lines), VBLANK at line 240.
// Frame: 192 × 264 = 50688 cycles → ≈ 59.99 Hz. ROT90 ⇒ display is 224×256.

pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 48_660_000 / 16, // 3_041_250
    cycles_per_scanline: 192,
    total_scanlines: 264,
    display_width: (NATIVE_HEIGHT - VBLANK_END) as u32, // 224 (rotated)
    display_height: NATIVE_WIDTH as u32,                // 256 (rotated)
};

pub const NATIVE_WIDTH: usize = 256;
pub const NATIVE_HEIGHT: usize = 240;
pub const VBLANK_END: usize = 16; // first visible scanline
pub const VISIBLE_LINES: u64 = 240; // lines rendered (top VBLANK_END clipped on output)

// Sound section: a second Z80 @ 4 MHz with two SN76489A PSGs (4 MHz and 1 MHz)
// and an i8255 PPI. The sound CPU takes a periodic IRQ at SOUND_CLOCK/16/16/16/4
// ≈ 244 Hz (one IRQ every 16384 sound cycles).
pub const SOUND_CLOCK: u64 = 4_000_000;
pub const SOUND_PSG2_CLOCK: u32 = 1_000_000;
pub const SOUND_IRQ_PERIOD: u64 = 16 * 16 * 16 * 4; // 16384 sound cycles
pub const OUTPUT_SAMPLE_RATE: u64 = 44_100;

// ---------------------------------------------------------------------------
// ROM definitions ("congo" parent set — 2-board stack, Sega ID 834-5180)
// ---------------------------------------------------------------------------

/// Main Z80 program ROM at 0x0000-0x7FFF (four 8KB chips).
pub static CONGO_MAIN_ROM: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        RomEntry {
            name: "congo_rev_c_rom1.u21",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x09355b5b],
        },
        RomEntry {
            name: "congo_rev_c_rom2a.u22",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x1c5e30ae],
        },
        RomEntry {
            name: "congo_rev_c_rom3.u23",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x5ee1132c],
        },
        RomEntry {
            name: "congo_rev_c_rom4.u24",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0x5332b9bf],
        },
    ],
};

/// Sound Z80 program ROM at 0x0000-0x1FFF.
pub static CONGO_SOUND_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[RomEntry {
        name: "tip_top_rom_17.u19",
        size: 0x2000,
        offset: 0x0000,
        crc32: &[0x5024e673],
    }],
};

/// Foreground/text tile ROM (gfx_tx): one 4KB chip, 8×8 2bpp.
pub static CONGO_GFX_TX_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "tip_top_rom_5.u76",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0x7bf6ba2b],
    }],
};

/// Background tile ROM (gfx_bg): three 8KB chips, 8×8 3bpp.
pub static CONGO_GFX_BG_ROM: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        RomEntry {
            name: "tip_top_rom_8.u93",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xdb99a619],
        },
        RomEntry {
            name: "tip_top_rom_9.u94",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x93e2309e],
        },
        RomEntry {
            name: "tip_top_rom_10.u95",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0xf27a9407],
        },
    ],
};

/// Sprite ROM (gfx_spr): six 8KB chips, 32×32 3bpp.
pub static CONGO_GFX_SPR_ROM: RomRegion = RomRegion {
    size: 0xc000,
    entries: &[
        RomEntry {
            name: "tip_top_rom_12.u78",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x15e3377a],
        },
        RomEntry {
            name: "tip_top_rom_13.u79",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x1d1321c8],
        },
        RomEntry {
            name: "tip_top_rom_11.u77",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x73e2709f],
        },
        RomEntry {
            name: "tip_top_rom_14.u104",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0xbf9169fe],
        },
        RomEntry {
            name: "tip_top_rom_16.u106",
            size: 0x2000,
            offset: 0x8000,
            crc32: &[0xcb6d5775],
        },
        RomEntry {
            name: "tip_top_rom_15.u105",
            size: 0x2000,
            offset: 0xa000,
            crc32: &[0x7b15a7a4],
        },
    ],
};

/// Background map data (tilemap_dat): two 8KB chips describing the scrolling map.
pub static CONGO_TILEMAP_DAT_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "tip_top_rom_6.u57",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xd637f02b],
        },
        RomEntry {
            name: "tip_top_rom_7.u58",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x80927943],
        },
    ],
};

/// Color PROM (`mr019`, TBP28L22): 256 bytes, reloaded into the high half ⇒ 512.
pub static CONGO_PALETTE_PROM: RomRegion = RomRegion {
    size: 0x0200,
    entries: &[
        RomEntry {
            name: "mr019.u87",
            size: 0x100,
            offset: 0x0000,
            crc32: &[0xb788d8ae],
        },
        RomEntry {
            name: "mr019.u87",
            size: 0x100,
            offset: 0x0100,
            crc32: &[0xb788d8ae],
        },
    ],
};

// GFX bit-plane layouts, promoted to `'static` so both the runtime decode
// (`decode_gfx_roms`) and the gfxview `GfxRegion`s borrow the same tables. All
// three are MAME `*_planar` layouts with `plane_offsets` LSB-first.

/// Foreground/text: 256 chars, 8×8 2bpp; planes split at the ROM midpoint 0x800.
pub static CONGO_TX_GFX_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 0x0800 * 8],
    x_offsets: &[0, 1, 2, 3, 4, 5, 6, 7],
    y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
    char_increment: 8 * 8,
};

/// Background: 1024 chars, 8×8 3bpp; planes at thirds of the 0x6000 region.
pub static CONGO_BG_GFX_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 0x2000 * 8, 2 * 0x2000 * 8],
    x_offsets: &[0, 1, 2, 3, 4, 5, 6, 7],
    y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
    char_increment: 8 * 8,
};

/// Sprites: 128 sprites, 32×32 3bpp; planes at thirds of the 0xC000 region. Each
/// 8×8 sub-cell is 8 consecutive bytes, laid out left-to-right then top-to-bottom
/// — `x_offsets[px] = (px/8)*64 + px%8`, `y_offsets[py] = (py/8)*256 + (py%8)*8`.
pub static CONGO_SPR_GFX_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 0x4000 * 8, 2 * 0x4000 * 8],
    x_offsets: &[
        0, 1, 2, 3, 4, 5, 6, 7, // sub-cell col 0
        64, 65, 66, 67, 68, 69, 70, 71, // sub-cell col 1
        128, 129, 130, 131, 132, 133, 134, 135, // sub-cell col 2
        192, 193, 194, 195, 196, 197, 198, 199, // sub-cell col 3
    ],
    y_offsets: &[
        0, 8, 16, 24, 32, 40, 48, 56, // sub-cell row 0
        256, 264, 272, 280, 288, 296, 304, 312, // sub-cell row 1
        512, 520, 528, 536, 544, 552, 560, 568, // sub-cell row 2
        768, 776, 784, 792, 800, 808, 816, 824, // sub-cell row 3
    ],
    char_increment: 128 * 8,
};

// gfxview GFX regions. Congo Bongo's scanline renderer is already wired and
// validated, so the palette decode is settled: each region carries the real
// mr019 PROM palette via `congo_gfx_palette` (shared with `build_palette`).
inventory::submit! {
    GfxRegion {
        machine: "congobongo",
        region: "fg",
        count: 256,
        width: 8,
        height: 8,
        layout: &CONGO_TX_GFX_LAYOUT,
        load: |rs| CONGO_GFX_TX_ROM.load(rs),
        palette: Some(congo_gfx_palette),
    }
}
inventory::submit! {
    GfxRegion {
        machine: "congobongo",
        region: "bg",
        count: 1024,
        width: 8,
        height: 8,
        layout: &CONGO_BG_GFX_LAYOUT,
        load: |rs| CONGO_GFX_BG_ROM.load(rs),
        palette: Some(congo_gfx_palette),
    }
}
inventory::submit! {
    GfxRegion {
        machine: "congobongo",
        region: "sprites",
        count: 128,
        width: 32,
        height: 32,
        layout: &CONGO_SPR_GFX_LAYOUT,
        load: |rs| CONGO_GFX_SPR_ROM.load(rs),
        palette: Some(congo_gfx_palette),
    }
}

/// gfxview palette hook: load the mr019 color PROM and build the RGB palette
/// with the same resistor-DAC math as the runtime path.
fn congo_gfx_palette(rom_set: &RomSet) -> Result<Vec<(u8, u8, u8)>, RomLoadError> {
    let prom = CONGO_PALETTE_PROM.load(rom_set)?;
    Ok(congo_palette_rgb(&prom).to_vec())
}

/// Build the 512-entry RGB palette from the `mr019` color PROM.
///
/// 3-3-2 resistor DAC (per `zaxxon_palette` in `sega/zaxxon_v.cpp`): R = PROM
/// bits 0-2 and G = bits 3-5 (1k/470/220 Ω), B = bits 6-7 (470/220 Ω), all with
/// a 470 Ω pulldown. The PROM is 256 bytes mirrored into 512, so the upper half
/// (selected by the CBS color-bank latch) duplicates the lower.
///
/// Shared by [`CongoBongoBoard::build_palette`] and [`congo_gfx_palette`] so the
/// runtime renderer and the offline viewer never diverge.
fn congo_palette_rgb(palette_prom: &[u8]) -> [(u8, u8, u8); 512] {
    let rgweights = compute_resistor_weights(&[1000.0, 470.0, 220.0], Some(470.0));
    let bweights = compute_resistor_weights(&[470.0, 220.0], Some(470.0));
    let mut out = [(0u8, 0u8, 0u8); 512];
    for (i, entry) in out.iter_mut().enumerate() {
        let v = palette_prom[i];
        let r = combine_weights(&rgweights, &[v & 1, (v >> 1) & 1, (v >> 2) & 1]);
        let g = combine_weights(&rgweights, &[(v >> 3) & 1, (v >> 4) & 1, (v >> 5) & 1]);
        let b = combine_weights(&bweights, &[(v >> 6) & 1, (v >> 7) & 1]);
        *entry = (r, g, b);
    }
    out
}

// ---------------------------------------------------------------------------
// CongoBongoBoard
// ---------------------------------------------------------------------------

#[derive(BusDebug, DebugTrace)]
pub struct CongoBongoBoard {
    #[debug_cpu("Z80 Main")]
    pub(crate) cpu: Z80,
    #[debug_cpu("Z80 Sound")]
    pub(crate) sound_cpu: Z80,

    #[debug_map(cpu = 0)]
    pub(crate) main_map: AddressSpace16,
    #[debug_map(cpu = 1)]
    pub(crate) sound_map: AddressSpace16,

    // GFX ROMs + their decoded pixel caches.
    pub(crate) tx_rom: [u8; 0x1000],
    pub(crate) bg_rom: [u8; 0x6000],
    pub(crate) spr_rom: [u8; 0xc000],
    pub(crate) tilemap_dat: [u8; 0x4000],
    pub(crate) tx_cache: gfx::GfxCache, // 256 × 8×8 2bpp foreground tiles
    pub(crate) bg_cache: gfx::GfxCache, // 1024 × 8×8 3bpp background tiles
    pub(crate) sprite_cache: gfx::GfxCache, // 128 × 32×32 3bpp sprites

    // Pre-rendered background tilemap pixmap (256×4096 palette-pen indices). The
    // bg map is fixed in `tilemap_dat` ROM, so it is built once at load.
    pub(crate) bg_pixmap: Vec<u8>,

    // Color PROM + the 512-entry RGB palette decoded from it.
    pub(crate) palette_prom: [u8; 0x0200],
    pub(crate) palette_rgb: [(u8, u8, u8); 512],

    // Scanline-rendered framebuffer (256 × 240 × RGB24, pre-rotation).
    pub(crate) scanline_buffer: Vec<u8>,

    // Inputs (active-high) + DIP banks. `in2` holds the start buttons; the coin
    // bits (SW100 5/6/7) come from `coin_status`, which the game latches and
    // clears via the coin-enable latch lines.
    pub(crate) in0: u8,
    pub(crate) in1: u8,
    pub(crate) in2: u8,
    pub(crate) dsw2: u8,
    pub(crate) dsw3: u8,
    pub(crate) coin_status: [bool; 3], // coin A, coin B, service

    // 74LS259 addressable latches (raw bytes; individual lines decoded by the
    // render/input issues). `int_enabled`/`bg_enabled` are broken out because the
    // run loop needs them now.
    pub(crate) latch1: u8,
    pub(crate) latch2: u8,
    pub(crate) int_enabled: bool,
    pub(crate) bg_enabled: bool,

    // Background scroll position (two raw bytes; decoded by issue .6).
    pub(crate) bg_position: [u8; 2],

    // Custom sprite-DMA registers (src lo/hi, count, trigger) and the 256-byte
    // sprite RAM the DMA engine fills (not in the CPU address map).
    pub(crate) sprite_dma: [u8; 4],
    pub(crate) sprite_ram: [u8; 0x100],

    // Sound command latch (main CPU → PPI port A).
    pub(crate) sound_latch: u8,

    // Sound section devices.
    #[debug_device("SN76489A #1")]
    pub(crate) sn1: Sn76489a,
    #[debug_device("SN76489A #2")]
    pub(crate) sn2: Sn76489a,
    #[debug_device("PPI")]
    pub(crate) ppi: I8255,

    // Sound-CPU clocking (4 MHz from the 3.041 MHz main loop ⇒ fractional, may
    // step more than once per main cycle) + the ~244 Hz periodic IRQ.
    pub(crate) sound_cycle_accum: u64,
    pub(crate) sound_irq_counter: u64,
    pub(crate) sound_irq_pending: bool,

    // PSG generator clocks (chip_clock / 16). `audio` box-filters the summed PSG
    // output to 44.1 kHz, which feeds the discrete percussion circuit; the
    // circuit mixes the PSGs with the five synthesized voices and is drained for
    // the final output.
    pub(crate) sn1_clock: ClockDivider,
    pub(crate) sn2_clock: ClockDivider,
    pub(crate) audio: AudioResampler<i16>,
    pub(crate) congo_sound: CongoSound,

    // Timing / interrupts.
    pub(crate) clock: u64,
    pub(crate) vblank_irq_pending: bool,

    #[debug_events]
    pub(crate) debug_trace: DebugTraceBuffer,
}

impl Default for CongoBongoBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl CongoBongoBoard {
    pub fn new() -> Self {
        Self {
            cpu: Z80::new(),
            sound_cpu: Z80::new(),
            main_map: Self::build_main_map(),
            sound_map: Self::build_sound_map(),
            tx_rom: [0; 0x1000],
            bg_rom: [0; 0x6000],
            spr_rom: [0; 0xc000],
            tilemap_dat: [0; 0x4000],
            tx_cache: gfx::GfxCache::new(0, 8, 8),
            bg_cache: gfx::GfxCache::new(0, 8, 8),
            sprite_cache: gfx::GfxCache::new(0, 32, 32),
            bg_pixmap: Vec::new(),
            palette_prom: [0; 0x0200],
            palette_rgb: [(0, 0, 0); 512],
            scanline_buffer: vec![0u8; NATIVE_WIDTH * NATIVE_HEIGHT * 3],
            in0: 0x00,
            in1: 0x00,
            in2: 0x00,
            dsw2: DSW2_DEFAULT,
            dsw3: DSW3_DEFAULT,
            coin_status: [false; 3],
            latch1: 0x00,
            latch2: 0x00,
            int_enabled: false,
            bg_enabled: false,
            bg_position: [0; 2],
            sprite_dma: [0; 4],
            sprite_ram: [0; 0x100],
            sound_latch: 0,
            sn1: Sn76489a::new(SOUND_CLOCK as u32),
            sn2: Sn76489a::new(SOUND_PSG2_CLOCK),
            ppi: I8255::new(),
            sound_cycle_accum: 0,
            sound_irq_counter: 0,
            sound_irq_pending: false,
            sn1_clock: ClockDivider::new((SOUND_CLOCK / 16) as u32, TIMING.cpu_clock_hz as u32),
            sn2_clock: ClockDivider::new(SOUND_PSG2_CLOCK / 16, TIMING.cpu_clock_hz as u32),
            audio: AudioResampler::new(TIMING.cpu_clock_hz, OUTPUT_SAMPLE_RATE),
            congo_sound: CongoSound::new(),
            clock: 0,
            vblank_irq_pending: false,
            debug_trace: DebugTraceBuffer::new(),
        }
    }

    fn build_main_map() -> AddressSpace16 {
        use MainRegion::*;
        let mut map = AddressSpace16::new();
        map.region(Rom, "Program ROM", 0x0000, 0x8000, AccessKind::ReadOnly)
            .region(Ram, "Work RAM", 0x8000, 0x1000, AccessKind::ReadWrite)
            .region(VideoRam, "Video RAM", 0xA000, 0x0400, AccessKind::ReadWrite)
            .region(ColorRam, "Color RAM", 0xA400, 0x0400, AccessKind::ReadWrite)
            .region(Io, "I/O Ports", 0xC000, 0x2000, AccessKind::Io);
        map
    }

    fn build_sound_map() -> AddressSpace16 {
        use SoundRegion::*;
        let mut map = AddressSpace16::new();
        map.region(Rom, "Sound ROM", 0x0000, 0x2000, AccessKind::ReadOnly)
            .region(Ram, "Sound RAM", 0x4000, 0x0800, AccessKind::ReadWrite);
        map
    }

    // -----------------------------------------------------------------------
    // GFX decode + palette (call after loading ROMs)
    // -----------------------------------------------------------------------

    /// Decode the three Zaxxon-family GFX regions into pixel caches.
    ///
    /// All three are MAME `*_planar` layouts; phosphor's [`decode_gfx`] takes
    /// `plane_offsets` LSB-first, i.e. MAME's `planeoffset` array reversed (see
    /// `gfx_8x8x2_planar`/`gfx_8x8x3_planar`/`zaxxon_spritelayout` in
    /// `sega/zaxxon.cpp`).
    pub fn decode_gfx_roms(&mut self) {
        // The same `'static` layouts the gfxview `GfxRegion`s borrow, so the
        // offline sheet export and the runtime renderer decode identically.
        self.tx_cache = decode_gfx(&self.tx_rom, 0, 256, &CONGO_TX_GFX_LAYOUT);
        self.bg_cache = decode_gfx(&self.bg_rom, 0, 1024, &CONGO_BG_GFX_LAYOUT);
        self.sprite_cache = decode_gfx(&self.spr_rom, 0, 128, &CONGO_SPR_GFX_LAYOUT);
    }

    /// Build the 512-entry RGB palette from the `mr019` color PROM.
    ///
    /// Delegates to the shared [`congo_palette_rgb`] so the runtime renderer and
    /// the gfxview export apply identical resistor-DAC math.
    pub fn build_palette(&mut self) {
        self.palette_rgb = congo_palette_rgb(&self.palette_prom);
    }

    /// Pre-render the background tilemap into a 256×4096 pixmap of palette pen
    /// indices (`color * 8 + pen`, before the runtime color base is added).
    ///
    /// Per `get_bg_tile_info` (`sega/zaxxon_v.cpp`): the 32×512 tilemap is filled
    /// from `tilemap_dat` — first half = low 8 code bits, second half = high 2
    /// code bits (`& 3`) + 4 color bits (`>> 4`). The tile index wraps at
    /// `bytes/2` (0x2000), so the bottom half of the map mirrors the top.
    pub fn build_bg_pixmap(&mut self) {
        if self.bg_cache.count() == 0 {
            return;
        }
        const W: usize = 256; // 32 tiles × 8
        const H: usize = 4096; // 512 tiles × 8
        const SIZE: usize = 0x2000; // tilemap_dat bytes / 2
        let mut pixmap = vec![0u8; W * H];
        for tile_index in 0..(32 * 512) {
            let col = tile_index % 32;
            let row = tile_index / 32;
            let eff = tile_index & (SIZE - 1);
            let attr = self.tilemap_dat[eff + SIZE];
            let code = self.tilemap_dat[eff] as usize + 256 * (attr as usize & 3);
            let base_pen = (attr >> 4) as usize * 8;
            for py in 0..8 {
                for px in 0..8 {
                    let pen = self.bg_cache.pixel(code, px, py) as usize;
                    pixmap[(row * 8 + py) * W + col * 8 + px] = (base_pen + pen) as u8;
                }
            }
        }
        self.bg_pixmap = pixmap;
    }

    /// Source pixmap row for screen row `abs_y`, per the U56/U74/U75 adders:
    /// `VF + ((bg_position << 1) ^ 0xfff) + 1`, masked to the pixmap height.
    /// (Upright only; flip-screen VF inversion is deferred.)
    fn bg_src_y(&self, abs_y: usize) -> usize {
        let bgpos = ((self.bg_position[1] as usize & 0x07) << 8) | self.bg_position[0] as usize;
        (abs_y + ((bgpos << 1) ^ 0xfff) + 1) & 0xFFF
    }

    /// Source pixmap column for screen pixel `(x, abs_y)` with the isometric
    /// skew (U53/U54 adders): `HF + ((VF >> 1) ^ 0xff) + 1 + 0x3F`, masked to the
    /// pixmap width. The 0x3F constant is the non-flipped `flipoffs` (0x40 − 1).
    fn bg_src_x(abs_y: usize, x: usize) -> usize {
        (x + ((abs_y >> 1) ^ 0xff) + 1 + 0x3F) & 0xFF
    }

    // -----------------------------------------------------------------------
    // Custom sprite DMA
    // -----------------------------------------------------------------------

    /// Read one byte of the main CPU address space (for the sprite DMA source).
    fn read_main(&self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x8FFF => self.main_map.read_backing(addr),
            0xA000..=0xBFFF => self.main_map.read_backing(0xA000 | (addr & 0x07FF)),
            _ => 0xFF,
        }
    }

    /// Write a Congo custom sprite-DMA register (0xC030-0xC033).
    ///
    /// A write of 0x01 to register 3 triggers the transfer (`congo_sprite_custom_w`):
    /// for `count + 1` descriptors starting at `[reg0|reg1<<8]`, the first source
    /// byte is the destination sprite slot (`*4`) and the next four bytes are
    /// copied into sprite RAM; the source advances by 0x20 each descriptor.
    pub fn write_sprite_dma(&mut self, offset: usize, data: u8) {
        self.sprite_dma[offset] = data;
        if offset != 3 || data != 0x01 {
            return;
        }
        let mut saddr = (self.sprite_dma[0] as u16) | ((self.sprite_dma[1] as u16) << 8);
        let mut count = self.sprite_dma[2] as i32; // loops count + 1 times
        while count >= 0 {
            let daddr = self.read_main(saddr) as usize * 4;
            for i in 0..4 {
                self.sprite_ram[(daddr + i) & 0xff] =
                    self.read_main(saddr.wrapping_add(i as u16 + 1));
            }
            saddr = saddr.wrapping_add(0x20);
            count -= 1;
        }
    }

    /// Sprite top scanline from its Y byte (`find_minimum_y`): the first line
    /// where `(Y + 0xf2 + VF) & 0xe0 == 0xe0`, scanned to its minimum, +1.
    /// (Upright only; the flip path is kept for a later flip-screen pass.)
    fn find_minimum_y(value: u8, flip: bool) -> i32 {
        let flipmask = if flip { 0xff } else { 0x00 };
        let flipconst = if flip { 0xef } else { 0xf1 };
        let mut y: i32 = 0;
        while y < 256 {
            let sum = (value as i32 + flipconst + 1) + (y ^ flipmask);
            if sum & 0xe0 == 0xe0 {
                break;
            }
            y += 16;
        }
        loop {
            let sum = (value as i32 + flipconst + 1) + ((y - 1) ^ flipmask);
            if sum & 0xe0 != 0xe0 {
                break;
            }
            y -= 1;
        }
        (y + 1) & 0xff
    }

    /// Sprite left column from its X byte (`find_minimum_x`).
    fn find_minimum_x(value: u8, flip: bool) -> i32 {
        let flipmask = if flip { 0xff } else { 0x00 };
        let mut x = (value as i32 + 0xef + 1) ^ flipmask;
        if flipmask != 0 {
            x -= 31;
        }
        x & 0xff
    }

    /// Draw the sprites covering one scanline (32×32 3bpp, transparent pen 0).
    ///
    /// Only the lower half of sprite RAM is scanned, back-to-front (offs 0x7C →
    /// 0) so lower-indexed sprites land on top. Each sprite is positioned via
    /// `find_minimum_x/y` and drawn with 256-pixel X and Y wrap (`draw_sprites`
    /// with flip masks 0x280/0x180). Per-sprite color = `(byte & 0x1f) +
    /// (color_bank << 5)`, palette pen = `color * 8 + pen`.
    fn render_sprites_scanline(&mut self, abs_y: usize) {
        if self.sprite_cache.count() == 0 {
            return;
        }
        let color_bank = ((self.latch2 >> 7) & 1) as usize;
        let sprites = &self.sprite_cache;
        let palette = &self.palette_rgb;
        let ram = &self.sprite_ram;
        let buf_start = abs_y * NATIVE_WIDTH * 3;
        let buf = &mut self.scanline_buffer[buf_start..buf_start + NATIVE_WIDTH * 3];
        let clip = SpriteClip {
            x_min: 0,
            x_max: NATIVE_WIDTH as i32,
            wrap_offset: Some(-0x100),
        };

        let mut offs = 0x7c;
        loop {
            let sy = Self::find_minimum_y(ram[offs], false);
            let code = (ram[offs + 1] & 0x7f) as u16; // bit 7 = flip Y
            let flip_y = ram[offs + 1] & 0x80 != 0;
            let color = (ram[offs + 2] & 0x1f) as usize + (color_bank << 5);
            let flip_x = ram[offs + 2] & 0x80 != 0;
            let sx = Self::find_minimum_x(ram[offs + 3], false);

            // Sprite covers `abs_y` from its primary anchor or the −256 Y wrap.
            for sy_anchor in [sy, sy - 0x100] {
                let row = abs_y as i32 - sy_anchor;
                if (0..32).contains(&row) {
                    let src_py = if flip_y { 31 - row } else { row } as usize;
                    draw_sprite_row(
                        sprites,
                        code,
                        src_py,
                        sx,
                        flip_x,
                        |pv| pv == 0,
                        |pv| palette[(color * 8 + pv as usize) & 0x1FF],
                        buf,
                        &clip,
                    );
                }
            }

            if offs == 0 {
                break;
            }
            offs -= 4;
        }
    }

    /// Decoded foreground / background / sprite pixel caches (consumed by the
    /// scanline renderer in the follow-up issues, and by debug tooling).
    pub fn tx_cache(&self) -> &gfx::GfxCache {
        &self.tx_cache
    }
    pub fn bg_cache(&self) -> &gfx::GfxCache {
        &self.bg_cache
    }
    pub fn sprite_cache(&self) -> &gfx::GfxCache {
        &self.sprite_cache
    }

    /// One entry of the decoded 512-color RGB palette.
    pub fn palette_color(&self, index: usize) -> (u8, u8, u8) {
        self.palette_rgb[index & 0x1FF]
    }

    // -----------------------------------------------------------------------
    // 74LS259 control latches
    // -----------------------------------------------------------------------

    /// Write one bit of main latch 1 (0xC018-0xC01F, U52, LS259 `write_d0`).
    /// Bits 0-2 arm coin inputs 1/2/service; bit 2 = coin-enable; bit 5 = BEN
    /// (background enable); bit 6 = flip; bit 7 = INTON (VBlank IRQ enable).
    pub fn write_latch1(&mut self, bit: u8, value: bool) {
        if value {
            self.latch1 |= 1 << bit;
        } else {
            self.latch1 &= !(1 << bit);
        }
        // Each coin latch is async-cleared while its enable line (bits 0-2) is
        // low. The game acknowledges a credit by pulsing that line low→high (see
        // the per-coin pulses at 0x0B73 in the program ROM), so the clear must
        // run on every latch write, not only on the shared coin-enable bit.
        for n in 0..3 {
            if (self.latch1 >> n) & 1 == 0 {
                self.coin_status[n] = false;
            }
        }
        match bit {
            5 => self.bg_enabled = value,
            7 => {
                self.int_enabled = value;
                if !value {
                    self.vblank_irq_pending = false;
                }
            }
            _ => {}
        }
    }

    /// Latch a coin insert (`zaxxon_coin_inserted`): the coin registers only
    /// while its arming line (latch-1 bit `n`) is high.
    pub fn coin_inserted(&mut self, n: usize) {
        if (self.latch1 >> n) & 1 == 1 {
            self.coin_status[n] = true;
        }
    }

    /// SW100 (0xC008) input: start buttons plus the three latched coin bits.
    pub fn read_sw100(&self) -> u8 {
        self.in2
            | (self.coin_status[0] as u8) << 5
            | (self.coin_status[1] as u8) << 6
            | (self.coin_status[2] as u8) << 7
    }

    /// Write one bit of main latch 2 (0xC020-0xC027, U53, LS259 `write_d0`).
    /// Bit 1 = CREF1 (fg color), bit 3 = CREF3 (bg color), bit 6 = BS (fg bank),
    /// bit 7 = CBS (color bank); decoded by the render issues from `latch2`.
    pub fn write_latch2(&mut self, bit: u8, value: bool) {
        if value {
            self.latch2 |= 1 << bit;
        } else {
            self.latch2 &= !(1 << bit);
        }
    }

    // -----------------------------------------------------------------------
    // Core tick
    // -----------------------------------------------------------------------

    /// Execute one main-CPU clock cycle (≈3.041 MHz), advancing the sound CPU,
    /// PSGs, and audio resampler alongside it.
    pub fn tick(&mut self, bus: &mut dyn Bus<Address = u16, Data = u8>) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();

        // Per-scanline rendering hook (layers added by issues .5/.6/.7).
        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let scanline = (frame_cycle / TIMING.cycles_per_scanline) as u16;
            if scanline < VISIBLE_LINES as u16 {
                self.render_scanline(scanline as usize);
            }
        }

        // VBlank IRQ: asserted at line 240 when INTON is enabled, held until the
        // game clears INTON (see `write_latch1`). Z80 IRQ is level-triggered and
        // IFF1-masked, so the handler's DI/EI sequencing avoids re-entry.
        let vblank_cycle = VISIBLE_LINES * TIMING.cycles_per_scanline;
        if frame_cycle == vblank_cycle && self.int_enabled {
            self.vblank_irq_pending = true;
        }

        if self.main_map.has_any_watchpoints() || self.debug_trace.enabled() {
            let pc = self
                .cpu
                .at_instruction_boundary()
                .then_some(self.cpu.pc as u32);
            self.main_map.latch_access_context(self.clock, pc);
        }

        self.cpu.execute_cycle(bus, BusMaster::Cpu(0));

        self.tick_sound(bus);

        self.clock += 1;
    }

    /// Advance the sound CPU, its periodic IRQ, the two PSGs, and the audio
    /// resampler by one main-CPU cycle's worth of time.
    fn tick_sound(&mut self, bus: &mut dyn Bus<Address = u16, Data = u8>) {
        // Sound Z80 runs at 4 MHz against the 3.041 MHz main loop, so it may take
        // more than one step per main cycle (a fractional Bresenham accumulator).
        self.sound_cycle_accum += SOUND_CLOCK;
        while self.sound_cycle_accum >= TIMING.cpu_clock_hz {
            self.sound_cycle_accum -= TIMING.cpu_clock_hz;

            // Periodic ~244 Hz IRQ (irq0_line_hold).
            self.sound_irq_counter += 1;
            if self.sound_irq_counter >= SOUND_IRQ_PERIOD {
                self.sound_irq_counter = 0;
                self.sound_irq_pending = true;
            }

            // HOLD_LINE auto-clear: the line drops when the CPU acknowledges the
            // interrupt, which it does at an instruction boundary while IFF1 is
            // set — observed here as IFF1 going 1→0 with the IRQ pending. (A DI
            // with the IRQ pending could also clear it a cycle early; harmless.)
            let iff1_before = self.sound_cpu.iff1;
            self.sound_cpu.execute_cycle(bus, BusMaster::Cpu(1));
            if self.sound_irq_pending && iff1_before && !self.sound_cpu.iff1 {
                self.sound_irq_pending = false;
            }
        }

        // PSG generators run at chip_clock/16; sample the summed output each main
        // cycle, box-filter it to the audio rate, and feed each resulting sample
        // into the percussion circuit (which mixes PSGs + voices).
        if self.sn1_clock.tick() {
            self.sn1.tick();
        }
        if self.sn2_clock.tick() {
            self.sn2.tick();
        }
        let mix = (self.sn1.output() as i32 + self.sn2.output() as i32)
            .clamp(i16::MIN as i32, i16::MAX as i32);
        if let Some(avg) = self.audio.tick_sample(mix as i16) {
            self.congo_sound.feed_psg(avg);
        }
    }

    /// Drain the mixed PSG + percussion audio into the output buffer.
    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.congo_sound.fill_audio(buffer)
    }

    /// Push the current PPI port B/C latches to the percussion gates.
    pub fn sync_percussion(&mut self) {
        let port_b = self.ppi.read_output_b();
        let port_c = self.ppi.read_output_c();
        self.congo_sound.set_triggers(port_b, port_c);
    }

    /// Render one native screen scanline (`abs_y` = bitmap row 0-239).
    ///
    /// Layer order matches `screen_update_congo`: the row is cleared, then the
    /// scrolling background, sprites, and the foreground tilemap (transparent
    /// pen 0) are drawn on top of one another.
    pub fn render_scanline(&mut self, abs_y: usize) {
        let row_offset = abs_y * NATIVE_WIDTH * 3;
        self.scanline_buffer[row_offset..row_offset + NATIVE_WIDTH * 3].fill(0);
        self.render_bg_scanline(abs_y);
        self.render_sprites_scanline(abs_y);
        self.render_fg_scanline(abs_y);
    }

    /// Draw the pseudo-3D scrolling background for one scanline.
    ///
    /// Samples the pre-built pixmap with the isometric skew (`draw_background`
    /// with `skew = true`) and adds the runtime color base `bg_color (CREF3) +
    /// (color_bank << 8)`. When the layer is disabled the row stays black.
    fn render_bg_scanline(&mut self, abs_y: usize) {
        if !self.bg_enabled || self.bg_pixmap.is_empty() {
            return;
        }
        let colorbase =
            ((self.latch2 >> 3) & 1) as usize * 0x80 + ((self.latch2 >> 7) & 1) as usize * 0x100;
        let row_base = self.bg_src_y(abs_y) * 256;
        let pixmap = &self.bg_pixmap;
        let palette = &self.palette_rgb;
        let buf_start = abs_y * NATIVE_WIDTH * 3;
        let buf = &mut self.scanline_buffer[buf_start..buf_start + NATIVE_WIDTH * 3];
        for x in 0..NATIVE_WIDTH {
            let val = pixmap[row_base + Self::bg_src_x(abs_y, x)] as usize;
            let (r, g, b) = palette[(val + colorbase) & 0x1FF];
            let off = x * 3;
            buf[off] = r;
            buf[off + 1] = g;
            buf[off + 2] = b;
        }
    }

    /// Draw the foreground/text tilemap for one scanline (32×32 of 8×8 2bpp
    /// tiles, transparent pen 0). Per `congo_get_fg_tile_info`: tile code =
    /// `videoram + (fg_bank << 8)`, color = `colorram & 0x1f`, and the gfx pen
    /// (granularity 8) is offset by `fg_color (CREF1) + (color_bank << 8)`.
    fn render_fg_scanline(&mut self, abs_y: usize) {
        let tile_count = self.tx_cache.count();
        if tile_count == 0 {
            return; // GFX ROMs not loaded yet
        }
        let video_ram = self.main_map.region_data(MainRegion::VideoRam);
        let color_ram = self.main_map.region_data(MainRegion::ColorRam);
        let tiles = &self.tx_cache;
        let palette = &self.palette_rgb;

        // Latch-2 control lines: bit1 = fg_color (CREF1), bit6 = fg bank (BS),
        // bit7 = color bank (CBS).
        let fg_bank = ((self.latch2 >> 6) & 1) as usize;
        let pal_offset =
            ((self.latch2 >> 1) & 1) as usize * 0x80 + ((self.latch2 >> 7) & 1) as usize * 0x100;

        let row = abs_y / 8;
        let py = abs_y % 8;
        let buf_start = abs_y * NATIVE_WIDTH * 3;
        let buf = &mut self.scanline_buffer[buf_start..buf_start + NATIVE_WIDTH * 3];

        for col in 0..32 {
            let idx = row * 32 + col;
            // The 0x1000 fg ROM only decodes 256 tiles; the fg-bank high bit has
            // no ROM behind it on this set, so wrap rather than index past it.
            let code = (video_ram[idx] as usize + (fg_bank << 8)) % tile_count;
            let color = (color_ram[idx] & 0x1f) as usize;
            let base = color * 8 + pal_offset;
            let screen_x = col * 8;
            for px in 0..8 {
                let pen = tiles.pixel(code, px, py);
                if pen == 0 {
                    continue; // transparent — lower layers show through
                }
                let (r, g, b) = palette[(base + pen as usize) & 0x1FF];
                let off = (screen_x + px) * 3;
                buf[off] = r;
                buf[off + 1] = g;
                buf[off + 2] = b;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Frame output (ROT90)
    // -----------------------------------------------------------------------

    /// Rotate the visible raster (rows 16..240, 256×224) 90° CCW into the
    /// portrait 224×256 RGB24 output buffer.
    pub fn render_frame(&self, buffer: &mut [u8]) {
        let start = VBLANK_END * NATIVE_WIDTH * 3;
        let visible =
            &self.scanline_buffer[start..start + (NATIVE_HEIGHT - VBLANK_END) * NATIVE_WIDTH * 3];
        gfx::rotate_90_ccw(visible, buffer, NATIVE_WIDTH, NATIVE_HEIGHT - VBLANK_END);
    }

    // -----------------------------------------------------------------------
    // Reset / interrupts
    // -----------------------------------------------------------------------

    pub fn reset(&mut self) {
        self.int_enabled = false;
        self.bg_enabled = false;
        self.vblank_irq_pending = false;
        self.latch1 = 0;
        self.latch2 = 0;
        self.bg_position = [0; 2];
        self.sprite_dma = [0; 4];
        self.sprite_ram = [0; 0x100];
        self.coin_status = [false; 3];
        self.sound_latch = 0;
        self.clock = 0;

        self.sn1.reset();
        self.sn2.reset();
        self.ppi.reset();
        self.sound_cycle_accum = 0;
        self.sound_irq_counter = 0;
        self.sound_irq_pending = false;
        self.sn1_clock.reset();
        self.sn2_clock.reset();
        self.audio.reset();
        self.congo_sound.reset();

        self.main_map.region_data_mut(MainRegion::Ram).fill(0);
        self.main_map.region_data_mut(MainRegion::VideoRam).fill(0);
        self.main_map.region_data_mut(MainRegion::ColorRam).fill(0);
        self.sound_map.region_data_mut(SoundRegion::Ram).fill(0);
        self.scanline_buffer.fill(0);
    }

    pub fn check_interrupts(&self, target: BusMaster) -> InterruptState {
        match target {
            BusMaster::Cpu(0) => InterruptState {
                irq: self.vblank_irq_pending && self.int_enabled,
                ..Default::default()
            },
            BusMaster::Cpu(1) => InterruptState {
                irq: self.sound_irq_pending,
                ..Default::default()
            },
            _ => InterruptState::default(),
        }
    }

    pub fn debug_tick_boundaries(&self) -> u32 {
        let mut result = 0;
        if self.cpu.at_instruction_boundary() {
            result |= 1;
        }
        if self.sound_cpu.at_instruction_boundary() {
            result |= 2;
        }
        result
    }
}

impl Saveable for CongoBongoBoard {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        self.sound_cpu.save_state(w);
        w.write_bytes(self.main_map.region_data(MainRegion::Ram));
        w.write_bytes(self.main_map.region_data(MainRegion::VideoRam));
        w.write_bytes(self.main_map.region_data(MainRegion::ColorRam));
        w.write_bytes(self.sound_map.region_data(SoundRegion::Ram));
        w.write_u8(self.in0);
        w.write_u8(self.in1);
        w.write_u8(self.in2);
        w.write_u8(self.dsw2);
        w.write_u8(self.dsw3);
        for &c in &self.coin_status {
            w.write_bool(c);
        }
        w.write_u8(self.latch1);
        w.write_u8(self.latch2);
        w.write_bool(self.int_enabled);
        w.write_bool(self.bg_enabled);
        w.write_bytes(&self.bg_position);
        w.write_bytes(&self.sprite_dma);
        w.write_bytes(&self.sprite_ram);
        w.write_u8(self.sound_latch);
        self.sn1.save_state(w);
        self.sn2.save_state(w);
        self.ppi.save_state(w);
        w.write_u64_le(self.sound_cycle_accum);
        w.write_u64_le(self.sound_irq_counter);
        w.write_bool(self.sound_irq_pending);
        self.sn1_clock.save_state(w);
        self.sn2_clock.save_state(w);
        self.audio.save_state(w);
        self.congo_sound.save_state(w);
        w.write_u64_le(self.clock);
        w.write_bool(self.vblank_irq_pending);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        self.sound_cpu.load_state(r)?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::Ram))?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::VideoRam))?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::ColorRam))?;
        r.read_bytes_into(self.sound_map.region_data_mut(SoundRegion::Ram))?;
        self.in0 = r.read_u8()?;
        self.in1 = r.read_u8()?;
        self.in2 = r.read_u8()?;
        self.dsw2 = r.read_u8()?;
        self.dsw3 = r.read_u8()?;
        for c in &mut self.coin_status {
            *c = r.read_bool()?;
        }
        self.latch1 = r.read_u8()?;
        self.latch2 = r.read_u8()?;
        self.int_enabled = r.read_bool()?;
        self.bg_enabled = r.read_bool()?;
        r.read_bytes_into(&mut self.bg_position)?;
        r.read_bytes_into(&mut self.sprite_dma)?;
        r.read_bytes_into(&mut self.sprite_ram)?;
        self.sound_latch = r.read_u8()?;
        self.sn1.load_state(r)?;
        self.sn2.load_state(r)?;
        self.ppi.load_state(r)?;
        self.sound_cycle_accum = r.read_u64_le()?;
        self.sound_irq_counter = r.read_u64_le()?;
        self.sound_irq_pending = r.read_bool()?;
        self.sn1_clock.load_state(r)?;
        self.sn2_clock.load_state(r)?;
        self.audio.load_state(r)?;
        self.congo_sound.load_state(r)?;
        self.clock = r.read_u64_le()?;
        self.vblank_irq_pending = r.read_bool()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// CongoBongoSystem wrapper
// ---------------------------------------------------------------------------

/// Sega Congo Bongo (1983).
#[derive(Saveable)]
pub struct CongoBongoSystem {
    pub board: CongoBongoBoard,
}

impl Default for CongoBongoSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl CongoBongoSystem {
    pub fn new() -> Self {
        Self {
            board: CongoBongoBoard::new(),
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let prog = CONGO_MAIN_ROM.load(rom_set)?;
        self.board
            .main_map
            .load_region_at(MainRegion::Rom, 0, &prog);

        let sound = CONGO_SOUND_ROM.load(rom_set)?;
        self.board
            .sound_map
            .load_region_at(SoundRegion::Rom, 0, &sound);

        self.board
            .tx_rom
            .copy_from_slice(&CONGO_GFX_TX_ROM.load(rom_set)?);
        self.board
            .bg_rom
            .copy_from_slice(&CONGO_GFX_BG_ROM.load(rom_set)?);
        self.board
            .spr_rom
            .copy_from_slice(&CONGO_GFX_SPR_ROM.load(rom_set)?);
        self.board
            .tilemap_dat
            .copy_from_slice(&CONGO_TILEMAP_DAT_ROM.load(rom_set)?);
        self.board
            .palette_prom
            .copy_from_slice(&CONGO_PALETTE_PROM.load(rom_set)?);

        self.board.decode_gfx_roms();
        self.board.build_palette();
        self.board.build_bg_pixmap();
        Ok(())
    }

    /// Canonicalize a 0xA000-0xBFFF access (video/color RAM + their 0x800/0x1000/
    /// 0x1800 mirrors) to the 0xA000-0xA7FF backing window.
    #[inline]
    fn vram_addr(addr: u16) -> u16 {
        0xA000 | (addr & 0x07FF)
    }
}

impl Bus for CongoBongoSystem {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        match master {
            BusMaster::Cpu(0) => {
                let data = match addr {
                    0x0000..=0x8FFF => self.board.main_map.read_backing(addr),
                    0xA000..=0xBFFF => self.board.main_map.read_backing(Self::vram_addr(addr)),
                    0xC000..=0xDFFF => match addr & 0x3F {
                        0x00 => self.board.in0,
                        0x01 => self.board.in1,
                        0x02 => self.board.dsw2,
                        0x03 => self.board.dsw3,
                        0x08 => self.board.read_sw100(),
                        _ => 0xFF,
                    },
                    _ => 0xFF,
                };
                self.board.main_map.watch_read(0, master, addr, data);
                data
            }

            // Sound CPU: program ROM, work RAM, and the i8255 PPI (the SN76489As
            // are write-only). The PSGs and PPI are heavily mirrored across 0x2000.
            BusMaster::Cpu(1) => match addr {
                0x0000..=0x1FFF => self.board.sound_map.read_backing(addr),
                0x4000..=0x4FFF => self.board.sound_map.read_backing(0x4000 | (addr & 0x07FF)),
                0x8000..=0x9FFF => self.board.ppi.read(addr & 0x03),
                _ => 0xFF,
            },

            _ => 0xFF,
        }
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        match master {
            BusMaster::Cpu(0) => {
                self.board.main_map.watch_write(0, master, addr, data);
                match addr {
                    0x8000..=0x8FFF => self.board.main_map.write_backing(addr, data),
                    0xA000..=0xBFFF => self
                        .board
                        .main_map
                        .write_backing(Self::vram_addr(addr), data),
                    0xC000..=0xDFFF => match addr & 0x3F {
                        0x18..=0x1F => self.board.write_latch1((addr & 0x07) as u8, data & 1 != 0),
                        0x20..=0x27 => self.board.write_latch2((addr & 0x07) as u8, data & 1 != 0),
                        0x28..=0x29 => self.board.bg_position[(addr & 0x01) as usize] = data,
                        0x30..=0x33 => self.board.write_sprite_dma((addr & 0x03) as usize, data),
                        0x38..=0x3F => {
                            // Latch the sound command and drive it onto PPI port A
                            // (the sound CPU reads it there).
                            self.board.sound_latch = data;
                            self.board.ppi.set_port_a_input(data);
                        }
                        _ => {}
                    },
                    _ => {} // ROM / unmapped
                }
            }
            // Sound CPU: work RAM, SN76489A #1 (0x6000), i8255 PPI (0x8000), and
            // SN76489A #2 (0xA000), each mirrored across its 0x2000 block.
            BusMaster::Cpu(1) => match addr {
                0x4000..=0x4FFF => self
                    .board
                    .sound_map
                    .write_backing(0x4000 | (addr & 0x07FF), data),
                0x6000..=0x7FFF => self.board.sn1.write(data),
                0x8000..=0x9FFF => {
                    self.board.ppi.write(addr & 0x03, data);
                    // The PPI port B/C outputs drive the percussion triggers.
                    self.board.sync_percussion();
                }
                0xA000..=0xBFFF => self.board.sn2.write(data),
                _ => {}
            },
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

crate::impl_board_delegation!(CongoBongoSystem, board, crate::congo_bongo::TIMING);

impl MachineCore for CongoBongoSystem {
    crate::machine_core_metadata!("congobongo", crate::congo_bongo::TIMING);

    fn run_frame(&mut self) {
        bus_split!(self, bus => {
            for _ in 0..crate::congo_bongo::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });
    }

    fn reset(&mut self) {
        self.board.reset();
        bus_split!(self, bus => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
            self.board.sound_cpu.reset(bus, BusMaster::Cpu(1));
        });
    }
}

impl SaveState for CongoBongoSystem {
    crate::machine_save_state!();
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------
// Stable input IDs. SW00 = P1 joystick + button, SW01 = P2 (cocktail), SW100 =
// start + coin status. Coins go through the latch/ack path in `coin_inserted`.
const INPUT_P1_RIGHT: u16 = 0;
const INPUT_P1_LEFT: u16 = 1;
const INPUT_P1_UP: u16 = 2;
const INPUT_P1_DOWN: u16 = 3;
const INPUT_P1_BUTTON: u16 = 4;
const INPUT_P2_RIGHT: u16 = 5;
const INPUT_P2_LEFT: u16 = 6;
const INPUT_P2_UP: u16 = 7;
const INPUT_P2_DOWN: u16 = 8;
const INPUT_P2_BUTTON: u16 = 9;
const INPUT_P1_START: u16 = 10;
const INPUT_P2_START: u16 = 11;
const INPUT_COIN1: u16 = 12;
const INPUT_COIN2: u16 = 13;
const INPUT_SERVICE: u16 = 14;

#[allow(clippy::too_many_arguments)]
const fn dir(
    id: u16,
    name: &'static str,
    label: &'static str,
    direction: Direction,
    player: u8,
    bindings: &'static [phosphor_core::core::machine::DefaultBinding],
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

const fn button(id: u16, name: &'static str, label: &'static str, player: u8) -> InputControl {
    InputControl {
        id: InputId(id),
        stable_name: name,
        label,
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(player),
        default_bindings: &[],
    }
}

use crate::input_defaults as ind;

const CONGO_CONTROLS: &[InputControl] = &[
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
    button(INPUT_P1_BUTTON, "p1_button", "P1 Button", 1),
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
    button(INPUT_P2_BUTTON, "p2_button", "P2 Button", 2),
    InputControl {
        id: InputId(INPUT_P1_START),
        stable_name: "p1_start",
        label: "P1 Start",
        kind: InputKind::Start,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_START,
    },
    InputControl {
        id: InputId(INPUT_P2_START),
        stable_name: "p2_start",
        label: "P2 Start",
        kind: InputKind::Start,
        player: Some(2),
        default_bindings: crate::input_defaults::P2_START,
    },
    InputControl {
        id: InputId(INPUT_COIN1),
        stable_name: "coin1",
        label: "Coin 1",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(INPUT_COIN2),
        stable_name: "coin2",
        label: "Coin 2",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_SERVICE),
        stable_name: "service",
        label: "Service",
        kind: InputKind::Service,
        player: None,
        default_bindings: crate::input_defaults::SERVICE,
    },
];

impl InputConfigurable for CongoBongoSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        CONGO_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let InputEvent::Button { id, pressed } = event else {
            return;
        };
        let b = &mut self.board;
        match id.0 {
            INPUT_P1_RIGHT => set_bit_active_high(&mut b.in0, 0, pressed),
            INPUT_P1_LEFT => set_bit_active_high(&mut b.in0, 1, pressed),
            INPUT_P1_UP => set_bit_active_high(&mut b.in0, 2, pressed),
            INPUT_P1_DOWN => set_bit_active_high(&mut b.in0, 3, pressed),
            INPUT_P1_BUTTON => set_bit_active_high(&mut b.in0, 4, pressed),
            INPUT_P2_RIGHT => set_bit_active_high(&mut b.in1, 0, pressed),
            INPUT_P2_LEFT => set_bit_active_high(&mut b.in1, 1, pressed),
            INPUT_P2_UP => set_bit_active_high(&mut b.in1, 2, pressed),
            INPUT_P2_DOWN => set_bit_active_high(&mut b.in1, 3, pressed),
            INPUT_P2_BUTTON => set_bit_active_high(&mut b.in1, 4, pressed),
            INPUT_P1_START => set_bit_active_high(&mut b.in2, 2, pressed),
            INPUT_P2_START => set_bit_active_high(&mut b.in2, 3, pressed),
            // Coins latch on the press edge (and only while armed).
            INPUT_COIN1 if pressed => b.coin_inserted(0),
            INPUT_COIN2 if pressed => b.coin_inserted(1),
            INPUT_SERVICE if pressed => b.coin_inserted(2),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// DIP switches (per INPUT_PORTS(congo) in sega/zaxxon.cpp)
// ---------------------------------------------------------------------------

const DSW2_DEFAULT: u8 = 0x77; // 10000 bonus, Medium, 3 lives, sound on, upright
const DSW3_DEFAULT: u8 = 0x33; // 1C/1C both slots

/// Coinage choices shared by Coin A (bits 4-7) and Coin B (bits 0-3); `shift`
/// places them in the right nibble.
const fn coinage(shift: u8) -> [DipChoice; 16] {
    [
        DipChoice {
            label: "4 Coins/1 Credit",
            value: 0x0f << shift,
        },
        DipChoice {
            label: "3 Coins/1 Credit",
            value: 0x07 << shift,
        },
        DipChoice {
            label: "2 Coins/1 Credit",
            value: 0x0b << shift,
        },
        DipChoice {
            label: "2C/1C 5C/3C 6C/4C",
            value: 0x06 << shift,
        },
        DipChoice {
            label: "2C/1C 3C/2C 4C/3C",
            value: 0x0a << shift,
        },
        DipChoice {
            label: "1 Coin/1 Credit",
            value: 0x03 << shift,
        },
        DipChoice {
            label: "1C/1C 5C/6C",
            value: 0x02 << shift,
        },
        DipChoice {
            label: "1C/1C 4C/5C",
            value: 0x0c << shift,
        },
        DipChoice {
            label: "1C/1C 2C/3C",
            value: 0x04 << shift,
        },
        DipChoice {
            label: "1 Coin/2 Credits",
            value: 0x0d << shift,
        },
        DipChoice {
            label: "1C/2C 5C/11C",
            value: 0x08 << shift,
        },
        DipChoice {
            label: "1C/2C 4C/9C",
            value: 0x00 << shift,
        },
        DipChoice {
            label: "1 Coin/3 Credits",
            value: 0x05 << shift,
        },
        DipChoice {
            label: "1 Coin/4 Credits",
            value: 0x09 << shift,
        },
        DipChoice {
            label: "1 Coin/5 Credits",
            value: 0x01 << shift,
        },
        DipChoice {
            label: "1 Coin/6 Credits",
            value: 0x0e << shift,
        },
    ]
}

const COIN_B_CHOICES: [DipChoice; 16] = coinage(0);
const COIN_A_CHOICES: [DipChoice; 16] = coinage(4);

const CONGO_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSW02",
        options: &[
            DipOption {
                name: "Bonus Life",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "10000",
                        value: 0x03,
                    },
                    DipChoice {
                        label: "20000",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "30000",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "40000",
                        value: 0x00,
                    },
                ],
            },
            DipOption {
                name: "Difficulty",
                mask: 0x0c,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Easy",
                        value: 0x0c,
                    },
                    DipChoice {
                        label: "Medium",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "Hard",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "Hardest",
                        value: 0x00,
                    },
                ],
            },
            DipOption {
                name: "Lives",
                mask: 0x30,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "3",
                        value: 0x30,
                    },
                    DipChoice {
                        label: "4",
                        value: 0x10,
                    },
                    DipChoice {
                        label: "5",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "Free Play",
                        value: 0x00,
                    },
                ],
            },
            DipOption {
                name: "Sound",
                mask: 0x40,
                apply: DipApplyTiming::Immediate,
                choices: &[
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
            DipOption {
                name: "Cabinet",
                mask: 0x80,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Upright",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Cocktail",
                        value: 0x80,
                    },
                ],
            },
        ],
    },
    DipSwitchBank {
        name: "DSW03",
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
    },
];

impl DipSwitches for CongoBongoSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        CONGO_DIP_BANKS
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        match bank {
            0 => self.board.dsw2,
            1 => self.board.dsw3,
            _ => 0,
        }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        match bank {
            0 => self.board.dsw2 = value,
            1 => self.board.dsw3 = value,
            _ => {}
        }
    }
}

impl Nvram for CongoBongoSystem {}
impl phosphor_core::core::machine::Profilable for CongoBongoSystem {}
crate::impl_board_debug_trace!(CongoBongoSystem, board);

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = CongoBongoSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("congobongo", &["congo", "congobongo"], create_machine)
}

inventory::submit! {
    DisasmRegion {
        machine: "congobongo",
        region: "main",
        cpu: DisasmCpu::Z80,
        org: 0x0000,
        size: CONGO_MAIN_ROM.size as u32,
        load: |rs| CONGO_MAIN_ROM.load(rs),
    }
}

inventory::submit! {
    DisasmRegion {
        machine: "congobongo",
        region: "sound",
        cpu: DisasmCpu::Z80,
        org: 0x0000,
        size: CONGO_SOUND_ROM.size as u32,
        load: |rs| CONGO_SOUND_ROM.load(rs),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::debug::Debuggable;

    #[test]
    fn registered_in_machine_and_disasm_registries() {
        let entry = crate::registry::find("congobongo").expect("machine registered");
        assert_eq!(entry.rom_names, &["congo", "congobongo"]);

        let regions = crate::disasm_registry::regions_for("congobongo");
        assert_eq!(
            regions.iter().map(|r| r.region).collect::<Vec<_>>(),
            vec!["main", "sound"],
        );
        let main = crate::disasm_registry::find("congobongo", "main").unwrap();
        assert_eq!((main.cpu, main.org, main.size), (DisasmCpu::Z80, 0, 0x8000));
        let sound = crate::disasm_registry::find("congobongo", "sound").unwrap();
        assert_eq!(
            (sound.cpu, sound.org, sound.size),
            (DisasmCpu::Z80, 0, 0x2000)
        );
    }

    #[test]
    fn gfx_decode_sizes_and_planes() {
        let mut board = CongoBongoBoard::new();
        // Plant a known bit pattern and confirm the planar decode reads it.
        // tx plane 0 (LSB) lives in the first half, plane 1 (MSB) at +0x800.
        board.tx_rom[0] = 0b1000_0000; // tile 0, row 0, col 0 → plane-0 bit set
        board.tx_rom[0x800] = 0b0100_0000; // tile 0, row 0, col 1 → plane-1 bit set
        board.decode_gfx_roms();

        assert_eq!(board.tx_cache().count(), 256);
        assert_eq!(
            (board.tx_cache().width(), board.tx_cache().height()),
            (8, 8)
        );
        assert_eq!(board.bg_cache().count(), 1024);
        assert_eq!(board.sprite_cache().count(), 128);
        assert_eq!(
            (board.sprite_cache().width(), board.sprite_cache().height()),
            (32, 32)
        );

        // Pixel (0,0) gets only plane 0 → value 1; pixel (1,0) only plane 1 → 2.
        assert_eq!(board.tx_cache().pixel(0, 0, 0), 1);
        assert_eq!(board.tx_cache().pixel(0, 1, 0), 2);
    }

    #[test]
    fn palette_3_3_2_resistor_dac() {
        let mut board = CongoBongoBoard::new();
        // All bits set in entry 1 → white; entry 2 = red only (bits 0-2).
        board.palette_prom[0] = 0x00;
        board.palette_prom[1] = 0xFF;
        board.palette_prom[2] = 0x07;
        board.build_palette();

        assert_eq!(board.palette_color(0), (0, 0, 0));
        assert_eq!(board.palette_color(1), (255, 255, 255));
        let (r, g, b) = board.palette_color(2);
        assert_eq!((g, b), (0, 0));
        assert_eq!(r, 255, "all three red bits on → full red");

        // The PROM is mirrored into the upper half (CBS color bank).
        board.palette_prom[0x100] = 0xFF;
        board.build_palette();
        assert_eq!(board.palette_color(0x100), (255, 255, 255));
    }

    #[test]
    fn foreground_tilemap_renders_with_transparency() {
        let mut board = CongoBongoBoard::new();
        // Tile 1, row 0: col 0 → plane-0 set (pen 1, opaque); col 1 → pen 0.
        board.tx_rom[8] = 0b1000_0000;
        board.decode_gfx_roms();
        // color 2 → pen base 2*8 = 16; pen 1 lands at palette[17].
        board.palette_prom[17] = 0xFF; // white
        board.build_palette();
        board.main_map.region_data_mut(MainRegion::VideoRam)[0] = 1; // tile code
        board.main_map.region_data_mut(MainRegion::ColorRam)[0] = 2; // color

        board.render_scanline(0);
        let buf = &board.scanline_buffer;
        assert_eq!((buf[0], buf[1], buf[2]), (255, 255, 255), "opaque pen 1");
        assert_eq!(
            (buf[3], buf[4], buf[5]),
            (0, 0, 0),
            "pen 0 transparent → black"
        );
    }

    #[test]
    fn foreground_fg_color_offsets_palette() {
        let mut board = CongoBongoBoard::new();
        board.tx_rom[8] = 0b1000_0000; // tile 1, pen 1 at (0,0)
        board.decode_gfx_roms();
        // fg_color (CREF1) adds 0x80, so color 0 pen 1 resolves at palette[0x81].
        board.palette_prom[0x81] = 0xFF;
        board.build_palette();
        // fg_bank=1 → code 0x101 wraps to tile 1 (256-tile ROM); fg_color set.
        board.main_map.region_data_mut(MainRegion::VideoRam)[0] = 0x01;
        board.main_map.region_data_mut(MainRegion::ColorRam)[0] = 0x00;
        board.latch2 = (1 << 6) | (1 << 1); // fg bank (BS) + fg color (CREF1)

        board.render_scanline(0);
        let buf = &board.scanline_buffer;
        assert_eq!((buf[0], buf[1], buf[2]), (255, 255, 255));
    }

    #[test]
    fn background_skew_source_coords() {
        let mut board = CongoBongoBoard::new();
        // No scroll: srcy = ((0<<1)^0xfff)+1 = 0x1000 & 0xfff = 0; srcx(0) = 0x3f.
        assert_eq!(board.bg_src_y(0), 0);
        assert_eq!(CongoBongoBoard::bg_src_x(0, 0), 0x3F);
        // Successive rows step the skew column left by one every two lines.
        assert_eq!(CongoBongoBoard::bg_src_x(0, 1), 0x40);
        assert_eq!(CongoBongoBoard::bg_src_x(2, 0), 0x3E);

        // 11-bit scroll split across the two position bytes (0xC028/0xC029).
        board.bg_position = [0x10, 0x01]; // bgpos = 0x110
        assert_eq!(board.bg_src_y(0), 0xDE0);
    }

    #[test]
    fn background_pixmap_and_render() {
        let mut board = CongoBongoBoard::new();
        // Every bg cell: code 0, color 1 (attr 0x10) → pixmap pen = 1*8 + 0 = 8.
        for b in board.tilemap_dat[0x2000..0x4000].iter_mut() {
            *b = 0x10;
        }
        board.decode_gfx_roms();
        board.build_bg_pixmap();
        assert_eq!(board.bg_pixmap.len(), 256 * 4096);
        assert_eq!(board.bg_pixmap[0], 8, "color 1, pen 0");

        board.palette_prom[8] = 0xFF; // palette[8] = white
        board.build_palette();

        // Disabled → row stays black.
        board.bg_enabled = false;
        board.render_scanline(100);
        let off = 100 * NATIVE_WIDTH * 3;
        assert_eq!(
            (board.scanline_buffer[off], board.scanline_buffer[off + 2]),
            (0, 0)
        );

        // Enabled, uniform map → every pixel resolves to palette[8] = white.
        board.bg_enabled = true;
        board.render_scanline(100);
        assert_eq!(
            (
                board.scanline_buffer[off],
                board.scanline_buffer[off + 1],
                board.scanline_buffer[off + 2]
            ),
            (255, 255, 255)
        );
    }

    #[test]
    fn sprite_dma_copies_descriptors_into_sprite_ram() {
        let mut sys = CongoBongoSystem::new();
        // Build one sprite descriptor in work RAM at 0x8100: slot byte then the
        // four sprite bytes (Y, code, color, X).
        let desc = [0x03u8, 0xAA, 0xBB, 0xCC, 0xDD]; // slot 3 → sprite_ram[12..16]
        for (i, b) in desc.iter().enumerate() {
            sys.write(BusMaster::Cpu(0), 0x8100 + i as u16, *b);
        }
        // Program the DMA: source 0x8100, count 0 (one descriptor), then trigger.
        sys.write(BusMaster::Cpu(0), 0xC030, 0x00);
        sys.write(BusMaster::Cpu(0), 0xC031, 0x81);
        sys.write(BusMaster::Cpu(0), 0xC032, 0x00);
        sys.write(BusMaster::Cpu(0), 0xC033, 0x01); // go

        assert_eq!(&sys.board.sprite_ram[12..16], &[0xAA, 0xBB, 0xCC, 0xDD]);
    }

    #[test]
    fn sprite_positioning_helpers_match_mame() {
        // find_minimum_x (upright) = value + 0xf0, wrapped to 8 bits.
        assert_eq!(CongoBongoBoard::find_minimum_x(0x10, false), 0x00);
        assert_eq!(CongoBongoBoard::find_minimum_x(0x00, false), 0xF0);
        // find_minimum_y returns a value in 0..=0x100 (top scanline + 1).
        let y = CongoBongoBoard::find_minimum_y(0x20, false);
        assert!((0..=0x100).contains(&y));
    }

    #[test]
    fn sound_command_reaches_ppi_port_a() {
        let mut sys = CongoBongoSystem::new();
        // Main CPU writes the sound command (0xC038); the sound CPU reads it back
        // through PPI port A (configured as input by control word 0x90 at 0x8003).
        sys.write(BusMaster::Cpu(0), 0xC038, 0x7e);
        sys.write(BusMaster::Cpu(1), 0x8003, 0x90); // PPI: A in, B/C out
        assert_eq!(sys.read(BusMaster::Cpu(1), 0x8000), 0x7e);
    }

    #[test]
    fn sound_writes_route_to_psgs_and_ppi() {
        let mut sys = CongoBongoSystem::new();
        sys.write(BusMaster::Cpu(1), 0x8003, 0x90); // PPI mode
        // Port B/C outputs (percussion lines) latch and read back.
        sys.write(BusMaster::Cpu(1), 0x8001, 0x02); // port B
        sys.write(BusMaster::Cpu(1), 0x8002, 0x0d); // port C
        assert_eq!(sys.board.ppi.read_output_b(), 0x02);
        assert_eq!(sys.board.ppi.read_output_c(), 0x0d);
        // PSG #1 (0x6000) and #2 (0xA000) accept register writes (set a tone period).
        sys.write(BusMaster::Cpu(1), 0x6000, 0x80 | 0x04); // sn1 tone0 low nibble
        sys.write(BusMaster::Cpu(1), 0xA000, 0x80 | 0x04); // sn2 tone0 low nibble
        assert_eq!(sys.board.sn1.debug_registers()[0].value, 0x04);
        assert_eq!(sys.board.sn2.debug_registers()[0].value, 0x04);
    }

    #[test]
    fn sound_irq_fires_periodically() {
        let mut sys = CongoBongoSystem::new();
        sys.reset();
        // Run a couple of frames; the ~244 Hz timer must assert the sound IRQ at
        // least once (and the audio buffer must fill).
        // One frame is ~66k sound cycles ⇒ several 16384-cycle IRQ periods.
        let mut fired = false;
        for _ in 0..TIMING.cycles_per_frame() {
            bus_split!(&mut sys, bus => { sys.board.tick(bus); });
            if sys.board.sound_irq_pending {
                fired = true;
            }
        }
        assert!(fired, "sound CPU periodic IRQ asserted");

        let mut audio = vec![0i16; 64];
        let _ = sys.board.fill_audio(&mut audio);
    }

    #[test]
    fn sprite_renders_into_scanline() {
        let mut board = CongoBongoBoard::new();
        // Sprite 0, row 0, col 0 → plane-0 set (pen 1). Sprite plane 0 is at the
        // top third of the ROM; byte 0 bit 7 is sub-cell (0,0) row 0 col 0.
        board.spr_rom[0] = 0b1000_0000;
        board.decode_gfx_roms();
        assert_eq!(board.sprite_cache.pixel(0, 0, 0), 1);
        // color 0, pen 1 → palette[1].
        board.palette_prom[1] = 0xFF;
        board.build_palette();

        // One sprite, slot 0: Y, code 0, color 0, X. Pick X so the left column
        // lands at screen x 0: find_minimum_x(value) = value + 0xf0 = 0 → 0x10.
        board.sprite_ram[0] = 0x80; // Y (places the sprite somewhere on screen)
        board.sprite_ram[1] = 0x00; // code 0, no flip
        board.sprite_ram[2] = 0x00; // color 0, no flip
        board.sprite_ram[3] = 0x10; // X → screen column 0

        let sy = CongoBongoBoard::find_minimum_y(0x80, false) as usize;
        board.render_scanline(sy); // top row of the sprite
        let off = sy * NATIVE_WIDTH * 3;
        assert_eq!(
            (
                board.scanline_buffer[off],
                board.scanline_buffer[off + 1],
                board.scanline_buffer[off + 2]
            ),
            (255, 255, 255),
            "sprite pen 1 drawn at column 0"
        );
    }

    fn press(sys: &mut CongoBongoSystem, id: u16, pressed: bool) {
        sys.handle_input(InputEvent::Button {
            id: InputId(id),
            pressed,
        });
    }

    #[test]
    fn input_maps_to_port_bits() {
        let mut sys = CongoBongoSystem::new();
        press(&mut sys, INPUT_P1_RIGHT, true);
        press(&mut sys, INPUT_P1_UP, true);
        press(&mut sys, INPUT_P1_BUTTON, true);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xC000), 0b0001_0101);

        press(&mut sys, INPUT_P2_LEFT, true);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xC001), 0b0000_0010);

        press(&mut sys, INPUT_P1_START, true);
        press(&mut sys, INPUT_P2_START, true);
        // SW100 start bits (2,3); no coins armed yet.
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xC008) & 0x0C, 0x0C);

        // Releasing clears the bit.
        press(&mut sys, INPUT_P1_RIGHT, false);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xC000) & 0x01, 0);
    }

    #[test]
    fn coin_latches_only_when_armed_and_clears_on_ack() {
        let mut sys = CongoBongoSystem::new();
        // Not armed → coin ignored (SW100 bit5 stays low).
        press(&mut sys, INPUT_COIN1, true);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xC008) & 0x20, 0);

        // Arm coin 1 (latch1 bit0 high), then insert → latches into SW100 bit5.
        sys.write(BusMaster::Cpu(0), 0xC018, 0x01);
        press(&mut sys, INPUT_COIN1, true);
        assert_eq!(
            sys.read(BusMaster::Cpu(0), 0xC008) & 0x20,
            0x20,
            "coin latched"
        );

        // Acknowledge as the game does (0x0B73): pulse the coin's own enable line
        // low → the latch async-clears immediately, without touching bit 2.
        sys.write(BusMaster::Cpu(0), 0xC018, 0x00);
        assert_eq!(
            sys.read(BusMaster::Cpu(0), 0xC008) & 0x20,
            0,
            "coin cleared"
        );
        sys.write(BusMaster::Cpu(0), 0xC018, 0x01); // re-arm for the next coin
    }

    #[test]
    fn dip_banks_valid_and_defaults() {
        crate::assert_dip_banks_valid(CONGO_DIP_BANKS, &[DSW2_DEFAULT, DSW3_DEFAULT]);
        let mut sys = CongoBongoSystem::new();
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xC002), DSW2_DEFAULT);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xC003), DSW3_DEFAULT);
    }

    #[test]
    fn input_controls_have_stable_names() {
        let sys = CongoBongoSystem::new();
        let names: Vec<_> = sys.input_controls().iter().map(|c| c.stable_name).collect();
        for expected in ["p1_right", "p1_button", "p2_down", "coin1", "service"] {
            assert!(names.contains(&expected), "missing {expected}");
        }
    }

    #[test]
    fn boots_and_runs_frames_without_panicking() {
        let mut sys = CongoBongoSystem::new();
        sys.reset();
        for _ in 0..3 {
            sys.run_frame();
        }
        let (w, h) = TIMING.display_size();
        assert_eq!((w, h), (224, 256));
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.board.render_frame(&mut buf);
    }

    #[test]
    fn bus_decodes_ram_video_color() {
        let mut sys = CongoBongoSystem::new();
        for (addr, val) in [(0x8000u16, 0x11u8), (0xA000, 0x22), (0xA400, 0x33)] {
            sys.write(BusMaster::Cpu(0), addr, val);
            assert_eq!(sys.read(BusMaster::Cpu(0), addr), val, "addr {addr:#06x}");
        }
        // Video/color RAM mirrors (+0x800) alias the same backing store.
        sys.write(BusMaster::Cpu(0), 0xA800, 0x44);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xA000), 0x44);

        // Program ROM is read-only: writes are ignored.
        sys.board.main_map.region_data_mut(MainRegion::Rom)[0] = 0xAB;
        sys.write(BusMaster::Cpu(0), 0x0000, 0x00);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0x0000), 0xAB);
    }

    #[test]
    fn bus_decodes_inputs_and_control_latches() {
        let mut sys = CongoBongoSystem::new();
        sys.board.in0 = 0x55;
        sys.board.in1 = 0xAA;
        sys.board.dsw2 = 0x3C;
        sys.board.dsw3 = 0x12;
        sys.board.in2 = 0x77;
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xC000), 0x55);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xC001), 0xAA);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xC002), 0x3C);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xC003), 0x12);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xC008), 0x77);

        // LS259: INTON (latch1 Q7) gates the VBlank IRQ.
        sys.write(BusMaster::Cpu(0), 0xC01F, 0x01);
        assert!(sys.board.int_enabled);
        sys.write(BusMaster::Cpu(0), 0xC01F, 0x00);
        assert!(!sys.board.int_enabled);

        // BEN (latch1 Q5) toggles the background enable.
        sys.write(BusMaster::Cpu(0), 0xC01D, 0x01);
        assert!(sys.board.bg_enabled);

        // Scroll, sprite-DMA, and sound latches store their bytes.
        sys.write(BusMaster::Cpu(0), 0xC028, 0x84);
        assert_eq!(sys.board.bg_position[0], 0x84);
        sys.write(BusMaster::Cpu(0), 0xC032, 0x09);
        assert_eq!(sys.board.sprite_dma[2], 0x09);
        sys.write(BusMaster::Cpu(0), 0xC038, 0x5A);
        assert_eq!(sys.board.sound_latch, 0x5A);
    }

    #[test]
    fn vblank_irq_respects_int_enable() {
        let mut sys = CongoBongoSystem::new();
        // Disabled: no IRQ even at the VBlank cycle.
        assert!(!sys.check_interrupts(BusMaster::Cpu(0)).irq);

        sys.board.int_enabled = true;
        sys.board.vblank_irq_pending = true;
        assert!(sys.check_interrupts(BusMaster::Cpu(0)).irq);

        // Clearing INTON via the latch drops the pending IRQ.
        sys.write(BusMaster::Cpu(0), 0xC01F, 0x00);
        assert!(!sys.check_interrupts(BusMaster::Cpu(0)).irq);
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = CongoBongoSystem::new();
        sys.write(BusMaster::Cpu(0), 0xA000, 0xC3);
        sys.board.latch2 = 0xC0;
        sys.board.sound_latch = 0x7E;
        sys.board.clock = 12345;

        let data = SaveState::save_state(&sys).expect("save_state should return Some");

        let mut sys2 = CongoBongoSystem::new();
        SaveState::load_state(&mut sys2, &data).unwrap();
        assert_eq!(sys2.read(BusMaster::Cpu(0), 0xA000), 0xC3);
        assert_eq!(sys2.board.latch2, 0xC0);
        assert_eq!(sys2.board.sound_latch, 0x7E);
        assert_eq!(sys2.board.clock, 12345);
    }

    #[test]
    fn gfx_regions_registered_with_expected_geometry() {
        let regions = crate::gfx_registry::regions_for("congobongo");
        assert_eq!(
            regions.iter().map(|r| r.region).collect::<Vec<_>>(),
            vec!["bg", "fg", "sprites"],
        );

        let fg = crate::gfx_registry::find("congobongo", "fg").unwrap();
        assert_eq!((fg.count, fg.width, fg.height), (256, 8, 8));
        let bg = crate::gfx_registry::find("congobongo", "bg").unwrap();
        assert_eq!((bg.count, bg.width, bg.height), (1024, 8, 8));
        let spr = crate::gfx_registry::find("congobongo", "sprites").unwrap();
        assert_eq!((spr.count, spr.width, spr.height), (128, 32, 32));

        for r in [fg, bg, spr] {
            assert!(r.palette.is_some(), "{} carries the PROM palette", r.region);
        }
    }

    /// The gfxview `GfxRegion` layouts must decode byte-for-byte identically to
    /// the runtime `decode_gfx_roms()` (already validated by the working
    /// scanline renderer), so the offline export matches what the game shows.
    #[test]
    fn gfx_region_layouts_match_runtime_decode() {
        let mut board = CongoBongoBoard::new();
        for (i, b) in board.tx_rom.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(31).wrapping_add(7);
        }
        for (i, b) in board.bg_rom.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(17).wrapping_add(3);
        }
        for (i, b) in board.spr_rom.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(13).wrapping_add(5);
        }
        board.decode_gfx_roms();

        let fg = crate::gfx_registry::find("congobongo", "fg").unwrap();
        assert_caches_eq(
            &decode_gfx(&board.tx_rom, 0, fg.count as usize, fg.layout),
            &board.tx_cache,
        );
        let bg = crate::gfx_registry::find("congobongo", "bg").unwrap();
        assert_caches_eq(
            &decode_gfx(&board.bg_rom, 0, bg.count as usize, bg.layout),
            &board.bg_cache,
        );
        let spr = crate::gfx_registry::find("congobongo", "sprites").unwrap();
        assert_caches_eq(
            &decode_gfx(&board.spr_rom, 0, spr.count as usize, spr.layout),
            &board.sprite_cache,
        );
    }

    /// The gfxview palette hook must apply the same resistor-DAC math as the
    /// runtime `build_palette()` — both route through `congo_palette_rgb`.
    #[test]
    fn gfx_palette_matches_runtime_build() {
        let mut prom = [0u8; 0x200];
        for (i, b) in prom.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(53).wrapping_add(11);
        }

        let mut board = CongoBongoBoard::new();
        board.palette_prom.copy_from_slice(&prom);
        board.build_palette();

        assert_eq!(congo_palette_rgb(&prom), board.palette_rgb);
    }

    fn assert_caches_eq(a: &gfx::GfxCache, b: &gfx::GfxCache) {
        assert_eq!(
            (a.count(), a.width(), a.height()),
            (b.count(), b.width(), b.height())
        );
        for code in 0..a.count() {
            for py in 0..a.height() {
                for px in 0..a.width() {
                    assert_eq!(
                        a.pixel(code, px, py),
                        b.pixel(code, px, py),
                        "pixel mismatch at code {code} ({px},{py})"
                    );
                }
            }
        }
    }
}
