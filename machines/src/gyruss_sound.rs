//! Gyruss sound/IO board (Konami, 1983).
//!
//! Self-contained audio Z80 + 5xAY-8910 + 8039 SFX MCU as documented in
//! `docs/schematics/gyruss-sound.md`. The main board writes a command byte
//! into latch 1 (a 0xC100 write) and pulses the audio IRQ (a 0xC080 write);
//! the audio program reads the latch at 0x8000, which acknowledges the IRQ.
//!
//! # Audio Z80 memory map (MAME `audio_cpu1_map`)
//!
//! | Address       | R/W | Description                              |
//! |---------------|-----|------------------------------------------|
//! | 0x0000-0x3FFF | R   | Sound ROM (2x8K; 0x4000-0x5FFF empty)    |
//! | 0x6000-0x63FF | R/W | RAM (1 KB)                               |
//! | 0x8000        | R   | Latch 1 (command; read acks the IRQ)     |
//!
//! # Audio Z80 IO map (MAME `audio_cpu1_io_map`, mask 0xFF)
//!
//! Five AY-8910 triples (address/read/write) at 0x00/0x04/0x08/0x0C/0x10,
//! 0x14 asserts the 8039 INT, 0x18 writes latch 2 (audio -> 8039).
//!
//! AY3 (index 2) port A reads a divide-by-10240 timer off the audio clock;
//! AY1/AY2 (indexes 0/1) port B selects per-channel RC filter caps. Chips 1
//! and 2 voice through their filters (right and left); chips 3-5 sum through
//! 3.3K legs (3 and 4 right, 5 left) with the 8039 DAC joining the left mix.

use phosphor_core::audio::{AudioResampler, DcBlocker, host_sample_rate};
use phosphor_core::core::debug::{DebugRegister, Debuggable};
use phosphor_core::core::{AccessKind, AddressSpace16, Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::i8035::I8035;
use phosphor_core::cpu::z80::Z80;
use phosphor_core::device::Ay8910;
use phosphor_macros::{BusDebug, MemoryRegion, Saveable};

use phosphor_core::device::Device;

/// Debug index of the audio Z80: main is 0, sub is 1, so the sound board's
/// CPUs are 2 and 3. Same fact `#[debug_map(cpu = 2)]` below states.
pub(crate) const SOUND_CPU_INDEX: usize = 2;
/// Debug index of the 8039 SFX MCU.
pub(crate) const MCU_INDEX: usize = 3;

/// Regions of the sound address space.
///
/// The 8039's program ROM lives at 0xC000 in this map, not at the 0x0000 the
/// MCU addresses: both CPUs use address 0, so the bus translates the MCU
/// view up by 0xC000 (see [`GyrussSoundBus::mcu_read`]).
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum Region {
    SndRom = 1,
    SndRam = 2,
    McuRom = 3,
}

/// The two switchable capacitors on each filtered voice, in port-B bit order:
/// bit 0 switches the 0.047 uF mylar (C38/C41/C45, C42/C46/C48), bit 1 the
/// 0.22 uF tantalum (C39/C40/C44, C43/C47/C49). Traced on the sheet from each
/// 4066 control pin back to its port-B pin.
const FILTER_CAPS: [f64; 2] = [0.047e-6, 0.22e-6];

/// One filtered AY voice: the channel drives a 1K series resistor into a node
/// that the 4066s can hang either capacitor (or both) on, and the node meets
/// the bus through a 2.2K leg.
///
/// The node is a divider with a pole. With no capacitor switched in it sits at
/// `x * R_FILT_LEG / (R_FILT_SRC + R_FILT_LEG)`, and with capacitance `C` it
/// relaxes toward that value with `tau = C * (R_FILT_SRC || R_FILT_LEG)`, the
/// Thevenin resistance the capacitor sees with the bus taken as ground (it
/// sits within a few hundred millivolts of it behind the 200-ohm VR).
///
/// Each capacitor keeps its own charge. A 4066 that opens leaves its
/// capacitor holding the last node voltage, and closing it again shares that
/// charge with whatever else is on the node. Dropping the charge on deselect
/// would put a step into the voice every time the program reselects a cap.
#[derive(Clone, Copy, Debug, Saveable)]
#[save_version(1)]
#[save_tlv]
struct VoiceFilter {
    /// The 2-bit port-B select: bit 0 = 0.047 uF, bit 1 = 0.22 uF.
    #[save(id = 1)]
    cap: u8,
    /// Voltage held on each capacitor, in [`FILTER_CAPS`] order.
    #[save(id = 2)]
    held: [f32; 2],
}

impl VoiceFilter {
    const fn new() -> Self {
        Self {
            cap: 0,
            held: [0.0; 2],
        }
    }

    /// Advance the node one output sample with the channel at `x` volts and
    /// return the node voltage.
    fn process(&mut self, x: f32) -> f32 {
        let target = x * FILT_NODE_GAIN;
        let sel = self.cap & 3;
        if sel == 0 {
            return target;
        }
        let mut c = 0.0f64;
        let mut q = 0.0f64;
        for (i, &ci) in FILTER_CAPS.iter().enumerate() {
            if sel & (1 << i) != 0 {
                c += ci;
                q += ci * f64::from(self.held[i]);
            }
        }
        let v0 = (q / c) as f32;
        let r_th = f64::from(R_FILT_SRC * R_FILT_LEG / (R_FILT_SRC + R_FILT_LEG));
        let fs = host_sample_rate() as f64;
        let decay = (-1.0 / (r_th * c * fs)).exp() as f32;
        let v = target + (v0 - target) * decay;
        for i in 0..2 {
            if sel & (1 << i) != 0 {
                self.held[i] = v;
            }
        }
        v
    }
}

/// The 0.1 uF from each LA4460 input (the VR wiper) to ground, C36 and C37,
/// driven by the bus's own Thevenin resistance. With the wiper at the top that
/// resistance is the whole bus in parallel, about 134 ohms right and 145
/// left, so the corner lands near 11-12 kHz.
#[derive(Clone, Copy, Debug, Saveable)]
#[save_version(1)]
#[save_tlv]
struct WiperCap {
    #[save(id = 1)]
    v: f32,
}

impl WiperCap {
    #[inline]
    fn process(&mut self, x: f32, g_bus: f32) -> f32 {
        let fs = host_sample_rate() as f64;
        let tau = C_WIPER / f64::from(g_bus);
        let k = (1.0 - (-1.0 / (tau * fs)).exp()) as f32;
        self.v += (x - self.v) * k;
        self.v
    }
}

/// Gyruss sound board: audio Z80, 8039 MCU, and the bus they share.
///
/// `BusDebug` is derived here (not on the main board) because both CPUs are
/// fields of this struct; the machine merges this tree in with `#[debug_bus]`
/// on its own `sound` field.
#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct GyrussSound {
    // `index = 2/3` spell the CPU-index consts as literals, which is what
    // the attribute parser takes.
    #[debug_cpu("Z80 Sound", index = 2)]
    #[save(id = 1)]
    cpu: Z80,
    #[debug_cpu("8039 SFX", index = 3)]
    #[save(id = 2)]
    mcu: I8035,
    /// Everything both sound CPUs talk to. Held apart from the CPUs so each
    /// cycle dispatches at a concrete bus rather than a trait object; see
    /// `docs/designs/concrete-bus-dispatch.md`.
    #[debug_bus]
    #[save(id = 3)]
    bus: GyrussSoundBus,
}

