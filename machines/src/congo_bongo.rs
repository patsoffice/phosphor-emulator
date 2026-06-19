//! Sega Congo Bongo (1983) — Zaxxon-family hardware.
//!
//! Hardware (per MAME `src/mame/sega/zaxxon.cpp`, the `congo` set):
//! - Main CPU: Z80 @ MASTER_CLOCK/16 = 48.66 MHz / 16 ≈ 3.041 MHz
//! - Sound CPU: Z80 @ 4 MHz, with a ~244 Hz periodic IRQ (`SOUND_CLOCK/16/16/16/4`)
//! - Video: 256×224 raster, ROT90 (portrait 224×256), VBlank IRQ on the main CPU
//! - Foreground: 32×32 8×8 2bpp tilemap + per-tile color RAM, fg-bank/color-bank latches
//! - Background: 8×8 3bpp `tilemap_dat` map, 11-bit scroll with an isometric skew
//! - Sprites: 32×32 3bpp, moved into a 256-byte sprite RAM by a custom DMA engine
//! - Sound: 2× SN76489A + i8255 PPI driving 5 synthesized percussion voices
//!
//! This is the **skeleton** (issue `phosphor-emulator-5tf.3`): ROM regions, the
//! dual-Z80 memory maps, the board/system structs, registry + disasm regions, and
//! a main-CPU run loop with the VBlank IRQ. It boots and renders a (black) frame
//! of the correct size. Graphics decode/palette, the three render layers, input,
//! DIP switches, and the entire sound path are added by the follow-up issues
//! (`.4`–`.10`); the relevant fields and maps are wired here so those changes are
//! additive.

use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::DebugTraceBuffer;
use phosphor_core::core::machine::{
    DipSwitchBank, DipSwitches, InputConfigurable, InputControl, InputEvent, MachineCore, Nvram,
    SaveState,
};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::z80::Z80;
use phosphor_core::gfx;
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion, Saveable};

use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};

// ---------------------------------------------------------------------------
// Memory map region IDs
// ---------------------------------------------------------------------------

/// Main CPU (Z80) address space regions.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum MainRegion {
    Rom = 1,      // 0x0000-0x7FFF (32KB program ROM)
    Ram = 2,      // 0x8000-0x8FFF (4KB work RAM)
    VideoRam = 3, // 0xA000-0xA3FF (1KB tilemap RAM, mirrored at 0xA800)
    ColorRam = 4, // 0xA400-0xA7FF (1KB color RAM, mirrored at 0xAC00)
    Io = 5,       // 0xC000-0xDFFF (I/O ports, heavily mirrored)
}

/// Sound CPU (Z80) address space regions.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum SoundRegion {
    Rom = 1, // 0x0000-0x1FFF (8KB sound program ROM)
    Ram = 2, // 0x4000-0x47FF (2KB work RAM, mirrored at 0x4800)
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------
// Master clock 48.66 MHz; main Z80 = /16 ≈ 3.041 MHz; pixel clock = /8 ≈ 6.083 MHz.
// HTOTAL 384 px → 192 main-CPU cycles/scanline (pixel clock is 2× the CPU clock).
// VTOTAL 264 lines; visible Y 16..239 (224 lines), VBLANK at line 240.
// Frame: 192 × 264 = 50688 cycles → ≈ 59.99 Hz. ROT90 ⇒ display is 224×256.

pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 48_660_000 / 16, // 3_041_250
    cycles_per_scanline: 192,
    total_scanlines: 264,
    display_width: (NATIVE_HEIGHT - VBLANK_END) as u32, // 224 (rotated)
    display_height: NATIVE_WIDTH as u32,                // 256 (rotated)
};

pub const NATIVE_WIDTH: usize = 256;
pub const NATIVE_HEIGHT: usize = 240;
pub const VBLANK_END: usize = 16; // first visible scanline
pub const VISIBLE_LINES: u64 = 240; // lines rendered (top VBLANK_END clipped on output)

// ---------------------------------------------------------------------------
// ROM definitions ("congo" parent set — 2-board stack, Sega ID 834-5180)
// ---------------------------------------------------------------------------

