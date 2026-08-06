use phosphor_core::bus_split;
use phosphor_core::core::address_space::AccessKind;
use phosphor_core::core::address_space16::WriteAnnotation;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug::{BusDebug, DebugCpu, Debuggable};
use phosphor_core::core::debug_trace::DebugEventKind;
use phosphor_core::core::machine::{
    AudioSource, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, MachineCore, MachineDebug,
    Nvram, Profilable, Renderable, SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::device::Er2055;
use phosphor_core::gfx;
use phosphor_core::gfx::GfxCache;
use phosphor_core::gfx::decode::{GfxLayout, decode_gfx};
use phosphor_macros::{MemoryRegion, Saveable};

use crate::namco_galaga::{self, GALAGA_SPRITE_LAYOUT, NamcoGalagaBoard};
use crate::namco_pac::PACMAN_TILE_LAYOUT;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};

// ---------------------------------------------------------------------------
// GfxLayout descriptors for Dig Dug
// ---------------------------------------------------------------------------

const DIGDUG_CHAR_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0],
    x_offsets: &[7, 6, 5, 4, 3, 2, 1, 0],
    y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
    char_increment: 64,
};

// ---------------------------------------------------------------------------
// ROM definitions
// ---------------------------------------------------------------------------

static DIGDUG_MAIN_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "dd1a.1",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xa80ec984],
        },
        RomEntry {
            name: "dd1a.2",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x559f00bd],
        },
        RomEntry {
            name: "dd1a.3",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0x8cbc6fe1],
        },
        RomEntry {
            name: "dd1a.4",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0xd066f830],
        },
    ],
};

static DIGDUG_SUB_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "dd1a.5",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x6687933b],
        },
        RomEntry {
            name: "dd1a.6",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x843d857f],
        },
    ],
};

static DIGDUG_SOUND_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "dd1.7",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0xa41bce72],
    }],
};

/// Characters: 1bpp 8x8 (0x800 bytes → 256 tiles).
static DIGDUG_GFX1_ROM: RomRegion = RomRegion {
    size: 0x0800,
    entries: &[RomEntry {
        name: "dd1.9",
        size: 0x0800,
        offset: 0x0000,
        crc32: &[0xf14a6fe1],
    }],
};

/// Sprites: 2bpp 16x16 (4 × 4KB = 16KB → 256 sprites).
static DIGDUG_GFX2_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "dd1.15",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xe22957c8],
        },
        RomEntry {
            name: "dd1.14",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x2829ec99],
        },
        RomEntry {
            name: "dd1.13",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0x458499e9],
        },
        RomEntry {
            name: "dd1.12",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0xc58252a0],
        },
    ],
};

/// Background tiles: 2bpp 8x8 (4KB → 256 tiles).
static DIGDUG_GFX3_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "dd1.11",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0x7b383983],
    }],
};

/// Playfield ROM: 4KB tile map data (4 pages × 1KB).
static DIGDUG_GFX4_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "dd1.10b",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0x2cf399c2],
    }],
};

/// PROMs: palette (32) + sprite LUT (256) + BG LUT (256).
static DIGDUG_PROMS: RomRegion = RomRegion {
    size: 0x0220,
    entries: &[
        RomEntry {
            name: "136007.113",
            size: 0x0020,
            offset: 0x0000,
            crc32: &[0x4cb9da99],
        },
        RomEntry {
            name: "136007.111",
            size: 0x0100,
            offset: 0x0020,
            crc32: &[0x00c7c419],
        },
        RomEntry {
            name: "136007.112",
            size: 0x0100,
            offset: 0x0120,
            crc32: &[0xe9b3e08e],
        },
    ],
};

/// Sound waveform PROM.
static DIGDUG_SOUND_PROM: RomRegion = RomRegion {
    size: 0x0100,
    entries: &[RomEntry {
        name: "136007.110",
        size: 0x0100,
        offset: 0x0000,
        crc32: &[0x7a2815b4],
    }],
};

/// Namco 51XX MCU firmware ROM (MB8843, 1KB).
static DIGDUG_51XX_ROM: RomRegion = RomRegion {
    size: 0x0400,
    entries: &[RomEntry {
        name: "51xx.bin",
        size: 0x0400,
        offset: 0x0000,
        crc32: &[0xc2f57ef8],
    }],
};

// ---------------------------------------------------------------------------
// ROM definitions — Dig Dug (Namco rev 1, "digdug1")
// ---------------------------------------------------------------------------

static DIGDUG1_MAIN_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "dd1.1",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xb9198079],
        },
        RomEntry {
            name: "dd1.2",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0xb2acbe49],
        },
        RomEntry {
            name: "dd1.3",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0xd6407b49],
        },
        RomEntry {
            name: "dd1.4b",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0xf4cebc16],
        },
    ],
};

static DIGDUG1_SUB_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "dd1.5b",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x370ef9b4],
        },
        RomEntry {
            name: "dd1.6b",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x361eeb71],
        },
    ],
};

// ---------------------------------------------------------------------------
// ROM definitions — Dig Dug (Atari rev 2, "digdugat")
// ---------------------------------------------------------------------------

static DIGDUGAT_MAIN_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "136007.201",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x23d0b1a4],
        },
        RomEntry {
            name: "136007.202",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x5453dc1f],
        },
        RomEntry {
            name: "136007.203",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0xc9077dfa],
        },
        RomEntry {
            name: "136007.204",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0xa8fc8eac],
        },
    ],
};

static DIGDUGAT_SUB_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "136007.205",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x5ba385c5],
        },
        RomEntry {
            name: "136007.206",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x382b4011],
        },
    ],
};

