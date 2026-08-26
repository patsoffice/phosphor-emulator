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

use phosphor_core::audio::{AudioResampler, DcBlocker};
use phosphor_core::core::debug::{DebugRegister, Debuggable};
use phosphor_core::core::machine::ProfileSpan;
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{
    Bus, BusMaster, ClockDomainName as Clk, ClockTree, DomainId, InterruptState, TimingConfig,
};
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
    // Native (pre-orientation) framebuffer: Q*Bert declares ROT270 and the
    // frontend rotates centrally, so these are the unrotated dimensions.
    display_width: NATIVE_WIDTH as u32,   // 256
    display_height: NATIVE_HEIGHT as u32, // 240
    display_aspect: Some((3, 4)),         // portrait tube as viewed (after ROT270)
};

/// The board's crystals and everything divided out of them.
///
/// Three. The main board carries two, 15 MHz and 20 MHz, and the sound board a
/// third at 3.579545 MHz (Q*Bert instruction manual, parts list). The two main
/// board crystals both land on 5 MHz by different divisions, which is why the
/// I8088 and the pixel clock look like one clock in the code and are not.
///
/// The Votrax SC-01's VCO is a fourth clock source rather than a division of
/// any of them: it is an RC oscillator steered by the speech-clock DAC, so its
/// declared rate is only the power-on nominal and `set_domain_hz` moves it.
pub fn clock_tree() -> ClockTree {
    use phosphor_core::core::RootId;
    let mut t = ClockTree::new(15_000_000);
    let vid = t.add_root(20_000_000);
    let snd = t.add_root(3_579_545);
    let vco = t.add_root(VOTRAX_NOMINAL_CLOCK_HZ as u32);
    let cpu = t.add_domain(Clk::Cpu, RootId::MAIN, 1, 3); // 5 MHz I8088
    let dot = t.add_domain(Clk::Pixel, vid, 1, 4); // 5 MHz pixel clock
    t.add_domain(Clk::SoundCpu, snd, 1, 4); // 894886.25 Hz 6502
    t.add_domain(Clk::Speech, vco, 1, 1); // Votrax SC-01, 950 kHz at DAC centre
    t.set_step_domain(cpu);
    // Two crystals, but both divide to exactly 5 MHz, so 318 pixel clocks is
    // exactly 318 CPU cycles with nothing to round.
    t.set_raster(dot, 318, 0);
    t
}

pub const VISIBLE_LINES: u64 = 240;
pub fn output_sample_rate() -> u64 {
    phosphor_core::audio::host_sample_rate() as u64
}

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

// The I8088's 5 MHz, the sound 6502's rate and the Votrax's VCO all come from
// `clock_tree()` now, so none of them is a constant here any more.

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
#[save_version(4)] // v4: the two ClockDividers became a ClockTree living here
pub(crate) struct GottliebSoundBoard {
    riot: Riot6532,
    dac: Mc1408Dac,
    votrax: VotraxSc01,
    resampler: AudioResampler<i16>,
    /// The sound board's coupling into the amplifier.
    ///
    /// The MC1408 is a current-output ladder and is UNIPOLAR, so what it puts
    /// out sits above ground rather than either side of it, and the sound CPU
    /// drives it around a low code rather than the mid-scale its conversion
    /// assumes. Q*bert measured a DC of -0.447 under recorded play with 5.6 %
    /// of samples clipped, which is that pedestal eating headroom until the
    /// peaks run out of room.
    ///
    /// Applied to the summed output rather than the ladder alone: the speech
    /// synthesizer joins the same amplifier, and one capacitor at that point is
    /// what the board has.
    output_coupling: DcBlocker,
    #[save_skip]
    sound_rom: Vec<u8>, // 8KB (mapped at 0x6000-0x7FFF in 15-bit space)
    clock: u64,
    /// Previous A/R state for edge detection (NMI on rising edge).
    votrax_ar_prev: bool,
    /// NMI pending from Votrax A/R rising edge.
    votrax_nmi: bool,
    /// The whole board's clock tree, exactly as [`clock_tree`] declares it.
    ///
    /// It lives on the *sound* board rather than on [`GottliebBoard`] because
    /// the speech-clock DAC write that retunes the Votrax VCO arrives here, in
    /// this struct's `Bus` impl. Holding the tree where the retune happens is
    /// what lets that be a single call site instead of a flag the outer board
    /// polls once per CPU cycle. The main CPU and pixel domains ride along
    /// unstepped: they are the reference the other two are expressed against.
    clocks: ClockTree,
    #[save_skip]
    sound_dom: DomainId,
    #[save_skip]
    votrax_dom: DomainId,
}

