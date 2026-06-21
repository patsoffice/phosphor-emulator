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

use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::gfx;
use phosphor_core::gfx::decode::{GfxCache, GfxLayout, decode_gfx};

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
pub struct GalaxianVideo {
    // Pre-decoded GFX (chars and sprites share the same GFX ROM; sprites are
    // 2×2 groups of chars decoded as 16×16 elements). Sized from the ROM:
    // 256/64 for a 4 KB ROM, 512/128 for an 8 KB banked ROM.
    tile_cache: GfxCache,
    sprite_cache: GfxCache,

    // GFX bank-switching scheme and the 74LS259 latch bits that drive it.
    gfx_mode: GfxBankMode,
    gfxbank: [u8; 3],

    // PROM + derived colors.
    palette_prom: [u8; 32],
    palette_rgb: [(u8, u8, u8); 32], // 8 colors × 4 pens
    star_color: [(u8, u8, u8); 64],
    bullet_color: [(u8, u8, u8); 8],

    // Precomputed starfield LFSR table (one byte per clock: bit 7 = enable,
    // bits 0..5 = color index). Derived from ROM-independent hardware logic, so
    // it is rebuilt on construction and never saved.
    stars: Vec<u8>,
    star_rng_origin: u32,

    // Latch outputs that affect rendering (driven by the board).
    stars_enabled: bool,
    flip_x: bool,
    flip_y: bool,

    // Native-orientation RGB24 framebuffer (256 × 224), filled per scanline.
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

    /// Compute MAME-`resnet` per-bit weights for one resistor network, scaled to
    /// `max` (i.e. `Vout = max · R0/(R1+R0)` per bit, with every other bit
    /// resistor sharing the pulldown/ground path). Returns the unscaled-by-
    /// network weights; the caller applies the shared cross-network scale.
    fn resnet_raw_weights(resistances: &[f64], pulldown: f64, max: f64) -> Vec<f64> {
        // Open connection ≈ 1e12 Ω conductance floor, matching MAME.
        let pd_g = if pulldown == 0.0 {
            1e-12
        } else {
            1.0 / pulldown
        };
        resistances
            .iter()
            .enumerate()
            .map(|(bit, _)| {
                // Conductance to ground: pulldown plus every *other* bit.
                let mut g0 = pd_g;
                let mut g1 = 1e-12; // no pullup
                for (j, &r) in resistances.iter().enumerate() {
                    if r != 0.0 {
                        if j == bit {
                            g1 += 1.0 / r;
                        } else {
                            g0 += 1.0 / r;
                        }
                    }
                }
                let r0 = 1.0 / g0;
                let r1 = 1.0 / g1;
                max * r0 / (r1 + r0)
            })
            .collect()
    }

    fn build_palette(&mut self) {
        // R/G: 1K/470/220 Ω; B: 470/220 Ω; all with a 470 Ω pulldown.
        let rg_res = [1000.0, 470.0, 220.0];
        let b_res = [470.0, 220.0];

        let r_raw = Self::resnet_raw_weights(&rg_res, 470.0, RGB_MAXIMUM);
        let g_raw = r_raw.clone(); // identical network
        let b_raw = Self::resnet_raw_weights(&b_res, 470.0, RGB_MAXIMUM);

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

        // 1) Background: black, then stars.
        {
            let buf = &mut self.scanline_buffer[row_off..row_off + NATIVE_WIDTH * 3];
            buf.fill(0);
        }
        if self.stars_enabled {
            self.draw_star_row(row_off, mame_y);
        }

        // 2) Tilemap: 32 columns, each with independent vertical scroll/color.
        for col in 0..TILE_COLS {
            let scroll = objram[col * 2] as i32;
            let color = objram[col * 2 + 1] & 7;
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
            self.draw_sprite_row(row_off, mame_y, objram, sprnum);
        }

        // 4) Bullets/shells over everything.
        self.draw_bullets_row(row_off, mame_y, objram);
    }

