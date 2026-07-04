//! Gottlieb System 80 (GG-III) shared arcade hardware.
//!
//! Self-contained board struct supporting the ~17 games built on Gottlieb's
//! System 80 platform (Reactor, Q*Bert, Mad Planets, Krull, etc.).
//!
//! # Hardware
//!
//! - **Main CPU**: Intel 8088 @ 5 MHz (15 MHz XTAL / 3)
//! - **Sound CPU**: MOS 6502 @ 894,886 Hz (3.579545 MHz XTAL / 4)
//! - **Screen**: 256×240 visible, 318×256 total, ~61.42 Hz, 5 MHz pixel clock
//! - **Video**: 32×32 tilemap (8×8, 4bpp packed) + 64 sprites (16×16, 4bpp planar)
//! - **Palette**: 16 colors × 2 bytes = 32 bytes palette RAM (4-bit RGB)
//! - **Sound**: MC1408 DAC + Votrax SC-01A speech synthesizer
//! - **I/O**: MOS 6532 RIOT (128B RAM, 2 ports, timer, edge detect)
//! - **NMI**: VBLANK → main CPU NMI; RIOT IRQ → sound CPU IRQ; Votrax A/R → sound CPU NMI

use phosphor_core::audio::AudioResampler;
use phosphor_core::bus_split;
use phosphor_core::core::debug::{DebugRegister, Debuggable};
use phosphor_core::core::machine::ProfileSpan;
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, ClockDivider, InterruptState, TimingConfig};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::i8088::I8088;
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::device::{Device, Mc1408Dac, Riot6532, VotraxSc01};
use phosphor_core::gfx;
use phosphor_core::gfx::decode::{GfxLayout, decode_gfx, decode_gfx_element};

use phosphor_macros::{BusDebug, MemoryRegion, Saveable};

// ---------------------------------------------------------------------------
// Memory map regions
// ---------------------------------------------------------------------------

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum Region {
    Nvram = 1,
    Ram = 2,
    SpriteRam = 3,
    VideoRam = 4,
    CharRam = 5,
    ProgramRom = 6,
}

// ---------------------------------------------------------------------------
// Timing constants
// ---------------------------------------------------------------------------

// Master pixel clock: 20 MHz / 4 = 5 MHz (= CPU clock)
// HTOTAL: 318 pixel clocks per scanline
// VTOTAL: 256 lines per field
// Visible: 256×240 (HBSTART=256, VBSTART=240)
// Frame: 318 × 256 = 81,408 CPU cycles per field

pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 5_000_000,
    cycles_per_scanline: 318,
    total_scanlines: 256,
    display_width: NATIVE_HEIGHT as u32, // 240 (rotated 270° CW for Q*Bert)
    display_height: NATIVE_WIDTH as u32, // 256
    display_aspect: None,
};

pub const VISIBLE_LINES: u64 = 240;
pub const OUTPUT_SAMPLE_RATE: u64 = 44_100;

pub const NATIVE_WIDTH: usize = 256;
pub const NATIVE_HEIGHT: usize = 240;

// Tilemap dimensions (32×32 grid, only 32×30 visible)
const TILE_COLS: usize = 32;
const TILE_ROWS: usize = 30;

// GfxLayout for Gottlieb 8×8 4bpp packed tiles (also used for charram re-decode)
const GOTTLIEB_TILE_LAYOUT: GfxLayout<'static> = GfxLayout {
    plane_offsets: &[3, 2, 1, 0],
    x_offsets: &[0, 4, 8, 12, 16, 20, 24, 28],
    y_offsets: &[0, 32, 64, 96, 128, 160, 192, 224],
    char_increment: 256,
};

// Sound CPU ratio: 894,886 / 5,000,000 ≈ 179/1000
const SOUND_CLOCK_NUM: u32 = 179;
const SOUND_CLOCK_DEN: u32 = 1000;

// Sound CPU clock (for audio resampler)
const SOUND_CLOCK_HZ: u64 = 894_886;

// I8088 main CPU clock; the Votrax tick divider is derived against this.
const I8088_CLOCK_HZ: u64 = 5_000_000;

// Votrax SC-01 VCO frequency at the speech-clock DAC center (data = 0xA0).
// The VCO is driven by the speech-clock DAC at 0x3000, so the actual clock is
// set at runtime via `convert_speech_clock`; this is only the power-on default.
const VOTRAX_NOMINAL_CLOCK_HZ: u64 = 950_000;

