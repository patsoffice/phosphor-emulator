//! Super Pac-Man (Namco, 1982).
//!
//! # Schematics
//!
//! | Drawing | Source | Pages |
//! |---|---|---|
//! | SUPER PAC-MAN CPU BD. A084-91436-F316 (sheet 9-5) and VIDEO BD. A084-91435-D316 (sheet 9-3), Bally Midway | `arcade-museum.com/manuals-videogames/S/superpacman-schematics.pdf` | 14 pages, read 2026-10-08 |
//!
//! Transcribed to `docs/schematics/superpacman-board.md`. Part of the address
//! decode is in two PALs (SPC-5, SPC-6) whose equations are not on the
//! drawing, and the I/O, sound and video sequencing live in Namco customs
//! (07XX, 15XX, 16XX, 56XX, 00XX, 04XX, 11XX, 12XX) drawn as boxes. Where the
//! sheets go dark, what is here was inferred from the program's behavior, and
//! each such place says so where it is used.
//!
//! The first of Namco's Mappy-family boards: two MC6809E, a main CPU running
//! the game and a sound CPU that does nothing but write the 15XX's voice
//! registers, which live in the 1 KB of RAM the two share. Two 56XX MCUs
//! behind a 16XX read the controls and count coins.
//!
//! Memory map, main CPU:
//! ```text
//!   0x0000-0x07ff  Video RAM: tile codes 0x000-0x3ff, attributes 0x400-0x7ff
//!   0x0800-0x1fff  Work RAM, with the sprite registers at 0x0f80, 0x1780, 0x1f80
//!   0x2000-0x27ff  Flip screen, video board 4A (write D0; a read sets it)
//!   0x4000-0x43ff  Sound RAM, shared; 0x4000-0x403f are the 15XX's registers
//!   0x4800-0x480f  56XX #0 (16 nibbles)    0x4810-0x481f  56XX #1
//!   0x5000-0x500f  LS259 latch: line = A1-A3, data = A0
//!   0x8000         Watchdog reset (not modeled)
//!   0xa000-0xbfff  Socket 1D, empty on this set (open bus)
//!   0xc000-0xffff  Program ROM (1C, 1B)
//! ```
//!
//! Memory map, sound CPU:
//! ```text
//!   0x0000-0x03ff  Sound RAM (the main CPU's 0x4000-0x43ff)
//!   0x2000-0x200f  The same LS259 latch
//!   0xe000-0xffff  Program ROM (1K): a 2732 in a 2764 socket, so it mirrors
//! ```
//!
//! LS259 latch (2M) outputs:
//! ```text
//!   Q0 sound CPU IRQ enable   Q1 main CPU IRQ enable   Q2 n.c.
//!   Q3 SOUND ON               Q4 56XX reset (low = reset)
//!   Q5 sound CPU reset (low = reset)   Q6, Q7 n.c.
//! ```

use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::DebugTraceBuffer;
use phosphor_core::core::machine::{
    ActionRole, DipApplyTiming, DipChoice, DipCondition, DipOption, DipSwitchBank, Direction,
    InputConfigurable, InputControl, InputEvent, InputId, InputKind, MachineCore, Nvram,
    Orientation, Profilable, SaveState, TimingConfig,
};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, ClockDomainName as Clk, ClockTree, DomainId};
use phosphor_core::cpu::m6809::M6809;
use phosphor_core::cpu::{Cpu, CpuStateTrait};
use phosphor_core::device::namco_15xx::Namco15xx;
use phosphor_core::device::namco56::{InPort, Namco56};
use phosphor_core::gfx::decode::{GfxCache, GfxLayout, decode_gfx};
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion, Saveable};

use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::namco_wsg_output::{BoardParams, WsgOutputStage};
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::scanline::ScanlineDriven;

/// Both CPUs: 18.432 MHz / 12 = 1.536 MHz. The pixel clock is 18.432 / 3, so
/// a 384-dot line is 96 CPU cycles, and 264 lines make 25344 cycles a frame
/// (60.61 Hz).
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
/// One 18.432 MHz crystal (X1, oscillator at 4A). A 74LS109 at 3B divides it
/// by 3 for the 6.144 MHz dot clock; the 07XX at 1N counts that into 1H and
/// 2H, and both 6809Es run on 2H, 1.536 MHz. The 15XX's voice update, in which
/// each of its eight voices advances once, is 256 dots (see
/// [`Namco15xx`]): 18.432 MHz / 768.
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

// Input button IDs.
pub const INPUT_COIN1: u8 = 0;
pub const INPUT_COIN2: u8 = 1;
pub const INPUT_SERVICE: u8 = 2;
pub const INPUT_P1_START: u8 = 3;
pub const INPUT_P2_START: u8 = 4;
pub const INPUT_P1_UP: u8 = 5;
pub const INPUT_P1_RIGHT: u8 = 6;
pub const INPUT_P1_DOWN: u8 = 7;
pub const INPUT_P1_LEFT: u8 = 8;
pub const INPUT_P1_BUTTON: u8 = 9;
pub const INPUT_P2_UP: u8 = 10;
pub const INPUT_P2_RIGHT: u8 = 11;
pub const INPUT_P2_DOWN: u8 = 12;
pub const INPUT_P2_LEFT: u8 = 13;
pub const INPUT_P2_BUTTON: u8 = 14;

// ---------------------------------------------------------------------------
// GFX layouts: two bitplanes four bits apart in each byte, four pixels to a
// byte (`decode_gfx` numbers planes LSB first)
// ---------------------------------------------------------------------------

