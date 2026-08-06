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

/// Analog sensitivity multiplier when the user has not chosen one.
const DEFAULT_SCALE: f32 = 1.0;

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
    /// A whole gamepad axis driving an analog control, deflection preserved
    /// (distinct from `PadAxis`, which thresholds one direction into a button).
    PadFullAxis(Axis),
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
            PhysicalInput::PadFullAxis(axis) => format!("padfullaxis:{}", axis.string()),
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
            PhysicalInput::PadFullAxis(axis) => format!("Pad {axis:?}"),
            PhysicalInput::MouseButtonInput(mb) => format!("Mouse {mb:?}"),
            PhysicalInput::MouseAxis(axis) => format!("Mouse {axis:?}"),
        }
    }

    /// Category used when rebinding: replacing a control's keyboard binding
    /// leaves its gamepad/mouse bindings intact, and vice versa.
    fn category(&self) -> PhysicalCategory {
        match self {
            PhysicalInput::Key(_) => PhysicalCategory::Keyboard,
            PhysicalInput::PadButton(_)
            | PhysicalInput::PadAxis(..)
            | PhysicalInput::PadFullAxis(_) => PhysicalCategory::Pad,
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
            "padfullaxis" => Axis::from_string(rest).map(PhysicalInput::PadFullAxis),
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SerializedBinding {
    pub control: String,
    pub input: String,
    /// Analog sensitivity, omitted when it is the 1.0 default. `Option` plus
    /// `serde(default)` so existing `state.toml` files load unchanged and
    /// untouched bindings stay a two-key table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    /// Axis deadzone, omitted when it is the built-in default for the input's
    /// kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadzone: Option<f32>,
}

/// One physical input bound to one logical control.
#[derive(Clone, Copy)]
pub struct InputBinding {
    pub physical: PhysicalInput,
    pub target: InputId,
    /// Which pad slot may drive this binding, for gamepad inputs only.
    ///
    /// `None` means any pad (and is always the case for keyboard and mouse
    /// bindings, which have no slot). `Some(n)` restricts the binding to the
    /// n-th connected pad, so two plugged-in controllers drive their own
    /// players instead of both driving player 1.
    pub player_slot: Option<u8>,
    /// Multiplier applied to analog motion (relative axes). Unused for digital.
    pub scale: f32,
    /// Normalized deflection (0..1) past which a gamepad axis counts as pressed.
    pub deadzone: f32,
}

/// Rescale a raw axis deflection (`-1.0..=1.0`) from the deadzone edge and
/// apply `scale`, so the value ramps from 0.0 as the stick leaves the deadzone
/// instead of jumping to the deadzone fraction the moment it is crossed.
fn analog_value(raw: f32, deadzone: f32, scale: f32) -> f32 {
    let magnitude = ((raw.abs() - deadzone) / (1.0 - deadzone)).clamp(0.0, 1.0) * scale;
    magnitude.copysign(raw)
}

/// The deadzone a physical input gets when the user has not chosen one.
/// Gamepad axes rest noisily around center; nothing else needs a deadzone.
fn default_deadzone(physical: PhysicalInput) -> f32 {
    match physical {
        PhysicalInput::PadAxis(..) | PhysicalInput::PadFullAxis(_) => STICK_DEADZONE_NORM,
        _ => 0.0,
    }
}

/// Build a binding with default sensitivity and deadzone.
fn make_binding(physical: PhysicalInput, target: InputId, player: Option<u8>) -> InputBinding {
    // Only pad bindings are slot-scoped: a keyboard binding for player 2 is
    // still just a key, and constraining it would make it unreachable.
    let player_slot = match physical {
        PhysicalInput::PadButton(_)
        | PhysicalInput::PadAxis(..)
        | PhysicalInput::PadFullAxis(_) => player,
        _ => None,
    };
    InputBinding {
        physical,
        target,
        player_slot,
        scale: DEFAULT_SCALE,
        deadzone: default_deadzone(physical),
    }
}

/// All physical→logical bindings active for the current machine.
///
/// Lookups are linear scans; binding counts are tiny (tens at most), so this is
/// cheaper and simpler than indexing, and supports multiple bindings per input.
pub struct BindingSet {
    bindings: Vec<InputBinding>,
}

/// Whether an event originating from `slot` may drive `binding`.
///
/// `slot` is `None` for "any slot" — keyboard and mouse events, and callers
/// that want every binding regardless of routing (the settings UI). A binding
/// with no `player_slot` is likewise unrestricted.
fn slot_matches(binding: &InputBinding, slot: Option<u8>) -> bool {
    match (slot, binding.player_slot) {
        (None, _) | (_, None) => true,
        (Some(from), Some(want)) => from == want,
    }
}

impl BindingSet {
    fn new() -> Self {
        Self {
            bindings: Vec::new(),
        }
    }

    /// Logical controls bound to an exact digital physical input, restricted to
    /// those a `slot` event may drive (`None` for any).
    pub fn digital_targets(
        &self,
        physical: PhysicalInput,
        slot: Option<u8>,
    ) -> impl Iterator<Item = InputId> + '_ {
        self.bindings
            .iter()
            .filter(move |b| b.physical == physical && slot_matches(b, slot))
            .map(|b| b.target)
    }

    /// Gamepad-axis targets for `axis`, as (control, direction, deadzone).
    pub fn pad_axis_targets(
        &self,
        axis: Axis,
        slot: Option<u8>,
    ) -> impl Iterator<Item = (InputId, AxisDir, f32)> + '_ {
        self.bindings.iter().filter_map(move |b| match b.physical {
            PhysicalInput::PadAxis(a, dir) if a == axis && slot_matches(b, slot) => {
                Some((b.target, dir, b.deadzone))
            }
            _ => None,
        })
    }

    /// Full-axis analog targets for `axis`, as (control, scale, deadzone).
    ///
    /// Distinct from [`pad_axis_targets`](Self::pad_axis_targets): those
    /// threshold one signed direction into a button, these want the axis's
    /// continuous deflection.
    pub fn pad_analog_targets(
        &self,
        axis: Axis,
        slot: Option<u8>,
    ) -> impl Iterator<Item = (InputId, f32, f32)> + '_ {
        self.bindings.iter().filter_map(move |b| match b.physical {
            PhysicalInput::PadFullAxis(a) if a == axis && slot_matches(b, slot) => {
                Some((b.target, b.scale, b.deadzone))
            }
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
                set.bindings
                    .push(make_binding(physical, control.id, control.player));
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
                    // Diff-only, matching what `MachineSettings::is_empty`
                    // enforces for the file as a whole.
                    scale: (b.scale != DEFAULT_SCALE).then_some(b.scale),
                    deadzone: (b.deadzone != default_deadzone(b.physical)).then_some(b.deadzone),
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
                let player = controls
                    .iter()
                    .find(|c| c.id == target)
                    .and_then(|c| c.player);
                let mut binding = make_binding(physical, target, player);
                binding.scale = s.scale.unwrap_or(DEFAULT_SCALE);
                binding.deadzone = s.deadzone.unwrap_or_else(|| default_deadzone(physical));
                self.bindings.push(binding);
            }
        }
    }

    /// Every physical input bound to anything, for host-hotkey conflict checks.
    pub fn all_physical(&self) -> impl Iterator<Item = PhysicalInput> + '_ {
        self.bindings.iter().map(|b| b.physical)
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
    pub fn rebind(&mut self, controls: &[InputControl], target: InputId, physical: PhysicalInput) {
        let category = physical.category();
        self.bindings
            .retain(|b| b.target != target || b.physical.category() != category);
        let player = controls
            .iter()
            .find(|c| c.id == target)
            .and_then(|c| c.player);
        self.bindings.push(make_binding(physical, target, player));
    }

    /// Set the analog sensitivity of every binding driving `target`.
    pub fn set_scale(&mut self, target: InputId, scale: f32) {
        for b in self.bindings.iter_mut().filter(|b| b.target == target) {
            b.scale = scale;
        }
    }

    /// Set the deadzone of every gamepad-axis binding driving `target`.
    ///
    /// Scoped to axis bindings because nothing else has a deadzone — applying
    /// it to a key would make the stored value diverge from its default and be
    /// persisted for no reason.
    pub fn set_deadzone(&mut self, target: InputId, deadzone: f32) {
        for b in self.bindings.iter_mut().filter(|b| {
            b.target == target
                && matches!(
                    b.physical,
                    PhysicalInput::PadAxis(..) | PhysicalInput::PadFullAxis(_)
                )
        }) {
            b.deadzone = deadzone;
        }
    }

    /// Current sensitivity for `target`, or the default when unbound.
    pub fn scale_of(&self, target: InputId) -> f32 {
        self.bindings
            .iter()
            .find(|b| b.target == target)
            .map_or(DEFAULT_SCALE, |b| b.scale)
    }

    /// Current deadzone for `target`'s axis bindings, if it has any.
    pub fn deadzone_of(&self, target: InputId) -> Option<f32> {
        self.bindings
            .iter()
            .find(|b| {
                b.target == target
                    && matches!(
                        b.physical,
                        PhysicalInput::PadAxis(..) | PhysicalInput::PadFullAxis(_)
                    )
            })
            .map(|b| b.deadzone)
    }
}

