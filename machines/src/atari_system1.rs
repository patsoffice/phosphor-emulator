//! Atari System 1 shared board (1984-85).
//!
//! System 1 is a motherboard + game-cartridge design: an MC68010 main CPU
//! (7.15909 MHz) over a sparse 24-bit address space, an M6502 sound board
//! ([`AtariSystem1Sound`]: POKEY + YM2151, plus optional TMS5220 speech), a
//! **Slapstic** copy-protection PAL that bank-switches the `0x080000-0x087FFF`
//! ROM window, a 2804 EEPROM, and a banked tilemap + linked-list motion-object
//! video pipeline. The reset vectors live in the shared motherboard BIOS at
//! `0x000000`, which jumps into the cartridge's banked program ROMs.
//!
//! This module owns everything that is identical across the catalog (Marble
//! Madness, Road Runner, …). A per-game wrapper (see [`crate::marble`]) holds
//! only the cartridge ROM manifest, the slapstic chip id, and the game's own
//! input ports; its [`Bus`] intercepts those ports and forwards the rest to the
//! board's [`bus_read`](AtariSystem1Board::bus_read)/[`bus_write`](AtariSystem1Board::bus_write),
//! exactly like `WilliamsBoard` + `JoustSystem`.
//!
//! ## Main-CPU memory map (word bus, big-endian; base windows only)
//! ```text
//!   000000-07FFFF  Program ROM (BIOS @ 0, cartridge banks @ 0x10000+)
//!   080000-087FFF  Slapstic-banked ROM window (4 × 8 KB banks)
//!   2E0000         R  Sprite/MO scanline-interrupt state (bit 7)
//!   400000-401FFF  R/W Work RAM
//!   800000         W  Playfield X scroll      820000  W  Playfield Y scroll
//!   840000         W  Playfield priority color mask
//!   860001         W  Audio/video control latch (sound reset, MO/PF banks)
//!   880001         W  Watchdog reset          8A0001  W  VBLANK IRQ ack
//!   8C0001         W  EEPROM unlock
//!   900000-9FFFFF  R/W Cartridge external RAM
//!   A00000-A01FFF  R/W Playfield RAM    A02000-A02FFF  R/W Motion-object RAM
//!   A03000-A03FFF  R/W Alphanumerics RAM
//!   B00000-B007FF  R/W Palette RAM
//!   F00000-F003FF  R/W EEPROM 2804 (512 bytes, low byte)
//!   F20000-F4001F  R  Game-specific input ports (handled by the game wrapper)
//!   F60000         R  Switch inputs (start/service/VBLANK/sound-buffer)
//!   FC0001         R  Sound response read       FE0001  W  Sound command write
//! ```
//!
//! ## Byte registers on a word bus
//! The single-byte control registers sit at odd addresses (`860001`, `880001`,
//! `8A0001`, …). A 68000 byte write becomes a word read-modify-write at the even
//! base, so the board sees a word access at `860000` with the value in the low
//! byte — we decode on the even base and take `data & 0xFF`.

use std::collections::HashMap;

use phosphor_core::audio::SampleRing;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace32};
use phosphor_core::core::{Bus, BusMaster, ClockDivider, TimingConfig};
use phosphor_core::cpu::m68000::{M68kVariant, M68000};
use phosphor_core::device::slapstic::Slapstic;
use phosphor_core::gfx::decode::{GfxCache, GfxLayout, decode_gfx};
use phosphor_macros::{BusDebug, MemoryRegion};

use crate::atari_system1_sound::AtariSystem1Sound;

// ---------------------------------------------------------------------------
// Address-space regions (backed memory only; I/O is decoded in the Bus impl)
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub enum Region {
    /// Program ROM: BIOS @ 0, cartridge banks @ 0x10000+ (covers 000000-07FFFF).
    Rom = 1,
    Ram = 3,
    /// Cartridge external RAM (900000-9FFFFF). Unused by some games, but mapped
    /// so stray accesses are backed rather than faulting.
    CartRam = 4,
    Playfield = 5,
    Mob = 6,
    Alpha = 7,
    Palette = 8,
}

// ---------------------------------------------------------------------------
// Motherboard alpha font (shared across all System 1 games)
// ---------------------------------------------------------------------------

/// 8×8 2bpp alpha tiles. The two bitplanes sit at bit offsets 0 and 4 within
/// each 16-bit row, MSB-first; `decode_gfx` wants LSB-first (entry 0 = pen bit
/// 0), so the plane list is reversed to `{4, 0}`.
const ALPHA_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[4, 0],
    x_offsets: &[0, 1, 2, 3, 8, 9, 10, 11],
    y_offsets: &[0, 16, 32, 48, 64, 80, 96, 112],
    char_increment: 128,
};

/// Number of 8×8 alpha tiles in the font ROM (0x2000 / 16 bytes per tile).
pub const ALPHA_TILE_COUNT: usize = 512;

// ---------------------------------------------------------------------------
// Playfield / motion-object GFX decode (PROM-driven bank + bpp selection)
// ---------------------------------------------------------------------------
//
// Each of the 256 playfield lookup entries is keyed by a remap PROM pair that
// yields a tile bank (1-7), a bit depth (4/5/6), a colour, and a code offset.
// The same machinery feeds the motion objects.

// PROM1 / PROM2 bit assignments (the two graphics-mapping PROMs).
const PROM1_BANK_4: u8 = 0x80; // active low
const PROM1_BANK_3: u8 = 0x40;
const PROM1_BANK_2: u8 = 0x20;
const PROM1_BANK_1: u8 = 0x10;
const PROM1_OFFSET_MASK: u8 = 0x0F; // positive logic
const PROM2_BANK_6_OR_7: u8 = 0x80; // active low
const PROM2_BANK_5: u8 = 0x40;
const PROM2_PLANE_5_ENABLE: u8 = 0x20; // active high
const PROM2_PLANE_4_ENABLE: u8 = 0x10;
const PROM2_PF_COLOR_MASK: u8 = 0x0F; // negative logic
const PROM2_BANK_7: u8 = 0x08;
const PROM2_MO_COLOR_MASK: u8 = 0x07; // negative logic (motion objects)

/// 8×8 tiles, `char_increment` 64 bits (8 bytes). Planes sit 0x10000 bytes apart
/// (`0x80000` bits) and are stored MSB-first, so the plane list is reversed for
/// `decode_gfx`'s LSB-first convention (plane 0 = pen bit 0).
const TILE_X_OFFSETS: [usize; 8] = [0, 1, 2, 3, 4, 5, 6, 7];
const TILE_Y_OFFSETS: [usize; 8] = [0, 8, 16, 24, 32, 40, 48, 56];
const TILE_PLANES_4: [usize; 4] = [0, 0x80000, 0x100000, 0x180000];
const TILE_PLANES_5: [usize; 5] = [0, 0x80000, 0x100000, 0x180000, 0x200000];
const TILE_PLANES_6: [usize; 6] = [0, 0x80000, 0x100000, 0x180000, 0x200000, 0x280000];

/// 4096 8×8 tiles per gfx bank.
const TILES_PER_BANK: usize = 4096;

fn tile_layout(bpp: u8) -> GfxLayout<'static> {
    let plane_offsets: &'static [usize] = match bpp {
        4 => &TILE_PLANES_4,
        5 => &TILE_PLANES_5,
        _ => &TILE_PLANES_6,
    };
    GfxLayout {
        plane_offsets,
        x_offsets: &TILE_X_OFFSETS,
        y_offsets: &TILE_Y_OFFSETS,
        char_increment: 64,
    }
}

/// One decoded tile gfx bank: a 4096-tile cache and its bit depth.
pub struct GfxBank {
    pub cache: GfxCache,
    pub bpp: u8,
}

impl GfxBank {
    fn blank() -> Self {
        Self {
            cache: GfxCache::new(1, 8, 8),
            bpp: 4,
        }
    }
}

/// The decoded tile graphics: the playfield and motion-object remap lookups
/// (256 entries each) plus the tile banks they share. `banks[0]` is a blank
/// placeholder so real banks are 1-indexed (matching the lookups' gfx field;
/// the 0 slot is reserved — and is what an unmapped motion-object bank resolves
/// to, i.e. an invisible sprite).
pub struct PlayfieldGfx {
    pub lookup: [u16; 256],
    pub mo_lookup: [u16; 256],
    pub banks: Vec<GfxBank>,
}

