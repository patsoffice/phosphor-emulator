//! Atari Missile Command (1980).
//!
//! # Schematics
//!
//! | Drawing | Source | Pages |
//! |---|---|---|
//! | `Missile Command` 035467-XX rev D, sheet 2 side B | `arcade-museum.com/manuals-videogames/M/MissileCommand.pdf` | PDF p63, `Input and Output Circuitry` carries the POKEY |
//! | `Regulator/Audio II PCB` 035435-02 rev B | same | PDF p60, sheet 1 side A |
//!
//! That PDF is three manuals in one and the Drawing Package Supplement is
//! appended at the end, PDF pages 59 to 63.
//!
//! The audio path is transcribed with Tempest's, which shares the amplifier
//! board, in
//! [`docs/schematics/atari-pokey-audio-output.md`](../../docs/schematics/atari-pokey-audio-output.md).
//! None of it is modelled: a 10k/0.1 uF load network at the POKEY, a follower,
//! an antiphase output pair, and two TDA2002A channels driving two speakers. See
//! `phosphor-emulator-hd8n`.

use phosphor_core::audio::{DcBlocker, SampleRing};
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::input::{DrainPolicy, RelativeCounter};
use phosphor_core::core::machine::{
    ActionRole, AnalogAxisKind, AudioSource, AxisSign, DefaultBinding, DipApplyTiming, DipChoice,
    DipOption, DipSwitchBank, DipSwitches, Direction, InputConfigurable, InputControl, InputEvent,
    InputId, InputKind, KeyId, MachineCore, MouseControl, PadAxis, PadButton, PadControl,
    Renderable, SaveState,
};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::cpu::state::M6502State;
use phosphor_core::cpu::{Cpu, CpuStateTrait};
use phosphor_core::device::pokey::Pokey;
use phosphor_macros::{BusDebug, MemoryRegion, Saveable};

use crate::rom_loader::{RomEntry, RomRegion};
use crate::set_bit_active_low;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
enum Region {
    Ram = 1,
    Io = 2,
    Rom = 3,
}

// ---------------------------------------------------------------------------
// Missile Command ROM definitions
// ---------------------------------------------------------------------------

/// Program ROM: 12KB at 0x5000-0x7FFF.
/// The last 2KB (0x7800-0x7FFF) is also mirrored to 0xF800-0xFFFF for vectors.
pub static MISSILE_COMMAND_ROM: RomRegion = RomRegion {
    size: 0x3000, // 12KB
    entries: &[
        RomEntry {
            name: "035820-02.h1",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0x7a62ce6a],
        },
        RomEntry {
            name: "035821-02.jk1",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0xdf3bd57f],
        },
        RomEntry {
            name: "035822-03e.kl1",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0x1a2f599a, 0xa1cd384a], // -03e (parent) and -02 (missile2)
        },
        RomEntry {
            name: "035823-02.lm1",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0x82e552bb],
        },
        RomEntry {
            name: "035824-02.np1",
            size: 0x0800,
            offset: 0x2000,
            crc32: &[0x606e42e0],
        },
        RomEntry {
            name: "035825-02.r1",
            size: 0x0800,
            offset: 0x2800,
            crc32: &[0xf752eaeb],
        },
    ],
};

// ---------------------------------------------------------------------------
// Input button IDs
// ---------------------------------------------------------------------------
pub const INPUT_COIN: u8 = 0;
pub const INPUT_START1: u8 = 1;
pub const INPUT_START2: u8 = 2;
pub const INPUT_FIRE_LEFT: u8 = 3;
pub const INPUT_FIRE_CENTER: u8 = 4;
pub const INPUT_FIRE_RIGHT: u8 = 5;
pub const INPUT_TRACK_L: u8 = 6;
pub const INPUT_TRACK_R: u8 = 7;
pub const INPUT_TRACK_U: u8 = 8;
pub const INPUT_TRACK_D: u8 = 9;
// 10 and 11 are the analog trackball axes below, so the digital run continues
// at 12 rather than at 10.
pub const INPUT_SELFTEST: u8 = 12;

// ---------------------------------------------------------------------------
// Analog axis IDs (trackball)
// ---------------------------------------------------------------------------
pub const ANALOG_TRACKBALL_X: u8 = 0;
pub const ANALOG_TRACKBALL_Y: u8 = 1;

// Typed control ids for the analog axes. The digital controls reuse the
// `INPUT_*` numbering (0..=9); the trackball axes need ids distinct from those,
// since `InputId` is a single namespace.
const CTRL_TRACKBALL_X: InputId = InputId(10);
const CTRL_TRACKBALL_Y: InputId = InputId(11);

/// Typed logical controls. Default bindings reproduce the historical
/// name-matched keyboard/gamepad/mouse defaults.
const MISSILE_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_COIN as u16),
        stable_name: "coin",
        label: "Coin",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[
            DefaultBinding::Key(KeyId::Num5),
            DefaultBinding::Pad(PadControl::Button(PadButton::Back)),
        ],
    },
    InputControl {
        id: InputId(INPUT_START1 as u16),
        stable_name: "start1",
        label: "P1 Start",
        kind: InputKind::Start,
        player: Some(1),
        default_bindings: &[
            DefaultBinding::Key(KeyId::Num1),
            DefaultBinding::Pad(PadControl::Button(PadButton::Start)),
        ],
    },
    InputControl {
        id: InputId(INPUT_START2 as u16),
        stable_name: "start2",
        label: "P2 Start",
        kind: InputKind::Start,
        player: Some(2),
        default_bindings: &[DefaultBinding::Key(KeyId::Num2)],
    },
    InputControl {
        id: InputId(INPUT_FIRE_LEFT as u16),
        stable_name: "fire_left",
        label: "Fire Left",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        // Primary role plus the middle mouse button (the three-trackball cabinet
        // maps left/center/right fire to a mouse button each).
        default_bindings: &[DefaultBinding::Mouse(MouseControl::Middle)],
    },
    InputControl {
        id: InputId(INPUT_FIRE_CENTER as u16),
        stable_name: "fire_center",
        label: "Fire Center",
        kind: InputKind::Action(ActionRole::Secondary),
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::Left)],
    },
    InputControl {
        id: InputId(INPUT_FIRE_RIGHT as u16),
        stable_name: "fire_right",
        label: "Fire Right",
        kind: InputKind::Action(ActionRole::Tertiary),
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::Right)],
    },
    InputControl {
        id: InputId(INPUT_TRACK_L as u16),
        stable_name: "track_left",
        label: "P1 Left",
        kind: InputKind::DigitalDirection {
            direction: Direction::Left,
        },
        player: Some(1),
        default_bindings: &[
            DefaultBinding::Key(KeyId::Left),
            DefaultBinding::Pad(PadControl::Button(PadButton::DPadLeft)),
            DefaultBinding::Pad(PadControl::Axis(PadAxis::LeftX, AxisSign::Negative)),
        ],
    },
    InputControl {
        id: InputId(INPUT_TRACK_R as u16),
        stable_name: "track_right",
        label: "P1 Right",
        kind: InputKind::DigitalDirection {
            direction: Direction::Right,
        },
        player: Some(1),
        default_bindings: &[
            DefaultBinding::Key(KeyId::Right),
            DefaultBinding::Pad(PadControl::Button(PadButton::DPadRight)),
            DefaultBinding::Pad(PadControl::Axis(PadAxis::LeftX, AxisSign::Positive)),
        ],
    },
    InputControl {
        id: InputId(INPUT_TRACK_U as u16),
        stable_name: "track_up",
        label: "P1 Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(1),
        default_bindings: &[
            DefaultBinding::Key(KeyId::Up),
            DefaultBinding::Pad(PadControl::Button(PadButton::DPadUp)),
            DefaultBinding::Pad(PadControl::Axis(PadAxis::LeftY, AxisSign::Negative)),
        ],
    },
    InputControl {
        id: InputId(INPUT_TRACK_D as u16),
        stable_name: "track_down",
        label: "P1 Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(1),
        default_bindings: &[
            DefaultBinding::Key(KeyId::Down),
            DefaultBinding::Pad(PadControl::Button(PadButton::DPadDown)),
            DefaultBinding::Pad(PadControl::Axis(PadAxis::LeftY, AxisSign::Positive)),
        ],
    },
    InputControl {
        id: InputId(INPUT_SELFTEST as u16),
        stable_name: "self_test",
        label: "Self-Test",
        kind: InputKind::Service,
        player: None,
        default_bindings: &[DefaultBinding::Key(KeyId::T)],
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
// Master clock: 10 MHz XTAL
// CPU clock: 10 MHz / 8 = 1.25 MHz
// Pixel clock: 10 MHz / 2 = 5 MHz
// HTOTAL: 320 pixel clocks → 320/4 = 80 CPU cycles per scanline
// VTOTAL: 256 scanlines per frame
// Frame rate: 5 MHz / (320 * 256) ≈ 61.04 Hz
const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 1_250_000, // 10 MHz / 8
    cycles_per_scanline: 80, // 320 pixel clocks / 4
    total_scanlines: 256,    // VTOTAL
    display_width: 256,
    display_height: 231,
    display_aspect: Some((4, 3)),
};

