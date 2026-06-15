use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    ActionRole, AudioSource, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, DipSwitches,
    FrontendMachine, InputConfigurable, InputControl, InputEvent, InputId, InputKind, MachineCore,
    Nvram, Profilable, SaveState,
};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::device::Er2055;
use phosphor_core::device::pokey::Pokey;
use phosphor_macros::Saveable;

use crate::atari_dvg::{self, AtariDvgBoard, Region};
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_high;

// ---------------------------------------------------------------------------
// ROM definitions (MAME `astdelux` set, revision 3)
// ---------------------------------------------------------------------------

/// Program ROM: 8KB at CPU addresses 0x6000–0x7FFF.
static PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "036430-02.d1",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0xa4d7a525],
        },
        RomEntry {
            name: "036431-02.ef1",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0xd4004aae],
        },
        RomEntry {
            name: "036432-02.fh1",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0x6d720c41],
        },
        RomEntry {
            name: "036433-03.j1",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0x0dcc0be6],
        },
    ],
};

/// Vector ROM: 4KB at CPU addresses 0x4800–0x57FF.
static VECTOR_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[
        RomEntry {
            name: "036800-02.r2",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0xbb8cabe1],
        },
        RomEntry {
            name: "036799-01.np2",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x7d511572],
        },
    ],
};

// ---------------------------------------------------------------------------
// Input button IDs
// ---------------------------------------------------------------------------

pub const INPUT_COIN: u8 = 0;
pub const INPUT_START1: u8 = 1;
pub const INPUT_START2: u8 = 2;
pub const INPUT_THRUST: u8 = 3;
pub const INPUT_FIRE: u8 = 4;
pub const INPUT_SHIELD: u8 = 5;
pub const INPUT_ROT_LEFT: u8 = 6;
pub const INPUT_ROT_RIGHT: u8 = 7;

/// Typed logical controls. `InputId`s reuse the `INPUT_*` numbering; default
/// bindings mirror the legacy name-matched defaults (thrust = "P1 Up", shield =
/// "P1 Jump", rotate = "P1 Left/Right"). Fire is the primary action (gamepad A);
/// shield is key-only.
const ASTDELUX_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_COIN as u16),
        stable_name: "coin",
        label: "Coin",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
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
        id: InputId(INPUT_THRUST as u16),
        stable_name: "thrust",
        label: "Thrust",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_UP,
    },
    InputControl {
        id: InputId(INPUT_FIRE as u16),
        stable_name: "fire",
        label: "Fire",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_SHIELD as u16),
        stable_name: "shield",
        label: "Shield",
        kind: InputKind::Action(ActionRole::Secondary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_ROT_LEFT as u16),
        stable_name: "rotate_left",
        label: "Rotate Left",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(INPUT_ROT_RIGHT as u16),
        stable_name: "rotate_right",
        label: "Rotate Right",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
];

// ---------------------------------------------------------------------------
// AsteroidsDeluxeSystem — Atari DVG board configured for Asteroids Deluxe (1980)
// ---------------------------------------------------------------------------

/// Asteroids Deluxe-specific wrapper around the shared Atari DVG board.
///
/// Adds POKEY sound chip at 0x2C00 and EAROM (ER2055) for high score storage.
///
/// Memory map (15-bit address bus, `addr & 0x7FFF`):
///   0x0000–0x03FF  RAM (1 KB)
///   0x2000–0x2007  IN0 read (buttons, 3 KHz clock, VG_HALT)
///   0x2400–0x2407  IN1 read (coins, start, thrust, rotate)
///   0x2800–0x2803  DSW1 read (DIP switches)
///   0x2C00–0x2C0F  POKEY read/write
///   0x2C40–0x2C7F  EAROM data read
///   0x3000         DVG GO write
///   0x3200–0x323F  EAROM data/address write
///   0x3400         Watchdog reset write
///   0x3600         Explosion sound write
///   0x3A00         EAROM control write
///   0x3C00–0x3C07  Audio latch write (74LS259)
///   0x3E00         Noise reset write
///   0x4000–0x47FF  Vector RAM (2 KB, shared CPU/DVG)
///   0x4800–0x57FF  Vector ROM (4 KB)
///   0x6000–0x7FFF  Program ROM (8 KB)
#[derive(Saveable)]
pub struct AsteroidsDeluxeSystem {
    pub board: AtariDvgBoard,

