use phosphor_core::core::address_space16::{AddressSpace16, WriteAnnotation};
use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind};
use phosphor_core::core::machine::{
    ActionRole, Direction, InputControl, InputId, InputKind, TimingConfig,
};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{Bus, BusMaster, ClockDomainName as Clk, ClockTree, DomainId};
use phosphor_core::cpu::z80::Z80;
use phosphor_core::device::namco_wsg::NamcoWsg;
use phosphor_core::device::namco06::Namco06;
use phosphor_core::device::namco50::Namco50;
use phosphor_core::device::namco51::Namco51;
use phosphor_core::device::namco51_lle::Namco51Lle;
use phosphor_core::device::namco53::Namco53;
use phosphor_core::gfx::decode::GfxLayout;
use phosphor_macros::{MemoryRegion, Saveable};

// ---------------------------------------------------------------------------
// Memory map region IDs
// ---------------------------------------------------------------------------

/// Regions the board itself declares: the three program ROMs, one per CPU.
///
/// Each game wrapper declares its own RAM windows on the same map with ids
/// from 4 up, naming them for the debugger (see e.g. [`crate::galaga::Region`]).
/// Region id 0 is reserved by the core for "unmapped", so ids start at 1.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub enum Region {
    MainRom = 1,
    SubRom = 2,
    SoundRom = 3,
}

// ---------------------------------------------------------------------------
// Input button IDs (shared across Galaga family)
// ---------------------------------------------------------------------------
pub const INPUT_P1_UP: u8 = 0;
pub const INPUT_P1_RIGHT: u8 = 1;
pub const INPUT_P1_DOWN: u8 = 2;
pub const INPUT_P1_LEFT: u8 = 3;
pub const INPUT_P2_UP: u8 = 4;
pub const INPUT_P2_RIGHT: u8 = 5;
pub const INPUT_P2_DOWN: u8 = 6;
pub const INPUT_P2_LEFT: u8 = 7;
pub const INPUT_P1_BUTTON1: u8 = 8;
pub const INPUT_P2_BUTTON1: u8 = 9;
pub const INPUT_START1: u8 = 10;
pub const INPUT_START2: u8 = 11;
pub const INPUT_COIN1: u8 = 12;
pub const INPUT_COIN2: u8 = 13;
pub const INPUT_SERVICE: u8 = 14;
// Second action button (Xevious blaster/bomb). Galaga and Dig Dug have only
// one button, so these IDs are unused by NAMCO_GALAGA_CONTROLS; Xevious adds
// them to its own control table and routes them to the DSWB port bits.
pub const INPUT_P1_BUTTON2: u8 = 15;
pub const INPUT_P2_BUTTON2: u8 = 16;

/// Typed logical controls shared across the Galaga family (Galaga, Dig Dug).
/// `InputId`s reuse the `INPUT_*` numbering. Default bindings mirror the legacy
/// name-matched defaults.
pub const NAMCO_GALAGA_CONTROLS: &[InputControl] = &[
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
        id: InputId(INPUT_P1_BUTTON1 as u16),
        stable_name: "p1_fire",
        label: "P1 Fire",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P2_BUTTON1 as u16),
        stable_name: "p2_fire",
        label: "P2 Fire",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(2),
        default_bindings: &[],
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
        id: InputId(INPUT_COIN1 as u16),
        stable_name: "coin1",
        label: "Coin 1",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(INPUT_COIN2 as u16),
        stable_name: "coin2",
        label: "Coin 2",
        kind: InputKind::Coin,
        player: None,
        // Unbound by default: coin slot 2 must not share the coin-1 key, or a
        // single coin key press would insert into both slots (two credits).
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_SERVICE as u16),
        stable_name: "service",
        label: "Service",
        kind: InputKind::Service,
        player: None,
        default_bindings: crate::input_defaults::SERVICE,
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
    display_width: 224,       // rotated 90° CCW from native 288×224
    display_height: 288,
    display_aspect: Some((3, 4)),
};

/// The board's crystal and everything divided out of it.
///
/// One 18.432 MHz crystal: all three Z80s at /6, the pixel clock at /3, and the
/// Namco 51xx at /12, which is the divide-by-two the board applies to the CPU
/// clock.
pub fn clock_tree() -> phosphor_core::core::ClockTree {
    use phosphor_core::core::RootId;
    let mut t = ClockTree::new(18_432_000);
    let cpu = t.add_domain(Clk::Cpu, RootId::MAIN, 1, 6); // 3.072 MHz
    t.add_domain(Clk::Cpu2, RootId::MAIN, 1, 6);
    t.add_domain(Clk::Cpu3, RootId::MAIN, 1, 6);
    let dot = t.add_domain(Clk::Pixel, RootId::MAIN, 1, 3); // 6.144 MHz
    t.add_domain(Clk::Mcu, RootId::MAIN, 1, 12); // Namco 51xx at 1.536 MHz
    t.set_step_domain(cpu);
    // Pixel clock is exactly twice the CPU clock, so 384 dot clocks is exactly
    // 192 CPU cycles.
    t.set_raster(dot, 384, 0);
    t
}

const VISIBLE_LINES: u64 = 224;

/// CPU clock / 06XX clock = 3.072 MHz / 48 kHz = 64.
const NAMCO06_BASE_DIVISOR: u32 = 64;

// Resistor weights for palette PROM (same as Pac-Man)
const R_WEIGHTS: [f64; 3] = [1000.0, 470.0, 220.0];
const G_WEIGHTS: [f64; 3] = [1000.0, 470.0, 220.0];
const B_WEIGHTS: [f64; 2] = [470.0, 220.0];

// ---------------------------------------------------------------------------
// GfxLayout descriptors for Galaga-family hardware
// ---------------------------------------------------------------------------

pub(crate) const GALAGA_SPRITE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[4, 0],
    x_offsets: &[
        0, 1, 2, 3, 64, 65, 66, 67, 128, 129, 130, 131, 192, 193, 194, 195,
    ],
    y_offsets: &[
        0, 8, 16, 24, 32, 40, 48, 56, 256, 264, 272, 280, 288, 296, 304, 312,
    ],
    char_increment: 512,
};

// ---------------------------------------------------------------------------
// Namco 51XX wrapper — HLE (behavioral) or LLE (MB8843 firmware)
// ---------------------------------------------------------------------------

/// Namco 51XX emulation mode: either high-level emulation (HLE, behavioral
/// model) or low-level emulation (LLE, running actual MB8843 firmware ROM).
pub(crate) enum Namco51Wrapper {
    /// Behavioral emulation of the 51XX firmware (no ROM required).
    Hle(Namco51),
    /// Cycle-accurate MB8843 MCU running the 51XX firmware ROM.
    Lle(Namco51Lle),
}

