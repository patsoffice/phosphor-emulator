//! Data East btime board (shared hardware).
//!
//! `BtimeBoard` models the hardware common to every game on Data East's
//! btime family (Burgertime, Bump'n'Jump, Lock'n'Chase, Zoar,
//! Disco No.1, …). Per-game wrappers (see `burgertime.rs`) own a `board`
//! field plus a [`BtimeConfig`] describing the variation points and forward
//! the `MachineCore`/capability traits to the board.
//!
//! Fully implemented: the main DECO CPU-7 (an NMOS 6502 with runtime opcode
//! encryption), the memory map, GFX decode, the `BGR_233_inverted` palette, the
//! char/sprite/background renderer (ROT270), inputs, DIP banks, the live VBLANK
//! bit, the coin IRQ, the frame loop, and the sound subsystem (a second M6502 @
//! 500 kHz driving two AY-3-8910s @ 1.5 MHz, with the command latch IRQ and the
//! scanline-gated NMI). The game boots, plays, and has sound.

use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, ClockDivider, TimingConfig};
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::device::ay8910::Ay8910;
use phosphor_core::gfx::decode::{GfxCache, GfxLayout, decode_gfx};
use phosphor_core::gfx::rotate_270_indexed;
use phosphor_macros::{BusDebug, MemoryRegion};

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
enum Region {
    /// Main-CPU program ROM at 0xB000-0xFFFF (0xB000-0xBFFF is an unused gap;
    /// the physical ROMs sit at 0xC000-0xFFFF with the vectors at 0xFFFA-0xFFFF).
    Main = 1,
    /// Sound-CPU program ROM at 0xE000-0xEFFF, mirrored to 0xF000-0xFFFF
    /// (vectors read from 0xFFFA-0xFFFF).
    SoundRom = 2,
}

/// DECO CPU-7 opcode deobfuscation: bit-permute the fetched byte.
///
/// The moving bits form one 5-cycle (2→3→5→6→7→2); bits 0/1/4 pass through.
/// Applied to an opcode fetch that (a) follows a main-CPU write and (b) sits at
/// an address where `(addr & 0x0104) == 0x0104`.
#[inline]
fn deco_cpu7_decrypt(v: u8) -> u8 {
    ((v >> 6) & 1) << 7
        | ((v >> 5) & 1) << 6
        | ((v >> 3) & 1) << 5
        | ((v >> 4) & 1) << 4
        | ((v >> 2) & 1) << 3
        | ((v >> 7) & 1) << 2
        | ((v >> 1) & 1) << 1
        | (v & 1)
}

// ---------------------------------------------------------------------------
// GFX layouts. The 3bpp planar ROM orders its planes so that plane 0 is the
// most-significant pixel bit; phosphor's GfxLayout is LSB-first
// (plane_offsets[0] -> pixel bit 0), so the three plane offsets are reversed.
// ---------------------------------------------------------------------------

/// Characters: gfx1, 8×8, 3bpp planar, 1024 tiles.
const CHAR_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 0x2000 * 8, 0x4000 * 8],
    x_offsets: &[0, 1, 2, 3, 4, 5, 6, 7],
    y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
    char_increment: 8 * 8,
};

/// Sprites: gfx1, 16×16, 3bpp planar, 256 tiles. The two 8-pixel halves of each
/// row are stored 16 bytes apart (x offsets 128..135 then 0..7).
const SPRITE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 0x2000 * 8, 0x4000 * 8],
    x_offsets: &[
        128, 129, 130, 131, 132, 133, 134, 135, 0, 1, 2, 3, 4, 5, 6, 7,
    ],
    y_offsets: &[
        0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104, 112, 120,
    ],
    char_increment: 32 * 8,
};

/// Background tiles: gfx2 (0x1800), 16×16, 3bpp planar, 64 tiles (same layout as
/// the sprites but over the smaller region, so the plane thirds are 0/0x800/0x1000).
const BG_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 0x0800 * 8, 0x1000 * 8],
    x_offsets: &[
        128, 129, 130, 131, 132, 133, 134, 135, 0, 1, 2, 3, 4, 5, 6, 7,
    ],
    y_offsets: &[
        0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104, 112, 120,
    ],
    char_increment: 32 * 8,
};

const NUM_CHARS: usize = 1024;
const NUM_SPRITES: usize = 256;
const NUM_BG_TILES: usize = 64;

// Video draws into a native 256×256 palette-index buffer; the visible window is
// the [8,248) square (horizontal and vertical blank end/start = 8/248), cropped
// to 240×240 and rotated ROT270. Background tiles use palette entries 8..15
// (color base 8); chars/sprites use 0..7.
//
// The square raster is displayed on a 4:3 tube rotated to portrait, so the final
// display is 3:4 — the 240×240 image is stretched vertically to 240×320 to
// restore the intended aspect (see TIMING.display_*).
const NATIVE_DIM: usize = 256;
const CROP_LO: usize = 8;
const VISIBLE_DIM: usize = 240;
const BG_PALETTE_BASE: usize = 8;

/// Size in bytes of the display framebuffer (RGB24 at the presentation size).
const fn display_bytes() -> usize {
    let (w, h) = TIMING.display_size();
    (w * h * 3) as usize
}

/// AY-3-8910 chip clock: 12 MHz / 2 / 2 / 2 = 1.5 MHz (equal to the main clock,
/// so each chip is ticked once per main tick).
const AY_CLOCK_HZ: u64 = 1_500_000;

/// Expand a 3-bit color component to 8 bits (bit-replicated).
#[inline]
fn pal3bit(x: u8) -> u8 {
    let x = x & 7;
    (x << 5) | (x << 2) | (x >> 1)
}

/// Expand a 2-bit color component to 8 bits (bit-replicated).
#[inline]
fn pal2bit(x: u8) -> u8 {
    let x = x & 3;
    (x << 6) | (x << 4) | (x << 2) | x
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------
// Main CPU: 12 MHz / 2 / 2 / 2 = 1.5 MHz.
// Screen: pixel clock 6 MHz, HTOTAL 384, VTOTAL 272,
// visible 240x240, orientation ROT270. Frame rate: 6e6 / (384 * 272) ≈ 57.44 Hz.
// CPU cycles per scanline: 1_500_000 / (57.44 * 272) ≈ 96.
pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 1_500_000, // 12 MHz / 8
    cycles_per_scanline: 96, // HTOTAL 384 pixel clocks / 4
    total_scanlines: 272,    // VTOTAL
    // Display size is the presentation size, not the raster: the 240×240 visible
    // square is shown 3:4 (portrait 4:3 tube), so height is stretched to 320.
    display_width: 240,
    display_height: 320,
    display_aspect: None,
};

// ---------------------------------------------------------------------------
// Per-game configuration
// ---------------------------------------------------------------------------