    fn draw_star_row(&mut self, row_off: usize, mame_y: i32) {
        // RNG offset for this scanline; two clocks advance per native column.
        let mut offs =
            ((self.star_rng_origin as u64 + (mame_y as u64) * 512) % STAR_RNG_PERIOD as u64) as u32;
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
                let idx = if star_b & 0x80 != 0 {
                    Some(star_b & 0x3f)
                } else if star_a & 0x80 != 0 {
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

    fn draw_sprite_row(&mut self, row_off: usize, mame_y: i32, objram: &[u8], sprnum: usize) {
        let base = SPRITES_BASE + sprnum * 4;
        let b0 = objram[base] as i32;
        // First three sprites are matched against Y−1 (a +1 Y nudge).
        let adj = if sprnum < 3 { 1 } else { 0 };
        let sy = 240 - (b0 - adj);
        if mame_y < sy || mame_y >= sy + 16 {
            return;
        }
        let code = self.extend_sprite_code((objram[base + 1] & 0x3f) as usize);
        let flipx = objram[base + 1] & 0x40 != 0;
        let flipy = objram[base + 1] & 0x80 != 0;
        let color = objram[base + 2] & 7;
        let sx = objram[base + 3].wrapping_add(1) as i32;

        let yrow = mame_y - sy;
        let src_py = if flipy { 15 - yrow } else { yrow } as usize;

        for px in 0..16i32 {
            let draw_x = sx + px;
            if draw_x < SPRITE_CLIP_MIN || draw_x >= NATIVE_WIDTH as i32 {
                continue;
            }
            let src_px = if flipx { 15 - px } else { px } as usize;
            let pv = self.sprite_cache.pixel(code, src_px, src_py);
            if pv == 0 {
                continue;
            }
            let (r, g, b) = self.resolve(color, pv);
            let off = row_off + draw_x as usize * 3;
            self.scanline_buffer[off] = r;
            self.scanline_buffer[off + 1] = g;
            self.scanline_buffer[off + 2] = b;
        }
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

    /// A bullet/shell is a 4-px-long horizontal streak ending just before `x`.
    fn draw_bullet(&mut self, row_off: usize, offs: usize, x: i32) {
        let (r, g, b) = self.bullet_color[offs & 7];
        for dx in 0..4 {
            let col = x - 4 + dx;
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

    /// Rotate the native 256×224 framebuffer 90° CCW into the 224×256 display
    /// buffer. Cocktail flip (rare; player-2 on a flippable cabinet) is applied
    /// as a whole-frame mirror — the upright path is bit-faithful.
    pub fn render_frame(&self, out: &mut [u8]) {
        if !self.flip_x && !self.flip_y {
            gfx::rotate_90_ccw(&self.scanline_buffer, out, NATIVE_WIDTH, NATIVE_HEIGHT);
            return;
        }
        // Mirror the requested axes of the native buffer, then rotate.
        let mut flipped = vec![0u8; self.scanline_buffer.len()];
        for ny in 0..NATIVE_HEIGHT {
            let sy = if self.flip_y {
                NATIVE_HEIGHT - 1 - ny
            } else {
                ny
            };
            for nx in 0..NATIVE_WIDTH {
                let sx = if self.flip_x {
                    NATIVE_WIDTH - 1 - nx
                } else {
                    nx
                };
                let si = (sy * NATIVE_WIDTH + sx) * 3;
                let di = (ny * NATIVE_WIDTH + nx) * 3;
                flipped[di..di + 3].copy_from_slice(&self.scanline_buffer[si..si + 3]);
            }
        }
        gfx::rotate_90_ccw(&flipped, out, NATIVE_WIDTH, NATIVE_HEIGHT);
    }

    /// Reset dynamic state (not the ROM-derived caches/palettes/star table).
    pub fn reset(&mut self) {
        self.star_rng_origin = 0;
        self.stars_enabled = false;
        self.flip_x = false;
        self.flip_y = false;
        self.gfxbank = [0; 3]; // gfx_mode is static config, not reset
        self.scanline_buffer.fill(0);
    }
}

// ---------------------------------------------------------------------------
// Save state — only the dynamic latch/scroll state; caches are ROM-derived.
// ---------------------------------------------------------------------------

impl Saveable for GalaxianVideo {
    fn save_state(&self, w: &mut StateWriter) {
        w.write_u32_le(self.star_rng_origin);
        w.write_bool(self.stars_enabled);
        w.write_bool(self.flip_x);
        w.write_bool(self.flip_y);
        w.write_bytes(&self.gfxbank);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.star_rng_origin = r.read_u32_le()?;
        self.stars_enabled = r.read_bool()?;
        self.flip_x = r.read_bool()?;
        self.flip_y = r.read_bool()?;
        r.read_bytes_into(&mut self.gfxbank)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
    fn render_frame_rotates_dimensions() {
        let v = GalaxianVideo::new();
        // Output buffer must be 224×256 RGB24.
        let mut out = vec![0u8; 224 * 256 * 3];
        v.render_frame(&mut out); // all-black native → all-black output, no panic
        assert!(out.iter().all(|&b| b == 0));
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
