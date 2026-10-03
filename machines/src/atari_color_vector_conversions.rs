//! Atari's color vector conversion class: Space Duel (1982), Gravitar (1982)
//! and Black Widow (1983).
//!
//! The three are one hardware family, each built on its predecessor, and
//! Black Widow was sold partly as a conversion kit for Gravitar boards
//! (`Gravitar/Black Widow Retrofit`, TM-232). They share the 6502, the base
//! AVG, two POKEYs reading switch banks through ALLPOT, the ER2055 behind its
//! K2 control latch, and one audio topology with different part values.
//!
//! There are two PCBs. Gravitar and Black Widow share one; Space Duel's
//! decodes the same parts at different addresses ([`Decode`]). Each game's
//! file supplies its controls, switch tables, ROMs, display field and audio
//! part values. The memory map and notes below are the Gravitar/Black Widow
//! PCB's; Space Duel's are in `spaceduel.rs`.
//!
//! # Schematics
//!
//! | Drawing | Source | Sheets |
//! |---|---|---|
//! | `Space Duel` PCB schematic, SP-181, 2nd printing | `arcarc.xmission.com/PDF_Arcade_Atari_Kee/Space_Duel/Space_Duel_SP-181_2nd_Printing.pdf` | 4A clock and watchdog, 4B decoder and IRQ, 5B I/O and audio, CAT-box memory map |
//! | `Black Widow PCB Schematic Diagram`, SP-234, 2nd printing | `arcarc.xmission.com/PDF_Arcade_Atari_Kee/Black_Widow/Black_Widow_SP-234_2nd_Printing.pdf` | 3A memory map, 4A/4B signal names, 5B clock and watchdog, 6A decoder and IRQ, 6B memories, 7A I/O and audio |
//! | `Gravitar PCB Schematic Diagram`, SP-206, 2nd printing | `arcarc.xmission.com/PDF_Arcade_Atari_Kee/Gravitar/Gravitar_SP-206_2nd_Printing.pdf` | 5A I/O and audio, 11A memory map |
//!
//! Everything this file models is transcribed, with the places the sheets
//! disagree, in
//! [`docs/schematics/bwidow-gravitar-board.md`](../../docs/schematics/bwidow-gravitar-board.md)
//! and, for Space Duel's PCB,
//! [`docs/schematics/space-duel-audio-output.md`](../../docs/schematics/space-duel-audio-output.md).
//!
//! # The board
//!
//! - **CPU**, a 6502 at 12.096 MHz / 8 = 1.512 MHz, with 2K of program RAM,
//!   24K of program ROM, the vector generator's 2K of RAM and 14K of ROM, two
//!   POKEYs, an ER2055 EAROM and three input buffers. One CPU does
//!   everything; there is no sound CPU.
//! - **The AVG**, the same base generator as Space Duel's
//!   ([`AvgVariant::SpaceDuel`]): 3-bit color in the STAT, no color RAM.
//! - **Decode** by a pair of 32 x 8 PROMs on A15-A11 (`136010-112` at R2 for
//!   the RAM, I/O and inputs, `136010-111` at R1 for the ROMs) and an LS138 at
//!   P3 for the write strobes. [`decode`] is R2's table.
//! - **IRQ** from J4, an LS161 counting the 3 kHz clock from zero after each
//!   acknowledge and holding `/IRQ` low from the twelfth edge until the next.
//! - **Watchdog** from H4, an LS393 pair on the same clock: 128 periods
//!   without a write to `8980` resets the board.
//! - **The output latch** R9 carries the coin counters, the start lamps, a RAM
//!   bank select and two picture inverts, and clears on reset.
//!
//! Memory map:
//!
//! ```text
//!   $0000-$07FF  RAM (2 KB; A10 exclusive-ORed with BANK SEL)
//!   $2000-$27FF  Vector RAM (2 KB)
//!   $2800-$5FFF  Vector ROM (14 KB)
//!   $6000-$67FF  POKEY C/D3 (ALLPOT reads switch bank D4)
//!   $6800-$6FFF  POKEY B3 (ALLPOT reads switch bank B4)
//!   $7000-$77FF  EAROM read
//!   $7800-$7FFF  IN0: coin door, AVG halt, 3 kHz clock
//!   $8000-$87FF  IN1: player 1, option jumpers
//!   $8800-$8FFF  IN2: player 2 or the fire stick, starts, cabinet (read)
//!   $8800        output latch R9 (write; strobes repeat every $200)
//!   $8840        AVG GO
//!   $8880        AVG reset
//!   $88C0        IRQ acknowledge
//!   $8900        EAROM control
//!   $8940-$897F  EAROM write
//!   $8980        watchdog clear
//!   $9000-$EFFF  Program ROM (24 KB, the $E000 page repeated at $F000)
//! ```
//!
//! What is not modeled: the coin counters, coin lockout and start lamps on
//! the latch; the watchdog's reset pulse length (the board is reset at once
//! rather than held for another 128 periods); and the cocktail cabinet, whose
//! jumper stays open.

use crate::rom_loader::RomRegion;
use phosphor_core::audio::{DcBlocker, SampleRing};
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind, DebugTraceBuffer};
use phosphor_core::core::display::display_settings;
use phosphor_core::core::machine::TimingConfig;
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
// Timing and clocks
// ---------------------------------------------------------------------------

/// The 12.096 MHz crystal Y1. The AVG runs from it directly, the 6502 and both
/// POKEYs through E4's divide-by-eight.
const MASTER_CLOCK_HZ: u32 = 12_096_000;

