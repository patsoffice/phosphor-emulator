//! Galaxian hardware board (Namco, 1979).
//!
//! Shared base for the Galaxian → Scramble → Frogger lineage's simplest tier:
//! a single Zilog Z80 @ 3.072 MHz driving the [`crate::galaxian_video`] engine
//! and the [`GalaxianSound`] discrete sound board, with a 74LS259-style
//! addressable latch for IRQ-enable, starfield-enable, and cocktail flip.
//!
//! Modeled on [`crate::namco_pac::NamcoPacBoard`]: the board owns the CPU,
//! address space, video engine, and sound device and exposes inherent
//! `tick`/`render_frame`/`fill_audio`/bus-dispatch helpers. A game wrapper
//! (added separately) implements [`Bus`] and the frontend capability traits on
//! top of it.
//!
//! Memory map (MAME `galaxian_map`):
//! ```text
//!   0x0000-0x3fff  Program ROM
//!   0x4000-0x47ff  Work RAM        (1 KB, mirrored)
//!   0x5000-0x57ff  Video RAM       (tile codes, mirrored)
//!   0x5800-0x5fff  Object RAM      (scroll/color, sprites, bullets; mirrored)
//!   0x6000-0x67ff  IN0 (r) / lamps+coin (w 0-3) / sound LFO freq (w 4-7)
//!   0x6800-0x6fff  IN1 (r) / sound 74LS259 latch (w)
//!   0x7000-0x77ff  IN2 (r) / 74LS259 latch (w)
//!   0x7800-0x7fff  watchdog (r) / sound pitch (w)
//! ```

use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind, DebugTraceBuffer};
use phosphor_core::core::machine::{
    ActionRole, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, Direction, InputConfigurable,
    InputControl, InputEvent, InputId, InputKind, MachineCore, Nvram, Profilable, SaveState,
};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::watchpoint::DebugAccessSource;
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::state::Z80State;
use phosphor_core::cpu::z80::Z80;
use phosphor_core::cpu::{Cpu, CpuStateTrait};
use phosphor_core::device::GalaxianSound;
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion};

use crate::galaxian_video::{self, GalaxianVideo, GfxBankMode};
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};

/// Audio output rate produced by [`GalaxianBoard::fill_audio`]; matches the
/// frontend's `audio_sample_rate`.
pub const SAMPLE_RATE: u32 = 44_100;

// ---------------------------------------------------------------------------
// Memory map regions
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum Region {
    Rom = 1,
    Ram = 2,
    VideoRam = 3,
    ObjRam = 4,
    Io = 5,
}

// ---------------------------------------------------------------------------
// Input button IDs (shared across the Galaxian family)
// ---------------------------------------------------------------------------
pub const INPUT_COIN: u8 = 0;
pub const INPUT_P1_LEFT: u8 = 1;
pub const INPUT_P1_RIGHT: u8 = 2;
pub const INPUT_P1_FIRE: u8 = 3;
pub const INPUT_P1_START: u8 = 4;
pub const INPUT_P2_START: u8 = 5;
pub const INPUT_P2_LEFT: u8 = 6;
pub const INPUT_P2_RIGHT: u8 = 7;
pub const INPUT_P2_FIRE: u8 = 8;

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------
// Master clock:  18.432 MHz
// CPU clock:     18.432 / 6 = 3.072 MHz
// HTOTAL:        384 px = 192 CPU cycles/scanline; VTOTAL: 264 lines
// Visible:       native 256×224 (scanlines 16..=239), rotated 90° CCW → 224×256
// Frame:         192 × 264 = 50688 CPU cycles → 3072000 / 50688 ≈ 60.61 Hz

pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 3_072_000,
    cycles_per_scanline: 192,
    total_scanlines: 264,
    // Native (pre-orientation) framebuffer: the board declares ROT90 (plus any
    // cocktail flip) and the frontend rotates centrally, so these are the
    // unrotated dimensions.
    display_width: galaxian_video::NATIVE_WIDTH as u32, // 256
    display_height: galaxian_video::NATIVE_HEIGHT as u32, // 224
    display_aspect: Some((3, 4)),                       // portrait tube as viewed (after ROT90)
};

/// Visible scanlines per frame (native rows rendered into the framebuffer).
pub const VISIBLE_LINES: u64 = galaxian_video::NATIVE_HEIGHT as u64;

// ---------------------------------------------------------------------------
// GalaxianBoard
// ---------------------------------------------------------------------------

/// A Galaxian-family bus: the shared board, or a game view over it (Pisces
/// re-wires one write line).
///
/// [`tick`] is generic over this trait, so every access resolves to a direct
/// call rather than a vtable entry.
pub trait GalaxianBus: Bus<Address = u16, Data = u8> {
    fn board(&mut self) -> &mut GalaxianBoard;
}

/// The board is a complete bus for the games that add nothing to it.
impl GalaxianBus for GalaxianBoard {
    #[inline]
    fn board(&mut self) -> &mut GalaxianBoard {
        self
    }
}

/// One CPU cycle: board work, the Z80, then the sound tick.
///
/// The CPU lives on the machine and the bus is the board (or a game view over
/// it), so this takes them as separate borrows and dispatches at a concrete
/// type. This is the debugger's path — it tests the frame position on every
/// cycle; a whole frame goes through [`run_scanlines`], which hoists that out.
#[inline]
pub fn tick<B: GalaxianBus>(cpu: &mut Z80, bus: &mut B) {
    let board = bus.board();
    let frame_cycle = board.clock % TIMING.cycles_per_frame();
    if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
        board.begin_scanline(frame_cycle / TIMING.cycles_per_scanline);
    }
    step_cycle(cpu, bus);
}

