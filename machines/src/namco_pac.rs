use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind, DebugTraceBuffer};
use phosphor_core::core::machine::{
    DipApplyTiming, DipChoice, DipOption, DipSwitchBank, Direction, InputControl, InputId,
    InputKind,
};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::watchpoint::DebugAccessSource;
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::z80::Z80;
use phosphor_core::device::namco_wsg::NamcoWsg;
use phosphor_core::gfx;
use phosphor_core::gfx::decode::{GfxLayout, decode_gfx};
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion};

// ---------------------------------------------------------------------------
// Memory map region IDs (shared across all Namco Pac-Man hardware games)
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum Region {
    Rom = 1,
    VideoRam = 2,
    ColorRam = 3,
    Ram = 4,
    Io = 5,
}

// ---------------------------------------------------------------------------
// Input button IDs (shared across Pac-Man family)
// ---------------------------------------------------------------------------
pub const INPUT_P1_UP: u8 = 0;
pub const INPUT_P1_LEFT: u8 = 1;
pub const INPUT_P1_RIGHT: u8 = 2;
pub const INPUT_P1_DOWN: u8 = 3;
pub const INPUT_COIN: u8 = 4;
pub const INPUT_P1_START: u8 = 5;
pub const INPUT_P2_START: u8 = 6;
pub const INPUT_P2_UP: u8 = 7;
pub const INPUT_P2_LEFT: u8 = 8;
pub const INPUT_P2_RIGHT: u8 = 9;
pub const INPUT_P2_DOWN: u8 = 10;

/// Typed logical controls shared by Pac-Man and Ms. Pac-Man. `InputId`s reuse
/// the `INPUT_*` numbering so `handle_input` and the legacy `set_input` shim
/// share one id space.
pub const NAMCO_PAC_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_P1_UP as u16),
        stable_name: "p1_up",
        label: "P1 Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_UP,
    },
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
        id: InputId(INPUT_P1_DOWN as u16),
        stable_name: "p1_down",
        label: "P1 Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_DOWN,
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
        id: InputId(INPUT_P2_UP as u16),
        stable_name: "p2_up",
        label: "P2 Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(2),
        default_bindings: crate::input_defaults::P2_UP,
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
        id: InputId(INPUT_P2_DOWN as u16),
        stable_name: "p2_down",
        label: "P2 Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(2),
        default_bindings: crate::input_defaults::P2_DOWN,
    },
];

// ---------------------------------------------------------------------------
// Timing constants
// ---------------------------------------------------------------------------
// Master clock:  18.432 MHz
// CPU clock:     18.432 / 6 = 3.072 MHz
// Pixel clock:   18.432 / 3 = 6.144 MHz
// HTOTAL:        384 pixels = 192 CPU cycles per scanline
// VTOTAL:        264 lines
// VBSTART:       224 (visible height)
// Frame:         192 × 264 = 50688 CPU cycles per frame
// Frame rate:    3072000 / 50688 ≈ 60.61 Hz

pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 3_072_000,  // 18.432 MHz / 6
    cycles_per_scanline: 192, // 384 pixels / 2
    total_scanlines: 264,     // VTOTAL
    // Native (pre-orientation) framebuffer: the board declares ROT90 and the
    // frontend rotates centrally, so these are the unrotated dimensions.
    display_width: 288,
    display_height: 224,
    display_aspect: Some((3, 4)),
};

/// The board's crystal and everything divided out of it.
///
/// One 18.432 MHz crystal, with the Z80 at /6 and the pixel clock at /3.
pub fn clock_tree() -> phosphor_core::core::ClockTree {
    use phosphor_core::core::{ClockDomainName as Clk, ClockTree, RootId};
    let mut t = ClockTree::new(18_432_000);
    let cpu = t.add_domain(Clk::Cpu, RootId::MAIN, 1, 6); // 3.072 MHz
    let dot = t.add_domain(Clk::Pixel, RootId::MAIN, 1, 3); // 6.144 MHz
    t.set_step_domain(cpu);
    // Pixel clock is exactly twice the CPU clock, so 384 dot clocks is exactly
    // 192 CPU cycles.
    t.set_raster(dot, 384, 0);
    t
}

pub const VISIBLE_LINES: u64 = 224;

