use phosphor_core::core::AddressSpace16;
use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind, DebugTraceBuffer};
use phosphor_core::core::machine::Renderable;
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::watchpoint::DebugAccessSource;
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::device::dvg::{Dvg, VectorLine};
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion};

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
    display_aspect: Some((4, 3)),
};

pub const NMI_PERIOD_CYCLES: u64 = TIMING.cpu_clock_hz / 250;

// ---------------------------------------------------------------------------
// Atari DVG board
// ---------------------------------------------------------------------------

/// An Atari DVG bus: a game wrapper's view over the shared board plus its own
/// I/O (inputs, DIPs, sound, EAROM).
///
/// [`tick`] is generic over this trait, so every access resolves to a direct
/// call rather than a vtable entry.
pub trait AtariDvgBus: Bus<Address = u16, Data = u8> {
    fn board(&mut self) -> &mut AtariDvgBoard;

    /// Per-cycle game hook, run before the board's own cycle work. Asteroids
    /// Deluxe clocks its POKEY here; the others need nothing.
    #[inline]
    fn begin_cycle(&mut self) {}
}

/// One CPU cycle: NMI timing, then the 6502.
///
/// The CPU lives on the machine, beside the bus it drives, so this takes them
/// as two disjoint borrows and dispatches at a concrete type.
#[inline]
pub fn tick<B: AtariDvgBus>(cpu: &mut M6502, bus: &mut B) {
    bus.begin_cycle();
    bus.board().begin_cycle(cpu);
    cpu.execute_cycle(bus, BusMaster::Cpu(0));
    bus.board().end_cycle();
}

/// Run one frame's worth of cycles. There is no scanline structure on this
/// board -- the only periodic event is the 250 Hz NMI, counted per cycle -- so
/// this is a plain loop.
pub fn run_frame<B: AtariDvgBus>(cpu: &mut M6502, bus: &mut B) {
    for _ in 0..TIMING.cycles_per_frame() {
        tick(cpu, bus);
    }
}

/// Shared hardware for Atari DVG-based arcade games (1979–1980).
///
/// Hardware: MOS 6502 @ 1.512 MHz, Atari DVG vector display.
/// Video: 1024×1024 vector display via Digital Vector Generator.
/// Used by: Asteroids, Asteroids Deluxe, Lunar Lander.
///
/// Each game provides its own memory map, I/O decode, and ROM definitions
/// via a thin wrapper struct that owns this board and implements `Bus`.
#[derive(BusDebug, DebugTrace)]
pub struct AtariDvgBoard {
    #[debug_device("DVG")]
    pub(crate) dvg: Dvg,

    #[debug_map(cpu = 0)]
    pub(crate) map: AddressSpace16,

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

    // Debug event ring (observer state — never saved in save states)
    #[debug_events]
    pub(crate) debug_trace: DebugTraceBuffer,
}

impl AtariDvgBoard {
    /// Create a new board with a pre-configured memory map and DVG ROM placement.
    pub fn new(map: AddressSpace16, vrom_dvg_offset: usize, vrom_size: usize) -> Self {
        Self {
            dvg: Dvg::new(),
            map,
            clock: 0,
            nmi_counter: 0,
            nmi_pending: false,
            watchdog_frame_count: 0,
            display_list: Vec::with_capacity(512),
            vrom_dvg_offset,
            vrom_size,
            debug_trace: DebugTraceBuffer::new(),
        }
    }

    /// Per-cycle board work that runs before the CPU.
    fn begin_cycle(&mut self, cpu: &M6502) {
        // NMI generation: 3 KHz / 12 ≈ 250 Hz
        self.nmi_counter += 1;
        if self.nmi_counter >= NMI_PERIOD_CYCLES {
            self.nmi_counter = 0;
            self.nmi_pending = true;
            if self.debug_trace.enabled() {
                self.debug_trace.record(DebugEvent {
                    cpu_index: Some(0),
                    detail: Some("250 Hz NMI"),
                    ..DebugEvent::new(
                        self.clock,
                        DebugAccessSource::Unknown,
                        DebugEventKind::InterruptAssert,
                    )
                });
            }
        }
        // Clear NMI pulse after 16 cycles (long enough for CPU to detect the edge).
        if self.nmi_pending && self.nmi_counter == 16 {
            self.nmi_pending = false;
        }

        // Latch debug attribution context (cycle + instruction PC) before
        // CPU execution — bus dispatch cannot read CPU state mid-tick.
        // Both watchpoint hits and trace events draw PC from this latch.
        if self.map.has_any_watchpoints() || self.debug_trace.enabled() {
            let pc = cpu.at_instruction_boundary().then_some(cpu.pc as u32);
            self.map.latch_access_context(self.clock, pc);
        }
    }

