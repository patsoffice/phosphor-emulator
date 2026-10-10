//! The Namco Mappy board: Mappy (Namco, 1983) and Dig Dug II (Namco, 1985).
//!
//! One board, two games. Dig Dug II runs on Mappy hardware (no schematics of
//! its own are known): same CPUs, maps, video pipeline, sound chain and latch,
//! with its own ROMs, a 56XX in place of the second 58XX, twice the sprite
//! ROM, and the program ROM filling 0x8000-0xFFFF instead of 0xA000-0xFFFF.
//! The per-game differences are a [`MappyVariant]` the board is constructed
//! with; everything else below is shared.
//!
//! # Schematics
//!
//! | Drawing | Source | Pages |
//! |---|---|---|
//! | Mappy CPU BD. (sheet 8-5) and VIDEO BD. (sheet 8-7), Bally Midway Mappy Parts and Operating Manual (Apr 1983) | `arcarc.xmission.com/PDF_Arcade_Bally_Midway/Mappy_Parts_and_Operating_Manual_(Apr_1983).pdf` | 42 pages, schematics on PDF pp32-39, read 2026-10-09 |
//!
//! Transcribed to `docs/schematics/mappy-board.md`. Part of the address
//! decode is in two PALs (SPC-5, SPC-6) whose equations are not on the
//! drawing, the video mixing is in a PAL (MPI-4, 5D) whose equations are
//! not on the drawing either, and the I/O, sound and video sequencing live
//! in Namco customs (07XX, 15XX, 58XX, 99XX, 17XX, 04XX, 11XX, 12XX) drawn
//! as boxes. Where the sheets go dark, what is here was inferred from the
//! program's behavior or anchored on MAME's `namco/mappy.cpp`, and each such
//! place says so where it is used.
//!
//! The board Super Pac-Man's module calls the Mappy family grown up: two
//! MC6809E, a main CPU running the game and a sound CPU that does nothing
//! but write the 15XX's voice registers, which live in the 1 KB of RAM the
//! two share. Two 58XX MCUs read the controls and count coins, a 99XX mixes
//! the 15XX's eight voices behind a volume pot, and the playfield is a
//! 36x60 scrolling tilemap with fixed side strips.
//!
//! Memory map, main CPU (both games; the program ROM window differs):
//! ```text
//!   0x0000-0x0fff  Video RAM: tile codes 0x000-0x7ff, attributes 0x800-0xfff
//!   0x1000-0x27ff  Work RAM, with the sprite registers at 0x1780, 0x1f80, 0x2780
//!   0x3800-0x3fff  Scroll, write-only: the value is the address (offset >> 3)
//!   0x4000-0x43ff  Sound RAM, shared; 0x4000-0x403f are the 15XX's registers
//!   0x4800-0x480f  MCU #0: 58XX on both    0x4810-0x481f  MCU #1: 58XX/56XX
//!   0x5000-0x500f  LS259 latch: line = A1-A3, data = A0
//!   0x8000         Watchdog reset on Mappy (not modeled); ROM on Dig Dug II
//!   0xa000-0xffff  Program ROM on Mappy (1D, 1C, 1B); 0x8000-0xffff on DD2
//! ```
//!
//! Memory map, sound CPU:
//! ```text
//!   0x0000-0x03ff  Sound RAM (the main CPU's 0x4000-0x43ff)
//!   0x2000-0x200f  The same LS259 latch
//!   0xe000-0xffff  Sound ROM (1K), a full 8K: no mirror
//! ```
//!
//! LS259 latch (2M) outputs:
//! ```text
//!   Q0 sound CPU IRQ enable   Q1 main CPU IRQ enable   Q2 cocktail flip
//!   Q3 SOUND ON               Q4 58XX reset (low = reset)
//!   Q5 sound CPU reset (low = reset)   Q6, Q7 n.c.
//! ```

use phosphor_core::audio::host_sample_rate;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::DebugTraceBuffer;
use phosphor_core::core::machine::{
    ActionRole, DipApplyTiming, DipChoice, DipCondition, DipOption, DipSwitchBank, Direction,
    InputConfigurable, InputControl, InputEvent, InputId, InputKind, MachineCore, Nvram,
    Orientation, Profilable, SaveState, TimingConfig,
};
use phosphor_core::core::save_state::{SaveError, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, ClockDomainName as Clk, ClockTree, DomainId};
use phosphor_core::cpu::m6809::M6809;
use phosphor_core::cpu::{Cpu, CpuStateTrait};
use phosphor_core::device::discrete::{
    CustomComponent, DataInputId, DiscreteCircuit, DiscreteCircuitBuilder, NodeId, OutputGain,
};
use phosphor_core::device::namco_15xx::{Namco15xx, VOICES};
use phosphor_core::device::namco56::{InPort, Namco56};
use phosphor_core::device::namco58::Namco58;
use phosphor_core::gfx::decode::{GfxCache, GfxLayout, decode_gfx};
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion, Saveable};

use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::scanline::ScanlineDriven;

/// Both CPUs: 18.432 MHz / 12 = 1.536 MHz. The pixel clock is 18.432 / 3, so
/// a 384-dot line is 96 CPU cycles, and 264 lines make 25344 cycles a frame
/// (60.61 Hz). Same counts as Super Pac-Man: same crystal, same divider.
pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 1_536_000,
    cycles_per_scanline: 96,
    total_scanlines: 264,
    // Native (pre-orientation) framebuffer: the board declares ROT90 and the
    // frontend rotates centrally.
    display_width: NATIVE_WIDTH as u32,
    display_height: NATIVE_HEIGHT as u32,
    display_aspect: Some((3, 4)),
};

/// The board's crystal and everything divided out of it.
///
/// One 18.432 MHz crystal (X1, oscillator around two LS368 sections at 5A).
/// A 74LS109 at 3B divides it by 3 for the 6.144 MHz dot clock; the 07XX
/// counts that into 1H and 2H, and both 6809Es run on 2H, 1.536 MHz. The
/// 15XX's voice update, in which each of its eight voices advances once, is
/// 256 dots (see [`Namco15xx`]): 18.432 MHz / 768.
pub fn clock_tree() -> ClockTree {
    use phosphor_core::core::RootId;
    let mut t = ClockTree::new(18_432_000);
    let cpu = t.add_domain(Clk::Cpu, RootId::MAIN, 1, 12); // 1.536 MHz
    let dot = t.add_domain(Clk::Pixel, RootId::MAIN, 1, 3); // 6.144 MHz
    t.add_domain(Clk::SoundCpu, RootId::MAIN, 1, 12); // 1.536 MHz
    t.add_domain(Clk::Psg, RootId::MAIN, 1, 768); // 15XX, 24 kHz
    t.set_step_domain(cpu);
    // 384 dot clocks per line is exactly 96 CPU cycles.
    t.set_raster(dot, 384, 0);
    t
}

/// Native framebuffer: 36 by 28 tiles.
pub const NATIVE_WIDTH: usize = 288;
pub const NATIVE_HEIGHT: usize = 224;
/// The raster line VBLANK starts on, which is also where both CPUs' IRQs are
/// raised.
const VBLANK_LINE: u64 = 224;

/// LS259 (2M) outputs.
const LATCH_SUB_INT_ON: u8 = 0x01;
const LATCH_MAIN_INT_ON: u8 = 0x02;
const LATCH_SUB_RUN: u8 = 0x20;

/// Which game the board runs. The three things that differ are the program
/// ROM window (0xA000 versus 0x8000), the DIP MCU at 0x4810 (58XX versus
/// 56XX), and the mix of live inputs and DIPs on that MCU's port D.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Saveable)]
#[repr(u8)]
pub enum MappyVariant {
    Mappy = 0,
    DigDug2 = 1,
}

impl MappyVariant {
    /// Where the program ROM window starts (it always runs to 0xFFFF).
    fn rom_base(self) -> u16 {
        match self {
            MappyVariant::Mappy => 0xA000,
            MappyVariant::DigDug2 => 0x8000,
        }
    }
}

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum Region {
    VideoRam = 1,
    WorkRam = 2,
    /// The 1 KB both CPUs share. The sound CPU's view at 0x0000 is translated
    /// onto this backing rather than mirrored, so the main CPU never sees it
    /// at 0x0000.
    SoundRam = 3,
    MainRom = 4,
    /// The sound CPU's ROM, at a translated address the main CPU never
    /// decodes (0x6000); see `build_map`.
    SubRom = 5,
}

// Input button IDs. The sticks are 2-way: only left and right exist.
pub const INPUT_COIN1: u8 = 0;
pub const INPUT_COIN2: u8 = 1;
pub const INPUT_SERVICE: u8 = 2;
pub const INPUT_P1_START: u8 = 3;
pub const INPUT_P2_START: u8 = 4;
pub const INPUT_P1_RIGHT: u8 = 5;
pub const INPUT_P1_LEFT: u8 = 6;
pub const INPUT_P1_BUTTON: u8 = 7;
pub const INPUT_P2_RIGHT: u8 = 8;
pub const INPUT_P2_LEFT: u8 = 9;
pub const INPUT_P2_BUTTON: u8 = 10;

// ---------------------------------------------------------------------------
// GFX layouts. `decode_gfx` numbers plane 0 as the LSB while MAME numbers it
// as the MSB (`gfx_element::decode`), so these carry MAME's bit offsets in
// reverse and decode bit-identical pixels.
// ---------------------------------------------------------------------------

/// 8x8 2bpp chars, 256 codes. Pixels 0-3 of a row are in byte `y + 8` and
/// pixels 4-7 in byte `y`, which is Pac-Man's tile layout exactly. The
/// loader complements the ROM first: the tile bus is inverted (MAME
/// ROMREGION_INVERT on the tiles region).
pub static MAPPY_CHAR_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[4, 0],
    x_offsets: &[64, 65, 66, 67, 0, 1, 2, 3],
    y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
    char_increment: 128,
};

/// 16x16 4bpp sprites, 128 codes: four groups of four pixels 128 bits
/// apart, rows 16 bits apart, and the lower eight rows 512 bits on. Decoded
/// over the de-interleaved 16K (even bytes from 3M, odd bytes from 3N).
pub static MAPPY_SPRITE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[12, 8, 4, 0],
    x_offsets: &[
        0, 1, 2, 3, 128, 129, 130, 131, 256, 257, 258, 259, 384, 385, 386, 387,
    ],
    y_offsets: &[
        0, 16, 32, 48, 64, 80, 96, 112, 512, 528, 544, 560, 576, 592, 608, 624,
    ],
    char_increment: 1024,
};