impl Namco51Wrapper {
    fn read(&mut self, in0: u8, in1: u8) -> u8 {
        match self {
            Self::Hle(n) => n.read(in0, in1),
            Self::Lle(n) => n.read(),
        }
    }

    fn write(&mut self, data: u8) {
        match self {
            Self::Hle(n) => n.write(data),
            Self::Lle(n) => n.write(data),
        }
    }

    fn reset(&mut self) {
        match self {
            Self::Hle(n) => n.reset(),
            Self::Lle(n) => n.reset(),
        }
    }

    /// Enable the Xevious 6-argument coinage quirk. Only meaningful for the HLE
    /// path; the LLE MCU reproduces the quirk from its firmware.
    fn set_xevious_coinage_kludge(&mut self, on: bool) {
        if let Self::Hle(n) = self {
            n.set_xevious_coinage_kludge(on);
        }
    }
}

/// Mode discriminants for the wrapper's own body.
const NAMCO51_MODE_HLE: u8 = 0;
const NAMCO51_MODE_LLE: u8 = 1;

/// Hand-written, and staying that way: which mode this chip is in is decided by
/// whether a 51XX firmware ROM was found, and the LLE variant cannot be
/// constructed from a file at all. A derive would have to build the variant the
/// bytes name, which is exactly what must not happen here.
impl Saveable for Namco51Wrapper {
    fn save_state(&self, w: &mut StateWriter) {
        match self {
            Self::Hle(n) => {
                w.write_u8(NAMCO51_MODE_HLE);
                n.save_state(w);
            }
            Self::Lle(n) => {
                w.write_u8(NAMCO51_MODE_LLE);
                n.save_state(w);
            }
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        match r.read_u8()? {
            NAMCO51_MODE_HLE => {
                let mut n = Namco51::new();
                n.load_state(r)?;
                *self = Self::Hle(n);
                Ok(())
            }
            NAMCO51_MODE_LLE => match self {
                Self::Lle(n) => n.load_state(r),
                _ => Err(SaveError::InvalidFormat(
                    "51XX LLE save state but no ROM loaded".to_string(),
                )),
            },
            mode => Err(SaveError::InvalidFormat(format!(
                "unknown 51XX mode: {mode}"
            ))),
        }
    }
}

use phosphor_core::core::debug::{DebugRegister, Debuggable};

impl Debuggable for Namco51Wrapper {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        match self {
            Self::Hle(n) => n.debug_registers(),
            Self::Lle(n) => n.debug_registers(),
        }
    }
}

// ---------------------------------------------------------------------------
// NamcoGalagaBoard — shared hardware for the Galaga platform
// ---------------------------------------------------------------------------

/// The three Z80s that share the Galaga bus.
///
/// They live outside [`NamcoGalagaBoard`] — and outside the game wrapper's bus
/// state — so `cpu.execute_cycle(&mut bus, ..)` is a pair of disjoint field
/// borrows and dispatches at a concrete bus type.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct GalagaCpus {
    #[save(id = 1)]
    pub main: Z80,
    #[save(id = 2)]
    pub sub: Z80,
    #[save(id = 3)]
    pub sound: Z80,
}

impl Default for GalagaCpus {
    fn default() -> Self {
        Self::new()
    }
}

impl GalagaCpus {
    pub fn new() -> Self {
        Self {
            main: Z80::new(),
            sub: Z80::new(),
            sound: Z80::new(),
        }
    }

    /// Instruction-boundary mask (bit 0 = main, 1 = sub, 2 = sound) for the
    /// debugger's instruction-granularity stepping. CPUs held in reset do not
    /// count.
    pub fn instruction_boundaries(&self, sub_running: bool) -> u32 {
        let mut mask = 0u32;
        if self.main.at_instruction_boundary() {
            mask |= 1;
        }
        if sub_running && self.sub.at_instruction_boundary() {
            mask |= 2;
        }
        if sub_running && self.sound.at_instruction_boundary() {
            mask |= 4;
        }
        mask
    }
}

/// A Galaga-family bus: the shared board plus whatever the game puts in front
/// of it (video latches, an EAROM, background-map lookup ROMs).
///
/// [`tick`] is generic over this trait, so every access the Z80s make resolves
/// to a direct call rather than a vtable entry.
pub trait NamcoGalagaBus: Bus<Address = u16, Data = u8> {
    fn board(&mut self) -> &mut NamcoGalagaBoard;
}

/// What the board decided at the top of a cycle, handed back so the caller can
/// step the CPUs without holding a borrow on the board.
pub struct CycleGate {
    /// Debug attribution is active (watchpoints set or tracing enabled).
    debug: bool,
}

/// Run `cycles` CPU cycles, taking the scanline-outer path for whole scanlines
/// and the per-cycle path for any partial scanline at either end — which only
/// arises when the debugger has left the clock off-boundary.
pub fn run_cycles<B: NamcoGalagaBus>(cpus: &mut GalagaCpus, bus: &mut B, cycles: u64) {
    let scanline = TIMING.cycles_per_scanline;
    let mut remaining = cycles;

    let lead = ((scanline - bus.board().clock % scanline) % scanline).min(remaining);
    for _ in 0..lead {
        tick(cpus, bus);
    }
    remaining -= lead;

    let whole = remaining - remaining % scanline;
    run_scanlines(cpus, bus, whole);
    remaining -= whole;

    for _ in 0..remaining {
        tick(cpus, bus);
    }
}

/// Run `cycles` CPU cycles, scanline-outer and cycle-inner.
///
/// The scanline-boundary work — VBLANK, the 51XX TC pin, the sound CPU's
/// scanline-timer NMI — happens 264 times a frame instead of on each of the
/// 50,688 cycles, which on this board is a ladder that was being evaluated
/// three times per cycle's worth of CPU work. The caller must start on a
/// scanline boundary and pass a multiple of `cycles_per_scanline`; the
/// debugger's off-boundary stepping goes through [`tick`] instead.
pub fn run_scanlines<B: NamcoGalagaBus>(cpus: &mut GalagaCpus, bus: &mut B, cycles: u64) {
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
            let gate = bus.board().begin_cycle_inner(cpus);
            step_cpus(cpus, bus, gate);
        }
    }
}

/// One CPU cycle of a Galaga-family machine: board work, the three Z80s, then
/// the custom-MCU servicing and clock advance.
///
/// This is the debugger's path — it tests the frame position on every cycle.
/// Whole scanlines go through [`run_scanlines`], which hoists that test out.
#[inline]
pub fn tick<B: NamcoGalagaBus>(cpus: &mut GalagaCpus, bus: &mut B) {
    let gate = bus.board().begin_cycle(cpus);
    step_cpus(cpus, bus, gate);
}

