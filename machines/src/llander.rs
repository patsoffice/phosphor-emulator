//! Lunar Lander (Atari, 1979), on the shared Atari DVG vector board.
//!
//! # Schematics
//!
//! | Drawing | Source | Pages |
//! |---|---|---|
//! | `POWER INPUTS AND OUTPUTS 034230-XX A`, sheet 1 side B | `arcade-museum.com/manuals-videogames/L/Lunar-Lander-DP136-3rd-Printing-Missing-Sheet-01-Side-A.pdf` | PDF p1 |
//! | `VECTOR GENERATOR SCHEMATIC 034230-XX A`, sheet 2 sides A and B | same | PDF p2, p3 |
//!
//! Two blocks have been read. The audio block on sheet 1 side B is transcribed
//! in
//! [`docs/schematics/llander-audio-output.md`](../../docs/schematics/llander-audio-output.md).
//! `OPTIONS INPUT CIRCUITRY`, top left of sheet 2 side A, is the source for the
//! DSW1 decode at 0x2800-0x2803: an LS253 at N8 with the 8-position switch SW2
//! at P8 wired toggle 1 to 1C3, 3 to 1C2, 5 to 1C1 and 7 to 1C0 (all reaching
//! DB0 through 1Y), and toggle 2 to 2C3, 4 to 2C2, 6 to 2C1 and 8 to 2C0
//! (reaching DB1). Since a '253 selects Cn with n = (B,A) = (AB1,AB0), the
//! pairs count DOWN: 0x2800 reads toggles 7-8 and 0x2803 reads toggles 1-2. The
//! block also states that toggle inputs are on when pulled to ground, which is
//! why an ON toggle reads 0. This wiring is identical to Asteroids', but it was
//! read here rather than assumed to carry over.
//!
//! The vector generator sheets were not read.
//!
//! **Sheet 1 side A is missing from this scan**, as its filename says. That is
//! the sheet carrying the address decode, so the `AUDIO` strobe that clocks the
//! sound latch and the `0x3E00` noise-reset strobe are known from the memory map
//! and not from any drawing. Do not go looking for them in this PDF.
//!
//! # Manual
//!
//! | Document | Source | Pages |
//! |---|---|---|
//! | `Operation, Maintenance and Service Manual` TM-136 1st printing | `arcade-museum.com/manuals-videogames/L/Lunar-Lander-TM136-1st-Printing.pdf` | PDF p15 (printed p10) |
//!
//! `Operator Option Switch Settings` on that page describes the toggles, but it
//! does **not** describe this ROM and [`LLANDER_DIP_BANKS`] deliberately
//! disagrees with it. The 1st printing lists four fuel amounts on toggles 7-8
//! and marks toggle 6 unused; the game's own option routine reads the toggle
//! 5-6 pair, rotates it left twice and merges it into the toggle 7-8 pair, so
//! the fuel amount is three toggles and eight values and free play is toggle 6.
//! Believe the routine.

use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    ActionRole, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, InputConfigurable,
    InputControl, InputEvent, InputId, InputKind, MachineCore, SaveState,
};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_macros::Saveable;

use crate::atari_dvg::{self, AtariDvgBoard, AtariDvgBus, DSW_UNDRIVEN, Region};
use crate::llander_sound::LunarLanderDiscreteSound;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::{set_bit_active_high, set_bit_active_low};
use phosphor_core::cpu::m6502::M6502;

// ---------------------------------------------------------------------------
// ROM definitions (MAME `llander` set, revision 2)
// ---------------------------------------------------------------------------

/// Program ROM: 8KB at CPU addresses 0x6000–0x7FFF.
static PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "034572-02.f1",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0xb8763eea],
        },
        RomEntry {
            name: "034571-02.de1",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x77da4b2f],
        },
        RomEntry {
            name: "034570-01.c1",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0x2724e591],
        },
        RomEntry {
            name: "034569-02.b1",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0x72837a4e],
        },
    ],
};

