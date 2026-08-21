use phosphor_core::audio::{DcBlocker, SampleRing};
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::input::{DrainPolicy, RelativeCounter};
use phosphor_core::core::machine::{
    AnalogAxisKind, AudioSource, DefaultBinding, DipApplyTiming, DipChoice, DipOption,
    DipSwitchBank, Direction, InputConfigurable, InputControl, InputEvent, InputId, InputKind,
    KeyId, MachineCore, MouseControl, Nvram, PadButton, PadControl, Profilable, Renderable,
    SaveState,
};
use phosphor_core::core::save_state::{self, SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::cpu::state::M6502State;
use phosphor_core::cpu::{Cpu, CpuStateTrait};
use phosphor_core::device::output_latch::OutputLatch;
use phosphor_core::device::pokey::Pokey;
use phosphor_core::gfx::decode::{GfxCache, GfxLayout, decode_gfx};
use phosphor_macros::{BusDebug, MemoryRegion};

use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_low;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
enum Region {
    VideoRam = 1,
    Sram = 2,
    SpriteRam = 3,
    Nvram = 4,
    Io = 5,
    RomBank0 = 6,
    RomBank1 = 7,
    RomFixed = 8,
}

// ---------------------------------------------------------------------------
// Crystal Castles ROM definitions
// ---------------------------------------------------------------------------
// Layout in our 40KB rom[] array:
//   [0x0000..0x2000] = bank 0 low  (0xA000-0xBFFF, version-specific)
//   [0x2000..0x4000] = bank 0 high (0xC000-0xDFFF, version-specific)
//   [0x4000..0x6000] = fixed ROM   (0xE000-0xFFFF, version-specific)
//   [0x6000..0x8000] = bank 1 low  (0xA000-0xBFFF, 136022-102, common)
//   [0x8000..0xA000] = bank 1 high (0xC000-0xDFFF, 136022-101, common)
//
// MAME bank config: configure_entries(0, 2, base + 0xa000, 0x6000)
//   Bank 0 reads rom[0x0000..0x4000], Bank 1 reads rom[0x6000..0xA000].

/// Program ROM: 40KB across 5 chips (3 version-specific + 2 common).
/// Supports all 8 MAME variants (v1-v4, German, Spanish, French, Joystick).
pub static CCASTLES_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0xA000, // 40KB
    entries: &[
        // Bank 0 low (8KB at 0xA000-0xBFFF)
        RomEntry {
            name: "136022-403.1k",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[
                0x81471ae5, // v4 (parent)
                0x10e39fce, // v3 / v3 German / v3 Spanish / v3 French
                0x348a96f0, // v2
                0x9d10e314, // v1
                0x0d911ef4, // joystick
            ],
        },
        // Bank 0 high (8KB at 0xC000-0xDFFF)
        RomEntry {
            name: "136022-404.1l",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[
                0x820daf29, // v4 (parent)
                0x74510f72, // v3 / v3 German / v3 Spanish / v3 French
                0xd48d8c1f, // v2
                0xfe2647a4, // v1
                0x246079de, // joystick
            ],
        },
        // Fixed ROM (8KB at 0xE000-0xFFFF)
        RomEntry {
            name: "136022-405.1n",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[
                0x4befc296, // v4 (parent)
                0x9418cf8a, // v3
                0x69b8d906, // v3 German
                0xb833936e, // v3 Spanish
                0x8585b4d1, // v3 French
                0x0e4883cc, // v2
                0x5a13af07, // v1
                0x3beec4f3, // joystick
            ],
        },
        // Bank 1 low (8KB, 136022-102, common to all variants)
        RomEntry {
            name: "136022-102.1h",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0xf6ccfbd4],
        },
        // Bank 1 high (8KB, 136022-101, common to all variants)
        RomEntry {
            name: "136022-101.1f",
            size: 0x2000,
            offset: 0x8000,
            crc32: &[0xe2e17236],
        },
    ],
};

// Disassemblable code regions for the standalone `disasm` tool. The M6502
// program ROM is bank-switched: banks 0 and 1 both map to 0xA000-0xDFFF, so
// each is registered separately (region-per-bank) with the same org but a
// loader that slices its 16KB out of the flat 40KB image. The fixed ROM at
// 0xE000-0xFFFF (which holds the reset/IRQ vectors) is always mapped.
inventory::submit! {
    DisasmRegion {
        machine: "ccastles",
        region: "bank0",
        cpu: DisasmCpu::M6502,
        org: 0xA000,
        size: 0x4000,
        load: |rs| Ok(CCASTLES_PROGRAM_ROM.load(rs)?[0x0000..0x4000].to_vec()),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "ccastles",
        region: "bank1",
        cpu: DisasmCpu::M6502,
        org: 0xA000,
        size: 0x4000,
        load: |rs| Ok(CCASTLES_PROGRAM_ROM.load(rs)?[0x6000..0xA000].to_vec()),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "ccastles",
        region: "fixed",
        cpu: DisasmCpu::M6502,
        org: 0xE000,
        size: 0x2000,
        load: |rs| Ok(CCASTLES_PROGRAM_ROM.load(rs)?[0x4000..0x6000].to_vec()),
    }
}

/// Sprite graphics ROM: 16KB (two 8KB chips, 3bpp sprites 8x16 pixels).
pub static CCASTLES_GFX_ROM: RomRegion = RomRegion {
    size: 0x4000, // 16KB
    entries: &[
        RomEntry {
            name: "136022-106.8d",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x9d1d89fc],
        },
        RomEntry {
            name: "136022-107.8b",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x39960b7d],
        },
    ],
};

/// Sync PROM: 256 bytes — VBLANK and IRQ timing (one entry per scanline).
/// Bit 0 = VBLANK, Bit 3 = IRQCK (rising edge triggers IRQ).
pub static CCASTLES_SYNC_PROM: RomRegion = RomRegion {
    size: 0x100,
    entries: &[RomEntry {
        name: "82s129-136022-108.7k",
        size: 0x100,
        offset: 0x0000,
        crc32: &[0x6ed31e3b],
    }],
};

/// Write-protect PROM: 256 bytes — controls which VRAM nibbles can be written.
pub static CCASTLES_WP_PROM: RomRegion = RomRegion {
    size: 0x100,
    entries: &[RomEntry {
        name: "82s129-136022-110.11l",
        size: 0x100,
        offset: 0x0000,
        crc32: &[0x068bdc7e],
    }],
};

/// Priority PROM: 256 bytes — sprite/bitmap compositing priority.
pub static CCASTLES_PRI_PROM: RomRegion = RomRegion {
    size: 0x100,
    entries: &[RomEntry {
        name: "82s129-136022-111.10k",
        size: 0x100,
        offset: 0x0000,
        crc32: &[0xc29c18d9],
    }],
};

// ---------------------------------------------------------------------------
// Input button IDs
// ---------------------------------------------------------------------------
pub const INPUT_COIN_L: u8 = 0;
pub const INPUT_COIN_R: u8 = 1;
pub const INPUT_JUMP_LEFT: u8 = 2; // also P1 Start in upright mode
pub const INPUT_JUMP_RIGHT: u8 = 3; // also P2 Start in upright mode
pub const INPUT_TRACK_L: u8 = 4;
pub const INPUT_TRACK_R: u8 = 5;
pub const INPUT_TRACK_U: u8 = 6;
pub const INPUT_TRACK_D: u8 = 7;

// ---------------------------------------------------------------------------
// Analog axis IDs (trackball)
// ---------------------------------------------------------------------------
pub const ANALOG_TRACKBALL_X: u8 = 0;
pub const ANALOG_TRACKBALL_Y: u8 = 1;

// Typed control ids for the analog axes (distinct from the 0..=7 digital ids).
const CTRL_TRACKBALL_X: InputId = InputId(8);
const CTRL_TRACKBALL_Y: InputId = InputId(9);

