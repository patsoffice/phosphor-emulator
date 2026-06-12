use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind, DebugTraceBuffer};
use phosphor_core::core::machine::Renderable;
use phosphor_core::core::memory_map::MemoryMap;
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::watchpoint::DebugAccessSource;
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::device::avg::Avg;
use phosphor_core::device::dvg::VectorLine;
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion};

use crate::atari_dvg::rasterize_vectors;

// ---------------------------------------------------------------------------
// Memory regions (shared by AVG-based games: Tempest, etc.)
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum Region {
    Ram = 1,
    ColorRam = 2,
    Io = 3,
    VectorRam = 4,
    VectorRom = 5,
    ProgramRom = 6,
}

// ---------------------------------------------------------------------------
// Timing constants
// ---------------------------------------------------------------------------

// Master clock: 12.096 MHz
// CPU clock: 12.096 / 8 = 1.512 MHz
// 3 KHz clock: 12.096 MHz / 4096 = 2953.125 Hz
// IRQ: 3 KHz / 12 = 246.09375 Hz → every 6144 CPU cycles
// Frame: ~60 Hz → ~25200 CPU cycles
pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 1_512_000,     // 12.096 MHz / 8
    cycles_per_scanline: 25_200, // no scanline hardware; whole frame
    total_scanlines: 1,
    display_width: 580,
    display_height: 570,
};

/// IRQ period: master_clock / 4096 / 12 = 246.09375 Hz → 6144 CPU cycles.
pub const IRQ_PERIOD_CYCLES: u64 = 6144;

// ---------------------------------------------------------------------------
// Atari AVG board
// ---------------------------------------------------------------------------

/// Shared hardware for Atari AVG-based arcade games (1980–1983).
///
/// Hardware: MOS 6502 @ 1.512 MHz, Atari AVG color vector display.
/// Video: 1024×1024 color vector display via Analog Vector Generator.
/// Used by: Tempest (initially; potentially Major Havoc, Star Wars later).
///
/// Each game provides its own memory map, I/O decode, and ROM definitions
/// via a thin wrapper struct that owns this board and implements `Bus`.
#[derive(BusDebug, DebugTrace)]
pub struct AtariAvgBoard {
    #[debug_cpu("M6502")]
    pub(crate) cpu: M6502,
    #[debug_device("AVG")]
    pub(crate) avg: Avg,

    #[debug_map(cpu = 0)]
    pub(crate) map: MemoryMap,

    // IRQ timing (250 Hz periodic)
    pub(crate) clock: u64,
    pub(crate) irq_counter: u64,
    pub(crate) irq_pending: bool,

    // Watchdog (resets if not written within 8 frames)
    pub(crate) watchdog_frame_count: u8,

    // Vector display (unrotated AVG coordinates)
    pub(crate) display_list: Vec<VectorLine>,

    // Debug event ring (observer state — never saved in save states)
    #[debug_events]
    pub(crate) debug_trace: DebugTraceBuffer,
}

impl AtariAvgBoard {
    /// Create a new board with a pre-configured memory map and visible area dimensions.
    ///
    /// `visible_width`/`visible_height` define the AVG beam center (half of each).
    /// For Tempest: 580×570.
    pub fn new(map: MemoryMap, visible_width: i32, visible_height: i32) -> Self {
        Self {
            cpu: M6502::new(),
            avg: Avg::new(visible_width, visible_height),
            map,
            clock: 0,
            irq_counter: 0,
            irq_pending: false,
            watchdog_frame_count: 0,
            display_list: Vec::with_capacity(2048),
            debug_trace: DebugTraceBuffer::new(),
        }
    }

