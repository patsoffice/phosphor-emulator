//! Atari Space Duel (1982).
//!
//! # Schematics
//!
//! | Drawing | Source | Pages |
//! |---|---|---|
//! | `Space Duel Main PCB Schematic Diagram`, SP-181, 2nd printing | `arcarc.xmission.com/PDF_Arcade_Atari_Kee/Space_Duel/Space_Duel_SP-181_2nd_Printing.pdf` | sheet 6A `Option Switch Input And Audio Output`, PDF p10 |
//!
//! The audio output is transcribed in
//! [`docs/schematics/space-duel-audio-output.md`](../../docs/schematics/space-duel-audio-output.md).
//!
//! # The board
//!
//! - **CPU**, a 6502 at 12.096 MHz / 8 = 1.512 MHz: 1K RAM, the vector
//!   generator with 2K of vector RAM and 6K of vector ROM, 20K of program ROM,
//!   two POKEYs, an ER2055 EAROM for scores and settings, and the player
//!   inputs. One CPU does everything; there is no sound CPU.
//! - **The AVG**, the reference driver's plain `AVG` device
//!   ([`AvgVariant::SpaceDuel`](phosphor_core::device::avg::AvgVariant)): 3-bit
//!   `color111` STAT, unswapped draw, X/Y flip from the coin latch.
//! - **Interrupts**: the 3 kHz clock (12.096 MHz / 4096) divided by 12, so an
//!   IRQ every 6144 CPU cycles, acknowledged at 0x0E00.
//!
//! Memory map:
//!
//! ```text
//!   $0000-$03FF  RAM (1 KB)
//!   $0800        IN0: coins, service, AVG done, 3 kHz clock
//!   $0900-$0907  IN3 mux: players, DSW2 options, cabinet
//!   $0A00        EAROM read
//!   $0C00        coin counters, lamps, AVG flip X/Y (write)
//!   $0C80        AVG GO
//!   $0D00        watchdog clear
//!   $0D80        AVG reset
//!   $0E00        IRQ acknowledge
//!   $0E80        EAROM control
//!   $0F00-$0F3F  EAROM write
//!   $1000-$13FF  POKEY 1 (C/D3; ALLPOT reads DSW0)
//!   $1400-$17FF  POKEY 2 (B3; ALLPOT reads DSW1)
//!   $2000-$27FF  Vector RAM (2 KB)
//!   $2800-$3FFF  Vector ROM (6 KB)
//!   $4000-$8FFF  Program ROM (20 KB, $8000 page mirrored to $9000-$FFFF)
//! ```
//!
//! What is not modeled: the watchdog (cleared but never bites), the coin
//! counters, the coin lockout and the start/select lamps on the 0x0C00 latch,
//! and the cocktail cabinet (upright only, so the flip bits stay clear).

use crate::atari_avg;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_low;
use phosphor_core::audio::{DcBlocker, SampleRing};
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind, DebugTraceBuffer};
use phosphor_core::core::display::display_settings;
use phosphor_core::core::machine::{
    ActionRole, AudioSource, DefaultBinding, DipApplyTiming, DipChoice, DipOption, DipSwitchBank,
    DipSwitches, InputConfigurable, InputControl, InputEvent, InputId, InputKind, KeyId,
    MachineCore, Nvram, PadButton, PadControl, Profilable, Renderable, SaveState, TimingConfig,
};
use phosphor_core::core::watchpoint::DebugAccessSource;
use phosphor_core::core::{AccessKind, AddressSpace16, Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::device::Er2055;
use phosphor_core::device::avg::{Avg, AvgVariant, VectorMemory};
use phosphor_core::device::dvg::{VectorLine, raster_size_for_field};
use phosphor_core::device::pokey::{Pokey, PokeyLoad};
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion, Saveable};

// ---------------------------------------------------------------------------
// ROM sets
// ---------------------------------------------------------------------------

macro_rules! rom {
    ($name:expr, $size:expr, $offset:expr, $crc:expr) => {
        RomEntry {
            name: $name,
            size: $size,
            offset: $offset,
            crc32: &[$crc],
        }
    };
}

/// Vector ROM: 2K at AVG address $800-$FFF (CPU $2800-$2FFF), 4K at
/// $1000-$1FFF (CPU $3000-$3FFF).
static VECTOR_ROM: RomRegion = RomRegion {
    size: 0x1800,
    entries: &[
        rom!("136006-106.r7", 0x0800, 0x0000, 0x691122fe),
        rom!("136006-107.np7", 0x1000, 0x0800, 0xd8dd0461),
    ],
};

/// The AVG state PROM at N4, common to both sets (and the same part as every
/// other AVG game here: CRC 5903af03).
static AVG_PROM: RomRegion = RomRegion {
    size: 0x100,
    entries: &[rom!("136002-125.n4", 0x100, 0, 0x5903af03)],
};

/// Program ROM, version 2.
static PROGRAM_V2: RomRegion = RomRegion {
    size: 0x5000,
    entries: &[
        rom!("136006-201.r1", 0x1000, 0x0000, 0xf4037b6e),
        rom!("136006-102.np1", 0x1000, 0x1000, 0x4c451e8a),
        rom!("136006-103.m1", 0x1000, 0x2000, 0xee72da63),
        rom!("136006-104.kl1", 0x1000, 0x3000, 0xe41b38a3),
        rom!("136006-105.j1", 0x1000, 0x4000, 0x5652710f),
    ],
};

/// Program ROM, version 1. Only the $4000 page differs from version 2.
static PROGRAM_V1: RomRegion = RomRegion {
    size: 0x5000,
    entries: &[
        rom!("136006-101.r1", 0x1000, 0x0000, 0xcd239e6c),
        rom!("136006-102.np1", 0x1000, 0x1000, 0x4c451e8a),
        rom!("136006-103.m1", 0x1000, 0x2000, 0xee72da63),
        rom!("136006-104.kl1", 0x1000, 0x3000, 0xe41b38a3),
        rom!("136006-105.j1", 0x1000, 0x4000, 0x5652710f),
    ],
};

// ---------------------------------------------------------------------------
// Timing and clocks
// ---------------------------------------------------------------------------

/// The 12.096 MHz crystal both the CPU and the POKEYs divide from, and the
/// AVG runs from directly.
const MASTER_CLOCK_HZ: u32 = 12_096_000;