/// Convert a speech-clock DAC value to the Votrax SC-01 VCO frequency (Hz).
///
/// Matches MAME's `gottlieb_sound_speech_r1_device::convert_speech_clock`:
/// 950 kHz nominal at the DAC center (0xA0), ±5.5 kHz per step.
fn convert_speech_clock(data: u8) -> u64 {
    (950_000 + (data as i32 - 0xA0) * 5_500).max(1) as u64
}

/// 4-bit resistor-weighted DAC lookup table.
///
/// Gottlieb palette DAC uses resistors {2000, 1000, 470, 240}Ω with a 180Ω
/// pulldown. Values computed from MAME's `compute_resistor_weights` formula:
/// for each bit i, weight = maxval × R0 / (R[i] + R0) where R0 is the
/// parallel resistance of the pulldown and all other resistors to ground.
/// Weights are auto-scaled so that all-bits-on = 255.
const RESISTOR_DAC: [u8; 16] = [
    0, 16, 33, 49, 70, 86, 102, 119, 136, 153, 169, 185, 206, 222, 239, 255,
];

// ---------------------------------------------------------------------------
// Gottlieb Sound Board (Rev 1)
// ---------------------------------------------------------------------------

/// Self-contained sound board with M6502, RIOT, DAC, and Votrax SC-01.
///
/// The main board sends sound commands by writing to the RIOT's Port A
/// through [`write_sound_command`]. The RIOT PA7 edge triggers an IRQ
/// to wake the M6502, which reads the command and drives the DAC.
///
/// The Votrax SC-01A speech synthesizer is mapped at 0x2000 in the
/// sound CPU address space. Its A/R (articulate/request) output is
/// wired to RIOT Port B bit 7. A/R rising edge triggers sound CPU NMI.
#[derive(Saveable)]
#[save_version(3)]
pub(crate) struct GottliebSoundBoard {
    cpu: M6502,
    riot: Riot6532,
    dac: Mc1408Dac,
    votrax: VotraxSc01,
    resampler: AudioResampler<i16>,
    #[save_skip]
    sound_rom: Vec<u8>, // 8KB (mapped at 0x6000-0x7FFF in 15-bit space)
    clock: u64,
    /// Previous A/R state for edge detection (NMI on rising edge).
    votrax_ar_prev: bool,
    /// NMI pending from Votrax A/R rising edge.
    votrax_nmi: bool,
    /// Votrax VCO frequency (Hz) last requested by the speech-clock DAC.
    /// The board reads this to retune both the Votrax tick rate and the
    /// device's internal sample/capacitor clocks.
    speech_clock_hz: u64,
}

impl GottliebSoundBoard {
    fn new() -> Self {
        Self {
            cpu: M6502::new(),
            riot: Riot6532::new(),
            dac: Mc1408Dac::new(),
            votrax: VotraxSc01::new(VOTRAX_NOMINAL_CLOCK_HZ),
            resampler: AudioResampler::new(SOUND_CLOCK_HZ, OUTPUT_SAMPLE_RATE),
            sound_rom: vec![0xFF; 0x2000],
            clock: 0,
            votrax_ar_prev: true,
            votrax_nmi: false,
            speech_clock_hz: VOTRAX_NOMINAL_CLOCK_HZ,
        }
    }

    /// Apply a new Votrax VCO frequency to the speech device.
    fn set_votrax_clock(&mut self, clock_hz: u64) {
        self.votrax.set_clock(clock_hz);
    }

    /// Load sound ROM data (up to 8KB, mapped at 0x6000-0x7FFF).
    fn load_rom(&mut self, data: &[u8]) {
        let len = data.len().min(self.sound_rom.len());
        self.sound_rom[..len].copy_from_slice(&data[..len]);
    }

    /// Load the Votrax SC-01 internal phoneme ROM (512 bytes).
    fn load_votrax_rom(&mut self, data: &[u8]) {
        self.votrax.load_rom(data);
    }

    /// Send a sound command from the main CPU.
    ///
    /// Inverts bits 0-5, computes PA7 = NAND(bits 0-3), and writes to
    /// the RIOT's Port A with mask 0xBF (bits 0-5 and 7, leaving bit 6).
    fn write_sound_command(&mut self, data: u8) {
        let pa0_5 = !data & 0x3F;
        let pa7 = u8::from((data & 0x0F) != 0x0F);
        self.riot.set_pa_input_masked(pa0_5 | (pa7 << 7), 0xBF);
    }

