//! Atari Marble Madness (1984) — the project's first 16-bit multi-CPU board.
//!
//! Marble Madness runs on **Atari System 1**: an MC68010 main CPU (7.15909 MHz)
//! over a sparse 24-bit address space, with an M6502 sound CPU (Phase 4). It is
//! a motherboard + game-cartridge design — the reset vectors live in the shared
//! **motherboard BIOS** at `0x000000`, which then jumps into the cartridge's
//! banked program ROMs. The board's protection is a **Slapstic** that
//! bank-switches the `0x080000-0x087FFF` ROM window (Phase 2).
//!
//! This module is built in phases (see the `marble-madness` beads epic). The
//! 68010 boot loop, memory map, control/IRQ registers, the **Slapstic** bank
//! switching, the **2804 EEPROM**, and the **playfield + alpha** video layers
//! are in place. Motion objects (Phase 3c), the sound CPU (Phase 4), and
//! trackball input (Phase 5) are still stubbed and filled in by their own issues.
//!
//! Closest existing template: [`crate::foodf`] (68000 + `AddressSpace32` + raster
//! IRQs + watchdog). This adds the BIOS/cartridge split and the richer I/O map.
//!
//! ## Main-CPU memory map (word bus, big-endian; base windows only)
//! ```text
//!   000000-07FFFF  Program ROM (BIOS @ 0, cartridge banks @ 0x10000+)
//!   080000-087FFF  Slapstic-banked ROM window (4 × 8 KB banks)
//!   2E0000         R  Sprite/MO scanline-interrupt state (bit 7)   [Phase 3d]
//!   400000-401FFF  R/W Work RAM
//!   800000         W  Playfield X scroll      820000  W  Playfield Y scroll
//!   840000         W  Playfield priority color mask
//!   860001         W  Audio/video control latch (sound reset, MO/PF banks)
//!   880001         W  Watchdog reset          8A0001  W  VBLANK IRQ ack
//!   8C0001         W  EEPROM unlock
//!   900000-9FFFFF  R/W Cartridge external RAM (unused by marble)
//!   A00000-A01FFF  R/W Playfield RAM    A02000-A02FFF  R/W Motion-object RAM
//!   A03000-A03FFF  R/W Alphanumerics RAM
//!   B00000-B007FF  R/W Palette RAM
//!   F00000-F003FF  R/W EEPROM 2804 (512 bytes, low byte)
//!   F20000-F20007  R  Trackballs               [Phase 5]
//!   F40000-F4001F  R/W Joystick/ADC (unused by marble)
//!   F60000         R  Switch inputs (start/service/VBLANK/sound-buffer)
//!   FC0001         R  Sound response read       FE0001  W  Sound command write   [Phase 4]
//! ```
//!
//! ## Byte registers on a word bus
//! The single-byte control registers sit at odd addresses (`860001`, `880001`,
//! `8A0001`, …). A 68000 byte write becomes a word read-modify-write at the even
//! base, so the board sees a word access at `860000` with the value in the low
//! byte — we decode on the even base and take `data & 0xFF`, exactly like
//! [`crate::foodf`].

use std::collections::HashMap;

use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    AnalogAxisKind, AudioSource, DefaultBinding, Direction, InputConfigurable, InputControl,
    InputEvent, InputId, InputKind, MachineCore, MouseControl, Nvram, Profilable, Renderable,
    SaveState,
};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace32};
use phosphor_core::core::{Bus, BusMaster, ClockDivider, TimingConfig};
use phosphor_core::cpu::m68000::{M68kVariant, M68000};
use phosphor_core::cpu::state::M68000State;
use phosphor_core::cpu::{Cpu, CpuStateTrait};
use phosphor_core::device::slapstic::Slapstic;
use phosphor_core::gfx::decode::{GfxCache, GfxLayout, decode_gfx};
use phosphor_macros::{BusDebug, MemoryRegion};

use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::marble_sound::MarbleSound;
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_low;

// ---------------------------------------------------------------------------
// Address-space regions (backed memory only; I/O is decoded in the Bus impl)
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
enum Region {
    /// Program ROM: BIOS @ 0, cartridge banks @ 0x10000+ (covers 000000-07FFFF).
    Rom = 1,
    Ram = 3,
    /// Cartridge external RAM (900000-9FFFFF). Unused by marble, but mapped so
    /// stray accesses are backed rather than faulting.
    CartRam = 4,
    Playfield = 5,
    Mob = 6,
    Alpha = 7,
    Palette = 8,
}

// ---------------------------------------------------------------------------
// ROM manifest ("marble" parent set, TTL Rev 2 motherboard BIOS)
// ---------------------------------------------------------------------------

/// All 68010 program chips, concatenated back-to-back in load order, then
/// de-interleaved into the big-endian `maincpu` image by [`load_maincpu_image`].
///
/// Order: BIOS even/odd, then the four cartridge banks (even/odd each), then the
/// two slapstic chips (even/odd). Each chip is 0x4000 bytes; even chip = high
/// byte of the 68k word, odd chip = low byte (standard 16-bit ROM interleave).
pub static MARBLE_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x30000,
    entries: &[
        // Motherboard BIOS (TTL Rev 2) — holds the reset vectors at 0x000000.
        RomEntry {
            name: "136032.205.l13",
            size: 0x4000,
            offset: 0x00000,
            crc32: &[0x88d0be26],
        },
        RomEntry {
            name: "136032.206.l12",
            size: 0x4000,
            offset: 0x04000,
            crc32: &[0x3c79ef05],
        },
        // Cartridge program banks (even/odd pairs).
        RomEntry {
            name: "136033.623",
            size: 0x4000,
            offset: 0x08000,
            crc32: &[0x284ed2e9],
        },
        RomEntry {
            name: "136033.624",
            size: 0x4000,
            offset: 0x0C000,
            crc32: &[0xd541b021],
        },
        RomEntry {
            name: "136033.625",
            size: 0x4000,
            offset: 0x10000,
            crc32: &[0x563755c7],
        },
        RomEntry {
            name: "136033.626",
            size: 0x4000,
            offset: 0x14000,
            crc32: &[0x860feeb3],
        },
        RomEntry {
            name: "136033.627",
            size: 0x4000,
            offset: 0x18000,
            crc32: &[0xd1dbd439],
        },
        RomEntry {
            name: "136033.628",
            size: 0x4000,
            offset: 0x1C000,
            crc32: &[0x957d6801],
        },
        RomEntry {
            name: "136033.229",
            size: 0x4000,
            offset: 0x20000,
            crc32: &[0xc81d5c14],
        },
        RomEntry {
            name: "136033.630",
            size: 0x4000,
            offset: 0x24000,
            crc32: &[0x687a09f7],
        },
        // Slapstic-banked ROM (even/odd).
        RomEntry {
            name: "136033.107",
            size: 0x4000,
            offset: 0x28000,
            crc32: &[0xf3b8745b],
        },
        RomEntry {
            name: "136033.108",
            size: 0x4000,
            offset: 0x2C000,
            crc32: &[0xe51eecaa],
        },
    ],
};

/// Alphanumerics character ROM (the shared motherboard font, 136032.104.f5):
/// 512 tiles, 8×8, 2bpp.
pub static MARBLE_ALPHA_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[RomEntry {
        name: "136032.104.f5",
        size: 0x2000,
        offset: 0x0000,
        crc32: &[0x7a29dc07],
    }],
};

/// 8×8 2bpp alpha tiles. The two bitplanes sit at bit offsets 0 and 4 within
/// each 16-bit row, MSB-first; `decode_gfx` wants LSB-first (entry 0 = pen bit
/// 0), so the plane list is reversed to `{4, 0}`.
const MARBLE_ALPHA_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[4, 0],
    x_offsets: &[0, 1, 2, 3, 8, 9, 10, 11],
    y_offsets: &[0, 16, 32, 48, 64, 80, 96, 112],
    char_increment: 128,
};

/// Number of 8×8 alpha tiles in the font ROM (0x2000 / 16 bytes per tile).
const ALPHA_TILE_COUNT: usize = 512;

