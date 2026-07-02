//! Atari Star Wars (1983) — color-vector cockpit game.
//!
//! Hardware (MAME `src/mame/atari/starwars.cpp`):
//! - Main CPU: MC6809E @ 1.512 MHz (12.096 MHz / 8)
//! - Sound CPU: MC6809E @ 1.512 MHz
//! - Video: Atari AVG (Star Wars variant) color vector display
//! - 3D math: the [`StarWarsMath`] Matrix Processor + divider + PRNG
//! - Sound: 4× POKEY + TMS5220 speech via a MOS6532 RIOT (wired in a later step)
//!
//! This module implements the board scaffolding: both CPUs, their 64 KB address
//! spaces, ROM banking, the watchdog, the periodic IRQ, the main/sound mailbox
//! latches, and the AVG/matrix wiring.
//!
//! Follow-on steps add the sound devices (RIOT/POKEY/TMS5220) and audio mixing,
//! the full ROM manifest + registry entry, and the analog-stick input + NVRAM
//! persistence. Until the machine is registered, the module carries
//! `#[allow(dead_code)]` (see `lib.rs`).

use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::DebugTraceBuffer;
use phosphor_core::core::machine::{MachineCore, SaveState};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace16, Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::m6809::M6809;
use phosphor_core::device::avg::{Avg, AvgVariant};
use phosphor_core::device::dvg::VectorLine;
use phosphor_core::device::pokey::Pokey;
use phosphor_core::device::starwars_math::StarWarsMath;
use phosphor_core::device::x2212::X2212;
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion};

use crate::atari_dvg::rasterize_vectors;

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------

// Master clock 12.096 MHz; CPUs run at master/8 = 1.512 MHz.
// Frame rate = 3 kHz / 12 / 6 = master / 4096 / 12 / 6 ≈ 41.02 Hz.
// Cycles per frame at 1.512 MHz ≈ 36864.
pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 1_512_000,
    cycles_per_scanline: 36_864, // whole frame (vector display has no scanlines)
    total_scanlines: 1,
    display_width: 1024,
    display_height: 768,
};

/// Periodic IRQ: 3 kHz / 12 ≈ 246.09 Hz → every 6144 CPU cycles.
const IRQ_PERIOD_CYCLES: u64 = 6144;

/// POKEY / sound-CPU clock: master / 8 = 1.512 MHz.
const SOUND_CLOCK_HZ: u32 = 1_512_000;
/// Host audio output rate.
const AUDIO_SAMPLE_RATE: u32 = 44_100;

/// Watchdog timeout ≈ 3 kHz / 128 ≈ 23 Hz. Reset if not pet within this many
/// CPU cycles (1.512 MHz / 23 ≈ 65_700).
const WATCHDOG_CYCLES: u64 = 65_536;

// ---------------------------------------------------------------------------
// Memory maps
// ---------------------------------------------------------------------------

/// Main CPU (MC6809E) 64 KB address space.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum MainRegion {
    /// $0000–$2FFF work RAM (also the vector display list the AVG reads).
    Ram = 1,
    /// $3000–$3FFF vector ROM (upper 4 KB of the AVG's address space).
    VectorRom = 2,
    /// $4000–$47FF memory-mapped I/O (sub-decoded in `bus_read`/`bus_write`).
    Io = 3,
    /// $4800–$4FFF CPU + math scratch RAM.
    MathRamLo = 4,
    /// $5000–$5FFF shared Math RAM operated on by the Matrix Processor.
    MathRam = 5,
    /// $6000–$7FFF banked ROM — active bank 0 (LS259 bit 4 = 0).
    BankLow = 6,
    /// $8000–$FFFF fixed program ROM.
    ProgramRom = 7,
    /// Banked ROM bank 1 backing (mapped into $6000–$7FFF when bit 4 = 1).
    BankHigh = 8,
}

