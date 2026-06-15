use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    ActionRole, AnalogAxisKind, AudioSource, AxisSign, DefaultBinding, DipApplyTiming, DipChoice,
    DipOption, DipSwitchBank, DipSwitches, Direction, InputConfigurable, InputControl, InputEvent,
    InputId, InputKind, KeyId, MachineCore, MouseControl, PadAxis, PadButton, PadControl,
    Renderable, SaveState,
};
use phosphor_core::core::save_state::{self, SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::cpu::state::M6502State;
use phosphor_core::cpu::{Cpu, CpuStateTrait};
use phosphor_core::device::pokey::Pokey;
use phosphor_macros::{BusDebug, MemoryRegion};

use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_low;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
enum Region {
    Ram = 1,
    Io = 2,
    Rom = 3,
}

// ---------------------------------------------------------------------------
// Missile Command ROM definitions
// ---------------------------------------------------------------------------

/// Program ROM: 12KB at 0x5000-0x7FFF.
/// The last 2KB (0x7800-0x7FFF) is also mirrored to 0xF800-0xFFFF for vectors.
pub static MISSILE_COMMAND_ROM: RomRegion = RomRegion {
    size: 0x3000, // 12KB
    entries: &[
        RomEntry {
            name: "035820-02.h1",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0x7a62ce6a],
        },
        RomEntry {
            name: "035821-02.jk1",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0xdf3bd57f],
        },
        RomEntry {
            name: "035822-03e.kl1",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0x1a2f599a, 0xa1cd384a], // -03e (parent) and -02 (missile2)
        },
        RomEntry {
            name: "035823-02.lm1",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0x82e552bb],
        },
        RomEntry {
            name: "035824-02.np1",
            size: 0x0800,
            offset: 0x2000,
            crc32: &[0x606e42e0],
        },
        RomEntry {
            name: "035825-02.r1",
            size: 0x0800,
            offset: 0x2800,
            crc32: &[0xf752eaeb],
        },
    ],
};

// ---------------------------------------------------------------------------
// Input button IDs
// ---------------------------------------------------------------------------
pub const INPUT_COIN: u8 = 0;
pub const INPUT_START1: u8 = 1;
pub const INPUT_START2: u8 = 2;
pub const INPUT_FIRE_LEFT: u8 = 3;
pub const INPUT_FIRE_CENTER: u8 = 4;
pub const INPUT_FIRE_RIGHT: u8 = 5;
pub const INPUT_TRACK_L: u8 = 6;
pub const INPUT_TRACK_R: u8 = 7;
pub const INPUT_TRACK_U: u8 = 8;
pub const INPUT_TRACK_D: u8 = 9;

// ---------------------------------------------------------------------------
// Analog axis IDs (trackball)
// ---------------------------------------------------------------------------
pub const ANALOG_TRACKBALL_X: u8 = 0;
pub const ANALOG_TRACKBALL_Y: u8 = 1;

// Typed control ids for the analog axes. The digital controls reuse the
// `INPUT_*` numbering (0..=9); the trackball axes need ids distinct from those,
// since `InputId` is a single namespace.
const CTRL_TRACKBALL_X: InputId = InputId(10);
const CTRL_TRACKBALL_Y: InputId = InputId(11);

/// Typed logical controls. Default bindings reproduce the historical
/// name-matched keyboard/gamepad/mouse defaults.
const MISSILE_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_COIN as u16),
        stable_name: "coin",
        label: "Coin",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[
            DefaultBinding::Key(KeyId::Num5),
            DefaultBinding::Pad(PadControl::Button(PadButton::Back)),
        ],
    },
    InputControl {
        id: InputId(INPUT_START1 as u16),
        stable_name: "start1",
        label: "P1 Start",
        kind: InputKind::Start,
        player: Some(1),
        default_bindings: &[
            DefaultBinding::Key(KeyId::Num1),
            DefaultBinding::Pad(PadControl::Button(PadButton::Start)),
        ],
    },
    InputControl {
        id: InputId(INPUT_START2 as u16),
        stable_name: "start2",
        label: "P2 Start",
        kind: InputKind::Start,
        player: Some(2),
        default_bindings: &[DefaultBinding::Key(KeyId::Num2)],
    },
    InputControl {
        id: InputId(INPUT_FIRE_LEFT as u16),
        stable_name: "fire_left",
        label: "Fire Left",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        // Primary role plus the middle mouse button (the three-trackball cabinet
        // maps left/center/right fire to a mouse button each).
        default_bindings: &[DefaultBinding::Mouse(MouseControl::Middle)],
    },
    InputControl {
        id: InputId(INPUT_FIRE_CENTER as u16),
        stable_name: "fire_center",
        label: "Fire Center",
        kind: InputKind::Action(ActionRole::Secondary),
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::Left)],
    },
    InputControl {
        id: InputId(INPUT_FIRE_RIGHT as u16),
        stable_name: "fire_right",
        label: "Fire Right",
        kind: InputKind::Action(ActionRole::Tertiary),
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::Right)],
    },
    InputControl {
        id: InputId(INPUT_TRACK_L as u16),
        stable_name: "track_left",
        label: "P1 Left",
        kind: InputKind::DigitalDirection {
            direction: Direction::Left,
        },
        player: Some(1),
        default_bindings: &[
            DefaultBinding::Key(KeyId::Left),
            DefaultBinding::Pad(PadControl::Button(PadButton::DPadLeft)),
            DefaultBinding::Pad(PadControl::Axis(PadAxis::LeftX, AxisSign::Negative)),
        ],
    },
    InputControl {
        id: InputId(INPUT_TRACK_R as u16),
        stable_name: "track_right",
        label: "P1 Right",
        kind: InputKind::DigitalDirection {
            direction: Direction::Right,
        },
        player: Some(1),
        default_bindings: &[
            DefaultBinding::Key(KeyId::Right),
            DefaultBinding::Pad(PadControl::Button(PadButton::DPadRight)),
            DefaultBinding::Pad(PadControl::Axis(PadAxis::LeftX, AxisSign::Positive)),
        ],
    },
    InputControl {
        id: InputId(INPUT_TRACK_U as u16),
        stable_name: "track_up",
        label: "P1 Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(1),
        default_bindings: &[
            DefaultBinding::Key(KeyId::Up),
            DefaultBinding::Pad(PadControl::Button(PadButton::DPadUp)),
            DefaultBinding::Pad(PadControl::Axis(PadAxis::LeftY, AxisSign::Negative)),
        ],
    },
    InputControl {
        id: InputId(INPUT_TRACK_D as u16),
        stable_name: "track_down",
        label: "P1 Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(1),
        default_bindings: &[
            DefaultBinding::Key(KeyId::Down),
            DefaultBinding::Pad(PadControl::Button(PadButton::DPadDown)),
            DefaultBinding::Pad(PadControl::Axis(PadAxis::LeftY, AxisSign::Positive)),
        ],
    },
    InputControl {
        id: CTRL_TRACKBALL_X,
        stable_name: "trackball_x",
        label: "Trackball X",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::X,
        },
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::AxisX)],
    },
    InputControl {
        id: CTRL_TRACKBALL_Y,
        stable_name: "trackball_y",
        label: "Trackball Y",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::Y,
        },
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::AxisY)],
    },
];

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------
// Master clock: 10 MHz XTAL
// CPU clock: 10 MHz / 8 = 1.25 MHz
// Pixel clock: 10 MHz / 2 = 5 MHz
// HTOTAL: 320 pixel clocks → 320/4 = 80 CPU cycles per scanline
// VTOTAL: 256 scanlines per frame
// Frame rate: 5 MHz / (320 * 256) ≈ 61.04 Hz
const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 1_250_000, // 10 MHz / 8
    cycles_per_scanline: 80, // 320 pixel clocks / 4
    total_scanlines: 256,    // VTOTAL
    display_width: 256,
    display_height: 231,
};

