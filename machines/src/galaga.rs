use phosphor_core::core::address_space::AccessKind;
use phosphor_core::core::address_space16::WriteAnnotation;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug::{BusDebug, DebugCpu, Debuggable};
use phosphor_core::core::debug_trace::DebugEventKind;
use phosphor_core::core::machine::{
    AudioSource, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, MachineCore, MachineDebug,
    Renderable, SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::gfx::GfxCache;
use phosphor_core::gfx::decode::{GfxLayout, decode_gfx};
use phosphor_macros::{MemoryRegion, Saveable};

use crate::namco_galaga::{
    self, GALAGA_SPRITE_LAYOUT, GalagaCpus, NamcoGalagaBoard, NamcoGalagaBus, ScanlineGame,
};
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

/// The starfield control latch as it stood at the top of the visible area.
///
/// See [`GalagaSystem::star_frame`] for why this layer is sampled once a frame
/// while every other layer on this board is read per row.
#[derive(Clone, Copy, Default)]
struct StarFrame {
    enabled: bool,
    scroll_x: u8,
    set_a: u8,
    set_b: u8,
}

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
/// Galaga's RAM windows, declared on the shared board's address map.
///
/// Ids start at 4: 0 is the core's unmapped sentinel and 1-3 are the board's
/// per-CPU ROMs ([`namco_galaga::Region`]). The names given here are what the
/// debugger shows against every write and watchpoint hit in these windows.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum Region {
    VideoRam = 4,
    Ram1 = 5,
    Ram2 = 6,
    Ram3 = 7,
}

#[derive(Saveable)]
pub struct GalagaSystem {
    /// The three Z80s. Held beside the bus, not inside it, so their cycles
    /// dispatch to a concrete `GalagaBus` (see [`GalagaSystem::split`]).
    pub cpus: GalagaCpus,

    pub board: NamcoGalagaBoard,

    // Video latch state (0xA000-0xA007 LS259)
    starfield_scroll_x: u8,  // Q0-Q2: X scroll speed index
    star_set_a: u8,          // Q3
    star_set_b: u8,          // Q4 (OR'd with 2 per MAME)
    starfield_enabled: bool, // Q5: _STARCLR (active-high enable)

    // Starfield generator state (Namco 05XX)
    star_lfsr: u16,

    /// The four starfield control bits as they stood when the beam reached the
    /// first visible line, held for the rest of the frame.
    ///
    /// This is the one layer on this board that is NOT read per row, and the
    /// reason is that nothing establishes it may be. The starfield is a
    /// free-running shift register whose output position is a function of how
    /// many times it has been clocked since the frame began: `pre_vis` (which
    /// the scroll index perturbs by -4..+3) then 224 rows of 256, then
    /// `post_vis`. Reading the scroll index per row would change how far the
    /// register had advanced by the *next* row, so it does not merely recolour
    /// a row, it moves every star below it.
    ///
    /// Whether the 05XX re-reads its control latch per line is not on any
    /// drawing: the Galaga video sheet names 4M as the starfield generator and
    /// it is a Namco custom LSI, the same dead end MMC02 is for `mcr2` (see
    /// `docs/schematics/mcr-video-timing.md`). So this keeps exactly the
    /// whole-frame semantics the board had before it drew per row, including
    /// the LFSR not advancing at all on a frame where the field is disabled,
    /// and the question is left open rather than guessed at.
    ///
    /// Derived per frame, so not saved; re-sampled at the next row 0.
    #[save_skip]
    star_frame: StarFrame,

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
        let mut board = NamcoGalagaBoard::new();
        // The three CPUs share these windows; the split into four chips is the
        // sprite hardware's, which reads attributes, positions and flip/size
        // bits from three separate RAMs at a fixed 0x380 offset.
        board
            .map
            .region(
                Region::VideoRam,
                "Video RAM",
                0x8000,
                0x800,
                AccessKind::ReadWrite,
            )
            .region(
                Region::Ram1,
                "Work RAM / Sprite Attributes",
                0x8800,
                0x400,
                AccessKind::ReadWrite,
            )
            .region(
                Region::Ram2,
                "Work RAM / Sprite Positions",
                0x9000,
                0x400,
                AccessKind::ReadWrite,
            )
            .region(
                Region::Ram3,
                "Work RAM / Sprite Flip+Size",
                0x9800,
                0x400,
                AccessKind::ReadWrite,
            );

