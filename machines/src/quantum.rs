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

use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    AnalogAxisKind, AudioSource, DefaultBinding, DipApplyTiming, DipChoice, DipOption,
    DipSwitchBank, DipSwitches, InputConfigurable, InputControl, InputEvent, InputId, InputKind,
    KeyId, MachineCore, MachineDebug, MouseControl, Nvram, Profilable, Renderable, SaveState,
};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace32};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::m68000::M68000;
use phosphor_core::cpu::state::M68000State;
use phosphor_core::cpu::{Cpu, CpuStateTrait};
use phosphor_core::device::avg::{Avg, AvgVariant};
use phosphor_core::device::dvg::VectorLine;
use phosphor_core::device::pokey::Pokey;
use phosphor_macros::MemoryRegion;

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
static QUANTUM_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x14000,
    entries: &[
        RomEntry {
            name: "136016-201.2e",
            size: 0x2000,
            offset: 0x00000,
            crc32: &[0x7e7be63a],
        },
        RomEntry {
            name: "136016-206.3e",
            size: 0x2000,
            offset: 0x02000,
            crc32: &[0x2d8f5759],
        },
        RomEntry {
            name: "136016-102.2f",
            size: 0x2000,
            offset: 0x04000,
            crc32: &[0x408d34f4],
        },
        RomEntry {
            name: "136016-107.3f",
            size: 0x2000,
            offset: 0x06000,
            crc32: &[0x63154484],
        },
        RomEntry {
            name: "136016-203.2hj",
            size: 0x2000,
            offset: 0x08000,
            crc32: &[0xbdc52fad],
        },
        RomEntry {
            name: "136016-208.3hj",
            size: 0x2000,
            offset: 0x0A000,
            crc32: &[0xdab4066b],
        },
        RomEntry {
            name: "136016-104.2k",
            size: 0x2000,
            offset: 0x0C000,
            crc32: &[0xbf271e5c],
        },
        RomEntry {
            name: "136016-109.3k",
            size: 0x2000,
            offset: 0x0E000,
            crc32: &[0xd2894424],
        },
        RomEntry {
            name: "136016-105.2l",
            size: 0x2000,
            offset: 0x10000,
            crc32: &[0x13ec512c],
        },
        RomEntry {
            name: "136016-110.3l",
            size: 0x2000,
            offset: 0x12000,
            crc32: &[0xacb50363],
        },
    ],
};

/// Quantum (rev 1) — differs from rev 2 in four program ROMs.
static QUANTUM1_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x14000,
    entries: &[
        RomEntry {
            name: "136016-101.2e",
            size: 0x2000,
            offset: 0x00000,
            crc32: &[0x5af0bd5b],
        },
        RomEntry {
            name: "136016-106.3e",
            size: 0x2000,
            offset: 0x02000,
            crc32: &[0xf9724666],
        },
        RomEntry {
            name: "136016-102.2f",
            size: 0x2000,
            offset: 0x04000,
            crc32: &[0x408d34f4],
        },
        RomEntry {
            name: "136016-107.3f",
            size: 0x2000,
            offset: 0x06000,
            crc32: &[0x63154484],
        },
        RomEntry {
            name: "136016-103.2hj",
            size: 0x2000,
            offset: 0x08000,
            crc32: &[0x948f228b],
        },
        RomEntry {
            name: "136016-108.3hj",
            size: 0x2000,
            offset: 0x0A000,
            crc32: &[0xe4c48e4e],
        },
        RomEntry {
            name: "136016-104.2k",
            size: 0x2000,
            offset: 0x0C000,
            crc32: &[0xbf271e5c],
        },
        RomEntry {
            name: "136016-109.3k",
            size: 0x2000,
            offset: 0x0E000,
            crc32: &[0xd2894424],
        },
        RomEntry {
            name: "136016-105.2l",
            size: 0x2000,
            offset: 0x10000,
            crc32: &[0x13ec512c],
        },
        RomEntry {
            name: "136016-110.3l",
            size: 0x2000,
            offset: 0x12000,
            crc32: &[0xacb50363],
        },
    ],
};

