//! Gyruss (Konami, 1983).
//!
//! # Schematics
//!
//! | Drawing | Source | Pages |
//! |---|---|---|
//! | COMPUTER LOGIC BOARD - GYRUSS (Centuri 010-4623) | `arcade-museum.com/manuals-videogames/G/Gyruss__1983__Konami.pdf` | manual p27, read 2026-10-07 |
//! | GYRUSS SOUND BOARD (Konami sheet) | same manual | manual p28, read 2026-10-07 |
//! | DIP settings | same manual (Centuri book) | manual pp5-6, read 2026-10-07 |
//!
//! Transcribed to `docs/schematics/gyruss-cpu.md` and
//! `docs/schematics/gyruss-sound.md`. Address decode and video sequencing
//! live in Konami customs (501, 503, 083) whose truth tables are not on the
//! drawings; where the sheets go dark the MAME driver (`konami/gyruss.cpp`)
//! is the fallback.
//!
//! Four CPUs: a main Z80 driving a 32x32 tilemap, a Konami-1 (6809 with
//! scrambled opcodes) cooking the sprite list into sprite RAM, and the sound
//! board ([`crate::gyruss_sound`]: audio Z80 + 5xAY-8910 + 8039 SFX MCU).
//!
//! Memory map, main Z80 (MAME `main_cpu1_map`):
//! ```text
//!   0x0000-0x5fff  Program ROM (24 KB; 0x6000-0x7fff empty diagnostics socket)
//!   0x8000-0x83ff  Color RAM (tile attributes)
//!   0x8400-0x87ff  Video RAM (tile codes)
//!   0x9000-0x9fff  Work RAM
//!   0xa000-0xa7ff  Shared RAM (sub CPU sees it at 0x6000-0x67ff)
//!   0xc000 (r) DSW2  (w) watchdog reset (not modeled)
//!   0xc080 (r) SYSTEM  (w) audio IRQ trigger
//!   0xc0a0 P1  0xc0c0 P2  0xc0e0 DSW1
//!   0xc100 (r) DSW3  (w) sound latch 1
//!   0xc180-0xc187  LS259 latch (line = addr & 7, bit = data & 1)
//! ```
//!
//! Memory map, sub CPU (MAME `main_cpu2_map`):
//! ```text
//!   0x0000 (r) scanline  0x2000 (w) IRQ mask bit 0
//!   0x4000-0x47ff  RAM (sprite list at 0x4040-0x40ff)
//!   0x6000-0x67ff  Shared RAM (main CPU's 0xa000-0xa7ff)
//!   0xe000-0xffff  Program ROM (Konami-1 cipher text)
//! ```
//!
//! LS259 latch (3C) bit assignments (MAME `mainlatch`):
//! ```text
//!   Q0 NMI enable  Q1/Q4/Q6/Q7 unwired  Q2/Q3 coin counters (not modeled)
//!   Q5 flip screen
//! ```
//!
//! Frame timing is taken from MAME (PCB-measured H = 15.50 kHz, V = 60.56
//! Hz): 396 dots/line at the /3 pixel clock is 198 CPU cycles/line over 256
//! lines. Time Pilot runs the same numbers off the same crystal.

use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::DebugTraceBuffer;
use phosphor_core::core::machine::{
    ActionRole, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, Direction, InputConfigurable,
    InputControl, InputEvent, InputId, InputKind, MachineCore, Nvram, Orientation, Profilable,
    SaveState,
};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{
    Bus, BusMaster, ClockDomainName as Clk, ClockTree, DomainId, TimingConfig,
};
use phosphor_core::cpu::m6809::M6809;
use phosphor_core::cpu::z80::Z80;
use phosphor_core::cpu::{Cpu, CpuStateTrait};
use phosphor_core::gfx::decode::{GfxCache, GfxLayout, decode_gfx};
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion, Saveable};

use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::gyruss_sound::GyrussSound;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};

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
/// Three: 18.432 MHz on the logic board (main Z80 at /6, sub at /12, pixel
/// clock at /3), a 14.318181 MHz colorburst crystal on the sound board (audio
/// Z80 at /4, AYs at /8), and an 8 MHz crystal for the 8039.
///
/// Only the sub domain is stepped from this tree: the audio clock outruns the
/// main clock (3.58 vs 3.07 MHz) and the tree cannot step a domain faster
/// than its step domain, so the audio section runs on an exact-ratio
/// Bresenham accumulator instead (see `GyrussBoard::acc_audio`). The tree is
/// still the one derivation every rate is read from.
pub fn clock_tree() -> ClockTree {
    use phosphor_core::core::RootId;
    let mut t = ClockTree::new(18_432_000);
    let snd = t.add_root(14_318_181);
    let mcu = t.add_root(8_000_000);
    let cpu = t.add_domain(Clk::Cpu, RootId::MAIN, 1, 6); // 3.072 MHz
    let dot = t.add_domain(Clk::Pixel, RootId::MAIN, 1, 3); // 6.144 MHz
    t.add_domain(Clk::SubCpu, RootId::MAIN, 1, 12); // 1.536 MHz
    t.add_domain(Clk::SoundCpu, snd, 1, 4); // 3.579545 MHz
    t.add_domain(Clk::Psg, snd, 1, 8); // 1.789772 MHz
    t.add_domain(Clk::Mcu, mcu, 1, 15); // 8039 machine cycles, 533.3 kHz
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
/// Raster line where vblank (and the NMI/IRQ) starts.
const VBLANK_LINE: u64 = 240;

/// LS259 latch (3C) output bits.
const LATCH_NMI_ENABLE: u8 = 0x01;
const LATCH_FLIP: u8 = 0x20;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum Region {
    MainRom = 1,
    ColorRam = 2,
    VideoRam = 3,
    WorkRam = 4,
    /// Main 0xA000-0xA7FF = sub 0x6000-0x67FF. One backing; the sub view is
    /// translated, never mirrored (a mirror would also show it to the main
    /// CPU at 0x6000, where the diagnostics socket reads open bus).
    SharedRam = 5,
    /// Sub RAM at its translated address (sub 0x4000-0x47FF); see `build_map`.
    SubRam = 6,
    /// Sub ROM at its translated address (sub 0xE000-0xFFFF); see `build_map`.
    SubRom = 7,
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
// GFX layouts (MAME `charlayout`/`spritelayout`, verbatim except planes:
// `decode_gfx` numbers planes LSB-first, so MAME's MSB-first lists are
// reversed. Bits within a byte are MSB-first, matching MAME `readbit`.)
// ---------------------------------------------------------------------------

/// 8x8 2bpp chars (tiles ROM, 512 codes). Same layout as Time Pilot's: each
/// row's pixels 0-3 live in byte `y` and pixels 4-7 in byte `y + 8`.
pub static GYRUSS_CHAR_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 4],
    x_offsets: &[0, 1, 2, 3, 64, 65, 66, 67],
    y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
    char_increment: 16 * 8,
};

/// 8x16 4bpp sprites (sprite ROMs, 256 codes per bank). Planes 0/1 come from
/// the low 16 KB, planes 2/3 from 0x4000 past them (the upper two ROMs);
/// rows 8-15 sit 32 bytes into the 64-byte code. Bank 1 decodes the same
/// layout 16 bytes later (MAME's second `GFXDECODE_ENTRY` offset), which
/// `load_sprite_rom` passes as the decode base.
pub static GYRUSS_SPRITE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 4, 0x4000 * 8, 0x4000 * 8 + 4],
    x_offsets: &[0, 1, 2, 3, 64, 65, 66, 67],
    y_offsets: &[
        0, 8, 16, 24, 32, 40, 48, 56, 256, 264, 272, 280, 288, 296, 304, 312,
    ],
    char_increment: 64 * 8,
};

