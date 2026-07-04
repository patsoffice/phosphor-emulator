//! Burgertime (Data East, 1982) — the first game on the btime board.
//!
//! Thin wrapper around the shared [`BtimeBoard`] (see `btime.rs`) following the
//! Board Wrapper Pattern (`joust.rs` / `gridlee.rs`): it constructs the board,
//! registers the machine, defines the ROM regions, and wires the game-specific
//! inputs and DIP banks. The board provides the CPUs, video, and sound.

use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    ActionRole, DefaultBinding, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, DipSwitches,
    Direction, InputConfigurable, InputControl, InputEvent, InputId, InputKind, KeyId, MachineCore,
    SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_macros::Saveable;

use crate::btime::{self, BtimeBoard, BtimeConfig};
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::{set_bit_active_high, set_bit_active_low};

// ---------------------------------------------------------------------------
// ROM definitions
// ---------------------------------------------------------------------------

/// Main-CPU program ROM. Region base is 0xB000; the physical ROMs occupy
/// 0xC000-0xFFFF, so the 0xB000-0xBFFF slot (offset 0) is an unused gap.
pub static BURGERTIME_MAIN_ROM: RomRegion = RomRegion {
    size: 0x5000,
    entries: &[
        RomEntry {
            name: "aa04.9b",
            size: 0x1000,
            offset: 0x1000, // -> 0xC000
            crc32: &[0x368a25b5],
        },
        RomEntry {
            name: "aa06.13b",
            size: 0x1000,
            offset: 0x2000, // -> 0xD000
            crc32: &[0xb4ba400d],
        },
        RomEntry {
            name: "aa05.10b",
            size: 0x1000,
            offset: 0x3000, // -> 0xE000
            crc32: &[0x8005bffa],
        },
        RomEntry {
            name: "aa07.15b",
            size: 0x1000,
            offset: 0x4000, // -> 0xF000 (vectors at 0xFFFA-0xFFFF)
            crc32: &[0x086440ad],
        },
    ],
};

/// Character + sprite graphics (gfx1): six 4KB chips, 3bpp planar.
pub static BURGERTIME_GFX1_ROM: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        RomEntry {
            name: "aa12.7k",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xc4617243],
        },
        RomEntry {
            name: "ab13.9k",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0xac01042f],
        },
        RomEntry {
            name: "ab10.10k",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0x854a872a],
        },
        RomEntry {
            name: "ab11.12k",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0xd4848014],
        },
        RomEntry {
            name: "aa8.13k",
            size: 0x1000,
            offset: 0x4000,
            crc32: &[0x8650c788],
        },
        RomEntry {
            name: "ab9.15k",
            size: 0x1000,
            offset: 0x5000,
            crc32: &[0x8dec15e6],
        },
    ],
};

/// Background-tilemap graphics (gfx2): three 2KB chips, 3bpp planar.
pub static BURGERTIME_GFX2_ROM: RomRegion = RomRegion {
    size: 0x1800,
    entries: &[
        RomEntry {
            name: "ab00.1b",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0xc7a14485],
        },
        RomEntry {
            name: "ab01.3b",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x25b49078],
        },
        RomEntry {
            name: "ab02.4b",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0xb8ef56c3],
        },
    ],
};

/// Background tilemap (bg_map): one 2KB chip selecting the 16×16 backdrop tiles.
pub static BURGERTIME_BG_MAP_ROM: RomRegion = RomRegion {
    size: 0x0800,
    entries: &[RomEntry {
        name: "ab03.6b",
        size: 0x0800,
        offset: 0x0000,
        crc32: &[0xd26bc1f3],
    }],
};

/// Sound-CPU program ROM (mapped at 0xE000 on the sound bus). Defined now;
/// wired with the sound M6502 + 2× AY-3-8910 in the audio follow-up (`.8`).
pub static BURGERTIME_SOUND_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "ab14.12h",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0xf55e5211],
    }],
};

