//! Scramble hardware (Konami, 1981).
//!
//! Tier-2 anchor of the Galaxian family: a Galaxian-derived video board (the
//! shared [`crate::galaxian_video`] engine plus the Scramble blue background and
//! blinking stars) driven by a main Z80, with the Konami sound board
//! ([`KonamiSound`]: a second Z80 + 2×AY-8910) reached through an 8255 PPI. A
//! second 8255 routes the inputs.
//!
//! Memory map (MAME `theend_map`, the modern konami_base driver):
//! ```text
//!   0x0000-0x3fff  Program ROM (16 KB)
//!   0x4000-0x47ff  Work RAM
//!   0x4800-0x4bff  Video RAM (tiles, mirrored to 0x4fff)
//!   0x5000-0x50ff  Object RAM (scroll/color 00-3f, sprites 40-5f, bullets 60-7f)
//!   0x6801 NMI enable  0x6802 coin  0x6803 background  0x6804 stars  0x6806/7 flip
//!   0x7000/0x7800 (r) watchdog
//!   0x8000-0xffff  8255 PPIs (addr bit 8 = PPI #0 inputs, bit 9 = PPI #1)
//! ```
//!
//! PPI #1 carries the sound command (port A) and control/IRQ (port B) to the
//! Konami board; its port C is the "The End"/Scramble protection (a PAL whose
//! result the boot self-test polls — see [`ScrambleBoard::protection_w`]).

use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::DebugTraceBuffer;
use phosphor_core::core::machine::{
    ActionRole, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, Direction, InputConfigurable,
    InputControl, InputEvent, InputId, InputKind, MachineCore, Nvram, Profilable, SaveState,
};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::state::Z80State;
use phosphor_core::cpu::z80::Z80;
use phosphor_core::cpu::{Cpu, CpuStateTrait};
use phosphor_core::device::{I8255, KonamiSound};
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion};

use crate::galaxian_video::{self, GalaxianVideo, GfxBankMode};
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};

pub const SAMPLE_RATE: u32 = 44_100;

/// Main CPU: 18.432 MHz / 6 = 3.072 MHz (same as Galaxian).
pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 3_072_000,
    cycles_per_scanline: 192,
    total_scanlines: 264,
    // Native (pre-orientation) framebuffer: the board declares ROT90 (plus any
    // cocktail flip) and the frontend rotates centrally, so these are the
    // unrotated dimensions.
    display_width: galaxian_video::NATIVE_WIDTH as u32, // 256
    display_height: galaxian_video::NATIVE_HEIGHT as u32, // 224
    display_aspect: Some((3, 4)),                       // portrait tube as viewed (after ROT90)
};

/// Sound CPU clock: 14.318 MHz / 8 ≈ 1.79 MHz.
const SOUND_CLOCK_HZ: u64 = 14_318_000 / 8;

const VISIBLE_LINES: u64 = galaxian_video::NATIVE_HEIGHT as u64;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum Region {
    Rom = 1,
    Ram = 2,
    VideoRam = 3,
    ObjRam = 4,
}

// Input button IDs.
pub const INPUT_COIN: u8 = 0;
pub const INPUT_P1_LEFT: u8 = 1;
pub const INPUT_P1_RIGHT: u8 = 2;
pub const INPUT_P1_UP: u8 = 3;
pub const INPUT_P1_DOWN: u8 = 4;
pub const INPUT_P1_FIRE: u8 = 5;
pub const INPUT_P1_BOMB: u8 = 6;
pub const INPUT_P1_START: u8 = 7;
pub const INPUT_P2_START: u8 = 8;

// ---------------------------------------------------------------------------
// Hardware layout
// ---------------------------------------------------------------------------

/// The two Scramble-family memory maps. Both share the video engine, Konami
/// sound, and 8255s; they differ in region/I-O base addresses, ROM size, and
/// whether the "The End" protection sits on PPI #1 port C.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Hw {
    /// `theend_map`: ROM 16 KB, RAM/IO at 0x4000-0x7fff, PPIs at 0x8000-0xffff,
    /// protection on PPI #1 port C.
    Scramble,
    /// `scobra_map`: ROM 32 KB, RAM at 0x8000, PPIs at 0x9800/0xa000, I/O at
    /// 0xa8xx, no protection.
    Scobra,
    /// `frogger_map`: ROM 16 KB, RAM at 0x8000, VRAM at 0xa800, ObjRAM at
    /// 0xb000, 74LS259 latch at 0xb808-0xb81c, PPIs at 0xc000-0xffff. Single-AY
    /// Frogger sound board and the Frogger video extras; no protection.
    Frogger,
}

