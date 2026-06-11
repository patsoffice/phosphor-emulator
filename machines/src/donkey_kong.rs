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
// Donkey Kong ROM definitions (TKG-04 / "dkong" MAME set)
// ---------------------------------------------------------------------------

/// Main CPU program ROMs: 16KB at 0x0000-0x3FFF (four 4KB chips).
pub static DKONG_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "c_5et_g.bin",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xba70b88b],
        },
        RomEntry {
            name: "c_5ct_g.bin",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x5ec461ec],
        },
        RomEntry {
            name: "c_5bt_g.bin",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0x1c97d324],
        },
        RomEntry {
            name: "c_5at_g.bin",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0xb9005ac0],
        },
    ],
};

/// Sound CPU ROM: 2KB at 0x0000-0x07FF, mirrored to 0x0800-0x0FFF.
pub static DKONG_SOUND_ROM: RomRegion = RomRegion {
    size: 0x0800,
    entries: &[RomEntry {
        name: "s_3i_b.bin",
        size: 0x0800,
        offset: 0x0000,
        crc32: &[0x45a4ed06],
    }],
};

/// Tune ROM: 2KB, accessed via MOVX with P2 bank select.
pub static DKONG_TUNE_ROM: RomRegion = RomRegion {
    size: 0x0800,
    entries: &[RomEntry {
        name: "s_3j_b.bin",
        size: 0x0800,
        offset: 0x0000,
        crc32: &[0x4743fe92],
    }],
};

/// Tile GFX: 4KB (two 2KB ROMs, one per bitplane).
pub static DKONG_TILE_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[
        RomEntry {
            name: "v_5h_b.bin",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0x12c8c95d],
        },
        RomEntry {
            name: "v_3pt.bin",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x15e9c5e9],
        },
    ],
};

/// Sprite GFX: 8KB (four 2KB ROMs, interleaved).
pub static DKONG_SPRITE_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "l_4m_b.bin",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0x59f8054d],
        },
        RomEntry {
            name: "l_4n_b.bin",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x672e4714],
        },
        RomEntry {
            name: "l_4r_b.bin",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0xfeaa59ee],
        },
        RomEntry {
            name: "l_4s_b.bin",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0x20f2ef7e],
        },
    ],
};

/// Palette PROMs: c-2k (256B), c-2j (256B), v-5e (256B color codes).
pub static DKONG_PALETTE_PROMS: RomRegion = RomRegion {
    size: 0x0300,
    entries: &[
        RomEntry {
            name: "c-2k.bpr",
            size: 0x0100,
            offset: 0x0000,
            crc32: &[0xe273ede5],
        },
        RomEntry {
            name: "c-2j.bpr",
            size: 0x0100,
            offset: 0x0100,
            crc32: &[0xd6412358],
        },
        RomEntry {
            name: "v-5e.bpr",
            size: 0x0100,
            offset: 0x0200,
            crc32: &[0xb869b8f5],
        },
    ],
};

// ---------------------------------------------------------------------------
// Input button IDs (active-high: 0x00 = all released)
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

