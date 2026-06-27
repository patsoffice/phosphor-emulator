//! Atari Marble Madness (1984) — the project's first 16-bit multi-CPU board.
//!
//! Marble Madness runs on **Atari System 1**: an MC68010 main CPU (7.15909 MHz)
//! over a sparse 24-bit address space, with an M6502 sound CPU (Phase 4). It is
//! a motherboard + game-cartridge design — the reset vectors live in the shared
//! **motherboard BIOS** at `0x000000`, which then jumps into the cartridge's
//! banked program ROMs. The board's protection is a **Slapstic** that
//! bank-switches the `0x080000-0x087FFF` ROM window (Phase 2).
//!
//! This module is built in phases (see the `marble-madness` beads epic). The
//! 68010 boot loop, memory map, control/IRQ registers, the **Slapstic** bank
//! switching of the protected ROM window, and the **2804 EEPROM** are in place.
//! The System 1 video pipeline (Phase 3), the sound CPU (Phase 4), and trackball
//! input (Phase 5) are still stubbed and filled in by their own issues.
//!
//! Closest existing template: [`crate::foodf`] (68000 + `AddressSpace32` + raster
//! IRQs + watchdog). This adds the BIOS/cartridge split and the richer I/O map.
//!
//! Hardware reference: MAME `src/mame/atari/atarisy1.cpp` / `atarisy1_v.cpp`.
//!
//! ## Main-CPU memory map (word bus, big-endian; base windows only)
//! ```text
//!   000000-07FFFF  Program ROM (BIOS @ 0, cartridge banks @ 0x10000+)
//!   080000-087FFF  Slapstic-banked ROM window (4 × 8 KB banks)
//!   2E0000         R  Sprite/MO scanline-interrupt state (bit 7)   [Phase 3d]
//!   400000-401FFF  R/W Work RAM
//!   800000         W  Playfield X scroll      820000  W  Playfield Y scroll
//!   840000         W  Playfield priority color mask
//!   860001         W  Audio/video control latch (sound reset, MO/PF banks)
//!   880001         W  Watchdog reset          8A0001  W  VBLANK IRQ ack
//!   8C0001         W  EEPROM unlock
//!   900000-9FFFFF  R/W Cartridge external RAM (unused by marble)
//!   A00000-A01FFF  R/W Playfield RAM    A02000-A02FFF  R/W Motion-object RAM
//!   A03000-A03FFF  R/W Alphanumerics RAM
//!   B00000-B007FF  R/W Palette RAM
//!   F00000-F003FF  R/W EEPROM 2804 (512 bytes, low byte)
//!   F20000-F20007  R  Trackballs               [Phase 5]
//!   F40000-F4001F  R/W Joystick/ADC (unused by marble)
//!   F60000         R  Switch inputs (start/service/VBLANK/sound-buffer)
//!   FC0001         R  Sound response read       FE0001  W  Sound command write   [Phase 4]
//! ```
//!
//! ## Byte registers on a word bus
//! The single-byte control registers sit at odd addresses (`860001`, `880001`,
//! `8A0001`, …). A 68000 byte write becomes a word read-modify-write at the even
//! base, so the board sees a word access at `860000` with the value in the low
//! byte — we decode on the even base and take `data & 0xFF`, exactly like
//! [`crate::foodf`].

use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    AudioSource, InputConfigurable, InputControl, InputEvent, InputId, InputKind, MachineCore,
    Nvram, Profilable, Renderable, SaveState,
};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace32};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::m68000::{M68kVariant, M68000};
use phosphor_core::cpu::state::M68000State;
use phosphor_core::cpu::{Cpu, CpuStateTrait};
use phosphor_core::device::slapstic::Slapstic;
use phosphor_macros::{BusDebug, MemoryRegion};

use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_low;

// ---------------------------------------------------------------------------
// Address-space regions (backed memory only; I/O is decoded in the Bus impl)
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
enum Region {
    /// Program ROM: BIOS @ 0, cartridge banks @ 0x10000+ (covers 000000-07FFFF).
    Rom = 1,
    Ram = 3,
    /// Cartridge external RAM (900000-9FFFFF). Unused by marble, but mapped so
    /// stray accesses are backed rather than faulting.
    CartRam = 4,
    Playfield = 5,
    Mob = 6,
    Alpha = 7,
    Palette = 8,
}

// ---------------------------------------------------------------------------
// ROM manifest ("marble" parent set, TTL Rev 2 motherboard BIOS)
// ---------------------------------------------------------------------------

