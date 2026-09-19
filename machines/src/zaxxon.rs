//! Sega Zaxxon (1982), the board the family is named after.
//!
//! # Schematics
//!
//! Source for all of these: `arcade-museum.com/manuals-videogames/Z/Zaxxon.pdf`.
//! Each sheet is spread across two PDF pages, left half then right half.
//!
//! | Drawing | Sheets | PDF pages | What is on them |
//! |---|---|---|---|
//! | `IC Board A 834-0214 rev A` | 11-12 of 16 | 132-135 | The discrete sound board: the 8255 at U23, an MM5837 noise source, 74123 one-shots, MB4391 VCAs and an LA4460 power amp |
//! | `IC Board A 834-0214 rev A` | 13 of 16 | 136-137 | Controls, the latched coin inputs, and the color PROM and its resistor DAC |
//! | `IC Board A 834-0214 rev A` | 14 of 16 | 138-139 | The Z80 at U24, program ROM at U27/U28, video RAM and its address mux |
//! | `IC Board B 834-0211 rev A` | 6-8 of 9 | 140-145 | Background scroll adders and map ROMs, the sync chain, sprite ROMs and the 93422 line buffers |
//! | `IC Board B 834-0257 rev A` | 6-8 of 9 | 146-151 | The same three sheets of a second board revision |
//!
//! **The set is partial.** IC Board A sheets 1-10 and 15-16, and IC Board B
//! sheets 1-5 and 9, are not in this manual, and the two 74LS259 control latches
//! and the `0xE0F0` write decode are on sheets it does not have. Those parts of
//! this file still rest on the reference driver alone, and say so where they are
//! written.
//!
//! What has been read is transcribed in two files:
//! [`docs/schematics/zaxxon-color-dac.md`](../../docs/schematics/zaxxon-color-dac.md)
//! for the color DAC, because the palette had been wrong and the drawing is what
//! settled it, and
//! [`docs/schematics/zaxxon-discrete-sound.md`](../../docs/schematics/zaxxon-discrete-sound.md)
//! for the whole sound board. Confirmed in passing while reading, and not separately
//! transcribed: the 11-bit background scroll arrives on P2 as POS0-POS10, every
//! GFX and program ROM socket number in the tables below matches the drawing,
//! and each coin input really is a flip-flop cleared by its own enable line.
//!
//! Hardware (per MAME `src/mame/sega/zaxxon.cpp`, the `zaxxon` set):
//! - Main CPU: Z80 @ MASTER_CLOCK/16 = 48.66 MHz / 16 ~= 3.041 MHz. There is no
//!   second CPU: Congo Bongo's sound Z80 arrived a year later.
//! - Video: 256x224 raster, ROT90 (portrait 224x256), VBlank IRQ gated by INTON
//! - Foreground: 32x32 8x8 2bpp tilemap, colored by a second PROM indexed by
//!   screen position rather than by anything the CPU writes
//! - Background: 8x8 3bpp `tilemap_dat` map, 11-bit scroll with an isometric
//!   skew, which is the pseudo-3D view
//! - Sprites: 32x32 3bpp, written straight into a 256-byte sprite RAM at 0xA000
//!   (no DMA engine; that too is a Congo Bongo addition)
//! - Sound: **entirely discrete.** An i8255 PPI's three output ports gate eleven
//!   analog voices on the sound board; see [Sound](#sound).
//!
//! Everything this board shares with Congo Bongo is in [`crate::sega_zaxxon`].
//!
//! # Sound
//!
//! There is no sound chip on this board to emulate. Twelve active-low bits of
//! the PPI gate eleven analog voices (two engine tones, homing missile, base
//! missile, laser, battleship, small and medium explosion, cannon, shot, and the
//! two alarms which share a leg), and two further bits set a level rather than
//! gating anything. All eleven meet at one passive summing node, `SJ`, and leave
//! through an LA4460 in bridge configuration.
//!
//! [`crate::zaxxon_sound`] models it, built on the `DiscreteCircuit` framework
//! from the transcription in
//! [`docs/schematics/zaxxon-discrete-sound.md`](../../docs/schematics/zaxxon-discrete-sound.md).
//! The drawing is the only reference: the reference driver plays recorded WAV
//! samples, which is a recording of somebody's board rather than a model of any
//! board, so there is nothing to compare against and the row in
//! `tools/sound-compare/targets.toml` says `implemented-unvalidated`.
//!
//! The two things worth knowing before touching a constant there: **player ship
//! A and B are a two-bit level with `PA0` as the more significant bit and the
//! level falling as the bits rise**, which is not what a `data & 3` volume fit
//! produces; and the board's entire mix balance is the attenuator ahead of each
//! leg, because all eleven summing resistors are the same 51 kOhm.

use crate::zaxxon_sound::ZaxxonSound;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::DebugTraceBuffer;
use phosphor_core::core::machine::{
    DipApplyTiming, DipChoice, DipOption, DipSwitchBank, InputConfigurable, InputControl,
    InputEvent, MachineCore, Nvram, SaveState,
};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, ClockDomainName as Clk, ClockTree};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::z80::Z80;
use phosphor_core::device::i8255::I8255;
use phosphor_core::gfx::decode::GfxLayout;
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion, Saveable};

use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::gfx_registry::GfxRegion;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::sega_zaxxon::{
    self as family, COIN_A_CHOICES, COIN_B_CHOICES, CoinLatch, INPUT_COIN1, INPUT_COIN2,
    INPUT_P1_BUTTON, INPUT_P1_DOWN, INPUT_P1_LEFT, INPUT_P1_RIGHT, INPUT_P1_START, INPUT_P1_UP,
    INPUT_P2_BUTTON, INPUT_P2_DOWN, INPUT_P2_LEFT, INPUT_P2_RIGHT, INPUT_P2_START, INPUT_P2_UP,
    INPUT_SERVICE, TIMING, VISIBLE_LINES, Variant, ZAXXON_FAMILY_CONTROLS, ZaxxonVideo,
};
use crate::set_bit_active_high;

// ---------------------------------------------------------------------------
// Memory map region IDs
// ---------------------------------------------------------------------------