/// 6502 at 1.512 MHz, one frame per 1/60 s. The board has no raster timing;
/// the "scanline" is the whole frame, as on the shared AVG board.
const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 1_512_000,
    cycles_per_scanline: 25_200,
    total_scanlines: 1,
    display_width: 540,
    display_height: 400,
    display_aspect: Some((4, 3)),
};

/// IRQ period: master / 4096 / 12 = 246.09 Hz, one every 6144 CPU cycles.
const IRQ_PERIOD_CYCLES: u64 = 6144;

/// AVG master-clock cycles per CPU cycle: the crystal drives the generator
/// directly and the 6502 through a divide-by-8.
const AVG_CYCLES_PER_CPU_CYCLE: u32 = 8;

// ---------------------------------------------------------------------------
// Address map
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
enum SpaceduelRegion {
    Ram = 1,
    Io = 2,
    VectorRam = 3,
    VectorRom = 4,
    ProgramRom = 5,
}

fn build_map() -> AddressSpace16 {
    let mut map = AddressSpace16::new();
    map.region(
        SpaceduelRegion::Ram,
        "RAM",
        0x0000,
        0x0400,
        AccessKind::ReadWrite,
    )
    .region(SpaceduelRegion::Io, "I/O", 0x0800, 0x1000, AccessKind::Io)
    .region(
        SpaceduelRegion::VectorRam,
        "Vector RAM",
        0x2000,
        0x0800,
        AccessKind::ReadWrite,
    )
    .region(
        SpaceduelRegion::VectorRom,
        "Vector ROM",
        0x2800,
        0x1800,
        AccessKind::ReadOnly,
    )
    .region(
        SpaceduelRegion::ProgramRom,
        "Program ROM",
        0x4000,
        0x5000,
        AccessKind::ReadOnly,
    );
    // The $8000 page repeats through $9000-$FFFF for the reset vectors.
    for page in 0..7u16 {
        map.mirror(0x9000 + page * 0x1000, 0x8000, 0x1000);
    }
    map
}

// ---------------------------------------------------------------------------
// The board
// ---------------------------------------------------------------------------

/// Everything the 6502 talks to. The CPU lives on the machine so a cycle can
/// dispatch at a concrete type (see `docs/designs/concrete-bus-dispatch.md`).
#[derive(BusDebug, DebugTrace, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct SpaceduelBoard {
    #[debug_map(cpu = 0)]
    #[save(id = 1)]
    map: AddressSpace16,
    #[debug_device("AVG")]
    #[save(id = 2)]
    avg: Avg,
    /// What the tube shows: the list the generator finished since the last
    /// GO. Redrawn after a load rather than restored.
    #[save_skip(default)]
    display_list: Vec<VectorLine>,

    /// CPU cycles since power-on; bit 8 is the 3 kHz input at 0x0800.
    #[save(id = 3)]
    clock: u64,
    #[save(id = 4)]
    irq_counter: u64,
    #[save(id = 5)]
    irq_pending: bool,
    /// Frames since the watchdog was cleared; eight without one resets.
    #[save(id = 6)]
    watchdog_frame_count: u8,

    /// IN0 at 0x0800, coins and service, active low. Bits 6-7 are generated.
    #[save(id = 7)]
    in0: u8,
    /// IN3 (player 1) and IN4 (player 2), active high: 1 means pressed.
    #[save(id = 8)]
    in3: u8,
    #[save(id = 9)]
    in4: u8,
    /// DSW0 at D4 and DSW1 at B4, read through the POKEYs' ALLPOT, and DSW2
    /// on the P10/11 jumpers, read through the IN3 mux. Board configuration,
    /// so not saved.
    #[save_skip]
    dsw0: u8,
    #[save_skip]
    dsw1: u8,
    #[save_skip]
    dsw2: u8,
    /// Cabinet jumper, bit 7: upright only here, so always zero.
    #[save_skip]
    cabinet: u8,

    /// POKEY 1 (C/D3) and POKEY 2 (B3) at 1.512 MHz.
    #[save(id = 10)]
    pokey1: Pokey,
    #[save(id = 11)]
    pokey2: Pokey,
    /// ER2055 EAROM for scores and settings.
    #[save(id = 12)]
    earom: Er2055,

    // Audio: see `mix_audio`.
    #[save(id = 13)]
    b1_low_pass: DcBlocker,
    #[save(id = 14)]
    b_coupling: DcBlocker,
    #[save(id = 15)]
    a1_low_pass: DcBlocker,
    #[save(id = 16)]
    a_coupling: DcBlocker,
    #[save(id = 17)]
    a2_low_pass: DcBlocker,
    #[save(id = 18)]
    a3_low_pass: DcBlocker,
    #[save(id = 19)]
    mixer_low_pass: DcBlocker,

    #[debug_events]
    #[save_skip]
    debug_trace: DebugTraceBuffer,
}

/// A one-pole low-pass as the input less a one-pole high-pass at the same
/// corner, which is the low-pass exactly.
fn low_pass(stage: &mut DcBlocker, x: f32) -> f32 {
    x - stage.process(x)
}

fn corner(ohms: f64, farads: f64) -> f32 {
    (1.0 / (std::f64::consts::TAU * ohms * farads)) as f32
}

/// Each POKEY's AUD pin goes straight to an LM324 inverting input, a zero-ohm
/// virtual ground. C/D3's reference is +5 V off R49; B3's was not traced, and
/// the reference cancels in the normalization below in any case.
const POKEY_LOAD: PokeyLoad = PokeyLoad::VirtualGround {
    series_ohms: 0.0,
    reference_v: 5.0,
};

/// Mixer weight of the B3 chain against C/D3's: R51 3.3k over R45 10k.
/// Every device on both chips is scaled to 1.0.
const B3_WEIGHT: f32 = 3.3 / 10.0;
const SCALE: f32 = 1.0 / (1.0 + B3_WEIGHT);