static DIGDUGAT_SOUND_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "136007.107",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0xa41bce72],
    }],
};

static DIGDUGAT_GFX1_ROM: RomRegion = RomRegion {
    size: 0x0800,
    entries: &[RomEntry {
        name: "136007.108",
        size: 0x0800,
        offset: 0x0000,
        crc32: &[0x3d24a3af],
    }],
};

static DIGDUGAT_GFX2_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "136007.116",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xe22957c8],
        },
        RomEntry {
            name: "136007.117",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0xa3bbfd85],
        },
        RomEntry {
            name: "136007.118",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0x458499e9],
        },
        RomEntry {
            name: "136007.119",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0xc58252a0],
        },
    ],
};

static DIGDUGAT_GFX3_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "136007.115",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0x754539be],
    }],
};

static DIGDUGAT_GFX4_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "136007.114",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0xd6822397],
    }],
};

// ---------------------------------------------------------------------------
// ROM definitions — Dig Dug (Atari rev 1, "digdugat1")
// ---------------------------------------------------------------------------

static DIGDUGAT1_MAIN_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "136007.101",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xb9198079],
        },
        RomEntry {
            name: "136007.102",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0xb2acbe49],
        },
        RomEntry {
            name: "136007.103",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0xd6407b49],
        },
        RomEntry {
            name: "136007.104",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0xb3ad42c3],
        },
    ],
};

static DIGDUGAT1_SUB_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "136007.105",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x0a2aef4a],
        },
        RomEntry {
            name: "136007.106",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0xa2876d6e],
        },
    ],
};

// ---------------------------------------------------------------------------
// ROM configuration (variant-specific ROM region references)
// ---------------------------------------------------------------------------

struct DigDugRomConfig {
    main_rom: &'static RomRegion,
    sub_rom: &'static RomRegion,
    sound_rom: &'static RomRegion,
    gfx1_rom: &'static RomRegion,
    gfx2_rom: &'static RomRegion,
    gfx3_rom: &'static RomRegion,
    gfx4_rom: &'static RomRegion,
    proms: &'static RomRegion,
    sound_prom: &'static RomRegion,
}

static DIGDUG_CONFIG: DigDugRomConfig = DigDugRomConfig {
    main_rom: &DIGDUG_MAIN_ROM,
    sub_rom: &DIGDUG_SUB_ROM,
    sound_rom: &DIGDUG_SOUND_ROM,
    gfx1_rom: &DIGDUG_GFX1_ROM,
    gfx2_rom: &DIGDUG_GFX2_ROM,
    gfx3_rom: &DIGDUG_GFX3_ROM,
    gfx4_rom: &DIGDUG_GFX4_ROM,
    proms: &DIGDUG_PROMS,
    sound_prom: &DIGDUG_SOUND_PROM,
};

static DIGDUG1_CONFIG: DigDugRomConfig = DigDugRomConfig {
    main_rom: &DIGDUG1_MAIN_ROM,
    sub_rom: &DIGDUG1_SUB_ROM,
    sound_rom: &DIGDUG_SOUND_ROM, // shared
    gfx1_rom: &DIGDUG_GFX1_ROM,   // shared
    gfx2_rom: &DIGDUG_GFX2_ROM,   // shared
    gfx3_rom: &DIGDUG_GFX3_ROM,   // shared
    gfx4_rom: &DIGDUG_GFX4_ROM,   // shared
    proms: &DIGDUG_PROMS,         // shared
    sound_prom: &DIGDUG_SOUND_PROM,
};

static DIGDUGAT_CONFIG: DigDugRomConfig = DigDugRomConfig {
    main_rom: &DIGDUGAT_MAIN_ROM,
    sub_rom: &DIGDUGAT_SUB_ROM,
    sound_rom: &DIGDUGAT_SOUND_ROM,
    gfx1_rom: &DIGDUGAT_GFX1_ROM,
    gfx2_rom: &DIGDUGAT_GFX2_ROM,
    gfx3_rom: &DIGDUGAT_GFX3_ROM,
    gfx4_rom: &DIGDUGAT_GFX4_ROM,
    proms: &DIGDUG_PROMS, // shared
    sound_prom: &DIGDUG_SOUND_PROM,
};

static DIGDUGAT1_CONFIG: DigDugRomConfig = DigDugRomConfig {
    main_rom: &DIGDUGAT1_MAIN_ROM,
    sub_rom: &DIGDUGAT1_SUB_ROM,
    sound_rom: &DIGDUGAT_SOUND_ROM, // shared with digdugat
    gfx1_rom: &DIGDUGAT_GFX1_ROM,   // shared with digdugat
    gfx2_rom: &DIGDUGAT_GFX2_ROM,   // shared with digdugat
    gfx3_rom: &DIGDUGAT_GFX3_ROM,   // shared with digdugat
    gfx4_rom: &DIGDUGAT_GFX4_ROM,   // shared with digdugat
    proms: &DIGDUG_PROMS,           // shared
    sound_prom: &DIGDUG_SOUND_PROM,
};

// ---------------------------------------------------------------------------
// DigDugSystem
// ---------------------------------------------------------------------------

/// Dig Dug Arcade System (Namco, 1982)
///
/// Hardware: 3×Z80 @ 3.072 MHz, Namco WSG 3-voice, Namco 06XX/51XX/53XX
/// custom I/O. Video: 36×28 tilemap foreground, ROM-based background,
/// 64 sprites (16×16 or 32×32). Screen: 288×224 rotated 90° CCW.
/// Dig Dug's RAM windows, declared on the shared board's address map.
///
/// Ids start at 4: 0 is the core's unmapped sentinel and 1-3 are the board's
/// per-CPU ROMs ([`namco_galaga::Region`]). The names given here are what the
/// debugger shows against every write and watchpoint hit in these windows.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum Region {
    VideoRam = 4,
    WorkRam = 5,
    SpriteAttrs = 6,
    SpritePos = 7,
    SpriteFlip = 8,
}

