use phosphor_core::core::AddressSpace16;
use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind, DebugTraceBuffer};
use phosphor_core::core::machine::Renderable;
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::watchpoint::DebugAccessSource;
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::device::dvg::{
    BEAM_CUTOFF_SIGMAS, Dvg, HALATION_OFF, MIN_CYCLES_PER_UNIT, MIN_SIGMA_PIXELS, VectorLine,
    beam_sigma_units, halation_sigma_units, raster_size_for_field,
};
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
        let field = TIMING.display_size();
        let (rw, rh) = raster_size_for_field(field.0, field.1);
        rasterize_vectors(
            &self.display_list,
            buffer,
            rw,
            rh,
            field,
            true,
            HALATION_OFF,
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
        // The timing's dimensions are the display list's coordinate extent; how
        // many pixels to draw it into comes from the tube. See
        // `Renderable::vector_field_size`.
        let (w, h) = TIMING.display_size();
        phosphor_core::device::dvg::raster_size_for_field(w, h)
    }

    fn vector_field_size(&self) -> Option<(u32, u32)> {
        Some(TIMING.display_size())
    }

    fn display_aspect(&self) -> Option<(u32, u32)> {
        TIMING.display_aspect()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        let field = TIMING.display_size();
        let (rw, rh) = raster_size_for_field(field.0, field.1);
        rasterize_vectors(
            &self.display_list,
            buffer,
            rw,
            rh,
            field,
            true,
            HALATION_OFF,
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

std::thread_local! {
    /// Per-frame energy accumulator, kept across frames. See `rasterize_vectors`.
    static ACCUMULATOR: std::cell::RefCell<Vec<f32>> =
        const { std::cell::RefCell::new(Vec::new()) };

    /// Reduced-resolution halation field, kept across frames for the same reason.
    static HALO: std::cell::RefCell<Vec<f32>> =
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
    field: (u32, u32),
    flip_y: bool,
    halation: f32,
) {
    buffer.fill(0);

    let w = width as i32;
    let h = height as i32;
    let y_max = h - 1;

    // Display-list coordinates are in the generator's own extent; this maps them
    // onto however many pixels we are drawing into. They are usually different:
    // a generator's units are a numeric range its programmers chose, and the
    // resolution worth drawing at comes from the tube (see
    // `raster_size_for_field`).
    let (fw, fh) = field;
    let scale = if fw > 0 && fh > 0 {
        (w as f32 / fw as f32).min(h as f32 / fh as f32)
    } else {
        1.0
    };

    // The spot's size in output pixels depends only on how many pixels the long
    // axis has, since the generator's extent cancels out of
    // `field_units * spot_fraction * (pixels / field_units)`. Floored where the
    // grid can no longer represent it.
    let sigma = beam_sigma_units(w.max(h) as f32).max(MIN_SIGMA_PIXELS);
    // Where the profile is cut off. Truncating leaves a step the height of the
    // profile there, so it has to fall below one level of an 8-bit channel:
    // 3 sigma is 1.1% of the peak and would show as a faint edge, 3.5 is 0.2%
    // and rounds away.
    let radius = (BEAM_CUTOFF_SIGMAS * sigma).ceil() as i32;

    // The halation skirt: the fraction of each spot's light that leaves the tube
    // by way of the faceplate rather than straight out of it. Zero turns it off,
    // which is what the boards ask for: compositing it is O(pixels) and costs
    // several times the sweep, and the frontend's GL path does it on the GPU for
    // nothing. See the note on the callers.
    let halo_sigma = halation_sigma_units(w.max(h) as f32);
    let halo_fraction = halation;

    // The monitor's brightness control, set where an operator would set it: a
    // full-intensity vector reaches full white along its centre and no further.
    //
    // Spreading a fixed energy across a wider spot lowers its peak, so without
    // this a machine with a coarser coordinate space looks dimmer for no reason
    // that is about the hardware. The peak of a unit-area Gaussian is
    // 1/(sigma*sqrt(2*pi)), so its reciprocal is the gain that puts the centre
    // of a line back at full scale, whatever the spot works out to.
    //
    // Halation is light taken *from* the core rather than added to it, so the
    // operator would turn the brightness up to compensate. For an isolated
    // straight line both profiles have unit area, so its centre ends up at
    // `(1 - f) + f*sigma/halo_sigma` of what the core alone would give, and the
    // reciprocal of that restores it.
    let halo_peak_share = (1.0 - halo_fraction) + halo_fraction * sigma / halo_sigma;
    let gain = sigma * std::f32::consts::TAU.sqrt() / halo_peak_share;

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
            // Beam current, before any question of how long it was applied.
            let current = INTENSITY_LUT[(line.intensity & 0xF) as usize] as f32 / 255.0 * gain;
            let colour = [
                current * line.r as f32,
                current * line.g as f32,
                current * line.b as f32,
            ];

            // Along the segment, light lands per unit of length, so what matters
            // is dwell per unit of length.
            let along = dwell_gain(line);
            let energy = [colour[0] * along, colour[1] * along, colour[2] * along];

            let (sy0, sy1) = if flip_y {
                // Normal: vector Y=0 is bottom, screen Y=0 is top.
                (
                    y_max as f32 - line.y0 * scale,
                    y_max as f32 - line.y1 * scale,
                )
            } else {
                // ROT270: Y already maps to screen-Y directly.
                (line.y0 * scale, line.y1 * scale)
            };

            let (lo, hi) = sweep_beam(
                acc,
                w,
                h,
                (line.x0 * scale, sy0),
                (line.x1 * scale, sy1),
                radius,
                &profile,
                energy,
            );
            dirty_lo = dirty_lo.min(lo);
            dirty_hi = dirty_hi.max(hi);

            // The beam stood still here before setting off, so this much light
            // lands on one point instead of being spread along a path. It is
            // what makes the corners of a shape brighter than its sides.
            //
            // A degenerate sweep deposits `sigma * sqrt(2*pi)` times what it is
            // given, that being the volume under a profile of unit width used as
            // a disc, so dividing it out leaves the dot carrying exactly the
            // light of its dwell.
            if line.dwell_cycles > 0 {
                let at_rest = (line.dwell_cycles as f32 / MIN_CYCLES_PER_UNIT)
                    / (sigma * std::f32::consts::TAU.sqrt());
                let dot = [
                    colour[0] * at_rest,
                    colour[1] * at_rest,
                    colour[2] * at_rest,
                ];
                let (lo, hi) = sweep_beam(
                    acc,
                    w,
                    h,
                    (line.x0 * scale, sy0),
                    (line.x0 * scale, sy0),
                    radius,
                    &profile,
                    dot,
                );
                dirty_lo = dirty_lo.min(lo);
                dirty_hi = dirty_hi.max(hi);
            }
        }

        if dirty_hi < dirty_lo {
            return;
        }

        if halo_fraction <= 0.0 {
            // No halation: the core is the whole picture, and only the rows the
            // vectors touched need converting.
            let from = (dirty_lo * w * 3) as usize;
            let to = ((dirty_hi + 1) * w * 3) as usize;
            for (out, e) in buffer[from..to].iter_mut().zip(acc[from..to].iter()) {
                *out = e.clamp(0.0, 255.0) as u8;
            }
            return;
        }

        // Halation spreads far past the vectors that caused it, so the rows it
        // reaches have to be converted too.
        let reach = (BEAM_CUTOFF_SIGMAS * halo_sigma).ceil() as i32;
        let out_lo = (dirty_lo - reach).max(0);
        let out_hi = (dirty_hi + reach).min(h - 1);

        HALO.with_borrow_mut(|halo| {
            let (halo_w, halo_h, down) = build_halo(acc, w, h, halo_sigma, halo);

            // Which two samples each destination row and column falls between,
            // worked out once. Done per pixel instead, this is a float divide
            // and a handful of casts on every one of them, which costs more than
            // the interpolation it feeds.
            let cols = upsample_taps(w, halo_w, down);
            let rows = upsample_taps(h, halo_h, down);

            let core_share = 1.0 - halo_fraction;
            let stride = (halo_w * 3) as usize;

            for py in out_lo..=out_hi {
                let (ry0, ry1, fy) = rows[py as usize];
                let (row0, row1) = (ry0 * stride, ry1 * stride);
                let base = (py * w * 3) as usize;

                for (px, &(cx0, cx1, fx)) in cols.iter().enumerate() {
                    let (a, b) = (cx0 * 3, cx1 * 3);
                    let o = base + px * 3;
                    for c in 0..3 {
                        // Bilinear in the reduced-resolution halo. It is a
                        // broad, smooth field, so sampling it coarsely and
                        // interpolating is invisible, and it saves blurring at
                        // full size.
                        let t = halo[row0 + a + c];
                        let top = t + (halo[row0 + b + c] - t) * fx;
                        let l = halo[row1 + a + c];
                        let bot = l + (halo[row1 + b + c] - l) * fx;
                        let glow = top + (bot - top) * fy;
                        let v = acc[o + c] * core_share + glow * halo_fraction;
                        buffer[o + c] = v.clamp(0.0, 255.0) as u8;
                    }
                }
            }
        });
    });
}