// Resistor weights for palette PROM
// 3-bit RGB channels with 1K/470/220 ohm resistors
const R_WEIGHTS: [f64; 3] = [1000.0, 470.0, 220.0];
const G_WEIGHTS: [f64; 3] = [1000.0, 470.0, 220.0];
const B_WEIGHTS: [f64; 2] = [470.0, 220.0];

// ---------------------------------------------------------------------------
// GfxLayout descriptors for Pac-Man hardware
// ---------------------------------------------------------------------------

pub(crate) const PACMAN_TILE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[4, 0],
    x_offsets: &[64, 65, 66, 67, 0, 1, 2, 3],
    y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
    char_increment: 128,
};

const PACMAN_SPRITE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[4, 0],
    x_offsets: &[
        64, 65, 66, 67, 128, 129, 130, 131, 192, 193, 194, 195, 0, 1, 2, 3,
    ],
    y_offsets: &[
        0, 8, 16, 24, 32, 40, 48, 56, 256, 264, 272, 280, 288, 296, 304, 312,
    ],
    char_increment: 512,
};

// ---------------------------------------------------------------------------
// DIP switches
// ---------------------------------------------------------------------------

/// DIP switch metadata for the Pac-Man DSW1 bank (the single byte read at
/// 0x5080). Choice bit patterns and labels follow MAME's `pacman` layout; the
/// factory default of every option OR's together to the historical `0xC9`
/// (1 coin/1 credit, 3 lives, 10000 bonus, normal difficulty, normal ghosts)
/// that [`NamcoPacBoard::new`] initializes `dip_switches` to.
pub(crate) const DIP_BANKS: &[DipSwitchBank] = &[DipSwitchBank {
    name: "DSW1",
    options: &[
        DipOption {
            name: "Coinage",
            mask: 0x03,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "Free Play",
                    value: 0x00,
                },
                DipChoice {
                    label: "1 Coin/1 Credit",
                    value: 0x01,
                },
                DipChoice {
                    label: "1 Coin/2 Credits",
                    value: 0x02,
                },
                DipChoice {
                    label: "2 Coins/1 Credit",
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
                    label: "1",
                    value: 0x00,
                },
                DipChoice {
                    label: "2",
                    value: 0x04,
                },
                DipChoice {
                    label: "3",
                    value: 0x08,
                },
                DipChoice {
                    label: "5",
                    value: 0x0C,
                },
            ],
        },
        DipOption {
            name: "Bonus Life",
            mask: 0x30,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "10000",
                    value: 0x00,
                },
                DipChoice {
                    label: "15000",
                    value: 0x10,
                },
                DipChoice {
                    label: "20000",
                    value: 0x20,
                },
                DipChoice {
                    label: "None",
                    value: 0x30,
                },
            ],
        },
        DipOption {
            name: "Difficulty",
            mask: 0x40,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "Hard",
                    value: 0x00,
                },
                DipChoice {
                    label: "Normal",
                    value: 0x40,
                },
            ],
        },
        DipOption {
            name: "Ghost Names",
            mask: 0x80,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "Alternate",
                    value: 0x00,
                },
                DipChoice {
                    label: "Normal",
                    value: 0x80,
                },
            ],
        },
    ],
}];

// ---------------------------------------------------------------------------
// Bus wiring
// ---------------------------------------------------------------------------

/// A Pac-Man-family bus: the shared board, plus whatever a particular game
/// interposes in front of it (Ms. Pac-Man's decode latch and daughter-card
/// ROMs, for instance).
///
/// Implemented by concrete types only — [`tick`] is generic over it, so every
/// bus access the Z80 makes resolves to a direct, inlinable call.
pub trait NamcoPacBus: Bus<Address = u16, Data = u8> {
    fn board(&mut self) -> &mut NamcoPacBoard;
}

/// One CPU cycle of a Pac-Man-family machine: board work, the Z80's cycle, then
/// the clock advance.
///
/// Callers hold the CPU and the bus as separate fields, so this takes them as
/// two disjoint borrows rather than splitting one struct behind a raw pointer.
///
/// This is the debugger's path — it tests the frame position on every cycle.
/// A whole frame goes through [`run_scanlines`], which hoists that test out.
#[inline]
pub fn tick<B: NamcoPacBus>(cpu: &mut Z80, bus: &mut B) {
    bus.board().begin_cycle(cpu);
    cpu.execute_cycle(bus, BusMaster::Cpu(0));
    bus.board().end_cycle();
}

