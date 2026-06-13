//! Physical-input binding layer.
//!
//! Maps concrete physical inputs (keyboard scancodes, gamepad buttons/axes,
//! mouse buttons/motion) to a machine's logical controls ([`InputId`]). A
//! [`BindingSet`] is built once per machine: from the machine's own
//! [`InputControl`]s (the typed model) when it exposes them, or from the legacy
//! name-matched [`InputButton`]s otherwise (see [`legacy_binding_set`]).
//!
//! The legacy path is a migration bridge: it reproduces the historical
//! name-based default bindings for machines that have not yet been converted to
//! the typed [`phosphor_core::core::machine::InputConfigurable`] model, and is
//! removed once every machine is migrated.

use std::collections::HashMap;

use phosphor_core::core::machine::{
    AnalogInput, AxisSign, DefaultBinding, FrontendMachine, InputButton, InputControl, InputId,
    KeyId, MouseControl, PadAxis as CorePadAxis, PadButton as CorePadButton, PadControl,
};
use sdl2::controller::{Axis, Button};
use sdl2::keyboard::Scancode;
use sdl2::mouse::MouseButton;
use serde::{Deserialize, Serialize};

/// Deadzone threshold for analog sticks acting as digital directions
/// (±10000 of the ±32768 axis range, ~30%), expressed as a normalized fraction.
const STICK_DEADZONE_NORM: f32 = 10_000.0 / 32_768.0;

/// Coarse class of a physical input, used to scope rebinding.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PhysicalCategory {
    Keyboard,
    Pad,
    Mouse,
}

/// Sign of a gamepad-axis deflection used as a digital direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxisDir {
    Positive,
    Negative,
}

/// A mouse motion axis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAxis {
    X,
    Y,
}

/// A concrete physical input that can be bound to a logical control.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PhysicalInput {
    Key(Scancode),
    PadButton(Button),
    PadAxis(Axis, AxisDir),
    MouseButtonInput(MouseButton),
    MouseAxis(MouseAxis),
}

impl PhysicalInput {
    /// Encode as a stable, human-readable token for persistence
    /// (e.g. `"key:4"`, `"pad:a"`, `"padaxis:leftx:-"`, `"mouse:left"`,
    /// `"mouseaxis:y"`). The token is independent of `InputId` numbering.
    pub fn to_token(&self) -> String {
        match self {
            PhysicalInput::Key(sc) => format!("key:{}", *sc as i32),
            PhysicalInput::PadButton(b) => format!("pad:{}", b.string()),
            PhysicalInput::PadAxis(axis, dir) => format!(
                "padaxis:{}:{}",
                axis.string(),
                match dir {
                    AxisDir::Positive => "+",
                    AxisDir::Negative => "-",
                }
            ),
            PhysicalInput::MouseButtonInput(mb) => format!(
                "mouse:{}",
                match mb {
                    MouseButton::Right => "right",
                    MouseButton::Middle => "middle",
                    _ => "left",
                }
            ),
            PhysicalInput::MouseAxis(axis) => format!(
                "mouseaxis:{}",
                match axis {
                    MouseAxis::X => "x",
                    MouseAxis::Y => "y",
                }
            ),
        }
    }

    /// Human-readable name for the rebinding UI (e.g. `"Left"`, `"Pad A"`,
    /// `"Pad LeftX-"`, `"Mouse Left"`, `"Mouse X"`).
    pub fn display_name(&self) -> String {
        match self {
            PhysicalInput::Key(sc) => format!("{sc:?}"),
            PhysicalInput::PadButton(b) => format!("Pad {b:?}"),
            PhysicalInput::PadAxis(axis, dir) => format!(
                "Pad {axis:?}{}",
                match dir {
                    AxisDir::Positive => "+",
                    AxisDir::Negative => "-",
                }
            ),
            PhysicalInput::MouseButtonInput(mb) => format!("Mouse {mb:?}"),
            PhysicalInput::MouseAxis(axis) => format!("Mouse {axis:?}"),
        }
    }

