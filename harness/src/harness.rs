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
use phosphor_machines::rom_loader::RomSet;

use crate::load_rom_set;
use crate::movie::{Movie, MovieError, MoviePlayer, rom_digest};

/// Default frames to hold a scripted input down (coin / `--press` pulse).
const DEFAULT_HOLD: usize = 8;

/// Load the first dump in `entry.rom_names` that this machine can actually be
/// built from, and hand back both the set and the built machine.
///
/// **The machine is the judge, not the filesystem.** `load_rom_set` pointed at a
/// directory returns the first name with an archive present, which is not the
/// same question as which archive satisfies the machine's ROM entries. Donkey
/// Kong Jr. is the case that found this: it declares `["dkongjr", "dkongjr2"]`,
/// its entries name the members of the *dkongjr2* dump (`0`, `1`, `2`, `8`, `9`,
/// `10`, `v_7c.bin`, …), and a collection holding both archives handed it
/// `dkongjr.zip`, whose members are named `djr1-c-2e.2e` and friends. Nothing
/// matched, construction failed, and every ROM-gated suite reported the machine
/// as having no ROM set at all while a working dump sat beside it.
///
/// Six machines declare more than one name, so this is not one game's problem;
/// it is decided by which archives a given collection happens to hold.
///
/// The first candidate's error is the one reported, because it is the dump the
/// old behaviour would have chosen and so the one a reader is most likely asking
/// about. Trying a candidate costs decompressing it, which is why this stops at
/// the first success rather than scoring them all.
fn load_set_the_machine_accepts(
    entry: &registry::MachineEntry,
    path: &str,
) -> Result<(RomSet, Box<dyn FrontendMachine>), String> {
    // Pointed straight at an archive there is nothing to choose between.
    let single = path.to_ascii_lowercase().ends_with(".zip") || entry.rom_names.len() < 2;
    if single {
        let set = load_rom_set(path, entry.rom_names)
            .map_err(|e| format!("loading ROM set {path}: {e}"))?;
        let machine =
            (entry.create)(&set).map_err(|e| format!("creating machine '{}': {e}", entry.name))?;
        return Ok((set, machine));
    }

    let mut first_error = None;
    for name in entry.rom_names {
        if !Path::new(path).join(format!("{name}.zip")).exists() {
            continue;
        }
        let set = match load_rom_set(path, std::slice::from_ref(name)) {
            Ok(set) => set,
            Err(e) => {
                first_error.get_or_insert(format!("loading ROM set {name}.zip: {e}"));
                continue;
            }
        };
        match (entry.create)(&set) {
            Ok(machine) => return Ok((set, machine)),
            Err(e) => {
                first_error.get_or_insert(format!("creating machine '{}': {e}", entry.name));
            }
        }
    }

    // No candidate archive worked. Fall back so a loose-file directory, which
    // names no archive at all, still resolves the way it always has.
    match first_error {
        Some(e) => Err(e),
        None => {
            let set = load_rom_set(path, entry.rom_names)
                .map_err(|e| format!("loading ROM set {path}: {e}"))?;
            let machine = (entry.create)(&set)
                .map_err(|e| format!("creating machine '{}': {e}", entry.name))?;
            Ok((set, machine))
        }
    }
}

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
    /// A bound input movie, when this harness is replaying one. Independent of
    /// `presses`/`motions`: a movie is a recorded trace, those are CLI sugar,
    /// and nothing stops a caller using both.
    movie: Option<MoviePlayer>,
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

        let (_set, mut machine_box) = load_set_the_machine_accepts(entry, path)?;

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
            movie: None,
            frame: 0,
        })
    }

    /// Boot the machine an input movie was recorded against, and bind the movie
    /// for replay.
    ///
    /// The machine name comes from the movie, not the caller — a movie knows
    /// what it was recorded against, and letting the caller assert a different
    /// one would only create a way to be wrong.
    pub fn build_with_movie(roms_path: &str, movie_path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(movie_path)
            .map_err(|e| format!("reading movie {}: {e}", movie_path.display()))?;
        let movie = Movie::decode(&bytes)
            .map_err(|e| format!("reading movie {}: {e}", movie_path.display()))?;
        Self::from_movie(roms_path, movie)
    }

    /// Boot and bind an already-decoded movie. The seam tests use to replay a
    /// movie they just recorded, without a round trip through the filesystem.
    ///
    /// Reconstructs the recording's starting conditions in the order they were
    /// established: the host sample rate is set *before* the machine is built
    /// (devices derive their resampler ratios at construction, so setting it
    /// afterwards would leave the chips disagreeing), then reset, NVRAM, and the
    /// power-on DIP bytes.
    pub fn from_movie(roms_path: &str, movie: Movie) -> Result<Self, String> {
        let name = movie.header.machine.clone();
        let entry = registry::find(&name).ok_or_else(|| {
            let avail: Vec<&str> = registry::all().iter().map(|e| e.name).collect();
            format!(
                "movie names unknown machine '{name}'; available: {}",
                avail.join(", ")
            )
        })?;

        phosphor_core::audio::set_host_sample_rate(movie.header.host_sample_rate);

        let (set, mut machine_box) = load_set_the_machine_accepts(entry, roms_path)?;

        // A movie replayed against a different dump boots fine and then diverges
        // silently, so this check is the difference between a clear error and a
        // golden hash that moved for no visible reason.
        //
        // Report both digests and how this build chose its dump. Pointed at a
        // directory, more than one of `rom_names` can have an archive there, so
        // a collection holding several dumps of a game can legitimately have
        // handed the recording and this replay different ones, and naming the
        // order is what makes that diagnosable. Pointed straight at an archive
        // it consults no names at all, and saying it "tried" any would be a
        // fabrication.
        let actual = rom_digest(&set);
        if actual != movie.header.rom_digest {
            let chose = if roms_path.to_ascii_lowercase().ends_with(".zip") {
                String::new()
            } else {
                format!(
                    " (of {}, the first in {roms_path} this machine accepted was used)",
                    entry.rom_names.join(", ")
                )
            };
            return Err(format!(
                "{}: movie expects {}, this build computes {} for '{name}'{chose}. \
                 This digest covers the member files of the dump that was loaded, so a \
                 mismatch means the bytes differ: a different revision of the set, or \
                 a different archive of the same game.",
                MovieError::RomMismatch,
                crate::movie::hex(&movie.header.rom_digest),
                crate::movie::hex(&actual),
            ));
        }

        machine_box.reset();

        if let Some(nv) = &movie.header.nvram {
            machine_box.load_nvram(nv);
        }
        for (bank, &value) in movie.header.dip.iter().enumerate() {
            machine_box.set_dip_bank_value(bank, value);
        }

        let player = MoviePlayer::bind(movie, machine_box.input_controls())
            .map_err(|e| format!("binding movie to '{name}': {e}"))?;

        Ok(Self {
            machine: machine_box,
            presses: Vec::new(),
            motions: Vec::new(),
            movie: Some(player),
            frame: 0,
        })
    }

    /// The bound movie, if this harness is replaying one.
    pub fn movie(&self) -> Option<&MoviePlayer> {
        self.movie.as_ref()
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
        self.apply_scheduled_input();
        self.machine.run_frame();
        self.advance_frame();
    }

    /// Apply the scripted input edges scheduled for the frame that is about to
    /// run.
    ///
    /// [`run_frame`](Self::run_frame) calls this and [`advance_frame`] for you.
    /// The pair is public for callers that drive the machine cycle-by-cycle
    /// (`debug_tick`) rather than frame-by-frame, and so cannot use
    /// `run_frame`: without them a scripted press silently stops firing the
    /// moment such a caller takes over, which reads as "the input did nothing"
    /// rather than as a loop mismatch.
    ///
    /// [`advance_frame`]: Self::advance_frame
    pub fn apply_scheduled_input(&mut self) {
        self.apply_movie_input();
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
    }

    /// Deliver the bound movie's records for the frame about to run, in the
    /// order they were recorded.
    ///
    /// Order is load-bearing twice over. Across records, because a press and its
    /// release in the same frame must arrive that way round. Within analog
    /// records, because each one truncates independently inside the machine —
    /// which is why a movie stores every delta rather than a per-frame sum.
    fn apply_movie_input(&mut self) {
        let Some(player) = &mut self.movie else {
            return;
        };
        player.deliver(&mut *self.machine, self.frame as u32);
    }

    /// Record that the frame just run is complete, advancing the frame number
    /// the scheduled input is measured against.
    pub fn advance_frame(&mut self) {
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
            movie: None,
            frame: 0,
        }
    }

    /// Bind a movie to an already-wrapped machine, restoring the starting
    /// conditions the harness can still reach.
    ///
    /// [`from_movie`](Self::from_movie) is the normal path: it boots the machine
    /// the movie names, so it also owns the two conditions that must be
    /// established *before* construction — the host sample rate and the ROM set.
    /// This is the seam for callers holding a machine they built themselves,
    /// notably the ROM-less registry-driven tests.
    ///
    /// It still applies the movie's NVRAM and power-on DIP bytes. Leaving those
    /// to the caller would make the two entry points silently disagree about
    /// what "bound" means, and a DIP the replay did not restore diverges in a
    /// way that looks like a movie bug rather than a missing step.
    pub fn bind_movie(&mut self, movie: Movie) -> Result<(), MovieError> {
        if let Some(nv) = &movie.header.nvram {
            self.machine.load_nvram(nv);
        }
        for (bank, &value) in movie.header.dip.iter().enumerate() {
            self.machine.set_dip_bank_value(bank, value);
        }
        self.movie = Some(MoviePlayer::bind(movie, self.machine.input_controls())?);
        Ok(())
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
