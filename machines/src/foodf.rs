//! Atari Food Fight (1983) — the project's first MC68000-driven arcade machine.
//!
//! Food Fight runs a single Motorola 68000 @ 6.048 MHz against a sparse 24-bit
//! address space (ROM/RAM/sprite/playfield/NVRAM windows plus a band of memory-
//! mapped I/O). Sound is three POKEY chips; there is no sound CPU. Video is a
//! 32×32 column-scan tilemap playfield (8×8, 2bpp) composited with 16×16 2bpp
//! sprites. This module exists primarily to exercise the `M68000` core inside a
//! full board — see `core/src/cpu/m68000/`.
//!
//! Structurally this mirrors [`crate::ccastles`] (single Atari CPU + POKEY +
//! analog stick + NVRAM + GfxCache sprite decode), swapping `AddressSpace16` →
//! `AddressSpace32`, the 6502 → `M68000`, and the bitmap pipeline → a tilemap +
//! sprite pipeline.
//!
//! Hardware reference: MAME `src/mame/atari/foodf.cpp`.
//!
//! ## Memory map (word bus, big-endian; base windows only, mirrors ignored)
//! ```text
//!   000000-00FFFF  Program ROM (64 KB, interleaved even/odd chips)
//!   014000-01BFFF  Work RAM
//!   01C000-01C0FF  Sprite / motion-object RAM (64 entries × 2 words)
//!   800000-8007FF  Playfield tilemap RAM (32×32, column-scan)
//!   900000-9001FF  NVRAM (X2212, low byte only)
//!   940001         ADC0809 data read (analog stick)
//!   944000-944007  ADC channel select (write)
//!   948000         Digital inputs (SYSTEM, active-low)
//!   948001         Digital outputs (digital_w): flip, NVRAM store, INT acks
//!   950000-9501FF  Palette RAM (256 entries, low byte = RGB)
//!   954000         NVRAM recall
//!   958000         Watchdog reset
//!   A40000-A4001F  POKEY 2     A80000-A8001F  POKEY 1     AC0000-AC001F  POKEY 3
//! ```
//!
//! ## Byte writes on a word bus
//! The 68000 core turns a byte write into a read-modify-write of the containing
//! word, so the board sees a word read (the RMW read) followed by a word write
//! for every byte the game stores into a low-byte I/O register (POKEY, NVRAM,
//! `digital_w`). We take `data & 0xFF` on I/O writes and keep I/O reads
//! side-effect-light, so the stray RMW read is harmless. This is the documented
//! limitation in the m68000 README and the main thing to watch during bring-up.

use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    ActionRole, AnalogAxisKind, AudioSource, DefaultBinding, DipApplyTiming, DipChoice, DipOption,
    DipSwitchBank, DipSwitches, Direction, InputConfigurable, InputControl, InputEvent, InputId,
    InputKind, KeyId, MachineCore, MouseControl, Nvram, Profilable, Renderable, SaveState,
};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace32};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::m68000::M68000;
use phosphor_core::cpu::state::M68000State;
use phosphor_core::cpu::{Cpu, CpuStateTrait};
use phosphor_core::device::pokey::Pokey;
use phosphor_core::gfx::decode::{GfxCache, GfxLayout, decode_gfx};
use phosphor_core::gfx::{combine_weights, compute_resistor_weights};
use phosphor_macros::{BusDebug, MemoryRegion};

use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_low;

// ---------------------------------------------------------------------------
// Address-space regions (backed memory only; I/O is decoded in the Bus impl)
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
enum Region {
    Rom = 1,
    Ram = 2,
    SpriteRam = 3,
    Playfield = 4,
}

// ---------------------------------------------------------------------------
// ROM definitions ("foodf" parent, rev 3)
// ---------------------------------------------------------------------------

/// Program ROM: eight 8 KB chips, loaded back-to-back here and de-interleaved
/// into a 64 KB big-endian image in `load_rom_set` (each pair is an
/// even-byte/odd-byte `ROM_LOAD16_BYTE` chip).
///
/// Concatenation order in the loaded buffer:
///   [301][302][303][204][305][306][307][208]
/// Pairs (high-byte = even chip, low-byte = odd chip):
///   301/302 → 0x0000, 303/204 → 0x4000, 305/306 → 0x8000, 307/208 → 0xC000
pub static FOODF_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x10000,
    entries: &[
        RomEntry {
            name: "136020-301.8c",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xdfc3d5a8],
        },
        RomEntry {
            name: "136020-302.9c",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0xef92dc5c],
        },
        RomEntry {
            name: "136020-303.8d",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x64b93076],
        },
        RomEntry {
            name: "136020-204.9d",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0xea596480],
        },
        RomEntry {
            name: "136020-305.8e",
            size: 0x2000,
            offset: 0x8000,
            crc32: &[0xe6cff1b1],
        },
        RomEntry {
            name: "136020-306.9e",
            size: 0x2000,
            offset: 0xA000,
            crc32: &[0x95159a3e],
        },
        RomEntry {
            name: "136020-307.8f",
            size: 0x2000,
            offset: 0xC000,
            crc32: &[0x17828dbb],
        },
        RomEntry {
            name: "136020-208.9f",
            size: 0x2000,
            offset: 0xE000,
            crc32: &[0x608690c9],
        },
    ],
};

/// Playfield tile ROM: 8 KB, 8×8 2bpp, 512 tiles.
pub static FOODF_TILE_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[RomEntry {
        name: "136020-109.6lm",
        size: 0x2000,
        offset: 0x0000,
        crc32: &[0xc13c90eb],
    }],
};

/// Sprite ROM: two 8 KB halves (RGN_FRAC 1/2), 16×16 2bpp, 256 sprites.
pub static FOODF_SPRITE_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "136020-110.4e",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x8870e3d6],
        },
        RomEntry {
            name: "136020-111.4d",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x84372edf],
        },
    ],
};

