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
//! Status (Phase 0 — boot skeleton): the 6809, full memory map with ROM/RAM
//! bank switching, the 32V scanline IRQ + mathbox FIRQ wiring, inputs, DIP
//! switches, X2212 NVRAM, and the alphanumeric text layer are implemented. The
//! mathbox, polygon rasterizer, POKEY sound, and ADC analog stick are stubbed
//! so the program boots and renders text; they arrive in later phases.

use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    ActionRole, AudioSource, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, DipSwitches,
    InputConfigurable, InputControl, InputEvent, InputId, InputKind, MachineCore, Nvram,
    Profilable, Renderable, SaveState,
};
use phosphor_core::core::save_state::{self, SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::m6809::M6809;
use phosphor_core::device::x2212::X2212;
use phosphor_core::gfx::decode::{GfxCache, GfxLayout, decode_gfx};
use phosphor_macros::{BusDebug, MemoryRegion};

use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::registry::MachineEntry;
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
    SharedRam = 4, // 0x2000-0x3FFF  8K mathbox shared RAM
    BankedRom = 5, // 0x4000-0x5FFF  6 × 8K banked ROM (backing 0xC000)
    Rom = 6,       // 0x6000-0xFFFF  40K fixed program ROM
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
};

const VBLANK_LINE: u64 = 224; // status VBLANK flag set at line 224, cleared at 0

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
/// microcode PROMs (used from Phase 1 onward).
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

    // Control registers / bank latches.
    out0: u8,       // 0x1180: RAM bank, mathbox page/bank, alphamap (bit 7)
    statwr: u8,     // 0x1140: polygon/mathbox control (edge-detected)
    rombanksel: u8, // 0x11C0: ROM bank select
    mb_outx: u8,    // mathbox memory select (Phase 1)
    mb_mpage: u8,   // mathbox bank select (Phase 1)

    // Inputs (active low: 1 = released) + DIP banks.
    in0: u8,
    in1: u8,
    dsw1: u8,
    dsw2: u8,

    // NVRAM.
    novram: X2212,

    // Interrupts / timing.
    irq_pending: bool,
    firq_pending: bool,
    prev_v32: bool,
    clock: u64,
}

impl Default for IrobotSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl IrobotSystem {
    pub fn new() -> Self {
        Self {
            cpu: M6809::new(),
            map: Self::build_map(),
            char_cache: GfxCache::new(0, 8, 8),
            text_palette: [(0, 0, 0); 32],
            poly_palette: [(0, 0, 0); 64],
            proms: Vec::new(),
            out0: 0,
            statwr: 0,
            rombanksel: 0,
            mb_outx: 0,
            mb_mpage: 0,
            in0: 0xFF,
            in1: 0xFF,
            dsw1: DSW1_DEFAULT,
            dsw2: DSW2_DEFAULT,
            novram: X2212::new(),
            irq_pending: false,
            firq_pending: false,
            prev_v32: false,
            clock: 0,
        }
    }