#[derive(Saveable)]
pub struct DigDugSystem {
    pub board: NamcoGalagaBoard,

    #[save_skip]
    earom: Er2055, // 0xB800-0xB83F (ER2055 64×4-bit EAROM for high scores)

    // Video latch state (written via 0xA000-0xA007)
    bg_select: u8,       // bits 0-1: background page (0-3)
    tx_color_mode: bool, // bit 2
    bg_disable: bool,    // bit 3
    bg_color_bank: u8,   // bits 4-5 (stored as 0x00 or 0x10/0x20/0x30)

    // GFX caches
    #[save_skip]
    char_cache: GfxCache, // 1bpp 8×8 (256 tiles)
    #[save_skip]
    sprite_cache: GfxCache, // 2bpp 16×16 (256 sprites)
    #[save_skip]
    bg_tile_cache: GfxCache, // 2bpp 8×8 (256 tiles)
    #[save_skip]
    playfield_rom: Vec<u8>, // Background tile codes (4 pages × 1KB)

    // Color lookup tables (from PROMs)
    #[save_skip]
    sprite_lut: [u8; 256],
    #[save_skip]
    bg_lut: [u8; 256],

    // Frame buffer (288 × 224 native, indexed — rotated to RGB in render_frame)
    #[save_skip]
    native_buffer: Vec<u8>,
}