/// Missile Command Arcade System (Atari, 1980)
///
/// Hardware: MOS 6502 @ 1.25 MHz, POKEY for sound/IO.
/// Video: 256x231 bitmap, bit-planar 2bpp (8-color with 3rd bit region
/// for bottom scanlines), 8-entry programmable palette at 0x4B00.
///
/// Memory map:
///   0x0000-0x3FFF  Video/Work RAM (16KB)
///   0x4000-0x400F  POKEY (mirrored across 0x4000-0x47FF)
///   0x4800         Read: IN0 (switches) or trackball (CTRLD-dependent)
///                  Write: Output latch (CTRLD, LEDs, coin counters, flip)
///   0x4900         Read: IN1 (fire buttons, VBLANK, tilt, test)
///   0x4A00         Read: DIP switches (pricing options)
///   0x4B00-0x4B07  Write: Color RAM (8 entries, 1-bit RGB)
///   0x4C00         Write: Watchdog reset
///   0x4D00         Write: IRQ acknowledge
///   0x5000-0x7FFF  Program ROM (12KB)
///   0xF800-0xFFFF  ROM mirror (vectors)
#[derive(BusDebug)]
pub struct MissileCommandSystem {
    #[debug_cpu("M6502")]
    cpu: M6502,
    #[debug_device("POKEY")]
    pokey: Pokey,

    #[debug_map(cpu = 0)]
    map: AddressSpace16,

    // I/O registers
    // IN0 at 0x4800 (active-low switches, directly stored active-low: 1=released, 0=pressed)
    //   Bit 7: Right Coin    Bit 6: Center Coin   Bit 5: Left Coin
    //   Bit 4: 1P Start      Bit 3: 2P Start
    //   Bit 2-0: Cocktail fire buttons (active-low)
    in0: u8,
    // IN1 at 0x4900 (mixed polarity)
    //   Bit 7: VBLANK (active-high, set dynamically)
    //   Bit 6: Self-test (active-low, normally 1)
    //   Bit 5: SLAM/Tilt (active-low, normally 1)
    //   Bit 4-3: Trackball direction (set dynamically)
    //   Bit 2: Fire Left (active-low, normally 1)
    //   Bit 1: Fire Center (active-low, normally 1)
    //   Bit 0: Fire Right (active-low, normally 1)
    in1: u8,
    // DIP switches at 0x4A00 (pricing options)
    dip_switches: u8,
    // CTRLD: bit 0 of output latch (0x4800 write) — selects trackball vs switches at 0x4800 read
    ctrld: bool,
    // Color RAM: 8 palette entries at 0x4B00-0x4B07
    palette: [u8; 8],

    // Trackball counters (4-bit each, combined into one byte when CTRLD=1)
    trackball_x: u8,
    trackball_y: u8,
    trackball_l_pressed: bool,
    trackball_r_pressed: bool,
    trackball_u_pressed: bool,
    trackball_d_pressed: bool,
    // Mouse accumulator: set_analog() adds here; tick() drains ±1 per tick
    // so the 4-bit counters never skip values and the game reads correct deltas.
    mouse_accum_x: i32,
    mouse_accum_y: i32,

    // IRQ state — based on /32V signal (inverted bit 5 of V counter)
    // Asserted at scanlines where 32V=0 (scanlines 0-31, 64-95, 128-159, 192-223)
    // Cleared by writing to 0x4D00 (IRQ acknowledge)
    irq_state: bool,

