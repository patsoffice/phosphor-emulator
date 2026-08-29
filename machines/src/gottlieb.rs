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
pub(crate) const GOTTLIEB_TILE_LAYOUT: GfxLayout<'static> = GfxLayout {
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
// v4: the two ClockDividers became a ClockTree living here.
//
// Bumped to 5 by the move to field TLV. This struct is the reason it is worth
// doing: four bumps in its life, three of them a component being added to or
// taken out of the middle of the body, which is exactly what TLV absorbs.
#[save_version(5)]
#[save_tlv]
pub(crate) struct GottliebSoundBoard {
    #[save(id = 1)]
    riot: Riot6532,
    #[save(id = 2)]
    dac: Mc1408Dac,
    #[save(id = 3)]
    votrax: VotraxSc01,
    #[save(id = 4)]
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
    #[save(id = 5)]
    output_coupling: DcBlocker,
    #[save_skip]
    sound_rom: Vec<u8>, // 8KB (mapped at 0x6000-0x7FFF in 15-bit space)
    #[save(id = 6)]
    clock: u64,
    /// Previous A/R state for edge detection (NMI on rising edge).
    #[save(id = 7)]
    votrax_ar_prev: bool,
    /// NMI pending from Votrax A/R rising edge.
    #[save(id = 8)]
    votrax_nmi: bool,
    /// The whole board's clock tree, exactly as [`clock_tree`] declares it.
    ///
    /// It lives on the *sound* board rather than on [`GottliebBoard`] because
    /// the speech-clock DAC write that retunes the Votrax VCO arrives here, in
    /// this struct's `Bus` impl. Holding the tree where the retune happens is
    /// what lets that be a single call site instead of a flag the outer board
    /// polls once per CPU cycle. The main CPU and pixel domains ride along
    /// unstepped: they are the reference the other two are expressed against.
    #[save(id = 9)]
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
        let mut regs = vec![
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
        ];
        // The live clocks, including the Votrax VCO's, which the speech-clock
        // DAC moves at runtime. Reading it off the constructor stopped being
        // right the first time the game wrote to 0x3000.
        regs.extend(self.clocks.debug_registers());
        regs
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
///
/// This is the debugger's path: it tests the frame position on every cycle so
/// that single-stepping still crosses scanline boundaries. A whole frame goes
/// through [`run_scanlines`], which hoists that test out.
#[inline]
pub fn tick(cpu: &mut I8088, board: &mut GottliebBoard) {
    let frame_cycle = board.clock % TIMING.cycles_per_frame();
    if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
        board.begin_scanline(frame_cycle / TIMING.cycles_per_scanline);
    }
    step_cycle(cpu, board);
}

/// Run `cycles` CPU cycles, scanline-outer and cycle-inner. The caller must
/// start on a scanline boundary and pass a multiple of `cycles_per_scanline`;
/// the debugger's off-boundary stepping goes through [`tick`] instead.
pub fn run_scanlines(cpu: &mut I8088, board: &mut GottliebBoard, cycles: u64) {
    debug_assert!(
        board.clock.is_multiple_of(TIMING.cycles_per_scanline)
            && cycles.is_multiple_of(TIMING.cycles_per_scanline),
        "run_scanlines must start on a scanline boundary and run whole scanlines"
    );
    for _ in 0..cycles / TIMING.cycles_per_scanline {
        let scanline = board.clock % TIMING.cycles_per_frame() / TIMING.cycles_per_scanline;
        board.begin_scanline(scanline);
        for _ in 0..TIMING.cycles_per_scanline {
            step_cycle(cpu, board);
        }
    }
}

/// Run one frame's worth of cycles. Whole scanlines go through
/// [`run_scanlines`]; a partial scanline at either end (only after the debugger
/// has left the clock off-boundary) goes through [`tick`].
pub fn run_frame(cpu: &mut I8088, board: &mut GottliebBoard) {
    let scanline = TIMING.cycles_per_scanline;
    let mut remaining = TIMING.cycles_per_frame();

    let lead = ((scanline - board.clock % scanline) % scanline).min(remaining);
    for _ in 0..lead {
        tick(cpu, board);
    }
    remaining -= lead;

    let whole = remaining - remaining % scanline;
    run_scanlines(cpu, board, whole);
    remaining -= whole;

    for _ in 0..remaining {
        tick(cpu, board);
    }
}

