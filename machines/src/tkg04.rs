//! Nintendo TKG-04 board (Z80 + I8035 + DMA), shared by Donkey Kong, Donkey
//! Kong Jr. and Mario Bros.
//!
//! # Schematics
//!
//! All three games are drawn, in three packages that share almost no
//! conventions with each other. Their sound sections are three different
//! designs rather than revisions of one, which is the fact this section mainly
//! exists to record: see "Sound is per game" below before assuming anything in
//! [`crate::dkong_sound`] generalizes.
//!
//! | Game | Drawing | Source | Page |
//! |---|---|---|---|
//! | Donkey Kong | `TKG4-14-CPU`, sheet 3 | [dk-tkg4u.pdf] | pp29-30, read |
//! | Donkey Kong | `TKG4-14-VIDEO`, sheet 4 | [dk-tkg4u.pdf] | pp31-32, unread |
//! | DK Jr. | `Donkey Kong Junior CPU P.C. Board`, sheet 5 | [dkjr.pdf] | pp30-31, read |
//! | DK Jr. | `Donkey Kong Junior Video P.C. Board`, sheet 4 | [dkjr.pdf] | pp28-29, unread |
//! | Mario Bros. | `TMA1-CPU` | [marioborspak.pdf] | p39, read |
//! | Mario Bros. | `TMA1-VIDEO` | [marioborspak.pdf] | p40, unread |
//!
//! Read 2026-08-30. Every package also carries power supply sheets, which are
//! omitted here because nothing on this board needs them.
//!
//! HOW THE SCANS ARE LAID OUT, because no two agree and each cost a pass.
//!
//! - Donkey Kong (pp23-32) and Donkey Kong Jr. (pp24-31) are Nintendo Co. Ltd.
//!   drawings with numbered sheets, 300 dpi 1-bit and no text layer, so nothing
//!   in them is searchable. Their large sheets are each cut across two PDF
//!   pages, left half then right half, so a sheet number and a PDF page never
//!   agree.
//! - Donkey Kong's contents page lists two video monitor sheets, "20-EZV" (5)
//!   and "20-EZV(R-B)" (6), that are not in its file. Donkey Kong Jr.'s package
//!   does carry the monitor, as its sheet 3 at pp26-27.
//! - Mario Bros. is a Nintendo of America drawing with lettered sheets, and
//!   there are two scans. Prefer [marioborspak.pdf]: whole sheets, one per page,
//!   600 dpi. [MarioBros.pdf] is the same drawings at roughly 430 dpi with each
//!   sheet split in two, which puts the walk oscillators (p51) on a different
//!   page from the filter chain they feed (p52).
//!
//! SOUND IS PER GAME. The three boards share an output stage and nothing else.
//!
//! - Donkey Kong: analog tone sources. An NE556 dual timer (R42 47 kΩ / R43
//!   27 kΩ / C28 33 nF, and R40 47 kΩ / R41 27 kΩ / C27 47 nF), two 4049
//!   inverter oscillators, and envelope networks on Q1-Q7 2SC1815 with 1S553
//!   steering diodes. Music through a DAC-08 at 8K off an MB8884 (8035) at 7H,
//!   command latch LS75 x2 at 4H/4F, two 2716 at 3H/3F. This is the drawing
//!   behind the 555 constants in [`crate::dkong_sound`].
//! - Donkey Kong Jr.: digital tone sources. No NE556 and no 4049 anywhere. Two
//!   74LS629 VCOs (5K and 8L), a 4020 ripple counter at 6L, an LS157 at 6K
//!   selecting counter taps, and an LS123 one-shot at 4K, with only Q1/Q3/Q4
//!   2SC1815 and small RC slewing the control voltages. Same DAC-08 at 8K and
//!   MB8884 at 7H, but one 2732 at 3H behind an LS373 at 3F.
//! - Mario Bros.: 74LS629 again, different circuit. Both halves in one package
//!   at 4K, control voltages slewed by R64 20 kΩ / C43 3.3 µF and R65 10 kΩ /
//!   C44 3.3 µF, timing caps C39 4.7 nF and C40 22 nF, driven by a 4020B at 3H
//!   and a 74123 at 4L (C41 4.7 µF, R61 47 kΩ). Its filter chain is two LM3900
//!   Norton sections at 3M rather than an MB3614, and its music DAC is a
//!   discrete resistor ladder (MXR1 / RM7) off a 374 latch at 3K, not a DAC-08.
//!   These are the walk/skid oscillators [`crate::mario_bros`] currently defers.
//!
//! Common to all three: an MB3712 power amplifier with VR1 10 kΩ into SPEAKER
//! P11, a TV Audio tap, and a dashed box around the amplifier meaning optional
//! parts (note 2 on both Nintendo Co. sheets).
//!
//! COLOR IS SHARED, which is the opposite of the sound and worth stating for
//! that reason: reading one board's color network tells you the other two. The
//! transistor cluster at the top right of the Donkey Kong and Donkey Kong Jr.
//! sheets (C828P, Q15-Q17, VR3/VR4, R104-R112) looks like sound and is not, it
//! is this. Read from Donkey Kong sheet 3, Donkey Kong Jr. sheet 5 and
//! `TMA1-CPU`, all three at the pages in the table above.
//!
//! Identical on all three: resistor ladders of {1 kΩ, 470 Ω, 220 Ω} on the two
//! 3-bit channels and {470 Ω, 220 Ω} on 2-bit blue, biased 470/470/680 Ω, each
//! node through 100 Ω into its amplifier and out through 68 Ω. The amplifiers
//! are deliberately asymmetric: red and green run an NPN into an A564 PNP
//! (Donkey Kong and Donkey Kong Jr. Q9 into Q13 and Q10 into Q14, Mario Bros.
//! Q5 into Q7 and Q8 into Q9) whose base-emitter drops cancel, while blue has
//! the NPN alone (Q11, and Mario Bros. Q6). Blue therefore sits 0.7 V above the
//! other two for its whole range. That pedestal is real and belongs in the
//! model; what cancels it is the monitor's per-channel black-level restoration,
//! which is why `normalize_tkg04_palette` is shared by all three boards.
//!
//! What differs is only the lookup feeding those ladders. The Nintendo Co.
//! boards use two MB7052 256x4 PROMs at 2F and 2E behind a color decoder that
//! forces pens `& 3 == 0` black; Mario Bros. uses one 82S42 512x8 at 4P with no
//! such rule and a different bit order. The Nintendo Co. boards also add a
//! dashed video amplifier block per channel with a 1 kΩ trimmer (VR3/VR4/VR5 on
//! Donkey Kong, VR2/VR3/VR4 on Donkey Kong Jr.); Mario Bros. has none and runs
//! straight to the connector.
//!
//! WHAT THIS DOES NOT ESTABLISH. On none of the three was the path from a
//! main-CPU sound write to an individual effect traced end to end; on Donkey
//! Kong the LS138 at 1C is visible feeding the effect section, but its enable
//! was not followed back. Only the CPU sheets were read, and on the two
//! Nintendo Co. packages only their right halves in detail. Every video sheet,
//! the monitor sheet and every power supply sheet is unread. The sound sections
//! are a parts census rather than a transcription, taken to establish that the
//! three differ, and the values quoted are those legible without tracing a net.
//! The color network was traced far enough to place every resistor and
//! transistor named above, but the PROM address lines were not followed, so
//! which pen a given tile or sprite attribute selects is not established here.
//!
//! [dk-tkg4u.pdf]: https://www.arcade-museum.com/manuals-videogames/D/dk-tkg4u.pdf
//! [dkjr.pdf]: https://www.arcade-museum.com/manuals-videogames/D/DKJr.pdf
//! [marioborspak.pdf]: https://www.arcade-museum.com/manuals-videogames/M/marioborspak.pdf
//! [MarioBros.pdf]: https://www.arcade-museum.com/manuals-videogames/M/MarioBros.pdf

