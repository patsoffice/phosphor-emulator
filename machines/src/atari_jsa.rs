//! Atari JSA-I sound board: an M6502 with a YM2151 (OPM FM synthesis) and, on
//! the boards that fit one, a POKEY, talking to the 68010 main board through a
//! pair of one-byte latches.
//!
//! The board runs off its own 3.579545 MHz crystal, independent of whatever the
//! main board is clocked at: the 6502 and the POKEY take half of it (1.789772
//! MHz) and the YM2151 takes all of it.
//!
//! It is the successor to the System 1 sound board and is wired much the same
//! way, but it is not the same board. Three differences matter:
//!
//! - The upper ROM is banked. A 4 KB window at `0x3000` selects one of four
//!   pages out of the low 16 KB of the sound chip; `0x4000` upward is fixed.
//! - The 6502 takes a free-running periodic interrupt, once every 7168 of its
//!   own cycles (about 249.7 Hz), separate from the YM2151's timer interrupt.
//!   Either source raises IRQ, and only the periodic one is acknowledged by the
//!   `/IRQACK` strobe.
//! - A mix register sets the POKEY and YM2151 volumes in coarse steps, so the
//!   program controls the balance between the two rather than the board fixing
//!   it.
//!
//! **Coin switches are on this board, not on the main board**, read back on the
//! `/RDIO` port along with the two handshake flags and the operator self-test
//! line. A game using this board has no coin input until this board exists.
//!
//! ## Sound-CPU memory map
//! ```text
//!   0000-1FFF  RAM
//!   2000-2001  YM2151 (address / data; reads return status)
//!   2800-2BFF  I/O, decoded on address bits 1, 2 and 9:
//!                R 002 command latch (clears the pending flag and the NMI)
//!                R 004 coin / handshake / self-test port
//!                RW 006 periodic-interrupt acknowledge
//!                W 202 response latch (to the main board, raises its IRQ2)
//!                W 204 ROM bank, coin counters, YM2151 reset
//!                W 206 mix: POKEY and YM2151 volumes
//!   2C00-2C0F  POKEY (only on boards that fit one)
//!   3000-3FFF  banked ROM window (one of four pages)
//!   4000-FFFF  fixed ROM
//! ```
//!
//! The speech variant of this board (a TMS5220 on the `/VOICE` strobe, with the
//! read and write strobes and a pitch select on the bank register) is not
//! modeled: no game in the registry fits one. The strobes it would use are
//! decoded and ignored, and the port bit that reports the speech chip ready
//! reads low, which is what an empty socket gives.
//!
//! ## The analog output stage
//!
//! Transcribed in `docs/schematics/toobin-audio-output.md` and modeled here,
//! with gains taken off the board rather than fitted by ear.
//!
//! All three volume ladders are binary-weighted CD4066 switches summing into an
//! LM324 virtual ground, so each is linear in its code and the two feedback
//! resistors fix the ratio between the sources: the YM2151 reaches 2.80 at code
//! 7 and the POKEY 0.94 at code 3.
//!
//! **The POKEY's route is program-controlled, and can be a mute.** The POKEY and
//! the speech socket are summed first into one signal, which is then injected
//! into the left and right channel mixers through legs gated by the YM2151's own
//! `CT1` and `CT2` output pins. Clearing both takes the POKEY off *both*
//! speakers whatever its volume code says.
//!
//! **Each channel has a switched low-pass.** A fixed pole near 6 kHz, plus a
//! shunt that moves from about 13.3 kHz to about 3.6 kHz when a transistor
//! switches a second capacitor in. Its base is driven from the mix register's
//! `LPF` bit **or** from `YM0`, the bottom bit of the YM volume field, so the
//! corner also drops whenever that code is odd. That second path looks like an
//! accident of the design rather than an intent and is flagged in the
//! transcription for a second reading.
//!
//! **The board is stereo and this is mono, and the collapse is ours.** It
//! happens in two places, not one. `Ym2151::drain_audio` already returns a
//! single stream: it sums all eight FM channels and does not look at their
//! per-channel left/right enable bits at all, so the FM's own panning is gone
//! before this module sees a sample. On top of that, the two channel mixers
//! here become one, with the POKEY present at full gain if it reaches either
//! speaker. Recovering any of it starts in the YM2151 core, not here.
//!
//! **On Toobin' specifically, none of that costs anything, because the game
//! never uses the stereo the board offers.** Measured across 2950 frames of a
//! recorded session covering attract, a coin and real play: all eight FM
//! channels sit centered (both enables set) in every frame, and `CT1`/`CT2` are
//! both set in every frame. Both sources are therefore identical on the two
//! speakers, and the mono sum is what the board produces rather than an
//! approximation of it. The `CT1`/`CT2` gating above is correct and inert here;
//! it is modeled for the rest of the JSA-I catalog, and because a mute is not a
//! thing to discover later. One session is not a proof for every screen the
//! game has, but 2950 frames with zero variation is strong.
//!
//! **What else is approximate.** The volume codes, the routing and the filter
//! switch are sampled once per drain rather than per sample. And the absolute
//! level assumes our YM2151 and POKEY cores sit at the same relative scale as
//! the chips do, which nobody has checked: the board's ratio is right, the two
//! cores' agreement with it is not established.