/// Run one frame's worth of cycles.
///
/// Whole scanlines go through [`run_scanlines`]; any partial scanline at either
/// end — which only happens when the debugger has left the clock off-boundary —
/// goes through [`tick`], so the frame is the same sequence of cycles either
/// way.
pub fn run_frame<B: NamcoPacBus>(cpu: &mut Z80, bus: &mut B) {
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

/// Run `cycles` CPU cycles, scanline-outer and cycle-inner.
///
/// The scanline-boundary work — rendering a line, asserting VBLANK — happens
/// 264 times a frame, not on each of the 50,688 cycles, so the inner loop is
/// the sound generator plus the CPU and nothing else. The caller must start on
/// a scanline boundary and pass a multiple of `cycles_per_scanline`; the
/// debugger's off-boundary stepping goes through [`tick`] instead.
pub fn run_scanlines<B: NamcoPacBus>(cpu: &mut Z80, bus: &mut B, cycles: u64) {
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
            bus.board().begin_cycle_inner(cpu);
            cpu.execute_cycle(bus, BusMaster::Cpu(0));
            bus.board().end_cycle();
        }
    }
}

/// The base board is itself a complete bus for games that add nothing to it.
impl NamcoPacBus for NamcoPacBoard {
    #[inline]
    fn board(&mut self) -> &mut NamcoPacBoard {
        self
    }
}

/// Base Pac-Man hardware address decoding. A15 is not connected, so the upper
/// 32K mirrors the lower; games that overlay their own ROM banking above
/// 0x8000 (Ms. Pac-Man) intercept before delegating here.
impl Bus for NamcoPacBoard {
    type Address = u16;
    type Data = u8;

    #[inline]
    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        self.bus_read_common(addr & 0x7FFF)
    }

    #[inline]
    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        self.bus_write_common(addr & 0x7FFF, data);
    }

    #[inline]
    fn io_read(&mut self, _master: BusMaster, _addr: u16) -> u8 {
        0xFF // No I/O read ports on this hardware
    }

    #[inline]
    fn io_write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        // Port 0x00: interrupt vector byte latch (Z80 IM2)
        if addr & 0xFF == 0x00 {
            self.interrupt_vector = data;
        }
    }

    #[inline]
    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false // No DMA hardware
    }

    #[inline]
    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            nmi: false,
            irq: self.vblank_irq_pending && self.irq_enabled,
            firq: false,
            irq_vector: self.interrupt_vector,
            irq_level: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// NamcoPacBoard — shared hardware for the Namco Pac-Man platform
// ---------------------------------------------------------------------------

/// Namco Pac-Man hardware base (Z80 @ 3.072 MHz, Namco WSG 3-voice, tilemap + sprites).
///
/// Shared by Pac-Man, Ms. Pac-Man, and other games on identical hardware.
///
/// The board is everything the Z80 talks *to* — the CPU itself lives in the
/// game wrapper. Keeping them in separate structs is what lets
/// `cpu.execute_cycle(&mut bus, ..)` borrow-check without a raw-pointer split,
/// so bus dispatch monomorphises at a concrete type instead of going through
/// `&mut dyn Bus` on every access.
#[derive(BusDebug, DebugTrace)]
pub struct NamcoPacBoard {
    #[debug_map(cpu = 0)]
    pub(crate) map: AddressSpace16,

    pub(crate) sprite_coords: [u8; 0x10], // 0x5060-0x506F: sprite X/Y positions

    // Sound
    #[debug_device("NamcoWSG")]
    pub(crate) wsg: NamcoWsg,

    // Pre-decoded GFX caches (from GFX ROM)
    pub(crate) tile_cache: gfx::GfxCache,
    pub(crate) sprite_cache: gfx::GfxCache,

    // PROMs
    pub(crate) palette_prom: [u8; 32],
    pub(crate) color_lut_prom: [u8; 256],

    // Pre-computed palette (32 RGB entries from PROM resistor weighting)
    pub(crate) palette_rgb: [(u8, u8, u8); 32],

    // Scanline-rendered framebuffer (288 x 224 x RGB24 = 193,536 bytes).
    // Native orientation, populated incrementally during run_frame().
    pub(crate) scanline_buffer: Vec<u8>,

    // I/O state (active-low: 0xFF = all released)
    pub(crate) in0: u8,
    pub(crate) in1: u8,
    pub(crate) dip_switches: u8,

