//! Constants for `machines/src/asteroids_sound.rs`, solved from `asteroids-audio.toml`
//! by `netlist derive` from `asteroids-audio.derive.toml` beside it.
//!
//! **Generated. Do not edit.** Change the transcription or the spec and run
//! `netlist derive` on the spec again. A test in `tools/netlist` fails while
//! this file differs from what that writes.
//!
//! Each value comes from the whole passive network solved at once, not from a
//! judgment about which capacitor is a short or an open. The solver treats
//! every pin of a part that is not an R, C or L as open; `netlist solve` with
//! the same group and drives lists them.
//!
//! A scenario whose constants say "op-amps ideal" is the exception: it
//! solves its op-amps as ideal, so a gain through a stage with feedback is the
//! feedback network's. Such a scenario gives gains only, never modes.

/// How far `AUDIO1` moves at DC per volt of `THUMPSND`: the whole board,
/// op-amps ideal, `EXPLOSND` held at 5 V, `LIFESND` held at 5 V, `SAUCRFIRESND`
/// held at 5 V, `SAUCRSND` held at 5 V, `SHPFIRESND` held at 5 V, `SHPSND` held
/// at 5 V, `THUMPSND` held at 5 V.
pub(super) const MIX_THUMP: f64 = -0.02128;

/// How far `AUDIO1` moves at DC per volt of `SAUCRSND`: the whole board,
/// op-amps ideal, `EXPLOSND` held at 5 V, `LIFESND` held at 5 V, `SAUCRFIRESND`
/// held at 5 V, `SAUCRSND` held at 5 V, `SHPFIRESND` held at 5 V, `SHPSND` held
/// at 5 V, `THUMPSND` held at 5 V.
pub(super) const MIX_SAUCER: f64 = -0.02564;

/// How far `AUDIO1` moves at DC per volt of `LIFESND`: the whole board, op-amps
/// ideal, `EXPLOSND` held at 5 V, `LIFESND` held at 5 V, `SAUCRFIRESND` held at
/// 5 V, `SAUCRSND` held at 5 V, `SHPFIRESND` held at 5 V, `SHPSND` held at 5 V,
/// `THUMPSND` held at 5 V.
pub(super) const MIX_LIFE: f64 = -0.02128;

/// How far `AUDIO1` moves at DC per volt of `SAUCRFIRESND`: the whole board,
/// op-amps ideal, `EXPLOSND` held at 5 V, `LIFESND` held at 5 V, `SAUCRFIRESND`
/// held at 5 V, `SAUCRSND` held at 5 V, `SHPFIRESND` held at 5 V, `SHPSND` held
/// at 5 V, `THUMPSND` held at 5 V.
pub(super) const MIX_SAUCER_FIRE: f64 = -0.01000;

/// How far `AUDIO1` moves at DC per volt of `SHPFIRESND`: the whole board,
/// op-amps ideal, `EXPLOSND` held at 5 V, `LIFESND` held at 5 V, `SAUCRFIRESND`
/// held at 5 V, `SAUCRSND` held at 5 V, `SHPFIRESND` held at 5 V, `SHPSND` held
/// at 5 V, `THUMPSND` held at 5 V.
pub(super) const MIX_SHIP_FIRE: f64 = -0.01000;

/// How far `AUDIO1` moves at DC per volt of `EXPLOSND`: the whole board,
/// op-amps ideal, `EXPLOSND` held at 5 V, `LIFESND` held at 5 V, `SAUCRFIRESND`
/// held at 5 V, `SAUCRSND` held at 5 V, `SHPFIRESND` held at 5 V, `SHPSND` held
/// at 5 V, `THUMPSND` held at 5 V.
pub(super) const MIX_EXPLOSION: f64 = -0.21277;

/// How far `AUDIO1` moves at DC per volt of `SHPSND`: the whole board, op-amps
/// ideal, `EXPLOSND` held at 5 V, `LIFESND` held at 5 V, `SAUCRFIRESND` held at
/// 5 V, `SAUCRSND` held at 5 V, `SHPFIRESND` held at 5 V, `SHPSND` held at 5 V,
/// `THUMPSND` held at 5 V.
pub(super) const MIX_THRUST: f64 = -0.21277;

/// The time constant of the mode that stores 100.0 % of its energy in
/// `C64`: the whole board, `P11c summing node` held at 5 V, `thump square` held
/// at 0 V.
pub(super) const THUMP_C64_TAU: f64 = 3.08350e-4; // 0.308 ms

/// How far `THUMPSND` moves at DC per volt of `thump square`: the whole board,
/// `P11c summing node` held at 5 V, `thump square` held at 0 V.
pub(super) const THUMP_C64_GAIN: f64 = 0.93439;

/// The time constant of the mode that stores 100.0 % of its energy in
/// `C24`: the whole board, `P11c summing node` held at 5 V, `explosion leg 0`
/// held at 0.25 V, `explosion leg 1` held at 0.25 V, `explosion leg 2` held at
/// 0.25 V, `explosion leg 3` held at 0.25 V.
pub(super) const EXPLOSION_C24_TAU: f64 = 1.84706e-3; // 1.847 ms

/// How far `EXPLOSND` moves at DC per volt of `explosion leg 0`: the whole
/// board, `P11c summing node` held at 5 V, `explosion leg 0` held at 0.25 V,
/// `explosion leg 1` held at 0.25 V, `explosion leg 2` held at 0.25 V,
/// `explosion leg 3` held at 0.25 V.
pub(super) const EXPLOSION_LEG0_GAIN: f64 = 0.32983;

/// How far `EXPLOSND` moves at DC per volt of `explosion leg 1`: the whole
/// board, `P11c summing node` held at 5 V, `explosion leg 0` held at 0.25 V,
/// `explosion leg 1` held at 0.25 V, `explosion leg 2` held at 0.25 V,
/// `explosion leg 3` held at 0.25 V.
pub(super) const EXPLOSION_LEG1_GAIN: f64 = 0.15392;

/// How far `EXPLOSND` moves at DC per volt of `explosion leg 2`: the whole
/// board, `P11c summing node` held at 5 V, `explosion leg 0` held at 0.25 V,
/// `explosion leg 1` held at 0.25 V, `explosion leg 2` held at 0.25 V,
/// `explosion leg 3` held at 0.25 V.
pub(super) const EXPLOSION_LEG2_GAIN: f64 = 0.08396;

/// How far `EXPLOSND` moves at DC per volt of `explosion leg 3`: the whole
/// board, `P11c summing node` held at 5 V, `explosion leg 0` held at 0.25 V,
/// `explosion leg 1` held at 0.25 V, `explosion leg 2` held at 0.25 V,
/// `explosion leg 3` held at 0.25 V.
pub(super) const EXPLOSION_LEG3_GAIN: f64 = 0.03930;

/// The time constant of the mode that stores 100.0 % of its energy in
/// `C62`: the whole board, `P11b summing node` held at 5 V, `band-pass out`
/// held at 5 V, `thrust gated noise` held at 0.25 V.
pub(super) const THRUST_C62_TAU: f64 = 2.10426e-3; // 2.104 ms

/// How far `thrust RC node` moves at DC per volt of `thrust gated noise`: the
/// whole board, `P11b summing node` held at 5 V, `band-pass out` held at 5 V,
/// `thrust gated noise` held at 0.25 V.
pub(super) const THRUST_C62_GAIN: f64 = 0.95635;
