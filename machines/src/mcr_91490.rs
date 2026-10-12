//! Bally Midway 91490 "5 MHz CPU" board (1983-84), with the 91464 Super Video
//! Gen and the Super Sound I/O. Tapper is the set in the catalog.
//!
//! The stack is the MCR II one with the CPU board and the video board swapped
//! out: the Super Sound I/O (`A084-90913`) is the same board Satan's Hollow
//! carries, and `mcr2.rs` documents it. What changes is everything the main Z80
//! sees.
//!
//! # Schematics
//!
//! | Drawing | Source | Pages |
//! |---|---|---|
//! | 5 MHz CPU `A080-91490-H000`, dated 12/16/83 | `arcarc.xmission.com/PDF_Arcade_Bally_Midway/Tapper_Parts_and_Operating_Manual_(Dec_1983).pdf` | PDF p25, read |
//! | Option switch card `M051-00A11-A007` | same | PDF p5, read |
//!
//! Read 2026-10-11, transcribed at
//! [`docs/schematics/mcr-91490-cpu.md`](../../docs/schematics/mcr-91490-cpu.md).
//! The clock, the CTC wiring and the sync decode below are from the CPU sheet. The memory map, the tile and object formats and the palette
//! word are not transcribed from a drawing here; they follow the established
//! reading of this board, and the parts of it that are shared with MCR II follow
//! `mcr2.rs`.
//!
//! ## Clocks
//!
//! OSC1 is a 20 MHz oscillator. It clocks an S161 counter whose `QA` output is
//! `HCLK` at 10 MHz and whose `QB` output is `CK` at 5 MHz. `CK` drives both the
//! Z80B and the Z80B CTC. So the dot clock is exactly twice the CPU clock, where
//! MCR II's is four times it.
//!
//! ## The CTC
//!
//! Unlike the MCR II scan, this sheet's CTC pin numbers match the Z80 CTC
//! pinout, so the triggers read directly: `493` on pin 20 (CLK/TRG3), `VBLNK` on
//! pin 21 (CLK/TRG2), `INT3` from the connector on pin 23 (CLK/TRG0), and a wire
//! from pin 7 (ZC/TO0) to pin 22 (CLK/TRG1). That last one is the same
//! zero-count cascade `mcr2.rs` had to take on trust, and here it is legible.
//!
//! ## Horizontal and vertical timing
//!
//! The sync logic on the CPU sheet decodes nets named `634` and `317` from the
//! horizontal counter, the same `634` that sets MCR II's line length. At 10 MHz
//! that is a 15,773 Hz line, and at 5 MHz CPU a line is exactly 317 cycles, so
//! this board steps one scanline at a time and needs none of MCR II's line-pair
//! arithmetic. The `317` decode feeds the counter's load (`PLC`) through a
//! flip-flop clocked off `H6` and `VOO`, which looks like a half-line for the
//! interlace; how often it shortens a line is not traced here, and the frame is
//! taken as 512 full lines.
//!
//! The upper vertical count and the `493` decode are again inside a Midway
//! custom (12F, `MMC06`, which takes `DV0`..`DV6` and emits `DV7`, `DV8` and
//! `493`), and where `VBLNK` is generated was not found on the sheet, so the
//! phase of the 480 visible lines inside the 512 is as unreadable here as on
//! MCR II. See `docs/schematics/mcr-video-timing.md`.
//!
//! ## Not modeled
//!
//! - The `CPU WAIT` logic around 5C, 6C and 6D, which stretches CPU cycles
//!   against the horizontal counter. Its condition has not been transcribed, so
//!   every cycle runs at full speed.
//! - Cocktail flip (bit 6 of SSIO output port 0). The Cabinet switch is exposed,
//!   but a cocktail setting does not turn the picture.

use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::{AccessKind, AddressSpace16, Bus, BusMaster};
use phosphor_core::core::{ClockDomainName as Clk, ClockTree, DomainId, TimingConfig};
use phosphor_core::cpu::z80::Z80;
use phosphor_core::device::{SsioBoard, Z80Ctc};
use phosphor_core::gfx;
use phosphor_macros::{BusDebug, MemoryRegion, Saveable};