/// A game's timing: one 1/60 s frame of 6502 cycles, with the vector field
/// the game draws into. The board has no raster timing, so the "scanline" is
/// the whole frame, as on every AVG board here.
pub(crate) const fn timing(display_width: u32, display_height: u32) -> TimingConfig {
    TimingConfig {
        cpu_clock_hz: 1_512_000,
        cycles_per_scanline: 25_200,
        total_scanlines: 1,
        display_width,
        display_height,
        display_aspect: Some((4, 3)),
    }
}

/// The 3 kHz clock is F4's last stage, master / 4096, which is 512 CPU
/// cycles: it is high while bit 8 of the cycle count is set.
const CLOCK_3KHZ_BIT: u64 = 0x100;
const CLOCK_3KHZ_MASK: u64 = 0x1FF;

/// J4 asserts `/IRQ` when QC and QD are both high: a count of twelve.
const IRQ_COUNT: u8 = 12;

/// H4's second half reaches QD after 8 of the first half's 16-count cycles.
const WATCHDOG_COUNT: u8 = 128;

/// AVG master-clock cycles per CPU cycle.
const AVG_CYCLES_PER_CPU_CYCLE: u32 = 8;

// ---------------------------------------------------------------------------
// Address map
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum ConversionRegion {
    Ram = 1,
    VectorRam = 2,
    VectorRom = 3,
    Io = 4,
    ProgramRom = 5,
}

/// The two PCBs in the class decode the same parts at different addresses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Decode {
    /// SP-181: LS42s and an LS139. 1K of RAM, the I/O page at $0800, the
    /// POKEYs at $1000 and $1400, the input muxes at $0900-$0907, 6K of
    /// vector ROM and 20K of program ROM from $4000.
    SpaceDuel,
    /// SP-206 and SP-234: R2's and R1's PROMs ([`decode`]). 2K of banked RAM,
    /// the POKEYs at $6000 and $6800, the input buffers at $7800-$8FFF, 14K
    /// of vector ROM and 24K of program ROM from $9000.
    GravitarBlackWidow,
}

fn build_map(decode: Decode) -> AddressSpace16 {
    // (RAM size, I/O base and size, vector ROM size, program ROM base and size)
    let (ram, io, io_len, vrom, prom, prom_len) = match decode {
        Decode::SpaceDuel => (0x0400, 0x0800, 0x1000, 0x1800, 0x4000, 0x5000),
        Decode::GravitarBlackWidow => (0x0800, 0x6000, 0x3000, 0x3800, 0x9000, 0x6000),
    };
    let mut map = AddressSpace16::new();
    map.region(
        ConversionRegion::Ram,
        "RAM",
        0x0000,
        ram,
        AccessKind::ReadWrite,
    )
    .region(
        ConversionRegion::VectorRam,
        "Vector RAM",
        0x2000,
        0x0800,
        AccessKind::ReadWrite,
    )
    .region(
        ConversionRegion::VectorRom,
        "Vector ROM",
        0x2800,
        vrom,
        AccessKind::ReadOnly,
    )
    .region(ConversionRegion::Io, "I/O", io, io_len, AccessKind::Io)
    .region(
        ConversionRegion::ProgramRom,
        "Program ROM",
        prom,
        prom_len,
        AccessKind::ReadOnly,
    );
    match decode {
        // The $8000 page repeats through $9000-$FFFF for the reset vectors.
        Decode::SpaceDuel => {
            for page in 0..7u16 {
                map.mirror(0x9000 + page * 0x1000, 0x8000, 0x1000);
            }
        }
        // R1 selects ROM 5 for both $E000 and $F000, so the reset vectors
        // read the $E000 page.
        Decode::GravitarBlackWidow => {
            map.mirror(0xF000, 0xE000, 0x1000);
        }
    }
    map
}

/// The selects R2 drives for one 2K block. Each is one active-low output of
/// the PROM; [`decode`] returns the one that is low (the `/I/OS` buffer
/// steering output aside).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Select {
    /// B0, program RAM N/P1.
    Ram,
    /// B1, POKEY C/D3.
    Io0,
    /// B2, POKEY B3.
    Io1,
    /// B4, the EAROM's data buffer.
    EaromRead,
    /// B5, buffer M9.
    Sinp1,
    /// B6, buffer L9.
    Sinp2,
    /// B7, buffer N9 on a read and the LS138 at P3 on a write.
    Io,
    /// Nothing R2 drives: vector memory, ROM (R1's), or open bus.
    None,
}

/// R2's decode of a CPU address on the Gravitar/Black Widow PCB, from
/// A15-A11. The PROM's contents are in the `decode_matches_the_r2_prom` test.
pub(crate) fn decode(addr: u16) -> Select {
    match addr >> 11 {
        0x00 => Select::Ram,
        0x0C => Select::Io0,
        0x0D => Select::Io1,
        0x0E => Select::EaromRead,
        0x0F => Select::Sinp1,
        0x10 => Select::Sinp2,
        0x11 => Select::Io,
        _ => Select::None,
    }
}

// ---------------------------------------------------------------------------
// ROM loading
// ---------------------------------------------------------------------------

/// One revision's ROMs: the vector ROM region from CPU $2800 and the program
/// ROM region, each sized for its PCB's map.
pub struct ConversionRomConfig {
    pub(crate) vector: &'static RomRegion,
    pub(crate) program: &'static RomRegion,
}