/// The board's crystal and everything divided out of it.
///
/// One 10 MHz crystal, with the 6502 on a divide-by-eight and the pixel clock
/// on a divide-by-two.
pub fn clock_tree() -> phosphor_core::core::ClockTree {
    use phosphor_core::core::{ClockDomainName as Clk, ClockTree, RootId};
    let mut t = ClockTree::new(10_000_000);
    let cpu = t.add_domain(Clk::Cpu, RootId::MAIN, 1, 8); // 1.25 MHz
    let dot = t.add_domain(Clk::Pixel, RootId::MAIN, 1, 2); // 5 MHz
    t.set_step_domain(cpu);
    // 4:1 off one crystal, so 320 dot clocks is exactly 80 CPU cycles.
    t.set_raster(dot, 320, 0);
    t
}

/// Missile Command Arcade System (Atari, 1980)
///
/// Hardware: MOS 6502 @ 1.25 MHz, POKEY for sound/IO.
/// Video: 256x231 bitmap, bit-planar 2bpp (8-color with 3rd bit region
/// for bottom scanlines), 8-entry programmable palette at 0x4B00.
///
/// Memory map:
///   0x0000-0x3FFF  Video/Work RAM (16KB)
///   0x4000-0x400F  POKEY (mirrored across 0x4000-0x47FF)
///   0x4800         Read: IN0 (switches) or trackball (CTRLD-dependent)
///                  Write: Output latch (CTRLD, LEDs, coin counters, flip)
///   0x4900         Read: IN1 (fire buttons, VBLANK, tilt, test)
///   0x4A00         Read: DIP switches (pricing options)
///   0x4B00-0x4B07  Write: Color RAM (8 entries, 1-bit RGB)
///   0x4C00         Write: Watchdog reset
///   0x4D00         Write: IRQ acknowledge
///   0x5000-0x7FFF  Program ROM (12KB)
///   0xF800-0xFFFF  ROM mirror (vectors)
/// Missile Command's hardware, everything the 6502 talks *to*. Held apart from
/// the CPU so a cycle dispatches at a concrete bus rather than a trait object
/// (see `docs/designs/concrete-bus-dispatch.md`).
#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct MissileCommandBoard {
    #[debug_device("POKEY")]
    #[save(id = 1)]
    pokey: Pokey,

    /// The address space persists its own writable regions, which here is the
    /// single 16 KB RAM the MADSEL circuit also writes pixels into.
    #[debug_map(cpu = 0)]
    #[save(id = 2)]
    map: AddressSpace16,

    // I/O registers
    // IN0 at 0x4800 (active-low switches, directly stored active-low: 1=released, 0=pressed)
    //   Bit 7: Right Coin    Bit 6: Center Coin   Bit 5: Left Coin
    //   Bit 4: 1P Start      Bit 3: 2P Start
    //   Bit 2-0: Cocktail fire buttons (active-low)
    #[save(id = 3)]
    in0: u8,
    // IN1 at 0x4900 (mixed polarity)
    //   Bit 7: VBLANK (active-high, set dynamically)
    //   Bit 6: Self-test (active-low, normally 1)
    //   Bit 5: SLAM/Tilt (active-low, normally 1)
    //   Bit 4-3: Trackball direction (set dynamically)
    //   Bit 2: Fire Left (active-low, normally 1)
    //   Bit 1: Fire Center (active-low, normally 1)
    //   Bit 0: Fire Right (active-low, normally 1)
    #[save(id = 4)]
    in1: u8,
    // R10 DIP switches at 0x4A00 (pricing options). Operator configuration, which
    // survives a load the way the switches on a cabinet do.
    #[save_skip]
    dip_switches: u8,
    /// R8 DIP switches (game options), read through the POKEY's pot inputs
    /// rather than as a byte — see [`MissileCommandBoard::refresh_dip_pots`].
    ///
    /// **The two banks do not share a bit sense.** R10 reads a closed (On)
    /// switch as 0; at the pots a closed switch reads as 1. Confirmed on the
    /// game's own self-test screen: with these pots undriven the options display
    /// reads `7 CITIES` and shows no bonus-city line, which is toggles 1, 2, 5,
    /// 6 and 7 all *open*. Carrying R10's sense across would have inverted every
    /// option in this bank.
    #[save_skip]
    dip_r8: u8,
    // CTRLD: bit 0 of output latch (0x4800 write) — selects trackball vs switches at 0x4800 read
    #[save(id = 5)]
    ctrld: bool,
    // Color RAM: 8 palette entries at 0x4B00-0x4B07
    #[save(id = 6)]
    palette: [u8; 8],

    // Trackball counters (4-bit each, combined into one byte when CTRLD=1)
    #[save(id = 7)]
    trackball_x: RelativeCounter,
    #[save(id = 8)]
    trackball_y: RelativeCounter,
    // Mouse accumulator: set_analog() adds here; tick() drains ±1 per tick
    // so the 4-bit counters never skip values and the game reads correct deltas.

    // IRQ state — based on /32V signal (inverted bit 5 of V counter)
    // Asserted at scanlines where 32V=0 (scanlines 0-31, 64-95, 128-159, 192-223)
    // Cleared by writing to 0x4D00 (IRQ acknowledge)
    #[save(id = 9)]
    irq_state: bool,

    // MADSEL circuit: intercepts (zp,X) addressing mode instructions (opcodes with
    // low 5 bits == 0x01) and redirects bus access 5 CPU cycles later to VRAM.
    // This is how the game writes pixels — without it, the screen stays blank.
    // Timed in CPU cycles (not master ticks) so clock halving doesn't break it.
    #[save(id = 10)]
    madsel_lastcycles: u64,
    #[save(id = 11)]
    stall_cycles: u8, // extra cycle penalty for 3rd-bit MUSHROOM MADSEL accesses

    /// The 6502's SYNC pin, sampled once per cycle by `begin_cycle`. MADSEL is
    /// armed on an opcode fetch, so the bus has to know. A reset CPU sits in
    /// Fetch, so this starts asserted. Derived state: re-sampled before every
    /// cycle that can read the bus, so it is not saved.
    #[save_skip]
    cpu_is_sync: bool,

    // System
    #[save(id = 12)]
    clock: u64,
    #[save(id = 13)]
    cpu_cycles: u64, // incremented only when CPU actually executes
    #[save(id = 14)]
    watchdog_frame_count: u8, // frames since last write to 0x4C00; resets machine at 8

    #[save_skip]
    scanline_buffer: Vec<u8>, // 256 * 231 * 3 = 177,408 bytes (RGB24)
    /// A load lands mid-frame, so the buffer it left behind describes a frame
    /// that was never finished; the next `run_frame` refills it.
    #[save_skip(default)]
    scanline_buffer_valid: bool, // true after run_frame() completes

    /// Samples already mixed and waiting for the frontend to drain, which the
    /// next frame refills.
    #[save_skip(default = SampleRing::with_capacity(1024))]
    audio_buffer: SampleRing<i16>,
    /// The output coupling capacitor. POKEY's output is unipolar and sits at
    /// zero when idle, so it needs the DC removed rather than a fixed midpoint
    /// subtracted — see [`DcBlocker`].
    ///
    /// The capacitor this stands for is C6 and C15 on the Regulator/Audio II
    /// PCB, not anything on the game PCB: the path from POKEY pin 37 to the
    /// AUDIO1 and AUDIO2 connector pins is DC-coupled throughout, on op-amps
    /// running split supplies with their non-inverting inputs at ground.
    #[save(id = 15)]
    dc_blocker: DcBlocker,
}