/// All 68010 program chips, concatenated back-to-back in load order, then
/// de-interleaved into the big-endian `maincpu` image by [`load_maincpu_image`].
///
/// Order: BIOS even/odd, then the four cartridge banks (even/odd each), then the
/// two slapstic chips (even/odd). Each chip is 0x4000 bytes; even chip = high
/// byte of the 68k word, odd chip = low byte (MAME `ROM_LOAD16_BYTE`).
pub static MARBLE_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x30000,
    entries: &[
        // Motherboard BIOS (TTL Rev 2) — holds the reset vectors at 0x000000.
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
        // Cartridge program banks (even/odd pairs).
        RomEntry {
            name: "136033.623",
            size: 0x4000,
            offset: 0x08000,
            crc32: &[0x284ed2e9],
        },
        RomEntry {
            name: "136033.624",
            size: 0x4000,
            offset: 0x0C000,
            crc32: &[0xd541b021],
        },
        RomEntry {
            name: "136033.625",
            size: 0x4000,
            offset: 0x10000,
            crc32: &[0x563755c7],
        },
        RomEntry {
            name: "136033.626",
            size: 0x4000,
            offset: 0x14000,
            crc32: &[0x860feeb3],
        },
        RomEntry {
            name: "136033.627",
            size: 0x4000,
            offset: 0x18000,
            crc32: &[0xd1dbd439],
        },
        RomEntry {
            name: "136033.628",
            size: 0x4000,
            offset: 0x1C000,
            crc32: &[0x957d6801],
        },
        RomEntry {
            name: "136033.229",
            size: 0x4000,
            offset: 0x20000,
            crc32: &[0xc81d5c14],
        },
        RomEntry {
            name: "136033.630",
            size: 0x4000,
            offset: 0x24000,
            crc32: &[0x687a09f7],
        },
        // Slapstic-banked ROM (even/odd).
        RomEntry {
            name: "136033.107",
            size: 0x4000,
            offset: 0x28000,
            crc32: &[0xf3b8745b],
        },
        RomEntry {
            name: "136033.108",
            size: 0x4000,
            offset: 0x2C000,
            crc32: &[0xe51eecaa],
        },
    ],
};

/// M6502 sound program (Phase 4). 64 KB region with ROM at 0x8000-0xFFFF.
pub static MARBLE_SOUND_ROM: RomRegion = RomRegion {
    size: 0x10000,
    entries: &[
        RomEntry {
            name: "136033.421",
            size: 0x4000,
            offset: 0x8000,
            crc32: &[0x78153dc3],
        },
        RomEntry {
            name: "136033.422",
            size: 0x4000,
            offset: 0xC000,
            crc32: &[0x2e66300e],
        },
    ],
};

/// Build the 0x88000-byte `maincpu` image from the concatenated chips: the
/// 68010 program at 000000-07FFFF and the slapstic ROM at 080000-087FFF, with
/// each even/odd chip pair de-interleaved into big-endian words.
fn load_maincpu_image(rom_set: &RomSet) -> Result<Vec<u8>, RomLoadError> {
    let chips = MARBLE_PROGRAM_ROM.load(rom_set)?;
    let mut image = vec![0u8; 0x88000];
    // (dst_base, even_chip_offset, odd_chip_offset) for each 0x4000-byte pair.
    const PAIRS: [(usize, usize, usize); 6] = [
        (0x00000, 0x00000, 0x04000), // BIOS
        (0x10000, 0x08000, 0x0C000), // cartridge bank 0
        (0x18000, 0x10000, 0x14000), // cartridge bank 1
        (0x20000, 0x18000, 0x1C000), // cartridge bank 2
        (0x28000, 0x20000, 0x24000), // cartridge bank 3
        (0x80000, 0x28000, 0x2C000), // slapstic
    ];
    for (dst, even, odd) in PAIRS {
        for i in 0..0x4000 {
            image[dst + 2 * i] = chips[even + i]; // even address = high byte
            image[dst + 2 * i + 1] = chips[odd + i]; // odd address = low byte
        }
    }
    Ok(image)
}

// ---------------------------------------------------------------------------
// Input IDs (minimal Phase-1 set; trackball + coins land in Phase 5 / 4)
// ---------------------------------------------------------------------------

pub const INPUT_START1: u8 = 0;
pub const INPUT_START2: u8 = 1;
/// Service / self-test switch (F60000 bit 6, active-low `PORT_SERVICE`).
pub const INPUT_SERVICE: u8 = 2;

const MARBLE_CONTROLS: &[InputControl] = &[
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
        label: "Service / Self-Test",
        kind: InputKind::Service,
        player: None,
        default_bindings: crate::input_defaults::SERVICE,
    },
];

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------
// Master clock 14.318181 MHz. Main CPU = pixel clock = master/2 = 7.15909 MHz,
// so CPU cycles map 1:1 to pixel clocks: HTOTAL 456, VTOTAL 262 → ~59.92 Hz.
// Visible area 336×240 (MAME `set_raw(.../2, 456, 0, 336, 262, 0, 240)`).
const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 7_159_090,
    cycles_per_scanline: 456,
    total_scanlines: 262,
    display_width: 336,
    display_height: 240,
};

