//! Nintendo Mario Bros. (1983).
//!
//! Hardware (per MAME `src/mame/nintendo/mario.cpp`):
//! - Main CPU: Z80 @ 4 MHz (8 MHz XTAL / 2)
//! - Sound CPU: Mitsubishi M58715 (8049-clone, runs from external ROM ⇒
//!   functionally an I8039) @ 11 MHz, driving an 8-bit DAC
//! - Discrete analog sound (Mario/Luigi walk + skid) — **deferred**, silent here
//! - Video: 32×32 8×8 2bpp tilemap + 256 16×16 3bpp sprites, single 512-byte
//!   palette PROM, ROT0 (horizontal), 256×224 visible
//! - Sprite list moved by a Z80 DMA controller (I/O port 0x00)
//!
//! This is a close sibling of the Donkey Kong board (`tkg04.rs`); the renderer,
//! palette resistor-network model, and sound-CPU/DAC wiring follow the same
//! patterns. The notable differences are: no screen rotation, 3bpp sprites, a
//! single palette PROM, a vertical scroll register, and the Z80 DMA.
//!
//! **Deferred:** discrete walk/skid analog sound; faithful cocktail flip-screen
//! pixel offsets (flip is approximated as a 180° rotation and is unused on the
//! upright cabinet this set targets).

use phosphor_core::audio::AudioResampler;
use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind, DebugTraceBuffer};
use phosphor_core::core::machine::{
    ActionRole, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, DipSwitches, Direction,
    InputConfigurable, InputControl, InputEvent, InputId, InputKind, MachineCore, Nvram, SaveState,
};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::watchpoint::DebugAccessSource;
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, ClockDivider, TimingConfig};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::i8035::I8035;
use phosphor_core::cpu::z80::Z80;
use phosphor_core::device::dac::Mc1408Dac;
use phosphor_core::gfx;
use phosphor_core::gfx::decode::{GfxLayout, decode_gfx};
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion, Saveable};

use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::gfx_registry::GfxRegion;
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_high;
use crate::tkg04::{
    DARLINGTON_BIAS_R, DARLINGTON_RESISTORS, EMITTER_BIAS_R, EMITTER_RESISTORS,
    compute_tkg04_channel,
};
use crate::z80dma::Z80Dma;

// ---------------------------------------------------------------------------
// Memory map region IDs
// ---------------------------------------------------------------------------

/// Main CPU (Z80) address space regions.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum MainRegion {
    Rom = 1,       // 0x0000-0x5FFF (24KB program ROM)
    Ram = 2,       // 0x6000-0x67FF (2KB work RAM)
    Nvram = 3,     // 0x6800-0x6FFF (2KB battery-backed RAM)
    SpriteRam = 4, // 0x7000-0x73FF (1KB sprite RAM)
    VideoRam = 5,  // 0x7400-0x77FF (1KB video RAM)
    IoPorts = 6,   // 0x7C00-0x7FFF (input/control ports)
    RomHigh = 7,   // 0xF000-0xFFFF (4KB program ROM, upper)
}

/// Sound CPU (I8039) address space regions.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum SoundRegion {
    Rom = 1, // 0x0000-0x0FFF (4KB external sound ROM)
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------
// Main CPU:    8 MHz XTAL / 2 = 4.000 MHz Z80
// Pixel clock: 24 MHz XTAL / 4 = 6.000 MHz
// HTOTAL:      384 px → 256 Z80 cycles per scanline (384/6MHz × 4MHz)
// VTOTAL:      264 lines; visible Y 16..239 (224 lines), VBLANK at 240
// Frame:       256 × 264 = 67584 cycles → 4 MHz / 67584 ≈ 59.18 Hz

pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 4_000_000,
    cycles_per_scanline: 256,
    total_scanlines: 264,
    display_width: NATIVE_WIDTH as u32, // 256 (no rotation)
    display_height: (NATIVE_HEIGHT - VBLANK_END) as u32, // 224
    display_aspect: Some((4, 3)),
};

pub const VISIBLE_LINES: u64 = 240; // lines rendered (top VBLANK_END clipped on output)
pub const OUTPUT_SAMPLE_RATE: u64 = 44_100;

pub const NATIVE_WIDTH: usize = 256;
pub const NATIVE_HEIGHT: usize = 240;
pub const VBLANK_END: usize = 16; // first visible scanline

// Sound CPU: 8049 @ 11 MHz / 15 = 733.3 kHz machine cycles.
// Bresenham ratio to the 4 MHz Z80: (11e6/15) / 4e6 = 11 / 60.
pub const SOUND_TICK_NUM: u32 = 11;
pub const SOUND_TICK_DEN: u32 = 60;

// ---------------------------------------------------------------------------
// ROM definitions ("mario" parent set)
// ---------------------------------------------------------------------------

/// Program ROM at 0x0000-0x5FFF (three 8KB chips).
pub static MARIO_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        RomEntry {
            name: "tma1-c-7f_e-1.7f",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xc0c6e014],
        },
        RomEntry {
            name: "tma1-c-7e_e-3.7e",
            size: 0x2000,
            offset: 0x2000,
            // mario (E) | mariog (G)
            crc32: &[0xb09ab857, 0x116b3856],
        },
        RomEntry {
            name: "tma1-c-7d_e-1.7d",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0xdcceb6c1],
        },
    ],
};

/// Upper program ROM at 0xF000-0xFFFF (4KB).
pub static MARIO_PROGRAM_ROM_HIGH: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "tma1-c-7c_e-3.7c",
        size: 0x1000,
        offset: 0x0000,
        // mario (E) | mariog/mariof (G/F)
        crc32: &[0x0d31bd1c, 0x4a63d96b],
    }],
};

/// Sound CPU external ROM (4KB).
pub static MARIO_SOUND_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "tma1-c-6k_e.6k",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0x06b9ff85],
    }],
};

// Disassemblable code regions for the standalone `disasm` tool.
// `main`  — the Z80 game program (0x0000-0x5FFF).
// `sound` — the I8035 (M58715) external sound program (0x0000-0x0FFF).
inventory::submit! {
    DisasmRegion {
        machine: "mariobros",
        region: "main",
        cpu: DisasmCpu::Z80,
        org: 0,
        size: MARIO_PROGRAM_ROM.size as u32,
        load: |rs| MARIO_PROGRAM_ROM.load(rs),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "mariobros",
        region: "sound",
        cpu: DisasmCpu::I8035,
        org: 0,
        size: MARIO_SOUND_ROM.size as u32,
        load: |rs| MARIO_SOUND_ROM.load(rs),
    }
}

/// Tile GFX (gfx1): 8KB, two 4KB ROMs (one per bitplane).
pub static MARIO_TILE_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "tma1-v-3f.3f",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x28b0c42c],
        },
        RomEntry {
            name: "tma1-v-3j.3j",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x0c8cc04d],
        },
    ],
};

/// Sprite GFX (gfx2): 24KB, six 4KB ROMs (three bitplanes, two halves each).
pub static MARIO_SPRITE_ROM: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        RomEntry {
            name: "tma1-v.7m.7m",
            size: 0x1000,
            offset: 0x0000,
            // mario (E) | mariog (G)
            crc32: &[0xd01c0e2c, 0x22b7372e],
        },
        RomEntry {
            name: "tma1-v-7n.7n",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x4f3a1f47],
        },
        RomEntry {
            name: "tma1-v-7p.7p",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0x56be6ccd],
        },
        RomEntry {
            name: "tma1-v.7s.7s",
            size: 0x1000,
            offset: 0x3000,
            // mario (E) | mariog (G)
            crc32: &[0xff856e6f, 0x56f1d613],
        },
        RomEntry {
            name: "tma1-v-7t.7t",
            size: 0x1000,
            offset: 0x4000,
            crc32: &[0x641f0008],
        },
        RomEntry {
            name: "tma1-v.7u.7u",
            size: 0x1000,
            offset: 0x5000,
            // mario (E) | mariog (G)
            crc32: &[0xd2dbeb75, 0x7baf5309],
        },
    ],
};

