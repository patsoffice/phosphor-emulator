//! Universal "Mr. Do!" (1982) — Universal 8201 single-Z80 board.
//!
//! Hardware (per MAME `src/mame/universal/mrdo.cpp`, the Taito `mrdot` set):
//! - CPU: Z80 @ 8.2 MHz / 2 ≈ 4.1 MHz, one VBLANK IRQ per frame (`irq0_line_hold`)
//! - Sound: 2× SN76489 @ 4.1 MHz, byte-mapped at 0x9801 / 0x9802
//! - Video: 240×192 raster, ROT270; two 32×32 8×8 tilemaps (BG scrollable, FG
//!   fixed) + 16×16 sprites
//! - Palette: two 32-byte palette PROMs (diode-coupled resistor DAC) + a 32-byte
//!   sprite-color lookup PROM
//! - Protection: a PAL16R6 snooped on FG-VRAM writes, read back at 0x9803
//!
//! This file is the **scaffold** (issue `.1`): board/system split, memory map,
//! the CPU run loop with the VBLANK IRQ, input/DIP plumbing, and registry
//! entries. It boots and runs frames without panicking. Still stubbed, each in
//! its own follow-up issue:
//! - ROM regions beyond the main program (`.2`)
//! - GFX decode (`.3`) + palette DAC (`.4`)
//! - BG/FG tilemap render (`.5`) + sprites (`.6`)
//! - 2× SN76489 sound (`.7`)
//! - PAL16R6 protection (`.8`)
//! - full DIP tables (`.9`) and tests (`.10`)

use phosphor_core::audio::{AudioResampler, DcBlocker};
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::DefaultBinding;
use phosphor_core::core::machine::{
    ActionRole, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, Direction, InputConfigurable,
    InputControl, InputEvent, InputId, InputKind, MachineCore, SaveState,
};

use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{
    Bus, BusMaster, ClockDomainName as Clk, ClockTree, DomainId, TimingConfig,
};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::z80::Z80;
use phosphor_core::device::sn76489::Sn76489a;
use phosphor_core::gfx;
use phosphor_core::gfx::decode::{GfxLayout, decode_gfx};
use phosphor_macros::{BusDebug, MemoryRegion, Saveable};

use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::gfx_registry::GfxRegion;
use crate::input_defaults as ind;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_low;

// ---------------------------------------------------------------------------
// Memory map region IDs
// ---------------------------------------------------------------------------

/// Main CPU (Z80) address-space regions. I/O ports (0x9800-0x9803, 0xA000-
/// 0xA003, scroll at 0xF000/0xF800) are decoded directly in the `Bus` impl.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum MainRegion {
    Rom = 1,        // 0x0000-0x7FFF  32 KB program ROM
    BgVideoRam = 2, // 0x8000-0x87FF  BG tilemap RAM (codes + attrs)
    FgVideoRam = 3, // 0x8800-0x8FFF  FG tilemap RAM (codes + attrs)
    SpriteRam = 4,  // 0x9000-0x90FF  sprite RAM (write-only on HW)
    WorkRam = 5,    // 0xE000-0xEFFF  work RAM
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------
// XTAL 8.2 MHz; CPU = /2 = 4.1 MHz. Pixel clock 19.6 MHz/4 = 4.9 MHz, HTOTAL
// 312, VTOTAL 262 → VSYNC ≈ 59.94 Hz. We approximate the frame as 262 scanlines
// of 261 CPU cycles (68382 cycles/frame ⇒ ≈ 59.96 Hz); the exact per-scanline
// count is not a clean integer, and MAME itself derives the IRQ from VSYNC.

pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 8_200_000 / 2, // 4_100_000
    cycles_per_scanline: 261,
    total_scanlines: 262,
    // Native (pre-orientation) framebuffer: Mr. Do! declares ROT270 and the
    // frontend rotates centrally, so these are the unrotated dimensions.
    display_width: VISIBLE_WIDTH as u32,   // 240
    display_height: VISIBLE_HEIGHT as u32, // 192
    display_aspect: Some((3, 4)),          // portrait tube as viewed (after ROT270)
};

/// The board's crystals and everything divided out of them.
///
/// Two: an 8.2 MHz crystal for the Z80 at /2 and a 19.6 MHz one for the pixel
/// clock at /4. As on Do! Castle, that makes `TIMING.cycles_per_scanline` a
/// conversion rather than a hardware constant: 312 dot clocks is 261.061 CPU
/// cycles, and running 261 makes the video clock 235 ppm fast. The comment
/// above `TIMING` already says the count "is not a clean integer"; the
/// tolerance below is that admission as a number a test can hold.
pub fn clock_tree() -> phosphor_core::core::ClockTree {
    use phosphor_core::core::RootId;
    let mut t = ClockTree::new(8_200_000);
    let vid = t.add_root(19_600_000);
    let cpu = t.add_domain(Clk::Cpu, RootId::MAIN, 1, 2); // 4.1 MHz
    let dot = t.add_domain(Clk::Pixel, vid, 1, 4); // 4.9 MHz
    t.add_domain(Clk::Psg, RootId::MAIN, 1, 32); // SN76489 tones at 4.1 MHz / 16
    t.add_domain(Clk::Psg2, RootId::MAIN, 1, 32); // second SN76489, same clock
    t.set_step_domain(cpu);
    t.set_raster(dot, 312, 240);
    t
}

/// Native (pre-rotation) visible raster: visarea x 8..248, y 32..224.
pub const VISIBLE_WIDTH: usize = 240;
pub const VISIBLE_HEIGHT: usize = 192;

/// First visible column of the 256-pixel native line. Absolute x = `nx + 8`.
const VISIBLE_LEFT_COL: usize = 8;

/// First visible scanline. Native row `n` is drawn during scanline
/// `n + VISIBLE_TOP_LINE`, so absolute y = `ny + 32`.
const VISIBLE_TOP_LINE: u64 = 32;

/// Scanline at which the once-per-frame VBLANK IRQ is asserted: the first line
/// past the visible window, which is where vertical blanking begins.
const VBLANK_IRQ_LINE: u64 = VISIBLE_TOP_LINE + VISIBLE_HEIGHT as u64;

/// Both SN76489 PSGs and the CPU share the 8.2 MHz/2 clock; the tone/noise
/// generators advance at chip_clock / 16.
const SOUND_CLOCK: u32 = TIMING.cpu_clock_hz as u32;
fn output_sample_rate() -> u64 {
    phosphor_core::audio::host_sample_rate() as u64
}

// ---------------------------------------------------------------------------
// ROM definitions ("mrdot" — Taito set, full PAL protection equations)
// ---------------------------------------------------------------------------

/// Main Z80 program ROM at 0x0000-0x7FFF (four 8 KB chips).
pub static MRDO_MAIN_ROM: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        RomEntry {
            name: "d1",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x3dcd9359],
        },
        RomEntry {
            name: "d2",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x710058d8],
        },
        RomEntry {
            name: "d3",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x467d12d8],
        },
        RomEntry {
            name: "d4",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0xfce9afeb],
        },
    ],
};

/// FG (text) tiles, gfx1 — Taito-specific chars (two 4 KB planes). 8×8 2bpp.
pub static MRDO_GFX_FG_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "d9",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xde4cfe66],
        },
        RomEntry {
            name: "d10",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0xa6c2f38b],
        },
    ],
};

/// BG tiles, gfx2 (two 4 KB planes). 8×8 2bpp.
pub static MRDO_GFX_BG_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "r8-08.bin",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xdbdc9ffa],
        },
        RomEntry {
            name: "n8-07.bin",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x4b9973db],
        },
    ],
};

/// Sprites, gfx3 (two 4 KB chips, planes nibble-interleaved). 16×16 2bpp.
pub static MRDO_GFX_SPR_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "h5-05.bin",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xe1218cc5],
        },
        RomEntry {
            name: "k5-06.bin",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0xb1f68b04],
        },
    ],
};

/// Color PROMs: U2 palette hi-bits (0x00), T2 palette lo-bits (0x20), sprite
/// color LUT (0x40), timing PROM (0x60, unused by emulation).
pub static MRDO_PALETTE_PROM: RomRegion = RomRegion {
    size: 0x80,
    entries: &[
        RomEntry {
            name: "u02--2.bin",
            size: 0x20,
            offset: 0x0000,
            crc32: &[0x238a65d7],
        },
        RomEntry {
            name: "t02--3.bin",
            size: 0x20,
            offset: 0x0020,
            crc32: &[0xae263dc0],
        },
        RomEntry {
            name: "f10--1.bin",
            size: 0x20,
            offset: 0x0040,
            crc32: &[0x16ee4ca2],
        },
        RomEntry {
            name: "j10--4.bin",
            size: 0x20,
            offset: 0x0060,
            crc32: &[0xff7fe284],
        },
    ],
};

// ---------------------------------------------------------------------------
// GFX bit-plane layouts (per `charlayout`/`spritelayout` in MAME `mrdo.cpp`).
// `plane_offsets` are LSB-first — MAME's `planeoffset` array reversed.
// ---------------------------------------------------------------------------

