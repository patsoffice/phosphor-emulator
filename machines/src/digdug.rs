use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug::{BusDebug, DebugCpu, Debuggable};
use phosphor_core::core::machine::{
    AudioSource, InputReceiver, MachineCore, MachineDebug, Nvram, Profilable, Renderable, SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::device::Er2055;
use phosphor_core::gfx;
use phosphor_core::gfx::GfxCache;
use phosphor_core::gfx::decode::{GfxLayout, decode_gfx};
use phosphor_macros::Saveable;

use crate::namco_galaga::{self, GALAGA_SPRITE_LAYOUT, NamcoGalagaBoard};
use crate::namco_pac::PACMAN_TILE_LAYOUT;
use crate::registry::MachineEntry;
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
#[derive(Saveable)]
pub struct DigDugSystem {
    pub board: NamcoGalagaBoard,

    // RAM regions (shared by all 3 CPUs)
    video_ram: [u8; 0x400], // 0x8000-0x83FF (foreground tilemap)
    work_ram: [u8; 0x400],  // 0x8400-0x87FF (shared work RAM)
    obj_ram: [u8; 0x400],   // 0x8800-0x8BFF (sprite attribs + work)
    pos_ram: [u8; 0x400],   // 0x9000-0x93FF (sprite positions)
    flp_ram: [u8; 0x400],   // 0x9800-0x9BFF (sprite flip/size)
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
        Self {
            board: NamcoGalagaBoard::new(),

            video_ram: [0; 0x400],
            work_ram: [0; 0x400],
            obj_ram: [0; 0x400],
            pos_ram: [0; 0x400],
            flp_ram: [0; 0x400],
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
    // Full-frame video rendering (no raster effects in Dig Dug)
    // -----------------------------------------------------------------------

    fn render_video(&mut self) {
        // Layer 1: Background tilemap
        self.render_background();

        // Layer 2: Foreground text (1bpp, transparent pen 0)
        self.render_foreground();

        // Layer 3: Sprites
        self.render_sprites();
    }

    fn render_background(&mut self) {
        for tile_row in 0..28 {
            for tile_col in 0..36 {
                let offset = Self::tilemap_offset(tile_col as i32, tile_row as i32);
                let code = if offset < self.playfield_rom.len() {
                    self.playfield_rom[offset | ((self.bg_select as usize) << 10)] as usize
                } else {
                    0
                };

                let color = if self.bg_disable {
                    0x0F_usize
                } else {
                    ((code >> 4) | self.bg_color_bank as usize) & 0x3F
                };

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
                        let pixel = self.bg_tile_cache.pixel(code, px, py);
                        let lut_idx = color * 4 + pixel as usize;
                        let palette_idx = (self.bg_lut[lut_idx & 0xFF] & 0x0F) as usize;
                        self.native_buffer[row_off + screen_x] = palette_idx as u8;
                    }
                }
            }
        }
    }

    fn render_foreground(&mut self) {
        for tile_row in 0..28 {
            for tile_col in 0..36 {
                let offset = Self::tilemap_offset(tile_col as i32, tile_row as i32);
                if offset >= 0x400 {
                    continue;
                }
                let code = self.video_ram[offset] as usize;

                let color = if self.tx_color_mode {
                    code & 0x0F
                } else {
                    ((code >> 4) & 0x0E) | ((code >> 3) & 2)
                };

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
                        let pixel = self.char_cache.pixel(code & 0x7F, px, py);
                        if pixel == 0 {
                            continue; // transparent
                        }
                        // 1bpp: pen 1 → palette color = color group number (0-15)
                        let palette_idx = color as u8;
                        self.native_buffer[row_off + screen_x] = palette_idx;
                    }
                }
            }
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

            let sprite_byte = self.obj_ram[attr_addr];
            let color = (self.obj_ram[attr_addr + 1] & 0x3F) as usize;
            let sx = self.pos_ram[attr_addr + 1] as i32 - 40 + 1;
            let raw_sy = 256i32 - self.pos_ram[attr_addr] as i32 + 1;
            let flipx = self.flp_ram[attr_addr] & 0x01 != 0;
            let flipy = self.flp_ram[attr_addr] & 0x02 != 0;
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

        for py in 0..16 {
            let screen_y = sy + py as i32;
            if !(0..224).contains(&screen_y) {
                continue;
            }
            let src_py = if flipy { 15 - py } else { py };
            let row_off = screen_y as usize * 288;

            for px in 0..16 {
                let screen_x = sx + px as i32;
                // Clip to visible area (columns 2-33, i.e. pixels 16-271)
                if !(16..272).contains(&screen_x) {
                    continue;
                }

                let src_px = if flipx { 15 - px } else { px };
                let pixel = self.sprite_cache.pixel(code, src_px, src_py);

                // Look up through sprite color LUT
                let lut_idx = (color * 4 + pixel as usize) & 0xFF;
                let pal_lo = self.sprite_lut[lut_idx] & 0x0F;
                if pal_lo == 0x0F {
                    continue; // transparent
                }
                let palette_idx = pal_lo | 0x10;
                self.native_buffer[row_off + screen_x as usize] = palette_idx;
            }
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
            0x8000..=0x83FF => self.video_ram[(addr - 0x8000) as usize],
            0x8400..=0x87FF => self.work_ram[(addr - 0x8400) as usize],
            0x8800..=0x8BFF => self.obj_ram[(addr - 0x8800) as usize],
            0x9000..=0x93FF => self.pos_ram[(addr - 0x9000) as usize],
            0x9800..=0x9BFF => self.flp_ram[(addr - 0x9800) as usize],
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
        self.board.watch_write(master, addr, data);
        self.board.trace_bus_write(master, addr, data);
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
            0x8000..=0x83FF => self.video_ram[(addr - 0x8000) as usize] = data,
            0x8400..=0x87FF => self.work_ram[(addr - 0x8400) as usize] = data,
            0x8800..=0x8BFF => self.obj_ram[(addr - 0x8800) as usize] = data,
            0x9000..=0x93FF => self.pos_ram[(addr - 0x9000) as usize] = data,
            0x9800..=0x9BFF => self.flp_ram[(addr - 0x9800) as usize] = data,
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
                let c1 = data & 0x02 != 0; // bit 1 direct (Namco wiring)
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
        namco_galaga::TIMING.display_size()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        gfx::rotate_90_ccw_indexed(
            &self.native_buffer,
            buffer,
            288,
            224,
            &self.board.palette_rgb,
        );
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

impl InputReceiver for DigDugSystem {
    fn set_input(&mut self, button: u8, pressed: bool) {
        self.board.handle_input(button, pressed);
    }

    fn input_map(&self) -> &[phosphor_core::core::machine::InputButton] {
        namco_galaga::NAMCO_GALAGA_INPUT_MAP
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
                let offset = addr as usize;
                let rom = match cpu_index {
                    0 => &self.board.main_rom,
                    1 => &self.board.sub_rom,
                    2 => &self.board.sound_rom,
                    _ => return None,
                };
                Some(rom.get(offset).copied().unwrap_or(0xFF))
            }
            0x8000..=0x83FF => Some(self.video_ram[(addr - 0x8000) as usize]),
            0x8400..=0x87FF => Some(self.work_ram[(addr - 0x8400) as usize]),
            0x8800..=0x8BFF => Some(self.obj_ram[(addr - 0x8800) as usize]),
            0x9000..=0x93FF => Some(self.pos_ram[(addr - 0x9000) as usize]),
            0x9800..=0x9BFF => Some(self.flp_ram[(addr - 0x9800) as usize]),
            0xB800..=0xB83F => Some(self.earom.read(addr - 0xB800)),
            _ => None,
        }
    }

    fn write(&mut self, _cpu_index: usize, addr: u32, data: u8) {
        let Ok(addr) = u16::try_from(addr) else {
            return;
        };
        match addr {
            0x8000..=0x83FF => self.video_ram[(addr - 0x8000) as usize] = data,
            0x8400..=0x87FF => self.work_ram[(addr - 0x8400) as usize] = data,
            0x8800..=0x8BFF => self.obj_ram[(addr - 0x8800) as usize] = data,
            0x9000..=0x93FF => self.pos_ram[(addr - 0x9000) as usize] = data,
            0x9800..=0x9BFF => self.flp_ram[(addr - 0x9800) as usize] = data,
            0xB800..=0xB83F => self.earom.latch(addr - 0xB800, data),
            _ => {}
        }
    }

    // --- Watchpoints (board-level Watchpoints set; no AddressSpace16) ---

    fn take_watchpoint_hit(&mut self) -> Option<phosphor_core::core::watchpoint::WatchpointHit> {
        self.board.watchpoints.take_hit()
    }

    fn set_watchpoint(
        &mut self,
        cpu_index: usize,
        addr: u32,
        kind: phosphor_core::core::watchpoint::WatchpointKind,
    ) {
        self.board.watchpoints.set(cpu_index, addr, kind);
    }

    fn clear_watchpoint(
        &mut self,
        cpu_index: usize,
        addr: u32,
        kind: phosphor_core::core::watchpoint::WatchpointKind,
    ) {
        self.board.watchpoints.clear(cpu_index, addr, kind);
    }

    fn clear_all_watchpoints(&mut self) {
        self.board.watchpoints.clear_all();
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
        bus_split!(self, bus => {
            self.board.tick(bus);
        });
        self.board.debug_tick_boundaries()
    }
}

impl MachineCore for DigDugSystem {
    crate::machine_core_metadata!("digdug", namco_galaga::TIMING);

    fn run_frame(&mut self) {
        bus_split!(self, bus => {
            for _ in 0..namco_galaga::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });
        self.render_video();
    }

    fn reset(&mut self) {
        self.board.reset_board();
        self.video_ram.fill(0);
        self.work_ram.fill(0);
        self.obj_ram.fill(0);
        self.pos_ram.fill(0);
        self.flp_ram.fill(0);
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

impl Profilable for DigDugSystem {}
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

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut last_err = None;
    for config in ALL_CONFIGS {
        let mut sys = DigDugSystem::new();
        match sys.load_roms(rom_set, config) {
            Ok(()) => return Ok(Box::new(sys)),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap())
}

inventory::submit! {
    MachineEntry::new(
        "digdug",
        &["digdug", "digdug1", "digdugat", "digdugat1"],
        create_machine,
    )
}