/// Playfield / motion-object tile ROM ("tiles" region, 0x100000). Two 0x80000
/// banks; bank 1 holds five bitplanes, bank 2 three. The region is
/// `ROMREGION_INVERT | ROMREGION_ERASEFF`: erase to 0xFF, place the chips, then
/// invert the whole buffer (gaps 0xFF→0x00 = erase, data → inverted), see
/// `load_rom_set`. Bank 2's absent plane 3 thus reads 0, keeping sprite pens in
/// 0-7 rather than forcing pen bit 3.
pub static MARBLE_TILE_ROM: RomRegion = RomRegion {
    size: 0x100000,
    entries: &[
        // Bank 1: planes 0-4, each plane two 0x4000 ROMs, planes 0x10000 apart.
        RomEntry {
            name: "136033.137",
            size: 0x4000,
            offset: 0x00000,
            crc32: &[0x7a45f5c1],
        },
        RomEntry {
            name: "136033.138",
            size: 0x4000,
            offset: 0x04000,
            crc32: &[0x7e954a88],
        },
        RomEntry {
            name: "136033.139",
            size: 0x4000,
            offset: 0x10000,
            crc32: &[0x1eb1bb5f],
        },
        RomEntry {
            name: "136033.140",
            size: 0x4000,
            offset: 0x14000,
            crc32: &[0x8a82467b],
        },
        RomEntry {
            name: "136033.141",
            size: 0x4000,
            offset: 0x20000,
            crc32: &[0x52448965],
        },
        RomEntry {
            name: "136033.142",
            size: 0x4000,
            offset: 0x24000,
            crc32: &[0xb4a70e4f],
        },
        RomEntry {
            name: "136033.143",
            size: 0x4000,
            offset: 0x30000,
            crc32: &[0x7156e449],
        },
        RomEntry {
            name: "136033.144",
            size: 0x4000,
            offset: 0x34000,
            crc32: &[0x4c3e4c79],
        },
        RomEntry {
            name: "136033.145",
            size: 0x4000,
            offset: 0x40000,
            crc32: &[0x9062be7f],
        },
        RomEntry {
            name: "136033.146",
            size: 0x4000,
            offset: 0x44000,
            crc32: &[0x14566dca],
        },
        // Bank 2: planes 0-2, data 0x4000 into each plane slot (tiles 2048+).
        RomEntry {
            name: "136033.149",
            size: 0x4000,
            offset: 0x84000,
            crc32: &[0xb6658f06],
        },
        RomEntry {
            name: "136033.151",
            size: 0x4000,
            offset: 0x94000,
            crc32: &[0x84ee1c80],
        },
        RomEntry {
            name: "136033.153",
            size: 0x4000,
            offset: 0xa4000,
            crc32: &[0xdaa02926],
        },
    ],
};

/// Graphics-mapping PROMs ("proms" region, 0x400). `prom1` (136033.118) at 0x000
/// and `prom2` (136033.119) at 0x200 drive the per-tile bank / bpp / colour /
/// offset lookup. Entries 0-255 are the playfield, 256-511 the motion objects.
pub static MARBLE_PROM: RomRegion = RomRegion {
    size: 0x400,
    entries: &[
        RomEntry {
            name: "136033.118",
            size: 0x200,
            offset: 0x000,
            crc32: &[0x2101b0ed],
        },
        RomEntry {
            name: "136033.119",
            size: 0x200,
            offset: 0x200,
            crc32: &[0x19f6e767],
        },
    ],
};

/// M6502 sound program (Phase 4). 64 KB region with ROM at 0x8000-0xFFFF.
pub static MARBLE_SOUND_ROM: RomRegion = RomRegion {
    size: 0x10000,
    entries: &[
        RomEntry {
            name: "136033.421",
            size: 0x4000,
            offset: 0x8000,
            crc32: &[0x78153dc3],
        },
        RomEntry {
            name: "136033.422",
            size: 0x4000,
            offset: 0xC000,
            crc32: &[0x2e66300e],
        },
    ],
};

/// Build the 0x88000-byte `maincpu` image from the concatenated chips: the
/// 68010 program at 000000-07FFFF and the slapstic ROM at 080000-087FFF, with
/// each even/odd chip pair de-interleaved into big-endian words.
fn load_maincpu_image(rom_set: &RomSet) -> Result<Vec<u8>, RomLoadError> {
    let chips = MARBLE_PROGRAM_ROM.load(rom_set)?;
    let mut image = vec![0u8; 0x88000];
    // (dst_base, even_chip_offset, odd_chip_offset) for each 0x4000-byte pair.
    const PAIRS: [(usize, usize, usize); 6] = [
        (0x00000, 0x00000, 0x04000), // BIOS
        (0x10000, 0x08000, 0x0C000), // cartridge bank 0
        (0x18000, 0x10000, 0x14000), // cartridge bank 1
        (0x20000, 0x18000, 0x1C000), // cartridge bank 2
        (0x28000, 0x20000, 0x24000), // cartridge bank 3
        (0x80000, 0x28000, 0x2C000), // slapstic
    ];
    for (dst, even, odd) in PAIRS {
        for i in 0..0x4000 {
            image[dst + 2 * i] = chips[even + i]; // even address = high byte
            image[dst + 2 * i + 1] = chips[odd + i]; // odd address = low byte
        }
    }
    Ok(image)
}

// ---------------------------------------------------------------------------
// Playfield / motion-object GFX decode (PROM-driven bank + bpp selection)
// ---------------------------------------------------------------------------
//
// Each of the 256 playfield lookup entries is keyed by a remap PROM pair that
// yields a tile bank (1-7, only 1-2 populated on marble), a bit depth (4/5/6),
// a colour, and a code offset. The same machinery feeds the motion objects
// (Phase 3c).