// ---------------------------------------------------------------------------
// Video
// ---------------------------------------------------------------------------

/// Palette DAC ladders as drawn on the sheet (bottom right): red and green
/// through 1K/470/220 with a 470 pulldown, blue through 470/220 with a 470
/// pulldown. PROM bits 0-2 red, 3-5 green, 6-7 blue.
const DAC_RG: [f64; 3] = [1000.0, 470.0, 220.0];
const DAC_B: [f64; 2] = [470.0, 220.0];
const DAC_PULLDOWN: f64 = 470.0;

/// Unscaled weight of each ladder tap: the divider voltage with only that tap
/// tied high. An exact port of MAME `compute_resistor_weights` for one net
/// with no pullup.
fn dac_net_weights(resistors: &[f64]) -> Vec<f64> {
    resistors
        .iter()
        .enumerate()
        .map(|(n, _)| {
            let mut r0 = 1.0 / DAC_PULLDOWN;
            let mut r1 = 1.0 / 1e12; // no pullup
            for (j, &r) in resistors.iter().enumerate() {
                if j == n {
                    r1 += 1.0 / r;
                } else {
                    r0 += 1.0 / r;
                }
            }
            let vout = 255.0 * (1.0 / r0) / ((1.0 / r1) + (1.0 / r0));
            vout.clamp(0.0, 255.0)
        })
        .collect()
}

/// Build the 32-entry palette from the pr3 PROM: per-tap weights, autoscaled
/// so the hottest net (red/green) spans 0-255, combined with round-half-up
/// (MAME `combine_weights`).
fn gyruss_palette(prom: &[u8]) -> [[u8; 3]; 32] {
    let rg = dac_net_weights(&DAC_RG);
    let b = dac_net_weights(&DAC_B);
    let sum_rg: f64 = rg.iter().sum();
    let sum_b: f64 = b.iter().sum();
    let scale = 255.0 / sum_rg.max(sum_b);
    let combine = |w: &[f64], bits: u8| -> u8 {
        let mut acc = 0.0;
        for (i, &weight) in w.iter().enumerate() {
            if bits & (1 << i) != 0 {
                acc += weight * scale;
            }
        }
        (acc + 0.5) as u8
    };
    let mut palette = [[0u8; 3]; 32];
    for (i, entry) in palette.iter_mut().enumerate() {
        let byte = prom.get(i).copied().unwrap_or(0);
        *entry = [
            combine(&rg, byte & 0x07),
            combine(&rg, (byte >> 3) & 0x07),
            combine(&b, (byte >> 6) & 0x03),
        ];
    }
    palette
}

/// Video state: decoded chars/sprites, PROM-derived pens, and a
/// native-orientation RGB framebuffer. Rendering is per-scanline: each
/// scanline draws background tiles, sprites, then foreground tiles, in the
/// order MAME's `screen_update` layers them.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct GyrussVideo {
    /// Decoded 8x8 chars (tiles ROM, 512 codes). ROM-derived, not saved.
    #[save_skip]
    chars: GfxCache,
    /// Decoded 8x16 sprites, bank 0 (ROM offset code*64). ROM-derived.
    #[save_skip]
    sprites_lo: GfxCache,
    /// Decoded 8x16 sprites, bank 1 (ROM offset code*64+16). ROM-derived.
    #[save_skip]
    sprites_hi: GfxCache,
    /// 256 sprite pens: pr1 lookup through the palette. ROM-derived.
    #[save_skip]
    sprite_pens: [(u8, u8, u8); 256],
    /// 64 char pens: pr2 lookup through the palette. ROM-derived.
    #[save_skip]
    char_pens: [(u8, u8, u8); 64],
    /// Native-orientation RGB24 framebuffer. Rebuilt every frame, not saved.
    #[save_skip]
    framebuffer: Vec<u8>,
    /// Cocktail flip (latch Q5). Declared via `orientation` and applied
    /// centrally by the frontend; rendering stays unmirrored.
    #[save(id = 1)]
    flip: bool,
}

impl GyrussVideo {
    pub fn new() -> Self {
        Self {
            chars: GfxCache::new(512, 8, 8),
            sprites_lo: GfxCache::new(256, 8, 16),
            sprites_hi: GfxCache::new(256, 8, 16),
            sprite_pens: [(0, 0, 0); 256],
            char_pens: [(0, 0, 0); 64],
            framebuffer: vec![0u8; NATIVE_WIDTH * NATIVE_HEIGHT * 3],
            flip: false,
        }
    }

    /// Decode the tiles ROM (8 KB, 512 codes).
    pub fn load_tile_rom(&mut self, data: &[u8]) {
        self.chars = decode_gfx(data, 0, 512, &GYRUSS_CHAR_LAYOUT);
    }

    /// Decode the sprite ROMs (32 KB) into the two bank caches.
    pub fn load_sprite_rom(&mut self, data: &[u8]) {
        self.sprites_lo = decode_gfx(data, 0, 256, &GYRUSS_SPRITE_LAYOUT);
        self.sprites_hi = decode_gfx(data, 16, 256, &GYRUSS_SPRITE_LAYOUT);
    }

    /// Load the PROMs: 32 palette bytes, then the 256-byte sprite (pr1) and
    /// char (pr2) lookup tables. Sprites land in the lower 16 palette
    /// entries, chars in the upper 16 (MAME `palette`).
    pub fn load_proms(&mut self, data: &[u8]) {
        let palette = gyruss_palette(data.get(..32).unwrap_or(&[]));
        let rgb = |index: usize| {
            let [r, g, b] = palette[index % 32];
            (r, g, b)
        };
        for (i, pen) in self.sprite_pens.iter_mut().enumerate() {
            let entry = data.get(32 + i).copied().unwrap_or(0) & 0x0f;
            *pen = rgb(entry as usize);
        }
        for (i, pen) in self.char_pens.iter_mut().enumerate() {
            let entry = data.get(288 + i).copied().unwrap_or(0) & 0x0f;
            *pen = rgb(entry as usize | 0x10);
        }
    }

    pub fn set_flip(&mut self, flip: bool) {
        self.flip = flip;
    }

    pub fn reset(&mut self) {
        self.flip = false;
    }

