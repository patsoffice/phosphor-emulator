//! Q*Bert's Qubes (1983, Mylstar) on the Gottlieb board.
//!
//! Thin wrapper around `GottliebBoard` with the game's ROM loading and input
//! wiring. The `Bus` implementation lives on the shared board (see `qbert.rs`):
//! Qubes decodes the same map, with four program ROMs from $8000 instead of
//! three from $A000 and 64 KB of sprite ROMs instead of 32 KB.
//!
//! The extra sprite half is banked from the $5803 output latch's bit 4 (the
//! reference driver's `qbertqub_output_w`); the loader opts the board into
//! that latch. The coin slots are swapped against Q*Bert (COIN1 on IN1 bit 3,
//! COIN2 on bit 2), and the DIP table is the game's own. Sources: the Qubes
//! manual's logic sheets 1-2 (PDF pp16-20; sheet 3 is absent from the scan)
//! for the program ROMs and shared hardware, the reference driver's `qbert`
//! machine, `qbertqub` ports and `init_qbertqub`, and the game code itself,
//! which toggles the bank bit around sprite work and never writes $5804.

use std::time::Instant;

use phosphor_core::core::machine::{
    DipApplyTiming, DipChoice, DipOption, DipSwitchBank, Direction, InputConfigurable,
    InputControl, InputEvent, InputId, InputKind, MachineCore, Nvram, Profilable, ProfileSpan,
    SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::gfx::GfxLayout;
use phosphor_macros::Saveable;

use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::gottlieb::{self, GottliebBoard};
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_high;

// ---------------------------------------------------------------------------
// ROM definitions (from MAME gottlieb.cpp: qbertqub set)
// ---------------------------------------------------------------------------

static QBERTSQUBES_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x8000, // 32KB (4 × 8KB)
    entries: &[
        RomEntry {
            name: "qq-rom3.bin",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xc4dbdcd7],
        },
        RomEntry {
            name: "qq-rom2.bin",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x21a6c6cc],
        },
        RomEntry {
            name: "qq-rom1.bin",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x63e6c43d],
        },
        RomEntry {
            name: "qq-rom0.bin",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0x8ddbe438],
        },
    ],
};

static QBERTSQUBES_SOUND_ROM: RomRegion = RomRegion {
    size: 0x2000, // 8KB (2 × 2KB, loaded at end of region)
    entries: &[
        RomEntry {
            name: "qq-snd1.bin",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0xe704b450],
        },
        RomEntry {
            name: "qq-snd2.bin",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0xc6a98bf8],
        },
    ],
};

static QBERTSQUBES_TILE_ROM: RomRegion = RomRegion {
    size: 0x2000, // 8KB (2 × 4KB)
    entries: &[
        RomEntry {
            name: "qq-bg0.bin",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x050badde],
        },
        RomEntry {
            name: "qq-bg1.bin",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x8875902f],
        },
    ],
};

static QBERTSQUBES_SPRITE_ROM: RomRegion = RomRegion {
    size: 0x10000, // 64KB (4 × 16KB); the output latch banks 256-sprite halves
    entries: &[
        RomEntry {
            name: "qq-fg3.bin",
            size: 0x4000,
            offset: 0x0000,
            crc32: &[0x91a949cc],
        },
        RomEntry {
            name: "qq-fg2.bin",
            size: 0x4000,
            offset: 0x4000,
            crc32: &[0x782d9431],
        },
        RomEntry {
            name: "qq-fg1.bin",
            size: 0x4000,
            offset: 0x8000,
            crc32: &[0x71c3ac4c],
        },
        RomEntry {
            name: "qq-fg0.bin",
            size: 0x4000,
            offset: 0xc000,
            crc32: &[0x6192853f],
        },
    ],
};

static QBERTSQUBES_VOTRAX_ROM: RomRegion = RomRegion {
    size: 0x200, // 512 bytes (64 entries × 8 bytes LE-64)
    entries: &[RomEntry {
        name: "sc01a.bin",
        size: 0x200,
        offset: 0x0000,
        crc32: &[0xfc416227],
    }],
};

