use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug::{BusDebug, DebugCpu, Debuggable};
use phosphor_core::core::machine::{
    AudioSource, InputReceiver, MachineCore, MachineDebug, Renderable, SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::gfx;
use phosphor_core::gfx::GfxCache;
use phosphor_core::gfx::decode::{GfxLayout, decode_gfx};
use phosphor_macros::Saveable;

use crate::namco_galaga::{self, GALAGA_SPRITE_LAYOUT, NamcoGalagaBoard};
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};

// ---------------------------------------------------------------------------
// GfxLayout for Galaga characters (2bpp 8×8)
// ---------------------------------------------------------------------------

const GALAGA_CHAR_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[4, 0],
    x_offsets: &[64, 65, 66, 67, 0, 1, 2, 3],
    y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
    char_increment: 128, // 16 bytes per character
};

// ---------------------------------------------------------------------------
// Starfield constants (Namco 05XX)
// ---------------------------------------------------------------------------

const LFSR_SEED: u16 = 0x7FFF;
const LFSR_HIT_MASK: u16 = 0xFA14;
const LFSR_HIT_VALUE: u16 = 0x7800;
const STARFIELD_PIXEL_WIDTH: u16 = 256;
const VISIBLE_LINES: u16 = 224;
const STARFIELD_X_OFFSET: u16 = 16;
const STARFIELD_X_LIMIT: u16 = 256 + STARFIELD_X_OFFSET;

const SPEED_X_CYCLE_COUNT_OFFSET: [i32; 8] = [0, 1, 2, 3, -4, -3, -2, -1];

// Pre-visible line counts × 256 cycles/line, indexed by scroll_y (always 0 for Galaga)
const PRE_VIS_CYCLE_COUNT: [i32; 8] = [
    22 * 256,
    23 * 256,
    22 * 256,
    23 * 256,
    19 * 256,
    20 * 256,
    20 * 256,
    22 * 256,
];
const POST_VIS_CYCLE_COUNT: [i32; 8] = [
    10 * 256,
    10 * 256,
    12 * 256,
    12 * 256,
    9 * 256,
    9 * 256,
    10 * 256,
    9 * 256,
];

// ---------------------------------------------------------------------------
// ROM definitions — Galaga (Namco rev B, "galaga")
// ---------------------------------------------------------------------------

static GALAGA_MAIN_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "gg1_1b.3p",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xab036c9f],
        },
        RomEntry {
            name: "gg1_2b.3m",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0xd9232240],
        },
        RomEntry {
            name: "gg1_3.2m",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0x753ce503],
        },
        RomEntry {
            name: "gg1_4b.2l",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0x499fcc76],
        },
    ],
};

static GALAGA_SUB_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "gg1_5b.3f",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0xbb5caae3],
    }],
};

static GALAGA_SOUND_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "gg1_7b.2c",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0xd016686b],
    }],
};

// ---------------------------------------------------------------------------
// ROM definitions — Galaga (Namco original, "galagao")
// ---------------------------------------------------------------------------

static GALAGAO_MAIN_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "gg1-1.3p",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xa3a0f743],
        },
        RomEntry {
            name: "gg1-2.3m",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x43bb0d5c],
        },
        RomEntry {
            name: "gg1-3.2m",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0x753ce503],
        },
        RomEntry {
            name: "gg1-4.2l",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0x83874442],
        },
    ],
};

static GALAGAO_SUB_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "gg1-5.3f",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0x3102fccd],
    }],
};

static GALAGAO_SOUND_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "gg1-7.2c",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0x8995088d],
    }],
};

// ---------------------------------------------------------------------------
// ROM definitions — Galaga (Midway, "galagamw")
// ---------------------------------------------------------------------------

static GALAGAMW_MAIN_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "3200a.bin",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x3ef0b053],
        },
        RomEntry {
            name: "3300b.bin",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x1b280831],
        },
        RomEntry {
            name: "3400c.bin",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0x16233d33],
        },
        RomEntry {
            name: "3500d.bin",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0x0aaf5c23],
        },
    ],
};

