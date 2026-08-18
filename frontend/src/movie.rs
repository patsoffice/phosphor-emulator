//! Capturing a play session as an input movie.
//!
//! # The recording seam
//!
//! Every input a machine receives arrives through `InputConfigurable`, and the
//! frontend touches it from exactly two places — [`input::dispatch`] and
//! [`input::resync`], both generic over `M: InputConfigurable + ?Sized`. So the
//! recorder is a *tee wrapper* rather than a subscriber: [`Recording`] forwards
//! each call to the machine and appends it to a [`MovieRecorder`] on the way
//! past.
//!
//! That shape matters. Subscribing to binding resolution instead would miss
//! `resync`, which emits real events after a reset or state load with no
//! corresponding SDL event, and would not see `release_all_inputs` at all. With
//! the wrapper, no future input path can be recorded incompletely without also
//! bypassing the trait.
//!
//! [`input::dispatch`]: crate::input::dispatch
//! [`input::resync`]: crate::input::resync
//!
//! # Why arming resets
//!
//! A movie carries no save state, so it can only be replayed from power-on.
//! Arming therefore resets the machine rather than capturing from wherever the
//! session happens to be. The reset sequence deliberately mirrors
//! `Harness::from_movie`'s boot order — reset, then NVRAM, then DIP — so replay
//! reconstructs exactly what recording started from.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use phosphor_core::core::machine::{FrontendMachine, InputConfigurable, InputControl, InputEvent};
use phosphor_harness::movie::{MoviePlayer, MovieRecorder};

/// Tees every `InputConfigurable` call into a recorder before forwarding it.
pub struct Recording<'a, M: ?Sized> {
    inner: &'a mut M,
    sink: &'a mut MovieRecorder,
}

impl<'a, M: InputConfigurable + ?Sized> Recording<'a, M> {
    pub fn new(inner: &'a mut M, sink: &'a mut MovieRecorder) -> Self {
        Self { inner, sink }
    }
}

impl<M: InputConfigurable + ?Sized> InputConfigurable for Recording<'_, M> {
    fn input_controls(&self) -> &'static [InputControl] {
        self.inner.input_controls()
    }

    fn handle_input(&mut self, event: InputEvent) {
        self.sink.push_event(event);
        self.inner.handle_input(event);
    }

    fn release_all_inputs(&mut self) {
        // Recorded whole rather than expanded into a release per control:
        // machines with conditioned analog state override this to clear
        // trackball accumulators a per-control loop would not touch.
        self.sink.push_release_all();
        self.inner.release_all_inputs();
    }
}

/// Owns the in-progress recording and where finished movies are written.
pub struct MovieCapture {
    dir: PathBuf,
    machine_name: String,
    rom_digest: [u8; 32],
    recorder: Option<MovieRecorder>,
}

impl MovieCapture {
    pub fn new(dir: &Path, machine_name: &str, rom_digest: [u8; 32]) -> Self {
        Self {
            dir: dir.to_path_buf(),
            machine_name: machine_name.to_string(),
            rom_digest,
            recorder: None,
        }
    }

    pub fn is_recording(&self) -> bool {
        self.recorder.is_some()
    }

    /// The live recorder, for wrapping a machine in [`Recording`].
    pub fn recorder_mut(&mut self) -> Option<&mut MovieRecorder> {
        self.recorder.as_mut()
    }

    /// The starting conditions a recording must be armed on: the NVRAM and DIP
    /// bytes the live session had, carried across to a freshly built machine.
    pub fn starting_conditions(machine: &dyn FrontendMachine) -> (Option<Vec<u8>>, Vec<u8>) {
        (
            machine.save_nvram().map(<[u8]>::to_vec),
            (0..machine.dip_banks().len())
                .map(|b| machine.dip_bank_value(b))
                .collect(),
        )
    }