    /// Advance the sound board by one sound CPU tick.
    fn tick(&mut self) {
        // Update RIOT PB7 with Votrax A/R signal before CPU reads it.
        // PB7 = A/R (1=ready, 0=busy). Other PB bits are directly driven.
        let ar = self.votrax.ar_output();
        self.riot
            .set_pb_input_masked(if ar { 0x80 } else { 0x00 }, 0x80);

        // Detect A/R rising edge → trigger NMI (active-low, inverted on board)
        if ar && !self.votrax_ar_prev {
            self.votrax_nmi = true;
        }
        self.votrax_ar_prev = ar;

        // Execute one M6502 cycle
        bus_split!(self, bus => {
            self.cpu.execute_cycle(bus, BusMaster::Cpu(1));
        });

        // Tick RIOT timer (clocked at same rate as M6502)
        self.riot.tick();

        // Audio: sample DAC and resample to output rate
        let sample = self.dac.sample_i16();
        self.resampler.tick(sample);

        self.clock += 1;
    }

    /// Advance the Votrax SC-01 by one Votrax clock tick (720 kHz).
    fn tick_votrax(&mut self) {
        self.votrax.tick();
    }

    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        // Fill buffer with DAC audio
        let count = self.resampler.fill_audio(buffer);

        // Mix in Votrax speech audio (f32 → i16, additive)
        let speech = self.votrax.drain_audio();
        let mix_len = count.max(speech.len()).min(buffer.len());
        for i in 0..mix_len {
            let dac_sample = if i < count { buffer[i] as i32 } else { 0 };
            let speech_sample = if i < speech.len() {
                (speech[i] * 32000.0) as i32
            } else {
                0
            };
            buffer[i] = (dac_sample + speech_sample).clamp(-32768, 32767) as i16;
        }
        mix_len
    }

    fn reset(&mut self) {
        bus_split!(self, bus => {
            self.cpu.reset(bus, BusMaster::Cpu(1));
        });
        self.riot.reset();
        self.dac.reset();
        self.votrax.reset();
        self.resampler.reset();
        self.clock = 0;
        self.votrax_ar_prev = true; // A/R starts high after reset
        self.votrax_nmi = false;
    }
}

// ---------------------------------------------------------------------------
// Sound board Bus impl (M6502 memory map, 15-bit address space)
// ---------------------------------------------------------------------------

impl Bus for GottliebSoundBoard {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        let addr = addr & 0x7FFF;
        match addr {
            // RIOT: 0x0000-0x0FFF (mirrored). A9 selects RAM vs I/O registers.
            0x0000..=0x0FFF => {
                if addr & 0x200 != 0 {
                    self.riot.read_io((addr & 0x1F) as u8)
                } else {
                    self.riot.read_ram((addr & 0x7F) as u8)
                }
            }

            // Sound ROM: 0x6000-0x7FFF
            0x6000..=0x7FFF => self.sound_rom[(addr - 0x6000) as usize],

            _ => 0xFF,
        }
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        let addr = addr & 0x7FFF;
        match addr {
            // RIOT: 0x0000-0x0FFF
            0x0000..=0x0FFF => {
                if addr & 0x200 != 0 {
                    self.riot.write_io((addr & 0x1F) as u8, data);
                } else {
                    self.riot.write_ram((addr & 0x7F) as u8, data);
                }
            }

            // DAC write: 0x1000-0x1FFF
            0x1000..=0x1FFF => self.dac.write(data),

            // Votrax SC-01: 0x2000-0x2FFF
            // Bits 6-7: inflection, bits 0-5: phoneme code (active-low → invert)
            0x2000..=0x2FFF => {
                self.votrax.set_inflection(data >> 6);
                self.votrax.write_phoneme(!data);
            }

            // Speech clock DAC: 0x3000-0x3FFF retunes the Votrax VCO frequency.
            // The board picks this up on its next tick to retune the device.
            0x3000..=0x3FFF => self.speech_clock_hz = convert_speech_clock(data),

            _ => {}
        }
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        // NMI from Votrax A/R rising edge (edge-triggered, auto-clears)
        let nmi = self.votrax_nmi;
        self.votrax_nmi = false;
        InterruptState {
            irq: self.riot.irq_active(),
            nmi,
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Sound board Debuggable (for BusDebug derive on GottliebBoard)
// ---------------------------------------------------------------------------

// Save state support: derived via #[derive(Saveable)] on the struct.

impl phosphor_core::device::Device for GottliebSoundBoard {
    fn name(&self) -> &'static str {
        "Gottlieb Sound Rev 1"
    }

    fn reset(&mut self) {
        self.reset(); // Calls inherent method (shadowing)
    }
}

impl Debuggable for GottliebSoundBoard {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "SND_CLK",
                value: self.clock,
                width: 32,
            },
            DebugRegister {
                name: "DAC",
                value: self.dac.debug_registers()[0].value,
                width: 8,
            },
            DebugRegister {
                name: "RIOT_IRQ",
                value: u64::from(self.riot.irq_active()),
                width: 1,
            },
            DebugRegister {
                name: "VOTRAX_AR",
                value: u64::from(self.votrax.ar_output()),
                width: 1,
            },
        ]
    }
}