/// The part of a cycle with no frame-position test in it.
#[inline]
fn step_cycle(cpu: &mut I8088, board: &mut GottliebBoard) {
    board.begin_cycle(cpu);
    cpu.execute_cycle(board, BusMaster::Cpu(0));
    board.end_cycle();
}

/// The speech clock is the one thing a load cannot restore from the file. The
/// domain comes back at whatever rate it was retuned to, because `ClockDomain`
/// saves its live ratio, but the SC-01 itself save-skips its main clock and the
/// two derived from it, so they have to be handed back from the tree. Without
/// that a machine that had already retuned the VCO keeps the wrong speech clock
/// and emits a different number of samples per frame than the saved machine
/// did.
#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
#[save_after_load(restore_after_load)]
pub struct GottliebBoard {
    // Sound board (RIOT + DAC + Votrax). Its M6502 sits beside it rather than
    // inside it, so the sound CPU's cycles dispatch at a concrete type.
    #[save(id = 1)]
    pub(crate) sound_cpu: M6502,
    #[debug_device("Sound Board")]
    #[save(id = 2)]
    pub(crate) sound: GottliebSoundBoard,

    /// The address space persists its own writable regions: NVRAM, work RAM,
    /// sprite RAM, video RAM and char RAM here.
    #[debug_map(cpu = 0)]
    #[save(id = 3)]
    pub(crate) map: AddressSpace16,

    // GFX caches
    #[save_skip]
    pub(crate) tile_rom_cache: gfx::GfxCache,
    #[save_skip]
    pub(crate) charram_cache: gfx::GfxCache,
    #[save_skip]
    pub(crate) sprite_cache: gfx::GfxCache,

    // Palette (16 entries, 4-bit RGB per channel)
    #[save(id = 4)]
    pub(crate) palette_ram: [u8; 32],
    /// Expanded from `palette_ram`, and saved beside it rather than rebuilt
    /// after a load.
    #[save(id = 5)]
    pub(crate) palette_rgb: [(u8, u8, u8); 16],

    /// [`palette_rgb`](Self::palette_rgb) as it stood at the start of each
    /// visible scanline.
    ///
    /// The hardware resolves pen to RGB as the beam passes each pixel, so a
    /// palette write partway down the screen colours only the rows below it.
    /// Sampling per scanline is itself an approximation of that (a write
    /// mid-*line* still quantises to the line), but it captures every case
    /// observed on this board.
    ///
    /// This is now in phase with the rest of the picture: tiles and sprites are
    /// composited into [`pixel_buffer`](Self::pixel_buffer) at the same scanline
    /// boundary that fills this row's entry, so a row's colours and its pixels
    /// come from the same moment. They did not between W1 and W4 of the
    /// raster-sampling epic, when only the palette had moved.
    ///
    /// Derived state, rebuilt every frame, so not saved: seeded from
    /// `palette_rgb` in [`restore_after_load`](Self::restore_after_load) so the
    /// first frame after a load resolves against a real palette rather than a
    /// stale one.
    #[save_skip]
    pub(crate) palette_scanline: Vec<[(u8, u8, u8); 16]>,

    /// Framebuffer (256×240 palette indices), filled one row at a time as the
    /// beam reaches each visible scanline. A row therefore holds what the beam
    /// drew on that line, out of the video RAM, sprite RAM, sprite bank and
    /// layer-order bit as they stood at that line's boundary.
    ///
    /// Consequence worth knowing: the picture a completed frame presents does
    /// *not* contain that frame's own vblank writes, because the beam had
    /// already passed. The whole-frame render this replaced did contain them.
    #[save_skip]
    pub(crate) pixel_buffer: Vec<u8>,

    // Video state
    #[save(id = 6)]
    pub(crate) video_control: u8,
    #[save(id = 7)]
    pub(crate) sprite_bank: u8,

    // Tile source selection (true = ROM, false = charram). Set once at ROM load
    // by the game wrapper, so it is how the board is built rather than state.
    #[save_skip]
    pub(crate) gfxcharlo: bool, // codes 0x00-0x7F
    #[save_skip]
    pub(crate) gfxcharhi: bool, // codes 0x80-0xFF