    /// Render scanline `row` (0-223, raster lines 16-239) into the framebuffer.
    ///
    /// Three passes, matching MAME's `screen_update`: opaque background
    /// tiles (color bit 4 set), sprites in reverse entry order so entry 0
    /// wins, then foreground tiles (bit 4 clear) with pixel 0 transparent.
    pub fn render_scanline(
        &mut self,
        row: usize,
        colorram: &[u8],
        videoram: &[u8],
        spriteram: &[u8],
    ) {
        let line = row + VISIBLE_Y_OFFSET;
        let chars = &self.chars;
        let sprites_lo = &self.sprites_lo;
        let sprites_hi = &self.sprites_hi;
        let sprite_pens = &self.sprite_pens;
        let char_pens = &self.char_pens;
        let out = &mut self.framebuffer[row * NATIVE_WIDTH * 3..][..NATIVE_WIDTH * 3];

        // Tile fetches shared by the background and foreground passes.
        let tile = |x: usize| {
            let addr = (line >> 3) * 32 + (x >> 3);
            let color = colorram[addr];
            let code = videoram[addr] as usize | (((color & 0x20) as usize) << 3);
            let mut fx = x & 7;
            let mut fy = line & 7;
            if color & 0x40 != 0 {
                fx = 7 - fx;
            }
            if color & 0x80 != 0 {
                fy = 7 - fy;
            }
            let pix = chars.pixel(code, fx, fy);
            (color, pix)
        };

        // Background pass: group-0 tiles opaque, group-1 left black.
        for x in 0..NATIVE_WIDTH {
            let (color, pix) = tile(x);
            let (r, g, b) = if color & 0x10 != 0 {
                char_pens[(((color & 0x0f) << 2) | pix) as usize]
            } else {
                (0, 0, 0)
            };
            out[x * 3..x * 3 + 3].copy_from_slice(&[r, g, b]);
        }

        // Sprites: 48 entries, 4 bytes each (x, code/bank, attr, y). Entry
        // 47 draws first so entry 0 ends on top (MAME `draw_sprites` walks
        // 0xBC down to 0). Pixel 0 is transparent.
        for s in (0..48).rev() {
            let base = s * 4;
            let top = 241i32 - spriteram[base + 3] as i32;
            if (line as i32) < top || (line as i32) >= top + 16 {
                continue;
            }
            let attr = spriteram[base + 2];
            let mut fy = (line as i32 - top) as usize;
            if attr & 0x80 != 0 {
                fy = 15 - fy;
            }
            // Flip X is inverted: the sprite flips when bit 6 is CLEAR.
            let flipx = attr & 0x40 == 0;
            let code = (spriteram[base + 1] >> 1) as usize | (((attr & 0x20) as usize) << 2);
            let cache = if spriteram[base + 1] & 1 == 0 {
                sprites_lo
            } else {
                sprites_hi
            };
            let color = attr & 0x0f;
            let x0 = spriteram[base] as usize;
            for dx in 0..8usize {
                let px = if flipx { 7 - dx } else { dx };
                let pix = cache.pixel(code, px, fy);
                if pix == 0 {
                    continue;
                }
                let tx = x0 + dx;
                if tx >= NATIVE_WIDTH {
                    continue;
                }
                let (r, g, b) = sprite_pens[((color << 4) | pix) as usize];
                out[tx * 3..tx * 3 + 3].copy_from_slice(&[r, g, b]);
            }
        }

        // Foreground pass: group-1 tiles over the sprites, pixel 0 showing
        // what is beneath.
        for x in 0..NATIVE_WIDTH {
            let (color, pix) = tile(x);
            if color & 0x10 == 0 && pix != 0 {
                let (r, g, b) = char_pens[(((color & 0x0f) << 2) | pix) as usize];
                out[x * 3..x * 3 + 3].copy_from_slice(&[r, g, b]);
            }
        }
    }

    /// Copy the finished buffer out. The cocktail flip is declared via
    /// [`orientation`](Self::orientation) and applied centrally by the
    /// frontend, so this emits pixels unmirrored.
    pub fn render_frame(&self, out: &mut [u8]) {
        out.copy_from_slice(&self.framebuffer);
    }

    /// Declarative screen orientation: base ROT90 composed with the live
    /// cocktail flip.
    ///
    /// The cabinet tube is mounted rotated 90 degrees clockwise (MAME
    /// `ROT90`). The cocktail flip mirrors both native axes (180 degrees),
    /// so flip set composes to `ROT270`.
    pub fn orientation(&self) -> Orientation {
        let mut o = Orientation::ROT90;
        if self.flip {
            o = o.compose(Orientation::COCKTAIL);
        }
        o
    }

    pub fn tile_cache(&self) -> &GfxCache {
        &self.chars
    }

    pub fn sprite_cache(&self) -> &GfxCache {
        &self.sprites_lo
    }

    pub fn char_palette(&self) -> &[(u8, u8, u8)] {
        &self.char_pens
    }

    pub fn sprite_palette(&self) -> &[(u8, u8, u8)] {
        &self.sprite_pens
    }
}

impl Default for GyrussVideo {
    fn default() -> Self {
        Self::new()
    }
}

/// Advance one main-CPU cycle: board work, the Z80, the half-rate sub CPU,
/// then the audio section and the clock.
pub fn tick(cpu: &mut Z80, sub: &mut M6809, board: &mut GyrussBoard) {
    board.begin_cycle();
    cpu.execute_cycle(board, BusMaster::Cpu(0));
    if board.clocks.tick(board.sub_dom) {
        // The sub CPU is not frontend-stepped, so attribute its accesses to
        // its own PC the way the sound board does for its CPUs.
        if board.map.debug_active() {
            let pc = sub.at_instruction_boundary().then_some(u32::from(sub.pc));
            board.map.latch_access_context(board.clock, pc);
        }
        sub.execute_cycle(board, BusMaster::Cpu(1));
    }
    board.end_cycle();
}

/// Run one frame's worth of main-CPU cycles.
pub fn run_frame(cpu: &mut Z80, sub: &mut M6809, board: &mut GyrussBoard) {
    for _ in 0..TIMING.cycles_per_frame() {
        tick(cpu, sub, board);
    }
}

/// Gyruss board: main/sub bus, video, sound board, inputs and DIPs.
#[derive(BusDebug, DebugTrace, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct GyrussBoard {
    /// The address space persists its own writable regions: color, video,
    /// work, shared and sub RAM here. Both CPUs share it (watchpoints carry
    /// each access's own index); the sub regions sit at translated addresses
    /// because both CPUs use address 0 (see `build_map`).
    #[debug_map(cpu = 0)]
    #[save(id = 1)]
    pub(crate) map: AddressSpace16,

    #[save(id = 2)]
    pub(crate) video: GyrussVideo,

    /// Both attributes, for two different views of the same board: the device
    /// entry is its latches (`COMMAND`, `IRQ`, `DAC`, ...), and `#[debug_bus]`
    /// merges the tree it derives for itself, which is how its Z80 becomes
    /// CPU 2 (and the 8039 CPU 3) and its address space becomes theirs.
    #[debug_device("Gyruss Sound")]
    #[debug_bus]
    #[save(id = 3)]
    pub(crate) sound: GyrussSound,

    // Input ports (active-low: 0xFF = nothing pressed).
    #[save(id = 4)]
    pub(crate) in_system: u8,
    #[save(id = 5)]
    pub(crate) in_p1: u8,
    #[save(id = 6)]
    pub(crate) in_p2: u8,
    // DIP switches on their own ports.
    #[save(id = 7)]
    pub(crate) dsw1: u8,
    #[save(id = 8)]
    pub(crate) dsw2: u8,
    #[save(id = 9)]
    pub(crate) dsw3: u8,

    /// LS259 latch (3C) output byte.
    #[save(id = 10)]
    pub(crate) latch: u8,
    #[save(id = 11)]
    pub(crate) nmi_pending: bool,
    #[save(id = 12)]
    pub(crate) sub_irq_pending: bool,
    /// Sub IRQ mask (bit 0 of a 0x2000 write).
    #[save(id = 13)]
    pub(crate) sub_irq_mask: bool,

    #[save(id = 14)]
    pub(crate) clock: u64,
    /// The board's clock tree, as [`clock_tree`] declares it. Only the sub
    /// domain is stepped; the rest is the derivation it rides on.
    #[debug_device("Clocks")]
    #[save(id = 15)]
    clocks: ClockTree,
    #[save_skip]
    sub_dom: DomainId,
    /// Bresenham numerator stepping the audio section (which outruns the main
    /// clock) off exact crystal rates; see `end_cycle`.
    #[save(id = 16)]
    acc_audio: u64,
    /// Audio over main crystal rates, read from the tree at build.
    #[save_skip]
    audio_num: u64,
    #[save_skip]
    audio_den: u64,

    #[debug_events]
    #[save_skip]
    pub(crate) debug_trace: DebugTraceBuffer,
}

