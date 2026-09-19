//! Sega Zaxxon-family video engine (Zaxxon 1982, Congo Bongo 1983).
//!
//! # Schematics
//!
//! **This engine has no package of its own, and should not grow one.** It is a
//! shared subsystem rather than a board, so the drawings that cover it are the
//! per-game packages, each recorded in that game's own file: [`crate::zaxxon`]
//! and [`crate::congo_bongo`].
//!
//! One thing here does rest on a drawing: the palette, transcribed in
//! [`docs/schematics/zaxxon-color-dac.md`](../../docs/schematics/zaxxon-color-dac.md)
//! from Zaxxon's `IC Board A 834-0214` sheet 13. The rest does not. In
//! particular the `U53`/`U54`/`U56`/`U74`/`U75` adder references in the
//! background scroll math are carried over from the reference driver's own
//! comments; Zaxxon's `IC Board B` sheet 6 does show that scroll built from
//! 74LS283 adders fed by the vertical counter taps and an 11-bit position from
//! P2, which is the shape those comments describe, but the reference
//! designators on that sheet do not match and were not reconciled.
//!
//! The family is MAME's `sega/zaxxon.cpp`, which is *not* Sega G80: the G80
//! raster and vector boards (`sega/segag80r.cpp`, `sega/segag80v.cpp`, Astro
//! Blaster through Eliminator) are a different machine with encrypted Z80
//! opcodes, a different raster, and the Universal Sound Board. What the two
//! lineages share is a vendor and an era, not a board.
//!
//! What every board in the family has in common, and therefore what lives here:
//!
//! - a 48.66 MHz master clock, a 384x264 raster with 256x224 visible, and a
//!   monitor mounted at ROT90,
//! - three GFX regions decoded the same way: 8x8 2bpp foreground characters,
//!   8x8 3bpp background tiles, and 32x32 3bpp sprites,
//! - a 3-3-2 resistor-DAC color PROM,
//! - a background layer that is *not* a CPU-writable tilemap. Its map is fixed
//!   in `tilemap_dat` ROM, so it is pre-rendered once into a 256x4096 pixmap and
//!   then sampled per row through an isometric skew, which is where the
//!   pseudo-3D look comes from, and
//! - a 256-byte sprite RAM scanned back to front, positioned by the
//!   `find_minimum_x`/`find_minimum_y` line-buffer address math.
//!
//! What the games differ on is carried by [`Variant`]: where a foreground tile
//! gets its color (Congo Bongo has a color RAM, Zaxxon has a second PROM
//! indexed by screen position), and which sprite byte holds the X flip.
//! Everything else that differs (CPU count, memory map, control-latch bit
//! assignments, and the entire sound path) belongs to the board and stays in
//! the per-game file.
//!
//! Video RAM and, on Congo Bongo, color RAM live in the owning board's address
//! space and are passed in by reference at render time, mirroring how
//! [`crate::galaxian_video`] keeps VRAM in the board and the GFX caches on the
//! side.

use phosphor_core::core::machine::{
    ActionRole, Direction, InputControl, InputId, InputKind, Orientation, TimingConfig,
};
use phosphor_core::gfx;
use phosphor_core::gfx::decode::{GfxLayout, decode_gfx};
use phosphor_core::gfx::resistor::compute_resnet_weights;
use phosphor_core::gfx::sprite::{SpriteClip, draw_sprite_row};
use phosphor_macros::Saveable;

// ---------------------------------------------------------------------------
// Timing and geometry
// ---------------------------------------------------------------------------
// Master clock 48.66 MHz; main Z80 = /16 ~= 3.041 MHz; pixel clock = /8 ~= 6.083
// MHz. HTOTAL 384 px -> 192 main-CPU cycles/scanline (the pixel clock is 2x the
// CPU clock). VTOTAL 264 lines; visible Y 16..239 (224 lines), VBLANK at line
// 240. Frame: 192 x 264 = 50688 cycles -> ~59.99 Hz. ROT90 => display is 224x256.

/// The board's master crystal, on every game in the family.
pub const MASTER_CLOCK: u32 = 48_660_000;

pub const NATIVE_WIDTH: usize = 256;
pub const NATIVE_HEIGHT: usize = 240;
pub const VBLANK_END: usize = 16; // first visible scanline
pub const VISIBLE_LINES: u64 = 240; // lines rendered (top VBLANK_END clipped on output)

/// Raster timing, shared by every board in the family.
pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: MASTER_CLOCK as u64 / 16, // 3_041_250
    cycles_per_scanline: 192,
    total_scanlines: 264,
    // Native (pre-orientation) framebuffer: the family declares ROT90 and the
    // frontend rotates centrally, so these are the unrotated dimensions.
    display_width: NATIVE_WIDTH as u32,                  // 256
    display_height: (NATIVE_HEIGHT - VBLANK_END) as u32, // 224
    display_aspect: Some((3, 4)),                        // portrait tube as viewed (after ROT90)
};

/// The foreground/text layer: 32x32 of 8x8 tiles covering the whole raster.
const FG_TILEMAP: gfx::TilemapConfig = gfx::TilemapConfig {
    cols: 32,
    rows: 32,
    tile_width: 8,
    tile_height: 8,
};

/// The pre-rendered background pixmap: 32 tiles wide by 512 tiles tall.
const BG_PIXMAP_WIDTH: usize = 32 * 8;
const BG_PIXMAP_HEIGHT: usize = 512 * 8;

// ---------------------------------------------------------------------------
// GFX layouts
// ---------------------------------------------------------------------------
// All three are MAME `*_planar` layouts with `plane_offsets` LSB-first, i.e.
// MAME's `planeoffset` array reversed (see `gfx_8x8x2_planar`,
// `gfx_8x8x3_planar` and `zaxxon_spritelayout` in `sega/zaxxon.cpp`). They are
// `'static` so both the runtime decode and the gfxview `GfxRegion`s borrow the
// same tables and can never diverge.
//
// The plane offsets are expressed as fractions of the region (MAME's
// `RGN_FRAC`). The text and background regions happen to be the same size on
// every board in the family, so those two layouts are shared statics here; the
// sprite region is not (Zaxxon has three chips, Congo Bongo six), so each game
// declares its own sprite layout from the offset tables below and passes it to
// [`ZaxxonVideo::load_gfx`].

