use phosphor_core::audio::SampleRing;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    ActionRole, AnalogAxisKind, AudioSource, DefaultBinding, DipApplyTiming, DipChoice, DipOption,
    DipSwitchBank, Direction, InputConfigurable, InputControl, InputEvent, InputId, InputKind,
    MachineCore, MouseControl, Nvram, Profilable, Renderable, SaveState,
};
use phosphor_core::core::save_state::{self, SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, ClockDivider, TimingConfig};
use phosphor_core::cpu::m6809::M6809;
use phosphor_core::cpu::state::M6809State;
use phosphor_core::cpu::{Cpu, CpuStateTrait};
use phosphor_core::gfx::decode::{GfxCache, GfxLayout, decode_gfx};
use phosphor_core::gfx::render_bitmap_scanline;
use phosphor_macros::{BusDebug, MemoryRegion};

use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
enum Region {
    Ram = 1,
    VideoRam = 2,
    Io = 3,
    Nvram = 4,
    Rom = 5,
}

// ---------------------------------------------------------------------------
// Gridlee ROM definitions
// ---------------------------------------------------------------------------
// Gridlee ROMs are freely distributable — original authors (Howard Delman,
// Roger Hector, Ed Rotberg) explicitly allowed distribution.

/// Program ROM: 24KB at 0xA000-0xFFFF (six 4KB chips).
pub static GRIDLEE_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        RomEntry {
            name: "gridfnla.bin",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x1c43539e],
        },
        RomEntry {
            name: "gridfnlb.bin",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0xc48b91b8],
        },
        RomEntry {
            name: "gridfnlc.bin",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0x6ad436dd],
        },
        RomEntry {
            name: "gridfnld.bin",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0xf7188ddb],
        },
        RomEntry {
            name: "gridfnle.bin",
            size: 0x1000,
            offset: 0x4000,
            crc32: &[0xd5330bee],
        },
        RomEntry {
            name: "gridfnlf.bin",
            size: 0x1000,
            offset: 0x5000,
            crc32: &[0x695d16a3],
        },
    ],
};

/// Sprite/graphics ROM: 16KB (four 4KB chips).
/// Each sprite is 8x16 pixels, 64 bytes (4 bytes/row, packed 2 pixels/byte).
pub static GRIDLEE_GFX_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "gridpix0.bin",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xe6ea15ae],
        },
        RomEntry {
            name: "gridpix1.bin",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0xd722f459],
        },
        RomEntry {
            name: "gridpix2.bin",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0x1e99143c],
        },
        RomEntry {
            name: "gridpix3.bin",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0x274342a0],
        },
    ],
};

/// Color PROMs: 3x2KB (R, G, B channels, 4-bit per channel).
/// 2048 palette entries = 64 banks x 32 colors.
pub static GRIDLEE_COLOR_PROMS: RomRegion = RomRegion {
    size: 0x1800,
    entries: &[
        RomEntry {
            name: "grdrprom.bin",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0xf28f87ed],
        },
        RomEntry {
            name: "grdgprom.bin",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x921b0328],
        },
        RomEntry {
            name: "grdbprom.bin",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0x04350348],
        },
    ],
};

// ---------------------------------------------------------------------------
// Input button IDs
// ---------------------------------------------------------------------------
const INPUT_TRACK_U: u8 = 0;
const INPUT_TRACK_D: u8 = 1;
const INPUT_TRACK_L: u8 = 2;
const INPUT_TRACK_R: u8 = 3;
const INPUT_P1_FIRE: u8 = 4;
const INPUT_COIN: u8 = 5;
const INPUT_START1: u8 = 6;
const INPUT_START2: u8 = 7;

// Typed control ids for the analog axes (distinct from the 0..=7 digital ids).
const CTRL_TRACKBALL_X: InputId = InputId(8);
const CTRL_TRACKBALL_Y: InputId = InputId(9);

/// Typed logical controls. `InputId`s reuse the `INPUT_*` numbering for digital
/// controls; default bindings mirror the legacy name-matched defaults. P1 Fire
/// is also bound to the left mouse button (trackball cabinet); the trackball
/// axes map to the mouse.
const GRIDLEE_CONTROLS: &[InputControl] = &[
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
        id: InputId(INPUT_P1_FIRE as u16),
        stable_name: "p1_fire",
        label: "P1 Fire",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        // Primary role (LShift + gamepad A) plus the left mouse button
        // (trackball cabinet) as a machine-specific extra.
        default_bindings: &[DefaultBinding::Mouse(MouseControl::Left)],
    },
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
// Master clock: 20 MHz XTAL
// CPU clock: 20 MHz / 16 = 1.25 MHz
// Pixel clock: 20 MHz / 4 = 5 MHz
// HTOTAL: 320 pixel clocks → 320/4 = 80 CPU cycles per scanline
// VTOTAL: 264 scanlines per frame
// Active: 256x240 pixels (VBEND=16, VBSTART=256)
// Frame rate: 1,250,000 / (80 * 264) ≈ 59.185 Hz
const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 1_250_000, // 20 MHz / 16
    cycles_per_scanline: 80, // 320 pixel clocks / 4
    total_scanlines: 264,    // VTOTAL
    display_width: 256,
    display_height: 240,
    display_aspect: Some((4, 3)),
};
/// The board's crystal and everything divided out of it.
///
/// One 20 MHz crystal, with the 6809 at /16 and the pixel clock at /4.
pub fn clock_tree() -> phosphor_core::core::ClockTree {
    use phosphor_core::core::{ClockDomainName as Clk, ClockTree, RootId};
    let mut t = ClockTree::new(20_000_000);
    let cpu = t.add_domain(Clk::Cpu, RootId::MAIN, 1, 16); // 1.25 MHz
    let dot = t.add_domain(Clk::Pixel, RootId::MAIN, 1, 4); // 5 MHz
    t.set_step_domain(cpu);
    // 4:1 off one crystal, so 320 dot clocks is exactly 80 CPU cycles.
    t.set_raster(dot, 320, 0);
    t
}

const VBEND: u64 = 16; // First visible scanline
const VBSTART: u64 = 256; // First blanking scanline
const HBSTART_CYCLE: u64 = 64; // HBLANK at pixel 256 = CPU cycle 64 (of 80)
const FIRQ_SCANLINE: u64 = 92;

fn sample_rate() -> u64 {
    phosphor_core::audio::host_sample_rate() as u64
} // Audio output sample rate

// LFSR constants (MM5837 noise generator, same polynomial as POKEY)
const POLY17_SIZE: usize = (1 << 17) - 1; // 131071

// 8×16 sprites, 4bpp packed (high nibble = left pixel, low nibble = right pixel).
// 64 bytes per sprite (4 bytes/row × 16 rows), 256 sprites in 16KB ROM.
const GRIDLEE_SPRITE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[3, 2, 1, 0],
    x_offsets: &[0, 4, 8, 12, 16, 20, 24, 28],
    y_offsets: &[
        0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448, 480,
    ],
    char_increment: 512,
};

/// Gridlee Arcade System (Videa, 1982)
///
/// Hardware: Motorola 6809 @ 1.25 MHz, custom raster video.
/// Video: 256x240 bitmap with 32 hardware sprites (8x16), 2048-color
/// PROM-based palette (64 banks x 32 colors, per-scanline selectable).
///
/// Memory map:
///   0x0000-0x07FF  RAM (first 128 bytes = sprite RAM)
///   0x0800-0x7FFF  Video RAM (30KB, packed 2 pixels/byte)
///   0x9000         LS259 latch (LEDs, coin counter, cocktail flip)
///   0x9200         Palette bank select (6 bits)
///   0x9380         Watchdog reset
///   0x9500-0x9501  Trackball Y/X
///   0x9502         Fire buttons
///   0x9503         Coin/Start switches
///   0x9600         DIP switches
///   0x9700         Status (VBLANK, service)
///   0x9820         Random number generator (17-bit LFSR)
///   0x9828-0x993F  Sound registers
///   0x9C00-0x9CFF  NVRAM (256 bytes)
///   0xA000-0xFFFF  Program ROM (24KB)
/// Gridlee's hardware, everything the 6809 talks *to*. Held apart from the CPU
/// so a cycle dispatches at a concrete bus rather than a trait object (see
/// `docs/designs/concrete-bus-dispatch.md`).
#[derive(BusDebug)]
pub struct GridleeBoard {
    #[debug_map(cpu = 0)]
    map: AddressSpace16,

