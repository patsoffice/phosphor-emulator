//! Asteroids (Atari, 1979), on the shared Atari DVG vector board.
//!
//! # Schematics
//!
//! | Drawing | Source | Pages |
//! |---|---|---|
//! | `OPTIONS INPUT CIRCUITRY`, sheet 2 side B, DP-143-02 3rd printing | `arcade-museum.com/manuals-videogames/A/Asteroids-sp.pdf` | PDF p7 |
//!
//! Only that block has been read. It is the source for the DSW1 decode at
//! 0x2800-0x2803: an LS253 at P6 with the 8-position switch R6 wired toggle
//! 1 to 1C3, 3 to 1C2, 5 to 1C1 and 7 to 1C0 (all reaching DB0 through 1Y),
//! and toggle 2 to 2C3, 4 to 2C2, 6 to 2C1 and 8 to 2C0 (reaching DB1). Since
//! a '253 selects Cn with n = (B,A) = (AB1,AB0), the pairs count DOWN: 0x2800
//! reads toggles 7-8 and 0x2803 reads toggles 1-2. The block also states that
//! toggle inputs are on when pulled to ground, which is why an ON toggle
//! reads 0.
//!
//! # Manual
//!
//! | Document | Source | Pages |
//! |---|---|---|
//! | `Operation, Maintenance and Service Manual` TM-143 3rd printing | `arcade-museum.com/manuals-videogames/A/Asteroids.pdf` | PDF p13 (printed p7) |
//!
//! `Figure 7 Option Switch Settings` on that page is the source for
//! [`ASTEROIDS_DIP_BANKS`]: which toggle carries which option, the On/Off
//! pattern for every choice, and the suggested settings the power-on byte
//! encodes.

use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    ActionRole, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, InputConfigurable,
    InputControl, InputEvent, InputId, InputKind, MachineCore, SaveState,
};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_macros::Saveable;

use crate::asteroids_sound::AsteroidsDiscreteSound;
use crate::atari_dvg::{self, AtariDvgBoard, AtariDvgBus, DSW_UNDRIVEN, Region, ramsel_addr};
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_high;
use phosphor_core::cpu::m6502::M6502;

// ---------------------------------------------------------------------------
// ROM definitions (MAME `asteroid` parent set)
// ---------------------------------------------------------------------------

/// Program ROM: 6KB at CPU addresses 0x6800–0x7FFF.
static PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x1800,
    entries: &[
        RomEntry {
            name: "035145-04e.ef2",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0xb503eaf7],
        },
        RomEntry {
            name: "035144-04e.h2",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x25233192],
        },
        RomEntry {
            name: "035143-02.j2",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0x312caa02],
        },
    ],
};

/// Vector ROM: 2KB at CPU address 0x5000–0x57FF.
static VECTOR_ROM: RomRegion = RomRegion {
    size: 0x0800,
    entries: &[RomEntry {
        name: "035127-02.np3",
        size: 0x0800,
        offset: 0x0000,
        crc32: &[0x8b71fd9e],
    }],
};

// ---------------------------------------------------------------------------
// Input button IDs
// ---------------------------------------------------------------------------

pub const INPUT_COIN: u8 = 0;
pub const INPUT_START1: u8 = 1;
pub const INPUT_START2: u8 = 2;
pub const INPUT_THRUST: u8 = 3;
pub const INPUT_FIRE: u8 = 4;
pub const INPUT_HYPERSPACE: u8 = 5;
pub const INPUT_ROT_LEFT: u8 = 6;
pub const INPUT_ROT_RIGHT: u8 = 7;

/// Typed logical controls. `InputId`s reuse the `INPUT_*` numbering; default
/// bindings mirror the legacy name-matched defaults (the board reuses standard
/// names — thrust = "P1 Up", hyperspace = "P1 Jump", rotate = "P1 Left/Right").
/// Fire is the primary action (gamepad A); hyperspace is key-only.
const ASTEROIDS_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_COIN as u16),
        stable_name: "coin",
        label: "Coin",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
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
        id: InputId(INPUT_THRUST as u16),
        stable_name: "thrust",
        label: "Thrust",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_UP,
    },
    InputControl {
        id: InputId(INPUT_FIRE as u16),
        stable_name: "fire",
        label: "Fire",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_HYPERSPACE as u16),
        stable_name: "hyperspace",
        label: "Hyperspace",
        kind: InputKind::Action(ActionRole::Secondary),
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
];

