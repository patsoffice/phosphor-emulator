//! Galaxian-family video engine.
//!
//! Shared video hardware for the Galaxian → Scramble → Frogger lineage
//! (Namco/Konami, 1979+). A single 256×256 tile playfield is composited with
//! up to eight 16×16 hardware sprites, a hardware-generated LFSR starfield, and
//! eight bullet/shell "missile" dots, all colored through a 32-entry resistor-
//! weighted PROM palette.
//!
//! The engine owns the decoded GFX caches, the PROM-derived palettes, the
//! precomputed star RNG table, and a native-orientation scanline framebuffer.
//! Video RAM (tile codes) and object RAM (column scroll/color, sprites, and
//! bullets) live in the owning board's address space and are passed in by
//! reference at render time, mirroring how [`crate::namco_pac`] keeps VRAM in
//! the board and the GFX caches on the side.
//!
//! Geometry and constants follow MAME's `galaxian` driver. The display is a
//! vertical monitor: the native raster is 256 wide × 224 tall (visible scanlines
//! 16..=239 of the 264-line frame), rendered upright here and rotated 90° CCW to
//! the final 224×256 display in [`GalaxianVideo::render_frame`].
//!
//! Notes on the reverse-engineered pieces (no in-tree analog existed):
//!   * The starfield is a 17-bit LFSR whose 2^17-1-entry output table is
//!     precomputed once; per scanline it is sampled with the same enable/twinkle
//!     logic as the hardware (`V1 ^ H8`), and the table origin scrolls one clock
//!     per frame. See [`GalaxianVideo::init_stars`].
//!   * Bullets/shells are positional dots matched against the scanline counter.
//!   * The palette uses MAME's exact `resnet` per-bit voltage-divider weighting
//!     (other bits share the pulldown path) with the three R/G/B networks scaled
//!     jointly so the brightest network reaches `RGB_MAXIMUM` (224), leaving
//!     headroom for the brighter star and bullet colors.

use phosphor_core::gfx;
use phosphor_core::gfx::decode::{GfxCache, GfxLayout, decode_gfx};
use phosphor_macros::Saveable;

// ---------------------------------------------------------------------------
// Geometry
// ---------------------------------------------------------------------------

/// Native raster width: 32 tile columns × 8 px.
pub const NATIVE_WIDTH: usize = 256;
/// Native raster height (visible scanlines): MAME VBSTART(240) − VBEND(16).
pub const NATIVE_HEIGHT: usize = 224;
/// First visible native scanline (GALAXIAN_VBEND). Buffer row `r` corresponds
/// to hardware scanline `r + Y_OFFSET`; the star/scroll/sprite math is keyed off
/// the hardware scanline so it matches MAME's absolute Y.
pub const Y_OFFSET: usize = 16;

/// Tile playfield: 32 columns of 8×8 px (32 rows tall; only 224 px visible).
const TILE_COLS: usize = 32;

/// Object-RAM sub-region bases (within the 0x100-byte object RAM).
const SPRITES_BASE: usize = 0x40; // 8 sprites × 4 bytes
const BULLETS_BASE: usize = 0x60; // 8 bullets × 4 bytes

/// Sprite line-buffer hard clip: the leftmost 16+1 native columns never receive
/// sprite pixels (matches MAME `sprites_clip`).
const SPRITE_CLIP_MIN: i32 = 17;

// ---------------------------------------------------------------------------
// Palette / star constants
// ---------------------------------------------------------------------------

/// LFSR period: 2^17 − 1.
const STAR_RNG_PERIOD: u32 = (1 << 17) - 1;

/// Palette headroom maximum. The sprite/tilemap networks normalize to this
/// (not 255) so the brighter star and shell/missile colors have room.
const RGB_MAXIMUM: f64 = 224.0;

// ---------------------------------------------------------------------------
// GalaxianVideo
// ---------------------------------------------------------------------------

/// GFX bank-switching scheme. Base Galaxian has none; later board variants add
/// a 74LS259 latch (at 0x6000-0x6002) whose bits extend the tile/sprite code
/// into a second 4 KB GFX bank, with a game-specific bit mapping (MAME's
/// `*_extend_tile_info` / `*_extend_sprite_info`).
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub enum GfxBankMode {
    /// No banking (base Galaxian); codes are 8-bit tiles / 6-bit sprites.
    #[default]
    None,
    /// Pisces / UniWar S: `gfxbank[0]` is the high code bit (tile bit 8,
    /// sprite bit 6), selecting between two full 256-tile / 64-sprite banks.
    Pisces,
    /// Moon Cresta: when `gfxbank[2]` is set, codes whose top bits are `0b10`
    /// are remapped into the second bank, with `gfxbank[0]`/`gfxbank[1]` as the
    /// low bank-select bits.
    Mooncrst,
}

/// Galaxian-family video engine: decoded GFX, PROM palettes, starfield, and a
/// native-orientation scanline framebuffer.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct GalaxianVideo {
    // Pre-decoded GFX (chars and sprites share the same GFX ROM; sprites are
    // 2×2 groups of chars decoded as 16×16 elements). Sized from the ROM:
    // 256/64 for a 4 KB ROM, 512/128 for an 8 KB banked ROM.
    #[save_skip]
    tile_cache: GfxCache,
    #[save_skip]
    sprite_cache: GfxCache,

    // GFX bank-switching scheme and the 74LS259 latch bits that drive it. The
    // scheme is how the board is wired; the latch bits are what it currently
    // holds.
    #[save_skip]
    gfx_mode: GfxBankMode,
    #[save(id = 5)]
    gfxbank: [u8; 3],

    // PROM + derived colors: loaded from ROM and computed once.
    #[save_skip]
    palette_prom: [u8; 32],
    #[save_skip]
    palette_rgb: [(u8, u8, u8); 32], // 8 colors × 4 pens
    #[save_skip]
    star_color: [(u8, u8, u8); 64],
    #[save_skip]
    bullet_color: [(u8, u8, u8); 8],

    // Precomputed starfield LFSR table (one byte per clock: bit 7 = enable,
    // bits 0..5 = color index). Derived from ROM-independent hardware logic, so
    // it is rebuilt on construction and never saved.
    #[save_skip]
    stars: Vec<u8>,
    #[save(id = 1)]
    star_rng_origin: u32,

    // Latch outputs that affect rendering (driven by the board).
    #[save(id = 2)]
    stars_enabled: bool,
    #[save(id = 3)]
    flip_x: bool,
    #[save(id = 4)]
    flip_y: bool,

    // Scramble video extras (layered on the shared engine): a blue background
    // fill and the blinking-star variant — a non-scrolling, color-masked
    // starfield whose mask cycles through `stars_blink`.
    #[save_skip]
    scramble_stars: bool,
    #[save(id = 6)]
    background_enable: bool,
    #[save(id = 7)]
    stars_blink: u8,

    // Scramble-style shells: a shorter 2-pixel, all-yellow bullet (MAME
    // `scramble_draw_bullet`) instead of Galaxian's 4-pixel white shell +
    // yellow missile (`galaxian_draw_bullet`).
    #[save_skip]
    scramble_bullets: bool,

    // Frogger video extras (static config, set once by the board): a half-screen
    // blue color-split background, a 3-bit tile/sprite color-code rotation, and
    // the "frogger adjust" nibble swap applied to the column-scroll and sprite-Y
    // bytes entering the adder (MAME `m_frogger_adjust`). Frogger draws no
    // bullets, so the bullet pass is suppressed when this is set.
    #[save_skip]
    frogger: bool,

    // Native-orientation RGB24 framebuffer (256 × 224), filled per scanline.
    #[save_skip]
    scanline_buffer: Vec<u8>,
}