    // MADSEL circuit: intercepts (zp,X) addressing mode instructions (opcodes with
    // low 5 bits == 0x01) and redirects bus access 5 CPU cycles later to VRAM.
    // This is how the game writes pixels — without it, the screen stays blank.
    // Timed in CPU cycles (not master ticks) so clock halving doesn't break it.
    madsel_lastcycles: u64,
    stall_cycles: u8, // extra cycle penalty for 3rd-bit MUSHROOM MADSEL accesses

    // System
    clock: u64,
    cpu_cycles: u64,          // incremented only when CPU actually executes
    watchdog_frame_count: u8, // frames since last write to 0x4C00; resets machine at 8

    scanline_buffer: Vec<u8>,    // 256 * 231 * 3 = 177,408 bytes (RGB24)
    scanline_buffer_valid: bool, // true after run_frame() completes

    audio_buffer: Vec<i16>,
}

impl MissileCommandSystem {
    pub fn new() -> Self {
        Self {
            cpu: M6502::new(),
            pokey: Pokey::with_clock(1_250_000, 44100),
            map: Self::build_map(),
            in0: 0xFF,          // All buttons released (active-low: 1 = not pressed)
            in1: 0x67, // Fire buttons released (bits 0-2 = 1), test/tilt released (bits 5-6 = 1), VBLANK off
            dip_switches: 0x00, // Default DIP: 1 coin/1 play, English, standard options
            ctrld: false,
            palette: [0; 8],
            trackball_x: 0,
            trackball_y: 0,
            trackball_l_pressed: false,
            trackball_r_pressed: false,
            trackball_u_pressed: false,
            trackball_d_pressed: false,
            mouse_accum_x: 0,
            mouse_accum_y: 0,
            irq_state: false,
            madsel_lastcycles: 0,
            stall_cycles: 0,
            clock: 0,
            cpu_cycles: 0,
            watchdog_frame_count: 0,
            scanline_buffer: vec![0u8; 256 * 231 * 3],
            scanline_buffer_valid: false,
            audio_buffer: Vec::with_capacity(1024),
        }
    }

    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(Region::Ram, "RAM", 0x0000, 0x4000, AccessKind::ReadWrite)
            .region(Region::Io, "I/O", 0x4000, 0x1000, AccessKind::Io)
            .region(
                Region::Rom,
                "Program ROM",
                0x5000,
                0x3000,
                AccessKind::ReadOnly,
            )
            .mirror(0xF800, 0x7800, 0x0800);
        map
    }

    /// Current scanline (V counter), 0-255.
    pub fn current_scanline(&self) -> u16 {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();
        (frame_cycle / TIMING.cycles_per_scanline) as u16
    }

    pub fn tick(&mut self) {
        // Trackball movement: increment 4-bit counters from keyboard or mouse accumulator.
        // Keyboard: ±1 per tick while held. Mouse: drain ±1 per tick from accumulator.
        // Rate: every 1000 cycles ≈ 20 ticks/frame — enough for smooth crosshair tracking
        // while keeping deltas small enough for the 4-bit counter.
        if self.clock.is_multiple_of(1000) {
            if self.trackball_l_pressed {
                self.trackball_x = self.trackball_x.wrapping_sub(1) & 0x0F;
            }
            if self.trackball_r_pressed {
                self.trackball_x = self.trackball_x.wrapping_add(1) & 0x0F;
            }
            if self.trackball_u_pressed {
                self.trackball_y = self.trackball_y.wrapping_sub(1) & 0x0F;
            }
            if self.trackball_d_pressed {
                self.trackball_y = self.trackball_y.wrapping_add(1) & 0x0F;
            }
            // Drain mouse accumulator ±1 per tick
            if self.mouse_accum_x > 0 {
                self.trackball_x = self.trackball_x.wrapping_add(1) & 0x0F;
                self.mouse_accum_x -= 1;
            } else if self.mouse_accum_x < 0 {
                self.trackball_x = self.trackball_x.wrapping_sub(1) & 0x0F;
                self.mouse_accum_x += 1;
            }
            if self.mouse_accum_y > 0 {
                self.trackball_y = self.trackball_y.wrapping_add(1) & 0x0F;
                self.mouse_accum_y -= 1;
            } else if self.mouse_accum_y < 0 {
                self.trackball_y = self.trackball_y.wrapping_sub(1) & 0x0F;
                self.mouse_accum_y += 1;
            }
        }

        // Per-scanline rendering: at each scanline boundary, render the current
        // scanline from VRAM + palette before the CPU processes it, matching
        // hardware CRT read timing (the beam scans using VRAM at line start).
        let frame_cycle = self.clock % TIMING.cycles_per_frame();
        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let scanline = (frame_cycle / TIMING.cycles_per_scanline) as u16;
            if scanline >= 25 {
                self.render_scanline_to_buffer(scanline as usize);
            }

            // IRQ generation based on /32V signal
            // /IRQ is clocked by /16V transitions. When not flipped:
            //   At V=0,64,128,192 (32V=0): IRQ asserted
            //   At V=32,96,160,224 (32V=1): IRQ deasserted
            // The IRQ is latched on each SYNC (instruction fetch).
            if scanline.is_multiple_of(16) {
                self.irq_state = (scanline >> 5) & 1 == 0;
            }
        }

        // Update VBLANK bit in IN1 (bit 7, active-high)
        // VBLANK is active when V < 25
        let scanline = self.current_scanline();
        if scanline < 25 {
            self.in1 |= 0x80;
        } else {
            self.in1 &= !0x80;
        }

        // POKEY tick (runs at CPU clock rate = 1.25 MHz)
        self.pokey.tick();

        // CPU clock halving: at scanline 224+, CPU runs at MASTER_CLOCK/16 (0.625 MHz)
        // instead of MASTER_CLOCK/8 (1.25 MHz). We skip every other CPU cycle.
        // stall_cycles handles the extra cycle penalty for 3rd-bit MADSEL accesses.
        let run_cpu = if self.stall_cycles > 0 {
            self.stall_cycles -= 1;
            false
        } else if scanline >= 224 {
            self.clock.is_multiple_of(2)
        } else {
            true
        };

        if run_cpu {
            // Latch watchpoint attribution context (cycle + instruction PC)
            // before CPU execution — bus dispatch cannot read CPU state
            // mid-tick.
            if self.map.has_any_watchpoints() {
                let pc = self
                    .cpu
                    .at_instruction_boundary()
                    .then_some(self.cpu.pc as u32);
                self.map.latch_access_context(self.clock, pc);
            }

            bus_split!(self, bus => {
                self.cpu.execute_cycle(bus, BusMaster::Cpu(0));
            });
            self.cpu_cycles += 1;
        }

        self.clock += 1;
    }

    pub fn load_rom_set(
        &mut self,
        rom_set: &crate::rom_loader::RomSet,
    ) -> Result<(), crate::rom_loader::RomLoadError> {
        let rom_data = MISSILE_COMMAND_ROM.load(rom_set)?;
        self.map.load_region(Region::Rom, &rom_data);
        Ok(())
    }

    pub fn get_cpu_state(&self) -> M6502State {
        self.cpu.snapshot()
    }

    pub fn read_ram(&self, addr: usize) -> u8 {
        let ram = self.map.region_data(Region::Ram);
        if addr < ram.len() { ram[addr] } else { 0 }
    }

    pub fn write_ram(&mut self, addr: usize, data: u8) {
        let ram = self.map.region_data_mut(Region::Ram);
        if addr < ram.len() {
            ram[addr] = data;
        }
    }

    pub fn read_palette(&self, index: usize) -> u8 {
        if index < 8 { self.palette[index] } else { 0 }
    }

    pub fn clock(&self) -> u64 {
        self.clock
    }

    /// Check if the MADSEL signal is active. MADSEL goes high exactly 5 CPU
    /// cycles after arming and stays high for 1 cycle. Resets after firing.
    /// Uses cpu_cycles (not master clock) so clock halving doesn't break timing.
    fn get_madsel(&mut self) -> bool {
        if self.madsel_lastcycles > 0 {
            let elapsed = self.cpu_cycles.wrapping_sub(self.madsel_lastcycles);
            if elapsed == 5 {
                self.madsel_lastcycles = 0;
                return true;
            }
        }
        false
    }

    /// MADSEL write: redirect bus write to VRAM using bit-planar format.
    /// Address bits select VRAM byte and pixel within it.
    /// Data bits 7:6 select the 2-bit color value.
    /// Data bit 5 provides the 3rd color bit (for bottom scanlines).
    fn vram_madsel_write(&mut self, offset: u16, data: u8) {
        const DATA_LOOKUP: [u8; 4] = [0x00, 0x0F, 0xF0, 0xFF];

        // 2-bit planar write: VRAM address = offset >> 2
        let vramaddr = (offset >> 2) as usize;
        let pixel = offset & 3;
        let vramdata = DATA_LOOKUP[(data >> 6) as usize];
        let vrammask = !(0x11u8 << pixel);

        let ram = self.map.region_data_mut(Region::Ram);
        if vramaddr < ram.len() {
            ram[vramaddr] = (ram[vramaddr] & vrammask) | (vramdata & !vrammask);
        }

        // 3rd color bit write (MUSHROOM region): offset & 0xE000 == 0xE000
        // Extra cycle penalty: adjust_icount(-1)
        if (offset & 0xE000) == 0xE000 {
            let bit3_addr = Self::get_bit3_addr(offset) as usize;
            let bit3_data: u8 = if data & 0x20 != 0 { 0xFF } else { 0x00 };
            let bit3_mask = !(1u8 << (offset & 7));

            let ram = self.map.region_data_mut(Region::Ram);
            if bit3_addr < ram.len() {
                ram[bit3_addr] = (ram[bit3_addr] & bit3_mask) | (bit3_data & !bit3_mask);
            }
            self.stall_cycles += 1;
        }
    }

    /// MADSEL read: extract pixel color from VRAM and return in bits 7:6 (and bit 5
    /// for 3rd color bit region).
    fn vram_madsel_read(&mut self, offset: u16) -> u8 {
        let vramaddr = (offset >> 2) as usize;
        let vrammask = 0x11u8 << (offset & 3);
        let ram = self.map.region_data(Region::Ram);
        let vramdata = if vramaddr < ram.len() {
            ram[vramaddr] & vrammask
        } else {
            0
        };

        let mut result = 0xFFu8;
        if (vramdata & 0xF0) == 0 {
            result &= !0x80;
        }
        if (vramdata & 0x0F) == 0 {
            result &= !0x40;
        }

        // 3rd color bit read (MUSHROOM region)
        // Extra cycle penalty: adjust_icount(-1)
        if (offset & 0xE000) == 0xE000 {
            let bit3_addr = Self::get_bit3_addr(offset) as usize;
            let bit3_mask = 1u8 << (offset & 7);
            let ram = self.map.region_data(Region::Ram);
            let bit3_data = if bit3_addr < ram.len() {
                ram[bit3_addr] & bit3_mask
            } else {
                0
            };
            if bit3_data == 0 {
                result &= !0x20;
            }
            self.stall_cycles += 1;
        }

        result
    }

    /// Convert a 16-bit pixel address to a VRAM address for the 3rd color bit.
    pub fn get_bit3_addr(pixaddr: u16) -> u16 {
        ((pixaddr & 0x0800) >> 1)
            | ((!pixaddr & 0x0800) >> 2)
            | ((pixaddr & 0x07F8) >> 2)
            | ((pixaddr & 0x1000) >> 12)
    }

    /// Render the full frame directly from VRAM (fallback before first run_frame() completes).
    fn render_frame_from_vram(&self, buffer: &mut [u8]) {
        let (width, height) = self.display_size();
        let w = width as usize;
        let h = height as usize;

        // Resolve palette: each entry has 1-bit per RGB channel (inverted)
        // Bits 3/2/1 = ~R/~G/~B
        let mut palette_rgb = [(0u8, 0u8, 0u8); 8];
        for (i, rgb) in palette_rgb.iter_mut().enumerate() {
            let entry = self.palette[i];
            *rgb = (
                if entry & 0x08 == 0 { 255 } else { 0 }, // R = inverted bit 3
                if entry & 0x04 == 0 { 255 } else { 0 }, // G = inverted bit 2
                if entry & 0x02 == 0 { 255 } else { 0 }, // B = inverted bit 1
            );
        }

        let ram = self.map.region_data(Region::Ram);

        for screen_y in 0..h {
            let effy = screen_y + 25;
            let src_base = effy * 64;

            let bit3_base = if effy >= 224 {
                Some(Self::get_bit3_addr((effy as u16) << 8) as usize)
            } else {
                None
            };

            for screen_x in 0..w {
                let byte_offset = src_base + screen_x / 4;
                let pixel_in_byte = screen_x & 3;

                let byte = if byte_offset < 0x4000 {
                    ram[byte_offset]
                } else {
                    0
                };

                let pix = byte >> pixel_in_byte;
                let mut color_idx = ((pix >> 2) & 4) | ((pix << 1) & 2);

                if let Some(base) = bit3_base {
                    let bit3_offset = base + (screen_x / 8) * 2;
                    if bit3_offset < 0x4000 {
                        color_idx |= (ram[bit3_offset] >> (screen_x & 7)) & 1;
                    }
                }

                let (r, g, b) = palette_rgb[color_idx as usize];

                let pixel_offset = (screen_y * w + screen_x) * 3;
                buffer[pixel_offset] = r;
                buffer[pixel_offset + 1] = g;
                buffer[pixel_offset + 2] = b;
            }
        }
    }

    /// Render a single scanline from VRAM + palette into the internal scanline buffer.
    ///
    /// `effy` is the V counter value (25-255, where 25 = first visible line).
    /// Decodes the 8-entry palette, reads one VRAM row (64 bytes, 4 pixels/byte),
    /// and writes 256 RGB24 pixels to `self.scanline_buffer` at screen_y = effy - 25.
    /// For effy >= 224, the 3rd color bit region is used to enable 8-color output.
    pub fn render_scanline_to_buffer(&mut self, effy: usize) {
        // Resolve palette: each entry has 1-bit per RGB channel (inverted)
        // Bits 3/2/1 = ~R/~G/~B
        let mut palette_rgb = [(0u8, 0u8, 0u8); 8];
        for (i, rgb) in palette_rgb.iter_mut().enumerate() {
            let entry = self.palette[i];
            *rgb = (
                if entry & 0x08 == 0 { 255 } else { 0 }, // R = inverted bit 3
                if entry & 0x04 == 0 { 255 } else { 0 }, // G = inverted bit 2
                if entry & 0x02 == 0 { 255 } else { 0 }, // B = inverted bit 1
            );
        }

        let screen_y = effy - 25;
        let src_base = effy * 64;

        // Compute 3rd color bit base address for bottom scanlines
        let bit3_base = if effy >= 224 {
            Some(Self::get_bit3_addr((effy as u16) << 8) as usize)
        } else {
            None
        };

        let row_offset = screen_y * 256 * 3;

        let ram = self.map.region_data(Region::Ram);

        for screen_x in 0..256 {
            let byte_offset = src_base + screen_x / 4;
            let pixel_in_byte = screen_x & 3;

            let byte = if byte_offset < ram.len() {
                ram[byte_offset]
            } else {
                0
            };

            // Extract 2-bit color from bit-planar format
            let pix = byte >> pixel_in_byte;
            let mut color_idx = ((pix >> 2) & 4) | ((pix << 1) & 2);

            // Add 3rd color bit for bottom scanlines (effy >= 224)
            if let Some(base) = bit3_base {
                let bit3_offset = base + (screen_x / 8) * 2;
                if bit3_offset < ram.len() {
                    color_idx |= (ram[bit3_offset] >> (screen_x & 7)) & 1;
                }
            }

            let (r, g, b) = palette_rgb[color_idx as usize];

            let pixel_offset = row_offset + screen_x * 3;
            self.scanline_buffer[pixel_offset] = r;
            self.scanline_buffer[pixel_offset + 1] = g;
            self.scanline_buffer[pixel_offset + 2] = b;
        }
    }
}