/// Factory-default NVRAM contents (X2212), shipped in the MAME romset. Loaded
/// as the initial NVRAM so the game boots past its checksum check instead of
/// reporting a failure; a player's saved NVRAM file overrides this at startup.
pub static FOODF_NVRAM: RomRegion = RomRegion {
    size: 0x100,
    entries: &[RomEntry {
        name: "foodf.nv",
        size: 0x100,
        offset: 0x0000,
        crc32: &[0xa4186b13],
    }],
};

// ---------------------------------------------------------------------------
// Gfx layouts (translated from MAME charlayout / spritelayout)
// ---------------------------------------------------------------------------

/// 8×8 tiles, 2bpp, 512 tiles in 0x2000. MAME's charlayout planes are
/// `{ 0, 4 }`; `decode_gfx` orders `plane_offsets` LSB-first (entry 0 = pen
/// bit 0), the reverse of MAME's MSB-first `planeoffset`, so the list is
/// reversed to `{4, 0}`. (Getting this wrong swaps pen 1 ↔ pen 2.)
const FOODF_TILE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[4, 0],
    x_offsets: &[64, 65, 66, 67, 0, 1, 2, 3],
    y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
    char_increment: 128,
};

/// 16×16 sprites, 2bpp. RGN_FRAC(1,2) ⇒ one plane lives 0x10000 bits into the
/// 0x4000-byte region, the other at bit 0. MAME's spritelayout planes are
/// `{ RGN_FRAC(1,2), 0 } = { 0x10000, 0 }`; reversed here for `decode_gfx`'s
/// LSB-first convention (entry 0 = pen bit 0).
const FOODF_SPRITE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 0x10000],
    x_offsets: &[
        128, 129, 130, 131, 132, 133, 134, 135, 0, 1, 2, 3, 4, 5, 6, 7,
    ],
    y_offsets: &[
        0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104, 112, 120,
    ],
    char_increment: 256,
};

// ---------------------------------------------------------------------------
// Input IDs
// ---------------------------------------------------------------------------

pub const INPUT_COIN1: u8 = 0;
pub const INPUT_COIN2: u8 = 1;
pub const INPUT_START1: u8 = 2;
pub const INPUT_START2: u8 = 3;
pub const INPUT_SERVICE: u8 = 4;
pub const INPUT_P1_THROW: u8 = 5;
pub const INPUT_P2_THROW: u8 = 6;
pub const INPUT_P1_LEFT: u8 = 7;
pub const INPUT_P1_RIGHT: u8 = 8;
pub const INPUT_P1_UP: u8 = 9;
pub const INPUT_P1_DOWN: u8 = 10;
/// Self-test / operator switch (SYSTEM bit 7). Hold at boot — or toggle in
/// attract — to enter Food Fight's test menu, where the NVRAM/high-score reset
/// lives.
pub const INPUT_SELFTEST: u8 = 11;

pub const ANALOG_P1_X: u8 = 0;
pub const ANALOG_P1_Y: u8 = 1;
pub const ANALOG_P2_X: u8 = 2;
pub const ANALOG_P2_Y: u8 = 3;

// Typed control ids for the analog axes (distinct from the 0..=11 digital ids).
const CTRL_P1_STICK_X: InputId = InputId(12);
const CTRL_P1_STICK_Y: InputId = InputId(13);
const CTRL_P2_STICK_X: InputId = InputId(14);
const CTRL_P2_STICK_Y: InputId = InputId(15);

/// Typed logical controls. Default bindings mirror the legacy name-matched
/// defaults: the P1 stick axes map to the mouse; the P2 stick axes have no
/// default (the legacy frontend only bound the first two analog axes).
const FOODF_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_COIN1 as u16),
        stable_name: "coin1",
        label: "Coin",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(INPUT_COIN2 as u16),
        stable_name: "coin2",
        label: "Coin 2",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[],
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
        label: "Service",
        kind: InputKind::Service,
        player: None,
        default_bindings: crate::input_defaults::SERVICE,
    },
    InputControl {
        id: InputId(INPUT_P1_THROW as u16),
        stable_name: "p1_throw",
        label: "P1 Throw",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P2_THROW as u16),
        stable_name: "p2_throw",
        label: "P2 Throw",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(2),
        default_bindings: &[],
    },
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
        id: InputId(INPUT_SELFTEST as u16),
        stable_name: "self_test",
        label: "Self-Test",
        kind: InputKind::Service,
        player: None,
        default_bindings: &[DefaultBinding::Key(KeyId::T)],
    },
    InputControl {
        id: CTRL_P1_STICK_X,
        stable_name: "p1_stick_x",
        label: "P1 Stick X",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::X,
        },
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::AxisX)],
    },
    InputControl {
        id: CTRL_P1_STICK_Y,
        stable_name: "p1_stick_y",
        label: "P1 Stick Y",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::Y,
        },
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::AxisY)],
    },
    InputControl {
        id: CTRL_P2_STICK_X,
        stable_name: "p2_stick_x",
        label: "P2 Stick X",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::X,
        },
        player: Some(2),
        default_bindings: &[],
    },
    InputControl {
        id: CTRL_P2_STICK_Y,
        stable_name: "p2_stick_y",
        label: "P2 Stick Y",
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
// Master clock 12.096 MHz. CPU = pixel clock = master/2 = 6.048 MHz, so CPU
// cycles map 1:1 to pixel clocks: HTOTAL 384, VTOTAL 259 → 384 cycles/scanline,
// 259 scanlines/frame, ~60.8 Hz. Display 256×224 (visible h 0-256, v 0-224).
const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 6_048_000,
    cycles_per_scanline: 384,
    total_scanlines: 259,
    display_width: 256,
    display_height: 224,
    display_aspect: Some((4, 3)),
};