use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::device::pokey::Pokey;
use phosphor_core::device::ym2151::Ym2151;
use phosphor_macros::Saveable;

/// Sound CPU and POKEY clock: 3.579545 MHz / 2.
pub const SOUND_CLOCK_HZ: u32 = 1_789_772;

/// The YM2151 takes the whole crystal, twice the sound CPU's rate, so it
/// advances two chip clocks for every sound-CPU cycle.
const YM_CLOCKS_PER_TICK: u32 = 2;

/// Period of the board's free-running interrupt, in sound-CPU cycles.
///
/// The crystal is divided by 4, then by 16, 16 and 14, giving about 249.7 Hz.
/// Against the sound CPU's own rate that lands on exactly 7168 cycles, with
/// nothing rounded: the CPU is the same crystal over two, so the period is
/// `(4 * 16 * 16 * 14) / 2`.
const IRQ_PERIOD_CYCLES: u32 = 7168;

fn audio_sample_rate_hz() -> u32 {
    phosphor_core::audio::host_sample_rate() as u32
}

// ---------------------------------------------------------------------------
// The analog output stage, from docs/schematics/toobin-audio-output.md
// ---------------------------------------------------------------------------

/// The YM2151 ladder's gain per volume step into a channel mixer: its smallest
/// leg is 30k against the mixer's 12k feedback, and the ladder is
/// binary-weighted, so the gain is linear in the code and reaches 2.80 at 7.
const YM_GAIN_PER_STEP: f32 = 12.0 / 30.0;

/// The POKEY ladder's gain per volume step: 150k smallest leg against the
/// summing amplifier's 47k feedback, then unity through the 12k leg into each
/// channel. Reaches 0.94 at code 3.
const POKEY_GAIN_PER_STEP: f32 = 47.0 / 150.0;

/// Fixed pole across the output amplifier's 12k feedback: 0.0022 uF, 6.0 kHz.
const FIXED_POLE_HZ: f32 = 6030.0;

/// The shunt at the output amplifier's input node with the switch open: 12k
/// against 0.001 uF.
const SHUNT_OPEN_HZ: f32 = 13_260.0;

/// The same shunt once the transistor switches a second 0.0027 uF in parallel,
/// so 12k against 0.0037 uF.
const SHUNT_CLOSED_HZ: f32 = 3585.0;

/// One-pole RC section, which is what each of this board's two filter stages
/// is. Kept private here rather than in `phosphor-core`'s audio module: it is
/// the low-pass counterpart of `DcBlocker` and belongs beside it if a second
/// board ever needs one, but adding public API for a single user is worse.
///
/// The coefficient is passed per call rather than stored, because one of the
/// two sections switches its corner at runtime and has to do that without
/// discarding the state that makes it a filter.
#[derive(Clone, Copy, Debug, Default)]
struct OnePole {
    y: f32,
}

impl OnePole {
    /// `1 - exp(-2*pi*fc/fs)`, the exact step response of an RC section rather
    /// than a bilinear approximation of it.
    fn coefficient(cutoff_hz: f32, sample_rate: u32) -> f32 {
        let fs = sample_rate.max(1) as f32;
        (1.0 - (-std::f32::consts::TAU * cutoff_hz / fs).exp()).clamp(0.0, 1.0)
    }

    #[inline]
    fn process(&mut self, x: f32, a: f32) -> f32 {
        self.y += a * (x - self.y);
        self.y
    }

    fn reset(&mut self) {
        self.y = 0.0;
    }
}

/// Mix-register value that selects full volume on every source.
///
/// The board powers up with its volume lines undefined and the program sets the
/// mix before it plays anything, so starting at full is what keeps a board that
/// never writes the register audible rather than silent.
const MIX_FULL: u8 = 0xFE;

/// Which optional parts this particular board carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsaPokey {
    /// A POKEY is fitted at `0x2C00` (Toobin', Vindicators).
    Fitted,
    /// No POKEY: the window reads back as open bus (Blasteroids, Xybots).
    Absent,
}

/// The JSA-I sound board.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct AtariJsa1 {
    #[save(id = 1)]
    cpu: M6502,
    /// Everything the sound CPU talks to. Held apart from the CPU so a cycle
    /// dispatches at a concrete bus rather than a trait object.
    #[save(id = 2)]
    bus: Jsa1Bus,

    /// The two one-pole sections of the output stage, and their coefficients.
    ///
    /// The coefficients depend on the host sample rate, so they are computed
    /// once at construction rather than being constants. The state is filter
    /// history, which a load re-establishes within a few samples of resuming,
    /// so none of this is saved.
    #[save_skip]
    shunt_pole: OnePole,
    #[save_skip]
    fixed_pole: OnePole,
    #[save_skip]
    fixed_a: f32,
    #[save_skip]
    shunt_open_a: f32,
    #[save_skip]
    shunt_closed_a: f32,
}

#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
struct Jsa1Bus {
    #[save(id = 1)]
    ram: Box<[u8; 0x2000]>,
    /// Fixed ROM at `0x4000-0xFFFF`.
    #[save_skip]
    rom: Box<[u8; 0xC000]>,
    /// The four 4 KB pages the `0x3000` window selects between.
    #[save_skip]
    bank_pages: Box<[u8; 0x4000]>,
    /// Which page the window currently presents.
    #[save(id = 2)]
    bank: u8,

