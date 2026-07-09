//! Xevious (Namco, 1983) — runs on the shared Namco Galaga hardware.
//!
//! Three Z80s (main/sub/sound @ 3.072 MHz), a Namco WSG for melodic sound, and
//! the Namco 06XX bus arbiter fronting the 50XX (score/protection), 51XX (I/O)
//! and 54XX (explosion sound) custom MCUs. Unlike Galaga/Dig Dug, the DIP
//! switches are read directly at 0x6800-0x6807 rather than through a custom
//! chip, and the video hardware has three layers (a scrolling background
//! tilemap, a foreground text tilemap and sprites) with independent per-layer
//! scroll. Background tile data lives in ROM and is fetched through a hardware
//! lookup at 0xF000-0xFFFF.
//!
//! This is the first milestone: board plumbing, the full CPU memory map and the
//! 50XX start-up protection handshake — enough to boot. Graphics decode and the
//! three-layer renderer land in a later milestone, so `render_frame` currently
//! produces a blank field.

use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug::{BusDebug, DebugCpu, Debuggable};
use phosphor_core::core::machine::{
    AudioSource, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, DipSwitches, MachineCore,
    MachineDebug, Renderable, SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::gfx;
use phosphor_macros::Saveable;

use crate::namco_galaga::{self, NamcoGalagaBoard};
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};

// ---------------------------------------------------------------------------
// ROM definitions
// ---------------------------------------------------------------------------

static XEVIOUS_MAIN_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "xvi_1.3p",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x09964dda],
        },
        RomEntry {
            name: "xvi_2.3m",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0x60ecce84],
        },
        RomEntry {
            name: "xvi_3.2m",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0x79754b7d],
        },
        RomEntry {
            name: "xvi_4.2l",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0xc7d4bbf0],
        },
    ],
};

static XEVIOUS_SUB_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "xvi_5.3f",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0xc85b703f],
        },
        RomEntry {
            name: "xvi_6.3j",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0xe18cdaad],
        },
    ],
};

static XEVIOUS_SOUND_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "xvi_7.2c",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0xdd35cf1c],
    }],
};

/// Background tilemap data ROMs (2A/2B/2C), addressed through the hardware
/// lookup at 0xF000-0xFFFF.
static XEVIOUS_GFX4_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "xvi_9.2a",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x57ed9879],
        },
        RomEntry {
            name: "xvi_10.2b",
            size: 0x2000,
            offset: 0x1000,
            crc32: &[0xae3ba9e5],
        },
        RomEntry {
            name: "xvi_11.2c",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0x31e244dd],
        },
    ],
};

/// Namco WSG waveform PROM (256 bytes).
static XEVIOUS_SOUND_PROM: RomRegion = RomRegion {
    size: 0x0100,
    entries: &[RomEntry {
        name: "xvi-2.7n",
        size: 0x0100,
        offset: 0x0000,
        crc32: &[0x550f06bc],
    }],
};

// ---------------------------------------------------------------------------
// XeviousSystem
// ---------------------------------------------------------------------------

/// Xevious Arcade System (Namco, 1983).
///
/// Screen: 288×224, rotated 90° CCW for a vertical display.
#[derive(Saveable)]
pub struct XeviousSystem {
    pub board: NamcoGalagaBoard,

    // RAM regions (shared by all three CPUs), 2KB each.
    work_ram: [u8; 0x800],    // 0x7800-0x7FFF
    sr1: [u8; 0x800],         // 0x8000-0x87FF (work RAM + sprite X/Y regs)
    sr2: [u8; 0x800],         // 0x9000-0x97FF (work RAM + sprite flip/size regs)
    sr3: [u8; 0x800],         // 0xA000-0xA7FF (work RAM + sprite tile/color regs)
    fg_colorram: [u8; 0x800], // 0xB000-0xB7FF
    bg_colorram: [u8; 0x800], // 0xB800-0xBFFF
    fg_videoram: [u8; 0x800], // 0xC000-0xC7FF
    bg_videoram: [u8; 0x800], // 0xC800-0xCFFF