impl GottliebSoundBoard {
    fn new() -> Self {
        let clocks = clock_tree();
        let sound_dom = clocks.find(Clk::SoundCpu).expect("declared sound domain");
        let votrax_dom = clocks.find(Clk::Speech).expect("declared speech domain");
        // The rate the sound CPU is stepped at and the rate its samples are
        // resampled from are now the same derivation, read from the same
        // domain. They used to be two separately-rounded constants that
        // disagreed by 127 ppm in opposite directions.
        let sound_hz = clocks.hz(sound_dom);
        Self {
            riot: Riot6532::new(),
            dac: Mc1408Dac::new(),
            votrax: VotraxSc01::new(VOTRAX_NOMINAL_CLOCK_HZ),
            resampler: AudioResampler::new(sound_hz, output_sample_rate()),
            output_coupling: DcBlocker::new(output_sample_rate() as u32),
            sound_rom: vec![0xFF; 0x2000],
            clock: 0,
            votrax_ar_prev: true,
            votrax_nmi: false,
            clocks,
            sound_dom,
            votrax_dom,
        }
    }

    /// Retune the Votrax VCO, device and clock domain together.
    ///
    /// The pairing is the point: a rate that reaches one but not the other is
    /// how the speech clock got out of step in the first place
    /// (`phosphor-emulator-1fg`).
    fn set_votrax_clock(&mut self, clock_hz: u64) {
        self.clocks.set_domain_hz(self.votrax_dom, clock_hz as u32);
        self.votrax.set_clock(clock_hz);
    }

    /// Whether the sound 6502 is due a cycle this I8088 cycle.
    #[inline]
    fn sound_cpu_due(&mut self) -> bool {
        self.clocks.tick(self.sound_dom)
    }

    /// Whether the Votrax is due a cycle this I8088 cycle.
    #[inline]
    fn votrax_due(&mut self) -> bool {
        self.clocks.tick(self.votrax_dom)
    }

    /// Clear every domain's phase, leaving the rates alone.
    ///
    /// A board reset does not re-centre the speech-clock DAC, so the VCO keeps
    /// whatever frequency it was last steered to.
    fn reset_clock_phases(&mut self) {
        self.clocks.reset();
    }