const DKONG_INPUT_MAP: &[InputButton] = &[
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
// Donkey Kong game wrapper
// ---------------------------------------------------------------------------

/// Donkey Kong (Nintendo, 1981) on the shared DK hardware platform.
#[derive(Saveable)]
pub struct DkongSystem {
    pub board: Tkg04Board,
}

impl Default for DkongSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl DkongSystem {
    pub fn new() -> Self {
        Self {
            board: Tkg04Board::new(0x800), // 4KB tile ROM → plane 1 at 0x800
        }
    }

    /// Load all ROM sets.
    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let rom_data = DKONG_PROGRAM_ROM.load(rom_set)?;
        self.board
            .main_map
            .load_region_at(MainRegion::Rom, 0, &rom_data);

        let sound_data = DKONG_SOUND_ROM.load(rom_set)?;
        self.board
            .sound_map
            .load_region_at(SoundRegion::Rom, 0, &sound_data);
        self.board
            .sound_map
            .load_region_at(SoundRegion::Rom, 0x0800, &sound_data); // mirror

        let tune_data = DKONG_TUNE_ROM.load(rom_set)?;
        self.board.tune_rom.copy_from_slice(&tune_data);

        let tile_data = DKONG_TILE_ROM.load(rom_set)?;
        self.board.tile_rom[..0x1000].copy_from_slice(&tile_data);

        let sprite_data = DKONG_SPRITE_ROM.load(rom_set)?;
        self.board.sprite_rom.copy_from_slice(&sprite_data);

        let prom_data = DKONG_PALETTE_PROMS.load(rom_set)?;
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
// Bus implementation (DK-specific memory map)
// ---------------------------------------------------------------------------

impl Bus for DkongSystem {
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
                            // IN2: active-high inputs + sound status at bit 6
                            let sound_status = if self.board.sound_cpu.p2 & 0x10 != 0 {
                                0x00
                            } else {
                                0x40
                            };
                            (self.board.in2 & !0x40) | sound_status
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
                match self.board.main_map.page(addr).region_id {
                    MainRegion::RAM | MainRegion::SPRITE_RAM | MainRegion::VIDEO_RAM => {
                        self.board.main_map.write_backing(addr, data);
                    }
                    MainRegion::IO_DMA if addr <= 0x7808 => {
                        self.board.dma.write(addr - 0x7800, data);
                    }
                    MainRegion::IO_PORTS => match addr {
                        // Sound latch (ls175.3d)
                        0x7C00 => self.board.sound_latch = data,

                        // 74LS259 sound control latch: addr bits 0-2 select bit
                        0x7D00..=0x7D07 => {
                            let bit = (addr & 0x07) as u8;
                            self.board.write_sound_control_bit(bit, data & 1 != 0);
                        }

                        // Sound CPU IRQ trigger
                        0x7D80 => {
                            self.board.sound_irq_pending = data != 0;
                        }

                        // Flip screen
                        0x7D82 => self.board.flip_screen = (data & 1) != 0,

                        // Sprite bank select
                        0x7D83 => self.board.sprite_bank = (data & 1) != 0,

                        // NMI mask
                        0x7D84 => {
                            self.board.nmi_mask = (data & 1) != 0;
                            if !self.board.nmi_mask {
                                self.board.vblank_nmi_pending = false;
                            }
                        }

                        // DMA DRQ: trigger sprite DMA transfer from i8257 channel 0
                        0x7D85 => self.board.trigger_sprite_dma(),

                        // Palette bank (2-bit, one bit per address)
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
                self.board.main_map.watch_write(0, master, addr, data);
            }

            // Sound CPU writes to program memory are ignored
            BusMaster::Cpu(1) => {}

            _ => {}
        }
    }

    fn io_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        match master {
            // Main CPU: no I/O port reads used on DK
            BusMaster::Cpu(0) => 0xFF,

            // Sound CPU I/O
            BusMaster::Cpu(1) => match addr {
                // MOVX and INS A,BUS: dkong_tune_r logic
                0x00..=0x100 => {
                    if self.board.sound_cpu.p2 & 0x40 != 0 {
                        // Command mode: read sound latch (lower 4 bits, inverted by ls175.3d)
                        (self.board.sound_latch & 0x0F) ^ 0x0F
                    } else {
                        // Tune ROM mode: bank select from P2 bits 2-0
                        let bank = (self.board.sound_cpu.p2 & 0x07) as usize;
                        let offset = (addr & 0xFF) as usize;
                        let rom_addr = bank * 256 + offset;
                        if rom_addr < self.board.tune_rom.len() {
                            self.board.tune_rom[rom_addr]
                        } else {
                            0xFF
                        }
                    }
                }

                // IN A,P1: read P1 latch
                0x101 => self.board.sound_cpu.p1,

                // IN A,P2: virtual port with bit 5 from sound control latch bit 3 (XOR'd)
                0x102 => {
                    let mut val = self.board.sound_cpu.p2;
                    val = (val & !0x20)
                        | if self.board.sound_control_latch.bit(3) {
                            0x20
                        } else {
                            0x00
                        };
                    val ^ 0x20
                }

                // T0: inverted bit 5 of sound control latch
                0x110 => u8::from(!self.board.sound_control_latch.bit(5)),

                // T1: inverted bit 4 of sound control latch
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

crate::impl_board_delegation!(DkongSystem, board, tkg04::TIMING);

impl InputReceiver for DkongSystem {
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
        DKONG_INPUT_MAP
    }
}

impl MachineCore for DkongSystem {
    crate::machine_core_metadata!("dkong", tkg04::TIMING);

    fn run_frame(&mut self) {
        bus_split!(self, bus => {
            for _ in 0..tkg04::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });
    }

    fn reset(&mut self) {
        self.board.reset();
        self.board.dsw0 = 0x80; // upright cabinet, 3 lives, 7000 bonus, 1 coin/1 play
        bus_split!(self, bus => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
            self.board.sound_cpu.reset(bus, BusMaster::Cpu(1));
        });
    }
}

impl SaveState for DkongSystem {
    crate::machine_save_state!();
}

crate::impl_default_frontend_capabilities!(DkongSystem);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = DkongSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("dkong", &["dkong"], create_machine)
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
        let mut sys = DkongSystem::new();

        // Set known state
        sys.board.main_map.region_data_mut(MainRegion::Ram)[0x100] = 0xAA;
        sys.board.main_map.region_data_mut(MainRegion::SpriteRam)[0x50] = 0xBB;
        sys.board.main_map.region_data_mut(MainRegion::VideoRam)[0x100] = 0xCC;
        sys.board.in0 = 0x1F;
        sys.board.in1 = 0x0F;
        sys.board.in2 = 0x8C;
        sys.board.sound_latch = 0x42;
        // Set sound_control_latch to 0x33 via bit writes
        for bit in 0..8u8 {
            sys.board
                .write_sound_control_bit(bit, (0x33 >> bit) & 1 != 0);
        }
        sys.board.flip_screen = true;
        sys.board.sprite_bank = true;
        sys.board.nmi_mask = true;
        sys.board.palette_bank = 2;
        sys.board.sound_irq_pending = true;
        sys.board.clock = 200_000;
        sys.board.sound_clock.set_phase(150);
        sys.board.vblank_nmi_pending = true;

        // Save
        let data = sys.save_state().expect("save_state should return Some");
        let cpu_snap = sys.board.cpu.snapshot();
        let sound_snap = sys.board.sound_cpu.snapshot();

        // Mutate everything
        let mut sys2 = DkongSystem::new();
        sys2.board.main_map.region_data_mut(MainRegion::Ram)[0x100] = 0xFF;
        sys2.board.clock = 999;

        // Load
        sys2.load_state(&data).unwrap();

        // Verify CPUs
        assert_eq!(sys2.board.cpu.snapshot(), cpu_snap);
        assert_eq!(sys2.board.sound_cpu.snapshot(), sound_snap);

        // Verify memory
        assert_eq!(
            sys2.board.main_map.region_data(MainRegion::Ram)[0x100],
            0xAA
        );
        assert_eq!(
            sys2.board.main_map.region_data(MainRegion::SpriteRam)[0x50],
            0xBB
        );
        assert_eq!(
            sys2.board.main_map.region_data(MainRegion::VideoRam)[0x100],
            0xCC
        );

        // Verify I/O and control
        assert_eq!(sys2.board.in0, 0x1F);
        assert_eq!(sys2.board.in1, 0x0F);
        assert_eq!(sys2.board.in2, 0x8C);
        assert_eq!(sys2.board.sound_latch, 0x42);
        assert_eq!(sys2.board.sound_control_latch.value(), 0x33);
        assert!(sys2.board.flip_screen);
        assert!(sys2.board.sprite_bank);
        assert!(sys2.board.nmi_mask);
        assert_eq!(sys2.board.palette_bank, 2);
        assert!(sys2.board.sound_irq_pending);
        assert_eq!(sys2.board.clock, 200_000);
        assert_eq!(sys2.board.sound_clock.phase(), 150);
        assert!(sys2.board.vblank_nmi_pending);
    }

    #[test]
    fn save_does_not_include_rom() {
        let mut sys = DkongSystem::new();
        sys.board.main_map.region_data_mut(MainRegion::Rom)[0] = 0xDE;
        sys.board.sound_map.region_data_mut(SoundRegion::Rom)[0] = 0xAD;
        sys.board.tile_rom[0] = 0xBE;
        sys.board.sprite_rom[0] = 0xEF;

        let data = sys.save_state().unwrap();

        let mut sys2 = DkongSystem::new();
        sys2.load_state(&data).unwrap();

        assert_eq!(sys2.board.main_map.region_data(MainRegion::Rom)[0], 0x00);
        assert_eq!(sys2.board.sound_map.region_data(SoundRegion::Rom)[0], 0x00);
        assert_eq!(sys2.board.tile_rom[0], 0x00);
        assert_eq!(sys2.board.sprite_rom[0], 0x00);
    }
}
