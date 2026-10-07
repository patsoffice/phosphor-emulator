//! Time Pilot (Konami, 1982).
//!
//! # Schematics
//!
//! | Drawing | Source | Pages |
//! |---|---|---|
//! | C.P.U. BOARD KT-5207-1B | `arcade-museum.com/manuals-videogames/T/timepilot-cpu.pdf` | whole file, read 2026-10-06 |
//! | SOUND BOARD KT-5112-2B | `arcade-museum.com/manuals-videogames/T/timepilot-sound.pdf` | whole file, read 2026-10-06 |
//! | `TimePilot` manual, DIP settings | `arcade-museum.com/manuals-videogames/T/TimePilot.pdf` | DIP page, read 2026-10-06 |
//!
//! Transcribed to `docs/schematics/timepilot-cpu.md` and
//! `docs/schematics/timepilot-sound.md`. Address decode and video sequencing
//! live in Konami customs (K526, 082/083, 824-502/503) whose truth tables are
//! not on the drawings; where the sheets go dark the MAME driver
//! (`konami/timeplt.cpp`, `shared/timeplt_a.cpp`) is the fallback.
//!
//! A main Z80 drives a 32x32 tilemap plus 16x16 sprites through a PROM
//! palette, with the Time Pilot sound board ([`crate::timepilot_sound`]: a
//! second Z80 + 2xAY-8910) fed by an LS273 command latch.
//!
//! Memory map (MAME `main_map`):
//! ```text
//!   0x0000-0x5fff  Program ROM (24 KB)
//!   0xa000-0xa3ff  Color RAM (tile attributes)
//!   0xa400-0xa7ff  Video RAM (tile codes)
//!   0xa800-0xafff  Work RAM
//!   0xb000/0xb400  Sprite RAM banks (256 bytes each, mirrored by 0x0b00)
//!   0xc000 (r) scanline  (w) sound-data strobe
//!   0xc200 (r) DSW1  (w) watchdog reset
//!   0xc300 (r) IN0  (w) LS259 latch at 0xc300-0xc30f, line = (addr >> 1) & 7
//!   0xc320 IN1  0xc340 IN2  0xc360 DSW0
//! ```
//!
//! LS259 latch (B3) bit assignments (MAME `mainlatch`):
//! ```text
//!   Q0 NMI enable  Q1 flip screen (inverted)  Q2 sound IRQ trigger  Q3 mute
//!   Q4 video enable  Q5/Q6 coin counters (not modeled)  Q7 unused
//! ```
//!
//! Frame timing is not on the readable part of the sheet (the H/V chains run
//! through the customs), so it is taken from Gyruss: same 18.432 MHz crystal,
//! same 16-239 visible window, one year apart. 396 dots/line at the /3 pixel
//! clock is 198 CPU cycles/line over 256 lines (60.6 Hz).

use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::DebugTraceBuffer;
use phosphor_core::core::machine::{
    ActionRole, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, Direction, InputConfigurable,
    InputControl, InputEvent, InputId, InputKind, MachineCore, Nvram, Profilable, SaveState,
};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{
    Bus, BusMaster, ClockDomainName as Clk, ClockTree, DomainId, TimingConfig,
};
use phosphor_core::cpu::z80::Z80;
use phosphor_core::cpu::{Cpu, CpuStateTrait};
use phosphor_core::gfx::decode::{GfxCache, GfxLayout, decode_gfx};
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion, Saveable};

use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::timepilot_sound::TimePilotSound;

pub fn sample_rate() -> u32 {
    phosphor_core::audio::host_sample_rate() as u32
}

/// Main CPU: 18.432 MHz / 6 = 3.072 MHz. 198 cycles/line x 256 lines =
/// 50688 cycles/frame (60.6 Hz); visible rows are lines 16-239.
pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 3_072_000,
    cycles_per_scanline: 198,
    total_scanlines: 256,
    // Native (pre-orientation) framebuffer: the board declares ROT90 (plus any
    // cocktail flip) and the frontend rotates centrally, so these are the
    // unrotated dimensions.
    display_width: NATIVE_WIDTH as u32,   // 256
    display_height: NATIVE_HEIGHT as u32, // 224
    display_aspect: Some((3, 4)),         // portrait tube as viewed (after ROT90)
};

/// The board's crystals and everything divided out of them.
///
/// Two: 18.432 MHz on the main board (Z80 at /6, pixel clock at /3) and a
/// 14.318181 MHz colorburst crystal on the sound board, with its Z80 at /8.
pub fn clock_tree() -> ClockTree {
    use phosphor_core::core::RootId;
    let mut t = ClockTree::new(18_432_000);
    let snd = t.add_root(14_318_181);
    let cpu = t.add_domain(Clk::Cpu, RootId::MAIN, 1, 6); // 3.072 MHz
    let dot = t.add_domain(Clk::Pixel, RootId::MAIN, 1, 3); // 6.144 MHz
    t.add_domain(Clk::SoundCpu, snd, 1, 8); // 1.789772 MHz
    t.set_step_domain(cpu);
    // 396 dot clocks per line is exactly 198 CPU cycles.
    t.set_raster(dot, 396, 0);
    t
}

/// Native (pre-orientation) framebuffer dimensions: the visible 256x224
/// window of the 256-line raster.
pub const NATIVE_WIDTH: usize = 256;
pub const NATIVE_HEIGHT: usize = 224;
/// First visible raster line (MAME visarea y 16-239).
const VISIBLE_Y_OFFSET: usize = 16;
/// Raster line where vblank (and the NMI) starts.
const VBLANK_LINE: u64 = 240;

/// LS259 latch (B3) output bits.
const LATCH_NMI_ENABLE: u8 = 0x01;
const LATCH_FLIP: u8 = 0x02; // inverted: Q1 low flips the screen
const LATCH_SOUND_IRQ: u8 = 0x04;
const LATCH_MUTE: u8 = 0x08;
const LATCH_VIDEO_ENABLE: u8 = 0x10;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum Region {
    Rom = 1,
    ColorRam = 2,
    VideoRam = 3,
    Ram = 4,
    SpriteRam0 = 5,
    SpriteRam1 = 6,
}

// Input button IDs.
pub const INPUT_COIN1: u8 = 0;
pub const INPUT_COIN2: u8 = 1;
pub const INPUT_SERVICE: u8 = 2;
pub const INPUT_P1_START: u8 = 3;
pub const INPUT_P2_START: u8 = 4;
pub const INPUT_P1_LEFT: u8 = 5;
pub const INPUT_P1_RIGHT: u8 = 6;
pub const INPUT_P1_UP: u8 = 7;
pub const INPUT_P1_DOWN: u8 = 8;
pub const INPUT_P1_FIRE: u8 = 9;
pub const INPUT_P2_LEFT: u8 = 10;
pub const INPUT_P2_RIGHT: u8 = 11;
pub const INPUT_P2_UP: u8 = 12;
pub const INPUT_P2_DOWN: u8 = 13;
pub const INPUT_P2_FIRE: u8 = 14;

// ---------------------------------------------------------------------------
// Video
// ---------------------------------------------------------------------------

/// Weights of the 5-bit-per-gun resistor DAC (MAME `palette`, matching the
/// 390/470/560/820/1.2K ladders on the sheet, MSB first).
const DAC_WEIGHTS: [u16; 5] = [0x19, 0x24, 0x35, 0x40, 0x4d];

// ---------------------------------------------------------------------------
// GFX layouts (MAME `charlayout`/`spritelayout`, verbatim except planes:
// `decode_gfx` numbers planes LSB-first, so MAME's `{ 4, 0 }` is `[0, 4]`.
// Bits within a byte are MSB-first, matching MAME `readbit`.)
// ---------------------------------------------------------------------------