/// Main Z80 program ROM at 0x0000-0x7FFF (four 8KB chips).
pub static CONGO_MAIN_ROM: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        RomEntry {
            name: "congo_rev_c_rom1.u21",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x09355b5b],
        },
        RomEntry {
            name: "congo_rev_c_rom2a.u22",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x1c5e30ae],
        },
        RomEntry {
            name: "congo_rev_c_rom3.u23",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x5ee1132c],
        },
        RomEntry {
            name: "congo_rev_c_rom4.u24",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0x5332b9bf],
        },
    ],
};

/// Sound Z80 program ROM at 0x0000-0x1FFF.
pub static CONGO_SOUND_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[RomEntry {
        name: "tip_top_rom_17.u19",
        size: 0x2000,
        offset: 0x0000,
        crc32: &[0x5024e673],
    }],
};

/// Foreground/text tile ROM (gfx_tx): one 4KB chip, 8×8 2bpp.
pub static CONGO_GFX_TX_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "tip_top_rom_5.u76",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0x7bf6ba2b],
    }],
};

/// Background tile ROM (gfx_bg): three 8KB chips, 8×8 3bpp.
pub static CONGO_GFX_BG_ROM: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        RomEntry {
            name: "tip_top_rom_8.u93",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xdb99a619],
        },
        RomEntry {
            name: "tip_top_rom_9.u94",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x93e2309e],
        },
        RomEntry {
            name: "tip_top_rom_10.u95",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0xf27a9407],
        },
    ],
};

/// Sprite ROM (gfx_spr): six 8KB chips, 32×32 3bpp.
pub static CONGO_GFX_SPR_ROM: RomRegion = RomRegion {
    size: 0xc000,
    entries: &[
        RomEntry {
            name: "tip_top_rom_12.u78",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x15e3377a],
        },
        RomEntry {
            name: "tip_top_rom_13.u79",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x1d1321c8],
        },
        RomEntry {
            name: "tip_top_rom_11.u77",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0x73e2709f],
        },
        RomEntry {
            name: "tip_top_rom_14.u104",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0xbf9169fe],
        },
        RomEntry {
            name: "tip_top_rom_16.u106",
            size: 0x2000,
            offset: 0x8000,
            crc32: &[0xcb6d5775],
        },
        RomEntry {
            name: "tip_top_rom_15.u105",
            size: 0x2000,
            offset: 0xa000,
            crc32: &[0x7b15a7a4],
        },
    ],
};

/// Background map data (tilemap_dat): two 8KB chips describing the scrolling map.
pub static CONGO_TILEMAP_DAT_ROM: RomRegion = RomRegion {
    size: 0x4000,
    entries: &[
        RomEntry {
            name: "tip_top_rom_6.u57",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xd637f02b],
        },
        RomEntry {
            name: "tip_top_rom_7.u58",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x80927943],
        },
    ],
};

/// Color PROM (`mr019`, TBP28L22): 256 bytes, reloaded into the high half ⇒ 512.
pub static CONGO_PALETTE_PROM: RomRegion = RomRegion {
    size: 0x0200,
    entries: &[
        RomEntry {
            name: "mr019.u87",
            size: 0x100,
            offset: 0x0000,
            crc32: &[0xb788d8ae],
        },
        RomEntry {
            name: "mr019.u87",
            size: 0x100,
            offset: 0x0100,
            crc32: &[0xb788d8ae],
        },
    ],
};

// ---------------------------------------------------------------------------
// CongoBongoBoard
// ---------------------------------------------------------------------------

#[derive(BusDebug, DebugTrace)]
pub struct CongoBongoBoard {
    #[debug_cpu("Z80 Main")]
    pub(crate) cpu: Z80,
    #[debug_cpu("Z80 Sound")]
    pub(crate) sound_cpu: Z80,

    #[debug_map(cpu = 0)]
    pub(crate) main_map: AddressSpace16,
    #[debug_map(cpu = 1)]
    pub(crate) sound_map: AddressSpace16,

    // GFX ROMs (decoded into caches by issue .4).
    pub(crate) tx_rom: [u8; 0x1000],
    pub(crate) bg_rom: [u8; 0x6000],
    pub(crate) spr_rom: [u8; 0xc000],
    pub(crate) tilemap_dat: [u8; 0x4000],

    // Color PROM (decoded into an RGB palette by issue .4).
    pub(crate) palette_prom: [u8; 0x0200],

    // Scanline-rendered framebuffer (256 × 240 × RGB24, pre-rotation).
    pub(crate) scanline_buffer: Vec<u8>,