impl DigDugSystem {
    pub fn new() -> Self {
        let mut board = NamcoGalagaBoard::new();
        // All three CPUs share these windows. The split into five chips is the
        // video hardware's: the tilemap has its own RAM, and the sprite hardware
        // reads attributes, positions and flip/size bits from three separate
        // RAMs at a fixed 0x380 offset.
        board
            .map
            .region(
                Region::VideoRam,
                "Video RAM (Foreground Tilemap)",
                0x8000,
                0x400,
                AccessKind::ReadWrite,
            )
            .region(
                Region::WorkRam,
                "Work RAM",
                0x8400,
                0x400,
                AccessKind::ReadWrite,
            )
            .region(
                Region::SpriteAttrs,
                "Work RAM / Sprite Attributes",
                0x8800,
                0x400,
                AccessKind::ReadWrite,
            )
            .region(
                Region::SpritePos,
                "Work RAM / Sprite Positions",
                0x9000,
                0x400,
                AccessKind::ReadWrite,
            )
            .region(
                Region::SpriteFlip,
                "Work RAM / Sprite Flip+Size",
                0x9800,
                0x400,
                AccessKind::ReadWrite,
            );

        Self {
            board,

            earom: Er2055::new(),

            bg_select: 0,
            tx_color_mode: false,
            bg_disable: false,
            bg_color_bank: 0,

            char_cache: GfxCache::new(0, 8, 8),
            sprite_cache: GfxCache::new(0, 16, 16),
            bg_tile_cache: GfxCache::new(0, 8, 8),
            playfield_rom: Vec::new(),

            sprite_lut: [0; 256],
            bg_lut: [0; 256],

            native_buffer: vec![0u8; 288 * 224],
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        self.load_roms(rom_set, &DIGDUG_CONFIG)
    }

    fn load_roms(
        &mut self,
        rom_set: &RomSet,
        config: &DigDugRomConfig,
    ) -> Result<(), RomLoadError> {
        // Program ROMs
        self.board.load_main_rom(&config.main_rom.load(rom_set)?);
        self.board.load_sub_rom(&config.sub_rom.load(rom_set)?);
        self.board.load_sound_rom(&config.sound_rom.load(rom_set)?);

        // GFX ROMs
        let gfx1 = config.gfx1_rom.load(rom_set)?;
        self.char_cache = decode_gfx(&gfx1, 0, gfx1.len() / 8, &DIGDUG_CHAR_LAYOUT);

        let gfx2 = config.gfx2_rom.load(rom_set)?;
        self.sprite_cache = decode_gfx(&gfx2, 0, gfx2.len() / 64, &GALAGA_SPRITE_LAYOUT);

        let gfx3 = config.gfx3_rom.load(rom_set)?;
        self.bg_tile_cache = decode_gfx(&gfx3, 0, gfx3.len() / 16, &PACMAN_TILE_LAYOUT);

        self.playfield_rom = config.gfx4_rom.load(rom_set)?;

        // PROMs
        let proms = config.proms.load(rom_set)?;
        self.board.load_palette_prom(&proms[0..0x20]);
        self.sprite_lut.copy_from_slice(&proms[0x20..0x120]);
        self.bg_lut.copy_from_slice(&proms[0x120..0x220]);

        // Sound PROM
        self.board
            .load_sound_prom(&config.sound_prom.load(rom_set)?);

        // 51XX MCU firmware ROM (optional — falls back to HLE if unavailable)
        if let Ok(rom_51xx) = DIGDUG_51XX_ROM.load(rom_set) {
            self.board.load_51xx_rom(&rom_51xx);
        }

        // Default DIP switches matching MAME defaults for Dig Dug:
        // DSWA: Coin_B=1C/1C(0x01), Bonus=20K/60K(0x18), Lives=3(0x80)
        // DSWB: Coin_A=1C/1C(0x00), Freeze=Off(0x20), Demo=On(0x00),
        //       Continue=Yes(0x00), Upright(0x04), Difficulty=Easy(0x00)
        self.board.dswa = 0x99;
        self.board.dswb = 0x24;

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Video latch (0xA000-0xA007, LS259 pattern)
    // -----------------------------------------------------------------------

    fn write_video_latch(&mut self, bit: u8, value: bool) {
        match bit {
            0 => {
                // bg_select bit 0
                if value {
                    self.bg_select |= 1;
                } else {
                    self.bg_select &= !1;
                }
            }
            1 => {
                // bg_select bit 1
                if value {
                    self.bg_select |= 2;
                } else {
                    self.bg_select &= !2;
                }
            }
            2 => self.tx_color_mode = value,
            3 => self.bg_disable = value,
            4 => {
                // bg_color_bank bit 4
                if value {
                    self.bg_color_bank |= 0x10;
                } else {
                    self.bg_color_bank &= !0x10;
                }
            }
            5 => {
                // bg_color_bank bit 5
                if value {
                    self.bg_color_bank |= 0x20;
                } else {
                    self.bg_color_bank &= !0x20;
                }
            }
            7 => self.board.flip_screen = value,
            _ => {} // 6: unused
        }
    }

    // -----------------------------------------------------------------------
    // Tilemap addressing (same as Pac-Man)
    // -----------------------------------------------------------------------

    // -----------------------------------------------------------------------
    // Full-frame video rendering (no raster effects in Dig Dug)
    // -----------------------------------------------------------------------

    /// Advance one CPU cycle, refreshing the cached framebuffer whenever that
    /// cycle completes a frame.
    ///
    /// The render lives here rather than after `run_frame`'s loop so that the
    /// debugger's `debug_tick()` path refreshes the picture too (it never calls
    /// `run_frame`, which is why render-once machines showed a frozen image).
    ///
    /// The hook fires on the *last* cycle of the frame — the same video state
    /// the old end-of-loop render sampled — so output is byte-identical. It
    /// deliberately does **not** fire at the start of vblank: this board writes
    /// video state during vblank, so sampling earlier would change the picture.
    fn tick_frame_boundary(&mut self) {
        bus_split!(self, bus => {
            self.board.tick(bus);
        });
        if self
            .board
            .clock
            .is_multiple_of(namco_galaga::TIMING.cycles_per_frame())
        {
            self.render_video();
        }
    }

    fn render_video(&mut self) {
        // Layer 1: Background tilemap
        self.render_background();

        // Layer 2: Foreground text (1bpp, transparent pen 0)
        self.render_foreground();

        // Layer 3: Sprites
        self.render_sprites();
    }

    fn render_background(&mut self) {
        // Opaque 36×28 background tilemap of 8×8 tiles, codes read from the
        // playfield ROM, pens looked up through bg_lut. Rendered per-scanline via
        // the shared index-writing helper (native indexed buffer, no priority).
        let config = gfx::TilemapConfig {
            cols: 36,
            rows: 28,
            tile_width: 8,
            tile_height: 8,
        };
        // Split borrows: closures read the ROM/LUT, the helper writes native_buffer.
        let playfield_rom = &self.playfield_rom;
        let bg_lut = &self.bg_lut;
        let bg_tile_cache = &self.bg_tile_cache;
        let bg_select = self.bg_select;
        let bg_disable = self.bg_disable;
        let bg_color_bank = self.bg_color_bank;
        let native = &mut self.native_buffer;
        let mut prio = [0u8; 288];
        for scanline in 0..224 {
            let row_off = scanline * 288;
            let row = &mut native[row_off..row_off + 288];
            gfx::render_tilemap_scanline_indexed(
                &config,
                bg_tile_cache,
                scanline,
                |col, trow| {
                    let offset = crate::namco_video::namco_tilemap_offset(col as i32, trow as i32);
                    let code = if offset < playfield_rom.len() {
                        playfield_rom[offset | ((bg_select as usize) << 10)] as usize
                    } else {
                        0
                    };
                    let color = if bg_disable {
                        0x0F
                    } else {
                        ((code >> 4) | bg_color_bank as usize) & 0x3F
                    };
                    gfx::TileInfo::new(code as u16, color as u8)
                },
                // Opaque: every pixel writes bg_lut's low nibble.
                |color, pixel| {
                    Some((
                        bg_lut[(color as usize * 4 + pixel as usize) & 0xFF] & 0x0F,
                        0,
                    ))
                },
                row,
                &mut prio,
                0,
            );
        }
    }

    fn render_foreground(&mut self) {
        // 1bpp text tilemap: pen 0 transparent, pen 1 -> the tile's color group.
        // The Namco offset never reaches 0x400 within the visible grid, so no
        // per-tile skip is needed. Rendered per-scanline via the shared helper.
        let config = gfx::TilemapConfig {
            cols: 36,
            rows: 28,
            tile_width: 8,
            tile_height: 8,
        };
        let video_ram = self.board.map.region_data(Region::VideoRam);
        let char_cache = &self.char_cache;
        let tx_color_mode = self.tx_color_mode;
        let native = &mut self.native_buffer;
        let mut prio = [0u8; 288];
        for scanline in 0..224 {
            let row_off = scanline * 288;
            let row = &mut native[row_off..row_off + 288];
            gfx::render_tilemap_scanline_indexed(
                &config,
                char_cache,
                scanline,
                |col, trow| {
                    let offset = crate::namco_video::namco_tilemap_offset(col as i32, trow as i32);
                    let code = video_ram[offset] as usize;
                    let color = if tx_color_mode {
                        code & 0x0F
                    } else {
                        ((code >> 4) & 0x0E) | ((code >> 3) & 2)
                    };
                    // Cache lookup uses code & 0x7F; the color group rides in attr.
                    gfx::TileInfo::new((code & 0x7F) as u16, color as u8)
                },
                // Pen 0 transparent; pen 1 -> color group as the palette index.
                |color, pixel| (pixel != 0).then_some((color, 0)),
                row,
                &mut prio,
                0,
            );
        }
    }

    fn render_sprites(&mut self) {
        // Sprites are at obj_ram[0x380..], pos_ram[0x380..], flp_ram[0x380..]
        // Step 2, 64 entries (but last few are often unused)
        // Draw in reverse order (lower index = higher priority on top)
        for i in 0..64 {
            let offs = i * 2;
            let attr_addr = 0x380 + offs;
            if attr_addr + 1 >= 0x400 {
                continue;
            }

            // The sprite hardware reads one attribute pair from each of the
            // three RAMs at the same offset; copy the six bytes out before
            // draw_sprite_tile takes &mut self, so the map stays immutably
            // borrowed only for this read.
            let (obj_ram, pos_ram, flp_ram) = (
                self.board.map.region_data(Region::SpriteAttrs),
                self.board.map.region_data(Region::SpritePos),
                self.board.map.region_data(Region::SpriteFlip),
            );
            let sprite_byte = obj_ram[attr_addr];
            let color = (obj_ram[attr_addr + 1] & 0x3F) as usize;
            let sx = pos_ram[attr_addr + 1] as i32 - 40 + 1;
            let raw_sy = 256i32 - pos_ram[attr_addr] as i32 + 1;
            let flipx = flp_ram[attr_addr] & 0x01 != 0;
            let flipy = flp_ram[attr_addr] & 0x02 != 0;
            let size = (sprite_byte & 0x80) != 0; // true = 32×32

            // Sprite code transformation: only for 32×32 sprites.
            // For 32×32, shift bits 5-0 left by 2 to get base of 4 consecutive tiles.
            // For 16×16, use the raw byte as the tile code directly.
            let sprite = if size {
                ((sprite_byte as usize & 0xC0) | ((sprite_byte as usize & 0x3F) << 2)) & 0xFF
            } else {
                sprite_byte as usize
            };

            let sy = if size {
                ((raw_sy - 16) & 0xFF) - 32
            } else {
                (raw_sy & 0xFF) - 32
            };

            // Tile offset table for 2×2 grid: [row][col]
            const GFX_OFFS: [[usize; 2]; 2] = [[0, 1], [2, 3]];

            let grid = if size { 2 } else { 1 };

            for gy in 0..grid {
                for gx in 0..grid {
                    let tile_code = if size {
                        // XOR with flip flags to reverse tile order
                        let row = gy ^ if flipy { 1 } else { 0 };
                        let col = gx ^ if flipx { 1 } else { 0 };
                        (sprite + GFX_OFFS[row][col]) & 0xFF
                    } else {
                        sprite
                    };

                    // X wraps in 8-bit coordinate space
                    let tile_sx = (sx + (gx as i32) * 16) & 0xFF;
                    let tile_sy = sy + (gy as i32) * 16;

                    self.draw_sprite_tile(tile_code, color, tile_sx, tile_sy, flipx, flipy);
                    // Wraparound: draw again at x+256 for sprites crossing screen edge
                    self.draw_sprite_tile(tile_code, color, tile_sx + 0x100, tile_sy, flipx, flipy);
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

        // Clip to the visible area (columns 2..34, i.e. pixels 16..272); no tunnel
        // wrap (render_sprites handles the +0x100 second copy itself). Sprite pens
        // land in palette entries 0x10-0x1F; a LUT low-nibble of 0x0F is transparent.
        let clip = gfx::SpriteClip {
            x_min: 16,
            x_max: 272,
            wrap_offset: None,
        };
        // Split borrows so the LUT closures don't hold &self while native_buffer
        // is borrowed mutably. No priority buffer (composite by draw order).
        let sprite_lut = &self.sprite_lut;
        let sprite_cache = &self.sprite_cache;
        let native = &mut self.native_buffer;
        let is_transparent =
            |pixel: u8| sprite_lut[(color * 4 + pixel as usize) & 0xFF] & 0x0F == 0x0F;
        let resolve = |pixel: u8| {
            (
                (sprite_lut[(color * 4 + pixel as usize) & 0xFF] & 0x0F) | 0x10,
                0u8,
            )
        };
        let mut prio = [0u8; 288];

        for py in 0..16usize {
            let screen_y = sy + py as i32;
            if !(0..224).contains(&screen_y) {
                continue;
            }
            let src_py = if flipy { 15 - py } else { py };
            let row_off = screen_y as usize * 288;
            let row = &mut native[row_off..row_off + 288];
            gfx::draw_sprite_row_indexed(
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
}

impl Default for DigDugSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus implementation
// ---------------------------------------------------------------------------

/// Label a bus write for the event trace. Beyond the platform-common latches
/// and custom-I/O window, Dig Dug decodes its own video latch at 0xA000-0xA007
/// and the ER-2055 high-score EAROM at 0xB800-0xB83F — both game-specific, so
/// they are named here rather than on the shared board.
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
        0xB800..=0xB840 => WriteAnnotation::device("EAROM"),
        _ => WriteAnnotation::MEMORY,
    }
}

impl Bus for DigDugSystem {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let data = match addr {
            0x0000..=0x3FFF => self.board.read_rom(master, addr),
            0x6800..=0x6807 => {
                // These addresses respond as DIP switch reads on hardware,
                // but Dig Dug reads DIP switches through the 53XX.
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
            0xB800..=0xB83F => self.earom.read(addr - 0xB800),
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
            0x0000..=0x3FFF => {} // ROM
            0x6800..=0x681F => self.board.wsg.write(addr - 0x6800, data),
            0x6820..=0x6827 => {
                let bit = (addr & 7) as u8;
                let value = (data & 1) != 0;
                self.board.write_misc_latch(bit, value);
            }
            0x6830 => {
                // Watchdog reset (data ignored — any write feeds the watchdog).
                self.board.watchdog_counter = 0;
            }
            0x7000..=0x70FF => self.board.write_custom_io(data),
            0x7100 => self.board.write_custom_io_ctrl(data),
            0x8000..=0x87FF | 0x8800..=0x8BFF | 0x9000..=0x93FF | 0x9800..=0x9BFF => {
                self.board.map.write_backing(addr, data)
            }
            0xA000..=0xA007 => {
                let bit = (addr & 7) as u8;
                let value = (data & 1) != 0;
                self.write_video_latch(bit, value);
            }
            // ER2055 EAROM: latch address and data for subsequent control commit
            0xB800..=0xB83F => self.earom.latch(addr - 0xB800, data),
            // ER2055 control: commit write on rising clock edge
            0xB840 => {
                let clock = data & 0x01 != 0;
                let c1 = data & 0x02 == 0; // bit 1 inverted → C1 (active-low)
                let c2 = data & 0x04 != 0; // bit 2
                let cs1 = data & 0x08 != 0;
                self.earom.write_control(clock, cs1, c1, c2);
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

impl Renderable for DigDugSystem {
    fn display_size(&self) -> (u32, u32) {
        // Native (unrotated) landscape framebuffer. `namco_galaga::TIMING` reports
        // the pre-swapped portrait size (224×288); Dig Dug declares ROT90 and lets
        // the frontend rotate centrally, so it reports the native landscape size.
        (288, 224)
    }

    fn display_aspect(&self) -> Option<(u32, u32)> {
        namco_galaga::TIMING.display_aspect()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        // Native RGB24 in row-major order; the ROT90 the cabinet needs is applied
        // centrally by the frontend (see `orientation`), not baked here.
        let palette = &self.board.palette_rgb;
        let mask = palette.len() - 1;
        for (i, &idx) in self.native_buffer.iter().enumerate() {
            let (r, g, b) = palette[idx as usize & mask];
            buffer[i * 3] = r;
            buffer[i * 3 + 1] = g;
            buffer[i * 3 + 2] = b;
        }
    }

    fn orientation(&self) -> phosphor_core::core::machine::Orientation {
        // Dig Dug's monitor is mounted rotated 90°; declared, not baked.
        phosphor_core::core::machine::Orientation::ROT90
    }
}

impl AudioSource for DigDugSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.board.fill_audio(buffer)
    }

    fn audio_sample_rate(&self) -> u32 {
        44100
    }
}

impl BusDebug for DigDugSystem {
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
            0xB800..=0xB83F => Some(self.earom.read(addr - 0xB800)),
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
            0xB800..=0xB83F => self.earom.latch(addr - 0xB800, data),
            _ => {}
        }
    }

    /// Tagged debugger poke: routes RAM writes through the map's `poke` so a
    /// script/console poke records a `DebugAccessSource::Frontend` trace event.
    fn poke(&mut self, _cpu_index: usize, addr: u32, data: u8) {
        let Ok(addr) = u16::try_from(addr) else {
            return;
        };
        match addr {
            0x8000..=0x87FF | 0x8800..=0x8BFF | 0x9000..=0x93FF | 0x9800..=0x9BFF => {
                self.board.map.poke(addr, data)
            }
            0xB800..=0xB83F => self.earom.latch(addr - 0xB800, data),
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

impl MachineDebug for DigDugSystem {
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
        self.board.debug_tick_boundaries()
    }
}

impl MachineCore for DigDugSystem {
    crate::machine_core_metadata!("digdug", namco_galaga::TIMING);

    fn gfx_sheets(&self) -> Vec<phosphor_core::core::machine::GfxSheet<'_>> {
        use phosphor_core::core::machine::GfxSheet;
        vec![
            GfxSheet {
                name: "chars",
                cache: &self.char_cache,
                palette: &self.board.palette_rgb,
            },
            GfxSheet {
                name: "bg",
                cache: &self.bg_tile_cache,
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
        // The render happens in `tick_frame_boundary` on the frame's last cycle
        // (single render site, shared with `debug_tick`).
        for _ in 0..namco_galaga::TIMING.cycles_per_frame() {
            self.tick_frame_boundary();
        }
    }

    fn reset(&mut self) {
        self.board.reset_board();
        for region in [
            Region::VideoRam,
            Region::WorkRam,
            Region::SpriteAttrs,
            Region::SpritePos,
            Region::SpriteFlip,
        ] {
            self.board.map.region_data_mut(region).fill(0);
        }
        self.bg_select = 0;
        self.tx_color_mode = false;
        self.bg_disable = false;
        self.bg_color_bank = 0;
        self.earom.reset();
        self.native_buffer.fill(0);

        bus_split!(self, bus => {
            self.board.main_cpu.reset(bus, BusMaster::Cpu(0));
            self.board.sub_cpu.reset(bus, BusMaster::Cpu(1));
            self.board.sound_cpu.reset(bus, BusMaster::Cpu(2));
        });
    }
}

impl SaveState for DigDugSystem {
    crate::machine_save_state!();
}

impl Nvram for DigDugSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.earom.snapshot())
    }

    fn load_nvram(&mut self, data: &[u8]) {
        self.earom.load_from(data);
    }
}

impl phosphor_core::core::machine::InputConfigurable for DigDugSystem {
    fn input_controls(&self) -> &'static [phosphor_core::core::machine::InputControl] {
        namco_galaga::NAMCO_GALAGA_CONTROLS
    }

    fn handle_input(&mut self, event: phosphor_core::core::machine::InputEvent) {
        if let phosphor_core::core::machine::InputEvent::Button { id, pressed } = event {
            self.board.handle_input(id.0 as u8, pressed);
        }
    }
}
impl Profilable for DigDugSystem {}
/// DIP switch metadata for Dig Dug's two switch banks (DSWA at board byte
/// `dswa`, DSWB at `dswb`). Choice bit patterns and labels follow MAME's
/// `digdug` layout; each option's factory default OR's together to the
/// historical `0x99` (DSWA) and `0x24` (DSWB) that [`DigDugSystem::reset`]
/// initializes. The Bonus Life thresholds shown are those MAME displays for
/// the default (non-5) Lives setting.
const DIGDUG_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSWA",
        options: &[
            DipOption {
                name: "Coin B",
                mask: 0x07,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "1 Coin/7 Credits",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "1 Coin/1 Credit",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "1 Coin/3 Credits",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "2 Coins/1 Credit",
                        value: 0x03,
                    },
                    DipChoice {
                        label: "1 Coin/6 Credits",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "2 Coins/3 Credits",
                        value: 0x05,
                    },
                    DipChoice {
                        label: "1 Coin/2 Credits",
                        value: 0x06,
                    },
                    DipChoice {
                        label: "3 Coins/1 Credit",
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
                        label: "20K, 70K, Every 70K",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "10K, 50K, Every 50K",
                        value: 0x10,
                    },
                    DipChoice {
                        label: "20K and 60K Only",
                        value: 0x18,
                    },
                    DipChoice {
                        label: "10K, 40K, Every 40K",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "10K and 40K Only",
                        value: 0x28,
                    },
                    DipChoice {
                        label: "20K, 60K, Every 60K",
                        value: 0x30,
                    },
                    DipChoice {
                        label: "10K Only",
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
                        label: "1",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "2",
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
    DipSwitchBank {
        name: "DSWB",
        options: &[
            DipOption {
                name: "Coin A",
                mask: 0xC0,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "1 Coin/1 Credit",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "2 Coins/1 Credit",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "1 Coin/2 Credits",
                        value: 0x80,
                    },
                    DipChoice {
                        label: "2 Coins/3 Credits",
                        value: 0xC0,
                    },
                ],
            },
            DipOption {
                name: "Freeze",
                mask: 0x20,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Off",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "On",
                        value: 0x00,
                    },
                ],
            },
            DipOption {
                name: "Demo Sounds",
                mask: 0x10,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Off",
                        value: 0x10,
                    },
                    DipChoice {
                        label: "On",
                        value: 0x00,
                    },
                ],
            },
            DipOption {
                name: "Allow Continue",
                mask: 0x08,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Yes",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "No",
                        value: 0x08,
                    },
                ],
            },
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
            },
            DipOption {
                name: "Difficulty",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Easy",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Hard",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "Medium",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "Hardest",
                        value: 0x03,
                    },
                ],
            },
        ],
    },
];