    // 74LS259 addressable latch outputs
    pub(crate) irq_enabled: bool,
    pub(crate) sound_enabled: bool,
    pub(crate) flip_screen: bool,

    // Interrupt
    pub(crate) interrupt_vector: u8,
    pub(crate) vblank_irq_pending: bool,

    // Timing
    pub(crate) clock: u64,
    pub(crate) watchdog_counter: u32,

    // Debug event ring (observer state — never saved in save states)
    #[debug_events]
    pub(crate) debug_trace: DebugTraceBuffer,
}

impl Default for NamcoPacBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl NamcoPacBoard {
    pub fn new() -> Self {
        Self {
            map: Self::build_map(),
            sprite_coords: [0; 0x10],
            wsg: NamcoWsg::new(TIMING.cpu_clock_hz),
            tile_cache: gfx::GfxCache::new(256, 8, 8),
            sprite_cache: gfx::GfxCache::new(64, 16, 16),
            palette_prom: [0; 32],
            color_lut_prom: [0; 256],
            palette_rgb: [(0, 0, 0); 32],
            scanline_buffer: vec![0u8; 288 * 224 * 3],
            in0: 0xFF,
            in1: 0xFF,
            // Default DIP: 1 coin/1 credit, 3 lives, 10000 bonus, normal difficulty, normal ghosts
            dip_switches: 0xC9,
            irq_enabled: false,
            sound_enabled: false,
            flip_screen: false,
            interrupt_vector: 0,
            vblank_irq_pending: false,
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
            Region::VideoRam,
            "Video RAM",
            0x4000,
            0x0400,
            AccessKind::ReadWrite,
        )
        .region(
            Region::ColorRam,
            "Color RAM",
            0x4400,
            0x0400,
            AccessKind::ReadWrite,
        )
        .region(Region::Ram, "RAM", 0x4C00, 0x0400, AccessKind::ReadWrite)
        .region(Region::Io, "I/O", 0x5000, 0x0100, AccessKind::Io);
        map
    }

    // -----------------------------------------------------------------------
    // Core tick — the board half of one CPU cycle (see [`tick`])
    // -----------------------------------------------------------------------