/// Main CPU (Z80) address space regions.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum MainRegion {
    Rom = 1,       // 0x0000-0x5FFF (24KB program ROM)
    Ram = 2,       // 0x6000-0x6FFF (4KB work RAM)
    VideoRam = 3,  // 0x8000-0x83FF (1KB tilemap RAM, mirrored to 0x9FFF)
    SpriteRam = 4, // 0xA000-0xA0FF (256B sprite RAM, mirrored to 0xBFFF)
    Io = 5,        // 0xC000-0xFFFF (I/O ports and control latches, heavily mirrored)
}

/// The board's one crystal and everything divided out of it.
///
/// Unlike Congo Bongo there is no second root here: no sound crystal, because
/// there is no sound CPU. The two PSG domains that board declares have no
/// counterpart either, since the voices are analog.
pub fn clock_tree() -> ClockTree {
    use phosphor_core::core::RootId;
    let mut t = ClockTree::new(family::MASTER_CLOCK);
    let cpu = t.add_domain(Clk::Cpu, RootId::MAIN, 1, 16); // 3.04125 MHz
    let dot = t.add_domain(Clk::Pixel, RootId::MAIN, 1, 8); // 6.0825 MHz
    t.set_step_domain(cpu);
    // The pixel clock is exactly twice the CPU clock off the same crystal, so
    // 384 dot clocks is exactly 192 CPU cycles.
    t.set_raster(dot, 384, 0);
    t
}

// ---------------------------------------------------------------------------
// ROM definitions ("zaxxon" parent set, rev D)
// ---------------------------------------------------------------------------

/// Main Z80 program ROM at 0x0000-0x4FFF.
///
/// The region is 0x6000 but only 0x5000 of it is populated: two 8KB chips and a
/// 4KB one. 0x5000-0x5FFF has no chip behind it and reads back as zero.
pub static ZAXXON_MAIN_ROM: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        RomEntry {
            name: "zaxxon_rom3d.u27",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x6e2b4a30],
        },
        RomEntry {
            name: "zaxxon_rom2d.u28",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x1c9ea398],
        },
        RomEntry {
            name: "zaxxon_rom1d.u29",
            size: 0x1000,
            offset: 0x4000,
            crc32: &[0x1c123ef9],
        },
    ],
};

/// Foreground/text tile ROM (gfx_tx): two 2KB chips, 8x8 2bpp.
pub static ZAXXON_GFX_TX_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[
        RomEntry {
            name: "zaxxon_rom14.u68",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0x07bf8c52],
        },
        RomEntry {
            name: "zaxxon_rom15.u69",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0xc215edcb],
        },
    ],
};

/// Background tile ROM (gfx_bg): three 8KB chips, 8x8 3bpp.
pub static ZAXXON_GFX_BG_ROM: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        RomEntry {
            name: "zaxxon_rom6.u113",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x6e07bb68],
        },
        RomEntry {
            name: "zaxxon_rom5.u112",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x0a5bce6a],
        },
        RomEntry {
            name: "zaxxon_rom4.u111",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0xa5bf1465],
        },
    ],
};

/// Sprite ROM (gfx_spr): three 8KB chips, 32x32 3bpp, so 64 sprites. Congo
/// Bongo's six chips give it 128.
pub static ZAXXON_GFX_SPR_ROM: RomRegion = RomRegion {
    size: 0x6000,
    entries: &[
        RomEntry {
            name: "zaxxon_rom11.u77",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0xeaf0dd4b],
        },
        RomEntry {
            name: "zaxxon_rom12.u78",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x1c5369c7],
        },
        RomEntry {
            name: "zaxxon_rom13.u79",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0xab4e8a9a],
        },
    ],
};

/// Background map data (tilemap_dat): four 8KB chips. At 0x8000 this is twice
/// Congo Bongo's region, so the 32x512 grid is filled once rather than mirrored.
pub static ZAXXON_TILEMAP_DAT_ROM: RomRegion = RomRegion {
    size: 0x8000,
    entries: &[
        RomEntry {
            name: "zaxxon_rom8.u91",
            size: 0x2000,
            offset: 0x0000,
            crc32: &[0x28d65063],
        },
        RomEntry {
            name: "zaxxon_rom7.u90",
            size: 0x2000,
            offset: 0x2000,
            crc32: &[0x6284c200],
        },
        RomEntry {
            name: "zaxxon_rom10.u93",
            size: 0x2000,
            offset: 0x4000,
            crc32: &[0xa95e61fd],
        },
        RomEntry {
            name: "zaxxon_rom9.u92",
            size: 0x2000,
            offset: 0x6000,
            crc32: &[0x7e42691f],
        },
    ],
};

/// Color PROMs: `mro16` is the 256-entry palette, `zaxxon.u72` the 256
/// foreground color codes. Two different chips, unlike Congo Bongo where one
/// PROM is read twice.
pub static ZAXXON_PALETTE_PROM: RomRegion = RomRegion {
    size: 0x0200,
    entries: &[
        RomEntry {
            name: "mro16.u76",
            size: 0x100,
            offset: 0x0000,
            crc32: &[0x6cc6695b],
        },
        RomEntry {
            name: "zaxxon.u72",
            size: 0x100,
            offset: 0x0100,
            crc32: &[0xdeaa21f7],
        },
    ],
};

/// Sprites: 64 sprites, 32x32 3bpp; planes at thirds of the 0x6000 region.
///
/// The pixel offsets are the family's; only the plane offsets are this board's,
/// because it carries three sprite ROMs where Congo Bongo carries six. The text
/// and background layouts are the family's outright.
pub static ZAXXON_SPR_GFX_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[0, 0x2000 * 8, 2 * 0x2000 * 8],
    x_offsets: &family::SPRITE_X_OFFSETS,
    y_offsets: &family::SPRITE_Y_OFFSETS,
    char_increment: family::CHAR_INCREMENT_SPRITE,
};