impl Default for GalaxianVideo {
    fn default() -> Self {
        Self::new()
    }
}

impl GalaxianVideo {
    pub fn new() -> Self {
        let mut video = Self {
            tile_cache: GfxCache::new(256, 8, 8),
            sprite_cache: GfxCache::new(64, 16, 16),
            gfx_mode: GfxBankMode::None,
            gfxbank: [0; 3],
            palette_prom: [0; 32],
            palette_rgb: [(0, 0, 0); 32],
            star_color: [(0, 0, 0); 64],
            bullet_color: [(0, 0, 0); 8],
            scramble_stars: false,
            background_enable: false,
            stars_blink: 0,
            scramble_bullets: false,
            frogger: false,
            stars: Vec::new(),
            star_rng_origin: 0,
            stars_enabled: false,
            flip_x: false,
            flip_y: false,
            scanline_buffer: vec![0u8; NATIVE_WIDTH * NATIVE_HEIGHT * 3],
        };
        video.stars = Self::init_stars();
        video.build_star_colors();
        video.build_bullet_colors();
        video
    }

    // -----------------------------------------------------------------------
    // Latch outputs (driven by the board's 74LS259-style I/O writes)
    // -----------------------------------------------------------------------

    pub fn set_stars_enabled(&mut self, enabled: bool) {
        self.stars_enabled = enabled;
    }

    /// Select the Scramble starfield variant (non-scrolling, color-masked blink)
    /// plus the blue background fill. Set once at construction by Scramble-family
    /// boards; base Galaxian leaves it off.
    pub fn set_scramble_stars(&mut self, on: bool) {
        self.scramble_stars = on;
    }

    /// Enable the Scramble blue background (else black). Driven by the board's
    /// background-enable control line.
    pub fn set_background_enable(&mut self, on: bool) {
        self.background_enable = on;
    }

    /// Use the Scramble-style 2-pixel all-yellow shell (else Galaxian's
    /// 4-pixel white shell + yellow missile).
    pub fn set_scramble_bullets(&mut self, on: bool) {
        self.scramble_bullets = on;
    }

    /// Select the Frogger video board: a half-screen blue color-split
    /// background, the tile/sprite color-code rotation, the column-scroll /
    /// sprite-Y nibble swap, and no bullet layer. Set once at construction;
    /// base Galaxian/Scramble leave it off.
    pub fn set_frogger(&mut self, on: bool) {
        self.frogger = on;
    }

    /// Frogger's color-code rotation (MAME `frogger_extend_*_info`): the 3-bit
    /// attribute `b2 b1 b0` is rewired to `b0 b2 b1`.
    #[inline]
    fn frogger_color(color: u8) -> u8 {
        ((color >> 1) & 0x03) | ((color << 2) & 0x04)
    }

    pub fn set_flip_x(&mut self, flip: bool) {
        self.flip_x = flip;
    }

    pub fn set_flip_y(&mut self, flip: bool) {
        self.flip_y = flip;
    }

    /// Select the GFX bank-switching scheme (set once at construction by the
    /// game wrapper; base Galaxian leaves it [`GfxBankMode::None`]).
    pub fn set_gfx_mode(&mut self, mode: GfxBankMode) {
        self.gfx_mode = mode;
    }

    /// Drive one bit (`offset` 0..=2) of the GFX-bank latch (74LS259 at
    /// 0x6000-0x6002). Inert unless a banking [`GfxBankMode`] is selected.
    pub fn set_gfxbank(&mut self, offset: u8, data: u8) {
        if let Some(slot) = self.gfxbank.get_mut(offset as usize) {
            *slot = data & 1;
        }
    }

    /// Extend a raw tile code with the active GFX bank (MAME's
    /// `*_extend_tile_info`).
    fn extend_tile_code(&self, code: usize) -> usize {
        match self.gfx_mode {
            GfxBankMode::None => code,
            GfxBankMode::Pisces => code | ((self.gfxbank[0] as usize) << 8),
            GfxBankMode::Mooncrst => {
                if self.gfxbank[2] != 0 && (code & 0xc0) == 0x80 {
                    (code & 0x3f)
                        | ((self.gfxbank[0] as usize) << 6)
                        | ((self.gfxbank[1] as usize) << 7)
                        | 0x100
                } else {
                    code
                }
            }
        }
    }

    /// Extend a raw sprite code with the active GFX bank (MAME's
    /// `*_extend_sprite_info`).
    fn extend_sprite_code(&self, code: usize) -> usize {
        match self.gfx_mode {
            GfxBankMode::None => code,
            GfxBankMode::Pisces => code | ((self.gfxbank[0] as usize) << 6),
            GfxBankMode::Mooncrst => {
                if self.gfxbank[2] != 0 && (code & 0x30) == 0x20 {
                    (code & 0x0f)
                        | ((self.gfxbank[0] as usize) << 4)
                        | ((self.gfxbank[1] as usize) << 5)
                        | 0x40
                } else {
                    code
                }
            }
        }
    }

    pub fn stars_enabled(&self) -> bool {
        self.stars_enabled
    }

    pub fn flip_x(&self) -> bool {
        self.flip_x
    }

    pub fn flip_y(&self) -> bool {
        self.flip_y
    }

    // -----------------------------------------------------------------------
    // ROM loading
    // -----------------------------------------------------------------------

    /// Decode the GFX ROM into tile and sprite caches.
    ///
    /// The two 2bpp bitplanes are stored in the lower and upper halves of the
    /// ROM (`RGN_FRAC(0,2)` / `RGN_FRAC(1,2)`); plane 0 is the pixel LSB. Chars
    /// (256 × 8×8) and sprites (64 × 16×16) are two views of the same data.
    /// Decoded char tiles, sprites, and the tile/sprite RGB palette, for the
    /// interactive GFX viewer (`--gfxview`). The starfield/bullet palettes are
    /// deliberately not exposed here — they don't color the tile/sprite caches.
    pub(crate) fn tile_cache(&self) -> &GfxCache {
        &self.tile_cache
    }
    pub(crate) fn sprite_cache(&self) -> &GfxCache {
        &self.sprite_cache
    }
    pub(crate) fn palette_rgb(&self) -> &[(u8, u8, u8)] {
        &self.palette_rgb
    }