    /// POKEY, on the boards that fit one.
    ///
    /// An `Option` field is on the wire exactly when it is fitted, so a board
    /// with one and a board without differ by the id being present rather than
    /// by a trailing length.
    #[save(id = 3)]
    pokey: Option<Pokey>,
    #[save(id = 4)]
    ym: Ym2151,

    // -- Inter-CPU latches --------------------------------------------------
    /// Command from the main board. `command_pending` is what the main board's
    /// status port reports and what the `/RDIO` port calls the NMI line state.
    #[save(id = 5)]
    soundlatch: u8,
    #[save(id = 6)]
    command_pending: bool,
    /// Response to the main board; `response_pending` raises its IRQ2.
    #[save(id = 7)]
    mainlatch: u8,
    #[save(id = 8)]
    response_pending: bool,
    /// One-shot NMI to the 6502, set when a fresh command arrives.
    #[save(id = 9)]
    sound_nmi: bool,

    // -- Interrupt sources --------------------------------------------------
    /// The free-running periodic interrupt, cleared by the `/IRQACK` strobe.
    #[save(id = 10)]
    timed_int: bool,
    /// Sound-CPU cycles since the periodic interrupt last fired.
    #[save(id = 11)]
    irq_counter: u32,

    // -- Board I/O ----------------------------------------------------------
    /// Coin switches, bits 0 through 2. A set bit is a closed switch, which is
    /// the sense the `/RDIO` port reads them in.
    #[save(id = 12)]
    coin_inputs: u8,
    /// Operator self-test line, driven from the main board's service switch.
    #[save(id = 13)]
    self_test: bool,
    /// The bank / coin-counter / YM-reset latch.
    #[save(id = 14)]
    wrio: u8,
    /// The mix latch: POKEY volume in bits 4 and 5, YM2151 volume in bits 1
    /// through 3, low-pass filter enable in bit 0.
    #[save(id = 15)]
    mix: u8,

    /// Set when the main board pulses the reset line, so the next tick boots the
    /// CPU from its vector.
    #[save(id = 16)]
    reset_pending: bool,

    #[save(id = 17)]
    clock: u64,
}

impl AtariJsa1 {
    pub fn new(pokey: JsaPokey) -> Self {
        let rate = audio_sample_rate_hz();
        Self {
            shunt_pole: OnePole::default(),
            fixed_pole: OnePole::default(),
            fixed_a: OnePole::coefficient(FIXED_POLE_HZ, rate),
            shunt_open_a: OnePole::coefficient(SHUNT_OPEN_HZ, rate),
            shunt_closed_a: OnePole::coefficient(SHUNT_CLOSED_HZ, rate),
            cpu: M6502::new(),
            bus: Jsa1Bus {
                ram: Box::new([0; 0x2000]),
                rom: Box::new([0xFF; 0xC000]),
                bank_pages: Box::new([0xFF; 0x4000]),
                bank: 0,
                pokey: (pokey == JsaPokey::Fitted)
                    .then(|| Pokey::with_clock(SOUND_CLOCK_HZ, audio_sample_rate_hz())),
                ym: Ym2151::new(),
                soundlatch: 0,
                command_pending: false,
                mainlatch: 0,
                response_pending: false,
                sound_nmi: false,
                timed_int: false,
                irq_counter: 0,
                coin_inputs: 0,
                self_test: false,
                wrio: 0,
                mix: MIX_FULL,
                reset_pending: true,
                clock: 0,
            },
        }
    }

    /// Load the 64 KB sound chip.
    ///
    /// The chip is split across the map rather than mapped straight through:
    /// its low 16 KB are the four pages the `0x3000` window selects between, and
    /// the remaining 48 KB are the fixed ROM from `0x4000` up.
    pub fn load_rom(&mut self, image: &[u8]) {
        if image.len() < 0x10000 {
            return;
        }
        self.bus.bank_pages.copy_from_slice(&image[0x0000..0x4000]);
        self.bus.rom.copy_from_slice(&image[0x4000..0x10000]);
    }

    pub fn reset(&mut self) {
        // Clear the output filter's history so a loud passage before the reset
        // cannot bleed a settling transient into the frames after it.
        self.shunt_pole.reset();
        self.fixed_pole.reset();
        if let Some(pokey) = &mut self.bus.pokey {
            pokey.reset();
        }
        self.bus.ym.reset();
        self.bus.bank = 0;
        self.bus.soundlatch = 0;
        self.bus.command_pending = false;
        self.bus.mainlatch = 0;
        self.bus.response_pending = false;
        self.bus.sound_nmi = false;
        self.bus.timed_int = false;
        self.bus.irq_counter = 0;
        self.bus.coin_inputs = 0;
        self.bus.self_test = false;
        self.bus.wrio = 0;
        self.bus.mix = MIX_FULL;
        self.bus.reset_pending = true;
        self.bus.clock = 0;
    }

    // -- Main-board interface ------------------------------------------------