/// Palette PROM (512 bytes: inverted half 0-255, std-monitor half 256-511).
pub static MARIO_PALETTE_PROM: RomRegion = RomRegion {
    size: 0x0200,
    entries: &[RomEntry {
        name: "tma1-c-4p.4p",
        size: 0x0200,
        offset: 0x0000,
        crc32: &[0xafc9bd41],
    }],
};

// GFX bit-plane layouts, promoted to `'static` so both the runtime decode
// (`decode_gfx_roms`) and the gfxview `GfxRegion`s can borrow the same tables.

/// Tiles: 512 chars, 8×8, 2bpp; the two bitplanes are separated by 512*8*8 bits.
pub static MARIO_TILE_GFX_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 512 * 8 * 8],
    x_offsets: &[0, 1, 2, 3, 4, 5, 6, 7],
    y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
    char_increment: 8 * 8,
};

/// Bit distance between the two 8-pixel halves of a sprite row (256*16*8).
const MARIO_SPRITE_HALF: usize = 256 * 16 * 8;

/// Sprites: 256 chars, 16×16, 3bpp. Planes separated by 256*16*16 bits; the
/// two 8-pixel halves of each row are separated by [`MARIO_SPRITE_HALF`] bits.
pub static MARIO_SPRITE_GFX_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 256 * 16 * 16, 2 * 256 * 16 * 16],
    x_offsets: &[
        0,
        1,
        2,
        3,
        4,
        5,
        6,
        7,
        MARIO_SPRITE_HALF,
        MARIO_SPRITE_HALF + 1,
        MARIO_SPRITE_HALF + 2,
        MARIO_SPRITE_HALF + 3,
        MARIO_SPRITE_HALF + 4,
        MARIO_SPRITE_HALF + 5,
        MARIO_SPRITE_HALF + 6,
        MARIO_SPRITE_HALF + 7,
    ],
    y_offsets: &[
        0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104, 112, 120,
    ],
    char_increment: 16 * 8,
};

// gfxview GFX regions: the DK-family reference charset/sprite sheets. Both share
// the 256-entry PROM palette via `mario_palette_rgb` (see `build_palette`).
inventory::submit! {
    GfxRegion {
        machine: "mariobros",
        region: "tiles",
        count: 512,
        width: 8,
        height: 8,
        layout: &MARIO_TILE_GFX_LAYOUT,
        load: |rs| MARIO_TILE_ROM.load(rs),
        palette: Some(mario_gfx_palette),
    }
}
inventory::submit! {
    GfxRegion {
        machine: "mariobros",
        region: "sprites",
        count: 256,
        width: 16,
        height: 16,
        layout: &MARIO_SPRITE_GFX_LAYOUT,
        load: |rs| MARIO_SPRITE_ROM.load(rs),
        palette: Some(mario_gfx_palette),
    }
}

/// gfxview palette hook: load the color PROM and build the RGB palette with the
/// same resistor-net math as the runtime path.
fn mario_gfx_palette(rom_set: &RomSet) -> Result<Vec<(u8, u8, u8)>, RomLoadError> {
    let prom = MARIO_PALETTE_PROM.load(rom_set)?;
    Ok(mario_palette_rgb(&prom).to_vec())
}

