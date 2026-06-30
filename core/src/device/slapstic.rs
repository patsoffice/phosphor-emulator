//! Atari Slapstic — bank-switching copy-protection PAL.
//!
//! The Slapstic sits between the CPU and a small ROM window and selects one of
//! four 8 KB banks. The bank is not chosen by a plain register write — instead
//! the chip watches the *sequence of addresses* the CPU touches inside its
//! window and only commits a bank change when it recognizes one of three secret
//! access patterns (a direct switch, an "alternate" sequence, or a bit-at-a-time
//! sequence). A stray access that breaks the pattern silently aborts it, which
//! is what frustrates ROM-dumping bootleggers.
//!
//! This models the address-sequence state machine for the **137412-103** chip
//! used by Marble Madness (the rest of the 103-110 family share this state
//! graph). It is driven by [`Slapstic::test`], which the bus calls with the
//! **full byte address** of every access the CPU drives onto the bus — data
//! reads/writes *and* instruction prefetches, anywhere in the address map, not
//! just the window — because the chip only decodes address lines and some
//! sequence steps (`test_any`) match regardless of the window. The prefetch
//! coverage matters: the game arms the alternate-banking sequence by
//! *prefetching* an instruction placed at a magic address, so the CPU must
//! present prefetch fetches here in hardware order. After feeding an access,
//! read [`Slapstic::current_bank`] for the bank the window presents. The logic
//! mirrors `atari_slapstic_device` for chip 103.
//!
//! Reference: <http://www.aarongiles.com/slapstic.html> and MAME's
//! `src/mame/atari/slapstic.cpp`.

use crate::core::save_state::{SaveError, Saveable, StateReader, StateWriter};

/// A resolved address matcher: true when `addr & mask == value`. Unlike the raw
/// chip constants this works on the **full bus address**, so the secret
/// patterns are recognized wherever they appear — including outside the
/// slapstic's own window, exactly as the real PAL (which only sees address
/// lines) does.
#[derive(Clone, Copy)]
struct Matcher {
    mask: u32,
    value: u32,
}

impl Matcher {
    #[inline]
    fn matches(&self, addr: u32) -> bool {
        addr & self.mask == self.value
    }
}

// ---------------------------------------------------------------------------
// Chip 137412-103 parameters (Marble Madness)
// ---------------------------------------------------------------------------
//
// The chip watches the 0x80000-0x87FFF window and decodes 14 address lines.
// The raw `(mask, value)` chip constants are given in window word-offset terms;
// `test_in`/`test_any`/`test_bank` lift them onto the full byte address the bus
// presents, following MAME's `atari_slapstic_device::checker`.

/// Power-on / reset bank.
const BANKSTART: u8 = 3;

/// Window base byte address and the masks that pin an access into it.
const RANGE_VALUE: u32 = 0x0008_0000;
/// `!((end - start) | mirror)` for a 0x8000-byte, un-mirrored window.
const RANGE_MASK: u32 = !0x7FFF;
/// 14 decoded address lines (A1-A14), shifted up by the 16-bit data shift.
const INPUT_MASK: u32 = 0x3FFF << 1;

/// In-range matcher: the access must hit the window *and* match `(mask, value)`.
const fn test_in(mask: u16, value: u16) -> Matcher {
    Matcher {
        mask: RANGE_MASK | ((mask as u32) << 1),
        value: RANGE_VALUE | ((value as u32) << 1),
    }
}

/// Anywhere matcher: matches the pattern regardless of the window (the chip
/// only sees address lines, so these fire on any access — e.g. RAM/stack).
const fn test_any(mask: u16, value: u16) -> Matcher {
    Matcher {
        mask: (mask as u32) << 1,
        value: (value as u32) << 1,
    }
}

/// Direct bank-select matcher for bank-select value `b`.
const fn test_bank(b: u16) -> Matcher {
    Matcher {
        mask: RANGE_MASK | INPUT_MASK,
        value: RANGE_VALUE | ((b as u32) << 1),
    }
}

/// Re-arm: an access to the window base offset resets any sequence to active.
const RESET: Matcher = Matcher {
    mask: RANGE_MASK | INPUT_MASK,
    value: RANGE_VALUE,
};

/// Direct bank-select values 0..3.
const BANK: [Matcher; 4] = [
    test_bank(0x0040),
    test_bank(0x0050),
    test_bank(0x0060),
    test_bank(0x0070),
];

