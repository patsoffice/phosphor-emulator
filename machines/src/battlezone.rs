//! Atari Battlezone (1980).
//!
//! # Schematics
//!
//! Drawing Package Supplement to the Operation, Maintenance, and Service
//! Manual, DP-156-01 3rd printing:
//!
//! | Drawing | Sheet | What was read |
//! |---|---|---|
//! | Power/coin/wiring | Sheet 1 side A | Regulator/Audio II PCB 035435-02 rev C carries two TDA2002AV amplifiers, each at a gain of ten; the cabinet wiring bridges the speakers across them |
//! | Math Box signature analysis | Sheet 1 side B | Aux PCB 035678-01; the Mathbox answers addresses EA80-EAB4 with data EDB0-EDB7 (service prose, no circuit detail) |
//! | Game microprocessor | Sheet 2 side A | 12.096 MHz crystal, 6502 on φ0 1.5 MHz, NMI counter, watchdog, and the full memory-map table, which this file's map follows row for row |
//! | Coin door + AVG outputs | Sheet 2 side B | Option-switch LS244s, the coin-door input multiplexer (3KHz + HALT alongside the switches), the X/Y/Z video outputs with X INV / Y INV, and the three coin-counter drivers |
//!
//! Sheet 3 (Vector-Generator and Auxiliary PCB Math Box, switch inputs and
//! audio outputs) was not in the package at hand. Where it would speak, the
//! reference implementation does: MAME's `bzone.cpp` / `bzone_a.cpp` and its
//! `AVG_BZONE` vector generator, which boots these same ROMs.
//!
//! # Audio
//!
//! The POKEY's pin-37 load and the discrete engine/explosion/shell mix sit on
//! the Auxiliary PCB (sheet 3 side B, unread), so the POKEY runs at its
//! normalized output and the discrete voices are behavioral but part-valued;
//! see [`crate::battlezone_sound`]. The amplifier both feed is the
//! Regulator/Audio II 035435-02 already modeled for Missile Command and
//! Tempest in [`crate::atari_regulator_audio`]: same PCB number, here at
//! revision C, whose output-coupling value was never transcribed (revs B, E,
//! F and G are), so it runs rev B's 1000 uF. Either value corners below any
//! cabinet speaker's range (19.9 Hz against 6.0 Hz), which is why the
//! substitution is noted rather than modeled as a choice.
//!
//! # Video
//!
//! The issue filing this machine says "M6502 + DVG + Mathbox". The board is
//! the Analog Vector-Generator PCB A035742 (the schematic's own title), and
//! the reference drives it with its AVG device, so this machine uses the
//! shared [`crate::atari_avg`] board with the black-and-white AVG variant,
//! not the DVG.
//!
//! # Memory map (from the sheet 2 side A table)
//!
//! ```text
//!   $0000–$03FF  Program RAM (1 KB)
//!   $0800        IN0 (coins, slam, self-test, diag step, VG done, 3KHz)
//!   $0A00        Option switches 1 (M10)
//!   $0C00        Option switches 2 (P10)
//!   $1000        W: coin counters (right/center/left) + video invert X
//!   $1200        W: AVG GO (+ video invert Y, same strobe)
//!   $1400        W: watchdog clear
//!   $1600        W: AVG reset
//!   $1800        R: Mathbox status
//!   $1810        R: Mathbox result low
//!   $1818        R: Mathbox result high
//!   $1820–$182F  R/W: POKEY (reads of $1828/ALLPOT return the control byte)
//!   $1840        W: sounds latch
//!   $1860–$187F  W: Mathbox command
//!   $2000–$2FFF  Vector RAM (4 KB, shared CPU/AVG)
//!   $3000–$3FFF  Vector ROM (4 KB)
//!   $4000–$4FFF  Open bus
//!   $5000–$7FFF  Program ROM (12 KB)
//! ```
//!
//! Two rows of that table need a note. VIDEO INVERT X ($1000) and VIDEO
//! INVERT Y ($1200) share their addresses with the coin counters and AVG GO:
//! the invert bits ride the strobe's data byte. The upright game this machine
//! ships never inverts (that is the cocktail set's bit), and the reference
//! ignores both latches, so this machine does too rather than wire a latch to
//! a bit the package never names. What the game writes to $1000 and $1200 is
//! established from its own ROM: coin-counter patterns and GO strobes.
//!
//! The controls all arrive through POKEY ALLPOT ($1828), which the game's ROM
//! reads and nothing else on the POKEY does: no POT0–POT7 read appears
//! anywhere in the 12 KB program. The byte is the stick directions, fire and
//! starts as active-high bits, which is what the reference's IN3 port feeds
//! its ALLPOT callback.