    /// Board work that happens before the CPU's cycle: scanline rendering,
    /// VBLANK interrupt assertion, the sound generator, and latching debug
    /// attribution context.
    fn begin_cycle(&mut self, cpu: &Z80) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();
        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            self.begin_scanline(frame_cycle / TIMING.cycles_per_scanline);
        }
        self.begin_cycle_inner(cpu);
    }

    /// Work that only happens on the first cycle of a scanline: rendering that
    /// line, and asserting VBLANK when the beam leaves the visible area.
    ///
    /// Called once per scanline from [`run_scanlines`] and, for the debugger's
    /// single-step path, from `begin_cycle` when the clock lands on a boundary.
    fn begin_scanline(&mut self, scanline: u64) {
        // Per-scanline rendering: render the current scanline from VRAM +
        // sprites before the CPU processes it, matching hardware CRT read
        // timing.
        if scanline < VISIBLE_LINES {
            self.render_scanline(scanline as usize);
        }

        // VBLANK interrupt: fire at the start of VBLANK (scanline 224)
        if scanline == VISIBLE_LINES {
            self.vblank_irq_pending = true;
            if self.debug_trace.enabled() {
                self.debug_trace.record(DebugEvent {
                    cpu_index: Some(0),
                    detail: Some("VBLANK IRQ"),
                    ..DebugEvent::new(
                        self.clock,
                        DebugAccessSource::Unknown,
                        DebugEventKind::InterruptAssert,
                    )
                });
            }
        }
    }

    /// Per-cycle board work, with no frame-position tests in it.
    fn begin_cycle_inner(&mut self, cpu: &Z80) {
        // WSG tick (runs at CPU clock rate)
        self.wsg.tick();

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
        self.watchdog_counter += 1;
    }

    // -----------------------------------------------------------------------
    // Bus dispatch helpers — called from game wrapper Bus impls
    // -----------------------------------------------------------------------

    /// Record a bus event (caller gates on `debug_trace.enabled()`).
    /// All bus accesses on this single-CPU board originate from CPU 0.
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

    /// Shared memory read logic for all Namco Pac hardware.
    /// Caller is responsible for address masking (e.g. A15 mirror).
    pub fn bus_read_common(&mut self, addr: u16) -> u8 {
        let data = match self.map.page(addr).region_id {
            Region::ROM | Region::VIDEO_RAM | Region::COLOR_RAM | Region::RAM => {
                self.map.read_backing(addr)
            }

            Region::IO => match addr {
                0x5000..=0x503F => self.in0,
                0x5040..=0x507F => self.in1,
                0x5080..=0x50BF => self.dip_switches,
                _ => 0xFF,
            },

            _ => {
                // Bus float at 0x4800-0x4BFF (no device responds)
                if (0x4800..0x4C00).contains(&addr) {
                    0xBF
                } else {
                    0xFF
                }
            }
        };

        // Single-CPU board: all bus accesses originate from CPU 0.
        self.map.watch_read(0, BusMaster::Cpu(0), addr, data);
        // Trace I/O reads only — memory reads (instruction fetches) would
        // drown the ring.
        if self.debug_trace.enabled() && self.map.page(addr).region_id == Region::IO {
            self.trace_access(DebugEventKind::DeviceRead, addr, data, None, None);
        }
        data
    }

    /// Shared memory write logic for all Namco Pac hardware.
    /// Caller is responsible for address masking (e.g. A15 mirror).
    pub fn bus_write_common(&mut self, addr: u16, data: u8) {
        self.map.watch_write(0, BusMaster::Cpu(0), addr, data);

        if self.debug_trace.enabled() {
            let (kind, device, detail) = if self.map.page(addr).region_id == Region::IO {
                match addr {
                    0x5000..=0x5007 => (
                        DebugEventKind::DeviceWrite,
                        Some("I/O latch"),
                        Some(match addr & 7 {
                            0 => "IRQ enable",
                            1 => "sound enable",
                            3 => "flip screen",
                            _ => "latch bit",
                        }),
                    ),
                    0x5040..=0x505F => (DebugEventKind::DeviceWrite, Some("NamcoWSG"), None),
                    0x5060..=0x506F => (
                        DebugEventKind::DeviceWrite,
                        Some("Sprites"),
                        Some("sprite coordinate"),
                    ),
                    0x50C0..=0x50FF => (DebugEventKind::Watchdog, None, Some("watchdog cleared")),
                    _ => (DebugEventKind::IoWrite, None, None),
                }
            } else {
                (DebugEventKind::MemoryWrite, None, None)
            };
            self.trace_access(kind, addr, data, device, detail);
        }

        match self.map.page(addr).region_id {
            Region::VIDEO_RAM | Region::COLOR_RAM | Region::RAM => {
                self.map.write_backing(addr, data);
            }

            Region::IO => match addr {
                // 74LS259 addressable latch: address bits 0-2 select output, data bit 0 is value
                0x5000..=0x5007 => {
                    let bit = (addr & 7) as u8;
                    let value = (data & 1) != 0;
                    match bit {
                        0 => {
                            self.irq_enabled = value;
                            if !value {
                                self.vblank_irq_pending = false;
                            }
                        }
                        1 => {
                            self.sound_enabled = value;
                            self.wsg.set_sound_enabled(value);
                        }
                        3 => self.flip_screen = value,
                        // 2: unused, 4-5: LEDs (not connected), 6: coin lockout, 7: coin counter
                        _ => {}
                    }
                }

                // Namco WSG sound registers (32 nibble registers)
                0x5040..=0x505F => self.wsg.write(addr - 0x5040, data),

                // Sprite coordinates
                0x5060..=0x506F => self.sprite_coords[(addr - 0x5060) as usize] = data,

                // Watchdog reset
                0x50C0..=0x50FF => self.watchdog_counter = 0,

                _ => {}
            },

            _ => { /* ROM or unmapped: ignored */ }
        }
    }

    // -----------------------------------------------------------------------
    // Palette
    // -----------------------------------------------------------------------

    /// Pre-compute the 32-entry RGB palette from the palette PROM using
    /// resistor-weighted DAC values.
    pub fn build_palette(&mut self) {
        use phosphor_core::gfx::{combine_weights, compute_resistor_weights};

        let r_w = compute_resistor_weights(&R_WEIGHTS, None);
        let g_w = compute_resistor_weights(&G_WEIGHTS, None);
        let b_w = compute_resistor_weights(&B_WEIGHTS, None);

        for i in 0..32 {
            let entry = self.palette_prom[i];

            let r = combine_weights(&r_w, &[entry & 1, (entry >> 1) & 1, (entry >> 2) & 1]);
            let g = combine_weights(
                &g_w,
                &[(entry >> 3) & 1, (entry >> 4) & 1, (entry >> 5) & 1],
            );
            let b = combine_weights(&b_w, &[(entry >> 6) & 1, (entry >> 7) & 1]);

            self.palette_rgb[i] = (r, g, b);
        }
    }

    // -----------------------------------------------------------------------
    // ROM loading helpers
    // -----------------------------------------------------------------------

    pub fn load_program_rom(&mut self, data: &[u8]) {
        self.map.load_region(Region::Rom, data);
    }

    pub fn load_gfx_rom(&mut self, gfx_data: &[u8]) {
        self.tile_cache = decode_gfx(gfx_data, 0x0000, 256, &PACMAN_TILE_LAYOUT);
        self.sprite_cache = decode_gfx(gfx_data, 0x1000, 64, &PACMAN_SPRITE_LAYOUT);
    }

    pub fn load_color_proms(&mut self, color_data: &[u8]) {
        self.palette_prom.copy_from_slice(&color_data[0..32]);
        self.color_lut_prom.copy_from_slice(&color_data[32..288]);
        self.build_palette();
    }

    pub fn load_sound_prom(&mut self, sound_data: &[u8]) {
        self.wsg.load_waveform_rom(sound_data);
    }

    // -----------------------------------------------------------------------
    // CPU state accessors
    // -----------------------------------------------------------------------

    pub fn clock(&self) -> u64 {
        self.clock
    }

    // -----------------------------------------------------------------------
    // Video rendering
    // -----------------------------------------------------------------------

    /// Render a single scanline from current VRAM/sprite state into the scanline buffer.
    /// Composites tiles then sprites for native scanline Y (0-223).
    fn render_scanline(&mut self, scanline: usize) {
        let row_offset = scanline * 288 * 3;

        // Split borrows: immutable refs for closures, mutable ref for buffer
        let video_ram = self.map.region_data(Region::VideoRam);
        let color_ram = self.map.region_data(Region::ColorRam);
        let color_lut_prom = &self.color_lut_prom;
        let palette_rgb = &self.palette_rgb;
        let tile_cache = &self.tile_cache;
        let sprite_cache = &self.sprite_cache;
        let buf = &mut self.scanline_buffer[row_offset..row_offset + 288 * 3];

        // Inline color resolution (captures split borrows, not &self)
        let resolve = |attribute: u8, pixel_value: u8| -> (u8, u8, u8) {
            let lut_index = ((attribute & 0x1F) as usize) * 4 + pixel_value as usize;
            let palette_index = if lut_index < 256 {
                (color_lut_prom[lut_index] & 0x0F) as usize
            } else {
                0
            };
            palette_rgb[palette_index]
        };

        // Fill scanline with background color
        let bg = resolve(0, 0);
        for x in 0..288 {
            let off = x * 3;
            buf[off] = bg.0;
            buf[off + 1] = bg.1;
            buf[off + 2] = bg.2;
        }

        // Tiles: use shared tilemap renderer
        let config = gfx::TilemapConfig {
            cols: 36,
            rows: 28,
            tile_width: 8,
            tile_height: 8,
        };

        gfx::tilemap::render_tilemap_scanline(
            &config,
            tile_cache,
            scanline,
            |col, row| {
                let offset = crate::namco_video::namco_tilemap_offset(col as i32, row as i32);
                let tile_code = if offset < 0x400 {
                    video_ram[offset] as u16
                } else {
                    0
                };
                let attribute = if offset < 0x400 { color_ram[offset] } else { 0 };
                gfx::TileInfo::new(tile_code, attribute)
            },
            // Pac-Man's tilemap is opaque — every pixel writes.
            |attr, pv| Some(resolve(attr, pv)),
            buf,
            0,
        );

        // Sprites: draw in priority order (7→3, then 2→0 with +1 Y offset)
        let ram = self.map.region_data(Region::Ram);
        let sprite_coords = &self.sprite_coords;
        let y = scanline as i32;

        for pass in 0..2 {
            let (start, end, y_offset): (usize, usize, i32) =
                if pass == 0 { (7, 3, 0) } else { (2, 0, 1) };

            let mut offs = start;
            loop {
                let attr_base = 0x3F0 + offs * 2;
                let coord_base = offs * 2;

                let sprite_byte0 = ram[attr_base];
                let sprite_byte1 = ram[attr_base + 1];

                let sprite_code = (sprite_byte0 >> 2) as u16;
                let x_flip = (sprite_byte0 & 1) != 0;
                let y_flip = (sprite_byte0 & 2) != 0;
                let attribute = sprite_byte1 & 0x1F;

                let sx = 272i32 - sprite_coords[coord_base + 1] as i32;
                let sy = sprite_coords[coord_base] as i32 - 31 + y_offset;

                if y >= sy && y < sy + 16 {
                    let spy = (y - sy) as u8;
                    let src_py = if y_flip { 15 - spy } else { spy };

                    // Pre-compute transparency mask for this sprite's attribute
                    let trans_base = (attribute as usize & 0x1F) * 4;
                    let mut trans_mask: u8 = 0;
                    for pv in 0..4u8 {
                        if (color_lut_prom[trans_base + pv as usize] & 0x0F) == 0 {
                            trans_mask |= 1 << pv;
                        }
                    }

                    let clip = gfx::sprite::SpriteClip {
                        x_min: 16,
                        x_max: 272,
                        wrap_offset: Some(-256), // tunnel wraparound
                    };
                    gfx::sprite::draw_sprite_row(
                        sprite_cache,
                        sprite_code,
                        src_py as usize,
                        sx,
                        x_flip,
                        |pv| (trans_mask >> pv) & 1 != 0,
                        |pv| resolve(attribute, pv),
                        buf,
                        &clip,
                    );
                }

                if offs == end {
                    break;
                }
                offs -= 1;
            }
        }
    }

    /// Copy the native scanline buffer (288w × 224h RGB24) into the output
    /// buffer in native row-major order.
    ///
    /// The 90° rotation the cabinet needs is declared via
    /// [`orientation`](Self::orientation) and applied centrally by the frontend,
    /// so this emits pixels unrotated.
    pub fn render_frame(&self, buffer: &mut [u8]) {
        buffer.copy_from_slice(&self.scanline_buffer);
    }

    /// The Namco Pac-Man monitor is mounted rotated 90°. The orientation is
    /// declarative — the frontend rotates `render_frame`'s native output.
    pub fn orientation(&self) -> phosphor_core::core::machine::Orientation {
        phosphor_core::core::machine::Orientation::ROT90
    }

    // -----------------------------------------------------------------------
    // Audio
    // -----------------------------------------------------------------------

    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.wsg.fill_audio(buffer)
    }

    // -----------------------------------------------------------------------
    // Reset
    // -----------------------------------------------------------------------

    /// Reset all board state except ROMs, GFX caches, and palette.
    /// The caller resets the CPU separately — it lives in the game wrapper.
    pub fn reset_board(&mut self) {
        self.wsg.reset();
        self.irq_enabled = false;
        self.sound_enabled = false;
        self.flip_screen = false;
        self.interrupt_vector = 0;
        self.vblank_irq_pending = false;
        self.clock = 0;
        self.watchdog_counter = 0;
        self.in0 = 0xFF;
        self.in1 = 0xFF;
        self.map.region_data_mut(Region::VideoRam).fill(0);
        self.map.region_data_mut(Region::ColorRam).fill(0);
        self.map.region_data_mut(Region::Ram).fill(0);
        self.sprite_coords = [0; 0x10];
        self.scanline_buffer.fill(0);
    }
}