/// Per-game configuration for the shared btime board.
///
/// Only Burgertime's variant is implemented in this pass. Sibling games differ
/// in their opcode encryption (DECO CPU-7 vs. CPU-6/222 vs. none), palette
/// source (decoded `BGR_233_inverted` vs. PROM), background-tilemap presence,
/// and audio NMI wiring; those become fields here as each sibling is added.
pub struct BtimeConfig {
    /// Machine id — also the save-state tag and CLI name.
    pub name: &'static str,
}

// ---------------------------------------------------------------------------
// BtimeBoard
// ---------------------------------------------------------------------------

/// Shared Data East btime hardware (Burgertime configuration in pass 1).
///
/// Memory map (main CPU):
///   0x0000-0x07FF  Work RAM
///   0x0C00-0x0C0F  Palette RAM (16 entries)
///   0x1000-0x13FF  Video RAM (char codes; sprite RAM aliased into column 0)
///   0x1400-0x17FF  Color RAM
///   0x1800-0x1BFF  Video RAM via X/Y-swap mirror
///   0x1C00-0x1FFF  Color RAM via X/Y-swap mirror
///   0x4000         IN0 (P1)      0x4001  IN1 (P2)      0x4002  system
///   0x4003         DSW1 (bit7 = live VBLANK)           0x4004  DSW2
///   0xB000-0xFFFF  Program ROM
#[derive(BusDebug)]
pub struct BtimeBoard {
    #[debug_cpu("M6502 (DECO CPU-7)")]
    pub(crate) cpu: M6502,
    #[debug_cpu("M6502 Sound")]
    pub(crate) sound_cpu: M6502,

    #[debug_map(cpu = 0)]
    pub(crate) main_map: AddressSpace16,
    #[debug_map(cpu = 1)]
    pub(crate) sound_map: AddressSpace16,

    // Sound subsystem (sound CPU @ 500 kHz; two AY-3-8910 @ 1.5 MHz).
    #[debug_device("AY-3-8910 #1")]
    ay1: Ay8910,
    #[debug_device("AY-3-8910 #2")]
    ay2: Ay8910,
    sound_ram: [u8; 0x0400],
    sound_irq: bool,           // set on main write to 0x4003, cleared on 0xA000 read
    audio_nmi_enable: bool,    // 0xC000 write bit0; ANDs with scanline bit3 -> NMI
    sound_clock: ClockDivider, // 500 kHz from the 1.5 MHz main tick (1/3)

    // Work / video memory (kept as flat arrays, not in the AddressSpace16).
    ram: [u8; 0x0800],
    videoram: [u8; 0x0400],
    colorram: [u8; 0x0400],
    palette_ram: [u8; 16],
    /// RGB expansion of `palette_ram` (rebuilt on every palette write and after
    /// load_state; not itself part of the save state).
    palette_rgb: [(u8, u8, u8); 16],

    // Decoded graphics (derived from ROM at load; not saved). Consumed by the
    // renderer.
    chars: GfxCache,      // 8×8×3, 1024 tiles (gfx1)
    sprites: GfxCache,    // 16×16×3, 256 tiles (gfx1)
    bg_tiles: GfxCache,   // 16×16×3, 64 tiles (gfx2)
    bg_map: [u8; 0x0800], // background tilemap selector ROM

    // Display framebuffer (240×320 RGB), refreshed once per frame at the end of
    // run_frame. Derived output, not part of the save state.
    framebuffer: Vec<u8>,

    // DECO CPU-7 decryption state: any main-CPU write arms decryption of the
    // next opcode fetch (consumed in `bus_read`).
    main_had_written: bool,

    // I/O latches
    pub(crate) main_irq: bool, // coin-insertion IRQ (HOLD_LINE approximation)
    flip_screen: bool,         // 0x4002 write bit0
    bnj_scroll0: u8,           // 0x4004 write (bit4 -> background enable)
    sound_latch: u8,           // 0x4003 write — stored; sound CPU/IRQ deferred (§10)

    // Input ports (active-low players, active-high coins) and DIP banks.
    // Mutated directly by the wrapper's `handle_input` (same-crate access, per
    // the joust.rs pattern).
    pub(crate) p1: u8,
    pub(crate) p2: u8,
    pub(crate) system: u8,
    pub(crate) dsw1: u8, // bits 0-6 are DIPs; bit 7 is the live VBLANK (injected on read)
    pub(crate) dsw2: u8,

    // Per-game configuration (identity + future variation points).
    config: BtimeConfig,

    clock: u64,
}

impl BtimeBoard {
    /// X/Y address swap over the 32×32 video/color window.
    /// An involution: `off = 32*y + x  ->  32*x + y`. Backs the 0x1800/0x1C00
    /// sprite-RAM mirror (the game reaches column-0 sprite RAM through it).
    #[inline]
    pub(crate) fn swap(off: usize) -> usize {
        let x = off / 32;
        let y = off % 32;
        32 * y + x
    }

    pub fn new(config: BtimeConfig) -> Self {
        let mut main_map = AddressSpace16::new();
        main_map.region(
            Region::Main,
            "Program ROM",
            0xB000,
            0x5000,
            AccessKind::ReadOnly,
        );

        let mut sound_map = AddressSpace16::new();
        sound_map
            .region(
                Region::SoundRom,
                "Sound ROM",
                0xE000,
                0x1000,
                AccessKind::ReadOnly,
            )
            .mirror(0xF000, 0xE000, 0x1000);

        let mut board = Self {
            cpu: M6502::new(),
            sound_cpu: M6502::new(),
            main_map,
            sound_map,
            ay1: Ay8910::new(AY_CLOCK_HZ),
            ay2: Ay8910::new(AY_CLOCK_HZ),
            sound_ram: [0; 0x0400],
            sound_irq: false,
            audio_nmi_enable: false,
            sound_clock: ClockDivider::new(1, 3),
            ram: [0; 0x0800],
            videoram: [0; 0x0400],
            colorram: [0; 0x0400],
            palette_ram: [0; 16],
            palette_rgb: [(0, 0, 0); 16],
            chars: GfxCache::new(NUM_CHARS, 8, 8),
            sprites: GfxCache::new(NUM_SPRITES, 16, 16),
            bg_tiles: GfxCache::new(NUM_BG_TILES, 16, 16),
            bg_map: [0; 0x0800],
            framebuffer: vec![0u8; display_bytes()],
            main_had_written: false,
            main_irq: false,
            flip_screen: false,
            bnj_scroll0: 0,
            sound_latch: 0,
            // Players idle (active-low = all bits high).
            p1: 0xFF,
            p2: 0xFF,
            // Start/tilt idle high (bits 0-2), coin bits (6-7) low.
            system: 0x07,
            // DSW1: Coin A/B 1C/1C (0x03|0x0c), "Leave Off" bit4 set (required or
            // boot locks), Upright. Bit 7 excluded (live VBLANK, injected on read).
            dsw1: 0x1F,
            // DSW2: 3 lives, 20000 bonus, 4 enemies, end-of-level pepper on.
            dsw2: 0x0B,
            config,
            clock: 0,
        };
        board.rebuild_palette();
        board.render();
        board
    }