/// The sound CPUs' bus: PSGs, memory, the two command latches, the DAC, the
/// filters, and the stereo mix.
#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
struct GyrussSoundBus {
    /// The five PSGs in IO order: AY1/AY2 carry the filter selects on port B,
    /// AY3 the timer on port A. All deliver per-channel outputs so each
    /// channel meets its own filter or mixer leg.
    #[save(id = 1)]
    ay: [Ay8910; 5],

    /// Sound ROM/RAM plus the 8039 ROM (offset to 0xC000; see [`Region`]).
    /// This is what carries watchpoints and the write-event ring.
    #[debug_map(cpu = 2)]
    #[save(id = 2)]
    map: AddressSpace16,

    /// Latch 1: command from the main board (audio Z80 reads at 0x8000).
    #[save(id = 3)]
    command: u8,
    /// Latch 2: sound data from the audio Z80 (8039 reads over BUS).
    #[save(id = 4)]
    latch2: u8,

    // IRQ generation (MAME HOLD_LINE-style: set by the writer, cleared when
    // the reader consumes the latch).
    #[save(id = 5)]
    irq_pending: bool,
    #[save(id = 6)]
    mcu_irq_pending: bool,

    /// Current 8039 P1 output: the R-2R DAC value, sampled per mixed frame.
    #[save(id = 7)]
    dac: u8,
    /// 8039 P1/P2 output latches, readable back via IN. The bus holds them
    /// because the core reads ports through the bus, not its own fields.
    #[save(id = 8)]
    p1: u8,
    #[save(id = 9)]
    p2: u8,

    /// Last port-B filter selects (AY1 then AY2), for debug/state.
    #[save(id = 10)]
    filter_sel: [u8; 2],
    /// Six RC voices: AY1 channels 0-2 (right) then AY2 channels 0-2 (left).
    #[save(id = 11)]
    filters: [VoiceFilter; 6],

    /// Stereo mix queues. Pushed samples bypass conversion (they arrive at
    /// the host rate, one frame per AY emission), so these are queues in
    /// resampler clothing; the input rate below is the AY clock they drain.
    #[save(id = 12)]
    left: AudioResampler<i16>,
    #[save(id = 13)]
    right: AudioResampler<i16>,
    /// DC blockers for the stereo pair. The wiper reaches LA4460 pin 2 with
    /// no series capacitor; what removes the unipolar sources' DC is the
    /// amplifier's 100 uF feedback capacitor on pin 6 (C26, C30), against an
    /// internal resistor the sheet does not give, so the corner is the shared
    /// 20 Hz default. (The 1000 uF C25/C29 are pin 10 supply decoupling on
    /// the +12V line, not output coupling.)
    #[save(id = 14)]
    dc_left: DcBlocker,
    #[save(id = 15)]
    dc_right: DcBlocker,
    /// C37 (left bus, VR2) and C36 (right bus, VR1) at the amp inputs.
    #[save(id = 19)]
    wiper_left: WiperCap,
    #[save(id = 20)]
    wiper_right: WiperCap,

    /// Audio-CPU cycles elapsed (drives the AY3 timer).
    #[save(id = 16)]
    clock: u64,
    /// Bresenham accumulator stepping the 8039's machine cycles off the audio
    /// clock: `acc += 32_000_000` per audio cycle, one machine cycle per
    /// [`MCU_STEP`].
    #[save(id = 17)]
    acc8039: u64,
    /// Divide-by-2 phase: the AYs run at half the audio clock (1.79 MHz).
    #[save(id = 18)]
    ay_phase: bool,
}

/// The accumulator threshold for one 8039 machine cycle: 15 crystal periods,
/// in the units where an audio cycle adds 32_000_000 (see `acc8039`).
const MCU_STEP: u64 = 15 * 14_318_181;

/// The divide-by-10240 timer read on AY3 port A (MAME `porta_r`): a
/// divide-by-1024 plus an LS90 bi-quinary divide-by-10, indexed by
/// `(audio_cycles / 1024) % 10`.
///
/// MAME's comment says the timer feeds the UPPER nibble, but the table values
/// (0x00-0x0D) and the bit sequences beneath it are the low nibble; the code,
/// not the comment, is what the game reads.
const TIMER_TABLE: [u8; 10] = [0x00, 0x01, 0x02, 0x03, 0x04, 0x09, 0x0a, 0x0b, 0x0a, 0x0d];