    /// Category used when rebinding: replacing a control's keyboard binding
    /// leaves its gamepad/mouse bindings intact, and vice versa.
    fn category(&self) -> PhysicalCategory {
        match self {
            PhysicalInput::Key(_) => PhysicalCategory::Keyboard,
            PhysicalInput::PadButton(_) | PhysicalInput::PadAxis(..) => PhysicalCategory::Pad,
            PhysicalInput::MouseButtonInput(_) | PhysicalInput::MouseAxis(_) => {
                PhysicalCategory::Mouse
            }
        }
    }

    /// Parse a token produced by [`to_token`](Self::to_token). Returns `None`
    /// for unrecognized tokens (e.g. a key name no longer known to SDL), so
    /// stale persisted bindings are skipped rather than failing the load.
    pub fn from_token(token: &str) -> Option<Self> {
        let (kind, rest) = token.split_once(':')?;
        match kind {
            "key" => Scancode::from_i32(rest.parse().ok()?).map(PhysicalInput::Key),
            "pad" => Button::from_string(rest).map(PhysicalInput::PadButton),
            "padaxis" => {
                let (axis, sign) = rest.rsplit_once(':')?;
                let dir = match sign {
                    "+" => AxisDir::Positive,
                    "-" => AxisDir::Negative,
                    _ => return None,
                };
                Some(PhysicalInput::PadAxis(Axis::from_string(axis)?, dir))
            }
            "mouse" => Some(PhysicalInput::MouseButtonInput(match rest {
                "left" => MouseButton::Left,
                "right" => MouseButton::Right,
                "middle" => MouseButton::Middle,
                _ => return None,
            })),
            "mouseaxis" => Some(PhysicalInput::MouseAxis(match rest {
                "x" => MouseAxis::X,
                "y" => MouseAxis::Y,
                _ => return None,
            })),
            _ => None,
        }
    }
}

/// A persisted binding: a control referenced by its stable name plus a physical
/// input token. Stored per machine (keyed by `machine_id`) so saved configs
/// survive `InputId` renumbering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SerializedBinding {
    pub control: String,
    pub input: String,
}

/// One physical input bound to one logical control.
#[derive(Clone, Copy)]
pub struct InputBinding {
    pub physical: PhysicalInput,
    pub target: InputId,
    /// Multiplier applied to analog motion (relative axes). Unused for digital.
    pub scale: f32,
    /// Normalized deflection (0..1) past which a gamepad axis counts as pressed.
    pub deadzone: f32,
}

/// All physical→logical bindings active for the current machine.
///
/// Lookups are linear scans; binding counts are tiny (tens at most), so this is
/// cheaper and simpler than indexing, and supports multiple bindings per input.
pub struct BindingSet {
    bindings: Vec<InputBinding>,
}

impl BindingSet {
    fn new() -> Self {
        Self {
            bindings: Vec::new(),
        }
    }

    /// Bind a digital physical input (key / button) to a control.
    fn bind(&mut self, physical: PhysicalInput, target: InputId) {
        self.bindings.push(InputBinding {
            physical,
            target,
            scale: 1.0,
            deadzone: 0.0,
        });
    }

    /// Bind a gamepad axis (acting as a digital direction) with the stick deadzone.
    fn bind_pad_axis(&mut self, axis: Axis, dir: AxisDir, target: InputId) {
        self.bindings.push(InputBinding {
            physical: PhysicalInput::PadAxis(axis, dir),
            target,
            scale: 1.0,
            deadzone: STICK_DEADZONE_NORM,
        });
    }

    /// Bind a relative mouse axis to an analog control.
    fn bind_mouse_axis(&mut self, axis: MouseAxis, target: InputId) {
        self.bindings.push(InputBinding {
            physical: PhysicalInput::MouseAxis(axis),
            target,
            scale: 1.0,
            deadzone: 0.0,
        });
    }