impl GyrussBoard {
    pub fn new() -> Self {
        let clocks = clock_tree();
        let sub_dom = clocks.find(Clk::SubCpu).expect("declared sub domain");
        let audio_dom = clocks.find(Clk::SoundCpu).expect("declared sound domain");
        let main_dom = clocks.find(Clk::Cpu).expect("declared main domain");
        // One derivation reaches both places the audio rate is used: the
        // ratio this board steps the sound section at, and the AY-8910s'
        // chip clock inside the sound board.
        let audio_hz = clocks.hz(audio_dom);
        let main_hz = clocks.hz(main_dom);
        Self {
            map: Self::build_map(),
            video: GyrussVideo::new(),
            sound: GyrussSound::new(audio_hz),
            in_system: 0xFF,
            in_p1: 0xFF,
            in_p2: 0xFF,
            dsw1: DEFAULT_DSW1,
            dsw2: DEFAULT_DSW2,
            dsw3: DEFAULT_DSW3,
            latch: 0,
            nmi_pending: false,
            sub_irq_pending: false,
            sub_irq_mask: false,
            clock: 0,
            clocks,
            sub_dom,
            acc_audio: 0,
            audio_num: audio_hz,
            audio_den: main_hz,
            debug_trace: DebugTraceBuffer::new(),
        }
    }

    /// Both CPUs share one map, so the sub regions are translated out of the
    /// main CPU's way: sub 0x4000-0x47FF lives at 0xA800 and sub
    /// 0xE000-0xFFFF at 0xB000 (past the shared RAM, where the main CPU only
    /// sees open bus). Same trick as the sound board's MCU ROM.
    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(
            Region::MainRom,
            "Program ROM",
            0x0000,
            0x6000,
            AccessKind::ReadOnly,
        )
        .region(
            Region::ColorRam,
            "Color RAM",
            0x8000,
            0x0400,
            AccessKind::ReadWrite,
        )
        .region(
            Region::VideoRam,
            "Video RAM",
            0x8400,
            0x0400,
            AccessKind::ReadWrite,
        )
        .region(
            Region::WorkRam,
            "Work RAM",
            0x9000,
            0x1000,
            AccessKind::ReadWrite,
        )
        .region(
            Region::SharedRam,
            "Shared RAM (main 0xA000 = sub 0x6000)",
            0xA000,
            0x0800,
            AccessKind::ReadWrite,
        )
        .region(
            Region::SubRam,
            "Sub RAM (sub 0x4000)",
            0xA800,
            0x0800,
            AccessKind::ReadWrite,
        )
        .region(
            Region::SubRom,
            "Sub ROM (sub 0xE000)",
            0xB000,
            0x2000,
            AccessKind::ReadOnly,
        );
        map
    }

    pub fn load_program_rom(&mut self, data: &[u8]) {
        self.map.load_region(Region::MainRom, data);
    }
    pub fn load_sub_rom(&mut self, data: &[u8]) {
        self.map.load_region(Region::SubRom, data);
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
        self.sound.load_sound_rom(data);
    }
    pub fn load_mcu_rom(&mut self, data: &[u8]) {
        self.sound.load_mcu_rom(data);
    }

    pub fn clock(&self) -> u64 {
        self.clock
    }

    /// Current raster line (0-255), which is what the sub CPU's 0x0000
    /// scanline read returns.
    pub fn scanline(&self) -> u8 {
        ((self.clock % TIMING.cycles_per_frame()) / TIMING.cycles_per_scanline) as u8
    }

    // -----------------------------------------------------------------------
    // Core tick
    // -----------------------------------------------------------------------

    /// Board work that leads a CPU cycle: the per-scanline render and the
    /// VBLANK interrupt edges.
    fn begin_cycle(&mut self) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();

        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let line = frame_cycle / TIMING.cycles_per_scanline;
            if (VISIBLE_Y_OFFSET as u64..VBLANK_LINE).contains(&line) {
                let row = (line as usize) - VISIBLE_Y_OFFSET;
                let colorram = self.map.region_data(Region::ColorRam);
                let videoram = self.map.region_data(Region::VideoRam);
                let subram = self.map.region_data(Region::SubRam);
                self.video
                    .render_scanline(row, colorram, videoram, &subram[0x40..0x100]);
            }
        }

        if frame_cycle == VBLANK_LINE * TIMING.cycles_per_scanline {
            self.nmi_pending = true;
            self.sub_irq_pending = true;
        }
        if frame_cycle == 0 && self.clock > 0 {
            self.nmi_pending = false;
            self.sub_irq_pending = false;
        }
    }

    /// Board work after the CPUs' cycles: the audio section, then the clock.
    fn end_cycle(&mut self) {
        // The audio clock outruns the main clock, so it cannot step from the
        // tree (which only divides the step domain down): each main cycle
        // adds audio_hz, and each main_hz reached runs one audio tick. Exact
        // crystal ratio, no drift, mean ~1.165 ticks per cycle.
        self.acc_audio += self.audio_num;
        while self.acc_audio >= self.audio_den {
            self.acc_audio -= self.audio_den;
            self.sound.tick();
        }

        self.clock += 1;
    }

    /// The interrupt lines this board drives. Named to avoid shadowing
    /// [`Bus::check_interrupts`], which the board also implements.
    pub fn interrupt_state(&self, target: BusMaster) -> InterruptState {
        let mut state = InterruptState::default();
        match target {
            BusMaster::Cpu(0) => {
                state.nmi = self.nmi_pending && (self.latch & LATCH_NMI_ENABLE != 0);
            }
            BusMaster::Cpu(1) => {
                state.irq = self.sub_irq_pending && self.sub_irq_mask;
            }
            _ => {}
        }
        state
    }

    pub fn render_frame(&self, buffer: &mut [u8]) {
        self.video.render_frame(buffer);
    }

    /// Declarative orientation (the live cocktail flip); applied centrally by
    /// the frontend.
    pub fn orientation(&self) -> Orientation {
        self.video.orientation()
    }

    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize {
        self.sound.fill_audio(out)
    }

    pub fn reset_board(&mut self) {
        self.video.reset();
        phosphor_core::device::Device::reset(&mut self.sound);
        self.latch = 0;
        self.nmi_pending = false;
        self.sub_irq_pending = false;
        self.sub_irq_mask = false;
        self.clock = 0;
        self.clocks.reset();
        self.acc_audio = 0;
        self.map.region_data_mut(Region::ColorRam).fill(0);
        self.map.region_data_mut(Region::VideoRam).fill(0);
        self.map.region_data_mut(Region::WorkRam).fill(0);
        self.map.region_data_mut(Region::SharedRam).fill(0);
        self.map.region_data_mut(Region::SubRam).fill(0);
    }

    /// Which CPUs are between instructions, in `cpus()` order: the main Z80
    /// in bit 0, the sub in bit 1, the sound board's Z80 in bit 2 and the
    /// 8039 in bit 3.
    ///
    /// The main and sub CPUs live on the machine, which passes them back in;
    /// the sound CPUs are this board's own.
    pub fn instruction_boundaries(&self, cpu: &Z80, sub: &M6809) -> u32 {
        u32::from(cpu.at_instruction_boundary())
            | (u32::from(sub.at_instruction_boundary()) << 1)
            | (u32::from(self.sound.at_instruction_boundary()) << 2)
            | (u32::from(self.sound.mcu_at_instruction_boundary()) << 3)
    }

    // -----------------------------------------------------------------------
    // Bus dispatch
    // -----------------------------------------------------------------------

    fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let (index, data) = match master {
            BusMaster::Cpu(0) => (0, self.main_read(addr)),
            BusMaster::Cpu(1) => (1, self.sub_read(addr)),
            _ => return 0xFF,
        };
        self.map.watch_read(index, master, addr, data);
        data
    }

    fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        let index = match master {
            BusMaster::Cpu(0) => 0,
            BusMaster::Cpu(1) => 1,
            _ => return,
        };
        self.map.watch_write(index, master, addr, data);
        match master {
            BusMaster::Cpu(0) => self.main_write(addr, data),
            BusMaster::Cpu(1) => self.sub_write(addr, data),
            _ => {}
        }
    }

    fn main_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x5fff => self.map.read_backing(addr),
            // 0x6000-0x7FFF: empty diagnostics socket, reads open bus (never
            // 0x55, so the diagnostics branch stays off).
            0x8000..=0x87ff => self.map.read_backing(addr),
            0x9000..=0x9fff => self.map.read_backing(addr),
            0xa000..=0xa7ff => self.map.read_backing(addr),
            0xc000..=0xc1ff => self.main_io_read(addr),
            _ => 0xFF,
        }
    }

    fn main_write(&mut self, addr: u16, data: u8) {
        match addr {
            0x8000..=0x87ff => self.map.write_backing(addr, data),
            0x9000..=0x9fff => self.map.write_backing(addr, data),
            0xa000..=0xa7ff => self.map.write_backing(addr, data),
            0xc000 => {} // watchdog reset (not modeled)
            0xc080 => self.sound.pulse_irq(),
            0xc100 => self.sound.write_command(data),
            0xc180..=0xc187 => self.latch_write(addr, data),
            _ => {}
        }
    }

    fn main_io_read(&mut self, addr: u16) -> u8 {
        match addr {
            0xc000 => self.dsw2,
            0xc080 => self.in_system,
            0xc0a0 => self.in_p1,
            0xc0c0 => self.in_p2,
            0xc0e0 => self.dsw1,
            0xc100 => self.dsw3,
            _ => 0xFF,
        }
    }

    fn sub_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000 => self.scanline(),
            0x4000..=0x47ff => self.map.read_backing(addr - 0x4000 + 0xA800),
            0x6000..=0x67ff => self.map.read_backing(addr - 0x6000 + 0xA000),
            0xe000..=0xffff => self.map.read_backing(addr - 0xE000 + 0xB000),
            _ => 0xFF,
        }
    }

    fn sub_write(&mut self, addr: u16, data: u8) {
        match addr {
            0x2000 => {
                self.sub_irq_mask = data & 1 != 0;
                if !self.sub_irq_mask {
                    self.sub_irq_pending = false;
                }
            }
            0x4000..=0x47ff => self.map.write_backing(addr - 0x4000 + 0xA800, data),
            0x6000..=0x67ff => self.map.write_backing(addr - 0x6000 + 0xA000, data),
            _ => {}
        }
    }

    /// LS259 at 0xC180-0xC187: the latch line is `addr & 7` and the latched
    /// bit is D0 (MAME `write_d0`).
    fn latch_write(&mut self, addr: u16, data: u8) {
        let line = addr & 7;
        let bit = data & 1 != 0;
        if bit {
            self.latch |= 1 << line;
        } else {
            self.latch &= !(1 << line);
        }
        match line {
            0 => {
                if !bit {
                    self.nmi_pending = false;
                }
            }
            5 => self.video.set_flip(self.latch & LATCH_FLIP != 0),
            _ => {} // 2, 3 = coin counters (not modeled); 1, 4, 6, 7 unwired
        }
    }
}