/// Shared pixel offsets for an 8x8 character, for both the 2bpp text layer and
/// the 3bpp background layer.
pub static CHAR_X_OFFSETS: [usize; 8] = [0, 1, 2, 3, 4, 5, 6, 7];
pub static CHAR_Y_OFFSETS: [usize; 8] = [0, 8, 16, 24, 32, 40, 48, 56];

/// Foreground/text: 256 chars, 8x8 2bpp; planes split at the 0x1000 region's
/// midpoint.
pub static TX_GFX_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 0x0800 * 8],
    x_offsets: &CHAR_X_OFFSETS,
    y_offsets: &CHAR_Y_OFFSETS,
    char_increment: CHAR_INCREMENT_8X8,
};

/// Background: 1024 chars, 8x8 3bpp; planes at thirds of the 0x6000 region.
pub static BG_GFX_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 0x2000 * 8, 2 * 0x2000 * 8],
    x_offsets: &CHAR_X_OFFSETS,
    y_offsets: &CHAR_Y_OFFSETS,
    char_increment: CHAR_INCREMENT_8X8,
};

/// Sprites: 32x32 3bpp. Each 8x8 sub-cell is 8 consecutive bytes, laid out
/// left-to-right then top-to-bottom, so
/// `x_offsets[px] = (px/8)*64 + px%8` and
/// `y_offsets[py] = (py/8)*256 + (py%8)*8`.
pub static SPRITE_X_OFFSETS: [usize; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, // sub-cell col 0
    64, 65, 66, 67, 68, 69, 70, 71, // sub-cell col 1
    128, 129, 130, 131, 132, 133, 134, 135, // sub-cell col 2
    192, 193, 194, 195, 196, 197, 198, 199, // sub-cell col 3
];
pub static SPRITE_Y_OFFSETS: [usize; 32] = [
    0, 8, 16, 24, 32, 40, 48, 56, // sub-cell row 0
    256, 264, 272, 280, 288, 296, 304, 312, // sub-cell row 1
    512, 520, 528, 536, 544, 552, 560, 568, // sub-cell row 2
    768, 776, 784, 792, 800, 808, 816, 824, // sub-cell row 3
];

/// A tile's bytes per plane, so a caller sizing a cache does not restate the
/// layout: 8 for an 8x8 character, 128 for a 32x32 sprite.
pub const CHAR_INCREMENT_8X8: usize = 8 * 8;
pub const CHAR_INCREMENT_SPRITE: usize = 128 * 8;

// ---------------------------------------------------------------------------
// Palette
// ---------------------------------------------------------------------------

/// The value the strongest of the three DAC networks is scaled to.
const RGB_MAXIMUM: f64 = 255.0;

/// What a PROM byte of 0xFF resolves to: not white, because the two-bit blue
/// ladder tops out at 247 on the shared scale. Tests across the family plant
/// 0xFF and expect "the brightest color the board can draw", and naming it
/// keeps them from quietly re-asserting that it is white.
#[cfg(test)]
pub(crate) const BRIGHTEST: (u8, u8, u8) = (255, 255, 247);

/// Build the 512-entry RGB palette from a family color PROM.
///
/// 3-3-2 resistor DAC: R = PROM bits 0-2 and G = bits 3-5 (1k/470/220 ohm),
/// B = bits 6-7 (470/220 ohm), each summing into its own 470 ohm pulldown. That
/// is the same network Galaxian uses, and it is built the same way here: each
/// bit's weight is the Thevenin divider with the other bits grounded, and one
/// scale is shared across all three channels.
///
/// **The shared scale is the point, and getting it wrong is invisible.** The
/// two-bit blue ladder has less conductance above its node than the three-bit
/// red and green ones, and all three pulldowns are equal, so blue tops out at
/// 247 rather than 255 and the board's brightest color is slightly warm.
/// Normalizing each channel to its own maximum instead, as this used to, makes
/// blue reach 255 and shifts every mixed color: a byte of 0xF6 came out
/// (201, 201, 255) rather than (222, 222, 247), a lavender where the board
/// draws a near-white. Both look like plausible palettes, which is why this is
/// stated here.
///
/// Read off the drawing rather than taken from a reference driver, because that
/// is what settled it: the resistor values, the equal pulldowns, and the fact
/// that nothing sits between a ladder and the monitor that could give a channel
/// its own gain, are transcribed in
/// [`docs/schematics/zaxxon-color-dac.md`](../../docs/schematics/zaxxon-color-dac.md).
///
/// Only the low 256 PROM bytes are palette on any board in the family. The
/// table is 512 entries because Congo Bongo's CBS color-bank latch adds 0x100
/// to a palette index, and the upper half is the low half repeated, which is
/// what that board's PROM does by being read twice. Zaxxon never sets a color
/// bank, so it only ever reads the low half; its upper 256 PROM bytes are the
/// foreground color codes, not palette, and are kept separately by
/// [`ZaxxonVideo::color_codes`].
pub fn palette_rgb(palette_prom: &[u8]) -> [(u8, u8, u8); 512] {
    let rg_raw = compute_resnet_weights(&[1000.0, 470.0, 220.0], 470.0, RGB_MAXIMUM);
    let b_raw = compute_resnet_weights(&[470.0, 220.0], 470.0, RGB_MAXIMUM);

    // Shared autoscale: the network with the greatest summed output maps to
    // RGB_MAXIMUM, and the weaker two-bit blue network lands below it.
    let max_out = [&rg_raw, &b_raw]
        .iter()
        .map(|w| w.iter().sum::<f64>())
        .fold(0.0_f64, f64::max);
    let scale = RGB_MAXIMUM / max_out;

    let combine = |weights: &[f64], bits: &[u8]| -> u8 {
        let v: f64 = weights
            .iter()
            .zip(bits)
            .map(|(w, &b)| w * scale * b as f64)
            .sum();
        v.round().clamp(0.0, 255.0) as u8
    };

    let mut out = [(0u8, 0u8, 0u8); 512];
    for (i, entry) in out.iter_mut().enumerate() {
        let v = palette_prom[i & 0xFF];
        let r = combine(&rg_raw, &[v & 1, (v >> 1) & 1, (v >> 2) & 1]);
        let g = combine(&rg_raw, &[(v >> 3) & 1, (v >> 4) & 1, (v >> 5) & 1]);
        let b = combine(&b_raw, &[(v >> 6) & 1, (v >> 7) & 1]);
        *entry = (r, g, b);
    }
    out
}