/// Run `cycles` CPU cycles, scanline-outer and cycle-inner. The caller must
/// start on a scanline boundary and pass a multiple of `cycles_per_scanline`;
/// the debugger's off-boundary stepping goes through [`tick`] instead.
pub fn run_scanlines<B: GalaxianBus>(cpu: &mut Z80, bus: &mut B, cycles: u64) {
    debug_assert!(
        bus.board().clock.is_multiple_of(TIMING.cycles_per_scanline)
            && cycles.is_multiple_of(TIMING.cycles_per_scanline),
        "run_scanlines must start on a scanline boundary and run whole scanlines"
    );
    for _ in 0..cycles / TIMING.cycles_per_scanline {
        let board = bus.board();
        let scanline = board.clock % TIMING.cycles_per_frame() / TIMING.cycles_per_scanline;
        board.begin_scanline(scanline);
        for _ in 0..TIMING.cycles_per_scanline {
            step_cycle(cpu, bus);
        }
    }
}

/// Run one frame's worth of cycles. Whole scanlines go through
/// [`run_scanlines`]; a partial scanline at either end (only after the debugger
/// has left the clock off-boundary) goes through [`tick`].
pub fn run_frame<B: GalaxianBus>(cpu: &mut Z80, bus: &mut B) {
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
fn step_cycle<B: GalaxianBus>(cpu: &mut Z80, bus: &mut B) {
    bus.board().begin_cycle_inner(cpu);
    cpu.execute_cycle(bus, BusMaster::Cpu(0));
    bus.board().end_cycle();
}

/// Galaxian hardware base (Z80 @ 3.072 MHz, tilemap + sprites + starfield +
/// discrete sound).
///
/// The board is everything the Z80 talks *to* — and, since every game on it
/// decodes the same way, it implements [`Bus`] itself. The CPU lives on the
/// game wrapper.
#[derive(BusDebug, DebugTrace)]
pub struct GalaxianBoard {
    #[debug_map(cpu = 0)]
    pub(crate) map: AddressSpace16,

    pub(crate) video: GalaxianVideo,

    #[debug_device("Galaxian Sound")]
    pub(crate) sound: GalaxianSound,

    // Input ports (active-high: 0x00 = nothing pressed). IN2 is DIP-only.
    pub(crate) in0: u8,
    pub(crate) in1: u8,
    pub(crate) in2: u8,

    // 74LS259 latch output: NMI enable (gates the VBLANK NMI).
    pub(crate) irq_enabled: bool,

    // VBLANK NMI latch (edge-triggered, gated by irq_enabled).
    pub(crate) vblank_nmi_pending: bool,

    // Memory-map layout: base Galaxian (false) puts RAM/I/O at 0x4000-0x7fff;
    // the Moon Cresta layout (true) shifts them to 0x8000-0xbfff and moves a
    // couple of I/O lines (GFX bank latch + IRQ-enable).
    pub(crate) mooncrst_map: bool,

    // Timing
    pub(crate) clock: u64,
    pub(crate) watchdog_counter: u32,

    // Debug event ring (observer state — never saved).
    #[debug_events]
    pub(crate) debug_trace: DebugTraceBuffer,
}

impl Default for GalaxianBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl GalaxianBoard {
    pub fn new() -> Self {
        Self {
            map: Self::build_map(),
            video: GalaxianVideo::new(),
            sound: GalaxianSound::new(SAMPLE_RATE),
            in0: 0x00,
            in1: 0x00,
            in2: 0x00,
            irq_enabled: false,
            vblank_nmi_pending: false,
            mooncrst_map: false,
            clock: 0,
            watchdog_counter: 0,
            debug_trace: DebugTraceBuffer::new(),
        }
    }

    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(
            Region::Rom,
            "Program ROM",
            0x0000,
            0x4000,
            AccessKind::ReadOnly,
        )
        .region(
            Region::Ram,
            "Work RAM",
            0x4000,
            0x0400,
            AccessKind::ReadWrite,
        )
        .region(
            Region::VideoRam,
            "Video RAM",
            0x5000,
            0x0400,
            AccessKind::ReadWrite,
        )
        .region(
            Region::ObjRam,
            "Object RAM",
            0x5800,
            0x0100,
            AccessKind::ReadWrite,
        )
        .region(Region::Io, "I/O", 0x6000, 0x2000, AccessKind::Io);
        // Hardware address mirrors (incomplete decode).
        map.mirror(0x4400, 0x4000, 0x0400) // RAM
            .mirror(0x5400, 0x5000, 0x0400); // Video RAM
        for i in 1..8 {
            map.mirror(0x5800 + i * 0x100, 0x5800, 0x0100); // Object RAM ×8
        }
        map
    }

    // -----------------------------------------------------------------------
    // Core tick
    // -----------------------------------------------------------------------

    /// Work that only happens on the first cycle of a scanline: the starfield
    /// advance at the top of the frame, rendering the line, and the VBLANK NMI
    /// edges.
    ///
    /// Called once per scanline from [`run_scanlines`] and, for the debugger's
    /// single-step path, from [`tick`] when the clock lands on a boundary.
    fn begin_scanline(&mut self, scanline: u64) {
        // Advance the starfield scroll once at the top of each frame.
        if scanline == 0 {
            self.video.begin_frame();
        }

        // Per-scanline rendering, before the CPU runs.
        if scanline < VISIBLE_LINES {
            let vram = self.map.region_data(Region::VideoRam);
            let objram = self.map.region_data(Region::ObjRam);
            self.video.render_scanline(scanline as usize, vram, objram);
        }

        // VBLANK NMI: assert at the start of VBLANK (first non-visible line).
        if scanline == VISIBLE_LINES {
            self.vblank_nmi_pending = true;
            if self.debug_trace.enabled() {
                self.debug_trace.record(DebugEvent {
                    cpu_index: Some(0),
                    detail: Some(if self.irq_enabled {
                        "VBLANK NMI"
                    } else {
                        "VBLANK NMI (masked)"
                    }),
                    ..DebugEvent::new(
                        self.clock,
                        DebugAccessSource::Unknown,
                        DebugEventKind::InterruptAssert,
                    )
                });
            }
        }
        // Clear the NMI latch at the frame boundary (end of VBLANK).
        if scanline == 0 && self.clock > 0 {
            self.vblank_nmi_pending = false;
        }
    }

    /// Per-cycle board work that runs before the CPU, with no frame-position
    /// test in it.
    fn begin_cycle_inner(&mut self, cpu: &Z80) {
        // Latch debug attribution context before CPU execution.
        if self.map.has_any_watchpoints() || self.debug_trace.enabled() {
            let pc = cpu.at_instruction_boundary().then_some(cpu.pc as u32);
            self.map.latch_access_context(self.clock, pc);
        }
    }

    /// Board work after the CPU's cycle.
    fn end_cycle(&mut self) {
        self.sound.tick(1);
        self.clock += 1;
        self.watchdog_counter += 1;
    }

    /// Main CPU: VBLANK NMI, edge-triggered and gated by the IRQ-enable latch.
    /// Interrupt lines as the CPU sees them. Named apart from the `Bus` method
    /// so the impl can call it without recursing.
    pub fn interrupt_state(&self, target: BusMaster) -> InterruptState {
        match target {
            BusMaster::Cpu(0) => InterruptState {
                nmi: self.vblank_nmi_pending && self.irq_enabled,
                irq: false,
                firq: false,
                irq_vector: 0,
                irq_level: 0,
            },
            _ => InterruptState::default(),
        }
    }

    // -----------------------------------------------------------------------
    // Bus dispatch helpers — called from game-wrapper Bus impls
    // -----------------------------------------------------------------------

    fn trace_access(
        &mut self,
        kind: DebugEventKind,
        addr: u16,
        value: u8,
        device: Option<&'static str>,
        detail: Option<&'static str>,
    ) {
        self.debug_trace.record(DebugEvent {
            cpu_index: Some(0),
            pc: self.map.latched_pc(),
            addr: Some(addr as u32),
            value: Some(value as u32),
            width: 1,
            region: self.map.region_at(addr).map(|r| r.name),
            device,
            detail,
            ..DebugEvent::new(self.clock, DebugAccessSource::Cpu(0), kind)
        });
    }

    /// Shared memory read for all Galaxian hardware.
    pub fn bus_read_common(&mut self, addr: u16) -> u8 {
        let addr = self.norm(addr);
        let data = match addr {
            // ROM, Work RAM (+ its 0x4400 mirror), Video RAM (+ mirror) and
            // Object RAM (×8). 0x4800-0x4FFF decodes to nothing on the board,
            // so it floats high rather than reaching backing memory — the
            // match arms below are coarser than the address map, and a read
            // there would otherwise index an unbacked region.
            0x0000..=0x47ff | 0x5000..=0x5fff => self.map.read_backing(addr),
            0x4800..=0x4fff => 0xff,
            0x6000..=0x67ff => self.in0,
            0x6800..=0x6fff => self.in1,
            0x7000..=0x77ff => self.in2,
            0x7800..=0x7fff => {
                // Reading the watchdog resets it.
                self.watchdog_counter = 0;
                0xff
            }
            _ => 0xff,
        };

        self.map.watch_read(0, BusMaster::Cpu(0), addr, data);
        if self.debug_trace.enabled() && (0x6000..0x8000).contains(&addr) {
            self.trace_access(DebugEventKind::DeviceRead, addr, data, None, None);
        }
        data
    }

    /// Shared memory write for all Galaxian hardware.
    pub fn bus_write_common(&mut self, addr: u16, data: u8) {
        self.map.watch_write(0, BusMaster::Cpu(0), addr, data);
        let addr = self.norm(addr);

        if self.debug_trace.enabled() {
            let (kind, device, detail) = match addr {
                0x6000..=0x67ff if addr & 7 >= 4 => (
                    DebugEventKind::DeviceWrite,
                    Some("Galaxian Sound"),
                    Some("LFO freq"),
                ),
                0x6800..=0x6fff => (
                    DebugEventKind::DeviceWrite,
                    Some("Galaxian Sound"),
                    Some("sound latch"),
                ),
                0x7000..=0x77ff => (
                    DebugEventKind::DeviceWrite,
                    Some("I/O latch"),
                    Some(match addr & 7 {
                        1 => "IRQ enable",
                        4 => "stars enable",
                        6 => "flip screen X",
                        7 => "flip screen Y",
                        _ => "latch bit",
                    }),
                ),
                0x7800..=0x7fff => (
                    DebugEventKind::DeviceWrite,
                    Some("Galaxian Sound"),
                    Some("pitch"),
                ),
                0x5000..=0x5fff => (DebugEventKind::MemoryWrite, None, None),
                _ => (DebugEventKind::IoWrite, None, None),
            };
            self.trace_access(kind, addr, data, device, detail);
        }

        match addr {
            // Work RAM, Video RAM, Object RAM. 0x4800-0x4FFF decodes to
            // nothing (see `bus_read_common`), so a write there goes nowhere.
            0x4000..=0x47ff | 0x5000..=0x5fff => self.map.write_backing(addr, data),
            0x4800..=0x4fff => {}

            // 0x6000 block: lines 4-7 are the sound LFO ("wolf-whistle") DAC.
            // On base Galaxian lines 0-3 are lamps / coin (not modeled); on the
            // Moon Cresta map lines 0-2 instead drive the GFX-bank latch.
            0x6000..=0x67ff => {
                let line = (addr & 7) as u8;
                if self.mooncrst_map && line < 3 {
                    self.video.set_gfxbank(line, data);
                } else if line >= 4 {
                    self.sound.lfo_freq_w(line - 4, data);
                }
            }

            // 0x6800 block: the sound 74LS259 latch (FS1-3 / HIT / FIRE / VOL).
            0x6800..=0x6fff => self.sound.sound_w((addr & 7) as u8, data),

            // 0x7000 block: 74LS259 addressable latch (line = addr & 7). IRQ
            // enable is line 1 on Galaxian, line 0 on the Moon Cresta map.
            0x7000..=0x77ff => {
                let irq_line = if self.mooncrst_map { 0 } else { 1 };
                match addr & 7 {
                    l if l == irq_line => {
                        self.irq_enabled = data & 1 != 0;
                        if !self.irq_enabled {
                            self.vblank_nmi_pending = false;
                        }
                    }
                    4 => self.video.set_stars_enabled(data & 1 != 0),
                    6 => self.video.set_flip_x(data & 1 != 0),
                    7 => self.video.set_flip_y(data & 1 != 0),
                    _ => {}
                }
            }

            // 0x7800 block: background pitch latch (watchdog reset is on read).
            0x7800..=0x7fff => self.sound.pitch_w(data),

            _ => { /* ROM or unmapped: ignored */ }
        }
    }

    // -----------------------------------------------------------------------
    // ROM loading
    // -----------------------------------------------------------------------

    pub fn load_program_rom(&mut self, data: &[u8]) {
        self.map.load_region(Region::Rom, data);
    }

    pub fn load_gfx_rom(&mut self, gfx_data: &[u8]) {
        self.video.load_gfx_rom(gfx_data);
    }

    pub fn load_color_prom(&mut self, prom: &[u8]) {
        self.video.load_color_prom(prom);
    }

    /// Select the video GFX bank-switching scheme (banked board variants set
    /// this at construction; base Galaxian leaves it [`GfxBankMode::None`]).
    pub fn set_gfx_mode(&mut self, mode: GfxBankMode) {
        self.video.set_gfx_mode(mode);
    }

    /// Drive one bit of the GFX-bank latch. Called by banked game wrappers from
    /// their own bus decode (the bank address/index varies per game).
    pub fn set_gfxbank(&mut self, index: u8, data: u8) {
        self.video.set_gfxbank(index, data);
    }

    /// Switch to the Moon Cresta memory map (RAM/I/O at 0x8000-0xbfff, with the
    /// GFX-bank latch + IRQ-enable moved). Set once at construction.
    pub fn set_mooncrst_map(&mut self, on: bool) {
        self.mooncrst_map = on;
    }

    /// Normalize a CPU address into the board's internal (Galaxian) address
    /// space. The Moon Cresta layout is the Galaxian map shifted up 0x4000 for
    /// everything above ROM, so map storage stays at the Galaxian positions and
    /// only the decode shifts.
    fn norm(&self, addr: u16) -> u16 {
        if self.mooncrst_map && addr >= 0x8000 {
            addr - 0x4000
        } else {
            addr
        }
    }

    // -----------------------------------------------------------------------
    // CPU state / video output
    // -----------------------------------------------------------------------

    pub fn clock(&self) -> u64 {
        self.clock
    }

    pub fn render_frame(&self, buffer: &mut [u8]) {
        self.video.render_frame(buffer);
    }

    /// Declarative orientation (base ROT90 composed with the live cocktail
    /// flip); applied centrally by the frontend.
    pub fn orientation(&self) -> phosphor_core::core::machine::Orientation {
        self.video.orientation()
    }

    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.sound.fill_audio(buffer)
    }

    // -----------------------------------------------------------------------
    // Input — dispatched to active-high port bits by a game wrapper.
    // -----------------------------------------------------------------------

    pub fn handle_input(&mut self, button: u8, pressed: bool) {
        match button {
            INPUT_COIN => crate::set_bit_active_high(&mut self.in0, 0, pressed),
            INPUT_P1_LEFT => crate::set_bit_active_high(&mut self.in0, 2, pressed),
            INPUT_P1_RIGHT => crate::set_bit_active_high(&mut self.in0, 3, pressed),
            INPUT_P1_FIRE => crate::set_bit_active_high(&mut self.in0, 4, pressed),
            INPUT_P1_START => crate::set_bit_active_high(&mut self.in1, 0, pressed),
            INPUT_P2_START => crate::set_bit_active_high(&mut self.in1, 1, pressed),
            INPUT_P2_LEFT => crate::set_bit_active_high(&mut self.in1, 2, pressed),
            INPUT_P2_RIGHT => crate::set_bit_active_high(&mut self.in1, 3, pressed),
            INPUT_P2_FIRE => crate::set_bit_active_high(&mut self.in1, 4, pressed),
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Reset
    // -----------------------------------------------------------------------

    /// Reset all board state except ROMs, GFX/palette, and the input/DIP ports
    /// (those are external switches that a reset signal does not move). The
    /// caller resets the CPU separately (requires `bus_split`).
    pub fn reset_board(&mut self) {
        self.video.reset();
        phosphor_core::device::Device::reset(&mut self.sound);
        self.irq_enabled = false;
        self.vblank_nmi_pending = false;
        self.clock = 0;
        self.watchdog_counter = 0;
        self.map.region_data_mut(Region::Ram).fill(0);
        self.map.region_data_mut(Region::VideoRam).fill(0);
        self.map.region_data_mut(Region::ObjRam).fill(0);
    }

    // -----------------------------------------------------------------------
    // Debug
    // -----------------------------------------------------------------------

    /// Whether the CPU is at an instruction boundary. It lives on the machine,
    /// which passes it back in.
    pub fn instruction_boundaries(cpu: &Z80) -> u32 {
        u32::from(cpu.at_instruction_boundary())
    }
}

impl Saveable for GalaxianBoard {
    fn save_state(&self, w: &mut StateWriter) {
        // The CPU is saved by the machine, which owns it.
        w.write_bytes(self.map.region_data(Region::Ram));
        w.write_bytes(self.map.region_data(Region::VideoRam));
        w.write_bytes(self.map.region_data(Region::ObjRam));
        self.video.save_state(w);
        self.sound.save_state(w);
        w.write_u8(self.in0);
        w.write_u8(self.in1);
        w.write_u8(self.in2);
        w.write_bool(self.irq_enabled);
        w.write_bool(self.vblank_nmi_pending);
        w.write_u64_le(self.clock);
        w.write_u32_le(self.watchdog_counter);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        // The CPU is loaded by the machine, which owns it.
        r.read_bytes_into(self.map.region_data_mut(Region::Ram))?;
        r.read_bytes_into(self.map.region_data_mut(Region::VideoRam))?;
        r.read_bytes_into(self.map.region_data_mut(Region::ObjRam))?;
        self.video.load_state(r)?;
        self.sound.load_state(r)?;
        self.in0 = r.read_u8()?;
        self.in1 = r.read_u8()?;
        self.in2 = r.read_u8()?;
        self.irq_enabled = r.read_bool()?;
        self.vblank_nmi_pending = r.read_bool()?;
        self.clock = r.read_u64_le()?;
        self.watchdog_counter = r.read_u32_le()?;
        Ok(())
    }
}

// ===========================================================================
// Galaxian game (Namco/Midway, 1979) — wrapper around GalaxianBoard
// ===========================================================================

// ---------------------------------------------------------------------------
// ROM definitions ("galaxian" Namco/Midway set)
// ---------------------------------------------------------------------------

/// Program ROM: 0x0000-0x27FF (five 2 KB chips); the rest of the 16 KB region
/// is unmapped.
pub static GALAXIAN_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "galmidw.u",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0x745e2d61],
        },
        RomEntry {
            name: "galmidw.v",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x9c999a40],
        },
        RomEntry {
            name: "galmidw.w",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0xb5894925],
        },
        RomEntry {
            name: "galmidw.y",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0x6b3ca10b],
        },
        RomEntry {
            name: "7l",
            size: 0x0800,
            offset: 0x2000,
            crc32: &[0x1b933207],
        },
    ],
};