/// Quantum (prototype).
static QUANTUMP_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x14000,
    entries: &[
        RomEntry {
            name: "quantump.2e",
            size: 0x2000,
            offset: 0x00000,
            crc32: &[0x176d73d3],
        },
        RomEntry {
            name: "quantump.3e",
            size: 0x2000,
            offset: 0x02000,
            crc32: &[0x12fc631f],
        },
        RomEntry {
            name: "quantump.2f",
            size: 0x2000,
            offset: 0x04000,
            crc32: &[0xb64fab48],
        },
        RomEntry {
            name: "quantump.3f",
            size: 0x2000,
            offset: 0x06000,
            crc32: &[0xa52a9433],
        },
        RomEntry {
            name: "quantump.2hj",
            size: 0x2000,
            offset: 0x08000,
            crc32: &[0x5b29cba3],
        },
        RomEntry {
            name: "quantump.3hj",
            size: 0x2000,
            offset: 0x0A000,
            crc32: &[0xc64fc03a],
        },
        RomEntry {
            name: "quantump.2k",
            size: 0x2000,
            offset: 0x0C000,
            crc32: &[0x854f9c09],
        },
        RomEntry {
            name: "quantump.3k",
            size: 0x2000,
            offset: 0x0E000,
            crc32: &[0x1aac576c],
        },
        RomEntry {
            name: "quantump.2l",
            size: 0x2000,
            offset: 0x10000,
            crc32: &[0x1285b5e7],
        },
        RomEntry {
            name: "quantump.3l",
            size: 0x2000,
            offset: 0x12000,
            crc32: &[0xe19de844],
        },
    ],
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
        default_bindings: &[DefaultBinding::Key(KeyId::Num5)],
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
};

/// Periodic IRQ1: MASTER/4096/12 = 246.094 Hz → every 24576 CPU cycles.
const IRQ_PERIOD_CYCLES: u64 = 24_576;

/// POKEY runs at 600 kHz; CPU at 6.048 MHz → ~10 CPU cycles per POKEY clock.
const POKEY_CLOCK_HZ: u32 = 600_000;
const CPU_PER_POKEY: u64 = 10;

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
pub struct QuantumSystem {
    cpu: M68000,
    map: AddressSpace32,
    avg: Avg,
    /// POKEY 1 (0x840000) and POKEY 2 (0x840020).
    pokey: [Pokey; 2],

    /// Color RAM: 16 entries; only the low byte (4 active bits) is used.
    color_ram: [u8; 16],

    /// NVRAM (X2212): 256 low-byte cells at 0x900000.
    nvram: [u8; 256],

    /// Vector display list (unrotated AVG coordinates), refreshed on AVG GO.
    display_list: Vec<VectorLine>,

    // Trackball: two 4-bit up/down counters read at 0x940000. Mouse motion
    // accumulates into the *_accum fields, drained per-frame into the counters.
    track_x: u8,
    track_y: u8,
    track_x_accum: i32,
    track_y_accum: i32,

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

