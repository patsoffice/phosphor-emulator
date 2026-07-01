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
//! graph, differing only in the secret matcher values — see [`SlapsticConfig`]
//! and [`SLAPSTIC_103`]). It is driven by [`Slapstic::test`], which the bus calls with the
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
// Per-chip configuration
// ---------------------------------------------------------------------------
//
// Every Atari System 1 slapstic watches the 0x80000-0x87FFF window and decodes
// 14 address lines; the chips differ only in the *secret matcher values* baked
// into the PAL. The raw `(mask, value)` chip constants are given in window
// word-offset terms; `test_in`/`test_any`/`test_bank` lift them onto the full
// byte address the bus presents, following MAME's `atari_slapstic_device`.
// [`SlapsticConfig`] captures a whole chip's matchers so [`Slapstic::test`] can
// drive the shared state machine against any of them.

/// Window base byte address and the masks that pin an access into it. These are
/// common to the whole 103-110 family (all share the 0x80000-byte ROM window),
/// so the chip-specific configs only vary the `(mask, value)` pairs below.
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

/// All the parameters that distinguish one slapstic chip from another: the
/// power-on bank, the re-arm matcher, and the direct/alt/bitwise sequence
/// matchers. The state machine in [`Slapstic::test`] is identical across chips
/// and reads every matcher from here, so adding a new chip (e.g. Road Runner's
/// 137412-108) is just another `const` of this type.
pub struct SlapsticConfig {
    /// Power-on / reset bank.
    bankstart: u8,
    /// Re-arm: an access to the window base offset resets any sequence to active.
    reset: Matcher,
    /// Direct bank-select matchers for bank values 0..3.
    bank: [Matcher; 4],
    // Alternate-banking sequence (active → valid → select → commit). `alt_start`
    // matches *anywhere* (test_any); the remaining steps are in-window.
    alt_start: Matcher,
    alt_valid: Matcher,
    alt_select: Matcher,
    alt_commit: Matcher,
    // Bitwise-banking sequence (active → load → set/clear bits → commit).
    bit_start: Matcher,
    bit_load: Matcher,
    bit3c0: Matcher,
    bit3s0: Matcher,
    bit3c1: Matcher,
    bit3s1: Matcher,
    bit_commit: Matcher,
}

/// Chip **137412-103** (Marble Madness).
pub const SLAPSTIC_103: SlapsticConfig = SlapsticConfig {
    bankstart: 3,
    reset: Matcher {
        mask: RANGE_MASK | INPUT_MASK,
        value: RANGE_VALUE,
    },
    bank: [
        test_bank(0x0040),
        test_bank(0x0050),
        test_bank(0x0060),
        test_bank(0x0070),
    ],
    alt_start: test_any(0x007F, 0x002D),
    alt_valid: test_in(0x3FFF, 0x3D14),
    alt_select: test_in(0x3FFC, 0x3D24),
    alt_commit: test_in(0x3FCF, 0x0040),
    bit_start: test_in(0x3FF0, 0x34C0),
    bit_load: test_in(0x3FCF, 0x0040),
    bit3c0: test_in(0x3FF3, 0x34C0),
    bit3s0: test_in(0x3FF3, 0x34C1),
    bit3c1: test_in(0x3FF3, 0x34C2),
    bit3s1: test_in(0x3FF3, 0x34C3),
    bit_commit: test_in(0x3FF8, 0x34D0),
};

/// Chip **137412-108** (Road Runner). Same state graph and window as the 103;
/// only the secret matcher values differ.
pub const SLAPSTIC_108: SlapsticConfig = SlapsticConfig {
    bankstart: 3,
    reset: Matcher {
        mask: RANGE_MASK | INPUT_MASK,
        value: RANGE_VALUE,
    },
    bank: [
        test_bank(0x0028),
        test_bank(0x002A),
        test_bank(0x002C),
        test_bank(0x002E),
    ],
    alt_start: test_any(0x007F, 0x001F),
    alt_valid: test_in(0x3FFF, 0x3772),
    alt_select: test_in(0x3FFC, 0x3764),
    alt_commit: test_in(0x3FF9, 0x0028),
    bit_start: test_in(0x3FF0, 0x0060),
    bit_load: test_in(0x3FF9, 0x0028),
    bit3c0: test_in(0x3FF3, 0x0060),
    bit3s0: test_in(0x3FF3, 0x0061),
    bit3c1: test_in(0x3FF3, 0x0062),
    bit3s1: test_in(0x3FF3, 0x0063),
    bit_commit: test_in(0x3FF8, 0x0070),
};

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

/// Atari Slapstic address-sequence bank selector, parameterized by chip.
pub struct Slapstic {
    /// The chip's matcher constants; the state machine reads every pattern here.
    config: &'static SlapsticConfig,
    state: State,
    current_bank: u8,
    /// Bank assembled by an in-progress alt/bitwise sequence, committed at the end.
    loaded_bank: u8,
}