/// The AVG state PROM at N4, the same part on every AVG board here.
pub(crate) static AVG_PROM: RomRegion = RomRegion {
    size: 0x100,
    entries: &[rom!("136002-125.n4", 0x100, 0, 0x5903af03)],
};

// ---------------------------------------------------------------------------
// Audio
// ---------------------------------------------------------------------------

/// The parts that differ between the two games' audio chains. Everything
/// else on the sheet is common; see the schematic transcription's table.
pub(crate) struct AudioParts {
    /// C27, across the gain-of-10 stage's 1M feedback resistor R43.
    pub c27_farads: f64,
    /// R46, B3's leg into the summing amplifier.
    pub r46_ohms: f64,
    /// C34, across C/D3's 1k first-stage feedback resistor R51, where fitted.
    pub c34_farads: Option<f64>,
}

/// Each POKEY's AUD pin goes straight to an LM324 inverting input held at
/// +5 V by R50: a zero-ohm virtual ground.
const POKEY_LOAD: PokeyLoad = PokeyLoad::VirtualGround {
    series_ohms: 0.0,
    reference_v: 5.0,
};

/// R52, the summing amplifier's feedback, and R49, C/D3's leg into it.
const R52_OHMS: f64 = 3_900.0;
const R49_OHMS: f64 = 3_900.0;
/// R43 over R45: the B3 chain's second stage.
const B3_GAIN: f32 = 10.0;

/// A one-pole low-pass as the input less a one-pole high-pass at the same
/// corner, which is the low-pass exactly.
fn low_pass(stage: &mut DcBlocker, x: f32) -> f32 {
    x - stage.process(x)
}

fn corner(ohms: f64, farads: f64) -> f32 {
    (1.0 / (std::f64::consts::TAU * ohms * farads)) as f32
}

// ---------------------------------------------------------------------------
// The board
// ---------------------------------------------------------------------------

/// Everything the 6502 talks to. The CPU lives on the game's machine so a
/// cycle can dispatch at a concrete type.
#[derive(BusDebug, DebugTrace, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct AtariColorVectorConversionsBoard {
    #[debug_map(cpu = 0)]
    #[save(id = 1)]
    pub(crate) map: AddressSpace16,
    #[debug_device("AVG")]
    #[save(id = 2)]
    avg: Avg,
    /// What the tube shows: the list the generator finished since the last
    /// GO. Redrawn after a load rather than restored.
    #[save_skip(default)]
    pub(crate) display_list: Vec<VectorLine>,

    /// CPU cycles since power-on; bit 8 is the 3 kHz clock.
    #[save(id = 3)]
    pub(crate) clock: u64,
    /// J4's count, 0 to 12. `/IRQ` is low at 12.
    #[save(id = 4)]
    pub(crate) irq_count: u8,
    /// H4's count of 3 kHz periods since the last `WDCLR`.
    #[save(id = 5)]
    pub(crate) watchdog_count: u8,
    /// R9, the output latch.
    #[save(id = 6)]
    pub(crate) latch: u8,

    /// M9's switch inputs as the buffer sees them, active low: COIN R, COIN L,
    /// COIN AUX, SLAM and SELF-TEST on bits 0-4. Bit 5 is `/SA`, a test point
    /// that reads high, on Gravitar and Black Widow, and the DIAG STEP switch
    /// on Space Duel. Bits 6-7 are generated.
    #[save(id = 7)]
    pub(crate) in0: u8,
    /// L9's control pins D0-D4, active low; D5-D7 are the option jumpers. A
    /// buffer at $8000 on Gravitar and Black Widow, an LS251 mux read through
    /// its inverting W output at $0900-$0907 on Space Duel.
    #[save(id = 8)]
    pub(crate) l9: u8,
    /// N9's control pins D0-D6, active low; D7 is the cabinet. A buffer at
    /// $8800, or the second LS251 beside L9.
    #[save(id = 9)]
    pub(crate) n9: u8,
    /// Switch bank D4, read through C/D3's ALLPOT; on = 1. Board
    /// configuration, so not saved.
    #[save_skip]
    pub(crate) dsw_d4: u8,
    /// Switch bank B4, read through B3's ALLPOT.
    #[save_skip]
    pub(crate) dsw_b4: u8,
    /// P10/11 switches 2-4 as `OPTION 0-2`, 1 = open. No manual assigns them.
    #[save_skip]
    pub(crate) options: u8,
    /// The cabinet line on N9's D7: 0x80 with the harness jumper absent.
    #[save_skip]
    pub(crate) cabinet: u8,
    /// Which PCB's decode the board answers with. Fixed by the game.
    #[save_skip]
    decode: Decode,

    /// POKEY C/D3 at $6000 and B3 at $6800, both at 1.512 MHz.
    #[save(id = 10)]
    pokey_cd3: Pokey,
    #[save(id = 11)]
    pokey_b3: Pokey,
    /// ER2055 EAROM at M2.
    #[save(id = 12)]
    pub(crate) earom: Er2055,

    // Audio: see `mix_audio`.
    #[save(id = 13)]
    cd3_first_stage: DcBlocker,
    #[save(id = 14)]
    cd3_coupling: DcBlocker,
    #[save(id = 15)]
    b3_coupling: DcBlocker,
    #[save(id = 16)]
    b3_gain_stage: DcBlocker,
    #[save(id = 17)]
    b3_unity_stage: DcBlocker,
    /// Whether C34 is fitted, B3's weight at the mixer, and the headroom
    /// scale: fixed by the game's parts.
    #[save_skip]
    cd3_has_pole: bool,
    #[save_skip]
    b3_weight: f32,
    #[save_skip]
    mix_scale: f32,
    /// The vector field the game draws into.
    #[save_skip]
    field: (u32, u32),

    #[debug_events]
    #[save_skip]
    debug_trace: DebugTraceBuffer,
}