static GALAGAMW_SUB_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "3600e.bin",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0xbc556e76],
    }],
};

static GALAGAMW_SOUND_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "3700g.bin",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0xb07f0aa4],
    }],
};

// ---------------------------------------------------------------------------
// ROM definitions — shared GFX and PROMs
// ---------------------------------------------------------------------------

static GALAGA_GFX1_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "gg1_9.4l",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0x58b2f47c],
    }],
};

static GALAGA_GFX2_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "gg1_11.4d",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xad447c80],
        },
        RomEntry {
            name: "gg1_10.4f",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0xdd6f1afc],
        },
    ],
};

static GALAGA_PROMS: RomRegion = RomRegion {
    size: 0x0220,
    entries: &[
        RomEntry {
            name: "prom-5.5n",
            size: 0x0020,
            offset: 0x0000,
            crc32: &[0x54603c6b],
        },
        RomEntry {
            name: "prom-4.2n",
            size: 0x0100,
            offset: 0x0020,
            crc32: &[0x59b6edab],
        },
        RomEntry {
            name: "prom-3.1c",
            size: 0x0100,
            offset: 0x0120,
            crc32: &[0x4a04bb6b],
        },
    ],
};

static GALAGA_SOUND_PROM: RomRegion = RomRegion {
    size: 0x0100,
    entries: &[RomEntry {
        name: "prom-1.1d",
        size: 0x0100,
        offset: 0x0000,
        crc32: &[0x7a2815b4],
    }],
};

// ---------------------------------------------------------------------------
// ROM configuration
// ---------------------------------------------------------------------------

struct GalagaRomConfig {
    main_rom: &'static RomRegion,
    sub_rom: &'static RomRegion,
    sound_rom: &'static RomRegion,
    gfx1_rom: &'static RomRegion,
    gfx2_rom: &'static RomRegion,
    proms: &'static RomRegion,
    sound_prom: &'static RomRegion,
}

static GALAGA_CONFIG: GalagaRomConfig = GalagaRomConfig {
    main_rom: &GALAGA_MAIN_ROM,
    sub_rom: &GALAGA_SUB_ROM,
    sound_rom: &GALAGA_SOUND_ROM,
    gfx1_rom: &GALAGA_GFX1_ROM,
    gfx2_rom: &GALAGA_GFX2_ROM,
    proms: &GALAGA_PROMS,
    sound_prom: &GALAGA_SOUND_PROM,
};

static GALAGAO_CONFIG: GalagaRomConfig = GalagaRomConfig {
    main_rom: &GALAGAO_MAIN_ROM,
    sub_rom: &GALAGAO_SUB_ROM,
    sound_rom: &GALAGAO_SOUND_ROM,
    gfx1_rom: &GALAGA_GFX1_ROM,     // shared
    gfx2_rom: &GALAGA_GFX2_ROM,     // shared
    proms: &GALAGA_PROMS,           // shared
    sound_prom: &GALAGA_SOUND_PROM, // shared
};

static GALAGAMW_CONFIG: GalagaRomConfig = GalagaRomConfig {
    main_rom: &GALAGAMW_MAIN_ROM,
    sub_rom: &GALAGAMW_SUB_ROM,
    sound_rom: &GALAGAMW_SOUND_ROM,
    gfx1_rom: &GALAGA_GFX1_ROM,     // shared
    gfx2_rom: &GALAGA_GFX2_ROM,     // shared
    proms: &GALAGA_PROMS,           // shared
    sound_prom: &GALAGA_SOUND_PROM, // shared
};

// ---------------------------------------------------------------------------
// GalagaSystem
// ---------------------------------------------------------------------------

/// Galaga Arcade System (Namco, 1981)
///
/// Hardware: 3×Z80 @ 3.072 MHz, Namco WSG 3-voice, Namco 06XX/51XX/53XX
/// custom I/O, Namco 05XX starfield generator.
/// Video: 36×28 tilemap (2bpp), 64 sprites (variable size), scrolling starfield.
/// Screen: 288×224 rotated 90° CCW.
#[derive(Saveable)]
pub struct GalagaSystem {
    pub board: NamcoGalagaBoard,