    /// Logical controls bound to an exact digital physical input.
    pub fn digital_targets(&self, physical: PhysicalInput) -> impl Iterator<Item = InputId> + '_ {
        self.bindings
            .iter()
            .filter(move |b| b.physical == physical)
            .map(|b| b.target)
    }

    /// Gamepad-axis targets for `axis`, as (control, direction, deadzone).
    pub fn pad_axis_targets(
        &self,
        axis: Axis,
    ) -> impl Iterator<Item = (InputId, AxisDir, f32)> + '_ {
        self.bindings.iter().filter_map(move |b| match b.physical {
            PhysicalInput::PadAxis(a, dir) if a == axis => Some((b.target, dir, b.deadzone)),
            _ => None,
        })
    }

    /// Relative-mouse-axis targets for `axis`, as (control, scale).
    pub fn mouse_axis_targets(&self, axis: MouseAxis) -> impl Iterator<Item = (InputId, f32)> + '_ {
        self.bindings.iter().filter_map(move |b| match b.physical {
            PhysicalInput::MouseAxis(a) if a == axis => Some((b.target, b.scale)),
            _ => None,
        })
    }

    /// Build a binding set from a machine's typed controls' default bindings.
    pub fn from_controls(controls: &[InputControl]) -> Self {
        let mut set = BindingSet::new();
        for control in controls {
            for binding in control.default_bindings {
                let physical = match *binding {
                    DefaultBinding::Key(key) => PhysicalInput::Key(key_to_scancode(key)),
                    DefaultBinding::Pad(pad) => pad_to_physical(pad),
                    DefaultBinding::Mouse(mouse) => mouse_to_physical(mouse),
                };
                let deadzone = match physical {
                    PhysicalInput::PadAxis(..) => STICK_DEADZONE_NORM,
                    _ => 0.0,
                };
                set.bindings.push(InputBinding {
                    physical,
                    target: control.id,
                    scale: 1.0,
                    deadzone,
                });
            }
        }
        set
    }

    /// Serialize the current bindings, mapping each target `InputId` back to its
    /// control's stable name. Bindings whose target has no named control (e.g.
    /// a not-yet-migrated machine) are skipped.
    pub fn to_serialized(&self, controls: &[InputControl]) -> Vec<SerializedBinding> {
        let id_to_name: HashMap<InputId, &str> =
            controls.iter().map(|c| (c.id, c.stable_name)).collect();
        self.bindings
            .iter()
            .filter_map(|b| {
                id_to_name.get(&b.target).map(|name| SerializedBinding {
                    control: (*name).to_string(),
                    input: b.physical.to_token(),
                })
            })
            .collect()
    }

    /// Overlay persisted bindings onto the defaults. Any control named in
    /// `saved` has its default bindings replaced by the saved ones; controls
    /// not mentioned keep their defaults (so controls added in a later version
    /// still work). Unknown control names and unparseable tokens are ignored.
    pub fn apply_overrides(&mut self, controls: &[InputControl], saved: &[SerializedBinding]) {
        let name_to_id: HashMap<&str, InputId> =
            controls.iter().map(|c| (c.stable_name, c.id)).collect();

        // Controls whose defaults are being overridden.
        let touched: Vec<InputId> = saved
            .iter()
            .filter_map(|s| name_to_id.get(s.control.as_str()).copied())
            .collect();
        self.bindings.retain(|b| !touched.contains(&b.target));

        for s in saved {
            if let (Some(&target), Some(physical)) = (
                name_to_id.get(s.control.as_str()),
                PhysicalInput::from_token(&s.input),
            ) {
                let deadzone = match physical {
                    PhysicalInput::PadAxis(..) => STICK_DEADZONE_NORM,
                    _ => 0.0,
                };
                self.bindings.push(InputBinding {
                    physical,
                    target,
                    scale: 1.0,
                    deadzone,
                });
            }
        }
    }

    /// Physical inputs currently bound to a control (for the rebinding UI).
    pub fn physical_for(&self, target: InputId) -> impl Iterator<Item = PhysicalInput> + '_ {
        self.bindings
            .iter()
            .filter(move |b| b.target == target)
            .map(|b| b.physical)
    }

    /// Rebind a control to a captured physical input, replacing only the
    /// control's existing bindings of the same category (keyboard / pad /
    /// mouse), so rebinding a key keeps the gamepad binding and vice versa.
    pub fn rebind(&mut self, target: InputId, physical: PhysicalInput) {
        let category = physical.category();
        self.bindings
            .retain(|b| b.target != target || b.physical.category() != category);
        let deadzone = match physical {
            PhysicalInput::PadAxis(..) => STICK_DEADZONE_NORM,
            _ => 0.0,
        };
        self.bindings.push(InputBinding {
            physical,
            target,
            scale: 1.0,
            deadzone,
        });
    }
}