impl PlayfieldGfx {
    pub fn empty() -> Self {
        Self {
            lookup: [0; 256],
            mo_lookup: [0; 256],
            banks: vec![GfxBank::blank()],
        }
    }
}

/// Resolve the tile bank for a PROM pair, decoding it on first use. The bank
/// index comes from the active-low bank-select bits across both PROMs. Returns a
/// `banks` index (≥1), or 0 when the bank is unmapped.
fn get_pf_bank(
    prom1: u8,
    prom2: u8,
    bpp: u8,
    tiles: &[u8],
    banks: &mut Vec<GfxBank>,
    bank_gfx: &mut HashMap<(u8, u8), u8>,
) -> u8 {
    let bank_index = if prom1 & PROM1_BANK_1 == 0 {
        1
    } else if prom1 & PROM1_BANK_2 == 0 {
        2
    } else if prom1 & PROM1_BANK_3 == 0 {
        3
    } else if prom1 & PROM1_BANK_4 == 0 {
        4
    } else if prom2 & PROM2_BANK_5 == 0 {
        5
    } else if prom2 & PROM2_BANK_6_OR_7 == 0 {
        if prom2 & PROM2_BANK_7 == 0 { 7 } else { 6 }
    } else {
        return 0;
    };

    if let Some(&id) = bank_gfx.get(&(bpp, bank_index)) {
        return id;
    }

    // Out of range for the populated tile ROM → treat as unmapped.
    let bank_base = 0x80000 * (bank_index as usize - 1);
    if bank_base >= tiles.len() {
        return 0;
    }

    let cache = decode_gfx(&tiles[bank_base..], 0, TILES_PER_BANK, &tile_layout(bpp));
    let id = banks.len() as u8;
    banks.push(GfxBank { cache, bpp });
    bank_gfx.insert((bpp, bank_index), id);
    id
}

/// Build the playfield and motion-object remap lookups and decode the tile banks
/// they share, from the PROMs and the (already inverted) tile ROM. The two PROMs
/// hold two parallel halves: entries 0-255 drive the playfield, 256-511 the
/// motion objects (each with its own colour mask), keyed by prom1 at +0x000/
/// +0x100 and prom2 at +0x200/+0x300.
pub fn build_tile_gfx(prom: &[u8], tiles: &[u8]) -> PlayfieldGfx {
    let mut gfx = PlayfieldGfx::empty();
    let mut bank_gfx: HashMap<(u8, u8), u8> = HashMap::new();
    for i in 0..256 {
        // --- Playfield half (prom1 @ 0x000, prom2 @ 0x200) ---
        let p1 = prom[i];
        let p2 = prom[0x200 + i];
        let bpp = if p2 & PROM2_PLANE_4_ENABLE != 0 {
            if p2 & PROM2_PLANE_5_ENABLE != 0 { 6 } else { 5 }
        } else {
            4
        };
        let mut offset = (p1 & PROM1_OFFSET_MASK) as u16;
        let mut color = (((!p2) & PROM2_PF_COLOR_MASK) >> (bpp - 4)) as u16;
        let mut bank = get_pf_bank(p1, p2, bpp, tiles, &mut gfx.banks, &mut bank_gfx);
        // Unmapped bank → fall back to the first real bank, blank tile.
        if bank == 0 {
            bank = 1;
            offset = 0;
            color = 0;
        }
        gfx.lookup[i] = offset | ((bank as u16) << 8) | (color << 12);

        // --- Motion-object half (prom1 @ 0x100, prom2 @ 0x300) ---
        let m1 = prom[0x100 + i];
        let m2 = prom[0x300 + i];
        let mbpp = if m2 & PROM2_PLANE_4_ENABLE != 0 {
            if m2 & PROM2_PLANE_5_ENABLE != 0 { 6 } else { 5 }
        } else {
            4
        };
        let mo_offset = (m1 & PROM1_OFFSET_MASK) as u16;
        let mo_color = (((!m2) & PROM2_MO_COLOR_MASK) >> (mbpp - 4)) as u16;
        // No bank-0 remap for sprites — an unmapped bank stays 0 (invisible).
        let mo_bank = get_pf_bank(m1, m2, mbpp, tiles, &mut gfx.banks, &mut bank_gfx);
        gfx.mo_lookup[i] = mo_offset | ((mo_bank as u16) << 8) | (mo_color << 12);
    }
    gfx
}

/// Convert one IRGB-4444 palette word to RGB24. The word is `IIII RRRR GGGG
/// BBBB`: a 4-bit intensity scales each 4-bit colour component. Each nibble is
/// first expanded to 8 bits by replication (`n*0x11`), then `c = (i * comp) >> 8`.
pub fn irgb4444_to_rgb(raw: u16) -> (u8, u8, u8) {
    let expand4 = |n: u16| -> u32 {
        let n = (n & 0x0F) as u32;
        (n << 4) | n
    };
    let i = expand4(raw >> 12);
    let r = (i * expand4(raw >> 8)) >> 8;
    let g = (i * expand4(raw >> 4)) >> 8;
    let b = (i * expand4(raw)) >> 8;
    (r as u8, g as u8, b as u8)
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------
// Master clock 14.318181 MHz. Main CPU = pixel clock = master/2 = 7.15909 MHz,
// so CPU cycles map 1:1 to pixel clocks: HTOTAL 456, VTOTAL 262 → ~59.92 Hz.
// Visible area 336×240 (H total 456, V total 262).
pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 7_159_090,
    cycles_per_scanline: 456,
    total_scanlines: 262,
    display_width: 336,
    display_height: 240,
    display_aspect: Some((4, 3)),
};

/// First scanline of vertical blank (`vbstart`); VBLANK asserts IRQ4 here.
pub(crate) const VBLANK_SCANLINE: u16 = 240;

// ---------------------------------------------------------------------------
// AtariSystem1Board
// ---------------------------------------------------------------------------

/// An Atari System 1 bus: a game wrapper's view over the shared board plus its
/// cartridge-specific input ports.
///
/// [`tick`] is generic over this trait, so every access the 68010 makes — this
/// is a word-wide 24-bit bus — resolves to a direct call rather than a vtable
/// entry.
pub trait AtariSystem1Bus: Bus<Address = u32, Data = u16> {
    fn board(&mut self) -> &mut AtariSystem1Board;
}

/// One CPU cycle: board work, the 68010, then the sound board on its divider.
///
/// This is the debugger's path — it tests the frame position on every cycle. A
/// whole frame goes through [`run_scanlines`], which hoists that test out.
#[inline]
pub fn tick<B: AtariSystem1Bus>(cpu: &mut M68000, bus: &mut B) {
    let board = bus.board();
    let frame_cycle = board.clock % TIMING.cycles_per_frame();
    if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
        board.begin_scanline((frame_cycle / TIMING.cycles_per_scanline) as u16);
    }
    step_cycle(cpu, bus);
}

/// Run `cycles` CPU cycles, scanline-outer and cycle-inner. The caller must
/// start on a scanline boundary and pass a multiple of `cycles_per_scanline`;
/// the debugger's off-boundary stepping goes through [`tick`] instead.
pub fn run_scanlines<B: AtariSystem1Bus>(cpu: &mut M68000, bus: &mut B, cycles: u64) {
    debug_assert!(
        bus.board().clock.is_multiple_of(TIMING.cycles_per_scanline)
            && cycles.is_multiple_of(TIMING.cycles_per_scanline),
        "run_scanlines must start on a scanline boundary and run whole scanlines"
    );
    for _ in 0..cycles / TIMING.cycles_per_scanline {
        let board = bus.board();
        let scanline = board.clock % TIMING.cycles_per_frame() / TIMING.cycles_per_scanline;
        board.begin_scanline(scanline as u16);
        for _ in 0..TIMING.cycles_per_scanline {
            step_cycle(cpu, bus);
        }
    }
}