        Self {
            cpus: GalagaCpus::new(),
            board,

            starfield_scroll_x: 0,
            star_set_a: 0,
            star_set_b: 0,
            starfield_enabled: false,

            star_lfsr: LFSR_SEED,
            star_frame: StarFrame::default(),

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

    /// Borrow the CPUs and the bus they drive as two disjoint pieces.
    ///
    /// The starfield latch is bus state (the Z80 writes it at 0xA000-0xA007)
    /// while the caches and framebuffer are not, so the bus is a view over the
    /// board plus those few fields. The borrow checker verifies the split — no
    /// raw pointers — and dispatch through the view is concrete.
    #[inline]
    fn split(&mut self) -> (&mut GalagaCpus, GalagaBus<'_>) {
        (
            &mut self.cpus,
            GalagaBus {
                board: &mut self.board,
                starfield_scroll_x: &mut self.starfield_scroll_x,
                star_set_a: &mut self.star_set_a,
                star_set_b: &mut self.star_set_b,
                starfield_enabled: &mut self.starfield_enabled,
                star_lfsr: &mut self.star_lfsr,
            },
        )
    }

    // -----------------------------------------------------------------------
    // Tilemap addressing (same as Pac-Man / Dig Dug)
    // -----------------------------------------------------------------------

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

    /// Draw one visible row, out of the video state as it stands at that row's
    /// scanline boundary.
    ///
    /// The layers run in the board's order for this one row: backdrop,
    /// starfield, sprites, tilemap. Video RAM, the three sprite attribute RAMs
    /// and the colour LUTs are all read here rather than once at the frame
    /// boundary, so a write partway down the screen affects only the rows below
    /// it. The starfield is the exception, and says why on
    /// [`star_frame`](Self::star_frame).
    fn render_scanline(&mut self, y: usize) {
        // Galaga palette entry 0 is NOT black (PROM byte 0 = 0xF6 → near-white),
        // so the backdrop is an index in the unused padding range (96-127),
        // which is always (0,0,0).
        const BACKGROUND_PEN: u8 = 96;

        if y == 0 {
            self.begin_starfield_frame();
        }

        let row_off = y * 288;
        self.native_buffer[row_off..row_off + 288].fill(BACKGROUND_PEN);

        self.render_starfield_row(y);
        self.render_sprites_row(y);
        self.render_tilemap_row(y);

        if y as u16 == VISIBLE_LINES - 1 {
            self.end_starfield_frame();
        }
    }

    /// Latch the starfield controls for this frame and clock the generator
    /// through the pre-visible part of the raster.
    ///
    /// The scroll index only ever reaches the picture here, as a perturbation of
    /// -4..+3 on how far the shift register has advanced before the first
    /// visible pixel. That is what makes the field appear to scroll.
    fn begin_starfield_frame(&mut self) {
        self.star_frame = StarFrame {
            enabled: self.starfield_enabled,
            scroll_x: self.starfield_scroll_x,
            set_a: self.star_set_a,
            set_b: self.star_set_b,
        };
        if !self.star_frame.enabled {
            return;
        }
        // Galaga: scroll_y is always 0 (SCROLL_Y pins tied to ground)
        let pre_vis = (PRE_VIS_CYCLE_COUNT[0]
            + SPEED_X_CYCLE_COUNT_OFFSET[self.star_frame.scroll_x as usize])
            as u32;
        for _ in 0..pre_vis {
            self.star_lfsr = Self::lfsr_next(self.star_lfsr);
        }
    }

    /// Clock the generator through the post-visible part of the raster, so the
    /// next frame starts where the hardware's would.
    fn end_starfield_frame(&mut self) {
        if !self.star_frame.enabled {
            return;
        }
        for _ in 0..POST_VIS_CYCLE_COUNT[0] as u32 {
            self.star_lfsr = Self::lfsr_next(self.star_lfsr);
        }
    }

    /// One row of the starfield: 256 shift-register clocks, drawn where they
    /// hit.
    ///
    /// The register is clocked for every pixel of the row whether or not a star
    /// lands, because its position in the sequence is the picture. A row is
    /// therefore not independent of the rows above it, which is why this layer
    /// keeps a per-frame latch while the others read live.
    fn render_starfield_row(&mut self, y: usize) {
        if !self.star_frame.enabled {
            return;
        }
        for x in STARFIELD_X_OFFSET..(STARFIELD_PIXEL_WIDTH + STARFIELD_X_OFFSET) {
            if (self.star_lfsr & LFSR_HIT_MASK) == LFSR_HIT_VALUE {
                let star_set =
                    ((self.star_lfsr >> 10) & 1) as u8 | (((self.star_lfsr >> 8) & 1) << 1) as u8;

                if (self.star_frame.set_a == star_set || self.star_frame.set_b == star_set)
                    && x < STARFIELD_X_LIMIT
                {
                    let dx = x as usize;
                    if dx < 288 && y < 224 {
                        let color = (((self.star_lfsr >> 5) & 0x7)
                            | ((self.star_lfsr << 3) & 0x18)
                            | ((self.star_lfsr << 2) & 0x20))
                            as u8;
                        let color = (!color) & 0x3F;
                        // Star colors start at index 32 in combined palette
                        self.native_buffer[y * 288 + dx] = 32 + color;
                    }
                }
            }
            self.star_lfsr = Self::lfsr_next(self.star_lfsr);
        }
    }

    fn render_tilemap_row(&mut self, scanline: usize) {
        // 36×28 foreground tilemap of 8×8 chars, composited on top of the star +
        // sprite layers via the shared index-writing scanline helper. The Namco
        // offset never reaches 0x400 within the visible grid, and this tilemap has
        // no per-tile flip. Character pens land in palette entries 0x10-0x1F; a
        // LUT low-nibble of 0x0F is transparent.
        let config = phosphor_core::gfx::TilemapConfig {
            cols: 36,
            rows: 28,
            tile_width: 8,
            tile_height: 8,
        };
        // A cache with no entries has no pixels to index, which a board built
        // without graphics ROMs would otherwise walk into once every scanline
        // draws.
        if self.char_cache.count() == 0 {
            return;
        }
        // Split borrows: closures read VRAM + LUT, the helper writes native_buffer.
        let video_ram = self.board.map.region_data(Region::VideoRam);
        let char_lut = &self.char_lut;
        let char_cache = &self.char_cache;
        let native = &mut self.native_buffer;
        let mut prio = [0u8; 288];
        {
            let row_off = scanline * 288;
            let row = &mut native[row_off..row_off + 288];
            phosphor_core::gfx::render_tilemap_scanline_indexed(
                &config,
                char_cache,
                scanline,
                |col, row| {
                    let offset = crate::namco_video::namco_tilemap_offset(col as i32, row as i32);
                    let code = (video_ram[offset] & 0x7F) as u16;
                    let color = video_ram[offset + 0x400] & 0x3F;
                    phosphor_core::gfx::TileInfo::new(code, color)
                },
                |color, pixel| {
                    let lut_val = char_lut[(color as usize * 4 + pixel as usize) & 0xFF];
                    if lut_val & 0x0F == 0x0F {
                        None
                    } else {
                        Some(((lut_val & 0x0F) | 0x10, 0))
                    }
                },
                row,
                &mut prio,
                0,
            );
        }
    }

    /// One row of the sprite layer.
    ///
    /// Slots are still visited in list order, and the two-by-two tile grid in
    /// the same order within a slot, so the pixel a row ends up with is the one
    /// the whole-frame pass produced from the same attribute RAM: only the
    /// *moment* the RAM is read has moved. `draw_sprite_tile_row` rejects the
    /// tiles that are not on this line.
    ///
    /// The list is read as of this row. The 04XX walks it into a line buffer
    /// displayed on the next line, so a slot whose attributes change mid-screen
    /// would appear one row early here. That lead is deliberately not added:
    /// `raw_sy` below is `256 - y + 1`, and the `+ 1` is already the one-line
    /// delay, so folding it in again would double it (W3 of the raster-sampling
    /// epic).
    fn render_sprites_row(&mut self, y: usize) {
        // Tile offset table for 2×2 grid: [row][col]
        const GFX_OFFS: [[usize; 2]; 2] = [[0, 1], [2, 3]];

        for offs in (0..0x80).step_by(2) {
            let attr_addr = 0x380 + offs;
            if attr_addr + 1 >= 0x400 {
                continue;
            }

            // The sprite hardware reads one attribute pair from each of the
            // three RAMs at the same offset; copy the six bytes out before
            // touching the frame buffer so the map stays immutably borrowed.
            let (ram1, ram2, ram3) = (
                self.board.map.region_data(Region::Ram1),
                self.board.map.region_data(Region::Ram2),
                self.board.map.region_data(Region::Ram3),
            );
            let sprite = (ram1[attr_addr] & 0x7F) as usize;
            let color = (ram1[attr_addr + 1] & 0x3F) as usize;
            let sx = ram2[attr_addr + 1] as i32 - 40 + 0x100 * (ram3[attr_addr + 1] & 3) as i32;
            let raw_sy = 256i32 - ram2[attr_addr] as i32 + 1;
            let flipx = (ram3[attr_addr] & 0x01) != 0;
            let flipy = (ram3[attr_addr] & 0x02) != 0;
            let sizex = ((ram3[attr_addr] >> 2) & 1) as usize;
            let sizey = ((ram3[attr_addr] >> 3) & 1) as usize;

            let sy = (raw_sy - 16 * sizey as i32) & 0xFF;
            let sy = sy - 32; // fix wraparound (same as MAME)

            // Reject the slot before walking its 2x2 grid. Without this the grid
            // loop and the per-tile setup in `draw_sprite_tile_row` run 224 times
            // per slot to draw at most 32 rows of it. The attribute bytes above
            // are still read on every row, which is the point of the migration;
            // only the work that cannot produce a pixel is skipped.
            let slot_h = 16 * (1 + sizey) as i32;
            if !(sy..sy + slot_h).contains(&(y as i32)) {
                continue;
            }

            for gy in 0..=sizey {
                for gx in 0..=sizex {
                    let tile_code = sprite
                        + GFX_OFFS[gy ^ (sizey * flipy as usize)][gx ^ (sizex * flipx as usize)];

                    let tile_sx = sx + (gx as i32) * 16;
                    let tile_sy = sy + (gy as i32) * 16;

                    self.draw_sprite_tile_row(tile_code, color, tile_sx, tile_sy, flipx, flipy, y);
                }
            }
        }
    }

    /// Blit the one line of a sprite tile that crosses row `y`, or nothing if it
    /// does not cross it.
    #[allow(clippy::too_many_arguments)]
    fn draw_sprite_tile_row(
        &mut self,
        code: usize,
        color: usize,
        sx: i32,
        sy: i32,
        flipx: bool,
        flipy: bool,
        y: usize,
    ) {
        if code >= self.sprite_cache.count() {
            return;
        }

        let tile_h = 16usize;
        let py = y as i32 - sy;
        if !(0..tile_h as i32).contains(&py) {
            return;
        }
        let py = py as usize;
        // Clip to the visible area (columns 16..272), no tunnel wrap.
        let clip = phosphor_core::gfx::SpriteClip {
            x_min: 16,
            x_max: 272,
            wrap_offset: None,
        };
        // Split borrows so the LUT closures don't hold &self while the index
        // buffer (native_buffer) is borrowed mutably. Sprite pens are 0x00-0x0F;
        // a LUT low-nibble of 0x0F is transparent. Galaga composites by draw
        // order (no priority buffer), so priority is a scratch value.
        let sprite_lut = &self.sprite_lut;
        let sprite_cache = &self.sprite_cache;
        let native = &mut self.native_buffer;
        let is_transparent =
            |pixel: u8| sprite_lut[(color * 4 + pixel as usize) & 0xFF] & 0x0F == 0x0F;
        let resolve = |pixel: u8| (sprite_lut[(color * 4 + pixel as usize) & 0xFF] & 0x0F, 0u8);
        let mut prio = [0u8; 288];

        if y >= 224 {
            return;
        }
        let src_py = if flipy { tile_h - 1 - py } else { py };
        let row_off = y * 288;
        let row = &mut native[row_off..row_off + 288];
        phosphor_core::gfx::draw_sprite_row_indexed(
            sprite_cache,
            code as u16,
            src_py,
            sx,
            flipx,
            is_transparent,
            resolve,
            row,
            &mut prio,
            &clip,
        );
    }
}

/// The beam drive lives on the shared board; this supplies only what is Galaga's
/// own. The delegating bodies name `GalagaSystem::` explicitly because the
/// inherent methods and the trait methods share their names: inherent resolution
/// would pick the right one anyway, but not visibly.
impl ScanlineGame for GalagaSystem {
    type Bus<'a> = GalagaBus<'a>;