    fn build_map() -> AddressSpace16 {
        use Region::*;
        let mut map = AddressSpace16::new();
        map.region(Ram, "Fixed RAM", 0x0000, 0x0800, AccessKind::ReadWrite)
            .backing_region(BankedRam, "Banked RAM", 0x1800)
            .region(VideoRam, "Video RAM", 0x1c00, 0x0400, AccessKind::ReadWrite)
            .region(
                SharedRam,
                "Mathbox Shared RAM",
                0x2000,
                0x2000,
                AccessKind::ReadWrite,
            )
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
    /// intensity. Only the polygon layer (Phase 2) consumes this.
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
    /// generator running, bit 7 = VBLANK. The mathbox/polygon engines are
    /// stubbed (instantaneous), so report "mathbox done, generator idle".
    fn status_r(&self) -> u8 {
        let mut d = 0x20; // mathbox done (never busy in Phase 0)
        if self.scanline() >= VBLANK_LINE {
            d |= 0x80;
        }
        d
    }

    /// 0x1140 polygon/mathbox control. Stubbed: a rising edge on bit 4 starts
    /// the mathbox, which signals completion via FIRQ; bit 6 drives the NOVRAM
    /// RECALL line (active low).
    fn statwr_w(&mut self, data: u8) {
        if data & 0x10 != 0 && self.statwr & 0x10 == 0 {
            // Mathbox start → (stub) instantaneous completion raises FIRQ.
            self.firq_pending = true;
        }
        self.novram.recall(data & 0x40 == 0);
        self.statwr = data;
    }

    /// 0x1180 output latch: RAM bank (bits 6-5), mathbox memory/bank (bits 4-1),
    /// alphamap (bit 7).
    fn out0_w(&mut self, data: u8) {
        self.out0 = data;
        if data & 0x60 != 0x60 {
            let bank = ((data & 0x60) >> 5) as u32;
            self.map
                .remap_pages(0x08, 8, Region::BankedRam, bank * 0x800);
        }
        self.mb_outx = (data & 0x18) >> 3;
        self.mb_mpage = (data & 0x06) >> 1;
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

    /// Quad-POKEY read (`quad_pokeyn_r`). POKEY sound is stubbed; the one read
    /// the boot path needs is POKEY 0's ALLPOT register, wired to DSW2.
    fn quad_pokey_r(&self, offset: u16) -> u8 {
        let pokey_num = (offset >> 3) & !0x04;
        let control = (offset & 0x20) >> 2;
        let reg = (offset & 7) | control;
        if pokey_num == 0 && reg == 8 {
            self.dsw2
        } else {
            0x00
        }
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

        self.clock += 1;
    }

    /// Render the alphanumeric tile layer over the (Phase 0: black) polygon
    /// bitmap. Tile color = `((data & 0xC0) >> 6) | (alphamap >> 4)`; the gfx is
    /// 1bpp with pen 0 transparent.
    fn render(&self, buffer: &mut [u8]) {
        let w = TIMING.display_width as usize;
        let h = TIMING.display_height as usize;
        buffer[..w * h * 3].fill(0);
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
            0x1300..=0x13ff => 0x80, // ADC data (stub: centered stick)
            0x1400..=0x143f => self.quad_pokey_r(addr & 0x3f),
            0x1c00..=0x3fff => self.map.read_backing(addr), // video + shared RAM
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
            0x1400..=0x143f => {} // quad POKEY (stub)
            0x1800..=0x18ff => self.paletteram_w(addr & 0xff, data),
            0x1900..=0x19ff => {}                         // watchdog (stub)
            0x1a00..=0x1a3f => self.firq_pending = false, // clear FIRQ
            0x1b00..=0x1bff => {}                         // ADC channel select / start (stub)
            0x1c00..=0x1fff => self.map.write_backing(addr, data), // video RAM
            0x2000..=0x3fff => self.map.write_backing(addr, data), // shared RAM
            _ => {}                                       // ROM / unmapped
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
    fn render_frame(&self, buffer: &mut [u8]) {
        self.render(buffer);
    }
}

impl AudioSource for IrobotSystem {}

impl MachineCore for IrobotSystem {
    fn run_frame(&mut self) {
        for _ in 0..TIMING.cycles_per_frame() {
            self.tick();
        }
    }

    fn reset(&mut self) {
        self.out0 = 0;
        self.statwr = 0;
        self.rombanksel = 0;
        self.mb_outx = 0;
        self.mb_mpage = 0;
        self.irq_pending = false;
        self.firq_pending = false;
        self.prev_v32 = false;
        self.clock = 0;
        self.novram.reset();
        self.map.region_data_mut(Region::Ram).fill(0);
        self.map.region_data_mut(Region::BankedRam).fill(0);
        self.map.region_data_mut(Region::VideoRam).fill(0);
        self.map.region_data_mut(Region::SharedRam).fill(0);
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
        w.write_bytes(self.map.region_data(Region::SharedRam));
        self.novram.save_state(w);
        w.write_u8(self.out0);
        w.write_u8(self.statwr);
        w.write_u8(self.rombanksel);
        w.write_u8(self.mb_outx);
        w.write_u8(self.mb_mpage);
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
        r.read_bytes_into(self.map.region_data_mut(Region::SharedRam))?;
        self.novram.load_state(r)?;
        self.out0 = r.read_u8()?;
        self.statwr = r.read_u8()?;
        self.rombanksel = r.read_u8()?;
        self.mb_outx = r.read_u8()?;
        self.mb_mpage = r.read_u8()?;
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
        let InputEvent::Button { id, pressed } = event else {
            return;
        };
        // All inputs are active low: clear the bit on press, set it on release.
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
            _ => {}
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

impl DipSwitches for IrobotSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        IROBOT_DIP_BANKS
    }
    fn dip_bank_value(&self, bank: usize) -> u8 {
        match bank {
            0 => self.dsw1,
            1 => self.dsw2,
            _ => 0,
        }
    }
    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        match bank {
            0 => self.dsw1 = value,
            1 => self.dsw2 = value,
            _ => {}
        }
    }
}

crate::impl_standalone_debug!(IrobotSystem);
impl Profilable for IrobotSystem {}
impl phosphor_core::core::debug_trace::DebugTrace for IrobotSystem {}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = IrobotSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("irobot", &["irobot"], create_machine)
}

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
    fn save_state_round_trip() {
        let mut sys = IrobotSystem::new();
        sys.out0_w(0x20);
        sys.rom_banksel_w(0x02);
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x0042, 0x7E);
        sys.clock = 1234;
        let blob = SaveState::save_state(&sys).unwrap();

        let mut sys2 = IrobotSystem::new();
        SaveState::load_state(&mut sys2, &blob).unwrap();
        assert_eq!(sys2.out0, 0x20);
        assert_eq!(sys2.rombanksel, 0x02);
        assert_eq!(sys2.clock, 1234);
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
