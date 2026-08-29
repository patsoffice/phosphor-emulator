//! Data East btime board (shared hardware).
//!
//! `BtimeBoard` models the hardware common to every game on Data East's
//! btime family (Burgertime, Bump'n'Jump, Lock'n'Chase, Zoar,
//! Disco No.1, …). Per-game wrappers (see `burgertime.rs`) own a `board`
//! field plus a [`BtimeConfig`] describing the variation points and forward
//! the `MachineCore`/capability traits to the board.
//!
//! Fully implemented: the main DECO CPU-7 (an NMOS 6502 with runtime opcode
//! encryption), the memory map, GFX decode, the `BGR_233_inverted` palette, the
//! char/sprite/background renderer (ROT270), inputs, DIP banks, the live VBLANK
//! bit, the coin IRQ, the frame loop, and the sound subsystem (a second M6502 @
//! 500 kHz driving two AY-3-8910s @ 1.5 MHz, with the command latch IRQ and the
//! scanline-gated NMI). The game boots, plays, and has sound.

use phosphor_core::audio::DcBlocker;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{
    Bus, BusMaster, ClockDomainName as Clk, ClockTree, DomainId, TimingConfig,
};
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::device::ay8910::Ay8910;
use phosphor_core::gfx::decode::{GfxCache, GfxLayout, decode_gfx};
use phosphor_core::gfx::pal_nbit;
use phosphor_macros::{BusDebug, MemoryRegion, Saveable};

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
enum Region {
    /// Main-CPU program ROM at 0xB000-0xFFFF (0xB000-0xBFFF is an unused gap;
    /// the physical ROMs sit at 0xC000-0xFFFF with the vectors at 0xFFFA-0xFFFF).
    Main = 1,
    /// Sound-CPU program ROM at 0xE000-0xEFFF, mirrored to 0xF000-0xFFFF
    /// (vectors read from 0xFFFA-0xFFFF).
    SoundRom = 2,
}

/// DECO CPU-7 opcode deobfuscation: bit-permute the fetched byte.
///
/// The moving bits form one 5-cycle (2→3→5→6→7→2); bits 0/1/4 pass through.
/// Applied to an opcode fetch that (a) follows a main-CPU write and (b) sits at
/// an address where `(addr & 0x0104) == 0x0104`.
#[inline]
fn deco_cpu7_decrypt(v: u8) -> u8 {
    ((v >> 6) & 1) << 7
        | ((v >> 5) & 1) << 6
        | ((v >> 3) & 1) << 5
        | ((v >> 4) & 1) << 4
        | ((v >> 2) & 1) << 3
        | ((v >> 7) & 1) << 2
        | ((v >> 1) & 1) << 1
        | (v & 1)
}

// ---------------------------------------------------------------------------
// GFX layouts. The 3bpp planar ROM orders its planes so that plane 0 is the
// most-significant pixel bit; phosphor's GfxLayout is LSB-first
// (plane_offsets[0] -> pixel bit 0), so the three plane offsets are reversed.
// ---------------------------------------------------------------------------

/// Characters: gfx1, 8×8, 3bpp planar, 1024 tiles.
const CHAR_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 0x2000 * 8, 0x4000 * 8],
    x_offsets: &[0, 1, 2, 3, 4, 5, 6, 7],
    y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
    char_increment: 8 * 8,
};

/// Sprites: gfx1, 16×16, 3bpp planar, 256 tiles. The two 8-pixel halves of each
/// row are stored 16 bytes apart (x offsets 128..135 then 0..7).
const SPRITE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 0x2000 * 8, 0x4000 * 8],
    x_offsets: &[
        128, 129, 130, 131, 132, 133, 134, 135, 0, 1, 2, 3, 4, 5, 6, 7,
    ],
    y_offsets: &[
        0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104, 112, 120,
    ],
    char_increment: 32 * 8,
};

/// Background tiles: gfx2 (0x1800), 16×16, 3bpp planar, 64 tiles (same layout as
/// the sprites but over the smaller region, so the plane thirds are 0/0x800/0x1000).
const BG_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 0x0800 * 8, 0x1000 * 8],
    x_offsets: &[
        128, 129, 130, 131, 132, 133, 134, 135, 0, 1, 2, 3, 4, 5, 6, 7,
    ],
    y_offsets: &[
        0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104, 112, 120,
    ],
    char_increment: 32 * 8,
};

const NUM_CHARS: usize = 1024;
const NUM_SPRITES: usize = 256;
const NUM_BG_TILES: usize = 64;

// Video draws into a native 256×256 palette-index buffer; the visible window is
// the [8,248) square (horizontal and vertical blank end/start = 8/248), cropped
// to 240×240 and rotated ROT270. Background tiles use palette entries 8..15
// (color base 8); chars/sprites use 0..7.
//
// The square raster is displayed on a 4:3 tube rotated to portrait, so the final
// display is 3:4. The framebuffer stays native 240×240 (square pixels); the
// frontend stretches it to the 3:4 presentation via TIMING.display_aspect.
const NATIVE_DIM: usize = 256;
const CROP_LO: usize = 8;
const VISIBLE_DIM: usize = 240;
const BG_PALETTE_BASE: usize = 8;

/// Size in bytes of the display framebuffer (RGB24 at the presentation size).
const fn display_bytes() -> usize {
    let (w, h) = TIMING.display_size();
    (w * h * 3) as usize
}

/// AY-3-8910 chip clock: 12 MHz / 2 / 2 / 2 = 1.5 MHz (equal to the main clock,
/// so each chip is ticked once per main tick).
const AY_CLOCK_HZ: u64 = 1_500_000;

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------
// Main CPU: 12 MHz / 2 / 2 / 2 = 1.5 MHz.
// Screen: pixel clock 6 MHz, HTOTAL 384, VTOTAL 272,
// visible 240x240, orientation ROT270. Frame rate: 6e6 / (384 * 272) ≈ 57.44 Hz.
// CPU cycles per scanline: 1_500_000 / (57.44 * 272) ≈ 96.
pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 1_500_000, // 12 MHz / 8
    cycles_per_scanline: 96, // HTOTAL 384 pixel clocks / 4
    total_scanlines: 272,    // VTOTAL
    // Native square raster (240×240); the 4:3 tube is rotated to portrait, so
    // the frontend presents it 3:4 via display_aspect (no baked CPU stretch).
    display_width: 240,
    display_height: 240,
    display_aspect: Some((3, 4)),
};

/// The board's crystal and everything divided out of it.
///
/// One 12 MHz crystal: the main 6502 at /8, the pixel clock at /2, both
/// AY-3-8910s at the main CPU's own 1.5 MHz, and the sound 6502 at /24.
///
/// That last one is the divider the board's `1/3` of the main CPU implies:
/// 12 MHz over 24 is 500 kHz exactly, and 500000/1500000 reduces to 1/3. The
/// chain through the counter that produces it is not documented in this file,
/// but the division is a whole number either way.
pub fn clock_tree() -> ClockTree {
    use phosphor_core::core::RootId;
    let mut t = ClockTree::new(12_000_000);
    let cpu = t.add_domain(Clk::Cpu, RootId::MAIN, 1, 8); // 1.5 MHz
    let dot = t.add_domain(Clk::Pixel, RootId::MAIN, 1, 2); // 6 MHz
    t.add_domain(Clk::Psg, RootId::MAIN, 1, 8); // AY-3-8910 at 1.5 MHz
    t.add_domain(Clk::SoundCpu, RootId::MAIN, 1, 24); // sound 6502 at 500 kHz
    t.set_step_domain(cpu);
    // Both clocks come off the same crystal in a 4:1 ratio, so 384 dot clocks
    // is exactly 96 CPU cycles.
    t.set_raster(dot, 384, 0);
    t
}

// ---------------------------------------------------------------------------
// Per-game configuration
// ---------------------------------------------------------------------------

/// Per-game configuration for the shared btime board.
///
/// Only Burgertime's variant is implemented in this pass. Sibling games differ
/// in their opcode encryption (DECO CPU-7 vs. CPU-6/222 vs. none), palette
/// source (decoded `BGR_233_inverted` vs. PROM), background-tilemap presence,
/// and audio NMI wiring; those become fields here as each sibling is added.
pub struct BtimeConfig {
    /// Machine id — also the save-state tag and CLI name.
    pub name: &'static str,
}

// ---------------------------------------------------------------------------
// BtimeBoard
// ---------------------------------------------------------------------------