use phosphor_core::audio::{SampleRing, host_sample_rate};
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    ActionRole, AudioSource, DefaultBinding, DipApplyTiming, DipChoice, DipOption, DipSwitchBank,
    InputConfigurable, InputControl, InputEvent, InputId, InputKind, KeyId, MachineCore, Nvram,
    Profilable, SaveState,
};
use phosphor_core::core::{AccessKind, AddressSpace16, TimingConfig};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::device::Mathbox;
use phosphor_core::device::avg::AvgVariant;
use phosphor_core::device::pokey::Pokey;
use phosphor_macros::{BusDebug, Saveable};

use crate::atari_avg::{self, AtariAvgBoard, Region};
use crate::atari_regulator_audio::{C9_REV_B, RegulatorAudioII};
use crate::battlezone_sound::BattlezoneSound;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_high;
use crate::set_bit_active_low;

// ---------------------------------------------------------------------------
// ROM definitions (MAME `bzone` rev 2 and `bzonea` rev 1)
// ---------------------------------------------------------------------------

/// Program ROM: 12 KB at CPU addresses $5000–$7FFF. The two revisions differ
/// in the $5000 entry alone (036414-02 vs 036414-01).
static BZONE_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x3000,
    entries: &[
        RomEntry {
            name: "036414-02.e1",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0x13de36d5],
        },
        RomEntry {
            name: "036413-01.h1",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x5d9d9111],
        },
        RomEntry {
            name: "036412-01.j1",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0xab55cbd2],
        },
        RomEntry {
            name: "036411-01.k1",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0xad281297],
        },
        RomEntry {
            name: "036410-01.lm1",
            size: 0x0800,
            offset: 0x2000,
            crc32: &[0x0b7bfaa4],
        },
        RomEntry {
            name: "036409-01.n1",
            size: 0x0800,
            offset: 0x2800,
            crc32: &[0x1e14e919],
        },
    ],
};

/// Program ROM for rev 1 (`bzonea`): same five entries, older $5000 image.
static BZONEA_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x3000,
    entries: &[
        RomEntry {
            name: "036414-01.e1",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0xefbc3fa0],
        },
        RomEntry {
            name: "036413-01.h1",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x5d9d9111],
        },
        RomEntry {
            name: "036412-01.j1",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0xab55cbd2],
        },
        RomEntry {
            name: "036411-01.k1",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0xad281297],
        },
        RomEntry {
            name: "036410-01.lm1",
            size: 0x0800,
            offset: 0x2000,
            crc32: &[0x0b7bfaa4],
        },
        RomEntry {
            name: "036409-01.n1",
            size: 0x0800,
            offset: 0x2800,
            crc32: &[0x1e14e919],
        },
    ],
};

/// Vector ROM: 4 KB at AVG addresses $1000–$1FFF (CPU $3000–$3FFF).
static VECTOR_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[
        RomEntry {
            name: "036422-01.bc3",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0x7414177b],
        },
        RomEntry {
            name: "036421-01.a3",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x8ea8f939],
        },
    ],
};

/// AVG state PROM (256×4), the vector generator's next-state table. Same
/// image as Tempest's (same CRC), as the sequencer is the same circuit.
static AVG_PROM: RomRegion = RomRegion {
    size: 0x100,
    entries: &[RomEntry {
        name: "036408-01.k7",
        size: 0x100,
        offset: 0x0000,
        crc32: &[0x5903af03],
    }],
};

/// One ROM configuration per revision, paired by position with the revision
/// list in `register_machine!` below.
struct BattlezoneRomConfig {
    /// MAME set name this config loads (e.g. "bzone").
    set: &'static str,
    program_rom: &'static RomRegion,
}

static BZONE_CONFIG: BattlezoneRomConfig = BattlezoneRomConfig {
    set: "bzone",
    program_rom: &BZONE_PROGRAM_ROM,
};

static BZONEA_CONFIG: BattlezoneRomConfig = BattlezoneRomConfig {
    set: "bzonea",
    program_rom: &BZONEA_PROGRAM_ROM,
};