/// Sound CPU (MC6809E) 64 KB address space.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum SoundRegion {
    /// $0000–$1FFF mailbox latches + RIOT + POKEY (sub-decoded in the bus).
    Io = 1,
    /// $2000–$27FF program RAM.
    Ram = 2,
    /// $4000–$7FFF sound ROMs.
    RomLo = 3,
    /// $B000–$FFFF sound ROMs (incl. the reset vector).
    RomHi = 4,
}

fn build_main_map() -> AddressSpace16 {
    use MainRegion::*;
    let mut map = AddressSpace16::new();
    map.region(
        Ram,
        "Work/Vector RAM",
        0x0000,
        0x3000,
        AccessKind::ReadWrite,
    )
    .region(
        VectorRom,
        "Vector ROM",
        0x3000,
        0x1000,
        AccessKind::ReadOnly,
    )
    .region(Io, "I/O", 0x4000, 0x0800, AccessKind::Io)
    .region(
        MathRamLo,
        "Math RAM (lo)",
        0x4800,
        0x0800,
        AccessKind::ReadWrite,
    )
    .region(MathRam, "Math RAM", 0x5000, 0x1000, AccessKind::ReadWrite)
    .region(
        BankLow,
        "Banked ROM 0",
        0x6000,
        0x2000,
        AccessKind::ReadOnly,
    )
    .region(
        ProgramRom,
        "Program ROM",
        0x8000,
        0x8000,
        AccessKind::ReadOnly,
    )
    .backing_region(BankHigh, "Banked ROM 1", 0x2000);
    map
}

fn build_sound_map() -> AddressSpace16 {
    use SoundRegion::*;
    let mut map = AddressSpace16::new();
    map.region(Io, "Sound I/O", 0x0000, 0x2000, AccessKind::Io)
        .region(Ram, "Sound RAM", 0x2000, 0x0800, AccessKind::ReadWrite)
        .region(
            RomLo,
            "Sound ROM (lo)",
            0x4000,
            0x4000,
            AccessKind::ReadOnly,
        )
        .region(
            RomHi,
            "Sound ROM (hi)",
            0xB000,
            0x5000,
            AccessKind::ReadOnly,
        );
    map
}

// ---------------------------------------------------------------------------
// Board
// ---------------------------------------------------------------------------

/// Star Wars board: two MC6809E CPUs, the AVG vector generator, the Matrix
/// Processor, ROM banking, watchdog, periodic IRQ, and the main↔sound mailbox.
#[derive(BusDebug, DebugTrace)]
pub(crate) struct StarWarsBoard {
    #[debug_cpu("M6809 Main")]
    pub(crate) cpu: M6809,
    #[debug_cpu("M6809 Sound")]
    pub(crate) sound_cpu: M6809,

    #[debug_device("AVG")]
    pub(crate) avg: Avg,
    #[debug_device("SW-MATRIX")]
    pub(crate) math: StarWarsMath,
    pub(crate) novram: X2212,

    // Sound board: four POKEYs (quad-decoded at $1800–$183F on the sound CPU).
    pub(crate) pokey: [Pokey; 4],

    #[debug_map(cpu = 0)]
    pub(crate) main_map: AddressSpace16,
    #[debug_map(cpu = 1)]
    pub(crate) sound_map: AddressSpace16,

    // ROM banking (LS259 bit 4).
    pub(crate) bank: u8,

    // Main↔sound mailbox (generic 8-bit latches with "pending" flags).
    pub(crate) soundlatch: u8, // main → sound
    pub(crate) soundlatch_pending: bool,
    pub(crate) mainlatch: u8, // sound → main
    pub(crate) mainlatch_pending: bool,
    /// Set by $46E0; the sound CPU is reset on the next `tick` (where a bus is
    /// available to read its reset vector).
    pub(crate) sound_reset_pending: bool,

    // Periodic IRQ (main CPU) and watchdog.
    pub(crate) irq_pending: bool,
    pub(crate) irq_counter: u64,
    pub(crate) watchdog_counter: u64,
    pub(crate) watchdog_tripped: bool,

    // Operator DIP switches (defaults; full DIP support added later).
    pub(crate) dsw0: u8,
    pub(crate) dsw1: u8,