/// Shared Data East btime hardware (Burgertime configuration in pass 1).
///
/// Memory map (main CPU):
///   0x0000-0x07FF  Work RAM
///   0x0C00-0x0C0F  Palette RAM (16 entries)
///   0x1000-0x13FF  Video RAM (char codes; sprite RAM aliased into column 0)
///   0x1400-0x17FF  Color RAM
///   0x1800-0x1BFF  Video RAM via X/Y-swap mirror
///   0x1C00-0x1FFF  Color RAM via X/Y-swap mirror
///   0x4000         IN0 (P1)      0x4001  IN1 (P2)      0x4002  system
///   0x4003         DSW1 (bit7 = live VBLANK)           0x4004  DSW2
///   0xB000-0xFFFF  Program ROM
/// One CPU cycle: the main 6502, the sound 6502 on its divider, then the
/// PSGs and the clock.
///
/// The CPUs live on the machine and the board *is* the bus, so this takes them
/// as separate borrows and dispatches at a concrete type.
///
/// This is the debugger's path: it tests the frame position on every cycle so
/// that single-stepping still crosses scanline boundaries. A whole frame goes
/// through [`run_scanlines`], which hoists that test out.
#[inline]
pub fn tick(cpu: &mut M6502, sound_cpu: &mut M6502, board: &mut BtimeBoard) {
    let frame_cycle = board.clock % TIMING.cycles_per_frame();
    if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
        board.begin_scanline(frame_cycle / TIMING.cycles_per_scanline);
    }
    step_cycle(cpu, sound_cpu, board);
}

/// The part of a cycle with no frame-position test in it.
#[inline]
fn step_cycle(cpu: &mut M6502, sound_cpu: &mut M6502, board: &mut BtimeBoard) {
    // Main CPU @ 1.5 MHz.
    board.begin_main_cycle(cpu);
    cpu.execute_cycle(board, BusMaster::Cpu(0));

    // Sound CPU @ 500 kHz (main / 3).
    if board.clocks.tick(board.sound_dom) {
        board.latch_sound_pc(sound_cpu);
        sound_cpu.execute_cycle(board, BusMaster::Cpu(1));
    }

    board.end_cycle();
}

/// Run `cycles` CPU cycles, scanline-outer and cycle-inner. The caller must
/// start on a scanline boundary and pass a multiple of `cycles_per_scanline`;
/// the debugger's off-boundary stepping goes through [`tick`] instead.
pub fn run_scanlines(cpu: &mut M6502, sound_cpu: &mut M6502, board: &mut BtimeBoard, cycles: u64) {
    debug_assert!(
        board.clock.is_multiple_of(TIMING.cycles_per_scanline)
            && cycles.is_multiple_of(TIMING.cycles_per_scanline),
        "run_scanlines must start on a scanline boundary and run whole scanlines"
    );
    for _ in 0..cycles / TIMING.cycles_per_scanline {
        let scanline = board.clock % TIMING.cycles_per_frame() / TIMING.cycles_per_scanline;
        board.begin_scanline(scanline);
        for _ in 0..TIMING.cycles_per_scanline {
            step_cycle(cpu, sound_cpu, board);
        }
    }
}

/// Run one frame's worth of cycles. Whole scanlines go through
/// [`run_scanlines`]; a partial scanline at either end (only after the debugger
/// has left the clock off-boundary) goes through [`tick`].
pub fn run_frame(cpu: &mut M6502, sound_cpu: &mut M6502, board: &mut BtimeBoard) {
    let scanline = TIMING.cycles_per_scanline;
    let mut remaining = TIMING.cycles_per_frame();

    let lead = ((scanline - board.clock % scanline) % scanline).min(remaining);
    for _ in 0..lead {
        tick(cpu, sound_cpu, board);
    }
    remaining -= lead;

    let whole = remaining - remaining % scanline;
    run_scanlines(cpu, sound_cpu, board, whole);
    remaining -= whole;

    for _ in 0..remaining {
        tick(cpu, sound_cpu, board);
    }
}

// The board is the bus for every machine on it: they differ only in the
// per-game config the board already carries.
impl Bus for BtimeBoard {
    type Address = u16;
    type Data = u8;

    #[inline]
    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        self.bus_read(master, addr)
    }

    #[inline]
    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.bus_write(master, addr, data);
    }

    #[inline]
    fn is_halted_for(&self, master: BusMaster) -> bool {
        self.bus_is_halted_for(master)
    }

    #[inline]
    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        self.bus_check_interrupts(target)
    }
}

#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
// No `save_after_load` hook. There used to be one, to seed the per-row
// `bnj_scroll0` samples and redraw; both are gone now that the picture is drawn
// row by row as the beam passes. The framebuffer holds whatever was last drawn
// until the next frame's rows overwrite it, which is the same contract
// `gottlieb`'s index buffer has.
pub struct BtimeBoard {
    /// Both maps hold only ROM, so what they persist is their page layout
    /// rather than any bytes: this board keeps its memory in plain fields
    /// below, not in the address space.
    #[debug_map(cpu = 0)]
    #[save(id = 1)]
    pub(crate) main_map: AddressSpace16,
    #[debug_map(cpu = 1)]
    #[save(id = 2)]
    pub(crate) sound_map: AddressSpace16,

    // Sound subsystem (sound CPU @ 500 kHz; two AY-3-8910 @ 1.5 MHz).
    #[debug_device("AY-3-8910 #1")]
    #[save(id = 3)]
    ay1: Ay8910,
    #[debug_device("AY-3-8910 #2")]
    #[save(id = 4)]
    ay2: Ay8910,
    /// The PSGs' coupling into the amplifier.
    ///
    /// These chips are UNIPOLAR: a channel contributes its level while it is
    /// enabled and nothing while it is not, so the pin swings from ground up
    /// rather than either side of it. Summed, the two of them put the output on
    /// a DC offset of +0.198 of full scale under recorded play, on audio that
    /// was otherwise healthy and never clipped.
    ///
    /// The chip is not modelled wrongly; its output really is unipolar, and so
    /// is its reference implementation's. What was missing is the analog side
    /// between the chips and the speaker, which no board using this part can do
    /// without: a loudspeaker cannot reproduce DC and the amplifier would sit
    /// off centre. The exact capacitor is not on a schematic to hand, so the
    /// corner is the shared default, which is honest for a part whose only job
    /// is to remove an offset.
    #[save(id = 5)]
    ay_coupling: DcBlocker,
    #[save(id = 6)]
    sound_ram: [u8; 0x0400],
    #[save(id = 7)]
    sound_irq: bool, // set on main write to 0x4003, cleared on 0xA000 read
    #[save(id = 8)]
    audio_nmi_enable: bool, // 0xC000 write bit0; ANDs with scanline bit3 -> NMI
    /// The board's clock tree, as [`clock_tree`] declares it.
    #[debug_device("Clocks")]
    #[save(id = 9)]
    clocks: ClockTree,
    /// A handle into the clock tree, which is itself saved.
    #[save_skip]
    sound_dom: DomainId,

    // Work / video memory (kept as flat arrays, not in the AddressSpace16).
    #[save(id = 10)]
    ram: [u8; 0x0800],
    #[save(id = 11)]
    videoram: [u8; 0x0400],
    #[save(id = 12)]
    colorram: [u8; 0x0400],
    #[save(id = 13)]
    palette_ram: [u8; 16],
    /// RGB expansion of `palette_ram`, rebuilt on every palette write and saved
    /// beside the RAM it comes from rather than rebuilt after a load.
    #[save(id = 14)]
    palette_rgb: [(u8, u8, u8); 16],

    // Decoded graphics (derived from ROM at load; not saved). Consumed by the
    // renderer.
    #[save_skip]
    chars: GfxCache, // 8×8×3, 1024 tiles (gfx1)
    #[save_skip]
    sprites: GfxCache, // 16×16×3, 256 tiles (gfx1)
    #[save_skip]
    bg_tiles: GfxCache, // 16×16×3, 64 tiles (gfx2)
    #[save_skip]
    bg_map: [u8; 0x0800], // background tilemap selector ROM

    /// Display framebuffer (native 240×240 RGB, square pixels), filled one row
    /// at a time as the beam reaches each visible scanline.
    ///
    /// A row therefore holds what the beam drew on that line, out of the video
    /// RAM, color RAM, `bnj_scroll0`, flip latch and palette as they stood at
    /// that line's boundary. Burger Time writes `bnj_scroll0` *during active
    /// display*, measured on this ROM set at scanlines 88, 91 and 201, all
    /// inside the visible 8..248 window, in both the attract loop and real play,
    /// so rows above such a write genuinely differ from rows below it.
    ///
    /// Consequence worth knowing: the picture a completed frame presents does
    /// *not* contain that frame's own vblank writes, because the beam had
    /// already passed. The whole-frame render this replaced did contain them.
    ///
    /// Derived output, so not saved. It is not seeded after a load either: the
    /// rows of the next frame overwrite every one of them, and there is no
    /// moment a load could reconstruct a picture from.
    #[save_skip]
    pub(crate) framebuffer: Vec<u8>,

    // DECO CPU-7 decryption state: any main-CPU write arms decryption of the
    // next opcode fetch (consumed in `bus_read`).
    #[save(id = 15)]
    main_had_written: bool,
    /// The main CPU's SYNC pin, sampled once per cycle by `begin_main_cycle`.
    /// A reset CPU sits in Fetch, so this starts asserted. Not saved: it is
    /// re-derived from the CPU before every cycle that could read the bus.
    #[save_skip]
    main_is_sync: bool,

    // I/O latches
    #[save(id = 16)]
    pub(crate) main_irq: bool, // coin-insertion IRQ (HOLD_LINE approximation)
    #[save(id = 17)]
    flip_screen: bool, // 0x4002 write bit0
    #[save(id = 18)]
    bnj_scroll0: u8, // 0x4004 write (bit4 -> background enable)