    /// Tick one cycle: IRQ timing + CPU execution.
    pub fn tick(&mut self, bus: &mut dyn Bus<Address = u16, Data = u8>) {
        // IRQ generation: 250 Hz periodic
        self.irq_counter += 1;
        if self.irq_counter >= IRQ_PERIOD_CYCLES {
            self.irq_counter = 0;
            self.irq_pending = true;
            if self.debug_trace.enabled() {
                self.debug_trace.record(DebugEvent {
                    cpu_index: Some(0),
                    detail: Some("250 Hz IRQ"),
                    ..DebugEvent::new(
                        self.clock,
                        DebugAccessSource::Unknown,
                        DebugEventKind::InterruptAssert,
                    )
                });
            }
        }

        // Latch debug attribution context (cycle + instruction PC) before
        // CPU execution — bus dispatch cannot read CPU state mid-tick.
        // Both watchpoint hits and trace events draw PC from this latch.
        if self.map.has_any_watchpoints() || self.debug_trace.enabled() {
            let pc = self
                .cpu
                .at_instruction_boundary()
                .then_some(self.cpu.pc as u32);
            self.map.latch_access_context(self.clock, pc);
        }

        // CPU tick
        self.cpu.execute_cycle(bus, BusMaster::Cpu(0));
        self.clock += 1;
    }

    /// Record a main-bus write event from a game wrapper's `Bus::write`.
    /// Maps the shared AVG-platform I/O layout to event kinds; cheap no-op
    /// while tracing is disabled.
    pub(crate) fn trace_main_write(&mut self, addr: u16, data: u8) {
        if !self.debug_trace.enabled() {
            return;
        }
        let (kind, device, detail) = match addr {
            // AVG GO is recorded by trigger_avg itself.
            0x4800 => return,
            0x5000 => (
                DebugEventKind::Watchdog,
                None,
                Some("watchdog cleared + IRQ ack"),
            ),
            0x5800 => (
                DebugEventKind::DeviceWrite,
                Some("AVG"),
                Some("vector generator reset"),
            ),
            _ => match self.map.page(addr).region_id {
                Region::IO => (DebugEventKind::IoWrite, None, None),
                _ => (DebugEventKind::MemoryWrite, None, None),
            },
        };
        self.debug_trace.record(DebugEvent {
            cpu_index: Some(0),
            pc: self.map.latched_pc(),
            addr: Some(addr as u32),
            value: Some(data as u32),
            width: 1,
            region: self.map.region_at(addr).map(|r| r.name),
            device,
            detail,
            ..DebugEvent::new(self.clock, DebugAccessSource::Cpu(0), kind)
        });
    }

    /// Trigger the AVG: assemble vector memory and run to completion.
    ///
    /// The AVG has a 13-bit (8 KB) byte address space:
    ///   0x0000–0x0FFF  Vector RAM (4 KB)
    ///   0x1000–0x1FFF  Vector ROM (4 KB)
    pub fn trigger_avg(&mut self) {
        if self.debug_trace.enabled() {
            self.debug_trace.record(DebugEvent {
                cpu_index: Some(0),
                pc: self.map.latched_pc(),
                device: Some("AVG"),
                detail: Some("vector generator start"),
                ..DebugEvent::new(
                    self.clock,
                    DebugAccessSource::Cpu(0),
                    DebugEventKind::DeviceWrite,
                )
            });
        }
        let mut vmem = vec![0u8; 0x2000]; // 8 KB AVG address space
        vmem[0x0000..0x1000].copy_from_slice(self.map.region_data(Region::VectorRam));
        let vrom = self.map.region_data(Region::VectorRom);
        let vrom_len = vrom.len().min(0x1000);
        vmem[0x1000..0x1000 + vrom_len].copy_from_slice(&vrom[..vrom_len]);

        // Get color RAM for Tempest color lookup
        let color_ram_data = self.map.region_data(Region::ColorRam);
        let mut color_ram = [0u8; 16];
        let len = color_ram_data.len().min(16);
        color_ram[..len].copy_from_slice(&color_ram_data[..len]);

        self.avg.go();
        self.avg.execute(&vmem, &color_ram);
        self.display_list = self.avg.take_display_list();
    }

    /// Reset board state. CPU reset must be done separately by the wrapper.
    pub fn reset(&mut self) {
        self.avg.reset();
        self.irq_pending = false;
        self.irq_counter = 0;
        self.watchdog_frame_count = 0;
        self.display_list.clear();
    }

    pub fn render_frame(&self, buffer: &mut [u8]) {
        rasterize_vectors(
            &self.display_list,
            buffer,
            TIMING.display_width,
            TIMING.display_height,
            false,
        );
    }