use crate::dkong_sound::DkongDiscreteSound;
use phosphor_core::audio::AudioResampler;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind, DebugTraceBuffer};
use phosphor_core::core::machine::GfxSheet;

use phosphor_core::core::watchpoint::DebugAccessSource;
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{
    Bus, BusMaster, ClockDomainName as Clk, ClockTree, DomainId, TimingConfig,
};
use phosphor_core::cpu::i8035::I8035;
use phosphor_core::cpu::z80::Z80;
use phosphor_core::device::dac::Mc1408Dac;
use phosphor_core::device::i8257::I8257;
use phosphor_core::device::output_latch::OutputLatch;
use phosphor_core::gfx;
use phosphor_core::gfx::decode::{GfxLayout, decode_gfx};
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion, Saveable};

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
    cpu_clock_hz: 3_072_000,  // 61.44 MHz / 5 / 4
    cycles_per_scanline: 192, // 384 pixels / 2
    total_scanlines: 264,     // VTOTAL
    // Native (pre-orientation) framebuffer: the board declares ROT90 and the
    // frontend rotates centrally, so these are the unrotated dimensions.
    display_width: NATIVE_WIDTH as u32,                  // 256
    display_height: (NATIVE_HEIGHT - VBLANK_END) as u32, // 224
    display_aspect: Some((3, 4)),                        // portrait tube as viewed (after ROT90)
};

