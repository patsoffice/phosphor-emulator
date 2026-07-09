//! Xevious (Namco, 1983) — runs on the shared Namco Galaga hardware.
//!
//! Three Z80s (main/sub/sound @ 3.072 MHz), a Namco WSG for melodic sound, and
//! the Namco 06XX bus arbiter fronting the 50XX (score/protection), 51XX (I/O)
//! and 54XX (explosion sound) custom MCUs. Unlike Galaga/Dig Dug, the DIP
//! switches are read directly at 0x6800-0x6807 rather than through a custom
//! chip, and the video hardware has three layers (a scrolling background
//! tilemap, a foreground text tilemap and sprites) with independent per-layer
//! scroll. Background tile data lives in ROM and is fetched through a hardware
//! lookup at 0xF000-0xFFFF.
//!
//! This is the first milestone: board plumbing, the full CPU memory map and the
//! 50XX start-up protection handshake — enough to boot. Graphics decode and the
//! three-layer renderer land in a later milestone, so `render_frame` currently
//! produces a blank field.

use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug::{BusDebug, DebugCpu, Debuggable};
use phosphor_core::core::machine::{
    AudioSource, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, DipSwitches, MachineCore,
    MachineDebug, Renderable, SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::gfx;
use phosphor_core::gfx::GfxCache;
use phosphor_core::gfx::decode::{GfxLayout, decode_gfx};
use phosphor_macros::Saveable;

use crate::gfx_registry::GfxRegion;
use crate::namco_galaga::{self, NamcoGalagaBoard};
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};

// ---------------------------------------------------------------------------
// ROM definitions
// ---------------------------------------------------------------------------

static XEVIOUS_MAIN_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "xvi_1.3p",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x09964dda],
        },
        RomEntry {
            name: "xvi_2.3m",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x60ecce84],
        },
        RomEntry {
            name: "xvi_3.2m",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0x79754b7d],
        },
        RomEntry {
            name: "xvi_4.2l",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0xc7d4bbf0],
        },
    ],
};

static XEVIOUS_SUB_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "xvi_5.3f",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xc85b703f],
        },
        RomEntry {
            name: "xvi_6.3j",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0xe18cdaad],
        },
    ],
};

static XEVIOUS_SOUND_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "xvi_7.2c",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0xdd35cf1c],
    }],
};

/// Background tilemap data ROMs (2A/2B/2C), addressed through the hardware
/// lookup at 0xF000-0xFFFF.
static XEVIOUS_GFX4_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "xvi_9.2a",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x57ed9879],
        },
        RomEntry {
            name: "xvi_10.2b",
            size: 0x2000,
            offset: 0x1000,
            crc32: &[0xae3ba9e5],
        },
        RomEntry {
            name: "xvi_11.2c",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0x31e244dd],
        },
    ],
};

/// Namco WSG waveform PROM (256 bytes).
static XEVIOUS_SOUND_PROM: RomRegion = RomRegion {
    size: 0x0100,
    entries: &[RomEntry {
        name: "xvi-2.7n",
        size: 0x0100,
        offset: 0x0000,
        crc32: &[0x550f06bc],
    }],
};

/// Foreground characters (gfx1): 1bpp 8×8, 512 tiles.
static XEVIOUS_GFX1_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "xvi_12.3b",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0x088c8b26],
    }],
};

/// Background tiles (gfx2): 2bpp 8×8, planes split across the two ROMs, 512 tiles.
static XEVIOUS_GFX2_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "xvi_13.3c",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xde60ba25],
        },
        RomEntry {
            name: "xvi_14.3d",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x535cdbbc],
        },
    ],
};

/// Sprites (gfx3): 3bpp 16×16. Planes 1/2 live in the first half (0x0000-0x4FFF,
/// three sprite sets), plane 0 in the second half. The plane-0 ROM (xvi_18)
/// packs two nibbles per byte and is unpacked into 0x7000-0x8FFF at load time;
/// the region is padded to 0xA000 so the RGN_FRAC(1,2) split lands correctly.
static XEVIOUS_GFX3_ROM: RomRegion = RomRegion {
    size: 0xA000,
    entries: &[
        RomEntry {
            name: "xvi_15.4m",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xdc2c0ecb],
        },
        RomEntry {
            name: "xvi_17.4p",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0xdfb587ce],
        },
        RomEntry {
            name: "xvi_16.4n",
            size: 0x1000,
            offset: 0x4000,
            crc32: &[0x605ca889],
        },
        RomEntry {
            name: "xvi_18.4r",
            size: 0x2000,
            offset: 0x5000,
            crc32: &[0x02417d19],
        },
    ],
};