/// Typed logical controls. `InputId`s reuse the `INPUT_*` numbering for digital
/// controls. Default bindings mirror the legacy name-matched defaults — the
/// combo jump buttons keep their dual key bindings (start + action key), the
/// gamepad Start / mouse-button bindings, and the trackball maps to the mouse
/// (Y inverted in `handle_input`).
const CCASTLES_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_COIN_L as u16),
        stable_name: "coin",
        label: "Coin",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(INPUT_COIN_R as u16),
        stable_name: "coin2",
        label: "Coin 2",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_JUMP_LEFT as u16),
        stable_name: "jump_left",
        label: "Jump L / P1 Start",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: &[
            DefaultBinding::Key(KeyId::Num1),
            DefaultBinding::Key(KeyId::Space),
            DefaultBinding::Pad(PadControl::Button(PadButton::Start)),
            DefaultBinding::Mouse(MouseControl::Left),
        ],
    },
    InputControl {
        id: InputId(INPUT_JUMP_RIGHT as u16),
        stable_name: "jump_right",
        label: "Jump R / P2 Start",
        kind: InputKind::Button,
        player: Some(2),
        default_bindings: &[
            DefaultBinding::Key(KeyId::Num2),
            DefaultBinding::Key(KeyId::LShift),
            DefaultBinding::Mouse(MouseControl::Right),
        ],
    },
    InputControl {
        id: InputId(INPUT_TRACK_L as u16),
        stable_name: "p1_left",
        label: "P1 Left",
        kind: InputKind::DigitalDirection {
            direction: Direction::Left,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(INPUT_TRACK_R as u16),
        stable_name: "p1_right",
        label: "P1 Right",
        kind: InputKind::DigitalDirection {
            direction: Direction::Right,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
    InputControl {
        id: InputId(INPUT_TRACK_U as u16),
        stable_name: "p1_up",
        label: "P1 Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_UP,
    },
    InputControl {
        id: InputId(INPUT_TRACK_D as u16),
        stable_name: "p1_down",
        label: "P1 Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_DOWN,
    },
    InputControl {
        id: CTRL_TRACKBALL_X,
        stable_name: "trackball_x",
        label: "Trackball X",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::X,
        },
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::AxisX)],
    },
    InputControl {
        id: CTRL_TRACKBALL_Y,
        stable_name: "trackball_y",
        label: "Trackball Y",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::Y,
        },
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::AxisY)],
    },
];

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------
// Master clock: 10 MHz XTAL
// CPU clock: 10 MHz / 8 = 1.25 MHz
// Pixel clock: 10 MHz / 2 = 5 MHz
// HTOTAL: 320 pixel clocks → 320/4 = 80 CPU cycles per scanline
// VTOTAL: 256 scanlines per frame
// VBLANK: scanlines 0-23 (sync PROM bit 0), visible: 24-255 (232 lines)
// Frame rate: 5 MHz / (320 × 256) ≈ 61.04 Hz
const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 1_250_000, // 10 MHz / 8
    cycles_per_scanline: 80, // 320 pixel clocks / 4
    total_scanlines: 256,    // VTOTAL
    display_width: 256,
    display_height: 232, // 256 - 24 vblank lines
    display_aspect: Some((4, 3)),
};

// Palette resistor values: 22K / 10K / 4.7K with 1K pulldown.
// Each color channel uses a 3-bit inverted DAC through this network.
const PALETTE_RESISTORS: [f64; 3] = [22_000.0, 10_000.0, 4_700.0];
const PALETTE_PULLDOWN: f64 = 1_000.0;

// 8×16 sprites, 3bpp across two ROM halves (0x0000 and 0x2000).
// Plane 2 (MSB) from first-half low nibble, planes 1 and 0 from second-half
// high and low nibbles respectively. 32 bytes per sprite, 256 sprites.
const CCASTLES_SPRITE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0x10004, 0x10000, 4],
    x_offsets: &[0, 1, 2, 3, 8, 9, 10, 11],
    y_offsets: &[
        0, 16, 32, 48, 64, 80, 96, 112, 128, 144, 160, 176, 192, 208, 224, 240,
    ],
    char_increment: 256,
};

/// Crystal Castles Arcade System (Atari, 1983)
///
/// Hardware: MOS 6502 @ 1.25 MHz, 2× POKEY for sound.
/// Video: 256×232 bitmap, 4bpp packed (2 pixels/byte), hardware H/V scroll,
/// 80 motion objects (8×16, 3bpp), PROM-based priority compositing.
///
/// Memory map:
///   0x0000-0x0001  Bitmode address latches (write: set X/Y + write-through to VRAM)
///   0x0002         Bitmode data (R/W: pixel-level VRAM access via latches)
///   0x0003-0x7FFF  Video RAM (32KB bitmap, 4bpp packed, PROM write-protected)
///   0x8000-0x8DFF  Static RAM (3.5KB)
///   0x8E00-0x8FFF  Sprite RAM (two 256-byte MOB buffers)
///   0x9000-0x90FF  NVRAM (256 bytes, mirrored to 0x93FF)
///   0x9400-0x9403  Trackball inputs (LETA0-3, mirrored to 0x95FF)
///   0x9600-0x97FF  IN0 (digital inputs + VBLANK)
///   0x9800-0x980F  POKEY 1 (mirrored to 0x99FF)
///   0x9A00-0x9A0F  POKEY 2 (mirrored to 0x9BFF, ALLPOT=DIP switches)
///   0x9C00         NVRAM recall
///   0x9C80         H scroll register
///   0x9D00         V scroll register
///   0x9D80         IRQ acknowledge
///   0x9E00         Watchdog reset
///   0x9E80-0x9E87  Output latch 0 (ROM bank, coin counters, NVRAM store)
///   0x9F00-0x9F07  Output latch 1 / video control (bitmode, flip, sprite bank)
///   0x9F80-0x9FBF  Palette RAM (32 entries, 3-bit RGB inverted)
///   0xA000-0xDFFF  Banked program ROM (16KB, 2 banks)
///   0xE000-0xFFFF  Fixed program ROM (8KB)
/// Crystal Castles' hardware, everything the 6502 talks *to*. Held apart from
/// the CPU so a cycle dispatches at a concrete bus rather than a trait object
/// (see `docs/designs/concrete-bus-dispatch.md`).
#[derive(BusDebug)]
pub struct CrystalCastlesBoard {
    #[debug_device("POKEY 1")]
    pokey1: Pokey,
    #[debug_device("POKEY 2")]
    pokey2: Pokey,

    #[debug_map(cpu = 0)]
    map: AddressSpace16,

    gfx_rom: [u8; 0x4000],  // 16KB sprite graphics (not CPU-addressable)
    sprite_cache: GfxCache, // Pre-decoded 256 sprites (8×16, 3bpp)
    sync_prom: [u8; 0x100], // VBLANK/IRQ timing
    wp_prom: [u8; 0x100],   // Write-protect
    pri_prom: [u8; 0x100],  // Priority compositing

    // Video state
    bitmode_addr: [u8; 2], // X,Y auto-increment latches
    hscroll: u8,
    vscroll: u8,
    palette_ram: [u8; 64],           // Color RAM (64 addresses, 32 pens)
    palette_rgb: [(u8, u8, u8); 32], // Pre-computed RGB24

    // Output latches (LS259)
    // Latch 0 (8N) at 0x9E80: bit 0 = data & 1
    //   Bit 0: Trackball LED P1      Bit 1: Trackball LED P2
    //   Bit 2: NVRAM store low       Bit 3: NVRAM store high
    //   Bit 4: Spare                 Bit 5: Coin counter R
    //   Bit 6: Coin counter L        Bit 7: ROM bank select
    outlatch0: OutputLatch,
    // Latch 1 (6P) at 0x9F00: bit 0 = (data >> 3) & 1
    //   Bit 0: /AX (auto-X enable)   Bit 1: /AY (auto-Y enable)
    //   Bit 2: /XINC (X direction)    Bit 3: /YINC (Y direction)
    //   Bit 4: PLAYER2 (flip)         Bit 5: /SIRE
    //   Bit 6: BOTHRAM                Bit 7: BUF1/^BUF2 (sprite bank)
    outlatch1: OutputLatch,

    // I/O state
    // IN0 at 0x9600 (active-low except VBLANK):
    //   Bit 0: Coin R       Bit 1: Coin L       Bit 2: Service
    //   Bit 3: Tilt         Bit 4: Self-test     Bit 5: VBLANK (active-high)
    //   Bit 6: Jump Left    Bit 7: Jump Right
    in0: u8,
    dip_switches: u8, // Read via POKEY2 ALLPOT (0x9A08)
    /// LETA0-3 (Y1, X1, Y2, X2). Only player 1's pair is driven; the player 2
    /// counters exist so the 0x9400 read can index all four uniformly.
    trackball: [RelativeCounter; 4],

    // IRQ state — driven by sync PROM bit 3 rising edges (V=0,64,128,192)
    irq_state: bool,

    // System timing
    clock: u64,
    watchdog_frame_count: u8,