/// 8x8 2bpp chars, 256 codes. Pixels 0-3 of a row are in byte `y + 8` and
/// pixels 4-7 in byte `y`, which is Pac-Man's tile layout exactly.
pub static SUPERPAC_CHAR_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[4, 0],
    x_offsets: &[64, 65, 66, 67, 0, 1, 2, 3],
    y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
    char_increment: 128,
};

/// 16x16 2bpp sprites, 128 codes: four 8-byte columns of four pixels each,
/// and the lower eight rows 32 bytes on.
pub static SUPERPAC_SPRITE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[4, 0],
    x_offsets: &[
        0, 1, 2, 3, 64, 65, 66, 67, 128, 129, 130, 131, 192, 193, 194, 195,
    ],
    y_offsets: &[
        0, 8, 16, 24, 32, 40, 48, 56, 256, 264, 272, 280, 288, 296, 304, 312,
    ],
    char_increment: 512,
};

// ---------------------------------------------------------------------------
// Video
// ---------------------------------------------------------------------------

/// The palette PROM's three ladders on the video board (sheet 9-3): red R3-R5
/// and green R6-R8 through 1K/470/220, blue R9-R10 through 470/220, PROM bits
/// 0-2 red, 3-5 green, 6-7 blue. The PROM drives them directly.
const DAC_RG: [f64; 3] = [1000.0, 470.0, 220.0];
const DAC_B: [f64; 2] = [470.0, 220.0];
/// Each color node's load to the video return (R56-R58).
const DAC_LOAD: f64 = 2200.0;

/// One ladder's per-tap weights, as fractions of the drive level: a tap's
/// conductance over the whole node's, the other taps and the load included.
/// A resistive node is linear in its sources, so these superpose exactly.
fn dac_weights(ladder: &[f64]) -> Vec<f64> {
    let total: f64 = ladder.iter().map(|r| 1.0 / r).sum::<f64>() + 1.0 / DAC_LOAD;
    ladder.iter().map(|r| (1.0 / r) / total).collect()
}

/// Build the 32-entry palette, with one scale shared by all three guns so the
/// hottest ladder spans 0-255. Blue's ladder has one tap fewer against the
/// same load, so its full drive is a little lower than red's and green's: 253
/// against 255.
fn superpac_palette(prom: &[u8]) -> [(u8, u8, u8); 32] {
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
/// the transparency and the super-priority tests are made on.
#[derive(Clone, Copy)]
struct SpritePixel {
    pen: u8,
}

/// Video state: decoded graphics, the PROM lookups, and the native RGB
/// framebuffer, composited one row at a time at the start of each scanline.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct SuperPacVideo {
    /// 8x8 chars, 256 codes. ROM-derived.
    #[save_skip]
    chars: GfxCache,
    /// 16x16 sprites, 128 codes. ROM-derived.
    #[save_skip]
    sprites: GfxCache,
    /// 32 palette entries. ROM-derived.
    #[save_skip]
    palette: [(u8, u8, u8); 32],
    /// Char lookup (4E): color * 4 + pixel -> palette index (upper 16).
    #[save_skip]
    char_lut: [u8; 256],
    /// Sprite lookup (3L): color * 4 + pixel -> palette index (lower 16).
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
    #[save(id = 1)]
    flip: bool,
}

impl SuperPacVideo {
    pub fn new() -> Self {
        Self {
            chars: GfxCache::new(256, 8, 8),
            sprites: GfxCache::new(128, 16, 16),
            palette: [(0, 0, 0); 32],
            char_lut: [0; 256],
            sprite_lut: [0; 256],
            char_pens: [(0, 0, 0); 256],
            sprite_pens: [(0, 0, 0); 256],
            framebuffer: vec![0; NATIVE_WIDTH * NATIVE_HEIGHT * 3],
            sprite_line: vec![None; NATIVE_WIDTH],
            flip: false,
        }
    }

    pub fn load_tile_rom(&mut self, data: &[u8]) {
        self.chars = decode_gfx(data, 0, 256, &SUPERPAC_CHAR_LAYOUT);
    }

    pub fn load_sprite_rom(&mut self, data: &[u8]) {
        self.sprites = decode_gfx(data, 0, 128, &SUPERPAC_SPRITE_LAYOUT);
    }

    /// Load the PROMs: 32 bytes of palette (4C), then the 256-entry char
    /// lookup (4E) and the 256-entry sprite lookup (3L).
    ///
    /// Chars reach the upper sixteen palette entries through the complement
    /// of their lookup nibble, and sprites the lower sixteen through the
    /// nibble as stored. Both mappings are made by the PAL at 4D (SPV-5),
    /// which takes 4E's four outputs and the sprite pixel and drives the
    /// palette PROM's A0-A4; its equations are not on the drawing, so the
    /// mapping is inferred from what the program draws.
    pub fn load_proms(&mut self, data: &[u8]) {
        self.palette = superpac_palette(data.get(..32).unwrap_or(&[]));
        for i in 0..256 {
            let c = data.get(32 + i).copied().unwrap_or(0) & 0x0F;
            self.char_lut[i] = (c ^ 0x0F) | 0x10;
            self.sprite_lut[i] = data.get(288 + i).copied().unwrap_or(0) & 0x0F;
            self.char_pens[i] = self.palette[self.char_lut[i] as usize];
            self.sprite_pens[i] = self.palette[self.sprite_lut[i] as usize];
        }
    }