/// 8x8 2bpp chars (tm6, 512 codes). Each row's pixels 0-3 live in byte `y`
/// and pixels 4-7 in byte `y + 8`.
pub static TIMEPILOT_CHAR_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 4],
    x_offsets: &[0, 1, 2, 3, 64, 65, 66, 67],
    y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
    char_increment: 16 * 8,
};

/// 16x16 2bpp sprites (tm4+tm5, 256 codes). Four 4-pixel groups per row at
/// byte offsets 0/8/16/24; rows 8-15 sit in the second 32 bytes.
pub static TIMEPILOT_SPRITE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 4],
    x_offsets: &[
        0, 1, 2, 3, 64, 65, 66, 67, 128, 129, 130, 131, 192, 193, 194, 195,
    ],
    y_offsets: &[
        0, 8, 16, 24, 32, 40, 48, 56, 256, 264, 272, 280, 288, 296, 304, 312,
    ],
    char_increment: 64 * 8,
};

/// Time Pilot video: 32x32 tilemap, 16x16 sprites, PROM palette.
///
/// Tiles render in two categories around the sprites (category 0 under,
/// category 1 over); both layers resolve 2bpp pixels through lookup PROMs
/// into 32 palette entries. Rendering is per-scanline from live RAM: row `r`
/// samples the video state as it stands at raster line `r + 16`.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct TimePilotVideo {
    /// Decoded 8x8 chars (tm6, 512 codes). ROM-derived, never written after
    /// load, so not saved.
    #[save_skip]
    tile_cache: GfxCache,
    /// Decoded 16x16 sprites (tm4+tm5, 256 codes). Likewise not saved.
    #[save_skip]
    sprite_cache: GfxCache,
    /// 128 char pens through the e12 lookup PROM. ROM-derived, not saved.
    #[save_skip]
    char_pens: [(u8, u8, u8); 128],
    /// 256 sprite pens through the e9 lookup PROM. ROM-derived, not saved.
    #[save_skip]
    sprite_pens: [(u8, u8, u8); 256],
    /// Native-orientation scanline framebuffer. Rebuilt every frame, not saved.
    #[save_skip]
    scanline_buffer: Vec<u8>,

    /// Cocktail flip (latch Q1, inverted). Declared via `orientation` and
    /// applied centrally by the frontend; rendering stays unmirrored.
    #[save(id = 1)]
    flip: bool,
    /// Video enable (latch Q4). Clear blanks the raster to black.
    #[save(id = 2)]
    video_enable: bool,
}

impl TimePilotVideo {
    pub fn new() -> Self {
        Self {
            tile_cache: GfxCache::new(512, 8, 8),
            sprite_cache: GfxCache::new(256, 16, 16),
            char_pens: [(0, 0, 0); 128],
            sprite_pens: [(0, 0, 0); 256],
            scanline_buffer: vec![0u8; NATIVE_WIDTH * NATIVE_HEIGHT * 3],
            flip: false,
            video_enable: false,
        }
    }

    /// Decode tm6 (8 KB, 512 codes) with [`TIMEPILOT_CHAR_LAYOUT`].
    pub fn load_tile_rom(&mut self, data: &[u8]) {
        self.tile_cache = decode_gfx(data, 0, 512, &TIMEPILOT_CHAR_LAYOUT);
    }

    /// Decode tm4+tm5 (16 KB, 256 codes) with [`TIMEPILOT_SPRITE_LAYOUT`].
    pub fn load_sprite_rom(&mut self, data: &[u8]) {
        self.sprite_cache = decode_gfx(data, 0, 256, &TIMEPILOT_SPRITE_LAYOUT);
    }

    /// Build the 128 char + 256 sprite pens from the 0x240-byte PROM region:
    /// 32 palette entries from b4/b5, sprites through the e9 lookup, chars
    /// through e12 with the +0x10 offset (MAME `palette`).
    pub fn load_proms(&mut self, prom: &[u8]) {
        let bit = |b: u8, n: u8| u16::from((b >> n) & 1);
        let mut val = [(0u8, 0u8, 0u8); 32];
        for (i, slot) in val.iter_mut().enumerate() {
            let b4 = prom[i];
            let b5 = prom[32 + i];
            let r = DAC_WEIGHTS[0] * bit(b5, 1)
                + DAC_WEIGHTS[1] * bit(b5, 2)
                + DAC_WEIGHTS[2] * bit(b5, 3)
                + DAC_WEIGHTS[3] * bit(b5, 4)
                + DAC_WEIGHTS[4] * bit(b5, 5);
            let g = DAC_WEIGHTS[0] * bit(b5, 6)
                + DAC_WEIGHTS[1] * bit(b5, 7)
                + DAC_WEIGHTS[2] * bit(b4, 0)
                + DAC_WEIGHTS[3] * bit(b4, 1)
                + DAC_WEIGHTS[4] * bit(b4, 2);
            let b = DAC_WEIGHTS[0] * bit(b4, 3)
                + DAC_WEIGHTS[1] * bit(b4, 4)
                + DAC_WEIGHTS[2] * bit(b4, 5)
                + DAC_WEIGHTS[3] * bit(b4, 6)
                + DAC_WEIGHTS[4] * bit(b4, 7);
            *slot = (r as u8, g as u8, b as u8);
        }
        for (i, pen) in self.sprite_pens.iter_mut().enumerate() {
            *pen = val[(prom[0x40 + i] & 0x0f) as usize];
        }
        for (i, pen) in self.char_pens.iter_mut().enumerate() {
            *pen = val[((prom[0x140 + i] & 0x0f) + 0x10) as usize];
        }
    }

    pub fn set_flip(&mut self, flip: bool) {
        self.flip = flip;
    }

    pub fn set_video_enable(&mut self, enable: bool) {
        self.video_enable = enable;
    }

    pub(crate) fn tile_cache(&self) -> &GfxCache {
        &self.tile_cache
    }

    pub(crate) fn sprite_cache(&self) -> &GfxCache {
        &self.sprite_cache
    }

    pub(crate) fn char_palette(&self) -> &[(u8, u8, u8)] {
        &self.char_pens
    }

    pub(crate) fn sprite_palette(&self) -> &[(u8, u8, u8)] {
        &self.sprite_pens
    }

    /// Render one visible scanline (`row` in 0..224) into the framebuffer:
    /// black, category-0 tiles, sprites, category-1 tiles.
    pub fn render_scanline(
        &mut self,
        row: usize,
        colorram: &[u8],
        videoram: &[u8],
        spriteram0: &[u8],
        spriteram1: &[u8],
    ) {
        debug_assert!(row < NATIVE_HEIGHT);
        let mame_y = (row + VISIBLE_Y_OFFSET) as i32;
        let row_off = row * NATIVE_WIDTH * 3;
        let buf = &mut self.scanline_buffer[row_off..row_off + NATIVE_WIDTH * 3];
        buf.fill(0);
        if !self.video_enable {
            return;
        }
        self.draw_tiles_row(row_off, mame_y, colorram, videoram, 0);
        self.draw_sprites_row(row_off, mame_y, spriteram0, spriteram1);
        self.draw_tiles_row(row_off, mame_y, colorram, videoram, 1);
    }