/// First scanline of vertical blank (`vbstart`); VBLANK asserts IRQ4 here.
const VBLANK_SCANLINE: u16 = 240;

// ---------------------------------------------------------------------------
// MarbleSystem
// ---------------------------------------------------------------------------

/// Atari Marble Madness (System 1) arcade system.
#[derive(BusDebug)]
pub struct MarbleSystem {
    #[debug_cpu("M68010")]
    cpu: M68000,
    #[debug_map(cpu = 0)]
    map: AddressSpace32,

    /// Slapstic 137412-103 protection PAL gating the 080000-087FFF ROM window.
    slapstic: Slapstic,
    /// The 32 KB (4 × 8 KB bank) slapstic ROM the window selects between.
    slapstic_rom: Vec<u8>,

    /// EEPROM 2804 (512 bytes, low byte at F00000-F003FF), gated by `eeprom_unlocked`.
    eeprom: [u8; 512],

    // Video control latches (consumed by the Phase 3 video pipeline).
    xscroll: u16,
    yscroll: u16,
    priority_pens: u16,
    /// 0x860001 audio/video control: bit 7 = sound-CPU reset (Phase 4), bits
    /// 5-3 = motion-object bank, bit 2 = playfield tile bank (Phase 3).
    bankselect: u8,
    /// 0x8C0001 EEPROM unlock latch. The 2804 re-locks after each write.
    eeprom_unlocked: bool,

    // F60000 switch port low byte (active-low; bits 0/1 = start, bit 6 = service).
    // Bits 4 (VBLANK) and 7 (sound buffer) are computed live in `read_f60000`.
    f60000_buttons: u8,

    // VBLANK interrupt latch (IRQ4), held until acked via 0x8A0001.
    video_int: bool,

    clock: u64,
    watchdog_count: u8,
}

impl MarbleSystem {
    fn build_map() -> AddressSpace32 {
        let mut map = AddressSpace32::new();
        map.region(
            Region::Rom,
            "Program ROM",
            0x00_0000,
            0x8_0000,
            AccessKind::ReadOnly,
        )
        .region(
            Region::Ram,
            "Work RAM",
            0x40_0000,
            0x2000,
            AccessKind::ReadWrite,
        )
        .region(
            Region::CartRam,
            "Cartridge RAM",
            0x90_0000,
            0x10_0000,
            AccessKind::ReadWrite,
        )
        .region(
            Region::Playfield,
            "Playfield RAM",
            0xA0_0000,
            0x2000,
            AccessKind::ReadWrite,
        )
        .region(
            Region::Mob,
            "Motion-object RAM",
            0xA0_2000,
            0x1000,
            AccessKind::ReadWrite,
        )
        .region(
            Region::Alpha,
            "Alpha RAM",
            0xA0_3000,
            0x1000,
            AccessKind::ReadWrite,
        )
        .region(
            Region::Palette,
            "Palette RAM",
            0xB0_0000,
            0x800,
            AccessKind::ReadWrite,
        );
        map
    }