/// Run one frame's worth of cycles. Whole scanlines go through
/// [`run_scanlines`]; a partial scanline at either end (only after the debugger
/// has left the clock off-boundary) goes through [`tick`].
pub fn run_frame<B: AtariSystem1Bus>(cpu: &mut M68000, bus: &mut B) {
    let scanline = TIMING.cycles_per_scanline;
    let mut remaining = TIMING.cycles_per_frame();

    let lead = ((scanline - bus.board().clock % scanline) % scanline).min(remaining);
    for _ in 0..lead {
        tick(cpu, bus);
    }
    remaining -= lead;

    let whole = remaining - remaining % scanline;
    run_scanlines(cpu, bus, whole);
    remaining -= whole;

    for _ in 0..remaining {
        tick(cpu, bus);
    }
}

/// The part of a cycle with no frame-position test in it.
#[inline]
fn step_cycle<B: AtariSystem1Bus>(cpu: &mut M68000, bus: &mut B) {
    bus.board().begin_cycle_inner(cpu);
    cpu.execute_cycle(bus, BusMaster::Cpu(0));
    bus.board().end_cycle();
}

/// The shared Atari System 1 hardware. A game wrapper owns one of these plus its
/// cartridge-specific input ports and ROM manifest.
///
/// The board is everything the 68010 talks *to*; the CPU itself lives on the
/// game wrapper.
#[derive(BusDebug)]
pub struct AtariSystem1Board {
    #[debug_map(cpu = 0)]
    pub(crate) map: AddressSpace32,

    /// Slapstic protection PAL gating the 080000-087FFF ROM window.
    pub(crate) slapstic: Slapstic,
    /// The 32 KB (4 × 8 KB bank) slapstic ROM the window selects between.
    pub(crate) slapstic_rom: Vec<u8>,

    /// EEPROM 2804 (512 bytes, low byte at F00000-F003FF), gated by `eeprom_unlocked`.
    pub(crate) eeprom: [u8; 512],
    /// 0x8C0001 EEPROM unlock latch. The 2804 re-locks after each write.
    pub(crate) eeprom_unlocked: bool,
    /// Count of accepted EEPROM byte writes (bring-up diagnostic; not saved).
    eeprom_writes: u64,

    /// Decoded 8×8 2bpp alpha (text/HUD) font tiles. Not CPU-addressable.
    pub(crate) alpha_cache: GfxCache,
    /// Decoded playfield tile banks + the PROM remap lookup. Not CPU-addressable.
    pub(crate) playfield: PlayfieldGfx,

    // Video control latches (consumed by the video pipeline).
    pub(crate) xscroll: u16,
    pub(crate) yscroll: u16,
    pub(crate) priority_pens: u16,
    /// 0x860001 audio/video control: bit 7 = sound-CPU reset, bits 5-3 =
    /// motion-object bank, bit 2 = playfield tile bank.
    pub(crate) bankselect: u8,

    // F60000 switch port low byte (active-low; bits 0/1 = start, bit 6 = service).
    // Bits 4 (VBLANK) and 7 (sound buffer) are computed live in `read_f60000`.
    pub(crate) f60000_buttons: u8,

    // VBLANK interrupt latch (IRQ4), held until acked via 0x8A0001.
    pub(crate) video_int: bool,
    /// Scanline motion-object interrupt (IRQ3 / "SLIP"). Asserted for the one
    /// scanline a motion-object timer entry targets; also read back at 0x2E0000
    /// bit 7. Recomputed at every scanline boundary from the active sprite bank.
    pub(crate) scanline_int: bool,
    /// Analog-joystick interrupt (IRQ2). Games with an ADC0809 (Road Runner et
    /// al.) drive this from the converter's end-of-conversion line, gated by the
    /// joystick-IRQ enable; games without one (Marble) leave it false.
    pub(crate) int2: bool,

    /// One-pole DC-blocker state (prev input, prev output) for the audio mix —
    /// removes the POKEY's unipolar DC so the FM music gets full headroom, the
    /// way the cabinet's AC-coupled amplifier does.
    audio_dc: (f32, f32),

    /// M6502 sound board (POKEY + YM2151 + optional speech + inter-CPU latches).
    #[debug_device("Sound")]
    pub(crate) sound: AtariSystem1Sound,
    /// Sound CPU runs at 1/4 the main CPU rate.
    sound_clock: ClockDivider,
    audio_buffer: SampleRing<i16>,

    pub(crate) clock: u64,
    pub(crate) watchdog_count: u8,

    /// Per-frame log of motion-object bank switches as `(scanline, mo_bank)`,
    /// sorted by scanline with the frame's starting bank at scanline 0. The game
    /// reprograms the MO bank mid-frame (e.g. Road Runner), so the compositor
    /// renders each scanline band with the bank that was live there — mirroring
    /// the hardware's beam-time bank select. Rebuilt every frame; not saved.
    mo_bank_changes: Vec<(u16, u8)>,

    /// Motion-object state as it stood when the beam finished the visible area,
    /// captured at the start of vblank: a copy of the sprite RAM plus the band
    /// log for those scanlines. Both games double-buffer the display list —
    /// they rebuild it during vblank and publish it with a bank swap — so the
    /// live sprite RAM at the frame boundary already holds the *next* scanout's
    /// list. Rendering from this snapshot keeps the sprite RAM and the bands
    /// paired with the frame they actually describe. Empty until the first
    /// vblank (a direct render without stepping falls back to live state).
    mo_shadow: Vec<u8>,
    mo_shadow_bands: Vec<(u16, u8)>,
}

impl AtariSystem1Board {
    fn build_map() -> AddressSpace32 {
        let mut map = AddressSpace32::new();
        map.region(
            Region::Rom,
            "Program ROM",
            0x00_0000,
            0x8_0000,
            AccessKind::ReadOnly,
        )
        .region(
            Region::Ram,
            "Work RAM",
            0x40_0000,
            0x2000,
            AccessKind::ReadWrite,
        )
        .region(
            Region::CartRam,
            "Cartridge RAM",
            0x90_0000,
            0x10_0000,
            AccessKind::ReadWrite,
        )
        .region(
            Region::Playfield,
            "Playfield RAM",
            0xA0_0000,
            0x2000,
            AccessKind::ReadWrite,
        )
        .region(
            Region::Mob,
            "Motion-object RAM",
            0xA0_2000,
            0x1000,
            AccessKind::ReadWrite,
        )
        .region(
            Region::Alpha,
            "Alpha RAM",
            0xA0_3000,
            0x1000,
            AccessKind::ReadWrite,
        )
        .region(
            Region::Palette,
            "Palette RAM",
            0xB0_0000,
            0x800,
            AccessKind::ReadWrite,
        );
        map
    }

    /// Build a board for the given slapstic chip (`137412-NNN`), with `speech`
    /// enabling the sound board's TMS5220 window.
    /// The 68010 the board is designed for. The machine owns the CPU; this
    /// builds one configured for this hardware.
    pub fn new_cpu() -> M68000 {
        let mut cpu = M68000::new();
        cpu.variant = M68kVariant::M68010;
        cpu
    }

    pub fn new(slapstic_chip: u16, speech: bool) -> Self {
        Self {
            map: Self::build_map(),
            slapstic: Slapstic::for_chip(slapstic_chip),
            slapstic_rom: vec![0; 0x8000],
            eeprom: [0xFF; 512], // 2804 reads 0xFF erased; game checksums + reinits
            eeprom_unlocked: false,
            eeprom_writes: 0,
            alpha_cache: GfxCache::new(ALPHA_TILE_COUNT, 8, 8),
            playfield: PlayfieldGfx::empty(),
            xscroll: 0,
            yscroll: 0,
            priority_pens: 0,
            bankselect: 0,
            f60000_buttons: 0xFF,
            video_int: false,
            scanline_int: false,
            int2: false,
            audio_dc: (0.0, 0.0),
            sound: AtariSystem1Sound::new(speech),
            sound_clock: ClockDivider::new(1, 4),
            audio_buffer: SampleRing::with_capacity(2048),
            clock: 0,
            watchdog_count: 0,
            mo_bank_changes: Vec::with_capacity(16),
            mo_shadow: Vec::new(),
            mo_shadow_bands: Vec::with_capacity(16),
        }
    }

    // -- ROM install (the game wrapper decodes its manifest and hands us images) --

    /// Install the 0x88000-byte `maincpu` image: 68010 program at 000000-07FFFF
    /// into the ROM region, slapstic ROM (080000-087FFF) held outside the map.
    pub fn load_program(&mut self, image: &[u8]) {
        self.map.load_region(Region::Rom, &image[0x00000..0x80000]);
        // The slapstic ROM is held outside the map: the bus picks a bank per
        // access via the slapstic state machine (see `slapstic_read`).
        self.slapstic_rom.copy_from_slice(&image[0x80000..0x88000]);
    }