    fn split(&mut self) -> (&mut GalagaCpus, GalagaBus<'_>) {
        GalagaSystem::split(self)
    }

    fn board(&self) -> &NamcoGalagaBoard {
        &self.board
    }

    fn render_scanline(&mut self, y: usize) {
        GalagaSystem::render_scanline(self, y);
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

/// Label a bus write for the event trace. The shared board latches and the
/// custom-I/O window are common to the platform; 0xA000-0xA007 is Galaga's
/// own video latch (starfield scroll + flip), so it is decoded here rather
/// than on the board — other games on this hardware put RAM there.
pub(crate) fn write_annotation(addr: u16) -> WriteAnnotation {
    match addr {
        0x6800..=0x681F => WriteAnnotation::device("Namco WSG"),
        0x6820..=0x6827 => WriteAnnotation::detail(
            "Misc latch",
            match addr & 7 {
                0 => "main IRQ enable",
                1 => "sub IRQ enable",
                2 => "sound NMI enable",
                3 => "sub/sound reset",
                _ => "latch bit",
            },
        ),
        0x6830..=0x683F => WriteAnnotation {
            kind: DebugEventKind::Watchdog,
            detail: Some("watchdog cleared"),
            ..WriteAnnotation::MEMORY
        },
        // The custom-I/O helpers record these transactions themselves, tagged
        // with the 06XX-selected chip; suppress the duplicate bus event.
        0x7000..=0x71FF => WriteAnnotation::SUPPRESSED,
        0xA000..=0xA007 => WriteAnnotation::device("Video latch"),
        _ => WriteAnnotation::MEMORY,
    }
}

/// The Galaga bus: the shared board plus the starfield latch the Z80 writes.
///
/// Crate-visible because it is this game's `ScanlineGame::Bus`.
pub(crate) struct GalagaBus<'a> {
    board: &'a mut NamcoGalagaBoard,
    starfield_scroll_x: &'a mut u8,
    star_set_a: &'a mut u8,
    star_set_b: &'a mut u8,
    starfield_enabled: &'a mut bool,
    star_lfsr: &'a mut u16,
}

impl GalagaBus<'_> {
    // -----------------------------------------------------------------------
    // Video latch (0xA000-0xA007, LS259)
    // -----------------------------------------------------------------------