// ---------------------------------------------------------------------------
// Video
// ---------------------------------------------------------------------------

/// The palette PROM's three ladders on the video board (sheet 8-7): red
/// R7-R9 and green R10-R12 through 1K/470/220, blue R13-R14 through 470/220,
/// PROM bits 0-2 red, 3-5 green, 6-7 blue. The PROM drives them directly.
const DAC_RG: [f64; 3] = [1000.0, 470.0, 220.0];
const DAC_B: [f64; 2] = [470.0, 220.0];

/// One ladder's per-tap weights, as fractions of full drive: a tap's
/// conductance over the whole ladder's. There is no on-board load on any
/// color node between the ladders and J2, so unlike Super Pac-Man's 2.2K
/// there is no load term here and every gun reaches 255.
fn dac_weights(ladder: &[f64]) -> Vec<f64> {
    let total: f64 = ladder.iter().map(|r| 1.0 / r).sum::<f64>();
    ladder.iter().map(|r| (1.0 / r) / total).collect()
}

/// Build the 32-entry palette, with one scale shared by all three guns so
/// the hottest ladder spans 0-255. With no load term every gun's full drive
/// is the same fraction, so all three reach 255.
fn mappy_palette(prom: &[u8]) -> [(u8, u8, u8); 32] {
    use phosphor_core::gfx::combine_weights;
    let (rg, b) = (dac_weights(&DAC_RG), dac_weights(&DAC_B));
    let scale = 255.0 / rg.iter().sum::<f64>().max(b.iter().sum::<f64>());
    let rg: Vec<f64> = rg.iter().map(|w| w * scale).collect();
    let b: Vec<f64> = b.iter().map(|w| w * scale).collect();
    let mut out = [(0u8, 0u8, 0u8); 32];
    for (i, entry) in out.iter_mut().enumerate() {
        let v = prom.get(i).copied().unwrap_or(0);
        let bit = |n: u8| (v >> n) & 1;
        *entry = (
            combine_weights(&rg, &[bit(0), bit(1), bit(2)]),
            combine_weights(&rg, &[bit(3), bit(4), bit(5)]),
            combine_weights(&b, &[bit(6), bit(7)]),
        );
    }
    out
}

/// What one sprite pixel resolved to: its palette index, which is what both
/// the transparency and the priority tests are made on.
#[derive(Clone, Copy)]
struct SpritePixel {
    pen: u8,
}

/// Video state: decoded graphics, the PROM lookups, and the native RGB
/// framebuffer, composited one row at a time at the start of each scanline.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct MappyVideo {
    /// 8x8 chars, 256 codes. ROM-derived.
    #[save_skip]
    chars: GfxCache,
    /// 16x16 sprites, up to 256 codes (128 on Mappy, 256 on Dig Dug II).
    /// ROM-derived.
    #[save_skip]
    sprites: GfxCache,
    /// 32 palette entries. ROM-derived.
    #[save_skip]
    palette: [(u8, u8, u8); 32],
    /// Char lookup (4C): color * 4 + pixel -> palette index (upper 16).
    #[save_skip]
    char_lut: [u8; 256],
    /// Sprite lookup (5K): color * 16 + pixel -> palette index (lower 16).
    #[save_skip]
    sprite_lut: [u8; 256],
    /// The two lookups resolved to RGB, for the debugger's graphics sheets.
    #[save_skip]
    char_pens: [(u8, u8, u8); 256],
    #[save_skip]
    sprite_pens: [(u8, u8, u8); 256],
    /// RGB24, rebuilt row by row every frame.
    #[save_skip]
    framebuffer: Vec<u8>,
    /// One row of sprite pixels, rebuilt per row.
    #[save_skip]
    sprite_line: Vec<Option<SpritePixel>>,
    /// The scroll register: a vertical pixel offset for columns 2-33.
    #[save(id = 1)]
    scroll: u8,
    #[save(id = 2)]
    flip: bool,
}

impl MappyVideo {
    pub fn new() -> Self {
        Self {
            chars: GfxCache::new(256, 8, 8),
            sprites: GfxCache::new(256, 16, 16),
            palette: [(0, 0, 0); 32],
            char_lut: [0; 256],
            sprite_lut: [0; 256],
            char_pens: [(0, 0, 0); 256],
            sprite_pens: [(0, 0, 0); 256],
            framebuffer: vec![0; NATIVE_WIDTH * NATIVE_HEIGHT * 3],
            sprite_line: vec![None; NATIVE_WIDTH],
            scroll: 0,
            flip: false,
        }
    }

    /// Load the tile ROM, complementing each byte: the tile bus is inverted
    /// between the 2732 and the shifter (MAME's ROMREGION_INVERT).
    pub fn load_tile_rom(&mut self, data: &[u8]) {
        let inverted: Vec<u8> = data.iter().map(|b| !b).collect();
        self.chars = decode_gfx(&inverted, 0, 256, &MAPPY_CHAR_LAYOUT);
    }

    /// Load the sprite ROMs: two chips back-to-back (8K each on Mappy, 16K
    /// on Dig Dug II), de-interleaved even/odd into the image the layout
    /// decodes (MAME ROM_LOAD16_BYTE: even bytes from 3M, odd bytes from
    /// 3N). The code count comes from the ROM size: 128 or 256.
    pub fn load_sprite_rom(&mut self, data: &[u8]) {
        let half = data.len() / 2;
        let mut image = vec![0u8; data.len()];
        for i in 0..half {
            image[2 * i] = data.get(i).copied().unwrap_or(0);
            image[2 * i + 1] = data.get(half + i).copied().unwrap_or(0);
        }
        let codes = (data.len() / 128).min(256);
        self.sprites = decode_gfx(&image, 0, codes, &MAPPY_SPRITE_LAYOUT);
    }

    /// Load the PROMs: 32 bytes of palette (5B), then the 256-entry char
    /// lookup (4C) and the 256-entry sprite lookup (5K).
    ///
    /// Chars reach the upper sixteen palette entries through their lookup
    /// nibble as stored, and sprites the lower sixteen the same way. Both
    /// mappings are made by the PAL at 5D (MPI-4), which takes the lookup
    /// outputs and the sprite pixel and drives the palette PROM's A0-A4;
    /// its equations are not on the drawing, so the mapping is inferred
    /// from what the program draws. Note the char side has no complement:
    /// Super Pac-Man's SPV-5 XORs 0x0F and this PAL does not.
    pub fn load_proms(&mut self, data: &[u8]) {
        self.palette = mappy_palette(data.get(..32).unwrap_or(&[]));
        for i in 0..256 {
            let c = data.get(32 + i).copied().unwrap_or(0) & 0x0F;
            self.char_lut[i] = c | 0x10;
            self.sprite_lut[i] = data.get(288 + i).copied().unwrap_or(0) & 0x0F;
            self.char_pens[i] = self.palette[self.char_lut[i] as usize];
            self.sprite_pens[i] = self.palette[self.sprite_lut[i] as usize];
        }
    }

    pub fn set_scroll(&mut self, scroll: u8) {
        self.scroll = scroll;
    }

    pub fn set_flip(&mut self, flip: bool) {
        self.flip = flip;
    }

    pub fn reset(&mut self) {
        self.scroll = 0;
        self.flip = false;
    }

    /// Composite native row `row` (0-223).
    ///
    /// Three layers:
    ///
    /// 1. every tile, opaque, columns 2-33 scrolled;
    /// 2. sprites, over everything, a pixel transparent when its lookup
    ///    nibble is 15 (MAME's transpen mask with transcolor 15);
    /// 3. tiles whose attribute bit 6 is set, again, over the sprites.
    ///
    /// The last two are the MPI-4 PAL's and are inferred from the program's
    /// behavior: MAME redraws the high category opaquely, with no pen
    /// exception (contrast Super Pac-Man's SPV-5, whose nibble-0 high tiles
    /// let sprites through).
    pub fn render_scanline(&mut self, row: usize, videoram: &[u8], workram: &[u8]) {
        self.draw_sprite_line(row, workram);

        let chars = &self.chars;
        let char_lut = &self.char_lut;
        let palette = &self.palette;
        let sprite_line = &self.sprite_line;
        let scroll = self.scroll;
        let out = &mut self.framebuffer[row * NATIVE_WIDTH * 3..][..NATIVE_WIDTH * 3];

        for col in 0..NATIVE_WIDTH / 8 {
            let (offset, line) = tile_pixel(col, row, scroll);
            let code = videoram[offset] as usize;
            let attr = videoram[offset + 0x800];
            let color = (attr & 0x3F) as usize;
            let high = attr & 0x40 != 0;
            let pixels = chars.row_slice(code, line);
            for (dx, &pix) in pixels.iter().enumerate() {
                let x = col * 8 + dx;
                let tile_pen = char_lut[color * 4 + pix as usize];
                let pen = match sprite_line[x] {
                    None => tile_pen,
                    Some(_) if high => tile_pen,
                    Some(s) => s.pen,
                };
                let (r, g, b) = palette[pen as usize];
                out[x * 3..x * 3 + 3].copy_from_slice(&[r, g, b]);
            }
        }
    }

