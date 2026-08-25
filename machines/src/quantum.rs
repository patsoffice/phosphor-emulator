//! Atari Quantum (1982) — MC68000 + Atari color AVG vector display.
//!
//! Quantum was designed by General Computer Corporation under Atari license.
//! It pairs a Motorola 68000 with Atari's color Analog Vector Generator (the
//! Quantum variant — see [`phosphor_core::device::avg`]), two POKEYs for sound,
//! and a trackball. There is no sound CPU; the POKEYs are memory-mapped on the
//! 68000 bus and also serve the DIP switches through their pot inputs.
//!
//! Structurally this mirrors [`crate::foodf`] (single 68000 + POKEY + NVRAM +
//! autovectored IRQ + RMW low-byte I/O on a big-endian word bus), swapping the
//! tilemap/sprite pipeline for a color vector pipeline driven by the shared
//! [`Avg`] device, exactly as [`crate::tempest`] does for the 6502 AVG board.
//!
//! Hardware reference: MAME `src/mame/atari/quantum.cpp`.
//!
//! ## Memory map (word bus, big-endian; base windows, mirrors ignored)
//! ```text
//!   000000-013FFF  Program ROM (5 even/odd chip pairs → 0x14000 image)
//!   018000-01CFFF  Work RAM
//!   800000-801FFF  Vector RAM (8 KB, the AVG display list)
//!   840000-84001F  POKEY 1  (low byte)
//!   840020-84003F  POKEY 2  (low byte)
//!   900000-9001FF  NVRAM (X2212, 256 low-byte cells)
//!   940000         Trackball: (TRACKY << 4) | TRACKX, each 4-bit
//!   948000         SYSTEM input (active-low; bit0 = AVG halt, active-HIGH)
//!   950000-95001F  Color RAM (16 entries, write-only, low byte used)
//!   958000         led_w: coin counters, LEDs, NVRAM store, AVG flip x/y
//!   960000         NVRAM recall (no-op; we persist)
//!   968000         AVG reset (VGRST)
//!   970000         AVG GO   (VGGO)
//!   978000         Watchdog clear
//! ```
//!
//! ## Byte writes on a word bus
//! As in Food Fight, the 68000 turns a byte store into a read-modify-write of
//! the containing word, so low-byte I/O writes (POKEY, NVRAM, color RAM,
//! `led_w`) take `data & 0xFF` and I/O reads stay side-effect-light.

use phosphor_core::audio::{DcBlocker, SampleRing};
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::input::{DrainPolicy, RelativeCounter};
use phosphor_core::core::machine::{
    AnalogAxisKind, AudioSource, DefaultBinding, DipApplyTiming, DipChoice, DipOption,
    DipSwitchBank, DipSwitches, InputConfigurable, InputControl, InputEvent, InputId, InputKind,
    MachineCore, MouseControl, Nvram, Profilable, Renderable, SaveState,
};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace32};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::m68000::M68000;
use phosphor_core::cpu::state::M68000State;
use phosphor_core::cpu::{Cpu, CpuStateTrait};
use phosphor_core::device::avg::{Avg, AvgVariant, VectorMemory};
use phosphor_core::device::dvg::{HALATION_OFF, VectorLine, raster_size_for_field};
use phosphor_core::device::pokey::Pokey;
use phosphor_macros::{BusDebug, MemoryRegion};

use crate::atari_dvg::rasterize_vectors;
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_low;

// ---------------------------------------------------------------------------
// Address-space regions (backed memory only; I/O is decoded in the Bus impl)
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
enum Region {
    Rom = 1,
    Ram = 2,
    VectorRam = 3,
}

// ---------------------------------------------------------------------------
// ROM definitions (three revisions; matched by CRC32 so modern and 0.148
// filenames both resolve). Each is five even/odd `ROM_LOAD16_BYTE` pairs.
// The entries here just concatenate the ten 8 KB chips back-to-back
// ([even0][odd0][even1][odd1]…); `load_program` then de-interleaves each pair
// into the big-endian 0x14000 image (even chip = high byte).
// ---------------------------------------------------------------------------

/// Quantum (rev 2) — the parent set.
/// Quantum program ROMs — ten 0x2000 chips loaded as `ROM_LOAD16_BYTE` pairs
/// and de-interleaved into the 68000 big-endian image (see `load_program`).
///
/// A single region covers all three released ROM sets (rev 2, rev 1, and the
/// prototype). The loader matches each chip by CRC32 first, so whichever set is
/// present loads correctly; the `name` fallbacks use the rev 2 (parent
/// "quantum") filenames. Per-chip CRC order is rev 2, rev 1, prototype, deduped
/// where a chip is shared between revisions.
static QUANTUM_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x14000,
    entries: &[
        RomEntry {
            name: "136016-201.2e",
            size: 0x2000,
            offset: 0x00000,
            crc32: &[0x7e7be63a, 0x5af0bd5b, 0x176d73d3], // rev2, rev1, proto
        },
        RomEntry {
            name: "136016-206.3e",
            size: 0x2000,
            offset: 0x02000,
            crc32: &[0x2d8f5759, 0xf9724666, 0x12fc631f], // rev2, rev1, proto
        },
        RomEntry {
            name: "136016-102.2f",
            size: 0x2000,
            offset: 0x04000,
            crc32: &[0x408d34f4, 0xb64fab48], // rev2+rev1, proto
        },
        RomEntry {
            name: "136016-107.3f",
            size: 0x2000,
            offset: 0x06000,
            crc32: &[0x63154484, 0xa52a9433], // rev2+rev1, proto
        },
        RomEntry {
            name: "136016-203.2hj",
            size: 0x2000,
            offset: 0x08000,
            crc32: &[0xbdc52fad, 0x948f228b, 0x5b29cba3], // rev2, rev1, proto
        },
        RomEntry {
            name: "136016-208.3hj",
            size: 0x2000,
            offset: 0x0A000,
            crc32: &[0xdab4066b, 0xe4c48e4e, 0xc64fc03a], // rev2, rev1, proto
        },
        RomEntry {
            name: "136016-104.2k",
            size: 0x2000,
            offset: 0x0C000,
            crc32: &[0xbf271e5c, 0x854f9c09], // rev2+rev1, proto
        },
        RomEntry {
            name: "136016-109.3k",
            size: 0x2000,
            offset: 0x0E000,
            crc32: &[0xd2894424, 0x1aac576c], // rev2+rev1, proto
        },
        RomEntry {
            name: "136016-105.2l",
            size: 0x2000,
            offset: 0x10000,
            crc32: &[0x13ec512c, 0x1285b5e7], // rev2+rev1, proto
        },
        RomEntry {
            name: "136016-110.3l",
            size: 0x2000,
            offset: 0x12000,
            crc32: &[0xacb50363, 0xe19de844], // rev2+rev1, proto
        },
    ],
};