// ---------------------------------------------------------------------------
// AsteroidsSystem — Atari DVG board configured for Asteroids (1979)
// ---------------------------------------------------------------------------

/// Asteroids-specific wrapper around the shared Atari DVG board.
///
/// Memory map (15-bit address bus, `addr & 0x7FFF`):
///   0x0000–0x03FF  RAM (1 KB)
///   0x2000–0x2007  IN0 read (buttons, 3 KHz clock, VG_HALT)
///   0x2400–0x2407  IN1 read (coins, start, thrust, rotate)
///   0x2800–0x2803  DSW1 read (DIP switches)
///   0x3000         DVG GO write
///   0x3200         Output latch write (74LS259)
///   0x3400         Watchdog reset write
///   0x3600         Explosion sound write
///   0x3A00         Thump sound write
///   0x3C00–0x3C07  Audio latch write (74LS259)
///   0x3E00         Noise reset write
///   0x4000–0x47FF  Vector RAM (2 KB, shared CPU/DVG)
///   0x5000–0x57FF  Vector ROM (2 KB)
///   0x6800–0x7FFF  Program ROM (6 KB)
#[derive(Saveable, phosphor_macros::BusDebug)]
pub struct AsteroidsSystem {
    /// The 6502 is held beside the bus view over the board.
    #[debug_cpu("M6502")]
    pub cpu: M6502,

    #[debug_bus]
    pub board: AtariDvgBoard,

    /// Discrete analog sound (explosion/thump/saucer/fire/thrust/life).
    sound: AsteroidsDiscreteSound,

    // I/O — active-HIGH inputs (default 0x00 = all released)
    in0: u8,
    in1: u8,
    /// DIP switches: default 0x84 (English, 3 lives, 1 coin/1 credit).
    #[save_skip]
    dip_switches: u8,
    /// RAMSEL, from output-latch bit 2: swaps the two 256-byte SRAMs over the
    /// 0x0200-0x02FF and 0x0300-0x03FF windows.
    ramsel: bool,
}