/// Build the 256-entry RGB palette from the 512-byte PROM's inverted half
/// (bytes 0-255) using the shared Nintendo resistor-network / monitor model.
/// R = PROM bits 7-5, G = bits 4-2, B = bits 1-0 (LSB-first within each field).
///
/// Shared by [`MarioBrosBoard::build_palette`] and [`mario_gfx_palette`] so the
/// runtime renderer and the offline viewer never diverge.
fn mario_palette_rgb(palette_prom: &[u8]) -> [(u8, u8, u8); 256] {
    let mut raw: [(f64, f64, f64); 256] = [(0.0, 0.0, 0.0); 256];
    for (i, entry) in raw.iter_mut().enumerate() {
        let v = palette_prom[i];
        let r_bits = [
            ((v >> 5) & 1) as f64,
            ((v >> 6) & 1) as f64,
            ((v >> 7) & 1) as f64,
        ];
        let g_bits = [
            ((v >> 2) & 1) as f64,
            ((v >> 3) & 1) as f64,
            ((v >> 4) & 1) as f64,
        ];
        let b_bits = [(v & 1) as f64, ((v >> 1) & 1) as f64];
        let r = compute_tkg04_channel(&r_bits, &DARLINGTON_RESISTORS, DARLINGTON_BIAS_R, true);
        let g = compute_tkg04_channel(&g_bits, &DARLINGTON_RESISTORS, DARLINGTON_BIAS_R, true);
        let b = compute_tkg04_channel(&b_bits, &EMITTER_RESISTORS, EMITTER_BIAS_R, false);
        *entry = (r, g, b);
    }

    let max_val = raw
        .iter()
        .flat_map(|&(r, g, b)| [r, g, b])
        .fold(0.0f64, f64::max);
    let scale = if max_val > 0.0 { 255.0 / max_val } else { 1.0 };
    let mut out = [(0u8, 0u8, 0u8); 256];
    for (o, &(r, g, b)) in out.iter_mut().zip(raw.iter()) {
        *o = (
            (r * scale).round().min(255.0) as u8,
            (g * scale).round().min(255.0) as u8,
            (b * scale).round().min(255.0) as u8,
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Input button IDs (active-high: 0x00 = all released)
// ---------------------------------------------------------------------------
pub const INPUT_P1_RIGHT: u8 = 0;
pub const INPUT_P1_LEFT: u8 = 1;
pub const INPUT_P1_JUMP: u8 = 2;
pub const INPUT_P2_RIGHT: u8 = 3;
pub const INPUT_P2_LEFT: u8 = 4;
pub const INPUT_P2_JUMP: u8 = 5;
pub const INPUT_P1_START: u8 = 6;
pub const INPUT_P2_START: u8 = 7;
pub const INPUT_COIN: u8 = 8;
pub const INPUT_COIN2: u8 = 9;
pub const INPUT_SERVICE: u8 = 10;

/// Typed logical controls. Mario Bros. is a 2-player, 2-way (left/right) + jump
/// game; both players share the upright screen.
pub const MARIO_CONTROLS: &[InputControl] = &[
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
        id: InputId(INPUT_P1_JUMP as u16),
        stable_name: "p1_jump",
        label: "P1 Jump",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
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
        id: InputId(INPUT_P2_JUMP as u16),
        stable_name: "p2_jump",
        label: "P2 Jump",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(2),
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
    InputControl {
        id: InputId(INPUT_COIN as u16),
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
        player: None,
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_SERVICE as u16),
        stable_name: "service",
        label: "Service",
        kind: InputKind::Service,
        player: None,
        default_bindings: crate::input_defaults::SERVICE,
    },
];

// ---------------------------------------------------------------------------
// MarioBrosBoard
// ---------------------------------------------------------------------------

#[derive(BusDebug, DebugTrace)]
pub struct MarioBrosBoard {
    #[debug_cpu("Z80 Main")]
    pub(crate) cpu: Z80,
    #[debug_cpu("I8039 Sound")]
    pub(crate) sound_cpu: I8035,

    #[debug_map(cpu = 0)]
    pub(crate) main_map: AddressSpace16,
    #[debug_map(cpu = 1)]
    pub(crate) sound_map: AddressSpace16,

    // GFX ROMs
    pub(crate) tile_rom: [u8; 0x2000],   // gfx1 (2 planes × 4KB)
    pub(crate) sprite_rom: [u8; 0x6000], // gfx2 (3 planes × 8KB)

    // Palette PROM (inverted half used for the default Nintendo monitor)
    pub(crate) palette_prom: [u8; 0x0200],
    pub(crate) palette_rgb: [(u8, u8, u8); 256],

    // Scanline-rendered framebuffer (256 × 240 × RGB24)
    pub(crate) scanline_buffer: Vec<u8>,

    // I/O state (active-high)
    pub(crate) in0: u8,
    pub(crate) in1: u8,
    pub(crate) dsw: u8,

    // Sound command + trigger latches (MAME soundlatch[0/1/3])
    pub(crate) sound_latch: u8,  // 0x7E00 → 8039 command (P2 bit7 select)
    pub(crate) sound_latch1: u8, // crab/turtle/fly/coin → 8039 P1 input
    pub(crate) sound_latch3: u8, // get-coin/ice → 8039 T0/T1 inputs

    // LS259 control bits + scroll
    pub(crate) flip_screen: bool,
    pub(crate) gfx_bank: u8,
    pub(crate) palette_bank: u8,
    pub(crate) nmi_mask: bool,
    pub(crate) scroll_y: u8,

    pub(crate) sound_irq_pending: bool,

    // Z80 DMA controller (sprite-list transfer)
    pub(crate) dma: Z80Dma,

    // Audio output (8-bit DAC + resampler; discrete sound deferred)
    #[debug_device("DAC")]
    pub(crate) dac: Mc1408Dac,
    pub(crate) resampler: AudioResampler<i16>,

    // Pre-decoded GFX caches
    pub(crate) tile_cache: gfx::GfxCache,
    pub(crate) sprite_cache: gfx::GfxCache,

    // Timing
    pub(crate) clock: u64,
    pub(crate) sound_clock: ClockDivider,
    pub(crate) vblank_nmi_pending: bool,

    #[debug_events]
    pub(crate) debug_trace: DebugTraceBuffer,
}

impl Default for MarioBrosBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl MarioBrosBoard {
    pub fn new() -> Self {
        let mut sound_cpu = I8035::new();
        sound_cpu.ram_mask = 0x7F; // 8039/8049: 128 bytes of internal RAM
        Self {
            cpu: Z80::new(),
            sound_cpu,
            main_map: Self::build_main_map(),
            sound_map: Self::build_sound_map(),
            tile_rom: [0; 0x2000],
            sprite_rom: [0; 0x6000],
            palette_prom: [0; 0x0200],
            palette_rgb: [(0, 0, 0); 256],
            scanline_buffer: vec![0u8; NATIVE_WIDTH * NATIVE_HEIGHT * 3],
            in0: 0x00,
            in1: 0x00,
            dsw: 0x00,
            sound_latch: 0,
            sound_latch1: 0,
            sound_latch3: 0,
            flip_screen: false,
            gfx_bank: 0,
            palette_bank: 0,
            nmi_mask: false,
            scroll_y: 0,
            sound_irq_pending: false,
            dma: Z80Dma::new(),
            dac: Mc1408Dac::new(),
            resampler: AudioResampler::new(TIMING.cpu_clock_hz, OUTPUT_SAMPLE_RATE),
            tile_cache: gfx::GfxCache::new(0, 8, 8),
            sprite_cache: gfx::GfxCache::new(0, 16, 16),
            clock: 0,
            sound_clock: ClockDivider::new(SOUND_TICK_NUM, SOUND_TICK_DEN),
            vblank_nmi_pending: false,
            debug_trace: DebugTraceBuffer::new(),
        }
    }

    fn build_main_map() -> AddressSpace16 {
        use MainRegion::*;
        let mut map = AddressSpace16::new();
        map.region(Rom, "Program ROM", 0x0000, 0x6000, AccessKind::ReadOnly)
            .region(Ram, "Work RAM", 0x6000, 0x0800, AccessKind::ReadWrite)
            .region(Nvram, "NVRAM", 0x6800, 0x0800, AccessKind::ReadWrite)
            .region(
                SpriteRam,
                "Sprite RAM",
                0x7000,
                0x0400,
                AccessKind::ReadWrite,
            )
            .region(VideoRam, "Video RAM", 0x7400, 0x0400, AccessKind::ReadWrite)
            .region(IoPorts, "I/O Ports", 0x7C00, 0x0400, AccessKind::Io)
            .region(
                RomHigh,
                "Program ROM (high)",
                0xF000,
                0x1000,
                AccessKind::ReadOnly,
            );
        map
    }

    fn build_sound_map() -> AddressSpace16 {
        use SoundRegion::*;
        let mut map = AddressSpace16::new();
        map.region(Rom, "Sound ROM", 0x0000, 0x1000, AccessKind::ReadOnly);
        map
    }

    /// Pre-decode tile and sprite ROMs into GFX caches. Call after loading ROMs.
    ///
    /// Uses the same `'static` [`MARIO_TILE_GFX_LAYOUT`] / [`MARIO_SPRITE_GFX_LAYOUT`]
    /// tables the gfxview `GfxRegion`s reference, so the offline sheet export
    /// and the runtime renderer decode identically.
    pub fn decode_gfx_roms(&mut self) {
        self.tile_cache = decode_gfx(&self.tile_rom, 0, 512, &MARIO_TILE_GFX_LAYOUT);
        self.sprite_cache = decode_gfx(&self.sprite_rom, 0, 256, &MARIO_SPRITE_GFX_LAYOUT);
    }

    /// Pre-compute the 256-entry RGB palette from the inverted PROM half.
    ///
    /// Delegates to the shared [`mario_palette_rgb`] so the runtime renderer and
    /// the gfxview export apply identical resistor-net math.
    pub fn build_palette(&mut self) {
        self.palette_rgb = mario_palette_rgb(&self.palette_prom);
    }

    // -----------------------------------------------------------------------
    // Scanline rendering
    // -----------------------------------------------------------------------

    /// Render one screen scanline (`abs_y` is the absolute bitmap row, 0-239).
    pub fn render_scanline(&mut self, abs_y: usize) {
        let row_offset = abs_y * NATIVE_WIDTH * 3;

        let video_ram = self.main_map.region_data(MainRegion::VideoRam);
        let palette_rgb = &self.palette_rgb;
        let tile_cache = &self.tile_cache;
        let sprite_cache = &self.sprite_cache;
        let gfx_bank = self.gfx_bank;
        let palette_bank = self.palette_bank;
        let buf = &mut self.scanline_buffer[row_offset..row_offset + NATIVE_WIDTH * 3];

        // Palette granularity is 8 pens per color code for both tiles and sprites.
        let resolve = |color: u8, pixel: u8| -> (u8, u8, u8) {
            palette_rgb[(color as usize * 8 + pixel as usize) & 0xFF]
        };

        // Vertical scroll: MAME `set_scrolly(0, data + 17)` ⇒ the tilemap source
        // row for screen row Y is `(Y - scrolly) mod 256`.
        let scrolly = (self.scroll_y as i32 + 17) & 0xFF;
        let src_y = ((abs_y as i32 - scrolly) & 0xFF) as usize;

        let config = gfx::TilemapConfig {
            cols: 32,
            rows: 32,
            tile_width: 8,
            tile_height: 8,
        };
        gfx::tilemap::render_tilemap_scanline(
            &config,
            tile_cache,
            src_y,
            |col, row| {
                let v = video_ram[row * 32 + col];
                let tile_code = v as u16 + 256 * gfx_bank as u16;
                let color = 8 + (v >> 5) + 16 * palette_bank;
                (tile_code, color)
            },
            resolve,
            buf,
            0,
        );

        // Sprites: 4 bytes each [y, attr, code, x]; later sprites draw on top.
        // Drawn when non-zero; row test derived from MAME `mario_state::draw_sprites`.
        let sprite_ram = self.main_map.region_data(MainRegion::SpriteRam);
        let mut offs = 0;
        while offs < 0x400 {
            let y_byte = sprite_ram[offs];
            if y_byte != 0 {
                let attr = sprite_ram[offs + 1];
                let code = sprite_ram[offs + 2] as u16;
                let x_byte = sprite_ram[offs + 3];

                let sprite_top = 241 - ((y_byte as i32 + 0xFA) & 0xFF);
                let row = abs_y as i32 - sprite_top;
                if (0..16).contains(&row) {
                    let flip_x = attr & 0x80 != 0;
                    let flip_y = attr & 0x40 != 0;
                    let color = (attr & 0x0F) + 16 * palette_bank;
                    let src_py = if flip_y { 15 - row } else { row } as usize;
                    let sprite_x = x_byte as i32 - 8;

                    let clip = gfx::sprite::SpriteClip {
                        x_min: 0,
                        x_max: NATIVE_WIDTH as i32,
                        wrap_offset: Some(-256),
                    };
                    gfx::sprite::draw_sprite_row(
                        sprite_cache,
                        code,
                        src_py,
                        sprite_x,
                        flip_x,
                        |pv| pv == 0,
                        |pv| resolve(color, pv),
                        buf,
                        &clip,
                    );
                }
            }
            offs += 4;
        }
    }

    // -----------------------------------------------------------------------
    // Core tick
    // -----------------------------------------------------------------------

    /// Execute one Z80 clock cycle (4 MHz).
    pub fn tick(&mut self, bus: &mut dyn Bus<Address = u16, Data = u8>) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();

        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let scanline = (frame_cycle / TIMING.cycles_per_scanline) as u16;
            if scanline < VISIBLE_LINES as u16 {
                self.render_scanline(scanline as usize);
            }
        }

        // VBLANK NMI at scanline 240.
        let vblank_cycle = VISIBLE_LINES * TIMING.cycles_per_scanline;
        if frame_cycle == vblank_cycle {
            self.vblank_nmi_pending = true;
            if self.debug_trace.enabled() {
                self.debug_trace.record(DebugEvent {
                    cpu_index: Some(0),
                    detail: Some(if self.nmi_mask {
                        "VBLANK NMI"
                    } else {
                        "VBLANK NMI (masked)"
                    }),
                    ..DebugEvent::new(
                        self.clock,
                        DebugAccessSource::Unknown,
                        DebugEventKind::InterruptAssert,
                    )
                });
            }
        }
        if frame_cycle == 0 && self.clock > 0 {
            self.vblank_nmi_pending = false;
        }

        if self.main_map.has_any_watchpoints() || self.debug_trace.enabled() {
            let pc = self
                .cpu
                .at_instruction_boundary()
                .then_some(self.cpu.pc as u32);
            self.main_map.latch_access_context(self.clock, pc);
        }

        self.cpu.execute_cycle(bus, BusMaster::Cpu(0));

        // Sound CPU (8049): 11/60 of the Z80 clock.
        if self.sound_clock.tick() {
            self.sound_cpu.execute_cycle(bus, BusMaster::Cpu(1));
        }

        // Audio: 8-bit DAC resampled to 44.1 kHz. (Discrete sound deferred.)
        if let Some(avg) = self.resampler.tick_sample(self.dac.sample_i16()) {
            self.resampler.push_sample(avg);
        }

        self.clock += 1;
    }

    // -----------------------------------------------------------------------
    // Frame output (no rotation)
    // -----------------------------------------------------------------------

    /// Copy the visible region (lines 16..240, full width) to the native RGB24
    /// output. The cocktail flip is declared via [`orientation`](Self::orientation)
    /// and applied centrally by the frontend, so this always emits the upright
    /// (unflipped) image.
    pub fn render_frame(&self, buffer: &mut [u8]) {
        let start = VBLANK_END * NATIVE_WIDTH * 3;
        let visible =
            &self.scanline_buffer[start..start + (NATIVE_HEIGHT - VBLANK_END) * NATIVE_WIDTH * 3];
        buffer.copy_from_slice(visible);
    }

    /// Live screen orientation. The cocktail DIP flips the picture 180° for the
    /// seated second player; this is queried every frame so the flip tracks the
    /// DIP state (`COCKTAIL` == a 180° rotation, i.e. the former hand-rolled
    /// reverse). The upright cabinet returns `NORMAL`.
    pub fn orientation(&self) -> phosphor_core::core::machine::Orientation {
        if self.flip_screen {
            phosphor_core::core::machine::Orientation::COCKTAIL
        } else {
            phosphor_core::core::machine::Orientation::NORMAL
        }
    }

    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.resampler.fill_audio(buffer)
    }

    // -----------------------------------------------------------------------
    // DMA + control helpers
    // -----------------------------------------------------------------------

    /// Run any pending Z80 DMA block transfer (memory-to-memory). Mirrors the
    /// two-phase read-then-write of the sprite-list copy.
    pub fn service_dma(&mut self) {
        while let Some(t) = self.dma.take_transfer() {
            let mut src = t.src;
            let mut dst = t.dst;
            for _ in 0..=t.count {
                let v = self.main_map.debug_read(src).unwrap_or(0xFF);
                if matches!(
                    self.main_map.page(dst).region_id,
                    MainRegion::RAM
                        | MainRegion::NVRAM
                        | MainRegion::SPRITE_RAM
                        | MainRegion::VIDEO_RAM
                ) {
                    self.main_map.write_backing(dst, v);
                }
                src = src.wrapping_add_signed(t.src_step);
                dst = dst.wrapping_add_signed(t.dst_step);
            }
        }
    }

    /// Write one bit of the 74LS259 control latch (0x7E80-0x7E87).
    pub fn write_control_bit(&mut self, bit: u8, value: bool) {
        match bit {
            0 => self.gfx_bank = value as u8,     // ~T ROM (gfx bank)
            1 => {}                               // 2 PSL (unused)
            2 => self.flip_screen = value,        // FLIP
            3 => self.palette_bank = value as u8, // CREF 0
            4 => {
                self.nmi_mask = value; // NMI enable
                if !value {
                    self.vblank_nmi_pending = false;
                }
            }
            5 => {
                self.dma.set_rdy(value); // DMA SET (RDY line)
                self.service_dma();
            }
            6 | 7 => {} // coin counters
            _ => {}
        }
    }

    /// Route a 0x7F00-0x7F07 sample-trigger write to the sound subsystem.
    pub fn write_sample_trigger(&mut self, offset: u16, data: u8) {
        let bit = data & 1 != 0;
        match offset {
            0 => self.sound_irq_pending = bit, // death → 8039 IRQ
            1 | 2 => {
                let mask = 1u8 << (offset - 1); // get-coin / ice → T0/T1
                if bit {
                    self.sound_latch3 |= mask;
                } else {
                    self.sound_latch3 &= !mask;
                }
            }
            3..=6 => {
                let mask = 1u8 << (offset - 3); // crab/turtle/fly/coin → P1
                if bit {
                    self.sound_latch1 |= mask;
                } else {
                    self.sound_latch1 &= !mask;
                }
            }
            7 => {} // skid → discrete (deferred)
            _ => {}
        }
    }

    pub fn reset(&mut self) {
        self.nmi_mask = false;
        self.vblank_nmi_pending = false;
        self.sound_irq_pending = false;
        self.sound_latch = 0;
        self.sound_latch1 = 0;
        self.sound_latch3 = 0;
        self.flip_screen = false;
        self.gfx_bank = 0;
        self.palette_bank = 0;
        self.scroll_y = 0;
        self.dma.reset();

        self.clock = 0;
        self.sound_clock.reset();
        self.resampler.reset();
        self.dac.reset();

        self.in0 = 0x00;
        self.in1 = 0x00;

        self.main_map.region_data_mut(MainRegion::VideoRam).fill(0);
        self.main_map.region_data_mut(MainRegion::Ram).fill(0);
        self.main_map.region_data_mut(MainRegion::SpriteRam).fill(0);
        self.scanline_buffer.fill(0);

        // 8039/8049 RAM size is configuration, not reset by the CPU core.
        self.sound_cpu.ram_mask = 0x7F;
    }

    pub fn check_interrupts(&self, target: BusMaster) -> InterruptState {
        match target {
            BusMaster::Cpu(0) => InterruptState {
                nmi: self.vblank_nmi_pending && self.nmi_mask,
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

impl Saveable for MarioBrosBoard {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        self.sound_cpu.save_state(w);
        w.write_bytes(self.main_map.region_data(MainRegion::Ram));
        w.write_bytes(self.main_map.region_data(MainRegion::Nvram));
        w.write_bytes(self.main_map.region_data(MainRegion::SpriteRam));
        w.write_bytes(self.main_map.region_data(MainRegion::VideoRam));
        w.write_u8(self.in0);
        w.write_u8(self.in1);
        w.write_u8(self.sound_latch);
        w.write_u8(self.sound_latch1);
        w.write_u8(self.sound_latch3);
        w.write_bool(self.flip_screen);
        w.write_u8(self.gfx_bank);
        w.write_u8(self.palette_bank);
        w.write_bool(self.nmi_mask);
        w.write_u8(self.scroll_y);
        w.write_bool(self.sound_irq_pending);
        self.dma.save_state(w);
        self.dac.save_state(w);
        self.resampler.save_state(w);
        w.write_u64_le(self.clock);
        self.sound_clock.save_state(w);
        w.write_bool(self.vblank_nmi_pending);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        self.sound_cpu.load_state(r)?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::Ram))?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::Nvram))?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::SpriteRam))?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::VideoRam))?;
        self.in0 = r.read_u8()?;
        self.in1 = r.read_u8()?;
        self.sound_latch = r.read_u8()?;
        self.sound_latch1 = r.read_u8()?;
        self.sound_latch3 = r.read_u8()?;
        self.flip_screen = r.read_bool()?;
        self.gfx_bank = r.read_u8()?;
        self.palette_bank = r.read_u8()?;
        self.nmi_mask = r.read_bool()?;
        self.scroll_y = r.read_u8()?;
        self.sound_irq_pending = r.read_bool()?;
        self.dma.load_state(r)?;
        self.dac.load_state(r)?;
        self.resampler.load_state(r)?;
        self.clock = r.read_u64_le()?;
        self.sound_clock.load_state(r)?;
        self.vblank_nmi_pending = r.read_bool()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// MarioBrosSystem wrapper
