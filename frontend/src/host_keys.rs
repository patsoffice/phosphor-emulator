//! Rebindable frontend hotkeys.
//!
//! The emulator's own keys (reset, save state, pause, panel toggles) were fixed
//! `Scancode` arms matched before game input, so they were unconditionally
//! stolen from the machine. `Tab` and `P` are both in
//! [`KeyId`](phosphor_core::core::machine::KeyId), so a machine binding either
//! was silently shadowed with nothing logged.
//!
//! This is a second, deliberately smaller binding system than
//! [`BindingSet`](crate::input::BindingSet): host actions are global rather than
//! per-machine (they are properties of the emulator, not the cabinet), each maps
//! to exactly one key, and there is no analog anything.

use std::collections::HashMap;

use sdl2::keyboard::Scancode;
use serde::{Deserialize, Serialize};

/// An action the frontend performs itself, rather than passing to the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HostAction {
    Quit,
    Reset,
    QuickSave,
    QuickLoad,
    Screenshot,
    TogglePause,
    ToggleDebugPanel,
    ToggleSettingsPanel,
    ToggleDipPanel,
    ToggleProfiler,
    ToggleThrottle,
    ToggleFps,
    ToggleMouseGrab,
    ToggleKeyLegend,
    StepInstruction,
    StepCycle,
    StepFrame,
    ToggleDebugPause,
    MovieRecord,
}

impl HostAction {
    /// Label for the settings panel.
    pub fn label(self) -> &'static str {
        match self {
            HostAction::Quit => "Quit",
            HostAction::Reset => "Reset machine",
            HostAction::QuickSave => "Quick save",
            HostAction::QuickLoad => "Quick load",
            HostAction::Screenshot => "Screenshot",
            HostAction::TogglePause => "Pause",
            HostAction::ToggleDebugPanel => "Debug panel",
            HostAction::ToggleSettingsPanel => "Input settings panel",
            HostAction::ToggleDipPanel => "DIP switch panel",
            HostAction::ToggleProfiler => "Profiler",
            HostAction::ToggleThrottle => "Frame throttle",
            HostAction::ToggleFps => "FPS overlay",
            HostAction::ToggleMouseGrab => "Grab mouse",
            HostAction::ToggleKeyLegend => "Key legend",
            HostAction::StepInstruction => "Debugger: step instruction",
            HostAction::MovieRecord => "Record input movie",
            HostAction::StepCycle => "Debugger: step cycle",
            HostAction::StepFrame => "Debugger: step frame",
            HostAction::ToggleDebugPause => "Debugger: pause/run",
        }
    }

    /// Whether this action is only meaningful while the debugger is open.
    ///
    /// These stay reserved: they are modal, and a machine binding the same key
    /// still reaches it whenever the debugger is closed.
    pub fn is_debugger_modal(self) -> bool {
        matches!(
            self,
            HostAction::StepInstruction
                | HostAction::StepCycle
                | HostAction::StepFrame
                | HostAction::ToggleDebugPause
        )
    }
}

/// Every action with its factory-default key, in settings-panel order.
///
/// The step keys sit on the number row rather than on function keys or letters:
/// F1 and F5-F12 are all taken, and letters/Space collide with game input. They
/// run 7/8/9 in order of **increasing granularity** — cycle, instruction, frame
/// — so the row reads left-to-right as "step a little / a bit more / a lot",
/// with 0 (run/pause) beside them. Modifier combos were considered and rejected:
/// [`HostBindings`] maps one bare scancode per action, and a chord would need a
/// modifier in the binding model for four keys' benefit. Discoverability comes
/// from [`HostAction::ToggleKeyLegend`] (`?`) instead, and every key here is
/// rebindable.
pub const DEFAULTS: &[(HostAction, Scancode)] = &[
    (HostAction::Quit, Scancode::Escape),
    (HostAction::Reset, Scancode::F5),
    (HostAction::QuickSave, Scancode::F6),
    (HostAction::QuickLoad, Scancode::F7),
    (HostAction::Screenshot, Scancode::F12),
    (HostAction::TogglePause, Scancode::P),
    (HostAction::ToggleDebugPanel, Scancode::F1),
    (HostAction::ToggleSettingsPanel, Scancode::Tab),
    (HostAction::ToggleDipPanel, Scancode::Grave),
    (HostAction::ToggleProfiler, Scancode::F8),
    (HostAction::ToggleThrottle, Scancode::F9),
    (HostAction::ToggleFps, Scancode::F10),
    (HostAction::ToggleMouseGrab, Scancode::F11),
    (HostAction::ToggleKeyLegend, Scancode::Slash),
    (HostAction::StepCycle, Scancode::Num7),
    (HostAction::StepInstruction, Scancode::Num8),
    (HostAction::StepFrame, Scancode::Num9),
    (HostAction::ToggleDebugPause, Scancode::Num0),
    (HostAction::MovieRecord, Scancode::F2),
];