    // Scroll latch (0xD000-0xD07F): 9-bit scroll per layer, plus flip.
    bg_scroll_x: u16,
    fg_scroll_x: u16,
    bg_scroll_y: u16,
    fg_scroll_y: u16,

    // Background-map lookup selector (two bytes written to 0xF000-0xFFFF).
    bs: [u8; 2],

    // Background tilemap data ROMs (2A/2B/2C combined), read via the lookup.
    #[save_skip]
    playfield_rom: Vec<u8>,

    // Frame buffer (288 × 224 native, rotated in render_frame). Blank until the
    // video milestone lands.
    #[save_skip]
    native_buffer: Vec<u8>,
    #[save_skip]
    palette: Vec<(u8, u8, u8)>,
}

impl XeviousSystem {
    pub fn new() -> Self {
        Self {
            board: NamcoGalagaBoard::new(),
            work_ram: [0; 0x800],
            sr1: [0; 0x800],
            sr2: [0; 0x800],
            sr3: [0; 0x800],
            fg_colorram: [0; 0x800],
            bg_colorram: [0; 0x800],
            fg_videoram: [0; 0x800],
            bg_videoram: [0; 0x800],
            bg_scroll_x: 0,
            fg_scroll_x: 0,
            bg_scroll_y: 0,
            fg_scroll_y: 0,
            bs: [0; 2],
            playfield_rom: Vec::new(),
            native_buffer: vec![0u8; 288 * 224],
            // Placeholder palette until the indirect-palette build lands.
            palette: vec![(0, 0, 0); 256],
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        // Program ROMs (per CPU).
        self.board.load_main_rom(&XEVIOUS_MAIN_ROM.load(rom_set)?);
        self.board.load_sub_rom(&XEVIOUS_SUB_ROM.load(rom_set)?);
        self.board.load_sound_rom(&XEVIOUS_SOUND_ROM.load(rom_set)?);

        // Background tilemap data ROMs, and the WSG waveform.
        self.playfield_rom = XEVIOUS_GFX4_ROM.load(rom_set)?;
        self.board
            .load_sound_prom(&XEVIOUS_SOUND_PROM.load(rom_set)?);

        // Fit the 50XX score/protection chip, which Xevious queries with a
        // periodic protection check.
        self.board.fit_50xx();

        // Factory DIP defaults: both banks read all-ones with switches at their
        // shipped positions and the button-2 lines released.
        self.board.dswa = 0xFF;
        self.board.dswb = 0xFF;

        Ok(())
    }

    /// Main-CPU program counter (for headless boot checks).
    pub fn main_pc(&self) -> u16 {
        self.board.main_cpu.pc
    }

    /// True once the main CPU has released the sub/sound CPUs from reset
    /// (LS259 Q3), which Xevious does only after clearing early init.
    pub fn sub_released(&self) -> bool {
        !self.board.sub_reset
    }

    /// Sub/sound CPU program counters and main IRQ enable state (boot checks).
    pub fn sub_pc(&self) -> u16 {
        self.board.sub_cpu.pc
    }
    pub fn sound_pc(&self) -> u16 {
        self.board.sound_cpu.pc
    }
    pub fn main_irq_on(&self) -> bool {
        self.board.main_irq_enabled
    }

    /// Count of non-zero bytes in the foreground and background video RAM —
    /// a proxy for "the attract screen has been drawn" during boot checks.
    pub fn video_ram_nonzero(&self) -> (usize, usize) {
        let fg = self.fg_videoram.iter().filter(|&&b| b != 0).count();
        let bg = self.bg_videoram.iter().filter(|&&b| b != 0).count();
        (fg, bg)
    }

    /// Read the interleaved DIP switch port at 0x6800-0x6807. Each address
    /// returns one bit of each bank: bit 0 from DSWB, bit 1 from DSWA.
    fn dsw_read(&self, offset: u8) -> u8 {
        let bit0 = (self.board.dswb >> offset) & 1;
        let bit1 = (self.board.dswa >> offset) & 1;
        bit0 | (bit1 << 1)
    }

    /// Write the video latch at 0xD000-0xD07F. The register is selected by
    /// address bits 4-7; the 9th scroll bit comes from address bit 0.
    fn write_video_latch(&mut self, offset: u8, data: u8) {
        let scroll = (data as u16) | (((offset & 0x01) as u16) << 8);
        match (offset >> 4) & 0x0F {
            0 => self.bg_scroll_x = scroll,
            1 => self.fg_scroll_x = scroll,
            2 => self.bg_scroll_y = scroll,
            3 => self.fg_scroll_y = scroll,
            7 => self.board.flip_screen = (scroll & 1) != 0,
            _ => {}
        }
    }

    /// Set one of the two background-map selector bytes (0xF000-0xFFFF write).
    fn write_bg_select(&mut self, addr: u16, data: u8) {
        self.bs[(addr & 1) as usize] = data;
    }

    /// Background-map lookup ("schematic 9B"). Given the current selector bytes,
    /// walk the 2A/2B/2C data ROMs to produce the tile number (even address) or
    /// its attribute byte (odd address) for the scrolling background layer.
    fn read_bg_map(&self, addr: u16) -> u8 {
        let rom = &self.playfield_rom;
        // Sub-ROM bases within the combined gfx4 region.
        let rom2a = 0x0000usize; // xvi_9  (0x1000)
        let rom2b = 0x1000usize; // xvi_10 (0x2000)
        let rom2c = 0x3000usize; // xvi_11 (0x1000)
        let at = |base: usize, i: usize| rom.get(base + i).copied().unwrap_or(0) as usize;

        let bs0 = self.bs[0] as usize;
        let bs1 = self.bs[1] as usize;

        // 12-bit address into 2A/2B.
        let adr_2b = ((bs1 & 0x7e) << 6) | ((bs0 & 0xfe) >> 1);
        let dat1 = if adr_2b & 1 != 0 {
            // High-nibble select from 2A.
            ((at(rom2a, adr_2b >> 1) & 0xf0) << 4) | at(rom2b, adr_2b)
        } else {
            // Low-nibble select from 2A.
            ((at(rom2a, adr_2b >> 1) & 0x0f) << 8) | at(rom2b, adr_2b)
        };

        let mut adr_2c = ((dat1 & 0x1ff) << 2) | ((bs1 & 1) << 1) | (bs0 & 1);
        if dat1 & 0x400 != 0 {
            adr_2c ^= 1;
        }
        if dat1 & 0x200 != 0 {
            adr_2c ^= 2;
        }

        if addr & 1 != 0 {
            // Attribute byte (BB1).
            at(rom2c, adr_2c | 0x800) as u8
        } else {
            // Tile number (BB0): swap bits 6 and 7, then apply the flip bits.
            let raw = at(rom2c, adr_2c) as u8;
            let mut dat2 = (raw & 0x3F) | ((raw & 0x40) << 1) | ((raw & 0x80) >> 1);
            if dat1 & 0x400 != 0 {
                dat2 ^= 0x40;
            }
            if dat1 & 0x200 != 0 {
                dat2 ^= 0x80;
            }
            dat2
        }
    }
}

impl Default for XeviousSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for XeviousSystem {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let data = match addr {
            0x0000..=0x3FFF => self.board.read_rom(master, addr),
            0x6800..=0x6807 => self.dsw_read((addr & 0x07) as u8),
            0x7000..=0x70FF => self.board.read_custom_io(),
            0x7100 => self.board.namco06.ctrl_read(),
            0x7800..=0x7FFF => self.work_ram[(addr - 0x7800) as usize],
            0x8000..=0x87FF => self.sr1[(addr - 0x8000) as usize],
            0x9000..=0x97FF => self.sr2[(addr - 0x9000) as usize],
            0xA000..=0xA7FF => self.sr3[(addr - 0xA000) as usize],
            0xB000..=0xB7FF => self.fg_colorram[(addr - 0xB000) as usize],
            0xB800..=0xBFFF => self.bg_colorram[(addr - 0xB800) as usize],
            0xC000..=0xC7FF => self.fg_videoram[(addr - 0xC000) as usize],
            0xC800..=0xCFFF => self.bg_videoram[(addr - 0xC800) as usize],
            0xF000..=0xFFFF => self.read_bg_map(addr),
            _ => 0xFF,
        };
        self.board.watch_read(master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.board.watch_write(master, addr, data);
        self.board.trace_bus_write(master, addr, data);
        match addr {
            0x0000..=0x3FFF => {} // ROM (nopw)
            0x6800..=0x681F => self.board.wsg.write(addr - 0x6800, data),
            0x6820..=0x6827 => {
                let bit = (addr & 7) as u8;
                self.board.write_misc_latch(bit, (data & 1) != 0);
            }
            0x6830 => self.board.watchdog_counter = 0,
            0x7000..=0x70FF => self.board.write_custom_io(data),
            0x7100 => self.board.write_custom_io_ctrl(data),
            0x7800..=0x7FFF => self.work_ram[(addr - 0x7800) as usize] = data,
            0x8000..=0x87FF => self.sr1[(addr - 0x8000) as usize] = data,
            0x9000..=0x97FF => self.sr2[(addr - 0x9000) as usize] = data,
            0xA000..=0xA7FF => self.sr3[(addr - 0xA000) as usize] = data,
            0xB000..=0xB7FF => self.fg_colorram[(addr - 0xB000) as usize] = data,
            0xB800..=0xBFFF => self.bg_colorram[(addr - 0xB800) as usize] = data,
            0xC000..=0xC7FF => self.fg_videoram[(addr - 0xC000) as usize] = data,
            0xC800..=0xCFFF => self.bg_videoram[(addr - 0xC800) as usize] = data,
            0xD000..=0xD07F => self.write_video_latch((addr & 0x7F) as u8, data),
            0xF000..=0xFFFF => self.write_bg_select(addr, data),
            _ => {}
        }
    }