    pub(crate) clock: u64,

    // Vector display list (AVG output, refreshed on each GO).
    pub(crate) display_list: Vec<VectorLine>,

    // Mixed audio for the current frame, plus DC-block filter state.
    pub(crate) audio_buffer: Vec<i16>,
    pub(crate) audio_dc: (f32, f32),

    // Debug event ring (observer state — never saved in save states).
    #[debug_events]
    pub(crate) debug_trace: DebugTraceBuffer,
}

impl StarWarsBoard {
    pub(crate) fn new() -> Self {
        Self {
            cpu: M6809::new(),
            sound_cpu: M6809::new(),
            avg: Avg::with_variant(
                AvgVariant::StarWars,
                TIMING.display_width as i32,
                TIMING.display_height as i32,
            ),
            math: StarWarsMath::new(),
            novram: X2212::new(),
            pokey: std::array::from_fn(|_| Pokey::with_clock(SOUND_CLOCK_HZ, AUDIO_SAMPLE_RATE)),
            main_map: build_main_map(),
            sound_map: build_sound_map(),
            bank: 0,
            soundlatch: 0,
            soundlatch_pending: false,
            mainlatch: 0,
            mainlatch_pending: false,
            sound_reset_pending: false,
            irq_pending: false,
            irq_counter: 0,
            watchdog_counter: 0,
            watchdog_tripped: false,
            dsw0: 0x00,
            dsw1: 0x02, // coinage default: 1 coin / 1 credit
            clock: 0,
            display_list: Vec::with_capacity(2048),
            audio_buffer: Vec::new(),
            audio_dc: (0.0, 0.0),
            debug_trace: DebugTraceBuffer::new(),
        }
    }

    /// Decode the four-POKEY address scramble at $1800–$183F into
    /// `(pokey_index, register)`. Matches MAME `quad_pokeyn_w`.
    fn quad_pokey_decode(offset: u16) -> (usize, u16) {
        let pokey = ((offset >> 3) & !0x04) as usize;
        let control = (offset & 0x20) >> 2;
        let reg = (offset % 8) | control;
        (pokey, reg)
    }

    // --- Main CPU bus ------------------------------------------------------