/// 8×8 2bpp tiles, used for both FG (gfx1) and BG (gfx2). The two planes are the
/// two 0x1000 halves of the region; MAME `{ RGN_FRAC(0,2), RGN_FRAC(1,2) }`
/// reversed → `[second-half, first-half]`. 512 tiles per region.
pub static MRDO_CHAR_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0x1000 * 8, 0],
    x_offsets: &[7, 6, 5, 4, 3, 2, 1, 0],
    y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
    char_increment: 8 * 8,
};

/// 16×16 2bpp sprites (gfx3). Planes nibble-interleaved in the same bytes
/// (MAME `{ 4, 0 }` → `[0, 4]`); each row is four 8-bit columns read high-nibble
/// first. 128 sprites, 64 bytes each.
pub static MRDO_SPRITE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 4],
    x_offsets: &[
        3, 2, 1, 0, // column group 0 (byte 0, bits 3..0)
        11, 10, 9, 8, // column group 1 (byte 1)
        19, 18, 17, 16, // column group 2 (byte 2)
        27, 26, 25, 24, // column group 3 (byte 3)
    ],
    y_offsets: &[
        0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448, 480,
    ],
    char_increment: 64 * 8,
};

const FG_TILE_COUNT: usize = 512;
const BG_TILE_COUNT: usize = 512;
const SPRITE_COUNT: usize = 128;
/// Palette pens: 256 char pens (1:1 with indirect colors) + 64 sprite pens.
const PALETTE_LEN: usize = 0x140;

/// Build Mr. Do!'s RGB palette from the two 32-byte palette PROMs and the
/// 32-byte sprite-color LUT (per `palette_init` in MAME `mrdo_v.cpp`).
///
/// Each of R/G/B is a 4-bit value formed from 2 bits of the T2 PROM (150 Ω /
/// 120 Ω resistors) and 2 bits of the U2 PROM (100 Ω / 75 Ω), fed through a
/// diode-coupled pot ladder with a 220 Ω pulldown and a 0.7 V diode drop. The
/// 256 "indirect" colors map 1:1 to the char pens; the 64 sprite pens resolve
/// through the LUT. Shared by [`MrdoBoard::build_palette`] and the gfxview hook.
fn mrdo_palette_rgb(prom: &[u8]) -> [(u8, u8, u8); PALETTE_LEN] {
    // Resistor-pot weight table: index bits 0-1 = T2 (150/120 Ω), bits 2-3 = U2
    // (100/75 Ω). weight[i] = 255 * pot[i] / pot[0x0f], clamped at 0.
    const R: [f32; 4] = [150.0, 120.0, 100.0, 75.0];
    const PULL: f32 = 220.0;
    const POTADJUST: f32 = 0.7; // diode voltage drop
    let mut pot = [0.0f32; 16];
    for (i, p) in pot.iter_mut().enumerate() {
        let mut par = 0.0f32;
        for (bit, &r) in R.iter().enumerate() {
            if i & (1 << bit) != 0 {
                par += 1.0 / r;
            }
        }
        *p = if par != 0.0 {
            PULL / (PULL + 1.0 / par) - POTADJUST
        } else {
            0.0
        };
    }
    let mut weight = [0i32; 16];
    for (i, w) in weight.iter_mut().enumerate() {
        *w = (255.0 * pot[i] / pot[0x0f]) as i32;
        if *w < 0 {
            *w = 0;
        }
    }
    let gun = |a1: usize, a2: usize, shift: u8| -> u8 {
        let bits0 = (prom[a1] >> shift) & 0x03;
        let bits2 = (prom[a2] >> shift) & 0x03;
        weight[(bits0 + (bits2 << 2)) as usize] as u8
    };

    // 256 indirect colors. a1 addresses the T2 PROM (+0x20), a2 the U2 PROM.
    let mut indirect = [(0u8, 0u8, 0u8); 256];
    for (i, c) in indirect.iter_mut().enumerate() {
        let a1 = ((i >> 3) & 0x1c) + (i & 0x03) + 0x20;
        let a2 = (i & 0x1c) + (i & 0x03);
        *c = (gun(a1, a2, 0), gun(a1, a2, 2), gun(a1, a2, 4));
    }

    let mut out = [(0u8, 0u8, 0u8); PALETTE_LEN];
    // Char pens 0x000-0x0FF map 1:1 to indirect colors.
    out[..256].copy_from_slice(&indirect);
    // Sprite pens 0x100-0x13F resolve through the LUT at PROM offset 0x40.
    for i in 0..0x40 {
        let mut ctab = prom[0x40 + (i & 0x1f)];
        if i & 0x20 != 0 {
            ctab >>= 4; // high nibble = sprite color n + 8
        } else {
            ctab &= 0x0f; // low nibble = sprite color n
        }
        let idx = ctab as usize + ((ctab as usize & 0x0c) << 3);
        out[0x100 + i] = indirect[idx & 0xff];
    }
    out
}

/// Evaluate the Mr. Do! protection PAL16R6 (IC U001) for a written data byte.
///
/// The Taito `mrdot` set uses the real jedutil-extracted equations (per
/// `mrdot_state::protection_w` in MAME `mrdo.cpp`). The 8 data bits drive the
/// PAL inputs (i2 = bit7 … i9 = bit0; i7/bit2 is unused), four product terms
/// `t1..t4` feed six registered outputs, and the chip presents the inverted
/// result — read back by the game at 0x9803. The registered outputs latch on
/// the write "clock", so a read after a write reflects the last byte written.
fn mrdo_protection_output(data: u8) -> u8 {
    let bit = |n: u8| (data >> n) & 1;
    let not = |x: u8| x ^ 1;
    let (i9, i8) = (bit(0), bit(1));
    let (i6, i5, i4, i3, i2) = (bit(3), bit(4), bit(5), bit(6), bit(7));

    let t1 = i2 & not(i3) & i4 & not(i5) & not(i6) & not(i8) & i9;
    let t2 = not(i2) & not(i3) & i4 & i5 & not(i6) & i8 & not(i9);
    let t3 = i2 & i3 & not(i4) & not(i5) & i6 & not(i8) & i9;
    let t4 = not(i2) & i3 & i4 & not(i5) & i6 & i8 & i9;

    // r12 and r19 are always 0.
    let r13 = t1 << 1;
    let r14 = (t1 | t2) << 2;
    let r15 = (t1 | t3) << 3;
    let r16 = t1 << 4;
    let r17 = (t1 | t3) << 5;
    let r18 = (t3 | t4) << 6;
    !(r18 | r17 | r16 | r15 | r14 | r13)
}

// ---------------------------------------------------------------------------
// MrdoBoard
// ---------------------------------------------------------------------------

/// One CPU cycle (≈4.1 MHz): the scanline boundary, the VBLANK IRQ edge, the
/// Z80, then the PSGs.
///
/// The CPU lives on the machine and the board *is* the bus, so this takes them
/// as separate borrows and dispatches at a concrete type.
///
/// This is the debugger's path: it tests the frame position on every cycle so
/// that single-stepping still crosses scanline boundaries. A whole frame goes
/// through [`run_scanlines`], which hoists that test out.
#[inline]
pub fn tick(cpu: &mut Z80, board: &mut MrdoBoard) {
    let frame_cycle = board.clock % TIMING.cycles_per_frame();
    if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
        board.begin_scanline(frame_cycle / TIMING.cycles_per_scanline);
    }
    step_cycle(cpu, board);
}

/// The part of a cycle with no scanline-boundary test in it.
#[inline]
fn step_cycle(cpu: &mut Z80, board: &mut MrdoBoard) {
    board.begin_cycle(cpu);

    // `irq0_line_hold` acknowledgement: the CPU takes the interrupt at an
    // instruction boundary, observed here as IFF1 going 1->0 while pending.
    let iff1_before = cpu.iff1;
    cpu.execute_cycle(board, BusMaster::Cpu(0));
    if board.vblank_irq_pending && iff1_before && !cpu.iff1 {
        board.vblank_irq_pending = false;
    }

    board.end_cycle();
}

/// Run `cycles` CPU cycles, scanline-outer and cycle-inner. The caller must
/// start on a scanline boundary and pass a multiple of `cycles_per_scanline`;
/// the debugger's off-boundary stepping goes through [`tick`] instead.
pub fn run_scanlines(cpu: &mut Z80, board: &mut MrdoBoard, cycles: u64) {
    debug_assert!(
        board.clock.is_multiple_of(TIMING.cycles_per_scanline)
            && cycles.is_multiple_of(TIMING.cycles_per_scanline),
        "run_scanlines must start on a scanline boundary and run whole scanlines"
    );
    for _ in 0..cycles / TIMING.cycles_per_scanline {
        let scanline = board.clock % TIMING.cycles_per_frame() / TIMING.cycles_per_scanline;
        board.begin_scanline(scanline);
        for _ in 0..TIMING.cycles_per_scanline {
            step_cycle(cpu, board);
        }
    }
}

/// Run one frame's worth of CPU cycles. Whole scanlines go through
/// [`run_scanlines`]; a partial scanline at either end (only after the debugger
/// has left the clock off-boundary) goes through [`tick`].
pub fn run_frame(cpu: &mut Z80, board: &mut MrdoBoard) {
    let scanline = TIMING.cycles_per_scanline;
    let mut remaining = TIMING.cycles_per_frame();

    let lead = ((scanline - board.clock % scanline) % scanline).min(remaining);
    for _ in 0..lead {
        tick(cpu, board);
    }
    remaining -= lead;

    let whole = remaining - remaining % scanline;
    run_scanlines(cpu, board, whole);
    remaining -= whole;

    for _ in 0..remaining {
        tick(cpu, board);
    }
}