/// How much brighter this vector is than one drawn at the beam's top speed.
///
/// Light deposited per unit of length is beam current times how long the beam
/// spent there, and the intensity code only supplies the current. A vector the
/// beam crossed slowly was written over for longer and comes out brighter, which
/// is why a game that turns its analog scale down gets a hotter picture rather
/// than a smaller one.
///
/// Returns 1.0 for a generator that reports no travel time, which is the DVG:
/// with nothing to divide, the intensity code is all there is to go on.
fn dwell_gain(line: &VectorLine) -> f32 {
    if line.beam_cycles == 0 {
        return 1.0;
    }
    let dx = line.x1 - line.x0;
    let dy = line.y1 - line.y0;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1.0 {
        // A dot: all of the travel time landed on one spot rather than being
        // spread along a path, and dividing by a length near zero would send it
        // to infinity. One unit is the smallest length the display list can
        // describe, so that is the most it can concentrate into.
        return line.beam_cycles as f32 / MIN_CYCLES_PER_UNIT;
    }
    (line.beam_cycles as f32 / len) / MIN_CYCLES_PER_UNIT
}

/// One axis of the bilinear upsample: which two samples of the reduced field a
/// destination row or column falls between, and how far along.
fn upsample_taps(dst_len: i32, src_len: i32, down: i32) -> Vec<(usize, usize, f32)> {
    let inv = 1.0 / down as f32;
    (0..dst_len)
        .map(|i| {
            let s = (i as f32 + 0.5) * inv - 0.5;
            let a = s.floor().clamp(0.0, (src_len - 1) as f32) as i32;
            let b = (a + 1).min(src_len - 1);
            (a as usize, b as usize, (s - a as f32).clamp(0.0, 1.0))
        })
        .collect()
}