/// The board's crystals and everything divided out of them.
///
/// Two: a 61.44 MHz master (Z80 at /20, pixel clock at /10) and a 6 MHz crystal
/// on the sound section, whose I8035 machine cycles are its crystal over
/// fifteen. That sound domain reduces to 25/192 against the Z80, which is the
/// ratio [`SOUND_TICK_NUM`]/[`SOUND_TICK_DEN`] states by hand.
pub fn clock_tree() -> phosphor_core::core::ClockTree {
    use phosphor_core::core::RootId;
    let mut t = ClockTree::new(61_440_000);
    let snd = t.add_root(6_000_000);
    let cpu = t.add_domain(Clk::Cpu, RootId::MAIN, 1, 20); // 3.072 MHz
    let dot = t.add_domain(Clk::Pixel, RootId::MAIN, 1, 10); // 6.144 MHz
    t.add_domain(Clk::Mcu, snd, 1, 15); // I8035 machine cycles, 400 kHz
    t.set_step_domain(cpu);
    // Pixel clock is exactly twice the CPU clock off the same crystal, so 384
    // dot clocks is exactly 192 CPU cycles.
    t.set_raster(dot, 384, 0);
    t
}

pub const VISIBLE_LINES: u64 = 240;
pub fn output_sample_rate() -> u64 {
    phosphor_core::audio::host_sample_rate() as u64
}

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
/// with the color-decoder NOR forcing pens `& 0x03 == 0` to black, then hands
/// the result to [`normalize_tkg04_palette`].
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

    normalize_tkg04_palette(&raw, |i| (i & 0x03) == 0x00)
}

/// Set each channel's black level and gain independently, and quantize to 8-bit.
///
/// `forced_black` marks pens a board's color decoder pins to black; those are
/// written as black and excluded from the range, because they are not a channel
/// output and their zeros would otherwise drag every channel's minimum to 0 and
/// defeat the black-level adjustment. A board with no such rule passes
/// `|_| false`.
///
/// # Why per channel and not one global scale
///
/// The three channels do not reach the monitor on a common baseline. Red and
/// green leave an NPN into an A564 PNP whose base-emitter drops cancel (Donkey
/// Kong and Donkey Kong Jr.: Q9 into Q13, Q10 into Q14; Mario Bros.: Q5 into Q7,
/// Q8 into Q9), while blue has only the NPN (Q11, and Mario Bros.' Q6), so blue
/// sits a whole 0.7 V follower drop above the other two along its entire range.
/// That pedestal is in the hardware and [`compute_tkg04_channel`] reproduces it
/// correctly; what removes it is the monitor, which DC-restores each channel to
/// black during the back porch, which is inherently per channel. A single
/// global gain cannot subtract a per-channel offset.
///
/// Donkey Kong is where the difference is visible rather than merely wrong. A
/// pen with every PROM bit inactive came out (4, 4, 56) instead of black, which
/// put a dark blue rectangle over the ladders in attract mode: the board's solid
/// mask sprites, whose whole job is to hide Kong behind the girder as he climbs.
///
/// All three boards carry the same network, so they share this: ladders of
/// {1 kΩ, 470 Ω, 220 Ω} on the two 3-bit channels and {470 Ω, 220 Ω} on the
/// 2-bit blue, biased 470/470/680, into the amplifiers above.
pub(crate) fn normalize_tkg04_palette(
    raw: &[(f64, f64, f64); 256],
    forced_black: impl Fn(usize) -> bool,
) -> [(u8, u8, u8); 256] {
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for (i, &(r, g, b)) in raw.iter().enumerate() {
        if forced_black(i) {
            continue;
        }
        for (channel, v) in [r, g, b].into_iter().enumerate() {
            lo[channel] = lo[channel].min(v);
            hi[channel] = hi[channel].max(v);
        }
    }

    let normalize = |v: f64, channel: usize| -> u8 {
        let span = hi[channel] - lo[channel];
        if span <= 0.0 {
            return 0;
        }
        (((v - lo[channel]) / span) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8
    };

    let mut out = [(0u8, 0u8, 0u8); 256];
    for (i, (o, &(r, g, b))) in out.iter_mut().zip(raw.iter()).enumerate() {
        if forced_black(i) {
            continue;
        }
        *o = (normalize(r, 0), normalize(g, 1), normalize(b, 2));
    }
    out
}

// ---------------------------------------------------------------------------
// Tkg04Board — shared Nintendo TKG/TRS arcade hardware
// ---------------------------------------------------------------------------

/// The two CPUs that share the TKG-04 board, borrowed together.
///
/// Each machine owns them as its own fields — so the debug derive sees one
/// `#[debug_cpu]` per CPU, and the save-state layout is unchanged — and hands
/// them to [`tick`] as a pair alongside the bus they drive.
pub struct Tkg04Cpus<'a> {
    pub main: &'a mut Z80,
    pub sound: &'a mut I8035,
}