    /// The main board writes a sound command: latch it, flag it pending, and
    /// pulse the 6502's NMI.
    pub fn write_command(&mut self, data: u8) {
        self.bus.soundlatch = data;
        self.bus.command_pending = true;
        self.bus.sound_nmi = true;
    }

    /// The main board reads the response latch, which clears the pending flag
    /// and so drops its IRQ2.
    pub fn read_response(&mut self) -> u8 {
        self.bus.response_pending = false;
        self.bus.mainlatch
    }

    /// A command is latched but the sound CPU has not read it yet. This is the
    /// main board's status bit and the `/RDIO` port's NMI line state.
    pub fn command_pending(&self) -> bool {
        self.bus.command_pending
    }

    /// A response is latched for the main board, which is what raises its IRQ2.
    pub fn response_pending(&self) -> bool {
        self.bus.response_pending
    }

    /// The main board pulses the sound CPU's reset line.
    ///
    /// Unlike the System 1 board there is no latch holding the sound CPU down:
    /// this is a strobe, and the CPU reboots from its vector rather than
    /// stopping. Any response the main board had not collected is dropped with
    /// it, along with its interrupt.
    pub fn reset_pulse(&mut self) {
        self.bus.response_pending = false;
        self.bus.reset_pending = true;
    }

    /// Press or release a coin switch (`index` 0 through 3 for coins 1 to 4).
    /// The board carries four mechs; a cabinet need not fit them all.
    pub fn set_coin(&mut self, index: u8, pressed: bool) {
        let mask = 1u8 << (index & 0x03);
        if pressed {
            self.bus.coin_inputs |= mask;
        } else {
            self.bus.coin_inputs &= !mask;
        }
    }

    /// Drive the operator self-test line, which on the main board is the same
    /// switch that reads back on its own status port.
    pub fn set_self_test(&mut self, pressed: bool) {
        self.bus.self_test = pressed;
    }

    /// Advance the board by one sound-CPU cycle.
    pub fn tick(&mut self) {
        if self.bus.reset_pending {
            self.bus.reset_pending = false;
            self.bus.timed_int = false;
            self.bus.irq_counter = 0;
            self.cpu.reset(&mut self.bus, BusMaster::Cpu(1));
        }

        // The periodic interrupt is free-running: it is divided straight off the
        // crystal and neither the CPU nor the main board can stop it, only
        // acknowledge it.
        self.bus.irq_counter += 1;
        if self.bus.irq_counter >= IRQ_PERIOD_CYCLES {
            self.bus.irq_counter = 0;
            self.bus.timed_int = true;
        }

        self.cpu.execute_cycle(&mut self.bus, BusMaster::Cpu(1));
        if let Some(pokey) = &mut self.bus.pokey {
            pokey.tick();
        }
        self.bus.ym.tick(YM_CLOCKS_PER_TICK);
        self.bus.clock += 1;
    }

    /// Drain and mix the board's audio the way the board mixes it.
    ///
    /// The POKEY output is unipolar, sitting at 0 for silence, and the YM2151's
    /// is bipolar; both resample to the host rate, so they line up sample for
    /// sample. The result still carries the POKEY's DC, which the machine
    /// removes before output.
    ///
    /// Gains are the board's, derived in
    /// `docs/schematics/toobin-audio-output.md`, rather than fitted: the volume
    /// ladders are binary-weighted switches summing into a virtual ground, so
    /// each is linear in its code, and the two feedback resistors fix the ratio
    /// between them. The board's two channels become one here, and the FM's own
    /// panning was already gone before that: see the module header.
    ///
    /// The volume codes, the routing and the filter switch are sampled once per
    /// drain rather than per sample, so a change to any of them inside a frame
    /// takes effect at the next frame boundary. That is the same approximation
    /// the volumes were already under.
    pub fn drain_audio(&mut self) -> Vec<f32> {
        let ym_gain = self.ym_gain();
        // The POKEY reaches a speaker only through a leg gated by one of the
        // YM2151's CT pins. With both clear it reaches neither, whatever its
        // volume code says, so this is a mute and not a pan.
        let (ct1, ct2) = self.pokey_route();
        let pokey_gain = if ct1 || ct2 { self.pokey_gain() } else { 0.0 };
        let shunt_a = self.shunt_coefficient();
        let fixed_a = self.fixed_a;

        let ym = self.bus.ym.drain_audio();
        let pokey = self
            .bus
            .pokey
            .as_mut()
            .map(Pokey::drain_audio)
            .unwrap_or_default();

        let n = pokey.len().max(ym.len());
        let mut out = vec![0.0f32; n];
        for (i, slot) in out.iter_mut().enumerate() {
            let mixed = pokey.get(i).copied().unwrap_or(0.0) * pokey_gain
                + ym.get(i).copied().unwrap_or(0.0) * ym_gain;
            // Two cascaded one-pole sections, in the order the board has them:
            // the switched shunt at the output amplifier's input node, then the
            // fixed pole across its feedback.
            let shunted = self.shunt_pole.process(mixed, shunt_a);
            *slot = self.fixed_pole.process(shunted, fixed_a);
        }
        out
    }