/// Vector ROM: 6KB at CPU addresses 0x4800–0x5FFF.
static VECTOR_ROM: RomRegion = RomRegion {
    size: 0x1800,
    entries: &[
        RomEntry {
            name: "034599-01.r3",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0x355a9371],
        },
        RomEntry {
            name: "034598-01.np3",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x9c4ffa68],
        },
        RomEntry {
            name: "034597-01.m3",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0xebb744f2],
        },
    ],
};

// ---------------------------------------------------------------------------
// Input button IDs
// ---------------------------------------------------------------------------

pub const INPUT_COIN: u8 = 0;
pub const INPUT_START: u8 = 1;
pub const INPUT_SELECT: u8 = 2;
pub const INPUT_ABORT: u8 = 3;
pub const INPUT_ROT_LEFT: u8 = 4;
pub const INPUT_ROT_RIGHT: u8 = 5;
pub const INPUT_THRUST: u8 = 6;

/// Typed logical controls. `InputId`s reuse the `INPUT_*` numbering; default
/// bindings mirror the legacy name-matched defaults (abort = "P1 Fire", thrust
/// = "P1 Up", rotate = "P1 Left/Right"). The Select button had no legacy
/// default binding (now rebindable via the settings UI).
const LLANDER_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_COIN as u16),
        stable_name: "coin",
        label: "Coin",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(INPUT_START as u16),
        stable_name: "p1_start",
        label: "P1 Start",
        kind: InputKind::Start,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_START,
    },
    InputControl {
        id: InputId(INPUT_SELECT as u16),
        stable_name: "select",
        label: "Select",
        kind: InputKind::Action(ActionRole::Secondary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_ABORT as u16),
        stable_name: "abort",
        label: "Abort / Fire",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_ROT_LEFT as u16),
        stable_name: "rotate_left",
        label: "Rotate Left",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(INPUT_ROT_RIGHT as u16),
        stable_name: "rotate_right",
        label: "Rotate Right",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
    InputControl {
        id: InputId(INPUT_THRUST as u16),
        stable_name: "thrust",
        label: "Thrust",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_UP,
    },
];

// ---------------------------------------------------------------------------
// LunarLanderSystem — Atari DVG board configured for Lunar Lander (1979)
// ---------------------------------------------------------------------------

/// Lunar Lander-specific wrapper around the shared Atari DVG board.
///
/// Features an analog thrust pedal (mapped to digital button) and mission
/// lamp outputs. Lunar Lander uses a flat IN0 byte read (not multiplexed)
/// unlike Asteroids which uses an 8:1 multiplexer.
///
/// Memory map (15-bit address bus, `addr & 0x7FFF`):
///   0x0000–0x00FF  RAM (256 bytes, mirrored)
///   0x2000         IN0 read (flat byte: VG_HALT, service, tilt, clock)
///   0x2400–0x2407  IN1 read (coins, start, select, abort, rotate)
///   0x2800–0x2803  DSW1 read (DIP switches)
///   0x2C00         Thrust pedal read (analog, 0x00–0xFE)
///   0x3000         DVG GO write
///   0x3200         Output latch write (mission lamps)
///   0x3400         Watchdog reset write
///   0x3C00         Sound register write (thrust/tones/explosion)
///   0x3E00         Noise reset write
///   0x4000–0x47FF  Vector RAM (2 KB, shared CPU/DVG)
///   0x4800–0x5FFF  Vector ROM (6 KB)
///   0x6000–0x7FFF  Program ROM (8 KB)
#[derive(Saveable, phosphor_macros::BusDebug)]
pub struct LunarLanderSystem {
    /// The 6502 is held beside the bus view over the board.
    #[debug_cpu("M6502")]
    pub cpu: M6502,

    #[debug_bus]
    pub board: AtariDvgBoard,

