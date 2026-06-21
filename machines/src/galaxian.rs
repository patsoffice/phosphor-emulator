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
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::watchpoint::DebugAccessSource;
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::CpuStateTrait;
use phosphor_core::cpu::state::Z80State;
use phosphor_core::cpu::z80::Z80;
use phosphor_core::device::GalaxianSound;
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion};

use crate::galaxian_video::{self, GalaxianVideo};

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
    display_width: 224, // rotated 90° CCW from native 256×224
    display_height: 256,
};

/// Visible scanlines per frame (native rows rendered into the framebuffer).
pub const VISIBLE_LINES: u64 = galaxian_video::NATIVE_HEIGHT as u64;

// ---------------------------------------------------------------------------
// GalaxianBoard
// ---------------------------------------------------------------------------

/// Galaxian hardware base (Z80 @ 3.072 MHz, tilemap + sprites + starfield +
/// discrete sound).
///
/// Game wrappers compose this struct and implement [`Bus`] to route memory
/// accesses.
#[derive(BusDebug, DebugTrace)]
pub struct GalaxianBoard {
    #[debug_cpu("Z80")]
    pub(crate) cpu: Z80,

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
            cpu: Z80::new(),
            map: Self::build_map(),
            video: GalaxianVideo::new(),
            sound: GalaxianSound::new(SAMPLE_RATE),
            in0: 0x00,
            in1: 0x00,
            in2: 0x00,
            irq_enabled: false,
            vblank_nmi_pending: false,
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

    pub fn tick(&mut self, bus: &mut dyn Bus<Address = u16, Data = u8>) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();

        // Advance the starfield scroll once at the top of each frame.
        if frame_cycle == 0 {
            self.video.begin_frame();
        }

        // Per-scanline rendering at each scanline boundary, before the CPU runs.
        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let scanline = (frame_cycle / TIMING.cycles_per_scanline) as usize;
            if (scanline as u64) < VISIBLE_LINES {
                let vram = self.map.region_data(Region::VideoRam);
                let objram = self.map.region_data(Region::ObjRam);
                self.video.render_scanline(scanline, vram, objram);
            }
        }

        // VBLANK NMI: assert at the start of VBLANK (first non-visible line).
        let vblank_cycle = VISIBLE_LINES * TIMING.cycles_per_scanline;
        if frame_cycle == vblank_cycle {
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
        if frame_cycle == 0 && self.clock > 0 {
            self.vblank_nmi_pending = false;
        }

        // Latch debug attribution context before CPU execution.
        if self.map.has_any_watchpoints() || self.debug_trace.enabled() {
            let pc = self
                .cpu
                .at_instruction_boundary()
                .then_some(self.cpu.pc as u32);
            self.map.latch_access_context(self.clock, pc);
        }

        self.cpu.execute_cycle(bus, BusMaster::Cpu(0));
        self.sound.tick(1);

        self.clock += 1;
        self.watchdog_counter += 1;
    }

    /// Main CPU: VBLANK NMI, edge-triggered and gated by the IRQ-enable latch.
    pub fn check_interrupts(&self, target: BusMaster) -> InterruptState {
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
        let data = match addr {
            0x0000..=0x5fff => self.map.read_backing(addr),
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
            // Work RAM, Video RAM, Object RAM
            0x4000..=0x5fff => self.map.write_backing(addr, data),

            // 0x6000 block: lines 0-3 are lamps / coin counter / coin lockout
            // (not modeled); lines 4-7 are the sound LFO ("wolf-whistle") DAC.
            0x6000..=0x67ff => {
                let line = (addr & 7) as u8;
                if line >= 4 {
                    self.sound.lfo_freq_w(line - 4, data);
                }
            }

            // 0x6800 block: the sound 74LS259 latch (FS1-3 / HIT / FIRE / VOL).
            0x6800..=0x6fff => self.sound.sound_w((addr & 7) as u8, data),

            // 0x7000 block: 74LS259 addressable latch (line = addr & 7).
            0x7000..=0x77ff => match addr & 7 {
                1 => {
                    self.irq_enabled = data & 1 != 0;
                    if !self.irq_enabled {
                        self.vblank_nmi_pending = false;
                    }
                }
                4 => self.video.set_stars_enabled(data & 1 != 0),
                6 => self.video.set_flip_x(data & 1 != 0),
                7 => self.video.set_flip_y(data & 1 != 0),
                _ => {}
            },

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

    // -----------------------------------------------------------------------
    // CPU state / video output
    // -----------------------------------------------------------------------

    pub fn get_cpu_state(&self) -> Z80State {
        self.cpu.snapshot()
    }

    pub fn clock(&self) -> u64 {
        self.clock
    }

    pub fn render_frame(&self, buffer: &mut [u8]) {
        self.video.render_frame(buffer);
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

    /// Reset all board state except ROMs, GFX caches, and palette. The caller
    /// resets the CPU separately (requires `bus_split`).
    pub fn reset_board(&mut self) {
        self.video.reset();
        phosphor_core::device::Device::reset(&mut self.sound);
        self.in0 = 0x00;
        self.in1 = 0x00;
        self.in2 = 0x00;
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

    pub fn debug_tick_boundaries(&self) -> u32 {
        if self.cpu.at_instruction_boundary() {
            1
        } else {
            0
        }
    }
}

impl Saveable for GalaxianBoard {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
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
        self.cpu.load_state(r)?;
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
        let cpu_snap = board.cpu.snapshot();

        let mut board2 = GalaxianBoard::new();
        board2.map.region_data_mut(Region::VideoRam)[0x100] = 0xFF;
        board2.clock = 7;
        let mut r = StateReader::new(&bytes);
        board2.load_state(&mut r).unwrap();

        assert_eq!(board2.cpu.snapshot(), cpu_snap);
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
}