impl Hw {
    /// Program ROM region size.
    fn rom_size(self) -> u32 {
        match self {
            Hw::Scramble | Hw::Frogger => 0x4000,
            Hw::Scobra => 0x8000,
        }
    }
    /// Base of the Work RAM (Video RAM is +0x800, Object RAM +0x1000). Only the
    /// contiguous Scramble/Scobra maps use this; Frogger has its own layout.
    fn ram_base(self) -> u16 {
        match self {
            Hw::Scramble => 0x4000,
            Hw::Scobra => 0x8000,
            Hw::Frogger => 0x8000,
        }
    }
    /// Last address served from the memory backing (ROM + RAM/VRAM/ObjRAM) on
    /// the contiguous Scramble/Scobra maps.
    fn backing_end(self) -> u16 {
        self.ram_base() + 0x10ff
    }
}

// ---------------------------------------------------------------------------
// ScrambleBoard
// ---------------------------------------------------------------------------

/// Scramble hardware base: main Z80 + Galaxian-derived video + Konami sound.
#[derive(BusDebug, DebugTrace)]
pub struct ScrambleBoard {
    #[debug_cpu("Z80")]
    pub(crate) cpu: Z80,

    #[debug_map(cpu = 0)]
    pub(crate) map: AddressSpace16,

    pub(crate) video: GalaxianVideo,

    #[debug_device("Konami Sound")]
    pub(crate) sound: KonamiSound,

    // Input ports (active-low: 0xFF = nothing pressed). DIP bits share them.
    pub(crate) in0: u8,
    pub(crate) in1: u8,
    pub(crate) in2: u8,
    ppi0: I8255, // routes IN0/IN1/IN2 to the CPU

    pub(crate) hw: Hw,
    pub(crate) nmi_enabled: bool,
    pub(crate) vblank_nmi_pending: bool,

    // "The End"/Scramble protection (a PAL on ppi1 port C): writes shift a
    // nibble into a state register; an opcode nibble computes the result the
    // game reads back on port C and on IN2 bits 5/7.
    protection_state: u32,
    protection_result: u8,

    pub(crate) clock: u64,
    sound_acc: u64, // Bresenham accumulator for the slower sound clock
    watchdog_counter: u32,

    #[debug_events]
    pub(crate) debug_trace: DebugTraceBuffer,
}

impl Default for ScrambleBoard {
    fn default() -> Self {
        Self::new(Hw::Scramble)
    }
}

impl ScrambleBoard {
    pub fn new(hw: Hw) -> Self {
        let mut video = GalaxianVideo::new();
        let _ = GfxBankMode::None; // base Scramble has no GFX banking
        let sound = if hw == Hw::Frogger {
            // Frogger: blue color-split background, color/scroll/sprite remaps,
            // no stars or bullets; single-AY sound board.
            video.set_frogger(true);
            KonamiSound::new_frogger()
        } else {
            video.set_scramble_stars(true);
            video.set_scramble_bullets(true);
            KonamiSound::new(2)
        };
        Self {
            cpu: Z80::new(),
            map: Self::build_map(hw),
            video,
            sound,
            in0: 0xFF,
            in1: 0xFF,
            in2: 0xFF,
            ppi0: I8255::new(),
            hw,
            nmi_enabled: false,
            vblank_nmi_pending: false,
            protection_state: 0,
            protection_result: 0,
            clock: 0,
            sound_acc: 0,
            watchdog_counter: 0,
            debug_trace: DebugTraceBuffer::new(),
        }
    }

    fn build_map(hw: Hw) -> AddressSpace16 {
        if hw == Hw::Frogger {
            return Self::build_frogger_map();
        }
        let rb = hw.ram_base();
        let mut map = AddressSpace16::new();
        map.region(
            Region::Rom,
            "Program ROM",
            0x0000,
            hw.rom_size(),
            AccessKind::ReadOnly,
        )
        .region(Region::Ram, "Work RAM", rb, 0x0800, AccessKind::ReadWrite)
        .region(
            Region::VideoRam,
            "Video RAM",
            rb + 0x0800,
            0x0400,
            AccessKind::ReadWrite,
        )
        .region(
            Region::ObjRam,
            "Object RAM",
            rb + 0x1000,
            0x0100,
            AccessKind::ReadWrite,
        );
        // Video RAM mirror at +0x400.
        map.mirror(rb + 0x0c00, rb + 0x0800, 0x0400);
        map
    }