    pub fn set_flip(&mut self, flip: bool) {
        self.flip = flip;
    }

    pub fn reset(&mut self) {
        self.flip = false;
    }

    /// Composite native row `row` (0-223).
    ///
    /// Four layers:
    ///
    /// 1. every tile, opaque;
    /// 2. sprites, over everything, a pen of palette entry 15 transparent;
    /// 3. tiles whose attribute bit 6 is set, again, over the sprites, except
    ///    where the tile's own lookup nibble is 0 (palette entry 31);
    /// 4. sprite pixels of palette entry 0 or 1, again, over even those.
    ///
    /// The second is on the drawing: a 74LS20 at 4J decodes all four of 3L's
    /// outputs high and blocks the line-buffer write. The other three are the
    /// SPV-5 PAL's and are inferred from the program's behavior, the fourth in
    /// particular from Pac & Pal, whose ghost eyes are what it is for; Super
    /// Pac-Man's program may never use it.
    pub fn render_scanline(&mut self, row: usize, videoram: &[u8], workram: &[u8]) {
        self.draw_sprite_line(row, workram);

        let chars = &self.chars;
        let char_lut = &self.char_lut;
        let palette = &self.palette;
        let sprite_line = &self.sprite_line;
        let out = &mut self.framebuffer[row * NATIVE_WIDTH * 3..][..NATIVE_WIDTH * 3];

        let tile_row = row / 8;
        let line = row % 8;
        for col in 0..NATIVE_WIDTH / 8 {
            let offset = crate::namco_video::namco_tilemap_offset(col as i32, tile_row as i32);
            let code = videoram[offset] as usize;
            let attr = videoram[offset + 0x400];
            let color = (attr & 0x3F) as usize;
            let high = attr & 0x40 != 0;
            let pixels = chars.row_slice(code, line);
            for (dx, &pix) in pixels.iter().enumerate() {
                let x = col * 8 + dx;
                let tile_pen = char_lut[color * 4 + pix as usize];
                let pen = match sprite_line[x] {
                    None => tile_pen,
                    Some(s) if s.pen <= 1 => s.pen,
                    Some(_) if high && tile_pen != 0x1F => tile_pen,
                    Some(s) => s.pen,
                };
                let (r, g, b) = palette[pen as usize];
                out[x * 3..x * 3 + 3].copy_from_slice(&[r, g, b]);
            }
        }
    }

