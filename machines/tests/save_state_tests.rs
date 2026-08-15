//! Save-state round-trip tests for every registered machine.
//!
//! # Why this is shaped the way it is
//!
//! The obvious round trip — save a machine, load into a fresh one, re-save,
//! compare bytes — cannot fail. Both sides sit at power-on defaults, so every
//! field that only diverges after execution serializes identically whether or
//! not `save_state` captured it. A device runtime counter missing from a
//! board's `Saveable` impl, which is the bug this file exists to catch, passes
//! that test on every machine.
//!
//! What replaces it is a differential test: **several instances with different
//! histories load the same snapshot, and must then behave identically.**
//!
//! ```text
//! origin : run WARMUP frames, snapshot
//! for each history in HISTORIES:
//!     fresh machine -> reset -> run `history` frames -> load snapshot
//!                   -> run REPLAY frames -> observe
//! assert every observation matches the first
//! ```
//!
//! Two properties make this bite where the old test could not:
//!
//! 1. **The instances have different pre-load histories.** Every field the
//!    snapshot fails to carry therefore holds a *different* value in each of
//!    them at the moment of the load. Freshly-constructed instances would agree
//!    on it by accident.
//! 2. **They replay frames before being observed.** An unsaved field that
//!    survived the load feeds the following frames' computation, so the
//!    divergence propagates outward into the picture, the sound, and the
//!    serialized state.
//!
//! Comparing restores against *each other*, rather than against the machine the
//! snapshot came from, is deliberate. The CPU cores exclude their
//! mid-instruction execution temporaries from the save format on purpose
//! (`#[save_skip]` in `Z80`, and the same elsewhere), so a machine restored from
//! a snapshot resumes at an instruction boundary while the machine it was saved
//! from does not. Every restore is on the same side of that, so a deliberate
//! omission cancels out while an accidental one still shows.
//!
//! The observation is the rendered frame and the emitted audio as well as the
//! snapshot bytes. The audio matters most: sound devices are where free-running
//! counters live, and a counter dropped from a `Saveable` impl also vanishes
//! from the snapshot bytes — so the snapshot cannot be the only witness.
//!
//! Everything is driven from `registry::all()` via `MachineEntry::create_bare`,
//! so a newly registered machine is covered without touching this file.
//!
//! # What blank ROMs can and cannot reach
//!
//! The CPU still runs and every video, audio and timer device still ticks, so
//! anything that free-runs — CPU registers, RAM, clock dividers, sound
//! oscillators, resampler phase — diverges between histories and is checked.
//! Dropping a sound device from a board's `Saveable` impl fails this file.
//!
//! What a blank ROM cannot reach is state only a *running game* writes: video
//! latches, bank registers, protection handshakes. Those stay at their
//! power-on value in every instance, so omitting them from the snapshot cannot
//! be detected here. `phosphor-harness` carries the ROM-gated counterpart,
//! which runs this same protocol on a machine that has really booted and does
//! see them.

use phosphor_core::core::machine::FrontendMachine;
use phosphor_machines::registry;

/// Frames the origin runs before snapshotting, so it is not captured at
/// power-on defaults.
const WARMUP: usize = 2;

/// Pre-load histories to compare. They must leave the machine in *different*
/// states — that is what gives an unsaved field a different value on each side.
/// Zero is the frontend's "start up and immediately load a save" path.
///
/// A machine fed a blank ROM often settles into a short limit cycle — Star Wars
/// and ESB alternate between two states once their 6809s have nowhere left to
/// go — so the gaps here are not round numbers.
/// `the_pre_load_histories_actually_differ` checks the choice still holds for
/// every machine.
const HISTORIES: [usize; 3] = [0, 3, 10];

/// Frames every instance replays after loading, giving any surviving difference
/// time to propagate into what we observe.
const REPLAY: usize = 4;

/// What a machine does after the load: what it draws, what it plays, and what
/// it says its state is.
struct Observation {
    frame: Vec<u8>,
    audio: Vec<i16>,
    state: Vec<u8>,
}

fn run(sys: &mut dyn FrontendMachine, frames: usize) {
    for _ in 0..frames {
        sys.run_frame();
    }
}

/// Consume everything the machine has queued for the speaker.
///
/// Called just before the load, so the replay that follows starts from an empty
/// queue on every instance. Pending output samples are emulator plumbing rather
/// than hardware state, and the frontend drains them every frame; without this
/// the histories would carry different backlogs into the comparison.
fn drain_audio(sys: &mut dyn FrontendMachine) {
    let mut chunk = vec![0i16; 4096];
    while sys.fill_audio(&mut chunk) != 0 {}
}

/// Build one instance, give it `history` frames of its own, load `snapshot`
/// into it, then replay and capture everything observable.
fn restore_and_observe(
    entry: &registry::MachineEntry,
    history: usize,
    snapshot: &[u8],
) -> Observation {
    let name = entry.name;
    let mut sys = (entry.create_bare)();
    sys.reset();
    run(&mut *sys, history);
    drain_audio(&mut *sys);
    sys.load_state(snapshot)
        .unwrap_or_else(|e| panic!("{name}: load_state after {history} frames failed: {e:?}"));

    let mut audio = Vec::new();
    let mut chunk = vec![0i16; 4096];
    for _ in 0..REPLAY {
        sys.run_frame();
        loop {
            let n = sys.fill_audio(&mut chunk);
            if n == 0 {
                break;
            }
            audio.extend_from_slice(&chunk[..n]);
        }
    }

    let (w, h) = sys.display_size();
    let mut frame = vec![0u8; (w as usize) * (h as usize) * 3];
    sys.render_frame(&mut frame);

    Observation {
        frame,
        audio,
        state: sys
            .save_state()
            .unwrap_or_else(|| panic!("{name}: save_state() returned None")),
    }
}

