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

/// A requested sustained motion: feed `delta` to `control` (by stable name)
/// once per frame for `frames` frames starting at frame `at`.
///
/// The per-frame shape is deliberate. Trackball and spinner machines drain a
/// bounded amount of accumulated motion per frame (or per cycle-divider tick),
/// so one large delta is not equivalent to the same total spread over several
/// frames — the machine clamps, and depending on the game either carries the
/// remainder or discards it.
pub struct MotionSpec {
    pub control: String,
    pub at: usize,
    pub frames: usize,
    pub delta: f32,
}

/// A booted machine plus its input-scripting and frame-accounting state.
///
/// Construct with [`Harness::build`], advance with [`Harness::run_frame`],
/// and reach the underlying machine (for rendering, audio, NVRAM, or the
/// debug traits) via [`Harness::machine_mut`].
pub struct Harness {
    machine: Box<dyn FrontendMachine>,
    presses: Vec<ScheduledPress>,
    motions: Vec<ScheduledMotion>,
    /// Number of frames run so far (also the index of the next frame).
    frame: usize,
}

/// A resolved input pulse: press `id` at frame `at`, release at `release`.
struct ScheduledPress {
    id: InputId,
    at: usize,
    release: usize,
}

/// A resolved sustained motion: feed `delta` to `id` on every frame in
/// `at..until`.
struct ScheduledMotion {
    id: InputId,
    at: usize,
    until: usize,
    delta: f32,
}

impl ScheduledMotion {
    /// Whether this motion feeds a delta on `frame`. Half-open: the motion
    /// fires on `at` and not on `until`, so `frames` in the spec is exactly the
    /// number of deltas emitted.
    fn active_on(&self, frame: usize) -> bool {
        frame >= self.at && frame < self.until
    }
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
        motions: &[MotionSpec],
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

        let mut scheduled_motions = Vec::new();
        for m in motions {
            scheduled_motions.push(ScheduledMotion {
                id: resolve(&m.control)?,
                at: m.at,
                until: m.at + m.frames,
                delta: m.delta,
            });
        }

        Ok(Self {
            machine: machine_box,
            presses: scheduled,
            motions: scheduled_motions,
            frame: 0,
        })
    }

    /// Schedule sustained relative motion on an already-resolved control.
    ///
    /// [`build`](Self::build) is the normal path (it resolves stable names for
    /// you); this is the seam for callers holding a machine constructed via
    /// [`from_machine`](Self::from_machine), which has no control table to
    /// resolve against at construction time.
    pub fn schedule_motion(&mut self, id: InputId, at: usize, frames: usize, delta: f32) {
        self.motions.push(ScheduledMotion {
            id,
            at,
            until: at + frames,
            delta,
        });
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
        for m in &self.motions {
            if m.active_on(self.frame) {
                self.machine.handle_input(InputEvent::Relative {
                    id: m.id,
                    delta: m.delta,
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

    /// Reset the machine to its power-on state and zero the frame counter.
    /// Scheduled presses and motions are left intact (they fire relative to
    /// frame 0 again).
    pub fn reset(&mut self) {
        self.machine.reset();
        self.frame = 0;
    }

    /// Wrap an already-constructed machine, with no scheduled input.
    ///
    /// [`build`](Self::build) is the normal entry point (registry → ROM load →
    /// create → reset). This constructor is for callers that already hold a
    /// booted machine: unit tests that inject a stub, and the in-frontend
    /// console that binds the *live* machine.
    pub fn from_machine(machine: Box<dyn FrontendMachine>) -> Self {
        Self {
            machine,
            presses: Vec::new(),
            motions: Vec::new(),
            frame: 0,
        }
    }

    /// Consume the harness and return the wrapped machine — the seam the
    /// frontend uses to reclaim its machine after driving it through a session.
    pub fn into_machine(self) -> Box<dyn FrontendMachine> {
        self.machine
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn motion(at: usize, frames: usize) -> ScheduledMotion {
        ScheduledMotion {
            id: InputId(0),
            at,
            until: at + frames,
            delta: 1.0,
        }
    }

    #[test]
    fn motion_is_active_for_exactly_frames_starting_at_at() {
        let m = motion(2, 3);
        let active: Vec<usize> = (0..8).filter(|&f| m.active_on(f)).collect();
        assert_eq!(active, vec![2, 3, 4]);
    }

    #[test]
    fn zero_frame_motion_never_fires() {
        let m = motion(2, 0);
        assert!((0..8).all(|f| !m.active_on(f)));
    }
}
