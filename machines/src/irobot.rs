//! Atari I, Robot (1983) — the first arcade game rendered with real-time
//! filled 3D polygons.
//!
//! Hardware (per MAME `src/mame/atari/irobot{,_m,_v}.cpp`):
//! - Main CPU: MC6809E @ 12.096 MHz / 8 = 1.512 MHz
//! - Mathbox: a microcoded AM2901 bit-slice coprocessor (3D transform + display
//!   list generation) — *not* the fixed-function Battlezone/Tempest mathbox
//! - Video: a custom TTL polygon rasterizer into a double-buffered 256×232
//!   6-bit bitmap, plus a 32×32 alphanumeric tile overlay
//! - Sound: 4× POKEY @ 1.512 MHz (quad-pokey, mixed to mono)
//! - ADC0809 8-channel ADC for the analog flight stick; X2212 NOVRAM for scores
//! - Heavy ROM / RAM / mathbox / comm-RAM bank switching
//!
//! Status: complete and playable. The 6809 with full ROM/RAM bank switching,
//! the 32V scanline IRQ, inputs, DIP switches, and X2212 NVRAM; the alphanumeric
//! text layer; the AM2901 microcoded mathbox with its paged 0x2000-0x3FFF window
//! and completion FIRQ; the double-buffered polygon rasterizer composited under
//! the text layer; four POKEYs mixed to mono; and the self-centering analog
//! flight stick via the ADC0809 are all implemented and verified on the real ROM.

use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::input::{AnalogAxis, AxisRange};
use phosphor_core::core::machine::{
    ActionRole, AnalogAxisKind, AudioSource, DefaultBinding, DipApplyTiming, DipChoice, DipOption,
    DipSwitchBank, Direction, InputConfigurable, InputControl, InputEvent, InputId, InputKind,
    MachineCore, MouseControl, Nvram, PadAxis, PadControl, Profilable, Renderable, SaveState,
};
use phosphor_core::core::save_state::{self, SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::m6809::M6809;
use phosphor_core::device::adc0809::Adc0809;
use phosphor_core::device::irobot_mathbox::IrobotMathbox;
use phosphor_core::device::pokey::Pokey;
use phosphor_core::device::x2212::X2212;
use phosphor_core::gfx::decode::{GfxCache, GfxLayout, decode_gfx};
use phosphor_macros::{BusDebug, MemoryRegion};

use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};

// ---------------------------------------------------------------------------
// Memory map region IDs
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
enum Region {
    Ram = 1,       // 0x0000-0x07FF  fixed 2K work RAM
    BankedRam = 2, // 0x0800-0x0FFF  3 × 2K banked RAM (backing 0x1800)
    VideoRam = 3,  // 0x1C00-0x1FFF  32×32 alphanumeric RAM
    BankedRom = 5, // 0x4000-0x5FFF  6 × 8K banked ROM (backing 0xC000)
    Rom = 6,       // 0x6000-0xFFFF  40K fixed program ROM
                   // 0x2000-0x3FFF is a paged window into the mathbox (not a region).
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------
// Main 6809E: 12.096 MHz / 8 = 1.512 MHz. Horizontal frequency ≈ 15.75 kHz
// (pixel clock 6.048 MHz / HTOTAL 384) ⇒ 96 CPU cycles per scanline. The video
// line counter is 9-bit; the maskable IRQ follows the 32V line (bit 5). Display
// is 256×232 (visarea), upright (ROT0). VTOTAL chosen for ≈60 Hz.

pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 1_512_000,
    cycles_per_scanline: 96,
    total_scanlines: 262,
    display_width: 256,
    display_height: 232,
    display_aspect: Some((4, 3)),
};

const VBLANK_LINE: u64 = 224; // status VBLANK flag set at line 224, cleared at 0

// Polygon bitmap geometry. The generator draws into a 256×256 8-bit buffer
// (MAME `BITMAP_WIDTH` × screen height); only the top 232 rows are displayed.
const BITMAP_W: usize = 256;
const BITMAP_H: usize = 256;

// Sound: four POKEYs clocked at the 6809 rate; mixed to mono at 44.1 kHz.
const POKEY_CLOCK: u32 = 1_512_000;
const SAMPLE_RATE: u32 = 44_100;

// ---------------------------------------------------------------------------
// ROM definitions ("irobot" parent set)
// ---------------------------------------------------------------------------

/// Main 6809 program: 40K fixed ROM at 0x6000-0xFFFF (loaded from region offset
/// 0x6000) plus 48K banked ROM at 0x4000-0x5FFF (region offset 0x10000).
pub static IROBOT_MAINCPU_ROM: RomRegion = RomRegion {
    size: 0x1c000,
    entries: &[
        RomEntry {
            name: "136029-208.bin",
            size: 0x2000,
            offset: 0x06000,
            crc32: &[0xb4d0be59],
        },
        RomEntry {
            name: "136029-209.bin",
            size: 0x4000,
            offset: 0x08000,
            crc32: &[0xf6be3cd0],
        },
        RomEntry {
            name: "136029-210.bin",
            size: 0x4000,
            offset: 0x0c000,
            crc32: &[0xc0eb2133],
        },
        RomEntry {
            name: "136029-405.bin",
            size: 0x4000,
            offset: 0x10000,
            crc32: &[0x9163efe4],
        },
        RomEntry {
            name: "136029-206.bin",
            size: 0x4000,
            offset: 0x14000,
            crc32: &[0xe114a526],
        },
        RomEntry {
            name: "136029-207.bin",
            size: 0x4000,
            offset: 0x18000,
            crc32: &[0xb4556cb0],
        },
    ],
};

/// Alphanumeric character set: 64 chars, 8×8 1bpp.
pub static IROBOT_ALPHA_ROM: RomRegion = RomRegion {
    size: 0x800,
    entries: &[RomEntry {
        name: "136029-124.bin",
        size: 0x800,
        offset: 0x0000,
        crc32: &[0x848948b6],
    }],
};

/// PROMs: a 32-byte text-color PROM at offset 0, followed by the 13 mathbox
/// microcode PROMs.
pub static IROBOT_PROMS: RomRegion = RomRegion {
    size: 0x3420,
    entries: &[
        RomEntry {
            name: "136029-125.bin",
            size: 0x0020,
            offset: 0x0000,
            crc32: &[0x446335ba],
        },
        RomEntry {
            name: "136029-111.bin",
            size: 0x0400,
            offset: 0x0020,
            crc32: &[0x9fbc9bf3],
        },
        RomEntry {
            name: "136029-112.bin",
            size: 0x0400,
            offset: 0x0420,
            crc32: &[0xb2713214],
        },
        RomEntry {
            name: "136029-113.bin",
            size: 0x0400,
            offset: 0x0820,
            crc32: &[0x7875930a],
        },
        RomEntry {
            name: "136029-114.bin",
            size: 0x0400,
            offset: 0x0c20,
            crc32: &[0x51d29666],
        },
        RomEntry {
            name: "136029-115.bin",
            size: 0x0400,
            offset: 0x1020,
            crc32: &[0x00f9b304],
        },
        RomEntry {
            name: "136029-116.bin",
            size: 0x0400,
            offset: 0x1420,
            crc32: &[0x326aba54],
        },
        RomEntry {
            name: "136029-117.bin",
            size: 0x0400,
            offset: 0x1820,
            crc32: &[0x98efe8d0],
        },
        RomEntry {
            name: "136029-118.bin",
            size: 0x0400,
            offset: 0x1c20,
            crc32: &[0x4a6aa7f9],
        },
        RomEntry {
            name: "136029-119.bin",
            size: 0x0400,
            offset: 0x2020,
            crc32: &[0xa5a13ad8],
        },
        RomEntry {
            name: "136029-120.bin",
            size: 0x0400,
            offset: 0x2420,
            crc32: &[0x2a083465],
        },
        RomEntry {
            name: "136029-121.bin",
            size: 0x0400,
            offset: 0x2820,
            crc32: &[0xadebcb99],
        },
        RomEntry {
            name: "136029-122.bin",
            size: 0x0400,
            offset: 0x2c20,
            crc32: &[0xda7b6f79],
        },
        RomEntry {
            name: "136029-123.bin",
            size: 0x0400,
            offset: 0x3020,
            crc32: &[0x39fff18f],
        },
    ],
};