const BURGERTIME_CONFIG: BtimeConfig = BtimeConfig { name: "burgertime" };

// ---------------------------------------------------------------------------
// Input definitions.
//   P1 (0x4000) / P2 (0x4001): 4-way joystick + button1, active-low.
//   SYSTEM (0x4002): start1/start2/tilt active-low (bits 0-2); coin1/coin2
//                    active-high (bits 6-7), each coin's rising edge pulses IRQ.
// ---------------------------------------------------------------------------

pub const INPUT_P1_RIGHT: u8 = 0;
pub const INPUT_P1_LEFT: u8 = 1;
pub const INPUT_P1_UP: u8 = 2;
pub const INPUT_P1_DOWN: u8 = 3;
pub const INPUT_P1_BUTTON1: u8 = 4;
pub const INPUT_P2_RIGHT: u8 = 5;
pub const INPUT_P2_LEFT: u8 = 6;
pub const INPUT_P2_UP: u8 = 7;
pub const INPUT_P2_DOWN: u8 = 8;
pub const INPUT_P2_BUTTON1: u8 = 9;
pub const INPUT_COIN1: u8 = 10;
pub const INPUT_START1: u8 = 11;
pub const INPUT_START2: u8 = 12;
pub const INPUT_COIN2: u8 = 13;
pub const INPUT_TILT: u8 = 14;

const BURGERTIME_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_P1_RIGHT as u16),
        stable_name: "p1_right",
        label: "P1 Right",
        kind: InputKind::DigitalDirection {
            direction: Direction::Right,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
    InputControl {
        id: InputId(INPUT_P1_LEFT as u16),
        stable_name: "p1_left",
        label: "P1 Left",
        kind: InputKind::DigitalDirection {
            direction: Direction::Left,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(INPUT_P1_UP as u16),
        stable_name: "p1_up",
        label: "P1 Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_UP,
    },
    InputControl {
        id: InputId(INPUT_P1_DOWN as u16),
        stable_name: "p1_down",
        label: "P1 Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_DOWN,
    },
    InputControl {
        id: InputId(INPUT_P1_BUTTON1 as u16),
        stable_name: "p1_button1",
        label: "P1 Pepper",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P2_RIGHT as u16),
        stable_name: "p2_right",
        label: "P2 Right",
        kind: InputKind::DigitalDirection {
            direction: Direction::Right,
        },
        player: Some(2),
        default_bindings: crate::input_defaults::P2_RIGHT,
    },
    InputControl {
        id: InputId(INPUT_P2_LEFT as u16),
        stable_name: "p2_left",
        label: "P2 Left",
        kind: InputKind::DigitalDirection {
            direction: Direction::Left,
        },
        player: Some(2),
        default_bindings: crate::input_defaults::P2_LEFT,
    },
    InputControl {
        id: InputId(INPUT_P2_UP as u16),
        stable_name: "p2_up",
        label: "P2 Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(2),
        default_bindings: crate::input_defaults::P2_UP,
    },
    InputControl {
        id: InputId(INPUT_P2_DOWN as u16),
        stable_name: "p2_down",
        label: "P2 Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(2),
        default_bindings: crate::input_defaults::P2_DOWN,
    },
    InputControl {
        id: InputId(INPUT_P2_BUTTON1 as u16),
        stable_name: "p2_button1",
        label: "P2 Pepper",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(2),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_COIN1 as u16),
        stable_name: "coin1",
        label: "Coin",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(INPUT_START1 as u16),
        stable_name: "start1",
        label: "P1 Start",
        kind: InputKind::Start,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_START,
    },
    InputControl {
        id: InputId(INPUT_START2 as u16),
        stable_name: "start2",
        label: "P2 Start",
        kind: InputKind::Start,
        player: Some(2),
        default_bindings: crate::input_defaults::P2_START,
    },
    InputControl {
        id: InputId(INPUT_COIN2 as u16),
        stable_name: "coin2",
        label: "Coin 2",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[DefaultBinding::Key(KeyId::Num6)],
    },
    InputControl {
        id: InputId(INPUT_TILT as u16),
        stable_name: "tilt",
        label: "Tilt",
        kind: InputKind::Action(ActionRole::Secondary),
        player: None,
        default_bindings: &[DefaultBinding::Key(KeyId::T)],
    },
];