    // POKEY sound chip at 0x2C00–0x2C0F
    pokey: Pokey,

    // I/O — active-HIGH inputs (default 0x00 = all released)
    in0: u8,
    in1: u8,
    /// DIP switches (R5): default 0x00 (English, 2-4 ships, easy, 10K bonus).
    #[save_skip]
    dip_switches: u8,

    // EAROM (ER2055): 64-byte non-volatile RAM for high scores
    earom: Er2055,

    // Audio buffer from POKEY
    #[save_skip(default)]
    audio_buffer: Vec<i16>,
}

impl AsteroidsDeluxeSystem {
    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(Region::Ram, "RAM", 0x0000, 0x0400, AccessKind::ReadWrite)
            .region(Region::Io, "I/O", 0x2000, 0x2000, AccessKind::Io)
            .region(
                Region::VectorRam,
                "Vector RAM",
                0x4000,
                0x0800,
                AccessKind::ReadWrite,
            )
            .region(
                Region::VectorRom,
                "Vector ROM",
                0x4800,
                0x1000,
                AccessKind::ReadOnly,
            )
            .region(
                Region::ProgramRom,
                "Program ROM",
                0x6000,
                0x2000,
                AccessKind::ReadOnly,
            );
        map
    }

    pub fn new() -> Self {
        Self {
            // Asteroids Deluxe: VROM at DVG 0x0800, size 0x1000
            board: AtariDvgBoard::new(Self::build_map(), 0x0800, 0x1000),
            pokey: Pokey::with_clock(1_512_000, 44100),
            in0: 0x00,
            in1: 0x00,
            dip_switches: 0x00,
            earom: Er2055::new(),
            audio_buffer: Vec::with_capacity(1024),
        }
    }

    fn debug_pre_tick(&mut self) {
        self.pokey.tick();
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let prog = PROGRAM_ROM.load(rom_set)?;
        self.board.map.load_region(Region::ProgramRom, &prog);
        let vrom = VECTOR_ROM.load(rom_set)?;
        self.board.map.load_region(Region::VectorRom, &vrom);
        Ok(())
    }
}

impl Default for AsteroidsDeluxeSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus implementation
// ---------------------------------------------------------------------------

impl Bus for AsteroidsDeluxeSystem {
    type Address = u16;
    type Data = u8;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let addr = addr & 0x7FFF; // 15-bit address bus

        let data = match self.board.map.page(addr).region_id {
            Region::RAM | Region::VECTOR_RAM | Region::VECTOR_ROM | Region::PROGRAM_ROM => {
                self.board.map.read_backing(addr)
            }

            Region::IO => match addr {
                // IN0: 0x2000–0x2007 — 74LS251 8:1 multiplexer (same as Asteroids).
                //   Bit 0: unused     Bit 1: 3 KHz clock     Bit 2: VG_HALT
                //   Bit 3: Shield     Bit 4: Fire
                //   Bit 5: Diagnostic Bit 6: Tilt     Bit 7: Self-test
                0x2000..=0x2007 => {
                    let offset = (addr & 7) as u8;
                    let mut val = self.in0;
                    if self.board.clock & 0x100 != 0 {
                        val |= 0x02;
                    } else {
                        val &= !0x02;
                    }
                    if !self.board.dvg.is_halted() {
                        val |= 0x04;
                    } else {
                        val &= !0x04;
                    }
                    ((val >> offset) & 1) << 7
                }

                // IN1: 0x2400–0x2407 — 74LS251 8:1 multiplexer (same as Asteroids).
                0x2400..=0x2407 => {
                    let offset = (addr & 7) as u8;
                    ((self.in1 >> offset) & 1) << 7
                }

                // DSW1: 0x2800–0x2803 — 74LS253 dual 4:1 multiplexer (same as Asteroids).
                0x2800..=0x2803 => {
                    let offset = (addr & 3) as u8;
                    let bit0 = (self.dip_switches >> (offset * 2)) & 1;
                    let bit7 = (self.dip_switches >> (offset * 2 + 1)) & 1;
                    bit0 | (bit7 << 7)
                }

                // POKEY: 0x2C00–0x2C0F
                0x2C00..=0x2C0F => self.pokey.read(addr & 0x0F),

                // EAROM data read: 0x2C40–0x2C7F
                0x2C40..=0x2C7F => self.earom.read(addr & 0x3F),

                _ => 0,
            },

            _ => 0,
        };