/// Order-insensitive equality of two serialized binding lists, used to decide
/// whether a machine's bindings differ from its defaults (and thus need saving).
pub fn bindings_eq(a: &[SerializedBinding], b: &[SerializedBinding]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let key = |s: &SerializedBinding| (s.control.clone(), s.input.clone());
    let mut a = a.to_vec();
    let mut b = b.to_vec();
    a.sort_by_key(key);
    b.sort_by_key(key);
    a == b
}

/// Build the active binding set for a machine.
///
/// Uses the machine's typed [`InputControl`]s when present, falling back to the
/// legacy name-matched defaults for machines not yet migrated.
pub fn build_bindings(machine: &dyn FrontendMachine) -> BindingSet {
    let controls = machine.input_controls();
    if controls.is_empty() {
        legacy_binding_set(machine.input_map(), machine.analog_map())
    } else {
        BindingSet::from_controls(controls)
    }
}

// ---------------------------------------------------------------------------
// Core descriptor → SDL translation
// ---------------------------------------------------------------------------

fn pad_to_physical(pad: PadControl) -> PhysicalInput {
    match pad {
        PadControl::Button(button) => PhysicalInput::PadButton(pad_button(button)),
        PadControl::Axis(axis, sign) => PhysicalInput::PadAxis(
            pad_axis(axis),
            match sign {
                AxisSign::Positive => AxisDir::Positive,
                AxisSign::Negative => AxisDir::Negative,
            },
        ),
    }
}

fn mouse_to_physical(mouse: MouseControl) -> PhysicalInput {
    match mouse {
        MouseControl::Left => PhysicalInput::MouseButtonInput(MouseButton::Left),
        MouseControl::Right => PhysicalInput::MouseButtonInput(MouseButton::Right),
        MouseControl::Middle => PhysicalInput::MouseButtonInput(MouseButton::Middle),
        MouseControl::AxisX => PhysicalInput::MouseAxis(MouseAxis::X),
        MouseControl::AxisY => PhysicalInput::MouseAxis(MouseAxis::Y),
    }
}

fn pad_button(button: CorePadButton) -> Button {
    match button {
        CorePadButton::A => Button::A,
        CorePadButton::B => Button::B,
        CorePadButton::X => Button::X,
        CorePadButton::Y => Button::Y,
        CorePadButton::Back => Button::Back,
        CorePadButton::Start => Button::Start,
        CorePadButton::Guide => Button::Guide,
        CorePadButton::LeftShoulder => Button::LeftShoulder,
        CorePadButton::RightShoulder => Button::RightShoulder,
        CorePadButton::LeftStick => Button::LeftStick,
        CorePadButton::RightStick => Button::RightStick,
        CorePadButton::DPadUp => Button::DPadUp,
        CorePadButton::DPadDown => Button::DPadDown,
        CorePadButton::DPadLeft => Button::DPadLeft,
        CorePadButton::DPadRight => Button::DPadRight,
    }
}

fn pad_axis(axis: CorePadAxis) -> Axis {
    match axis {
        CorePadAxis::LeftX => Axis::LeftX,
        CorePadAxis::LeftY => Axis::LeftY,
        CorePadAxis::RightX => Axis::RightX,
        CorePadAxis::RightY => Axis::RightY,
        CorePadAxis::TriggerLeft => Axis::TriggerLeft,
        CorePadAxis::TriggerRight => Axis::TriggerRight,
    }
}