    // Graphics ROMs (not CPU-addressable)
    gfx_rom: [u8; 0x4000],  // 16KB sprite graphics
    sprite_cache: GfxCache, // Pre-decoded 256 sprites (8×16, 4bpp)

    // Palette: pre-computed from 3x2KB PROMs (2048 entries, RGB)
    palette_rgb: [(u8, u8, u8); 2048],
    palette_bank: u8, // Current bank (6 bits, 0-63)
    palette_bank_per_scanline: [u8; TIMING.total_scanlines as usize], // Latched per-scanline

    // I/O — coin/start and fire buttons are ACTIVE LOW (1 = not pressed, 0 = pressed)
    fire_buttons: u8, // 0x9502: bit 0 = P1 fire, bit 1 = P2 fire (active low)
    coin_start: u8,   // 0x9503: bits 0-3 = coin/start (active low), bits 4-5 = coinage DIP
    dip_switches: u8, // 0x9600: bonus/lives/free-play/cabinet/reset
    cocktail_flip: bool,

    // Trackball state (keyboard emulation → cumulative delta)
    track_u_pressed: bool,
    track_d_pressed: bool,
    track_l_pressed: bool,
    track_r_pressed: bool,
    last_analog_input: [u8; 2],  // Last raw trackball position [Y, X]
    last_analog_output: [u8; 2], // Cumulative output [Y, X]
    trackball_pos: [u8; 2],      // Simulated raw position [Y, X]

    // Random number generator (17-bit LFSR)
    rand17: Vec<u8>, // Pre-computed LFSR table (POLY17_SIZE + 1 entries)

    // Sound
    sound_data: [u8; 24], // Sound registers (0x00-0x17: triggers, freq, volume)
    tone_step: u64,       // Phase increment per output sample
    tone_fraction: u64,   // 24-bit phase accumulator
    tone_volume: u8,      // 8-bit volume
    audio_buffer: SampleRing<i16>,
    audio_clock: ClockDivider, // Bresenham phase for 1.25 MHz → 44.1 kHz

    // Interrupt state
    irq_pending: bool,
    firq_pending: bool,

    // Timing
    clock: u64,
    cpu_cycles: u64,
    watchdog_frame_count: u8,

    // Framebuffer (256 x 240 x RGB24)
    scanline_buffer: Vec<u8>,
}

/// Videa Gridlee (1982): a 6809 beside the board it drives.
#[derive(BusDebug)]
pub struct GridleeSystem {
    #[debug_cpu("M6809")]
    cpu: M6809,
    #[debug_bus]
    pub board: GridleeBoard,
}

/// One CPU cycle: the board's per-scanline work and audio, then the 6809
/// against the board, which *is* the bus.
#[inline]
pub fn tick(cpu: &mut M6809, board: &mut GridleeBoard) {
    board.begin_cycle(cpu);
    cpu.execute_cycle(board, BusMaster::Cpu(0));
    board.cpu_cycles += 1;
    board.clock += 1;
}