    // Rendering
    vblank_end: u8,           // First visible scanline (from sync PROM, typically 24)
    scanline_buffer: Vec<u8>, // 256 × 232 × 3 = 177,408 bytes (RGB24)
    scanline_buffer_valid: bool,
    sprite_buffer: Vec<u8>, // 256 × 256 temporary sprite layer (5-bit index)

    audio_buffer: SampleRing<i16>,
    /// The POKEYs' coupling into the amplifier.
    ///
    /// Both chips are unipolar, so their sum carries an offset that varies with
    /// how much they are playing. Removing it needs the running mean, not a
    /// constant: see the mixing site, where a fixed subtraction used to pin the
    /// output at the rail.
    pokey_coupling: DcBlocker,
}

/// Atari Crystal Castles (1983): a 6502 beside the board it drives.
#[derive(BusDebug)]
pub struct CrystalCastlesSystem {
    #[debug_cpu("M6502")]
    cpu: M6502,
    #[debug_bus]
    pub board: CrystalCastlesBoard,
}

/// One CPU cycle: the board's per-scanline work and POKEYs, then the 6502
/// against the board, which *is* the bus.
#[inline]
pub fn tick(cpu: &mut M6502, board: &mut CrystalCastlesBoard) {
    board.begin_cycle(cpu);
    cpu.execute_cycle(board, BusMaster::Cpu(0));
    board.clock += 1;
}

/// Crystal Castles reads full 8-bit trackball counters. `tick` drains them from
/// a 200-cycle divider (~100 ticks/frame), so each call moves a single unit and
/// the divider rate sets the responsiveness; the remainder stays pending.
fn new_track_counter() -> RelativeCounter {
    RelativeCounter::new(0xFF, 1, false, DrainPolicy::Unit)
}

