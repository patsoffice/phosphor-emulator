use phosphor_core::core::memory_map::{AccessKind, AddressSpace16};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{ClockDivider, TimingConfig};
use phosphor_core::cpu::z80::Z80;
use phosphor_core::device::Z80Ctc;
use phosphor_core::dirty_bitset::DirtyBitset;
use phosphor_core::gfx;
use phosphor_core::gfx::decode::{GfxLayout, decode_gfx};
use phosphor_macros::{BusDebug, MemoryRegion};

use phosphor_core::device::SsioBoard;

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum Region {
    Rom = 1,
    Nvram = 2,
    SpriteRam = 3,
    VideoRam = 4,
}

// ---------------------------------------------------------------------------
// MCR II hardware constants
// ---------------------------------------------------------------------------
// Master oscillator: 19.968 MHz
// CPU clock: 19.968 / 8 = 2.496 MHz
// Pixel clock: 19.968 / 4 = 4.992 MHz
// HTOTAL: 512 pixel clocks = 256 CPU cycles per scanline
// VTOTAL: 264 lines per field
// VISIBLE: 240 scanlines per field (480 interlaced)
// Frame: 256 × 264 = 67584 CPU cycles per field

pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 2_496_000,             // 19.968 MHz / 8
    cycles_per_scanline: 256,            // 512 pixel clocks / 2
    total_scanlines: 264,                // VTOTAL
    display_width: NATIVE_HEIGHT as u32, // 480 (rotated 90° CW)
    display_height: NATIVE_WIDTH as u32, // 512
};

pub const VISIBLE_LINES: u64 = 240;

pub const OUTPUT_SAMPLE_RATE: u64 = 44_100;

// SSIO runs at 2 MHz, main CPU at 2.496 MHz. Ratio = 2000000/2496000 = 125/156.
pub const SSIO_CLOCK_NUM: u32 = 125;
pub const SSIO_CLOCK_DEN: u32 = 156;

// Native framebuffer: 512 wide × 480 tall (32×30 tiles at 16×16 pixels).
// Each 8×8 ROM tile is displayed at 2× in both dimensions.
pub const NATIVE_WIDTH: usize = 512;
pub const NATIVE_HEIGHT: usize = 480;

// Tilemap dimensions
pub(crate) const TILE_COLS: usize = 32;
pub(crate) const TILE_ROWS: usize = 30;

// ---------------------------------------------------------------------------
// 9-bit palette helpers
// ---------------------------------------------------------------------------

/// Expand 3-bit color to 8-bit (standard 3-to-8 expansion).
fn pal3bit(x: u8) -> u8 {
    let v = x & 7;
    (v << 5) | (v << 2) | (v >> 1)
}

// ---------------------------------------------------------------------------
// Mcr2Board — shared Bally Midway MCR II arcade hardware
// ---------------------------------------------------------------------------

/// Shared hardware for the Bally Midway MCR II platform.
///
/// Hardware: Z80 @ 2.496 MHz (main), SSIO sound board (Z80 + 2×AY-8910),
/// Z80 CTC for interrupt generation.
/// Video: 32×30 tile playfield (8×8 tiles displayed at 16×16) + 32×32 sprites,
/// 4bpp, 9-bit programmable palette (64 entries).
/// Screen: 512×480 interlaced, displayed rotated 90° CW on vertical monitor.
#[derive(BusDebug)]
pub struct Mcr2Board {
    // Main CPU (Z80 @ 2.496 MHz)
    #[debug_cpu("Z80 Main")]
    pub(crate) cpu: Z80,

    // Devices
    #[debug_device("SSIO")]
    pub(crate) ssio: SsioBoard,
    #[debug_device("CTC")]
    pub(crate) ctc: Z80Ctc,

    // Memory
    #[debug_map(cpu = 0)]
    pub(crate) map: AddressSpace16,

    // GFX caches (pre-decoded from ROM)
    pub(crate) tile_cache: gfx::GfxCache,
    pub(crate) sprite_cache: gfx::GfxCache,

    // Palette (64 entries; 9-bit values embedded in video_ram[0x780..0x800])
    // palette_ram caches the canonical 2-byte representation for save state.
    pub(crate) palette_ram: [u8; 0x80],
    pub(crate) palette_rgb: [(u8, u8, u8); 64],