impl AtariColorVectorConversionsBoard {
    pub(crate) fn new(decode: Decode, timing: &TimingConfig, audio: &AudioParts) -> Self {
        let rate = phosphor_core::audio::host_sample_rate();
        let mut pokey_cd3 = Pokey::with_clock(MASTER_CLOCK_HZ / 8, rate);
        pokey_cd3.set_output_load(POKEY_LOAD);
        let mut pokey_b3 = Pokey::with_clock(MASTER_CLOCK_HZ / 8, rate);
        pokey_b3.set_output_load(POKEY_LOAD);
        let b3_weight = (R52_OHMS / audio.r46_ohms) as f32;
        Self {
            map: build_map(decode),
            avg: Avg::with_variant(
                AvgVariant::SpaceDuel,
                timing.display_width as i32,
                timing.display_height as i32,
            ),
            display_list: Vec::with_capacity(2048),
            clock: 0,
            irq_count: 0,
            watchdog_count: 0,
            latch: 0,
            in0: 0x3F,
            l9: 0x1F,
            n9: 0x7F,
            dsw_d4: 0x00,
            dsw_b4: 0x00,
            options: 0x07,
            cabinet: 0x80,
            decode,
            pokey_cd3,
            pokey_b3,
            earom: Er2055::new(),
            // C34 with R51; C33 into R49; C30 into R45 100k; R43 1M with C27;
            // R44 1M with C28 0.001 uF.
            cd3_first_stage: DcBlocker::with_cutoff(
                corner(1_000.0, audio.c34_farads.unwrap_or(0.22e-6)),
                rate,
            ),
            cd3_coupling: DcBlocker::with_cutoff(corner(R49_OHMS, 0.22e-6), rate),
            b3_coupling: DcBlocker::with_cutoff(corner(100_000.0, 0.22e-6), rate),
            b3_gain_stage: DcBlocker::with_cutoff(corner(1_000_000.0, audio.c27_farads), rate),
            b3_unity_stage: DcBlocker::with_cutoff(corner(1_000_000.0, 1e-9), rate),
            cd3_has_pole: audio.c34_farads.is_some(),
            b3_weight,
            mix_scale: 1.0 / (1.0 + B3_GAIN * b3_weight),
            field: (timing.display_width, timing.display_height),
            debug_trace: DebugTraceBuffer::new(),
        }
    }

    /// Load one revision's ROMs and the AVG's state PROM.
    pub(crate) fn load_roms(
        &mut self,
        rom_set: &crate::rom_loader::RomSet,
        config: &ConversionRomConfig,
    ) -> Result<(), crate::rom_loader::RomLoadError> {
        self.map
            .load_region(ConversionRegion::ProgramRom, &config.program.load(rom_set)?);
        self.map
            .load_region(ConversionRegion::VectorRom, &config.vector.load(rom_set)?);
        self.avg.load_state_prom(&AVG_PROM.load(rom_set)?);
        Ok(())
    }

    /// `/IRQ`, from L3: low while J4 holds its count of twelve.
    pub(crate) fn irq_asserted(&self) -> bool {
        self.irq_count >= IRQ_COUNT
    }

    /// The RAM address the chip sees. On the Gravitar/Black Widow PCB, B6
    /// exclusive-ORs A10 with BANK SEL; Space Duel's 1K has no bank.
    fn ram_address(&self, addr: u16) -> u16 {
        match self.decode {
            Decode::GravitarBlackWidow => addr ^ (u16::from(self.latch & 0x04 != 0) << 10),
            Decode::SpaceDuel => addr,
        }
    }

    /// INVERT X and INVERT Y, R9's D6 and D7: each axis is inverted while its
    /// bit is clear. Every label says 1 = invert, but both Black Widow's and
    /// Space Duel's programs write the bits set in an upright cabinet, so the
    /// sense flips in a stage after the latch that was not traced (see the
    /// schematic transcription).
    fn apply_inverts(&mut self) {
        self.avg
            .set_flip(self.latch & 0x40 == 0, self.latch & 0x80 == 0);
    }

    /// IN0 at $7800: the coin door below, AVG halt and the clock above.
    fn in0(&self) -> u8 {
        let mut v = self.in0 & 0x3F;
        v |= u8::from(self.avg.is_halted()) << 6;
        v |= u8::from(self.clock & CLOCK_3KHZ_BIT != 0) << 7;
        v
    }

    /// Clock J4 and H4 off the 3 kHz line: J4 (an LS161) on its rising edge
    /// while `/IRQ` is high, H4 (an LS393) on its falling edge. Returns true
    /// when the watchdog bites.
    fn clock_counters(&mut self) -> bool {
        match self.clock & CLOCK_3KHZ_MASK {
            0x100 => {
                // ET is `/IRQ`, so the count stops at twelve.
                if self.irq_count < IRQ_COUNT {
                    self.irq_count += 1;
                }
                false
            }
            0x000 if self.clock != 0 => {
                self.watchdog_count += 1;
                self.watchdog_count >= WATCHDOG_COUNT
            }
            _ => false,
        }
    }

