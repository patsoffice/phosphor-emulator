//! Constants for `machines/src/llander_sound.rs`, solved from `llander-audio.toml`
//! by `netlist derive` from `llander-audio.derive.toml` beside it.
//!
//! **Generated. Do not edit.** Change the transcription or the spec and run
//! `netlist derive` on the spec again. A test in `tools/netlist` fails while
//! this file differs from what that writes.
//!
//! Each value comes from the whole passive network solved at once, not from a
//! judgment about which capacitor is a short or an open. The solver treats
//! every pin of a part that is not an R, C or L as open; `netlist solve` with
//! the same group and drives lists them.

/// The time constant of the mode that stores 100.0 % of its energy in
/// `C15`: the whole board, `AUD0` held at 0 V, `AUD1` held at 0 V, `AUD2` held
/// at 0 V, `AUD3` held at 0 V, `R7b summing node` held at 5 V, `noise out` held
/// at 3.8 V.
pub(super) const THROTTLE_0_C15_TAU: f64 = 4.82070e-2; // 48.207 ms

/// How far `common node` moves at DC per volt of `noise out`: the whole board,
/// `AUD0` held at 0 V, `AUD1` held at 0 V, `AUD2` held at 0 V, `AUD3` held at 0
/// V, `R7b summing node` held at 5 V, `noise out` held at 3.8 V.
pub(super) const THROTTLE_0_GAIN: f64 = 0.00000;

/// The time constant of the mode that stores 98.9 % of its energy in
/// `C15`: the whole board, `AUD0` held at 5 V, `AUD1` held at 0 V, `AUD2` held
/// at 0 V, `AUD3` held at 0 V, `R7b summing node` held at 5 V, `noise out` held
/// at 3.8 V.
pub(super) const THROTTLE_1_C15_TAU: f64 = 1.14774e-2; // 11.477 ms

/// How far `common node` moves at DC per volt of `noise out`: the whole board,
/// `AUD0` held at 5 V, `AUD1` held at 0 V, `AUD2` held at 0 V, `AUD3` held at 0
/// V, `R7b summing node` held at 5 V, `noise out` held at 3.8 V.
pub(super) const THROTTLE_1_GAIN: f64 = 0.76169;

/// The time constant of the mode that stores 100.0 % of its energy in
/// `C15`: the whole board, `AUD0` held at 0 V, `AUD1` held at 5 V, `AUD2` held
/// at 0 V, `AUD3` held at 0 V, `R7b summing node` held at 5 V, `noise out` held
/// at 3.8 V.
pub(super) const THROTTLE_2_C15_TAU: f64 = 7.06599e-3; // 7.066 ms

/// How far `common node` moves at DC per volt of `noise out`: the whole board,
/// `AUD0` held at 0 V, `AUD1` held at 5 V, `AUD2` held at 0 V, `AUD3` held at 0
/// V, `R7b summing node` held at 5 V, `noise out` held at 3.8 V.
pub(super) const THROTTLE_2_GAIN: f64 = 0.85340;

/// The time constant of the mode that stores 100.0 % of its energy in
/// `C15`: the whole board, `AUD0` held at 5 V, `AUD1` held at 5 V, `AUD2` held
/// at 0 V, `AUD3` held at 0 V, `R7b summing node` held at 5 V, `noise out` held
/// at 3.8 V.
pub(super) const THROTTLE_3_C15_TAU: f64 = 4.81167e-3; // 4.812 ms

/// How far `common node` moves at DC per volt of `noise out`: the whole board,
/// `AUD0` held at 5 V, `AUD1` held at 5 V, `AUD2` held at 0 V, `AUD3` held at 0
/// V, `R7b summing node` held at 5 V, `noise out` held at 3.8 V.
pub(super) const THROTTLE_3_GAIN: f64 = 0.90018;

/// The time constant of the mode that stores 100.0 % of its energy in
/// `C15`: the whole board, `AUD0` held at 0 V, `AUD1` held at 0 V, `AUD2` held
/// at 5 V, `AUD3` held at 0 V, `R7b summing node` held at 5 V, `noise out` held
/// at 3.8 V.
pub(super) const THROTTLE_4_C15_TAU: f64 = 3.67656e-3; // 3.677 ms

/// How far `common node` moves at DC per volt of `noise out`: the whole board,
/// `AUD0` held at 0 V, `AUD1` held at 0 V, `AUD2` held at 5 V, `AUD3` held at 0
/// V, `R7b summing node` held at 5 V, `noise out` held at 3.8 V.
pub(super) const THROTTLE_4_GAIN: f64 = 0.92373;

/// The time constant of the mode that stores 100.0 % of its energy in
/// `C15`: the whole board, `AUD0` held at 5 V, `AUD1` held at 0 V, `AUD2` held
/// at 5 V, `AUD3` held at 0 V, `R7b summing node` held at 5 V, `noise out` held
/// at 3.8 V.
pub(super) const THROTTLE_5_C15_TAU: f64 = 2.95594e-3; // 2.956 ms

/// How far `common node` moves at DC per volt of `noise out`: the whole board,
/// `AUD0` held at 5 V, `AUD1` held at 0 V, `AUD2` held at 5 V, `AUD3` held at 0
/// V, `R7b summing node` held at 5 V, `noise out` held at 3.8 V.
pub(super) const THROTTLE_5_GAIN: f64 = 0.93868;

/// The time constant of the mode that stores 100.0 % of its energy in
/// `C15`: the whole board, `AUD0` held at 0 V, `AUD1` held at 5 V, `AUD2` held
/// at 5 V, `AUD3` held at 0 V, `R7b summing node` held at 5 V, `noise out` held
/// at 3.8 V.
pub(super) const THROTTLE_6_C15_TAU: f64 = 2.54610e-3; // 2.546 ms

/// How far `common node` moves at DC per volt of `noise out`: the whole board,
/// `AUD0` held at 0 V, `AUD1` held at 5 V, `AUD2` held at 5 V, `AUD3` held at 0
/// V, `R7b summing node` held at 5 V, `noise out` held at 3.8 V.
pub(super) const THROTTLE_6_GAIN: f64 = 0.94718;

/// The time constant of the mode that stores 100.0 % of its energy in
/// `C15`: the whole board, `AUD0` held at 5 V, `AUD1` held at 5 V, `AUD2` held
/// at 5 V, `AUD3` held at 0 V, `R7b summing node` held at 5 V, `noise out` held
/// at 3.8 V.
pub(super) const THROTTLE_7_C15_TAU: f64 = 2.17834e-3; // 2.178 ms

/// How far `common node` moves at DC per volt of `noise out`: the whole board,
/// `AUD0` held at 5 V, `AUD1` held at 5 V, `AUD2` held at 5 V, `AUD3` held at 0
/// V, `R7b summing node` held at 5 V, `noise out` held at 3.8 V.
pub(super) const THROTTLE_7_GAIN: f64 = 0.95481;