const ALL_CONFIGS: &[&BattlezoneRomConfig] = &[&BZONE_CONFIG, &BZONEA_CONFIG];

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------

/// Master clock: 12.096 MHz (sheet 2 side A, Y2).
/// CPU clock: 12.096 / 8 = 1.512 MHz.
/// 3 KHz clock: 12.096 MHz / 4096 = 2953.125 Hz.
/// NMI: 3 KHz / 12 = 246.09 Hz → every 6144 CPU cycles.
/// Frame: NMI / 6 = 41.02 Hz → 36864 CPU cycles, the reference's refresh.
pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 1_512_000,
    cycles_per_scanline: 36_864, // no scanline hardware; whole frame
    total_scanlines: 1,
    display_width: 580,
    display_height: 400,
    display_aspect: Some((4, 3)),
};

/// The board's crystal and everything divided out of it. The crystals and
/// dividers are Tempest's (same 12.096 MHz crystal, same divide-by-eight to
/// the 6502, same AVG clock), so this is the shared AVG tree.
pub fn clock_tree() -> phosphor_core::core::ClockTree {
    atari_avg::clock_tree()
}

// ---------------------------------------------------------------------------
// Input button IDs
// ---------------------------------------------------------------------------

pub const INPUT_COIN1: u8 = 0;
pub const INPUT_COIN2: u8 = 1;
pub const INPUT_SERVICE: u8 = 2;
pub const INPUT_DIAG_STEP: u8 = 3;
pub const INPUT_LEFT_FWD: u8 = 4;
pub const INPUT_LEFT_BACK: u8 = 5;
pub const INPUT_RIGHT_FWD: u8 = 6;
pub const INPUT_RIGHT_BACK: u8 = 7;
pub const INPUT_FIRE: u8 = 8;
pub const INPUT_START1: u8 = 9;
pub const INPUT_START2: u8 = 10;

/// Typed logical controls. The two tank sticks are digital to the game (fore
/// and aft per tread through ALLPOT), so they are buttons, not axes: right
/// tread on the arrow keys, left tread on W/S.
const BATTLEZONE_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_COIN1 as u16),
        stable_name: "coin1",
        label: "Coin",
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
    InputControl {
        id: InputId(INPUT_DIAG_STEP as u16),
        stable_name: "diag_step",
        label: "Diagnostic Step",
        kind: InputKind::Service,
        player: None,
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_LEFT_FWD as u16),
        stable_name: "left_fwd",
        label: "Left Tread Forward",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: &[DefaultBinding::Key(KeyId::W)],
    },
    InputControl {
        id: InputId(INPUT_LEFT_BACK as u16),
        stable_name: "left_back",
        label: "Left Tread Back",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: &[DefaultBinding::Key(KeyId::S)],
    },
    InputControl {
        id: InputId(INPUT_RIGHT_FWD as u16),
        stable_name: "right_fwd",
        label: "Right Tread Forward",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: &[DefaultBinding::Key(KeyId::Up)],
    },
    InputControl {
        id: InputId(INPUT_RIGHT_BACK as u16),
        stable_name: "right_back",
        label: "Right Tread Back",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: &[DefaultBinding::Key(KeyId::Down)],
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
];

// ---------------------------------------------------------------------------
// BattlezoneSystem: Atari AVG board configured for Battlezone (1980)
// ---------------------------------------------------------------------------

/// Battlezone-specific hardware beside the shared AVG board: the Mathbox, one
/// POKEY, the discrete voices, and the input bytes.
#[derive(Saveable)]
pub struct BattlezoneIo {
    /// Mathbox coprocessor ($1800/$1810/$1818 read, $1860–$187F write).
    mathbox: Mathbox,

    /// POKEY sound + control multiplexer ($1820–$182F).
    pokey: Pokey,

    /// Discrete engine/shell/explosion voices ($1840 latch).
    sound: BattlezoneSound,

    /// IN0 ($0800): coins (active-LOW bits 0–1), self-test (4), diag step
    /// (5). Bits 2–3 are wired high (the reference marks them unused), bit 6
    /// (VG done) and bit 7 (3KHz clock) are generated dynamically.
    in0: u8,

