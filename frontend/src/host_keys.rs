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
//! to exactly one chord, the only modifier is shift, and there is no analog
//! anything.

use std::collections::HashMap;

use sdl2::keyboard::{Mod, Scancode};
use serde::{Deserialize, Serialize};

/// An action the frontend performs itself, rather than passing to the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum HostAction {
    Quit,
    Reset,
    HardReset,
    QuickSave,
    QuickLoad,
    Screenshot,
    TogglePause,
    FrameAdvance,
    ToggleDebugPanel,
    ToggleSettingsPanel,
    ToggleDipPanel,
    ToggleDisplayPanel,
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
            HostAction::HardReset => "Hard reset (power cycle)",
            HostAction::QuickSave => "Quick save",
            HostAction::QuickLoad => "Quick load",
            HostAction::Screenshot => "Screenshot",
            HostAction::TogglePause => "Pause",
            HostAction::FrameAdvance => "Pause and advance one frame",
            HostAction::ToggleDebugPanel => "Debug panel",
            HostAction::ToggleSettingsPanel => "Input settings panel",
            HostAction::ToggleDipPanel => "DIP switch panel",
            HostAction::ToggleDisplayPanel => "Display panel",
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

/// A key together with the modifier state it requires.
///
/// Matched **exactly**, never as "at least these modifiers": bare `F5` fires
/// only with shift up, `Shift+F5` only with shift down. Anything looser makes
/// the two indistinguishable, so one of the pair could never be reached.
///
/// Shift is the only modifier modelled. Ctrl and Alt were considered and left
/// out: nothing in [`DEFAULTS`] needs them, and the one Ctrl chord the frontend
/// has (Ctrl+`` ` `` for the console) is hardcoded in `emulator.rs` and is not a
/// [`HostAction`] at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct HostChord {
    pub scancode: Scancode,
    pub shift: bool,
}

impl HostChord {
    /// The key on its own, which will not fire while shift is held.
    pub const fn bare(scancode: Scancode) -> Self {
        Self {
            scancode,
            shift: false,
        }
    }

    /// The key with shift held.
    pub const fn shift(scancode: Scancode) -> Self {
        Self {
            scancode,
            shift: true,
        }
    }

    /// The chord an SDL key event represents.
    ///
    /// Either shift key counts. SDL reports them separately and no binding here
    /// distinguishes them, so treating one as different from the other would
    /// only produce a hotkey that works on half a keyboard.
    pub fn from_event(scancode: Scancode, keymod: Mod) -> Self {
        Self {
            scancode,
            shift: keymod.intersects(Mod::LSHIFTMOD | Mod::RSHIFTMOD),
        }
    }
}

