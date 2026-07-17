use crate::dkong_sound::DkongDiscreteSound;
use phosphor_core::audio::AudioResampler;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind, DebugTraceBuffer};
use phosphor_core::core::machine::GfxSheet;
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::watchpoint::DebugAccessSource;
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, ClockDivider, TimingConfig};
use phosphor_core::cpu::i8035::I8035;
use phosphor_core::cpu::z80::Z80;
use phosphor_core::device::dac::Mc1408Dac;
use phosphor_core::device::i8257::I8257;
use phosphor_core::device::output_latch::OutputLatch;
use phosphor_core::gfx;
use phosphor_core::gfx::decode::{GfxLayout, decode_gfx};
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion};

// ---------------------------------------------------------------------------
// Memory map region IDs (machine-specific constants for page table dispatch)
// ---------------------------------------------------------------------------

/// Main CPU (Z80) address space region IDs.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum MainRegion {
    Rom = 1,       // 0x0000-0x5FFF (24KB max program ROM)
    Ram = 2,       // 0x6000-0x6FFF (4KB work RAM)
    SpriteRam = 3, // 0x7000-0x73FF (1KB sprite RAM)
    VideoRam = 4,  // 0x7400-0x77FF (1KB video RAM)
    IoDma = 5,     // 0x7800-0x78FF (DMA controller)
    IoPorts = 6,   // 0x7C00-0x7DFF (input/control ports)
}

/// Sound CPU (I8035) address space region IDs.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum SoundRegion {
    Rom = 1, // 0x0000-0x0FFF (4KB sound ROM)
}

// ---------------------------------------------------------------------------
// Shared timing constants (Nintendo TKG / TRS hardware)
// ---------------------------------------------------------------------------
// Master clock:  61.44 MHz
// CPU clock:     61.44 / 5 / 4 = 3.072 MHz
// Pixel clock:   61.44 / 10 = 6.144 MHz
// HTOTAL:        384 pixels = 192 CPU cycles per scanline
// VTOTAL:        264 lines
// VBSTART:       240 (visible height)
// Frame:         192 × 264 = 50688 CPU cycles per frame
// Frame rate:    3072000 / 50688 ≈ 60.61 Hz

pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 3_072_000,                            // 61.44 MHz / 5 / 4
    cycles_per_scanline: 192,                           // 384 pixels / 2
    total_scanlines: 264,                               // VTOTAL
    display_width: (NATIVE_HEIGHT - VBLANK_END) as u32, // 224 (rotated 90° CCW)
    display_height: NATIVE_WIDTH as u32,                // 256
    display_aspect: Some((3, 4)),
};

pub const VISIBLE_LINES: u64 = 240;
pub const OUTPUT_SAMPLE_RATE: u64 = 44_100;

// Sound CPU: I8035 @ 6 MHz / 15 = 400 kHz machine cycles
// Bresenham ratio: 400000 / 3072000 = 25 / 192
pub const SOUND_TICK_NUM: u32 = 25;
pub const SOUND_TICK_DEN: u32 = 192;

// Screen: 256×240 native, visible region Y: 16-239 (224 lines, VBEND=16).
// Rotated 90° CCW → 224×256 output.
pub const NATIVE_WIDTH: usize = 256;
pub const NATIVE_HEIGHT: usize = 240;
pub const VBLANK_END: usize = 16; // first visible scanline

// Resistor networks for palette PROM decoding (MB7052 TTL output PROMs).
// Signal chain: PROM → resistor DAC → Darlington/emitter amp → SANYO EZV20 monitor.
// Darlington amplifier (R and G): 1kΩ/470Ω/220Ω DAC with 470Ω pullup bias to VCC.
// Emitter follower (B): 470Ω/220Ω DAC with 680Ω pullup bias to VCC.
// These are `pub(crate)` because Mario Bros. (machines/src/mario_bros.rs) shares
// the same Nintendo monitor/amplifier model (Sanyo EZV20, Darlington/emitter
// stages) — only its PROM bit layout differs.
pub(crate) const DARLINGTON_RESISTORS: [f64; 3] = [1000.0, 470.0, 220.0];
pub(crate) const DARLINGTON_BIAS_R: f64 = 470.0;
pub(crate) const EMITTER_RESISTORS: [f64; 2] = [470.0, 220.0];
pub(crate) const EMITTER_BIAS_R: f64 = 680.0;

