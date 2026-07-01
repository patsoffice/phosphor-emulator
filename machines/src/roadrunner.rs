//! Atari Road Runner (1985) — the second game on the Atari System 1 board.
//!
//! Road Runner shares all of the System 1 hardware in [`AtariSystem1Board`] with
//! Marble Madness; this module is the thin game wrapper (repo board-wrapper
//! pattern). It contributes Road Runner's cartridge ROM manifest, its slapstic
//! chip (137412-108), and the fact that its sound board carries speech (a
//! TMS5220 behind a VIA6522, currently stubbed).
//!
//! Its analog "Hall-effect" joystick (an ADC0809 at `0xF40000` driving IRQ2) is
//! a follow-up; here the wrapper is a straight pass-through to the board plus the
//! digital switch/coin inputs, which is enough to boot and render attract mode.

use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    InputConfigurable, InputControl, InputEvent, InputId, InputKind, MachineCore, Nvram,
    Profilable, SaveState,
};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::state::M68000State;

use crate::atari_system1::{self, AtariSystem1Board};
use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_low;

// ---------------------------------------------------------------------------
// ROM manifest ("roadrunn" parent set — Road Runner rev 2)
// ---------------------------------------------------------------------------

/// All 68010 program chips, concatenated back-to-back in load order, then
/// de-interleaved into the big-endian `maincpu` image by [`load_maincpu_image`].
///
/// The motherboard BIOS holds the reset vectors; the cartridge program and the
/// slapstic-banked ROM follow as `LOAD16_BYTE` even/odd pairs (even chip = high
/// byte of the 68k word, odd chip = low byte). Unlike Marble the banks are not
/// contiguous — they land at the region offsets MAME loads them to.
pub static ROADRUNNER_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x60000,
    entries: &[
        // Motherboard BIOS (TTL Rev 2) — reset vectors at 0x000000 (shared chip).
        RomEntry {
            name: "136032.205.l13",
            size: 0x4000,
            offset: 0x00000,
            crc32: &[0x88d0be26],
        },
        RomEntry {
            name: "136032.206.l12",
            size: 0x4000,
            offset: 0x04000,
            crc32: &[0x3c79ef05],
        },
        // Cartridge program (even/odd pairs), region 0x10000 / 0x20000.
        RomEntry {
            name: "136040-228.11c",
            size: 0x8000,
            offset: 0x08000,
            crc32: &[0xb66c629a],
        },
        RomEntry {
            name: "136040-229.11a",
            size: 0x8000,
            offset: 0x10000,
            crc32: &[0x5638959f],
        },
        RomEntry {
            name: "136040-230.13c",
            size: 0x8000,
            offset: 0x18000,
            crc32: &[0xcd7956a3],
        },
        RomEntry {
            name: "136040-231.13a",
            size: 0x8000,
            offset: 0x20000,
            crc32: &[0x722f2d3b],
        },
        // Cartridge program, region 0x50000 / 0x60000 / 0x70000.
        RomEntry {
            name: "136040-134.12c",
            size: 0x8000,
            offset: 0x28000,
            crc32: &[0x18f431fe],
        },
        RomEntry {
            name: "136040-135.12a",
            size: 0x8000,
            offset: 0x30000,
            crc32: &[0xcb06f9ab],
        },
        RomEntry {
            name: "136040-136.14c",
            size: 0x8000,
            offset: 0x38000,
            crc32: &[0x8050bce4],
        },
        RomEntry {
            name: "136040-137.14a",
            size: 0x8000,
            offset: 0x40000,
            crc32: &[0x3372a5cf],
        },
        RomEntry {
            name: "136040-138.16c",
            size: 0x8000,
            offset: 0x48000,
            crc32: &[0xa83155ee],
        },
        RomEntry {
            name: "136040-139.16a",
            size: 0x8000,
            offset: 0x50000,
            crc32: &[0x23aead1c],
        },
        // Slapstic-banked ROM (even/odd), region 0x80000.
        RomEntry {
            name: "136040-140.17c",
            size: 0x4000,
            offset: 0x58000,
            crc32: &[0xd1464c88],
        },
        RomEntry {
            name: "136040-141.17a",
            size: 0x4000,
            offset: 0x5C000,
            crc32: &[0xf8f2acdf],
        },
    ],
};