/// A game on this board that draws its picture one row at a time.
///
/// The three games differ in their layers and in the bus view they hand the
/// CPUs, but not in how the beam drives them. Before this trait, the clock test,
/// the debugger's per-cycle path and the scanline-outer frame loop were
/// duplicated in `galaga.rs`, `digdug.rs` and `xevious.rs` identically, down to
/// the comments. Those three are the provided methods here; a game supplies only
/// what is genuinely its own.
///
/// The renderer lives on the game wrapper while the clock and the tick loop live
/// on the board, and [`run_cycles`] is generic over the *bus view* rather than
/// the wrapper, so the board cannot reach a game's renderer directly. That is
/// what [`Self::Bus`] and [`Self::split`] are for.
pub(crate) trait ScanlineGame {
    /// The bus view this game hands the CPUs.
    ///
    /// Different per game because the register sets differ: Galaga's starfield
    /// latch, Dig Dug's EAROM and background selector, Xevious's four scroll
    /// registers.
    type Bus<'a>: NamcoGalagaBus
    where
        Self: 'a;

    /// Borrow the CPUs and the bus they drive as two disjoint pieces, so a
    /// cycle dispatches at a concrete type.
    fn split(&mut self) -> (&mut GalagaCpus, Self::Bus<'_>);

    /// The shared board, for its clock.
    fn board(&self) -> &NamcoGalagaBoard;

    /// Draw one visible row out of the video state as it stands at this moment.
    ///
    /// `y` is `0..224`. This is the only part of the drive a game writes.
    fn render_scanline(&mut self, y: usize);

    /// Draw the row about to be scanned, if the clock is on the boundary of a
    /// visible one.
    ///
    /// The off-boundary case only arises after the debugger has single-stepped
    /// the clock out of phase; the row is then drawn at the next boundary it
    /// crosses, as the beam would.
    fn begin_scanline_render(&mut self) {
        let per_line = TIMING.cycles_per_scanline;
        let clock = self.board().clock;
        if !clock.is_multiple_of(per_line) {
            return;
        }
        let scanline = clock % TIMING.cycles_per_frame() / per_line;
        if scanline < VISIBLE_LINES {
            self.render_scanline(scanline as usize);
        }
    }

    /// Advance one CPU cycle, drawing the row the beam is about to paint
    /// whenever that cycle starts a visible scanline.
    ///
    /// This is the debugger's path: it tests the frame position on every cycle
    /// so that single-stepping still crosses scanline boundaries. A whole frame
    /// goes through [`Self::run_frame_scanline_outer`], which hoists that test
    /// out.
    fn tick_frame_boundary(&mut self) {
        self.begin_scanline_render();
        let (cpus, mut bus) = self.split();
        tick(cpus, &mut bus);
    }

    /// Run one frame, scanline-outer: draw the row the beam is about to paint,
    /// then run that row's worth of cycles.
    ///
    /// The CPU/bus split is re-formed once per scanline rather than once per
    /// frame, which is 264 times instead of one; a per-*cycle* split cost about
    /// 6% on this board when it was measured, and this is 1/192nd of that
    /// frequency.
    fn run_frame_scanline_outer(&mut self) {
        let per_line = TIMING.cycles_per_scanline;
        let mut remaining = TIMING.cycles_per_frame();
        while remaining > 0 {
            self.begin_scanline_render();
            // A partial leading scanline only arises when the debugger has left
            // the clock off-phase; it runs up to the next boundary and the row
            // is drawn there.
            let run = (per_line - self.board().clock % per_line).min(remaining);
            {
                let (cpus, mut bus) = self.split();
                run_cycles(cpus, &mut bus, run);
            }
            remaining -= run;
        }
    }
}

/// The three CPUs' half of a cycle, plus the post-CPU board work.
#[inline]
fn step_cpus<B: NamcoGalagaBus>(cpus: &mut GalagaCpus, bus: &mut B, gate: CycleGate) {
    // The map has one PC latch but three CPUs drive this bus, so hand it the
    // about-to-run CPU's PC immediately before that CPU steps; every access it
    // then makes is attributed to its own instruction.
    if gate.debug {
        bus.board().latch_pc(0);
    }
    cpus.main.execute_cycle(bus, BusMaster::Cpu(0));

    // Read the reset latch *after* the main CPU's cycle: its write to the misc
    // latch takes effect immediately, and hardware holds the other two CPUs
    // from that moment.
    if !bus.board().sub_reset {
        if gate.debug {
            bus.board().latch_pc(1);
        }
        cpus.sub.execute_cycle(bus, BusMaster::Cpu(1));
        if gate.debug {
            bus.board().latch_pc(2);
        }
        cpus.sound.execute_cycle(bus, BusMaster::Cpu(2));
    }

    bus.board().end_cycle();
}

/// Namco Galaga hardware base (3×Z80 @ 3.072 MHz, Namco WSG, custom I/O chips).
///
/// Shared by Galaga, Dig Dug, Bosconian, and other games on the same PCB.
/// Game wrappers compose this struct, own their RAM arrays, and implement
/// Bus to route memory accesses. The CPUs themselves live in [`GalagaCpus`],
/// beside the bus rather than inside it.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct NamcoGalagaBoard {
    /// Address space owning the three program ROMs and all debug observability
    /// (watchpoints, the write-event trace, and the access-context latch).
    ///
    /// It persists its own writable regions, which is the rule this board used
    /// to apply by hand: `saved_region_ids` filtered `regions()` on
    /// `AccessKind::ReadWrite` and wrote each in turn.
    #[save(id = 1)]
    pub(crate) map: AddressSpace16,

    // Devices
    #[save(id = 2)]
    pub(crate) wsg: NamcoWsg,
    #[save(id = 3)]
    pub(crate) namco06: Namco06,
    #[save(id = 4)]
    pub(crate) namco51: Namco51Wrapper,
    #[save(id = 5)]
    pub(crate) namco53: Namco53,

    /// Optional score/protection MCU (06XX chip-select 2). Present only on the
    /// boards that fit it (e.g. Xevious, Bosconian); `None` on Galaga/Dig Dug.
    ///
    /// An `Option` field is on the wire exactly when it is fitted, which is
    /// what the hand-written impl's presence flag did.
    #[save(id = 6)]
    pub(crate) namco50: Option<Namco50>,

    // Clock divider for the 51XX MCU (LLE mode only). MB88xx runs at 256 kHz.
    /// The board's clock tree, as [`clock_tree`] declares it.
    ///
    /// The game wrappers hand-write `BusDebug::devices`, so unlike the boards
    /// that derive it this one is listed there rather than by attribute.
    #[save(id = 7)]
    pub(crate) clocks: ClockTree,
    /// A handle into the clock tree, which is itself saved.
    #[save_skip]
    pub(crate) namco51_dom: DomainId,