    /// Machine id (identity comes from the per-game [`BtimeConfig`]).
    pub fn machine_id(&self) -> &str {
        self.config.name
    }

    /// Load the assembled main-CPU program ROM (region base 0xB000; the physical
    /// ROMs occupy 0xC000-0xFFFF, so `data` is 0x5000 bytes with a 0x1000 gap).
    pub fn load_main_rom(&mut self, data: &[u8]) {
        self.main_map.load_region(Region::Main, data);
    }

    /// Decode the gfx1 region into the char (8×8) and sprite (16×16) caches.
    pub fn load_gfx1(&mut self, gfx1: &[u8]) {
        self.chars = decode_gfx(gfx1, 0, NUM_CHARS, &CHAR_LAYOUT);
        self.sprites = decode_gfx(gfx1, 0, NUM_SPRITES, &SPRITE_LAYOUT);
    }

    /// Decode the gfx2 region into the background-tile cache (16×16).
    pub fn load_gfx2(&mut self, gfx2: &[u8]) {
        self.bg_tiles = decode_gfx(gfx2, 0, NUM_BG_TILES, &BG_LAYOUT);
    }

    /// Copy the background tilemap selector ROM (bg_map).
    pub fn load_bg_map(&mut self, data: &[u8]) {
        let n = data.len().min(self.bg_map.len());
        self.bg_map[..n].copy_from_slice(&data[..n]);
    }

    /// Load the sound-CPU program ROM (0x1000 bytes at 0xE000, mirrored to 0xF000).
    pub fn load_sound_rom(&mut self, data: &[u8]) {
        self.sound_map.load_region(Region::SoundRom, data);
    }

    // --- Decoded-graphics / palette accessors (used by the `.4` renderer) ---

    pub fn chars(&self) -> &GfxCache {
        &self.chars
    }
    pub fn sprites(&self) -> &GfxCache {
        &self.sprites
    }
    pub fn bg_tiles(&self) -> &GfxCache {
        &self.bg_tiles
    }
    pub fn bg_map(&self) -> &[u8] {
        &self.bg_map
    }