/// Alphanumerics character ROM (the shared motherboard font, 136032.104.f5):
/// 512 tiles, 8×8, 2bpp — identical chip to Marble.
pub static ROADRUNNER_ALPHA_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[RomEntry {
        name: "136032.104.f5",
        size: 0x2000,
        offset: 0x0000,
        crc32: &[0x7a29dc07],
    }],
};

/// Playfield / motion-object tile ROM ("tiles" region, 0x300000): six 0x80000
/// banks, each holding four 0x8000 bitplanes spaced 0x10000 apart. The region is
/// `ROMREGION_INVERT | ROMREGION_ERASEFF`: erase to 0xFF, place the chips, then
/// invert the whole buffer (see [`RoadRunnerSystem::load_rom_set`]).
pub static ROADRUNNER_TILE_ROM: RomRegion = RomRegion {
    size: 0x300000,
    entries: &[
        // Bank 1.
        RomEntry {
            name: "136040-101.4b",
            size: 0x8000,
            offset: 0x000000,
            crc32: &[0x26d9f29c],
        },
        RomEntry {
            name: "136040-107.9b",
            size: 0x8000,
            offset: 0x010000,
            crc32: &[0x8aac0ba4],
        },
        RomEntry {
            name: "136040-113.4f",
            size: 0x8000,
            offset: 0x020000,
            crc32: &[0x48b74c52],
        },
        RomEntry {
            name: "136040-119.9f",
            size: 0x8000,
            offset: 0x030000,
            crc32: &[0x17a6510c],
        },
        // Bank 2.
        RomEntry {
            name: "136040-102.3b",
            size: 0x8000,
            offset: 0x080000,
            crc32: &[0xae88f54b],
        },
        RomEntry {
            name: "136040-108.8b",
            size: 0x8000,
            offset: 0x090000,
            crc32: &[0xa2ac13d4],
        },
        RomEntry {
            name: "136040-114.3f",
            size: 0x8000,
            offset: 0x0a0000,
            crc32: &[0xc91c3fcb],
        },
        RomEntry {
            name: "136040-120.8f",
            size: 0x8000,
            offset: 0x0b0000,
            crc32: &[0x42d25859],
        },
        // Bank 3.
        RomEntry {
            name: "136040-103.2b",
            size: 0x8000,
            offset: 0x100000,
            crc32: &[0xf2d7ef55],
        },
        RomEntry {
            name: "136040-109.7b",
            size: 0x8000,
            offset: 0x110000,
            crc32: &[0x11a843dc],
        },
        RomEntry {
            name: "136040-115.2f",
            size: 0x8000,
            offset: 0x120000,
            crc32: &[0x8b1fa5bc],
        },
        RomEntry {
            name: "136040-121.7f",
            size: 0x8000,
            offset: 0x130000,
            crc32: &[0xecf278f2],
        },
        // Bank 4.
        RomEntry {
            name: "136040-104.1b",
            size: 0x8000,
            offset: 0x180000,
            crc32: &[0x0203d89c],
        },
        RomEntry {
            name: "136040-110.6b",
            size: 0x8000,
            offset: 0x190000,
            crc32: &[0x64801601],
        },
        RomEntry {
            name: "136040-116.1f",
            size: 0x8000,
            offset: 0x1a0000,
            crc32: &[0x52b23a36],
        },
        RomEntry {
            name: "136040-122.6f",
            size: 0x8000,
            offset: 0x1b0000,
            crc32: &[0xb1137a9d],
        },
        // Bank 5.
        RomEntry {
            name: "136040-105.4d",
            size: 0x8000,
            offset: 0x200000,
            crc32: &[0x398a36f8],
        },
        RomEntry {
            name: "136040-111.9d",
            size: 0x8000,
            offset: 0x210000,
            crc32: &[0xf08b418b],
        },
        RomEntry {
            name: "136040-117.2d",
            size: 0x8000,
            offset: 0x220000,
            crc32: &[0xc4394834],
        },
        RomEntry {
            name: "136040-123.7d",
            size: 0x8000,
            offset: 0x230000,
            crc32: &[0xdafd3dbe],
        },
        // Bank 6.
        RomEntry {
            name: "136040-106.3d",
            size: 0x8000,
            offset: 0x280000,
            crc32: &[0x36a77bc5],
        },
        RomEntry {
            name: "136040-112.8d",
            size: 0x8000,
            offset: 0x290000,
            crc32: &[0xb6624f3c],
        },
        RomEntry {
            name: "136040-118.1d",
            size: 0x8000,
            offset: 0x2a0000,
            crc32: &[0xf489a968],
        },
        RomEntry {
            name: "136040-124.6d",
            size: 0x8000,
            offset: 0x2b0000,
            crc32: &[0x524d65f7],
        },
    ],
};

