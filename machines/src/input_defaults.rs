//! Shared default physical bindings for typed input controls.
//!
//! Each constant is the `&'static [DefaultBinding]` for one canonical control,
//! mirroring exactly what the legacy name-matching in the frontend
//! (`frontend/src/input.rs::legacy_binding_set`) produced for that button name.
//! Machines migrating to [`InputConfigurable`](phosphor_core::core::machine::InputConfigurable)
//! reference these so the typed defaults stay identical to the historical ones,
//! defined in a single place.
//!
//! Constants are added here as machines that use them are migrated (so the set
//! never contains unused entries). Controls whose legacy bindings are
//! machine-specific (combo buttons, vector game controls, trackball axes)
//! define their bindings inline in the machine file instead.

use phosphor_core::core::machine::{
    AxisSign, DefaultBinding as D, KeyId as K, PadAxis as PA, PadButton as PB, PadControl as P,
};

// Player 1 joystick directions: arrow keys, D-pad, and the left analog stick.
pub const P1_LEFT: &[D] = &[
    D::Key(K::Left),
    D::Pad(P::Button(PB::DPadLeft)),
    D::Pad(P::Axis(PA::LeftX, AxisSign::Negative)),
];
pub const P1_RIGHT: &[D] = &[
    D::Key(K::Right),
    D::Pad(P::Button(PB::DPadRight)),
    D::Pad(P::Axis(PA::LeftX, AxisSign::Positive)),
];
pub const P1_UP: &[D] = &[
    D::Key(K::Up),
    D::Pad(P::Button(PB::DPadUp)),
    D::Pad(P::Axis(PA::LeftY, AxisSign::Negative)),
];
pub const P1_DOWN: &[D] = &[
    D::Key(K::Down),
    D::Pad(P::Button(PB::DPadDown)),
    D::Pad(P::Axis(PA::LeftY, AxisSign::Positive)),
];

// Player 2 joystick directions: WASD-style keys (no gamepad in the legacy map).
pub const P2_LEFT: &[D] = &[D::Key(K::A)];
pub const P2_RIGHT: &[D] = &[D::Key(K::D)];
pub const P2_UP: &[D] = &[D::Key(K::W)];
pub const P2_DOWN: &[D] = &[D::Key(K::S)];

// System buttons.
pub const COIN: &[D] = &[D::Key(K::Num5), D::Pad(P::Button(PB::Back))];
pub const SERVICE: &[D] = &[D::Key(K::Num6)];
pub const P1_START: &[D] = &[D::Key(K::Num1), D::Pad(P::Button(PB::Start))];
pub const P2_START: &[D] = &[D::Key(K::Num2)];

// Primary action button: keyboard key plus gamepad A. The legacy map gives
// gamepad A to the first present of P1 Fire / P1 Flap / P1 Jump / Fire Center,
// so use this for a machine's single primary fire action.
pub const FIRE: &[D] = &[D::Key(K::Space), D::Pad(P::Button(PB::A))];
pub const JUMP: &[D] = &[D::Key(K::LShift), D::Pad(P::Button(PB::A))];

// Twin-stick fire directions (Robotron): IJKL keys and the right analog stick.
pub const P1_FIRE_UP: &[D] = &[
    D::Key(K::I),
    D::Pad(P::Axis(PA::RightY, AxisSign::Negative)),
];
pub const P1_FIRE_DOWN: &[D] = &[
    D::Key(K::K),
    D::Pad(P::Axis(PA::RightY, AxisSign::Positive)),
];
pub const P1_FIRE_LEFT: &[D] = &[
    D::Key(K::J),
    D::Pad(P::Axis(PA::RightX, AxisSign::Negative)),
];
pub const P1_FIRE_RIGHT: &[D] = &[
    D::Key(K::L),
    D::Pad(P::Axis(PA::RightX, AxisSign::Positive)),
];