crate::impl_dip_switches!(DigDugSystem, DIGDUG_DIP_BANKS, board.dswa, board.dswb);
crate::impl_board_debug_trace!(DigDugSystem, board);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

const ALL_CONFIGS: &[&DigDugRomConfig] = &[
    &DIGDUG_CONFIG,
    &DIGDUG1_CONFIG,
    &DIGDUGAT_CONFIG,
    &DIGDUGAT1_CONFIG,
];

crate::register_machine!(
    DigDugSystem,
    "digdug",
    &["digdug", "digdug1", "digdugat", "digdugat1"],
    namco_galaga::NAMCO_GALAGA_CONTROLS,
    configs = ALL_CONFIGS
);

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::DipSwitches;

    #[test]
    fn dip_defaults_match_historical_bytes() {
        let sys = DigDugSystem::new();
        // DSWA: 1C/1C coin B, 20K/60K bonus, 3 lives. DSWB: freeze off,
        // upright cabinet, everything else at factory default.
        assert_eq!(sys.dip_bank_value(0), 0x99);
        assert_eq!(sys.dip_bank_value(1), 0x24);
        // Two banks; out-of-range reads 0.
        assert_eq!(sys.dip_banks().len(), 2);
        assert_eq!(sys.dip_bank_value(2), 0);
    }

    #[test]
    fn dip_metadata_is_well_formed() {
        for bank in DigDugSystem::new().dip_banks() {
            let mut covered = 0u8;
            for opt in bank.options {
                assert_eq!(covered & opt.mask, 0, "overlapping masks in {}", bank.name);
                covered |= opt.mask;
                for choice in opt.choices {
                    assert_eq!(
                        choice.value & !opt.mask,
                        0,
                        "choice {} escapes mask of {}",
                        choice.label,
                        opt.name
                    );
                }
            }
            // Both Dig Dug banks fully populate their byte.
            assert_eq!(covered, 0xFF, "{} leaves bits unmapped", bank.name);
        }
    }

    #[test]
    fn dip_defaults_decompose_into_known_choices() {
        let sys = DigDugSystem::new();
        for (bank_idx, bank) in sys.dip_banks().iter().enumerate() {
            let default = sys.dip_bank_value(bank_idx);
            for opt in bank.options {
                let selected = default & opt.mask;
                assert!(
                    opt.choices.iter().any(|c| c.value == selected),
                    "{} default 0x{default:02X} has no choice for {} (slice 0x{selected:02X})",
                    bank.name,
                    opt.name
                );
            }
        }
    }

    #[test]
    fn set_dip_option_mutates_only_masked_bits() {
        let mut sys = DigDugSystem::new();
        // DSWA Lives is option index 2 (mask 0xC0); pick "5" (0xC0).
        sys.set_dip_option(0, 2, 0xC0);
        // 0x99 with bits 6-7 set -> 0xD9; lower bits unchanged.
        assert_eq!(sys.dip_bank_value(0), 0xD9);
        // DSWB untouched by a DSWA edit.
        assert_eq!(sys.dip_bank_value(1), 0x24);

        // Stray bits outside the mask are filtered out.
        sys.set_dip_option(0, 2, 0xFF);
        assert_eq!(sys.dip_bank_value(0) & 0xC0, 0xC0);
        assert_eq!(sys.dip_bank_value(0) & !0xC0, 0x19);
    }

    #[test]
    fn dip_bank_values_round_trip() {
        let mut sys = DigDugSystem::new();
        sys.set_dip_bank_value(0, 0x3C);
        sys.set_dip_bank_value(1, 0xC3);
        assert_eq!(sys.dip_bank_value(0), 0x3C);
        assert_eq!(sys.dip_bank_value(1), 0xC3);
        // Out-of-range writes are ignored.
        sys.set_dip_bank_value(5, 0xFF);
        assert_eq!(sys.dip_bank_value(0), 0x3C);
        assert_eq!(sys.dip_bank_value(1), 0xC3);
    }

    // Drive the ER2055 EAROM the way the game ROM does: latch address+data at
    // 0xB800-0xB83F, then pulse the control latch at 0xB840. The control byte is
    // bit0=CK, bit1=!C1, bit2=C2, bit3=CS1 (CS2 hardwired). Regression guard for
    // the C1 inversion: with C1 decoded non-inverted the erase/write modes flip,
    // the write becomes a no-op, and the stored value stays 0xFF.
    // `write`/`read` are ambiguous here (both Bus and BusDebug are in scope), so
    // the helpers pin them to the Bus trait — the CPU-facing memory map.
    fn cpu_write(sys: &mut DigDugSystem, addr: u16, data: u8) {
        Bus::write(sys, BusMaster::Cpu(0), addr, data);
    }
    fn cpu_read(sys: &mut DigDugSystem, addr: u16) -> u8 {
        Bus::read(sys, BusMaster::Cpu(0), addr)
    }

    // Commit the value latched at 0xB800+addr into the EAROM cell: erase (set to
    // 0xFF) then write (AND). Control byte 0xB840 bits: 0=CK, 1=!C1, 2=C2, 3=CS1.
    fn earom_commit(sys: &mut DigDugSystem) {
        cpu_write(sys, 0xB840, 0x0F); // erase, clock high
        cpu_write(sys, 0xB840, 0x0E); // erase, clock low
        cpu_write(sys, 0xB840, 0x0B); // write, clock high
        cpu_write(sys, 0xB840, 0x0A); // write, clock low
    }

    #[test]
    fn earom_write_read() {
        let mut sys = DigDugSystem::new();

        // Latch address 0x05 with data 0xAB, then commit it.
        cpu_write(&mut sys, 0xB805, 0xAB);
        earom_commit(&mut sys);

        // The EAROM read port returns the stored value.
        assert_eq!(cpu_read(&mut sys, 0xB805), 0xAB);
    }

    // The whole point of the EAROM is that high scores survive a power cycle:
    // a snapshot taken after a write must reload into a fresh machine.
    #[test]
    fn earom_nvram_round_trip() {
        let mut sys = DigDugSystem::new();

        cpu_write(&mut sys, 0xB800, 0x42);
        earom_commit(&mut sys);
        cpu_write(&mut sys, 0xB83F, 0x37);
        earom_commit(&mut sys);

        let snapshot = Nvram::save_nvram(&sys).expect("digdug has NVRAM").to_vec();

        let mut sys2 = DigDugSystem::new();
        Nvram::load_nvram(&mut sys2, &snapshot);
        assert_eq!(cpu_read(&mut sys2, 0xB800), 0x42);
        assert_eq!(cpu_read(&mut sys2, 0xB83F), 0x37);
    }

    #[test]
    fn declares_native_dims_and_rot90() {
        use phosphor_core::core::machine::{Orientation, Renderable};
        let sys = DigDugSystem::new();
        // Native landscape framebuffer; the frontend applies ROT90 to present
        // portrait. (Galaxian shares the Namco board and is already declarative.)
        assert_eq!(sys.display_size(), (288, 224));
        assert_eq!(sys.orientation(), Orientation::ROT90);
        assert!(sys.orientation().swaps_axes());
        assert_eq!(sys.display_aspect(), Some((3, 4)));
    }

    #[test]
    fn render_frame_emits_native_unrotated_rgb() {
        use phosphor_core::core::machine::Renderable;
        let mut sys = DigDugSystem::new();
        // Tag a native pixel + palette entry; confirm it lands at the native
        // row-major position (288 wide), i.e. no baked rotation.
        sys.board.palette_rgb[7] = (11, 22, 33);
        let (nx, ny) = (9usize, 3usize);
        sys.native_buffer[ny * 288 + nx] = 7;
        let mut buf = vec![0u8; 288 * 224 * 3];
        sys.render_frame(&mut buf);
        let i = (ny * 288 + nx) * 3;
        assert_eq!(&buf[i..i + 3], &[11, 22, 33]);
    }
}