impl AsteroidsSystem {
    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(Region::Ram, "RAM", 0x0000, 0x0400, AccessKind::ReadWrite)
            .mirror(0x0400, 0x0000, 0x0400)
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
                0x5000,
                0x0800,
                AccessKind::ReadOnly,
            )
            .region(
                Region::ProgramRom,
                "Program ROM",
                0x6800,
                0x1800,
                AccessKind::ReadOnly,
            );
        map
    }

    pub fn new() -> Self {
        Self {
            cpu: M6502::new(),
            // Asteroids: VROM at DVG 0x1000, size 0x0800
            board: AtariDvgBoard::new(Self::build_map(), 0x1000, 0x0800),
            sound: AsteroidsDiscreteSound::new(),
            in0: 0x00,
            in1: 0x00,
            dip_switches: 0x84, // English, 3 lives, 1C/1C
            ramsel: false,
        }
    }

    /// Borrow the CPU and the bus it drives as two disjoint pieces.
    #[inline]
    fn split(&mut self) -> (&mut M6502, AsteroidsBus<'_>) {
        (
            &mut self.cpu,
            AsteroidsBus {
                board: &mut self.board,
                sound: &mut self.sound,
                in0: self.in0,
                in1: self.in1,
                dip_switches: self.dip_switches,
                ramsel: &mut self.ramsel,
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

impl Default for AsteroidsSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus implementation
// ---------------------------------------------------------------------------

/// The Asteroids bus: the shared DVG board plus this game's I/O -- the input
/// multiplexers, the DIP switches, and the discrete sound board.
struct AsteroidsBus<'a> {
    board: &'a mut AtariDvgBoard,
    sound: &'a mut AsteroidsDiscreteSound,
    in0: u8,
    in1: u8,
    dip_switches: u8,
    /// Borrowed rather than copied: the CPU writes RAMSEL through the output
    /// latch mid-frame and the new value has to outlive the bus view.
    ramsel: &'a mut bool,
}

impl AtariDvgBus for AsteroidsBus<'_> {
    #[inline]
    fn board(&mut self) -> &mut AtariDvgBoard {
        self.board
    }

    /// Drive TEST, which gates the 250 Hz NMI off while the self-test switch is
    /// on. IN0 bit 7 is the switch, active HIGH on this board.
    #[inline]
    fn begin_cycle(&mut self) {
        self.board.test_asserted = self.in0 & 0x80 != 0;
    }
}

impl Bus for AsteroidsBus<'_> {
    type Address = u16;
    type Data = u8;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let addr = addr & 0x7FFF; // 15-bit address bus

        let data = match self.board.map.page(addr).region_id {
            Region::RAM => self.board.map.read_backing(ramsel_addr(addr, *self.ramsel)),
            Region::VECTOR_RAM | Region::VECTOR_ROM | Region::PROGRAM_ROM => {
                self.board.map.read_backing(addr)
            }

            Region::IO => match addr {
                // IN0: 0x2000–0x2007 — 74LS251 8:1 multiplexer.
                // A0–A2 select which input bit to read; the selected bit appears on D7.
                // The 6502 tests it via BIT (N flag = D7).
                //   Bit 0: unused
                //   Bit 1: 3 KHz clock (cpu total_cycles & 0x100)
                //   Bit 2: VG_HALT (active-LOW: 0 = done, 1 = running)
                //   Bit 3: Hyperspace     Bit 4: Fire
                //   Bit 5: Diagnostic     Bit 6: Tilt     Bit 7: Self-test
                0x2000..=0x2007 => {
                    let offset = (addr & 7) as u8;
                    let mut val = self.in0;
                    if self.board.clock & 0x100 != 0 {
                        val |= 0x02;
                    } else {
                        val &= !0x02;
                    }
                    if !self.board.dvg.is_halted() {
                        val |= 0x04;
                    } else {
                        val &= !0x04;
                    }
                    ((val >> offset) & 1) << 7
                }

                // IN1: 0x2400–0x2407 — 74LS251 8:1 multiplexer.
                //   Bit 0: Left coin   Bit 1: Center coin   Bit 2: Right coin
                //   Bit 3: 1P Start    Bit 4: 2P Start
                //   Bit 5: Thrust      Bit 6: Rotate right   Bit 7: Rotate left
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
            Region::RAM => self
                .board
                .map
                .write_backing(ramsel_addr(addr, *self.ramsel), data),
            Region::VECTOR_RAM => self.board.map.write_backing(addr, data),

            Region::IO => match addr {
                0x3000 => self.board.trigger_dvg(),
                // Output latch (LS174 at N11). Only RAMSEL is modeled: bit 2
                // selects which of the two 256-byte SRAMs answers which half of
                // 0x0200-0x03FF. The lamp and coin-counter bits are outputs
                // nothing here reads back.
                0x3200 => *self.ramsel = data & 0x04 != 0,
                0x3400 => self.board.watchdog_frame_count = 0,
                0x3600 => self.sound.write_explosion(data),
                0x3A00 => self.sound.write_thump(data),
                // 74LS259 addressable latch (write_d7): A0-A2 select the line,
                // the latched value comes from D7.
                0x3C00..=0x3C07 => self
                    .sound
                    .write_audio_latch_bit((addr & 7) as u8, data & 0x80 != 0),
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
// game wrapper's discrete sound device, so AudioSource is implemented by hand
// (the board has no sound hardware to delegate to).
crate::impl_board_renderable!(AsteroidsSystem, board, atari_dvg::TIMING, vectors);
crate::impl_board_debug!(AsteroidsSystem, board, atari_dvg::TIMING);

impl phosphor_core::core::machine::AudioSource for AsteroidsSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.sound.fill_audio(buffer)
    }
    fn audio_sample_rate(&self) -> u32 {
        self.sound.sample_rate()
    }
}

impl InputConfigurable for AsteroidsSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        ASTEROIDS_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let InputEvent::Button { id, pressed } = event else {
            return;
        };
        match id.0 as u8 {
            // IN1 (active-HIGH: set bit on press, clear on release)
            INPUT_COIN => set_bit_active_high(&mut self.in1, 0, pressed),
            INPUT_START1 => set_bit_active_high(&mut self.in1, 3, pressed),
            INPUT_START2 => set_bit_active_high(&mut self.in1, 4, pressed),
            INPUT_THRUST => set_bit_active_high(&mut self.in1, 5, pressed),
            INPUT_ROT_RIGHT => set_bit_active_high(&mut self.in1, 6, pressed),
            INPUT_ROT_LEFT => set_bit_active_high(&mut self.in1, 7, pressed),

            // IN0 (active-HIGH)
            INPUT_FIRE => set_bit_active_high(&mut self.in0, 4, pressed),
            INPUT_HYPERSPACE => set_bit_active_high(&mut self.in0, 3, pressed),

            _ => {}
        }
    }
}