    fn write_video_latch(&mut self, bit: u8, value: bool) {
        match bit {
            0 => {
                if value {
                    *self.starfield_scroll_x |= 1;
                } else {
                    *self.starfield_scroll_x &= !1;
                }
            }
            1 => {
                if value {
                    *self.starfield_scroll_x |= 2;
                } else {
                    *self.starfield_scroll_x &= !2;
                }
            }
            2 => {
                if value {
                    *self.starfield_scroll_x |= 4;
                } else {
                    *self.starfield_scroll_x &= !4;
                }
            }
            3 => *self.star_set_a = if value { 1 } else { 0 },
            4 => *self.star_set_b = if value { 3 } else { 2 }, // Q4 | 2
            5 => {
                // _STARCLR: low resets LFSR, high enables starfield
                if !value {
                    *self.star_lfsr = LFSR_SEED;
                }
                *self.starfield_enabled = value;
            }
            7 => self.board.flip_screen = value,
            _ => {} // 6: unused
        }
    }
}

impl NamcoGalagaBus for GalagaBus<'_> {
    #[inline]
    fn board(&mut self) -> &mut NamcoGalagaBoard {
        self.board
    }
}

impl Bus for GalagaBus<'_> {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let data = match addr {
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
            // RAM windows resolve through the map, which turns the address into
            // the right region and offset (see the Region declarations in new).
            0x8000..=0x87FF | 0x8800..=0x8BFF | 0x9000..=0x93FF | 0x9800..=0x9BFF => {
                self.board.map.read_backing(addr)
            }
            0xA000..=0xA007 => 0, // video latch (write-only)
            _ => 0xFF,
        };
        self.board.watch_read(master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        // Check the watchpoint before the side effect so the hit records
        // pre-write state (WatchpointPhase::Before); trace alongside.
        self.board
            .watch_write_annotated(master, addr, data, write_annotation(addr));
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
            0x8000..=0x87FF | 0x8800..=0x8BFF | 0x9000..=0x93FF | 0x9800..=0x9BFF => {
                self.board.map.write_backing(addr, data);
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
        // Native (unrotated) 288×224 framebuffer. Galaga (like Dig Dug and
        // Xevious on the same Namco board) declares ROT90 and lets the frontend
        // rotate centrally, so it reports the native landscape size rather than
        // `namco_galaga::TIMING`'s pre-swapped portrait size (224×288).
        (288, 224)
    }

    fn display_aspect(&self) -> Option<(u32, u32)> {
        namco_galaga::TIMING.display_aspect()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        // Native RGB24 in row-major order; the ROT90 the cabinet needs is applied
        // centrally by the frontend (see `orientation`), not baked here.
        let mask = self.combined_palette.len() - 1;
        for (i, &idx) in self.native_buffer.iter().enumerate() {
            let (r, g, b) = self.combined_palette[idx as usize & mask];
            buffer[i * 3] = r;
            buffer[i * 3 + 1] = g;
            buffer[i * 3 + 2] = b;
        }
    }

    fn orientation(&self) -> phosphor_core::core::machine::Orientation {
        // Galaga's monitor is mounted rotated 90°; declared, not baked.
        phosphor_core::core::machine::Orientation::ROT90
    }
}