    // RAM regions (shared by all 3 CPUs)
    video_ram: [u8; 0x800], // 0x8000-0x87FF (tile codes + tile colors)
    ram1: [u8; 0x400],      // 0x8800-0x8BFF (work RAM + sprite attribs)
    ram2: [u8; 0x400],      // 0x9000-0x93FF (work RAM + sprite positions)
    ram3: [u8; 0x400],      // 0x9800-0x9BFF (work RAM + sprite flip/size)

    // Video latch state (0xA000-0xA007 LS259)
    starfield_scroll_x: u8,  // Q0-Q2: X scroll speed index
    star_set_a: u8,          // Q3
    star_set_b: u8,          // Q4 (OR'd with 2 per MAME)
    starfield_enabled: bool, // Q5: _STARCLR (active-high enable)

    // Starfield generator state (Namco 05XX)
    star_lfsr: u16,

    // Star palette (64 colors, computed at ROM load)
    #[save_skip]
    star_palette: [(u8, u8, u8); 64],

    // Combined palette for render_frame: 32 base + 64 star = 96 entries
    #[save_skip]
    combined_palette: Vec<(u8, u8, u8)>,

    // GFX caches
    #[save_skip]
    char_cache: GfxCache, // 2bpp 8×8 (256 tiles)
    #[save_skip]
    sprite_cache: GfxCache, // 2bpp 16×16 (128 sprites)

    // Color lookup tables (from PROMs)
    #[save_skip]
    char_lut: [u8; 256],
    #[save_skip]
    sprite_lut: [u8; 256],

    // Frame buffer (288 × 224 native, indexed — rotated in render_frame)
    #[save_skip]
    native_buffer: Vec<u8>,
}

impl GalagaSystem {
    pub fn new() -> Self {
        Self {
            board: NamcoGalagaBoard::new(),

            video_ram: [0; 0x800],
            ram1: [0; 0x400],
            ram2: [0; 0x400],
            ram3: [0; 0x400],

            starfield_scroll_x: 0,
            star_set_a: 0,
            star_set_b: 0,
            starfield_enabled: false,

            star_lfsr: LFSR_SEED,

            star_palette: [(0, 0, 0); 64],
            combined_palette: vec![(0, 0, 0); 128],

            char_cache: GfxCache::new(0, 8, 8),
            sprite_cache: GfxCache::new(0, 16, 16),

            char_lut: [0; 256],
            sprite_lut: [0; 256],

            native_buffer: vec![0u8; 288 * 224],
        }
    }

    fn load_roms(
        &mut self,
        rom_set: &RomSet,
        config: &GalagaRomConfig,
    ) -> Result<(), RomLoadError> {
        // Program ROMs
        self.board.load_main_rom(&config.main_rom.load(rom_set)?);
        self.board.load_sub_rom(&config.sub_rom.load(rom_set)?);
        self.board.load_sound_rom(&config.sound_rom.load(rom_set)?);

        // GFX ROMs
        let gfx1 = config.gfx1_rom.load(rom_set)?;
        self.char_cache = decode_gfx(&gfx1, 0, gfx1.len() / 16, &GALAGA_CHAR_LAYOUT);

        let gfx2 = config.gfx2_rom.load(rom_set)?;
        self.sprite_cache = decode_gfx(&gfx2, 0, gfx2.len() / 64, &GALAGA_SPRITE_LAYOUT);

        // PROMs: 0x00-0x1F palette, 0x20-0x11F char LUT, 0x120-0x21F sprite LUT
        let proms = config.proms.load(rom_set)?;

        // Build palette using Galaga-specific weights (NOT the board's resistor-weight DAC)
        self.build_galaga_palette(&proms[0..0x20]);

        self.char_lut.copy_from_slice(&proms[0x20..0x120]);
        self.sprite_lut.copy_from_slice(&proms[0x120..0x220]);

        // Sound PROM
        self.board
            .load_sound_prom(&config.sound_prom.load(rom_set)?);

        // Build star palette
        self.build_star_palette();

        // Build combined palette (base 32 + 64 star colors)
        self.rebuild_combined_palette();

        // Galaga DIP switch defaults (matching MAME factory defaults):
        // DSWA: Difficulty=Easy(0x03), Unused(0x04), DemoSounds=On(0x00),
        //       Freeze=Off(0x10), RackTest=Off(0x20), Unused(0x40), Cabinet=Upright(0x80)
        self.board.dswa = 0xF7;
        // DSWB: Coinage=1C/1C(0x07), Bonus=20K,70K,Every70K(0x10), Lives=3(0x80)
        self.board.dswb = 0x97;

        Ok(())
    }