    /// One row of the tilemap for a single category: 32 columns, tile-outer,
    /// one `row_slice` per tile. Attribute: bit 5 extends the code past 0xFF,
    /// bits 0-4 are the color, bit 4 the category, bits 6-7 flip Y/X.
    ///
    /// Tiles are opaque: pen 0 paints (MAME maps every pen to LAYER0 unless
    /// the driver sets a transparent pen, and timeplt never does). The sky is
    /// pen-0-dominant tiles, so skipping pen 0 turns it black.
    fn draw_tiles_row(
        &mut self,
        row_off: usize,
        mame_y: i32,
        colorram: &[u8],
        videoram: &[u8],
        category: u8,
    ) {
        let tile_row = (mame_y >> 3) as usize;
        let py_raw = (mame_y & 7) as usize;
        for col in 0..32 {
            let idx = tile_row * 32 + col;
            let attr = colorram[idx];
            if (attr & 0x10) >> 4 != category {
                continue;
            }
            let code = videoram[idx] as usize + ((attr & 0x20) as usize) * 8;
            let color = (attr & 0x1f) as usize;
            let py = if attr & 0x80 != 0 { 7 - py_raw } else { py_raw };
            let slice = self.tile_cache.row_slice(code, py);
            let base_x = col * 8;
            for px in 0..8 {
                let sx = if attr & 0x40 != 0 { 7 - px } else { px };
                let pv = slice[sx];
                let (r, g, b) = self.char_pens[color * 4 + pv as usize];
                let off = row_off + (base_x + px) * 3;
                self.scanline_buffer[off] = r;
                self.scanline_buffer[off + 1] = g;
                self.scanline_buffer[off + 2] = b;
            }
        }
    }

    /// One row of sprites, slots 0x3E down to 0x10 so lower-numbered sprites
    /// win (drawn last, on top). `sy = 241 - y`, flip X is inverted.
    fn draw_sprites_row(
        &mut self,
        row_off: usize,
        mame_y: i32,
        spriteram0: &[u8],
        spriteram1: &[u8],
    ) {
        for slot in (0..24).rev() {
            let offs = 0x10 + slot * 2;
            let sy = 241 - i32::from(spriteram1[offs + 1]);
            let dy = mame_y - sy;
            if !(0..16).contains(&dy) {
                continue;
            }
            let attr = spriteram1[offs];
            let py = if attr & 0x80 != 0 {
                (15 - dy) as usize
            } else {
                dy as usize
            };
            let code = spriteram0[offs + 1] as usize;
            let color = (attr & 0x3f) as usize;
            let flipx = attr & 0x40 == 0;
            let slice = self.sprite_cache.row_slice(code, py);
            let sx = i32::from(spriteram0[offs]);
            for i in 0..16 {
                let x = sx + i;
                if x < 0 || x >= NATIVE_WIDTH as i32 {
                    continue;
                }
                let px = if flipx { 15 - i as usize } else { i as usize };
                let pv = slice[px];
                if pv != 0 {
                    let (r, g, b) = self.sprite_pens[color * 4 + pv as usize];
                    let off = row_off + (x as usize) * 3;
                    self.scanline_buffer[off] = r;
                    self.scanline_buffer[off + 1] = g;
                    self.scanline_buffer[off + 2] = b;
                }
            }
        }
    }

    /// Copy the finished buffer out. The cocktail flip is declared via
    /// [`orientation`](Self::orientation) and applied centrally by the
    /// frontend, so this emits pixels unmirrored.
    pub fn render_frame(&self, out: &mut [u8]) {
        out.copy_from_slice(&self.scanline_buffer);
    }

    /// Declarative screen orientation: base ROT90 composed with the live
    /// cocktail flip.
    ///
    /// The cabinet tube is mounted rotated 90° clockwise (MAME `ROT90`). The
    /// cocktail flip mirrors both native axes (180°), so flip set composes to
    /// `ROT270`.
    pub fn orientation(&self) -> phosphor_core::core::machine::Orientation {
        use phosphor_core::core::machine::Orientation;
        let mut o = Orientation::ROT90;
        if self.flip {
            o = o.compose(Orientation::COCKTAIL);
        }
        o
    }

    /// Reset dynamic state (not the ROM-derived caches, pens or pixels).
    pub fn reset(&mut self) {
        self.flip = false;
        self.video_enable = false;
        self.scanline_buffer.fill(0);
    }
}

impl Default for TimePilotVideo {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// TimePilotBoard
// ---------------------------------------------------------------------------

/// One CPU cycle: the video/NMI work for this clock, the Z80, then the sound
/// board on its Bresenham divider.
///
/// The CPU lives on the machine and the board *is* the bus, so this takes them
/// as separate borrows and dispatches at a concrete type.
#[inline]
pub fn tick(cpu: &mut Z80, board: &mut TimePilotBoard) {
    board.begin_cycle();
    cpu.execute_cycle(board, BusMaster::Cpu(0));
    board.end_cycle();
}

/// Run one frame's worth of CPU cycles.
pub fn run_frame(cpu: &mut Z80, board: &mut TimePilotBoard) {
    for _ in 0..TIMING.cycles_per_frame() {
        tick(cpu, board);
    }
}

/// Time Pilot board: main Z80 bus, video, sound board, inputs and DIPs.
#[derive(BusDebug, DebugTrace, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct TimePilotBoard {
    /// The address space persists its own writable regions: color RAM, video
    /// RAM, work RAM and both sprite RAM banks here.
    #[debug_map(cpu = 0)]
    #[save(id = 1)]
    pub(crate) map: AddressSpace16,

    #[save(id = 2)]
    pub(crate) video: TimePilotVideo,

    /// Both attributes, for two different views of the same board: the device
    /// entry is its latches (`COMMAND`, `IRQ`, `MUTE`, `FILTER`), and
    /// `#[debug_bus]` merges the tree it derives for itself, which is how its
    /// Z80 becomes CPU 1 and its address space becomes CPU 1's.
    #[debug_device("Time Pilot Sound")]
    #[debug_bus]
    #[save(id = 3)]
    pub(crate) sound: TimePilotSound,

    // Input ports (active-low: 0xFF = nothing pressed).
    #[save(id = 4)]
    pub(crate) in0: u8,
    #[save(id = 5)]
    pub(crate) in1: u8,
    #[save(id = 6)]
    pub(crate) in2: u8,
    // DIP switches on their own ports (unlike the Scramble family, where DIP
    // bits share the input ports).
    #[save(id = 7)]
    pub(crate) dsw0: u8,
    #[save(id = 8)]
    pub(crate) dsw1: u8,

    /// LS259 latch (B3) output byte.
    #[save(id = 9)]
    pub(crate) latch: u8,
    #[save(id = 10)]
    pub(crate) vblank_nmi_pending: bool,

    #[save(id = 11)]
    pub(crate) clock: u64,
    /// The board's clock tree, as [`clock_tree`] declares it. Only the sound
    /// domain is stepped; the rest is the derivation it rides on.
    #[debug_device("Clocks")]
    #[save(id = 12)]
    clocks: ClockTree,
    #[save_skip]
    sound_dom: DomainId,
    /// Kept out of the save, as it was before: the watchdog is a countdown to a
    /// reset, and a load restarting it is the safer of the two.
    #[save_skip]
    watchdog_counter: u32,

    #[debug_events]
    #[save_skip]
    pub(crate) debug_trace: DebugTraceBuffer,
}