impl Default for MissileCommandSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for MissileCommandSystem {
    type Address = u16;
    type Data = u8;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false // No DMA hardware on Missile Command
    }

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        // MADSEL check: if active, redirect read to VRAM (bypasses normal decoding)
        if self.get_madsel() {
            return self.vram_madsel_read(addr);
        }

        // 15-bit address bus masking (global_mask 0x7FFF).
        // The 6502 vectors at 0xFFFC map through: 0xFFFC & 0x7FFF = 0x7FFC → ROM.
        let addr = addr & 0x7FFF;

        let data = match self.map.page(addr).region_id {
            Region::RAM | Region::ROM => self.map.read_backing(addr),

            Region::IO => match addr {
                0x4000..=0x47FF => self.pokey.read(addr & 0x0F),
                0x4800..=0x48FF => {
                    if self.ctrld {
                        (self.trackball_y << 4) | (self.trackball_x & 0x0F)
                    } else {
                        self.in0
                    }
                }
                0x4900..=0x49FF => self.in1,
                0x4A00..=0x4AFF => self.dip_switches,
                _ => 0xFF,
            },

            _ => 0xFF,
        };

        // MADSEL arming: during SYNC (opcode fetch), if the opcode has low 5 bits
        // == 0x01 (indirect X addressing mode) and IRQ is not asserted, arm the
        // MADSEL counter. It will fire 5 CPU cycles later.
        if self.cpu.is_sync() && (data & 0x1F) == 0x01 && !self.irq_state {
            self.madsel_lastcycles = self.cpu_cycles;
        }

        self.map.watch_read(0, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        // MADSEL check: if active, redirect write to VRAM (bypasses normal decoding)
        if self.get_madsel() {
            self.vram_madsel_write(addr, data);
            return;
        }

        // 15-bit address bus masking
        let addr = addr & 0x7FFF;
        self.map.watch_write(0, master, addr, data);

        match self.map.page(addr).region_id {
            Region::RAM => self.map.write_backing(addr, data),

            Region::IO => match addr {
                0x4000..=0x47FF => self.pokey.write(addr & 0x0F, data),
                0x4800..=0x48FF => {
                    self.ctrld = (data & 1) != 0;
                }
                0x4B00..=0x4BFF => {
                    self.palette[(addr & 0x07) as usize] = data;
                }
                0x4C00..=0x4CFF => {
                    self.watchdog_frame_count = 0;
                }
                0x4D00..=0x4DFF => {
                    self.irq_state = false;
                }
                _ => {}
            },

            _ => {} // ROM and unmapped: writes ignored
        }
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            nmi: false,
            irq: self.irq_state,
            firq: false,
            irq_vector: 0,
            irq_level: 0,
        }
    }
}