// ---------------------------------------------------------------------------
// GottliebBoard — shared Gottlieb System 80 arcade hardware
// ---------------------------------------------------------------------------

/// Shared hardware for the Gottlieb System 80 (GG-III) platform.
///
/// Hardware: I8088 @ 5 MHz (main), M6502 @ 894 kHz (sound) with RIOT + DAC.
/// Video: 32×32 tilemap (8×8 tiles, 4bpp) + 64 sprites (16×16, 4bpp),
/// 16-color programmable palette, ROT270 display orientation.
#[derive(BusDebug)]
pub struct GottliebBoard {
    // Main CPU (I8088 @ 5 MHz)
    #[debug_cpu("I8088 Main")]
    pub(crate) cpu: I8088,

    // Sound board (M6502 + RIOT + DAC)
    #[debug_device("Sound Board")]
    pub(crate) sound: GottliebSoundBoard,

    // Memory
    #[debug_map(cpu = 0)]
    pub(crate) map: AddressSpace16,

    // GFX caches
    pub(crate) tile_rom_cache: gfx::GfxCache,
    pub(crate) charram_cache: gfx::GfxCache,
    pub(crate) sprite_cache: gfx::GfxCache,

    // Palette (16 entries, 4-bit RGB per channel)
    pub(crate) palette_ram: [u8; 32],
    pub(crate) palette_rgb: [(u8, u8, u8); 16],

    // Framebuffer (256×240 palette indices)
    pub(crate) pixel_buffer: Vec<u8>,

    // Video state
    pub(crate) video_control: u8,
    pub(crate) sprite_bank: u8,

    // Tile source selection (true = ROM, false = charram)
    pub(crate) gfxcharlo: bool, // codes 0x00-0x7F
    pub(crate) gfxcharhi: bool, // codes 0x80-0xFF

    // I/O ports (active-high for Q*Bert joystick/buttons)
    pub(crate) input_ports: [u8; 4], // IN1-IN4
    pub(crate) dsw: u8,

    // Timing
    pub(crate) clock: u64,
    pub(crate) sound_clock: ClockDivider,
    pub(crate) votrax_clock: ClockDivider,
    /// Votrax VCO frequency currently applied to `votrax_clock` and the
    /// speech device. Transient (not saved): reset to 0 on construction/load
    /// so the next tick re-derives the divider from `sound.speech_clock_hz`.
    pub(crate) votrax_clock_applied: u64,
    pub(crate) watchdog_counter: u16,

    // Profiling (not saved)
    pub(crate) profiling: bool,
    pub(crate) profile_spans: Vec<ProfileSpan>,
}