impl MachineCore for AsteroidsSystem {
    crate::machine_core_metadata!("asteroids", atari_dvg::TIMING, atari_dvg::clock_tree);

    fn run_frame(&mut self) {
        let (cpu, mut bus) = self.split();
        atari_dvg::run_frame(cpu, &mut bus);

        // Advance the discrete sound circuit for the frame's worth of CPU
        // cycles (register writes during the frame have already landed).
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

impl SaveState for AsteroidsSystem {
    crate::machine_save_state!();
}

impl phosphor_core::core::machine::Nvram for AsteroidsSystem {}
impl phosphor_core::core::machine::Profilable for AsteroidsSystem {}
/// DIP switch metadata for Asteroids' DSW1 byte, the 8-toggle switch read two
/// toggles at a time through the 74LS253 mux at 0x2800-0x2803.
///
/// Transcribed from `Figure 7 Option Switch Settings` (TM-143 3rd printing,
/// printed page 7). Byte bit *n* carries toggle *n+1*, and a toggle reads 0 when
/// it is ON and 1 when OFF: the manual states that sense directly, since the
/// self-test shows an ON toggle as `0` and an OFF toggle as `1`. Each choice
/// value below is that figure's On/Off column read as a binary number.
///
/// The default 0x84 is the manual's own suggested setting, whose photograph is
/// captioned "toggles 1, 2, 4-7 on, and toggles 3 and 8 off": English, a
/// 3-ship game, both coin mechs at x1, and one coin for one play.
const ASTEROIDS_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSW1",
        options: &[
            DipOption {
                name: "Language",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "English",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "German",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "French",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "Spanish",
                        value: 0x03,
                    },
                ],
            },
            DipOption {
                name: "Lives",
                mask: 0x04,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "4",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "3",
                        value: 0x04,
                    },
                ],
            },
            DipOption {
                name: "Center Mech",
                mask: 0x08,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "x1",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "x2",
                        value: 0x08,
                    },
                ],
            },
            DipOption {
                name: "Right Mech",
                mask: 0x30,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "x1",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "x4",
                        value: 0x10,
                    },
                    DipChoice {
                        label: "x5",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "x6",
                        value: 0x30,
                    },
                ],
            },
            DipOption {
                name: "Coinage",
                mask: 0xC0,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Free Play",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "1 Coin/2 Credits",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "1 Coin/1 Credit",
                        value: 0x80,
                    },
                    DipChoice {
                        label: "2 Coins/1 Credit",
                        value: 0xC0,
                    },
                ],
            },
        ],
    },
    SELF_TEST_BANK,
];

/// The self-test switch, which is not one of the eight option toggles.
///
/// It is a maintained slide switch on a bracket inside the coin door, read with
/// the player inputs through the IN0 multiplexer on bit 7 rather than through
/// the option port. It belongs here anyway because it is a switch an operator
/// sets and leaves set, not a button: the self-test screen is where the option
/// toggles are read back, so it is held on while they are adjusted.
///
/// Bit 7 of IN0 is active HIGH on this board and on the Deluxe; Lunar Lander's
/// is bit 1 and active LOW.
const SELF_TEST_BANK: DipSwitchBank = DipSwitchBank {
    name: "Service",
    options: &[DipOption {
        name: "Self-Test",
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
    }],
};

crate::impl_dip_switches!(
    AsteroidsSystem,
    ASTEROIDS_DIP_BANKS,
    dip_switches & 0xFF,
    in0 & 0x80,
);
crate::impl_board_debug_trace!(AsteroidsSystem, board);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