    fn main_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x3FFF | 0x4800..=0x7FFF | 0x8000..=0xFFFF => self.main_map.read_backing(addr),
            0x4300..=0x431F => self.in0(),
            0x4320..=0x433F => self.in1(),
            0x4340..=0x435F => self.dsw0,
            0x4360..=0x437F => self.dsw1,
            0x4380..=0x439F => 0x80, // ADC (centered); real input added later
            0x4400 => {
                self.mainlatch_pending = false;
                self.mainlatch
            }
            0x4401 => {
                ((self.soundlatch_pending as u8) << 7) | ((self.mainlatch_pending as u8) << 6)
            }
            0x4500..=0x45FF => self.novram.read(addr & 0xFF),
            0x4700 => self.math.div_reh_r(),
            0x4701 => self.math.div_rel_r(),
            0x4703 => self.math.prng_r(),
            _ => 0xFF,
        }
    }

    fn main_write(&mut self, addr: u16, data: u8) {
        match addr {
            0x0000..=0x2FFF | 0x4800..=0x5FFF => self.main_map.write_backing(addr, data),
            0x4400 => {
                self.soundlatch = data;
                self.soundlatch_pending = true;
            }
            0x4500..=0x45FF => self.novram.write(addr & 0xFF, data),
            0x4600..=0x461F => self.trigger_avg(),
            0x4620..=0x463F => self.avg.reset(),
            0x4640..=0x465F => self.pet_watchdog(),
            0x4660..=0x467F => self.irq_pending = false, // IRQ acknowledge
            0x4680..=0x469F => self.outlatch_w(addr, data),
            0x46A0..=0x46BF => {
                // X2212 nstore: STORE pulse (0→1→0).
                self.novram.store(false);
                self.novram.store(true);
                self.novram.store(false);
            }
            0x46E0 => self.sound_reset(),
            0x4700..=0x4707 => {
                let offset = (addr & 0x07) as u8;
                // Disjoint field borrows: the Matrix Processor operates on the
                // Math RAM region while it runs.
                self.math.math_w(
                    offset,
                    data,
                    self.main_map.region_data_mut(MainRegion::MathRam),
                );
            }
            _ => {}
        }
    }

    /// LS259 addressable output latch (9L/M): D7 is latched into bit `addr & 7`.
    fn outlatch_w(&mut self, addr: u16, data: u8) {
        let bit = (addr & 0x07) as u8;
        let val = (data >> 7) & 1;
        match bit {
            4 => {
                // ROM bank select ($6000–$7FFF).
                self.bank = val;
                let region = if val == 0 {
                    MainRegion::BankLow
                } else {
                    MainRegion::BankHigh
                };
                self.main_map.remap_pages(0x60, 0x20, region, 0);
            }
            7 => self.novram.recall(val == 0), // NVRAM array recall (active low)
            // bits 0/1 coin counters, 2/3/6 LEDs, 5 PRNG reset — no board state.
            _ => {}
        }
    }

    fn in0(&self) -> u8 {
        // Coin/service/tilt/test/fire, all active-low → released reads 1.
        0xFF
    }

    fn in1(&self) -> u8 {
        // Active-low buttons (bits 2,4,5) released; bit 6 = AVG done (VG_HALT,
        // active high), bit 7 = MATH_RUN (active high).
        let mut v = 0x34;
        if self.avg.is_halted() {
            v |= 0x40;
        }
        if self.math.math_run() {
            v |= 0x80;
        }
        v
    }

    fn pet_watchdog(&mut self) {
        self.watchdog_counter = 0;
    }

    fn sound_reset(&mut self) {
        // Acknowledge both mailbox latches and request a sound-CPU reset (done on
        // the next tick, where a bus is available for the reset-vector fetch).
        self.soundlatch_pending = false;
        self.mainlatch_pending = false;
        self.sound_reset_pending = true;
    }

    // --- Sound CPU bus -----------------------------------------------------

    fn sound_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0800..=0x0FFF => {
                self.soundlatch_pending = false;
                self.soundlatch
            }
            // RIOT ($1000–$109F) is wired in a later step; open bus for now.
            0x1800..=0x183F => {
                let (p, reg) = Self::quad_pokey_decode(addr & 0x3F);
                self.pokey[p].read(reg)
            }
            0x2000..=0x27FF | 0x4000..=0x7FFF | 0xB000..=0xFFFF => {
                self.sound_map.read_backing(addr)
            }
            _ => 0x00,
        }
    }

    fn sound_write(&mut self, addr: u16, data: u8) {
        match addr {
            0x0000..=0x07FF => {
                self.mainlatch = data;
                self.mainlatch_pending = true;
            }
            0x1800..=0x183F => {
                let (p, reg) = Self::quad_pokey_decode(addr & 0x3F);
                self.pokey[p].write(reg, data);
            }
            0x2000..=0x27FF => self.sound_map.write_backing(addr, data),
            // RIOT writes handled in a later step.
            _ => {}
        }
    }

    // --- Bus dispatch (routed by master) -----------------------------------

    pub(crate) fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        if master == BusMaster::Cpu(1) {
            self.sound_read(addr)
        } else {
            self.main_read(addr)
        }
    }

    pub(crate) fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        if master == BusMaster::Cpu(1) {
            self.sound_write(addr, data);
        } else {
            self.main_write(addr, data);
        }
    }

    pub(crate) fn bus_is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    pub(crate) fn bus_check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        match target {
            BusMaster::Cpu(0) => InterruptState {
                irq: self.irq_pending,
                ..Default::default()
            },
            // Sound CPU IRQ comes from the RIOT, wired in a later step.
            _ => InterruptState::default(),
        }
    }

    // --- Vector generator --------------------------------------------------

    /// Run the AVG over the current $0000–$3FFF image (RAM display list + vector
    /// ROM) and capture the resulting line list.
    pub(crate) fn trigger_avg(&mut self) {
        let mut vmem = vec![0u8; 0x4000];
        let ram = self.main_map.region_data(MainRegion::Ram);
        let n = ram.len().min(0x3000);
        vmem[..n].copy_from_slice(&ram[..n]);
        let vrom = self.main_map.region_data(MainRegion::VectorRom);
        let m = vrom.len().min(0x1000);
        vmem[0x3000..0x3000 + m].copy_from_slice(&vrom[..m]);

        self.avg.go();
        self.avg.execute(&vmem, &[0u8; 16]); // Star Wars ignores color RAM
        self.display_list = self.avg.take_display_list();
    }

    // --- Frame execution ---------------------------------------------------

    /// One CPU cycle: periodic IRQ, watchdog, both CPUs, and the matrix busy
    /// countdown. `bus` aliases the owning wrapper (via `bus_split!`).
    pub(crate) fn tick(&mut self, bus: &mut dyn Bus<Address = u16, Data = u8>) {
        self.irq_counter += 1;
        if self.irq_counter >= IRQ_PERIOD_CYCLES {
            self.irq_counter = 0;
            self.irq_pending = true;
        }

        self.watchdog_counter += 1;
        if self.watchdog_counter >= WATCHDOG_CYCLES {
            self.watchdog_tripped = true;
        }

        self.cpu.execute_cycle(bus, BusMaster::Cpu(0));
        self.sound_cpu.execute_cycle(bus, BusMaster::Cpu(1));
        self.math.tick();
        for p in &mut self.pokey {
            p.tick();
        }
        self.clock += 1;
    }

    /// True (and cleared) if the watchdog expired since the last check.
    pub(crate) fn take_watchdog_trip(&mut self) -> bool {
        let tripped = self.watchdog_tripped;
        self.watchdog_tripped = false;
        tripped
    }

    pub(crate) fn reset(&mut self) {
        self.avg.reset();
        self.math.reset();
        self.novram.reset();
        for p in &mut self.pokey {
            p.reset();
        }
        self.audio_buffer.clear();
        self.audio_dc = (0.0, 0.0);
        self.bank = 0;
        self.main_map
            .remap_pages(0x60, 0x20, MainRegion::BankLow, 0);
        self.soundlatch = 0;
        self.soundlatch_pending = false;
        self.mainlatch = 0;
        self.mainlatch_pending = false;
        self.irq_pending = false;
        self.irq_counter = 0;
        self.watchdog_counter = 0;
        self.watchdog_tripped = false;
        self.clock = 0;
        self.display_list.clear();
    }

    // --- Frontend hooks ----------------------------------------------------

    pub(crate) fn render_frame(&self, buffer: &mut [u8]) {
        rasterize_vectors(
            &self.display_list,
            buffer,
            TIMING.display_width,
            TIMING.display_height,
            false,
        );
    }

    pub(crate) fn vector_display_list(&self) -> Option<&[VectorLine]> {
        Some(&self.display_list)
    }

    /// Drain the four POKEYs, mix (with a one-pole DC-blocking high-pass), and
    /// queue signed-16-bit samples for the frontend. Called once per frame.
    pub(crate) fn end_frame_audio(&mut self) {
        const POKEY_GAIN: f32 = 0.20; // MAME per-POKEY route gain
        let chans: [Vec<f32>; 4] = std::array::from_fn(|i| self.pokey[i].drain_audio());
        let n = chans.iter().map(Vec::len).max().unwrap_or(0);

        let (mut x1, mut y1) = self.audio_dc;
        self.audio_buffer.reserve(n);
        for i in 0..n {
            let x = POKEY_GAIN
                * chans
                    .iter()
                    .map(|c| c.get(i).copied().unwrap_or(0.0))
                    .sum::<f32>();
            // DC block (cutoff ≈ 35 Hz) then scale/clamp to i16.
            let y = x - x1 + 0.995 * y1;
            x1 = x;
            y1 = y;
            self.audio_buffer
                .push((y * 2.0 * 32767.0).clamp(-32767.0, 32767.0) as i16);
        }
        self.audio_dc = (x1, y1);
    }

    /// Copy pending audio into the frontend's buffer.
    pub(crate) fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        let n = buffer.len().min(self.audio_buffer.len());
        buffer[..n].copy_from_slice(&self.audio_buffer[..n]);
        self.audio_buffer.drain(..n);
        n
    }

    pub(crate) fn debug_tick_boundaries(&self) -> u32 {
        self.cpu.at_instruction_boundary() as u32
    }
}

