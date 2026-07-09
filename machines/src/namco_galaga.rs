use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind, DebugTraceBuffer};
use phosphor_core::core::machine::{
    ActionRole, DefaultBinding, Direction, InputControl, InputId, InputKind, KeyId, TimingConfig,
};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::watchpoint::Watchpoints;
use phosphor_core::core::{Bus, BusMaster, ClockDivider};
use phosphor_core::cpu::z80::Z80;
use phosphor_core::device::namco_wsg::NamcoWsg;
use phosphor_core::device::namco06::Namco06;
use phosphor_core::device::namco50::Namco50;
use phosphor_core::device::namco51::Namco51;
use phosphor_core::device::namco51_lle::Namco51Lle;
use phosphor_core::device::namco53::Namco53;
use phosphor_core::gfx::decode::GfxLayout;
use phosphor_macros::DebugTrace;

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
        // Legacy bound every "Coin*" name to the 5 key; only the first also
        // got gamepad Back (handled by Coin 1 above).
        default_bindings: &[DefaultBinding::Key(KeyId::Num5)],
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

/// Namco Galaga hardware base (3×Z80 @ 3.072 MHz, Namco WSG, custom I/O chips).
///
/// Shared by Galaga, Dig Dug, Bosconian, and other games on the same PCB.
/// Game wrappers compose this struct, own their RAM arrays, and implement
/// Bus to route memory accesses.
#[derive(DebugTrace)]
pub struct NamcoGalagaBoard {
    // CPUs
    pub(crate) main_cpu: Z80,
    pub(crate) sub_cpu: Z80,
    pub(crate) sound_cpu: Z80,

    // Per-CPU ROM (each CPU sees different ROM at 0x0000-0x3FFF)
    pub(crate) main_rom: Vec<u8>,
    pub(crate) sub_rom: Vec<u8>,
    pub(crate) sound_rom: Vec<u8>,

    // Devices
    pub(crate) wsg: NamcoWsg,
    pub(crate) namco06: Namco06,
    pub(crate) namco51: Namco51Wrapper,
    pub(crate) namco53: Namco53,

    // Optional score/protection MCU (06XX chip-select 2). Present only on the
    // boards that fit it (e.g. Xevious, Bosconian); `None` on Galaga/Dig Dug.
    pub(crate) namco50: Option<Namco50>,

    // Clock dividers for the LLE MCUs (LLE mode only). The MB88xx machine-cycle
    // rate is 256 kHz = the Z80 clock / 12 (external 1.536 MHz internally
    // divided by 6). The 50XX's command/response handshake with the 06XX is
    // timing-sensitive and needs this exact rate; the 51XX (inputs only, level
    // signals) currently runs faster at /2 and tolerates it.
    pub(crate) namco51_divider: ClockDivider,
    pub(crate) namco50_divider: ClockDivider,

    // Input ports (active-low: 0xFF = all released)
    pub(crate) in0: u8,
    pub(crate) in1: u8,
    pub(crate) dswa: u8,
    pub(crate) dswb: u8,

    // LS259 misc latch outputs
    pub(crate) main_irq_enabled: bool,  // Q0
    pub(crate) sub_irq_enabled: bool,   // Q1
    pub(crate) sound_nmi_enabled: bool, // Q2 (inverted!)
    pub(crate) sub_reset: bool,         // Q3 (true = sub/sound held in reset)

    // Interrupt state
    pub(crate) main_irq_pending: bool,
    pub(crate) main_nmi_pending: bool, // from 06XX timer
    pub(crate) sub_irq_pending: bool,
    pub(crate) sound_nmi_pending: bool, // from scanline timer (64/192), gated by Q2

    // Palette
    pub(crate) palette_prom: [u8; 32],
    pub(crate) palette_rgb: [(u8, u8, u8); 32],

    // Timing
    pub(crate) clock: u64,
    pub(crate) watchdog_counter: u32,
    pub(crate) flip_screen: bool,