/// Compute a single color channel using the TKG-04 hardware signal chain.
///
/// Models the physical circuit: MB7052 PROM with TTL output levels drives a
/// resistor DAC network with a VCC pullup bias resistor.  The DAC output feeds
/// a Darlington or emitter-follower amplifier stage, then an inverting SANYO
/// EZV20 monitor input circuit with ≈0.7 V diode drops.
///
/// `raw_bits` contains non-inverted PROM bit values (0.0 = TTL low/active,
/// 1.0 = TTL high/inactive).  The function returns a raw floating-point
/// intensity (not yet clamped to 0–255) suitable for palette normalization.
pub(crate) fn compute_tkg04_channel(
    raw_bits: &[f64],
    resistors: &[f64],
    bias_r: f64,
    is_darlington: bool,
) -> f64 {
    const VCC: f64 = 5.0;
    const V_BIAS: f64 = 5.0;
    const V_OL: f64 = 0.05; // TTL low output voltage
    const V_OH: f64 = 4.0; // TTL high output voltage
    const TTL_H_RES: f64 = 50.0; // TTL high-state output impedance (Ω)

    let mut r_total: f64 = 0.0;
    let mut v: f64 = 0.0;

    // First pass: low inputs (raw bit = 0, PROM output driving to vOL)
    for (&bit, &r) in raw_bits.iter().zip(resistors) {
        if r != 0.0 && bit == 0.0 {
            r_total += 1.0 / r;
            v += V_OL / r;
        }
    }

    // Bias pullup to VCC
    r_total += 1.0 / bias_r;
    v += V_BIAS / bias_r;

    // Second pass: high inputs (raw bit = 1, TTL high through R + output impedance)
    for (&bit, &r) in raw_bits.iter().zip(resistors) {
        if r != 0.0 && bit != 0.0 {
            let r_eff = r + TTL_H_RES;
            r_total += 1.0 / r_eff;
            v += V_OH / r_eff;
        }
    }

    // Node voltage (Thévenin equivalent)
    let v_node = v / r_total;

    // Amplifier stage
    let v_amp = if is_darlington {
        v_node.max(0.7) // Darlington: minimum output ≈ 0.7 V
    } else {
        (v_node - 0.7).max(0.0) // Emitter follower: base-emitter drop ≈ 0.7 V
    };

    // SANYO EZV20 monitor: inverting circuit with diode clipping
    let v_inv = VCC - v_amp;
    let v_clip = (v_inv - 0.7).clamp(0.0, VCC - 1.4);
    v_clip / (VCC - 1.4) * 255.0
}

/// Compute the 256-entry RGB palette from the two 256-byte color PROMs.
///
/// `palette_prom` must be at least 512 bytes: the c-2k/c-2e PROM at `[0..256]`
/// and c-2j/c-2f at `[256..512]`. Uses the MAME-compatible resistor-network
/// model (TTL levels, Darlington/emitter amps, SANYO EZV20 monitor inversion),
/// with the color-decoder NOR forcing pens `& 0x03 == 0` to black, then
/// normalizes so the brightest component reaches 255.
///
/// Shared by [`Tkg04Board::build_palette`] and each machine's gfxview
/// `GfxRegion` palette hook so the runtime and offline paths never diverge.
pub(crate) fn compute_tkg04_palette(palette_prom: &[u8]) -> [(u8, u8, u8); 256] {
    let mut raw: [(f64, f64, f64); 256] = [(0.0, 0.0, 0.0); 256];

    for (i, entry) in raw.iter_mut().enumerate() {
        // Tri-state: NOR on color decoder forces output black
        if (i & 0x03) == 0x00 {
            continue;
        }

        // Raw (non-inverted) PROM bytes — inversion is handled by the
        // TTL output model inside compute_tkg04_channel.
        let c2k = palette_prom[i]; // first PROM (c-2k / c-2e)
        let c2j = palette_prom[0x100 + i]; // second PROM (c-2j / c-2f)

        // Red: 3 bits from c-2j (bits 1-3), Darlington amp
        let r_bits = [
            ((c2j >> 1) & 1) as f64,
            ((c2j >> 2) & 1) as f64,
            ((c2j >> 3) & 1) as f64,
        ];
        let r = compute_tkg04_channel(&r_bits, &DARLINGTON_RESISTORS, DARLINGTON_BIAS_R, true);

        // Green: c-2k bits 2-3 + c-2j bit 0, Darlington amp
        let g_bits = [
            ((c2k >> 2) & 1) as f64,
            ((c2k >> 3) & 1) as f64,
            (c2j & 1) as f64,
        ];
        let g = compute_tkg04_channel(&g_bits, &DARLINGTON_RESISTORS, DARLINGTON_BIAS_R, true);

        // Blue: 2 bits from c-2k (bits 0-1), emitter follower
        let b_bits = [(c2k & 1) as f64, ((c2k >> 1) & 1) as f64];
        let b = compute_tkg04_channel(&b_bits, &EMITTER_RESISTORS, EMITTER_BIAS_R, false);

        *entry = (r, g, b);
    }

    // Normalize palette range so maximum component reaches 255
    // (matches MAME's palette.normalize_range)
    let max_val = raw
        .iter()
        .flat_map(|&(r, g, b)| [r, g, b])
        .fold(0.0f64, f64::max);
    let scale = if max_val > 0.0 { 255.0 / max_val } else { 1.0 };

    let mut out = [(0u8, 0u8, 0u8); 256];
    for (o, &(r, g, b)) in out.iter_mut().zip(raw.iter()) {
        *o = (
            (r * scale).round().min(255.0) as u8,
            (g * scale).round().min(255.0) as u8,
            (b * scale).round().min(255.0) as u8,
        );
    }
    out
}