/// GFX ROM: 4 KB (two 2 KB bitplane halves shared by tiles and sprites).
pub static GALAXIAN_GFX_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[
        RomEntry {
            name: "1h.bin",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0x39fb43a4],
        },
        RomEntry {
            name: "1k.bin",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x7e3f56a2],
        },
    ],
};

/// Palette PROM (32 bytes).
pub static GALAXIAN_COLOR_PROM: RomRegion = RomRegion {
    size: 0x0020,
    entries: &[RomEntry {
        name: "6l.bpr",
        size: 0x0020,
        offset: 0x0000,
        crc32: &[0xc3ac9467],
    }],
};

// ---------------------------------------------------------------------------
// DIP switches
// ---------------------------------------------------------------------------
//
// Galaxian's DIPs are not a dedicated DSW byte; they share the IN0/IN1/IN2
// input ports at fixed bit positions (the live input bits occupy the rest).
// Each bank below maps to one port and exposes only its DIP bits; the masks
// keep `set_dip_bank_value` from disturbing the input bits in the same byte.

const DIP0_MASK: u8 = 0x20; // IN0: Cabinet
const DIP1_MASK: u8 = 0xc0; // IN1: Coinage
const DIP2_MASK: u8 = 0x07; // IN2: Bonus Life + Lives
/// Factory default DIP bits: IN0/IN1 = 0, IN2 = 7000 bonus + 3 lives (0x04).
const DIP2_DEFAULT: u8 = 0x04;