impl SpaceduelBoard {
    fn new() -> Self {
        let rate = phosphor_core::audio::host_sample_rate();
        let mut pokey1 = Pokey::with_clock(MASTER_CLOCK_HZ / 8, rate);
        pokey1.set_output_load(POKEY_LOAD);
        let mut pokey2 = Pokey::with_clock(MASTER_CLOCK_HZ / 8, rate);
        pokey2.set_output_load(POKEY_LOAD);
        Self {
            map: build_map(),
            avg: Avg::with_variant(
                AvgVariant::SpaceDuel,
                TIMING.display_width as i32,
                TIMING.display_height as i32,
            ),
            display_list: Vec::with_capacity(2048),
            clock: 0,
            irq_counter: 0,
            irq_pending: false,
            watchdog_frame_count: 0,
            in0: 0xFF,
            in3: 0x00,
            in4: 0x00,
            dsw0: 0x01,
            dsw1: 0x00,
            dsw2: 0x07,
            cabinet: 0x00,
            pokey1,
            pokey2,
            earom: Er2055::new(),
            // Sheet 6A corners: B1 R50/C32, C31 into R48; A1 R46/C29, C30
            // into R44; A2 R42/C27; A3 R43/C28; mixer R51/C33.
            b1_low_pass: DcBlocker::with_cutoff(corner(1_000.0, 0.22e-6), rate),
            b_coupling: DcBlocker::with_cutoff(corner(39_000.0, 0.22e-6), rate),
            a1_low_pass: DcBlocker::with_cutoff(corner(1_000.0, 0.1e-6), rate),
            a_coupling: DcBlocker::with_cutoff(corner(100_000.0, 0.22e-6), rate),
            a2_low_pass: DcBlocker::with_cutoff(corner(1_000_000.0, 1e-9), rate),
            a3_low_pass: DcBlocker::with_cutoff(corner(1_000_000.0, 1e-9), rate),
            mixer_low_pass: DcBlocker::with_cutoff(corner(3_300.0, 0.1e-6), rate),
            debug_trace: DebugTraceBuffer::new(),
        }
    }

    /// IN0 at 0x0800: coins and service below, AVG done and the clock above.
    fn in0(&self) -> u8 {
        let mut v = self.in0 & 0x3F;
        v |= u8::from(self.avg.is_halted()) << 6;
        v |= u8::from(self.clock & 0x100 != 0) << 7;
        v
    }

    /// The IN3 mux at 0x0900-0x0907: two players' controls spread over eight
    /// reads, with DSW2's option jumpers and the cabinet mixed into the top
    /// three. Bit positions follow the reference driver exactly.
    fn in3_mux(&self, offset: u16) -> u8 {
        let mut res = 0u8;
        match offset & 7 {
            0 => {
                if self.in3 & 0x08 != 0 {
                    res |= 0x80;
                }
                if self.in3 & 0x04 != 0 {
                    res |= 0x40;
                }
            }
            1 => {
                if self.in4 & 0x08 != 0 {
                    res |= 0x80;
                }
                if self.in4 & 0x04 != 0 {
                    res |= 0x40;
                }
            }
            2 => {
                if self.in3 & 0x01 != 0 {
                    res |= 0x80;
                }
                if self.in3 & 0x02 != 0 {
                    res |= 0x40;
                }
            }
            3 => {
                if self.in4 & 0x01 != 0 {
                    res |= 0x80;
                }
                if self.in4 & 0x02 != 0 {
                    res |= 0x40;
                }
            }
            4 => {
                if self.in3 & 0x10 != 0 {
                    res |= 0x80;
                }
                if self.in3 & 0x20 != 0 {
                    res |= 0x40;
                }
            }
            5 => {
                if self.in4 & 0x10 != 0 {
                    res |= 0x80;
                }
                if self.dsw2 & 0x01 == 0 {
                    res |= 0x40;
                }
            }
            6 => {
                if self.in3 & 0x40 != 0 {
                    res |= 0x80;
                }
                if self.dsw2 & 0x02 == 0 {
                    res |= 0x40;
                }
            }
            _ => {
                res = self.cabinet;
                if self.dsw2 & 0x04 == 0 {
                    res |= 0x40;
                }
            }
        }
        res
    }

    /// AVG GO: restart the generator, dropping whatever it drew since the
    /// last frame boundary.
    fn trigger_avg(&mut self) {
        if self.debug_trace.enabled() {
            self.debug_trace.record(DebugEvent {
                cpu_index: Some(0),
                pc: self.map.latched_pc(),
                device: Some("AVG"),
                detail: Some("vector generator start"),
                ..DebugEvent::new(
                    self.clock,
                    DebugAccessSource::Cpu(0),
                    DebugEventKind::DeviceWrite,
                )
            });
        }
        self.avg.go();
        self.avg.take_display_list();
    }

    /// Run the vector generator for one CPU cycle's worth of its own clock.
    fn step_avg(&mut self) {
        let mem = VectorMemory::split(
            self.map.region_data(SpaceduelRegion::VectorRam),
            self.map.region_data(SpaceduelRegion::VectorRom),
            0x0800,
        );
        // No color RAM on this board; the STAT carries color111 directly.
        if self.avg.step(AVG_CYCLES_PER_CPU_CYCLE, &mem, &[]) {
            let list = self.avg.take_display_list();
            if self.avg.is_halted() {
                // The list ends in HALT, and that pass is the frame.
                self.display_list = list;
            }
            // Otherwise the list branched back to address 0 mid-pass, which
            // clears the beam path on hardware; the pass is dropped.
        }
    }

    /// Clock the 246 Hz interrupt source. Compare before incrementing, as on
    /// the shared AVG board: incrementing first lands the assert a cycle
    /// early against the reference.
    fn clock_interrupts(&mut self) {
        if self.irq_counter >= IRQ_PERIOD_CYCLES {
            self.irq_counter = 0;
            self.irq_pending = true;
        }
        self.irq_counter += 1;
    }

    fn trace_write(
        &mut self,
        addr: u16,
        device: Option<&'static str>,
        detail: Option<&'static str>,
    ) {
        if self.debug_trace.enabled() {
            self.debug_trace.record(DebugEvent {
                cpu_index: Some(0),
                pc: self.map.latched_pc(),
                addr: Some(addr as u32),
                device,
                detail,
                ..DebugEvent::new(
                    self.clock,
                    DebugAccessSource::Cpu(0),
                    DebugEventKind::DeviceWrite,
                )
            });
        }
    }

    fn render(&self, buffer: &mut [u8]) {
        let field = TIMING.display_size();
        let (rw, rh) = raster_size_for_field(field.0, field.1);
        crate::atari_dvg::rasterize_vectors(
            &self.display_list,
            buffer,
            rw,
            rh,
            field,
            true,
            &display_settings().without_halation(),
        );
    }
}

// ---------------------------------------------------------------------------
// The CPU's bus
// ---------------------------------------------------------------------------