/// Atari Missile Command (1980): a 6502 beside the board it drives.
#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct MissileCommandSystem {
    #[debug_cpu("M6502")]
    #[save(id = 1)]
    cpu: M6502,
    #[debug_bus]
    #[save(id = 2)]
    pub board: MissileCommandBoard,
}

/// One master cycle. The board decides whether the CPU runs this cycle (the
/// clock halves during vblank, and a MADSEL access can stall it), then the
/// 6502 executes against the board, which *is* the bus.
#[inline]
pub fn tick(cpu: &mut M6502, board: &mut MissileCommandBoard) {
    if board.begin_cycle(cpu) {
        cpu.execute_cycle(board, BusMaster::Cpu(0));
        board.cpu_cycles += 1;
    }
    board.clock += 1;
}

/// Missile Command reads two 4-bit trackball counters. `tick` drains them from
/// a 1000-cycle divider (~20 ticks/frame) rather than once per frame, so the
/// per-call step is a single unit and the divider rate is what sets the
/// crosshair speed; the remainder stays pending for the next tick.
fn new_track_counter() -> RelativeCounter {
    RelativeCounter::new(0x0F, 1, false, DrainPolicy::Unit)
}

impl MissileCommandSystem {
    pub fn new() -> Self {
        Self {
            cpu: M6502::new(),
            board: MissileCommandBoard::new(),
        }
    }

    /// Advance one master cycle, returning the instruction-boundary mask.
    pub fn step_cycle(&mut self) -> u32 {
        tick(&mut self.cpu, &mut self.board);
        u32::from(self.cpu.at_instruction_boundary())
    }

    pub fn load_rom_set(
        &mut self,
        rom_set: &crate::rom_loader::RomSet,
    ) -> Result<(), crate::rom_loader::RomLoadError> {
        self.board.load_rom_set(rom_set)
    }