/// POKEY runs at master/20 = 604.8 kHz, i.e. one POKEY clock per 10 CPU cycles.
const POKEY_CLOCK_HZ: u32 = 604_800;
const CPU_PER_POKEY: u64 = 10;

const VBLANK_SCANLINE: u16 = 224;

const PLAYFIELD_ROWS: usize = 32;

// ---------------------------------------------------------------------------
// FoodFightSystem
// ---------------------------------------------------------------------------

/// Atari Food Fight arcade system.
#[derive(BusDebug)]
pub struct FoodFightSystem {
    #[debug_cpu("M68000")]
    cpu: M68000,
    #[debug_map(cpu = 0)]
    map: AddressSpace32,
    /// POKEY 1, 2, 3 (index 0 = chip "pokey1" at 0xA80000).
    #[debug_device("POKEY")]
    pokey: [Pokey; 3],

    // Graphics (not CPU-addressable)
    tile_cache: GfxCache,   // 512 × 8×8 × 2bpp
    sprite_cache: GfxCache, // 256 × 16×16 × 2bpp

    // Palette: 256 entries, low byte written by the CPU, pre-converted to RGB24.
    palette_ram: [u8; 256],
    palette_rgb: [(u8, u8, u8); 256],

    // NVRAM (X2212): 256 low-byte cells at 0x900000-9001FF.
    nvram: [u8; 256],

    // Analog stick ADC channels: [0]=P2 Y, [1]=P1 Y, [2]=P2 X, [3]=P1 X
    // (MAME adc in_callbacks); selected channel latched by writes to 0x944000.
    stick: [u8; 4],
    adc_channel: usize,
    // P1 digital direction state (keyboard play drives the ADC stick).
    p1_left: bool,
    p1_right: bool,
    p1_up: bool,
    p1_down: bool,

    // Digital SYSTEM port (active-low): see set_input.
    system_input: u8,
    dip_switches: u8,

    playfield_flip: bool,

    // Autovectored interrupt latches (held until acked via digital_w).
    scanline_int: bool, // IRQ1 (32V)
    video_int: bool,    // IRQ2 (VBLANK)

    clock: u64,
    watchdog_count: u8,

    audio_buffer: Vec<i16>,
}

impl FoodFightSystem {
    fn build_map() -> AddressSpace32 {
        let mut map = AddressSpace32::new();
        map.region(
            Region::Rom,
            "Program ROM",
            0x00_0000,
            0x1_0000,
            AccessKind::ReadOnly,
        )
        .region(
            Region::Ram,
            "Work RAM",
            0x01_4000,
            0x8000,
            AccessKind::ReadWrite,
        )
        .region(
            Region::SpriteRam,
            "Sprite RAM",
            0x01_C000,
            0x100,
            AccessKind::ReadWrite,
        )
        .region(
            Region::Playfield,
            "Playfield",
            0x80_0000,
            0x800,
            AccessKind::ReadWrite,
        );
        map
    }

    pub fn new() -> Self {
        let mut sys = Self {
            cpu: M68000::new(),
            map: Self::build_map(),
            pokey: [
                Pokey::with_clock(POKEY_CLOCK_HZ, 44100),
                Pokey::with_clock(POKEY_CLOCK_HZ, 44100),
                Pokey::with_clock(POKEY_CLOCK_HZ, 44100),
            ],
            tile_cache: GfxCache::new(512, 8, 8),
            sprite_cache: GfxCache::new(256, 16, 16),
            palette_ram: [0; 256],
            palette_rgb: [(0, 0, 0); 256],
            nvram: [0; 256],
            stick: [0x7F; 4],
            adc_channel: 0,
            p1_left: false,
            p1_right: false,
            p1_up: false,
            p1_down: false,
            system_input: 0xFF,
            dip_switches: 0x00,
            playfield_flip: false,
            scanline_int: false,
            video_int: false,
            clock: 0,
            watchdog_count: 0,
            audio_buffer: Vec::with_capacity(2048),
        };
        sys.refresh_dip_pots();
        sys
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        // Program ROM: load the eight chips back-to-back, then de-interleave the
        // even/odd pairs into a big-endian 64 KB image (even chip = high byte).
        let chips = FOODF_PROGRAM_ROM.load(rom_set)?;
        let mut image = vec![0u8; 0x1_0000];
        // (dst_base, odd_chip_offset, even_chip_offset)
        const PAIRS: [(usize, usize, usize); 4] = [
            (0x0000, 0x0000, 0x2000),
            (0x4000, 0x4000, 0x6000),
            (0x8000, 0x8000, 0xA000),
            (0xC000, 0xC000, 0xE000),
        ];
        for (dst, odd, even) in PAIRS {
            for i in 0..0x2000 {
                image[dst + 2 * i] = chips[even + i]; // even address = high byte
                image[dst + 2 * i + 1] = chips[odd + i]; // odd address = low byte
            }
        }
        self.map.load_region(Region::Rom, &image);

        let tiles = FOODF_TILE_ROM.load(rom_set)?;
        self.tile_cache = decode_gfx(&tiles, 0, 512, &FOODF_TILE_LAYOUT);

        let sprites = FOODF_SPRITE_ROM.load(rom_set)?;
        self.sprite_cache = decode_gfx(&sprites, 0, 256, &FOODF_SPRITE_LAYOUT);

        // Factory-default NVRAM (optional — the game reinitializes if absent).
        if let Ok(nv) = FOODF_NVRAM.load(rom_set) {
            let len = nv.len().min(self.nvram.len());
            self.nvram[..len].copy_from_slice(&nv[..len]);
        }

        Ok(())
    }

    pub fn get_cpu_state(&self) -> M68000State {
        self.cpu.snapshot()
    }

    pub fn clock(&self) -> u64 {
        self.clock
    }