// PROM1 / PROM2 bit assignments (the two graphics-mapping PROMs, 136033.118/119).
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
struct GfxBank {
    cache: GfxCache,
    bpp: u8,
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
struct PlayfieldGfx {
    lookup: [u16; 256],
    mo_lookup: [u16; 256],
    banks: Vec<GfxBank>,
}

impl PlayfieldGfx {
    fn empty() -> Self {
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
fn build_tile_gfx(prom: &[u8], tiles: &[u8]) -> PlayfieldGfx {
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
fn irgb4444_to_rgb(raw: u16) -> (u8, u8, u8) {
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
// Input IDs (minimal Phase-1 set; trackball + coins land in Phase 5 / 4)
// ---------------------------------------------------------------------------

pub const INPUT_START1: u8 = 0;
pub const INPUT_START2: u8 = 1;
/// Service / self-test switch (F60000 bit 6, active-low `PORT_SERVICE`).
pub const INPUT_SERVICE: u8 = 2;
/// Coin insert (sound port 0x1820 bit 0, active-low).
pub const INPUT_COIN: u8 = 3;
/// P1 trackball direction keys (the keyboard way to roll the ball).
pub const INPUT_P1_TRACK_LEFT: u8 = 4;
pub const INPUT_P1_TRACK_RIGHT: u8 = 5;
pub const INPUT_P1_TRACK_UP: u8 = 6;
pub const INPUT_P1_TRACK_DOWN: u8 = 7;

/// Counter step per frame for a held trackball direction key — a gentle roll
/// in the range of a normal mouse motion (the marble accelerates from sustained
/// input, so a small constant is plenty).
const TRACK_KEY_STEP: i32 = 6;
/// Max counter change applied per frame, so a fast flick can't alias the 8-bit
/// counter (the game reads the delta as a signed byte, valid to ±127).
const TRACK_MAX_STEP: i32 = 100;

/// Typed control ids for the analog trackball axes — a separate `InputId`
/// namespace from the digital buttons above.
const CTRL_P1_TRACK_X: InputId = InputId(10);
const CTRL_P1_TRACK_Y: InputId = InputId(11);
const CTRL_P2_TRACK_X: InputId = InputId(12);
const CTRL_P2_TRACK_Y: InputId = InputId(13);

const MARBLE_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_COIN as u16),
        stable_name: "coin",
        label: "Coin",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(INPUT_START1 as u16),
        stable_name: "p1_start",
        label: "P1 Start",
        kind: InputKind::Start,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_START,
    },
    InputControl {
        id: InputId(INPUT_START2 as u16),
        stable_name: "p2_start",
        label: "P2 Start",
        kind: InputKind::Start,
        player: Some(2),
        default_bindings: crate::input_defaults::P2_START,
    },
    InputControl {
        id: InputId(INPUT_SERVICE as u16),
        stable_name: "service",
        label: "Service / Self-Test",
        kind: InputKind::Service,
        player: None,
        default_bindings: crate::input_defaults::SERVICE,
    },
    // P1 trackball roll via the keyboard / D-pad (digital fallback for the
    // analog axes below), bound to the standard P1 direction defaults.
    InputControl {
        id: InputId(INPUT_P1_TRACK_LEFT as u16),
        stable_name: "p1_track_left",
        label: "P1 Roll Left",
        kind: InputKind::DigitalDirection {
            direction: Direction::Left,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(INPUT_P1_TRACK_RIGHT as u16),
        stable_name: "p1_track_right",
        label: "P1 Roll Right",
        kind: InputKind::DigitalDirection {
            direction: Direction::Right,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
    InputControl {
        id: InputId(INPUT_P1_TRACK_UP as u16),
        stable_name: "p1_track_up",
        label: "P1 Roll Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_UP,
    },
    InputControl {
        id: InputId(INPUT_P1_TRACK_DOWN as u16),
        stable_name: "p1_track_down",
        label: "P1 Roll Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_DOWN,
    },
    // Trackballs: P1 drives the mouse; P2 is rebindable (no mouse default).
    InputControl {
        id: CTRL_P1_TRACK_X,
        stable_name: "p1_trackball_x",
        label: "P1 Trackball X",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::X,
        },
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::AxisX)],
    },
    InputControl {
        id: CTRL_P1_TRACK_Y,
        stable_name: "p1_trackball_y",
        label: "P1 Trackball Y",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::Y,
        },
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::AxisY)],
    },
    InputControl {
        id: CTRL_P2_TRACK_X,
        stable_name: "p2_trackball_x",
        label: "P2 Trackball X",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::X,
        },
        player: Some(2),
        default_bindings: &[],
    },
    InputControl {
        id: CTRL_P2_TRACK_Y,
        stable_name: "p2_trackball_y",
        label: "P2 Trackball Y",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::Y,
        },
        player: Some(2),
        default_bindings: &[],
    },
];

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------
// Master clock 14.318181 MHz. Main CPU = pixel clock = master/2 = 7.15909 MHz,
// so CPU cycles map 1:1 to pixel clocks: HTOTAL 456, VTOTAL 262 → ~59.92 Hz.
// Visible area 336×240 (H total 456, V total 262).
const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 7_159_090,
    cycles_per_scanline: 456,
    total_scanlines: 262,
    display_width: 336,
    display_height: 240,
};

/// First scanline of vertical blank (`vbstart`); VBLANK asserts IRQ4 here.
const VBLANK_SCANLINE: u16 = 240;

// ---------------------------------------------------------------------------
// MarbleSystem
// ---------------------------------------------------------------------------

/// Atari Marble Madness (System 1) arcade system.
#[derive(BusDebug)]
pub struct MarbleSystem {
    #[debug_cpu("M68010")]
    cpu: M68000,
    #[debug_map(cpu = 0)]
    map: AddressSpace32,

    /// Slapstic 137412-103 protection PAL gating the 080000-087FFF ROM window.
    slapstic: Slapstic,
    /// The 32 KB (4 × 8 KB bank) slapstic ROM the window selects between.
    slapstic_rom: Vec<u8>,

    /// EEPROM 2804 (512 bytes, low byte at F00000-F003FF), gated by `eeprom_unlocked`.
    eeprom: [u8; 512],

    /// Decoded 8×8 2bpp alpha (text/HUD) font tiles. Not CPU-addressable.
    alpha_cache: GfxCache,
    /// Decoded playfield tile banks + the PROM remap lookup. Not CPU-addressable.
    playfield: PlayfieldGfx,

    // Video control latches (consumed by the Phase 3 video pipeline).
    xscroll: u16,
    yscroll: u16,
    priority_pens: u16,
    /// 0x860001 audio/video control: bit 7 = sound-CPU reset (Phase 4), bits
    /// 5-3 = motion-object bank, bit 2 = playfield tile bank (Phase 3).
    bankselect: u8,
    /// 0x8C0001 EEPROM unlock latch. The 2804 re-locks after each write.
    eeprom_unlocked: bool,
    /// Count of accepted EEPROM byte writes (bring-up diagnostic; not saved).
    eeprom_writes: u64,

    // F60000 switch port low byte (active-low; bits 0/1 = start, bit 6 = service).
    // Bits 4 (VBLANK) and 7 (sound buffer) are computed live in `read_f60000`.
    f60000_buttons: u8,

    /// Trackball counters [p1x, p1y, p2x, p2y] — free-running 8-bit counters,
    /// the trackball motion the game samples at 0xF20000.
    trackball: [u8; 4],
    /// Per-player 45°-rotated counter pair, latched on the even (X) read so the
    /// paired odd (Y) read sees the same snapshot — see [`trackball_read`].
    trackball_cur: [[u8; 2]; 2],
    /// Pending sub-counter motion per axis (mouse deltas + held keys), drained a
    /// capped step at a time each frame so a fast flick can't alias the 8-bit
    /// counter (a >127 jump would read as motion the wrong way). See
    /// [`update_trackball`].
    track_accum: [i32; 4],
    /// P1 trackball direction keys held [left, right, up, down] — the keyboard
    /// way to roll the ball; each contributes a fixed step per frame.
    track_keys: [bool; 4],

    // VBLANK interrupt latch (IRQ4), held until acked via 0x8A0001.
    video_int: bool,

    /// Scanline motion-object interrupt (IRQ3 / "SLIP"). Asserted for the one
    /// scanline a motion-object timer entry targets; also read back at 0x2E0000
    /// bit 7. Recomputed at every scanline boundary from the active sprite bank.
    scanline_int: bool,

    /// One-pole DC-blocker state (prev input, prev output) for the audio mix —
    /// removes the POKEY's unipolar DC so the FM music gets full headroom, the
    /// way the cabinet's AC-coupled amplifier does.
    audio_dc: (f32, f32),

    /// M6502 sound board (POKEY + YM2151 stub + inter-CPU latches).
    #[debug_device("Sound")]
    sound: MarbleSound,
    /// Sound CPU runs at 1/4 the main CPU rate.
    sound_clock: ClockDivider,
    audio_buffer: Vec<i16>,

    clock: u64,
    watchdog_count: u8,
}

impl MarbleSystem {
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