    /// `frogger_map` backing layout: ROM, Work RAM, Video RAM, Object RAM at
    /// their (non-contiguous) Frogger addresses.
    fn build_frogger_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(
            Region::Rom,
            "Program ROM",
            0x0000,
            0x4000,
            AccessKind::ReadOnly,
        )
        .region(
            Region::Ram,
            "Work RAM",
            0x8000,
            0x0800,
            AccessKind::ReadWrite,
        )
        .region(
            Region::VideoRam,
            "Video RAM",
            0xa800,
            0x0400,
            AccessKind::ReadWrite,
        )
        .region(
            Region::ObjRam,
            "Object RAM",
            0xb000,
            0x0100,
            AccessKind::ReadWrite,
        );
        // Video RAM mirror at 0xac00.
        map.mirror(0xac00, 0xa800, 0x0400);
        map
    }

    pub fn load_program_rom(&mut self, data: &[u8]) {
        self.map.load_region(Region::Rom, data);
    }
    pub fn load_gfx_rom(&mut self, data: &[u8]) {
        self.video.load_gfx_rom(data);
    }
    pub fn load_color_prom(&mut self, prom: &[u8]) {
        self.video.load_color_prom(prom);
    }
    pub fn load_sound_rom(&mut self, data: &[u8]) {
        self.sound.load_rom(data);
    }

    pub fn get_cpu_state(&self) -> Z80State {
        self.cpu.snapshot()
    }
    pub fn clock(&self) -> u64 {
        self.clock
    }

    // -----------------------------------------------------------------------
    // Core tick
    // -----------------------------------------------------------------------

    pub fn tick(&mut self, bus: &mut dyn Bus<Address = u16, Data = u8>) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();

        if frame_cycle == 0 {
            self.video.begin_frame();
        }
        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let scanline = (frame_cycle / TIMING.cycles_per_scanline) as usize;
            if (scanline as u64) < VISIBLE_LINES {
                let vram = self.map.region_data(Region::VideoRam);
                let objram = self.map.region_data(Region::ObjRam);
                self.video.render_scanline(scanline, vram, objram);
            }
        }

        let vblank_cycle = VISIBLE_LINES * TIMING.cycles_per_scanline;
        if frame_cycle == vblank_cycle {
            self.vblank_nmi_pending = true;
        }
        if frame_cycle == 0 && self.clock > 0 {
            self.vblank_nmi_pending = false;
        }

        self.cpu.execute_cycle(bus, BusMaster::Cpu(0));

        // Tick the slower sound board in proportion (Bresenham).
        self.sound_acc += SOUND_CLOCK_HZ;
        while self.sound_acc >= TIMING.cpu_clock_hz {
            self.sound_acc -= TIMING.cpu_clock_hz;
            self.sound.tick();
        }

        self.clock += 1;
        self.watchdog_counter += 1;
    }

    pub fn check_interrupts(&self, target: BusMaster) -> InterruptState {
        let mut state = InterruptState::default();
        if let BusMaster::Cpu(0) = target {
            state.nmi = self.vblank_nmi_pending && self.nmi_enabled;
        }
        state
    }

    pub fn render_frame(&self, buffer: &mut [u8]) {
        self.video.render_frame(buffer);
    }

    /// Declarative orientation (base ROT90 composed with the live cocktail
    /// flip); applied centrally by the frontend.
    pub fn orientation(&self) -> phosphor_core::core::machine::Orientation {
        self.video.orientation()
    }

    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize {
        self.sound.fill_audio(out)
    }

    pub fn handle_input(&mut self, button: u8, pressed: bool) {
        // Scramble inputs are active-low; pressing clears the bit.
        match button {
            INPUT_COIN => crate::set_bit_active_low(&mut self.in0, 7, pressed), // coin1
            INPUT_P1_FIRE => crate::set_bit_active_low(&mut self.in0, 3, pressed), // button1
            INPUT_P1_BOMB => crate::set_bit_active_low(&mut self.in0, 1, pressed), // button2
            INPUT_P1_RIGHT => crate::set_bit_active_low(&mut self.in0, 4, pressed),
            INPUT_P1_LEFT => crate::set_bit_active_low(&mut self.in0, 5, pressed),
            INPUT_P1_UP => crate::set_bit_active_low(&mut self.in2, 4, pressed),
            INPUT_P1_DOWN => crate::set_bit_active_low(&mut self.in2, 6, pressed),
            INPUT_P1_START => crate::set_bit_active_low(&mut self.in1, 7, pressed),
            INPUT_P2_START => crate::set_bit_active_low(&mut self.in1, 6, pressed),
            _ => {}
        }
    }

    pub fn reset_board(&mut self) {
        self.video.reset();
        phosphor_core::device::Device::reset(&mut self.sound);
        self.ppi0.reset();
        self.nmi_enabled = false;
        self.vblank_nmi_pending = false;
        self.protection_state = 0;
        self.protection_result = 0;
        self.clock = 0;
        self.sound_acc = 0;
        self.watchdog_counter = 0;
        self.map.region_data_mut(Region::Ram).fill(0);
        self.map.region_data_mut(Region::VideoRam).fill(0);
        self.map.region_data_mut(Region::ObjRam).fill(0);
    }

    pub fn debug_tick_boundaries(&self) -> u32 {
        u32::from(self.cpu.at_instruction_boundary())
    }

    // -----------------------------------------------------------------------
    // Bus dispatch
    // -----------------------------------------------------------------------

    /// Read PPI #0 (the input port 8255), injecting the protection's high bit
    /// into IN2 bits 5/7 (Scramble only; Scobra/Frogger pass IN2 through).
    fn read_ppi0(&mut self, off: u16) -> u8 {
        self.ppi0.set_port_a_input(self.in0);
        self.ppi0.set_port_b_input(self.in1);
        let port_c = if self.hw == Hw::Scramble {
            let prot = if self.protection_result & 0x80 != 0 {
                0xa0
            } else {
                0x00
            };
            (self.in2 & !0xa0) | prot
        } else {
            self.in2
        };
        self.ppi0.set_port_c_input(port_c);
        self.ppi0.read(off)
    }

    /// Read PPI #1: port C is the protection on Scramble, else the Konami sound
    /// board's command 8255 (port C reads IN3 there).
    fn read_ppi1(&mut self, off: u16) -> u8 {
        if off == 2 && self.hw == Hw::Scramble {
            self.protection_result
        } else {
            self.sound.ppi_read(off)
        }
    }

    /// Write PPI #1: port C drives the protection on Scramble, else the sound
    /// command/control 8255.
    fn write_ppi1(&mut self, off: u16, data: u8) {
        if off == 2 && self.hw == Hw::Scramble {
            self.protection_w(data);
        } else {
            self.sound.ppi_write(off, data);
        }
    }

    pub fn bus_read_common(&mut self, addr: u16) -> u8 {
        let data = match self.hw {
            Hw::Frogger => self.frogger_read(addr),
            _ => self.scramble_read(addr),
        };
        self.map.watch_read(0, BusMaster::Cpu(0), addr, data);
        data
    }

    /// Scramble/Super Cobra read path (contiguous backing + range-decoded I/O).
    fn scramble_read(&mut self, addr: u16) -> u8 {
        if addr <= self.hw.backing_end() {
            self.map.read_backing(addr)
        } else {
            match self.hw {
                Hw::Scramble => match addr {
                    0x7000 | 0x7800 => {
                        self.watchdog_counter = 0;
                        0xff
                    }
                    // PPIs at 0x8000-0xffff: bit 8 = PPI0, bit 9 = PPI1.
                    0x8000..=0xffff => {
                        let mut r = 0xff;
                        if addr & 0x0100 != 0 {
                            r &= self.read_ppi0(addr & 3);
                        }
                        if addr & 0x0200 != 0 {
                            r &= self.read_ppi1(addr & 3);
                        }
                        r
                    }
                    _ => 0xff,
                },
                Hw::Scobra => match addr {
                    0x9800..=0x9803 => self.read_ppi0(addr & 3),
                    0xa000..=0xa003 => self.read_ppi1(addr & 3),
                    0xb000 => {
                        self.watchdog_counter = 0;
                        0xff
                    }
                    _ => 0xff,
                },
                Hw::Frogger => 0xff, // handled by frogger_read
            }
        }
    }

    pub fn bus_write_common(&mut self, addr: u16, data: u8) {
        self.map.watch_write(0, BusMaster::Cpu(0), addr, data);
        if self.hw == Hw::Frogger {
            self.frogger_write(addr, data);
            return;
        }
        let rb = self.hw.ram_base();
        // Work RAM / Video RAM / Object RAM (writable backing).
        if (rb..=rb + 0x10ff).contains(&addr) {
            self.map.write_backing(addr, data);
            return;
        }
        // The latch block (NMI/coin/background/stars/flip) is at 0x6800 on
        // Scramble, 0xa800 on Super Cobra.
        let latch_base = match self.hw {
            Hw::Scramble => 0x6800,
            Hw::Scobra => 0xa800,
            Hw::Frogger => unreachable!(),
        };
        if addr & 0xfff8 == latch_base {
            match addr & 7 {
                1 => {
                    self.nmi_enabled = data & 1 != 0;
                    if !self.nmi_enabled {
                        self.vblank_nmi_pending = false;
                    }
                }
                3 => self.video.set_background_enable(data & 1 != 0),
                4 => self.video.set_stars_enabled(data & 1 != 0),
                6 => self.video.set_flip_x(data & 1 != 0),
                7 => self.video.set_flip_y(data & 1 != 0),
                _ => {} // 2 = coin, 5 = POUT2 (not modeled)
            }
            return;
        }
        match self.hw {
            Hw::Scramble => {
                if addr & 0x0100 != 0 {
                    self.ppi0.write(addr & 3, data);
                }
                if addr & 0x0200 != 0 {
                    self.write_ppi1(addr & 3, data);
                }
            }
            Hw::Scobra => match addr {
                0x9800..=0x9803 => self.ppi0.write(addr & 3, data),
                0xa000..=0xa003 => self.write_ppi1(addr & 3, data),
                _ => {}
            },
            Hw::Frogger => unreachable!(), // handled above
        }
    }

    // -----------------------------------------------------------------------
    // Frogger bus dispatch (frogger_map)
    // -----------------------------------------------------------------------

    fn frogger_read(&mut self, addr: u16) -> u8 {
        match addr {
            // ROM, Work RAM.
            0x0000..=0x3fff | 0x8000..=0x87ff => self.map.read_backing(addr),
            // Watchdog (0x8800 mirror 0x07ff).
            0x8800..=0x8fff => {
                self.watchdog_counter = 0;
                0xff
            }
            // Video RAM (0xa800-0xabff + mirror at 0xac00).
            0xa800..=0xafff => self.map.read_backing(addr),
            // Object RAM (0xb000-0xb0ff, mirrored every 0x100 up to 0xb7ff).
            0xb000..=0xb7ff => self.map.read_backing(0xb000 | (addr & 0x00ff)),
            // PPIs: bit 12 = sound PPI #1, bit 13 = input PPI #0.
            0xc000..=0xffff => self.frogger_ppi_read(addr & 0x3fff),
            _ => 0xff,
        }
    }

    fn frogger_write(&mut self, addr: u16, data: u8) {
        match addr {
            0x8000..=0x87ff => self.map.write_backing(addr, data), // Work RAM
            0xa800..=0xafff => self.map.write_backing(addr, data), // Video RAM
            0xb000..=0xb7ff => self.map.write_backing(0xb000 | (addr & 0x00ff), data), // ObjRAM
            0xb800..=0xbfff => self.frogger_latch_w(addr, data),   // 74LS259 latch
            0xc000..=0xffff => self.frogger_ppi_write(addr & 0x3fff, data),
            _ => {}
        }
    }

    /// Frogger PPI decode: A12 selects the sound PPI #1, A13 the input PPI #0;
    /// the register index is on A1/A2 (`(offset >> 1) & 3`).
    fn frogger_ppi_read(&mut self, offset: u16) -> u8 {
        let reg = (offset >> 1) & 3;
        let mut r = 0xff;
        if offset & 0x1000 != 0 {
            r &= self.read_ppi1(reg);
        }
        if offset & 0x2000 != 0 {
            r &= self.read_ppi0(reg);
        }
        r
    }

    fn frogger_ppi_write(&mut self, offset: u16, data: u8) {
        let reg = (offset >> 1) & 3;
        if offset & 0x1000 != 0 {
            self.write_ppi1(reg, data);
        }
        if offset & 0x2000 != 0 {
            self.ppi0.write(reg, data);
        }
    }

    /// Frogger 74LS259 at 0xb800-0xbfff: the latch line is `(addr >> 2) & 7`
    /// (line 2 = NMI enable, 3 = flip Y, 4 = flip X, 6/7 = coin counters). The
    /// latched bit is D0.
    fn frogger_latch_w(&mut self, addr: u16, data: u8) {
        match (addr >> 2) & 7 {
            2 => {
                self.nmi_enabled = data & 1 != 0;
                if !self.nmi_enabled {
                    self.vblank_nmi_pending = false;
                }
            }
            3 => self.video.set_flip_y(data & 1 != 0),
            4 => self.video.set_flip_x(data & 1 != 0),
            _ => {} // 6, 7 = coin counters (not modeled)
        }
    }

    /// `theend_protection_w`: shift the low nibble into the state register and
    /// recompute the result based on the opcode nibble (MAME's reverse-engineered
    /// PAL behavior).
    fn protection_w(&mut self, data: u8) {
        self.protection_state = (self.protection_state << 4) | (data as u32 & 0x0f);
        let num1 = ((self.protection_state >> 8) & 0x0f) as i32;
        let num2 = ((self.protection_state >> 4) & 0x0f) as i32;
        match self.protection_state & 0x0f {
            0x6 => self.protection_result ^= 0x80,
            0x9 => self.protection_result = ((num1 + 1).min(0xf) as u8) << 4,
            0xb => self.protection_result = ((num2 - num1).max(0) as u8) << 4,
            0xa => self.protection_result = 0x00,
            0xf => self.protection_result = ((num1 - num2).max(0) as u8) << 4,
            _ => {}
        }
    }
}

