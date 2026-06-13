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
use crate::williams::{self, WILLIAMS_SOUND_ROM, WilliamsBoard};

// Re-export decoder PROM under the original name for backward compatibility.
pub use crate::williams::WILLIAMS_DECODER_PROM as JOUST_DECODER_PROM;

// ---------------------------------------------------------------------------
// Joust ROM definitions (from MAME williams.cpp)
//
// Three label variants exist: Green (parent "joust"), Yellow ("jousty"),
// Red ("joustr"). Each CRC32 slice lists accepted values across variants.
// The `name` field uses the MAME parent set filename as a fallback.
// ---------------------------------------------------------------------------

/// Banked program ROMs: 36KB at 0x0000-0x8FFF, nine 4KB chips.
/// These overlap video RAM and require ROM banking (register 0xC900).
pub static JOUST_BANKED_ROM: RomRegion = RomRegion {
    size: 0x9000, // 36KB
    entries: &[
        RomEntry {
            name: "joust_rom_1b_3006-13.e4",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xfe41b2af], // same across all variants
        },
        RomEntry {
            name: "joust_rom_2b_3006-14.c4",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x501c143c], // same across all variants
        },
        RomEntry {
            name: "joust_rom_3b_3006-15.a4",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0x43f7161d], // same across all variants
        },
        RomEntry {
            name: "joust_rom_4b_3006-16.e5",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0xdb5571b6, 0xab347170], // green+yellow, red
        },
        RomEntry {
            name: "joust_rom_5b_3006-17.c5",
            size: 0x1000,
            offset: 0x4000,
            crc32: &[0xc686bb6b], // same across all variants
        },
        RomEntry {
            name: "joust_rom_6b_3006-18.a5",
            size: 0x1000,
            offset: 0x5000,
            crc32: &[0xfac5f2cf, 0x3d9a6fac], // green+yellow, red
        },
        RomEntry {
            name: "joust_rom_7b_3006-19.e6",
            size: 0x1000,
            offset: 0x6000,
            crc32: &[0x81418240, 0xe6f439c4, 0x0a70b3d1], // green, yellow, red
        },
        RomEntry {
            name: "joust_rom_8b_3006-20.c6",
            size: 0x1000,
            offset: 0x7000,
            crc32: &[0xba5359ba, 0xa7f01504], // green+yellow, red
        },
        RomEntry {
            name: "joust_rom_9b_3006-21.a6",
            size: 0x1000,
            offset: 0x8000,
            crc32: &[0x39643147, 0x978687ad], // green+yellow, red
        },
    ],
};

/// Fixed program ROMs: 12KB at 0xD000-0xFFFF, three 4KB chips.
pub static JOUST_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x3000, // 12KB
    entries: &[
        RomEntry {
            name: "joust_rom_10b_3006-22.a7",
            size: 0x1000,
            offset: 0x0000,                               // -> 0xD000-0xDFFF
            crc32: &[0x3f1c4f89, 0x2039014a, 0xc0c6e52a], // green, yellow, red
        },
        RomEntry {
            name: "joust_rom_11b_3006-23.c7",
            size: 0x1000,
            offset: 0x1000,                   // -> 0xE000-0xEFFF
            crc32: &[0xea48b359, 0xab11bcf9], // green+yellow, red
        },
        RomEntry {
            name: "joust_rom_12b_3006-24.e7",
            size: 0x1000,
            offset: 0x2000,                   // -> 0xF000-0xFFFF
            crc32: &[0xc710717b, 0xea14574b], // green+yellow, red
        },
    ],
};

// ---------------------------------------------------------------------------
// Input definitions
// ---------------------------------------------------------------------------

// Widget PIA Port A — player controls via LS157 mux (active-high)
// CB2 output selects P1 (B input, CB2=1) vs P2 (A input, CB2=0).
// Bits 0-3 come from the active mux channel, bits 4-5 are start buttons.
pub const INPUT_P1_LEFT: u8 = 0; // Mux bit 0 when CB2=1
pub const INPUT_P1_RIGHT: u8 = 1; // Mux bit 1 when CB2=1
pub const INPUT_P1_FLAP: u8 = 2; // Mux bit 2 when CB2=1
pub const INPUT_P2_LEFT: u8 = 3; // Mux bit 0 when CB2=0
pub const INPUT_P2_RIGHT: u8 = 4; // Mux bit 1 when CB2=0
pub const INPUT_P2_FLAP: u8 = 5; // Mux bit 2 when CB2=0
pub const INPUT_P1_START: u8 = 6; // Port A bit 5 (direct, active-high)
pub const INPUT_P2_START: u8 = 7; // Port A bit 4 (direct, active-high)

