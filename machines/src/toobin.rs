//! Atari Toobin' (1988).
//!
//! Toobin' rides its own board rather than System 1: a 68010 at 8 MHz off a
//! 32 MHz crystal, with no slapstic in front of the program ROM. The video
//! section is three layers merged by a PAL:
//!
//! - a 128×64 playfield of 8×8 4bpp tiles (two words per cell, so 1024×512
//!   pixels of scrolling map behind a 512×384 window),
//! - motion objects: 256 linked entries of four words each, 16×16 4bpp tiles
//!   stacked up to 8×8 tiles per object, and
//! - a 64×48 alpha overlay of 8×8 2bpp tiles, exactly covering the visible
//!   raster, drawn last with pen 0 transparent.
//!
//! The 1024-entry palette is RGB-555 in the low bits with bit 15 marking a pen
//! that ignores the board's global intensity control. That control is a single
//! 5-bit register that dims every other pen together, which is how the game
//! fades the world down while leaving the score display at full brightness.
//!
//! The cabinet monitor is mounted rotated, so the native 512×384 framebuffer
//! this module fills is presented through [`Orientation::ROT270`].
//!
//! The sound board (a 6502 with an FM chip and a stereo pair of POKEYs) is not
//! modeled yet: this board is silent, and the command latch reports itself
//! drained so the main program's handshake never stalls. See the module's
//! `sound_w`/`sound_r` for exactly what is stubbed.

use phosphor_core::audio::{DcBlocker, SampleRing};
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    ActionRole, DefaultBinding, InputConfigurable, InputControl, InputEvent, InputId, InputKind,
    MachineCore, Nvram, Orientation, Profilable, SaveState,
};
use phosphor_core::core::{AccessKind, AddressSpace32};
use phosphor_core::core::{
    Bus, Bus16, BusMaster, BusSignals, ClockDomainName as Clk, ClockTree, DomainId, TimingConfig,
    select_byte,
};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::m68000::{M68kVariant, M68000};
use phosphor_core::gfx::decode::{GfxCache, GfxLayout, decode_gfx};
use phosphor_macros::{BusDebug, MemoryRegion, Saveable};

use crate::atari_jsa::{AtariJsa1, JsaPokey};
use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};

// ---------------------------------------------------------------------------
// ROM manifest (the "toobin" parent set, revision 3)
// ---------------------------------------------------------------------------

/// The eight 68010 program chips, concatenated in load order. The J chips carry
/// the even (high) byte of each word and the F chips the odd (low) byte;
/// [`load_maincpu_image`] interleaves them into the big-endian program image.
pub static TOOBIN_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x80000,
    entries: &[
        RomEntry {
            name: "3133-1j.061",
            size: 0x10000,
            offset: 0x00000,
            crc32: &[0x79a92d02],
        },
        RomEntry {
            name: "3137-1f.061",
            size: 0x10000,
            offset: 0x10000,
            crc32: &[0xe389ef60],
        },
        RomEntry {
            name: "3134-2j.061",
            size: 0x10000,
            offset: 0x20000,
            crc32: &[0x3dbe9a48],
        },
        RomEntry {
            name: "3138-2f.061",
            size: 0x10000,
            offset: 0x30000,
            crc32: &[0xa17fb16c],
        },
        RomEntry {
            name: "3135-4j.061",
            size: 0x10000,
            offset: 0x40000,
            crc32: &[0xdc90b45c],
        },
        RomEntry {
            name: "3139-4f.061",
            size: 0x10000,
            offset: 0x50000,
            crc32: &[0x6f8a719a],
        },
        RomEntry {
            name: "1136-5j.061",
            size: 0x10000,
            offset: 0x60000,
            crc32: &[0x5ae3eeac],
        },
        RomEntry {
            name: "1140-5f.061",
            size: 0x10000,
            offset: 0x70000,
            crc32: &[0xdacbbd94],
        },
    ],
};

/// Playfield tile ROM: 0x80000 bytes in two 0x40000 halves, each half holding
/// two of the four bitplanes.
pub static TOOBIN_PLAYFIELD_ROM: RomRegion = RomRegion {
    size: 0x80000,
    entries: &[
        RomEntry {
            name: "1101-1a.061",
            size: 0x10000,
            offset: 0x00000,
            crc32: &[0x02696f15],
        },
        RomEntry {
            name: "1102-2a.061",
            size: 0x10000,
            offset: 0x10000,
            crc32: &[0x4bed4262],
        },
        RomEntry {
            name: "1103-4a.061",
            size: 0x10000,
            offset: 0x20000,
            crc32: &[0xe62b037f],
        },
        RomEntry {
            name: "1104-5a.061",
            size: 0x10000,
            offset: 0x30000,
            crc32: &[0xfa05aee6],
        },
        RomEntry {
            name: "1105-1b.061",
            size: 0x10000,
            offset: 0x40000,
            crc32: &[0xab1c5578],
        },
        RomEntry {
            name: "1106-2b.061",
            size: 0x10000,
            offset: 0x50000,
            crc32: &[0x4020468e],
        },
        RomEntry {
            name: "1107-4b.061",
            size: 0x10000,
            offset: 0x60000,
            crc32: &[0xfe6f6aed],
        },
        RomEntry {
            name: "1108-5b.061",
            size: 0x10000,
            offset: 0x70000,
            crc32: &[0x26fe71e1],
        },
    ],
};

/// Motion-object tile ROM: 0x200000 bytes in two 0x100000 halves, two bitplanes
/// per half.
///
/// The four 64 KB chips in each half are populated twice, at +0x80000 and
/// +0xC0000 within that half, because the board wires only enough address lines
/// to the small sockets to address half their window. Listing each chip twice
/// is what fills the second copy: without it the upper quarter of each half
/// decodes to blank tiles.
pub static TOOBIN_MO_ROM: RomRegion = RomRegion {
    size: 0x200000,
    entries: &[
        RomEntry {
            name: "1143-10a.061",
            size: 0x20000,
            offset: 0x000000,
            crc32: &[0x211c1049],
        },
        RomEntry {
            name: "1144-13a.061",
            size: 0x20000,
            offset: 0x020000,
            crc32: &[0xef62ed2c],
        },
        RomEntry {
            name: "1145-16a.061",
            size: 0x20000,
            offset: 0x040000,
            crc32: &[0x067ecb8a],
        },
        RomEntry {
            name: "1146-18a.061",
            size: 0x20000,
            offset: 0x060000,
            crc32: &[0xfea6bc92],
        },
        RomEntry {
            name: "1125-21a.061",
            size: 0x10000,
            offset: 0x080000,
            crc32: &[0xc37f24ac],
        },
        RomEntry {
            name: "1126-23a.061",
            size: 0x10000,
            offset: 0x090000,
            crc32: &[0x015257f0],
        },
        RomEntry {
            name: "1127-24a.061",
            size: 0x10000,
            offset: 0x0A0000,
            crc32: &[0xd05417cb],
        },
        RomEntry {
            name: "1128-25a.061",
            size: 0x10000,
            offset: 0x0B0000,
            crc32: &[0xfba3e203],
        },
        // Second copy of the four small chips in the low half.
        RomEntry {
            name: "1125-21a.061",
            size: 0x10000,
            offset: 0x0C0000,
            crc32: &[0xc37f24ac],
        },
        RomEntry {
            name: "1126-23a.061",
            size: 0x10000,
            offset: 0x0D0000,
            crc32: &[0x015257f0],
        },
        RomEntry {
            name: "1127-24a.061",
            size: 0x10000,
            offset: 0x0E0000,
            crc32: &[0xd05417cb],
        },
        RomEntry {
            name: "1128-25a.061",
            size: 0x10000,
            offset: 0x0F0000,
            crc32: &[0xfba3e203],
        },
        RomEntry {
            name: "1147-10b.061",
            size: 0x20000,
            offset: 0x100000,
            crc32: &[0xca4308cf],
        },
        RomEntry {
            name: "1148-13b.061",
            size: 0x20000,
            offset: 0x120000,
            crc32: &[0x23ddd45c],
        },
        RomEntry {
            name: "1149-16b.061",
            size: 0x20000,
            offset: 0x140000,
            crc32: &[0xd77cd1d0],
        },
        RomEntry {
            name: "1150-18b.061",
            size: 0x20000,
            offset: 0x160000,
            crc32: &[0xa37157b8],
        },
        RomEntry {
            name: "1129-21b.061",
            size: 0x10000,
            offset: 0x180000,
            crc32: &[0x294aaa02],
        },
        RomEntry {
            name: "1130-23b.061",
            size: 0x10000,
            offset: 0x190000,
            crc32: &[0xdd610817],
        },
        RomEntry {
            name: "1131-24b.061",
            size: 0x10000,
            offset: 0x1A0000,
            crc32: &[0xe8e2f919],
        },
        RomEntry {
            name: "1132-25b.061",
            size: 0x10000,
            offset: 0x1B0000,
            crc32: &[0xc79f8ffc],
        },
        // Second copy of the four small chips in the high half.
        RomEntry {
            name: "1129-21b.061",
            size: 0x10000,
            offset: 0x1C0000,
            crc32: &[0x294aaa02],
        },
        RomEntry {
            name: "1130-23b.061",
            size: 0x10000,
            offset: 0x1D0000,
            crc32: &[0xdd610817],
        },
        RomEntry {
            name: "1131-24b.061",
            size: 0x10000,
            offset: 0x1E0000,
            crc32: &[0xe8e2f919],
        },
        RomEntry {
            name: "1132-25b.061",
            size: 0x10000,
            offset: 0x1F0000,
            crc32: &[0xc79f8ffc],
        },
    ],
};