#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct MrdoBoard {
    /// The address space persists its own writable regions: the two video RAMs,
    /// sprite RAM and work RAM here.
    #[debug_map(cpu = 0)]
    #[save(id = 1)]
    pub(crate) main_map: AddressSpace16,

    // GFX ROMs + their decoded pixel caches.
    #[save_skip]
    pub(crate) fg_rom: [u8; 0x2000],
    #[save_skip]
    pub(crate) bg_rom: [u8; 0x2000],
    #[save_skip]
    pub(crate) spr_rom: [u8; 0x2000],
    #[save_skip]
    pub(crate) fg_cache: gfx::GfxCache, // 512 × 8×8 2bpp FG tiles
    #[save_skip]
    pub(crate) bg_cache: gfx::GfxCache, // 512 × 8×8 2bpp BG tiles
    #[save_skip]
    pub(crate) sprite_cache: gfx::GfxCache, // 128 × 16×16 2bpp sprites

    /// Color PROMs + the decoded RGB palette (256 char pens + 64 sprite pens).
    ///
    /// Derived from the PROM rather than from anything the CPU writes, so it is
    /// rebuilt at ROM load and is not state.
    #[save_skip]
    pub(crate) palette_prom: [u8; 0x80],
    #[save_skip]
    pub(crate) palette_rgb: [(u8, u8, u8); PALETTE_LEN],

    // Video registers: BG scroll (only the BG layer scrolls) + flipscreen.
    #[save(id = 2)]
    pub(crate) bg_scroll_x: u8,
    #[save(id = 3)]
    pub(crate) bg_scroll_y: u8,
    #[save(id = 4)]
    pub(crate) flipscreen: bool,

    // Protection PAL16R6 (IC U001) latched output, read back at 0x9803.
    #[save(id = 5)]
    pub(crate) pal_u001: u8,

    // Inputs (active-low, latched by `handle_input`) + DIP banks.
    #[save(id = 6)]
    pub(crate) in0: u8,
    #[save(id = 7)]
    pub(crate) in1: u8,
    #[save(id = 8)]
    pub(crate) dsw1: u8,
    #[save(id = 9)]
    pub(crate) dsw2: u8,

    // Sound: 2× SN76489 (write-only, at 0x9801/0x9802), box-filtered to the
    // output rate. `snN_clock` gates each chip's generators (chip_clock/16).
    #[debug_device("SN76489 #1")]
    #[save(id = 10)]
    pub(crate) sn1: Sn76489a,
    #[debug_device("SN76489 #2")]
    #[save(id = 11)]
    pub(crate) sn2: Sn76489a,
    /// The board's clock tree, as [`clock_tree`] declares it.
    #[debug_device("Clocks")]
    #[save(id = 12)]
    pub(crate) clocks: ClockTree,
    #[save_skip]
    pub(crate) sn1_dom: DomainId,
    #[save_skip]
    pub(crate) sn2_dom: DomainId,
    /// Output coupling capacitor.
    ///
    /// An SN76489's output swings from ground upward rather than either side of
    /// it, so the summed pair carries a pedestal that the board's analog stage
    /// blocks on the way to the amplifier. Without it the mean sat at +0.183 of
    /// full scale, which eats the headroom every real voice needs. The chips
    /// themselves are not modelled wrongly: a unipolar swing is what the silicon
    /// does, and the same omission accounted for twelve machines across five
    /// different sound parts before this one.
    #[save(id = 13)]
    pub(crate) sn_coupling: DcBlocker,
    #[save(id = 14)]
    pub(crate) audio: AudioResampler<i16>,

    // Timing / interrupts.
    #[save(id = 15)]
    pub(crate) clock: u64,
    #[save(id = 16)]
    pub(crate) vblank_irq_pending: bool,

    /// Display framebuffer (native 240×192 RGB), filled one row at a time as
    /// the beam reaches each visible scanline.
    ///
    /// A row holds what the beam drew on that line, out of the two video RAMs,
    /// sprite RAM, the BG scroll registers and the flip latch as they stood at
    /// that line's boundary. The picture a completed frame presents therefore
    /// does *not* contain that frame's own vblank writes: the beam had already
    /// passed. The whole-frame render this replaced did contain them.
    ///
    /// Derived output, so not saved, and not seeded after a load either: the
    /// rows of the next frame overwrite every one of them, and there is no
    /// moment a load could reconstruct a picture from.
    #[save_skip]
    pub(crate) framebuffer: Vec<u8>,
}

impl Default for MrdoBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl MrdoBoard {
    pub fn new() -> Self {
        let clocks = clock_tree();
        let sn1_dom = clocks.find(Clk::Psg).expect("declared PSG domain");
        let sn2_dom = clocks.find(Clk::Psg2).expect("declared second PSG domain");
        Self {
            main_map: Self::build_main_map(),
            fg_rom: [0; 0x2000],
            bg_rom: [0; 0x2000],
            spr_rom: [0; 0x2000],
            fg_cache: gfx::GfxCache::new(0, 8, 8),
            bg_cache: gfx::GfxCache::new(0, 8, 8),
            sprite_cache: gfx::GfxCache::new(0, 16, 16),
            palette_prom: [0; 0x80],
            palette_rgb: [(0, 0, 0); PALETTE_LEN],
            bg_scroll_x: 0,
            bg_scroll_y: 0,
            flipscreen: false,
            pal_u001: 0xFF, // PAL registered outputs power up high
            in0: 0xFF,
            in1: 0xFF,
            dsw1: DSW1_DEFAULT,
            dsw2: DSW2_DEFAULT,
            sn1: Sn76489a::new(SOUND_CLOCK),
            sn2: Sn76489a::new(SOUND_CLOCK),
            clocks,
            sn1_dom,
            sn2_dom,
            sn_coupling: DcBlocker::new(output_sample_rate() as u32),
            audio: AudioResampler::new(TIMING.cpu_clock_hz, output_sample_rate()),
            clock: 0,
            vblank_irq_pending: false,
            framebuffer: vec![0u8; VISIBLE_WIDTH * VISIBLE_HEIGHT * 3],
        }
    }

    fn build_main_map() -> AddressSpace16 {
        use MainRegion::*;
        let mut map = AddressSpace16::new();
        map.region(Rom, "Program ROM", 0x0000, 0x8000, AccessKind::ReadOnly)
            .region(
                BgVideoRam,
                "BG Video RAM",
                0x8000,
                0x0800,
                AccessKind::ReadWrite,
            )
            .region(
                FgVideoRam,
                "FG Video RAM",
                0x8800,
                0x0800,
                AccessKind::ReadWrite,
            )
            .region(
                SpriteRam,
                "Sprite RAM",
                0x9000,
                0x0100,
                AccessKind::ReadWrite,
            )
            .region(WorkRam, "Work RAM", 0xE000, 0x1000, AccessKind::ReadWrite);
        map
    }

    // -----------------------------------------------------------------------
    // GFX decode + palette (call after loading ROMs)
    // -----------------------------------------------------------------------

    /// Decode the FG/BG tile and sprite ROMs into pixel caches.
    pub fn decode_gfx_roms(&mut self) {
        self.fg_cache = decode_gfx(&self.fg_rom, 0, FG_TILE_COUNT, &MRDO_CHAR_LAYOUT);
        self.bg_cache = decode_gfx(&self.bg_rom, 0, BG_TILE_COUNT, &MRDO_CHAR_LAYOUT);
        self.sprite_cache = decode_gfx(&self.spr_rom, 0, SPRITE_COUNT, &MRDO_SPRITE_LAYOUT);
    }

    /// Build the RGB palette from the U2/T2 palette PROMs and sprite-color LUT.
    pub fn build_palette(&mut self) {
        self.palette_rgb = mrdo_palette_rgb(&self.palette_prom);
    }

    // -----------------------------------------------------------------------
    // Core tick
    // -----------------------------------------------------------------------

    /// Board work that leads a CPU cycle: the VBLANK IRQ edge (asserted at the
    /// top of VBLANK and held until the CPU acknowledges it) and the debugger's
    /// access-attribution latch.
    fn begin_cycle(&mut self, cpu: &Z80) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();
        if frame_cycle == VBLANK_IRQ_LINE * TIMING.cycles_per_scanline {
            self.vblank_irq_pending = true;
        }