    // Input ports (active-low: 0xFF = all released)
    #[save(id = 8)]
    pub(crate) in0: u8,
    #[save(id = 9)]
    pub(crate) in1: u8,
    #[save(id = 10)]
    pub(crate) dswa: u8,
    #[save(id = 11)]
    pub(crate) dswb: u8,

    // LS259 misc latch outputs
    #[save(id = 12)]
    pub(crate) main_irq_enabled: bool, // Q0
    #[save(id = 13)]
    pub(crate) sub_irq_enabled: bool, // Q1
    #[save(id = 14)]
    pub(crate) sound_nmi_enabled: bool, // Q2 (inverted!)
    #[save(id = 15)]
    pub(crate) sub_reset: bool, // Q3 (true = sub/sound held in reset)

    // Interrupt state
    #[save(id = 16)]
    pub(crate) main_irq_pending: bool,
    #[save(id = 17)]
    pub(crate) main_nmi_pending: bool, // from 06XX timer
    #[save(id = 18)]
    pub(crate) sub_irq_pending: bool,
    #[save(id = 19)]
    pub(crate) sound_nmi_pending: bool, // from scanline timer (64/192), gated by Q2

    /// The colour PROM and the palette expanded from it. Derived from ROM
    /// rather than from anything the CPU writes, so it is rebuilt at ROM load
    /// and stays out of the save.
    #[save_skip]
    pub(crate) palette_prom: [u8; 32],
    #[save_skip]
    pub(crate) palette_rgb: [(u8, u8, u8); 32],

    // Timing
    #[save(id = 20)]
    pub(crate) clock: u64,
    #[save(id = 21)]
    pub(crate) watchdog_counter: u32,
    #[save(id = 22)]
    pub(crate) flip_screen: bool,

    /// Deferred sub CPU reset (set by write_misc_latch, acted on in tick).
    /// Describes a hand-off inside one tick, which a save is never taken part
    /// way through, so a load starts it clear.
    #[save_skip(default)]
    pending_sub_cpu_reset: bool,

    // Debug observability (observer state — never saved in save states).
    /// Instruction PC per CPU (main/sub/sound), latched at instruction
    /// boundaries each tick for hit/event attribution. The map has a single
    /// PC latch, but three CPUs share this bus, so the board remembers all
    /// three and feeds the map the relevant one before each CPU runs.
    #[save_skip]
    pub(crate) debug_pc: [Option<u32>; 3],
}

impl NamcoGalagaBoard {
    pub fn new() -> Self {
        let clocks = clock_tree();
        let namco51_dom = clocks.find(Clk::Mcu).expect("declared Namco 51xx domain");
        Self {
            map: Self::build_map(),

            wsg: {
                let mut wsg = NamcoWsg::new(TIMING.cpu_clock_hz);
                // Galaga-family hardware has no sound-enable latch; WSG is
                // always active (unlike Pac-Man which gates via 0x5003).
                wsg.set_sound_enabled(true);
                wsg
            },
            namco06: Namco06::new(NAMCO06_BASE_DIVISOR),
            namco51: Namco51Wrapper::Hle(Namco51::new()),
            namco53: Namco53::new(),
            namco50: None,

            clocks,
            namco51_dom,

            in0: 0xFF,
            in1: 0xFF,
            // DIP switch defaults matching MAME/factory settings:
            // DSWA: 3 lives (0x80), bonus 20K/60K (0x18), coin B 1C/1C (0x01)
            // DSWB: freeze off (0x20), cabinet upright (0x04)
            dswa: 0x99,
            dswb: 0x24,

            main_irq_enabled: false,
            sub_irq_enabled: false,
            sound_nmi_enabled: false,
            sub_reset: true, // sub+sound held in reset at power-on

            main_irq_pending: false,
            main_nmi_pending: false,
            sub_irq_pending: false,
            sound_nmi_pending: false,

            palette_prom: [0; 32],
            palette_rgb: [(0, 0, 0); 32],

            clock: 0,
            watchdog_counter: 0,
            flip_screen: false,

            pending_sub_cpu_reset: false,

            debug_pc: [None; 3],
        }
    }