    /// Decode the motherboard alpha (text/HUD) font ROM into the tile cache.
    pub fn load_alpha(&mut self, alpha_rom: &[u8]) {
        self.alpha_cache = decode_gfx(alpha_rom, 0, ALPHA_TILE_COUNT, &ALPHA_LAYOUT);
    }

    /// Build the playfield + motion-object tile banks and PROM remap from the
    /// (already inverted) tile ROM and the two mapping PROMs.
    pub fn load_gfx(&mut self, prom: &[u8], tiles: &[u8]) {
        self.playfield = build_tile_gfx(prom, tiles);
    }

    /// Load the M6502 sound program into the sound board.
    pub fn load_sound(&mut self, sound_image: &[u8]) {
        self.sound.load_rom(sound_image);
    }

    // -- Bring-up diagnostics (used by the headless boot-check example) ------

    pub fn clock(&self) -> u64 {
        self.clock
    }

    /// Sound board state (held_reset, cycles, command_pending, response_pending).
    pub fn sound_debug(&self) -> (bool, u64, bool, bool) {
        self.sound.debug_state()
    }

    /// (EEPROM bytes != 0xFF, total EEPROM byte writes accepted) — bring-up.
    pub fn eeprom_debug(&self) -> (usize, u64) {
        let nonff = self.eeprom.iter().filter(|&&b| b != 0xFF).count();
        (nonff, self.eeprom_writes)
    }

    /// Non-zero byte counts in (palette, alpha, playfield) RAM — for headless
    /// bring-up diagnostics.
    pub fn video_ram_stats(&self) -> (usize, usize, usize) {
        let nz = |r| self.map.region_data(r).iter().filter(|&&b| b != 0).count();
        (
            nz(Region::Palette),
            nz(Region::Alpha),
            nz(Region::Playfield),
        )
    }

    // -- NVRAM (2804 EEPROM) --------------------------------------------------

    pub fn nvram(&self) -> &[u8] {
        &self.eeprom
    }

    pub fn load_nvram(&mut self, data: &[u8]) {
        let len = data.len().min(self.eeprom.len());
        self.eeprom[..len].copy_from_slice(&data[..len]);
    }

    // -- Control / status decode ---------------------------------------------

    /// 0x860001 audio/video control latch. Bit 7 drives the sound-CPU reset line
    /// (1 = run, 0 = hold); bit 2 selects the playfield tile bank; bits 5-3
    /// select the motion-object bank.
    fn bankselect_w(&mut self, data: u8) {
        let old_mo = (self.bankselect >> 3) & 7;
        self.bankselect = data;
        self.sound.set_reset(data & 0x80 == 0);

        // Record a mid-frame motion-object bank change so the compositor renders
        // each scanline band with its live bank. The change takes effect on the
        // next scanline (the beam has already drawn the current one), matching
        // the hardware's partial-render-then-switch behavior.
        let new_mo = (self.bankselect >> 3) & 7;
        if new_mo != old_mo {
            let frame_cycle = self.clock % TIMING.cycles_per_frame();
            let line = (frame_cycle / TIMING.cycles_per_scanline) as u16 + 1;
            match self.mo_bank_changes.last_mut() {
                Some(last) if last.0 == line => last.1 = new_mo,
                _ => self.mo_bank_changes.push((line, new_mo)),
            }
        }
    }

    /// True while the beam is in vertical blank (scanline ≥ `VBLANK_SCANLINE`).
    fn in_vblank(&self) -> bool {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();
        let scanline = (frame_cycle / TIMING.cycles_per_scanline) as u16;
        scanline >= VBLANK_SCANLINE
    }

    /// F60000 switch port (word). Active-low: idle bits read 1. Bit 4 is the
    /// live VBLANK line (0 during blank); bit 7 (68KBUF, active-high) is set
    /// while a sound command is latched but unread by the sound CPU.
    pub(crate) fn read_f60000(&self) -> u16 {
        let mut low = self.f60000_buttons;
        if self.in_vblank() {
            low &= !0x10; // VBLANK active-low
        } else {
            low |= 0x10;
        }
        if self.sound.command_pending() {
            low |= 0x80;
        } else {
            low &= !0x80;
        }
        0xFF00 | low as u16
    }

    /// Scanline-interrupt state read at 0x2E0000 (bit 7): set while a
    /// motion-object scanline interrupt (IRQ3) is asserted.
    pub(crate) fn int3_state(&self) -> u16 {
        if self.scanline_int { 0x0080 } else { 0x0000 }
    }

    /// Whether any motion-object timer entry in the active sprite bank targets
    /// `scanline` — i.e. IRQ3 should be asserted there. Timer entries are flagged
    /// by 0xFFFF in word[1]; word[0] gives the height and Y, and the interrupt
    /// fires at the top of that sprite's band: `256 - (word0>>5) - vsize*8 - 1`.
    pub(crate) fn timer_irq_at_scanline(&self, scanline: u16) -> bool {
        let mob = self.map.region_data(Region::Mob);
        let bank_base = ((self.bankselect >> 3) & 7) as usize * 256; // words
        let word = |wi: usize| u16::from_be_bytes([mob[wi * 2], mob[wi * 2 + 1]]);

        let mut visited = [false; 64];
        let mut link = 0usize;
        for _ in 0..64 {
            if visited[link] {
                break;
            }
            visited[link] = true;
            if word(bank_base + 0x40 + link) == 0xFFFF {
                let w0 = word(bank_base + link);
                let vsize = (w0 & 0x0F) as i32 + 1;
                let ypos = (256 - (w0 >> 5) as i32 - vsize * 8 - 1) & 0x1FF;
                if ypos == scanline as i32 {
                    return true;
                }
            }
            link = (word(bank_base + 0xC0 + link) & 0x3F) as usize;
        }
        false
    }

    /// Read a word from the slapstic-banked window (080000-087FFF) using the
    /// bank the slapstic currently presents. The state machine is driven
    /// separately by [`Slapstic::test`] on every *data* access (see
    /// [`bus_observe_data_access`](Self::bus_observe_data_access)); opcode
    /// prefetches read through without perturbing it. The window is mirrored ×4,
    /// so the bank offset is just the low 13 bits of the address.
    fn slapstic_read(&self, addr: u32) -> u16 {
        let bank = self.slapstic.current_bank() as usize;
        let base = bank * 0x2000 + (addr as usize & 0x1FFE);
        u16::from_be_bytes([self.slapstic_rom[base], self.slapstic_rom[base + 1]])
    }

    /// Set the analog-joystick interrupt (IRQ2) line. Driven by the game wrapper
    /// from its ADC0809 (end-of-conversion gated by the joystick-IRQ enable).
    pub(crate) fn set_int2(&mut self, asserted: bool) {
        self.int2 = asserted;
    }

    /// Effective autovector interrupt level (the 68000 takes the highest
    /// pending). IRQ6 = sound response, IRQ4 = VBLANK, IRQ3 = motion-object
    /// scanline (SLIP), IRQ2 = ADC/analog joystick.
    pub(crate) fn interrupt_level(&self) -> u8 {
        if self.sound.response_pending() {
            6
        } else if self.video_int {
            4
        } else if self.scanline_int {
            3
        } else if self.int2 {
            2
        } else {
            0
        }
    }

    // -----------------------------------------------------------------------
    // Rendering (full-frame compositor)
    // -----------------------------------------------------------------------