    // Inputs (active-high here; bus inverts as needed) + DIP banks.
    pub(crate) in0: u8,
    pub(crate) in1: u8,
    pub(crate) in2: u8,
    pub(crate) dsw2: u8,
    pub(crate) dsw3: u8,

    // 74LS259 addressable latches (raw bytes; individual lines decoded by the
    // render/input issues). `int_enabled`/`bg_enabled` are broken out because the
    // run loop needs them now.
    pub(crate) latch1: u8,
    pub(crate) latch2: u8,
    pub(crate) int_enabled: bool,
    pub(crate) bg_enabled: bool,

    // Background scroll position (two raw bytes; decoded by issue .6).
    pub(crate) bg_position: [u8; 2],

    // Custom sprite-DMA registers (src lo/hi, count, trigger; issue .7).
    pub(crate) sprite_dma: [u8; 4],

    // Sound command latch (main CPU → PPI port A; issue .9).
    pub(crate) sound_latch: u8,

    // Timing / interrupts.
    pub(crate) clock: u64,
    pub(crate) vblank_irq_pending: bool,

    #[debug_events]
    pub(crate) debug_trace: DebugTraceBuffer,
}

impl Default for CongoBongoBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl CongoBongoBoard {
    pub fn new() -> Self {
        Self {
            cpu: Z80::new(),
            sound_cpu: Z80::new(),
            main_map: Self::build_main_map(),
            sound_map: Self::build_sound_map(),
            tx_rom: [0; 0x1000],
            bg_rom: [0; 0x6000],
            spr_rom: [0; 0xc000],
            tilemap_dat: [0; 0x4000],
            palette_prom: [0; 0x0200],
            scanline_buffer: vec![0u8; NATIVE_WIDTH * NATIVE_HEIGHT * 3],
            in0: 0x00,
            in1: 0x00,
            in2: 0x00,
            dsw2: 0x00,
            dsw3: 0x00,
            latch1: 0x00,
            latch2: 0x00,
            int_enabled: false,
            bg_enabled: false,
            bg_position: [0; 2],
            sprite_dma: [0; 4],
            sound_latch: 0,
            clock: 0,
            vblank_irq_pending: false,
            debug_trace: DebugTraceBuffer::new(),
        }
    }

    fn build_main_map() -> AddressSpace16 {
        use MainRegion::*;
        let mut map = AddressSpace16::new();
        map.region(Rom, "Program ROM", 0x0000, 0x8000, AccessKind::ReadOnly)
            .region(Ram, "Work RAM", 0x8000, 0x1000, AccessKind::ReadWrite)
            .region(VideoRam, "Video RAM", 0xA000, 0x0400, AccessKind::ReadWrite)
            .region(ColorRam, "Color RAM", 0xA400, 0x0400, AccessKind::ReadWrite)
            .region(Io, "I/O Ports", 0xC000, 0x2000, AccessKind::Io);
        map
    }

    fn build_sound_map() -> AddressSpace16 {
        use SoundRegion::*;
        let mut map = AddressSpace16::new();
        map.region(Rom, "Sound ROM", 0x0000, 0x2000, AccessKind::ReadOnly)
            .region(Ram, "Sound RAM", 0x4000, 0x0800, AccessKind::ReadWrite);
        map
    }

    // -----------------------------------------------------------------------
    // 74LS259 control latches
    // -----------------------------------------------------------------------

    /// Write one bit of main latch 1 (0xC018-0xC01F, LS259 `write_d0`).
    /// Bit 5 = BEN (background enable), bit 7 = INTON (VBlank IRQ enable); the
    /// remaining lines (coin counters / flip screen) are kept in `latch1` for the
    /// render/input issues.
    pub fn write_latch1(&mut self, bit: u8, value: bool) {
        if value {
            self.latch1 |= 1 << bit;
        } else {
            self.latch1 &= !(1 << bit);
        }
        match bit {
            5 => self.bg_enabled = value,
            7 => {
                self.int_enabled = value;
                if !value {
                    self.vblank_irq_pending = false;
                }
            }
            _ => {}
        }
    }

    /// Write one bit of main latch 2 (0xC020-0xC027, LS259 `write_d0`).
    /// Bit 3 = CREF3 (bg color), bit 6 = BS (fg bank), bit 7 = CBS (color bank);
    /// decoded by the render issues from `latch2`.
    pub fn write_latch2(&mut self, bit: u8, value: bool) {
        if value {
            self.latch2 |= 1 << bit;
        } else {
            self.latch2 &= !(1 << bit);
        }
    }