    /// Board work after the CPU's cycle.
    fn end_cycle(&mut self) {
        self.clock += 1;
    }

    /// Record a main-bus write event from a game wrapper's `Bus::write`.
    /// Maps the shared DVG-platform I/O layout to event kinds; cheap no-op
    /// while tracing is disabled.
    pub(crate) fn trace_main_write(&mut self, addr: u16, data: u8) {
        if !self.debug_trace.enabled() {
            return;
        }
        let (kind, device, detail) = match addr {
            // DVG GO is recorded by trigger_dvg itself.
            0x3000 => return,
            0x3400..=0x34FF => (DebugEventKind::Watchdog, None, Some("watchdog cleared")),
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

    /// Trigger the DVG: assemble vector memory and run to completion.
    ///
    /// The DVG has a 13-bit (8 KB) byte address space:
    ///   0x0000–0x07FF  Vector RAM (always)
    ///   0x0800–0x1FFF  Vector ROM (game-specific offset and size)
    pub fn trigger_dvg(&mut self) {
        if self.debug_trace.enabled() {
            self.debug_trace.record(DebugEvent {
                cpu_index: Some(0),
                pc: self.map.latched_pc(),
                device: Some("DVG"),
                detail: Some("vector generator start"),
                ..DebugEvent::new(
                    self.clock,
                    DebugAccessSource::Cpu(0),
                    DebugEventKind::DeviceWrite,
                )
            });
        }
        let mut vmem = vec![0u8; 0x2000]; // 8 KB DVG address space
        vmem[0x0000..0x0800].copy_from_slice(self.map.region_data(Region::VectorRam));
        let vrom = self.map.region_data(Region::VectorRom);
        let end = self.vrom_dvg_offset + self.vrom_size;
        vmem[self.vrom_dvg_offset..end].copy_from_slice(&vrom[..self.vrom_size]);
        self.dvg.go();
        self.dvg.execute(&vmem);
        self.display_list = self.dvg.take_display_list();
    }

    /// Reset board state. The CPU lives on the machine, which resets it
    /// against this board.
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

    /// Whether the CPU is at an instruction boundary (for debug stepping).
    /// The CPU lives on the machine, which passes it back in.
    pub fn instruction_boundaries(cpu: &M6502) -> u32 {
        u32::from(cpu.at_instruction_boundary())
    }
}

impl Saveable for AtariDvgBoard {
    fn save_state(&self, w: &mut StateWriter) {
        // The CPU is saved by the machine, which owns it.
        self.dvg.save_state(w);
        w.write_bytes(self.map.region_data(Region::Ram));
        w.write_bytes(self.map.region_data(Region::VectorRam));
        w.write_u64_le(self.clock);
        w.write_u64_le(self.nmi_counter);
        w.write_bool(self.nmi_pending);
        w.write_u8(self.watchdog_frame_count);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        // The CPU is loaded by the machine, which owns it.
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

    fn display_aspect(&self) -> Option<(u32, u32)> {
        TIMING.display_aspect()
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

/// Focused beam spot diameter as a fraction of the tube's long axis.
///
/// The Atari colour XY monitors are 19 inch shadow-mask tubes, the same family
/// as the raster monitors of the era, and two things bound the spot: the mask
/// pitch, about 0.6 mm, below which nothing is resolvable, and the focused spot
/// itself at about 0.7 mm. The long axis of a 19 inch 4:3 viewable area is about
/// 360 mm. So the spot is 0.7/360 of the screen whatever coordinate space a
/// particular generator uses, which works out at about 1.1 units on Tempest's
/// 580, 1.8 on Quantum's 900, and 2.0 on the DVG's 1024.
const BEAM_SPOT_FRACTION: f32 = 0.7 / 360.0;

/// A Gaussian's standard deviation for a given full width at half maximum:
/// `FWHM = 2*sqrt(2*ln 2)*sigma`.
const FWHM_TO_SIGMA: f32 = 1.0 / 2.354_82;

/// Floor on the spot's sigma, in output pixels.
///
/// Not a taste value: a Gaussian sampled on a unit grid has a residual ripple of
/// about `2*exp(-2*pi^2*sigma^2)` depending on where its centre falls between
/// samples, which is the spot aliasing against the grid. That ripple is a
/// brightness that varies with the angle of the line, the very defect this
/// rasterizer exists to fix, so sigma has to stay where the ripple is
/// negligible: 0.4 gives 8%, 0.5 gives 1.5%, 0.6 gives 0.2%.
///
/// Tempest's physical spot works out slightly under this, so it renders a touch
/// wider than the tube would. That is a limit of rasterizing at display-list
/// resolution, not of the tube; the GL path draws at window resolution and can
/// use the true figure.
const MIN_SIGMA_PIXELS: f32 = 0.6;

std::thread_local! {
    /// Per-frame energy accumulator, kept across frames. See `rasterize_vectors`.
    static ACCUMULATOR: std::cell::RefCell<Vec<f32>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Rasterize a display list of vector line segments into an RGB24 framebuffer.
///
/// The beam is a round spot with a Gaussian profile, swept along each segment,
/// so a line has real width and soft edges rather than being one hard pixel.
/// Energy is deposited per unit *length*: a segment of length L emits L times
/// its per-unit energy however it is angled, spread across the spot's profile.
/// That is what the hardware does, and it is also what Bresenham could not do,
/// since it lights one pixel per unit of x and so drew a 45 degree line at
/// about 1/sqrt(2) of the brightness per unit length of a horizontal one.
///
/// Segments are clipped in parameter space to the visible area plus the spot's
/// reach, so a vector running far off screen costs only the part that shows.
/// Vector Y=0 is at bottom; the framebuffer uses Y=0 at top.
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
    let y_max = h - 1;

    let sigma = (w.max(h) as f32 * BEAM_SPOT_FRACTION * FWHM_TO_SIGMA).max(MIN_SIGMA_PIXELS);
    // Where the profile is cut off. Truncating leaves a step the height of the
    // profile there, so it has to fall below one level of an 8-bit channel:
    // 3 sigma is 1.1% of the peak and would show as a faint edge, 3.5 is 0.2%
    // and rounds away.
    let radius = (3.5 * sigma).ceil() as i32;

    // The monitor's brightness control, set where an operator would set it: a
    // full-intensity vector reaches full white along its centre and no further.
    //
    // Spreading a fixed energy across a wider spot lowers its peak, so without
    // this a machine with a coarser coordinate space looks dimmer for no reason
    // that is about the hardware. The peak of a unit-area Gaussian is
    // 1/(sigma*sqrt(2*pi)), so its reciprocal is the gain that puts the centre
    // of a line back at full scale, whatever the spot works out to.
    let gain = sigma * (std::f32::consts::TAU).sqrt();

    // Energy accumulates in float and is quantised once at the end. Summing
    // 8-bit steps per segment would lose every contribution under half a step,
    // which is most of the profile's skirt and all of a dim vector.
    //
    // The buffer is kept between frames rather than allocated per frame:
    // `render_frame` takes `&self` so there is nowhere on the board to put it,
    // and a screen's worth of fresh pages costs more in faults than the
    // rasterizing does.
    ACCUMULATOR.with_borrow_mut(|acc| {
        let n = (w * h * 3) as usize;
        if acc.len() < n {
            acc.resize(n, 0.0);
        }
        acc[..n].fill(0.0);
        let acc = &mut acc[..n];

        // The profile, sampled once over the squared distances the sweep can
        // produce, so the inner loop indexes a table instead of calling exp(). The
        // table is fine enough that the step between neighbouring entries is under
        // one level of an 8-bit channel.
        let profile = beam_profile(sigma, radius);

        // Rows that were actually touched, so a sparse frame does not pay to
        // convert a screenful of untouched black at the end.
        let (mut dirty_lo, mut dirty_hi) = (h, -1i32);

        for line in display_list {
            if line.intensity == 0 {
                continue;
            }
            let brightness = INTENSITY_LUT[(line.intensity & 0xF) as usize] as f32 / 255.0 * gain;
            let energy = [
                brightness * line.r as f32,
                brightness * line.g as f32,
                brightness * line.b as f32,
            ];

            let (sy0, sy1) = if flip_y {
                // Normal: vector Y=0 is bottom, screen Y=0 is top.
                (y_max - line.y0, y_max - line.y1)
            } else {
                // ROT270: Y already maps to screen-Y directly.
                (line.y0, line.y1)
            };

            let (lo, hi) = sweep_beam(
                acc,
                w,
                h,
                (line.x0 as f32, sy0 as f32),
                (line.x1 as f32, sy1 as f32),
                radius,
                &profile,
                energy,
            );
            dirty_lo = dirty_lo.min(lo);
            dirty_hi = dirty_hi.max(hi);
        }

        if dirty_hi >= dirty_lo {
            let from = (dirty_lo * w * 3) as usize;
            let to = ((dirty_hi + 1) * w * 3) as usize;
            for (out, e) in buffer[from..to].iter_mut().zip(acc[from..to].iter()) {
                *out = e.clamp(0.0, 255.0) as u8;
            }
        }
    });
}

/// The beam profile sampled against squared distance, for the sweep's inner
/// loop to index rather than evaluating an exponential per pixel.
///
/// Entry `i` is the profile at `d^2 = i * radius^2 / (LEN - 1)`, and the
/// profile has unit area across the line so that sweeping it deposits one unit
/// of energy per unit of length.
fn beam_profile(sigma: f32, radius: i32) -> Vec<f32> {
    /// Long enough that the step between neighbouring entries stays under one
    /// level of an 8-bit channel: the profile falls fastest at the centre, at
    /// `1/(2*sigma^2)` per unit of squared distance.
    const LEN: usize = 2048;

    let norm = 1.0 / (sigma * std::f32::consts::TAU.sqrt());
    let inv_two_sigma_sq = 1.0 / (2.0 * sigma * sigma);
    let r2 = (radius * radius) as f32;

    (0..LEN)
        .map(|i| {
            let d2 = r2 * i as f32 / (LEN - 1) as f32;
            norm * (-d2 * inv_two_sigma_sq).exp()
        })
        .collect()
}

/// Sweep the beam spot from `p0` to `p1`, depositing `energy` per unit length.
///
/// Every pixel within the spot's reach of the segment gets the beam profile
/// evaluated at its distance from the segment. Summed over the pixel grid, a
/// profile of unit area deposits one unit of energy per unit of length, which
/// is what makes the result independent of the angle of the line: the grid has
/// unit density whichever way the segment runs, so the sum tracks the area
/// integral and the area integral is just the length.
///
/// Distance is measured to the segment rather than to the infinite line, so the
/// ends are round, which is what a round spot arriving and leaving looks like.
///
/// The work is proportional to the length times the spot's width, and the rows
/// are windowed to the stadium around the segment, so a long diagonal costs its
/// own length rather than the area of its bounding box.
/// Returns the range of rows it touched, so the caller can convert only those.
#[allow(clippy::too_many_arguments)]
fn sweep_beam(
    acc: &mut [f32],
    w: i32,
    h: i32,
    p0: (f32, f32),
    p1: (f32, f32),
    radius: i32,
    profile: &[f32],
    energy: [f32; 3],
) -> (i32, i32) {
    let (dx, dy) = (p1.0 - p0.0, p1.1 - p0.1);
    let len_sq = dx * dx + dy * dy;
    let len = len_sq.sqrt();
    let r = radius as f32;

    let y_lo = (p0.1.min(p1.1) - r).floor().max(0.0) as i32;
    let y_hi = (p0.1.max(p1.1) + r).ceil().min((h - 1) as f32) as i32;
    if y_lo > y_hi {
        return (h, -1);
    }
    let seg_x_lo = p0.0.min(p1.0) - r;
    let seg_x_hi = p0.0.max(p1.0) + r;

    let r2 = r * r;
    let lut_scale = (profile.len() - 1) as f32 / r2;

    for py in y_lo..=y_hi {
        let y = py as f32;

        // Where this row crosses the stadium: the slab around the infinite
        // line, widened by whichever end caps reach this far.
        let (mut x_min, mut x_max);
        if dy.abs() > 1e-6 {
            // |(-dy)*x + dx*y + c| <= r*len is the slab of half-width r.
            let (a, b) = (-dy, dx);
            let c = -(a * p0.0 + b * p0.1);
            let rhs = r * len;
            let lo = (-rhs - b * y - c) / a;
            let hi = (rhs - b * y - c) / a;
            x_min = lo.min(hi);
            x_max = lo.max(hi);
        } else {
            x_min = seg_x_lo;
            x_max = seg_x_hi;
        }
        for (cx, cy) in [p0, p1] {
            let ddy = (y - cy).abs();
            if ddy <= r {
                let half = (r * r - ddy * ddy).sqrt();
                x_min = x_min.min(cx - half);
                x_max = x_max.max(cx + half);
            }
        }

        // Clamp to the segment's own reach and to the screen, so the part of a
        // vector that runs off the display costs nothing.
        let x_from = x_min.max(seg_x_lo).max(0.0);
        let x_to = x_max.min(seg_x_hi).min((w - 1) as f32);
        if x_from > x_to {
            continue;
        }

        let row = (py * w) as usize;
        for px in (x_from.floor() as i32)..=(x_to.ceil() as i32) {
            if px < 0 || px >= w {
                continue;
            }
            let x = px as f32;

            // Distance to the segment: project onto it, clamped to its ends.
            let t = if len_sq > 0.0 {
                (((x - p0.0) * dx + (y - p0.1) * dy) / len_sq).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let ex = x - (p0.0 + dx * t);
            let ey = y - (p0.1 + dy * t);
            let d2 = ex * ex + ey * ey;
            if d2 > r2 {
                continue;
            }

            let weight = profile[(d2 * lut_scale) as usize];
            let o = (row + px as usize) * 3;
            acc[o] += energy[0] * weight;
            acc[o + 1] += energy[1] * weight;
            acc[o + 2] += energy[2] * weight;
        }
    }

    (y_lo, y_hi)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::AccessKind;

    /// The beam rasterizer, on its own, without a machine around it.
    mod beam {
        use super::*;

        const W: u32 = 256;
        const H: u32 = 256;

        fn white(x0: i32, y0: i32, x1: i32, y1: i32) -> VectorLine {
            VectorLine {
                x0,
                y0,
                x1,
                y1,
                intensity: 15,
                r: 255,
                g: 255,
                b: 255,
            }
        }

        fn render(lines: &[VectorLine]) -> Vec<u8> {
            let mut buf = vec![0u8; (W * H * 3) as usize];
            rasterize_vectors(lines, &mut buf, W, H, false);
            buf
        }

        /// Total light emitted, summing one channel over the whole frame.
        fn total_light(buf: &[u8]) -> u64 {
            buf.iter().step_by(3).map(|&v| v as u64).sum()
        }

        fn peak(buf: &[u8]) -> u8 {
            buf.iter().step_by(3).copied().max().unwrap_or(0)
        }

        #[test]
        fn a_full_intensity_vector_reaches_full_white_at_its_centre() {
            // The monitor's brightness control is set here: full intensity puts
            // the centre of a line at full scale. Without that, spreading the
            // beam's energy over a wider spot would make every machine dimmer
            // in proportion to how coarse its coordinate space happens to be.
            let buf = render(&[white(40, 128, 200, 128)]);
            assert!(
                peak(&buf) >= 250,
                "a full-intensity line should peak at full white, got {}",
                peak(&buf)
            );
        }

        #[test]
        fn brightness_per_unit_length_does_not_depend_on_angle() {
            // The defect Bresenham had: it lights one pixel per unit of x, so a
            // 45 degree line got about 1/sqrt(2) of the light per unit length of
            // a horizontal one. Sweeping a spot deposits per unit *length*, so
            // equal-length segments emit equal light whatever their angle.
            let len = 100.0f32;
            let horizontal = total_light(&render(&[white(70, 128, 70 + len as i32, 128)]));

            let d = (len / std::f32::consts::SQRT_2).round() as i32;
            let diagonal = total_light(&render(&[white(70, 80, 70 + d, 80 + d)]));

            let ratio = diagonal as f64 / horizontal as f64;
            assert!(
                (0.95..=1.05).contains(&ratio),
                "a diagonal of the same length emitted {ratio:.3} of the light of a horizontal one"
            );
        }

        #[test]
        fn splitting_a_vector_in_two_conserves_the_light_it_emits() {
            // Energy is deposited per unit length, so where the display list
            // happens to put its vertices cannot change how much light comes
            // out. The two collinear halves share an endpoint, where the beam
            // is stamped twice, so allow that one spot's worth of overlap.
            let whole = total_light(&render(&[white(60, 128, 180, 128)]));
            let halves = total_light(&render(&[
                white(60, 128, 120, 128),
                white(120, 128, 180, 128),
            ]));

            let ratio = halves as f64 / whole as f64;
            assert!(
                (0.98..=1.06).contains(&ratio),
                "splitting the vector changed emitted light by a factor of {ratio:.3}"
            );
        }

        #[test]
        fn a_vector_running_off_screen_costs_only_the_part_that_shows() {
            // The beam really does run off the screen (see the AVG's unclamped
            // position), so the rasterizer has to clip rather than trust the
            // coordinates, and must not light anything outside.
            let buf = render(&[white(-100_000, 128, 100_000, 128)]);
            assert!(peak(&buf) >= 250, "the visible part is still drawn");

            // Nothing outside the row the line runs along, give or take the spot.
            let mut stray = 0;
            for y in 0..H as usize {
                for x in 0..W as usize {
                    if buf[(y * W as usize + x) * 3] > 0 && y.abs_diff(128) > 4 {
                        stray += 1;
                    }
                }
            }
            assert_eq!(stray, 0, "light landed away from the vector");
        }
    }

    mod debug_events {
        use super::*;

        /// Minimal Asteroids-like map: RAM, I/O (covers the watchdog at
        /// 0x3400), and the two vector regions trigger_dvg requires.
        fn board() -> AtariDvgBoard {
            let mut map = AddressSpace16::new();
            map.region(Region::Ram, "RAM", 0x0000, 0x0400, AccessKind::ReadWrite)
                .region(Region::Io, "I/O", 0x3000, 0x1000, AccessKind::Io)
                .region(
                    Region::VectorRam,
                    "Vector RAM",
                    0x4000,
                    0x0800,
                    AccessKind::ReadWrite,
                )
                .region(
                    Region::VectorRom,
                    "Vector ROM",
                    0x4800,
                    0x0800,
                    AccessKind::ReadOnly,
                );
            AtariDvgBoard::new(map, 0x0800, 0x0800)
        }

        #[test]
        fn tracing_disabled_records_nothing() {
            let mut b = board();
            b.trace_main_write(0x3400, 0x00);
            b.trigger_dvg();
            assert!(b.debug_trace.is_empty());
        }

        #[test]
        fn trigger_dvg_records_vector_generator_start() {
            let mut b = board();
            b.debug_trace.set_enabled(true);
            b.clock = 321;

            b.trigger_dvg();

            let events = b.debug_trace.events();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].kind, DebugEventKind::DeviceWrite);
            assert_eq!(events[0].device, Some("DVG"));
            assert_eq!(events[0].detail, Some("vector generator start"));
            assert_eq!(events[0].cycle, 321);
        }

        #[test]
        fn write_kinds_map_by_address_and_region() {
            let mut b = board();
            b.debug_trace.set_enabled(true);

            b.trace_main_write(0x3400, 0x00); // watchdog
            b.trace_main_write(0x3200, 0x01); // other I/O
            b.trace_main_write(0x0100, 0x42); // RAM
            b.trace_main_write(0x3000, 0xFF); // DVG GO: recorded by trigger only

            let events = b.debug_trace.events();
            assert_eq!(events.len(), 3);
            assert_eq!(events[0].kind, DebugEventKind::Watchdog);
            assert_eq!(events[0].detail, Some("watchdog cleared"));
            assert_eq!(events[1].kind, DebugEventKind::IoWrite);
            assert_eq!(events[2].kind, DebugEventKind::MemoryWrite);
            assert_eq!(events[2].region, Some("RAM"));
        }
    }
}
