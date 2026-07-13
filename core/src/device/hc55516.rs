//! Harris HC-55516 CVSD (Continuously Variable Slope Delta) speech decoder.
//!
//! Used on Williams Sinistar (and Blaster/Playball) for the "sini-scream" and
//! digitized voice. The sound CPU bit-bangs a 1-bit delta-modulated stream: it
//! sets the data bit on the sound PIA's CA2 line ([`digit_w`](Hc55516::digit_w))
//! then toggles the clock on CB2 ([`clock_w`](Hc55516::clock_w)). On each active
//! (rising) clock edge the bit is shifted into a syllabic-companding integrator
//! whose output is the reconstructed audio sample.
//!
//! This is an integer model of the software-clocked decode path (Sinistar drives
//! the clock from the CPU; there is no external oscillator). The device holds one
//! output sample; the board reads [`sample_i16`](Hc55516::sample_i16) each tick
//! and mixes it with the DAC.
//!
//! HC55516 device constants: `sylmask 0xfc0`, `sylshift 6`, `syladd 0xfc1`,
//! `intshift 4`, `shiftreg_mask 0x7`, active clock edge = rising.

use phosphor_macros::Saveable;

// HC55516-specific device constants.
const SHIFTREG_MASK: u8 = 0x07;
const SYLMASK: i32 = 0xfc0;
const SYLSHIFT: i32 = 6;
const SYLADD: i32 = 0xfc1;
const INTSHIFT: i32 = 4;

/// Sign-extend the low `bits` bits of `value` to a full `i32`.
#[inline]
fn sext(value: i32, bits: u32) -> i32 {
    let shift = 32 - bits;
    (value << shift) >> shift
}

/// Harris HC-55516 CVSD decoder (software-clocked).
#[derive(Saveable)]
#[save_version(1)]
pub struct Hc55516 {
    /// 3-bit coincidence shift register (only the low `SHIFTREG_MASK` bits matter).
    shiftreg: u8,
    /// Last clock level seen, for edge detection.
    last_clock_state: bool,
    /// Data bit latched by `digit_w`, consumed on the next active clock edge.
    buffered_bit: bool,
    /// Syllabic (slope) filter accumulator, 12-bit.
    sylfilter: i32,
    /// Integrator accumulator, 10-bit signed.
    intfilter: i32,
    /// Most recent reconstructed sample, scaled to signed 16-bit.
    next_sample: i32,
    /// Automatic gain control flag (mirrors the AGC pin; exposed for parity).
    agc: bool,
    /// Buffered /FZ (force-zero) state; high/inactive in the Sinistar wiring.
    buffered_fzq: bool,
}

impl Default for Hc55516 {
    /// Power-on construction state.
    fn default() -> Self {
        Self {
            shiftreg: 0,
            last_clock_state: false,
            buffered_bit: false,
            sylfilter: 0,
            intfilter: 0,
            next_sample: 0,
            agc: true,
            buffered_fzq: true,
        }
    }
}

impl Hc55516 {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset — simulates /FZ having been held for a while (syllabic filter
    /// pre-charged, integrator cleared).
    pub fn reset(&mut self) {
        self.last_clock_state = false;
        self.sylfilter = 0x3f;
        self.intfilter = 0;
        self.agc = true;
        self.buffered_fzq = true;
    }

    /// Latch the data bit (sound PIA CA2). Consumed on the next active clock edge.
    pub fn digit_w(&mut self, bit: bool) {
        self.buffered_bit = bit;
    }

    /// Drive the clock line (sound PIA CB2). Processing runs on every clock
    /// change; the shift register only advances on the active (rising) edge.
    pub fn clock_w(&mut self, state: bool) {
        if state != self.last_clock_state {
            self.process_bit(self.buffered_bit, state);
        }
        self.last_clock_state = state;
    }

    /// Current reconstructed output as signed 16-bit PCM.
    pub fn sample_i16(&self) -> i16 {
        self.next_sample.clamp(i16::MIN as i32, i16::MAX as i32) as i16
    }

    /// AGC pin state (true when the integrator is within the AGC window).
    pub fn agc(&self) -> bool {
        self.agc
    }

    #[inline]
    fn is_active_clock_transition(&self, clock_state: bool) -> bool {
        // Active edge is rising: transition into the high state.
        clock_state != self.last_clock_state && clock_state
    }

    /// Core CVSD step: syllabic-companding delta integration for one clock edge.
    fn process_bit(&mut self, mut bit: bool, clock_state: bool) {
        let frozen = (self.intfilter >= 0x180 && !bit) || (self.intfilter <= -0x180 && bit);
        let sum: i32;

        if self.is_active_clock_transition(clock_state) {
            // /FZ active forces the inverse of the previous bit instead of the input.
            if !self.buffered_fzq {
                bit = (self.shiftreg & 1) == 0;
            }

            // Shift the new bit into the coincidence register.
            self.shiftreg = (self.shiftreg << 1) | (bit as u8);

            let coincidence = self.shiftreg & SHIFTREG_MASK;
            if coincidence == 0 || coincidence == SHIFTREG_MASK {
                // All 0's or all 1's in the last n bits.
                if !frozen {
                    self.sylfilter += ((!self.sylfilter) & SYLMASK) >> SYLSHIFT;
                }
            } else if !frozen {
                self.sylfilter += (((!self.sylfilter) & SYLMASK) >> SYLSHIFT) + SYLADD;
            }
            self.sylfilter &= 0xfff;

            sum = sext(((!self.intfilter) >> INTSHIFT) + 1, 10);
        } else {
            // Inactive clock transition: slew by the syllabic filter magnitude.
            if self.shiftreg & 1 != 0 {
                sum = sext((!(self.sylfilter >> 6).max(2)) + 1, 10);
            } else {
                sum = sext((self.sylfilter >> 6).max(2), 10);
            }
        }

        if !frozen {
            self.intfilter = sext(self.intfilter + sum, 10);
        }

        // Scale the 10-bit result (-512..511) to signed 16-bit.
        self.next_sample = (self.intfilter << 6) | (((self.intfilter & 0x3ff) ^ 0x200) >> 4);

        // AGC is asserted while the integrator stays within +/-0x100.
        self.agc = !(self.intfilter >= 0x100 || self.intfilter <= -0x100);
    }
}

impl super::Device for Hc55516 {
    fn name(&self) -> &'static str {
        "HC55516 CVSD"
    }
    fn reset(&mut self) {
        self.reset();
    }
}