    /// Control byte read back through POKEY ALLPOT ($1828), active-HIGH:
    /// bit 0 right tread back, 1 right forward, 2 left back, 3 left forward,
    /// 4 fire, 5 start 1, 6 start 2.
    in3: u8,

    // DIP switches: M10 game options ($0A00) and P10 pricing ($0C00).
    #[save_skip]
    dsw0: u8,
    #[save_skip]
    dsw1: u8,
}

#[derive(Saveable, BusDebug)]
#[save_version(1)]
#[save_tlv]
pub struct BattlezoneSystem {
    #[debug_cpu("M6502")]
    #[save(id = 1)]
    cpu: M6502,

    #[debug_bus]
    #[save(id = 2)]
    pub board: AtariAvgBoard,

    #[save(id = 3)]
    io: BattlezoneIo,

    /// Mixed POKEY + discrete output, through the amplifier below.
    #[save_skip(default)]
    audio_buffer: SampleRing<i16>,

    /// The Regulator/Audio II PCB's amplifier at rev B's output coupling
    /// (see the module docs for why rev C runs it).
    #[save(id = 4)]
    amp: RegulatorAudioII,

    /// MAME set name this machine loaded (e.g., "bzonea"), reported by
    /// `MachineCore::revision`. Loading a save restores state, never ROMs,
    /// so this keeps its value across loads.
    #[save_skip]
    loaded_revision: &'static str,
}

/// The bus the 6502 sees: the shared AVG board plus Battlezone's own I/O,
/// borrowed disjointly from the machine so the CPU can execute against a
/// concrete type.
pub struct BattlezoneBus<'a> {
    board: &'a mut AtariAvgBoard,
    io: &'a mut BattlezoneIo,
}

/// One CPU cycle: the POKEY, the TEST gate, the board's NMI timing, then the
/// 6502.
#[inline]
fn tick(cpu: &mut M6502, bus: &mut BattlezoneBus<'_>) {
    bus.io.pokey.tick();
    // TEST is the self-test switch, active-LOW on IN0 bit 4: asserted while
    // the switch is pressed, which gates the 250 Hz NMI off.
    bus.board.test_asserted = bus.io.in0 & 0x10 == 0;
    bus.board.begin_cycle(cpu);
    cpu.execute_cycle(bus, BusMaster::Cpu(0));
    bus.board.end_cycle();
}

impl BattlezoneSystem {
    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(Region::Ram, "RAM", 0x0000, 0x0400, AccessKind::ReadWrite)
            .region(Region::Io, "I/O", 0x0800, 0x1800, AccessKind::Io)
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
            .region(
                Region::ProgramRom,
                "Program ROM",
                0x5000,
                0x3000,
                AccessKind::ReadOnly,
            );
        map
    }

    pub fn new() -> Self {
        let mut board =
            AtariAvgBoard::new_with_variant(Self::build_map(), AvgVariant::Battlezone, 580, 400);
        board.tick_drives_nmi = true;
        Self {
            cpu: M6502::new(),
            board,
            io: BattlezoneIo {
                mathbox: Mathbox::new(),
                pokey: Pokey::with_clock(1_512_000, host_sample_rate()),
                sound: BattlezoneSound::new(),
                in0: 0xFF,  // coins/test/diag all released (active-LOW)
                in3: 0x00,  // sticks/fire/starts all released (active-HIGH)
                dsw0: 0x15, // 3 tanks, missile at 10k, bonus 15k+100k, English
                dsw1: 0x03, // pricing default, as the reference ships it
                            // (the game inverts the DIP bits itself)
            },
            audio_buffer: SampleRing::with_capacity(2048),
            amp: RegulatorAudioII::new(C9_REV_B, host_sample_rate()),
            loaded_revision: "",
        }
    }

    fn split(&mut self) -> (&mut M6502, BattlezoneBus<'_>) {
        (
            &mut self.cpu,
            BattlezoneBus {
                board: &mut self.board,
                io: &mut self.io,
            },
        )
    }

    /// Advance one CPU cycle, returning the instruction-boundary mask.
    pub fn step_cycle(&mut self) -> u32 {
        let (cpu, mut bus) = self.split();
        tick(cpu, &mut bus);
        AtariAvgBoard::instruction_boundaries(&self.cpu)
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

    fn load_roms(
        &mut self,
        rom_set: &RomSet,
        config: &BattlezoneRomConfig,
    ) -> Result<(), RomLoadError> {
        // Recorded before the first ROM read so a blank-set load still
        // carries the attempted revision.
        self.loaded_revision = config.set;

        let prog = config.program_rom.load(rom_set)?;
        self.board.map.load_region(Region::ProgramRom, &prog);
        let vrom = VECTOR_ROM.load(rom_set)?;
        self.board.map.load_region(Region::VectorRom, &vrom);
        let avg_prom = AVG_PROM.load(rom_set)?;
        self.board.avg.load_state_prom(&avg_prom);
        Ok(())
    }
}