// Mathbox ROM: four chips interleaved big-endian (`ROM_LOAD16_BYTE`). 104/103
// are the high/low bytes of words 0x0000-0x1FFF; 102/101 the high/low bytes of
// words 0x2000-0x5FFF. Assembled into 0x6000 16-bit words by `load_mathbox_rom`.
static IROBOT_MB_HI_LO: RomRegion = RomRegion {
    size: 0xc000,
    entries: &[
        RomEntry {
            name: "136029-104.bin",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x0a6cdcca],
        },
        RomEntry {
            name: "136029-103.bin",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x0c83296d],
        },
        RomEntry {
            name: "136029-102.bin",
            size: 0x4000,
            offset: 0x4000,
            crc32: &[0x9d588f22],
        },
        RomEntry {
            name: "136029-101.bin",
            size: 0x4000,
            offset: 0x8000,
            crc32: &[0x62a38c08],
        },
    ],
};

/// Assemble the mathbox ROM into 0x6000 big-endian 16-bit words. The four files
/// are loaded contiguously (104,103,102,101) then interleaved: words
/// 0x0000-0x1FFF from 104(hi)/103(lo), words 0x2000-0x5FFF from 102(hi)/101(lo).
fn load_mathbox_rom(rom_set: &RomSet) -> Result<Vec<u16>, RomLoadError> {
    let raw = IROBOT_MB_HI_LO.load(rom_set)?;
    let (hi0, lo0) = (&raw[0x0000..0x2000], &raw[0x2000..0x4000]);
    let (hi1, lo1) = (&raw[0x4000..0x8000], &raw[0x8000..0xc000]);
    let mut words = vec![0u16; 0x6000];
    for i in 0..0x2000 {
        words[i] = ((hi0[i] as u16) << 8) | lo0[i] as u16;
    }
    for i in 0..0x4000 {
        words[0x2000 + i] = ((hi1[i] as u16) << 8) | lo1[i] as u16;
    }
    Ok(words)
}

/// 8×8 1bpp character layout (MAME `charlayout`): each row is two bytes, the
/// pixels come from bits 4..7 of each byte; 16 bytes per character.
const CHAR_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0],
    x_offsets: &[4, 5, 6, 7, 12, 13, 14, 15],
    y_offsets: &[0, 16, 32, 48, 64, 80, 96, 112],
    char_increment: 16 * 8,
};

// ---------------------------------------------------------------------------
// Input control IDs
// ---------------------------------------------------------------------------
const INPUT_COIN1: u16 = 0;
const INPUT_COIN2: u16 = 1;
const INPUT_COIN3: u16 = 2;
const INPUT_SERVICE: u16 = 3;
const INPUT_START1: u16 = 4;
const INPUT_START2: u16 = 5;
const INPUT_FIRE: u16 = 6;
const INPUT_BUTTON2: u16 = 7;
// Analog flight-stick axes (distinct InputId space from the digital controls).
const INPUT_STICK_X: u16 = 8;
const INPUT_STICK_Y: u16 = 9;
// Digital direction keys that drive the self-centering stick.
const INPUT_STICK_LEFT: u16 = 10;
const INPUT_STICK_RIGHT: u16 = 11;
const INPUT_STICK_UP: u16 = 12;
const INPUT_STICK_DOWN: u16 = 13;

// Analog stick channel ranges (MAME AN0/AN1 PORT_MINMAX), centered at 0x80.
const STICK_CENTER: i32 = 0x80;
const STICK_X_MIN: i32 = 96;
const STICK_X_MAX: i32 = 159;
const STICK_Y_MIN: i32 = 96;
const STICK_Y_MAX: i32 = 163;