    /// POKEY volume code from the mix register: four steps, bits 4 and 5.
    pub fn pokey_code(&self) -> u8 {
        (self.bus.mix >> 4) & 3
    }

    /// YM2151 volume code from the mix register: eight steps, bits 1 through 3.
    pub fn ym_code(&self) -> u8 {
        (self.bus.mix >> 1) & 7
    }

    /// The POKEY's gain into a channel mixer, as the board sets it.
    ///
    /// Its ladder sums into the 47k feedback of the summing amplifier, one step
    /// per 150k leg, and the result reaches each channel through a 12k leg into
    /// a 12k feedback, which is unity. So the whole path is `47k / 150k` per
    /// volume step, reaching 0.94 at code 3.
    pub fn pokey_gain(&self) -> f32 {
        POKEY_GAIN_PER_STEP * self.pokey_code() as f32
    }

    /// The YM2151's gain into a channel mixer, as the board sets it: its ladder
    /// sums into a 12k feedback one step per 30k leg, so `12k / 30k` per step,
    /// reaching 2.80 at code 7.
    pub fn ym_gain(&self) -> f32 {
        YM_GAIN_PER_STEP * self.ym_code() as f32
    }

    /// The YM2151's `CT1` and `CT2` pins, which on this board gate whether the
    /// POKEY reaches the left and the right speaker. Clearing both mutes it.
    pub fn pokey_route(&self) -> (bool, bool) {
        (self.bus.ym.ct1(), self.bus.ym.ct2())
    }

    /// The shunt pole's coefficient for the switch position the board is in.
    ///
    /// A transistor switches a second capacitor across the output amplifier's
    /// input node, and its base is driven from the mix register's `LPF` bit OR
    /// from `YM0`, the bottom bit of the YM volume field, wired through two 1k
    /// resistors. So the corner drops whenever the program asks for the filter
    /// *or* the YM volume code happens to be odd. The second of those looks
    /// like an accident of the design rather than an intent, and is flagged in
    /// `docs/schematics/toobin-audio-output.md` for a second reading.
    fn shunt_coefficient(&self) -> f32 {
        if self.bus.mix & 0x03 != 0 {
            self.shunt_closed_a
        } else {
            self.shunt_open_a
        }
    }

    /// Which of the four ROM pages the `0x3000` window presents.
    pub fn bank(&self) -> u8 {
        self.bus.bank
    }

    /// (sound-CPU cycles run, command_pending, response_pending) for headless
    /// bring-up diagnostics.
    pub fn debug_state(&self) -> (u64, bool, bool) {
        (
            self.bus.clock,
            self.bus.command_pending,
            self.bus.response_pending,
        )
    }
}

impl Default for AtariJsa1 {
    fn default() -> Self {
        Self::new(JsaPokey::Absent)
    }
}

impl Jsa1Bus {
    /// The `/RDIO` port: coin switches, the two handshake flags and self-test.
    fn read_rdio(&self) -> u8 {
        // Bit 6 idles high and falls while a command is waiting, which is the
        // NMI line the program can poll instead of taking the interrupt.
        let mut v = 0x40u8;
        if self.command_pending {
            v &= !0x40;
        }
        // Bit 5 rises while the main board has not collected a response.
        if self.response_pending {
            v |= 0x20;
        }
        // Bit 7 is the operator self-test switch, and bit 4 reports a speech
        // chip ready. With no speech socket populated bit 4 stays low.
        if self.self_test {
            v |= 0x80;
        }
        // Bits 3 through 0 are FOUR coin switches, not two. Sheet 22 of the
        // board's schematic package buffers COIN4 through COIN1 onto D3-D0,
        // each pulled up by 1k to VCC with 0.1 uF to ground and closing to
        // ground. This used to mask to two on the belief that D3 was a tied
        // +5V, which is what the part of the port nobody had read looked like
        // from the outside.
        //
        // The polarity here is the non-inverting reading of the buffer at 5J,
        // whose designator is ambiguous in the available scan and which the
        // board's own parts list would settle. It is the reading the sound
        // program's polling works under. See docs/schematics/toobin-audio-output.md.
        v | (self.coin_inputs & 0x0F)
    }

    /// The `/WRIO` latch: ROM bank, coin counters and the YM2151 reset line.
    fn write_wrio(&mut self, data: u8) {
        self.wrio = data;
        self.bank = (data >> 6) & 3;
        // Bit 0 is the YM2151's reset, active low. Bits 1 through 3 are the
        // speech chip's strobes and pitch select, which no board here fits.
        if data & 0x01 == 0 {
            self.ym.reset();
        }
    }
}