    /// Discrete analog sound (thrust/explosion/3 KHz + 6 KHz tones).
    sound: LunarLanderDiscreteSound,

    // I/O — Lunar Lander uses mixed active-HIGH/LOW inputs.
    // in0: active-LOW bits 1,2,3,4,5,7 idle HIGH; bits 0,6 are dynamic.
    in0: u8,
    // in1: active-LOW bits 1,3 idle HIGH; others idle LOW.
    in1: u8,
    /// DIP switches (P8): default 0x80 (English, 750 fuel/coin).
    #[save_skip]
    dip_switches: u8,

    // Thrust pedal: analog value 0x00–0xFE, read by the game at 0x2C00.
    thrust_value: u8,
    // Target the pedal ramps toward: 0xFE while the thrust key is held, 0x00 when
    // released. Live input, re-derived from key state, so it isn't saved.
    #[save_skip]
    thrust_target: u8,
}

/// Per-frame step the thrust pedal ramps toward its target. The game reads the
/// pedal through a self-centering DAC/counter and mis-tracks an instant
/// 0x00↔0xFE step (leaving thrust stuck "on"), so the digital key is smoothed
/// into a gradual pedal sweep, mirroring the real analog control.
const THRUST_RAMP: u8 = 12;

impl LunarLanderSystem {
    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        // Lunar Lander has only 256 bytes of RAM, mirrored throughout 0x0000–0x01FF.
        map.region(Region::Ram, "RAM", 0x0000, 0x0100, AccessKind::ReadWrite)
            .mirror(0x0100, 0x0000, 0x0100)
            .region(Region::Io, "I/O", 0x2000, 0x2000, AccessKind::Io)
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
                0x1800,
                AccessKind::ReadOnly,
            )
            .region(
                Region::ProgramRom,
                "Program ROM",
                0x6000,
                0x2000,
                AccessKind::ReadOnly,
            );
        map
    }

    pub fn new() -> Self {
        Self {
            cpu: M6502::new(),
            // Lunar Lander: VROM at DVG 0x0800, size 0x1800
            board: AtariDvgBoard::new(Self::build_map(), 0x0800, 0x1800),
            sound: LunarLanderDiscreteSound::new(),
            // Active-LOW bits idle HIGH: IN0 bits 1,2,3,4,5,7
            in0: 0xBE,
            // Active-LOW bits idle HIGH: IN1 bits 1,3
            in1: 0x0A,
            dip_switches: 0x80,
            thrust_value: 0x00,
            thrust_target: 0x00,
        }
    }

    /// Borrow the CPU and the bus it drives as two disjoint pieces.
    #[inline]
    fn split(&mut self) -> (&mut M6502, LunarLanderBus<'_>) {
        (
            &mut self.cpu,
            LunarLanderBus {
                board: &mut self.board,
                sound: &mut self.sound,
                in0: self.in0,
                in1: self.in1,
                dip_switches: self.dip_switches,
                thrust_value: self.thrust_value,
            },
        )
    }

    /// One CPU cycle. Returns 1 at an instruction boundary (for the debugger,
    /// which steps instructions rather than cycles).
    pub fn step_cycle(&mut self) -> u32 {
        let (cpu, mut bus) = self.split();
        atari_dvg::tick(cpu, &mut bus);
        AtariDvgBoard::instruction_boundaries(&self.cpu)
    }

    /// Read the CPU-facing bus, side effects and all. Distinct from the
    /// debugger's `BusDebug::peek`/`poke`, which avoid side effects.
    pub fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        self.split().1.read(master, addr)
    }

    /// Write the CPU-facing bus, side effects and all. See [`Self::bus_read`].
    pub fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.split().1.write(master, addr, data);
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let prog = PROGRAM_ROM.load(rom_set)?;
        self.board.map.load_region(Region::ProgramRom, &prog);
        let vrom = VECTOR_ROM.load(rom_set)?;
        self.board.map.load_region(Region::VectorRom, &vrom);
        Ok(())
    }
}