    /// Decoded char/bg/sprite sheets for the interactive GFX viewer
    /// (`--gfxview`). A board method so it can borrow the private `palette_rgb`.
    pub(crate) fn gfx_sheets(&self) -> Vec<phosphor_core::core::machine::GfxSheet<'_>> {
        use phosphor_core::core::machine::GfxSheet;
        vec![
            GfxSheet {
                name: "chars",
                cache: &self.chars,
                palette: &self.palette_rgb,
            },
            GfxSheet {
                name: "bg",
                cache: &self.bg_tiles,
                palette: &self.palette_rgb,
            },
            GfxSheet {
                name: "sprites",
                cache: &self.sprites,
                palette: &self.palette_rgb,
            },
        ]
    }

    /// Recompute one palette entry from `palette_ram` using the DECO
    /// `BGR_233_inverted` decode (invert, then R=bits0-2, G=bits3-5, B=bits6-7).
    fn update_palette_entry(&mut self, i: usize) {
        let v = !self.palette_ram[i & 0x0F];
        let r = pal3bit(v & 7);
        let g = pal3bit((v >> 3) & 7);
        let b = pal2bit((v >> 6) & 3);
        self.palette_rgb[i & 0x0F] = (r, g, b);
    }

    /// Recompute all 16 palette entries (after construction / load_state).
    fn rebuild_palette(&mut self) {
        for i in 0..16 {
            self.update_palette_entry(i);
        }
    }

    // --- Core tick ---

    pub fn tick(&mut self, bus: &mut dyn Bus<Address = u16, Data = u8>) {
        // Main CPU @ 1.5 MHz.
        if self.main_map.has_any_watchpoints() {
            let pc = self
                .cpu
                .at_instruction_boundary()
                .then_some(self.cpu.pc as u32);
            self.main_map.latch_access_context(self.clock, pc);
        }
        self.cpu.execute_cycle(bus, BusMaster::Cpu(0));

        // Sound CPU @ 500 kHz (main / 3).
        if self.sound_clock.tick() {
            if self.sound_map.has_any_watchpoints() {
                let pc = self
                    .sound_cpu
                    .at_instruction_boundary()
                    .then_some(self.sound_cpu.pc as u32);
                self.sound_map.latch_access_context(self.clock, pc);
            }
            self.sound_cpu.execute_cycle(bus, BusMaster::Cpu(1));
        }

        // Both AY-3-8910s @ 1.5 MHz (once per main tick).
        self.ay1.tick();
        self.ay2.tick();

        self.clock += 1;
    }

    pub fn reset(&mut self) {
        self.main_had_written = false;
        self.main_irq = false;
        self.sound_irq = false;
        self.audio_nmi_enable = false;
        self.sound_clock.reset();
        self.ay1.reset();
        self.ay2.reset();
        self.clock = 0;
        // CPU resets are driven by the wrapper via `bus_split!` (Bus lives there).
    }

    /// Current scanline (0-271) within the frame.
    fn current_scanline(&self) -> u64 {
        (self.clock % TIMING.cycles_per_frame()) / TIMING.cycles_per_scanline
    }

    /// True during vertical blanking — the current scanline is outside the
    /// visible [8, 248) window. The game polls this on the 0x4003 read.
    fn in_vblank(&self) -> bool {
        !(8..248).contains(&self.current_scanline())
    }

    /// Sound-CPU NMI line: the audio NMI enable ANDed with scanline bit 3 (the
    /// "8vck" timer). The M6502 edge-detects, so this fires once per rising edge.
    fn sound_nmi_asserted(&self) -> bool {
        self.audio_nmi_enable && ((self.current_scanline() >> 3) & 1) != 0
    }

    /// Returns a bitmask of CPUs at instruction boundaries. Bit 0 = main CPU,
    /// bit 1 = sound CPU.
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

    // --- Capability-trait helpers (called by the game wrapper) ---

    /// Refresh the display framebuffer from current video state. Called once per
    /// frame at the end of `run_frame`. Draws the visible 240×240 image, then
    /// stretches it vertically to the 3:4 presentation aspect (240×320).
    pub fn render(&mut self) {
        let mut visible = vec![0u8; VISIBLE_DIM * VISIBLE_DIM * 3];
        self.render_visible(&mut visible);

        // Vertical nearest-neighbor stretch VISIBLE_DIM -> display height.
        let (w, h) = TIMING.display_size();
        let (w, h) = (w as usize, h as usize);
        let row_bytes = w * 3;
        for oy in 0..h {
            let sy = oy * VISIBLE_DIM / h;
            let src = sy * row_bytes;
            let dst = oy * row_bytes;
            self.framebuffer[dst..dst + row_bytes].copy_from_slice(&visible[src..src + row_bytes]);
        }
    }

    /// Copy the latest framebuffer into the frontend's `buffer`.
    pub fn render_frame(&self, buffer: &mut [u8]) {
        buffer.copy_from_slice(&self.framebuffer);
    }

    /// Drain both AY-3-8910s and mix them into `buffer` (mono, 44.1 kHz). Both
    /// chips are ticked identically, so they produce the same sample count.
    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        let n1 = self.ay1.fill_audio(buffer);
        let mut tmp = vec![0i16; n1];
        let n2 = self.ay2.fill_audio(&mut tmp);
        for (out, &s) in buffer.iter_mut().zip(tmp.iter()).take(n1.min(n2)) {
            *out = out.saturating_add(s);
        }
        n1
    }

    /// Render the visible frame: draw the native 256×256 layers into a
    /// palette-index buffer, crop the visible [8,248)² window, and rotate
    /// ROT270 into the RGB `buffer` (240×240, square pixels).
    fn render_visible(&self, buffer: &mut [u8]) {
        let mut native = vec![0u8; NATIVE_DIM * NATIVE_DIM];

        if self.bnj_scroll0 & 0x10 != 0 {
            self.draw_background(&mut native);
            self.draw_chars(&mut native, true);
        } else {
            self.draw_chars(&mut native, false);
        }
        self.draw_sprites(&mut native);

        // Crop the visible window to a 240×240 index buffer.
        let mut cropped = vec![0u8; VISIBLE_DIM * VISIBLE_DIM];
        for y in 0..VISIBLE_DIM {
            let src = (y + CROP_LO) * NATIVE_DIM + CROP_LO;
            cropped[y * VISIBLE_DIM..(y + 1) * VISIBLE_DIM]
                .copy_from_slice(&native[src..src + VISIBLE_DIM]);
        }

        rotate_270_indexed(
            &cropped,
            buffer,
            VISIBLE_DIM,
            VISIBLE_DIM,
            &self.palette_rgb,
        );
    }

    /// Blit one tile from `cache` into the native index buffer at (`sx`,`sy`),
    /// clipping to the 256×256 native area. `transparent` skips pen 0; otherwise
    /// every pixel (including 0) is written. Final index is `pal_base + pixel`.
    #[allow(clippy::too_many_arguments)]
    fn blit_tile(
        &self,
        native: &mut [u8],
        cache: &GfxCache,
        code: usize,
        sx: i32,
        sy: i32,
        flipx: bool,
        flipy: bool,
        pal_base: usize,
        transparent: bool,
    ) {
        if code >= cache.count() {
            return;
        }
        let w = cache.width();
        let h = cache.height();
        for ty in 0..h {
            let dy = sy + ty as i32;
            if !(0..NATIVE_DIM as i32).contains(&dy) {
                continue;
            }
            let py = if flipy { h - 1 - ty } else { ty };
            for tx in 0..w {
                let dx = sx + tx as i32;
                if !(0..NATIVE_DIM as i32).contains(&dx) {
                    continue;
                }
                let px = if flipx { w - 1 - tx } else { tx };
                let pixel = cache.pixel(code, px, py);
                if transparent && pixel == 0 {
                    continue;
                }
                native[dy as usize * NATIVE_DIM + dx as usize] = (pal_base + pixel as usize) as u8;
            }
        }
    }

    /// Chars: 32×32 grid, `code = videoram[off] + 256*(colorram[off] & 3)`,
    /// transposed `x = 31 - off/32`, `y = off % 32`.
    fn draw_chars(&self, native: &mut [u8], transparent: bool) {
        for off in 0..0x400 {
            let mut x = 31 - (off / 32);
            let mut y = off % 32;
            let code = self.videoram[off] as usize + 256 * (self.colorram[off] as usize & 3);
            if self.flip_screen {
                x = 31 - x;
                y = 31 - y;
            }
            self.blit_tile(
                native,
                &self.chars,
                code,
                8 * x as i32,
                8 * y as i32,
                self.flip_screen,
                self.flip_screen,
                0,
                transparent,
            );
        }
    }

    /// Sprites: 8 hardware sprites, attributes interleaved 0x20 apart in video
    /// RAM; drawn twice for ±256 wrap.
    fn draw_sprites(&self, native: &mut [u8]) {
        for i in 0..8 {
            let off = i * 0x80;
            if self.videoram[off] & 0x01 == 0 {
                continue;
            }
            let mut x = 240 - self.videoram[off + 0x60] as i32;
            let mut y = 240 - self.videoram[off + 0x40] as i32;
            let mut flipx = self.videoram[off] & 0x04 != 0;
            let mut flipy = self.videoram[off] & 0x02 != 0;
            if self.flip_screen {
                x = 240 - x;
                y = 240 - y; // sprite_y_adjust_flip_screen = 0
                flipx = !flipx;
                flipy = !flipy;
            }
            y -= 1; // sprite_y_adjust = 1
            let code = self.videoram[off + 0x20] as usize;
            self.blit_tile(native, &self.sprites, code, x, y, flipx, flipy, 0, true);
            // Wrap-around copy.
            let y2 = y + if self.flip_screen { -256 } else { 256 };
            self.blit_tile(native, &self.sprites, code, x, y2, flipx, flipy, 0, true);
        }
    }

    /// Background: up to 4 columns of 16×16 tiles selected from `bg_map`,
    /// horizontally scrolled by `(bnj_scroll0 & 3) << 8`. The four column tiles
    /// cycle `start..start+3`, offset by `bnj_scroll0 & 0x04`.
    fn draw_background(&self, native: &mut [u8]) {
        let mut start = if self.flip_screen { 0u8 } else { 1u8 };
        let mut tmap = [0u8; 4];
        for slot in tmap.iter_mut() {
            *slot = start | (self.bnj_scroll0 & 0x04);
            start = (start + 1) & 0x03;
        }

        // The second scroll register is never written on this game, so it is 0.
        let mut scroll: i32 = -(((self.bnj_scroll0 & 0x03) as i32) << 8);
        for i in 0..5 {
            if scroll > 256 {
                break;
            }
            if scroll >= -256 {
                let tileoffset = tmap[i & 3] as usize * 0x100;
                for off in 0..0x100usize {
                    let mut x = 240 - (16 * (off / 16) as i32 + scroll) - 1;
                    let mut y = 16 * (off % 16) as i32;
                    if self.flip_screen {
                        x = 240 - x;
                        y = 240 - y;
                    }
                    let code = self.bg_map[tileoffset + off] as usize;
                    self.blit_tile(
                        native,
                        &self.bg_tiles,
                        code,
                        x,
                        y,
                        self.flip_screen,
                        self.flip_screen,
                        BG_PALETTE_BASE,
                        false,
                    );
                }
            }
            scroll += 256;
        }
    }

    // --- Bus (master-dispatched: Cpu(0) = main, Cpu(1) = sound) ---

    pub(crate) fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        if master == BusMaster::Cpu(1) {
            return self.sound_read(addr);
        }
        let a = addr as usize;
        let mut data = match addr {
            0x0000..=0x07FF => self.ram[a & 0x07FF],
            0x0C00..=0x0C0F => self.palette_ram[a & 0x0F],
            0x1000..=0x13FF => self.videoram[a & 0x03FF],
            0x1400..=0x17FF => self.colorram[a & 0x03FF],
            0x1800..=0x1BFF => self.videoram[Self::swap(a & 0x03FF)],
            0x1C00..=0x1FFF => self.colorram[Self::swap(a & 0x03FF)],
            0x4000 => self.p1,
            0x4001 => self.p2,
            0x4002 => self.system,
            // Bit 7 is the live VBLANK line (active-high), not a DIP.
            0x4003 => (self.dsw1 & 0x7F) | if self.in_vblank() { 0x80 } else { 0 },
            0x4004 => self.dsw2,
            0xB000..=0xFFFF => self.main_map.read_backing(addr),
            _ => 0,
        };

        // The coin IRQ is HOLD_LINE: the CPU vectoring through 0xFFFE (IRQ/BRK
        // vector low byte) acknowledges it, so exactly one IRQ fires per coin
        // edge even while the coin button is held.
        if addr == 0xFFFE {
            self.main_irq = false;
        }

        // DECO CPU-7: on an opcode fetch (is_sync) that follows any main-CPU
        // write, consume the "had written" flag and deobfuscate the fetched
        // byte when (addr & 0x0104) == 0x0104. The flag clears on every sync
        // fetch regardless of address; only matching addresses are decrypted.
        if self.cpu.is_sync() && self.main_had_written {
            self.main_had_written = false;
            if (addr & 0x0104) == 0x0104 {
                data = deco_cpu7_decrypt(data);
            }
        }

        self.main_map.watch_read(0, master, addr, data);
        data
    }

    pub(crate) fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        if master == BusMaster::Cpu(1) {
            self.sound_write(addr, data);
            return;
        }
        self.main_map.watch_write(0, master, addr, data);
        // Any main-CPU write arms DECO CPU-7 decryption of the next opcode fetch.
        self.main_had_written = true;

        let a = addr as usize;
        match addr {
            0x0000..=0x07FF => self.ram[a & 0x07FF] = data,
            0x0C00..=0x0C0F => {
                self.palette_ram[a & 0x0F] = data;
                self.update_palette_entry(a & 0x0F);
            }
            0x1000..=0x13FF => self.videoram[a & 0x03FF] = data,
            0x1400..=0x17FF => self.colorram[a & 0x03FF] = data,
            0x1800..=0x1BFF => self.videoram[Self::swap(a & 0x03FF)] = data,
            0x1C00..=0x1FFF => self.colorram[Self::swap(a & 0x03FF)] = data,
            0x4002 => self.flip_screen = data & 1 != 0,
            // Latch the sound command and raise the sound-CPU IRQ.
            0x4003 => {
                self.sound_latch = data;
                self.sound_irq = true;
            }
            0x4004 => self.bnj_scroll0 = data,
            _ => {}
        }
    }

    /// Sound-CPU bus read (audio map). RAM is mirrored across 0x0000-0x1FFF;
    /// reading the command latch at 0xA000 acknowledges the sound IRQ.
    fn sound_read(&mut self, addr: u16) -> u8 {
        let data = match addr {
            0x0000..=0x1FFF => self.sound_ram[(addr & 0x03FF) as usize],
            0xA000..=0xBFFF => {
                self.sound_irq = false;
                self.sound_latch
            }
            0xE000..=0xFFFF => self.sound_map.read_backing(addr),
            _ => 0,
        };
        self.sound_map.watch_read(1, BusMaster::Cpu(1), addr, data);
        data
    }

    /// Sound-CPU bus write (audio map): RAM, the two AYs (address/data latches),
    /// and the audio NMI enable at 0xC000.
    fn sound_write(&mut self, addr: u16, data: u8) {
        self.sound_map.watch_write(1, BusMaster::Cpu(1), addr, data);
        match addr {
            0x0000..=0x1FFF => self.sound_ram[(addr & 0x03FF) as usize] = data,
            0x2000..=0x3FFF => self.ay1.data_write(data),
            0x4000..=0x5FFF => self.ay1.address_write(data),
            0x6000..=0x7FFF => self.ay2.data_write(data),
            0x8000..=0x9FFF => self.ay2.address_write(data),
            0xC000..=0xDFFF => self.audio_nmi_enable = data & 1 != 0,
            _ => {}
        }
    }

    pub(crate) fn bus_is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    pub(crate) fn bus_check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        let (nmi, irq) = if target == BusMaster::Cpu(1) {
            (self.sound_nmi_asserted(), self.sound_irq)
        } else {
            (false, self.main_irq)
        };
        InterruptState {
            nmi,
            irq,
            firq: false,
            irq_vector: 0,
            irq_level: 0,
        }
    }
}

