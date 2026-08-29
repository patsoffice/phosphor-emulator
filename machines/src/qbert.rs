//! Q*Bert (1982, Gottlieb) — Gottlieb System 80 (GG-III) platform.
//!
//! Thin wrapper around `GottliebBoard` providing game-specific ROM loading,
//! input wiring, and `Bus` implementation for the main I8088's memory map.

use std::time::Instant;

use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    DipApplyTiming, DipChoice, DipOption, DipSwitchBank, Direction, InputConfigurable,
    InputControl, InputEvent, InputId, InputKind, MachineCore, Nvram, Profilable, ProfileSpan,
    SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::gfx::GfxLayout;
use phosphor_macros::Saveable;

use crate::gottlieb::{self, GottliebBoard};
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_high;

// ---------------------------------------------------------------------------
// ROM definitions (from MAME gottlieb.cpp — qbert parent set)
// ---------------------------------------------------------------------------

static QBERT_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x6000, // 24KB (3 × 8KB)
    entries: &[
        RomEntry {
            name: "qb-rom2.bin",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xfe434526],
        },
        RomEntry {
            name: "qb-rom1.bin",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x55635447],
        },
        RomEntry {
            name: "qb-rom0.bin",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x8e318641],
        },
    ],
};

static QBERT_SOUND_ROM: RomRegion = RomRegion {
    size: 0x2000, // 8KB (2 × 2KB, loaded at end of region)
    entries: &[
        RomEntry {
            name: "qb-snd1.bin",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0x15787c07],
        },
        RomEntry {
            name: "qb-snd2.bin",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0x58437508],
        },
    ],
};

static QBERT_TILE_ROM: RomRegion = RomRegion {
    size: 0x2000, // 8KB (2 × 4KB)
    entries: &[
        RomEntry {
            name: "qb-bg0.bin",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x7a9ba824],
        },
        RomEntry {
            name: "qb-bg1.bin",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x22e5b891],
        },
    ],
};

static QBERT_SPRITE_ROM: RomRegion = RomRegion {
    size: 0x8000, // 32KB (4 × 8KB — one per bitplane)
    entries: &[
        RomEntry {
            name: "qb-fg3.bin",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xdd436d3a],
        },
        RomEntry {
            name: "qb-fg2.bin",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0xf69b9483],
        },
        RomEntry {
            name: "qb-fg1.bin",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x224e8356],
        },
        RomEntry {
            name: "qb-fg0.bin",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0x2f695b85],
        },
    ],
};