/// Every action with its factory-default chord, in settings-panel order.
///
/// The function keys follow MAME, which is the layout an arcade emulator user
/// already has in their fingers. That is a deliberate choice to copy a UI
/// convention, not a claim that MAME is authoritative about anything: the layout
/// is worth matching precisely *because* it is what people expect, and the keys
/// used to disagree with it in ways that actively misfired. F5 in particular
/// meant reset here and pause there, so muscle memory reset the machine.
///
/// Where we have no counterpart the key is left free rather than filled:
///
/// - `F2` is MAME's service switch, which is a *machine* input, not an emulator
///   one. Left unbound so a board can claim it.
/// - `F4` is MAME's decoded-graphics viewer. We have one, but only as the
///   `--gfxview` CLI mode, so the key is reserved rather than bound.
/// - `Shift+F4` is MAME's rewind step. Reserved for the checkpoint-rewind work,
///   which has not decided between a step and a hold.
/// - `F9` is frameskip increment in MAME. We do not implement frameskip, so it
///   stays unbound.
/// - `Insert` (fast forward) has no counterpart here.
///
/// One key is knowingly taken for something else. `F8` is MAME's frameskip
/// decrement and `Shift+F8` its cheat toggle, neither of which exists here, so
/// the debugger takes `F8` and the three shifted keys above rather than leaving
/// a whole run of function keys idle. That is the layout's one real divergence.
///
/// Two more deviations worth stating rather than discovering:
///
/// - MAME's `F6`/`F7` prompt for a save slot and put the quick variants on
///   `Shift+F6`/`Shift+F7`. There is one slot here, so the single quick save and
///   load take the bare keys; ours are semantically MAME's shifted pair.
/// - Mouse grab is on `ScrollLock`. MAME releases the pointer with RCtrl+RAlt,
///   which needs modifiers [`HostChord`] deliberately does not model.
///   `ScrollLock` is MAME's "toggle UI controls", the nearest surviving idea of
///   which side owns the input device, and no game wants the key.
///
/// The debugger's four keys sit on `F8` and `Shift+F8`/`F9`/`F10`: run/pause on
/// the bare key with its steps shifted directly above, reading left to right in
/// order of **increasing granularity** (cycle, instruction, frame). They were on
/// `7/8/9/0`, which put them on printable keys, so typing an address into the
/// debug panel's own watch field stepped the CPU. Discoverability comes from
/// [`HostAction::ToggleKeyLegend`] (`?`), and every chord here is rebindable.
pub const DEFAULTS: &[(HostAction, HostChord)] = &[
    (HostAction::Quit, HostChord::bare(Scancode::Escape)),
    (HostAction::Reset, HostChord::bare(Scancode::F3)),
    (HostAction::HardReset, HostChord::shift(Scancode::F3)),
    (HostAction::QuickSave, HostChord::bare(Scancode::F6)),
    (HostAction::QuickLoad, HostChord::bare(Scancode::F7)),
    (HostAction::Screenshot, HostChord::bare(Scancode::F12)),
    (HostAction::TogglePause, HostChord::bare(Scancode::F5)),
    (HostAction::FrameAdvance, HostChord::shift(Scancode::F5)),
    (HostAction::ToggleDebugPanel, HostChord::bare(Scancode::F1)),
    (
        HostAction::ToggleSettingsPanel,
        HostChord::bare(Scancode::Tab),
    ),
    (HostAction::ToggleDipPanel, HostChord::bare(Scancode::Grave)),
    // Shift+` for the display knobs, beside the DIP panel on bare `: both are
    // "how this cabinet is set up" rather than anything MAME has a key for.
    (
        HostAction::ToggleDisplayPanel,
        HostChord::shift(Scancode::Grave),
    ),
    (HostAction::ToggleProfiler, HostChord::shift(Scancode::F11)),
    (HostAction::ToggleThrottle, HostChord::bare(Scancode::F10)),
    (HostAction::ToggleFps, HostChord::bare(Scancode::F11)),
    (
        HostAction::ToggleMouseGrab,
        HostChord::bare(Scancode::ScrollLock),
    ),
    (
        HostAction::ToggleKeyLegend,
        HostChord::bare(Scancode::Slash),
    ),
    (HostAction::StepCycle, HostChord::shift(Scancode::F8)),
    (HostAction::StepInstruction, HostChord::shift(Scancode::F9)),
    (HostAction::StepFrame, HostChord::shift(Scancode::F10)),
    (HostAction::ToggleDebugPause, HostChord::bare(Scancode::F8)),
    (HostAction::MovieRecord, HostChord::shift(Scancode::F12)),
];

/// Human label for a chord in the legend and the rebinding panel.
pub fn chord_label(chord: HostChord) -> String {
    let key = key_label(chord.scancode);
    if chord.shift {
        format!("Shift+{key}")
    } else {
        key
    }
}

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

/// Whether this key is a modifier rather than a key a chord can be built on.
///
/// Used by hotkey capture: a modifier presses *before* the key it modifies and
/// raises its own `KeyDown`, so a capture that took the first press would bind
/// the modifier and never see the key the user was reaching for.
pub fn is_modifier(scancode: Scancode) -> bool {
    matches!(
        scancode,
        Scancode::LShift
            | Scancode::RShift
            | Scancode::LCtrl
            | Scancode::RCtrl
            | Scancode::LAlt
            | Scancode::RAlt
            | Scancode::LGui
            | Scancode::RGui
    )
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

/// One override as it appears in state.toml.
///
/// The untagged `Bare` arm is the pre-chord format, which stored a plain
/// scancode number. Files written before chords existed still deserialize, as
/// the unmodified key they meant. Overrides are rewritten in the `Modified`
/// shape from then on, so this arm is read-only in practice and exists purely so
/// an upgrade does not throw away somebody's rebinds.
///
/// Keyed by scancode *number*, not name, for stability across SDL versions, the
/// same reasoning `PhysicalInput`'s tokens use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
enum StoredChord {
    Bare(i32),
    Modified { code: i32, shift: bool },
}

impl StoredChord {
    fn to_chord(self) -> Option<HostChord> {
        let (code, shift) = match self {
            StoredChord::Bare(code) => (code, false),
            StoredChord::Modified { code, shift } => (code, shift),
        };
        Scancode::from_i32(code).map(|scancode| HostChord { scancode, shift })
    }

    fn from_chord(chord: HostChord) -> Self {
        StoredChord::Modified {
            code: chord.scancode as i32,
            shift: chord.shift,
        }
    }
}

/// Which chord triggers each host action.
///
/// Persisted globally rather than per machine: these are properties of the
/// emulator. Only entries differing from [`DEFAULTS`] are stored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostBindings {
    /// Action → chord overrides.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    overrides: HashMap<String, StoredChord>,
    /// Actions deliberately left with no chord at all.
    ///
    /// A separate list rather than a `None` in `overrides` because TOML has no
    /// null, and [`save`](crate::state::save) discards a serialization error
    /// without telling anyone. Absent from files written before this existed,
    /// which is exactly the "nothing was unbound" they meant.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    unbound: Vec<String>,
}