/// AVG state PROM (256×4) — the same part Tempest uses. It is the vector
/// generator's next-state table: it sequences every vector instruction and so
/// decides how long the generator takes. Without it the AVG draws nothing.
static QUANTUM_AVG_PROM: RomRegion = RomRegion {
    size: 0x100,
    entries: &[RomEntry {
        name: "136002-125.6h",
        size: 0x100,
        offset: 0x0000,
        crc32: &[0x5903af03],
    }],
};

// ---------------------------------------------------------------------------
// Input IDs
// ---------------------------------------------------------------------------

pub const INPUT_COIN1: u8 = 0;
pub const INPUT_COIN2: u8 = 1;
pub const INPUT_COIN3: u8 = 2;
pub const INPUT_START1: u8 = 3;
pub const INPUT_START2: u8 = 4;
pub const INPUT_SERVICE: u8 = 5;

// Typed control ids for the trackball axes (distinct from the digital ids).
const CTRL_TRACK_X: InputId = InputId(8);
const CTRL_TRACK_Y: InputId = InputId(9);

/// Typed logical controls. The trackball axes map to the mouse; coins/start use
/// the shared default bindings.
const QUANTUM_CONTROLS: &[InputControl] = &[
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
        id: InputId(INPUT_COIN3 as u16),
        stable_name: "coin3",
        label: "Coin 3",
        kind: InputKind::Coin,
        player: None,
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
        id: InputId(INPUT_SERVICE as u16),
        stable_name: "service",
        label: "Service",
        kind: InputKind::Service,
        player: None,
        default_bindings: crate::input_defaults::SERVICE,
    },
    InputControl {
        id: CTRL_TRACK_X,
        stable_name: "trackball_x",
        label: "Trackball X",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::X,
        },
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::AxisX)],
    },
    InputControl {
        id: CTRL_TRACK_Y,
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
// Master clock 12.096 MHz. CPU = master/2 = 6.048 MHz. ~60 Hz frame →
// 6_048_000 / 60 = 100_800 CPU cycles/frame. No raster hardware (vector
// display), so the whole frame is one "scanline".
const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 6_048_000,
    cycles_per_scanline: 100_800,
    total_scanlines: 1,
    // Portrait display buffer (the cabinet is ROT270). MAME's vector space is
    // 0..900 × 0..600 with the AVG transposing X/Y; this engine emits the beam
    // untransposed and rotates by using a portrait buffer, so the dimensions
    // (and the AVG beam center, derived from them) are swapped to 600 × 900.
    display_width: 600,
    display_height: 900,
    display_aspect: Some((3, 4)),
};

/// Periodic IRQ1: MASTER/4096/12 = 246.094 Hz → every 24576 CPU cycles.
const IRQ_PERIOD_CYCLES: u64 = 24_576;

/// POKEY runs at 600 kHz; CPU at 6.048 MHz → ~10 CPU cycles per POKEY clock.
const POKEY_CLOCK_HZ: u32 = 600_000;
const CPU_PER_POKEY: u64 = 10;

/// AVG master-clock cycles per 68000 cycle. The 12.096 MHz crystal drives the
/// vector generator directly and the CPU through a divide-by-2.
const AVG_CYCLES_PER_CPU_CYCLE: u32 = 2;

/// Watchdog timeout in frames (~1 s at 60 Hz). Long enough to survive the
/// boot-time delay loops, short enough to recover from a genuine hang.
const WATCHDOG_FRAMES: u8 = 64;

/// De-interleave the five `ROM_LOAD16_BYTE` pairs into the big-endian program
/// image. The input concatenates the ten 0x2000 chips as
/// `[even0][odd0][even1][odd1]…[even4][odd4]`; each pair fills a 0x4000 region
/// with the even chip at even (high) byte addresses and the odd chip at odd
/// (low) ones.
fn deinterleave_program(chips: &[u8]) -> Vec<u8> {
    let mut image = vec![0u8; 0x1_4000];
    for pair in 0..5 {
        let dst = pair * 0x4000;
        let even = pair * 0x4000; // even-byte chip
        let odd = pair * 0x4000 + 0x2000; // odd-byte chip
        for i in 0..0x2000 {
            image[dst + 2 * i] = chips[even + i]; // even address = high byte
            image[dst + 2 * i + 1] = chips[odd + i]; // odd address = low byte
        }
    }
    image
}

// ---------------------------------------------------------------------------
// QuantumSystem
// ---------------------------------------------------------------------------

/// Atari Quantum arcade system.
#[derive(BusDebug)]
pub struct QuantumSystem {
    #[debug_cpu("M68000")]
    cpu: M68000,