// ---------------------------------------------------------------------------
// Input definitions
// ---------------------------------------------------------------------------

// IN1: start/coin (active-high except service)
const INPUT_START1: u8 = 0;
const INPUT_START2: u8 = 1;
const INPUT_COIN1: u8 = 2;
const INPUT_COIN2: u8 = 3;
const INPUT_SERVICE: u8 = 4;

// IN4: joystick (active-high)
const INPUT_RIGHT: u8 = 10;
const INPUT_LEFT: u8 = 11;
const INPUT_UP: u8 = 12;
const INPUT_DOWN: u8 = 13;

/// Typed logical controls. `InputId`s reuse the `INPUT_*` numbering; default
/// bindings mirror the legacy name-matched defaults.
const QBERTSQUBES_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_COIN1 as u16),
        stable_name: "coin1",
        label: "Coin 1",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(INPUT_COIN2 as u16),
        stable_name: "coin2",
        label: "Coin 2",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[],
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
        id: InputId(INPUT_SERVICE as u16),
        stable_name: "service",
        label: "Service",
        kind: InputKind::Service,
        player: None,
        default_bindings: crate::input_defaults::SERVICE,
    },
    InputControl {
        id: InputId(INPUT_RIGHT as u16),
        stable_name: "p1_right",
        label: "P1 Right",
        kind: InputKind::DigitalDirection {
            direction: Direction::Right,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
    InputControl {
        id: InputId(INPUT_LEFT as u16),
        stable_name: "p1_left",
        label: "P1 Left",
        kind: InputKind::DigitalDirection {
            direction: Direction::Left,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(INPUT_UP as u16),
        stable_name: "p1_up",
        label: "P1 Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_UP,
    },
    InputControl {
        id: InputId(INPUT_DOWN as u16),
        stable_name: "p1_down",
        label: "P1 Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_DOWN,
    },
];

// ---------------------------------------------------------------------------
// QbertQubesSystem
// ---------------------------------------------------------------------------

/// Q*Bert's Qubes (1983, Mylstar) on the Gottlieb board.
///
/// Wraps `GottliebBoard` with the game's ROM loading and input wiring. The
/// I8088 memory map is the shared `Bus for GottliebBoard` in `qbert.rs`.
#[derive(Saveable, phosphor_macros::BusDebug)]
pub struct QbertQubesSystem {
    /// The 8088 is held beside the board, which is its bus.
    #[debug_cpu("I8088 Main")]
    pub cpu: phosphor_core::cpu::i8088::I8088,

    #[debug_bus]
    pub board: GottliebBoard,
}

impl QbertQubesSystem {
    pub fn new() -> Self {
        let mut board = GottliebBoard::new();
        // IN1 default: service bit 6 is active-LOW (idle high)
        board.input_ports[0] = 0x40;
        Self {
            cpu: phosphor_core::cpu::i8088::I8088::new(),
            board,
        }
    }

    /// One CPU cycle, returning the instruction-boundary mask the debugger steps
    /// instructions with: bit 0 the I8088, bit 1 the sound board's 6502.
    pub fn step_cycle(&mut self) -> u32 {
        gottlieb::tick(&mut self.cpu, &mut self.board);
        self.board.instruction_boundaries(&self.cpu)
    }

    /// Read the CPU-facing bus, side effects and all. Distinct from the
    /// debugger's `BusDebug::peek`/`poke`, which avoid side effects.
    pub fn bus_read(&mut self, master: BusMaster, addr: u32) -> u8 {
        Bus::read(&mut self.board, master, addr)
    }

    /// Write the CPU-facing bus, side effects and all. See [`Self::bus_read`].
    pub fn bus_write(&mut self, master: BusMaster, addr: u32, data: u8) {
        Bus::write(&mut self.board, master, addr, data);
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        // Program ROM (32KB, loaded at end of 0x6000-0xFFFF region → 0x8000-0xFFFF)
        let prog_data = QBERTSQUBES_PROGRAM_ROM.load(rom_set)?;
        self.board.load_program_rom(&prog_data);

        // Sound ROM (8KB, loaded into sound board)
        let sound_data = QBERTSQUBES_SOUND_ROM.load(rom_set)?;
        self.board.load_sound_rom(&sound_data);

        // Votrax SC-01A phoneme ROM (optional; speech disabled if missing)
        if let Ok(votrax_data) = QBERTSQUBES_VOTRAX_ROM.load(rom_set) {
            self.board.load_votrax_rom(&votrax_data);
        }

        // GFX ROMs
        let tile_data = QBERTSQUBES_TILE_ROM.load(rom_set)?;
        let sprite_data = QBERTSQUBES_SPRITE_ROM.load(rom_set)?;
        self.board.decode_gfx(&tile_data, &sprite_data);

        // Qubes uses ROM tiles for all codes (init_romtiles)
        self.board.gfxcharlo = true;
        self.board.gfxcharhi = true;

        // And its output latch banks the 512 sprites from bit 4.
        self.board.output_bit4_banks_sprites = true;

        // IN1 default: service bit 6 is active-LOW (idle high)
        self.board.input_ports[0] = 0x40;

        Ok(())
    }
}

impl Default for QbertQubesSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Machine traits (MachineCore + capabilities)
// ---------------------------------------------------------------------------

crate::impl_board_delegation!(
    QbertQubesSystem,
    board,
    gottlieb::TIMING,
    orientation,
    overlay_stats
);

impl QbertQubesSystem {
    /// The live clock domains, under the FPS counter.
    ///
    /// Same board as Q*Bert, whose speech clock is a VCO the game steers at
    /// runtime, so the same live rates are worth watching here.
    fn overlay_stats_impl(&self) -> Option<String> {
        Some(self.board.clock_summary())
    }
}

impl InputConfigurable for QbertQubesSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        QBERTSQUBES_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let InputEvent::Button { id, pressed } = event else {
            return;
        };
        match id.0 as u8 {
            // IN1: start/coin (active-high, bits 0-3; service active-low, bit 6).
            // The coin slots are swapped against Q*Bert: COIN1 is bit 3 here.
            INPUT_START1 => set_bit_active_high(&mut self.board.input_ports[0], 0, pressed),
            INPUT_START2 => set_bit_active_high(&mut self.board.input_ports[0], 1, pressed),
            INPUT_COIN1 => set_bit_active_high(&mut self.board.input_ports[0], 3, pressed),
            INPUT_COIN2 => set_bit_active_high(&mut self.board.input_ports[0], 2, pressed),
            // Active-LOW: clear on press, set on release
            INPUT_SERVICE => crate::set_bit_active_low(&mut self.board.input_ports[0], 6, pressed),
            // IN4: joystick (active-high, bits 0-3)
            INPUT_RIGHT => set_bit_active_high(&mut self.board.input_ports[3], 0, pressed),
            INPUT_LEFT => set_bit_active_high(&mut self.board.input_ports[3], 1, pressed),
            INPUT_UP => set_bit_active_high(&mut self.board.input_ports[3], 2, pressed),
            INPUT_DOWN => set_bit_active_high(&mut self.board.input_ports[3], 3, pressed),
            _ => {}
        }
    }
}

