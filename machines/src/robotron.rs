use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    InputButton, InputReceiver, MachineCore, Nvram, Profilable, SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_macros::Saveable;

use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_high;
use crate::williams::{self, WilliamsBoard};

// Re-export decoder PROM under the game-specific name.
pub use crate::williams::WILLIAMS_DECODER_PROM as ROBOTRON_DECODER_PROM;

// ---------------------------------------------------------------------------
// Robotron ROM definitions (from MAME williams.cpp)
//
// Two main label variants: Blue (parent "robotron"), Yellow/Orange ("robotronyo").
// Each CRC32 slice lists accepted values across variants.
// The `name` field uses the MAME parent set filename as a fallback.
// ---------------------------------------------------------------------------

/// Banked program ROMs: 36KB at 0x0000-0x8FFF, nine 4KB chips.
/// These overlap video RAM and require ROM banking (register 0xC900).
pub static ROBOTRON_BANKED_ROM: RomRegion = RomRegion {
    size: 0x9000, // 36KB
    entries: &[
        RomEntry {
            name: "2084_rom_1b_3005-13.e4",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x66c7d3ef], // same across blue+yellow
        },
        RomEntry {
            name: "2084_rom_2b_3005-14.c4",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x5bc6c614], // same across blue+yellow
        },
        RomEntry {
            name: "2084_rom_3b_3005-15.a4",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0xe99a82be, 0x67a369bc], // blue, yellow
        },
        RomEntry {
            name: "2084_rom_4b_3005-16.e5",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0xafb1c561, 0xb0de677a], // blue, yellow
        },
        RomEntry {
            name: "2084_rom_5b_3005-17.c5",
            size: 0x1000,
            offset: 0x4000,
            crc32: &[0x62691e77, 0x24726007], // blue, yellow
        },
        RomEntry {
            name: "2084_rom_6b_3005-18.a5",
            size: 0x1000,
            offset: 0x5000,
            crc32: &[0xbd2c853d, 0x028181a6], // blue, yellow
        },
        RomEntry {
            name: "2084_rom_7b_3005-19.e6",
            size: 0x1000,
            offset: 0x6000,
            crc32: &[0x49ac400c, 0x4dfcceae, 0x8981a43b], // blue, yellow, unpatched
        },
        RomEntry {
            name: "2084_rom_8b_3005-20.c6",
            size: 0x1000,
            offset: 0x7000,
            crc32: &[0x3a96e88c], // same across all variants
        },
        RomEntry {
            name: "2084_rom_9b_3005-21.a6",
            size: 0x1000,
            offset: 0x8000,
            crc32: &[0xb124367b], // same across all variants
        },
    ],
};

/// Fixed program ROMs: 12KB at 0xD000-0xFFFF, three 4KB chips.
pub static ROBOTRON_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x3000, // 12KB
    entries: &[
        RomEntry {
            name: "2084_rom_10b_3005-22.a7",
            size: 0x1000,
            offset: 0x0000,                   // -> 0xD000-0xDFFF
            crc32: &[0x13797024, 0x4a9d5f52], // blue, yellow
        },
        RomEntry {
            name: "2084_rom_11b_3005-23.c7",
            size: 0x1000,
            offset: 0x1000,                   // -> 0xE000-0xEFFF
            crc32: &[0x7e3c1b87, 0x2afc5e7f], // blue, yellow
        },
        RomEntry {
            name: "2084_rom_12b_3005-24.e7",
            size: 0x1000,
            offset: 0x2000,                   // -> 0xF000-0xFFFF
            crc32: &[0x645d543e, 0x45da9202], // blue, yellow
        },
    ],
};

/// Robotron sound ROM: 4KB SC-1 sound board ROM (different from Joust).
/// video_sound_rom_3 (part 767), distinct from Joust's video_sound_rom_4 (part 780).
pub static ROBOTRON_SOUND_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "video_sound_rom_3_std_767.ic12",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0xc56c1d28],
    }],
};

// ---------------------------------------------------------------------------
// Input definitions
// ---------------------------------------------------------------------------

// Widget PIA Port A — move stick (bits 0-3), starts (bits 4-5), fire up/down (bits 6-7)
pub const INPUT_MOVE_UP: u8 = 0;
pub const INPUT_MOVE_DOWN: u8 = 1;
pub const INPUT_MOVE_LEFT: u8 = 2;
pub const INPUT_MOVE_RIGHT: u8 = 3;
pub const INPUT_P1_START: u8 = 4;
pub const INPUT_P2_START: u8 = 5;
pub const INPUT_FIRE_UP: u8 = 6;
pub const INPUT_FIRE_DOWN: u8 = 7;