    /// Build the board's address space.
    ///
    /// The three ROMs are declared as *backing* regions with no page mapping:
    /// 0x0000-0x3FFF decodes to a different ROM depending on which CPU is the
    /// bus master, and a single page table cannot map three regions over the
    /// same addresses. The regions still give the debugger named, enumerable
    /// ROM storage, and `read_rom` selects between them by bus master.
    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.backing_region(Region::MainRom, "Main CPU ROM", 0x4000)
            .backing_region(Region::SubRom, "Sub CPU ROM", 0x4000)
            .backing_region(Region::SoundRom, "Sound CPU ROM", 0x4000);
        // Backing allocates zeroed, but an unpopulated ROM socket floats high:
        // addresses past the loaded image must read 0xFF. The sub and sound
        // ROMs are typically only 0x1000 bytes and do NOT mirror, so most of
        // their 16 KB window is this fill.
        map.region_data_mut(Region::MainRom).fill(0xFF);
        map.region_data_mut(Region::SubRom).fill(0xFF);
        map.region_data_mut(Region::SoundRom).fill(0xFF);
        map
    }

    // -----------------------------------------------------------------------
    // Core tick — the board half of one CPU cycle (see [`tick`])
    // -----------------------------------------------------------------------

    /// Board work that happens before the CPUs' cycle: deferred resets,
    /// interrupt timing, the 06XX timer, the sound generator, and sampling
    /// debug attribution context.
    fn begin_cycle(&mut self, cpus: &mut GalagaCpus) -> CycleGate {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();
        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            self.begin_scanline(frame_cycle / TIMING.cycles_per_scanline);
        }
        self.begin_cycle_inner(cpus)
    }

    /// Work that only happens on the first cycle of a scanline: the VBLANK
    /// interrupts and the 51XX's TC pin, and the sound CPU's scanline-timer
    /// NMI. Every one of these fires on a scanline boundary, so none of it
    /// belongs in the per-cycle path.
    ///
    /// Called once per scanline from [`run_scanlines`] and, for the debugger's
    /// single-step path, from `begin_cycle` when the clock lands on a boundary.
    fn begin_scanline(&mut self, scanline: u64) {
        // VBLANK interrupt: fire at the start of VBLANK (scanline 224).
        // Only assert IRQ if the mask (enable latch) is set, matching MAME's
        // vblank_irq: `if (state && m_main_irq_mask) set_input_line(ASSERT_LINE)`.
        // This prevents a race where VBLANK fires while the IRQ handler has
        // temporarily cleared the mask, which would cause spurious re-entry.
        if scanline == VISIBLE_LINES {
            if self.main_irq_enabled {
                self.main_irq_pending = true;
                self.trace_interrupt(0, "VBLANK IRQ (main)");
            }
            if self.sub_irq_enabled {
                self.sub_irq_pending = true;
                self.trace_interrupt(1, "VBLANK IRQ (sub)");
            }
            // Drive VBLANK to 51XX TC pin (active on falling edge).
            // Matches MAME: vblank(state) → set_input_line(TC_LINE, !state)
            if let Namco51Wrapper::Lle(ref mut lle) = self.namco51 {
                lle.mcu.set_tc(false); // Assert (active low)
            }
        }
        // Clear TC at end of VBLANK (start of visible area = scanline 0)
        if scanline == 0
            && let Namco51Wrapper::Lle(ref mut lle) = self.namco51
        {
            lle.mcu.set_tc(true); // Deassert
        }

        // Sound CPU NMI: fires at scanlines 64 and 192 (every 128 lines),
        // matching MAME's cpu3_interrupt_callback. Gated by misc latch Q2.
        const SOUND_NMI_SCANLINE_A: u64 = 64;
        const SOUND_NMI_SCANLINE_B: u64 = 192;
        if (scanline == SOUND_NMI_SCANLINE_A || scanline == SOUND_NMI_SCANLINE_B)
            && self.sound_nmi_enabled
            && !self.sub_reset
        {
            self.sound_nmi_pending = true;
            self.trace_interrupt(2, "sound NMI (scanline timer)");
        }
    }

    /// Per-cycle board work, with no frame-position tests in it.
    fn begin_cycle_inner(&mut self, cpus: &mut GalagaCpus) -> CycleGate {
        // Handle deferred sub CPU reset (set by write_misc_latch bit 3).
        // Mirrors Z80::reset() without needing 'static bus lifetime.
        if self.pending_sub_cpu_reset {
            self.pending_sub_cpu_reset = false;
            cpus.sub.hardware_reset();
            cpus.sound.hardware_reset();
            // A CPU coming out of reset must start clean at 0x0000 with no
            // stale interrupt latched. Clearing these prevents a pending sound
            // NMI (accumulated while the CPU was held in reset) from firing
            // before the freshly reset CPU has set up its stack pointer — which
            // would otherwise push onto the reset SP (0xFFFF) and wreck it.
            self.sub_irq_pending = false;
            self.sound_nmi_pending = false;
        }

        // 06XX timer tick — NMI output is a level signal to the main CPU.
        //
        // Always propagate the NMI level regardless of Z80 HALT state.
        // MAME's set_nmi() checks for scheduler suspension (SUSPEND_REASON_HALT |
        // SUSPEND_REASON_RESET | SUSPEND_REASON_DISABLE), which are board-level
        // disable flags — NOT the Z80 HALT instruction. A HALTed Z80 must still
        // receive NMI (NMI wakes it from HALT). The main CPU is never board-
        // suspended in Dig Dug / Galaga.
        self.namco06.tick();
        self.main_nmi_pending = self.namco06.nmi_output();

        // WSG tick (runs at CPU clock rate)
        self.wsg.tick();

        // Sample debug attribution context (per-CPU instruction PCs) before
        // CPU execution — bus dispatch cannot read CPU state mid-tick.
        let debug = self.map.debug_active();
        if debug {
            if cpus.main.at_instruction_boundary() {
                self.debug_pc[0] = Some(cpus.main.pc as u32);
            }
            if cpus.sub.at_instruction_boundary() {
                self.debug_pc[1] = Some(cpus.sub.pc as u32);
            }
            if cpus.sound.at_instruction_boundary() {
                self.debug_pc[2] = Some(cpus.sound.pc as u32);
            }
        }

        CycleGate { debug }
    }

    /// Point the map's single access-context latch at CPU `index`, so the
    /// accesses that CPU is about to make are attributed to its instruction.
    #[inline]
    fn latch_pc(&mut self, index: usize) {
        self.map
            .latch_access_context(self.clock, self.debug_pc[index]);
    }

    /// Board work after the CPUs' cycle. The custom MCUs run here, after the
    /// Z80s, so their K inputs see this cycle's writes to the 06XX latch
    /// (K is a hardware wire, not a latch).
    fn end_cycle(&mut self) {
        // Drive chip_select IRQ to LLE 51XX and tick MCU.
        // Executed AFTER Z80 so K reflects latest data writes.
        // Matches MAME's nmi_generate which pulses chip_select for selected
        // chips on each timer toggle: `m_chipsel[N](0, BIT(ctrl, N) && timer_state)`.
        if let Namco51Wrapper::Lle(ref mut lle) = self.namco51 {
            let cs = self.namco06.chip_select_active(0);
            lle.mcu.set_irq(cs);
            // K port: in dynamic_k mode, INK computes K at execution time as
            // (rw_input << 3) | (o_latch & 0x07), matching MAME's K_r() callback.
            // We only need to keep rw_input current; o_latch updates instantly
            // when the Z80 writes via write_custom_io → namco51.write().
            lle.mcu.rw_input = if self.namco06.is_read_mode() { 1 } else { 0 };
            if self.clocks.tick(self.namco51_dom) {
                lle.update_inputs(self.in0, self.in1);
                lle.tick();
            }
        }

        // Drive the 50XX score/protection MCU (if fitted) the same way: assert
        // its chip-select IRQ and R/W line after the Z80s have run, then step
        // it on its own machine-cycle divider.
        // The 50XX HLE responds immediately via the 06XX read/write dispatch and
        // needs no per-cycle servicing.

        self.clock += 1;
        self.watchdog_counter += 1;
    }

    // -----------------------------------------------------------------------
    // Debug observability helpers
    // -----------------------------------------------------------------------

    /// Check a read against the map's watchpoints. The 3 CPUs share one bus,
    /// so the accessing CPU's index is what scopes the watch.
    #[inline]
    pub(crate) fn watch_read(&mut self, master: BusMaster, addr: u16, data: u8) {
        if let BusMaster::Cpu(i) = master {
            self.map.watch_read(i, master, addr, data);
        }
    }

    /// Check a write against the map's watchpoints and record its trace event.
    /// Call before applying the side effect (hits record
    /// `WatchpointPhase::Before`).
    ///
    /// Address decode below page granularity (the misc latch, the watchdog
    /// address) is game-specific, so the wrapper supplies the annotation.
    #[inline]
    pub(crate) fn watch_write_annotated(
        &mut self,
        master: BusMaster,
        addr: u16,
        data: u8,
        annotation: WriteAnnotation,
    ) {
        if let BusMaster::Cpu(i) = master {
            self.map
                .watch_write_annotated(i, master, addr, data, annotation);
        }
    }

    /// Record an interrupt assertion against `cpu_index` (gated internally).
    fn trace_interrupt(&mut self, cpu_index: usize, detail: &'static str) {
        self.map.trace_record(DebugEvent {
            cpu_index: Some(cpu_index),
            detail: Some(detail),
            // Interrupts are raised at the top of `tick`, before the map's
            // access context is latched for this cycle, so stamp them from the
            // board clock directly rather than the (still previous-cycle) latch.
            ..DebugEvent::new(
                self.clock,
                phosphor_core::core::watchpoint::DebugAccessSource::Unknown,
                DebugEventKind::InterruptAssert,
            )
        });
    }

    /// Record a custom-I/O (06XX-routed) transaction. Custom I/O lives on
    /// the main CPU bus, so events are attributed to CPU 0.
    fn trace_custom_io(
        &mut self,
        kind: DebugEventKind,
        addr: u16,
        value: u8,
        device: &'static str,
        detail: Option<&'static str>,
    ) {
        self.map.trace_record(DebugEvent {
            cpu_index: Some(0),
            pc: self.debug_pc[0],
            addr: Some(addr as u32),
            value: Some(value as u32),
            width: 1,
            device: Some(device),
            detail,
            ..DebugEvent::new(
                self.map.debug_cycle(),
                phosphor_core::core::watchpoint::DebugAccessSource::Cpu(0),
                kind,
            )
        });
    }

    // -----------------------------------------------------------------------
    // Bus dispatch helpers — called from game wrapper Bus impls
    // -----------------------------------------------------------------------

    /// Read ROM for the requesting CPU. Each CPU sees a different ROM at the
    /// same addresses, so the bus master picks the region.
    pub fn read_rom(&self, master: BusMaster, addr: u16) -> u8 {
        let region = match master {
            BusMaster::Cpu(0) => Region::MainRom,
            BusMaster::Cpu(1) => Region::SubRom,
            BusMaster::Cpu(2) => Region::SoundRom,
            _ => return 0xFF,
        };
        // Past the region (addresses above 0x3FFF) the socket is not selected
        // at all; unloaded space inside it already reads 0xFF from the fill.
        self.map
            .region_data(region)
            .get(addr as usize)
            .copied()
            .unwrap_or(0xFF)
    }

    /// Read the 06XX custom I/O data port. Dispatches to the selected chip
    /// based on the 06XX control register chip-select bits.
    ///
    /// Per MAME: reading in write mode returns 0 and does NOT trigger the
    /// custom chip, preventing spurious read_index advances.
    pub fn read_custom_io(&mut self) -> u8 {
        if !self.namco06.is_read_mode() {
            return 0;
        }
        let chip = if self.namco06.chip_select(0) {
            0
        } else if self.namco06.chip_select(1) {
            1
        } else if self.namco06.chip_select(2) {
            2
        } else {
            0xFF
        };
        let data = match chip {
            0 => self.namco51.read(self.in0, self.in1),
            1 => self.namco53.read(self.dswa, self.dswb),
            2 => self.namco50.as_mut().map_or(0xFF, Namco50::read),
            _ => 0xFF,
        };
        let device = match chip {
            0 => "Namco 51XX",
            1 => "Namco 53XX",
            2 => "Namco 50XX",
            _ => "Namco 06XX",
        };
        self.trace_custom_io(DebugEventKind::DeviceRead, 0x7000, data, device, None);
        data
    }

    /// Write the 06XX custom I/O data port. Dispatches to the selected chip.
    ///
    /// Per MAME: writing in read mode is ignored and does NOT trigger the
    /// custom chip.
    pub fn write_custom_io(&mut self, data: u8) {
        if self.namco06.is_read_mode() {
            return;
        }
        if self.namco06.chip_select(0) {
            self.trace_custom_io(
                DebugEventKind::DeviceWrite,
                0x7000,
                data,
                "Namco 51XX",
                Some("command/argument"),
            );
            self.namco51.write(data);
        }
        // 53XX has no write interface
        if self.namco06.chip_select(2) && self.namco50.is_some() {
            self.trace_custom_io(
                DebugEventKind::DeviceWrite,
                0x7000,
                data,
                "Namco 50XX",
                Some("command/argument"),
            );
            if let Some(ref mut n50) = self.namco50 {
                n50.write(data);
            }
        }
        // Chip-select 3 (54XX explosion-sound MCU) is write-only; its discrete
        // audio network is not yet modelled, so its commands are discarded
        // (explosions are silent for now).
    }

    /// Write the 06XX control register.
    ///
    /// The custom chip MCUs (51XX, 53XX) maintain continuous read_index state
    /// across transactions. The 53XX cycles through 2 reads (DSWA, DSWB).
    /// Do NOT reset read indices here.
    pub fn write_custom_io_ctrl(&mut self, data: u8) {
        self.trace_custom_io(
            DebugEventKind::DeviceWrite,
            0x7100,
            data,
            "Namco 06XX",
            Some("control (mode + chip select + timer)"),
        );
        self.namco06.ctrl_write(data, self.clock);
    }

    /// Write the LS259 misc latch at 0x6820-0x6827.
    /// `bit` is address & 7, `value` is data bit 0.
    pub fn write_misc_latch(&mut self, bit: u8, value: bool) {
        match bit {
            0 => {
                self.main_irq_enabled = value;
                if !value {
                    self.main_irq_pending = false;
                }
            }
            1 => {
                self.sub_irq_enabled = value;
                if !value {
                    self.sub_irq_pending = false;
                }
            }
            2 => {
                // Sound NMI enable is INVERTED: writing 0 enables NMI
                self.sound_nmi_enabled = !value;
            }
            3 => {
                // Sub/sound CPU reset: 0 = held in reset, 1 = running
                let was_reset = self.sub_reset;
                self.sub_reset = !value;

                // When releasing from reset, defer CPU reset to tick()
                // where bus access is available.
                if was_reset && !self.sub_reset {
                    self.pending_sub_cpu_reset = true;
                }

                // Reset the custom I/O MCUs when entering reset (Q3=0): the
                // latch's reset output is wired to the 51XX/53XX and, where
                // fitted, the 50XX.
                if !value {
                    self.namco51.reset();
                    self.namco53.reset();
                    if let Some(ref mut n50) = self.namco50 {
                        n50.reset();
                    }
                }
            }
            7 => {
                self.flip_screen = value;
            }
            _ => {} // 4-6: game-specific (mod_bits, LEDs, etc.)
        }
    }

    /// Check interrupt state for a given CPU.
    pub fn check_interrupts(
        &mut self,
        target: BusMaster,
    ) -> phosphor_core::core::bus::InterruptState {
        use phosphor_core::core::bus::InterruptState;
        match target {
            BusMaster::Cpu(0) => {
                // IRQ is level-triggered: stays asserted until the game
                // explicitly clears it by writing 0 to the IRQ enable latch
                // (0x6820). Matches MAME's ASSERT_LINE / CLEAR_LINE semantics.
                // Do NOT clear main_irq_pending here — only write_misc_latch
                // bit 0 clears it (via irq1_clear_w equivalent).
                let irq = self.main_irq_pending && self.main_irq_enabled;

                // NMI is a level signal driven by the 06XX timer. The Z80's
                // internal rising-edge detector converts this level into
                // discrete NMI events. Do NOT consume here — the level
                // persists until the 06XX timer's CLEAR phase drives it low.
                let nmi = self.main_nmi_pending;
                InterruptState {
                    irq,
                    nmi,
                    ..Default::default()
                }
            }
            BusMaster::Cpu(1) => {
                // IRQ is level-triggered (same as CPU 0).
                let irq = self.sub_irq_pending && self.sub_irq_enabled;
                InterruptState {
                    irq,
                    ..Default::default()
                }
            }
            BusMaster::Cpu(2) => {
                let nmi = self.sound_nmi_pending;
                if nmi {
                    self.sound_nmi_pending = false;
                }
                InterruptState {
                    nmi,
                    ..Default::default()
                }
            }
            _ => InterruptState::default(),
        }
    }

    /// Check if a CPU is halted (sub+sound halted when sub_reset is true).
    pub fn is_halted_for(&self, master: BusMaster) -> bool {
        match master {
            BusMaster::Cpu(1) | BusMaster::Cpu(2) => self.sub_reset,
            _ => false,
        }
    }

    // -----------------------------------------------------------------------
    // Palette
    // -----------------------------------------------------------------------

    /// Pre-compute the 32-entry RGB palette from the palette PROM using
    /// resistor-weighted DAC values (same resistor network as Pac-Man).
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

    /// Copy a ROM image to offset 0 of `region`, truncating anything past the
    /// 16 KB window the CPU can address rather than rejecting the load.
    fn load_rom_region(&mut self, region: Region, data: &[u8]) {
        let dest = self.map.region_data_mut(region);
        let len = data.len().min(dest.len());
        dest[..len].copy_from_slice(&data[..len]);
    }

    pub fn load_main_rom(&mut self, data: &[u8]) {
        self.load_rom_region(Region::MainRom, data);
    }

    pub fn load_sub_rom(&mut self, data: &[u8]) {
        self.load_rom_region(Region::SubRom, data);
    }

    pub fn load_sound_rom(&mut self, data: &[u8]) {
        self.load_rom_region(Region::SoundRom, data);
    }

    pub fn load_palette_prom(&mut self, data: &[u8]) {
        let len = data.len().min(32);
        self.palette_prom[..len].copy_from_slice(&data[..len]);
        self.build_palette();
    }

    pub fn load_sound_prom(&mut self, data: &[u8]) {
        self.wsg.load_waveform_rom(data);
    }

    /// Load the Namco 51XX MCU firmware ROM, switching from HLE to LLE mode.
    /// If not called, the board uses the behavioral HLE model (no ROM required).
    pub fn load_51xx_rom(&mut self, data: &[u8]) {
        let mut lle = Namco51Lle::new();
        lle.load_rom(data);
        // Enable dynamic K port: INK reads K = (rw_input << 3) | (o_latch & 0x07)
        // at execution time, matching MAME's K_r() callback. This ensures the MCU
        // sees the latest Z80 writes to o_latch even when the write happens only
        // a few Z80 cycles before the MCU's INK instruction.
        lle.mcu.dynamic_k = true;
        self.namco51 = Namco51Wrapper::Lle(lle);
    }

    /// Fit the Namco 50XX score/protection chip (06XX chip-select 2). Only
    /// boards that carry it (e.g. Xevious) call this; otherwise chip-select 2
    /// reads back the idle bus.
    pub fn fit_50xx(&mut self) {
        self.namco50 = Some(Namco50::new());
    }

    /// Enable the Xevious 51XX coinage quirk (command 01 consumes 6 arguments
    /// instead of 4). Without it the HLE 51XX swallows Xevious's trailing
    /// "enter credit mode" command and never leaves switch mode.
    pub fn enable_xevious_51xx_kludge(&mut self) {
        self.namco51.set_xevious_coinage_kludge(true);
    }

    // -----------------------------------------------------------------------
    // Input handling
    // -----------------------------------------------------------------------

    /// Dispatch an input event to the appropriate port bit (active-low).
    pub fn handle_input(&mut self, button: u8, pressed: bool) {
        match button {
            INPUT_P1_UP => crate::set_bit_active_low(&mut self.in0, 0, pressed),
            INPUT_P1_RIGHT => crate::set_bit_active_low(&mut self.in0, 1, pressed),
            INPUT_P1_DOWN => crate::set_bit_active_low(&mut self.in0, 2, pressed),
            INPUT_P1_LEFT => crate::set_bit_active_low(&mut self.in0, 3, pressed),
            INPUT_P2_UP => crate::set_bit_active_low(&mut self.in0, 4, pressed),
            INPUT_P2_RIGHT => crate::set_bit_active_low(&mut self.in0, 5, pressed),
            INPUT_P2_DOWN => crate::set_bit_active_low(&mut self.in0, 6, pressed),
            INPUT_P2_LEFT => crate::set_bit_active_low(&mut self.in0, 7, pressed),
            INPUT_P1_BUTTON1 => crate::set_bit_active_low(&mut self.in1, 0, pressed),
            INPUT_P2_BUTTON1 => crate::set_bit_active_low(&mut self.in1, 1, pressed),
            INPUT_START1 => crate::set_bit_active_low(&mut self.in1, 2, pressed),
            INPUT_START2 => crate::set_bit_active_low(&mut self.in1, 3, pressed),
            INPUT_COIN1 => crate::set_bit_active_low(&mut self.in1, 4, pressed),
            INPUT_COIN2 => crate::set_bit_active_low(&mut self.in1, 5, pressed),
            INPUT_SERVICE => crate::set_bit_active_low(&mut self.in1, 6, pressed),
            _ => {}
        }
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

    /// Reset all board state except ROMs and palette PROMs. The machine owns
    /// the CPUs and resets them against this board.
    pub fn reset_board(&mut self) {
        self.wsg.reset();
        // Galaga-family hardware has no sound-enable latch; WSG is always
        // active. Re-enable after reset (which clears the flag).
        self.wsg.set_sound_enabled(true);
        self.namco06.reset();
        self.namco51.reset();
        self.namco53.reset();
        if let Some(ref mut n50) = self.namco50 {
            n50.reset();
        }
        self.clocks.reset();

        self.in0 = 0xFF;
        self.in1 = 0xFF;

        self.main_irq_enabled = false;
        self.sub_irq_enabled = false;
        self.sound_nmi_enabled = false;
        self.sub_reset = true;

        self.main_irq_pending = false;
        self.main_nmi_pending = false;
        self.sub_irq_pending = false;
        self.sound_nmi_pending = false;

        self.clock = 0;
        self.watchdog_counter = 0;
        self.flip_screen = false;

        self.pending_sub_cpu_reset = false;
    }

    // -----------------------------------------------------------------------
    // Debug
    // -----------------------------------------------------------------------

    /// Whether the sub and sound CPUs are running (not held in reset).
    pub fn sub_running(&self) -> bool {
        !self.sub_reset
    }
}