pub(crate) const GALAXIAN_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "IN0",
        options: &[DipOption {
            name: "Cabinet",
            mask: DIP0_MASK,
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
    },
    DipSwitchBank {
        name: "IN1",
        options: &[DipOption {
            name: "Coinage",
            mask: DIP1_MASK,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "1 Coin/1 Credit",
                    value: 0x00,
                },
                DipChoice {
                    label: "2 Coins/1 Credit",
                    value: 0x40,
                },
                DipChoice {
                    label: "1 Coin/2 Credits",
                    value: 0x80,
                },
                DipChoice {
                    label: "Free Play",
                    value: 0xc0,
                },
            ],
        }],
    },
    DipSwitchBank {
        name: "IN2",
        options: &[
            DipOption {
                name: "Bonus Life",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "7000",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "10000",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "12000",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "20000",
                        value: 0x03,
                    },
                ],
            },
            DipOption {
                name: "Lives",
                mask: 0x04,
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
                ],
            },
        ],
    },
];

// ---------------------------------------------------------------------------
// Input controls
// ---------------------------------------------------------------------------

/// Galaxian is a 2-way (left/right) shooter with one fire button; player 2 uses
/// the same controls on a cocktail cabinet. `InputId`s reuse the board's
/// `INPUT_*` numbering so `handle_input` shares one id space.
pub const GALAXIAN_CONTROLS: &[InputControl] = &[
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
        id: InputId(INPUT_P1_FIRE as u16),
        stable_name: "p1_fire",
        label: "P1 Fire",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
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
        id: InputId(INPUT_P1_START as u16),
        stable_name: "p1_start",
        label: "P1 Start",
        kind: InputKind::Start,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_START,
    },
    InputControl {
        id: InputId(INPUT_P2_START as u16),
        stable_name: "p2_start",
        label: "P2 Start",
        kind: InputKind::Start,
        player: Some(2),
        default_bindings: crate::input_defaults::P2_START,
    },
    InputControl {
        id: InputId(INPUT_P2_LEFT as u16),
        stable_name: "p2_left",
        label: "P2 Left",
        kind: InputKind::DigitalDirection {
            direction: Direction::Left,
        },
        player: Some(2),
        default_bindings: crate::input_defaults::P2_LEFT,
    },
    InputControl {
        id: InputId(INPUT_P2_RIGHT as u16),
        stable_name: "p2_right",
        label: "P2 Right",
        kind: InputKind::DigitalDirection {
            direction: Direction::Right,
        },
        player: Some(2),
        default_bindings: crate::input_defaults::P2_RIGHT,
    },
    InputControl {
        id: InputId(INPUT_P2_FIRE as u16),
        stable_name: "p2_fire",
        label: "P2 Fire",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(2),
        default_bindings: &[],
    },
];

