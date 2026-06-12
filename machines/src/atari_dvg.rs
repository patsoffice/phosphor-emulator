use phosphor_core::core::machine::Renderable;
use phosphor_core::core::memory_map::MemoryMap;
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::device::dvg::{Dvg, VectorLine};
use phosphor_macros::{BusDebug, MemoryRegion};

// ---------------------------------------------------------------------------
// Memory regions (shared by Asteroids, Asteroids Deluxe, Lunar Lander)
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum Region {
    Ram = 1,
    Io = 2,
    VectorRam = 3,
    VectorRom = 4,
    ProgramRom = 5,
}

// ---------------------------------------------------------------------------
// Timing constants
// ---------------------------------------------------------------------------

// Master clock: 12.096 MHz
// CPU clock: 12.096 / 8 = 1.512 MHz
// NMI: 3 KHz / 12 ≈ 250 Hz → every ~6048 CPU cycles
// Frame: ~60 Hz → ~25200 CPU cycles
pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 1_512_000,     // 12.096 MHz / 8
    cycles_per_scanline: 25_200, // no scanline hardware; whole frame
    total_scanlines: 1,
    display_width: 1024, // vector display
    display_height: 1024,
};

pub const NMI_PERIOD_CYCLES: u64 = TIMING.cpu_clock_hz / 250;

// ---------------------------------------------------------------------------
// Atari DVG board
// ---------------------------------------------------------------------------

/// Shared hardware for Atari DVG-based arcade games (1979–1980).
///
/// Hardware: MOS 6502 @ 1.512 MHz, Atari DVG vector display.
/// Video: 1024×1024 vector display via Digital Vector Generator.
/// Used by: Asteroids, Asteroids Deluxe, Lunar Lander.
///
/// Each game provides its own memory map, I/O decode, and ROM definitions
/// via a thin wrapper struct that owns this board and implements `Bus`.
#[derive(BusDebug)]
pub struct AtariDvgBoard {
    #[debug_cpu("M6502")]
    pub(crate) cpu: M6502,
    #[debug_device("DVG")]
    pub(crate) dvg: Dvg,

    #[debug_map(cpu = 0)]
    pub(crate) map: MemoryMap,

    // NMI timing
    pub(crate) clock: u64,
    pub(crate) nmi_counter: u64,
    pub(crate) nmi_pending: bool,

    // Watchdog (resets if not written within 8 frames)
    pub(crate) watchdog_frame_count: u8,

    // Vector display
    pub(crate) display_list: Vec<VectorLine>,

    // DVG vector ROM placement in the 8 KB DVG address space.
    // Vector RAM always occupies DVG 0x0000–0x07FF.
    // Vector ROM offset and size vary per game:
    //   Asteroids:        offset 0x1000, size 0x0800 (2 KB)
    //   Asteroids Deluxe: offset 0x0800, size 0x1000 (4 KB)
    //   Lunar Lander:     offset 0x0800, size 0x1800 (6 KB)
    vrom_dvg_offset: usize,
    vrom_size: usize,
}

impl AtariDvgBoard {
    /// Create a new board with a pre-configured memory map and DVG ROM placement.
    pub fn new(map: MemoryMap, vrom_dvg_offset: usize, vrom_size: usize) -> Self {
        Self {
            cpu: M6502::new(),
            dvg: Dvg::new(),
            map,
            clock: 0,
            nmi_counter: 0,
            nmi_pending: false,
            watchdog_frame_count: 0,
            display_list: Vec::with_capacity(512),
            vrom_dvg_offset,
            vrom_size,
        }
    }