impl Default for GyrussBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for GyrussBoard {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        self.bus_read(master, addr)
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.bus_write(master, addr, data);
    }

    fn io_read(&mut self, _master: BusMaster, _addr: u16) -> u8 {
        0xFF
    }

    fn io_write(&mut self, _master: BusMaster, _addr: u16, _data: u8) {}

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        self.interrupt_state(target)
    }
}

// ---------------------------------------------------------------------------
// ROM definitions (MAME `ROM_START(gyruss)` / `ROM_START(gyrussce)`)
// ---------------------------------------------------------------------------

/// One loader per revision, paired by position with the `Revision` list in
/// the `register_machine!` call below. Only the PROMs are shared; every other
/// region is a different filename per set (same CRCs on sub/sound/MCU).
pub struct GyrussRomConfig {
    pub set: &'static str,
    pub program: &'static RomRegion,
    pub sub: &'static RomRegion,
    pub sound: &'static RomRegion,
    pub mcu: &'static RomRegion,
    pub tiles: &'static RomRegion,
    pub sprites: &'static RomRegion,
    pub dsw1: u8,
    pub dsw2: u8,
    pub dsw3: u8,
}

const ALL_CONFIGS: &[&GyrussRomConfig] = &[
    &GyrussRomConfig {
        set: "gyruss",
        program: &GYRUSS_PROGRAM_ROM,
        sub: &GYRUSS_SUB_ROM,
        sound: &GYRUSS_SOUND_ROM,
        mcu: &GYRUSS_MCU_ROM,
        tiles: &GYRUSS_TILE_ROM,
        sprites: &GYRUSS_SPRITE_ROM,
        dsw1: DEFAULT_DSW1,
        dsw2: DEFAULT_DSW2,
        dsw3: DEFAULT_DSW3,
    },
    &GyrussRomConfig {
        set: "gyrussce",
        program: &GYRUSSCE_PROGRAM_ROM,
        sub: &GYRUSSCE_SUB_ROM,
        sound: &GYRUSSCE_SOUND_ROM,
        mcu: &GYRUSSCE_MCU_ROM,
        tiles: &GYRUSSCE_TILE_ROM,
        sprites: &GYRUSSCE_SPRITE_ROM,
        dsw1: DEFAULT_DSW1,
        dsw2: DEFAULTCE_DSW2,
        dsw3: DEFAULT_DSW3,
    },
];

pub static GYRUSS_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        RomEntry {
            name: "gyrussk.1",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xc673b43d],
        },
        RomEntry {
            name: "gyrussk.2",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0xa4ec03e4],
        },
        RomEntry {
            name: "gyrussk.3",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x27454a98],
        },
    ],
};

pub static GYRUSS_SUB_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[RomEntry {
        name: "gyrussk.9",
        size: 0x2000,
        offset: 0x0000,
        crc32: &[0x822bf27e],
    }],
};

pub static GYRUSS_SOUND_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "gyrussk.1a",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xf4ae1c17],
        },
        RomEntry {
            name: "gyrussk.2a",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0xba498115],
        },
    ],
};

pub static GYRUSS_MCU_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "gyrussk.3a",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0x3f9b5dea],
    }],
};

pub static GYRUSS_TILE_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[RomEntry {
        name: "gyrussk.4",
        size: 0x2000,
        offset: 0x0000,
        crc32: &[0x27d8329b],
    }],
};

pub static GYRUSS_SPRITE_ROM: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        RomEntry {
            name: "gyrussk.6",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xc949db10],
        },
        RomEntry {
            name: "gyrussk.5",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x4f22411a],
        },
        RomEntry {
            name: "gyrussk.8",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x47cd1fbc],
        },
        RomEntry {
            name: "gyrussk.7",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0x8e8d388c],
        },
    ],
};

pub static GYRUSSCE_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        RomEntry {
            name: "gya-1.11j",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x85f8b7c2],
        },
        RomEntry {
            name: "gya-2.12j",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x1e1a970f],
        },
        RomEntry {
            name: "gya-3.13j",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0xf6dbb33b],
        },
    ],
};

pub static GYRUSSCE_SUB_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[RomEntry {
        name: "gy-5.19e",
        size: 0x2000,
        offset: 0x0000,
        crc32: &[0x822bf27e],
    }],
};

pub static GYRUSSCE_SOUND_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "gy-11.7a",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xf4ae1c17],
        },
        RomEntry {
            name: "gy-12.8a",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0xba498115],
        },
    ],
};

pub static GYRUSSCE_MCU_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "gy-13.11h",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0x3f9b5dea],
    }],
};

pub static GYRUSSCE_TILE_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[RomEntry {
        name: "gy-6.1g",
        size: 0x2000,
        offset: 0x0000,
        crc32: &[0x27d8329b],
    }],
};