    pub fn get_cpu_state(&self) -> M6502State {
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

impl MissileCommandBoard {
    pub fn new() -> Self {
        let mut board = Self {
            pokey: Pokey::with_clock(1_250_000, phosphor_core::audio::host_sample_rate()),
            map: Self::build_map(),
            in0: 0xFF,          // All buttons released (active-low: 1 = not pressed)
            in1: 0x67, // Fire buttons released (bits 0-2 = 1), test/tilt released (bits 5-6 = 1), VBLANK off
            dip_switches: 0x00, // Default DIP: 1 coin/1 play, English, standard options
            // 0x00 is what the game already saw when these pots were undriven:
            // 7 cities, bonus credit for 4 coins, large Trak-Ball, no bonus city.
            // Three of those four are the manual's factory setting; the bonus
            // city is not (factory is every 10,000, which is 0x70). Keeping 0x00
            // preserves the behaviour this machine has always had.
            dip_r8: 0x00,
            ctrld: false,
            palette: [0; 8],
            trackball_x: new_track_counter(),
            trackball_y: new_track_counter(),
            irq_state: false,
            madsel_lastcycles: 0,
            stall_cycles: 0,
            cpu_is_sync: true,
            clock: 0,
            cpu_cycles: 0,
            watchdog_frame_count: 0,
            scanline_buffer: vec![0u8; 256 * 231 * 3],
            scanline_buffer_valid: false,
            audio_buffer: SampleRing::with_capacity(1024),
            dc_blocker: DcBlocker::new(phosphor_core::audio::host_sample_rate()),
        };
        board.refresh_dip_pots();
        board
    }

    /// Drive the R8 DIP switches onto the POKEY's pot inputs.
    ///
    /// R8's eight toggles are wired one per pot line rather than onto a byte
    /// the CPU can read directly, so the game reads them the long way round:
    /// strobe POTGO, poll ALLPOT until the scan finishes, then read POT0-7. That
    /// this is the path, and not the ALLPOT shortcut `ccastles.rs` uses, was
    /// settled by watching which POKEY offsets the ROM actually reads — it
    /// touches 0x00-0x08, which is the full scan.
    ///
    /// A closed switch drives its line high, so a set bit becomes a full-scale
    /// pot reading. See [`dip_r8`](Self::dip_r8) for why that is the opposite of
    /// the R10 byte's sense and how it was confirmed.
    fn refresh_dip_pots(&mut self) {
        for n in 0..8 {
            let level = if self.dip_r8 & (1 << n) != 0 {
                0x80
            } else {
                0x00
            };
            self.pokey.set_pot_input(n, level);
        }
    }

    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(Region::Ram, "RAM", 0x0000, 0x4000, AccessKind::ReadWrite)
            .region(Region::Io, "I/O", 0x4000, 0x1000, AccessKind::Io)
            .region(
                Region::Rom,
                "Program ROM",
                0x5000,
                0x3000,
                AccessKind::ReadOnly,
            )
            .mirror(0xF800, 0x7800, 0x0800);
        map
    }

    /// Current scanline (V counter), 0-255.
    pub fn current_scanline(&self) -> u16 {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();
        (frame_cycle / TIMING.cycles_per_scanline) as u16
    }

    /// Board work that leads a master cycle, returning whether the CPU runs on
    /// it. The CPU lives on the machine, which passes it in for the debugger's
    /// access-attribution latch.
    fn begin_cycle(&mut self, cpu: &M6502) -> bool {
        self.cpu_is_sync = cpu.is_sync();
        // Trackball movement: increment 4-bit counters from keyboard or mouse accumulator.
        // Keyboard: ±1 per tick while held. Mouse: drain ±1 per tick from accumulator.
        // Rate: every 1000 cycles ≈ 20 ticks/frame — enough for smooth crosshair tracking
        // while keeping deltas small enough for the 4-bit counter.
        if self.clock.is_multiple_of(1000) {
            self.trackball_x.update();
            self.trackball_y.update();
        }

        // Per-scanline rendering: at each scanline boundary, render the current
        // scanline from VRAM + palette before the CPU processes it, matching
        // hardware CRT read timing (the beam scans using VRAM at line start).
        let frame_cycle = self.clock % TIMING.cycles_per_frame();
        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let scanline = (frame_cycle / TIMING.cycles_per_scanline) as u16;
            if scanline >= 25 {
                self.render_scanline_to_buffer(scanline as usize);
            }

            // IRQ generation based on /32V signal
            // /IRQ is clocked by /16V transitions. When not flipped:
            //   At V=0,64,128,192 (32V=0): IRQ asserted
            //   At V=32,96,160,224 (32V=1): IRQ deasserted
            // The IRQ is latched on each SYNC (instruction fetch).
            if scanline.is_multiple_of(16) {
                self.irq_state = (scanline >> 5) & 1 == 0;
            }
        }

        // Update VBLANK bit in IN1 (bit 7, active-high)
        // VBLANK is active when V < 25
        let scanline = self.current_scanline();
        if scanline < 25 {
            self.in1 |= 0x80;
        } else {
            self.in1 &= !0x80;
        }

        // POKEY tick (runs at CPU clock rate = 1.25 MHz)
        self.pokey.tick();

        // CPU clock halving: at scanline 224+, CPU runs at MASTER_CLOCK/16 (0.625 MHz)
        // instead of MASTER_CLOCK/8 (1.25 MHz). We skip every other CPU cycle.
        // stall_cycles handles the extra cycle penalty for 3rd-bit MADSEL accesses.
        let run_cpu = if self.stall_cycles > 0 {
            self.stall_cycles -= 1;
            false
        } else if scanline >= 224 {
            self.clock.is_multiple_of(2)
        } else {
            true
        };

        if run_cpu {
            // Latch watchpoint attribution context (cycle + instruction PC)
            // before CPU execution — bus dispatch cannot read CPU state
            // mid-tick.
            if self.map.has_any_watchpoints() {
                let pc = cpu.at_instruction_boundary().then_some(cpu.pc as u32);
                self.map.latch_access_context(self.clock, pc);
            }
        }

        run_cpu
    }

    pub fn load_rom_set(
        &mut self,
        rom_set: &crate::rom_loader::RomSet,
    ) -> Result<(), crate::rom_loader::RomLoadError> {
        let rom_data = MISSILE_COMMAND_ROM.load(rom_set)?;
        self.map.load_region(Region::Rom, &rom_data);
        Ok(())
    }

    pub fn read_ram(&self, addr: usize) -> u8 {
        let ram = self.map.region_data(Region::Ram);
        if addr < ram.len() { ram[addr] } else { 0 }
    }

    pub fn write_ram(&mut self, addr: usize, data: u8) {
        let ram = self.map.region_data_mut(Region::Ram);
        if addr < ram.len() {
            ram[addr] = data;
        }
    }

    pub fn read_palette(&self, index: usize) -> u8 {
        if index < 8 { self.palette[index] } else { 0 }
    }

    pub fn clock(&self) -> u64 {
        self.clock
    }

    /// Check if the MADSEL signal is active. MADSEL goes high exactly 5 CPU
    /// cycles after arming and stays high for 1 cycle. Resets after firing.
    /// Uses cpu_cycles (not master clock) so clock halving doesn't break timing.
    fn get_madsel(&mut self) -> bool {
        if self.madsel_lastcycles > 0 {
            let elapsed = self.cpu_cycles.wrapping_sub(self.madsel_lastcycles);
            if elapsed == 5 {
                self.madsel_lastcycles = 0;
                return true;
            }
        }
        false
    }

    /// MADSEL write: redirect bus write to VRAM using bit-planar format.
    /// Address bits select VRAM byte and pixel within it.
    /// Data bits 7:6 select the 2-bit color value.
    /// Data bit 5 provides the 3rd color bit (for bottom scanlines).
    fn vram_madsel_write(&mut self, offset: u16, data: u8) {
        const DATA_LOOKUP: [u8; 4] = [0x00, 0x0F, 0xF0, 0xFF];

        // 2-bit planar write: VRAM address = offset >> 2
        let vramaddr = (offset >> 2) as usize;
        let pixel = offset & 3;
        let vramdata = DATA_LOOKUP[(data >> 6) as usize];
        let vrammask = !(0x11u8 << pixel);

        let ram = self.map.region_data_mut(Region::Ram);
        if vramaddr < ram.len() {
            ram[vramaddr] = (ram[vramaddr] & vrammask) | (vramdata & !vrammask);
        }

        // 3rd color bit write (MUSHROOM region): offset & 0xE000 == 0xE000
        // Extra cycle penalty: adjust_icount(-1)
        if (offset & 0xE000) == 0xE000 {
            let bit3_addr = Self::get_bit3_addr(offset) as usize;
            let bit3_data: u8 = if data & 0x20 != 0 { 0xFF } else { 0x00 };
            let bit3_mask = !(1u8 << (offset & 7));

            let ram = self.map.region_data_mut(Region::Ram);
            if bit3_addr < ram.len() {
                ram[bit3_addr] = (ram[bit3_addr] & bit3_mask) | (bit3_data & !bit3_mask);
            }
            self.stall_cycles += 1;
        }
    }

    /// MADSEL read: extract pixel color from VRAM and return in bits 7:6 (and bit 5
    /// for 3rd color bit region).
    fn vram_madsel_read(&mut self, offset: u16) -> u8 {
        let vramaddr = (offset >> 2) as usize;
        let vrammask = 0x11u8 << (offset & 3);
        let ram = self.map.region_data(Region::Ram);
        let vramdata = if vramaddr < ram.len() {
            ram[vramaddr] & vrammask
        } else {
            0
        };

        let mut result = 0xFFu8;
        if (vramdata & 0xF0) == 0 {
            result &= !0x80;
        }
        if (vramdata & 0x0F) == 0 {
            result &= !0x40;
        }

        // 3rd color bit read (MUSHROOM region)
        // Extra cycle penalty: adjust_icount(-1)
        if (offset & 0xE000) == 0xE000 {
            let bit3_addr = Self::get_bit3_addr(offset) as usize;
            let bit3_mask = 1u8 << (offset & 7);
            let ram = self.map.region_data(Region::Ram);
            let bit3_data = if bit3_addr < ram.len() {
                ram[bit3_addr] & bit3_mask
            } else {
                0
            };
            if bit3_data == 0 {
                result &= !0x20;
            }
            self.stall_cycles += 1;
        }

        result
    }