impl Default for LunarLanderSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus implementation
// ---------------------------------------------------------------------------

/// The Lunar Lander bus: the shared DVG board plus this game's I/O -- the input
/// multiplexers, the DIP switches, the analog thrust pedal and the discrete
/// sound board.
struct LunarLanderBus<'a> {
    board: &'a mut AtariDvgBoard,
    sound: &'a mut LunarLanderDiscreteSound,
    in0: u8,
    in1: u8,
    dip_switches: u8,
    thrust_value: u8,
}

impl AtariDvgBus for LunarLanderBus<'_> {
    #[inline]
    fn board(&mut self) -> &mut AtariDvgBoard {
        self.board
    }

    /// Drive TEST, which gates the 250 Hz NMI off while the self-test switch is
    /// on. IN0 bit 1 is the switch, active LOW on this board.
    #[inline]
    fn begin_cycle(&mut self) {
        self.board.test_asserted = self.in0 & 0x02 == 0;
    }
}

impl Bus for LunarLanderBus<'_> {
    type Address = u16;
    type Data = u8;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let addr = addr & 0x7FFF; // 15-bit address bus

        let data = match self.board.map.page(addr).region_id {
            Region::RAM | Region::VECTOR_RAM | Region::VECTOR_ROM | Region::PROGRAM_ROM => {
                self.board.map.read_backing(addr)
            }

            Region::IO => match addr {
                // IN0: 0x2000 — flat byte read (not multiplexed like Asteroids).
                //   Bit 0: VG_HALT (1 = done)
                //   Bit 1: Service switch (active-LOW)
                //   Bit 2: Tilt (active-LOW)
                //   Bit 3-5: unused (active-LOW)
                //   Bit 6: 3 KHz clock
                //   Bit 7: Diagnostic step (active-LOW)
                0x2000 => {
                    let mut val = self.in0;
                    // Bit 0: VG_HALT (1 = halted/done)
                    if self.board.dvg.is_halted() {
                        val |= 0x01;
                    } else {
                        val &= !0x01;
                    }
                    // Bit 6: 3 KHz clock
                    if self.board.clock & 0x100 != 0 {
                        val |= 0x40;
                    } else {
                        val &= !0x40;
                    }
                    val
                }

                // IN1: 0x2400–0x2407 — 74LS251 8:1 multiplexer.
                //   Bit 0: Start (active-HIGH)
                //   Bit 1: Coin1 (active-LOW)
                //   Bit 2: Coin2 (active-HIGH)
                //   Bit 3: Coin3 (active-LOW)
                //   Bit 4: Select (active-HIGH)
                //   Bit 5: Abort (active-HIGH)
                //   Bit 6: Rotate right (active-HIGH)
                //   Bit 7: Rotate left (active-HIGH)
                0x2400..=0x2407 => {
                    let offset = (addr & 7) as u8;
                    ((self.in1 >> offset) & 1) << 7
                }

                // DSW1: 0x2800-0x2803, a 74LS253 dual 4:1 multiplexer reading
                // the option bank two toggles at a time.
                //
                // Each read puts the odd toggle of a pair on DB0 and the even
                // toggle on DB1, and AB0/AB1 count the pairs DOWNWARDS: 0x2800
                // selects toggles 7-8 and 0x2803 selects toggles 1-2. So byte
                // bit n carries toggle n+1.
                //
                // This used to return the second toggle of each pair on DB7 and
                // count the pairs upwards. Every ROM site that reads one of
                // these addresses masks with `AND #$03`, so the DB7 half was
                // discarded and every EVEN toggle reached nothing; the ascending
                // count then swapped the first option with the last. The same
                // expression was wrong the same two ways on the Deluxe board,
                // where the corrected decode was measured against the running
                // game option by option.
                0x2800..=0x2803 => {
                    let pair = 6 - (addr & 3) as u8 * 2;
                    DSW_UNDRIVEN | ((self.dip_switches >> pair) & 0x03)
                }

                // Thrust pedal: 0x2C00 — analog value 0x00–0xFE.
                0x2C00 => self.thrust_value,

                _ => 0,
            },

            _ => 0,
        };

        self.board.map.watch_read(0, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        let addr = addr & 0x7FFF; // 15-bit address bus

        self.board.map.watch_write(0, master, addr, data);
        self.board.trace_main_write(addr, data);

        match self.board.map.page(addr).region_id {
            Region::RAM | Region::VECTOR_RAM => self.board.map.write_backing(addr, data),

            Region::IO => match addr {
                0x3000 => self.board.trigger_dvg(),
                0x3200 => { /* output latch: mission lamps stub */ }
                0x3400 => self.board.watchdog_frame_count = 0,
                // Sound register: bits 0-2 thrust volume, bit 3 explosion,
                // bit 4 3KHz tone, bit 5 6KHz tone
                0x3C00 => self.sound.write_sound_register(data),
                0x3E00 => self.sound.pulse_noise_reset(),
                _ => {}
            },

            _ => {}
        }
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            nmi: self.board.nmi_pending,
            irq: false,
            firq: false,
            irq_vector: 0,
            irq_level: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Machine traits (MachineCore + capabilities)
// ---------------------------------------------------------------------------

// Renderable + MachineDebug delegate to the shared board; audio is owned by the
// game wrapper's discrete sound device, so AudioSource is hand-written.
crate::impl_board_renderable!(LunarLanderSystem, board, atari_dvg::TIMING, vectors);
crate::impl_board_debug!(LunarLanderSystem, board, atari_dvg::TIMING);

impl phosphor_core::core::machine::AudioSource for LunarLanderSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.sound.fill_audio(buffer)
    }
    fn audio_sample_rate(&self) -> u32 {
        self.sound.sample_rate()
    }
}