// ---------------------------------------------------------------------------
// Variant
// ---------------------------------------------------------------------------

/// The two things the video engine does differently per game.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Variant {
    /// Zaxxon: a foreground tile's color comes from the second color PROM,
    /// indexed by screen position rather than by anything the CPU wrote, and
    /// the sprite X flip shares byte 1 with the code and the Y flip.
    #[default]
    Zaxxon,
    /// Congo Bongo: a foreground tile's color comes from a CPU-writable color
    /// RAM, the tile code has a bank bit above it, and the sprite X flip moves
    /// to byte 2 so that byte 1 can carry a 7-bit code.
    Congo,
}

impl Variant {
    /// Which sprite RAM byte holds the X flip, and with which mask.
    ///
    /// MAME passes these to `draw_sprites` as a packed `flipxmask`/`flipymask`
    /// where the high byte is the RAM offset and the low byte is the bit:
    /// Zaxxon `0x140`/`0x180`, Congo Bongo `0x280`/`0x180`. The Y flip is byte
    /// 1 bit 7 on both, so only the X flip is per-variant.
    fn sprite_flip_x(self, ram: &[u8; 0x100], offs: usize) -> bool {
        match self {
            Variant::Zaxxon => ram[offs + 1] & 0x40 != 0,
            Variant::Congo => ram[offs + 2] & 0x80 != 0,
        }
    }
}

// ---------------------------------------------------------------------------
// ZaxxonVideo
// ---------------------------------------------------------------------------

/// The family's video hardware: the three GFX regions, the PROM palette, the
/// pre-rendered background pixmap, sprite RAM, and the control-latch lines that
/// steer them.
///
/// The owning board decodes its own 74LS259 latches (whose bit assignments move
/// between games) and pushes the decoded lines in through the setters here, so
/// this struct never has to know which address wrote what.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct ZaxxonVideo {
    /// Which game's rules to apply. Fixed at construction, so not state.
    #[save_skip]
    variant: Variant,

    // GFX ROMs' decoded pixel caches, plus the background map ROM the pixmap is
    // built from. All derived from ROM at load, so none of it is saved.
    #[save_skip]
    tx_cache: gfx::GfxCache, // 8x8 2bpp foreground characters
    #[save_skip]
    bg_cache: gfx::GfxCache, // 8x8 3bpp background tiles
    #[save_skip]
    sprite_cache: gfx::GfxCache, // 32x32 3bpp sprites

    /// Pre-rendered background pixmap (256x4096 palette-pen indices). The map is
    /// fixed in `tilemap_dat` ROM, so it is built once at load.
    #[save_skip]
    bg_pixmap: Vec<u8>,

    /// The 512-entry RGB palette decoded from the color PROM, and the 256
    /// foreground color codes that share that PROM on Zaxxon.
    ///
    /// Expanded from the PROM rather than from anything the CPU writes, so
    /// unlike the boards whose palette lives in RAM it stays derived and is
    /// rebuilt at ROM load.
    #[save_skip]
    palette_rgb: [(u8, u8, u8); 512],
    #[save_skip]
    color_codes: [u8; 256],

    /// Scanline-rendered framebuffer (256 x 240 x RGB24, pre-orientation).
    #[save_skip]
    scanline_buffer: Vec<u8>,

    // Control-latch lines, decoded by the board from its own LS259s.
    #[save(id = 1)]
    bg_enable: bool, // BEN
    #[save(id = 2)]
    bg_color: bool, // CREF3
    #[save(id = 3)]
    fg_color: bool, // CREF1
    #[save(id = 4)]
    fg_bank: bool, // BS, Congo Bongo only
    #[save(id = 5)]
    color_bank: bool, // CBS, Congo Bongo only

    /// Background scroll position, as the two raw bytes the CPU writes. Eleven
    /// bits are stored: all eight of the first, the low three of the second.
    #[save(id = 6)]
    bg_position: [u8; 2],

    /// The 256-byte sprite RAM. It is directly CPU-writable on Zaxxon and
    /// filled by a custom DMA engine on Congo Bongo, so the board owns the
    /// writes and this owns the bytes.
    #[save(id = 7)]
    sprite_ram: [u8; 0x100],
}

impl ZaxxonVideo {
    pub fn new(variant: Variant) -> Self {
        Self {
            variant,
            tx_cache: gfx::GfxCache::new(0, 8, 8),
            bg_cache: gfx::GfxCache::new(0, 8, 8),
            sprite_cache: gfx::GfxCache::new(0, 32, 32),
            bg_pixmap: Vec::new(),
            palette_rgb: [(0, 0, 0); 512],
            color_codes: [0; 256],
            scanline_buffer: vec![0u8; NATIVE_WIDTH * NATIVE_HEIGHT * 3],
            bg_enable: false,
            bg_color: false,
            fg_color: false,
            fg_bank: false,
            color_bank: false,
            bg_position: [0; 2],
            sprite_ram: [0; 0x100],
        }
    }