// ---------------------------------------------------------------------------
// GalaxianSystem — the Galaxian game wrapped around GalaxianBoard
// ---------------------------------------------------------------------------

/// Galaxian (Namco/Midway, 1979): Z80 @ 3.072 MHz, tilemap + sprites +
/// hardware starfield, discrete sound. Vertical monitor, 224×256 display.
#[derive(phosphor_macros::Saveable, BusDebug)]
pub struct GalaxianSystem {
    /// The Z80 is held beside the board, which is its bus.
    #[debug_cpu("Z80")]
    pub cpu: Z80,

    #[debug_bus]
    pub board: GalaxianBoard,
}

impl GalaxianSystem {
    pub fn new() -> Self {
        let mut board = GalaxianBoard::new();
        // Apply factory-default DIP positions (IN0/IN1 = 0).
        board.in2 = DIP2_DEFAULT;
        Self {
            cpu: Z80::new(),
            board,
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        self.board
            .load_program_rom(&GALAXIAN_PROGRAM_ROM.load(rom_set)?);
        self.board.load_gfx_rom(&GALAXIAN_GFX_ROM.load(rom_set)?);
        self.board
            .load_color_prom(&GALAXIAN_COLOR_PROM.load(rom_set)?);
        Ok(())
    }

    pub fn get_cpu_state(&self) -> Z80State {
        self.cpu.snapshot()
    }

    pub fn clock(&self) -> u64 {
        self.board.clock()
    }

    /// One CPU cycle. Returns 1 at an instruction boundary (for the debugger,
    /// which steps instructions rather than cycles).
    pub fn step_cycle(&mut self) -> u32 {
        tick(&mut self.cpu, &mut self.board);
        GalaxianBoard::instruction_boundaries(&self.cpu)
    }

    /// Read the CPU-facing bus, side effects and all. Distinct from the
    /// debugger's `BusDebug::peek`/`poke`, which avoid side effects.
    pub fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        Bus::read(&mut self.board, master, addr)
    }