    /// Build palette from PROM using Galaga-specific DAC weights.
    /// Galaga uses: R = 0x21*b0 + 0x47*b1 + 0x97*b2
    ///              G = 0x21*b3 + 0x47*b4 + 0x97*b5
    ///              B = 0x00*0  + 0x47*b6 + 0x97*b7
    fn build_galaga_palette(&mut self, prom: &[u8]) {
        for (i, &entry) in prom.iter().enumerate().take(32) {
            let r = 0x21 * (entry & 1) as u32
                + 0x47 * ((entry >> 1) & 1) as u32
                + 0x97 * ((entry >> 2) & 1) as u32;
            let g = 0x21 * ((entry >> 3) & 1) as u32
                + 0x47 * ((entry >> 4) & 1) as u32
                + 0x97 * ((entry >> 5) & 1) as u32;
            let b = 0x47 * ((entry >> 6) & 1) as u32 + 0x97 * ((entry >> 7) & 1) as u32;

            self.board.palette_rgb[i] = (r as u8, g as u8, b as u8);
        }
    }

    /// Build the 64-entry star color palette.
    fn build_star_palette(&mut self) {
        const MAP: [u8; 4] = [0x00, 0x47, 0x97, 0xDE];
        for i in 0..64 {
            let r = MAP[i & 0x03];
            let g = MAP[(i >> 2) & 0x03];
            let b = MAP[(i >> 4) & 0x03];
            self.star_palette[i] = (r, g, b);
        }
    }

    /// Rebuild the combined palette (32 base + 64 star, padded to 128 for power-of-2 masking).
    fn rebuild_combined_palette(&mut self) {
        self.combined_palette.resize(128, (0, 0, 0));
        self.combined_palette[..32].copy_from_slice(&self.board.palette_rgb);
        self.combined_palette[32..96].copy_from_slice(&self.star_palette);
        // Entries 96-127 remain black (unused padding for power-of-2 mask)
    }

    // -----------------------------------------------------------------------
    // Video latch (0xA000-0xA007, LS259)
    // -----------------------------------------------------------------------

    fn write_video_latch(&mut self, bit: u8, value: bool) {
        match bit {
            0 => {
                if value {
                    self.starfield_scroll_x |= 1;
                } else {
                    self.starfield_scroll_x &= !1;
                }
            }
            1 => {
                if value {
                    self.starfield_scroll_x |= 2;
                } else {
                    self.starfield_scroll_x &= !2;
                }
            }
            2 => {
                if value {
                    self.starfield_scroll_x |= 4;
                } else {
                    self.starfield_scroll_x &= !4;
                }
            }
            3 => self.star_set_a = if value { 1 } else { 0 },
            4 => self.star_set_b = if value { 3 } else { 2 }, // Q4 | 2
            5 => {
                // _STARCLR: low resets LFSR, high enables starfield
                if !value {
                    self.star_lfsr = LFSR_SEED;
                }
                self.starfield_enabled = value;
            }
            7 => self.board.flip_screen = value,
            _ => {} // 6: unused
        }
    }

    // -----------------------------------------------------------------------
    // Tilemap addressing (same as Pac-Man / Dig Dug)
    // -----------------------------------------------------------------------

    fn tilemap_offset(col: i32, row: i32) -> usize {
        let r = row + 2;
        let c = col - 2;
        if c & 0x20 != 0 {
            (r + ((c & 0x1F) << 5)) as usize
        } else {
            (c + (r << 5)) as usize
        }
    }