impl MachineCore for QbertQubesSystem {
    crate::machine_core_metadata!("qbertsqubes", gottlieb::TIMING, gottlieb::clock_tree);

    fn gfx_sheets(&self) -> Vec<phosphor_core::core::machine::GfxSheet<'_>> {
        use phosphor_core::core::machine::GfxSheet;
        vec![
            GfxSheet {
                name: "tiles",
                cache: &self.board.tile_rom_cache,
                palette: &self.board.palette_rgb,
            },
            GfxSheet {
                name: "sprites",
                cache: &self.board.sprite_cache,
                palette: &self.board.palette_rgb,
            },
        ]
    }

    fn run_frame(&mut self) {
        let t0 = self.board.profiling.then(Instant::now);

        // The board renders on the frame's last cycle inside `tick`, so the
        // single render site is shared with the debugger's `debug_tick` path.
        gottlieb::run_frame(&mut self.cpu, &mut self.board);

        if let Some(t0) = t0 {
            // The render now runs inside the loop, so split it back out of the
            // total using the duration the board recorded.
            let total = t0.elapsed();
            let gfx = self.board.last_render;
            self.board.profile_spans.clear();
            self.board.profile_spans.push(ProfileSpan {
                name: "cpu",
                duration: total.saturating_sub(gfx),
            });
            self.board.profile_spans.push(ProfileSpan {
                name: "gfx",
                duration: gfx,
            });
        }
    }

    fn reset(&mut self) {
        self.board.reset_board();
        self.cpu.reset(&mut self.board, BusMaster::Cpu(0));
        // Re-initialize IN1 idle state
        self.board.input_ports[0] = 0x40;
    }
}