impl Saveable for BtimeBoard {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        self.sound_cpu.save_state(w);
        w.write_bytes(&self.ram);
        w.write_bytes(&self.videoram);
        w.write_bytes(&self.colorram);
        w.write_bytes(&self.palette_ram);
        w.write_bytes(&self.sound_ram);
        self.ay1.save_state(w);
        self.ay2.save_state(w);
        self.sound_clock.save_state(w);
        w.write_bool(self.main_had_written);
        w.write_bool(self.main_irq);
        w.write_bool(self.sound_irq);
        w.write_bool(self.audio_nmi_enable);
        w.write_bool(self.flip_screen);
        w.write_u8(self.bnj_scroll0);
        w.write_u8(self.sound_latch);
        w.write_u8(self.p1);
        w.write_u8(self.p2);
        w.write_u8(self.system);
        w.write_u8(self.dsw1);
        w.write_u8(self.dsw2);
        w.write_u64_le(self.clock);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        self.sound_cpu.load_state(r)?;
        r.read_bytes_into(&mut self.ram)?;
        r.read_bytes_into(&mut self.videoram)?;
        r.read_bytes_into(&mut self.colorram)?;
        r.read_bytes_into(&mut self.palette_ram)?;
        r.read_bytes_into(&mut self.sound_ram)?;
        self.ay1.load_state(r)?;
        self.ay2.load_state(r)?;
        self.sound_clock.load_state(r)?;
        self.main_had_written = r.read_bool()?;
        self.main_irq = r.read_bool()?;
        self.sound_irq = r.read_bool()?;
        self.audio_nmi_enable = r.read_bool()?;
        self.flip_screen = r.read_bool()?;
        self.bnj_scroll0 = r.read_u8()?;
        self.sound_latch = r.read_u8()?;
        self.p1 = r.read_u8()?;
        self.p2 = r.read_u8()?;
        self.system = r.read_u8()?;
        self.dsw1 = r.read_u8()?;
        self.dsw2 = r.read_u8()?;
        self.clock = r.read_u64_le()?;
        // palette_rgb and the framebuffer are derived, not saved — rebuild them
        // so a render before the next run_frame reflects the restored state.
        self.rebuild_palette();
        self.render();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> BtimeBoard {
        BtimeBoard::new(BtimeConfig { name: "btime-test" })
    }