    /// Everything the 68000 talks *to*. Held in its own struct so the CPU and
    /// the bus are disjoint fields: `cpu.execute_cycle(&mut self.board, ..)`
    /// then borrow-checks natively and dispatches at a concrete type.
    #[debug_bus]
    board: QuantumBoard,
}

/// The Quantum bus: address space, AVG, POKEYs, NVRAM and I/O.
#[derive(BusDebug)]
pub struct QuantumBoard {
    #[debug_map(cpu = 0)]
    map: AddressSpace32,
    #[debug_device("AVG")]
    avg: Avg,
    /// POKEY 1 (0x840000) and POKEY 2 (0x840020).
    #[debug_device("POKEY")]
    pokey: [Pokey; 2],

    /// Color RAM: 16 entries; only the low byte (4 active bits) is used.
    color_ram: [u8; 16],

    /// NVRAM (X2212): 256 low-byte cells at 0x900000.
    nvram: [u8; 256],

    /// Vector display list (unrotated AVG coordinates), refreshed on AVG GO.
    display_list: Vec<VectorLine>,

    // Trackball: two 4-bit up/down counters read at 0x940000. Mouse motion
    // accumulates into the *_accum fields, drained per-frame into the counters.
    track_x: RelativeCounter,
    track_y: RelativeCounter,

    /// SYSTEM port (948000), active-low except bit0 (AVG halt, supplied live).
    system_input: u8,
    dsw0: u8,
    dsw1: u8,

    // Periodic IRQ1 (HOLD_LINE: auto-acked when the CPU takes it).
    irq_counter: u64,
    irq_pending: bool,
    prev_irq_taken: bool,

    clock: u64,
    watchdog_count: u8,

    audio_buffer: SampleRing<i16>,
    /// Output coupling capacitor: POKEY is unipolar and idles at zero, so the
    /// DC must be tracked and removed rather than a fixed midpoint assumed.
    dc_blocker: DcBlocker,
}

/// Quantum reads each trackball counter as a small signed 4-bit per-frame
/// delta, so a step larger than +-7 aliases into a stall or a reversal. The
/// excess is dropped rather than carried, so the ball stops when the pointing
/// device does.
fn new_track_counter() -> RelativeCounter {
    RelativeCounter::new(0x0F, 0, false, DrainPolicy::ClampDrop { max_step: 7 })
}

impl QuantumSystem {
    fn build_map() -> AddressSpace32 {
        let mut map = AddressSpace32::new();
        map.region(
            Region::Rom,
            "Program ROM",
            0x00_0000,
            0x1_4000,
            AccessKind::ReadOnly,
        )
        .region(
            Region::Ram,
            "Work RAM",
            0x01_8000,
            0x5000,
            AccessKind::ReadWrite,
        )
        .region(
            Region::VectorRam,
            "Vector RAM",
            0x80_0000,
            0x2000,
            AccessKind::ReadWrite,
        );
        map
    }

    pub fn new() -> Self {
        let mut sys = Self {
            cpu: M68000::new(),
            board: QuantumBoard {
                map: Self::build_map(),
                avg: Avg::with_variant(
                    AvgVariant::Quantum,
                    TIMING.display_width as i32,
                    TIMING.display_height as i32,
                ),
                pokey: [
                    Pokey::with_clock(POKEY_CLOCK_HZ, phosphor_core::audio::host_sample_rate()),
                    Pokey::with_clock(POKEY_CLOCK_HZ, phosphor_core::audio::host_sample_rate()),
                ],
                color_ram: [0; 16],
                nvram: [0xFF; 256], // X2212 powers up 1-filled
                display_list: Vec::with_capacity(2048),
                track_x: new_track_counter(),
                track_y: new_track_counter(),
                system_input: 0xFF,
                dsw0: 0x00,
                dsw1: 0x00,
                irq_counter: 0,
                irq_pending: false,
                prev_irq_taken: false,
                clock: 0,
                watchdog_count: 0,
                audio_buffer: SampleRing::with_capacity(2048),
                dc_blocker: DcBlocker::new(phosphor_core::audio::host_sample_rate()),
            },
        };
        sys.board.refresh_dip_pots();
        sys
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        self.load_program(&QUANTUM_PROGRAM_ROM, rom_set)?;
        let avg_prom = QUANTUM_AVG_PROM.load(rom_set)?;
        self.board.avg.load_state_prom(&avg_prom);
        Ok(())
    }

    fn load_program(&mut self, region: &RomRegion, rom_set: &RomSet) -> Result<(), RomLoadError> {
        // The region concatenates the ten chips as [even0][odd0]…[even4][odd4]
        // (each 0x2000). They are `ROM_LOAD16_BYTE` pairs, so de-interleave each
        // pair into the big-endian image: even chip → even byte (high), odd chip
        // → odd byte (low). Without this the 68000 runs scrambled code.
        let chips = region.load(rom_set)?;
        self.board
            .map
            .load_region(Region::Rom, &deinterleave_program(&chips));
        Ok(())
    }

    pub fn get_cpu_state(&self) -> M68000State {
        self.cpu.snapshot()
    }

    pub fn clock(&self) -> u64 {
        self.board.clock
    }
}

impl QuantumBoard {
    /// Feed the DIP switches to the POKEY pot inputs (same wiring as Food
    /// Fight): POKEY1 pots read DSW0, POKEY2 pots read DSW1; pot n returns DIP
    /// bit n in bit 7, so set the pot level to 0x80 when the bit is set.
    fn refresh_dip_pots(&mut self) {
        for n in 0..8 {
            let l0 = if self.dsw0 & (1 << n) != 0 {
                0x80
            } else {
                0x00
            };
            let l1 = if self.dsw1 & (1 << n) != 0 {
                0x80
            } else {
                0x00
            };
            self.pokey[0].set_pot_input(n, l0);
            self.pokey[1].set_pot_input(n, l1);
        }
    }