/// The mixer as drawn on the sound sheet (all ohms; the sheet writes decimals
/// with commas, "2,2K", "4,7K"). Every source is a Thevenin voltage behind its
/// leg, and each bus is the passive node where the legs meet, loaded by its
/// 200-ohm VR to ground.
///
/// - Filtered voices (11D and 12D, all six channels): AY pin, 1K
///   (R38/R42/R66, R43/R64/R65), the switched-capacitor node, then 2.2K
///   (R39/R41/R67, R40/R44/R68) to the bus. The 1K is in series with the
///   2.2K, so a filtered voice meets the bus through 3.2K plus the chip's own
///   output resistance, not through the 2.2K alone.
/// - Unfiltered voices (8B, 9B, 10B): 3.3K per channel (R30-R32, R35-R37,
///   R61-R63) straight to the bus.
/// - DAC: the uPC324 follower (J6) through R34, 4.7K.
const R_AY_OUT: f32 = 356.0;
const R_FILT_SERIES: f32 = 1_000.0;
const R_FILT_LEG: f32 = 2_200.0;
const R_AY: f32 = 3_300.0;
const R_DAC: f32 = 4_700.0;
const R_VR: f32 = 200.0;
/// C36/C37, the 0.1 uF mylar at each amplifier input.
const C_WIPER: f64 = 0.1e-6;

/// What drives a filtered voice's node: the AY's output resistance plus 1K.
const R_FILT_SRC: f32 = R_AY_OUT + R_FILT_SERIES;
/// The filtered node's open-capacitor divider into its 2.2K leg.
const FILT_NODE_GAIN: f32 = R_FILT_LEG / (R_FILT_SRC + R_FILT_LEG);

/// AY channel swing at amplitude 15, in volts, for the chip's full-scale
/// output: the measured swing the AY core's volume curve comes from. It was
/// read in another circuit, so its use here, as the source driving the legs
/// above behind `R_AY_OUT`, is ASSUMED, as is `R_AY_OUT` itself; nothing on
/// the sheet gives either.
const AY_FULL_SCALE: f32 = phosphor_core::device::ay8910::FULL_SCALE as f32;
const AY_SWING_V: f32 = phosphor_core::device::ay8910::FULL_SCALE_VOLTS as f32;

/// The DAC: an 8-bit R-2R ladder (200K/100K, R45-R60, P17 at the follower
/// end) off the 8039's P1, so the ladder output is `code / 256` of the port's
/// high level. That level is ASSUMED at 4V (an 8039 quasi-bidirectional
/// output into a ladder of a few hundred K). The follower runs from +5V alone
/// (the sheet's arrow is +5V), and a 324 on a single 5V rail cannot drive its
/// output (or take its input) above about Vcc - 1.5V, so the top of the
/// ladder's range flattens at 3.5V.
const DAC_HIGH_V: f32 = 4.0;
const DAC_CLIP_V: f32 = 3.5;

/// Bus conductances. A filtered leg is counted at its DC conductance, the
/// whole series path with no capacitor in it.
const G_FILT: f32 = 1.0 / (R_FILT_SRC + R_FILT_LEG);
const G_AY: f32 = 1.0 / (R_AY_OUT + R_AY);
/// Right (VR1, 6E): 11D filtered + 9B + 10B. Left (VR2, 6F): 12D filtered +
/// 8B + the DAC. The 6F amplifier drives the SPL pins.
const GBUS_RIGHT: f32 = 3.0 * G_FILT + 6.0 * G_AY + 1.0 / R_VR;
const GBUS_LEFT: f32 = 3.0 * G_FILT + 3.0 * G_AY + 1.0 / R_DAC + 1.0 / R_VR;

/// Loudest each bus can carry with every source at full swing (volts).
const MAX_RIGHT: f32 = AY_SWING_V * (3.0 * G_FILT + 6.0 * G_AY) / GBUS_RIGHT;
const MAX_LEFT: f32 = (AY_SWING_V * (3.0 * G_FILT + 3.0 * G_AY) + DAC_CLIP_V / R_DAC) / GBUS_LEFT;

/// Presentation gain from bus volts to i16: full scale over the loudest the
/// loaded buses can carry. The wiper setting, the LA4460's fixed gain, and
/// the speaker-to-line mapping fold into this one gain, derived from the
/// model's own quantities so the loudest the sequencer can drive peaks at
/// digital full scale without chronic clipping. Nothing here is tuned to a
/// reference recording.
const PRESENTATION_GAIN: f32 = 32767.0
    / if MAX_RIGHT > MAX_LEFT {
        MAX_RIGHT
    } else {
        MAX_LEFT
    };

impl GyrussSound {
    /// Create the board. Call `load_sound_rom`/`load_mcu_rom` before use.
    ///
    /// `audio_hz` is the audio Z80's rate (14.318181 MHz / 4); the AYs run at
    /// half that. One number reaches the stepping, the PSG clocks, and the
    /// resampler input rates, so they cannot disagree.
    pub fn new(audio_hz: u64) -> Self {
        let ay_hz = audio_hz / 2;
        let mut ay = [
            Ay8910::new(ay_hz),
            Ay8910::new(ay_hz),
            Ay8910::new(ay_hz),
            Ay8910::new(ay_hz),
            Ay8910::new(ay_hz),
        ];
        for chip in &mut ay {
            chip.enable_channel_outputs();
        }
        let host = host_sample_rate() as u64;
        Self {
            cpu: Z80::new(),
            mcu: I8035::new_8039(),
            bus: GyrussSoundBus {
                ay,
                map: GyrussSoundBus::build_map(),
                command: 0,
                latch2: 0,
                irq_pending: false,
                mcu_irq_pending: false,
                dac: 0,
                p1: 0xFF,
                p2: 0xFF,
                filter_sel: [0; 2],
                filters: [VoiceFilter::new(); 6],
                left: AudioResampler::new(ay_hz, host),
                right: AudioResampler::new(ay_hz, host),
                dc_left: DcBlocker::new(host as u32),
                dc_right: DcBlocker::new(host as u32),
                wiper_left: WiperCap { v: 0.0 },
                wiper_right: WiperCap { v: 0.0 },
                clock: 0,
                acc8039: 0,
                ay_phase: false,
            },
        }
    }

    /// The audio CPU rate this board was built with, read back from the audio
    /// path: twice the AY clock the mix queues were told.
    pub fn cpu_clock_hz(&self) -> u64 {
        self.bus.left.input_rate() * 2
    }