    // -----------------------------------------------------------------------
    // Core tick (main CPU only; sound CPU + audio wired by issue .9)
    // -----------------------------------------------------------------------

    /// Execute one main-CPU clock cycle (≈3.041 MHz).
    pub fn tick(&mut self, bus: &mut dyn Bus<Address = u16, Data = u8>) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();

        // Per-scanline rendering hook (layers added by issues .5/.6/.7).
        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let scanline = (frame_cycle / TIMING.cycles_per_scanline) as u16;
            if scanline < VISIBLE_LINES as u16 {
                self.render_scanline(scanline as usize);
            }
        }

        // VBlank IRQ: asserted at line 240 when INTON is enabled, held until the
        // game clears INTON (see `write_latch1`). Z80 IRQ is level-triggered and
        // IFF1-masked, so the handler's DI/EI sequencing avoids re-entry.
        let vblank_cycle = VISIBLE_LINES * TIMING.cycles_per_scanline;
        if frame_cycle == vblank_cycle && self.int_enabled {
            self.vblank_irq_pending = true;
        }

        if self.main_map.has_any_watchpoints() || self.debug_trace.enabled() {
            let pc = self
                .cpu
                .at_instruction_boundary()
                .then_some(self.cpu.pc as u32);
            self.main_map.latch_access_context(self.clock, pc);
        }

        self.cpu.execute_cycle(bus, BusMaster::Cpu(0));

        self.clock += 1;
    }

    /// Render one screen scanline. The layers (foreground tilemap, scrolling
    /// background, sprites) are filled in by issues .5/.6/.7; for now the frame
    /// stays cleared.
    pub fn render_scanline(&mut self, _abs_y: usize) {}

    // -----------------------------------------------------------------------
    // Frame output (ROT90)
    // -----------------------------------------------------------------------

    /// Rotate the visible raster (rows 16..240, 256×224) 90° CCW into the
    /// portrait 224×256 RGB24 output buffer.
    pub fn render_frame(&self, buffer: &mut [u8]) {
        let start = VBLANK_END * NATIVE_WIDTH * 3;
        let visible =
            &self.scanline_buffer[start..start + (NATIVE_HEIGHT - VBLANK_END) * NATIVE_WIDTH * 3];
        gfx::rotate_90_ccw(visible, buffer, NATIVE_WIDTH, NATIVE_HEIGHT - VBLANK_END);
    }

    // -----------------------------------------------------------------------
    // Reset / interrupts
    // -----------------------------------------------------------------------

    pub fn reset(&mut self) {
        self.int_enabled = false;
        self.bg_enabled = false;
        self.vblank_irq_pending = false;
        self.latch1 = 0;
        self.latch2 = 0;
        self.bg_position = [0; 2];
        self.sprite_dma = [0; 4];
        self.sound_latch = 0;
        self.clock = 0;

        self.main_map.region_data_mut(MainRegion::Ram).fill(0);
        self.main_map.region_data_mut(MainRegion::VideoRam).fill(0);
        self.main_map.region_data_mut(MainRegion::ColorRam).fill(0);
        self.sound_map.region_data_mut(SoundRegion::Ram).fill(0);
        self.scanline_buffer.fill(0);
    }

    pub fn check_interrupts(&self, target: BusMaster) -> InterruptState {
        match target {
            BusMaster::Cpu(0) => InterruptState {
                irq: self.vblank_irq_pending && self.int_enabled,
                ..Default::default()
            },
            // Sound CPU interrupts are wired by issue .9.
            _ => InterruptState::default(),
        }
    }

    pub fn debug_tick_boundaries(&self) -> u32 {
        let mut result = 0;
        if self.cpu.at_instruction_boundary() {
            result |= 1;
        }
        if self.sound_cpu.at_instruction_boundary() {
            result |= 2;
        }
        result
    }
}