const IROBOT_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_FIRE),
        stable_name: "p1_fire",
        label: "Fire",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        // Action role supplies the default key/pad binding (LShift + pad A).
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_BUTTON2),
        stable_name: "p1_button2",
        label: "Button 2",
        kind: InputKind::Action(ActionRole::Secondary),
        player: Some(1),
        // Action role supplies the default key/pad binding (Space + pad B).
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_START1),
        stable_name: "p1_start",
        label: "P1 Start",
        kind: InputKind::Start,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_START,
    },
    InputControl {
        id: InputId(INPUT_START2),
        stable_name: "p2_start",
        label: "P2 Start",
        kind: InputKind::Start,
        player: Some(2),
        default_bindings: crate::input_defaults::P2_START,
    },
    InputControl {
        id: InputId(INPUT_COIN1),
        stable_name: "coin1",
        label: "Coin 1",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(INPUT_COIN2),
        stable_name: "coin2",
        label: "Coin 2",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_COIN3),
        stable_name: "coin3",
        label: "Aux Coin",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_SERVICE),
        stable_name: "service",
        label: "Self-Test",
        kind: InputKind::Service,
        player: None,
        default_bindings: crate::input_defaults::SERVICE,
    },
    InputControl {
        id: InputId(INPUT_STICK_X),
        stable_name: "stick_x",
        label: "Stick X",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::X,
        },
        player: Some(1),
        // Right stick, not left: the shared direction defaults (P1_LEFT/RIGHT)
        // already bind LeftX as signed digital, and `update_stick` assigns the
        // axis absolutely — one stick driving both would fight itself.
        default_bindings: &[
            DefaultBinding::Mouse(MouseControl::AxisX),
            DefaultBinding::Pad(PadControl::FullAxis(PadAxis::RightX)),
        ],
    },
    InputControl {
        id: InputId(INPUT_STICK_Y),
        stable_name: "stick_y",
        label: "Stick Y",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::Y,
        },
        player: Some(1),
        default_bindings: &[
            DefaultBinding::Mouse(MouseControl::AxisY),
            DefaultBinding::Pad(PadControl::FullAxis(PadAxis::RightY)),
        ],
    },
    // Digital direction keys (arrow keys / D-pad) drive the self-centering
    // stick: holding one deflects the axis; releasing returns it to center.
    InputControl {
        id: InputId(INPUT_STICK_LEFT),
        stable_name: "stick_left",
        label: "Stick Left",
        kind: InputKind::DigitalDirection {
            direction: Direction::Left,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(INPUT_STICK_RIGHT),
        stable_name: "stick_right",
        label: "Stick Right",
        kind: InputKind::DigitalDirection {
            direction: Direction::Right,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
    InputControl {
        id: InputId(INPUT_STICK_UP),
        stable_name: "stick_up",
        label: "Stick Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_UP,
    },
    InputControl {
        id: InputId(INPUT_STICK_DOWN),
        stable_name: "stick_down",
        label: "Stick Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_DOWN,
    },
];

// ---------------------------------------------------------------------------
// System
// ---------------------------------------------------------------------------

/// Atari I, Robot (1983).
#[derive(BusDebug)]
pub struct IrobotSystem {
    #[debug_cpu("M6809")]
    cpu: M6809,

    #[debug_map(cpu = 0)]
    map: AddressSpace16,

    // GFX + palettes.
    char_cache: GfxCache,             // 64 × 8×8 1bpp alphanumeric chars
    text_palette: [(u8, u8, u8); 32], // text-layer RGB (PROM 136029-125)
    poly_palette: [(u8, u8, u8); 64], // polygon RGB (written via paletteram_w)

    // PROMs retained for later phases (text-color PROM + mathbox microcode).
    proms: Vec<u8>, // 0x3420

    // AM2901 microcoded mathbox (owns the paged 0x2000-0x3FFF window memories).
    mathbox: IrobotMathbox,

    // Polygon video: two 256×256 8-bit (palette-index) buffers, double-buffered.
    polybitmap: [Vec<u8>; 2],
    bufsel: u8,         // current draw buffer (statwr bit 1); displayed = bufsel^1
    vg_clear: bool,     // polygon-clear latch (statwr bit 0)
    commbank: u8,       // comm-RAM bank read by the polygon generator (statwr bit 7)
    irvg_running: bool, // polygon generator busy flag (status bit 6)

    // Control registers / bank latches.
    out0: u8,       // 0x1180: RAM bank, mathbox page/bank, alphamap (bit 7)
    statwr: u8,     // 0x1140: polygon/mathbox control (edge-detected)
    rombanksel: u8, // 0x11C0: ROM bank select

    // Inputs (active low: 1 = released) + DIP banks.
    in0: u8,
    in1: u8,
    dsw1: u8,
    dsw2: u8,

    // NVRAM.
    novram: X2212,

    // Analog flight stick: ADC0809 (channel 0 = Y, channel 1 = X) and the
    // current [X, Y] raw stick positions feeding it. `dir_held` tracks the four
    // digital direction keys [left, right, up, down] for self-centering.
    adc: Adc0809,
    stick: [AnalogAxis; 2],

    // Sound: four POKEYs @ 1.512 MHz, all outputs summed to mono.
    pokeys: [Pokey; 4],
    audio_buffer: Vec<i16>,

    // Interrupts / timing.
    irq_pending: bool,
    firq_pending: bool,
    prev_v32: bool,
    clock: u64,
}

/// I, Robot's two stick channels. Their electrical ranges are genuinely
/// asymmetric about the 0x80 rest position — X spans 96..159, Y 96..163 — so an
/// absolute deflection scales by whichever side it is heading toward, which is
/// exactly AnalogAxis::set_absolute's contract.
fn new_stick() -> [AnalogAxis; 2] {
    [
        AnalogAxis::new(AxisRange::new(STICK_X_MIN, STICK_CENTER, STICK_X_MAX)),
        AnalogAxis::new(AxisRange::new(STICK_Y_MIN, STICK_CENTER, STICK_Y_MAX)),
    ]
}

impl Default for IrobotSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl IrobotSystem {
    pub fn new() -> Self {
        let mut sys = Self {
            cpu: M6809::new(),
            map: Self::build_map(),
            char_cache: GfxCache::new(0, 8, 8),
            text_palette: [(0, 0, 0); 32],
            poly_palette: [(0, 0, 0); 64],
            proms: Vec::new(),
            mathbox: IrobotMathbox::new(),
            polybitmap: [vec![0; BITMAP_W * BITMAP_H], vec![0; BITMAP_W * BITMAP_H]],
            bufsel: 0,
            vg_clear: false,
            commbank: 0,
            irvg_running: false,
            out0: 0,
            statwr: 0,
            rombanksel: 0,
            in0: 0xFF,
            in1: 0xFF,
            dsw1: DSW1_DEFAULT,
            dsw2: DSW2_DEFAULT,
            novram: X2212::new(),
            adc: Adc0809::new(),
            stick: new_stick(),
            pokeys: std::array::from_fn(|_| Pokey::with_clock(POKEY_CLOCK, SAMPLE_RATE)),
            audio_buffer: Vec::with_capacity(2048),
            irq_pending: false,
            firq_pending: false,
            prev_v32: false,
            clock: 0,
        };
        // Seed the ADC with the centered stick so the analog axes read neutral
        // before any input even if the host never calls reset().
        sys.update_adc_inputs();
        sys
    }

    fn build_map() -> AddressSpace16 {
        use Region::*;
        let mut map = AddressSpace16::new();
        map.region(Ram, "Fixed RAM", 0x0000, 0x0800, AccessKind::ReadWrite)
            .backing_region(BankedRam, "Banked RAM", 0x1800)
            .region(VideoRam, "Video RAM", 0x1c00, 0x0400, AccessKind::ReadWrite)
            .backing_region(BankedRom, "Banked ROM", 0xc000)
            .region(Rom, "Program ROM", 0x6000, 0xa000, AccessKind::ReadOnly);
        // Point the banked windows at bank 0.
        map.remap_pages(0x08, 8, BankedRam, 0);
        map.remap_pages(0x40, 0x20, BankedRom, 0);
        map
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let main = IROBOT_MAINCPU_ROM.load(rom_set)?;
        self.map.load_region(Region::Rom, &main[0x6000..0x10000]);
        self.map
            .load_region(Region::BankedRom, &main[0x10000..0x1c000]);

        let alpha = IROBOT_ALPHA_ROM.load(rom_set)?;
        self.char_cache = decode_gfx(&alpha, 0, 64, &CHAR_LAYOUT);

        self.proms = IROBOT_PROMS.load(rom_set)?;
        self.build_text_palette();

        // Mathbox: assembled big-endian ROM + the microcode PROMs (the bytes
        // after the 32-byte text-color PROM).
        let mathbox_rom = load_mathbox_rom(rom_set)?;
        self.mathbox.load(&self.proms[0x20..], &mathbox_rom);

        Ok(())
    }

    /// Build the 32-entry text palette from the 32-byte color PROM
    /// (`irobot_state::palette` in `irobot_v.cpp`). 3×2-bit RGB scaled by a
    /// 2-bit intensity; the entry index is bit-swapped so a tile's pen offset
    /// (`color*2 + pixel`) indexes directly.
    fn build_text_palette(&mut self) {
        for i in 0..32usize {
            let v = self.proms.get(i).copied().unwrap_or(0);
            let intensity = (v & 0x03) as u32;
            let r = (28 * ((v >> 6) & 0x03) as u32 * intensity) as u8;
            let g = (28 * ((v >> 4) & 0x03) as u32 * intensity) as u8;
            let b = (28 * ((v >> 2) & 0x03) as u32 * intensity) as u8;
            // MAME bitswap<8>(i,7,6,5,4,3,0,1,2): swap bit0 and bit2 of low 3 bits.
            let swapped = (i & 0xF8) | ((i & 1) << 2) | (i & 2) | ((i >> 2) & 1);
            self.text_palette[swapped] = (r, g, b);
        }
    }

    /// Write the polygon color RAM (`paletteram_w`): 9-bit value (8 data bits +
    /// address LSB), inverted, decoded as 3×2-bit RGB scaled by a 3-bit
    /// intensity. Consumed by the polygon layer in `render`.
    fn paletteram_w(&mut self, offset: u16, data: u8) {
        let color = (((data as u32) << 1) | (offset as u32 & 0x01)) ^ 0x1ff;
        let intensity = color & 0x07;
        let b = (12 * ((color >> 3) & 0x03) * intensity) as u8;
        let g = (12 * ((color >> 5) & 0x03) * intensity) as u8;
        let r = (12 * ((color >> 7) & 0x03) * intensity) as u8;
        self.poly_palette[((offset >> 1) & 0x3f) as usize] = (r, g, b);
    }

    /// Current video line (0..VTOTAL).
    fn scanline(&self) -> u64 {
        (self.clock % TIMING.cycles_per_frame()) / TIMING.cycles_per_scanline
    }

    /// Status register at 0x1080: bit 5 = mathbox done, bit 6 = polygon
    /// generator running, bit 7 = VBLANK. Like MAME's non-timing build, reading
    /// the mathbox-done and generator-running bits clears their flip-flops (so a
    /// started run reads busy exactly once).
    fn status_r(&mut self) -> u8 {
        let mut d = 0;
        if !self.mathbox.running() {
            d |= 0x20;
        }
        self.mathbox.clear_running();
        if self.irvg_running {
            d |= 0x40;
        }
        self.irvg_running = false;
        if self.scanline() >= VBLANK_LINE {
            d |= 0x80;
        }
        d
    }

    /// 0x1140 polygon/mathbox control (MAME `statwr_w`): bit 7 selects the
    /// comm-RAM bank; bit 1 selects the polygon draw buffer; bit 0 is the
    /// polygon-clear latch (rising edge clears the current draw buffer); a rising
    /// edge on bit 2 runs the polygon generator; a rising edge on bit 4 runs the
    /// mathbox + raises the completion FIRQ; bit 6 drives NOVRAM RECALL (active
    /// low).
    fn statwr_w(&mut self, data: u8) {
        self.commbank = (data >> 7) & 1;
        self.mathbox.set_commbank(self.commbank);
        self.bufsel = (data >> 1) & 1;
        if data & 0x01 != 0 && !self.vg_clear {
            self.poly_clear(self.bufsel as usize);
        }
        self.vg_clear = data & 0x01 != 0;
        if data & 0x04 != 0 && self.statwr & 0x04 == 0 {
            self.run_video();
            self.irvg_running = true;
        }
        if data & 0x10 != 0 && self.statwr & 0x10 == 0 {
            self.mathbox.run();
            self.firq_pending = true;
        }
        self.novram.recall(data & 0x40 == 0);
        self.statwr = data;
    }

    /// 0x1180 output latch: RAM bank (bits 6-5), mathbox memory select (bits
    /// 4-3) and bank (bits 2-1), alphamap (bit 7).
    fn out0_w(&mut self, data: u8) {
        self.out0 = data;
        if data & 0x60 != 0x60 {
            let bank = ((data & 0x60) >> 5) as u32;
            self.map
                .remap_pages(0x08, 8, Region::BankedRam, bank * 0x800);
        }
        self.mathbox.set_outx((data & 0x18) >> 3);
        self.mathbox.set_mpage((data & 0x06) >> 1);
    }

    /// 0x11C0 ROM bank select (bits 3-1 select one of six 8K banks).
    fn rom_banksel_w(&mut self, data: u8) {
        self.rombanksel = data;
        if data & 0x0e < 0x0c {
            let bank = ((data & 0x0e) >> 1) as u32;
            self.map
                .remap_pages(0x40, 0x20, Region::BankedRom, bank * 0x2000);
        }
    }

    /// Re-apply the live bank selection (after a state load, which restores the
    /// latches but not the page table).
    fn apply_banking(&mut self) {
        let ram_bank = if self.out0 & 0x60 != 0x60 {
            ((self.out0 & 0x60) >> 5) as u32
        } else {
            0
        };
        self.map
            .remap_pages(0x08, 8, Region::BankedRam, ram_bank * 0x800);
        let rom_bank = if self.rombanksel & 0x0e < 0x0c {
            ((self.rombanksel & 0x0e) >> 1) as u32
        } else {
            0
        };
        self.map
            .remap_pages(0x40, 0x20, Region::BankedRom, rom_bank * 0x2000);
    }

    /// Drive the self-centering stick from the held direction keys: a held
    /// direction deflects the axis to its range limit; releasing returns it to
    /// center. `dir_held` is `[left, right, up, down]`.
    fn update_stick(&mut self) {
        self.update_adc_inputs();
    }

    /// Push the raw stick positions onto the ADC inputs. Channel 0 = Y (AN0,
    /// direct); channel 1 = X (AN1), which MAME drives PORT_REVERSE — reflect it
    /// around the 0x80 center. `stick` is `[X, Y]`.
    fn update_adc_inputs(&mut self) {
        self.adc.set_input(0, self.stick[1].position() as u8);
        self.adc
            .set_input(1, (2 * STICK_CENTER - self.stick[0].position()) as u8);
    }

    /// Absolute deflection in `-1.0..=1.0` mapped to the channel range (centered
    /// at 0x80). `axis` is 0 = X, 1 = Y.
    fn set_stick_abs(&mut self, axis: usize, value: f32, _min: i32, _max: i32) {
        self.stick[axis].set_absolute(value);
        self.update_adc_inputs();
    }

    /// Relative (mouse) motion accumulated into the stick position and clamped.
    fn move_stick_rel(&mut self, axis: usize, delta: f32, _min: i32, _max: i32) {
        self.stick[axis].move_relative(delta);
        self.update_adc_inputs();
    }

    /// Decode a quad-POKEY access (`quad_pokeyn_r/w`): which POKEY (0-3) and
    /// register (0-15) the window offset selects.
    fn quad_pokey_decode(offset: u16) -> (usize, u16) {
        let pokey_num = ((offset >> 3) & !0x04) as usize;
        let reg = (offset & 7) | ((offset & 0x20) >> 2);
        (pokey_num, reg)
    }

    /// Quad-POKEY read. POKEY 0's ALLPOT register is wired directly to DSW2
    /// (MAME `allpot_r().set_ioport("DSW2")`); everything else comes from the
    /// POKEY register file.
    fn quad_pokey_r(&mut self, offset: u16) -> u8 {
        let (num, reg) = Self::quad_pokey_decode(offset);
        if num == 0 && reg == 8 {
            self.dsw2
        } else {
            self.pokeys[num].read(reg)
        }
    }

    /// Quad-POKEY write.
    fn quad_pokey_w(&mut self, offset: u16, data: u8) {
        let (num, reg) = Self::quad_pokey_decode(offset);
        self.pokeys[num].write(reg, data);
    }

    pub fn tick(&mut self) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();

        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let scanline = frame_cycle / TIMING.cycles_per_scanline;
            // Maskable IRQ follows 32V (line counter bit 5); assert on its
            // rising edge and let the handler clear it via 0x1100.
            let v32 = scanline & 32 != 0;
            if v32 && !self.prev_v32 {
                self.irq_pending = true;
            }
            self.prev_v32 = v32;
        }

        if self.map.has_any_watchpoints() {
            let pc = self
                .cpu
                .at_instruction_boundary()
                .then_some(self.cpu.pc as u32);
            self.map.latch_access_context(self.clock, pc);
        }

        bus_split!(self, bus => {
            self.cpu.execute_cycle(bus, BusMaster::Cpu(0));
        });

        // The four POKEYs run at the CPU clock (1:1).
        for p in &mut self.pokeys {
            p.tick();
        }

        self.clock += 1;
    }

    /// Drain the four POKEYs' resampled output and mix it to mono (0.25 each,
    /// matching MAME's routing). Called once per frame.
    fn mix_audio(&mut self) {
        let chans: [Vec<f32>; 4] = std::array::from_fn(|k| self.pokeys[k].drain_audio());
        let [c0, c1, c2, c3] = &chans;
        for (((a, b), c), d) in c0.iter().zip(c1).zip(c2).zip(c3) {
            let sum = a + b + c + d;
            self.audio_buffer.push((sum * 0.25 * 32767.0) as i16);
        }
    }

    /// Clear a polygon draw buffer to the background pen.
    fn poly_clear(&mut self, buf: usize) {
        self.polybitmap[buf].fill(0);
    }

    /// Bresenham line into the polygon bitmap, clipped to the 256×256 window
    /// (MAME `draw_line`).
    fn draw_line(bitmap: &mut [u8], x1: i32, y1: i32, x2: i32, y2: i32, col: u8) {
        let dx = (x1 - x2).abs();
        let dy = (y1 - y2).abs();
        let sx = if x1 <= x2 { 1 } else { -1 };
        let sy = if y1 <= y2 { 1 } else { -1 };
        let (mut x, mut y) = (x1, y1);
        let mut cx = dx / 2;
        let mut cy = dy / 2;
        let plot = |bitmap: &mut [u8], x: i32, y: i32| {
            if (0..BITMAP_W as i32).contains(&x) && (0..BITMAP_H as i32).contains(&y) {
                bitmap[((y << 8) + x) as usize] = col;
            }
        };
        if dx >= dy {
            loop {
                plot(bitmap, x, y);
                if x == x2 {
                    break;
                }
                x += sx;
                cx -= dy;
                if cx < 0 {
                    y += sy;
                    cx += dx;
                }
            }
        } else {
            loop {
                plot(bitmap, x, y);
                if y == y2 {
                    break;
                }
                y += sy;
                cy -= dx;
                if cy < 0 {
                    x += sx;
                    cy += dy;
                }
            }
        }
    }

    /// Run the polygon generator (MAME `run_video`): walk the mathbox-built
    /// display list in comm RAM `commbank` and rasterize points / vectors /
    /// filled polygons into the current draw buffer `bufsel`.
    fn run_video(&mut self) {
        const XMAX: i32 = BITMAP_W as i32;
        const YMAX: i32 = BITMAP_H as i32;
        let round = |x: i32| (x >> 7) - 128;
        let sext = |v: i32| if v >= 0x8000 { v - 0x10000 } else { v };

        // Disjoint field borrows: the draw buffer (mut) and the comm RAM (shared).
        let bitmap = &mut self.polybitmap[self.bufsel as usize];
        let comram = self.mathbox.comram(self.commbank as usize);
        // 11-bit (0x800-word) comm-RAM address space wraps, matching the hardware.
        let cw = |i: i32| comram[i as usize & 0x7ff] as i32;

        let mut lpnt: i32 = 0;
        while lpnt < 0x7ff {
            let d1 = cw(lpnt);
            lpnt += 1;
            if d1 == 0xffff {
                break;
            }
            let mut spnt = d1 & 0x07ff;
            match (d1 & 0xf000) >> 12 {
                // Point objects.
                0x8 => {
                    while spnt < 0x7ff {
                        let raw_x = cw(spnt);
                        if raw_x == 0xffff {
                            break;
                        }
                        let raw_y = cw(spnt + 1);
                        let color = (raw_y & 0x3f) as u8;
                        let (x, y) = (round(raw_x), round(raw_y));
                        if (0..XMAX).contains(&x) && (0..YMAX).contains(&y) {
                            bitmap[((y << 8) + x) as usize] = color;
                        }
                        spnt += 2;
                    }
                }
                // Vector (line) objects.
                0xc => {
                    while spnt < 0x7ff {
                        let raw_ey = cw(spnt);
                        if raw_ey == 0xffff {
                            break;
                        }
                        let ey = round(raw_ey);
                        let raw_sy = cw(spnt + 1);
                        let color = (raw_sy & 0x3f) as u8;
                        let sy = round(raw_sy);
                        let sx = cw(spnt + 3);
                        let word1 = sext(cw(spnt + 2));
                        let ex = sx + word1 * (ey - sy + 1);
                        Self::draw_line(bitmap, round(sx), sy, round(ex), ey, color);
                        spnt += 4;
                    }
                }
                // Filled polygon: two slope lists advanced in lockstep, the span
                // between the left/right edges filled on each scanline.
                0x4 => {
                    let mut spnt2 = cw(spnt) & 0x7ff;
                    let mut sx = cw(spnt + 1);
                    let mut sx2 = cw(spnt + 2);
                    let raw_sy = cw(spnt + 3);
                    let color = (raw_sy & 0x3f) as u8;
                    let mut sy = round(raw_sy);
                    spnt += 4;

                    let mut word1 = sext(cw(spnt));
                    let mut ey = cw(spnt + 1);
                    if word1 != -1 || ey != 0xffff {
                        ey = round(ey);
                        spnt += 2;
                        let mut word2 = sext(cw(spnt2));
                        let mut ey2 = round(cw(spnt2 + 1));
                        spnt2 += 2;
                        loop {
                            if (0..YMAX).contains(&sy) {
                                let mut x1 = round(sx);
                                let mut x2 = round(sx2);
                                if x1 > x2 {
                                    std::mem::swap(&mut x1, &mut x2);
                                }
                                x1 = x1.max(0);
                                x2 = x2.min(XMAX - 1);
                                if x1 < x2 {
                                    let start = ((sy << 8) + x1 + 1) as usize;
                                    bitmap[start..start + (x2 - x1) as usize].fill(color);
                                }
                            }
                            sy += 1;
                            if sy > ey {
                                word1 = sext(cw(spnt));
                                ey = cw(spnt + 1);
                                if word1 == -1 && ey == 0xffff {
                                    break;
                                }
                                ey = round(ey);
                                spnt += 2;
                            } else {
                                sx += word1;
                            }
                            if sy > ey2 {
                                word2 = sext(cw(spnt2));
                                ey2 = round(cw(spnt2 + 1));
                                spnt2 += 2;
                            } else {
                                sx2 += word2;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Render the displayed polygon buffer through the polygon palette, then
    /// composite the alphanumeric tile layer (transparent pen 0) on top. Tile
    /// color = `((data & 0xC0) >> 6) | (alphamap >> 4)`; the char gfx is 1bpp.
    fn render(&self, buffer: &mut [u8]) {
        let w = TIMING.display_width as usize;
        let h = TIMING.display_height as usize;

        // Displayed polygon buffer is the one not currently being drawn into.
        let poly = &self.polybitmap[(self.bufsel ^ 1) as usize];
        for y in 0..h {
            for x in 0..w {
                let (r, g, b) = self.poly_palette[poly[(y << 8) + x] as usize & 0x3f];
                let off = (y * w + x) * 3;
                buffer[off] = r;
                buffer[off + 1] = g;
                buffer[off + 2] = b;
            }
        }

        if self.char_cache.count() == 0 {
            return;
        }
        let vram = self.map.region_data(Region::VideoRam);
        let alphamap = ((self.out0 & 0x80) >> 4) as usize;
        for ty in 0..(h / 8) {
            for tx in 0..32usize {
                let data = vram[ty * 32 + tx] as usize;
                let code = data & 0x3f;
                let color = ((data & 0xc0) >> 6) | alphamap;
                for py in 0..8 {
                    for px in 0..8 {
                        if self.char_cache.pixel(code, px, py) == 0 {
                            continue; // transparent
                        }
                        let (r, g, b) = self.text_palette[(color * 2 + 1) & 0x1f];
                        let off = ((ty * 8 + py) * w + (tx * 8 + px)) * 3;
                        buffer[off] = r;
                        buffer[off + 1] = g;
                        buffer[off + 2] = b;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Bus
// ---------------------------------------------------------------------------

impl Bus for IrobotSystem {
    type Address = u16;
    type Data = u8;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let data = match addr {
            0x0000..=0x0fff => self.map.read_backing(addr), // fixed + banked RAM
            0x1000..=0x103f => self.in0,
            0x1040..=0x107f => self.in1,
            0x1080..=0x10bf => self.status_r(),
            0x10c0..=0x10ff => self.dsw1,
            0x1200..=0x12ff => self.novram.read(addr & 0xff),
            0x1300..=0x13ff => self.adc.data_r(), // analog-stick ADC result
            0x1400..=0x143f => self.quad_pokey_r(addr & 0x3f),
            0x1c00..=0x1fff => self.map.read_backing(addr), // video RAM
            0x2000..=0x3fff => self.mathbox.sharedmem_r(addr - 0x2000), // paged mathbox window
            0x4000..=0xffff => self.map.read_backing(addr), // banked + fixed ROM
            _ => 0xff,
        };
        self.map.watch_read(0, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.map.watch_write(0, master, addr, data);
        match addr {
            0x0000..=0x0fff => self.map.write_backing(addr, data),
            0x1100..=0x113f => self.irq_pending = false, // clear IRQ
            0x1140..=0x117f => self.statwr_w(data),
            0x1180..=0x11bf => self.out0_w(data),
            0x11c0..=0x11ff => self.rom_banksel_w(data),
            0x1200..=0x12ff => self.novram.write(addr & 0xff, data),
            0x1400..=0x143f => self.quad_pokey_w(addr & 0x3f, data),
            0x1800..=0x18ff => self.paletteram_w(addr & 0xff, data),
            0x1900..=0x19ff => {} // watchdog reset (not modelled)
            0x1a00..=0x1a3f => self.firq_pending = false, // clear FIRQ
            0x1b00..=0x1bff => self.adc.address_offset_start_w(addr & 0x03), // ADC start
            0x1c00..=0x1fff => self.map.write_backing(addr, data), // video RAM
            0x2000..=0x3fff => self.mathbox.sharedmem_w(addr - 0x2000, data), // paged mathbox window
            _ => {}                                                           // ROM / unmapped
        }
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            nmi: false,
            irq: self.irq_pending,
            firq: self.firq_pending,
            irq_vector: 0,
            irq_level: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Machine traits
// ---------------------------------------------------------------------------

impl Renderable for IrobotSystem {
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

impl AudioSource for IrobotSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        let n = buffer.len().min(self.audio_buffer.len());
        buffer[..n].copy_from_slice(&self.audio_buffer[..n]);
        self.audio_buffer.drain(..n);
        n
    }
    fn audio_sample_rate(&self) -> u32 {
        SAMPLE_RATE
    }
}

impl MachineCore for IrobotSystem {
    fn gfx_sheets(&self) -> Vec<phosphor_core::core::machine::GfxSheet<'_>> {
        use phosphor_core::core::machine::GfxSheet;
        // I, Robot renders 3-D polygons into a framebuffer; its only decoded
        // GFX is the alphanumeric overlay font.
        vec![GfxSheet {
            name: "chars",
            cache: &self.char_cache,
            palette: &self.text_palette,
        }]
    }

    fn run_frame(&mut self) {
        for _ in 0..TIMING.cycles_per_frame() {
            self.tick();
        }
        self.mix_audio();
    }

    fn reset(&mut self) {
        self.out0 = 0;
        self.statwr = 0;
        self.rombanksel = 0;
        self.irq_pending = false;
        self.firq_pending = false;
        self.prev_v32 = false;
        self.clock = 0;
        self.novram.reset();
        self.mathbox.reset();
        self.adc.reset();
        self.stick = new_stick();
        self.update_adc_inputs();
        for p in &mut self.pokeys {
            p.reset();
        }
        self.audio_buffer.clear();
        self.bufsel = 0;
        self.vg_clear = false;
        self.commbank = 0;
        self.irvg_running = false;
        self.polybitmap[0].fill(0);
        self.polybitmap[1].fill(0);
        self.map.region_data_mut(Region::Ram).fill(0);
        self.map.region_data_mut(Region::BankedRam).fill(0);
        self.map.region_data_mut(Region::VideoRam).fill(0);
        self.apply_banking();
        bus_split!(self, bus => {
            self.cpu.reset(bus, BusMaster::Cpu(0));
        });
    }

    fn frame_rate_hz(&self) -> f64 {
        TIMING.frame_rate_hz()
    }

    fn machine_id(&self) -> &str {
        "irobot"
    }
}

impl Saveable for IrobotSystem {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        w.write_bytes(self.map.region_data(Region::Ram));
        w.write_bytes(self.map.region_data(Region::BankedRam));
        w.write_bytes(self.map.region_data(Region::VideoRam));
        self.novram.save_state(w);
        self.mathbox.save_state(w);
        self.adc.save_state(w);
        for axis in &self.stick {
            w.write_u8(axis.position() as u8);
        }
        for p in &self.pokeys {
            p.save_state(w);
        }
        w.write_bytes(&self.polybitmap[0]);
        w.write_bytes(&self.polybitmap[1]);
        w.write_u8(self.bufsel);
        w.write_bool(self.vg_clear);
        w.write_u8(self.commbank);
        w.write_bool(self.irvg_running);
        w.write_u8(self.out0);
        w.write_u8(self.statwr);
        w.write_u8(self.rombanksel);
        w.write_u8(self.in0);
        w.write_u8(self.in1);
        w.write_u8(self.dsw1);
        w.write_u8(self.dsw2);
        w.write_bool(self.irq_pending);
        w.write_bool(self.firq_pending);
        w.write_bool(self.prev_v32);
        w.write_u64_le(self.clock);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        r.read_bytes_into(self.map.region_data_mut(Region::Ram))?;
        r.read_bytes_into(self.map.region_data_mut(Region::BankedRam))?;
        r.read_bytes_into(self.map.region_data_mut(Region::VideoRam))?;
        self.novram.load_state(r)?;
        self.mathbox.load_state(r)?;
        self.adc.load_state(r)?;
        for axis in &mut self.stick {
            axis.set_position(r.read_u8()? as i32);
        }
        for p in &mut self.pokeys {
            p.load_state(r)?;
        }
        r.read_bytes_into(&mut self.polybitmap[0])?;
        r.read_bytes_into(&mut self.polybitmap[1])?;
        self.bufsel = r.read_u8()?;
        self.vg_clear = r.read_bool()?;
        self.commbank = r.read_u8()?;
        self.irvg_running = r.read_bool()?;
        self.out0 = r.read_u8()?;
        self.statwr = r.read_u8()?;
        self.rombanksel = r.read_u8()?;
        self.in0 = r.read_u8()?;
        self.in1 = r.read_u8()?;
        self.dsw1 = r.read_u8()?;
        self.dsw2 = r.read_u8()?;
        self.irq_pending = r.read_bool()?;
        self.firq_pending = r.read_bool()?;
        self.prev_v32 = r.read_bool()?;
        self.clock = r.read_u64_le()?;
        self.apply_banking();
        Ok(())
    }
}

impl SaveState for IrobotSystem {
    fn save_state(&self) -> Option<Vec<u8>> {
        Some(save_state::save_machine(self, self.machine_id()))
    }
    fn load_state(&mut self, data: &[u8]) -> Result<(), SaveError> {
        let id = self.machine_id().to_string();
        save_state::load_machine(self, &id, data)
    }
}

impl Nvram for IrobotSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.novram.nvram())
    }
    fn load_nvram(&mut self, data: &[u8]) {
        self.novram.load_nvram(data);
    }
}

impl InputConfigurable for IrobotSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        IROBOT_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Button { id, pressed } => {
                // All buttons are active low: clear on press, set on release.
                let apply = |reg: &mut u8, bit: u8| {
                    if pressed {
                        *reg &= !(1 << bit);
                    } else {
                        *reg |= 1 << bit;
                    }
                };
                match id.0 {
                    INPUT_SERVICE => apply(&mut self.in0, 4),
                    INPUT_COIN3 => apply(&mut self.in0, 5),
                    INPUT_COIN1 => apply(&mut self.in0, 6),
                    INPUT_COIN2 => apply(&mut self.in0, 7),
                    INPUT_FIRE => apply(&mut self.in1, 4),
                    INPUT_BUTTON2 => apply(&mut self.in1, 5),
                    INPUT_START2 => apply(&mut self.in1, 6),
                    INPUT_START1 => apply(&mut self.in1, 7),
                    // Digital stick directions feed the self-centering stick.
                    INPUT_STICK_LEFT => {
                        self.stick[0].set_held(false, pressed);
                        self.update_stick();
                    }
                    INPUT_STICK_RIGHT => {
                        self.stick[0].set_held(true, pressed);
                        self.update_stick();
                    }
                    INPUT_STICK_UP => {
                        self.stick[1].set_held(false, pressed);
                        self.update_stick();
                    }
                    INPUT_STICK_DOWN => {
                        self.stick[1].set_held(true, pressed);
                        self.update_stick();
                    }
                    _ => {}
                }
            }
            // Analog stick: an absolute deflection (-1.0..=1.0) maps straight to
            // the channel range; relative motion (mouse) accumulates and clamps.
            InputEvent::Absolute { id, value } => match id.0 {
                INPUT_STICK_X => self.set_stick_abs(0, value, STICK_X_MIN, STICK_X_MAX),
                INPUT_STICK_Y => self.set_stick_abs(1, value, STICK_Y_MIN, STICK_Y_MAX),
                _ => {}
            },
            InputEvent::Relative { id, delta } => match id.0 {
                INPUT_STICK_X => self.move_stick_rel(0, delta, STICK_X_MIN, STICK_X_MAX),
                INPUT_STICK_Y => self.move_stick_rel(1, delta, STICK_Y_MIN, STICK_Y_MAX),
                _ => {}
            },
        }
    }

    /// Also clears conditioned analog state: the digital releases above cannot
    /// reach accumulated motion or a held deflection.
    fn release_all_inputs(&mut self) {
        phosphor_core::core::machine::release_all_controls(self);
        for c in &mut self.stick {
            c.release_all();
        }
    }
}

// ---------------------------------------------------------------------------
// DIP switches
// ---------------------------------------------------------------------------
// DSW1 (3J) is read directly at 0x10C0; DSW2 (5E) is read through POKEY 0's
// ALLPOT line. Choice bits/labels follow MAME's `irobot` port layout.
const DSW1_DEFAULT: u8 = 0x00;
const DSW2_DEFAULT: u8 = 0xFF;

const IROBOT_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSW1 (3J)",
        options: &[
            DipOption {
                name: "Coins Per Credit",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "1 Coin/1 Credit",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "2 Coins/1 Credit",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "3 Coins/1 Credit",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "4 Coins/1 Credit",
                        value: 0x03,
                    },
                ],
            },
            DipOption {
                name: "Right Coin",
                mask: 0x0c,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "×1",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "×4",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "×5",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "×6",
                        value: 0x0c,
                    },
                ],
            },
            DipOption {
                name: "Left Coin",
                mask: 0x10,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "×1",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "×2",
                        value: 0x10,
                    },
                ],
            },
            DipOption {
                name: "Bonus Adder",
                mask: 0xe0,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "None",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "1 Credit / 2 Units",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "1 Credit / 3 Units",
                        value: 0xa0,
                    },
                    DipChoice {
                        label: "1 Credit / 4 Units",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "1 Credit / 5 Units",
                        value: 0x80,
                    },
                    DipChoice {
                        label: "2 Credits / 4 Units",
                        value: 0x60,
                    },
                    DipChoice {
                        label: "Free Play",
                        value: 0xe0,
                    },
                ],
            },
        ],
    },
    DipSwitchBank {
        name: "DSW2 (5E)",
        options: &[
            DipOption {
                name: "Language",
                mask: 0x01,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "German",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "English",
                        value: 0x01,
                    },
                ],
            },
            DipOption {
                name: "Minimum Game Time",
                mask: 0x02,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "90 Seconds",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "None",
                        value: 0x02,
                    },
                ],
            },
            DipOption {
                name: "Bonus Life",
                mask: 0x0c,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "None",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "20000",
                        value: 0x0c,
                    },
                    DipChoice {
                        label: "30000",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "50000",
                        value: 0x04,
                    },
                ],
            },
            DipOption {
                name: "Lives",
                mask: 0x30,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "2",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "3",
                        value: 0x30,
                    },
                    DipChoice {
                        label: "4",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "5",
                        value: 0x10,
                    },
                ],
            },
            DipOption {
                name: "Difficulty",
                mask: 0x40,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Easy",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Medium",
                        value: 0x40,
                    },
                ],
            },
            DipOption {
                name: "Demo Mode",
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
            },
        ],
    },
];

