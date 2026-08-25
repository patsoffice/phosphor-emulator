//! Auto-saved emulator state persisted across sessions.
//!
//! Stored at `~/.config/phosphor/state.toml`. Unlike `config.toml` (user-edited),
//! this file is written automatically on exit.
//!
//! All per-machine data lives under a single `[machines.<name>]` section keyed
//! by the machine's registry/CLI name (e.g. `joust`). Each [`MachineSettings`]
//! holds optional per-game *config overrides* (scale, ROM/NVRAM/save paths) that
//! take precedence over the global `config.toml` defaults, plus the persisted
//! DIP-switch and input-binding diffs. Every field is diff-only: a field is
//! present only when it differs from the resolved default, and an entry that
//! carries no overrides is dropped entirely, keeping the file free of redundant
//! defaults.

use std::collections::HashMap;

use phosphor_core::core::display::DisplaySettings;
use serde::{Deserialize, Serialize};

use crate::input::SerializedBinding;

/// Per-machine settings: config overrides plus persisted DIP + input state.
///
/// Config-override fields (`scale`, `rom_path`, `nvram_path`, `save_path`) win
/// over the global `config.toml` defaults but are themselves overridden by CLI
/// arguments — see the resolution chain in `main.rs`. `dip_switches` and
/// `input_bindings` are written automatically when they diverge from the
/// machine's power-on / default-binding state. All fields are optional so an
/// entry only records what actually differs from the defaults.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct MachineSettings {
    /// Per-game window scale override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<u32>,
    /// Per-game fullscreen override. `None` falls back to the global config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fullscreen: Option<bool>,
    /// Per-game ROM directory/file override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rom_path: Option<String>,
    /// Per-game NVRAM directory override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nvram_path: Option<String>,
    /// Per-game save-state directory override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_path: Option<String>,
    /// Live DIP byte per bank, in `dip_banks()` order. Empty means "use the
    /// machine's power-on defaults".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dip_switches: Vec<u8>,
    /// Overridden input bindings. Empty means "use the machine defaults".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_bindings: Vec<SerializedBinding>,
    /// Display knobs that this game is set differently from the defaults.
    #[serde(default, skip_serializing_if = "DisplayOverrides::is_empty")]
    pub display: DisplayOverrides,
}

impl MachineSettings {
    /// True when no field carries a non-default value. Such entries are dropped
    /// from [`State::machines`] so unchanged games leave no trace in state.toml.
    pub fn is_empty(&self) -> bool {
        self.scale.is_none()
            && self.fullscreen.is_none()
            && self.rom_path.is_none()
            && self.nvram_path.is_none()
            && self.save_path.is_none()
            && self.dip_switches.is_empty()
            && self.input_bindings.is_empty()
            && self.display.is_empty()
    }
}

/// Display knobs recorded only where they differ from what they are resolved
/// against, so a game left alone writes nothing at all.
///
/// A serialisable mirror of [`DisplaySettings`] rather than the type itself:
/// phosphor-core carries no serde, and it should not grow a dependency for a
/// frontend preference.
#[derive(Debug, Default, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DisplayOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brightness: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub halation: Option<f32>,
}

impl DisplayOverrides {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// What `settings` differs from `base` in, and nothing else.
    pub fn diff(settings: DisplaySettings, base: DisplaySettings) -> Self {
        let keep = |value: f32, against: f32| (value != against).then_some(value);
        Self {
            brightness: keep(settings.brightness, base.brightness),
            focus: keep(settings.focus, base.focus),
            halation: keep(settings.halation, base.halation),
        }
    }

    /// `base` with whatever this records laid over it.
    pub fn apply_to(&self, base: DisplaySettings) -> DisplaySettings {
        DisplaySettings {
            brightness: self.brightness.unwrap_or(base.brightness),
            focus: self.focus.unwrap_or(base.focus),
            halation: self.halation.unwrap_or(base.halation),
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    /// Frontend hotkey overrides. Global rather than per machine: these are
    /// properties of the emulator, not the cabinet.
    #[serde(
        default,
        skip_serializing_if = "crate::host_keys::HostBindings::is_empty"
    )]
    pub host_bindings: crate::host_keys::HostBindings,
    pub window_x: Option<i32>,
    pub window_y: Option<i32>,
    /// Per-machine settings, keyed by the machine's registry/CLI name. Absent
    /// means "use the resolved defaults for every field".
    #[serde(default)]
    pub machines: HashMap<String, MachineSettings>,