    /// AVG GO: restart the generator, dropping whatever it drew since the
    /// last frame boundary.
    fn trigger_avg(&mut self, addr: u16) {
        self.trace_write(addr, Some("AVG"), Some("vector generator start"));
        self.avg.go();
        self.avg.take_display_list();
    }

    /// Run the vector generator for one CPU cycle's worth of its own clock.
    fn step_avg(&mut self) {
        let mem = VectorMemory::split(
            self.map.region_data(ConversionRegion::VectorRam),
            self.map.region_data(ConversionRegion::VectorRom),
            0x0800,
        );
        if self.avg.step(AVG_CYCLES_PER_CPU_CYCLE, &mem, &[]) {
            let list = self.avg.take_display_list();
            if self.avg.is_halted() {
                // The list ends in HALT, and that pass is the frame.
                self.display_list = list;
            }
        }
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

    /// The board side of `/RESET`: R9 and the AVG clear (DISRST is RESET
    /// ORed with VGRST), the POKEYs and the EAROM's control latch K2 reset.
    /// J4 is cleared only by INTACK, so it keeps its count.
    fn reset_hardware(&mut self) {
        self.latch = 0;
        self.apply_inverts();
        self.avg.reset();
        self.display_list.clear();
        self.watchdog_count = 0;
        self.pokey_cd3.reset();
        self.pokey_b3.reset();
        self.earom.reset();
    }

    /// Reset the board and the CPU, as the RESET test point does.
    pub(crate) fn reset(&mut self, cpu: &mut M6502) {
        self.reset_hardware();
        self.cd3_first_stage.reset();
        self.cd3_coupling.reset();
        self.b3_coupling.reset();
        self.b3_gain_stage.reset();
        self.b3_unity_stage.reset();
        cpu.reset(self, BusMaster::Cpu(0));
    }

    /// One CPU cycle: the POKEYs, the counters and the AVG, then the 6502.
    #[inline]
    pub(crate) fn tick(&mut self, cpu: &mut M6502) {
        self.pokey_cd3.tick();
        self.pokey_b3.tick();
        if self.clock_counters() {
            // K3 drives `/RESET`; see the module notes on its pulse length.
            self.reset_hardware();
            cpu.reset(self, BusMaster::Cpu(0));
        }
        if self.map.has_any_watchpoints() || self.debug_trace.enabled() {
            let pc = cpu.at_instruction_boundary().then_some(cpu.pc as u32);
            self.map.latch_access_context(self.clock, pc);
        }
        self.step_avg();
        cpu.execute_cycle(self, BusMaster::Cpu(0));
        self.clock += 1;
    }

    /// Drain both POKEYs' currents and carry them to the speaker, as sheet
    /// 7A (Black Widow) and 5A (Gravitar) do. Refdes are in the schematic
    /// transcription.
    ///
    /// Each chip works into a zero-ohm virtual ground, so its first stage's
    /// output is R47 or R51 (1k) times the current its devices sink; one chip
    /// with every device on is 1.0. C/D3 passes C34's pole where fitted and
    /// C33/R49's 185 Hz coupling to the mixer at 1. B3 passes C30/R45's 7.2
    /// Hz coupling, the gain-of-10 stage with C27's pole, and the unity stage
    /// at 159 Hz, to the mixer at R52/R46. B3's chain inverts four times and
    /// C/D3's twice, so they add in phase.
    ///
    /// The mix is scaled so both chains at their in-band peaks still fit,
    /// which leaves C/D3 at `mix_scale` on its own. The two antiphase
    /// outputs, AUD 1 and AUD 2, are the same signal for a mono speaker.
    pub(crate) fn mix_audio(&mut self, out: &mut SampleRing<i16>) {
        let i_cd3 = self.pokey_cd3.drain_audio();
        let i_b3 = self.pokey_b3.drain_audio();
        let full = POKEY_LOAD.full_scale() as f32;
        let len = i_cd3.len().min(i_b3.len());
        for i in 0..len {
            let mut c = i_cd3[i] / full;
            if self.cd3_has_pole {
                c = low_pass(&mut self.cd3_first_stage, c);
            }
            let c = self.cd3_coupling.process(c);
            let b = self.b3_coupling.process(i_b3[i] / full);
            let b = B3_GAIN * low_pass(&mut self.b3_gain_stage, b);
            let b = low_pass(&mut self.b3_unity_stage, b);
            let mixed = self.mix_scale * (c + self.b3_weight * b);
            out.push((mixed * 32767.0).clamp(i16::MIN as f32, i16::MAX as f32) as i16);
        }
    }

    pub(crate) fn render(&self, buffer: &mut [u8]) {
        let (rw, rh) = raster_size_for_field(self.field.0, self.field.1);
        crate::atari_dvg::rasterize_vectors(
            &self.display_list,
            buffer,
            rw,
            rh,
            self.field,
            true,
            &display_settings().without_halation(),
        );
    }
}

// ---------------------------------------------------------------------------
// The CPU's bus
// ---------------------------------------------------------------------------

/// The write strobes both PCBs have, at different addresses.
#[derive(Clone, Copy)]
enum Strobe {
    Latch,
    AvgGo,
    AvgReset,
    IntAck,
    EaromControl,
    EaromWrite,
    WatchdogClear,
}

impl AtariColorVectorConversionsBoard {
    /// L9's pins, with the option jumpers on D5-D7.
    fn l9_pins(&self) -> u8 {
        (self.options << 5) | (self.l9 & 0x1F)
    }