impl SaveState for QbertQubesSystem {
    crate::machine_save_state!();
}

impl Nvram for QbertQubesSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.map.region_data(gottlieb::Region::Nvram))
    }

    fn load_nvram(&mut self, data: &[u8]) {
        let nvram = self.board.map.region_data_mut(gottlieb::Region::Nvram);
        let len = data.len().min(nvram.len());
        nvram[..len].copy_from_slice(&data[..len]);
    }
}

impl Profilable for QbertQubesSystem {
    fn set_profiling(&mut self, enabled: bool) {
        self.board.profiling = enabled;
    }

    fn frame_profile_spans(&self) -> &[ProfileSpan] {
        &self.board.profile_spans
    }
}
/// DIP switch metadata for Qubes' DSW byte (read flat at 0x5800, which the
/// Gottlieb board exposes as I/O port 0 -> `board.dsw`). Choice bits and labels
/// follow MAME's `qbertqub` layout; every option defaults to 0x00, the value
/// the board powers on with.
const QBERTSQUBES_DIP_BANKS: &[DipSwitchBank] = &[DipSwitchBank {
    name: "DSW",
    options: &[
        DipOption {
            name: "Demo Sounds",
            mask: 0x08,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "On",
                    value: 0x00,
                },
                DipChoice {
                    label: "Off",
                    value: 0x08,
                },
            ],
            conditional: &[],
        },
        DipOption {
            name: "Coinage",
            mask: 0x35,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "1 Coin/1 Credit",
                    value: 0x00,
                },
                DipChoice {
                    label: "2 Coins/1 Credit",
                    value: 0x30,
                },
                DipChoice {
                    label: "1 Coin/2 Credits",
                    value: 0x05,
                },
                DipChoice {
                    label: "2 Coins/3 Credits",
                    value: 0x10,
                },
                DipChoice {
                    label: "1 Coin/3 Credits",
                    value: 0x15,
                },
                DipChoice {
                    label: "1 Coin/4 Credits",
                    value: 0x20,
                },
                DipChoice {
                    label: "1 Coin/5 Credits",
                    value: 0x25,
                },
                DipChoice {
                    label: "Free Play",
                    value: 0x11,
                },
                DipChoice {
                    label: "1 Coin/6 Credits",
                    value: 0x01,
                },
                DipChoice {
                    label: "1 Coin/7 Credits",
                    value: 0x04,
                },
                DipChoice {
                    label: "2 Coins/5 Credits",
                    value: 0x24,
                },
                DipChoice {
                    label: "2 Coins/1 Credit 2 Coins/3 Credits",
                    value: 0x14,
                },
                DipChoice {
                    label: "1 Coin/1 Credit 2 Coins/4 Credits",
                    value: 0x34,
                },
            ],
            conditional: &[],
        },
        DipOption {
            name: "Bonus Life at",
            mask: 0x02,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "10000",
                    value: 0x00,
                },
                DipChoice {
                    label: "15000",
                    value: 0x02,
                },
            ],
            conditional: &[],
        },
        DipOption {
            name: "Additional Bonus Life Every",
            mask: 0x40,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "20000",
                    value: 0x00,
                },
                DipChoice {
                    label: "25000",
                    value: 0x40,
                },
            ],
            conditional: &[],
        },
        DipOption {
            name: "Difficulty",
            mask: 0x80,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "Normal",
                    value: 0x00,
                },
                DipChoice {
                    label: "Hard",
                    value: 0x80,
                },
            ],
            conditional: &[],
        },
    ],
}];