    /// Resolve the sprites crossing native row `row` into the line buffer.
    ///
    /// 64 slots, two bytes apart in three banks: code and color at 0x0f80,
    /// Y and X at 0x1780, and attributes at 0x1f80 (flip X/Y, double width and
    /// height, X bit 8, disable). Later slots draw over earlier ones.
    ///
    /// The position offsets (`- 40` in X; `256 - y`, `- 32` in Y) are inside
    /// the 04XX and 12XX customs and are inferred from where the program puts
    /// things. The `+ 1` in `top` is the one-line delay of the line buffers
    /// (4M, 4N, banked on 1V), folded in here; see
    /// `docs/schematics/sprite-list-scan.md` before adding it again.
    ///
    /// Under flip the tilemap needs nothing (the frontend mirrors the whole
    /// frame), but a sprite's position does not mirror on this board: the
    /// program writes mirrored coordinates itself and only the image flips. So
    /// the row and the columns are mirrored here to cancel the frontend's.
    fn draw_sprite_line(&mut self, row: usize, workram: &[u8]) {
        // workram starts at CPU 0x0800.
        const CODE: usize = 0x0F80 - 0x0800;
        const POS: usize = 0x1780 - 0x0800;
        const ATTR: usize = 0x1F80 - 0x0800;
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
            let color = (workram[CODE + offs + 1] & 0x3F) as usize;
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
                    let pen = self.sprite_lut[color * 4 + pixels[src] as usize];
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

impl Default for SuperPacVideo {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// The drive
// ---------------------------------------------------------------------------

/// The two CPUs and the board as disjoint borrows, so a cycle dispatches at a
/// concrete type.
struct Drive<'a> {
    main: &'a mut M6809,
    sub: &'a mut M6809,
    board: &'a mut SuperPacBoard,
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
/// the main CPU's E is /2H and its Q is a 74LS74 (2A) re-timing of 2H, and
/// that Q is the sound CPU's E. So in each cycle the sound CPU's bus access
/// falls a quarter cycle before the main CPU's, which is the order they are
/// stepped in here. The shared RAM is time-sliced on 1H and 2H (15XX while 2H
/// is high, sound CPU while both are low, main CPU while 1H is high), so
/// neither CPU ever waits for the other and that order is the whole of the
/// arbitration.
#[inline]
fn step_cycle(main: &mut M6809, sub: &mut M6809, board: &mut SuperPacBoard) {
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
pub fn tick(main: &mut M6809, sub: &mut M6809, board: &mut SuperPacBoard) {
    Drive { main, sub, board }.tick();
}

/// Run one frame.
pub fn run_frame(main: &mut M6809, sub: &mut M6809, board: &mut SuperPacBoard) {
    Drive { main, sub, board }.run_frame();
}

// ---------------------------------------------------------------------------
// The board
// ---------------------------------------------------------------------------

#[derive(BusDebug, DebugTrace, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct SuperPacBoard {
    /// Video, work and sound RAM, and both CPUs' ROMs. Both CPUs share it;
    /// the sound CPU's regions sit at translated addresses (see `build_map`).
    #[debug_map(cpu = 0)]
    #[save(id = 1)]
    pub(crate) map: AddressSpace16,

    #[save(id = 2)]
    pub(crate) video: SuperPacVideo,

    #[debug_device("Namco 15XX")]
    #[save(id = 3)]
    pub(crate) wsg: Namco15xx,

    #[save(id = 4)]
    /// The 4M latch, the two resistor ladders, C25 and the coupling: Pac-Man's
    /// stage with this board's bias arm and eight voices. See
    /// [`BoardParams::SUPERPAC`].
    pub(crate) audio: WsgOutputStage,

    #[debug_device("56XX #0")]
    #[save(id = 5)]
    pub(crate) io0: Namco56,
    #[debug_device("56XX #1")]
    #[save(id = 6)]
    pub(crate) io1: Namco56,

    // Cabinet switches, active-low nibbles as they sit on the 56XX pins.
    /// 56XX #0 port A: coin 1, coin 2, unused, service coin.
    #[save(id = 7)]
    pub(crate) in_coins: u8,
    /// 56XX #0 port B: P1 up, right, down, left.
    #[save(id = 8)]
    pub(crate) in_p1: u8,
    /// 56XX #0 port C: P2 up, right, down, left.
    #[save(id = 9)]
    pub(crate) in_p2: u8,
    /// 56XX #0 port D: P1 button, P2 button, start 1, start 2.
    #[save(id = 10)]
    pub(crate) in_buttons: u8,

    /// DIP bank SW2 at 5B (56XX #1 ports B and C).
    #[save(id = 11)]
    pub(crate) dsw1: u8,
    /// DIP bank SW3 at 5E (56XX #1 port A, through the 74LS157 at 4E).
    #[save(id = 12)]
    pub(crate) dsw2: u8,
    /// TEST (pin 33) and C.T. VERSION (pin 32) (56XX #1 port D).
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

impl SuperPacBoard {
    pub fn new() -> Self {
        let clocks = clock_tree();
        let wsg_dom = clocks.find(Clk::Psg).expect("declared 15XX domain");
        Self {
            map: Self::build_map(),
            video: SuperPacVideo::new(),
            wsg: Namco15xx::new(),
            audio: WsgOutputStage::new(BoardParams::SUPERPAC, TIMING.cpu_clock_hz),
            io0: Namco56::new(),
            io1: Namco56::new(),
            in_coins: 0x0F,
            in_p1: 0x0F,
            in_p2: 0x0F,
            in_buttons: 0x0F,
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
    /// main CPU's side decodes.
    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(
            Region::VideoRam,
            "Video RAM",
            0x0000,
            0x0800,
            AccessKind::ReadWrite,
        )
        .region(
            Region::WorkRam,
            "Work RAM (sprite registers at 0x0F80/0x1780/0x1F80)",
            0x0800,
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
            "Sound ROM (sound 0xF000)",
            0x6000,
            0x1000,
            AccessKind::ReadOnly,
        )
        .region(
            Region::MainRom,
            "Program ROM",
            0xC000,
            0x4000,
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
    /// the 56XXs one line later.
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

    /// Run both 56XXs once.
    ///
    /// The drawing ties VBLANK to each chip's pin 4, its interrupt, so they
    /// act once a frame on VBLANK. How long each takes to respond is its
    /// undumped program's business and not on any drawing. Here they run on
    /// the scanline boundary after VBLANK starts, 62.5 us in, the nearest
    /// point the scanline drive already visits. **That latency is a stand-in
    /// for the MCU's, not a part on the board.**
    fn run_io(&mut self) {
        let (coins, p1, p2, buttons) = (self.in_coins, self.in_p1, self.in_p2, self.in_buttons);
        self.io0.run(|port, _| match port {
            InPort::A => coins,
            InPort::B => p1,
            InPort::C => p2,
            InPort::D => buttons,
        });
        let (dsw0, dsw1, dsw2) = (self.dsw0, self.dsw1, self.dsw2);
        self.io1.run(|port, out_a| match port {
            // The 74LS157: pin 13 low selects SW2's low four switches, high
            // its high four.
            InPort::A => {
                if out_a & 1 == 0 {
                    dsw2 & 0x0F
                } else {
                    dsw2 >> 4
                }
            }
            InPort::B => dsw1 & 0x0F,
            InPort::C => dsw1 >> 4,
            InPort::D => dsw0 & 0x0F,
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
        self.io1.reset();
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
            0x0000..=0x1FFF => self.map.read_backing(addr),
            0x2000..=0x27FF => {
                // The flip flop (video 4A) is clocked by the video board's
                // 74LS138 (1C) Y4, which decodes this whole 2K page. Whether
                // SPC-6's strobe into that decoder also fires on a read is the
                // PAL's secret. Pac & Pal's program turns the flip on with a
                // read, which only works if it does, latching the bus floating
                // high; so a read sets it here.
                self.video.set_flip(true);
                0xFF
            }
            0x4000..=0x43FF => self.map.read_backing(addr),
            0x4800..=0x480F => self.io0.read(addr),
            0x4810..=0x481F => self.io1.read(addr),
            0xC000..=0xFFFF => self.map.read_backing(addr),
            _ => 0xFF,
        }
    }

    fn main_write(&mut self, addr: u16, data: u8) {
        match addr {
            0x0000..=0x1FFF => self.map.write_backing(addr, data),
            0x2000..=0x27FF => self.video.set_flip(data & 1 != 0),
            0x4000..=0x43FF => self.sound_ram_write(addr - 0x4000, data),
            0x4800..=0x480F => self.io0.write(addr, data),
            0x4810..=0x481F => self.io1.write(addr, data),
            0x5000..=0x500F => self.latch_write(addr),
            _ => {} // 0x8000 watchdog (not modeled), ROM, unmapped
        }
    }

    fn sub_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x03FF => self.map.read_backing(0x4000 + addr),
            // A 2732 in the 2764 socket: A12 reaches no pin, so 0xE000-0xEFFF
            // is the same 4 KB as 0xF000-0xFFFF.
            0xE000..=0xFFFF => self.map.read_backing(0x6000 + (addr & 0x0FFF)),
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
            3 => self.wsg.set_sound_enabled(bit),
            4 => {
                self.io0.set_reset(!bit);
                self.io1.set_reset(!bit);
            }
            5 if bit && before & LATCH_SUB_RUN == 0 => self.pending_sub_reset = true,
            _ => {}
        }
    }
}

/// The 56XXs run this many lines into the frame; see `run_io`.
const IO_RUN_LINE: u64 = VBLANK_LINE + 1;

impl Default for SuperPacBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for SuperPacBoard {
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
// ROM definitions: the `superpac` set
// ---------------------------------------------------------------------------

pub static SUPERPAC_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "sp1-2.1c",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x4bb33d9c],
        },
        RomEntry {
            name: "sp1-1.1b",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x846fbb4a],
        },
    ],
};

pub static SUPERPAC_SOUND_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "spc-3.1k",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0x04445ddb],
    }],
};

