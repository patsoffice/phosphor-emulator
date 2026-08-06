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
    StepInstruction,
    StepCycle,
    StepFrame,
    ToggleDebugPause,
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
            HostAction::StepInstruction => "Debugger: step instruction",
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
    (HostAction::StepInstruction, Scancode::Num7),
    (HostAction::StepCycle, Scancode::Num8),
    (HostAction::StepFrame, Scancode::Num9),
    (HostAction::ToggleDebugPause, Scancode::Num0),
];

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
    fn reset_restores_factory_keys() {
        let mut b = HostBindings::default();
        b.rebind(HostAction::Reset, Scancode::F4);
        b.reset();
        assert!(b.is_empty());
        assert_eq!(b.key_for(HostAction::Reset), Some(Scancode::F5));
    }
}