    pub fn new() -> Self {
        let mut cpu = M68000::new();
        cpu.variant = M68kVariant::M68010;
        Self {
            cpu,
            map: Self::build_map(),
            slapstic: Slapstic::new(),
            slapstic_rom: vec![0; 0x8000],
            eeprom: [0xFF; 512], // 2804 reads 0xFF erased; game checksums + reinits
            alpha_cache: GfxCache::new(ALPHA_TILE_COUNT, 8, 8),
            playfield: PlayfieldGfx::empty(),
            xscroll: 0,
            yscroll: 0,
            priority_pens: 0,
            bankselect: 0,
            eeprom_unlocked: false,
            eeprom_writes: 0,
            f60000_buttons: 0xFF,
            video_int: false,
            scanline_int: false,
            audio_dc: (0.0, 0.0),
            trackball: [0; 4],
            trackball_cur: [[0; 2]; 2],
            track_accum: [0; 4],
            track_keys: [false; 4],
            sound: MarbleSound::new(),
            sound_clock: ClockDivider::new(1, 4),
            audio_buffer: Vec::with_capacity(2048),
            clock: 0,
            watchdog_count: 0,
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let image = load_maincpu_image(rom_set)?;
        self.map.load_region(Region::Rom, &image[0x00000..0x80000]);
        // The slapstic ROM is held outside the map: the bus picks a bank per
        // access via the slapstic state machine (see `slapstic_read`).
        self.slapstic_rom.copy_from_slice(&image[0x80000..0x88000]);

        // Alpha (text/HUD) font tiles.
        let alpha = MARBLE_ALPHA_ROM.load(rom_set)?;
        self.alpha_cache = decode_gfx(&alpha, 0, ALPHA_TILE_COUNT, &MARBLE_ALPHA_LAYOUT);

        // Playfield + motion-object tile banks and PROM remap. The tiles region
        // is ROMREGION_INVERT | ROMREGION_ERASEFF: erase to 0xFF, place the
        // chips, then invert the whole buffer — so gaps become 0x00 and chip
        // data is inverted, exactly as MAME's region_post_process does. (Gaps
        // matter: bank 2 has no plane-3 chip, and a 0x00 plane 3 keeps its
        // sprite pens in 0-7 — an 0xFF gap would force pen bit 3 and render the
        // marble black.)
        let mut tiles = MARBLE_TILE_ROM.load_erased(rom_set, 0xFF)?;
        for b in tiles.iter_mut() {
            *b = !*b;
        }
        let prom = MARBLE_PROM.load(rom_set)?;
        self.playfield = build_tile_gfx(&prom, &tiles);

        // M6502 sound program.
        let sound_image = MARBLE_SOUND_ROM.load(rom_set)?;
        self.sound.load_rom(&sound_image);
        Ok(())
    }

    pub fn get_cpu_state(&self) -> M68000State {
        self.cpu.snapshot()
    }

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

    /// 0x860001 audio/video control latch. Bit 7 drives the sound-CPU reset line
    /// (1 = run, 0 = hold); bit 2 selects the playfield tile bank; bits 5-3
    /// select the motion-object bank (Phase 3c).
    fn bankselect_w(&mut self, data: u8) {
        self.bankselect = data;
        self.sound.set_reset(data & 0x80 == 0);
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
    fn read_f60000(&self) -> u16 {
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
    fn int3_state(&self) -> u16 {
        if self.scanline_int { 0x0080 } else { 0x0000 }
    }

    /// Trackball read (0xF20000-0xF20007: four byte ports). Marble's trackballs
    /// are mounted at 45°, so the hardware returns rotated counter pairs: the X
    /// port yields `x + y`, the paired Y port `x - y`. The even (X) read latches
    /// both from the live counters so the odd (Y) read sees the same snapshot.
    fn trackball_read(&mut self, addr: u32) -> u16 {
        let offset = ((addr >> 1) & 3) as usize;
        let player = (offset >> 1) & 1;
        let which = offset & 1;
        if which == 0 {
            let (x, y) = (self.trackball[player * 2], self.trackball[player * 2 + 1]);
            self.trackball_cur[player][0] = x.wrapping_add(y);
            self.trackball_cur[player][1] = x.wrapping_sub(y);
        }
        self.trackball_cur[player][which] as u16
    }

    /// Advance the trackball counters once per frame from pending input: held P1
    /// direction keys add a fixed step, then each axis drains a capped amount of
    /// its accumulator (mouse + keys) into the 8-bit counter. The X axes are
    /// reversed to match the cabinet's PORT_REVERSE wiring.
    fn update_trackball(&mut self) {
        if self.track_keys[1] {
            self.track_accum[0] += TRACK_KEY_STEP;
        }
        if self.track_keys[0] {
            self.track_accum[0] -= TRACK_KEY_STEP;
        }
        if self.track_keys[3] {
            self.track_accum[1] += TRACK_KEY_STEP;
        }
        if self.track_keys[2] {
            self.track_accum[1] -= TRACK_KEY_STEP;
        }

        for axis in 0..4 {
            let step = self.track_accum[axis].clamp(-TRACK_MAX_STEP, TRACK_MAX_STEP);
            self.track_accum[axis] -= step;
            let applied = if axis % 2 == 0 { -step } else { step };
            self.trackball[axis] = (self.trackball[axis] as i32).wrapping_add(applied) as u8;
        }
    }

    /// Whether any motion-object timer entry in the active sprite bank targets
    /// `scanline` — i.e. IRQ3 should be asserted there. Timer entries are flagged
    /// by 0xFFFF in word[1]; word[0] gives the height and Y, and the interrupt
    /// fires at the top of that sprite's band: `256 - (word0>>5) - vsize*8 - 1`.
    fn timer_irq_at_scanline(&self, scanline: u16) -> bool {
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
    /// separately by [`Slapstic::test`] on every *data* access (see the `Bus`
    /// impl); opcode prefetches read through without perturbing it. The window
    /// is mirrored ×4, so the bank offset is just the low 13 bits of the address.
    fn slapstic_read(&self, addr: u32) -> u16 {
        let bank = self.slapstic.current_bank() as usize;
        let base = bank * 0x2000 + (addr as usize & 0x1FFE);
        u16::from_be_bytes([self.slapstic_rom[base], self.slapstic_rom[base + 1]])
    }

    /// Effective autovector interrupt level (the 68000 takes the highest
    /// pending). IRQ6 = sound response, IRQ4 = VBLANK, IRQ3 = motion-object
    /// scanline (SLIP). IRQ2 (ADC/joystick) arrives in a later phase.
    fn interrupt_level(&self) -> u8 {
        if self.sound.response_pending() {
            6
        } else if self.video_int {
            4
        } else if self.scanline_int {
            3
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
    fn render(&self, buffer: &mut [u8]) {
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
    /// playfield palette bank (see [`render`]).
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

                if let Some(bank) = self.playfield.banks.get(bank_id) {
                    let pen = bank
                        .cache
                        .pixel(code % bank.cache.count(), src_x % 8, src_y % 8);
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
        let mob = self.map.region_data(Region::Mob);
        let bank_base = ((self.bankselect >> 3) & 7) as usize * 256; // words
        let word = |wi: usize| u16::from_be_bytes([mob[wi * 2], mob[wi * 2 + 1]]);

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
                self.draw_mo_entry(&mut mo, w, h, w0, w1, w2, PRIORITY_BIT);
            }
            link = (w3 & 0x3F) as usize;
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
    /// pen 0. The palette index is `0x100 + palcolor*16 + pen`, with the priority
    /// flag OR'd in for the merge step.
    #[allow(clippy::too_many_arguments)]
    fn draw_mo_entry(
        &self,
        mo: &mut [u16],
        w: usize,
        h: usize,
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
                if dy < 0 || dy >= h as i32 {
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

    pub fn tick(&mut self) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();

        // At each scanline boundary: VBLANK raises IRQ4 on the first blanked
        // line, and IRQ3 tracks whether a motion-object timer targets this line
        // (a one-scanline pulse, like MAME's int3/int3off timer pair).
        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let scanline = (frame_cycle / TIMING.cycles_per_scanline) as u16;
            if scanline == VBLANK_SCANLINE {
                self.video_int = true;
            }
            self.scanline_int = self.timer_irq_at_scanline(scanline);
        }

        // Latch watchpoint attribution context before CPU execution.
        if self.map.has_any_watchpoints() {
            let pc = self.cpu.at_instruction_boundary().then_some(self.cpu.pc);
            self.map.latch_access_context(self.clock, pc);
        }

        bus_split!(self, bus: u32 word => {
            self.cpu.execute_cycle(bus, BusMaster::Cpu(0));
        });

        // The sound board runs at 1/4 the main CPU rate.
        if self.sound_clock.tick() {
            self.sound.tick();
        }

        self.clock += 1;
    }
}

impl Default for MarbleSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus
// ---------------------------------------------------------------------------

impl Bus for MarbleSystem {
    type Address = u32;
    type Data = u16;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn observe_data_access(&mut self, _master: BusMaster, addr: u32, _is_write: bool) {
        // The slapstic snoops the address bus of every access the CPU drives —
        // data reads/writes *and* instruction prefetches (the protection arms
        // itself by prefetching code at magic addresses) — anywhere in the map,
        // since its `test_any` patterns can land in RAM, and at the exact byte
        // address (so consecutive byte accesses present distinct odd/even
        // addresses, like the real chip's pins). Read/write is irrelevant: the
        // PAL only decodes address lines.
        self.slapstic.test(addr);
    }

    fn read(&mut self, master: BusMaster, addr: u32) -> u16 {
        // The slapstic state machine is driven by `observe_data_access` (called
        // by the CPU for data accesses only); a read here just returns the bank
        // it currently presents.
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
            0xF2_0000..=0xF2_0007 => self.trackball_read(addr),
            0xF4_0000..=0xF4_001F => 0x00FF, // Joystick/ADC (unused by marble)
            0xF6_0000..=0xF6_0003 => self.read_f60000(),
            0xFC_0000..=0xFC_0001 => self.sound.read_response() as u16,
            _ => 0xFFFF,
        };
        self.map.watch_read(0, master, addr, val as u32, 2);
        val
    }

    fn write(&mut self, master: BusMaster, addr: u32, data: u16) {
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
            0xF0_0000..=0xF0_03FF => {
                // 2804 writes are gated by the unlock latch and re-lock after one byte.
                if self.eeprom_unlocked {
                    self.eeprom[((addr >> 1) & 0x1FF) as usize] = byte;
                    self.eeprom_unlocked = false;
                    self.eeprom_writes += 1;
                }
            }
            0xF4_0000..=0xF4_001F => {} // ADC channel select (Phase 5)
            0xF8_0000..=0xF8_0001 => {} // Sound latch (RoadBlasters only)
            0xFE_0000..=0xFE_0001 => self.sound.write_command(byte),
            _ => {}
        }
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            irq_level: self.interrupt_level(),
            // 0xFF ⇒ the 68000 core autovectors (vector 24 + level).
            irq_vector: 0xFF,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Capability traits
// ---------------------------------------------------------------------------

impl Renderable for MarbleSystem {
    fn display_size(&self) -> (u32, u32) {
        TIMING.display_size()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        self.render(buffer);
    }
}

impl AudioSource for MarbleSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        let n = buffer.len().min(self.audio_buffer.len());
        buffer[..n].copy_from_slice(&self.audio_buffer[..n]);
        self.audio_buffer.drain(..n);
        n
    }

    fn audio_sample_rate(&self) -> u32 {
        44100
    }
}

impl InputConfigurable for MarbleSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        MARBLE_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Button { id, pressed } => match id.0 as u8 {
                INPUT_START1 => set_bit_active_low(&mut self.f60000_buttons, 0, pressed),
                INPUT_START2 => set_bit_active_low(&mut self.f60000_buttons, 1, pressed),
                INPUT_SERVICE => set_bit_active_low(&mut self.f60000_buttons, 6, pressed),
                // Coins are read on the sound board's 0x1820 port.
                INPUT_COIN => self.sound.set_coin(0, pressed),
                // P1 keyboard trackball roll — held direction keys.
                INPUT_P1_TRACK_LEFT => self.track_keys[0] = pressed,
                INPUT_P1_TRACK_RIGHT => self.track_keys[1] = pressed,
                INPUT_P1_TRACK_UP => self.track_keys[2] = pressed,
                INPUT_P1_TRACK_DOWN => self.track_keys[3] = pressed,
                _ => {}
            },
            InputEvent::Relative { id, delta } => {
                // Mouse motion → pending sub-counter movement, drained per frame.
                let d = delta as i32;
                if id == CTRL_P1_TRACK_X {
                    self.track_accum[0] += d;
                } else if id == CTRL_P1_TRACK_Y {
                    self.track_accum[1] += d;
                } else if id == CTRL_P2_TRACK_X {
                    self.track_accum[2] += d;
                } else if id == CTRL_P2_TRACK_Y {
                    self.track_accum[3] += d;
                }
            }
            InputEvent::Absolute { .. } => {}
        }
    }
}

impl MachineCore for MarbleSystem {
    crate::machine_core_metadata!("marble", TIMING);

    fn run_frame(&mut self) {
        // Fold this frame's trackball input into the counters the game samples.
        self.update_trackball();

        for _ in 0..TIMING.cycles_per_frame() {
            self.tick();
        }

        // Watchdog: System 1 reboots after 8 VBLANKs without a strobe to
        // 0x880001. The game kicks it every frame; if it stops, reset.
        self.watchdog_count = self.watchdog_count.saturating_add(1);
        if self.watchdog_count >= 8 {
            self.reset();
        }

        // Drain the sound board's mixed audio (POKEY + YM2151 FM, mono), strip
        // the DC with a one-pole high-pass (cutoff ≈ 35 Hz), then scale and clamp
        // to signed 16-bit.
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

    fn reset(&mut self) {
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
        self.audio_dc = (0.0, 0.0);
        self.trackball = [0; 4];
        self.trackball_cur = [[0; 2]; 2];
        self.track_accum = [0; 4];
        self.track_keys = [false; 4];
        self.watchdog_count = 0;
        // EEPROM contents are non-volatile and survive reset.

        bus_split!(self, bus: u32 word => {
            self.cpu.reset(bus, BusMaster::Cpu(0));
        });
    }
}

// `MachineDebug` (debug_bus + cycle stepping) via the standalone-debug macro;
// `BusDebug` is `#[derive]`d on the struct above (24-bit `AddressSpace32` bus).
crate::impl_standalone_debug!(MarbleSystem);

impl Saveable for MarbleSystem {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
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
        w.write_bytes(&self.trackball);
        w.write_bytes(&self.trackball_cur[0]);
        w.write_bytes(&self.trackball_cur[1]);
        w.write_u64_le(self.clock);
        w.write_u8(self.watchdog_count);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
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
        r.read_bytes_into(&mut self.trackball)?;
        r.read_bytes_into(&mut self.trackball_cur[0])?;
        r.read_bytes_into(&mut self.trackball_cur[1])?;
        self.clock = r.read_u64_le()?;
        self.watchdog_count = r.read_u8()?;
        Ok(())
    }
}

impl SaveState for MarbleSystem {
    crate::machine_save_state!();
}

// The 2804 EEPROM is the machine's battery-backed store; the frontend persists
// it through the Nvram trait (high scores, config, the boot game-id byte).
impl Nvram for MarbleSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(&self.eeprom)
    }