inventory::submit! {
    GfxRegion {
        machine: "zaxxon",
        region: "fg",
        count: 256,
        width: 8,
        height: 8,
        layout: &family::TX_GFX_LAYOUT,
        load: |rs| ZAXXON_GFX_TX_ROM.load(rs),
        palette: Some(zaxxon_gfx_palette),
    }
}
inventory::submit! {
    GfxRegion {
        machine: "zaxxon",
        region: "bg",
        count: 1024,
        width: 8,
        height: 8,
        layout: &family::BG_GFX_LAYOUT,
        load: |rs| ZAXXON_GFX_BG_ROM.load(rs),
        palette: Some(zaxxon_gfx_palette),
    }
}
inventory::submit! {
    GfxRegion {
        machine: "zaxxon",
        region: "sprites",
        count: 64,
        width: 32,
        height: 32,
        layout: &ZAXXON_SPR_GFX_LAYOUT,
        load: |rs| ZAXXON_GFX_SPR_ROM.load(rs),
        palette: Some(zaxxon_gfx_palette),
    }
}

/// gfxview palette hook: load the color PROMs and build the RGB palette with
/// the same resistor-DAC math as the runtime path.
fn zaxxon_gfx_palette(rom_set: &RomSet) -> Result<Vec<(u8, u8, u8)>, RomLoadError> {
    let prom = ZAXXON_PALETTE_PROM.load(rom_set)?;
    Ok(family::palette_rgb(&prom).to_vec())
}

// ---------------------------------------------------------------------------
// ZaxxonBoard
// ---------------------------------------------------------------------------

/// One main-CPU cycle: the video and interrupt work for this clock, then the
/// Z80.
///
/// The CPU lives on the machine and the board *is* the bus, so this takes them
/// as separate borrows and dispatches at a concrete type.
#[inline]
pub fn tick(cpu: &mut Z80, board: &mut ZaxxonBoard) {
    board.begin_cycle(cpu);
    cpu.execute_cycle(board, BusMaster::Cpu(0));
    board.clock += 1;
}

/// Run one frame's worth of main-CPU cycles.
pub fn run_frame(cpu: &mut Z80, board: &mut ZaxxonBoard) {
    for _ in 0..TIMING.cycles_per_frame() {
        tick(cpu, board);
    }
}

#[derive(BusDebug, DebugTrace, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct ZaxxonBoard {
    /// The address space persists its own writable regions: work RAM, video RAM
    /// and sprite RAM here. Sprite RAM is the odd one: the CPU writes it through
    /// this map, but the renderer reads the copy in [`Self::video`], so the two
    /// are kept in step on write.
    #[debug_map(cpu = 0)]
    #[save(id = 1)]
    pub(crate) main_map: AddressSpace16,

    /// The raw GFX ROM images. The board loaded them, so the board keeps them;
    /// everything decoded from them lives in `video`.
    #[save_skip]
    pub(crate) tx_rom: [u8; 0x1000],
    #[save_skip]
    pub(crate) bg_rom: [u8; 0x6000],
    #[save_skip]
    pub(crate) spr_rom: [u8; 0x6000],
    #[save_skip]
    pub(crate) tilemap_dat: [u8; 0x8000],
    #[save_skip]
    pub(crate) palette_prom: [u8; 0x0200],

    /// The family video engine.
    #[save(id = 2)]
    pub(crate) video: ZaxxonVideo,

    // Inputs (active-high) + DIP banks. `sw100` holds the start buttons; the
    // coin bits (5/6/7) come from `coins`.
    #[save(id = 3)]
    pub(crate) sw00: u8,
    #[save(id = 4)]
    pub(crate) sw01: u8,
    #[save(id = 5)]
    pub(crate) sw100: u8,
    #[save(id = 6)]
    pub(crate) dsw02: u8,
    #[save(id = 7)]
    pub(crate) dsw03: u8,
    #[save(id = 8)]
    pub(crate) coins: CoinLatch,

    // The two 74LS259 addressable latches, as raw output bytes. U55 is written
    // directly at 0xC000-0xC007; U56 is written through the 0xE0F0 decode.
    #[save(id = 9)]
    pub(crate) latch1: u8,
    #[save(id = 10)]
    pub(crate) latch2: u8,
    #[save(id = 11)]
    pub(crate) int_enabled: bool,

    /// The i8255 PPI whose three output ports gate the discrete sound board.
    #[debug_device("PPI")]
    #[save(id = 12)]
    pub(crate) ppi: I8255,

    /// The sound board the PPI gates: eleven analog voices and a mix bus.
    #[debug_device("Sound")]
    #[save(id = 16)]
    pub(crate) sound: ZaxxonSound,

    /// The board's clock tree, as [`clock_tree`] declares it.
    #[debug_device("Clocks")]
    #[save(id = 13)]
    pub(crate) clocks: ClockTree,

    // Timing / interrupts.
    #[save(id = 14)]
    pub(crate) clock: u64,
    #[save(id = 15)]
    pub(crate) vblank_irq_pending: bool,

    /// The debugger's own ring buffer, which belongs to whoever is debugging
    /// rather than to the machine.
    #[debug_events]
    #[save_skip]
    pub(crate) debug_trace: DebugTraceBuffer,
}

impl Default for ZaxxonBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl ZaxxonBoard {
    pub fn new() -> Self {
        Self {
            main_map: Self::build_main_map(),
            tx_rom: [0; 0x1000],
            bg_rom: [0; 0x6000],
            spr_rom: [0; 0x6000],
            tilemap_dat: [0; 0x8000],
            palette_prom: [0; 0x0200],
            video: ZaxxonVideo::new(Variant::Zaxxon),
            sw00: 0x00,
            sw01: 0x00,
            sw100: 0x00,
            dsw02: DSW02_DEFAULT,
            dsw03: DSW03_DEFAULT,
            coins: CoinLatch::default(),
            latch1: 0x00,
            latch2: 0x00,
            int_enabled: false,
            ppi: I8255::new(),
            sound: ZaxxonSound::new(TIMING.cpu_clock_hz),
            clocks: clock_tree(),
            clock: 0,
            vblank_irq_pending: false,
            debug_trace: DebugTraceBuffer::new(),
        }
    }

    fn build_main_map() -> AddressSpace16 {
        use MainRegion::*;
        let mut map = AddressSpace16::new();
        map.region(Rom, "Program ROM", 0x0000, 0x6000, AccessKind::ReadOnly)
            .region(Ram, "Work RAM", 0x6000, 0x1000, AccessKind::ReadWrite)
            .region(VideoRam, "Video RAM", 0x8000, 0x0400, AccessKind::ReadWrite)
            .region(
                SpriteRam,
                "Sprite RAM",
                0xA000,
                0x0100,
                AccessKind::ReadWrite,
            )
            .region(Io, "I/O Ports", 0xC000, 0x4000, AccessKind::Io);
        map
    }