/// Human label for a key in the legend and the rebinding panel.
///
/// `{Scancode:?}` alone reads badly for the punctuation keys — the legend key
/// prints as `Slash`, not the `?` printed on it — so the ones this frontend
/// binds by default get a printable name.
pub fn key_label(scancode: Scancode) -> String {
    match scancode {
        Scancode::Slash => "? /".to_string(),
        Scancode::Grave => "`".to_string(),
        Scancode::Num0 => "0".to_string(),
        Scancode::Num1 => "1".to_string(),
        Scancode::Num2 => "2".to_string(),
        Scancode::Num3 => "3".to_string(),
        Scancode::Num4 => "4".to_string(),
        Scancode::Num5 => "5".to_string(),
        Scancode::Num6 => "6".to_string(),
        Scancode::Num7 => "7".to_string(),
        Scancode::Num8 => "8".to_string(),
        Scancode::Num9 => "9".to_string(),
        other => format!("{other:?}"),
    }
}

/// Whether a hotkey on this key may still fire while an egui text field has
/// keyboard focus.
///
/// The debug panel has text fields (a watch address, a breakpoint address). Any
/// hotkey on a printable key fired *while typing into one*: entering an address
/// containing `7` stepped a cycle, `0` toggled run/pause, and `/`, `` ` ``, `P`
/// and `Tab` were hit the same way. The frontend has a focus flag for exactly
/// this, but it was only ever consulted on the game-input path
/// ([`DispatchCtx::egui_wants_keyboard`](crate::input::DispatchCtx)), never on
/// the hotkeys, which match earlier.
///
/// The exemption is the keys egui cannot turn into text or focus movement, so
/// suppressing them would cost something and protect nothing. That is the
/// function keys, plus `ScrollLock` because a hotkey lives there. `Escape` is
/// deliberately *not* exempt: egui takes it to leave a field, so the first press
/// defocuses and only the second reaches [`HostAction::Quit`].
pub fn survives_text_entry(scancode: Scancode) -> bool {
    matches!(
        scancode,
        Scancode::F1
            | Scancode::F2
            | Scancode::F3
            | Scancode::F4
            | Scancode::F5
            | Scancode::F6
            | Scancode::F7
            | Scancode::F8
            | Scancode::F9
            | Scancode::F10
            | Scancode::F11
            | Scancode::F12
            | Scancode::ScrollLock
    )
}

/// Which key triggers each host action.
///
/// Persisted globally rather than per machine: these are properties of the
/// emulator. Only entries differing from [`DEFAULTS`] are stored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostBindings {
    /// Action → scancode overrides, keyed by scancode number for stability
    /// across SDL versions the same way `PhysicalInput`'s tokens are.
    #[serde(default)]
    overrides: HashMap<String, i32>,
}

impl HostBindings {
    /// The key currently bound to `action`.
    pub fn key_for(&self, action: HostAction) -> Option<Scancode> {
        if let Some(code) = self.overrides.get(&format!("{action:?}")) {
            return Scancode::from_i32(*code);
        }
        DEFAULTS
            .iter()
            .find(|(a, _)| *a == action)
            .map(|(_, sc)| *sc)
    }

    /// The action `scancode` triggers, if any.
    ///
    /// A rebound action releases its default key: if the user moves Pause to
    /// F4, pressing P reaches the machine instead.
    pub fn action_for(&self, scancode: Scancode) -> Option<HostAction> {
        DEFAULTS
            .iter()
            .map(|(action, _)| *action)
            .find(|action| self.key_for(*action) == Some(scancode))
    }

    /// Bind `action` to `scancode`, clearing whatever else held that key so two
    /// actions can never fire from one press.
    pub fn rebind(&mut self, action: HostAction, scancode: Scancode) {
        if let Some(previous) = self.action_for(scancode)
            && previous != action
        {
            // Leave the displaced action unbound rather than silently sharing.
            self.overrides
                .insert(format!("{previous:?}"), Scancode::Escape as i32);
        }
        self.overrides
            .insert(format!("{action:?}"), scancode as i32);
    }

    /// Restore every action to its factory key.
    pub fn reset(&mut self) {
        self.overrides.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.overrides.is_empty()
    }
}