impl Tkg04Cpus<'_> {
    /// Bitmask of CPUs at an instruction boundary: bit 0 = main (Z80),
    /// bit 1 = sound (I8035).
    pub fn instruction_boundaries(main: &Z80, sound: &I8035) -> u32 {
        let mut result = 0;
        if main.at_instruction_boundary() {
            result |= 1;
        }
        if sound.at_instruction_boundary() {
            result |= 2;
        }
        result
    }
}

/// A TKG-04 bus: the shared board behind a game's address decoding.
///
/// [`tick`] is generic over this trait, so every access the CPUs make resolves
/// to a direct call rather than a vtable entry.
pub trait Tkg04Bus: Bus<Address = u16, Data = u8> {
    fn board(&mut self) -> &mut Tkg04Board;
}

/// One CPU cycle of a TKG-04 machine: board work, the Z80, then the sound CPU
/// on its own divider, then the audio tail.
///
/// This is the debugger's path — it tests the frame position on every cycle.
/// A whole frame goes through [`run_scanlines`], which hoists that test out.
#[inline]
pub fn tick<B: Tkg04Bus>(cpus: &mut Tkg04Cpus<'_>, bus: &mut B) {
    let board = bus.board();
    let frame_cycle = board.clock % TIMING.cycles_per_frame();
    if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
        board.begin_scanline((frame_cycle / TIMING.cycles_per_scanline) as u16);
    }
    step_cycle(cpus, bus);
}

/// Run `cycles` CPU cycles, scanline-outer and cycle-inner.
///
/// The scanline-boundary work — rendering a line and raising the VBLANK NMI —
/// happens 264 times a frame instead of on each of the 50,688 cycles. The
/// caller must start on a scanline boundary and pass a multiple of
/// `cycles_per_scanline`; the debugger's off-boundary stepping goes through
/// [`tick`] instead.
pub fn run_scanlines<B: Tkg04Bus>(cpus: &mut Tkg04Cpus<'_>, bus: &mut B, cycles: u64) {
    debug_assert!(
        bus.board().clock.is_multiple_of(TIMING.cycles_per_scanline)
            && cycles.is_multiple_of(TIMING.cycles_per_scanline),
        "run_scanlines must start on a scanline boundary and run whole scanlines"
    );
    for _ in 0..cycles / TIMING.cycles_per_scanline {
        let board = bus.board();
        let scanline = board.clock % TIMING.cycles_per_frame() / TIMING.cycles_per_scanline;
        board.begin_scanline(scanline as u16);
        for _ in 0..TIMING.cycles_per_scanline {
            step_cycle(cpus, bus);
        }
    }
}