impl TimePilotBoard {
    pub fn new() -> Self {
        let clocks = clock_tree();
        let sound_dom = clocks.find(Clk::SoundCpu).expect("declared sound domain");
        // One derivation reaches all three places the sound rate is used: the
        // ratio this board steps the sound section at, the AY-8910s' chip
        // clock, and the resampler's input rate.
        let sound_hz = clocks.hz(sound_dom);
        Self {
            map: Self::build_map(),
            video: TimePilotVideo::new(),
            sound: TimePilotSound::new(sound_hz),
            in0: 0xFF,
            in1: 0xFF,
            in2: 0xFF,
            dsw0: 0xFF,
            dsw1: 0xFF,
            latch: 0,
            vblank_nmi_pending: false,
            clock: 0,
            clocks,
            sound_dom,
            watchdog_counter: 0,
            debug_trace: DebugTraceBuffer::new(),
        }
    }

    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(
            Region::Rom,
            "Program ROM",
            0x0000,
            0x6000,
            AccessKind::ReadOnly,
        )
        .region(
            Region::ColorRam,
            "Color RAM",
            0xa000,
            0x0400,
            AccessKind::ReadWrite,
        )
        .region(
            Region::VideoRam,
            "Video RAM",
            0xa400,
            0x0400,
            AccessKind::ReadWrite,
        )
        .region(
            Region::Ram,
            "Work RAM",
            0xa800,
            0x0800,
            AccessKind::ReadWrite,
        )
        .region(
            Region::SpriteRam0,
            "Sprite RAM bank 0",
            0xb000,
            0x0100,
            AccessKind::ReadWrite,
        )
        .region(
            Region::SpriteRam1,
            "Sprite RAM bank 1",
            0xb400,
            0x0100,
            AccessKind::ReadWrite,
        );
        // Sprite RAM mirrors (MAME `mirror(0x0b00)`): address bits 8, 9 and 11
        // are don't-care, bit 10 selects the bank.
        for m in [0x100, 0x200, 0x300, 0x800, 0x900, 0xa00, 0xb00] {
            map.mirror(0xb000 + m, 0xb000, 0x0100);
            map.mirror(0xb400 + m, 0xb400, 0x0100);
        }
        map
    }

    pub fn load_program_rom(&mut self, data: &[u8]) {
        self.map.load_region(Region::Rom, data);
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
    pub fn load_sound_rom(&mut self, data: &[u8]) {
        self.sound.load_rom(data);
    }

    pub fn clock(&self) -> u64 {
        self.clock
    }

    /// Current raster line (0-255), which is what the 0xC000 scanline read
    /// returns.
    pub fn scanline(&self) -> u8 {
        ((self.clock % TIMING.cycles_per_frame()) / TIMING.cycles_per_scanline) as u8
    }

    // -----------------------------------------------------------------------
    // Core tick
    // -----------------------------------------------------------------------

    /// Board work that leads a CPU cycle: the per-scanline render and the
    /// VBLANK NMI edge.
    fn begin_cycle(&mut self) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();

        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let line = frame_cycle / TIMING.cycles_per_scanline;
            if (VISIBLE_Y_OFFSET as u64..VBLANK_LINE).contains(&line) {
                let row = (line as usize) - VISIBLE_Y_OFFSET;
                let colorram = self.map.region_data(Region::ColorRam);
                let videoram = self.map.region_data(Region::VideoRam);
                let spr0 = self.map.region_data(Region::SpriteRam0);
                let spr1 = self.map.region_data(Region::SpriteRam1);
                self.video
                    .render_scanline(row, colorram, videoram, spr0, spr1);
            }
        }

        if frame_cycle == VBLANK_LINE * TIMING.cycles_per_scanline {
            self.vblank_nmi_pending = true;
        }
        if frame_cycle == 0 && self.clock > 0 {
            self.vblank_nmi_pending = false;
        }
    }

    /// Board work after the CPU's cycle: the sound board, the clock, and the
    /// watchdog counter.
    fn end_cycle(&mut self) {
        // Tick the slower sound board in proportion.
        if self.clocks.tick(self.sound_dom) {
            self.sound.tick();
        }

        self.clock += 1;
        self.watchdog_counter += 1;
    }

    /// The interrupt lines this board drives. Named to avoid shadowing
    /// [`Bus::check_interrupts`], which the board also implements.
    pub fn interrupt_state(&self, target: BusMaster) -> InterruptState {
        let mut state = InterruptState::default();
        if let BusMaster::Cpu(0) = target {
            state.nmi = self.vblank_nmi_pending && (self.latch & LATCH_NMI_ENABLE != 0);
        }
        state
    }

    pub fn render_frame(&self, buffer: &mut [u8]) {
        self.video.render_frame(buffer);
    }

    /// Declarative orientation (the live cocktail flip); applied centrally by
    /// the frontend.
    pub fn orientation(&self) -> phosphor_core::core::machine::Orientation {
        self.video.orientation()
    }

    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize {
        self.sound.fill_audio(out)
    }

    pub fn reset_board(&mut self) {
        self.video.reset();
        phosphor_core::device::Device::reset(&mut self.sound);
        self.latch = 0;
        self.vblank_nmi_pending = false;
        self.clock = 0;
        self.clocks.reset();
        self.watchdog_counter = 0;
        self.map.region_data_mut(Region::Ram).fill(0);
        self.map.region_data_mut(Region::ColorRam).fill(0);
        self.map.region_data_mut(Region::VideoRam).fill(0);
        self.map.region_data_mut(Region::SpriteRam0).fill(0);
        self.map.region_data_mut(Region::SpriteRam1).fill(0);
    }

    /// Which CPUs are between instructions, in `cpus()` order: the main Z80 in
    /// bit 0, the sound board's Z80 in bit 1.
    ///
    /// The main CPU lives on the machine, which passes it back in; the sound
    /// CPU is this board's own.
    pub fn instruction_boundaries(&self, cpu: &Z80) -> u32 {
        u32::from(cpu.at_instruction_boundary())
            | (u32::from(self.sound.at_instruction_boundary()) << 1)
    }

    // -----------------------------------------------------------------------
    // Bus dispatch
    // -----------------------------------------------------------------------

    pub fn bus_read_common(&mut self, addr: u16) -> u8 {
        let data = self.main_read(addr);
        self.map.watch_read(0, BusMaster::Cpu(0), addr, data);
        data
    }

    fn main_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x5fff => self.map.read_backing(addr),
            0xa000..=0xafff => self.map.read_backing(addr),
            0xb000..=0xbfff => self.map.read_backing(addr),
            0xc000..=0xffff => self.io_read(addr),
            _ => 0xff,
        }
    }

    pub fn bus_write_common(&mut self, addr: u16, data: u8) {
        self.map.watch_write(0, BusMaster::Cpu(0), addr, data);
        match addr {
            0xa000..=0xafff => {
                self.map.write_backing(addr, data);
            }
            0xb000..=0xbfff => self.map.write_backing(addr, data),
            0xc000..=0xffff => self.io_write(addr, data),
            _ => {}
        }
    }

    /// I/O page decode. Address bits 10-11 are don't-care (MAME mirrors
    /// 0x0c00/0x0c9f/0x0cff); within 0xC3xx, bits 4-6 select the port.
    fn io_read(&mut self, addr: u16) -> u8 {
        let work = addr & 0xf3ff;
        match work & 0xff00 {
            0xc000 => self.scanline(),
            0xc200 => self.dsw1,
            0xc300 => match (work & 0x70) >> 4 {
                0 => self.in0,
                2 => self.in1,
                4 => self.in2,
                6 => self.dsw0,
                _ => 0xff,
            },
            _ => 0xff,
        }
    }

    fn io_write(&mut self, addr: u16, data: u8) {
        let work = addr & 0xf3ff;
        match work & 0xff00 {
            0xc000 => self.sound.write_command(data),
            0xc200 => self.watchdog_counter = 0,
            0xc300 if (work & 0x70) >> 4 == 0 => self.latch_write(work, data),
            _ => {}
        }
    }

    /// LS259 at 0xC300-0xC30F: the latch line is `(addr >> 1) & 7`
    /// (MAME `write_d0(offset >> 1, data)`) and the latched bit is D0.
    fn latch_write(&mut self, addr: u16, data: u8) {
        let line = (addr >> 1) & 7;
        let bit = data & 1 != 0;
        let old = self.latch;
        if bit {
            self.latch |= 1 << line;
        } else {
            self.latch &= !(1 << line);
        }
        match line {
            0 => {
                if !bit {
                    self.vblank_nmi_pending = false;
                }
            }
            1 => self.video.set_flip(self.latch & LATCH_FLIP == 0), // inverted output
            2 => {
                if bit && old & LATCH_SOUND_IRQ == 0 {
                    self.sound.pulse_irq();
                }
            }
            3 => self.sound.set_mute(self.latch & LATCH_MUTE != 0),
            4 => self
                .video
                .set_video_enable(self.latch & LATCH_VIDEO_ENABLE != 0),
            _ => {} // 5, 6 = coin counters (not modeled), 7 unused
        }
    }
}