// Widget PIA Port B — fire left/right (bits 0-1)
pub const INPUT_FIRE_LEFT: u8 = 8;
pub const INPUT_FIRE_RIGHT: u8 = 9;

// ROM PIA Port A — coin/service inputs (active-high)
pub const INPUT_COIN: u8 = 10; // bit 4 (Left Coin)

const ROBOTRON_INPUT_MAP: &[InputButton] = &[
    InputButton {
        id: INPUT_MOVE_UP,
        name: "P1 Up",
    },
    InputButton {
        id: INPUT_MOVE_DOWN,
        name: "P1 Down",
    },
    InputButton {
        id: INPUT_MOVE_LEFT,
        name: "P1 Left",
    },
    InputButton {
        id: INPUT_MOVE_RIGHT,
        name: "P1 Right",
    },
    InputButton {
        id: INPUT_P1_START,
        name: "P1 Start",
    },
    InputButton {
        id: INPUT_P2_START,
        name: "P2 Start",
    },
    InputButton {
        id: INPUT_FIRE_UP,
        name: "P1 Fire Up",
    },
    InputButton {
        id: INPUT_FIRE_DOWN,
        name: "P1 Fire Down",
    },
    InputButton {
        id: INPUT_FIRE_LEFT,
        name: "P1 Fire Left",
    },
    InputButton {
        id: INPUT_FIRE_RIGHT,
        name: "P1 Fire Right",
    },
    InputButton {
        id: INPUT_COIN,
        name: "Coin",
    },
];

// ---------------------------------------------------------------------------
// RobotronSystem — Williams gen-1 board configured for Robotron 2084 (1982)
// ---------------------------------------------------------------------------

/// Robotron 2084 wrapper around the shared Williams gen-1 board.
///
/// Twin-stick controls: move stick on Widget PIA Port A bits 0-3,
/// fire stick split across Port A bits 6-7 (up/down) and Port B bits 0-1 (left/right).
/// No LS157 mux — all inputs directly wired.
#[derive(Saveable)]
pub struct RobotronSystem {
    pub board: WilliamsBoard,

    // Direct-wired input state
    widget_port_a: u8, // bits 0-3: move, bit 4: P1 Start, bit 5: P2 Start, bits 6-7: fire up/down
    widget_port_b: u8, // bits 0-1: fire left/right
}

impl RobotronSystem {
    pub fn new() -> Self {
        Self {
            board: WilliamsBoard::new(),
            widget_port_a: 0,
            widget_port_b: 0,
        }
    }

    fn debug_pre_tick(&mut self) {
        self.board.widget_pia.set_port_a_input(self.widget_port_a);
        self.board.widget_pia.set_port_b_input(self.widget_port_b);
    }

    /// Tick one cycle, splitting the borrow so the board can access the bus.
    pub fn tick(&mut self) {
        bus_split!(self, bus => {
            self.board.tick(bus);
        });
    }

    /// Load program ROM from a RomSet using the Robotron ROM mapping.
    pub fn load_rom_set(
        &mut self,
        rom_set: &crate::rom_loader::RomSet,
    ) -> Result<(), crate::rom_loader::RomLoadError> {
        // Validate decoder PROMs (not yet wired into memory, but must be present)
        crate::williams::WILLIAMS_DECODER_PROM.load(rom_set)?;

        self.board.load_rom_regions(
            rom_set,
            &ROBOTRON_BANKED_ROM,
            &ROBOTRON_PROGRAM_ROM,
            &ROBOTRON_SOUND_ROM,
        )
    }
}

impl Default for RobotronSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus — pure delegation to WilliamsBoard (no game-specific hooks)
// ---------------------------------------------------------------------------