/// Color PROMs: three 256×4 RGB palette PROMs then four 512×4 lookup PROMs
/// (BG low/high, sprite low/high).
static XEVIOUS_PROMS: RomRegion = RomRegion {
    size: 0x0B00,
    entries: &[
        RomEntry {
            name: "xvi-8.6a",
            size: 0x0100,
            offset: 0x0000,
            crc32: &[0x5cc2727f],
        },
        RomEntry {
            name: "xvi-9.6d",
            size: 0x0100,
            offset: 0x0100,
            crc32: &[0x5c8796cc],
        },
        RomEntry {
            name: "xvi-10.6e",
            size: 0x0100,
            offset: 0x0200,
            crc32: &[0x3cb60975],
        },
        RomEntry {
            name: "xvi-7.4h",
            size: 0x0200,
            offset: 0x0300,
            crc32: &[0x22d98032],
        },
        RomEntry {
            name: "xvi-6.4f",
            size: 0x0200,
            offset: 0x0500,
            crc32: &[0x3a7599f0],
        },
        RomEntry {
            name: "xvi-4.3l",
            size: 0x0200,
            offset: 0x0700,
            crc32: &[0xfd8b9d91],
        },
        RomEntry {
            name: "xvi-5.3m",
            size: 0x0200,
            offset: 0x0900,
            crc32: &[0xbf906d82],
        },
    ],
};

// ---------------------------------------------------------------------------
// GFX layouts (bit offsets; plane_offsets are LSB-first, i.e. reversed from
// MAME's MSB-first plane order).
// ---------------------------------------------------------------------------

/// Foreground characters: 1bpp 8×8.
const FG_CHAR_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0],
    x_offsets: &[0, 1, 2, 3, 4, 5, 6, 7],
    y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
    char_increment: 64,
};

/// Background tiles: 2bpp 8×8, planes split at the region half (0x1000 bytes =
/// 0x8000 bits): pixel bit 0 from the upper half, bit 1 from the lower.
const BG_TILE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0x8000, 0],
    x_offsets: &[0, 1, 2, 3, 4, 5, 6, 7],
    y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
    char_increment: 64,
};

/// Sprites: 3bpp 16×16. Planes 1/2 are in the first half of the region; plane 0
/// (pixel bit 2) is in the second half (RGN_FRAC(1,2) = 0x5000 bytes = 0x28000
/// bits) offset by 4.
const SPRITE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[4, 0, 0x28004],
    x_offsets: &[
        0, 1, 2, 3, 64, 65, 66, 67, 128, 129, 130, 131, 192, 193, 194, 195,
    ],
    y_offsets: &[
        0, 8, 16, 24, 32, 40, 48, 56, 256, 264, 272, 280, 288, 296, 304, 312,
    ],
    char_increment: 512,
};

/// Number of decoded elements in each sheet.
const FG_CHAR_COUNT: usize = 512; // 0x1000 / 8 bytes
const BG_TILE_COUNT: usize = 512; // (0x2000 / 2) / 8 bytes
const SPRITE_COUNT: usize = 320; // (0xA000 / 2) / 64 bytes

/// Load the sprite ROM region and unpack the plane-0 ROM (xvi_18): its high
/// nibble at 0x5000-0x6FFF is shifted into 0x7000-0x8FFF so each sprite set's
/// third bit plane is addressable separately (0x9000-0x9FFF stays zero —
/// sprite set #3 has no third plane). Shared by the runtime and gfxview paths.
fn xevious_load_gfx3(rom_set: &RomSet) -> Result<Vec<u8>, RomLoadError> {
    let mut gfx3 = XEVIOUS_GFX3_ROM.load(rom_set)?;
    for i in 0..0x2000 {
        gfx3[0x7000 + i] = gfx3[0x5000 + i] >> 4;
    }
    Ok(gfx3)
}

