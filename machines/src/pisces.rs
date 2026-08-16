//! Pisces (Subelectro) and UniWar S (Irem) — base-Galaxian follow-ons.
//!
//! Both run on the stock [`GalaxianBoard`] with the Pisces GFX-bank scheme
//! ([`GfxBankMode::Pisces`]: `gfxbank[0]` is the high tile/sprite code bit,
//! doubling the GFX). They differ only in ROM maps and DIP layout, so a single
//! config-parameterised wrapper covers both (MAME's shared `pisces` machine /
//! `init_pisces`). No program encryption.

use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    DipApplyTiming, DipChoice, DipOption, DipSwitchBank, DipSwitches, InputConfigurable,
    InputControl, InputEvent, MachineCore, Nvram, Profilable, SaveState,
};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;

use crate::galaxian::{GALAXIAN_CONTROLS, GalaxianBoard, TIMING};
use crate::galaxian_video::GfxBankMode;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};

// ---------------------------------------------------------------------------
// ROM definitions
// ---------------------------------------------------------------------------

// Padded to the full 0x4000 program region (Pisces only populates 0x0000-0x2fff;
// the board's ROM region is 16 KB and load_region requires an exact-size fill).
static PISCES_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "p1.bin",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0x40c5b0e4],
        },
        RomEntry {
            name: "p2.bin",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x055f9762],
        },
        RomEntry {
            name: "p3.bin",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0x3073dd04],
        },
        RomEntry {
            name: "p4.bin",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0x44aaf525],
        },
        RomEntry {
            name: "p5.bin",
            size: 0x0800,
            offset: 0x2000,
            crc32: &[0xfade512b],
        },
        RomEntry {
            name: "p6.bin",
            size: 0x0800,
            offset: 0x2800,
            crc32: &[0x5ab2822f],
        },
    ],
};

static PISCES_GFX_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "g09.bin",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0x9503a23a],
        },
        RomEntry {
            name: "g11.bin",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x0adfc3fe],
        },
        RomEntry {
            name: "g10.bin",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0x3e61f849],
        },
        RomEntry {
            name: "g12.bin",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0x7130e9eb],
        },
    ],
};

static PISCES_COLOR_PROM: RomRegion = RomRegion {
    size: 0x0020,
    entries: &[RomEntry {
        name: "colour.bin",
        size: 0x0020,
        offset: 0x0000,
        crc32: &[0x57a45057],
    }],
};

static UNIWARS_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "f07_1a.bin",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0xd975af10],
        },
        RomEntry {
            name: "h07_2a.bin",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0xb2ed14c3],
        },
        RomEntry {
            name: "k07_3a.bin",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0x945f4160],
        },
        RomEntry {
            name: "m07_4a.bin",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0xddc80bc5],
        },
        RomEntry {
            name: "d08p_5a.bin",
            size: 0x0800,
            offset: 0x2000,
            crc32: &[0x62354351],
        },
        RomEntry {
            name: "gg6",
            size: 0x0800,
            offset: 0x2800,
            crc32: &[0x270a3f4d],
        },
        RomEntry {
            name: "m08p_7a.bin",
            size: 0x0800,
            offset: 0x3000,
            crc32: &[0xc9245346],
        },
        RomEntry {
            name: "n08p_8a.bin",
            size: 0x0800,
            offset: 0x3800,
            crc32: &[0x797d45c7],
        },
    ],
};

static UNIWARS_GFX_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "egg10",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0x012941e0],
        },
        RomEntry {
            name: "h01_2.bin",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0xc26132af],
        },
        RomEntry {
            name: "egg9",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0xfc8b58fd],
        },
        RomEntry {
            name: "k01_2.bin",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0xdcc2b33b],
        },
    ],
};

static UNIWARS_COLOR_PROM: RomRegion = RomRegion {
    size: 0x0020,
    entries: &[RomEntry {
        name: "uniwars.clr",
        size: 0x0020,
        offset: 0x0000,
        crc32: &[0x25c79518],
    }],
};