    // Framebuffers (indexed — palette lookup deferred to rotation pass)
    pub(crate) pixel_buffer: Vec<u8>, // 512×480 palette index (u8)
    pub(crate) priority_buffer: Vec<u8>, // 512×480 (sprite palette bank per pixel)

    // Tile dirty tracking (960 tiles = 15 × 64 bits)
    pub(crate) tile_dirty: DirtyBitset<15>,
    // Tracks which tiles had sprites composited on them (for next-frame erasure)
    sprite_tile_dirty: DirtyBitset<15>,
    // Dirty tracking stats (for debug overlay)
    pub(crate) tiles_redrawn: usize,

    // CTC interrupt handling
    pub(crate) ctc_ack_needed: bool,
    pub(crate) ctc_vector_latch: u8,

    // Timing
    pub(crate) clock: u64,
    pub(crate) ssio_clock: ClockDivider,
    pub(crate) watchdog_counter: u16,
}

impl Mcr2Board {
    pub fn new() -> Self {
        Self {
            cpu: Z80::new(),
            ssio: SsioBoard::new(),
            ctc: Z80Ctc::new(),
            map: Self::build_map(),
            tile_cache: gfx::GfxCache::new(0, 8, 8),
            sprite_cache: gfx::GfxCache::new(0, 32, 32),
            palette_ram: [0; 0x80],
            palette_rgb: [(0, 0, 0); 64],
            pixel_buffer: vec![0u8; NATIVE_WIDTH * NATIVE_HEIGHT],
            priority_buffer: vec![0u8; NATIVE_WIDTH * NATIVE_HEIGHT],
            tile_dirty: DirtyBitset::new_all_dirty(),
            sprite_tile_dirty: DirtyBitset::new_all_dirty(),
            tiles_redrawn: 0,
            ctc_ack_needed: false,
            ctc_vector_latch: 0,
            clock: 0,
            ssio_clock: ClockDivider::new(SSIO_CLOCK_NUM, SSIO_CLOCK_DEN),
            watchdog_counter: 0,
        }
    }

    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(
            Region::Rom,
            "Program ROM",
            0x0000,
            0xC000,
            AccessKind::ReadOnly,
        )
        .region(
            Region::Nvram,
            "NVRAM",
            0xC000,
            0x0800,
            AccessKind::ReadWrite,
        )
        .region(
            Region::SpriteRam,
            "Sprite RAM",
            0xE000,
            0x0200,
            AccessKind::ReadWrite,
        )
        .region(
            Region::VideoRam,
            "Video RAM",
            0xE800,
            0x0800,
            AccessKind::ReadWrite,
        );
        // NVRAM mirrors (2KB repeated across 0xC000-0xDFFF)
        for i in 1..4u16 {
            map.mirror(0xC000 + i * 0x800, 0xC000, 0x0800);
        }
        // Sprite RAM mirrors within 0xE000-0xE7FF (512B repeated 4×)
        for i in 1..4u16 {
            map.mirror(0xE000 + i * 0x200, 0xE000, 0x0200);
        }
        // Sprite RAM mirrors within 0xF000-0xF7FF (512B repeated 4×)
        for i in 0..4u16 {
            map.mirror(0xF000 + i * 0x200, 0xE000, 0x0200);
        }
        // Video RAM mirror (0xF800-0xFFFF → 0xE800-0xEFFF)
        map.mirror(0xF800, 0xE800, 0x0800);
        map
    }

    /// Pre-decode tile and sprite ROMs into GFX caches.
    /// `bg_rom` is the background tile ROM, `fg_rom` is the sprite ROM.
    pub fn decode_gfx(&mut self, bg_rom: &[u8], fg_rom: &[u8]) {
        // Tiles: 4bpp, 8x8, ROM split in two halves
        let tile_count = bg_rom.len() / 32;
        let half_bits = (bg_rom.len() / 2) * 8;
        let tile_planes: [usize; 4] = [1, 0, half_bits + 1, half_bits];
        self.tile_cache = decode_gfx(
            bg_rom,
            0,
            tile_count,
            &GfxLayout {
                plane_offsets: &tile_planes,
                x_offsets: &[0, 2, 4, 6, 8, 10, 12, 14],
                y_offsets: &[0, 16, 32, 48, 64, 80, 96, 112],
                char_increment: 128,
            },
        );

        // Sprites: 4bpp, 32x32, 4 ROM quarters
        let sprite_count = fg_rom.len() / 512;
        let q8 = (fg_rom.len() / 4) * 8;
        let x_offsets: [usize; 32] =
            std::array::from_fn(|px| ((px / 2) % 4) * q8 + (px / 8) * 8 + (px % 2) * 4);
        let y_offsets: [usize; 32] = std::array::from_fn(|py| py * 32);
        self.sprite_cache = decode_gfx(
            fg_rom,
            0,
            sprite_count,
            &GfxLayout {
                plane_offsets: &[3, 2, 1, 0],
                x_offsets: &x_offsets,
                y_offsets: &y_offsets,
                char_increment: 1024,
            },
        );
    }

    // -----------------------------------------------------------------------
    // Palette
    // -----------------------------------------------------------------------

    /// Update palette entry from a video RAM write in the palette range.
    ///
    /// On real 90010 hardware, the palette occupies the upper 128 bytes of
    /// video RAM (offset 0x780-0x7FF). Each byte write immediately sets the
    /// 9-bit colour value: `val9 = data | (addr_bit0 << 8)`.
    pub fn update_palette_from_vram(&mut self, vram_offset: usize, data: u8) {
        let entry = (vram_offset / 2) & 0x3F;
        let val9 = data as u16 | (((vram_offset & 1) as u16) << 8);
        // Cache canonical bytes for save state (rebuild_palette reads these)
        self.palette_ram[entry * 2] = val9 as u8;
        self.palette_ram[entry * 2 + 1] = (val9 >> 8) as u8;
        let r = pal3bit((val9 >> 6) as u8);
        let g = pal3bit(val9 as u8);
        let b = pal3bit((val9 >> 3) as u8);
        self.palette_rgb[entry] = (r, g, b);
    }

    /// Rebuild the entire palette from the cached palette_ram (used after state load).
    pub fn rebuild_palette(&mut self) {
        for entry in 0..64 {
            let low = self.palette_ram[entry * 2] as u16;
            let high = self.palette_ram[entry * 2 + 1] as u16;
            let val9 = low | ((high & 1) << 8);
            let r = pal3bit((val9 >> 6) as u8);
            let g = pal3bit(val9 as u8);
            let b = pal3bit((val9 >> 3) as u8);
            self.palette_rgb[entry] = (r, g, b);
        }
    }

    /// Mark a tile as dirty from a VRAM write offset.
    ///
    /// Offsets 0x000–0x77F are tile data (2 bytes per tile, 960 tiles).
    /// Offsets 0x780–0x7FF are palette — use `tile_dirty.mark_all()` for those.
    #[inline]
    pub fn mark_tile_dirty(&mut self, vram_offset: usize) {
        if vram_offset < 0x780 {
            self.tile_dirty.mark(vram_offset / 2);
        }
    }

    // -----------------------------------------------------------------------
    // Core tick
    // -----------------------------------------------------------------------

    /// Execute one CPU cycle at the Z80 clock rate (2.496 MHz).
    ///
    /// The `bus` parameter is the game wrapper (which implements `Bus`) passed
    /// in from the wrapper's `run_frame()` / `debug_tick()`.
    pub fn tick(&mut self, bus: &mut dyn phosphor_core::core::Bus<Address = u16, Data = u8>) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();

        // CTC triggers at scanline boundaries
        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let scanline = frame_cycle / TIMING.cycles_per_scanline;

            // CTC channel 2: triggered at scanlines 0 and 240 (VBLANK)
            if scanline == 0 || scanline == VISIBLE_LINES {
                self.ctc.trigger(2, true);
                self.ctc.trigger(2, false);
            }

            // CTC channel 3: triggered at scanline 0 only (once per frame)
            if scanline == 0 {
                self.ctc.trigger(3, true);
                self.ctc.trigger(3, false);
            }
        }

        // Tick CTC (timer-mode channels count CPU clocks)
        self.ctc.tick();

        // Latch watchpoint attribution context (cycle + instruction PC)
        // before CPU execution — bus dispatch cannot read CPU state mid-tick.
        if self.map.has_any_watchpoints() {
            let pc = self
                .cpu
                .at_instruction_boundary()
                .then_some(self.cpu.pc as u32);
            self.map.latch_access_context(self.clock, pc);
        }

        // Execute main CPU cycle
        self.cpu
            .execute_cycle(bus, phosphor_core::core::BusMaster::Cpu(0));

        // Deferred CTC interrupt acknowledge (after CPU has read the vector)
        if self.ctc_ack_needed {
            self.ctc.acknowledge_interrupt();
            self.ctc_ack_needed = false;
        }

        // Tick SSIO at 125/156 ratio (2 MHz from 2.496 MHz)
        if self.ssio_clock.tick() {
            self.ssio.tick();
        }

        self.clock += 1;
        self.watchdog_counter = self.watchdog_counter.wrapping_add(1);
    }

    // -----------------------------------------------------------------------
    // Frame rendering
    // -----------------------------------------------------------------------

    /// Render the full frame into the indexed pixel buffer.
    /// Called once per frame from the game wrapper's run_frame().
    pub fn render_frame_internal(&mut self) {
        // Tiles under previous frame's sprites must be redrawn to erase
        // stale sprite pixels before compositing new sprites.
        self.tile_dirty.merge(&self.sprite_tile_dirty);
        self.sprite_tile_dirty.clear();

        self.render_tiles();
        self.render_sprites();
    }

    /// Render dirty tiles from video RAM into the indexed pixel buffer.
    fn render_tiles(&mut self) {
        let tile_count = self.tile_cache.count().max(1);
        let video_ram = self.map.region_data(Region::VideoRam);
        let mut redrawn = 0usize;

        for tile_row in 0..TILE_ROWS {
            for tile_col in 0..TILE_COLS {
                let tile_index = tile_row * TILE_COLS + tile_col;
                if !self.tile_dirty.is_dirty(tile_index) {
                    continue;
                }
                redrawn += 1;

                let vram_offset = tile_index * 2;
                let low = video_ram[vram_offset] as u16;
                let high = video_ram[vram_offset + 1] as u16;
                let data = low | (high << 8);

                let code = (data & 0x1FF) as usize % tile_count;
                let hflip = (data >> 9) & 1 != 0;
                let vflip = (data >> 10) & 1 != 0;
                let color = ((data >> 11) & 3) as u8;
                let spr_bank = ((data >> 14) & 3) as u8;
                let pri_val = spr_bank << 4;

                // Each 8×8 tile is rendered at 16×16 (2× in both dimensions).
                // Iterate source pixels and write 2×2 blocks to avoid redundant lookups.
                for src_y in 0..8usize {
                    let actual_py = if vflip { 7 - src_y } else { src_y };
                    let row = self.tile_cache.row_slice(code, actual_py);
                    let screen_y0 = tile_row * 16 + src_y * 2;
                    let row_base0 = screen_y0 * NATIVE_WIDTH + tile_col * 16;
                    let row_base1 = row_base0 + NATIVE_WIDTH;

                    for src_x in 0..8usize {
                        let actual_px = if hflip { 7 - src_x } else { src_x };
                        let pixel = row[actual_px];
                        let pal = if pixel != 0 { (color << 4) | pixel } else { 0 };
                        let dx = src_x * 2;
                        self.pixel_buffer[row_base0 + dx] = pal;
                        self.pixel_buffer[row_base0 + dx + 1] = pal;
                        self.pixel_buffer[row_base1 + dx] = pal;
                        self.pixel_buffer[row_base1 + dx + 1] = pal;
                        self.priority_buffer[row_base0 + dx] = pri_val;
                        self.priority_buffer[row_base0 + dx + 1] = pri_val;
                        self.priority_buffer[row_base1 + dx] = pri_val;
                        self.priority_buffer[row_base1 + dx + 1] = pri_val;
                    }
                }
            }
        }
        self.tile_dirty.clear();
        self.tiles_redrawn = redrawn;
    }

    /// Render sprites from sprite RAM, compositing with the priority buffer.
    fn render_sprites(&mut self) {
        let sprite_count = self.sprite_cache.count().max(1);
        let sprite_ram = self.map.region_data(Region::SpriteRam);

        // Iterate back-to-front (later entries have higher priority)
        let mut offs = sprite_ram.len().saturating_sub(4);
        loop {
            if sprite_ram[offs] != 0 {
                let code = (sprite_ram[offs + 1] & 0x3F) as usize % sprite_count;
                let hflip: usize = if sprite_ram[offs + 1] & 0x40 != 0 {
                    31
                } else {
                    0
                };
                let vflip: usize = if sprite_ram[offs + 1] & 0x80 != 0 {
                    31
                } else {
                    0
                };
                let sx = (sprite_ram[offs + 2] as i32) * 2;
                let sy = (240i32 - sprite_ram[offs] as i32) * 2;

                for y in 0..32usize {
                    let ty = ((sy + (y ^ vflip) as i32) & 0x1FF) as usize;
                    if ty >= NATIVE_HEIGHT {
                        continue;
                    }

                    for x in 0..32usize {
                        let tx = ((sx + (x ^ hflip) as i32) & 0x1FF) as usize;
                        if tx >= NATIVE_WIDTH {
                            continue;
                        }

                        // Source pixel is always (x, y) — flip only affects destination
                        let src_pixel = self.sprite_cache.pixel(code, x, y);
                        let buf_idx = ty * NATIVE_WIDTH + tx;
                        let pix = self.priority_buffer[buf_idx] | src_pixel;

                        if pix & 0x07 != 0 {
                            self.pixel_buffer[buf_idx] = pix;
                            self.sprite_tile_dirty.mark((ty / 16) * TILE_COLS + tx / 16);
                        }
                    }
                }
            }

            if offs < 4 {
                break;
            }
            offs -= 4;
        }
    }

    pub fn render_frame(&self, buffer: &mut [u8]) {
        gfx::rotate_90_ccw_indexed_blocked(
            &self.pixel_buffer,
            buffer,
            NATIVE_WIDTH,
            NATIVE_HEIGHT,
            &self.palette_rgb,
            16,
        );
    }

    // -----------------------------------------------------------------------
    // Audio
    // -----------------------------------------------------------------------

    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.ssio.fill_audio(buffer)
    }

    // -----------------------------------------------------------------------
    // Reset (does NOT reset CPU — wrapper must do that with bus_split)
    // -----------------------------------------------------------------------

    pub fn reset_board(&mut self) {
        self.ctc.reset();
        self.ssio.reset();
        self.map.region_data_mut(Region::SpriteRam).fill(0);
        self.map.region_data_mut(Region::VideoRam).fill(0);
        self.palette_ram.fill(0);
        self.rebuild_palette();
        self.pixel_buffer.fill(0);
        self.priority_buffer.fill(0);
        self.tile_dirty = DirtyBitset::new_all_dirty();
        self.sprite_tile_dirty = DirtyBitset::new_all_dirty();
        self.clock = 0;
        self.ssio_clock.reset();
        self.watchdog_counter = 0;
        self.ctc_ack_needed = false;
        self.ctc_vector_latch = 0;
        // NVRAM is NOT cleared (battery-backed)
    }

    // -----------------------------------------------------------------------
    // Debug
    // -----------------------------------------------------------------------

    pub fn debug_tick_boundaries(&self) -> u32 {
        if self.cpu.at_instruction_boundary() {
            1
        } else {
            0
        }
    }
}

