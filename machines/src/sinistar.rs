use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    ActionRole, DefaultBinding, InputConfigurable, InputControl, InputEvent, InputId, InputKind,
    KeyId, MachineCore, Nvram, PadButton, PadControl, Profilable, SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_macros::Saveable;

use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_high;
use crate::williams::{self, WilliamsBoard, WilliamsConfig};

// ---------------------------------------------------------------------------
// Sinistar ROM definitions (parent set "sinistar", rev 3, from MAME
// williams.cpp). ROMs are matched by CRC32; the `name` is a fallback for
// name-based lookup. Sinistar uses the extra-RAM memory map: 9 banked 4KB
// program ROMs at 0x0000-0x8FFF, two fixed 4KB ROMs at 0xE000-0xFFFF, and a
// 20KB sound ROM (four speech ROMs + the standard sound ROM) at 0xB000-0xFFFF.
// ---------------------------------------------------------------------------

/// Banked program ROMs: 36KB at 0x0000-0x8FFF, nine 4KB chips (overlay video
/// RAM via the 0xC900 bank register, same mechanism as Joust).
pub static SINISTAR_BANKED_ROM: RomRegion = RomRegion {
    size: 0x9000, // 36KB
    entries: &[
        RomEntry {
            name: "sinistar_rom_1-b_16-3004-53.1d",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xf6f3a22c],
        },
        RomEntry {
            name: "sinistar_rom_2-b_16-3004-54.1c",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0xcab3185c],
        },
        RomEntry {
            name: "sinistar_rom_3-b_16-3004-55.1a",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0x1ce1b3cc],
        },
        RomEntry {
            name: "sinistar_rom_4-b_16-3004-56.2d",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0x6da632ba],
        },
        RomEntry {
            name: "sinistar_rom_5-b_16-3004-57.2c",
            size: 0x1000,
            offset: 0x4000,
            crc32: &[0xb662e8fc],
        },
        RomEntry {
            name: "sinistar_rom_6-b_16-3004-58.2a",
            size: 0x1000,
            offset: 0x5000,
            crc32: &[0x2306183d],
        },
        RomEntry {
            name: "sinistar_rom_7-b_16-3004-59.3d",
            size: 0x1000,
            offset: 0x6000,
            crc32: &[0xe5dd918e],
        },
        RomEntry {
            name: "sinistar_rom_8-b_16-3004-60.3c",
            size: 0x1000,
            offset: 0x7000,
            crc32: &[0x4785a787],
        },
        RomEntry {
            name: "sinistar_rom_9-b_16-3004-61.3a",
            size: 0x1000,
            offset: 0x8000,
            crc32: &[0x50cb63ad],
        },
    ],
};

/// Fixed program ROMs: 8KB at 0xE000-0xFFFF, two 4KB chips.
/// Offsets are relative to the extra-RAM `ProgramRom` region base (0xE000).
pub static SINISTAR_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x2000, // 8KB
    entries: &[
        RomEntry {
            name: "sinistar_rom_10-b_16-3004-62.4c",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x3d670417],
        }, // -> 0xE000
        RomEntry {
            name: "sinistar_rom_11-b_16-3004-63.4a",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x3162bc50],
        }, // -> 0xF000
    ],
};

/// Sound board ROMs: 20KB at 0xB000-0xFFFF, four 4KB speech ROMs plus the
/// standard sound ROM. Offsets are relative to the sound ROM region base (0xB000).
pub static SINISTAR_SOUND_ROM: RomRegion = RomRegion {
    size: 0x5000, // 20KB
    entries: &[
        RomEntry {
            name: "3004_speech_ic7_r1_16-3004-52.ic7",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xe1019568],
        }, // -> 0xB000
        RomEntry {
            name: "3004_speech_ic5_r1_16-3004-50.ic5",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0xcf3b5ffd],
        }, // -> 0xC000
        RomEntry {
            name: "3004_speech_ic6_r1_16-3004-51.ic6",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0xff8d2645],
        }, // -> 0xD000
        RomEntry {
            name: "3004_speech_ic4_r1_16-3004-49.ic4",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0x4b56a626],
        }, // -> 0xE000
        RomEntry {
            name: "video_sound_rom_9_std.808.ic12",
            size: 0x1000,
            offset: 0x4000,
            crc32: &[0xb82f4ddb],
        }, // -> 0xF000
    ],
};

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