    // I/O ports (active-high for Q*Bert joystick/buttons) and the DIP byte,
    // which keep their previous treatment: live input and operator
    // configuration, neither of which a load takes back.
    #[save_skip]
    pub(crate) input_ports: [u8; 4], // IN1-IN4
    #[save_skip]
    pub(crate) dsw: u8,

    // Timing
    #[save(id = 8)]
    pub(crate) clock: u64,
    #[save(id = 9)]
    pub(crate) watchdog_counter: u16,

    // Profiling (not saved)
    #[save_skip]
    pub(crate) profiling: bool,
    #[save_skip]
    pub(crate) profile_spans: Vec<ProfileSpan>,
    /// Time spent in the most recent frame-boundary render. The render now runs
    /// inside `tick`, so the wrapper subtracts this to keep its "cpu" vs "gfx"
    /// spans meaningful. Only populated while `profiling` is on.
    #[save_skip]
    pub(crate) last_render: std::time::Duration,
}

impl GottliebBoard {
    /// Put back the derived state a load cannot carry.
    ///
    /// The SC-01's clocks, for the reason on the struct, and the per-scanline
    /// palette, which is rebuilt as a frame runs and would otherwise resolve the
    /// first post-load frame's upper rows against whatever the previous machine
    /// had. Seeding every row from the loaded `palette_rgb` makes that first
    /// frame behave the way the whole-frame render used to.
    fn restore_after_load(&mut self) {
        self.sound.reapply_speech_clock();
        self.palette_scanline.fill(self.palette_rgb);
    }

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
            palette_scanline: vec![[(0, 0, 0); 16]; VISIBLE_LINES as usize],
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

    /// Work that only happens on the first cycle of a scanline: drawing the row
    /// the beam is about to paint, out of the video state as it stands now.
    ///
    /// `scanline` is 0..256; 0..239 are visible and 240..255 are vblank, the
    /// same split `check_interrupts` uses to raise the VBLANK NMI. Only visible
    /// lines have a row to draw.
    ///
    /// Called once per scanline from [`run_scanlines`] and, for the debugger's
    /// single-step path, from [`tick`] when the clock lands on a boundary.
    pub(crate) fn begin_scanline(&mut self, scanline: u64) {
        if scanline >= VISIBLE_LINES {
            return;
        }
        let y = scanline as usize;
        self.palette_scanline[y] = self.palette_rgb;

        // The wrapper splits "gfx" out of its frame total using `last_render`,
        // which used to be one frame-boundary render. It is now 240 of them, so
        // start the sum at the top of the visible area and add each row to it.
        let started = self.profiling.then(std::time::Instant::now);
        if y == 0 {
            self.last_render = std::time::Duration::ZERO;
        }
        self.render_scanline(y);
        if let Some(started) = started {
            self.last_render += started.elapsed();
        }
    }

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