impl Bus for Jsa1Bus {
    type Address = u16;
    type Data = u8;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        match addr {
            0x0000..=0x1FFF => self.ram[addr as usize],
            0x2000..=0x27FF => self.ym.read(addr & 1),
            // The I/O block decodes on address bits 1, 2 and 9 only, so each
            // strobe answers across a wide span of the window.
            0x2800..=0x2BFF => match addr & 0x206 {
                // Reading the command latch acknowledges it and drops the NMI.
                0x002 => {
                    self.command_pending = false;
                    self.soundlatch
                }
                0x004 => self.read_rdio(),
                // The acknowledge strobe fires on the access, read or write.
                0x006 => {
                    self.timed_int = false;
                    0xFF
                }
                _ => 0xFF,
            },
            0x2C00..=0x2C0F => self.pokey.as_mut().map_or(0xFF, |p| p.read(addr & 0x0F)),
            0x3000..=0x3FFF => {
                self.bank_pages[(self.bank as usize) * 0x1000 + (addr & 0x0FFF) as usize]
            }
            0x4000..=0xFFFF => self.rom[(addr - 0x4000) as usize],
            _ => 0xFF,
        }
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        match addr {
            0x0000..=0x1FFF => self.ram[addr as usize] = data,
            0x2000..=0x27FF => self.ym.write(addr & 1, data),
            0x2800..=0x2BFF => match addr & 0x206 {
                0x006 => self.timed_int = false,
                // The speech data strobe, with no speech socket behind it.
                0x200 => {}
                0x202 => {
                    self.mainlatch = data;
                    self.response_pending = true; // raises the main board's IRQ2
                }
                0x204 => self.write_wrio(data),
                0x206 => self.mix = data,
                _ => {}
            },
            0x2C00..=0x2C0F => {
                if let Some(pokey) = &mut self.pokey {
                    pokey.write(addr & 0x0F, data);
                }
            }
            _ => {} // ROM, banked or fixed
        }
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        let nmi = self.sound_nmi;
        self.sound_nmi = false;
        InterruptState {
            nmi,
            // Only these two sources are wired to the 6502's IRQ. The POKEY's
            // own interrupt output is not: it is here to make sound, and its
            // timers are read by the program rather than taken.
            irq: self.timed_int || self.ym.irq(),
            ..Default::default()
        }
    }
}

impl phosphor_core::device::Device for AtariJsa1 {
    fn name(&self) -> &'static str {
        "Atari JSA-I"
    }

    fn reset(&mut self) {
        AtariJsa1::reset(self);
    }
}