    // -----------------------------------------------------------------------
    // ROM load: GFX decode, palette, background pixmap
    // -----------------------------------------------------------------------

    /// Decode the three GFX regions and build everything derived from ROM.
    ///
    /// Call once after the board has loaded its ROM set. `tilemap_dat` sizes
    /// itself: the map is the first half of the region and the attributes the
    /// second, and the tile index wraps at that half, so Congo Bongo's 0x4000
    /// region mirrors its 0x2000-entry map twice down the 32x512 grid while
    /// Zaxxon's 0x8000 region fills it exactly once.
    pub fn load_gfx(
        &mut self,
        tx_rom: &[u8],
        bg_rom: &[u8],
        spr_rom: &[u8],
        spr_layout: &GfxLayout<'_>,
        tilemap_dat: &[u8],
        palette_prom: &[u8],
    ) {
        // A `char_increment` is a tile's size in *bits* per plane, so the tile
        // count is one plane's size in bits divided by it: 2 planes for the text
        // layer, 3 for the background and the sprites. Counting from the region
        // size rather than hard-coding it keeps a differently sized ROM set from
        // silently decoding the wrong number of tiles.
        let tx_count = tx_rom.len() / 2 * 8 / CHAR_INCREMENT_8X8;
        let bg_count = bg_rom.len() / 3 * 8 / CHAR_INCREMENT_8X8;
        let spr_count = spr_rom.len() / 3 * 8 / CHAR_INCREMENT_SPRITE;

        self.tx_cache = decode_gfx(tx_rom, 0, tx_count, &TX_GFX_LAYOUT);
        self.bg_cache = decode_gfx(bg_rom, 0, bg_count, &BG_GFX_LAYOUT);
        self.sprite_cache = decode_gfx(spr_rom, 0, spr_count, spr_layout);

        self.palette_rgb = palette_rgb(palette_prom);
        for (i, code) in self.color_codes.iter_mut().enumerate() {
            *code = palette_prom.get(0x100 + i).copied().unwrap_or(0);
        }

        self.build_bg_pixmap(tilemap_dat);
    }

    /// Pre-render the background tilemap into a 256x4096 pixmap of palette pen
    /// indices (`color * 8 + pen`, before the runtime color base is added).
    ///
    /// Per `get_bg_tile_info` (`sega/zaxxon_v.cpp`): the 32x512 tilemap is
    /// filled from `tilemap_dat` where the first half holds the low 8 code bits
    /// and the second half holds the high 2 code bits (`& 3`) plus 4 color bits
    /// (`>> 4`). The tile index wraps at `bytes/2`.
    fn build_bg_pixmap(&mut self, tilemap_dat: &[u8]) {
        if self.bg_cache.count() == 0 || tilemap_dat.is_empty() {
            self.bg_pixmap = Vec::new();
            return;
        }
        let size = tilemap_dat.len() / 2;
        let mut pixmap = vec![0u8; BG_PIXMAP_WIDTH * BG_PIXMAP_HEIGHT];
        for tile_index in 0..(32 * 512) {
            let col = tile_index % 32;
            let row = tile_index / 32;
            let eff = tile_index & (size - 1);
            let attr = tilemap_dat[eff + size];
            let code = tilemap_dat[eff] as usize + 256 * (attr as usize & 3);
            let base_pen = (attr >> 4) as usize * 8;
            for py in 0..8 {
                for px in 0..8 {
                    let pen = self.bg_cache.pixel(code, px, py) as usize;
                    pixmap[(row * 8 + py) * BG_PIXMAP_WIDTH + col * 8 + px] =
                        (base_pen + pen) as u8;
                }
            }
        }
        self.bg_pixmap = pixmap;
    }

    // -----------------------------------------------------------------------
    // Control lines
    // -----------------------------------------------------------------------

    /// BEN: enable the background layer. When low the layer renders black.
    pub fn set_bg_enable(&mut self, on: bool) {
        self.bg_enable = on;
    }
    /// CREF3: select the high half of the background's palette bank.
    pub fn set_bg_color(&mut self, on: bool) {
        self.bg_color = on;
    }
    /// CREF1: select the high half of the foreground's palette bank.
    pub fn set_fg_color(&mut self, on: bool) {
        self.fg_color = on;
    }
    /// BS: the topmost foreground character code bit (Congo Bongo).
    pub fn set_fg_bank(&mut self, on: bool) {
        self.fg_bank = on;
    }
    /// CBS: the topmost bit into the color PROM (Congo Bongo).
    pub fn set_color_bank(&mut self, on: bool) {
        self.color_bank = on;
    }

    /// Write one of the two background scroll bytes. Eleven bits are stored.
    pub fn write_bg_position(&mut self, offset: usize, data: u8) {
        self.bg_position[offset & 1] = data;
    }

    pub fn sprite_ram(&self) -> &[u8; 0x100] {
        &self.sprite_ram
    }
    pub fn sprite_ram_mut(&mut self) -> &mut [u8; 0x100] {
        &mut self.sprite_ram
    }

    /// The decoded control lines, for the debugger and for tests.
    pub fn bg_enable(&self) -> bool {
        self.bg_enable
    }
    pub fn bg_position(&self) -> [u8; 2] {
        self.bg_position
    }

    // -----------------------------------------------------------------------
    // Debug / gfxview accessors
    // -----------------------------------------------------------------------