    /// Resolve the sprites crossing native row `row` into the line buffer.
    ///
    /// 64 slots, two bytes apart in three banks: code and color at 0x1780,
    /// Y and X at 0x1f80, and attributes at 0x2780 (flip X/Y, double width
    /// and height, X bit 8, disable). Later slots draw over earlier ones.
    ///
    /// The position offsets (`- 40` in X; `256 - y`, `- 32` in Y) are inside
    /// the 04XX and 12XX customs and are inferred from where the program puts
    /// things. The `+ 1` in `top` is the one-line delay of the line buffers
    /// (3E, 4E, banked on the vertical count), folded in here; see
    /// `docs/schematics/sprite-list-scan.md` before adding it again.
    ///
    /// Under flip the tilemap needs nothing (the frontend mirrors the whole
    /// frame), but a sprite's position does not mirror on this board: the
    /// program writes mirrored coordinates itself and only the image flips. So
    /// the row and the columns are mirrored here to cancel the frontend's.
    fn draw_sprite_line(&mut self, row: usize, workram: &[u8]) {
        // workram starts at CPU 0x1000.
        const CODE: usize = 0x1780 - 0x1000;
        const POS: usize = 0x1F80 - 0x1000;
        const ATTR: usize = 0x2780 - 0x1000;
        const GFX_OFFS: [[usize; 2]; 2] = [[0, 1], [2, 3]];

        self.sprite_line.fill(None);
        let flip = self.flip;
        let v = if flip { NATIVE_HEIGHT - 1 - row } else { row } as i32;

        for offs in (0..0x80).step_by(2) {
            let attr1 = workram[ATTR + offs + 1];
            if attr1 & 0x02 != 0 {
                continue;
            }
            let attr0 = workram[ATTR + offs];
            let size_y = ((attr0 >> 3) & 1) as i32;
            let mut top = 256 - workram[POS + offs] as i32 + 1;
            top -= 16 * size_y;
            top = (top & 0xFF) - 32;
            let height = 16 * (size_y + 1);
            if v < top || v >= top + height {
                continue;
            }

            let size_x = ((attr0 >> 2) & 1) as usize;
            let mut flip_x = attr0 & 1 != 0;
            let mut flip_y = attr0 & 2 != 0;
            if flip {
                flip_x = !flip_x;
                flip_y = !flip_y;
            }
            let code = workram[CODE + offs] as usize & !size_x & !((size_y as usize) << 1);
            // Sixteen colors: the 256-entry sprite LUT is color * 16 + pixel.
            // The program keeps the color byte's high bits clear; masking
            // keeps a stray one inside the table.
            let color = (workram[CODE + offs + 1] & 0x0F) as usize;
            let sx = workram[POS + offs + 1] as i32 + 0x100 * (attr1 & 1) as i32 - 40;

            let dy = (v - top) as usize;
            let cell_y = dy / 16;
            let mut py = dy % 16;
            if flip_y {
                py = 15 - py;
            }
            let gfx_y = cell_y ^ (size_y as usize * flip_y as usize);

            for cell_x in 0..=size_x {
                let gfx_x = cell_x ^ (size_x * flip_x as usize);
                let tile = (code + GFX_OFFS[gfx_y][gfx_x]) % self.sprites.count().max(1);
                let pixels = self.sprites.row_slice(tile, py);
                for px in 0..16 {
                    let u = sx + (cell_x * 16 + px) as i32;
                    if !(0..NATIVE_WIDTH as i32).contains(&u) {
                        continue;
                    }
                    let src = if flip_x { 15 - px } else { px };
                    let pen = self.sprite_lut[color * 16 + pixels[src] as usize];
                    // Transparent is lookup nibble 15, whatever pixel maps to
                    // it: `transpen_mask` takes a transcolor, not a pen index.
                    // (Mappy's low colors map pen 15 to nibble 15, which is why
                    // matching pen 15's nibble looked right there and broke on
                    // Dig Dug II, whose LUT mostly maps it to 0.)
                    if pen == 0x0F {
                        continue;
                    }
                    let x = if flip {
                        NATIVE_WIDTH - 1 - u as usize
                    } else {
                        u as usize
                    };
                    self.sprite_line[x] = Some(SpritePixel { pen });
                }
            }
        }
    }

    pub fn render_frame(&self, out: &mut [u8]) {
        out.copy_from_slice(&self.framebuffer);
    }

    /// The tube is mounted rotated 90 degrees clockwise; the cocktail
    /// flip composes a further 180.
    pub fn orientation(&self) -> Orientation {
        let mut o = Orientation::ROT90;
        if self.flip {
            o = o.compose(Orientation::COCKTAIL);
        }
        o
    }

    pub fn tile_cache(&self) -> &GfxCache {
        &self.chars
    }

    pub fn sprite_cache(&self) -> &GfxCache {
        &self.sprites
    }

    /// Char pens in lookup order, for the debugger's graphics sheet.
    pub fn char_pens(&self) -> &[(u8, u8, u8)] {
        &self.char_pens
    }

    /// Sprite pens in lookup order, for the debugger's graphics sheet.
    pub fn sprite_pens(&self) -> &[(u8, u8, u8)] {
        &self.sprite_pens
    }
}

impl Default for MappyVideo {
    fn default() -> Self {
        Self::new()
    }
}

/// The tilemap address and within-tile line for one screen column and native
/// row: the playfield is 36 columns by 60 rows, columns 2-33 scrolled by
/// `scroll` pixels with wraparound, columns 0-1 and 34-35 fixed side strips
/// addressed out of 0x780-0x7FF with a +2 row fold.
///
/// This is MAME's `mappy_tilemap_scan` with the per-column scroll folded in:
/// screen row `row` shows playfield row `(row + scroll) / 8` (mod 60). The
/// +2/0x0F fold in the side strips is true to the hardware; laying the
/// strips out linearly drops tiles in Motos and Tower of Druaga.
fn tile_pixel(col: usize, row: usize, scroll: u8) -> (usize, usize) {
    let c = col as i32 - 2;
    if c & 0x20 != 0 {
        // The strip fold addresses tile rows, not pixel rows: every pixel row
        // of one tile row reads the same tile.
        let tr = row / 8;
        if tr & 0x20 != 0 {
            return (0x7FF, row % 8); // outside the visible area
        }
        let offset = ((tr + 2) & 0x0F) + (tr & 0x10) + (((c & 3) << 5) as usize) + 0x780;
        (offset, row % 8)
    } else {
        let y = (row + scroll as usize) % (60 * 8);
        (c as usize + (y / 8) * 32, y % 8)
    }
}

// ---------------------------------------------------------------------------
// Audio: the 99XX stand-in, the coupling, and nothing else
// ---------------------------------------------------------------------------

/// The 99XX's voice mixing, assumed linear: each voice contributes its
/// signed sample times its volume, over the eight voices' combined full
/// swing. The mixing law is inside the 99XX custom and is NOT FOUND (see
/// `docs/schematics/mappy-board.md` section 8.2); this sum is the
/// documented stand-in, not a schematic value. MAME's `namco_15xx` device
/// stands in the same place for the same reason.
struct LinearMix;

impl CustomComponent for LinearMix {
    fn reset(&mut self) {}

    fn step(&mut self, inputs: &[f64], _dt: f64) -> f64 {
        const FULL_SWING: f64 = (VOICES * 8 * 15) as f64;
        let (samples, volumes) = inputs.split_at(VOICES);
        let sum: f64 = samples.iter().zip(volumes.iter()).map(|(s, v)| s * v).sum();
        sum / FULL_SWING
    }

    fn save_state(&self, _w: &mut StateWriter) {}

    fn load_state(&mut self, _r: &mut StateReader) -> Result<(), SaveError> {
        Ok(())
    }
}

/// Mappy's audio stage: the assumed-linear 99XX mix through C29 into a flat
/// amp. C29 (2.2MF) sees about 10K of source (R30 with VR1 at maximum), one
/// high-pass pole near 7 Hz; the R30/VR1 division and the LA4460's gain are
/// flat in band and normalize out. The Zobels stabilize the amp above the
/// audio band and are not modeled.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct MappyAudio {
    #[save(id = 1)]
    circuit: DiscreteCircuit,
    /// Input handles, fixed when the circuit is built.
    #[save_skip]
    sample: [DataInputId; VOICES],
    #[save_skip]
    volume: [DataInputId; VOICES],
}

impl MappyAudio {
    pub fn new(board_clock_hz: u64) -> Self {
        let mut b = DiscreteCircuitBuilder::new(board_clock_hz, host_sample_rate() as u64);
        let mut sample = Vec::with_capacity(VOICES);
        let mut volume = Vec::with_capacity(VOICES);
        for v in 0..VOICES {
            sample.push(b.data_input(&format!("SAMPLE{v}"), 1.0));
            volume.push(b.data_input(&format!("VOL{v}"), 1.0));
        }
        let mut inputs = sample.clone();
        inputs.extend(volume.iter().copied());
        let nodes: Vec<NodeId> = inputs.into_iter().map(NodeId::from).collect();
        let mixed = b.custom("MIX", nodes, Box::new(LinearMix));
        let coupled = b.rc_high_pass("COUPLING", mixed, 10_000.0, 2.2e-6);
        // The mixer already normalizes to the board's own full swing.
        b.output(coupled, OutputGain::unity());
        let circuit = b.build();
        Self {
            circuit,
            sample: sample.try_into().expect("eight voices"),
            volume: volume.try_into().expect("eight voices"),
        }
    }

    /// Latch one board cycle's worth of voice codes and advance the stage.
    ///
    /// `voices` is [`Namco15xx::voices`]'s result: each voice's signed
    /// waveform sample (-8..+7, code 8 being zero) and its volume code.
    /// Every code is pushed every cycle rather than only on a change (see
    /// `WsgOutputStage::tick` for why a cache would break save states).
    pub fn tick(&mut self, voices: [(i32, u8); VOICES]) {
        for (v, &(s, vol)) in voices.iter().enumerate() {
            self.circuit.set_data(self.sample[v], s as f64);
            self.circuit.set_data(self.volume[v], vol as f64);
        }
        self.circuit.tick(1);
    }

    /// Drain produced mono `i16` samples. Returns the number written.
    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize {
        self.circuit.fill_audio(out)
    }

    pub fn reset(&mut self) {
        self.circuit.reset();
    }
}

// ---------------------------------------------------------------------------
// The drive
// ---------------------------------------------------------------------------

/// The two CPUs and the board as disjoint borrows, so a cycle dispatches at a
/// [`Bus`] without reborrowing through the system. Stays minimal on purpose:
/// adding a field here means threading it through every machine's drive and
/// the debugger's tick path, so a board-local concern belongs on the board's
/// concrete type.
struct Drive<'a> {
    main: &'a mut M6809,
    sub: &'a mut M6809,
    board: &'a mut MappyBoard,
}

impl ScanlineDriven for Drive<'_> {
    const TIMING: TimingConfig = TIMING;

    fn clock(&mut self) -> u64 {
        self.board.clock
    }

    fn begin_scanline(&mut self, scanline: u64) {
        self.board.begin_scanline(scanline);
    }

    #[inline]
    fn step_cycle(&mut self) {
        step_cycle(self.main, self.sub, self.board);
    }
}