/// Order-insensitive equality of two serialized binding lists, used to decide
/// whether a machine's bindings differ from its defaults (and thus need saving).
pub fn bindings_eq(a: &[SerializedBinding], b: &[SerializedBinding]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    // Sorting by (control, input) alone would let a sensitivity-only change
    // compare equal to the defaults and never be written; the tuning is part of
    // the identity here.
    let key = |s: &SerializedBinding| {
        (
            s.control.clone(),
            s.input.clone(),
            s.scale.map(f32::to_bits),
            s.deadzone.map(f32::to_bits),
        )
    };
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

/// What [`dispatch`] remembers between events.
///
/// Gamepad axes stream `ControllerAxisMotion` continuously — a worn stick that
/// rests a little off-center emits events forever without ever leaving the
/// deadzone. Re-deriving a control's state from each of those events and
/// dispatching it unconditionally means the axis re-asserts "released" many
/// times a second, silently overriding a key the player is holding for the
/// same control (every direction is bound to both a key and a stick axis by
/// default). Latching the last value and dispatching only on a *change* keeps
/// a resting axis silent.
#[derive(Default)]
pub struct DispatchState {
    /// Last digital state sent for a (axis, direction, target) triple.
    pad_axis: Vec<((Axis, AxisDir, InputId), bool)>,
    /// Last analog value sent for an (axis, target) pair.
    pad_analog: Vec<((Axis, InputId), f32)>,
}

impl DispatchState {
    /// Record `pressed` for this axis direction, returning true if it changed.
    fn digital_changed(&mut self, key: (Axis, AxisDir, InputId), pressed: bool) -> bool {
        match self.pad_axis.iter_mut().find(|(k, _)| *k == key) {
            Some((_, last)) => std::mem::replace(last, pressed) != pressed,
            // An axis first seen at rest has nothing to announce; only a
            // deflection is news.
            None => {
                self.pad_axis.push((key, pressed));
                pressed
            }
        }
    }

    /// Record `value` for this analog axis, returning true if it changed.
    fn analog_changed(&mut self, key: (Axis, InputId), value: f32) -> bool {
        match self.pad_analog.iter_mut().find(|(k, _)| *k == key) {
            Some((_, last)) => std::mem::replace(last, value) != value,
            None => {
                self.pad_analog.push((key, value));
                value != 0.0
            }
        }
    }
}

/// Everything [`dispatch`] needs from the frontend besides the bindings.
#[derive(Clone, Copy)]
pub struct DispatchCtx {
    /// egui holds keyboard focus. Suppresses key *presses* but never releases.
    pub egui_wants_keyboard: bool,
    /// The cursor is captured for the game rather than the UI.
    pub mouse_grabbed: bool,
    /// Slot of the pad that produced this event, when it came from one.
    ///
    /// Resolved by the caller from the SDL instance id, because slot
    /// assignment is connection-order bookkeeping the dispatch layer does not
    /// own. `None` for keyboard and mouse events, which have no slot.
    pub pad_slot: Option<u8>,
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
    state: &mut DispatchState,
) -> bool {
    let mut press = |physical, pressed| {
        for id in bindings.digital_targets(physical, ctx.pad_slot) {
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

        // One axis can drive both digital and analog targets, so both loops run.
        Event::ControllerAxisMotion { axis, value, .. } => {
            let normalized = f32::from(*value) / 32_768.0;

            // Analog stick standing in for digital directions. Only a change
            // is dispatched — see `DispatchState`.
            for (id, dir, deadzone) in bindings.pad_axis_targets(*axis, ctx.pad_slot) {
                let pressed = match dir {
                    AxisDir::Positive => normalized > deadzone,
                    AxisDir::Negative => normalized < -deadzone,
                };
                if state.digital_changed((*axis, dir, id), pressed) {
                    machine.handle_input(InputEvent::Button { id, pressed });
                }
            }

            // Whole axis driving an analog control. The magnitude is rescaled
            // from the deadzone edge rather than passed through, so the value
            // ramps from 0.0 as the stick leaves the deadzone instead of
            // jumping to the deadzone fraction the moment it is crossed.
            for (id, scale, deadzone) in bindings.pad_analog_targets(*axis, ctx.pad_slot) {
                let value = analog_value(normalized, deadzone, scale);
                // Same latch, for the same reason: a resting stick would
                // otherwise pin the axis at 0.0 forever, and on machines where
                // the mouse drives the same control (starwars, irobot) that
                // would fight every mouse motion.
                if state.analog_changed((*axis, id), value) {
                    machine.handle_input(InputEvent::Absolute { id, value });
                }
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
    fn pad_button_pressed(&self, button: Button, slot: Option<u8>) -> bool;
    /// Deflection of `axis` on whichever pad is pushing it hardest, normalized
    /// to `-1.0..=1.0`.
    fn pad_axis(&self, axis: Axis, slot: Option<u8>) -> f32;
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
    // A control usually has several bindings — a key, a D-pad button and a
    // stick direction all drive `p1_left`. Its held state is the OR across
    // them, so they must be combined before dispatching. Emitting one event
    // per binding lets an unpressed pad button overwrite a genuinely held key,
    // which is exactly the desync resync exists to repair.
    let mut digital: Vec<(InputId, bool)> = Vec::new();

    for binding in bindings.all() {
        let pressed = match binding.physical {
            PhysicalInput::Key(sc) => devices.key_pressed(sc),
            PhysicalInput::PadButton(b) => devices.pad_button_pressed(b, binding.player_slot),
            PhysicalInput::PadAxis(axis, dir) => {
                let deflection = devices.pad_axis(axis, binding.player_slot);
                match dir {
                    AxisDir::Positive => deflection > binding.deadzone,
                    AxisDir::Negative => deflection < -binding.deadzone,
                }
            }
            PhysicalInput::MouseButtonInput(mb) => devices.mouse_button_pressed(mb),
            // A stick held off-center across a reset or state load should stay
            // deflected, so this re-asserts a value rather than a press.
            PhysicalInput::PadFullAxis(axis) => {
                let raw = devices.pad_axis(axis, binding.player_slot);
                machine.handle_input(InputEvent::Absolute {
                    id: binding.target,
                    value: analog_value(raw, binding.deadzone, binding.scale),
                });
                continue;
            }
            // Relative motion has no "current value" to re-assert.
            PhysicalInput::MouseAxis(_) => continue,
        };

        match digital.iter_mut().find(|(id, _)| *id == binding.target) {
            Some((_, held)) => *held |= pressed,
            None => digital.push((binding.target, pressed)),
        }
    }

    for (id, pressed) in digital {
        machine.handle_input(InputEvent::Button { id, pressed });
    }
}

// ---------------------------------------------------------------------------
// Core descriptor → SDL translation
// ---------------------------------------------------------------------------

fn pad_to_physical(pad: PadControl) -> PhysicalInput {
    match pad {
        PadControl::Button(button) => PhysicalInput::PadButton(pad_button(button)),
        PadControl::FullAxis(axis) => PhysicalInput::PadFullAxis(pad_axis(axis)),
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
        /// `Absolute` events, as (id, value).
        absolute: Vec<(u16, f32)>,
    }

    impl InputConfigurable for Recorder {
        fn input_controls(&self) -> &'static [InputControl] {
            &[]
        }
        fn handle_input(&mut self, event: InputEvent) {
            match event {
                InputEvent::Button { id, pressed } => self.seen.push((id.0, pressed)),
                InputEvent::Relative { id, delta } => self.relative.push((id.0, delta)),
                InputEvent::Absolute { id, value } => self.absolute.push((id.0, value)),
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
        fn pad_button_pressed(&self, button: Button, _slot: Option<u8>) -> bool {
            self.pad_buttons.contains(&button)
        }
        fn pad_axis(&self, axis: Axis, _slot: Option<u8>) -> f32 {
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
            player_slot: None,
            scale: 1.0,
            deadzone: STICK_DEADZONE_NORM,
        }
    }

    fn set(bindings: Vec<InputBinding>) -> BindingSet {
        BindingSet { bindings }
    }

    fn ctx(egui_wants_keyboard: bool, mouse_grabbed: bool, pad_slot: Option<u8>) -> DispatchCtx {
        DispatchCtx {
            egui_wants_keyboard,
            mouse_grabbed,
            pad_slot,
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
            ctx(true, false, None),
            &mut DispatchState::default(),
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
            ctx(true, false, None),
            &mut DispatchState::default(),
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
                ctx(false, grabbed, None),
                &mut DispatchState::default(),
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
        dispatch(
            &event(-30_000),
            &bindings,
            &mut rec,
            ctx(false, false, None),
            &mut DispatchState::default(),
        );
        // Only the crossed direction is announced. The opposite direction was
        // already at rest, and re-sending its "released" state on every motion
        // event is what used to stomp on a held key.
        assert_eq!(rec.seen, vec![(1, true)]);

        // Inside the deadzone nothing is dispatched at all.
        let mut rec = Recorder::default();
        dispatch(
            &event(-1_000),
            &bindings,
            &mut rec,
            ctx(false, false, None),
            &mut DispatchState::default(),
        );
        assert_eq!(rec.seen, vec![]);
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
        let consumed = dispatch(
            &event,
            &bindings,
            &mut rec,
            ctx(false, true, None),
            &mut DispatchState::default(),
        );
        assert!(consumed);
        assert_eq!(rec.relative, vec![(1, 10.0)]);

        // Ungrabbed, the cursor belongs to the UI and motion is not game input.
        let mut rec = Recorder::default();
        let consumed = dispatch(
            &event,
            &bindings,
            &mut rec,
            ctx(false, false, None),
            &mut DispatchState::default(),
        );
        assert!(!consumed);
        assert!(rec.relative.is_empty());
    }

    #[test]
    fn pad_full_axis_produces_absolute_ramping_from_the_deadzone_edge() {
        let bindings = set(vec![binding(PhysicalInput::PadFullAxis(Axis::LeftX), 1)]);
        let event = |value| Event::ControllerAxisMotion {
            timestamp: 0,
            which: 0,
            axis: Axis::LeftX,
            value,
        };

        // Exactly at the deadzone edge the value is 0.0, not the deadzone
        // fraction — that ramp is the point of the rescale. 0.0 is also
        // indistinguishable from rest, so the latch stays silent; stepping past
        // the edge is what produces the first event.
        let mut rec = Recorder::default();
        let mut state = DispatchState::default();
        let edge = (STICK_DEADZONE_NORM * 32_768.0) as i16;
        dispatch(
            &event(edge),
            &bindings,
            &mut rec,
            ctx(false, false, None),
            &mut state,
        );
        assert_eq!(rec.absolute, vec![]);

        dispatch(
            &event(edge + 4_000),
            &bindings,
            &mut rec,
            ctx(false, false, None),
            &mut state,
        );
        assert_eq!(rec.absolute.len(), 1);
        assert!(rec.absolute[0].1 > 0.0, "{:?}", rec.absolute);

        // Fully deflected reaches 1.0, and the sign follows the stick.
        let mut rec = Recorder::default();
        dispatch(
            &event(-32_768),
            &bindings,
            &mut rec,
            ctx(false, false, None),
            &mut DispatchState::default(),
        );
        assert_eq!(rec.absolute.len(), 1);
        assert!((rec.absolute[0].1 + 1.0).abs() < 1e-6, "{:?}", rec.absolute);

        // Inside the deadzone it stays at rest, and says nothing — a drifting
        // stick must not keep pinning the axis to 0.0 over the mouse.
        let mut rec = Recorder::default();
        dispatch(
            &event(1_000),
            &bindings,
            &mut rec,
            ctx(false, false, None),
            &mut DispatchState::default(),
        );
        assert_eq!(rec.absolute, vec![]);
    }

    #[test]
    fn one_axis_drives_digital_and_analog_targets_together() {
        // A pad axis may legitimately be bound to both a digital direction and
        // an analog control; neither loop may shadow the other.
        let bindings = set(vec![
            binding(PhysicalInput::PadAxis(Axis::LeftX, AxisDir::Negative), 1),
            binding(PhysicalInput::PadFullAxis(Axis::LeftX), 2),
        ]);

        let mut rec = Recorder::default();
        dispatch(
            &Event::ControllerAxisMotion {
                timestamp: 0,
                which: 0,
                axis: Axis::LeftX,
                value: -32_768,
            },
            &bindings,
            &mut rec,
            ctx(false, false, None),
            &mut DispatchState::default(),
        );
        assert_eq!(rec.seen, vec![(1, true)]);
        assert_eq!(rec.absolute.len(), 1);
        assert_eq!(rec.absolute[0].0, 2);
    }

    #[test]
    fn analog_value_scales_and_clamps() {
        // Deadzone 0.5: half deflection is the edge, three-quarters is halfway.
        assert!((analog_value(0.5, 0.5, 1.0) - 0.0).abs() < 1e-6);
        assert!((analog_value(0.75, 0.5, 1.0) - 0.5).abs() < 1e-6);
        assert!((analog_value(1.0, 0.5, 1.0) - 1.0).abs() < 1e-6);
        // Scale multiplies the ramped magnitude, sign is preserved.
        assert!((analog_value(-1.0, 0.5, 2.0) + 2.0).abs() < 1e-6);
        // Beyond full deflection the magnitude clamps before scaling.
        assert!((analog_value(2.0, 0.0, 1.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn full_axis_defaults_reach_only_the_analog_lookup() {
        let controls = &[
            InputControl {
                id: InputId(1),
                stable_name: "fire",
                label: "Fire",
                // Plain Button, not Action: an Action would also pull bindings
                // from the shared role ladder and clutter the assertion.
                kind: InputKind::Button,
                player: Some(1),
                default_bindings: &[DefaultBinding::Pad(PadControl::Button(
                    phosphor_core::core::machine::PadButton::A,
                ))],
            },
            InputControl {
                id: InputId(2),
                stable_name: "yoke_x",
                label: "Yoke X",
                kind: InputKind::AnalogAxis {
                    axis: phosphor_core::core::machine::AnalogAxisKind::X,
                },
                player: Some(1),
                default_bindings: &[DefaultBinding::Pad(PadControl::FullAxis(
                    CorePadAxis::LeftX,
                ))],
            },
        ];

        let set = BindingSet::from_controls(controls);

        // The full-axis binding is reachable through the analog lookup only.
        assert_eq!(
            ids(set.pad_axis_targets(Axis::LeftX, None).map(|(id, ..)| id)),
            []
        );
        assert_eq!(
            ids(set.pad_analog_targets(Axis::LeftX, None).map(|(id, ..)| id)),
            [2]
        );
    }

    /// Every registered machine's defaults must survive being written to
    /// `state.toml` and read back.
    ///
    /// This is the persistence contract for real control tables rather than
    /// hand-built ones: a physical input whose token does not round-trip would
    /// silently drop that binding on the next launch, and the user would find
    /// a control dead with nothing logged. Reachable without ROMs because
    /// `MachineEntry` carries the control table.
    #[test]
    fn every_machine_default_binding_survives_a_state_toml_round_trip() {
        let entries = phosphor_machines::registry::all();
        assert!(
            entries.len() > 20,
            "registry looks empty; test would be vacuous"
        );
        for entry in entries {
            let defaults = BindingSet::from_controls(entry.controls);
            let serialized = defaults.to_serialized(entry.controls);

            let mut restored = BindingSet::from_controls(entry.controls);
            restored.apply_overrides(entry.controls, &serialized);

            assert!(
                bindings_eq(&serialized, &restored.to_serialized(entry.controls)),
                "{}: bindings changed across a serialize/restore cycle",
                entry.name
            );
        }
    }

    /// Every default binding's token must survive `to_token` → `from_token`.
    ///
    /// `apply_overrides` silently skips tokens it cannot parse (deliberately —
    /// a stale binding should not fail the load), so a broken token would not
    /// show up as an error anywhere. This catches it at the source.
    #[test]
    fn every_machine_default_binding_token_parses_back() {
        for entry in phosphor_machines::registry::all() {
            for binding in BindingSet::from_controls(entry.controls).all() {
                let token = binding.physical.to_token();
                assert_eq!(
                    PhysicalInput::from_token(&token),
                    Some(binding.physical),
                    "{}: token '{token}' does not parse back to itself",
                    entry.name
                );
            }
        }
    }

    /// A stick resting inside its deadzone must not override a held key.
    ///
    /// Regression: gamepad axes stream motion events continuously, and each one
    /// re-derived every bound control's state and dispatched it. A worn stick
    /// resting slightly off-center therefore re-sent "released" many times a
    /// second for `p1_left`/`p1_right`, cancelling the arrow keys — every game
    /// lost left/right while up/down kept working, because only the X axis was
    /// drifting. Reported from the field.
    #[test]
    fn a_resting_pad_axis_does_not_cancel_held_keys() {
        let bindings = set(vec![
            binding(PhysicalInput::Key(Scancode::Left), 1),
            binding(PhysicalInput::PadAxis(Axis::LeftX, AxisDir::Negative), 1),
            binding(PhysicalInput::PadAxis(Axis::LeftX, AxisDir::Positive), 2),
        ]);
        let mut state = DispatchState::default();
        let mut rec = Recorder::default();

        // Key down: p1_left is held.
        dispatch(
            &key_down(Scancode::Left),
            &bindings,
            &mut rec,
            ctx(false, false, None),
            &mut state,
        );
        assert_eq!(rec.seen, vec![(1, true)]);

        // Now the stick jitters around center, well inside the deadzone.
        for value in [80, -140, 200, -60, 15] {
            dispatch(
                &Event::ControllerAxisMotion {
                    timestamp: 0,
                    which: 0,
                    axis: Axis::LeftX,
                    value,
                },
                &bindings,
                &mut rec,
                ctx(false, false, None),
                &mut state,
            );
        }

        // Nothing further was dispatched — the key is still held.
        assert_eq!(
            rec.seen,
            vec![(1, true)],
            "a resting axis dispatched over the held key"
        );
    }

    /// The latch must not swallow a genuine deflection or its release.
    #[test]
    fn pad_axis_still_reports_crossings_in_both_directions() {
        let bindings = set(vec![binding(
            PhysicalInput::PadAxis(Axis::LeftX, AxisDir::Negative),
            1,
        )]);
        let mut state = DispatchState::default();
        let mut rec = Recorder::default();
        let event = |value| Event::ControllerAxisMotion {
            timestamp: 0,
            which: 0,
            axis: Axis::LeftX,
            value,
        };

        dispatch(
            &event(-30_000),
            &bindings,
            &mut rec,
            ctx(false, false, None),
            &mut state,
        );
        // Still deflected — no repeat.
        dispatch(
            &event(-31_000),
            &bindings,
            &mut rec,
            ctx(false, false, None),
            &mut state,
        );
        // Back to center — one release.
        dispatch(
            &event(0),
            &bindings,
            &mut rec,
            ctx(false, false, None),
            &mut state,
        );
        dispatch(
            &event(10),
            &bindings,
            &mut rec,
            ctx(false, false, None),
            &mut state,
        );

        assert_eq!(rec.seen, vec![(1, true), (1, false)]);
    }

    #[test]
    fn tuning_is_omitted_at_defaults_and_round_trips_when_changed() {
        let controls = &[InputControl {
            id: InputId(1),
            stable_name: "ball_x",
            label: "Trackball X",
            kind: InputKind::AnalogAxis {
                axis: phosphor_core::core::machine::AnalogAxisKind::X,
            },
            player: Some(1),
            default_bindings: &[DefaultBinding::Mouse(MouseControl::AxisX)],
        }];

        // At defaults the table stays two keys, so an untouched state.toml is
        // not churned by this feature existing.
        let defaults = BindingSet::from_controls(controls);
        let ser = defaults.to_serialized(controls);
        assert_eq!(ser.len(), 1);
        assert_eq!((ser[0].scale, ser[0].deadzone), (None, None));

        // A sensitivity change is emitted and restored.
        let mut tuned = BindingSet::from_controls(controls);
        tuned.set_scale(InputId(1), 2.5);
        let ser = tuned.to_serialized(controls);
        assert_eq!(ser[0].scale, Some(2.5));

        let mut restored = BindingSet::from_controls(controls);
        restored.apply_overrides(controls, &ser);
        assert_eq!(restored.scale_of(InputId(1)), 2.5);
    }

    #[test]
    fn a_tuning_only_change_is_detected_as_differing_from_defaults() {
        // bindings_eq decides whether anything gets written at all, so if it
        // ignored the tuning a sensitivity change would be silently discarded.
        let controls = &[InputControl {
            id: InputId(1),
            stable_name: "ball_x",
            label: "Trackball X",
            kind: InputKind::AnalogAxis {
                axis: phosphor_core::core::machine::AnalogAxisKind::X,
            },
            player: Some(1),
            default_bindings: &[DefaultBinding::Mouse(MouseControl::AxisX)],
        }];

        let defaults = BindingSet::from_controls(controls).to_serialized(controls);
        let mut tuned = BindingSet::from_controls(controls);
        tuned.set_scale(InputId(1), 0.5);

        assert!(!bindings_eq(&tuned.to_serialized(controls), &defaults));
    }

    #[test]
    fn deadzone_is_tunable_on_digital_direction_axis_bindings() {
        // The reported failure was a resting stick holding a *digital*
        // direction, so the knob has to reach PadAxis bindings, not just the
        // full-axis analog ones.
        let controls = &[InputControl {
            id: InputId(1),
            stable_name: "p1_left",
            label: "P1 Left",
            kind: InputKind::DigitalDirection {
                direction: phosphor_core::core::machine::Direction::Left,
            },
            player: Some(1),
            default_bindings: &[
                DefaultBinding::Key(KeyId::Left),
                DefaultBinding::Pad(PadControl::Axis(CorePadAxis::LeftX, AxisSign::Negative)),
            ],
        }];

        let mut set = BindingSet::from_controls(controls);
        assert_eq!(set.deadzone_of(InputId(1)), Some(STICK_DEADZONE_NORM));
        set.set_deadzone(InputId(1), 0.6);
        assert_eq!(set.deadzone_of(InputId(1)), Some(0.6));

        // Only the axis binding carries it; the key must stay at its default so
        // it is not needlessly persisted.
        let ser = set.to_serialized(controls);
        let key_entry = ser.iter().find(|s| s.input.starts_with("key:")).unwrap();
        assert_eq!(key_entry.deadzone, None);
        let axis_entry = ser
            .iter()
            .find(|s| s.input.starts_with("padaxis:"))
            .unwrap();
        assert_eq!(axis_entry.deadzone, Some(0.6));
    }

    #[test]
    fn a_state_toml_without_tuning_keys_still_loads() {
        // Backward compatibility: every existing state.toml predates these
        // fields, and serde(default) must fill them in rather than fail.
        let toml = r#"
            control = "p1_left"
            input = "key:80"
        "#;
        let parsed: SerializedBinding = toml::from_str(toml).expect("legacy entry parses");
        assert_eq!(parsed.control, "p1_left");
        assert_eq!((parsed.scale, parsed.deadzone), (None, None));
    }

    /// Two plugged-in pads must drive their own players, not both player 1.
    #[test]
    fn pad_bindings_are_scoped_to_their_player_slot() {
        let controls = &[
            InputControl {
                id: InputId(1),
                stable_name: "p1_fire",
                label: "P1 Fire",
                kind: InputKind::Button,
                player: Some(1),
                default_bindings: &[
                    DefaultBinding::Key(KeyId::LShift),
                    DefaultBinding::Pad(PadControl::Button(
                        phosphor_core::core::machine::PadButton::A,
                    )),
                ],
            },
            InputControl {
                id: InputId(2),
                stable_name: "p2_fire",
                label: "P2 Fire",
                kind: InputKind::Button,
                player: Some(2),
                default_bindings: &[DefaultBinding::Pad(PadControl::Button(
                    phosphor_core::core::machine::PadButton::A,
                ))],
            },
            InputControl {
                id: InputId(3),
                stable_name: "coin",
                label: "Coin",
                kind: InputKind::Coin,
                player: None,
                default_bindings: &[DefaultBinding::Pad(PadControl::Button(
                    phosphor_core::core::machine::PadButton::Back,
                ))],
            },
        ];
        let set = BindingSet::from_controls(controls);
        let a = PhysicalInput::PadButton(Button::A);

        // Pad A on slot 1 reaches only player 1; on slot 2 only player 2.
        assert_eq!(ids(set.digital_targets(a, Some(1))), [1]);
        assert_eq!(ids(set.digital_targets(a, Some(2))), [2]);

        // A control with no owning player stays slot-agnostic — coin works from
        // whichever pad is nearest.
        let back = PhysicalInput::PadButton(Button::Back);
        assert_eq!(ids(set.digital_targets(back, Some(1))), [3]);
        assert_eq!(ids(set.digital_targets(back, Some(2))), [3]);

        // Keyboard bindings are never slot-scoped, even for an owned control.
        let shift = PhysicalInput::Key(Scancode::LShift);
        assert_eq!(ids(set.digital_targets(shift, Some(2))), [1]);

        // `None` means "any slot", for the settings UI and non-pad events.
        assert_eq!(ids(set.digital_targets(a, None)), [1, 2]);
    }

    #[test]
    fn dispatch_routes_a_pad_button_to_the_owning_player_only() {
        let bindings = set(vec![
            InputBinding {
                physical: PhysicalInput::PadButton(Button::A),
                target: InputId(1),
                player_slot: Some(1),
                scale: 1.0,
                deadzone: 0.0,
            },
            InputBinding {
                physical: PhysicalInput::PadButton(Button::A),
                target: InputId(2),
                player_slot: Some(2),
                scale: 1.0,
                deadzone: 0.0,
            },
        ]);
        let event = sdl2::event::Event::ControllerButtonDown {
            timestamp: 0,
            which: 0,
            button: Button::A,
        };

        let mut rec = Recorder::default();
        dispatch(
            &event,
            &bindings,
            &mut rec,
            ctx(false, false, Some(2)),
            &mut DispatchState::default(),
        );
        assert_eq!(rec.seen, vec![(2, true)], "slot 2's pad drove player 1");
    }

    #[test]
    fn unrelated_events_are_not_consumed() {
        let bindings = set(vec![binding(PhysicalInput::Key(Scancode::Left), 1)]);
        let mut rec = Recorder::default();
        let quit = Event::Quit { timestamp: 0 };
        assert!(!dispatch(
            &quit,
            &bindings,
            &mut rec,
            ctx(false, true, None),
            &mut DispatchState::default()
        ));
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

    /// A held key must survive resync even though the same control is also
    /// bound to pad inputs that are not pressed.
    ///
    /// Regression: resync used to emit one event per *binding*, so `p1_left`
    /// (Key(Left) + DPadLeft + LeftX-) received `true, false, false` and the
    /// absent gamepad won. Any resync trigger — reset, state load, focus
    /// regain, controller unplug — dropped whatever the player was holding,
    /// the exact desync resync exists to repair.
    #[test]
    fn resync_ors_a_controls_bindings_rather_than_letting_the_last_win() {
        let entry =
            phosphor_machines::registry::find("roadrunner").expect("roadrunner is registered");
        let bindings = BindingSet::from_controls(entry.controls);
        let id_of = |name: &str| {
            entry
                .controls
                .iter()
                .find(|c| c.stable_name == name)
                .unwrap()
                .id
                .0
        };

        // Left and Up held on the keyboard; no gamepad connected.
        let devices = FakeDevices {
            keys: vec![Scancode::Left, Scancode::Up],
            ..Default::default()
        };
        let mut rec = Recorder::default();
        resync(&bindings, &mut rec, &devices);

        // Exactly one event per control, and the held keys stay held.
        for (name, expected) in [
            ("p1_left", true),
            ("p1_up", true),
            ("p1_right", false),
            ("p1_down", false),
        ] {
            let events: Vec<_> = rec
                .seen
                .iter()
                .filter(|(id, _)| *id == id_of(name))
                .collect();
            assert_eq!(
                events.len(),
                1,
                "{name}: expected one event, got {events:?}"
            );
            assert_eq!(events[0].1, expected, "{name} resynced to the wrong state");
        }
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
            scale: None,
            deadzone: None,
        }));
        assert!(ser.contains(&SerializedBinding {
            control: "coin".to_string(),
            input: PhysicalInput::Key(Scancode::Num5).to_token(),
            scale: None,
            deadzone: None,
        }));
    }

    #[test]
    fn apply_overrides_replaces_only_named_controls() {
        let mut set = BindingSet::from_controls(TEST_CONTROLS);
        // Rebind "fire" from Space to Enter; leave "coin" untouched.
        let saved = vec![SerializedBinding {
            control: "fire".to_string(),
            input: PhysicalInput::Key(Scancode::Return).to_token(),
            scale: None,
            deadzone: None,
        }];
        set.apply_overrides(TEST_CONTROLS, &saved);

        // fire now responds to Enter, not Space.
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::Key(Scancode::Return), None)),
            vec![0]
        );
        assert!(ids(set.digital_targets(PhysicalInput::Key(Scancode::Space), None)).is_empty());
        // coin default preserved.
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::Key(Scancode::Num5), None)),
            vec![1]
        );
    }

    #[test]
    fn bindings_eq_is_order_insensitive() {
        let a = vec![
            SerializedBinding {
                control: "fire".into(),
                input: "key:1".into(),
                scale: None,
                deadzone: None,
            },
            SerializedBinding {
                control: "coin".into(),
                input: "key:2".into(),
                scale: None,
                deadzone: None,
            },
        ];
        let b = vec![
            SerializedBinding {
                control: "coin".into(),
                input: "key:2".into(),
                scale: None,
                deadzone: None,
            },
            SerializedBinding {
                control: "fire".into(),
                input: "key:1".into(),
                scale: None,
                deadzone: None,
            },
        ];
        assert!(bindings_eq(&a, &b));
        let c = vec![SerializedBinding {
            control: "fire".into(),
            input: "key:9".into(),
            scale: None,
            deadzone: None,
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
            ids(set.digital_targets(PhysicalInput::Key(Scancode::Space), None)),
            vec![5]
        );
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::PadButton(Button::A), None)),
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
            ids(set.digital_targets(PhysicalInput::Key(Scancode::LShift), None)),
            vec![7]
        );
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::PadButton(Button::A), None)),
            vec![7]
        );
        // Primary moves Fire off Space — the legacy default must be gone.
        assert!(ids(set.digital_targets(PhysicalInput::Key(Scancode::Space), None)).is_empty());
        // The machine-specific extra unions in on top of the role defaults.
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::MouseButtonInput(MouseButton::Left), None)),
            vec![7]
        );
    }

    #[test]
    fn action_role_gives_player_two_its_own_keyboard_half_and_a_slot_scoped_pad() {
        // Player 2 takes RShift rather than LShift so two players can share a
        // keyboard, and the same pad button as player 1 — routed to slot 2.
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
            ids(set.digital_targets(PhysicalInput::Key(Scancode::RShift), None)),
            vec![8]
        );

        // Pad A reaches this control from slot 2 only; slot 1 is player 1's.
        let a = PhysicalInput::PadButton(Button::A);
        assert_eq!(ids(set.digital_targets(a, Some(2))), vec![8]);
        assert!(ids(set.digital_targets(a, Some(1))).is_empty());

        // The keyboard half stays unscoped — a key is a key.
        assert_eq!(
            ids(set.digital_targets(PhysicalInput::Key(Scancode::RShift), Some(1))),
            vec![8]
        );
    }
}