impl Saveable for Mcr2Board {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        self.ctc.save_state(w);
        self.ssio.save_state(w);
        w.write_bytes(self.map.region_data(Region::Nvram));
        w.write_bytes(self.map.region_data(Region::SpriteRam));
        w.write_bytes(self.map.region_data(Region::VideoRam));
        w.write_bytes(&self.palette_ram);
        w.write_u64_le(self.clock);
        self.ssio_clock.save_state(w);
        w.write_u16_le(self.watchdog_counter);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        self.ctc.load_state(r)?;
        self.ssio.load_state(r)?;
        r.read_bytes_into(self.map.region_data_mut(Region::Nvram))?;
        r.read_bytes_into(self.map.region_data_mut(Region::SpriteRam))?;
        r.read_bytes_into(self.map.region_data_mut(Region::VideoRam))?;
        r.read_bytes_into(&mut self.palette_ram)?;
        self.clock = r.read_u64_le()?;
        self.ssio_clock.load_state(r)?;
        self.watchdog_counter = r.read_u16_le()?;
        // Rebuild derived state from loaded data
        self.rebuild_palette();
        self.tile_dirty = DirtyBitset::new_all_dirty();
        self.sprite_tile_dirty = DirtyBitset::new_all_dirty();
        self.ctc_ack_needed = false;
        self.ctc_vector_latch = 0;
        Ok(())
    }
}

impl Default for Mcr2Board {
    fn default() -> Self {
        Self::new()
    }
}