impl phosphor_core::core::debug::Debuggable for AtariJsa1 {
    fn debug_registers(&self) -> Vec<phosphor_core::core::debug::DebugRegister> {
        use phosphor_core::core::debug::DebugRegister;
        vec![
            DebugRegister {
                name: "SND_CLK",
                value: self.bus.clock,
                width: 32,
            },
            DebugRegister {
                name: "BANK",
                value: self.bus.bank as u64,
                width: 2,
            },
            DebugRegister {
                name: "WRIO",
                value: self.bus.wrio as u64,
                width: 8,
            },
            DebugRegister {
                name: "MIX",
                value: self.bus.mix as u64,
                width: 8,
            },
            DebugRegister {
                name: "CMD",
                value: self.bus.soundlatch as u64,
                width: 8,
            },
            DebugRegister {
                name: "RESP",
                value: self.bus.mainlatch as u64,
                width: 8,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A board whose ROM is a program that answers every command.
    ///
    /// The 6502 boots at `0xF000` and polls the `/RDIO` port's NMI line rather
    /// than taking the interrupt: while bit 6 is high there is no command, and
    /// when it falls the program reads the command latch (which clears the flag)
    /// and writes the same byte back as the response. That is the whole
    /// handshake the main board depends on, so it is enough to exercise the
    /// latches without a real sound program.
    ///
    /// Both the NMI and the IRQ vector point at a bare RTI. They have to point
    /// somewhere: a command raises the NMI whether or not the program uses it,
    /// and the board's periodic interrupt fires on its own regardless. Leaving
    /// the vectors as erased ROM sends the CPU to 0xFFFF and the program never
    /// runs again, which is exactly what this fixture did when first written.
    fn board_with_echo_program() -> AtariJsa1 {
        let mut jsa = AtariJsa1::new(JsaPokey::Fitted);
        let mut image = vec![0xFFu8; 0x10000];

        // The fixed ROM starts at chip 0x4000 and maps to CPU 0x4000, so a
        // program at CPU 0xF000 sits at chip offset 0xF000.
        let prog: &[u8] = &[
            0xAD, 0x04, 0x28, // LDA $2804   the /RDIO port
            0x29, 0x40, //       AND #$40    the NMI line state
            0xD0, 0xF9, //       BNE $F000   still high: no command waiting
            0xAD, 0x02, 0x28, // LDA $2802   read the command, clearing the flag
            0x8D, 0x02, 0x2A, // STA $2A02   echo it back as the response
            0x4C, 0x00, 0xF0, // JMP $F000
        ];
        image[0xF000..0xF000 + prog.len()].copy_from_slice(prog);
        image[0xF040] = 0x40; // RTI

        image[0xFFFA] = 0x40; // NMI   -> 0xF040
        image[0xFFFB] = 0xF0;
        image[0xFFFC] = 0x00; // RESET -> 0xF000
        image[0xFFFD] = 0xF0;
        image[0xFFFE] = 0x40; // IRQ   -> 0xF040
        image[0xFFFF] = 0xF0;

        jsa.load_rom(&image);
        jsa
    }

    fn run(jsa: &mut AtariJsa1, cycles: usize) {
        for _ in 0..cycles {
            jsa.tick();
        }
    }

    #[test]
    fn the_rom_splits_into_four_banks_and_a_fixed_window() {
        let mut jsa = AtariJsa1::new(JsaPokey::Absent);
        let mut image = vec![0u8; 0x10000];
        // Stamp each 4 KB page of the low 16 KB with its own page number.
        for page in 0..4usize {
            image[page * 0x1000] = 0xA0 + page as u8;
        }
        image[0x4000] = 0x5A; // first byte of the fixed window
        image[0xFFFF] = 0x5B; // last byte of it
        jsa.load_rom(&image);

        for page in 0..4u8 {
            jsa.bus.write_wrio((page << 6) | 0x01);
            assert_eq!(jsa.bank(), page);
            assert_eq!(
                jsa.bus.read(BusMaster::Cpu(1), 0x3000),
                0xA0 + page,
                "bank {page} in the 0x3000 window"
            );
        }

        assert_eq!(jsa.bus.read(BusMaster::Cpu(1), 0x4000), 0x5A);
        assert_eq!(jsa.bus.read(BusMaster::Cpu(1), 0xFFFF), 0x5B);
    }

    /// The whole reason this board exists for Toobin': the command and response
    /// latches, and the flags either side reads them through.
    #[test]
    fn the_command_response_handshake_completes() {
        let mut jsa = board_with_echo_program();
        run(&mut jsa, 200); // let it boot and settle into the loop

        assert!(!jsa.command_pending());
        assert!(!jsa.response_pending());

        jsa.write_command(0x42);
        assert!(
            jsa.command_pending(),
            "the flag is up as soon as it latches"
        );

        run(&mut jsa, 400);

        assert!(!jsa.command_pending(), "the sound CPU collected it");
        assert!(jsa.response_pending(), "and answered");
        assert_eq!(jsa.read_response(), 0x42);
        assert!(!jsa.response_pending(), "reading it drops the main IRQ");
    }

    /// The `/RDIO` port carries the same two flags, which is how a sound program
    /// that polls rather than taking the NMI still sees a command arrive.
    #[test]
    fn the_rdio_port_reports_both_handshake_flags() {
        let mut jsa = AtariJsa1::new(JsaPokey::Fitted);

        // Idle: the NMI line reads high and nothing else is set.
        assert_eq!(jsa.bus.read_rdio(), 0x40);

        jsa.write_command(0x11);
        assert_eq!(
            jsa.bus.read_rdio() & 0x40,
            0,
            "command waiting pulls it low"
        );

        jsa.bus.response_pending = true;
        assert_eq!(jsa.bus.read_rdio() & 0x20, 0x20, "response not collected");

        jsa.set_self_test(true);
        assert_eq!(jsa.bus.read_rdio() & 0x80, 0x80, "self test");
    }

    /// Coin switches are on this board, so a game without it has no coin input
    /// at all. They read back closed-is-high.
    #[test]
    fn coin_switches_read_back_on_the_io_port() {
        let mut jsa = AtariJsa1::new(JsaPokey::Fitted);
        assert_eq!(jsa.bus.read_rdio() & 0x03, 0);

        jsa.set_coin(0, true);
        assert_eq!(jsa.bus.read_rdio() & 0x03, 0x01);
        jsa.set_coin(1, true);
        assert_eq!(jsa.bus.read_rdio() & 0x03, 0x03);
        jsa.set_coin(0, false);
        assert_eq!(jsa.bus.read_rdio() & 0x03, 0x02);
    }

    /// The periodic interrupt is free-running and is cleared only by its
    /// acknowledge strobe, which answers a read or a write alike.
    #[test]
    fn the_periodic_interrupt_fires_on_its_own_and_acknowledges() {
        let mut jsa = AtariJsa1::new(JsaPokey::Absent);
        jsa.bus.reset_pending = false;

        assert!(!jsa.bus.timed_int);
        run(&mut jsa, IRQ_PERIOD_CYCLES as usize - 1);
        assert!(!jsa.bus.timed_int, "not yet");
        run(&mut jsa, 1);
        assert!(jsa.bus.timed_int, "fires at its period");

        // A read of the acknowledge strobe clears it.
        jsa.bus.read(BusMaster::Cpu(1), 0x2806);
        assert!(!jsa.bus.timed_int);

        // So does a write.
        jsa.bus.timed_int = true;
        jsa.bus.write(BusMaster::Cpu(1), 0x2806, 0);
        assert!(!jsa.bus.timed_int);
    }

    /// The mix register sets both volume codes, and each code maps to the gain
    /// the board's ladder actually gives it.
    #[test]
    fn the_mix_register_sets_both_volumes() {
        let mut jsa = AtariJsa1::new(JsaPokey::Fitted);

        // Powers up at full so a board that never writes it is still audible.
        assert_eq!(jsa.ym_code(), 7);
        assert_eq!(jsa.pokey_code(), 3);
        // The two feedback resistors fix the ratio between the sources, and at
        // full codes the board is about 3 to 1 in the FM's favor.
        assert!((jsa.ym_gain() - 2.80).abs() < 0.01, "{}", jsa.ym_gain());
        assert!(
            (jsa.pokey_gain() - 0.94).abs() < 0.01,
            "{}",
            jsa.pokey_gain()
        );

        jsa.bus.write(BusMaster::Cpu(1), 0x2A06, 0x00);
        assert_eq!(jsa.ym_gain(), 0.0);
        assert_eq!(jsa.pokey_gain(), 0.0);

        // POKEY at 2 of 3, YM at 4 of 7. Both ladders are linear in the code.
        jsa.bus.write(BusMaster::Cpu(1), 0x2A06, 0x20 | 0x08);
        assert_eq!(jsa.pokey_code(), 2);
        assert_eq!(jsa.ym_code(), 4);
        assert!((jsa.pokey_gain() - 2.0 * POKEY_GAIN_PER_STEP).abs() < 1e-6);
        assert!((jsa.ym_gain() - 4.0 * YM_GAIN_PER_STEP).abs() < 1e-6);
    }

    /// The POKEY reaches a speaker only through legs gated by the YM2151's CT
    /// pins, so clearing both is a mute however the volume is set. This is the
    /// one part of the output stage that can silence a source outright, which
    /// is why it is worth a test rather than a comment.
    #[test]
    fn clearing_both_ct_pins_mutes_the_pokey() {
        let mut jsa = AtariJsa1::new(JsaPokey::Fitted);

        // YM2151 register 0x1B carries CT2 in bit 7 and CT1 in bit 6.
        let set_ct = |jsa: &mut AtariJsa1, v: u8| {
            jsa.bus.write(BusMaster::Cpu(1), 0x2000, 0x1B);
            jsa.bus.write(BusMaster::Cpu(1), 0x2001, v);
        };

        set_ct(&mut jsa, 0x00);
        assert_eq!(jsa.pokey_route(), (false, false));

        set_ct(&mut jsa, 0x40);
        assert_eq!(jsa.pokey_route(), (true, false), "CT1 only");
        set_ct(&mut jsa, 0x80);
        assert_eq!(jsa.pokey_route(), (false, true), "CT2 only");
        set_ct(&mut jsa, 0xC0);
        assert_eq!(jsa.pokey_route(), (true, true));
    }

    /// The shunt's corner drops when the program asks for the filter, and also
    /// whenever the YM volume code is odd, because the bottom bit of that field
    /// is wired to the same transistor base.
    #[test]
    fn the_low_pass_switches_on_lpf_or_an_odd_ym_code() {
        let mut jsa = AtariJsa1::new(JsaPokey::Fitted);
        let open = jsa.shunt_open_a;
        let closed = jsa.shunt_closed_a;
        assert!(closed < open, "the closed switch is the lower corner");

        // Even YM code, filter off.
        jsa.bus.write(BusMaster::Cpu(1), 0x2A06, 0x04);
        assert_eq!(jsa.shunt_coefficient(), open);

        // Filter bit alone.
        jsa.bus.write(BusMaster::Cpu(1), 0x2A06, 0x05);
        assert_eq!(jsa.shunt_coefficient(), closed);

        // Odd YM code alone, with the filter bit clear.
        jsa.bus.write(BusMaster::Cpu(1), 0x2A06, 0x02);
        assert_eq!(jsa.shunt_coefficient(), closed, "YM0 reaches the same base");
    }

    /// A board with no POKEY fitted reads its window as open bus and takes
    /// writes to it without panicking.
    #[test]
    fn an_absent_pokey_is_open_bus() {
        let mut jsa = AtariJsa1::new(JsaPokey::Absent);
        assert_eq!(jsa.bus.read(BusMaster::Cpu(1), 0x2C00), 0xFF);
        jsa.bus.write(BusMaster::Cpu(1), 0x2C00, 0x55);
        assert!(jsa.drain_audio().is_empty() || jsa.drain_audio().iter().all(|&s| s == 0.0));
    }

    /// A reset pulse reboots the sound CPU and drops a response the main board
    /// never collected.
    #[test]
    fn a_reset_pulse_drops_an_uncollected_response() {
        let mut jsa = board_with_echo_program();
        run(&mut jsa, 200);

        jsa.write_command(0x77);
        run(&mut jsa, 400);
        assert!(jsa.response_pending());

        jsa.reset_pulse();
        assert!(!jsa.response_pending(), "the response goes with the reset");

        // And the CPU comes back up and still answers.
        run(&mut jsa, 200);
        jsa.write_command(0x33);
        run(&mut jsa, 400);
        assert_eq!(jsa.read_response(), 0x33);
    }

    #[test]
    fn save_load_round_trip() {
        use phosphor_core::core::save_state::{Saveable, StateReader, StateWriter};

        let mut jsa = board_with_echo_program();
        run(&mut jsa, 500);
        jsa.bus.write_wrio(0x80 | 0x01); // bank 2
        jsa.bus.mix = 0x2A;
        jsa.write_command(0x5A);
        run(&mut jsa, 200);
        let before = jsa.debug_state();

        let mut w = StateWriter::new();
        jsa.save_state(&mut w);
        let bytes = w.into_vec();

        let mut other = board_with_echo_program();
        let mut r = StateReader::new(&bytes);
        other.load_state(&mut r).unwrap();

        assert_eq!(other.debug_state(), before);
        assert_eq!(other.bank(), 2);
        assert_eq!(other.bus.mix, 0x2A);
    }
}