impl Saveable for CongoBongoBoard {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        self.sound_cpu.save_state(w);
        w.write_bytes(self.main_map.region_data(MainRegion::Ram));
        w.write_bytes(self.main_map.region_data(MainRegion::VideoRam));
        w.write_bytes(self.main_map.region_data(MainRegion::ColorRam));
        w.write_bytes(self.sound_map.region_data(SoundRegion::Ram));
        w.write_u8(self.in0);
        w.write_u8(self.in1);
        w.write_u8(self.in2);
        w.write_u8(self.dsw2);
        w.write_u8(self.dsw3);
        w.write_u8(self.latch1);
        w.write_u8(self.latch2);
        w.write_bool(self.int_enabled);
        w.write_bool(self.bg_enabled);
        w.write_bytes(&self.bg_position);
        w.write_bytes(&self.sprite_dma);
        w.write_u8(self.sound_latch);
        w.write_u64_le(self.clock);
        w.write_bool(self.vblank_irq_pending);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        self.sound_cpu.load_state(r)?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::Ram))?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::VideoRam))?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::ColorRam))?;
        r.read_bytes_into(self.sound_map.region_data_mut(SoundRegion::Ram))?;
        self.in0 = r.read_u8()?;
        self.in1 = r.read_u8()?;
        self.in2 = r.read_u8()?;
        self.dsw2 = r.read_u8()?;
        self.dsw3 = r.read_u8()?;
        self.latch1 = r.read_u8()?;
        self.latch2 = r.read_u8()?;
        self.int_enabled = r.read_bool()?;
        self.bg_enabled = r.read_bool()?;
        r.read_bytes_into(&mut self.bg_position)?;
        r.read_bytes_into(&mut self.sprite_dma)?;
        self.sound_latch = r.read_u8()?;
        self.clock = r.read_u64_le()?;
        self.vblank_irq_pending = r.read_bool()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// CongoBongoSystem wrapper
// ---------------------------------------------------------------------------

/// Sega Congo Bongo (1983).
#[derive(Saveable)]
pub struct CongoBongoSystem {
    pub board: CongoBongoBoard,
}

impl Default for CongoBongoSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl CongoBongoSystem {
    pub fn new() -> Self {
        Self {
            board: CongoBongoBoard::new(),
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let prog = CONGO_MAIN_ROM.load(rom_set)?;
        self.board
            .main_map
            .load_region_at(MainRegion::Rom, 0, &prog);

        let sound = CONGO_SOUND_ROM.load(rom_set)?;
        self.board
            .sound_map
            .load_region_at(SoundRegion::Rom, 0, &sound);

        self.board
            .tx_rom
            .copy_from_slice(&CONGO_GFX_TX_ROM.load(rom_set)?);
        self.board
            .bg_rom
            .copy_from_slice(&CONGO_GFX_BG_ROM.load(rom_set)?);
        self.board
            .spr_rom
            .copy_from_slice(&CONGO_GFX_SPR_ROM.load(rom_set)?);
        self.board
            .tilemap_dat
            .copy_from_slice(&CONGO_TILEMAP_DAT_ROM.load(rom_set)?);
        self.board
            .palette_prom
            .copy_from_slice(&CONGO_PALETTE_PROM.load(rom_set)?);

        // GFX decode + palette build land in issue .4.
        Ok(())
    }

    /// Canonicalize a 0xA000-0xBFFF access (video/color RAM + their 0x800/0x1000/
    /// 0x1800 mirrors) to the 0xA000-0xA7FF backing window.
    #[inline]
    fn vram_addr(addr: u16) -> u16 {
        0xA000 | (addr & 0x07FF)
    }
}

impl Bus for CongoBongoSystem {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        match master {
            BusMaster::Cpu(0) => {
                let data = match addr {
                    0x0000..=0x8FFF => self.board.main_map.read_backing(addr),
                    0xA000..=0xBFFF => self.board.main_map.read_backing(Self::vram_addr(addr)),
                    0xC000..=0xDFFF => match addr & 0x3F {
                        0x00 => self.board.in0,
                        0x01 => self.board.in1,
                        0x02 => self.board.dsw2,
                        0x03 => self.board.dsw3,
                        0x08 => self.board.in2,
                        _ => 0xFF,
                    },
                    _ => 0xFF,
                };
                self.board.main_map.watch_read(0, master, addr, data);
                data
            }

            // Sound CPU program ROM / work RAM (devices wired by issue .9).
            BusMaster::Cpu(1) => match addr {
                0x0000..=0x1FFF => self.board.sound_map.read_backing(addr),
                0x4000..=0x4FFF => self.board.sound_map.read_backing(0x4000 | (addr & 0x07FF)),
                _ => 0xFF,
            },