    fn load_nvram(&mut self, data: &[u8]) {
        let len = data.len().min(self.eeprom.len());
        self.eeprom[..len].copy_from_slice(&data[..len]);
    }
}

// No sub-span profiling, no event tracing.
impl Profilable for MarbleSystem {}
impl phosphor_core::core::debug_trace::DebugTrace for MarbleSystem {}

// Marble Madness has no operator DIP switches — coinage and game options live
// in the EEPROM and the sound-board config. The all-default trait exposes no banks.
impl phosphor_core::core::machine::DipSwitches for MarbleSystem {}

// ---------------------------------------------------------------------------
// Registry + disassembly
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = MarbleSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("marble", &["marble"], create_machine)
}

// Disassemblable code regions for the standalone `disasm` tool.
// `main`  — the MC68010 program image (000000-07FFFF, de-interleaved).
// `sound` — the M6502 sound program (8000-FFFF).
inventory::submit! {
    DisasmRegion {
        machine: "marble",
        region: "main",
        cpu: DisasmCpu::M68000,
        org: 0,
        size: 0x80000,
        load: |rs| load_maincpu_image(rs).map(|mut v| { v.truncate(0x80000); v }),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "marble",
        region: "sound",
        cpu: DisasmCpu::M6502,
        org: 0x8000,
        size: 0x8000,
        load: |rs| MARBLE_SOUND_ROM.load(rs).map(|v| v[0x8000..0x10000].to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_decodes_documented_windows() {
        let sys = MarbleSystem::new();
        assert_eq!(sys.map.region_at(0x00_0000).unwrap().id, Region::Rom.into());
        // The slapstic window (080000-087FFF) is not a map region — it is decoded
        // in the bus and banked by the slapstic.
        assert!(sys.map.region_at(0x08_0000).is_none());
        assert_eq!(sys.map.region_at(0x40_0000).unwrap().id, Region::Ram.into());
        assert_eq!(
            sys.map.region_at(0x90_0000).unwrap().id,
            Region::CartRam.into()
        );
        assert_eq!(
            sys.map.region_at(0xA0_0000).unwrap().id,
            Region::Playfield.into()
        );
        assert_eq!(sys.map.region_at(0xA0_2000).unwrap().id, Region::Mob.into());
        assert_eq!(
            sys.map.region_at(0xA0_3000).unwrap().id,
            Region::Alpha.into()
        );
        assert_eq!(
            sys.map.region_at(0xB0_0000).unwrap().id,
            Region::Palette.into()
        );
    }

    #[test]
    fn cpu_is_a_68010() {
        let sys = MarbleSystem::new();
        assert_eq!(sys.cpu.variant, M68kVariant::M68010);
    }

    #[test]
    fn reset_loads_ssp_and_pc_from_bios_vectors() {
        let mut sys = MarbleSystem::new();
        // Reset vectors live in the BIOS at 0x000000: SSP = 0x00401000,
        // PC = 0x00000400.
        let rom = sys.map.region_data_mut(Region::Rom);
        rom[0..8].copy_from_slice(&[0x00, 0x40, 0x10, 0x00, 0x00, 0x00, 0x04, 0x00]);
        sys.reset();
        let st = sys.get_cpu_state();
        assert_eq!(st.a[7], 0x0040_1000);
        assert_eq!(st.pc, 0x0000_0400);
    }

    #[test]
    fn ram_word_access_round_trips() {
        let mut sys = MarbleSystem::new();
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x40_0000, 0xBEEF);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x40_0000), 0xBEEF);
    }

    #[test]
    fn palette_and_video_ram_round_trip() {
        let mut sys = MarbleSystem::new();
        Bus::write(&mut sys, BusMaster::Cpu(0), 0xB0_0000, 0x0ABC);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0xB0_0000), 0x0ABC);
        Bus::write(&mut sys, BusMaster::Cpu(0), 0xA0_3000, 0x1234);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0xA0_3000), 0x1234);
    }

    #[test]
    fn control_latches_and_acks() {
        let mut sys = MarbleSystem::new();
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x80_0000, 0x0040);
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x82_0000, 0x0020);
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x86_0000, 0x00AC);
        assert_eq!(sys.xscroll, 0x0040);
        assert_eq!(sys.yscroll, 0x0020);
        assert_eq!(sys.bankselect, 0xAC);

        // VBLANK IRQ4 asserts, then 0x8A0001 acks it.
        sys.video_int = true;
        assert_eq!(sys.interrupt_level(), 4);
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x8A_0000, 0x0000);
        assert!(!sys.video_int);
        let st = sys.check_interrupts(BusMaster::Cpu(0));
        assert_eq!(st.irq_level, 0);
        assert_eq!(st.irq_vector, 0xFF);
    }

    #[test]
    fn watchdog_strobe_resets_count() {
        let mut sys = MarbleSystem::new();
        sys.watchdog_count = 5;
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x88_0000, 0x0000);
        assert_eq!(sys.watchdog_count, 0);
    }

    #[test]
    fn f60000_reports_vblank_and_start_buttons() {
        let mut sys = MarbleSystem::new();
        // Outside VBLANK (clock at 0 → scanline 0): bit 4 set, bit 7 clear.
        assert_eq!(sys.read_f60000() & 0x0090, 0x0010);
        // Inside VBLANK: bit 4 clears.
        sys.clock = (VBLANK_SCANLINE as u64) * TIMING.cycles_per_scanline;
        assert_eq!(sys.read_f60000() & 0x0010, 0x0000);

        // Start1 is active-low bit 0.
        sys.clock = 0;
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_START1 as u16),
            pressed: true,
        });
        assert_eq!(sys.read_f60000() & 0x0001, 0x0000);
    }

    #[test]
    fn trackball_reads_rotated_counters() {
        let mut sys = MarbleSystem::new();
        // Move P1's trackball: X +10 (reversed → counter −10 = 246), Y +3. The
        // pending motion is drained into the counters once per frame.
        sys.handle_input(InputEvent::Relative {
            id: CTRL_P1_TRACK_X,
            delta: 10.0,
        });
        sys.handle_input(InputEvent::Relative {
            id: CTRL_P1_TRACK_Y,
            delta: 3.0,
        });
        sys.update_trackball();
        assert_eq!(sys.trackball, [246, 3, 0, 0]);

        // The 45° rotation: X port = x+y, Y port = x-y. The even read latches
        // both, so the odd read sees the same snapshot.
        let xport = sys.trackball_read(0xF2_0000); // P1 X
        let yport = sys.trackball_read(0xF2_0002); // P1 Y
        assert_eq!(xport, 246u16.wrapping_add(3) & 0xFF);
        assert_eq!(yport, 246u16.wrapping_sub(3) & 0xFF);

        // P2 ports are independent and idle at zero.
        assert_eq!(sys.trackball_read(0xF2_0004), 0);
        assert_eq!(sys.trackball_read(0xF2_0006), 0);
    }

    #[test]
    fn keyboard_rolls_the_trackball() {
        let mut sys = MarbleSystem::new();
        // Holding "roll right" steps the (reversed) X counter each frame.
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_P1_TRACK_RIGHT as u16),
            pressed: true,
        });
        sys.update_trackball();
        assert_eq!(sys.trackball[0], (256 - TRACK_KEY_STEP) as u8);
        sys.update_trackball();
        assert_eq!(sys.trackball[0], (256 - 2 * TRACK_KEY_STEP) as u8);
        // Releasing stops the roll.
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_P1_TRACK_RIGHT as u16),
            pressed: false,
        });
        let held = sys.trackball[0];
        sys.update_trackball();
        assert_eq!(sys.trackball[0], held, "no motion once released");
    }

    #[test]
    fn fast_trackball_flick_is_capped_to_avoid_aliasing() {
        let mut sys = MarbleSystem::new();
        // A huge one-frame delta is applied a capped step at a time so the 8-bit
        // counter never jumps more than ±127 (which the game would misread).
        sys.handle_input(InputEvent::Relative {
            id: CTRL_P1_TRACK_Y,
            delta: 500.0,
        });
        sys.update_trackball();
        assert_eq!(sys.trackball[1], TRACK_MAX_STEP as u8);
        sys.update_trackball();
        assert_eq!(sys.trackball[1], (2 * TRACK_MAX_STEP) as u8);
    }

    /// Boot a hand-assembled 68010 program on the full board and prove the core
    /// runs it, services the autovectored VBLANK IRQ, and stores into RAM —
    /// exercising the 68010 exception frame end-to-end inside the machine.
    #[test]
    fn synthetic_program_boots_and_takes_vblank_irq() {
        let mut sys = MarbleSystem::new();
        {
            let rom = sys.map.region_data_mut(Region::Rom);
            // Reset vectors: SSP = 0x00401000, PC = 0x00000400.
            rom[0x00..0x08].copy_from_slice(&[0x00, 0x40, 0x10, 0x00, 0x00, 0x00, 0x04, 0x00]);
            // VBLANK is IRQ4 → autovector 28 (0x70) → handler at 0x500.
            rom[28 * 4..28 * 4 + 4].copy_from_slice(&[0x00, 0x00, 0x05, 0x00]);

            // Main @ 0x400:
            //   move #$2000, sr        ; supervisor, interrupt mask = 0
            //   loop: addq.l #1, $400000
            //         bra.s loop
            let main: &[u8] = &[
                0x46, 0xFC, 0x20, 0x00, // move #$2000, sr
                0x52, 0xB9, 0x00, 0x40, 0x00, 0x00, // addq.l #1, $00400000
                0x60, 0xF8, // bra.s loop
            ];
            rom[0x400..0x400 + main.len()].copy_from_slice(main);

            // IRQ4 handler @ 0x500:
            //   addq.l #1, $400010     ; bump the interrupt counter
            //   move.b #0, $8A0001     ; ack VBLANK IRQ4
            //   rte
            let handler: &[u8] = &[
                0x52, 0xB9, 0x00, 0x40, 0x00, 0x10, // addq.l #1, $00400010
                0x13, 0xFC, 0x00, 0x00, 0x00, 0x8A, 0x00, 0x01, // move.b #0, $008A0001
                0x4E, 0x73, // rte
            ];
            rom[0x500..0x500 + handler.len()].copy_from_slice(handler);
        }

        sys.reset();
        assert_eq!(sys.get_cpu_state().pc, 0x0000_0400);

        // Three frames stays under the 8-frame watchdog timeout.
        for _ in 0..3 {
            sys.run_frame();
        }

        // CPU is still executing inside the ROM main loop.
        let pc = sys.get_cpu_state().pc;
        assert!(pc < 0x8_0000, "PC {pc:#08X} escaped ROM");

        // The "alive" counter advanced → the core actually ran code.
        let ram = sys.map.region_data(Region::Ram);
        let alive = u32::from_be_bytes([ram[0], ram[1], ram[2], ram[3]]);
        assert!(alive > 0, "main loop never ran");

        // The interrupt counter advanced → the autovectored VBLANK IRQ was
        // taken and RTE returned cleanly through the 68010 frame.
        let taken = u32::from_be_bytes([ram[0x10], ram[0x11], ram[0x12], ram[0x13]]);
        assert!(taken > 0, "VBLANK IRQ was never serviced");
    }

    #[test]
    fn disasm_regions_registered() {
        use crate::disasm_registry::{find, regions_for};
        assert_eq!(
            regions_for("marble")
                .iter()
                .map(|r| r.region)
                .collect::<Vec<_>>(),
            vec!["main", "sound"],
        );
        let main = find("marble", "main").expect("main region");
        assert_eq!(main.cpu, DisasmCpu::M68000);
        assert_eq!((main.org, main.size), (0, 0x80000));
        let sound = find("marble", "sound").expect("sound region");
        assert_eq!(sound.cpu, DisasmCpu::M6502);
        assert_eq!((sound.org, sound.size), (0x8000, 0x8000));
    }

    #[test]
    fn irgb4444_decodes_like_mame() {
        // Black: all nibbles 0.
        assert_eq!(irgb4444_to_rgb(0x0000), (0, 0, 0));
        // Zero intensity forces black regardless of RGB.
        assert_eq!(irgb4444_to_rgb(0x0FFF), (0, 0, 0));
        // Full intensity + full white: (0xFF * 0xFF) >> 8 = 254.
        assert_eq!(irgb4444_to_rgb(0xFFFF), (254, 254, 254));
        // Full intensity, pure red.
        assert_eq!(irgb4444_to_rgb(0xFF00), (254, 0, 0));
        // Half intensity (0x8→0x88), full green: (0x88 * 0xFF) >> 8 = 135.
        assert_eq!(irgb4444_to_rgb(0x80F0), (0, 135, 0));
    }

    #[test]
    fn alpha_layer_composites_palette_through_the_cell() {
        let mut sys = MarbleSystem::new();
        // Palette entry 0 (color 0, pen 0) = IRGB full-intensity red.
        let palette = sys.map.region_data_mut(Region::Palette);
        palette[0] = 0xFF;
        palette[1] = 0x00;
        // Top-left alpha cell: code 0, color 0, opaque (bit 13) so pen 0 draws.
        // (The default font cache is all pen 0, so the opaque flag is what makes
        // the cell visible — this exercises the cell decode + palette path.)
        let alpha = sys.map.region_data_mut(Region::Alpha);
        alpha[0] = 0x20;
        alpha[1] = 0x00;

        let (w, h) = sys.display_size();
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.render_frame(&mut buf);
        assert_eq!(&buf[0..3], &[254, 0, 0], "opaque pen 0 → palette entry 0");
    }

    #[test]
    fn alpha_transparent_pen0_stays_black() {
        let mut sys = MarbleSystem::new();
        // Paint palette entry 0 red, but leave the alpha cell transparent
        // (no opaque bit): pen 0 must NOT draw — background stays black.
        let palette = sys.map.region_data_mut(Region::Palette);
        palette[0] = 0xFF;
        palette[1] = 0x00;
        // Alpha cell defaults to 0 (code 0, color 0, not opaque).

        let (w, h) = sys.display_size();
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.render_frame(&mut buf);
        assert_eq!(&buf[0..3], &[0, 0, 0], "transparent pen 0 shows background");
    }

    #[test]
    fn playfield_prom_lookup_decodes_bank_offset_color() {
        let mut prom = vec![0u8; 0x400];
        // Entry 0 → bank 1 (PROM1 bit4 clear), offset 5; 4bpp (PROM2 bit4 clear),
        // colour 0xA (negative-logic low nibble: ~0xC5 & 0xF = 0xA).
        prom[0x000] = 0xE5;
        prom[0x200] = 0xC5;
        let tiles = vec![0u8; 0x100000];
        let gfx = build_tile_gfx(&prom, &tiles);

        // offset 5 | bank-id 1 (first decoded) | colour 0xA.
        assert_eq!(gfx.lookup[0], 0x0005 | (1 << 8) | (0xA << 12));
        assert_eq!(gfx.banks.len(), 2, "blank placeholder + one decoded bank");
        assert_eq!(gfx.banks[1].bpp, 4);
    }

    #[test]
    fn playfield_prom_selects_bpp_and_remaps_unmapped() {
        let mut prom = vec![0u8; 0x400];
        // Entry 0: 6bpp (PROM2 planes 4+5 enabled), bank 1.
        prom[0x000] = 0xE0; // bank 1
        prom[0x200] = 0xF0; // bit4|bit5 set → 6bpp; low nibble 0 → colour 0
        // Entry 1: no bank bits clear anywhere → unmapped → remapped to bank 1.
        prom[0x001] = 0xF0; // all PROM1 bank bits set
        prom[0x201] = 0xC0; // PROM2 bank bits set, planes off (4bpp)
        let tiles = vec![0u8; 0x100000];
        let gfx = build_tile_gfx(&prom, &tiles);

        assert_eq!(gfx.banks[1].bpp, 6, "plane 4+5 enable → 6bpp");
        // Unmapped entry falls back to bank 1 with offset/colour zeroed.
        assert_eq!(gfx.lookup[1], 1 << 8);
    }

    #[test]
    fn playfield_pixel_composites_below_alpha() {
        let mut sys = MarbleSystem::new();
        // One synthetic 4bpp bank: tile 0, pixel (0,0) = pen 3.
        let mut cache = GfxCache::new(1, 8, 8);
        cache.set_pixel(0, 0, 0, 3);
        sys.playfield.banks.push(GfxBank { cache, bpp: 4 });
        // Playfield cell (0,0) → lookup[0]: bank id 1, offset 0, colour 0.
        sys.playfield.lookup[0] = 1 << 8;
        // Palette entry 0x203 (= 0x100 gfx base + (0x20<<3) + pen 3, landing in
        // the playfield bank at 0x200) = IRGB pure green.
        let palette = sys.map.region_data_mut(Region::Palette);
        palette[0x203 * 2] = 0xF0;
        palette[0x203 * 2 + 1] = 0xF0;

        let (w, h) = sys.display_size();
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.render_frame(&mut buf);
        // Alpha cell 0 is transparent, so the playfield pixel shows through.
        assert_eq!(&buf[0..3], &[0, 254, 0]);
    }

    /// Place one synthetic sprite at screen (0,0) and check it composites over
    /// the playfield in the motion-object palette bank.
    fn sprite_test_system(pen: u8) -> MarbleSystem {
        let mut sys = MarbleSystem::new();
        // Sprite gfx bank 1: tile 0, pixel (0,0) = `pen`.
        let mut cache = GfxCache::new(1, 8, 8);
        cache.set_pixel(0, 0, 0, pen);
        sys.playfield.banks.push(GfxBank { cache, bpp: 4 });
        // Colour byte 0 → bank 1, offset 0, palcolor 0.
        sys.playfield.mo_lookup[0] = 1 << 8;
        // Active bank 0, entry 0 (split layout): word[0] positions the sprite at
        // screen (0,0) — xpos 0, and ypos_raw 248 cancels the −256 yscroll and
        // the −8 height to land at y=0. words 1-3 stay 0 (colour:code 0, no
        // priority, link 0 → list ends after this entry).
        let mob = sys.map.region_data_mut(Region::Mob);
        mob[0] = 0x1F; // word[0] = 0x1F00
        mob[1] = 0x00;
        sys
    }

    #[test]
    fn motion_object_composites_over_playfield() {
        let mut sys = sprite_test_system(5);
        // Sprite pen 5, palcolor 0 → motion bank index 0x105 = pure green.
        let palette = sys.map.region_data_mut(Region::Palette);
        palette[0x105 * 2] = 0xF0;
        palette[0x105 * 2 + 1] = 0xF0;

        let (w, h) = sys.display_size();
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.render_frame(&mut buf);
        assert_eq!(
            &buf[0..3],
            &[0, 254, 0],
            "low-priority sprite drawn opaquely"
        );
    }

    #[test]
    fn high_priority_sprite_blends_through_translucent_bank() {
        let mut sys = sprite_test_system(5);
        // Mark the sprite high priority (word[2] bit 15).
        let mob = sys.map.region_data_mut(Region::Mob);
        mob[0x100] = 0x80; // word[2] = 0x8000
        // The playfield pen under it is 0 (blank bank), so the blend index is
        // 0x300 + (0<<4) + 5 = 0x305. Colour it pure green.
        let palette = sys.map.region_data_mut(Region::Palette);
        palette[0x305 * 2] = 0xF0;
        palette[0x305 * 2 + 1] = 0xF0;

        let (w, h) = sys.display_size();
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.render_frame(&mut buf);
        assert_eq!(
            &buf[0..3],
            &[0, 254, 0],
            "high-priority sprite uses the translucent bank"
        );
    }

    #[test]
    fn slip_interrupt_fires_at_timer_scanline() {
        let mut sys = MarbleSystem::new();
        // Active bank 0, entry 0: a timer entry (word[1] = 0xFFFF), word[0]
        // chosen so the interrupt lands on scanline 100:
        // ypos = 256 - (0x1260>>5) - 1*8 - 1 = 256 - 147 - 8 - 1 = 100.
        let mob = sys.map.region_data_mut(Region::Mob);
        mob[0] = 0x12; // word[0] = 0x1260 (height 1, Y field 147)
        mob[1] = 0x60;
        mob[0x80] = 0xFF; // word[1] = 0xFFFF marks a timer
        mob[0x81] = 0xFF;

        assert!(
            sys.timer_irq_at_scanline(100),
            "fires on the target scanline"
        );
        assert!(!sys.timer_irq_at_scanline(99), "not on adjacent scanlines");
        assert!(!sys.timer_irq_at_scanline(101));

        // A non-timer sprite at the same entry must not arm the interrupt.
        let mob = sys.map.region_data_mut(Region::Mob);
        mob[0x80] = 0x01; // word[1] no longer 0xFFFF
        mob[0x81] = 0x23;
        assert!(
            !sys.timer_irq_at_scanline(100),
            "ordinary sprites don't fire IRQ3"
        );

        // The state bit drives IRQ3 and the 0x2E0000 status read together.
        sys.scanline_int = true;
        assert_eq!(sys.interrupt_level(), 3);
        assert_eq!(sys.int3_state(), 0x0080);
        sys.scanline_int = false;
        assert_eq!(sys.int3_state(), 0x0000);
    }

    #[test]
    fn slapstic_banks_the_window_through_the_bus() {
        let mut sys = MarbleSystem::new();
        // Distinct marker word at offset 0 of each 8 KB bank.
        for b in 0..4u8 {
            sys.slapstic_rom[b as usize * 0x2000] = 0x10 + b;
        }
        // The CPU snoops each data access onto the slapstic via
        // `observe_data_access`, then performs the read; reproduce that pairing.
        let read = |sys: &mut MarbleSystem, a| {
            Bus::observe_data_access(sys, BusMaster::Cpu(0), a, false);
            Bus::read(sys, BusMaster::Cpu(0), a)
        };

        // Power-on bank is 3; the arming read (offset 0) returns its marker.
        assert_eq!(read(&mut sys, 0x08_0000), 0x1300);
        // Direct-select bank 0 (offset 0x40 → byte 0x80), then read its marker.
        read(&mut sys, 0x08_0080);
        assert_eq!(read(&mut sys, 0x08_0000), 0x1000);
        assert_eq!(sys.slapstic.current_bank(), 0);
    }

    #[test]
    fn eeprom_writes_gated_by_unlock_and_relock() {
        let mut sys = MarbleSystem::new();
        let w = |sys: &mut MarbleSystem, a, d| Bus::write(sys, BusMaster::Cpu(0), a, d);
        let r = |sys: &mut MarbleSystem, a| Bus::read(sys, BusMaster::Cpu(0), a);

        // Locked: the write is dropped (still reads the erased 0xFF).
        w(&mut sys, 0xF0_0000, 0x0042);
        assert_eq!(r(&mut sys, 0xF0_0000), 0x00FF);

        // Unlock (8C0001), then one write sticks...
        w(&mut sys, 0x8C_0001, 0x0001);
        w(&mut sys, 0xF0_0000, 0x0042);
        assert_eq!(r(&mut sys, 0xF0_0000), 0x0042);

        // ...and the 2804 re-locked, so the next write without an unlock is dropped.
        w(&mut sys, 0xF0_0002, 0x0099);
        assert_eq!(r(&mut sys, 0xF0_0002), 0x00FF);
        assert!(!sys.eeprom_unlocked);
    }

    #[test]
    fn nvram_exposes_the_eeprom() {
        let mut sys = MarbleSystem::new();
        sys.eeprom[0x6E] = 0x42; // the boot game-id byte lives around here
        assert_eq!(Nvram::save_nvram(&sys).unwrap()[0x6E], 0x42);

        let mut sys2 = MarbleSystem::new();
        let snapshot = Nvram::save_nvram(&sys).unwrap().to_vec();
        Nvram::load_nvram(&mut sys2, &snapshot);
        assert_eq!(sys2.eeprom[0x6E], 0x42);
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = MarbleSystem::new();
        sys.map.region_data_mut(Region::Ram)[0x100] = 0xAB;
        sys.map.region_data_mut(Region::Playfield)[0x10] = 0xCD;
        sys.map.region_data_mut(Region::Alpha)[0x20] = 0xEF;
        sys.eeprom[0x30] = 0x99;
        // Drive the slapstic to a non-default bank so its state is exercised.
        // Bank 1's select offset is 0x50 (word) → byte address 0x0800A0. The
        // CPU snoops accesses onto the chip via `observe_data_access`.
        Bus::observe_data_access(&mut sys, BusMaster::Cpu(0), 0x08_0000, false); // arm
        Bus::observe_data_access(&mut sys, BusMaster::Cpu(0), 0x08_00A0, false); // select bank 1
        assert_eq!(sys.slapstic.current_bank(), 1);
        sys.xscroll = 0x1234;
        sys.bankselect = 0x5A;
        sys.video_int = true;
        sys.clock = 99_999;
        sys.watchdog_count = 4;

        let data = SaveState::save_state(&sys).expect("save");
        let cpu_snap = sys.get_cpu_state();

        let mut sys2 = MarbleSystem::new();
        SaveState::load_state(&mut sys2, &data).unwrap();

        assert_eq!(sys2.get_cpu_state(), cpu_snap);
        assert_eq!(sys2.map.region_data(Region::Ram)[0x100], 0xAB);
        assert_eq!(sys2.map.region_data(Region::Playfield)[0x10], 0xCD);
        assert_eq!(sys2.map.region_data(Region::Alpha)[0x20], 0xEF);
        assert_eq!(sys2.eeprom[0x30], 0x99);
        assert_eq!(sys2.slapstic.current_bank(), 1);
        assert_eq!(sys2.xscroll, 0x1234);
        assert_eq!(sys2.bankselect, 0x5A);
        assert!(sys2.video_int);
        assert_eq!(sys2.clock, 99_999);
        assert_eq!(sys2.watchdog_count, 4);
    }
}