// ROM PIA Port A — coin/service inputs (active-high)
pub const INPUT_COIN: u8 = 8; // bit 4 (Left Coin)

const JOUST_INPUT_MAP: &[InputButton] = &[
    InputButton {
        id: INPUT_P1_LEFT,
        name: "P1 Left",
    },
    InputButton {
        id: INPUT_P1_RIGHT,
        name: "P1 Right",
    },
    InputButton {
        id: INPUT_P1_FLAP,
        name: "P1 Flap",
    },
    InputButton {
        id: INPUT_P2_LEFT,
        name: "P2 Left",
    },
    InputButton {
        id: INPUT_P2_RIGHT,
        name: "P2 Right",
    },
    InputButton {
        id: INPUT_P2_FLAP,
        name: "P2 Flap",
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
        id: INPUT_COIN,
        name: "Coin",
    },
];

// ---------------------------------------------------------------------------
// JoustSystem — Williams gen-1 board configured for Joust (1982)
// ---------------------------------------------------------------------------

/// Joust-specific wrapper around the shared Williams gen-1 board.
///
/// Adds the LS157 mux for player input multiplexing (CB2 selects P1 vs P2)
/// and Joust-specific ROM definitions.
#[derive(Saveable)]
pub struct JoustSystem {
    pub board: WilliamsBoard,

    // Joust-specific: LS157 mux input state
    p1_controls: u8, // bits 0-2: left, right, flap (mux B input)
    p2_controls: u8, // bits 0-2: left, right, flap (mux A input)
    start_bits: u8,  // bit 4: P2 Start, bit 5: P1 Start
}

impl JoustSystem {
    pub fn new() -> Self {
        Self {
            board: WilliamsBoard::new(),
            p1_controls: 0,
            p2_controls: 0,
            start_bits: 0,
        }
    }

    /// Update Widget PIA Port A based on the LS157 mux state.
    ///
    /// The mux select line is Widget PIA CB2 output:
    /// - CB2 = 1 (select B): P1 controls on bits 0-3
    /// - CB2 = 0 (select A): P2 controls on bits 0-3
    ///
    /// Start buttons on bits 4-5 are always present (direct wiring).
    fn update_widget_mux(&mut self) {
        let select_p1 = self.board.widget_pia.cb2_output();
        let mux_bits = if select_p1 {
            self.p1_controls
        } else {
            self.p2_controls
        };
        let port_a = self.start_bits | (mux_bits & 0x0F);
        self.board.widget_pia.set_port_a_input(port_a);
    }

    fn debug_pre_tick(&mut self) {
        self.update_widget_mux();
    }

    /// Tick one cycle, splitting the borrow so the board can access the bus.
    pub fn tick(&mut self) {
        bus_split!(self, bus => {
            self.board.tick(bus);
        });
    }

    /// Load program ROM from a RomSet using the Joust ROM mapping.
    ///
    /// Matches ROM files by CRC32 checksum (for MAME ROMs with any filename)
    /// with fallback to name-based lookup.
    pub fn load_rom_set(
        &mut self,
        rom_set: &crate::rom_loader::RomSet,
    ) -> Result<(), crate::rom_loader::RomLoadError> {
        // Validate decoder PROMs (not yet wired into memory, but must be present)
        crate::williams::WILLIAMS_DECODER_PROM.load(rom_set)?;

        self.board.load_rom_regions(
            rom_set,
            &JOUST_BANKED_ROM,
            &JOUST_PROGRAM_ROM,
            &WILLIAMS_SOUND_ROM,
        )
    }
}

impl Default for JoustSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus — delegates to WilliamsBoard with Joust-specific mux hook
// ---------------------------------------------------------------------------