/// Host keys that a machine also binds, which the machine therefore cannot see.
///
/// Reported at load so a shadowed control is visible rather than mysteriously
/// dead. Debugger-modal actions are excluded: they only claim the key while the
/// debugger is open, so the machine still gets it the rest of the time.
pub fn conflicts(
    bindings: &HostBindings,
    machine_keys: &[Scancode],
) -> Vec<(HostAction, Scancode)> {
    DEFAULTS
        .iter()
        .map(|(action, _)| *action)
        .filter(|a| !a.is_debugger_modal())
        .filter_map(|action| {
            let key = bindings.key_for(action)?;
            machine_keys.contains(&key).then_some((action, key))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_resolve_both_ways() {
        let b = HostBindings::default();
        assert_eq!(b.key_for(HostAction::Reset), Some(Scancode::F5));
        assert_eq!(b.action_for(Scancode::F5), Some(HostAction::Reset));
        assert_eq!(b.action_for(Scancode::Q), None);
    }

    #[test]
    fn every_default_key_is_unique() {
        // Two actions on one key would make the press ambiguous.
        let mut keys: Vec<i32> = DEFAULTS.iter().map(|(_, sc)| *sc as i32).collect();
        let before = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(before, keys.len(), "duplicate default host key");
    }

    #[test]
    fn rebinding_frees_the_old_key_for_the_machine() {
        let mut b = HostBindings::default();
        b.rebind(HostAction::TogglePause, Scancode::F4);
        assert_eq!(b.key_for(HostAction::TogglePause), Some(Scancode::F4));
        // P is no longer a host key, so a machine binding it now works.
        assert_eq!(b.action_for(Scancode::P), None);
    }

    #[test]
    fn rebinding_onto_a_taken_key_displaces_the_other_action() {
        let mut b = HostBindings::default();
        b.rebind(HostAction::TogglePause, Scancode::F5);
        assert_eq!(b.action_for(Scancode::F5), Some(HostAction::TogglePause));
        assert_ne!(b.key_for(HostAction::Reset), Some(Scancode::F5));
    }

    #[test]
    fn conflicts_report_machine_shadowing_but_not_debugger_keys() {
        let b = HostBindings::default();
        // A machine binding Tab and P loses both to the frontend today.
        let found = conflicts(&b, &[Scancode::Tab, Scancode::P, Scancode::Num7]);
        let actions: Vec<HostAction> = found.iter().map(|(a, _)| *a).collect();
        assert!(actions.contains(&HostAction::ToggleSettingsPanel));
        assert!(actions.contains(&HostAction::TogglePause));
        // Num7 only steps while the debugger is open, so it is not a conflict.
        assert!(!actions.contains(&HostAction::StepInstruction));
    }

    #[test]
    fn step_keys_run_in_order_of_increasing_granularity() {
        // 7/8/9 should read left-to-right as "step a little / a bit more / a
        // lot"; the pause toggle sits beside them on 0.
        let b = HostBindings::default();
        assert_eq!(b.key_for(HostAction::StepCycle), Some(Scancode::Num7));
        assert_eq!(b.key_for(HostAction::StepInstruction), Some(Scancode::Num8));
        assert_eq!(b.key_for(HostAction::StepFrame), Some(Scancode::Num9));
        assert_eq!(
            b.key_for(HostAction::ToggleDebugPause),
            Some(Scancode::Num0)
        );
    }

    #[test]
    fn key_legend_is_bound_and_not_debugger_modal() {
        let b = HostBindings::default();
        // `?` is the legend key, and it works whether or not the debugger is up.
        assert_eq!(
            b.key_for(HostAction::ToggleKeyLegend),
            Some(Scancode::Slash)
        );
        assert!(!HostAction::ToggleKeyLegend.is_debugger_modal());
    }

    #[test]
    fn key_labels_print_the_key_cap_not_the_scancode_name() {
        assert_eq!(key_label(Scancode::Slash), "? /");
        assert_eq!(key_label(Scancode::Grave), "`");
        assert_eq!(key_label(Scancode::Num7), "7");
        // Keys with a readable Debug name pass through unchanged.
        assert_eq!(key_label(Scancode::F5), "F5");
        assert_eq!(key_label(Scancode::Tab), "Tab");
    }

    #[test]
    fn text_entry_suppresses_printable_hotkeys_but_not_function_keys() {
        // Every one of these fired while typing an address into the debug
        // panel: 7/8/9/0 stepped the CPU, `/` opened the legend, `` ` `` the DIP
        // panel, P paused and Tab opened the settings panel.
        for sc in [
            Scancode::Num7,
            Scancode::Num8,
            Scancode::Num9,
            Scancode::Num0,
            Scancode::Slash,
            Scancode::Grave,
            Scancode::P,
            Scancode::Tab,
        ] {
            assert!(
                !survives_text_entry(sc),
                "{sc:?} must yield to a focused text field"
            );
        }
        // Function keys produce no text, so suppressing them would cost the
        // debugger its step keys and protect nothing.
        for sc in [
            Scancode::F1,
            Scancode::F5,
            Scancode::F8,
            Scancode::F12,
            Scancode::ScrollLock,
        ] {
            assert!(survives_text_entry(sc), "{sc:?} must stay live");
        }
        // Escape leaves the field on the first press and quits on the second.
        assert!(!survives_text_entry(Scancode::Escape));
    }

    #[test]
    fn reset_restores_factory_keys() {
        let mut b = HostBindings::default();
        b.rebind(HostAction::Reset, Scancode::F4);
        b.reset();
        assert!(b.is_empty());
        assert_eq!(b.key_for(HostAction::Reset), Some(Scancode::F5));
    }
}