        // No frame-boundary render here any more: each visible row is
        // composited at its own scanline boundary in `begin_scanline`, which
        // both `run_scanlines` and the debugger's `tick` reach.
    }

    // -----------------------------------------------------------------------
    // Frame rendering
    // -----------------------------------------------------------------------

    /// Draw one visible row into the indexed pixel buffer, out of the video
    /// state as it stands at that row's scanline boundary.
    ///
    /// Layer *order* is itself a live register on this board: `video_control`
    /// bit 0 picks sprites-behind-tiles, and it is read per row here rather than
    /// once per frame, so a mid-screen write to it swaps the order from that row
    /// down.
    fn render_scanline(&mut self, y: usize) {
        let bg_priority = self.video_control & 0x01 != 0;

        // Clear to background (palette index 0)
        let row_off = y * NATIVE_WIDTH;
        self.pixel_buffer[row_off..row_off + NATIVE_WIDTH].fill(0);

        if bg_priority {
            // Background priority: sprites behind tiles
            self.render_sprites_scanline(y);
            self.render_tiles_scanline(y);
        } else {
            // Normal: tiles behind sprites
            self.render_tiles_scanline(y);
            self.render_sprites_scanline(y);
        }
    }

    /// Render one row of tiles from video RAM into the indexed pixel buffer.
    ///
    /// Each tile selects one of two 8×8 caches (ROM vs char-RAM) by its code and
    /// the gfxchar latches. `render_tilemap_scanline_indexed` renders from a
    /// single cache, so this runs one pass per cache: a tile belonging to the
    /// other cache resolves to `None` (attr 0 = skip). Tiles tile the screen
    /// without overlap, so the two passes are independent and pixel-identical to
    /// the original single-pass select. The decoded pixel is the palette index
    /// directly (pen 0 transparent); there is no colour attribute.
    fn render_tiles_scanline(&mut self, y: usize) {
        self.render_tile_layer_scanline(y, true); // ROM-char tiles
        self.render_tile_layer_scanline(y, false); // char-RAM tiles
    }

    fn render_tile_layer_scanline(&mut self, scanline: usize, is_rom: bool) {
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
        // A cache with no entries has no pixels to index. Q*Bert leaves the ROM
        // cache empty (both gfxchar latches select char RAM), and a board built
        // without graphics ROMs leaves both empty.
        let count = cache.count();
        if count == 0 {
            return;
        }
        let row_off = scanline * NATIVE_WIDTH;
        let row = &mut self.pixel_buffer[row_off..row_off + NATIVE_WIDTH];
        // Scratch, because this board's resolver always returns priority 0 and
        // nothing reads it back.
        let mut prio = [0u8; NATIVE_WIDTH];
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

    /// Render one row of sprites from sprite RAM into the indexed pixel buffer.
    ///
    /// 16×16 sprites, no flip; the decoded pixel is the palette index (pen 0
    /// transparent). Composited by draw order (`render_scanline` picks the
    /// tiles/sprites order from the background-priority bit), so no priority
    /// buffer is used.
    ///
    /// Entries are still visited in list order within the row, so the pixel a
    /// row ends up with is the one the whole-frame pass produced from the same
    /// sprite RAM: only the *moment* the RAM is read has moved.
    ///
    /// The list is read as of this row. The hardware's line-object RAM is filled
    /// during the *previous* line and displayed on this one, so a sprite whose
    /// entry changes mid-screen would appear one row early here. That one-line
    /// lead is deliberately not modelled: `sy - 13` is a position constant, and
    /// the epic's W3 note warns that folding the lead in on top of a constant
    /// that already carries it doubles the delay. Tracked separately.
    fn render_sprites_scanline(&mut self, y: usize) {
        let sprite_ram = self.map.region_data(Region::SpriteRam);
        let sprite_cache = &self.sprite_cache;
        // As for tiles: no entries, no pixels to index.
        let sprite_count = sprite_cache.count();
        if sprite_count == 0 {
            return;
        }
        let sprite_bank = self.sprite_bank;
        let row_off = y * NATIVE_WIDTH;
        let row = &mut self.pixel_buffer[row_off..row_off + NATIVE_WIDTH];
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
            let py = y as i32 - sy;
            if !(0..16).contains(&py) {
                continue;
            }
            let code = ((255 ^ code_raw) as usize + 256 * sprite_bank as usize) % sprite_count;

            gfx::draw_sprite_row_indexed(
                sprite_cache,
                code as u16,
                py as usize,
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

    /// Convert the indexed pixel buffer to native (unrotated) RGB24.
    ///
    /// Each row resolves against the palette that was live when the beam drew
    /// it, from [`palette_scanline`](Self::palette_scanline), rather than
    /// against one palette for the whole frame.
    ///
    /// The 270° CW rotation Q*Bert's cabinet needs is declared via
    /// [`orientation`](Self::orientation) and applied centrally by the frontend,
    /// so this emits pixels in native row-major order.
    pub fn render_frame(&self, buffer: &mut [u8]) {
        gfx::resolve_indexed_rows(
            &self.pixel_buffer,
            NATIVE_WIDTH,
            |y| &self.palette_scanline[y][..],
            buffer,
        );
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

    /// One line of live clock rates, for the frame overlay.
    pub fn clock_summary(&self) -> String {
        self.sound.clocks.overlay_summary()
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
    use phosphor_core::core::save_state::{Saveable, StateReader, StateWriter};

    // -----------------------------------------------------------------------
    // Per-scanline palette
    // -----------------------------------------------------------------------

    /// Write palette entry 1 through the real palette-RAM path, so these tests
    /// exercise the DAC expansion rather than poking `palette_rgb`.
    ///
    /// Even byte is G[7:4] B[3:0]; odd byte is R[3:0]. `0x0F, 0x00` is full
    /// blue, `0xF0, 0x00` is full green.
    fn set_entry1(board: &mut GottliebBoard, even: u8, odd: u8) -> (u8, u8, u8) {
        board.update_palette(2, even);
        board.update_palette(3, odd);
        board.palette_rgb[1]
    }

    fn row_pixel(rgb: &[u8], y: usize) -> (u8, u8, u8) {
        let off = y * NATIVE_WIDTH * 3;
        (rgb[off], rgb[off + 1], rgb[off + 2])
    }

    /// The behaviour W1 exists for: a palette write partway down the screen
    /// colours only the rows below it.
    ///
    /// Driven through `begin_scanline` rather than the CPU so the split lands on
    /// an exact, stated row. The companion test below is what proves the frame
    /// loop actually calls `begin_scanline`; without it this one would pass on a
    /// board that never sampled anything.
    #[test]
    fn a_mid_frame_palette_write_colours_only_the_rows_below_it() {
        const SPLIT: usize = 100;
        let mut board = GottliebBoard::new();

        let blue = set_entry1(&mut board, 0x0F, 0x00);
        for y in 0..SPLIT as u64 {
            board.begin_scanline(y);
        }
        let green = set_entry1(&mut board, 0xF0, 0x00);
        for y in SPLIT as u64..TIMING.total_scanlines {
            board.begin_scanline(y);
        }
        assert_ne!(blue, green, "the two palettes must differ for this to test");

        board.pixel_buffer.fill(1);
        let mut rgb = vec![0u8; NATIVE_WIDTH * NATIVE_HEIGHT * 3];
        board.render_frame(&mut rgb);

        assert_eq!(row_pixel(&rgb, 0), blue, "row 0 is above the write");
        assert_eq!(
            row_pixel(&rgb, SPLIT - 1),
            blue,
            "the last row above the write keeps the old colour"
        );
        assert_eq!(
            row_pixel(&rgb, SPLIT),
            green,
            "the first row below the write takes the new colour"
        );
        assert_eq!(
            row_pixel(&rgb, NATIVE_HEIGHT - 1),
            green,
            "the bottom row is below the write"
        );
    }

    /// `render_frame` reading the snapshot is only half of it: the frame loop
    /// has to fill the snapshot. This fails if `begin_scanline` is ever dropped
    /// from `tick` or `run_scanlines`, which would leave every row resolving
    /// against the constructor's black palette.
    #[test]
    fn the_frame_loop_samples_the_palette_at_scanline_boundaries() {
        let mut board = GottliebBoard::new();
        let mut cpu = I8088::new();

        let blue = set_entry1(&mut board, 0x0F, 0x00);
        assert_ne!(
            board.palette_scanline[0][1], blue,
            "the snapshot starts stale, so the sampling below is what changes it"
        );

        // One cycle at clock 0 crosses the scanline-0 boundary.
        tick(&mut cpu, &mut board);
        assert_eq!(board.palette_scanline[0][1], blue, "tick() samples");

        // And the hoisted loop the frame actually runs through samples too.
        let rest = TIMING.cycles_per_scanline - board.clock;
        for _ in 0..rest {
            tick(&mut cpu, &mut board);
        }
        let green = set_entry1(&mut board, 0xF0, 0x00);
        run_scanlines(&mut cpu, &mut board, TIMING.cycles_per_scanline);
        assert_eq!(
            board.palette_scanline[1][1], green,
            "run_scanlines() samples"
        );
        assert_eq!(
            board.palette_scanline[0][1], blue,
            "and sampling scanline 1 must not disturb scanline 0"
        );
    }

    /// Vblank has no row to colour, and writing one would run off the end of a
    /// snapshot sized to the visible area.
    #[test]
    fn vblank_scanlines_have_no_row_to_sample_into() {
        let mut board = GottliebBoard::new();
        assert_eq!(board.palette_scanline.len(), VISIBLE_LINES as usize);
        for y in VISIBLE_LINES..TIMING.total_scanlines {
            board.begin_scanline(y);
        }
    }

    /// A load restores `palette_rgb` but not the per-row snapshot, so the first
    /// frame after one would resolve its upper rows against whatever the
    /// previous machine had. Seeding closes that.
    #[test]
    fn a_load_seeds_every_row_from_the_restored_palette() {
        let mut board = GottliebBoard::new();
        let blue = set_entry1(&mut board, 0x0F, 0x00);
        board.palette_scanline.fill([(9, 9, 9); 16]); // stale from another machine

        board.restore_after_load();

        assert!(
            board.palette_scanline.iter().all(|row| row[1] == blue),
            "every row seeds from the restored palette"
        );
    }

    // -----------------------------------------------------------------------
    // Per-scanline tiles, sprites and layer order (W4)
    // -----------------------------------------------------------------------

    /// Fill char-RAM tile `code` solid with pen `pen`. `gfxcharlo`/`gfxcharhi`
    /// are false on a bare board, so every map code resolves out of this cache.
    fn solid_char_tile(board: &mut GottliebBoard, code: usize, pen: u8) {
        for py in 0..8 {
            for px in 0..8 {
                board.charram_cache.set_pixel(code, px, py, pen);
            }
        }
    }

    /// Give the board a solid 16×16 sprite in code 0 and park it at the top-left
    /// of the visible area, covering rows 0..16.
    fn solid_sprite0(board: &mut GottliebBoard, pen: u8) {
        board.sprite_cache = gfx::GfxCache::new(1, 16, 16);
        for py in 0..16 {
            for px in 0..16 {
                board.sprite_cache.set_pixel(0, px, py, pen);
            }
        }
        let ram = board.map.region_data_mut(Region::SpriteRam);
        ram[0] = 13; // sy - 13 == 0
        ram[1] = 4; //  sx -  4 == 0
        ram[2] = 255; // 255 ^ 255 == code 0
    }

    /// The behaviour W4 exists for on the tilemap layer: video RAM is read as
    /// the beam passes it, so rewriting the map partway down the screen changes
    /// only the rows below the write.
    ///
    /// The split is deliberately at row 100, which is *inside* tile row 12 (rows
    /// 96..103). A whole-frame render draws a tile row from one snapshot and
    /// could not produce this picture at all.
    #[test]
    fn a_mid_frame_vram_write_changes_only_the_rows_below_it() {
        const SPLIT: usize = 100;
        let mut board = GottliebBoard::new();
        solid_char_tile(&mut board, 1, 1);
        solid_char_tile(&mut board, 2, 2);

        board.map.region_data_mut(Region::VideoRam).fill(1);
        for y in 0..SPLIT as u64 {
            board.begin_scanline(y);
        }
        board.map.region_data_mut(Region::VideoRam).fill(2);
        for y in SPLIT as u64..VISIBLE_LINES {
            board.begin_scanline(y);
        }

        let px = |y: usize| board.pixel_buffer[y * NATIVE_WIDTH];
        assert_eq!(px(0), 1, "row 0 was drawn before the write");
        assert_eq!(
            px(SPLIT - 1),
            1,
            "the last row above the write keeps the old tile, mid-tile-row"
        );
        assert_eq!(
            px(SPLIT),
            2,
            "the first row below the write takes the new tile"
        );
        assert_eq!(px(NATIVE_HEIGHT - 1), 2, "the bottom row is below it");
    }

    /// Layer order is a live register on this board (`video_control` bit 0), and
    /// per-scanline rendering makes it per-row. Rows above a mid-screen write
    /// composite in the old order, rows below in the new one.
    #[test]
    fn a_mid_frame_layer_order_write_swaps_priority_only_below_it() {
        const SPLIT: usize = 8;
        let mut board = GottliebBoard::new();
        solid_char_tile(&mut board, 1, 1);
        solid_sprite0(&mut board, 2);
        board.map.region_data_mut(Region::VideoRam).fill(1);

        // Normal order: tiles first, so the sprite wins where they overlap.
        board.video_control = 0;
        for y in 0..SPLIT as u64 {
            board.begin_scanline(y);
        }
        // Background priority: sprites first, so the tile covers them.
        board.video_control = 1;
        for y in SPLIT as u64..16 {
            board.begin_scanline(y);
        }

        let px = |y: usize| board.pixel_buffer[y * NATIVE_WIDTH];
        assert_eq!(px(0), 2, "sprite over tile above the write");
        assert_eq!(px(SPLIT - 1), 2, "still sprite on the last row above it");
        assert_eq!(px(SPLIT), 1, "tile over sprite from the write down");
        assert_eq!(px(15), 1, "and to the bottom of the sprite");
    }

    /// `render_frame` resolving a per-row buffer is only half of it: the frame
    /// loop has to *draw* the rows. This fails if `begin_scanline` stops
    /// compositing, or is dropped from `tick`/`run_scanlines`, which would leave
    /// the picture at whatever the buffer last held.
    #[test]
    fn the_frame_loop_draws_rows_at_scanline_boundaries() {
        let mut board = GottliebBoard::new();
        let mut cpu = I8088::new();
        solid_char_tile(&mut board, 1, 1);
        board.map.region_data_mut(Region::VideoRam).fill(1);
        board.pixel_buffer.fill(0xFF);

        // One cycle at clock 0 crosses the scanline-0 boundary.
        tick(&mut cpu, &mut board);
        assert_eq!(board.pixel_buffer[0], 1, "tick() draws row 0");
        assert_eq!(
            board.pixel_buffer[NATIVE_WIDTH], 0xFF,
            "and only row 0: nothing has drawn row 1 yet"
        );

        // And the hoisted loop the frame actually runs through draws too.
        let rest = TIMING.cycles_per_scanline - board.clock;
        for _ in 0..rest {
            tick(&mut cpu, &mut board);
        }
        board.map.region_data_mut(Region::VideoRam).fill(2);
        solid_char_tile(&mut board, 2, 2);
        run_scanlines(&mut cpu, &mut board, TIMING.cycles_per_scanline);
        assert_eq!(
            board.pixel_buffer[NATIVE_WIDTH],
            2,
            "run_scanlines() draws row 1"
        );
        assert_eq!(
            board.pixel_buffer[0], 1,
            "and drawing row 1 must not disturb row 0"
        );
    }

    /// Vblank has no row to draw, and drawing one would run off the end of a
    /// framebuffer sized to the visible area.
    #[test]
    fn vblank_scanlines_have_no_row_to_draw() {
        let mut board = GottliebBoard::new();
        solid_char_tile(&mut board, 1, 1);
        board.map.region_data_mut(Region::VideoRam).fill(1);
        board.pixel_buffer.fill(0xFF);
        for y in VISIBLE_LINES..TIMING.total_scanlines {
            board.begin_scanline(y);
        }
        assert!(
            board.pixel_buffer.iter().all(|&p| p == 0xFF),
            "vblank drew nothing"
        );
    }

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

    /// The debugger shows the Votrax's live rate, and it moves when the game
    /// retunes the VCO.
    ///
    /// This is the question the epic set out to make answerable: "is the sound
    /// chip running at the right rate?" used to mean reading the constructor,
    /// and after a speech-clock DAC write the constructor is not even the right
    /// answer any more.
    #[test]
    fn the_debugger_shows_the_live_votrax_rate() {
        let mut snd = GottliebSoundBoard::new();
        let speech_hz = |snd: &GottliebSoundBoard| {
            snd.debug_registers()
                .into_iter()
                .find(|r| r.name == "SPEECH_HZ")
                .expect("the debug panel lists the speech domain")
                .value
        };
        assert_eq!(speech_hz(&snd), VOTRAX_NOMINAL_CLOCK_HZ);

        Bus::write(&mut snd, BusMaster::Cpu(1), 0x3000, 0xC0);
        assert_eq!(speech_hz(&snd), convert_speech_clock(0xC0));
        assert_ne!(convert_speech_clock(0xC0), VOTRAX_NOMINAL_CLOCK_HZ);

        // The sound CPU's own domain is listed beside it, at the rate the
        // crystal gives rather than the rounded one the board used to run.
        let regs = snd.debug_registers();
        let sndcpu = regs.iter().find(|r| r.name == "SNDCPU_HZ").expect("listed");
        assert_eq!(sndcpu.value, 894_886);
    }

    #[test]
    fn the_overlay_names_every_domain_and_its_rate() {
        let mut board = GottliebBoard::new();
        let line = board.clock_summary();
        assert_eq!(
            line,
            "cpu:5.000MHz pixel:5.000MHz soundcpu:894.9kHz speech:950.0kHz"
        );

        // And it follows a retune, which is the point of showing it live.
        Bus::write(&mut board.sound, BusMaster::Cpu(1), 0x3000, 0x80);
        assert!(
            board.clock_summary().contains("speech:774.0kHz"),
            "overlay did not follow the VCO: {}",
            board.clock_summary()
        );
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