// Widget PIA Port B (IN1) — player buttons (active-high).
const INPUT_FIRE: u8 = 0; // bit 0
const INPUT_BOMB: u8 = 1; // bit 1
const INPUT_P1_START: u8 = 2; // bit 4
const INPUT_P2_START: u8 = 3; // bit 5
// ROM PIA Port A (IN2) — coin/service (active-high).
const INPUT_COIN: u8 = 4; // bit 4 (Coin 1)
const INPUT_ADVANCE: u8 = 5; // bit 1 (operator "Advance")
const INPUT_AUTO_UP: u8 = 6; // bit 0 (operator "Auto Up / Manual Down")

/// Neutral 49-way joystick encoding on Widget PIA Port A. The stick is read as
/// `(translate49[x] << 4) | translate49[y])`; centered (x=y=0x38, index 3)
/// gives `translate49[3] = 0x7` in both nibbles. Dynamic mapping arrives with
/// the 49-way input issue; until then the stick reads centered.
const NEUTRAL_49WAY: u8 = 0x77;

const SINISTAR_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_FIRE as u16),
        stable_name: "fire",
        label: "Fire",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_BOMB as u16),
        stable_name: "bomb",
        label: "Bomb",
        kind: InputKind::Action(ActionRole::Secondary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P1_START as u16),
        stable_name: "p1_start",
        label: "P1 Start",
        kind: InputKind::Start,
        player: Some(1),
        default_bindings: &[
            DefaultBinding::Key(KeyId::Num1),
            DefaultBinding::Pad(PadControl::Button(PadButton::Start)),
        ],
    },
    InputControl {
        id: InputId(INPUT_P2_START as u16),
        stable_name: "p2_start",
        label: "P2 Start",
        kind: InputKind::Start,
        player: Some(2),
        default_bindings: &[DefaultBinding::Key(KeyId::Num2)],
    },
    InputControl {
        id: InputId(INPUT_COIN as u16),
        stable_name: "coin",
        label: "Coin",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[
            DefaultBinding::Key(KeyId::Num5),
            DefaultBinding::Pad(PadControl::Button(PadButton::Back)),
        ],
    },
    InputControl {
        id: InputId(INPUT_ADVANCE as u16),
        stable_name: "advance",
        label: "Advance",
        kind: InputKind::Service,
        player: None,
        default_bindings: &[DefaultBinding::Key(KeyId::Num6)],
    },
    InputControl {
        id: InputId(INPUT_AUTO_UP as u16),
        stable_name: "auto_up",
        label: "Auto Up / Manual Down",
        kind: InputKind::Service,
        player: None,
        default_bindings: &[DefaultBinding::Key(KeyId::Num7)],
    },
];

// ---------------------------------------------------------------------------
// SinistarSystem — Williams board with the Sinistar (extra-RAM) variant
// ---------------------------------------------------------------------------

/// Sinistar (Williams, 1982). Wraps the shared [`WilliamsBoard`] configured for
/// the extra-RAM memory map, and adds Sinistar's button wiring.
///
/// The 49-way joystick (Widget PIA Port A) and CVSD speech are added by later
/// issues; until then the stick reads centered and the CVSD channel is silent.
#[derive(Saveable)]
pub struct SinistarSystem {
    pub board: WilliamsBoard,

    /// Widget PIA Port B (IN1): fire (b0), bomb (b1), P1 start (b4), P2 start (b5).
    port_b: u8,
}

impl SinistarSystem {
    pub fn new() -> Self {
        let mut sys = Self {
            board: WilliamsBoard::with_config(WilliamsConfig::sinistar()),
            port_b: 0,
        };
        sys.apply_inputs();
        sys
    }

    /// Push the current input state onto the PIA input registers.
    fn apply_inputs(&mut self) {
        self.board.widget_pia.set_port_b_input(self.port_b);
        self.board.widget_pia.set_port_a_input(NEUTRAL_49WAY);
        self.board
            .rom_pia
            .set_port_a_input(self.board.rom_pia_input);
    }

    /// Tick one cycle, splitting the borrow so the board can access the bus.
    pub fn tick(&mut self) {
        bus_split!(self, bus => {
            self.board.tick(bus);
        });
    }

    /// Load program, sound and decoder ROMs from a RomSet using Sinistar's map.
    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        // Validate decoder PROMs (shared across gen-1 boards, not yet wired).
        crate::williams::WILLIAMS_DECODER_PROM.load(rom_set)?;