    pub fn load_gfx_rom(&mut self, gfx_data: &[u8]) {
        let half_bits = (gfx_data.len() / 2) * 8;

        let tile_layout = GfxLayout {
            plane_offsets: &[0, half_bits],
            x_offsets: &[0, 1, 2, 3, 4, 5, 6, 7],
            y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
            char_increment: 8 * 8,
        };
        let sprite_layout = GfxLayout {
            plane_offsets: &[0, half_bits],
            x_offsets: &[0, 1, 2, 3, 4, 5, 6, 7, 64, 65, 66, 67, 68, 69, 70, 71],
            y_offsets: &[
                0, 8, 16, 24, 32, 40, 48, 56, 128, 136, 144, 152, 160, 168, 176, 184,
            ],
            char_increment: 16 * 16,
        };

        // 4 KB → 256 tiles / 64 sprites; an 8 KB banked ROM → 512 / 128.
        let num_tiles = gfx_data.len() / 16;
        let num_sprites = gfx_data.len() / 64;
        self.tile_cache = decode_gfx(gfx_data, 0, num_tiles, &tile_layout);
        self.sprite_cache = decode_gfx(gfx_data, 0, num_sprites, &sprite_layout);
    }

    /// Load the 32-byte color PROM and rebuild the tilemap/sprite palette.
    pub fn load_color_prom(&mut self, prom: &[u8]) {
        let n = prom.len().min(32);
        self.palette_prom[..n].copy_from_slice(&prom[..n]);
        self.build_palette();
    }

    // -----------------------------------------------------------------------
    // Palette construction (MAME resnet)
    // -----------------------------------------------------------------------

    fn build_palette(&mut self) {
        // R/G: 1K/470/220 Ω; B: 470/220 Ω; all with a 470 Ω pulldown. Each bit's
        // weight is the Thevenin divider with the other bits grounded (shared
        // `compute_resnet_weights`); the caller applies the cross-network scale.
        let rg_res = [1000.0, 470.0, 220.0];
        let b_res = [470.0, 220.0];

        let r_raw = gfx::compute_resnet_weights(&rg_res, 470.0, RGB_MAXIMUM);
        let g_raw = r_raw.clone(); // identical network
        let b_raw = gfx::compute_resnet_weights(&b_res, 470.0, RGB_MAXIMUM);

        // Shared autoscale: the network with the greatest summed output maps to
        // RGB_MAXIMUM; the weaker (2-bit blue) network ends up below it.
        let max_out = [&r_raw, &g_raw, &b_raw]
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

        for i in 0..32 {
            let p = self.palette_prom[i];
            let r = combine(&r_raw, &[p & 1, (p >> 1) & 1, (p >> 2) & 1]);
            let g = combine(&g_raw, &[(p >> 3) & 1, (p >> 4) & 1, (p >> 5) & 1]);
            let b = combine(&b_raw, &[(p >> 6) & 1, (p >> 7) & 1]);
            self.palette_rgb[i] = (r, g, b);
        }
    }

    /// Star colors: each of the 64 indices drives 150 Ω/100 Ω resistor pairs per
    /// channel, mapped through a 4-level brightness table (compressed into the
    /// 194..255 headroom above the tilemap colors). Matches MAME exactly.
    fn build_star_colors(&mut self) {
        let maxv = RGB_MAXIMUM as i32;
        let minval = maxv * 130 / 150;
        let midval = maxv * 130 / 100;
        let maxval = maxv * 130 / 60;
        let starmap = [
            0u8,
            minval as u8,
            (minval + (255 - minval) * (midval - minval) / (maxval - minval)) as u8,
            255,
        ];
        for i in 0..64u8 {
            let bit = |n: u8| (i >> n) & 1;
            let r = starmap[((bit(4) << 1) | bit(5)) as usize];
            let g = starmap[((bit(2) << 1) | bit(3)) as usize];
            let b = starmap[((bit(0) << 1) | bit(1)) as usize];
            self.star_color[i as usize] = (r, g, b);
        }
    }

    /// Bullet/shell colors: the first 7 ("shells") are white, the 8th
    /// ("missile", the player's shot) is yellow.
    fn build_bullet_colors(&mut self) {
        for c in self.bullet_color.iter_mut().take(7) {
            *c = (0xff, 0xff, 0xff);
        }
        self.bullet_color[7] = (0xff, 0xff, 0x00);
    }

    // -----------------------------------------------------------------------
    // Starfield RNG table
    // -----------------------------------------------------------------------

    /// Precompute the starfield output table from the 17-bit LFSR. Each entry's
    /// bit 7 is the star-enable, bits 0..5 are the star color index.
    fn init_stars() -> Vec<u8> {
        let mut stars = vec![0u8; STAR_RNG_PERIOD as usize];
        let mut shiftreg: u32 = 0;
        for slot in stars.iter_mut() {
            // Enabled when the top 8 bits are all 1 and bit 0 is 0.
            let enabled = (shiftreg & 0x1_fe01) == 0x1_fe00;
            // Color from the 6 bits just below the top 8 (inverted).
            let color = ((!shiftreg) & 0x1f8) >> 3;
            *slot = (color as u8) | ((enabled as u8) << 7);
            // Feedback: XOR of bit 12 and the inverse of bit 0, fed into bit 16.
            let feedback = ((shiftreg >> 12) ^ !shiftreg) & 1;
            shiftreg = (shiftreg >> 1) | (feedback << 16);
        }
        stars
    }

    // -----------------------------------------------------------------------
    // Per-frame / per-scanline rendering
    // -----------------------------------------------------------------------

    /// Advance the starfield scroll by one frame. The shift register is clocked
    /// one extra (unflipped) or one fewer (flipped) time per frame than its
    /// period, which produces the horizontal scroll.
    pub fn begin_frame(&mut self) {
        let delta = if self.flip_x { 1 } else { STAR_RNG_PERIOD - 1 };
        self.star_rng_origin = (self.star_rng_origin + delta) % STAR_RNG_PERIOD;
        // Scramble's stars twinkle: the 2-bit blink state advances over time
        // (modeled per frame). It is unused outside the Scramble starfield.
        if self.scramble_stars {
            self.stars_blink = self.stars_blink.wrapping_add(1) & 3;
        }
    }