    #[save(id = 19)]
    sound_latch: u8, // 0x4003 write — stored; sound CPU/IRQ deferred (§10)

    // Input ports (active-low players, active-high coins) and DIP banks.
    // Mutated directly by the wrapper's `handle_input` (same-crate access, per
    // the joust.rs pattern).
    #[save(id = 20)]
    pub(crate) p1: u8,
    #[save(id = 21)]
    pub(crate) p2: u8,
    #[save(id = 22)]
    pub(crate) system: u8,
    #[save(id = 23)]
    pub(crate) dsw1: u8, // bits 0-6 are DIPs; bit 7 is the live VBLANK (injected on read)
    #[save(id = 24)]
    pub(crate) dsw2: u8,

    /// Per-game configuration (identity + future variation points), fixed at
    /// construction.
    #[save_skip]
    config: BtimeConfig,

    #[save(id = 25)]
    clock: u64,
}

impl BtimeBoard {
    /// X/Y address swap over the 32×32 video/color window.
    /// An involution: `off = 32*y + x  ->  32*x + y`. Backs the 0x1800/0x1C00
    /// sprite-RAM mirror (the game reaches column-0 sprite RAM through it).
    #[inline]
    pub(crate) fn swap(off: usize) -> usize {
        let x = off / 32;
        let y = off % 32;
        32 * y + x
    }

    pub fn new(config: BtimeConfig) -> Self {
        let clocks = clock_tree();
        let sound_dom = clocks.find(Clk::SoundCpu).expect("declared sound domain");
        let mut main_map = AddressSpace16::new();
        main_map.region(
            Region::Main,
            "Program ROM",
            0xB000,
            0x5000,
            AccessKind::ReadOnly,
        );

        let mut sound_map = AddressSpace16::new();
        sound_map
            .region(
                Region::SoundRom,
                "Sound ROM",
                0xE000,
                0x1000,
                AccessKind::ReadOnly,
            )
            .mirror(0xF000, 0xE000, 0x1000);

        let mut board = Self {
            main_map,
            sound_map,
            ay1: Ay8910::new(AY_CLOCK_HZ),
            ay2: Ay8910::new(AY_CLOCK_HZ),
            ay_coupling: DcBlocker::new(phosphor_core::audio::host_sample_rate()),
            sound_ram: [0; 0x0400],
            sound_irq: false,
            audio_nmi_enable: false,
            clocks,
            sound_dom,
            ram: [0; 0x0800],
            videoram: [0; 0x0400],
            colorram: [0; 0x0400],
            palette_ram: [0; 16],
            palette_rgb: [(0, 0, 0); 16],
            chars: GfxCache::new(NUM_CHARS, 8, 8),
            sprites: GfxCache::new(NUM_SPRITES, 16, 16),
            bg_tiles: GfxCache::new(NUM_BG_TILES, 16, 16),
            bg_map: [0; 0x0800],
            framebuffer: vec![0u8; display_bytes()],
            main_had_written: false,
            main_is_sync: true,
            main_irq: false,
            flip_screen: false,
            bnj_scroll0: 0,
            sound_latch: 0,
            // Players idle (active-low = all bits high).
            p1: 0xFF,
            p2: 0xFF,
            // Start/tilt idle high (bits 0-2), coin bits (6-7) low.
            system: 0x07,
            // DSW1: Coin A/B 1C/1C (0x03|0x0c), "Leave Off" bit4 set (required or
            // boot locks), Upright. Bit 7 excluded (live VBLANK, injected on read).
            dsw1: 0x1F,
            // DSW2: 3 lives, 20000 bonus, 4 enemies, end-of-level pepper on.
            dsw2: 0x0B,
            config,
            clock: 0,
        };
        board.rebuild_palette();
        // No initial render: the framebuffer stays black until the beam draws
        // its first frame, because there is no moment before that to draw.
        board
    }

    /// Machine id (identity comes from the per-game [`BtimeConfig`]).
    pub fn machine_id(&self) -> &str {
        self.config.name
    }

    /// Load the assembled main-CPU program ROM (region base 0xB000; the physical
    /// ROMs occupy 0xC000-0xFFFF, so `data` is 0x5000 bytes with a 0x1000 gap).
    pub fn load_main_rom(&mut self, data: &[u8]) {
        self.main_map.load_region(Region::Main, data);
    }

    /// Decode the gfx1 region into the char (8×8) and sprite (16×16) caches.
    pub fn load_gfx1(&mut self, gfx1: &[u8]) {
        self.chars = decode_gfx(gfx1, 0, NUM_CHARS, &CHAR_LAYOUT);
        self.sprites = decode_gfx(gfx1, 0, NUM_SPRITES, &SPRITE_LAYOUT);
    }

    /// Decode the gfx2 region into the background-tile cache (16×16).
    pub fn load_gfx2(&mut self, gfx2: &[u8]) {
        self.bg_tiles = decode_gfx(gfx2, 0, NUM_BG_TILES, &BG_LAYOUT);
    }

    /// Copy the background tilemap selector ROM (bg_map).
    pub fn load_bg_map(&mut self, data: &[u8]) {
        let n = data.len().min(self.bg_map.len());
        self.bg_map[..n].copy_from_slice(&data[..n]);
    }

    /// Load the sound-CPU program ROM (0x1000 bytes at 0xE000, mirrored to 0xF000).
    pub fn load_sound_rom(&mut self, data: &[u8]) {
        self.sound_map.load_region(Region::SoundRom, data);
    }

    // --- Decoded-graphics / palette accessors (used by the `.4` renderer) ---

    pub fn chars(&self) -> &GfxCache {
        &self.chars
    }
    pub fn sprites(&self) -> &GfxCache {
        &self.sprites
    }
    pub fn bg_tiles(&self) -> &GfxCache {
        &self.bg_tiles
    }
    pub fn bg_map(&self) -> &[u8] {
        &self.bg_map
    }

