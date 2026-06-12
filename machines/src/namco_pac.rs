use phosphor_core::core::machine::InputButton;
use phosphor_core::core::memory_map::{AccessKind, MemoryMap};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::CpuStateTrait;
use phosphor_core::cpu::state::Z80State;
use phosphor_core::cpu::z80::Z80;
use phosphor_core::device::namco_wsg::NamcoWsg;
use phosphor_core::gfx;
use phosphor_core::gfx::decode::{GfxLayout, decode_gfx};
use phosphor_macros::{BusDebug, MemoryRegion};

// ---------------------------------------------------------------------------
// Memory map region IDs (shared across all Namco Pac-Man hardware games)
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum Region {
    Rom = 1,
    VideoRam = 2,
    ColorRam = 3,
    Ram = 4,
    Io = 5,
}

// ---------------------------------------------------------------------------
// Input button IDs (shared across Pac-Man family)
// ---------------------------------------------------------------------------
pub const INPUT_P1_UP: u8 = 0;
pub const INPUT_P1_LEFT: u8 = 1;
pub const INPUT_P1_RIGHT: u8 = 2;
pub const INPUT_P1_DOWN: u8 = 3;
pub const INPUT_COIN: u8 = 4;
pub const INPUT_P1_START: u8 = 5;
pub const INPUT_P2_START: u8 = 6;
pub const INPUT_P2_UP: u8 = 7;
pub const INPUT_P2_LEFT: u8 = 8;
pub const INPUT_P2_RIGHT: u8 = 9;
pub const INPUT_P2_DOWN: u8 = 10;

pub const NAMCO_PAC_INPUT_MAP: &[InputButton] = &[
    InputButton {
        id: INPUT_P1_UP,
        name: "P1 Up",
    },
    InputButton {
        id: INPUT_P1_LEFT,
        name: "P1 Left",
    },
    InputButton {
        id: INPUT_P1_RIGHT,
        name: "P1 Right",
    },
    InputButton {
        id: INPUT_P1_DOWN,
        name: "P1 Down",
    },
    InputButton {
        id: INPUT_COIN,
        name: "Coin",
    },
    InputButton {
        id: INPUT_P1_START,
        name: "P1 Start",
    },
    InputButton {
        id: INPUT_P2_START,
        name: "P2 Start",
    },
    InputButton {
        id: INPUT_P2_UP,
        name: "P2 Up",
    },
    InputButton {
        id: INPUT_P2_LEFT,
        name: "P2 Left",
    },
    InputButton {
        id: INPUT_P2_RIGHT,
        name: "P2 Right",
    },
    InputButton {
        id: INPUT_P2_DOWN,
        name: "P2 Down",
    },
];

// ---------------------------------------------------------------------------
// Timing constants
// ---------------------------------------------------------------------------
// Master clock:  18.432 MHz
// CPU clock:     18.432 / 6 = 3.072 MHz
// Pixel clock:   18.432 / 3 = 6.144 MHz
// HTOTAL:        384 pixels = 192 CPU cycles per scanline
// VTOTAL:        264 lines
// VBSTART:       224 (visible height)
// Frame:         192 × 264 = 50688 CPU cycles per frame
// Frame rate:    3072000 / 50688 ≈ 60.61 Hz

pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 3_072_000,  // 18.432 MHz / 6
    cycles_per_scanline: 192, // 384 pixels / 2
    total_scanlines: 264,     // VTOTAL
    display_width: 224,       // rotated 90° CCW from native 288×224
    display_height: 288,
};

pub const VISIBLE_LINES: u64 = 224;

// Resistor weights for palette PROM
// 3-bit RGB channels with 1K/470/220 ohm resistors
const R_WEIGHTS: [f64; 3] = [1000.0, 470.0, 220.0];
const G_WEIGHTS: [f64; 3] = [1000.0, 470.0, 220.0];
const B_WEIGHTS: [f64; 2] = [470.0, 220.0];

// ---------------------------------------------------------------------------
// GfxLayout descriptors for Pac-Man hardware
// ---------------------------------------------------------------------------