impl Bus for SpaceduelBoard {
    type Address = u16;
    type Data = u8;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let data = match self.map.page(addr).region_id {
            SpaceduelRegion::RAM
            | SpaceduelRegion::VECTOR_RAM
            | SpaceduelRegion::VECTOR_ROM
            | SpaceduelRegion::PROGRAM_ROM => self.map.read_backing(addr),
            SpaceduelRegion::IO => match addr {
                // IN0: coins and service; done and clock generated.
                0x0800 => self.in0(),
                // IN3 mux: players, DSW2 options, cabinet.
                0x0900..=0x0907 => self.in3_mux(addr),
                // EAROM data register.
                0x0A00 => self.earom.read_latched(),
                // POKEY 1 (C/D3): ALLPOT is wired to DSW0.
                0x1000..=0x13FF => {
                    let reg = addr & 0x0F;
                    if reg == 0x08 {
                        self.dsw0
                    } else {
                        self.pokey1.read(reg)
                    }
                }
                // POKEY 2 (B3): ALLPOT is wired to DSW1.
                0x1400..=0x17FF => {
                    let reg = addr & 0x0F;
                    if reg == 0x08 {
                        self.dsw1
                    } else {
                        self.pokey2.read(reg)
                    }
                }
                _ => 0,
            },
            phosphor_core::core::UNMAPPED => 0,
            _ => 0,
        };
        self.map.watch_read(0, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.map.watch_write(0, master, addr, data);
        let region = self.map.page(addr).region_id;
        if region == SpaceduelRegion::IO {
            match addr {
                // Coin latch: counters, lockout and lamps are bookkeeping
                // with no emulation effect; the flip bits reach the AVG.
                // Both flip when their bit is CLEAR.
                0x0C00 => {
                    self.trace_write(addr, None, Some("coin latch"));
                    self.avg.set_flip(data & 0x40 == 0, data & 0x80 == 0);
                }
                // AVG GO.
                0x0C80 => self.trigger_avg(),
                // Watchdog clear.
                0x0D00 => {
                    self.watchdog_frame_count = 0;
                }
                // AVG reset.
                0x0D80 => {
                    self.trace_write(addr, Some("AVG"), Some("vector generator reset"));
                    self.avg.reset();
                }
                // IRQ acknowledge.
                0x0E00 => {
                    self.irq_pending = false;
                }
                // EAROM control: CK = DB0, C1 = /DB2, C2 = DB1, CS1 = DB3.
                0x0E80 => {
                    self.earom.write_control(
                        data & 0x01 != 0,
                        data & 0x08 != 0,
                        data & 0x04 == 0,
                        data & 0x02 != 0,
                    );
                }
                // EAROM write: address in the low six bits.
                0x0F00..=0x0F3F => {
                    self.earom.latch(addr & 0x3F, data);
                }
                // POKEY 1 and 2, including the POTGO strobe at reg 8.
                0x1000..=0x13FF => self.pokey1.write(addr & 0x0F, data),
                0x1400..=0x17FF => self.pokey2.write(addr & 0x0F, data),
                _ => {}
            }
        } else if region == SpaceduelRegion::RAM || region == SpaceduelRegion::VECTOR_RAM {
            self.map.write_backing(addr, data);
        }
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            irq: self.irq_pending,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// The machine
// ---------------------------------------------------------------------------

/// Atari Space Duel, both ROM revisions on the one board.
#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct SpaceduelSystem {
    #[debug_cpu("M6502")]
    #[save(id = 1)]
    cpu: M6502,
    #[debug_bus]
    #[save(id = 2)]
    board: SpaceduelBoard,
    #[save_skip(default)]
    audio_buffer: SampleRing<i16>,
}

/// One ROM revision's program ROM. Version 1 and 2 differ only in the $4000
/// page; the vector ROM and the AVG PROM are shared, so one config each.
pub struct SpaceduelRomConfig {
    program: &'static RomRegion,
}

/// Space Duel, version 2.
pub static SPACEDUEL_CONFIG: SpaceduelRomConfig = SpaceduelRomConfig {
    program: &PROGRAM_V2,
};

/// Space Duel, version 1.
pub static SPACEDUEL1_CONFIG: SpaceduelRomConfig = SpaceduelRomConfig {
    program: &PROGRAM_V1,
};

/// Newest revision first: a set matches the first config whose files it has.
const ALL_CONFIGS: &[&SpaceduelRomConfig] = &[&SPACEDUEL_CONFIG, &SPACEDUEL1_CONFIG];

/// One CPU cycle: the two POKEYs, the IRQ clock and the AVG, then the 6502.
#[inline]
fn tick(cpu: &mut M6502, board: &mut SpaceduelBoard) {
    board.pokey1.tick();
    board.pokey2.tick();
    board.clock_interrupts();
    if board.map.has_any_watchpoints() || board.debug_trace.enabled() {
        let pc = cpu.at_instruction_boundary().then_some(cpu.pc as u32);
        board.map.latch_access_context(board.clock, pc);
    }
    board.step_avg();
    cpu.execute_cycle(board, BusMaster::Cpu(0));
    board.clock += 1;
}

impl SpaceduelSystem {
    pub fn new() -> Self {
        Self {
            cpu: M6502::new(),
            board: SpaceduelBoard::new(),
            audio_buffer: SampleRing::with_capacity(2048),
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        self.load_roms(rom_set, &SPACEDUEL_CONFIG)
    }

    fn load_roms(
        &mut self,
        rom_set: &RomSet,
        config: &SpaceduelRomConfig,
    ) -> Result<(), RomLoadError> {
        let b = &mut self.board;
        b.map
            .load_region(SpaceduelRegion::ProgramRom, &config.program.load(rom_set)?);
        b.map
            .load_region(SpaceduelRegion::VectorRom, &VECTOR_ROM.load(rom_set)?);
        b.avg.load_state_prom(&AVG_PROM.load(rom_set)?);
        Ok(())
    }

    /// Advance one CPU cycle, returning the instruction-boundary mask.
    pub fn step_cycle(&mut self) -> u32 {
        tick(&mut self.cpu, &mut self.board);
        u32::from(self.cpu.at_instruction_boundary())
    }

    /// Read the CPU-facing bus, side effects and all. Distinct from the
    /// debugger's `BusDebug::peek`/`poke`, which avoid side effects.
    pub fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        self.board.read(master, addr)
    }

