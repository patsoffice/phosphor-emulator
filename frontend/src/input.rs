//! Physical-input binding layer.
//!
//! Maps concrete physical inputs (keyboard scancodes, gamepad buttons/axes,
//! mouse buttons/motion) to a machine's logical controls ([`InputId`]). A
//! [`BindingSet`] is built once per machine from the machine's
//! [`InputControl`]s (their default bindings), and can be re-bound at runtime.

use std::collections::HashMap;

use phosphor_core::core::machine::{
    AxisSign, DefaultBinding, FrontendMachine, InputConfigurable, InputControl, InputEvent,
    InputId, InputKind, KeyId, MouseControl, PadAxis as CorePadAxis, PadButton as CorePadButton,
    PadControl,
};
use sdl2::controller::{Axis, Button};
use sdl2::event::Event;
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
    pub fn to_token(self) -> String {
        match self {
            PhysicalInput::Key(sc) => format!("key:{}", sc as i32),
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
/// input token. Stored per machine (under that machine's `MachineSettings`) so
/// saved configs survive `InputId` renumbering.
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

    /// Every binding, for callers that must walk the whole set (see [`resync`]).
    fn all(&self) -> impl Iterator<Item = &InputBinding> + '_ {
        self.bindings.iter()
    }

    /// Build a binding set from a machine's typed controls' default bindings.
    pub fn from_controls(controls: &[InputControl]) -> Self {
        let mut set = BindingSet::new();
        for control in controls {
            // Action controls draw their physical defaults from the shared role
            // ladder; `default_bindings` then carries only machine-specific extras
            // (e.g. a trackball cabinet's mouse button), unioned on top.
            let role_defaults = match control.kind {
                InputKind::Action(role) => role.default_bindings(control.player),
                _ => &[][..],
            };
            for binding in role_defaults.iter().chain(control.default_bindings) {
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

/// Build the active binding set for a machine from its typed controls'
/// default bindings.
pub fn build_bindings(machine: &dyn FrontendMachine) -> BindingSet {
    BindingSet::from_controls(machine.input_controls())
}

/// Everything [`dispatch`] needs from the frontend besides the bindings.
#[derive(Clone, Copy)]
pub struct DispatchCtx {
    /// egui holds keyboard focus. Suppresses key *presses* but never releases.
    pub egui_wants_keyboard: bool,
    /// The cursor is captured for the game rather than the UI.
    pub mouse_grabbed: bool,
}

/// Translate one SDL event into machine input, returning `true` when the event
/// was consumed as game input.
///
/// Hotkeys, hotplug and window events are deliberately *not* handled here —
/// they mutate frontend state (the controller list, resync flags) rather than
/// the machine, and they must keep matching before this in the caller so hotkey
/// precedence is unchanged.
pub fn dispatch<M: InputConfigurable + ?Sized>(
    event: &Event,
    bindings: &BindingSet,
    machine: &mut M,
    ctx: DispatchCtx,
) -> bool {
    let mut press = |physical, pressed| {
        for id in bindings.digital_targets(physical) {
            machine.handle_input(InputEvent::Button { id, pressed });
        }
    };

    match event {
        // Keyboard presses only reach the game when egui does not want them.
        Event::KeyDown {
            scancode: Some(sc),
            repeat: false,
            ..
        } if !ctx.egui_wants_keyboard => {
            press(PhysicalInput::Key(*sc), true);
            true
        }

        // Releases dispatch unconditionally, even if egui now wants the
        // keyboard. The press above is gated, so if egui grabs focus while a
        // game key is held (held arrows move egui's widget focus, flipping
        // `wants_keyboard` true), a guarded release would be dropped and the
        // button would stick "on". An extra release is idempotent.
        Event::KeyUp {
            scancode: Some(sc), ..
        } => {
            press(PhysicalInput::Key(*sc), false);
            true
        }

        // Pad buttons — egui never intercepts these.
        Event::ControllerButtonDown { button, .. } => {
            press(PhysicalInput::PadButton(*button), true);
            true
        }
        Event::ControllerButtonUp { button, .. } => {
            press(PhysicalInput::PadButton(*button), false);
            true
        }

        // Analog stick standing in for digital directions.
        Event::ControllerAxisMotion { axis, value, .. } => {
            let normalized = f32::from(*value) / 32_768.0;
            for (id, dir, deadzone) in bindings.pad_axis_targets(*axis) {
                let pressed = match dir {
                    AxisDir::Positive => normalized > deadzone,
                    AxisDir::Negative => normalized < -deadzone,
                };
                machine.handle_input(InputEvent::Button { id, pressed });
            }
            true
        }

        // Mouse motion → analog axes (trackball games). When grabbed, the
        // cursor belongs to the game (captured and warped to window center), so
        // route motion unconditionally — egui's `wants_pointer` would otherwise
        // report the warped cursor as "over an area" and swallow every delta.
        Event::MouseMotion { xrel, yrel, .. } if ctx.mouse_grabbed => {
            for (id, scale) in bindings.mouse_axis_targets(MouseAxis::X) {
                let delta = *xrel as f32 * scale;
                machine.handle_input(InputEvent::Relative { id, delta });
            }
            for (id, scale) in bindings.mouse_axis_targets(MouseAxis::Y) {
                let delta = *yrel as f32 * scale;
                machine.handle_input(InputEvent::Relative { id, delta });
            }
            true
        }

        Event::MouseButtonDown { mouse_btn, .. } if ctx.mouse_grabbed => {
            press(PhysicalInput::MouseButtonInput(*mouse_btn), true);
            true
        }

        // Unconditional for the same reason as `KeyUp`: F11 can clear
        // `mouse_grabbed` while a button is held, and a guarded release would
        // strand it down.
        Event::MouseButtonUp { mouse_btn, .. } => {
            press(PhysicalInput::MouseButtonInput(*mouse_btn), false);
            true
        }

        _ => false,
    }
}

/// Live state of the physical devices, as [`resync`] needs to see it.
///
/// A trait rather than the SDL types directly so the reconciliation logic is
/// exercisable without an SDL context — `KeyboardState` and `GameController`
/// can only be obtained from a live event pump.
pub trait DeviceState {
    fn key_pressed(&self, scancode: Scancode) -> bool;
    /// Any connected pad holding `button`.
    fn pad_button_pressed(&self, button: Button) -> bool;
    /// Deflection of `axis` on whichever pad is pushing it hardest, normalized
    /// to `-1.0..=1.0`.
    fn pad_axis(&self, axis: Axis) -> f32;
    /// `false` whenever the mouse is ungrabbed — the cursor belongs to the UI
    /// then, so from the game's point of view no mouse button is down.
    fn mouse_button_pressed(&self, button: MouseButton) -> bool;
}

/// Re-assert the true current state of every binding.
///
/// Used where the machine's idea of what is held and the physical devices' can
/// diverge: after a reset or a state load (which rewrite the machine's port bits
/// with no corresponding key event), and on regaining window focus. Stronger
/// than `release_all_inputs`, which only clears — this restores, so a direction
/// held across a reset keeps working instead of going dead until the user
/// releases and re-presses it.
///
/// Mouse *axes* are relative and carry no re-assertable state, so they are
/// skipped; every other binding is driven to its live value.
///
/// Must run *after* the mutation it reconciles, never before.
pub fn resync<M: InputConfigurable + ?Sized>(
    bindings: &BindingSet,
    machine: &mut M,
    devices: &dyn DeviceState,
) {
    for binding in bindings.all() {
        let pressed = match binding.physical {
            PhysicalInput::Key(sc) => devices.key_pressed(sc),
            PhysicalInput::PadButton(b) => devices.pad_button_pressed(b),
            PhysicalInput::PadAxis(axis, dir) => {
                let deflection = devices.pad_axis(axis);
                match dir {
                    AxisDir::Positive => deflection > binding.deadzone,
                    AxisDir::Negative => deflection < -binding.deadzone,
                }
            }
            PhysicalInput::MouseButtonInput(mb) => devices.mouse_button_pressed(mb),
            // Relative motion has no "current value" to re-assert.
            PhysicalInput::MouseAxis(_) => continue,
        };
        machine.handle_input(InputEvent::Button {
            id: binding.target,
            pressed,
        });
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

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::{ActionRole, InputKind, MouseControl};

    fn ids(it: impl Iterator<Item = InputId>) -> Vec<u16> {
        it.map(|i| i.0).collect()
    }

    /// Minimal `InputConfigurable` that just records what it is handed —
    /// `resync` needs nothing more, which is why it takes that bound rather
    /// than the full `FrontendMachine`.
    #[derive(Default)]
    struct Recorder {
        /// `Button` events, as (id, pressed).
        seen: Vec<(u16, bool)>,
        /// `Relative` events, as (id, delta).
        relative: Vec<(u16, f32)>,
    }

    impl InputConfigurable for Recorder {
        fn input_controls(&self) -> &'static [InputControl] {
            &[]
        }
        fn handle_input(&mut self, event: InputEvent) {
            match event {
                InputEvent::Button { id, pressed } => self.seen.push((id.0, pressed)),
                InputEvent::Relative { id, delta } => self.relative.push((id.0, delta)),
                InputEvent::Absolute { .. } => {}
            }
        }
    }

    /// Device state driven entirely by the test.
    #[derive(Default)]
    struct FakeDevices {
        keys: Vec<Scancode>,
        pad_buttons: Vec<Button>,
        axes: Vec<(Axis, f32)>,
        mouse_buttons: Vec<MouseButton>,
    }

    impl DeviceState for FakeDevices {
        fn key_pressed(&self, scancode: Scancode) -> bool {
            self.keys.contains(&scancode)
        }
        fn pad_button_pressed(&self, button: Button) -> bool {
            self.pad_buttons.contains(&button)
        }
        fn pad_axis(&self, axis: Axis) -> f32 {
            self.axes
                .iter()
                .find(|(a, _)| *a == axis)
                .map_or(0.0, |(_, v)| *v)
        }
        fn mouse_button_pressed(&self, button: MouseButton) -> bool {
            self.mouse_buttons.contains(&button)
        }
    }

    fn binding(physical: PhysicalInput, target: u16) -> InputBinding {
        InputBinding {
            physical,
            target: InputId(target),
            scale: 1.0,
            deadzone: STICK_DEADZONE_NORM,
        }
    }

    fn set(bindings: Vec<InputBinding>) -> BindingSet {
        BindingSet { bindings }
    }

    fn ctx(egui_wants_keyboard: bool, mouse_grabbed: bool) -> DispatchCtx {
        DispatchCtx {
            egui_wants_keyboard,
            mouse_grabbed,
        }
    }

    fn key_down(sc: Scancode) -> Event {
        Event::KeyDown {
            timestamp: 0,
            window_id: 0,
            keycode: None,
            scancode: Some(sc),
            keymod: sdl2::keyboard::Mod::empty(),
            repeat: false,
        }
    }

    fn key_up(sc: Scancode) -> Event {
        Event::KeyUp {
            timestamp: 0,
            window_id: 0,
            keycode: None,
            scancode: Some(sc),
            keymod: sdl2::keyboard::Mod::empty(),
            repeat: false,
        }
    }

    fn mouse_up(btn: MouseButton) -> Event {
        Event::MouseButtonUp {
            timestamp: 0,
            window_id: 0,
            which: 0,
            mouse_btn: btn,
            clicks: 1,
            x: 0,
            y: 0,
        }
    }

    #[test]
    fn key_press_is_suppressed_while_egui_wants_the_keyboard() {
        let bindings = set(vec![binding(PhysicalInput::Key(Scancode::Left), 1)]);
        let mut rec = Recorder::default();

        let consumed = dispatch(
            &key_down(Scancode::Left),
            &bindings,
            &mut rec,
            ctx(true, false),
        );
        assert!(!consumed);
        assert!(rec.seen.is_empty());
    }

    #[test]
    fn key_release_dispatches_even_while_egui_wants_the_keyboard() {
        // The asymmetry is deliberate: egui can grab focus *while* a game key is
        // held (held arrows move its widget focus), and a guarded release would
        // strand the button down.
        let bindings = set(vec![binding(PhysicalInput::Key(Scancode::Left), 1)]);
        let mut rec = Recorder::default();

        let consumed = dispatch(
            &key_up(Scancode::Left),
            &bindings,
            &mut rec,
            ctx(true, false),
        );
        assert!(consumed);
        assert_eq!(rec.seen, vec![(1, false)]);
    }

    #[test]
    fn mouse_release_dispatches_regardless_of_grab() {
        // Regression pin: this arm used to be gated on `mouse_grabbed`, so
        // pressing mouse-fire then hitting F11 to ungrab stranded fire "on".
        let bindings = set(vec![binding(
            PhysicalInput::MouseButtonInput(MouseButton::Left),
            1,
        )]);

        for grabbed in [true, false] {
            let mut rec = Recorder::default();
            let consumed = dispatch(
                &mouse_up(MouseButton::Left),
                &bindings,
                &mut rec,
                ctx(false, grabbed),
            );
            assert!(consumed, "grabbed={grabbed}");
            assert_eq!(rec.seen, vec![(1, false)], "grabbed={grabbed}");
        }
    }

    #[test]
    fn pad_axis_crossing_the_deadzone_presses_only_that_direction() {
        let bindings = set(vec![
            binding(PhysicalInput::PadAxis(Axis::LeftX, AxisDir::Negative), 1),
            binding(PhysicalInput::PadAxis(Axis::LeftX, AxisDir::Positive), 2),
        ]);
        let event = |value| Event::ControllerAxisMotion {
            timestamp: 0,
            which: 0,
            axis: Axis::LeftX,
            value,
        };

        let mut rec = Recorder::default();
        dispatch(&event(-30_000), &bindings, &mut rec, ctx(false, false));
        assert_eq!(rec.seen, vec![(1, true), (2, false)]);

        // Inside the deadzone both read released.
        let mut rec = Recorder::default();
        dispatch(&event(-1_000), &bindings, &mut rec, ctx(false, false));
        assert_eq!(rec.seen, vec![(1, false), (2, false)]);
    }

    #[test]
    fn mouse_motion_applies_scale_and_needs_the_grab() {
        let mut bindings = set(vec![binding(PhysicalInput::MouseAxis(MouseAxis::X), 1)]);
        bindings.bindings[0].scale = 2.5;
        let event = Event::MouseMotion {
            timestamp: 0,
            window_id: 0,
            which: 0,
            mousestate: sdl2::mouse::MouseState::from_sdl_state(0),
            x: 0,
            y: 0,
            xrel: 4,
            yrel: 0,
        };

        let mut rec = Recorder::default();
        let consumed = dispatch(&event, &bindings, &mut rec, ctx(false, true));
        assert!(consumed);
        assert_eq!(rec.relative, vec![(1, 10.0)]);

        // Ungrabbed, the cursor belongs to the UI and motion is not game input.
        let mut rec = Recorder::default();
        let consumed = dispatch(&event, &bindings, &mut rec, ctx(false, false));
        assert!(!consumed);
        assert!(rec.relative.is_empty());
    }

    #[test]
    fn unrelated_events_are_not_consumed() {
        let bindings = set(vec![binding(PhysicalInput::Key(Scancode::Left), 1)]);
        let mut rec = Recorder::default();
        let quit = Event::Quit { timestamp: 0 };
        assert!(!dispatch(&quit, &bindings, &mut rec, ctx(false, true)));
        assert!(rec.seen.is_empty());
    }

    #[test]
    fn resync_drives_each_binding_to_its_live_value() {
        let bindings = set(vec![
            binding(PhysicalInput::Key(Scancode::Left), 1),
            binding(PhysicalInput::Key(Scancode::Right), 2),
            binding(PhysicalInput::PadButton(Button::A), 3),
            binding(PhysicalInput::MouseButtonInput(MouseButton::Left), 4),
        ]);
        let devices = FakeDevices {
            keys: vec![Scancode::Left],
            pad_buttons: vec![Button::A],
            ..Default::default()
        };

        let mut rec = Recorder::default();
        resync(&bindings, &mut rec, &devices);

        // Held inputs are re-asserted as pressed, everything else as released —
        // that is the difference from a plain release-all.
        assert_eq!(rec.seen, vec![(1, true), (2, false), (3, true), (4, false)]);
    }

    #[test]
    fn resync_skips_mouse_axes() {
        let bindings = set(vec![
            binding(PhysicalInput::MouseAxis(MouseAxis::X), 1),
            binding(PhysicalInput::Key(Scancode::Left), 2),
        ]);

        let mut rec = Recorder::default();
        resync(&bindings, &mut rec, &FakeDevices::default());

        // Relative motion has no current value to re-assert; sending a release
        // would be meaningless for a trackball axis.
        assert_eq!(rec.seen, vec![(2, false)]);
    }

    #[test]
    fn resync_applies_the_deadzone_per_direction() {
        let bindings = set(vec![
            binding(PhysicalInput::PadAxis(Axis::LeftX, AxisDir::Negative), 1),
            binding(PhysicalInput::PadAxis(Axis::LeftX, AxisDir::Positive), 2),
        ]);

        // Deflected hard negative: only the negative binding is pressed.
        let mut rec = Recorder::default();
        let devices = FakeDevices {
            axes: vec![(Axis::LeftX, -0.9)],
            ..Default::default()
        };
        resync(&bindings, &mut rec, &devices);
        assert_eq!(rec.seen, vec![(1, true), (2, false)]);

        // Resting inside the deadzone: neither.
        let mut rec = Recorder::default();
        let devices = FakeDevices {
            axes: vec![(Axis::LeftX, -0.1)],
            ..Default::default()
        };
        resync(&bindings, &mut rec, &devices);
        assert_eq!(rec.seen, vec![(1, false), (2, false)]);
    }

    #[test]
    fn resync_releases_mouse_buttons_when_ungrabbed() {
        // The ungrabbed case is modeled by the device state reporting nothing
        // pressed, which is what `SdlDevices` does with `mouse: None`. This is
        // the F11-while-firing bug: the release has to come from somewhere.
        let bindings = set(vec![binding(
            PhysicalInput::MouseButtonInput(MouseButton::Left),
            1,
        )]);

        let mut rec = Recorder::default();
        resync(&bindings, &mut rec, &FakeDevices::default());
        assert_eq!(rec.seen, vec![(1, false)]);
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

    #[test]
    fn action_role_resolves_to_role_defaults_plus_extras() {
        // A Primary P1 action with a machine-specific extra (a trackball
        // cabinet's left mouse button, like Gridlee fire).
        const CONTROLS: &[InputControl] = &[InputControl {
            id: InputId(7),
            stable_name: "p1_fire",
            label: "P1 Fire",
            kind: InputKind::Action(ActionRole::Primary),
            player: Some(1),
            default_bindings: &[DefaultBinding::Mouse(MouseControl::Left)],
        }];
        let set = BindingSet::from_controls(CONTROLS);

        // Role defaults: LShift + gamepad A.
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::Key(Scancode::LShift))),
            vec![7]
        );
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::PadButton(Button::A))),
            vec![7]
        );
        // Primary moves Fire off Space — the legacy default must be gone.
        assert!(ids(set.digital_targets(PhysicalInput::Key(Scancode::Space))).is_empty());
        // The machine-specific extra unions in on top of the role defaults.
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::MouseButtonInput(MouseButton::Left))),
            vec![7]
        );
    }

    #[test]
    fn action_role_player_two_is_keyboard_only() {
        // Player 2 shares the keyboard but not the pad: Primary → RShift, no pad.
        const CONTROLS: &[InputControl] = &[InputControl {
            id: InputId(8),
            stable_name: "p2_fire",
            label: "P2 Fire",
            kind: InputKind::Action(ActionRole::Primary),
            player: Some(2),
            default_bindings: &[],
        }];
        let set = BindingSet::from_controls(CONTROLS);
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::Key(Scancode::RShift))),
            vec![8]
        );
        assert!(ids(set.digital_targets(PhysicalInput::PadButton(Button::A))).is_empty());
    }
}