    pub fn new() -> Self {
        let mut cpu = M68000::new();
        cpu.variant = M68kVariant::M68010;
        Self {
            cpu,
            map: Self::build_map(),
            slapstic: Slapstic::new(),
            slapstic_rom: vec![0; 0x8000],
            eeprom: [0xFF; 512], // 2804 reads 0xFF erased; game checksums + reinits
            xscroll: 0,
            yscroll: 0,
            priority_pens: 0,
            bankselect: 0,
            eeprom_unlocked: false,
            f60000_buttons: 0xFF,
            video_int: false,
            clock: 0,
            watchdog_count: 0,
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let image = load_maincpu_image(rom_set)?;
        self.map.load_region(Region::Rom, &image[0x00000..0x80000]);
        // The slapstic ROM is held outside the map: the bus picks a bank per
        // access via the slapstic state machine (see `slapstic_read`).
        self.slapstic_rom.copy_from_slice(&image[0x80000..0x88000]);
        // GFX/PROM decode is Phase 3; the sound program is Phase 4.
        Ok(())
    }

    pub fn get_cpu_state(&self) -> M68000State {
        self.cpu.snapshot()
    }

    pub fn clock(&self) -> u64 {
        self.clock
    }

    /// 0x860001 audio/video control latch. Phase 1 only stashes the value; the
    /// sound-reset (bit 7) and bank bits (5-3 / 2) are decoded in later phases.
    fn bankselect_w(&mut self, data: u8) {
        self.bankselect = data;
    }

    /// True while the beam is in vertical blank (scanline ≥ `VBLANK_SCANLINE`).
    fn in_vblank(&self) -> bool {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();
        let scanline = (frame_cycle / TIMING.cycles_per_scanline) as u16;
        scanline >= VBLANK_SCANLINE
    }

    /// F60000 switch port (word). Active-low: idle bits read 1. Bit 4 is the
    /// live VBLANK line (0 during blank); bit 7 is the sound-buffer-pending flag
    /// (active-high, always 0 until the sound CPU exists in Phase 4).
    fn read_f60000(&self) -> u16 {
        let mut low = self.f60000_buttons;
        if self.in_vblank() {
            low &= !0x10; // VBLANK active-low
        } else {
            low |= 0x10;
        }
        low &= !0x80; // sound buffer not pending
        0xFF00 | low as u16
    }

    /// Scanline-interrupt state read at 0x2E0000 (bit 7). Always clear until the
    /// SLIP/IRQ3 timer arrives in Phase 3d.
    fn int3_state(&self) -> u16 {
        0x0000
    }

    /// Read a word from the slapstic-banked window (080000-087FFF). Each access
    /// feeds its word offset to the slapstic, which may change the live bank;
    /// the word is then read from that 8 KB bank. The window is mirrored ×4, so
    /// the bank offset is just the low 13 bits of the address.
    fn slapstic_read(&mut self, addr: u32) -> u16 {
        let word_offset = (((addr - 0x08_0000) >> 1) & 0x3FFF) as u16;
        let bank = self.slapstic.tweak(word_offset) as usize;
        let base = bank * 0x2000 + (addr as usize & 0x1FFE);
        u16::from_be_bytes([self.slapstic_rom[base], self.slapstic_rom[base + 1]])
    }

    /// Effective autovector interrupt level. Phase 1 wires only IRQ4 (VBLANK);
    /// IRQ3 (SLIP), IRQ6 (sound), and IRQ2 (ADC) arrive in later phases.
    fn interrupt_level(&self) -> u8 {
        if self.video_int { 4 } else { 0 }
    }

    pub fn tick(&mut self) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();

        // VBLANK raises IRQ4 at the start of the first blanked scanline.
        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let scanline = (frame_cycle / TIMING.cycles_per_scanline) as u16;
            if scanline == VBLANK_SCANLINE {
                self.video_int = true;
            }
        }

        // Latch watchpoint attribution context before CPU execution.
        if self.map.has_any_watchpoints() {
            let pc = self.cpu.at_instruction_boundary().then_some(self.cpu.pc);
            self.map.latch_access_context(self.clock, pc);
        }

        bus_split!(self, bus: u32 word => {
            self.cpu.execute_cycle(bus, BusMaster::Cpu(0));
        });

        self.clock += 1;
    }
}

impl Default for MarbleSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus
// ---------------------------------------------------------------------------

impl Bus for MarbleSystem {
    type Address = u32;
    type Data = u16;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn read(&mut self, master: BusMaster, addr: u32) -> u16 {
        let val = match addr {
            // Backed ROM / RAM windows.
            0x00_0000..=0x07_FFFF
            | 0x40_0000..=0x40_1FFF
            | 0x90_0000..=0x9F_FFFF
            | 0xA0_0000..=0xA0_3FFF
            | 0xB0_0000..=0xB0_07FF => self.map.read_bus_word_be(addr),
            0x08_0000..=0x08_7FFF => self.slapstic_read(addr),
            0x2E_0000..=0x2E_0001 => self.int3_state(),
            0xF0_0000..=0xF0_03FF => self.eeprom[((addr >> 1) & 0x1FF) as usize] as u16,
            0xF2_0000..=0xF2_0007 => 0x0000, // Trackballs (Phase 5)
            0xF4_0000..=0xF4_001F => 0x00FF, // Joystick/ADC (unused by marble)
            0xF6_0000..=0xF6_0003 => self.read_f60000(),
            0xFC_0000..=0xFC_0001 => 0x0000, // Sound response (Phase 4)
            _ => 0xFFFF,
        };
        self.map.watch_read(0, master, addr, val as u32, 2);
        val
    }