    /// Write the CPU-facing bus, side effects and all. See [`Self::bus_read`].
    pub fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        Bus::write(&mut self.board, master, addr, data);
    }
}

impl Default for GalaxianSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus implementation
// ---------------------------------------------------------------------------

// The board is the bus for every Galaxian-family game: they all decode the
// same way, differing only in the map layout flag and GFX banking mode the
// board already carries.
impl Bus for GalaxianBoard {
    type Address = u16;
    type Data = u8;

    #[inline]
    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        self.bus_read_common(addr)
    }

    #[inline]
    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        self.bus_write_common(addr, data);
    }

    #[inline]
    fn io_read(&mut self, _master: BusMaster, _addr: u16) -> u8 {
        0xFF // Galaxian uses no Z80 I/O ports (all I/O is memory-mapped)
    }

    #[inline]
    fn io_write(&mut self, _master: BusMaster, _addr: u16, _data: u8) {}

    #[inline]
    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false // No DMA hardware
    }

    #[inline]
    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        self.interrupt_state(target)
    }
}

// ---------------------------------------------------------------------------
// Trait implementations
// ---------------------------------------------------------------------------

crate::impl_board_delegation!(GalaxianSystem, board, TIMING, orientation, split_cpu);

impl MachineCore for GalaxianSystem {
    crate::machine_core_metadata!("galaxian", TIMING);

    fn gfx_sheets(&self) -> Vec<phosphor_core::core::machine::GfxSheet<'_>> {
        use phosphor_core::core::machine::GfxSheet;
        let v = &self.board.video;
        vec![
            GfxSheet {
                name: "chars",
                cache: v.tile_cache(),
                palette: v.palette_rgb(),
            },
            GfxSheet {
                name: "sprites",
                cache: v.sprite_cache(),
                palette: v.palette_rgb(),
            },
        ]
    }

    fn run_frame(&mut self) {
        run_frame(&mut self.cpu, &mut self.board);
    }

    fn reset(&mut self) {
        self.board.reset_board();
        self.cpu.reset(&mut self.board, BusMaster::Cpu(0));
    }
}

impl SaveState for GalaxianSystem {
    crate::machine_save_state!();
}

impl Nvram for GalaxianSystem {}
impl Profilable for GalaxianSystem {}

impl InputConfigurable for GalaxianSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        GALAXIAN_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        if let InputEvent::Button { id, pressed } = event {
            self.board.handle_input(id.0 as u8, pressed);
        }
    }
}

crate::impl_dip_switches!(
    GalaxianSystem,
    GALAXIAN_DIP_BANKS,
    board.in0 & DIP0_MASK,
    board.in1 & DIP1_MASK,
    board.in2 & DIP2_MASK
);

crate::impl_board_debug_trace!(GalaxianSystem, board);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