            _ => 0xFF,
        }
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        match master {
            BusMaster::Cpu(0) => {
                self.board.main_map.watch_write(0, master, addr, data);
                match addr {
                    0x8000..=0x8FFF => self.board.main_map.write_backing(addr, data),
                    0xA000..=0xBFFF => self
                        .board
                        .main_map
                        .write_backing(Self::vram_addr(addr), data),
                    0xC000..=0xDFFF => match addr & 0x3F {
                        0x18..=0x1F => self.board.write_latch1((addr & 0x07) as u8, data & 1 != 0),
                        0x20..=0x27 => self.board.write_latch2((addr & 0x07) as u8, data & 1 != 0),
                        0x28..=0x29 => self.board.bg_position[(addr & 0x01) as usize] = data,
                        0x30..=0x33 => self.board.sprite_dma[(addr & 0x03) as usize] = data,
                        0x38..=0x3F => self.board.sound_latch = data,
                        _ => {}
                    },
                    _ => {} // ROM / unmapped
                }
            }
            // Sound CPU work RAM; SN76489 / PPI writes are wired by issue .9.
            BusMaster::Cpu(1) if (0x4000..=0x4FFF).contains(&addr) => {
                self.board
                    .sound_map
                    .write_backing(0x4000 | (addr & 0x07FF), data);
            }
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

crate::impl_board_delegation!(
    CongoBongoSystem,
    board,
    crate::congo_bongo::TIMING,
    no_audio
);

impl MachineCore for CongoBongoSystem {
    crate::machine_core_metadata!("congobongo", crate::congo_bongo::TIMING);

    fn run_frame(&mut self) {
        bus_split!(self, bus => {
            for _ in 0..crate::congo_bongo::TIMING.cycles_per_frame() {
                self.board.tick(bus);
            }
        });
    }

    fn reset(&mut self) {
        self.board.reset();
        bus_split!(self, bus => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
            self.board.sound_cpu.reset(bus, BusMaster::Cpu(1));
        });
    }
}

impl SaveState for CongoBongoSystem {
    crate::machine_save_state!();
}

// Input controls and DIP-switch options are fleshed out by issue .8; the skeleton
// exposes the empty/raw shells so the machine satisfies `FrontendMachine`.
impl InputConfigurable for CongoBongoSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        &[]
    }

    fn handle_input(&mut self, _event: InputEvent) {}
}

const CONGO_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSW02",
        options: &[],
    },
    DipSwitchBank {
        name: "DSW03",
        options: &[],
    },
];

impl DipSwitches for CongoBongoSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        CONGO_DIP_BANKS
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        match bank {
            0 => self.board.dsw2,
            1 => self.board.dsw3,
            _ => 0,
        }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        match bank {
            0 => self.board.dsw2 = value,
            1 => self.board.dsw3 = value,
            _ => {}
        }
    }
}

impl Nvram for CongoBongoSystem {}
impl phosphor_core::core::machine::Profilable for CongoBongoSystem {}
crate::impl_board_debug_trace!(CongoBongoSystem, board);

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

fn create_machine(
    rom_set: &RomSet,
) -> Result<Box<dyn phosphor_core::core::machine::FrontendMachine>, RomLoadError> {
    let mut sys = CongoBongoSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
    MachineEntry::new("congobongo", &["congo", "congobongo"], create_machine)
}

inventory::submit! {
    DisasmRegion {
        machine: "congobongo",
        region: "main",
        cpu: DisasmCpu::Z80,
        org: 0x0000,
        size: CONGO_MAIN_ROM.size as u32,
        load: |rs| CONGO_MAIN_ROM.load(rs),
    }
}