    // -----------------------------------------------------------------------
    // Starfield LFSR
    // -----------------------------------------------------------------------

    /// Advance the 16-bit Fibonacci LFSR by one step.
    /// Taps at bits 16, 13, 11, 6 (maximal period = 65535).
    #[inline]
    fn lfsr_next(lfsr: u16) -> u16 {
        let bit = (lfsr ^ (lfsr >> 3) ^ (lfsr >> 5) ^ (lfsr >> 10)) & 1;
        (lfsr >> 1) | (bit << 15)
    }

    // -----------------------------------------------------------------------
    // Full-frame video rendering
    // -----------------------------------------------------------------------

    fn render_video(&mut self) {
        // 1. Fill with black background.  Galaga palette entry 0 is NOT black
        //    (PROM byte 0 = 0xF6 → near-white), so we use an index in the
        //    unused padding range (96-127) which is always (0,0,0).
        const BACKGROUND_PEN: u8 = 96;
        self.native_buffer.fill(BACKGROUND_PEN);

        // 2. Starfield (background)
        self.render_starfield();

        // 3. Sprites (middle layer)
        self.render_sprites();

        // 4. Tilemap (foreground, on top)
        self.render_tilemap();
    }

    fn render_starfield(&mut self) {
        if !self.starfield_enabled {
            return;
        }

        // Galaga: scroll_y is always 0 (SCROLL_Y pins tied to ground)
        let scroll_y_index: usize = 0;

        let pre_vis = (PRE_VIS_CYCLE_COUNT[scroll_y_index]
            + SPEED_X_CYCLE_COUNT_OFFSET[self.starfield_scroll_x as usize])
            as u32;
        let post_vis = POST_VIS_CYCLE_COUNT[scroll_y_index] as u32;

        // Advance LFSR during pre-visible portion
        for _ in 0..pre_vis {
            self.star_lfsr = Self::lfsr_next(self.star_lfsr);
        }

        // Visible portion: 224 lines × 256 pixels
        for y in 0..VISIBLE_LINES {
            for x in STARFIELD_X_OFFSET..(STARFIELD_PIXEL_WIDTH + STARFIELD_X_OFFSET) {
                if (self.star_lfsr & LFSR_HIT_MASK) == LFSR_HIT_VALUE {
                    let star_set = ((self.star_lfsr >> 10) & 1) as u8
                        | (((self.star_lfsr >> 8) & 1) << 1) as u8;

                    if (self.star_set_a == star_set || self.star_set_b == star_set)
                        && x < STARFIELD_X_LIMIT
                    {
                        let dx = x as usize;
                        let dy = y as usize;
                        if dx < 288 && dy < 224 {
                            let color = (((self.star_lfsr >> 5) & 0x7)
                                | ((self.star_lfsr << 3) & 0x18)
                                | ((self.star_lfsr << 2) & 0x20))
                                as u8;
                            let color = (!color) & 0x3F;
                            // Star colors start at index 32 in combined palette
                            self.native_buffer[dy * 288 + dx] = 32 + color;
                        }
                    }
                }
                self.star_lfsr = Self::lfsr_next(self.star_lfsr);
            }
        }

        // Advance LFSR during post-visible portion
        for _ in 0..post_vis {
            self.star_lfsr = Self::lfsr_next(self.star_lfsr);
        }
    }

    fn render_tilemap(&mut self) {
        for tile_row in 0..28 {
            for tile_col in 0..36 {
                let offset = Self::tilemap_offset(tile_col as i32, tile_row as i32);
                if offset >= 0x400 {
                    continue;
                }

                let code = (self.video_ram[offset] & 0x7F) as usize;
                let color = (self.video_ram[offset + 0x400] & 0x3F) as usize;

                let px_base = tile_col * 8;
                let py_base = tile_row * 8;

                for py in 0..8 {
                    let screen_y = py_base + py;
                    if screen_y >= 224 {
                        continue;
                    }
                    let row_off = screen_y * 288;
                    for px in 0..8 {
                        let screen_x = px_base + px;
                        if screen_x >= 288 {
                            continue;
                        }
                        let pixel = self.char_cache.pixel(code, px, py);
                        let lut_idx = (color * 4 + pixel as usize) & 0xFF;
                        let lut_val = self.char_lut[lut_idx];
                        // Transparent when low nibble == 0x0F
                        if lut_val & 0x0F == 0x0F {
                            continue;
                        }
                        // Character palette uses entries 0x10-0x1F
                        let palette_idx = (lut_val & 0x0F) | 0x10;
                        self.native_buffer[row_off + screen_x] = palette_idx;
                    }
                }
            }
        }
    }