impl AudioSource for GalagaSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.board.fill_audio(buffer)
    }

    fn audio_sample_rate(&self) -> u32 {
        phosphor_core::audio::host_sample_rate()
    }
}

impl BusDebug for GalagaSystem {
    fn devices(&self) -> Vec<(&str, &dyn Debuggable)> {
        vec![
            ("Z80 Main", &self.cpus.main as &dyn Debuggable),
            ("Z80 Sub", &self.cpus.sub as &dyn Debuggable),
            ("Z80 Sound", &self.cpus.sound as &dyn Debuggable),
            ("Namco WSG", &self.board.wsg as &dyn Debuggable),
            ("Namco 06XX", &self.board.namco06 as &dyn Debuggable),
            ("Namco 51XX", &self.board.namco51 as &dyn Debuggable),
            ("Namco 53XX", &self.board.namco53 as &dyn Debuggable),
            ("Clocks", &self.board.clocks as &dyn Debuggable),
        ]
    }

    fn cpus(&self) -> Vec<(&str, &dyn DebugCpu)> {
        vec![
            ("Z80 Main", &self.cpus.main as &dyn DebugCpu),
            ("Z80 Sub", &self.cpus.sub as &dyn DebugCpu),
            ("Z80 Sound", &self.cpus.sound as &dyn DebugCpu),
        ]
    }

    fn read(&self, cpu_index: usize, addr: u32) -> Option<u8> {
        // All three Z80 address spaces are 16-bit.
        let addr = u16::try_from(addr).ok()?;
        match addr {
            0x0000..=0x3FFF => {
                // Each CPU sees its own ROM here; the board selects by master.
                if cpu_index > 2 {
                    return None;
                }
                Some(self.board.read_rom(BusMaster::Cpu(cpu_index), addr))
            }
            0x8000..=0x87FF | 0x8800..=0x8BFF | 0x9000..=0x93FF | 0x9800..=0x9BFF => {
                Some(self.board.map.read_backing(addr))
            }
            _ => None,
        }
    }

    fn write(&mut self, _cpu_index: usize, addr: u32, data: u8) {
        let Ok(addr) = u16::try_from(addr) else {
            return;
        };
        match addr {
            0x8000..=0x87FF | 0x8800..=0x8BFF | 0x9000..=0x93FF | 0x9800..=0x9BFF => {
                self.board.map.write_backing(addr, data)
            }
            _ => {}
        }
    }