impl HostBindings {
    /// The chord currently bound to `action`.
    pub fn key_for(&self, action: HostAction) -> Option<HostChord> {
        let key = format!("{action:?}");
        if self.unbound.contains(&key) {
            return None;
        }
        if let Some(stored) = self.overrides.get(&key) {
            return stored.to_chord();
        }
        DEFAULTS
            .iter()
            .find(|(a, _)| *a == action)
            .map(|(_, chord)| *chord)
    }

    /// The action `chord` triggers, if any.
    ///
    /// A rebound action releases its default chord: if the user moves Pause to
    /// F4, pressing P reaches the machine instead.
    pub fn action_for(&self, chord: HostChord) -> Option<HostAction> {
        DEFAULTS
            .iter()
            .map(|(action, _)| *action)
            .find(|action| self.key_for(*action) == Some(chord))
    }

    /// Bind `action` to `chord`, clearing whatever else held it so two actions
    /// can never fire from one press.
    pub fn rebind(&mut self, action: HostAction, chord: HostChord) {
        if let Some(previous) = self.action_for(chord)
            && previous != action
        {
            // Leave the displaced action unbound rather than silently sharing.
            // This used to write Escape's scancode, which is not "unbound": the
            // settings panel showed the displaced action sitting on Escape, and
            // pressing Escape quit, because Quit is matched first.
            self.set_unbound(previous);
        }
        let key = format!("{action:?}");
        self.unbound.retain(|a| *a != key);
        self.overrides.insert(key, StoredChord::from_chord(chord));
    }

    /// Record that `action` has no chord.
    fn set_unbound(&mut self, action: HostAction) {
        let key = format!("{action:?}");
        self.overrides.remove(&key);
        if !self.unbound.contains(&key) {
            self.unbound.push(key);
        }
    }

    /// Restore every action to its factory chord.
    pub fn reset(&mut self) {
        self.overrides.clear();
        self.unbound.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.overrides.is_empty() && self.unbound.is_empty()
    }
}