pub static GYRUSSCE_SPRITE_ROM: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        RomEntry {
            name: "gy-10.9d",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xc949db10],
        },
        RomEntry {
            name: "gy-9.8d",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x4f22411a],
        },
        RomEntry {
            name: "gy-8.7d",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x47cd1fbc],
        },
        RomEntry {
            name: "gy-7.6d",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0x8e8d388c],
        },
    ],
};

/// Shared by both sets: the CE ROM definition lists the same three PROMs.
pub static GYRUSS_PROM: RomRegion = RomRegion {
    size: 0x0220,
    entries: &[
        RomEntry {
            name: "gyrussk.pr3",
            size: 0x0020,
            offset: 0x0000,
            crc32: &[0x98782db3],
        },
        RomEntry {
            name: "gyrussk.pr1",
            size: 0x0100,
            offset: 0x0020,
            crc32: &[0x7ed057de],
        },
        RomEntry {
            name: "gyrussk.pr2",
            size: 0x0100,
            offset: 0x0120,
            crc32: &[0xde823a81],
        },
    ],
};

// ---------------------------------------------------------------------------
// DIP switches
// ---------------------------------------------------------------------------
//
// DSW1 is the standard Konami coinage table (MAME `KONAMI_COINAGE_LOC` with
// Free Play on both coins); DSW2 carries gameplay, DSW3 the demo music.
//
// Sources: the Centuri manual pp5-6 (transcribed in
// `docs/schematics/gyruss-sound.md`) is the gyrussce table, so the CE labels
// below are the manual's words. The manual is the Centuri book, so the Konami
// parent's bonus thresholds and difficulty default follow MAME and are
// flagged: `PARENT LABELS FROM MAME`.

const DEFAULT_DSW1: u8 = 0xff;
const DEFAULT_DSW2: u8 = 0x3b;
const DEFAULTCE_DSW2: u8 = 0x23;
const DEFAULT_DSW3: u8 = 0xfe;

const DSW1_OPTIONS: &[DipOption] = &[
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
];

/// Parent DSW2 gameplay (MAME `gyruss` input ports).
const DSW2_OPTIONS: &[DipOption] = &[
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
                label: "256",
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
        // PARENT LABELS FROM MAME (code tables at 0x1653/0x4bf3).
        choices: &[
            DipChoice {
                label: "30K, 90K, then every 60K",
                value: 0x08,
            },
            DipChoice {
                label: "40K, 110K, then every 70K",
                value: 0x00,
            },
        ],
        conditional: &[],
    },
    DipOption {
        name: "Difficulty",
        mask: 0x70,
        apply: DipApplyTiming::Immediate,
        // PARENT LABELS FROM MAME (no manual for the Konami set).
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
                label: "5 (Average)",
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
                label: "8 (Hardest)",
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
                label: "On",
                value: 0x00,
            },
            DipChoice {
                label: "Off",
                value: 0x80,
            },
        ],
        conditional: &[],
    },
];

/// Centuri DSW2 gameplay: the manual's pp5-6 words, default Difficult.
const DSW2CE_OPTIONS: &[DipOption] = &[
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
                label: "256",
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
                label: "50K, 120K, then every 70K",
                value: 0x08,
            },
            DipChoice {
                label: "60K, 140K, then every 80K",
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
                label: "Very Easy",
                value: 0x70,
            },
            DipChoice {
                label: "Easy 1",
                value: 0x60,
            },
            DipChoice {
                label: "Easy 2",
                value: 0x50,
            },
            DipChoice {
                label: "Easy 3",
                value: 0x40,
            },
            DipChoice {
                label: "Average",
                value: 0x30,
            },
            DipChoice {
                label: "Difficult",
                value: 0x20,
            },
            DipChoice {
                label: "Very Difficult",
                value: 0x10,
            },
            DipChoice {
                label: "Most Difficult",
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
                label: "On",
                value: 0x00,
            },
            DipChoice {
                label: "Off",
                value: 0x80,
            },
        ],
        conditional: &[],
    },
];

const DSW3_OPTIONS: &[DipOption] = &[DipOption {
    name: "Demo Music",
    mask: 0x01,
    apply: DipApplyTiming::Immediate,
    choices: &[
        DipChoice {
            label: "On",
            value: 0x00,
        },
        DipChoice {
            label: "Off",
            value: 0x01,
        },
    ],
    conditional: &[],
}];

pub(crate) const GYRUSS_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSW1",
        options: DSW1_OPTIONS,
    },
    DipSwitchBank {
        name: "DSW2",
        options: DSW2_OPTIONS,
    },
    DipSwitchBank {
        name: "DSW3",
        options: DSW3_OPTIONS,
    },
];

pub(crate) const GYRUSSCE_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSW1",
        options: DSW1_OPTIONS,
    },
    DipSwitchBank {
        name: "DSW2",
        options: DSW2CE_OPTIONS,
    },
    DipSwitchBank {
        name: "DSW3",
        options: DSW3_OPTIONS,
    },
];

// ---------------------------------------------------------------------------
// Inputs (MAME `gyruss` input ports; all active-low)
// ---------------------------------------------------------------------------

pub const GYRUSS_CONTROLS: &[InputControl] = &[
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
// GyrussSystem wrapper
// ---------------------------------------------------------------------------

/// Gyruss (Konami, 1983).
///
/// Same split as the other multi-CPU machines: the main and sub CPUs sit
/// beside the board, which *is* their bus, so each cycle dispatches at a
/// concrete type. The sound board's two CPUs are the board's own.
#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct GyrussSystem {
    #[debug_cpu("Z80 Main")]
    #[save(id = 1)]
    pub cpu: Z80,
    #[debug_cpu("Konami-1 Sub")]
    #[save(id = 2)]
    pub sub: M6809,
    #[debug_bus]
    #[save(id = 3)]
    pub board: GyrussBoard,
    /// MAME set name this machine loaded (e.g., "gyrussce"), reported by
    /// `MachineCore::revision` and selecting the DIP bank table. Loading a
    /// save restores state, never ROMs, so this keeps its value across loads.
    #[save_skip]
    loaded_revision: &'static str,
}

impl GyrussSystem {
    pub fn new() -> Self {
        let mut sub = M6809::new();
        sub.set_konami_decryption(true);
        Self {
            cpu: Z80::new(),
            sub,
            board: GyrussBoard::new(),
            loaded_revision: "",
        }
    }

    pub fn load_roms(
        &mut self,
        rom_set: &RomSet,
        config: &GyrussRomConfig,
    ) -> Result<(), RomLoadError> {
        // Recorded before the first ROM read so a blank-set load still
        // carries the attempted revision. `create` builds a fresh instance
        // per attempt, so a failed attempt cannot poison a later success.
        self.loaded_revision = config.set;
        self.board.dsw1 = config.dsw1;
        self.board.dsw2 = config.dsw2;
        self.board.dsw3 = config.dsw3;
        self.board.load_program_rom(&config.program.load(rom_set)?);
        self.board.load_sub_rom(&config.sub.load(rom_set)?);
        self.board.load_sound_rom(&config.sound.load(rom_set)?);
        self.board.load_mcu_rom(&config.mcu.load(rom_set)?);
        self.board.load_tile_rom(&config.tiles.load(rom_set)?);
        self.board.load_sprite_rom(&config.sprites.load(rom_set)?);
        self.board.load_proms(&GYRUSS_PROM.load(rom_set)?);
        Ok(())
    }