    fn render_sprites(&mut self) {
        // Tile offset table for 2×2 grid: [row][col]
        const GFX_OFFS: [[usize; 2]; 2] = [[0, 1], [2, 3]];

        for offs in (0..0x80).step_by(2) {
            let attr_addr = 0x380 + offs;
            if attr_addr + 1 >= 0x400 {
                continue;
            }

            let sprite = (self.ram1[attr_addr] & 0x7F) as usize;
            let color = (self.ram1[attr_addr + 1] & 0x3F) as usize;
            let sx = self.ram2[attr_addr + 1] as i32 - 40
                + 0x100 * (self.ram3[attr_addr + 1] & 3) as i32;
            let raw_sy = 256i32 - self.ram2[attr_addr] as i32 + 1;
            let flipx = (self.ram3[attr_addr] & 0x01) != 0;
            let flipy = (self.ram3[attr_addr] & 0x02) != 0;
            let sizex = ((self.ram3[attr_addr] >> 2) & 1) as usize;
            let sizey = ((self.ram3[attr_addr] >> 3) & 1) as usize;

            let sy = (raw_sy - 16 * sizey as i32) & 0xFF;
            let sy = sy - 32; // fix wraparound (same as MAME)

            for gy in 0..=sizey {
                for gx in 0..=sizex {
                    let tile_code = sprite
                        + GFX_OFFS[gy ^ (sizey * flipy as usize)][gx ^ (sizex * flipx as usize)];

                    let tile_sx = sx + (gx as i32) * 16;
                    let tile_sy = sy + (gy as i32) * 16;

                    self.draw_sprite_tile(tile_code, color, tile_sx, tile_sy, flipx, flipy);
                }
            }
        }
    }

    fn draw_sprite_tile(
        &mut self,
        code: usize,
        color: usize,
        sx: i32,
        sy: i32,
        flipx: bool,
        flipy: bool,
    ) {
        if code >= self.sprite_cache.count() {
            return;
        }

        let tile_h = 16;
        let tile_w = 16;

        for py in 0..tile_h {
            let screen_y = sy + py as i32;
            if !(0..224).contains(&screen_y) {
                continue;
            }
            let src_py = if flipy { tile_h - 1 - py } else { py };
            let row_off = screen_y as usize * 288;

            for px in 0..tile_w {
                let screen_x = sx + px as i32;
                // Clip to visible area (columns 16-271)
                if !(16..272).contains(&screen_x) {
                    continue;
                }

                let src_px = if flipx { tile_w - 1 - px } else { px };
                let pixel = self.sprite_cache.pixel(code, src_px, src_py);

                let lut_idx = (color * 4 + pixel as usize) & 0xFF;
                let lut_val = self.sprite_lut[lut_idx];
                // Transparent when low nibble == 0x0F
                if lut_val & 0x0F == 0x0F {
                    continue;
                }
                // Sprite palette uses entries 0x00-0x0F
                let palette_idx = lut_val & 0x0F;
                self.native_buffer[row_off + screen_x as usize] = palette_idx;
            }
        }
    }

    /// Update starfield scroll parameters at vblank (called at end of frame).
    fn update_starfield_at_vblank(&mut self) {
        // Starfield scroll is read from the video latch at vblank time.
        // In Galaga, SCROLL_Y is tied to ground, so only X scrolling applies.
        // The latch values are already stored by write_video_latch().
        // Nothing additional needed — the latch state is consumed by render_starfield()
        // on the next frame.
    }
}

impl Default for GalagaSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus implementation
// ---------------------------------------------------------------------------