impl Saveable for ScrambleBoard {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        w.write_bytes(self.map.region_data(Region::Ram));
        w.write_bytes(self.map.region_data(Region::VideoRam));
        w.write_bytes(self.map.region_data(Region::ObjRam));
        self.video.save_state(w);
        self.sound.save_state(w);
        self.ppi0.save_state(w);
        w.write_u8(self.in0);
        w.write_u8(self.in1);
        w.write_u8(self.in2);
        w.write_bool(self.nmi_enabled);
        w.write_bool(self.vblank_nmi_pending);
        w.write_u32_le(self.protection_state);
        w.write_u8(self.protection_result);
        w.write_u64_le(self.clock);
        w.write_u64_le(self.sound_acc);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        r.read_bytes_into(self.map.region_data_mut(Region::Ram))?;
        r.read_bytes_into(self.map.region_data_mut(Region::VideoRam))?;
        r.read_bytes_into(self.map.region_data_mut(Region::ObjRam))?;
        self.video.load_state(r)?;
        self.sound.load_state(r)?;
        self.ppi0.load_state(r)?;
        self.in0 = r.read_u8()?;
        self.in1 = r.read_u8()?;
        self.in2 = r.read_u8()?;
        self.nmi_enabled = r.read_bool()?;
        self.vblank_nmi_pending = r.read_bool()?;
        self.protection_state = r.read_u32_le()?;
        self.protection_result = r.read_u8()?;
        self.clock = r.read_u64_le()?;
        self.sound_acc = r.read_u64_le()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ROM definitions ("scramble" set)
// ---------------------------------------------------------------------------

pub static SCRAMBLE_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "s1.2d",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0xea35ccaa],
        },
        RomEntry {
            name: "s2.2e",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0xe7bba1b3],
        },
        RomEntry {
            name: "s3.2f",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0x12d7fc3e],
        },
        RomEntry {
            name: "s4.2h",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0xb59360eb],
        },
        RomEntry {
            name: "s5.2j",
            size: 0x0800,
            offset: 0x2000,
            crc32: &[0x4919a91c],
        },
        RomEntry {
            name: "s6.2l",
            size: 0x0800,
            offset: 0x2800,
            crc32: &[0x26a4547b],
        },
        RomEntry {
            name: "s7.2m",
            size: 0x0800,
            offset: 0x3000,
            crc32: &[0x0bb49470],
        },
        RomEntry {
            name: "s8.2p",
            size: 0x0800,
            offset: 0x3800,
            crc32: &[0x6a5740e5],
        },
    ],
};