// Alternate-banking sequence (active → valid → select → commit). ALT_START
// matches *anywhere* (test_any); the remaining steps are in-window.
const ALT_START: Matcher = test_any(0x007F, 0x002D);
const ALT_VALID: Matcher = test_in(0x3FFF, 0x3D14);
const ALT_SELECT: Matcher = test_in(0x3FFC, 0x3D24);
const ALT_COMMIT: Matcher = test_in(0x3FCF, 0x0040);

// Bitwise-banking sequence (active → load → set/clear bits → commit).
const BIT_START: Matcher = test_in(0x3FF0, 0x34C0);
const BIT_LOAD: Matcher = test_in(0x3FCF, 0x0040);
const BIT3C0: Matcher = test_in(0x3FF3, 0x34C0);
const BIT3S0: Matcher = test_in(0x3FF3, 0x34C1);
const BIT3C1: Matcher = test_in(0x3FF3, 0x34C2);
const BIT3S1: Matcher = test_in(0x3FF3, 0x34C3);
const BIT_COMMIT: Matcher = test_in(0x3FF8, 0x34D0);

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    /// Waiting for the window's base offset (0) to arm the chip.
    Idle,
    /// Armed: accepts a direct switch, or the start of an alt/bitwise sequence.
    Active,
    AltValid,
    AltSelect,
    AltCommit,
    BitLoad,
    /// Bitwise sequences alternate between two phases as each bit is set/cleared.
    BitSetOdd,
    BitSetEven,
}

impl State {
    fn to_u8(self) -> u8 {
        match self {
            State::Idle => 0,
            State::Active => 1,
            State::AltValid => 2,
            State::AltSelect => 3,
            State::AltCommit => 4,
            State::BitLoad => 5,
            State::BitSetOdd => 6,
            State::BitSetEven => 7,
        }
    }
    fn from_u8(v: u8) -> Self {
        match v {
            1 => State::Active,
            2 => State::AltValid,
            3 => State::AltSelect,
            4 => State::AltCommit,
            5 => State::BitLoad,
            6 => State::BitSetOdd,
            7 => State::BitSetEven,
            _ => State::Idle,
        }
    }
}

/// Atari Slapstic 137412-103 address-sequence bank selector.
pub struct Slapstic {
    state: State,
    current_bank: u8,
    /// Bank assembled by an in-progress alt/bitwise sequence, committed at the end.
    loaded_bank: u8,
}

impl Slapstic {
    /// Create a chip-103 Slapstic in its power-on state (idle, bank 3).
    pub fn new() -> Self {
        Self {
            state: State::Idle,
            current_bank: BANKSTART,
            loaded_bank: BANKSTART,
        }
    }

    /// Reset to power-on: idle, back on the start bank.
    pub fn reset(&mut self) {
        self.state = State::Idle;
        self.current_bank = BANKSTART;
        self.loaded_bank = BANKSTART;
    }

    /// The bank the window currently presents (0-3).
    pub fn current_bank(&self) -> u8 {
        self.current_bank
    }