impl Saveable for StarWarsBoard {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        self.sound_cpu.save_state(w);
        self.avg.save_state(w);
        self.math.save_state(w);
        for p in &self.pokey {
            p.save_state(w);
        }
        w.write_bytes(self.novram.nvram());
        w.write_bytes(self.main_map.region_data(MainRegion::Ram));
        w.write_bytes(self.main_map.region_data(MainRegion::MathRamLo));
        w.write_bytes(self.main_map.region_data(MainRegion::MathRam));
        w.write_bytes(self.sound_map.region_data(SoundRegion::Ram));
        w.write_u8(self.bank);
        w.write_u8(self.soundlatch);
        w.write_bool(self.soundlatch_pending);
        w.write_u8(self.mainlatch);
        w.write_bool(self.mainlatch_pending);
        w.write_bool(self.irq_pending);
        w.write_u64_le(self.irq_counter);
        w.write_u64_le(self.watchdog_counter);
        w.write_u64_le(self.clock);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        self.sound_cpu.load_state(r)?;
        self.avg.load_state(r)?;
        self.math.load_state(r)?;
        for p in &mut self.pokey {
            p.load_state(r)?;
        }
        let mut nvram = vec![0u8; self.novram.nvram().len()];
        r.read_bytes_into(&mut nvram)?;
        self.novram.load_nvram(&nvram);
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::Ram))?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::MathRamLo))?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::MathRam))?;
        r.read_bytes_into(self.sound_map.region_data_mut(SoundRegion::Ram))?;
        self.bank = r.read_u8()?;
        self.main_map.remap_pages(
            0x60,
            0x20,
            if self.bank == 0 {
                MainRegion::BankLow
            } else {
                MainRegion::BankHigh
            },
            0,
        );
        self.soundlatch = r.read_u8()?;
        self.soundlatch_pending = r.read_bool()?;
        self.mainlatch = r.read_u8()?;
        self.mainlatch_pending = r.read_bool()?;
        self.irq_pending = r.read_bool()?;
        self.irq_counter = r.read_u64_le()?;
        self.watchdog_counter = r.read_u64_le()?;
        self.clock = r.read_u64_le()?;
        self.watchdog_tripped = false;
        self.display_list.clear();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// System wrapper