use crate::core::debug::{DebugRegister, Debuggable};

impl Debuggable for Hc55516 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "SHIFT",
                value: self.shiftreg as u64,
                width: 8,
            },
            DebugRegister {
                name: "SYL",
                value: self.sylfilter as u64,
                width: 12,
            },
            DebugRegister {
                name: "INT",
                value: (self.intfilter & 0xffff) as u64,
                width: 16,
            },
            DebugRegister {
                name: "SAMPLE",
                value: (self.next_sample & 0xffff) as u64,
                width: 16,
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
    use crate::core::save_state::{Saveable, StateReader, StateWriter};

    /// Feed one CVSD bit the way the sound CPU does: latch the digit, pulse the
    /// clock high (active edge) then low (inactive edge).
    fn feed(dev: &mut Hc55516, bit: bool) {
        dev.digit_w(bit);
        dev.clock_w(true);
        dev.clock_w(false);
    }

    #[test]
    fn reset_precharges_syllabic_filter() {
        let mut dev = Hc55516::new();
        // Perturb, then reset.
        feed(&mut dev, true);
        dev.reset();
        assert_eq!(dev.sylfilter, 0x3f);
        assert_eq!(dev.intfilter, 0);
        assert!(dev.agc);
        assert!(dev.buffered_fzq);
        assert!(!dev.last_clock_state);
    }

    #[test]
    fn only_rising_edge_advances_shift_register() {
        let mut dev = Hc55516::new();
        dev.reset();
        dev.digit_w(true);

        dev.clock_w(true); // rising -> shift in a 1
        let after_rise = dev.shiftreg;
        dev.clock_w(false); // falling -> no shift
        assert_eq!(dev.shiftreg, after_rise, "falling edge must not shift");
        assert_eq!(after_rise & 1, 1, "rising edge shifts the data bit in");

        dev.clock_w(true); // rising again -> another shift
        assert_eq!(dev.shiftreg & 0b11, 0b11);
    }

    #[test]
    fn constant_streams_slew_in_opposite_directions_and_stay_bounded() {
        // A long run of 1s and a long run of 0s must drive the integrator to
        // opposite extremes, and the output must remain within i16 range.
        let mut hi = Hc55516::new();
        hi.reset();
        for _ in 0..200 {
            feed(&mut hi, true);
            assert!((i16::MIN as i32..=i16::MAX as i32).contains(&hi.next_sample));
        }

        let mut lo = Hc55516::new();
        lo.reset();
        for _ in 0..200 {
            feed(&mut lo, false);
            assert!((i16::MIN as i32..=i16::MAX as i32).contains(&lo.next_sample));
        }

        // The two constant streams slew to opposite extremes, both near
        // saturation. (Which stream goes positive is a CVSD polarity convention;
        // what matters is that they diverge and stay in range.)
        assert_ne!(
            hi.next_sample.signum(),
            lo.next_sample.signum(),
            "streams should slew to opposite signs: hi={}, lo={}",
            hi.next_sample,
            lo.next_sample
        );
        assert!(
            hi.next_sample.abs() > 16000 && lo.next_sample.abs() > 16000,
            "both streams should approach saturation: hi={}, lo={}",
            hi.next_sample,
            lo.next_sample
        );
    }

    #[test]
    fn silence_holds_a_stable_sample() {
        // With no clock activity the held sample does not change.
        let mut dev = Hc55516::new();
        dev.reset();
        feed(&mut dev, true);
        let s = dev.sample_i16();
        // No further clock edges -> sample unchanged.
        assert_eq!(dev.sample_i16(), s);
    }

    #[test]
    fn save_load_round_trip() {
        let mut dev = Hc55516::new();
        dev.reset();
        for i in 0..37 {
            feed(&mut dev, i % 3 == 0);
        }

        let mut w = StateWriter::new();
        dev.save_state(&mut w);
        let data = w.into_vec();

        let mut dev2 = Hc55516::new();
        let mut r = StateReader::new(&data);
        dev2.load_state(&mut r).unwrap();

        assert_eq!(dev2.shiftreg, dev.shiftreg);
        assert_eq!(dev2.sylfilter, dev.sylfilter);
        assert_eq!(dev2.intfilter, dev.intfilter);
        assert_eq!(dev2.next_sample, dev.next_sample);
        assert_eq!(dev2.sample_i16(), dev.sample_i16());
    }
}