impl Renderable for MissileCommandSystem {
    fn display_size(&self) -> (u32, u32) {
        TIMING.display_size()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        if self.scanline_buffer_valid {
            buffer.copy_from_slice(&self.scanline_buffer);
        } else {
            self.render_frame_from_vram(buffer);
        }
    }
}

impl AudioSource for MissileCommandSystem {
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

impl InputConfigurable for MissileCommandSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        MISSILE_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Button { id, pressed } => match id.0 as u8 {
                // IN0 switches (active-low: clear bit when pressed, set when released)
                INPUT_COIN => set_bit_active_low(&mut self.in0, 5, pressed), // Left Coin
                INPUT_START1 => set_bit_active_low(&mut self.in0, 4, pressed), // 1P Start
                INPUT_START2 => set_bit_active_low(&mut self.in0, 3, pressed), // 2P Start

                // IN1 fire buttons (active-low: clear bit when pressed, set when released)
                INPUT_FIRE_LEFT => set_bit_active_low(&mut self.in1, 2, pressed), // Left fire
                INPUT_FIRE_CENTER => set_bit_active_low(&mut self.in1, 1, pressed), // Center fire
                INPUT_FIRE_RIGHT => set_bit_active_low(&mut self.in1, 0, pressed), // Right fire

                // Trackball directions
                INPUT_TRACK_L => self.trackball_l_pressed = pressed,
                INPUT_TRACK_R => self.trackball_r_pressed = pressed,
                INPUT_TRACK_U => self.trackball_u_pressed = pressed,
                INPUT_TRACK_D => self.trackball_d_pressed = pressed,
                _ => {}
            },
            InputEvent::Relative { id, delta } => {
                let delta = delta as i32;
                if id == CTRL_TRACKBALL_X {
                    self.mouse_accum_x += delta;
                } else if id == CTRL_TRACKBALL_Y {
                    // Y axis inverted: mouse down (positive delta) moves the crosshair
                    // down on screen, but the trackball counter must decrease.
                    self.mouse_accum_y -= delta;
                }
            }
            InputEvent::Absolute { .. } => {}
        }
    }
}