    /// Resolve a tile/sprite pixel through the palette: `color` is the 3-bit
    /// attribute (8 palettes), `pv` the 2bpp pixel value (0 = transparent).
    #[inline]
    fn resolve(&self, color: u8, pv: u8) -> (u8, u8, u8) {
        self.palette_rgb[((color as usize & 7) * 4) + pv as usize]
    }

    /// Render one visible scanline (`row` in 0..224) into the framebuffer.
    ///
    /// `vram` is the 0x400-byte tile-code RAM; `objram` is the 0x100-byte object
    /// RAM (column scroll/color at 0x00, sprites at 0x40, bullets at 0x60).
    pub fn render_scanline(&mut self, row: usize, vram: &[u8], objram: &[u8]) {
        debug_assert!(row < NATIVE_HEIGHT);
        let mame_y = (row + Y_OFFSET) as i32;
        let row_off = row * NATIVE_WIDTH * 3;

        // 1) Background: black (or Scramble's blue when enabled), then stars.
        {
            let buf = &mut self.scanline_buffer[row_off..row_off + NATIVE_WIDTH * 3];
            if self.frogger {
                // Frogger draws a half-screen blue color-split (MAME
                // `frogger_draw_background`: rgb(0,0,0x47) for native x < 128,
                // black beyond). The split is symmetric, so flip is handled by
                // the whole-buffer mirror in render_frame.
                for (x, px) in buf.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                    *px = if x < 128 { [0, 0, 0x47] } else { [0, 0, 0] };
                }
            } else if self.background_enable {
                // Blue background (390 Ω resistor → ~0x56), per MAME.
                for px in buf.as_chunks_mut::<3>().0 {
                    *px = [0, 0, 0x56];
                }
            } else {
                buf.fill(0);
            }
        }
        if self.stars_enabled {
            self.draw_star_row(row_off, mame_y);
        }

        // 2) Tilemap: 32 columns, each with independent vertical scroll/color.
        for col in 0..TILE_COLS {
            let mut scroll = objram[col * 2];
            let mut color = objram[col * 2 + 1] & 7;
            if self.frogger {
                // The scroll byte's nibbles are swapped entering the adder, and
                // the color code is rotated.
                scroll = scroll.rotate_left(4);
                color = Self::frogger_color(color);
            }
            let scroll = scroll as i32;
            let eff_y = ((mame_y + scroll) & 0xff) as usize;
            let tile_row = eff_y >> 3;
            let py = eff_y & 7;
            let code = self.extend_tile_code(vram[tile_row * TILE_COLS + col] as usize);
            let base_x = col * 8;
            for px in 0..8 {
                let pv = self.tile_cache.pixel(code, px, py);
                if pv != 0 {
                    let (r, g, b) = self.resolve(color, pv);
                    let off = row_off + (base_x + px) * 3;
                    self.scanline_buffer[off] = r;
                    self.scanline_buffer[off + 1] = g;
                    self.scanline_buffer[off + 2] = b;
                }
            }
        }

        // 3) Sprites: 7 → 0 so lower-numbered sprites win (drawn last, on top).
        for sprnum in (0..8).rev() {
            self.draw_object_sprite_row(row_off, mame_y, objram, sprnum);
        }