impl CrystalCastlesBoard {
    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(
            Region::VideoRam,
            "Video RAM",
            0x0000,
            0x8000,
            AccessKind::ReadWrite,
        )
        .region(Region::Sram, "SRAM", 0x8000, 0x0E00, AccessKind::ReadWrite)
        .region(
            Region::SpriteRam,
            "Sprite RAM",
            0x8E00,
            0x0200,
            AccessKind::ReadWrite,
        )
        .region(
            Region::Nvram,
            "NVRAM",
            0x9000,
            0x0100,
            AccessKind::ReadWrite,
        )
        .mirror(0x9100, 0x9000, 0x0100)
        .mirror(0x9200, 0x9000, 0x0100)
        .mirror(0x9300, 0x9000, 0x0100)
        .region(Region::Io, "I/O", 0x9400, 0x0C00, AccessKind::Io)
        .region(
            Region::RomBank0,
            "ROM Bank 0",
            0xA000,
            0x4000,
            AccessKind::ReadOnly,
        )
        .backing_region(Region::RomBank1, "ROM Bank 1", 0x4000)
        .region(
            Region::RomFixed,
            "Fixed ROM",
            0xE000,
            0x2000,
            AccessKind::ReadOnly,
        );
        map
    }

    fn update_rom_bank(&mut self) {
        let id = if self.outlatch0.bit(7) {
            Region::RomBank1
        } else {
            Region::RomBank0
        };
        self.map.remap_pages(0xA0, 0x40, id, 0);
    }

    pub fn new() -> Self {
        Self {
            pokey1: Pokey::with_clock(1_250_000, phosphor_core::audio::host_sample_rate()),
            pokey2: Pokey::with_clock(1_250_000, phosphor_core::audio::host_sample_rate()),

            map: Self::build_map(),
            gfx_rom: [0; 0x4000],
            sprite_cache: GfxCache::new(256, 8, 16),
            sync_prom: [0; 0x100],
            wp_prom: [0; 0x100],
            pri_prom: [0; 0x100],

            bitmode_addr: [0; 2],
            hscroll: 0,
            vscroll: 0,
            palette_ram: [0; 64],
            palette_rgb: [(0, 0, 0); 32],

            outlatch0: OutputLatch::new(),
            outlatch1: OutputLatch::new(),

            // All active-low bits released (1), VBLANK off (bit 5 = 0)
            in0: 0xDF,
            dip_switches: 0x00,
            trackball: [
                new_track_counter(),
                new_track_counter(),
                new_track_counter(),
                new_track_counter(),
            ],

            irq_state: false,
            clock: 0,
            watchdog_frame_count: 0,

            vblank_end: 24,
            scanline_buffer: vec![0u8; 256 * 232 * 3],
            scanline_buffer_valid: false,
            sprite_buffer: vec![0u8; 256 * 256],

            audio_buffer: SampleRing::with_capacity(2048),
            pokey_coupling: DcBlocker::new(phosphor_core::audio::host_sample_rate()),
        }
    }

    /// Current scanline (V counter), 0-255.
    pub fn current_scanline(&self) -> u16 {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();
        (frame_cycle / TIMING.cycles_per_scanline) as u16
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let program = CCASTLES_PROGRAM_ROM.load(rom_set)?;
        self.map
            .load_region(Region::RomBank0, &program[0x0000..0x4000]);
        self.map
            .load_region(Region::RomFixed, &program[0x4000..0x6000]);
        self.map
            .load_region(Region::RomBank1, &program[0x6000..0xA000]);

        let gfx = CCASTLES_GFX_ROM.load(rom_set)?;
        self.gfx_rom.copy_from_slice(&gfx);
        self.sprite_cache = decode_gfx(&self.gfx_rom, 0, 256, &CCASTLES_SPRITE_LAYOUT);

        let sync = CCASTLES_SYNC_PROM.load(rom_set)?;
        self.sync_prom.copy_from_slice(&sync);

        let wp = CCASTLES_WP_PROM.load(rom_set)?;
        self.wp_prom.copy_from_slice(&wp);

        let pri = CCASTLES_PRI_PROM.load(rom_set)?;
        self.pri_prom.copy_from_slice(&pri);

        // Compute first visible scanline from sync PROM (bit 0 = VBLANK)
        self.vblank_end = (0..=255u8)
            .find(|&i| self.sync_prom[i as usize] & 1 == 0)
            .unwrap_or(24);

        Ok(())
    }

    /// Rebuild sprite cache from gfx_rom (for tests that modify ROM data directly).
    #[cfg(test)]
    fn decode_sprite_cache(&mut self) {
        self.sprite_cache = decode_gfx(&self.gfx_rom, 0, 256, &CCASTLES_SPRITE_LAYOUT);
    }

    // -----------------------------------------------------------------------
    // Video RAM write with write-protect PROM
    // -----------------------------------------------------------------------

    /// Write to VRAM through the write-protect PROM.
    ///
    /// The WP PROM controls which nibbles of two adjacent bytes can be written.
    /// Inputs to the PROM:
    ///   Bit 7 = BA1520 (1 if address bits 15-12 are all zero)
    ///   Bit 6-5 = DRBA11-10 (address bits 11-10)
    ///   Bit 4 = /BITMD (inverted bitmode flag)
    ///   Bit 3 = GND (always 0)
    ///   Bit 2 = BA0 (address bit 0)
    ///   Bit 1-0 = PIXB,PIXA (pixel position bits)
    fn write_vram(&mut self, addr: u16, data: u8, bitmd: u8, pixba: u8) {
        let dest_addr = (addr as usize) & 0x7FFE;

        let mut promaddr: u8 = 0;
        promaddr |= ((addr & 0xF000) == 0) as u8 * 0x80; // BA1520
        promaddr |= ((addr & 0x0C00) >> 5) as u8; // DRBA11-10
        promaddr |= ((bitmd == 0) as u8) << 4; // /BITMD
        // bit 3 = GND = 0
        promaddr |= ((addr & 0x0001) << 2) as u8; // BA0
        promaddr |= pixba & 3; // PIXB, PIXA

        let wpbits = self.wp_prom[promaddr as usize];

        // Write to the appropriate nibbles of two adjacent VRAM bytes
        if dest_addr < 0x8000 {
            let vram = self.map.region_data_mut(Region::VideoRam);
            if wpbits & 1 == 0 {
                vram[dest_addr] = (vram[dest_addr] & 0xF0) | (data & 0x0F);
            }
            if wpbits & 2 == 0 {
                vram[dest_addr] = (vram[dest_addr] & 0x0F) | (data & 0xF0);
            }
            if dest_addr + 1 < 0x8000 {
                if wpbits & 4 == 0 {
                    vram[dest_addr + 1] = (vram[dest_addr + 1] & 0xF0) | (data & 0x0F);
                }
                if wpbits & 8 == 0 {
                    vram[dest_addr + 1] = (vram[dest_addr + 1] & 0x0F) | (data & 0xF0);
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Bitmode — pixel-level VRAM access via auto-increment latches
    // -----------------------------------------------------------------------

    /// Auto-increment the bitmode X/Y latches after each access.
    /// Controlled by outlatch1: /AX (bit 0), /AY (bit 1),
    /// /XINC (bit 2, 0=increment), /YINC (bit 3, 0=increment).
    fn bitmode_autoinc(&mut self) {
        // Auto-increment X if /AX is low (bit 0 = 0)
        if !self.outlatch1.bit(0) {
            if !self.outlatch1.bit(2) {
                // /XINC low → increment
                self.bitmode_addr[0] = self.bitmode_addr[0].wrapping_add(1);
            } else {
                self.bitmode_addr[0] = self.bitmode_addr[0].wrapping_sub(1);
            }
        }
        // Auto-increment Y if /AY is low (bit 1 = 0)
        if !self.outlatch1.bit(1) {
            if !self.outlatch1.bit(3) {
                // /YINC low → increment
                self.bitmode_addr[1] = self.bitmode_addr[1].wrapping_add(1);
            } else {
                self.bitmode_addr[1] = self.bitmode_addr[1].wrapping_sub(1);
            }
        }
    }

    /// Bitmode read (address 0x0002): read a single pixel from VRAM.
    /// Address comes from the auto-increment latches. The appropriate nibble
    /// is shifted into the upper 4 bits; lower 4 bits are undriven (0xF).
    fn bitmode_r(&mut self) -> u8 {
        let addr = ((self.bitmode_addr[1] as u16) << 7) | ((self.bitmode_addr[0] as u16) >> 1);
        let shift = (!self.bitmode_addr[0] & 1) * 4;
        let result = self.map.region_data(Region::VideoRam)[addr as usize] << shift;

        self.bitmode_autoinc();
        result | 0x0F
    }

    /// Bitmode write (address 0x0002): write a single pixel to VRAM.
    /// Upper 4 bits of data are the pixel value, replicated to lower 4 bits.
    /// Writes go through the WP PROM with the low 2 X bits as PIXB/PIXA.
    fn bitmode_w(&mut self, data: u8) {
        let addr = ((self.bitmode_addr[1] as u16) << 7) | ((self.bitmode_addr[0] as u16) >> 1);
        let data = (data & 0xF0) | (data >> 4);

        self.write_vram(addr, data, 1, self.bitmode_addr[0] & 3);
        self.bitmode_autoinc();
    }

    /// Bitmode address write (addresses 0x0000-0x0001): set X or Y latch.
    /// Also writes through to VRAM as a standard videoram_w (bitmd=0, pixba=0).
    fn bitmode_addr_w(&mut self, offset: u8, data: u8) {
        self.write_vram(offset as u16, data, 0, 0);
        self.bitmode_addr[offset as usize] = data;
    }

    // -----------------------------------------------------------------------
    // Palette
    // -----------------------------------------------------------------------

    /// Recompute one RGB24 palette entry from palette RAM.
    ///
    /// Color format (from MAME):
    ///   R = ((data >> 6) & 3) | ((offset & 0x20) >> 3)  → 3-bit inverted
    ///   B = (data >> 3) & 7                               → 3-bit inverted
    ///   G = data & 7                                      → 3-bit inverted
    /// The 6-bit offset (0-63) provides the red MSB via bit 5.
    /// Weighted by 22K/10K/4.7K resistor network with 1K pulldown.
    fn update_palette_entry(&mut self, offset: usize) {
        use phosphor_core::gfx::{combine_weights, compute_resistor_weights};

        let data = self.palette_ram[offset];
        let r_raw = ((data & 0xC0) >> 6) | (((offset as u8) & 0x20) >> 3);
        let b_raw = (data & 0x38) >> 3;
        let g_raw = data & 0x07;

        // Invert all 3 bits, then weight via resistor network
        let r_inv = r_raw ^ 0x07;
        let g_inv = g_raw ^ 0x07;
        let b_inv = b_raw ^ 0x07;

        let w = compute_resistor_weights(&PALETTE_RESISTORS, Some(PALETTE_PULLDOWN));
        let r = combine_weights(&w, &[r_inv & 1, (r_inv >> 1) & 1, (r_inv >> 2) & 1]);
        let g = combine_weights(&w, &[g_inv & 1, (g_inv >> 1) & 1, (g_inv >> 2) & 1]);
        let b = combine_weights(&w, &[b_inv & 1, (b_inv >> 1) & 1, (b_inv >> 2) & 1]);

        self.palette_rgb[offset & 0x1F] = (r, g, b);
    }

    // -----------------------------------------------------------------------
    // Sprite rendering
    // -----------------------------------------------------------------------

    /// Render all sprites from the active MOB buffer into the sprite buffer.
    ///
    /// Called once per frame at VBLANK start. The sprite buffer is a 256×256
    /// array of 5-bit pixel indices (color_base | pixel_value), with 0x0F
    /// meaning transparent (no sprite).
    ///
    /// Sprite RAM format (4 bytes per sprite, 40 sprites max):
    ///   [offs+0] = sprite code (which, 0-255)
    ///   [offs+1] = Y position (displayed at 256 - 16 - value)
    ///   [offs+2] = bit 7: color group (0 or 1, selects palette 0-7 or 8-15)
    ///   [offs+3] = X position
    fn render_sprites_to_buffer(&mut self) {
        self.sprite_buffer.fill(0x0F);

        // Select active MOB buffer (outlatch1 bit 7: BUF1/BUF2)
        let buf_offset: usize = if self.outlatch1.bit(7) { 0x100 } else { 0x00 };
        let flip = self.outlatch1.bit(4);
        let sprites = self.map.region_data(Region::SpriteRam);

        // 40 sprites: 160 bytes / 4 bytes per sprite
        for offs in (0..160).step_by(4) {
            let which = sprites[buf_offset + offs];
            let sy = 256u16
                .wrapping_sub(16)
                .wrapping_sub(sprites[buf_offset + offs + 1] as u16);
            let color_base = (sprites[buf_offset + offs + 2] >> 7) * 8;
            let sx = sprites[buf_offset + offs + 3] as u16;

            for row in 0..16u16 {
                for col in 0..8u16 {
                    let r = if flip { 15 - row } else { row };
                    let c = if flip { 7 - col } else { col };
                    let pixel = self
                        .sprite_cache
                        .pixel(which as usize, c as usize, r as usize);
                    if pixel == 7 {
                        continue; // transparent pen
                    }

                    let dy = sy.wrapping_add(row) & 0xFF;
                    let dx = sx.wrapping_add(col) & 0xFF;
                    self.sprite_buffer[(dy as usize) * 256 + (dx as usize)] = color_base | pixel;
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Per-scanline compositing
    // -----------------------------------------------------------------------

    /// Render one hardware scanline to the RGB24 output buffer.
    ///
    /// Composites the scrolled 4bpp bitmap with the sprite layer using the
    /// priority PROM to select between them and assign the final 5-bit
    /// palette index (0-31).
    ///
    /// Priority PROM inputs (from MAME):
    ///   Bit 6 = /CRAM (always 1)
    ///   Bits 4-2 = MV2,MV1,MV0 (sprite pixel value bits 2,1,0)
    ///   Bit 1 = MPI (sprite color group: mopix bit 3)
    ///   Bit 0 = BIT3 (bitmap pixel bit 3)
    /// Priority PROM outputs:
    ///   Bit 1 = select sprite (1) or bitmap (0)
    ///   Bit 0 = set bit 4 of final palette index (upper/lower 16 colors)
    fn render_scanline_to_buffer(&mut self, hw_scanline: u8) {
        // Skip VBLANK scanlines
        if self.sync_prom[hw_scanline as usize] & 1 != 0 {
            return;
        }

        // Wrapping, like the `effy` computation below: a scanline above
        // `vblank_end` is off-screen, and letting it wrap to a large value lets
        // the visible-height check below reject it. A plain subtraction relies
        // on the sync PROM flagging every such scanline as VBLANK, which is
        // true of the real PROM but panics in debug on a blank one.
        let screen_y = hw_scanline.wrapping_sub(self.vblank_end) as usize;
        if screen_y >= 232 {
            return;
        }

        let flip: u8 = if self.outlatch1.bit(4) { 0xFF } else { 0x00 };
        let vscroll_val = if flip != 0 { 0u8 } else { self.vscroll };

        // Effective Y into the bitmap, with scroll and flip
        let mut effy = (hw_scanline
            .wrapping_sub(self.vblank_end)
            .wrapping_add(vscroll_val)
            ^ flip) as usize;
        if effy < self.vblank_end as usize {
            effy = self.vblank_end as usize;
        }

        let src_base = effy * 128;
        let row_offset = screen_y * 256 * 3;

        for x in 0..256usize {
            let effx = self.hscroll.wrapping_add((x as u8) ^ flip) as usize;

            // Read 4bpp bitmap pixel (2 pixels per byte: low nibble = even, high = odd)
            let vram = self.map.region_data(Region::VideoRam);
            let pix = (vram[src_base + effx / 2] >> ((effx & 1) * 4)) & 0x0F;

            // Read sprite pixel from sprite buffer (screen-space, not scrolled)
            let mopix = self.sprite_buffer[hw_scanline as usize * 256 + x];

            // Priority PROM lookup
            let prindex: u8 = 0x40 | ((mopix & 7) << 2) | ((mopix & 8) >> 2) | ((pix & 8) >> 3);
            let prvalue = self.pri_prom[prindex as usize];

            // Bit 1: select sprite or bitmap as source
            let base_pix = if prvalue & 2 != 0 { mopix } else { pix };
            // Bit 0: set bit 4 of final palette index
            let final_pix = (base_pix & 0x0F) | ((prvalue & 1) << 4);

            let (r, g, b) = self.palette_rgb[final_pix as usize];
            let pixel_offset = row_offset + x * 3;
            self.scanline_buffer[pixel_offset] = r;
            self.scanline_buffer[pixel_offset + 1] = g;
            self.scanline_buffer[pixel_offset + 2] = b;
        }
    }

    // -----------------------------------------------------------------------
    // Tick
    // -----------------------------------------------------------------------

    /// Board work that leads a CPU cycle: trackballs, the per-scanline IRQ and
    /// render, the VBLANK bit, the POKEYs, and the debugger's attribution latch.
    fn begin_cycle(&mut self, cpu: &M6502) {
        // Trackball movement: drain mouse accumulator / apply keyboard input.
        // Rate: every 200 cycles (~100 ticks/frame) for responsive 8-bit counters.
        if self.clock.is_multiple_of(200) {
            for counter in &mut self.trackball {
                counter.update();
            }
        }

        // Per-scanline processing: IRQ generation, VBLANK, and rendering
        let frame_cycle = self.clock % TIMING.cycles_per_frame();
        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let scanline = (frame_cycle / TIMING.cycles_per_scanline) as u8;

            // IRQ generation from sync PROM rising edges on bit 3
            let prev = if scanline == 0 { 255 } else { scanline - 1 };
            if (self.sync_prom[prev as usize] & 8) == 0
                && (self.sync_prom[scanline as usize] & 8) != 0
                && !self.irq_state
            {
                self.irq_state = true;
            }

            // Render sprites once at VBLANK start (scanline 0)
            if scanline == 0 {
                self.render_sprites_to_buffer();
            }

            // Render visible scanlines (composites bitmap + sprites)
            self.render_scanline_to_buffer(scanline);
        }

        // Update VBLANK bit in IN0 (bit 5, active-high from sync PROM bit 0)
        let scanline = self.current_scanline() as u8;
        if self.sync_prom[scanline as usize] & 1 != 0 {
            self.in0 |= 0x20; // VBLANK active
        } else {
            self.in0 &= !0x20; // VBLANK inactive
        }

        // POKEY ticks (both run at CPU clock = 1.25 MHz)
        self.pokey1.tick();
        self.pokey2.tick();

        // Latch watchpoint attribution context (cycle + instruction PC)
        // before CPU execution — bus dispatch cannot read CPU state mid-tick.
        if self.map.has_any_watchpoints() {
            let pc = cpu.at_instruction_boundary().then_some(cpu.pc as u32);
            self.map.latch_access_context(self.clock, pc);
        }
    }
}

impl CrystalCastlesSystem {
    pub fn new() -> Self {
        Self {
            cpu: M6502::new(),
            board: CrystalCastlesBoard::new(),
        }
    }

    /// Advance one CPU cycle, returning the instruction-boundary mask.
    pub fn step_cycle(&mut self) -> u32 {
        tick(&mut self.cpu, &mut self.board);
        u32::from(self.cpu.at_instruction_boundary())
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        self.board.load_rom_set(rom_set)
    }

    pub fn get_cpu_state(&self) -> M6502State {
        self.cpu.snapshot()
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

impl Default for CrystalCastlesSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for CrystalCastlesBoard {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus implementation — full address decoding
// ---------------------------------------------------------------------------

// The board is the bus.
impl Bus for CrystalCastlesBoard {
    type Address = u16;
    type Data = u8;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false // No DMA hardware on Crystal Castles
    }

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let data = match self.map.page(addr).region_id {
            Region::VIDEO_RAM => {
                if addr == 0x0002 {
                    self.bitmode_r()
                } else {
                    self.map.read_backing(addr)
                }
            }

            Region::SRAM
            | Region::SPRITE_RAM
            | Region::NVRAM
            | Region::ROM_BANK0
            | Region::ROM_BANK1
            | Region::ROM_FIXED => self.map.read_backing(addr),

            Region::IO => match addr {
                // Trackball LETA0-3 (mirrored: 0x9400-0x95FF)
                0x9400..=0x95FF => self.trackball[(addr & 0x03) as usize].counter(),
                // IN0 — digital inputs + VBLANK (0x9600-0x97FF)
                0x9600..=0x97FF => self.in0,
                // POKEY 1 (mirrored: 0x9800-0x99FF)
                0x9800..=0x99FF => self.pokey1.read(addr & 0x0F),
                // POKEY 2 (mirrored: 0x9A00-0x9BFF)
                // ALLPOT (offset 0x08) is wired to DIP switches
                0x9A00..=0x9BFF => {
                    let offset = addr & 0x0F;
                    if offset == 0x08 {
                        self.dip_switches
                    } else {
                        self.pokey2.read(offset)
                    }
                }
                _ => 0xFF,
            },

            _ => 0xFF,
        };

        self.map.watch_read(0, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.map.watch_write(0, master, addr, data);

        match self.map.page(addr).region_id {
            Region::VIDEO_RAM => match addr {
                0x0000..=0x0001 => self.bitmode_addr_w(addr as u8, data),
                0x0002 => self.bitmode_w(data),
                _ => self.write_vram(addr, data, 0, 0),
            },

            Region::SRAM | Region::SPRITE_RAM | Region::NVRAM => {
                self.map.write_backing(addr, data);
            }

            Region::IO => match addr {
                // POKEY 1 (mirrored: 0x9800-0x99FF)
                0x9800..=0x99FF => self.pokey1.write(addr & 0x0F, data),
                // POKEY 2 (mirrored: 0x9A00-0x9BFF)
                0x9A00..=0x9BFF => self.pokey2.write(addr & 0x0F, data),
                // NVRAM recall (no-op)
                0x9C00..=0x9C7F => {}
                // H scroll
                0x9C80..=0x9CFF => self.hscroll = data,
                // V scroll
                0x9D00..=0x9D7F => self.vscroll = data,
                // IRQ acknowledge
                0x9D80..=0x9DFF => self.irq_state = false,
                // Watchdog reset
                0x9E00..=0x9E7F => self.watchdog_frame_count = 0,
                // Output latch 0: bit = addr & 7, value = data & 1
                0x9E80..=0x9EFF => {
                    self.outlatch0.write((addr & 7) as u8, data & 1 != 0);
                    if addr & 7 == 7 {
                        self.update_rom_bank();
                    }
                }
                // Output latch 1: bit = addr & 7, value = (data >> 3) & 1
                0x9F00..=0x9F7F => {
                    self.outlatch1.write((addr & 7) as u8, data & 0x08 != 0);
                }
                // Palette RAM (64 addresses → 32 pens)
                0x9F80..=0x9FFF => {
                    let offset = (addr & 0x3F) as usize;
                    self.palette_ram[offset] = data;
                    self.update_palette_entry(offset);
                }
                _ => {}
            },

            _ => {}
        }
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            nmi: false,
            irq: self.irq_state,
            firq: false,
            irq_vector: 0,
            irq_level: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Machine traits (MachineCore + capabilities)
// ---------------------------------------------------------------------------

impl Renderable for CrystalCastlesSystem {
    fn display_size(&self) -> (u32, u32) {
        TIMING.display_size()
    }

    fn display_aspect(&self) -> Option<(u32, u32)> {
        TIMING.display_aspect()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        if self.board.scanline_buffer_valid {
            buffer.copy_from_slice(&self.board.scanline_buffer);
        } else {
            // Black screen before first frame
            buffer.fill(0);
        }
    }
}

impl AudioSource for CrystalCastlesSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.board.audio_buffer.pop_front_into(buffer)
    }

    fn audio_sample_rate(&self) -> u32 {
        phosphor_core::audio::host_sample_rate()
    }
}

impl InputConfigurable for CrystalCastlesSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        CCASTLES_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Button { id, pressed } => match id.0 as u8 {
                INPUT_COIN_L => set_bit_active_low(&mut self.board.in0, 1, pressed),
                INPUT_COIN_R => set_bit_active_low(&mut self.board.in0, 0, pressed),
                INPUT_JUMP_LEFT => set_bit_active_low(&mut self.board.in0, 6, pressed),
                INPUT_JUMP_RIGHT => set_bit_active_low(&mut self.board.in0, 7, pressed),
                INPUT_TRACK_L => self.board.trackball[1].set_held(false, pressed),
                INPUT_TRACK_R => self.board.trackball[1].set_held(true, pressed),
                INPUT_TRACK_U => self.board.trackball[0].set_held(false, pressed),
                INPUT_TRACK_D => self.board.trackball[0].set_held(true, pressed),
                _ => {}
            },
            InputEvent::Relative { id, delta } => {
                let delta = delta as i32;
                if id == CTRL_TRACKBALL_X {
                    self.board.trackball[1].add_delta(delta as f32);
                } else if id == CTRL_TRACKBALL_Y {
                    // Y inverted: mouse down → trackball counter increases (moves down)
                    self.board.trackball[0].add_delta(-delta as f32);
                }
            }
            InputEvent::Absolute { .. } => {}
        }
    }

    /// Also clears conditioned analog state: the digital releases above cannot
    /// reach accumulated motion or a held deflection.
    fn release_all_inputs(&mut self) {
        phosphor_core::core::machine::release_all_controls(self);
        for c in &mut self.board.trackball {
            c.release_all();
        }
    }
}

crate::impl_standalone_debug!(CrystalCastlesSystem);

impl Saveable for CrystalCastlesSystem {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        self.board.pokey1.save_state(w);
        self.board.pokey2.save_state(w);
        self.board.pokey_coupling.save_state(w);
        w.write_bytes(self.board.map.region_data(Region::VideoRam));
        w.write_bytes(self.board.map.region_data(Region::Sram));
        w.write_bytes(self.board.map.region_data(Region::SpriteRam));
        w.write_bytes(self.board.map.region_data(Region::Nvram));
        w.write_bytes(&self.board.bitmode_addr);
        w.write_u8(self.board.hscroll);
        w.write_u8(self.board.vscroll);
        w.write_bytes(&self.board.palette_ram);
        self.board.outlatch0.save_state(w);
        self.board.outlatch1.save_state(w);
        w.write_u8(self.board.in0);
        for counter in &self.board.trackball {
            counter.save_state(w);
        }
        w.write_bool(self.board.irq_state);
        w.write_u64_le(self.board.clock);
        w.write_u8(self.board.watchdog_frame_count);
        w.write_u8(self.board.dip_switches);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        self.board.pokey1.load_state(r)?;
        self.board.pokey2.load_state(r)?;
        self.board.pokey_coupling.load_state(r)?;
        r.read_bytes_into(self.board.map.region_data_mut(Region::VideoRam))?;
        r.read_bytes_into(self.board.map.region_data_mut(Region::Sram))?;
        r.read_bytes_into(self.board.map.region_data_mut(Region::SpriteRam))?;
        r.read_bytes_into(self.board.map.region_data_mut(Region::Nvram))?;
        r.read_bytes_into(&mut self.board.bitmode_addr)?;
        self.board.hscroll = r.read_u8()?;
        self.board.vscroll = r.read_u8()?;
        r.read_bytes_into(&mut self.board.palette_ram)?;
        self.board.outlatch0.load_state(r)?;
        self.board.outlatch1.load_state(r)?;
        self.board.in0 = r.read_u8()?;
        for counter in &mut self.board.trackball {
            counter.load_state(r)?;
        }
        self.board.irq_state = r.read_bool()?;
        self.board.clock = r.read_u64_le()?;
        self.board.watchdog_frame_count = r.read_u8()?;
        self.board.dip_switches = r.read_u8()?;
        // Recompute derived state
        for i in 0..64 {
            self.board.update_palette_entry(i);
        }
        self.board.update_rom_bank();
        self.board.scanline_buffer_valid = false;
        self.board.audio_buffer.clear();
        Ok(())
    }
}

impl MachineCore for CrystalCastlesSystem {
    // No gfx_sheets() override: the playfield is a 4bpp VRAM bitmap (no tile
    // cache), and the lone sprite cache colors entirely from palette color code
    // 0, which is black — a pen-group-0 sheet would be an all-black window.
    // Revisit if/when the viewer gains color-code selection.

    fn run_frame(&mut self) {
        for _ in 0..TIMING.cycles_per_frame() {
            tick(&mut self.cpu, &mut self.board);
        }
        self.board.scanline_buffer_valid = true;

        // Watchdog: 8-VBLANK timeout
        self.board.watchdog_frame_count += 1;
        if self.board.watchdog_frame_count >= 8 {
            self.reset();
        }

        // Drain both POKEYs and mix to mono
        let samples1 = self.board.pokey1.drain_audio();
        let samples2 = self.board.pokey2.drain_audio();
        let len = samples1.len().min(samples2.len());
        // Each POKEY emits 0..1: a channel contributes its volume while its
        // output is high and nothing while it is low, so the pin swings from
        // ground up rather than either side of it.
        //
        // This used to centre the pair by subtracting 1.0, which assumes their
        // mean is exactly 1.0, i.e. that both chips sit at half scale. They do
        // not: they are near zero most of the time, so the subtraction pinned
        // the output at the negative rail. It measured a DC of -0.973 with 74 %
        // of samples clipped, which is the offset and the saturation both
        // coming from this one constant.
        //
        // A coupling capacitor centres on the mean the chips actually produce
        // instead of an assumed one, which is also what the board has between
        // them and the amplifier.
        let coupling = &mut self.board.pokey_coupling;
        self.board.audio_buffer.extend((0..len).map(|i| {
            let mixed = coupling.process(samples1[i] + samples2[i]);
            (mixed * 32767.0).clamp(i16::MIN as f32, i16::MAX as f32) as i16
        }));
    }

    fn reset(&mut self) {
        self.board.irq_state = false;
        self.board.watchdog_frame_count = 0;
        self.board.outlatch0.reset();
        self.board.outlatch1.reset();
        self.board.update_rom_bank();
        self.board.bitmode_addr = [0; 2];
        self.board.hscroll = 0;
        self.board.vscroll = 0;
        self.board.in0 = 0xDF;
        self.board.scanline_buffer.fill(0);
        self.board.scanline_buffer_valid = false;
        self.board.sprite_buffer.fill(0);
        self.board.audio_buffer.clear();

        self.cpu.reset(&mut self.board, BusMaster::Cpu(0));
    }

    fn frame_rate_hz(&self) -> f64 {
        TIMING.frame_rate_hz()
    }

    fn machine_id(&self) -> &str {
        "ccastles"
    }
}

impl SaveState for CrystalCastlesSystem {
    fn save_state(&self) -> Option<Vec<u8>> {
        Some(save_state::save_machine(self, self.machine_id()))
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), SaveError> {
        let id = self.machine_id().to_string();
        save_state::load_machine(self, &id, data)
    }
}

impl Nvram for CrystalCastlesSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.map.region_data(Region::Nvram))
    }

    fn load_nvram(&mut self, data: &[u8]) {
        let nvram = self.board.map.region_data_mut(Region::Nvram);
        let len = data.len().min(nvram.len());
        nvram[..len].copy_from_slice(&data[..len]);
    }
}

impl Profilable for CrystalCastlesSystem {}
/// DIP switch metadata for Crystal Castles. The option byte is read back
/// through POKEY2's ALLPOT input (MAME wires `pokey2.allpot_r` to its "IN1"
/// port); of that port, Cabinet is the only documented option switch — the
/// other bits are start buttons and undocumented switches. Gameplay options
/// (difficulty, bonus, etc.) are configured via the in-game service menu stored
/// in the EAROM, not hardware DIPs. Default 0x00 = upright.
const CCASTLES_DIP_BANKS: &[DipSwitchBank] = &[DipSwitchBank {
    name: "Options",
    options: &[DipOption {
        name: "Cabinet",
        mask: 0x20,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "Upright",
                value: 0x00,
            },
            DipChoice {
                label: "Cocktail",
                value: 0x20,
            },
        ],
    }],
}];