impl GridleeSystem {
    pub fn new() -> Self {
        Self {
            cpu: M6809::new(),
            board: GridleeBoard::new(),
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

    pub fn get_cpu_state(&self) -> M6809State {
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

impl GridleeBoard {
    pub fn new() -> Self {
        Self {
            map: Self::build_map(),
            gfx_rom: [0; 0x4000],
            sprite_cache: GfxCache::new(256, 8, 16),
            palette_rgb: [(0, 0, 0); 2048],
            palette_bank: 0,
            palette_bank_per_scanline: [0; TIMING.total_scanlines as usize],
            fire_buttons: 0xFF, // Active low: all bits high = no buttons pressed
            coin_start: 0xCF,   // Active low: bits 0-3 + 6-7 high, coinage DIP bits 4-5 = 0 (1C_1C)
            dip_switches: 0x05, // 3 lives (bits 3-2=01), bonus 10000 (bits 1-0=01)
            cocktail_flip: false,
            track_u_pressed: false,
            track_d_pressed: false,
            track_l_pressed: false,
            track_r_pressed: false,
            last_analog_input: [0; 2],
            last_analog_output: [0; 2],
            trackball_pos: [0; 2],
            rand17: Vec::new(),
            sound_data: [0; 24],
            tone_step: 0,
            tone_fraction: 0,
            tone_volume: 0,
            audio_buffer: SampleRing::with_capacity(1024),
            audio_clock: ClockDivider::new(sample_rate() as u32, TIMING.cpu_clock_hz as u32),
            irq_pending: false,
            firq_pending: false,
            clock: 0,
            cpu_cycles: 0,
            watchdog_frame_count: 0,
            scanline_buffer: vec![
                0u8;
                TIMING.display_width as usize * TIMING.display_height as usize * 3
            ],
        }
    }

    fn build_map() -> AddressSpace16 {
        use Region::*;
        let mut map = AddressSpace16::new();
        map.region(Ram, "RAM", 0x0000, 0x0800, AccessKind::ReadWrite)
            .region(VideoRam, "Video RAM", 0x0800, 0x7800, AccessKind::ReadWrite)
            .region(Io, "I/O", 0x9000, 0x0C00, AccessKind::Io)
            .region(Nvram, "NVRAM", 0x9C00, 0x0100, AccessKind::ReadWrite)
            .region(Rom, "Program ROM", 0xA000, 0x6000, AccessKind::ReadOnly);
        map
    }

    /// Current scanline (0-263).
    fn current_scanline(&self) -> u64 {
        (self.clock % TIMING.cycles_per_frame()) / TIMING.cycles_per_scanline
    }

    /// Board work that leads a CPU cycle: the trackball simulation, the
    /// per-scanline render and interrupts, the tone generator, and the
    /// debugger's access-attribution latch.
    fn begin_cycle(&mut self, cpu: &M6809) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();

        // Trackball movement simulation: increment raw position while keys held.
        // 8 counts/frame. 21120 cycles/frame ÷ 8 ≈ 2640 cycles/count.
        if self.clock.is_multiple_of(2640) {
            if self.track_u_pressed {
                self.trackball_pos[0] = self.trackball_pos[0].wrapping_sub(1);
            }
            if self.track_d_pressed {
                self.trackball_pos[0] = self.trackball_pos[0].wrapping_add(1);
            }
            // X axis is reversed
            if self.track_l_pressed {
                self.trackball_pos[1] = self.trackball_pos[1].wrapping_add(1);
            }
            if self.track_r_pressed {
                self.trackball_pos[1] = self.trackball_pos[1].wrapping_sub(1);
            }
        }

        // Per-scanline processing at scanline boundaries
        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let scanline = frame_cycle / TIMING.cycles_per_scanline;

            // Latch palette bank for this scanline
            self.palette_bank_per_scanline[scanline as usize] = self.palette_bank;

            // Render visible scanlines (VBEND..VBSTART = 16..255)
            if (VBEND..VBSTART).contains(&scanline) {
                self.render_scanline(scanline as usize);
            }

            // IRQ: every 64 scanlines at {64, 128, 192, 256}.
            // After scanline 256, next IRQ wraps to 64 (not 320).
            if scanline > 0 && scanline <= 256 && scanline.is_multiple_of(64) {
                self.irq_pending = true;
            }

            // FIRQ: at scanline 92, cleared at HBLANK
            if scanline == FIRQ_SCANLINE {
                self.firq_pending = true;
            }
        }

        // Clear IRQ/FIRQ at HBLANK start (pixel 256 = CPU cycle 64 within scanline).
        // This gives the CPU 64 cycles to respond.
        let cycle_in_scanline = frame_cycle % TIMING.cycles_per_scanline;
        if cycle_in_scanline == HBSTART_CYCLE {
            self.irq_pending = false;
            self.firq_pending = false;
        }

        // Sound: Bresenham downsampling from 1.25 MHz → 44.1 kHz.
        // Tone phase accumulator advances once per output sample.
        if self.audio_clock.tick() {
            let sample = if self.tone_volume > 0 && self.tone_step > 0 {
                self.tone_fraction = self.tone_fraction.wrapping_add(self.tone_step);
                if self.tone_fraction & 0x0800000 != 0 {
                    // MAME normalizes by (32768 >> 6) = 512: vol * 32767 / 512 ≈ vol * 64
                    self.tone_volume as i16 * 64
                } else {
                    0
                }
            } else {
                0
            };
            self.audio_buffer.push(sample);
        }

        // Latch watchpoint attribution context (cycle + instruction PC)
        // before CPU execution — bus dispatch cannot read CPU state mid-tick.
        if self.map.has_any_watchpoints() {
            let pc = cpu.at_instruction_boundary().then_some(cpu.pc as u32);
            self.map.latch_access_context(self.clock, pc);
        }
    }

    /// Read trackball axis (0=Y, 1=X). Implements the analog_port_r logic:
    /// compute signed delta from last read, filter tiny deltas, accumulate magnitude.
    fn read_trackball(&mut self, axis: usize) -> u8 {
        let newval = self.trackball_pos[axis];
        let mut delta = newval as i16 - self.last_analog_input[axis] as i16;

        // Handle wraparound (inclusive bounds)
        if delta >= 0x80 {
            delta -= 0x100;
        }
        if delta <= -0x80 {
            delta += 0x100;
        }

        // Ignore deltas of -1, 0, or +1 (noise filter)
        if (-1..=1).contains(&delta) {
            return self.last_analog_output[axis];
        }
        self.last_analog_input[axis] = newval;

        let sign: u8 = if delta < 0 { 0x10 } else { 0x00 };
        let magnitude = delta.unsigned_abs() as u8;

        self.last_analog_output[axis] = self.last_analog_output[axis].wrapping_add(magnitude);

        (self.last_analog_output[axis] & 0x0F) | sign
    }

    /// Read the LFSR-based random number generator, keyed to CPU cycle count.
    fn read_rng(&self) -> u8 {
        if self.rand17.is_empty() {
            return 0;
        }
        // CPU at 1.25 MHz, noise source at 100 kHz → multiply by 12.5
        // 12.5 = 8 + 4 + 0.5
        let cc = self.cpu_cycles;
        let index = ((cc << 3).wrapping_add(cc << 2).wrapping_add(cc >> 1)) as usize;
        self.rand17[index & POLY17_SIZE]
    }

    /// Write to LS259 latch. Address bits 6-4 select the output bit; data bit 0 is the value.
    fn write_latch(&mut self, addr: u16, data: u8) {
        let bit = (addr >> 4) & 0x07;
        if bit == 7 {
            self.cocktail_flip = data & 1 != 0;
        }
        // Q0-Q2: LEDs/coin counter (cosmetic), Q6: unknown — ignored
    }

    /// Write to sound registers (offset from 0x9828).
    ///
    /// Register layout:
    ///   0x04        Sample trigger (0xEF = play on channel 4)
    ///   0x08-0x0B   Bounce sample select (channels 0-3)
    ///   0x0C-0x0F   Bounce trigger (bit 0 edge starts/stops sample)
    ///   0x10        Tone frequency (value * 5 → phase step)
    ///   0x11        Tone volume (direct amplitude)
    fn write_sound(&mut self, offset: u16, data: u8) {
        let off = offset as usize;

        match off {
            // Bounce sample trigger on channel 4: edge-detect 0xEF
            0x04 => {
                let prev = self.sound_data.get(off).copied().unwrap_or(0);
                if data == 0xEF && prev != 0xEF {
                    // Would start bounce sample 1 on channel 4 (stubbed)
                } else if data != 0xEF && prev == 0xEF {
                    // Would stop channel 4 (stubbed)
                }
            }

            // Bounce triggers on channels 0-3: edge-detect bit 0
            0x0C..=0x0F => {
                let prev = self.sound_data.get(off).copied().unwrap_or(0);
                if (data & 1) != 0 && (prev & 1) == 0 {
                    // 0→1 edge: would start sample on channel (off - 0x0C)
                    // Sample ID = 1 - sound_data[off - 4] (stubbed)
                } else if (data & 1) == 0 && (prev & 1) != 0 {
                    // 1→0 edge: would stop channel (off - 0x0C) (stubbed)
                }
            }

            // Tone frequency: offset 0x10 (address 0x9838)
            // tone_step = freq_to_step * (data * 5)
            // where freq_to_step = (1 << 24) / sample_rate.
            // We compute in full precision to avoid intermediate truncation.
            0x10 => {
                if data > 0 {
                    self.tone_step = (1u64 << 24) * data as u64 * 5 / sample_rate();
                } else {
                    self.tone_step = 0;
                }
            }

            // Tone volume: offset 0x11 (address 0x9839)
            0x11 => {
                self.tone_volume = data;
            }

            _ => {}
        }

        if off < self.sound_data.len() {
            self.sound_data[off] = data;
        }
    }

    /// Render one visible scanline into the framebuffer.
    fn render_scanline(&mut self, scanline: usize) {
        let screen_y = scanline - VBEND as usize;
        if screen_y >= TIMING.display_height as usize {
            return;
        }

        let palette_bank = self.palette_bank_per_scanline[scanline];
        let row_offset = screen_y * TIMING.display_width as usize * 3;

        // Background: each VRAM byte packs 2 pixels (background pens 16..31).
        // Precompute this scanline's pen->RGB table so the unpack closure doesn't
        // borrow &self while the framebuffer row is borrowed mutably.
        let bg_lut: [(u8, u8, u8); 16] =
            std::array::from_fn(|i| self.resolve_color(palette_bank, i as u8 + 16));
        let w = TIMING.display_width as usize;
        // Gather the 128-byte packed row (copied out so the VRAM borrow ends
        // before the framebuffer is borrowed mutably). Cocktail flip reverses the
        // byte order (and swaps the nibble order via `high_first = false`).
        let mut packed = [0u8; 128];
        if self.cocktail_flip {
            let src_y = (VBSTART as usize - 1 - scanline) - VBEND as usize;
            let vram_row_start = src_y * 128;
            let vram = self.map.region_data(Region::VideoRam);
            for (k, b) in packed.iter_mut().enumerate() {
                let idx = vram_row_start + (127 - k);
                if idx < vram.len() {
                    *b = vram[idx];
                }
            }
            let row = &mut self.scanline_buffer[row_offset..row_offset + w * 3];
            render_bitmap_scanline(&packed, 2, false, |v| bg_lut[v as usize], row, 0);
        } else {
            let vram_row_start = screen_y * 128;
            let vram = self.map.region_data(Region::VideoRam);
            for (k, b) in packed.iter_mut().enumerate() {
                let idx = vram_row_start + k;
                if idx < vram.len() {
                    *b = vram[idx];
                }
            }
            let row = &mut self.scanline_buffer[row_offset..row_offset + w * 3];
            render_bitmap_scanline(&packed, 2, true, |v| bg_lut[v as usize], row, 0);
        }

        // Sprites: 32 sprites from RAM at 0x0000 (4 bytes each).
        // Format: [image_num, unused, y_pos, x_pos]
        // Each sprite is 8 wide x 16 tall, 64 bytes in GFX ROM.
        // Y positions wrap at 256.
        // Clips sprites to ypos >= (16 + VBEND) = 32, preventing
        // wrap-around artifacts on the top 16 visible scanlines.
        if scanline < (16 + VBEND as usize) {
            return;
        }
        for i in 0..32 {
            let base = i * 4;
            let ram = self.map.region_data(Region::Ram);
            let image_num = ram[base] as usize;
            // Start Y = sprite_ram[2] + 17 + VBEND, wrapped to 8 bits
            let sprite_y_start = (ram[base + 2] as u16 + 17 + VBEND as u16) as u8;
            let sprite_x = ram[base + 3] as usize;

            // Cocktail flip
            let (check_scanline, x_xor) = if self.cocktail_flip {
                (271usize.wrapping_sub(scanline) as u8, 0xFF)
            } else {
                (scanline as u8, 0x00)
            };

            // Which row of the sprite falls on this scanline? (wrapping subtraction)
            let row_in_sprite = check_scanline.wrapping_sub(sprite_y_start);
            if row_in_sprite >= 16 {
                continue;
            }

            let row = row_in_sprite as usize;
            for dx in 0..8 {
                let idx = self.sprite_cache.pixel(image_num, dx, row);
                if idx == 0 {
                    continue; // transparent
                }
                let px = (sprite_x + dx) ^ x_xor;
                if px >= TIMING.display_width as usize {
                    continue;
                }
                let color = self.resolve_color(palette_bank, idx);
                self.write_pixel(row_offset, px, color);
            }
        }
    }

    /// Write a single RGB pixel to the scanline buffer.
    #[inline]
    fn write_pixel(&mut self, row_offset: usize, px: usize, color: (u8, u8, u8)) {
        let off = row_offset + px * 3;
        if off + 2 < self.scanline_buffer.len() {
            self.scanline_buffer[off] = color.0;
            self.scanline_buffer[off + 1] = color.1;
            self.scanline_buffer[off + 2] = color.2;
        }
    }

    /// Look up an RGB color from the pre-computed palette.
    fn resolve_color(&self, palette_bank: u8, color_index: u8) -> (u8, u8, u8) {
        let addr = ((palette_bank as usize & 0x3F) << 5) | (color_index as usize & 0x1F);
        self.palette_rgb[addr]
    }

    /// Build the 2048-entry RGB palette from color PROMs.
    fn build_palette(&mut self, prom_data: &[u8]) {
        for i in 0..2048 {
            let r4 = prom_data[i] & 0x0F;
            let g4 = prom_data[0x0800 + i] & 0x0F;
            let b4 = prom_data[0x1000 + i] & 0x0F;
            // Expand 4-bit to 8-bit: 0x0→0x00, 0xF→0xFF
            self.palette_rgb[i] = (r4 * 17, g4 * 17, b4 * 17);
        }
    }

    /// Initialize the 17-bit LFSR polynomial table (MM5837 noise generator).
    fn init_lfsr(&mut self) {
        let mut rand17 = vec![0u8; POLY17_SIZE + 1];
        let mut x: u32 = 0;

        for entry in rand17.iter_mut().take(POLY17_SIZE) {
            // Store random byte (bits 3-10 of state)
            *entry = (x >> 3) as u8;
            // Advance polynomial: x = ((x << 7) + (x >> 10) + 0x18000) & POLY17_SIZE
            x = ((x << 7).wrapping_add(x >> 10).wrapping_add(0x18000)) & POLY17_SIZE as u32;
        }

        self.rand17 = rand17;
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let program_data = GRIDLEE_PROGRAM_ROM.load(rom_set)?;
        self.map.load_region(Region::Rom, &program_data);

        let gfx_data = GRIDLEE_GFX_ROM.load(rom_set)?;
        self.gfx_rom.copy_from_slice(&gfx_data);
        self.sprite_cache = decode_gfx(&self.gfx_rom, 0, 256, &GRIDLEE_SPRITE_LAYOUT);

        let prom_data = GRIDLEE_COLOR_PROMS.load(rom_set)?;
        self.build_palette(&prom_data);

        self.init_lfsr();

        Ok(())
    }

    /// Rebuild sprite cache from gfx_rom (for tests that modify ROM data directly).
    #[cfg(test)]
    fn decode_sprite_cache(&mut self) {
        self.sprite_cache = decode_gfx(&self.gfx_rom, 0, 256, &GRIDLEE_SPRITE_LAYOUT);
    }
}

impl Default for GridleeBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for GridleeSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus implementation
// ---------------------------------------------------------------------------

// The board is the bus.
impl Bus for GridleeBoard {
    type Address = u16;
    type Data = u8;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let data = match self.map.page(addr).region_id {
            Region::RAM | Region::VIDEO_RAM | Region::NVRAM | Region::ROM => {
                self.map.read_backing(addr)
            }

            Region::IO => match addr {
                0x9500 => self.read_trackball(0),
                0x9501 => self.read_trackball(1),
                0x9502 => self.fire_buttons,
                0x9503 => self.coin_start,
                0x9600 => self.dip_switches,
                0x9700 => {
                    let scanline = self.current_scanline();
                    let vblank = if !(VBEND..VBSTART).contains(&scanline) {
                        0x80
                    } else {
                        0x00
                    };
                    vblank | 0x7F
                }
                0x9820 => self.read_rng(),
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
            Region::RAM | Region::VIDEO_RAM | Region::NVRAM => {
                self.map.write_backing(addr, data);
            }

            Region::IO => match addr {
                0x9000..=0x907F => self.write_latch(addr, data),
                0x9200 => self.palette_bank = data & 0x3F,
                0x9380 => self.watchdog_frame_count = 0,
                0x9828..=0x993F => self.write_sound(addr - 0x9828, data),
                _ => {}
            },

            _ => {} // ROM and unmapped: writes ignored
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
// Machine traits (MachineCore + capabilities)
// ---------------------------------------------------------------------------

impl Renderable for GridleeSystem {
    fn display_size(&self) -> (u32, u32) {
        TIMING.display_size()
    }

    fn display_aspect(&self) -> Option<(u32, u32)> {
        TIMING.display_aspect()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        buffer.copy_from_slice(&self.board.scanline_buffer);
    }
}

impl AudioSource for GridleeSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.board.audio_buffer.pop_front_into(buffer)
    }

    fn audio_sample_rate(&self) -> u32 {
        phosphor_core::audio::host_sample_rate()
    }
}

impl InputConfigurable for GridleeSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        GRIDLEE_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Button { id, pressed } => match id.0 as u8 {
                INPUT_TRACK_U => self.board.track_u_pressed = pressed,
                INPUT_TRACK_D => self.board.track_d_pressed = pressed,
                INPUT_TRACK_L => self.board.track_l_pressed = pressed,
                INPUT_TRACK_R => self.board.track_r_pressed = pressed,
                // Active-low buttons: clear bit on press, set bit on release
                INPUT_P1_FIRE => {
                    if pressed {
                        self.board.fire_buttons &= !0x01;
                    } else {
                        self.board.fire_buttons |= 0x01;
                    }
                }
                INPUT_COIN => {
                    if pressed {
                        self.board.coin_start &= !0x01;
                    } else {
                        self.board.coin_start |= 0x01;
                    }
                }
                INPUT_START1 => {
                    if pressed {
                        self.board.coin_start &= !0x04;
                    } else {
                        self.board.coin_start |= 0x04;
                    }
                }
                INPUT_START2 => {
                    if pressed {
                        self.board.coin_start &= !0x08;
                    } else {
                        self.board.coin_start |= 0x08;
                    }
                }
                _ => {}
            },
            InputEvent::Relative { id, delta } => {
                // Scale down mouse motion for comfortable sensitivity (÷3, clamp ±6)
                let scaled = ((delta as i32) / 3).clamp(-6, 6) as i8;
                if id == CTRL_TRACKBALL_X {
                    // X axis reversed: positive mouse motion (right) decreases counter
                    self.board.trackball_pos[1] =
                        self.board.trackball_pos[1].wrapping_sub(scaled as u8);
                } else if id == CTRL_TRACKBALL_Y {
                    // Y axis: positive mouse motion (down) increases counter
                    self.board.trackball_pos[0] =
                        self.board.trackball_pos[0].wrapping_add(scaled as u8);
                }
            }
            InputEvent::Absolute { .. } => {}
        }
    }
}