    /// N9's pins, with the cabinet line on D7.
    fn n9_pins(&self) -> u8 {
        self.cabinet | (self.n9 & 0x7F)
    }

    /// POKEY C/D3; its ALLPOT is wired to switch bank D4.
    fn read_cd3(&mut self, addr: u16) -> u8 {
        match addr & 0x0F {
            0x08 => self.dsw_d4,
            reg => self.pokey_cd3.read(reg),
        }
    }

    /// POKEY B3; its ALLPOT is wired to switch bank B4.
    fn read_b3(&mut self, addr: u16) -> u8 {
        match addr & 0x0F {
            0x08 => self.dsw_b4,
            reg => self.pokey_b3.read(reg),
        }
    }

    /// The memories, by region, where neither decode has a device.
    fn read_memory(&mut self, addr: u16) -> u8 {
        match self.map.page(addr).region_id {
            ConversionRegion::RAM => self.map.read_backing(self.ram_address(addr)),
            ConversionRegion::VECTOR_RAM
            | ConversionRegion::VECTOR_ROM
            | ConversionRegion::PROGRAM_ROM => self.map.read_backing(addr),
            _ => 0,
        }
    }

    fn write_memory(&mut self, addr: u16, data: u8) {
        match self.map.page(addr).region_id {
            ConversionRegion::RAM => {
                let a = self.ram_address(addr);
                self.map.write_backing(a, data);
            }
            ConversionRegion::VECTOR_RAM => self.map.write_backing(addr, data),
            _ => {}
        }
    }

    fn strobe(&mut self, strobe: Strobe, addr: u16, data: u8) {
        match strobe {
            Strobe::Latch => {
                self.trace_write(addr, None, Some("output latch"));
                self.latch = data;
                self.apply_inverts();
            }
            Strobe::AvgGo => self.trigger_avg(addr),
            Strobe::AvgReset => {
                self.trace_write(addr, Some("AVG"), Some("vector generator reset"));
                self.avg.reset();
            }
            Strobe::IntAck => self.irq_count = 0,
            // K2 latches DB3-DB0: CK = DB0, C2 = DB1, C1 = /DB2, CS1 = DB3.
            Strobe::EaromControl => self.earom.write_control(
                data & 0x01 != 0,
                data & 0x08 != 0,
                data & 0x04 == 0,
                data & 0x02 != 0,
            ),
            // The address latch takes AB5-AB0 and the data latch the byte.
            Strobe::EaromWrite => self.earom.latch(addr & 0x3F, data),
            Strobe::WatchdogClear => self.watchdog_count = 0,
        }
    }

    /// The Gravitar/Black Widow PCB: R2 selects the block.
    fn read_gravitar_black_widow(&mut self, addr: u16) -> u8 {
        match decode(addr) {
            Select::Io0 => self.read_cd3(addr),
            Select::Io1 => self.read_b3(addr),
            Select::EaromRead => self.earom.read_latched(),
            Select::Sinp1 => self.in0(),
            Select::Sinp2 => self.l9_pins(),
            Select::Io => self.n9_pins(),
            Select::Ram | Select::None => self.read_memory(addr),
        }
    }

    fn write_gravitar_black_widow(&mut self, addr: u16, data: u8) {
        match decode(addr) {
            Select::Io0 => self.pokey_cd3.write(addr & 0x0F, data),
            Select::Io1 => self.pokey_b3.write(addr & 0x0F, data),
            // P3, an LS138 on A8-A6; A9 and A10 are not decoded.
            Select::Io => {
                let strobe = match (addr >> 6) & 7 {
                    0 => Strobe::Latch,
                    1 => Strobe::AvgGo,
                    2 => Strobe::AvgReset,
                    3 => Strobe::IntAck,
                    4 => Strobe::EaromControl,
                    5 => Strobe::EaromWrite,
                    6 => Strobe::WatchdogClear,
                    _ => return,
                };
                self.strobe(strobe, addr, data);
            }
            Select::Ram | Select::None => self.write_memory(addr, data),
            Select::EaromRead | Select::Sinp1 | Select::Sinp2 => {}
        }
    }

    /// Space Duel's PCB, at the addresses its CAT-box memory map lists.
    fn read_space_duel(&mut self, addr: u16) -> u8 {
        match addr {
            0x0800 => self.in0(),
            // N9 and L9 are LS251s on AB2-AB0; their inverting W outputs
            // drive DB7 and DB6, so a grounded (pressed) pin reads 1.
            0x0900..=0x0907 => {
                let k = addr & 7;
                let n9 = (!self.n9_pins() >> k) & 1;
                let l9 = (!self.l9_pins() >> k) & 1;
                (n9 << 7) | (l9 << 6)
            }
            0x0A00 => self.earom.read_latched(),
            0x1000..=0x13FF => self.read_cd3(addr),
            0x1400..=0x17FF => self.read_b3(addr),
            _ => self.read_memory(addr),
        }
    }