/// JSA-I sound board program: one 64 KB chip. Its low 16 KB are the four pages
/// the sound CPU's banked window selects between and the rest is its fixed ROM,
/// which [`AtariJsa1::load_rom`] splits.
pub static TOOBIN_SOUND_ROM: RomRegion = RomRegion {
    size: 0x10000,
    entries: &[RomEntry {
        name: "1141-2k.061",
        size: 0x10000,
        offset: 0x0000,
        crc32: &[0xc0dcce1a],
    }],
};

/// Alphanumerics character ROM: 1024 tiles, 8×8, 2bpp.
pub static TOOBIN_ALPHA_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[RomEntry {
        name: "1142-20h.061",
        size: 0x4000,
        offset: 0x0000,
        crc32: &[0xa6ab551f],
    }],
};

/// Interleave the even/odd program chips into the 0x80000-byte big-endian
/// program image. Each pair contributes 0x20000 bytes: chip `2n` supplies the
/// high byte of every word and chip `2n+1` the low byte.
fn load_maincpu_image(rom_set: &RomSet) -> Result<Vec<u8>, RomLoadError> {
    let chips = TOOBIN_PROGRAM_ROM.load(rom_set)?;
    let mut image = vec![0u8; 0x80000];
    for pair in 0..4 {
        let even = pair * 0x20000;
        let odd = even + 0x10000;
        let dst = pair * 0x20000;
        for i in 0..0x10000 {
            image[dst + 2 * i] = chips[even + i];
            image[dst + 2 * i + 1] = chips[odd + i];
        }
    }
    Ok(image)
}

// ---------------------------------------------------------------------------
// Tile decode
// ---------------------------------------------------------------------------

/// Playfield tiles: 8×8, four bitplanes. Two planes live in each half of the
/// 0x80000-byte region, at bit offsets 0 and 4 within a nibble pair. The board
/// numbers planes most-significant first, so the list is reversed for
/// `decode_gfx`, whose plane 0 is pen bit 0.
const PLAYFIELD_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[4, 0, 0x20_0000 + 4, 0x20_0000],
    x_offsets: &[0, 1, 2, 3, 8, 9, 10, 11],
    y_offsets: &[0, 16, 32, 48, 64, 80, 96, 112],
    char_increment: 128,
};

/// Number of 8×8 playfield tiles: 0x40000 bytes per half at 16 bytes a tile.
const PLAYFIELD_TILE_COUNT: usize = 0x4000;

/// Motion-object tiles: 16×16, four bitplanes, two per half of the 0x200000-byte
/// region. Same most-significant-first plane order as the playfield.
const MO_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[4, 0, 0x80_0000 + 4, 0x80_0000],
    x_offsets: &[0, 1, 2, 3, 8, 9, 10, 11, 16, 17, 18, 19, 24, 25, 26, 27],
    y_offsets: &[
        0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448, 480,
    ],
    char_increment: 512,
};

/// Number of 16×16 motion-object tiles: 0x100000 bytes per half at 64 bytes a
/// tile.
const MO_TILE_COUNT: usize = 0x4000;

/// Alpha tiles: 8×8, two bitplanes at bit offsets 0 and 4, reversed for
/// `decode_gfx`.
const ALPHA_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[4, 0],
    x_offsets: &[0, 1, 2, 3, 8, 9, 10, 11],
    y_offsets: &[0, 16, 32, 48, 64, 80, 96, 112],
    char_increment: 128,
};

/// Number of 8×8 alpha tiles in the character ROM.
const ALPHA_TILE_COUNT: usize = 0x400;

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------

/// Master crystal: 32 MHz, divided by 4 for the 68010 and by 2 for the dot
/// clock. The raster is 640 dots by 416 lines with 512×384 visible, so one
/// scanline is 320 CPU cycles and a frame is 133,120 of them: 60.096 Hz, an
/// exact division with nothing rounded.
pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 8_000_000,
    cycles_per_scanline: 320,
    total_scanlines: 416,
    display_width: 512,
    display_height: 384,
    // The framebuffer is native (unrotated) landscape; the cabinet monitor is
    // portrait, which is the aspect the frontend presents the rotated image at.
    display_aspect: Some((3, 4)),
};

/// The board's two crystals and what is divided out of each.
///
/// The main board runs off 32 MHz: the 68010 at a quarter of it and the dot
/// clock at half. The JSA-I sound board has its own 3.579545 MHz crystal, so
/// its parts keep no fixed ratio to the main CPU at all: the sound 6502 and its
/// POKEY take half of that crystal and the YM2151 takes all of it. Stepping the
/// sound CPU off the main CPU is therefore a fractional divide, which the clock
/// tree's phase accumulator carries rather than any integer counter.
pub fn clock_tree() -> ClockTree {
    use phosphor_core::core::RootId;
    let mut t = ClockTree::new(32_000_000);
    let cpu = t.add_domain(Clk::Cpu, RootId::MAIN, 1, 4); // 8 MHz 68010
    let dot = t.add_domain(Clk::Pixel, RootId::MAIN, 1, 2); // 16 MHz dot clock

    let sound_xtal = t.add_root(3_579_545);
    t.add_domain(Clk::SoundCpu, sound_xtal, 1, 2); // 1.789772 MHz 6502
    t.add_domain(Clk::Pokey, sound_xtal, 1, 2); // POKEY shares the sound CPU's rate
    t.add_domain(Clk::Psg, sound_xtal, 1, 1); // YM2151, twice the sound CPU

    t.set_step_domain(cpu);
    t.set_raster(dot, 640, 0);
    t
}

/// First blanked scanline. Lines 0 to 383 are visible.
const VBLANK_SCANLINE: u16 = 384;

const VISIBLE_WIDTH: usize = TIMING.display_width as usize; // 512
const VISIBLE_HEIGHT: usize = TIMING.display_height as usize; // 384

/// The playfield map is 128×64 tiles of 8×8, so it wraps in a 1024×512 space.
const PF_WRAP_X: usize = 0x3FF;
const PF_WRAP_Y: usize = 0x1FF;

/// Motion objects position in a 1024×512 space, the power-of-two round-up of
/// their 10-bit X and 9-bit Y fields.
const MO_WRAP_X: i32 = 0x3FF;
const MO_WRAP_Y: i32 = 0x1FF;

/// Sentinel for "no motion-object pixel here" in the object line buffer. Pen 0
/// is the transparent pen, so a real index is never this.
const MO_TRANSPARENT: u16 = 0xFFFF;

// ---------------------------------------------------------------------------
// Address-space regions (backed memory; registers are decoded in the Bus impl)
// ---------------------------------------------------------------------------

/// Region bases are the *masked* addresses (see [`ToobinBoard::mask_addr`]),
/// which is where every access lands by the time it reaches the map.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub enum Region {
    Rom = 1,
    Ram = 2,
    Playfield = 3,
    Alpha = 4,
    Mob = 5,
    Palette = 6,
}

// ---------------------------------------------------------------------------
// Control register block
// ---------------------------------------------------------------------------
//
// The registers occupy 0xC78000 through 0xC79FFF in masked space. Address bits
// 0 through 5 are not decoded, so each register answers across a 64-byte span;
// what is left after masking them off is the offset within the block.

/// First and last masked address of the control block.
const REGISTER_BLOCK: std::ops::RangeInclusive<u32> = 0x00C7_8000..=0x00C7_9FFF;
/// The address bits that pick a register out of the block.
const REGISTER_SELECT: u32 = 0x1FC0;

const REG_WATCHDOG: u32 = 0x0000;
const REG_SOUND_COMMAND: u32 = 0x0100;
const REG_INTENSITY: u32 = 0x0300;
const REG_INTERRUPT_SCAN: u32 = 0x0340;
const REG_SLIP: u32 = 0x0380;
const REG_SCANLINE_INT_ACK: u32 = 0x03C0;
const REG_SOUND_RESET: u32 = 0x0400;
const REG_EEPROM_ENABLE: u32 = 0x0500;
const REG_XSCROLL: u32 = 0x0600;
const REG_YSCROLL: u32 = 0x0700;
const REG_SWITCHES: u32 = 0x0800;
const REG_STATUS: u32 = 0x1000;
const REG_SOUND_RESPONSE: u32 = 0x1800;

// ---------------------------------------------------------------------------
// ToobinBoard
// ---------------------------------------------------------------------------