impl InputConfigurable for LunarLanderSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        LLANDER_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let InputEvent::Button { id, pressed } = event else {
            return;
        };
        match id.0 as u8 {
            // IN1
            INPUT_COIN => set_bit_active_low(&mut self.in1, 1, pressed),
            INPUT_START => set_bit_active_high(&mut self.in1, 0, pressed),
            INPUT_SELECT => set_bit_active_high(&mut self.in1, 4, pressed),
            INPUT_ABORT => set_bit_active_high(&mut self.in1, 5, pressed),
            INPUT_ROT_RIGHT => set_bit_active_high(&mut self.in1, 6, pressed),
            INPUT_ROT_LEFT => set_bit_active_high(&mut self.in1, 7, pressed),

            // Thrust pedal: set the ramp target; run_frame sweeps thrust_value
            // toward it so the game's analog read sees a gradual pedal, not a step.
            INPUT_THRUST => {
                self.thrust_target = if pressed { 0xFE } else { 0x00 };
            }

            _ => {}
        }
    }
}

impl MachineCore for LunarLanderSystem {
    crate::machine_core_metadata!("llander", atari_dvg::TIMING, atari_dvg::clock_tree);

    fn run_frame(&mut self) {
        // Sweep the thrust pedal toward its target before the CPU reads it, so a
        // held/released key presents as a gradual analog ramp (see THRUST_RAMP).
        self.thrust_value = if self.thrust_value < self.thrust_target {
            self.thrust_value
                .saturating_add(THRUST_RAMP)
                .min(self.thrust_target)
        } else {
            self.thrust_value
                .saturating_sub(THRUST_RAMP)
                .max(self.thrust_target)
        };

        let (cpu, mut bus) = self.split();
        atari_dvg::run_frame(cpu, &mut bus);

        // Advance the discrete sound circuit for the frame's worth of CPU cycles.
        self.sound.tick(atari_dvg::TIMING.cycles_per_frame());

        // Clear NMI at frame boundary to avoid stale edges.
        self.board.nmi_pending = false;

        // Watchdog
        self.board.watchdog_frame_count += 1;
        if self.board.watchdog_frame_count >= 8 {
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.board.reset();
        self.sound.reset();
        let (cpu, mut bus) = self.split();
        cpu.reset(&mut bus, BusMaster::Cpu(0));
    }
}

impl SaveState for LunarLanderSystem {
    crate::machine_save_state!();
}

impl phosphor_core::core::machine::Nvram for LunarLanderSystem {}
impl phosphor_core::core::machine::Profilable for LunarLanderSystem {}
/// DIP switch metadata for Lunar Lander's DSW1 byte, the 8-toggle switch at
/// PCB position P8, read two toggles at a time through the 74LS253 mux at
/// 0x2800-0x2803.
///
/// Byte bit *n* carries toggle *n+1*, and a toggle reads 0 when it is ON
/// (closed to ground) and 1 when OFF.
///
/// The default 0x80 is 750 fuel units per coin, normal coinage, English, and
/// the right mech registering one credit per coin.
///
/// `Operator Option Switch Settings` (TM-136 1st printing, printed page 10)
/// describes only four fuel amounts on toggles 7-8 and marks toggle 6 unused.
/// **Do not follow it here.** The game's own option routine reads the toggle
/// 5-6 pair, rotates it left twice and merges it into the toggle 7-8 pair, so
/// the fuel amount is three toggles and eight values, and free play is toggle
/// 6. The 1st-printing table does not describe what this ROM does.
const LLANDER_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSW1",
        options: &[
            DipOption {
                name: "Right Coin",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "x1",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "x4",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "x5",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "x6",
                        value: 0x03,
                    },
                ],
            },
            DipOption {
                name: "Language",
                mask: 0x0C,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "English",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "French",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "Spanish",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "German",
                        value: 0x0C,
                    },
                ],
            },
            // Toggle 6.
            DipOption {
                name: "Coinage",
                mask: 0x20,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Normal",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Free Play",
                        value: 0x20,
                    },
                ],
            },
            // Toggles 5, 7 and 8, which is why the mask is not contiguous: the
            // fuel amount is three toggles with the free-play toggle sitting in
            // the middle of them. The game combines them by rotating the toggle
            // 5-6 read left twice and merging it with the toggle 7-8 read, so
            // toggle 5 contributes the high step and 7-8 the low two.
            DipOption {
                name: "Fuel Units Per Coin",
                mask: 0xD0,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "450",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "600",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "750",
                        value: 0x80,
                    },
                    DipChoice {
                        label: "900",
                        value: 0xC0,
                    },
                    DipChoice {
                        label: "1100",
                        value: 0x10,
                    },
                    DipChoice {
                        label: "1300",
                        value: 0x50,
                    },
                    DipChoice {
                        label: "1550",
                        value: 0x90,
                    },
                    DipChoice {
                        label: "1800",
                        value: 0xD0,
                    },
                ],
            },
        ],
    },
    LLANDER_SERVICE_BANK,
];