    /// Rebuild everything the video engine derives from ROM.
    pub fn reload_gfx(&mut self) {
        self.video.load_gfx(
            &self.tx_rom,
            &self.bg_rom,
            &self.spr_rom,
            &ZAXXON_SPR_GFX_LAYOUT,
            &self.tilemap_dat,
            &self.palette_prom,
        );
    }

    // -----------------------------------------------------------------------
    // 74LS259 control latches
    // -----------------------------------------------------------------------

    /// Write one bit of main latch 1 (U55, 0xC000-0xC007, LS259 `write_d0`).
    ///
    /// Bits 0-2 arm coin inputs A, B and service; bits 3-4 are the mechanical
    /// coin counters, which have nothing to emulate; bit 6 is FLIP. Bits 5 and 7
    /// are unused here, which is the visible difference from Congo Bongo, where
    /// they carry BEN and INTON.
    pub fn write_latch1(&mut self, bit: u8, value: bool) {
        if value {
            self.latch1 |= 1 << bit;
        } else {
            self.latch1 &= !(1 << bit);
        }
        self.coins.apply_enables(self.latch1);
        // FLIP is inverted on the way in (`flipscreen_w` stores `!state`), so
        // the cabinet is upright while this line is high. The renderer does not
        // apply flip yet on any board in the family, so the line is recorded
        // and not acted on.
    }

    /// Write one bit of main latch 2 (U56), reached through the 0xE0F0 decode.
    ///
    /// Bit 0 = INTON, bit 1 = CREF1 (fg color), bit 6 = CREF3 (bg color),
    /// bit 7 = BEN (background enable). Congo Bongo moves every one of these.
    pub fn write_latch2(&mut self, bit: u8, value: bool) {
        if value {
            self.latch2 |= 1 << bit;
        } else {
            self.latch2 &= !(1 << bit);
        }
        match bit {
            0 => {
                self.int_enabled = value;
                if !value {
                    self.vblank_irq_pending = false;
                }
            }
            1 => self.video.set_fg_color(value),
            6 => self.video.set_bg_color(value),
            7 => self.video.set_bg_enable(value),
            _ => {}
        }
    }

    /// The 0xE0F0-0xE0FB write decode (`zaxxon_control_w`).
    ///
    /// One 74LS138 at U57 shares its G2B enable with this latch, so a single
    /// write can land in two places. `offset` is the low four address bits:
    /// bit 3 selects the high half of the latch (`bit = (a3 ? 4 : 0) | offset &
    /// 3`), and offsets 8 and 9, which are the two bits of the high half that
    /// nothing else uses, *also* write the two background scroll bytes.
    pub fn write_control(&mut self, offset: usize, data: u8) {
        let a3 = offset & 0x08 != 0;
        let bit = if a3 { 4 } else { 0 } | (offset & 0x03) as u8;
        self.write_latch2(bit, data & 1 != 0);
        if a3 && offset & 0x02 == 0 {
            self.video.write_bg_position(offset & 1, data);
        }
    }

    /// Latch a coin insert: the coin registers only while its arming line
    /// (latch-1 bit `n`) is high.
    pub fn coin_inserted(&mut self, n: usize) {
        self.coins.insert(n, self.latch1);
    }

    /// SW100 (0xC100) input: start buttons plus the three latched coin bits.
    pub fn read_sw100(&self) -> u8 {
        self.sw100 | self.coins.sw100_bits()
    }

    // -----------------------------------------------------------------------
    // Core tick
    // -----------------------------------------------------------------------