impl Bus for GalagaSystem {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        match addr {
            0x0000..=0x3FFF => self.board.read_rom(master, addr),
            0x6800..=0x6807 => {
                // DIP switch reads (active-low, accent via 51XX/53XX).
                // Direct reads at 0x6800-0x6807 return the DIP switch bits;
                // Galaga's bosco_dsw_r reads bits from DSWA/DSWB based on address.
                // However, the game primarily reads DIP switches through the 53XX.
                // Return 0xFF for now (matches common behavior).
                0xFF
            }
            0x7000..=0x70FF => self.board.read_custom_io(),
            0x7100 => self.board.namco06.ctrl_read(),
            0x8000..=0x87FF => self.video_ram[(addr - 0x8000) as usize],
            0x8800..=0x8BFF => self.ram1[(addr - 0x8800) as usize],
            0x9000..=0x93FF => self.ram2[(addr - 0x9000) as usize],
            0x9800..=0x9BFF => self.ram3[(addr - 0x9800) as usize],
            0xA000..=0xA007 => 0, // video latch (write-only)
            _ => 0xFF,
        }
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        match addr {
            0x0000..=0x3FFF => {} // ROM (nopw)
            0x6800..=0x681F => {
                self.board.wsg.write(addr - 0x6800, data);
            }
            0x6820..=0x6827 => {
                let bit = (addr & 7) as u8;
                let value = (data & 1) != 0;
                self.board.write_misc_latch(bit, value);
            }
            0x6830 => {
                self.board.watchdog_counter = 0;
            }
            0x7000..=0x70FF => {
                self.board.write_custom_io(data);
            }
            0x7100 => {
                self.board.write_custom_io_ctrl(data);
            }
            0x8000..=0x87FF => {
                self.video_ram[(addr - 0x8000) as usize] = data;
            }
            0x8800..=0x8BFF => {
                self.ram1[(addr - 0x8800) as usize] = data;
            }
            0x9000..=0x93FF => {
                self.ram2[(addr - 0x9000) as usize] = data;
            }
            0x9800..=0x9BFF => {
                self.ram3[(addr - 0x9800) as usize] = data;
            }
            0xA000..=0xA007 => {
                let bit = (addr & 7) as u8;
                let value = (data & 1) != 0;
                self.write_video_latch(bit, value);
            }
            _ => {}
        }
    }

    fn is_halted_for(&self, master: BusMaster) -> bool {
        self.board.is_halted_for(master)
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        self.board.check_interrupts(target)
    }
}

// ---------------------------------------------------------------------------
// Trait implementations
// ---------------------------------------------------------------------------

impl Renderable for GalagaSystem {
    fn display_size(&self) -> (u32, u32) {
        namco_galaga::TIMING.display_size()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        gfx::rotate_90_ccw_indexed(
            &self.native_buffer,
            buffer,
            288,
            224,
            &self.combined_palette,
        );
    }
}

impl AudioSource for GalagaSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.board.fill_audio(buffer)
    }

    fn audio_sample_rate(&self) -> u32 {
        44100
    }
}

impl InputReceiver for GalagaSystem {
    fn set_input(&mut self, button: u8, pressed: bool) {
        self.board.handle_input(button, pressed);
    }

    fn input_map(&self) -> &[phosphor_core::core::machine::InputButton] {
        namco_galaga::NAMCO_GALAGA_INPUT_MAP
    }
}

impl BusDebug for GalagaSystem {
    fn devices(&self) -> Vec<(&str, &dyn Debuggable)> {
        vec![
            ("Z80 Main", &self.board.main_cpu as &dyn Debuggable),
            ("Z80 Sub", &self.board.sub_cpu as &dyn Debuggable),
            ("Z80 Sound", &self.board.sound_cpu as &dyn Debuggable),
            ("Namco WSG", &self.board.wsg as &dyn Debuggable),
            ("Namco 06XX", &self.board.namco06 as &dyn Debuggable),
            ("Namco 51XX", &self.board.namco51 as &dyn Debuggable),
            ("Namco 53XX", &self.board.namco53 as &dyn Debuggable),
        ]
    }