    fn write_space_duel(&mut self, addr: u16, data: u8) {
        let strobe = match addr {
            0x0C00 => Strobe::Latch,
            0x0C80 => Strobe::AvgGo,
            0x0D00 => Strobe::WatchdogClear,
            0x0D80 => Strobe::AvgReset,
            0x0E00 => Strobe::IntAck,
            0x0E80 => Strobe::EaromControl,
            0x0F00..=0x0F3F => Strobe::EaromWrite,
            0x1000..=0x13FF => return self.pokey_cd3.write(addr & 0x0F, data),
            0x1400..=0x17FF => return self.pokey_b3.write(addr & 0x0F, data),
            _ => return self.write_memory(addr, data),
        };
        self.strobe(strobe, addr, data);
    }
}

impl Bus for AtariColorVectorConversionsBoard {
    type Address = u16;
    type Data = u8;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let data = match self.decode {
            Decode::GravitarBlackWidow => self.read_gravitar_black_widow(addr),
            Decode::SpaceDuel => self.read_space_duel(addr),
        };
        self.map.watch_read(0, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.map.watch_write(0, master, addr, data);
        match self.decode {
            Decode::GravitarBlackWidow => self.write_gravitar_black_widow(addr, data),
            Decode::SpaceDuel => self.write_space_duel(addr, data),
        }
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            irq: self.irq_asserted(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TIMING: TimingConfig = timing(480, 440);
    const PARTS: AudioParts = AudioParts {
        c27_farads: 100e-12,
        r46_ohms: 22_000.0,
        c34_farads: None,
    };

    fn board() -> AtariColorVectorConversionsBoard {
        AtariColorVectorConversionsBoard::new(Decode::GravitarBlackWidow, &TIMING, &PARTS)
    }

    fn read(b: &mut AtariColorVectorConversionsBoard, addr: u16) -> u8 {
        b.read(BusMaster::Cpu(0), addr)
    }

    fn write(b: &mut AtariColorVectorConversionsBoard, addr: u16, data: u8) {
        b.write(BusMaster::Cpu(0), addr, data)
    }

    /// R2's contents, `136010-112`, dumped from the Gravitar set: one byte
    /// per 2K block, each output active low.
    const R2_PROM: [u8; 32] = [
        0xF6, 0xF7, 0xF7, 0xF7, 0xF7, 0xF7, 0xF7, 0xF7, 0xF7, 0xF7, 0xF7, 0xF7, 0xFD, 0xFB, 0xE7,
        0xD7, 0xB7, 0x77, 0xF7, 0xF7, 0xF7, 0xF7, 0xF7, 0xF7, 0xF7, 0xF7, 0xF7, 0xF7, 0xF7, 0xF7,
        0xF7, 0xF7,
    ];

    /// `decode` is R2: for every block, the select it returns is the one
    /// device output the PROM drives low (B3, `/I/OS`, steers the data buffer
    /// and is not a device).
    #[test]
    fn decode_matches_the_r2_prom() {
        let outputs = [
            (0, Select::Ram),
            (1, Select::Io0),
            (2, Select::Io1),
            (4, Select::EaromRead),
            (5, Select::Sinp1),
            (6, Select::Sinp2),
            (7, Select::Io),
        ];
        for (block, &byte) in R2_PROM.iter().enumerate() {
            let low: Vec<Select> = outputs
                .iter()
                .filter(|(bit, _)| byte & (1 << bit) == 0)
                .map(|&(_, s)| s)
                .collect();
            let expected = low.first().copied().unwrap_or(Select::None);
            assert!(low.len() <= 1, "block {block:#x} selects {low:?}");
            assert_eq!(decode((block as u16) << 11), expected, "block {block:#x}");
        }
    }

    /// J4 asserts IRQ on the twelfth rising edge of the 3 kHz clock after an
    /// acknowledge, and holds it until the next.
    #[test]
    fn irq_asserts_twelve_clocks_after_the_acknowledge_and_holds() {
        let mut b = board();
        // Twelve rising edges are at cycles 0x100, 0x300, ... 0x1700.
        while b.clock <= 0x1700 {
            assert!(!b.irq_asserted(), "early at cycle {:#x}", b.clock);
            b.clock_counters();
            b.clock += 1;
        }
        assert!(b.irq_asserted(), "asserted on the twelfth edge");
        // Held: further edges change nothing.
        for _ in 0..0x2000 {
            b.clock_counters();
            b.clock += 1;
        }
        assert!(b.irq_asserted(), "held until acknowledged");
        assert_eq!(b.irq_count, IRQ_COUNT, "ET stops the count");
        write(&mut b, 0x88C0, 0);
        assert!(!b.irq_asserted(), "INTACK clears J4");
    }

    /// The next interrupt is timed from the acknowledge, not from the last
    /// interrupt: a late acknowledge delays it.
    #[test]
    fn a_late_acknowledge_delays_the_next_interrupt() {
        let mut b = board();
        b.irq_count = IRQ_COUNT;
        b.clock = 0x10000 + 0x180; // past a rising edge
        write(&mut b, 0x88C0, 0);
        let start = b.clock;
        while !b.irq_asserted() {
            b.clock_counters();
            b.clock += 1;
        }
        // The next edge is 0x180 on, then every 0x200: the twelfth is
        // 0x180 + 11 * 0x200, and the loop exits a cycle after it.
        assert_eq!(b.clock - start, 0x180 + 11 * 0x200 + 1);
    }

    /// The watchdog bites on the 128th falling edge of the 3 kHz clock
    /// without a WDCLR, and a write to $8980 restarts it.
    #[test]
    fn watchdog_bites_after_128_periods_and_wdclr_restarts_it() {
        let mut b = board();
        b.clock = 1;
        let mut bites = 0;
        let mut cycles = 0u64;
        while bites == 0 {
            if b.clock_counters() {
                bites += 1;
            }
            b.clock += 1;
            cycles += 1;
        }
        assert_eq!(cycles, 128 * 512, "bites on the 128th period");

        b.watchdog_count = 100;
        write(&mut b, 0x8980, 0);
        assert_eq!(b.watchdog_count, 0, "WDCLR clears H4");
        // A9 and A10 are not decoded: $8B80 is the same strobe.
        b.watchdog_count = 100;
        write(&mut b, 0x8B80, 0);
        assert_eq!(b.watchdog_count, 0);
    }

    /// BANK SEL, R9's D2, swaps the two 1K halves of program RAM.
    #[test]
    fn bank_select_swaps_the_ram_halves() {
        let mut b = board();
        write(&mut b, 0x0010, 0x11);
        write(&mut b, 0x0410, 0x44);
        write(&mut b, 0x8800, 0x04);
        assert_eq!(read(&mut b, 0x0010), 0x44);
        assert_eq!(read(&mut b, 0x0410), 0x11);
        write(&mut b, 0x0010, 0x55);
        write(&mut b, 0x8800, 0x00);
        assert_eq!(read(&mut b, 0x0410), 0x55, "written through the swap");
    }

    /// RESET clears R9, so the bank select comes up clear and both axes
    /// inverted until the program's first latch write.
    #[test]
    fn reset_clears_the_output_latch() {
        let mut b = board();
        let mut cpu = M6502::new();
        write(&mut b, 0x8800, 0xFF);
        assert_eq!(b.latch, 0xFF);
        b.reset(&mut cpu);
        assert_eq!(b.latch, 0x00);
    }

    /// The inputs: $7800 carries the coin door with `/SA` high, and the
    /// generated clock and halt bits; $8000 the jumpers above the controls;
    /// $8800 the cabinet above them. Each repeats through its 2K block.
    #[test]
    fn input_buffers_read_their_ports_through_their_blocks() {
        let mut b = board();
        b.clock = 0x100;
        let in0 = read(&mut b, 0x7800);
        assert_eq!(in0 & 0x3F, 0x3F, "switches open and /SA high");
        assert_eq!(in0 & 0x80, 0x80, "3 kHz clock high");
        assert_eq!(read(&mut b, 0x7FFF), in0);

        b.l9 = 0x1E;
        assert_eq!(read(&mut b, 0x8000), 0xFE, "options open over the controls");
        assert_eq!(read(&mut b, 0x87FF), 0xFE);
        b.n9 = 0x5F;
        assert_eq!(read(&mut b, 0x8800), 0xDF, "cabinet open over the controls");
        assert_eq!(read(&mut b, 0x8FFF), 0xDF);
    }

    /// Both POKEYs' ALLPOT read their switch bank: D4 at $6000, B4 at $6800.
    #[test]
    fn allpot_reads_d4_and_b4() {
        let mut b = board();
        b.dsw_d4 = 0xA5;
        b.dsw_b4 = 0x5A;
        assert_eq!(read(&mut b, 0x6008), 0xA5);
        assert_eq!(read(&mut b, 0x6808), 0x5A);
        assert_eq!(read(&mut b, 0x67F8), 0xA5, "A3-A0 only");
        assert_eq!(read(&mut b, 0x6FF8), 0x5A);
    }

    /// The EAROM writes through $8940 and reads back at $7000, through K2's
    /// control bits.
    #[test]
    fn earom_write_read() {
        let mut b = board();
        write(&mut b, 0x8945, 0xAB);
        write(&mut b, 0x8900, 0x0F); // erase: C1=0, C2=1, CS1=1, clock high
        write(&mut b, 0x8900, 0x0E);
        write(&mut b, 0x8900, 0x0D); // write: C1=0, C2=0
        write(&mut b, 0x8900, 0x0C);
        write(&mut b, 0x8900, 0x09); // read: C1=1
        write(&mut b, 0x8900, 0x08);
        assert_eq!(read(&mut b, 0x7000), 0xAB);
    }

    /// $0800-$1FFF selects nothing, and the $E000 page repeats at $F000.
    #[test]
    fn open_blocks_and_the_rom_mirror() {
        let mut b = board();
        write(&mut b, 0x0800, 0x12);
        assert_eq!(read(&mut b, 0x0800), 0x00);
        assert_eq!(read(&mut b, 0x0000), 0x00, "not aliased into RAM");
        b.map.region_data_mut(ConversionRegion::ProgramRom)[0x5FFC] = 0x34;
        assert_eq!(read(&mut b, 0xEFFC), 0x34);
        assert_eq!(read(&mut b, 0xFFFC), 0x34);
    }

    /// The headroom scale covers both chains at their in-band peaks, and
    /// B3's weight is R52 over R46: 0.177 on Black Widow, 0.39 on Gravitar.
    #[test]
    fn mixer_weights_follow_r46() {
        let bw = board();
        assert!((bw.b3_weight - 0.177).abs() < 0.001);
        assert!((bw.mix_scale * (1.0 + 10.0 * bw.b3_weight) - 1.0).abs() < 1e-6);
        let gv = AtariColorVectorConversionsBoard::new(
            Decode::GravitarBlackWidow,
            &TIMING,
            &AudioParts {
                c27_farads: 1e-9,
                r46_ohms: 10_000.0,
                c34_farads: Some(0.22e-6),
            },
        );
        assert!((gv.b3_weight - 0.39).abs() < 0.001);
        assert!(gv.cd3_has_pole);
    }
}