    audio_buffer: Vec<i16>,
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
            map: Self::build_map(),
            avg: Avg::with_variant(
                AvgVariant::Quantum,
                TIMING.display_width as i32,
                TIMING.display_height as i32,
            ),
            pokey: [
                Pokey::with_clock(POKEY_CLOCK_HZ, 44100),
                Pokey::with_clock(POKEY_CLOCK_HZ, 44100),
            ],
            color_ram: [0; 16],
            nvram: [0xFF; 256], // X2212 powers up 1-filled
            display_list: Vec::with_capacity(2048),
            track_x: 0,
            track_y: 0,
            track_x_accum: 0,
            track_y_accum: 0,
            system_input: 0xFF,
            dsw0: 0x00,
            dsw1: 0x00,
            irq_counter: 0,
            irq_pending: false,
            prev_irq_taken: false,
            clock: 0,
            watchdog_count: 0,
            audio_buffer: Vec::with_capacity(2048),
        };
        sys.refresh_dip_pots();
        sys
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        self.load_program(&QUANTUM_PROGRAM_ROM, rom_set)
    }

    fn load_program(&mut self, region: &RomRegion, rom_set: &RomSet) -> Result<(), RomLoadError> {
        // The region concatenates the ten chips as [even0][odd0]…[even4][odd4]
        // (each 0x2000). They are `ROM_LOAD16_BYTE` pairs, so de-interleave each
        // pair into the big-endian image: even chip → even byte (high), odd chip
        // → odd byte (low). Without this the 68000 runs scrambled code.
        let chips = region.load(rom_set)?;
        self.map
            .load_region(Region::Rom, &deinterleave_program(&chips));
        Ok(())
    }

    pub fn get_cpu_state(&self) -> M68000State {
        self.cpu.snapshot()
    }

    pub fn clock(&self) -> u64 {
        self.clock
    }

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

    /// Assemble vector memory and run the AVG to a frame boundary.
    fn trigger_avg(&mut self) {
        // Quantum has no vector ROM; the whole 0x800000 window is vector RAM.
        let vmem = self.map.region_data(Region::VectorRam).to_vec();
        self.avg.go();
        self.avg.execute(&vmem, &self.color_ram);
        self.display_list = self.avg.take_display_list();
    }

    /// Drain trackball accumulators into the 4-bit counters. Like Tempest's
    /// spinner, the game reads each counter as a small signed per-frame delta,
    /// so clamp to ±7 to avoid 4-bit aliasing on fast motion.
    fn update_trackball(&mut self) {
        const MAX_STEP: i32 = 7;
        let sx = self.track_x_accum.clamp(-MAX_STEP, MAX_STEP);
        let sy = self.track_y_accum.clamp(-MAX_STEP, MAX_STEP);
        self.track_x_accum = 0;
        self.track_y_accum = 0;
        self.track_x = self.track_x.wrapping_add(sx as u8) & 0x0F;
        self.track_y = self.track_y.wrapping_add(sy as u8) & 0x0F;
    }

    pub fn tick(&mut self) {
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

        if self.map.has_any_watchpoints() {
            let pc = self.cpu.at_instruction_boundary().then_some(self.cpu.pc);
            self.map.latch_access_context(self.clock, pc);
        }

        bus_split!(self, bus: u32 word => {
            self.cpu.execute_cycle(bus, BusMaster::Cpu(0));
        });

        // HOLD_LINE auto-ack: the periodic IRQ stays asserted until the CPU's
        // interrupt-acknowledge. The 68000 core takes a level-1 IRQ while
        // `level > mask` and raises the mask to 1, so detect the rising edge of
        // "mask reached the IRQ level" and release the line — otherwise it would
        // re-storm after the handler's RTE.
        let taken = self.cpu.interrupt_mask() >= 1;
        if self.irq_pending && taken && !self.prev_irq_taken {
            self.irq_pending = false;
        }
        self.prev_irq_taken = taken;

        self.clock += 1;
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

impl Bus for QuantumSystem {
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
                (((self.track_y & 0x0F) as u16) << 4) | (self.track_x & 0x0F) as u16
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
        TIMING.display_size()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        rasterize_vectors(
            &self.display_list,
            buffer,
            TIMING.display_width,
            TIMING.display_height,
            false,
        );
    }

    fn vector_display_list(&self) -> Option<&[VectorLine]> {
        Some(&self.display_list)
    }

    // No screen_rotation override: the cabinet is a vertical (portrait) monitor,
    // but this engine's vector "rotation" only Y-flips and swaps the window to
    // landscape. Instead the display_size is already portrait (600×900) and the
    // AVG emits screen-space coordinates, so the default (no rotation) is right.
}

impl AudioSource for QuantumSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        let n = buffer.len().min(self.audio_buffer.len());
        buffer[..n].copy_from_slice(&self.audio_buffer[..n]);
        self.audio_buffer.drain(..n);
        n
    }

    fn audio_sample_rate(&self) -> u32 {
        44100
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
                INPUT_COIN1 => set_bit_active_low(&mut self.system_input, 5, pressed),
                INPUT_COIN2 => set_bit_active_low(&mut self.system_input, 4, pressed),
                INPUT_COIN3 => set_bit_active_low(&mut self.system_input, 1, pressed),
                INPUT_START1 => set_bit_active_low(&mut self.system_input, 2, pressed),
                INPUT_START2 => set_bit_active_low(&mut self.system_input, 3, pressed),
                INPUT_SERVICE => set_bit_active_low(&mut self.system_input, 7, pressed),
                _ => {}
            },
            InputEvent::Relative { id, delta } => {
                // Hardware crossover: TRACKX reads vertical (reversed), TRACKY
                // reads horizontal. Mouse X drives TRACKY, mouse Y drives
                // TRACKX (negated for the PORT_REVERSE).
                if id == CTRL_TRACK_X {
                    self.track_y_accum += delta as i32;
                } else if id == CTRL_TRACK_Y {
                    self.track_x_accum -= delta as i32;
                }
            }
            InputEvent::Absolute { .. } => {}
        }
    }
}

