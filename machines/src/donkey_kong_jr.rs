use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{InputButton, InputReceiver, MachineCore, SaveState};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_macros::Saveable;

use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_high;
use crate::tkg04::{self, MainRegion, SoundRegion, Tkg04Board};

// ---------------------------------------------------------------------------
// Donkey Kong Jr ROM definitions
// ---------------------------------------------------------------------------
// Primary definitions use the "dkongjr2" MAME set (three contiguous 8KB ROMs).
// The parent "dkongjr" set uses ROM_CONTINUE for non-contiguous loading; see
// `load_parent_program_rom()` for that fallback path.

/// Main CPU program ROMs: 24KB at 0x0000-0x5FFF (three 8KB chips).
pub static DKONGJR_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        RomEntry {
            name: "0",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xdc1f1d12],
        },
        RomEntry {
            name: "1",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0xf1f286d0],
        },
        RomEntry {
            name: "2",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x4cb856c4],
        },
    ],
};

/// Sound CPU ROM: 4KB at 0x0000-0x0FFF (single chip, no mirroring needed).
pub static DKONGJR_SOUND_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "8",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0x715da5f8],
    }],
};

/// Tile GFX: 8KB (two 4KB ROMs, one per bitplane).
pub static DKONGJR_TILE_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "9",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x8d51aca9],
        },
        RomEntry {
            name: "10",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x4ef64ba5],
        },
    ],
};

/// Sprite GFX: 8KB (four 2KB ROMs, same interleaved layout as DK).
pub static DKONGJR_SPRITE_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "v_7c.bin",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0xdc7f4164],
        },
        RomEntry {
            name: "v_7d.bin",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x0ce7dcf6],
        },
        RomEntry {
            name: "v_7e.bin",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0x24d1ff17],
        },
        RomEntry {
            name: "v_7f.bin",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0x0f8c083f],
        },
    ],
};

/// Palette PROMs: c-2e (256B), c-2f (256B), v-2n (256B color codes).
pub static DKONGJR_PALETTE_PROMS: RomRegion = RomRegion {
    size: 0x0300,
    entries: &[
        RomEntry {
            name: "c-2e.bpr",
            size: 0x0100,
            offset: 0x0000,
            crc32: &[0x463dc7ad],
        },
        RomEntry {
            name: "c-2f.bpr",
            size: 0x0100,
            offset: 0x0100,
            crc32: &[0x47ba0042],
        },
        RomEntry {
            name: "v-2n.bpr",
            size: 0x0100,
            offset: 0x0200,
            crc32: &[0xdbf185bf],
        },
    ],
};

// ---------------------------------------------------------------------------
// Input button IDs (same layout as DK: 4-way joystick + jump)
// ---------------------------------------------------------------------------
pub const INPUT_P1_RIGHT: u8 = 0;
pub const INPUT_P1_LEFT: u8 = 1;
pub const INPUT_P1_UP: u8 = 2;
pub const INPUT_P1_DOWN: u8 = 3;
pub const INPUT_P1_JUMP: u8 = 4;
pub const INPUT_P1_START: u8 = 5;
pub const INPUT_P2_START: u8 = 6;
pub const INPUT_COIN: u8 = 7;
pub const INPUT_P2_RIGHT: u8 = 8;
pub const INPUT_P2_LEFT: u8 = 9;
pub const INPUT_P2_UP: u8 = 10;
pub const INPUT_P2_DOWN: u8 = 11;
pub const INPUT_P2_JUMP: u8 = 12;