    /// Load audio Z80 ROM data (2x8K at 0x0000; 0x4000-0x5FFF reads 0xFF).
    pub fn load_sound_rom(&mut self, data: &[u8]) {
        let region = self.bus.map.region_data_mut(Region::SndRom);
        let len = data.len().min(region.len());
        region[..len].copy_from_slice(&data[..len]);
        region[len..].fill(0xFF);
    }

    /// Load 8039 ROM data (2732, 4 KB).
    pub fn load_mcu_rom(&mut self, data: &[u8]) {
        let region = self.bus.map.region_data_mut(Region::McuRom);
        let len = data.len().min(region.len());
        region[..len].copy_from_slice(&data[..len]);
        region[len..].fill(0xFF);
    }

    /// Whether the audio Z80 is between instructions.
    pub fn at_instruction_boundary(&self) -> bool {
        self.cpu.at_instruction_boundary()
    }

    /// Whether the 8039 is between instructions.
    pub fn mcu_at_instruction_boundary(&self) -> bool {
        self.mcu.at_instruction_boundary()
    }

    // -----------------------------------------------------------------------
    // Main-board interface (the command latch and IRQ trigger)
    // -----------------------------------------------------------------------

    /// Latch a command byte from the main board (a 0xC100 write).
    pub fn write_command(&mut self, data: u8) {
        self.bus.command = data;
    }

    /// Pulse the audio CPU IRQ (a 0xC080 write). Held until the audio program
    /// reads the latch, which is its acknowledge.
    pub fn pulse_irq(&mut self) {
        self.bus.irq_pending = true;
    }

    /// Acknowledge the held IRQ the way the audio program does: read latch 1.
    #[cfg(test)]
    pub(crate) fn acknowledge_for_test(&mut self) {
        self.bus.read(BusMaster::Cpu(SOUND_CPU_INDEX), 0x8000);
    }

    // -----------------------------------------------------------------------
    // Tick (called at the audio-CPU clock rate, 1-2x per main cycle)
    // -----------------------------------------------------------------------

    /// Advance the board by one audio-CPU clock: present the timer on AY3
    /// port A, run one Z80 cycle, step the 8039 on its Bresenham divider,
    /// tick the AYs on their /2 phase, and filter, mix and queue one stereo
    /// frame per channel-sample the AYs produced.
    pub fn tick(&mut self) {
        // AY3 reads the timer on port A.
        let b = &mut self.bus;
        let timer = b.timer();
        b.ay[2].set_port_a(timer);

        // Bus dispatch cannot read CPU state while the CPU is mid-cycle, so the
        // cycle and instruction address a hit is attributed to are latched here.
        if self.bus.map.debug_active() {
            let pc = self
                .cpu
                .at_instruction_boundary()
                .then_some(u32::from(self.cpu.pc));
            self.bus.map.latch_access_context(self.bus.clock, pc);
        }

        self.cpu
            .execute_cycle(&mut self.bus, BusMaster::Cpu(SOUND_CPU_INDEX));

        // One 8039 machine cycle is 15 periods of its 8 MHz crystal, 533.3
        // kHz, so it steps about once per 6.7 audio cycles. The accumulator
        // keeps the exact crystal ratio, 32_000_000 against 15 x 14_318_181
        // (the audio clock is 14_318_181 / 4), with no drift.
        self.bus.acc8039 += 32_000_000;
        while self.bus.acc8039 >= MCU_STEP {
            self.bus.acc8039 -= MCU_STEP;
            self.mcu
                .execute_cycle(&mut self.bus, BusMaster::Cpu(MCU_INDEX));
        }

        let b = &mut self.bus;
        b.ay_phase = !b.ay_phase;
        if b.ay_phase {
            for chip in &mut b.ay {
                chip.tick();
            }
        }

        // A chip emits only at the end of a group of eight of its clocks, so
        // most cycles there is nothing to drain; checking first spares fifteen
        // empty drains per cycle.
        if b.ay.iter().all(|chip| chip.channel_samples_buffered() == 0) {
            b.clock += 1;
            return;
        }

        // All five AYs share one clock, so all fifteen channels produce the
        // same count; each chip is still drained separately so a phase slip
        // degrades to a silent chip rather than a stuck one.
        let mut ch = [0i16; 15];
        let mut produced = [0usize; 5];
        for (i, chip) in b.ay.iter_mut().enumerate() {
            let base = i * 3;
            produced[i] = chip.fill_channel_audio(0, &mut ch[base..base + 1]);
            chip.fill_channel_audio(1, &mut ch[base + 1..base + 2]);
            chip.fill_channel_audio(2, &mut ch[base + 2..base + 3]);
        }
        if produced.iter().any(|&n| n > 0) {
            let (left, right) = b.mix_frame(&ch, &produced);
            b.left.push_sample(left);
            b.right.push_sample(right);
        }

        b.clock += 1;
    }

    /// Drain accumulated stereo frames, interleaved left-first. Returns the
    /// number of samples written (always even).
    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        let frames = (buffer.len() / 2)
            .min(self.bus.left.buffered())
            .min(self.bus.right.buffered());
        for i in 0..frames {
            self.bus.left.fill_audio(&mut buffer[2 * i..2 * i + 1]);
            self.bus.right.fill_audio(&mut buffer[2 * i + 1..2 * i + 2]);
        }
        frames * 2
    }

    /// Reset the board to power-on state.
    pub fn reset(&mut self) {
        self.cpu
            .reset(&mut self.bus, BusMaster::Cpu(SOUND_CPU_INDEX));
        self.mcu.reset(&mut self.bus, BusMaster::Cpu(MCU_INDEX));
        for chip in &mut self.bus.ay {
            chip.reset();
        }
        self.bus.map.region_data_mut(Region::SndRam).fill(0);
        self.bus.command = 0;
        self.bus.latch2 = 0;
        self.bus.irq_pending = false;
        self.bus.mcu_irq_pending = false;
        self.bus.dac = 0;
        self.bus.p1 = 0xFF;
        self.bus.p2 = 0xFF;
        self.bus.filter_sel = [0; 2];
        self.bus.filters = [VoiceFilter::new(); 6];
        self.bus.wiper_left = WiperCap { v: 0.0 };
        self.bus.wiper_right = WiperCap { v: 0.0 };
        self.bus.left.reset();
        self.bus.right.reset();
        self.bus.dc_left.reset();
        self.bus.dc_right.reset();
        self.bus.clock = 0;
        self.bus.acc8039 = 0;
        self.bus.ay_phase = false;
    }
}