pub static SUPERPAC_TILE_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "sp1-6.3c",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0x91c5935c],
    }],
};

pub static SUPERPAC_SPRITE_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[RomEntry {
        name: "spv-2.3f",
        size: 0x2000,
        offset: 0x0000,
        crc32: &[0x670a42f2],
    }],
};

/// Palette (4C), char lookup (4E), sprite lookup (3L).
pub static SUPERPAC_COLOR_PROMS: RomRegion = RomRegion {
    size: 0x0220,
    entries: &[
        RomEntry {
            name: "superpac.4c",
            size: 0x0020,
            offset: 0x0000,
            crc32: &[0x9ce22c46],
        },
        RomEntry {
            name: "superpac.4e",
            size: 0x0100,
            offset: 0x0020,
            crc32: &[0x1253c5c1],
        },
        RomEntry {
            name: "superpac.3l",
            size: 0x0100,
            offset: 0x0120,
            crc32: &[0xd4d7026f],
        },
    ],
};

/// The 15XX's waveforms (3M).
pub static SUPERPAC_SOUND_PROM: RomRegion = RomRegion {
    size: 0x0100,
    entries: &[RomEntry {
        name: "superpac.3m",
        size: 0x0100,
        offset: 0x0000,
        crc32: &[0xad43688f],
    }],
};

// ---------------------------------------------------------------------------
// DIP switches. Values are pin levels: the 56XX inverts what it reads, so a
// switch that is ON reads 0 here.
//
// Bank names are the drawing's (sheet 9-5): SW2 at 5B on 56XX #1 pins 22-29,
// switch 1 on pin 22, and SW3 at 5E reaching pins 38-41 through the 4E
// 74LS157, switches 1-4 with pin 13 low and 5-8 with it high. What each
// setting means is not on the drawing; the tables below are inferred from the
// program's behavior and have not been checked against an operator manual.
// ---------------------------------------------------------------------------

const DEFAULT_DSW1: u8 = 0xFF;
const DEFAULT_DSW2: u8 = 0xFF;
const DEFAULT_DSW0: u8 = 0x0F;