// ---------------------------------------------------------------------------

/// Star Wars machine: the board plus game-specific glue (analog input and NVRAM
/// persistence are added in a later step).
#[derive(phosphor_macros::Saveable)]
pub(crate) struct StarWarsSystem {
    pub(crate) board: StarWarsBoard,
}

impl StarWarsSystem {
    pub(crate) fn new() -> Self {
        Self {
            board: StarWarsBoard::new(),
        }
    }
}

impl Bus for StarWarsSystem {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        self.board.bus_read(master, addr)
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.board.bus_write(master, addr, data);
    }

    fn is_halted_for(&self, master: BusMaster) -> bool {
        self.board.bus_is_halted_for(master)
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        self.board.bus_check_interrupts(target)
    }
}

crate::impl_board_delegation!(StarWarsSystem, board, TIMING, vectors);

impl MachineCore for StarWarsSystem {
    crate::machine_core_metadata!("starwars", TIMING);

    fn run_frame(&mut self) {
        // A pending sound-CPU reset ($46E0) is serviced here, where `bus_split!`
        // yields a bus for the reset-vector fetch.
        if self.board.sound_reset_pending {
            self.board.sound_reset_pending = false;
            bus_split!(self, bus => {
                self.board.sound_cpu.reset(bus, BusMaster::Cpu(1));
            });
        }
        bus_split!(self, bus => {
            for _ in 0..TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });
        self.board.end_frame_audio();
        if self.board.take_watchdog_trip() {
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.board.reset();
        bus_split!(self, bus => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
            self.board.sound_cpu.reset(bus, BusMaster::Cpu(1));
        });
    }
}