crate::impl_dip_switches!(QbertQubesSystem, QBERTSQUBES_DIP_BANKS, board.dsw);

crate::impl_map_debug_trace!(QbertQubesSystem, board.map);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

crate::register_machine!(
    QbertQubesSystem,
    "qbertsqubes",
    &["qbertqub"],
    QBERTSQUBES_CONTROLS
);

// ---------------------------------------------------------------------------
// Disassembly regions
// ---------------------------------------------------------------------------
//
// The origins are where each image sits in its CPU's space, not where it sits
// in the ROM file, because that is what a relative branch in the listing
// resolves against.

// The main 8088's program ROM. `load_program_rom` puts the image at the END of
// the 0x6000-0xFFFF region, so a 32 KB ROM occupies 0x8000 upward and the reset
// vector at the top of the space is the last bytes of the file.
inventory::submit! {
    DisasmRegion {
        machine: "qbertsqubes",
        region: "main",
        cpu: DisasmCpu::I8088,
        org: 0x1_0000 - QBERTSQUBES_PROGRAM_ROM.size as u32,
        size: QBERTSQUBES_PROGRAM_ROM.size as u32,
        load: |rs| QBERTSQUBES_PROGRAM_ROM.load(rs),
    }
}

// The sound board's 6502 ROM, which the board maps at 0x6000. A15 is not
// decoded on that side, which is how the reset vector at 0xFFFC reaches the
// end of this 8 KB image.
inventory::submit! {
    DisasmRegion {
        machine: "qbertsqubes",
        region: "sound",
        cpu: DisasmCpu::M6502,
        org: 0x6000,
        size: QBERTSQUBES_SOUND_ROM.size as u32,
        load: |rs| QBERTSQUBES_SOUND_ROM.load(rs),
    }
}

// ---------------------------------------------------------------------------
// Graphics viewer regions
// ---------------------------------------------------------------------------

/// The sprite decode `gottlieb::decode_gfx` builds at load time, restated as a
/// `'static` layout for `disasm gfxview`.
///
/// The plane offsets there are computed from the ROM length (`(3 - p) * len/4 *
/// 8`); this region is a fixed 0x10000 bytes, so they are the constants below.
/// Both must describe the same decode: a divergence would show up as a viewer
/// that disagrees with the screen.
static QBERTSQUBES_SPRITE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0x60000, 0x40000, 0x20000, 0],
    x_offsets: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    y_offsets: &[
        0, 16, 32, 48, 64, 80, 96, 112, 128, 144, 160, 176, 192, 208, 224, 240,
    ],
    char_increment: 256,
};

// Qubes has no color PROM, like Q*Bert: its palette is 16 entries of RAM
// written by the CPU, so there is nothing to hand the viewer and it falls
// back to a grayscale ramp. The shape of a sprite is readable; its colors
// are not.
inventory::submit! {
    crate::gfx_registry::GfxRegion {
        machine: "qbertsqubes",
        region: "sprites",
        count: 512, // 0x10000 bytes / 128 bytes per 16x16 4bpp sprite
        width: 16,
        height: 16,
        layout: &QBERTSQUBES_SPRITE_LAYOUT,
        load: |rs| QBERTSQUBES_SPRITE_ROM.load(rs),
        palette: None,
    }
}