impl Saveable for NamcoPacBoard {
    fn save_state(&self, w: &mut StateWriter) {
        w.write_bytes(self.map.region_data(Region::VideoRam));
        w.write_bytes(self.map.region_data(Region::ColorRam));
        w.write_bytes(self.map.region_data(Region::Ram));
        w.write_bytes(&self.sprite_coords);
        self.wsg.save_state(w);
        w.write_u8(self.in0);
        w.write_u8(self.in1);
        w.write_bool(self.irq_enabled);
        w.write_bool(self.sound_enabled);
        w.write_bool(self.flip_screen);
        w.write_u8(self.interrupt_vector);
        w.write_bool(self.vblank_irq_pending);
        w.write_u64_le(self.clock);
        w.write_u32_le(self.watchdog_counter);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        r.read_bytes_into(self.map.region_data_mut(Region::VideoRam))?;
        r.read_bytes_into(self.map.region_data_mut(Region::ColorRam))?;
        r.read_bytes_into(self.map.region_data_mut(Region::Ram))?;
        r.read_bytes_into(&mut self.sprite_coords)?;
        self.wsg.load_state(r)?;
        self.in0 = r.read_u8()?;
        self.in1 = r.read_u8()?;
        self.irq_enabled = r.read_bool()?;
        self.sound_enabled = r.read_bool()?;
        self.flip_screen = r.read_bool()?;
        self.interrupt_vector = r.read_u8()?;
        self.vblank_irq_pending = r.read_bool()?;
        self.clock = r.read_u64_le()?;
        self.watchdog_counter = r.read_u32_le()?;
        Ok(())
    }
}