    /// Write the CPU-facing bus, side effects and all. See [`Self::bus_read`].
    pub fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.board.write(master, addr, data)
    }

    /// Drain both POKEYs' currents and carry them to the speaker, as sheet 6A
    /// does. Refdes and values are in
    /// `docs/schematics/space-duel-audio-output.md`.
    ///
    /// Each chip works into a zero-ohm virtual ground, so its output is the
    /// current its devices sink; one chip with every device on is 1.0. C/D3
    /// (POKEY 1) passes its 723 Hz first stage and 18.5 Hz coupling to the
    /// mixer at 1; B3 (POKEY 2) passes its 1.59 kHz first stage, 7.2 Hz
    /// coupling, and the -10 and -1 stages at 159 Hz to the mixer at 0.33.
    /// The unity inverters contribute no frequency effect and are omitted,
    /// and each chain inverts four times, so the two add in phase. The mixer
    /// low-passes the sum at 482 Hz. Every device on both chips is 1.0.
    fn mix_audio(&mut self) {
        let b = &mut self.board;
        let i1 = b.pokey1.drain_audio();
        let i2 = b.pokey2.drain_audio();
        let full = POKEY_LOAD.full_scale() as f32;
        let len = i1.len().min(i2.len());
        for i in 0..len {
            let vb = low_pass(&mut b.b1_low_pass, i1[i] / full);
            let vb = b.b_coupling.process(vb);
            let va = low_pass(&mut b.a1_low_pass, i2[i] / full);
            let va = b.a_coupling.process(va);
            let va = 10.0 * low_pass(&mut b.a2_low_pass, va);
            let va = low_pass(&mut b.a3_low_pass, va);
            let mixed = low_pass(&mut b.mixer_low_pass, SCALE * (vb + B3_WEIGHT * va));
            self.audio_buffer
                .push((mixed * 32767.0).clamp(i16::MIN as f32, i16::MAX as f32) as i16);
        }
    }
}

impl Default for SpaceduelSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Controls
// ---------------------------------------------------------------------------

const INPUT_COIN1: u8 = 0;
const INPUT_COIN2: u8 = 1;
const INPUT_P1_LEFT: u8 = 2;
const INPUT_P1_RIGHT: u8 = 3;
const INPUT_P1_FIRE: u8 = 4;
const INPUT_P1_THRUST: u8 = 5;
const INPUT_P1_SHIELD: u8 = 6;
const INPUT_P1_START: u8 = 7;
const INPUT_SELECT: u8 = 8;
const INPUT_P2_LEFT: u8 = 9;
const INPUT_P2_RIGHT: u8 = 10;
const INPUT_P2_FIRE: u8 = 11;
const INPUT_P2_THRUST: u8 = 12;
const INPUT_P2_SHIELD: u8 = 13;
const INPUT_SERVICE: u8 = 14;
const INPUT_DIAG: u8 = 15;

/// Set a bit in an active-high input byte: set on press, clear on release.
fn set_bit_active_high(reg: &mut u8, bit: u8, pressed: bool) {
    if pressed {
        *reg |= 1 << bit;
    } else {
        *reg &= !(1 << bit);
    }
}

const SPACEDUEL_CONTROLS: &[InputControl] = &[
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
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P1_LEFT as u16),
        stable_name: "p1_left",
        label: "P1 Rotate Left",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(INPUT_P1_RIGHT as u16),
        stable_name: "p1_right",
        label: "P1 Rotate Right",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
    InputControl {
        id: InputId(INPUT_P1_FIRE as u16),
        stable_name: "p1_fire",
        label: "P1 Fire",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P1_THRUST as u16),
        stable_name: "p1_thrust",
        label: "P1 Thrust",
        kind: InputKind::Action(ActionRole::Secondary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P1_SHIELD as u16),
        stable_name: "p1_shield",
        label: "P1 Shield",
        kind: InputKind::Action(ActionRole::Tertiary),
        player: Some(1),
        default_bindings: &[],
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
        id: InputId(INPUT_SELECT as u16),
        stable_name: "select",
        label: "Select (2P Start)",
        kind: InputKind::Start,
        player: Some(1),
        default_bindings: crate::input_defaults::P2_START,
    },
    // Player 2 plays along, so every control needs a default. Fire rides
    // Primary (RShift, the one role the ladder differentiates per player);
    // thrust and shield stay plain Buttons with explicit keys because
    // Secondary and Tertiary share Space and LCtrl across both players.
    // The pads mirror P1's A/B/X, slot-scoped to player 2.
    InputControl {
        id: InputId(INPUT_P2_LEFT as u16),
        stable_name: "p2_left",
        label: "P2 Rotate Left",
        kind: InputKind::Button,
        player: Some(2),
        default_bindings: crate::input_defaults::P2_LEFT,
    },
    InputControl {
        id: InputId(INPUT_P2_RIGHT as u16),
        stable_name: "p2_right",
        label: "P2 Rotate Right",
        kind: InputKind::Button,
        player: Some(2),
        default_bindings: crate::input_defaults::P2_RIGHT,
    },
    InputControl {
        id: InputId(INPUT_P2_FIRE as u16),
        stable_name: "p2_fire",
        label: "P2 Fire",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(2),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P2_THRUST as u16),
        stable_name: "p2_thrust",
        label: "P2 Thrust",
        kind: InputKind::Button,
        player: Some(2),
        default_bindings: &[
            DefaultBinding::Key(KeyId::Enter),
            DefaultBinding::Pad(PadControl::Button(PadButton::B)),
        ],
    },
    InputControl {
        id: InputId(INPUT_P2_SHIELD as u16),
        stable_name: "p2_shield",
        label: "P2 Shield",
        kind: InputKind::Button,
        player: Some(2),
        default_bindings: &[
            DefaultBinding::Key(KeyId::RCtrl),
            DefaultBinding::Pad(PadControl::Button(PadButton::X)),
        ],
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
        id: InputId(INPUT_DIAG as u16),
        stable_name: "diag_step",
        label: "Diagnostic Step",
        kind: InputKind::Service,
        player: None,
        default_bindings: &[],
    },
];

