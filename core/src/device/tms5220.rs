//! TMS5220 / TMS5220C voice synthesis processor — host interface and FIFO.
//!
//! This models the chip's parallel host interface and its 16-byte FIFO: the
//! command decode, the `speak-external` data path, and the `/READY` / `/INT` /
//! status handshake a host CPU polls while streaming speech. The LPC synthesis
//! that turns FIFO data into audio (frame parse, interpolation, the 10-pole
//! lattice filter) is a separate follow-up; until it lands the chip runs the
//! handshake correctly but is silent.
//!
//! # Host-agnostic
//!
//! The device exposes only the raw chip pins/registers. It knows nothing about
//! the bridge chip a given board uses to reach it: on Atari System 1 (Road
//! Runner) the CPU reaches it through a [`crate::device::via6522::Via6522`]
//! (Port A = data / status byte; Port B = `/WS`, `/RS`, `/READY`, `/INT`, plus
//! a clock-select line the board turns into [`Tms5220::set_clock`]); Atari's
//! Star Wars reaches the same chip through a MOS6532 RIOT instead. Each board
//! translates its bridge's port operations into [`Tms5220::data_w`] /
//! [`Tms5220::status_r`] / [`Tms5220::ready`] / [`Tms5220::int_asserted`] and
//! decides when to call them.
//!
//! # Variants
//!
//! [`Tms52xxVariant::Tms5220`] (e.g. Star Wars) and [`Tms52xxVariant::Tms5220C`]
//! (e.g. Road Runner) share the LPC tables and synthesis; the only difference
//! modeled here is that the `0x00`/`0x20` command is `SET RATE` on the 5220C and
//! a NOP on the 5220.
//!
//! # Speech ROM (VSM)
//!
//! Neither consumer wires a VSM speech ROM — both stream to the FIFO. The
//! VSM-only commands (`LOAD ADDRESS`, `SPEAK`, `READ BYTE`, `READ AND BRANCH`)
//! are decoded but have no effect.

use crate::audio::AudioResampler;
use crate::core::debug::{DebugRegister, Debuggable};
use crate::device::Device;
use phosphor_macros::Saveable;

/// FIFO depth in bytes.
const FIFO_SIZE: usize = 16;

/// Host output sample rate the internal resampler targets.
const OUTPUT_SAMPLE_RATE: u64 = 44_100;

/// TMS5220C nominal master clock on Atari System 1: 14.318181 MHz / 2 / 11.
/// The board re-selects the exact rate at runtime via [`Tms5220::set_clock`].
const TMS5220C_NOMINAL_HZ: u32 = 650_826;

/// The chip synthesizes one sample every 80 master clocks.
fn sample_rate(clock_hz: u32) -> u64 {
    (clock_hz / 80).max(1) as u64
}

/// TMS52xx family variant. Selects the handful of variant-gated behaviors; the
/// LPC coefficient tables and synthesis are shared.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tms52xxVariant {
    /// Base TMS5220 (1983). `0x00`/`0x20` is a NOP.
    Tms5220,
    /// TMS5220C (1986+). `0x00`/`0x20` is the `SET RATE` command.
    Tms5220C,
}

/// TMS5220 / TMS5220C voice synthesis processor.
///
/// Runtime state (FIFO + handshake flags) is serialized; the variant, clock,
/// and resampler are configuration preserved across reset.
#[derive(Saveable)]
#[save_version(1)]
pub struct Tms5220 {
    /// Speak-external FIFO (ring buffer).
    fifo: [u8; FIFO_SIZE],
    fifo_head: u8,
    fifo_tail: u8,
    fifo_count: u8,

    /// SPEN: synthesizer enabled (set by a speak command / FIFO passing
    /// half-full; cleared by reset or buffer-empty in speak-external mode).
    spen: bool,
    /// TALKD: synthesis actively producing samples. Always false until the LPC
    /// synthesizer lands; kept here so `talk_status` is already faithful.
    talkd: bool,
    /// DDIS: speak-external (FIFO) mode, entered by the SPEAK EXTERNAL command.
    ddis: bool,

    /// BL: buffer low (FIFO half-empty or less).
    buffer_low: bool,
    /// BE: buffer empty.
    buffer_empty: bool,
    /// Previous talk-status, for edge-detecting the falling `/INT`.
    previous_talk_status: bool,
    /// `/INT` pin state (true = interrupt asserted).
    irq_pin: bool,

    /// 5220C SET RATE value (low nibble of the last set-rate command).
    c_variant_rate: u8,