/// Everything the 68010 talks to. The CPU itself lives on [`ToobinSystem`].
#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct ToobinBoard {
    /// Program ROM, work RAM and the four video RAMs.
    #[debug_map(cpu = 0)]
    #[save(id = 1)]
    pub(crate) map: AddressSpace32,

    /// Decoded 8×8 4bpp playfield tiles. Not CPU-addressable.
    #[save_skip]
    playfield_gfx: GfxCache,
    /// Decoded 16×16 4bpp motion-object tiles. Not CPU-addressable.
    #[save_skip]
    mo_gfx: GfxCache,
    /// Decoded 8×8 2bpp alpha tiles. Not CPU-addressable.
    #[save_skip]
    alpha_gfx: GfxCache,

    /// Horizontal scroll latch. The playfield and the objects both take the
    /// pixel scroll from bits 6 and up; the low six bits are not a scroll.
    #[save(id = 2)]
    pub(crate) xscroll: u16,
    /// Vertical scroll latch, read the same way as [`Self::xscroll`].
    #[save(id = 3)]
    pub(crate) yscroll: u16,
    /// Object list start pointer. The board divides the screen into bands of
    /// 1024 pixels, which at 384 lines is one band, so this single latch is the
    /// starting link for the whole frame.
    #[save(id = 4)]
    pub(crate) slip: u16,

    /// Global intensity: 0 (dark) through 31 (full), stored already inverted
    /// from the register's active-low 5 bits. Pens whose palette word has bit
    /// 15 set ignore it and always display at full brightness.
    #[save(id = 5)]
    pub(crate) intensity: u8,

    /// Scanline the interrupt comparator fires on, from the 9-bit latch.
    #[save(id = 6)]
    pub(crate) interrupt_scan: u16,
    /// Scanline interrupt latch (IRQ1), held until acked.
    #[save(id = 7)]
    pub(crate) scanline_int: bool,

    /// EEPROM: 2048 bytes, each on the low half of a word, gated by
    /// [`Self::eeprom_unlocked`].
    #[save(id = 9)]
    pub(crate) eeprom: Vec<u8>,
    /// Unlock latch. The part re-locks after a single accepted write.
    #[save(id = 10)]
    pub(crate) eeprom_unlocked: bool,

    /// Paddle and throw switches (port at 0xFF8800), active low.
    #[save(id = 11)]
    pub(crate) buttons: u16,
    /// Service / self-test switch, active low (bit 12 of the status port).
    #[save(id = 12)]
    pub(crate) service: bool,

    /// JSA-I sound board: a 6502 with a YM2151 and a POKEY, on its own crystal.
    /// It also carries this game's coin switches.
    #[debug_device("Sound")]
    #[save(id = 18)]
    pub(crate) sound: AtariJsa1,
    /// The board's clock tree, as [`clock_tree`] declares it, stepped in
    /// main-CPU cycles.
    #[debug_device("Clocks")]
    #[save(id = 19)]
    clocks: ClockTree,
    /// A handle into the clock tree, which is itself saved.
    #[save_skip]
    sound_dom: DomainId,
    /// Removes the POKEY's unipolar DC from the mix, the way the cabinet's
    /// AC-coupled amplifier does. Two samples of filter history, which a load
    /// re-establishes within a sample or two of resuming.
    #[save_skip]
    dc_blocker: DcBlocker,
    /// Samples already mixed and waiting for the frontend to drain.
    #[save_skip]
    audio_buffer: SampleRing<i16>,

    #[save(id = 13)]
    pub(crate) clock: u64,
    /// Vertical-blank count since the last watchdog strobe. Eight reboots it.
    #[save(id = 14)]
    pub(crate) watchdog_count: u8,

    /// Native 512×384 RGB framebuffer, filled one row at a time as the beam
    /// reaches each visible scanline. Derived output, so not saved: the next
    /// frame overwrites every row.
    #[save_skip]
    framebuffer: Vec<u8>,
}