// ---------------------------------------------------------------------------
// DIP switches
// ---------------------------------------------------------------------------

// Pisces (MAME `pisces`): coin bits swapped on IN0 (no IN0 DIP); Lives/Cabinet
// on IN1; Bonus/Coinage/Difficulty on IN2.
const PISCES_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "IN1",
        options: &[
            DipOption {
                name: "Lives",
                mask: 0x40,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "3",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "4",
                        value: 0x40,
                    },
                ],
            },
            DipOption {
                name: "Cabinet",
                mask: 0x80,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Upright",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Cocktail",
                        value: 0x80,
                    },
                ],
            },
        ],
    },
    DipSwitchBank {
        name: "IN2",
        options: &[
            DipOption {
                name: "Bonus Life",
                mask: 0x01,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "10000",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "20000",
                        value: 0x01,
                    },
                ],
            },
            DipOption {
                name: "Coinage",
                mask: 0x02,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "A 1C/1C  B 1C/6C",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "A 2C/1C  B 1C/3C",
                        value: 0x02,
                    },
                ],
            },
            DipOption {
                name: "Difficulty",
                mask: 0x04,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Easy",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Hard",
                        value: 0x04,
                    },
                ],
            },
        ],
    },
];

// UniWar S (MAME `superg`): base-Galaxian layout — Cabinet on IN0, Coinage on
// IN1, Bonus Life + Lives on IN2.
const UNIWARS_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "IN0",
        options: &[DipOption {
            name: "Cabinet",
            mask: 0x20,
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
        options: &[DipOption {
            name: "Coinage",
            mask: 0xc0,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "1 Coin/1 Credit",
                    value: 0x00,
                },
                DipChoice {
                    label: "2 Coins/1 Credit",
                    value: 0x40,
                },
                DipChoice {
                    label: "1 Coin/2 Credits",
                    value: 0x80,
                },
                DipChoice {
                    label: "Free Play",
                    value: 0xc0,
                },
            ],
        }],
    },
    DipSwitchBank {
        name: "IN2",
        options: &[
            DipOption {
                name: "Bonus Life",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "None",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "4000",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "5000",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "7000",
                        value: 0x03,
                    },
                ],
            },
            DipOption {
                name: "Lives",
                mask: 0x04,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "3",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "5",
                        value: 0x04,
                    },
                ],
            },
        ],
    },
];

// ---------------------------------------------------------------------------
// Per-game configuration
// ---------------------------------------------------------------------------

/// Static description of one Pisces-family game.
pub struct PiscesGame {
    id: &'static str,
    program: &'static RomRegion,
    gfx: &'static RomRegion,
    prom: &'static RomRegion,
    dip_banks: &'static [DipSwitchBank],
    /// Per DIP bank: which input port (0=IN0, 1=IN1, 2=IN2) and its DIP mask.
    dip_ports: &'static [(u8, u8)],
    /// Power-on IN0/IN1/IN2 values (DIP defaults; input bits are active-high 0).
    port_defaults: [u8; 3],
}

static PISCES: PiscesGame = PiscesGame {
    id: "pisces",
    program: &PISCES_PROGRAM_ROM,
    gfx: &PISCES_GFX_ROM,
    prom: &PISCES_COLOR_PROM,
    dip_banks: PISCES_DIP_BANKS,
    dip_ports: &[(1, 0xc0), (2, 0x07)],
    port_defaults: [0x00, 0x00, 0x00],
};

static UNIWARS: PiscesGame = PiscesGame {
    id: "uniwars",
    program: &UNIWARS_PROGRAM_ROM,
    gfx: &UNIWARS_GFX_ROM,
    prom: &UNIWARS_COLOR_PROM,
    dip_banks: UNIWARS_DIP_BANKS,
    dip_ports: &[(0, 0x20), (1, 0xc0), (2, 0x07)],
    port_defaults: [0x00, 0x00, 0x01], // 4000 bonus default
};