pub static SCRAMBLE_SOUND_ROM: RomRegion = RomRegion {
    size: 0x1800,
    entries: &[
        RomEntry {
            name: "ot1.5c",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0xbcd297f0],
        },
        RomEntry {
            name: "ot2.5d",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0xde7912da],
        },
        RomEntry {
            name: "ot3.5e",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0xba2fa933],
        },
    ],
};

pub static SCRAMBLE_GFX_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[
        RomEntry {
            name: "c2.5f",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0x4708845b],
        },
        RomEntry {
            name: "c1.5h",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x11fd2887],
        },
    ],
};

pub static SCRAMBLE_COLOR_PROM: RomRegion = RomRegion {
    size: 0x0020,
    entries: &[RomEntry {
        name: "c01s.6e",
        size: 0x0020,
        offset: 0x0000,
        crc32: &[0x4e3caeab],
    }],
};

// ---------------------------------------------------------------------------
// DIP switches (MAME `scramble`): all on the IN1/IN2 input ports.
// ---------------------------------------------------------------------------

const SC_DIP1_MASK: u8 = 0x03; // IN1: Lives
const SC_DIP2_MASK: u8 = 0x0e; // IN2: Coinage (0x06) + Cabinet (0x08)

pub(crate) const SCRAMBLE_DIP_BANKS: &[DipSwitchBank] = &[
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
                    label: "4",
                    value: 0x01,
                },
                DipChoice {
                    label: "5",
                    value: 0x02,
                },
                DipChoice {
                    label: "Free Play",
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
                        label: "1 Coin/1 Credit",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "1 Coin/2 Credits",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "1 Coin/3 Credits",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "1 Coin/4 Credits",
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

pub const SCRAMBLE_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_P1_LEFT as u16),
        stable_name: "p1_left",
        label: "P1 Left",
        kind: InputKind::DigitalDirection {
            direction: Direction::Left,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(INPUT_P1_RIGHT as u16),
        stable_name: "p1_right",
        label: "P1 Right",
        kind: InputKind::DigitalDirection {
            direction: Direction::Right,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
    InputControl {
        id: InputId(INPUT_P1_UP as u16),
        stable_name: "p1_up",
        label: "P1 Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_UP,
    },
    InputControl {
        id: InputId(INPUT_P1_DOWN as u16),
        stable_name: "p1_down",
        label: "P1 Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_DOWN,
    },
    InputControl {
        id: InputId(INPUT_P1_FIRE as u16),
        stable_name: "p1_fire",
        label: "Fire",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_P1_BOMB as u16),
        stable_name: "p1_bomb",
        label: "Bomb",
        kind: InputKind::Action(ActionRole::Secondary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_COIN as u16),
        stable_name: "coin",
        label: "Coin",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(INPUT_P1_START as u16),
        stable_name: "p1_start",
        label: "P1 Start",
        kind: InputKind::Start,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_START,
    },
    InputControl {
        id: InputId(INPUT_P2_START as u16),
        stable_name: "p2_start",
        label: "P2 Start",
        kind: InputKind::Start,
        player: Some(2),
        default_bindings: crate::input_defaults::P2_START,
    },
];