    // Deferred sub CPU reset (set by write_misc_latch, acted on in tick)
    pending_sub_cpu_reset: bool,

    // Debug observability (observer state — never saved in save states).
    // This board has no AddressSpace16, so it carries a raw watchpoint set
    // and latches per-CPU instruction PCs itself.
    pub(crate) watchpoints: Watchpoints,
    #[debug_events]
    pub(crate) debug_trace: DebugTraceBuffer,
    /// Instruction PC per CPU (main/sub/sound), latched at instruction
    /// boundaries each tick for hit/event attribution.
    pub(crate) debug_pc: [Option<u32>; 3],
}

impl NamcoGalagaBoard {
    pub fn new() -> Self {
        Self {
            main_cpu: Z80::new(),
            sub_cpu: Z80::new(),
            sound_cpu: Z80::new(),

            main_rom: Vec::new(),
            sub_rom: Vec::new(),
            sound_rom: Vec::new(),

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

            namco51_divider: ClockDivider::new(1, 2),
            namco50_divider: ClockDivider::new(1, 12),

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

            watchpoints: Watchpoints::new(),
            debug_trace: DebugTraceBuffer::new(),
            debug_pc: [None; 3],
        }
    }

    // -----------------------------------------------------------------------
    // Core tick — called from game wrappers via bus_split!
    // -----------------------------------------------------------------------

    /// Reset a Z80 to power-on state.
    fn reset_z80(cpu: &mut Z80) {
        cpu.hardware_reset();
    }

    pub fn tick(&mut self, bus: &mut dyn Bus<Address = u16, Data = u8>) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();

        // Handle deferred sub CPU reset (set by write_misc_latch bit 3).
        // Mirrors Z80::reset() without needing 'static bus lifetime.
        if self.pending_sub_cpu_reset {
            self.pending_sub_cpu_reset = false;
            Self::reset_z80(&mut self.sub_cpu);
            Self::reset_z80(&mut self.sound_cpu);
            // A CPU coming out of reset must start clean at 0x0000 with no
            // stale interrupt latched. Clearing these prevents a pending sound
            // NMI (accumulated while the CPU was held in reset) from firing
            // before the freshly reset CPU has set up its stack pointer — which
            // would otherwise push onto the reset SP (0xFFFF) and wreck it.
            self.sub_irq_pending = false;
            self.sound_nmi_pending = false;
        }

