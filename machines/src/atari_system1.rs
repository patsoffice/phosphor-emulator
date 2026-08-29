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
use phosphor_core::core::{AccessKind, AddressSpace32};
use phosphor_core::core::{
    Bus, BusMaster, ClockDomainName as Clk, ClockTree, DomainId, TimingConfig,
};
use phosphor_core::cpu::m68000::{M68kVariant, M68000};
use phosphor_core::device::slapstic::Slapstic;
use phosphor_core::gfx::decode::{GfxCache, GfxLayout, decode_gfx};
use phosphor_macros::{BusDebug, MemoryRegion, Saveable};

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

/// The board's crystal and everything divided out of it.
///
/// One 14.318181 MHz crystal (four times the NTSC colour subcarrier) feeds the
/// whole board: the 68010 and the pixel clock at /2, the sound 6502 and its
/// POKEY at /8, the YM2151 at /4.
///
/// The CPU rate is exactly 7159090.5 Hz, so `TIMING.cpu_clock_hz` is the
/// nearest whole hertz to it rather than an exact division.
///
/// The TMS5220 hangs off master/2 through a divider Port B bit 4 reselects at
/// runtime: /11 nominal, /9 when the bit is set. It is declared here at the
/// nominal /22 of the crystal, and `atari_system1_sound.rs` moves it with
/// `set_domain_hz` when the bit changes.
pub fn clock_tree() -> phosphor_core::core::ClockTree {
    use phosphor_core::core::RootId;
    let mut t = ClockTree::new(14_318_181);
    let cpu = t.add_domain(Clk::Cpu, RootId::MAIN, 1, 2); // 7.15909 MHz 68010
    let dot = t.add_domain(Clk::Pixel, RootId::MAIN, 1, 2); // same clock as the CPU
    t.add_domain(Clk::SoundCpu, RootId::MAIN, 1, 8); // 1.789772 MHz 6502
    t.add_domain(Clk::Pokey, RootId::MAIN, 1, 8); // POKEY shares the sound CPU's rate
    t.add_domain(Clk::Psg, RootId::MAIN, 1, 4); // YM2151, twice the sound CPU
    t.add_domain(Clk::Speech, RootId::MAIN, 1, 22); // TMS5220 at master/2/11
    t.set_step_domain(cpu);
    // CPU and pixel clock are the same signal, so HTOTAL is the cycle count
    // with nothing to round.
    t.set_raster(dot, 456, 0);
    t
}

/// First scanline of vertical blank (`vbstart`); VBLANK asserts IRQ4 here.
pub(crate) const VBLANK_SCANLINE: u16 = 240;