use crate::mcr2;
use crate::scanline::ScanlineDriven;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum Region {
    Rom = 1,
    Nvram = 2,
    SpriteRam = 3,
    VideoRam = 4,
    Palette = 5,
}

// ---------------------------------------------------------------------------
// Hardware constants
// ---------------------------------------------------------------------------

pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 5_000_000,  // 20 MHz / 4, the S161's QB
    cycles_per_scanline: 317, // 634 dot clocks at twice the CPU clock
    total_scanlines: 512,     // interlaced frame, two 256-line fields
    display_width: NATIVE_WIDTH as u32,
    display_height: NATIVE_HEIGHT as u32,
    display_aspect: Some((4, 3)),
};

/// The board's oscillator and everything divided out of it, plus the sound
/// board's own 2 MHz clock as a second source.
pub fn clock_tree() -> ClockTree {
    use phosphor_core::core::RootId;
    let mut t = ClockTree::new(20_000_000);
    let ssio = t.add_root(2_000_000);
    let cpu = t.add_domain(Clk::Cpu, RootId::MAIN, 1, 4); // 5 MHz (CK)
    let dot = t.add_domain(Clk::Pixel, RootId::MAIN, 1, 2); // 10 MHz (HCLK)
    t.add_domain(Clk::SoundCpu, ssio, 1, 1); // SSIO Z80 at 2 MHz
    t.set_step_domain(cpu);
    t.set_raster(dot, mcr2::HTOTAL_DOTS, 0);
    t
}

/// Scanlines from the start of one field to the start of the next. `VBLNK`
/// reaches the CTC once per field.
const LINES_PER_FIELD: u64 = 256;

/// Native framebuffer: 32×30 tiles of 16×16 pixels, each an 8×8 ROM tile
/// doubled in both directions.
pub const NATIVE_WIDTH: usize = 512;
pub const NATIVE_HEIGHT: usize = 480;

pub(crate) const TILE_COLS: usize = 32;
pub(crate) const TILE_ROWS: usize = 30;

/// Bytes of palette RAM: 64 entries of two bytes, mirrored through 0xF800-0xFFFF.
const PALETTE_BYTES: usize = 0x80;

// ---------------------------------------------------------------------------
// Frame drive
// ---------------------------------------------------------------------------

/// One CPU cycle, testing the frame position first: the debugger's path.
#[inline]
pub fn tick(cpu: &mut Z80, board: &mut Mcr91490Board) {
    Drive { cpu, board }.tick();
}

/// Run one frame's worth of cycles.
pub fn run_frame(cpu: &mut Z80, board: &mut Mcr91490Board) {
    Drive { cpu, board }.run_frame();
}

/// The CPU and the board as two disjoint borrows.
struct Drive<'a> {
    cpu: &'a mut Z80,
    board: &'a mut Mcr91490Board,
}

impl ScanlineDriven for Drive<'_> {
    const TIMING: TimingConfig = TIMING;

    fn clock(&mut self) -> u64 {
        self.board.clock
    }

    fn begin_scanline(&mut self, scanline: u64) {
        self.board.begin_scanline(scanline);
    }

    #[inline]
    fn step_cycle(&mut self) {
        self.board.begin_cycle_inner(self.cpu);
        self.cpu
            .execute_cycle(self.board, phosphor_core::core::BusMaster::Cpu(0));
        self.board.end_cycle();
    }
}

// ---------------------------------------------------------------------------
// Mcr91490Board
// ---------------------------------------------------------------------------