    pub fn tx_cache(&self) -> &gfx::GfxCache {
        &self.tx_cache
    }
    pub fn bg_cache(&self) -> &gfx::GfxCache {
        &self.bg_cache
    }
    pub fn sprite_cache(&self) -> &gfx::GfxCache {
        &self.sprite_cache
    }
    pub fn palette(&self) -> &[(u8, u8, u8)] {
        &self.palette_rgb
    }
    /// One entry of the decoded RGB palette.
    pub fn palette_color(&self, index: usize) -> (u8, u8, u8) {
        self.palette_rgb[index & 0x1FF]
    }
    /// The pre-rendered background pixmap (256x4096 pen indices), empty until
    /// the GFX ROMs are loaded.
    pub fn bg_pixmap(&self) -> &[u8] {
        &self.bg_pixmap
    }
    /// The native scanline framebuffer (256x240 RGB24, pre-orientation).
    pub fn scanline_buffer(&self) -> &[u8] {
        &self.scanline_buffer
    }
    /// One pixel of the native framebuffer, as the renderer left it.
    pub fn scanline_pixel(&self, x: usize, y: usize) -> (u8, u8, u8) {
        let off = (y * NATIVE_WIDTH + x) * 3;
        (
            self.scanline_buffer[off],
            self.scanline_buffer[off + 1],
            self.scanline_buffer[off + 2],
        )
    }

    // -----------------------------------------------------------------------
    // Scanline rendering
    // -----------------------------------------------------------------------

    /// Render one native screen scanline (`abs_y` = bitmap row 0-239).
    ///
    /// Layer order matches `screen_update_zaxxon`/`screen_update_congo`: the row
    /// is cleared, then the scrolling background, the sprites, and the
    /// foreground tilemap (transparent pen 0) are drawn over one another.
    ///
    /// `color_ram` is only read on [`Variant::Congo`]; Zaxxon passes an empty
    /// slice, because on that board a foreground tile's color comes from the
    /// PROM rather than from anything the CPU wrote.
    pub fn render_scanline(&mut self, abs_y: usize, video_ram: &[u8], color_ram: &[u8]) {
        let row_offset = abs_y * NATIVE_WIDTH * 3;
        self.scanline_buffer[row_offset..row_offset + NATIVE_WIDTH * 3].fill(0);
        self.render_bg_scanline(abs_y);
        self.render_sprites_scanline(abs_y);
        self.render_fg_scanline(abs_y, video_ram, color_ram);
    }

    /// Source pixmap row for screen row `abs_y`, per the U56/U74/U75 adders:
    /// `VF + ((bg_position << 1) ^ 0xfff) + 1`, masked to the pixmap height.
    /// (Upright only; flip-screen VF inversion is deferred.)
    fn bg_src_y(&self, abs_y: usize) -> usize {
        let bgpos = ((self.bg_position[1] as usize & 0x07) << 8) | self.bg_position[0] as usize;
        (abs_y + ((bgpos << 1) ^ 0xfff) + 1) & (BG_PIXMAP_HEIGHT - 1)
    }

    /// Source pixmap column for screen pixel `(x, abs_y)` with the isometric
    /// skew (U53/U54 adders): `HF + ((VF >> 1) ^ 0xff) + 1 + 0x3F`, masked to
    /// the pixmap width. The 0x3F constant is the non-flipped `flipoffs`
    /// (0x40 - 1).
    fn bg_src_x(abs_y: usize, x: usize) -> usize {
        (x + ((abs_y >> 1) ^ 0xff) + 1 + 0x3F) & (BG_PIXMAP_WIDTH - 1)
    }