    /// `led_w` (0x958000, low byte): coin counters, NVRAM store, LEDs, screen
    /// flip into the AVG.
    fn led_w(&mut self, data: u8) {
        // bits 0,1 coin counters; bit 2 NVRAM store (we persist); bits 4,5 LEDs
        // — none modelled. bits 6,7 flip screen.
        self.avg.set_flip(data & 0x40 != 0, data & 0x80 != 0);
    }

    /// VGGO: restart the vector generator at the top of the list.
    ///
    /// The generator then runs on its own clock from [`step_avg`], so this only
    /// restarts it. Whatever it had drawn since the last frame boundary is
    /// dropped: the game writes GO when it wants the list drawn from the top,
    /// and a partial pass is not a frame.
    ///
    /// [`step_avg`]: Self::step_avg
    fn trigger_avg(&mut self) {
        self.avg.go();
        self.avg.take_display_list();
    }

    /// Advance the vector generator alongside the CPU.
    ///
    /// The 12.096 MHz crystal feeds the 68000 through a divide-by-2 and the
    /// generator's own counter directly, so one CPU cycle is two AVG cycles.
    /// The generator reads vector RAM live, which is the point: Quantum's list
    /// loops forever and the CPU rewrites it underneath a generator that is
    /// still walking it.
    fn step_avg(&mut self) {
        // Quantum has no vector ROM; the whole 0x800000 window is vector RAM.
        let vmem = VectorMemory::ram_only(self.map.region_data(Region::VectorRam));
        if self
            .avg
            .step(AVG_CYCLES_PER_CPU_CYCLE, &vmem, &self.color_ram)
        {
            // Branched back to address 0: that pass over the list is a frame.
            self.display_list = self.avg.take_display_list();
        }
    }

    /// Drain trackball accumulators into the 4-bit counters. Like Tempest's
    /// spinner, the game reads each counter as a small signed per-frame delta,
    /// so clamp to ±7 to avoid 4-bit aliasing on fast motion.
    fn update_trackball(&mut self) {
        self.track_x.update();
        self.track_y.update();
    }

    /// Board work before the CPU's cycle: the periodic IRQ, the POKEYs on their
    /// divider, and the watchpoint attribution latch.
    fn begin_cycle(&mut self, cpu: &M68000) {
        // Periodic IRQ1 (HOLD_LINE).
        self.irq_counter += 1;
        if self.irq_counter >= IRQ_PERIOD_CYCLES {
            self.irq_counter = 0;
            self.irq_pending = true;
        }

        // POKEY runs at 600 kHz ≈ one tick per 10 CPU cycles.
        if self.clock.is_multiple_of(CPU_PER_POKEY) {
            for p in &mut self.pokey {
                p.tick();
            }
        }

        self.step_avg();

        if self.map.has_any_watchpoints() {
            let pc = cpu.at_instruction_boundary().then_some(cpu.pc);
            self.map.latch_access_context(self.clock, pc);
        }
    }

    /// HOLD_LINE auto-ack plus the clock advance.
    ///
    /// The periodic IRQ stays asserted until the CPU's interrupt-acknowledge.
    /// The 68000 core takes a level-1 IRQ while `level > mask` and raises the
    /// mask to 1, so detect the rising edge of "mask reached the IRQ level" and
    /// release the line — otherwise it would re-storm after the handler's RTE.
    fn end_cycle(&mut self, cpu: &M68000) {
        let taken = cpu.interrupt_mask() >= 1;
        if self.irq_pending && taken && !self.prev_irq_taken {
            self.irq_pending = false;
        }
        self.prev_irq_taken = taken;

        self.clock += 1;
    }
}

impl QuantumSystem {
    /// One CPU cycle. The CPU and the board are disjoint fields, so the 68000
    /// drives the board directly — no trait object, no raw-pointer split.
    pub fn tick(&mut self) {
        self.board.begin_cycle(&self.cpu);
        self.cpu.execute_cycle(&mut self.board, BusMaster::Cpu(0));
        self.board.end_cycle(&self.cpu);
    }

    /// Advance one CPU cycle, returning the instruction-boundary mask.
    pub fn step_cycle(&mut self) -> u32 {
        self.tick();
        u32::from(self.cpu.at_instruction_boundary())
    }
}

impl Default for QuantumSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus
// ---------------------------------------------------------------------------

impl Bus for QuantumBoard {
    type Address = u32;
    type Data = u16;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn read(&mut self, master: BusMaster, addr: u32) -> u16 {
        let val = match addr {
            0x00_0000..=0x01_3FFF | 0x01_8000..=0x01_CFFF | 0x80_0000..=0x80_1FFF => {
                self.map.read_bus_word_be(addr)
            }
            0x84_0000..=0x84_001F => self.pokey[0].read(((addr >> 1) & 0x0F) as u16) as u16,
            0x84_0020..=0x84_003F => self.pokey[1].read(((addr >> 1) & 0x0F) as u16) as u16,
            0x90_0000..=0x90_01FF => self.nvram[((addr >> 1) & 0xFF) as usize] as u16,
            // Trackball: (TRACKY << 4) | TRACKX.
            0x94_0000..=0x94_0001 => {
                ((self.track_y.counter() as u16) << 4) | self.track_x.counter() as u16
            }
            // SYSTEM: bit0 = AVG halt (active-HIGH), the rest active-low inputs.
            0x94_8000..=0x94_8001 => {
                let mut v = self.system_input & 0xFE;
                if self.avg.is_halted() {
                    v |= 0x01;
                }
                v as u16
            }
            0x97_8000..=0x97_8001 => 0xFFFF, // watchdog read: no-op
            _ => 0xFFFF,
        };
        self.map.watch_read(0, master, addr, val as u32, 2);
        val
    }