// ---------------------------------------------------------------------------
// Tkg04Board — shared Nintendo TKG/TRS arcade hardware
// ---------------------------------------------------------------------------

/// Shared hardware for the Nintendo TKG-04 arcade platform.
///
/// Named after the Nintendo PCB designation "TKG-04", the final 2-board
/// Donkey Kong design. The same core hardware (with minor variations) is
/// used by Donkey Kong (TKG-04), Donkey Kong Jr, and Radar Scope (TRS-02).
/// Earlier 4-board sets (TKG-02, TKG-03) are electrically equivalent.
///
/// Hardware: Z80 @ 3.072 MHz (main), I8035 @ 6 MHz (sound).
/// Video: 32×32 tile playfield + 16×16 sprites, 2bpp, PROM palette.
/// Audio: I8035 DAC + discrete circuits (walk, jump, stomp effects).
/// Screen: 256×240 displayed rotated 90° CCW on vertical monitor.
#[derive(BusDebug, DebugTrace)]
pub struct Tkg04Board {
    // CPUs (debug reads/writes auto-routed through matching #[debug_map])
    #[debug_cpu("Z80 Main")]
    pub(crate) cpu: Z80,
    #[debug_cpu("I8035 Sound")]
    pub(crate) sound_cpu: I8035,

    // Memory maps (page-table dispatch + watchpoints + backing memory)
    // CPU-addressable RAM/ROM storage lives in the AddressSpace16 backing store.
    #[debug_map(cpu = 0)]
    pub(crate) main_map: AddressSpace16,
    #[debug_map(cpu = 1)]
    pub(crate) sound_map: AddressSpace16,
    pub(crate) tune_rom: [u8; 0x0800], // 2KB (DK only, unused by DK Jr)

    // GFX ROMs
    pub(crate) tile_rom: [u8; 0x2000], // 8KB max (DK=4KB, DK Jr=8KB)
    pub(crate) sprite_rom: [u8; 0x2000], // 8KB

    // PROMs
    pub(crate) palette_prom: [u8; 0x0200], // c-2k/c-2e + c-2j/c-2f
    pub(crate) color_prom: [u8; 0x0100],   // v-5e/v-2n

    // Pre-computed palette (256 RGB entries)
    pub(crate) palette_rgb: [(u8, u8, u8); 256],

    // Scanline-rendered framebuffer (256 × 240 × RGB24)
    pub(crate) scanline_buffer: Vec<u8>,

    // I/O state (active-high: 0x00 = all released)
    pub(crate) in0: u8,
    pub(crate) in1: u8,
    pub(crate) in2: u8,
    pub(crate) dsw0: u8,

    // Control registers
    pub(crate) sound_latch: u8,
    pub(crate) sound_control_latch: OutputLatch,
    pub(crate) flip_screen: bool,
    pub(crate) sprite_bank: bool,
    pub(crate) nmi_mask: bool,
    pub(crate) palette_bank: u8,

    // DK Jr extras (always 0 for DK)
    pub(crate) gfx_bank: u8,
    pub(crate) sound_control_latch_4h: OutputLatch,

    // Pre-decoded GFX caches (from tile_rom / sprite_rom)
    pub(crate) tile_cache: gfx::GfxCache,
    pub(crate) sprite_cache: gfx::GfxCache,

    // Configuration (set at construction, not saved)
    tile_plane1_offset: usize, // 0x800 for DK (4KB tiles), 0x1000 for DK Jr (8KB)

    // DMA controller (i8257)
    #[debug_device("DMA")]
    pub(crate) dma: I8257,

    // Sound CPU interface
    pub(crate) sound_irq_pending: bool,

    // Audio output
    #[debug_device("DAC")]
    pub(crate) dac: Mc1408Dac,
    pub(crate) resampler: AudioResampler<i16>,

    // Timing
    pub(crate) clock: u64,
    pub(crate) sound_clock: ClockDivider,
    pub(crate) vblank_nmi_pending: bool,

    // Discrete sound: DAC stream + walk/jump/stomp effects, mixed in-circuit.
    #[debug_device("Discrete")]
    pub(crate) sound: DkongDiscreteSound,

    // Debug event ring (observer state — never saved in save states)
    #[debug_events]
    pub(crate) debug_trace: DebugTraceBuffer,
}