crate::impl_dip_switches!(CrystalCastlesSystem, CCASTLES_DIP_BANKS, board.dip_switches);
impl phosphor_core::core::debug_trace::DebugTrace for CrystalCastlesSystem {}

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

crate::register_machine!(
    CrystalCastlesSystem,
    "ccastles",
    &["ccastles"],
    CCASTLES_CONTROLS
);

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::DipSwitches;
    use phosphor_core::cpu::CpuStateTrait;

    #[test]
    fn dip_default_and_metadata() {
        let sys = CrystalCastlesSystem::new();
        assert_eq!(sys.dip_bank_value(0), 0x00); // upright
        crate::assert_dip_banks_valid(sys.dip_banks(), &[sys.dip_bank_value(0)]);
    }

    #[test]
    fn set_dip_option_masks_only_its_bits() {
        let mut sys = CrystalCastlesSystem::new();
        sys.board.dip_switches = 0x05; // stray bits outside the Cabinet mask
        // Cabinet is option 0 (mask 0x20); pick "Cocktail" (0x20).
        sys.set_dip_option(0, 0, 0x20);
        assert_eq!(sys.dip_bank_value(0), 0x25); // 0x05 preserved + cabinet bit
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = CrystalCastlesSystem::new();
        sys.board.map.region_data_mut(Region::VideoRam)[0x1000] = 0xAB;
        sys.board.map.region_data_mut(Region::Sram)[0x100] = 0xCD;
        sys.board.map.region_data_mut(Region::SpriteRam)[0x10] = 0xEF;
        sys.board.map.region_data_mut(Region::Nvram)[0x20] = 0x42;
        sys.board.hscroll = 0x80;
        sys.board.vscroll = 0x40;
        // Set outlatch0 to 0x80 (bit 7) via latch API
        sys.board.outlatch0.write(7, true);
        // Set outlatch1 to 0x0F (bits 0-3) via latch API
        for b in 0..4u8 {
            sys.board.outlatch1.write(b, true);
        }
        sys.board.in0 = 0xBF;
        sys.board.trackball[1].set_counter(0x55);
        sys.board.trackball[1].add_delta(-10.0);
        sys.board.irq_state = true;
        sys.board.clock = 50_000;
        sys.board.watchdog_frame_count = 3;
        sys.board.dip_switches = 0x55;

        let data = SaveState::save_state(&sys).expect("save_state should return Some");
        let cpu_snap = sys.cpu.snapshot();

        let mut sys2 = CrystalCastlesSystem::new();
        SaveState::load_state(&mut sys2, &data).unwrap();

        assert_eq!(sys2.cpu.snapshot(), cpu_snap);
        assert_eq!(sys2.board.map.region_data(Region::VideoRam)[0x1000], 0xAB);
        assert_eq!(sys2.board.map.region_data(Region::Sram)[0x100], 0xCD);
        assert_eq!(sys2.board.map.region_data(Region::SpriteRam)[0x10], 0xEF);
        assert_eq!(sys2.board.map.region_data(Region::Nvram)[0x20], 0x42);
        assert_eq!(sys2.board.hscroll, 0x80);
        assert_eq!(sys2.board.vscroll, 0x40);
        assert_eq!(sys2.board.outlatch0.value(), 0x80);
        assert_eq!(sys2.board.outlatch1.value(), 0x0F);
        assert_eq!(sys2.board.in0, 0xBF);
        assert_eq!(sys2.board.trackball[1].counter(), 0x55);
        // Pending motion survives the round-trip: one drain step moves it.
        sys2.board.trackball[1].update();
        assert_eq!(sys2.board.trackball[1].counter(), 0x54);
        assert!(sys2.board.irq_state);
        assert_eq!(sys2.board.clock, 50_000);
        assert_eq!(sys2.board.watchdog_frame_count, 3);
        assert_eq!(sys2.board.dip_switches, 0x55);
    }

    #[test]
    fn rom_banking_selects_correct_bank() {
        let mut sys = CrystalCastlesSystem::new();
        // Fill bank 0 low with 0xAA, bank 1 low with 0xBB
        sys.board.map.region_data_mut(Region::RomBank0)[..0x2000].fill(0xAA);
        sys.board.map.region_data_mut(Region::RomBank1)[..0x2000].fill(0xBB);

        // Bank 0 (default, outlatch0 bit 7 = 0)
        sys.board.outlatch0.reset();
        sys.board.update_rom_bank();
        assert_eq!(
            Bus::read(&mut sys.board, BusMaster::Cpu(0), 0xA000),
            0xAA,
            "Bank 0 should read from ROM_BANK0"
        );

        // Bank 1 (outlatch0 bit 7 = 1)
        sys.board.outlatch0.write(7, true);
        sys.board.update_rom_bank();
        assert_eq!(
            Bus::read(&mut sys.board, BusMaster::Cpu(0), 0xA000),
            0xBB,
            "Bank 1 should read from ROM_BANK1"
        );
    }

    #[test]
    fn fixed_rom_always_accessible() {
        let mut sys = CrystalCastlesSystem::new();
        sys.board.map.region_data_mut(Region::RomFixed)[0x0000] = 0xDE;
        sys.board.map.region_data_mut(Region::RomFixed)[0x1FFF] = 0xAD;

        assert_eq!(Bus::read(&mut sys.board, BusMaster::Cpu(0), 0xE000), 0xDE);
        assert_eq!(Bus::read(&mut sys.board, BusMaster::Cpu(0), 0xFFFF), 0xAD);
    }

    #[test]
    fn nvram_mirroring() {
        let mut sys = CrystalCastlesSystem::new();
        Bus::write(&mut sys.board, BusMaster::Cpu(0), 0x9000, 0x42);
        assert_eq!(Bus::read(&mut sys.board, BusMaster::Cpu(0), 0x9100), 0x42);
        assert_eq!(Bus::read(&mut sys.board, BusMaster::Cpu(0), 0x9200), 0x42);
        assert_eq!(Bus::read(&mut sys.board, BusMaster::Cpu(0), 0x9300), 0x42);
    }

    #[test]
    fn outlatch0_bit_write() {
        let mut sys = CrystalCastlesSystem::new();
        // Set bit 7 (ROM bank select) by writing data & 1 = 1 to addr 0x9E87
        Bus::write(&mut sys.board, BusMaster::Cpu(0), 0x9E87, 0x01);
        assert!(sys.board.outlatch0.bit(7));
        // Clear it
        Bus::write(&mut sys.board, BusMaster::Cpu(0), 0x9E87, 0x00);
        assert!(!sys.board.outlatch0.bit(7));
    }

    #[test]
    fn outlatch1_uses_bit3_of_data() {
        let mut sys = CrystalCastlesSystem::new();
        // Set bit 0 (/AX): data bit 3 must be set
        Bus::write(&mut sys.board, BusMaster::Cpu(0), 0x9F00, 0x08);
        assert!(sys.board.outlatch1.bit(0));
        // Data bit 0 should NOT set the latch (only D3 matters)
        Bus::write(&mut sys.board, BusMaster::Cpu(0), 0x9F00, 0x01);
        assert!(!sys.board.outlatch1.bit(0));
    }

    #[test]
    fn irq_acknowledge_clears_state() {
        let mut sys = CrystalCastlesSystem::new();
        sys.board.irq_state = true;
        Bus::write(&mut sys.board, BusMaster::Cpu(0), 0x9D80, 0x00);
        assert!(!sys.board.irq_state);
    }

    #[test]
    fn palette_entry_updates_rgb() {
        let mut sys = CrystalCastlesSystem::new();
        // Write all-zeros to palette entry 0 → all bits inverted → max brightness
        Bus::write(&mut sys.board, BusMaster::Cpu(0), 0x9F80, 0x00);
        assert_eq!(sys.board.palette_rgb[0], (255, 255, 255));

        // Write all-ones (0xFF) → r_raw = 3, g_raw = 7, b_raw = 7
        // Inverted: r = 7^7=4 (wait, r_raw = ((0xC0>>6) | (0&0x20)>>3) = 3)
        // r_inv = 3^7=4 → bits 2,0 set → 144+36=180? No: 4 = 0b100 → bit2=1 → 144
        // Actually: 3 ^ 7 = 0b011 ^ 0b111 = 0b100 = 4. bit0=0, bit1=0, bit2=1 → 144
        // g_inv = 7^7=0 → all zero → 0
        // b_inv = 7^7=0 → 0
        Bus::write(&mut sys.board, BusMaster::Cpu(0), 0x9F80, 0xFF);
        assert_eq!(sys.board.palette_rgb[0], (144, 0, 0));
    }

    #[test]
    fn input_active_low() {
        let mut sys = CrystalCastlesSystem::new();
        // Default: all active-low bits set (released)
        assert_eq!(sys.board.in0 & 0x02, 0x02, "Coin L should be released");
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_COIN_L) as u16),
            pressed: true,
        });
        assert_eq!(
            sys.board.in0 & 0x02,
            0x00,
            "Coin L should be pressed (active-low)"
        );
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_COIN_L) as u16),
            pressed: false,
        });
        assert_eq!(
            sys.board.in0 & 0x02,
            0x02,
            "Coin L should be released again"
        );
    }

    #[test]
    fn exposes_two_analog_axes() {
        let sys = CrystalCastlesSystem::new();
        let analog = sys
            .input_controls()
            .iter()
            .filter(|c| matches!(c.kind, InputKind::AnalogAxis { .. }))
            .count();
        assert_eq!(analog, 2);
    }

    #[test]
    fn sprite_pixel_extraction() {
        let mut sys = CrystalCastlesSystem::new();
        // Set up GFX ROM for sprite 0, row 0:
        // gfx_rom[0] = 0x0B = 0000_1011 → low nibble bits 3,2,1,0 = 1,0,1,1
        // gfx_rom[0x2000] = 0xD6 = 1101_0110
        //   high nibble bits 7,6,5,4 = 1,1,0,1
        //   low nibble bits 3,2,1,0 = 0,1,1,0
        sys.board.gfx_rom[0x0000] = 0x0B;
        sys.board.gfx_rom[0x2000] = 0xD6;
        sys.board.decode_sprite_cache();

        // Pixel 0: p2=bit3(0x0B)=1, p1=bit7(0xD6)=1, p0=bit3(0xD6)=0 → 0b110 = 6
        assert_eq!(sys.board.sprite_cache.pixel(0, 0, 0), 6);
        // Pixel 1: p2=bit2(0x0B)=0, p1=bit6(0xD6)=1, p0=bit2(0xD6)=1 → 0b011 = 3
        assert_eq!(sys.board.sprite_cache.pixel(0, 1, 0), 3);
        // Pixel 2: p2=bit1(0x0B)=1, p1=bit5(0xD6)=0, p0=bit1(0xD6)=1 → 0b101 = 5
        assert_eq!(sys.board.sprite_cache.pixel(0, 2, 0), 5);
        // Pixel 3: p2=bit0(0x0B)=1, p1=bit4(0xD6)=1, p0=bit0(0xD6)=0 → 0b110 = 6
        assert_eq!(sys.board.sprite_cache.pixel(0, 3, 0), 6);
    }

    #[test]
    fn sprite_transparent_pixel_not_drawn() {
        let mut sys = CrystalCastlesSystem::new();
        // Set all GFX ROM to produce pixel value 7 (transparent pen):
        // p0=1, p1=1, p2=1 → 7
        // Plane 0 (first half, low nibble): all 1s → 0x0F
        // Plane 1 (second half, high nibble): all 1s → 0xF0
        // Plane 2 (second half, low nibble): all 1s → 0x0F
        sys.board.gfx_rom[0..0x2000].fill(0x0F);
        sys.board.gfx_rom[0x2000..0x4000].fill(0xFF); // 0xF0 | 0x0F
        sys.board.decode_sprite_cache();

        // Place sprite 0 at position (100, 100)
        let sprites = sys.board.map.region_data_mut(Region::SpriteRam);
        sprites[0] = 0; // sprite code
        sprites[1] = (256 - 16 - 100) as u8; // Y = 100
        sprites[2] = 0; // color group 0
        sprites[3] = 100; // X = 100

        sys.board.render_sprites_to_buffer();

        // All transparent → sprite buffer should remain 0x0F everywhere
        assert_eq!(sys.board.sprite_buffer[100 * 256 + 100], 0x0F);
        assert_eq!(sys.board.sprite_buffer[100 * 256 + 107], 0x0F);
    }

    #[test]
    fn sprite_renders_to_buffer() {
        let mut sys = CrystalCastlesSystem::new();
        // Set GFX ROM so sprite 1, row 0, pixel 0 produces value 5 (not transparent):
        // p0=1, p1=0, p2=1 → 0b101 = 5
        // Pixel 0 uses bit position 3 (MSB-first: 3 - 0%4 = 3).
        // First half: sprite 1 starts at byte 32. Row 0, byte 0 = offset 32.
        //   Plane 0 (low nibble) bit 3 → set bit 3 = 0x08
        sys.board.gfx_rom[32] = 0x08;
        // Second half: offset 0x2000 + 32 = 0x2020.
        //   Plane 1 (high nibble) bit 7 → clear (want p1=0)
        //   Plane 2 (low nibble) bit 3 → set bit 3 = 0x08
        sys.board.gfx_rom[0x2020] = 0x08;
        sys.board.decode_sprite_cache();

        // Place sprite with code 1 at (50, 200)
        let sprites = sys.board.map.region_data_mut(Region::SpriteRam);
        sprites[0] = 1; // sprite code
        sprites[1] = (256u16.wrapping_sub(16).wrapping_sub(200)) as u8; // Y
        sprites[2] = 0x80; // color group 1 → color_base = 8
        sprites[3] = 50; // X

        sys.board.render_sprites_to_buffer();

        // Sprite pixel 0 of row 0 should be at (50, 200): color_base(8) | 5 = 13
        assert_eq!(sys.board.sprite_buffer[200 * 256 + 50], 13);
    }

    #[test]
    fn scanline_compositing_renders_bitmap() {
        let mut sys = CrystalCastlesSystem::new();
        // Set sync PROM: scanlines 0-23 = VBLANK (bit 0 set), 24-255 = visible
        sys.board.sync_prom[..24].fill(0x01);
        sys.board.sync_prom[24..].fill(0x00);
        sys.board.vblank_end = 24;

        // Set palette entry 5 to a known color
        sys.board.palette_ram[5] = 0x00; // all zeros → inverted = all 1s → white
        sys.board.update_palette_entry(5);
        assert_eq!(sys.board.palette_rgb[5], (255, 255, 255));

        // Write bitmap pixel value 5 at effective Y=24, X=0
        // videoram[24 * 128 + 0] low nibble = 5
        sys.board.map.region_data_mut(Region::VideoRam)[24 * 128] = 0x05;

        // Sprite buffer clear (transparent)
        sys.board.sprite_buffer.fill(0x0F);

        // Set a priority PROM that selects bitmap (bit 1 = 0) and no bit 4 (bit 0 = 0)
        // For transparent sprite (mopix=0x0F): prindex = 0x40 | (7<<2) | (8>>2) | (5>>3)
        //   = 0x40 | 0x1C | 0x02 | 0x01 = 0x5F
        sys.board.pri_prom[0x5F] = 0x00; // select bitmap, no bit 4

        // Render scanline 24 (first visible)
        sys.board.render_scanline_to_buffer(24);

        // Screen Y = 24 - 24 = 0. Pixel 0 should be white.
        assert_eq!(sys.board.scanline_buffer[0], 255); // R
        assert_eq!(sys.board.scanline_buffer[1], 255); // G
        assert_eq!(sys.board.scanline_buffer[2], 255); // B
    }

    #[test]
    fn scanline_skips_vblank() {
        let mut sys = CrystalCastlesSystem::new();
        sys.board.sync_prom[10] = 0x01; // VBLANK active

        // Fill scanline buffer with a known pattern
        sys.board.scanline_buffer.fill(0xAA);

        // Rendering a VBLANK scanline should not modify the buffer
        sys.board.render_scanline_to_buffer(10);
        assert_eq!(sys.board.scanline_buffer[0], 0xAA);
    }
}