crate::impl_standalone_debug!(MissileCommandSystem);

impl Saveable for MissileCommandSystem {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        self.pokey.save_state(w);
        w.write_bytes(self.map.region_data(Region::Ram));
        w.write_u8(self.in0);
        w.write_u8(self.in1);
        w.write_bool(self.ctrld);
        w.write_bytes(&self.palette);
        w.write_u8(self.trackball_x);
        w.write_u8(self.trackball_y);
        w.write_bool(self.trackball_l_pressed);
        w.write_bool(self.trackball_r_pressed);
        w.write_bool(self.trackball_u_pressed);
        w.write_bool(self.trackball_d_pressed);
        w.write_i32_le(self.mouse_accum_x);
        w.write_i32_le(self.mouse_accum_y);
        w.write_bool(self.irq_state);
        w.write_u64_le(self.madsel_lastcycles);
        w.write_u8(self.stall_cycles);
        w.write_u64_le(self.clock);
        w.write_u64_le(self.cpu_cycles);
        w.write_u8(self.watchdog_frame_count);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        self.pokey.load_state(r)?;
        r.read_bytes_into(self.map.region_data_mut(Region::Ram))?;
        self.in0 = r.read_u8()?;
        self.in1 = r.read_u8()?;
        self.ctrld = r.read_bool()?;
        r.read_bytes_into(&mut self.palette)?;
        self.trackball_x = r.read_u8()?;
        self.trackball_y = r.read_u8()?;
        self.trackball_l_pressed = r.read_bool()?;
        self.trackball_r_pressed = r.read_bool()?;
        self.trackball_u_pressed = r.read_bool()?;
        self.trackball_d_pressed = r.read_bool()?;
        self.mouse_accum_x = r.read_i32_le()?;
        self.mouse_accum_y = r.read_i32_le()?;
        self.irq_state = r.read_bool()?;
        self.madsel_lastcycles = r.read_u64_le()?;
        self.stall_cycles = r.read_u8()?;
        self.clock = r.read_u64_le()?;
        self.cpu_cycles = r.read_u64_le()?;
        self.watchdog_frame_count = r.read_u8()?;
        self.scanline_buffer_valid = false;
        self.audio_buffer.clear();
        Ok(())
    }
}