impl InputConfigurable for SpaceduelSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        SPACEDUEL_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let b = &mut self.board;
        match event {
            InputEvent::Button { id, pressed } => match id.0 as u8 {
                // IN0, active low.
                INPUT_COIN1 => set_bit_active_low(&mut b.in0, 1, pressed),
                INPUT_COIN2 => set_bit_active_low(&mut b.in0, 0, pressed),
                INPUT_SERVICE => set_bit_active_low(&mut b.in0, 4, pressed),
                INPUT_DIAG => set_bit_active_low(&mut b.in0, 5, pressed),
                // IN3/IN4, active high. Fire is bit 2 on both players;
                // shield is bit 3 and thrust is bit 4, confirmed by playtest:
                // Space (the Secondary default on thrust) shielded before
                // the swap.
                INPUT_P1_LEFT => set_bit_active_high(&mut b.in3, 0, pressed),
                INPUT_P1_RIGHT => set_bit_active_high(&mut b.in3, 1, pressed),
                INPUT_P1_FIRE => set_bit_active_high(&mut b.in3, 2, pressed),
                INPUT_P1_SHIELD => set_bit_active_high(&mut b.in3, 3, pressed),
                INPUT_P1_THRUST => set_bit_active_high(&mut b.in3, 4, pressed),
                INPUT_P1_START => set_bit_active_high(&mut b.in3, 5, pressed),
                INPUT_SELECT => set_bit_active_high(&mut b.in3, 6, pressed),
                INPUT_P2_LEFT => set_bit_active_high(&mut b.in4, 0, pressed),
                INPUT_P2_RIGHT => set_bit_active_high(&mut b.in4, 1, pressed),
                INPUT_P2_FIRE => set_bit_active_high(&mut b.in4, 2, pressed),
                INPUT_P2_SHIELD => set_bit_active_high(&mut b.in4, 3, pressed),
                INPUT_P2_THRUST => set_bit_active_high(&mut b.in4, 4, pressed),
                _ => {}
            },
            InputEvent::Relative { .. } => {}
            InputEvent::Absolute { .. } => {}
        }
    }

    fn release_all_inputs(&mut self) {
        phosphor_core::core::machine::release_all_controls(self);
    }
}

// ---------------------------------------------------------------------------
// Machine traits
// ---------------------------------------------------------------------------

impl Renderable for SpaceduelSystem {
    fn display_size(&self) -> (u32, u32) {
        let (w, h) = TIMING.display_size();
        raster_size_for_field(w, h)
    }

    fn vector_field_size(&self) -> Option<(u32, u32)> {
        Some(TIMING.display_size())
    }

    fn display_aspect(&self) -> Option<(u32, u32)> {
        TIMING.display_aspect()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        self.board.render(buffer);
    }

    fn vector_display_list(&self) -> Option<&[VectorLine]> {
        Some(&self.board.display_list)
    }
}

impl AudioSource for SpaceduelSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.audio_buffer.pop_front_into(buffer)
    }

    fn audio_sample_rate(&self) -> u32 {
        phosphor_core::audio::host_sample_rate()
    }
}

crate::impl_board_debug!(SpaceduelSystem, board, TIMING);
crate::impl_board_debug_trace!(SpaceduelSystem, board);

impl MachineCore for SpaceduelSystem {
    fn frame_rate_hz(&self) -> f64 {
        TIMING.frame_rate_hz()
    }

    fn machine_id(&self) -> &str {
        "spaceduel"
    }

    crate::machine_clock_declaration!(TIMING, atari_avg::clock_tree);

    fn run_frame(&mut self) {
        for _ in 0..TIMING.cycles_per_frame() {
            tick(&mut self.cpu, &mut self.board);
        }
        self.mix_audio();

        // Watchdog: eight frames without a clear resets the board.
        self.board.watchdog_frame_count += 1;
        if self.board.watchdog_frame_count >= 8 {
            self.reset();
        }
    }

    fn reset(&mut self) {
        let b = &mut self.board;
        b.avg.reset();
        b.display_list.clear();
        b.irq_counter = 0;
        b.irq_pending = false;
        b.watchdog_frame_count = 0;
        b.pokey1.reset();
        b.pokey2.reset();
        b.earom.reset();
        b.b1_low_pass.reset();
        b.b_coupling.reset();
        b.a1_low_pass.reset();
        b.a_coupling.reset();
        b.a2_low_pass.reset();
        b.a3_low_pass.reset();
        b.mixer_low_pass.reset();
        b.avg.set_flip(false, false);
        self.audio_buffer.clear();
        self.cpu.reset(&mut self.board, BusMaster::Cpu(0));
    }
}

impl SaveState for SpaceduelSystem {
    crate::machine_save_state!();
}

impl Nvram for SpaceduelSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.earom.snapshot())
    }

    fn load_nvram(&mut self, data: &[u8]) {
        self.board.earom.load_from(data);
    }
}

impl Profilable for SpaceduelSystem {}

// ---------------------------------------------------------------------------
// DIP switches
// ---------------------------------------------------------------------------

const fn choice(label: &'static str, value: u8) -> DipChoice {
    DipChoice { label, value }
}

const fn option(name: &'static str, mask: u8, choices: &'static [DipChoice]) -> DipOption {
    DipOption {
        name,
        mask,
        apply: DipApplyTiming::Immediate,
        choices,
        conditional: &[],
    }
}

/// DSW0 at D4: lives, difficulty, language, bonus life. Choice values follow
/// the reference driver's `spacduel` layout; the power-on default is 0x01
/// (3 ships, normal difficulty, English, 10K bonus).
const DSW0: DipSwitchBank = DipSwitchBank {
    name: "DSW0 (D4)",
    options: &[
        option(
            "Lives",
            0x03,
            &[
                choice("4", 0x00),
                choice("3", 0x01),
                choice("6", 0x02),
                choice("5", 0x03),
            ],
        ),
        option(
            "Difficulty",
            0x0C,
            &[
                choice("Normal", 0x00),
                choice("Easy", 0x04),
                choice("Hard", 0x08),
                choice("Medium", 0x0C),
            ],
        ),
        option(
            "Language",
            0x30,
            &[
                choice("English", 0x00),
                choice("German", 0x10),
                choice("French", 0x20),
                choice("Spanish", 0x30),
            ],
        ),
        option(
            "Bonus Life",
            0xC0,
            &[
                choice("10000", 0x00),
                choice("15000", 0x40),
                choice("None", 0x80),
                choice("8000", 0xC0),
            ],
        ),
    ],
};