/// One CPU cycle: the sound CPU unless the latch holds it in reset, then the
/// main CPU, then the 15XX and the clock.
///
/// Both 6809Es run off the 07XX's 2H, and the sound CPU is the one that leads:
/// the main CPU's E is /2H and its Q is a 74LS74 (2A) re-timing of the CPU
/// clock, and that Q is the sound CPU's E. So in each cycle the sound CPU's
/// bus access falls a quarter cycle before the main CPU's, which is the order
/// they are stepped in here. The shared RAM is time-sliced on 1H and 2H, so
/// neither CPU ever waits for the other and that order is the whole of the
/// arbitration.
#[inline]
fn step_cycle(main: &mut M6809, sub: &mut M6809, board: &mut MappyBoard) {
    // A SUB RESET written by the main CPU last cycle takes hold here, before
    // the sound CPU's next cycle.
    if board.pending_sub_reset {
        board.pending_sub_reset = false;
        sub.reset(board, BusMaster::Cpu(1));
        board.sub_irq_pending = false;
    }
    if board.latch & LATCH_SUB_RUN != 0 {
        if board.map.debug_active() {
            let pc = sub.at_instruction_boundary().then_some(u32::from(sub.pc));
            board.map.latch_access_context(board.clock, pc);
        }
        sub.execute_cycle(board, BusMaster::Cpu(1));
    }

    if board.map.debug_active() {
        let pc = main.at_instruction_boundary().then_some(u32::from(main.pc));
        board.map.latch_access_context(board.clock, pc);
    }
    main.execute_cycle(board, BusMaster::Cpu(0));

    board.end_cycle();
}

/// One cycle, testing the frame position first: the debugger's path.
pub fn tick(main: &mut M6809, sub: &mut M6809, board: &mut MappyBoard) {
    Drive { main, sub, board }.tick();
}

/// Run one frame.
pub fn run_frame(main: &mut M6809, sub: &mut M6809, board: &mut MappyBoard) {
    Drive { main, sub, board }.run_frame();
}

// ---------------------------------------------------------------------------
// The board
// ---------------------------------------------------------------------------

#[derive(BusDebug, DebugTrace, Saveable)]
#[save_version(2)]
#[save_tlv]
pub struct MappyBoard {
    /// Which game runs on this board. Version 2 added it with the Dig Dug II
    /// bring-up, which also fitted io1 as an Option: a version 1 save fails
    /// loudly here rather than misreading either.
    #[save(id = 20)]
    pub(crate) variant: MappyVariant,
    /// Video, work and sound RAM, and both CPUs' ROMs. Both CPUs share it;
    /// the sound CPU's regions sit at translated addresses (see `build_map`).
    #[debug_map(cpu = 0)]
    #[save(id = 1)]
    pub(crate) map: AddressSpace16,

    #[save(id = 2)]
    pub(crate) video: MappyVideo,

    #[debug_device("Namco 15XX")]
    #[save(id = 3)]
    pub(crate) wsg: Namco15xx,

    #[save(id = 4)]
    /// The assumed-linear 99XX mix and the C29 coupling. See [`MappyAudio`].
    pub(crate) audio: MappyAudio,

    #[debug_device("58XX #0")]
    #[save(id = 5)]
    pub(crate) io0: Namco58,
    /// The DIP MCU at 0x4810: a 58XX on Mappy, a 56XX on Dig Dug II. Exactly
    /// one is fitted; an Option field is on the wire exactly when it is.
    #[save(id = 6)]
    pub(crate) io1_58: Option<Namco58>,
    #[save(id = 22)]
    pub(crate) io1_56: Option<Namco56>,

    // Cabinet switches, active-low nibbles as they sit on the 58XX pins.
    /// 58XX #0 port A: coin 1, coin 2, unused, service coin.
    #[save(id = 7)]
    pub(crate) in_coins: u8,
    /// 58XX #0 port B: P1 right and left on bits 1 and 3 (2-way stick).
    #[save(id = 8)]
    pub(crate) in_p1: u8,
    /// 58XX #0 port C: P2 right and left on bits 1 and 3.
    #[save(id = 9)]
    pub(crate) in_p2: u8,
    /// 58XX #0 port D: P1 button, P2 button, start 1, start 2.
    #[save(id = 10)]
    pub(crate) in_buttons: u8,
    /// Dig Dug II second buttons on MCU #1 port D bits 0-1: P1 drill, P2
    /// drill. Unused on Mappy.
    #[save(id = 21)]
    pub(crate) in_buttons2: u8,
    /// Dig Dug II service-mode button on MCU #1 port D bit 3. Unused on
    /// Mappy (its service mode is a DIP).
    #[save(id = 23)]
    pub(crate) in_service_mode: bool,

    /// DIP bank SW2 at CPU 5B (58XX #1 ports B and C).
    #[save(id = 11)]
    pub(crate) dsw1: u8,
    /// DIP bank SW3 at CPU 5E (58XX #1 port A, through the mux).
    #[save(id = 12)]
    pub(crate) dsw2: u8,
    /// Cabinet and service mode (MCU #1 port D). On Dig Dug II only bit 2
    /// is a DIP; bits 0-1 and 3 are the live buttons above.
    #[save(id = 13)]
    pub(crate) dsw0: u8,

    /// LS259 (2M) output byte.
    #[save(id = 14)]
    pub(crate) latch: u8,
    #[save(id = 15)]
    pub(crate) main_irq_pending: bool,
    #[save(id = 16)]
    pub(crate) sub_irq_pending: bool,
    /// SUB RESET went high this cycle: the sound CPU runs its reset sequence
    /// before its next cycle.
    #[save(id = 17)]
    pending_sub_reset: bool,

    #[save(id = 18)]
    pub(crate) clock: u64,
    #[debug_device("Clocks")]
    #[save(id = 19)]
    clocks: ClockTree,
    #[save_skip]
    wsg_dom: DomainId,

    #[debug_events]
    #[save_skip]
    pub(crate) debug_trace: DebugTraceBuffer,
}

/// The fitted DIP MCU at 0x4810, reborrowed for one call. Both chips share
/// the same call shapes (`read`, `write`, `set_reset`, `run`, `reset` over
/// [`InPort`]), so this transient enum is the only dispatch the Option pair
/// needs. It is runtime-only: the fields stay concrete for the save derive.
enum Io1<'a> {
    M58(&'a mut Namco58),
    M56(&'a mut Namco56),
}

impl Io1<'_> {
    fn read(&self, addr: u16) -> u8 {
        match self {
            Io1::M58(io) => io.read(addr),
            Io1::M56(io) => io.read(addr),
        }
    }

    fn write(&mut self, addr: u16, data: u8) {
        match self {
            Io1::M58(io) => io.write(addr, data),
            Io1::M56(io) => io.write(addr, data),
        }
    }

    fn set_reset(&mut self, asserted: bool) {
        match self {
            Io1::M58(io) => io.set_reset(asserted),
            Io1::M56(io) => io.set_reset(asserted),
        }
    }

    fn run<F: FnMut(InPort, u8) -> u8>(&mut self, read: F) {
        match self {
            Io1::M58(io) => io.run(read),
            Io1::M56(io) => io.run(read),
        }
    }

    fn reset(&mut self) {
        match self {
            Io1::M58(io) => io.reset(),
            Io1::M56(io) => io.reset(),
        }
    }
}