    fn write(&mut self, master: BusMaster, addr: u32, data: u16) {
        self.map.watch_write(0, master, addr, data as u32, 2);
        let byte = (data & 0xFF) as u8;
        match addr {
            0x00_0000..=0x01_3FFF => {} // ROM, ignore
            0x01_8000..=0x01_CFFF | 0x80_0000..=0x80_1FFF => {
                self.map.write_bus_word_be(addr, data);
            }
            0x84_0000..=0x84_001F => self.pokey[0].write(((addr >> 1) & 0x0F) as u16, byte),
            0x84_0020..=0x84_003F => self.pokey[1].write(((addr >> 1) & 0x0F) as u16, byte),
            0x90_0000..=0x90_01FF => self.nvram[((addr >> 1) & 0xFF) as usize] = byte,
            0x95_0000..=0x95_001F => self.color_ram[((addr >> 1) & 0x0F) as usize] = byte,
            0x95_8000..=0x95_8001 => self.led_w(byte),
            0x96_0000..=0x96_0001 => {} // NVRAM recall — no-op (persistent)
            0x96_8000..=0x96_8001 => self.avg.reset(),
            0x97_0000..=0x97_0001 => self.trigger_avg(),
            0x97_8000..=0x97_8001 => self.watchdog_count = 0,
            _ => {}
        }
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            irq_level: if self.irq_pending { 1 } else { 0 },
            // 0xFF ⇒ the 68000 core autovectors (vector 25 for level 1).
            irq_vector: 0xFF,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Capability traits
// ---------------------------------------------------------------------------

impl Renderable for QuantumSystem {
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
        // flip_y = true: Quantum is unrotated (orientation NORMAL), so the GL
        // path maps vector Y=0 to the bottom of the screen (vector_gl.rs). The
        // CPU rasterizer must match, or the picture is vertically mirrored when
        // the debug/profiler panel forces this fallback path. (Tempest's AVG
        // board uses flip_y=false because its GL path applies a 270° rotation
        // that already negates Y.)
        let field = TIMING.display_size();
        let (rw, rh) = raster_size_for_field(field.0, field.1);
        rasterize_vectors(
            &self.board.display_list,
            buffer,
            rw,
            rh,
            field,
            true,
            HALATION_OFF,
        );
    }

    fn vector_display_list(&self) -> Option<&[VectorLine]> {
        Some(&self.board.display_list)
    }

    // No orientation override: the cabinet is a vertical (portrait) monitor,
    // but this engine's vector "rotation" only Y-flips and swaps the window to
    // landscape. Instead the display_size is already portrait (600×900) and the
    // AVG emits screen-space coordinates, so the default (no rotation) is right.
}

impl AudioSource for QuantumSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.board.audio_buffer.pop_front_into(buffer)
    }

    fn audio_sample_rate(&self) -> u32 {
        phosphor_core::audio::host_sample_rate()
    }
}

impl InputConfigurable for QuantumSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        QUANTUM_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Button { id, pressed } => match id.0 as u8 {
                // SYSTEM active-low: coins (bits 5/4/1), start (2/3), service (7).
                INPUT_COIN1 => set_bit_active_low(&mut self.board.system_input, 5, pressed),
                INPUT_COIN2 => set_bit_active_low(&mut self.board.system_input, 4, pressed),
                INPUT_COIN3 => set_bit_active_low(&mut self.board.system_input, 1, pressed),
                INPUT_START1 => set_bit_active_low(&mut self.board.system_input, 2, pressed),
                INPUT_START2 => set_bit_active_low(&mut self.board.system_input, 3, pressed),
                INPUT_SERVICE => set_bit_active_low(&mut self.board.system_input, 7, pressed),
                _ => {}
            },
            InputEvent::Relative { id, delta } => {
                // Hardware crossover: TRACKX reads vertical (reversed), TRACKY
                // reads horizontal. Mouse X drives TRACKY, mouse Y drives
                // TRACKX (negated for the PORT_REVERSE).
                if id == CTRL_TRACK_X {
                    self.board.track_y.add_delta(delta);
                } else if id == CTRL_TRACK_Y {
                    self.board.track_x.add_delta(-delta);
                }
            }
            InputEvent::Absolute { .. } => {}
        }
    }

    /// Also clears conditioned analog state: the digital releases above cannot
    /// reach accumulated motion or a held deflection.
    fn release_all_inputs(&mut self) {
        phosphor_core::core::machine::release_all_controls(self);
        self.board.track_x.release_all();
        self.board.track_y.release_all();
    }
}

impl MachineCore for QuantumSystem {
    crate::machine_core_metadata!("quantum", TIMING);

    fn run_frame(&mut self) {
        self.board.update_trackball();

        for _ in 0..TIMING.cycles_per_frame() {
            self.tick();
        }

        // Watchdog: reboot if the game stops strobing the reset register. The
        // timeout must outlast Quantum's boot-time delay loops (which busy-wait
        // for ~0.3 s ≈ 18 frames while only *reading* the watchdog), so this is
        // far longer than Food Fight's 8-frame timeout.
        self.board.watchdog_count = self.board.watchdog_count.saturating_add(1);
        if self.board.watchdog_count >= WATCHDOG_FRAMES {
            self.reset();
        }

        // Mix the two POKEYs to mono.
        let s0 = self.board.pokey[0].drain_audio();
        let s1 = self.board.pokey[1].drain_audio();
        let len = s0.len().min(s1.len());
        let blocker = &mut self.board.dc_blocker;
        self.board.audio_buffer.extend((0..len).map(|i| {
            // Both POKEYs are unipolar [0, 1] and idle at *zero*, so the board's
            // coupling capacitor is what centres the mix. Subtracting a fixed
            // 0.5 instead mapped silence to -32767 and pinned the output.
            let mixed = (s0[i] + s1[i]) * 0.5;
            (blocker.process(mixed) * 2.0 * 32767.0).clamp(i16::MIN as f32, i16::MAX as f32) as i16
        }));
    }