/// DSW1 at B4: coinage. Power-on default 0x00 (1 coin 1 credit, both mechs
/// x1, no bonus coins).
const DSW1: DipSwitchBank = DipSwitchBank {
    name: "DSW1 (B4)",
    options: &[
        option(
            "Coinage",
            0x03,
            &[
                choice("1 Coin/1 Credit", 0x00),
                choice("2 Coins/1 Credit", 0x01),
                choice("Free Play", 0x02),
                choice("1 Coin/2 Credits", 0x03),
            ],
        ),
        option(
            "Right Coin Mechanism",
            0x0C,
            &[
                choice("x1", 0x00),
                choice("x4", 0x04),
                choice("x5", 0x08),
                choice("x6", 0x0C),
            ],
        ),
        option(
            "Left Coin Mechanism",
            0x10,
            &[choice("x1", 0x00), choice("x2", 0x10)],
        ),
        option(
            "Bonus Coins",
            0xE0,
            &[
                choice("None", 0x00),
                choice("1 each 2", 0x20),
                choice("1 each 4", 0x40),
                choice("1 each 5", 0x80),
                choice("1 each 3", 0xA0),
                choice("2 each 4", 0x60),
            ],
        ),
    ],
};

/// DSW2 on the P10/11 option jumpers, read through the IN3 mux rather than a
/// POKEY. Only bits 0-2 connect; bit 3 is absent on the PCB. Default 0x07.
const DSW2: DipSwitchBank = DipSwitchBank {
    name: "DSW2 (P10/11)",
    options: &[
        option(
            "Charge By",
            0x01,
            &[choice("Game", 0x00), choice("Player", 0x01)],
        ),
        option(
            "2-Credit Minimum",
            0x02,
            &[choice("On", 0x00), choice("Off", 0x02)],
        ),
        option(
            "1-Player Game Only",
            0x04,
            &[choice("On", 0x00), choice("Off", 0x04)],
        ),
    ],
};

/// The three banks both revisions share.
const SPACEDUEL_DIP_BANKS: &[DipSwitchBank] = &[DSW0, DSW1, DSW2];

