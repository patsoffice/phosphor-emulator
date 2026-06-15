//! Persistent configuration loaded from `~/.config/phosphor/config.toml`.
//!
//! Supported keys:
//! - `rom_path` — default directory to search for ROM files
//! - `nvram_path` — directory for battery-backed NVRAM files
//! - `save_path` — directory for save state files
//! - `scale` — default window scale factor
//!
//! These are the *global* defaults. Each field can be overridden per game via a
//! machine's `MachineSettings` in `state.toml`; the resolution order applied in
//! `main.rs` is CLI arg > per-game override > this global config > built-in
//! default.

use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    pub rom_path: Option<String>,
    pub nvram_path: Option<String>,
    pub save_path: Option<String>,
    pub scale: Option<u32>,
}

/// Return the platform config directory: `~/.config/phosphor` (macOS/Linux).
pub fn config_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|d| d.join(".config/phosphor"))
}

/// Load config from `~/.config/phosphor/config.toml`, falling back to defaults.
pub fn load() -> Config {
    let Some(path) = config_dir().map(|d| d.join("config.toml")) else {
        return Config::default();
    };
    match std::fs::read_to_string(&path) {
        Ok(contents) => toml::from_str(&contents).unwrap_or_else(|e| {
            eprintln!("Warning: invalid config at {}: {e}", path.display());
            Config::default()
        }),
        Err(_) => Config::default(),
    }
}