        if self.main_map.debug_active() {
            let pc = cpu.at_instruction_boundary().then_some(cpu.pc as u32);
            self.main_map.latch_access_context(self.clock, pc);
        }
    }

    /// Board work after the CPU's cycle: the PSGs and the clock.
    fn end_cycle(&mut self) {
        // Advance both PSG generators (chip_clock/16) and box-filter the summed,
        // clamped output to the 44.1 kHz audio rate — one input sample per cycle.
        if self.clocks.tick(self.sn1_dom) {
            self.sn1.tick();
        }
        if self.clocks.tick(self.sn2_dom) {
            self.sn2.tick();
        }
        let mix = (i32::from(self.sn1.output()) + i32::from(self.sn2.output()))
            .clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        // Coupled after the box filter, once per output sample: the capacitor
        // sits between the chips and the amplifier, so what it strips the offset
        // from is the summed signal on its way to the speaker.
        if let Some(avg) = self.audio.tick_sample(mix) {
            let coupled = self.sn_coupling.process(avg as f32);
            self.audio
                .push_sample(coupled.clamp(i16::MIN as f32, i16::MAX as f32) as i16);
        }

        self.clock += 1;
    }

    /// Drain the resampled PSG mix into the output buffer.
    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.audio.fill_audio(buffer)
    }

    /// Copy the latest framebuffer into the frontend's `buffer` (240×192 RGB24
    /// native; the cabinet's ROT270 is applied centrally by the frontend).
    ///
    /// This does not draw. Each visible row was composited at its own scanline
    /// boundary in [`render_scanline`](Self::render_scanline) as the beam
    /// reached it.
    pub fn render_frame(&self, buffer: &mut [u8]) {
        buffer.copy_from_slice(&self.framebuffer);
    }

    /// Work that only happens on the first cycle of a scanline: compositing the
    /// row the beam is about to draw.
    ///
    /// `scanline` is 0..262 and the visible window is `[32, 224)`; only visible
    /// lines have a row to draw, so walking `0..total_scanlines` draws exactly
    /// the visible rows and nothing else.
    ///
    /// Called once per scanline from [`run_scanlines`] and, for the debugger's
    /// single-step path, from [`tick`] when the clock lands on a boundary.
    /// Public because the picture only exists once the beam has passed over it:
    /// a caller that wants a frame without running CPU cycles (the integration
    /// tests) has to step the beam itself.
    pub fn begin_scanline(&mut self, scanline: u64) {
        let top = VISIBLE_TOP_LINE;
        if (top..top + VISIBLE_HEIGHT as u64).contains(&scanline) {
            self.render_scanline((scanline - top) as usize);
        }
    }

    /// Draw one visible scanline into the framebuffer, out of the video state as
    /// it stands at that line's boundary.
    ///
    /// `ny` is a native row in `[0, VISIBLE_HEIGHT)`, drawn during scanline
    /// `ny + VISIBLE_TOP_LINE`. The layers run in the board's order for this one
    /// row — backdrop pen 0, the scrollable BG tilemap, the fixed FG tilemap,
    /// then sprites — and every register they read is read here, so a mid-frame
    /// write to the BG scroll registers or the flip latch splits the picture at
    /// the row it lands on.
    ///
    /// The palette is resolved here too, so it is the palette as of this row.
    /// On this board that is a formality: the palette comes from PROMs and the
    /// CPU cannot change it.
    ///
    /// **Sprite sampling point.** W3 established that a sprite circuit of this
    /// shape scans its object list into a line buffer displayed on the *next*
    /// line, so the list for row `r` is the one that stood at row `r - 1`. This
    /// board's `256 - rawY` carries no such lead, unlike `galaga`'s
    /// `256 - y + 1`, and none is added here: the object sheet shows two pairs
    /// of 6148s on the output path, which is a line buffer's shape, but which
    /// pair buffers and how the two alternate was read as chips rather than as a
    /// traced path (`docs/schematics/sprite-list-scan.md`). Adding a one-line
    /// lead would move every sprite pixel on the strength of a guess, so the
    /// constant is left as it was and the question stays open.
    fn render_scanline(&mut self, ny: usize) {
        // GFX ROMs not decoded yet (a bare board, before `load_rom_set`): leave
        // the row as it was rather than indexing an empty cache. `GfxCache::
        // pixel` indexes a `Vec`, so a zero-entry cache is a panic and not a
        // zero, and this went unnoticed while only a whole-frame render — which
        // had this same guard — could reach it. `decode_gfx_roms` fills all
        // three caches together, so `fg_cache` answers for the other two.
        if self.fg_cache.count() == 0 {
            return;
        }

        // One native row of palette indices, backdrop (pen 0) first.
        let mut row = [0u16; VISIBLE_WIDTH];

        self.draw_tilemaps_row(&mut row, ny);
        self.draw_sprites_row(&mut row, ny);

        let out = ny * VISIBLE_WIDTH * 3;
        for (x, &p) in row.iter().enumerate() {
            let (r, g, b) = self.palette_rgb[p as usize];
            let di = out + x * 3;
            self.framebuffer[di] = r;
            self.framebuffer[di + 1] = g;
            self.framebuffer[di + 2] = b;
        }
    }

    /// Composite the BG and FG tilemaps into one native row.
    ///
    /// Both are 32×32 grids of 8×8 tiles read straight out of video RAM, so a
    /// row's grid row is fixed by `ny` and only the column varies; the per-pixel
    /// tile lookup below is what the whole-frame renderer did too, and a row
    /// pass does not multiply it.
    ///
    /// `set_flip_all` mirrors both tilemaps 180° in cocktail mode, so the
    /// mirrored coordinate is what is sampled. Sprites are NOT flipped by the
    /// hardware (the game writes cocktail-adjusted sprite data itself).
    fn draw_tilemaps_row(&self, row: &mut [u16; VISIBLE_WIDTH], ny: usize) {
        let bgram = self.main_map.region_data(MainRegion::BgVideoRam);
        let fgram = self.main_map.region_data(MainRegion::FgVideoRam);
        let flip = self.flipscreen;
        let sx = self.bg_scroll_x as usize;
        let sy = self.bg_scroll_y as usize;

        let ay = ny + VISIBLE_TOP_LINE as usize;
        let ey = if flip { 255 - ay } else { ay };

        // Fixed for the whole row: both maps' grid row, and which line inside
        // the tile that row lands on. Only the column varies along the row, so
        // these come out of the per-pixel loop.
        let by = (ey + sy) & 0xff;
        let bg_row_base = (by / 8) * 32;
        let bg_py = by & 7;
        let fg_row_base = (ey / 8) * 32;
        let fg_py = ey & 7;

        // A tile's identity changes only every 8 pixels, so the map fetch and
        // the decode of its 8-pixel line are cached until the column index
        // moves. That is 30 to 31 fetches per layer per row rather than 240.
        // Cached on the *column index* rather than on a span, which is what
        // keeps this correct in both directions: with flipscreen the source
        // walks backwards (ex = 255 - ax), and the scrolled BG wraps at 256.
        let mut bg_col = usize::MAX;
        let mut bg_attr = 0u8;
        let mut bg_line: &[u8] = &[];
        let mut fg_col = usize::MAX;
        let mut fg_attr = 0u8;
        let mut fg_line: &[u8] = &[];

        for (nx, p) in row.iter_mut().enumerate() {
            let ax = nx + VISIBLE_LEFT_COL;
            let ex = if flip { 255 - ax } else { ax };

            // BG tilemap (gfx2), scrolled; source = (screen + scroll) & 0xff.
            // A pixel is opaque when its value is non-zero, OR when the tile
            // carries the force-layer-0 attribute (attr bit 6), which means "no
            // transparency", so such a tile's value-0 pixels draw pen `color*4`
            // too. This is how the scrolling rainbow band shows all its colors
            // while ordinary value-0 pixels (dug maze corridors) stay
            // transparent.
            {
                let bx = (ex + sx) & 0xff;
                let col = bx / 8;
                if col != bg_col {
                    bg_col = col;
                    let bidx = bg_row_base + col;
                    bg_attr = bgram[bidx];
                    let bcode = bgram[bidx + 0x400] as usize + ((bg_attr as usize & 0x80) << 1);
                    bg_line = self.bg_cache.row_slice(bcode, bg_py);
                }
                let bval = bg_line[bx & 7];
                if bval != 0 || bg_attr & 0x40 != 0 {
                    *p = (bg_attr as u16 & 0x3f) * 4 + bval as u16;
                }
            }

            // FG tilemap (gfx1), fixed; same force-layer-0 rule. The
            // priority-flagged tiles around the logo mask off the BG rainbow.
            {
                let col = ex / 8;
                if col != fg_col {
                    fg_col = col;
                    let fidx = fg_row_base + col;
                    fg_attr = fgram[fidx];
                    let fcode = fgram[fidx + 0x400] as usize + ((fg_attr as usize & 0x80) << 1);
                    fg_line = self.fg_cache.row_slice(fcode, fg_py);
                }
                let fval = fg_line[ex & 7];
                if fval != 0 || fg_attr & 0x40 != 0 {
                    *p = (fg_attr as u16 & 0x3f) * 4 + fval as u16;
                }
            }
        }
    }

    /// Composite the sprites that cross native row `ny` into `row`.
    ///
    /// 64 entries of 4 bytes, walked high→low so a lower-numbered slot draws
    /// over a higher one, and skipped when the raw Y byte is 0. Screen Y =
    /// 256 - rawY; screen X is direct. Sprite pens live at palette base 0x100
    /// (color 0-15, 2bpp).
    ///
    /// The vertical range test is taken as soon as the slot's Y byte is read,
    /// so a slot that is not on this line costs one byte read rather than a
    /// decode: 64 slots × 192 rows is the whole per-frame cost of the pass.
    fn draw_sprites_row(&self, row: &mut [u16; VISIBLE_WIDTH], ny: usize) {
        let sprram = self.main_map.region_data(MainRegion::SpriteRam);
        let nyi = ny as i32;

        for offs in (0..0x100).step_by(4).rev() {
            let raw_y = sprram[offs + 1];
            if raw_y == 0 {
                continue;
            }
            // Top of the 16-pixel sprite in native rows.
            let top = 256 - raw_y as i32 - VISIBLE_TOP_LINE as i32;
            let py = nyi - top;
            if !(0..16).contains(&py) {
                continue;
            }

            let code = sprram[offs] as usize & 0x7f;
            let attr = sprram[offs + 2];
            let color = (attr & 0x0f) as u16;
            let flipx = attr & 0x10 != 0;
            let flipy = attr & 0x20 != 0;
            let sxp = sprram[offs + 3] as i32;

            let ry = if flipy { 15 - py } else { py } as usize;
            for px in 0..16i32 {
                let nxi = sxp + px - VISIBLE_LEFT_COL as i32;
                if !(0..VISIBLE_WIDTH as i32).contains(&nxi) {
                    continue;
                }
                let rx = if flipx { 15 - px } else { px };
                let val = self.sprite_cache.pixel(code, rx as usize, ry);
                if val == 0 {
                    continue;
                }
                row[nxi as usize] = 0x100 + color * 4 + val as u16;
            }
        }
    }

    /// Mr. Do!'s monitor is mounted rotated 270° clockwise. The orientation is
    /// declarative — the frontend rotates `render_frame`'s native output.
    pub fn orientation(&self) -> phosphor_core::core::machine::Orientation {
        phosphor_core::core::machine::Orientation::ROT270
    }

    // -----------------------------------------------------------------------
    // Reset / interrupts
    // -----------------------------------------------------------------------

    pub fn reset(&mut self) {
        self.vblank_irq_pending = false;
        self.clock = 0;
        self.bg_scroll_x = 0;
        self.bg_scroll_y = 0;
        self.flipscreen = false;
        self.pal_u001 = 0xFF;
        self.sn1.reset();
        self.sn2.reset();
        self.clocks.reset();
        self.sn_coupling.reset();
        self.audio.reset();
        self.main_map
            .region_data_mut(MainRegion::BgVideoRam)
            .fill(0);
        self.main_map
            .region_data_mut(MainRegion::FgVideoRam)
            .fill(0);
        self.main_map.region_data_mut(MainRegion::SpriteRam).fill(0);
        self.main_map.region_data_mut(MainRegion::WorkRam).fill(0);
        // The framebuffer is not cleared: the next frame's rows overwrite every
        // one of them as the beam reaches them.
    }

    /// The interrupt lines this board drives. Named to avoid shadowing
    /// [`Bus::check_interrupts`], which the board also implements.
    pub fn interrupt_state(&self, target: BusMaster) -> InterruptState {
        match target {
            BusMaster::Cpu(0) => InterruptState {
                irq: self.vblank_irq_pending,
                ..Default::default()
            },
            _ => InterruptState::default(),
        }
    }

    /// Whether the CPU is at an instruction boundary. It lives on the machine,
    /// which passes it back in.
    pub fn instruction_boundaries(cpu: &Z80) -> u32 {
        u32::from(cpu.at_instruction_boundary())
    }
}