impl Default for TimePilotBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for TimePilotBoard {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        self.bus_read_common(addr)
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        self.bus_write_common(addr, data);
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        self.interrupt_state(target)
    }
}

// ---------------------------------------------------------------------------
// ROM definitions (parent "timeplt" set + shared regions)
// ---------------------------------------------------------------------------

pub static TIMEPILOT_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        RomEntry {
            name: "tm1",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x1551f1b9],
        },
        RomEntry {
            name: "tm2",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x58636cb5],
        },
        RomEntry {
            name: "tm3",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0xff4e0d83],
        },
    ],
};

pub static TIMEPILOTC_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        RomEntry {
            name: "cd1y",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x83ec72c2],
        },
        RomEntry {
            name: "cd2y",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x0dcf5287],
        },
        RomEntry {
            name: "cd3y",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0xc789b912],
        },
    ],
};

pub static TIMEPILOTA_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        RomEntry {
            name: "cd_e1.bin",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xa4513b35],
        },
        RomEntry {
            name: "cd_e2.bin",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x38b0c72a],
        },
        RomEntry {
            name: "cd_e3.bin",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x83846870],
        },
    ],
};

/// One loader per revision, paired by position with the `Revision` list in
/// the `register_machine!` call below. Only the program ROMs differ between
/// revisions; sound, tiles, sprites and PROMs are shared.
pub struct TimePilotRomConfig {
    pub set: &'static str,
    pub program: &'static RomRegion,
}

const ALL_CONFIGS: &[&TimePilotRomConfig] = &[
    &TimePilotRomConfig {
        set: "timeplt",
        program: &TIMEPILOT_PROGRAM_ROM,
    },
    &TimePilotRomConfig {
        set: "timepltc",
        program: &TIMEPILOTC_PROGRAM_ROM,
    },
    &TimePilotRomConfig {
        set: "timeplta",
        program: &TIMEPILOTA_PROGRAM_ROM,
    },
];

pub static TIMEPILOT_SOUND_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "tm7",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0xd66da813],
    }],
};

pub static TIMEPILOT_TILE_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[RomEntry {
        name: "tm6",
        size: 0x2000,
        offset: 0x0000,
        crc32: &[0xc2507f40],
    }],
};

pub static TIMEPILOT_SPRITE_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "tm4",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x7e437c3e],
        },
        RomEntry {
            name: "tm5",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0xe8ca87b9],
        },
    ],
};

pub static TIMEPILOT_PROM: RomRegion = RomRegion {
    size: 0x0240,
    entries: &[
        RomEntry {
            name: "timeplt.b4",
            size: 0x0020,
            offset: 0x0000,
            crc32: &[0x34c91839],
        },
        RomEntry {
            name: "timeplt.b5",
            size: 0x0020,
            offset: 0x0020,
            crc32: &[0x463b2b07],
        },
        RomEntry {
            name: "timeplt.e9",
            size: 0x0100,
            offset: 0x0040,
            crc32: &[0x4bbb2150],
        },
        RomEntry {
            name: "timeplt.e12",
            size: 0x0100,
            offset: 0x0140,
            crc32: &[0xf7b7663e],
        },
    ],
};

// ---------------------------------------------------------------------------
// DIP switches (MAME `timeplt`: DSW0 = coinage, DSW1 = play options)
// ---------------------------------------------------------------------------

pub(crate) const TIMEPILOT_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSW0",
        options: &[
            DipOption {
                name: "Coin A",
                mask: 0x0f,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "4C/1C",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "3C/1C",
                        value: 0x05,
                    },
                    DipChoice {
                        label: "2C/1C",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "3C/2C",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "4C/3C",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "1C/1C",
                        value: 0x0f,
                    },
                    DipChoice {
                        label: "3C/4C",
                        value: 0x03,
                    },
                    DipChoice {
                        label: "2C/3C",
                        value: 0x07,
                    },
                    DipChoice {
                        label: "1C/2C",
                        value: 0x0e,
                    },
                    DipChoice {
                        label: "2C/5C",
                        value: 0x06,
                    },
                    DipChoice {
                        label: "1C/3C",
                        value: 0x0d,
                    },
                    DipChoice {
                        label: "1C/4C",
                        value: 0x0c,
                    },
                    DipChoice {
                        label: "1C/5C",
                        value: 0x0b,
                    },
                    DipChoice {
                        label: "1C/6C",
                        value: 0x0a,
                    },
                    DipChoice {
                        label: "1C/7C",
                        value: 0x09,
                    },
                    DipChoice {
                        label: "Free Play",
                        value: 0x00,
                    },
                ],
                conditional: &[],
            },
            DipOption {
                name: "Coin B",
                mask: 0xf0,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "4C/1C",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "3C/1C",
                        value: 0x50,
                    },
                    DipChoice {
                        label: "2C/1C",
                        value: 0x80,
                    },
                    DipChoice {
                        label: "3C/2C",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "4C/3C",
                        value: 0x10,
                    },
                    DipChoice {
                        label: "1C/1C",
                        value: 0xf0,
                    },
                    DipChoice {
                        label: "3C/4C",
                        value: 0x30,
                    },
                    DipChoice {
                        label: "2C/3C",
                        value: 0x70,
                    },
                    DipChoice {
                        label: "1C/2C",
                        value: 0xe0,
                    },
                    DipChoice {
                        label: "2C/5C",
                        value: 0x60,
                    },
                    DipChoice {
                        label: "1C/3C",
                        value: 0xd0,
                    },
                    DipChoice {
                        label: "1C/4C",
                        value: 0xc0,
                    },
                    DipChoice {
                        label: "1C/5C",
                        value: 0xb0,
                    },
                    DipChoice {
                        label: "1C/6C",
                        value: 0xa0,
                    },
                    DipChoice {
                        label: "1C/7C",
                        value: 0x90,
                    },
                    DipChoice {
                        label: "Free Play",
                        value: 0x00,
                    },
                ],
                conditional: &[],
            },
        ],
    },
    DipSwitchBank {
        name: "DSW1",
        options: &[
            DipOption {
                name: "Lives",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "3",
                        value: 0x03,
                    },
                    DipChoice {
                        label: "4",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "5",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "255 (Cheat)",
                        value: 0x00,
                    },
                ],
                conditional: &[],
            },
            DipOption {
                name: "Cabinet",
                mask: 0x04,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Upright",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Cocktail",
                        value: 0x04,
                    },
                ],
                conditional: &[],
            },
            DipOption {
                name: "Bonus Life",
                mask: 0x08,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "10000 50000",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "20000 60000",
                        value: 0x00,
                    },
                ],
                conditional: &[],
            },
            DipOption {
                name: "Difficulty",
                mask: 0x70,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "1 (Easiest)",
                        value: 0x70,
                    },
                    DipChoice {
                        label: "2",
                        value: 0x60,
                    },
                    DipChoice {
                        label: "3",
                        value: 0x50,
                    },
                    DipChoice {
                        label: "4",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "5",
                        value: 0x30,
                    },
                    DipChoice {
                        label: "6",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "7",
                        value: 0x10,
                    },
                    DipChoice {
                        label: "8 (Difficult)",
                        value: 0x00,
                    },
                ],
                conditional: &[],
            },
            DipOption {
                name: "Demo Sounds",
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
        ],
    },
];