// ---------------------------------------------------------------------------
// ScrambleSystem wrapper
// ---------------------------------------------------------------------------

/// Scramble (Konami, 1981).
#[derive(phosphor_macros::Saveable)]
pub struct ScrambleSystem {
    pub board: ScrambleBoard,
}

impl ScrambleSystem {
    pub fn new() -> Self {
        let mut board = ScrambleBoard::new(Hw::Scramble);
        // Apply factory-default DIPs (active-low: clear the configured bits).
        board.in1 &= !SC_DIP1_MASK;
        board.in2 &= !SC_DIP2_MASK;
        Self { board }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        self.board
            .load_program_rom(&SCRAMBLE_PROGRAM_ROM.load(rom_set)?);
        self.board
            .load_sound_rom(&SCRAMBLE_SOUND_ROM.load(rom_set)?);
        self.board.load_gfx_rom(&SCRAMBLE_GFX_ROM.load(rom_set)?);
        self.board
            .load_color_prom(&SCRAMBLE_COLOR_PROM.load(rom_set)?);
        Ok(())
    }
}

impl Default for ScrambleSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for ScrambleSystem {
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

crate::impl_board_delegation!(ScrambleSystem, board, TIMING, orientation);

impl MachineCore for ScrambleSystem {
    crate::machine_core_metadata!("scramble", TIMING);

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

impl SaveState for ScrambleSystem {
    crate::machine_save_state!();
}

impl Nvram for ScrambleSystem {}
impl Profilable for ScrambleSystem {}

impl InputConfigurable for ScrambleSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        SCRAMBLE_CONTROLS
    }
    fn handle_input(&mut self, event: InputEvent) {
        if let InputEvent::Button { id, pressed } = event {
            self.board.handle_input(id.0 as u8, pressed);
        }
    }
}