impl Tkg04Board {
    /// Create a new board with the given tile ROM plane-1 offset.
    ///
    /// - DK: `tile_plane1_offset = 0x800` (4KB tile ROM)
    /// - DK Jr: `tile_plane1_offset = 0x1000` (8KB tile ROM)
    pub fn new(tile_plane1_offset: usize) -> Self {
        Self {
            cpu: Z80::new(),
            sound_cpu: I8035::new(),
            main_map: Self::build_main_map(),
            sound_map: Self::build_sound_map(),
            tune_rom: [0; 0x0800],
            tile_rom: [0; 0x2000],
            sprite_rom: [0; 0x2000],
            palette_prom: [0; 0x0200],
            color_prom: [0; 0x0100],
            palette_rgb: [(0, 0, 0); 256],
            scanline_buffer: vec![0u8; NATIVE_WIDTH * NATIVE_HEIGHT * 3],
            in0: 0x00,
            in1: 0x00,
            in2: 0x00,
            dsw0: 0x80, // default: upright cabinet, 3 lives, 7000 bonus, 1 coin/1 play
            sound_latch: 0,
            sound_control_latch: OutputLatch::new(),
            flip_screen: false,
            sprite_bank: false,
            nmi_mask: false,
            palette_bank: 0,
            gfx_bank: 0,
            sound_control_latch_4h: OutputLatch::new(),
            tile_cache: gfx::GfxCache::new(0, 8, 8),
            sprite_cache: gfx::GfxCache::new(0, 16, 16),
            tile_plane1_offset,
            dma: I8257::new(),
            sound_irq_pending: false,
            dac: Mc1408Dac::new(),
            resampler: AudioResampler::new(TIMING.cpu_clock_hz, OUTPUT_SAMPLE_RATE),
            clock: 0,
            sound_clock: ClockDivider::new(SOUND_TICK_NUM, SOUND_TICK_DEN),
            vblank_nmi_pending: false,
            sound: DkongDiscreteSound::new(),
            debug_trace: DebugTraceBuffer::new(),
        }
    }

    fn build_main_map() -> AddressSpace16 {
        use MainRegion::*;
        let mut map = AddressSpace16::new();
        map.region(Rom, "Program ROM", 0x0000, 0x6000, AccessKind::ReadOnly)
            .region(Ram, "Work RAM", 0x6000, 0x1000, AccessKind::ReadWrite)
            .region(
                SpriteRam,
                "Sprite RAM",
                0x7000,
                0x0400,
                AccessKind::ReadWrite,
            )
            .region(VideoRam, "Video RAM", 0x7400, 0x0400, AccessKind::ReadWrite)
            .region(IoDma, "DMA", 0x7800, 0x100, AccessKind::Io)
            .region(IoPorts, "I/O Ports", 0x7C00, 0x200, AccessKind::Io);
        map
    }

    fn build_sound_map() -> AddressSpace16 {
        use SoundRegion::*;
        let mut map = AddressSpace16::new();
        map.region(Rom, "Sound ROM", 0x0000, 0x1000, AccessKind::ReadOnly);
        map
    }

    /// Pre-decode tile and sprite ROMs into GFX caches.
    /// Call after loading tile_rom and sprite_rom.
    pub fn decode_gfx_roms(&mut self) {
        // Tiles: separated-plane 2bpp, 8x8
        let tile_count = self.tile_plane1_offset / 8; // DK: 256, DK Jr: 512
        let plane1_bits = self.tile_plane1_offset * 8;
        let tile_planes: [usize; 2] = [0, plane1_bits];
        self.tile_cache = decode_gfx(
            &self.tile_rom,
            0,
            tile_count,
            &GfxLayout {
                plane_offsets: &tile_planes,
                x_offsets: &[0, 1, 2, 3, 4, 5, 6, 7],
                y_offsets: &[0, 8, 16, 24, 32, 40, 48, 56],
                char_increment: 64,
            },
        );

        // Sprites: 4-ROM interleaved 2bpp, 16x16
        let sprite_count = self.sprite_rom.len() / 4 / 16; // 128
        let q = self.sprite_rom.len() / 4;
        let q8 = q * 8;
        let sprite_planes: [usize; 2] = [0, 2 * q8];
        let x_offsets: [usize; 16] =
            std::array::from_fn(|px| if px < 8 { px } else { q8 + (px - 8) });
        let y_offsets: [usize; 16] = std::array::from_fn(|py| py * 8);
        self.sprite_cache = decode_gfx(
            &self.sprite_rom,
            0,
            sprite_count,
            &GfxLayout {
                plane_offsets: &sprite_planes,
                x_offsets: &x_offsets,
                y_offsets: &y_offsets,
                char_increment: 128,
            },
        );
    }

    // -----------------------------------------------------------------------
    // Palette
    // -----------------------------------------------------------------------

    /// Pre-compute the 256-entry RGB palette from the two color PROMs.
    ///
    /// Delegates to the shared [`compute_tkg04_palette`] so the runtime renderer
    /// and each machine's gfxview export apply identical resistor-net math.
    pub fn build_palette(&mut self) {
        self.palette_rgb = compute_tkg04_palette(&self.palette_prom);
    }