impl ToobinBoard {
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
            Region::Playfield,
            "Playfield RAM",
            0xC0_0000,
            0x8000,
            AccessKind::ReadWrite,
        )
        .region(
            Region::Alpha,
            "Alpha RAM",
            0xC0_8000,
            0x1800,
            AccessKind::ReadWrite,
        )
        .region(
            Region::Mob,
            "Motion-object RAM",
            0xC0_9800,
            0x800,
            AccessKind::ReadWrite,
        )
        .region(
            Region::Palette,
            "Palette RAM",
            0xC1_0000,
            0x800,
            AccessKind::ReadWrite,
        )
        .region(
            Region::Ram,
            "Work RAM",
            0xC7_C000,
            0x4000,
            AccessKind::ReadWrite,
        );
        map
    }

    /// The 68010 this board is built around.
    pub fn new_cpu() -> M68000 {
        let mut cpu = M68000::new();
        cpu.variant = M68kVariant::M68010;
        cpu
    }

    pub fn new() -> Self {
        let clocks = clock_tree();
        let sound_dom = clocks
            .find(Clk::SoundCpu)
            .expect("clock tree declares a sound CPU domain");
        let rate = phosphor_core::audio::host_sample_rate() as u32;
        Self {
            map: Self::build_map(),
            sound: AtariJsa1::new(JsaPokey::Fitted),
            clocks,
            sound_dom,
            dc_blocker: DcBlocker::new(rate),
            audio_buffer: SampleRing::with_capacity(2048),
            playfield_gfx: GfxCache::new(PLAYFIELD_TILE_COUNT, 8, 8),
            mo_gfx: GfxCache::new(MO_TILE_COUNT, 16, 16),
            alpha_gfx: GfxCache::new(ALPHA_TILE_COUNT, 8, 8),
            xscroll: 0,
            yscroll: 0,
            slip: 0,
            intensity: 31,
            interrupt_scan: 0,
            scanline_int: false,
            eeprom: vec![0xFF; 0x800],
            eeprom_unlocked: false,
            buttons: 0xFFFF,
            service: false,
            clock: 0,
            watchdog_count: 0,
            framebuffer: vec![0; VISIBLE_WIDTH * VISIBLE_HEIGHT * 3],
        }
    }

    // -- ROM loading ---------------------------------------------------------

    pub fn load_program(&mut self, image: &[u8]) {
        let rom = self.map.region_data_mut(Region::Rom);
        let n = image.len().min(rom.len());
        rom[..n].copy_from_slice(&image[..n]);
    }

    pub fn load_playfield_gfx(&mut self, tiles: &[u8]) {
        self.playfield_gfx = decode_gfx(tiles, 0, PLAYFIELD_TILE_COUNT, &PLAYFIELD_LAYOUT);
    }

    pub fn load_mo_gfx(&mut self, tiles: &[u8]) {
        self.mo_gfx = decode_gfx(tiles, 0, MO_TILE_COUNT, &MO_LAYOUT);
    }

    pub fn load_alpha_gfx(&mut self, tiles: &[u8]) {
        self.alpha_gfx = decode_gfx(tiles, 0, ALPHA_TILE_COUNT, &ALPHA_LAYOUT);
    }

    // -- Accessors used by tests and the debugger ----------------------------

    pub fn clock(&self) -> u64 {
        self.clock
    }

    pub fn nvram(&self) -> &[u8] {
        &self.eeprom
    }

    pub fn load_nvram(&mut self, data: &[u8]) {
        let n = data.len().min(self.eeprom.len());
        self.eeprom[..n].copy_from_slice(&data[..n]);
    }

    // -----------------------------------------------------------------------
    // Video
    // -----------------------------------------------------------------------

    /// Resolve one palette word to RGB, applying the global intensity.
    ///
    /// The word is `F--- ---R RRRR GGGG GBBB BB` in the low 15 bits with bit 15
    /// (`F`) marking a pen that is exempt from dimming. Each 5-bit component is
    /// scaled to 8 bits by `(c * 224) >> 5` and then given a 38-count pedestal
    /// unless it is exactly zero, which is the video DAC's black level: the
    /// darkest non-black step sits well above black rather than just above it.
    fn palette_rgb(&self, word: u16) -> (u8, u8, u8) {
        let comp = |c: u16| -> u32 {
            let v = ((c & 0x1F) as u32 * 224) >> 5;
            if v != 0 { v + 38 } else { 0 }
        };
        let (r, g, b) = (comp(word >> 10), comp(word >> 5), comp(word));
        // Bit 15 set means this pen ignores the intensity control.
        if word & 0x8000 != 0 {
            return (r as u8, g as u8, b as u8);
        }
        let scale = self.intensity as u32;
        let dim = |v: u32| ((v * scale) / 31) as u8;
        (dim(r), dim(g), dim(b))
    }

    /// Copy the latest framebuffer out. This does not draw: every visible row
    /// was composited at its own scanline boundary in
    /// [`render_scanline`](Self::render_scanline).
    pub fn render_frame(&self, buffer: &mut [u8]) {
        buffer.copy_from_slice(&self.framebuffer);
    }

    /// Composite one visible row out of the video state as it stands at that
    /// row's scanline boundary.
    ///
    /// Every register the layers read is read here (both scroll latches, the
    /// object list pointer, the intensity control and the palette), so a
    /// mid-frame write to any of them splits the picture at the row it lands
    /// on, which is what the game's scroll updates depend on.
    fn render_scanline(&mut self, sy: usize) {
        let mut pf = [0u16; VISIBLE_WIDTH];
        let mut pf_priority = [0u8; VISIBLE_WIDTH];
        self.draw_playfield_row(&mut pf, &mut pf_priority, sy);

        let mut mo = [MO_TRANSPARENT; VISIBLE_WIDTH];
        self.draw_motion_objects_row(&mut mo, sy);

        // Merge the objects over the playfield. The rule the PAL implements is
        // that an object pixel loses only to a playfield pixel that is both in
        // a raised priority category and has pen bit 3 set; everywhere else the
        // object wins.
        for x in 0..VISIBLE_WIDTH {
            let m = mo[x];
            if m != MO_TRANSPARENT && (pf_priority[x] == 0 || pf[x] & 0x08 == 0) {
                pf[x] = m;
            }
        }

        self.draw_alpha_row(&mut pf, sy);

        // Resolve to RGB against the palette as it stands on this row. The
        // one-entry memo pays off because a tilemap row holds long runs of one
        // index, and it keeps this from decoding 512 palette words per row.
        let out = sy * VISIBLE_WIDTH * 3;
        let mut last = usize::MAX;
        let mut rgb = (0u8, 0u8, 0u8);
        for (x, &idx) in pf.iter().enumerate() {
            let i = idx as usize & 0x3FF;
            if i != last {
                last = i;
                let pal = self.map.region_data(Region::Palette);
                rgb = self.palette_rgb(u16::from_be_bytes([pal[i * 2], pal[i * 2 + 1]]));
            }
            let o = out + x * 3;
            self.framebuffer[o] = rgb.0;
            self.framebuffer[o + 1] = rgb.1;
            self.framebuffer[o + 2] = rgb.2;
        }
    }

    /// Rasterize one row of the 128×64 playfield into the index row, and record
    /// each pixel's priority category alongside it for the object merge.
    ///
    /// Each cell is two words: the first carries the color and the priority
    /// category, the second the tile code and the two flip bits. The row and the
    /// line within the tile are fixed by `sy`, so they hoist out of the loop,
    /// and the cell words plus the tile's 8-pixel line are fetched once per tile
    /// column rather than once per pixel: 65 fetches a row instead of 512.
    fn draw_playfield_row(
        &self,
        index: &mut [u16; VISIBLE_WIDTH],
        priority: &mut [u8; VISIBLE_WIDTH],
        sy: usize,
    ) {
        let pf_ram = self.map.region_data(Region::Playfield);
        let xscroll = (self.xscroll >> 6) as usize;
        let yscroll = (self.yscroll >> 6) as usize;

        let src_y = (sy + yscroll) & PF_WRAP_Y;
        let row_base = (src_y / 8) * 128;
        let ty = src_y % 8;

        let mut cached_cell = usize::MAX;
        let mut hflip = false;
        let mut pal_base = 0usize;
        let mut cat = 0u8;
        let mut line: &[u8] = &[];

        for sx in 0..VISIBLE_WIDTH {
            let src_x = (sx + xscroll) & PF_WRAP_X;
            let cell = row_base + src_x / 8;

            if cell != cached_cell {
                cached_cell = cell;
                let w0 = u16::from_be_bytes([pf_ram[cell * 4], pf_ram[cell * 4 + 1]]);
                let w1 = u16::from_be_bytes([pf_ram[cell * 4 + 2], pf_ram[cell * 4 + 3]]);
                let code = (w1 & 0x3FFF) as usize;
                cat = ((w0 >> 4) & 3) as u8;
                pal_base = (w0 & 0x0F) as usize * 16;
                hflip = w1 & 0x4000 != 0;
                let row = if w1 & 0x8000 != 0 { 7 - ty } else { ty };
                line = self
                    .playfield_gfx
                    .row_slice(code % self.playfield_gfx.count(), row);
            }

            let tx = if hflip { 7 - src_x % 8 } else { src_x % 8 };
            index[sx] = (pal_base + line[tx] as usize) as u16;
            priority[sx] = cat;
        }
    }

    /// Draw one row of the 64×48 alpha overlay over the index row.
    ///
    /// The overlay is drawn 1:1 from the origin with no scroll, and it covers
    /// the visible raster exactly. Pen 0 is transparent.
    fn draw_alpha_row(&self, index: &mut [u16; VISIBLE_WIDTH], sy: usize) {
        let alpha = self.map.region_data(Region::Alpha);
        let row_base = (sy / 8) * 64;
        let ty = sy % 8;

        let mut cached_cell = usize::MAX;
        let mut hflip = false;
        let mut pal_base = 0usize;
        let mut line: &[u8] = &[];

        for (sx, out) in index.iter_mut().enumerate() {
            let cell = row_base + sx / 8;
            if cell != cached_cell {
                cached_cell = cell;
                let data = u16::from_be_bytes([alpha[cell * 2], alpha[cell * 2 + 1]]);
                let code = (data & 0x3FF) as usize;
                hflip = data & 0x0400 != 0;
                // Alpha pens occupy 0x200 and up, four to a color set.
                pal_base = 0x200 + ((data >> 12) & 0x0F) as usize * 4;
                line = self.alpha_gfx.row_slice(code % self.alpha_gfx.count(), ty);
            }
            let tx = if hflip { 7 - sx % 8 } else { sx % 8 };
            let pen = line[tx];
            if pen != 0 {
                *out = (pal_base + pen as usize) as u16;
            }
        }
    }

    /// Walk the object list and rasterize the one row of each object that falls
    /// on this scanline.
    ///
    /// The list is 256 entries of four words, chained through the link field in
    /// word 2 and started from the [`slip`](Self::slip) pointer. Each entry
    /// describes a rectangle of up to 8×8 tiles of 16×16 pixels. The vertical
    /// range test is taken on word 0 alone, before the other words are read, so
    /// an entry that is not on this line costs one word fetch.
    fn draw_motion_objects_row(&self, mo: &mut [u16; VISIBLE_WIDTH], sy: usize) {
        let ram = self.map.region_data(Region::Mob);
        let word = |wi: usize| u16::from_be_bytes([ram[wi * 2], ram[wi * 2 + 1]]);

        let mo_xscroll = (self.xscroll >> 6) as i32;
        let mo_yscroll = ((self.yscroll >> 6) & 0x1FF) as i32;

        let mut visited = [false; 256];
        let mut link = (self.slip & 0xFF) as usize;

        for _ in 0..256 {
            if visited[link] {
                break;
            }
            visited[link] = true;

            let w0 = word(link * 4);
            let width = ((w0 & 0x0007) as usize) + 1;
            let height = (((w0 >> 3) & 0x0007) as usize) + 1;

            // Word 0 carries the Y position, the size and the absolute-coordinate
            // flag, so whether this entry touches this line is decidable before
            // anything else is read. The link still has to be followed either
            // way: the list is a chain, not an array.
            let mut ypos = -(((w0 >> 6) & 0x1FF) as i32);
            let mut xpos_pending = None;
            if w0 & 0x8000 == 0 {
                ypos -= mo_yscroll;
            }
            ypos -= (height * 16) as i32;
            ypos &= MO_WRAP_Y;
            if ypos > (VISIBLE_HEIGHT - 1) as i32 {
                ypos -= MO_WRAP_Y + 1;
            }

            let dy = sy as i32 - ypos;
            if dy >= 0 && dy < (height * 16) as i32 {
                let w3 = word(link * 4 + 3);
                let mut xpos = ((w3 >> 6) & 0x3FF) as i32;
                if w0 & 0x8000 == 0 {
                    xpos -= mo_xscroll;
                }
                xpos &= MO_WRAP_X;
                if xpos > (VISIBLE_WIDTH - 1) as i32 {
                    xpos -= MO_WRAP_X + 1;
                }
                xpos_pending = Some((xpos, w3));
            }

            if let Some((xpos, w3)) = xpos_pending {
                let w1 = word(link * 4 + 1);
                self.draw_mo_entry_row(mo, w0, w1, w3, xpos, sy as i32 - ypos, width, height);
            }

            link = (word(link * 4 + 2) & 0x00FF) as usize;
        }
    }

    /// Rasterize the single 16-pixel-tall band of one object that crosses this
    /// row, across all of its tile columns.
    ///
    /// Tiles are numbered down each column before moving to the next column, so
    /// the code for the tile at column `c`, row `r` is `base + c * height + r`.
    /// The flip bits reverse where a tile lands on screen and mirror it
    /// internally, but they do not renumber the tiles.
    #[allow(clippy::too_many_arguments)]
    fn draw_mo_entry_row(
        &self,
        mo: &mut [u16; VISIBLE_WIDTH],
        w0: u16,
        w1: u16,
        w3: u16,
        xpos: i32,
        dy: i32,
        width: usize,
        height: usize,
    ) {
        let base_code = (w1 & 0x3FFF) as usize;
        let hflip = w1 & 0x4000 != 0;
        let vflip = w1 & 0x8000 != 0;
        let pal_base = 0x100 + (w3 & 0x000F) as usize * 16;
        let _ = w0;

        // Which 16-pixel band of the object this row lands in, and the line
        // inside it.
        let slot_y = (dy / 16) as usize;
        let py = (dy % 16) as usize;
        let tile_y = if vflip { height - 1 - slot_y } else { slot_y };
        let src_row = if vflip { 15 - py } else { py };

        for slot_x in 0..width {
            let sx0 = xpos + (slot_x * 16) as i32;
            if sx0 >= VISIBLE_WIDTH as i32 || sx0 <= -16 {
                continue;
            }
            let tile_x = if hflip { width - 1 - slot_x } else { slot_x };
            let code = (base_code + tile_x * height + tile_y) % self.mo_gfx.count();
            let line = self.mo_gfx.row_slice(code, src_row);
            for px in 0..16usize {
                let dx = sx0 + px as i32;
                if dx < 0 || dx >= VISIBLE_WIDTH as i32 {
                    continue;
                }
                let pen = line[if hflip { 15 - px } else { px }];
                if pen == 0 {
                    continue; // transparent pen
                }
                mo[dx as usize] = (pal_base + pen as usize) as u16;
            }
        }
    }

    // -----------------------------------------------------------------------
    // Stepping
    // -----------------------------------------------------------------------

    /// Work that only happens on the first cycle of a scanline: the scanline
    /// interrupt comparator, and the row the beam is about to draw.
    pub(crate) fn begin_scanline(&mut self, scanline: u16) {
        if scanline == (self.interrupt_scan & 0x1FF) {
            self.scanline_int = true;
        }
        if (scanline as usize) < VISIBLE_HEIGHT {
            self.render_scanline(scanline as usize);
        }
    }

    fn begin_cycle_inner(&mut self, cpu: &M68000) {
        if self.map.debug_active() {
            let pc = cpu.at_instruction_boundary().then_some(cpu.pc());
            self.map.latch_access_context(self.clock, pc);
        }
    }

    fn end_cycle(&mut self) {
        // The sound board is on its own crystal, so its cycles land on a
        // fractional divide of the main CPU's rather than every Nth one.
        if self.clocks.tick(self.sound_dom) {
            self.sound.tick();
        }
        self.clock += 1;
    }

    /// Drain the sound board's mixed audio, strip the POKEY's DC, then scale and
    /// clamp to signed 16-bit into the pending buffer. Called once per frame.
    pub fn end_frame_audio(&mut self) {
        let mut samples = self.sound.drain_audio();
        self.dc_blocker.process_slice(&mut samples);
        self.audio_buffer.extend(
            samples
                .iter()
                .map(|&y| (y * 2.0 * 32767.0).clamp(-32767.0, 32767.0) as i16),
        );
    }

    /// Copy pending audio into the frontend's buffer.
    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.audio_buffer.pop_front_into(buffer)
    }

    pub fn instruction_boundaries(cpu: &M68000) -> u32 {
        u32::from(cpu.at_instruction_boundary())
    }

    /// Advance the per-frame watchdog. The board reboots after eight vertical
    /// blanks with no strobe to the watchdog address.
    pub fn advance_watchdog(&mut self) -> bool {
        self.watchdog_count = self.watchdog_count.saturating_add(1);
        self.watchdog_count >= 8
    }

    /// Reset everything but the CPU. The EEPROM is non-volatile and survives.
    pub fn reset(&mut self) {
        self.sound.reset();
        self.clocks.reset();
        self.dc_blocker.reset();
        self.audio_buffer.clear();
        self.xscroll = 0;
        self.yscroll = 0;
        self.slip = 0;
        self.intensity = 31;
        self.interrupt_scan = 0;
        self.scanline_int = false;
        self.eeprom_unlocked = false;
        self.buttons = 0xFFFF;
        self.service = false;
        self.watchdog_count = 0;
        // The framebuffer is not cleared: the next frame's rows overwrite it.
    }

    // -----------------------------------------------------------------------
    // Bus decode
    // -----------------------------------------------------------------------

    /// Fold a CPU address into the space the board actually decodes.
    ///
    /// Three address lines (A19 through A21) are not wired to the decoder, so
    /// they are don't-cares and everything the board answers to lands in the
    /// masked space the region table is built in.
    #[inline]
    fn mask_addr(addr: u32) -> u32 {
        addr & 0x00C7_FFFF
    }

    /// True while the beam is in vertical blank.
    fn in_vblank(&self) -> bool {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();
        (frame_cycle / TIMING.cycles_per_scanline) as u16 >= VBLANK_SCANLINE
    }

    /// True while the beam is in horizontal blank: the visible 512 dots of a
    /// 640-dot line are drawn first, so the last 128 are blanked.
    fn in_hblank(&self) -> bool {
        let dot = (self.clock % TIMING.cycles_per_scanline) * 2;
        dot >= VISIBLE_WIDTH as u64
    }

    /// The paddle and throw switch port. Active low, and the unused upper bits
    /// idle high.
    fn read_buttons(&self) -> u16 {
        self.buttons | 0xFC00
    }

    /// The status port: blanking, the sound handshake and the service switch.
    ///
    /// Every bit is active low. Bit 15 falls during horizontal blank and bit 14
    /// during vertical blank; bit 13 falls while a sound command is latched and
    /// the sound CPU has not collected it; bit 12 is the service and self-test
    /// switch, which the sound board reads back on its own port as well.
    fn read_status(&self) -> u16 {
        let mut v = 0xFFFFu16;
        if self.in_hblank() {
            v &= !0x8000;
        }
        if self.in_vblank() {
            v &= !0x4000;
        }
        if self.sound.command_pending() {
            v &= !0x2000;
        }
        if self.service {
            v &= !0x1000;
        }
        v
    }

    /// The sound board has a response the main CPU has not collected, which is
    /// what holds its interrupt line down.
    pub(crate) fn sound_int(&self) -> bool {
        self.sound.response_pending()
    }

    /// Interrupt level the 68010 sees. The scanline and sound lines are wired to
    /// autovector levels 1 and 2, and to level 3 together, so both at once is a
    /// level 3 rather than the higher of the two.
    pub(crate) fn interrupt_level(&self) -> u8 {
        match (self.scanline_int, self.sound_int()) {
            (true, true) => 3,
            (false, true) => 2,
            (true, false) => 1,
            (false, false) => 0,
        }
    }

    /// Write one of the board's control registers.
    ///
    /// `reg` is the register's offset within the control block, reduced to the
    /// address bits the decoder actually uses: the low six are don't-cares, so
    /// [`REGISTER_SELECT`] keeps bits 6 through 12. `data` is the full word and
    /// `byte` the half a byte transfer drove, or the low half of a word write.
    fn write_register(&mut self, reg: u32, data: u16, byte: u8) {
        match reg {
            REG_WATCHDOG => self.watchdog_count = 0,
            // Sound command latch, which also pulses the sound CPU's NMI.
            REG_SOUND_COMMAND => self.sound.write_command(byte),
            REG_INTENSITY => self.intensity = !byte & 0x1F,
            REG_INTERRUPT_SCAN => self.interrupt_scan = data & 0x1FF,
            REG_SLIP => self.slip = data,
            REG_SCANLINE_INT_ACK => self.scanline_int = false,
            // A strobe, not a latch: the sound CPU reboots rather than being
            // held down, and any response it had not delivered goes with it.
            REG_SOUND_RESET => self.sound.reset_pulse(),
            REG_EEPROM_ENABLE => self.eeprom_unlocked = true,
            REG_XSCROLL => self.xscroll = data,
            REG_YSCROLL => self.yscroll = data,
            _ => {}
        }
    }

    pub(crate) fn bus_is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    pub(crate) fn bus_observe_cycle(
        &mut self,
        _master: BusMaster,
        _addr: u32,
        _signals: BusSignals,
    ) {
    }

    pub(crate) fn bus_read(&mut self, master: BusMaster, addr: u32) -> u16 {
        let a = Self::mask_addr(addr);
        let val = match a {
            0x00_0000..=0x07_FFFF
            | 0xC0_0000..=0xC0_9FFF
            | 0xC1_0000..=0xC1_07FF
            | 0xC7_C000..=0xC7_FFFF => self.map.read_bus_word_be(a),
            // Read at controls time, with nothing behind it.
            0xC7_6000..=0xC7_6001 => 0xFFFF,
            0xC7_A000..=0xC7_AFFF => 0xFF00 | self.eeprom[((a >> 1) & 0x7FF) as usize] as u16,
            _ if REGISTER_BLOCK.contains(&a) => match a & REGISTER_SELECT {
                REG_SWITCHES => self.read_buttons(),
                REG_STATUS => self.read_status(),
                // Reading the response latch clears its flag, which drops the
                // sound board's interrupt line.
                REG_SOUND_RESPONSE => 0xFF00 | self.sound.read_response() as u16,
                _ => 0xFFFF,
            },
            _ => 0xFFFF,
        };
        self.map.watch_read(0, master, addr, val as u32, 2);
        val
    }

    pub(crate) fn bus_write(&mut self, master: BusMaster, addr: u32, data: u16) {
        self.map.watch_write(0, master, addr, data as u32, 2);
        let a = Self::mask_addr(addr);
        match a {
            0x00_0000..=0x07_FFFF => {} // ROM
            0xC0_0000..=0xC0_9FFF | 0xC1_0000..=0xC1_07FF | 0xC7_C000..=0xC7_FFFF => {
                self.map.write_bus_word_be(a, data)
            }
            0xC7_A000..=0xC7_AFFF if self.eeprom_unlocked => {
                self.eeprom[((a >> 1) & 0x7FF) as usize] = data as u8;
                self.eeprom_unlocked = false;
            }
            _ if REGISTER_BLOCK.contains(&a) => {
                self.write_register(a & REGISTER_SELECT, data, data as u8)
            }
            _ => {}
        }
    }

    pub(crate) fn bus_check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            irq_level: self.interrupt_level(),
            // 0xFF means the 68000 core autovectors (vector 24 + level).
            irq_vector: 0xFF,
            ..Default::default()
        }
    }

    /// Read one byte, with the strobe implied by the address's low bit. The
    /// 68010 drives no A0: it puts an even address on the bus and asserts UDS
    /// for the even byte or LDS for the odd one.
    pub(crate) fn bus_read_byte(&mut self, master: BusMaster, addr: u32) -> u8 {
        let a = Self::mask_addr(addr);
        match a {
            0x00_0000..=0x07_FFFF
            | 0xC0_0000..=0xC0_9FFF
            | 0xC1_0000..=0xC1_07FF
            | 0xC7_6000..=0xC7_6001
            | 0xC7_8000..=0xC7_9FFF
            | 0xC7_C000..=0xC7_FFFF => {
                let word = self.bus_read(master, addr & !1);
                select_byte(word, addr)
            }
            // The EEPROM sits on the lower half of the word.
            0xC7_A000..=0xC7_AFFF if addr & 1 != 0 => self.eeprom[((a >> 1) & 0x7FF) as usize],
            _ => 0xFF,
        }
    }

    pub(crate) fn bus_write_byte(&mut self, master: BusMaster, addr: u32, data: u8) {
        self.map.watch_write(0, master, addr, data as u32, 1);
        let a = Self::mask_addr(addr);
        match a {
            0x00_0000..=0x07_FFFF => {} // ROM
            // Word-wide memory: patch the half this strobe selects and leave the
            // other alone.
            0xC0_0000..=0xC0_9FFF | 0xC1_0000..=0xC1_07FF | 0xC7_C000..=0xC7_FFFF => {
                let word_addr = a & !1;
                let word = self.map.read_bus_word_be(word_addr);
                let merged = if addr & 1 != 0 {
                    (word & 0xFF00) | data as u16
                } else {
                    (word & 0x00FF) | ((data as u16) << 8)
                };
                self.map.write_bus_word_be(word_addr, merged);
            }
            0xC7_A000..=0xC7_AFFF if self.eeprom_unlocked && addr & 1 != 0 => {
                self.eeprom[((a >> 1) & 0x7FF) as usize] = data;
                self.eeprom_unlocked = false;
            }
            // Word-wide registers: a byte transfer drives only its own half, so
            // the other half keeps what it had.
            _ if REGISTER_BLOCK.contains(&a) => {
                let reg = a & REGISTER_SELECT;
                let current = match reg {
                    REG_XSCROLL => self.xscroll,
                    REG_YSCROLL => self.yscroll,
                    REG_SLIP => self.slip,
                    REG_INTERRUPT_SCAN => self.interrupt_scan,
                    _ => 0,
                };
                let merged = if addr & 1 != 0 {
                    (current & 0xFF00) | data as u16
                } else {
                    (current & 0x00FF) | ((data as u16) << 8)
                };
                self.write_register(reg, merged, data);
            }
            _ => {}
        }
    }
}

