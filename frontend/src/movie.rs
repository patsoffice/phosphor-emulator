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
use phosphor_harness::movie::MovieRecorder;

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

    /// Start recording, resetting the machine to power-on first.
    ///
    /// Returns the message to show the user. The caller must resync input
    /// afterwards: the reset clears the machine's port bits, and a key held
    /// across it produces no new event.
    pub fn arm(&mut self, machine: &mut dyn FrontendMachine) {
        // Capture the battery state *before* the reset, then restore it after —
        // the same order `Harness::from_movie` uses, so replay reconstructs this
        // exact starting point rather than a subtly different one.
        let nvram: Option<Vec<u8>> = machine.save_nvram().map(<[u8]>::to_vec);
        machine.reset();
        if let Some(nv) = &nvram {
            machine.load_nvram(nv);
        }

        let dip: Vec<u8> = (0..machine.dip_banks().len())
            .map(|b| machine.dip_bank_value(b))
            .collect();

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

    /// Toggle recording, returning a message for the user. Arming resets the
    /// machine; the caller must resync input afterwards.
    pub fn toggle(&mut self, machine: &mut dyn FrontendMachine) -> String {
        if self.is_recording() {
            self.stop()
        } else {
            self.arm(machine);
            format!(
                "Movie: recording {} from power-on (press again to stop)",
                self.machine_name
            )
        }
    }
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