/// Everything the main Z80 talks to on the 91490 stack: memory, the CTC, the
/// Super Sound I/O, and the 91464's object path.
#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct Mcr91490Board {
    #[debug_device("SSIO")]
    #[save(id = 1)]
    pub(crate) ssio: SsioBoard,
    #[debug_device("CTC")]
    #[save(id = 2)]
    pub(crate) ctc: Z80Ctc,

    /// ROM, NVRAM, sprite RAM and video RAM. The palette window is mapped for
    /// the debugger's benefit and has no backing: see `palette_ram`.
    #[debug_map(cpu = 0)]
    #[save(id = 3)]
    pub(crate) map: AddressSpace16,

    #[save_skip]
    pub(crate) tile_cache: gfx::GfxCache,
    #[save_skip]
    pub(crate) sprite_cache: gfx::GfxCache,

    /// The 64 nine-bit palette words, written through 0xF800-0xF87F and its
    /// mirrors.
    ///
    /// Held here rather than in the map because the window repeats every 0x80
    /// bytes, finer than the map's 256-byte pages. Each entry is one 9-bit
    /// word, and its ninth bit is the address's low bit rather than a data
    /// line: a write to the odd address of a pair stores the byte with bit 8
    /// set, a write to the even one stores it with bit 8 clear.
    #[save(id = 4)]
    pub(crate) palette_words: [u16; 64],
    /// The 64 entries as RGB, derived from `palette_words` on every write.
    #[save(id = 5)]
    pub(crate) palette_rgb: [(u8, u8, u8); 64],

    /// 512×480 palette indices, composited at the frame boundary.
    #[save_skip]
    pub(crate) pixel_buffer: Vec<u8>,
    /// Which pixels an object has already claimed this frame.
    #[save_skip]
    object_claimed: Vec<bool>,

    // CTC interrupt handling
    #[save_skip(default)]
    pub(crate) ctc_ack_needed: bool,
    #[save_skip(default)]
    pub(crate) ctc_vector_latch: u8,

    #[save(id = 6)]
    pub(crate) clock: u64,
    #[debug_device("Clocks")]
    #[save(id = 7)]
    pub(crate) clocks: ClockTree,
    #[save_skip]
    pub(crate) ssio_dom: DomainId,
    #[save(id = 8)]
    pub(crate) watchdog_counter: u16,
}