/// Graphics-mapping PROMs ("proms" region, 0x400): prom1 (136040-126) at 0x000,
/// prom2 (136040-125) at 0x200, driving the per-tile bank / bpp / colour / offset
/// lookup (entries 0-255 playfield, 256-511 motion objects).
pub static ROADRUNNER_PROM: RomRegion = RomRegion {
    size: 0x400,
    entries: &[
        RomEntry {
            name: "136040-126.7a",
            size: 0x200,
            offset: 0x000,
            crc32: &[0x1713c0cd],
        },
        RomEntry {
            name: "136040-125.5a",
            size: 0x200,
            offset: 0x200,
            crc32: &[0xa9ca8795],
        },
    ],
};

/// M6502 sound program. 64 KB region with ROM at 0x8000-0xFFFF.
pub static ROADRUNNER_SOUND_ROM: RomRegion = RomRegion {
    size: 0x10000,
    entries: &[
        RomEntry {
            name: "136040-143.15e",
            size: 0x4000,
            offset: 0x8000,
            crc32: &[0x62b9878e],
        },
        RomEntry {
            name: "136040-144.17e",
            size: 0x4000,
            offset: 0xC000,
            crc32: &[0x6ef1b804],
        },
    ],
};

/// Build the 0x88000-byte `maincpu` image: the 68010 program at 000000-07FFFF
/// and the slapstic ROM at 080000-087FFF, de-interleaving each even/odd chip
/// pair into big-endian words at its region offset.
fn load_maincpu_image(rom_set: &RomSet) -> Result<Vec<u8>, RomLoadError> {
    let chips = ROADRUNNER_PROGRAM_ROM.load(rom_set)?;
    let mut image = vec![0u8; 0x88000];
    // (dst_region_offset, even_chip_offset, odd_chip_offset, half_size).
    const PAIRS: [(usize, usize, usize, usize); 7] = [
        (0x00000, 0x00000, 0x04000, 0x4000), // BIOS
        (0x10000, 0x08000, 0x10000, 0x8000), // 228/229
        (0x20000, 0x18000, 0x20000, 0x8000), // 230/231
        (0x50000, 0x28000, 0x30000, 0x8000), // 134/135
        (0x60000, 0x38000, 0x40000, 0x8000), // 136/137
        (0x70000, 0x48000, 0x50000, 0x8000), // 138/139
        (0x80000, 0x58000, 0x5C000, 0x4000), // 140/141 (slapstic)
    ];
    for (dst, even, odd, half) in PAIRS {
        for i in 0..half {
            image[dst + 2 * i] = chips[even + i]; // even address = high byte
            image[dst + 2 * i + 1] = chips[odd + i]; // odd address = low byte
        }
    }
    Ok(image)
}

// ---------------------------------------------------------------------------
// Input IDs (digital switches; the analog joystick is a follow-up)
// ---------------------------------------------------------------------------

/// F60000 bit 0 — "Left Hop / P1 Start" (active-low).
pub const INPUT_START1: u8 = 0;
/// F60000 bit 1 — "Right Hop / P2 Start" (active-low).
pub const INPUT_START2: u8 = 1;
/// Service / self-test switch (F60000 bit 6, active-low).
pub const INPUT_SERVICE: u8 = 2;
/// Coin insert (sound port 0x1820 bit 0, active-low).
pub const INPUT_COIN: u8 = 3;

const ROADRUNNER_CONTROLS: &[InputControl] = &[
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
        label: "P1 Start / Left Hop",
        kind: InputKind::Start,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_START,
    },
    InputControl {
        id: InputId(INPUT_START2 as u16),
        stable_name: "p2_start",
        label: "P2 Start / Right Hop",
        kind: InputKind::Start,
        player: Some(2),
        default_bindings: crate::input_defaults::P2_START,
    },
    InputControl {
        id: InputId(INPUT_SERVICE as u16),
        stable_name: "service",
        label: "Service / Self-Test",
        kind: InputKind::Service,
        player: None,
        default_bindings: crate::input_defaults::SERVICE,
    },
];

// ---------------------------------------------------------------------------
// RoadRunnerSystem — Atari System 1 board configured for Road Runner
// ---------------------------------------------------------------------------