impl Default for BattlezoneSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus implementation
// ---------------------------------------------------------------------------

impl Bus for BattlezoneBus<'_> {
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
                // IN0: coins, self-test, diag step, VG done, 3KHz clock.
                // Bits 0–1 coins, bit 4 self-test, bit 5 diag step, all
                // active-LOW; bit 6 is VG done (active HIGH) and bit 7 the
                // 3KHz clock.
                0x0800 => {
                    let mut val = self.io.in0 & 0x3F;
                    if self.board.avg.is_halted() {
                        val |= 0x40;
                    }
                    if self.board.clock & 0x100 != 0 {
                        val |= 0x80;
                    }
                    val
                }

                // Option switches.
                0x0A00 => self.io.dsw0,
                0x0C00 => self.io.dsw1,

                // Mathbox status (always ready).
                0x1800 => self.io.mathbox.status_r(),

                // Mathbox result low/high.
                0x1810 => self.io.mathbox.lo_r(),
                0x1818 => self.io.mathbox.hi_r(),

                // ALLPOT reads back the control byte (sticks, fire, starts),
                // not the POKEY's scan status: that is the only POKEY read
                // the game's ROM makes outside RANDOM.
                0x1828 => self.io.in3,

                // POKEY.
                0x1820..=0x182F => self.io.pokey.read(addr & 0x0F),

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
                // Coin counters (right/center/left). No counter device is
                // modeled; the write has no visible effect.
                0x1000 => {}

                // AVG GO.
                0x1200 => self.board.trigger_avg(),

                // Watchdog clear.
                0x1400 => self.board.watchdog_frame_count = 0,

                // AVG reset.
                0x1600 => self.board.avg.reset(),

                // POKEY (a write to $1828 sets AUDCTL on the chip).
                0x1820..=0x182F => self.io.pokey.write(addr & 0x0F, data),

                // Sounds latch.
                0x1840 => self.io.sound.write_latch(data),

                // Mathbox command ($1860–$187F).
                0x1860..=0x187F => self.io.mathbox.go_w((addr & 0x1F) as u8, data),

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

impl phosphor_core::core::machine::Renderable for BattlezoneSystem {
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
        self.board.render_frame(buffer);
    }
    fn vector_display_list(&self) -> Option<&[phosphor_core::device::dvg::VectorLine]> {
        self.board.vector_display_list()
    }
}

impl AudioSource for BattlezoneSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.audio_buffer.pop_front_into(buffer)
    }

    fn audio_sample_rate(&self) -> u32 {
        host_sample_rate()
    }
}

impl InputConfigurable for BattlezoneSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        BATTLEZONE_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Button { id, pressed } => match id.0 as u8 {
                // IN0: coins, self-test, diag step (active-LOW).
                INPUT_COIN1 => set_bit_active_low(&mut self.io.in0, 0, pressed),
                INPUT_COIN2 => set_bit_active_low(&mut self.io.in0, 1, pressed),
                INPUT_SERVICE => set_bit_active_low(&mut self.io.in0, 4, pressed),
                INPUT_DIAG_STEP => set_bit_active_low(&mut self.io.in0, 5, pressed),

                // Control byte: sticks, fire, starts (active-HIGH).
                INPUT_LEFT_FWD => set_bit_active_high(&mut self.io.in3, 3, pressed),
                INPUT_LEFT_BACK => set_bit_active_high(&mut self.io.in3, 2, pressed),
                INPUT_RIGHT_FWD => set_bit_active_high(&mut self.io.in3, 1, pressed),
                INPUT_RIGHT_BACK => set_bit_active_high(&mut self.io.in3, 0, pressed),
                INPUT_FIRE => set_bit_active_high(&mut self.io.in3, 4, pressed),
                INPUT_START1 => set_bit_active_high(&mut self.io.in3, 5, pressed),
                INPUT_START2 => set_bit_active_high(&mut self.io.in3, 6, pressed),