/// Votrax SC-01A internal phoneme ROM (optional — speech works only if present).
static QBERT_VOTRAX_ROM: RomRegion = RomRegion {
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
const QBERT_CONTROLS: &[InputControl] = &[
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
// QbertSystem
// ---------------------------------------------------------------------------

/// Q*Bert (1982, Gottlieb) on the Gottlieb System 80 (GG-III) platform.
///
/// Wraps `GottliebBoard` with Q*Bert-specific ROM loading, input mapping,
/// and `Bus<Address = u32>` implementation for the I8088 main CPU.
#[derive(Saveable, phosphor_macros::BusDebug)]
pub struct QbertSystem {
    /// The 8088 is held beside the board, which is its bus.
    #[debug_cpu("I8088 Main")]
    pub cpu: phosphor_core::cpu::i8088::I8088,

    #[debug_bus]
    pub board: GottliebBoard,
}

impl QbertSystem {
    pub fn new() -> Self {
        let mut board = GottliebBoard::new();
        // IN1 default: service bit 6 is active-LOW (idle high)
        board.input_ports[0] = 0x40;
        Self {
            cpu: phosphor_core::cpu::i8088::I8088::new(),
            board,
        }
    }

    /// One CPU cycle. Returns 1 at an instruction boundary (for the debugger,
    /// which steps instructions rather than cycles).
    pub fn step_cycle(&mut self) -> u32 {
        gottlieb::tick(&mut self.cpu, &mut self.board);
        GottliebBoard::instruction_boundaries(&self.cpu)
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
        // Program ROM (24KB, loaded at end of 0x6000-0xFFFF region → 0xA000-0xFFFF)
        let prog_data = QBERT_PROGRAM_ROM.load(rom_set)?;
        self.board.load_program_rom(&prog_data);

        // Sound ROM (8KB, loaded into sound board)
        let sound_data = QBERT_SOUND_ROM.load(rom_set)?;
        self.board.load_sound_rom(&sound_data);

        // Votrax SC-01A phoneme ROM (optional — speech disabled if missing)
        if let Ok(votrax_data) = QBERT_VOTRAX_ROM.load(rom_set) {
            self.board.load_votrax_rom(&votrax_data);
        }

        // GFX ROMs
        let tile_data = QBERT_TILE_ROM.load(rom_set)?;
        let sprite_data = QBERT_SPRITE_ROM.load(rom_set)?;
        self.board.decode_gfx(&tile_data, &sprite_data);

        // Q*Bert uses ROM tiles for all codes (init_romtiles)
        self.board.gfxcharlo = true;
        self.board.gfxcharhi = true;

        // IN1 default: service bit 6 is active-LOW (idle high)
        self.board.input_ports[0] = 0x40;

        Ok(())
    }
}

impl Default for QbertSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus — I8088 main CPU memory map (20-bit address masked to 16-bit)
// ---------------------------------------------------------------------------

impl Bus for GottliebBoard {
    type Address = u32;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u32) -> u8 {
        let addr16 = (addr & 0xFFFF) as u16;
        let data = match addr16 {
            // NVRAM: 0x0000-0x0FFF
            0x0000..=0x0FFF => self.map.read_backing(addr16),

            // RAM: 0x1000-0x2FFF
            0x1000..=0x2FFF => self.map.read_backing(addr16),

            // Sprite RAM: 0x3000-0x37FF (256 bytes mirrored)
            0x3000..=0x37FF => {
                let offset = addr16 & 0xFF;
                self.map.read_backing(0x3000 + offset)
            }

            // Video RAM: 0x3800-0x3FFF (1KB mirrored)
            0x3800..=0x3FFF => {
                let offset = addr16 & 0x3FF;
                self.map.read_backing(0x3800 + offset)
            }

            // Char RAM: 0x4000-0x4FFF
            0x4000..=0x4FFF => self.map.read_backing(addr16),

            // Palette RAM: 0x5000-0x57FF (32 bytes mirrored)
            0x5000..=0x57FF => {
                let offset = (addr16 & 0x1F) as usize;
                self.palette_ram[offset]
            }

            // I/O ports: 0x5800-0x5FFF (3-bit decode)
            0x5800..=0x5FFF => self.io_port_read(addr16 as u8),

            // Program ROM: 0x6000-0xFFFF
            0x6000..=0xFFFF => self.map.read_backing(addr16),
        };
        self.map.watch_read(0, master, addr16, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u32, data: u8) {
        let addr16 = (addr & 0xFFFF) as u16;
        self.map.watch_write(0, master, addr16, data);
        match addr16 {
            // NVRAM: 0x0000-0x0FFF
            0x0000..=0x0FFF => self.map.write_backing(addr16, data),

            // RAM: 0x1000-0x2FFF
            0x1000..=0x2FFF => self.map.write_backing(addr16, data),

            // Sprite RAM: 0x3000-0x37FF (256 bytes mirrored)
            0x3000..=0x37FF => {
                let offset = addr16 & 0xFF;
                self.map.write_backing(0x3000 + offset, data);
            }

            // Video RAM: 0x3800-0x3FFF (1KB mirrored)
            0x3800..=0x3FFF => {
                let offset = addr16 & 0x3FF;
                self.map.write_backing(0x3800 + offset, data);
            }

            // Char RAM: 0x4000-0x4FFF
            0x4000..=0x4FFF => {
                let offset = (addr16 - 0x4000) as usize;
                self.charram_write(offset, data);
            }

            // Palette RAM: 0x5000-0x57FF (32 bytes mirrored)
            0x5000..=0x57FF => {
                let offset = (addr16 & 0x1F) as usize;
                self.update_palette(offset, data);
            }

            // I/O ports: 0x5800-0x5FFF
            0x5800..=0x5FFF => self.io_port_write(addr16 as u8, data),

            _ => {} // ROM and unmapped: writes ignored
        }
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        match target {
            BusMaster::Cpu(0) => {
                // VBLANK NMI: asserted during blanking period (scanlines 240-255)
                let scanline = self.clock / gottlieb::TIMING.cycles_per_scanline
                    % gottlieb::TIMING.total_scanlines;
                let in_vblank = scanline >= gottlieb::VISIBLE_LINES;
                InterruptState {
                    nmi: in_vblank,
                    ..Default::default()
                }
            }
            _ => InterruptState::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Machine traits (MachineCore + capabilities)
// ---------------------------------------------------------------------------

crate::impl_board_delegation!(
    QbertSystem,
    board,
    gottlieb::TIMING,
    orientation,
    overlay_stats
);

impl QbertSystem {
    /// The live clock domains, under the FPS counter.
    ///
    /// Q*Bert is the board whose speech clock is a VCO the game steers at
    /// runtime, so its rates are the ones actually worth watching move rather
    /// than reading once out of a constructor.
    fn overlay_stats_impl(&self) -> Option<String> {
        Some(self.board.clock_summary())
    }
}

impl InputConfigurable for QbertSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        QBERT_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let InputEvent::Button { id, pressed } = event else {
            return;
        };
        match id.0 as u8 {
            // IN1: start/coin (active-high, bits 0-3; service active-low, bit 6)
            INPUT_START1 => set_bit_active_high(&mut self.board.input_ports[0], 0, pressed),
            INPUT_START2 => set_bit_active_high(&mut self.board.input_ports[0], 1, pressed),
            INPUT_COIN1 => set_bit_active_high(&mut self.board.input_ports[0], 2, pressed),
            INPUT_COIN2 => set_bit_active_high(&mut self.board.input_ports[0], 3, pressed),
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

impl MachineCore for QbertSystem {
    crate::machine_core_metadata!("qbert", gottlieb::TIMING, gottlieb::clock_tree);

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

impl SaveState for QbertSystem {
    crate::machine_save_state!();
}

impl Nvram for QbertSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.map.region_data(gottlieb::Region::Nvram))
    }

    fn load_nvram(&mut self, data: &[u8]) {
        let nvram = self.board.map.region_data_mut(gottlieb::Region::Nvram);
        let len = data.len().min(nvram.len());
        nvram[..len].copy_from_slice(&data[..len]);
    }
}

impl Profilable for QbertSystem {
    fn set_profiling(&mut self, enabled: bool) {
        self.board.profiling = enabled;
    }

    fn frame_profile_spans(&self) -> &[ProfileSpan] {
        &self.board.profile_spans
    }
}
/// DIP switch metadata for Q*bert's DSW byte (read flat at 0x5800, which the
/// Gottlieb board exposes as I/O port 0 -> `board.dsw`). Choice bits and labels
/// follow MAME's `qbert` layout; option defaults OR to the historical 0x00 the
/// board powers on with (note: this leaves Kicker Off, where MAME's factory
/// default is On). Bits 0x20/0x40/0x80 are unused.
const QBERT_DIP_BANKS: &[DipSwitchBank] = &[DipSwitchBank {
    name: "DSW",
    options: &[
        DipOption {
            name: "Demo Sounds",
            mask: 0x01,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "On",
                    value: 0x00,
                },
                DipChoice {
                    label: "Off",
                    value: 0x01,
                },
            ],
        },
        DipOption {
            name: "Kicker",
            mask: 0x02,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "Off",
                    value: 0x00,
                },
                DipChoice {
                    label: "On",
                    value: 0x02,
                },
            ],
        },
        DipOption {
            name: "Cabinet",
            mask: 0x04,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "Upright",
                    value: 0x00,
                },
                DipChoice {
                    label: "Cocktail",
                    value: 0x04,
                },
            ],
        },
        DipOption {
            name: "Demo Mode (Cheat)",
            mask: 0x08,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "Off",
                    value: 0x00,
                },
                DipChoice {
                    label: "On",
                    value: 0x08,
                },
            ],
        },
        DipOption {
            name: "Free Play",
            mask: 0x10,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "Off",
                    value: 0x00,
                },
                DipChoice {
                    label: "On",
                    value: 0x10,
                },
            ],
        },
    ],
}];

