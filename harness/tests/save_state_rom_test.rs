//! ROM-gated save-state round trip: the same protocol as
//! `machines/tests/save_state_tests.rs`, on machines that have really booted.
//!
//! The version in `phosphor-machines` runs without ROMs, which is what lets it
//! run in CI, but a machine fed a blank ROM never executes its game. Video
//! latches, bank registers, protection handshakes and sound commands all stay
//! at their power-on values there, so omitting any of them from a `Saveable`
//! impl is undetectable. This file runs the same comparison after a few
//! seconds of attract mode, where all of that is live.
//!
//! The protocol is deliberately identical — see that file for why it compares
//! restores against each other rather than against the machine the snapshot
//! came from, and why the observation includes audio. It is duplicated rather
//! than shared because `phosphor-machines` cannot depend on this crate.
//!
//! Skips entirely without a ROM directory, and per machine for a ROM set this
//! collection cannot supply.

mod common;

use std::path::Path;

use phosphor_core::core::machine::FrontendMachine;
use phosphor_harness::{load_rom_set, roms_dir};
use phosphor_machines::registry;

/// Frames of attract mode before the snapshot, so the machine is past its
/// power-on self-test and has written the state a blank-ROM machine never
/// reaches.
const WARMUP: usize = 300;

/// Pre-load histories to compare. Different histories are the whole point:
/// they give any field the snapshot omits a different value on each side.
const HISTORIES: [usize; 3] = [0, 3, 10];

/// Frames each instance replays after loading, so a surviving difference has
/// time to reach the picture, the sound, or the serialized state.
const REPLAY: usize = 4;

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

fn drain_audio(sys: &mut dyn FrontendMachine) {
    let mut chunk = vec![0i16; 4096];
    while sys.fill_audio(&mut chunk) != 0 {}
}

/// Build a booted machine, or `None` when this collection cannot supply its
/// ROMs (see `boot_check_test.rs` for why every load error is a skip).
fn booted(dir: &Path, entry: &registry::MachineEntry) -> Option<Box<dyn FrontendMachine>> {
    if !entry
        .rom_names
        .iter()
        .any(|n| dir.join(format!("{n}.zip")).exists())
    {
        return None;
    }
    let set = load_rom_set(dir.to_str().unwrap(), entry.rom_names).ok()?;
    let mut machine = match (entry.create)(&set) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("skipping {}: {e}", entry.name);
            return None;
        }
    };
    machine.reset();
    Some(machine)
}

fn restore_and_observe(
    dir: &Path,
    entry: &registry::MachineEntry,
    history: usize,
    snapshot: &[u8],
) -> Observation {
    let name = entry.name;
    let mut sys = booted(dir, entry).expect("ROMs were present a moment ago");
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

fn assert_same<T: PartialEq + std::fmt::Debug>(
    machine: &str,
    what: &str,
    history: usize,
    a: &[T],
    b: &[T],
) {
    let context = format!(
        "the instance that ran {} frames before loading disagrees with the one \
         that ran {history} frames. That is state the running game mutates and \
         the machine's Saveable impl does not capture",
        HISTORIES[0]
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

/// A snapshot taken from a running game must determine everything that
/// follows it.
#[test]
fn a_snapshot_of_a_running_game_determines_everything_that_follows_it() {
    let Some(dir) = roms_dir() else {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
        return;
    };

    // One machine's round trip cannot see another's, and each boots several
    // instances of its own, so this is the expensive part and it fans out. A
    // failing assertion still panics inside its worker; `map_parallel` re-raises
    // the lowest-index one with its own message, which is the same machine a
    // sequential run would have stopped on.
    let entries = registry::all();
    let checked: Vec<&'static str> = common::map_parallel(&entries, |entry| {
        let name = entry.name;
        let mut origin = booted(&dir, entry)?;
        run(&mut *origin, WARMUP);
        let snapshot = origin
            .save_state()
            .unwrap_or_else(|| panic!("{name}: save_state() returned None"));
        drop(origin);

        // Every instance re-boots and runs its own history from power-on, so
        // `history` frames here means the same thing it does above: this
        // machine has been running that long when the snapshot lands on it.
        let baseline = restore_and_observe(&dir, entry, HISTORIES[0], &snapshot);
        for &history in &HISTORIES[1..] {
            let other = restore_and_observe(&dir, entry, history, &snapshot);
            assert_same(name, "audio output", history, &baseline.audio, &other.audio);
            assert_same(
                name,
                "rendered frame",
                history,
                &baseline.frame,
                &other.frame,
            );
            assert_same(
                name,
                "machine state",
                history,
                &baseline.state,
                &other.state,
            );
        }
        Some(name)
    })
    .into_iter()
    .flatten()
    .collect();

    eprintln!("round-tripped {} booted machine(s)", checked.len());
    assert!(
        !checked.is_empty(),
        "the ROM directory {} exists but holds no registered machine's set — \
         this test would otherwise pass having checked nothing",
        dir.display()
    );
}