const DKONGJR_INPUT_MAP: &[InputButton] = &[
    InputButton {
        id: INPUT_P1_RIGHT,
        name: "P1 Right",
    },
    InputButton {
        id: INPUT_P1_LEFT,
        name: "P1 Left",
    },
    InputButton {
        id: INPUT_P1_UP,
        name: "P1 Up",
    },
    InputButton {
        id: INPUT_P1_DOWN,
        name: "P1 Down",
    },
    InputButton {
        id: INPUT_P1_JUMP,
        name: "P1 Jump",
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
    InputButton {
        id: INPUT_P2_RIGHT,
        name: "P2 Right",
    },
    InputButton {
        id: INPUT_P2_LEFT,
        name: "P2 Left",
    },
    InputButton {
        id: INPUT_P2_UP,
        name: "P2 Up",
    },
    InputButton {
        id: INPUT_P2_DOWN,
        name: "P2 Down",
    },
    InputButton {
        id: INPUT_P2_JUMP,
        name: "P2 Jump",
    },
];

// ---------------------------------------------------------------------------
// Donkey Kong Jr game wrapper
// ---------------------------------------------------------------------------

/// Donkey Kong Junior (Nintendo, 1982) on the shared DK hardware platform.
///
/// Key hardware differences from DK:
/// - 24KB program ROM (0x0000-0x5FFF) vs DK's 16KB
/// - 8KB tile ROM with gfx_bank select (256 extra tiles)
/// - 4KB sound ROM (no mirroring), no tune ROM
/// - Sound CPU MOVX reads sound latch directly (5 bits, no tune ROM banking)
/// - P2 virtual port: bit 6 from ls259.4h, bit 4 from dev_6h bit 6, XOR 0x70
/// - ls259.4h latch at 0x7C80-0x7C87 for sound/gfx control
#[derive(Saveable)]
pub struct DkongJrSystem {
    pub board: Tkg04Board,
}

impl Default for DkongJrSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl DkongJrSystem {
    pub fn new() -> Self {
        Self {
            board: Tkg04Board::new(0x1000), // 8KB tile ROM → plane 1 at 0x1000
        }
    }

    /// Load all ROM sets.
    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        // Try "dkongjr2" contiguous layout first, then fall back to the parent
        // "dkongjr" set which uses ROM_CONTINUE (non-contiguous loading).
        let rom_data = match DKONGJR_PROGRAM_ROM.load(rom_set) {
            Ok(data) => data,
            Err(_) => load_parent_program_rom(rom_set)?,
        };
        self.board.main_map.load_region(MainRegion::Rom, &rom_data);

        let sound_data = DKONGJR_SOUND_ROM.load(rom_set)?;
        self.board
            .sound_map
            .load_region(SoundRegion::Rom, &sound_data);
        // DK Jr has no tune ROM (board.tune_rom stays zeroed)

        let tile_data = DKONGJR_TILE_ROM.load(rom_set)?;
        self.board.tile_rom.copy_from_slice(&tile_data);

        let sprite_data = DKONGJR_SPRITE_ROM.load(rom_set)?;
        self.board.sprite_rom.copy_from_slice(&sprite_data);

        let prom_data = DKONGJR_PALETTE_PROMS.load(rom_set)?;
        self.board.palette_prom.copy_from_slice(&prom_data[..0x200]);
        self.board
            .color_prom
            .copy_from_slice(&prom_data[0x200..0x300]);

        self.board.build_palette();
        self.board.decode_gfx_roms();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Bus implementation (DK Jr-specific memory map)
// ---------------------------------------------------------------------------

impl Bus for DkongJrSystem {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        match master {
            // Main CPU (Z80)
            BusMaster::Cpu(0) => {
                let data = match self.board.main_map.page(addr).region_id {
                    MainRegion::ROM
                    | MainRegion::RAM
                    | MainRegion::SPRITE_RAM
                    | MainRegion::VIDEO_RAM => self.board.main_map.read_backing(addr),
                    MainRegion::IO_DMA if addr <= 0x7808 => self.board.dma.read(addr - 0x7800),
                    MainRegion::IO_PORTS => match addr {
                        0x7C00 => self.board.in0,
                        0x7C80 => self.board.in1,
                        0x7D00 => {
                            // IN2: DK Jr does not have the MCU line connected (bit 6 always 0)
                            self.board.in2 & !0x40
                        }
                        0x7D80 => self.board.dsw0,
                        _ => 0x00,
                    },
                    _ => 0x00,
                };
                self.board.main_map.watch_read(0, master, addr, data);
                data
            }

            // Sound CPU (I8035) - program memory
            BusMaster::Cpu(1) => self.board.sound_map.read_backing(addr & 0x0FFF),

            _ => 0x00,
        }
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        match master {
            BusMaster::Cpu(0) => {
                // Check the watchpoint before the side effect so the hit
                // records pre-write state (WatchpointPhase::Before).
                self.board.main_map.watch_write(0, master, addr, data);
                match self.board.main_map.page(addr).region_id {
                    MainRegion::RAM | MainRegion::SPRITE_RAM | MainRegion::VIDEO_RAM => {
                        self.board.main_map.write_backing(addr, data);
                    }
                    MainRegion::IO_DMA if addr <= 0x7808 => {
                        self.board.dma.write(addr - 0x7800, data);
                    }
                    MainRegion::IO_PORTS => match addr {
                        // Sound latch (ls174.3d)
                        0x7C00 => self.board.sound_latch = data,

                        // ls259.4h latch (0x7C80-0x7C87): sound/gfx control
                        0x7C80..=0x7C87 => {
                            let bit = (addr & 0x07) as u8;
                            self.board.sound_control_latch_4h.write(bit, data & 1 != 0);
                            // Bit 0 of ls259.4h is also the gfx bank select
                            if bit == 0 {
                                self.board.gfx_bank = data & 1;
                            }
                        }

                        // 74LS259 sound control latch (dev_6h): addr bits 0-2 select bit
                        0x7D00..=0x7D07 => {
                            let bit = (addr & 0x07) as u8;
                            self.board.write_sound_control_bit(bit, data & 1 != 0);
                        }

                        // ls259.5h latch (0x7D80-0x7D87)
                        // 0x7D80 also triggers sound CPU IRQ
                        0x7D80 => {
                            self.board.sound_irq_pending = data != 0;
                        }

                        0x7D82 => self.board.flip_screen = (data & 1) != 0,
                        0x7D83 => self.board.sprite_bank = (data & 1) != 0,
                        0x7D84 => {
                            self.board.nmi_mask = (data & 1) != 0;
                            if !self.board.nmi_mask {
                                self.board.vblank_nmi_pending = false;
                            }
                        }
                        0x7D85 => self.board.trigger_sprite_dma(),
                        0x7D86 => {
                            if data & 1 != 0 {
                                self.board.palette_bank |= 0x01;
                            } else {
                                self.board.palette_bank &= !0x01;
                            }
                        }
                        0x7D87 => {
                            if data & 1 != 0 {
                                self.board.palette_bank |= 0x02;
                            } else {
                                self.board.palette_bank &= !0x02;
                            }
                        }

                        _ => {}
                    },
                    _ => {} // ROM or unmapped: ignored
                }
            }

            // Sound CPU writes to program memory are ignored
            BusMaster::Cpu(1) => {}

            _ => {}
        }
    }

    fn io_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        match master {
            // Main CPU: no I/O port reads used
            BusMaster::Cpu(0) => 0xFF,

            // Sound CPU I/O
            BusMaster::Cpu(1) => match addr {
                // MOVX/INS A,BUS: read sound latch directly (ls174.3d, 5 bits)
                // DK Jr has no tune ROM — MOVX always reads the sound latch.
                // ls174.3d maskout=0xe0 → only bits 0-4 are valid.
                0x00..=0x100 => self.board.sound_latch & 0x1F,

                // IN A,P1: read P1 latch
                0x101 => self.board.sound_cpu.p1,

                // IN A,P2: virtual port (m_dev_vp2) with XOR 0x70
                // Bit 6: from ls259.4h bit 1
                // Bit 5: from dev_6h (sound_control_latch) bit 3
                // Bit 4: from dev_6h bit 6
                // Then XOR with 0x70 (invert bits 4,5,6)
                0x102 => {
                    let mut val = self.board.sound_cpu.p2;
                    // Bit 6: from ls259.4h bit 1
                    val = (val & !0x40)
                        | if self.board.sound_control_latch_4h.bit(1) {
                            0x40
                        } else {
                            0x00
                        };
                    // Bit 5: from sound_control_latch (dev_6h) bit 3
                    val = (val & !0x20)
                        | if self.board.sound_control_latch.bit(3) {
                            0x20
                        } else {
                            0x00
                        };
                    // Bit 4: from sound_control_latch (dev_6h) bit 6
                    val = (val & !0x10)
                        | if self.board.sound_control_latch.bit(6) {
                            0x10
                        } else {
                            0x00
                        };
                    val ^ 0x70 // XOR with 0x70 (invert bits 4,5,6)
                }

                // T0: inverted bit 5 of sound control latch (same as DK)
                0x110 => u8::from(!self.board.sound_control_latch.bit(5)),

                // T1: inverted bit 4 of sound control latch (same as DK)
                0x111 => u8::from(!self.board.sound_control_latch.bit(4)),

                _ => 0xFF,
            },

            _ => 0xFF,
        }
    }

    fn io_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        match master {
            BusMaster::Cpu(0) => {}

            BusMaster::Cpu(1) => match addr {
                // OUTL P1,A: DAC output
                0x101 => self.board.dac.write(data),

                // OUTL P2,A: control port (tracked by I8035 internally)
                0x102 => {}

                _ => {}
            },

            _ => {}
        }
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        self.board.check_interrupts(target)
    }
}