        self.board.map.watch_read(0, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        let addr = addr & 0x7FFF; // 15-bit address bus

        self.board.map.watch_write(0, master, addr, data);
        self.board.trace_main_write(addr, data);

        match self.board.map.page(addr).region_id {
            Region::RAM | Region::VECTOR_RAM => self.board.map.write_backing(addr, data),

            Region::IO => match addr {
                // POKEY: 0x2C00–0x2C0F
                0x2C00..=0x2C0F => self.pokey.write(addr & 0x0F, data),

                0x3000 => self.board.trigger_dvg(),

                // EAROM data/address write: 0x3200–0x323F
                // Offset selects 6-bit address, data byte is the value to write.
                0x3200..=0x323F => self.earom.latch(addr & 0x3F, data),

                0x3400 => self.board.watchdog_frame_count = 0,
                0x3600 => { /* explosion sound stub */ }

                // EAROM control: 0x3A00
                // Bit 0: CK (clock), Bit 1: !C1, Bit 2: C2, Bit 3: CS1
                0x3A00 => {
                    let clock = data & 0x01 != 0;
                    let c1 = data & 0x02 == 0; // bit 1 inverted
                    let c2 = data & 0x04 != 0; // bit 2
                    let cs1 = data & 0x08 != 0;
                    self.earom.write_control(clock, cs1, c1, c2);
                }

                0x3C00..=0x3C07 => { /* audio latch stub */ }
                0x3E00 => { /* noise reset stub */ }
                _ => {}
            },

            _ => {}
        }
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            nmi: self.board.nmi_pending,
            irq: self.pokey.irq(),
            firq: false,
            irq_vector: 0,
            irq_level: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Machine traits (MachineCore + capabilities)
// ---------------------------------------------------------------------------

crate::impl_board_renderable!(AsteroidsDeluxeSystem, board, atari_dvg::TIMING, vectors);

impl AudioSource for AsteroidsDeluxeSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        let n = buffer.len().min(self.audio_buffer.len());
        buffer[..n].copy_from_slice(&self.audio_buffer[..n]);
        self.audio_buffer.drain(..n);
        n
    }

    fn audio_sample_rate(&self) -> u32 {
        44100
    }
}

impl InputConfigurable for AsteroidsDeluxeSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        ASTDELUX_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let InputEvent::Button { id, pressed } = event else {
            return;
        };
        match id.0 as u8 {
            // IN1 (active-HIGH)
            INPUT_COIN => set_bit_active_high(&mut self.in1, 0, pressed),
            INPUT_START1 => set_bit_active_high(&mut self.in1, 3, pressed),
            INPUT_START2 => set_bit_active_high(&mut self.in1, 4, pressed),
            INPUT_THRUST => set_bit_active_high(&mut self.in1, 5, pressed),
            INPUT_ROT_RIGHT => set_bit_active_high(&mut self.in1, 6, pressed),
            INPUT_ROT_LEFT => set_bit_active_high(&mut self.in1, 7, pressed),

            // IN0 (active-HIGH)
            INPUT_FIRE => set_bit_active_high(&mut self.in0, 4, pressed),
            INPUT_SHIELD => set_bit_active_high(&mut self.in0, 3, pressed),

            _ => {}
        }
    }
}

crate::impl_board_debug!(
    AsteroidsDeluxeSystem,
    board,
    atari_dvg::TIMING,
    debug_tick_pre
);

impl MachineCore for AsteroidsDeluxeSystem {
    crate::machine_core_metadata!("astdelux", atari_dvg::TIMING);