impl MappyBoard {
    /// The fitted DIP MCU. Exactly one is Some; the constructor fits the
    /// variant's and there is no path that fits both or neither.
    fn io1(&mut self) -> Io1<'_> {
        match (&mut self.io1_58, &mut self.io1_56) {
            (Some(io), None) => Io1::M58(io),
            (None, Some(io)) => Io1::M56(io),
            _ => unreachable!("exactly one DIP MCU is fitted"),
        }
    }

    pub fn new(variant: MappyVariant) -> Self {
        let clocks = clock_tree();
        let wsg_dom = clocks.find(Clk::Psg).expect("declared 15XX domain");
        let (io1_58, io1_56) = match variant {
            MappyVariant::Mappy => (Some(Namco58::new()), None),
            MappyVariant::DigDug2 => (None, Some(Namco56::new())),
        };
        Self {
            variant,
            map: Self::build_map(variant),
            video: MappyVideo::new(),
            wsg: Namco15xx::new(),
            audio: MappyAudio::new(TIMING.cpu_clock_hz),
            io0: Namco58::new(),
            io1_58,
            io1_56,
            in_coins: 0x0F,
            in_p1: 0x0F,
            in_p2: 0x0F,
            in_buttons: 0x0F,
            in_buttons2: 0x0F,
            in_service_mode: false,
            dsw1: DEFAULT_DSW1,
            dsw2: DEFAULT_DSW2,
            dsw0: DEFAULT_DSW0,
            latch: 0,
            main_irq_pending: false,
            sub_irq_pending: false,
            pending_sub_reset: false,
            clock: 0,
            clocks,
            wsg_dom,
            debug_trace: DebugTraceBuffer::new(),
        }
    }

    /// One map for both CPUs. The sound CPU's RAM is the main CPU's sound RAM
    /// at 0x4000, and its ROM is translated to 0x6000, which nothing on the
    /// main CPU's side decodes. The program ROM window starts at 0xA000 on
    /// Mappy and 0x8000 on Dig Dug II.
    fn build_map(variant: MappyVariant) -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        let rom_base = variant.rom_base();
        map.region(
            Region::VideoRam,
            "Video RAM (codes 0x0000, attrs 0x0800)",
            0x0000,
            0x1000,
            AccessKind::ReadWrite,
        )
        .region(
            Region::WorkRam,
            "Work RAM (sprite registers at 0x1780/0x1F80/0x2780)",
            0x1000,
            0x1800,
            AccessKind::ReadWrite,
        )
        .region(
            Region::SoundRam,
            "Sound RAM (main 0x4000 = sound 0x0000)",
            0x4000,
            0x0400,
            AccessKind::ReadWrite,
        )
        .region(
            Region::SubRom,
            "Sound ROM (sound 0xE000)",
            0x6000,
            0x2000,
            AccessKind::ReadOnly,
        )
        .region(
            Region::MainRom,
            "Program ROM",
            rom_base,
            0x1_0000 - u32::from(rom_base),
            AccessKind::ReadOnly,
        );
        map
    }

    pub fn load_program_rom(&mut self, data: &[u8]) {
        self.map.load_region(Region::MainRom, data);
    }
    pub fn load_sub_rom(&mut self, data: &[u8]) {
        self.map.load_region(Region::SubRom, data);
    }
    pub fn load_tile_rom(&mut self, data: &[u8]) {
        self.video.load_tile_rom(data);
    }
    pub fn load_sprite_rom(&mut self, data: &[u8]) {
        self.video.load_sprite_rom(data);
    }
    pub fn load_proms(&mut self, data: &[u8]) {
        self.video.load_proms(data);
    }
    pub fn load_sound_prom(&mut self, data: &[u8]) {
        self.wsg.load_waveform_rom(data);
    }

    pub fn clock(&self) -> u64 {
        self.clock
    }

    // -----------------------------------------------------------------------
    // Timing
    // -----------------------------------------------------------------------

    /// The scanline boundary: composite the row, raise VBLANK's IRQs, and run
    /// the 58XXs one line later.
    fn begin_scanline(&mut self, scanline: u64) {
        if scanline < NATIVE_HEIGHT as u64 {
            let videoram = self.map.region_data(Region::VideoRam);
            let workram = self.map.region_data(Region::WorkRam);
            self.video
                .render_scanline(scanline as usize, videoram, workram);
        }
        if scanline == VBLANK_LINE {
            if self.latch & LATCH_MAIN_INT_ON != 0 {
                self.main_irq_pending = true;
            }
            if self.latch & LATCH_SUB_INT_ON != 0 {
                self.sub_irq_pending = true;
            }
        }
        if scanline == IO_RUN_LINE {
            self.run_io();
        }
    }

    /// The board half of a cycle after the CPUs: the 15XX on its own clock,
    /// the output stage on every cycle, then the clock.
    fn end_cycle(&mut self) {
        if self.clocks.tick(self.wsg_dom) {
            self.wsg.tick();
        }
        self.audio.tick(self.wsg.voices());
        self.clock += 1;
    }

    /// Run both 58XXs once.
    ///
    /// The drawing ties VBLANK to each chip's interrupt, so they act once a
    /// frame on VBLANK. How long each takes to respond is its undumped
    /// program's business and not on any drawing. Here they run on the
    /// scanline boundary after VBLANK starts, 62.5 us in, the nearest point
    /// the scanline drive already visits (MAME uses a 50 us timer for the
    /// same wait). **That latency is a stand-in for the MCU's, not a part on
    /// the board.**
    pub(crate) fn run_io(&mut self) {
        let (coins, p1, p2, buttons) = (self.in_coins, self.in_p1, self.in_p2, self.in_buttons);
        self.io0.run(|port, _| match port {
            InPort::A => coins,
            InPort::B => p1,
            InPort::C => p2,
            InPort::D => buttons,
        });
        let (dsw0, dsw1, dsw2) = (self.dsw0, self.dsw1, self.dsw2);
        // Port D is all DIPs on Mappy; on Dig Dug II only bit 2 is, with
        // live buttons on bits 0-1 (second buttons) and 3 (service mode).
        let port_d = match self.variant {
            MappyVariant::Mappy => dsw0 & 0x0F,
            MappyVariant::DigDug2 => {
                (self.in_buttons2 & 0x03) | (dsw0 & 0x04) | (u8::from(!self.in_service_mode) << 3)
            }
        };
        self.io1().run(|port, out_a| match port {
            // The mux on port A: select low reads SW3's low four switches,
            // high its high four.
            InPort::A => {
                if out_a & 1 == 0 {
                    dsw2 & 0x0F
                } else {
                    dsw2 >> 4
                }
            }
            InPort::B => dsw1 & 0x0F,
            InPort::C => dsw1 >> 4,
            InPort::D => port_d,
        });
    }

    pub fn interrupt_state(&self, target: BusMaster) -> InterruptState {
        let mut state = InterruptState::default();
        match target {
            BusMaster::Cpu(0) => state.irq = self.main_irq_pending,
            BusMaster::Cpu(1) => state.irq = self.sub_irq_pending,
            _ => {}
        }
        state
    }

    pub fn render_frame(&self, buffer: &mut [u8]) {
        self.video.render_frame(buffer);
    }

    pub fn orientation(&self) -> Orientation {
        self.video.orientation()
    }

    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize {
        self.audio.fill_audio(out)
    }

    pub fn reset_board(&mut self) {
        self.video.reset();
        self.wsg.reset();
        self.audio.reset();
        self.io0.reset();
        self.io1().reset();
        self.latch = 0;
        self.main_irq_pending = false;
        self.sub_irq_pending = false;
        self.pending_sub_reset = false;
        self.clock = 0;
        self.clocks.reset();
        self.map.region_data_mut(Region::VideoRam).fill(0);
        self.map.region_data_mut(Region::WorkRam).fill(0);
        self.map.region_data_mut(Region::SoundRam).fill(0);
    }

    /// Which CPUs are between instructions: the main CPU in bit 0, the sound
    /// CPU in bit 1.
    pub fn instruction_boundaries(&self, main: &M6809, sub: &M6809) -> u32 {
        u32::from(main.at_instruction_boundary()) | (u32::from(sub.at_instruction_boundary()) << 1)
    }

    // -----------------------------------------------------------------------
    // Bus dispatch
    // -----------------------------------------------------------------------

    fn main_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x27FF => self.map.read_backing(addr),
            // 0x3800-0x3FFF is the write-only scroll register.
            0x4000..=0x43FF => self.map.read_backing(addr),
            0x4800..=0x480F => self.io0.read(addr),
            0x4810..=0x481F => self.io1().read(addr),
            // The program ROM window starts at 0xA000 on Mappy (below that
            // floats high) and 0x8000 on Dig Dug II.
            0x8000..=0xFFFF => {
                if addr < 0xA000 && self.variant == MappyVariant::Mappy {
                    0xFF
                } else {
                    self.map.read_backing(addr)
                }
            }
            _ => 0xFF,
        }
    }

    fn main_write(&mut self, addr: u16, data: u8) {
        // The scroll register loads the write address, not the data byte:
        // the value is the offset into the 0x3800 page, shifted. How the
        // address lines reach the counter is not traced; MAME's
        // `mappy_scroll_w` takes `offset >> 3`.
        match addr {
            0x0000..=0x27FF => self.map.write_backing(addr, data),
            0x3800..=0x3FFF => self.video.set_scroll(((addr - 0x3800) >> 3) as u8),
            0x4000..=0x43FF => self.sound_ram_write(addr - 0x4000, data),
            0x4800..=0x480F => self.io0.write(addr, data),
            0x4810..=0x481F => self.io1().write(addr, data),
            0x5000..=0x500F => self.latch_write(addr),
            _ => {} // 0x8000 watchdog (not modeled), ROM, unmapped
        }
    }

    fn sub_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x03FF => self.map.read_backing(0x4000 + addr),
            // A full 8K ROM: 0xE000-0xFFFF reads straight through, no mirror.
            0xE000..=0xFFFF => self.map.read_backing(0x6000 + (addr - 0xE000)),
            _ => 0xFF,
        }
    }

    fn sub_write(&mut self, addr: u16, data: u8) {
        match addr {
            0x0000..=0x03FF => self.sound_ram_write(addr, data),
            0x2000..=0x200F => self.latch_write(addr),
            _ => {}
        }
    }

    /// Either CPU writing the shared sound RAM. The 15XX reads its voice
    /// registers out of the first 64 bytes, so it sees every write there.
    fn sound_ram_write(&mut self, offset: u16, data: u8) {
        self.map.write_backing(0x4000 + offset, data);
        self.wsg.write(offset, data);
    }

    /// The LS259 at 2M: A1-A3 drive its A/B/C select and A0 its D input, so
    /// the data byte is ignored. Either CPU strobes it (SPC-5 merges both into
    /// LTWR).
    fn latch_write(&mut self, addr: u16) {
        let line = (addr >> 1) & 7;
        let bit = addr & 1 != 0;
        let before = self.latch;
        if bit {
            self.latch |= 1 << line;
        } else {
            self.latch &= !(1 << line);
        }
        match line {
            0 if !bit => self.sub_irq_pending = false,
            1 if !bit => self.main_irq_pending = false,
            2 => self.video.set_flip(bit),
            3 => self.wsg.set_sound_enabled(bit),
            4 => {
                self.io0.set_reset(!bit);
                self.io1().set_reset(!bit);
            }
            5 if bit && before & LATCH_SUB_RUN == 0 => self.pending_sub_reset = true,
            _ => {}
        }
    }
}

/// The 58XXs run this many lines into the frame; see `run_io`.
const IO_RUN_LINE: u64 = VBLANK_LINE + 1;

impl Default for MappyBoard {
    fn default() -> Self {
        Self::new(MappyVariant::Mappy)
    }
}

impl Bus for MappyBoard {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let (index, data) = match master {
            BusMaster::Cpu(0) => (0, self.main_read(addr)),
            BusMaster::Cpu(1) => (1, self.sub_read(addr)),
            _ => return 0xFF,
        };
        self.map.watch_read(index, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        let index = match master {
            BusMaster::Cpu(0) => 0,
            BusMaster::Cpu(1) => 1,
            _ => return,
        };
        self.map.watch_write(index, master, addr, data);
        match master {
            BusMaster::Cpu(0) => self.main_write(addr, data),
            BusMaster::Cpu(1) => self.sub_write(addr, data),
            _ => {}
        }
    }

    fn io_read(&mut self, _master: BusMaster, _addr: u16) -> u8 {
        0xFF
    }

    fn io_write(&mut self, _master: BusMaster, _addr: u16, _data: u8) {}

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        self.interrupt_state(target)
    }
}

// ---------------------------------------------------------------------------
// ROM definitions: the `mappy` set
// ---------------------------------------------------------------------------

pub static MAPPY_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        RomEntry {
            name: "mpx_3.1d",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x52e6c708],
        },
        RomEntry {
            name: "mp1_2.1c",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0xa958a61c],
        },
        RomEntry {
            name: "mpx_1.1b",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x203766d4],
        },
    ],
};