    #[save_skip]
    variant: Tms52xxVariant,
    #[save_skip]
    clock_hz: u32,
    #[save_skip]
    resampler: AudioResampler<f32>,
}

impl Tms5220 {
    /// Create a TMS5220C at the Atari System 1 nominal clock. The board resets
    /// the exact rate via [`set_clock`](Self::set_clock).
    pub fn new() -> Self {
        Self::with_variant(Tms52xxVariant::Tms5220C, TMS5220C_NOMINAL_HZ)
    }

    /// Create a chip of the given variant and master clock (e.g. Star Wars:
    /// `with_variant(Tms52xxVariant::Tms5220, 672_000)`).
    pub fn with_variant(variant: Tms52xxVariant, clock_hz: u32) -> Self {
        let clock_hz = clock_hz.max(1);
        let mut chip = Self {
            fifo: [0; FIFO_SIZE],
            fifo_head: 0,
            fifo_tail: 0,
            fifo_count: 0,
            spen: false,
            talkd: false,
            ddis: false,
            buffer_low: true,
            buffer_empty: true,
            previous_talk_status: false,
            irq_pin: false,
            c_variant_rate: 0,
            variant,
            clock_hz,
            resampler: AudioResampler::new(sample_rate(clock_hz), OUTPUT_SAMPLE_RATE),
        };
        chip.reset();
        chip
    }

    /// Reset to the idle power-on state. Configuration (variant, clock) is
    /// preserved; the FIFO and handshake are cleared. The `/INT` pin starts
    /// inactive and both buffer flags start active (empty FIFO).
    pub fn reset(&mut self) {
        self.fifo = [0; FIFO_SIZE];
        self.fifo_head = 0;
        self.fifo_tail = 0;
        self.fifo_count = 0;
        self.spen = false;
        self.talkd = false;
        self.ddis = false;
        self.buffer_low = true;
        self.buffer_empty = true;
        self.previous_talk_status = false;
        self.irq_pin = false;
        self.c_variant_rate = 0;
        self.resampler.reset();
    }

    /// Write a byte to the chip (Port A / data port, latched on `/WS`). In
    /// speak-external mode the byte enters the FIFO; otherwise it is a command.
    pub fn data_w(&mut self, data: u8) {
        if self.ddis {
            // Speak-external mode: append to the FIFO if there is room.
            if (self.fifo_count as usize) < FIFO_SIZE {
                let old_buffer_low = self.buffer_low;
                self.fifo[self.fifo_tail as usize] = data;
                self.fifo_tail = (self.fifo_tail + 1) % FIFO_SIZE as u8;
                self.fifo_count += 1;
                self.update_fifo_status_and_ints();

                // Once the FIFO fills past half (BL falls) and we were not yet
                // speaking, SPEN goes active and synthesis begins.
                if !self.spen && old_buffer_low && !self.buffer_low {
                    self.spen = true;
                }
            }
            // FIFO full: byte is dropped; `ready()` already reports not-ready.
        } else {
            self.process_command(data);
        }
    }

    /// Decode and execute a command byte (bits 6-4 select the command).
    fn process_command(&mut self, cmd: u8) {
        match cmd & 0x70 {
            0x00 | 0x20 => {
                // SET RATE on the 5220C; NOP on the 5220.
                if self.variant == Tms52xxVariant::Tms5220C {
                    self.c_variant_rate = cmd & 0x0F;
                }
            }
            // READ BYTE / READ AND BRANCH / LOAD ADDRESS / SPEAK are all VSM
            // (speech-ROM) operations. No VSM is wired, so they are no-ops.
            0x10 | 0x30 | 0x40 | 0x50 => {}
            0x60 => {
                // SPEAK EXTERNAL: clear the FIFO and enter speak-external mode.
                self.fifo = [0; FIFO_SIZE];
                self.fifo_head = 0;
                self.fifo_tail = 0;
                self.fifo_count = 0;
                self.ddis = true;
            }
            0x70 => self.reset(),
            _ => unreachable!("cmd & 0x70 is always one of the above"),
        }
        self.update_fifo_status_and_ints();
    }