impl Default for ToobinBoard {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Frame loop
// ---------------------------------------------------------------------------

/// A Toobin' bus: the board, seen through whatever holds it. [`tick`] is generic
/// over this so every access the 68010 makes resolves to a direct call rather
/// than a vtable entry.
pub trait ToobinBusView: Bus16 {
    fn board(&mut self) -> &mut ToobinBoard;
}

/// One CPU cycle, testing the frame position on every cycle. This is the
/// debugger's path; a whole frame goes through [`run_scanlines`], which hoists
/// that test out.
#[inline]
pub fn tick<B: ToobinBusView>(cpu: &mut M68000, bus: &mut B) {
    let board = bus.board();
    let frame_cycle = board.clock % TIMING.cycles_per_frame();
    if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
        board.begin_scanline((frame_cycle / TIMING.cycles_per_scanline) as u16);
    }
    step_cycle(cpu, bus);
}

/// Run `cycles` CPU cycles, scanline-outer and cycle-inner.
pub fn run_scanlines<B: ToobinBusView>(cpu: &mut M68000, bus: &mut B, cycles: u64) {
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

/// Run one frame. Whole scanlines go through [`run_scanlines`]; a partial
/// scanline at either end, which only happens after the debugger has left the
/// clock off-boundary, goes through [`tick`].
pub fn run_frame<B: ToobinBusView>(cpu: &mut M68000, bus: &mut B) {
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

#[inline]
fn step_cycle<B: ToobinBusView>(cpu: &mut M68000, bus: &mut B) {
    bus.board().begin_cycle_inner(cpu);
    cpu.execute_cycle(bus, BusMaster::Cpu(0));
    bus.board().end_cycle();
}

// ---------------------------------------------------------------------------
// Bus view
// ---------------------------------------------------------------------------

struct ToobinBus<'a> {
    board: &'a mut ToobinBoard,
}

impl ToobinBusView for ToobinBus<'_> {
    #[inline]
    fn board(&mut self) -> &mut ToobinBoard {
        self.board
    }
}