    // --- Legacy fields (pre-unification) -----------------------------------
    // Older state.toml files stored bindings and DIPs as separate top-level
    // maps keyed by `machine_id`. These are read on load and folded into
    // `machines` by `migrate()`, then never written again (skip_serializing).
    #[serde(default, rename = "input_bindings", skip_serializing)]
    legacy_input_bindings: HashMap<String, Vec<SerializedBinding>>,
    #[serde(default, rename = "dip_switches", skip_serializing)]
    legacy_dip_switches: HashMap<String, Vec<u8>>,
}

impl State {
    /// Borrow a machine's persisted settings, if any.
    pub fn machine(&self, name: &str) -> Option<&MachineSettings> {
        self.machines.get(name)
    }

    /// Insert or replace `settings` for `name`, dropping the entry entirely when
    /// it carries no overrides (keeps state.toml free of empty entries).
    pub fn set_machine(&mut self, name: &str, settings: MachineSettings) {
        if settings.is_empty() {
            self.machines.remove(name);
        } else {
            self.machines.insert(name.to_string(), settings);
        }
    }

    /// One-time migration: fold legacy top-level `input_bindings` / `dip_switches`
    /// maps into the unified `machines` map, then clear them so they are not
    /// re-serialized.
    ///
    /// Legacy entries are keyed by the old `machine_id`. For the handful of
    /// machines whose CLI name differs from `machine_id` (e.g. `asteroid` vs
    /// `asteroids`) the migrated entry keeps the old key and is simply never
    /// matched again — an acceptable reset for an auto-generated file.
    fn migrate(&mut self) {
        for (id, bindings) in self.legacy_input_bindings.drain() {
            if !bindings.is_empty() {
                self.machines.entry(id).or_default().input_bindings = bindings;
            }
        }
        for (id, dips) in self.legacy_dip_switches.drain() {
            if !dips.is_empty() {
                self.machines.entry(id).or_default().dip_switches = dips;
            }
        }
    }
}

/// Load state from `~/.config/phosphor/state.toml`, falling back to defaults.
pub fn load() -> State {
    let Some(path) = crate::config::config_dir().map(|d| d.join("state.toml")) else {
        return State::default();
    };
    let mut state: State = match std::fs::read_to_string(&path) {
        Ok(contents) => toml::from_str(&contents).unwrap_or_default(),
        Err(_) => State::default(),
    };
    state.migrate();
    state
}

/// Save state to `~/.config/phosphor/state.toml`.
pub fn save(state: &State) {
    let Some(dir) = crate::config::config_dir() else {
        return;
    };
    std::fs::create_dir_all(&dir).ok();
    if let Ok(contents) = toml::to_string_pretty(state)
        && let Err(e) = std::fs::write(dir.join("state.toml"), contents)
    {
        eprintln!("Warning: failed to save state: {e}");
    }
}

#[cfg(test)]
mod display_overrides_tests {
    use super::*;

    #[test]
    fn a_setting_left_alone_records_nothing() {
        let base = DisplaySettings::MEASURED;
        let diff = DisplayOverrides::diff(base, base);
        assert!(
            diff.is_empty(),
            "a game at the defaults should leave no trace in state.toml"
        );
        assert_eq!(diff.apply_to(base), base);
    }

    #[test]
    fn only_the_knob_that_moved_is_recorded() {
        let base = DisplaySettings::MEASURED;
        let moved = DisplaySettings {
            halation: base.halation + 0.05,
            ..base
        };

        let diff = DisplayOverrides::diff(moved, base);
        assert_eq!(diff.halation, Some(moved.halation));
        assert_eq!(diff.brightness, None, "brightness did not move");
        assert_eq!(diff.focus, None, "focus did not move");
        assert_eq!(diff.apply_to(base), moved, "and it round trips");
    }