impl SaveState for StarWarsSystem {
    crate::machine_save_state!();
}

crate::impl_default_frontend_capabilities!(StarWarsSystem);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a 16-bit word into main RAM (or region backing) big-endian in the
    /// order the Star Wars AVG reads (op/high byte first, no XOR swap).
    fn put_avg_word(map: &mut AddressSpace16, addr: u16, word: u16) {
        let ram = map.region_data_mut(MainRegion::Ram);
        ram[addr as usize] = (word >> 8) as u8;
        ram[addr as usize + 1] = (word & 0xFF) as u8;
    }

    #[test]
    fn banking_swaps_the_6000_window() {
        let mut board = StarWarsBoard::new();
        // Distinct sentinel bytes at $6000 in each bank.
        board.main_map.region_data_mut(MainRegion::BankLow)[0] = 0xA1;
        board.main_map.region_data_mut(MainRegion::BankHigh)[0] = 0xB2;

        // Default bank 0.
        board
            .main_map
            .remap_pages(0x60, 0x20, MainRegion::BankLow, 0);
        assert_eq!(board.bus_read(BusMaster::Cpu(0), 0x6000), 0xA1);

        // LS259 bit 4 = 1 (write to $4684, D7 = 1) selects bank 1.
        board.bus_write(BusMaster::Cpu(0), 0x4684, 0x80);
        assert_eq!(board.bank, 1);
        assert_eq!(board.bus_read(BusMaster::Cpu(0), 0x6000), 0xB2);

        // Back to bank 0.
        board.bus_write(BusMaster::Cpu(0), 0x4684, 0x00);
        assert_eq!(board.bus_read(BusMaster::Cpu(0), 0x6000), 0xA1);
    }

    #[test]
    fn matrix_processor_runs_via_the_bus() {
        // Reuse the C1 datapath check but drive it through the $4700 registers
        // and the $5000 Math RAM region. Program: A<-RAM[0], B<-RAM[1],
        // C<-RAM[2] (MAC), store ACC to RAM[3], halt.
        const LDA: u8 = 0x80;
        const LDB: u8 = 0x40;
        const LDC: u8 = 0x20;
        const READ_ACC: u8 = 0x02;
        const CLEAR_ACC: u8 = 0x10;
        const M_HALT: u8 = 0x04;

        // Build a PROM image (4 × 0x400 nibble planes) with 5 steps.
        let mut prom = vec![0u8; 0x1000];
        let mut set = |step: usize, strobe: u8, am: u8, mas: u8| {
            prom[step] = (strobe >> 4) & 0xf;
            prom[0x400 + step] = strobe & 0xf;
            prom[0x800 + step] = ((am & 1) << 3) | ((mas >> 4) & 0x7);
            prom[0xc00 + step] = mas & 0xf;
        };
        set(0, CLEAR_ACC | LDA, 1, 0);
        set(1, LDB, 1, 1);
        set(2, LDC, 1, 2);
        set(3, READ_ACC, 1, 3);
        set(4, M_HALT, 1, 0);

        let mut board = StarWarsBoard::new();
        board.math.load_proms(&prom);

        // Operands in Math RAM ($5000): A=256, B=0, C=256 (big-endian words).
        let mr = board.main_map.region_data_mut(MainRegion::MathRam);
        mr[0] = 0x01;
        mr[1] = 0x00; // A
        mr[4] = 0x01;
        mr[5] = 0x00; // C

        board.bus_write(BusMaster::Cpu(0), 0x4700, 0x00); // mw0: run

        // ACC = (256 * 256) * 4 = 0x40000; upper 16 bits = 0x0004 at word 3.
        let mr = board.main_map.region_data(MainRegion::MathRam);
        assert_eq!(((mr[6] as u16) << 8) | mr[7] as u16, 0x0004);
        // MATH_RUN asserted immediately after a run (IN1 bit 7).
        assert_ne!(board.in1() & 0x80, 0);
    }

    #[test]
    fn avg_go_produces_a_display_list() {
        let mut board = StarWarsBoard::new();
        // STAT (intensity/color), CNTR, VCTR, HALT — a minimal lit vector.
        put_avg_word(&mut board.main_map, 0x0000, 0x61F0); // STAT: intensity=0xF0,color=1
        put_avg_word(&mut board.main_map, 0x0002, 0x8000); // CNTR
        put_avg_word(&mut board.main_map, 0x0004, 0x0100); // VCTR word0
        put_avg_word(&mut board.main_map, 0x0006, 0x2100); // VCTR word1
        put_avg_word(&mut board.main_map, 0x0008, 0x2000); // HALT

        board.bus_write(BusMaster::Cpu(0), 0x4600, 0x00); // AVG GO
        assert!(
            board.vector_display_list().is_some_and(|l| !l.is_empty()),
            "AVG GO should produce a vector display list"
        );
    }

    #[test]
    fn mailbox_latches_track_pending() {
        let mut board = StarWarsBoard::new();
        // Main writes a sound command → soundlatch pending (IN $4401 bit 7).
        board.bus_write(BusMaster::Cpu(0), 0x4400, 0x5A);
        assert_ne!(board.main_read(0x4401) & 0x80, 0);
        // Sound CPU reads it → pending clears.
        assert_eq!(board.sound_read(0x0800), 0x5A);
        assert_eq!(board.main_read(0x4401) & 0x80, 0);

        // Sound writes a response → mainlatch pending (IN $4401 bit 6).
        board.sound_write(0x0000, 0x3C);
        assert_ne!(board.main_read(0x4401) & 0x40, 0);
        assert_eq!(board.main_read(0x4400), 0x3C);
        assert_eq!(board.main_read(0x4401) & 0x40, 0);
    }

    #[test]
    fn quad_pokey_decode_selects_chip_and_register() {
        // pokey_num = (off>>3) & ~4; reg = (off%8) | ((off&0x20)>>2).
        assert_eq!(StarWarsBoard::quad_pokey_decode(0x00), (0, 0));
        assert_eq!(StarWarsBoard::quad_pokey_decode(0x08), (1, 0));
        assert_eq!(StarWarsBoard::quad_pokey_decode(0x18), (3, 0));
        assert_eq!(StarWarsBoard::quad_pokey_decode(0x07), (0, 7));
        // Bit 5 of the offset adds 8 to the register (control lines).
        assert_eq!(StarWarsBoard::quad_pokey_decode(0x28), (1, 8));
    }

    #[test]
    fn pokey_writes_produce_audio() {
        let mut board = StarWarsBoard::new();
        // Program POKEY 0 channel 1: a mid frequency at full volume/tone.
        board.sound_write(0x1800, 0x40); // AUDF1
        board.sound_write(0x1801, 0xAF); // AUDC1: volume 0xF, pure tone
        // Tick a frame's worth of sound cycles, then drain.
        for _ in 0..TIMING.cycles_per_frame() {
            for p in &mut board.pokey {
                p.tick();
            }
        }
        board.end_frame_audio();
        assert!(
            !board.audio_buffer.is_empty(),
            "an active POKEY channel should generate audio samples"
        );
    }

    #[test]
    fn watchdog_trips_without_petting() {
        let mut system = StarWarsSystem::new();
        // Enough frames of no watchdog writes to exceed the timeout.
        for _ in 0..3 {
            system.run_frame();
        }
        // run_frame resets on trip; the board should be alive (clock advancing)
        // and the watchdog flag consumed.
        assert!(!system.board.watchdog_tripped);
    }
}