// ---------------------------------------------------------------------------
// Bus implementation (audio Z80 memory+IO, 8039 program+ports)
// ---------------------------------------------------------------------------

impl GyrussSoundBus {
    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(
            Region::SndRom,
            "Sound ROM",
            0x0000,
            0x4000,
            AccessKind::ReadOnly,
        )
        .region(
            Region::SndRam,
            "Sound RAM",
            0x6000,
            0x0400,
            AccessKind::ReadWrite,
        )
        .region(
            Region::McuRom,
            "8039 ROM",
            0xC000,
            0x1000,
            AccessKind::ReadOnly,
        );
        map
    }

    /// Audio Z80 memory read.
    fn audio_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x3fff => self.map.read_backing(addr),
            // 0x4000-0x5FFF: empty socket, reads open bus (never 0x55, so the
            // diagnostics branch stays off).
            0x6000..=0x63ff => self.map.read_backing(addr),
            0x8000 => {
                // Latch 1 fetch: the acknowledge that clears the held IRQ.
                self.irq_pending = false;
                self.command
            }
            _ => 0xFF,
        }
    }

    /// 8039 program read, translated up by the 0xC000 the region is offset.
    fn mcu_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x0fff => self.map.read_backing(0xC000 + addr),
            _ => 0xFF,
        }
    }

    /// Audio Z80 IO read (mask 0xFF): the odd AY data ports.
    fn audio_io_read(&mut self, addr: u16) -> u8 {
        match addr & 0xFF {
            0x01 | 0x05 | 0x09 | 0x0D | 0x11 => self.ay[((addr & 0xFF) >> 2) as usize].data_read(),
            _ => 0xFF,
        }
    }

    /// 8039 port read: BUS (latch 2), the P1/P2 latches, T0/T1 grounded.
    /// The low 0x00-0xFF window (MOVX) also sees latch 2, per MAME's
    /// `audio_cpu2_io_map`; the program has no external RAM to address.
    fn mcu_io_read(&mut self, addr: u16) -> u8 {
        match addr {
            0x0000..=0x0100 => self.latch2,
            0x0101 => self.p1,
            0x0102 => self.p2,
            _ => 0,
        }
    }

    /// Audio Z80 IO write: AY address/data, 8039 IRQ, latch 2.
    fn audio_io_write(&mut self, addr: u16, data: u8) {
        match addr & 0xFF {
            0x00 | 0x04 | 0x08 | 0x0C | 0x10 => {
                self.ay[((addr & 0xFF) >> 2) as usize].address_write(data);
            }
            0x02 | 0x06 | 0x0A | 0x0E | 0x12 => {
                let chip = ((addr & 0xFF) >> 2) as usize;
                self.ay[chip].data_write(data);
                // AY1/AY2 port B (register 15) selects the filter caps.
                if chip < 2 && self.ay[chip].latched_register() == 15 {
                    self.filter_write(chip, self.ay[chip].port_b_read());
                }
            }
            0x14 => self.mcu_irq_pending = true,
            0x18 => self.latch2 = data,
            _ => {}
        }
    }

    /// 8039 port write: P1 is the DAC, any P2 write clears the 8039 INT
    /// (MAME `irq_clear_w`); BUS/MOVX writes have no device to reach.
    fn mcu_io_write(&mut self, addr: u16, data: u8) {
        match addr {
            0x0101 => {
                self.p1 = data;
                self.dac = data;
            }
            0x0102 => {
                self.p2 = data;
                self.mcu_irq_pending = false;
            }
            _ => {}
        }
    }
}

impl Bus for GyrussSoundBus {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let (index, data) = match master {
            BusMaster::Cpu(2) => (SOUND_CPU_INDEX, self.audio_read(addr)),
            BusMaster::Cpu(3) => (MCU_INDEX, self.mcu_read(addr)),
            _ => return 0xFF,
        };
        self.map.watch_read(index, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        match master {
            BusMaster::Cpu(2) => {
                self.map.watch_write(SOUND_CPU_INDEX, master, addr, data);
                if (0x6000..=0x63ff).contains(&addr) {
                    self.map.write_backing(addr, data);
                }
            }
            // The 8039 has no writable memory (no external RAM).
            BusMaster::Cpu(3) => {
                self.map.watch_write(MCU_INDEX, master, addr, data);
            }
            _ => {}
        }
    }

    fn io_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        match master {
            BusMaster::Cpu(2) => self.audio_io_read(addr),
            BusMaster::Cpu(3) => self.mcu_io_read(addr),
            _ => 0xFF,
        }
    }

    fn io_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        match master {
            BusMaster::Cpu(2) => self.audio_io_write(addr, data),
            BusMaster::Cpu(3) => self.mcu_io_write(addr, data),
            _ => {}
        }
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, target: BusMaster) -> phosphor_core::core::bus::InterruptState {
        let irq = match target {
            BusMaster::Cpu(2) => self.irq_pending,
            BusMaster::Cpu(3) => self.mcu_irq_pending,
            _ => false,
        };
        phosphor_core::core::bus::InterruptState {
            nmi: false,
            irq,
            firq: false,
            irq_vector: 0xFF,
            irq_level: 0,
        }
    }
}

impl GyrussSoundBus {
    /// The divide-by-10240 timer presented on AY3 port A.
    fn timer(&self) -> u8 {
        TIMER_TABLE[((self.clock / 1024) % 10) as usize]
    }

    /// Apply an AY1/AY2 port-B filter select: 2 bits per channel, channel A
    /// in B0/B1, B in B2/B3, C in B4/B5, as traced from each 4066 control pin.
    /// A capacitor switched out keeps its charge (see [`VoiceFilter`]).
    fn filter_write(&mut self, chip: usize, data: u8) {
        self.filter_sel[chip] = data;
        for ch in 0..3 {
            self.filters[chip * 3 + ch].cap = (data >> (ch * 2)) & 3;
        }
    }