    /// Decoded tile/sprite sheets for the interactive GFX viewer (`--gfxview`).
    /// Shared by every TKG-04 game (DK, DK Jr); the caches carry each game's
    /// own decoded geometry.
    pub(crate) fn gfx_sheets(&self) -> Vec<GfxSheet<'_>> {
        vec![
            GfxSheet {
                name: "tiles",
                cache: &self.tile_cache,
                palette: &self.palette_rgb,
            },
            GfxSheet {
                name: "sprites",
                cache: &self.sprite_cache,
                palette: &self.palette_rgb,
            },
        ]
    }

    // -----------------------------------------------------------------------
    // Scanline rendering
    // -----------------------------------------------------------------------

    /// Render a single scanline from current VRAM/sprite state.
    pub fn render_scanline(&mut self, scanline: usize) {
        let row_offset = scanline * NATIVE_WIDTH * 3;

        // Split borrows: immutable refs for closures, mutable ref for buffer
        let video_ram = self.main_map.region_data(MainRegion::VideoRam);
        let color_prom = &self.color_prom;
        let palette_rgb = &self.palette_rgb;
        let tile_cache = &self.tile_cache;
        let sprite_cache = &self.sprite_cache;
        let gfx_bank = self.gfx_bank;
        let palette_bank = self.palette_bank;
        let buf = &mut self.scanline_buffer[row_offset..row_offset + NATIVE_WIDTH * 3];

        // Inline color resolution (captures split borrows, not &self)
        let resolve = |color: u8, pixel_value: u8| -> (u8, u8, u8) {
            let palette_index = (color as usize & 0x3F) * 4 + (pixel_value as usize & 0x03);
            palette_rgb[palette_index & 0xFF]
        };

        // --- Background tiles: 32×32 tilemap, 8×8 tiles ---
        let config = gfx::TilemapConfig {
            cols: 32,
            rows: 32,
            tile_width: 8,
            tile_height: 8,
        };

        gfx::tilemap::render_tilemap_scanline(
            &config,
            tile_cache,
            scanline,
            |col, row| {
                let vram_offset = row * 32 + col;
                let tile_code = video_ram[vram_offset] as u16 + 256 * gfx_bank as u16;
                let attribute = (color_prom[col + 32 * (row / 4)] & 0x0F) + 0x10 * palette_bank;
                gfx::TileInfo::new(tile_code, attribute)
            },
            // Background tilemap is opaque — every pixel writes.
            |attr, pv| Some(resolve(attr, pv)),
            buf,
            0,
        );

        // --- Sprites ---
        // Iterate forward: later sprites overwrite earlier ones.
        let sprite_ram = self.main_map.region_data(MainRegion::SpriteRam);
        let sprite_base = if self.sprite_bank { 0x200 } else { 0x000 };
        let mut offs = sprite_base;
        while offs < sprite_base + 0x200 {
            let y_byte = sprite_ram[offs];
            let code_byte = sprite_ram[offs + 1];
            let attr_byte = sprite_ram[offs + 2];
            let x_byte = sprite_ram[offs + 3];

            let test = y_byte.wrapping_add(0xF9).wrapping_add(scanline as u8);
            if (test & 0xF0) == 0xF0 {
                let row_in_sprite = test & 0x0F;

                let spr_code = (code_byte & 0x7F) as u16 | (((attr_byte & 0x40) as u16) << 1);
                let flip_y = (code_byte & 0x80) != 0;
                let flip_x = (attr_byte & 0x80) != 0;
                let color_attr = (attr_byte & 0x0F) + 0x10 * palette_bank;

                let src_py = if flip_y {
                    15 - row_in_sprite
                } else {
                    row_in_sprite
                };

                let sprite_x = x_byte.wrapping_add(0xF8) as i32;

                let clip = gfx::sprite::SpriteClip {
                    x_min: 0,
                    x_max: NATIVE_WIDTH as i32,
                    wrap_offset: Some(-256), // X wraparound
                };
                gfx::sprite::draw_sprite_row(
                    sprite_cache,
                    spr_code,
                    src_py as usize,
                    sprite_x,
                    flip_x,
                    |pv| pv == 0,
                    |pv| resolve(color_attr, pv),
                    buf,
                    &clip,
                );
            }

            offs += 4;
        }
    }

    // -----------------------------------------------------------------------
    // Core tick
    // -----------------------------------------------------------------------

    /// Execute one CPU cycle at the Z80 clock rate (3.072 MHz).
    ///
    /// The `bus` parameter is the game wrapper (which implements `Bus`) passed
    /// in from the wrapper's `run_frame()` / `debug_tick()`.
    pub fn tick(&mut self, bus: &mut dyn Bus<Address = u16, Data = u8>) {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();

        // Per-scanline rendering at scanline boundary
        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let scanline = (frame_cycle / TIMING.cycles_per_scanline) as u16;
            if scanline < VISIBLE_LINES as u16 {
                self.render_scanline(scanline as usize);
            }
        }

        // VBLANK NMI: assert at scanline 240
        let vblank_cycle = VISIBLE_LINES * TIMING.cycles_per_scanline;
        if frame_cycle == vblank_cycle {
            self.vblank_nmi_pending = true;
            if self.debug_trace.enabled() {
                self.debug_trace.record(DebugEvent {
                    cpu_index: Some(0),
                    detail: Some(if self.nmi_mask {
                        "VBLANK NMI"
                    } else {
                        "VBLANK NMI (masked)"
                    }),
                    ..DebugEvent::new(
                        self.clock,
                        DebugAccessSource::Unknown,
                        DebugEventKind::InterruptAssert,
                    )
                });
            }
        }
        // Clear NMI at frame boundary (end of VBLANK)
        if frame_cycle == 0 && self.clock > 0 {
            self.vblank_nmi_pending = false;
        }

        // Latch debug attribution context (cycle + instruction PC) before
        // CPU execution — bus dispatch cannot read CPU state mid-tick.
        // (sound_map has no watchpoint hooks in bus dispatch yet.)
        if self.main_map.has_any_watchpoints() || self.debug_trace.enabled() {
            let pc = self
                .cpu
                .at_instruction_boundary()
                .then_some(self.cpu.pc as u32);
            self.main_map.latch_access_context(self.clock, pc);
        }

        // Execute main CPU cycle
        self.cpu.execute_cycle(bus, BusMaster::Cpu(0));

        // Tick sound CPU (Bresenham 25/192 ratio: 400 kHz from 3.072 MHz)
        if self.sound_clock.tick() {
            self.sound_cpu.execute_cycle(bus, BusMaster::Cpu(1));
        }

        // Box-filter the DAC (3.072 MHz → 44.1 kHz); each produced sample drives
        // one step of the discrete circuit, which sums it with the effects.
        if let Some(dac_avg) = self.resampler.tick_sample(self.dac.sample_i16()) {
            self.sound.feed_dac(dac_avg);
        }

        self.clock += 1;
    }

    // -----------------------------------------------------------------------
    // Frame rendering (rotation)
    // -----------------------------------------------------------------------

    /// Rotate 90° CCW from native scanline_buffer (256w × 240h)
    /// to output buffer (224w × 256h), clipping VBLANK (scanlines 0-15).
    pub fn render_frame(&self, buffer: &mut [u8]) {
        gfx::rotate_90_ccw(
            &self.scanline_buffer[VBLANK_END * NATIVE_WIDTH * 3..],
            buffer,
            NATIVE_WIDTH,
            NATIVE_HEIGHT - VBLANK_END,
        );
    }

    // -----------------------------------------------------------------------
    // Audio
    // -----------------------------------------------------------------------

    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.sound.fill_audio(buffer)
    }

    // -----------------------------------------------------------------------
    // Reset
    // -----------------------------------------------------------------------

    /// Reset board state (does not reset CPUs — the wrapper must do that
    /// using its own unsafe borrow-split, since Bus is on the wrapper).
    pub fn reset(&mut self) {
        self.nmi_mask = false;
        self.vblank_nmi_pending = false;
        self.sound_irq_pending = false;
        self.sound_latch = 0;
        self.sound_control_latch.reset();
        self.sound_control_latch_4h.reset();
        self.flip_screen = false;
        self.sprite_bank = false;
        self.palette_bank = 0;
        self.gfx_bank = 0;
        self.dma.reset();

        self.clock = 0;
        self.sound_clock.reset();
        self.resampler.reset();
        self.dac.reset();
        self.sound.reset();

        self.in0 = 0x00;
        self.in1 = 0x00;
        self.in2 = 0x00;

        self.main_map.region_data_mut(MainRegion::VideoRam).fill(0);
        self.main_map.region_data_mut(MainRegion::Ram).fill(0);
        self.main_map.region_data_mut(MainRegion::SpriteRam).fill(0);
        self.scanline_buffer.fill(0);
    }

    // -----------------------------------------------------------------------
    // Shared I/O helpers
    // -----------------------------------------------------------------------

    /// Trigger sprite DMA transfer from i8257 channel 0.
    pub fn trigger_sprite_dma(&mut self) {
        let src_addr = self.dma.channel_address(0);
        let sprite_len = self.main_map.region_data(MainRegion::SpriteRam).len();
        let count = ((self.dma.channel_count(0) & 0x3FFF) + 1).min(sprite_len as u16);
        if self.debug_trace.enabled() {
            self.debug_trace.record(DebugEvent {
                addr: Some(src_addr as u32),
                value: Some(count as u32),
                device: Some("DMA"),
                detail: Some("sprite DMA transfer (value = byte count)"),
                ..DebugEvent::new(self.clock, DebugAccessSource::Dma, DebugEventKind::DmaWrite)
            });
        }
        // Two-phase: read source bytes first, then bulk-write to sprite RAM
        let mut buf = [0u8; 0x0400];
        for i in 0..count {
            let addr = src_addr.wrapping_add(i);
            buf[i as usize] = self.main_map.debug_read(addr).unwrap_or(0);
        }
        let sprite_data = self.main_map.region_data_mut(MainRegion::SpriteRam);
        sprite_data[..count as usize].copy_from_slice(&buf[..count as usize]);
    }

    /// Write a single bit to the 74LS259 sound control latch (0x7D00-0x7D07).
    pub fn write_sound_control_bit(&mut self, bit: u8, value: bool) {
        self.sound_control_latch.write(bit, value);
        // Forward bits 0-2 to the discrete sound device (walk/jump/stomp).
        if bit < 3 {
            self.sound.write_sound_bit(bit, value);
        }
    }

    /// Record a main-bus write event from a game wrapper's `Bus::write`.
    /// Maps the shared TKG-04 I/O layout to event kinds; cheap no-op while
    /// tracing is disabled.
    pub(crate) fn trace_main_write(&mut self, addr: u16, data: u8) {
        if !self.debug_trace.enabled() {
            return;
        }
        let (kind, device, detail) = match self.main_map.page(addr).region_id {
            MainRegion::IO_DMA => (DebugEventKind::DeviceWrite, Some("DMA"), None),
            MainRegion::IO_PORTS => match addr {
                0x7C00 => (DebugEventKind::DeviceWrite, None, Some("sound latch")),
                0x7C80..=0x7C87 => (
                    DebugEventKind::DeviceWrite,
                    None,
                    Some("sound/gfx control latch"),
                ),
                0x7D00..=0x7D07 => (
                    DebugEventKind::DeviceWrite,
                    Some("Discrete"),
                    Some("sound control bit"),
                ),
                0x7D80 => (
                    if data != 0 {
                        DebugEventKind::InterruptAssert
                    } else {
                        DebugEventKind::InterruptClear
                    },
                    None,
                    Some("sound CPU IRQ"),
                ),
                0x7D84 => (DebugEventKind::DeviceWrite, None, Some("NMI mask")),
                // 0x7D85 sprite DMA trigger: the transfer itself is
                // recorded by trigger_sprite_dma.
                0x7D85 => return,
                _ => (DebugEventKind::IoWrite, None, None),
            },
            _ => (DebugEventKind::MemoryWrite, None, None),
        };
        self.debug_trace.record(DebugEvent {
            cpu_index: Some(0),
            pc: self.main_map.latched_pc(),
            addr: Some(addr as u32),
            value: Some(data as u32),
            width: 1,
            region: self.main_map.region_at(addr).map(|r| r.name),
            device,
            detail,
            ..DebugEvent::new(self.clock, DebugAccessSource::Cpu(0), kind)
        });
    }

    /// Record a main-bus I/O read event from a game wrapper's `Bus::read`.
    /// Only I/O regions are traced — memory reads (instruction fetches)
    /// would drown the ring.
    pub(crate) fn trace_main_read(&mut self, addr: u16, data: u8) {
        if !self.debug_trace.enabled() {
            return;
        }
        if !matches!(
            self.main_map.page(addr).region_id,
            MainRegion::IO_DMA | MainRegion::IO_PORTS
        ) {
            return;
        }
        self.debug_trace.record(DebugEvent {
            cpu_index: Some(0),
            pc: self.main_map.latched_pc(),
            addr: Some(addr as u32),
            value: Some(data as u32),
            width: 1,
            region: self.main_map.region_at(addr).map(|r| r.name),
            ..DebugEvent::new(
                self.clock,
                DebugAccessSource::Cpu(0),
                DebugEventKind::DeviceRead,
            )
        });
    }

    // -----------------------------------------------------------------------
    // Debug
    // -----------------------------------------------------------------------

    /// Return instruction-boundary bitmask for debugger.
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

