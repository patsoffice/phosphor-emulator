//! Moon Cresta (Nichibutsu, 1980) — a base-Galaxian follow-on.
//!
//! Same [`GalaxianBoard`] hardware as Galaxian (Z80 @ 3.072 MHz, tilemap +
//! sprites + starfield + discrete sound). The differences are all per-game
//! quirks: a larger 16 KB program, an 8 KB two-bank GFX ROM selected by the
//! GFX-bank latch ([`GfxBankMode::Mooncrst`]), its own color PROM and DIP
//! layout, and an XOR/bitswap encryption on the program ROM
//! ([`decode_mooncrst`], MAME's `init_mooncrst`).

use phosphor_core::core::machine::{
    DipApplyTiming, DipChoice, DipOption, DipSwitchBank, InputConfigurable, InputControl,
    InputEvent, MachineCore, Nvram, Profilable, SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;

use crate::galaxian::{GALAXIAN_CONTROLS, GalaxianBoard, TIMING};
use crate::galaxian_video::GfxBankMode;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};

// ---------------------------------------------------------------------------
// ROM definitions (canonical Nichibutsu "mooncrst" set, encrypted)
// ---------------------------------------------------------------------------

/// Program ROM: 16 KB (eight 2 KB chips) at 0x0000-0x3fff, encrypted (decoded
/// by [`decode_mooncrst`] at load).
pub static MOONCRST_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "mc1",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0x7d954a7a],
        },
        RomEntry {
            name: "mc2",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x44bb7cfa],
        },
        RomEntry {
            name: "mc3",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0x9c412104],
        },
        RomEntry {
            name: "mc4",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0x7e9b1ab5],
        },
        RomEntry {
            name: "mc5.7r",
            size: 0x0800,
            offset: 0x2000,
            crc32: &[0x16c759af],
        },
        RomEntry {
            name: "mc6.8d",
            size: 0x0800,
            offset: 0x2800,
            crc32: &[0x69bcafdb],
        },
        RomEntry {
            name: "mc7.8e",
            size: 0x0800,
            offset: 0x3000,
            crc32: &[0xb50dbc46],
        },
        RomEntry {
            name: "mc8",
            size: 0x0800,
            offset: 0x3800,
            crc32: &[0x18ca312b],
        },
    ],
};

/// GFX ROM: 8 KB (two 4 KB banks); the first half is plane 0, the second
/// plane 1. The bank latch selects between the two 256-tile halves.
pub static MOONCRST_GFX_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "mcs_b",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0xfb0f1f81],
        },
        RomEntry {
            name: "mcs_d",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x13932a15],
        },
        RomEntry {
            name: "mcs_a",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0x631ebb5a],
        },
        RomEntry {
            name: "mcs_c",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0x24cfd145],
        },
    ],
};

/// Palette PROM (32 bytes), shared with Galaxian-family boards.
pub static MOONCRST_COLOR_PROM: RomRegion = RomRegion {
    size: 0x0020,
    entries: &[RomEntry {
        name: "mmi6331.6l",
        size: 0x0020,
        offset: 0x0000,
        crc32: &[0x6a0c7d87],
    }],
};

/// Decrypt the Moon Cresta program ROM in place (MAME `decode_mooncrst`): two
/// data-bit-conditional XORs, then a bit 2↔6 swap on even addresses. Applied to
/// both opcodes and data (the real board decodes on the bus).
fn decode_mooncrst(rom: &mut [u8]) {
    for (offs, byte) in rom.iter_mut().enumerate() {
        let data = *byte;
        let mut res = data;
        if data & 0x02 != 0 {
            res ^= 0x40;
        }
        if data & 0x20 != 0 {
            res ^= 0x04;
        }
        if offs & 1 == 0 {
            // bitswap<8>(res, 7,2,5,4,3,6,1,0) — swap bits 2 and 6.
            let b2 = (res >> 2) & 1;
            let b6 = (res >> 6) & 1;
            res = (res & !0x44) | (b6 << 2) | (b2 << 6);
        }
        *byte = res;
    }
}

// ---------------------------------------------------------------------------
// DIP switches (MAME `mooncrst` input ports)
// ---------------------------------------------------------------------------
//
// Like Galaxian, Moon Cresta's DIPs share the IN0/IN1/IN2 input ports; the
// masks keep DIP writes from disturbing the live input bits.