    /// Draw the pseudo-3D scrolling background for one scanline.
    ///
    /// Samples the pre-built pixmap with the isometric skew (`draw_background`
    /// with `skew = true`) and adds the runtime color base `bg_color (CREF3) +
    /// (color_bank << 8)`. When the layer is disabled the row stays black.
    fn render_bg_scanline(&mut self, abs_y: usize) {
        if !self.bg_enable || self.bg_pixmap.is_empty() {
            return;
        }
        let colorbase = usize::from(self.bg_color) * 0x80 + usize::from(self.color_bank) * 0x100;
        let row_base = self.bg_src_y(abs_y) * BG_PIXMAP_WIDTH;
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

    /// Sprite top scanline from its Y byte (`find_minimum_y`): the first line
    /// where `(Y + 0xf2 + VF) & 0xe0 == 0xe0`, scanned back to its minimum, +1.
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

    /// Draw the sprites covering one scanline (32x32 3bpp, transparent pen 0).
    ///
    /// Only the lower half of sprite RAM is scanned, back to front (offs 0x7C
    /// down to 0) so lower-indexed sprites land on top. Each sprite is
    /// positioned via `find_minimum_x`/`find_minimum_y` and drawn with 256-pixel
    /// X and Y wrap. Per-sprite color = `(byte & 0x1f) + (color_bank << 5)`,
    /// palette pen = `color * 8 + pen`.
    fn render_sprites_scanline(&mut self, abs_y: usize) {
        let count = self.sprite_cache.count();
        if count == 0 {
            return;
        }
        // The code field is whatever the sprite ROM can address: 7 bits on
        // Congo Bongo's six chips, 6 on Zaxxon's three. The bits above it are
        // the flip flags, which the variant decodes.
        let code_mask = (count - 1) as u16;
        let variant = self.variant;
        let color_bank = usize::from(self.color_bank);
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
            let code = ram[offs + 1] as u16 & code_mask;
            let flip_y = ram[offs + 1] & 0x80 != 0;
            let flip_x = variant.sprite_flip_x(ram, offs);
            let color = (ram[offs + 2] & 0x1f) as usize + (color_bank << 5);
            let sx = Self::find_minimum_x(ram[offs + 3], false);

            // Sprite covers `abs_y` from its primary anchor or the -256 Y wrap.
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

    /// Draw the foreground/text tilemap for one scanline (32x32 of 8x8 2bpp
    /// tiles, transparent pen 0).
    ///
    /// The tile code is `videoram[index]`, plus the BS bank bit on Congo Bongo
    /// (`congo_get_fg_tile_info`). The color is where the two games part:
    ///
    /// - Congo Bongo reads `colorram[index] & 0x1f`, five bits the CPU wrote.
    /// - Zaxxon reads `color_codes[col + 32 * (row / 4)] & 0x0f` from the second
    ///   color PROM (`zaxxon_get_fg_tile_info`). The index is screen position,
    ///   not tile content, so the color of a character is fixed by *where* it is
    ///   drawn: the PROM gives the score line, the fuel gauge and the playfield
    ///   their own colors in bands four tile rows tall.
    ///
    /// The gfx color granularity is 8 pens either way, and the whole layer is
    /// then offset by `fg_color (CREF1) + (color_bank << 8)`.
    fn render_fg_scanline(&mut self, abs_y: usize, video_ram: &[u8], color_ram: &[u8]) {
        let tile_count = self.tx_cache.count();
        if tile_count == 0 || video_ram.is_empty() {
            return; // GFX ROMs not loaded yet
        }
        let variant = self.variant;
        let color_codes = &self.color_codes;
        let tiles = &self.tx_cache;
        let palette = &self.palette_rgb;
        let fg_bank = usize::from(self.fg_bank);
        let pal_offset = usize::from(self.fg_color) * 0x80 + usize::from(self.color_bank) * 0x100;

        let buf_start = abs_y * NATIVE_WIDTH * 3;
        let buf = &mut self.scanline_buffer[buf_start..buf_start + NATIVE_WIDTH * 3];

        gfx::render_tilemap_scanline(
            &FG_TILEMAP,
            tiles,
            abs_y,
            |col, row| {
                let idx = row * FG_TILEMAP.cols + col;
                match variant {
                    Variant::Zaxxon => {
                        let attr = color_codes[col + 32 * (row / 4)] & 0x0f;
                        gfx::TileInfo::new(video_ram[idx] as u16, attr)
                    }
                    Variant::Congo => {
                        // The 0x1000 fg ROM only decodes 256 tiles; the fg-bank
                        // high bit has no ROM behind it on this set, so wrap
                        // rather than index past it.
                        let code = (video_ram[idx] as usize + (fg_bank << 8)) % tile_count;
                        gfx::TileInfo::new(code as u16, color_ram[idx] & 0x1f)
                    }
                }
            },
            |attr, pen| {
                // Pen 0 is transparent, which is what lets the background and
                // the sprites drawn before this show through.
                (pen != 0).then(|| {
                    let base = attr as usize * 8 + pal_offset;
                    palette[(base + pen as usize) & 0x1FF]
                })
            },
            buf,
            0,
        );
    }

    // -----------------------------------------------------------------------
    // Frame output (native; ROT90 applied centrally by the frontend)
    // -----------------------------------------------------------------------

    /// Copy the visible raster (rows 16..240, native 256x224 RGB24) into the
    /// output buffer in native row-major order.
    ///
    /// The 90 degree rotation these cabinets need is declared via
    /// [`orientation`](Self::orientation) and applied centrally by the frontend,
    /// so this emits pixels unrotated.
    pub fn render_frame(&self, buffer: &mut [u8]) {
        let start = VBLANK_END * NATIVE_WIDTH * 3;
        let visible =
            &self.scanline_buffer[start..start + (NATIVE_HEIGHT - VBLANK_END) * NATIVE_WIDTH * 3];
        buffer.copy_from_slice(visible);
    }

    /// Every monitor in the family is mounted rotated 90 degrees. The
    /// orientation is declarative: the frontend rotates `render_frame`'s native
    /// output.
    pub fn orientation(&self) -> Orientation {
        Orientation::ROT90
    }

    /// Clear the video state a reset clears. The ROM-derived caches, palette and
    /// pixmap survive, because a reset does not reload ROMs.
    pub fn reset(&mut self) {
        self.bg_enable = false;
        self.bg_color = false;
        self.fg_color = false;
        self.fg_bank = false;
        self.color_bank = false;
        self.bg_position = [0; 2];
        self.sprite_ram = [0; 0x100];
        self.scanline_buffer.fill(0);
    }
}

// ---------------------------------------------------------------------------
// Shared input table
// ---------------------------------------------------------------------------
// Stable input IDs. SW00 = P1 joystick + button, SW01 = P2 (cocktail), SW100 =
// start + coin status. Coins go through a latch/acknowledge path on the board.

pub const INPUT_P1_RIGHT: u16 = 0;
pub const INPUT_P1_LEFT: u16 = 1;
pub const INPUT_P1_UP: u16 = 2;
pub const INPUT_P1_DOWN: u16 = 3;
pub const INPUT_P1_BUTTON: u16 = 4;
pub const INPUT_P2_RIGHT: u16 = 5;
pub const INPUT_P2_LEFT: u16 = 6;
pub const INPUT_P2_UP: u16 = 7;
pub const INPUT_P2_DOWN: u16 = 8;
pub const INPUT_P2_BUTTON: u16 = 9;
pub const INPUT_P1_START: u16 = 10;
pub const INPUT_P2_START: u16 = 11;
pub const INPUT_COIN1: u16 = 12;
pub const INPUT_COIN2: u16 = 13;
pub const INPUT_SERVICE: u16 = 14;

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

/// The control panel every board in the family carries: an 8-way stick and one
/// button per player, two starts, two coin slots and a service credit.
///
/// The games do not agree on which port bit each direction lands on (Zaxxon has
/// down above up on SW00, Congo Bongo has them the other way round), but that is
/// the board's wiring rather than the panel, so it is applied in each game's
/// `handle_input` and the table itself is shared.
pub const ZAXXON_FAMILY_CONTROLS: &[InputControl] = &[
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
        default_bindings: ind::P1_START,
    },
    InputControl {
        id: InputId(INPUT_P2_START),
        stable_name: "p2_start",
        label: "P2 Start",
        kind: InputKind::Start,
        player: Some(2),
        default_bindings: ind::P2_START,
    },
    InputControl {
        id: InputId(INPUT_COIN1),
        stable_name: "coin1",
        label: "Coin 1",
        kind: InputKind::Coin,
        player: None,
        default_bindings: ind::COIN,
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
        default_bindings: ind::SERVICE,
    },
];

// ---------------------------------------------------------------------------
// Shared coin latch
// ---------------------------------------------------------------------------

/// The family's coin inputs, which are latched rather than locked out.
///
/// There is no external coin lockout circuitry: the board latches each coin
/// input and the game has to clear it explicitly by pulsing the matching enable
/// line (bits 0-2 of the first LS259) low. Each input first passes a debounce
/// circuit of an LS175 quad flip-flop and an LS10 3-input NAND, which is not
/// modeled here.
#[derive(Clone, Copy, Debug, Default, Saveable)]
#[save_version(1)]
pub struct CoinLatch {
    /// Coin A, coin B, service.
    pub status: [bool; 3],
}

impl CoinLatch {
    /// Latch a coin insert (`zaxxon_coin_inserted`): the coin registers only
    /// while its arming line (latch bit `n`) is high.
    pub fn insert(&mut self, n: usize, enables: u8) {
        if (enables >> n) & 1 == 1 {
            self.status[n] = true;
        }
    }