    /// Feed one bus access (the full byte address) to the state machine. The
    /// real chip only sees address lines, so this must be called for every
    /// access the CPU drives — data reads/writes *and* instruction prefetches —
    /// anywhere, not just inside the window, because the secret sequences are
    /// armed by `test_any` patterns that can land in RAM/stack or in prefetched
    /// code. Read [`current_bank`] afterwards for the bank the window presents.
    ///
    /// Mirrors `atari_slapstic_device::*::test()` for chip 103.
    pub fn test(&mut self, addr: u32) {
        match self.state {
            // Idle until the window base re-arms the chip.
            State::Idle => {
                if RESET.matches(addr) {
                    self.state = State::Active;
                }
            }
            // Direct switch, or the first step of an alt/bitwise sequence.
            State::Active => {
                if let Some(bank) = BANK.iter().position(|m| m.matches(addr)) {
                    self.current_bank = bank as u8;
                    self.state = State::Idle;
                } else if ALT_START.matches(addr) {
                    self.state = State::AltValid;
                } else if BIT_START.matches(addr) {
                    self.state = State::BitLoad;
                }
            }
            // Alt sequence: reset re-arms, the matching step advances, anything
            // else breaks back to active.
            State::AltValid => {
                self.state = if RESET.matches(addr) {
                    State::Active
                } else if ALT_VALID.matches(addr) {
                    State::AltSelect
                } else {
                    State::Active
                };
            }
            State::AltSelect => {
                if RESET.matches(addr) {
                    self.state = State::Active;
                } else if ALT_SELECT.matches(addr) {
                    // The bank rides in address bits 1-2 (data-shift + altshift 0).
                    self.loaded_bank = ((addr >> 1) & 3) as u8;
                    self.state = State::AltCommit;
                } else {
                    self.state = State::Active;
                }
            }
            // Commit is patient: only a reset or the commit access act on it.
            State::AltCommit => {
                if RESET.matches(addr) {
                    self.state = State::Active;
                } else if ALT_COMMIT.matches(addr) {
                    self.current_bank = self.loaded_bank;
                    self.state = State::Idle;
                }
            }
            // Bitwise sequence: load the current bank, then set/clear one bit at a
            // time, alternating phase, until the commit access.
            State::BitLoad => {
                if RESET.matches(addr) {
                    self.state = State::Active;
                } else if BIT_LOAD.matches(addr) {
                    self.loaded_bank = self.current_bank;
                    self.state = State::BitSetOdd;
                }
            }
            State::BitSetOdd | State::BitSetEven => {
                let odd = self.state == State::BitSetOdd;
                // The odd and even phases swap which access clears vs. sets each bit.
                let (clear0, set0, clear1, set1) = if odd {
                    (BIT3C0, BIT3S0, BIT3C1, BIT3S1)
                } else {
                    (BIT3S1, BIT3C1, BIT3S0, BIT3C0)
                };
                let next_phase = if odd {
                    State::BitSetEven
                } else {
                    State::BitSetOdd
                };
                if RESET.matches(addr) {
                    self.state = State::Active;
                } else if clear0.matches(addr) {
                    self.loaded_bank &= !1;
                    self.state = next_phase;
                } else if set0.matches(addr) {
                    self.loaded_bank |= 1;
                    self.state = next_phase;
                } else if clear1.matches(addr) {
                    self.loaded_bank &= !2;
                    self.state = next_phase;
                } else if set1.matches(addr) {
                    self.loaded_bank |= 2;
                    self.state = next_phase;
                } else if BIT_COMMIT.matches(addr) {
                    self.current_bank = self.loaded_bank;
                    self.state = State::Idle;
                }
            }
        }
    }
}

impl Default for Slapstic {
    fn default() -> Self {
        Self::new()
    }
}