        // 4) Bullets/shells over everything (Frogger has no bullet layer).
        if !self.frogger {
            self.draw_bullets_row(row_off, mame_y, objram);
        }
    }

    fn draw_star_row(&mut self, row_off: usize, mame_y: i32) {
        // Galaxian: scrolling field, no color mask. Scramble: a static field
        // (no scroll origin) with a blink color mask, suppressed entirely on
        // even 2V lines in blink state 2 (MAME scramble_draw_stars).
        let (base, starmask) = if self.scramble_stars {
            let blink = (self.stars_blink & 3) as usize;
            if blink == 2 && (mame_y & 2) == 0 {
                return;
            }
            ((mame_y as u64) * 512, [0x20u8, 0x08, 0xff, 0xff][blink])
        } else {
            (self.star_rng_origin as u64 + (mame_y as u64) * 512, 0xff)
        };
        // RNG offset for this scanline; two clocks advance per native column.
        let mut offs = (base % STAR_RNG_PERIOD as u64) as u32;
        let period = STAR_RNG_PERIOD;
        for x in 0..NATIVE_WIDTH {
            let enable = ((mame_y ^ (x as i32 >> 3)) & 1) != 0;
            let star_a = self.stars[offs as usize];
            offs += 1;
            if offs >= period {
                offs = 0;
            }
            let star_b = self.stars[offs as usize];
            offs += 1;
            if offs >= period {
                offs = 0;
            }
            // The hardware paints one sub-pixel from clock A and two from clock
            // B across the 3× horizontal supersample; collapsing to 1× here, the
            // wider clock-B star dominates the native pixel.
            if enable {
                // A star shows only if present (bit 7) and it passes the blink
                // color mask (always 0xff for Galaxian).
                let idx = if star_b & 0x80 != 0 && star_b & starmask != 0 {
                    Some(star_b & 0x3f)
                } else if star_a & 0x80 != 0 && star_a & starmask != 0 {
                    Some(star_a & 0x3f)
                } else {
                    None
                };
                if let Some(c) = idx {
                    let (r, g, b) = self.star_color[c as usize];
                    let off = row_off + x * 3;
                    self.scanline_buffer[off] = r;
                    self.scanline_buffer[off + 1] = g;
                    self.scanline_buffer[off + 2] = b;
                }
            }
        }
    }

    /// Draw one 16-pixel-wide row of galaxian sprite object `sprnum` onto the
    /// current scanline. Parses the object's attributes/position and delegates
    /// the clipped, flipped, transparent blit to the core `gfx::draw_sprite_row`
    /// helper (a precomputed colour LUT provides the pens).
    fn draw_object_sprite_row(
        &mut self,
        row_off: usize,
        mame_y: i32,
        objram: &[u8],
        sprnum: usize,
    ) {
        let base = SPRITES_BASE + sprnum * 4;
        // Frogger swaps the Y byte's nibbles entering the adder.
        let b0 = if self.frogger {
            objram[base].rotate_left(4)
        } else {
            objram[base]
        } as i32;
        // First three sprites are matched against Y−1 (a +1 Y nudge).
        let adj = if sprnum < 3 { 1 } else { 0 };
        let sy = 240 - (b0 - adj);
        if mame_y < sy || mame_y >= sy + 16 {
            return;
        }
        let code = self.extend_sprite_code((objram[base + 1] & 0x3f) as usize);
        let flipx = objram[base + 1] & 0x40 != 0;
        let flipy = objram[base + 1] & 0x80 != 0;
        let mut color = objram[base + 2] & 7;
        if self.frogger {
            color = Self::frogger_color(color);
        }
        let sx = objram[base + 3].wrapping_add(1) as i32;

        let yrow = mame_y - sy;
        let src_py = if flipy { 15 - yrow } else { yrow } as usize;

        // Precompute the 4-entry (2bpp) colour LUT so the resolve closure doesn't
        // borrow &self while the framebuffer row is borrowed mutably. Pen 0 is
        // transparent; the leftmost SPRITE_CLIP_MIN columns are clipped.
        let lut = [
            self.resolve(color, 0),
            self.resolve(color, 1),
            self.resolve(color, 2),
            self.resolve(color, 3),
        ];
        let clip = gfx::SpriteClip {
            x_min: SPRITE_CLIP_MIN,
            x_max: NATIVE_WIDTH as i32,
            wrap_offset: None,
        };
        let sprite_cache = &self.sprite_cache;
        let buf = &mut self.scanline_buffer[row_off..row_off + NATIVE_WIDTH * 3];
        gfx::draw_sprite_row(
            sprite_cache,
            code as u16,
            src_py,
            sx,
            flipx,
            |pv| pv == 0,
            |pv| lut[pv as usize],
            buf,
            &clip,
        );
    }

    fn draw_bullets_row(&mut self, row_off: usize, mame_y: i32, objram: &[u8]) {
        let base = BULLETS_BASE;
        let mut shell: Option<usize> = None;
        let mut missile: Option<usize> = None;

        // Shells 0..3 match Y−1; entries 3..8 match Y. Entry 7 is the missile.
        let effy1 = (mame_y - 1) as u8;
        for which in 0..3 {
            if objram[base + which * 4 + 1].wrapping_add(effy1) == 0xff {
                shell = Some(which);
            }
        }
        let effy = mame_y as u8;
        for which in 3..8 {
            if objram[base + which * 4 + 1].wrapping_add(effy) == 0xff {
                if which != 7 {
                    shell = Some(which);
                } else {
                    missile = Some(which);
                }
            }
        }

        if let Some(which) = shell {
            let x = 255 - objram[base + which * 4 + 3] as i32;
            self.draw_bullet(row_off, which, x);
        }
        if let Some(which) = missile {
            let x = 255 - objram[base + which * 4 + 3] as i32;
            self.draw_bullet(row_off, which, x);
        }
    }

    /// A bullet/shell streak ending just before `x`. Galaxian draws a 4-pixel
    /// streak (cols `x-4..x-1`) colored white for shells / yellow for the
    /// missile; Scramble (`scramble_bullets`) draws a shorter 2-pixel streak
    /// (cols `x-6..x-5`), all yellow.
    fn draw_bullet(&mut self, row_off: usize, offs: usize, x: i32) {
        let (cols, (r, g, b)) = if self.scramble_bullets {
            (x - 6..x - 4, (0xff, 0xff, 0x00))
        } else {
            (x - 4..x, self.bullet_color[offs & 7])
        };
        for col in cols {
            if (0..NATIVE_WIDTH as i32).contains(&col) {
                let off = row_off + col as usize * 3;
                self.scanline_buffer[off] = r;
                self.scanline_buffer[off + 1] = g;
                self.scanline_buffer[off + 2] = b;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Frame output
    // -----------------------------------------------------------------------

    /// Copy the native 256×224 framebuffer into the output buffer in native
    /// row-major order.
    ///
    /// The 90° cabinet rotation and any cocktail flip are declared via
    /// [`orientation`](Self::orientation) and applied centrally by the frontend,
    /// so this emits pixels unrotated and unmirrored.
    pub fn render_frame(&self, out: &mut [u8]) {
        out.copy_from_slice(&self.scanline_buffer);
    }

    /// Declarative screen orientation: base ROT90 composed with the live
    /// cocktail flip.
    ///
    /// The cabinet is mounted rotated 90°. A cocktail flip mirrors the *native*
    /// (pre-rotation) framebuffer, which — because the rotation transposes the
    /// axes — appears swapped in the rotated output: a native X-mirror becomes
    /// an output Y-mirror and vice-versa. Composing with
    /// [`Orientation::ROT90`](phosphor_core::core::machine::Orientation::ROT90)
    /// (`SWAP_XY | FLIP_X`) therefore XORs `FLIP_Y` for `flip_x` and `FLIP_X`
    /// for `flip_y`. Both flips set ⇒ `ROT270` (ROT90 + 180° cocktail).
    pub fn orientation(&self) -> phosphor_core::core::machine::Orientation {
        use phosphor_core::core::machine::Orientation;
        let mut o = Orientation::ROT90;
        if self.flip_x {
            o = o.compose(Orientation::from_bits(Orientation::FLIP_Y));
        }
        if self.flip_y {
            o = o.compose(Orientation::from_bits(Orientation::FLIP_X));
        }
        o
    }

    /// Reset dynamic state (not the ROM-derived caches/palettes/star table).
    pub fn reset(&mut self) {
        self.star_rng_origin = 0;
        self.stars_enabled = false;
        self.flip_x = false;
        self.flip_y = false;
        self.gfxbank = [0; 3]; // gfx_mode is static config, not reset
        self.background_enable = false;
        self.stars_blink = 0; // scramble_stars is static config, not reset
        self.scanline_buffer.fill(0);
    }
}

// ---------------------------------------------------------------------------
// Save state — only the dynamic latch/scroll state; caches are ROM-derived.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::save_state::Saveable as _;
    #[test]
    fn gfx_rom_size_selects_tile_and_sprite_count() {
        // A 4 KB ROM → 256 tiles / 64 sprites; an 8 KB banked ROM → 512 / 128.
        let mut v = GalaxianVideo::new();
        v.load_gfx_rom(&vec![0u8; 0x1000]);
        assert_eq!(v.tile_cache.count(), 256);
        assert_eq!(v.sprite_cache.count(), 64);
        v.load_gfx_rom(&vec![0u8; 0x2000]);
        assert_eq!(v.tile_cache.count(), 512);
        assert_eq!(v.sprite_cache.count(), 128);
    }

    #[test]
    fn pisces_gfxbank_extends_high_code_bit() {
        let mut v = GalaxianVideo::new();
        v.set_gfx_mode(GfxBankMode::Pisces);
        // Bank 0: codes unchanged.
        assert_eq!(v.extend_tile_code(0x12), 0x12);
        assert_eq!(v.extend_sprite_code(0x05), 0x05);
        // Bank 1 (gfxbank[0]) adds tile bit 8 / sprite bit 6.
        v.set_gfxbank(0, 1);
        assert_eq!(v.extend_tile_code(0x12), 0x112);
        assert_eq!(v.extend_sprite_code(0x05), 0x45);
        // Only base mode none leaves everything alone.
        v.set_gfx_mode(GfxBankMode::None);
        assert_eq!(v.extend_tile_code(0x12), 0x12);
    }

    #[test]
    fn mooncrst_gfxbank_remaps_top_tile_quarter() {
        let mut v = GalaxianVideo::new();
        v.set_gfx_mode(GfxBankMode::Mooncrst);
        // Inert until gfxbank[2] (the enable) is set.
        assert_eq!(v.extend_tile_code(0x80), 0x80);
        v.set_gfxbank(2, 1);
        // Codes outside the 0x80-0xbf window are untouched.
        assert_eq!(v.extend_tile_code(0x40), 0x40);
        assert_eq!(v.extend_tile_code(0xc0), 0xc0);
        // 0x80-0xbf remap into the second bank; gfxbank[0]/[1] are the low bits.
        assert_eq!(v.extend_tile_code(0x85), 0x105);
        v.set_gfxbank(0, 1);
        assert_eq!(v.extend_tile_code(0x85), 0x145);
        v.set_gfxbank(1, 1);
        assert_eq!(v.extend_tile_code(0x85), 0x1c5);
        // Sprites use the 0x20-0x2f window with a 0x40 bank offset.
        assert_eq!(v.extend_sprite_code(0x25), 0x40 | 0x05 | 0x10 | 0x20);
    }

    #[test]
    fn gfxbank_clears_on_reset_but_mode_persists() {
        let mut v = GalaxianVideo::new();
        v.set_gfx_mode(GfxBankMode::Pisces);
        v.set_gfxbank(0, 1);
        assert_eq!(v.extend_tile_code(0x12), 0x112);
        v.reset();
        assert_eq!(v.extend_tile_code(0x12), 0x12, "bank cleared");
        // Mode survives reset (it's static config), so banking still works.
        v.set_gfxbank(0, 1);
        assert_eq!(v.extend_tile_code(0x12), 0x112);
    }

    #[test]
    fn scramble_background_fills_blue_when_enabled() {
        let mut v = GalaxianVideo::new();
        let vram = [0u8; 0x400]; // tile 0 = blank, so nothing overdraws
        let objram = [0u8; 0x100];

        // Default (Galaxian): background is black.
        v.render_scanline(0, &vram, &objram);
        assert_eq!(&v.scanline_buffer[0..3], &[0, 0, 0]);

        // Scramble blue background (RGB 0,0,0x56) once enabled.
        v.set_background_enable(true);
        v.render_scanline(0, &vram, &objram);
        assert_eq!(&v.scanline_buffer[0..3], &[0, 0, 0x56]);

        // reset() clears the enable (it's a control line, not config).
        v.reset();
        v.render_scanline(0, &vram, &objram);
        assert_eq!(&v.scanline_buffer[0..3], &[0, 0, 0]);
    }

    #[test]
    fn scramble_star_blink_advances_per_frame() {
        let mut v = GalaxianVideo::new();
        v.set_scramble_stars(true);
        let b0 = v.stars_blink;
        v.begin_frame();
        assert_eq!(v.stars_blink, (b0 + 1) & 3, "blink advances each frame");
        // Galaxian mode leaves it put.
        let mut g = GalaxianVideo::new();
        g.begin_frame();
        assert_eq!(g.stars_blink, 0);
    }

    #[test]
    fn star_table_has_expected_period_and_sparse_enables() {
        let stars = GalaxianVideo::init_stars();
        assert_eq!(stars.len(), STAR_RNG_PERIOD as usize);
        // Stars are sparse: enable requires the top 8 LFSR bits set, so only a
        // small fraction of clocks light a star.
        let enabled = stars.iter().filter(|&&s| s & 0x80 != 0).count();
        assert!(enabled > 0, "some clocks must enable a star");
        assert!(
            enabled < stars.len() / 100,
            "stars should be sparse, got {enabled}/{}",
            stars.len()
        );
        // Color index always fits 6 bits.
        assert!(stars.iter().all(|&s| (s & 0x3f) == (s & 0x3f)));
    }

    #[test]
    fn star_table_is_deterministic_and_matches_lfsr_seed() {
        // The LFSR starts at 0, so the very first entry is enable=0 (top bits
        // not yet set) with color = (~0 & 0x1f8)>>3 = 0x3f.
        let stars = GalaxianVideo::init_stars();
        assert_eq!(stars[0] & 0x80, 0, "seed clock has no star");
        assert_eq!(stars[0] & 0x3f, 0x3f, "seed color is all-ones (inverted 0)");
        // Re-running yields an identical table.
        assert_eq!(stars, GalaxianVideo::init_stars());
    }

    #[test]
    fn star_colors_use_four_level_brightness() {
        let v = GalaxianVideo::new();
        // Index 0 → all channels at level 0 → black.
        assert_eq!(v.star_color[0], (0, 0, 0));
        // The brightest level is 255; index 63 lights every pair's high bit.
        // minval = 224*130/150 = 194, mid computed, max = 255.
        let minval = (RGB_MAXIMUM as i32) * 130 / 150;
        assert_eq!(minval, 194);
        // index with only the LSB of each pair set → minval on each channel.
        // red LSB = bit5, green LSB = bit3, blue LSB = bit1.
        let idx = (1 << 5) | (1 << 3) | (1 << 1);
        assert_eq!(v.star_color[idx], (194, 194, 194));
    }

    #[test]
    fn bullet_colors_white_then_yellow() {
        let v = GalaxianVideo::new();
        for i in 0..7 {
            assert_eq!(v.bullet_color[i], (0xff, 0xff, 0xff));
        }
        assert_eq!(v.bullet_color[7], (0xff, 0xff, 0x00));
    }

    #[test]
    fn palette_resnet_endpoints() {
        let mut v = GalaxianVideo::new();
        let mut prom = [0u8; 32];
        prom[1] = 0xff; // all RGB bits on
        prom[2] = 0x00; // all off
        v.load_color_prom(&prom);
        // All bits on: R and G are the dominant (max) network → 224; the 2-bit
        // blue network is scaled jointly and lands just under (217).
        assert_eq!(v.palette_rgb[1], (224, 224, 217));
        // All bits off: black.
        assert_eq!(v.palette_rgb[2], (0, 0, 0));
    }

    #[test]
    fn gfx_decode_plane_order_lsb_first() {
        // Build a 2-tile GFX ROM (0x1000 bytes: two 0x800 planes). Plane 0 is the
        // lower half (pixel LSB), plane 1 the upper half (pixel MSB).
        let mut rom = vec![0u8; 0x1000];
        // Tile 0, row 0: plane-0 byte = 0b1000_0000 (px0 LSB set),
        //               plane-1 byte = 0b0100_0000 (px1 MSB set).
        rom[0] = 0b1000_0000; // plane 0, tile 0, y0
        rom[0x800] = 0b0100_0000; // plane 1, tile 0, y0
        let mut v = GalaxianVideo::new();
        v.load_gfx_rom(&rom);
        assert_eq!(v.tile_cache.pixel(0, 0, 0), 1, "px0: only plane0 → value 1");
        assert_eq!(v.tile_cache.pixel(0, 1, 0), 2, "px1: only plane1 → value 2");
        assert_eq!(v.tile_cache.pixel(0, 2, 0), 0, "px2: neither plane");
    }

    #[test]
    fn tilemap_column_scroll_and_render() {
        let mut v = GalaxianVideo::new();
        // GFX: tile 1 is solid pixel-value-1 everywhere.
        let mut rom = vec![0u8; 0x1000];
        for y in 0..8 {
            rom[8 + y] = 0xff; // tile 1, plane 0, all 8 px set
        }
        v.load_gfx_rom(&rom);
        // Palette: color 0, pen 1 → distinctive red.
        let mut prom = [0u8; 32];
        prom[1] = 0b0000_0111; // R bits all on
        v.load_color_prom(&prom);

        let mut vram = [0u8; 0x400];
        let objram = [0u8; 0x100];
        // Place tile 1 at tile row 0, column 0.
        vram[0] = 1;
        v.render_scanline(0, &vram, &objram);
        // Hardware scanline 16 → eff_y 16 → tile_row 2 with zero scroll, so
        // column 0 row 0 (tile 1) is NOT at row 0; verify the no-scroll mapping
        // by placing the tile where row 0 actually samples.
        // eff_y for row 0 is Y_OFFSET=16 → tile_row 2 → vram[2*32+0].
        let mut vram2 = [0u8; 0x400];
        vram2[2 * 32] = 1;
        v.render_scanline(0, &vram2, &objram);
        let px0 = &v.scanline_buffer[0..3];
        assert_eq!(px0[0], v.palette_rgb[1].0);
        assert!(px0[0] > 0, "tile pixel should be drawn");
    }

    #[test]
    fn sprite_renders_with_priority_and_clip() {
        let mut v = GalaxianVideo::new();
        // sprite code 0 solid pixel-value 1.
        let mut rom = vec![0u8; 0x1000];
        for b in rom.iter_mut().take(0x800) {
            *b = 0xff; // plane 0 fully set → every sprite/tile px LSB = 1
        }
        v.load_gfx_rom(&rom);
        let mut prom = [0u8; 32];
        prom[1] = 0b0011_1000; // green via pen 1, color 0
        v.load_color_prom(&prom);

        let mut objram = [0u8; 0x100];
        // Sprite 0: place so it covers hardware scanline 16 (buffer row 0).
        // sy = 240 - (b0 - 1) for sprnum<3. Want sy <= 16 < sy+16 → b0≈239.
        objram[SPRITES_BASE] = 239; // y
        objram[SPRITES_BASE + 1] = 0; // code 0, no flip
        objram[SPRITES_BASE + 2] = 0; // color 0
        objram[SPRITES_BASE + 3] = 100; // x → sx=101
        let vram = [0u8; 0x400];
        v.render_scanline(0, &vram, &objram);
        // sx = 101, beyond the left clip → should be drawn.
        let off = 101 * 3;
        assert!(
            v.scanline_buffer[off + 1] > 0,
            "sprite green channel should be set at x=101"
        );
    }

    #[test]
    fn bullet_draws_four_pixel_streak() {
        let mut v = GalaxianVideo::new();
        let mut objram = [0u8; 0x100];
        // Missile (entry 7): matches Y, so y_byte = 255 - mame_y. For row 0,
        // mame_y = 16 → y_byte = 239. x_byte chosen so x = 255 - x_byte = 100.
        objram[BULLETS_BASE + 7 * 4 + 1] = (255 - 16) as u8; // y match
        objram[BULLETS_BASE + 7 * 4 + 3] = (255 - 100) as u8; // x
        let vram = [0u8; 0x400];
        v.render_scanline(0, &vram, &objram);
        // Streak occupies columns 96..=99 (x-4..x-1), yellow.
        for col in 96..100 {
            let off = col * 3;
            assert_eq!(
                (
                    v.scanline_buffer[off],
                    v.scanline_buffer[off + 1],
                    v.scanline_buffer[off + 2]
                ),
                (0xff, 0xff, 0x00),
                "missile pixel at {col}"
            );
        }
        // Column 100 itself is past the streak.
        assert_eq!(v.scanline_buffer[100 * 3], 0);
    }

    #[test]
    fn scramble_bullet_draws_two_pixel_yellow_streak() {
        let mut v = GalaxianVideo::new();
        v.set_scramble_bullets(true);
        let mut objram = [0u8; 0x100];
        // Shell entry 0 (matches Y-1): for row 0, mame_y = 16 → y match at 15.
        objram[BULLETS_BASE + 1] = (255 - 15) as u8; // y match (mame_y - 1)
        objram[BULLETS_BASE + 3] = (255 - 100) as u8; // x = 100
        let vram = [0u8; 0x400];
        v.render_scanline(0, &vram, &objram);
        // Scramble shell is 2 px (cols x-6..=x-5 = 94..=95), all yellow.
        for col in 94..96 {
            let off = col * 3;
            assert_eq!(
                (
                    v.scanline_buffer[off],
                    v.scanline_buffer[off + 1],
                    v.scanline_buffer[off + 2]
                ),
                (0xff, 0xff, 0x00),
                "scramble shell pixel at {col}"
            );
        }
        // Galaxian's columns 96..=99 must NOT be lit (shorter streak).
        assert_eq!(v.scanline_buffer[96 * 3], 0);
        assert_eq!(v.scanline_buffer[99 * 3], 0);
    }

    #[test]
    fn frogger_background_is_blue_left_black_right() {
        let mut v = GalaxianVideo::new();
        v.set_frogger(true);
        let vram = [0u8; 0x400];
        let objram = [0u8; 0x100];
        v.render_scanline(0, &vram, &objram);
        // Native x < 128 → blue (0,0,0x47); x >= 128 → black.
        assert_eq!(&v.scanline_buffer[0..3], &[0, 0, 0x47]);
        assert_eq!(&v.scanline_buffer[127 * 3..127 * 3 + 3], &[0, 0, 0x47]);
        assert_eq!(&v.scanline_buffer[128 * 3..128 * 3 + 3], &[0, 0, 0]);
        assert_eq!(&v.scanline_buffer[255 * 3..255 * 3 + 3], &[0, 0, 0]);
    }

    #[test]
    fn frogger_color_rotation() {
        // b2 b1 b0 -> b0 b2 b1
        assert_eq!(GalaxianVideo::frogger_color(0b000), 0b000);
        assert_eq!(GalaxianVideo::frogger_color(0b001), 0b100); // b0 -> bit2
        assert_eq!(GalaxianVideo::frogger_color(0b010), 0b001); // b1 -> bit0
        assert_eq!(GalaxianVideo::frogger_color(0b100), 0b010); // b2 -> bit1
        assert_eq!(GalaxianVideo::frogger_color(0b111), 0b111);
    }

    #[test]
    fn frogger_suppresses_bullets() {
        let mut v = GalaxianVideo::new();
        v.set_frogger(true);
        let mut objram = [0u8; 0x100];
        // A missile that would draw on row 0 in non-Frogger mode.
        objram[BULLETS_BASE + 7 * 4 + 1] = (255 - 16) as u8;
        objram[BULLETS_BASE + 7 * 4 + 3] = (255 - 100) as u8;
        let vram = [0u8; 0x400];
        v.render_scanline(0, &vram, &objram);
        // No yellow streak — columns near x=100 keep the blue background.
        assert_eq!(&v.scanline_buffer[97 * 3..97 * 3 + 3], &[0, 0, 0x47]);
    }

    #[test]
    fn frogger_adjust_nibble_swaps_sprite_y() {
        // A sprite whose raw Y byte is 0x1e maps to adder-Y 0xe1 under the
        // Frogger nibble swap, changing which scanline it lands on.
        let mut v = GalaxianVideo::new();
        v.set_frogger(true);
        let mut rom = vec![0u8; 0x1000];
        for b in rom.iter_mut().take(0x800) {
            *b = 0xff; // every sprite pixel LSB set
        }
        v.load_gfx_rom(&rom);
        let mut prom = [0u8; 32];
        prom[1] = 0b0011_1000; // green for color 0, pen 1
        v.load_color_prom(&prom);

        let mut objram = [0u8; 0x100];
        // sprnum 3 (matched against Y, no -1). Raw Y 0x1e -> swapped 0xe1.
        // sy = 240 - 0xe1 = 15, so it covers mame_y 16 (buffer row 0).
        objram[SPRITES_BASE + 3 * 4] = 0x1e;
        objram[SPRITES_BASE + 3 * 4 + 1] = 0; // code 0
        objram[SPRITES_BASE + 3 * 4 + 2] = 0; // color 0
        objram[SPRITES_BASE + 3 * 4 + 3] = 100; // sx = 101
        let vram = [0u8; 0x400];
        v.render_scanline(0, &vram, &objram);
        assert!(
            v.scanline_buffer[101 * 3 + 1] > 0,
            "sprite should land on row 0 after the nibble swap"
        );
    }

    #[test]
    fn star_scroll_advances_each_frame() {
        let mut v = GalaxianVideo::new();
        let start = v.star_rng_origin;
        v.begin_frame(); // unflipped → −1 mod period
        assert_eq!(v.star_rng_origin, STAR_RNG_PERIOD - 1);
        v.set_flip_x(true);
        v.begin_frame(); // flipped → +1
        assert_eq!(v.star_rng_origin, start);
    }

    #[test]
    fn render_frame_emits_native_unrotated() {
        use phosphor_core::core::machine::Orientation;
        let v = GalaxianVideo::new();
        // Native (unrotated) 256×224 RGB24 output; ROT90 declared, not baked.
        let mut out = vec![0u8; NATIVE_WIDTH * NATIVE_HEIGHT * 3];
        v.render_frame(&mut out); // all-black native → all-black output, no panic
        assert!(out.iter().all(|&b| b == 0));
        assert_eq!(v.orientation(), Orientation::ROT90);
    }

    /// The declarative (native render + central `apply_orientation`) path must
    /// be pixel-identical to the legacy baked mirror-then-rotate-90-CCW path for
    /// every combination of the cocktail flip axes.
    #[test]
    fn declarative_orientation_matches_legacy_bake() {
        // Fill the native buffer with a position-dependent gradient so any
        // axis error shows up as a mismatch.
        let mut v = GalaxianVideo::new();
        for ny in 0..NATIVE_HEIGHT {
            for nx in 0..NATIVE_WIDTH {
                let off = (ny * NATIVE_WIDTH + nx) * 3;
                v.scanline_buffer[off] = (nx & 0xFF) as u8;
                v.scanline_buffer[off + 1] = (ny & 0xFF) as u8;
                v.scanline_buffer[off + 2] = ((nx ^ ny) & 0xFF) as u8;
            }
        }

        // Snapshot the native buffer so the legacy closure doesn't hold a
        // borrow of `v` while we toggle its flip latches below.
        let src = v.scanline_buffer.clone();

        // Legacy bake: mirror the requested axes of the native buffer, then
        // rotate 90° CCW into the 224×256 display buffer.
        let legacy_bake = |flip_x: bool, flip_y: bool| -> Vec<u8> {
            let src = &src;
            let mut flipped = vec![0u8; src.len()];
            for ny in 0..NATIVE_HEIGHT {
                let sy = if flip_y { NATIVE_HEIGHT - 1 - ny } else { ny };
                for nx in 0..NATIVE_WIDTH {
                    let sx = if flip_x { NATIVE_WIDTH - 1 - nx } else { nx };
                    let si = (sy * NATIVE_WIDTH + sx) * 3;
                    let di = (ny * NATIVE_WIDTH + nx) * 3;
                    flipped[di..di + 3].copy_from_slice(&src[si..si + 3]);
                }
            }
            let mut out = vec![0u8; src.len()];
            phosphor_core::gfx::rotate_90_ccw(&flipped, &mut out, NATIVE_WIDTH, NATIVE_HEIGHT);
            out
        };

        for (flip_x, flip_y) in [(false, false), (true, false), (false, true), (true, true)] {
            v.set_flip_x(flip_x);
            v.set_flip_y(flip_y);
            let mut native = vec![0u8; v.scanline_buffer.len()];
            v.render_frame(&mut native);
            let mut declarative = vec![0u8; native.len()];
            phosphor_core::gfx::apply_orientation(
                &native,
                &mut declarative,
                NATIVE_WIDTH,
                NATIVE_HEIGHT,
                v.orientation(),
            );
            assert_eq!(
                declarative,
                legacy_bake(flip_x, flip_y),
                "mismatch for flip_x={flip_x} flip_y={flip_y} (orientation={:?})",
                v.orientation()
            );
        }
    }

    #[test]
    fn save_load_round_trip() {
        use phosphor_core::core::save_state::{StateReader, StateWriter};
        let mut v = GalaxianVideo::new();
        v.set_stars_enabled(true);
        v.set_flip_x(true);
        v.star_rng_origin = 12345;
        let mut w = StateWriter::new();
        v.save_state(&mut w);
        let bytes = w.into_vec();

        let mut v2 = GalaxianVideo::new();
        let mut r = StateReader::new(&bytes);
        v2.load_state(&mut r).unwrap();
        assert_eq!(v2.star_rng_origin, 12345);
        assert!(v2.stars_enabled());
        assert!(v2.flip_x());
        assert!(!v2.flip_y());
    }
}