impl Default for NamcoGalagaBoard {
    fn default() -> Self {
        Self::new()
    }
}

// All board events — bus writes, interrupts, custom-I/O transactions — land in
// the map's ring, so the trace capability is just the map's.
crate::impl_map_debug_trace!(NamcoGalagaBoard, map);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::watchpoint::{DebugAccessSource, WatchpointKind, WatchpointPhase};

    // Coin slot 2 must not default to the coin-1 key, or one press awards two
    // credits (regression: the legacy defaults bound every coin to Num5).
    #[test]
    fn coin_keys_do_not_double_credit() {
        crate::assert_no_coin_binding_collision(NAMCO_GALAGA_CONTROLS);
    }

    /// Stand in for the per-CPU latching `tick()` does before stepping CPU
    /// `cpu`, so a bare helper call is attributed like a real bus access.
    fn latch_for(board: &mut NamcoGalagaBoard, cpu: usize) {
        let (clock, pc) = (board.clock, board.debug_pc[cpu]);
        board.map.latch_access_context(clock, pc);
    }

    mod watchpoints {
        use super::*;

        #[test]
        fn write_watch_fires_with_cpu_and_context_attribution() {
            let mut board = NamcoGalagaBoard::new();
            board.clock = 1234;
            board.debug_pc = [Some(0x0100), Some(0x0200), None];
            board.map.set_watchpoint(1, 0x8800, WatchpointKind::Write);

            // Main CPU access: watch is scoped to the sub CPU → no hit.
            latch_for(&mut board, 0);
            board.watch_write_annotated(BusMaster::Cpu(0), 0x8800, 0x11, WriteAnnotation::MEMORY);
            assert!(board.map.take_hit().is_none());

            // Sub CPU access fires with sub-CPU PC attribution.
            latch_for(&mut board, 1);
            board.watch_write_annotated(BusMaster::Cpu(1), 0x8800, 0x22, WriteAnnotation::MEMORY);
            let hit = board.map.take_hit().unwrap();
            assert_eq!(hit.cpu_index, 1);
            assert_eq!(hit.source, DebugAccessSource::Cpu(1));
            assert_eq!(hit.cycle, 1234);
            assert_eq!(hit.pc, Some(0x0200));
            assert_eq!(hit.value, 0x22);
            assert_eq!(hit.phase, WatchpointPhase::Before);
        }

        #[test]
        fn read_watch_fires_after_value_known() {
            let mut board = NamcoGalagaBoard::new();
            board.map.set_watchpoint(0, 0x9000, WatchpointKind::Read);

            board.watch_read(BusMaster::Cpu(0), 0x9000, 0xAB);
            let hit = board.map.take_hit().unwrap();
            assert_eq!(hit.kind, WatchpointKind::Read);
            assert_eq!(hit.phase, WatchpointPhase::After);
            assert_eq!(hit.value, 0xAB);
        }
    }

    mod debug_events {
        use super::*;
        // The write-annotation table is per-game (address decode differs);
        // exercise the board through Galaga's.
        use crate::galaga::write_annotation;

        #[test]
        fn tracing_disabled_records_nothing() {
            let mut board = NamcoGalagaBoard::new();
            board.watch_write_annotated(BusMaster::Cpu(0), 0x6800, 0x01, write_annotation(0x6800));
            board.write_custom_io_ctrl(0xA1);
            assert!(board.map.trace_events().is_empty());
        }

        #[test]
        fn custom_io_transactions_attribute_chips() {
            let mut board = NamcoGalagaBoard::new();
            board.map.set_trace_enabled(true);
            board.debug_pc[0] = Some(0x1BCC);

            // 06XX control: write mode, chip select 0 (51XX)
            board.write_custom_io_ctrl(0x01);
            // Data write routed to the 51XX
            board.write_custom_io(0xA8);

            let events = board.map.trace_events();
            assert_eq!(events.len(), 2);
            assert_eq!(events[0].kind, DebugEventKind::DeviceWrite);
            assert_eq!(events[0].device, Some("Namco 06XX"));
            assert_eq!(events[0].addr, Some(0x7100));
            assert_eq!(events[1].device, Some("Namco 51XX"));
            assert_eq!(events[1].addr, Some(0x7000));
            assert_eq!(events[1].value, Some(0xA8));
            assert_eq!(events[1].pc, Some(0x1BCC));
        }

        #[test]
        fn bus_writes_map_to_kinds_with_multi_cpu_attribution() {
            let mut board = NamcoGalagaBoard::new();
            board.map.set_trace_enabled(true);
            board.debug_pc = [Some(0x0100), Some(0x0200), Some(0x0300)];

            let write = |board: &mut NamcoGalagaBoard, cpu: usize, addr: u16, data: u8| {
                latch_for(board, cpu);
                board.watch_write_annotated(
                    BusMaster::Cpu(cpu),
                    addr,
                    data,
                    write_annotation(addr),
                );
            };
            write(&mut board, 0, 0x6805, 0x07); // WSG
            write(&mut board, 0, 0x6823, 0x01); // misc latch bit 3
            write(&mut board, 0, 0x6830, 0x00); // watchdog
            write(&mut board, 1, 0x8900, 0x42); // memory, sub CPU
            write(&mut board, 0, 0x7100, 0x01); // custom io: suppressed here

            let events = board.map.trace_events();
            assert_eq!(events.len(), 4);
            assert_eq!(events[0].device, Some("Namco WSG"));
            assert_eq!(events[1].detail, Some("sub/sound reset"));
            assert_eq!(events[2].kind, DebugEventKind::Watchdog);
            assert_eq!(events[3].kind, DebugEventKind::MemoryWrite);
            assert_eq!(events[3].cpu_index, Some(1));
            assert_eq!(events[3].pc, Some(0x0200));
        }
    }
}