// ---------------------------------------------------------------------------
// BurgertimeSystem
// ---------------------------------------------------------------------------

/// Data East btime board configured for Burgertime (1982).
#[derive(Saveable)]
pub struct BurgertimeSystem {
    pub board: BtimeBoard,
}

impl BurgertimeSystem {
    pub fn new() -> Self {
        Self {
            board: BtimeBoard::new(BURGERTIME_CONFIG),
        }
    }

    /// Apply a coin button to the given active-high SYSTEM bit, pulsing the main
    /// IRQ on the rising edge (release -> press).
    fn coin(&mut self, bit: u8, pressed: bool) {
        let was_set = self.board.system & (1 << bit) != 0;
        set_bit_active_high(&mut self.board.system, bit, pressed);
        if pressed && !was_set {
            self.board.main_irq = true;
        }
    }

    /// Load the Burgertime ROM set: main + sound program ROMs into the board,
    /// and decode the char/sprite/background graphics + copy the bg_map.
    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let main = BURGERTIME_MAIN_ROM.load(rom_set)?;
        self.board.load_main_rom(&main);
        let sound = BURGERTIME_SOUND_ROM.load(rom_set)?;
        self.board.load_sound_rom(&sound);

        let gfx1 = BURGERTIME_GFX1_ROM.load(rom_set)?;
        self.board.load_gfx1(&gfx1);
        let gfx2 = BURGERTIME_GFX2_ROM.load(rom_set)?;
        self.board.load_gfx2(&gfx2);
        let bg_map = BURGERTIME_BG_MAP_ROM.load(rom_set)?;
        self.board.load_bg_map(&bg_map);

        Ok(())
    }
}

impl Default for BurgertimeSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus — delegates to the board
// ---------------------------------------------------------------------------

impl Bus for BurgertimeSystem {
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

crate::impl_board_delegation!(BurgertimeSystem, board, btime::TIMING);

impl MachineCore for BurgertimeSystem {
    fn gfx_sheets(&self) -> Vec<phosphor_core::core::machine::GfxSheet<'_>> {
        self.board.gfx_sheets()
    }

    fn run_frame(&mut self) {
        // Run one frame's worth of main-CPU cycles. The live VBLANK bit (read at
        // 0x4003) is derived from the clock each cycle, so the game's frame sync
        // works without a periodic interrupt; the coin IRQ is edge-driven.
        bus_split!(self, bus => {
            for _ in 0..btime::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });
        // Render the completed frame once, after the cycle loop.
        self.board.render();
    }

    fn reset(&mut self) {
        self.board.reset();
        bus_split!(self, bus => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
            self.board.sound_cpu.reset(bus, BusMaster::Cpu(1));
        });
    }

    fn frame_rate_hz(&self) -> f64 {
        btime::TIMING.frame_rate_hz()
    }

    fn machine_id(&self) -> &str {
        self.board.machine_id()
    }
}

impl SaveState for BurgertimeSystem {
    crate::machine_save_state!();
}