crate::impl_dip_switches!(QbertSystem, QBERT_DIP_BANKS, board.dsw);

crate::impl_map_debug_trace!(QbertSystem, board.map);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

crate::register_machine!(QbertSystem, "qbert", &["qbert"], QBERT_CONTROLS);

// ---------------------------------------------------------------------------
// Graphics viewer regions
// ---------------------------------------------------------------------------

/// The sprite decode `gottlieb::decode_gfx` builds at load time, restated as a
/// `'static` layout for `disasm gfxview`.
///
/// The plane offsets there are computed from the ROM length (`(3 - p) * len/4 *
/// 8`); this region is a fixed 0x8000 bytes, so they are the constants below.
/// Both must describe the same decode: a divergence would show up as a viewer
/// that disagrees with the screen.
static QBERT_SPRITE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0x30000, 0x20000, 0x10000, 0],
    x_offsets: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    y_offsets: &[
        0, 16, 32, 48, 64, 80, 96, 112, 128, 144, 160, 176, 192, 208, 224, 240,
    ],
    char_increment: 256,
};

// Q*Bert has no colour PROM: its palette is 16 entries of RAM written by the
// CPU, so there is nothing to hand the viewer and it falls back to a grayscale
// ramp. The shape of a sprite is readable; its colours are not.
inventory::submit! {
    crate::gfx_registry::GfxRegion {
        machine: "qbert",
        region: "sprites",
        count: 256, // 0x8000 bytes / 128 bytes per 16x16 4bpp sprite
        width: 16,
        height: 16,
        layout: &QBERT_SPRITE_LAYOUT,
        load: |rs| QBERT_SPRITE_ROM.load(rs),
        palette: None,
    }
}