                _ => {}
            },
            InputEvent::Relative { .. } | InputEvent::Absolute { .. } => {}
        }
    }
}

crate::impl_board_debug!(BattlezoneSystem, board, TIMING);
crate::impl_board_debug_trace!(BattlezoneSystem, board);

impl MachineCore for BattlezoneSystem {
    crate::machine_core_metadata!("battlezone", TIMING, clock_tree);

    fn revision(&self) -> &str {
        self.loaded_revision
    }

    fn run_frame(&mut self) {
        {
            let (cpu, mut bus) = self.split();
            for _ in 0..TIMING.cycles_per_frame() {
                tick(cpu, &mut bus);
            }
        }

        // Mix the POKEY with the discrete voices, then through the
        // Regulator/Audio II amplifier. The POKEY drains normalized samples
        // (its pin-37 load is unread), so its full scale is the chip's own.
        let pokey = self.io.pokey.drain_audio();
        let mixed = self.io.sound.mix_frame(&pokey);
        for s in mixed {
            let v = f32::from(s) / 32768.0;
            let amplified = self.amp.process(v);
            self.audio_buffer
                .push((amplified * 32767.0).clamp(i16::MIN as f32, i16::MAX as f32) as i16);
        }

        // Watchdog
        self.board.watchdog_frame_count += 1;
        if self.board.watchdog_frame_count >= 8 {
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.board.reset();
        self.io.mathbox.reset();
        self.io.pokey.reset();
        self.io.sound.reset();
        self.amp.reset();
        let (cpu, mut bus) = self.split();
        cpu.reset(&mut bus, BusMaster::Cpu(0));
    }
}

impl SaveState for BattlezoneSystem {
    crate::machine_save_state!();
}

// No battery RAM and no sub-span profiling → default empty impls. DebugTrace
// comes from `impl_board_debug_trace!` above, routed to the board's ring, so
// the combined default macro (which also emits an empty DebugTrace) does not
// apply here.
impl Nvram for BattlezoneSystem {}
impl Profilable for BattlezoneSystem {}

/// DIP switch metadata: DSW0 (game options at $0A00, M10) and DSW1 (pricing
/// at $0C00, P10). Bits and labels follow the reference's `bzone` layout;
/// the game inverts the DIP bits itself, so these are the raw switch values.
const BATTLEZONE_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSW0 (Game)",
        options: &[
            DipOption {
                name: "Tanks",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "2",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "3",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "4",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "5",
                        value: 0x03,
                    },
                ],
                conditional: &[],
            },
            DipOption {
                name: "Missile Appears At",
                mask: 0x0C,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "5000",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "10000",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "20000",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "30000",
                        value: 0x0C,
                    },
                ],
                conditional: &[],
            },
            DipOption {
                name: "Bonus Tank",
                mask: 0x30,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "None",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "15k and 100k",
                        value: 0x10,
                    },
                    DipChoice {
                        label: "25k and 100k",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "50k and 100k",
                        value: 0x30,
                    },
                ],
                conditional: &[],
            },
            DipOption {
                name: "Language",
                mask: 0xC0,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "English",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "German",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "French",
                        value: 0x80,
                    },
                    DipChoice {
                        label: "Spanish",
                        value: 0xC0,
                    },
                ],
                conditional: &[],
            },
        ],
    },
    DipSwitchBank {
        name: "DSW1 (Pricing)",
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
                        label: "1 Coin/2 Credits",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "1 Coin/1 Credit",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "2 Coins/1 Credit",
                        value: 0x03,
                    },
                ],
                conditional: &[],
            },
            DipOption {
                name: "Right Coin Multiplier",
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
                conditional: &[],
            },
            DipOption {
                name: "Center Coin Multiplier",
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
                conditional: &[],
            },
            DipOption {
                name: "Bonus Coins",
                mask: 0xE0,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "None",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "1 Each 2 Coins",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "1 Each 4 Coins",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "2 Each 4 Coins",
                        value: 0x60,
                    },
                    DipChoice {
                        label: "1 Each 5 Coins",
                        value: 0x80,
                    },
                ],
                conditional: &[],
            },
        ],
    },
];

crate::impl_dip_switches!(BattlezoneSystem, BATTLEZONE_DIP_BANKS, io.dsw0, io.dsw1);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