    /// Recompute the BL / BE flags and `/INT` from the current FIFO level.
    fn update_fifo_status_and_ints(&mut self) {
        // BL: buffer low when 8 or fewer bytes remain; interrupt on its rising
        // edge.
        if self.fifo_count <= 8 {
            if !self.buffer_low {
                self.buffer_low = true;
                self.irq_pin = true;
            }
        } else {
            self.buffer_low = false;
        }

        // BE: buffer empty at zero bytes; interrupt on its rising edge. In
        // speak-external mode an empty buffer ends speech (clears TALKD/SPEN).
        if self.fifo_count == 0 {
            if !self.buffer_empty {
                self.buffer_empty = true;
                self.irq_pin = true;
            }
            if self.ddis {
                self.talkd = false;
                self.spen = false;
            }
        } else {
            self.buffer_empty = false;
        }

        // Interrupt on a falling talk-status edge, and leave speak-external
        // mode when speech stops.
        let talk = self.talk_status();
        if self.previous_talk_status && !talk {
            self.irq_pin = true;
            self.ddis = false;
        }
        self.previous_talk_status = talk;
    }

    /// Talk status (status bit 7): speaking or enabled to speak.
    fn talk_status(&self) -> bool {
        self.spen || self.talkd
    }

    /// Read the status register (Port A on the `/RS` strobe). Reading status
    /// clears the `/INT` pin. Bit 7 = talk status, bit 6 = buffer low, bit 5 =
    /// buffer empty.
    pub fn status_r(&mut self) -> u8 {
        self.irq_pin = false;
        ((self.talk_status() as u8) << 7)
            | ((self.buffer_low as u8) << 6)
            | ((self.buffer_empty as u8) << 5)
    }

    /// `/READY` line: `true` when the chip can accept the next byte. Ready
    /// unless the FIFO is full while in speak-external mode.
    pub fn ready(&self) -> bool {
        (self.fifo_count as usize) < FIFO_SIZE || !self.ddis
    }

    /// `/INT` line: `true` while an interrupt is asserted.
    pub fn int_asserted(&self) -> bool {
        self.irq_pin
    }

    /// Retune the master clock (Hz). The board drives this from its clock-select
    /// line; the synthesis sample rate follows as `clock / 80`.
    pub fn set_clock(&mut self, clock_hz: u32) {
        let clock_hz = clock_hz.max(1);
        if clock_hz == self.clock_hz {
            return;
        }
        self.clock_hz = clock_hz;
        self.resampler.set_input_rate(sample_rate(clock_hz));
    }

    /// Drain synthesized audio at the host sample rate. Silent (empty) until the
    /// LPC synthesizer lands.
    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.resampler.drain_audio()
    }
}

impl Default for Tms5220 {
    fn default() -> Self {
        Self::new()
    }
}

impl Device for Tms5220 {
    fn name(&self) -> &'static str {
        match self.variant {
            Tms52xxVariant::Tms5220 => "TMS5220",
            Tms52xxVariant::Tms5220C => "TMS5220C",
        }
    }

    fn reset(&mut self) {
        // Inherent method takes priority; this is not recursive.
        self.reset();
    }

    // `tick` is a no-op until the LPC synthesizer lands.
}