    /// Async-clear each latch whose enable line is low. The game acknowledges a
    /// credit by pulsing that line low then high, so this must run on every
    /// write to the latch, not only on the bit that changed.
    pub fn apply_enables(&mut self, enables: u8) {
        for n in 0..3 {
            if (enables >> n) & 1 == 0 {
                self.status[n] = false;
            }
        }
    }

    /// The three coin bits as they appear in the high bits of SW100.
    pub fn sw100_bits(&self) -> u8 {
        (self.status[0] as u8) << 5 | (self.status[1] as u8) << 6 | (self.status[2] as u8) << 7
    }
}

// ---------------------------------------------------------------------------
// Shared DIP coinage table
// ---------------------------------------------------------------------------

/// The 16-position coinage table, shared by both DIP nibbles and by every game
/// in the family (`DSW03` on Zaxxon and Congo Bongo alike).
///
/// `shift` is 0 for the low nibble (coin B) and 4 for the high nibble (coin A).
pub const fn coinage(shift: u8) -> [phosphor_core::core::machine::DipChoice; 16] {
    use phosphor_core::core::machine::DipChoice;
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

pub const COIN_B_CHOICES: [phosphor_core::core::machine::DipChoice; 16] = coinage(0);
pub const COIN_A_CHOICES: [phosphor_core::core::machine::DipChoice; 16] = coinage(4);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A video engine with a one-pixel foreground tile and a one-color palette
    /// entry, enough to check placement without a ROM set.
    fn video_with_gfx(variant: Variant, prom: &mut [u8; 0x200]) -> ZaxxonVideo {
        let mut tx = vec![0u8; 0x1000];
        let mut bg = vec![0u8; 0x6000];
        let mut spr = vec![0u8; 0x6000];
        let tilemap = vec![0u8; 0x8000];
        // Tile/sprite 0, row 0, col 0 set in plane 0 only, so its pen is 1.
        tx[0] = 0b1000_0000;
        bg[0] = 0b1000_0000;
        spr[0] = 0b1000_0000;
        let spr_layout = GfxLayout {
            plane_offsets: &[0, 0x2000 * 8, 2 * 0x2000 * 8],
            x_offsets: &SPRITE_X_OFFSETS,
            y_offsets: &SPRITE_Y_OFFSETS,
            char_increment: CHAR_INCREMENT_SPRITE,
        };
        let mut video = ZaxxonVideo::new(variant);
        video.load_gfx(&tx, &bg, &spr, &spr_layout, &tilemap, prom);
        video
    }

    #[test]
    fn tile_counts_follow_the_region_sizes() {
        let mut prom = [0u8; 0x200];
        let zaxxon = video_with_gfx(Variant::Zaxxon, &mut prom);
        assert_eq!(zaxxon.tx_cache().count(), 256);
        assert_eq!(zaxxon.bg_cache().count(), 1024);
        assert_eq!(
            zaxxon.sprite_cache().count(),
            64,
            "three 8KB sprite ROMs hold 64 sprites, not Congo Bongo's 128"
        );
        assert_eq!(
            (
                zaxxon.sprite_cache().width(),
                zaxxon.sprite_cache().height()
            ),
            (32, 32)
        );
        // Plane 0 only at (0,0) -> pen 1.
        assert_eq!(zaxxon.tx_cache().pixel(0, 0, 0), 1);
    }

    /// The blue channel topping out at 247 rather than 255 is the whole content
    /// of this test, and it is what a per-channel normalization silently gets
    /// wrong. The 0xF6 case is the byte that exposed it against a reference
    /// capture of Zaxxon's attract screen.
    #[test]
    fn palette_is_a_3_3_2_resistor_dac_on_one_shared_scale() {
        let mut prom = [0u8; 0x200];
        prom[1] = 0xFF; // every bit on
        prom[2] = 0x07; // the three red bits only
        prom[3] = 0xF6; // red and green bits 1-2, both blue bits
        let rgb = palette_rgb(&prom);

        assert_eq!(rgb[0], (0, 0, 0));
        assert_eq!(
            rgb[1], BRIGHTEST,
            "every bit on is not white: the two-bit blue ladder has less total \
             conductance than the three-bit red and green ones, and on one \
             shared scale that leaves the board's brightest color slightly warm"
        );
        let (r, g, b) = rgb[2];
        assert_eq!((g, b), (0, 0));
        assert_eq!(r, 255, "all three red bits on gives full red");
        assert_eq!(
            rgb[3],
            (222, 222, 247),
            "both blue bits reach only 247, and red/green bits 1-2 reach 222"
        );

        // Congo Bongo's CBS latch adds 0x100 to a palette index, and reads the
        // same 256 PROM bytes again. Zaxxon's upper PROM half is color codes
        // rather than palette, so this must come from the low half either way.
        prom[0x101] = 0x00;
        let rgb = palette_rgb(&prom);
        assert_eq!(rgb[0x101], BRIGHTEST, "high half mirrors the low");
    }

    #[test]
    fn background_skew_source_coords() {
        let mut video = ZaxxonVideo::new(Variant::Congo);
        // No scroll: srcy = ((0<<1)^0xfff)+1 = 0x1000 & 0xfff = 0; srcx(0) = 0x3f.
        assert_eq!(video.bg_src_y(0), 0);
        assert_eq!(ZaxxonVideo::bg_src_x(0, 0), 0x3F);
        // Successive rows step the skew column left by one every two lines,
        // which is the isometric shear the whole family's look rests on.
        assert_eq!(ZaxxonVideo::bg_src_x(0, 1), 0x40);
        assert_eq!(ZaxxonVideo::bg_src_x(2, 0), 0x3E);

        // 11-bit scroll split across the two position bytes.
        video.write_bg_position(0, 0x10);
        video.write_bg_position(1, 0x01); // bgpos = 0x110
        assert_eq!(video.bg_src_y(0), 0xDE0);
    }

    #[test]
    fn sprite_positioning_helpers_match_the_line_buffer_math() {
        // find_minimum_x (upright) = value + 0xf0, wrapped to 8 bits.
        assert_eq!(ZaxxonVideo::find_minimum_x(0x10, false), 0x00);
        assert_eq!(ZaxxonVideo::find_minimum_x(0x00, false), 0xF0);
        // find_minimum_y returns a value in 0..=0x100 (top scanline + 1).
        let y = ZaxxonVideo::find_minimum_y(0x20, false);
        assert!((0..=0x100).contains(&y));
    }

    /// The one sprite field the two games genuinely disagree about.
    ///
    /// MAME encodes it as a packed mask per game (`0x140` for Zaxxon, `0x280`
    /// for Congo Bongo): byte 1 bit 6 on one board, byte 2 bit 7 on the other.
    /// Get it wrong and sprites mirror horizontally at random, which is subtle
    /// enough on a 32x32 cell to pass a glance, so it is pinned here.
    #[test]
    fn the_sprite_x_flip_bit_moves_between_the_games() {
        let mut ram = [0u8; 0x100];
        ram[1] = 0x40; // byte 1 bit 6
        ram[2] = 0x00;
        assert!(Variant::Zaxxon.sprite_flip_x(&ram, 0));
        assert!(!Variant::Congo.sprite_flip_x(&ram, 0));

        ram[1] = 0x00;
        ram[2] = 0x80; // byte 2 bit 7
        assert!(!Variant::Zaxxon.sprite_flip_x(&ram, 0));
        assert!(Variant::Congo.sprite_flip_x(&ram, 0));
    }

    /// Zaxxon colors a foreground tile by *where* it sits, not by what it is:
    /// the second color PROM is indexed by `col + 32 * (row / 4)`, so a color
    /// band is four tile rows tall. Congo Bongo reads a color RAM instead.
    #[test]
    fn foreground_color_comes_from_the_prom_on_zaxxon_and_ram_on_congo() {
        let mut prom = [0u8; 0x200];
        // Palette entry for color 2, pen 1: 2 * 8 + 1 = 17.
        prom[17] = 0xFF;
        // Color code for tile column 0 of row-group 0, and for row-group 1.
        prom[0x100] = 0x02;
        prom[0x100 + 32] = 0x00;

        let mut video = video_with_gfx(Variant::Zaxxon, &mut prom);
        let video_ram = [0u8; 0x400];
        video.render_scanline(0, &video_ram, &[]);
        assert_eq!(
            video.scanline_pixel(0, 0),
            BRIGHTEST,
            "row 0 takes color 2 from the PROM"
        );

        // Row 4 is the next row-group, whose PROM byte is color 0, so pen 1
        // there resolves at palette entry 1, which is black.
        video.render_scanline(32, &video_ram, &[]);
        assert_eq!(
            video.scanline_pixel(0, 32),
            (0, 0, 0),
            "the color band changes every four tile rows"
        );

        // The same tile on Congo Bongo takes its color from color RAM.
        let mut video = video_with_gfx(Variant::Congo, &mut prom);
        let mut color_ram = [0u8; 0x400];
        color_ram[0] = 0x02;
        video.render_scanline(0, &video_ram, &color_ram);
        assert_eq!(video.scanline_pixel(0, 0), BRIGHTEST);
    }

    #[test]
    fn the_family_control_table_is_complete_and_uniquely_named() {
        let names: Vec<_> = ZAXXON_FAMILY_CONTROLS
            .iter()
            .map(|c| c.stable_name)
            .collect();
        for expected in ["p1_right", "p1_button", "p2_down", "coin1", "service"] {
            assert!(names.contains(&expected), "missing {expected}");
        }
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len(), "duplicate stable name");
    }

    #[test]
    fn a_coin_latches_only_while_its_enable_line_is_high() {
        let mut coins = CoinLatch::default();
        // Not armed: the insert is dropped on the floor.
        coins.insert(0, 0x00);
        assert_eq!(coins.sw100_bits(), 0);

        // Armed on bit 0 -> latches into SW100 bit 5.
        coins.insert(0, 0x01);
        assert_eq!(coins.sw100_bits(), 0x20);

        // The game acknowledges by pulsing that one enable low, which must clear
        // this latch and leave the others alone.
        coins.insert(1, 0x02);
        coins.apply_enables(0x02);
        assert_eq!(coins.sw100_bits(), 0x40, "coin A cleared, coin B kept");
    }

    #[test]
    fn the_coinage_nibbles_are_the_same_table_shifted() {
        assert_eq!(COIN_B_CHOICES.len(), 16);
        for (b, a) in COIN_B_CHOICES.iter().zip(COIN_A_CHOICES.iter()) {
            assert_eq!(b.label, a.label);
            assert_eq!(b.value << 4, a.value, "{}", b.label);
        }
    }
}