crate::register_machine!(
    BattlezoneSystem,
    "battlezone",
    &[
        crate::registry::Revision {
            names: &["bzone"],
            nvram_group: None
        },
        crate::registry::Revision {
            names: &["bzonea"],
            nvram_group: None
        },
    ],
    BATTLEZONE_CONTROLS,
    configs = ALL_CONFIGS
);

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::{Orientation, Renderable};
    use phosphor_core::cpu::CpuStateTrait;

    crate::dip_test_suite!(BattlezoneSystem, &[0x15, 0x03]);

    fn press(sys: &mut BattlezoneSystem, id: u8, pressed: bool) {
        sys.handle_input(InputEvent::Button {
            id: InputId(id as u16),
            pressed,
        });
    }

    /// Coins land on IN0's low bits (active-LOW); the VG-done bit reads back
    /// because the generator is halted at power-on.
    #[test]
    fn coin_door_lands_on_in0() {
        let mut sys = BattlezoneSystem::new();
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x0800) & 0x43, 0x43);
        press(&mut sys, INPUT_COIN1, true);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x0800) & 0x43, 0x42);
        press(&mut sys, INPUT_COIN2, true);
        press(&mut sys, INPUT_SERVICE, true);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x0800) & 0x33, 0x20);
    }

    /// Sticks, fire and starts land on the ALLPOT control byte (active-HIGH).
    #[test]
    fn sticks_fire_and_starts_land_on_allpot() {
        let mut sys = BattlezoneSystem::new();
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x1828), 0x00);
        press(&mut sys, INPUT_LEFT_FWD, true);
        press(&mut sys, INPUT_RIGHT_BACK, true);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x1828), 0x09);
        press(&mut sys, INPUT_FIRE, true);
        press(&mut sys, INPUT_START1, true);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x1828), 0x39);
        press(&mut sys, INPUT_LEFT_FWD, false);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x1828), 0x31);
    }

    /// The NMI pulse reaches the machine's interrupt line, and the self-test
    /// switch gates it off: one divider period with the switch pressed leaves
    /// no pulse pending.
    #[test]
    fn nmi_pulse_is_gated_by_the_self_test_switch() {
        let mut sys = BattlezoneSystem::new();
        for _ in 0..atari_avg::IRQ_PERIOD_CYCLES + 1 {
            sys.step_cycle();
        }
        assert!(sys.board.nmi_pending, "the tick raises the NMI pulse");
        press(&mut sys, INPUT_SERVICE, true);
        for _ in 0..atari_avg::IRQ_PERIOD_CYCLES {
            sys.step_cycle();
        }
        assert!(!sys.board.nmi_pending, "no pulse while TEST is asserted");
    }

    /// The watchdog write clears the frame counter.
    #[test]
    fn watchdog_write_clears_the_counter() {
        let mut sys = BattlezoneSystem::new();
        sys.board.watchdog_frame_count = 5;
        sys.bus_write(BusMaster::Cpu(0), 0x1400, 0x00);
        assert_eq!(sys.board.watchdog_frame_count, 0);
    }

    /// The Mathbox answers through the bus: a register write reads back as
    /// the result, like the reference's go/status/lo trio.
    #[test]
    fn mathbox_accessible() {
        let mut sys = BattlezoneSystem::new();
        sys.bus_write(BusMaster::Cpu(0), 0x1860, 0x42);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x1800), 0x00);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x1810), 0x42);
    }

    /// The POKEY ticks with the CPU: a frame leaves mixed samples behind.
    #[test]
    fn pokey_produces_audio() {
        let mut sys = BattlezoneSystem::new();
        sys.run_frame();
        let mut buf = vec![0i16; 8192];
        let n = sys.fill_audio(&mut buf);
        assert!(n > 0, "a frame of ticks leaves samples to drain");
    }

    /// The sounds latch reaches the output: motor + sound enable put signal
    /// in the buffer, and the buffer is silent with the latch clear.
    #[test]
    fn sounds_latch_reaches_the_speaker() {
        let mut sys = BattlezoneSystem::new();
        sys.run_frame();
        let mut buf = vec![0i16; 4096];
        let n = sys.fill_audio(&mut buf);
        assert!(buf[..n].iter().all(|&v| v == 0));

        sys.bus_write(BusMaster::Cpu(0), 0x1840, 0xA0); // sound + motor
        sys.run_frame();
        let n = sys.fill_audio(&mut buf);
        assert!(n > 0);
        assert!(
            buf[..n].iter().any(|&v| v != 0),
            "the engine voice is audible"
        );
    }

    /// Landscape upright cabinet: no rotation, 4:3 tube.
    #[test]
    fn upright_cabinet_needs_no_orientation() {
        let sys = BattlezoneSystem::new();
        assert_eq!(sys.orientation(), Orientation::NORMAL);
        assert!(!sys.orientation().swaps_axes());
        assert_eq!(sys.display_aspect(), Some((4, 3)));
    }

    /// A vector drawn up the display list lands up the screen: the end-to-end
    /// statement of the Y convention, mirroring the Tempest test that caught
    /// the rotation bug there.
    #[test]
    fn a_vector_drawn_up_the_display_list_lands_up_the_screen() {
        use phosphor_core::device::dvg::VectorLine;

        let (fw, fh) = (TIMING.display_width as f32, TIMING.display_height as f32);
        let mut sys = BattlezoneSystem::new();
        sys.board.display_list = vec![VectorLine {
            x0: fw / 2.0,
            y0: fh / 2.0,
            x1: fw / 2.0,
            y1: fh * 0.9,
            intensity: 15,
            r: 255,
            g: 255,
            b: 255,
            beam_cycles: 0,
            dwell_cycles: 0,
        }];

        let (w, h) = sys.display_size();
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.render_frame(&mut buf);

        let row_light = |row: u32| -> u64 {
            (0..w)
                .map(|x| buf[((row * w + x) * 3) as usize] as u64)
                .sum()
        };
        let above: u64 = (0..h / 2).map(row_light).sum();
        let below: u64 = (h / 2..h).map(row_light).sum();
        assert!(
            above > below * 4,
            "drawn up the list, it should land up the screen: {above} above, {below} below"
        );
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = BattlezoneSystem::new();

        // Set known state
        sys.board.map.region_data_mut(Region::Ram)[0x100] = 0xAA;
        sys.board.map.region_data_mut(Region::VectorRam)[0x200] = 0xBB;
        sys.io.in0 = 0xFB;
        sys.io.in3 = 0x57;
        sys.board.clock = 75_000;
        sys.board.irq_counter = 3000;
        sys.board.nmi_pending = true;
        sys.board.watchdog_frame_count = 5;

        // Save
        let data = sys.save_state().expect("save_state should return Some");
        let cpu_snap = sys.cpu.snapshot();

        // Mutate everything
        let mut sys2 = BattlezoneSystem::new();
        sys2.board.map.region_data_mut(Region::Ram)[0x100] = 0xFF;
        sys2.io.in0 = 0x00;
        sys2.board.clock = 999;
        sys2.loaded_revision = "bzonea";

        // Load
        sys2.load_state(&data).unwrap();

        // Verify CPU
        assert_eq!(sys2.cpu.snapshot(), cpu_snap);

        // Verify memory
        assert_eq!(sys2.board.map.region_data(Region::Ram)[0x100], 0xAA);
        assert_eq!(sys2.board.map.region_data(Region::VectorRam)[0x200], 0xBB);

        // Verify I/O and timing state
        assert_eq!(sys2.io.in0, 0xFB);
        assert_eq!(sys2.io.in3, 0x57);
        assert_eq!(sys2.board.clock, 75_000);
        assert_eq!(sys2.board.irq_counter, 3000);
        assert!(sys2.board.nmi_pending);
        assert_eq!(sys2.board.watchdog_frame_count, 5);
        // Loading a save restores state, never ROMs.
        assert_eq!(sys2.loaded_revision, "bzonea");
    }

    #[test]
    fn save_does_not_include_rom() {
        let mut sys = BattlezoneSystem::new();
        sys.board.map.region_data_mut(Region::ProgramRom)[0] = 0xDE;
        sys.board.map.region_data_mut(Region::VectorRom)[0] = 0xAD;

        let data = sys.save_state().unwrap();

        let mut sys2 = BattlezoneSystem::new();
        sys2.load_state(&data).unwrap();

        assert_eq!(sys2.board.map.region_data(Region::ProgramRom)[0], 0x00);
        assert_eq!(sys2.board.map.region_data(Region::VectorRom)[0], 0x00);
    }
}
