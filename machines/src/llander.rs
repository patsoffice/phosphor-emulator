use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    FrontendMachine, InputButton, InputReceiver, MachineCore, SaveState,
};
use phosphor_core::core::memory_map::{AccessKind, MemoryMap};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_macros::Saveable;

use crate::atari_dvg::{self, AtariDvgBoard, Region};
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::{set_bit_active_high, set_bit_active_low};

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

const LLANDER_INPUT_MAP: &[InputButton] = &[
    InputButton {
        id: INPUT_COIN,
        name: "Coin",
    },
    InputButton {
        id: INPUT_START,
        name: "P1 Start",
    },
    InputButton {
        id: INPUT_SELECT,
        name: "P1 Select",
    },
    InputButton {
        id: INPUT_ABORT,
        name: "P1 Fire",
    },
    InputButton {
        id: INPUT_ROT_LEFT,
        name: "P1 Left",
    },
    InputButton {
        id: INPUT_ROT_RIGHT,
        name: "P1 Right",
    },
    InputButton {
        id: INPUT_THRUST,
        name: "P1 Up",
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
#[derive(Saveable)]
pub struct LunarLanderSystem {
    pub board: AtariDvgBoard,

    // I/O — Lunar Lander uses mixed active-HIGH/LOW inputs.
    // in0: active-LOW bits 1,2,3,4,5,7 idle HIGH; bits 0,6 are dynamic.
    in0: u8,
    // in1: active-LOW bits 1,3 idle HIGH; others idle LOW.
    in1: u8,
    /// DIP switches (P8): default 0x80 (English, 750 fuel/coin).
    #[save_skip]
    dip_switches: u8,

    // Thrust pedal: analog value 0x00–0xFE.
    // Digital button maps to 0xFE (full thrust) when pressed, 0x00 when released.
    thrust_value: u8,
}

impl LunarLanderSystem {
    fn build_map() -> MemoryMap {
        let mut map = MemoryMap::new();
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
            // Lunar Lander: VROM at DVG 0x0800, size 0x1800
            board: AtariDvgBoard::new(Self::build_map(), 0x0800, 0x1800),
            // Active-LOW bits idle HIGH: IN0 bits 1,2,3,4,5,7
            in0: 0xBE,
            // Active-LOW bits idle HIGH: IN1 bits 1,3
            in1: 0x0A,
            dip_switches: 0x80,
            thrust_value: 0x00,
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

impl Default for LunarLanderSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus implementation
// ---------------------------------------------------------------------------

impl Bus for LunarLanderSystem {
    type Address = u16;
    type Data = u8;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
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

                // DSW1: 0x2800–0x2803 — 74LS253 dual 4:1 multiplexer.
                0x2800..=0x2803 => {
                    let offset = (addr & 3) as u8;
                    let bit0 = (self.dip_switches >> (offset * 2)) & 1;
                    let bit7 = (self.dip_switches >> (offset * 2 + 1)) & 1;
                    bit0 | (bit7 << 7)
                }

                // Thrust pedal: 0x2C00 — analog value 0x00–0xFE.
                0x2C00 => self.thrust_value,

                _ => 0,
            },

            _ => 0,
        };

        self.board.map.check_read_watch(addr, data);
        data
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        let addr = addr & 0x7FFF; // 15-bit address bus

        self.board.map.check_write_watch(addr, data);

        match self.board.map.page(addr).region_id {
            Region::RAM | Region::VECTOR_RAM => self.board.map.write_backing(addr, data),

            Region::IO => match addr {
                0x3000 => self.board.trigger_dvg(),
                0x3200 => { /* output latch: mission lamps stub */ }
                0x3400 => self.board.watchdog_frame_count = 0,
                // Sound register: bits 0-2 thrust volume, bit 3 explosion,
                // bit 4 3KHz tone, bit 5 6KHz tone
                0x3C00 => { /* sound stub */ }
                0x3E00 => { /* noise reset stub */ }
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
        }
    }
}

// ---------------------------------------------------------------------------
// Machine implementation
// ---------------------------------------------------------------------------

crate::impl_board_delegation!(
    LunarLanderSystem,
    board,
    atari_dvg::TIMING,
    no_audio,
    vectors
);

impl InputReceiver for LunarLanderSystem {
    fn set_input(&mut self, button: u8, pressed: bool) {
        match button {
            // IN1
            INPUT_COIN => set_bit_active_low(&mut self.in1, 1, pressed),
            INPUT_START => set_bit_active_high(&mut self.in1, 0, pressed),
            INPUT_SELECT => set_bit_active_high(&mut self.in1, 4, pressed),
            INPUT_ABORT => set_bit_active_high(&mut self.in1, 5, pressed),
            INPUT_ROT_RIGHT => set_bit_active_high(&mut self.in1, 6, pressed),
            INPUT_ROT_LEFT => set_bit_active_high(&mut self.in1, 7, pressed),

            // Thrust pedal: digital approximation
            INPUT_THRUST => {
                self.thrust_value = if pressed { 0xFE } else { 0x00 };
            }

            _ => {}
        }
    }

    fn input_map(&self) -> &[InputButton] {
        LLANDER_INPUT_MAP
    }
}

impl MachineCore for LunarLanderSystem {
    crate::machine_core_metadata!("llander", atari_dvg::TIMING);

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

impl SaveState for LunarLanderSystem {
    crate::machine_save_state!();
}

crate::impl_default_frontend_capabilities!(LunarLanderSystem);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

fn create_machine(rom_set: &RomSet) -> Result<Box<dyn FrontendMachine>, RomLoadError> {
    let mut sys = LunarLanderSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("llander", &["llander"], create_machine)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atari_dvg::Region;
    use phosphor_core::cpu::CpuStateTrait;

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
        let cpu_snap = sys.board.cpu.snapshot();

        // Mutate everything
        let mut sys2 = LunarLanderSystem::new();
        sys2.board.map.region_data_mut(Region::Ram)[0x50] = 0xFF;
        sys2.in0 = 0xFF;
        sys2.board.clock = 999;

        // Load
        sys2.load_state(&data).unwrap();

        // Verify CPU
        assert_eq!(sys2.board.cpu.snapshot(), cpu_snap);

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