    fn is_halted_for(&self, master: BusMaster) -> bool {
        self.board.is_halted_for(master)
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        self.board.check_interrupts(target)
    }
}

impl Renderable for XeviousSystem {
    fn display_size(&self) -> (u32, u32) {
        namco_galaga::TIMING.display_size()
    }

    fn display_aspect(&self) -> Option<(u32, u32)> {
        namco_galaga::TIMING.display_aspect()
    }

    fn render_frame(&self, buffer: &mut [u8]) {
        gfx::rotate_90_ccw_indexed(&self.native_buffer, buffer, 288, 224, &self.palette);
    }
}

impl AudioSource for XeviousSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.board.fill_audio(buffer)
    }

    fn audio_sample_rate(&self) -> u32 {
        44100
    }
}

impl BusDebug for XeviousSystem {
    fn devices(&self) -> Vec<(&str, &dyn Debuggable)> {
        vec![
            ("Z80 Main", &self.board.main_cpu as &dyn Debuggable),
            ("Z80 Sub", &self.board.sub_cpu as &dyn Debuggable),
            ("Z80 Sound", &self.board.sound_cpu as &dyn Debuggable),
            ("Namco WSG", &self.board.wsg as &dyn Debuggable),
            ("Namco 06XX", &self.board.namco06 as &dyn Debuggable),
            ("Namco 51XX", &self.board.namco51 as &dyn Debuggable),
        ]
    }

