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
//! The renderer composites the three layers in hardware order: the opaque
//! scrolling background, then 3bpp 16×16 sprites (double width/height, flip,
//! pen 0x80 transparent), then the transparent foreground text on top, into a
//! 288×224 native buffer rotated 90° CCW for the vertical display.
//!
//! Audio: melodic and effect sound is the shared Namco WSG (3 voices, mapped at
//! 0x6800-0x681F and driven by the sound Z80), delegated through the board's
//! `AudioSource`. The 54XX explosion channel — a discrete analog noise network
//! fed by the write-only 54XX MCU on 06XX chip-select 3 — is not yet modelled,
//! so explosions are silent; the seam to add it lives at
//! `namco_galaga.rs` chip-select-3 dispatch (see `write_custom_io`).

use phosphor_core::bus_split;
use phosphor_core::core::address_space::AccessKind;
use phosphor_core::core::address_space16::WriteAnnotation;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug::{BusDebug, DebugCpu, Debuggable};
use phosphor_core::core::debug_trace::DebugEventKind;
use phosphor_core::core::machine::{
    ActionRole, AudioSource, DipApplyTiming, DipChoice, DipOption, DipSwitchBank,
    InputConfigurable, InputControl, InputEvent, InputId, InputKind, MachineCore, MachineDebug,
    Renderable, SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::gfx;
use phosphor_core::gfx::GfxCache;
use phosphor_core::gfx::decode::{GfxLayout, decode_gfx};
use phosphor_macros::{MemoryRegion, Saveable};

use crate::gfx_registry::GfxRegion;
use crate::namco_galaga::{self, NamcoGalagaBoard};
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
// GFX layouts (bit offsets; plane_offsets are listed LSB-first — the low plane
// bit first — which is the reverse of the ROM's MSB-first pixel bit packing).
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

// Screen-space origin of each tilemap within the 288×224 viewport: the source
// pixel shown at native (x, y) is (x + scroll + DX, y + scroll + DY). These
// position the background and (12px left / 2px up of it) foreground layers so
// both align with the visible playfield.
const BG_SCROLL_DX: i32 = 20;
const BG_SCROLL_DY: i32 = 16;
const FG_SCROLL_DX: i32 = 32;
const FG_SCROLL_DY: i32 = 18;

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

/// Xevious's RAM windows, declared on the shared board's address map.
///
/// Ids start at 4: 0 is the core's unmapped sentinel and 1-3 are the board's
/// per-CPU ROMs ([`namco_galaga::Region`]). The names given here are what the
/// debugger shows against every write and watchpoint hit in these windows.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum Region {
    WorkRam = 4,
    Sr1 = 5,
    Sr2 = 6,
    Sr3 = 7,
    FgColorRam = 8,
    BgColorRam = 9,
    FgVideoRam = 10,
    BgVideoRam = 11,
}

/// Xevious Arcade System (Namco, 1983).
///
/// Screen: 288×224, rotated 90° CCW for a vertical display.
#[derive(Saveable)]
pub struct XeviousSystem {
    pub board: NamcoGalagaBoard,

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
        let mut board = NamcoGalagaBoard::new();
        // All three CPUs share these eight 2KB windows. The SR trio is general
        // work RAM whose top 0x80 bytes are the sprite registers: the sprite
        // hardware reads position from SR1, flip/size/bank from SR2 and
        // tile/colour from SR3 at the same offset in each.
        board
            .map
            .region(
                Region::WorkRam,
                "Work RAM",
                0x7800,
                0x800,
                AccessKind::ReadWrite,
            )
            .region(
                Region::Sr1,
                "Work RAM / Sprite Positions",
                0x8000,
                0x800,
                AccessKind::ReadWrite,
            )
            .region(
                Region::Sr2,
                "Work RAM / Sprite Flip+Size",
                0x9000,
                0x800,
                AccessKind::ReadWrite,
            )
            .region(
                Region::Sr3,
                "Work RAM / Sprite Tile+Color",
                0xA000,
                0x800,
                AccessKind::ReadWrite,
            )
            .region(
                Region::FgColorRam,
                "FG Colour RAM",
                0xB000,
                0x800,
                AccessKind::ReadWrite,
            )
            .region(
                Region::BgColorRam,
                "BG Colour RAM",
                0xB800,
                0x800,
                AccessKind::ReadWrite,
            )
            .region(
                Region::FgVideoRam,
                "FG Video RAM",
                0xC000,
                0x800,
                AccessKind::ReadWrite,
            )
            .region(
                Region::BgVideoRam,
                "BG Video RAM",
                0xC800,
                0x800,
                AccessKind::ReadWrite,
            );