    /// Render the current frame as palette indices, then resolve to RGB. The
    /// layers composite the way the hardware merges them: the 64×64 playfield
    /// tilemap is the opaque background, motion objects merge over it with
    /// priority/translucency, and the 64×32 alpha (text/HUD) tilemap draws on
    /// top (transparent pen 0 unless the cell forces layer 0). Working in the
    /// shared 1024-entry palette-index space (alpha 0x000 / motion 0x100 /
    /// playfield 0x200 / translucent 0x300) is what lets the priority merge
    /// inspect playfield pens — so the compositor builds an index buffer and
    /// only converts to RGB at the end.
    pub fn render(&self, buffer: &mut [u8]) {
        let w = TIMING.display_width as usize;
        let h = TIMING.display_height as usize;

        // Decode the IRGB-4444 palette (1024 entries) for this frame.
        let pal = self.map.region_data(Region::Palette);
        let mut palette_rgb = [(0u8, 0u8, 0u8); 1024];
        for (i, slot) in palette_rgb.iter_mut().enumerate() {
            let raw = u16::from_be_bytes([pal[i * 2], pal[i * 2 + 1]]);
            *slot = irgb4444_to_rgb(raw);
        }

        // Layer 1: playfield → a per-pixel palette-index buffer.
        let mut index = vec![0u16; w * h];
        self.render_playfield(&mut index, w, h);

        // Layer 2: motion objects, merged over the playfield.
        self.render_motion_objects(&mut index, w, h);

        // Layer 3: alpha on top, then resolve every pixel to RGB.
        let alpha = self.map.region_data(Region::Alpha);
        for sy in 0..h {
            for sx in 0..w {
                let mut idx = index[sy * w + sx];

                // Alpha (text/HUD), drawn 1:1 from the origin.
                let a_cell = (sy / 8) * 64 + (sx / 8);
                let a_data = u16::from_be_bytes([alpha[a_cell * 2], alpha[a_cell * 2 + 1]]);
                let a_code = (a_data & 0x3FF) as usize;
                let a_color = ((a_data >> 10) & 0x07) as usize;
                let a_opaque = a_data & 0x2000 != 0;
                let a_pen = self.alpha_cache.pixel(a_code & 0x1FF, sx % 8, sy % 8);
                if a_pen != 0 || a_opaque {
                    idx = (a_color * 4 + a_pen as usize) as u16;
                }

                let (r, g, b) = palette_rgb[idx as usize & 0x3FF];
                let o = (sy * w + sx) * 3;
                buffer[o] = r;
                buffer[o + 1] = g;
                buffer[o + 2] = b;
            }
        }
    }

    /// Rasterise the 64×64 playfield tilemap into a palette-index buffer. Each
    /// 8×8 cell carries a flip/tile-select word; the PROM remap yields the gfx
    /// bank, tile code, and colour. The map is 512×512 and wraps; the visible
    /// origin is the X/Y scroll. The index is `0x200 + colour*8 + pen` — the
    /// playfield palette bank (see [`render`](Self::render)).
    fn render_playfield(&self, index: &mut [u16], w: usize, h: usize) {
        let pf_ram = self.map.region_data(Region::Playfield);
        // Playfield tile bank from the 0x860001 control latch (bit 2).
        let tile_bank = ((self.bankselect >> 2) & 1) as usize;
        let xscroll = self.xscroll as usize;
        let yscroll = self.yscroll as usize;

        for sy in 0..h {
            for sx in 0..w {
                let src_x = (sx + xscroll) & 0x1FF;
                let src_y = (sy + yscroll) & 0x1FF;
                let pf_cell = (src_y / 8) * 64 + (src_x / 8);
                let pf_data = u16::from_be_bytes([pf_ram[pf_cell * 2], pf_ram[pf_cell * 2 + 1]]);
                let lookup =
                    self.playfield.lookup[(pf_data >> 8) as usize & 0x7F | (tile_bank << 7)];
                let bank_id = ((lookup >> 8) & 0x0F) as usize;
                let code = (((lookup & 0xFF) as usize) << 8) | (pf_data & 0xFF) as usize;
                let palcolor = ((lookup >> 12) & 0x0F) as usize;
                // Bit 15 of the cell word horizontally mirrors the 8×8 tile.
                let hflip = pf_data & 0x8000 != 0;

                if let Some(bank) = self.playfield.banks.get(bank_id) {
                    let tx = if hflip { 7 - src_x % 8 } else { src_x % 8 };
                    let pen = bank.cache.pixel(code % bank.cache.count(), tx, src_y % 8);
                    let color = 0x20 + (palcolor << (bank.bpp - 3));
                    index[sy * w + sx] = ((0x100 + color * 8 + pen as usize) & 0x3FF) as u16;
                }
            }
        }
    }

    /// Walk the active motion-object bank's linked list, rasterise each sprite,
    /// and merge it over the playfield index buffer.
    ///
    /// A bank is 64 entries of 4 words in *split* layout: entry N's four words
    /// sit 0x40 words apart at base+N, base+0x40+N, base+0x80+N, base+0xC0+N.
    /// Word[0] = X-flip / Y-pos / height-1, word[1] = colour:code (0xFFFF marks a
    /// timer entry, not a sprite), word[2] = priority / X-pos, word[3] = link to
    /// the next entry. The list is followed from entry 0 until it loops or 56
    /// entries are visited. The sprite layer has a fixed yscroll of 256 and no
    /// xscroll; positions wrap in a 512×512 space.
    ///
    /// Merge (matching the hardware's screen_update): a high-priority sprite pen
    /// blends through the translucent bank (0x300 + pf_pen<<4 + mo_pen) unless its
    /// pen is 1; a low-priority sprite draws over the playfield unless that pixel
    /// is one of colour 0's priority pens (the 0x840000 mask), which lets the
    /// playfield stand in front of sprites.
    fn render_motion_objects(&self, index: &mut [u16], w: usize, h: usize) {
        const TRANSPARENT: u16 = 0xFFFF;
        const PRIORITY_BIT: u16 = 0x1000; // mobitmap priority flag (shift 12)

        let mut mo = vec![TRANSPARENT; w * h];

        // Draw from the vblank snapshot: the sprite RAM and band log as they
        // stood when the beam finished the visible area, before the game
        // rebuilt the list for the next frame. Without a snapshot — a direct
        // render that never stepped the board (unit tests) — fall back to live
        // state, and to the current bank when no bands were logged either.
        let live = self.map.region_data(Region::Mob);
        let mob: &[u8] = if self.mo_shadow.is_empty() {
            live
        } else {
            &self.mo_shadow
        };
        let word = |wi: usize| u16::from_be_bytes([mob[wi * 2], mob[wi * 2 + 1]]);

        let fallback = [(0u16, (self.bankselect >> 3) & 7)];
        let logged = if self.mo_shadow.is_empty() {
            &self.mo_bank_changes
        } else {
            &self.mo_shadow_bands
        };
        let bands: &[(u16, u8)] = if logged.is_empty() { &fallback } else { logged };

        for (bi, &(start_line, bank)) in bands.iter().enumerate() {
            let y_start = start_line as usize;
            let y_end = bands
                .get(bi + 1)
                .map_or(h, |&(next, _)| (next as usize).min(h));
            if y_start >= y_end {
                continue; // zero-height or off-screen (vblank) band
            }
            let bank_base = bank as usize * 256; // words

            let mut visited = [false; 64];
            let mut link = 0usize;
            for _ in 0..56 {
                if visited[link] {
                    break;
                }
                visited[link] = true;
                let w0 = word(bank_base + link);
                let w1 = word(bank_base + 0x40 + link);
                let w2 = word(bank_base + 0x80 + link);
                let w3 = word(bank_base + 0xC0 + link);
                // 0xFFFF in word[1] is a scanline timer, not a sprite.
                if w1 != 0xFFFF {
                    self.draw_mo_entry(&mut mo, w, h, y_start, y_end, w0, w1, w2, PRIORITY_BIT);
                }
                link = (w3 & 0x3F) as usize;
            }
        }

        // Merge the sprite bitmap over the playfield.
        let priority_pens = self.priority_pens;
        for (dst, &m) in index.iter_mut().zip(mo.iter()) {
            if m == TRANSPARENT {
                continue;
            }
            let pf = *dst;
            if m & PRIORITY_BIT != 0 {
                // High priority → translucent blend, unless the sprite pen is 1.
                if m & 0x0F != 1 {
                    *dst = 0x300 + ((pf & 0x0F) << 4) + (m & 0x0F);
                }
            } else if pf & 0xF8 != 0 || priority_pens & (1 << (pf & 0x07)) == 0 {
                // Low priority → draw unless the playfield pixel is a colour-0
                // priority pen.
                *dst = m;
            }
        }
    }

