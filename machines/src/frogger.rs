//! Frogger (Konami, 1981).
//!
//! A Scramble-family board: the shared [`crate::galaxian_video`] engine plus the
//! Frogger video extras (a half-screen blue color-split background, a tile/sprite
//! color-code rotation, and the column-scroll / sprite-Y nibble swap), a main Z80,
//! and the single-AY Frogger variant of the Konami sound board reached through an
//! 8255 PPI. A second 8255 routes the inputs. The hardware reuses
//! [`crate::scramble::ScrambleBoard`] with the [`Hw::Frogger`] memory map.
//!
//! Memory map (MAME `frogger_map`):
//! ```text
//!   0x0000-0x3fff  Program ROM (12 KB used)
//!   0x8000-0x87ff  Work RAM
//!   0x8800         watchdog (mirror 0x07ff)
//!   0xa800-0xabff  Video RAM (tiles, mirror at 0xac00)
//!   0xb000-0xb0ff  Object RAM (scroll/color, sprites)  (mirror to 0xb7ff)
//!   0xb808 NMI enable  0xb80c flip-y  0xb810 flip-x  0xb818/c coin counters
//!   0xc000-0xffff  8255 PPIs (A12 = sound PPI #1, A13 = input PPI #0)
//! ```
//!
//! Both the first sound ROM and the second GFX ROM ship with their D0/D1 data
//! lines swapped (MAME `decode_frogger_sound` / `decode_frogger_gfx`), undone at
//! load time.

use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    DipApplyTiming, DipChoice, DipOption, DipSwitchBank, DipSwitches, Direction, InputConfigurable,
    InputControl, InputEvent, InputId, InputKind, MachineCore, Nvram, Profilable, SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;

use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::scramble::{Hw, ScrambleBoard, TIMING};

// Input button IDs (Frogger is a 4-way joystick with no fire buttons).
const F_COIN: u8 = 0;
const F_LEFT: u8 = 1;
const F_RIGHT: u8 = 2;
const F_UP: u8 = 3;
const F_DOWN: u8 = 4;
const F_START1: u8 = 5;
const F_START2: u8 = 6;

// ---------------------------------------------------------------------------
// ROM definitions ("frogger" parent set)
// ---------------------------------------------------------------------------

/// Program ROM: 3×4 KB at 0x0000-0x2fff, padded to the 16 KB region.
pub static FROGGER_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "frogger.26",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x597696d6],
        },
        RomEntry {
            name: "frogger.27",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0xb6e6fcc3],
        },
        RomEntry {
            name: "frsm3.7",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0xaca22ae0],
        },
    ],
};

/// Sound ROM: 3×2 KB. The first ROM (0x0000-0x07ff) has D0/D1 swapped.
pub static FROGGER_SOUND_ROM: RomRegion = RomRegion {
    size: 0x1800,
    entries: &[
        RomEntry {
            name: "frogger.608",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0xe8ab0256],
        },
        RomEntry {
            name: "frogger.609",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x7380a48f],
        },
        RomEntry {
            name: "frogger.610",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0x31d7eb27],
        },
    ],
};

/// GFX ROM: 2×2 KB. The plane order is reversed relative to MAME's ROM region
/// to match the shared engine's bitplane convention (`frogger.606` is plane 0
/// here, the same way Super Cobra puts `5h` first); `frogger.606` also has its
/// D0/D1 data lines swapped on the board (`decode_frogger_gfx`).
pub static FROGGER_GFX_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[
        RomEntry {
            name: "frogger.606",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0xf524ee30],
        },
        RomEntry {
            name: "frogger.607",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x05f7d883],
        },
    ],
};

pub static FROGGER_COLOR_PROM: RomRegion = RomRegion {
    size: 0x0020,
    entries: &[RomEntry {
        name: "pr-91.6l",
        size: 0x0020,
        offset: 0x0000,
        crc32: &[0x413703bf],
    }],
};

/// Swap data lines D0 and D1 of every byte in `data[range]` (MAME's
/// `bitswap<8>(b, 7,6,5,4,3,2,0,1)`).
fn swap_d0_d1(data: &mut [u8], range: std::ops::Range<usize>) {
    for b in &mut data[range] {
        *b = (*b & !0x03) | ((*b & 0x01) << 1) | ((*b & 0x02) >> 1);
    }
}

// ---------------------------------------------------------------------------
// DIP switches (MAME `frogger`): on the IN1/IN2 input ports.
// ---------------------------------------------------------------------------

const FR_DIP1_MASK: u8 = 0x03; // IN1: Lives
const FR_DIP2_MASK: u8 = 0x0e; // IN2: Coinage (0x06) + Cabinet (0x08)