impl Saveable for Tkg04Board {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        self.sound_cpu.save_state(w);
        w.write_bytes(self.main_map.region_data(MainRegion::Ram));
        w.write_bytes(self.main_map.region_data(MainRegion::SpriteRam));
        w.write_bytes(self.main_map.region_data(MainRegion::VideoRam));
        w.write_u8(self.in0);
        w.write_u8(self.in1);
        w.write_u8(self.in2);
        w.write_u8(self.sound_latch);
        self.sound_control_latch.save_state(w);
        w.write_bool(self.flip_screen);
        w.write_bool(self.sprite_bank);
        w.write_bool(self.nmi_mask);
        w.write_u8(self.palette_bank);
        w.write_u8(self.gfx_bank);
        self.sound_control_latch_4h.save_state(w);
        self.dma.save_state(w);
        self.dac.save_state(w);
        self.sound.save_state(w);
        w.write_bool(self.sound_irq_pending);
        self.resampler.save_state(w);
        w.write_u64_le(self.clock);
        self.sound_clock.save_state(w);
        w.write_bool(self.vblank_nmi_pending);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        self.sound_cpu.load_state(r)?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::Ram))?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::SpriteRam))?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::VideoRam))?;
        self.in0 = r.read_u8()?;
        self.in1 = r.read_u8()?;
        self.in2 = r.read_u8()?;
        self.sound_latch = r.read_u8()?;
        self.sound_control_latch.load_state(r)?;
        self.flip_screen = r.read_bool()?;
        self.sprite_bank = r.read_bool()?;
        self.nmi_mask = r.read_bool()?;
        self.palette_bank = r.read_u8()?;
        self.gfx_bank = r.read_u8()?;
        self.sound_control_latch_4h.load_state(r)?;
        self.dma.load_state(r)?;
        self.dac.load_state(r)?;
        self.sound.load_state(r)?;
        self.sound_irq_pending = r.read_bool()?;
        self.resampler.load_state(r)?;
        self.clock = r.read_u64_le()?;
        self.sound_clock.load_state(r)?;
        self.vblank_nmi_pending = r.read_bool()?;
        Ok(())
    }
}