// ---------------------------------------------------------------------------
// Machine implementation
// ---------------------------------------------------------------------------

crate::impl_board_delegation!(DkongJrSystem, board, tkg04::TIMING);

impl InputReceiver for DkongJrSystem {
    fn set_input(&mut self, button: u8, pressed: bool) {
        match button {
            INPUT_P1_RIGHT => set_bit_active_high(&mut self.board.in0, 0, pressed),
            INPUT_P1_LEFT => set_bit_active_high(&mut self.board.in0, 1, pressed),
            INPUT_P1_UP => set_bit_active_high(&mut self.board.in0, 2, pressed),
            INPUT_P1_DOWN => set_bit_active_high(&mut self.board.in0, 3, pressed),
            INPUT_P1_JUMP => set_bit_active_high(&mut self.board.in0, 4, pressed),

            INPUT_P2_RIGHT => set_bit_active_high(&mut self.board.in1, 0, pressed),
            INPUT_P2_LEFT => set_bit_active_high(&mut self.board.in1, 1, pressed),
            INPUT_P2_UP => set_bit_active_high(&mut self.board.in1, 2, pressed),
            INPUT_P2_DOWN => set_bit_active_high(&mut self.board.in1, 3, pressed),
            INPUT_P2_JUMP => set_bit_active_high(&mut self.board.in1, 4, pressed),

            INPUT_P1_START => set_bit_active_high(&mut self.board.in2, 2, pressed),
            INPUT_P2_START => set_bit_active_high(&mut self.board.in2, 3, pressed),
            INPUT_COIN => set_bit_active_high(&mut self.board.in2, 7, pressed),

            _ => {}
        }
    }