impl MachineCore for QuantumSystem {
    crate::machine_core_metadata!("quantum", TIMING);

    fn run_frame(&mut self) {
        self.update_trackball();

        for _ in 0..TIMING.cycles_per_frame() {
            self.tick();
        }

        // Watchdog: reboot if the game stops strobing the reset register. The
        // timeout must outlast Quantum's boot-time delay loops (which busy-wait
        // for ~0.3 s ≈ 18 frames while only *reading* the watchdog), so this is
        // far longer than Food Fight's 8-frame timeout.
        self.watchdog_count = self.watchdog_count.saturating_add(1);
        if self.watchdog_count >= WATCHDOG_FRAMES {
            self.reset();
        }

        // Mix the two POKEYs to mono.
        let s0 = self.pokey[0].drain_audio();
        let s1 = self.pokey[1].drain_audio();
        let len = s0.len().min(s1.len());
        self.audio_buffer.extend((0..len).map(|i| {
            let mixed = (s0[i] + s1[i]) * 0.5; // [0, 1]
            ((mixed - 0.5) * 2.0 * 32767.0) as i16
        }));
    }

    fn reset(&mut self) {
        self.avg.reset();
        self.display_list.clear();
        self.irq_pending = false;
        self.irq_counter = 0;
        self.prev_irq_taken = false;
        self.watchdog_count = 0;
        self.system_input = 0xFF;
        self.track_x = 0;
        self.track_y = 0;
        self.track_x_accum = 0;
        self.track_y_accum = 0;
        self.audio_buffer.clear();
        for p in &mut self.pokey {
            p.reset();
        }
        self.refresh_dip_pots();

        bus_split!(self, bus: u32 word => {
            self.cpu.reset(bus, BusMaster::Cpu(0));
        });
    }
}

impl MachineDebug for QuantumSystem {
    // No BusDebug: the debugger's memory inspector is 16-bit addressed and
    // cannot represent Quantum's 24-bit bus (same as Food Fight). Cycle
    // stepping still works via debug_tick.
    fn cycles_per_frame(&self) -> u64 {
        TIMING.cycles_per_frame()
    }

    fn debug_tick(&mut self) -> u32 {
        self.tick();
        if self.cpu.at_instruction_boundary() {
            1
        } else {
            0
        }
    }
}

impl Saveable for QuantumSystem {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        self.avg.save_state(w);
        for p in &self.pokey {
            p.save_state(w);
        }
        w.write_bytes(self.map.region_data(Region::Ram));
        w.write_bytes(self.map.region_data(Region::VectorRam));
        w.write_bytes(&self.color_ram);
        w.write_bytes(&self.nvram);
        w.write_u8(self.track_x);
        w.write_u8(self.track_y);
        w.write_u8(self.system_input);
        w.write_u8(self.dsw0);
        w.write_u8(self.dsw1);
        w.write_u64_le(self.irq_counter);
        w.write_bool(self.irq_pending);
        w.write_u64_le(self.clock);
        w.write_u8(self.watchdog_count);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        self.avg.load_state(r)?;
        for p in &mut self.pokey {
            p.load_state(r)?;
        }
        r.read_bytes_into(self.map.region_data_mut(Region::Ram))?;
        r.read_bytes_into(self.map.region_data_mut(Region::VectorRam))?;
        r.read_bytes_into(&mut self.color_ram)?;
        r.read_bytes_into(&mut self.nvram)?;
        self.track_x = r.read_u8()?;
        self.track_y = r.read_u8()?;
        self.system_input = r.read_u8()?;
        self.dsw0 = r.read_u8()?;
        self.dsw1 = r.read_u8()?;
        self.irq_counter = r.read_u64_le()?;
        self.irq_pending = r.read_bool()?;
        self.clock = r.read_u64_le()?;
        self.watchdog_count = r.read_u8()?;
        self.prev_irq_taken = false;
        self.display_list.clear();
        self.refresh_dip_pots();
        self.audio_buffer.clear();
        Ok(())
    }
}

impl SaveState for QuantumSystem {
    crate::machine_save_state!();
}

impl Nvram for QuantumSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(&self.nvram)
    }

    fn load_nvram(&mut self, data: &[u8]) {
        let len = data.len().min(self.nvram.len());
        self.nvram[..len].copy_from_slice(&data[..len]);
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
        if bank == 0 { self.dsw0 } else { 0 }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        if bank == 0 {
            self.dsw0 = value;
            self.refresh_dip_pots();
        }
    }
}