// ---------------------------------------------------------------------------
// MrdoSystem wrapper
// ---------------------------------------------------------------------------

/// Universal Mr. Do! (1982).
///
/// The Z80 sits beside the board rather than inside it, so each cycle
/// dispatches at the concrete [`MrdoBoard`] -- which *is* the bus.
#[derive(Saveable, BusDebug)]
pub struct MrdoSystem {
    #[debug_cpu("Z80")]
    pub(crate) cpu: Z80,
    #[debug_bus]
    pub board: MrdoBoard,
}

impl Default for MrdoSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl MrdoSystem {
    pub fn new() -> Self {
        Self {
            cpu: Z80::new(),
            board: MrdoBoard::new(),
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let prog = MRDO_MAIN_ROM.load(rom_set)?;
        self.board
            .main_map
            .load_region_at(MainRegion::Rom, 0, &prog);

        self.board
            .fg_rom
            .copy_from_slice(&MRDO_GFX_FG_ROM.load(rom_set)?);
        self.board
            .bg_rom
            .copy_from_slice(&MRDO_GFX_BG_ROM.load(rom_set)?);
        self.board
            .spr_rom
            .copy_from_slice(&MRDO_GFX_SPR_ROM.load(rom_set)?);
        self.board
            .palette_prom
            .copy_from_slice(&MRDO_PALETTE_PROM.load(rom_set)?);

        self.board.decode_gfx_roms();
        self.board.build_palette();
        Ok(())
    }

    /// Advance one CPU cycle, returning the instruction-boundary mask.
    pub fn step_cycle(&mut self) -> u32 {
        tick(&mut self.cpu, &mut self.board);
        MrdoBoard::instruction_boundaries(&self.cpu)
    }

    /// Read the CPU-facing bus, side effects and all. Distinct from the
    /// debugger's `BusDebug::peek`/`poke`, which avoid side effects.
    pub fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        self.board.read(master, addr)
    }

    /// Write the CPU-facing bus, side effects and all. See [`Self::bus_read`].
    pub fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.board.write(master, addr, data);
    }
}

// The board is the bus: Mr. Do! is the only machine on it.
impl Bus for MrdoBoard {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let BusMaster::Cpu(0) = master else {
            return 0xFF;
        };
        let data = match addr {
            0x0000..=0x90FF => self.main_map.read_backing(addr),
            0x9803 => self.pal_u001, // protection PAL16R6 readback

            0xA000 => self.in0,
            0xA001 => self.in1,
            0xA002 => self.dsw1,
            0xA003 => self.dsw2,
            0xE000..=0xEFFF => self.main_map.read_backing(addr),
            _ => 0xFF,
        };
        self.main_map.watch_read(0, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        let BusMaster::Cpu(0) = master else {
            return;
        };
        self.main_map.watch_write(0, master, addr, data);
        match addr {
            // BG video RAM + sprite RAM: plain backing writes.
            0x8000..=0x87FF | 0x9000..=0x90FF => self.main_map.write_backing(addr, data),
            // FG video RAM: the PAL16R6 snoops the data bus on every FG write.
            0x8800..=0x8FFF => {
                self.main_map.write_backing(addr, data);
                self.pal_u001 = mrdo_protection_output(data);
            }
            // Flipscreen (bit 0); bits 1-3 are playfield priority, unused by Mr. Do!.
            0x9800 => self.flipscreen = data & 0x01 != 0,
            0x9801 => self.sn1.write(data),
            0x9802 => self.sn2.write(data),
            0xE000..=0xEFFF => self.main_map.write_backing(addr, data),
            0xF000..=0xF7FF => self.bg_scroll_x = data, // scrollx_w
            // scrolly_w: NOT affected by flipscreen — compensate when flipped.
            0xF800..=0xFFFF => {
                self.bg_scroll_y = if self.flipscreen {
                    ((256 - data as u16) & 0xff) as u8
                } else {
                    data
                };
            }
            _ => {} // ROM / unmapped
        }
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        self.interrupt_state(target)
    }
}

crate::impl_board_delegation!(MrdoSystem, board, crate::mrdo::TIMING, orientation);

impl MachineCore for MrdoSystem {
    crate::machine_core_metadata!("mrdo", crate::mrdo::TIMING, crate::mrdo::clock_tree);

    fn gfx_sheets(&self) -> Vec<phosphor_core::core::machine::GfxSheet<'_>> {
        use phosphor_core::core::machine::GfxSheet;
        vec![
            GfxSheet {
                name: "fg",
                cache: &self.board.fg_cache,
                palette: &self.board.palette_rgb,
            },
            GfxSheet {
                name: "bg",
                cache: &self.board.bg_cache,
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
        run_frame(&mut self.cpu, &mut self.board);
    }

    fn reset(&mut self) {
        self.board.reset();
        self.cpu.reset(&mut self.board, BusMaster::Cpu(0));
    }
}

impl SaveState for MrdoSystem {
    crate::machine_save_state!();
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------
// IN0 (0xA000): b0 L, b1 D, b2 R, b3 U, b4 Button1, b5 Start1, b6 Start2, b7 Tilt.
// IN1 (0xA001): b0 L, b1 D, b2 R, b3 U, b4 Button1, b6 Coin1, b7 Coin2.
// All active-low; a pressed control clears its bit.

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

const MRDO_CONTROLS: &[InputControl] = &[
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
    button(INPUT_P1_BUTTON, "p1_button", "P1 Dig", 1),
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
    button(INPUT_P2_BUTTON, "p2_button", "P2 Dig", 2),
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
];

impl InputConfigurable for MrdoSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        MRDO_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let InputEvent::Button { id, pressed } = event else {
            return;
        };
        let b = &mut self.board;
        match id.0 {
            INPUT_P1_LEFT => set_bit_active_low(&mut b.in0, 0, pressed),
            INPUT_P1_DOWN => set_bit_active_low(&mut b.in0, 1, pressed),
            INPUT_P1_RIGHT => set_bit_active_low(&mut b.in0, 2, pressed),
            INPUT_P1_UP => set_bit_active_low(&mut b.in0, 3, pressed),
            INPUT_P1_BUTTON => set_bit_active_low(&mut b.in0, 4, pressed),
            INPUT_P1_START => set_bit_active_low(&mut b.in0, 5, pressed),
            INPUT_P2_START => set_bit_active_low(&mut b.in0, 6, pressed),
            INPUT_P2_LEFT => set_bit_active_low(&mut b.in1, 0, pressed),
            INPUT_P2_DOWN => set_bit_active_low(&mut b.in1, 1, pressed),
            INPUT_P2_RIGHT => set_bit_active_low(&mut b.in1, 2, pressed),
            INPUT_P2_UP => set_bit_active_low(&mut b.in1, 3, pressed),
            INPUT_P2_BUTTON => set_bit_active_low(&mut b.in1, 4, pressed),
            INPUT_COIN1 => set_bit_active_low(&mut b.in1, 6, pressed),
            INPUT_COIN2 => set_bit_active_low(&mut b.in1, 7, pressed),
            _ => {}
        }
    }
}