        Self {
            board,
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

        // Xevious drives the 51XX coinage command with a 6-argument quirk;
        // teach the HLE 51XX to expect it so it enters credit mode correctly.
        self.board.enable_xevious_51xx_kludge();

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
    pub fn sub_irq_on(&self) -> bool {
        self.board.sub_irq_enabled
    }
    pub fn sound_nmi_on(&self) -> bool {
        self.board.sound_nmi_enabled
    }

    /// Count of non-zero bytes in the foreground and background video RAM —
    /// a proxy for "the attract screen has been drawn" during boot checks.
    pub fn video_ram_nonzero(&self) -> (usize, usize) {
        let fg = self
            .board
            .map
            .region_data(Region::FgVideoRam)
            .iter()
            .filter(|&&b| b != 0)
            .count();
        let bg = self
            .board
            .map
            .region_data(Region::BgVideoRam)
            .iter()
            .filter(|&&b| b != 0)
            .count();
        (fg, bg)
    }

    /// Render one frame into the native (unrotated) 288×224 index buffer.
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

    /// Layer order matches the hardware: opaque background, then sprites, then
    /// the transparent foreground text on top.
    fn render_video(&mut self) {
        self.render_background();
        self.render_sprites();
        self.render_foreground();
    }

    /// Draw the scrolling background tilemap (bottom, opaque layer). The 64×32
    /// tile map (512×256 px) is scrolled into the 288×224 viewport; each tile's
    /// 9-bit code, 7-bit colour, and flip come from BG video/colour RAM, and the
    /// 2bpp pen is mapped to an indirect palette index through the BG LUT.
    fn render_background(&mut self) {
        // Opaque 64×32 scrolled tilemap (512×256 px) into the 288-wide viewport,
        // via the shared scroll-aware index-writing helper. Pens map through the
        // background LUT; per-tile H/V flip from colour-RAM bits 6/7.
        let sx0 = self.bg_scroll_x as i32 + BG_SCROLL_DX;
        let sy0 = self.bg_scroll_y as i32 + BG_SCROLL_DY;
        let bg_videoram = self.board.map.region_data(Region::BgVideoRam);
        let bg_colorram = self.board.map.region_data(Region::BgColorRam);
        let bg_lut = &self.bg_lut;
        let bg_tile_cache = &self.bg_tile_cache;
        let native = &mut self.native_buffer;
        let mut prio = [0u8; 288];
        for screen_y in 0..224usize {
            let row_off = screen_y * 288;
            let row = &mut native[row_off..row_off + 288];
            gfx::render_scrolled_tilemap_scanline_indexed(
                bg_tile_cache,
                64,
                32,
                8,
                8,
                sx0,
                sy0,
                288,
                screen_y,
                |col, mrow| {
                    let idx = mrow * 64 + col;
                    let code = bg_videoram[idx] as usize;
                    let attr = bg_colorram[idx] as usize;
                    let tile = code + ((attr & 0x01) << 8);
                    let color = ((attr & 0x3c) >> 2) | ((code & 0x80) >> 3) | ((attr & 0x03) << 5);
                    gfx::TileInfo {
                        code: tile as u16,
                        attr: color as u8,
                        flip_x: attr & 0x40 != 0,
                        flip_y: attr & 0x80 != 0,
                    }
                },
                // Opaque: the background LUT gives the indirect palette index.
                |color, pixel| Some((bg_lut[color as usize * 4 + pixel as usize], 0)),
                row,
                &mut prio,
                0,
            );
        }
    }

    /// Draw the foreground text tilemap (top, transparent layer). The 64×32 map
    /// (512×256 px) uses the FG scroll registers; each 8×8 char is 1bpp with pen
    /// 0 transparent. The 6-bit colour code maps directly to an indirect palette
    /// entry (0-63) for the opaque pen — no lookup PROM, unlike the background.
    fn render_foreground(&mut self) {
        // Transparent 64×32 scrolled text tilemap (1bpp, pen 0 transparent) into
        // the 288-wide viewport; the 6-bit colour maps directly to an indirect
        // palette entry. Same scroll-aware helper as the background.
        let sx0 = self.fg_scroll_x as i32 + FG_SCROLL_DX;
        let sy0 = self.fg_scroll_y as i32 + FG_SCROLL_DY;
        let fg_videoram = self.board.map.region_data(Region::FgVideoRam);
        let fg_colorram = self.board.map.region_data(Region::FgColorRam);
        let char_cache = &self.char_cache;
        let native = &mut self.native_buffer;
        let mut prio = [0u8; 288];
        for screen_y in 0..224usize {
            let row_off = screen_y * 288;
            let row = &mut native[row_off..row_off + 288];
            gfx::render_scrolled_tilemap_scanline_indexed(
                char_cache,
                64,
                32,
                8,
                8,
                sx0,
                sy0,
                288,
                screen_y,
                |col, mrow| {
                    let idx = mrow * 64 + col;
                    let code = fg_videoram[idx] as usize;
                    let attr = fg_colorram[idx] as usize;
                    let color = ((attr & 0x03) << 4) | ((attr & 0x3c) >> 2);
                    gfx::TileInfo {
                        code: code as u16,
                        attr: color as u8,
                        flip_x: attr & 0x40 != 0,
                        flip_y: attr & 0x80 != 0,
                    }
                },
                // Pen 0 transparent; the colour is the indirect palette index.
                |color, pixel| (pixel != 0).then_some((color, 0)),
                row,
                &mut prio,
                0,
            );
        }
    }

    /// Draw the sprite layer (middle, between background and foreground). Each
    /// of the 64 sprite slots lives in the top 0x80 bytes of the three SR banks:
    /// SR3 = tile code, SR1 = X/Y position, SR2 = flip/size/bank. Sprites are
    /// 3bpp 16×16 with optional double width/height and H/V flip; pen 0x80 is
    /// transparent. Screen origin: X = xpos - 40 (+256 when the high bit is set),
    /// Y counts up from the bottom of the visible area.
    fn render_sprites(&mut self) {
        for offs in (0..0x80usize).step_by(2) {
            // The sprite hardware reads one register pair from each of the three
            // SR banks at the same offset; copy the six bytes out before calling
            // the (mutably borrowing) blitter below.
            let (sr1, sr2, sr3) = (
                self.board.map.region_data(Region::Sr1),
                self.board.map.region_data(Region::Sr2),
                self.board.map.region_data(Region::Sr3),
            );
            let code_byte = sr3[0x780 + offs];
            let flags = sr3[0x780 + offs + 1];
            if flags & 0x40 != 0 {
                continue; // slot disabled
            }
            let attr = sr2[0x780 + offs];
            let attr_hi = sr2[0x780 + offs + 1];
            let ypos = sr1[0x780 + offs];
            let xpos = sr1[0x780 + offs + 1];

            let base = if attr & 0x80 != 0 {
                (code_byte as u32 & 0x3f) + 0x100
            } else {
                code_byte as u32
            };
            let color = (flags & 0x7f) as usize;
            let flipx = attr & 0x04 != 0;
            let flipy = attr & 0x08 != 0;
            let sx = xpos as i32 - 40 + 0x100 * (attr_hi & 1) as i32;
            let sy = 28 * 8 - ypos as i32 - 1;

            match (attr & 0x02 != 0, attr & 0x01 != 0) {
                (true, true) => {
                    // Double width and height: a 2×2 block of tiles.
                    let c = base & !3;
                    let (l, r) = if flipx { (sx + 16, sx) } else { (sx, sx + 16) };
                    let (t, b) = if flipy { (sy - 16, sy) } else { (sy, sy - 16) };
                    self.draw_sprite_tile(c, color, flipx, flipy, l, b);
                    self.draw_sprite_tile(c + 2, color, flipx, flipy, l, t);
                    self.draw_sprite_tile(c + 1, color, flipx, flipy, r, b);
                    self.draw_sprite_tile(c + 3, color, flipx, flipy, r, t);
                }
                (true, false) => {
                    // Double height: two tiles stacked vertically.
                    let c = base & !2;
                    let x = if flipx { sx + 16 } else { sx };
                    let (t, b) = if flipy { (sy - 16, sy) } else { (sy, sy - 16) };
                    self.draw_sprite_tile(c, color, flipx, flipy, x, b);
                    self.draw_sprite_tile(c + 2, color, flipx, flipy, x, t);
                }
                (false, true) => {
                    // Double width: two tiles side by side.
                    let c = base & !1;
                    let y = if flipy { sy - 16 } else { sy };
                    let (l, r) = if flipx { (sx + 16, sx) } else { (sx, sx + 16) };
                    self.draw_sprite_tile(c, color, flipx, flipy, l, y);
                    self.draw_sprite_tile(c + 1, color, flipx, flipy, r, y);
                }
                (false, false) => {
                    self.draw_sprite_tile(base, color, flipx, flipy, sx, sy);
                }
            }
        }
    }

    /// Blit one 16×16 sprite tile into the native buffer with H/V flip and
    /// per-pixel transparency (indirect pen 0x80). `sx`/`sy` are the top-left
    /// screen coordinates; off-screen pixels are clipped.
    fn draw_sprite_tile(
        &mut self,
        code: u32,
        color: usize,
        flipx: bool,
        flipy: bool,
        sx: i32,
        sy: i32,
    ) {
        let code = code as usize;
        let color_base = (color & 0x3f) * 8;
        // Full-width clip (0..288), no tunnel wrap. 3bpp pens map through the
        // sprite LUT; an indirect pen of 0x80 is transparent. Composited by draw
        // order, so no priority buffer.
        let clip = gfx::SpriteClip {
            x_min: 0,
            x_max: 288,
            wrap_offset: None,
        };
        // Split borrows: closures read the LUT, the helper writes native_buffer.
        let sprite_lut = &self.sprite_lut;
        let sprite_cache = &self.sprite_cache;
        let native = &mut self.native_buffer;
        let is_transparent = |pixel: u8| sprite_lut[color_base + pixel as usize] == 0x80;
        let resolve = |pixel: u8| (sprite_lut[color_base + pixel as usize], 0u8);
        let mut prio = [0u8; 288];

        for py in 0..16i32 {
            let dy = sy + py;
            if !(0..224).contains(&dy) {
                continue;
            }
            let fpy = if flipy { 15 - py } else { py } as usize;
            let row_off = dy as usize * 288;
            let row = &mut native[row_off..row_off + 288];
            gfx::draw_sprite_row_indexed(
                sprite_cache,
                code as u16,
                fpy,
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

/// Label a bus write for the event trace. Only the platform-common latches and
/// the custom-I/O window are shared; Xevious's video/scroll latch sits at
/// 0xD000-0xD07F, and 0xA000-0xBFFF — a video latch and EAROM on other games
/// of this family — is plain sr3 and colour RAM here, so it stays untagged.
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
        0xD000..=0xD07F => WriteAnnotation::device("Video latch"),
        _ => WriteAnnotation::MEMORY,
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
            // RAM windows resolve through the map, which turns the address into
            // the right region and offset (see the Region declarations in new).
            0x7800..=0x7FFF
            | 0x8000..=0x87FF
            | 0x9000..=0x97FF
            | 0xA000..=0xA7FF
            | 0xB000..=0xBFFF
            | 0xC000..=0xCFFF => self.board.map.read_backing(addr),
            0xF000..=0xFFFF => self.read_bg_map(addr),
            _ => 0xFF,
        };
        self.board.watch_read(master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.board
            .watch_write_annotated(master, addr, data, write_annotation(addr));
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
            0x7800..=0x7FFF
            | 0x8000..=0x87FF
            | 0x9000..=0x97FF
            | 0xA000..=0xA7FF
            | 0xB000..=0xBFFF
            | 0xC000..=0xCFFF => self.board.map.write_backing(addr, data),
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
        // Native (unrotated) landscape framebuffer. `namco_galaga::TIMING` reports
        // the pre-swapped portrait size (224×288); Xevious declares ROT90 and lets
        // the frontend rotate centrally, so it reports the native landscape size.
        (288, 224)
    }

    fn display_aspect(&self) -> Option<(u32, u32)> {
        namco_galaga::TIMING.display_aspect()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        // Native RGB24 in row-major order; the ROT90 the cabinet needs is applied
        // centrally by the frontend (see `orientation`), not baked here.
        let mask = self.palette.len() - 1;
        for (i, &idx) in self.native_buffer.iter().enumerate() {
            let (r, g, b) = self.palette[idx as usize & mask];
            buffer[i * 3] = r;
            buffer[i * 3 + 1] = g;
            buffer[i * 3 + 2] = b;
        }
    }

    fn orientation(&self) -> phosphor_core::core::machine::Orientation {
        // Xevious's monitor is mounted rotated 90°; declared, not baked.
        phosphor_core::core::machine::Orientation::ROT90
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
                // Each CPU sees its own ROM here; the board selects by master.
                if cpu_index > 2 {
                    return None;
                }
                Some(self.board.read_rom(BusMaster::Cpu(cpu_index), addr))
            }
            0x7800..=0x7FFF
            | 0x8000..=0x87FF
            | 0x9000..=0x97FF
            | 0xA000..=0xA7FF
            | 0xB000..=0xBFFF
            | 0xC000..=0xCFFF => Some(self.board.map.read_backing(addr)),
            _ => None,
        }
    }

    fn write(&mut self, _cpu_index: usize, addr: u32, data: u8) {
        let Ok(addr) = u16::try_from(addr) else {
            return;
        };
        match addr {
            0x7800..=0x7FFF
            | 0x8000..=0x87FF
            | 0x9000..=0x97FF
            | 0xA000..=0xA7FF
            | 0xB000..=0xBFFF
            | 0xC000..=0xCFFF => self.board.map.write_backing(addr, data),
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
            0x7800..=0x7FFF
            | 0x8000..=0x87FF
            | 0x9000..=0x97FF
            | 0xA000..=0xA7FF
            | 0xB000..=0xBFFF
            | 0xC000..=0xCFFF => self.board.map.poke(addr, data),
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
        self.tick_frame_boundary();
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
        // The render happens in `tick_frame_boundary` on the frame's last cycle
        // (single render site, shared with `debug_tick`).
        for _ in 0..namco_galaga::TIMING.cycles_per_frame() {
            self.tick_frame_boundary();
        }
    }

    fn reset(&mut self) {
        self.board.reset_board();
        for region in [
            Region::WorkRam,
            Region::Sr1,
            Region::Sr2,
            Region::Sr3,
            Region::FgColorRam,
            Region::BgColorRam,
            Region::FgVideoRam,
            Region::BgVideoRam,
        ] {
            self.board.map.region_data_mut(region).fill(0);
        }
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
/// Xevious controls: the shared Galaga-family table plus a second action button
/// per player. Unlike Galaga's single fire button, Xevious has an air-to-air
/// zapper (button 1, via the 51XX) and an air-to-ground blaster/bomb (button 2,
/// wired to the DSWB port).
const XEVIOUS_CONTROLS: [InputControl; namco_galaga::NAMCO_GALAGA_CONTROLS.len() + 2] = {
    let base = namco_galaga::NAMCO_GALAGA_CONTROLS;
    let mut out = [base[0]; namco_galaga::NAMCO_GALAGA_CONTROLS.len() + 2];
    let mut i = 0;
    while i < base.len() {
        out[i] = base[i];
        i += 1;
    }
    out[base.len()] = InputControl {
        id: InputId(namco_galaga::INPUT_P1_BUTTON2 as u16),
        stable_name: "p1_bomb",
        label: "P1 Bomb",
        kind: InputKind::Action(ActionRole::Secondary),
        player: Some(1),
        default_bindings: &[],
    };
    out[base.len() + 1] = InputControl {
        id: InputId(namco_galaga::INPUT_P2_BUTTON2 as u16),
        stable_name: "p2_bomb",
        label: "P2 Bomb",
        kind: InputKind::Action(ActionRole::Secondary),
        player: Some(2),
        default_bindings: &[],
    };
    out
};

impl InputConfigurable for XeviousSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        &XEVIOUS_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        if let InputEvent::Button { id, pressed } = event {
            // The blaster (button 2) is read through the DSWB port, active low:
            // P1 on bit 0, P2 on bit 4. Everything else is 51XX I/O.
            match id.0 as u8 {
                namco_galaga::INPUT_P1_BUTTON2 => {
                    crate::set_bit_active_low(&mut self.board.dswb, 0, pressed)
                }
                namco_galaga::INPUT_P2_BUTTON2 => {
                    crate::set_bit_active_low(&mut self.board.dswb, 4, pressed)
                }
                other => self.board.handle_input(other, pressed),
            }
        }
    }
}
impl phosphor_core::core::machine::Profilable for XeviousSystem {}

// ---------------------------------------------------------------------------
// DIP switches
// ---------------------------------------------------------------------------

/// Xevious DIP banks (DSWA at board byte `dswa`, DSWB at `dswb`). Both banks
/// default to 0xFF at the factory settings. DSWB bits 0 and 4 are not DIP
/// switches — they carry the player 1/2 blaster (button 2) inputs — so no
/// option covers them.
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
            // Bonus-life schedule. On hardware these three bits are interpreted
            // differently when Lives is set to 5 (the 0x60 slice is 0); we model
            // the common table used for the 1/2/3-life settings (which includes
            // the factory default of 3 lives). The DIP bits still reach the game
            // when 5 lives are selected, only the labels here would differ.
            DipOption {
                name: "Bonus Life",
                mask: 0x1C,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "10K, 40K, Every 40K",
                        value: 0x18,
                    },
                    DipChoice {
                        label: "10K, 50K, Every 50K",
                        value: 0x14,
                    },
                    DipChoice {
                        label: "20K, 50K, Every 50K",
                        value: 0x10,
                    },
                    DipChoice {
                        label: "20K, 60K, Every 60K",
                        value: 0x1C,
                    },
                    DipChoice {
                        label: "20K, 70K, Every 70K",
                        value: 0x0C,
                    },
                    DipChoice {
                        label: "20K, 80K, Every 80K",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "20K and 60K Only",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "None",
                        value: 0x00,
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

crate::impl_dip_switches!(XeviousSystem, XEVIOUS_DIP_BANKS, board.dswa, board.dswb);

#[cfg(test)]
mod dip_tests {
    use super::*;
    use phosphor_core::core::machine::DipSwitches;

    // Factory defaults: both banks read all-ones with the switches at their
    // shipped positions and the blaster (button 2) lines released.
    const DSWA_DEFAULT: u8 = 0xFF;
    const DSWB_DEFAULT: u8 = 0xFF;

    fn xevious_at_defaults() -> XeviousSystem {
        let mut sys = XeviousSystem::new();
        sys.set_dip_bank_value(0, DSWA_DEFAULT);
        sys.set_dip_bank_value(1, DSWB_DEFAULT);
        sys
    }

    #[test]
    fn dip_defaults_and_metadata() {
        let sys = xevious_at_defaults();
        crate::assert_dip_banks_valid(sys.dip_banks(), &[DSWA_DEFAULT, DSWB_DEFAULT]);
    }

    #[test]
    fn set_dip_option_masks_only_its_bits() {
        let mut sys = xevious_at_defaults();
        // DSWA Lives is bank 0, option 2 (mask 0x60); pick "1" (0x40).
        sys.set_dip_option(0, 2, 0x40);
        assert_eq!(sys.dip_bank_value(0), 0xDF); // 0xFF with bit 5 cleared
        assert_eq!(sys.dip_bank_value(1), 0xFF); // other bank untouched
    }

    #[test]
    fn dip_change_preserves_blaster_bits() {
        // Button 2 lives on DSWB bits 0/4; changing a DSWB DIP must not disturb
        // a blaster that is currently held.
        let mut sys = xevious_at_defaults();
        sys.board.dswb = DSWB_DEFAULT & !0x01; // P1 blaster held (bit 0 low)
        // DSWB Difficulty is bank 1, option 2 (mask 0x60); pick "Hard" (0x20).
        sys.set_dip_option(1, 2, 0x20);
        assert_eq!(sys.dip_bank_value(1) & 0x60, 0x20); // difficulty applied
        assert_eq!(sys.dip_bank_value(1) & 0x01, 0x00); // blaster bit preserved
    }

    #[test]
    fn dip_bank_values_round_trip() {
        // Mirrors galaga.rs/digdug.rs: a raw bank value written back reads out
        // unchanged, and out-of-range bank indices are ignored.
        let mut sys = XeviousSystem::new();
        sys.set_dip_bank_value(0, 0x3C);
        sys.set_dip_bank_value(1, 0xC3);
        assert_eq!(sys.dip_bank_value(0), 0x3C);
        assert_eq!(sys.dip_bank_value(1), 0xC3);
        sys.set_dip_bank_value(5, 0xFF); // no such bank
        assert_eq!(sys.dip_bank_value(0), 0x3C);
        assert_eq!(sys.dip_bank_value(1), 0xC3);
    }

    #[test]
    fn dsw_port_multiplexes_the_two_banks() {
        // The DIPs are read one bit-pair at a time at 0x6800-0x6807: bit 0 of the
        // returned byte is DSWB[offset], bit 1 is DSWA[offset] (active high).
        let mut sys = XeviousSystem::new();
        sys.board.dswa = 0b0000_0001; // DSWA bit 0 set
        sys.board.dswb = 0b0000_0010; // DSWB bit 1 set
        assert_eq!(sys.dsw_read(0), 0b10); // DSWA bit0 -> result bit1
        assert_eq!(sys.dsw_read(1), 0b01); // DSWB bit1 -> result bit0
        assert_eq!(sys.dsw_read(2), 0b00); // neither bank set here
    }
}

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

crate::register_machine!(XeviousSystem, "xevious", &["xevious"], &XEVIOUS_CONTROLS);

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

    #[test]
    fn background_lookup_and_scroll() {
        let mut sys = XeviousSystem::new();
        // Two tiles: tile 0 has pen 1 at (0,0); tile 1 has pen 2 at (0,0).
        sys.bg_tile_cache = GfxCache::new(2, 8, 8);
        sys.bg_tile_cache.set_pixel(0, 0, 0, 1);
        sys.bg_tile_cache.set_pixel(1, 0, 0, 2);
        // Colour 0: pen 1 -> palette 0x0A, pen 2 -> palette 0x0B.
        sys.bg_lut[1] = 0x0A;
        sys.bg_lut[2] = 0x0B;
        // Tilemap cell (col 1, row 0) selects tile 1.
        sys.board.map.region_data_mut(Region::BgVideoRam)[1] = 1;
        // Cancel the fixed scroll offset so native (x,y) == tilemap (x,y).
        sys.bg_scroll_x = (-BG_SCROLL_DX) as u16;
        sys.bg_scroll_y = (-BG_SCROLL_DY) as u16;
        sys.render_background();
        assert_eq!(sys.native_buffer[0], 0x0A); // native (0,0) -> tile 0
        assert_eq!(sys.native_buffer[8], 0x0B); // native (8,0) -> tile 1
        // Scrolling +8 in X shows tilemap content 8px to the right: tile 1 at (0,0).
        sys.bg_scroll_x = (-BG_SCROLL_DX + 8) as u16;
        sys.render_background();
        assert_eq!(sys.native_buffer[0], 0x0B);
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

    /// A single enabled sprite slot lands at (xpos-40, 223-ypos) with the LUT
    /// pen, and transparent/disabled pixels leave the background untouched.
    #[test]
    fn sprite_render_position_and_transparency() {
        let mut sys = XeviousSystem::new();
        // Tile 5: pen 3 at (0,0); pen 0 (transparent) at (1,0).
        sys.sprite_cache = GfxCache::new(6, 16, 16);
        sys.sprite_cache.set_pixel(5, 0, 0, 3);
        // Colour 2: pen 3 -> palette 0x0C, pen 0 -> transparent (0x80).
        sys.sprite_lut[2 * 8 + 3] = 0x0C;
        sys.sprite_lut[2 * 8] = 0x80;
        // Slot 0: code 5, colour 2, enabled; xpos 140 -> sx 100, ypos 123 -> sy 100.
        let sr3 = sys.board.map.region_data_mut(Region::Sr3);
        sr3[0x780] = 5; // code
        sr3[0x781] = 2; // flags: bit6 clear (enabled), colour = 2
        let sr1 = sys.board.map.region_data_mut(Region::Sr1);
        sr1[0x780] = 123; // ypos
        sr1[0x781] = 140; // xpos
        let sr2 = sys.board.map.region_data_mut(Region::Sr2);
        sr2[0x780] = 0; // no flip / size / bank
        sr2[0x781] = 0; // high X bit clear
        sys.render_sprites();
        assert_eq!(sys.native_buffer[100 * 288 + 100], 0x0C);
        // Neighbour pixel is the transparent pen: background (0) preserved.
        assert_eq!(sys.native_buffer[100 * 288 + 101], 0);

        // A disabled slot (bit 6 set) draws nothing.
        sys.native_buffer.fill(0);
        sys.board.map.region_data_mut(Region::Sr3)[0x781] = 0x40;
        sys.render_sprites();
        assert_eq!(sys.native_buffer[100 * 288 + 100], 0);
    }

    /// The blaster (button 2) is exposed as its own control and drives the
    /// active-low DSWB port bits (P1 = bit 0, P2 = bit 4).
    #[test]
    fn button2_maps_to_dswb_port() {
        let mut sys = XeviousSystem::new();
        sys.board.dswb = 0xFF;
        let press = |sys: &mut XeviousSystem, id: u8, pressed: bool| {
            sys.handle_input(InputEvent::Button {
                id: InputId(id as u16),
                pressed,
            });
        };
        press(&mut sys, namco_galaga::INPUT_P1_BUTTON2, true);
        assert_eq!(sys.board.dswb & 0x01, 0, "P1 blaster clears DSWB bit 0");
        press(&mut sys, namco_galaga::INPUT_P1_BUTTON2, false);
        assert_eq!(sys.board.dswb & 0x01, 0x01, "release restores DSWB bit 0");
        press(&mut sys, namco_galaga::INPUT_P2_BUTTON2, true);
        assert_eq!(sys.board.dswb & 0x10, 0, "P2 blaster clears DSWB bit 4");

        // Both blaster controls are advertised to the frontend.
        let names: Vec<&str> = sys.input_controls().iter().map(|c| c.stable_name).collect();
        assert!(names.contains(&"p1_bomb") && names.contains(&"p2_bomb"));
    }

    /// The 50XX score/protection chip answers through the 06XX at 0x7000/0x7100.
    /// Drive the game's periodic protection check (reset, add 5, read the score)
    /// end-to-end over the bus to prove the chip is fitted and wired, not just
    /// exercised in isolation. Selecting a chip: control bit N = chip N, bit 4 =
    /// read direction; the 50XX is chip 2 (mask 0x04).
    #[test]
    fn namco50_protection_response_over_bus() {
        let mut sys = XeviousSystem::new();
        sys.board.fit_50xx(); // Xevious carries the 50XX; new() leaves it unfitted.

        let send = |sys: &mut XeviousSystem, cmd: u8| {
            Bus::write(sys, BusMaster::Cpu(0), 0x7100, 0x04); // select 50XX, write mode
            Bus::write(sys, BusMaster::Cpu(0), 0x7000, cmd);
        };
        send(&mut sys, 0x10); // reset scores
        send(&mut sys, 0x80); // increment player 1 by 5

        Bus::write(&mut sys, BusMaster::Cpu(0), 0x7100, 0x14); // select 50XX, read mode
        let resp = [
            Bus::read(&mut sys, BusMaster::Cpu(0), 0x7000),
            Bus::read(&mut sys, BusMaster::Cpu(0), 0x7000),
            Bus::read(&mut sys, BusMaster::Cpu(0), 0x7000),
            Bus::read(&mut sys, BusMaster::Cpu(0), 0x7000),
        ];
        // Byte 0 = flags (high-score bit set), bytes 1-3 = BCD score = 000005.
        assert_eq!(resp, [0x80, 0x00, 0x00, 0x05]);
    }

    /// The background-map hardware lookup ("schematic 9B") walks the 2A/2B/2C
    /// data ROMs. With both selector bytes zero the low-nibble path of 2A feeds a
    /// dat1 of 0x005, giving a 2C index of 0x14; even reads return the tile
    /// number (with the 6↔7 bit swap), odd reads return the attribute at +0x800.
    #[test]
    fn bb_r_walks_2a_2b_2c_lookup() {
        let mut sys = XeviousSystem::new();
        let mut rom = vec![0u8; 0x4000];
        // rom2a base 0x0000, rom2b base 0x1000, rom2c base 0x3000.
        rom[0x0000] = 0x00; // 2A[0] low nibble 0 -> dat1 high bits clear
        rom[0x1000] = 0x05; // 2B[0] -> dat1 = 0x005
        rom[0x3000 + 0x14] = 0x40; // 2C tile byte: bit6 set -> swaps to bit7
        rom[0x3000 + 0x814] = 0x34; // 2C attribute byte (BB1 lives at +0x800)
        sys.playfield_rom = rom;
        sys.bs = [0x00, 0x00];

        assert_eq!(sys.read_bg_map(0xF000), 0x80); // even: tile, 0x40 -> 0x80 swap
        assert_eq!(sys.read_bg_map(0xF001), 0x34); // odd: attribute byte
    }

    /// The 0xD000-0xD07F video latch selects a scroll register by address bits
    /// 4-7 and takes the 9th scroll bit from address bit 0; register 7 is the
    /// flip-screen latch.
    #[test]
    fn scroll_latch_decode() {
        let mut sys = XeviousSystem::new();
        sys.write_video_latch(0x00, 0x34); // bg X low
        assert_eq!(sys.bg_scroll_x, 0x034);
        sys.write_video_latch(0x01, 0x34); // bg X + 9th bit
        assert_eq!(sys.bg_scroll_x, 0x134);
        sys.write_video_latch(0x10, 0x12); // fg X
        assert_eq!(sys.fg_scroll_x, 0x012);
        sys.write_video_latch(0x11, 0x00); // fg X = 9th bit only
        assert_eq!(sys.fg_scroll_x, 0x100);
        sys.write_video_latch(0x20, 0x56); // bg Y
        assert_eq!(sys.bg_scroll_y, 0x056);
        sys.write_video_latch(0x30, 0x78); // fg Y
        assert_eq!(sys.fg_scroll_y, 0x078);
        sys.write_video_latch(0x70, 0x01); // flip on
        assert!(sys.board.flip_screen);
        sys.write_video_latch(0x70, 0x00); // flip off
        assert!(!sys.board.flip_screen);
    }

    #[test]
    fn declares_native_dims_and_rot90() {
        use phosphor_core::core::machine::{Orientation, Renderable};
        let sys = XeviousSystem::new();
        // Native landscape framebuffer; the frontend applies ROT90 to present
        // portrait.
        assert_eq!(sys.display_size(), (288, 224));
        assert_eq!(sys.orientation(), Orientation::ROT90);
        assert!(sys.orientation().swaps_axes());
        assert_eq!(sys.display_aspect(), Some((3, 4)));
    }

    #[test]
    fn render_frame_emits_native_unrotated_rgb() {
        use phosphor_core::core::machine::Renderable;
        let mut sys = XeviousSystem::new();
        // Tag a native pixel + palette entry; confirm it lands at the native
        // row-major position (288 wide), i.e. no baked rotation.
        sys.palette[7] = (11, 22, 33);
        let (nx, ny) = (9usize, 3usize);
        sys.native_buffer[ny * 288 + nx] = 7;
        let mut buf = vec![0u8; 288 * 224 * 3];
        sys.render_frame(&mut buf);
        let i = (ny * 288 + nx) * 3;
        assert_eq!(&buf[i..i + 3], &[11, 22, 33]);
    }
}