inventory::submit! {
    crate::gfx_registry::GfxRegion {
        machine: "qbertsqubes",
        region: "tiles",
        count: 256, // 0x2000 bytes / 32 bytes per 8x8 4bpp tile
        width: 8,
        height: 8,
        layout: &gottlieb::GOTTLIEB_TILE_LAYOUT,
        load: |rs| QBERTSQUBES_TILE_ROM.load(rs),
        palette: None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::DipSwitches;

    #[test]
    fn dip_default_and_metadata() {
        let sys = QbertQubesSystem::new();
        assert_eq!(sys.dip_bank_value(0), 0x00);
        crate::assert_dip_banks_valid(sys.dip_banks(), &[sys.dip_bank_value(0)]);
    }

    #[test]
    fn set_dip_option_masks_only_its_bits() {
        let mut sys = QbertQubesSystem::new();
        // Difficulty is option 4 (mask 0x80); pick "Hard" (0x80).
        sys.set_dip_option(0, 4, 0x80);
        assert_eq!(sys.dip_bank_value(0), 0x80);
        // Bonus Life is option 2 (mask 0x02); enabling it preserves Difficulty.
        sys.set_dip_option(0, 2, 0x02);
        assert_eq!(sys.dip_bank_value(0), 0x82);
    }

    #[test]
    fn input_coin_slots_are_swapped_against_qbert() {
        let mut sys = QbertQubesSystem::new();

        // IN1 starts with service bit 6 idle high
        assert_eq!(sys.board.input_ports[0], 0x40);

        // Coin 1 lands on bit 3 here (bit 2 on Q*Bert)
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_COIN1 as u16),
            pressed: true,
        });
        assert_eq!(sys.board.input_ports[0], 0x48);

        // Coin 2 lands on bit 2
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_COIN2 as u16),
            pressed: true,
        });
        assert_eq!(sys.board.input_ports[0], 0x4C);
    }

    #[test]
    fn output_latch_bit4_banks_sprites_through_the_bus() {
        // The board test pins the latch; this pins the address: a $5803 write
        // with the flag set must switch the bank, and without it must not.
        let mut sys = QbertQubesSystem::new();
        sys.bus_write(BusMaster::Cpu(0), 0x5803, 0x10);
        assert_eq!(sys.board.sprite_bank, 0);

        sys.board.output_bit4_banks_sprites = true;
        sys.bus_write(BusMaster::Cpu(0), 0x5803, 0x10);
        assert_eq!(sys.board.sprite_bank, 1);
        sys.bus_write(BusMaster::Cpu(0), 0x5803, 0x00);
        assert_eq!(sys.board.sprite_bank, 0);
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = QbertQubesSystem::new();

        // Set known state
        sys.board.map.region_data_mut(gottlieb::Region::Nvram)[0x100] = 0xAA;
        sys.board.map.region_data_mut(gottlieb::Region::Ram)[0x50] = 0xBB;
        sys.board.map.region_data_mut(gottlieb::Region::VideoRam)[0x10] = 0xCC;
        sys.board.palette_ram[0] = 0x55;
        sys.board.clock = 50_000;
        sys.board.watchdog_counter = 42;
        sys.board.video_control = 1;
        sys.board.sprite_bank = 1;

        // Save
        let data = sys.save_state().expect("save_state should return Some");

        // Load into fresh system
        let mut sys2 = QbertQubesSystem::new();
        sys2.load_state(&data).unwrap();

        // Verify
        assert_eq!(
            sys2.board.map.region_data(gottlieb::Region::Nvram)[0x100],
            0xAA
        );
        assert_eq!(
            sys2.board.map.region_data(gottlieb::Region::Ram)[0x50],
            0xBB
        );
        assert_eq!(
            sys2.board.map.region_data(gottlieb::Region::VideoRam)[0x10],
            0xCC
        );
        assert_eq!(sys2.board.palette_ram[0], 0x55);
        assert_eq!(sys2.board.clock, 50_000);
        assert_eq!(sys2.board.watchdog_counter, 42);
        assert_eq!(sys2.board.video_control, 1);
        assert_eq!(sys2.board.sprite_bank, 1);
    }
}