// ---------------------------------------------------------------------------

/// Nintendo Mario Bros. (1983).
#[derive(Saveable)]
pub struct MarioBrosSystem {
    pub board: MarioBrosBoard,
}

impl Default for MarioBrosSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl MarioBrosSystem {
    pub fn new() -> Self {
        Self {
            board: MarioBrosBoard::new(),
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let prog = MARIO_PROGRAM_ROM.load(rom_set)?;
        self.board
            .main_map
            .load_region_at(MainRegion::Rom, 0, &prog);

        let prog_high = MARIO_PROGRAM_ROM_HIGH.load(rom_set)?;
        self.board
            .main_map
            .load_region_at(MainRegion::RomHigh, 0, &prog_high);

        let sound = MARIO_SOUND_ROM.load(rom_set)?;
        self.board
            .sound_map
            .load_region_at(SoundRegion::Rom, 0, &sound);

        let tiles = MARIO_TILE_ROM.load(rom_set)?;
        self.board.tile_rom.copy_from_slice(&tiles);

        let sprites = MARIO_SPRITE_ROM.load(rom_set)?;
        self.board.sprite_rom.copy_from_slice(&sprites);

        let prom = MARIO_PALETTE_PROM.load(rom_set)?;
        self.board.palette_prom.copy_from_slice(&prom);

        self.board.build_palette();
        self.board.decode_gfx_roms();
        Ok(())
    }
}

impl Bus for MarioBrosSystem {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        match master {
            BusMaster::Cpu(0) => {
                let data = match self.board.main_map.page(addr).region_id {
                    MainRegion::ROM
                    | MainRegion::RAM
                    | MainRegion::NVRAM
                    | MainRegion::SPRITE_RAM
                    | MainRegion::VIDEO_RAM
                    | MainRegion::ROM_HIGH => self.board.main_map.read_backing(addr),
                    MainRegion::IO_PORTS => match addr {
                        0x7C00 => self.board.in0,
                        0x7C80 => self.board.in1,
                        0x7F80 => self.board.dsw,
                        _ => 0xFF,
                    },
                    _ => 0xFF,
                };
                self.board.main_map.watch_read(0, master, addr, data);
                data
            }