    /// Start recording against a machine that was just built from ROM.
    ///
    /// `machine` MUST be freshly constructed, not the live one reset in place.
    /// `reset()` is a reset button, not a power cycle — state survives it, and
    /// measurably so: after 600 frames of attract mode and a reset, every
    /// machine sampled differs from a fresh build (burgertime by 1449 state
    /// bytes and 28134 rendered pixels). Arming on a reset live machine
    /// therefore starts the recording somewhere `Harness::from_movie` — which
    /// builds from ROM — can never reconstruct, and the replay diverges.
    ///
    /// The order below mirrors `from_movie` exactly: reset, NVRAM, DIP.
    pub fn arm_fresh(
        &mut self,
        machine: &mut dyn FrontendMachine,
        nvram: Option<Vec<u8>>,
        dip: Vec<u8>,
    ) {
        machine.reset();
        if let Some(nv) = &nvram {
            machine.load_nvram(nv);
        }
        for (bank, &value) in dip.iter().enumerate() {
            machine.set_dip_bank_value(bank, value);
        }
        self.recorder = Some(MovieRecorder::new(
            self.machine_name.clone(),
            self.rom_digest,
            machine.input_controls(),
            dip,
            nvram,
        ));
    }

    /// Note that a whole frame completed.
    ///
    /// Must only be called for frames that actually ran. Under the debugger a
    /// "frame" may be a handful of stepped cycles, and counting those would slide
    /// every later record one frame early on replay.
    pub fn advance_frame(&mut self) {
        if let Some(rec) = &mut self.recorder {
            rec.advance_frame();
        }
    }

    /// Record a mid-session DIP change, so replay applies it at the same frame.
    pub fn push_dip(&mut self, bank: u8, value: u8) {
        if let Some(rec) = &mut self.recorder {
            rec.push_dip(bank, value);
        }
    }

    /// Stop recording and write the movie, returning a message for the user.
    ///
    /// Written atomically via a temporary file and a rename, so an interrupted
    /// write cannot leave a half-file that decodes as a short session.
    pub fn stop(&mut self) -> String {
        let Some(rec) = self.recorder.take() else {
            return "Not recording".to_string();
        };
        let frames = rec.frame();
        let records = rec.len();
        let unmapped = rec.unmapped();
        let movie = rec.finish();

        if let Err(e) = std::fs::create_dir_all(&self.dir) {
            return format!("Movie: cannot create {}: {e}", self.dir.display());
        }
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = self.dir.join(format!("{}-{stamp}.phmi", self.machine_name));
        let tmp = path.with_extension("phmi.tmp");

        if let Err(e) = std::fs::write(&tmp, movie.encode()) {
            return format!("Movie: writing {}: {e}", tmp.display());
        }
        if let Err(e) = std::fs::rename(&tmp, &path) {
            return format!("Movie: renaming into {}: {e}", path.display());
        }

        let mut msg = format!(
            "Movie: wrote {frames} frame(s), {records} record(s) -> {}",
            path.display()
        );
        if unmapped > 0 {
            // Should never happen: every event the frontend dispatches targets a
            // control from the machine's own table. If it does, those inputs are
            // missing from the movie and it will not replay faithfully.
            msg.push_str(&format!(
                " (WARNING: {unmapped} event(s) targeted controls outside the \
                 machine's table and were not recorded)"
            ));
        }
        msg
    }

    /// Message shown when a recording has just been armed.
    pub fn armed_message(&self) -> String {
        format!(
            "Movie: recording {} from power-on (press again to stop)",
            self.machine_name
        )
    }
}

// ---------------------------------------------------------------------------
// Playback
// ---------------------------------------------------------------------------

/// A movie being replayed in the live window.
///
/// Playback state is held here rather than in the session's `Harness` because
/// the frame loop borrows the machine *out* of the session and so cannot reach
/// back into it mid-frame. `MoviePlayer::deliver` is the shared body, so the
/// harness and the frontend agree on delivery order without duplicating it.
pub struct MoviePlayback {
    player: MoviePlayer,
    frame: u32,
    total: u32,
}

impl MoviePlayback {
    /// Bind a decoded movie to a machine that is about to be reset to power-on.
    ///
    /// Reproduces the same starting conditions `Harness::from_movie` does, minus
    /// the two it cannot reach from here — the host sample rate and the ROM set,
    /// both fixed when the frontend built the machine. The caller must have
    /// verified the ROM digest before getting here.
    pub fn bind(
        movie: phosphor_harness::Movie,
        machine: &mut dyn FrontendMachine,
    ) -> Result<Self, phosphor_harness::MovieError> {
        let total = movie.header.frames;
        machine.reset();
        if let Some(nv) = &movie.header.nvram {
            machine.load_nvram(nv);
        }
        for (bank, &value) in movie.header.dip.iter().enumerate() {
            machine.set_dip_bank_value(bank, value);
        }
        let player = MoviePlayer::bind(movie, machine.input_controls())?;
        Ok(Self {
            player,
            frame: 0,
            total,
        })
    }