/// Factory-default DIPs: 1C/1C both coins; 3 lives, upright, bonus at
/// 10000/50000, difficulty 4, demo sounds on.
const DEFAULT_DSW0: u8 = 0xff;
const DEFAULT_DSW1: u8 = 0x4b;

// ---------------------------------------------------------------------------
// Input controls
// ---------------------------------------------------------------------------

pub const TIMEPILOT_CONTROLS: &[InputControl] = &[
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
        id: InputId(INPUT_P1_FIRE as u16),
        stable_name: "p1_fire",
        label: "P1 Fire",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
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
        id: InputId(INPUT_P2_FIRE as u16),
        stable_name: "p2_fire",
        label: "P2 Fire",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(2),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_COIN1 as u16),
        stable_name: "coin1",
        label: "Coin 1 (Left)",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(INPUT_COIN2 as u16),
        stable_name: "coin2",
        label: "Coin 2 (Right)",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_SERVICE as u16),
        stable_name: "service",
        label: "Service",
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
// TimePilotSystem wrapper
// ---------------------------------------------------------------------------

/// Time Pilot (Konami, 1982).
///
/// Same split as the other Z80 machines: the CPU sits beside the board, which
/// *is* the bus, so each cycle dispatches at a concrete type.
#[derive(phosphor_macros::Saveable, phosphor_macros::BusDebug)]
pub struct TimePilotSystem {
    #[debug_cpu("Z80")]
    pub(crate) cpu: Z80,
    #[debug_bus]
    pub board: TimePilotBoard,
    /// MAME set name this machine loaded (e.g., "timepltc"), reported by
    /// `MachineCore::revision`. Loading a save restores state, never ROMs,
    /// so this keeps its value across loads.
    #[save_skip]
    loaded_revision: &'static str,
}

impl TimePilotSystem {
    pub fn new() -> Self {
        let mut board = TimePilotBoard::new();
        board.dsw0 = DEFAULT_DSW0;
        board.dsw1 = DEFAULT_DSW1;
        Self {
            cpu: Z80::new(),
            board,
            loaded_revision: "",
        }
    }

    pub fn load_roms(
        &mut self,
        rom_set: &RomSet,
        config: &TimePilotRomConfig,
    ) -> Result<(), RomLoadError> {
        // Recorded before the first ROM read so a blank-set load still
        // carries the attempted revision. `create` builds a fresh instance
        // per attempt, so a failed attempt cannot poison a later success.
        self.loaded_revision = config.set;
        self.board.load_program_rom(&config.program.load(rom_set)?);
        self.board
            .load_sound_rom(&TIMEPILOT_SOUND_ROM.load(rom_set)?);
        self.board.load_tile_rom(&TIMEPILOT_TILE_ROM.load(rom_set)?);
        self.board
            .load_sprite_rom(&TIMEPILOT_SPRITE_ROM.load(rom_set)?);
        self.board.load_proms(&TIMEPILOT_PROM.load(rom_set)?);
        Ok(())
    }

    /// Time Pilot input bit mapping (active-low; pressing clears the bit).
    fn apply_input(&mut self, button: u8, pressed: bool) {
        let b = &mut self.board;
        match button {
            INPUT_COIN1 => crate::set_bit_active_low(&mut b.in0, 0, pressed),
            INPUT_COIN2 => crate::set_bit_active_low(&mut b.in0, 1, pressed),
            INPUT_SERVICE => crate::set_bit_active_low(&mut b.in0, 2, pressed),
            INPUT_P1_START => crate::set_bit_active_low(&mut b.in0, 3, pressed),
            INPUT_P2_START => crate::set_bit_active_low(&mut b.in0, 4, pressed),
            INPUT_P1_LEFT => crate::set_bit_active_low(&mut b.in1, 0, pressed),
            INPUT_P1_RIGHT => crate::set_bit_active_low(&mut b.in1, 1, pressed),
            INPUT_P1_UP => crate::set_bit_active_low(&mut b.in1, 2, pressed),
            INPUT_P1_DOWN => crate::set_bit_active_low(&mut b.in1, 3, pressed),
            INPUT_P1_FIRE => crate::set_bit_active_low(&mut b.in1, 4, pressed),
            INPUT_P2_LEFT => crate::set_bit_active_low(&mut b.in2, 0, pressed),
            INPUT_P2_RIGHT => crate::set_bit_active_low(&mut b.in2, 1, pressed),
            INPUT_P2_UP => crate::set_bit_active_low(&mut b.in2, 2, pressed),
            INPUT_P2_DOWN => crate::set_bit_active_low(&mut b.in2, 3, pressed),
            INPUT_P2_FIRE => crate::set_bit_active_low(&mut b.in2, 4, pressed),
            _ => {}
        }
    }

    /// Snapshot of the Z80's registers, for tests and the debugger.
    pub fn get_cpu_state(&self) -> phosphor_core::cpu::state::Z80State {
        self.cpu.snapshot()
    }

    /// Advance one CPU cycle, returning the instruction-boundary mask.
    pub fn step_cycle(&mut self) -> u32 {
        tick(&mut self.cpu, &mut self.board);
        self.board.instruction_boundaries(&self.cpu)
    }

    /// Read the CPU-facing bus, side effects and all. Distinct from the
    /// debugger's `BusDebug::peek`/`poke`, which avoid side effects.
    pub fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        self.board.read(master, addr)
    }

    /// Write the CPU-facing bus, side effects and all. See [`Self::bus_read`].
    pub fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.board.write(master, addr, data);
    }
}

impl Default for TimePilotSystem {
    fn default() -> Self {
        Self::new()
    }
}

// The board is the bus -- see `impl Bus for TimePilotBoard` above.
crate::impl_board_delegation!(TimePilotSystem, board, TIMING, orientation);

impl MachineCore for TimePilotSystem {
    crate::machine_core_metadata!("timepilot", TIMING, crate::timepilot::clock_tree);

    fn revision(&self) -> &str {
        self.loaded_revision
    }

    fn gfx_sheets(&self) -> Vec<phosphor_core::core::machine::GfxSheet<'_>> {
        use phosphor_core::core::machine::GfxSheet;
        let v = &self.board.video;
        vec![
            GfxSheet {
                name: "chars",
                cache: v.tile_cache(),
                palette: v.char_palette(),
            },
            GfxSheet {
                name: "sprites",
                cache: v.sprite_cache(),
                palette: v.sprite_palette(),
            },
        ]
    }

    fn run_frame(&mut self) {
        run_frame(&mut self.cpu, &mut self.board);
    }

    fn reset(&mut self) {
        self.board.reset_board();
        self.cpu.reset(&mut self.board, BusMaster::Cpu(0));
    }
}

impl SaveState for TimePilotSystem {
    crate::machine_save_state!();
}

impl Nvram for TimePilotSystem {}
impl Profilable for TimePilotSystem {}

impl InputConfigurable for TimePilotSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        TIMEPILOT_CONTROLS
    }
    fn handle_input(&mut self, event: InputEvent) {
        if let InputEvent::Button { id, pressed } = event {
            self.apply_input(id.0 as u8, pressed);
        }
    }
}

crate::impl_dip_switches!(TimePilotSystem, TIMEPILOT_DIP_BANKS, board.dsw0, board.dsw1);

crate::impl_board_debug_trace!(TimePilotSystem, board);