    fn cpus(&self) -> Vec<(&str, &dyn DebugCpu)> {
        vec![
            ("Z80 Main", &self.board.main_cpu as &dyn DebugCpu),
            ("Z80 Sub", &self.board.sub_cpu as &dyn DebugCpu),
            ("Z80 Sound", &self.board.sound_cpu as &dyn DebugCpu),
        ]
    }

    fn read(&self, cpu_index: usize, addr: u32) -> Option<u8> {
        let addr = u16::try_from(addr).ok()?;
        match addr {
            0x0000..=0x3FFF => {
                let rom = match cpu_index {
                    0 => &self.board.main_rom,
                    1 => &self.board.sub_rom,
                    2 => &self.board.sound_rom,
                    _ => return None,
                };
                Some(rom.get(addr as usize).copied().unwrap_or(0xFF))
            }
            0x7800..=0x7FFF => Some(self.work_ram[(addr - 0x7800) as usize]),
            0x8000..=0x87FF => Some(self.sr1[(addr - 0x8000) as usize]),
            0x9000..=0x97FF => Some(self.sr2[(addr - 0x9000) as usize]),
            0xA000..=0xA7FF => Some(self.sr3[(addr - 0xA000) as usize]),
            0xB000..=0xB7FF => Some(self.fg_colorram[(addr - 0xB000) as usize]),
            0xB800..=0xBFFF => Some(self.bg_colorram[(addr - 0xB800) as usize]),
            0xC000..=0xC7FF => Some(self.fg_videoram[(addr - 0xC000) as usize]),
            0xC800..=0xCFFF => Some(self.bg_videoram[(addr - 0xC800) as usize]),
            _ => None,
        }
    }