    /// Board work that leads a main-CPU cycle: the per-scanline render, the
    /// VBlank IRQ edge, and the debugger's access-attribution latch.
    fn begin_cycle(&mut self, cpu: &Z80) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();

        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let scanline = (frame_cycle / TIMING.cycles_per_scanline) as u16;
            if scanline < VISIBLE_LINES as u16 {
                self.render_scanline(scanline as usize);
            }
        }

        // VBlank IRQ: asserted at line 240 when INTON is enabled, held until the
        // game clears INTON. The Z80 IRQ is level-triggered and IFF1-masked, so
        // the handler's DI/EI sequencing avoids re-entry.
        let vblank_cycle = VISIBLE_LINES * TIMING.cycles_per_scanline;
        if frame_cycle == vblank_cycle && self.int_enabled {
            self.vblank_irq_pending = true;
        }

        if self.main_map.has_any_watchpoints() || self.debug_trace.enabled() {
            let pc = cpu.at_instruction_boundary().then_some(cpu.pc as u32);
            self.main_map.latch_access_context(self.clock, pc);
        }

        // The sound board is analog and free-running: it keeps oscillating
        // whether or not the program writes, so it advances every cycle rather
        // than on a write.
        self.sound.tick(1);
    }

    /// Push the PPI's three output latches to the sound board.
    ///
    /// Called after every PPI write rather than per cycle, because the latches
    /// only change there and the gates are what the writes are for.
    fn sync_sound(&mut self) {
        let a = self.ppi.read_output_a();
        let b = self.ppi.read_output_b();
        let c = self.ppi.read_output_c();
        self.sound.set_ports(a, b, c);
    }

    /// Drain the sound board's output.
    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.sound.fill_audio(buffer)
    }

    /// Render one native screen scanline (`abs_y` = bitmap row 0-239).
    ///
    /// Zaxxon's foreground colors come from a PROM the engine already holds, so
    /// unlike Congo Bongo there is no color RAM to pass in.
    pub fn render_scanline(&mut self, abs_y: usize) {
        let video_ram = self.main_map.region_data(MainRegion::VideoRam);
        self.video.render_scanline(abs_y, video_ram, &[]);
    }

    pub fn render_frame(&self, buffer: &mut [u8]) {
        self.video.render_frame(buffer);
    }

    /// Zaxxon's monitor is mounted rotated 90 degrees, like the rest of the
    /// family. The orientation is declarative: the frontend rotates
    /// `render_frame`'s native output.
    pub fn orientation(&self) -> phosphor_core::core::machine::Orientation {
        self.video.orientation()
    }

    // -----------------------------------------------------------------------
    // Reset / interrupts
    // -----------------------------------------------------------------------

    pub fn reset(&mut self) {
        self.int_enabled = false;
        self.vblank_irq_pending = false;
        self.latch1 = 0;
        self.latch2 = 0;
        self.video.reset();
        self.coins = CoinLatch::default();
        self.clock = 0;

        self.ppi.reset();
        self.sound.reset();
        self.sync_sound();
        self.clocks.reset();

        self.main_map.region_data_mut(MainRegion::Ram).fill(0);
        self.main_map.region_data_mut(MainRegion::VideoRam).fill(0);
        self.main_map.region_data_mut(MainRegion::SpriteRam).fill(0);
    }

    /// The interrupt lines this board drives. Named to avoid shadowing
    /// [`Bus::check_interrupts`], which the board also implements.
    pub fn interrupt_state(&self, target: BusMaster) -> InterruptState {
        match target {
            BusMaster::Cpu(0) => InterruptState {
                irq: self.vblank_irq_pending && self.int_enabled,
                ..Default::default()
            },
            _ => InterruptState::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// ZaxxonSystem wrapper
// ---------------------------------------------------------------------------

/// Sega Zaxxon (1982).
///
/// The Z80 sits beside the board rather than inside it, so each cycle
/// dispatches at the concrete [`ZaxxonBoard`], which *is* the bus.
#[derive(Saveable, BusDebug)]
pub struct ZaxxonSystem {
    #[debug_cpu("Z80")]
    pub(crate) cpu: Z80,
    #[debug_bus]
    pub board: ZaxxonBoard,
}

impl Default for ZaxxonSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl ZaxxonSystem {
    pub fn new() -> Self {
        Self {
            cpu: Z80::new(),
            board: ZaxxonBoard::new(),
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let prog = ZAXXON_MAIN_ROM.load(rom_set)?;
        self.board
            .main_map
            .load_region_at(MainRegion::Rom, 0, &prog);

        self.board
            .tx_rom
            .copy_from_slice(&ZAXXON_GFX_TX_ROM.load(rom_set)?);
        self.board
            .bg_rom
            .copy_from_slice(&ZAXXON_GFX_BG_ROM.load(rom_set)?);
        self.board
            .spr_rom
            .copy_from_slice(&ZAXXON_GFX_SPR_ROM.load(rom_set)?);
        self.board
            .tilemap_dat
            .copy_from_slice(&ZAXXON_TILEMAP_DAT_ROM.load(rom_set)?);
        self.board
            .palette_prom
            .copy_from_slice(&ZAXXON_PALETTE_PROM.load(rom_set)?);

        self.board.reload_gfx();
        Ok(())
    }

    /// Advance one main-CPU cycle, returning the instruction-boundary mask.
    pub fn step_cycle(&mut self) -> u32 {
        tick(&mut self.cpu, &mut self.board);
        u32::from(self.cpu.at_instruction_boundary())
    }

    /// Read the CPU-facing bus, side effects and all. Distinct from the
    /// debugger's `BusDebug::peek`/`poke`, which avoid side effects.
    pub fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        self.board.read(master, addr)
    }

    /// Write the CPU-facing bus, side effects and all. See [`Self::bus_read`].
    pub fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.board.write(master, addr, data);
    }
}

// The board is the bus: Zaxxon is the only machine on it.
impl Bus for ZaxxonBoard {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let data = match addr {
            0x0000..=0x6FFF => self.main_map.read_backing(addr),
            // Video RAM mirrors every 0x400 up to 0x9FFF.
            0x8000..=0x9FFF => self.main_map.read_backing(0x8000 | (addr & 0x03FF)),
            // Sprite RAM mirrors every 0x100 up to 0xBFFF.
            0xA000..=0xBFFF => self.main_map.read_backing(0xA000 | (addr & 0x00FF)),
            // The input ports decode on bits 8-10 (mirror masks 0x18fc and
            // 0x18ff leave those significant), so 0xC1xx is SW100 and 0xC0xx is
            // the four-port block; nothing else in the window is mapped.
            0xC000..=0xDFFF => match addr & 0x0700 {
                0x0000 => match addr & 0x03 {
                    0x00 => self.sw00,
                    0x01 => self.sw01,
                    0x02 => self.dsw02,
                    _ => self.dsw03,
                },
                0x0100 => self.read_sw100(),
                _ => 0xFF,
            },
            // The PPI at 0xE03C-0xE03F, mirrored every 0x100 to 0xFF3C, which is
            // the address the program actually uses.
            0xE000..=0xFFFF if addr & 0x00FC == 0x003C => self.ppi.read(addr & 0x03),
            _ => 0xFF,
        };
        self.main_map.watch_read(0, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.main_map.watch_write(0, master, addr, data);
        match addr {
            0x6000..=0x6FFF => self.main_map.write_backing(addr, data),
            0x8000..=0x9FFF => self.main_map.write_backing(0x8000 | (addr & 0x03FF), data),
            0xA000..=0xBFFF => {
                // Sprite RAM is ordinary CPU-writable RAM on this board, but the
                // renderer reads the engine's copy (which Congo Bongo's DMA
                // engine fills instead), so both are written here.
                let offset = (addr & 0x00FF) as usize;
                self.main_map.write_backing(0xA000 | offset as u16, data);
                self.video.sprite_ram_mut()[offset] = data;
            }
            // Main latch 1 (U55). Same decode as the input ports above.
            0xC000..=0xDFFF => {
                if addr & 0x0700 == 0 {
                    self.write_latch1((addr & 0x07) as u8, data & 1 != 0);
                }
            }
            0xE000..=0xFFFF => match addr & 0x00FF {
                0x3C..=0x3F => {
                    self.ppi.write(addr & 0x03, data);
                    self.sync_sound();
                }
                // Main latch 2 (U56) plus, on two of its offsets, the
                // background scroll bytes.
                0xF0..=0xF3 | 0xF8..=0xFB => self.write_control((addr & 0x0F) as usize, data),
                _ => {}
            },
            _ => {} // ROM / unmapped
        }
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        self.interrupt_state(target)
    }
}

crate::impl_board_delegation!(ZaxxonSystem, board, crate::sega_zaxxon::TIMING, orientation);

impl MachineCore for ZaxxonSystem {
    crate::machine_core_metadata!(
        "zaxxon",
        crate::sega_zaxxon::TIMING,
        crate::zaxxon::clock_tree
    );

    fn gfx_sheets(&self) -> Vec<phosphor_core::core::machine::GfxSheet<'_>> {
        use phosphor_core::core::machine::GfxSheet;
        let video = &self.board.video;
        vec![
            GfxSheet {
                name: "fg",
                cache: video.tx_cache(),
                palette: video.palette(),
            },
            GfxSheet {
                name: "bg",
                cache: video.bg_cache(),
                palette: video.palette(),
            },
            GfxSheet {
                name: "sprites",
                cache: video.sprite_cache(),
                palette: video.palette(),
            },
        ]
    }

    fn run_frame(&mut self) {
        run_frame(&mut self.cpu, &mut self.board);
    }

    fn reset(&mut self) {
        self.board.reset();
        self.cpu.reset(&mut self.board, BusMaster::Cpu(0));
    }
}