/// Atari Road Runner (System 1). Slapstic 108, speech-equipped sound board.
pub struct RoadRunnerSystem {
    pub board: AtariSystem1Board,
}

impl RoadRunnerSystem {
    pub fn new() -> Self {
        Self {
            board: AtariSystem1Board::new(108, true),
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let image = load_maincpu_image(rom_set)?;
        self.board.load_program(&image);

        // Alpha (text/HUD) font tiles (motherboard, shared with Marble).
        let alpha = ROADRUNNER_ALPHA_ROM.load(rom_set)?;
        self.board.load_alpha(&alpha);

        // Playfield + motion-object tiles. The region is ROMREGION_INVERT |
        // ROMREGION_ERASEFF: erase to 0xFF, place the chips, then invert — so
        // absent planes read 0 and chip data is inverted.
        let mut tiles = ROADRUNNER_TILE_ROM.load_erased(rom_set, 0xFF)?;
        for b in tiles.iter_mut() {
            *b = !*b;
        }
        let prom = ROADRUNNER_PROM.load(rom_set)?;
        self.board.load_gfx(&prom, &tiles);

        // M6502 sound program.
        let sound_image = ROADRUNNER_SOUND_ROM.load(rom_set)?;
        self.board.load_sound(&sound_image);
        Ok(())
    }

    // -- Bring-up diagnostics (forwarded to the board) -----------------------

    pub fn get_cpu_state(&self) -> M68000State {
        self.board.get_cpu_state()
    }

    pub fn clock(&self) -> u64 {
        self.board.clock()
    }

    pub fn sound_debug(&self) -> (bool, u64, bool, bool) {
        self.board.sound_debug()
    }

    pub fn eeprom_debug(&self) -> (usize, u64) {
        self.board.eeprom_debug()
    }

    pub fn video_ram_stats(&self) -> (usize, usize, usize) {
        self.board.video_ram_stats()
    }
}

impl Default for RoadRunnerSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus — a straight pass-through to the board (no game-specific ports yet)
// ---------------------------------------------------------------------------

impl Bus for RoadRunnerSystem {
    type Address = u32;
    type Data = u16;

    fn is_halted_for(&self, master: BusMaster) -> bool {
        self.board.bus_is_halted_for(master)
    }

    fn observe_data_access(&mut self, master: BusMaster, addr: u32, is_write: bool) {
        self.board.bus_observe_data_access(master, addr, is_write);
    }

    fn read(&mut self, master: BusMaster, addr: u32) -> u16 {
        self.board.bus_read(master, addr)
    }

    fn write(&mut self, master: BusMaster, addr: u32, data: u16) {
        self.board.bus_write(master, addr, data);
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        self.board.bus_check_interrupts(target)
    }
}

// ---------------------------------------------------------------------------
// Capability traits
// ---------------------------------------------------------------------------

crate::impl_board_delegation!(RoadRunnerSystem, board, atari_system1::TIMING, bus_addr: u32 word);

impl MachineCore for RoadRunnerSystem {
    crate::machine_core_metadata!("roadrunner", atari_system1::TIMING);

    fn run_frame(&mut self) {
        bus_split!(self, bus: u32 word => {
            for _ in 0..atari_system1::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });

        // Watchdog: System 1 reboots after 8 VBLANKs without a strobe to
        // 0x880001.
        if self.board.advance_watchdog() {
            self.reset();
        }

        self.board.end_frame_audio();
    }

    fn reset(&mut self) {
        self.board.reset();
        bus_split!(self, bus: u32 word => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
        });
    }
}

impl InputConfigurable for RoadRunnerSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        ROADRUNNER_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let InputEvent::Button { id, pressed } = event else {
            return;
        };
        match id.0 as u8 {
            INPUT_START1 => set_bit_active_low(&mut self.board.f60000_buttons, 0, pressed),
            INPUT_START2 => set_bit_active_low(&mut self.board.f60000_buttons, 1, pressed),
            INPUT_SERVICE => set_bit_active_low(&mut self.board.f60000_buttons, 6, pressed),
            // Coins are read on the sound board's 0x1820 port.
            INPUT_COIN => self.board.sound.set_coin(0, pressed),
            _ => {}
        }
    }
}

impl SaveState for RoadRunnerSystem {
    crate::machine_save_state!();
}