impl Mcr91490Board {
    pub fn new() -> Self {
        let clocks = clock_tree();
        let ssio_dom = clocks.find(Clk::SoundCpu).expect("declared SSIO domain");
        Self {
            ssio: SsioBoard::new(),
            ctc: Z80Ctc::new(),
            map: Self::build_map(),
            tile_cache: gfx::GfxCache::new(0, 8, 8),
            sprite_cache: gfx::GfxCache::new(0, 32, 32),
            palette_words: [0; 64],
            palette_rgb: [(0, 0, 0); 64],
            pixel_buffer: vec![0u8; NATIVE_WIDTH * NATIVE_HEIGHT],
            object_claimed: vec![false; NATIVE_WIDTH * NATIVE_HEIGHT],
            ctc_ack_needed: false,
            ctc_vector_latch: 0,
            clock: 0,
            clocks,
            ssio_dom,
            watchdog_counter: 0,
        }
    }

    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(
            Region::Rom,
            "Program ROM",
            0x0000,
            0xE000,
            AccessKind::ReadOnly,
        )
        .region(
            Region::Nvram,
            "NVRAM",
            0xE000,
            0x0800,
            AccessKind::ReadWrite,
        )
        .region(
            Region::SpriteRam,
            "Sprite RAM",
            0xE800,
            0x0200,
            AccessKind::ReadWrite,
        )
        .region(
            Region::VideoRam,
            "Video RAM",
            0xF000,
            0x0800,
            AccessKind::ReadWrite,
        )
        .region(
            Region::Palette,
            "Palette RAM (write-only)",
            0xF800,
            0x0800,
            AccessKind::Io,
        );
        // Sprite RAM repeats once, at 0xEA00. 0xEC00-0xEFFF is unmapped.
        map.mirror(0xEA00, 0xE800, 0x0200);
        map
    }

    /// Pre-decode the background and object ROMs.
    pub fn decode_gfx(&mut self, bg_rom: &[u8], fg_rom: &[u8]) {
        self.tile_cache = mcr2::decode_tile_gfx(bg_rom);
        self.sprite_cache = mcr2::decode_sprite_gfx(fg_rom);
    }

    // -----------------------------------------------------------------------
    // Palette
    // -----------------------------------------------------------------------

    /// A palette write at any address in the window; only the low seven bits
    /// of the address are decoded.
    pub fn write_palette(&mut self, addr: usize, data: u8) {
        let offset = addr & (PALETTE_BYTES - 1);
        let word = u16::from(data) | (((offset & 1) as u16) << 8);
        self.palette_words[offset / 2] = word;
        self.palette_rgb[offset / 2] = mcr2::rgb_from_9bit(word);
    }

    /// Rebuild every RGB entry from `palette_words`.
    pub fn rebuild_palette(&mut self) {
        for (rgb, &word) in self.palette_rgb.iter_mut().zip(&self.palette_words) {
            *rgb = mcr2::rgb_from_9bit(word);
        }
    }

    // -----------------------------------------------------------------------
    // Per-cycle work
    // -----------------------------------------------------------------------

    /// The CTC's externally triggered channels, on the first cycle of a line.
    ///
    /// Channel 2 is `VBLNK`, once per 256-line field. Channel 3 is `493`, once
    /// per frame.
    fn begin_scanline(&mut self, scanline: u64) {
        if scanline.is_multiple_of(LINES_PER_FIELD) {
            self.ctc.trigger(2, true);
            self.ctc.trigger(2, false);
        }
        if scanline == 0 {
            self.ctc.trigger(3, true);
            self.ctc.trigger(3, false);
        }
    }

    /// Board work before the CPU's cycle.
    fn begin_cycle_inner(&mut self, cpu: &Z80) {
        self.ctc.tick();

        // ZC/TO0 is wired to CLK/TRG1, so channels 0 and 1 cascade. One pulse
        // per zero count gives channel 1 one edge whichever edge it selects.
        if self.ctc.zc_output(0) {
            self.ctc.trigger(1, true);
            self.ctc.trigger(1, false);
        }

        if self.map.debug_active() {
            let pc = cpu.at_instruction_boundary().then_some(cpu.pc as u32);
            self.map.latch_access_context(self.clock, pc);
        }
    }

    /// Board work after the CPU's cycle: the deferred CTC acknowledge, the
    /// sound board, the clock, and the render on the frame's last cycle.
    fn end_cycle(&mut self) {
        if self.ctc_ack_needed {
            self.ctc.acknowledge_interrupt();
            self.ctc_ack_needed = false;
        }

        if self.clocks.tick(self.ssio_dom) {
            self.ssio.tick();
        }

        self.clock += 1;
        self.watchdog_counter = self.watchdog_counter.wrapping_add(1);

        if self.clock.is_multiple_of(TIMING.cycles_per_frame()) {
            self.render_frame_internal();
        }
    }

    // -----------------------------------------------------------------------
    // Rendering
    // -----------------------------------------------------------------------

    /// Composite the whole frame from the video state at the frame boundary.
    ///
    /// This is a whole-frame render for the reason `mcr2.rs` gives at its
    /// `render_frame`: the frame is interlaced and the vertical decode is
    /// inside a custom (`MMC06` here), so neither where the 480 visible lines
    /// sit in the 512 nor which field paints a given row can be read from a
    /// drawing. A per-row render would have to guess both, and a wrong guess
    /// combs the picture rather than blurring it.
    pub fn render_frame_internal(&mut self) {
        self.render_tiles();
        self.render_objects();
    }

    /// Draw every background tile.
    ///
    /// Each tile is two bytes of video RAM:
    ///
    /// ```text
    /// byte 0   pppppppp  picture, low 8 bits
    /// byte 1   ss------  object palette bank (unused by this board's jumpering)
    ///          --cc----  tile palette bank
    ///          ----y---  Y flip
    ///          -----x--  X flip
    ///          ------pp  picture, high 2 bits
    /// ```
    ///
    /// Every pen is opaque, pen 0 included: it selects entry `bank * 16`.
    fn render_tiles(&mut self) {
        let tile_count = self.tile_cache.count();
        if tile_count == 0 {
            return;
        }
        let video_ram = self.map.region_data(Region::VideoRam);

        for tile_row in 0..TILE_ROWS {
            for tile_col in 0..TILE_COLS {
                let offs = (tile_row * TILE_COLS + tile_col) * 2;
                let data = u16::from(video_ram[offs]) | (u16::from(video_ram[offs + 1]) << 8);
                let code = (data & 0x3FF) as usize % tile_count;
                let hflip = data & 0x0400 != 0;
                let vflip = data & 0x0800 != 0;
                let bank = (((data >> 12) & 3) as u8) << 4;

                for src_y in 0..8usize {
                    let row = self
                        .tile_cache
                        .row_slice(code, if vflip { 7 - src_y } else { src_y });
                    let base = (tile_row * 16 + src_y * 2) * NATIVE_WIDTH + tile_col * 16;
                    for src_x in 0..8usize {
                        let pen = bank | row[if hflip { 7 - src_x } else { src_x }];
                        let at = base + src_x * 2;
                        self.pixel_buffer[at] = pen;
                        self.pixel_buffer[at + 1] = pen;
                        self.pixel_buffer[at + NATIVE_WIDTH] = pen;
                        self.pixel_buffer[at + NATIVE_WIDTH + 1] = pen;
                    }
                }
            }
        }
    }

    /// Draw the object list, topmost first.
    ///
    /// Each object is four bytes of sprite RAM:
    ///
    /// ```text
    /// byte 0   Y, counted up from the bottom in 2-pixel units
    /// byte 1   --yx----  Y flip, X flip
    ///          ----p---  picture, bit 8
    ///          ------cc  palette bank, inverted
    /// byte 2   picture, low 8 bits
    /// byte 3   X in 2-pixel units
    /// ```
    ///
    /// The 91464 writes its line buffer only where it has not already written,
    /// so the walk runs from the end of the list to the start and the first
    /// object to reach a pixel keeps it. Any non-zero pen claims the pixel;
    /// only a pen with one of its low three bits set colors it. Pen 8 is
    /// therefore a hole: it shows the background and still hides every object
    /// below it.
    ///
    /// Rows 0 and 1 take no object pixels. That is carried as established
    /// behavior and is not derived from a drawing here.
    fn render_objects(&mut self) {
        let sprite_count = self.sprite_cache.count();
        if sprite_count == 0 {
            return;
        }
        self.object_claimed.fill(false);
        let sprite_ram = self.map.region_data(Region::SpriteRam);

        for offs in (0..sprite_ram.len()).step_by(4).rev() {
            let flags = sprite_ram[offs + 1];
            let code = (usize::from(sprite_ram[offs + 2]) | (usize::from(flags & 0x08) << 5))
                % sprite_count;
            let bank = (!flags & 3) << 4;
            let hflip = if flags & 0x10 != 0 { 31 } else { 0 };
            let vflip = if flags & 0x20 != 0 { 31 } else { 0 };
            let sx = ((i32::from(sprite_ram[offs + 3]) - 3) * 2) & 0x1FF;
            let sy = ((241 - i32::from(sprite_ram[offs])) * 2) & 0x1FF;

            for y in 0..32usize {
                let ty = ((sy + y as i32) & 0x1FF) as usize;
                if !(2..NATIVE_HEIGHT).contains(&ty) {
                    continue;
                }
                let row = self.sprite_cache.row_slice(code, y ^ vflip);
                let line = ty * NATIVE_WIDTH;
                for x in 0..32usize {
                    // The X counter is nine bits and the line is 512 wide, so
                    // every wrapped position is on screen.
                    let at = line + ((sx + x as i32) & 0x1FF) as usize;
                    if self.object_claimed[at] {
                        continue;
                    }
                    let pen = row[x ^ hflip];
                    if pen & 0x0F != 0 {
                        self.object_claimed[at] = true;
                        if pen & 0x07 != 0 {
                            self.pixel_buffer[at] = bank | pen;
                        }
                    }
                }
            }
        }
    }

    /// Resolve the indexed frame to RGB24, unrotated: the monitor is mounted
    /// horizontally.
    pub fn render_frame(&self, buffer: &mut [u8]) {
        gfx::resolve_indexed_rows(
            &self.pixel_buffer,
            NATIVE_WIDTH,
            |_| &self.palette_rgb[..],
            buffer,
        );
    }

    // -----------------------------------------------------------------------
    // Audio, reset, debug
    // -----------------------------------------------------------------------

    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.ssio.fill_audio(buffer)
    }

    /// Reset everything but the CPU (which the machine owns) and NVRAM (which
    /// is battery-backed).
    pub fn reset_board(&mut self) {
        self.ctc.reset();
        self.ssio.reset();
        self.map.region_data_mut(Region::SpriteRam).fill(0);
        self.map.region_data_mut(Region::VideoRam).fill(0);
        self.palette_words.fill(0);
        self.rebuild_palette();
        self.pixel_buffer.fill(0);
        self.object_claimed.fill(false);
        self.clock = 0;
        self.clocks.reset();
        self.watchdog_counter = 0;
        self.ctc_ack_needed = false;
        self.ctc_vector_latch = 0;
    }

    pub fn instruction_boundaries(cpu: &Z80) -> u32 {
        u32::from(cpu.at_instruction_boundary())
    }
}