impl SaveState for ZaxxonSystem {
    crate::machine_save_state!();
}

// ---------------------------------------------------------------------------
// Input
// ---------------------------------------------------------------------------

impl InputConfigurable for ZaxxonSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        ZAXXON_FAMILY_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let InputEvent::Button { id, pressed } = event else {
            return;
        };
        let b = &mut self.board;
        // Up and down swap between the two ports, and between this board and
        // Congo Bongo: SW00 has down on bit 2, SW01 has up there. That is the
        // harness rather than a typo, and the reference driver's own port
        // definitions have it the same way round.
        match id.0 {
            INPUT_P1_RIGHT => set_bit_active_high(&mut b.sw00, 0, pressed),
            INPUT_P1_LEFT => set_bit_active_high(&mut b.sw00, 1, pressed),
            INPUT_P1_DOWN => set_bit_active_high(&mut b.sw00, 2, pressed),
            INPUT_P1_UP => set_bit_active_high(&mut b.sw00, 3, pressed),
            INPUT_P1_BUTTON => set_bit_active_high(&mut b.sw00, 4, pressed),
            INPUT_P2_RIGHT => set_bit_active_high(&mut b.sw01, 0, pressed),
            INPUT_P2_LEFT => set_bit_active_high(&mut b.sw01, 1, pressed),
            INPUT_P2_UP => set_bit_active_high(&mut b.sw01, 2, pressed),
            INPUT_P2_DOWN => set_bit_active_high(&mut b.sw01, 3, pressed),
            INPUT_P2_BUTTON => set_bit_active_high(&mut b.sw01, 4, pressed),
            INPUT_P1_START => set_bit_active_high(&mut b.sw100, 2, pressed),
            INPUT_P2_START => set_bit_active_high(&mut b.sw100, 3, pressed),
            // Coins latch on the press edge (and only while armed).
            INPUT_COIN1 if pressed => b.coin_inserted(0),
            INPUT_COIN2 if pressed => b.coin_inserted(1),
            INPUT_SERVICE if pressed => b.coin_inserted(2),
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// DIP switches (per INPUT_PORTS(zaxxon) in sega/zaxxon.cpp)
// ---------------------------------------------------------------------------

const DSW02_DEFAULT: u8 = 0x7F; // 10000 bonus, 3 lives, sound on, upright
const DSW03_DEFAULT: u8 = 0x33; // 1C/1C both slots

const ZAXXON_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSW02",
        options: &[
            DipOption {
                name: "Bonus Life",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "10000",
                        value: 0x03,
                    },
                    DipChoice {
                        label: "20000",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "30000",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "40000",
                        value: 0x00,
                    },
                ],
            },
            // SW1:3 and SW1:4 are marked unused on this set. Super Zaxxon
            // reuses SW1:3 as a difficulty switch, which is one of the places
            // the two sets differ in more than ROM contents.
            DipOption {
                name: "Lives",
                mask: 0x30,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "3",
                        value: 0x30,
                    },
                    DipChoice {
                        label: "4",
                        value: 0x10,
                    },
                    DipChoice {
                        label: "5",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "Free Play",
                        value: 0x00,
                    },
                ],
            },
            DipOption {
                name: "Sound",
                mask: 0x40,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Off",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "On",
                        value: 0x40,
                    },
                ],
            },
            DipOption {
                name: "Cabinet",
                mask: 0x80,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Upright",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Cocktail",
                        value: 0x80,
                    },
                ],
            },
        ],
    },
    DipSwitchBank {
        name: "DSW03",
        options: &[
            DipOption {
                name: "Coin B",
                mask: 0x0f,
                apply: DipApplyTiming::Immediate,
                choices: &COIN_B_CHOICES,
            },
            DipOption {
                name: "Coin A",
                mask: 0xf0,
                apply: DipApplyTiming::Immediate,
                choices: &COIN_A_CHOICES,
            },
        ],
    },
];

crate::impl_dip_switches!(ZaxxonSystem, ZAXXON_DIP_BANKS, board.dsw02, board.dsw03);

impl Nvram for ZaxxonSystem {}
impl phosphor_core::core::machine::Profilable for ZaxxonSystem {}
crate::impl_board_debug_trace!(ZaxxonSystem, board);

// ---------------------------------------------------------------------------
// Registry
// ---------------------------------------------------------------------------

crate::register_machine!(ZaxxonSystem, "zaxxon", &["zaxxon"], ZAXXON_FAMILY_CONTROLS);