    fn input_map(&self) -> &[InputButton] {
        DKONGJR_INPUT_MAP
    }
}

impl MachineCore for DkongJrSystem {
    crate::machine_core_metadata!("dkongjr", tkg04::TIMING);

    fn run_frame(&mut self) {
        bus_split!(self, bus => {
            for _ in 0..tkg04::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });
    }

    fn reset(&mut self) {
        self.board.reset();
        self.board.dsw0 = 0x80; // upright cabinet, 3 lives, 10000 bonus, 1 coin/1 play
        bus_split!(self, bus => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
            self.board.sound_cpu.reset(bus, BusMaster::Cpu(1));
        });
    }
}

impl SaveState for DkongJrSystem {
    crate::machine_save_state!();
}

crate::impl_default_frontend_capabilities!(DkongJrSystem);

// ---------------------------------------------------------------------------
// Parent "dkongjr" ROM set support (ROM_CONTINUE layout)
// ---------------------------------------------------------------------------

/// Load program ROMs from the parent "dkongjr" MAME set.
///
/// The parent set uses ROM_CONTINUE for non-contiguous loading:
/// - djr1-c_5b_f-2.5b (16KB, CRC 0xdea28158): first 8KB → 0x0000, next 8KB → 0x4000
/// - djr1-c_5c_f-2.5c (8KB,  CRC 0x6a9a2e6f): → 0x2000
fn load_parent_program_rom(rom_set: &RomSet) -> Result<Vec<u8>, RomLoadError> {
    const PARENT_5B_CRC: u32 = 0xdea28158;
    const PARENT_5C_CRC: u32 = 0x6a9a2e6f;

    let rom_5b = rom_set
        .find_by_crc32(PARENT_5B_CRC, 0x4000)
        .map(|(_, data)| data)
        .ok_or_else(|| RomLoadError::MissingFile("djr1-c_5b_f-2.5b".to_string()))?;
    let rom_5c = rom_set
        .find_by_crc32(PARENT_5C_CRC, 0x2000)
        .map(|(_, data)| data)
        .ok_or_else(|| RomLoadError::MissingFile("djr1-c_5c_f-2.5c".to_string()))?;

    let mut region = vec![0u8; 0x6000];
    region[0x0000..0x2000].copy_from_slice(&rom_5b[..0x2000]);
    region[0x2000..0x4000].copy_from_slice(rom_5c);
    region[0x4000..0x6000].copy_from_slice(&rom_5b[0x2000..]);
    Ok(region)
}

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = DkongJrSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("dkongjr", &["dkongjr", "dkongjr2"], create_machine)
}