// ---------------------------------------------------------------------------
// PiscesSystem — config-parameterised wrapper around GalaxianBoard
// ---------------------------------------------------------------------------

/// A Pisces-family game (Pisces or UniWar S) on the Galaxian board.
#[derive(phosphor_macros::BusDebug)]
pub struct PiscesSystem {
    /// The Z80 is held beside the bus view over the board.
    #[debug_cpu("Z80")]
    pub cpu: phosphor_core::cpu::z80::Z80,

    #[debug_bus]
    pub board: GalaxianBoard,
    cfg: &'static PiscesGame,
}

impl PiscesSystem {
    fn new(cfg: &'static PiscesGame) -> Self {
        let mut board = GalaxianBoard::new();
        board.set_gfx_mode(GfxBankMode::Pisces);
        board.in0 = cfg.port_defaults[0];
        board.in1 = cfg.port_defaults[1];
        board.in2 = cfg.port_defaults[2];
        Self {
            cpu: phosphor_core::cpu::z80::Z80::new(),
            board,
            cfg,
        }
    }

    fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        self.board
            .load_program_rom(&self.cfg.program.load(rom_set)?);
        self.board.load_gfx_rom(&self.cfg.gfx.load(rom_set)?);
        self.board.load_color_prom(&self.cfg.prom.load(rom_set)?);
        Ok(())
    }

    /// Borrow the CPU and the bus view over the board as disjoint pieces.
    #[inline]
    fn split(&mut self) -> (&mut phosphor_core::cpu::z80::Z80, PiscesBus<'_>) {
        (&mut self.cpu, PiscesBus(&mut self.board))
    }

    /// One CPU cycle. Returns 1 at an instruction boundary (for the debugger,
    /// which steps instructions rather than cycles).
    pub fn step_cycle(&mut self) -> u32 {
        let (cpu, mut bus) = self.split();
        crate::galaxian::tick(cpu, &mut bus);
        GalaxianBoard::instruction_boundaries(&self.cpu)
    }

    /// Read the CPU-facing bus, side effects and all. Distinct from the
    /// debugger's `BusDebug::peek`/`poke`, which avoid side effects.
    pub fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        self.split().1.read(master, addr)
    }

    /// Write the CPU-facing bus, side effects and all. See [`Self::bus_read`].
    pub fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.split().1.write(master, addr, data);
    }

    fn port(&self, idx: u8) -> u8 {
        match idx {
            0 => self.board.in0,
            1 => self.board.in1,
            _ => self.board.in2,
        }
    }

    fn set_port_masked(&mut self, idx: u8, mask: u8, value: u8) {
        let port = match idx {
            0 => &mut self.board.in0,
            1 => &mut self.board.in1,
            _ => &mut self.board.in2,
        };
        *port = (*port & !mask) | (value & mask);
    }
}

/// The Pisces bus: the Galaxian board with one write line re-wired.
///
/// A newtype rather than using the board directly, because the 0x6002 line
/// differs — keeping the test here rather than in `bus_write_common` means the
/// other Galaxian games don't pay for it on every write.
struct PiscesBus<'a>(&'a mut GalaxianBoard);

impl crate::galaxian::GalaxianBus for PiscesBus<'_> {
    #[inline]
    fn board(&mut self) -> &mut GalaxianBoard {
        self.0
    }
}

impl Bus for PiscesBus<'_> {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        self.0.bus_read_common(addr)
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        // pisces_map: the 0x6002 coin-lockout line is replaced by the GFX-bank
        // bit (gfxbank[0], mirror 0x07f8). Everything else is the Galaxian map.
        if (0x6000..=0x67ff).contains(&addr) && addr & 7 == 2 {
            self.0.set_gfxbank(0, data);
        }
        self.0.bus_write_common(addr, data);
    }

    fn io_read(&mut self, _master: BusMaster, _addr: u16) -> u8 {
        0xFF
    }

    fn io_write(&mut self, _master: BusMaster, _addr: u16, _data: u8) {}

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        self.0.interrupt_state(target)
    }
}