    fn write(&mut self, master: BusMaster, addr: u32, data: u16) {
        self.map.watch_write(0, master, addr, data as u32, 2);
        let byte = (data & 0xFF) as u8;
        match addr {
            0x00_0000..=0x08_7FFF => {} // ROM, ignore
            0x40_0000..=0x40_1FFF
            | 0x90_0000..=0x9F_FFFF
            | 0xA0_0000..=0xA0_3FFF
            | 0xB0_0000..=0xB0_07FF => self.map.write_bus_word_be(addr, data),
            0x80_0000..=0x80_0001 => self.xscroll = data,
            0x82_0000..=0x82_0001 => self.yscroll = data,
            0x84_0000..=0x84_0001 => self.priority_pens = data,
            0x86_0000..=0x86_0001 => self.bankselect_w(byte),
            0x88_0000..=0x88_0001 => self.watchdog_count = 0, // watchdog reset
            0x8A_0000..=0x8A_0001 => self.video_int = false,  // VBLANK IRQ4 ack
            0x8C_0000..=0x8C_0001 => self.eeprom_unlocked = true, // EEPROM unlock
            0xF0_0000..=0xF0_03FF => {
                // 2804 writes are gated by the unlock latch and re-lock after one byte.
                if self.eeprom_unlocked {
                    self.eeprom[((addr >> 1) & 0x1FF) as usize] = byte;
                    self.eeprom_unlocked = false;
                }
            }
            0xF4_0000..=0xF4_001F => {} // ADC channel select (Phase 5)
            0xF8_0000..=0xF8_0001 => {} // Sound latch (RoadBlasters only)
            0xFE_0000..=0xFE_0001 => {} // Sound command (Phase 4)
            _ => {}
        }
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            irq_level: self.interrupt_level(),
            // 0xFF ⇒ the 68000 core autovectors (vector 24 + level).
            irq_vector: 0xFF,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Capability traits
// ---------------------------------------------------------------------------

impl Renderable for MarbleSystem {
    fn display_size(&self) -> (u32, u32) {
        TIMING.display_size()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        // Video is Phase 3; emit a black frame until the compositor exists.
        buffer.fill(0);
    }
}

impl AudioSource for MarbleSystem {
    fn fill_audio(&mut self, _buffer: &mut [i16]) -> usize {
        0 // Sound CPU + POKEY arrive in Phase 4.
    }

    fn audio_sample_rate(&self) -> u32 {
        44100
    }
}

impl InputConfigurable for MarbleSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        MARBLE_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        if let InputEvent::Button { id, pressed } = event {
            match id.0 as u8 {
                INPUT_START1 => set_bit_active_low(&mut self.f60000_buttons, 0, pressed),
                INPUT_START2 => set_bit_active_low(&mut self.f60000_buttons, 1, pressed),
                INPUT_SERVICE => set_bit_active_low(&mut self.f60000_buttons, 6, pressed),
                _ => {}
            }
        }
        // Trackballs (F20000) and coins (sound port 1820) are Phases 5 and 4.
    }
}

impl MachineCore for MarbleSystem {
    crate::machine_core_metadata!("marble", TIMING);

    fn run_frame(&mut self) {
        for _ in 0..TIMING.cycles_per_frame() {
            self.tick();
        }

        // Watchdog: System 1 reboots after 8 VBLANKs without a strobe to
        // 0x880001. The game kicks it every frame; if it stops, reset.
        self.watchdog_count = self.watchdog_count.saturating_add(1);
        if self.watchdog_count >= 8 {
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.slapstic.reset();
        self.xscroll = 0;
        self.yscroll = 0;
        self.priority_pens = 0;
        self.bankselect = 0;
        self.eeprom_unlocked = false;
        self.f60000_buttons = 0xFF;
        self.video_int = false;
        self.watchdog_count = 0;
        // EEPROM contents are non-volatile and survive reset.

        bus_split!(self, bus: u32 word => {
            self.cpu.reset(bus, BusMaster::Cpu(0));
        });
    }
}

// `MachineDebug` (debug_bus + cycle stepping) via the standalone-debug macro;
// `BusDebug` is `#[derive]`d on the struct above (24-bit `AddressSpace32` bus).
crate::impl_standalone_debug!(MarbleSystem);

impl Saveable for MarbleSystem {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        self.slapstic.save_state(w);
        w.write_bytes(self.map.region_data(Region::Ram));
        w.write_bytes(self.map.region_data(Region::CartRam));
        w.write_bytes(self.map.region_data(Region::Playfield));
        w.write_bytes(self.map.region_data(Region::Mob));
        w.write_bytes(self.map.region_data(Region::Alpha));
        w.write_bytes(self.map.region_data(Region::Palette));
        w.write_bytes(&self.eeprom);
        w.write_u16_le(self.xscroll);
        w.write_u16_le(self.yscroll);
        w.write_u16_le(self.priority_pens);
        w.write_u8(self.bankselect);
        w.write_bool(self.eeprom_unlocked);
        w.write_u8(self.f60000_buttons);
        w.write_bool(self.video_int);
        w.write_u64_le(self.clock);
        w.write_u8(self.watchdog_count);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        self.slapstic.load_state(r)?;
        r.read_bytes_into(self.map.region_data_mut(Region::Ram))?;
        r.read_bytes_into(self.map.region_data_mut(Region::CartRam))?;
        r.read_bytes_into(self.map.region_data_mut(Region::Playfield))?;
        r.read_bytes_into(self.map.region_data_mut(Region::Mob))?;
        r.read_bytes_into(self.map.region_data_mut(Region::Alpha))?;
        r.read_bytes_into(self.map.region_data_mut(Region::Palette))?;
        r.read_bytes_into(&mut self.eeprom)?;
        self.xscroll = r.read_u16_le()?;
        self.yscroll = r.read_u16_le()?;
        self.priority_pens = r.read_u16_le()?;
        self.bankselect = r.read_u8()?;
        self.eeprom_unlocked = r.read_bool()?;
        self.f60000_buttons = r.read_u8()?;
        self.video_int = r.read_bool()?;
        self.clock = r.read_u64_le()?;
        self.watchdog_count = r.read_u8()?;
        Ok(())
    }
}