crate::register_machine!(GalaxianSystem, "galaxian", &["galaxian"], GALAXIAN_CONTROLS);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::DipSwitches;

    #[test]
    fn memory_map_mirrors_fold_to_one_backing() {
        let mut board = GalaxianBoard::new();
        // RAM mirror: 0x4400 aliases 0x4000.
        board.bus_write_common(0x4000, 0xAA);
        assert_eq!(board.bus_read_common(0x4400), 0xAA);
        // Video RAM mirror: 0x5400 aliases 0x5000.
        board.bus_write_common(0x5123, 0x5A);
        assert_eq!(board.bus_read_common(0x5523), 0x5A);
        // Object RAM mirror: 0x5900 aliases 0x5800.
        board.bus_write_common(0x5840, 0x33);
        assert_eq!(board.bus_read_common(0x5940), 0x33);
        assert_eq!(board.bus_read_common(0x5f40), 0x33);
    }

    #[test]
    fn input_ports_read_back() {
        let mut board = GalaxianBoard::new();
        board.in0 = 0x12;
        board.in1 = 0x34;
        board.in2 = 0x56;
        assert_eq!(board.bus_read_common(0x6000), 0x12);
        assert_eq!(board.bus_read_common(0x6800), 0x34);
        assert_eq!(board.bus_read_common(0x7000), 0x56);
    }

    #[test]
    fn ls259_latch_drives_irq_stars_and_flip() {
        let mut board = GalaxianBoard::new();
        board.bus_write_common(0x7001, 0x01); // IRQ enable
        board.bus_write_common(0x7004, 0x01); // stars enable
        board.bus_write_common(0x7006, 0x01); // flip X
        board.bus_write_common(0x7007, 0x01); // flip Y
        assert!(board.irq_enabled);
        assert!(board.video.stars_enabled());
        assert!(board.video.flip_x());
        assert!(board.video.flip_y());

        // Clearing IRQ-enable also clears any pending NMI.
        board.vblank_nmi_pending = true;
        board.bus_write_common(0x7001, 0x00);
        assert!(!board.irq_enabled);
        assert!(!board.vblank_nmi_pending);
    }

    #[test]
    fn nmi_gated_by_irq_enable() {
        let mut board = GalaxianBoard::new();
        board.vblank_nmi_pending = true;
        // Disabled latch → no NMI.
        assert!(!board.check_interrupts(BusMaster::Cpu(0)).nmi);
        board.irq_enabled = true;
        assert!(board.check_interrupts(BusMaster::Cpu(0)).nmi);
        // Other CPUs never see this NMI.
        assert!(!board.check_interrupts(BusMaster::Cpu(1)).nmi);
    }

    #[test]
    fn handle_input_sets_active_high_bits() {
        let mut board = GalaxianBoard::new();
        board.handle_input(INPUT_COIN, true);
        board.handle_input(INPUT_P1_FIRE, true);
        board.handle_input(INPUT_P2_START, true);
        assert_eq!(board.in0, 0b0001_0001); // coin (bit0) + fire (bit4)
        assert_eq!(board.in1, 0b0000_0010); // P2 start (bit1)
        board.handle_input(INPUT_COIN, false);
        assert_eq!(board.in0, 0b0001_0000);
    }

    #[test]
    fn rom_writes_are_ignored() {
        let mut board = GalaxianBoard::new();
        let mut rom = vec![0u8; 0x4000];
        rom[0] = 0x12;
        board.load_program_rom(&rom);
        board.bus_write_common(0x0000, 0xFF);
        assert_eq!(board.bus_read_common(0x0000), 0x12);
    }

    #[test]
    fn watchdog_reset_on_read_only() {
        let mut board = GalaxianBoard::new();
        // Reading 0x7800 resets the watchdog.
        board.watchdog_counter = 999;
        let _ = board.bus_read_common(0x7800);
        assert_eq!(board.watchdog_counter, 0);
        // Writing 0x7800 is the sound pitch latch, not a watchdog reset.
        board.watchdog_counter = 999;
        board.bus_write_common(0x7800, 0x80);
        assert_eq!(board.watchdog_counter, 999);
    }

    #[test]
    fn sound_register_writes_dispatch_and_produce_audio() {
        use phosphor_core::core::debug::Debuggable;
        let mut board = GalaxianBoard::new();
        // pitch (0x7800), LFO line 2 (0x6006), sound latch FIRE (0x6805).
        board.bus_write_common(0x7800, 0xC4);
        board.bus_write_common(0x6006, 0x01);
        board.bus_write_common(0x6805, 0x01);
        let regs = board.sound.debug_registers();
        let by = |name: &str| regs.iter().find(|r| r.name == name).unwrap().value;
        assert_eq!(by("PITCH"), 0xC4);
        assert_eq!(by("LFO"), 0b0100); // line 2 set
        assert_eq!(by("LATCH"), 0b0010_0000); // FIRE = line 5

        // Advancing the sound circuit yields samples (the board's tick() calls
        // sound.tick(1) per CPU cycle; here we drive it directly).
        board.sound.tick(4000);
        let mut buf = [0i16; 64];
        assert!(board.fill_audio(&mut buf) > 0);
    }

    #[test]
    fn save_load_round_trip() {
        let mut board = GalaxianBoard::new();
        board.map.region_data_mut(Region::VideoRam)[0x100] = 0xAA;
        board.map.region_data_mut(Region::ObjRam)[0x42] = 0xBB;
        board.map.region_data_mut(Region::Ram)[0x10] = 0xCC;
        board.in0 = 0x12;
        board.in1 = 0x34;
        board.in2 = 0x56;
        board.irq_enabled = true;
        board.vblank_nmi_pending = true;
        board.video.set_stars_enabled(true);
        board.bus_write_common(0x7800, 0x9A); // sound pitch latch
        board.clock = 100_000;
        board.watchdog_counter = 99;

        let mut w = StateWriter::new();
        board.save_state(&mut w);
        let bytes = w.into_vec();

        let mut board2 = GalaxianBoard::new();
        board2.map.region_data_mut(Region::VideoRam)[0x100] = 0xFF;
        board2.clock = 7;
        let mut r = StateReader::new(&bytes);
        board2.load_state(&mut r).unwrap();

        // CPU state is saved by the machine, not the board.
        assert_eq!(board2.map.region_data(Region::VideoRam)[0x100], 0xAA);
        assert_eq!(board2.map.region_data(Region::ObjRam)[0x42], 0xBB);
        assert_eq!(board2.map.region_data(Region::Ram)[0x10], 0xCC);
        assert_eq!(board2.in0, 0x12);
        assert_eq!(board2.in1, 0x34);
        assert_eq!(board2.in2, 0x56);
        assert!(board2.irq_enabled);
        assert!(board2.vblank_nmi_pending);
        assert!(board2.video.stars_enabled());
        assert_eq!(board2.clock, 100_000);
        assert_eq!(board2.watchdog_counter, 99);
        // Sound device state survives too (PITCH register restored).
        use phosphor_core::core::debug::Debuggable;
        let pitch = board2
            .sound
            .debug_registers()
            .iter()
            .find(|r| r.name == "PITCH")
            .unwrap()
            .value;
        assert_eq!(pitch, 0x9A);
    }

    #[test]
    fn save_does_not_include_rom() {
        let mut board = GalaxianBoard::new();
        board.map.region_data_mut(Region::Rom)[0] = 0xDE;
        let mut w = StateWriter::new();
        board.save_state(&mut w);
        let bytes = w.into_vec();

        let mut board2 = GalaxianBoard::new();
        let mut r = StateReader::new(&bytes);
        board2.load_state(&mut r).unwrap();
        assert_eq!(board2.map.region_data(Region::Rom)[0], 0x00);
    }

    #[test]
    fn debug_trace_records_latch_and_port_access() {
        let mut board = GalaxianBoard::new();
        board.debug_trace.set_enabled(true);
        board.bus_write_common(0x7004, 0x01); // stars enable latch
        board.bus_read_common(0x7000); // IN2 port read

        let events = board.debug_trace.events();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, DebugEventKind::DeviceWrite);
        assert_eq!(events[0].device, Some("I/O latch"));
        assert_eq!(events[0].detail, Some("stars enable"));
        assert_eq!(events[1].kind, DebugEventKind::DeviceRead);
        assert_eq!(events[1].addr, Some(0x7000));
    }

    // -----------------------------------------------------------------------
    // GalaxianSystem wrapper
    // -----------------------------------------------------------------------

    #[test]
    fn machine_is_registered() {
        let entry = crate::registry::find("galaxian").expect("galaxian registered");
        assert_eq!(entry.name, "galaxian");
        assert_eq!(entry.rom_names, &["galaxian"]);
    }

    #[test]
    fn metadata_and_display_size() {
        use phosphor_core::core::machine::{MachineCore, Orientation, Renderable};
        let sys = GalaxianSystem::new();
        assert_eq!(sys.machine_id(), "galaxian");
        assert!((sys.frame_rate_hz() - 60.606).abs() < 0.01);
        // Native (unrotated) 256×224 framebuffer; the frontend applies ROT90.
        assert_eq!(sys.display_size(), (256, 224));
        assert_eq!(sys.orientation(), Orientation::ROT90);
        // Portrait cabinet (4:3 tube rotated to 3:4).
        assert_eq!(sys.display_aspect(), Some((3, 4)));
    }

    #[test]
    fn dip_metadata_is_well_formed_and_defaults_decompose() {
        // Disjoint masks per bank, every choice fits its mask, and the live
        // power-on bytes decompose into defined choices.
        crate::assert_dip_banks_valid(GALAXIAN_DIP_BANKS, &[0x00, 0x00, DIP2_DEFAULT]);
    }

    #[test]
    fn dip_defaults_match_historical() {
        let sys = GalaxianSystem::new();
        assert_eq!(sys.dip_bank_value(0), 0x00); // Upright
        assert_eq!(sys.dip_bank_value(1), 0x00); // 1C/1C
        assert_eq!(sys.dip_bank_value(2), 0x04); // 7000 bonus, 3 lives
        assert_eq!(sys.dip_banks().len(), 3);
        assert_eq!(sys.dip_bank_value(9), 0); // out of range
    }

    #[test]
    fn dip_set_touches_only_its_bits_not_inputs() {
        let mut sys = GalaxianSystem::new();
        // A held P2-start input lives in IN1 bit1 — must survive a coinage change.
        sys.board.handle_input(INPUT_P2_START, true);
        assert_eq!(sys.board.in1 & 0x02, 0x02);

        sys.set_dip_bank_value(1, 0xc0); // Free Play
        assert_eq!(sys.dip_bank_value(1), 0xc0);
        assert_eq!(sys.board.in1 & 0x02, 0x02, "input bit preserved");

        // Stray bits outside the mask are filtered out.
        sys.set_dip_bank_value(0, 0xff);
        assert_eq!(sys.board.in0, 0x20);
    }

    #[test]
    fn input_controls_cover_two_players() {
        let sys = GalaxianSystem::new();
        let controls = sys.input_controls();
        // Stable names are unique and include both players' fire buttons.
        let mut names: Vec<_> = controls.iter().map(|c| c.stable_name).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "stable names must be unique");
        assert!(controls.iter().any(|c| c.stable_name == "p1_fire"));
        assert!(controls.iter().any(|c| c.stable_name == "p2_fire"));
    }

    #[test]
    fn handle_input_routes_to_board_ports() {
        let mut sys = GalaxianSystem::new();
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_P1_FIRE as u16),
            pressed: true,
        });
        assert_eq!(sys.board.in0 & 0x10, 0x10); // IN0 bit4 = P1 fire
    }

    #[test]
    fn system_save_load_round_trip() {
        let mut sys = GalaxianSystem::new();
        sys.board.map.region_data_mut(Region::VideoRam)[0x40] = 0x99;
        sys.board.handle_input(INPUT_COIN, true);
        sys.set_dip_bank_value(1, 0xc0); // Free Play
        sys.board.bus_write_common(0x7800, 0x77); // sound pitch

        let data = SaveState::save_state(&sys).expect("save_state returns Some");

        let mut sys2 = GalaxianSystem::new();
        SaveState::load_state(&mut sys2, &data).unwrap();
        assert_eq!(sys2.board.map.region_data(Region::VideoRam)[0x40], 0x99);
        assert_eq!(sys2.board.in0 & 0x01, 0x01); // coin held
        assert_eq!(sys2.dip_bank_value(1), 0xc0); // Free Play
    }

    #[test]
    fn reset_preserves_dips_but_clears_ram() {
        let mut sys = GalaxianSystem::new();
        sys.set_dip_bank_value(0, 0x20); // Cocktail
        sys.board.map.region_data_mut(Region::Ram)[0] = 0xEE;
        sys.board.clock = 5000;

        sys.reset();
        assert_eq!(sys.dip_bank_value(0), 0x20, "DIP switch survives reset");
        assert_eq!(sys.board.map.region_data(Region::Ram)[0], 0x00);
        assert_eq!(sys.board.clock, 0);
    }
}