        // VBLANK interrupt: fire at the start of VBLANK (scanline 224).
        // Only assert IRQ if the mask (enable latch) is set, matching MAME's
        // vblank_irq: `if (state && m_main_irq_mask) set_input_line(ASSERT_LINE)`.
        // This prevents a race where VBLANK fires while the IRQ handler has
        // temporarily cleared the mask, which would cause spurious re-entry.
        let vblank_cycle = VISIBLE_LINES * TIMING.cycles_per_scanline;
        if frame_cycle == vblank_cycle {
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
        // Clear TC at end of VBLANK (start of visible area = frame_cycle 0)
        if frame_cycle == 0
            && let Namco51Wrapper::Lle(ref mut lle) = self.namco51
        {
            lle.mcu.set_tc(true); // Deassert
        }

        // Sound CPU NMI: fires at scanlines 64 and 192 (every 128 lines),
        // matching MAME's cpu3_interrupt_callback. Gated by misc latch Q2.
        const SOUND_NMI_SCANLINE_A: u64 = 64;
        const SOUND_NMI_SCANLINE_B: u64 = 192;
        let scanline_a_cycle = SOUND_NMI_SCANLINE_A * TIMING.cycles_per_scanline;
        let scanline_b_cycle = SOUND_NMI_SCANLINE_B * TIMING.cycles_per_scanline;
        if (frame_cycle == scanline_a_cycle || frame_cycle == scanline_b_cycle)
            && self.sound_nmi_enabled
            && !self.sub_reset
        {
            self.sound_nmi_pending = true;
            self.trace_interrupt(2, "sound NMI (scanline timer)");
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

        // Latch debug attribution context (per-CPU instruction PCs) before
        // CPU execution — bus dispatch cannot read CPU state mid-tick. This
        // board has no AddressSpace16, so the latch lives on the board.
        if !self.watchpoints.is_empty() || self.debug_trace.enabled() {
            if self.main_cpu.at_instruction_boundary() {
                self.debug_pc[0] = Some(self.main_cpu.pc as u32);
            }
            if self.sub_cpu.at_instruction_boundary() {
                self.debug_pc[1] = Some(self.sub_cpu.pc as u32);
            }
            if self.sound_cpu.at_instruction_boundary() {
                self.debug_pc[2] = Some(self.sound_cpu.pc as u32);
            }
        }

        // Execute all 3 CPUs BEFORE MCU so Z80 writes reach o_latch
        // before the MCU reads K (K is a hardware wire, not latched).
        self.main_cpu.execute_cycle(bus, BusMaster::Cpu(0));
        if !self.sub_reset {
            self.sub_cpu.execute_cycle(bus, BusMaster::Cpu(1));
            self.sound_cpu.execute_cycle(bus, BusMaster::Cpu(2));
        }

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
            if self.namco51_divider.tick() {
                lle.update_inputs(self.in0, self.in1);
                lle.tick();
            }
        }

        // Drive the 50XX score/protection MCU (if fitted) the same way: assert
        // its chip-select IRQ and R/W line after the Z80s have run, then step
        // it on its own machine-cycle divider.
        if let Some(ref mut n50) = self.namco50 {
            n50.set_chip_select(self.namco06.chip_select_active(2));
            n50.set_rw(self.namco06.is_read_mode());
            if self.namco50_divider.tick() {
                n50.tick();
            }
        }

        self.clock += 1;
        self.watchdog_counter += 1;
    }

    // -----------------------------------------------------------------------
    // Debug observability helpers
    // -----------------------------------------------------------------------

    /// Check a read against the watchpoint set. The 3 CPUs share one bus,
    /// so the accessing CPU's index scopes the watch.
    #[inline]
    pub(crate) fn watch_read(&mut self, master: BusMaster, addr: u16, data: u8) {
        if self.watchpoints.is_empty() {
            return;
        }
        if let BusMaster::Cpu(i) = master {
            let pc = self.debug_pc.get(i).copied().flatten();
            self.watchpoints.check_read(
                i,
                master.into(),
                self.clock,
                pc,
                addr as u32,
                data as u32,
                1,
            );
        }
    }