// DIP defaults (per MAME `INPUT_PORTS(mrdo)`): DSW1 = Easy, Rack Test off,
// Special/Extra Easy, Upright, 3 lives; DSW2 = 1 Coin/1 Credit both slots.
const DSW1_DEFAULT: u8 = 0xDF;
const DSW2_DEFAULT: u8 = 0xFF;

/// Coinage choices, shared by Coin B (low nibble, `shift = 0`) and Coin A
/// (high nibble, `shift = 4`). Values per MAME `DSW2`.
const fn coinage(shift: u8) -> [DipChoice; 11] {
    [
        DipChoice {
            label: "4 Coins/1 Credit",
            value: 0x06 << shift,
        },
        DipChoice {
            label: "3 Coins/1 Credit",
            value: 0x08 << shift,
        },
        DipChoice {
            label: "2 Coins/1 Credit",
            value: 0x0a << shift,
        },
        DipChoice {
            label: "3 Coins/2 Credits",
            value: 0x07 << shift,
        },
        DipChoice {
            label: "1 Coin/1 Credit",
            value: 0x0f << shift,
        },
        DipChoice {
            label: "2 Coins/3 Credits",
            value: 0x09 << shift,
        },
        DipChoice {
            label: "1 Coin/2 Credits",
            value: 0x0e << shift,
        },
        DipChoice {
            label: "1 Coin/3 Credits",
            value: 0x0d << shift,
        },
        DipChoice {
            label: "1 Coin/4 Credits",
            value: 0x0c << shift,
        },
        DipChoice {
            label: "1 Coin/5 Credits",
            value: 0x0b << shift,
        },
        DipChoice {
            label: "Free Play",
            value: 0x00 << shift,
        },
    ]
}

const COIN_B_CHOICES: [DipChoice; 11] = coinage(0);
const COIN_A_CHOICES: [DipChoice; 11] = coinage(4);

const fn choice(label: &'static str, value: u8) -> DipChoice {
    DipChoice { label, value }
}

const MRDO_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSW1",
        options: &[
            DipOption {
                name: "Difficulty",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    choice("Easy", 0x03),
                    choice("Medium", 0x02),
                    choice("Hard", 0x01),
                    choice("Hardest", 0x00),
                ],
            },
            DipOption {
                name: "Rack Test (Cheat)",
                mask: 0x04,
                apply: DipApplyTiming::Immediate,
                choices: &[choice("Off", 0x04), choice("On", 0x00)],
            },
            DipOption {
                name: "Special",
                mask: 0x08,
                apply: DipApplyTiming::Immediate,
                choices: &[choice("Easy", 0x08), choice("Hard", 0x00)],
            },
            DipOption {
                name: "Extra",
                mask: 0x10,
                apply: DipApplyTiming::Immediate,
                choices: &[choice("Easy", 0x10), choice("Hard", 0x00)],
            },
            DipOption {
                name: "Cabinet",
                mask: 0x20,
                apply: DipApplyTiming::Immediate,
                choices: &[choice("Upright", 0x00), choice("Cocktail", 0x20)],
            },
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
    DipSwitchBank {
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
    },
];

crate::impl_dip_switches!(MrdoSystem, MRDO_DIP_BANKS, board.dsw1, board.dsw2);

impl phosphor_core::core::machine::Nvram for MrdoSystem {}
impl phosphor_core::core::machine::Profilable for MrdoSystem {}
// DebugTrace routes to the board's main-CPU map (write-event ring) rather than
// the default no-op, so `disasm trace --events` sees this board's bus writes.
crate::impl_map_debug_trace!(MrdoSystem, board.main_map);

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

crate::register_machine!(MrdoSystem, "mrdo", &["mrdot", "mrdofix"], MRDO_CONTROLS);

inventory::submit! {
    DisasmRegion {
        machine: "mrdo",
        region: "main",
        cpu: DisasmCpu::Z80,
        org: 0x0000,
        size: MRDO_MAIN_ROM.size as u32,
        load: |rs| MRDO_MAIN_ROM.load(rs),
    }
}

// ---------------------------------------------------------------------------
// GFX viewer regions
// ---------------------------------------------------------------------------

/// gfxview palette hook: build the RGB palette with the same PROM math as the
/// runtime path so the offline viewer and scanline renderer never diverge.
fn mrdo_gfx_palette(rom_set: &RomSet) -> Result<Vec<(u8, u8, u8)>, RomLoadError> {
    let prom = MRDO_PALETTE_PROM.load(rom_set)?;
    Ok(mrdo_palette_rgb(&prom).to_vec())
}