    #[test]
    fn timing_frame_rate_is_btime() {
        // ~57.44 Hz from 6 MHz / (384 * 272).
        let hz = TIMING.frame_rate_hz();
        assert!((hz - 57.44).abs() < 0.5, "frame rate {hz} not ~57.44");
        assert_eq!(TIMING.cycles_per_frame(), 96 * 272);
    }

    #[test]
    fn ram_read_write_roundtrip() {
        let mut b = board();
        b.bus_write(BusMaster::Cpu(0), 0x0042, 0xAB);
        assert_eq!(b.bus_read(BusMaster::Cpu(0), 0x0042), 0xAB);
    }

    #[test]
    fn video_and_color_ram_roundtrip() {
        let mut b = board();
        b.bus_write(BusMaster::Cpu(0), 0x1005, 0x12);
        b.bus_write(BusMaster::Cpu(0), 0x1405, 0x34);
        assert_eq!(b.bus_read(BusMaster::Cpu(0), 0x1005), 0x12);
        assert_eq!(b.bus_read(BusMaster::Cpu(0), 0x1405), 0x34);
    }

    #[test]
    fn any_write_arms_deco_decryption() {
        let mut b = board();
        assert!(!b.main_had_written);
        b.bus_write(BusMaster::Cpu(0), 0x0000, 0x00);
        assert!(b.main_had_written);
    }

    #[test]
    fn coin_irq_reported_through_interrupts() {
        let mut b = board();
        assert!(!b.bus_check_interrupts(BusMaster::Cpu(0)).irq);
        b.main_irq = true;
        assert!(b.bus_check_interrupts(BusMaster::Cpu(0)).irq);
    }

    #[test]
    fn irq_vector_fetch_acknowledges_coin_irq() {
        let mut b = board();
        b.main_irq = true;
        // A normal read does not clear it.
        b.bus_read(BusMaster::Cpu(0), 0x0000);
        assert!(b.main_irq);
        // Vectoring through 0xFFFE (IRQ/BRK vector) acknowledges it.
        b.bus_read(BusMaster::Cpu(0), 0xFFFE);
        assert!(!b.main_irq);
    }

    // --- Sound subsystem ---

    #[test]
    fn sound_ram_reads_write_and_mirror() {
        let mut b = board();
        b.sound_write(0x0100, 0xAB);
        assert_eq!(b.sound_read(0x0100), 0xAB);
        // RAM is mirrored across 0x0000-0x1FFF (mirror mask 0x1C00).
        assert_eq!(b.sound_read(0x0100 + 0x1C00), 0xAB);
    }

    #[test]
    fn sound_latch_raises_and_reading_acks_the_irq() {
        let mut b = board();
        assert!(!b.bus_check_interrupts(BusMaster::Cpu(1)).irq);

        // A main-CPU write to 0x4003 latches the command and raises the sound IRQ.
        b.bus_write(BusMaster::Cpu(0), 0x4003, 0x42);
        assert!(b.sound_irq);
        assert!(b.bus_check_interrupts(BusMaster::Cpu(1)).irq);

        // The sound CPU reads the latch at 0xA000: returns the value and acks.
        assert_eq!(b.sound_read(0xA000), 0x42);
        assert!(!b.sound_irq);
        assert!(!b.bus_check_interrupts(BusMaster::Cpu(1)).irq);
    }

    #[test]
    fn sound_nmi_is_enable_anded_with_scanline_bit3() {
        let mut b = board();
        assert!(!b.sound_nmi_asserted());

        b.sound_write(0xC000, 0x01); // audio NMI enable
        b.clock = 0; // scanline 0 -> bit3 = 0
        assert!(!b.sound_nmi_asserted());
        b.clock = 8 * 96; // scanline 8 -> bit3 = 1
        assert!(b.sound_nmi_asserted());
        assert!(b.bus_check_interrupts(BusMaster::Cpu(1)).nmi);

        b.sound_write(0xC000, 0x00); // disable
        assert!(!b.sound_nmi_asserted());
    }

    #[test]
    fn ay_register_write_routes_and_produces_audio() {
        let mut b = board();
        // Program AY1 channel A: tone period (R0/R1) + full amplitude (R8), via
        // the address latch (0x4000) then data (0x2000).
        b.sound_write(0x4000, 0); // address = R0
        b.sound_write(0x2000, 0x55); // R0 fine tune
        b.sound_write(0x4000, 1); // address = R1
        b.sound_write(0x2000, 0x01); // R1 coarse tune
        b.sound_write(0x4000, 8); // address = R8 (channel A amplitude)
        b.sound_write(0x2000, 0x0F); // full volume

        for _ in 0..4000 {
            b.ay1.tick();
        }
        let mut buf = vec![0i16; 512];
        let n = b.ay1.fill_audio(&mut buf);
        assert!(buf[..n].iter().any(|&s| s != 0), "AY1 should output a tone");
    }

    // --- DIP defaults + live VBLANK bit ---

    #[test]
    fn dsw_power_on_defaults() {
        let b = board();
        assert_eq!(b.dsw1, 0x1F); // Coin A/B 1C1C, Leave-Off set, Upright
        assert_eq!(b.dsw2, 0x0B); // 3 lives, 20000 bonus, 4 enemies, pepper on
    }

    #[test]
    fn vblank_bit_injected_on_dsw1_read() {
        let mut b = board();
        // Visible scanline 100 -> VBLANK bit clear; DIP bits still read.
        b.clock = 100 * 96;
        let v = b.bus_read(BusMaster::Cpu(0), 0x4003);
        assert_eq!(v & 0x80, 0, "not in vblank");
        assert_eq!(v & 0x7F, 0x1F, "dsw1 bits present");
        // Scanline 0 and 260 are outside the visible [8,248) window -> bit set.
        b.clock = 0;
        assert_ne!(b.bus_read(BusMaster::Cpu(0), 0x4003) & 0x80, 0);
        b.clock = 260 * 96;
        assert_ne!(b.bus_read(BusMaster::Cpu(0), 0x4003) & 0x80, 0);
    }