/// Run one frame's worth of cycles.
///
/// Whole scanlines go through [`run_scanlines`]; any partial scanline at either
/// end — which only happens when the debugger has left the clock off-boundary —
/// goes through [`tick`], so the frame is the same sequence of cycles either
/// way.
pub fn run_frame<B: Tkg04Bus>(cpus: &mut Tkg04Cpus<'_>, bus: &mut B) {
    let scanline = TIMING.cycles_per_scanline;
    let mut remaining = TIMING.cycles_per_frame();

    let lead = ((scanline - bus.board().clock % scanline) % scanline).min(remaining);
    for _ in 0..lead {
        tick(cpus, bus);
    }
    remaining -= lead;

    let whole = remaining - remaining % scanline;
    run_scanlines(cpus, bus, whole);
    remaining -= whole;

    for _ in 0..remaining {
        tick(cpus, bus);
    }
}

/// The part of a cycle with no frame-position test in it.
#[inline]
fn step_cycle<B: Tkg04Bus>(cpus: &mut Tkg04Cpus<'_>, bus: &mut B) {
    bus.board().begin_cycle_inner(cpus);

    cpus.main.execute_cycle(bus, BusMaster::Cpu(0));

    // Sound CPU (Bresenham 25/192 ratio: 400 kHz from 3.072 MHz)
    let sound_due = {
        let board = bus.board();
        board.clocks.tick(board.sound_dom)
    };
    if sound_due {
        cpus.sound.execute_cycle(bus, BusMaster::Cpu(1));
    }

    bus.board().end_cycle();
}