inventory::submit! {
    GfxRegion {
        machine: "mrdo",
        region: "fg",
        count: FG_TILE_COUNT as u32,
        width: 8,
        height: 8,
        layout: &MRDO_CHAR_LAYOUT,
        load: |rs| MRDO_GFX_FG_ROM.load(rs),
        palette: Some(mrdo_gfx_palette),
    }
}
inventory::submit! {
    GfxRegion {
        machine: "mrdo",
        region: "bg",
        count: BG_TILE_COUNT as u32,
        width: 8,
        height: 8,
        layout: &MRDO_CHAR_LAYOUT,
        load: |rs| MRDO_GFX_BG_ROM.load(rs),
        palette: Some(mrdo_gfx_palette),
    }
}
inventory::submit! {
    GfxRegion {
        machine: "mrdo",
        region: "sprites",
        count: SPRITE_COUNT as u32,
        width: 16,
        height: 16,
        layout: &MRDO_SPRITE_LAYOUT,
        load: |rs| MRDO_GFX_SPR_ROM.load(rs),
        palette: Some(mrdo_gfx_palette),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::DipSwitches;

    #[test]
    fn machine_is_registered() {
        assert!(crate::registry::find("mrdo").is_some());
    }

    #[test]
    fn disasm_region_registered() {
        assert!(crate::disasm_registry::find("mrdo", "main").is_some());
    }

    #[test]
    fn timing_is_sane() {
        assert_eq!(TIMING.cpu_clock_hz, 4_100_000);
        let hz = TIMING.frame_rate_hz();
        assert!((59.0..61.0).contains(&hz), "frame rate {hz} out of range");
        // Native (unrotated) 240×192; the frontend applies ROT270 to present
        // the 192×240 portrait image.
        assert_eq!(TIMING.display_size(), (240, 192));
    }

    #[test]
    fn input_controls_have_stable_names() {
        let sys = MrdoSystem::new();
        let controls = sys.input_controls();
        assert!(controls.iter().any(|c| c.stable_name == "p1_right"));
        assert!(controls.iter().any(|c| c.stable_name == "coin1"));
    }

    #[test]
    fn gfx_regions_registered_with_expected_geometry() {
        let regions = crate::gfx_registry::regions_for("mrdo");
        assert_eq!(regions.len(), 3);
        let spr = regions.iter().find(|r| r.region == "sprites").unwrap();
        assert_eq!((spr.count, spr.width, spr.height), (128, 16, 16));
        let fg = regions.iter().find(|r| r.region == "fg").unwrap();
        assert_eq!((fg.count, fg.width, fg.height), (512, 8, 8));
    }

    #[test]
    fn gfx_decode_produces_populated_caches() {
        let mut board = MrdoBoard::new();
        // Plane 1 (MSB) of tile 0 all-set, plane 0 (LSB) clear ⇒ pixels read as 2.
        board.fg_rom[0] = 0xFF; // first half = MSB plane, row 0
        board.decode_gfx_roms();
        assert_eq!(board.fg_cache.count(), 512);
        assert_eq!(board.sprite_cache.count(), 128);
        assert_eq!(board.fg_cache.pixel(0, 0, 0), 2);
        assert_eq!(board.fg_cache.pixel(0, 7, 0), 2);
    }

    #[test]
    fn palette_has_full_pen_range_and_black_zero() {
        // An all-zero PROM ⇒ every gun weight 0 ⇒ black everywhere.
        let pal = mrdo_palette_rgb(&[0u8; 0x80]);
        assert_eq!(pal.len(), 0x140);
        assert_eq!(pal[0], (0, 0, 0));
        assert_eq!(pal[0x13F], (0, 0, 0));
        // A saturated T2+U2 red nibble should light red without green/blue.
        let mut prom = [0u8; 0x80];
        // index 0: a1 = 0x20 (T2), a2 = 0x00 (U2); set both low 2 bits (red).
        prom[0x20] = 0x03;
        prom[0x00] = 0x03;
        let pal = mrdo_palette_rgb(&prom);
        assert_eq!(pal[0], (0xFF, 0, 0));
    }

    /// Native visible pixel (nx, ny) lands at this offset in the native
    /// (unrotated) output; ROT270 is applied centrally by the frontend.
    fn out_offset(nx: usize, ny: usize) -> usize {
        (ny * VISIBLE_WIDTH + nx) * 3
    }

    /// Walk the beam over a whole frame's scanlines so every visible row is
    /// drawn. The picture only exists once the beam has passed over it, so a
    /// test that pokes video state has to scan before it can look.
    fn scan_frame(board: &mut MrdoBoard) {
        for s in 0..TIMING.total_scanlines {
            board.begin_scanline(s);
        }
    }

    #[test]
    fn render_frame_draws_fg_tile_native() {
        let mut sys = MrdoSystem::new();
        // Decode a solid FG tile 0 with pixel value 2 (MSB plane set, LSB clear):
        // first 0x1000 half is the MSB plane for our layout.
        sys.board.fg_rom[0..8].fill(0xFF); // tile 0, all 8 rows, MSB plane
        sys.board.decode_gfx_roms();
        // Give color code 1 → pen 1*4 + 2 = 6 a recognizable red.
        sys.board.palette_rgb[6] = (200, 0, 0);
        // Place tile 0, color 1, at FG tile index for the top-left visible cell.
        // Visible origin is abs (8,32) → tile col 1, row 4 → index 4*32 + 1.
        let idx = 4 * 32 + 1;
        {
            let fg = sys.board.main_map.region_data_mut(MainRegion::FgVideoRam);
            fg[idx] = 0x01; // attr: color 1, no high code bit
            fg[idx + 0x400] = 0x00; // code 0
        }
        let mut buf = vec![0u8; VISIBLE_WIDTH * VISIBLE_HEIGHT * 3];
        scan_frame(&mut sys.board);
        sys.board.render_frame(&mut buf);
        // Abs (8,32) is native (0,0); that pixel should be the tile's red pen.
        let di = out_offset(0, 0);
        assert_eq!((buf[di], buf[di + 1], buf[di + 2]), (200, 0, 0));
    }

    #[test]
    fn render_frame_draws_sprite() {
        let mut sys = MrdoSystem::new();
        sys.board.spr_rom[0..64].fill(0xFF); // sprite 0 fully set → every pixel value 3
        sys.board.decode_gfx_roms();
        // Sprite pen 0x100 + color 0*4 + 3 = 0x103.
        sys.board.palette_rgb[0x103] = (0, 180, 0);
        {
            let spr = sys.board.main_map.region_data_mut(MainRegion::SpriteRam);
            spr[0] = 0x00; // code 0
            spr[1] = 200; // rawY → screenY = 56
            spr[2] = 0x00; // color 0, no flip
            spr[3] = 40; // screenX = 40
        }
        let mut buf = vec![0u8; VISIBLE_WIDTH * VISIBLE_HEIGHT * 3];
        scan_frame(&mut sys.board);
        sys.board.render_frame(&mut buf);
        // At least one sprite pixel should be green somewhere in the output.
        let any_green = buf.as_chunks::<3>().0.contains(&[0u8, 180, 0]);
        assert!(any_green, "sprite pixel not found in output");
    }

    // -----------------------------------------------------------------------
    // Per-scanline rendering (W4)
    // -----------------------------------------------------------------------

    const BLUE: (u8, u8, u8) = (0, 0, 0xFF);
    const RED: (u8, u8, u8) = (0xFF, 0, 0);

    fn pixel(buffer: &[u8], nx: usize, ny: usize) -> (u8, u8, u8) {
        let di = out_offset(nx, ny);
        (buffer[di], buffer[di + 1], buffer[di + 2])
    }

    fn row_has_blue(buffer: &[u8], ny: usize) -> bool {
        let row = &buffer[ny * VISIBLE_WIDTH * 3..(ny + 1) * VISIBLE_WIDTH * 3];
        row.as_chunks::<3>().0.contains(&[0, 0, 0xFF])
    }

    /// A machine whose FG tilemap covers the screen in one solid color, so
    /// palette entry 6 marks any row the foreground layer reached.
    fn system_with_visible_fg() -> MrdoSystem {
        let mut sys = MrdoSystem::new();
        sys.board.fg_rom[0..8].fill(0xFF); // tile 0: every pixel value 2
        sys.board.decode_gfx_roms();
        sys.board.palette_rgb[6] = BLUE; // color 1, pixel 2
        sys.board.palette_rgb[10] = RED; // color 2, pixel 2
        let fg = sys.board.main_map.region_data_mut(MainRegion::FgVideoRam);
        fg[0..0x400].fill(0x01); // every cell: color 1, code 0
        sys
    }

    /// The tilemap half of W4: video RAM is read as the beam passes it, so
    /// rewriting a cell's attribute partway down the screen changes only the
    /// rows below the write.
    ///
    /// The split is at native row 100 (scanline 132), which is *inside* char
    /// row 16 (native rows 96..103). A whole-frame render draws a char row from
    /// one snapshot and cannot produce this picture at all.
    #[test]
    fn a_mid_frame_vram_write_changes_only_the_rows_below_it() {
        const SPLIT_ROW: usize = 100;
        let top = VISIBLE_TOP_LINE;

        let mut sys = system_with_visible_fg();
        for s in top..top + SPLIT_ROW as u64 {
            sys.board.begin_scanline(s);
        }
        {
            let fg = sys.board.main_map.region_data_mut(MainRegion::FgVideoRam);
            fg[0..0x400].fill(0x02); // color 2 -> pen 10
        }
        for s in top + SPLIT_ROW as u64..top + VISIBLE_HEIGHT as u64 {
            sys.board.begin_scanline(s);
        }

        let buffer = &sys.board.framebuffer;
        assert_eq!(
            pixel(buffer, 100, 0),
            BLUE,
            "row 0 was drawn before the write"
        );
        assert_eq!(
            pixel(buffer, 100, SPLIT_ROW - 1),
            BLUE,
            "the last row above the write keeps the old attribute, mid-char-row"
        );
        assert_eq!(
            pixel(buffer, 100, SPLIT_ROW),
            RED,
            "the first row below the write takes the new attribute"
        );
        assert_eq!(
            pixel(buffer, 100, VISIBLE_HEIGHT - 1),
            RED,
            "the bottom row is below the write"
        );
    }

    /// The raster effect this epic exists for. Mr. Do! writes the BG scroll
    /// registers during active display — traced on this ROM set at scanline 145
    /// of frame 3 — and the rows above such a write must keep the old scroll.
    ///
    /// The BG map is 32x32 and this fixture splits it by *column*, so the
    /// distinguishing attribute has to be written in all 32 map rows: a row-0
    /// assertion would pass with only cell 0 set and a row-100 one would not.
    #[test]
    fn a_mid_frame_scroll_write_splits_the_background() {
        const SPLIT_ROW: usize = 100;
        let top = VISIBLE_TOP_LINE;

        let mut sys = MrdoSystem::new();
        // BG tile 0 is all pixel value 0, so the force-layer-0 attribute (bit 6)
        // is what makes it opaque and the tile's color alone paints the row.
        // The FG is left transparent: attr 0 everywhere, and an all-zero
        // fg_rom.
        sys.board.decode_gfx_roms();
        sys.board.palette_rgb[4] = BLUE; // color 1, pixel 0
        sys.board.palette_rgb[8] = RED; // color 2, pixel 0
        {
            let bg = sys.board.main_map.region_data_mut(MainRegion::BgVideoRam);
            for r in 0..32 {
                for c in 0..32 {
                    bg[r * 32 + c] = if c < 16 { 0x41 } else { 0x42 };
                }
            }
        }

        // Native x 0 is absolute x 8. With no scroll that samples map column 1
        // (color 1); scrolled by 128 it samples column 17 (color 2).
        sys.board.bg_scroll_x = 0;
        for s in top..top + SPLIT_ROW as u64 {
            sys.board.begin_scanline(s);
        }
        sys.board.bg_scroll_x = 128;
        for s in top + SPLIT_ROW as u64..top + VISIBLE_HEIGHT as u64 {
            sys.board.begin_scanline(s);
        }

        let buffer = &sys.board.framebuffer;
        assert_eq!(
            pixel(buffer, 0, 0),
            BLUE,
            "row 0 was drawn before the write"
        );
        assert_eq!(
            pixel(buffer, 0, SPLIT_ROW - 1),
            BLUE,
            "the last row above the write keeps the old scroll, mid-tile-row"
        );
        assert_eq!(
            pixel(buffer, 0, SPLIT_ROW),
            RED,
            "the first row below the write takes the new scroll"
        );
        assert_eq!(
            pixel(buffer, 0, VISIBLE_HEIGHT - 1),
            RED,
            "the bottom row is below the write"
        );
    }

    /// The tests above drive `begin_scanline` by hand, which proves nothing
    /// about whether the frame loop ever calls it. Without this one they would
    /// both pass on a board that never drew anything, so this walks the real
    /// `tick` and `run_scanlines` paths and checks the picture changed.
    #[test]
    fn the_frame_loop_draws_rows_at_scanline_boundaries() {
        let mut sys = system_with_visible_fg();
        sys.board.framebuffer.fill(0);

        // Wind through the top blanking lines to the last one before the
        // visible window. The debugger's per-cycle path is what this exercises.
        for _ in 0..VISIBLE_TOP_LINE * TIMING.cycles_per_scanline {
            tick(&mut sys.cpu, &mut sys.board);
        }
        assert!(
            sys.board.framebuffer.iter().all(|&c| c == 0),
            "tick() drew nothing during the top blanking lines"
        );

        // One more scanline through tick(), which crosses the first visible
        // boundary and must draw row 0 and only row 0.
        for _ in 0..TIMING.cycles_per_scanline {
            tick(&mut sys.cpu, &mut sys.board);
        }
        assert!(
            row_has_blue(&sys.board.framebuffer, 0),
            "tick() draws row 0"
        );
        assert!(
            !row_has_blue(&sys.board.framebuffer, 1),
            "and only row 0: nothing has drawn row 1 yet"
        );

        // And the hoisted loop a whole frame actually runs through draws too.
        run_scanlines(&mut sys.cpu, &mut sys.board, TIMING.cycles_per_scanline);
        assert!(
            row_has_blue(&sys.board.framebuffer, 1),
            "run_scanlines() draws row 1"
        );
    }

    /// A blanking line has no row to draw, and drawing one would run off the
    /// end of a framebuffer sized to the visible window.
    #[test]
    fn blanking_scanlines_have_no_row_to_draw() {
        let mut sys = system_with_visible_fg();
        sys.board.framebuffer.fill(0);
        for s in 0..VISIBLE_TOP_LINE {
            sys.board.begin_scanline(s);
        }
        for s in VISIBLE_TOP_LINE + VISIBLE_HEIGHT as u64..TIMING.total_scanlines {
            sys.board.begin_scanline(s);
        }
        assert!(
            sys.board.framebuffer.iter().all(|&c| c == 0),
            "the blanking lines drew nothing"
        );
    }

    /// Cocktail flip mirrors both tilemaps 180°, which the row pass gets by
    /// sampling `255 - ax` — so with the flip on, the source walks *backwards*
    /// along the row. That reversed walk is the case the per-tile cache in
    /// `draw_tilemaps_row` is most able to get wrong, and nothing covered it.
    ///
    /// The visible window is symmetric under the 255-complement (x 8..248 maps
    /// onto itself, and so does y 32..224), so the flipped picture must be the
    /// exact 180° rotation of the unflipped one — not merely similar. Sprites
    /// are excluded because the hardware does not flip them.
    #[test]
    fn flipscreen_mirrors_the_tilemaps_across_both_axes() {
        let mut sys = MrdoSystem::new();
        sys.board.fg_rom[0..8].fill(0xFF); // tile 0: every pixel value 2
        sys.board.decode_gfx_roms();
        // Distinct color per pen, so a mirror is distinguishable from a copy.
        for (i, c) in sys.board.palette_rgb.iter_mut().enumerate() {
            let i = i as u8;
            *c = (i, i.wrapping_mul(3), i.wrapping_mul(7));
        }
        {
            // A different color code in every cell. Bits 6 and 7 stay clear, so
            // the tile code is 0 throughout and only the color varies.
            let fg = sys.board.main_map.region_data_mut(MainRegion::FgVideoRam);
            for (idx, cell) in fg[0..0x400].iter_mut().enumerate() {
                *cell = (idx % 0x40) as u8;
            }
        }

        scan_frame(&mut sys.board);
        let unflipped = sys.board.framebuffer.clone();
        sys.board.flipscreen = true;
        scan_frame(&mut sys.board);
        let flipped = &sys.board.framebuffer;

        // The picture has to be non-uniform, or the mirror below proves nothing.
        assert!(
            unflipped
                .as_chunks::<3>()
                .0
                .iter()
                .any(|&px| px != unflipped[0..3]),
            "the fixture drew more than one color"
        );

        for ny in 0..VISIBLE_HEIGHT {
            for nx in 0..VISIBLE_WIDTH {
                assert_eq!(
                    pixel(flipped, nx, ny),
                    pixel(&unflipped, VISIBLE_WIDTH - 1 - nx, VISIBLE_HEIGHT - 1 - ny),
                    "flipped ({nx},{ny}) is the unflipped picture rotated 180°"
                );
            }
        }
    }

    #[test]
    fn protection_pal_equations() {
        // No product term active ⇒ all registered outputs 0 ⇒ inverted 0xFF.
        assert_eq!(mrdo_protection_output(0x00), 0xFF);
        // t1 active: i2=1,i3=0,i4=1,i5=0,i6=0,i8=0,i9=1 (bits 7,5,0). i7/bit2
        // is unused, so 0xA1 and 0xA5 give the same output.
        assert_eq!(mrdo_protection_output(0xA1), 0xC1);
        assert_eq!(mrdo_protection_output(0xA5), 0xC1);
    }

    #[test]
    fn protection_latches_on_fg_write_and_reads_at_9803() {
        let mut sys = MrdoSystem::new();
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x9803), 0xFF); // power-up
        // A write into FG video RAM clocks the PAL with the data byte.
        sys.bus_write(BusMaster::Cpu(0), 0x8800, 0xA1);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x9803), 0xC1);
        // A BG write must NOT disturb the protection latch.
        sys.bus_write(BusMaster::Cpu(0), 0x8000, 0x00);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x9803), 0xC1);
    }

    #[test]
    fn scroll_and_flip_registers_latch() {
        let mut sys = MrdoSystem::new();
        sys.bus_write(BusMaster::Cpu(0), 0xF000, 0x12);
        assert_eq!(sys.board.bg_scroll_x, 0x12);
        // Unflipped scroll-Y is stored verbatim.
        sys.bus_write(BusMaster::Cpu(0), 0xF800, 0x05);
        assert_eq!(sys.board.bg_scroll_y, 0x05);
        // Flipscreen on → scroll-Y is compensated: (256 - data) & 0xff.
        sys.bus_write(BusMaster::Cpu(0), 0x9800, 0x01);
        assert!(sys.board.flipscreen);
        sys.bus_write(BusMaster::Cpu(0), 0xF800, 0x05);
        assert_eq!(sys.board.bg_scroll_y, 0xFB);
    }

    #[test]
    fn p1_input_clears_active_low_bits() {
        let mut sys = MrdoSystem::new();
        assert_eq!(sys.board.in0, 0xFF);
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_P1_RIGHT),
            pressed: true,
        });
        assert_eq!(sys.board.in0 & 0x04, 0, "P1 Right should clear bit 2");
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_P1_RIGHT),
            pressed: false,
        });
        assert_eq!(sys.board.in0, 0xFF);
    }

    #[test]
    fn sound_produces_non_silent_audio() {
        let mut sys = MrdoSystem::new();
        // Program SN76489 #1 channel 0: a mid tone at full volume (attenuator 0).
        // With no program ROM the Z80 just NOPs, so it never disturbs the chip.
        for byte in [0x8Eu8, 0x0F, 0x90] {
            sys.bus_write(BusMaster::Cpu(0), 0x9801, byte);
        }
        sys.run_frame();
        let mut buf = vec![0i16; 1024];
        let n = sys.board.fill_audio(&mut buf);
        assert!(n > 0, "resampler produced no samples for a frame");
        assert!(
            buf[..n].iter().any(|&s| s != 0),
            "a full-volume tone should produce audible output"
        );
    }

    #[test]
    fn reset_silences_sound() {
        let mut sys = MrdoSystem::new();
        for byte in [0x8Eu8, 0x0F, 0x90] {
            sys.bus_write(BusMaster::Cpu(0), 0x9802, byte);
        }
        sys.board.reset();
        // After reset both PSGs are attenuated to silence.
        assert_eq!(sys.board.sn1.output(), 0);
        assert_eq!(sys.board.sn2.output(), 0);
    }

    #[test]
    fn boots_and_runs_frames_without_panicking() {
        let mut sys = MrdoSystem::new();
        sys.reset();
        for _ in 0..3 {
            sys.run_frame();
        }
        // Native (unrotated) framebuffer; the frontend applies ROT270.
        assert_eq!(
            TIMING.display_size(),
            (VISIBLE_WIDTH as u32, VISIBLE_HEIGHT as u32)
        );
        assert_eq!(
            sys.board.orientation(),
            phosphor_core::core::machine::Orientation::ROT270
        );
        let mut buf = vec![0u8; VISIBLE_WIDTH * VISIBLE_HEIGHT * 3];
        sys.board.render_frame(&mut buf);
    }

    #[test]
    fn dip_banks_are_valid() {
        crate::assert_dip_banks_valid(MRDO_DIP_BANKS, &[DSW1_DEFAULT, DSW2_DEFAULT]);
    }

    #[test]
    fn dip_defaults_match_mame() {
        let sys = MrdoSystem::new();
        // DSW1: Easy difficulty, upright cabinet, 3 lives.
        assert_eq!(sys.dip_bank_value(0) & 0x03, 0x03);
        assert_eq!(sys.dip_bank_value(0) & 0x20, 0x00);
        assert_eq!(sys.dip_bank_value(0) & 0xc0, 0xc0);
        // DSW2: 1 Coin / 1 Credit on both slots.
        assert_eq!(sys.dip_bank_value(1) & 0x0f, 0x0f);
        assert_eq!(sys.dip_bank_value(1) & 0xf0, 0xf0);
    }
}