    // --- X/Y-swap sprite-RAM mirror ---

    #[test]
    fn xy_swap_is_an_involution() {
        for off in 0..0x400usize {
            assert_eq!(BtimeBoard::swap(BtimeBoard::swap(off)), off);
        }
    }

    #[test]
    fn mirror_videoram_swaps_x_and_y() {
        let mut b = board();
        // A direct write to video RAM offset 5 is reachable through the 0x1800
        // mirror at the swapped address.
        b.bus_write(BusMaster::Cpu(0), 0x1000 + 5, 0x7E);
        let mirror = 0x1800 + BtimeBoard::swap(5) as u16;
        assert_eq!(b.bus_read(BusMaster::Cpu(0), mirror), 0x7E);

        // A write through the mirror lands at the swapped video-RAM offset.
        b.bus_write(BusMaster::Cpu(0), 0x1801, 0x3C); // off 1 -> videoram[swap(1)=32]
        assert_eq!(b.bus_read(BusMaster::Cpu(0), 0x1000 + 32), 0x3C);
    }

    #[test]
    fn mirror_colorram_swaps_x_and_y() {
        let mut b = board();
        b.bus_write(BusMaster::Cpu(0), 0x1400 + 5, 0x11);
        let mirror = 0x1C00 + BtimeBoard::swap(5) as u16;
        assert_eq!(b.bus_read(BusMaster::Cpu(0), mirror), 0x11);
    }

    // --- DECO CPU-7 opcode decryption ---
    //
    // A fresh M6502 sits in the Fetch state, so `is_sync()` is true and every
    // test `bus_read` behaves as an opcode fetch.

    #[test]
    fn deco_decrypt_pure_fn_known_vectors() {
        // Permutation: out7<-in6, out6<-in5, out5<-in3, out4<-in4,
        // out3<-in2, out2<-in7, out1<-in1, out0<-in0.
        assert_eq!(deco_cpu7_decrypt(0x84), 0x0C); // in7|in2 -> out2|out3
        assert_eq!(deco_cpu7_decrypt(0x00), 0x00);
        assert_eq!(deco_cpu7_decrypt(0xFF), 0xFF);
        assert_eq!(deco_cpu7_decrypt(0x01), 0x01); // bit0 fixed
        assert_eq!(deco_cpu7_decrypt(0x02), 0x02); // bit1 fixed
        assert_eq!(deco_cpu7_decrypt(0x10), 0x10); // bit4 fixed
        // The moving bits form one 5-cycle (2->3->5->6->7->2); applying the
        // swap five times is the identity.
        let mut v = 0xA5;
        for _ in 0..5 {
            v = deco_cpu7_decrypt(v);
        }
        assert_eq!(v, 0xA5);
    }

    #[test]
    fn deco_decrypts_matching_address_fetch_after_write() {
        let mut b = board();
        let mut rom = vec![0u8; 0x5000];
        rom[0x1104] = 0x84; // -> 0xC104, and 0xC104 & 0x0104 == 0x0104
        b.load_main_rom(&rom);

        b.main_had_written = true; // as a prior write would set it
        let got = b.bus_read(BusMaster::Cpu(0), 0xC104);
        assert_eq!(got, 0x0C, "fetched byte deobfuscated");
        assert!(!b.main_had_written, "sync fetch consumes the flag");
    }

    #[test]
    fn deco_leaves_nonmatching_address_raw_but_still_clears_flag() {
        let mut b = board();
        let mut rom = vec![0u8; 0x5000];
        rom[0x1000] = 0x84; // -> 0xC000, and 0xC000 & 0x0104 == 0
        b.load_main_rom(&rom);

        b.main_had_written = true;
        assert_eq!(b.bus_read(BusMaster::Cpu(0), 0xC000), 0x84, "raw byte");
        assert!(!b.main_had_written, "flag clears on any sync fetch");
    }

    #[test]
    fn deco_no_decrypt_without_a_prior_write() {
        let mut b = board();
        let mut rom = vec![0u8; 0x5000];
        rom[0x1104] = 0x84;
        b.load_main_rom(&rom);
        // main_had_written stays false: a matching address is left untouched.
        assert_eq!(b.bus_read(BusMaster::Cpu(0), 0xC104), 0x84);
    }

    // --- Palette (BGR_233_inverted) ---

    #[test]
    fn pal_bit_expanders() {
        assert_eq!(pal3bit(0), 0x00);
        assert_eq!(pal3bit(7), 0xFF);
        assert_eq!(pal2bit(0), 0x00);
        assert_eq!(pal2bit(3), 0xFF);
    }

    #[test]
    fn palette_zero_ram_is_white_after_invert() {
        // palette_ram = 0x00 -> inverted 0xFF -> full R/G/B.
        let b = board();
        assert_eq!(b.palette_rgb[0], (0xFF, 0xFF, 0xFF));
    }

    #[test]
    fn palette_write_decodes_bgr233_inverted() {
        let mut b = board();
        // 0xFF -> inverted 0x00 -> black.
        b.bus_write(BusMaster::Cpu(0), 0x0C00, 0xFF);
        assert_eq!(b.palette_rgb[0], (0, 0, 0));

        // 0xF8 -> inverted 0x07 -> R=7 only -> pure red.
        b.bus_write(BusMaster::Cpu(0), 0x0C01, 0xF8);
        assert_eq!(b.palette_rgb[1], (0xFF, 0, 0));

        // 0xC7 -> inverted 0x38 -> G=7 only -> pure green (bits 3-5).
        b.bus_write(BusMaster::Cpu(0), 0x0C02, 0xC7);
        assert_eq!(b.palette_rgb[2], (0, 0xFF, 0));

        // 0x3F -> inverted 0xC0 -> B=3 only -> pure blue (bits 6-7).
        b.bus_write(BusMaster::Cpu(0), 0x0C03, 0x3F);
        assert_eq!(b.palette_rgb[3], (0, 0, 0xFF));
    }

    // --- GFX decode ---

    #[test]
    fn gfx_cache_dimensions() {
        let b = board();
        assert_eq!((b.chars().count(), b.chars().width()), (NUM_CHARS, 8));
        assert_eq!(
            (b.sprites().count(), b.sprites().width()),
            (NUM_SPRITES, 16)
        );
        assert_eq!(
            (b.bg_tiles().count(), b.bg_tiles().height()),
            (NUM_BG_TILES, 16)
        );
        assert_eq!(b.bg_map().len(), 0x0800);
    }