/// Build the halation field: the core energy, blurred to the faceplate's scale.
///
/// The blur runs at reduced resolution because the field is broad and smooth,
/// and a Gaussian of this width applied at full size would cost more than
/// everything else here put together. Returns the reduced field's dimensions and
/// the factor it was reduced by.
fn build_halo(acc: &[f32], w: i32, h: i32, halo_sigma: f32, out: &mut Vec<f32>) -> (i32, i32, i32) {
    /// Sigma to aim for in the reduced field. Small enough that the blur is a
    /// handful of taps, large enough that the reduction is not visible once the
    /// field is interpolated back up.
    const TARGET_SIGMA: f32 = 4.0;

    let down = (halo_sigma / TARGET_SIGMA).round().max(1.0) as i32;
    let (sw, sh) = ((w + down - 1) / down, (h + down - 1) / down);

    let n = (sw * sh * 3) as usize;
    if out.len() < n {
        out.resize(n, 0.0);
    }
    out[..n].fill(0.0);

    // Box-average each block down. This is a resampling of an energy field, so
    // the average is what carries the energy across rather than a point sample.
    //
    // Walked as blocks rather than per pixel, because the obvious form needs two
    // integer divisions on every source pixel to find the block it lands in, and
    // there are a million of them.
    let block = (down * down) as f32;
    for sy in 0..sh {
        let y_end = ((sy + 1) * down).min(h);
        for sx in 0..sw {
            let x_end = ((sx + 1) * down).min(w);
            let mut sum = [0f32; 3];
            for py in (sy * down)..y_end {
                let row = (py * w) as usize;
                for px in (sx * down)..x_end {
                    let src = (row + px as usize) * 3;
                    sum[0] += acc[src];
                    sum[1] += acc[src + 1];
                    sum[2] += acc[src + 2];
                }
            }
            let dst = ((sy * sw + sx) * 3) as usize;
            out[dst] = sum[0] / block;
            out[dst + 1] = sum[1] / block;
            out[dst + 2] = sum[2] / block;
        }
    }

    blur_separable(&mut out[..n], sw, sh, halo_sigma / down as f32);
    (sw, sh, down)
}