crate::impl_standalone_debug!(GridleeSystem);

impl Saveable for GridleeSystem {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        w.write_bytes(self.board.map.region_data(Region::Ram));
        w.write_bytes(self.board.map.region_data(Region::VideoRam));
        w.write_bytes(self.board.map.region_data(Region::Nvram));
        w.write_u8(self.board.palette_bank);
        w.write_bytes(&self.board.palette_bank_per_scanline);
        w.write_u8(self.board.fire_buttons);
        w.write_u8(self.board.coin_start);
        w.write_bool(self.board.cocktail_flip);
        w.write_bool(self.board.track_u_pressed);
        w.write_bool(self.board.track_d_pressed);
        w.write_bool(self.board.track_l_pressed);
        w.write_bool(self.board.track_r_pressed);
        w.write_bytes(&self.board.last_analog_input);
        w.write_bytes(&self.board.last_analog_output);
        w.write_bytes(&self.board.trackball_pos);
        w.write_bytes(&self.board.sound_data);
        w.write_u64_le(self.board.tone_step);
        w.write_u64_le(self.board.tone_fraction);
        w.write_u8(self.board.tone_volume);
        self.board.audio_clock.save_state(w);
        w.write_bool(self.board.irq_pending);
        w.write_bool(self.board.firq_pending);
        w.write_u64_le(self.board.clock);
        w.write_u64_le(self.board.cpu_cycles);
        w.write_u8(self.board.watchdog_frame_count);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        r.read_bytes_into(self.board.map.region_data_mut(Region::Ram))?;
        r.read_bytes_into(self.board.map.region_data_mut(Region::VideoRam))?;
        r.read_bytes_into(self.board.map.region_data_mut(Region::Nvram))?;
        self.board.palette_bank = r.read_u8()?;
        r.read_bytes_into(&mut self.board.palette_bank_per_scanline)?;
        self.board.fire_buttons = r.read_u8()?;
        self.board.coin_start = r.read_u8()?;
        self.board.cocktail_flip = r.read_bool()?;
        self.board.track_u_pressed = r.read_bool()?;
        self.board.track_d_pressed = r.read_bool()?;
        self.board.track_l_pressed = r.read_bool()?;
        self.board.track_r_pressed = r.read_bool()?;
        r.read_bytes_into(&mut self.board.last_analog_input)?;
        r.read_bytes_into(&mut self.board.last_analog_output)?;
        r.read_bytes_into(&mut self.board.trackball_pos)?;
        r.read_bytes_into(&mut self.board.sound_data)?;
        self.board.tone_step = r.read_u64_le()?;
        self.board.tone_fraction = r.read_u64_le()?;
        self.board.tone_volume = r.read_u8()?;
        self.board.audio_clock.load_state(r)?;
        self.board.irq_pending = r.read_bool()?;
        self.board.firq_pending = r.read_bool()?;
        self.board.clock = r.read_u64_le()?;
        self.board.cpu_cycles = r.read_u64_le()?;
        self.board.watchdog_frame_count = r.read_u8()?;
        Ok(())
    }
}