    /// Gyruss input bit mapping (active-low; pressing clears the bit).
    fn apply_input(&mut self, button: u8, pressed: bool) {
        let b = &mut self.board;
        match button {
            INPUT_COIN1 => crate::set_bit_active_low(&mut b.in_system, 0, pressed),
            INPUT_COIN2 => crate::set_bit_active_low(&mut b.in_system, 1, pressed),
            INPUT_SERVICE => crate::set_bit_active_low(&mut b.in_system, 2, pressed),
            INPUT_P1_START => crate::set_bit_active_low(&mut b.in_system, 3, pressed),
            INPUT_P2_START => crate::set_bit_active_low(&mut b.in_system, 4, pressed),
            INPUT_P1_LEFT => crate::set_bit_active_low(&mut b.in_p1, 0, pressed),
            INPUT_P1_RIGHT => crate::set_bit_active_low(&mut b.in_p1, 1, pressed),
            INPUT_P1_UP => crate::set_bit_active_low(&mut b.in_p1, 2, pressed),
            INPUT_P1_DOWN => crate::set_bit_active_low(&mut b.in_p1, 3, pressed),
            INPUT_P1_FIRE => crate::set_bit_active_low(&mut b.in_p1, 4, pressed),
            INPUT_P2_LEFT => crate::set_bit_active_low(&mut b.in_p2, 0, pressed),
            INPUT_P2_RIGHT => crate::set_bit_active_low(&mut b.in_p2, 1, pressed),
            INPUT_P2_UP => crate::set_bit_active_low(&mut b.in_p2, 2, pressed),
            INPUT_P2_DOWN => crate::set_bit_active_low(&mut b.in_p2, 3, pressed),
            INPUT_P2_FIRE => crate::set_bit_active_low(&mut b.in_p2, 4, pressed),
            _ => {}
        }
    }

    /// Snapshot of the Z80's registers, for tests and the debugger.
    pub fn get_cpu_state(&self) -> phosphor_core::cpu::state::Z80State {
        self.cpu.snapshot()
    }

    /// Advance one main-CPU cycle, returning the instruction-boundary mask.
    pub fn step_cycle(&mut self) -> u32 {
        tick(&mut self.cpu, &mut self.sub, &mut self.board);
        self.board.instruction_boundaries(&self.cpu, &self.sub)
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

impl Default for GyrussSystem {
    fn default() -> Self {
        Self::new()
    }
}

// The board is the bus; see `impl Bus for GyrussBoard` above. Composed from
// the pieces rather than `impl_board_delegation!` because the board is stereo
// (like Star Wars): the delegation macro only speaks mono.
crate::impl_board_renderable!(GyrussSystem, board, TIMING, orientation);
crate::impl_board_audio!(GyrussSystem, board, 2);
crate::impl_board_debug!(GyrussSystem, board, TIMING);

impl MachineCore for GyrussSystem {
    crate::machine_core_metadata!("gyruss", TIMING, crate::gyruss::clock_tree);

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
        run_frame(&mut self.cpu, &mut self.sub, &mut self.board);
    }

    fn reset(&mut self) {
        self.board.reset_board();
        self.cpu.reset(&mut self.board, BusMaster::Cpu(0));
        self.sub.reset(&mut self.board, BusMaster::Cpu(1));
    }
}

impl SaveState for GyrussSystem {
    crate::machine_save_state!();
}

impl Nvram for GyrussSystem {}
impl Profilable for GyrussSystem {}

impl InputConfigurable for GyrussSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        GYRUSS_CONTROLS
    }
    fn handle_input(&mut self, event: InputEvent) {
        if let InputEvent::Button { id, pressed } = event {
            self.apply_input(id.0 as u8, pressed);
        }
    }
}

// Hand-written rather than `impl_dip_switches!`: the DSW2 bonus thresholds
// and difficulty labels genuinely differ between the Konami parent and the
// Centuri set, so each revision gets its own bank table.
impl phosphor_core::core::machine::DipSwitches for GyrussSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        if self.loaded_revision == "gyrussce" {
            GYRUSSCE_DIP_BANKS
        } else {
            GYRUSS_DIP_BANKS
        }
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        match bank {
            0 => self.board.dsw1,
            1 => self.board.dsw2,
            2 => self.board.dsw3,
            _ => 0,
        }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        match bank {
            0 => self.board.dsw1 = value,
            1 => self.board.dsw2 = value,
            2 => self.board.dsw3 = value,
            _ => {}
        }
    }
}

crate::impl_board_debug_trace!(GyrussSystem, board);

crate::register_machine!(
    GyrussSystem,
    "gyruss",
    &[
        crate::registry::Revision {
            names: &["gyruss"],
            nvram_group: None
        },
        crate::registry::Revision {
            names: &["gyrussce"],
            nvram_group: None
        },
    ],
    GYRUSS_CONTROLS,
    configs = ALL_CONFIGS
);