    fn reset(&mut self) {
        self.board.avg.reset();
        self.board.display_list.clear();
        self.board.irq_pending = false;
        self.board.irq_counter = 0;
        self.board.prev_irq_taken = false;
        self.board.watchdog_count = 0;
        self.board.system_input = 0xFF;
        self.board.track_x = new_track_counter();
        self.board.track_y = new_track_counter();
        self.board.audio_buffer.clear();
        self.board.dc_blocker.reset();
        for p in &mut self.board.pokey {
            p.reset();
        }
        self.board.refresh_dip_pots();

        self.cpu.reset(&mut self.board, BusMaster::Cpu(0));
    }
}

// `MachineDebug` (debug_bus + cycle stepping) via the standalone-debug macro;
// `BusDebug` is `#[derive]`d on the struct above (24-bit `AddressSpace32` bus).
crate::impl_standalone_debug!(QuantumSystem);

impl Saveable for QuantumSystem {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        self.board.avg.save_state(w);
        for p in &self.board.pokey {
            p.save_state(w);
        }
        self.board.dc_blocker.save_state(w);
        w.write_bytes(self.board.map.region_data(Region::Ram));
        w.write_bytes(self.board.map.region_data(Region::VectorRam));
        w.write_bytes(&self.board.color_ram);
        w.write_bytes(&self.board.nvram);
        w.write_u8(self.board.track_x.counter());
        w.write_u8(self.board.track_y.counter());
        w.write_u8(self.board.system_input);
        w.write_u8(self.board.dsw0);
        w.write_u8(self.board.dsw1);
        w.write_u64_le(self.board.irq_counter);
        w.write_bool(self.board.irq_pending);
        w.write_u64_le(self.board.clock);
        w.write_u8(self.board.watchdog_count);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        self.board.avg.load_state(r)?;
        for p in &mut self.board.pokey {
            p.load_state(r)?;
        }
        self.board.dc_blocker.load_state(r)?;
        r.read_bytes_into(self.board.map.region_data_mut(Region::Ram))?;
        r.read_bytes_into(self.board.map.region_data_mut(Region::VectorRam))?;
        r.read_bytes_into(&mut self.board.color_ram)?;
        r.read_bytes_into(&mut self.board.nvram)?;
        self.board.track_x.set_counter(r.read_u8()?);
        self.board.track_y.set_counter(r.read_u8()?);
        self.board.system_input = r.read_u8()?;
        self.board.dsw0 = r.read_u8()?;
        self.board.dsw1 = r.read_u8()?;
        self.board.irq_counter = r.read_u64_le()?;
        self.board.irq_pending = r.read_bool()?;
        self.board.clock = r.read_u64_le()?;
        self.board.watchdog_count = r.read_u8()?;
        self.board.prev_irq_taken = false;
        self.board.display_list.clear();
        self.board.refresh_dip_pots();
        self.board.audio_buffer.clear();
        self.board.dc_blocker.reset();
        Ok(())
    }
}

impl SaveState for QuantumSystem {
    crate::machine_save_state!();
}

impl Nvram for QuantumSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(&self.board.nvram)
    }

    fn load_nvram(&mut self, data: &[u8]) {
        let len = data.len().min(self.board.nvram.len());
        self.board.nvram[..len].copy_from_slice(&data[..len]);
    }
}

impl Profilable for QuantumSystem {}

/// DIP switch metadata for Quantum's DSW0 (read back bit-by-bit through POKEY 1
/// pot lines). DSW1 is unused. Choice bits/labels follow MAME's `quantum`.
const QUANTUM_DIP_BANKS: &[DipSwitchBank] = &[DipSwitchBank {
    name: "DSW0",
    options: &[
        DipOption {
            name: "Bonus Coins",
            mask: 0x07,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "None",
                    value: 0x00,
                },
                DipChoice {
                    label: "1 each 5",
                    value: 0x01,
                },
                DipChoice {
                    label: "1 each 4",
                    value: 0x02,
                },
                DipChoice {
                    label: "1 each 3",
                    value: 0x05,
                },
                DipChoice {
                    label: "2 each 4",
                    value: 0x06,
                },
            ],
        },
        DipOption {
            name: "Left Coin",
            mask: 0x08,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "*1",
                    value: 0x00,
                },
                DipChoice {
                    label: "*2",
                    value: 0x08,
                },
            ],
        },
        DipOption {
            name: "Right Coin",
            mask: 0x30,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "*1",
                    value: 0x00,
                },
                DipChoice {
                    label: "*4",
                    value: 0x20,
                },
                DipChoice {
                    label: "*5",
                    value: 0x10,
                },
                DipChoice {
                    label: "*6",
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
                    label: "1 Coin/1 Credit",
                    value: 0x00,
                },
                DipChoice {
                    label: "2 Coins/1 Credit",
                    value: 0x80,
                },
                DipChoice {
                    label: "1 Coin/2 Credits",
                    value: 0xC0,
                },
                DipChoice {
                    label: "Free Play",
                    value: 0x40,
                },
            ],
        },
    ],
}];

impl DipSwitches for QuantumSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        QUANTUM_DIP_BANKS
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        if bank == 0 { self.board.dsw0 } else { 0 }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        if bank == 0 {
            self.board.dsw0 = value;
            self.board.refresh_dip_pots();
        }
    }
}