    fn write(&mut self, _cpu_index: usize, addr: u32, data: u8) {
        let Ok(addr) = u16::try_from(addr) else {
            return;
        };
        match addr {
            0x7800..=0x7FFF => self.work_ram[(addr - 0x7800) as usize] = data,
            0x8000..=0x87FF => self.sr1[(addr - 0x8000) as usize] = data,
            0x9000..=0x97FF => self.sr2[(addr - 0x9000) as usize] = data,
            0xA000..=0xA7FF => self.sr3[(addr - 0xA000) as usize] = data,
            0xB000..=0xB7FF => self.fg_colorram[(addr - 0xB000) as usize] = data,
            0xB800..=0xBFFF => self.bg_colorram[(addr - 0xB800) as usize] = data,
            0xC000..=0xC7FF => self.fg_videoram[(addr - 0xC000) as usize] = data,
            0xC800..=0xCFFF => self.bg_videoram[(addr - 0xC800) as usize] = data,
            _ => {}
        }
    }

    fn take_watchpoint_hit(&mut self) -> Option<phosphor_core::core::watchpoint::WatchpointHit> {
        self.board.watchpoints.take_hit()
    }

    fn set_watchpoint(
        &mut self,
        cpu_index: usize,
        addr: u32,
        kind: phosphor_core::core::watchpoint::WatchpointKind,
    ) {
        self.board.watchpoints.set(cpu_index, addr, kind);
    }

    fn clear_watchpoint(
        &mut self,
        cpu_index: usize,
        addr: u32,
        kind: phosphor_core::core::watchpoint::WatchpointKind,
    ) {
        self.board.watchpoints.clear(cpu_index, addr, kind);
    }

    fn clear_all_watchpoints(&mut self) {
        self.board.watchpoints.clear_all();
    }
}

impl MachineDebug for XeviousSystem {
    fn debug_bus(&self) -> Option<&dyn BusDebug> {
        Some(self)
    }

    fn debug_bus_mut(&mut self) -> Option<&mut dyn BusDebug> {
        Some(self)
    }

    fn cycles_per_frame(&self) -> u64 {
        namco_galaga::TIMING.cycles_per_frame()
    }

    fn debug_tick(&mut self) -> u32 {
        bus_split!(self, bus => {
            self.board.tick(bus);
        });
        self.board.debug_tick_boundaries()
    }
}