const MC_DIP0_MASK: u8 = 0x20; // IN0: Cabinet
const MC_DIP1_MASK: u8 = 0xc0; // IN1: Bonus Life + Language
const MC_DIP2_MASK: u8 = 0x0f; // IN2: Coin A + Coin B
/// Factory defaults: Upright, 30000 bonus, English, 1C/1C both slots.
const MC_DIP1_DEFAULT: u8 = 0x80; // Language = English

pub(crate) const MOONCRST_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "IN0",
        options: &[DipOption {
            name: "Cabinet",
            mask: MC_DIP0_MASK,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "Upright",
                    value: 0x00,
                },
                DipChoice {
                    label: "Cocktail",
                    value: 0x20,
                },
            ],
        }],
    },
    DipSwitchBank {
        name: "IN1",
        options: &[
            DipOption {
                name: "Bonus Life",
                mask: 0x40,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "30000",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "50000",
                        value: 0x40,
                    },
                ],
            },
            DipOption {
                name: "Language",
                mask: 0x80,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "English",
                        value: 0x80,
                    },
                    DipChoice {
                        label: "Japanese",
                        value: 0x00,
                    },
                ],
            },
        ],
    },
    DipSwitchBank {
        name: "IN2",
        options: &[
            DipOption {
                name: "Coin A",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "1 Coin/1 Credit",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "2 Coins/1 Credit",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "3 Coins/1 Credit",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "4 Coins/1 Credit",
                        value: 0x03,
                    },
                ],
            },
            DipOption {
                name: "Coin B",
                mask: 0x0c,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "1 Coin/1 Credit",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "1 Coin/2 Credits",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "1 Coin/3 Credits",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "Free Play",
                        value: 0x0c,
                    },
                ],
            },
        ],
    },
];

// ---------------------------------------------------------------------------
// MoonCrestaSystem — Moon Cresta wrapped around GalaxianBoard
// ---------------------------------------------------------------------------

/// Moon Cresta (Nichibutsu, 1980): base Galaxian board with GFX banking and an
/// encrypted program ROM.
#[derive(phosphor_macros::Saveable, phosphor_macros::BusDebug)]
pub struct MoonCrestaSystem {
    /// The Z80 is held beside the board, which is its bus.
    #[debug_cpu("Z80")]
    pub cpu: phosphor_core::cpu::z80::Z80,

    #[debug_bus]
    pub board: GalaxianBoard,
}

impl MoonCrestaSystem {
    pub fn new() -> Self {
        let mut board = GalaxianBoard::new();
        board.set_gfx_mode(GfxBankMode::Mooncrst);
        board.set_mooncrst_map(true);
        board.in1 = MC_DIP1_DEFAULT;
        Self {
            cpu: phosphor_core::cpu::z80::Z80::new(),
            board,
        }
    }

    /// One CPU cycle. Returns 1 at an instruction boundary (for the debugger,
    /// which steps instructions rather than cycles).
    pub fn step_cycle(&mut self) -> u32 {
        crate::galaxian::tick(&mut self.cpu, &mut self.board);
        GalaxianBoard::instruction_boundaries(&self.cpu)
    }

    /// Read the CPU-facing bus, side effects and all. Distinct from the
    /// debugger's `BusDebug::peek`/`poke`, which avoid side effects.
    pub fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        Bus::read(&mut self.board, master, addr)
    }

    /// Write the CPU-facing bus, side effects and all. See [`Self::bus_read`].
    pub fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        Bus::write(&mut self.board, master, addr, data);
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let mut program = MOONCRST_PROGRAM_ROM.load(rom_set)?;
        decode_mooncrst(&mut program);
        self.board.load_program_rom(&program);
        self.board.load_gfx_rom(&MOONCRST_GFX_ROM.load(rom_set)?);
        self.board
            .load_color_prom(&MOONCRST_COLOR_PROM.load(rom_set)?);
        Ok(())
    }
}

impl Default for MoonCrestaSystem {
    fn default() -> Self {
        Self::new()
    }
}

// The board is the bus -- see `impl Bus for GalaxianBoard` in galaxian.rs.

crate::impl_board_delegation!(MoonCrestaSystem, board, TIMING, orientation);

impl MachineCore for MoonCrestaSystem {
    crate::machine_core_metadata!("mooncrst", TIMING);

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
        crate::galaxian::run_frame(&mut self.cpu, &mut self.board);
    }

    fn reset(&mut self) {
        self.board.reset_board();
        self.cpu.reset(&mut self.board, BusMaster::Cpu(0));
    }
}

impl SaveState for MoonCrestaSystem {
    crate::machine_save_state!();
}