impl NamcoPacBoard {
    /// Dispatch an input event to the appropriate port bit (active-low).
    /// Called from game wrapper `InputConfigurable::handle_input` impls.
    pub fn handle_input(&mut self, button: u8, pressed: bool) {
        match button {
            INPUT_P1_UP => crate::set_bit_active_low(&mut self.in0, 0, pressed),
            INPUT_P1_LEFT => crate::set_bit_active_low(&mut self.in0, 1, pressed),
            INPUT_P1_RIGHT => crate::set_bit_active_low(&mut self.in0, 2, pressed),
            INPUT_P1_DOWN => crate::set_bit_active_low(&mut self.in0, 3, pressed),
            INPUT_COIN => crate::set_bit_active_low(&mut self.in0, 5, pressed),
            INPUT_P2_UP => crate::set_bit_active_low(&mut self.in1, 0, pressed),
            INPUT_P2_LEFT => crate::set_bit_active_low(&mut self.in1, 1, pressed),
            INPUT_P2_RIGHT => crate::set_bit_active_low(&mut self.in1, 2, pressed),
            INPUT_P2_DOWN => crate::set_bit_active_low(&mut self.in1, 3, pressed),
            INPUT_P1_START => crate::set_bit_active_low(&mut self.in1, 5, pressed),
            INPUT_P2_START => crate::set_bit_active_low(&mut self.in1, 6, pressed),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    mod debug_events {
        use super::*;

        #[test]
        fn tracing_disabled_records_nothing() {
            let mut board = NamcoPacBoard::new();
            board.bus_write_common(0x5045, 0x07);
            board.bus_read_common(0x5000);
            assert!(board.debug_trace.is_empty());
        }

        #[test]
        fn wsg_write_emits_device_write_event() {
            let mut board = NamcoPacBoard::new();
            board.debug_trace.set_enabled(true);
            board.clock = 777;

            board.bus_write_common(0x5045, 0x07);

            let events = board.debug_trace.events();
            assert_eq!(events.len(), 1);
            let e = &events[0];
            assert_eq!(e.kind, DebugEventKind::DeviceWrite);
            assert_eq!(e.device, Some("NamcoWSG"));
            assert_eq!(e.cycle, 777);
            assert_eq!(e.addr, Some(0x5045));
            assert_eq!(e.value, Some(0x07));
            assert_eq!(e.region, Some("I/O"));
        }

        #[test]
        fn latch_and_watchdog_writes_emit_annotated_events() {
            let mut board = NamcoPacBoard::new();
            board.debug_trace.set_enabled(true);

            board.bus_write_common(0x5000, 0x01); // IRQ enable
            board.bus_write_common(0x50C0, 0x00); // watchdog clear

            let events = board.debug_trace.events();
            assert_eq!(events[0].kind, DebugEventKind::DeviceWrite);
            assert_eq!(events[0].device, Some("I/O latch"));
            assert_eq!(events[0].detail, Some("IRQ enable"));
            assert_eq!(events[1].kind, DebugEventKind::Watchdog);
            assert_eq!(events[1].detail, Some("watchdog cleared"));
        }

        #[test]
        fn io_reads_traced_memory_writes_plain() {
            let mut board = NamcoPacBoard::new();
            board.debug_trace.set_enabled(true);

            board.bus_read_common(0x5080); // DIP switches: DeviceRead
            board.bus_read_common(0x4C10); // RAM: not traced
            board.bus_write_common(0x4C10, 0x42); // RAM: MemoryWrite

            let events = board.debug_trace.events();
            assert_eq!(events.len(), 2);
            assert_eq!(events[0].kind, DebugEventKind::DeviceRead);
            assert_eq!(events[0].addr, Some(0x5080));
            assert_eq!(events[1].kind, DebugEventKind::MemoryWrite);
            assert_eq!(events[1].region, Some("RAM"));
        }
    }
}