impl MachineCore for MissileCommandSystem {
    fn run_frame(&mut self) {
        for _ in 0..TIMING.cycles_per_frame() {
            self.tick();
        }
        self.scanline_buffer_valid = true;

        // Watchdog: 8-VBLANK timeout. If the game hasn't written
        // to 0x4C00 within 8 frames, reset the machine.
        self.watchdog_frame_count += 1;
        if self.watchdog_frame_count >= 8 {
            self.reset();
            return;
        }

        // Drain POKEY's resampled f32 buffer and convert to i16 PCM.
        // POKEY outputs unipolar [0.0, 1.0]; center around zero for signed PCM.
        let samples = self.pokey.drain_audio();
        self.audio_buffer
            .extend(samples.iter().map(|&s| ((s * 2.0 - 1.0) * 32767.0) as i16));
    }

    fn reset(&mut self) {
        self.irq_state = false;
        self.madsel_lastcycles = 0;
        self.stall_cycles = 0;
        self.watchdog_frame_count = 0;
        self.scanline_buffer.fill(0);
        self.scanline_buffer_valid = false;
        self.audio_buffer.clear();

        bus_split!(self, bus => {
            self.cpu.reset(bus, BusMaster::Cpu(0));
        });
    }

    fn frame_rate_hz(&self) -> f64 {
        TIMING.frame_rate_hz()
    }

    fn machine_id(&self) -> &str {
        "missile_command"
    }
}

impl SaveState for MissileCommandSystem {
    fn save_state(&self) -> Option<Vec<u8>> {
        Some(save_state::save_machine(self, self.machine_id()))
    }

    fn load_state(&mut self, data: &[u8]) -> Result<(), SaveError> {
        let id = self.machine_id().to_string();
        save_state::load_machine(self, &id, data)
    }
}

// No battery RAM, sub-span profiling, or event tracing; DIP switches are real
// (see the DipSwitches impl below).
crate::impl_default_frontend_capabilities!(MissileCommandSystem);

/// DIP switch metadata for Missile Command's R10 pricing DIP (read at 0x4A00).
/// The separate R8 game-options bank (cities, bonus city, cabinet) is not
/// emulated as a settable byte. Choice bits and labels follow MAME's `missile`
/// R10 layout; option defaults OR to the historical 0x00.
const MISSILE_DIP_BANKS: &[DipSwitchBank] = &[DipSwitchBank {
    name: "R10 (Pricing)",
    options: &[
        DipOption {
            name: "Coinage",
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
                    label: "Free Play",
                    value: 0x02,
                },
                DipChoice {
                    label: "1 Coin/2 Credits",
                    value: 0x03,
                },
            ],
        },
        DipOption {
            name: "Right Coin",
            mask: 0x0C,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "x1",
                    value: 0x00,
                },
                DipChoice {
                    label: "x4",
                    value: 0x04,
                },
                DipChoice {
                    label: "x5",
                    value: 0x08,
                },
                DipChoice {
                    label: "x6",
                    value: 0x0C,
                },
            ],
        },
        DipOption {
            name: "Center Coin",
            mask: 0x10,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "x1",
                    value: 0x00,
                },
                DipChoice {
                    label: "x2",
                    value: 0x10,
                },
            ],
        },
        DipOption {
            name: "Language",
            mask: 0x60,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "English",
                    value: 0x00,
                },
                DipChoice {
                    label: "French",
                    value: 0x20,
                },
                DipChoice {
                    label: "German",
                    value: 0x40,
                },
                DipChoice {
                    label: "Spanish",
                    value: 0x60,
                },
            ],
        },
        DipOption {
            name: "Unknown",
            mask: 0x80,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "On",
                    value: 0x00,
                },
                DipChoice {
                    label: "Off",
                    value: 0x80,
                },
            ],
        },
    ],
}];