    /// Feed the DIP switches to POKEY 1's pot inputs. The hardware wires each
    /// DIP bit to a POKEY pot line (`pot_r` returns `(dsw >> n) << 7`); the game
    /// reads the DIP byte back through POKEY POT0-7.
    fn refresh_dip_pots(&mut self) {
        for n in 0..8 {
            let level = if self.dip_switches & (1 << n) != 0 {
                0x80
            } else {
                0x00
            };
            self.pokey[0].set_pot_input(n, level);
        }
    }

    /// Digital outputs latch at 0x948001 (low byte).
    fn digital_w(&mut self, data: u8) {
        self.playfield_flip = data & 0x01 != 0;
        // bit 1 = NVRAM store (we persist NVRAM unconditionally — no-op here)
        if data & 0x04 == 0 {
            self.scanline_int = false; // INT1 ack (active-low)
        }
        if data & 0x08 == 0 {
            self.video_int = false; // INT2 ack (active-low)
        }
        // bits 4-5 LEDs, bits 6-7 coin counters — not modelled
    }

    /// Recompute one RGB24 palette entry. Low byte: R=bits0-2, G=bits3-5,
    /// B=bits6-7, through 1K/470/220Ω resistor weights (no pulldown).
    fn update_palette_entry(&mut self, idx: usize) {
        let rgb3 = compute_resistor_weights(&[1000.0, 470.0, 220.0], None);
        let b2 = compute_resistor_weights(&[470.0, 220.0], None);
        let d = self.palette_ram[idx];
        let r = combine_weights(&rgb3, &[d & 1, (d >> 1) & 1, (d >> 2) & 1]);
        let g = combine_weights(&rgb3, &[(d >> 3) & 1, (d >> 4) & 1, (d >> 5) & 1]);
        let b = combine_weights(&b2, &[(d >> 6) & 1, (d >> 7) & 1]);
        self.palette_rgb[idx] = (r, g, b);
    }

    /// Drive the P1 ADC stick from held direction keys (center = 0x7F).
    fn update_p1_stick(&mut self) {
        // stick[3] = P1 X, stick[1] = P1 Y. These store the raw stick position
        // (left/up = 0x00, right/down = 0xFF); the PORT_REVERSE mirroring is
        // applied when the ADC is read, so both axes end up flipped in-game.
        self.stick[3] = if self.p1_left {
            0x00
        } else if self.p1_right {
            0xFF
        } else {
            0x7F
        };
        self.stick[1] = if self.p1_up {
            0x00
        } else if self.p1_down {
            0xFF
        } else {
            0x7F
        };
    }

    /// Effective interrupt level from the latches (IRQ3 when both pending).
    fn interrupt_level(&self) -> u8 {
        match (self.scanline_int, self.video_int) {
            (true, true) => 3,
            (false, true) => 2,
            (true, false) => 1,
            (false, false) => 0,
        }
    }

    pub fn tick(&mut self) {
        let cycles_per_frame = TIMING.cycles_per_frame();
        let frame_cycle = self.clock % cycles_per_frame;

        // Raster-timed interrupts, asserted at scanline boundaries.
        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let scanline = (frame_cycle / TIMING.cycles_per_scanline) as u16;
            // IRQ1 fires on 32V (MAME's scanline_update_timer: 0/64/128/192).
            if matches!(scanline, 0 | 64 | 128 | 192) {
                self.scanline_int = true;
            }
            if scanline == VBLANK_SCANLINE {
                self.video_int = true;
            }
        }

        // POKEY runs at master/20 = one tick per 10 CPU cycles.
        if self.clock.is_multiple_of(CPU_PER_POKEY) {
            for p in &mut self.pokey {
                p.tick();
            }
        }

        // Latch watchpoint attribution context before CPU execution.
        if self.map.has_any_watchpoints() {
            let pc = self.cpu.at_instruction_boundary().then_some(self.cpu.pc);
            self.map.latch_access_context(self.clock, pc);
        }

        bus_split!(self, bus: u32 word => {
            self.cpu.execute_cycle(bus, BusMaster::Cpu(0));
        });