pub(crate) const PACMAN_TILE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[4, 0],
    x_offsets: &[64, 65, 66, 67, 0, 1, 2, 3],
    y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
    char_increment: 128,
};

const PACMAN_SPRITE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[4, 0],
    x_offsets: &[
        64, 65, 66, 67, 128, 129, 130, 131, 192, 193, 194, 195, 0, 1, 2, 3,
    ],
    y_offsets: &[
        0, 8, 16, 24, 32, 40, 48, 56, 256, 264, 272, 280, 288, 296, 304, 312,
    ],
    char_increment: 512,
};

// ---------------------------------------------------------------------------
// NamcoPacBoard — shared hardware for the Namco Pac-Man platform
// ---------------------------------------------------------------------------

/// Namco Pac-Man hardware base (Z80 @ 3.072 MHz, Namco WSG 3-voice, tilemap + sprites).
///
/// Shared by Pac-Man, Ms. Pac-Man, and other games on identical hardware.
/// Game wrappers compose this struct and implement Bus to route memory accesses.
#[derive(BusDebug)]
pub struct NamcoPacBoard {
    #[debug_cpu("Z80")]
    pub(crate) cpu: Z80,

    #[debug_map(cpu = 0)]
    pub(crate) map: MemoryMap,

    pub(crate) sprite_coords: [u8; 0x10], // 0x5060-0x506F: sprite X/Y positions

    // Sound
    #[debug_device("NamcoWSG")]
    pub(crate) wsg: NamcoWsg,

    // Pre-decoded GFX caches (from GFX ROM)
    pub(crate) tile_cache: gfx::GfxCache,
    pub(crate) sprite_cache: gfx::GfxCache,

    // PROMs
    pub(crate) palette_prom: [u8; 32],
    pub(crate) color_lut_prom: [u8; 256],

    // Pre-computed palette (32 RGB entries from PROM resistor weighting)
    pub(crate) palette_rgb: [(u8, u8, u8); 32],

    // Scanline-rendered framebuffer (288 x 224 x RGB24 = 193,536 bytes).
    // Native orientation, populated incrementally during run_frame().
    pub(crate) scanline_buffer: Vec<u8>,

    // I/O state (active-low: 0xFF = all released)
    pub(crate) in0: u8,
    pub(crate) in1: u8,
    pub(crate) dip_switches: u8,

    // 74LS259 addressable latch outputs
    pub(crate) irq_enabled: bool,
    pub(crate) sound_enabled: bool,
    pub(crate) flip_screen: bool,

    // Interrupt
    pub(crate) interrupt_vector: u8,
    pub(crate) vblank_irq_pending: bool,

    // Timing
    pub(crate) clock: u64,
    pub(crate) watchdog_counter: u32,
}

impl Default for NamcoPacBoard {
    fn default() -> Self {
        Self::new()
    }
}

impl NamcoPacBoard {
    pub fn new() -> Self {
        Self {
            cpu: Z80::new(),
            map: Self::build_map(),
            sprite_coords: [0; 0x10],
            wsg: NamcoWsg::new(TIMING.cpu_clock_hz),
            tile_cache: gfx::GfxCache::new(256, 8, 8),
            sprite_cache: gfx::GfxCache::new(64, 16, 16),
            palette_prom: [0; 32],
            color_lut_prom: [0; 256],
            palette_rgb: [(0, 0, 0); 32],
            scanline_buffer: vec![0u8; 288 * 224 * 3],
            in0: 0xFF,
            in1: 0xFF,
            // Default DIP: 1 coin/1 credit, 3 lives, 10000 bonus, normal difficulty, normal ghosts
            dip_switches: 0xC9,
            irq_enabled: false,
            sound_enabled: false,
            flip_screen: false,
            interrupt_vector: 0,
            vblank_irq_pending: false,
            clock: 0,
            watchdog_counter: 0,
        }
    }