    /// Mix one stereo frame from the fifteen AY channels plus the DAC.
    ///
    /// Each bus is the passive node the sheet draws: every voice meets it
    /// through its leg, loaded by the 200-ohm VR trimmer, and the wiper (at
    /// full; an operator control) feeds the LA4460 across C36/C37. DC-blocked
    /// per channel: the sources are unipolar, and the amplifier's feedback
    /// capacitor (C26/C30, 100 uF on pin 6) takes its DC gain to unity.
    fn mix_frame(&mut self, ch: &[i16; 15], produced: &[usize; 5]) -> (i16, i16) {
        let (bus_left, bus_right) = self.bus_volts(ch, produced);
        let bus_left = self.wiper_left.process(bus_left, GBUS_LEFT);
        let bus_right = self.wiper_right.process(bus_right, GBUS_RIGHT);
        let mut pair = [bus_left * PRESENTATION_GAIN, bus_right * PRESENTATION_GAIN];
        self.dc_left.process_slice(&mut pair[0..1]);
        self.dc_right.process_slice(&mut pair[1..2]);
        (
            pair[0].clamp(-32768.0, 32767.0) as i16,
            pair[1].clamp(-32768.0, 32767.0) as i16,
        )
    }

    /// The two bus voltages, left then right, ahead of the wiper capacitors.
    /// Steps the six voice filters.
    fn bus_volts(&mut self, ch: &[i16; 15], produced: &[usize; 5]) -> (f32, f32) {
        // Channel volts at the AY's open-circuit source.
        let sample = |chip: usize, c: usize| -> f32 {
            if produced[chip] > 0 {
                f32::from(ch[chip * 3 + c]) * (AY_SWING_V / AY_FULL_SCALE)
            } else {
                0.0
            }
        };
        let mut filt = [0.0f32; 6];
        for (i, f) in filt.iter_mut().enumerate() {
            *f = self.filters[i].process(sample(i / 3, i % 3));
        }
        let dac = (f32::from(self.dac) / 256.0 * DAC_HIGH_V).min(DAC_CLIP_V);
        let unfiltered = |chip: usize| sample(chip, 0) + sample(chip, 1) + sample(chip, 2);
        let bus_right = ((filt[0] + filt[1] + filt[2]) / R_FILT_LEG
            + (unfiltered(2) + unfiltered(3)) * G_AY)
            / GBUS_RIGHT;
        let bus_left =
            ((filt[3] + filt[4] + filt[5]) / R_FILT_LEG + unfiltered(4) * G_AY + dac / R_DAC)
                / GBUS_LEFT;
        (bus_left, bus_right)
    }
}

// ---------------------------------------------------------------------------
// Device trait
// ---------------------------------------------------------------------------

impl Device for GyrussSound {
    fn name(&self) -> &'static str {
        "Gyruss Sound"
    }

    fn reset(&mut self) {
        self.reset();
    }

    fn tick(&mut self) {
        self.tick();
    }
}

// ---------------------------------------------------------------------------
// Debug support
// ---------------------------------------------------------------------------