impl Bus for ToobinBus<'_> {
    type Address = u32;
    type Data = u16;

    fn is_halted_for(&self, master: BusMaster) -> bool {
        self.board.bus_is_halted_for(master)
    }

    fn observe_bus_cycle(&mut self, master: BusMaster, addr: u32, signals: BusSignals) {
        self.board.bus_observe_cycle(master, addr, signals);
    }

    fn read(&mut self, master: BusMaster, addr: u32) -> u16 {
        self.board.bus_read(master, addr)
    }

    fn write(&mut self, master: BusMaster, addr: u32, data: u16) {
        self.board.bus_write(master, addr, data);
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        self.board.bus_check_interrupts(target)
    }
}

impl Bus16 for ToobinBus<'_> {
    fn read_byte(&mut self, master: BusMaster, addr: u32) -> u8 {
        self.board.bus_read_byte(master, addr)
    }

    fn write_byte(&mut self, master: BusMaster, addr: u32, data: u8) {
        self.board.bus_write_byte(master, addr, data);
    }
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

// Switch-port bit assignments (all active low, port at 0xFF8800).
const BIT_P2_RIGHT_FWD: u8 = 0;
const BIT_P2_LEFT_FWD: u8 = 1;
const BIT_P2_LEFT_BACK: u8 = 2;
const BIT_P2_RIGHT_BACK: u8 = 3;
const BIT_P1_RIGHT_FWD: u8 = 4;
const BIT_P1_LEFT_FWD: u8 = 5;
const BIT_P1_LEFT_BACK: u8 = 6;
const BIT_P1_RIGHT_BACK: u8 = 7;
const BIT_P1_THROW: u8 = 8;
const BIT_P2_THROW: u8 = 9;

// Control ids.
const CTRL_SERVICE: InputId = InputId(0);
const CTRL_COIN1: InputId = InputId(11);
const CTRL_COIN2: InputId = InputId(12);
const CTRL_P1_LEFT_FWD: InputId = InputId(1);
const CTRL_P1_LEFT_BACK: InputId = InputId(2);
const CTRL_P1_RIGHT_FWD: InputId = InputId(3);
const CTRL_P1_RIGHT_BACK: InputId = InputId(4);
const CTRL_P1_THROW: InputId = InputId(5);
const CTRL_P2_LEFT_FWD: InputId = InputId(6);
const CTRL_P2_LEFT_BACK: InputId = InputId(7);
const CTRL_P2_RIGHT_FWD: InputId = InputId(8);
const CTRL_P2_RIGHT_BACK: InputId = InputId(9);
const CTRL_P2_THROW: InputId = InputId(10);

use phosphor_core::core::machine::{KeyId as K, PadButton as PB, PadControl as P};