/// Host keys that a machine also binds, which the machine therefore cannot see.
///
/// Reported at load so a shadowed control is visible rather than mysteriously
/// dead. Debugger-modal actions are excluded: they only claim the key while the
/// debugger is open, so the machine still gets it the rest of the time.
///
/// Only *bare* chords can shadow anything. Game input is dispatched from the
/// scancode alone with no regard for modifiers, so a machine bound to F6 still
/// receives F6 when the frontend holds Shift+F6; the two never collide.
pub fn conflicts(
    bindings: &HostBindings,
    machine_keys: &[Scancode],
) -> Vec<(HostAction, Scancode)> {
    DEFAULTS
        .iter()
        .map(|(action, _)| *action)
        .filter(|a| !a.is_debugger_modal())
        .filter_map(|action| {
            let chord = bindings.key_for(action)?;
            (!chord.shift && machine_keys.contains(&chord.scancode))
                .then_some((action, chord.scancode))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_resolve_both_ways() {
        let b = HostBindings::default();
        let f3 = HostChord::bare(Scancode::F3);
        assert_eq!(b.key_for(HostAction::Reset), Some(f3));
        assert_eq!(b.action_for(f3), Some(HostAction::Reset));
        assert_eq!(b.action_for(HostChord::bare(Scancode::Q)), None);
    }

    /// The layout this file exists to match. Spelled out rather than derived, so
    /// a change to `DEFAULTS` has to be made here too and cannot drift silently.
    #[test]
    fn the_function_keys_match_mame() {
        let b = HostBindings::default();
        let expected = [
            (HostAction::Reset, HostChord::bare(Scancode::F3)),
            (HostAction::HardReset, HostChord::shift(Scancode::F3)),
            (HostAction::TogglePause, HostChord::bare(Scancode::F5)),
            (HostAction::FrameAdvance, HostChord::shift(Scancode::F5)),
            (HostAction::QuickSave, HostChord::bare(Scancode::F6)),
            (HostAction::QuickLoad, HostChord::bare(Scancode::F7)),
            (HostAction::ToggleThrottle, HostChord::bare(Scancode::F10)),
            (HostAction::ToggleFps, HostChord::bare(Scancode::F11)),
            (HostAction::ToggleProfiler, HostChord::shift(Scancode::F11)),
            (HostAction::Screenshot, HostChord::bare(Scancode::F12)),
            (HostAction::MovieRecord, HostChord::shift(Scancode::F12)),
        ];
        for (action, chord) in expected {
            assert_eq!(
                b.key_for(action),
                Some(chord),
                "{action:?} must sit where MAME puts it"
            );
        }
    }

    /// Keys held free because we have no counterpart for what MAME does with
    /// them. Binding one of these is a decision, not an accident, so it should
    /// have to come through here.
    #[test]
    fn the_keys_reserved_for_missing_features_stay_unbound() {
        let b = HostBindings::default();
        for chord in [
            // MAME: service switch, and a machine input rather than a UI one.
            HostChord::bare(Scancode::F2),
            // MAME: decoded-graphics viewer. Ours is the --gfxview CLI mode.
            HostChord::bare(Scancode::F4),
            // MAME: rewind one checkpoint. Owned by the rewind work.
            HostChord::shift(Scancode::F4),
            // MAME: frameskip. We have none.
            HostChord::bare(Scancode::F9),
        ] {
            assert_eq!(
                b.action_for(chord),
                None,
                "{} is reserved and must stay unbound",
                chord_label(chord)
            );
        }
    }

    /// The keys the old layout used, which now belong to the machine.
    #[test]
    fn the_keys_the_old_layout_stole_are_returned_to_the_machine() {
        let b = HostBindings::default();
        for sc in [
            Scancode::P,
            Scancode::Num7,
            Scancode::Num8,
            Scancode::Num9,
            Scancode::Num0,
        ] {
            assert_eq!(
                b.action_for(HostChord::bare(sc)),
                None,
                "{sc:?} must reach the machine now"
            );
        }
    }

    #[test]
    fn a_chord_matches_its_modifier_state_exactly() {
        // The whole point of chords: the shifted and unshifted forms of one key
        // are different bindings. Matching loosely (shift ignored, or "at least
        // these modifiers") makes one of the pair unreachable.
        let mut b = HostBindings::default();
        b.rebind(HostAction::Screenshot, HostChord::shift(Scancode::F12));

        assert_eq!(
            b.action_for(HostChord::shift(Scancode::F12)),
            Some(HostAction::Screenshot)
        );
        assert_eq!(b.action_for(HostChord::bare(Scancode::F12)), None);
        // ...and in the other direction: a bare binding must not fire shifted.
        // F6 rather than F3, which legitimately has an action on both halves.
        assert_eq!(
            b.action_for(HostChord::bare(Scancode::F6)),
            Some(HostAction::QuickSave)
        );
        assert_eq!(b.action_for(HostChord::shift(Scancode::F6)), None);
        // F3 is the case that motivates exact matching: two different resets,
        // one destructive, separated only by the modifier.
        assert_eq!(
            b.action_for(HostChord::bare(Scancode::F3)),
            Some(HostAction::Reset)
        );
        assert_eq!(
            b.action_for(HostChord::shift(Scancode::F3)),
            Some(HostAction::HardReset)
        );
    }

    #[test]
    fn either_shift_key_builds_the_same_chord() {
        // Binding only one of them would produce a hotkey that works on half a
        // keyboard.
        let left = HostChord::from_event(Scancode::F5, Mod::LSHIFTMOD);
        let right = HostChord::from_event(Scancode::F5, Mod::RSHIFTMOD);
        assert_eq!(left, right);
        assert_eq!(left, HostChord::shift(Scancode::F5));
        // Modifiers we do not model must not leak into the chord.
        assert_eq!(
            HostChord::from_event(Scancode::F5, Mod::LCTRLMOD),
            HostChord::bare(Scancode::F5)
        );
    }

    #[test]
    fn every_default_chord_is_unique() {
        // Two actions on one chord would make the press ambiguous. Compared on
        // the pair, not the scancode: F8 and Shift+F8 are legitimately distinct.
        let mut keys: Vec<(i32, bool)> = DEFAULTS
            .iter()
            .map(|(_, chord)| (chord.scancode as i32, chord.shift))
            .collect();
        let before = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(before, keys.len(), "duplicate default host chord");
    }

    #[test]
    fn rebinding_frees_the_old_key_for_the_machine() {
        let mut b = HostBindings::default();
        b.rebind(HostAction::TogglePause, HostChord::bare(Scancode::F4));
        assert_eq!(
            b.key_for(HostAction::TogglePause),
            Some(HostChord::bare(Scancode::F4))
        );
        // P is no longer a host key, so a machine binding it now works.
        assert_eq!(b.action_for(HostChord::bare(Scancode::P)), None);
    }

    #[test]
    fn rebinding_onto_a_taken_chord_leaves_the_other_action_unbound() {
        let mut b = HostBindings::default();
        b.rebind(HostAction::TogglePause, HostChord::bare(Scancode::F3));
        assert_eq!(
            b.action_for(HostChord::bare(Scancode::F3)),
            Some(HostAction::TogglePause)
        );
        // Displacement used to write Escape's scancode, so the panel showed the
        // displaced action sitting on Escape and pressing Escape quit. It has to
        // be genuinely unbound.
        assert_eq!(b.key_for(HostAction::Reset), None);
        assert_eq!(
            b.action_for(HostChord::bare(Scancode::Escape)),
            Some(HostAction::Quit)
        );
    }

    #[test]
    fn rebinding_a_displaced_action_takes_it_off_the_unbound_list() {
        let mut b = HostBindings::default();
        b.rebind(HostAction::TogglePause, HostChord::bare(Scancode::F3));
        assert_eq!(b.key_for(HostAction::Reset), None);
        b.rebind(HostAction::Reset, HostChord::shift(Scancode::F3));
        assert_eq!(
            b.key_for(HostAction::Reset),
            Some(HostChord::shift(Scancode::F3))
        );
    }

    #[test]
    fn a_pre_chord_state_toml_still_loads() {
        // A literal old-format string, NOT something built by the current
        // serializer: a round trip through our own writer would pass with the
        // compatibility arm deleted, which is the whole thing being tested.
        // 61 is Scancode::F4 and 41 is Scancode::Escape.
        let legacy = r#"
            [overrides]
            Reset = 61
            TogglePause = 41
        "#;
        let b: HostBindings = toml::from_str(legacy).expect("pre-chord format must still parse");
        assert_eq!(
            b.key_for(HostAction::Reset),
            Some(HostChord::bare(Scancode::F4)),
            "an unmodified scancode is what the old format meant"
        );
        assert_eq!(
            b.key_for(HostAction::TogglePause),
            Some(HostChord::bare(Scancode::Escape))
        );
        // Untouched actions still fall through to the factory chord.
        assert_eq!(
            b.key_for(HostAction::Screenshot),
            Some(HostChord::bare(Scancode::F12))
        );
    }

    #[test]
    fn chords_round_trip_through_the_current_format() {
        let mut b = HostBindings::default();
        b.rebind(HostAction::Screenshot, HostChord::shift(Scancode::F12));
        b.rebind(HostAction::Reset, HostChord::bare(Scancode::F3));
        let text = toml::to_string(&b).expect("bindings must serialize");
        let back: HostBindings = toml::from_str(&text).expect("and parse back");
        assert_eq!(back, b, "serialized form: {text}");
        // The shift bit specifically has to survive; it is the new part.
        assert_eq!(
            back.key_for(HostAction::Screenshot),
            Some(HostChord::shift(Scancode::F12))
        );
    }

    #[test]
    fn conflicts_report_machine_shadowing_but_not_debugger_keys() {
        let b = HostBindings::default();
        // A machine binding Tab and ` loses both to the frontend.
        let found = conflicts(&b, &[Scancode::Tab, Scancode::Grave, Scancode::F8]);
        let actions: Vec<HostAction> = found.iter().map(|(a, _)| *a).collect();
        assert!(actions.contains(&HostAction::ToggleSettingsPanel));
        assert!(actions.contains(&HostAction::ToggleDipPanel));
        // F8 only runs/pauses while the debugger is open, so it is not a
        // conflict: the machine keeps it the rest of the time.
        assert!(!actions.contains(&HostAction::ToggleDebugPause));
    }

    #[test]
    fn a_shifted_chord_does_not_shadow_a_machine_key() {
        // Game input dispatches on the scancode alone, so a machine bound to F12
        // still receives F12 while the frontend holds Shift+F12.
        let mut b = HostBindings::default();
        b.rebind(HostAction::Screenshot, HostChord::shift(Scancode::F12));
        let found = conflicts(&b, &[Scancode::F12]);
        assert!(
            found.is_empty(),
            "Shift+F12 cannot take F12 from a machine, got {found:?}"
        );
        // The same action on the bare key does shadow it, which is what makes
        // the assertion above meaningful.
        b.rebind(HostAction::Screenshot, HostChord::bare(Scancode::F12));
        assert_eq!(
            conflicts(&b, &[Scancode::F12]),
            vec![(HostAction::Screenshot, Scancode::F12)]
        );
    }

    #[test]
    fn step_keys_run_in_order_of_increasing_granularity() {
        // Shift+F8/F9/F10 should read left-to-right as "step a little / a bit
        // more / a lot"; run/pause sits on the bare F8 underneath them. All four
        // are on function keys so they survive typing in the debugger's own
        // address fields, which is why they moved off 7/8/9/0.
        let b = HostBindings::default();
        assert_eq!(
            b.key_for(HostAction::StepCycle),
            Some(HostChord::shift(Scancode::F8))
        );
        assert_eq!(
            b.key_for(HostAction::StepInstruction),
            Some(HostChord::shift(Scancode::F9))
        );
        assert_eq!(
            b.key_for(HostAction::StepFrame),
            Some(HostChord::shift(Scancode::F10))
        );
        assert_eq!(
            b.key_for(HostAction::ToggleDebugPause),
            Some(HostChord::bare(Scancode::F8))
        );
        // The point of the move: none of them can be typed into a text field.
        for action in [
            HostAction::StepCycle,
            HostAction::StepInstruction,
            HostAction::StepFrame,
            HostAction::ToggleDebugPause,
        ] {
            let chord = b.key_for(action).expect("bound");
            assert!(
                survives_text_entry(chord.scancode),
                "{action:?} is back on a key that types"
            );
        }
    }

    #[test]
    fn key_legend_is_bound_and_not_debugger_modal() {
        let b = HostBindings::default();
        // `?` is the legend key, and it works whether or not the debugger is up.
        assert_eq!(
            b.key_for(HostAction::ToggleKeyLegend),
            Some(HostChord::bare(Scancode::Slash))
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
    fn chord_labels_name_the_modifier() {
        assert_eq!(chord_label(HostChord::bare(Scancode::F5)), "F5");
        assert_eq!(chord_label(HostChord::shift(Scancode::F11)), "Shift+F11");
        // The key-cap rendering still applies underneath the modifier.
        assert_eq!(chord_label(HostChord::shift(Scancode::Slash)), "Shift+? /");
    }

    #[test]
    fn modifiers_are_not_bindable_on_their_own() {
        // Hotkey capture waits for a non-modifier, or holding shift to build a
        // chord would bind shift and end the capture.
        for sc in [
            Scancode::LShift,
            Scancode::RShift,
            Scancode::LCtrl,
            Scancode::LAlt,
            Scancode::LGui,
        ] {
            assert!(is_modifier(sc), "{sc:?} must not be capturable");
        }
        for sc in [Scancode::F5, Scancode::A, Scancode::Num7, Scancode::Escape] {
            assert!(!is_modifier(sc), "{sc:?} must be capturable");
        }
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
        // Displace Pause too, so reset() has an unbound entry to clear as well
        // as an override.
        b.rebind(HostAction::Reset, HostChord::bare(Scancode::F5));
        assert_eq!(b.key_for(HostAction::TogglePause), None);
        b.reset();
        assert!(b.is_empty());
        assert_eq!(
            b.key_for(HostAction::Reset),
            Some(HostChord::bare(Scancode::F3))
        );
        assert_eq!(
            b.key_for(HostAction::TogglePause),
            Some(HostChord::bare(Scancode::F5))
        );
    }
}