#[rustfmt::skip]
fn key_to_scancode(key: KeyId) -> Scancode {
    match key {
        KeyId::A => Scancode::A, KeyId::B => Scancode::B, KeyId::C => Scancode::C,
        KeyId::D => Scancode::D, KeyId::E => Scancode::E, KeyId::F => Scancode::F,
        KeyId::G => Scancode::G, KeyId::H => Scancode::H, KeyId::I => Scancode::I,
        KeyId::J => Scancode::J, KeyId::K => Scancode::K, KeyId::L => Scancode::L,
        KeyId::M => Scancode::M, KeyId::N => Scancode::N, KeyId::O => Scancode::O,
        KeyId::P => Scancode::P, KeyId::Q => Scancode::Q, KeyId::R => Scancode::R,
        KeyId::S => Scancode::S, KeyId::T => Scancode::T, KeyId::U => Scancode::U,
        KeyId::V => Scancode::V, KeyId::W => Scancode::W, KeyId::X => Scancode::X,
        KeyId::Y => Scancode::Y, KeyId::Z => Scancode::Z,
        KeyId::Num0 => Scancode::Num0, KeyId::Num1 => Scancode::Num1,
        KeyId::Num2 => Scancode::Num2, KeyId::Num3 => Scancode::Num3,
        KeyId::Num4 => Scancode::Num4, KeyId::Num5 => Scancode::Num5,
        KeyId::Num6 => Scancode::Num6, KeyId::Num7 => Scancode::Num7,
        KeyId::Num8 => Scancode::Num8, KeyId::Num9 => Scancode::Num9,
        KeyId::Up => Scancode::Up, KeyId::Down => Scancode::Down,
        KeyId::Left => Scancode::Left, KeyId::Right => Scancode::Right,
        KeyId::Space => Scancode::Space, KeyId::Enter => Scancode::Return,
        KeyId::Tab => Scancode::Tab, KeyId::Escape => Scancode::Escape,
        KeyId::LShift => Scancode::LShift, KeyId::RShift => Scancode::RShift,
        KeyId::LCtrl => Scancode::LCtrl, KeyId::RCtrl => Scancode::RCtrl,
        KeyId::LAlt => Scancode::LAlt, KeyId::RAlt => Scancode::RAlt,
    }
}

// ---------------------------------------------------------------------------
// Legacy name-matched defaults (migration bridge — removed once all machines
// expose typed InputControls)
// ---------------------------------------------------------------------------