impl Nvram for MoonCrestaSystem {}
impl Profilable for MoonCrestaSystem {}

impl InputConfigurable for MoonCrestaSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        GALAXIAN_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        if let InputEvent::Button { id, pressed } = event {
            self.board.handle_input(id.0 as u8, pressed);
        }
    }
}

crate::impl_dip_switches!(
    MoonCrestaSystem,
    MOONCRST_DIP_BANKS,
    board.in0 & MC_DIP0_MASK,
    board.in1 & MC_DIP1_MASK,
    board.in2 & MC_DIP2_MASK
);

crate::impl_board_debug_trace!(MoonCrestaSystem, board);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

crate::register_machine!(
    MoonCrestaSystem,
    "mooncrst",
    &["mooncrst"],
    GALAXIAN_CONTROLS
);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::DipSwitches;
    use phosphor_core::core::save_state::{Saveable, StateReader, StateWriter};

    #[test]
    fn machine_id_and_gfx_mode() {
        let sys = MoonCrestaSystem::new();
        assert_eq!(sys.machine_id(), "mooncrst");
        // Language DIP defaults to English (IN1 bit 7).
        assert_eq!(sys.board.in1 & MC_DIP1_MASK, MC_DIP1_DEFAULT);
    }

    #[test]
    fn decode_mooncrst_matches_known_vectors() {
        // Hand-computed against MAME's decode_mooncrst:
        //   0x02 @ even: bit1 set → ^0x40 = 0x42; swap bits 2,6 of 0x42 → 0x06.
        //   0x02 @ odd:  ^0x40 = 0x42; no swap.
        //   0x20 @ even: bit5 set → ^0x04 = 0x24; swap bits 2,6 of 0x24 → 0x60.
        //   0x00 anywhere: unchanged.
        let mut d = [0x02u8, 0x02, 0x20, 0x00];
        decode_mooncrst(&mut d);
        assert_eq!(d, [0x06, 0x42, 0x60, 0x00]);
    }

    #[test]
    fn mooncrst_map_shifts_ram_vram_and_io() {
        let mut sys = MoonCrestaSystem::new();
        // Work RAM at 0x8000 (Galaxian 0x4000) round-trips.
        sys.board.bus_write_common(0x8000, 0xab);
        assert_eq!(sys.board.bus_read_common(0x8000), 0xab);
        // Video RAM at 0x9000 (Galaxian 0x5000) round-trips.
        sys.board.bus_write_common(0x9123, 0x42);
        assert_eq!(sys.board.bus_read_common(0x9123), 0x42);
        // IN0 is read at 0xa000 (Galaxian 0x6000).
        sys.board.handle_input(crate::galaxian::INPUT_COIN, true);
        assert_eq!(sys.board.bus_read_common(0xa000) & 0x01, 0x01);
        // The 0xa000 block drives the GFX-bank latch (line 2 → gfxbank[2]).
        sys.board.bus_write_common(0xa002, 0x01);
        // IRQ enable is line 0 of the 0xb000 block on this map.
        sys.board.bus_write_common(0xb000, 0x01);
        assert!(sys.board.irq_enabled);
    }

    #[test]
    fn dip_round_trips_per_bank() {
        let mut sys = MoonCrestaSystem::new();
        sys.set_dip_bank_value(0, 0x20); // Cocktail
        sys.set_dip_bank_value(1, 0x40); // 50000 bonus, Japanese (bit7=0)
        sys.set_dip_bank_value(2, 0x0c); // Free Play coin A
        assert_eq!(sys.dip_bank_value(0), 0x20);
        assert_eq!(sys.dip_bank_value(1), 0x40);
        assert_eq!(sys.dip_bank_value(2), 0x0c);
    }

    #[test]
    fn dip_writes_preserve_input_bits() {
        let mut sys = MoonCrestaSystem::new();
        sys.board.handle_input(crate::galaxian::INPUT_COIN, true); // IN0 bit0
        sys.set_dip_bank_value(0, 0x20); // change Cabinet on IN0
        assert_eq!(sys.board.in0 & 0x01, 0x01, "coin bit survives DIP write");
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = MoonCrestaSystem::new();
        sys.board.in0 = 0x12;
        let mut w = StateWriter::new();
        Saveable::save_state(&sys, &mut w);
        let bytes = w.into_vec();

        let mut sys2 = MoonCrestaSystem::new();
        let mut r = StateReader::new(&bytes);
        Saveable::load_state(&mut sys2, &mut r).unwrap();
        assert_eq!(sys2.board.in0, 0x12);
    }
}