crate::impl_board_delegation!(PiscesSystem, board, TIMING, orientation, split_cpu);

impl MachineCore for PiscesSystem {
    // Hand-written (not machine_core_metadata!) because the id is per-instance
    // config, which macro hygiene can't thread through `self`.
    fn frame_rate_hz(&self) -> f64 {
        TIMING.frame_rate_hz()
    }

    fn machine_id(&self) -> &str {
        self.cfg.id
    }

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

impl Saveable for PiscesSystem {
    fn save_state(&self, w: &mut StateWriter) {
        self.board.save_state(w);
    }
    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.board.load_state(r)
    }
}

impl SaveState for PiscesSystem {
    crate::machine_save_state!();
}

impl Nvram for PiscesSystem {}
impl Profilable for PiscesSystem {}

impl InputConfigurable for PiscesSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        GALAXIAN_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        if let InputEvent::Button { id, pressed } = event {
            self.board.handle_input(id.0 as u8, pressed);
        }
    }
}

impl DipSwitches for PiscesSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        self.cfg.dip_banks
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        match self.cfg.dip_ports.get(bank) {
            Some(&(port, mask)) => self.port(port) & mask,
            None => 0,
        }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        if let Some(&(port, mask)) = self.cfg.dip_ports.get(bank) {
            self.set_port_masked(port, mask, value);
        }
    }
}

crate::impl_board_debug_trace!(PiscesSystem, board);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

crate::register_machine!(
    new = PiscesSystem::new(&PISCES),
    "pisces",
    &["pisces"],
    GALAXIAN_CONTROLS
);
crate::register_machine!(
    new = PiscesSystem::new(&UNIWARS),
    "uniwars",
    &["uniwars"],
    GALAXIAN_CONTROLS
);

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_ids() {
        assert_eq!(PiscesSystem::new(&PISCES).machine_id(), "pisces");
        assert_eq!(PiscesSystem::new(&UNIWARS).machine_id(), "uniwars");
    }

    #[test]
    fn uniwars_bonus_default_is_4000() {
        // superg layout defaults IN2 to 0x01 (4000-point bonus).
        let sys = PiscesSystem::new(&UNIWARS);
        assert_eq!(sys.dip_bank_value(2) & 0x03, 0x01);
    }

    #[test]
    fn pisces_dip_banks_map_to_in1_in2() {
        let mut sys = PiscesSystem::new(&PISCES);
        // Bank 0 → IN1 (Lives/Cabinet), bank 1 → IN2 (Bonus/Coinage/Difficulty).
        sys.set_dip_bank_value(0, 0xc0); // 4 lives + cocktail
        sys.set_dip_bank_value(1, 0x07); // 20000 + coinage + hard
        assert_eq!(sys.dip_bank_value(0), 0xc0);
        assert_eq!(sys.dip_bank_value(1), 0x07);
        assert_eq!(sys.board.in1 & 0xc0, 0xc0);
        assert_eq!(sys.board.in2 & 0x07, 0x07);
    }

    #[test]
    fn dip_writes_preserve_input_bits() {
        let mut sys = PiscesSystem::new(&UNIWARS);
        sys.board
            .handle_input(crate::galaxian::INPUT_P1_START, true); // IN1 bit0
        sys.set_dip_bank_value(1, 0x40); // change Coinage on IN1
        assert_eq!(sys.board.in1 & 0x01, 0x01, "start bit survives DIP write");
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = PiscesSystem::new(&PISCES);
        sys.board.in1 = 0x42;
        let mut w = StateWriter::new();
        Saveable::save_state(&sys, &mut w);
        let bytes = w.into_vec();

        let mut sys2 = PiscesSystem::new(&PISCES);
        let mut r = StateReader::new(&bytes);
        Saveable::load_state(&mut sys2, &mut r).unwrap();
        assert_eq!(sys2.board.in1, 0x42);
    }
}