    /// Convert a 16-bit pixel address to a VRAM address for the 3rd color bit.
    pub fn get_bit3_addr(pixaddr: u16) -> u16 {
        ((pixaddr & 0x0800) >> 1)
            | ((!pixaddr & 0x0800) >> 2)
            | ((pixaddr & 0x07F8) >> 2)
            | ((pixaddr & 0x1000) >> 12)
    }

    /// Render the full frame directly from VRAM (fallback before first run_frame() completes).
    fn render_frame_from_vram(&self, buffer: &mut [u8]) {
        let (width, height) = TIMING.display_size();
        let w = width as usize;
        let h = height as usize;

        // Resolve palette: each entry has 1-bit per RGB channel (inverted)
        // Bits 3/2/1 = ~R/~G/~B
        let mut palette_rgb = [(0u8, 0u8, 0u8); 8];
        for (i, rgb) in palette_rgb.iter_mut().enumerate() {
            let entry = self.palette[i];
            *rgb = (
                if entry & 0x08 == 0 { 255 } else { 0 }, // R = inverted bit 3
                if entry & 0x04 == 0 { 255 } else { 0 }, // G = inverted bit 2
                if entry & 0x02 == 0 { 255 } else { 0 }, // B = inverted bit 1
            );
        }

        let ram = self.map.region_data(Region::Ram);

        for screen_y in 0..h {
            let effy = screen_y + 25;
            let src_base = effy * 64;

            let bit3_base = if effy >= 224 {
                Some(Self::get_bit3_addr((effy as u16) << 8) as usize)
            } else {
                None
            };

            for screen_x in 0..w {
                let byte_offset = src_base + screen_x / 4;
                let pixel_in_byte = screen_x & 3;

                let byte = if byte_offset < 0x4000 {
                    ram[byte_offset]
                } else {
                    0
                };

                let pix = byte >> pixel_in_byte;
                let mut color_idx = ((pix >> 2) & 4) | ((pix << 1) & 2);

                if let Some(base) = bit3_base {
                    let bit3_offset = base + (screen_x / 8) * 2;
                    if bit3_offset < 0x4000 {
                        color_idx |= (ram[bit3_offset] >> (screen_x & 7)) & 1;
                    }
                }

                let (r, g, b) = palette_rgb[color_idx as usize];

                let pixel_offset = (screen_y * w + screen_x) * 3;
                buffer[pixel_offset] = r;
                buffer[pixel_offset + 1] = g;
                buffer[pixel_offset + 2] = b;
            }
        }
    }

    /// Render a single scanline from VRAM + palette into the internal scanline buffer.
    ///
    /// `effy` is the V counter value (25-255, where 25 = first visible line).
    /// Decodes the 8-entry palette, reads one VRAM row (64 bytes, 4 pixels/byte),
    /// and writes 256 RGB24 pixels to `self.scanline_buffer` at screen_y = effy - 25.
    /// For effy >= 224, the 3rd color bit region is used to enable 8-color output.
    pub fn render_scanline_to_buffer(&mut self, effy: usize) {
        // Resolve palette: each entry has 1-bit per RGB channel (inverted)
        // Bits 3/2/1 = ~R/~G/~B
        let mut palette_rgb = [(0u8, 0u8, 0u8); 8];
        for (i, rgb) in palette_rgb.iter_mut().enumerate() {
            let entry = self.palette[i];
            *rgb = (
                if entry & 0x08 == 0 { 255 } else { 0 }, // R = inverted bit 3
                if entry & 0x04 == 0 { 255 } else { 0 }, // G = inverted bit 2
                if entry & 0x02 == 0 { 255 } else { 0 }, // B = inverted bit 1
            );
        }

        let screen_y = effy - 25;
        let src_base = effy * 64;

        // Compute 3rd color bit base address for bottom scanlines
        let bit3_base = if effy >= 224 {
            Some(Self::get_bit3_addr((effy as u16) << 8) as usize)
        } else {
            None
        };

        let row_offset = screen_y * 256 * 3;

        let ram = self.map.region_data(Region::Ram);

        for screen_x in 0..256 {
            let byte_offset = src_base + screen_x / 4;
            let pixel_in_byte = screen_x & 3;

            let byte = if byte_offset < ram.len() {
                ram[byte_offset]
            } else {
                0
            };

            // Extract 2-bit color from bit-planar format
            let pix = byte >> pixel_in_byte;
            let mut color_idx = ((pix >> 2) & 4) | ((pix << 1) & 2);

            // Add 3rd color bit for bottom scanlines (effy >= 224)
            if let Some(base) = bit3_base {
                let bit3_offset = base + (screen_x / 8) * 2;
                if bit3_offset < ram.len() {
                    color_idx |= (ram[bit3_offset] >> (screen_x & 7)) & 1;
                }
            }

            let (r, g, b) = palette_rgb[color_idx as usize];

            let pixel_offset = row_offset + screen_x * 3;
            self.scanline_buffer[pixel_offset] = r;
            self.scanline_buffer[pixel_offset + 1] = g;
            self.scanline_buffer[pixel_offset + 2] = b;
        }
    }
}

impl Default for MissileCommandSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for MissileCommandBoard {
    fn default() -> Self {
        Self::new()
    }
}

// The board is the bus.
impl Bus for MissileCommandBoard {
    type Address = u16;
    type Data = u8;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false // No DMA hardware on Missile Command
    }

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        // MADSEL check: if active, redirect read to VRAM (bypasses normal decoding)
        if self.get_madsel() {
            return self.vram_madsel_read(addr);
        }

        // 15-bit address bus masking (global_mask 0x7FFF).
        // The 6502 vectors at 0xFFFC map through: 0xFFFC & 0x7FFF = 0x7FFC → ROM.
        let addr = addr & 0x7FFF;

        let data = match self.map.page(addr).region_id {
            Region::RAM | Region::ROM => self.map.read_backing(addr),

            Region::IO => match addr {
                0x4000..=0x47FF => self.pokey.read(addr & 0x0F),
                0x4800..=0x48FF => {
                    if self.ctrld {
                        (self.trackball_y.counter() << 4) | self.trackball_x.counter()
                    } else {
                        self.in0
                    }
                }
                0x4900..=0x49FF => self.in1,
                0x4A00..=0x4AFF => self.dip_switches,
                _ => 0xFF,
            },

            _ => 0xFF,
        };

        // MADSEL arming: during SYNC (opcode fetch), if the opcode has low 5 bits
        // == 0x01 (indirect X addressing mode) and IRQ is not asserted, arm the
        // MADSEL counter. It will fire 5 CPU cycles later.
        if self.cpu_is_sync && (data & 0x1F) == 0x01 && !self.irq_state {
            self.madsel_lastcycles = self.cpu_cycles;
        }

        self.map.watch_read(0, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        // MADSEL check: if active, redirect write to VRAM (bypasses normal decoding)
        if self.get_madsel() {
            self.vram_madsel_write(addr, data);
            return;
        }

        // 15-bit address bus masking
        let addr = addr & 0x7FFF;
        self.map.watch_write(0, master, addr, data);

        match self.map.page(addr).region_id {
            Region::RAM => self.map.write_backing(addr, data),

            Region::IO => match addr {
                0x4000..=0x47FF => self.pokey.write(addr & 0x0F, data),
                0x4800..=0x48FF => {
                    self.ctrld = (data & 1) != 0;
                }
                0x4B00..=0x4BFF => {
                    self.palette[(addr & 0x07) as usize] = data;
                }
                0x4C00..=0x4CFF => {
                    self.watchdog_frame_count = 0;
                }
                0x4D00..=0x4DFF => {
                    self.irq_state = false;
                }
                _ => {}
            },

            _ => {} // ROM and unmapped: writes ignored
        }
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            nmi: false,
            irq: self.irq_state,
            firq: false,
            irq_vector: 0,
            irq_level: 0,
        }
    }
}

