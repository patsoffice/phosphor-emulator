//! Shared headless boot + frame-stepping harness.
//!
//! Both the `frameshot` (video capture) and `trace` (CPU/bus observation)
//! subcommands boot a registered machine the same way: resolve it in the
//! registry, load its ROM set, optionally load a factory NVRAM, reset, and
//! then step frames while scripting input (coin insert, and — via `--press`
//! — any control by stable name, e.g. `fire1` to start a game). This module
//! owns that shared machinery so the two subcommands don't fork machine
//! construction.
//!
//! The harness only advances *frames* (`run_frame`); the cycle-granular
//! instruction-trace loop drives the same [`Harness::machine_mut`] via
//! `debug_tick`.

use std::path::Path;

use phosphor_core::core::machine::{FrontendMachine, InputEvent, InputId};
use phosphor_machines::registry;

use crate::load_rom_set;

/// Default frames to hold a scripted input down (coin / `--press` pulse).
const DEFAULT_HOLD: usize = 8;

/// A requested input pulse: hold `control` (by stable name) down for `hold`
/// frames starting at frame `at`.
pub struct PressSpec {
    pub control: String,
    pub at: usize,
    pub hold: usize,
}

/// A booted machine plus its input-scripting and frame-accounting state.
///
/// Construct with [`Harness::build`], advance with [`Harness::run_frame`],
/// and reach the underlying machine (for rendering, audio, NVRAM, or the
/// debug traits) via [`Harness::machine_mut`].
pub struct Harness {
    machine: Box<dyn FrontendMachine>,
    presses: Vec<ScheduledPress>,
    /// Number of frames run so far (also the index of the next frame).
    frame: usize,
}

/// A resolved input pulse: press `id` at frame `at`, release at `release`.
struct ScheduledPress {
    id: InputId,
    at: usize,
    release: usize,
}

impl Harness {
    /// Boot `machine` from the ROM set at `path`.
    ///
    /// Mirrors the original `run_frameshot` boot sequence: registry lookup →
    /// ROM load → create → reset → optional NVRAM load → resolve scripted
    /// inputs (`--coin-at` is sugar for a `coin` press; `presses` are the
    /// generic `--press` pulses) against the machine's control table.
    pub fn build(
        machine: &str,
        path: &str,
        nvram: Option<&Path>,
        coin_at: Option<usize>,
        presses: &[PressSpec],
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

        // Resolve every scripted input to its InputId by stable name. `--coin-at`
        // is just a `coin` press with the default hold.
        let resolve = |name: &str| -> Result<InputId, String> {
            machine_box
                .input_controls()
                .iter()
                .find(|c| c.stable_name == name)
                .map(|c| c.id)
                .ok_or_else(|| format!("machine '{machine}' has no '{name}' input control"))
        };

        let mut scheduled = Vec::new();
        if let Some(at) = coin_at {
            scheduled.push(ScheduledPress {
                id: resolve("coin")?,
                at,
                release: at + DEFAULT_HOLD,
            });
        }
        for p in presses {
            scheduled.push(ScheduledPress {
                id: resolve(&p.control)?,
                at: p.at,
                release: p.at + p.hold.max(1),
            });
        }

        Ok(Self {
            machine: machine_box,
            presses: scheduled,
            frame: 0,
        })
    }

    /// Advance the machine by one frame, applying any scripted input edges for
    /// the frame that is about to run.
    pub fn run_frame(&mut self) {
        for p in &self.presses {
            if self.frame == p.at {
                self.machine.handle_input(InputEvent::Button {
                    id: p.id,
                    pressed: true,
                });
            } else if self.frame == p.release {
                self.machine.handle_input(InputEvent::Button {
                    id: p.id,
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

    /// Shared access to the booted machine (for the `&self` inspection
    /// accessors — `display_size`, `machine_id`, and other side-effect-free
    /// reads).
    pub fn machine(&self) -> &dyn FrontendMachine {
        &*self.machine
    }

    /// Number of frames run so far via [`run_frame`](Self::run_frame).
    pub fn frame_count(&self) -> usize {
        self.frame
    }

    /// Wrap an already-constructed machine, with no scheduled presses.
    ///
    /// [`build`](Self::build) is the normal entry point (registry → ROM load →
    /// create → reset). This constructor is for callers that already hold a
    /// booted machine: unit tests that inject a stub, and the deferred
    /// in-frontend console that binds the *live* machine.
    pub fn from_machine(machine: Box<dyn FrontendMachine>) -> Self {
        Self {
            machine,
            presses: Vec::new(),
            frame: 0,
        }
    }
}