    #[test]
    fn a_per_game_override_wins_over_the_global_one() {
        // The chain is measured, then the global preference, then this game's
        // own, the same order the paths and the scale resolve in.
        let measured = DisplaySettings::MEASURED;
        let global = DisplayOverrides {
            brightness: Some(1.4),
            focus: Some(1.5),
            halation: None,
        };
        let per_game = DisplayOverrides {
            brightness: Some(0.8),
            ..DisplayOverrides::default()
        };

        let base = global.apply_to(measured);
        let resolved = per_game.apply_to(base);

        assert_eq!(resolved.brightness, 0.8, "the game's own wins");
        assert_eq!(
            resolved.focus, 1.5,
            "the global shows through where it is silent"
        );
        assert_eq!(
            resolved.halation, measured.halation,
            "and the measured figure where both are"
        );

        // Recorded against what it was resolved from, so the game only carries
        // its own departure and not the global preference as well.
        let recorded = DisplayOverrides::diff(resolved, base);
        assert_eq!(recorded.brightness, Some(0.8));
        assert_eq!(recorded.focus, None);
        assert_eq!(recorded.halation, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(control: &str, input: &str) -> SerializedBinding {
        SerializedBinding {
            control: control.to_string(),
            input: input.to_string(),
            scale: None,
            deadzone: None,
        }
    }

    /// A legacy state.toml (top-level maps keyed by machine_id) folds into the
    /// unified `machines` map on load and stops re-serializing the old keys.
    #[test]
    fn migrates_legacy_top_level_maps() {
        let legacy = r#"
            window_x = 10
            window_y = 20

            [input_bindings]
            joust = [{ control = "p1_fire", input = "Key:Space" }]

            [dip_switches]
            joust = [3]
            pacman = [255]
        "#;
        let mut state: State = toml::from_str(legacy).unwrap();
        state.migrate();

        assert!(state.legacy_input_bindings.is_empty());
        assert!(state.legacy_dip_switches.is_empty());

        let joust = state.machine("joust").expect("joust entry");
        assert_eq!(joust.dip_switches, vec![3]);
        assert_eq!(joust.input_bindings, vec![binding("p1_fire", "Key:Space")]);
        assert_eq!(state.machine("pacman").unwrap().dip_switches, vec![255]);

        // Re-serialized output uses the unified section, not the legacy maps.
        let out = toml::to_string_pretty(&state).unwrap();
        assert!(out.contains("[machines.joust]"));
        assert!(!out.contains("[input_bindings]"));
        assert!(!out.contains("[dip_switches]"));
    }

    /// Empty legacy entries do not create empty `machines` entries.
    #[test]
    fn migration_skips_empty_legacy_entries() {
        let legacy = r#"
            [input_bindings]
            joust = []
            [dip_switches]
            pacman = []
        "#;
        let mut state: State = toml::from_str(legacy).unwrap();
        state.migrate();
        assert!(state.machines.is_empty());
    }

    /// `set_machine` drops entries that carry no overrides.
    #[test]
    fn set_machine_drops_empty_entries() {
        let mut state = State::default();
        state.set_machine("joust", MachineSettings::default());
        assert!(state.machine("joust").is_none());

        let settings = MachineSettings {
            dip_switches: vec![1],
            ..MachineSettings::default()
        };
        state.set_machine("joust", settings);
        assert!(state.machine("joust").is_some());

        // Clearing the diff removes the entry again.
        state.set_machine("joust", MachineSettings::default());
        assert!(state.machine("joust").is_none());
    }

    /// Diff-only fields are omitted from the serialized form.
    #[test]
    fn diff_only_serialization_omits_defaults() {
        let mut state = State::default();
        let settings = MachineSettings {
            scale: Some(3),
            ..MachineSettings::default()
        };
        state.set_machine("joust", settings);

        let out = toml::to_string_pretty(&state).unwrap();
        assert!(out.contains("scale = 3"));
        assert!(!out.contains("rom_path"));
        assert!(!out.contains("dip_switches"));
        assert!(!out.contains("input_bindings"));

        // Round-trips back to the same value.
        let reloaded: State = toml::from_str(&out).unwrap();
        assert_eq!(reloaded.machine("joust").unwrap().scale, Some(3));
    }
}