impl Renderable for MissileCommandSystem {
    fn display_size(&self) -> (u32, u32) {
        TIMING.display_size()
    }

    fn display_aspect(&self) -> Option<(u32, u32)> {
        TIMING.display_aspect()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        if self.board.scanline_buffer_valid {
            buffer.copy_from_slice(&self.board.scanline_buffer);
        } else {
            self.board.render_frame_from_vram(buffer);
        }
    }
}

impl AudioSource for MissileCommandSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.board.audio_buffer.pop_front_into(buffer)
    }

    fn audio_sample_rate(&self) -> u32 {
        phosphor_core::audio::host_sample_rate()
    }
}

impl InputConfigurable for MissileCommandSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        MISSILE_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Button { id, pressed } => match id.0 as u8 {
                // IN0 switches (active-low: clear bit when pressed, set when released)
                INPUT_COIN => set_bit_active_low(&mut self.board.in0, 5, pressed), // Left Coin
                INPUT_START1 => set_bit_active_low(&mut self.board.in0, 4, pressed), // 1P Start
                INPUT_START2 => set_bit_active_low(&mut self.board.in0, 3, pressed), // 2P Start

                // IN1 fire buttons (active-low: clear bit when pressed, set when released)
                INPUT_FIRE_LEFT => set_bit_active_low(&mut self.board.in1, 2, pressed), // Left fire
                INPUT_FIRE_CENTER => set_bit_active_low(&mut self.board.in1, 1, pressed), // Center fire
                INPUT_FIRE_RIGHT => set_bit_active_low(&mut self.board.in1, 0, pressed), // Right fire
                // IN1 bit 6 = self-test (active-low). The cabinet's switch is on
                // the coin door; holding it enters the option display the
                // operator manual's Figure 6 describes.
                INPUT_SELFTEST => set_bit_active_low(&mut self.board.in1, 6, pressed),

                // Trackball directions
                INPUT_TRACK_L => self.board.trackball_x.set_held(false, pressed),
                INPUT_TRACK_R => self.board.trackball_x.set_held(true, pressed),
                INPUT_TRACK_U => self.board.trackball_y.set_held(false, pressed),
                INPUT_TRACK_D => self.board.trackball_y.set_held(true, pressed),
                _ => {}
            },
            InputEvent::Relative { id, delta } => {
                let delta = delta as i32;
                if id == CTRL_TRACKBALL_X {
                    self.board.trackball_x.add_delta(delta as f32);
                } else if id == CTRL_TRACKBALL_Y {
                    // Y axis inverted: mouse down (positive delta) moves the crosshair
                    // down on screen, but the trackball counter must decrease.
                    self.board.trackball_y.add_delta(-delta as f32);
                }
            }
            InputEvent::Absolute { .. } => {}
        }
    }

    /// Also clears conditioned analog state: the digital releases above cannot
    /// reach accumulated motion or a held deflection.
    fn release_all_inputs(&mut self) {
        phosphor_core::core::machine::release_all_controls(self);
        self.board.trackball_x.release_all();
        self.board.trackball_y.release_all();
    }
}

crate::impl_standalone_debug!(MissileCommandSystem);

impl MachineCore for MissileCommandSystem {
    fn run_frame(&mut self) {
        for _ in 0..TIMING.cycles_per_frame() {
            tick(&mut self.cpu, &mut self.board);
        }
        self.board.scanline_buffer_valid = true;

        // Watchdog: 8-VBLANK timeout. If the game hasn't written
        // to 0x4C00 within 8 frames, reset the machine.
        self.board.watchdog_frame_count += 1;
        if self.board.watchdog_frame_count >= 8 {
            self.reset();
            return;
        }

        // Drain POKEY's resampled f32 buffer and convert to i16 PCM.
        //
        // POKEY's output is unipolar [0.0, 1.0] and sits at *zero* when idle,
        // not at half scale, so the board's coupling capacitor is what centres
        // it. Subtracting a fixed 0.5 instead — as this did — mapped silence to
        // -32767 and pinned the output at the rail for the whole attract mode.
        // The ×2 restores the level that centring on 0.5 was reaching for.
        let samples = self.board.pokey.drain_audio();
        let blocker = &mut self.board.dc_blocker;
        self.board.audio_buffer.extend(samples.iter().map(|&s| {
            (blocker.process(s) * 2.0 * 32767.0).clamp(i16::MIN as f32, i16::MAX as f32) as i16
        }));
    }

    fn reset(&mut self) {
        self.board.irq_state = false;
        self.board.madsel_lastcycles = 0;
        self.board.stall_cycles = 0;
        self.board.cpu_is_sync = true;
        self.board.watchdog_frame_count = 0;
        self.board.scanline_buffer.fill(0);
        self.board.scanline_buffer_valid = false;
        self.board.audio_buffer.clear();
        self.board.dc_blocker.reset();
        // The pots are wiring, not state: a reset must leave the DIP switches
        // still driving them, exactly as the cabinet's do.
        self.board.refresh_dip_pots();

        self.cpu.reset(&mut self.board, BusMaster::Cpu(0));
    }

    fn frame_rate_hz(&self) -> f64 {
        TIMING.frame_rate_hz()
    }

    fn machine_id(&self) -> &str {
        "missile_command"
    }

    crate::machine_clock_declaration!(TIMING, crate::missile_command::clock_tree);
}

impl SaveState for MissileCommandSystem {
    crate::machine_save_state!();
}

// No battery RAM, sub-span profiling, or event tracing; DIP switches are real
// (see the DipSwitches impl below).
crate::impl_default_frontend_capabilities!(MissileCommandSystem);