impl Default for Mcr91490Board {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus: the main Z80's memory and I/O map
// ---------------------------------------------------------------------------

impl Bus for Mcr91490Board {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let data = match self.map.page(addr).region_id {
            Region::ROM | Region::NVRAM | Region::SPRITE_RAM | Region::VIDEO_RAM => {
                self.map.read_backing(addr)
            }
            // The palette is write-only, and 0xEC00-0xEFFF decodes nothing.
            _ => 0xFF,
        };
        self.map.watch_read(0, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.map.watch_write(0, master, addr, data);
        match self.map.page(addr).region_id {
            Region::NVRAM | Region::SPRITE_RAM | Region::VIDEO_RAM => {
                self.map.write_backing(addr, data);
            }
            Region::PALETTE => self.write_palette(usize::from(addr), data),
            _ => {} // ROM and unmapped
        }
    }

    fn io_read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        let port = addr as u8;
        match port {
            // SSIO: bits 3 and 4 are not decoded.
            0x00..=0x1F => match port & 0x07 {
                base @ 0x00..=0x04 => self.ssio.input_port(usize::from(base)),
                0x07 => self.ssio.status_read(),
                _ => 0xFF,
            },
            0xF0..=0xF3 => self.ctc.read(port - 0xF0),
            _ => 0xFF,
        }
    }