        self.clock += 1;
    }

    // -----------------------------------------------------------------------
    // Rendering (full-frame compositor, read from current state)
    // -----------------------------------------------------------------------

    fn render(&self, buffer: &mut [u8]) {
        let w = TIMING.display_width as usize;
        let h = TIMING.display_height as usize;
        // Playfield pen value (0-3) per screen pixel — used for sprite priority.
        let mut pf_pen = vec![0u8; w * h];

        let playfield = self.map.region_data(Region::Playfield);
        let flip = self.playfield_flip;

        for sy in 0..h {
            for sx in 0..w {
                // Apply screen flip (cocktail) by mirroring source coords.
                let dx = if flip { w - 1 - sx } else { sx };
                let dy = if flip { h - 1 - sy } else { sy };

                // Playfield scrolls left by 8 pixels (set_scrollx(-8)).
                let src_x = (dx as i32 - 8).rem_euclid(256) as usize;
                let src_y = dy; // no vertical scroll

                let tile_col = src_x / 8;
                let tile_row = src_y / 8;
                let mut px = src_x % 8;
                let mut py = src_y % 8;
                if flip {
                    px = 7 - px;
                    py = 7 - py;
                }

                // Column-scan tilemap order: index = col * rows + row.
                let mem = tile_col * PLAYFIELD_ROWS + tile_row;
                let word = if mem * 2 + 1 < playfield.len() {
                    u16::from_be_bytes([playfield[mem * 2], playfield[mem * 2 + 1]])
                } else {
                    0
                };
                let code = ((word & 0xFF) | ((word >> 7) & 0x100)) as usize;
                let color = ((word >> 8) & 0x3F) as usize;
                let pen = self.tile_cache.pixel(code & 0x1FF, px, py);
                pf_pen[sy * w + sx] = pen;

                let pal = (color * 4 + pen as usize) & 0xFF;
                let (r, g, b) = self.palette_rgb[pal];
                let o = (sy * w + sx) * 3;
                buffer[o] = r;
                buffer[o + 1] = g;
                buffer[o + 2] = b;
            }
        }

        // Sprites: MAME draws motion-object words 0x20..0x80 front-to-back
        // (offs 0x7E down to 0x20) with `prio_transpen`, whose priority bitmap
        // makes the *first* opaque pixel written win — a later (lower-offset)
        // sprite cannot overwrite a pixel an earlier (higher-offset) one already
        // claimed. So higher offsets sit on top. We replicate that with a
        // per-pixel "claimed" mask rather than plain painter's overwrite, which
        // would otherwise invert the layering of overlapping objects (e.g. it
        // drew Charley's blue head over his yellow hair).
        let mut claimed = vec![false; w * h];
        let sprites = self.map.region_data(Region::SpriteRam);
        let read_word = |i: usize| -> u16 {
            if i * 2 + 1 < sprites.len() {
                u16::from_be_bytes([sprites[i * 2], sprites[i * 2 + 1]])
            } else {
                0
            }
        };
        for offs in (0x20..0x80).step_by(2).rev() {
            let data1 = read_word(offs);
            let data2 = read_word(offs + 1);
            let pict = (data1 & 0xFF) as usize;
            let color = ((data1 >> 8) & 0x1F) as usize;
            let xpos = ((data2 >> 8) & 0xFF) as usize;
            let ypos = ((0xFFu16.wrapping_sub(data2).wrapping_sub(16)) & 0xFF) as usize;
            let hflip = (data1 >> 15) & 1 != 0;
            let vflip = (data1 >> 14) & 1 != 0;
            let pri = (data1 >> 13) & 1 != 0;

            for row in 0..16usize {
                for col in 0..16usize {
                    let sc = if hflip { 15 - col } else { col };
                    let sr = if vflip { 15 - row } else { row };
                    let pen = self.sprite_cache.pixel(pict, sc, sr);
                    if pen == 0 {
                        continue; // transparent
                    }
                    let mut dx = (xpos + col) & 0xFF;
                    let mut dy = (ypos + row) & 0xFF;
                    if flip {
                        dx = 255 - dx;
                        dy = 255 - dy;
                    }
                    if dx >= w || dy >= h {
                        continue;
                    }
                    let i = dy * w + dx;
                    // First opaque sprite pixel claims the location (MAME's
                    // prio_transpen first-wins); later sprites cannot overwrite.
                    if claimed[i] {
                        continue;
                    }
                    claimed[i] = true;
                    // Priority (MAME prio_transpen, pmask = pri*2): pri=0 always
                    // draws over the playfield; pri=1 is hidden behind non-
                    // transparent playfield pixels (pen != 0). A blocked pri=1
                    // pixel still claims the location, matching MAME.
                    if pri && pf_pen[i] != 0 {
                        continue;
                    }
                    let pal = (color * 4 + pen as usize) & 0xFF;
                    let (r, g, b) = self.palette_rgb[pal];
                    let o = i * 3;
                    buffer[o] = r;
                    buffer[o + 1] = g;
                    buffer[o + 2] = b;
                }
            }
        }
    }
}

impl Default for FoodFightSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus
// ---------------------------------------------------------------------------

impl Bus for FoodFightSystem {
    type Address = u32;
    type Data = u16;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn read(&mut self, master: BusMaster, addr: u32) -> u16 {
        let val = match addr {
            0x00_0000..=0x00_FFFF
            | 0x01_4000..=0x01_BFFF
            | 0x01_C000..=0x01_C0FF
            | 0x80_0000..=0x80_07FF => self.map.read_bus_word_be(addr),
            0x90_0000..=0x90_01FF => self.nvram[((addr >> 1) & 0xFF) as usize] as u16,
            // ADC data (0x940001). The real sticks read reversed (MAME applies
            // PORT_REVERSE on both axes), so mirror the value here — this flips
            // both the digital-direction and analog-mouse input paths at once.
            0x94_0000..=0x94_01FF => 0xFF - self.stick[self.adc_channel] as u16,
            0x94_8000..=0x94_81FF => self.system_input as u16, // SYSTEM
            0x95_8000..=0x95_81FF => {
                self.watchdog_count = 0; // watchdog also resets on read
                0xFFFF
            }
            0xA4_0000..=0xA4_001F => self.pokey[1].read(((addr >> 1) & 0x0F) as u16) as u16,
            0xA8_0000..=0xA8_001F => self.pokey[0].read(((addr >> 1) & 0x0F) as u16) as u16,
            0xAC_0000..=0xAC_001F => self.pokey[2].read(((addr >> 1) & 0x0F) as u16) as u16,
            _ => 0xFFFF,
        };
        self.map.watch_read(0, master, addr, val as u32, 2);
        val
    }