/// Build the 256-slot indirect RGB palette (colours 0x00-0x7F from the three
/// 4-bit R/G/B PROMs via the resistor-DAC weights; index 0x80 = transparent
/// black; the rest black).
fn xevious_palette_rgb(proms: &[u8]) -> Vec<(u8, u8, u8)> {
    const W: [u32; 4] = [0x0e, 0x1f, 0x43, 0x8f];
    let dac = |v: u8| -> u8 { (0..4).map(|b| W[b] * ((v >> b) & 1) as u32).sum::<u32>() as u8 };
    let mut pal = vec![(0u8, 0u8, 0u8); 256];
    for i in 0..128 {
        pal[i] = (dac(proms[i]), dac(proms[0x100 + i]), dac(proms[0x200 + i]));
    }
    pal[0x80] = (0, 0, 0);
    pal
}

/// gfxview palette hook: build the RGB palette from the colour PROMs.
fn xevious_gfx_palette(rom_set: &RomSet) -> Result<Vec<(u8, u8, u8)>, RomLoadError> {
    Ok(xevious_palette_rgb(&XEVIOUS_PROMS.load(rom_set)?))
}

inventory::submit! {
    GfxRegion {
        machine: "xevious",
        region: "fg_chars",
        count: FG_CHAR_COUNT as u32,
        width: 8,
        height: 8,
        layout: &FG_CHAR_LAYOUT,
        load: |rs| XEVIOUS_GFX1_ROM.load(rs),
        palette: Some(xevious_gfx_palette),
    }
}
inventory::submit! {
    GfxRegion {
        machine: "xevious",
        region: "bg_tiles",
        count: BG_TILE_COUNT as u32,
        width: 8,
        height: 8,
        layout: &BG_TILE_LAYOUT,
        load: |rs| XEVIOUS_GFX2_ROM.load(rs),
        palette: Some(xevious_gfx_palette),
    }
}
inventory::submit! {
    GfxRegion {
        machine: "xevious",
        region: "sprites",
        count: SPRITE_COUNT as u32,
        width: 16,
        height: 16,
        layout: &SPRITE_LAYOUT,
        load: xevious_load_gfx3,
        palette: Some(xevious_gfx_palette),
    }
}

// ---------------------------------------------------------------------------
// XeviousSystem
// ---------------------------------------------------------------------------

/// Xevious Arcade System (Namco, 1983).
///
/// Screen: 288×224, rotated 90° CCW for a vertical display.
#[derive(Saveable)]
pub struct XeviousSystem {
    pub board: NamcoGalagaBoard,

    // RAM regions (shared by all three CPUs), 2KB each.
    work_ram: [u8; 0x800],    // 0x7800-0x7FFF
    sr1: [u8; 0x800],         // 0x8000-0x87FF (work RAM + sprite X/Y regs)
    sr2: [u8; 0x800],         // 0x9000-0x97FF (work RAM + sprite flip/size regs)
    sr3: [u8; 0x800],         // 0xA000-0xA7FF (work RAM + sprite tile/color regs)
    fg_colorram: [u8; 0x800], // 0xB000-0xB7FF
    bg_colorram: [u8; 0x800], // 0xB800-0xBFFF
    fg_videoram: [u8; 0x800], // 0xC000-0xC7FF
    bg_videoram: [u8; 0x800], // 0xC800-0xCFFF

    // Scroll latch (0xD000-0xD07F): 9-bit scroll per layer, plus flip.
    bg_scroll_x: u16,
    fg_scroll_x: u16,
    bg_scroll_y: u16,
    fg_scroll_y: u16,

    // Background-map lookup selector (two bytes written to 0xF000-0xFFFF).
    bs: [u8; 2],

    // Background tilemap data ROMs (2A/2B/2C combined), read via the lookup.
    #[save_skip]
    playfield_rom: Vec<u8>,

    // Decoded graphics.
    #[save_skip]
    char_cache: GfxCache, // FG chars, 1bpp 8×8
    #[save_skip]
    bg_tile_cache: GfxCache, // BG tiles, 2bpp 8×8
    #[save_skip]
    sprite_cache: GfxCache, // sprites, 3bpp 16×16

    // Colour lookup tables mapping a gfx pen to an indirect palette index
    // (0x00-0x7F = colour, 0x80 = transparent).
    #[save_skip]
    bg_lut: [u8; 512],
    #[save_skip]
    sprite_lut: [u8; 512],

    // Frame buffer (288 × 224 native, rotated in render_frame). Holds indirect
    // palette indices; blank until the renderer lands (later M2 tasks).
    #[save_skip]
    native_buffer: Vec<u8>,
    // 128 indirect colours (index 0x80 is the transparent black marker).
    #[save_skip]
    palette: Vec<(u8, u8, u8)>,
}