    /// Tick one cycle: NMI timing + CPU execution.
    ///
    /// The caller provides a `Bus` reference (created via `bus_split!` on the
    /// game wrapper) so the CPU's memory accesses route through game-specific
    /// I/O decode logic.
    pub fn tick(&mut self, bus: &mut dyn Bus<Address = u16, Data = u8>) {
        // NMI generation: 3 KHz / 12 ≈ 250 Hz
        self.nmi_counter += 1;
        if self.nmi_counter >= NMI_PERIOD_CYCLES {
            self.nmi_counter = 0;
            self.nmi_pending = true;
        }
        // Clear NMI pulse after 16 cycles (long enough for CPU to detect the edge).
        if self.nmi_pending && self.nmi_counter == 16 {
            self.nmi_pending = false;
        }

        // Latch watchpoint attribution context (cycle + instruction PC)
        // before CPU execution — bus dispatch cannot read CPU state mid-tick.
        if self.map.has_any_watchpoints() {
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

    /// Trigger the DVG: assemble vector memory and run to completion.
    ///
    /// The DVG has a 13-bit (8 KB) byte address space:
    ///   0x0000–0x07FF  Vector RAM (always)
    ///   0x0800–0x1FFF  Vector ROM (game-specific offset and size)
    pub fn trigger_dvg(&mut self) {
        let mut vmem = vec![0u8; 0x2000]; // 8 KB DVG address space
        vmem[0x0000..0x0800].copy_from_slice(self.map.region_data(Region::VectorRam));
        let vrom = self.map.region_data(Region::VectorRom);
        let end = self.vrom_dvg_offset + self.vrom_size;
        vmem[self.vrom_dvg_offset..end].copy_from_slice(&vrom[..self.vrom_size]);
        self.dvg.go();
        self.dvg.execute(&vmem);
        self.display_list = self.dvg.take_display_list();
    }

    /// Reset board state. CPU reset must be done separately by the wrapper
    /// via `bus_split!` (since the CPU needs a Bus reference).
    pub fn reset(&mut self) {
        self.dvg.reset();
        self.nmi_pending = false;
        self.nmi_counter = 0;
        self.watchdog_frame_count = 0;
        self.display_list.clear();
    }

    /// Render the vector display list into an RGB24 framebuffer.
    pub fn render_frame(&self, buffer: &mut [u8]) {
        rasterize_vectors(
            &self.display_list,
            buffer,
            TIMING.display_width,
            TIMING.display_height,
            true,
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

impl Saveable for AtariDvgBoard {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        self.dvg.save_state(w);
        w.write_bytes(self.map.region_data(Region::Ram));
        w.write_bytes(self.map.region_data(Region::VectorRam));
        w.write_u64_le(self.clock);
        w.write_u64_le(self.nmi_counter);
        w.write_bool(self.nmi_pending);
        w.write_u8(self.watchdog_frame_count);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        self.dvg.load_state(r)?;
        r.read_bytes_into(self.map.region_data_mut(Region::Ram))?;
        r.read_bytes_into(self.map.region_data_mut(Region::VectorRam))?;
        self.clock = r.read_u64_le()?;
        self.nmi_counter = r.read_u64_le()?;
        self.nmi_pending = r.read_bool()?;
        self.watchdog_frame_count = r.read_u8()?;
        self.display_list.clear();
        Ok(())
    }
}

impl Renderable for AtariDvgBoard {
    fn display_size(&self) -> (u32, u32) {
        TIMING.display_size()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        rasterize_vectors(
            &self.display_list,
            buffer,
            TIMING.display_width,
            TIMING.display_height,
            true,
        );
    }

    fn vector_display_list(&self) -> Option<&[VectorLine]> {
        Some(&self.display_list)
    }
}

// ---------------------------------------------------------------------------
// Vector rasterizer
// ---------------------------------------------------------------------------

/// Intensity-to-brightness lookup table (4-bit, 0 = invisible).
const INTENSITY_LUT: [u8; 16] = [
    0, 20, 40, 60, 80, 100, 120, 140, 160, 175, 190, 205, 220, 232, 244, 255,
];

/// Rasterize a display list of vector line segments into an RGB24 framebuffer.
///
/// Uses Cohen-Sutherland line clipping followed by Bresenham line drawing
/// with additive blending (saturating add) so crossing lines appear brighter.
/// Coordinates may extend beyond the display bounds; clipping trims to the
/// visible area. Vector Y=0 is at bottom; the framebuffer uses Y=0 at top.
///
/// `width` and `height` define the display dimensions (e.g. 1024×1024 for DVG,
/// 580×570 for Tempest AVG).
pub(crate) fn rasterize_vectors(
    display_list: &[VectorLine],
    buffer: &mut [u8],
    width: u32,
    height: u32,
    flip_y: bool,
) {
    buffer.fill(0);

    let w = width as i32;
    let h = height as i32;
    let x_max = w - 1;
    let y_max = h - 1;

    for line in display_list {
        if line.intensity == 0 {
            continue;
        }
        let brightness = INTENSITY_LUT[(line.intensity & 0xF) as usize];
        let br = ((brightness as u16 * line.r as u16) / 255) as u8;
        let bg = ((brightness as u16 * line.g as u16) / 255) as u8;
        let bb = ((brightness as u16 * line.b as u16) / 255) as u8;

        let (sy0, sy1) = if flip_y {
            // Normal: vector Y=0 is bottom, screen Y=0 is top.
            (y_max - line.y0, y_max - line.y1)
        } else {
            // ROT270: Y already maps to screen-Y directly.
            (line.y0, line.y1)
        };

        // Clip line to display bounds (Cohen-Sutherland).
        if let Some((cx0, cy0, cx1, cy1)) = clip_line(line.x0, sy0, line.x1, sy1, x_max, y_max) {
            draw_line(buffer, cx0, cy0, cx1, cy1, w, [br, bg, bb]);
        }
    }
}

/// Cohen-Sutherland line clipping.
/// Returns None if the line is entirely outside 0..x_max × 0..y_max.
fn clip_line(
    mut x0: i32,
    mut y0: i32,
    mut x1: i32,
    mut y1: i32,
    x_max: i32,
    y_max: i32,
) -> Option<(i32, i32, i32, i32)> {
    const INSIDE: u8 = 0;
    const LEFT: u8 = 1;
    const RIGHT: u8 = 2;
    const BOTTOM: u8 = 4;
    const TOP: u8 = 8;

    let outcode = |x: i32, y: i32| -> u8 {
        let mut code = INSIDE;
        if x < 0 {
            code |= LEFT;
        } else if x > x_max {
            code |= RIGHT;
        }
        if y < 0 {
            code |= TOP;
        } else if y > y_max {
            code |= BOTTOM;
        }
        code
    };

    let mut oc0 = outcode(x0, y0);
    let mut oc1 = outcode(x1, y1);

    loop {
        if (oc0 | oc1) == INSIDE {
            return Some((x0, y0, x1, y1));
        }
        if (oc0 & oc1) != 0 {
            return None;
        }

        let oc_out = if oc0 != INSIDE { oc0 } else { oc1 };
        let dx = x1 as i64 - x0 as i64;
        let dy = y1 as i64 - y0 as i64;

        let (nx, ny) = if oc_out & BOTTOM != 0 {
            let nx = x0 as i64 + dx * (y_max as i64 - y0 as i64) / dy;
            (nx as i32, y_max)
        } else if oc_out & TOP != 0 {
            let nx = x0 as i64 + dx * (0i64 - y0 as i64) / dy;
            (nx as i32, 0)
        } else if oc_out & RIGHT != 0 {
            let ny = y0 as i64 + dy * (x_max as i64 - x0 as i64) / dx;
            (x_max, ny as i32)
        } else {
            let ny = y0 as i64 + dy * (0i64 - x0 as i64) / dx;
            (0, ny as i32)
        };

        if oc_out == oc0 {
            x0 = nx;
            y0 = ny;
            oc0 = outcode(x0, y0);
        } else {
            x1 = nx;
            y1 = ny;
            oc1 = outcode(x1, y1);
        }
    }
}

/// Bresenham line drawing with additive blending.
/// All coordinates must be within display bounds (pre-clipped).
fn draw_line(buffer: &mut [u8], x0: i32, y0: i32, x1: i32, y1: i32, width: i32, rgb: [u8; 3]) {
    let mut x = x0;
    let mut y = y0;
    let dx = (x1 - x0).abs();
    let dy = -(y1 - y0).abs();
    let sx: i32 = if x0 < x1 { 1 } else { -1 };
    let sy: i32 = if y0 < y1 { 1 } else { -1 };
    let mut err = dx + dy;

    loop {
        let offset = ((y * width + x) as usize) * 3;
        buffer[offset] = buffer[offset].saturating_add(rgb[0]);
        buffer[offset + 1] = buffer[offset + 1].saturating_add(rgb[1]);
        buffer[offset + 2] = buffer[offset + 2].saturating_add(rgb[2]);

        if x == x1 && y == y1 {
            break;
        }

        let e2 = 2 * err;
        if e2 >= dy {
            err += dy;
            x += sx;
        }
        if e2 <= dx {
            err += dx;
            y += sy;
        }
    }
}