    fn write(&mut self, master: BusMaster, addr: u32, data: u16) {
        self.map.watch_write(0, master, addr, data as u32, 2);
        let byte = (data & 0xFF) as u8;
        match addr {
            0x00_0000..=0x00_FFFF => {} // ROM, ignore
            0x01_4000..=0x01_BFFF | 0x01_C000..=0x01_C0FF | 0x80_0000..=0x80_07FF => {
                self.map.write_bus_word_be(addr, data);
            }
            0x90_0000..=0x90_01FF => self.nvram[((addr >> 1) & 0xFF) as usize] = byte,
            0x94_4000..=0x94_4007 => self.adc_channel = ((addr >> 1) & 0x07) as usize,
            0x94_8000..=0x94_81FF => self.digital_w(byte),
            0x95_0000..=0x95_01FF => {
                let idx = ((addr >> 1) & 0xFF) as usize;
                self.palette_ram[idx] = byte;
                self.update_palette_entry(idx);
            }
            0x95_4000..=0x95_41FF => {} // NVRAM recall — no-op (persistent)
            0x95_8000..=0x95_81FF => self.watchdog_count = 0,
            0xA4_0000..=0xA4_001F => self.pokey[1].write(((addr >> 1) & 0x0F) as u16, byte),
            0xA8_0000..=0xA8_001F => self.pokey[0].write(((addr >> 1) & 0x0F) as u16, byte),
            0xAC_0000..=0xAC_001F => self.pokey[2].write(((addr >> 1) & 0x0F) as u16, byte),
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

impl Renderable for FoodFightSystem {
    fn display_size(&self) -> (u32, u32) {
        TIMING.display_size()
    }

    fn display_aspect(&self) -> Option<(u32, u32)> {
        TIMING.display_aspect()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        self.render(buffer);
    }
}

impl AudioSource for FoodFightSystem {
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

impl InputConfigurable for FoodFightSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        FOODF_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Button { id, pressed } => match id.0 as u8 {
                INPUT_COIN1 => set_bit_active_low(&mut self.system_input, 0, pressed),
                INPUT_COIN2 => set_bit_active_low(&mut self.system_input, 1, pressed),
                INPUT_START1 => set_bit_active_low(&mut self.system_input, 2, pressed),
                INPUT_START2 => set_bit_active_low(&mut self.system_input, 3, pressed),
                INPUT_SERVICE => set_bit_active_low(&mut self.system_input, 4, pressed),
                INPUT_SELFTEST => set_bit_active_low(&mut self.system_input, 7, pressed),
                INPUT_P1_THROW => set_bit_active_low(&mut self.system_input, 5, pressed),
                INPUT_P2_THROW => set_bit_active_low(&mut self.system_input, 6, pressed),
                INPUT_P1_LEFT => {
                    self.p1_left = pressed;
                    self.update_p1_stick();
                }
                INPUT_P1_RIGHT => {
                    self.p1_right = pressed;
                    self.update_p1_stick();
                }
                INPUT_P1_UP => {
                    self.p1_up = pressed;
                    self.update_p1_stick();
                }
                INPUT_P1_DOWN => {
                    self.p1_down = pressed;
                    self.update_p1_stick();
                }
                _ => {}
            },
            InputEvent::Relative { id, delta } => {
                let delta = delta as i32;
                let apply = |v: &mut u8, d: i32| *v = (*v as i32 + d).clamp(0, 255) as u8;
                if id == CTRL_P1_STICK_X {
                    apply(&mut self.stick[3], delta);
                } else if id == CTRL_P1_STICK_Y {
                    apply(&mut self.stick[1], delta);
                } else if id == CTRL_P2_STICK_X {
                    apply(&mut self.stick[2], delta);
                } else if id == CTRL_P2_STICK_Y {
                    apply(&mut self.stick[0], delta);
                }
            }
            InputEvent::Absolute { .. } => {}
        }
    }
}

impl MachineCore for FoodFightSystem {
    crate::machine_core_metadata!("foodf", TIMING);

    fn gfx_sheets(&self) -> Vec<phosphor_core::core::machine::GfxSheet<'_>> {
        use phosphor_core::core::machine::GfxSheet;
        vec![
            GfxSheet {
                name: "tiles",
                cache: &self.tile_cache,
                palette: &self.palette_rgb,
            },
            GfxSheet {
                name: "sprites",
                cache: &self.sprite_cache,
                palette: &self.palette_rgb,
            },
        ]
    }

    fn run_frame(&mut self) {
        for _ in 0..TIMING.cycles_per_frame() {
            self.tick();
        }

        // Watchdog: Food Fight uses an 8-VBLANK timeout. The game strobes the
        // watchdog reset register each frame; if it stops, reboot.
        self.watchdog_count = self.watchdog_count.saturating_add(1);
        if self.watchdog_count >= 8 {
            self.reset();
        }

        // Mix the three POKEYs to mono.
        let s0 = self.pokey[0].drain_audio();
        let s1 = self.pokey[1].drain_audio();
        let s2 = self.pokey[2].drain_audio();
        let len = s0.len().min(s1.len()).min(s2.len());
        self.audio_buffer.extend((0..len).map(|i| {
            let mixed = (s0[i] + s1[i] + s2[i]) / 3.0; // [0, 1]
            ((mixed - 0.5) * 2.0 * 32767.0) as i16
        }));
    }

    fn reset(&mut self) {
        self.scanline_int = false;
        self.video_int = false;
        self.watchdog_count = 0;
        self.playfield_flip = false;
        self.adc_channel = 0;
        self.system_input = 0xFF;
        self.audio_buffer.clear();
        for p in &mut self.pokey {
            p.reset();
        }
        self.refresh_dip_pots();

        bus_split!(self, bus: u32 word => {
            self.cpu.reset(bus, BusMaster::Cpu(0));
        });
    }
}

// `MachineDebug` (debug_bus + cycle stepping) via the standalone-debug macro;
// `BusDebug` is `#[derive]`d on the struct above (24-bit `AddressSpace32` bus).
crate::impl_standalone_debug!(FoodFightSystem);