pub(crate) const FROGGER_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "IN1",
        options: &[DipOption {
            name: "Lives",
            mask: 0x03,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "3",
                    value: 0x00,
                },
                DipChoice {
                    label: "5",
                    value: 0x01,
                },
                DipChoice {
                    label: "7",
                    value: 0x02,
                },
                DipChoice {
                    label: "256 (Cheat)",
                    value: 0x03,
                },
            ],
        }],
    },
    DipSwitchBank {
        name: "IN2",
        options: &[
            DipOption {
                name: "Coinage",
                mask: 0x06,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "A 1/1 B 1/1 C 1/1",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "A 2/1 B 2/1 C 2/1",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "A 2/1 B 1/3 C 2/1",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "A 1/1 B 1/6 C 1/1",
                        value: 0x06,
                    },
                ],
            },
            DipOption {
                name: "Cabinet",
                mask: 0x08,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Upright",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Cocktail",
                        value: 0x08,
                    },
                ],
            },
        ],
    },
];

pub const FROGGER_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(F_LEFT as u16),
        stable_name: "p1_left",
        label: "P1 Left",
        kind: InputKind::DigitalDirection {
            direction: Direction::Left,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(F_RIGHT as u16),
        stable_name: "p1_right",
        label: "P1 Right",
        kind: InputKind::DigitalDirection {
            direction: Direction::Right,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
    InputControl {
        id: InputId(F_UP as u16),
        stable_name: "p1_up",
        label: "P1 Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_UP,
    },
    InputControl {
        id: InputId(F_DOWN as u16),
        stable_name: "p1_down",
        label: "P1 Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_DOWN,
    },
    InputControl {
        id: InputId(F_COIN as u16),
        stable_name: "coin",
        label: "Coin",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(F_START1 as u16),
        stable_name: "p1_start",
        label: "P1 Start",
        kind: InputKind::Start,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_START,
    },
    InputControl {
        id: InputId(F_START2 as u16),
        stable_name: "p2_start",
        label: "P2 Start",
        kind: InputKind::Start,
        player: Some(2),
        default_bindings: crate::input_defaults::P2_START,
    },
];

// ---------------------------------------------------------------------------
// FroggerSystem wrapper
// ---------------------------------------------------------------------------

/// Frogger (Konami, 1981).
#[derive(phosphor_macros::Saveable)]
pub struct FroggerSystem {
    pub board: ScrambleBoard,
}

impl FroggerSystem {
    pub fn new() -> Self {
        let mut board = ScrambleBoard::new(Hw::Frogger);
        // Factory-default DIPs (active-low: clear the configured bits → 3 lives,
        // 1C/1C, upright).
        board.in1 &= !FR_DIP1_MASK;
        board.in2 &= !FR_DIP2_MASK;
        Self { board }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        self.board
            .load_program_rom(&FROGGER_PROGRAM_ROM.load(rom_set)?);

        // The first sound ROM has D0/D1 swapped on the board.
        let mut sound = FROGGER_SOUND_ROM.load(rom_set)?;
        swap_d0_d1(&mut sound, 0x0000..0x0800);
        self.board.load_sound_rom(&sound);

        // frogger.606 (now plane 0, at 0x0000-0x07ff) has D0/D1 swapped.
        let mut gfx = FROGGER_GFX_ROM.load(rom_set)?;
        swap_d0_d1(&mut gfx, 0x0000..0x0800);
        self.board.load_gfx_rom(&gfx);

        self.board
            .load_color_prom(&FROGGER_COLOR_PROM.load(rom_set)?);
        Ok(())
    }

    /// Frogger input bit mapping (active-low; pressing clears the bit). The
    /// upright player's joystick spans IN0 (left/right) and IN2 (up/down).
    fn apply_input(&mut self, button: u8, pressed: bool) {
        let b = &mut self.board;
        match button {
            F_COIN => crate::set_bit_active_low(&mut b.in0, 7, pressed), // coin1
            F_RIGHT => crate::set_bit_active_low(&mut b.in0, 4, pressed),
            F_LEFT => crate::set_bit_active_low(&mut b.in0, 5, pressed),
            F_UP => crate::set_bit_active_low(&mut b.in2, 4, pressed),
            F_DOWN => crate::set_bit_active_low(&mut b.in2, 6, pressed),
            F_START1 => crate::set_bit_active_low(&mut b.in1, 7, pressed),
            F_START2 => crate::set_bit_active_low(&mut b.in1, 6, pressed),
            _ => {}
        }
    }
}

impl Default for FroggerSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for FroggerSystem {
    type Address = u16;
    type Data = u8;
    fn read(&mut self, _m: BusMaster, addr: u16) -> u8 {
        self.board.bus_read_common(addr)
    }
    fn write(&mut self, _m: BusMaster, addr: u16, data: u8) {
        self.board.bus_write_common(addr, data);
    }
    fn io_read(&mut self, _m: BusMaster, _addr: u16) -> u8 {
        0xFF
    }
    fn io_write(&mut self, _m: BusMaster, _addr: u16, _data: u8) {}
    fn is_halted_for(&self, _m: BusMaster) -> bool {
        false
    }
    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        self.board.check_interrupts(target)
    }
}