impl DipSwitches for SpaceduelSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        SPACEDUEL_DIP_BANKS
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        match bank {
            0 => self.board.dsw0,
            1 => self.board.dsw1,
            2 => self.board.dsw2,
            _ => 0,
        }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        match bank {
            0 => self.board.dsw0 = value,
            1 => self.board.dsw1 = value,
            2 => self.board.dsw2 = value,
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

// One board, two ROM revisions: the registry tries each config in
// ALL_CONFIGS order and the first whose files the set has wins.
crate::register_machine!(
    SpaceduelSystem,
    "spaceduel",
    &["spacduel", "spacduel1"],
    SPACEDUEL_CONTROLS,
    configs = ALL_CONFIGS
);

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::DipSwitches;
    use phosphor_core::cpu::CpuStateTrait;

    fn read(sys: &mut SpaceduelSystem, addr: u16) -> u8 {
        sys.bus_read(BusMaster::Cpu(0), addr)
    }

    /// All three banks' tables are valid against the power-on switch bytes.
    #[test]
    fn dip_tables_are_valid() {
        let sys = SpaceduelSystem::new();
        assert_eq!(sys.dip_bank_value(0), 0x01);
        assert_eq!(sys.dip_bank_value(1), 0x00);
        assert_eq!(sys.dip_bank_value(2), 0x07);
        crate::assert_dip_banks_valid(sys.dip_banks(), &[0x01, 0x00, 0x07]);
    }

    /// The IN3 mux spreads the players, the DSW2 jumpers and the cabinet over
    /// eight reads at 0x0900-0x0907, in the reference driver's bit positions.
    #[test]
    fn in3_mux_spreads_players_options_and_cabinet() {
        let mut sys = SpaceduelSystem::new();
        // P1: rotate left + fire; P2: BUTTON3 + thrust; jumpers clear.
        sys.board.in3 = 0x01 | 0x04;
        sys.board.in4 = 0x08 | 0x10;
        sys.board.dsw2 = 0x00;
        sys.board.cabinet = 0x00;
        assert_eq!(read(&mut sys, 0x0900), 0x40, "P1 fire lands on bit 6");
        assert_eq!(read(&mut sys, 0x0901), 0x80, "P2 BUTTON3 lands on bit 7");
        assert_eq!(
            read(&mut sys, 0x0902),
            0x80,
            "P1 rotate left lands on bit 7"
        );
        assert_eq!(read(&mut sys, 0x0903), 0x00, "P2 has no rotate here");
        assert_eq!(read(&mut sys, 0x0904), 0x00, "P1 has no thrust/start here");
        assert_eq!(
            read(&mut sys, 0x0905),
            0xC0,
            "P2 thrust plus the charge-by jumper"
        );
        assert_eq!(read(&mut sys, 0x0906), 0x40, "only the 2-credit jumper");
        assert_eq!(read(&mut sys, 0x0907), 0x40, "only the 1-player jumper");

        // Setting the jumpers clears the option bits; pressing P1 select
        // sets offset 6's high bit.
        sys.board.dsw2 = 0x07;
        sys.board.in3 = 0x40;
        assert_eq!(
            read(&mut sys, 0x0905),
            0x80,
            "jumper set, P2 thrust still stands"
        );
        assert_eq!(read(&mut sys, 0x0906), 0x80, "P1 select lands on bit 7");
        assert_eq!(read(&mut sys, 0x0907), 0x00, "cabinet upright, jumper set");
    }

    /// P1 shield rides the Tertiary rung (LCtrl): the third ranked action
    /// after fire (Primary) and thrust (Secondary).
    #[test]
    fn p1_shield_rides_the_tertiary_ladder() {
        use phosphor_core::core::machine::InputConfigurable;
        let sys = SpaceduelSystem::new();
        let shield = sys
            .input_controls()
            .iter()
            .find(|c| c.stable_name == "p1_shield")
            .expect("p1_shield control exists");
        assert_eq!(
            shield.kind,
            InputKind::Action(ActionRole::Tertiary),
            "P1 shield must carry a default key (LCtrl)"
        );
    }

    /// Both players can play out of the box: every P2 control resolves to
    /// at least one physical default, through the role ladder or inline.
    /// Space Duel runs both players at once, so an unbound P2 control is a
    /// control P2 cannot reach without rebinding.
    #[test]
    fn p2_controls_all_have_defaults() {
        use phosphor_core::core::machine::InputConfigurable;
        let sys = SpaceduelSystem::new();
        for name in ["p2_left", "p2_right", "p2_fire", "p2_thrust", "p2_shield"] {
            let control = sys
                .input_controls()
                .iter()
                .find(|c| c.stable_name == name)
                .unwrap_or_else(|| panic!("{name} control exists"));
            let role: &[DefaultBinding] = match control.kind {
                InputKind::Action(role) => role.default_bindings(control.player),
                _ => &[],
            };
            assert!(
                !role.is_empty() || !control.default_bindings.is_empty(),
                "{name} has no default binding"
            );
        }
    }

    /// Thrust and shield land on the playtest-confirmed bits: pressing the
    /// thrust control fires the game's bit 4, shield its bit 3, on both
    /// players, and the mux port shows them where the game reads them
    /// (thrust on offsets 4/5, shield on offsets 0/1). Space (the Secondary
    /// default) thrusts.
    #[test]
    fn thrust_and_shield_land_on_the_playtest_confirmed_bits() {
        use phosphor_core::core::machine::InputConfigurable;
        let mut sys = SpaceduelSystem::new();
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_P1_THRUST as u16),
            pressed: true,
        });
        assert_eq!(sys.board.in3, 0x10, "P1 thrust is bit 4");
        assert_eq!(
            read(&mut sys, 0x0904) & 0x80,
            0x80,
            "P1 thrust reaches the game"
        );
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_P1_SHIELD as u16),
            pressed: true,
        });
        assert_eq!(sys.board.in3, 0x18, "P1 shield is bit 3");
        assert_eq!(
            read(&mut sys, 0x0900) & 0x80,
            0x80,
            "P1 shield reaches the game"
        );
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_P2_THRUST as u16),
            pressed: true,
        });
        assert_eq!(sys.board.in4, 0x10, "P2 thrust is bit 4");
        assert_eq!(
            read(&mut sys, 0x0905) & 0x80,
            0x80,
            "P2 thrust reaches the game"
        );
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_P2_SHIELD as u16),
            pressed: true,
        });
        assert_eq!(sys.board.in4, 0x18, "P2 shield is bit 3");
        assert_eq!(
            read(&mut sys, 0x0901) & 0x80,
            0x80,
            "P2 shield reaches the game"
        );
    }

    /// Both POKEYs' ALLPOT registers read their DIP bank, not the pot scan.
    #[test]
    fn allpot_reads_the_dip_banks() {
        let mut sys = SpaceduelSystem::new();
        sys.board.dsw0 = 0xA5;
        sys.board.dsw1 = 0x5A;
        assert_eq!(read(&mut sys, 0x1008), 0xA5);
        assert_eq!(read(&mut sys, 0x1408), 0x5A);
        // Mirrors of the POKEY windows decode the same way.
        assert_eq!(read(&mut sys, 0x1308), 0xA5);
        assert_eq!(read(&mut sys, 0x1708), 0x5A);
    }

    /// The EAROM writes through the 0x0F00 window and reads back at 0x0A00,
    /// through the same control bits the reference driver uses.
    #[test]
    fn earom_write_read() {
        let mut sys = SpaceduelSystem::new();

        // Latch address 0x05 with data 0xAB.
        sys.bus_write(BusMaster::Cpu(0), 0x0F05, 0xAB);

        // Erase address 5: C1=0 (bit 2 set), C2=1 (bit 1 set), CS1=1 (bit 3).
        sys.bus_write(BusMaster::Cpu(0), 0x0E80, 0x0F); // clock high
        sys.bus_write(BusMaster::Cpu(0), 0x0E80, 0x0E); // clock low

        // Write 0xAB: C1=0 (bit 2 set), C2=0 (bit 1 clear), CS1=1 (bit 3).
        sys.bus_write(BusMaster::Cpu(0), 0x0E80, 0x0D); // clock high
        sys.bus_write(BusMaster::Cpu(0), 0x0E80, 0x0C); // clock low

        // Read: C1=1 (bit 2 clear), CS1=1 (bit 3 set); the falling edge
        // loads the data register.
        sys.bus_write(BusMaster::Cpu(0), 0x0E80, 0x09); // clock high
        sys.bus_write(BusMaster::Cpu(0), 0x0E80, 0x08); // falling edge

        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x0A00), 0xAB);
    }

    /// Save/load carries RAM, inputs, IRQ state and the EAROM; ROM is not
    /// carried.
    #[test]
    fn save_load_round_trip() {
        let mut sys = SpaceduelSystem::new();
        sys.board.map.region_data_mut(SpaceduelRegion::Ram)[0x100] = 0xAA;
        sys.board.map.region_data_mut(SpaceduelRegion::VectorRam)[0x200] = 0xBB;
        sys.board.in3 = 0x2A;
        sys.board.clock = 75_000;
        sys.board.irq_counter = 3000;
        sys.board.irq_pending = true;
        sys.board.earom.load_from(&{
            let mut d = [0u8; 64];
            d[0] = 0x42;
            d[63] = 0xEF;
            d
        });
        let data = sys.save_state().expect("save_state should return Some");
        let cpu_snap = sys.cpu.snapshot();

        let mut sys2 = SpaceduelSystem::new();
        sys2.load_state(&data).unwrap();
        assert_eq!(sys2.cpu.snapshot(), cpu_snap);
        assert_eq!(
            sys2.board.map.region_data(SpaceduelRegion::Ram)[0x100],
            0xAA
        );
        assert_eq!(
            sys2.board.map.region_data(SpaceduelRegion::VectorRam)[0x200],
            0xBB
        );
        assert_eq!(sys2.board.in3, 0x2A);
        assert_eq!(sys2.board.clock, 75_000);
        assert_eq!(sys2.board.irq_counter, 3000);
        assert!(sys2.board.irq_pending);
        assert_eq!(sys2.board.earom.read(0), 0x42);
        assert_eq!(sys2.board.earom.read(63), 0xEF);
    }

    /// A vector drawn up the display list lands up the screen: the
    /// end-to-end Y-sign statement, as on Tempest.
    #[test]
    fn a_vector_drawn_up_the_display_list_lands_up_the_screen() {
        use phosphor_core::core::machine::Renderable;
        let (fw, fh) = (TIMING.display_width as f32, TIMING.display_height as f32);
        let mut sys = SpaceduelSystem::new();
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
}