/// Shared hardware for the Nintendo TKG-04 arcade platform.
///
/// Named after the Nintendo PCB designation "TKG-04", the final 2-board
/// Donkey Kong design. The same core hardware (with minor variations) is
/// used by Donkey Kong (TKG-04), Donkey Kong Jr, and Radar Scope (TRS-02).
/// Earlier 4-board sets (TKG-02, TKG-03) are electrically equivalent.
///
/// The board is everything the CPUs talk *to* — they live on the machine, so
/// `cpu.execute_cycle(&mut bus, ..)` is a pair of disjoint field borrows and
/// dispatches at a concrete bus type.
///
/// Hardware: Z80 @ 3.072 MHz (main), I8035 @ 6 MHz (sound).
/// Video: 32×32 tile playfield + 16×16 sprites, 2bpp, PROM palette.
/// Audio: I8035 DAC + discrete circuits (walk, jump, stomp effects).
/// Screen: 256×240 displayed rotated 90° CCW on vertical monitor.
#[derive(BusDebug, DebugTrace, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct Tkg04Board {
    /// Memory maps (page-table dispatch + watchpoints + backing memory).
    ///
    /// CPU-addressable RAM/ROM storage lives in the `AddressSpace16` backing
    /// store, and each space persists its own writable regions: work RAM,
    /// sprite RAM and video RAM on the main map.
    #[debug_map(cpu = 0)]
    #[save(id = 1)]
    pub(crate) main_map: AddressSpace16,
    #[debug_map(cpu = 1)]
    #[save(id = 2)]
    pub(crate) sound_map: AddressSpace16,
    #[save_skip]
    pub(crate) tune_rom: [u8; 0x0800], // 2KB (DK only, unused by DK Jr)

    // GFX ROMs
    #[save_skip]
    pub(crate) tile_rom: [u8; 0x2000], // 8KB max (DK=4KB, DK Jr=8KB)
    #[save_skip]
    pub(crate) sprite_rom: [u8; 0x2000], // 8KB

    // PROMs
    #[save_skip]
    pub(crate) palette_prom: [u8; 0x0200], // c-2k/c-2e + c-2j/c-2f
    #[save_skip]
    pub(crate) color_prom: [u8; 0x0100], // v-5e/v-2n

    /// Pre-computed palette (256 RGB entries), expanded from the PROMs rather
    /// than from anything the CPU writes, so it is rebuilt at ROM load.
    #[save_skip]
    pub(crate) palette_rgb: [(u8, u8, u8); 256],

    // Scanline-rendered framebuffer (256 × 240 × RGB24)
    #[save_skip]
    pub(crate) scanline_buffer: Vec<u8>,

    // I/O state (active-high: 0x00 = all released)
    #[save(id = 3)]
    pub(crate) in0: u8,
    #[save(id = 4)]
    pub(crate) in1: u8,
    #[save(id = 5)]
    pub(crate) in2: u8,
    /// Operator configuration, as it was before: not part of the snapshot.
    #[save_skip]
    pub(crate) dsw0: u8,

    // Control registers
    #[save(id = 6)]
    pub(crate) sound_latch: u8,
    #[save(id = 7)]
    pub(crate) sound_control_latch: OutputLatch,
    #[save(id = 8)]
    pub(crate) flip_screen: bool,
    #[save(id = 9)]
    pub(crate) sprite_bank: bool,
    #[save(id = 10)]
    pub(crate) nmi_mask: bool,
    #[save(id = 11)]
    pub(crate) palette_bank: u8,

    // DK Jr extras (always 0 for DK)
    #[save(id = 12)]
    pub(crate) gfx_bank: u8,
    #[save(id = 13)]
    pub(crate) sound_control_latch_4h: OutputLatch,

    // Pre-decoded GFX caches (from tile_rom / sprite_rom)
    #[save_skip]
    pub(crate) tile_cache: gfx::GfxCache,
    #[save_skip]
    pub(crate) sprite_cache: gfx::GfxCache,

    // Configuration (set at construction, not saved)
    #[save_skip]
    tile_plane1_offset: usize, // 0x800 for DK (4KB tiles), 0x1000 for DK Jr (8KB)

    // DMA controller (i8257)
    #[debug_device("DMA")]
    #[save(id = 14)]
    pub(crate) dma: I8257,

    // Sound CPU interface
    #[save(id = 15)]
    pub(crate) sound_irq_pending: bool,

    /// Mirror of the sound CPU's P1/P2 port latches, refreshed at the top of
    /// every cycle from the CPU itself.
    ///
    /// The ports are hardware wires the *bus* has to answer for -- the sound
    /// CPU reads its own ports back through `io_read`, and the main CPU reads
    /// P2 bit 4 as a sound-busy status bit -- but the CPU that owns them now
    /// lives outside the bus. Derived state, so it is not saved: the next
    /// cycle re-latches it before anything can read it.
    #[save_skip]
    pub(crate) sound_p1: u8,
    #[save_skip]
    pub(crate) sound_p2: u8,

    // Audio output
    #[debug_device("DAC")]
    #[save(id = 16)]
    pub(crate) dac: Mc1408Dac,
    #[save(id = 17)]
    pub(crate) resampler: AudioResampler<i16>,

    // Timing
    #[save(id = 18)]
    pub(crate) clock: u64,
    /// The board's clock tree, as [`clock_tree`] declares it.
    #[debug_device("Clocks")]
    #[save(id = 19)]
    pub(crate) clocks: ClockTree,
    #[save_skip]
    pub(crate) sound_dom: DomainId,
    #[save(id = 20)]
    pub(crate) vblank_nmi_pending: bool,

    // Discrete sound: DAC stream + walk/jump/stomp effects, mixed in-circuit.
    #[debug_device("Discrete")]
    #[save(id = 21)]
    pub(crate) sound: DkongDiscreteSound,

    // Debug event ring (observer state — never saved in save states)
    #[debug_events]
    #[save_skip]
    pub(crate) debug_trace: DebugTraceBuffer,
}