impl InputConfigurable for BurgertimeSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        BURGERTIME_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let InputEvent::Button { id, pressed } = event else {
            return;
        };
        match id.0 as u8 {
            // P1 controls -> IN0 (0x4000), active-low.
            INPUT_P1_RIGHT => set_bit_active_low(&mut self.board.p1, 0, pressed),
            INPUT_P1_LEFT => set_bit_active_low(&mut self.board.p1, 1, pressed),
            INPUT_P1_UP => set_bit_active_low(&mut self.board.p1, 2, pressed),
            INPUT_P1_DOWN => set_bit_active_low(&mut self.board.p1, 3, pressed),
            INPUT_P1_BUTTON1 => set_bit_active_low(&mut self.board.p1, 4, pressed),
            // P2 controls -> IN1 (0x4001), active-low.
            INPUT_P2_RIGHT => set_bit_active_low(&mut self.board.p2, 0, pressed),
            INPUT_P2_LEFT => set_bit_active_low(&mut self.board.p2, 1, pressed),
            INPUT_P2_UP => set_bit_active_low(&mut self.board.p2, 2, pressed),
            INPUT_P2_DOWN => set_bit_active_low(&mut self.board.p2, 3, pressed),
            INPUT_P2_BUTTON1 => set_bit_active_low(&mut self.board.p2, 4, pressed),
            // Start/tilt -> system (0x4002), active-low.
            INPUT_START1 => set_bit_active_low(&mut self.board.system, 0, pressed),
            INPUT_START2 => set_bit_active_low(&mut self.board.system, 1, pressed),
            INPUT_TILT => set_bit_active_low(&mut self.board.system, 2, pressed),
            // Coins -> system, active-high; each coin's rising edge pulses the
            // main IRQ (HOLD_LINE; cleared when the CPU vectors the interrupt).
            INPUT_COIN1 => self.coin(6, pressed),
            INPUT_COIN2 => self.coin(7, pressed),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// DIP switches
// ---------------------------------------------------------------------------

const BURGERTIME_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSW1 (15D)",
        options: &[
            DipOption {
                name: "Coin A",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "1 Coin/1 Credit",
                        value: 0x03,
                    },
                    DipChoice {
                        label: "1 Coin/2 Credits",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "1 Coin/3 Credits",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "2 Coins/1 Credit",
                        value: 0x00,
                    },
                ],
            },
            DipOption {
                name: "Coin B",
                mask: 0x0C,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "1 Coin/1 Credit",
                        value: 0x0C,
                    },
                    DipChoice {
                        label: "1 Coin/2 Credits",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "1 Coin/3 Credits",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "2 Coins/1 Credit",
                        value: 0x00,
                    },
                ],
            },
            DipOption {
                // Hardware has no test mode; this must stay Off or boot locks up.
                name: "Leave Off",
                mask: 0x10,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Off",
                        value: 0x10,
                    },
                    DipChoice {
                        label: "On",
                        value: 0x00,
                    },
                ],
            },
            DipOption {
                name: "Cabinet",
                mask: 0x40,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Upright",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Cocktail",
                        value: 0x40,
                    },
                ],
            },
        ],
    },
    DipSwitchBank {
        name: "DSW2 (14D)",
        options: &[
            DipOption {
                name: "Lives",
                mask: 0x01,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "3",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "5",
                        value: 0x00,
                    },
                ],
            },
            DipOption {
                name: "Bonus Life",
                mask: 0x06,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "10000",
                        value: 0x06,
                    },
                    DipChoice {
                        label: "15000",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "20000",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "30000",
                        value: 0x00,
                    },
                ],
            },
            DipOption {
                name: "Enemies",
                mask: 0x08,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "4",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "6",
                        value: 0x00,
                    },
                ],
            },
            DipOption {
                name: "End of Level Pepper",
                mask: 0x10,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Yes",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "No",
                        value: 0x10,
                    },
                ],
            },
        ],
    },
];

impl DipSwitches for BurgertimeSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        BURGERTIME_DIP_BANKS
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        match bank {
            0 => self.board.dsw1 & 0x7F, // bit 7 is live VBLANK, not a DIP
            1 => self.board.dsw2,
            _ => 0,
        }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        match bank {
            0 => self.board.dsw1 = value & 0x7F,
            1 => self.board.dsw2 = value,
            _ => {}
        }
    }
}