impl Slapstic {
    /// Create a Slapstic for the given chip config in its power-on state
    /// (idle, on the config's start bank).
    pub fn new(config: &'static SlapsticConfig) -> Self {
        Self {
            config,
            state: State::Idle,
            current_bank: config.bankstart,
            loaded_bank: config.bankstart,
        }
    }

    /// Create a Slapstic for a chip by its `137412-NNN` number (e.g. `103` for
    /// Marble Madness, `108` for Road Runner). Panics on an unsupported chip.
    pub fn for_chip(chip: u16) -> Self {
        let config = match chip {
            103 => &SLAPSTIC_103,
            108 => &SLAPSTIC_108,
            _ => panic!("unsupported slapstic chip 137412-{chip}"),
        };
        Self::new(config)
    }

    /// Reset to power-on: idle, back on the start bank.
    pub fn reset(&mut self) {
        self.state = State::Idle;
        self.current_bank = self.config.bankstart;
        self.loaded_bank = self.config.bankstart;
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
        let cfg = self.config;
        match self.state {
            // Idle until the window base re-arms the chip.
            State::Idle => {
                if cfg.reset.matches(addr) {
                    self.state = State::Active;
                }
            }
            // Direct switch, or the first step of an alt/bitwise sequence.
            State::Active => {
                if let Some(bank) = cfg.bank.iter().position(|m| m.matches(addr)) {
                    self.current_bank = bank as u8;
                    self.state = State::Idle;
                } else if cfg.alt_start.matches(addr) {
                    self.state = State::AltValid;
                } else if cfg.bit_start.matches(addr) {
                    self.state = State::BitLoad;
                }
            }
            // Alt sequence: reset re-arms, the matching step advances, anything
            // else breaks back to active.
            State::AltValid => {
                self.state = if cfg.reset.matches(addr) {
                    State::Active
                } else if cfg.alt_valid.matches(addr) {
                    State::AltSelect
                } else {
                    State::Active
                };
            }
            State::AltSelect => {
                if cfg.reset.matches(addr) {
                    self.state = State::Active;
                } else if cfg.alt_select.matches(addr) {
                    // The bank rides in address bits 1-2 (data-shift + altshift 0).
                    self.loaded_bank = ((addr >> 1) & 3) as u8;
                    self.state = State::AltCommit;
                } else {
                    self.state = State::Active;
                }
            }
            // Commit is patient: only a reset or the commit access act on it.
            State::AltCommit => {
                if cfg.reset.matches(addr) {
                    self.state = State::Active;
                } else if cfg.alt_commit.matches(addr) {
                    self.current_bank = self.loaded_bank;
                    self.state = State::Idle;
                }
            }
            // Bitwise sequence: load the current bank, then set/clear one bit at a
            // time, alternating phase, until the commit access.
            State::BitLoad => {
                if cfg.reset.matches(addr) {
                    self.state = State::Active;
                } else if cfg.bit_load.matches(addr) {
                    self.loaded_bank = self.current_bank;
                    self.state = State::BitSetOdd;
                }
            }
            State::BitSetOdd | State::BitSetEven => {
                let odd = self.state == State::BitSetOdd;
                // The odd and even phases swap which access clears vs. sets each bit.
                let (clear0, set0, clear1, set1) = if odd {
                    (cfg.bit3c0, cfg.bit3s0, cfg.bit3c1, cfg.bit3s1)
                } else {
                    (cfg.bit3s1, cfg.bit3c1, cfg.bit3s0, cfg.bit3c0)
                };
                let next_phase = if odd {
                    State::BitSetEven
                } else {
                    State::BitSetOdd
                };
                if cfg.reset.matches(addr) {
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
                } else if cfg.bit_commit.matches(addr) {
                    self.current_bank = self.loaded_bank;
                    self.state = State::Idle;
                }
            }
        }
    }
}

impl Default for Slapstic {
    /// Defaults to chip 137412-103 (Marble Madness).
    fn default() -> Self {
        Self::new(&SLAPSTIC_103)
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
    fn chip_108_direct_and_alt_banking() {
        // Road Runner's 108 shares the 103 state graph with different matchers.
        // Direct bank-select byte addresses: 0x80000 | (bankval << 1) for
        // bankval 0x28/0x2A/0x2C/0x2E.
        const DIRECT_108: [u32; 4] = [0x0008_0050, 0x0008_0054, 0x0008_0058, 0x0008_005C];
        for (i, &sel) in DIRECT_108.iter().enumerate() {
            let mut sl = Slapstic::for_chip(108);
            assert_eq!(sl.current_bank(), 3, "powers on to bank 3");
            assert_eq!(run(&mut sl, &[ARM, sel]), i as u8, "108 direct bank {i}");
        }

        // Alternate sequence: start (test_any 0x1F) → valid (0x3772) → select
        // (0x3764, bank in bits 1-2) → commit (0x28). ALT3 carries the bank as
        // 0x86EC8 + 2*bank.
        for bank in 0u32..4 {
            let mut sl = Slapstic::for_chip(108);
            let final_bank = run(
                &mut sl,
                &[
                    ARM,
                    0x0008_003E,            // alt-start (0x1F) in-window
                    0x0008_6EE4,            // alt-valid (0x3772)
                    0x0008_6EC8 + 2 * bank, // alt-select (0x3764 | bank)
                    0x0008_0050,            // alt-commit (0x28)
                ],
            );
            assert_eq!(final_bank, bank as u8, "108 alt bank {bank}");
        }
    }

    #[test]
    fn powers_on_to_start_bank() {
        let sl = Slapstic::for_chip(103);
        assert_eq!(sl.current_bank(), 3);
    }

    #[test]
    fn arming_requires_the_base_offset() {
        let mut sl = Slapstic::for_chip(103);
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
            let mut sl = Slapstic::for_chip(103);
            assert_eq!(
                run(&mut sl, &[ARM, sel]),
                i as u8,
                "bank {i} via {sel:#08X}"
            );
        }
    }