impl Tkg04Board {
    /// Create a new board with the given tile ROM plane-1 offset.
    ///
    /// - DK: `tile_plane1_offset = 0x800` (4KB tile ROM)
    /// - DK Jr: `tile_plane1_offset = 0x1000` (8KB tile ROM)
    pub fn new(tile_plane1_offset: usize) -> Self {
        let clocks = clock_tree();
        let sound_dom = clocks.find(Clk::Mcu).expect("declared I8035 domain");
        Self {
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
            sound_p1: 0,
            sound_p2: 0,
            dac: Mc1408Dac::new(),
            resampler: AudioResampler::new(TIMING.cpu_clock_hz, output_sample_rate()),
            clock: 0,
            clocks,
            sound_dom,
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

                // The code space is 0..=255, but the sprite ROM is 8 KB — 128
                // sprites of 16x16 2bpp — so the top bit addresses a line that
                // is not there. Wrapping models an unconnected address line:
                // the ROM sees the low bits and repeats. Without this a sprite
                // with attribute bit 0x40 set indexes past the cache and panics,
                // which dkong reaches after a reset mid-session.
                let spr_code = ((code_byte & 0x7F) as u16 | (((attr_byte & 0x40) as u16) << 1))
                    % sprite_cache.count().max(1) as u16;
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

    /// Work that only happens on the first cycle of a scanline: rendering the
    /// line, and the VBLANK NMI edges.
    ///
    /// Called once per scanline from [`run_scanlines`] and, for the debugger's
    /// single-step path, from [`tick`] when the clock lands on a boundary.
    fn begin_scanline(&mut self, scanline: u16) {
        // Per-scanline rendering
        if scanline < VISIBLE_LINES as u16 {
            self.render_scanline(scanline as usize);
        }

        // VBLANK NMI: assert at scanline 240
        if scanline == VISIBLE_LINES as u16 {
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
        if scanline == 0 && self.clock > 0 {
            self.vblank_nmi_pending = false;
        }
    }

    /// Per-cycle board work that runs before the CPUs, with no frame-position
    /// test in it.
    fn begin_cycle_inner(&mut self, cpus: &Tkg04Cpus<'_>) {
        // Mirror the sound CPU's port latches. The bus has to answer for them —
        // the sound CPU reads its own ports back, and the main CPU reads P2 bit
        // 4 as a sound-busy bit — but the CPU that owns them is outside the bus.
        // Sampling here is faithful: only the sound CPU changes these, and it
        // steps later in this same cycle.
        self.sound_p1 = cpus.sound.p1;
        self.sound_p2 = cpus.sound.p2;

        // Latch debug attribution context (cycle + instruction PC) before
        // CPU execution — bus dispatch cannot read CPU state mid-tick.
        // (sound_map has no watchpoint hooks in bus dispatch yet.)
        if self.main_map.has_any_watchpoints() || self.debug_trace.enabled() {
            let pc = cpus
                .main
                .at_instruction_boundary()
                .then_some(cpus.main.pc as u32);
            self.main_map.latch_access_context(self.clock, pc);
        }
    }

    /// Board work after the CPUs' cycle: the audio tail and the clock advance.
    fn end_cycle(&mut self) {
        // Box-filter the DAC (3.072 MHz → 44.1 kHz); each produced sample drives
        // one step of the discrete circuit, which sums it with the effects.
        if let Some(dac_avg) = self.resampler.tick_sample(self.dac.sample_i16()) {
            // P2 bit 7 is the DAC's signal-decay line. The sound CPU drops it
            // when a sample finishes, and the board fades the DAC out over a
            // ~100 ms time constant rather than cutting it — so the tail of
            // every sound decays instead of ending on a step. Leaving it undriven
            // is audible as clicks where the steps land and silence between.
            self.sound.set_discharge(self.sound_p2 & 0x80 == 0);
            self.sound.feed_dac(dac_avg);
        }

        self.clock += 1;
    }

    // -----------------------------------------------------------------------
    // Frame rendering (rotation)
    // -----------------------------------------------------------------------

    /// Copy the visible native raster (256w × 224h RGB24, VBLANK scanlines 0-15
    /// clipped) into the output buffer in native row-major order.
    ///
    /// The 90° rotation the cabinet needs is declared via
    /// [`orientation`](Self::orientation) and applied centrally by the frontend,
    /// so this emits pixels unrotated.
    pub fn render_frame(&self, buffer: &mut [u8]) {
        buffer.copy_from_slice(&self.scanline_buffer[VBLANK_END * NATIVE_WIDTH * 3..]);
    }

    /// The Donkey Kong / TKG-04 monitor is mounted rotated 90°. The orientation
    /// is declarative — the frontend rotates `render_frame`'s native output.
    pub fn orientation(&self) -> phosphor_core::core::machine::Orientation {
        phosphor_core::core::machine::Orientation::ROT90
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

    /// Reset board state (does not reset the CPUs — the machine owns them and
    /// resets them against its bus view over this board).
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
        self.clocks.reset();
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