/// Separable Gaussian blur over an RGB float field, in place.
fn blur_separable(buf: &mut [f32], w: i32, h: i32, sigma: f32) {
    if sigma <= 0.0 {
        return;
    }
    let radius = (BEAM_CUTOFF_SIGMAS * sigma).ceil() as i32;
    let kernel: Vec<f32> = (-radius..=radius)
        .map(|i| (-(i * i) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let sum: f32 = kernel.iter().sum();
    let kernel: Vec<f32> = kernel.iter().map(|k| k / sum).collect();

    let mut tmp = vec![0f32; buf.len()];

    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let mut acc = 0.0;
                for (k, &weight) in kernel.iter().enumerate() {
                    let sx = (x + k as i32 - radius).clamp(0, w - 1);
                    acc += buf[((y * w + sx) * 3 + c) as usize] * weight;
                }
                tmp[((y * w + x) * 3 + c) as usize] = acc;
            }
        }
    }
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let mut acc = 0.0;
                for (k, &weight) in kernel.iter().enumerate() {
                    let sy = (y + k as i32 - radius).clamp(0, h - 1);
                    acc += tmp[((sy * w + x) * 3 + c) as usize] * weight;
                }
                buf[((y * w + x) * 3 + c) as usize] = acc;
            }
        }
    }
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

        /// A lit vector on whole-unit coordinates, which is what these tests
        /// reason in; the display list itself carries a fraction as well.
        fn white(x0: i32, y0: i32, x1: i32, y1: i32) -> VectorLine {
            VectorLine {
                x0: x0 as f32,
                y0: y0 as f32,
                x1: x1 as f32,
                y1: y1 as f32,
                intensity: 15,
                r: 255,
                g: 255,
                b: 255,
                beam_cycles: 0,
                dwell_cycles: 0,
            }
        }

        /// The core beam alone, which is what the boards ask for.
        fn render(lines: &[VectorLine]) -> Vec<u8> {
            render_with_halation(lines, HALATION_OFF)
        }

        /// The full optical model, halation included. Passed explicitly rather
        /// than taken from what the boards happen to be configured with, so
        /// these tests keep testing halation whatever that default becomes.
        fn render_with_halation(lines: &[VectorLine], halation: f32) -> Vec<u8> {
            let mut buf = vec![0u8; (W * H * 3) as usize];
            // Field and raster are the same here: these tests reason in whole
            // units and want one unit to be one pixel so the numbers they assert
            // are the numbers the sweep sees.
            rasterize_vectors(lines, &mut buf, W, H, (W, H), false, halation);
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
            // coordinates, and must not light anything beyond its reach.
            let buf = render(&[white(-100_000, 128, 100_000, 128)]);
            assert!(peak(&buf) >= 250, "the visible part is still drawn");

            // `render` is the core beam alone, so the only thing lit should be
            // the row the vector runs along, give or take the spot.
            let reach = (BEAM_CUTOFF_SIGMAS * MIN_SIGMA_PIXELS).ceil() as usize + 1;
            let mut stray = 0;
            for y in 0..H as usize {
                for x in 0..W as usize {
                    if buf[(y * W as usize + x) * 3] > 0 && y.abs_diff(128) > reach {
                        stray += 1;
                    }
                }
            }
            assert_eq!(stray, 0, "light landed beyond the beam's reach");
        }

        /// As `white`, but reporting the beam time a generator would have.
        fn timed(x0: i32, y0: i32, x1: i32, y1: i32, beam_cycles: u32) -> VectorLine {
            VectorLine {
                beam_cycles,
                ..white(x0, y0, x1, y1)
            }
        }

        #[test]
        fn a_vector_the_beam_crossed_slowly_is_brighter_per_unit_length() {
            // Light per unit length is beam current times dwell, and the
            // intensity code only supplies the current. Two identical segments
            // at the same code, one given twice the travel time, should differ
            // by exactly that factor in the light they emit.
            //
            // Drawn at a low code so there is headroom: the brightness control
            // is set so a full-intensity vector at the beam's top speed already
            // reads full white, so at that code twice the dwell only saturates.
            // That is the display blooming, which is right, but it is not what
            // this test is asking about.
            let len = 100.0f32;
            let at_top_speed = (len * MIN_CYCLES_PER_UNIT) as u32;
            let dim = |cycles| VectorLine {
                intensity: 4,
                ..timed(70, 128, 170, 128, cycles)
            };

            let quick = total_light(&render(&[dim(at_top_speed)]));
            let slow = total_light(&render(&[dim(at_top_speed * 2)]));

            let ratio = slow as f64 / quick as f64;
            assert!(
                (1.9..=2.1).contains(&ratio),
                "twice the dwell should be twice the light, got {ratio:.3}"
            );
        }

        #[test]
        fn the_beams_top_speed_is_the_brightness_the_control_is_set_against() {
            // A full-intensity vector drawn as fast as the deflection hardware
            // can move reads full white and no more. Anything slower than that
            // is brighter, which is what makes it bloom rather than what makes
            // everything else dim.
            let len = 100.0f32;
            let at_top_speed = (len * MIN_CYCLES_PER_UNIT) as u32;

            let quick = render(&[timed(70, 128, 170, 128, at_top_speed)]);
            assert!(
                (250..=255).contains(&peak(&quick)),
                "a vector at the beam's top speed should peak at full white, got {}",
                peak(&quick)
            );

            let slow = render(&[timed(70, 128, 170, 128, at_top_speed * 4)]);
            assert_eq!(peak(&slow), 255, "and a slower one saturates");
        }

        #[test]
        fn standing_still_at_a_vertex_puts_a_dot_there() {
            // The beam holds position while the sequencer fetches the next
            // instruction, so it writes one point for that whole time. Against a
            // moving line, which spreads its light along a path, that lands as a
            // bright dot at the corner.
            let len = 100.0f32;
            let at_top_speed = (len * MIN_CYCLES_PER_UNIT) as u32;
            let moving = VectorLine {
                intensity: 4,
                ..timed(70, 128, 170, 128, at_top_speed)
            };
            let parked = VectorLine {
                // Eight states at eight cycles, which is what a VCTR costs.
                dwell_cycles: 64,
                ..moving.clone()
            };

            let without = render(&[moving]);
            let with = render(&[parked]);

            // The extra light is at the start of the segment, not along it.
            let at = |buf: &[u8], x: usize| buf[(128 * W as usize + x) * 3] as i32;
            assert!(
                at(&with, 70) > at(&without, 70) + 8,
                "the vertex should be brighter: {} against {}",
                at(&with, 70),
                at(&without, 70)
            );
            assert_eq!(
                at(&with, 140),
                at(&without, 140),
                "the middle of the segment is untouched by it"
            );

            // And it is light added, not moved: the beam really was on for
            // longer.
            assert!(total_light(&with) > total_light(&without));
        }

        #[test]
        fn a_generator_that_reports_no_travel_time_falls_back_to_the_code() {
            // The DVG models no beam timing, so there is nothing to divide and
            // the intensity code is all there is. It must not come out black.
            let none = render(&[timed(70, 128, 170, 128, 0)]);
            assert!(
                peak(&none) >= 250,
                "a vector with no reported travel time went dark, peak {}",
                peak(&none)
            );
        }

        #[test]
        fn halation_moves_light_outward_and_holds_the_peak() {
            // The composite takes halation out of the core rather than adding it
            // on: (1-f)*core + f*halo. But the brightness gain is then raised so
            // a full-intensity vector still peaks at full white, and that is an
            // operator turning the monitor up, which emits more light overall.
            // So total emitted light *rises* with the halation fraction, and the
            // invariants worth pinning are where the light goes and where the
            // peak sits, not the total.
            //
            // Exercised at a fraction well above the default, which is a taste
            // value someone will keep turning: at the 0.07 it currently sits at,
            // the skirt of one thin line is under half of an 8-bit level and
            // rounds to nothing, so a test pinned to it would be testing the
            // quantiser rather than the optics. The glow earns its keep where
            // many vectors overlap, not on a single line.
            const STRONG: f32 = 0.4;

            let line = [white(60, 128, 180, 128)];
            let with_halo = render_with_halation(&line, STRONG);
            let without = render_with_halation(&line, HALATION_OFF);

            let core_reach = (BEAM_CUTOFF_SIGMAS * MIN_SIGMA_PIXELS).ceil() as usize + 1;
            let halo_reach =
                (BEAM_CUTOFF_SIGMAS * halation_sigma_units(W.max(H) as f32)).ceil() as usize;
            assert!(
                halo_reach > core_reach * 3,
                "the faceplate's glow should be far broader than the spot"
            );

            let skirt = |buf: &[u8]| -> u64 {
                let mut sum = 0;
                for y in 0..H as usize {
                    if y.abs_diff(128) <= core_reach || y.abs_diff(128) > halo_reach {
                        continue;
                    }
                    for x in 0..W as usize {
                        sum += buf[(y * W as usize + x) * 3] as u64;
                    }
                }
                sum
            };

            // Both halves: light lands out where only halation can put it, and
            // with halation off nothing lands there at all.
            assert!(skirt(&with_halo) > 0, "no halation reached beyond the core");
            assert_eq!(
                skirt(&without),
                0,
                "something other than halation lit the skirt"
            );

            // The peak holds at full white either way. This is what the gain
            // correction exists for: spreading a fixed energy into a much wider
            // skirt drops the centre, and without compensating, turning halation
            // up would quietly dim every machine.
            assert!(
                peak(&with_halo) >= 250 && peak(&without) >= 250,
                "peak went from {} to {} when halation was turned on",
                peak(&without),
                peak(&with_halo)
            );

            // And more light is emitted, not less, because that compensation is
            // the brightness control going up. A composite that added the glow
            // on top *and* compensated would overshoot this badly; one that
            // forgot to compensate would fall below it.
            let (on, off) = (total_light(&with_halo), total_light(&without));
            assert!(
                on > off,
                "turning halation up should emit more light, got {on} against {off}"
            );
            assert!(
                on < off * 3,
                "halation emitted {on} against {off}, far past what holding the \
                 peak through a wider skirt calls for"
            );
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