impl Tkg04Board {
    /// Check interrupt state for the given bus master.
    /// Main CPU: VBlank NMI (edge-triggered, gated by nmi_mask).
    /// Sound CPU: IRQ from main CPU.
    pub fn check_interrupts(&self, target: BusMaster) -> InterruptState {
        match target {
            BusMaster::Cpu(0) => InterruptState {
                nmi: self.vblank_nmi_pending && self.nmi_mask,
                irq: false,
                firq: false,
                ..Default::default()
            },
            BusMaster::Cpu(1) => InterruptState {
                nmi: false,
                irq: self.sound_irq_pending,
                firq: false,
                ..Default::default()
            },
            _ => InterruptState::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    mod debug_events {
        use super::*;

        /// DK tile ROM layout — the offset doesn't matter for these tests.
        fn board() -> Tkg04Board {
            Tkg04Board::new(0x800)
        }

        #[test]
        fn tracing_disabled_records_nothing() {
            let mut b = board();
            b.trace_main_write(0x7C00, 0x05);
            b.trigger_sprite_dma();
            assert!(b.debug_trace.is_empty());
        }

        #[test]
        fn sound_latch_and_irq_writes_emit_annotated_events() {
            let mut b = board();
            b.debug_trace.set_enabled(true);
            b.clock = 555;

            b.trace_main_write(0x7C00, 0x05); // sound latch
            b.trace_main_write(0x7D80, 0x01); // sound CPU IRQ assert
            b.trace_main_write(0x7D80, 0x00); // sound CPU IRQ clear

            let events = b.debug_trace.events();
            assert_eq!(events[0].kind, DebugEventKind::DeviceWrite);
            assert_eq!(events[0].detail, Some("sound latch"));
            assert_eq!(events[0].cycle, 555);
            assert_eq!(events[1].kind, DebugEventKind::InterruptAssert);
            assert_eq!(events[1].detail, Some("sound CPU IRQ"));
            assert_eq!(events[2].kind, DebugEventKind::InterruptClear);
        }

        #[test]
        fn sprite_dma_emits_dma_event_with_count() {
            let mut b = board();
            b.debug_trace.set_enabled(true);

            // Program i8257 channel 0: source 0x6900, count 0x17F
            b.dma.write(0, 0x00);
            b.dma.write(0, 0x69);
            b.dma.write(1, 0x7F);
            b.dma.write(1, 0x01);
            b.debug_trace.clear(); // drop the register-write noise (none traced here anyway)

            b.trigger_sprite_dma();

            let events = b.debug_trace.events();
            assert_eq!(events.len(), 1);
            let e = &events[0];
            assert_eq!(e.kind, DebugEventKind::DmaWrite);
            assert_eq!(e.source, DebugAccessSource::Dma);
            assert_eq!(e.addr, Some(0x6900));
            assert_eq!(e.value, Some(0x180), "count = (0x17F & 0x3FFF) + 1");
            assert_eq!(e.device, Some("DMA"));
        }

        #[test]
        fn memory_and_io_writes_map_to_kinds() {
            let mut b = board();
            b.debug_trace.set_enabled(true);

            b.trace_main_write(0x6100, 0x42); // work RAM
            b.trace_main_write(0x7801, 0x10); // i8257 register
            b.trace_main_write(0x7D85, 0x01); // sprite DMA trigger: not traced here

            let events = b.debug_trace.events();
            assert_eq!(events.len(), 2);
            assert_eq!(events[0].kind, DebugEventKind::MemoryWrite);
            assert_eq!(events[0].region, Some("Work RAM"));
            assert_eq!(events[1].kind, DebugEventKind::DeviceWrite);
            assert_eq!(events[1].device, Some("DMA"));
        }

        #[test]
        fn io_reads_traced_memory_reads_not() {
            let mut b = board();
            b.debug_trace.set_enabled(true);

            b.trace_main_read(0x7C00, 0x12); // input port: traced
            b.trace_main_read(0x6100, 0x34); // work RAM: not traced

            let events = b.debug_trace.events();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].kind, DebugEventKind::DeviceRead);
        }
    }
}