    /// Tagged debugger poke: routes RAM writes through the map's `poke` so a
    /// script/console poke records a `DebugAccessSource::Frontend` event in the
    /// trace (the board's DebugTrace surfaces the same map ring).
    fn poke(&mut self, _cpu_index: usize, addr: u32, data: u8) {
        let Ok(addr) = u16::try_from(addr) else {
            return;
        };
        match addr {
            0x8000..=0x87FF | 0x8800..=0x8BFF | 0x9000..=0x93FF | 0x9800..=0x9BFF => {
                self.board.map.poke(addr, data)
            }
            _ => {}
        }
    }

    // --- Watchpoints (owned by the board's shared address space) ---
    // The debugger addresses in u32; anything outside the Z80's 16-bit space
    // cannot be watched, so it is dropped rather than truncated.

    fn take_watchpoint_hit(&mut self) -> Option<phosphor_core::core::watchpoint::WatchpointHit> {
        self.board.map.take_hit()
    }

    fn set_watchpoint(
        &mut self,
        cpu_index: usize,
        addr: u32,
        kind: phosphor_core::core::watchpoint::WatchpointKind,
    ) {
        if let Ok(addr) = u16::try_from(addr) {
            self.board.map.set_watchpoint(cpu_index, addr, kind);
        }
    }

    fn set_watchpoint_cond(
        &mut self,
        cpu_index: usize,
        addr: u32,
        kind: phosphor_core::core::watchpoint::WatchpointKind,
        condition: phosphor_core::core::watchpoint::WatchpointCondition,
    ) {
        if let Ok(addr) = u16::try_from(addr) {
            self.board
                .map
                .set_watchpoint_cond(cpu_index, addr, kind, condition);
        }
    }

    fn clear_watchpoint(
        &mut self,
        cpu_index: usize,
        addr: u32,
        kind: phosphor_core::core::watchpoint::WatchpointKind,
    ) {
        if let Ok(addr) = u16::try_from(addr) {
            self.board.map.clear_watchpoint(cpu_index, addr, kind);
        }
    }

    fn clear_all_watchpoints(&mut self) {
        self.board.map.clear_all_watchpoints();
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
        self.tick_frame_boundary();
        self.cpus.instruction_boundaries(self.board.sub_running())
    }
}

impl MachineCore for GalagaSystem {
    crate::machine_core_metadata!("galaga", namco_galaga::TIMING, namco_galaga::clock_tree);

    fn gfx_sheets(&self) -> Vec<phosphor_core::core::machine::GfxSheet<'_>> {
        use phosphor_core::core::machine::GfxSheet;
        vec![
            GfxSheet {
                name: "chars",
                cache: &self.char_cache,
                palette: &self.board.palette_rgb,
            },
            GfxSheet {
                name: "sprites",
                cache: &self.sprite_cache,
                palette: &self.board.palette_rgb,
            },
        ]
    }

    fn run_frame(&mut self) {
        self.run_frame_scanline_outer();
    }

    fn reset(&mut self) {
        self.board.reset_board();
        for region in [Region::VideoRam, Region::Ram1, Region::Ram2, Region::Ram3] {
            self.board.map.region_data_mut(region).fill(0);
        }
        self.starfield_scroll_x = 0;
        self.star_set_a = 0;
        self.star_set_b = 0;
        self.starfield_enabled = false;
        self.star_lfsr = LFSR_SEED;
        self.native_buffer.fill(0);

        // Power-on reset of all three Z80s. `hardware_reset` is what the board
        // already uses when the misc latch releases the sub/sound CPUs, and it
        // clears the interrupt-enable and execution state that `Cpu::reset`
        // leaves alone — a stronger reset, and one that needs no bus.
        self.cpus.main.hardware_reset();
        self.cpus.sub.hardware_reset();
        self.cpus.sound.hardware_reset();
    }
}

impl SaveState for GalagaSystem {
    crate::machine_save_state!();
}