pub static MAPPY_SOUND_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[RomEntry {
        name: "mp1_4.1k",
        size: 0x2000,
        offset: 0x0000,
        crc32: &[0x8182dd5b],
    }],
};

pub static MAPPY_TILE_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "mp1_5.3b",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0x16498b9f],
    }],
};

/// The two sprite ROMs back-to-back, de-interleaved even/odd on load.
pub static MAPPY_SPRITE_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "mp1_6.3m",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xf2d9647a],
        },
        RomEntry {
            name: "mp1_7.3n",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x757cf2b6],
        },
    ],
};

/// Palette (5B), char lookup (4C), sprite lookup (5K).
pub static MAPPY_COLOR_PROMS: RomRegion = RomRegion {
    size: 0x0220,
    entries: &[
        RomEntry {
            name: "mp1-5.5b",
            size: 0x0020,
            offset: 0x0000,
            crc32: &[0x56531268],
        },
        RomEntry {
            name: "mp1-6.4c",
            size: 0x0100,
            offset: 0x0020,
            crc32: &[0x50765082],
        },
        RomEntry {
            name: "mp1-7.5k",
            size: 0x0100,
            offset: 0x0120,
            crc32: &[0x5396bd78],
        },
    ],
};

/// The 15XX's waveforms (3M on the CPU board).
pub static MAPPY_SOUND_PROM: RomRegion = RomRegion {
    size: 0x0100,
    entries: &[RomEntry {
        name: "mp1-3.3m",
        size: 0x0100,
        offset: 0x0000,
        crc32: &[0x16a9166a],
    }],
};

// ---------------------------------------------------------------------------
// DIP switches. Values are pin levels: the 58XX inverts what it reads, so a
// switch that is ON reads 0 here.
//
// Bank names are the drawing's (sheet 8-5): SW2 at CPU 5B on 58XX #1 pins
// 22-29, switch 1 on pin 22, and SW3 at CPU 5E reaching pins 38-41 through
// the mux, switches 1-4 with the select low and 5-8 with it high. The
// manual's option pages (1-9, 1-10) call them DIP A and DIP B. The manual
// documents a 2-bit difficulty and marks SW2's switches 3-5 MUST BE OFF,
// but the program reads all three low bits plus coin B on bits 3-4 (coinage
// table at $D4D6), so those options carry the program's superset; see
// `docs/schematics/mappy-board.md` section 10.
// ---------------------------------------------------------------------------

/// Power-on DIP state, all switches OFF (pin-high). Both games share it:
// Dig Dug II's bank 2 is entirely unused and its bank 0 carries live buttons
// that default high, so 0xFF/0xFF/0x0F is still the all-off state.
pub(crate) const DEFAULT_DSW1: u8 = 0xFF;
pub(crate) const DEFAULT_DSW2: u8 = 0xFF;
pub(crate) const DEFAULT_DSW0: u8 = 0x0F;

const DSW1_OPTIONS: &[DipOption] = &[
    DipOption {
        name: "Difficulty",
        mask: 0x07,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "Rank A",
                value: 0x07,
            },
            DipChoice {
                label: "Rank B",
                value: 0x06,
            },
            DipChoice {
                label: "Rank C",
                value: 0x05,
            },
            DipChoice {
                label: "Rank D",
                value: 0x04,
            },
            DipChoice {
                label: "Rank E",
                value: 0x03,
            },
            DipChoice {
                label: "Rank F",
                value: 0x02,
            },
            DipChoice {
                label: "Rank G",
                value: 0x01,
            },
            DipChoice {
                label: "Rank H",
                value: 0x00,
            },
        ],
        conditional: &[],
    },
    DipOption {
        name: "Coin B",
        mask: 0x18,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "1C/1C",
                value: 0x18,
            },
            DipChoice {
                label: "2C/1C",
                value: 0x00,
            },
            DipChoice {
                label: "1C/5C",
                value: 0x10,
            },
            DipChoice {
                label: "1C/7C",
                value: 0x08,
            },
        ],
        conditional: &[],
    },
    DipOption {
        name: "Demo Sounds",
        mask: 0x20,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "On",
                value: 0x20,
            },
            DipChoice {
                label: "Off",
                value: 0x00,
            },
        ],
        conditional: &[],
    },
    DipOption {
        name: "Rack Test",
        mask: 0x40,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "Off",
                value: 0x40,
            },
            DipChoice {
                label: "On",
                value: 0x00,
            },
        ],
        conditional: &[],
    },
    DipOption {
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
        conditional: &[],
    },
];

/// Bonus schedules when the game starts with 5 Mappy's: the manual's second
/// column, in force while the lives bits read 0x80.
const BONUS_WITH_5_MAPPYS: &[DipChoice] = &[
    DipChoice {
        label: "30k & 80k Only",
        value: 0x38,
    },
    DipChoice {
        label: "30k & 100k Only",
        value: 0x30,
    },
    DipChoice {
        label: "30k & 120k Only",
        value: 0x28,
    },
    DipChoice {
        label: "30k Only",
        value: 0x20,
    },
    DipChoice {
        label: "40k Only",
        value: 0x18,
    },
    DipChoice {
        label: "30k, 100k & Every 100k",
        value: 0x10,
    },
    DipChoice {
        label: "40k, 120k & Every 120k",
        value: 0x08,
    },
    DipChoice {
        label: "None",
        value: 0x00,
    },
];

const DSW2_OPTIONS: &[DipOption] = &[
    DipOption {
        name: "Coin A",
        mask: 0x07,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "1C/1C",
                value: 0x07,
            },
            DipChoice {
                label: "1C/2C",
                value: 0x06,
            },
            DipChoice {
                label: "1C/3C",
                value: 0x05,
            },
            DipChoice {
                label: "1C/6C",
                value: 0x04,
            },
            DipChoice {
                label: "2C/1C",
                value: 0x03,
            },
            // The manual's 2C/3C row. The program's coinage table reads
            // (10,3) at this index; see the board doc. The label keeps the
            // manual's value and the game follows the ROM either way.
            DipChoice {
                label: "2C/3C",
                value: 0x02,
            },
            DipChoice {
                label: "3C/1C",
                value: 0x01,
            },
            DipChoice {
                label: "3C/2C",
                value: 0x00,
            },
        ],
        conditional: &[],
    },
    DipOption {
        name: "Bonus Life",
        mask: 0x38,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "20k & 70k Only",
                value: 0x38,
            },
            DipChoice {
                label: "20k & 60k Only",
                value: 0x30,
            },
            DipChoice {
                label: "20k & 80k Only",
                value: 0x28,
            },
            DipChoice {
                label: "30k & 100k Only",
                value: 0x20,
            },
            DipChoice {
                label: "20k Only",
                value: 0x18,
            },
            DipChoice {
                label: "20k, 70k & Every 70k",
                value: 0x10,
            },
            DipChoice {
                label: "20k, 80k & Every 80k",
                value: 0x08,
            },
            DipChoice {
                label: "None",
                value: 0x00,
            },
        ],
        conditional: &[DipCondition {
            mask: 0xC0,
            equals: 0x80,
            choices: BONUS_WITH_5_MAPPYS,
        }],
    },
    DipOption {
        name: "Lives",
        mask: 0xC0,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "3",
                value: 0xC0,
            },
            DipChoice {
                label: "5",
                value: 0x80,
            },
            DipChoice {
                label: "1",
                value: 0x40,
            },
            DipChoice {
                label: "2",
                value: 0x00,
            },
        ],
        conditional: &[],
    },
];

const DSW0_OPTIONS: &[DipOption] = &[
    DipOption {
        name: "Cabinet",
        mask: 0x04,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "Upright",
                value: 0x04,
            },
            DipChoice {
                label: "Cocktail",
                value: 0x00,
            },
        ],
        conditional: &[],
    },
    DipOption {
        name: "Service Mode",
        mask: 0x08,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "Off",
                value: 0x08,
            },
            DipChoice {
                label: "On",
                value: 0x00,
            },
        ],
        conditional: &[],
    },
];

pub(crate) const MAPPY_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "SW2",
        options: DSW1_OPTIONS,
    },
    DipSwitchBank {
        name: "SW3",
        options: DSW2_OPTIONS,
    },
    DipSwitchBank {
        name: "Cabinet",
        options: DSW0_OPTIONS,
    },
];

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

pub const MAPPY_CONTROLS: &[InputControl] = &[
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
        id: InputId(INPUT_P1_BUTTON as u16),
        stable_name: "p1_button",
        label: "P1 Button",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
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
        id: InputId(INPUT_P2_BUTTON as u16),
        stable_name: "p2_button",
        label: "P2 Button",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(2),
        default_bindings: &[],
    },
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
        id: InputId(INPUT_SERVICE as u16),
        stable_name: "service",
        label: "Service Coin",
        kind: InputKind::Service,
        player: None,
        default_bindings: crate::input_defaults::SERVICE,
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
];

// ---------------------------------------------------------------------------
// The machine
// ---------------------------------------------------------------------------

/// Mappy. Both CPUs sit beside the board, which is their bus.
#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct MappySystem {
    #[debug_cpu("M6809 Main")]
    #[save(id = 1)]
    pub main: M6809,
    #[debug_cpu("M6809 Sound")]
    #[save(id = 2)]
    pub sub: M6809,
    #[debug_bus]
    #[save(id = 3)]
    pub board: MappyBoard,
}