    #[test]
    fn direct_switch_returns_to_idle() {
        let mut sl = Slapstic::for_chip(103);
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
            let mut sl = Slapstic::for_chip(103);
            let final_bank = run(
                &mut sl,
                &[
                    ARM,
                    ALT_START_IN,
                    ALT_VALID_AT,
                    alt_select_at(bank),
                    ALT_COMMIT_AT,
                ],
            );
            assert_eq!(final_bank, bank as u8, "alt bank {bank}");
        }
    }

    #[test]
    fn alt_start_fires_outside_the_window() {
        // The real chip only sees address lines: an ALT-start pattern in RAM
        // (here a stack-like 0x40005A) must arm the sequence just the same.
        let mut sl = Slapstic::for_chip(103);
        let final_bank = run(
            &mut sl,
            &[
                ARM,
                0x0040_005A,
                ALT_VALID_AT,
                alt_select_at(2),
                ALT_COMMIT_AT,
            ],
        );
        assert_eq!(final_bank, 2, "off-window alt start must still bank-switch");
    }

    #[test]
    fn alt_sequence_break_aborts_without_changing_bank() {
        let mut sl = Slapstic::for_chip(103);
        // Break the sequence at the select step (before a bank is loaded); the
        // bank must stay at the power-on value.
        run(&mut sl, &[ARM, ALT_START_IN, ALT_VALID_AT, 0x0008_1234]);
        assert_eq!(sl.current_bank(), 3, "broken sequence left the bank alone");
    }

    #[test]
    fn bitwise_sequence_sets_bank_bits() {
        // arm, bit-start, bit-load, then two phases of the 0x34C0 access, commit.
        let mut sl = Slapstic::for_chip(103);
        let final_bank = run(
            &mut sl,
            &[
                ARM,
                0x0008_6980,
                0x0008_0080,
                0x0008_6980,
                0x0008_6980,
                0x0008_69A0,
            ],
        );
        // odd clear0 = 0x34C0 clears bit0 (3→2); on the even phase the same
        // access maps to set1 (|=2), so the result stays 2 — documenting the
        // phase swap rather than a hand-guessed value.
        assert_eq!(final_bank, 2);
    }

    #[test]
    fn bitwise_commit_after_one_bit() {
        // Arm, bit-start, load, set bit0 on the odd phase (0x34C1), then commit.
        let mut sl = Slapstic::for_chip(103);
        let final_bank = run(
            &mut sl,
            &[ARM, 0x0008_6980, 0x0008_0080, 0x0008_6982, 0x0008_69A0],
        );
        assert_eq!(final_bank, 3);
    }

    #[test]
    fn reset_returns_to_start_bank() {
        let mut sl = Slapstic::for_chip(103);
        run(&mut sl, &[ARM, DIRECT[1]]);
        assert_eq!(sl.current_bank(), 1);
        sl.reset();
        assert_eq!(sl.current_bank(), 3);
        assert_eq!(sl.state, State::Idle);
    }

    #[test]
    fn save_load_round_trips_state() {
        let mut sl = Slapstic::for_chip(103);
        // Drive partway into an alt sequence so state + loaded_bank are non-trivial.
        run(
            &mut sl,
            &[ARM, ALT_START_IN, ALT_VALID_AT, alt_select_at(2)],
        );
        assert_eq!(sl.state, State::AltCommit);
        assert_eq!(sl.loaded_bank, 2);

        let mut w = StateWriter::new();
        sl.save_state(&mut w);
        let bytes = w.into_vec();

        let mut sl2 = Slapstic::for_chip(103);
        let mut r = StateReader::new(&bytes);
        sl2.load_state(&mut r).unwrap();
        assert_eq!(sl2.state, State::AltCommit);
        assert_eq!(sl2.loaded_bank, 2);
        // Completing the sequence on the restored chip commits bank 2.
        sl2.test(ALT_COMMIT_AT);
        assert_eq!(sl2.current_bank(), 2);
    }
}