impl DipSwitches for MissileCommandSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        MISSILE_DIP_BANKS
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

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = MissileCommandSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("missile", &["missile"], create_machine)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::cpu::CpuStateTrait;

    #[test]
    fn dip_default_and_metadata() {
        let sys = MissileCommandSystem::new();
        assert_eq!(sys.dip_bank_value(0), 0x00);
        crate::assert_dip_banks_valid(sys.dip_banks(), &[sys.dip_bank_value(0)]);
    }

    #[test]
    fn set_dip_option_masks_only_its_bits() {
        let mut sys = MissileCommandSystem::new();
        // Language is option 3 (mask 0x60); pick "German" (0x40).
        sys.set_dip_option(0, 3, 0x40);
        assert_eq!(sys.dip_bank_value(0), 0x40);
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = MissileCommandSystem::new();

        // Set known state
        sys.map.region_data_mut(Region::Ram)[0x100] = 0xAA;
        sys.in0 = 0xEE;
        sys.in1 = 0x77;
        sys.ctrld = true;
        sys.palette[3] = 0x0E;
        sys.trackball_x = 7;
        sys.trackball_y = 12;
        sys.trackball_l_pressed = true;
        sys.trackball_d_pressed = true;
        sys.mouse_accum_x = -7;
        sys.mouse_accum_y = 12;
        sys.irq_state = true;
        sys.madsel_lastcycles = 42;
        sys.stall_cycles = 1;
        sys.clock = 100_000;
        sys.cpu_cycles = 80_000;
        sys.watchdog_frame_count = 5;

        // Save
        let data = SaveState::save_state(&sys).expect("save_state should return Some");
        let cpu_snap = sys.cpu.snapshot();

        // Mutate everything
        let mut sys2 = MissileCommandSystem::new();
        sys2.map.region_data_mut(Region::Ram)[0x100] = 0xFF;
        sys2.clock = 999;

        // Load
        SaveState::load_state(&mut sys2, &data).unwrap();

        // Verify
        assert_eq!(sys2.cpu.snapshot(), cpu_snap);
        assert_eq!(sys2.map.region_data_mut(Region::Ram)[0x100], 0xAA);
        assert_eq!(sys2.in0, 0xEE);
        assert_eq!(sys2.in1, 0x77);
        assert!(sys2.ctrld);
        assert_eq!(sys2.palette[3], 0x0E);
        assert_eq!(sys2.trackball_x, 7);
        assert_eq!(sys2.trackball_y, 12);
        assert!(sys2.trackball_l_pressed);
        assert!(sys2.trackball_d_pressed);
        assert_eq!(sys2.mouse_accum_x, -7);
        assert_eq!(sys2.mouse_accum_y, 12);
        assert!(sys2.irq_state);
        assert_eq!(sys2.madsel_lastcycles, 42);
        assert_eq!(sys2.stall_cycles, 1);
        assert_eq!(sys2.clock, 100_000);
        assert_eq!(sys2.cpu_cycles, 80_000);
        assert_eq!(sys2.watchdog_frame_count, 5);
        assert!(!sys2.scanline_buffer_valid);
    }

    #[test]
    fn save_does_not_include_rom() {
        let mut sys = MissileCommandSystem::new();
        sys.map.region_data_mut(Region::Rom)[0] = 0xDE;

        let data = SaveState::save_state(&sys).unwrap();

        let mut sys2 = MissileCommandSystem::new();
        SaveState::load_state(&mut sys2, &data).unwrap();

        assert_eq!(sys2.map.region_data_mut(Region::Rom)[0], 0x00);
    }

    #[test]
    fn set_analog_accumulates_x() {
        let mut sys = MissileCommandSystem::new();
        sys.handle_input(InputEvent::Relative {
            id: CTRL_TRACKBALL_X,
            delta: (3) as f32,
        });
        assert_eq!(sys.mouse_accum_x, 3);
        // Counter unchanged until tick() drains
        assert_eq!(sys.trackball_x, 0);
    }

    #[test]
    fn set_analog_accumulates_y_inverted() {
        let mut sys = MissileCommandSystem::new();
        sys.handle_input(InputEvent::Relative {
            id: CTRL_TRACKBALL_Y,
            delta: (-5) as f32,
        });
        // Y axis is inverted: negative mouse delta → positive accumulator
        assert_eq!(sys.mouse_accum_y, 5);
    }

    #[test]
    fn tick_drains_mouse_accum_positive() {
        let mut sys = MissileCommandSystem::new();
        sys.mouse_accum_x = 3;
        // Run enough ticks to drain (tick fires every 1000 cycles)
        for _ in 0..3000 {
            sys.tick();
            sys.clock += 1;
        }
        assert_eq!(sys.trackball_x, 3);
        assert_eq!(sys.mouse_accum_x, 0);
    }

    #[test]
    fn tick_drains_mouse_accum_negative() {
        let mut sys = MissileCommandSystem::new();
        sys.trackball_y = 5;
        sys.mouse_accum_y = -3;
        for _ in 0..3000 {
            sys.tick();
            sys.clock += 1;
        }
        assert_eq!(sys.trackball_y, 2);
        assert_eq!(sys.mouse_accum_y, 0);
    }

    #[test]
    fn tick_drains_mouse_accum_wraps_4_bit() {
        let mut sys = MissileCommandSystem::new();
        sys.trackball_x = 14;
        sys.mouse_accum_x = 5;
        for _ in 0..5000 {
            sys.tick();
            sys.clock += 1;
        }
        assert_eq!(sys.trackball_x, 3); // (14 + 5) & 0x0F = 3
        assert_eq!(sys.mouse_accum_x, 0);
    }

    #[test]
    fn exposes_two_analog_axes() {
        let sys = MissileCommandSystem::new();
        let axes: Vec<&str> = sys
            .input_controls()
            .iter()
            .filter(|c| matches!(c.kind, InputKind::AnalogAxis { .. }))
            .map(|c| c.label)
            .collect();
        assert_eq!(axes, vec!["Trackball X", "Trackball Y"]);
    }

    #[test]
    fn handle_input_relative_accumulates_like_set_analog() {
        // Typed Relative events feed the same accumulators as set_analog,
        // including the Y inversion.
        let mut sys = MissileCommandSystem::new();
        sys.handle_input(InputEvent::Relative {
            id: CTRL_TRACKBALL_X,
            delta: 3.0,
        });
        sys.handle_input(InputEvent::Relative {
            id: CTRL_TRACKBALL_Y,
            delta: -5.0,
        });
        assert_eq!(sys.mouse_accum_x, 3);
        assert_eq!(sys.mouse_accum_y, 5);
    }

    #[test]
    fn handle_input_button_matches_set_input() {
        let mut typed = MissileCommandSystem::new();
        typed.handle_input(InputEvent::Button {
            id: InputId(INPUT_FIRE_CENTER as u16),
            pressed: true,
        });
        let mut legacy = MissileCommandSystem::new();
        legacy.handle_input(InputEvent::Button {
            id: InputId((INPUT_FIRE_CENTER) as u16),
            pressed: true,
        });
        // Active-low: pressing clears IN1 bit 1.
        assert_eq!(typed.in1 & 0b10, 0);
        assert_eq!(typed.in1, legacy.in1);
    }

    #[test]
    fn input_controls_include_analog_axes() {
        let sys = MissileCommandSystem::new();
        let controls = sys.input_controls();
        assert_eq!(controls.len(), 12); // 10 digital + 2 analog axes
        assert!(
            controls.iter().any(|c| c.stable_name == "trackball_x"
                && matches!(c.kind, InputKind::AnalogAxis { .. }))
        );
    }
}