/// DIP switch metadata for Missile Command's R10 pricing DIP (read at 0x4A00).
/// The separate R8 game-options bank (cities, bonus city, cabinet) is not
/// emulated as a settable byte. Choice bits and labels follow MAME's `missile`
/// R10 layout; option defaults OR to the historical 0x00.
const MISSILE_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "R10 (Pricing)",
        options: &[
            DipOption {
                name: "Coinage",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "1 Coin/1 Credit",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "2 Coins/1 Credit",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "Free Play",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "1 Coin/2 Credits",
                        value: 0x03,
                    },
                ],
            },
            DipOption {
                name: "Right Coin",
                mask: 0x0C,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "x1",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "x4",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "x5",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "x6",
                        value: 0x0C,
                    },
                ],
            },
            DipOption {
                name: "Center Coin",
                mask: 0x10,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "x1",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "x2",
                        value: 0x10,
                    },
                ],
            },
            DipOption {
                name: "Language",
                mask: 0x60,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "English",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "French",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "German",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "Spanish",
                        value: 0x60,
                    },
                ],
            },
            DipOption {
                name: "Unknown",
                mask: 0x80,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "On",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Off",
                        value: 0x80,
                    },
                ],
            },
        ],
    },
    DipSwitchBank {
        name: "R8 (Game Options)",
        options: &[
            DipOption {
                name: "Cities",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "7",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "5",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "4",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "6",
                        value: 0x03,
                    },
                ],
            },
            DipOption {
                name: "Bonus Credit",
                mask: 0x04,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "1 credit for 4 coins",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "None",
                        value: 0x04,
                    },
                ],
            },
            DipOption {
                name: "Trak-Ball Size",
                mask: 0x08,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Large (upright)",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Mini",
                        value: 0x08,
                    },
                ],
            },
            DipOption {
                name: "Bonus City",
                mask: 0x70,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "None",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Every 8,000",
                        value: 0x10,
                    },
                    DipChoice {
                        label: "Every 20,000",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "Every 18,000",
                        value: 0x30,
                    },
                    DipChoice {
                        label: "Every 15,000",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "Every 14,000",
                        value: 0x50,
                    },
                    DipChoice {
                        label: "Every 12,000",
                        value: 0x60,
                    },
                    DipChoice {
                        label: "Every 10,000",
                        value: 0x70,
                    },
                ],
            },
            DipOption {
                name: "Toggle 8 (unused)",
                mask: 0x80,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Off",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "On",
                        value: 0x80,
                    },
                ],
            },
        ],
    },
];