impl XeviousSystem {
    pub fn new() -> Self {
        Self {
            board: NamcoGalagaBoard::new(),
            work_ram: [0; 0x800],
            sr1: [0; 0x800],
            sr2: [0; 0x800],
            sr3: [0; 0x800],
            fg_colorram: [0; 0x800],
            bg_colorram: [0; 0x800],
            fg_videoram: [0; 0x800],
            bg_videoram: [0; 0x800],
            bg_scroll_x: 0,
            fg_scroll_x: 0,
            bg_scroll_y: 0,
            fg_scroll_y: 0,
            bs: [0; 2],
            playfield_rom: Vec::new(),
            char_cache: GfxCache::new(0, 8, 8),
            bg_tile_cache: GfxCache::new(0, 8, 8),
            sprite_cache: GfxCache::new(0, 16, 16),
            bg_lut: [0; 512],
            sprite_lut: [0; 512],
            native_buffer: vec![0u8; 288 * 224],
            palette: vec![(0, 0, 0); 256],
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        // Program ROMs (per CPU).
        self.board.load_main_rom(&XEVIOUS_MAIN_ROM.load(rom_set)?);
        self.board.load_sub_rom(&XEVIOUS_SUB_ROM.load(rom_set)?);
        self.board.load_sound_rom(&XEVIOUS_SOUND_ROM.load(rom_set)?);

        // Background tilemap data ROMs, and the WSG waveform.
        self.playfield_rom = XEVIOUS_GFX4_ROM.load(rom_set)?;
        self.board
            .load_sound_prom(&XEVIOUS_SOUND_PROM.load(rom_set)?);

        // Decode graphics.
        let gfx1 = XEVIOUS_GFX1_ROM.load(rom_set)?;
        self.char_cache = decode_gfx(&gfx1, 0, FG_CHAR_COUNT, &FG_CHAR_LAYOUT);

        let gfx2 = XEVIOUS_GFX2_ROM.load(rom_set)?;
        self.bg_tile_cache = decode_gfx(&gfx2, 0, BG_TILE_COUNT, &BG_TILE_LAYOUT);

        let gfx3 = xevious_load_gfx3(rom_set)?;
        self.sprite_cache = decode_gfx(&gfx3, 0, SPRITE_COUNT, &SPRITE_LAYOUT);

        // Colour PROMs → indirect palette + lookup tables.
        let proms = XEVIOUS_PROMS.load(rom_set)?;
        self.build_palette(&proms);

        // Fit the 50XX score/protection chip, which Xevious queries with a
        // periodic protection check.
        self.board.fit_50xx();

        // Factory DIP defaults: both banks read all-ones with switches at their
        // shipped positions and the button-2 lines released.
        self.board.dswa = 0xFF;
        self.board.dswb = 0xFF;

        Ok(())
    }

    /// Build the 128-entry indirect RGB palette and the BG/sprite colour lookup
    /// tables from the seven colour PROMs.
    fn build_palette(&mut self, proms: &[u8]) {
        self.palette = xevious_palette_rgb(proms);

        // Background lookup: BG low (xvi-7) + BG high (xvi-6), 512 pens.
        let bg_low = &proms[0x300..0x500];
        let bg_high = &proms[0x500..0x700];
        for i in 0..512 {
            self.bg_lut[i] = (bg_low[i] & 0x0F) | ((bg_high[i] & 0x0F) << 4);
        }
        // Sprite lookup: sprite low (xvi-4) + sprite high (xvi-5), 512 pens.
        // Bit 7 of the combined value marks an opaque pixel; otherwise the pen
        // is transparent (maps to the 0x80 marker).
        let spr_low = &proms[0x700..0x900];
        let spr_high = &proms[0x900..0xB00];
        for i in 0..512 {
            let c = (spr_low[i] & 0x0F) | ((spr_high[i] & 0x0F) << 4);
            self.sprite_lut[i] = if c & 0x80 != 0 { c & 0x7F } else { 0x80 };
        }
    }

    /// Main-CPU program counter (for headless boot checks).
    pub fn main_pc(&self) -> u16 {
        self.board.main_cpu.pc
    }

    /// True once the main CPU has released the sub/sound CPUs from reset
    /// (LS259 Q3), which Xevious does only after clearing early init.
    pub fn sub_released(&self) -> bool {
        !self.board.sub_reset
    }

    /// Sub/sound CPU program counters and main IRQ enable state (boot checks).
    pub fn sub_pc(&self) -> u16 {
        self.board.sub_cpu.pc
    }
    pub fn sound_pc(&self) -> u16 {
        self.board.sound_cpu.pc
    }
    pub fn main_irq_on(&self) -> bool {
        self.board.main_irq_enabled
    }

    /// Count of non-zero bytes in the foreground and background video RAM —
    /// a proxy for "the attract screen has been drawn" during boot checks.
    pub fn video_ram_nonzero(&self) -> (usize, usize) {
        let fg = self.fg_videoram.iter().filter(|&&b| b != 0).count();
        let bg = self.bg_videoram.iter().filter(|&&b| b != 0).count();
        (fg, bg)
    }

    /// Read the interleaved DIP switch port at 0x6800-0x6807. Each address
    /// returns one bit of each bank: bit 0 from DSWB, bit 1 from DSWA.
    fn dsw_read(&self, offset: u8) -> u8 {
        let bit0 = (self.board.dswb >> offset) & 1;
        let bit1 = (self.board.dswa >> offset) & 1;
        bit0 | (bit1 << 1)
    }

    /// Write the video latch at 0xD000-0xD07F. The register is selected by
    /// address bits 4-7; the 9th scroll bit comes from address bit 0.
    fn write_video_latch(&mut self, offset: u8, data: u8) {
        let scroll = (data as u16) | (((offset & 0x01) as u16) << 8);
        match (offset >> 4) & 0x0F {
            0 => self.bg_scroll_x = scroll,
            1 => self.fg_scroll_x = scroll,
            2 => self.bg_scroll_y = scroll,
            3 => self.fg_scroll_y = scroll,
            7 => self.board.flip_screen = (scroll & 1) != 0,
            _ => {}
        }
    }

    /// Set one of the two background-map selector bytes (0xF000-0xFFFF write).
    fn write_bg_select(&mut self, addr: u16, data: u8) {
        self.bs[(addr & 1) as usize] = data;
    }

    /// Background-map lookup ("schematic 9B"). Given the current selector bytes,
    /// walk the 2A/2B/2C data ROMs to produce the tile number (even address) or
    /// its attribute byte (odd address) for the scrolling background layer.
    fn read_bg_map(&self, addr: u16) -> u8 {
        let rom = &self.playfield_rom;
        // Sub-ROM bases within the combined gfx4 region.
        let rom2a = 0x0000usize; // xvi_9  (0x1000)
        let rom2b = 0x1000usize; // xvi_10 (0x2000)
        let rom2c = 0x3000usize; // xvi_11 (0x1000)
        let at = |base: usize, i: usize| rom.get(base + i).copied().unwrap_or(0) as usize;

        let bs0 = self.bs[0] as usize;
        let bs1 = self.bs[1] as usize;

        // 12-bit address into 2A/2B.
        let adr_2b = ((bs1 & 0x7e) << 6) | ((bs0 & 0xfe) >> 1);
        let dat1 = if adr_2b & 1 != 0 {
            // High-nibble select from 2A.
            ((at(rom2a, adr_2b >> 1) & 0xf0) << 4) | at(rom2b, adr_2b)
        } else {
            // Low-nibble select from 2A.
            ((at(rom2a, adr_2b >> 1) & 0x0f) << 8) | at(rom2b, adr_2b)
        };

        let mut adr_2c = ((dat1 & 0x1ff) << 2) | ((bs1 & 1) << 1) | (bs0 & 1);
        if dat1 & 0x400 != 0 {
            adr_2c ^= 1;
        }
        if dat1 & 0x200 != 0 {
            adr_2c ^= 2;
        }

        if addr & 1 != 0 {
            // Attribute byte (BB1).
            at(rom2c, adr_2c | 0x800) as u8
        } else {
            // Tile number (BB0): swap bits 6 and 7, then apply the flip bits.
            let raw = at(rom2c, adr_2c) as u8;
            let mut dat2 = (raw & 0x3F) | ((raw & 0x40) << 1) | ((raw & 0x80) >> 1);
            if dat1 & 0x400 != 0 {
                dat2 ^= 0x40;
            }
            if dat1 & 0x200 != 0 {
                dat2 ^= 0x80;
            }
            dat2
        }
    }
}

impl Default for XeviousSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for XeviousSystem {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let data = match addr {
            0x0000..=0x3FFF => self.board.read_rom(master, addr),
            0x6800..=0x6807 => self.dsw_read((addr & 0x07) as u8),
            0x7000..=0x70FF => self.board.read_custom_io(),
            0x7100 => self.board.namco06.ctrl_read(),
            0x7800..=0x7FFF => self.work_ram[(addr - 0x7800) as usize],
            0x8000..=0x87FF => self.sr1[(addr - 0x8000) as usize],
            0x9000..=0x97FF => self.sr2[(addr - 0x9000) as usize],
            0xA000..=0xA7FF => self.sr3[(addr - 0xA000) as usize],
            0xB000..=0xB7FF => self.fg_colorram[(addr - 0xB000) as usize],
            0xB800..=0xBFFF => self.bg_colorram[(addr - 0xB800) as usize],
            0xC000..=0xC7FF => self.fg_videoram[(addr - 0xC000) as usize],
            0xC800..=0xCFFF => self.bg_videoram[(addr - 0xC800) as usize],
            0xF000..=0xFFFF => self.read_bg_map(addr),
            _ => 0xFF,
        };
        self.board.watch_read(master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.board.watch_write(master, addr, data);
        self.board.trace_bus_write(master, addr, data);
        match addr {
            0x0000..=0x3FFF => {} // ROM (nopw)
            0x6800..=0x681F => self.board.wsg.write(addr - 0x6800, data),
            0x6820..=0x6827 => {
                let bit = (addr & 7) as u8;
                self.board.write_misc_latch(bit, (data & 1) != 0);
            }
            0x6830 => self.board.watchdog_counter = 0,
            0x7000..=0x70FF => self.board.write_custom_io(data),
            0x7100 => self.board.write_custom_io_ctrl(data),
            0x7800..=0x7FFF => self.work_ram[(addr - 0x7800) as usize] = data,
            0x8000..=0x87FF => self.sr1[(addr - 0x8000) as usize] = data,
            0x9000..=0x97FF => self.sr2[(addr - 0x9000) as usize] = data,
            0xA000..=0xA7FF => self.sr3[(addr - 0xA000) as usize] = data,
            0xB000..=0xB7FF => self.fg_colorram[(addr - 0xB000) as usize] = data,
            0xB800..=0xBFFF => self.bg_colorram[(addr - 0xB800) as usize] = data,
            0xC000..=0xC7FF => self.fg_videoram[(addr - 0xC000) as usize] = data,
            0xC800..=0xCFFF => self.bg_videoram[(addr - 0xC800) as usize] = data,
            0xD000..=0xD07F => self.write_video_latch((addr & 0x7F) as u8, data),
            0xF000..=0xFFFF => self.write_bg_select(addr, data),
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

impl Renderable for XeviousSystem {
    fn display_size(&self) -> (u32, u32) {
        namco_galaga::TIMING.display_size()
    }

    fn display_aspect(&self) -> Option<(u32, u32)> {
        namco_galaga::TIMING.display_aspect()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        gfx::rotate_90_ccw_indexed(&self.native_buffer, buffer, 288, 224, &self.palette);
    }
}

impl AudioSource for XeviousSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.board.fill_audio(buffer)
    }

    fn audio_sample_rate(&self) -> u32 {
        44100
    }
}

impl BusDebug for XeviousSystem {
    fn devices(&self) -> Vec<(&str, &dyn Debuggable)> {
        vec![
            ("Z80 Main", &self.board.main_cpu as &dyn Debuggable),
            ("Z80 Sub", &self.board.sub_cpu as &dyn Debuggable),
            ("Z80 Sound", &self.board.sound_cpu as &dyn Debuggable),
            ("Namco WSG", &self.board.wsg as &dyn Debuggable),
            ("Namco 06XX", &self.board.namco06 as &dyn Debuggable),
            ("Namco 51XX", &self.board.namco51 as &dyn Debuggable),
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
        let addr = u16::try_from(addr).ok()?;
        match addr {
            0x0000..=0x3FFF => {
                let rom = match cpu_index {
                    0 => &self.board.main_rom,
                    1 => &self.board.sub_rom,
                    2 => &self.board.sound_rom,
                    _ => return None,
                };
                Some(rom.get(addr as usize).copied().unwrap_or(0xFF))
            }
            0x7800..=0x7FFF => Some(self.work_ram[(addr - 0x7800) as usize]),
            0x8000..=0x87FF => Some(self.sr1[(addr - 0x8000) as usize]),
            0x9000..=0x97FF => Some(self.sr2[(addr - 0x9000) as usize]),
            0xA000..=0xA7FF => Some(self.sr3[(addr - 0xA000) as usize]),
            0xB000..=0xB7FF => Some(self.fg_colorram[(addr - 0xB000) as usize]),
            0xB800..=0xBFFF => Some(self.bg_colorram[(addr - 0xB800) as usize]),
            0xC000..=0xC7FF => Some(self.fg_videoram[(addr - 0xC000) as usize]),
            0xC800..=0xCFFF => Some(self.bg_videoram[(addr - 0xC800) as usize]),
            _ => None,
        }
    }

    fn write(&mut self, _cpu_index: usize, addr: u32, data: u8) {
        let Ok(addr) = u16::try_from(addr) else {
            return;
        };
        match addr {
            0x7800..=0x7FFF => self.work_ram[(addr - 0x7800) as usize] = data,
            0x8000..=0x87FF => self.sr1[(addr - 0x8000) as usize] = data,
            0x9000..=0x97FF => self.sr2[(addr - 0x9000) as usize] = data,
            0xA000..=0xA7FF => self.sr3[(addr - 0xA000) as usize] = data,
            0xB000..=0xB7FF => self.fg_colorram[(addr - 0xB000) as usize] = data,
            0xB800..=0xBFFF => self.bg_colorram[(addr - 0xB800) as usize] = data,
            0xC000..=0xC7FF => self.fg_videoram[(addr - 0xC000) as usize] = data,
            0xC800..=0xCFFF => self.bg_videoram[(addr - 0xC800) as usize] = data,
            _ => {}
        }
    }

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

impl MachineDebug for XeviousSystem {
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

impl MachineCore for XeviousSystem {
    crate::machine_core_metadata!("xevious", namco_galaga::TIMING);

    fn gfx_sheets(&self) -> Vec<phosphor_core::core::machine::GfxSheet<'_>> {
        use phosphor_core::core::machine::GfxSheet;
        vec![
            GfxSheet {
                name: "fg_chars",
                cache: &self.char_cache,
                palette: &self.palette,
            },
            GfxSheet {
                name: "bg_tiles",
                cache: &self.bg_tile_cache,
                palette: &self.palette,
            },
            GfxSheet {
                name: "sprites",
                cache: &self.sprite_cache,
                palette: &self.palette,
            },
        ]
    }

    fn run_frame(&mut self) {
        bus_split!(self, bus => {
            for _ in 0..namco_galaga::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });
        // Video rendering lands in the next milestone; the frame stays blank.
    }

    fn reset(&mut self) {
        self.board.reset_board();
        self.work_ram.fill(0);
        self.sr1.fill(0);
        self.sr2.fill(0);
        self.sr3.fill(0);
        self.fg_colorram.fill(0);
        self.bg_colorram.fill(0);
        self.fg_videoram.fill(0);
        self.bg_videoram.fill(0);
        self.bg_scroll_x = 0;
        self.fg_scroll_x = 0;
        self.bg_scroll_y = 0;
        self.fg_scroll_y = 0;
        self.bs = [0; 2];
        self.native_buffer.fill(0);

        bus_split!(self, bus => {
            self.board.main_cpu.reset(bus, BusMaster::Cpu(0));
            self.board.sub_cpu.reset(bus, BusMaster::Cpu(1));
            self.board.sound_cpu.reset(bus, BusMaster::Cpu(2));
        });
    }
}

impl SaveState for XeviousSystem {
    crate::machine_save_state!();
}

impl phosphor_core::core::machine::Nvram for XeviousSystem {}
impl phosphor_core::core::machine::InputConfigurable for XeviousSystem {
    fn input_controls(&self) -> &'static [phosphor_core::core::machine::InputControl] {
        namco_galaga::NAMCO_GALAGA_CONTROLS
    }

    fn handle_input(&mut self, event: phosphor_core::core::machine::InputEvent) {
        if let phosphor_core::core::machine::InputEvent::Button { id, pressed } = event {
            self.board.handle_input(id.0 as u8, pressed);
        }
    }
}
impl phosphor_core::core::machine::Profilable for XeviousSystem {}

// ---------------------------------------------------------------------------
// DIP switches
// ---------------------------------------------------------------------------

/// Xevious DIP banks (DSWA at board byte `dswa`, DSWB at `dswb`). Both banks
/// default to 0xFF at the factory settings. The DSWB button-2 (bomb) bits and
/// the conditional bonus-life option are not modelled yet and keep their
/// power-on value.
const XEVIOUS_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSWA",
        options: &[
            DipOption {
                name: "Coin A",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "2C/3C",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "2C/1C",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "1C/2C",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "1C/1C",
                        value: 0x03,
                    },
                ],
            },
            DipOption {
                name: "Lives",
                mask: 0x60,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "5",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "2",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "1",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "3",
                        value: 0x60,
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
                name: "Flags Award Bonus Life",
                mask: 0x02,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "No",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Yes",
                        value: 0x02,
                    },
                ],
            },
            DipOption {
                name: "Coin B",
                mask: 0x0C,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "2C/3C",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "2C/1C",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "1C/2C",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "1C/1C",
                        value: 0x0C,
                    },
                ],
            },
            DipOption {
                name: "Difficulty",
                mask: 0x60,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Hardest",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Hard",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "Easy",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "Normal",
                        value: 0x60,
                    },
                ],
            },
            DipOption {
                name: "Freeze",
                mask: 0x80,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "On",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Off",
                        value: 0x80,
                    },
                ],
            },
        ],
    },
];