/// Native visible raster. There is no vertical offset on this board: the beam
/// draws native row `n` during scanline `n`, and [`VBLANK_SCANLINE`] onward is
/// blanked.
const VISIBLE_WIDTH: usize = TIMING.display_width as usize; // 336
const VISIBLE_HEIGHT: usize = TIMING.display_height as usize; // 240

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
#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct AtariSystem1Board {
    /// The address space persists its own writable regions: work RAM,
    /// cartridge RAM, and the playfield, motion-object, alpha and palette RAMs.
    #[debug_map(cpu = 0)]
    #[save(id = 1)]
    pub(crate) map: AddressSpace32,

    /// Slapstic protection PAL gating the 080000-087FFF ROM window.
    #[save(id = 2)]
    pub(crate) slapstic: Slapstic,
    /// The 32 KB (4 × 8 KB bank) slapstic ROM the window selects between.
    #[save_skip]
    pub(crate) slapstic_rom: Vec<u8>,

    /// EEPROM 2804 (512 bytes, low byte at F00000-F003FF), gated by `eeprom_unlocked`.
    #[save(id = 3)]
    pub(crate) eeprom: [u8; 512],
    /// 0x8C0001 EEPROM unlock latch. The 2804 re-locks after each write.
    #[save(id = 4)]
    pub(crate) eeprom_unlocked: bool,
    /// Count of accepted EEPROM byte writes (bring-up diagnostic; not saved).
    #[save_skip]
    eeprom_writes: u64,

    /// Decoded 8×8 2bpp alpha (text/HUD) font tiles. Not CPU-addressable.
    #[save_skip]
    pub(crate) alpha_cache: GfxCache,
    /// Decoded playfield tile banks + the PROM remap lookup. Not CPU-addressable.
    #[save_skip]
    pub(crate) playfield: PlayfieldGfx,

    // Video control latches (consumed by the video pipeline).
    #[save(id = 5)]
    pub(crate) xscroll: u16,
    #[save(id = 6)]
    pub(crate) yscroll: u16,
    #[save(id = 7)]
    pub(crate) priority_pens: u16,
    /// 0x860001 audio/video control: bit 7 = sound-CPU reset, bits 5-3 =
    /// motion-object bank, bit 2 = playfield tile bank.
    #[save(id = 8)]
    pub(crate) bankselect: u8,

    // F60000 switch port low byte (active-low; bits 0/1 = start, bit 6 = service).
    // Bits 4 (VBLANK) and 7 (sound buffer) are computed live in `read_f60000`.
    #[save(id = 9)]
    pub(crate) f60000_buttons: u8,

    // VBLANK interrupt latch (IRQ4), held until acked via 0x8A0001.
    #[save(id = 10)]
    pub(crate) video_int: bool,
    /// Scanline motion-object interrupt (IRQ3 / "SLIP"). Asserted for the one
    /// scanline a motion-object timer entry targets; also read back at 0x2E0000
    /// bit 7. Recomputed at every scanline boundary from the active sprite bank,
    /// but only on a cartridge that has the circuit -- see
    /// [`has_scanline_int`](Self::has_scanline_int).
    #[save(id = 11)]
    pub(crate) scanline_int: bool,
    /// Whether this cartridge carries the circuit that generates IRQ3 at all.
    ///
    /// It is not part of the shared board: it exists on the LSI cartridges 2, 3
    /// and 4 and on the cockpit boards, and is absent from the TTL and LSI
    /// cartridges. Road Runner has it and Marble Madness does not, which is why
    /// MAME splits the driver into `atarisy1r_state`, whose `update_timers`
    /// walks the display list, and `atarisy1_state`, whose `update_timers` is an
    /// empty function so the interrupt is never scheduled.
    ///
    /// Fixed at construction by [`with_scanline_interrupt`](Self::with_scanline_interrupt),
    /// so a load rebuilds it from the factory rather than from the save.
    #[save_skip]
    pub(crate) has_scanline_int: bool,
    /// Analog-joystick interrupt (IRQ2). Games with an ADC0809 (Road Runner et
    /// al.) drive this from the converter's end-of-conversion line, gated by the
    /// joystick-IRQ enable; games without one (Marble) leave it false.
    #[save(id = 12)]
    pub(crate) int2: bool,

    /// One-pole DC-blocker state (prev input, prev output) for the audio mix —
    /// removes the POKEY's unipolar DC so the FM music gets full headroom, the
    /// way the cabinet's AC-coupled amplifier does.
    ///
    /// Two samples of filter history, which a load re-establishes within a
    /// sample or two of resuming.
    #[save_skip]
    audio_dc: (f32, f32),

    /// M6502 sound board (POKEY + YM2151 + optional speech + inter-CPU latches).
    #[debug_device("Sound")]
    #[save(id = 13)]
    pub(crate) sound: AtariSystem1Sound,
    /// Sound CPU runs at 1/4 the main CPU rate.
    /// The board's clock tree, as [`clock_tree`] declares it, stepped in
    /// main-CPU cycles. The speech section holds its own copy of the same
    /// declaration stepped in sound-CPU cycles, which is the rate its loop
    /// counts in.
    #[debug_device("Clocks")]
    #[save(id = 14)]
    clocks: ClockTree,
    /// A handle into the clock tree, which is itself saved.
    #[save_skip]
    sound_dom: DomainId,
    /// Samples already mixed and waiting for the frontend to drain, which the
    /// next frame refills.
    #[save_skip]
    audio_buffer: SampleRing<i16>,

    #[save(id = 15)]
    pub(crate) clock: u64,
    #[save(id = 16)]
    pub(crate) watchdog_count: u8,

    /// Display framebuffer (native 336x240 RGB), filled one row at a time as
    /// the beam reaches each visible scanline.
    ///
    /// A row holds what the beam drew on that line, out of the playfield RAM,
    /// the scroll registers, the *live* motion-object RAM and bank, the alpha
    /// RAM and the palette as they stood at that line's boundary.
    ///
    /// This is what retired `mo_shadow`. That field held a copy of the sprite
    /// RAM taken at the start of vblank, and it was correct for a whole-frame
    /// render: the frame boundary is at the *end* of vblank, by which point the
    /// game has rebuilt its display list into the back bank and swapped, so the
    /// live RAM described the next frame rather than the one being presented.
    /// A row drawn while the beam is on it has no such problem — the list it
    /// reads is the list the beam saw — so the snapshot, its band log and the
    /// per-band compositing that consumed them are all gone.
    ///
    /// Derived output, so not saved, and not seeded after a load: the rows of
    /// the next frame overwrite every one of them.
    #[save_skip]
    pub(crate) framebuffer: Vec<u8>,
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
        let clocks = clock_tree();
        let sound_dom = clocks.find(Clk::SoundCpu).expect("declared sound domain");
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
            has_scanline_int: false,
            int2: false,
            audio_dc: (0.0, 0.0),
            sound: AtariSystem1Sound::new(speech),
            clocks,
            sound_dom,
            audio_buffer: SampleRing::with_capacity(2048),
            clock: 0,
            watchdog_count: 0,
            framebuffer: vec![0u8; VISIBLE_WIDTH * VISIBLE_HEIGHT * 3],
        }
    }

    /// Declare that this cartridge carries the motion-object scanline-interrupt
    /// circuit, so a timer entry in the display list raises IRQ3.
    ///
    /// Opt-in rather than opt-out because the absence is the more common case
    /// and the safer default: a board built without it behaves like the TTL and
    /// LSI cartridges, which never assert the line. See
    /// [`has_scanline_int`](Self::has_scanline_int).
    pub fn with_scanline_interrupt(mut self) -> Self {
        self.has_scanline_int = true;
        self
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
        self.bankselect = data;
        self.sound.set_reset(data & 0x80 == 0);

        // A mid-frame motion-object bank change used to be logged here as a
        // `(scanline, bank)` band for the whole-frame compositor to replay.
        // Nothing needs logging now: each row reads the live bank when it is
        // drawn, and a row is drawn at the *start* of its scanline, so a write
        // during scanline N is first seen by row N+1 — which is the same
        // one-line delay the band log applied by hand.
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
    ///
    /// The address decodes on every cartridge, so this stays mapped even where
    /// the interrupt circuit is absent; there it reads a constant zero, because
    /// nothing ever sets the latch behind it.
    pub(crate) fn int3_state(&self) -> u16 {
        if self.scanline_int { 0x0080 } else { 0x0000 }
    }

    /// Whether any motion-object timer entry in the active sprite bank targets
    /// `scanline` — i.e. IRQ3 should be asserted there. Timer entries are flagged
    /// by 0xFFFF in word[1]; word[0] gives the height and Y, and the interrupt
    /// fires at the top of that sprite's band: `256 - (word0>>5) - vsize*8 - 1`.
    ///
    /// This answers what the *display list* says, which is a property of the
    /// list and not of the cartridge. Whether the board can act on it is
    /// [`has_scanline_int`](Self::has_scanline_int), and the caller applies it.
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

    /// Copy the latest framebuffer into the frontend's `buffer`.
    ///
    /// This does not draw. Each visible row was composited at its own scanline
    /// boundary in [`render_scanline`](Self::render_scanline) as the beam
    /// reached it.
    pub fn render(&self, buffer: &mut [u8]) {
        buffer.copy_from_slice(&self.framebuffer);
    }

    /// Draw one visible scanline into the framebuffer, out of the video state
    /// as it stands at that line's boundary.
    ///
    /// The layers composite the way the hardware merges them: the 64×64
    /// playfield tilemap is the opaque background, motion objects merge over it
    /// with priority/translucency, and the 64×32 alpha (text/HUD) tilemap draws
    /// on top (transparent pen 0 unless the cell forces layer 0). Working in the
    /// shared 1024-entry palette-index space (alpha 0x000 / motion 0x100 /
    /// playfield 0x200 / translucent 0x300) is what lets the priority merge
    /// inspect playfield pens, so the row is built as indices and resolved once
    /// at the end.
    ///
    /// Every register the layers read is read here — the scroll pair, the
    /// playfield and motion-object bank selects, the priority-pen mask and the
    /// palette — so a mid-frame write to any of them splits the picture at the
    /// row it lands on. Road Runner reprograms the motion-object bank mid-frame
    /// and that is now simply a live read.
    ///
    /// **No sprite sampling lead is added.** W3 established from the SP-277
    /// sheets that the object path is a doubled horizontal line buffer, so the
    /// list for row `r` is read while the beam is on row `r - 1`; but the
    /// `ypos` expression below carries no such term, the sheets do not
    /// establish the buffers' phase (only that they alternate), and the
    /// reference driver's own +2 is documented there as a kludge over the +1 it
    /// calls correct. Adding a lead would move every sprite pixel on the
    /// strength of that, so the constant is left as it was.
    ///
    /// No empty-cache guard: `alpha_cache` is allocated at full size in
    /// [`new`](Self::new) and the playfield banks are looked up with `get`,
    /// which already yields `None` on a board that never loaded ROMs.
    fn render_scanline(&mut self, sy: usize) {
        let mut index = [0u16; VISIBLE_WIDTH];
        self.draw_playfield_row(&mut index, sy);
        self.draw_motion_objects_row(&mut index, sy);
        self.draw_alpha_row(&mut index, sy);

        // Resolve to RGB against the palette as it stands on this row. The
        // one-entry memo is worth having because a tilemap row holds long runs
        // of one index, and it keeps this from decoding 336 IRGB words a row.
        let pal = self.map.region_data(Region::Palette);
        let out = sy * VISIBLE_WIDTH * 3;
        let mut last = usize::MAX;
        let mut rgb = (0u8, 0u8, 0u8);
        for (x, &idx) in index.iter().enumerate() {
            let i = idx as usize & 0x3FF;
            if i != last {
                last = i;
                rgb = irgb4444_to_rgb(u16::from_be_bytes([pal[i * 2], pal[i * 2 + 1]]));
            }
            let o = out + x * 3;
            self.framebuffer[o] = rgb.0;
            self.framebuffer[o + 1] = rgb.1;
            self.framebuffer[o + 2] = rgb.2;
        }
    }

    /// Rasterise one row of the 64×64 playfield tilemap into the index row.
    ///
    /// Each 8×8 cell carries a flip/tile-select word; the PROM remap yields the
    /// gfx bank, tile code and colour. The map is 512×512 and wraps; the visible
    /// origin is the X/Y scroll. The index is `0x200 + colour*8 + pen` — the
    /// playfield palette bank.
    ///
    /// The map row and the line inside the tile are fixed by `sy`, so they come
    /// out of the loop, and the cell word, its PROM lookup and the tile's
    /// 8-pixel line are fetched once per tile column rather than once per pixel:
    /// 42 fetches a row instead of 336. Cached on the cell index, so the scroll
    /// wrap at 512 needs no case of its own.
    fn draw_playfield_row(&self, index: &mut [u16; VISIBLE_WIDTH], sy: usize) {
        let pf_ram = self.map.region_data(Region::Playfield);
        // Playfield tile bank from the 0x860001 control latch (bit 2).
        let tile_bank = ((self.bankselect >> 2) & 1) as usize;
        let xscroll = self.xscroll as usize;
        let yscroll = self.yscroll as usize;

        let src_y = (sy + yscroll) & 0x1FF;
        let row_base = (src_y / 8) * 64;
        let ty = src_y % 8;

        let mut cached_cell = usize::MAX;
        let mut hflip = false;
        let mut pal_base = 0usize;
        let mut line: Option<&[u8]> = None;

        for (sx, out) in index.iter_mut().enumerate() {
            let src_x = (sx + xscroll) & 0x1FF;
            let pf_cell = row_base + src_x / 8;

            if pf_cell != cached_cell {
                cached_cell = pf_cell;
                let pf_data = u16::from_be_bytes([pf_ram[pf_cell * 2], pf_ram[pf_cell * 2 + 1]]);
                let lookup =
                    self.playfield.lookup[(pf_data >> 8) as usize & 0x7F | (tile_bank << 7)];
                let bank_id = ((lookup >> 8) & 0x0F) as usize;
                let code = (((lookup & 0xFF) as usize) << 8) | (pf_data & 0xFF) as usize;
                let palcolor = ((lookup >> 12) & 0x0F) as usize;
                // Bit 15 of the cell word horizontally mirrors the 8×8 tile.
                hflip = pf_data & 0x8000 != 0;
                line = self.playfield.banks.get(bank_id).map(|bank| {
                    let color = 0x20 + (palcolor << (bank.bpp - 3));
                    pal_base = 0x100 + color * 8;
                    bank.cache.row_slice(code % bank.cache.count(), ty)
                });
            }

            if let Some(line) = line {
                let tx = if hflip { 7 - src_x % 8 } else { src_x % 8 };
                *out = ((pal_base + line[tx] as usize) & 0x3FF) as u16;
            }
        }
    }

    /// Draw one row of the 64×32 alpha (text/HUD) tilemap over the index row.
    ///
    /// Drawn 1:1 from the origin, transparent on pen 0 unless the cell's bit 13
    /// forces it opaque. Same per-tile fetch as the playfield: 42 cell words a
    /// row instead of 336.
    fn draw_alpha_row(&self, index: &mut [u16; VISIBLE_WIDTH], sy: usize) {
        let alpha = self.map.region_data(Region::Alpha);
        let row_base = (sy / 8) * 64;
        let ty = sy % 8;

        let mut cached_cell = usize::MAX;
        let mut opaque = false;
        let mut pal_base = 0usize;
        let mut line: &[u8] = &[];

        for (sx, out) in index.iter_mut().enumerate() {
            let a_cell = row_base + sx / 8;
            if a_cell != cached_cell {
                cached_cell = a_cell;
                let a_data = u16::from_be_bytes([alpha[a_cell * 2], alpha[a_cell * 2 + 1]]);
                let a_code = (a_data & 0x3FF) as usize;
                opaque = a_data & 0x2000 != 0;
                pal_base = ((a_data >> 10) & 0x07) as usize * 4;
                line = self.alpha_cache.row_slice(a_code & 0x1FF, ty);
            }
            let pen = line[sx % 8];
            if pen != 0 || opaque {
                *out = (pal_base + pen as usize) as u16;
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
    fn draw_motion_objects_row(&self, index: &mut [u16; VISIBLE_WIDTH], sy: usize) {
        const TRANSPARENT: u16 = 0xFFFF;
        const PRIORITY_BIT: u16 = 0x1000; // mobitmap priority flag (shift 12)

        // Read the LIVE sprite RAM and the LIVE bank. There is no snapshot and
        // no band log any more: the row is drawn while the beam is on it, so
        // the list it reads is the list the beam saw, and a write to the active
        // bank partway down the screen changes the rows below it the way the
        // hardware does.
        let mob = self.map.region_data(Region::Mob);
        let word = |wi: usize| u16::from_be_bytes([mob[wi * 2], mob[wi * 2 + 1]]);
        let bank_base = ((self.bankselect >> 3) & 7) as usize * 256; // words

        let mut mo = [TRANSPARENT; VISIBLE_WIDTH];
        let mut visited = [false; 64];
        let mut link = 0usize;
        for _ in 0..56 {
            if visited[link] {
                break;
            }
            visited[link] = true;
            let w0 = word(bank_base + link);
            let w3 = word(bank_base + 0xC0 + link);

            // Word[0] carries both the Y position and the height, so whether
            // this entry is on this line is decidable before the other two
            // words are read and the PROM is consulted. The link still has to
            // be followed either way — the list is a chain, not an array, which
            // is why this is an early-out rather than an index.
            if let Some(ty) = Self::mo_row_of(w0, sy) {
                let w1 = word(bank_base + 0x40 + link);
                let w2 = word(bank_base + 0x80 + link);
                // EVERY entry is drawn, including one flagged 0xFFFF in word[1].
                // That flag is what the scanline-interrupt comparator watches
                // for, and that comparator is on the CARTRIDGE: it exists on
                // LSI carts 2, 3, 4 and the cockpit board and nowhere else,
                // which is why `has_scanline_int` is a per-machine fact. This
                // renderer is on the motherboard and serves every cartridge, so
                // it cannot be conditioned on a feature only some of them have:
                // on a Marble Madness board nothing whatever watches word[1],
                // and the entry is simply a sprite. The hardware draws it and
                // the cartridge separately takes the interrupt.
                //
                // We suppressed it until 2026-08-28, which contradicted the
                // note on `timer_irq_at_scanline` saying the flag is a property
                // of the list rather than of the cartridge. It cost 64 pixels
                // an 8x8 block at (0, 121) in every frame of the Road Runner
                // picture comparison against a reference capture
                // (phosphor-emulator-h52k).
                //
                // Real games do not show the block because they park unused
                // entries off the left edge at X 504: measured over a recorded
                // Road Runner game, all 984 timer-entry samples sit there, as
                // do 10,706 dormant ordinary sprites. So the parking is a
                // general convention rather than a workaround for this, and it
                // is why nothing noticed until a conformance ROM put a timer at
                // X 0. What is NOT settled is the schematic itself; see the
                // issue.
                self.draw_mo_entry_row(&mut mo, w0, w1, w2, ty, PRIORITY_BIT);
            }
            link = (w3 & 0x3F) as usize;
        }

        // Merge the sprite row over the playfield row.
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

    /// Which pixel row of the object described by `w0` falls on screen row
    /// `sy`, or `None` when the object does not cross that line.
    ///
    /// The sprite layer has a fixed yscroll of 256 and no xscroll, and
    /// positions wrap in a 512×512 space, so this is the placement the
    /// whole-frame renderer computed per object, inverted to answer one row.
    fn mo_row_of(w0: u16, sy: usize) -> Option<usize> {
        let height = ((w0 & 0x000F) as usize) + 1;
        let mut ypos = -(((w0 >> 5) & 0x1FF) as i32) - 256 - (height as i32) * 8;
        ypos &= 0x1FF;
        if ypos >= VISIBLE_HEIGHT as i32 {
            ypos -= 512;
        }
        let dy = sy as i32 - ypos;
        (dy >= 0 && dy < (height * 8) as i32).then_some(dy as usize)
    }

    /// Rasterise the one line of a motion object that falls on this row into
    /// the sprite index row, honouring horizontal flip and the transparent pen
    /// 0. `ty` is that line within the object, from [`mo_row_of`](Self::mo_row_of).
    /// The palette index is `0x100 + palcolor*16 + pen`, with the priority flag
    /// OR'd in for the merge step.
    fn draw_mo_entry_row(
        &self,
        mo: &mut [u16; VISIBLE_WIDTH],
        w0: u16,
        w1: u16,
        w2: u16,
        ty: usize,
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
        let hflip = w0 & 0x8000 != 0;

        let mut xpos = ((w2 >> 5) & 0x1FF) as i32;
        if xpos >= VISIBLE_WIDTH as i32 {
            xpos -= 512;
        }

        // One tile of the object's vertical stack, and one line of that tile.
        let code = (base_code + ty / 8) % bank.cache.count();
        let line = bank.cache.row_slice(code, ty % 8);
        for px in 0..8usize {
            let dx = xpos + px as i32;
            if dx < 0 || dx >= VISIBLE_WIDTH as i32 {
                continue;
            }
            let pen = line[if hflip { 7 - px } else { px }];
            if pen == 0 {
                continue; // transparent pen
            }
            mo[dx as usize] = (pal_base + pen as u16) | prio_flag;
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
    pub(crate) fn begin_scanline(&mut self, scanline: u16) {
        // VBLANK raises IRQ4 on the first blanked line; IRQ3 tracks whether a
        // motion-object timer targets this line (a one-scanline pulse, like the
        // int3/int3off timer pair).
        if scanline == VBLANK_SCANLINE {
            self.video_int = true;
        }
        self.scanline_int = self.has_scanline_int && self.timer_irq_at_scanline(scanline);

        // The row the beam is about to draw. The motion-object snapshot that
        // used to be taken here is gone: a row drawn on its own scanline reads
        // the list the beam saw, so there is nothing to preserve for later.
        if (scanline as usize) < VISIBLE_HEIGHT {
            self.render_scanline(scanline as usize);
        }
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
        if self.clocks.tick(self.sound_dom) {
            self.sound.tick();
        }

        self.clock += 1;
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
        self.clocks.reset();
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
        // The framebuffer is not cleared: the next frame's rows overwrite every
        // one of them as the beam reaches them.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Scanline the motion-object timer entry built by [`timer_entry_board`]
    /// targets. Any visible line works; 100 is comfortably inside the picture.
    const TIMER_LINE: u16 = 100;

    /// A board whose display list holds exactly one motion-object timer entry,
    /// aimed at [`TIMER_LINE`].
    ///
    /// Entry 0 of bank 0 (`bankselect` is 0 after construction). Word 1 is the
    /// 0xFFFF flag that marks the entry a timer rather than a sprite. Word 0
    /// carries the height and Y that place the band: with the size nibble 0 the
    /// height is one tile, so `256 - (word0 >> 5) - 8 - 1` is the target, and
    /// `word0 = 147 << 5 = 0x1260` puts it on line 100. Word 3's link is 0,
    /// pointing the list at the entry already visited, which ends the walk.
    fn timer_entry_board(scanline_interrupt: bool) -> AtariSystem1Board {
        let mut board = AtariSystem1Board::new(103, false);
        if scanline_interrupt {
            board = board.with_scanline_interrupt();
        }
        let mob = board.map.region_data_mut(Region::Mob);
        mob[0x00] = 0x12; // word 0 = 0x1260
        mob[0x01] = 0x60;
        mob[0x80] = 0xFF; // word 0x40 = 0xFFFF: this entry is a timer
        mob[0x81] = 0xFF;
        board
    }

    /// The same display list on both cartridges, with opposite outcomes.
    ///
    /// The circuit that turns a timer entry into IRQ3 is on the LSI cartridges
    /// 2, 3 and 4 and on the cockpit boards, and is absent from the TTL and LSI
    /// cartridges that Marble Madness ships on. MAME splits the driver over
    /// exactly this: `atarisy1r_state::update_timers` walks the list and
    /// `atarisy1_state::update_timers` is an empty function.
    ///
    /// Asserting both halves is what makes this able to fail in both
    /// directions: drop the gate and Marble raises an interrupt its cartridge
    /// cannot, invert it and Road Runner loses one it depends on.
    #[test]
    fn the_motion_object_timer_interrupt_is_a_cartridge_option() {
        let mut roadrunner = timer_entry_board(true);
        roadrunner.begin_scanline(TIMER_LINE);
        assert!(
            roadrunner.scanline_int,
            "an LSI-cartridge board takes IRQ3 from a timer entry"
        );
        assert_eq!(roadrunner.interrupt_level(), 3, "IRQ3 reaches the CPU");
        assert_eq!(
            roadrunner.int3_state(),
            0x0080,
            "and reads back at 0x2E0000"
        );

        let mut marble = timer_entry_board(false);
        marble.begin_scanline(TIMER_LINE);
        assert!(
            !marble.scanline_int,
            "a cartridge without the circuit ignores the same entry"
        );
        assert_eq!(marble.interrupt_level(), 0, "no interrupt reaches the CPU");
        assert_eq!(
            marble.int3_state(),
            0x0000,
            "and 0x2E0000 reads a flat zero"
        );
    }

    /// The gate is on the cartridge, not on the line: a board that has the
    /// circuit still only asserts on the line the entry names.
    ///
    /// Without this, a gate stuck off would pass the Marble half of the test
    /// above for the wrong reason, and a `scanline_int` left latched from a
    /// previous line would pass the Road Runner half for the wrong reason.
    #[test]
    fn the_timer_interrupt_lasts_one_scanline() {
        let mut board = timer_entry_board(true);

        board.begin_scanline(TIMER_LINE - 1);
        assert!(!board.scanline_int, "not yet on the line before");
        board.begin_scanline(TIMER_LINE);
        assert!(board.scanline_int, "asserted on the line itself");
        board.begin_scanline(TIMER_LINE + 1);
        assert!(!board.scanline_int, "released on the line after");
    }

    // -----------------------------------------------------------------------
    // Per-scanline rendering (W4)
    // -----------------------------------------------------------------------

    const GREEN: (u8, u8, u8) = (0, 254, 0);

    /// Walk the beam over a whole frame's scanlines so every visible row is
    /// drawn. The picture only exists once the beam has passed over it, so a
    /// test that pokes video state has to scan before it can look.
    fn scan_frame(board: &mut AtariSystem1Board) {
        for s in 0..TIMING.total_scanlines as u16 {
            board.begin_scanline(s);
        }
    }

    fn px(board: &AtariSystem1Board, x: usize, y: usize) -> (u8, u8, u8) {
        let o = (y * VISIBLE_WIDTH + x) * 3;
        (
            board.framebuffer[o],
            board.framebuffer[o + 1],
            board.framebuffer[o + 2],
        )
    }

    /// A board with one gfx bank whose tile 0 has a pen-5 pixel at its top-left
    /// corner, and a palette in which the motion-object index for that pen is
    /// pure green.
    fn board_with_one_green_sprite_pen() -> AtariSystem1Board {
        let mut board = AtariSystem1Board::new(103, false);
        let mut cache = GfxCache::new(1, 8, 8);
        cache.set_pixel(0, 0, 0, 5);
        board.playfield.banks.push(GfxBank { cache, bpp: 4 });
        board.playfield.mo_lookup[0] = 1 << 8;

        // Sprite pen 5, palcolor 0 → motion palette index 0x105 = pure green.
        let palette = board.map.region_data_mut(Region::Palette);
        palette[0x105 * 2] = 0xF0;
        palette[0x105 * 2 + 1] = 0xF0;
        board
    }

    /// The motion-object bank is read live by each row, so switching it partway
    /// down the screen shows one bank's list above the switch and the other's
    /// below.
    ///
    /// This replaces two tests. `bankselect_logs_midframe_mo_bank_changes`
    /// asserted the `(scanline, bank)` band log that the whole-frame compositor
    /// replayed; that log is gone, because a row reads the live bank when it is
    /// drawn. The one-line delay it used to apply by hand falls out of the
    /// structure instead — a row is drawn at the *start* of its scanline, so a
    /// write during scanline N is first seen by row N+1 — and that is what the
    /// middle two assertions below pin.
    #[test]
    fn a_mid_frame_mo_bank_switch_changes_only_the_rows_below_it() {
        let mut board = board_with_one_green_sprite_pen();

        // A sprite in MO bank 0 at screen y=0, and one in MO bank 1 at y=200
        // (word[0] encodes Y; words 1-3 = 0 → colour/code 0, no prio, link 0).
        let mob = board.map.region_data_mut(Region::Mob);
        mob[0] = 0x1F; // bank 0 entry 0 word[0] = 0x1F00 → y 0
        mob[1] = 0x00;
        mob[0x200] = 0x06; // bank 1 entry 0 word[0] = 0x0600 → y 200
        mob[0x201] = 0x00;

        // Scan the top of the frame on bank 0, switch at scanline 120, finish.
        for s in 0..120u16 {
            board.begin_scanline(s);
        }
        board.clock = 120 * TIMING.cycles_per_scanline;
        board.bankselect_w(0x08); // MO bank 1
        for s in 120..TIMING.total_scanlines as u16 {
            board.begin_scanline(s);
        }

        assert_eq!(
            px(&board, 0, 0),
            GREEN,
            "the bank-0 sprite drew before the switch"
        );
        assert_eq!(
            px(&board, 0, 200),
            GREEN,
            "the bank-1 sprite draws after it"
        );
    }

    /// A write to the ACTIVE motion-object bank during active display changes
    /// what the beam draws from the next line on. This is
    /// `phosphor-emulator-x7rn`, which W3 opened against the whole-frame
    /// renderer: drawing the frame from one vblank snapshot made such a write
    /// invisible until the following frame. Road Runner depends on it.
    #[test]
    fn a_mid_frame_write_to_the_active_mo_bank_changes_only_the_rows_below_it() {
        let mut board = board_with_one_green_sprite_pen();

        // Entry 0 of the active bank places a sprite at y=0; entry 1 is unused
        // and entry 0 links to it, so the list is 0 -> 1 -> 1 (terminates).
        let mob = board.map.region_data_mut(Region::Mob);
        mob[0] = 0x1F; // word[0] = 0x1F00 → y 0
        mob[1] = 0x00;
        mob[0xC0 * 2] = 0x00; // entry 0 word[3] = link 1
        mob[0xC0 * 2 + 1] = 0x01;

        for s in 0..120u16 {
            board.begin_scanline(s);
        }
        // Mid-frame, the game rewrites entry 1 of the *active* bank to place a
        // second sprite at y=200. On hardware the beam picks it up below here.
        let mob = board.map.region_data_mut(Region::Mob);
        mob[2] = 0x06; // entry 1 word[0] = 0x0600 → y 200
        mob[3] = 0x00;
        for s in 120..TIMING.total_scanlines as u16 {
            board.begin_scanline(s);
        }

        assert_eq!(
            px(&board, 0, 0),
            GREEN,
            "the sprite that was already in the list drew at the top"
        );
        assert_eq!(
            px(&board, 0, 200),
            GREEN,
            "the sprite written mid-frame draws below the write"
        );
    }

    /// What the retired `mo_shadow` existed to guarantee, expressed against the
    /// structure that replaced it.
    ///
    /// Both System 1 games double-buffer the display list in software: they
    /// rebuild it during vblank and publish it by swapping the MO bank. A
    /// whole-frame render at the frame boundary therefore saw sprite RAM that
    /// already described the *next* scanout, which is why it needed a snapshot
    /// taken when vblank began. Rows drawn on their own scanlines need no
    /// snapshot: by the time the game rebuilds the list, the beam has passed.
    /// The concern is the same — the picture must show the list the beam
    /// scanned out — and this asserts it end to end.
    #[test]
    fn the_picture_holds_the_list_the_beam_scanned_out_not_the_one_staged_after() {
        let mut board = board_with_one_green_sprite_pen();

        // The list the beam scans out: one sprite at y=0 in bank 0.
        let mob = board.map.region_data_mut(Region::Mob);
        mob[0] = 0x1F;
        mob[1] = 0x00;
        scan_frame(&mut board);

        // Now the game does what it does in vblank: tear down bank 0's entry,
        // stage a new sprite in bank 1, and publish by swapping the bank.
        let mob = board.map.region_data_mut(Region::Mob);
        mob[0] = 0x00;
        mob[1] = 0x00;
        mob[0x200] = 0x06;
        mob[0x201] = 0x00;
        board.bankselect_w(0x08);

        assert_eq!(
            px(&board, 0, 0),
            GREEN,
            "the sprite the beam scanned out is still what the frame shows"
        );
        assert_eq!(
            px(&board, 0, 200),
            (0, 0, 0),
            "the list staged afterwards belongs to the next frame, not this one"
        );
    }

    /// A blanked line has no row to draw, and drawing one would run off the end
    /// of a framebuffer sized to the visible window.
    #[test]
    fn blanking_scanlines_have_no_row_to_draw() {
        let mut board = board_with_one_green_sprite_pen();
        let mob = board.map.region_data_mut(Region::Mob);
        mob[0] = 0x1F;
        mob[1] = 0x00;
        board.framebuffer.fill(0);
        for s in VBLANK_SCANLINE..TIMING.total_scanlines as u16 {
            board.begin_scanline(s);
        }
        assert!(
            board.framebuffer.iter().all(|&c| c == 0),
            "the blanked lines drew nothing"
        );
    }

    #[test]
    fn playfield_tile_horizontal_flip() {
        // Bit 15 of a playfield cell word mirrors its 8×8 tile left-to-right.
        let mut board = AtariSystem1Board::new(103, false);

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

        scan_frame(&mut board);

        let px = |x: usize, y: usize| px(&board, x, y);
        // Unflipped cell: the pen stays at the tile's left edge.
        assert_eq!(px(0, 0), GREEN, "unflipped pen at left edge");
        assert_ne!(px(7, 0), GREEN, "unflipped right edge is blank");
        // Flipped cell: the same pen moves to the tile's right edge.
        assert_ne!(px(8, 0), GREEN, "flipped left edge is blank");
        assert_eq!(px(15, 0), GREEN, "flipped pen at right edge");
    }
}