/// Build a binding set from legacy name-matched [`InputButton`]s.
///
/// Reproduces the historical default keyboard/gamepad/mouse bindings by button
/// name, so machines that have not yet migrated to typed controls keep their
/// existing defaults. The logical control id is the button's `u8` id widened to
/// [`InputId`], matching the `InputConfigurable` bridge in core.
pub fn legacy_binding_set(buttons: &[InputButton], analog: &[AnalogInput]) -> BindingSet {
    let mut set = BindingSet::new();

    // --- Keyboard ---
    for button in buttons {
        let id = InputId(button.id as u16);

        // Exact name match first, then substring fallback for combo names
        // like "Jump L / P1 Start".
        let scancode = match button.name {
            "P1 Left" => Some(Scancode::Left),
            "P1 Right" => Some(Scancode::Right),
            "P1 Up" => Some(Scancode::Up),
            "P1 Down" => Some(Scancode::Down),
            "P1 Flap" => Some(Scancode::LCtrl),
            "P1 Jump" => Some(Scancode::LShift),
            "P1 Fire" => Some(Scancode::Space),
            "P1 Start" => Some(Scancode::Num1),

            "P2 Left" => Some(Scancode::A),
            "P2 Right" => Some(Scancode::D),
            "P2 Up" => Some(Scancode::W),
            "P2 Down" => Some(Scancode::S),
            "P2 Flap" => Some(Scancode::W),
            "P2 Jump" => Some(Scancode::E),
            "P2 Fire" => Some(Scancode::E),
            "P2 Start" => Some(Scancode::Num2),

            "P1 Fire Up" => Some(Scancode::I),
            "P1 Fire Down" => Some(Scancode::K),
            "P1 Fire Left" => Some(Scancode::J),
            "P1 Fire Right" => Some(Scancode::L),

            "Fire Left" => Some(Scancode::Z),
            "Fire Center" => Some(Scancode::X),
            "Fire Right" => Some(Scancode::C),

            "P1 Throw" => Some(Scancode::Space),
            "P2 Throw" => Some(Scancode::RShift),
            "Self-Test" => Some(Scancode::T),

            "Coin" => Some(Scancode::Num5),
            "Service" => Some(Scancode::Num6),

            name if name.contains("P1 Start") => Some(Scancode::Num1),
            name if name.contains("P2 Start") => Some(Scancode::Num2),
            name if name.contains("Coin") => Some(Scancode::Num5),

            _ => None,
        };
        if let Some(sc) = scancode {
            set.bind(PhysicalInput::Key(sc), id);
        }

        // Secondary bindings: combo buttons that serve dual roles (e.g. a jump
        // button that also acts as a start button) get an action key too.
        if button.name.contains("Jump") {
            if button.name.contains(" L") {
                set.bind(PhysicalInput::Key(Scancode::Space), id);
            } else if button.name.contains(" R") {
                set.bind(PhysicalInput::Key(Scancode::LShift), id);
            }
        }
    }

    // --- Gamepad ---
    let by_name = |name: &str| buttons.iter().find(|b| b.name == name).map(|b| b.id);
    let by_name_or_contains = |name: &str| {
        by_name(name).or_else(|| buttons.iter().find(|b| b.name.contains(name)).map(|b| b.id))
    };
    let bind_button = |set: &mut BindingSet, button: Button, id: u8| {
        set.bind(PhysicalInput::PadButton(button), InputId(id as u16));
    };

    // Face button A → primary action (first present of these names).
    for name in ["P1 Fire", "P1 Flap", "P1 Jump", "Fire Center"] {
        if let Some(id) = by_name(name) {
            bind_button(&mut set, Button::A, id);
            break;
        }
    }
    // Missile Command: three fire buttons on X/A/B.
    if let Some(id) = by_name("Fire Left") {
        bind_button(&mut set, Button::X, id);
    }
    if let Some(id) = by_name("Fire Right") {
        bind_button(&mut set, Button::B, id);
    }
    // Asteroids: hyperspace on B, thrust on Y.
    if let Some(id) = by_name("Hyperspace") {
        bind_button(&mut set, Button::B, id);
    }
    if let Some(id) = by_name("Thrust") {
        bind_button(&mut set, Button::Y, id);
    }
    // System buttons (exact match first, then substring fallback for combo names).
    if let Some(id) = by_name_or_contains("Coin") {
        bind_button(&mut set, Button::Back, id);
    }
    if let Some(id) = by_name_or_contains("P1 Start") {
        bind_button(&mut set, Button::Start, id);
    }
    // D-pad → P1 directions.
    if let Some(id) = by_name("P1 Left") {
        bind_button(&mut set, Button::DPadLeft, id);
    }
    if let Some(id) = by_name("P1 Right") {
        bind_button(&mut set, Button::DPadRight, id);
    }
    if let Some(id) = by_name("P1 Up") {
        bind_button(&mut set, Button::DPadUp, id);
    }
    if let Some(id) = by_name("P1 Down") {
        bind_button(&mut set, Button::DPadDown, id);
    }
    // Left stick → P1 directions.
    if let Some(id) = by_name("P1 Left") {
        set.bind_pad_axis(Axis::LeftX, AxisDir::Negative, InputId(id as u16));
    }
    if let Some(id) = by_name("P1 Right") {
        set.bind_pad_axis(Axis::LeftX, AxisDir::Positive, InputId(id as u16));
    }
    if let Some(id) = by_name("P1 Up") {
        set.bind_pad_axis(Axis::LeftY, AxisDir::Negative, InputId(id as u16));
    }
    if let Some(id) = by_name("P1 Down") {
        set.bind_pad_axis(Axis::LeftY, AxisDir::Positive, InputId(id as u16));
    }
    // Right stick → fire directions (Robotron twin-stick).
    if let Some(id) = by_name("P1 Fire Left") {
        set.bind_pad_axis(Axis::RightX, AxisDir::Negative, InputId(id as u16));
    }
    if let Some(id) = by_name("P1 Fire Right") {
        set.bind_pad_axis(Axis::RightX, AxisDir::Positive, InputId(id as u16));
    }
    if let Some(id) = by_name("P1 Fire Up") {
        set.bind_pad_axis(Axis::RightY, AxisDir::Negative, InputId(id as u16));
    }
    if let Some(id) = by_name("P1 Fire Down") {
        set.bind_pad_axis(Axis::RightY, AxisDir::Positive, InputId(id as u16));
    }

    // --- Mouse ---
    // Relative trackball/spinner motion → first two analog axes.
    if let Some(a) = analog.first() {
        set.bind_mouse_axis(MouseAxis::X, InputId(a.id as u16));
    }
    if let Some(a) = analog.get(1) {
        set.bind_mouse_axis(MouseAxis::Y, InputId(a.id as u16));
    }
    // Mouse buttons → fire (trackball games).
    let left = by_name("Fire Center")
        .or_else(|| by_name("P1 Fire"))
        .or_else(|| {
            buttons
                .iter()
                .find(|b| b.name.contains("Jump L"))
                .map(|b| b.id)
        });
    if let Some(id) = left {
        set.bind(
            PhysicalInput::MouseButtonInput(MouseButton::Left),
            InputId(id as u16),
        );
    }
    let right = by_name("Fire Right").or_else(|| {
        buttons
            .iter()
            .find(|b| b.name.contains("Jump R"))
            .map(|b| b.id)
    });
    if let Some(id) = right {
        set.bind(
            PhysicalInput::MouseButtonInput(MouseButton::Right),
            InputId(id as u16),
        );
    }
    if let Some(id) = by_name("Fire Left") {
        set.bind(
            PhysicalInput::MouseButtonInput(MouseButton::Middle),
            InputId(id as u16),
        );
    }

    set
}

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::InputKind;

    fn ids(it: impl Iterator<Item = InputId>) -> Vec<u16> {
        it.map(|i| i.0).collect()
    }

    #[test]
    fn legacy_keyboard_bindings_match_button_names() {
        let buttons = [
            InputButton {
                id: 0,
                name: "P1 Left",
            },
            InputButton {
                id: 2,
                name: "P1 Flap",
            },
            InputButton {
                id: 8,
                name: "Coin",
            },
        ];
        let set = legacy_binding_set(&buttons, &[]);
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::Key(Scancode::Left))),
            vec![0]
        );
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::Key(Scancode::LCtrl))),
            vec![2]
        );
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::Key(Scancode::Num5))),
            vec![8]
        );
    }

    #[test]
    fn legacy_combo_jump_gets_secondary_action_key() {
        // Crystal Castles "Jump L / P1 Start" binds both Num1 (P1 Start
        // substring) and the Space action key.
        let buttons = [InputButton {
            id: 3,
            name: "Jump L / P1 Start",
        }];
        let set = legacy_binding_set(&buttons, &[]);
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::Key(Scancode::Num1))),
            vec![3]
        );
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::Key(Scancode::Space))),
            vec![3]
        );
    }

    #[test]
    fn legacy_mouse_axes_bind_to_analog_inputs() {
        let analog = [
            AnalogInput {
                id: 0,
                name: "Trackball X",
            },
            AnalogInput {
                id: 1,
                name: "Trackball Y",
            },
        ];
        let set = legacy_binding_set(&[], &analog);
        assert_eq!(
            set.mouse_axis_targets(MouseAxis::X)
                .map(|(id, _)| id.0)
                .collect::<Vec<_>>(),
            vec![0]
        );
        assert_eq!(
            set.mouse_axis_targets(MouseAxis::Y)
                .map(|(id, _)| id.0)
                .collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn physical_input_token_round_trips() {
        let cases = [
            PhysicalInput::Key(Scancode::Left),
            PhysicalInput::Key(Scancode::Num5),
            PhysicalInput::PadButton(Button::A),
            PhysicalInput::PadButton(Button::DPadLeft),
            PhysicalInput::PadAxis(Axis::LeftX, AxisDir::Negative),
            PhysicalInput::PadAxis(Axis::LeftY, AxisDir::Positive),
            PhysicalInput::MouseButtonInput(MouseButton::Left),
            PhysicalInput::MouseButtonInput(MouseButton::Right),
            PhysicalInput::MouseButtonInput(MouseButton::Middle),
            PhysicalInput::MouseAxis(MouseAxis::X),
            PhysicalInput::MouseAxis(MouseAxis::Y),
        ];
        for c in cases {
            let token = c.to_token();
            assert_eq!(PhysicalInput::from_token(&token), Some(c), "token {token}");
        }
        assert_eq!(PhysicalInput::from_token("bogus:1"), None);
        assert_eq!(PhysicalInput::from_token("key:not_a_number"), None);
    }

    const TEST_CONTROLS: &[InputControl] = &[
        InputControl {
            id: InputId(0),
            stable_name: "fire",
            label: "Fire",
            kind: InputKind::Button,
            player: Some(1),
            default_bindings: &[DefaultBinding::Key(KeyId::Space)],
        },
        InputControl {
            id: InputId(1),
            stable_name: "coin",
            label: "Coin",
            kind: InputKind::Coin,
            player: None,
            default_bindings: &[DefaultBinding::Key(KeyId::Num5)],
        },
    ];

    #[test]
    fn to_serialized_uses_stable_names() {
        let set = BindingSet::from_controls(TEST_CONTROLS);
        let ser = set.to_serialized(TEST_CONTROLS);
        assert!(ser.contains(&SerializedBinding {
            control: "fire".to_string(),
            input: PhysicalInput::Key(Scancode::Space).to_token(),
        }));
        assert!(ser.contains(&SerializedBinding {
            control: "coin".to_string(),
            input: PhysicalInput::Key(Scancode::Num5).to_token(),
        }));
    }

    #[test]
    fn apply_overrides_replaces_only_named_controls() {
        let mut set = BindingSet::from_controls(TEST_CONTROLS);
        // Rebind "fire" from Space to Enter; leave "coin" untouched.
        let saved = vec![SerializedBinding {
            control: "fire".to_string(),
            input: PhysicalInput::Key(Scancode::Return).to_token(),
        }];
        set.apply_overrides(TEST_CONTROLS, &saved);

        // fire now responds to Enter, not Space.
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::Key(Scancode::Return))),
            vec![0]
        );
        assert!(ids(set.digital_targets(PhysicalInput::Key(Scancode::Space))).is_empty());
        // coin default preserved.
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::Key(Scancode::Num5))),
            vec![1]
        );
    }

    #[test]
    fn bindings_eq_is_order_insensitive() {
        let a = vec![
            SerializedBinding {
                control: "fire".into(),
                input: "key:1".into(),
            },
            SerializedBinding {
                control: "coin".into(),
                input: "key:2".into(),
            },
        ];
        let b = vec![
            SerializedBinding {
                control: "coin".into(),
                input: "key:2".into(),
            },
            SerializedBinding {
                control: "fire".into(),
                input: "key:1".into(),
            },
        ];
        assert!(bindings_eq(&a, &b));
        let c = vec![SerializedBinding {
            control: "fire".into(),
            input: "key:9".into(),
        }];
        assert!(!bindings_eq(&a, &c));
    }

    #[test]
    fn from_controls_translates_default_bindings() {
        const CONTROLS: &[InputControl] = &[InputControl {
            id: InputId(5),
            stable_name: "p1_fire",
            label: "P1 Fire",
            kind: InputKind::Button,
            player: Some(1),
            default_bindings: &[
                DefaultBinding::Key(KeyId::Space),
                DefaultBinding::Pad(PadControl::Button(CorePadButton::A)),
            ],
        }];
        let set = BindingSet::from_controls(CONTROLS);
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::Key(Scancode::Space))),
            vec![5]
        );
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::PadButton(Button::A))),
            vec![5]
        );
    }
}
