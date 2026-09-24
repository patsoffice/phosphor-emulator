//! Constants for `machines/src/zaxxon_sound.rs`, solved from `zaxxon-sound.toml`
//! by `netlist derive` from `zaxxon-sound.derive.toml` beside it.
//!
//! **Generated. Do not edit.** Change the transcription or the spec and run
//! `netlist derive` on the spec again. A test in `tools/netlist` fails while
//! this file differs from what that writes.
//!
//! Each value comes from the whole passive network solved at once, not from a
//! judgment about which capacitor is a short or an open. The solver treats
//! every pin of a part that is not an R, C or L as open; `netlist solve` with
//! the same group and drives lists them.

/// The time constant of the mode that stores 97.7 % of its energy in
/// `C88`: group `shot`, `Qbar` held at 5 V.
pub(super) const SHOT_C88_TAU: f64 = 2.57412e-2; // 25.741 ms

/// The time constant of the mode that stores 97.7 % of its energy in
/// `C89`: group `shot`, `Qbar` held at 5 V.
pub(super) const SHOT_C89_TAU: f64 = 7.76726e-1; // 776.726 ms

/// How far `node X` moves per volt of `node Y`, in the mode that stores
/// 97.7 % of its energy in `C89`: group `shot`, `Qbar` held at 5 V.
pub(super) const SHOT_C89_X_PER_Y: f64 = 0.57908;