impl MachineCore for GridleeSystem {
    fn gfx_sheets(&self) -> Vec<phosphor_core::core::machine::GfxSheet<'_>> {
        use phosphor_core::core::machine::GfxSheet;
        // Gridlee's playfield is a VRAM bitmap; only the sprites are a decoded
        // cache. Its palette is 64 banks × 32 colors selected per-scanline, so
        // there's no single "the" palette — show bank 0's 32 entries.
        vec![GfxSheet {
            name: "sprites",
            cache: &self.board.sprite_cache,
            palette: &self.board.palette_rgb[..32],
        }]
    }

    fn run_frame(&mut self) {
        for _ in 0..TIMING.cycles_per_frame() {
            tick(&mut self.cpu, &mut self.board);
        }

        // Watchdog: We keep the frame counter for documentation but
        //don't reset.
        self.board.watchdog_frame_count += 1;
    }

    fn reset(&mut self) {
        self.board.irq_pending = false;
        self.board.firq_pending = false;
        self.board.watchdog_frame_count = 0;
        self.board.clock = 0;
        self.board.cpu_cycles = 0;
        self.board.tone_step = 0;
        self.board.tone_fraction = 0;
        self.board.tone_volume = 0;
        self.board.audio_buffer.clear();
        self.board.audio_clock.reset();
        self.board.scanline_buffer.fill(0);

        self.cpu.reset(&mut self.board, BusMaster::Cpu(0));
    }

    fn frame_rate_hz(&self) -> f64 {
        TIMING.frame_rate_hz()
    }

    fn machine_id(&self) -> &str {
        "gridlee"
    }

    crate::machine_clock_declaration!(TIMING, crate::gridlee::clock_tree);
}

impl SaveState for GridleeSystem {
    fn save_state(&self) -> Option<Vec<u8>> {
        Some(save_state::save_machine(self, self.machine_id()))
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), SaveError> {
        let id = self.machine_id().to_string();
        save_state::load_machine(self, &id, data)
    }
}

impl Nvram for GridleeSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.map.region_data(Region::Nvram))
    }

    fn load_nvram(&mut self, data: &[u8]) {
        let nvram = self.board.map.region_data_mut(Region::Nvram);
        let len = data.len().min(nvram.len());
        nvram[..len].copy_from_slice(&data[..len]);
    }
}

impl Profilable for GridleeSystem {}
/// DIP switch metadata for Gridlee's DSW byte (read at 0x9600). Choice bits and
/// labels follow MAME's `gridlee` layout; option defaults OR to the historical
/// 0x05 (10000-point bonus, 3 lives).
const GRIDLEE_DIP_BANKS: &[DipSwitchBank] = &[DipSwitchBank {
    name: "DSW",
    options: &[
        DipOption {
            name: "Bonus Life",
            mask: 0x03,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "8000 points",
                    value: 0x00,
                },
                DipChoice {
                    label: "10000 points",
                    value: 0x01,
                },
                DipChoice {
                    label: "12000 points",
                    value: 0x02,
                },
                DipChoice {
                    label: "14000 points",
                    value: 0x03,
                },
            ],
        },
        DipOption {
            name: "Lives",
            mask: 0x0C,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "2",
                    value: 0x00,
                },
                DipChoice {
                    label: "3",
                    value: 0x04,
                },
                DipChoice {
                    label: "4",
                    value: 0x08,
                },
                DipChoice {
                    label: "5",
                    value: 0x0C,
                },
            ],
        },
        DipOption {
            name: "Free Play",
            mask: 0x10,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "Off",
                    value: 0x00,
                },
                DipChoice {
                    label: "On",
                    value: 0x10,
                },
            ],
        },
        DipOption {
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
        },
        DipOption {
            name: "Reset Hall of Fame",
            mask: 0x40,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "No",
                    value: 0x00,
                },
                DipChoice {
                    label: "Yes",
                    value: 0x40,
                },
            ],
        },
        DipOption {
            name: "Reset Game Data",
            mask: 0x80,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "No",
                    value: 0x00,
                },
                DipChoice {
                    label: "Yes",
                    value: 0x80,
                },
            ],
        },
    ],
}];

crate::impl_dip_switches!(GridleeSystem, GRIDLEE_DIP_BANKS, board.dip_switches);
impl phosphor_core::core::debug_trace::DebugTrace for GridleeSystem {}

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