crate::impl_dip_switches!(
    ScrambleSystem,
    SCRAMBLE_DIP_BANKS,
    board.in1 & SC_DIP1_MASK,
    board.in2 & SC_DIP2_MASK
);

crate::impl_board_debug_trace!(ScrambleSystem, board);

crate::register_machine!(ScrambleSystem, "scramble", &["scramble"], SCRAMBLE_CONTROLS);

// ===========================================================================
// Super Cobra (Konami, 1981) — same hardware family, different memory map.
// ===========================================================================

// Padded to the full 0x8000 ROM region (the program populates only 0x0000-0x5fff).
pub static SCOBRA_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        RomEntry {
            name: "epr1265.2c",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xa0744b3f],
        },
        RomEntry {
            name: "2e",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x8e7245cd],
        },
        RomEntry {
            name: "epr1267.2f",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0x47a4e6fb],
        },
        RomEntry {
            name: "2h",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0x7244f21c],
        },
        RomEntry {
            name: "epr1269.2j",
            size: 0x1000,
            offset: 0x4000,
            crc32: &[0xe1f8a801],
        },
        RomEntry {
            name: "2l",
            size: 0x1000,
            offset: 0x5000,
            crc32: &[0xd52affde],
        },
    ],
};

pub static SCOBRA_SOUND_ROM: RomRegion = RomRegion {
    size: 0x1800,
    entries: &[
        RomEntry {
            name: "5c",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0xd4346959],
        },
        RomEntry {
            name: "5d",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0xcc025d95],
        },
        RomEntry {
            name: "5e",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0x1628c53f],
        },
    ],
};

// GFX plane order is reversed relative to Scramble (5h is plane 0 here).
pub static SCOBRA_GFX_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[
        RomEntry {
            name: "epr1274.5h",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0x64d113b4],
        },
        RomEntry {
            name: "epr1273.5f",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0xa96316d3],
        },
    ],
};

pub static SCOBRA_COLOR_PROM: RomRegion = RomRegion {
    size: 0x0020,
    entries: &[RomEntry {
        name: "82s123.6e",
        size: 0x0020,
        offset: 0x0000,
        crc32: &[0x9b87f90d],
    }],
};

const SCB_DIP1_MASK: u8 = 0x03; // IN1: Allow Continue + Lives
const SCB_DIP2_MASK: u8 = 0x0e; // IN2: Coinage + Cabinet

pub(crate) const SCOBRA_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "IN1",
        options: &[
            DipOption {
                name: "Allow Continue",
                mask: 0x01,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "No",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "4 Times",
                        value: 0x01,
                    },
                ],
            },
            DipOption {
                name: "Lives",
                mask: 0x02,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "3",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "4",
                        value: 0x02,
                    },
                ],
            },
        ],
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
                        label: "99 Credits",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "1 Coin/1 Credit",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "2 Coins/1 Credit",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "4 Coins/3 Credits",
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

/// Super Cobra (Konami, 1981) on the Scramble board with the `scobra_map`.
#[derive(phosphor_macros::Saveable)]
pub struct ScobraSystem {
    pub board: ScrambleBoard,
}