    fn build_map() -> MemoryMap {
        let mut map = MemoryMap::new();
        map.region(
            Region::Rom,
            "Program ROM",
            0x0000,
            0x4000,
            AccessKind::ReadOnly,
        )
        .region(
            Region::VideoRam,
            "Video RAM",
            0x4000,
            0x0400,
            AccessKind::ReadWrite,
        )
        .region(
            Region::ColorRam,
            "Color RAM",
            0x4400,
            0x0400,
            AccessKind::ReadWrite,
        )
        .region(Region::Ram, "RAM", 0x4C00, 0x0400, AccessKind::ReadWrite)
        .region(Region::Io, "I/O", 0x5000, 0x0100, AccessKind::Io);
        map
    }

    // -----------------------------------------------------------------------
    // Core tick — called from game wrappers via bus_split!
    // -----------------------------------------------------------------------

    pub fn tick(&mut self, bus: &mut dyn Bus<Address = u16, Data = u8>) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();

        // Per-scanline rendering: at each scanline boundary, render the current
        // scanline from VRAM + sprites before the CPU processes it, matching
        // hardware CRT read timing.
        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let scanline = (frame_cycle / TIMING.cycles_per_scanline) as u16;
            if scanline < VISIBLE_LINES as u16 {
                self.render_scanline(scanline as usize);
            }
        }

        // VBLANK interrupt: fire at the start of VBLANK (scanline 224)
        let vblank_cycle = VISIBLE_LINES * TIMING.cycles_per_scanline;
        if frame_cycle == vblank_cycle {
            self.vblank_irq_pending = true;
        }

        // WSG tick (runs at CPU clock rate)
        self.wsg.tick();

        // Latch watchpoint attribution context (cycle + instruction PC)
        // before CPU execution — bus dispatch cannot read CPU state mid-tick.
        if self.map.has_any_watchpoints() {
            let pc = self
                .cpu
                .at_instruction_boundary()
                .then_some(self.cpu.pc as u32);
            self.map.latch_access_context(self.clock, pc);
        }

        self.cpu.execute_cycle(bus, BusMaster::Cpu(0));

        self.clock += 1;
        self.watchdog_counter += 1;
    }

    // -----------------------------------------------------------------------
    // Bus dispatch helpers — called from game wrapper Bus impls
    // -----------------------------------------------------------------------

    /// Shared memory read logic for all Namco Pac hardware.
    /// Caller is responsible for address masking (e.g. A15 mirror).
    pub fn bus_read_common(&mut self, addr: u16) -> u8 {
        let data = match self.map.page(addr).region_id {
            Region::ROM | Region::VIDEO_RAM | Region::COLOR_RAM | Region::RAM => {
                self.map.read_backing(addr)
            }

            Region::IO => match addr {
                0x5000..=0x503F => self.in0,
                0x5040..=0x507F => self.in1,
                0x5080..=0x50BF => self.dip_switches,
                _ => 0xFF,
            },

            _ => {
                // Bus float at 0x4800-0x4BFF (no device responds)
                if (0x4800..0x4C00).contains(&addr) {
                    0xBF
                } else {
                    0xFF
                }
            }
        };

        // Single-CPU board: all bus accesses originate from CPU 0.
        self.map.watch_read(0, BusMaster::Cpu(0), addr, data);
        data
    }

    /// Shared memory write logic for all Namco Pac hardware.
    /// Caller is responsible for address masking (e.g. A15 mirror).
    pub fn bus_write_common(&mut self, addr: u16, data: u8) {
        self.map.watch_write(0, BusMaster::Cpu(0), addr, data);

        match self.map.page(addr).region_id {
            Region::VIDEO_RAM | Region::COLOR_RAM | Region::RAM => {
                self.map.write_backing(addr, data);
            }

            Region::IO => match addr {
                // 74LS259 addressable latch: address bits 0-2 select output, data bit 0 is value
                0x5000..=0x5007 => {
                    let bit = (addr & 7) as u8;
                    let value = (data & 1) != 0;
                    match bit {
                        0 => {
                            self.irq_enabled = value;
                            if !value {
                                self.vblank_irq_pending = false;
                            }
                        }
                        1 => {
                            self.sound_enabled = value;
                            self.wsg.set_sound_enabled(value);
                        }
                        3 => self.flip_screen = value,
                        // 2: unused, 4-5: LEDs (not connected), 6: coin lockout, 7: coin counter
                        _ => {}
                    }
                }

                // Namco WSG sound registers (32 nibble registers)
                0x5040..=0x505F => self.wsg.write(addr - 0x5040, data),

                // Sprite coordinates
                0x5060..=0x506F => self.sprite_coords[(addr - 0x5060) as usize] = data,

                // Watchdog reset
                0x50C0..=0x50FF => self.watchdog_counter = 0,

                _ => {}
            },

            _ => { /* ROM or unmapped: ignored */ }
        }
    }

    // -----------------------------------------------------------------------
    // Palette
    // -----------------------------------------------------------------------

    /// Pre-compute the 32-entry RGB palette from the palette PROM using
    /// resistor-weighted DAC values.
    pub fn build_palette(&mut self) {
        use phosphor_core::gfx::{combine_weights, compute_resistor_weights};

        let r_w = compute_resistor_weights(&R_WEIGHTS, None);
        let g_w = compute_resistor_weights(&G_WEIGHTS, None);
        let b_w = compute_resistor_weights(&B_WEIGHTS, None);

        for i in 0..32 {
            let entry = self.palette_prom[i];

            let r = combine_weights(&r_w, &[entry & 1, (entry >> 1) & 1, (entry >> 2) & 1]);
            let g = combine_weights(
                &g_w,
                &[(entry >> 3) & 1, (entry >> 4) & 1, (entry >> 5) & 1],
            );
            let b = combine_weights(&b_w, &[(entry >> 6) & 1, (entry >> 7) & 1]);

            self.palette_rgb[i] = (r, g, b);
        }
    }

    // -----------------------------------------------------------------------
    // ROM loading helpers
    // -----------------------------------------------------------------------

    pub fn load_program_rom(&mut self, data: &[u8]) {
        self.map.load_region(Region::Rom, data);
    }

    pub fn load_gfx_rom(&mut self, gfx_data: &[u8]) {
        self.tile_cache = decode_gfx(gfx_data, 0x0000, 256, &PACMAN_TILE_LAYOUT);
        self.sprite_cache = decode_gfx(gfx_data, 0x1000, 64, &PACMAN_SPRITE_LAYOUT);
    }

    pub fn load_color_proms(&mut self, color_data: &[u8]) {
        self.palette_prom.copy_from_slice(&color_data[0..32]);
        self.color_lut_prom.copy_from_slice(&color_data[32..288]);
        self.build_palette();
    }

    pub fn load_sound_prom(&mut self, sound_data: &[u8]) {
        self.wsg.load_waveform_rom(sound_data);
    }

    // -----------------------------------------------------------------------
    // CPU state accessors
    // -----------------------------------------------------------------------

    pub fn get_cpu_state(&self) -> Z80State {
        self.cpu.snapshot()
    }

    pub fn clock(&self) -> u64 {
        self.clock
    }

    // -----------------------------------------------------------------------
    // Video rendering
    // -----------------------------------------------------------------------

    /// Map a tile index in the 36×28 tilemap to a VRAM offset.
    ///
    /// The Pac-Man tilemap uses a non-linear address mapping:
    ///   row += 2; col -= 2;
    ///   if (col & 0x20) return row + ((col & 0x1f) << 5);
    ///   else return col + (row << 5);
    fn tilemap_offset(col: i32, row: i32) -> usize {
        let r = row + 2;
        let c = col - 2;
        if c & 0x20 != 0 {
            (r + ((c & 0x1F) << 5)) as usize
        } else {
            (c + (r << 5)) as usize
        }
    }

    /// Render a single scanline from current VRAM/sprite state into the scanline buffer.
    /// Composites tiles then sprites for native scanline Y (0-223).
    fn render_scanline(&mut self, scanline: usize) {
        let row_offset = scanline * 288 * 3;

        // Split borrows: immutable refs for closures, mutable ref for buffer
        let video_ram = self.map.region_data(Region::VideoRam);
        let color_ram = self.map.region_data(Region::ColorRam);
        let color_lut_prom = &self.color_lut_prom;
        let palette_rgb = &self.palette_rgb;
        let tile_cache = &self.tile_cache;
        let sprite_cache = &self.sprite_cache;
        let buf = &mut self.scanline_buffer[row_offset..row_offset + 288 * 3];

        // Inline color resolution (captures split borrows, not &self)
        let resolve = |attribute: u8, pixel_value: u8| -> (u8, u8, u8) {
            let lut_index = ((attribute & 0x1F) as usize) * 4 + pixel_value as usize;
            let palette_index = if lut_index < 256 {
                (color_lut_prom[lut_index] & 0x0F) as usize
            } else {
                0
            };
            palette_rgb[palette_index]
        };

        // Fill scanline with background color
        let bg = resolve(0, 0);
        for x in 0..288 {
            let off = x * 3;
            buf[off] = bg.0;
            buf[off + 1] = bg.1;
            buf[off + 2] = bg.2;
        }

        // Tiles: use shared tilemap renderer
        let config = gfx::TilemapConfig {
            cols: 36,
            rows: 28,
            tile_width: 8,
            tile_height: 8,
        };

        gfx::tilemap::render_tilemap_scanline(
            &config,
            tile_cache,
            scanline,
            |col, row| {
                let offset = Self::tilemap_offset(col as i32, row as i32);
                let tile_code = if offset < 0x400 {
                    video_ram[offset] as u16
                } else {
                    0
                };
                let attribute = if offset < 0x400 { color_ram[offset] } else { 0 };
                (tile_code, attribute)
            },
            resolve,
            buf,
            0,
        );

        // Sprites: draw in priority order (7→3, then 2→0 with +1 Y offset)
        let ram = self.map.region_data(Region::Ram);
        let sprite_coords = &self.sprite_coords;
        let y = scanline as i32;

        for pass in 0..2 {
            let (start, end, y_offset): (usize, usize, i32) =
                if pass == 0 { (7, 3, 0) } else { (2, 0, 1) };

            let mut offs = start;
            loop {
                let attr_base = 0x3F0 + offs * 2;
                let coord_base = offs * 2;

                let sprite_byte0 = ram[attr_base];
                let sprite_byte1 = ram[attr_base + 1];

                let sprite_code = (sprite_byte0 >> 2) as u16;
                let x_flip = (sprite_byte0 & 1) != 0;
                let y_flip = (sprite_byte0 & 2) != 0;
                let attribute = sprite_byte1 & 0x1F;

                let sx = 272i32 - sprite_coords[coord_base + 1] as i32;
                let sy = sprite_coords[coord_base] as i32 - 31 + y_offset;

                if y >= sy && y < sy + 16 {
                    let spy = (y - sy) as u8;
                    let src_py = if y_flip { 15 - spy } else { spy };

                    // Pre-compute transparency mask for this sprite's attribute
                    let trans_base = (attribute as usize & 0x1F) * 4;
                    let mut trans_mask: u8 = 0;
                    for pv in 0..4u8 {
                        if (color_lut_prom[trans_base + pv as usize] & 0x0F) == 0 {
                            trans_mask |= 1 << pv;
                        }
                    }

                    let clip = gfx::sprite::SpriteClip {
                        x_min: 16,
                        x_max: 272,
                        wrap_offset: Some(-256), // tunnel wraparound
                    };
                    gfx::sprite::draw_sprite_row(
                        sprite_cache,
                        sprite_code,
                        src_py as usize,
                        sx,
                        x_flip,
                        |pv| (trans_mask >> pv) & 1 != 0,
                        |pv| resolve(attribute, pv),
                        buf,
                        &clip,
                    );
                }

                if offs == end {
                    break;
                }
                offs -= 1;
            }
        }
    }

    /// Rotate 90° CCW from native scanline_buffer (288w × 224h)
    /// to output buffer (224w × 288h).
    pub fn render_frame(&self, buffer: &mut [u8]) {
        gfx::rotate_90_ccw(&self.scanline_buffer, buffer, 288, 224);
    }

    // -----------------------------------------------------------------------
    // Audio
    // -----------------------------------------------------------------------

    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.wsg.fill_audio(buffer)
    }

    // -----------------------------------------------------------------------
    // Reset
    // -----------------------------------------------------------------------

    /// Reset all board state except ROMs, GFX caches, and palette.
    /// The caller must reset the CPU separately (requires bus_split).
    pub fn reset_board(&mut self) {
        self.wsg.reset();
        self.irq_enabled = false;
        self.sound_enabled = false;
        self.flip_screen = false;
        self.interrupt_vector = 0;
        self.vblank_irq_pending = false;
        self.clock = 0;
        self.watchdog_counter = 0;
        self.in0 = 0xFF;
        self.in1 = 0xFF;
        self.map.region_data_mut(Region::VideoRam).fill(0);
        self.map.region_data_mut(Region::ColorRam).fill(0);
        self.map.region_data_mut(Region::Ram).fill(0);
        self.sprite_coords = [0; 0x10];
        self.scanline_buffer.fill(0);
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

impl Saveable for NamcoPacBoard {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        w.write_bytes(self.map.region_data(Region::VideoRam));
        w.write_bytes(self.map.region_data(Region::ColorRam));
        w.write_bytes(self.map.region_data(Region::Ram));
        w.write_bytes(&self.sprite_coords);
        self.wsg.save_state(w);
        w.write_u8(self.in0);
        w.write_u8(self.in1);
        w.write_bool(self.irq_enabled);
        w.write_bool(self.sound_enabled);
        w.write_bool(self.flip_screen);
        w.write_u8(self.interrupt_vector);
        w.write_bool(self.vblank_irq_pending);
        w.write_u64_le(self.clock);
        w.write_u32_le(self.watchdog_counter);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        r.read_bytes_into(self.map.region_data_mut(Region::VideoRam))?;
        r.read_bytes_into(self.map.region_data_mut(Region::ColorRam))?;
        r.read_bytes_into(self.map.region_data_mut(Region::Ram))?;
        r.read_bytes_into(&mut self.sprite_coords)?;
        self.wsg.load_state(r)?;
        self.in0 = r.read_u8()?;
        self.in1 = r.read_u8()?;
        self.irq_enabled = r.read_bool()?;
        self.sound_enabled = r.read_bool()?;
        self.flip_screen = r.read_bool()?;
        self.interrupt_vector = r.read_u8()?;
        self.vblank_irq_pending = r.read_bool()?;
        self.clock = r.read_u64_le()?;
        self.watchdog_counter = r.read_u32_le()?;
        Ok(())
    }
}