/// The self-test switch, which is not one of the eight option toggles.
///
/// It is a maintained slide switch on a bracket inside the coin door, read with
/// the player inputs on IN0 bit 1 rather than through the option port. It
/// belongs beside the option bank because it is a switch an operator sets and
/// leaves set, not a button: the self-test screen is where the option toggles
/// are read back, so it is held on while they are adjusted.
///
/// This bit is active LOW, so the choice values are inverted against the
/// Asteroids and Deluxe boards, whose switch is IN0 bit 7 and active HIGH.
const LLANDER_SERVICE_BANK: DipSwitchBank = DipSwitchBank {
    name: "Service",
    options: &[DipOption {
        name: "Self-Test",
        mask: 0x02,
        apply: DipApplyTiming::Immediate,
        choices: &[
            DipChoice {
                label: "Off",
                value: 0x02,
            },
            DipChoice {
                label: "On",
                value: 0x00,
            },
        ],
    }],
};

crate::impl_dip_switches!(
    LunarLanderSystem,
    LLANDER_DIP_BANKS,
    dip_switches & 0xFF,
    in0 & 0x02,
);
crate::impl_board_debug_trace!(LunarLanderSystem, board);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

crate::register_machine!(LunarLanderSystem, "llander", &["llander"], LLANDER_CONTROLS);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atari_dvg::Region;
    use phosphor_core::core::machine::DipSwitches;
    use phosphor_core::cpu::CpuStateTrait;

    #[test]
    fn dip_default_and_metadata() {
        let sys = LunarLanderSystem::new();
        assert_eq!(sys.dip_bank_value(0), 0x80); // 750 fuel units per coin
        crate::assert_dip_banks_valid(
            sys.dip_banks(),
            &[sys.dip_bank_value(0), sys.dip_bank_value(1)],
        );
        // The self-test switch is active LOW and powers on released.
        assert_eq!(sys.dip_bank_value(1), 0x02);
    }

    #[test]
    fn set_dip_option_masks_only_its_bits() {
        let mut sys = LunarLanderSystem::new();
        // Language is option 1 (mask 0x0C); pick "German" (0x0C). The Fuel bit
        // 0x80 must be preserved.
        sys.set_dip_option(0, 1, 0x0C);
        assert_eq!(sys.dip_bank_value(0), 0x8C);
    }

    /// The option mux hands the CPU two toggles per read, odd toggle on DB0 and
    /// even on DB1, with the pairs counted DOWN from the top of the byte.
    ///
    /// The pattern distinguishes both ways this decode has been wrong. `0xE4`
    /// holds 3, 2, 1, 0 in its four pairs from the top down, so the four
    /// addresses must read 3, 2, 1, 0:
    ///
    /// * returning the second toggle of a pair on DB7 instead of DB1 makes
    ///   every read 0x00 or 0x81, because the ROM masks with `AND #$03`;
    /// * counting the pairs upwards reads 0, 1, 2, 3, which swaps the fuel
    ///   setting with the right coin mech end for end.
    ///
    /// The six bits the mux never drives float high, so every read carries
    /// `0xFC`. That is load-bearing on this machine rather than cosmetic: the
    /// fuel routine reads 0x2800 without masking it.
    #[test]
    fn the_dip_mux_reads_pairs_from_the_top_of_the_byte_down() {
        let mut sys = LunarLanderSystem::new();
        sys.set_dip_bank_value(0, 0b11_10_01_00);
        for (offset, expect) in [(0, 3), (1, 2), (2, 1), (3, 0)] {
            let got = sys.bus_read(BusMaster::Cpu(0), 0x2800 + offset);
            assert_eq!(
                got,
                DSW_UNDRIVEN | expect,
                "read of 0x{:04X}",
                0x2800 + offset
            );
        }

        // Only DB0 and DB1 carry a toggle; the rest read high either way.
        sys.set_dip_bank_value(0, 0x00);
        for offset in 0..4 {
            let got = sys.bus_read(BusMaster::Cpu(0), 0x2800 + offset);
            assert_eq!(
                got,
                DSW_UNDRIVEN,
                "read of 0x{:04X} with every toggle clear",
                0x2800 + offset
            );
        }
    }

    /// The option layout follows the game's own routine, not the 1st-printing
    /// manual: free play is toggle 6, and the fuel amount spans toggles 5, 7
    /// and 8, which is why its mask is not contiguous and it has eight choices.
    #[test]
    fn the_fuel_option_spans_three_toggles_around_the_free_play_one() {
        let banks = LunarLanderSystem::new().dip_banks();
        let by_name = |name: &str| {
            banks[0]
                .options
                .iter()
                .find(|o| o.name == name)
                .unwrap_or_else(|| panic!("no {name} option"))
        };
        assert_eq!(by_name("Coinage").mask, 0x20, "free play is toggle 6");
        let fuel = by_name("Fuel Units Per Coin");
        assert_eq!(fuel.mask, 0xD0, "fuel is toggles 5, 7 and 8");
        assert_eq!(fuel.choices.len(), 8);
        assert_eq!(
            fuel.mask & by_name("Coinage").mask,
            0,
            "the free-play toggle sits between the fuel toggles without overlapping"
        );
    }

    /// The power-on byte is 750 fuel units per coin, normal coinage, English,
    /// and the right mech registering one credit per coin.
    #[test]
    fn the_default_byte_reads_back_as_its_options() {
        let mut sys = LunarLanderSystem::new();
        assert_eq!(sys.dip_bank_value(0), 0x80);
        // 0x2800 is toggles 7-8, down to 0x2803 for toggles 1-2.
        for (offset, expect, what) in [
            (0, 0b10, "fuel toggles 7-8"),
            (1, 0b00, "toggle 5 fuel step clear, toggle 6 normal coinage"),
            (2, 0b00, "English"),
            (3, 0b00, "right coin mech: 1 credit per coin"),
        ] {
            let got = sys.bus_read(BusMaster::Cpu(0), 0x2800 + offset);
            assert_eq!(got, DSW_UNDRIVEN | expect, "{what}");
        }
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = LunarLanderSystem::new();

        // Set known state
        sys.board.map.region_data_mut(Region::Ram)[0x50] = 0xAA;
        sys.board.map.region_data_mut(Region::VectorRam)[0x200] = 0xBB;
        sys.in0 = 0x3C;
        sys.in1 = 0xE8;
        sys.board.clock = 75_000;
        sys.board.nmi_counter = 3000;
        sys.board.nmi_pending = true;
        sys.board.watchdog_frame_count = 5;
        sys.thrust_value = 0x80;

        // Save
        let data = sys.save_state().expect("save_state should return Some");
        let cpu_snap = sys.cpu.snapshot();

        // Mutate everything
        let mut sys2 = LunarLanderSystem::new();
        sys2.board.map.region_data_mut(Region::Ram)[0x50] = 0xFF;
        sys2.in0 = 0xFF;
        sys2.board.clock = 999;

        // Load
        sys2.load_state(&data).unwrap();

        // Verify CPU
        assert_eq!(sys2.cpu.snapshot(), cpu_snap);

        // Verify memory
        assert_eq!(sys2.board.map.region_data(Region::Ram)[0x50], 0xAA);
        assert_eq!(sys2.board.map.region_data(Region::VectorRam)[0x200], 0xBB);

        // Verify I/O and timing state
        assert_eq!(sys2.in0, 0x3C);
        assert_eq!(sys2.in1, 0xE8);
        assert_eq!(sys2.board.clock, 75_000);
        assert_eq!(sys2.board.nmi_counter, 3000);
        assert!(sys2.board.nmi_pending);
        assert_eq!(sys2.board.watchdog_frame_count, 5);
        assert_eq!(sys2.thrust_value, 0x80);
    }

    #[test]
    fn save_does_not_include_rom() {
        let mut sys = LunarLanderSystem::new();
        sys.board.map.region_data_mut(Region::ProgramRom)[0] = 0xDE;
        sys.board.map.region_data_mut(Region::VectorRom)[0] = 0xAD;

        let data = sys.save_state().unwrap();

        // Load into a fresh system (ROMs are zeroed)
        let mut sys2 = LunarLanderSystem::new();
        sys2.load_state(&data).unwrap();

        // ROMs should remain at their default (zeroed), not overwritten
        assert_eq!(sys2.board.map.region_data(Region::ProgramRom)[0], 0x00);
        assert_eq!(sys2.board.map.region_data(Region::VectorRom)[0], 0x00);
    }
}