    /// Push the loaded VCO rate back into the speech device.
    ///
    /// The tree restores its own ratio, because [`ClockDomain`] saves the live
    /// one. The device cannot: `VotraxSc01` save-skips `main_clock_hz` and the
    /// sample and capacitor clocks derived from it, so they have to be re-fed
    /// from the tree once, here, rather than left for a per-cycle comparison to
    /// notice.
    ///
    /// [`ClockDomain`]: phosphor_core::core::ClockDomain
    fn reapply_speech_clock(&mut self) {
        self.votrax.set_clock(self.clocks.hz(self.votrax_dom));
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
    ///
    /// The sound 6502 lives on [`GottliebBoard`] rather than in here, because
    /// this struct *is* the bus it drives -- RIOT, DAC, Votrax and the sound
    /// ROM. Taking the CPU as a separate borrow is what makes that dispatch
    /// concrete instead of a trait object.
    fn tick(&mut self, cpu: &mut M6502) {
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
        cpu.execute_cycle(self, BusMaster::Cpu(1));

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
        // The coupling capacitor, after the sum: it sits between the sound
        // board and the amplifier, so it sees the ladder and the speech
        // together.
        for s in buffer.iter_mut().take(mix_len) {
            *s = self
                .output_coupling
                .process(*s as f32)
                .clamp(i16::MIN as f32, i16::MAX as f32) as i16;
        }
        mix_len
    }

    /// Reset the sound board's devices. The CPU is reset by its owner (see
    /// [`GottliebBoard::reset_sound`]), which resets it against this bus.
    fn reset(&mut self) {
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

            // Speech clock DAC: 0x3000-0x3FFF retunes the Votrax VCO frequency,
            // applied to the clock domain and the device in the same call.
            0x3000..=0x3FFF => self.set_votrax_clock(convert_speech_clock(data)),

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
/// One CPU cycle: board work, the 8088, then the sound board and Votrax.
///
/// The CPU lives on the machine and the board *is* the bus (Q*bert is the only
/// machine on this hardware), so this takes them as separate borrows and
/// dispatches at a concrete type.
#[inline]
pub fn tick(cpu: &mut I8088, board: &mut GottliebBoard) {
    board.begin_cycle(cpu);
    cpu.execute_cycle(board, BusMaster::Cpu(0));
    board.end_cycle();
}

/// Run one frame's worth of cycles. The board has no scanline-boundary work --
/// its only frame-position test is the end-of-frame render inside `end_cycle`
/// -- so this is a plain loop.
pub fn run_frame(cpu: &mut I8088, board: &mut GottliebBoard) {
    for _ in 0..TIMING.cycles_per_frame() {
        tick(cpu, board);
    }
}

#[derive(BusDebug)]
pub struct GottliebBoard {
    // Sound board (RIOT + DAC + Votrax). Its M6502 sits beside it rather than
    // inside it, so the sound CPU's cycles dispatch at a concrete type.
    pub(crate) sound_cpu: M6502,
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
    pub(crate) watchdog_counter: u16,

    // Profiling (not saved)
    pub(crate) profiling: bool,
    pub(crate) profile_spans: Vec<ProfileSpan>,
    /// Time spent in the most recent frame-boundary render. The render now runs
    /// inside `tick`, so the wrapper subtracts this to keep its "cpu" vs "gfx"
    /// spans meaningful. Only populated while `profiling` is on.
    pub(crate) last_render: std::time::Duration,
}

impl GottliebBoard {
    pub fn new() -> Self {
        Self {
            sound_cpu: M6502::new(),
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
            watchdog_counter: 0,
            profiling: false,
            profile_spans: Vec::new(),
            last_render: std::time::Duration::ZERO,
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
    /// Per-cycle board work that runs before the CPU.
    fn begin_cycle(&mut self, cpu: &I8088) {
        // Latch watchpoint attribution context (cycle + instruction PC)
        // before CPU execution — bus dispatch cannot read CPU state mid-tick.
        // The I8088 debug surface uses IP as its PC (matching debug_pc).
        if self.map.debug_active() {
            let pc = cpu.at_instruction_boundary().then_some(cpu.ip as u32);
            self.map.latch_access_context(self.clock, pc);
        }
    }

    /// Board work after the CPU's cycle: the sound board, the Votrax, the
    /// clock, and the end-of-frame render.
    fn end_cycle(&mut self) {
        // Tick sound board at fractional rate (~895 kHz). The sound CPU and the
        // board it drives are disjoint fields here, so its cycle dispatches at a
        // concrete type just like the main CPU's.
        //
        // No speech-clock comparison precedes this any more: the DAC write
        // retunes the domain and the device together where it lands, so there
        // is nothing for this loop to notice.
        if self.sound.sound_cpu_due() {
            self.sound.tick(&mut self.sound_cpu);
        }

        // Tick Votrax SC-01 at its VCO rate (nominally 950 kHz, DAC-tunable)
        if self.sound.votrax_due() {
            self.sound.tick_votrax();
        }

        self.clock += 1;
        self.watchdog_counter = self.watchdog_counter.wrapping_add(1);

        // Refresh the cached framebuffer whenever this cycle completed a frame.
        // Rendering here rather than after `run_frame`'s loop means the
        // debugger's `debug_tick()` path (which never calls `run_frame`) also
        // refreshes the picture. Firing on the frame's *last* cycle samples the
        // same video state the old end-of-loop render saw, so output is
        // byte-identical — note this board writes palette during vblank, so
        // rendering at the vblank boundary instead would change the picture.
        if self.clock.is_multiple_of(TIMING.cycles_per_frame()) {
            let started = self.profiling.then(std::time::Instant::now);
            self.render_frame_internal();
            if let Some(started) = started {
                self.last_render = started.elapsed();
            }
        }
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

    /// Render tiles from video RAM into the indexed pixel buffer.
    ///
    /// Each tile selects one of two 8×8 caches (ROM vs char-RAM) by its code and
    /// the gfxchar latches. `render_tilemap_scanline_indexed` renders from a
    /// single cache, so this runs one pass per cache: a tile belonging to the
    /// other cache resolves to `None` (attr 0 = skip). Tiles tile the screen
    /// without overlap, so the two passes are independent and pixel-identical to
    /// the original single-pass select. The decoded pixel is the palette index
    /// directly (pen 0 transparent); there is no colour attribute.
    fn render_tiles(&mut self) {
        self.render_tile_layer(true); // ROM-char tiles
        self.render_tile_layer(false); // char-RAM tiles
    }

    fn render_tile_layer(&mut self, is_rom: bool) {
        let config = gfx::TilemapConfig {
            cols: TILE_COLS,
            rows: TILE_ROWS,
            tile_width: 8,
            tile_height: 8,
        };
        let video_ram = self.map.region_data(Region::VideoRam);
        let gfxcharhi = self.gfxcharhi;
        let gfxcharlo = self.gfxcharlo;
        let cache = if is_rom {
            &self.tile_rom_cache
        } else {
            &self.charram_cache
        };
        let count = cache.count().max(1);
        let pixel_buffer = &mut self.pixel_buffer;
        let mut prio = [0u8; NATIVE_WIDTH];
        for scanline in 0..NATIVE_HEIGHT {
            let row_off = scanline * NATIVE_WIDTH;
            let row = &mut pixel_buffer[row_off..row_off + NATIVE_WIDTH];
            gfx::render_tilemap_scanline_indexed(
                &config,
                cache,
                scanline,
                |col, trow| {
                    let tile_index = trow * TILE_COLS + col;
                    let code = video_ram[tile_index & 0x3FF] as usize;
                    let use_rom = if code & 0x80 != 0 {
                        gfxcharhi
                    } else {
                        gfxcharlo
                    };
                    if use_rom == is_rom {
                        let cache_code = if is_rom {
                            code % count
                        } else {
                            (code & 0x7F) % count
                        };
                        gfx::TileInfo::new(cache_code as u16, 1) // attr 1 = this cache
                    } else {
                        gfx::TileInfo::new(0, 0) // attr 0 = skip (other cache)
                    }
                },
                // Skip tiles owned by the other cache; pen 0 is transparent. The
                // decoded pixel value is the palette index.
                |attr, pixel| (attr != 0 && pixel != 0).then_some((pixel, 0)),
                row,
                &mut prio,
                0,
            );
        }
    }

    /// Render sprites from sprite RAM into the indexed pixel buffer.
    ///
    /// 16×16 sprites, no flip; the decoded pixel is the palette index (pen 0
    /// transparent). Composited by draw order (render_frame_internal picks the
    /// tiles/sprites order from the background-priority bit), so no priority
    /// buffer is used.
    fn render_sprites(&mut self) {
        let sprite_ram = self.map.region_data(Region::SpriteRam);
        let sprite_cache = &self.sprite_cache;
        let sprite_count = sprite_cache.count().max(1);
        let sprite_bank = self.sprite_bank;
        let pixel_buffer = &mut self.pixel_buffer;
        let clip = gfx::SpriteClip {
            x_min: 0,
            x_max: NATIVE_WIDTH as i32,
            wrap_offset: None,
        };
        let mut prio = [0u8; NATIVE_WIDTH];

        for entry in 0..64usize {
            let offs = entry * 4;
            let sy_raw = sprite_ram[offs & 0xFF];
            let sx_raw = sprite_ram[(offs + 1) & 0xFF];
            let code_raw = sprite_ram[(offs + 2) & 0xFF];

            let sx = sx_raw as i32 - 4;
            let sy = sy_raw as i32 - 13;
            let code = ((255 ^ code_raw) as usize + 256 * sprite_bank as usize) % sprite_count;

            for py in 0..16usize {
                let screen_y = sy + py as i32;
                if screen_y < 0 || screen_y >= NATIVE_HEIGHT as i32 {
                    continue;
                }
                let row_off = screen_y as usize * NATIVE_WIDTH;
                let row = &mut pixel_buffer[row_off..row_off + NATIVE_WIDTH];
                gfx::draw_sprite_row_indexed(
                    sprite_cache,
                    code as u16,
                    py,
                    sx,
                    false,
                    |pixel| pixel == 0,
                    |pixel| (pixel, 0u8),
                    row,
                    &mut prio,
                    &clip,
                );
            }
        }
    }

    /// Convert the indexed pixel buffer to native (unrotated) RGB24.
    ///
    /// The 270° CW rotation Q*Bert's cabinet needs is declared via
    /// [`orientation`](Self::orientation) and applied centrally by the frontend,
    /// so this emits pixels in native row-major order.
    pub fn render_frame(&self, buffer: &mut [u8]) {
        let mask = self.palette_rgb.len() - 1;
        for (i, &idx) in self.pixel_buffer.iter().enumerate() {
            let (r, g, b) = self.palette_rgb[idx as usize & mask];
            buffer[i * 3] = r;
            buffer[i * 3 + 1] = g;
            buffer[i * 3 + 2] = b;
        }
    }

    /// Q*Bert's monitor is mounted rotated 270° clockwise. The orientation is
    /// declarative — the frontend rotates `render_frame`'s native output.
    pub fn orientation(&self) -> phosphor_core::core::machine::Orientation {
        phosphor_core::core::machine::Orientation::ROT270
    }

    // -----------------------------------------------------------------------
    // Audio
    // -----------------------------------------------------------------------

    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.sound.fill_audio(buffer)
    }

    /// Reset the sound board and its CPU. The 6502 fetches its reset vector
    /// through the sound bus, which is why the two are reset together here.
    fn reset_sound(&mut self) {
        self.sound.reset();
        self.sound_cpu.reset(&mut self.sound, BusMaster::Cpu(1));
    }

    // -----------------------------------------------------------------------
    // Reset
    // -----------------------------------------------------------------------

    pub fn reset_board(&mut self) {
        self.reset_sound();
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
        self.sound.reset_clock_phases();
        self.watchdog_counter = 0;
        // NVRAM is NOT cleared (battery-backed)
    }

    // -----------------------------------------------------------------------
    // Debug
    // -----------------------------------------------------------------------

    /// Whether the CPU is at an instruction boundary. It lives on the machine,
    /// which passes it back in.
    pub fn instruction_boundaries(cpu: &I8088) -> u32 {
        u32::from(cpu.at_instruction_boundary())
    }
}

impl Saveable for GottliebBoard {
    fn save_state(&self, w: &mut StateWriter) {
        // The CPU is saved by the machine, which owns it.
        // Sound CPU first, then the rest of the sound board: the same byte
        // order the sound board wrote when it owned the CPU.
        self.sound_cpu.save_state(w);
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
        // The clock tree travels inside the sound board, which owns it.
        w.write_u16_le(self.watchdog_counter);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        // The CPU is loaded by the machine, which owns it.
        self.sound_cpu.load_state(r)?;
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
        self.watchdog_counter = r.read_u16_le()?;
        // Rebuild derived state
        self.rebuild_palette();
        // The speech domain came back at whatever rate it was retuned to,
        // because ClockDomain saves its live ratio. The SC-01 itself cannot:
        // it save-skips its main clock and the two clocks derived from it, so
        // hand them back from the tree. Without this a machine that had already
        // retuned the VCO keeps the wrong speech clock, and emits a different
        // number of samples per frame than the saved machine did.
        self.sound.reapply_speech_clock();
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
        let mut cpu = M6502::new();

        // Initially A/R is high (ready)
        assert!(snd.votrax.ar_output());

        // Tick once so RIOT PB7 is updated
        snd.tick(&mut cpu);
        let pb = snd.riot.read_io(0x02); // Read Port B data
        assert_eq!(pb & 0x80, 0x80, "PB7 should be high when A/R is ready");

        // Write a phoneme → A/R goes low
        Bus::write(&mut snd, BusMaster::Cpu(1), 0x2000, 0x00);
        snd.tick(&mut cpu);
        let pb = snd.riot.read_io(0x02);
        assert_eq!(pb & 0x80, 0, "PB7 should be low when A/R is busy");
    }

    #[test]
    fn votrax_clock_divider_ratio() {
        // The Votrax VCO domain is expressed against the I8088's clock; at the
        // nominal 950 kHz it fires 950k times per second. Both figures come out
        // of the tree, so the test states no rate of its own.
        let mut snd = GottliebSoundBoard::new();
        assert_eq!(snd.clocks.hz(snd.votrax_dom), VOTRAX_NOMINAL_CLOCK_HZ);
        let cpu_hz = snd
            .clocks
            .hz(snd.clocks.find(Clk::Cpu).expect("cpu domain"));
        assert_eq!(cpu_hz, 5_000_000);
        let mut ticks = 0u32;
        for _ in 0..cpu_hz {
            if snd.votrax_due() {
                ticks += 1;
            }
        }
        assert_eq!(ticks, VOTRAX_NOMINAL_CLOCK_HZ as u32);
    }

    /// The sound CPU runs the ratio its crystal gives, not a hand-reduced one.
    ///
    /// 3.579545 MHz over four is 894886.25 Hz, and against the 5 MHz I8088 that
    /// is exactly 715909/4000000. The board ran 179/1000 for years, which is
    /// 895000 Hz: 114 Hz fast. Nothing rounds on the way here now, so pin the
    /// ratio itself rather than a rounded rate.
    #[test]
    fn sound_cpu_runs_the_crystal_ratio() {
        let snd = GottliebSoundBoard::new();
        assert_eq!(
            snd.clocks.domain(snd.sound_dom).step_ratio(),
            (715_909, 4_000_000)
        );
        // A quarter of the sound crystal, stated as such.
        assert_eq!(snd.clocks.domain(snd.sound_dom).root_ratio(), (1, 4));
        // The exact rate is 894886.25 Hz, so hz() reports the nearest hertz.
        assert_eq!(snd.clocks.hz_exact(snd.sound_dom), (3_579_545, 4));
        assert_eq!(snd.clocks.hz(snd.sound_dom), 894_886);
    }

    /// The divider and the resampler come from one derivation.
    ///
    /// They used to be separate constants: the domain ticked 895000 times a
    /// second while the resampler was told the board emitted at 894886, so the
    /// two disagreed by 127 ppm in opposite directions from the truth. That is
    /// the shape of the Votrax bug (`phosphor-emulator-1fg`), one level down.
    #[test]
    fn the_resampler_input_rate_is_the_domain_rate() {
        let snd = GottliebSoundBoard::new();
        assert_eq!(snd.resampler.input_rate(), snd.clocks.hz(snd.sound_dom));
    }

    #[test]
    fn speech_clock_dac_retunes_votrax() {
        // DAC center (0xA0) → nominal; the original hardcoded 720 kHz was the
        // bug. Higher/lower DAC values scale ±5.5 kHz per step.
        assert_eq!(convert_speech_clock(0xA0), 950_000);
        assert_eq!(convert_speech_clock(0xA0 + 10), 950_000 + 10 * 5_500);
        assert_eq!(convert_speech_clock(0xA0 - 10), 950_000 - 10 * 5_500);

        // A write to the speech-clock DAC region retunes the clock domain and
        // the device in the same call, which is the whole point of routing it
        // through `set_votrax_clock`.
        let mut snd = GottliebSoundBoard::new();
        assert_eq!(snd.clocks.hz(snd.votrax_dom), VOTRAX_NOMINAL_CLOCK_HZ);
        Bus::write(&mut snd, BusMaster::Cpu(1), 0x3000, 0xC0);
        let want = convert_speech_clock(0xC0);
        assert_eq!(snd.clocks.hz(snd.votrax_dom), want);
        assert_eq!(snd.votrax.clock_hz(), want);
    }

    /// A retuned speech clock survives a save/load, without the shadow field
    /// that used to force a re-derive on the next tick.
    #[test]
    fn a_retuned_speech_clock_reloads_retuned() {
        let mut board = GottliebBoard::new();
        Bus::write(&mut board.sound, BusMaster::Cpu(1), 0x3000, 0xC0);
        let want = convert_speech_clock(0xC0);

        let mut w = StateWriter::new();
        board.save_state(&mut w);
        let data = w.into_vec();

        let mut restored = GottliebBoard::new();
        assert_eq!(restored.sound.clocks.hz(restored.sound.votrax_dom), 950_000);
        let mut r = StateReader::new(&data);
        restored.load_state(&mut r).unwrap();

        assert_eq!(restored.sound.clocks.hz(restored.sound.votrax_dom), want);
        // The device save-skips its clock, so the load has to hand it back.
        assert_eq!(restored.sound.votrax.clock_hz(), want);
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
        let mut cpu = M6502::new();

        // Tick many times to accumulate audio samples
        for _ in 0..2000 {
            snd.tick(&mut cpu);
        }

        let mut buf = [0i16; 256];
        let count = snd.fill_audio(&mut buf);
        assert!(count > 0, "should produce audio samples after ticking");
    }
}