impl phosphor_core::core::machine::Nvram for GalagaSystem {}
impl phosphor_core::core::machine::InputConfigurable for GalagaSystem {
    fn input_controls(&self) -> &'static [phosphor_core::core::machine::InputControl] {
        namco_galaga::NAMCO_GALAGA_CONTROLS
    }

    fn handle_input(&mut self, event: phosphor_core::core::machine::InputEvent) {
        if let phosphor_core::core::machine::InputEvent::Button { id, pressed } = event {
            self.board.handle_input(id.0 as u8, pressed);
        }
    }
}
impl phosphor_core::core::machine::Profilable for GalagaSystem {}
/// DIP switch metadata for Galaga's two banks (DSWA at board byte `dswa`, DSWB
/// at `dswb`). Choice bits and labels follow MAME's `galaga` layout; the option
/// defaults OR to the historical 0xF7 (DSWA) and 0x97 (DSWB). The two unused
/// DSWA bits (0x04, 0x40) are not modelled and keep their power-on value. The
/// Bonus Life thresholds shown are those MAME displays for the default (non-5)
/// Lives setting.
const GALAGA_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSWA",
        options: &[
            DipOption {
                name: "Difficulty",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Medium",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Hard",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "Hardest",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "Easy",
                        value: 0x03,
                    },
                ],
            },
            DipOption {
                name: "Demo Sounds",
                mask: 0x08,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "On",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Off",
                        value: 0x08,
                    },
                ],
            },
            DipOption {
                name: "Freeze",
                mask: 0x10,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "On",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Off",
                        value: 0x10,
                    },
                ],
            },
            DipOption {
                name: "Rack Test",
                mask: 0x20,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "On",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Off",
                        value: 0x20,
                    },
                ],
            },
            DipOption {
                name: "Cabinet",
                mask: 0x80,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Cocktail",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Upright",
                        value: 0x80,
                    },
                ],
            },
        ],
    },
    DipSwitchBank {
        name: "DSWB",
        options: &[
            DipOption {
                name: "Coinage",
                mask: 0x07,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Free Play",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "2 Coins/3 Credits",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "3 Coins/1 Credit",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "1 Coin/2 Credits",
                        value: 0x03,
                    },
                    DipChoice {
                        label: "4 Coins/1 Credit",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "1 Coin/3 Credits",
                        value: 0x05,
                    },
                    DipChoice {
                        label: "2 Coins/1 Credit",
                        value: 0x06,
                    },
                    DipChoice {
                        label: "1 Coin/1 Credit",
                        value: 0x07,
                    },
                ],
            },
            DipOption {
                name: "Bonus Life",
                mask: 0x38,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "None",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "30K, 100K, Every 100K",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "20K, 70K, Every 70K",
                        value: 0x10,
                    },
                    DipChoice {
                        label: "20K and 60K Only",
                        value: 0x18,
                    },
                    DipChoice {
                        label: "20K, 60K, Every 60K",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "30K, 120K, Every 120K",
                        value: 0x28,
                    },
                    DipChoice {
                        label: "20K, 80K, Every 80K",
                        value: 0x30,
                    },
                    DipChoice {
                        label: "30K and 80K Only",
                        value: 0x38,
                    },
                ],
            },
            DipOption {
                name: "Lives",
                mask: 0xC0,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "2",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "4",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "3",
                        value: 0x80,
                    },
                    DipChoice {
                        label: "5",
                        value: 0xC0,
                    },
                ],
            },
        ],
    },
];

crate::impl_dip_switches!(GalagaSystem, GALAGA_DIP_BANKS, board.dswa, board.dswb);

#[cfg(test)]
mod dip_tests {
    use super::*;
    use phosphor_core::core::machine::DipSwitches;

    // Galaga's DIP defaults are applied in load_roms() (which a ROM-free unit
    // test can't run — new() leaves the shared board byte at 0x99/0x24), so set
    // the historical galaga bytes explicitly before validating the tables.
    const DSWA_DEFAULT: u8 = 0xF7;
    const DSWB_DEFAULT: u8 = 0x97;

    fn galaga_at_defaults() -> GalagaSystem {
        let mut sys = GalagaSystem::new();
        sys.set_dip_bank_value(0, DSWA_DEFAULT);
        sys.set_dip_bank_value(1, DSWB_DEFAULT);
        sys
    }

    #[test]
    fn dip_defaults_and_metadata() {
        let sys = galaga_at_defaults();
        crate::assert_dip_banks_valid(sys.dip_banks(), &[DSWA_DEFAULT, DSWB_DEFAULT]);
    }

    #[test]
    fn set_dip_option_masks_only_its_bits() {
        let mut sys = galaga_at_defaults();
        // DSWB Lives is bank 1, option 2 (mask 0xC0); pick "5" (0xC0).
        sys.set_dip_option(1, 2, 0xC0);
        assert_eq!(sys.dip_bank_value(1), 0xD7); // 0x97 with bits 6-7 set
        assert_eq!(sys.dip_bank_value(0), 0xF7); // other bank untouched
    }

    #[test]
    fn declares_native_dims_and_rot90() {
        use phosphor_core::core::machine::{Orientation, Renderable};
        let sys = GalagaSystem::new();
        // Native landscape framebuffer; the frontend applies ROT90 to present
        // portrait. (Dig Dug / Xevious share the board but stay baked.)
        assert_eq!(sys.display_size(), (288, 224));
        assert_eq!(sys.orientation(), Orientation::ROT90);
        assert!(sys.orientation().swaps_axes());
        assert_eq!(sys.display_aspect(), Some((3, 4)));
    }

    // -----------------------------------------------------------------------
    // Per-scanline rendering (W4)
    // -----------------------------------------------------------------------

    /// A system whose chars 1 and 2 are solid, resolving to distinguishable
    /// palette indices, so a video RAM cell marks the rows it reached.
    fn galaga_with_solid_chars() -> GalagaSystem {
        let mut sys = GalagaSystem::new();
        sys.char_cache = GfxCache::new(4, 8, 8);
        for py in 0..8 {
            for px in 0..8 {
                sys.char_cache.set_pixel(1, px, py, 1);
                sys.char_cache.set_pixel(2, px, py, 2);
            }
        }
        // Colour 0: pen 1 -> LUT 1, pen 2 -> LUT 2, everything else transparent.
        sys.char_lut = [0x0F; 256];
        sys.char_lut[1] = 0x01;
        sys.char_lut[2] = 0x02;
        let vram = sys.board.map.region_data_mut(Region::VideoRam);
        vram[..0x400].fill(1); // every cell is char 1
        vram[0x400..0x800].fill(0); // colour 0
        sys
    }