inventory::submit! {
    DisasmRegion {
        machine: "congobongo",
        region: "sound",
        cpu: DisasmCpu::Z80,
        org: 0x0000,
        size: CONGO_SOUND_ROM.size as u32,
        load: |rs| CONGO_SOUND_ROM.load(rs),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_in_machine_and_disasm_registries() {
        let entry = crate::registry::find("congobongo").expect("machine registered");
        assert_eq!(entry.rom_names, &["congo", "congobongo"]);

        let regions = crate::disasm_registry::regions_for("congobongo");
        assert_eq!(
            regions.iter().map(|r| r.region).collect::<Vec<_>>(),
            vec!["main", "sound"],
        );
        let main = crate::disasm_registry::find("congobongo", "main").unwrap();
        assert_eq!((main.cpu, main.org, main.size), (DisasmCpu::Z80, 0, 0x8000));
        let sound = crate::disasm_registry::find("congobongo", "sound").unwrap();
        assert_eq!((sound.cpu, sound.org, sound.size), (DisasmCpu::Z80, 0, 0x2000));
    }

    #[test]
    fn boots_and_runs_frames_without_panicking() {
        let mut sys = CongoBongoSystem::new();
        sys.reset();
        for _ in 0..3 {
            sys.run_frame();
        }
        let (w, h) = TIMING.display_size();
        assert_eq!((w, h), (224, 256));
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.board.render_frame(&mut buf);
    }

    #[test]
    fn bus_decodes_ram_video_color() {
        let mut sys = CongoBongoSystem::new();
        for (addr, val) in [(0x8000u16, 0x11u8), (0xA000, 0x22), (0xA400, 0x33)] {
            sys.write(BusMaster::Cpu(0), addr, val);
            assert_eq!(sys.read(BusMaster::Cpu(0), addr), val, "addr {addr:#06x}");
        }
        // Video/color RAM mirrors (+0x800) alias the same backing store.
        sys.write(BusMaster::Cpu(0), 0xA800, 0x44);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xA000), 0x44);

        // Program ROM is read-only: writes are ignored.
        sys.board.main_map.region_data_mut(MainRegion::Rom)[0] = 0xAB;
        sys.write(BusMaster::Cpu(0), 0x0000, 0x00);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0x0000), 0xAB);
    }

    #[test]
    fn bus_decodes_inputs_and_control_latches() {
        let mut sys = CongoBongoSystem::new();
        sys.board.in0 = 0x55;
        sys.board.in1 = 0xAA;
        sys.board.dsw2 = 0x3C;
        sys.board.dsw3 = 0x12;
        sys.board.in2 = 0x77;
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xC000), 0x55);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xC001), 0xAA);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xC002), 0x3C);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xC003), 0x12);
        assert_eq!(sys.read(BusMaster::Cpu(0), 0xC008), 0x77);

        // LS259: INTON (latch1 Q7) gates the VBlank IRQ.
        sys.write(BusMaster::Cpu(0), 0xC01F, 0x01);
        assert!(sys.board.int_enabled);
        sys.write(BusMaster::Cpu(0), 0xC01F, 0x00);
        assert!(!sys.board.int_enabled);

        // BEN (latch1 Q5) toggles the background enable.
        sys.write(BusMaster::Cpu(0), 0xC01D, 0x01);
        assert!(sys.board.bg_enabled);

        // Scroll, sprite-DMA, and sound latches store their bytes.
        sys.write(BusMaster::Cpu(0), 0xC028, 0x84);
        assert_eq!(sys.board.bg_position[0], 0x84);
        sys.write(BusMaster::Cpu(0), 0xC032, 0x09);
        assert_eq!(sys.board.sprite_dma[2], 0x09);
        sys.write(BusMaster::Cpu(0), 0xC038, 0x5A);
        assert_eq!(sys.board.sound_latch, 0x5A);
    }

    #[test]
    fn vblank_irq_respects_int_enable() {
        let mut sys = CongoBongoSystem::new();
        // Disabled: no IRQ even at the VBlank cycle.
        assert!(!sys.check_interrupts(BusMaster::Cpu(0)).irq);

        sys.board.int_enabled = true;
        sys.board.vblank_irq_pending = true;
        assert!(sys.check_interrupts(BusMaster::Cpu(0)).irq);

        // Clearing INTON via the latch drops the pending IRQ.
        sys.write(BusMaster::Cpu(0), 0xC01F, 0x00);
        assert!(!sys.check_interrupts(BusMaster::Cpu(0)).irq);
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = CongoBongoSystem::new();
        sys.write(BusMaster::Cpu(0), 0xA000, 0xC3);
        sys.board.latch2 = 0xC0;
        sys.board.sound_latch = 0x7E;
        sys.board.clock = 12345;

        let data = SaveState::save_state(&sys).expect("save_state should return Some");

        let mut sys2 = CongoBongoSystem::new();
        SaveState::load_state(&mut sys2, &data).unwrap();
        assert_eq!(sys2.read(BusMaster::Cpu(0), 0xA000), 0xC3);
        assert_eq!(sys2.board.latch2, 0xC0);
        assert_eq!(sys2.board.sound_latch, 0x7E);
        assert_eq!(sys2.board.clock, 12345);
    }
}