impl NamcoPacBoard {
    /// Dispatch an input event to the appropriate port bit (active-low).
    /// Called from game wrapper `InputReceiver` impls.
    pub fn handle_input(&mut self, button: u8, pressed: bool) {
        match button {
            INPUT_P1_UP => crate::set_bit_active_low(&mut self.in0, 0, pressed),
            INPUT_P1_LEFT => crate::set_bit_active_low(&mut self.in0, 1, pressed),
            INPUT_P1_RIGHT => crate::set_bit_active_low(&mut self.in0, 2, pressed),
            INPUT_P1_DOWN => crate::set_bit_active_low(&mut self.in0, 3, pressed),
            INPUT_COIN => crate::set_bit_active_low(&mut self.in0, 5, pressed),
            INPUT_P2_UP => crate::set_bit_active_low(&mut self.in1, 0, pressed),
            INPUT_P2_LEFT => crate::set_bit_active_low(&mut self.in1, 1, pressed),
            INPUT_P2_RIGHT => crate::set_bit_active_low(&mut self.in1, 2, pressed),
            INPUT_P2_DOWN => crate::set_bit_active_low(&mut self.in1, 3, pressed),
            INPUT_P1_START => crate::set_bit_active_low(&mut self.in1, 5, pressed),
            INPUT_P2_START => crate::set_bit_active_low(&mut self.in1, 6, pressed),
            _ => {}
        }
    }
}