crate::impl_board_delegation!(FroggerSystem, board, TIMING, orientation);

impl MachineCore for FroggerSystem {
    crate::machine_core_metadata!("frogger", TIMING);

    fn gfx_sheets(&self) -> Vec<phosphor_core::core::machine::GfxSheet<'_>> {
        use phosphor_core::core::machine::GfxSheet;
        let v = &self.board.video;
        vec![
            GfxSheet {
                name: "chars",
                cache: v.tile_cache(),
                palette: v.palette_rgb(),
            },
            GfxSheet {
                name: "sprites",
                cache: v.sprite_cache(),
                palette: v.palette_rgb(),
            },
        ]
    }

    fn run_frame(&mut self) {
        bus_split!(self, bus => {
            for _ in 0..TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });
    }

    fn reset(&mut self) {
        self.board.reset_board();
        bus_split!(self, bus => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
        });
    }
}

impl SaveState for FroggerSystem {
    crate::machine_save_state!();
}

impl Nvram for FroggerSystem {}
impl Profilable for FroggerSystem {}

impl InputConfigurable for FroggerSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        FROGGER_CONTROLS
    }
    fn handle_input(&mut self, event: InputEvent) {
        if let InputEvent::Button { id, pressed } = event {
            self.apply_input(id.0 as u8, pressed);
        }
    }
}

impl DipSwitches for FroggerSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        FROGGER_DIP_BANKS
    }
    fn dip_bank_value(&self, bank: usize) -> u8 {
        match bank {
            0 => self.board.in1 & FR_DIP1_MASK,
            1 => self.board.in2 & FR_DIP2_MASK,
            _ => 0,
        }
    }
    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        match bank {
            0 => self.board.in1 = (self.board.in1 & !FR_DIP1_MASK) | (value & FR_DIP1_MASK),
            1 => self.board.in2 = (self.board.in2 & !FR_DIP2_MASK) | (value & FR_DIP2_MASK),
            _ => {}
        }
    }
}

crate::impl_board_debug_trace!(FroggerSystem, board);

crate::register_machine!(FroggerSystem, "frogger", &["frogger"], FROGGER_CONTROLS);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_id_and_defaults() {
        let sys = FroggerSystem::new();
        assert_eq!(sys.machine_id(), "frogger");
        assert_eq!(sys.board.hw, Hw::Frogger);
        // Default DIPs (active-low): 3 lives, 1C/1C, upright → all bits clear.
        assert_eq!(sys.board.in1 & FR_DIP1_MASK, 0x00);
        assert_eq!(sys.board.in2 & FR_DIP2_MASK, 0x00);
    }

    #[test]
    fn active_low_joystick_spans_in0_and_in2() {
        let mut sys = FroggerSystem::new();
        sys.apply_input(F_LEFT, true);
        assert_eq!(sys.board.in0 & 0x20, 0x00, "left clears IN0 bit 5");
        sys.apply_input(F_UP, true);
        assert_eq!(sys.board.in2 & 0x10, 0x00, "up clears IN2 bit 4");
        sys.apply_input(F_LEFT, false);
        assert_eq!(sys.board.in0 & 0x20, 0x20, "release restores IN0 bit 5");
    }

    #[test]
    fn frogger_ram_and_vram_round_trip() {
        let mut b = ScrambleBoard::new(Hw::Frogger);
        b.bus_write_common(0x8000, 0xab); // work RAM
        assert_eq!(b.bus_read_common(0x8000), 0xab);
        b.bus_write_common(0xa800, 0x42); // video RAM
        assert_eq!(b.bus_read_common(0xa800), 0x42);
        assert_eq!(b.bus_read_common(0xac00), 0x42, "VRAM mirror at 0xac00");
        b.bus_write_common(0xb000, 0x17); // object RAM
        assert_eq!(b.bus_read_common(0xb000), 0x17);
        assert_eq!(b.bus_read_common(0xb100), 0x17, "objram mirror");
    }

    #[test]
    fn dip_round_trip() {
        let mut sys = FroggerSystem::new();
        sys.set_dip_bank_value(0, 0x02); // 7 lives
        assert_eq!(sys.dip_bank_value(0), 0x02);
        sys.set_dip_bank_value(1, 0x08); // cocktail
        assert_eq!(sys.dip_bank_value(1), 0x08);
    }

    #[test]
    fn swap_d0_d1_swaps_low_two_bits() {
        let mut data = [0b0000_0001u8, 0b0000_0010, 0b1010_1011];
        swap_d0_d1(&mut data, 0..3);
        assert_eq!(data[0], 0b0000_0010);
        assert_eq!(data[1], 0b0000_0001);
        assert_eq!(data[2], 0b1010_1011); // low bits 11 unchanged
    }
}