impl MappySystem {
    pub fn new() -> Self {
        Self {
            main: M6809::new(),
            sub: M6809::new(),
            board: MappyBoard::new(MappyVariant::Mappy),
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        self.board
            .load_program_rom(&MAPPY_PROGRAM_ROM.load(rom_set)?);
        self.board.load_sub_rom(&MAPPY_SOUND_ROM.load(rom_set)?);
        self.board.load_tile_rom(&MAPPY_TILE_ROM.load(rom_set)?);
        self.board.load_sprite_rom(&MAPPY_SPRITE_ROM.load(rom_set)?);
        self.board.load_proms(&MAPPY_COLOR_PROMS.load(rom_set)?);
        self.board.load_sound_prom(&MAPPY_SOUND_PROM.load(rom_set)?);
        Ok(())
    }

    /// Active-low: pressing clears the bit.
    fn apply_input(&mut self, button: u8, pressed: bool) {
        let b = &mut self.board;
        match button {
            INPUT_COIN1 => crate::set_bit_active_low(&mut b.in_coins, 0, pressed),
            INPUT_COIN2 => crate::set_bit_active_low(&mut b.in_coins, 1, pressed),
            INPUT_SERVICE => crate::set_bit_active_low(&mut b.in_coins, 3, pressed),
            INPUT_P1_RIGHT => crate::set_bit_active_low(&mut b.in_p1, 1, pressed),
            INPUT_P1_LEFT => crate::set_bit_active_low(&mut b.in_p1, 3, pressed),
            INPUT_P2_RIGHT => crate::set_bit_active_low(&mut b.in_p2, 1, pressed),
            INPUT_P2_LEFT => crate::set_bit_active_low(&mut b.in_p2, 3, pressed),
            INPUT_P1_BUTTON => crate::set_bit_active_low(&mut b.in_buttons, 0, pressed),
            INPUT_P2_BUTTON => crate::set_bit_active_low(&mut b.in_buttons, 1, pressed),
            INPUT_P1_START => crate::set_bit_active_low(&mut b.in_buttons, 2, pressed),
            INPUT_P2_START => crate::set_bit_active_low(&mut b.in_buttons, 3, pressed),
            _ => {}
        }
    }

    pub fn get_cpu_state(&self) -> phosphor_core::cpu::state::M6809State {
        self.main.snapshot()
    }

    /// One cycle, returning the instruction-boundary mask.
    pub fn step_cycle(&mut self) -> u32 {
        tick(&mut self.main, &mut self.sub, &mut self.board);
        self.board.instruction_boundaries(&self.main, &self.sub)
    }

    /// The CPU-facing bus, side effects and all.
    pub fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        self.board.read(master, addr)
    }

    pub fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.board.write(master, addr, data);
    }
}

impl Default for MappySystem {
    fn default() -> Self {
        Self::new()
    }
}

crate::impl_board_delegation!(MappySystem, board, TIMING, orientation);

impl MachineCore for MappySystem {
    crate::machine_core_metadata!("mappy", TIMING, crate::namco_mappy::clock_tree);

    fn gfx_sheets(&self) -> Vec<phosphor_core::core::machine::GfxSheet<'_>> {
        use phosphor_core::core::machine::GfxSheet;
        let v = &self.board.video;
        vec![
            GfxSheet {
                name: "chars",
                cache: v.tile_cache(),
                palette: v.char_pens(),
            },
            GfxSheet {
                name: "sprites",
                cache: v.sprite_cache(),
                palette: v.sprite_pens(),
            },
        ]
    }

    fn run_frame(&mut self) {
        run_frame(&mut self.main, &mut self.sub, &mut self.board);
    }

    fn reset(&mut self) {
        self.board.reset_board();
        self.main.reset(&mut self.board, BusMaster::Cpu(0));
        // The sound CPU sits in reset until the main CPU raises SUB RESET.
    }
}

impl SaveState for MappySystem {
    crate::machine_save_state!();
}

impl Nvram for MappySystem {}
impl Profilable for MappySystem {}

impl InputConfigurable for MappySystem {
    fn input_controls(&self) -> &'static [InputControl] {
        MAPPY_CONTROLS
    }
    fn handle_input(&mut self, event: InputEvent) {
        if let InputEvent::Button { id, pressed } = event {
            self.apply_input(id.0 as u8, pressed);
        }
    }
}

crate::impl_dip_switches!(
    MappySystem,
    MAPPY_DIP_BANKS,
    board.dsw1,
    board.dsw2,
    board.dsw0
);

crate::impl_board_debug_trace!(MappySystem, board);

crate::register_machine!(MappySystem, "mappy", &["mappy"], MAPPY_CONTROLS);

