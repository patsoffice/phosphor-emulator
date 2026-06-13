use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    DefaultBinding, FrontendMachine, InputConfigurable, InputControl, InputEvent, InputId,
    InputKind, KeyId, MachineCore, SaveState,
};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_macros::Saveable;

use crate::atari_dvg::{self, AtariDvgBoard, Region};
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_high;

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
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::FIRE,
    },
    InputControl {
        id: InputId(INPUT_HYPERSPACE as u16),
        stable_name: "hyperspace",
        label: "Hyperspace",
        kind: InputKind::Button,
        player: Some(1),
        // Fire takes gamepad A, so hyperspace is key-only (LShift).
        default_bindings: &[DefaultBinding::Key(KeyId::LShift)],
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
#[derive(Saveable)]
pub struct AsteroidsSystem {
    pub board: AtariDvgBoard,

    // I/O — active-HIGH inputs (default 0x00 = all released)
    in0: u8,
    in1: u8,
    /// DIP switches: default 0x84 (English, 3 lives, 1 coin/1 credit).
    #[save_skip]
    dip_switches: u8,
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
            // Asteroids: VROM at DVG 0x1000, size 0x0800
            board: AtariDvgBoard::new(Self::build_map(), 0x1000, 0x0800),
            in0: 0x00,
            in1: 0x00,
            dip_switches: 0x84, // English, 3 lives, 1C/1C
        }
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

impl Bus for AsteroidsSystem {
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

                // DSW1: 0x2800–0x2803 — 74LS253 dual 4:1 multiplexer.
                // A0–A1 select a pair of DIP switch bits: even bit → D0, odd bit → D7.
                0x2800..=0x2803 => {
                    let offset = (addr & 3) as u8;
                    let bit0 = (self.dip_switches >> (offset * 2)) & 1;
                    let bit7 = (self.dip_switches >> (offset * 2 + 1)) & 1;
                    bit0 | (bit7 << 7)
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
            Region::RAM | Region::VECTOR_RAM => self.board.map.write_backing(addr, data),

            Region::IO => match addr {
                0x3000 => self.board.trigger_dvg(),
                0x3200 => { /* output latch stub */ }
                0x3400 => self.board.watchdog_frame_count = 0,
                0x3600 => { /* audio stub */ }
                0x3A00 => { /* audio stub */ }
                0x3C00..=0x3C07 => { /* audio stub */ }
                0x3E00 => { /* audio stub */ }
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

crate::impl_board_delegation!(AsteroidsSystem, board, atari_dvg::TIMING, no_audio, vectors);

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
    crate::machine_core_metadata!("asteroids", atari_dvg::TIMING);

    fn run_frame(&mut self) {
        bus_split!(self, bus => {
            for _ in 0..atari_dvg::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });

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
        bus_split!(self, bus => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
        });
    }
}

impl SaveState for AsteroidsSystem {
    crate::machine_save_state!();
}

impl phosphor_core::core::machine::Nvram for AsteroidsSystem {}
impl phosphor_core::core::machine::Profilable for AsteroidsSystem {}
crate::impl_board_debug_trace!(AsteroidsSystem, board);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

fn create_machine(rom_set: &RomSet) -> Result<Box<dyn FrontendMachine>, RomLoadError> {
    let mut sys = AsteroidsSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("asteroid", &["asteroid"], create_machine)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atari_dvg::Region;
    use phosphor_core::cpu::CpuStateTrait;

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
        let cpu_snap = sys.board.cpu.snapshot();

        // Mutate everything
        let mut sys2 = AsteroidsSystem::new();
        sys2.board.map.region_data_mut(Region::Ram)[0x100] = 0xFF;
        sys2.in0 = 0xFF;
        sys2.board.clock = 999;

        // Load
        sys2.load_state(&data).unwrap();

        // Verify CPU
        assert_eq!(sys2.board.cpu.snapshot(), cpu_snap);

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