            // Sound CPU (I8039) program ROM.
            BusMaster::Cpu(1) => self.board.sound_map.read_backing(addr & 0x0FFF),

            _ => 0xFF,
        }
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        match master {
            BusMaster::Cpu(0) => {
                self.board.main_map.watch_write(0, master, addr, data);
                match self.board.main_map.page(addr).region_id {
                    MainRegion::RAM
                    | MainRegion::NVRAM
                    | MainRegion::SPRITE_RAM
                    | MainRegion::VIDEO_RAM => self.board.main_map.write_backing(addr, data),
                    MainRegion::IO_PORTS => match addr {
                        // 0x7C00 / 0x7C80 are reads (inputs); writes trigger the
                        // Mario/Luigi walk discrete sounds (deferred → ignored).
                        0x7C00 | 0x7C80 => {}
                        0x7D00 => self.board.scroll_y = data,
                        0x7E00 => self.board.sound_latch = data,
                        0x7E80..=0x7E87 => {
                            self.board
                                .write_control_bit((addr & 0x07) as u8, data & 1 != 0);
                        }
                        0x7F00..=0x7F07 => {
                            self.board.write_sample_trigger(addr & 0x07, data);
                        }
                        _ => {}
                    },
                    _ => {} // ROM / unmapped
                }
            }
            BusMaster::Cpu(1) => {} // sound ROM is read-only
            _ => {}
        }
    }