inventory::submit! {
    crate::gfx_registry::GfxRegion {
        machine: "qbert",
        region: "tiles",
        count: 256, // 0x2000 bytes / 32 bytes per 8x8 4bpp tile
        width: 8,
        height: 8,
        layout: &gottlieb::GOTTLIEB_TILE_LAYOUT,
        load: |rs| QBERT_TILE_ROM.load(rs),
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
        let sys = QbertSystem::new();
        assert_eq!(sys.dip_bank_value(0), 0x00);
        crate::assert_dip_banks_valid(sys.dip_banks(), &[sys.dip_bank_value(0)]);
    }

    #[test]
    fn declares_native_dims_and_rot270() {
        use phosphor_core::core::machine::{Orientation, Renderable};
        let sys = QbertSystem::new();
        // display_size() is the NATIVE (unrotated) framebuffer; the frontend
        // applies the declared ROT270 to present it portrait.
        assert_eq!(sys.display_size(), (256, 240));
        assert_eq!(sys.orientation(), Orientation::ROT270);
        assert!(sys.orientation().swaps_axes());
        assert_eq!(sys.display_aspect(), Some((3, 4)));
    }

    #[test]
    fn render_frame_emits_native_unrotated_rgb() {
        use phosphor_core::core::machine::Renderable;
        let mut sys = QbertSystem::new();
        // Tag a known native pixel and a palette entry, then confirm render_frame
        // writes it at the native row-major position (no baked rotation).
        //
        // The palette is sampled per scanline, so the row has to have been
        // scanned with this palette live for `render_frame` to resolve against
        // it -- that is what `begin_scanline` stands for here.
        let (nx, ny) = (5usize, 7usize);
        sys.board.palette_rgb[1] = (10, 20, 30);
        sys.board.begin_scanline(ny as u64);
        sys.board.pixel_buffer[ny * 256 + nx] = 1;
        let mut buf = vec![0u8; 256 * 240 * 3];
        sys.render_frame(&mut buf);
        let i = (ny * 256 + nx) * 3;
        assert_eq!(&buf[i..i + 3], &[10, 20, 30]);
    }

    #[test]
    fn set_dip_option_masks_only_its_bits() {
        let mut sys = QbertSystem::new();
        // Kicker is option 1 (mask 0x02); pick "On" (0x02).
        sys.set_dip_option(0, 1, 0x02);
        assert_eq!(sys.dip_bank_value(0), 0x02);
        // Free Play is option 4 (mask 0x10); enabling it preserves Kicker.
        sys.set_dip_option(0, 4, 0x10);
        assert_eq!(sys.dip_bank_value(0), 0x12);
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = QbertSystem::new();

        // Set known state
        sys.board.map.region_data_mut(gottlieb::Region::Nvram)[0x100] = 0xAA;
        sys.board.map.region_data_mut(gottlieb::Region::Ram)[0x50] = 0xBB;
        sys.board.map.region_data_mut(gottlieb::Region::VideoRam)[0x10] = 0xCC;
        sys.board.palette_ram[0] = 0x55;
        sys.board.clock = 50_000;
        sys.board.watchdog_counter = 42;
        sys.board.video_control = 1;
        sys.board.sprite_bank = 2;

        // Save
        let data = sys.save_state().expect("save_state should return Some");

        // Load into fresh system
        let mut sys2 = QbertSystem::new();
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
        assert_eq!(sys2.board.sprite_bank, 2);
    }

    #[test]
    fn input_active_high_joystick() {
        let mut sys = QbertSystem::new();

        // Initially joystick is idle (0x00)
        assert_eq!(sys.board.input_ports[3], 0x00);

        // Press right
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_RIGHT) as u16),
            pressed: true,
        });
        assert_eq!(sys.board.input_ports[3], 0x01); // bit 0 set

        // Release right
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_RIGHT) as u16),
            pressed: false,
        });
        assert_eq!(sys.board.input_ports[3], 0x00);

        // Press up
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_UP) as u16),
            pressed: true,
        });
        assert_eq!(sys.board.input_ports[3], 0x04); // bit 2 set
    }

    #[test]
    fn input_coin_and_start() {
        let mut sys = QbertSystem::new();

        // IN1 starts with service bit 6 idle high
        assert_eq!(sys.board.input_ports[0], 0x40);

        // Press coin 1 (active-high, bit 2)
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_COIN1) as u16),
            pressed: true,
        });
        assert_eq!(sys.board.input_ports[0], 0x44);

        // Press service (active-low, bit 6 → clear)
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_SERVICE) as u16),
            pressed: true,
        });
        assert_eq!(sys.board.input_ports[0], 0x04); // bit 6 cleared

        // Release service (bit 6 → set)
        sys.handle_input(InputEvent::Button {
            id: InputId((INPUT_SERVICE) as u16),
            pressed: false,
        });
        assert_eq!(sys.board.input_ports[0], 0x44);
    }

    #[test]
    fn palette_rgb_decode() {
        let mut sys = QbertSystem::new();

        // Write palette entry 0: even byte G=0xF B=0x0, odd byte R=0xF
        sys.bus_write(BusMaster::Cpu(0), 0x5000, 0xF0); // G=15, B=0
        sys.bus_write(BusMaster::Cpu(0), 0x5001, 0x0F); // R=15

        assert_eq!(sys.board.palette_rgb[0], (255, 255, 0)); // R=255, G=255, B=0
    }

    #[test]
    fn palette_resistor_weighted() {
        let mut sys = QbertSystem::new();

        // Value 4 (0100): resistor DAC = 70, not linear 68
        sys.bus_write(BusMaster::Cpu(0), 0x5000, 0x40); // G=4, B=0
        sys.bus_write(BusMaster::Cpu(0), 0x5001, 0x04); // R=4
        assert_eq!(sys.board.palette_rgb[0], (70, 70, 0));

        // Value 12 (1100): resistor DAC = 206, not linear 204
        sys.bus_write(BusMaster::Cpu(0), 0x5002, 0xC0); // G=12, B=0
        sys.bus_write(BusMaster::Cpu(0), 0x5003, 0x0C); // R=12
        assert_eq!(sys.board.palette_rgb[1], (206, 206, 0));
    }

    #[test]
    fn palette_mirror() {
        let mut sys = QbertSystem::new();

        // Write through mirror (0x5020 maps to same as 0x5000)
        sys.bus_write(BusMaster::Cpu(0), 0x5020, 0xAB);
        assert_eq!(sys.board.palette_ram[0], 0xAB);
    }

    #[test]
    fn memory_map_ram_read_write() {
        let mut sys = QbertSystem::new();

        sys.bus_write(BusMaster::Cpu(0), 0x1000, 0x55);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x1000), 0x55);
    }

    #[test]
    fn sprite_ram_mirror() {
        let mut sys = QbertSystem::new();

        sys.bus_write(BusMaster::Cpu(0), 0x3010, 0xBB);
        // Mirror: 0x3110 maps to 0x3010 (offset 0x10)
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x3110), 0xBB);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x3210), 0xBB);
    }

    #[test]
    fn video_ram_mirror() {
        let mut sys = QbertSystem::new();

        sys.bus_write(BusMaster::Cpu(0), 0x3900, 0xCC);
        // Mirror: 0x3D00 maps to 0x3900 (offset 0x100, bit 10 don't-care)
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x3D00), 0xCC);
    }

    #[test]
    fn address_wraps_to_16_bit() {
        let mut sys = QbertSystem::new();

        // I8088 physical address 0x10042 should wrap to 0x0042 (NVRAM)
        sys.bus_write(BusMaster::Cpu(0), 0x10042, 0xDD);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x0042), 0xDD);
    }
}