impl Debuggable for GyrussSound {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "COMMAND",
                value: u64::from(self.bus.command),
                width: 8,
            },
            DebugRegister {
                name: "LATCH2",
                value: u64::from(self.bus.latch2),
                width: 8,
            },
            DebugRegister {
                name: "IRQ",
                value: u64::from(self.bus.irq_pending),
                width: 1,
            },
            DebugRegister {
                name: "MCU_IRQ",
                value: u64::from(self.bus.mcu_irq_pending),
                width: 1,
            },
            DebugRegister {
                name: "DAC",
                value: u64::from(self.bus.dac),
                width: 8,
            },
            DebugRegister {
                name: "FILT1",
                value: u64::from(self.bus.filter_sel[0]),
                width: 8,
            },
            DebugRegister {
                name: "FILT2",
                value: u64::from(self.bus.filter_sel[1]),
                width: 8,
            },
        ]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::save_state::{Saveable, StateReader, StateWriter};

    /// The rate a Gyruss board supplies: its 14.318181 MHz sound crystal over
    /// four. Nothing here depends on the exact value, but using the real one
    /// keeps the device's tests honest about what it is fed.
    const TEST_AUDIO_CLOCK: u64 = 14_318_181 / 4;

    fn sound_bus() -> GyrussSoundBus {
        GyrussSound::new(TEST_AUDIO_CLOCK).bus
    }

    #[test]
    fn ram_round_trips_and_empty_socket_reads_ff() {
        let mut s = GyrussSound::new(TEST_AUDIO_CLOCK);
        s.load_sound_rom(&[0x11; 0x4000]);
        s.bus.write(BusMaster::Cpu(SOUND_CPU_INDEX), 0x6000, 0xab);
        assert_eq!(s.bus.read(BusMaster::Cpu(SOUND_CPU_INDEX), 0x6000), 0xab);
        assert_eq!(s.bus.read(BusMaster::Cpu(SOUND_CPU_INDEX), 0x0000), 0x11);
        assert_eq!(
            s.bus.read(BusMaster::Cpu(SOUND_CPU_INDEX), 0x4000),
            0xFF,
            "the 0x4000-0x5FFF socket is empty"
        );
    }

    #[test]
    fn ay_io_triples_decode_per_chip() {
        let mut b = sound_bus();
        let cpu = BusMaster::Cpu(SOUND_CPU_INDEX);
        // Chip 2 (AY3): latch R7, write it, read it back.
        b.io_write(cpu, 0x08, 7);
        b.io_write(cpu, 0x0A, 0x3F);
        b.io_write(cpu, 0x08, 7);
        assert_eq!(b.io_read(cpu, 0x09), 0x3F);
        // Chip 4 (AY5) data write lands on chip 4, not chip 2.
        b.io_write(cpu, 0x10, 8);
        b.io_write(cpu, 0x12, 0x0F);
        assert_eq!(b.ay[2].latched_register(), 7);
        assert_eq!(b.ay[4].latched_register(), 8);
        // Unmapped IO reads open bus.
        assert_eq!(b.io_read(cpu, 0x03), 0xFF);
    }

    #[test]
    fn ay3_port_a_reads_the_timer() {
        let mut s = GyrussSound::new(TEST_AUDIO_CLOCK);
        let cpu = BusMaster::Cpu(SOUND_CPU_INDEX);
        s.bus.io_write(cpu, 0x08, 14); // AY3 port A (R7 default = input)
        s.bus.clock = 5 * 1024;
        s.tick(); // presents the timer
        assert_eq!(s.bus.io_read(cpu, 0x09), TIMER_TABLE[5]);
    }

    #[test]
    fn latch_read_clears_the_held_irq() {
        let mut s = GyrussSound::new(TEST_AUDIO_CLOCK);
        s.pulse_irq();
        let state = s.bus.check_interrupts(BusMaster::Cpu(SOUND_CPU_INDEX));
        assert!(state.irq, "pulsed IRQ is pending");
        s.acknowledge_for_test();
        assert!(!s.bus.irq_pending);
        assert_eq!(s.bus.command, 0);
    }

    #[test]
    fn timer_walks_the_ls90_table() {
        let mut s = GyrussSound::new(TEST_AUDIO_CLOCK);
        for (i, expected) in TIMER_TABLE.iter().enumerate() {
            s.bus.clock = i as u64 * 1024;
            assert_eq!(s.bus.timer(), *expected, "step {i}");
        }
        s.bus.clock = 10 * 1024;
        assert_eq!(s.bus.timer(), TIMER_TABLE[0], "table wraps");
    }

    #[test]
    fn port_b_write_routes_two_bits_per_channel() {
        let mut b = sound_bus();
        let cpu = BusMaster::Cpu(SOUND_CPU_INDEX);
        // AY1 port B = 0x64: ch0 disabled, ch1 0.047u, ch2 0.22u.
        b.io_write(cpu, 0x00, 15);
        b.io_write(cpu, 0x02, 0x64);
        assert_eq!(b.filter_sel[0], 0x64);
        let caps: Vec<u8> = b.filters[0..3].iter().map(|f| f.cap).collect();
        assert_eq!(caps, vec![0, 1, 2]);
        // AY3 port B (chip 2) selects nothing.
        b.io_write(cpu, 0x08, 15);
        b.io_write(cpu, 0x0A, 0xFF);
        assert_eq!(b.filter_sel[1], 0);
        // A capacitor switched out keeps its charge.
        b.filters[1].held = [0.5, 0.0];
        b.io_write(cpu, 0x00, 15);
        b.io_write(cpu, 0x02, 0x00);
        assert_eq!(b.filters[1].cap, 0);
        assert_eq!(b.filters[1].held, [0.5, 0.0]);
    }

    #[test]
    fn a_reconnected_capacitor_shares_its_held_charge() {
        // 0.22u held at 1V, 0.047u at 0V: closing both starts the node from
        // the charge-weighted 0.824V, not from either capacitor alone.
        let mut f = VoiceFilter {
            cap: 3,
            held: [0.0, 1.0],
        };
        let x = 0.824 / FILT_NODE_GAIN; // the node's target equals the start
        let v = f.process(x);
        assert!((v - 0.824).abs() < 1e-3, "{v}");
        assert_eq!(f.held[0], f.held[1], "both caps follow the node");
    }

    #[test]
    fn filtered_and_unfiltered_voices_meet_the_bus_at_matching_weight() {
        // The 1K and the 2.2K are in series, so at DC a filtered voice drives
        // the bus through 3556 ohms and an unfiltered one through 3656: within
        // half a dB, not the 3.5 dB a bare 2.2K leg would give.
        let filtered = FILT_NODE_GAIN / R_FILT_LEG;
        let db = 20.0 * (filtered / G_AY).log10();
        assert!(db > 0.0 && db < 0.5, "{db} dB");
        assert!(
            (filtered - G_FILT).abs() < 1e-9,
            "node current is the leg's"
        );
    }

    #[test]
    fn voice_filter_corners_follow_the_node_thevenin_resistance() {
        // The capacitor sees (356 + 1K) || 2.2K = 839 ohms.
        let r = f64::from(R_FILT_SRC * R_FILT_LEG / (R_FILT_SRC + R_FILT_LEG));
        let fc = |c: f64| 1.0 / (std::f64::consts::TAU * r * c);
        let small = fc(FILTER_CAPS[0]);
        let large = fc(FILTER_CAPS[1]);
        let both = fc(FILTER_CAPS[0] + FILTER_CAPS[1]);
        assert!((small - 4036.0).abs() < 10.0, "{small}");
        assert!((large - 862.0).abs() < 5.0, "{large}");
        assert!((both - 710.0).abs() < 5.0, "{both}");
    }

    #[test]
    fn the_dac_flattens_at_the_followers_ceiling() {
        // 4V * code/256 reaches 3.5V at code 224; above it the 324 is pinned.
        let mut b = sound_bus();
        let silent = [0i16; 15];
        let none = [0usize; 5];
        b.dac = 224;
        let (at_224, _) = b.bus_volts(&silent, &none);
        b.dac = 255;
        let (at_255, _) = b.bus_volts(&silent, &none);
        b.dac = 128;
        let (at_128, _) = b.bus_volts(&silent, &none);
        assert_eq!(at_224, at_255);
        assert!((at_128 - 2.0 / R_DAC / GBUS_LEFT).abs() < 1e-6, "{at_128}");
    }

    #[test]
    fn mcu_sees_its_own_rom_at_zero() {
        let mut s = GyrussSound::new(TEST_AUDIO_CLOCK);
        s.load_sound_rom(&[0x11; 0x4000]);
        s.load_mcu_rom(&[0xAA; 0x1000]);
        assert_eq!(s.bus.read(BusMaster::Cpu(SOUND_CPU_INDEX), 0x0000), 0x11);
        assert_eq!(s.bus.read(BusMaster::Cpu(MCU_INDEX), 0x0000), 0xAA);
        assert_eq!(s.bus.read(BusMaster::Cpu(MCU_INDEX), 0x0FFF), 0xAA);
        assert_eq!(s.bus.read(BusMaster::Cpu(MCU_INDEX), 0x1000), 0xFF);
    }

    #[test]
    fn latch2_and_ports_cross_between_cpus() {
        let mut s = GyrussSound::new(TEST_AUDIO_CLOCK);
        let z80 = BusMaster::Cpu(SOUND_CPU_INDEX);
        let mcu = BusMaster::Cpu(MCU_INDEX);
        // Latch 2: audio writes, 8039 reads over BUS (and the MOVX window).
        s.bus.io_write(z80, 0x18, 0x5A);
        assert_eq!(s.bus.io_read(mcu, 0x100), 0x5A);
        assert_eq!(s.bus.io_read(mcu, 0x0042), 0x5A);
        // P1 is the DAC and reads back; P2 reads back.
        s.bus.io_write(mcu, 0x101, 0x77);
        assert_eq!(s.bus.dac, 0x77);
        assert_eq!(s.bus.io_read(mcu, 0x101), 0x77);
        s.bus.io_write(mcu, 0x102, 0x3C);
        assert_eq!(s.bus.io_read(mcu, 0x102), 0x3C);
        // 0x14 asserts the 8039 INT; any P2 write clears it.
        s.bus.io_write(z80, 0x14, 0);
        assert!(s.bus.check_interrupts(mcu).irq);
        s.bus.io_write(mcu, 0x102, 0x00);
        assert!(!s.bus.check_interrupts(mcu).irq);
    }

    #[test]
    fn mcu_runs_8mhz_over_15_against_the_audio_clock() {
        // Blank ROMs are NOP slides (0x00, one machine cycle each), so the
        // 8039 PC counts machine cycles exactly: 1000 audio cycles are
        // 279.4 us, and floor(1000 x 32M / (15 x 14318181)) = 148 machine
        // cycles of 1.875 us. (Stepping per crystal clock instead gives 2234.)
        let mut s = GyrussSound::new(TEST_AUDIO_CLOCK);
        for _ in 0..1000 {
            s.tick();
        }
        assert_eq!(s.mcu.pc, 148);
    }

    #[test]
    fn mix_gains_follow_the_drawn_legs() {
        // The presentation gain is derived, not tuned: full scale over the
        // loudest the loaded buses can carry (right, all AYs full, 0.476V).
        assert!((MAX_RIGHT - 0.4757).abs() < 0.001, "{MAX_RIGHT}");
        assert!((MAX_LEFT - 0.4551).abs() < 0.001, "{MAX_LEFT}");
        // Every channel at full swing with no capacitor switched in: each
        // bus carries exactly its stated maximum less the DAC.
        let mut b = sound_bus();
        let (left, right) = b.bus_volts(&[8192i16; 15], &[1usize; 5]);
        assert!((right - MAX_RIGHT).abs() < 1e-4, "right = {right}");
        let left_ay = MAX_LEFT - DAC_CLIP_V / R_DAC / GBUS_LEFT;
        assert!((left - left_ay).abs() < 1e-4, "left = {left}");
        // A chip whose PSG produced nothing this frame contributes nothing.
        let mut b = sound_bus();
        let (left, right) = b.bus_volts(&[8192i16; 15], &[0usize; 5]);
        assert_eq!((left, right), (0.0, 0.0));
    }

    #[test]
    fn the_wiper_capacitor_settles_to_the_bus() {
        // A constant bus settles through C36/C37 within a few samples (the
        // 13-15 us time constant is under one sample) and is held below the
        // input on the very first one.
        let mut w = WiperCap { v: 0.0 };
        let first = w.process(1.0, GBUS_RIGHT);
        assert!(first > 0.5 && first < 1.0, "{first}");
        for _ in 0..20 {
            w.process(1.0, GBUS_RIGHT);
        }
        assert!((w.v - 1.0).abs() < 1e-4, "{}", w.v);
    }

    #[test]
    fn steady_state_blocks_the_unipolar_offset() {
        // Constant full-scale input is pure DC: after settling, the blocked
        // output rests near zero instead of near full-scale.
        let mut b = sound_bus();
        let produced = [1usize; 5];
        let (l0, r0) = b.mix_frame(&[8192i16; 15], &produced);
        assert!(l0 > 1000 && r0 > 1000, "first frame passes: {l0},{r0}");
        let (mut l, mut r) = (l0, r0);
        // One call is one output sample; the blocker's ~700-sample tau
        // needs tens of thousands of samples to bury the post-gain step.
        for _ in 0..40_000 {
            (l, r) = b.mix_frame(&[8192i16; 15], &produced);
        }
        assert!(l.abs() < 100 && r.abs() < 100, "settled: {l},{r}");
    }

    #[test]
    fn save_round_trips_latches_irq_and_filters() {
        let mut s = GyrussSound::new(TEST_AUDIO_CLOCK);
        s.write_command(0xA5);
        s.pulse_irq();
        s.bus.io_write(BusMaster::Cpu(SOUND_CPU_INDEX), 0x18, 0x5A);
        s.bus.io_write(BusMaster::Cpu(SOUND_CPU_INDEX), 0x14, 0);
        s.bus.io_write(BusMaster::Cpu(MCU_INDEX), 0x101, 0x77);
        s.bus.io_write(BusMaster::Cpu(SOUND_CPU_INDEX), 0x00, 15);
        s.bus.io_write(BusMaster::Cpu(SOUND_CPU_INDEX), 0x02, 0xE4);
        let mut w = StateWriter::new();
        s.save_state(&mut w);
        let bytes = w.into_vec();

        let mut s2 = GyrussSound::new(TEST_AUDIO_CLOCK);
        let mut r = StateReader::new(&bytes);
        s2.load_state(&mut r).unwrap();
        assert_eq!(s2.bus.command, 0xA5);
        assert!(s2.bus.irq_pending);
        assert_eq!(s2.bus.latch2, 0x5A);
        assert!(s2.bus.mcu_irq_pending);
        assert_eq!(s2.bus.dac, 0x77);
        assert_eq!(s2.bus.filter_sel[0], 0xE4);
    }
}