crate::impl_default_frontend_capabilities!(BurgertimeSystem);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = BurgertimeSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("burgertime", &["btime"], create_machine)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::Renderable;

    #[test]
    fn registered_under_burgertime() {
        assert!(crate::registry::find("burgertime").is_some());
    }

    #[test]
    fn metadata_matches_board() {
        let sys = BurgertimeSystem::new();
        assert_eq!(sys.machine_id(), "burgertime");
        assert_eq!(sys.display_size(), (240, 240)); // native square raster
        assert_eq!(sys.display_aspect(), Some((3, 4))); // presented 3:4 portrait
    }

    #[test]
    fn input_controls_exposed() {
        let sys = BurgertimeSystem::new();
        let controls = sys.input_controls();
        // P1/P2 4-way+button (10), coin1/coin2, start1/start2, tilt.
        assert_eq!(controls.len(), 15);
        assert!(controls.iter().any(|c| c.stable_name == "coin1"));
        assert!(controls.iter().any(|c| c.stable_name == "tilt"));
    }

    #[test]
    fn p1_input_is_active_low() {
        let mut sys = BurgertimeSystem::new();
        assert_eq!(sys.board.p1, 0xFF);
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_P1_LEFT as u16),
            pressed: true,
        });
        assert_eq!(sys.board.p1 & 0x02, 0, "P1 Left clears IN0 bit 1");
    }

    fn press(sys: &mut BurgertimeSystem, id: u8, pressed: bool) {
        sys.handle_input(InputEvent::Button {
            id: InputId(id as u16),
            pressed,
        });
    }

    #[test]
    fn coin_irq_is_edge_triggered() {
        let mut sys = BurgertimeSystem::new();
        press(&mut sys, INPUT_COIN1, true);
        assert_ne!(sys.board.system & 0x40, 0, "coin sets system bit 6");
        assert!(sys.board.main_irq, "rising edge asserts IRQ");

        // Simulate the CPU acknowledging (vector fetch), then a redundant
        // press while still held: no new edge, so no re-assert.
        sys.board.main_irq = false;
        press(&mut sys, INPUT_COIN1, true);
        assert!(!sys.board.main_irq, "held coin does not re-assert");

        // Release then press: a fresh edge re-asserts.
        press(&mut sys, INPUT_COIN1, false);
        press(&mut sys, INPUT_COIN1, true);
        assert!(sys.board.main_irq, "new edge re-asserts IRQ");
    }

    #[test]
    fn tilt_and_coin2_map_to_system() {
        let mut sys = BurgertimeSystem::new();
        press(&mut sys, INPUT_TILT, true);
        assert_eq!(
            sys.board.system & 0x04,
            0,
            "tilt clears system bit 2 (active-low)"
        );
        press(&mut sys, INPUT_COIN2, true);
        assert_ne!(
            sys.board.system & 0x80,
            0,
            "coin2 sets system bit 7 (active-high)"
        );
        assert!(sys.board.main_irq, "coin2 edge asserts IRQ");
    }

    #[test]
    fn dip_banks_valid_with_defaults() {
        let sys = BurgertimeSystem::new();
        crate::assert_dip_banks_valid(
            sys.dip_banks(),
            &[sys.dip_bank_value(0), sys.dip_bank_value(1)],
        );
        assert_eq!(sys.dip_bank_value(0), 0x1F);
        assert_eq!(sys.dip_bank_value(1), 0x0B);
    }

    #[test]
    fn dip_set_option_preserves_other_bits() {
        let mut sys = BurgertimeSystem::new();
        // DSW2 Lives (bank 1, option 0): switch to "5" (0x00).
        sys.set_dip_option(1, 0, 0x00);
        assert_eq!(sys.board.dsw2 & 0x01, 0x00);
        // Bonus/enemies bits untouched.
        assert_eq!(sys.board.dsw2 & 0x0E, 0x0A);
    }
}