    fn io_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.map.trace_bus_io_write(master, addr, data, None);
        let port = addr as u8;
        match port {
            // SSIO output ports: coin meters, LEDs and cocktail flip on port 0.
            // None of them is modeled.
            0x00..=0x07 => {}
            0x1C..=0x1F => self.ssio.latch_write(port - 0x1C, data),
            0xE0 => self.watchdog_counter = 0,
            0xF0..=0xF3 => self.ctc.write(port - 0xF0, data),
            _ => {}
        }
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        match target {
            BusMaster::Cpu(0) => {
                if self.ctc.interrupt_pending() {
                    let vector = self.ctc.interrupt_vector();
                    self.ctc_vector_latch = vector;
                    self.ctc_ack_needed = true;
                    InterruptState {
                        irq: true,
                        irq_vector: vector,
                        ..Default::default()
                    }
                } else {
                    // The Z80 reads the vector during the acknowledge whether
                    // or not the line is still asserted.
                    InterruptState {
                        irq_vector: self.ctc_vector_latch,
                        ..Default::default()
                    }
                }
            }
            _ => InterruptState::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 20 MHz over 4 is the CPU and over 2 the dot clock, so a 634-dot line is
    /// 317 CPU cycles exactly and a frame is 512 of them.
    #[test]
    fn a_line_is_317_cycles_and_a_frame_about_30_8_hz() {
        let tree = clock_tree();
        let cpu = tree.find(Clk::Cpu).expect("cpu domain");
        let dot = tree.find(Clk::Pixel).expect("pixel domain");
        assert_eq!(tree.hz(cpu), TIMING.cpu_clock_hz);
        assert_eq!(tree.hz(dot), 10_000_000);
        assert_eq!(
            u64::from(mcr2::HTOTAL_DOTS) * tree.hz(cpu),
            TIMING.cycles_per_scanline * tree.hz(dot),
        );
        assert_eq!(TIMING.cycles_per_frame(), 162_304);
        let hz = TIMING.frame_rate_hz();
        assert!((30.7..30.9).contains(&hz), "frame rate {hz:.3} Hz");
    }

    /// `VBLNK` reaches channel 2 once per field and `493` reaches channel 3
    /// once per frame, counted through the device.
    #[test]
    fn the_ctc_sees_vblank_twice_a_frame_and_493_once() {
        const START: u8 = 200;
        const COUNTER_RISING: u8 = 0x40 | 0x10 | 0x04 | 0x01;
        let mut board = Mcr91490Board::new();
        for ch in [2u8, 3] {
            board.ctc.write(ch, COUNTER_RISING);
            board.ctc.write(ch, START);
        }
        for line in 0..TIMING.total_scanlines {
            board.begin_scanline(line);
        }
        assert_eq!(START - board.ctc.read(2), 2);
        assert_eq!(START - board.ctc.read(3), 1);
    }

    /// Channel 0's zero count clocks channel 1: the pin 7 to pin 22 wire.
    #[test]
    fn channel_0_zero_count_clocks_channel_1() {
        const TIMER_PRESCALE_16: u8 = 0x04 | 0x02 | 0x01;
        const COUNTER_RISING: u8 = 0x40 | 0x10 | 0x04 | 0x01;
        let mut board = Mcr91490Board::new();
        let cpu = Z80::new();
        board.ctc.write(1, COUNTER_RISING);
        board.ctc.write(1, 200);
        board.ctc.write(0, TIMER_PRESCALE_16);
        board.ctc.write(0, 3);
        for _ in 0..16 * 3 * 10 {
            board.begin_cycle_inner(&cpu);
            board.end_cycle();
        }
        assert_eq!(200 - board.ctc.read(1), 10);
    }

    #[test]
    fn memory_map_regions_and_mirrors() {
        let mut board = Mcr91490Board::new();
        let m = BusMaster::Cpu(0);
        board.map.region_data_mut(Region::Rom)[0xDFFF] = 0x5A;
        assert_eq!(Bus::read(&mut board, m, 0xDFFF), 0x5A);

        Bus::write(&mut board, m, 0xE123, 0x11);
        assert_eq!(board.map.region_data(Region::Nvram)[0x123], 0x11);

        Bus::write(&mut board, m, 0xE810, 0x22);
        assert_eq!(Bus::read(&mut board, m, 0xEA10), 0x22, "sprite RAM mirror");

        Bus::write(&mut board, m, 0xF7FF, 0x33);
        assert_eq!(board.map.region_data(Region::VideoRam)[0x7FF], 0x33);

        Bus::write(&mut board, m, 0xEC00, 0x44);
        assert_eq!(Bus::read(&mut board, m, 0xEC00), 0xFF, "unmapped");
    }

    /// The palette is write-only, repeats every 0x80 bytes, and takes its
    /// ninth bit from the address.
    #[test]
    fn palette_writes_decode_9_bits_through_the_mirror() {
        let mut board = Mcr91490Board::new();
        let m = BusMaster::Cpu(0);
        // Entry 0, odd address: 0xFF with the address bit is 0x1FF, white.
        Bus::write(&mut board, m, 0xF801, 0xFF);
        assert_eq!(board.palette_rgb[0], (255, 255, 255));
        // Entry 0, even address: 0x038 is blue at full scale only.
        Bus::write(&mut board, m, 0xF800, 0x38);
        assert_eq!(board.palette_rgb[0], (0, 0, 255));
        // Entry 1 through a mirror at 0xFF80 + 2.
        Bus::write(&mut board, m, 0xFF82, 0x07);
        assert_eq!(board.palette_rgb[1], (0, 255, 0));
        assert_eq!(Bus::read(&mut board, m, 0xF800), 0xFF, "write-only");
    }

    /// A cache of `pens.len()` elements, element `n` solid in `pens[n]`.
    fn solid_cache(pens: &[u8], size: usize) -> gfx::GfxCache {
        let mut cache = gfx::GfxCache::new(pens.len(), size, size);
        for (code, &pen) in pens.iter().enumerate() {
            for y in 0..size {
                for x in 0..size {
                    cache.set_pixel(code, x, y, pen);
                }
            }
        }
        cache
    }

    /// A board with solid tiles and objects, to test composition without ROMs.
    fn board_with_solid_gfx(tile_pens: &[u8], object_pens: &[u8]) -> Mcr91490Board {
        let mut board = Mcr91490Board::new();
        board.tile_cache = solid_cache(tile_pens, 8);
        board.sprite_cache = solid_cache(object_pens, 32);
        board
    }

    /// Place object `slot` at sprite-RAM coordinates (100, 200), which is
    /// pixel ((100 - 3) * 2, (241 - 200) * 2).
    fn place_object(board: &mut Mcr91490Board, slot: usize, code: u8, flags: u8) {
        let ram = board.map.region_data_mut(Region::SpriteRam);
        ram[slot * 4] = 200;
        ram[slot * 4 + 1] = flags;
        ram[slot * 4 + 2] = code;
        ram[slot * 4 + 3] = 100;
    }

    const OBJECT_PIXEL: usize = (241 - 200) * 2 * NATIVE_WIDTH + (100 - 3) * 2;

    #[test]
    fn tile_pen_0_is_opaque_and_takes_the_bank() {
        let mut board = board_with_solid_gfx(&[0], &[0]);
        // Tile 0 at the top-left, palette bank 2.
        board.map.region_data_mut(Region::VideoRam)[1] = 0x20;
        board.render_frame_internal();
        assert_eq!(board.pixel_buffer[0], 0x20);
        assert_eq!(board.pixel_buffer[NATIVE_WIDTH + 1], 0x20);
    }

    /// The object nearer the end of the list wins a pixel, and pen 8 claims a
    /// pixel without coloring it.
    #[test]
    fn later_objects_win_and_pen_8_is_a_hole() {
        // Tile pen 1 everywhere; object picture 0 is pen 8, picture 1 pen 5.
        // Flags 0x03 invert to object bank 0.
        let mut board = board_with_solid_gfx(&[1], &[8, 5]);
        place_object(&mut board, 0, 1, 0x03);
        place_object(&mut board, 1, 0, 0x03);
        board.render_frame_internal();
        assert_eq!(
            board.pixel_buffer[OBJECT_PIXEL], 0x01,
            "pen 8 on the later object shows the tile and hides the earlier one"
        );

        // Swap the order: the pen-5 object now wins.
        place_object(&mut board, 0, 0, 0x03);
        place_object(&mut board, 1, 1, 0x03);
        board.render_frame_internal();
        assert_eq!(board.pixel_buffer[OBJECT_PIXEL], 0x05);
    }

    #[test]
    fn object_bank_bits_are_inverted() {
        let mut board = board_with_solid_gfx(&[0], &[3]);
        place_object(&mut board, 0, 0, 0x01); // inverted: bank 2
        board.render_frame_internal();
        assert_eq!(board.pixel_buffer[OBJECT_PIXEL], 0x23);
    }
}