const DSW1_OPTIONS: &[DipOption] = &[
    DipOption {
        name: "Difficulty",
        mask: 0x0F,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "Rank 0 (Normal)",
                value: 0x0F,
            },
            DipChoice {
                label: "Rank 1 (Easiest)",
                value: 0x0E,
            },
            DipChoice {
                label: "Rank 2",
                value: 0x0D,
            },
            DipChoice {
                label: "Rank 3",
                value: 0x0C,
            },
            DipChoice {
                label: "Rank 4",
                value: 0x0B,
            },
            DipChoice {
                label: "Rank 5",
                value: 0x0A,
            },
            DipChoice {
                label: "Rank 6 (Medium)",
                value: 0x09,
            },
            DipChoice {
                label: "Rank 7",
                value: 0x08,
            },
            DipChoice {
                label: "Rank 8 (Default)",
                value: 0x07,
            },
            DipChoice {
                label: "Rank 9",
                value: 0x06,
            },
            DipChoice {
                label: "Rank A",
                value: 0x05,
            },
            DipChoice {
                label: "Rank B (Hardest)",
                value: 0x04,
            },
            DipChoice {
                label: "Rank C (Easy Auto)",
                value: 0x03,
            },
            DipChoice {
                label: "Rank D (Auto)",
                value: 0x02,
            },
            DipChoice {
                label: "Rank E (Auto)",
                value: 0x01,
            },
            DipChoice {
                label: "Rank F (Hard Auto)",
                value: 0x00,
            },
        ],
        conditional: &[],
    },
    DipOption {
        name: "Coin B",
        mask: 0x30,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "2C/1C",
                value: 0x10,
            },
            DipChoice {
                label: "1C/1C",
                value: 0x30,
            },
            DipChoice {
                label: "2C/3C",
                value: 0x00,
            },
            DipChoice {
                label: "1C/2C",
                value: 0x20,
            },
        ],
        conditional: &[],
    },
    DipOption {
        name: "Demo Sounds",
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
        conditional: &[],
    },
    DipOption {
        name: "Freeze / Rack Test",
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

/// Bonus life with 1, 2 or 3 lives.
const BONUS_LIFE: &[DipChoice] = &[
    DipChoice {
        label: "30K Only",
        value: 0x08,
    },
    DipChoice {
        label: "30K and 80K Only",
        value: 0x30,
    },
    DipChoice {
        label: "30K, 80K, Every 80K",
        value: 0x20,
    },
    DipChoice {
        label: "30K and 100K Only",
        value: 0x38,
    },
    DipChoice {
        label: "30K, 100K, Every 100K",
        value: 0x18,
    },
    DipChoice {
        label: "30K and 120K Only",
        value: 0x28,
    },
    DipChoice {
        label: "30K, 120K, Every 120K",
        value: 0x10,
    },
    DipChoice {
        label: "None",
        value: 0x00,
    },
];

/// Bonus life with 5 lives: the same switches mean different scores.
const BONUS_LIFE_5_LIVES: &[DipChoice] = &[
    DipChoice {
        label: "30K Only",
        value: 0x10,
    },
    DipChoice {
        label: "30K and 100K Only",
        value: 0x38,
    },
    DipChoice {
        label: "30K, 100K, Every 100K",
        value: 0x20,
    },
    DipChoice {
        label: "30K and 120K Only",
        value: 0x30,
    },
    DipChoice {
        label: "40K Only",
        value: 0x08,
    },
    DipChoice {
        label: "40K and 120K Only",
        value: 0x28,
    },
    DipChoice {
        label: "40K, 120K, Every 120K",
        value: 0x18,
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
                label: "3C/1C",
                value: 0x00,
            },
            DipChoice {
                label: "2C/1C",
                value: 0x02,
            },
            DipChoice {
                label: "1C/1C",
                value: 0x07,
            },
            DipChoice {
                label: "2C/3C",
                value: 0x01,
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
                label: "1C/7C",
                value: 0x03,
            },
        ],
        conditional: &[],
    },
    DipOption {
        name: "Bonus Life",
        mask: 0x38,
        apply: DipApplyTiming::Immediate,
        choices: BONUS_LIFE,
        conditional: &[DipCondition {
            mask: 0xC0,
            equals: 0x00,
            choices: BONUS_LIFE_5_LIVES,
        }],
    },
    DipOption {
        name: "Lives",
        mask: 0xC0,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "1",
                value: 0x80,
            },
            DipChoice {
                label: "2",
                value: 0x40,
            },
            DipChoice {
                label: "3",
                value: 0xC0,
            },
            DipChoice {
                label: "5",
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

pub(crate) const SUPERPAC_DIP_BANKS: &[DipSwitchBank] = &[
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

pub const SUPERPAC_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_P1_UP as u16),
        stable_name: "p1_up",
        label: "P1 Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_UP,
    },
    InputControl {
        id: InputId(INPUT_P1_DOWN as u16),
        stable_name: "p1_down",
        label: "P1 Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_DOWN,
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
        label: "P1 Super Speed",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P2_UP as u16),
        stable_name: "p2_up",
        label: "P2 Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(2),
        default_bindings: crate::input_defaults::P2_UP,
    },
    InputControl {
        id: InputId(INPUT_P2_DOWN as u16),
        stable_name: "p2_down",
        label: "P2 Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(2),
        default_bindings: crate::input_defaults::P2_DOWN,
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
        label: "P2 Super Speed",
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

/// Super Pac-Man. Both CPUs sit beside the board, which is their bus.
#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct SuperPacSystem {
    #[debug_cpu("M6809 Main")]
    #[save(id = 1)]
    pub main: M6809,
    #[debug_cpu("M6809 Sound")]
    #[save(id = 2)]
    pub sub: M6809,
    #[debug_bus]
    #[save(id = 3)]
    pub board: SuperPacBoard,
}