impl Saveable for RoadRunnerSystem {
    fn save_state(&self, w: &mut StateWriter) {
        self.board.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.board.load_state(r)
    }
}

// The 2804 EEPROM is the machine's battery-backed store.
impl Nvram for RoadRunnerSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.nvram())
    }

    fn load_nvram(&mut self, data: &[u8]) {
        self.board.load_nvram(data);
    }
}

impl Profilable for RoadRunnerSystem {}
impl phosphor_core::core::debug_trace::DebugTrace for RoadRunnerSystem {}

// Road Runner has no operator DIP switches — coinage and game options live in
// the EEPROM and the sound-board config.
impl phosphor_core::core::machine::DipSwitches for RoadRunnerSystem {}

// ---------------------------------------------------------------------------
// Registry + disassembly
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = RoadRunnerSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("roadrunner", &["roadrunn"], create_machine)
}

inventory::submit! {
    DisasmRegion {
        machine: "roadrunner",
        region: "main",
        cpu: DisasmCpu::M68000,
        org: 0,
        size: 0x80000,
        load: |rs| load_maincpu_image(rs).map(|mut v| { v.truncate(0x80000); v }),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "roadrunner",
        region: "sound",
        cpu: DisasmCpu::M6502,
        org: 0x8000,
        size: 0x8000,
        load: |rs| ROADRUNNER_SOUND_ROM.load(rs).map(|v| v[0x8000..0x10000].to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atari_system1::Region;
    use phosphor_core::cpu::m68000::M68kVariant;

    #[test]
    fn board_is_a_108_speech_system_1() {
        let sys = RoadRunnerSystem::new();
        assert_eq!(sys.board.cpu.variant, M68kVariant::M68010);
        // The slapstic powers on to bank 3 (chip 108 bankstart).
        assert_eq!(sys.board.slapstic.current_bank(), 3);
    }

    #[test]
    fn maincpu_pairs_cover_the_program_and_slapstic_windows() {
        // The de-interleave table must exactly tile the loaded program regions
        // and end at the slapstic window, with no overlap.
        let mut covered = 0usize;
        let pairs = [
            (0x00000usize, 0x4000usize),
            (0x10000, 0x8000),
            (0x20000, 0x8000),
            (0x50000, 0x8000),
            (0x60000, 0x8000),
            (0x70000, 0x8000),
            (0x80000, 0x4000),
        ];
        for (dst, half) in pairs {
            covered += half * 2;
            assert!(dst + half * 2 <= 0x88000, "region {dst:#x} fits the image");
        }
        assert_eq!(covered, 0x4000 * 2 + 0x8000 * 2 * 5 + 0x4000 * 2);
    }

    #[test]
    fn map_and_regions_match_the_board() {
        let sys = RoadRunnerSystem::new();
        // The shared board map is identical to Marble's.
        assert_eq!(
            sys.board.map.region_at(0x00_0000).unwrap().id,
            Region::Rom.into()
        );
        assert!(sys.board.map.region_at(0x08_0000).is_none());
        assert_eq!(
            sys.board.map.region_at(0xB0_0000).unwrap().id,
            Region::Palette.into()
        );
    }

    #[test]
    fn control_latches_forward_to_the_board() {
        let mut sys = RoadRunnerSystem::new();
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x80_0000, 0x0040);
        assert_eq!(sys.board.xscroll, 0x0040);
        // VBLANK IRQ4 asserts and acks through the forwarded bus.
        sys.board.video_int = true;
        assert_eq!(sys.check_interrupts(BusMaster::Cpu(0)).irq_level, 4);
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x8A_0000, 0x0000);
        assert!(!sys.board.video_int);
    }

    #[test]
    fn start_and_service_drive_f60000_active_low() {
        let mut sys = RoadRunnerSystem::new();
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_START1 as u16),
            pressed: true,
        });
        assert_eq!(sys.board.read_f60000() & 0x0001, 0x0000);
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_SERVICE as u16),
            pressed: true,
        });
        assert_eq!(sys.board.read_f60000() & 0x0040, 0x0000);
    }

    #[test]
    fn disasm_regions_registered() {
        use crate::disasm_registry::{find, regions_for};
        assert_eq!(
            regions_for("roadrunner")
                .iter()
                .map(|r| r.region)
                .collect::<Vec<_>>(),
            vec!["main", "sound"],
        );
        let main = find("roadrunner", "main").expect("main region");
        assert_eq!((main.org, main.size), (0, 0x80000));
    }
}