    /// Check if the CPU is at an instruction boundary (for debug stepping).
    pub fn debug_tick_boundaries(&self) -> u32 {
        if self.cpu.at_instruction_boundary() {
            1
        } else {
            0
        }
    }
}

impl Saveable for AtariAvgBoard {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        self.avg.save_state(w);
        w.write_bytes(self.map.region_data(Region::Ram));
        w.write_bytes(self.map.region_data(Region::ColorRam));
        w.write_bytes(self.map.region_data(Region::VectorRam));
        w.write_u64_le(self.clock);
        w.write_u64_le(self.irq_counter);
        w.write_bool(self.irq_pending);
        w.write_u8(self.watchdog_frame_count);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        self.avg.load_state(r)?;
        r.read_bytes_into(self.map.region_data_mut(Region::Ram))?;
        r.read_bytes_into(self.map.region_data_mut(Region::ColorRam))?;
        r.read_bytes_into(self.map.region_data_mut(Region::VectorRam))?;
        self.clock = r.read_u64_le()?;
        self.irq_counter = r.read_u64_le()?;
        self.irq_pending = r.read_bool()?;
        self.watchdog_frame_count = r.read_u8()?;
        self.display_list.clear();
        Ok(())
    }
}

impl Renderable for AtariAvgBoard {
    fn display_size(&self) -> (u32, u32) {
        TIMING.display_size()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        self.render_frame(buffer);
    }

    fn vector_display_list(&self) -> Option<&[VectorLine]> {
        Some(&self.display_list)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::memory_map::AccessKind;

    mod debug_events {
        use super::*;

        /// Minimal Tempest-like map: RAM, color RAM, I/O (covers the
        /// watchdog at 0x5000 and AVG reset at 0x5800), and the vector
        /// regions trigger_avg requires.
        fn board() -> AtariAvgBoard {
            let mut map = MemoryMap::new();
            map.region(Region::Ram, "RAM", 0x0000, 0x0800, AccessKind::ReadWrite)
                .region(
                    Region::ColorRam,
                    "Color RAM",
                    0x0800,
                    0x0100,
                    AccessKind::ReadWrite,
                )
                .region(
                    Region::VectorRam,
                    "Vector RAM",
                    0x2000,
                    0x1000,
                    AccessKind::ReadWrite,
                )
                .region(
                    Region::VectorRom,
                    "Vector ROM",
                    0x3000,
                    0x1000,
                    AccessKind::ReadOnly,
                )
                .region(Region::Io, "I/O", 0x4800, 0x1800, AccessKind::Io);
            AtariAvgBoard::new(map, 580, 570)
        }

        #[test]
        fn tracing_disabled_records_nothing() {
            let mut b = board();
            b.trace_main_write(0x5000, 0x00);
            b.trigger_avg();
            assert!(b.debug_trace.is_empty());
        }

        #[test]
        fn trigger_avg_records_vector_generator_start() {
            let mut b = board();
            b.debug_trace.set_enabled(true);

            b.trigger_avg();

            let events = b.debug_trace.events();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].kind, DebugEventKind::DeviceWrite);
            assert_eq!(events[0].device, Some("AVG"));
            assert_eq!(events[0].detail, Some("vector generator start"));
        }

        #[test]
        fn write_kinds_map_by_address_and_region() {
            let mut b = board();
            b.debug_trace.set_enabled(true);

            b.trace_main_write(0x5000, 0x00); // watchdog + IRQ ack
            b.trace_main_write(0x5800, 0x00); // AVG reset
            b.trace_main_write(0x0100, 0x42); // RAM
            b.trace_main_write(0x4800, 0xFF); // AVG GO: recorded by trigger only

            let events = b.debug_trace.events();
            assert_eq!(events.len(), 3);
            assert_eq!(events[0].kind, DebugEventKind::Watchdog);
            assert_eq!(events[0].detail, Some("watchdog cleared + IRQ ack"));
            assert_eq!(events[1].kind, DebugEventKind::DeviceWrite);
            assert_eq!(events[1].device, Some("AVG"));
            assert_eq!(events[1].detail, Some("vector generator reset"));
            assert_eq!(events[2].kind, DebugEventKind::MemoryWrite);
        }
    }
}