impl ScobraSystem {
    pub fn new() -> Self {
        let mut board = ScrambleBoard::new(Hw::Scobra);
        // Factory-default DIPs (active-low): IN1 = continue on + 3 lives (0x01),
        // IN2 = 1C/1C + upright (0x02).
        board.in1 = (board.in1 & !SCB_DIP1_MASK) | 0x01;
        board.in2 = (board.in2 & !SCB_DIP2_MASK) | 0x02;
        Self { board }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        self.board
            .load_program_rom(&SCOBRA_PROGRAM_ROM.load(rom_set)?);
        self.board.load_sound_rom(&SCOBRA_SOUND_ROM.load(rom_set)?);
        self.board.load_gfx_rom(&SCOBRA_GFX_ROM.load(rom_set)?);
        self.board
            .load_color_prom(&SCOBRA_COLOR_PROM.load(rom_set)?);
        Ok(())
    }
}

impl Default for ScobraSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for ScobraSystem {
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

crate::impl_board_delegation!(ScobraSystem, board, TIMING, orientation);

impl MachineCore for ScobraSystem {
    crate::machine_core_metadata!("scobra", TIMING);

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

impl SaveState for ScobraSystem {
    crate::machine_save_state!();
}

impl Nvram for ScobraSystem {}
impl Profilable for ScobraSystem {}

impl InputConfigurable for ScobraSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        SCRAMBLE_CONTROLS // same controls (2-button shooter)
    }
    fn handle_input(&mut self, event: InputEvent) {
        if let InputEvent::Button { id, pressed } = event {
            self.board.handle_input(id.0 as u8, pressed);
        }
    }
}

crate::impl_dip_switches!(
    ScobraSystem,
    SCOBRA_DIP_BANKS,
    board.in1 & SCB_DIP1_MASK,
    board.in2 & SCB_DIP2_MASK
);

crate::impl_board_debug_trace!(ScobraSystem, board);

crate::register_machine!(ScobraSystem, "scobra", &["scobra"], SCRAMBLE_CONTROLS);

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::DipSwitches;

    #[test]
    fn machine_id_and_defaults() {
        let sys = ScrambleSystem::new();
        assert_eq!(sys.machine_id(), "scramble");
        assert_eq!(sys.board.in0, 0xFF);
    }

    #[test]
    fn active_low_input() {
        let mut sys = ScrambleSystem::new();
        sys.board.handle_input(INPUT_COIN, true);
        assert_eq!(sys.board.in0 & 0x80, 0x00, "coin clears IN0 bit 7");
        sys.board.handle_input(INPUT_COIN, false);
        assert_eq!(sys.board.in0 & 0x80, 0x80, "release restores it");
    }

    #[test]
    fn dip_round_trip() {
        let mut sys = ScrambleSystem::new();
        sys.set_dip_bank_value(0, 0x02); // 5 lives
        assert_eq!(sys.dip_bank_value(0), 0x02);
    }

    #[test]
    fn scobra_layout_maps_ram_and_io() {
        let sys = ScobraSystem::new();
        assert_eq!(sys.machine_id(), "scobra");
        assert_eq!(sys.board.hw, Hw::Scobra);
        // Default DIPs (active-low): IN1 continue+3 lives, IN2 1C/1C upright.
        assert_eq!(sys.board.in1 & SCB_DIP1_MASK, 0x01);
        assert_eq!(sys.board.in2 & SCB_DIP2_MASK, 0x02);
    }

    /// Super Cobra shares Scramble's rotated monitor, so both wrappers have to
    /// forward the board's declared orientation. Scobra's delegation once
    /// omitted it and fell back to `NORMAL`, which drew the whole game 90° off
    /// while every existing test stayed green.
    #[test]
    fn scobra_declares_the_same_rotation_as_scramble() {
        use phosphor_core::core::machine::Renderable;
        let scobra = ScobraSystem::new();
        let scramble = ScrambleSystem::new();
        assert_eq!(scobra.orientation(), scramble.orientation());
        assert!(
            scobra.orientation().swaps_axes(),
            "the Scramble board is a portrait cabinet: {:?}",
            scobra.orientation()
        );
    }

    #[test]
    fn scobra_ram_round_trips_at_0x8000() {
        let mut b = ScrambleBoard::new(Hw::Scobra);
        b.bus_write_common(0x8000, 0xab);
        assert_eq!(b.bus_read_common(0x8000), 0xab);
        b.bus_write_common(0x8801, 0x42); // video RAM
        assert_eq!(b.bus_read_common(0x8801), 0x42);
    }

    #[test]
    fn theend_protection_scramble_op() {
        // The boot self-test writes 0x0A, 0x04, 0x09 to ppi1 port C and expects
        // 0xB0 back (op 0x9 = min(num1+1,0xf)<<4 with num1=0xA -> 0xB0).
        let mut b = ScrambleBoard::new(Hw::Scramble);
        for v in [0x0a, 0x04, 0x09] {
            b.protection_w(v);
        }
        assert_eq!(b.protection_result, 0xb0);
    }
}