crate::register_machine!(
    TimePilotSystem,
    "timepilot",
    &[
        crate::registry::Revision {
            names: &["timeplt"],
            nvram_group: None
        },
        crate::registry::Revision {
            names: &["timepltc"],
            nvram_group: None
        },
        crate::registry::Revision {
            names: &["timeplta"],
            nvram_group: None
        },
    ],
    TIMEPILOT_CONTROLS,
    configs = ALL_CONFIGS
);

inventory::submit! {
    DisasmRegion {
        machine: "timepilot",
        region: "main",
        cpu: DisasmCpu::Z80,
        org: 0,
        size: TIMEPILOT_PROGRAM_ROM.size as u32,
        load: |rs| TIMEPILOT_PROGRAM_ROM.load(rs),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "timepilot",
        region: "sound",
        cpu: DisasmCpu::Z80,
        org: 0,
        size: TIMEPILOT_SOUND_ROM.size as u32,
        load: |rs| TIMEPILOT_SOUND_ROM.load(rs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    crate::dip_test_suite!(TimePilotSystem, &[DEFAULT_DSW0, DEFAULT_DSW1]);

    #[test]
    fn machine_id_and_defaults() {
        let sys = TimePilotSystem::new();
        assert_eq!(sys.machine_id(), "timepilot");
        assert_eq!(sys.board.dsw0, DEFAULT_DSW0);
        assert_eq!(sys.board.dsw1, DEFAULT_DSW1);
        assert_eq!(sys.board.latch, 0);
    }

    #[test]
    fn active_low_inputs_land_on_their_ports() {
        let mut sys = TimePilotSystem::new();
        sys.apply_input(INPUT_COIN1, true);
        assert_eq!(sys.board.in0 & 0x01, 0x00, "coin1 clears IN0 bit 0");
        sys.apply_input(INPUT_P1_LEFT, true);
        assert_eq!(sys.board.in1 & 0x01, 0x00, "left clears IN1 bit 0");
        sys.apply_input(INPUT_P1_FIRE, true);
        assert_eq!(sys.board.in1 & 0x10, 0x00, "fire clears IN1 bit 4");
        sys.apply_input(INPUT_P2_DOWN, true);
        assert_eq!(sys.board.in2 & 0x08, 0x00, "P2 down clears IN2 bit 3");
        sys.apply_input(INPUT_P1_LEFT, false);
        assert_eq!(sys.board.in1 & 0x01, 0x01, "release restores the bit");
    }

    #[test]
    fn ram_and_vram_round_trip() {
        let mut b = TimePilotBoard::new();
        b.bus_write_common(0xa800, 0xab);
        assert_eq!(b.bus_read_common(0xa800), 0xab);
        b.bus_write_common(0xa000, 0x42);
        assert_eq!(b.bus_read_common(0xa000), 0x42);
        b.bus_write_common(0xa400, 0x17);
        assert_eq!(b.bus_read_common(0xa400), 0x17);
    }

    #[test]
    fn sprite_ram_banks_mirror_by_bit_8_9_11() {
        let mut b = TimePilotBoard::new();
        b.bus_write_common(0xb000, 0x11);
        b.bus_write_common(0xb400, 0x22);
        for addr in [
            0xb000, 0xb100, 0xb200, 0xb300, 0xb800, 0xb900, 0xba00, 0xbb00,
        ] {
            assert_eq!(b.bus_read_common(addr), 0x11, "bank 0 at {addr:#06x}");
        }
        for addr in [
            0xb400, 0xb500, 0xb600, 0xb700, 0xbc00, 0xbd00, 0xbe00, 0xbf00,
        ] {
            assert_eq!(b.bus_read_common(addr), 0x22, "bank 1 at {addr:#06x}");
        }
    }

    #[test]
    fn io_ports_decode() {
        let mut b = TimePilotBoard::new();
        b.in0 = 0xA0;
        b.in1 = 0xB1;
        b.in2 = 0xC2;
        b.dsw0 = 0xD3;
        b.dsw1 = 0xE4;
        assert_eq!(b.bus_read_common(0xc300), 0xA0);
        assert_eq!(b.bus_read_common(0xc320), 0xB1);
        assert_eq!(b.bus_read_common(0xc340), 0xC2);
        assert_eq!(b.bus_read_common(0xc360), 0xD3);
        assert_eq!(b.bus_read_common(0xc200), 0xE4);
        // Mirror bits 10-11 are don't-care.
        assert_eq!(b.bus_read_common(0xcf60), 0xD3, "DSW0 mirror");
    }

    #[test]
    fn scanline_read_tracks_the_raster() {
        let mut b = TimePilotBoard::new();
        b.clock = 100 * TIMING.cycles_per_scanline;
        assert_eq!(b.bus_read_common(0xc000), 100);
        b.clock = 255 * TIMING.cycles_per_scanline;
        assert_eq!(b.bus_read_common(0xc000), 255);
    }

    #[test]
    fn latch_lines_drive_their_outputs() {
        let mut b = TimePilotBoard::new();
        // Q0 enables the NMI; clearing it drops a pending edge.
        b.bus_write_common(0xc300, 1);
        assert_eq!(b.latch & LATCH_NMI_ENABLE, LATCH_NMI_ENABLE);
        b.vblank_nmi_pending = true;
        b.bus_write_common(0xc300, 0);
        assert!(!b.vblank_nmi_pending);
        // Q1 is inverted: writing 0 flips the screen.
        b.bus_write_common(0xc302, 0);
        assert!(b.video.flip);
        b.bus_write_common(0xc302, 1);
        assert!(!b.video.flip);
        // Q4 enables video.
        b.bus_write_common(0xc308, 1);
        assert!(b.video.video_enable);
        // Q3 mutes the sound board.
        b.bus_write_common(0xc306, 1);
        let regs = phosphor_core::core::debug::Debuggable::debug_registers(&b.sound);
        assert_eq!(regs.iter().find(|r| r.name == "MUTE").unwrap().value, 1);
    }

    #[test]
    fn sound_irq_fires_on_the_rising_edge_only() {
        let mut b = TimePilotBoard::new();
        let irq = |b: &mut TimePilotBoard| b.sound.check_interrupts_for_test(BusMaster::Cpu(0)).irq;
        b.bus_write_common(0xc304, 1);
        assert!(irq(&mut b), "rising edge pulses the IRQ");
        // Acknowledge by reading the latch, then hold the line high: no edge.
        b.sound.acknowledge_for_test();
        b.bus_write_common(0xc304, 1);
        assert!(!irq(&mut b), "no edge while the line stays high");
        // Falling then rising pulses again.
        b.bus_write_common(0xc304, 0);
        b.bus_write_common(0xc304, 1);
        assert!(irq(&mut b), "second rising edge pulses again");
    }

    #[test]
    fn vblank_sets_and_frame_wrap_clears_the_nmi() {
        let mut sys = TimePilotSystem::new();
        sys.bus_write(BusMaster::Cpu(0), 0xc300, 1); // NMI enable
        sys.board.clock = VBLANK_LINE * TIMING.cycles_per_scanline;
        sys.step_cycle();
        assert!(sys.board.vblank_nmi_pending);
        let state = sys.board.interrupt_state(BusMaster::Cpu(0));
        assert!(state.nmi);
        // Step to the frame wrap: 16 more lines.
        for _ in 0..16 * TIMING.cycles_per_scanline {
            sys.step_cycle();
        }
        assert!(!sys.board.vblank_nmi_pending);
    }

    #[test]
    fn char_decode_matches_mame_msb_first() {
        let mut v = TimePilotVideo::new();
        let mut rom = vec![0u8; 0x2000];
        // Code 0, row 0: 0x6A reads MSB-first as pixels 2, 1, 3, 0.
        rom[0] = 0x6a;
        v.load_tile_rom(&rom);
        assert_eq!(v.tile_cache.pixel(0, 0, 0), 2);
        assert_eq!(v.tile_cache.pixel(0, 1, 0), 1);
        assert_eq!(v.tile_cache.pixel(0, 2, 0), 3);
        assert_eq!(v.tile_cache.pixel(0, 3, 0), 0);
        // Row 0's px 4-7 come from byte 8.
        rom[8] = 0x6a;
        v.load_tile_rom(&rom);
        assert_eq!(v.tile_cache.pixel(0, 4, 0), 2);
    }

    #[test]
    fn sprite_decode_matches_mame_msb_first() {
        let mut v = TimePilotVideo::new();
        let mut rom = vec![0u8; 0x4000];
        // Code 0, row 0, px 0: MSB plane = bit 3 of byte 0.
        rom[0] = 0x08;
        // Row 8 lives 32 bytes in: px 0 LSB plane = bit 7 of byte 32.
        rom[32] = 0x80;
        v.load_sprite_rom(&rom);
        assert_eq!(v.sprite_cache.pixel(0, 0, 0), 2);
        assert_eq!(v.sprite_cache.pixel(0, 0, 8), 1);
    }

    #[test]
    fn palette_build_matches_the_mame_weights() {
        let mut v = TimePilotVideo::new();
        let mut prom = vec![0u8; 0x240];
        // Entry 0: all five red bits set (b5 bits 1-5) -> full red.
        prom[32] = 0x3e;
        // Sprite pen 0 and char pen 0 both index entry 0.
        prom[0x40] = 0x00;
        prom[0x140] = 0xf0; // low nibble 0 -> +0x10 = entry 16 (black)
        v.load_proms(&prom);
        assert_eq!(v.sprite_pens[0], (255, 0, 0));
        assert_eq!(v.char_pens[0], (0, 0, 0));
    }

    /// A mid-frame color RAM write splits the picture at the stated row: row 0
    /// renders with the old attribute, row 1 with the new one.
    #[test]
    fn mid_frame_color_write_splits_the_picture() {
        let mut sys = TimePilotSystem::new();
        sys.bus_write(BusMaster::Cpu(0), 0xc308, 1); // video enable
        // Tile (row 2, col 0) covers raster lines 16-23 = rows 0-7. Code 0's
        // row-0 px 0 is pixel value 2 (see char decode test).
        let mut tile = vec![0u8; 0x2000];
        tile[0] = 0x08;
        tile[1] = 0x08;
        sys.board.load_tile_rom(&tile);
        let mut prom = vec![0u8; 0x240];
        prom[0x140 + 2] = 0x00; // char color 0 pen 2 -> entry 16
        prom[0x140 + 6] = 0x01; // char color 1 pen 2 -> entry 17
        prom[32 + 16] = 0x3e; // entry 16 full red (b5 bits 1-5)
        prom[17] = 0x07; // entry 17 full green (b4 bits 0-2 ...
        prom[32 + 17] = 0xc0; // ... plus b5 bits 6-7)
        sys.board.load_proms(&prom);
        sys.bus_write(BusMaster::Cpu(0), 0xa400 + 2 * 32, 0); // video
        sys.bus_write(BusMaster::Cpu(0), 0xa000 + 2 * 32, 0x00); // color 0
        let (colorram, videoram, spr0, spr1) = (
            sys.board.map.region_data(Region::ColorRam).to_vec(),
            sys.board.map.region_data(Region::VideoRam).to_vec(),
            sys.board.map.region_data(Region::SpriteRam0).to_vec(),
            sys.board.map.region_data(Region::SpriteRam1).to_vec(),
        );
        sys.board
            .video
            .render_scanline(0, &colorram, &videoram, &spr0, &spr1);
        sys.bus_write(BusMaster::Cpu(0), 0xa000 + 2 * 32, 0x01); // color 1
        let colorram = sys.board.map.region_data(Region::ColorRam).to_vec();
        sys.board
            .video
            .render_scanline(1, &colorram, &videoram, &spr0, &spr1);
        let buf = |row: usize| {
            let off = row * NATIVE_WIDTH * 3;
            (
                sys.board.video.scanline_buffer[off],
                sys.board.video.scanline_buffer[off + 1],
                sys.board.video.scanline_buffer[off + 2],
            )
        };
        assert_eq!(buf(0), (255, 0, 0), "row 0 keeps the old color");
        assert_eq!(buf(1), (0, 255, 0), "row 1 takes the new color");
    }

    /// The frame loop reaches the scanline hook: a programmed tile appears in
    /// the buffer after `run_frame`.
    #[test]
    fn frame_loop_reaches_the_scanline_hook() {
        let mut sys = TimePilotSystem::new();
        sys.bus_write(BusMaster::Cpu(0), 0xc308, 1); // video enable
        let mut tile = vec![0u8; 0x2000];
        tile[0] = 0x08;
        sys.board.load_tile_rom(&tile);
        let mut prom = vec![0u8; 0x240];
        prom[32 + 16] = 0x3e; // entry 16 full red
        sys.board.load_proms(&prom);
        sys.bus_write(BusMaster::Cpu(0), 0xa400 + 2 * 32, 0);
        sys.bus_write(BusMaster::Cpu(0), 0xa000 + 2 * 32, 0x00);
        sys.run_frame();
        let mut out = vec![0u8; NATIVE_WIDTH * NATIVE_HEIGHT * 3];
        sys.board.render_frame(&mut out);
        assert_eq!((out[0], out[1], out[2]), (255, 0, 0));
    }

    /// Tile pen 0 paints rather than showing black: 0x6A reads as pixels
    /// 2, 1, 3, 0, so px 3 takes char color 0 pen 0 (entry 16, red here).
    /// The sky is pen-0-dominant tiles; skipping pen 0 turns it black.
    #[test]
    fn tile_pen_zero_paints_the_lookup_color() {
        let mut sys = TimePilotSystem::new();
        sys.bus_write(BusMaster::Cpu(0), 0xc308, 1); // video enable
        let mut tile = vec![0u8; 0x2000];
        tile[0] = 0x6a;
        sys.board.load_tile_rom(&tile);
        let mut prom = vec![0u8; 0x240];
        prom[0x140] = 0x00; // char color 0 pen 0 -> entry 16
        prom[0x140 + 2] = 0x01; // char color 0 pen 2 -> entry 17
        prom[32 + 16] = 0x3e; // entry 16 full red (b5 bits 1-5)
        prom[17] = 0x07; // entry 17 full green (b4 bits 0-2 ...
        prom[32 + 17] = 0xc0; // ... plus b5 bits 6-7)
        sys.board.load_proms(&prom);
        sys.bus_write(BusMaster::Cpu(0), 0xa400 + 2 * 32, 0); // video
        sys.bus_write(BusMaster::Cpu(0), 0xa000 + 2 * 32, 0x00); // color 0
        let (colorram, videoram, spr0, spr1) = (
            sys.board.map.region_data(Region::ColorRam).to_vec(),
            sys.board.map.region_data(Region::VideoRam).to_vec(),
            sys.board.map.region_data(Region::SpriteRam0).to_vec(),
            sys.board.map.region_data(Region::SpriteRam1).to_vec(),
        );
        sys.board
            .video
            .render_scanline(0, &colorram, &videoram, &spr0, &spr1);
        let px = |x: usize| {
            (
                sys.board.video.scanline_buffer[x * 3],
                sys.board.video.scanline_buffer[x * 3 + 1],
                sys.board.video.scanline_buffer[x * 3 + 2],
            )
        };
        assert_eq!(px(0), (0, 255, 0), "pen 2 paints entry 17");
        assert_eq!(px(3), (255, 0, 0), "pen 0 paints entry 16");
    }
}