    /// Rasterise one motion object (1 tile wide, `height` tiles tall) into the
    /// sprite index buffer `mo`, honouring horizontal flip and the transparent
    /// pen 0. Only rows within the scanline band `[y_start, y_end)` are written,
    /// so a sprite that straddles a mid-frame bank switch is cut at the boundary
    /// like the real beam. The palette index is `0x100 + palcolor*16 + pen`, with
    /// the priority flag OR'd in for the merge step.
    #[allow(clippy::too_many_arguments)]
    fn draw_mo_entry(
        &self,
        mo: &mut [u16],
        w: usize,
        h: usize,
        y_start: usize,
        y_end: usize,
        w0: u16,
        w1: u16,
        w2: u16,
        prio: u16,
    ) {
        let ml = self.playfield.mo_lookup[(w1 >> 8) as usize];
        let bank_id = ((ml >> 8) & 0x0F) as usize;
        let Some(bank) = self.playfield.banks.get(bank_id) else {
            return;
        };
        let base_code = (((ml & 0xFF) as usize) << 8) | (w1 & 0xFF) as usize;
        let pal_base = 0x100 + ((ml >> 12) & 0x0F) * 16;
        let prio_flag = if w2 & 0x8000 != 0 { prio } else { 0 };

        let height = ((w0 & 0x000F) as usize) + 1;
        let hflip = w0 & 0x8000 != 0;
        // Sprite layer: xscroll 0, yscroll 256; positions wrap in 512×512.
        let mut xpos = ((w2 >> 5) & 0x1FF) as i32;
        let mut ypos = -(((w0 >> 5) & 0x1FF) as i32) - 256 - (height as i32) * 8;
        xpos &= 0x1FF;
        ypos &= 0x1FF;
        if xpos >= w as i32 {
            xpos -= 512;
        }
        if ypos >= h as i32 {
            ypos -= 512;
        }

        let count = bank.cache.count();
        for ty in 0..height {
            let code = (base_code + ty) % count;
            for py in 0..8usize {
                let dy = ypos + (ty * 8 + py) as i32;
                // Clip to the screen and to this band's scanline range.
                if dy < y_start as i32 || dy >= y_end as i32 {
                    continue;
                }
                for px in 0..8usize {
                    let dx = xpos + px as i32;
                    if dx < 0 || dx >= w as i32 {
                        continue;
                    }
                    let tx = if hflip { 7 - px } else { px };
                    let pen = bank.cache.pixel(code, tx, py);
                    if pen == 0 {
                        continue; // transparent pen
                    }
                    mo[dy as usize * w + dx as usize] = (pal_base + pen as u16) | prio_flag;
                }
            }
        }
    }

    /// Render this frame's framebuffer (RGB24). Delegated to by the wrapper's
    /// `Renderable`.
    pub fn render_frame(&self, buffer: &mut [u8]) {
        self.render(buffer);
    }

    // -----------------------------------------------------------------------
    // Stepping
    // -----------------------------------------------------------------------

    /// Advance the board one main-CPU cycle. The wrapper passes a `bus` (itself)
    /// so game-specific input ports can be intercepted before falling through to
    /// [`bus_read`](Self::bus_read)/[`bus_write`](Self::bus_write).
    /// Work that only happens on the first cycle of a scanline: the VBLANK
    /// interrupt (IRQ4) and the motion-object scanline interrupt (IRQ3).
    ///
    /// Called once per scanline from [`run_scanlines`] and, for the debugger's
    /// single-step path, from [`tick`] when the clock lands on a boundary.
    fn begin_scanline(&mut self, scanline: u16) {
        // Start a fresh motion-object bank log for the new frame, seeded with
        // the bank in effect at scanline 0 (carried from the previous frame).
        if scanline == 0 {
            self.mo_bank_changes.clear();
            self.mo_bank_changes.push((0, (self.bankselect >> 3) & 7));
        }

        // VBLANK raises IRQ4 on the first blanked line; IRQ3 tracks whether a
        // motion-object timer targets this line (a one-scanline pulse, like the
        // int3/int3off timer pair).
        if scanline == VBLANK_SCANLINE {
            self.video_int = true;
            self.snapshot_motion_objects();
        }
        self.scanline_int = self.timer_irq_at_scanline(scanline);
    }

    /// Per-cycle board work that runs before the CPU, with no frame-position
    /// test in it.
    fn begin_cycle_inner(&mut self, cpu: &M68000) {
        // Latch watchpoint attribution context before CPU execution.
        if self.map.debug_active() {
            let pc = cpu.at_instruction_boundary().then_some(cpu.pc);
            self.map.latch_access_context(self.clock, pc);
        }
    }

    /// Board work after the CPU's cycle: the sound board and the clock.
    fn end_cycle(&mut self) {
        // The sound board runs at 1/4 the main CPU rate.
        if self.sound_clock.tick() {
            self.sound.tick();
        }

        self.clock += 1;
    }

    /// Latch the motion-object state the beam just scanned out, at the moment
    /// vblank begins. The game spends vblank rebuilding the display list into
    /// the back bank and then swaps banks to publish it, so by the time the
    /// frame boundary comes round the live sprite RAM and bank describe the
    /// *next* frame. See [`mo_shadow`](Self::mo_shadow).
    fn snapshot_motion_objects(&mut self) {
        let mob = self.map.region_data(Region::Mob);
        if self.mo_shadow.len() == mob.len() {
            self.mo_shadow.copy_from_slice(mob);
        } else {
            self.mo_shadow = mob.to_vec();
        }
        self.mo_shadow_bands.clear();
        self.mo_shadow_bands
            .extend_from_slice(&self.mo_bank_changes);
    }

    /// Report the number of main-CPU instruction boundaries this tick (0 or 1)
    /// for the debugger's step accounting.
    /// Report the number of main-CPU instruction boundaries this tick (0 or 1)
    /// for the debugger's step accounting. The CPU lives on the machine, which
    /// passes it back in.
    pub fn instruction_boundaries(cpu: &M68000) -> u32 {
        u32::from(cpu.at_instruction_boundary())
    }

    /// Advance the per-frame watchdog. System 1 reboots after 8 VBLANKs without a
    /// strobe to 0x880001; returns true when that timeout is reached so the
    /// wrapper can drive a full machine reset.
    pub fn advance_watchdog(&mut self) -> bool {
        self.watchdog_count = self.watchdog_count.saturating_add(1);
        self.watchdog_count >= 8
    }

    /// Drain the sound board's mixed audio (POKEY + YM2151 FM, mono), strip the
    /// DC with a one-pole high-pass (cutoff ≈ 35 Hz), then scale and clamp to
    /// signed 16-bit into the pending audio buffer. Called once per frame.
    pub fn end_frame_audio(&mut self) {
        let (mut x1, mut y1) = self.audio_dc;
        let samples = self.sound.drain_audio();
        self.audio_buffer.extend(samples.iter().map(|&x| {
            let y = x - x1 + 0.995 * y1;
            x1 = x;
            y1 = y;
            (y * 2.0 * 32767.0).clamp(-32767.0, 32767.0) as i16
        }));
        self.audio_dc = (x1, y1);
    }