    fn run_frame(&mut self) {
        bus_split!(self, bus => {
            for _ in 0..atari_dvg::TIMING.cycles_per_frame() {
                self.pokey.tick();
                self.board.tick(bus);
            }
        });

        // Drain POKEY audio samples
        let samples = self.pokey.drain_audio();
        self.audio_buffer
            .extend(samples.iter().map(|&s| (s * 32767.0) as i16));

        // Clear NMI at frame boundary to avoid stale edges.
        self.board.nmi_pending = false;

        // Watchdog
        self.board.watchdog_frame_count += 1;
        if self.board.watchdog_frame_count >= 8 {
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.board.reset();
        self.pokey.reset();
        bus_split!(self, bus => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
        });
    }
}

impl SaveState for AsteroidsDeluxeSystem {
    crate::machine_save_state!();
}

impl Nvram for AsteroidsDeluxeSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.earom.snapshot())
    }

    fn load_nvram(&mut self, data: &[u8]) {
        self.earom.load_from(data);
    }
}

impl Profilable for AsteroidsDeluxeSystem {}
/// DIP switch metadata for Asteroids Deluxe's game-options DIP (the R5 bank
/// read through the 74LS253 mux at 0x2800-0x2803; the separate coinage bank is
/// not emulated). Choice bits and labels follow MAME's `astdelux` R5 layout;
/// option defaults OR to the historical 0x00.
const ASTDELUX_DIP_BANKS: &[DipSwitchBank] = &[DipSwitchBank {
    name: "DSW1",
    options: &[
        DipOption {
            name: "Language",
            mask: 0x03,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "English",
                    value: 0x00,
                },
                DipChoice {
                    label: "German",
                    value: 0x01,
                },
                DipChoice {
                    label: "French",
                    value: 0x02,
                },
                DipChoice {
                    label: "Spanish",
                    value: 0x03,
                },
            ],
        },
        DipOption {
            name: "Lives",
            mask: 0x0C,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "2-4",
                    value: 0x00,
                },
                DipChoice {
                    label: "3-5",
                    value: 0x04,
                },
                DipChoice {
                    label: "4-6",
                    value: 0x08,
                },
                DipChoice {
                    label: "5-7",
                    value: 0x0C,
                },
            ],
        },
        DipOption {
            name: "Minimum Plays",
            mask: 0x10,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "1",
                    value: 0x00,
                },
                DipChoice {
                    label: "2",
                    value: 0x10,
                },
            ],
        },
        DipOption {
            name: "Difficulty",
            mask: 0x20,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "Easy",
                    value: 0x00,
                },
                DipChoice {
                    label: "Hard",
                    value: 0x20,
                },
            ],
        },
        DipOption {
            name: "Bonus Life",
            mask: 0xC0,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "10000",
                    value: 0x00,
                },
                DipChoice {
                    label: "12000",
                    value: 0x40,
                },
                DipChoice {
                    label: "15000",
                    value: 0x80,
                },
                DipChoice {
                    label: "None",
                    value: 0xC0,
                },
            ],
        },
    ],
}];

impl DipSwitches for AsteroidsDeluxeSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        ASTDELUX_DIP_BANKS
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        if bank == 0 { self.dip_switches } else { 0 }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        if bank == 0 {
            self.dip_switches = value;
        }
    }
}
crate::impl_board_debug_trace!(AsteroidsDeluxeSystem, board);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