impl phosphor_core::core::debug_trace::DebugTrace for QuantumSystem {}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

fn create_quantum(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = QuantumSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

fn create_quantum_bare() -> Box<dyn phosphor_core::core::machine::FrontendMachine> {
    let mut sys = QuantumSystem::new();
    let _ = sys.load_rom_set(&RomSet::blank());
    Box::new(sys)
}

// One registration covers all three ROM sets: the loader tries each ZIP in turn
// and CRC-matches whichever chips are present (see `QUANTUM_PROGRAM_ROM`).
inventory::submit! {
MachineEntry::new("quantum", &["quantum", "quantum1", "quantump"], create_quantum, create_quantum_bare, QUANTUM_CONTROLS) }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dip_default_and_metadata() {
        let sys = QuantumSystem::new();
        assert_eq!(sys.dip_bank_value(0), 0x00);
        crate::assert_dip_banks_valid(sys.dip_banks(), &[sys.dip_bank_value(0)]);
    }

    #[test]
    fn set_dip_option_masks_only_its_bits() {
        let mut sys = QuantumSystem::new();
        // Coinage is option 3 (mask 0xC0); pick "Free Play" (0x40).
        sys.set_dip_option(0, 3, 0x40);
        assert_eq!(sys.dip_bank_value(0), 0x40);
    }

    #[test]
    fn deinterleave_places_even_chip_in_high_bytes() {
        // Two distinct chips per pair: even chip filled with 0xAA, odd with 0x55.
        let mut chips = vec![0u8; 0x1_4000];
        for pair in 0..5 {
            for i in 0..0x2000 {
                chips[pair * 0x4000 + i] = 0xAA; // even chip
                chips[pair * 0x4000 + 0x2000 + i] = 0x55; // odd chip
            }
        }
        let image = deinterleave_program(&chips);
        assert_eq!(image.len(), 0x1_4000);
        // Even byte addresses (high) come from the even chip, odd from the odd.
        assert!(image.iter().step_by(2).all(|&b| b == 0xAA));
        assert!(image.iter().skip(1).step_by(2).all(|&b| b == 0x55));
    }

    #[test]
    fn deinterleave_reconstructs_word_at_pair_boundary() {
        // A recognizable byte at the start of pair 1's even chip lands at the
        // high byte of the first word of pair 1's region (image offset 0x4000).
        let mut chips = vec![0u8; 0x1_4000];
        chips[0x4000] = 0x12; // even chip of pair 1, byte 0
        chips[0x6000] = 0x34; // odd chip of pair 1, byte 0
        let image = deinterleave_program(&chips);
        assert_eq!(image[0x4000], 0x12); // high byte
        assert_eq!(image[0x4001], 0x34); // low byte
    }

    #[test]
    fn map_decodes_documented_windows() {
        let sys = QuantumSystem::new();
        assert_eq!(
            sys.board.map.region_at(0x00_0000).unwrap().id,
            Region::Rom.into()
        );
        assert_eq!(
            sys.board.map.region_at(0x01_8000).unwrap().id,
            Region::Ram.into()
        );
        assert_eq!(
            sys.board.map.region_at(0x80_0000).unwrap().id,
            Region::VectorRam.into()
        );
    }

    #[test]
    fn debug_bus_lists_devices_and_reaches_24bit_ram() {
        use phosphor_core::core::DebugRead;
        use phosphor_core::core::machine::MachineDebug;

        let mut sys = QuantumSystem::new();

        // devices() order follows field order: CPU, scalar AVG, then the
        // expanded [Pokey; 2] as "POKEY 1"/"POKEY 2".
        {
            let bus = sys.debug_bus().expect("Quantum exposes a debug bus");
            let devices: Vec<&str> = bus.devices().iter().map(|(n, _)| *n).collect();
            assert_eq!(devices, vec!["M68000", "AVG", "POKEY 1", "POKEY 2"]);
        }

        // Work RAM at 0x01_8100 sits above 0xFFFF — round-trips through the
        // full 24-bit debug address.
        sys.debug_bus_mut().unwrap().write(0, 0x01_8100, 0xA5);
        let bus = sys.debug_bus().unwrap();
        assert!(matches!(
            bus.peek(0, 0x01_8100),
            DebugRead::Backed { value: 0xA5, .. }
        ));
        assert!(matches!(bus.peek(0, 0x80_0000), DebugRead::Backed { .. }));
        assert_eq!(bus.peek(0, 0x50_0000), DebugRead::Unmapped);
    }

    #[test]
    fn render_frame_places_vector_y0_at_bottom() {
        // The GL renderer maps vector Y=0 to the bottom of the screen; the CPU
        // rasterizer (used while the debug/profiler panel is open) must agree,
        // or the image is vertically mirrored. A bright segment at Y=0 must
        // light pixels in the bottom rows, never the top half.
        use phosphor_core::device::dvg::VectorLine;

        let mut sys = QuantumSystem::new();
        sys.board.display_list = vec![VectorLine {
            x0: 280.0,
            y0: 0.0,
            x1: 320.0,
            y1: 0.0,
            intensity: 0xF,
            r: 255,
            g: 255,
            b: 255,
            beam_cycles: 0,
        }];

        let (w, h) = (
            TIMING.display_width as usize,
            TIMING.display_height as usize,
        );
        let mut buf = vec![0u8; w * h * 3];
        sys.render_frame(&mut buf);

        let row_lit = |row: usize| buf[row * w * 3..(row + 1) * w * 3].iter().any(|&b| b != 0);
        assert!((h - 10..h).any(row_lit), "Y=0 should light the bottom rows");
        assert!(!(0..h / 2).any(row_lit), "top half should be dark");
    }

    #[test]
    fn nvram_powers_up_one_filled_and_round_trips() {
        let mut sys = QuantumSystem::new();
        assert_eq!(sys.board.nvram[0], 0xFF);
        Bus::write(&mut sys.board, BusMaster::Cpu(0), 0x90_0000, 0x1234);
        assert_eq!(sys.board.nvram[0], 0x34);
        assert_eq!(
            Bus::read(&mut sys.board, BusMaster::Cpu(0), 0x90_0000),
            0x0034
        );
    }

    #[test]
    fn ram_word_access_round_trips() {
        let mut sys = QuantumSystem::new();
        Bus::write(&mut sys.board, BusMaster::Cpu(0), 0x01_8000, 0xBEEF);
        assert_eq!(
            Bus::read(&mut sys.board, BusMaster::Cpu(0), 0x01_8000),
            0xBEEF
        );
    }

    #[test]
    fn trackball_packs_two_nibbles() {
        let mut sys = QuantumSystem::new();
        sys.board.track_x.set_counter(0x3);
        sys.board.track_y.set_counter(0x5);
        assert_eq!(
            Bus::read(&mut sys.board, BusMaster::Cpu(0), 0x94_0000),
            0x0053
        );
    }

    #[test]
    fn system_reports_avg_halt_in_bit0() {
        let mut sys = QuantumSystem::new();
        // Fresh AVG is halted → bit0 set.
        assert_eq!(
            Bus::read(&mut sys.board, BusMaster::Cpu(0), 0x94_8000) & 1,
            1
        );
    }

    #[test]
    fn reset_loads_ssp_and_pc_from_vectors() {
        let mut sys = QuantumSystem::new();
        let rom = sys.board.map.region_data_mut(Region::Rom);
        rom[0..8].copy_from_slice(&[0x00, 0x01, 0x80, 0x00, 0x00, 0x00, 0x04, 0x00]);
        sys.reset();
        let st = sys.get_cpu_state();
        assert_eq!(st.a[7], 0x0001_8000);
        assert_eq!(st.pc, 0x0000_0400);
    }

    /// Boot a hand-assembled 68000 program on the full board and prove the core
    /// runs it and services the auto-acked periodic IRQ.
    #[test]
    fn synthetic_program_boots_and_takes_interrupts() {
        let mut sys = QuantumSystem::new();
        {
            let rom = sys.board.map.region_data_mut(Region::Rom);
            // Reset vectors: SSP = 0x00018800, PC = 0x00000400.
            rom[0x00..0x08].copy_from_slice(&[0x00, 0x01, 0x88, 0x00, 0x00, 0x00, 0x04, 0x00]);
            // Autovector 25 (level-1 IRQ) → 0x500.
            rom[25 * 4..25 * 4 + 4].copy_from_slice(&[0x00, 0x00, 0x05, 0x00]);

            // Main: enable interrupts (mask 0), bump a counter forever.
            //   move #$2000, sr ; addq.l #1,$18000 ; bra.s loop
            let main: &[u8] = &[
                0x46, 0xFC, 0x20, 0x00, // move #$2000, sr
                0x52, 0xB9, 0x00, 0x01, 0x80, 0x00, // addq.l #1, $00018000
                0x60, 0xF8, // bra.s loop
            ];
            rom[0x400..0x400 + main.len()].copy_from_slice(main);

            // IRQ handler: bump $18010, rte (HOLD_LINE auto-acks the line).
            let handler: &[u8] = &[
                0x52, 0xB9, 0x00, 0x01, 0x80, 0x10, // addq.l #1, $00018010
                0x4E, 0x73, // rte
            ];
            rom[0x500..0x500 + handler.len()].copy_from_slice(handler);
        }

        sys.reset();
        assert_eq!(sys.get_cpu_state().pc, 0x0000_0400);

        for _ in 0..3 {
            sys.run_frame();
        }

        let pc = sys.get_cpu_state().pc;
        assert!(pc < 0x1_4000, "PC {pc:#08X} escaped ROM");

        let ram = sys.board.map.region_data(Region::Ram);
        let alive = u32::from_be_bytes([ram[0], ram[1], ram[2], ram[3]]);
        assert!(alive > 0, "main loop never ran");
        let taken = u32::from_be_bytes([ram[0x10], ram[0x11], ram[0x12], ram[0x13]]);
        assert!(taken > 0, "no interrupts were serviced");
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = QuantumSystem::new();
        sys.board.map.region_data_mut(Region::Ram)[0x100] = 0xAB;
        sys.board.map.region_data_mut(Region::VectorRam)[0x10] = 0xCD;
        sys.board.color_ram[5] = 0x0A;
        sys.board.nvram[0x20] = 0x42;
        sys.board.system_input = 0xF0;
        sys.board.track_x.set_counter(0x7);
        sys.board.irq_pending = true;
        sys.board.clock = 12_345;
        sys.board.watchdog_count = 3;

        let data = SaveState::save_state(&sys).expect("save");
        let cpu_snap = sys.get_cpu_state();

        let mut sys2 = QuantumSystem::new();
        SaveState::load_state(&mut sys2, &data).unwrap();

        assert_eq!(sys2.get_cpu_state(), cpu_snap);
        assert_eq!(sys2.board.map.region_data(Region::Ram)[0x100], 0xAB);
        assert_eq!(sys2.board.map.region_data(Region::VectorRam)[0x10], 0xCD);
        assert_eq!(sys2.board.color_ram[5], 0x0A);
        assert_eq!(sys2.board.nvram[0x20], 0x42);
        assert_eq!(sys2.board.system_input, 0xF0);
        assert_eq!(sys2.board.track_x.counter(), 0x7);
        assert!(sys2.board.irq_pending);
        assert_eq!(sys2.board.clock, 12_345);
        assert_eq!(sys2.board.watchdog_count, 3);
    }
}