    fn cpus(&self) -> Vec<(&str, &dyn DebugCpu)> {
        vec![
            ("Z80 Main", &self.board.main_cpu as &dyn DebugCpu),
            ("Z80 Sub", &self.board.sub_cpu as &dyn DebugCpu),
            ("Z80 Sound", &self.board.sound_cpu as &dyn DebugCpu),
        ]
    }

    fn read(&self, cpu_index: usize, addr: u16) -> Option<u8> {
        match addr {
            0x0000..=0x3FFF => {
                let offset = addr as usize;
                let rom = match cpu_index {
                    0 => &self.board.main_rom,
                    1 => &self.board.sub_rom,
                    2 => &self.board.sound_rom,
                    _ => return None,
                };
                Some(rom.get(offset).copied().unwrap_or(0xFF))
            }
            0x8000..=0x87FF => Some(self.video_ram[(addr - 0x8000) as usize]),
            0x8800..=0x8BFF => Some(self.ram1[(addr - 0x8800) as usize]),
            0x9000..=0x93FF => Some(self.ram2[(addr - 0x9000) as usize]),
            0x9800..=0x9BFF => Some(self.ram3[(addr - 0x9800) as usize]),
            _ => None,
        }
    }

    fn write(&mut self, _cpu_index: usize, addr: u16, data: u8) {
        match addr {
            0x8000..=0x87FF => self.video_ram[(addr - 0x8000) as usize] = data,
            0x8800..=0x8BFF => self.ram1[(addr - 0x8800) as usize] = data,
            0x9000..=0x93FF => self.ram2[(addr - 0x9000) as usize] = data,
            0x9800..=0x9BFF => self.ram3[(addr - 0x9800) as usize] = data,
            _ => {}
        }
    }
}

impl MachineDebug for GalagaSystem {
    fn debug_bus(&self) -> Option<&dyn BusDebug> {
        Some(self)
    }

    fn debug_bus_mut(&mut self) -> Option<&mut dyn BusDebug> {
        Some(self)
    }

    fn cycles_per_frame(&self) -> u64 {
        namco_galaga::TIMING.cycles_per_frame()
    }

    fn debug_tick(&mut self) -> u32 {
        bus_split!(self, bus => {
            self.board.tick(bus);
        });
        self.board.debug_tick_boundaries()
    }
}

impl MachineCore for GalagaSystem {
    crate::machine_core_metadata!("galaga", namco_galaga::TIMING);

    fn run_frame(&mut self) {
        bus_split!(self, bus => {
            for _ in 0..namco_galaga::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });
        self.update_starfield_at_vblank();
        self.render_video();
    }

    fn reset(&mut self) {
        self.board.reset_board();
        self.video_ram.fill(0);
        self.ram1.fill(0);
        self.ram2.fill(0);
        self.ram3.fill(0);
        self.starfield_scroll_x = 0;
        self.star_set_a = 0;
        self.star_set_b = 0;
        self.starfield_enabled = false;
        self.star_lfsr = LFSR_SEED;
        self.native_buffer.fill(0);

        bus_split!(self, bus => {
            self.board.main_cpu.reset(bus, BusMaster::Cpu(0));
            self.board.sub_cpu.reset(bus, BusMaster::Cpu(1));
            self.board.sound_cpu.reset(bus, BusMaster::Cpu(2));
        });
    }
}

impl SaveState for GalagaSystem {
    crate::machine_save_state!();
}

crate::impl_default_frontend_capabilities!(GalagaSystem);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

const ALL_CONFIGS: &[&GalagaRomConfig] = &[&GALAGA_CONFIG, &GALAGAO_CONFIG, &GALAGAMW_CONFIG];

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut last_err = None;
    for config in ALL_CONFIGS {
        let mut sys = GalagaSystem::new();
        match sys.load_roms(rom_set, config) {
            Ok(()) => return Ok(Box::new(sys)),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap())
}

inventory::submit! {
    MachineEntry::new(
        "galaga",
        &["galaga", "galagao", "galagamw"],
        create_machine,
    )
}