crate::register_machine!(GridleeSystem, "gridlee", &["gridlee"], GRIDLEE_CONTROLS);

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::DipSwitches;
    use phosphor_core::cpu::CpuStateTrait;

    fn make_system() -> GridleeSystem {
        let mut sys = GridleeSystem::new();
        sys.board.init_lfsr();
        sys
    }

    // -----------------------------------------------------------------------
    // Palette
    // -----------------------------------------------------------------------

    #[test]
    fn palette_4bit_to_8bit_expansion() {
        let mut sys = GridleeSystem::new();
        // Build a minimal PROM: R=0xF, G=0x0, B=0x8 at entry 0
        let mut prom = vec![0u8; 0x1800];
        prom[0] = 0x0F; // R = 15
        prom[0x0800] = 0x00; // G = 0
        prom[0x1000] = 0x08; // B = 8
        sys.board.build_palette(&prom);
        // 0xF * 17 = 255, 0x0 * 17 = 0, 0x8 * 17 = 136
        assert_eq!(sys.board.palette_rgb[0], (255, 0, 136));
    }

    #[test]
    fn palette_bank_addressing() {
        let mut sys = GridleeSystem::new();
        let mut prom = vec![0u8; 0x1800];
        // Set entry at bank 2, index 5 → address (2 << 5) | 5 = 69
        prom[69] = 0x0A; // R
        prom[0x0800 + 69] = 0x05; // G
        prom[0x1000 + 69] = 0x03; // B
        sys.board.build_palette(&prom);
        let color = sys.board.resolve_color(2, 5);
        assert_eq!(color, (0x0A * 17, 0x05 * 17, 0x03 * 17));
    }

    // -----------------------------------------------------------------------
    // LFSR random number generator
    // -----------------------------------------------------------------------

    #[test]
    fn lfsr_table_non_zero() {
        let sys = make_system();
        assert_eq!(sys.board.rand17.len(), POLY17_SIZE + 1);
        // Table should have non-zero entries (not all zeros)
        let nonzero_count = sys.board.rand17.iter().filter(|&&b| b != 0).count();
        assert!(nonzero_count > POLY17_SIZE / 2, "LFSR table mostly zero");
    }

    #[test]
    fn rng_returns_different_values_at_different_cycles() {
        let mut sys = make_system();
        sys.board.cpu_cycles = 100;
        let v1 = sys.board.read_rng();
        sys.board.cpu_cycles = 200;
        let v2 = sys.board.read_rng();
        sys.board.cpu_cycles = 300;
        let v3 = sys.board.read_rng();
        // At least two of three should differ
        assert!(
            v1 != v2 || v2 != v3,
            "RNG returned same value at cycles 100, 200, 300"
        );
    }

    // -----------------------------------------------------------------------
    // Memory map
    // -----------------------------------------------------------------------

    #[test]
    fn ram_read_write_roundtrip() {
        let mut sys = make_system();
        Bus::write(&mut sys.board, BusMaster::Cpu(0), 0x0042, 0xAB);
        assert_eq!(Bus::read(&mut sys.board, BusMaster::Cpu(0), 0x0042), 0xAB);
    }

    #[test]
    fn vram_read_write_roundtrip() {
        let mut sys = make_system();
        Bus::write(&mut sys.board, BusMaster::Cpu(0), 0x0800, 0xCD);
        assert_eq!(Bus::read(&mut sys.board, BusMaster::Cpu(0), 0x0800), 0xCD);
    }

    #[test]
    fn nvram_read_write_roundtrip() {
        let mut sys = make_system();
        Bus::write(&mut sys.board, BusMaster::Cpu(0), 0x9C42, 0x77);
        assert_eq!(Bus::read(&mut sys.board, BusMaster::Cpu(0), 0x9C42), 0x77);
    }

    #[test]
    fn rom_write_ignored() {
        let mut sys = make_system();
        sys.board.map.region_data_mut(Region::Rom)[0] = 0xAA;
        Bus::write(&mut sys.board, BusMaster::Cpu(0), 0xA000, 0x55);
        assert_eq!(Bus::read(&mut sys.board, BusMaster::Cpu(0), 0xA000), 0xAA);
    }

    #[test]
    fn unmapped_reads_return_ff() {
        let mut sys = make_system();
        assert_eq!(Bus::read(&mut sys.board, BusMaster::Cpu(0), 0x8000), 0xFF);
    }

    // -----------------------------------------------------------------------
    // VBLANK
    // -----------------------------------------------------------------------

    #[test]
    fn vblank_active_during_blanking() {
        let mut sys = make_system();
        // Scanline 0 (< VBEND=16): in VBLANK
        sys.board.clock = 0;
        let status = Bus::read(&mut sys.board, BusMaster::Cpu(0), 0x9700);
        assert_ne!(status & 0x80, 0, "VBLANK should be active at scanline 0");
    }

    #[test]
    fn vblank_inactive_during_active_display() {
        let mut sys = make_system();
        // Scanline 128 (within VBEND..VBSTART): active display
        sys.board.clock = 128 * TIMING.cycles_per_scanline;
        let status = Bus::read(&mut sys.board, BusMaster::Cpu(0), 0x9700);
        assert_eq!(
            status & 0x80,
            0,
            "VBLANK should be inactive at scanline 128"
        );
    }

    #[test]
    fn vblank_active_after_vbstart() {
        let mut sys = make_system();
        // Scanline 256 (>= VBSTART): in VBLANK
        sys.board.clock = 256 * TIMING.cycles_per_scanline;
        let status = Bus::read(&mut sys.board, BusMaster::Cpu(0), 0x9700);
        assert_ne!(status & 0x80, 0, "VBLANK should be active at scanline 256");
    }

    // -----------------------------------------------------------------------
    // Interrupts
    // -----------------------------------------------------------------------

    #[test]
    fn irq_not_asserted_at_scanline_0() {
        let mut sys = make_system();
        // Steady-state pattern is {64, 128, 192, 256}.
        sys.board.clock = TIMING.cycles_per_frame(); // Start of next frame = scanline 0
        sys.step_cycle();
        assert!(!sys.board.irq_pending, "IRQ should NOT fire at scanline 0");
    }

    #[test]
    fn irq_asserted_at_scanline_64() {
        let mut sys = make_system();
        sys.board.clock = 64 * TIMING.cycles_per_scanline;
        sys.step_cycle();
        assert!(
            sys.board.irq_pending,
            "IRQ should be pending at scanline 64"
        );
    }

    #[test]
    fn irq_asserted_at_scanline_256() {
        let mut sys = make_system();
        // Scanline 256 is the VBLANK IRQ
        sys.board.clock = 256 * TIMING.cycles_per_scanline;
        sys.step_cycle();
        assert!(sys.board.irq_pending, "IRQ should fire at scanline 256");
    }

    #[test]
    fn firq_asserted_at_scanline_92() {
        let mut sys = make_system();
        sys.board.clock = 92 * TIMING.cycles_per_scanline;
        sys.step_cycle();
        assert!(
            sys.board.firq_pending,
            "FIRQ should be pending at scanline 92"
        );
    }

    #[test]
    fn irq_cleared_at_hblank() {
        let mut sys = make_system();
        // Assert IRQ at scanline 64
        sys.board.clock = 64 * TIMING.cycles_per_scanline;
        sys.step_cycle();
        assert!(sys.board.irq_pending);
        // Cleared at HBSTART (CPU cycle 64 within scanline)
        sys.board.clock = 64 * TIMING.cycles_per_scanline + HBSTART_CYCLE;
        sys.step_cycle();
        assert!(!sys.board.irq_pending, "IRQ should be cleared at HBLANK");
    }

    // -----------------------------------------------------------------------
    // Watchdog
    // -----------------------------------------------------------------------

    #[test]
    fn watchdog_reset_prevents_timeout() {
        let mut sys = make_system();
        sys.board.watchdog_frame_count = 7;
        // Write to watchdog resets counter
        Bus::write(&mut sys.board, BusMaster::Cpu(0), 0x9380, 0x00);
        assert_eq!(sys.board.watchdog_frame_count, 0);
    }

    // -----------------------------------------------------------------------
    // Palette bank select
    // -----------------------------------------------------------------------

    #[test]
    fn palette_bank_select_masks_to_6_bits() {
        let mut sys = make_system();
        Bus::write(&mut sys.board, BusMaster::Cpu(0), 0x9200, 0xFF);
        assert_eq!(sys.board.palette_bank, 0x3F);
    }

    // -----------------------------------------------------------------------
    // Trackball
    // -----------------------------------------------------------------------

    #[test]
    fn trackball_filters_small_deltas() {
        let mut sys = make_system();
        // Move by 1 (should be filtered)
        sys.board.trackball_pos[0] = 1;
        let result = sys.board.read_trackball(0);
        assert_eq!(result, 0, "Delta of 1 should be filtered out");
    }

    #[test]
    fn trackball_reports_magnitude_and_sign() {
        let mut sys = make_system();
        // Move by +5
        sys.board.trackball_pos[0] = 5;
        let result = sys.board.read_trackball(0);
        // Magnitude = 5 & 0xF = 5, sign = 0 (positive)
        assert_eq!(result & 0x0F, 5);
        assert_eq!(result & 0x10, 0, "Should be positive direction");
    }

    #[test]
    fn trackball_negative_direction() {
        let mut sys = make_system();
        // Move by -5 (wraps to 251)
        sys.board.trackball_pos[0] = 251;
        let result = sys.board.read_trackball(0);
        // Magnitude = 5 & 0xF = 5, sign = 0x10 (negative)
        assert_eq!(result & 0x0F, 5);
        assert_eq!(result & 0x10, 0x10, "Should be negative direction");
    }

    // -----------------------------------------------------------------------
    // Sound
    // -----------------------------------------------------------------------

    #[test]
    fn sound_tone_step_zero_when_freq_zero() {
        let mut sys = make_system();
        sys.board.write_sound(0x10, 0);
        assert_eq!(sys.board.tone_step, 0);
    }

    #[test]
    fn sound_tone_step_nonzero_when_freq_set() {
        let mut sys = make_system();
        sys.board.write_sound(0x10, 0x40);
        assert!(
            sys.board.tone_step > 0,
            "tone_step should be non-zero for freq=0x40"
        );
    }

    #[test]
    fn sound_tone_step_full_precision() {
        let mut sys = make_system();
        // For data=255: (float) ≈ (1<<24) * 255 * 5 / 44100 = 485097
        // Old truncated: ((1<<24)/44100) * 255 * 5 = 380 * 1275 = 484500
        // New full precision: (1<<24) * 255 * 5 / 44100 = 485097
        sys.board.write_sound(0x10, 255);
        let expected = (1u64 << 24) * 255 * 5 / sample_rate();
        assert_eq!(sys.board.tone_step, expected);
        // Verify it's more accurate than the truncated version
        let truncated = ((1u64 << 24) / sample_rate()) * 255 * 5;
        assert!(
            sys.board.tone_step > truncated,
            "Full precision should be larger"
        );
    }

    #[test]
    fn sound_volume_register() {
        let mut sys = make_system();
        sys.board.write_sound(0x11, 0xAB);
        assert_eq!(sys.board.tone_volume, 0xAB);
    }

    #[test]
    fn sound_bounce_trigger_edge_detect() {
        let mut sys = make_system();
        // Write initial state for bounce trigger offset 0x0C
        sys.board.write_sound(0x0C, 0x00);
        assert_eq!(sys.board.sound_data[0x0C], 0x00);
        // Trigger 0→1 edge (would start sample in full impl)
        sys.board.write_sound(0x0C, 0x01);
        assert_eq!(sys.board.sound_data[0x0C], 0x01);
        // Trigger 1→0 edge (would stop sample)
        sys.board.write_sound(0x0C, 0x00);
        assert_eq!(sys.board.sound_data[0x0C], 0x00);
    }

    #[test]
    fn sound_sample_trigger_0xef() {
        let mut sys = make_system();
        // Write non-0xEF first
        sys.board.write_sound(0x04, 0x00);
        assert_eq!(sys.board.sound_data[0x04], 0x00);
        // Write 0xEF (triggers sample in full impl)
        sys.board.write_sound(0x04, 0xEF);
        assert_eq!(sys.board.sound_data[0x04], 0xEF);
        // Write back (stops sample)
        sys.board.write_sound(0x04, 0x00);
        assert_eq!(sys.board.sound_data[0x04], 0x00);
    }

    // -----------------------------------------------------------------------
    // Input
    // -----------------------------------------------------------------------

    #[test]
    fn fire_button_active_low() {
        let mut sys = make_system();
        // Default: bit 0 high (not pressed)
        assert_eq!(sys.board.fire_buttons & 0x01, 0x01);
        // Press: bit 0 goes low
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_P1_FIRE) as u16),
            pressed: true,
        });
        assert_eq!(sys.board.fire_buttons & 0x01, 0x00);
        // Release: bit 0 goes high again
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_P1_FIRE) as u16),
            pressed: false,
        });
        assert_eq!(sys.board.fire_buttons & 0x01, 0x01);
    }

    #[test]
    fn coin_and_start_active_low() {
        let mut sys = make_system();
        // Default: bits 0-3 and 6-7 all high (nothing pressed, unknown bits active-low)
        assert_eq!(sys.board.coin_start, 0xCF);
        // Coin press: bit 0 goes low
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_COIN) as u16),
            pressed: true,
        });
        assert_eq!(sys.board.coin_start & 0x01, 0x00);
        // Start1 press: bit 2 goes low
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_START1) as u16),
            pressed: true,
        });
        assert_eq!(sys.board.coin_start & 0x04, 0x00);
        // Start2 press: bit 3 goes low
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_START2) as u16),
            pressed: true,
        });
        assert_eq!(sys.board.coin_start & 0x08, 0x00);
    }

    #[test]
    fn dip_switch_defaults() {
        let sys = make_system();
        // Default: 3 lives (bits 3-2 = 01), bonus 10000 (bits 1-0 = 01)
        assert_eq!(sys.board.dip_switches, 0x05);
        assert_eq!(sys.dip_bank_value(0), 0x05);
        crate::assert_dip_banks_valid(sys.dip_banks(), &[0x05]);
    }

    #[test]
    fn set_dip_option_masks_only_its_bits() {
        let mut sys = make_system();
        // Lives is option 1 (mask 0x0C); pick "5" (0x0C). Bonus bits preserved.
        sys.set_dip_option(0, 1, 0x0C);
        assert_eq!(sys.dip_bank_value(0), 0x0D); // 0x05 with lives bits set
    }

    // -----------------------------------------------------------------------
    // NVRAM persistence
    // -----------------------------------------------------------------------

    #[test]
    fn nvram_save_load_roundtrip() {
        let mut sys = make_system();
        sys.board.map.region_data_mut(Region::Nvram)[0] = 0x42;
        sys.board.map.region_data_mut(Region::Nvram)[255] = 0xFF;
        let saved = sys.save_nvram().unwrap().to_vec();
        assert_eq!(saved.len(), 256);

        let mut sys2 = make_system();
        sys2.load_nvram(&saved);
        assert_eq!(sys2.board.map.region_data_mut(Region::Nvram)[0], 0x42);
        assert_eq!(sys2.board.map.region_data_mut(Region::Nvram)[255], 0xFF);
    }

    // -----------------------------------------------------------------------
    // Sprite rendering
    // -----------------------------------------------------------------------

    #[test]
    fn sprite_not_rendered_on_scanline_below_32() {
        let mut sys = make_system();
        // Place sprite at image 1, Y position that puts it at scanline 20
        // sprite_y_start = (y_pos + 17 + 16) as u8
        // We want sprite_y_start = 20, so y_pos = 20 - 33 = -13 → wraps to 243
        sys.board.map.region_data_mut(Region::Ram)[0] = 1; // image
        sys.board.map.region_data_mut(Region::Ram)[2] = 243; // y_pos → (243 + 33) & 0xFF = 20
        sys.board.map.region_data_mut(Region::Ram)[3] = 10; // x_pos
        // Put non-zero pixel data in GFX ROM for image 1
        sys.board.gfx_rom[64] = 0x12; // left=1, right=2
        sys.board.decode_sprite_cache();

        // Build a simple palette so pixels would be visible
        sys.board.palette_rgb[1] = (255, 0, 0);
        sys.board.palette_rgb[2] = (0, 255, 0);

        // Render scanline 20 (< 32 clip threshold)
        sys.board.palette_bank_per_scanline[20] = 0;
        sys.board.render_scanline(20);

        // Sprite should NOT appear (clipped). Check pixel at x=10.
        let screen_y = 20 - VBEND as usize;
        let off = (screen_y * TIMING.display_width as usize + 10) * 3;
        // Should be background (black, since VRAM is zero → palette index 16)
        // not sprite color (255,0,0)
        assert_ne!(
            (
                sys.board.scanline_buffer[off],
                sys.board.scanline_buffer[off + 1]
            ),
            (255, 0),
            "Sprite should not render on scanline < 32"
        );
    }

    #[test]
    fn sprite_rendered_on_scanline_32() {
        let mut sys = make_system();
        // sprite_y_start = (y_pos + 33) as u8 = 32 → y_pos = 255 (wraps)
        sys.board.map.region_data_mut(Region::Ram)[0] = 0; // image 0
        sys.board.map.region_data_mut(Region::Ram)[2] = 255; // y_pos → (255 + 33) & 0xFF = 32
        sys.board.map.region_data_mut(Region::Ram)[3] = 0; // x_pos = 0
        // Non-zero pixel in image 0, row 0
        sys.board.gfx_rom[0] = 0x30; // left pixel = 3, right pixel = 0
        sys.board.decode_sprite_cache();

        sys.board.palette_rgb[3] = (0, 0, 255);
        sys.board.palette_bank_per_scanline[32] = 0;
        sys.board.render_scanline(32);

        // Sprite SHOULD appear at x=0 on scanline 32
        let screen_y = 32 - VBEND as usize;
        let off = (screen_y * TIMING.display_width as usize) * 3;
        assert_eq!(
            (
                sys.board.scanline_buffer[off],
                sys.board.scanline_buffer[off + 1],
                sys.board.scanline_buffer[off + 2]
            ),
            (0, 0, 255),
            "Sprite should render on scanline 32"
        );
    }

    #[test]
    fn sprite_transparent_pixel_zero() {
        let mut sys = make_system();
        // Set up sprite at a visible scanline
        sys.board.map.region_data_mut(Region::Ram)[0] = 0; // image 0
        sys.board.map.region_data_mut(Region::Ram)[2] = 255; // y_pos → sprite_y_start = 32
        sys.board.map.region_data_mut(Region::Ram)[3] = 0; // x_pos = 0
        // Pixel data: left=0 (transparent), right=5
        sys.board.gfx_rom[0] = 0x05;
        sys.board.decode_sprite_cache();

        sys.board.palette_rgb[5] = (100, 200, 50);
        // Set background color (palette index 16) to something distinct
        sys.board.palette_rgb[16] = (10, 20, 30);
        sys.board.palette_bank_per_scanline[32] = 0;
        sys.board.render_scanline(32);

        let screen_y = 32 - VBEND as usize;
        // x=0: should be background (transparent sprite pixel)
        let off0 = (screen_y * TIMING.display_width as usize) * 3;
        assert_eq!(
            (
                sys.board.scanline_buffer[off0],
                sys.board.scanline_buffer[off0 + 1],
                sys.board.scanline_buffer[off0 + 2]
            ),
            (10, 20, 30),
            "Sprite pixel 0 should be transparent (background shows through)"
        );
        // x=1: should be sprite color
        let off1 = (screen_y * TIMING.display_width as usize + 1) * 3;
        assert_eq!(
            (
                sys.board.scanline_buffer[off1],
                sys.board.scanline_buffer[off1 + 1],
                sys.board.scanline_buffer[off1 + 2]
            ),
            (100, 200, 50),
            "Sprite pixel 5 should render"
        );
    }

    #[test]
    fn cocktail_flip_reverses_background() {
        let mut sys = make_system();
        sys.board.cocktail_flip = true;

        // Write a distinctive byte to VRAM at bottom-right of normal screen.
        // Flipped: this should appear at top-left.
        // Normal scanline 255 (screen_y=239) maps to VRAM row 239.
        // Flipped scanline 16 reads from src_y = (256-1-16) - 16 = 223 (screen_y=223).
        // Actually: flipped reads src_y = (VBSTART-1-scanline) - VBEND = (255-16) - 16 = 223
        // VRAM row 223, reversed X. Byte 127 in that row (rightmost pair).
        let src_row = 223;
        let vram_offset = src_row * 128 + 127; // rightmost byte of row 223
        sys.board.map.region_data_mut(Region::VideoRam)[vram_offset] = 0xAB; // left=0xA(10), right=0xB(11)
        sys.board.palette_rgb[(10 + 16) as usize] = (111, 0, 0);
        sys.board.palette_rgb[(11 + 16) as usize] = (0, 222, 0);
        sys.board.palette_bank_per_scanline[16] = 0;

        sys.board.render_scanline(16);

        // When flipped, byte 127 becomes the leftmost pair (x_pair=0),
        // and the nibbles swap: right nibble (0xB) becomes left pixel,
        // left nibble (0xA) becomes right pixel.
        let screen_y = 16 - VBEND as usize; // 0
        let off0 = (screen_y * TIMING.display_width as usize) * 3;
        let off1 = (screen_y * TIMING.display_width as usize + 1) * 3;
        assert_eq!(
            (
                sys.board.scanline_buffer[off0],
                sys.board.scanline_buffer[off0 + 1],
                sys.board.scanline_buffer[off0 + 2]
            ),
            (0, 222, 0),
            "Flipped: lower nibble (0xB) should be left pixel"
        );
        assert_eq!(
            (
                sys.board.scanline_buffer[off1],
                sys.board.scanline_buffer[off1 + 1],
                sys.board.scanline_buffer[off1 + 2]
            ),
            (111, 0, 0),
            "Flipped: upper nibble (0xA) should be right pixel"
        );
    }

    // -----------------------------------------------------------------------
    // LS259 latch
    // -----------------------------------------------------------------------

    #[test]
    fn latch_cocktail_flip() {
        let mut sys = make_system();
        // Q7 (bit 7 select) = address bits 6:4 = 0b111, so addr = 0x9070
        Bus::write(&mut sys.board, BusMaster::Cpu(0), 0x9070, 0x01);
        assert!(sys.board.cocktail_flip);
        Bus::write(&mut sys.board, BusMaster::Cpu(0), 0x9070, 0x00);
        assert!(!sys.board.cocktail_flip);
    }

    // -----------------------------------------------------------------------
    // Frame rate
    // -----------------------------------------------------------------------

    #[test]
    fn frame_rate_approximately_59hz() {
        let sys = make_system();
        let hz = sys.frame_rate_hz();
        assert!(
            (59.0..60.0).contains(&hz),
            "Frame rate {hz} Hz not in expected range 59-60 Hz"
        );
    }

    // -----------------------------------------------------------------------
    // Save state
    // -----------------------------------------------------------------------

    #[test]
    fn save_load_round_trip() {
        let mut sys = make_system();

        // Set known state
        sys.board.map.region_data_mut(Region::Ram)[0x100] = 0xAA;
        sys.board.map.region_data_mut(Region::VideoRam)[0x200] = 0xBB;
        sys.board.map.region_data_mut(Region::Nvram)[50] = 0xCC;
        sys.board.palette_bank = 0x1F;
        sys.board.fire_buttons = 0xFE;
        sys.board.coin_start = 0xCE;
        sys.board.cocktail_flip = true;
        sys.board.track_u_pressed = true;
        sys.board.track_r_pressed = true;
        sys.board.last_analog_input = [5, 10];
        sys.board.last_analog_output = [15, 20];
        sys.board.trackball_pos = [25, 30];
        sys.board.sound_data[0x10] = 0x40;
        sys.board.tone_step = 1234;
        sys.board.tone_fraction = 5678;
        sys.board.tone_volume = 0xAB;
        sys.board.audio_clock.set_phase(9012);
        sys.board.irq_pending = true;
        sys.board.firq_pending = true;
        sys.board.clock = 150_000;
        sys.board.cpu_cycles = 120_000;
        sys.board.watchdog_frame_count = 3;

        // Save
        let data = SaveState::save_state(&sys).expect("save_state should return Some");
        let cpu_snap = sys.cpu.snapshot();

        // Mutate everything
        let mut sys2 = make_system();
        sys2.board.map.region_data_mut(Region::Ram)[0x100] = 0xFF;
        sys2.board.clock = 999;

        // Load
        SaveState::load_state(&mut sys2, &data).unwrap();

        // Verify
        assert_eq!(sys2.cpu.snapshot(), cpu_snap);
        assert_eq!(sys2.board.map.region_data_mut(Region::Ram)[0x100], 0xAA);
        assert_eq!(
            sys2.board.map.region_data_mut(Region::VideoRam)[0x200],
            0xBB
        );
        assert_eq!(sys2.board.map.region_data_mut(Region::Nvram)[50], 0xCC);
        assert_eq!(sys2.board.palette_bank, 0x1F);
        assert_eq!(sys2.board.fire_buttons, 0xFE);
        assert_eq!(sys2.board.coin_start, 0xCE);
        assert!(sys2.board.cocktail_flip);
        assert!(sys2.board.track_u_pressed);
        assert!(sys2.board.track_r_pressed);
        assert_eq!(sys2.board.last_analog_input, [5, 10]);
        assert_eq!(sys2.board.last_analog_output, [15, 20]);
        assert_eq!(sys2.board.trackball_pos, [25, 30]);
        assert_eq!(sys2.board.sound_data[0x10], 0x40);
        assert_eq!(sys2.board.tone_step, 1234);
        assert_eq!(sys2.board.tone_fraction, 5678);
        assert_eq!(sys2.board.tone_volume, 0xAB);
        assert_eq!(sys2.board.audio_clock.phase(), 9012);
        assert!(sys2.board.irq_pending);
        assert!(sys2.board.firq_pending);
        assert_eq!(sys2.board.clock, 150_000);
        assert_eq!(sys2.board.cpu_cycles, 120_000);
        assert_eq!(sys2.board.watchdog_frame_count, 3);
    }

    #[test]
    fn save_does_not_include_rom() {
        let mut sys = make_system();
        sys.board.map.region_data_mut(Region::Rom)[0] = 0xDE;
        sys.board.gfx_rom[0] = 0xAD;

        let data = SaveState::save_state(&sys).unwrap();

        let mut sys2 = make_system();
        SaveState::load_state(&mut sys2, &data).unwrap();

        assert_eq!(sys2.board.map.region_data_mut(Region::Rom)[0], 0x00);
        assert_eq!(sys2.board.gfx_rom[0], 0x00);
    }

    #[test]
    fn set_analog_y_updates_trackball() {
        let mut sys = make_system();
        sys.handle_input(InputEvent::Relative {
            id: CTRL_TRACKBALL_Y,
            delta: (6) as f32,
        }); // 6 / 3 = 2
        assert_eq!(sys.board.trackball_pos[0], 2);
    }

    #[test]
    fn set_analog_x_reversed() {
        let mut sys = make_system();
        sys.handle_input(InputEvent::Relative {
            id: CTRL_TRACKBALL_X,
            delta: (9) as f32,
        }); // 9 / 3 = 3
        // X axis is reversed: positive mouse motion (right) subtracts
        assert_eq!(sys.board.trackball_pos[1], 253); // 0u8.wrapping_sub(3)
    }

    #[test]
    fn set_analog_negative_x() {
        let mut sys = make_system();
        sys.handle_input(InputEvent::Relative {
            id: CTRL_TRACKBALL_X,
            delta: (-9) as f32,
        }); // -9 / 3 = -3
        // Negative delta (left) → adds 3 due to reversal
        assert_eq!(sys.board.trackball_pos[1], 3); // 0u8.wrapping_sub(-3 as u8) = wrapping_sub(253) = 3
    }

    #[test]
    fn exposes_two_analog_axes() {
        let sys = make_system();
        let axes: Vec<&str> = sys
            .input_controls()
            .iter()
            .filter(|c| matches!(c.kind, InputKind::AnalogAxis { .. }))
            .map(|c| c.label)
            .collect();
        assert_eq!(axes, vec!["Trackball X", "Trackball Y"]);
    }
}