crate::impl_dip_switches!(IrobotSystem, IROBOT_DIP_BANKS, dsw1, dsw2);

crate::impl_standalone_debug!(IrobotSystem);
impl Profilable for IrobotSystem {}
impl phosphor_core::core::debug_trace::DebugTrace for IrobotSystem {}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

crate::register_machine!(IrobotSystem, "irobot", &["irobot"], IROBOT_CONTROLS);

inventory::submit! {
    DisasmRegion {
        machine: "irobot",
        region: "main",
        cpu: DisasmCpu::M6809,
        org: 0x6000,
        size: 0xa000,
        load: |rs| IROBOT_MAINCPU_ROM.load(rs).map(|m| m[0x6000..0x10000].to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rom_regions_are_well_formed() {
        assert_eq!(IROBOT_MAINCPU_ROM.size, 0x1c000);
        assert_eq!(IROBOT_ALPHA_ROM.size, 0x800);
        assert_eq!(IROBOT_PROMS.size, 0x3420);
        // PROM entries are contiguous and fill the region exactly.
        let mut next = 0;
        for e in IROBOT_PROMS.entries {
            assert_eq!(e.offset, next, "PROM {} not contiguous", e.name);
            next += e.size;
        }
        assert_eq!(next, IROBOT_PROMS.size);
    }

    #[test]
    fn dip_tables_are_valid() {
        crate::assert_dip_banks_valid(IROBOT_DIP_BANKS, &[DSW1_DEFAULT, DSW2_DEFAULT]);
    }

    #[test]
    fn disasm_region_registered() {
        let main = crate::disasm_registry::find("irobot", "main").expect("main region");
        assert_eq!(main.cpu, crate::disasm_registry::DisasmCpu::M6809);
        assert_eq!((main.org, main.size), (0x6000, 0xa000));
    }

    /// End-to-end check on the real ROM set. Opt-in: set `IROBOT_ROM_DIR` to a
    /// directory of extracted I, Robot ROM files. Skipped (passes) when unset so
    /// CI without ROMs is unaffected.
    #[test]
    fn real_rom_renders_and_sounds() {
        let Ok(dir) = std::env::var("IROBOT_ROM_DIR") else {
            return;
        };
        let rom_set = RomSet::from_directory(std::path::Path::new(&dir)).unwrap();
        let mut sys = IrobotSystem::new();
        sys.load_rom_set(&rom_set).unwrap();
        MachineCore::reset(&mut sys);
        for _ in 0..600 {
            sys.run_frame();
        }
        // The mathbox built a display list that the rasterizer drew.
        let poly_pixels = sys.polybitmap[0]
            .iter()
            .chain(&sys.polybitmap[1])
            .filter(|&&b| b != 0)
            .count();
        assert!(poly_pixels > 0, "polygons should be rasterized");
        // The sound pipeline produced audio.
        let mut buf = vec![0i16; 8192];
        assert!(sys.fill_audio(&mut buf) > 0, "audio should be produced");
        // The CPU is running in ROM, not crashed into unmapped space.
        assert!(sys.cpu.pc >= 0x4000);
    }

    /// A raw value whose `ROUND_TO_PIXEL` ((v>>7)-128) maps to `px`, with the
    /// low 6 bits set to `color`.
    fn pixel_word(px: i32, color: u16) -> u16 {
        (((px + 128) as u16) << 7) | (color & 0x3f)
    }

    /// Write a 16-bit big-endian comm-RAM word (bank 0) through the CPU's paged
    /// window (out0 selects outx=2 = comm RAM).
    fn write_comram(sys: &mut IrobotSystem, word: u16, val: u16) {
        sys.out0_w(0x10); // outx = 2 (comm RAM), bank 0
        let off = 0x2000 + word * 2;
        Bus::write(sys, BusMaster::Cpu(0), off, (val >> 8) as u8);
        Bus::write(sys, BusMaster::Cpu(0), off + 1, (val & 0xff) as u8);
    }

    #[test]
    fn draw_line_plots_clipped_run() {
        let mut bm = vec![0u8; BITMAP_W * BITMAP_H];
        IrobotSystem::draw_line(&mut bm, 2, 5, 6, 5, 9); // horizontal run y=5
        for x in 2..=6 {
            assert_eq!(bm[(5 << 8) + x], 9, "x={x} on the line");
        }
        assert_eq!(bm[(5 << 8) + 7], 0, "past the end");
        // Off-screen endpoints are clipped, not panicking.
        IrobotSystem::draw_line(&mut bm, -50, -50, 300, 300, 1);
    }

    #[test]
    fn run_video_rasterizes_a_point() {
        let mut sys = IrobotSystem::new();
        // Object table: one point object whose data starts at word 2.
        write_comram(&mut sys, 0, 0x8000 | 2);
        write_comram(&mut sys, 1, 0xffff); // end of object table
        write_comram(&mut sys, 2, pixel_word(10, 0)); // X = 10
        write_comram(&mut sys, 3, pixel_word(20, 5)); // Y = 20, color 5
        write_comram(&mut sys, 4, 0xffff); // end of point list
        sys.statwr_w(0x04); // bit 2 rising → run the polygon generator (bufsel 0)
        assert_eq!(sys.polybitmap[0][(20 << 8) + 10], 5);
    }

    #[test]
    fn run_video_fills_a_polygon() {
        let mut sys = IrobotSystem::new();
        // Object table → polygon data at word 6.
        write_comram(&mut sys, 0, 0x4000 | 6);
        write_comram(&mut sys, 1, 0xffff);
        write_comram(&mut sys, 6, 20); // pointer to second slope list (word 20)
        write_comram(&mut sys, 7, pixel_word(20, 0)); // left edge start X = 20
        write_comram(&mut sys, 8, pixel_word(40, 0)); // right edge start X = 40
        write_comram(&mut sys, 9, pixel_word(10, 3)); // start Y = 10, color 3
        write_comram(&mut sys, 10, 0); // slope 1 = 0 (vertical edge)
        write_comram(&mut sys, 11, pixel_word(15, 0)); // edge 1 ends at Y = 15
        write_comram(&mut sys, 12, 0xffff); // slope-list 1 terminator (word1 = -1)
        write_comram(&mut sys, 13, 0xffff); // ...and ey = 0xffff
        write_comram(&mut sys, 20, 0); // slope 2 = 0
        write_comram(&mut sys, 21, pixel_word(15, 0)); // edge 2 ends at Y = 15
        sys.statwr_w(0x04);
        // Spans fill x in [21, 40] for rows 10..=15.
        assert_eq!(sys.polybitmap[0][(12 << 8) + 30], 3, "inside the polygon");
        assert_eq!(sys.polybitmap[0][(12 << 8) + 20], 0, "left edge excluded");
        assert_eq!(sys.polybitmap[0][(9 << 8) + 30], 0, "above the polygon");
        assert_eq!(sys.polybitmap[0][(16 << 8) + 30], 0, "below the polygon");
    }

    #[test]
    fn render_draws_polygon_through_palette() {
        let mut sys = IrobotSystem::new();
        sys.poly_palette[7] = (10, 20, 30);
        // bufsel 0 ⇒ the displayed buffer is index 1.
        sys.polybitmap[1][(50 << 8) + 60] = 7;
        let (w, h) = sys.display_size();
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.render_frame(&mut buf);
        let off = (50 * w as usize + 60) * 3;
        assert_eq!(&buf[off..off + 3], &[10, 20, 30]);
    }

    #[test]
    fn render_composites_text_over_polygon() {
        let mut sys = IrobotSystem::new();
        // All-ones char data ⇒ every char pixel is opaque (pen 1).
        sys.char_cache = decode_gfx(&[0xFFu8; 0x800], 0, 64, &CHAR_LAYOUT);
        sys.text_palette[1] = (200, 10, 20); // tile data 0 → color 0 → pen 1
        // A non-zero polygon background that the text must cover.
        sys.poly_palette[4] = (1, 2, 3);
        sys.polybitmap[1].fill(4);
        let (w, h) = sys.display_size();
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.render_frame(&mut buf);
        // Tile (0,0) is opaque text everywhere → text color wins over polygon.
        assert_eq!(&buf[0..3], &[200, 10, 20]);
    }

    #[test]
    fn fixed_ram_and_rom_decode() {
        let mut sys = IrobotSystem::new();
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x0042, 0xAB);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x0042), 0xAB);

        sys.map.region_data_mut(Region::Rom)[0] = 0x5A; // 0x6000
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x6000, 0x11); // ROM write ignored
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x6000), 0x5A);
    }

    #[test]
    fn ram_bank_switching() {
        let mut sys = IrobotSystem::new();
        // Bank 0 then bank 1 see independent backing at 0x0800.
        sys.out0_w(0x00); // ram bank 0
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x0800, 0x10);
        sys.out0_w(0x20); // ram bank 1 (bits 6-5 = 01)
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x0800, 0x11);
        sys.out0_w(0x00);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x0800), 0x10);
        sys.out0_w(0x20);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x0800), 0x11);
    }

    #[test]
    fn rom_bank_switching() {
        let mut sys = IrobotSystem::new();
        // Write distinct markers into two banks' backing, then select each.
        sys.map.region_data_mut(Region::BankedRom)[0] = 0xA0; // bank 0
        sys.map.region_data_mut(Region::BankedRom)[0x2000] = 0xA1; // bank 1
        sys.rom_banksel_w(0x00); // bank 0
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x4000), 0xA0);
        sys.rom_banksel_w(0x02); // bank 1 (bits 3-1 = 001)
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x4000), 0xA1);
    }

    #[test]
    fn mathbox_window_routes_scratch_ram_through_bus() {
        let mut sys = IrobotSystem::new();
        // out0 bits 4-3 = 11 selects the mathbox scratch RAM page (outx = 3),
        // with RAM bank 0 (bits 6-5 = 00) and mathbox page 0 (bits 2-1 = 00).
        sys.out0_w(0x18);
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x2000, 0x12);
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x2001, 0x34);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x2000), 0x12);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x2001), 0x34);
    }

    #[test]
    fn dsw_read_paths() {
        let mut sys = IrobotSystem::new();
        sys.dsw1 = 0x5A;
        sys.dsw2 = 0xC3;
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x10c0), 0x5A);
        // DSW2 via POKEY 0 ALLPOT: offset 0x20 → pokey 0, reg 8.
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x1420), 0xC3);
    }

    #[test]
    fn status_vblank_and_mathbox_done() {
        let mut sys = IrobotSystem::new();
        sys.clock = 0; // line 0: no vblank
        assert_eq!(sys.status_r() & 0x80, 0);
        assert_eq!(sys.status_r() & 0x20, 0x20); // mathbox reported done
        sys.clock = VBLANK_LINE * TIMING.cycles_per_scanline; // line 224
        assert_eq!(sys.status_r() & 0x80, 0x80);
    }

    #[test]
    fn irq_asserts_on_32v_rising_edge_and_clears_on_write() {
        let mut sys = IrobotSystem::new();
        // Advance to line 32 (32V rising edge).
        sys.clock = 32 * TIMING.cycles_per_scanline;
        sys.tick();
        assert!(sys.irq_pending, "IRQ should assert at the 32V rising edge");
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x1100, 0); // clear IRQ
        assert!(!sys.irq_pending);
    }

    #[test]
    fn mathbox_start_raises_firq_cleared_by_write() {
        let mut sys = IrobotSystem::new();
        sys.statwr_w(0x00);
        sys.statwr_w(0x10); // rising edge on bit 4
        assert!(sys.firq_pending);
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x1a00, 0); // clear FIRQ
        assert!(!sys.firq_pending);
    }

    #[test]
    fn nvram_round_trip_through_bus() {
        let mut sys = IrobotSystem::new();
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x1205, 0x09);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x1205) & 0x0f, 0x09);
        // save/load NVRAM image.
        let img = sys.save_nvram().unwrap().to_vec();
        let mut sys2 = IrobotSystem::new();
        sys2.load_nvram(&img);
        assert_eq!(Bus::read(&mut sys2, BusMaster::Cpu(0), 0x1205) & 0x0f, 0x09);
    }

    #[test]
    fn inputs_are_active_low() {
        let mut sys = IrobotSystem::new();
        assert_eq!(sys.in0, 0xFF);
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_COIN1),
            pressed: true,
        });
        assert_eq!(sys.in0 & 0x40, 0, "coin1 clears IN0 bit 6 while pressed");
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_COIN1),
            pressed: false,
        });
        assert_eq!(sys.in0 & 0x40, 0x40);
    }

    #[test]
    fn analog_stick_converts_through_adc() {
        let mut sys = IrobotSystem::new();
        // The X channel (AN1) is PORT_REVERSE'd: +X deflection (raw STICK_X_MAX)
        // is reflected around the 0x80 center before the game reads it.
        sys.handle_input(InputEvent::Absolute {
            id: InputId(INPUT_STICK_X),
            value: 1.0,
        });
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x1b01, 0); // start, channel 1
        assert_eq!(
            Bus::read(&mut sys, BusMaster::Cpu(0), 0x1300) as i32,
            2 * STICK_CENTER - STICK_X_MAX
        );
        // Y (AN0) is not reversed: full -Y deflection reads the Y min directly.
        sys.handle_input(InputEvent::Absolute {
            id: InputId(INPUT_STICK_Y),
            value: -1.0,
        });
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x1b00, 0); // start, channel 0
        assert_eq!(
            Bus::read(&mut sys, BusMaster::Cpu(0), 0x1300) as i32,
            STICK_Y_MIN
        );
    }

    #[test]
    fn digital_stick_self_centers() {
        let mut sys = IrobotSystem::new();
        let read_ch = |sys: &mut IrobotSystem, ch: u16| {
            Bus::write(sys, BusMaster::Cpu(0), 0x1b00 | ch, 0);
            Bus::read(sys, BusMaster::Cpu(0), 0x1300) as i32
        };
        // At rest both axes read center (X reversed around 0x80 stays 0x80).
        assert_eq!(read_ch(&mut sys, 0), STICK_CENTER); // Y
        assert_eq!(read_ch(&mut sys, 1), STICK_CENTER); // X
        // Hold left: X deflects; releasing returns it to center.
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_STICK_LEFT),
            pressed: true,
        });
        assert_ne!(read_ch(&mut sys, 1), STICK_CENTER, "held key deflects X");
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_STICK_LEFT),
            pressed: false,
        });
        assert_eq!(read_ch(&mut sys, 1), STICK_CENTER, "release recenters X");
    }

    #[test]
    fn pokey_audio_pipeline_produces_samples() {
        let mut sys = IrobotSystem::new();
        MachineCore::reset(&mut sys);
        // A POKEY register write must route without panicking (AUDC1 on POKEY 2).
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x1410 | 0x01, 0xAF);
        sys.run_frame();
        assert_eq!(sys.audio_sample_rate(), SAMPLE_RATE);
        assert!(
            !sys.audio_buffer.is_empty(),
            "a frame should mix POKEY output"
        );
        let mut buf = vec![0i16; 4096];
        let n = sys.fill_audio(&mut buf);
        assert!(n > 0);
        assert!(sys.audio_buffer.is_empty(), "fill_audio drains the buffer");
    }

    #[test]
    fn save_state_round_trip() {
        let mut sys = IrobotSystem::new();
        sys.out0_w(0x20);
        sys.rom_banksel_w(0x02);
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x0042, 0x7E);
        // Mathbox scratch RAM (via the paged window) is part of the snapshot.
        sys.out0_w(0x18); // outx = 3 (scratch RAM)
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x2002, 0x5C);
        sys.out0_w(0x20); // restore RAM bank 1 selection
        sys.clock = 1234;
        let blob = SaveState::save_state(&sys).unwrap();

        let mut sys2 = IrobotSystem::new();
        SaveState::load_state(&mut sys2, &blob).unwrap();
        assert_eq!(sys2.out0, 0x20);
        assert_eq!(sys2.rombanksel, 0x02);
        assert_eq!(sys2.clock, 1234);
        sys2.out0_w(0x18); // outx = 3 to read scratch RAM back
        assert_eq!(Bus::read(&mut sys2, BusMaster::Cpu(0), 0x2002), 0x5C);
        sys2.out0_w(0x20);
        assert_eq!(Bus::read(&mut sys2, BusMaster::Cpu(0), 0x0042), 0x7E);
    }

    #[test]
    fn text_palette_decodes_intensity_scaled_rgb() {
        let mut sys = IrobotSystem::new();
        sys.proms = vec![0u8; 0x3420];
        // entry 0: full R (bits 7-6=11), intensity 3 (bits 1-0=11) → 28*3*3 = 252
        sys.proms[0] = 0b1100_0011;
        sys.build_text_palette();
        // index 0 bit-swaps to 0; R channel should be 252.
        assert_eq!(sys.text_palette[0].0, 252);
        assert_eq!(sys.text_palette[0].1, 0);
    }

    #[test]
    fn boots_many_frames_without_panic() {
        // Build a minimal in-memory ROM set: a tiny program that just loops,
        // placed at the 6809 reset vector. Exercises tick()/run_frame() and the
        // full bus without needing the real ROM files.
        let mut main = vec![0u8; 0x1c000];
        // Program at 0x6000 (region offset 0x6000): SYNC-free busy loop `BRA *`.
        main[0x6000] = 0x20; // BRA
        main[0x6001] = 0xFE; // -2 → branch to self
        // Reset vector 0xFFFE/0xFFFF → 0x6000 (region offset 0xFFFE/0xFFFF).
        main[0xfffe] = 0x60;
        main[0xffff] = 0x00;

        let mut sys = IrobotSystem::new();
        // Load directly (skip CRC) so the synthetic ROM is accepted.
        sys.map.load_region(Region::Rom, &main[0x6000..0x10000]);
        sys.map
            .load_region(Region::BankedRom, &main[0x10000..0x1c000]);
        MachineCore::reset(&mut sys);

        for _ in 0..120 {
            sys.run_frame();
        }
        // The CPU should be parked in the loop at 0x6000.
        assert_eq!(sys.cpu.pc, 0x6000);

        // Rendering into a correctly-sized buffer must not panic.
        let (w, h) = sys.display_size();
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.render_frame(&mut buf);
    }
}