impl SuperPacSystem {
    pub fn new() -> Self {
        Self {
            main: M6809::new(),
            sub: M6809::new(),
            board: SuperPacBoard::new(),
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        self.board
            .load_program_rom(&SUPERPAC_PROGRAM_ROM.load(rom_set)?);
        self.board.load_sub_rom(&SUPERPAC_SOUND_ROM.load(rom_set)?);
        self.board.load_tile_rom(&SUPERPAC_TILE_ROM.load(rom_set)?);
        self.board
            .load_sprite_rom(&SUPERPAC_SPRITE_ROM.load(rom_set)?);
        self.board.load_proms(&SUPERPAC_COLOR_PROMS.load(rom_set)?);
        self.board
            .load_sound_prom(&SUPERPAC_SOUND_PROM.load(rom_set)?);
        Ok(())
    }

    /// Active-low: pressing clears the bit.
    fn apply_input(&mut self, button: u8, pressed: bool) {
        let b = &mut self.board;
        match button {
            INPUT_COIN1 => crate::set_bit_active_low(&mut b.in_coins, 0, pressed),
            INPUT_COIN2 => crate::set_bit_active_low(&mut b.in_coins, 1, pressed),
            INPUT_SERVICE => crate::set_bit_active_low(&mut b.in_coins, 3, pressed),
            INPUT_P1_UP => crate::set_bit_active_low(&mut b.in_p1, 0, pressed),
            INPUT_P1_RIGHT => crate::set_bit_active_low(&mut b.in_p1, 1, pressed),
            INPUT_P1_DOWN => crate::set_bit_active_low(&mut b.in_p1, 2, pressed),
            INPUT_P1_LEFT => crate::set_bit_active_low(&mut b.in_p1, 3, pressed),
            INPUT_P2_UP => crate::set_bit_active_low(&mut b.in_p2, 0, pressed),
            INPUT_P2_RIGHT => crate::set_bit_active_low(&mut b.in_p2, 1, pressed),
            INPUT_P2_DOWN => crate::set_bit_active_low(&mut b.in_p2, 2, pressed),
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

impl Default for SuperPacSystem {
    fn default() -> Self {
        Self::new()
    }
}

crate::impl_board_delegation!(SuperPacSystem, board, TIMING, orientation);

impl MachineCore for SuperPacSystem {
    crate::machine_core_metadata!("superpacman", TIMING, crate::superpacman::clock_tree);

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

impl SaveState for SuperPacSystem {
    crate::machine_save_state!();
}

impl Nvram for SuperPacSystem {}
impl Profilable for SuperPacSystem {}

impl InputConfigurable for SuperPacSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        SUPERPAC_CONTROLS
    }
    fn handle_input(&mut self, event: InputEvent) {
        if let InputEvent::Button { id, pressed } = event {
            self.apply_input(id.0 as u8, pressed);
        }
    }
}

crate::impl_dip_switches!(
    SuperPacSystem,
    SUPERPAC_DIP_BANKS,
    board.dsw1,
    board.dsw2,
    board.dsw0
);

crate::impl_board_debug_trace!(SuperPacSystem, board);

crate::register_machine!(
    SuperPacSystem,
    "superpacman",
    &["superpac"],
    SUPERPAC_CONTROLS
);

inventory::submit! {
    DisasmRegion {
        machine: "superpacman",
        region: "main",
        cpu: DisasmCpu::M6809,
        org: 0xC000,
        size: SUPERPAC_PROGRAM_ROM.size as u32,
        load: |rs| SUPERPAC_PROGRAM_ROM.load(rs),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "superpacman",
        region: "sound",
        cpu: DisasmCpu::M6809,
        org: 0xF000,
        size: SUPERPAC_SOUND_ROM.size as u32,
        load: |rs| SUPERPAC_SOUND_ROM.load(rs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::Renderable;

    crate::dip_test_suite!(SuperPacSystem, &[DEFAULT_DSW1, DEFAULT_DSW2, DEFAULT_DSW0]);

    const LINE: u64 = TIMING.cycles_per_scanline;

    /// A system whose two CPUs each spin on `BRA *`, so nothing the test does
    /// is disturbed by a program running out of zeroed memory.
    fn parked() -> SuperPacSystem {
        let mut sys = SuperPacSystem::new();
        let mut rom = vec![0u8; 0x4000];
        rom[0] = 0x20; // BRA *
        rom[1] = 0xFE;
        rom[0x3FFE] = 0xC0; // reset vector 0xC000
        sys.board.load_program_rom(&rom);
        let mut snd = vec![0u8; 0x1000];
        snd[0] = 0x20;
        snd[1] = 0xFE;
        snd[0xFFE] = 0xF0; // reset vector 0xF000
        sys.board.load_sub_rom(&snd);
        sys.reset();
        sys
    }

    /// The LS259 address for output `line` taking value `bit`.
    fn latch_addr(base: u16, line: u16, bit: bool) -> u16 {
        base | (line << 1) | u16::from(bit)
    }

    fn run_to(sys: &mut SuperPacSystem, clock: u64) {
        while sys.board.clock < clock {
            sys.step_cycle();
        }
    }

    fn pixel(sys: &SuperPacSystem, x: usize, row: usize) -> (u8, u8, u8) {
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
        p[32] = 0x0F; // 4E color 0 pixel 0 -> (0xF ^ 0xF) | 0x10 = 16
        p[32 + 4] = 0x0E; // 4E color 1 pixel 0 -> 17
        p[288..].fill(0x0F);
        p
    }

    const RED: (u8, u8, u8) = (255, 0, 0);
    const GREEN: (u8, u8, u8) = (0, 255, 0);

    #[test]
    fn machine_identity_and_geometry() {
        let sys = SuperPacSystem::new();
        assert_eq!(sys.machine_id(), "superpacman");
        assert_eq!(sys.display_size(), (288, 224));
        assert_eq!(sys.orientation(), Orientation::ROT90);
        assert_eq!(TIMING.cycles_per_frame(), 25_344);
        let mut flipped = SuperPacSystem::new();
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
        for a in 0x0400..0x0800u16 {
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
    fn sub_reset_holds_the_sound_cpu_until_raised_then_resets_it() {
        let mut sys = parked();
        sys.board.load_sub_rom(&{
            let mut snd = vec![0u8; 0x1000];
            snd[0x100] = 0x20; // BRA * at 0xF100
            snd[0x101] = 0xFE;
            snd[0xFFE] = 0xF1;
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
    fn the_sound_rom_is_a_2732_mirrored_through_its_2764_socket() {
        let mut sys = parked();
        let snd: Vec<u8> = (0..0x1000).map(|i| (i ^ (i >> 8)) as u8).collect();
        sys.board.load_sub_rom(&snd);
        for a in [0x000u16, 0x123, 0xFFF] {
            assert_eq!(
                sys.bus_read(BusMaster::Cpu(1), 0xE000 + a),
                sys.bus_read(BusMaster::Cpu(1), 0xF000 + a)
            );
        }
        assert_eq!(sys.bus_read(BusMaster::Cpu(1), 0xF123), snd[0x123]);
        // The main CPU never sees it at its translated backing.
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x6123), 0xFF);
    }

    #[test]
    fn the_56xx_pair_reads_the_cabinet_and_both_dip_banks() {
        let mut sys = parked();
        sys.board.dsw1 = 0xC3;
        sys.board.dsw2 = 0x5A;
        sys.board.dsw0 = 0x0B;
        // Chip 1 in mode 9, chip 0 in mode 1; then release 4 RESET.
        sys.bus_write(BusMaster::Cpu(0), 0x4818, 9);
        sys.bus_write(BusMaster::Cpu(0), 0x4808, 1);
        sys.board.run_io();
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x4810), 0xF0, "in reset");
        sys.bus_write(BusMaster::Cpu(0), latch_addr(0x5000, 4, true), 0);
        sys.apply_input(INPUT_P1_LEFT, true);
        sys.board.run_io();
        let n = |sys: &mut SuperPacSystem, a| sys.bus_read(BusMaster::Cpu(0), a) & 0x0F;
        // SW3 through the mux: switches 1-4 with pin 13 low, 5-8 high.
        assert_eq!(n(&mut sys, 0x4810), !0x0Au8 & 0x0F);
        assert_eq!(n(&mut sys, 0x4811), !0x05u8 & 0x0F);
        // SW2 on ports B and C, the test switches on D.
        assert_eq!(n(&mut sys, 0x4812), !0x03u8 & 0x0F);
        assert_eq!(n(&mut sys, 0x4814), !0x0Cu8 & 0x0F);
        assert_eq!(n(&mut sys, 0x4816), !0x0Bu8 & 0x0F);
        // Chip 0, mode 1: P1 left is port B pin 25, bit 3.
        assert_eq!(n(&mut sys, 0x4801), 0x08);
    }

    #[test]
    fn the_palette_dac_includes_each_guns_load() {
        // Per tap: G / (sum of the ladder's G + 1/2.2k), one scale shared by
        // the three guns. Red taps 33.23, 70.71, 151.06; blue 80.63, 172.25.
        let full = superpac_palette(&[0xFF; 32]);
        assert_eq!(full[0], (255, 255, 253), "blue's two taps fall short");
        let bits: Vec<_> = [0x01, 0x02, 0x04, 0x40, 0x80]
            .iter()
            .map(|&v| superpac_palette(&[v; 32])[0])
            .collect();
        assert_eq!(
            bits,
            vec![(33, 0, 0), (71, 0, 0), (151, 0, 0), (0, 0, 81), (0, 0, 172)]
        );
        assert_eq!(superpac_palette(&[0x08; 32])[0], (0, 33, 0));
        assert_eq!(superpac_palette(&[])[31], (0, 0, 0));
    }

    /// One tile at visible row 0, columns 2-3, and sprite slot 0 over its
    /// left eight pixels, with every other slot disabled.
    fn priority_case(tile_attr: u8, sprite_color: u8) -> (u8, u8, u8) {
        let mut v = SuperPacVideo::new();
        v.load_tile_rom(&[0xFF; 0x1000]); // every pixel 3
        v.load_sprite_rom(&[0xFF; 0x2000]);
        let mut p = vec![0u8; 0x220];
        p[0x10] = 0x07; // red
        p[0x1F] = 0x38; // green
        p[0x05] = 0xC0; // blue
        p[0x01] = 0x3F; // yellow
        p[32 + 3] = 0x0F; // char color 0, pixel 3 -> pen 0x10
        p[32 + 7] = 0x00; // char color 1, pixel 3 -> pen 0x1F
        p[288 + 3] = 0x05; // sprite color 0 -> pen 5
        p[288 + 7] = 0x01; // sprite color 1 -> pen 1
        p[288 + 11] = 0x0F; // sprite color 2 -> pen 15, transparent
        v.load_proms(&p);

        let mut vram = vec![0u8; 0x800];
        let off = crate::namco_video::namco_tilemap_offset(2, 0);
        vram[off + 0x400] = tile_attr;
        let mut work = vec![0u8; 0x1800];
        for s in 0..64 {
            work[0x1780 + s * 2 + 1] = 0x02; // disabled
        }
        work[0x1781] = 0x00; // slot 0 enabled
        work[0x0781] = sprite_color;
        work[0x0F80] = 225; // top = ((257 - 225) & 0xFF) - 32 = 0
        work[0x0F81] = 56; // sx = 56 - 40 = 16
        v.render_scanline(0, &vram, &work);
        let mut out = vec![0u8; NATIVE_WIDTH * NATIVE_HEIGHT * 3];
        v.render_frame(&mut out);
        (out[16 * 3], out[16 * 3 + 1], out[16 * 3 + 2])
    }

    #[test]
    fn sprites_cover_ordinary_tiles_and_pen_15_is_transparent() {
        assert_eq!(priority_case(0x00, 0), (0, 0, 253));
        assert_eq!(priority_case(0x00, 2), RED);
    }

    #[test]
    fn high_priority_tiles_cover_sprites_except_where_their_pen_is_31() {
        assert_eq!(priority_case(0x40, 0), RED);
        assert_eq!(priority_case(0x41, 0), (0, 0, 253));
    }

    #[test]
    fn sprite_pens_0_and_1_cover_even_high_priority_tiles() {
        assert_eq!(priority_case(0x40, 1), (255, 255, 0));
    }
}