impl Bus for JoustSystem {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        // Joust-specific: update LS157 mux before Widget PIA reads
        if master != BusMaster::Cpu(1) && (0xC804..=0xC807).contains(&addr) {
            self.update_widget_mux();
        }
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
// Machine traits — delegates to WilliamsBoard with Joust input wiring
// ---------------------------------------------------------------------------

crate::impl_board_delegation!(JoustSystem, board, williams::TIMING, debug_tick_pre);

impl InputReceiver for JoustSystem {
    fn set_input(&mut self, button: u8, pressed: bool) {
        match button {
            // Player controls go into separate P1/P2 registers.
            // The LS157 mux selects which register appears on Widget PIA
            // Port A bits 0-3 based on CB2 output.
            INPUT_P1_LEFT => set_bit_active_high(&mut self.p1_controls, 0, pressed),
            INPUT_P1_RIGHT => set_bit_active_high(&mut self.p1_controls, 1, pressed),
            INPUT_P1_FLAP => set_bit_active_high(&mut self.p1_controls, 2, pressed),
            INPUT_P2_LEFT => set_bit_active_high(&mut self.p2_controls, 0, pressed),
            INPUT_P2_RIGHT => set_bit_active_high(&mut self.p2_controls, 1, pressed),
            INPUT_P2_FLAP => set_bit_active_high(&mut self.p2_controls, 2, pressed),
            // Start buttons are wired directly to Port A (not muxed)
            INPUT_P1_START => set_bit_active_high(&mut self.start_bits, 5, pressed),
            INPUT_P2_START => set_bit_active_high(&mut self.start_bits, 4, pressed),
            // Coin goes to ROM PIA Port A bit 4 (Left Coin)
            INPUT_COIN => {
                set_bit_active_high(&mut self.board.rom_pia_input, 4, pressed);
                self.board
                    .rom_pia
                    .set_port_a_input(self.board.rom_pia_input);
            }
            _ => {}
        }
        self.update_widget_mux();
    }

    fn input_map(&self) -> &[InputButton] {
        JOUST_INPUT_MAP
    }
}

impl MachineCore for JoustSystem {
    crate::machine_core_metadata!("joust", williams::TIMING);

    fn run_frame(&mut self) {
        self.board
            .rom_pia
            .set_port_a_input(self.board.rom_pia_input);
        bus_split!(self, bus => {
            for _ in 0..williams::TIMING.cycles_per_frame() {
                self.update_widget_mux();
                self.board.tick(bus);
            }
        });
    }

    fn reset(&mut self) {
        self.board.reset();
        self.p1_controls = 0;
        self.p2_controls = 0;
        self.start_bits = 0;
        bus_split!(self, bus => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
            self.board.sound_cpu.reset(bus, BusMaster::Cpu(1));
        });
    }
}

impl SaveState for JoustSystem {
    crate::machine_save_state!();
}

impl Nvram for JoustSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.save_cmos())
    }

    fn load_nvram(&mut self, data: &[u8]) {
        self.board.load_cmos(data);
    }
}

impl phosphor_core::core::machine::InputConfigurable for JoustSystem {}
impl Profilable for JoustSystem {}
crate::impl_board_debug_trace!(JoustSystem, board);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = JoustSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("joust", &["joust"], create_machine)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::cpu::CpuStateTrait;

    #[test]
    fn save_load_round_trip() {
        use crate::williams::{MainRegion, SoundRegion};

        let mut sys = JoustSystem::new();

        // Set known board state
        sys.board.write_video_ram(0x100, 0xAA);
        sys.board.main_map.region_data_mut(MainRegion::Palette)[5] = 0x77;
        sys.board.rom_bank = 3;
        sys.board.clock = 50_000;
        sys.board.watchdog_counter = 42;
        sys.board.sound_map.region_data_mut(SoundRegion::Ram)[0x20] = 0xEF;

        // Set Joust-specific input state
        sys.p1_controls = 0x05;
        sys.p2_controls = 0x03;
        sys.start_bits = 0x30;

        // Save
        let data = sys.save_state().expect("save_state should return Some");

        // Capture CPU snapshots for comparison
        let cpu_snap = sys.board.cpu.snapshot();
        let sound_snap = sys.board.sound_cpu.snapshot();

        // Mutate everything
        let mut sys2 = JoustSystem::new();
        sys2.board.write_video_ram(0x100, 0xFF);
        sys2.board.rom_bank = 7;
        sys2.p1_controls = 0xFF;
        sys2.p2_controls = 0xFF;
        sys2.start_bits = 0xFF;

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

        // Verify Joust-specific state
        assert_eq!(sys2.p1_controls, 0x05);
        assert_eq!(sys2.p2_controls, 0x03);
        assert_eq!(sys2.start_bits, 0x30);
    }
}
