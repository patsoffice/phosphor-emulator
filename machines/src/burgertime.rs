//! Burgertime (Data East, 1982) — the first game on the `btime.cpp` board.
//!
//! Thin wrapper around the shared [`BtimeBoard`] (see `btime.rs`) following the
//! Board Wrapper Pattern (`joust.rs` / `gridlee.rs`). This is the initial
//! scaffold (issue `burgertime-z6c.1`): it constructs the board, registers the
//! machine, and defines the ROM regions. Memory-map/encryption (`.2`), GFX +
//! palette (`.3`), the renderer (`.4`), inputs/DIPs/VBLANK (`.5`), and the
//! run-frame timing (`.6`) fill in the behavior.
//!
//! Pass 1 is video-first and silent; the sound M6502 + 2× AY-3-8910 are a
//! follow-up (`.8`).

use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    ActionRole, DipSwitches, Direction, InputConfigurable, InputControl, InputEvent, InputId,
    InputKind, MachineCore, SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_macros::Saveable;

use crate::btime::{self, BtimeBoard, BtimeConfig};
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::{set_bit_active_high, set_bit_active_low};

// ---------------------------------------------------------------------------
// ROM definitions (MAME driver `btime`, btime.cpp:2546-2570)
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

/// Sound-CPU program ROM (audiocpu at 0xE000). Defined now; wired with the
/// sound M6502 + 2× AY-3-8910 in the audio follow-up (`.8`).
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
// Input definitions (bit layout per the Burgertime plan §203-210; polarity and
// the coin-IRQ edge behavior are refined in `.5`).
// ---------------------------------------------------------------------------

const INPUT_P1_RIGHT: u8 = 0;
const INPUT_P1_LEFT: u8 = 1;
const INPUT_P1_UP: u8 = 2;
const INPUT_P1_DOWN: u8 = 3;
const INPUT_P1_BUTTON1: u8 = 4;
const INPUT_P2_RIGHT: u8 = 5;
const INPUT_P2_LEFT: u8 = 6;
const INPUT_P2_UP: u8 = 7;
const INPUT_P2_DOWN: u8 = 8;
const INPUT_P2_BUTTON1: u8 = 9;
const INPUT_COIN1: u8 = 10;
const INPUT_START1: u8 = 11;
const INPUT_START2: u8 = 12;

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
];

// ---------------------------------------------------------------------------
// BurgertimeSystem
// ---------------------------------------------------------------------------

/// Data East `btime.cpp` board configured for Burgertime (1982).
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

    /// Load and validate the Burgertime ROM set. The main program ROM is placed
    /// into the board; GFX/bg_map presence is validated here and decoded in
    /// `.3`. The sound ROM is validated too but not yet wired (`.8`).
    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let main = BURGERTIME_MAIN_ROM.load(rom_set)?;
        self.board.load_main_rom(&main);

        // Validate the graphics ROMs are present (decoded in `.3`).
        BURGERTIME_GFX1_ROM.load(rom_set)?;
        BURGERTIME_GFX2_ROM.load(rom_set)?;
        BURGERTIME_BG_MAP_ROM.load(rom_set)?;

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

crate::impl_board_delegation!(BurgertimeSystem, board, btime::TIMING, no_audio);

impl MachineCore for BurgertimeSystem {
    fn run_frame(&mut self) {
        // Timing detail (per-scanline VBLANK visibility, coin-IRQ hold) lands
        // in `.6`; pass 1 just runs a frame's worth of main-CPU cycles.
        bus_split!(self, bus => {
            for _ in 0..btime::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });
    }

    fn reset(&mut self) {
        self.board.reset();
        bus_split!(self, bus => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
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
            // Coin -> system, active-high; latch the coin IRQ (refined in `.5`).
            INPUT_COIN1 => {
                set_bit_active_high(&mut self.board.system, 6, pressed);
                self.board.main_irq = pressed;
            }
            _ => {}
        }
    }
}

// DIP banks (settable coinage/lives/bonus tables) land in `.5`.
impl DipSwitches for BurgertimeSystem {}

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
        assert_eq!(sys.display_size(), (240, 240));
    }

    #[test]
    fn input_controls_exposed() {
        let sys = BurgertimeSystem::new();
        let controls = sys.input_controls();
        assert_eq!(controls.len(), 13); // P1/P2 4-way+button, coin, 2 starts
        assert!(controls.iter().any(|c| c.stable_name == "coin1"));
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

    #[test]
    fn coin_latches_main_irq() {
        let mut sys = BurgertimeSystem::new();
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_COIN1 as u16),
            pressed: true,
        });
        assert_ne!(sys.board.system & 0x40, 0, "coin sets system bit 6");
        assert!(sys.board.main_irq);
    }
}