    /// Copy pending audio into the frontend's buffer. Delegated to by the
    /// wrapper's `AudioSource`.
    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.audio_buffer.pop_front_into(buffer)
    }

    /// Reset the shared board state (everything but the CPU, which the machine
    /// owns and resets against this board). EEPROM contents are non-volatile
    /// and survive reset.
    pub fn reset(&mut self) {
        self.slapstic.reset();
        self.sound.reset();
        self.sound_clock.reset();
        self.audio_buffer.clear();
        self.xscroll = 0;
        self.yscroll = 0;
        self.priority_pens = 0;
        self.bankselect = 0;
        self.eeprom_unlocked = false;
        self.f60000_buttons = 0xFF;
        self.video_int = false;
        self.scanline_int = false;
        self.int2 = false;
        self.audio_dc = (0.0, 0.0);
        self.watchdog_count = 0;
        self.mo_bank_changes.clear();
        self.mo_shadow.clear();
        self.mo_shadow_bands.clear();
    }

    // -----------------------------------------------------------------------
    // Shared bus decode (the game wrapper's `Bus` forwards non-game addresses)
    // -----------------------------------------------------------------------

    pub(crate) fn bus_is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    /// The slapstic snoops the address bus of every access the CPU drives — data
    /// reads/writes *and* instruction prefetches (the protection arms itself by
    /// prefetching code at magic addresses) — anywhere in the map, since its
    /// `test_any` patterns can land in RAM, and at the exact byte address (so
    /// consecutive byte accesses present distinct odd/even addresses, like the
    /// real chip's pins). Read/write is irrelevant: the PAL only decodes address
    /// lines.
    pub(crate) fn bus_observe_data_access(
        &mut self,
        _master: BusMaster,
        addr: u32,
        _is_write: bool,
    ) {
        self.slapstic.test(addr);
    }

    /// Shared read decode. Game-specific input windows (F20000 trackballs,
    /// F40000 ADC) are handled by the wrapper before it forwards here.
    pub(crate) fn bus_read(&mut self, master: BusMaster, addr: u32) -> u16 {
        // The slapstic state machine is driven by `bus_observe_data_access`
        // (called by the CPU for data accesses only); a read here just returns
        // the bank it currently presents.
        let val = match addr {
            // Backed ROM / RAM windows.
            0x00_0000..=0x07_FFFF
            | 0x40_0000..=0x40_1FFF
            | 0x90_0000..=0x9F_FFFF
            | 0xA0_0000..=0xA0_3FFF
            | 0xB0_0000..=0xB0_07FF => self.map.read_bus_word_be(addr),
            0x08_0000..=0x08_7FFF => self.slapstic_read(addr),
            0x2E_0000..=0x2E_0001 => self.int3_state(),
            0xF0_0000..=0xF0_03FF => self.eeprom[((addr >> 1) & 0x1FF) as usize] as u16,
            0xF6_0000..=0xF6_0003 => self.read_f60000(),
            0xFC_0000..=0xFC_0001 => self.sound.read_response() as u16,
            _ => 0xFFFF,
        };
        self.map.watch_read(0, master, addr, val as u32, 2);
        val
    }

    /// Register a read the wrapper serviced itself (a game input port), so
    /// watchpoints still fire on it. The value is whatever the wrapper returned.
    pub(crate) fn note_read(&mut self, master: BusMaster, addr: u32, val: u16) {
        self.map.watch_read(0, master, addr, val as u32, 2);
    }

    /// Shared write decode.
    pub(crate) fn bus_write(&mut self, master: BusMaster, addr: u32, data: u16) {
        self.map.watch_write(0, master, addr, data as u32, 2);
        let byte = (data & 0xFF) as u8;
        match addr {
            0x00_0000..=0x07_FFFF => {} // fixed ROM, ignore
            0x08_0000..=0x08_7FFF => {} // slapstic window: ROM, bank state driven by observe
            0x40_0000..=0x40_1FFF
            | 0x90_0000..=0x9F_FFFF
            | 0xA0_0000..=0xA0_3FFF
            | 0xB0_0000..=0xB0_07FF => self.map.write_bus_word_be(addr, data),
            0x80_0000..=0x80_0001 => self.xscroll = data,
            0x82_0000..=0x82_0001 => self.yscroll = data,
            0x84_0000..=0x84_0001 => self.priority_pens = data,
            0x86_0000..=0x86_0001 => self.bankselect_w(byte),
            0x88_0000..=0x88_0001 => self.watchdog_count = 0, // watchdog reset
            0x8A_0000..=0x8A_0001 => self.video_int = false,  // VBLANK IRQ4 ack
            0x8C_0000..=0x8C_0001 => self.eeprom_unlocked = true, // EEPROM unlock
            // 2804 writes are gated by the unlock latch and re-lock after one byte.
            0xF0_0000..=0xF0_03FF if self.eeprom_unlocked => {
                self.eeprom[((addr >> 1) & 0x1FF) as usize] = byte;
                self.eeprom_unlocked = false;
                self.eeprom_writes += 1;
            }
            0xF8_0000..=0xF8_0001 => {} // Sound latch (RoadBlasters only)
            0xFE_0000..=0xFE_0001 => self.sound.write_command(byte),
            _ => {}
        }
    }

    pub(crate) fn bus_check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            irq_level: self.interrupt_level(),
            // 0xFF ⇒ the 68000 core autovectors (vector 24 + level).
            irq_vector: 0xFF,
            ..Default::default()
        }
    }
}