        self.board.load_rom_regions(
            rom_set,
            &SINISTAR_BANKED_ROM,
            &SINISTAR_PROGRAM_ROM,
            &SINISTAR_SOUND_ROM,
        )
    }
}

impl Default for SinistarSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus — delegates to WilliamsBoard
// ---------------------------------------------------------------------------

impl Bus for SinistarSystem {
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
// Machine traits
// ---------------------------------------------------------------------------

crate::impl_board_delegation!(SinistarSystem, board, williams::TIMING);

impl MachineCore for SinistarSystem {
    crate::machine_core_metadata!("sinistar", williams::TIMING);

    fn run_frame(&mut self) {
        self.apply_inputs();
        bus_split!(self, bus => {
            for _ in 0..williams::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });
    }

    fn reset(&mut self) {
        self.board.reset();
        self.port_b = 0;
        self.apply_inputs();
        bus_split!(self, bus => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
            self.board.sound_cpu.reset(bus, BusMaster::Cpu(1));
        });
    }
}

impl SaveState for SinistarSystem {
    crate::machine_save_state!();
}

impl Nvram for SinistarSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.save_cmos())
    }

    fn load_nvram(&mut self, data: &[u8]) {
        self.board.load_cmos(data);
    }
}

impl InputConfigurable for SinistarSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        SINISTAR_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let InputEvent::Button { id, pressed } = event else {
            return;
        };
        match id.0 as u8 {
            // Player buttons -> Widget PIA Port B (IN1)
            INPUT_FIRE => set_bit_active_high(&mut self.port_b, 0, pressed),
            INPUT_BOMB => set_bit_active_high(&mut self.port_b, 1, pressed),
            INPUT_P1_START => set_bit_active_high(&mut self.port_b, 4, pressed),
            INPUT_P2_START => set_bit_active_high(&mut self.port_b, 5, pressed),
            // Coin/service -> ROM PIA Port A (IN2)
            INPUT_AUTO_UP => set_bit_active_high(&mut self.board.rom_pia_input, 0, pressed),
            INPUT_ADVANCE => set_bit_active_high(&mut self.board.rom_pia_input, 1, pressed),
            INPUT_COIN => set_bit_active_high(&mut self.board.rom_pia_input, 4, pressed),
            _ => {}
        }
        self.apply_inputs();
    }
}

impl Profilable for SinistarSystem {}
impl phosphor_core::core::machine::DipSwitches for SinistarSystem {}
crate::impl_board_debug_trace!(SinistarSystem, board);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = SinistarSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("sinistar", &["sinistar"], create_machine)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_map_to_widget_port_b() {
        let mut sys = SinistarSystem::new();
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_FIRE as u16),
            pressed: true,
        });
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_P1_START as u16),
            pressed: true,
        });
        assert_eq!(sys.port_b, 0b0001_0001); // fire (b0) + P1 start (b4)

        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_FIRE as u16),
            pressed: false,
        });
        assert_eq!(sys.port_b, 0b0001_0000); // fire released, start held
    }

    #[test]
    fn coin_and_service_map_to_rom_pia() {
        let mut sys = SinistarSystem::new();
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_COIN as u16),
            pressed: true,
        });
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_ADVANCE as u16),
            pressed: true,
        });
        assert_eq!(sys.board.rom_pia_input, 0b0001_0010); // coin (b4) + advance (b1)
    }

    #[test]
    fn input_controls_exposed_with_stable_names() {
        let sys = SinistarSystem::new();
        let controls = sys.input_controls();
        assert!(controls.iter().any(|c| c.stable_name == "fire"));
        assert!(controls.iter().any(|c| c.stable_name == "bomb"));
        assert!(controls.iter().any(|c| c.stable_name == "coin"));
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = SinistarSystem::new();
        sys.board.write_video_ram(0x200, 0x5C);
        sys.board
            .main_map
            .region_data_mut(williams::MainRegion::Sram)[0x10] = 0x9E;
        sys.port_b = 0b0010_0011;

        let data = sys.save_state().expect("save_state should return Some");

        let mut sys2 = SinistarSystem::new();
        sys2.load_state(&data).unwrap();

        assert_eq!(sys2.board.read_video_ram(0x200), 0x5C);
        assert_eq!(
            sys2.board.main_map.region_data(williams::MainRegion::Sram)[0x10],
            0x9E
        );
        assert_eq!(sys2.port_b, 0b0010_0011);
    }
}