impl GottliebBoard {
    pub fn new() -> Self {
        Self {
            cpu: I8088::new(),
            sound: GottliebSoundBoard::new(),
            map: Self::build_map(),
            tile_rom_cache: gfx::GfxCache::new(0, 8, 8),
            charram_cache: gfx::GfxCache::new(128, 8, 8),
            sprite_cache: gfx::GfxCache::new(0, 16, 16),
            palette_ram: [0; 32],
            palette_rgb: [(0, 0, 0); 16],
            pixel_buffer: vec![0u8; NATIVE_WIDTH * NATIVE_HEIGHT],
            video_control: 0,
            sprite_bank: 0,
            gfxcharlo: false,
            gfxcharhi: false,
            input_ports: [0; 4],
            dsw: 0,
            clock: 0,
            sound_clock: ClockDivider::new(SOUND_CLOCK_NUM, SOUND_CLOCK_DEN),
            votrax_clock: ClockDivider::new(VOTRAX_NOMINAL_CLOCK_HZ as u32, I8088_CLOCK_HZ as u32),
            votrax_clock_applied: 0,
            watchdog_counter: 0,
            profiling: false,
            profile_spans: Vec::new(),
        }
    }

    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(
            Region::Nvram,
            "NVRAM",
            0x0000,
            0x1000,
            AccessKind::ReadWrite,
        )
        .region(Region::Ram, "RAM", 0x1000, 0x2000, AccessKind::ReadWrite)
        .region(
            Region::SpriteRam,
            "Sprite RAM",
            0x3000,
            0x0800,
            AccessKind::ReadWrite,
        )
        .region(
            Region::VideoRam,
            "Video RAM",
            0x3800,
            0x0800,
            AccessKind::ReadWrite,
        )
        .region(
            Region::CharRam,
            "Char RAM",
            0x4000,
            0x1000,
            AccessKind::ReadWrite,
        )
        .region(
            Region::ProgramRom,
            "Program ROM",
            0x6000,
            0xA000,
            AccessKind::ReadOnly,
        );
        map
    }

    /// Load program ROM data into the memory map.
    ///
    /// `data` is loaded at the END of the 0x6000-0xFFFF region, so
    /// a 24KB ROM occupies 0xA000-0xFFFF (offset 0x4000 in the region).
    pub fn load_program_rom(&mut self, data: &[u8]) {
        let region = self.map.region_data_mut(Region::ProgramRom);
        let start = region.len().saturating_sub(data.len());
        region[start..start + data.len()].copy_from_slice(data);
    }

    /// Load sound ROM data.
    pub fn load_sound_rom(&mut self, data: &[u8]) {
        self.sound.load_rom(data);
    }

    /// Load the Votrax SC-01 internal phoneme ROM (512 bytes).
    pub fn load_votrax_rom(&mut self, data: &[u8]) {
        self.sound.load_votrax_rom(data);
    }

    /// Pre-decode tile and sprite ROMs into GFX caches.
    pub fn decode_gfx(&mut self, tile_rom: &[u8], sprite_rom: &[u8]) {
        let tile_count = tile_rom.len() / 32;
        self.tile_rom_cache = decode_gfx(tile_rom, 0, tile_count, &GOTTLIEB_TILE_LAYOUT);

        // Sprites: 4bpp planar, 16x16, 4 equal ROM regions. MAME's spr_layout
        // planeoffset is { RGN_FRAC(0,4), 1/4, 2/4, 3/4 } and MAME is MSB-first
        // (planeoffset[0] = pen bit 3); decode_gfx is LSB-first (entry 0 = pen
        // bit 0), so reverse the list — bit 0 comes from the last ROM quarter.
        // (Same convention as GOTTLIEB_TILE_LAYOUT above; getting it wrong
        // bit-reverses every pen and scrambles MOB colors.)
        let sprite_count = sprite_rom.len() / 128;
        let quarter = sprite_rom.len() / 4;
        let planes: [usize; 4] = std::array::from_fn(|p| (3 - p) * quarter * 8);
        let y_offsets: [usize; 16] = std::array::from_fn(|py| py * 16);
        self.sprite_cache = decode_gfx(
            sprite_rom,
            0,
            sprite_count,
            &GfxLayout {
                plane_offsets: &planes,
                x_offsets: &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
                y_offsets: &y_offsets,
                char_increment: 256,
            },
        );
    }

    // -----------------------------------------------------------------------
    // I/O port handling (called by game wrapper's Bus impl)
    // -----------------------------------------------------------------------

    /// Read an I/O port (address bits 2:0).
    pub fn io_port_read(&self, port: u8) -> u8 {
        match port & 0x07 {
            0 => self.dsw,
            1 => self.input_ports[0],
            2 => self.input_ports[1],
            3 => self.input_ports[2],
            4 => self.input_ports[3],
            _ => 0xFF,
        }
    }

    /// Write an I/O port (address bits 2:0).
    pub fn io_port_write(&mut self, port: u8, data: u8) {
        match port & 0x07 {
            0 => self.watchdog_counter = 0,
            2 => self.sound.write_sound_command(data),
            3 => self.video_control = data,
            4 => self.sprite_bank = (data >> 2) & 3,
            _ => {}
        }
    }

    // -----------------------------------------------------------------------
    // Palette
    // -----------------------------------------------------------------------

    /// Update a palette entry from a palette RAM write.
    ///
    /// Even byte: G[7:4] B[3:0]. Odd byte: xxxx R[3:0].
    /// Uses resistor-weighted DAC (2000/1000/470/240Ω + 180Ω pulldown)
    /// matching MAME's `compute_resistor_weights` / `combine_weights`.
    pub fn update_palette(&mut self, offset: usize, data: u8) {
        let offset = offset & 0x1F;
        self.palette_ram[offset] = data;
        let entry = offset / 2;
        let even = self.palette_ram[entry * 2];
        let odd = self.palette_ram[entry * 2 + 1];
        let r = RESISTOR_DAC[(odd & 0x0F) as usize];
        let g = RESISTOR_DAC[(even >> 4) as usize];
        let b = RESISTOR_DAC[(even & 0x0F) as usize];
        self.palette_rgb[entry] = (r, g, b);
    }

    /// Rebuild the entire palette from palette_ram (after state load).
    fn rebuild_palette(&mut self) {
        for entry in 0..16 {
            let even = self.palette_ram[entry * 2];
            let odd = self.palette_ram[entry * 2 + 1];
            let r = RESISTOR_DAC[(odd & 0x0F) as usize];
            let g = RESISTOR_DAC[(even >> 4) as usize];
            let b = RESISTOR_DAC[(even & 0x0F) as usize];
            self.palette_rgb[entry] = (r, g, b);
        }
    }

    // -----------------------------------------------------------------------
    // Char RAM re-decode
    // -----------------------------------------------------------------------

    /// Re-decode a single charram tile after a write to character generator RAM.
    pub fn charram_write(&mut self, offset: usize, data: u8) {
        self.map.region_data_mut(Region::CharRam)[offset] = data;
        let tile_code = offset / 32;
        if tile_code < 128 {
            let charram = self.map.region_data(Region::CharRam);
            decode_gfx_element(
                charram,
                0,
                tile_code,
                &GOTTLIEB_TILE_LAYOUT,
                &mut self.charram_cache,
            );
        }
    }

    // -----------------------------------------------------------------------
    // Core tick
    // -----------------------------------------------------------------------

    /// Execute one CPU cycle at the I8088 clock rate (5 MHz).
    pub fn tick(&mut self, bus: &mut dyn Bus<Address = u32, Data = u8>) {
        // Latch watchpoint attribution context (cycle + instruction PC)
        // before CPU execution — bus dispatch cannot read CPU state mid-tick.
        // The I8088 debug surface uses IP as its PC (matching debug_pc).
        if self.map.has_any_watchpoints() {
            let pc = self
                .cpu
                .at_instruction_boundary()
                .then_some(self.cpu.ip as u32);
            self.map.latch_access_context(self.clock, pc);
        }

        // Execute main CPU cycle
        self.cpu.execute_cycle(bus, BusMaster::Cpu(0));

        // The speech-clock DAC (sound CPU 0x3000) retunes the Votrax VCO. When
        // it changes — or after a state load, where votrax_clock_applied is 0 —
        // re-derive the tick divider and the device's internal sample clock so
        // both phoneme rate and pitch track the requested frequency.
        let speech_hz = self.sound.speech_clock_hz;
        if speech_hz != self.votrax_clock_applied {
            self.votrax_clock_applied = speech_hz;
            self.votrax_clock
                .set_ratio(speech_hz as u32, I8088_CLOCK_HZ as u32);
            self.sound.set_votrax_clock(speech_hz);
        }

        // Tick sound board at fractional rate (~895 kHz)
        if self.sound_clock.tick() {
            self.sound.tick();
        }

        // Tick Votrax SC-01 at its VCO rate (nominally 950 kHz, DAC-tunable)
        if self.votrax_clock.tick() {
            self.sound.tick_votrax();
        }

        self.clock += 1;
        self.watchdog_counter = self.watchdog_counter.wrapping_add(1);
    }

    // -----------------------------------------------------------------------
    // Frame rendering
    // -----------------------------------------------------------------------

    /// Render the full frame into the indexed pixel buffer.
    pub fn render_frame_internal(&mut self) {
        let bg_priority = self.video_control & 0x01 != 0;

        // Clear to background (palette index 0)
        self.pixel_buffer.fill(0);

        if bg_priority {
            // Background priority: sprites behind tiles
            self.render_sprites();
            self.render_tiles();
        } else {
            // Normal: tiles behind sprites
            self.render_tiles();
            self.render_sprites();
        }
    }

    /// Render tiles from video RAM.
    fn render_tiles(&mut self) {
        let video_ram = self.map.region_data(Region::VideoRam);

        for tile_row in 0..TILE_ROWS {
            for tile_col in 0..TILE_COLS {
                let tile_index = tile_row * TILE_COLS + tile_col;
                let code = video_ram[tile_index & 0x3FF] as usize;

                // Select tile source: bit 7 selects gfxcharhi/gfxcharlo
                let use_rom = if code & 0x80 != 0 {
                    self.gfxcharhi
                } else {
                    self.gfxcharlo
                };
                let cache = if use_rom {
                    &self.tile_rom_cache
                } else {
                    &self.charram_cache
                };

                // ROM tiles use the full code; charram tiles use code & 0x7F
                let cache_code = if use_rom {
                    code % cache.count().max(1)
                } else {
                    (code & 0x7F) % cache.count().max(1)
                };

                let screen_x = tile_col * 8;
                let screen_y = tile_row * 8;

                for py in 0..8usize {
                    let sy = screen_y + py;
                    if sy >= NATIVE_HEIGHT {
                        break;
                    }
                    let row = cache.row_slice(cache_code, py);
                    let row_base = sy * NATIVE_WIDTH + screen_x;
                    for (px, &pixel) in row.iter().enumerate().take(8) {
                        if pixel != 0 {
                            self.pixel_buffer[row_base + px] = pixel;
                        }
                    }
                }
            }
        }
    }

    /// Render sprites from sprite RAM.
    fn render_sprites(&mut self) {
        let sprite_ram = self.map.region_data(Region::SpriteRam);
        let sprite_count = self.sprite_cache.count().max(1);

        for entry in 0..64usize {
            let offs = entry * 4;
            let sy_raw = sprite_ram[offs & 0xFF];
            let sx_raw = sprite_ram[(offs + 1) & 0xFF];
            let code_raw = sprite_ram[(offs + 2) & 0xFF];

            let sx = sx_raw as i32 - 4;
            let sy = sy_raw as i32 - 13;
            let code = ((255 ^ code_raw) as usize + 256 * self.sprite_bank as usize) % sprite_count;

            for py in 0..16usize {
                let screen_y = sy + py as i32;
                if screen_y < 0 || screen_y >= NATIVE_HEIGHT as i32 {
                    continue;
                }
                for px in 0..16usize {
                    let screen_x = sx + px as i32;
                    if screen_x < 0 || screen_x >= NATIVE_WIDTH as i32 {
                        continue;
                    }
                    let pixel = self.sprite_cache.pixel(code, px, py);
                    if pixel != 0 {
                        let buf_idx = screen_y as usize * NATIVE_WIDTH + screen_x as usize;
                        self.pixel_buffer[buf_idx] = pixel;
                    }
                }
            }
        }
    }

    /// Convert the indexed pixel buffer to RGB24 with 270° CW rotation.
    pub fn render_frame(&self, buffer: &mut [u8]) {
        gfx::rotate_270_indexed(
            &self.pixel_buffer,
            buffer,
            NATIVE_WIDTH,
            NATIVE_HEIGHT,
            &self.palette_rgb,
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

    pub fn reset_board(&mut self) {
        self.sound.reset();
        self.map.region_data_mut(Region::Ram).fill(0);
        self.map.region_data_mut(Region::SpriteRam).fill(0);
        self.map.region_data_mut(Region::VideoRam).fill(0);
        self.map.region_data_mut(Region::CharRam).fill(0);
        self.palette_ram.fill(0);
        self.rebuild_palette();
        self.pixel_buffer.fill(0);
        self.video_control = 0;
        self.sprite_bank = 0;
        self.clock = 0;
        self.sound_clock.reset();
        self.votrax_clock.reset();
        self.watchdog_counter = 0;
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

impl Saveable for GottliebBoard {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        self.sound.save_state(w);
        w.write_bytes(self.map.region_data(Region::Nvram));
        w.write_bytes(self.map.region_data(Region::Ram));
        w.write_bytes(self.map.region_data(Region::SpriteRam));
        w.write_bytes(self.map.region_data(Region::VideoRam));
        w.write_bytes(self.map.region_data(Region::CharRam));
        w.write_bytes(&self.palette_ram);
        w.write_u8(self.video_control);
        w.write_u8(self.sprite_bank);
        w.write_u64_le(self.clock);
        self.sound_clock.save_state(w);
        self.votrax_clock.save_state(w);
        w.write_u16_le(self.watchdog_counter);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        self.sound.load_state(r)?;
        r.read_bytes_into(self.map.region_data_mut(Region::Nvram))?;
        r.read_bytes_into(self.map.region_data_mut(Region::Ram))?;
        r.read_bytes_into(self.map.region_data_mut(Region::SpriteRam))?;
        r.read_bytes_into(self.map.region_data_mut(Region::VideoRam))?;
        r.read_bytes_into(self.map.region_data_mut(Region::CharRam))?;
        r.read_bytes_into(&mut self.palette_ram)?;
        self.video_control = r.read_u8()?;
        self.sprite_bank = r.read_u8()?;
        self.clock = r.read_u64_le()?;
        self.sound_clock.load_state(r)?;
        self.votrax_clock.load_state(r)?;
        self.watchdog_counter = r.read_u16_le()?;
        // Rebuild derived state
        self.rebuild_palette();
        Ok(())
    }
}

impl Default for GottliebBoard {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn votrax_write_sets_phoneme_and_inflection() {
        let mut snd = GottliebSoundBoard::new();

        // Write phoneme 0x0A (I2) with inflection 2 → data byte: (2 << 6) | 0x0A = 0x8A
        Bus::write(&mut snd, BusMaster::Cpu(1), 0x2000, 0x8A);

        // A/R should go low immediately after write
        assert!(!snd.votrax.ar_output());
    }

    #[test]
    fn votrax_ar_wires_to_riot_pb7() {
        let mut snd = GottliebSoundBoard::new();

        // Initially A/R is high (ready)
        assert!(snd.votrax.ar_output());

        // Tick once so RIOT PB7 is updated
        snd.tick();
        let pb = snd.riot.read_io(0x02); // Read Port B data
        assert_eq!(pb & 0x80, 0x80, "PB7 should be high when A/R is ready");

        // Write a phoneme → A/R goes low
        Bus::write(&mut snd, BusMaster::Cpu(1), 0x2000, 0x00);
        snd.tick();
        let pb = snd.riot.read_io(0x02);
        assert_eq!(pb & 0x80, 0, "PB7 should be low when A/R is busy");
    }

    #[test]
    fn votrax_clock_divider_ratio() {
        // The Votrax VCO divider is derived from its frequency against the
        // 5 MHz I8088 clock; at the nominal 950 kHz it fires 950k times/sec.
        let mut divider = ClockDivider::new(VOTRAX_NOMINAL_CLOCK_HZ as u32, I8088_CLOCK_HZ as u32);
        let mut ticks = 0u32;
        for _ in 0..I8088_CLOCK_HZ {
            if divider.tick() {
                ticks += 1;
            }
        }
        assert_eq!(ticks, VOTRAX_NOMINAL_CLOCK_HZ as u32);
    }

    #[test]
    fn speech_clock_dac_retunes_votrax() {
        // DAC center (0xA0) → nominal; the original hardcoded 720 kHz was the
        // bug. Higher/lower DAC values scale ±5.5 kHz per step.
        assert_eq!(convert_speech_clock(0xA0), 950_000);
        assert_eq!(convert_speech_clock(0xA0 + 10), 950_000 + 10 * 5_500);
        assert_eq!(convert_speech_clock(0xA0 - 10), 950_000 - 10 * 5_500);

        // A write to the speech-clock DAC region updates the requested clock.
        let mut snd = GottliebSoundBoard::new();
        assert_eq!(snd.speech_clock_hz, VOTRAX_NOMINAL_CLOCK_HZ);
        Bus::write(&mut snd, BusMaster::Cpu(1), 0x3000, 0xC0);
        assert_eq!(snd.speech_clock_hz, convert_speech_clock(0xC0));
    }

    #[test]
    fn sound_board_reset_resets_votrax() {
        let mut snd = GottliebSoundBoard::new();

        // Write a phoneme to put Votrax in busy state
        Bus::write(&mut snd, BusMaster::Cpu(1), 0x2000, 0x05);
        assert!(!snd.votrax.ar_output());

        // Reset should restore A/R to ready
        snd.reset();
        assert!(snd.votrax.ar_output());
    }

    #[test]
    fn votrax_address_mirror() {
        let mut snd = GottliebSoundBoard::new();

        // Write to 0x2123 should still hit Votrax (0x2000-0x2FFF range)
        Bus::write(&mut snd, BusMaster::Cpu(1), 0x2123, 0x0A);
        assert!(!snd.votrax.ar_output());
    }

    #[test]
    fn fill_audio_returns_samples() {
        let mut snd = GottliebSoundBoard::new();

        // Tick many times to accumulate audio samples
        for _ in 0..2000 {
            snd.tick();
        }

        let mut buf = [0i16; 256];
        let count = snd.fill_audio(&mut buf);
        assert!(count > 0, "should produce audio samples after ticking");
    }
}