impl phosphor_core::core::debug_trace::DebugTrace for QuantumSystem {}

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

fn make(
    region: &'static RomRegion,
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = QuantumSystem::new();
    sys.load_program(region, rom_set)?;
    Ok(Box::new(sys))
}

fn create_quantum(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    make(&QUANTUM_PROGRAM_ROM, rom_set)
}

fn create_quantum1(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    make(&QUANTUM1_PROGRAM_ROM, rom_set)
}

fn create_quantump(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    make(&QUANTUMP_PROGRAM_ROM, rom_set)
}

inventory::submit! {
    MachineEntry::new("quantum", &["quantum"], create_quantum)
}
inventory::submit! {
    MachineEntry::new("quantum1", &["quantum1", "quantum"], create_quantum1)
}
inventory::submit! {
    MachineEntry::new("quantump", &["quantump", "quantum"], create_quantump)
}

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
        assert_eq!(sys.map.region_at(0x00_0000).unwrap().id, Region::Rom.into());
        assert_eq!(sys.map.region_at(0x01_8000).unwrap().id, Region::Ram.into());
        assert_eq!(
            sys.map.region_at(0x80_0000).unwrap().id,
            Region::VectorRam.into()
        );
    }

    #[test]
    fn nvram_powers_up_one_filled_and_round_trips() {
        let mut sys = QuantumSystem::new();
        assert_eq!(sys.nvram[0], 0xFF);
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x90_0000, 0x1234);
        assert_eq!(sys.nvram[0], 0x34);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x90_0000), 0x0034);
    }

    #[test]
    fn ram_word_access_round_trips() {
        let mut sys = QuantumSystem::new();
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x01_8000, 0xBEEF);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x01_8000), 0xBEEF);
    }

    #[test]
    fn trackball_packs_two_nibbles() {
        let mut sys = QuantumSystem::new();
        sys.track_x = 0x3;
        sys.track_y = 0x5;
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x94_0000), 0x0053);
    }

    #[test]
    fn system_reports_avg_halt_in_bit0() {
        let mut sys = QuantumSystem::new();
        // Fresh AVG is halted → bit0 set.
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x94_8000) & 1, 1);
    }

    #[test]
    fn reset_loads_ssp_and_pc_from_vectors() {
        let mut sys = QuantumSystem::new();
        let rom = sys.map.region_data_mut(Region::Rom);
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
            let rom = sys.map.region_data_mut(Region::Rom);
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

        let ram = sys.map.region_data(Region::Ram);
        let alive = u32::from_be_bytes([ram[0], ram[1], ram[2], ram[3]]);
        assert!(alive > 0, "main loop never ran");
        let taken = u32::from_be_bytes([ram[0x10], ram[0x11], ram[0x12], ram[0x13]]);
        assert!(taken > 0, "no interrupts were serviced");
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = QuantumSystem::new();
        sys.map.region_data_mut(Region::Ram)[0x100] = 0xAB;
        sys.map.region_data_mut(Region::VectorRam)[0x10] = 0xCD;
        sys.color_ram[5] = 0x0A;
        sys.nvram[0x20] = 0x42;
        sys.system_input = 0xF0;
        sys.track_x = 0x7;
        sys.irq_pending = true;
        sys.clock = 12_345;
        sys.watchdog_count = 3;

        let data = SaveState::save_state(&sys).expect("save");
        let cpu_snap = sys.get_cpu_state();

        let mut sys2 = QuantumSystem::new();
        SaveState::load_state(&mut sys2, &data).unwrap();

        assert_eq!(sys2.get_cpu_state(), cpu_snap);
        assert_eq!(sys2.map.region_data(Region::Ram)[0x100], 0xAB);
        assert_eq!(sys2.map.region_data(Region::VectorRam)[0x10], 0xCD);
        assert_eq!(sys2.color_ram[5], 0x0A);
        assert_eq!(sys2.nvram[0x20], 0x42);
        assert_eq!(sys2.system_input, 0xF0);
        assert_eq!(sys2.track_x, 0x7);
        assert!(sys2.irq_pending);
        assert_eq!(sys2.clock, 12_345);
        assert_eq!(sys2.watchdog_count, 3);
    }
}