    fn io_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        match master {
            // Main CPU I/O port 0x00 = Z80 DMA.
            BusMaster::Cpu(0) if addr & 0xFF == 0x00 => self.board.dma.read(),
            BusMaster::Cpu(0) => 0xFF,

            // Sound CPU (I8039) I/O.
            BusMaster::Cpu(1) => match addr {
                // MOVX external read = tune_r: command latch (P2 bit7) or banked ROM.
                0x00..=0x100 => {
                    let p2 = self.board.sound_cpu.p2;
                    if p2 & 0x80 != 0 {
                        self.board.sound_latch
                    } else {
                        let rom_addr = (((p2 & 0x0F) as usize) << 8) | (addr as usize & 0xFF);
                        self.board.sound_map.read_backing(rom_addr as u16 & 0x0FFF)
                    }
                }
                // IN A,P1: crab/turtle/fly/coin triggers.
                0x101 => self.board.sound_latch1,
                // IN A,P2: last value written, bit 4 grounded.
                0x102 => self.board.sound_cpu.p2 & 0xEF,
                // T0 / T1: get-coin / ice triggers.
                0x110 => self.board.sound_latch3 & 0x01,
                0x111 => (self.board.sound_latch3 >> 1) & 0x01,
                _ => 0xFF,
            },

            _ => 0xFF,
        }
    }

    fn io_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        match master {
            BusMaster::Cpu(0) if addr & 0xFF == 0x00 => {
                self.board.dma.write(data);
                self.board.service_dma();
            }
            // Sound CPU: MOVX external write = DAC. P1/P2 out tracked internally.
            BusMaster::Cpu(1) if addr <= 0x100 => self.board.dac.write(data),
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

crate::impl_board_delegation!(
    MarioBrosSystem,
    board,
    crate::mario_bros::TIMING,
    orientation
);

impl InputConfigurable for MarioBrosSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        MARIO_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let InputEvent::Button { id, pressed } = event else {
            return;
        };
        match id.0 as u8 {
            INPUT_P1_RIGHT => set_bit_active_high(&mut self.board.in0, 0, pressed),
            INPUT_P1_LEFT => set_bit_active_high(&mut self.board.in0, 1, pressed),
            INPUT_P1_JUMP => set_bit_active_high(&mut self.board.in0, 4, pressed),
            INPUT_P1_START => set_bit_active_high(&mut self.board.in0, 5, pressed),
            INPUT_P2_START => set_bit_active_high(&mut self.board.in0, 6, pressed),
            INPUT_SERVICE => set_bit_active_high(&mut self.board.in0, 7, pressed),

            INPUT_P2_RIGHT => set_bit_active_high(&mut self.board.in1, 0, pressed),
            INPUT_P2_LEFT => set_bit_active_high(&mut self.board.in1, 1, pressed),
            INPUT_P2_JUMP => set_bit_active_high(&mut self.board.in1, 4, pressed),
            INPUT_COIN => set_bit_active_high(&mut self.board.in1, 5, pressed),
            INPUT_COIN2 => set_bit_active_high(&mut self.board.in1, 6, pressed),

            _ => {}
        }
    }
}

impl MachineCore for MarioBrosSystem {
    crate::machine_core_metadata!("mariobros", crate::mario_bros::TIMING);

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
            for _ in 0..crate::mario_bros::TIMING.cycles_per_frame() {
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
        self.board.sound_cpu.ram_mask = 0x7F; // 8049: 128 bytes internal RAM

        // The M58715 sound CPU has an internal-ROM bootstrap (undumped) that
        // runs before the external program. Per MAME's audiocpu trace it is:
        //   ANL P2,#$DF   ; clear P2 bit5 → EA high → execute external ROM
        //   SEL MB0
        //   CALL $101     ; enter the external program at 0x101 (STRT T)
        // Without it, execution starts at external 0x000, whose first bytes
        // decode so that the STRT T at 0x101 is consumed as an operand — the
        // sound engine's duration timer never starts and every effect hangs in
        // its playback loop (silent DAC). Replicate the bootstrap's net effect.
        self.board.sound_cpu.p2 = 0xDF;
        self.board.sound_cpu.a11 = false;
        self.board.sound_cpu.a11_pending = false;
        self.board.sound_cpu.pc = 0x101;
    }
}

impl SaveState for MarioBrosSystem {
    crate::machine_save_state!();
}

impl Nvram for MarioBrosSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.main_map.region_data(MainRegion::Nvram))
    }

    fn load_nvram(&mut self, data: &[u8]) {
        let nv = self.board.main_map.region_data_mut(MainRegion::Nvram);
        let n = data.len().min(nv.len());
        nv[..n].copy_from_slice(&data[..n]);
    }
}

impl phosphor_core::core::machine::Profilable for MarioBrosSystem {}

/// DIP switches for the parent `mario` set (read at 0x7F80, active-high).
const MARIO_DIP_BANKS: &[DipSwitchBank] = &[DipSwitchBank {
    name: "DSW",
    options: &[
        DipOption {
            name: "Lives",
            mask: 0x03,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "3",
                    value: 0x00,
                },
                DipChoice {
                    label: "4",
                    value: 0x01,
                },
                DipChoice {
                    label: "5",
                    value: 0x02,
                },
                DipChoice {
                    label: "6",
                    value: 0x03,
                },
            ],
        },
        DipOption {
            name: "Coinage",
            mask: 0x0C,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "1 Coin/1 Credit",
                    value: 0x00,
                },
                DipChoice {
                    label: "2 Coins/1 Credit",
                    value: 0x04,
                },
                DipChoice {
                    label: "1 Coin/2 Credits",
                    value: 0x08,
                },
                DipChoice {
                    label: "1 Coin/3 Credits",
                    value: 0x0C,
                },
            ],
        },
        DipOption {
            name: "Bonus Life",
            mask: 0x30,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "20000",
                    value: 0x00,
                },
                DipChoice {
                    label: "30000",
                    value: 0x10,
                },
                DipChoice {
                    label: "40000",
                    value: 0x20,
                },
                DipChoice {
                    label: "None",
                    value: 0x30,
                },
            ],
        },
        DipOption {
            name: "Difficulty",
            mask: 0xC0,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "Easy",
                    value: 0x00,
                },
                DipChoice {
                    label: "Medium",
                    value: 0x80,
                },
                DipChoice {
                    label: "Hard",
                    value: 0x40,
                },
                DipChoice {
                    label: "Hardest",
                    value: 0xC0,
                },
            ],
        },
    ],
}];

impl DipSwitches for MarioBrosSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        MARIO_DIP_BANKS
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        if bank == 0 { self.board.dsw } else { 0 }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        if bank == 0 {
            self.board.dsw = value;
        }
    }
}