crate::register_machine!(
    AsteroidsSystem,
    "asteroid",
    &["asteroid"],
    ASTEROIDS_CONTROLS
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atari_dvg::Region;
    use phosphor_core::core::machine::DipSwitches;
    use phosphor_core::cpu::CpuStateTrait;

    #[test]
    fn dip_default_and_metadata() {
        let sys = AsteroidsSystem::new();
        assert_eq!(sys.dip_bank_value(0), 0x84); // English, 3 lives, 1C/1C
        crate::assert_dip_banks_valid(
            sys.dip_banks(),
            &[sys.dip_bank_value(0), sys.dip_bank_value(1)],
        );
        // The self-test switch powers on released.
        assert_eq!(sys.dip_bank_value(1), 0x00);
    }

    #[test]
    fn set_dip_option_masks_only_its_bits() {
        let mut sys = AsteroidsSystem::new();
        // Coinage is option 4 (mask 0xC0); pick "2 Coins/1 Credit" (0xC0).
        sys.set_dip_option(0, 4, 0xC0);
        assert_eq!(sys.dip_bank_value(0), 0xC4); // 0x84 low bits kept, top two set
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
    /// * counting the pairs upwards reads 0, 1, 2, 3, which swaps Coinage with
    ///   Language end for end.
    ///
    /// Both were live at once here, which left the game reading its Language
    /// toggle as the coinage setting.
    #[test]
    fn the_dip_mux_reads_pairs_from_the_top_of_the_byte_down() {
        let mut sys = AsteroidsSystem::new();
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

        // Only DB0 and DB1 carry a toggle; the six the mux never drives float
        // high.
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

    /// The power-on byte is the manual's suggested setting, and each option
    /// lands on the toggles Figure 7 assigns it. 0x84 is "toggles 1, 2, 4-7 on,
    /// and toggles 3 and 8 off" with ON reading as 0, so the four mux addresses
    /// see Coinage=1C/1C, Right Mech=x1, Ships=3 with Center Mech=x1, English.
    #[test]
    fn the_default_byte_is_the_manuals_suggested_setting() {
        let mut sys = AsteroidsSystem::new();
        assert_eq!(sys.dip_bank_value(0), 0x84);
        // 0x2800 is toggles 7-8 (Coinage), down to 0x2803 for toggles 1-2.
        for (offset, expect, what) in [
            (0, 0b10, "coinage: 1 coin / 1 play"),
            (1, 0b00, "right coin mech x1"),
            (2, 0b01, "3-ship game, center mech x1"),
            (3, 0b00, "English"),
        ] {
            let got = sys.bus_read(BusMaster::Cpu(0), 0x2800 + offset);
            assert_eq!(got, DSW_UNDRIVEN | expect, "{what}");
        }
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = AsteroidsSystem::new();

        // Set known state
        sys.board.map.region_data_mut(Region::Ram)[0x100] = 0xAA;
        sys.board.map.region_data_mut(Region::VectorRam)[0x200] = 0xBB;
        sys.in0 = 0x18;
        sys.in1 = 0xE8;
        sys.board.clock = 75_000;
        sys.board.nmi_counter = 3000;
        sys.board.nmi_pending = true;
        sys.board.watchdog_frame_count = 5;

        // Save
        let data = sys.save_state().expect("save_state should return Some");
        let cpu_snap = sys.cpu.snapshot();

        // Mutate everything
        let mut sys2 = AsteroidsSystem::new();
        sys2.board.map.region_data_mut(Region::Ram)[0x100] = 0xFF;
        sys2.in0 = 0xFF;
        sys2.board.clock = 999;

        // Load
        sys2.load_state(&data).unwrap();

        // Verify CPU
        assert_eq!(sys2.cpu.snapshot(), cpu_snap);

        // Verify memory
        assert_eq!(sys2.board.map.region_data(Region::Ram)[0x100], 0xAA);
        assert_eq!(sys2.board.map.region_data(Region::VectorRam)[0x200], 0xBB);

        // Verify I/O and timing state
        assert_eq!(sys2.in0, 0x18);
        assert_eq!(sys2.in1, 0xE8);
        assert_eq!(sys2.board.clock, 75_000);
        assert_eq!(sys2.board.nmi_counter, 3000);
        assert!(sys2.board.nmi_pending);
        assert_eq!(sys2.board.watchdog_frame_count, 5);

        // Transient state should be cleared
        assert!(sys2.board.display_list.is_empty());
    }

    #[test]
    fn save_does_not_include_rom() {
        let mut sys = AsteroidsSystem::new();
        sys.board.map.region_data_mut(Region::ProgramRom)[0] = 0xDE;
        sys.board.map.region_data_mut(Region::VectorRom)[0] = 0xAD;

        let data = sys.save_state().unwrap();

        // Load into a fresh system (ROMs are zeroed)
        let mut sys2 = AsteroidsSystem::new();
        sys2.load_state(&data).unwrap();

        // ROMs should remain at their default (zeroed), not overwritten
        assert_eq!(sys2.board.map.region_data(Region::ProgramRom)[0], 0x00);
        assert_eq!(sys2.board.map.region_data(Region::VectorRom)[0], 0x00);
    }
}
