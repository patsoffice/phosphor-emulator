//! Shared headless boot + frame-stepping harness.
//!
//! Both the `frameshot` (video capture) and `trace` (CPU/bus observation)
//! subcommands boot a registered machine the same way: resolve it in the
//! registry, load its ROM set, optionally load a factory NVRAM, reset, and
//! then step frames while scripting the coin input. This module owns that
//! shared machinery so the two subcommands don't fork machine construction.
//!
//! The harness only advances *frames* (`run_frame`); the cycle-granular
//! instruction-trace loop (Feature 2) will live alongside it and drive the
//! same [`Harness::machine_mut`] via `debug_tick`.

use std::path::Path;

use phosphor_core::core::machine::{FrontendMachine, InputEvent, InputId};
use phosphor_machines::registry;

use crate::load_rom_set;

/// Frames to hold the coin input down after a `--coin-at` pulse.
const COIN_HOLD: usize = 8;

/// A booted machine plus its coin-scripting and frame-accounting state.
///
/// Construct with [`Harness::build`], advance with [`Harness::run_frame`],
/// and reach the underlying machine (for rendering, audio, NVRAM, or the
/// debug traits) via [`Harness::machine`] / [`Harness::machine_mut`].
pub struct Harness {
    machine: Box<dyn FrontendMachine>,
    coin: Option<CoinScript>,
    /// Number of frames run so far (also the index of the next frame).
    frame: usize,
}

/// A scheduled single coin pulse: press at `at`, release at `at + COIN_HOLD`.
struct CoinScript {
    id: InputId,
    at: usize,
}

impl Harness {
    /// Boot `machine` from the ROM set at `path`.
    ///
    /// Mirrors the original `run_frameshot` boot sequence: registry lookup →
    /// ROM load → create → reset → optional NVRAM load → resolve the coin
    /// control when `--coin-at` is requested.
    pub fn build(
        machine: &str,
        path: &str,
        nvram: Option<&Path>,
        coin_at: Option<usize>,
    ) -> Result<Self, String> {
        let entry = registry::find(machine).ok_or_else(|| {
            let avail: Vec<&str> = registry::all().iter().map(|e| e.name).collect();
            format!(
                "unknown machine '{machine}'; available: {}",
                avail.join(", ")
            )
        })?;

        let set = load_rom_set(path, entry.rom_names)
            .map_err(|e| format!("loading ROM set {path}: {e}"))?;
        let mut machine_box =
            (entry.create)(&set).map_err(|e| format!("creating machine '{machine}': {e}"))?;

        machine_box.reset();

        // Load a factory-initialized NVRAM so the game skips its self-test.
        if let Some(nv) = nvram {
            let data =
                std::fs::read(nv).map_err(|e| format!("reading nvram {}: {e}", nv.display()))?;
            machine_box.load_nvram(&data);
        }

        // Resolve the coin control (by stable name) for --coin-at.
        let coin = match coin_at {
            Some(at) => {
                let id = machine_box
                    .input_controls()
                    .iter()
                    .find(|c| c.stable_name == "coin")
                    .map(|c| c.id)
                    .ok_or_else(|| format!("machine '{machine}' has no 'coin' input control"))?;
                Some(CoinScript { id, at })
            }
            None => None,
        };

        Ok(Self {
            machine: machine_box,
            coin,
            frame: 0,
        })
    }

    /// Advance the machine by one frame, applying any scripted coin edges for
    /// the frame that is about to run.
    pub fn run_frame(&mut self) {
        if let Some(coin) = &self.coin {
            if self.frame == coin.at {
                self.machine.handle_input(InputEvent::Button {
                    id: coin.id,
                    pressed: true,
                });
            } else if self.frame == coin.at + COIN_HOLD {
                self.machine.handle_input(InputEvent::Button {
                    id: coin.id,
                    pressed: false,
                });
            }
        }
        self.machine.run_frame();
        self.frame += 1;
    }

    /// Mutable access to the booted machine (for rendering, audio draining,
    /// NVRAM dump, and the debug traits).
    pub fn machine_mut(&mut self) -> &mut dyn FrontendMachine {
        &mut *self.machine
    }
}