    /// Deliver this frame's recorded input, immediately before the frame runs.
    pub fn deliver(&mut self, machine: &mut dyn FrontendMachine) {
        self.player.deliver(machine, self.frame);
    }

    /// Note that a whole frame ran. Only whole frames count, for the same reason
    /// recording only counts them: the debugger can step cycles instead, and
    /// counting those would slide the rest of the movie one frame early.
    pub fn advance_frame(&mut self) {
        self.frame += 1;
    }

    /// `(frame, total)` for the overlay — the number a golden pin's `frames`
    /// field wants, read off while watching rather than guessed and re-rendered.
    pub fn progress(&self) -> (u32, u32) {
        (self.frame, self.total)
    }

    /// Whether playback has passed the movie's last recorded frame. The machine
    /// keeps running; it simply receives no further input.
    pub fn finished(&self) -> bool {
        self.frame >= self.total
    }
}

/// Load a movie for playback against an already-built machine, checking it
/// belongs here before anything is reset.
///
/// Both checks matter and fail differently. A movie for another machine would
/// bind against a control table it was never recorded against; a movie for the
/// same machine but a different ROM dump would bind fine and then silently
/// diverge, which is the failure the digest exists to turn into a message.
pub fn load_for_playback(
    path: &Path,
    machine_name: &str,
    rom_digest: [u8; 32],
) -> Result<phosphor_harness::Movie, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("reading movie {}: {e}", path.display()))?;
    let movie = phosphor_harness::Movie::decode(&bytes)
        .map_err(|e| format!("reading movie {}: {e}", path.display()))?;
    if movie.header.machine != machine_name {
        return Err(format!(
            "movie {} was recorded for '{}', but this session is running '{machine_name}'",
            path.display(),
            movie.header.machine
        ));
    }
    if movie.header.rom_digest != rom_digest {
        return Err(format!(
            "movie {} was recorded against a different ROM set than the one loaded",
            path.display()
        ));
    }
    Ok(movie)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::{InputId, InputKind};

    const CONTROLS: &[InputControl] = &[InputControl {
        id: InputId(3),
        stable_name: "coin",
        label: "Coin",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[],
    }];

    /// A stand-in machine that records what it was told, so the tee can be
    /// checked to forward as well as capture.
    #[derive(Default)]
    struct Spy {
        events: Vec<InputEvent>,
        releases: usize,
    }

    impl InputConfigurable for Spy {
        fn input_controls(&self) -> &'static [InputControl] {
            CONTROLS
        }
        fn handle_input(&mut self, event: InputEvent) {
            self.events.push(event);
        }
        fn release_all_inputs(&mut self) {
            self.releases += 1;
        }
    }

    #[test]
    fn the_tee_forwards_and_records_every_call() {
        let mut spy = Spy::default();
        let mut rec = MovieRecorder::new("t", [0; 32], CONTROLS, Vec::new(), None);
        {
            let mut tee = Recording::new(&mut spy, &mut rec);
            tee.handle_input(InputEvent::Button {
                id: InputId(3),
                pressed: true,
            });
            tee.release_all_inputs();
            tee.handle_input(InputEvent::Button {
                id: InputId(3),
                pressed: false,
            });
        }

        // Forwarded to the machine...
        assert_eq!(spy.events.len(), 2);
        assert_eq!(spy.releases, 1);
        // ...and captured, release_all kept whole rather than expanded.
        assert_eq!(rec.len(), 3);
    }

    #[test]
    fn the_tee_exposes_the_machines_own_control_table() {
        let mut spy = Spy::default();
        let mut rec = MovieRecorder::new("t", [0; 32], CONTROLS, Vec::new(), None);
        let tee = Recording::new(&mut spy, &mut rec);
        assert_eq!(tee.input_controls().len(), 1);
        assert_eq!(tee.input_controls()[0].stable_name, "coin");
    }
}