    /// Decoded char/bg/sprite sheets for the interactive GFX viewer
    /// (`--gfxview`). A board method so it can borrow the private `palette_rgb`.
    pub(crate) fn gfx_sheets(&self) -> Vec<phosphor_core::core::machine::GfxSheet<'_>> {
        use phosphor_core::core::machine::GfxSheet;
        vec![
            GfxSheet {
                name: "chars",
                cache: &self.chars,
                palette: &self.palette_rgb,
            },
            GfxSheet {
                name: "bg",
                cache: &self.bg_tiles,
                palette: &self.palette_rgb,
            },
            GfxSheet {
                name: "sprites",
                cache: &self.sprites,
                palette: &self.palette_rgb,
            },
        ]
    }

    /// Recompute one palette entry from `palette_ram` using the DECO
    /// `BGR_233_inverted` decode (invert, then R=bits0-2, G=bits3-5, B=bits6-7).
    fn update_palette_entry(&mut self, i: usize) {
        let v = !self.palette_ram[i & 0x0F];
        let r = pal_nbit(v & 7, 3);
        let g = pal_nbit((v >> 3) & 7, 3);
        let b = pal_nbit((v >> 6) & 3, 2);
        self.palette_rgb[i & 0x0F] = (r, g, b);
    }

    /// Recompute all 16 palette entries (after construction / load_state).
    fn rebuild_palette(&mut self) {
        for i in 0..16 {
            self.update_palette_entry(i);
        }
    }

    // --- Core tick ---

    /// Sample the main CPU state the bus needs for the coming cycle: the SYNC
    /// pin (which DECO CPU-7 decryption keys off) and, when the debugger is
    /// attached, the PC for access attribution. SYNC is stable across the whole
    /// cycle -- the 6502 only leaves Fetch after the opcode read completes -- so
    /// sampling it here sees exactly what the bus would see mid-read.
    fn begin_main_cycle(&mut self, cpu: &M6502) {
        self.main_is_sync = cpu.is_sync();
        if self.main_map.debug_active() {
            let pc = cpu.at_instruction_boundary().then_some(cpu.pc as u32);
            self.main_map.latch_access_context(self.clock, pc);
        }
    }

    /// Latch the sound CPU's PC before its cycle.
    fn latch_sound_pc(&mut self, sound_cpu: &M6502) {
        if self.sound_map.debug_active() {
            let pc = sound_cpu
                .at_instruction_boundary()
                .then_some(sound_cpu.pc as u32);
            self.sound_map.latch_access_context(self.clock, pc);
        }
    }

    /// Work that only happens on the first cycle of a scanline: sampling
    /// `bnj_scroll0` for the row the beam is about to draw.
    ///
    /// `scanline` is 0..272; the visible window is 8..248, the same one
    /// [`in_vblank`](Self::in_vblank) derives the VBLANK bit from. Only visible
    /// lines have a row to composite.
    ///
    /// Called once per scanline from [`run_scanlines`] and, for the debugger's
    /// single-step path, from [`tick`] when the clock lands on a boundary.
    /// Public because the picture only exists once the beam has passed over it:
    /// a caller that wants a frame without running CPU cycles (the integration
    /// tests) has to step the beam itself. Vblank lines are ignored here, so
    /// walking `0..total_scanlines` draws exactly the visible rows.
    pub fn begin_scanline(&mut self, scanline: u64) {
        let lo = CROP_LO as u64;
        if (lo..lo + VISIBLE_DIM as u64).contains(&scanline) {
            self.render_scanline(scanline as usize);
        }
    }

    /// Board work after the CPUs' cycle: the PSGs and the clock.
    fn end_cycle(&mut self) {
        // Both AY-3-8910s @ 1.5 MHz (once per main tick).
        self.ay1.tick();
        self.ay2.tick();

        self.clock += 1;

        // No frame-boundary render here any more: each visible row is drawn at
        // its own scanline boundary in `begin_scanline`, which both
        // `run_scanlines` and the debugger's `tick` reach.
    }

    pub fn reset(&mut self) {
        self.main_had_written = false;
        self.main_is_sync = true;
        self.main_irq = false;
        self.sound_irq = false;
        self.audio_nmi_enable = false;
        self.clocks.reset();
        self.ay1.reset();
        self.ay2.reset();
        self.clock = 0;
        // The framebuffer is not cleared: the next frame's rows overwrite every
        // one of them as the beam reaches them.
        // The CPUs live on the machine, which resets them against this board.
    }

    /// Current scanline (0-271) within the frame.
    fn current_scanline(&self) -> u64 {
        (self.clock % TIMING.cycles_per_frame()) / TIMING.cycles_per_scanline
    }

    /// True during vertical blanking — the current scanline is outside the
    /// visible [8, 248) window. The game polls this on the 0x4003 read.
    fn in_vblank(&self) -> bool {
        !(8..248).contains(&self.current_scanline())
    }

    /// Sound-CPU NMI line: the audio NMI enable ANDed with scanline bit 3 (the
    /// "8vck" timer). The M6502 edge-detects, so this fires once per rising edge.
    fn sound_nmi_asserted(&self) -> bool {
        self.audio_nmi_enable && ((self.current_scanline() >> 3) & 1) != 0
    }

    /// Returns a bitmask of CPUs at instruction boundaries. Bit 0 = main CPU,
    /// bit 1 = sound CPU. The CPUs live on the machine, which passes them in.
    pub fn instruction_boundaries(cpu: &M6502, sound_cpu: &M6502) -> u32 {
        u32::from(cpu.at_instruction_boundary())
            | (u32::from(sound_cpu.at_instruction_boundary()) << 1)
    }

    // --- Capability-trait helpers (called by the game wrapper) ---

    /// Copy the latest framebuffer into the frontend's `buffer`.
    pub fn render_frame(&self, buffer: &mut [u8]) {
        buffer.copy_from_slice(&self.framebuffer);
    }

    /// Drain both AY-3-8910s and mix them into `buffer` (mono, 44.1 kHz). Both
    /// chips are ticked identically, so they produce the same sample count.
    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        let n1 = self.ay1.fill_audio(buffer);
        let mut tmp = vec![0i16; n1];
        let n2 = self.ay2.fill_audio(&mut tmp);
        for (out, &s) in buffer.iter_mut().zip(tmp.iter()).take(n1.min(n2)) {
            *out = out.saturating_add(s);
        }
        // Coupled after the sum, because the capacitor sits between the chips
        // and the amplifier rather than inside either chip.
        for s in buffer.iter_mut().take(n1) {
            *s = self
                .ay_coupling
                .process(*s as f32)
                .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        }
        n1
    }

    /// Draw one visible scanline into the framebuffer, out of the video state as
    /// it stands at that line's boundary.
    ///
    /// `y` is a native row in `[CROP_LO, CROP_LO + VISIBLE_DIM)`; native row `n`
    /// is drawn during scanline `n`, and lands at output row `n - CROP_LO`.
    ///
    /// The layers run in the board's order for this one row. `bnj_scroll0` bit 4
    /// gates the background and, with it, whether the chars are drawn
    /// transparently over it or opaquely over the backdrop, so a mid-screen
    /// write to that register changes both layers from that row down. Sprites do
    /// not depend on the register and are drawn last either way.
    ///
    /// The palette is read here too, so a palette write partway down the screen
    /// colors only the rows below it, the same way `gottlieb` does it.
    ///
    /// The ROT270 the cabinet needs is declared via
    /// [`orientation`](Self::orientation) and applied centrally by the frontend,
    /// so this emits pixels in native row-major order.
    fn render_scanline(&mut self, y: usize) {
        // One native scanline of palette indices, backdrop (pen 0) first.
        let mut row = [0u8; NATIVE_DIM];

        let ctrl = self.bnj_scroll0;
        if ctrl & 0x10 != 0 {
            self.draw_background_row(&mut row, ctrl, y);
            self.draw_chars_row(&mut row, true, y);
        } else {
            self.draw_chars_row(&mut row, false, y);
        }
        self.draw_sprites_row(&mut row, y);

        // Crop to the visible [8,248) columns and resolve against the palette as
        // it stands now.
        let mask = self.palette_rgb.len() - 1;
        let out = (y - CROP_LO) * VISIBLE_DIM * 3;
        for x in 0..VISIBLE_DIM {
            let (r, g, b) = self.palette_rgb[row[x + CROP_LO] as usize & mask];
            let di = out + x * 3;
            self.framebuffer[di] = r;
            self.framebuffer[di + 1] = g;
            self.framebuffer[di + 2] = b;
        }
    }

    /// BurgerTime's monitor is mounted rotated 270° clockwise. The orientation
    /// is declarative — the frontend rotates `render_frame`'s native output.
    pub fn orientation(&self) -> phosphor_core::core::machine::Orientation {
        phosphor_core::core::machine::Orientation::ROT270
    }

    /// Blit the one line of tile `code` that crosses native row `y` into `row`,
    /// for a tile whose top-left corner is at (`sx`,`sy`).
    ///
    /// Clips to the 256-pixel native width. `transparent` skips pen 0;
    /// otherwise every pixel (including 0) is written. Final index is
    /// `pal_base + pixel`. A tile that does not cross `y` writes nothing, which
    /// is how the row passes below reject the tiles that are not on this line.
    #[allow(clippy::too_many_arguments)]
    fn blit_tile_row(
        row: &mut [u8; NATIVE_DIM],
        cache: &GfxCache,
        code: usize,
        sx: i32,
        sy: i32,
        flipx: bool,
        flipy: bool,
        pal_base: usize,
        transparent: bool,
        y: usize,
    ) {
        if code >= cache.count() {
            return;
        }
        let w = cache.width();
        let h = cache.height();
        let ty = y as i32 - sy;
        if !(0..h as i32).contains(&ty) {
            return;
        }
        let py = if flipy {
            h - 1 - ty as usize
        } else {
            ty as usize
        };
        for tx in 0..w {
            let dx = sx + tx as i32;
            if !(0..NATIVE_DIM as i32).contains(&dx) {
                continue;
            }
            let px = if flipx { w - 1 - tx } else { tx };
            let pixel = cache.pixel(code, px, py);
            if transparent && pixel == 0 {
                continue;
            }
            row[dx as usize] = (pal_base + pixel as usize) as u8;
        }
    }

    /// Chars: 32×32 grid, `code = videoram[off] + 256*(colorram[off] & 3)`,
    /// transposed `x = 31 - off/32`, `y = off % 32`.
    ///
    /// Only the one grid row that covers native row `y` is visited, 32 cells
    /// rather than all 1024. The cell's top edge is `8 * (off % 32)`, or
    /// `8 * (31 - off % 32)` flipped, so the grid row is fixed by `y` and only
    /// the column varies. Cells are still visited in ascending column order,
    /// which is the order the whole-frame pass used within a row, though chars
    /// tile without overlap so it cannot matter.
    fn draw_chars_row(&self, row: &mut [u8; NATIVE_DIM], transparent: bool, y: usize) {
        let cy = if self.flip_screen { 31 - y / 8 } else { y / 8 };
        for col in 0..32usize {
            let off = col * 32 + cy;
            let mut x = 31 - col;
            if self.flip_screen {
                x = 31 - x;
            }
            let code = self.videoram[off] as usize + 256 * (self.colorram[off] as usize & 3);
            Self::blit_tile_row(
                row,
                &self.chars,
                code,
                8 * x as i32,
                8 * cy as i32,
                self.flip_screen,
                self.flip_screen,
                0,
                transparent,
                y,
            );
        }
    }

    /// Sprites: 8 hardware sprites, attributes interleaved 0x20 apart in video
    /// RAM; drawn twice for ±256 wrap.
    ///
    /// All eight are visited per row and `blit_tile_row` rejects the ones that
    /// are not on this line: eight entries is not worth an index for.
    ///
    /// The list is read as of this row. The board's map RAM is walked per line
    /// into a line buffer that is displayed on the *next* line, so a sprite whose
    /// attributes change mid-screen would appear one row early here. That
    /// one-line lead is deliberately not modeled: `y -= 1` below is a position
    /// constant, and the epic's W3 note warns that folding the lead in on top of
    /// a constant that already carries it doubles the delay.
    fn draw_sprites_row(&self, row: &mut [u8; NATIVE_DIM], y: usize) {
        for i in 0..8 {
            let off = i * 0x80;
            if self.videoram[off] & 0x01 == 0 {
                continue;
            }
            let mut x = 240 - self.videoram[off + 0x60] as i32;
            let mut sy = 240 - self.videoram[off + 0x40] as i32;
            let mut flipx = self.videoram[off] & 0x04 != 0;
            let mut flipy = self.videoram[off] & 0x02 != 0;
            if self.flip_screen {
                x = 240 - x;
                sy = 240 - sy; // sprite_y_adjust_flip_screen = 0
                flipx = !flipx;
                flipy = !flipy;
            }
            sy -= 1; // sprite_y_adjust = 1
            let code = self.videoram[off + 0x20] as usize;
            Self::blit_tile_row(row, &self.sprites, code, x, sy, flipx, flipy, 0, true, y);
            // Wrap-around copy.
            let sy2 = sy + if self.flip_screen { -256 } else { 256 };
            Self::blit_tile_row(row, &self.sprites, code, x, sy2, flipx, flipy, 0, true, y);
        }
    }

    /// Background: up to 4 columns of 16×16 tiles selected from `bg_map`,
    /// horizontally scrolled by `(ctrl & 3) << 8`. The four column tiles
    /// cycle `start..start+3`, offset by `ctrl & 0x04`. `ctrl` is `bnj_scroll0`
    /// as it stood when this row was drawn.
    ///
    /// As for chars, only the one tile row covering native row `y` is visited:
    /// 16 tiles per 0x100 block rather than 256. Blocks are still visited in
    /// ascending order, which matters here because they are drawn opaquely and
    /// a later block overwrites an earlier one where the scroll overlaps them.
    fn draw_background_row(&self, row: &mut [u8; NATIVE_DIM], ctrl: u8, y: usize) {
        let Some(ty) = Self::bg_tile_row(y, self.flip_screen) else {
            return;
        };

        let mut start = if self.flip_screen { 0u8 } else { 1u8 };
        let mut tmap = [0u8; 4];
        for slot in tmap.iter_mut() {
            *slot = start | (ctrl & 0x04);
            start = (start + 1) & 0x03;
        }

        // The second scroll register is never written on this game, so it is 0.
        let mut scroll: i32 = -(((ctrl & 0x03) as i32) << 8);
        for i in 0..5 {
            if scroll > 256 {
                break;
            }
            if scroll >= -256 {
                let tileoffset = tmap[i & 3] as usize * 0x100;
                for col in 0..16usize {
                    let off = col * 16 + ty;
                    let mut x = 240 - (16 * col as i32 + scroll) - 1;
                    let mut sy = 16 * ty as i32;
                    if self.flip_screen {
                        x = 240 - x;
                        sy = 240 - sy;
                    }
                    let code = self.bg_map[tileoffset + off] as usize;
                    Self::blit_tile_row(
                        row,
                        &self.bg_tiles,
                        code,
                        x,
                        sy,
                        self.flip_screen,
                        self.flip_screen,
                        BG_PALETTE_BASE,
                        false,
                        y,
                    );
                }
            }
            scroll += 256;
        }
    }

    /// Which of the 16 tile rows in a background column block covers native row
    /// `y`, or `None` if none does.
    ///
    /// A tile's top edge is `16 * (off % 16)` unflipped, and `240 - 16 * (off %
    /// 16)` flipped. Unflipped that inverts to `y / 16`; flipped it is the one
    /// multiple of 16 in `[240 - y, 240 - y + 16)`, which is what the ceiling
    /// division computes. The flipped case can select a row outside `0..16`,
    /// hence the `Option`.
    fn bg_tile_row(y: usize, flip: bool) -> Option<usize> {
        let k = if flip {
            (240 - y as i32 + 15).div_euclid(16)
        } else {
            (y / 16) as i32
        };
        (0..16).contains(&k).then_some(k as usize)
    }

    // --- Bus (master-dispatched: Cpu(0) = main, Cpu(1) = sound) ---

    pub(crate) fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        if master == BusMaster::Cpu(1) {
            return self.sound_read(addr);
        }
        let a = addr as usize;
        let mut data = match addr {
            0x0000..=0x07FF => self.ram[a & 0x07FF],
            0x0C00..=0x0C0F => self.palette_ram[a & 0x0F],
            0x1000..=0x13FF => self.videoram[a & 0x03FF],
            0x1400..=0x17FF => self.colorram[a & 0x03FF],
            0x1800..=0x1BFF => self.videoram[Self::swap(a & 0x03FF)],
            0x1C00..=0x1FFF => self.colorram[Self::swap(a & 0x03FF)],
            0x4000 => self.p1,
            0x4001 => self.p2,
            0x4002 => self.system,
            // Bit 7 is the live VBLANK line (active-high), not a DIP.
            0x4003 => (self.dsw1 & 0x7F) | if self.in_vblank() { 0x80 } else { 0 },
            0x4004 => self.dsw2,
            0xB000..=0xFFFF => self.main_map.read_backing(addr),
            _ => 0,
        };

        // The coin IRQ is HOLD_LINE: the CPU vectoring through 0xFFFE (IRQ/BRK
        // vector low byte) acknowledges it, so exactly one IRQ fires per coin
        // edge even while the coin button is held.
        if addr == 0xFFFE {
            self.main_irq = false;
        }

        // DECO CPU-7: on an opcode fetch (is_sync) that follows any main-CPU
        // write, consume the "had written" flag and deobfuscate the fetched
        // byte when (addr & 0x0104) == 0x0104. The flag clears on every sync
        // fetch regardless of address; only matching addresses are decrypted.
        if self.main_is_sync && self.main_had_written {
            self.main_had_written = false;
            if (addr & 0x0104) == 0x0104 {
                data = deco_cpu7_decrypt(data);
            }
        }

        self.main_map.watch_read(0, master, addr, data);
        data
    }

    pub(crate) fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        if master == BusMaster::Cpu(1) {
            self.sound_write(addr, data);
            return;
        }
        self.main_map.watch_write(0, master, addr, data);
        // Any main-CPU write arms DECO CPU-7 decryption of the next opcode fetch.
        self.main_had_written = true;

        let a = addr as usize;
        match addr {
            0x0000..=0x07FF => self.ram[a & 0x07FF] = data,
            0x0C00..=0x0C0F => {
                self.palette_ram[a & 0x0F] = data;
                self.update_palette_entry(a & 0x0F);
            }
            0x1000..=0x13FF => self.videoram[a & 0x03FF] = data,
            0x1400..=0x17FF => self.colorram[a & 0x03FF] = data,
            0x1800..=0x1BFF => self.videoram[Self::swap(a & 0x03FF)] = data,
            0x1C00..=0x1FFF => self.colorram[Self::swap(a & 0x03FF)] = data,
            0x4002 => self.flip_screen = data & 1 != 0,
            // Latch the sound command and raise the sound-CPU IRQ.
            0x4003 => {
                self.sound_latch = data;
                self.sound_irq = true;
            }
            0x4004 => self.bnj_scroll0 = data,
            _ => {}
        }
    }

    /// Sound-CPU bus read (audio map). RAM is mirrored across 0x0000-0x1FFF;
    /// reading the command latch at 0xA000 acknowledges the sound IRQ.
    fn sound_read(&mut self, addr: u16) -> u8 {
        let data = match addr {
            0x0000..=0x1FFF => self.sound_ram[(addr & 0x03FF) as usize],
            0xA000..=0xBFFF => {
                self.sound_irq = false;
                self.sound_latch
            }
            0xE000..=0xFFFF => self.sound_map.read_backing(addr),
            _ => 0,
        };
        self.sound_map.watch_read(1, BusMaster::Cpu(1), addr, data);
        data
    }

    /// Sound-CPU bus write (audio map): RAM, the two AYs (address/data latches),
    /// and the audio NMI enable at 0xC000.
    fn sound_write(&mut self, addr: u16, data: u8) {
        self.sound_map.watch_write(1, BusMaster::Cpu(1), addr, data);
        match addr {
            0x0000..=0x1FFF => self.sound_ram[(addr & 0x03FF) as usize] = data,
            0x2000..=0x3FFF => self.ay1.data_write(data),
            0x4000..=0x5FFF => self.ay1.address_write(data),
            0x6000..=0x7FFF => self.ay2.data_write(data),
            0x8000..=0x9FFF => self.ay2.address_write(data),
            0xC000..=0xDFFF => self.audio_nmi_enable = data & 1 != 0,
            _ => {}
        }
    }

    pub(crate) fn bus_is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    pub(crate) fn bus_check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        let (nmi, irq) = if target == BusMaster::Cpu(1) {
            (self.sound_nmi_asserted(), self.sound_irq)
        } else {
            (false, self.main_irq)
        };
        InterruptState {
            nmi,
            irq,
            firq: false,
            irq_vector: 0,
            irq_level: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::save_state::{Saveable, StateReader, StateWriter};

    fn board() -> BtimeBoard {
        BtimeBoard::new(BtimeConfig { name: "btime-test" })
    }

    #[test]
    fn timing_frame_rate_is_btime() {
        // ~57.44 Hz from 6 MHz / (384 * 272).
        let hz = TIMING.frame_rate_hz();
        assert!((hz - 57.44).abs() < 0.5, "frame rate {hz} not ~57.44");
        assert_eq!(TIMING.cycles_per_frame(), 96 * 272);
    }

    #[test]
    fn ram_read_write_roundtrip() {
        let mut b = board();
        b.bus_write(BusMaster::Cpu(0), 0x0042, 0xAB);
        assert_eq!(b.bus_read(BusMaster::Cpu(0), 0x0042), 0xAB);
    }

    #[test]
    fn video_and_color_ram_roundtrip() {
        let mut b = board();
        b.bus_write(BusMaster::Cpu(0), 0x1005, 0x12);
        b.bus_write(BusMaster::Cpu(0), 0x1405, 0x34);
        assert_eq!(b.bus_read(BusMaster::Cpu(0), 0x1005), 0x12);
        assert_eq!(b.bus_read(BusMaster::Cpu(0), 0x1405), 0x34);
    }

    #[test]
    fn any_write_arms_deco_decryption() {
        let mut b = board();
        assert!(!b.main_had_written);
        b.bus_write(BusMaster::Cpu(0), 0x0000, 0x00);
        assert!(b.main_had_written);
    }

    #[test]
    fn coin_irq_reported_through_interrupts() {
        let mut b = board();
        assert!(!b.bus_check_interrupts(BusMaster::Cpu(0)).irq);
        b.main_irq = true;
        assert!(b.bus_check_interrupts(BusMaster::Cpu(0)).irq);
    }

    #[test]
    fn irq_vector_fetch_acknowledges_coin_irq() {
        let mut b = board();
        b.main_irq = true;
        // A normal read does not clear it.
        b.bus_read(BusMaster::Cpu(0), 0x0000);
        assert!(b.main_irq);
        // Vectoring through 0xFFFE (IRQ/BRK vector) acknowledges it.
        b.bus_read(BusMaster::Cpu(0), 0xFFFE);
        assert!(!b.main_irq);
    }

    // --- Sound subsystem ---

    #[test]
    fn sound_ram_reads_write_and_mirror() {
        let mut b = board();
        b.sound_write(0x0100, 0xAB);
        assert_eq!(b.sound_read(0x0100), 0xAB);
        // RAM is mirrored across 0x0000-0x1FFF (mirror mask 0x1C00).
        assert_eq!(b.sound_read(0x0100 + 0x1C00), 0xAB);
    }

    #[test]
    fn sound_latch_raises_and_reading_acks_the_irq() {
        let mut b = board();
        assert!(!b.bus_check_interrupts(BusMaster::Cpu(1)).irq);

        // A main-CPU write to 0x4003 latches the command and raises the sound IRQ.
        b.bus_write(BusMaster::Cpu(0), 0x4003, 0x42);
        assert!(b.sound_irq);
        assert!(b.bus_check_interrupts(BusMaster::Cpu(1)).irq);

        // The sound CPU reads the latch at 0xA000: returns the value and acks.
        assert_eq!(b.sound_read(0xA000), 0x42);
        assert!(!b.sound_irq);
        assert!(!b.bus_check_interrupts(BusMaster::Cpu(1)).irq);
    }

    #[test]
    fn sound_nmi_is_enable_anded_with_scanline_bit3() {
        let mut b = board();
        assert!(!b.sound_nmi_asserted());

        b.sound_write(0xC000, 0x01); // audio NMI enable
        b.clock = 0; // scanline 0 -> bit3 = 0
        assert!(!b.sound_nmi_asserted());
        b.clock = 8 * 96; // scanline 8 -> bit3 = 1
        assert!(b.sound_nmi_asserted());
        assert!(b.bus_check_interrupts(BusMaster::Cpu(1)).nmi);

        b.sound_write(0xC000, 0x00); // disable
        assert!(!b.sound_nmi_asserted());
    }

    #[test]
    fn ay_register_write_routes_and_produces_audio() {
        let mut b = board();
        // Program AY1 channel A: tone period (R0/R1) + full amplitude (R8), via
        // the address latch (0x4000) then data (0x2000).
        b.sound_write(0x4000, 0); // address = R0
        b.sound_write(0x2000, 0x55); // R0 fine tune
        b.sound_write(0x4000, 1); // address = R1
        b.sound_write(0x2000, 0x01); // R1 coarse tune
        b.sound_write(0x4000, 8); // address = R8 (channel A amplitude)
        b.sound_write(0x2000, 0x0F); // full volume

        for _ in 0..4000 {
            b.ay1.tick();
        }
        let mut buf = vec![0i16; 512];
        let n = b.ay1.fill_audio(&mut buf);
        assert!(buf[..n].iter().any(|&s| s != 0), "AY1 should output a tone");
    }

    // --- DIP defaults + live VBLANK bit ---

    #[test]
    fn dsw_power_on_defaults() {
        let b = board();
        assert_eq!(b.dsw1, 0x1F); // Coin A/B 1C1C, Leave-Off set, Upright
        assert_eq!(b.dsw2, 0x0B); // 3 lives, 20000 bonus, 4 enemies, pepper on
    }

    #[test]
    fn vblank_bit_injected_on_dsw1_read() {
        let mut b = board();
        // Visible scanline 100 -> VBLANK bit clear; DIP bits still read.
        b.clock = 100 * 96;
        let v = b.bus_read(BusMaster::Cpu(0), 0x4003);
        assert_eq!(v & 0x80, 0, "not in vblank");
        assert_eq!(v & 0x7F, 0x1F, "dsw1 bits present");
        // Scanline 0 and 260 are outside the visible [8,248) window -> bit set.
        b.clock = 0;
        assert_ne!(b.bus_read(BusMaster::Cpu(0), 0x4003) & 0x80, 0);
        b.clock = 260 * 96;
        assert_ne!(b.bus_read(BusMaster::Cpu(0), 0x4003) & 0x80, 0);
    }

    // --- X/Y-swap sprite-RAM mirror ---

    #[test]
    fn xy_swap_is_an_involution() {
        for off in 0..0x400usize {
            assert_eq!(BtimeBoard::swap(BtimeBoard::swap(off)), off);
        }
    }

    #[test]
    fn mirror_videoram_swaps_x_and_y() {
        let mut b = board();
        // A direct write to video RAM offset 5 is reachable through the 0x1800
        // mirror at the swapped address.
        b.bus_write(BusMaster::Cpu(0), 0x1000 + 5, 0x7E);
        let mirror = 0x1800 + BtimeBoard::swap(5) as u16;
        assert_eq!(b.bus_read(BusMaster::Cpu(0), mirror), 0x7E);

        // A write through the mirror lands at the swapped video-RAM offset.
        b.bus_write(BusMaster::Cpu(0), 0x1801, 0x3C); // off 1 -> videoram[swap(1)=32]
        assert_eq!(b.bus_read(BusMaster::Cpu(0), 0x1000 + 32), 0x3C);
    }

    #[test]
    fn mirror_colorram_swaps_x_and_y() {
        let mut b = board();
        b.bus_write(BusMaster::Cpu(0), 0x1400 + 5, 0x11);
        let mirror = 0x1C00 + BtimeBoard::swap(5) as u16;
        assert_eq!(b.bus_read(BusMaster::Cpu(0), mirror), 0x11);
    }

    // --- DECO CPU-7 opcode decryption ---
    //
    // A fresh board has `main_is_sync` set (a reset 6502 sits in Fetch), so every
    // test `bus_read` behaves as an opcode fetch.

    #[test]
    fn deco_decrypt_pure_fn_known_vectors() {
        // Permutation: out7<-in6, out6<-in5, out5<-in3, out4<-in4,
        // out3<-in2, out2<-in7, out1<-in1, out0<-in0.
        assert_eq!(deco_cpu7_decrypt(0x84), 0x0C); // in7|in2 -> out2|out3
        assert_eq!(deco_cpu7_decrypt(0x00), 0x00);
        assert_eq!(deco_cpu7_decrypt(0xFF), 0xFF);
        assert_eq!(deco_cpu7_decrypt(0x01), 0x01); // bit0 fixed
        assert_eq!(deco_cpu7_decrypt(0x02), 0x02); // bit1 fixed
        assert_eq!(deco_cpu7_decrypt(0x10), 0x10); // bit4 fixed
        // The moving bits form one 5-cycle (2->3->5->6->7->2); applying the
        // swap five times is the identity.
        let mut v = 0xA5;
        for _ in 0..5 {
            v = deco_cpu7_decrypt(v);
        }
        assert_eq!(v, 0xA5);
    }

    #[test]
    fn deco_decrypts_matching_address_fetch_after_write() {
        let mut b = board();
        let mut rom = vec![0u8; 0x5000];
        rom[0x1104] = 0x84; // -> 0xC104, and 0xC104 & 0x0104 == 0x0104
        b.load_main_rom(&rom);

        b.main_had_written = true; // as a prior write would set it
        let got = b.bus_read(BusMaster::Cpu(0), 0xC104);
        assert_eq!(got, 0x0C, "fetched byte deobfuscated");
        assert!(!b.main_had_written, "sync fetch consumes the flag");
    }

    #[test]
    fn deco_leaves_nonmatching_address_raw_but_still_clears_flag() {
        let mut b = board();
        let mut rom = vec![0u8; 0x5000];
        rom[0x1000] = 0x84; // -> 0xC000, and 0xC000 & 0x0104 == 0
        b.load_main_rom(&rom);

        b.main_had_written = true;
        assert_eq!(b.bus_read(BusMaster::Cpu(0), 0xC000), 0x84, "raw byte");
        assert!(!b.main_had_written, "flag clears on any sync fetch");
    }

    #[test]
    fn deco_no_decrypt_without_a_prior_write() {
        let mut b = board();
        let mut rom = vec![0u8; 0x5000];
        rom[0x1104] = 0x84;
        b.load_main_rom(&rom);
        // main_had_written stays false: a matching address is left untouched.
        assert_eq!(b.bus_read(BusMaster::Cpu(0), 0xC104), 0x84);
    }

    // --- Palette (BGR_233_inverted) ---

    #[test]
    fn palette_zero_ram_is_white_after_invert() {
        // palette_ram = 0x00 -> inverted 0xFF -> full R/G/B.
        let b = board();
        assert_eq!(b.palette_rgb[0], (0xFF, 0xFF, 0xFF));
    }

    #[test]
    fn palette_write_decodes_bgr233_inverted() {
        let mut b = board();
        // 0xFF -> inverted 0x00 -> black.
        b.bus_write(BusMaster::Cpu(0), 0x0C00, 0xFF);
        assert_eq!(b.palette_rgb[0], (0, 0, 0));

        // 0xF8 -> inverted 0x07 -> R=7 only -> pure red.
        b.bus_write(BusMaster::Cpu(0), 0x0C01, 0xF8);
        assert_eq!(b.palette_rgb[1], (0xFF, 0, 0));

        // 0xC7 -> inverted 0x38 -> G=7 only -> pure green (bits 3-5).
        b.bus_write(BusMaster::Cpu(0), 0x0C02, 0xC7);
        assert_eq!(b.palette_rgb[2], (0, 0xFF, 0));

        // 0x3F -> inverted 0xC0 -> B=3 only -> pure blue (bits 6-7).
        b.bus_write(BusMaster::Cpu(0), 0x0C03, 0x3F);
        assert_eq!(b.palette_rgb[3], (0, 0, 0xFF));
    }

    // --- GFX decode ---

    #[test]
    fn gfx_cache_dimensions() {
        let b = board();
        assert_eq!((b.chars().count(), b.chars().width()), (NUM_CHARS, 8));
        assert_eq!(
            (b.sprites().count(), b.sprites().width()),
            (NUM_SPRITES, 16)
        );
        assert_eq!(
            (b.bg_tiles().count(), b.bg_tiles().height()),
            (NUM_BG_TILES, 16)
        );
        assert_eq!(b.bg_map().len(), 0x0800);
    }

    #[test]
    fn char_decode_combines_three_planes() {
        let mut b = board();
        // gfx1 region: plane thirds at 0 / 0x2000 / 0x4000. Set pixel (0,0) of
        // char 0 in planes 0 and 1 (bit 0 and bit 1), leaving plane 2 clear.
        let mut gfx1 = vec![0u8; 0x6000];
        gfx1[0x0000] = 0x80; // plane 0 (LSB), row 0, leftmost pixel
        gfx1[0x2000] = 0x80; // plane 1, same pixel
        gfx1[0x4000] = 0x00; // plane 2 clear
        b.load_gfx1(&gfx1);
        assert_eq!(b.chars().pixel(0, 0, 0), 0b011);
        assert_eq!(b.chars().pixel(0, 1, 0), 0); // neighbor untouched
    }

    #[test]
    fn sprite_decode_uses_split_row_halves() {
        let mut b = board();
        // Sprite x offsets are [128..135, 0..7]: column 0 comes from bit offset
        // 128 (byte 16), column 8 from bit offset 0 (byte 0).
        let mut gfx1 = vec![0u8; 0x6000];
        gfx1[16] = 0x80; // plane 0, sprite 0, row 0, column 0 (x offset 128)
        gfx1[0] = 0x80; // plane 0, sprite 0, row 0, column 8 (x offset 0)
        b.load_gfx1(&gfx1);
        assert_eq!(b.sprites().pixel(0, 0, 0), 0b001);
        assert_eq!(b.sprites().pixel(0, 8, 0), 0b001);
        assert_eq!(b.sprites().pixel(0, 1, 0), 0);
    }

    #[test]
    fn bg_tile_decode_uses_smaller_plane_thirds() {
        let mut b = board();
        // gfx2 region (0x1800): plane thirds at 0 / 0x0800 / 0x1000.
        let mut gfx2 = vec![0u8; 0x1800];
        gfx2[0x0000] = 0x80; // plane 0
        gfx2[0x1000] = 0x80; // plane 2 (MSB)
        b.load_gfx2(&gfx2);
        assert_eq!(b.bg_tiles().pixel(0, 8, 0), 0b101); // x offset 0 -> column 8
    }

    #[test]
    fn palette_survives_save_load() {
        let mut b = board();
        b.bus_write(BusMaster::Cpu(0), 0x0C05, 0xF8); // red
        let mut w = StateWriter::new();
        b.save_state(&mut w);
        let bytes = w.into_vec();

        let mut b2 = board();
        let mut r = StateReader::new(&bytes);
        b2.load_state(&mut r).unwrap();
        assert_eq!(b2.palette_ram[5], 0xF8);
        assert_eq!(b2.palette_rgb[5], (0xFF, 0, 0));
    }

    // --- Renderer (layers + ROT270) ---
    //
    // These target the square logical image (240×240); the display-aspect
    // stretch is covered separately below.
    //
    // The board draws one row per visible scanline, so a test that wants a whole
    // picture has to walk the beam over it. `scan_frame` is that walk, and it is
    // deliberately the only way these tests get a frame: there is no
    // whole-frame render to call any more.

    fn scan_frame(b: &mut BtimeBoard) {
        for s in CROP_LO as u64..(CROP_LO + VISIBLE_DIM) as u64 {
            b.begin_scanline(s);
        }
    }

    fn pixel(buffer: &[u8], row: usize, col: usize) -> (u8, u8, u8) {
        let i = (row * VISIBLE_DIM + col) * 3;
        (buffer[i], buffer[i + 1], buffer[i + 2])
    }

    #[test]
    fn render_default_frame_is_backdrop_white() {
        // No gfx loaded: every char is code 0 / all-pen-0, drawn opaque (bg
        // disabled) -> palette entry 0, which decodes to white (ram 0 inverted).
        let mut b = board();
        scan_frame(&mut b);
        assert!(
            b.framebuffer.iter().all(|&c| c == 0xFF),
            "frame should be all white"
        );
    }

    #[test]
    fn render_char_lands_at_native_position() {
        let mut b = board();
        // char 1 = all pen 1 (plane 0 set for its 8 bytes at region offset 8).
        let mut gfx1 = vec![0u8; 0x6000];
        for byte in gfx1.iter_mut().skip(8).take(8) {
            *byte = 0xFF;
        }
        b.load_gfx1(&gfx1);
        // palette entry 1 -> red.
        b.bus_write(BusMaster::Cpu(0), 0x0C01, 0xF8);

        // Place char 1 at video RAM offset 495 (char cell x=16, y=15).
        b.videoram[495] = 1;

        scan_frame(&mut b);

        // The rows emit native (unrotated) pixels: the char occupies native
        // cols 128..135 × rows 120..127, i.e. crop rows 112..119 × cols
        // 120..127 after subtracting CROP_LO=8. ROT270 is applied centrally.
        let buffer = &b.framebuffer;
        assert_eq!(pixel(buffer, 115, 123), (0xFF, 0, 0), "char center is red");
        assert_eq!(pixel(buffer, 10, 10), (0xFF, 0xFF, 0xFF), "elsewhere white");
    }

    #[test]
    fn render_background_uses_palette_base_8() {
        let mut b = board();
        // Every bg-tile pixel gets plane-0 bit set -> pen 1 -> palette index 9.
        let mut gfx2 = vec![0u8; 0x1800];
        for byte in gfx2.iter_mut().take(0x0800) {
            *byte = 0xFF;
        }
        b.load_gfx2(&gfx2);
        // Enable the background layer; palette entry 9 -> blue. Rows are drawn
        // as the beam passes, so the register has to be set before the scan.
        b.bnj_scroll0 = 0x10;
        b.bus_write(BusMaster::Cpu(0), 0x0C09, 0x3F);

        scan_frame(&mut b);

        // Chars are transparent over the backdrop, so the blue bg shows through.
        let has_blue = b.framebuffer.as_chunks::<3>().0.contains(&[0, 0, 0xFF]);
        assert!(has_blue, "background (palette base 8) should be visible");
    }

    // -----------------------------------------------------------------------
    // Mid-frame bnj_scroll0
    // -----------------------------------------------------------------------

    /// A board whose background tiles are all pen 1, so palette entry 9 marks
    /// any row the background layer reached.
    fn board_with_visible_background() -> BtimeBoard {
        let mut b = board();
        let mut gfx2 = vec![0u8; 0x1800];
        for byte in gfx2.iter_mut().take(0x0800) {
            *byte = 0xFF;
        }
        b.load_gfx2(&gfx2);
        b.bus_write(BusMaster::Cpu(0), 0x0C09, 0x3F); // entry 9 -> blue
        b
    }

    fn row_has_blue(buffer: &[u8], y: usize) -> bool {
        let row = &buffer[y * VISIBLE_DIM * 3..(y + 1) * VISIBLE_DIM * 3];
        row.as_chunks::<3>().0.contains(&[0, 0, 0xFF])
    }

    /// The behaviour W2 exists for. Burger Time enables the background partway
    /// down the screen (measured at scanline 91 on this ROM set), and the rows
    /// above the write must not get it.
    #[test]
    fn a_mid_frame_background_enable_splits_the_screen() {
        const SPLIT_SCANLINE: u64 = 91;
        let split_row = (SPLIT_SCANLINE - CROP_LO as u64) as usize;

        let mut b = board_with_visible_background();
        b.bnj_scroll0 = 0x00;
        for s in CROP_LO as u64..SPLIT_SCANLINE {
            b.begin_scanline(s);
        }
        b.bnj_scroll0 = 0x13; // what the game actually writes
        for s in SPLIT_SCANLINE..(CROP_LO + VISIBLE_DIM) as u64 {
            b.begin_scanline(s);
        }

        let buffer = &b.framebuffer;
        assert!(!row_has_blue(buffer, 0), "row 0 is above the write");
        assert!(
            !row_has_blue(buffer, split_row - 1),
            "the last row above the write has no background"
        );
        assert!(
            row_has_blue(buffer, split_row),
            "the first row below the write has the background"
        );
        assert!(
            row_has_blue(buffer, VISIBLE_DIM - 1),
            "the bottom row is below the write"
        );
    }

    /// The tilemap half of W4: video RAM is read as the beam passes it, so
    /// rewriting a char partway down the screen changes only the rows below the
    /// write.
    ///
    /// The split is at native row 100, which is *inside* char row 12 (rows
    /// 96..103). A whole-frame render draws a char row from one snapshot and
    /// cannot produce this picture at all.
    #[test]
    fn a_mid_frame_vram_write_changes_only_the_rows_below_it() {
        const SPLIT_SCANLINE: u64 = 100;
        let split_row = (SPLIT_SCANLINE - CROP_LO as u64) as usize;

        let mut b = board();
        // char 1 = solid pen 1, char 2 = solid pen 2.
        let mut gfx1 = vec![0u8; 0x6000];
        for byte in gfx1.iter_mut().skip(8).take(8) {
            *byte = 0xFF; // char 1, plane 0
        }
        for byte in gfx1.iter_mut().skip(0x2010).take(8) {
            *byte = 0xFF; // char 2, plane 1
        }
        b.load_gfx1(&gfx1);
        b.bus_write(BusMaster::Cpu(0), 0x0C01, 0xF8); // entry 1 -> red
        b.bus_write(BusMaster::Cpu(0), 0x0C02, 0x3F); // entry 2 -> blue

        b.videoram.fill(1);
        for s in CROP_LO as u64..SPLIT_SCANLINE {
            b.begin_scanline(s);
        }
        b.videoram.fill(2);
        for s in SPLIT_SCANLINE..(CROP_LO + VISIBLE_DIM) as u64 {
            b.begin_scanline(s);
        }

        let red = (0xFF, 0, 0);
        let blue = (0, 0, 0xFF);
        let buffer = &b.framebuffer;
        assert_eq!(
            pixel(buffer, 0, 100),
            red,
            "row 0 was drawn before the write"
        );
        assert_eq!(
            pixel(buffer, split_row - 1, 100),
            red,
            "the last row above the write keeps the old char, mid-char-row"
        );
        assert_eq!(
            pixel(buffer, split_row, 100),
            blue,
            "the first row below the write takes the new char"
        );
        assert_eq!(
            pixel(buffer, VISIBLE_DIM - 1, 100),
            blue,
            "the bottom row is below the write"
        );
    }

    /// The tests above drive `begin_scanline` by hand, which proves nothing
    /// about whether the frame loop ever calls it. Without this one they would
    /// all pass on a board that never drew anything, so this walks the real
    /// `tick` and `run_scanlines` paths and checks the picture changed.
    #[test]
    fn the_frame_loop_draws_rows_at_scanline_boundaries() {
        let mut b = board_with_visible_background();
        let mut cpu = M6502::new();
        let mut sound = M6502::new();
        b.framebuffer.fill(0);

        // Wind through vblank to the last line before the visible window. The
        // debugger's per-cycle path is the one being exercised here.
        b.bnj_scroll0 = 0x13;
        for _ in 0..CROP_LO as u64 * TIMING.cycles_per_scanline {
            tick(&mut cpu, &mut sound, &mut b);
        }
        assert!(
            b.framebuffer.iter().all(|&c| c == 0),
            "tick() drew nothing during vblank"
        );

        // One more scanline through tick(), which crosses the first visible
        // boundary and must draw row 0 and only row 0.
        for _ in 0..TIMING.cycles_per_scanline {
            tick(&mut cpu, &mut sound, &mut b);
        }
        assert!(row_has_blue(&b.framebuffer, 0), "tick() draws row 0");
        assert!(
            !row_has_blue(&b.framebuffer, 1),
            "and only row 0: nothing has drawn row 1 yet"
        );

        // And the hoisted loop the frame actually runs through draws too.
        run_scanlines(&mut cpu, &mut sound, &mut b, TIMING.cycles_per_scanline);
        assert!(
            row_has_blue(&b.framebuffer, 1),
            "run_scanlines() draws row 1"
        );
    }

    /// Vblank has no row to draw, and drawing one would run off the end of a
    /// framebuffer sized to the visible window.
    #[test]
    fn vblank_scanlines_have_no_row_to_draw() {
        let mut b = board_with_visible_background();
        b.bnj_scroll0 = 0x13;
        b.framebuffer.fill(0);
        for s in 0..CROP_LO as u64 {
            b.begin_scanline(s);
        }
        for s in (CROP_LO + VISIBLE_DIM) as u64..TIMING.total_scanlines {
            b.begin_scanline(s);
        }
        assert!(b.framebuffer.iter().all(|&c| c == 0), "vblank drew nothing");
    }

    /// A load restores `bnj_scroll0` and leaves the framebuffer alone, so the
    /// picture has to come back from the rows the *next* frame draws.
    ///
    /// This replaces a test that asserted a load seeded the per-row
    /// `bnj_scroll0` samples. Those samples are gone: rows read the live
    /// register when they are drawn, so what needs proving now is that a
    /// restored register reaches the picture with no stale per-row state in
    /// between.
    #[test]
    fn a_load_restores_the_register_the_next_frames_rows_read() {
        let mut saved = board_with_visible_background();
        saved.bnj_scroll0 = 0x13; // background on
        let mut w = StateWriter::new();
        saved.save_state(&mut w);
        let bytes = w.into_vec();

        let mut b = board_with_visible_background();
        b.bnj_scroll0 = 0x00; // background off, and a picture drawn without it
        scan_frame(&mut b);
        assert!(
            !row_has_blue(&b.framebuffer, 0),
            "no background before load"
        );

        let mut r = StateReader::new(&bytes);
        b.load_state(&mut r).unwrap();
        scan_frame(&mut b);

        assert!(
            row_has_blue(&b.framebuffer, 0),
            "the frame after a load draws from the restored register"
        );
    }

    #[test]
    fn render_frame_is_native_square_with_3x4_aspect_hint() {
        let mut b = board();
        // Some vertical variation: a red char at one cell.
        let mut gfx1 = vec![0u8; 0x6000];
        for byte in gfx1.iter_mut().skip(8).take(8) {
            *byte = 0xFF; // char 1 = pen 1
        }
        b.load_gfx1(&gfx1);
        b.bus_write(BusMaster::Cpu(0), 0x0C01, 0xF8); // entry 1 -> red
        b.videoram[300] = 1;

        // Native raster is the square 240×240; the ROT270 rotation and 3:4
        // portrait presentation are declared and applied centrally, not baked.
        let (w, h) = TIMING.display_size();
        assert_eq!((w, h), (240, 240), "native square raster");
        assert_eq!(
            b.orientation(),
            phosphor_core::core::machine::Orientation::ROT270
        );
        assert_eq!(TIMING.display_aspect(), Some((3, 4)), "3:4 portrait hint");

        scan_frame(&mut b);
        let vis = b.framebuffer.clone();
        let mut disp = vec![0u8; (w * h * 3) as usize];
        b.render_frame(&mut disp);

        // render_frame emits the visible square verbatim, no vertical stretch.
        assert_eq!(disp, vis, "framebuffer is the native 240×240 visible image");
    }
}