impl Saveable for FoodFightSystem {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        for p in &self.pokey {
            p.save_state(w);
        }
        w.write_bytes(self.map.region_data(Region::Ram));
        w.write_bytes(self.map.region_data(Region::SpriteRam));
        w.write_bytes(self.map.region_data(Region::Playfield));
        w.write_bytes(&self.palette_ram);
        w.write_bytes(&self.nvram);
        w.write_bytes(&self.stick);
        w.write_u8(self.adc_channel as u8);
        w.write_u8(self.system_input);
        w.write_u8(self.dip_switches);
        w.write_bool(self.playfield_flip);
        w.write_bool(self.scanline_int);
        w.write_bool(self.video_int);
        w.write_u64_le(self.clock);
        w.write_u8(self.watchdog_count);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        for p in &mut self.pokey {
            p.load_state(r)?;
        }
        r.read_bytes_into(self.map.region_data_mut(Region::Ram))?;
        r.read_bytes_into(self.map.region_data_mut(Region::SpriteRam))?;
        r.read_bytes_into(self.map.region_data_mut(Region::Playfield))?;
        r.read_bytes_into(&mut self.palette_ram)?;
        r.read_bytes_into(&mut self.nvram)?;
        r.read_bytes_into(&mut self.stick)?;
        self.adc_channel = (r.read_u8()? & 0x07) as usize;
        self.system_input = r.read_u8()?;
        self.dip_switches = r.read_u8()?;
        self.playfield_flip = r.read_bool()?;
        self.scanline_int = r.read_bool()?;
        self.video_int = r.read_bool()?;
        self.clock = r.read_u64_le()?;
        self.watchdog_count = r.read_u8()?;
        // Recompute derived state.
        for i in 0..256 {
            self.update_palette_entry(i);
        }
        self.refresh_dip_pots();
        self.audio_buffer.clear();
        Ok(())
    }
}

impl SaveState for FoodFightSystem {
    crate::machine_save_state!();
}

impl Nvram for FoodFightSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(&self.nvram)
    }

    fn load_nvram(&mut self, data: &[u8]) {
        let len = data.len().min(self.nvram.len());
        self.nvram[..len].copy_from_slice(&data[..len]);
    }
}

impl Profilable for FoodFightSystem {}
/// DIP switch metadata for Food Fight's single DIP byte (read back bit-by-bit
/// through POKEY pot lines POT0-7, so bit n of `dip_switches` is DIP switch n).
/// Choice bits and labels follow MAME's `foodf` layout; option defaults OR to
/// the historical 0x00.
const FOODF_DIP_BANKS: &[DipSwitchBank] = &[DipSwitchBank {
    name: "DSW",
    options: &[
        DipOption {
            name: "Bonus Coins",
            mask: 0x07,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "None",
                    value: 0x00,
                },
                DipChoice {
                    label: "1 for every 5",
                    value: 0x01,
                },
                DipChoice {
                    label: "1 for every 4",
                    value: 0x02,
                },
                DipChoice {
                    label: "1 for every 2",
                    value: 0x05,
                },
                DipChoice {
                    label: "2 for every 4",
                    value: 0x06,
                },
            ],
        },
        DipOption {
            name: "Coin A",
            mask: 0x08,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "1 Coin/1 Credit",
                    value: 0x00,
                },
                DipChoice {
                    label: "1 Coin/2 Credits",
                    value: 0x08,
                },
            ],
        },
        DipOption {
            name: "Coin B",
            mask: 0x30,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "1 Coin/1 Credit",
                    value: 0x00,
                },
                DipChoice {
                    label: "1 Coin/5 Credits",
                    value: 0x10,
                },
                DipChoice {
                    label: "1 Coin/4 Credits",
                    value: 0x20,
                },
                DipChoice {
                    label: "1 Coin/6 Credits",
                    value: 0x30,
                },
            ],
        },
        DipOption {
            name: "Coinage",
            mask: 0xC0,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "1 Coin/1 Credit",
                    value: 0x00,
                },
                DipChoice {
                    label: "Free Play",
                    value: 0x40,
                },
                DipChoice {
                    label: "2 Coins/1 Credit",
                    value: 0x80,
                },
                DipChoice {
                    label: "1 Coin/2 Credits",
                    value: 0xC0,
                },
            ],
        },
    ],
}];