fn create_machine(rom_set: &RomSet) -> Result<Box<dyn FrontendMachine>, RomLoadError> {
    let mut sys = AsteroidsDeluxeSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("astdelux", &["astdelux"], create_machine)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atari_dvg::Region;
    use phosphor_core::cpu::CpuStateTrait;

    #[test]
    fn dip_default_and_metadata() {
        let sys = AsteroidsDeluxeSystem::new();
        assert_eq!(sys.dip_bank_value(0), 0x00);
        crate::assert_dip_banks_valid(sys.dip_banks(), &[sys.dip_bank_value(0)]);
    }

    #[test]
    fn set_dip_option_masks_only_its_bits() {
        let mut sys = AsteroidsDeluxeSystem::new();
        // Bonus Life is option 4 (mask 0xC0); pick "None" (0xC0).
        sys.set_dip_option(0, 4, 0xC0);
        assert_eq!(sys.dip_bank_value(0), 0xC0);
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = AsteroidsDeluxeSystem::new();

        // Set known state
        sys.board.map.region_data_mut(Region::Ram)[0x100] = 0xAA;
        sys.board.map.region_data_mut(Region::VectorRam)[0x200] = 0xBB;
        sys.in0 = 0x18;
        sys.in1 = 0xE8;
        sys.board.clock = 75_000;
        sys.board.nmi_counter = 3000;
        sys.board.nmi_pending = true;
        sys.board.watchdog_frame_count = 5;
        sys.earom.load_from(&{
            let mut d = [0u8; 64];
            d[0] = 0x42;
            d[63] = 0xEF;
            d
        });

        // Save
        let data = sys.save_state().expect("save_state should return Some");
        let cpu_snap = sys.board.cpu.snapshot();

        // Mutate everything
        let mut sys2 = AsteroidsDeluxeSystem::new();
        sys2.board.map.region_data_mut(Region::Ram)[0x100] = 0xFF;
        sys2.in0 = 0xFF;
        sys2.board.clock = 999;

        // Load
        sys2.load_state(&data).unwrap();

        // Verify CPU
        assert_eq!(sys2.board.cpu.snapshot(), cpu_snap);

        // Verify memory
        assert_eq!(sys2.board.map.region_data(Region::Ram)[0x100], 0xAA);
        assert_eq!(sys2.board.map.region_data(Region::VectorRam)[0x200], 0xBB);

        // Verify I/O and timing state
        assert_eq!(sys2.in0, 0x18);
        assert_eq!(sys2.in1, 0xE8);
        assert_eq!(sys2.board.clock, 75_000);
        assert_eq!(sys2.board.nmi_counter, 3000);
        assert!(sys2.board.nmi_pending);
        assert_eq!(sys2.board.watchdog_frame_count, 5);

        // Verify EAROM
        assert_eq!(sys2.earom.read(0), 0x42);
        assert_eq!(sys2.earom.read(63), 0xEF);
    }

    #[test]
    fn save_does_not_include_rom() {
        let mut sys = AsteroidsDeluxeSystem::new();
        sys.board.map.region_data_mut(Region::ProgramRom)[0] = 0xDE;
        sys.board.map.region_data_mut(Region::VectorRom)[0] = 0xAD;

        let data = sys.save_state().unwrap();

        // Load into a fresh system (ROMs are zeroed)
        let mut sys2 = AsteroidsDeluxeSystem::new();
        sys2.load_state(&data).unwrap();

        // ROMs should remain at their default (zeroed), not overwritten
        assert_eq!(sys2.board.map.region_data(Region::ProgramRom)[0], 0x00);
        assert_eq!(sys2.board.map.region_data(Region::VectorRom)[0], 0x00);
    }

    #[test]
    fn earom_write_read() {
        let mut sys = AsteroidsDeluxeSystem::new();

        // Latch address 0x05 with data 0xAB
        sys.write(BusMaster::Cpu(0), 0x3205, 0xAB);

        // Astdelux $3A00 bits: 0=CK, 1=!C1, 2=C2, 3=CS1

        // Erase address 5: C1=0(bit1=1), C2=1(bit2=1), CS1=1(bit3=1)
        sys.write(BusMaster::Cpu(0), 0x3A00, 0x0F); // clock high
        sys.write(BusMaster::Cpu(0), 0x3A00, 0x0E); // clock low

        // Write 0xAB: C1=0(bit1=1), C2=0(bit2=0), CS1=1(bit3=1)
        sys.write(BusMaster::Cpu(0), 0x3A00, 0x0B); // clock high
        sys.write(BusMaster::Cpu(0), 0x3A00, 0x0A); // clock low

        // Read: C1=1(bit1=0), CS1=1(bit3=1), falling edge loads data register
        sys.write(BusMaster::Cpu(0), 0x3A00, 0x09); // clock high
        sys.write(BusMaster::Cpu(0), 0x3A00, 0x08); // falling edge → read

        // Read data register
        let val = sys.read(BusMaster::Cpu(0), 0x2C45);
        assert_eq!(val, 0xAB);
    }
}