inventory::submit! {
    DisasmRegion {
        machine: "zaxxon",
        region: "main",
        cpu: DisasmCpu::Z80,
        org: 0x0000,
        size: ZAXXON_MAIN_ROM.size as u32,
        load: |rs| ZAXXON_MAIN_ROM.load(rs),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::InputId;
    // What a PROM byte of 0xFF resolves to on this DAC, which is not white.
    use crate::sega_zaxxon::BRIGHTEST;

    #[test]
    fn registered_in_machine_and_disasm_registries() {
        let entry = crate::registry::find("zaxxon").expect("machine registered");
        assert_eq!(entry.rom_names, &["zaxxon"]);

        let main = crate::disasm_registry::find("zaxxon", "main").unwrap();
        assert_eq!((main.cpu, main.org, main.size), (DisasmCpu::Z80, 0, 0x6000));
    }

    #[test]
    fn gfx_regions_registered_with_expected_geometry() {
        let fg = crate::gfx_registry::find("zaxxon", "fg").unwrap();
        assert_eq!((fg.count, fg.width, fg.height), (256, 8, 8));
        let bg = crate::gfx_registry::find("zaxxon", "bg").unwrap();
        assert_eq!((bg.count, bg.width, bg.height), (1024, 8, 8));
        let spr = crate::gfx_registry::find("zaxxon", "sprites").unwrap();
        assert_eq!(
            (spr.count, spr.width, spr.height),
            (64, 32, 32),
            "three sprite ROMs, half Congo Bongo's six"
        );
        for r in [fg, bg, spr] {
            assert!(r.palette.is_some(), "{} carries the PROM palette", r.region);
        }
    }

    #[test]
    fn bus_decodes_ram_video_and_sprite_ram() {
        let mut sys = ZaxxonSystem::new();
        for (addr, val) in [(0x6000u16, 0x11u8), (0x8000, 0x22), (0xA000, 0x33)] {
            sys.bus_write(BusMaster::Cpu(0), addr, val);
            assert_eq!(
                sys.bus_read(BusMaster::Cpu(0), addr),
                val,
                "addr {addr:#06x}"
            );
        }
        // Video RAM mirrors every 0x400 and sprite RAM every 0x100.
        sys.bus_write(BusMaster::Cpu(0), 0x8400, 0x44);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x8000), 0x44);
        sys.bus_write(BusMaster::Cpu(0), 0xA100, 0x55);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0xA000), 0x55);

        // Program ROM is read-only: writes are ignored.
        sys.board.main_map.region_data_mut(MainRegion::Rom)[0] = 0xAB;
        sys.bus_write(BusMaster::Cpu(0), 0x0000, 0x00);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x0000), 0xAB);
    }

    /// A sprite RAM write has to reach the renderer's copy as well as the map,
    /// because Congo Bongo fills that copy by DMA and this board does not.
    #[test]
    fn a_sprite_ram_write_reaches_the_video_engine() {
        let mut sys = ZaxxonSystem::new();
        sys.bus_write(BusMaster::Cpu(0), 0xA07F, 0x5A);
        assert_eq!(sys.board.video.sprite_ram()[0x7F], 0x5A);
        // Through a mirror, too.
        sys.bus_write(BusMaster::Cpu(0), 0xBF01, 0xA5);
        assert_eq!(sys.board.video.sprite_ram()[0x01], 0xA5);
    }

    #[test]
    fn bus_decodes_inputs_and_dips() {
        let mut sys = ZaxxonSystem::new();
        sys.board.sw00 = 0x55;
        sys.board.sw01 = 0xAA;
        sys.board.dsw02 = 0x3C;
        sys.board.dsw03 = 0x12;
        sys.board.sw100 = 0x0C;
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0xC000), 0x55);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0xC001), 0xAA);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0xC002), 0x3C);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0xC003), 0x12);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0xC100), 0x0C);
        // The ports are mirrored across bits 2-7 and 11-12 of the address.
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0xD8FC), 0x55);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0xD9FF), 0x0C);
    }

    /// The 0xE0F0 decode is the one genuinely odd thing on this board: a single
    /// write can set a latch bit *and* a scroll byte, because a 74LS138 shares
    /// its enable with the latch.
    #[test]
    fn the_control_write_drives_the_latch_and_the_scroll_together() {
        let mut sys = ZaxxonSystem::new();

        // 0xE0F0 is latch bit 0, which is INTON.
        sys.bus_write(BusMaster::Cpu(0), 0xE0F0, 0x01);
        assert!(sys.board.int_enabled);
        sys.bus_write(BusMaster::Cpu(0), 0xE0F0, 0x00);
        assert!(!sys.board.int_enabled);

        // 0xE0FB is latch bit 7, BEN.
        sys.bus_write(BusMaster::Cpu(0), 0xE0FB, 0x01);
        assert!(sys.board.video.bg_enable());

        // 0xE0F8 and 0xE0F9 write the two scroll bytes, and set latch bits 4 and
        // 5 from those same bytes' bit 0 on the way past. Nothing is connected
        // to bits 4 and 5, which is why the board can get away with it, but the
        // side effect is real and the scroll byte is what decides it: 0x84 has
        // bit 0 clear and 0x03 has it set.
        sys.bus_write(BusMaster::Cpu(0), 0xE0F8, 0x84);
        sys.bus_write(BusMaster::Cpu(0), 0xE0F9, 0x03);
        assert_eq!(sys.board.video.bg_position(), [0x84, 0x03]);
        assert_eq!(sys.board.latch2 & 0x30, 0x20);

        // 0xE0FA is latch bit 6 (CREF3) and must not touch the scroll.
        sys.bus_write(BusMaster::Cpu(0), 0xE0FA, 0x01);
        assert_eq!(sys.board.video.bg_position(), [0x84, 0x03]);
        assert_eq!(sys.board.latch2 & 0x40, 0x40);

        // And the whole block mirrors every 0x100 up to 0xFFFx.
        sys.bus_write(BusMaster::Cpu(0), 0xFFF8, 0x11);
        assert_eq!(sys.board.video.bg_position()[0], 0x11);
    }

    #[test]
    fn the_ppi_is_reachable_at_the_address_the_program_uses() {
        let mut sys = ZaxxonSystem::new();
        // The program writes the PPI at 0xFF3C-0xFF3F, a mirror of 0xE03C.
        sys.bus_write(BusMaster::Cpu(0), 0xFF3F, 0x80); // mode 0, all ports out
        sys.bus_write(BusMaster::Cpu(0), 0xFF3C, 0x5A);
        sys.bus_write(BusMaster::Cpu(0), 0xFF3D, 0x3C);
        sys.bus_write(BusMaster::Cpu(0), 0xFF3E, 0x0F);
        assert_eq!(sys.board.ppi.read_output_a(), 0x5A);
        assert_eq!(sys.board.ppi.read_output_b(), 0x3C);
        assert_eq!(sys.board.ppi.read_output_c(), 0x0F);
    }

    #[test]
    fn vblank_irq_respects_int_enable() {
        let mut sys = ZaxxonSystem::new();
        assert!(!sys.board.check_interrupts(BusMaster::Cpu(0)).irq);

        sys.board.int_enabled = true;
        sys.board.vblank_irq_pending = true;
        assert!(sys.board.check_interrupts(BusMaster::Cpu(0)).irq);

        // Clearing INTON through the latch drops the pending IRQ.
        sys.bus_write(BusMaster::Cpu(0), 0xE0F0, 0x00);
        assert!(!sys.board.check_interrupts(BusMaster::Cpu(0)).irq);
    }

    /// The frame loop must actually reach the scanline hook, or every
    /// per-scanline claim below is vacuous.
    #[test]
    fn the_frame_loop_reaches_the_scanline_hook() {
        let mut sys = ZaxxonSystem::new();
        sys.board.tx_rom[8] = 0b1000_0000; // tile 1, pen 1 at (0,0)
        sys.board.palette_prom[1] = 0xFF; // color 0 pen 1 -> white
        sys.board.reload_gfx();
        sys.board.main_map.region_data_mut(MainRegion::VideoRam)[2 * 32] = 1;

        sys.run_frame();
        assert_eq!(
            sys.board.video.scanline_pixel(0, 16),
            BRIGHTEST,
            "row 16 was composited during the frame, not at its end"
        );
    }

    /// A mid-frame scroll write must split the picture at the row it happened
    /// on, which is the whole point of rendering per scanline.
    #[test]
    fn a_mid_frame_scroll_write_splits_the_picture() {
        let mut sys = ZaxxonSystem::new();
        // A background whose every cell is color 1, pen 0, so the pixmap value
        // is 8 everywhere and only the palette lookup varies.
        for b in sys.board.tilemap_dat[0x4000..0x8000].iter_mut() {
            *b = 0x10;
        }
        sys.board.palette_prom[8] = 0xFF; // white through the low color base
        sys.board.palette_prom[0x88] = 0x07; // red once CREF3 adds 0x80
        sys.board.reload_gfx();
        sys.board.write_latch2(7, true); // BEN

        // Halfway down the frame, flip CREF3.
        let split = 120u64;
        let cycles_before = split * TIMING.cycles_per_scanline;
        for _ in 0..cycles_before {
            sys.step_cycle();
        }
        sys.board.write_latch2(6, true); // CREF3
        for _ in cycles_before..TIMING.cycles_per_frame() {
            sys.step_cycle();
        }

        assert_eq!(
            sys.board.video.scanline_pixel(0, 100),
            BRIGHTEST,
            "rows above the write keep the low color base"
        );
        assert_eq!(
            sys.board.video.scanline_pixel(0, 200),
            (255, 0, 0),
            "rows below it take the high one"
        );
    }

    fn press(sys: &mut ZaxxonSystem, id: u16, pressed: bool) {
        sys.handle_input(InputEvent::Button {
            id: InputId(id),
            pressed,
        });
    }

    /// Up and down sit on different bits for the two players, and P1's are the
    /// other way round from Congo Bongo's. Easy to "fix" into a bug.
    #[test]
    fn input_maps_to_port_bits_with_up_and_down_swapped_per_player() {
        let mut sys = ZaxxonSystem::new();
        press(&mut sys, INPUT_P1_RIGHT, true);
        press(&mut sys, INPUT_P1_DOWN, true);
        press(&mut sys, INPUT_P1_BUTTON, true);
        assert_eq!(
            sys.bus_read(BusMaster::Cpu(0), 0xC000),
            0b0001_0101,
            "P1 down is bit 2"
        );

        press(&mut sys, INPUT_P2_UP, true);
        assert_eq!(
            sys.bus_read(BusMaster::Cpu(0), 0xC001),
            0b0000_0100,
            "P2 up is bit 2"
        );

        press(&mut sys, INPUT_P1_START, true);
        press(&mut sys, INPUT_P2_START, true);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0xC100) & 0x0C, 0x0C);

        press(&mut sys, INPUT_P1_RIGHT, false);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0xC000) & 0x01, 0);
    }

    #[test]
    fn coin_latches_only_when_armed_and_clears_on_ack() {
        let mut sys = ZaxxonSystem::new();
        // Not armed: the coin is ignored.
        press(&mut sys, INPUT_COIN1, true);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0xC100) & 0x20, 0);

        // Arm coin A (latch1 bit 0 high), then insert.
        sys.bus_write(BusMaster::Cpu(0), 0xC000, 0x01);
        press(&mut sys, INPUT_COIN1, true);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0xC100) & 0x20, 0x20);

        // The game acknowledges by pulsing that enable line low.
        sys.bus_write(BusMaster::Cpu(0), 0xC000, 0x00);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0xC100) & 0x20, 0);
    }

    crate::dip_test_suite!(ZaxxonSystem, &[DSW02_DEFAULT, DSW03_DEFAULT]);

    #[test]
    fn boots_and_runs_frames_without_panicking() {
        let mut sys = ZaxxonSystem::new();
        sys.reset();
        for _ in 0..3 {
            sys.run_frame();
        }
        // Native (unrotated) framebuffer; the frontend applies ROT90 to present
        // the portrait 224x256 image.
        let (w, h) = TIMING.display_size();
        assert_eq!((w, h), (256, 224));
        assert_eq!(
            sys.board.orientation(),
            phosphor_core::core::machine::Orientation::ROT90
        );
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.board.render_frame(&mut buf);
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = ZaxxonSystem::new();
        sys.bus_write(BusMaster::Cpu(0), 0x8000, 0xC3);
        sys.bus_write(BusMaster::Cpu(0), 0xA010, 0x77);
        sys.bus_write(BusMaster::Cpu(0), 0xE0F8, 0x5C);
        sys.board.clock = 12345;

        let data = SaveState::save_state(&sys).expect("save_state should return Some");

        let mut sys2 = ZaxxonSystem::new();
        SaveState::load_state(&mut sys2, &data).unwrap();
        assert_eq!(sys2.bus_read(BusMaster::Cpu(0), 0x8000), 0xC3);
        assert_eq!(sys2.bus_read(BusMaster::Cpu(0), 0xA010), 0x77);
        assert_eq!(sys2.board.video.sprite_ram()[0x10], 0x77);
        assert_eq!(sys2.board.video.bg_position()[0], 0x5C);
        assert_eq!(sys2.board.clock, 12345);
    }
}