crate::impl_board_debug_trace!(MarioBrosSystem, board);

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = MarioBrosSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("mariobros", &["mario", "mariobros"], create_machine)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::cpu::CpuStateTrait;

    /// Program the on-board Z80 DMA for a memory-to-memory copy via the I/O bus,
    /// exactly as the game would (writes to port 0x00), then leave it armed.
    fn program_dma(sys: &mut MarioBrosSystem, src: u16, dst: u16, len_minus_1: u16) {
        let io = |sys: &mut MarioBrosSystem, b: u8| sys.io_write(BusMaster::Cpu(0), 0x00, b);
        // WR0: transfer, port A source, follow port-A addr L/H + block-len L/H.
        io(sys, 0b0111_1101);
        io(sys, (src & 0xFF) as u8);
        io(sys, (src >> 8) as u8);
        io(sys, (len_minus_1 & 0xFF) as u8);
        io(sys, (len_minus_1 >> 8) as u8);
        io(sys, 0b0001_0100); // WR1: port A memory, increment
        io(sys, 0b0001_0000); // WR2: port B memory, increment
        io(sys, 0b1010_1101); // WR4: continuous, follow port-B addr L/H
        io(sys, (dst & 0xFF) as u8);
        io(sys, (dst >> 8) as u8);
        io(sys, 0b1000_1010); // WR5: ready active-high
        io(sys, 0xCF); // LOAD
        io(sys, 0x87); // ENABLE
    }

    #[test]
    fn cocktail_dip_flips_orientation_live() {
        use phosphor_core::core::machine::{Orientation, Renderable};
        let mut sys = MarioBrosSystem::new();
        // Upright cabinet: no flip. display_size is native (no axis swap for 180°).
        assert_eq!(sys.orientation(), Orientation::NORMAL);
        assert_eq!(sys.display_size(), (256, 224));
        // Cocktail flip DIP engaged → orientation flips to COCKTAIL (180°) live,
        // queried per frame. COCKTAIL does not swap axes.
        sys.board.flip_screen = true;
        assert_eq!(sys.orientation(), Orientation::COCKTAIL);
        assert_eq!(sys.orientation(), Orientation::ROT180);
        assert!(!sys.orientation().swaps_axes());
        // Toggling back restores upright.
        sys.board.flip_screen = false;
        assert_eq!(sys.orientation(), Orientation::NORMAL);
    }

    #[test]
    fn render_frame_is_native_unflipped() {
        use phosphor_core::core::machine::Renderable;
        let mut sys = MarioBrosSystem::new();
        // Tag a pixel in the visible region of the scanline buffer; render_frame
        // must emit it at the native position even when the cocktail DIP is set
        // (the flip is applied centrally, not baked here).
        let start = VBLANK_END * NATIVE_WIDTH * 3;
        sys.board.scanline_buffer[start] = 0x42;
        sys.board.scanline_buffer[start + 1] = 0x43;
        sys.board.scanline_buffer[start + 2] = 0x44;
        sys.board.flip_screen = true;
        let mut buf = vec![0u8; 256 * 224 * 3];
        sys.render_frame(&mut buf);
        assert_eq!(&buf[0..3], &[0x42, 0x43, 0x44]);
    }

    #[test]
    fn boots_and_runs_frames_without_panicking() {
        let mut sys = MarioBrosSystem::new();
        // Size the GFX caches (load_rom_set normally does this); ROMs stay zeroed.
        sys.board.decode_gfx_roms();
        sys.reset();
        assert_eq!(sys.board.sound_cpu.ram_mask, 0x7F, "I8039 has 128B RAM");
        for _ in 0..3 {
            sys.run_frame();
        }
        // Render the visible frame (256×224 RGB24); must match display size.
        let (w, h) = TIMING.display_size();
        assert_eq!((w, h), (256, 224));
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.board.render_frame(&mut buf);
    }

    #[test]
    fn sound_cpu_bootstraps_to_external_0x101() {
        // The M58715 internal-ROM bootstrap must leave the sound CPU about to
        // execute external 0x101 (STRT T) with P2 bit5 cleared and MB0 selected;
        // otherwise the sound engine's timer never starts and the DAC is silent.
        let mut sys = MarioBrosSystem::new();
        sys.reset();
        assert_eq!(sys.board.sound_cpu.pc, 0x101);
        assert_eq!(sys.board.sound_cpu.p2, 0xDF);
        assert!(!sys.board.sound_cpu.a11);
        assert!(!sys.board.sound_cpu.a11_pending);
    }

    #[test]
    fn bus_decodes_ram_nvram_video_sprite() {
        let mut sys = MarioBrosSystem::new();
        // Work RAM, NVRAM, video and sprite RAM are CPU read/write.
        for (addr, val) in [
            (0x6000u16, 0x11u8),
            (0x6800, 0x22),
            (0x7400, 0x33),
            (0x7000, 0x44),
        ] {
            sys.write(BusMaster::Cpu(0), addr, val);
            assert_eq!(sys.read(BusMaster::Cpu(0), addr), val, "addr {addr:#06x}");
        }
        // Program ROM is read-only: writes are ignored.
        sys.board.main_map.region_data_mut(MainRegion::Rom)[0] = 0xAB;
        sys.write(BusMaster::Cpu(0), 0x0000, 0x00);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0x0000), 0xAB);
    }

    #[test]
    fn bus_decodes_inputs_and_control_latches() {
        let mut sys = MarioBrosSystem::new();
        sys.board.in0 = 0x55;
        sys.board.in1 = 0xAA;
        sys.board.dsw = 0x3C;
        assert_eq!(sys.read(BusMaster::Cpu(0), 0x7C00), 0x55);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0x7C80), 0xAA);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0x7F80), 0x3C);

        // Scroll + sound command latches.
        sys.write(BusMaster::Cpu(0), 0x7D00, 0x42);
        assert_eq!(sys.board.scroll_y, 0x42);
        sys.write(BusMaster::Cpu(0), 0x7E00, 0x99);
        assert_eq!(sys.board.sound_latch, 0x99);

        // LS259: gfx bank (Q0), flip (Q2), palette bank (Q3), NMI mask (Q4).
        sys.write(BusMaster::Cpu(0), 0x7E80, 0x01);
        sys.write(BusMaster::Cpu(0), 0x7E82, 0x01);
        sys.write(BusMaster::Cpu(0), 0x7E83, 0x01);
        sys.write(BusMaster::Cpu(0), 0x7E84, 0x01);
        assert_eq!(sys.board.gfx_bank, 1);
        assert!(sys.board.flip_screen);
        assert_eq!(sys.board.palette_bank, 1);
        assert!(sys.board.nmi_mask);
    }

    #[test]
    fn sample_triggers_route_to_sound_latches() {
        let mut sys = MarioBrosSystem::new();
        // 0x7F00: death → sound CPU IRQ.
        sys.write(BusMaster::Cpu(0), 0x7F00, 0x01);
        assert!(sys.board.sound_irq_pending);
        sys.write(BusMaster::Cpu(0), 0x7F00, 0x00);
        assert!(!sys.board.sound_irq_pending);

        // 0x7F01/0x7F02 → soundlatch3 bits 0/1 (T0/T1).
        sys.write(BusMaster::Cpu(0), 0x7F01, 0x01);
        sys.write(BusMaster::Cpu(0), 0x7F02, 0x01);
        assert_eq!(sys.board.sound_latch3 & 0x03, 0x03);

        // 0x7F03..0x7F06 → soundlatch1 bits 0..3 (P1 input).
        sys.write(BusMaster::Cpu(0), 0x7F05, 0x01); // crab/.. bit 2
        assert_eq!(sys.board.sound_latch1, 0x04);
    }

    #[test]
    fn dma_copies_work_ram_to_sprite_ram() {
        let mut sys = MarioBrosSystem::new();
        // Seed the source block in work RAM.
        let ram = sys.board.main_map.region_data_mut(MainRegion::Ram);
        ram[0..5].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF, 0x42]);

        program_dma(&mut sys, 0x6000, 0x7000, 0x0004); // 5 bytes
        // Trigger via LS259 Q5 (DMA SET → RDY).
        sys.write(BusMaster::Cpu(0), 0x7E85, 0x01);

        let spr = sys.board.main_map.region_data(MainRegion::SpriteRam);
        assert_eq!(&spr[0..5], &[0xDE, 0xAD, 0xBE, 0xEF, 0x42]);
    }

    #[test]
    fn input_maps_to_correct_port_bits() {
        let mut sys = MarioBrosSystem::new();
        let press = |sys: &mut MarioBrosSystem, id: u8| {
            sys.handle_input(InputEvent::Button {
                id: InputId(id as u16),
                pressed: true,
            });
        };
        press(&mut sys, INPUT_P1_RIGHT); // in0 bit 0
        press(&mut sys, INPUT_P1_JUMP); // in0 bit 4
        press(&mut sys, INPUT_P1_START); // in0 bit 5
        press(&mut sys, INPUT_SERVICE); // in0 bit 7
        assert_eq!(sys.board.in0, 0b1011_0001);

        press(&mut sys, INPUT_P2_LEFT); // in1 bit 1
        press(&mut sys, INPUT_COIN); // in1 bit 5
        press(&mut sys, INPUT_COIN2); // in1 bit 6
        assert_eq!(sys.board.in1, 0b0110_0010);
    }

    #[test]
    fn gfx_decode_has_expected_dimensions() {
        let mut sys = MarioBrosSystem::new();
        // Set the first plane-0 bit of tile 0 and sprite 0 (MSB of byte 0).
        sys.board.tile_rom[0] = 0x80;
        sys.board.sprite_rom[0] = 0x80;
        sys.board.decode_gfx_roms();

        assert_eq!(sys.board.tile_cache.count(), 512);
        assert_eq!(
            (sys.board.tile_cache.width(), sys.board.tile_cache.height()),
            (8, 8)
        );
        assert_eq!(sys.board.sprite_cache.count(), 256);
        assert_eq!(
            (
                sys.board.sprite_cache.width(),
                sys.board.sprite_cache.height()
            ),
            (16, 16)
        );
        // Plane 0 maps to pixel bit 0.
        assert_eq!(sys.board.tile_cache.pixel(0, 0, 0) & 1, 1);
        assert_eq!(sys.board.sprite_cache.pixel(0, 0, 0) & 1, 1);
    }

    #[test]
    fn nvram_save_and_load() {
        let mut sys = MarioBrosSystem::new();
        sys.board.main_map.region_data_mut(MainRegion::Nvram)[0x10] = 0x7E;
        let saved = sys.save_nvram().unwrap().to_vec();
        assert_eq!(saved.len(), 0x800);
        assert_eq!(saved[0x10], 0x7E);

        let mut sys2 = MarioBrosSystem::new();
        sys2.load_nvram(&saved);
        assert_eq!(
            sys2.board.main_map.region_data(MainRegion::Nvram)[0x10],
            0x7E
        );
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = MarioBrosSystem::new();
        sys.board.main_map.region_data_mut(MainRegion::Ram)[0x100] = 0xAA;
        sys.board.main_map.region_data_mut(MainRegion::Nvram)[0x40] = 0xBB;
        sys.board.main_map.region_data_mut(MainRegion::SpriteRam)[0x50] = 0xCC;
        sys.board.main_map.region_data_mut(MainRegion::VideoRam)[0x60] = 0xDD;
        sys.board.in0 = 0x1F;
        sys.board.in1 = 0x0E;
        sys.board.sound_latch = 0x42;
        sys.board.sound_latch1 = 0x07;
        sys.board.sound_latch3 = 0x02;
        sys.board.flip_screen = true;
        sys.board.gfx_bank = 1;
        sys.board.palette_bank = 1;
        sys.board.nmi_mask = true;
        sys.board.scroll_y = 0x21;
        sys.board.sound_irq_pending = true;
        sys.board.clock = 123_456;
        sys.board.vblank_nmi_pending = true;

        let data = SaveState::save_state(&sys).expect("save_state");
        let cpu_snap = sys.board.cpu.snapshot();
        let snd_snap = sys.board.sound_cpu.snapshot();

        let mut sys2 = MarioBrosSystem::new();
        sys2.board.clock = 999;
        SaveState::load_state(&mut sys2, &data).unwrap();

        assert_eq!(sys2.board.cpu.snapshot(), cpu_snap);
        assert_eq!(sys2.board.sound_cpu.snapshot(), snd_snap);
        assert_eq!(
            sys2.board.main_map.region_data(MainRegion::Ram)[0x100],
            0xAA
        );
        assert_eq!(
            sys2.board.main_map.region_data(MainRegion::Nvram)[0x40],
            0xBB
        );
        assert_eq!(
            sys2.board.main_map.region_data(MainRegion::SpriteRam)[0x50],
            0xCC
        );
        assert_eq!(
            sys2.board.main_map.region_data(MainRegion::VideoRam)[0x60],
            0xDD
        );
        assert_eq!(sys2.board.in0, 0x1F);
        assert_eq!(sys2.board.sound_latch, 0x42);
        assert_eq!(sys2.board.sound_latch1, 0x07);
        assert_eq!(sys2.board.sound_latch3, 0x02);
        assert!(sys2.board.flip_screen);
        assert_eq!(sys2.board.gfx_bank, 1);
        assert_eq!(sys2.board.palette_bank, 1);
        assert!(sys2.board.nmi_mask);
        assert_eq!(sys2.board.scroll_y, 0x21);
        assert!(sys2.board.sound_irq_pending);
        assert_eq!(sys2.board.clock, 123_456);
        assert!(sys2.board.vblank_nmi_pending);
    }

    #[test]
    fn save_does_not_include_rom() {
        let mut sys = MarioBrosSystem::new();
        sys.board.main_map.region_data_mut(MainRegion::Rom)[0] = 0xDE;
        sys.board.sound_map.region_data_mut(SoundRegion::Rom)[0] = 0xAD;
        sys.board.tile_rom[0] = 0xBE;
        sys.board.sprite_rom[0] = 0xEF;

        let data = SaveState::save_state(&sys).unwrap();
        let mut sys2 = MarioBrosSystem::new();
        SaveState::load_state(&mut sys2, &data).unwrap();

        assert_eq!(sys2.board.main_map.region_data(MainRegion::Rom)[0], 0x00);
        assert_eq!(sys2.board.sound_map.region_data(SoundRegion::Rom)[0], 0x00);
        assert_eq!(sys2.board.tile_rom[0], 0x00);
        assert_eq!(sys2.board.sprite_rom[0], 0x00);
    }

    #[test]
    fn dip_default_and_metadata() {
        let sys = MarioBrosSystem::new();
        assert_eq!(sys.dip_bank_value(0), 0x00); // 3 lives, 1C/1C, 20k, easy
        crate::assert_dip_banks_valid(sys.dip_banks(), &[sys.dip_bank_value(0)]);
    }

    #[test]
    fn set_dip_option_masks_only_its_bits() {
        let mut sys = MarioBrosSystem::new();
        sys.set_dip_option(0, 0, 0x03); // Lives → "6"
        assert_eq!(sys.dip_bank_value(0), 0x03);
        sys.set_dip_option(0, 3, 0xC0); // Difficulty → "Hardest"
        assert_eq!(sys.dip_bank_value(0), 0xC3);
    }

    #[test]
    fn gfx_regions_registered_with_expected_geometry() {
        let regions = crate::gfx_registry::regions_for("mariobros");
        assert_eq!(
            regions.iter().map(|r| r.region).collect::<Vec<_>>(),
            vec!["sprites", "tiles"],
        );

        let tiles = crate::gfx_registry::find("mariobros", "tiles").unwrap();
        assert_eq!((tiles.count, tiles.width, tiles.height), (512, 8, 8));
        assert!(tiles.palette.is_some(), "tiles carry the PROM palette");

        let sprites = crate::gfx_registry::find("mariobros", "sprites").unwrap();
        assert_eq!(
            (sprites.count, sprites.width, sprites.height),
            (256, 16, 16)
        );
        assert!(sprites.palette.is_some());
    }

    /// The gfxview `GfxRegion` layouts must decode byte-for-byte identically to
    /// the runtime `decode_gfx_roms()` — they share the same `'static` tables,
    /// and this pins that invariant against future drift of the region geometry.
    #[test]
    fn gfx_region_layout_matches_runtime_decode() {
        // Deterministic pseudo-ROM bytes (no real ROM / CRC needed here).
        let mut sys = MarioBrosSystem::new();
        for (i, b) in sys.board.tile_rom.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(31).wrapping_add(7);
        }
        for (i, b) in sys.board.sprite_rom.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(17).wrapping_add(3);
        }
        sys.board.decode_gfx_roms();

        let tiles = crate::gfx_registry::find("mariobros", "tiles").unwrap();
        let via_region = decode_gfx(&sys.board.tile_rom, 0, tiles.count as usize, tiles.layout);
        assert_caches_eq(&via_region, &sys.board.tile_cache);

        let sprites = crate::gfx_registry::find("mariobros", "sprites").unwrap();
        let via_region = decode_gfx(
            &sys.board.sprite_rom,
            0,
            sprites.count as usize,
            sprites.layout,
        );
        assert_caches_eq(&via_region, &sys.board.sprite_cache);
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