impl Bus for RobotronSystem {
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

// ---------------------------------------------------------------------------
// Machine trait — delegates to WilliamsBoard with Robotron input wiring
// ---------------------------------------------------------------------------

crate::impl_board_delegation!(RobotronSystem, board, williams::TIMING, debug_tick_pre);

impl InputReceiver for RobotronSystem {
    fn set_input(&mut self, button: u8, pressed: bool) {
        match button {
            // Move stick → Widget PIA Port A bits 0-3
            INPUT_MOVE_UP => set_bit_active_high(&mut self.widget_port_a, 0, pressed),
            INPUT_MOVE_DOWN => set_bit_active_high(&mut self.widget_port_a, 1, pressed),
            INPUT_MOVE_LEFT => set_bit_active_high(&mut self.widget_port_a, 2, pressed),
            INPUT_MOVE_RIGHT => set_bit_active_high(&mut self.widget_port_a, 3, pressed),
            // Start buttons → Widget PIA Port A bits 4-5
            INPUT_P1_START => set_bit_active_high(&mut self.widget_port_a, 4, pressed),
            INPUT_P2_START => set_bit_active_high(&mut self.widget_port_a, 5, pressed),
            // Fire stick up/down → Widget PIA Port A bits 6-7
            INPUT_FIRE_UP => set_bit_active_high(&mut self.widget_port_a, 6, pressed),
            INPUT_FIRE_DOWN => set_bit_active_high(&mut self.widget_port_a, 7, pressed),
            // Fire stick left/right → Widget PIA Port B bits 0-1
            INPUT_FIRE_LEFT => set_bit_active_high(&mut self.widget_port_b, 0, pressed),
            INPUT_FIRE_RIGHT => set_bit_active_high(&mut self.widget_port_b, 1, pressed),
            // Coin → ROM PIA Port A bit 4 (Left Coin)
            INPUT_COIN => {
                set_bit_active_high(&mut self.board.rom_pia_input, 4, pressed);
                self.board
                    .rom_pia
                    .set_port_a_input(self.board.rom_pia_input);
            }
            _ => {}
        }
        // Update PIA inputs immediately so direct reads see current state
        self.board.widget_pia.set_port_a_input(self.widget_port_a);
        self.board.widget_pia.set_port_b_input(self.widget_port_b);
    }

    fn input_map(&self) -> &[InputButton] {
        ROBOTRON_INPUT_MAP
    }
}

impl MachineCore for RobotronSystem {
    crate::machine_core_metadata!("robotron", williams::TIMING);

    fn run_frame(&mut self) {
        self.board.widget_pia.set_port_a_input(self.widget_port_a);
        self.board.widget_pia.set_port_b_input(self.widget_port_b);
        self.board
            .rom_pia
            .set_port_a_input(self.board.rom_pia_input);
        bus_split!(self, bus => {
            for _ in 0..williams::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });
    }

    fn reset(&mut self) {
        self.board.reset();
        self.widget_port_a = 0;
        self.widget_port_b = 0;
        bus_split!(self, bus => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
            self.board.sound_cpu.reset(bus, BusMaster::Cpu(1));
        });
    }
}

impl SaveState for RobotronSystem {
    crate::machine_save_state!();
}

impl Nvram for RobotronSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.save_cmos())
    }

    fn load_nvram(&mut self, data: &[u8]) {
        self.board.load_cmos(data);
    }
}

impl Profilable for RobotronSystem {}

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::Machine>, RomLoadError> {
    let mut sys = RobotronSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("robotron", &["robotron"], create_machine)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::cpu::CpuStateTrait;

    #[test]
    fn save_load_round_trip() {
        use crate::williams::{MainRegion, SoundRegion};

        let mut sys = RobotronSystem::new();

        // Set known board state
        sys.board.write_video_ram(0x100, 0xAA);
        sys.board.main_map.region_data_mut(MainRegion::Palette)[5] = 0x77;
        sys.board.rom_bank = 3;
        sys.board.clock = 50_000;
        sys.board.watchdog_counter = 42;
        sys.board.sound_map.region_data_mut(SoundRegion::Ram)[0x20] = 0xEF;

        // Set Robotron-specific input state
        sys.widget_port_a = 0x3F;
        sys.widget_port_b = 0x03;

        // Save
        let data = sys.save_state().expect("save_state should return Some");

        // Capture CPU snapshots for comparison
        let cpu_snap = sys.board.cpu.snapshot();
        let sound_snap = sys.board.sound_cpu.snapshot();

        // Mutate everything
        let mut sys2 = RobotronSystem::new();
        sys2.board.write_video_ram(0x100, 0xFF);
        sys2.board.rom_bank = 7;
        sys2.widget_port_a = 0xFF;
        sys2.widget_port_b = 0xFF;

        // Load
        sys2.load_state(&data).unwrap();

        // Verify CPU state
        assert_eq!(sys2.board.cpu.snapshot(), cpu_snap);
        assert_eq!(sys2.board.sound_cpu.snapshot(), sound_snap);

        // Verify board state
        assert_eq!(sys2.board.read_video_ram(0x100), 0xAA);
        assert_eq!(
            sys2.board.main_map.region_data(MainRegion::Palette)[5],
            0x77
        );
        assert_eq!(sys2.board.rom_bank, 3);
        assert_eq!(sys2.board.clock, 50_000);
        assert_eq!(sys2.board.watchdog_counter, 42);
        assert_eq!(
            sys2.board.sound_map.region_data(SoundRegion::Ram)[0x20],
            0xEF
        );

        // Verify Robotron-specific state
        assert_eq!(sys2.widget_port_a, 0x3F);
        assert_eq!(sys2.widget_port_b, 0x03);
    }
}