/// Toobin' has no start buttons: a credit is spent by paddling off, so the coin
/// switches and the two players' controls are the whole panel. The coin
/// switches are on the sound board rather than the main board's switch port.
const TOOBIN_CONTROLS: &[InputControl] = &[
    InputControl {
        id: CTRL_COIN1,
        stable_name: "coin1",
        label: "Coin 1",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: CTRL_COIN2,
        stable_name: "coin2",
        label: "Coin 2",
        kind: InputKind::Coin,
        player: None,
        // Coin 1 takes the shared default and the service switch takes Num6, so
        // the second mech gets the next key along rather than either of those.
        default_bindings: &[DefaultBinding::Key(K::Num7)],
    },
    InputControl {
        id: CTRL_SERVICE,
        stable_name: "service",
        label: "Service / Self-Test",
        kind: InputKind::Service,
        player: None,
        default_bindings: crate::input_defaults::SERVICE,
    },
    // Player 1: the two paddles each row forward or backward.
    InputControl {
        id: CTRL_P1_LEFT_FWD,
        stable_name: "p1_left_paddle_forward",
        label: "P1 Left Paddle Forward",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: &[
            DefaultBinding::Key(K::A),
            DefaultBinding::Pad(P::Button(PB::X)),
        ],
    },
    InputControl {
        id: CTRL_P1_LEFT_BACK,
        stable_name: "p1_left_paddle_backward",
        label: "P1 Left Paddle Backward",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: &[
            DefaultBinding::Key(K::Q),
            DefaultBinding::Pad(P::Button(PB::Y)),
        ],
    },
    InputControl {
        id: CTRL_P1_RIGHT_FWD,
        stable_name: "p1_right_paddle_forward",
        label: "P1 Right Paddle Forward",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: &[
            DefaultBinding::Key(K::D),
            DefaultBinding::Pad(P::Button(PB::B)),
        ],
    },
    InputControl {
        id: CTRL_P1_RIGHT_BACK,
        stable_name: "p1_right_paddle_backward",
        label: "P1 Right Paddle Backward",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: &[
            DefaultBinding::Key(K::E),
            DefaultBinding::Pad(P::Button(PB::A)),
        ],
    },
    InputControl {
        id: CTRL_P1_THROW,
        stable_name: "p1_throw",
        label: "P1 Throw",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
    },
    // Player 2.
    InputControl {
        id: CTRL_P2_LEFT_FWD,
        stable_name: "p2_left_paddle_forward",
        label: "P2 Left Paddle Forward",
        kind: InputKind::Button,
        player: Some(2),
        default_bindings: &[DefaultBinding::Key(K::J)],
    },
    InputControl {
        id: CTRL_P2_LEFT_BACK,
        stable_name: "p2_left_paddle_backward",
        label: "P2 Left Paddle Backward",
        kind: InputKind::Button,
        player: Some(2),
        default_bindings: &[DefaultBinding::Key(K::U)],
    },
    InputControl {
        id: CTRL_P2_RIGHT_FWD,
        stable_name: "p2_right_paddle_forward",
        label: "P2 Right Paddle Forward",
        kind: InputKind::Button,
        player: Some(2),
        default_bindings: &[DefaultBinding::Key(K::L)],
    },
    InputControl {
        id: CTRL_P2_RIGHT_BACK,
        stable_name: "p2_right_paddle_backward",
        label: "P2 Right Paddle Backward",
        kind: InputKind::Button,
        player: Some(2),
        default_bindings: &[DefaultBinding::Key(K::O)],
    },
    InputControl {
        id: CTRL_P2_THROW,
        stable_name: "p2_throw",
        label: "P2 Throw",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(2),
        default_bindings: &[],
    },
];

/// Set or clear one bit of an active-low switch port: a pressed switch pulls
/// its line down.
fn set_switch_bit(reg: &mut u16, bit: u8, pressed: bool) {
    if pressed {
        *reg &= !(1 << bit);
    } else {
        *reg |= 1 << bit;
    }
}

// ---------------------------------------------------------------------------
// ToobinSystem
// ---------------------------------------------------------------------------

/// Atari Toobin'. The 68010 sits beside the board it drives.
#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct ToobinSystem {
    #[debug_cpu("M68010")]
    #[save(id = 1)]
    pub cpu: M68000,

    #[debug_bus]
    #[save(id = 2)]
    pub board: ToobinBoard,
}

impl ToobinSystem {
    pub fn new() -> Self {
        let mut sys = Self {
            cpu: ToobinBoard::new_cpu(),
            board: ToobinBoard::new(),
        };
        sys.reset();
        sys
    }

    /// Split into the CPU and a concrete bus view. Formed once per frame, never
    /// per cycle.
    fn split(&mut self) -> (&mut M68000, ToobinBus<'_>) {
        (
            &mut self.cpu,
            ToobinBus {
                board: &mut self.board,
            },
        )
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let image = load_maincpu_image(rom_set)?;
        self.board.load_program(&image);

        let pf = TOOBIN_PLAYFIELD_ROM.load(rom_set)?;
        self.board.load_playfield_gfx(&pf);

        let mo = TOOBIN_MO_ROM.load(rom_set)?;
        self.board.load_mo_gfx(&mo);

        let alpha = TOOBIN_ALPHA_ROM.load(rom_set)?;
        self.board.load_alpha_gfx(&alpha);

        let sound = TOOBIN_SOUND_ROM.load(rom_set)?;
        self.board.sound.load_rom(&sound);

        // The reset vectors live in the program ROM, so the CPU has to be reset
        // again now that there is something to fetch them from.
        self.reset();
        Ok(())
    }

    pub fn clock(&self) -> u64 {
        self.board.clock()
    }

    /// (sound-CPU cycles run, command pending, response pending) for headless
    /// bring-up diagnostics.
    pub fn sound_debug(&self) -> (u64, bool, bool) {
        self.board.sound.debug_state()
    }

    /// Step one cycle, returning the instruction-boundary mask the debugger
    /// counts with.
    pub fn step_cycle(&mut self) -> u32 {
        let (cpu, mut bus) = self.split();
        tick(cpu, &mut bus);
        ToobinBoard::instruction_boundaries(&self.cpu)
    }
}

impl Default for ToobinSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Capability traits
// ---------------------------------------------------------------------------

crate::impl_board_delegation!(ToobinSystem, board, TIMING, orientation);

impl ToobinBoard {
    /// The cabinet monitor is mounted rotated a quarter turn, and the rotation
    /// is declared rather than baked: `render_frame` fills the native landscape
    /// framebuffer and the frontend turns it.
    pub fn orientation(&self) -> Orientation {
        Orientation::ROT270
    }
}

impl MachineCore for ToobinSystem {
    crate::machine_core_metadata!("toobin", TIMING, clock_tree);

    fn run_frame(&mut self) {
        {
            let (cpu, mut bus) = self.split();
            run_frame(cpu, &mut bus);
        }

        if self.board.advance_watchdog() {
            self.reset();
        }

        self.board.end_frame_audio();
    }

    fn reset(&mut self) {
        self.board.reset();
        let (cpu, mut bus) = self.split();
        cpu.reset(&mut bus, BusMaster::Cpu(0));
    }
}

impl InputConfigurable for ToobinSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        TOOBIN_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let InputEvent::Button { id, pressed } = event else {
            return;
        };
        let bit = match id {
            CTRL_SERVICE => {
                // The same switch reaches both boards: the main board reads it
                // on its status port and the sound board on its own I/O port.
                self.board.service = pressed;
                self.board.sound.set_self_test(pressed);
                return;
            }
            // Coin mechs are wired to the sound board.
            CTRL_COIN1 => {
                self.board.sound.set_coin(0, pressed);
                return;
            }
            CTRL_COIN2 => {
                self.board.sound.set_coin(1, pressed);
                return;
            }
            CTRL_P1_LEFT_FWD => BIT_P1_LEFT_FWD,
            CTRL_P1_LEFT_BACK => BIT_P1_LEFT_BACK,
            CTRL_P1_RIGHT_FWD => BIT_P1_RIGHT_FWD,
            CTRL_P1_RIGHT_BACK => BIT_P1_RIGHT_BACK,
            CTRL_P1_THROW => BIT_P1_THROW,
            CTRL_P2_LEFT_FWD => BIT_P2_LEFT_FWD,
            CTRL_P2_LEFT_BACK => BIT_P2_LEFT_BACK,
            CTRL_P2_RIGHT_FWD => BIT_P2_RIGHT_FWD,
            CTRL_P2_RIGHT_BACK => BIT_P2_RIGHT_BACK,
            CTRL_P2_THROW => BIT_P2_THROW,
            _ => return,
        };
        set_switch_bit(&mut self.board.buttons, bit, pressed);
    }
}

impl SaveState for ToobinSystem {
    crate::machine_save_state!();
}

/// The EEPROM is the machine's battery-backed store.
impl Nvram for ToobinSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.nvram())
    }

    fn load_nvram(&mut self, data: &[u8]) {
        self.board.load_nvram(data);
    }
}

impl Profilable for ToobinSystem {}
crate::impl_map_debug_trace!(ToobinSystem, board.map);

/// Toobin' has no operator DIP switches: coinage and game options live in the
/// EEPROM, set through the operator menu.
impl phosphor_core::core::machine::DipSwitches for ToobinSystem {}

// ---------------------------------------------------------------------------
// Registry + disassembly
// ---------------------------------------------------------------------------

crate::register_machine!(ToobinSystem, "toobin", &["toobin"], TOOBIN_CONTROLS);