inventory::submit! {
    DisasmRegion {
        machine: "mappy",
        region: "main",
        cpu: DisasmCpu::M6809,
        org: 0xA000,
        size: MAPPY_PROGRAM_ROM.size as u32,
        load: |rs| MAPPY_PROGRAM_ROM.load(rs),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "mappy",
        region: "sound",
        cpu: DisasmCpu::M6809,
        org: 0xE000,
        size: MAPPY_SOUND_ROM.size as u32,
        load: |rs| MAPPY_SOUND_ROM.load(rs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::Renderable;

    crate::dip_test_suite!(MappySystem, &[DEFAULT_DSW1, DEFAULT_DSW2, DEFAULT_DSW0]);

    const LINE: u64 = TIMING.cycles_per_scanline;

    /// A system whose two CPUs each spin on `BRA *`, so nothing the test does
    /// is disturbed by a program running out of zeroed memory.
    fn parked() -> MappySystem {
        let mut sys = MappySystem::new();
        let mut rom = vec![0u8; 0x6000];
        rom[0] = 0x20; // BRA *
        rom[1] = 0xFE;
        rom[0x5FFE] = 0xA0; // reset vector 0xA000
        sys.board.load_program_rom(&rom);
        let mut snd = vec![0u8; 0x2000];
        snd[0] = 0x20;
        snd[1] = 0xFE;
        snd[0x1FFE] = 0xE0; // reset vector 0xE000
        sys.board.load_sub_rom(&snd);
        sys.reset();
        sys
    }

    /// The LS259 address for output `line` taking value `bit`.
    fn latch_addr(base: u16, line: u16, bit: bool) -> u16 {
        base | (line << 1) | u16::from(bit)
    }

    fn run_to(sys: &mut MappySystem, clock: u64) {
        while sys.board.clock < clock {
            sys.step_cycle();
        }
    }

    fn pixel(sys: &MappySystem, x: usize, row: usize) -> (u8, u8, u8) {
        let mut frame = vec![0u8; NATIVE_WIDTH * NATIVE_HEIGHT * 3];
        sys.render_frame(&mut frame);
        let i = (row * NATIVE_WIDTH + x) * 3;
        (frame[i], frame[i + 1], frame[i + 2])
    }

    /// PROMs for the raster tests: char color 0 is palette 16 (red), char
    /// color 1 is palette 17 (green), and every sprite pen is transparent.
    fn raster_proms() -> Vec<u8> {
        let mut p = vec![0u8; 0x220];
        p[16] = 0x07;
        p[17] = 0x38;
        p[32] = 0x00; // 4C color 0 pixel 0 -> 0x0 | 0x10 = 16 (no complement)
        p[32 + 4] = 0x01; // 4C color 1 pixel 0 -> 17
        // Every sprite nibble equals pen 15's, so every sprite pixel skips.
        p[288..].fill(0x00);
        p
    }

    const RED: (u8, u8, u8) = (255, 0, 0);
    const GREEN: (u8, u8, u8) = (0, 255, 0);

    #[test]
    fn machine_identity_and_geometry() {
        let sys = MappySystem::new();
        assert_eq!(sys.machine_id(), "mappy");
        assert_eq!(sys.display_size(), (288, 224));
        assert_eq!(sys.orientation(), Orientation::ROT90);
        assert_eq!(TIMING.cycles_per_frame(), 25_344);
        let mut flipped = MappySystem::new();
        flipped.board.video.set_flip(true);
        assert_eq!(
            flipped.orientation(),
            Orientation::ROT90.compose(Orientation::COCKTAIL)
        );
    }

    #[test]
    fn the_frame_loop_reaches_the_scanline_hook() {
        // A fresh framebuffer is black; a frame through run_frame has to
        // composite every row from the tilemap, which here is all red.
        let mut sys = parked();
        sys.board.load_proms(&raster_proms());
        assert_eq!(pixel(&sys, 0, 0), (0, 0, 0));
        sys.run_frame();
        for row in [0, 111, 223] {
            assert_eq!(pixel(&sys, 0, row), RED, "row {row}");
            assert_eq!(pixel(&sys, 287, row), RED, "row {row}");
        }
    }

    #[test]
    fn a_mid_frame_write_splits_the_picture_after_its_row() {
        // Row 100 is composited at the start of line 100, so a write five
        // cycles into that line is first seen on row 101.
        let mut sys = parked();
        sys.board.load_proms(&raster_proms());
        run_to(&mut sys, 100 * LINE + 5);
        for a in 0x0800..0x1000u16 {
            sys.bus_write(BusMaster::Cpu(0), a, 0x01);
        }
        run_to(&mut sys, TIMING.cycles_per_frame());
        assert_eq!(pixel(&sys, 40, 100), RED, "row 100 drawn before the write");
        assert_eq!(pixel(&sys, 40, 101), GREEN, "row 101 drawn after it");
        assert_eq!(pixel(&sys, 40, 223), GREEN);
    }

    #[test]
    fn vblank_raises_each_irq_only_when_enabled_and_holds_it() {
        let mut sys = parked();
        // Main enabled (Q1), sound not (Q0 low).
        sys.bus_write(BusMaster::Cpu(0), latch_addr(0x5000, 1, true), 0);
        run_to(&mut sys, VBLANK_LINE * LINE);
        assert!(!sys.board.main_irq_pending, "not before VBLANK");
        sys.step_cycle();
        assert!(sys.board.interrupt_state(BusMaster::Cpu(0)).irq);
        assert!(!sys.board.interrupt_state(BusMaster::Cpu(1)).irq);

        // A 74LS74 set by the VBLANK edge: still held a whole frame later,
        // because nothing but INTON going low clears it.
        run_to(&mut sys, TIMING.cycles_per_frame() + 10 * LINE);
        assert!(sys.board.interrupt_state(BusMaster::Cpu(0)).irq);
        sys.bus_write(BusMaster::Cpu(0), latch_addr(0x5000, 1, false), 0);
        assert!(!sys.board.interrupt_state(BusMaster::Cpu(0)).irq);
    }

    #[test]
    fn the_latch_takes_its_data_from_a0_and_either_cpu_can_write_it() {
        let mut sys = parked();
        // The data byte is ignored: A0 is the bit.
        sys.bus_write(BusMaster::Cpu(0), latch_addr(0x5000, 3, true), 0x00);
        assert!(sys.board.wsg.sound_enabled());
        sys.bus_write(BusMaster::Cpu(0), latch_addr(0x5000, 3, false), 0xFF);
        assert!(!sys.board.wsg.sound_enabled());
        // The sound CPU reaches the same latch at 0x2000.
        sys.bus_write(BusMaster::Cpu(1), latch_addr(0x2000, 3, true), 0x00);
        assert!(sys.board.wsg.sound_enabled());
        assert_eq!(sys.board.latch, 0x08);
    }

    #[test]
    fn flip_comes_from_latch_q2() {
        let mut sys = parked();
        assert_eq!(sys.orientation(), Orientation::ROT90);
        sys.bus_write(BusMaster::Cpu(0), latch_addr(0x5000, 2, true), 0);
        assert_eq!(
            sys.orientation(),
            Orientation::ROT90.compose(Orientation::COCKTAIL)
        );
        sys.bus_write(BusMaster::Cpu(0), latch_addr(0x5000, 2, false), 0);
        assert_eq!(sys.orientation(), Orientation::ROT90);
    }

    #[test]
    fn the_scroll_page_loads_the_address_not_the_data() {
        let mut sys = parked();
        // The data byte is ignored; the value is the offset into the page.
        sys.bus_write(BusMaster::Cpu(0), 0x3800, 0xFF);
        assert_eq!(sys.board.video.scroll, 0);
        sys.bus_write(BusMaster::Cpu(0), 0x39C0, 0x00);
        assert_eq!(sys.board.video.scroll, 0x38);
        sys.bus_write(BusMaster::Cpu(0), 0x3FFF, 0x00);
        assert_eq!(sys.board.video.scroll, 0xFF);
        // Write-only: reads float high.
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x39C0), 0xFF);
    }

    #[test]
    fn the_tilemap_has_fixed_side_strips_and_a_scrolling_middle() {
        // Columns 0-1 and 34-35 come out of 0x780-0x7FF, unscrolled.
        assert_eq!(tile_pixel(0, 0, 0), (0x7C2, 0));
        assert_eq!(tile_pixel(0, 0, 200), (0x7C2, 0));
        assert_eq!(tile_pixel(35, 8, 0), (0x7A3, 0));
        // Adjacent pixel rows of one strip tile row share the tile.
        assert_eq!(tile_pixel(35, 9, 0), (0x7A3, 1));
        assert_eq!(tile_pixel(0, 32, 0), (0x7C6, 0));
        // Columns 2-33 are the linear 32x60 playfield, scrolled in pixels.
        assert_eq!(tile_pixel(2, 0, 0), (0, 0));
        assert_eq!(tile_pixel(2, 8, 0), (32, 0));
        assert_eq!(tile_pixel(2, 0, 8), (32, 0));
        assert_eq!(tile_pixel(33, 223, 0), (31 + 27 * 32, 7));
        // The scroll wraps around the 60 rows.
        assert_eq!(tile_pixel(2, 223, 255), (59 * 32, 6));
        // Past tile row 31 the strips read the padding word (never displayed;
        // the visible rows reach tile row 27).
        assert_eq!(tile_pixel(0, 32 * 8, 0), (0x7FF, 0));
    }

    #[test]
    fn sub_reset_holds_the_sound_cpu_until_raised_then_resets_it() {
        let mut sys = parked();
        sys.board.load_sub_rom(&{
            let mut snd = vec![0u8; 0x2000];
            snd[0x1100] = 0x20; // BRA * at 0xF100
            snd[0x1101] = 0xFE;
            snd[0x1FFE] = 0xF1;
            snd
        });
        for _ in 0..1000 {
            sys.step_cycle();
        }
        assert_eq!(sys.sub.pc, 0, "held in reset: never fetched a vector");
        sys.bus_write(BusMaster::Cpu(0), latch_addr(0x5000, 5, true), 0);
        sys.step_cycle();
        assert_eq!(sys.sub.pc, 0xF100, "released: the reset sequence ran");
        // Sampled between instructions: inside the BRA, PC has moved past its
        // opcode.
        let mut boundaries = 0;
        for _ in 0..100 {
            if sys.step_cycle() & 2 != 0 {
                boundaries += 1;
                assert_eq!(sys.sub.pc, 0xF100, "executing its loop");
            }
        }
        assert!(
            boundaries > 10,
            "the sound CPU reached {boundaries} boundaries"
        );
    }

    #[test]
    fn the_sound_ram_is_one_ram_and_its_first_64_bytes_drive_the_15xx() {
        let mut sys = parked();
        sys.bus_write(BusMaster::Cpu(0), latch_addr(0x5000, 3, true), 0);
        // Main writes voice 0's volume; the sound CPU writes voice 1's.
        sys.bus_write(BusMaster::Cpu(0), 0x4003, 9);
        sys.bus_write(BusMaster::Cpu(1), 0x000B, 6);
        let v = sys.board.wsg.voices();
        assert_eq!((v[0].1, v[1].1), (9, 6));
        // Each CPU reads what the other wrote.
        assert_eq!(sys.bus_read(BusMaster::Cpu(1), 0x0003), 9);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x400B), 6);
        // Past the register window it is only RAM.
        sys.bus_write(BusMaster::Cpu(0), 0x4043, 15);
        assert_eq!(sys.board.wsg.voices()[0].1, 9);
        assert_eq!(sys.bus_read(BusMaster::Cpu(1), 0x0043), 15);
    }

    #[test]
    fn the_sound_rom_is_a_full_8k_with_no_mirror() {
        let mut sys = parked();
        let snd: Vec<u8> = (0..0x2000).map(|i| (i ^ (i >> 8)) as u8).collect();
        sys.board.load_sub_rom(&snd);
        for a in [0x000u16, 0x123, 0xFFF] {
            assert_eq!(sys.bus_read(BusMaster::Cpu(1), 0xE000 + a), snd[a as usize]);
            assert_eq!(
                sys.bus_read(BusMaster::Cpu(1), 0xF000 + a),
                snd[0x1000 + a as usize]
            );
        }
        // The halves differ, so this is not a mirrored 4K.
        assert_ne!(
            sys.bus_read(BusMaster::Cpu(1), 0xE000),
            sys.bus_read(BusMaster::Cpu(1), 0xF000)
        );
        // The main CPU never sees it at its translated backing.
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x6123), 0xFF);
    }

    #[test]
    fn the_58xx_pair_reads_the_cabinet_and_both_dip_banks() {
        let mut sys = parked();
        sys.board.dsw1 = 0xC3;
        sys.board.dsw2 = 0x5A;
        sys.board.dsw0 = 0x0B;
        // Chip 1 in mode 4, chip 0 in mode 1; then release 4 RESET.
        sys.bus_write(BusMaster::Cpu(0), 0x4818, 4);
        sys.bus_write(BusMaster::Cpu(0), 0x4808, 1);
        sys.board.run_io();
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x4810), 0xF0, "in reset");
        sys.bus_write(BusMaster::Cpu(0), latch_addr(0x5000, 4, true), 0);
        sys.apply_input(INPUT_P1_LEFT, true);
        sys.board.run_io();
        let n = |sys: &mut MappySystem, a| sys.bus_read(BusMaster::Cpu(0), a) & 0x0F;
        // SW3 through the mux: switches 1-4 with the select low, 5-8 high.
        assert_eq!(n(&mut sys, 0x4810), !0x0Au8 & 0x0F);
        assert_eq!(n(&mut sys, 0x4811), !0x05u8 & 0x0F);
        // SW2 on ports B and C, the test switches on D.
        assert_eq!(n(&mut sys, 0x4812), !0x03u8 & 0x0F);
        assert_eq!(n(&mut sys, 0x4814), !0x0Cu8 & 0x0F);
        assert_eq!(n(&mut sys, 0x4816), !0x0Bu8 & 0x0F);
        // Chip 0, mode 1: P1 left is port B pin 25, bit 3, at nibble 5.
        assert_eq!(n(&mut sys, 0x4805), 0x08);
    }

    #[test]
    fn the_palette_dac_has_no_load_term() {
        // Per tap: G / the ladder's G, one scale shared by the three guns.
        // Red taps 33.23, 70.71, 151.06; blue taps 81.31, 173.69. With no
        // load every gun's full drive reaches 255.
        let full = mappy_palette(&[0xFF; 32]);
        assert_eq!(full[0], (255, 255, 255), "blue reaches full drive");
        let bits: Vec<_> = [0x01, 0x02, 0x04, 0x40, 0x80]
            .iter()
            .map(|&v| mappy_palette(&[v; 32])[0])
            .collect();
        assert_eq!(
            bits,
            [(33, 0, 0), (71, 0, 0), (151, 0, 0), (0, 0, 81), (0, 0, 174)]
        );
    }

    #[test]
    fn the_sprite_transparency_is_lookup_nibble_15() {
        // Color 3 maps pen 15 to nibble 7 and pen 0 to nibble 7 as well, but
        // transparency is nibble 15 itself, not pen 15's nibble: pen 0 draws
        // and pen 1 (nibble 15) skips. Both pixels distinguish the rules.
        let mut sys = parked();
        let mut p = vec![0u8; 0x220];
        p[7] = 0x07; // palette 7 red, for the drawn pen 0
        p[15] = 0x07; // palette 15 red, so a drawn pen 1 would show
        // Tiles stay black: palette 16 is zero, and char LUT entries are zero.
        p[288 + 3 * 16 + 15] = 0x07;
        p[288 + 3 * 16] = 0x07;
        p[288 + 3 * 16 + 1] = 0x0F;
        sys.board.load_proms(&p);
        // One sprite at slot 0: code 0, color 3, on, 16x16, top-left (48,153).
        sys.bus_write(BusMaster::Cpu(0), 0x1780, 0x00);
        sys.bus_write(BusMaster::Cpu(0), 0x1781, 0x03);
        sys.bus_write(BusMaster::Cpu(0), 0x1F80, 0x48);
        sys.bus_write(BusMaster::Cpu(0), 0x1F81, 0x58);
        sys.bus_write(BusMaster::Cpu(0), 0x2780, 0x00);
        sys.bus_write(BusMaster::Cpu(0), 0x2781, 0x00);
        // Sprite pixel (0,0) is pen 1 (plane 0's bit: image byte 1 bit 3, the
        // odd chip's first byte); every other pixel is pen 0.
        let mut sprites = vec![0u8; 0x4000];
        sprites[0x2000] = 0x08;
        sys.board.load_sprite_rom(&sprites);
        sys.run_frame();
        assert_eq!(pixel(&sys, 48, 153), (0, 0, 0), "pen 1 skips");
        assert_eq!(pixel(&sys, 49, 153), RED, "pen 0 draws");
    }
}