inventory::submit! {
    DisasmRegion {
        machine: "gyruss",
        region: "main",
        cpu: DisasmCpu::Z80,
        org: 0,
        size: GYRUSS_PROGRAM_ROM.size as u32,
        load: |rs| GYRUSS_PROGRAM_ROM.load(rs),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "gyruss",
        region: "sub",
        cpu: DisasmCpu::M6809,
        org: 0xE000,
        size: GYRUSS_SUB_ROM.size as u32,
        load: |rs| GYRUSS_SUB_ROM.load(rs),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "gyruss",
        region: "sound",
        cpu: DisasmCpu::Z80,
        org: 0,
        size: GYRUSS_SOUND_ROM.size as u32,
        load: |rs| GYRUSS_SOUND_ROM.load(rs),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "gyruss",
        region: "mcu",
        cpu: DisasmCpu::I8035,
        org: 0,
        size: GYRUSS_MCU_ROM.size as u32,
        load: |rs| GYRUSS_MCU_ROM.load(rs),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::DipSwitches;

    crate::dip_test_suite!(GyrussSystem, &[DEFAULT_DSW1, DEFAULT_DSW2, DEFAULT_DSW3]);

    #[test]
    fn machine_id_and_defaults() {
        let sys = GyrussSystem::new();
        assert_eq!(sys.machine_id(), "gyruss");
        assert_eq!(sys.board.dsw1, DEFAULT_DSW1);
        assert_eq!(sys.board.dsw2, DEFAULT_DSW2);
        assert_eq!(sys.board.dsw3, DEFAULT_DSW3);
        assert_eq!(sys.board.latch, 0);
    }

    #[test]
    fn centuri_banks_validate_against_centuri_defaults() {
        // The generated suite covers a fresh system (the parent table); the
        // CE table validates separately, against the defaults its config
        // applies on load.
        crate::assert_dip_banks_valid(
            GYRUSSCE_DIP_BANKS,
            &[DEFAULT_DSW1, DEFAULTCE_DSW2, DEFAULT_DSW3],
        );
        let config = ALL_CONFIGS[1];
        assert_eq!(config.set, "gyrussce");
        assert_eq!(config.dsw2, DEFAULTCE_DSW2);
        let mut sys = GyrussSystem::new();
        sys.loaded_revision = config.set;
        sys.board.dsw1 = config.dsw1;
        sys.board.dsw2 = config.dsw2;
        sys.board.dsw3 = config.dsw3;
        // The CE table carries the manual's difficulty words where the parent
        // table carries MAME's numbers.
        assert_eq!(sys.dip_banks()[1].options[3].choices[5].label, "Difficult");
        assert_eq!(
            GyrussSystem::new().dip_banks()[1].options[3].choices[5].label,
            "6"
        );
        assert_eq!(sys.dip_bank_value(1), DEFAULTCE_DSW2);
    }

    #[test]
    fn active_low_inputs_land_on_their_ports() {
        let mut sys = GyrussSystem::new();
        sys.apply_input(INPUT_COIN1, true);
        assert_eq!(
            sys.board.in_system & 0x01,
            0x00,
            "coin1 clears SYSTEM bit 0"
        );
        sys.apply_input(INPUT_P1_START, true);
        assert_eq!(
            sys.board.in_system & 0x08,
            0x00,
            "start clears SYSTEM bit 3"
        );
        sys.apply_input(INPUT_P1_LEFT, true);
        assert_eq!(sys.board.in_p1 & 0x01, 0x00, "left clears P1 bit 0");
        sys.apply_input(INPUT_P1_FIRE, true);
        assert_eq!(sys.board.in_p1 & 0x10, 0x00, "fire clears P1 bit 4");
        sys.apply_input(INPUT_P2_DOWN, true);
        assert_eq!(sys.board.in_p2 & 0x08, 0x00, "P2 down clears P2 bit 3");
        sys.apply_input(INPUT_P1_LEFT, false);
        assert_eq!(sys.board.in_p1 & 0x01, 0x01, "release restores the bit");
    }

    #[test]
    fn palette_dac_matches_hand_computed_weights() {
        // Resnet: rg taps (33.23, 70.71, 151.05), blue taps (78.74, 168.22),
        // combined round-half-up.
        let full = gyruss_palette(&[0xFF; 32]);
        assert_eq!(full[0], [255, 255, 247]);
        let empty = gyruss_palette(&[0x00; 32]);
        assert_eq!(empty[0], [0, 0, 0]);
        let red_bit0 = gyruss_palette(&[0x01; 32]);
        assert_eq!(red_bit0[0], [33, 0, 0]);
        let green_bit0 = gyruss_palette(&[0x08; 32]);
        assert_eq!(green_bit0[0], [0, 33, 0]);
        let blue_bit0 = gyruss_palette(&[0x40; 32]);
        assert_eq!(blue_bit0[0], [0, 0, 79]);
        // Short PROM reads black past the end (ROM-less board boots black).
        let short = gyruss_palette(&[]);
        assert_eq!(short[31], [0, 0, 0]);
    }

    #[test]
    fn sub_regions_translate_and_share() {
        let mut sys = GyrussSystem::new();
        // Sub ROM is readable at sub 0xE000, translated to 0xB000 backing.
        sys.board.load_sub_rom(&[0xA5; 0x2000]);
        assert_eq!(sys.bus_read(BusMaster::Cpu(1), 0xE000), 0xA5);
        assert_eq!(sys.bus_read(BusMaster::Cpu(1), 0xFFFF), 0xA5);
        // The main CPU sees open bus where the sub ROM's backing sits.
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0xB000), 0xFF);
        // Shared RAM crosses between the views.
        sys.bus_write(BusMaster::Cpu(0), 0xA000, 0x5A);
        assert_eq!(sys.bus_read(BusMaster::Cpu(1), 0x6000), 0x5A);
        sys.bus_write(BusMaster::Cpu(1), 0x67FF, 0x3C);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0xA7FF), 0x3C);
        // Empty sockets read 0xFF, never the 0x55 diagnostics flag.
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x6000), 0xFF);
    }

    #[test]
    fn vblank_sets_and_frame_start_clears_interrupts() {
        let mut sys = GyrussSystem::new();
        sys.board.latch = LATCH_NMI_ENABLE;
        sys.board.sub_irq_mask = true;
        // Step past the vblank edge: begin_cycle fires it at the START of
        // the edge cycle, so target steps leave it one cycle un-begun.
        let target = VBLANK_LINE * TIMING.cycles_per_scanline;
        for _ in 0..=target {
            sys.step_cycle();
        }
        assert!(sys.board.nmi_pending, "NMI pending at vblank");
        assert!(sys.board.sub_irq_pending, "sub IRQ pending at vblank");
        let nmi = sys.board.interrupt_state(BusMaster::Cpu(0));
        assert!(nmi.nmi, "NMI line asserted through the mask");
        let irq = sys.board.interrupt_state(BusMaster::Cpu(1));
        assert!(irq.irq, "sub IRQ line asserted through the mask");
        // Masking off drops the lines (MAME mask handlers clear on write).
        sys.bus_write(BusMaster::Cpu(0), 0xC180, 0x00);
        assert!(!sys.board.interrupt_state(BusMaster::Cpu(0)).nmi);
        sys.bus_write(BusMaster::Cpu(1), 0x2000, 0x00);
        assert!(!sys.board.interrupt_state(BusMaster::Cpu(1)).irq);
    }

    #[test]
    fn scanline_tracks_the_raster() {
        let mut sys = GyrussSystem::new();
        assert_eq!(sys.bus_read(BusMaster::Cpu(1), 0x0000), 0);
        for _ in 0..TIMING.cycles_per_scanline * 100 {
            sys.step_cycle();
        }
        assert_eq!(sys.bus_read(BusMaster::Cpu(1), 0x0000), 100);
    }

    #[test]
    fn scanline_render_layers_tiles_and_sprites() {
        let mut sys = GyrussSystem::new();
        // White palette, identity lookups: every pen renders white.
        let mut proms = vec![0xFFu8; 0x220];
        proms[32..288].fill(0x00);
        proms[288..352].fill(0x00);
        sys.board.load_proms(&proms);
        // Background tile (group 0) over visible row 0's first cell. Row 0
        // is raster line 16, tile row 2, so the cell is 64, not 0: color
        // 0x10 selects code bit 8 = 0, color 0, no flip.
        sys.bus_write(BusMaster::Cpu(0), 0x8040, 0x10);
        sys.bus_write(BusMaster::Cpu(0), 0x8440, 0x00);
        // Tiles ROM is zero, so code 0 decodes all pixel 0: pen 0 renders
        // palette[16] (white). Poke a live pixel instead: with no tile ROM
        // the pen is still white either way.
        let colorram = sys.board.map.region_data(Region::ColorRam).to_vec();
        let videoram = sys.board.map.region_data(Region::VideoRam).to_vec();
        let subram = sys.board.map.region_data(Region::SubRam).to_vec();
        sys.board
            .video
            .render_scanline(0, &colorram, &videoram, &subram[0x40..0x100]);
        let mut frame = vec![0u8; NATIVE_WIDTH * NATIVE_HEIGHT * 3];
        sys.board.video.render_frame(&mut frame);
        assert_eq!(&frame[0..3], &[255, 255, 247], "bg tile renders white");
        // Sprite 47 (lowest priority) over the same pixels: entry at the end
        // of the list, x = 0, y top = 16 (byte3 = 225), code 0 bank 0.
        sys.bus_write(BusMaster::Cpu(1), 0x40FC, 0x00);
        sys.bus_write(BusMaster::Cpu(1), 0x40FD, 0x00);
        sys.bus_write(BusMaster::Cpu(1), 0x40FE, 0x00);
        sys.bus_write(BusMaster::Cpu(1), 0x40FF, 225);
        // Sprite ROM is zero, so pixels are 0 (transparent): the tile shows
        // through. The write path is what this exercises.
        let subram = sys.board.map.region_data(Region::SubRam).to_vec();
        sys.board
            .video
            .render_scanline(0, &colorram, &videoram, &subram[0x40..0x100]);
        sys.board.video.render_frame(&mut frame);
        assert_eq!(
            &frame[0..3],
            &[255, 255, 247],
            "transparent sprite keeps tile"
        );
    }

    #[test]
    fn declares_native_dims_and_rot90() {
        use phosphor_core::core::machine::{Orientation, Renderable};
        let sys = GyrussSystem::new();
        assert_eq!(sys.display_size(), (256, 224));
        // Native landscape framebuffer; the frontend applies ROT90 to present
        // the portrait tube, composing to ROT270 under cocktail flip.
        assert_eq!(sys.orientation(), Orientation::ROT90);
        assert!(sys.orientation().swaps_axes());
        let mut flipped = GyrussSystem::new();
        flipped.board.video.set_flip(true);
        assert_eq!(
            flipped.orientation(),
            Orientation::ROT90.compose(Orientation::COCKTAIL)
        );
    }
}