impl MachineCore for XeviousSystem {
    crate::machine_core_metadata!("xevious", namco_galaga::TIMING);

    fn run_frame(&mut self) {
        bus_split!(self, bus => {
            for _ in 0..namco_galaga::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });
        // Video rendering lands in the next milestone; the frame stays blank.
    }

    fn reset(&mut self) {
        self.board.reset_board();
        self.work_ram.fill(0);
        self.sr1.fill(0);
        self.sr2.fill(0);
        self.sr3.fill(0);
        self.fg_colorram.fill(0);
        self.bg_colorram.fill(0);
        self.fg_videoram.fill(0);
        self.bg_videoram.fill(0);
        self.bg_scroll_x = 0;
        self.fg_scroll_x = 0;
        self.bg_scroll_y = 0;
        self.fg_scroll_y = 0;
        self.bs = [0; 2];
        self.native_buffer.fill(0);

        bus_split!(self, bus => {
            self.board.main_cpu.reset(bus, BusMaster::Cpu(0));
            self.board.sub_cpu.reset(bus, BusMaster::Cpu(1));
            self.board.sound_cpu.reset(bus, BusMaster::Cpu(2));
        });
    }
}

impl SaveState for XeviousSystem {
    crate::machine_save_state!();
}

impl phosphor_core::core::machine::Nvram for XeviousSystem {}
impl phosphor_core::core::machine::InputConfigurable for XeviousSystem {
    fn input_controls(&self) -> &'static [phosphor_core::core::machine::InputControl] {
        namco_galaga::NAMCO_GALAGA_CONTROLS
    }

    fn handle_input(&mut self, event: phosphor_core::core::machine::InputEvent) {
        if let phosphor_core::core::machine::InputEvent::Button { id, pressed } = event {
            self.board.handle_input(id.0 as u8, pressed);
        }
    }
}
impl phosphor_core::core::machine::Profilable for XeviousSystem {}

// ---------------------------------------------------------------------------
// DIP switches
// ---------------------------------------------------------------------------

/// Xevious DIP banks (DSWA at board byte `dswa`, DSWB at `dswb`). Both banks
/// default to 0xFF at the factory settings. The DSWB button-2 (bomb) bits and
/// the conditional bonus-life option are not modelled yet and keep their
/// power-on value.
const XEVIOUS_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSWA",
        options: &[
            DipOption {
                name: "Coin A",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "2C/3C",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "2C/1C",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "1C/2C",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "1C/1C",
                        value: 0x03,
                    },
                ],
            },
            DipOption {
                name: "Lives",
                mask: 0x60,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "5",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "2",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "1",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "3",
                        value: 0x60,
                    },
                ],
            },
            DipOption {
                name: "Cabinet",
                mask: 0x80,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Cocktail",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Upright",
                        value: 0x80,
                    },
                ],
            },
        ],
    },
    DipSwitchBank {
        name: "DSWB",
        options: &[
            DipOption {
                name: "Flags Award Bonus Life",
                mask: 0x02,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "No",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Yes",
                        value: 0x02,
                    },
                ],
            },
            DipOption {
                name: "Coin B",
                mask: 0x0C,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "2C/3C",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "2C/1C",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "1C/2C",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "1C/1C",
                        value: 0x0C,
                    },
                ],
            },
            DipOption {
                name: "Difficulty",
                mask: 0x60,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Hardest",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Hard",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "Easy",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "Normal",
                        value: 0x60,
                    },
                ],
            },
            DipOption {
                name: "Freeze",
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
    },
];

impl DipSwitches for XeviousSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        XEVIOUS_DIP_BANKS
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        match bank {
            0 => self.board.dswa,
            1 => self.board.dswb,
            _ => 0,
        }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        match bank {
            0 => self.board.dswa = value,
            1 => self.board.dswb = value,
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = XeviousSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("xevious", &["xevious"], create_machine)
}

crate::impl_board_debug_trace!(XeviousSystem, board);