/// Compare two runs of values, reporting the first difference rather than
/// dumping both buffers.
fn assert_same<T: PartialEq + std::fmt::Debug>(
    machine: &str,
    what: &str,
    history: usize,
    a: &[T],
    b: &[T],
) {
    let context = format!(
        "the instance that ran {} frames before loading disagrees with the one \
         that ran {} frames. That is state the machine mutates while running \
         and its Saveable impl does not capture",
        HISTORIES[0], history
    );
    assert_eq!(
        a.len(),
        b.len(),
        "{machine}: {what} has a different length ({} vs {}) — {context}",
        a.len(),
        b.len()
    );
    if let Some(i) = (0..a.len()).find(|&i| a[i] != b[i]) {
        let differing = (0..a.len()).filter(|&i| a[i] != b[i]).count();
        panic!(
            "{machine}: {what} differs at index {i} ({:?} vs {:?}), {differing} \
             of {} — {context}.",
            a[i],
            b[i],
            a.len()
        );
    }
}

/// The load must carry everything the machine's later behavior depends on.
///
/// See the module docs for the protocol and why restores are compared against
/// each other.
#[test]
fn a_snapshot_determines_everything_that_follows_it() {
    for entry in registry::all() {
        let name = entry.name;

        let mut origin = (entry.create_bare)();
        origin.reset();
        run(&mut *origin, WARMUP);
        let snapshot = origin
            .save_state()
            .unwrap_or_else(|| panic!("{name}: save_state() returned None"));
        assert!(
            !snapshot.is_empty(),
            "{name}: save data should not be empty"
        );
        drop(origin);

        let baseline = restore_and_observe(entry, HISTORIES[0], &snapshot);
        for history in &HISTORIES[1..] {
            let other = restore_and_observe(entry, *history, &snapshot);
            assert_same(
                name,
                "audio output",
                *history,
                &baseline.audio,
                &other.audio,
            );
            assert_same(
                name,
                "rendered frame",
                *history,
                &baseline.frame,
                &other.frame,
            );
            assert_same(
                name,
                "machine state",
                *history,
                &baseline.state,
                &other.state,
            );
        }
    }
}

/// The histories the test relies on must actually diverge.
///
/// Without this the protocol above could quietly degenerate: if a machine were
/// in the same state after every history, the comparison would hold trivially.
/// Blank ROMs make this a live risk — several machines settle into a two-frame
/// limit cycle once their CPU has nowhere to go.
#[test]
fn the_pre_load_histories_actually_differ() {
    for entry in registry::all() {
        let mut states: Vec<(usize, Vec<u8>)> = Vec::new();
        for &history in &HISTORIES {
            let mut sys = (entry.create_bare)();
            sys.reset();
            run(&mut *sys, history);
            let state = sys
                .save_state()
                .unwrap_or_else(|| panic!("{}: save_state() returned None", entry.name));
            if let Some((other, _)) = states.iter().find(|(_, s)| *s == state) {
                panic!(
                    "{}: identical state after {other} and {history} frames, so \
                     the round-trip test above has nothing to detect for this \
                     pair — pick different frame counts",
                    entry.name
                );
            }
            states.push((history, state));
        }
    }
}

/// The header's machine id is what stops a save being loaded into the wrong
/// machine.
#[test]
fn corrupted_machine_ids_are_rejected() {
    for entry in registry::all() {
        let mut sys = (entry.create_bare)();
        let saved = sys
            .save_state()
            .unwrap_or_else(|| panic!("{}: save_state() returned None", entry.name));

        // Offset 12 lands inside the id string for every machine here.
        assert!(
            saved.len() > 12,
            "{}: save data is too short to hold a machine id",
            entry.name
        );
        let mut corrupted = saved.clone();
        corrupted[12] ^= 0xFF;
        assert!(
            sys.load_state(&corrupted).is_err(),
            "{}: accepted a save with a corrupted machine id",
            entry.name
        );
    }
}

/// A truncated file must be refused rather than partially applied.
#[test]
fn truncated_saves_are_rejected() {
    for entry in registry::all() {
        let mut sys = (entry.create_bare)();
        let saved = sys
            .save_state()
            .unwrap_or_else(|| panic!("{}: save_state() returned None", entry.name));
        let truncated = &saved[..8.min(saved.len())];
        assert!(
            sys.load_state(truncated).is_err(),
            "{}: accepted a save truncated to its header",
            entry.name
        );
    }
}

/// Guard against a vacuous suite: every test here iterates `registry::all()`.
#[test]
fn the_registry_is_not_empty() {
    assert!(
        registry::all().len() > 30,
        "registry has {} machines — the tests above iterate it, so they would \
         pass vacuously",
        registry::all().len()
    );
}