impl DipSwitches for FoodFightSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        FOODF_DIP_BANKS
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        if bank == 0 { self.dip_switches } else { 0 }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        if bank == 0 {
            self.dip_switches = value;
        }
    }
}
impl phosphor_core::core::debug_trace::DebugTrace for FoodFightSystem {}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = FoodFightSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("foodf", &["foodf"], create_machine)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dip_default_and_metadata() {
        let sys = FoodFightSystem::new();
        assert_eq!(sys.dip_bank_value(0), 0x00);
        crate::assert_dip_banks_valid(sys.dip_banks(), &[sys.dip_bank_value(0)]);
    }

    #[test]
    fn set_dip_option_masks_only_its_bits() {
        let mut sys = FoodFightSystem::new();
        // Coinage is option 3 (mask 0xC0); pick "Free Play" (0x40).
        sys.set_dip_option(0, 3, 0x40);
        assert_eq!(sys.dip_bank_value(0), 0x40);
    }

    #[test]
    fn map_decodes_documented_windows() {
        let sys = FoodFightSystem::new();
        assert_eq!(sys.map.region_at(0x00_0000).unwrap().id, Region::Rom.into());
        assert_eq!(sys.map.region_at(0x01_4000).unwrap().id, Region::Ram.into());
        assert_eq!(
            sys.map.region_at(0x01_C000).unwrap().id,
            Region::SpriteRam.into()
        );
        assert_eq!(
            sys.map.region_at(0x80_0000).unwrap().id,
            Region::Playfield.into()
        );
    }

    #[test]
    fn reset_loads_ssp_and_pc_from_vectors() {
        let mut sys = FoodFightSystem::new();
        // Vector 0 (SSP) = 0x00010000, vector 1 (PC) = 0x00000400.
        let rom = sys.map.region_data_mut(Region::Rom);
        rom[0..8].copy_from_slice(&[0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00]);
        sys.reset();
        let st = sys.get_cpu_state();
        assert_eq!(st.a[7], 0x0001_0000);
        assert_eq!(st.pc, 0x0000_0400);
    }

    #[test]
    fn interrupt_level_encodes_and_acks() {
        let mut sys = FoodFightSystem::new();
        assert_eq!(sys.interrupt_level(), 0);
        sys.scanline_int = true;
        assert_eq!(sys.interrupt_level(), 1);
        sys.video_int = true;
        assert_eq!(sys.interrupt_level(), 3);
        sys.scanline_int = false;
        assert_eq!(sys.interrupt_level(), 2);

        // digital_w acks are active-low: bit 2 clears INT1, bit 3 clears INT2.
        sys.scanline_int = true;
        sys.video_int = true;
        sys.digital_w(0x00); // both ack bits low
        assert!(!sys.scanline_int);
        assert!(!sys.video_int);

        let st = sys.check_interrupts(BusMaster::Cpu(0));
        assert_eq!(st.irq_level, 0);
        assert_eq!(st.irq_vector, 0xFF);
    }

    #[test]
    fn nvram_low_byte_round_trips() {
        let mut sys = FoodFightSystem::new();
        // Byte write lands in the low byte of the NVRAM cell.
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x90_0000, 0x1234);
        assert_eq!(sys.nvram[0], 0x34);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x90_0000), 0x0034);
    }

    #[test]
    fn ram_word_access_round_trips() {
        let mut sys = FoodFightSystem::new();
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x01_4000, 0xBEEF);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x01_4000), 0xBEEF);
    }

    #[test]
    fn palette_write_updates_rgb() {
        let mut sys = FoodFightSystem::new();
        // All bits set in the low byte → white-ish (max R/G/B).
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x95_0000, 0x00FF);
        assert_eq!(sys.palette_rgb[0], (255, 255, 255));
        // All bits clear → black.
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x95_0002, 0x0000);
        assert_eq!(sys.palette_rgb[1], (0, 0, 0));
    }

    /// Boot a hand-assembled 68000 program on the full board and prove the core
    /// runs it, services autovectored interrupts, and stores into RAM — the
    /// end-to-end exercise the m68000 core has never had inside a machine.
    #[test]
    fn synthetic_program_boots_and_takes_interrupts() {
        let mut sys = FoodFightSystem::new();
        {
            let rom = sys.map.region_data_mut(Region::Rom);

            // Reset vectors: SSP = 0x00018000, PC = 0x00000400.
            rom[0x00..0x08].copy_from_slice(&[0x00, 0x01, 0x80, 0x00, 0x00, 0x00, 0x04, 0x00]);
            // Autovector handlers for IRQ levels 1/2/3 (vectors 25/26/27) → 0x500.
            for v in [25usize, 26, 27] {
                rom[v * 4..v * 4 + 4].copy_from_slice(&[0x00, 0x00, 0x05, 0x00]);
            }

            // Main program @ 0x400:
            //   move  #$2000, sr        ; supervisor, interrupt mask = 0
            //   loop: addq.l #1, $14000 ; bump the "alive" counter
            //         bra.s loop
            let main: &[u8] = &[
                0x46, 0xFC, 0x20, 0x00, // move #$2000, sr
                0x52, 0xB9, 0x00, 0x01, 0x40, 0x00, // addq.l #1, $00014000
                0x60, 0xF8, // bra.s loop
            ];
            rom[0x400..0x400 + main.len()].copy_from_slice(main);

            // IRQ handler @ 0x500:
            //   addq.l #1, $14010       ; bump the interrupt counter
            //   move.b #0, $948001      ; ack INT1/INT2 (digital_w bits 2,3 low)
            //   rte
            let handler: &[u8] = &[
                0x52, 0xB9, 0x00, 0x01, 0x40, 0x10, // addq.l #1, $00014010
                0x13, 0xFC, 0x00, 0x00, 0x00, 0x94, 0x80, 0x01, // move.b #0, $00948001
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

        // CPU is executing inside the ROM main loop.
        let pc = sys.get_cpu_state().pc;
        assert!(pc < 0x1_0000, "PC {pc:#08X} escaped ROM");

        // The "alive" counter advanced → the core is actually running code.
        let ram = sys.map.region_data(Region::Ram);
        let alive = u32::from_be_bytes([ram[0], ram[1], ram[2], ram[3]]);
        assert!(alive > 0, "main loop never ran");

        // The interrupt counter advanced → autovectored IRQs were taken and RTE
        // returned cleanly (otherwise the handler would not have re-entered).
        let taken = u32::from_be_bytes([ram[0x10], ram[0x11], ram[0x12], ram[0x13]]);
        assert!(taken > 0, "no interrupts were serviced");
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = FoodFightSystem::new();
        sys.map.region_data_mut(Region::Ram)[0x100] = 0xAB;
        sys.map.region_data_mut(Region::Playfield)[0x10] = 0xCD;
        sys.nvram[0x20] = 0x42;
        sys.palette_ram[5] = 0x55;
        sys.system_input = 0xF0;
        sys.scanline_int = true;
        sys.clock = 12_345;
        sys.watchdog_count = 3;

        let data = SaveState::save_state(&sys).expect("save");
        let cpu_snap = sys.get_cpu_state();

        let mut sys2 = FoodFightSystem::new();
        SaveState::load_state(&mut sys2, &data).unwrap();

        assert_eq!(sys2.get_cpu_state(), cpu_snap);
        assert_eq!(sys2.map.region_data(Region::Ram)[0x100], 0xAB);
        assert_eq!(sys2.map.region_data(Region::Playfield)[0x10], 0xCD);
        assert_eq!(sys2.nvram[0x20], 0x42);
        assert_eq!(sys2.palette_ram[5], 0x55);
        assert_eq!(sys2.system_input, 0xF0);
        assert!(sys2.scanline_int);
        assert_eq!(sys2.clock, 12_345);
        assert_eq!(sys2.watchdog_count, 3);
    }
}
