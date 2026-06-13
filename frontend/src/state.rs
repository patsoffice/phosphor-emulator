//! Auto-saved emulator state persisted across sessions.
//!
//! Stored at `~/.config/phosphor/state.toml`. Unlike `config.toml` (user-edited),
//! this file is written automatically on exit.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::input::SerializedBinding;

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    pub window_x: Option<i32>,
    pub window_y: Option<i32>,
    /// Per-machine input bindings, keyed by `machine_id`. Only machines with
    /// typed controls (stable names) appear here; absent means "use defaults".
    #[serde(default)]
    pub input_bindings: HashMap<String, Vec<SerializedBinding>>,
}

/// Load state from `~/.config/phosphor/state.toml`, falling back to defaults.
pub fn load() -> State {
    let Some(path) = crate::config::config_dir().map(|d| d.join("state.toml")) else {
        return State::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(contents) => toml::from_str(&contents).unwrap_or_default(),
        Err(_) => State::default(),
    }
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