inventory::submit! {
    DisasmRegion {
        machine: "toobin",
        region: "main",
        cpu: DisasmCpu::M68000,
        org: 0,
        size: 0x80000,
        load: load_maincpu_image,
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "toobin",
        region: "sound",
        cpu: DisasmCpu::M6502,
        // The fixed half of the sound chip, which is where the program lives;
        // the low 16 KB are the four banked pages and are not linear code.
        org: 0x4000,
        size: 0xC000,
        load: |rs| TOOBIN_SOUND_ROM.load(rs).map(|v| v[0x4000..0x10000].to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A machine with no ROMs loaded, which is what makes the video tests below
    /// deterministic rather than a race with the program.
    ///
    /// Every program word is 0x0000, which the 68010 decodes as `ORI.B #$00,D0`
    /// with a 0x0000 immediate. That writes no memory, so the video state a test
    /// places stays exactly as placed for the whole frame, and the program
    /// counter advances four bytes an instruction, which over one frame stays
    /// well inside the 512 KB ROM window. The scanline interrupt is raised but
    /// never taken: the 68010 comes out of reset with its interrupt mask at 7
    /// and nothing here lowers it.
    fn blank_machine() -> ToobinSystem {
        ToobinSystem::new()
    }

    fn set_palette(sys: &mut ToobinSystem, index: usize, word: u16) {
        let pal = sys.board.map.region_data_mut(Region::Palette);
        pal[index * 2] = (word >> 8) as u8;
        pal[index * 2 + 1] = word as u8;
    }

    fn pixel(sys: &ToobinSystem, x: usize, y: usize) -> (u8, u8, u8) {
        let o = (y * VISIBLE_WIDTH + x) * 3;
        let fb = &sys.board.framebuffer;
        (fb[o], fb[o + 1], fb[o + 2])
    }

    /// White, and not exempt from the intensity control: bit 15 clear.
    const WHITE: u16 = 0x7FFF;

    /// A minimal sound-board program: write one byte to the response latch and
    /// then spin. Enough to raise the sound board's interrupt line the way the
    /// real board does, without standing in a whole sound program.
    ///
    /// The NMI and IRQ vectors point at an RTI because both fire on their own:
    /// a command raises the NMI and the board's periodic interrupt runs free.
    fn responder_sound_rom() -> Vec<u8> {
        let mut image = vec![0xFFu8; 0x10000];
        let prog: &[u8] = &[
            0xA9, 0x99, //       LDA #$99
            0x8D, 0x02, 0x2A, // STA $2A02   the response latch
            0x4C, 0x05, 0xF0, // JMP $F005   spin
        ];
        image[0xF000..0xF000 + prog.len()].copy_from_slice(prog);
        image[0xF040] = 0x40; // RTI
        image[0xFFFA] = 0x40; // NMI   -> 0xF040
        image[0xFFFB] = 0xF0;
        image[0xFFFC] = 0x00; // RESET -> 0xF000
        image[0xFFFD] = 0xF0;
        image[0xFFFE] = 0x40; // IRQ   -> 0xF040
        image[0xFFFF] = 0xF0;
        image
    }

    /// The frame loop reaches the scanline hook for every visible row.
    ///
    /// Without this the mid-frame test below would pass on a board that renders
    /// nothing at all, because both of its samples would be the untouched
    /// framebuffer.
    #[test]
    fn the_frame_loop_renders_every_visible_row() {
        let mut sys = blank_machine();
        set_palette(&mut sys, 0, WHITE);

        // The framebuffer starts black, so any row the hook misses stays black.
        assert_eq!(pixel(&sys, 0, 0), (0, 0, 0));

        sys.run_frame();

        for y in [0, 1, VISIBLE_HEIGHT / 2, VISIBLE_HEIGHT - 1] {
            assert_eq!(
                pixel(&sys, 0, y),
                (255, 255, 255),
                "row {y} was never rendered"
            );
        }
    }

    /// A mid-frame write to the intensity register splits the picture at the row
    /// the beam had reached, because every row resolves its own colors against
    /// the register as it stood at that row's scanline boundary.
    #[test]
    fn a_mid_frame_intensity_write_splits_the_picture_at_the_beam() {
        const SPLIT: u64 = 100;
        let mut sys = blank_machine();
        set_palette(&mut sys, 0, WHITE);

        // Rows 0 through SPLIT-1 are drawn at the power-on full intensity.
        {
            let (cpu, mut bus) = sys.split();
            run_scanlines(cpu, &mut bus, SPLIT * TIMING.cycles_per_scanline);
        }

        // Halve the brightness. The register is active low, so 16 written means
        // 15 of 31 held.
        sys.board.bus_write(BusMaster::Cpu(0), 0x00FF_8300, 16);

        {
            let (cpu, mut bus) = sys.split();
            let rest = TIMING.cycles_per_frame() - SPLIT * TIMING.cycles_per_scanline;
            run_scanlines(cpu, &mut bus, rest);
        }

        let dim = (255u32 * 15 / 31) as u8;
        assert_eq!(
            pixel(&sys, 0, (SPLIT - 1) as usize),
            (255, 255, 255),
            "the row above the split should keep full intensity"
        );
        assert_eq!(
            pixel(&sys, 0, SPLIT as usize),
            (dim, dim, dim),
            "the split row itself should already be dimmed"
        );
        assert_eq!(pixel(&sys, 0, VISIBLE_HEIGHT - 1), (dim, dim, dim));
    }

    /// The control registers decode on the block offset, not on the raw address.
    ///
    /// This is the shape of a bug that cost a boot: matching the register file
    /// against full addresses silently dropped every write, because three
    /// address lines are not wired to the decoder and the block offset is what
    /// survives the masking.
    #[test]
    fn control_register_writes_reach_their_latches() {
        let mut sys = blank_machine();
        let w = |sys: &mut ToobinSystem, addr: u32, data: u16| {
            sys.board.bus_write(BusMaster::Cpu(0), addr, data);
        };

        w(&mut sys, 0x00FF_8600, 0x1234);
        assert_eq!(sys.board.xscroll, 0x1234, "xscroll latch");
        w(&mut sys, 0x00FF_8700, 0x5678);
        assert_eq!(sys.board.yscroll, 0x5678, "yscroll latch");
        w(&mut sys, 0x00FF_8380, 0x00AB);
        assert_eq!(sys.board.slip, 0x00AB, "object list pointer");
        w(&mut sys, 0x00FF_8340, 0x01FF);
        assert_eq!(sys.board.interrupt_scan, 0x1FF, "scanline comparator");
        w(&mut sys, 0x00FF_8300, 0x0000);
        assert_eq!(sys.board.intensity, 31, "intensity is active low");

        // The watchdog strobe clears the count, and the acknowledge clears the
        // scanline latch.
        sys.board.watchdog_count = 5;
        w(&mut sys, 0x00FF_8000, 0);
        assert_eq!(sys.board.watchdog_count, 0, "watchdog strobe");
        sys.board.scanline_int = true;
        w(&mut sys, 0x00FF_83C0, 0);
        assert!(!sys.board.scanline_int, "scanline interrupt acknowledge");
    }

    /// The scanline comparator raises IRQ1 on the programmed row and nowhere
    /// else, and the two interrupt lines combine to level 3 rather than to the
    /// higher of the two.
    #[test]
    fn the_scanline_comparator_fires_on_its_programmed_row() {
        let mut sys = blank_machine();
        sys.board.interrupt_scan = 200;
        sys.board.scanline_int = false;

        sys.board.begin_scanline(199);
        assert!(!sys.board.scanline_int);
        sys.board.begin_scanline(200);
        assert!(sys.board.scanline_int);
        assert_eq!(sys.board.interrupt_level(), 1);

        // The sound line is the sound board's uncollected response, so raise it
        // by letting the sound CPU actually write one rather than poking a flag.
        sys.board.sound.load_rom(&responder_sound_rom());
        sys.board.sound.reset_pulse();
        for _ in 0..200 {
            sys.board.sound.tick();
        }
        assert!(sys.board.sound_int(), "the response raises the sound line");
        assert_eq!(sys.board.interrupt_level(), 3, "both lines make a level 3");

        sys.board.scanline_int = false;
        assert_eq!(sys.board.interrupt_level(), 2);

        sys.board.sound.read_response();
        assert_eq!(
            sys.board.interrupt_level(),
            0,
            "collecting it drops the line"
        );
    }

    /// The EEPROM accepts exactly one byte per unlock and then re-locks itself.
    #[test]
    fn the_eeprom_re_locks_after_one_write() {
        let mut sys = blank_machine();
        let addr = 0x00FF_A000;

        // Locked: the write is refused.
        sys.board.bus_write(BusMaster::Cpu(0), addr, 0x0042);
        assert_eq!(sys.board.eeprom[0], 0xFF);

        sys.board.bus_write(BusMaster::Cpu(0), 0x00FF_8500, 0);
        sys.board.bus_write(BusMaster::Cpu(0), addr, 0x0042);
        assert_eq!(sys.board.eeprom[0], 0x42, "one write lands");

        // Still locked for the next one.
        sys.board.bus_write(BusMaster::Cpu(0), addr + 2, 0x0043);
        assert_eq!(sys.board.eeprom[1], 0xFF, "the part re-locked");
    }

    /// The three unwired address lines make the whole board mirror, which is
    /// what lets the program's canonical addresses and the decoder's masked
    /// space be the same thing.
    #[test]
    fn unwired_address_lines_are_dont_cares() {
        // 0xFF8600 and 0xFFF8600-style aliases fold onto the same latch.
        assert_eq!(
            ToobinBoard::mask_addr(0x00FF_8600),
            ToobinBoard::mask_addr(0x00C7_8600)
        );
        // Work RAM and the register block do not collide after masking.
        assert_ne!(
            ToobinBoard::mask_addr(0x00FF_C000),
            ToobinBoard::mask_addr(0x00FF_8000)
        );
    }

    /// Palette words carry their own exemption from the intensity control, which
    /// is how the score panels stay bright while the river fades.
    #[test]
    fn bit_fifteen_exempts_a_pen_from_the_intensity_control() {
        let mut sys = blank_machine();
        sys.board.intensity = 0; // fully dimmed

        assert_eq!(sys.board.palette_rgb(WHITE), (0, 0, 0), "dimmed to black");
        assert_eq!(
            sys.board.palette_rgb(WHITE | 0x8000),
            (255, 255, 255),
            "bit 15 ignores the dimmer"
        );
    }
}