impl SaveState for MarbleSystem {
    crate::machine_save_state!();
}

// The 2804 EEPROM is the machine's battery-backed store; the frontend persists
// it through the Nvram trait (high scores, config, the boot game-id byte).
impl Nvram for MarbleSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(&self.eeprom)
    }

    fn load_nvram(&mut self, data: &[u8]) {
        let len = data.len().min(self.eeprom.len());
        self.eeprom[..len].copy_from_slice(&data[..len]);
    }
}

// No sub-span profiling, no event tracing.
impl Profilable for MarbleSystem {}
impl phosphor_core::core::debug_trace::DebugTrace for MarbleSystem {}

// Marble Madness has no operator DIP switches — coinage and game options live
// in the EEPROM and the sound-board config. The all-default trait exposes no banks.
impl phosphor_core::core::machine::DipSwitches for MarbleSystem {}

// ---------------------------------------------------------------------------
// Registry + disassembly
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = MarbleSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("marble", &["marble"], create_machine)
}

// Disassemblable code regions for the standalone `disasm` tool.
// `main`  — the MC68010 program image (000000-07FFFF, de-interleaved).
// `sound` — the M6502 sound program (8000-FFFF).
inventory::submit! {
    DisasmRegion {
        machine: "marble",
        region: "main",
        cpu: DisasmCpu::M68000,
        org: 0,
        size: 0x80000,
        load: |rs| load_maincpu_image(rs).map(|mut v| { v.truncate(0x80000); v }),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "marble",
        region: "sound",
        cpu: DisasmCpu::M6502,
        org: 0x8000,
        size: 0x8000,
        load: |rs| MARBLE_SOUND_ROM.load(rs).map(|v| v[0x8000..0x10000].to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_decodes_documented_windows() {
        let sys = MarbleSystem::new();
        assert_eq!(sys.map.region_at(0x00_0000).unwrap().id, Region::Rom.into());
        // The slapstic window (080000-087FFF) is not a map region — it is decoded
        // in the bus and banked by the slapstic.
        assert!(sys.map.region_at(0x08_0000).is_none());
        assert_eq!(sys.map.region_at(0x40_0000).unwrap().id, Region::Ram.into());
        assert_eq!(
            sys.map.region_at(0x90_0000).unwrap().id,
            Region::CartRam.into()
        );
        assert_eq!(
            sys.map.region_at(0xA0_0000).unwrap().id,
            Region::Playfield.into()
        );
        assert_eq!(sys.map.region_at(0xA0_2000).unwrap().id, Region::Mob.into());
        assert_eq!(
            sys.map.region_at(0xA0_3000).unwrap().id,
            Region::Alpha.into()
        );
        assert_eq!(
            sys.map.region_at(0xB0_0000).unwrap().id,
            Region::Palette.into()
        );
    }

    #[test]
    fn cpu_is_a_68010() {
        let sys = MarbleSystem::new();
        assert_eq!(sys.cpu.variant, M68kVariant::M68010);
    }

    #[test]
    fn reset_loads_ssp_and_pc_from_bios_vectors() {
        let mut sys = MarbleSystem::new();
        // Reset vectors live in the BIOS at 0x000000: SSP = 0x00401000,
        // PC = 0x00000400.
        let rom = sys.map.region_data_mut(Region::Rom);
        rom[0..8].copy_from_slice(&[0x00, 0x40, 0x10, 0x00, 0x00, 0x00, 0x04, 0x00]);
        sys.reset();
        let st = sys.get_cpu_state();
        assert_eq!(st.a[7], 0x0040_1000);
        assert_eq!(st.pc, 0x0000_0400);
    }

    #[test]
    fn ram_word_access_round_trips() {
        let mut sys = MarbleSystem::new();
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x40_0000, 0xBEEF);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0x40_0000), 0xBEEF);
    }

    #[test]
    fn palette_and_video_ram_round_trip() {
        let mut sys = MarbleSystem::new();
        Bus::write(&mut sys, BusMaster::Cpu(0), 0xB0_0000, 0x0ABC);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0xB0_0000), 0x0ABC);
        Bus::write(&mut sys, BusMaster::Cpu(0), 0xA0_3000, 0x1234);
        assert_eq!(Bus::read(&mut sys, BusMaster::Cpu(0), 0xA0_3000), 0x1234);
    }

    #[test]
    fn control_latches_and_acks() {
        let mut sys = MarbleSystem::new();
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x80_0000, 0x0040);
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x82_0000, 0x0020);
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x86_0000, 0x00AC);
        assert_eq!(sys.xscroll, 0x0040);
        assert_eq!(sys.yscroll, 0x0020);
        assert_eq!(sys.bankselect, 0xAC);

        // VBLANK IRQ4 asserts, then 0x8A0001 acks it.
        sys.video_int = true;
        assert_eq!(sys.interrupt_level(), 4);
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x8A_0000, 0x0000);
        assert!(!sys.video_int);
        let st = sys.check_interrupts(BusMaster::Cpu(0));
        assert_eq!(st.irq_level, 0);
        assert_eq!(st.irq_vector, 0xFF);
    }

    #[test]
    fn watchdog_strobe_resets_count() {
        let mut sys = MarbleSystem::new();
        sys.watchdog_count = 5;
        Bus::write(&mut sys, BusMaster::Cpu(0), 0x88_0000, 0x0000);
        assert_eq!(sys.watchdog_count, 0);
    }

    #[test]
    fn f60000_reports_vblank_and_start_buttons() {
        let mut sys = MarbleSystem::new();
        // Outside VBLANK (clock at 0 → scanline 0): bit 4 set, bit 7 clear.
        assert_eq!(sys.read_f60000() & 0x0090, 0x0010);
        // Inside VBLANK: bit 4 clears.
        sys.clock = (VBLANK_SCANLINE as u64) * TIMING.cycles_per_scanline;
        assert_eq!(sys.read_f60000() & 0x0010, 0x0000);

        // Start1 is active-low bit 0.
        sys.clock = 0;
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_START1 as u16),
            pressed: true,
        });
        assert_eq!(sys.read_f60000() & 0x0001, 0x0000);
    }

    /// Boot a hand-assembled 68010 program on the full board and prove the core
    /// runs it, services the autovectored VBLANK IRQ, and stores into RAM —
    /// exercising the 68010 exception frame end-to-end inside the machine.
    #[test]
    fn synthetic_program_boots_and_takes_vblank_irq() {
        let mut sys = MarbleSystem::new();
        {
            let rom = sys.map.region_data_mut(Region::Rom);
            // Reset vectors: SSP = 0x00401000, PC = 0x00000400.
            rom[0x00..0x08].copy_from_slice(&[0x00, 0x40, 0x10, 0x00, 0x00, 0x00, 0x04, 0x00]);
            // VBLANK is IRQ4 → autovector 28 (0x70) → handler at 0x500.
            rom[28 * 4..28 * 4 + 4].copy_from_slice(&[0x00, 0x00, 0x05, 0x00]);

            // Main @ 0x400:
            //   move #$2000, sr        ; supervisor, interrupt mask = 0
            //   loop: addq.l #1, $400000
            //         bra.s loop
            let main: &[u8] = &[
                0x46, 0xFC, 0x20, 0x00, // move #$2000, sr
                0x52, 0xB9, 0x00, 0x40, 0x00, 0x00, // addq.l #1, $00400000
                0x60, 0xF8, // bra.s loop
            ];
            rom[0x400..0x400 + main.len()].copy_from_slice(main);

            // IRQ4 handler @ 0x500:
            //   addq.l #1, $400010     ; bump the interrupt counter
            //   move.b #0, $8A0001     ; ack VBLANK IRQ4
            //   rte
            let handler: &[u8] = &[
                0x52, 0xB9, 0x00, 0x40, 0x00, 0x10, // addq.l #1, $00400010
                0x13, 0xFC, 0x00, 0x00, 0x00, 0x8A, 0x00, 0x01, // move.b #0, $008A0001
                0x4E, 0x73, // rte
            ];
            rom[0x500..0x500 + handler.len()].copy_from_slice(handler);
        }

        sys.reset();
        assert_eq!(sys.get_cpu_state().pc, 0x0000_0400);

        // Three frames stays under the 8-frame watchdog timeout.
        for _ in 0..3 {
            sys.run_frame();
        }

        // CPU is still executing inside the ROM main loop.
        let pc = sys.get_cpu_state().pc;
        assert!(pc < 0x8_0000, "PC {pc:#08X} escaped ROM");

        // The "alive" counter advanced → the core actually ran code.
        let ram = sys.map.region_data(Region::Ram);
        let alive = u32::from_be_bytes([ram[0], ram[1], ram[2], ram[3]]);
        assert!(alive > 0, "main loop never ran");

        // The interrupt counter advanced → the autovectored VBLANK IRQ was
        // taken and RTE returned cleanly through the 68010 frame.
        let taken = u32::from_be_bytes([ram[0x10], ram[0x11], ram[0x12], ram[0x13]]);
        assert!(taken > 0, "VBLANK IRQ was never serviced");
    }

    #[test]
    fn disasm_regions_registered() {
        use crate::disasm_registry::{find, regions_for};
        assert_eq!(
            regions_for("marble")
                .iter()
                .map(|r| r.region)
                .collect::<Vec<_>>(),
            vec!["main", "sound"],
        );
        let main = find("marble", "main").expect("main region");
        assert_eq!(main.cpu, DisasmCpu::M68000);
        assert_eq!((main.org, main.size), (0, 0x80000));
        let sound = find("marble", "sound").expect("sound region");
        assert_eq!(sound.cpu, DisasmCpu::M6502);
        assert_eq!((sound.org, sound.size), (0x8000, 0x8000));
    }

    #[test]
    fn slapstic_banks_the_window_through_the_bus() {
        let mut sys = MarbleSystem::new();
        // Distinct marker word at offset 0 of each 8 KB bank.
        for b in 0..4u8 {
            sys.slapstic_rom[b as usize * 0x2000] = 0x10 + b;
        }
        let read = |sys: &mut MarbleSystem, a| Bus::read(sys, BusMaster::Cpu(0), a);

        // Power-on bank is 3; the arming read (offset 0) returns its marker.
        assert_eq!(read(&mut sys, 0x08_0000), 0x1300);
        // Direct-select bank 0 (offset 0x40 → byte 0x80), then read its marker.
        read(&mut sys, 0x08_0080);
        assert_eq!(read(&mut sys, 0x08_0000), 0x1000);
        assert_eq!(sys.slapstic.current_bank(), 0);
    }

    #[test]
    fn eeprom_writes_gated_by_unlock_and_relock() {
        let mut sys = MarbleSystem::new();
        let w = |sys: &mut MarbleSystem, a, d| Bus::write(sys, BusMaster::Cpu(0), a, d);
        let r = |sys: &mut MarbleSystem, a| Bus::read(sys, BusMaster::Cpu(0), a);

        // Locked: the write is dropped (still reads the erased 0xFF).
        w(&mut sys, 0xF0_0000, 0x0042);
        assert_eq!(r(&mut sys, 0xF0_0000), 0x00FF);

        // Unlock (8C0001), then one write sticks...
        w(&mut sys, 0x8C_0001, 0x0001);
        w(&mut sys, 0xF0_0000, 0x0042);
        assert_eq!(r(&mut sys, 0xF0_0000), 0x0042);

        // ...and the 2804 re-locked, so the next write without an unlock is dropped.
        w(&mut sys, 0xF0_0002, 0x0099);
        assert_eq!(r(&mut sys, 0xF0_0002), 0x00FF);
        assert!(!sys.eeprom_unlocked);
    }

    #[test]
    fn nvram_exposes_the_eeprom() {
        let mut sys = MarbleSystem::new();
        sys.eeprom[0x6E] = 0x42; // the boot game-id byte lives around here
        assert_eq!(Nvram::save_nvram(&sys).unwrap()[0x6E], 0x42);

        let mut sys2 = MarbleSystem::new();
        let snapshot = Nvram::save_nvram(&sys).unwrap().to_vec();
        Nvram::load_nvram(&mut sys2, &snapshot);
        assert_eq!(sys2.eeprom[0x6E], 0x42);
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = MarbleSystem::new();
        sys.map.region_data_mut(Region::Ram)[0x100] = 0xAB;
        sys.map.region_data_mut(Region::Playfield)[0x10] = 0xCD;
        sys.map.region_data_mut(Region::Alpha)[0x20] = 0xEF;
        sys.eeprom[0x30] = 0x99;
        // Drive the slapstic to a non-default bank so its state is exercised.
        // Bank 1's select offset is 0x50 (word) → byte address 0x0800A0.
        Bus::read(&mut sys, BusMaster::Cpu(0), 0x08_0000); // arm
        Bus::read(&mut sys, BusMaster::Cpu(0), 0x08_00A0); // select bank 1
        assert_eq!(sys.slapstic.current_bank(), 1);
        sys.xscroll = 0x1234;
        sys.bankselect = 0x5A;
        sys.video_int = true;
        sys.clock = 99_999;
        sys.watchdog_count = 4;

        let data = SaveState::save_state(&sys).expect("save");
        let cpu_snap = sys.get_cpu_state();

        let mut sys2 = MarbleSystem::new();
        SaveState::load_state(&mut sys2, &data).unwrap();

        assert_eq!(sys2.get_cpu_state(), cpu_snap);
        assert_eq!(sys2.map.region_data(Region::Ram)[0x100], 0xAB);
        assert_eq!(sys2.map.region_data(Region::Playfield)[0x10], 0xCD);
        assert_eq!(sys2.map.region_data(Region::Alpha)[0x20], 0xEF);
        assert_eq!(sys2.eeprom[0x30], 0x99);
        assert_eq!(sys2.slapstic.current_bank(), 1);
        assert_eq!(sys2.xscroll, 0x1234);
        assert_eq!(sys2.bankselect, 0x5A);
        assert!(sys2.video_int);
        assert_eq!(sys2.clock, 99_999);
        assert_eq!(sys2.watchdog_count, 4);
    }
}