impl Debuggable for Tms5220 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "TS",
                value: self.talk_status() as u64,
                width: 8,
            },
            DebugRegister {
                name: "BL",
                value: self.buffer_low as u64,
                width: 8,
            },
            DebugRegister {
                name: "BE",
                value: self.buffer_empty as u64,
                width: 8,
            },
            DebugRegister {
                name: "INT",
                value: self.irq_pin as u64,
                width: 8,
            },
            DebugRegister {
                name: "FIFO",
                value: self.fifo_count as u64,
                width: 8,
            },
            DebugRegister {
                name: "DDIS",
                value: self.ddis as u64,
                width: 8,
            },
            DebugRegister {
                name: "RATE",
                value: self.c_variant_rate as u64,
                width: 8,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::save_state::{Saveable, StateReader, StateWriter};

    // Command bytes (bits 6-4).
    const CMD_SET_RATE: u8 = 0x20;
    const CMD_SPEAK_EXTERNAL: u8 = 0x60;
    const CMD_RESET: u8 = 0x70;

    #[test]
    fn new_defaults_to_5220c_idle() {
        let mut tms = Tms5220::new();
        assert_eq!(tms.name(), "TMS5220C");
        assert!(tms.ready(), "idle chip is ready");
        assert!(!tms.int_asserted());
        // Idle: not talking (bit 7), buffer low (bit 6) + empty (bit 5) active.
        assert_eq!(tms.status_r(), 0b0110_0000);
    }

    #[test]
    fn with_variant_selects_base_5220() {
        let tms = Tms5220::with_variant(Tms52xxVariant::Tms5220, 672_000);
        assert_eq!(tms.name(), "TMS5220");
    }

    #[test]
    fn speak_external_streams_and_starts_talking() {
        let mut tms = Tms5220::new();
        tms.data_w(CMD_SPEAK_EXTERNAL);
        // Eight bytes keep the buffer at/under half — not talking yet.
        for _ in 0..8 {
            assert!(tms.ready());
            tms.data_w(0xA5);
        }
        assert_eq!(tms.status_r() & 0x80, 0, "TS still clear at 8 bytes");
        // The ninth byte pushes past half-full: BL falls, SPEN (talk) rises.
        tms.data_w(0xA5);
        assert_ne!(tms.status_r() & 0x80, 0, "TS set after 9 bytes");
        assert_eq!(tms.status_r() & 0x40, 0, "BL clear above half-full");
    }

    #[test]
    fn fifo_fills_and_reports_not_ready() {
        let mut tms = Tms5220::new();
        tms.data_w(CMD_SPEAK_EXTERNAL);
        for _ in 0..FIFO_SIZE {
            assert!(tms.ready());
            tms.data_w(0x00);
        }
        assert!(
            !tms.ready(),
            "full FIFO in speak-external mode is not ready"
        );
    }

    #[test]
    fn reset_clears_talk_and_fifo() {
        let mut tms = Tms5220::new();
        tms.data_w(CMD_SPEAK_EXTERNAL);
        for _ in 0..12 {
            tms.data_w(0xA5);
        }
        assert_ne!(tms.status_r() & 0x80, 0, "talking before reset");
        // reset() is the board's power-on/reset path.
        tms.reset();
        assert_eq!(tms.status_r() & 0x80, 0, "reset clears talk");
        assert_eq!(tms.status_r() & 0x20, 0x20, "reset leaves FIFO empty");
        assert!(tms.ready());
    }

    #[test]
    fn bytes_in_speak_external_mode_are_data_not_commands() {
        let mut tms = Tms5220::new();
        tms.data_w(CMD_SPEAK_EXTERNAL);
        // Once in speak-external mode every write is FIFO data — even a byte
        // that looks like the RESET command. So the buffer is no longer empty.
        tms.data_w(CMD_RESET);
        assert_eq!(
            tms.status_r() & 0x20,
            0,
            "byte was buffered as data, not executed"
        );
    }

    #[test]
    fn set_rate_is_variant_gated() {
        let mut c = Tms5220::with_variant(Tms52xxVariant::Tms5220C, TMS5220C_NOMINAL_HZ);
        c.data_w(CMD_SET_RATE | 0x0A);
        assert_eq!(rate_reg(&c), 0x0A, "5220C stores the rate nibble");

        let mut base = Tms5220::with_variant(Tms52xxVariant::Tms5220, 672_000);
        base.data_w(CMD_SET_RATE | 0x0A);
        assert_eq!(rate_reg(&base), 0, "5220 treats set-rate as a NOP");
    }

    #[test]
    fn vsm_commands_are_inert() {
        let mut tms = Tms5220::new();
        for cmd in [0x10, 0x30, 0x40, 0x50] {
            tms.data_w(cmd);
            assert_eq!(tms.status_r() & 0x80, 0, "VSM command does not talk");
            assert!(tms.ready());
        }
    }

    #[test]
    fn sample_rate_tracks_clock() {
        assert_eq!(sample_rate(650_826), 8135); // 5220C nominal / 80
        assert_eq!(sample_rate(795_454), 9943); // System 1 high-select
        assert_eq!(sample_rate(672_000), 8400); // Star Wars
    }

    #[test]
    fn set_clock_is_a_guarded_noop_at_same_rate() {
        let mut tms = Tms5220::new();
        tms.set_clock(0); // clamped, no panic
        tms.set_clock(795_454); // retune
        assert!(tms.drain_audio().is_empty(), "still silent");
    }

    #[test]
    fn save_load_round_trip() {
        let mut tms = Tms5220::new();
        tms.data_w(CMD_SPEAK_EXTERNAL);
        for _ in 0..10 {
            tms.data_w(0x5A);
        }
        let talking = tms.status_r() & 0x80;
        let ready = tms.ready();

        let mut w = StateWriter::new();
        tms.save_state(&mut w);
        let bytes = w.into_vec();
        assert!(!bytes.is_empty(), "device has runtime state to persist");

        let mut restored = Tms5220::new();
        let mut r = StateReader::new(&bytes);
        restored.load_state(&mut r).unwrap();
        assert_eq!(restored.status_r() & 0x80, talking);
        assert_eq!(restored.ready(), ready);
        assert_eq!(rate_reg(&restored), 0);
    }

    /// Read the RATE debug register value.
    fn rate_reg(tms: &Tms5220) -> u64 {
        tms.debug_registers()
            .into_iter()
            .find(|r| r.name == "RATE")
            .unwrap()
            .value
    }
}