/// Two banks, and only one of them is a plain byte read.
///
/// R10 is read directly at 0x4A00. R8 reaches the CPU through the POKEY's pot
/// inputs, so setting it has to re-drive them — the same shape as `quantum.rs`,
/// which is why this is hand-written rather than `impl_dip_switches!`.
impl DipSwitches for MissileCommandSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        MISSILE_DIP_BANKS
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        match bank {
            0 => self.board.dip_switches,
            1 => self.board.dip_r8,
            _ => 0,
        }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        match bank {
            0 => self.board.dip_switches = value,
            1 => {
                self.board.dip_r8 = value;
                self.board.refresh_dip_pots();
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

crate::register_machine!(
    MissileCommandSystem,
    "missile",
    &["missile"],
    MISSILE_CONTROLS
);

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::DipSwitches;
    use phosphor_core::cpu::CpuStateTrait;

    #[test]
    fn dip_default_and_metadata() {
        let sys = MissileCommandSystem::new();
        assert_eq!(sys.dip_bank_value(0), 0x00);
        assert_eq!(sys.dip_bank_value(1), 0x00);
        crate::assert_dip_banks_valid(
            sys.dip_banks(),
            &[sys.dip_bank_value(0), sys.dip_bank_value(1)],
        );
    }

    #[test]
    fn set_dip_option_masks_only_its_bits() {
        let mut sys = MissileCommandSystem::new();
        // Language is option 3 (mask 0x60); pick "German" (0x40).
        sys.set_dip_option(0, 3, 0x40);
        assert_eq!(sys.dip_bank_value(0), 0x40);
    }

    /// R8 reaches the CPU only through the POKEY's pot inputs, so setting the
    /// bank has to re-drive them. Without that the switches are settable and
    /// have no effect, which is the failure this bank existed to fix.
    #[test]
    fn setting_r8_drives_the_pokey_pots() {
        let mut sys = MissileCommandSystem::new();
        // Undriven default: every pot low.
        for n in 0..8 {
            assert_eq!(sys.board.pokey.pot_input(n), 0x00, "pot {n} at reset");
        }

        // The manual's factory setting: 6 cities and a bonus city every 10,000.
        sys.set_dip_bank_value(1, 0x73);
        for n in 0..8 {
            let expect = if 0x73 & (1 << n) != 0 { 0x80 } else { 0x00 };
            assert_eq!(sys.board.pokey.pot_input(n), expect, "pot {n} after set");
        }

        // And a reset must not drop the wiring on the floor.
        sys.reset();
        assert_eq!(sys.board.pokey.pot_input(0), 0x80);
        assert_eq!(sys.board.pokey.pot_input(4), 0x80);
        assert_eq!(sys.board.pokey.pot_input(3), 0x00);
    }

    /// The two banks do not share a bit sense, and the table encodes that.
    ///
    /// Both figures below were read off the game's own self-test screen: 0x00
    /// shows `7 CITIES` with no bonus-city line, and 0x73 shows `6 CITIES` and
    /// `BONUS CITY EVERY 10000 POINTS`, which is what the operator manual marks
    /// as the factory setting.
    #[test]
    fn r8_choices_match_the_self_test_display() {
        let banks = MISSILE_DIP_BANKS;
        let r8 = &banks[1];
        assert_eq!(r8.name, "R8 (Game Options)");

        let choice = |option: &str, label: &str| -> u8 {
            let o = r8.options.iter().find(|o| o.name == option).unwrap();
            o.choices.iter().find(|c| c.label == label).unwrap().value
        };
        // Undriven pots read 0, and the game calls that 7 cities and no bonus.
        assert_eq!(choice("Cities", "7"), 0x00);
        assert_eq!(choice("Bonus City", "None"), 0x00);
        // The factory setting the manual marks, and the screen confirmed.
        assert_eq!(
            choice("Cities", "6") | choice("Bonus City", "Every 10,000"),
            0x73
        );
        // Toggle 4 open is the large upright Trak-Ball, which the manual says
        // the switch "must be off" for. That falls out of the default.
        assert_eq!(choice("Trak-Ball Size", "Large (upright)"), 0x00);
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = MissileCommandSystem::new();

        // Set known state
        sys.board.map.region_data_mut(Region::Ram)[0x100] = 0xAA;
        sys.board.in0 = 0xEE;
        sys.board.in1 = 0x77;
        sys.board.ctrld = true;
        sys.board.palette[3] = 0x0E;
        sys.board.trackball_x.set_counter(7);
        sys.board.trackball_y.set_counter(12);
        sys.board.trackball_x.set_held(false, true);
        sys.board.trackball_y.set_held(true, true);
        sys.board.trackball_x.add_delta(-7.0);
        sys.board.trackball_y.add_delta(12.0);
        sys.board.irq_state = true;
        sys.board.madsel_lastcycles = 42;
        sys.board.stall_cycles = 1;
        sys.board.clock = 100_000;
        sys.board.cpu_cycles = 80_000;
        sys.board.watchdog_frame_count = 5;

        // Save
        let data = SaveState::save_state(&sys).expect("save_state should return Some");
        let cpu_snap = sys.cpu.snapshot();

        // Mutate everything
        let mut sys2 = MissileCommandSystem::new();
        sys2.board.map.region_data_mut(Region::Ram)[0x100] = 0xFF;
        sys2.board.clock = 999;

        // Load
        SaveState::load_state(&mut sys2, &data).unwrap();

        // Verify
        assert_eq!(sys2.cpu.snapshot(), cpu_snap);
        assert_eq!(sys2.board.map.region_data_mut(Region::Ram)[0x100], 0xAA);
        assert_eq!(sys2.board.in0, 0xEE);
        assert_eq!(sys2.board.in1, 0x77);
        assert!(sys2.board.ctrld);
        assert_eq!(sys2.board.palette[3], 0x0E);
        assert_eq!(sys2.board.trackball_x.counter(), 7);
        assert_eq!(sys2.board.trackball_y.counter(), 12);
        // Held keys and pending motion round-trip too: one tick of each
        // reproduces the pre-save step (key -1 plus drained -1 on X).
        sys2.board.trackball_x.update();
        sys2.board.trackball_y.update();
        assert_eq!(sys2.board.trackball_x.counter(), 5);
        assert_eq!(sys2.board.trackball_y.counter(), 14);
        assert!(sys2.board.irq_state);
        assert_eq!(sys2.board.madsel_lastcycles, 42);
        assert_eq!(sys2.board.stall_cycles, 1);
        assert_eq!(sys2.board.clock, 100_000);
        assert_eq!(sys2.board.cpu_cycles, 80_000);
        assert_eq!(sys2.board.watchdog_frame_count, 5);
        assert!(!sys2.board.scanline_buffer_valid);
    }

    #[test]
    fn save_does_not_include_rom() {
        let mut sys = MissileCommandSystem::new();
        sys.board.map.region_data_mut(Region::Rom)[0] = 0xDE;

        let data = SaveState::save_state(&sys).unwrap();

        let mut sys2 = MissileCommandSystem::new();
        SaveState::load_state(&mut sys2, &data).unwrap();

        assert_eq!(sys2.board.map.region_data_mut(Region::Rom)[0], 0x00);
    }

    #[test]
    fn set_analog_accumulates_x() {
        let mut sys = MissileCommandSystem::new();
        sys.handle_input(InputEvent::Relative {
            id: CTRL_TRACKBALL_X,
            delta: (3) as f32,
        });
        // Counter unchanged until tick() drains the pending motion.
        assert_eq!(sys.board.trackball_x.counter(), 0);
        for _ in 0..3000 {
            sys.step_cycle();
            sys.board.clock += 1;
        }
        assert_eq!(sys.board.trackball_x.counter(), 3);
    }

    #[test]
    fn set_analog_accumulates_y_inverted() {
        let mut sys = MissileCommandSystem::new();
        sys.handle_input(InputEvent::Relative {
            id: CTRL_TRACKBALL_Y,
            delta: (-5) as f32,
        });
        // Y axis is inverted: a negative mouse delta drives the counter up.
        for _ in 0..5000 {
            sys.step_cycle();
            sys.board.clock += 1;
        }
        assert_eq!(sys.board.trackball_y.counter(), 5);
    }

    #[test]
    fn tick_drains_mouse_accum_positive() {
        let mut sys = MissileCommandSystem::new();
        sys.board.trackball_x.add_delta(3.0);
        // Run enough ticks to drain (tick fires every 1000 cycles)
        for _ in 0..3000 {
            sys.step_cycle();
            sys.board.clock += 1;
        }
        assert_eq!(sys.board.trackball_x.counter(), 3);
        // Drained: further ticks do not move it.
        for _ in 0..3000 {
            sys.step_cycle();
            sys.board.clock += 1;
        }
        assert_eq!(sys.board.trackball_x.counter(), 3);
    }

    #[test]
    fn tick_drains_mouse_accum_negative() {
        let mut sys = MissileCommandSystem::new();
        sys.board.trackball_y.set_counter(5);
        sys.board.trackball_y.add_delta(-3.0);
        for _ in 0..3000 {
            sys.step_cycle();
            sys.board.clock += 1;
        }
        assert_eq!(sys.board.trackball_y.counter(), 2);
    }

    #[test]
    fn tick_drains_mouse_accum_wraps_4_bit() {
        let mut sys = MissileCommandSystem::new();
        sys.board.trackball_x.set_counter(14);
        sys.board.trackball_x.add_delta(5.0);
        for _ in 0..5000 {
            sys.step_cycle();
            sys.board.clock += 1;
        }
        assert_eq!(sys.board.trackball_x.counter(), 3); // (14 + 5) & 0x0F = 3
    }

    #[test]
    fn exposes_two_analog_axes() {
        let sys = MissileCommandSystem::new();
        let axes: Vec<&str> = sys
            .input_controls()
            .iter()
            .filter(|c| matches!(c.kind, InputKind::AnalogAxis { .. }))
            .map(|c| c.label)
            .collect();
        assert_eq!(axes, vec!["Trackball X", "Trackball Y"]);
    }

    #[test]
    fn handle_input_relative_accumulates_like_set_analog() {
        // Typed Relative events feed the same accumulators as set_analog,
        // including the Y inversion.
        let mut sys = MissileCommandSystem::new();
        sys.handle_input(InputEvent::Relative {
            id: CTRL_TRACKBALL_X,
            delta: 3.0,
        });
        sys.handle_input(InputEvent::Relative {
            id: CTRL_TRACKBALL_Y,
            delta: -5.0,
        });
        for _ in 0..6000 {
            sys.step_cycle();
            sys.board.clock += 1;
        }
        assert_eq!(sys.board.trackball_x.counter(), 3);
        assert_eq!(sys.board.trackball_y.counter(), 5);
    }

    #[test]
    fn handle_input_button_matches_set_input() {
        let mut typed = MissileCommandSystem::new();
        typed.handle_input(InputEvent::Button {
            id: InputId(INPUT_FIRE_CENTER as u16),
            pressed: true,
        });
        let mut legacy = MissileCommandSystem::new();
        legacy.handle_input(InputEvent::Button {
            id: InputId((INPUT_FIRE_CENTER) as u16),
            pressed: true,
        });
        // Active-low: pressing clears IN1 bit 1.
        assert_eq!(typed.board.in1 & 0b10, 0);
        assert_eq!(typed.board.in1, legacy.board.in1);
    }

    #[test]
    fn input_controls_include_analog_axes() {
        let sys = MissileCommandSystem::new();
        let controls = sys.input_controls();
        assert_eq!(controls.len(), 13); // 10 digital + self-test + 2 analog axes
        assert!(
            controls.iter().any(|c| c.stable_name == "trackball_x"
                && matches!(c.kind, InputKind::AnalogAxis { .. }))
        );
        assert!(
            controls
                .iter()
                .any(|c| c.stable_name == "self_test" && matches!(c.kind, InputKind::Service))
        );
    }
}