impl Saveable for Slapstic {
    fn save_state(&self, w: &mut StateWriter) {
        w.write_u8(self.state.to_u8());
        w.write_u8(self.current_bank);
        w.write_u8(self.loaded_bank);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.state = State::from_u8(r.read_u8()?);
        self.current_bank = r.read_u8()? & 3;
        self.loaded_bank = r.read_u8()? & 3;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Canonical byte addresses for each sequence step (window base 0x80000).
    const ARM: u32 = 0x0008_0000; // window base, the re-arm / reset access
    /// Direct bank-select byte addresses (bank values 0x40/0x50/0x60/0x70).
    const DIRECT: [u32; 4] = [0x0008_0080, 0x0008_00A0, 0x0008_00C0, 0x0008_00E0];
    const ALT_START_IN: u32 = 0x0008_005A; // matches test_any(0x2d) inside the window
    const ALT_VALID_AT: u32 = 0x0008_7A28; // test_in(0x3d14)
    const ALT_COMMIT_AT: u32 = 0x0008_0080; // test_in(0x40)
    /// ALT3 carries the bank in address bits 1-2: 0x87A48 + 2*bank.
    const fn alt_select_at(bank: u32) -> u32 {
        0x0008_7A48 + 2 * bank
    }

    /// Run a sequence of byte addresses, returning the final bank.
    fn run(sl: &mut Slapstic, addrs: &[u32]) -> u8 {
        for &a in addrs {
            sl.test(a);
        }
        sl.current_bank()
    }

    #[test]
    fn powers_on_to_start_bank() {
        let sl = Slapstic::new();
        assert_eq!(sl.current_bank(), 3);
    }

    #[test]
    fn arming_requires_the_base_offset() {
        let mut sl = Slapstic::new();
        // Touching a bank-select address while idle does nothing.
        sl.test(DIRECT[0]);
        assert_eq!(sl.current_bank(), 3);
        // Arm with the base address, then the direct select takes effect.
        sl.test(ARM);
        sl.test(DIRECT[0]);
        assert_eq!(sl.current_bank(), 0);
    }

    #[test]
    fn direct_switch_selects_each_bank() {
        for (i, &sel) in DIRECT.iter().enumerate() {
            let mut sl = Slapstic::new();
            assert_eq!(run(&mut sl, &[ARM, sel]), i as u8, "bank {i} via {sel:#08X}");
        }
    }

    #[test]
    fn direct_switch_returns_to_idle() {
        let mut sl = Slapstic::new();
        run(&mut sl, &[ARM, DIRECT[2]]);
        assert_eq!(sl.current_bank(), 2);
        // After a switch the chip is idle again: a bare bank access is ignored
        // until re-armed.
        sl.test(DIRECT[0]);
        assert_eq!(sl.current_bank(), 2);
        run(&mut sl, &[ARM, DIRECT[0]]);
        assert_eq!(sl.current_bank(), 0);
    }

    #[test]
    fn alt_sequence_selects_the_encoded_bank() {
        for bank in 0u32..4 {
            let mut sl = Slapstic::new();
            let final_bank = run(
                &mut sl,
                &[ARM, ALT_START_IN, ALT_VALID_AT, alt_select_at(bank), ALT_COMMIT_AT],
            );
            assert_eq!(final_bank, bank as u8, "alt bank {bank}");
        }
    }

    #[test]
    fn alt_start_fires_outside_the_window() {
        // The real chip only sees address lines: an ALT-start pattern in RAM
        // (here a stack-like 0x40005A) must arm the sequence just the same.
        let mut sl = Slapstic::new();
        let final_bank = run(
            &mut sl,
            &[ARM, 0x0040_005A, ALT_VALID_AT, alt_select_at(2), ALT_COMMIT_AT],
        );
        assert_eq!(final_bank, 2, "off-window alt start must still bank-switch");
    }

    #[test]
    fn alt_sequence_break_aborts_without_changing_bank() {
        let mut sl = Slapstic::new();
        // Break the sequence at the select step (before a bank is loaded); the
        // bank must stay at the power-on value.
        run(&mut sl, &[ARM, ALT_START_IN, ALT_VALID_AT, 0x0008_1234]);
        assert_eq!(sl.current_bank(), 3, "broken sequence left the bank alone");
    }

    #[test]
    fn bitwise_sequence_sets_bank_bits() {
        // arm, bit-start, bit-load, then two phases of the 0x34C0 access, commit.
        let mut sl = Slapstic::new();
        let final_bank = run(
            &mut sl,
            &[ARM, 0x0008_6980, 0x0008_0080, 0x0008_6980, 0x0008_6980, 0x0008_69A0],
        );
        // odd clear0 = 0x34C0 clears bit0 (3→2); on the even phase the same
        // access maps to set1 (|=2), so the result stays 2 — documenting the
        // phase swap rather than a hand-guessed value.
        assert_eq!(final_bank, 2);
    }

    #[test]
    fn bitwise_commit_after_one_bit() {
        // Arm, bit-start, load, set bit0 on the odd phase (0x34C1), then commit.
        let mut sl = Slapstic::new();
        let final_bank = run(
            &mut sl,
            &[ARM, 0x0008_6980, 0x0008_0080, 0x0008_6982, 0x0008_69A0],
        );
        assert_eq!(final_bank, 3);
    }

    #[test]
    fn reset_returns_to_start_bank() {
        let mut sl = Slapstic::new();
        run(&mut sl, &[ARM, DIRECT[1]]);
        assert_eq!(sl.current_bank(), 1);
        sl.reset();
        assert_eq!(sl.current_bank(), 3);
        assert_eq!(sl.state, State::Idle);
    }

    #[test]
    fn save_load_round_trips_state() {
        let mut sl = Slapstic::new();
        // Drive partway into an alt sequence so state + loaded_bank are non-trivial.
        run(&mut sl, &[ARM, ALT_START_IN, ALT_VALID_AT, alt_select_at(2)]);
        assert_eq!(sl.state, State::AltCommit);
        assert_eq!(sl.loaded_bank, 2);

        let mut w = StateWriter::new();
        sl.save_state(&mut w);
        let bytes = w.into_vec();

        let mut sl2 = Slapstic::new();
        let mut r = StateReader::new(&bytes);
        sl2.load_state(&mut r).unwrap();
        assert_eq!(sl2.state, State::AltCommit);
        assert_eq!(sl2.loaded_bank, 2);
        // Completing the sequence on the restored chip commits bank 2.
        sl2.test(ALT_COMMIT_AT);
        assert_eq!(sl2.current_bank(), 2);
    }
}