impl DipSwitches for XeviousSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        XEVIOUS_DIP_BANKS
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        match bank {
            0 => self.board.dswa,
            1 => self.board.dswb,
            _ => 0,
        }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        match bank {
            0 => self.board.dswa = value,
            1 => self.board.dswb = value,
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = XeviousSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("xevious", &["xevious"], create_machine)
}

crate::impl_board_debug_trace!(XeviousSystem, board);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_dac_and_luts() {
        let mut sys = XeviousSystem::new();
        let mut proms = vec![0u8; 0x0B00];
        // Palette entry 1: R = bit3 (0x8f), G = bit0 (0x0e), B = all bits (0xff).
        proms[1] = 0x08;
        proms[0x100 + 1] = 0x01;
        proms[0x200 + 1] = 0x0F;
        // BG pen 0: low nibble 5, high nibble 3 -> 0x35.
        proms[0x300] = 0x05;
        proms[0x500] = 0x03;
        // Sprite pen 0: low 5, high 0xB -> 0xB5, bit7 set -> opaque colour 0x35.
        proms[0x700] = 0x05;
        proms[0x900] = 0x0B;
        // Sprite pen 1: low 1, high 0 -> 0x01, bit7 clear -> transparent 0x80.
        proms[0x701] = 0x01;

        sys.build_palette(&proms);

        assert_eq!(sys.palette[1], (0x8f, 0x0e, 0xff));
        assert_eq!(sys.palette[0x80], (0, 0, 0));
        assert_eq!(sys.bg_lut[0], 0x35);
        assert_eq!(sys.sprite_lut[0], 0x35);
        assert_eq!(sys.sprite_lut[1], 0x80);
    }

    #[test]
    fn gfx_caches_start_empty() {
        // Element dimensions are fixed at construction; ROM decode fills them.
        let sys = XeviousSystem::new();
        assert_eq!(sys.char_cache.count(), 0);
        assert_eq!(sys.bg_tile_cache.count(), 0);
        assert_eq!(sys.sprite_cache.count(), 0);
    }

    /// A tiny synthetic 3bpp sprite exercises the plane-order + RGN_FRAC(1,2)
    /// split so a regression in the layout is caught without ROMs. Region is
    /// 0xA000; plane 0 lives at bit 0x28004, planes 1/2 at bits 4 and 0.
    #[test]
    fn sprite_layout_plane_order() {
        let mut rom = vec![0u8; 0xA000];
        // Sprite 0, px0/py0. Bits are read MSB-first, so plane offset N of byte
        // B reads bit (7 - (N & 7)). Plane offsets are [4, 0, 0x28004]:
        //   pixel bit0 <- byte 0 bit 3   (offset 4)
        //   pixel bit1 <- byte 0 bit 7   (offset 0)
        //   pixel bit2 <- byte 0x5000 bit 3 (offset 0x28004, the RGN_FRAC half)
        rom[0] = 0b1000_1000;
        rom[0x5000] = 0b0000_1000;
        let cache = decode_gfx(&rom, 0, 1, &SPRITE_LAYOUT);
        assert_eq!(cache.pixel(0, 0, 0), 0b111);
    }
}