    fn scan_rows(sys: &mut GalagaSystem, rows: std::ops::Range<usize>) {
        for y in rows {
            sys.render_scanline(y);
        }
    }

    /// The behavior W4 exists for on the tilemap layer: video RAM is read as the
    /// beam passes it, so rewriting a cell partway down the screen changes only
    /// the rows below the write.
    ///
    /// The split is at row 100, which is *inside* char row 12 (rows 96..103). A
    /// whole-frame render draws a char row from one snapshot and cannot produce
    /// this picture at all.
    #[test]
    fn a_mid_frame_vram_write_changes_only_the_rows_below_it() {
        const SPLIT: usize = 100;
        let mut sys = galaga_with_solid_chars();

        scan_rows(&mut sys, 0..SPLIT);
        sys.board.map.region_data_mut(Region::VideoRam)[..0x400].fill(2);
        scan_rows(&mut sys, SPLIT..224);

        let px = |y: usize| sys.native_buffer[y * 288 + 100];
        assert_eq!(px(0), 0x11, "row 0 was drawn before the write");
        assert_eq!(
            px(SPLIT - 1),
            0x11,
            "the last row above the write keeps the old char, mid-char-row"
        );
        assert_eq!(
            px(SPLIT),
            0x12,
            "the first row below the write takes the new char"
        );
        assert_eq!(px(223), 0x12, "the bottom row is below the write");
    }

    /// The acceptance criterion this work item was written with: state sampled
    /// once a frame must NOT become per-line. The starfield is that state here,
    /// because the shift register's position is a function of how many times it
    /// has been clocked since the frame began, so re-reading the controls per
    /// row would move every star below the change rather than recolour a row.
    ///
    /// Clearing the enable bit mid-frame must therefore leave the rows below it
    /// alone until the next frame latches it.
    #[test]
    fn the_starfield_controls_are_not_resampled_per_line() {
        let mut sys = GalagaSystem::new();
        sys.starfield_enabled = true;
        sys.star_set_a = 0;
        sys.star_set_b = 1;

        scan_rows(&mut sys, 0..100);
        // If this reached the picture, every row below would lose its stars.
        sys.starfield_enabled = false;
        scan_rows(&mut sys, 100..224);

        assert!(
            sys.star_frame.enabled,
            "the frame's latch keeps the value it had at row 0"
        );
        let stars_below = sys.native_buffer[150 * 288..224 * 288]
            .iter()
            .filter(|&&p| (32..96).contains(&p))
            .count();
        assert!(
            stars_below > 0,
            "rows below a mid-frame disable still carry the frame's starfield"
        );
    }

    /// `render_scanline` drawing a row is only half of it: the frame loop has to
    /// call it. Without this the tests above would pass on a board that never
    /// drew anything, since they drive `render_scanline` by hand.
    #[test]
    fn the_frame_loop_draws_rows_at_scanline_boundaries() {
        let mut sys = galaga_with_solid_chars();
        sys.native_buffer.fill(0xFF);

        // One cycle at clock 0 crosses the scanline-0 boundary. This is the
        // debugger's per-cycle path.
        sys.tick_frame_boundary();
        assert_eq!(sys.native_buffer[100], 0x11, "tick draws row 0");
        assert_eq!(
            sys.native_buffer[288 + 100],
            0xFF,
            "and only row 0: nothing has drawn row 1 yet"
        );

        // And the frame loop, which hoists the boundary test out.
        sys.native_buffer.fill(0xFF);
        sys.run_frame();
        let undrawn = (0..224)
            .filter(|&y| sys.native_buffer[y * 288 + 100] == 0xFF)
            .count();
        assert_eq!(undrawn, 0, "run_frame draws every visible row");
    }

    #[test]
    fn render_frame_emits_native_unrotated_rgb() {
        use phosphor_core::core::machine::Renderable;
        let mut sys = GalagaSystem::new();
        // Tag a native pixel + palette entry; confirm it lands at the native
        // row-major position (288 wide), i.e. no baked rotation.
        sys.combined_palette[7] = (11, 22, 33);
        let (nx, ny) = (9usize, 3usize);
        sys.native_buffer[ny * 288 + nx] = 7;
        let mut buf = vec![0u8; 288 * 224 * 3];
        sys.render_frame(&mut buf);
        let i = (ny * 288 + nx) * 3;
        assert_eq!(&buf[i..i + 3], &[11, 22, 33]);
    }
}
crate::impl_board_debug_trace!(GalagaSystem, board);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

const ALL_CONFIGS: &[&GalagaRomConfig] = &[&GALAGA_CONFIG, &GALAGAO_CONFIG, &GALAGAMW_CONFIG];

crate::register_machine!(
    GalagaSystem,
    "galaga",
    &["galaga", "galagao", "galagamw"],
    namco_galaga::NAMCO_GALAGA_CONTROLS,
    configs = ALL_CONFIGS
);