    #[test]
    fn char_decode_combines_three_planes() {
        let mut b = board();
        // gfx1 region: plane thirds at 0 / 0x2000 / 0x4000. Set pixel (0,0) of
        // char 0 in planes 0 and 1 (bit 0 and bit 1), leaving plane 2 clear.
        let mut gfx1 = vec![0u8; 0x6000];
        gfx1[0x0000] = 0x80; // plane 0 (LSB), row 0, leftmost pixel
        gfx1[0x2000] = 0x80; // plane 1, same pixel
        gfx1[0x4000] = 0x00; // plane 2 clear
        b.load_gfx1(&gfx1);
        assert_eq!(b.chars().pixel(0, 0, 0), 0b011);
        assert_eq!(b.chars().pixel(0, 1, 0), 0); // neighbor untouched
    }

    #[test]
    fn sprite_decode_uses_split_row_halves() {
        let mut b = board();
        // Sprite x offsets are [128..135, 0..7]: column 0 comes from bit offset
        // 128 (byte 16), column 8 from bit offset 0 (byte 0).
        let mut gfx1 = vec![0u8; 0x6000];
        gfx1[16] = 0x80; // plane 0, sprite 0, row 0, column 0 (x offset 128)
        gfx1[0] = 0x80; // plane 0, sprite 0, row 0, column 8 (x offset 0)
        b.load_gfx1(&gfx1);
        assert_eq!(b.sprites().pixel(0, 0, 0), 0b001);
        assert_eq!(b.sprites().pixel(0, 8, 0), 0b001);
        assert_eq!(b.sprites().pixel(0, 1, 0), 0);
    }

    #[test]
    fn bg_tile_decode_uses_smaller_plane_thirds() {
        let mut b = board();
        // gfx2 region (0x1800): plane thirds at 0 / 0x0800 / 0x1000.
        let mut gfx2 = vec![0u8; 0x1800];
        gfx2[0x0000] = 0x80; // plane 0
        gfx2[0x1000] = 0x80; // plane 2 (MSB)
        b.load_gfx2(&gfx2);
        assert_eq!(b.bg_tiles().pixel(0, 8, 0), 0b101); // x offset 0 -> column 8
    }

    #[test]
    fn palette_survives_save_load() {
        let mut b = board();
        b.bus_write(BusMaster::Cpu(0), 0x0C05, 0xF8); // red
        let mut w = StateWriter::new();
        b.save_state(&mut w);
        let bytes = w.into_vec();

        let mut b2 = board();
        let mut r = StateReader::new(&bytes);
        b2.load_state(&mut r).unwrap();
        assert_eq!(b2.palette_ram[5], 0xF8);
        assert_eq!(b2.palette_rgb[5], (0xFF, 0, 0));
    }

    // --- Renderer (layers + ROT270) ---
    //
    // These target the square logical image (render_visible, 240×240); the
    // display-aspect stretch is covered separately below.

    fn pixel(buffer: &[u8], row: usize, col: usize) -> (u8, u8, u8) {
        let i = (row * VISIBLE_DIM + col) * 3;
        (buffer[i], buffer[i + 1], buffer[i + 2])
    }

    #[test]
    fn render_default_frame_is_backdrop_white() {
        // No gfx loaded: every char is code 0 / all-pen-0, drawn opaque (bg
        // disabled) -> palette entry 0, which decodes to white (ram 0 inverted).
        let b = board();
        let mut buffer = vec![0u8; VISIBLE_DIM * VISIBLE_DIM * 3];
        b.render_visible(&mut buffer);
        assert!(
            buffer.iter().all(|&c| c == 0xFF),
            "frame should be all white"
        );
    }

    #[test]
    fn render_char_lands_at_rot270_position() {
        let mut b = board();
        // char 1 = all pen 1 (plane 0 set for its 8 bytes at region offset 8).
        let mut gfx1 = vec![0u8; 0x6000];
        for byte in gfx1.iter_mut().skip(8).take(8) {
            *byte = 0xFF;
        }
        b.load_gfx1(&gfx1);
        // palette entry 1 -> red.
        b.bus_write(BusMaster::Cpu(0), 0x0C01, 0xF8);

        // Place char 1 at video RAM offset 495 (char cell x=16, y=15).
        b.videoram[495] = 1;

        let mut buffer = vec![0u8; VISIBLE_DIM * VISIBLE_DIM * 3];
        b.render_visible(&mut buffer);

        // native (128..135, 120..127) -> crop (120..127, 112..119) -> ROT270
        // out (row 239-cx, col cy) = rows 112..119, cols 112..119.
        assert_eq!(pixel(&buffer, 115, 115), (0xFF, 0, 0), "char center is red");
        assert_eq!(
            pixel(&buffer, 10, 10),
            (0xFF, 0xFF, 0xFF),
            "elsewhere white"
        );
    }

    #[test]
    fn render_background_uses_palette_base_8() {
        let mut b = board();
        // Every bg-tile pixel gets plane-0 bit set -> pen 1 -> palette index 9.
        let mut gfx2 = vec![0u8; 0x1800];
        for byte in gfx2.iter_mut().take(0x0800) {
            *byte = 0xFF;
        }
        b.load_gfx2(&gfx2);
        // Enable the background layer; palette entry 9 -> blue.
        b.bnj_scroll0 = 0x10;
        b.bus_write(BusMaster::Cpu(0), 0x0C09, 0x3F);

        let mut buffer = vec![0u8; VISIBLE_DIM * VISIBLE_DIM * 3];
        b.render_visible(&mut buffer);

        // Chars are transparent over the backdrop, so the blue bg shows through.
        let has_blue = buffer.chunks_exact(3).any(|p| p == [0, 0, 0xFF]);
        assert!(has_blue, "background (palette base 8) should be visible");
    }

    #[test]
    fn render_frame_stretches_visible_to_3x4_display() {
        let mut b = board();
        // Some vertical variation: a red char at one cell.
        let mut gfx1 = vec![0u8; 0x6000];
        for byte in gfx1.iter_mut().skip(8).take(8) {
            *byte = 0xFF; // char 1 = pen 1
        }
        b.load_gfx1(&gfx1);
        b.bus_write(BusMaster::Cpu(0), 0x0C01, 0xF8); // entry 1 -> red
        b.videoram[300] = 1;

        let (w, h) = TIMING.display_size();
        let (w, h) = (w as usize, h as usize);
        assert_eq!((w, h), (240, 320), "portrait 3:4 presentation");

        let mut vis = vec![0u8; VISIBLE_DIM * VISIBLE_DIM * 3];
        b.render_visible(&mut vis);
        b.render(); // refresh the framebuffer from current state
        let mut disp = vec![0u8; w * h * 3];
        b.render_frame(&mut disp);

        // Each display row is exactly the stretched visible row (oy*240/320).
        for oy in 0..h {
            let sy = oy * VISIBLE_DIM / h;
            let d = &disp[oy * w * 3..(oy + 1) * w * 3];
            let s = &vis[sy * VISIBLE_DIM * 3..(sy + 1) * VISIBLE_DIM * 3];
            assert_eq!(d, s, "display row {oy} maps to visible row {sy}");
        }
    }
}
