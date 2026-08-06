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

// Player 2 joystick directions: WASD-style keys, plus the same pad controls
// player 1 gets. The frontend scopes pad bindings to the owning player's slot,
// so these reach player 2's controller rather than fighting player 1's.
pub const P2_LEFT: &[D] = &[
    D::Key(K::A),
    D::Pad(P::Button(PB::DPadLeft)),
    D::Pad(P::Axis(PA::LeftX, AxisSign::Negative)),
];
pub const P2_RIGHT: &[D] = &[
    D::Key(K::D),
    D::Pad(P::Button(PB::DPadRight)),
    D::Pad(P::Axis(PA::LeftX, AxisSign::Positive)),
];
pub const P2_UP: &[D] = &[
    D::Key(K::W),
    D::Pad(P::Button(PB::DPadUp)),
    D::Pad(P::Axis(PA::LeftY, AxisSign::Negative)),
];
pub const P2_DOWN: &[D] = &[
    D::Key(K::S),
    D::Pad(P::Button(PB::DPadDown)),
    D::Pad(P::Axis(PA::LeftY, AxisSign::Positive)),
];

// System buttons.
pub const COIN: &[D] = &[D::Key(K::Num5), D::Pad(P::Button(PB::Back))];
pub const SERVICE: &[D] = &[D::Key(K::Num6)];
pub const P1_START: &[D] = &[D::Key(K::Num1), D::Pad(P::Button(PB::Start))];
pub const P2_START: &[D] = &[D::Key(K::Num2)];

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