    /// Check a write against the watchpoint set. Call before applying the
    /// side effect (hits record `WatchpointPhase::Before`).
    #[inline]
    pub(crate) fn watch_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        if self.watchpoints.is_empty() {
            return;
        }
        if let BusMaster::Cpu(i) = master {
            let pc = self.debug_pc.get(i).copied().flatten();
            self.watchpoints.check_write(
                i,
                master.into(),
                self.clock,
                pc,
                addr as u32,
                data as u32,
                1,
            );
        }
    }

    /// Record an interrupt assertion against `cpu_index` (gated internally).
    fn trace_interrupt(&mut self, cpu_index: usize, detail: &'static str) {
        if !self.debug_trace.enabled() {
            return;
        }
        self.debug_trace.record(DebugEvent {
            cpu_index: Some(cpu_index),
            detail: Some(detail),
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
        self.debug_trace.record(DebugEvent {
            cpu_index: Some(0),
            pc: self.debug_pc[0],
            addr: Some(addr as u32),
            value: Some(value as u32),
            width: 1,
            device: Some(device),
            detail,
            ..DebugEvent::new(
                self.clock,
                phosphor_core::core::watchpoint::DebugAccessSource::Cpu(0),
                kind,
            )
        });
    }

    /// Record a shared-bus write event from a game wrapper's `Bus::write`.
    /// Maps the common Galaga-platform layout; cheap no-op while tracing
    /// is disabled.
    pub(crate) fn trace_bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        if !self.debug_trace.enabled() {
            return;
        }
        let (kind, device, detail) = match addr {
            0x6800..=0x681F => (DebugEventKind::DeviceWrite, Some("Namco WSG"), None),
            0x6820..=0x6827 => (
                DebugEventKind::DeviceWrite,
                Some("Misc latch"),
                Some(match addr & 7 {
                    0 => "main IRQ enable",
                    1 => "sub IRQ enable",
                    2 => "sound NMI enable",
                    3 => "sub/sound reset",
                    _ => "latch bit",
                }),
            ),
            0x6830..=0x683F => (DebugEventKind::Watchdog, None, Some("watchdog cleared")),
            // Custom I/O data/ctrl are recorded by the dedicated helpers.
            0x7000..=0x71FF => return,
            0xA000..=0xA007 => (DebugEventKind::DeviceWrite, Some("Video latch"), None),
            0xB800..=0xB840 => (DebugEventKind::DeviceWrite, Some("EAROM"), None),
            _ => (DebugEventKind::MemoryWrite, None, None),
        };
        let (cpu_index, pc) = match master {
            BusMaster::Cpu(i) if i < 3 => (Some(i), self.debug_pc[i]),
            _ => (None, None),
        };
        self.debug_trace.record(DebugEvent {
            cpu_index,
            pc,
            addr: Some(addr as u32),
            value: Some(data as u32),
            width: 1,
            device,
            detail,
            ..DebugEvent::new(self.clock, master.into(), kind)
        });
    }

    // -----------------------------------------------------------------------
    // Bus dispatch helpers — called from game wrapper Bus impls
    // -----------------------------------------------------------------------

    /// Read ROM for the requesting CPU.
    pub fn read_rom(&self, master: BusMaster, addr: u16) -> u8 {
        let offset = addr as usize;
        match master {
            BusMaster::Cpu(0) => self.main_rom.get(offset).copied().unwrap_or(0xFF),
            BusMaster::Cpu(1) => self.sub_rom.get(offset).copied().unwrap_or(0xFF),
            BusMaster::Cpu(2) => self.sound_rom.get(offset).copied().unwrap_or(0xFF),
            _ => 0xFF,
        }
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
            2 => self.namco50.as_ref().map_or(0xFF, Namco50::read),
            _ => 0xFF,
        };
        if self.debug_trace.enabled() {
            let device = match chip {
                0 => "Namco 51XX",
                1 => "Namco 53XX",
                2 => "Namco 50XX",
                _ => "Namco 06XX",
            };
            self.trace_custom_io(DebugEventKind::DeviceRead, 0x7000, data, device, None);
        }
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
            if self.debug_trace.enabled() {
                self.trace_custom_io(
                    DebugEventKind::DeviceWrite,
                    0x7000,
                    data,
                    "Namco 51XX",
                    Some("command/argument"),
                );
            }
            self.namco51.write(data);
        }
        // 53XX has no write interface
        if self.namco06.chip_select(2) && self.namco50.is_some() {
            if self.debug_trace.enabled() {
                self.trace_custom_io(
                    DebugEventKind::DeviceWrite,
                    0x7000,
                    data,
                    "Namco 50XX",
                    Some("command/argument"),
                );
            }
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
        if self.debug_trace.enabled() {
            self.trace_custom_io(
                DebugEventKind::DeviceWrite,
                0x7100,
                data,
                "Namco 06XX",
                Some("control (mode + chip select + timer)"),
            );
        }
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

    pub fn load_main_rom(&mut self, data: &[u8]) {
        self.main_rom = data.to_vec();
    }

    pub fn load_sub_rom(&mut self, data: &[u8]) {
        self.sub_rom = data.to_vec();
    }

    pub fn load_sound_rom(&mut self, data: &[u8]) {
        self.sound_rom = data.to_vec();
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

    /// Attach the Namco 50XX score/protection MCU and load its firmware ROM
    /// (2048 bytes). Only boards that fit the chip (e.g. Xevious) call this;
    /// otherwise 06XX chip-select 2 reads back the idle bus.
    pub fn load_50xx_rom(&mut self, data: &[u8]) {
        let mut n50 = Namco50::new();
        n50.load_rom(data);
        self.namco50 = Some(n50);
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

    /// Reset all board state except ROMs and palette PROMs.
    /// The caller must reset CPUs separately (requires bus_split).
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
        self.namco51_divider.reset();
        self.namco50_divider.reset();

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

    pub fn debug_tick_boundaries(&self) -> u32 {
        let mut mask = 0u32;
        if self.main_cpu.at_instruction_boundary() {
            mask |= 1;
        }
        if !self.sub_reset && self.sub_cpu.at_instruction_boundary() {
            mask |= 2;
        }
        if !self.sub_reset && self.sound_cpu.at_instruction_boundary() {
            mask |= 4;
        }
        mask
    }
}

impl Saveable for NamcoGalagaBoard {
    fn save_state(&self, w: &mut StateWriter) {
        // CPUs
        self.main_cpu.save_state(w);
        self.sub_cpu.save_state(w);
        self.sound_cpu.save_state(w);

        // Devices
        self.wsg.save_state(w);
        self.namco06.save_state(w);

        // 51XX: mode discriminant (0=HLE, 1=LLE) + mode-specific state
        match &self.namco51 {
            Namco51Wrapper::Hle(n) => {
                w.write_u8(0);
                n.save_state(w);
            }
            Namco51Wrapper::Lle(n) => {
                w.write_u8(1);
                n.save_state(w);
                self.namco51_divider.save_state(w);
            }
        }

        self.namco53.save_state(w);

        // 50XX: presence flag + (when fitted) MCU state and its divider.
        match &self.namco50 {
            Some(n50) => {
                w.write_bool(true);
                n50.save_state(w);
                self.namco50_divider.save_state(w);
            }
            None => w.write_bool(false),
        }

        // I/O state
        w.write_u8(self.in0);
        w.write_u8(self.in1);
        w.write_u8(self.dswa);
        w.write_u8(self.dswb);

        // Latch + interrupt state
        w.write_bool(self.main_irq_enabled);
        w.write_bool(self.sub_irq_enabled);
        w.write_bool(self.sound_nmi_enabled);
        w.write_bool(self.sub_reset);
        w.write_bool(self.main_irq_pending);
        w.write_bool(self.main_nmi_pending);
        w.write_bool(self.sub_irq_pending);
        w.write_bool(self.sound_nmi_pending);
        w.write_bool(self.flip_screen);

        // Timing
        w.write_u64_le(self.clock);
        w.write_u32_le(self.watchdog_counter);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        // CPUs
        self.main_cpu.load_state(r)?;
        self.sub_cpu.load_state(r)?;
        self.sound_cpu.load_state(r)?;

        // Devices
        self.wsg.load_state(r)?;
        self.namco06.load_state(r)?;

        // 51XX: read mode discriminant and load matching state
        let namco51_mode = r.read_u8()?;
        match namco51_mode {
            0 => {
                // HLE mode
                let mut n = Namco51::new();
                n.load_state(r)?;
                self.namco51 = Namco51Wrapper::Hle(n);
            }
            1 => {
                // LLE mode — requires that the ROM was already loaded
                match &mut self.namco51 {
                    Namco51Wrapper::Lle(n) => {
                        n.load_state(r)?;
                        self.namco51_divider.load_state(r)?;
                    }
                    _ => {
                        return Err(SaveError::InvalidFormat(
                            "51XX LLE save state but no ROM loaded".to_string(),
                        ));
                    }
                }
            }
            _ => {
                return Err(SaveError::InvalidFormat(format!(
                    "unknown 51XX mode: {}",
                    namco51_mode
                )));
            }
        }

        self.namco53.load_state(r)?;

        // 50XX: presence flag; if set, the MCU must already be attached.
        if r.read_bool()? {
            match &mut self.namco50 {
                Some(n50) => {
                    n50.load_state(r)?;
                    self.namco50_divider.load_state(r)?;
                }
                None => {
                    return Err(SaveError::InvalidFormat(
                        "50XX save state but no ROM loaded".to_string(),
                    ));
                }
            }
        }

        // I/O state
        self.in0 = r.read_u8()?;
        self.in1 = r.read_u8()?;
        self.dswa = r.read_u8()?;
        self.dswb = r.read_u8()?;

        // Latch + interrupt state
        self.main_irq_enabled = r.read_bool()?;
        self.sub_irq_enabled = r.read_bool()?;
        self.sound_nmi_enabled = r.read_bool()?;
        self.sub_reset = r.read_bool()?;
        self.main_irq_pending = r.read_bool()?;
        self.main_nmi_pending = r.read_bool()?;
        self.sub_irq_pending = r.read_bool()?;
        self.sound_nmi_pending = r.read_bool()?;
        self.flip_screen = r.read_bool()?;

        // Timing
        self.clock = r.read_u64_le()?;
        self.watchdog_counter = r.read_u32_le()?;

        Ok(())
    }
}

impl Default for NamcoGalagaBoard {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::watchpoint::{DebugAccessSource, WatchpointKind, WatchpointPhase};

    mod watchpoints {
        use super::*;

        #[test]
        fn write_watch_fires_with_cpu_and_context_attribution() {
            let mut board = NamcoGalagaBoard::new();
            board.clock = 1234;
            board.debug_pc = [Some(0x0100), Some(0x0200), None];
            board.watchpoints.set(1, 0x8800, WatchpointKind::Write);

            // Main CPU access: watch is scoped to the sub CPU → no hit.
            board.watch_write(BusMaster::Cpu(0), 0x8800, 0x11);
            assert!(board.watchpoints.take_hit().is_none());

            // Sub CPU access fires with sub-CPU PC attribution.
            board.watch_write(BusMaster::Cpu(1), 0x8800, 0x22);
            let hit = board.watchpoints.take_hit().unwrap();
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
            board.watchpoints.set(0, 0x9000, WatchpointKind::Read);

            board.watch_read(BusMaster::Cpu(0), 0x9000, 0xAB);
            let hit = board.watchpoints.take_hit().unwrap();
            assert_eq!(hit.kind, WatchpointKind::Read);
            assert_eq!(hit.phase, WatchpointPhase::After);
            assert_eq!(hit.value, 0xAB);
        }
    }

    mod debug_events {
        use super::*;

        #[test]
        fn tracing_disabled_records_nothing() {
            let mut board = NamcoGalagaBoard::new();
            board.trace_bus_write(BusMaster::Cpu(0), 0x6800, 0x01);
            board.write_custom_io_ctrl(0xA1);
            assert!(board.debug_trace.is_empty());
        }

        #[test]
        fn custom_io_transactions_attribute_chips() {
            let mut board = NamcoGalagaBoard::new();
            board.debug_trace.set_enabled(true);
            board.debug_pc[0] = Some(0x1BCC);

            // 06XX control: write mode, chip select 0 (51XX)
            board.write_custom_io_ctrl(0x01);
            // Data write routed to the 51XX
            board.write_custom_io(0xA8);

            let events = board.debug_trace.events();
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
            board.debug_trace.set_enabled(true);
            board.debug_pc = [Some(0x0100), Some(0x0200), Some(0x0300)];

            board.trace_bus_write(BusMaster::Cpu(0), 0x6805, 0x07); // WSG
            board.trace_bus_write(BusMaster::Cpu(0), 0x6823, 0x01); // misc latch bit 3
            board.trace_bus_write(BusMaster::Cpu(0), 0x6830, 0x00); // watchdog
            board.trace_bus_write(BusMaster::Cpu(1), 0x8900, 0x42); // memory, sub CPU
            board.trace_bus_write(BusMaster::Cpu(0), 0x7100, 0x01); // custom io: skipped here

            let events = board.debug_trace.events();
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