impl Saveable for AtariSystem1Board {
    fn save_state(&self, w: &mut StateWriter) {
        // The CPU is saved by the machine, which owns it.
        self.slapstic.save_state(w);
        self.sound.save_state(w);
        self.sound_clock.save_state(w);
        w.write_bytes(self.map.region_data(Region::Ram));
        w.write_bytes(self.map.region_data(Region::CartRam));
        w.write_bytes(self.map.region_data(Region::Playfield));
        w.write_bytes(self.map.region_data(Region::Mob));
        w.write_bytes(self.map.region_data(Region::Alpha));
        w.write_bytes(self.map.region_data(Region::Palette));
        w.write_bytes(&self.eeprom);
        w.write_u16_le(self.xscroll);
        w.write_u16_le(self.yscroll);
        w.write_u16_le(self.priority_pens);
        w.write_u8(self.bankselect);
        w.write_bool(self.eeprom_unlocked);
        w.write_u8(self.f60000_buttons);
        w.write_bool(self.video_int);
        w.write_bool(self.scanline_int);
        w.write_bool(self.int2);
        w.write_u64_le(self.clock);
        w.write_u8(self.watchdog_count);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        // The CPU is loaded by the machine, which owns it.
        self.slapstic.load_state(r)?;
        self.sound.load_state(r)?;
        self.sound_clock.load_state(r)?;
        r.read_bytes_into(self.map.region_data_mut(Region::Ram))?;
        r.read_bytes_into(self.map.region_data_mut(Region::CartRam))?;
        r.read_bytes_into(self.map.region_data_mut(Region::Playfield))?;
        r.read_bytes_into(self.map.region_data_mut(Region::Mob))?;
        r.read_bytes_into(self.map.region_data_mut(Region::Alpha))?;
        r.read_bytes_into(self.map.region_data_mut(Region::Palette))?;
        r.read_bytes_into(&mut self.eeprom)?;
        self.xscroll = r.read_u16_le()?;
        self.yscroll = r.read_u16_le()?;
        self.priority_pens = r.read_u16_le()?;
        self.bankselect = r.read_u8()?;
        self.eeprom_unlocked = r.read_bool()?;
        self.f60000_buttons = r.read_u8()?;
        self.video_int = r.read_bool()?;
        self.scanline_int = r.read_bool()?;
        self.int2 = r.read_bool()?;
        self.clock = r.read_u64_le()?;
        self.watchdog_count = r.read_u8()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bankselect_logs_midframe_mo_bank_changes() {
        let mut board = AtariSystem1Board::new(103, false);
        // Frame start seeds the log with the current MO bank at scanline 0.
        board.mo_bank_changes = vec![(0, 0)];

        // Move the beam to ~scanline 100 and switch the MO bank (bits 5-3):
        // 0x10 → MO bank 2. The change takes effect on the next line (101).
        board.clock = 100 * TIMING.cycles_per_scanline;
        board.bankselect_w(0x10);
        assert_eq!(board.mo_bank_changes, vec![(0, 0), (101, 2)]);

        // A second switch on the same line replaces rather than appends.
        board.bankselect_w(0x18); // MO bank 3
        assert_eq!(board.mo_bank_changes, vec![(0, 0), (101, 3)]);

        // A write that leaves the MO bank unchanged (only the playfield bank bit
        // toggles) is not logged.
        board.bankselect_w(0x18 | 0x04);
        assert_eq!(board.mo_bank_changes, vec![(0, 0), (101, 3)]);
    }

    #[test]
    fn motion_objects_follow_midframe_bank_switch() {
        let mut board = AtariSystem1Board::new(103, false);
        let w = TIMING.display_width as usize;

        // One gfx bank (id 1) with a pen-5 top-left pixel; MO colour byte 0 maps
        // entries to it.
        let mut cache = GfxCache::new(1, 8, 8);
        cache.set_pixel(0, 0, 0, 5);
        board.playfield.banks.push(GfxBank { cache, bpp: 4 });
        board.playfield.mo_lookup[0] = 1 << 8;

        // Sprite pen 5, palcolor 0 → motion palette index 0x105 = pure green.
        let palette = board.map.region_data_mut(Region::Palette);
        palette[0x105 * 2] = 0xF0;
        palette[0x105 * 2 + 1] = 0xF0;

        // A sprite in MO bank 0 at screen y=0, and one in MO bank 1 at y=200
        // (word[0] encodes Y; words 1-3 = 0 → colour/code 0, no prio, link 0).
        let mob = board.map.region_data_mut(Region::Mob);
        mob[0] = 0x1F; // bank 0 entry 0 word[0] = 0x1F00 → y 0
        mob[1] = 0x00;
        mob[0x200] = 0x06; // bank 1 entry 0 word[0] = 0x0600 → y 200
        mob[0x201] = 0x00;

        // The frame showed bank 0 up top, then switched to bank 1 at line 120.
        board.mo_bank_changes = vec![(0, 0), (120, 1)];

        let (dw, dh) = TIMING.display_size();
        let mut buf = vec![0u8; (dw * dh * 3) as usize];
        board.render_frame(&mut buf);

        let px = |x: usize, y: usize| {
            let o = (y * w + x) * 3;
            (buf[o], buf[o + 1], buf[o + 2])
        };
        // Both sprites appear — each in the band whose bank holds it. (Before the
        // per-band fix, only the final bank rendered, dropping the top sprite.)
        assert_eq!(px(0, 0), (0, 254, 0), "bank-0 sprite shows in the top band");
        assert_eq!(
            px(0, 200),
            (0, 254, 0),
            "bank-1 sprite shows in the bottom band"
        );
    }

    #[test]
    fn motion_objects_render_from_the_vblank_snapshot() {
        // Both System 1 games double-buffer the display list: they rebuild it
        // during vblank and publish it by swapping the MO bank. Rendering at the
        // frame boundary therefore sees sprite RAM that already describes the
        // *next* scanout, so the compositor must draw from the state latched
        // when vblank began — otherwise it draws the half-built back buffer with
        // the pre-publish bank and the sprites vanish.
        let mut board = AtariSystem1Board::new(103, false);
        let w = TIMING.display_width as usize;

        let mut cache = GfxCache::new(1, 8, 8);
        cache.set_pixel(0, 0, 0, 5);
        board.playfield.banks.push(GfxBank { cache, bpp: 4 });
        board.playfield.mo_lookup[0] = 1 << 8;

        // Sprite pen 5, palcolor 0 → motion palette index 0x105 = pure green.
        let palette = board.map.region_data_mut(Region::Palette);
        palette[0x105 * 2] = 0xF0;
        palette[0x105 * 2 + 1] = 0xF0;

        // The list the beam actually scanned out: one sprite at y=0 in bank 0.
        let mob = board.map.region_data_mut(Region::Mob);
        mob[0] = 0x1F; // bank 0 entry 0 word[0] = 0x1F00 → y 0
        mob[1] = 0x00;
        board.mo_bank_changes = vec![(0, 0)];

        // Vblank begins: latch that state.
        board.snapshot_motion_objects();

        // Now the game rebuilds the list for the next frame — it tears down
        // bank 0's entry and stages a new sprite in bank 1 at y=200 — and
        // publishes it by swapping the live MO bank to 1.
        let mob = board.map.region_data_mut(Region::Mob);
        mob[0] = 0x00;
        mob[1] = 0x00;
        mob[0x200] = 0x06; // bank 1 entry 0 word[0] = 0x0600 → y 200
        mob[0x201] = 0x00;
        board.bankselect_w(0x08); // MO bank 1

        let (dw, dh) = TIMING.display_size();
        let mut buf = vec![0u8; (dw * dh * 3) as usize];
        board.render_frame(&mut buf);

        let px = |x: usize, y: usize| {
            let o = (y * w + x) * 3;
            (buf[o], buf[o + 1], buf[o + 2])
        };
        assert_eq!(
            px(0, 0),
            (0, 254, 0),
            "the scanned-out sprite draws from the vblank snapshot"
        );
        assert_eq!(
            px(0, 200),
            (0, 0, 0),
            "the list staged during vblank belongs to the next frame, not this one"
        );
    }

    /// Companion to [`motion_objects_render_from_the_vblank_snapshot`], which
    /// latches the snapshot by hand and so only covers the compositor half of
    /// the double-buffer fix. This one drives the beam across the start of
    /// vblank and checks `tick` takes the snapshot itself — drop that call and
    /// the shadow stays empty, the compositor falls back to live state, and the
    /// sprites vanish exactly as they did originally.
    #[test]
    fn vblank_tick_latches_the_motion_object_snapshot() {
        let mut sys = crate::marble::MarbleSystem::new();

        let mut cache = GfxCache::new(1, 8, 8);
        cache.set_pixel(0, 0, 0, 5);
        sys.board.playfield.banks.push(GfxBank { cache, bpp: 4 });
        sys.board.playfield.mo_lookup[0] = 1 << 8;

        let palette = sys.board.map.region_data_mut(Region::Palette);
        palette[0x105 * 2] = 0xF0;
        palette[0x105 * 2 + 1] = 0xF0;

        // The list the beam is scanning out: one sprite at y=0 in bank 0.
        let mob = sys.board.map.region_data_mut(Region::Mob);
        mob[0] = 0x1F;
        mob[1] = 0x00;
        sys.board.mo_bank_changes = vec![(0, 0)];

        // Park the beam one cycle short of vblank, then step across it so the
        // scanline boundary that latches the snapshot actually runs.
        sys.board.clock = VBLANK_SCANLINE as u64 * TIMING.cycles_per_scanline - 1;
        for _ in 0..2 {
            sys.step_cycle();
        }
        assert!(
            !sys.board.mo_shadow.is_empty(),
            "entering vblank must latch the motion-object state"
        );

        // The game now tears down bank 0 and publishes a rebuilt list in bank 1.
        let mob = sys.board.map.region_data_mut(Region::Mob);
        mob[0] = 0x00;
        mob[1] = 0x00;
        sys.board.bankselect_w(0x08);

        let (dw, dh) = TIMING.display_size();
        let mut buf = vec![0u8; (dw * dh * 3) as usize];
        sys.board.render_frame(&mut buf);
        assert_eq!(
            &buf[0..3],
            &[0, 254, 0],
            "the sprite the beam scanned out still draws after the bank swap"
        );
    }

    #[test]
    fn playfield_tile_horizontal_flip() {
        // Bit 15 of a playfield cell word mirrors its 8×8 tile left-to-right.
        let mut board = AtariSystem1Board::new(103, false);
        let w = TIMING.display_width as usize;

        // One real gfx bank (index 1 — banks[0] is the blank placeholder) whose
        // tile 0 has a single pen-5 pixel at its left edge (x=0, y=0).
        let mut cache = GfxCache::new(1, 8, 8);
        cache.set_pixel(0, 0, 0, 5);
        board.playfield.banks.push(GfxBank { cache, bpp: 4 });
        // Playfield lookup 0 → gfx bank 1, code 0, palcolor 0.
        board.playfield.lookup[0] = 1 << 8;

        // Playfield pen 5, palcolor 0 → index 0x100 + 0x20*8 + 5 = 0x205 = green.
        let palette = board.map.region_data_mut(Region::Palette);
        palette[0x205 * 2] = 0xF0;
        palette[0x205 * 2 + 1] = 0xF0;

        // Cell 0 (screen x 0..7): no flip. Cell 1 (screen x 8..15): bit 15 set.
        let pf = board.map.region_data_mut(Region::Playfield);
        pf[2] = 0x80; // cell 1 word high byte → hflip

        let (dw, dh) = TIMING.display_size();
        let mut buf = vec![0u8; (dw * dh * 3) as usize];
        board.render_frame(&mut buf);

        let px = |x: usize, y: usize| {
            let o = (y * w + x) * 3;
            (buf[o], buf[o + 1], buf[o + 2])
        };
        const GREEN: (u8, u8, u8) = (0, 254, 0);
        // Unflipped cell: the pen stays at the tile's left edge.
        assert_eq!(px(0, 0), GREEN, "unflipped pen at left edge");
        assert_ne!(px(7, 0), GREEN, "unflipped right edge is blank");
        // Flipped cell: the same pen moves to the tile's right edge.
        assert_ne!(px(8, 0), GREEN, "flipped left edge is blank");
        assert_eq!(px(15, 0), GREEN, "flipped pen at right edge");
    }
}
