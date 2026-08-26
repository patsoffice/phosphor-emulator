//! The determinism gate for input movies.
//!
//! A movie is only worth committing if replaying it reproduces the session it
//! recorded, byte for byte. This file asserts that property two ways: ROM-less
//! over every registered machine (so it runs in CI and covers the whole roster),
//! and ROM-gated on machines that have really booted (so it covers the state a
//! blank-ROM machine never reaches).
//!
//! The ROM-less half is not redundant with the ROM-gated half, and neither
//! subsumes the other. A bare machine executes whatever a zero-filled ROM
//! decodes to, which exercises the *plumbing* — event ordering, control
//! resolution, frame indexing — across all 40 machines. A booted machine
//! exercises the plumbing against live video latches, bank registers and sound
//! commands, but only on the machines this collection can supply.
//!
//! Gating follows the convention in `save_state_rom_test.rs`: no ROM directory
//! skips the gated tests entirely, and a machine whose set the collection cannot
//! supply skips individually.

mod common;

use std::path::Path;

use phosphor_core::core::machine::{FrontendMachine, InputControl, InputEvent, InputKind};
use phosphor_harness::movie::{MovieError, MovieRecorder, rom_digest};
use phosphor_harness::{Harness, Movie, load_rom_set, roms_dir};
use phosphor_machines::registry;
use phosphor_machines::rom_loader::RomSet;

/// Frames each ROM-less case runs. Long enough that a mis-delivered event has
/// time to propagate into the saved state and the framebuffer, short enough to
/// run across the whole registry.
const BARE_FRAMES: usize = 120;

/// Frames of attract mode before a ROM-gated recording starts, so the machine is
/// past its power-on self-test and its video and sound state is live.
const BOOTED_WARMUP: usize = 200;

/// Frames of recorded input on a booted machine.
const BOOTED_FRAMES: usize = 60;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Deterministic pseudo-random source, so a generated input plan is identical on
/// every run and every host. A real PRNG dependency would be overkill and would
/// put the reproducibility of this test at the mercy of a crate version.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        // Numerical Recipes' constants; any full-period LCG does here.
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 { 0 } else { self.next() as usize % n }
    }
}

fn is_analog(c: &InputControl) -> bool {
    matches!(c.kind, InputKind::AnalogAxis { .. })
}

/// Build a per-frame input plan for a machine's control table.
///
/// Analog deltas deliberately include fractional values. `RelativeCounter`
/// truncates each delta independently, so a plan that emitted only whole numbers
/// would pass even if replay summed a frame's motion into one event — exactly
/// the bug the per-event record shape exists to prevent.
fn input_plan(controls: &[InputControl], frames: usize, seed: u64) -> Vec<Vec<InputEvent>> {
    const FRACTIONAL: [f32; 6] = [0.6, -0.6, 1.2, -2.5, 0.4, 3.7];
    let mut rng = Lcg(seed);
    let mut plan = Vec::with_capacity(frames);
    for _ in 0..frames {
        let mut frame_events = Vec::new();
        if !controls.is_empty() {
            for _ in 0..rng.below(4) {
                let c = &controls[rng.below(controls.len())];
                if is_analog(c) {
                    frame_events.push(InputEvent::Relative {
                        id: c.id,
                        delta: FRACTIONAL[rng.below(FRACTIONAL.len())],
                    });
                } else {
                    frame_events.push(InputEvent::Button {
                        id: c.id,
                        pressed: rng.next().is_multiple_of(2),
                    });
                }
            }
        }
        plan.push(frame_events);
    }
    plan
}

struct Fingerprint {
    state: Vec<u8>,
    frame: Vec<u8>,
}

impl Fingerprint {
    fn of(machine: &mut dyn FrontendMachine, name: &str) -> Self {
        let (w, h) = machine.display_size();
        let mut frame = vec![0u8; (w as usize) * (h as usize) * 3];
        machine.render_frame(&mut frame);
        Self {
            state: machine
                .save_state()
                .unwrap_or_else(|| panic!("{name}: save_state() returned None")),
            frame,
        }
    }
}

fn assert_same(name: &str, a: &Fingerprint, b: &Fingerprint, context: &str) {
    if a.state != b.state {
        let at = (0..a.state.len().min(b.state.len())).find(|&i| a.state[i] != b.state[i]);
        panic!(
            "{name}: {context} — saved state differs (lengths {} vs {}, first difference at {at:?})",
            a.state.len(),
            b.state.len()
        );
    }
    if a.frame != b.frame {
        let differing = (0..a.frame.len())
            .filter(|&i| a.frame[i] != b.frame[i])
            .count();
        panic!(
            "{name}: {context} — rendered frame differs in {differing} of {} bytes",
            a.frame.len()
        );
    }
}

fn dip_bytes(machine: &dyn FrontendMachine) -> Vec<u8> {
    (0..machine.dip_banks().len())
        .map(|b| machine.dip_bank_value(b))
        .collect()
}

/// Drive a machine through `plan`, recording everything as it goes.
///
/// Events are pushed to the recorder and delivered to the machine in the same
/// order, before the frame runs — which is exactly where the frontend delivers
/// them, and where replay will deliver them again.
fn record_session(
    machine: Box<dyn FrontendMachine>,
    name: &str,
    plan: &[Vec<InputEvent>],
) -> (Movie, Fingerprint) {
    let mut h = Harness::from_machine(machine);
    let controls = h.machine().input_controls();
    let dip = dip_bytes(h.machine());
    let mut rec = MovieRecorder::new(name, [0u8; 32], controls, dip, None);

    for frame_events in plan {
        for &event in frame_events {
            rec.push_event(event);
            h.machine_mut().handle_input(event);
        }
        h.run_frame();
        rec.advance_frame();
    }

    assert_eq!(
        rec.unmapped(),
        0,
        "{name}: recorder saw events outside the control table"
    );
    let movie = rec.finish();
    let fp = Fingerprint::of(h.machine_mut(), name);
    (movie, fp)
}

/// Replay a movie against a fresh machine for `frames` frames.
fn replay_session(
    machine: Box<dyn FrontendMachine>,
    name: &str,
    movie: Movie,
    frames: usize,
) -> Fingerprint {
    let mut h = Harness::from_machine(machine);
    h.bind_movie(movie)
        .unwrap_or_else(|e| panic!("{name}: binding the movie failed: {e}"));
    for _ in 0..frames {
        h.run_frame();
    }
    Fingerprint::of(h.machine_mut(), name)
}

// ---------------------------------------------------------------------------
// ROM-less, registry-driven
// ---------------------------------------------------------------------------

#[test]
fn the_registry_is_not_empty() {
    assert!(
        !registry::all().is_empty(),
        "no registered machines — every registry-driven test in this file \
         would otherwise pass having checked nothing"
    );
}

/// The property the format exists for: a recorded session, replayed, reproduces
/// the machine that recorded it.
#[test]
fn a_recorded_session_replays_to_the_same_machine_state() {
    let entries = registry::all();
    common::map_parallel(&entries, |entry| {
        let name = entry.name;
        let controls = (entry.create_bare)().input_controls();
        let plan = input_plan(controls, BARE_FRAMES, 0x5EED_0001);

        let (movie, recorded) = record_session((entry.create_bare)(), name, &plan);
        let replayed = replay_session((entry.create_bare)(), name, movie, BARE_FRAMES);

        assert_same(
            name,
            &recorded,
            &replayed,
            "replaying the movie did not reproduce the session it recorded",
        );
    });
}

/// Replay must also be deterministic against itself — the same movie, twice,
/// from power-on.
#[test]
fn replaying_the_same_movie_twice_gives_identical_state() {
    let entries = registry::all();
    common::map_parallel(&entries, |entry| {
        let name = entry.name;
        let controls = (entry.create_bare)().input_controls();
        let plan = input_plan(controls, BARE_FRAMES, 0x5EED_0002);
        let (movie, _) = record_session((entry.create_bare)(), name, &plan);

        let first = replay_session((entry.create_bare)(), name, movie.clone(), BARE_FRAMES);
        let second = replay_session((entry.create_bare)(), name, movie, BARE_FRAMES);
        assert_same(name, &first, &second, "two replays of one movie diverged");
    });
}

/// A movie survives the filesystem round trip, not just the in-memory one.
#[test]
fn a_movie_encoded_and_decoded_replays_identically() {
    let entries = registry::all();
    common::map_parallel(&entries, |entry| {
        let name = entry.name;
        let controls = (entry.create_bare)().input_controls();
        let plan = input_plan(controls, 40, 0x5EED_0003);
        let (movie, _) = record_session((entry.create_bare)(), name, &plan);

        let decoded = Movie::decode(&movie.encode())
            .unwrap_or_else(|e| panic!("{name}: re-decoding its own movie failed: {e}"));
        assert_eq!(decoded, movie, "{name}: movie changed across encode/decode");

        let direct = replay_session((entry.create_bare)(), name, movie, 40);
        let round_tripped = replay_session((entry.create_bare)(), name, decoded, 40);
        assert_same(
            name,
            &direct,
            &round_tripped,
            "a movie replayed differently after an encode/decode round trip",
        );
    });
}

/// Whether the host drains audio must not change the machine's saved state.
///
/// This is the hazard that would quietly undermine every fingerprint above.
/// Headless replay never calls `fill_audio`, while a live frontend session calls
/// it every frame; if any of the audio transport reached `Saveable`, the same
/// movie would fingerprint differently depending on who replayed it, and the
/// comparison would be measuring the harness rather than the machine.
///
/// `AudioResampler::save_state` writes its filter state but not its output
/// `SampleRing`, so this should hold. Asserting it means a future change that
/// starts saving the ring fails here, naming the reason, rather than surfacing
/// as an unreproducible golden frame.
#[test]
fn draining_audio_does_not_change_the_saved_state() {
    let entries = registry::all();
    common::map_parallel(&entries, |entry| {
        let name = entry.name;
        // The drain buffer moved inside the body: it was shared across
        // iterations, which was harmless sequentially and is not shareable now.
        let mut chunk = vec![0i16; 4096];

        let mut undrained = (entry.create_bare)();
        for _ in 0..BARE_FRAMES {
            undrained.run_frame();
        }

        let mut drained = (entry.create_bare)();
        for _ in 0..BARE_FRAMES {
            drained.run_frame();
            while drained.fill_audio(&mut chunk) != 0 {}
        }

        assert_same(
            name,
            &Fingerprint::of(&mut *undrained, name),
            &Fingerprint::of(&mut *drained, name),
            "draining audio changed the machine's saved state, so a movie \
             fingerprint depends on who replayed it",
        );
    });
}

/// Guard against the determinism tests passing vacuously.
///
/// Every assertion above compares a recorded run against a replayed one. If
/// replay delivered *nothing* — a broken cursor, an unbound player, a control
/// table that resolved to no ids — those comparisons would still pass, because
/// the recorded run's input would also have had no effect on a machine that
/// ignores it. This measures the other side: that the generated input plan
/// actually moves the machines it is fed to, so a replay that dropped it would
/// be caught.
#[test]
fn the_generated_input_actually_changes_machine_state() {
    let mut moved = Vec::new();
    let mut recorded_any = 0usize;

    for entry in registry::all() {
        let name = entry.name;
        let controls = (entry.create_bare)().input_controls();
        if controls.is_empty() {
            continue;
        }
        let plan = input_plan(controls, BARE_FRAMES, 0x5EED_0001);

        let (movie, with_input) = record_session((entry.create_bare)(), name, &plan);
        if !movie.records.is_empty() {
            recorded_any += 1;
        }

        let mut idle = (entry.create_bare)();
        for _ in 0..BARE_FRAMES {
            idle.run_frame();
        }
        let without_input = Fingerprint::of(&mut *idle, name);

        if with_input.state != without_input.state || with_input.frame != without_input.frame {
            moved.push(name);
        }
    }

    assert!(
        recorded_any > 0,
        "no machine recorded a single input record — the plan generator is broken"
    );
    // 38 of 40 react as this is written. Not every machine will: under a blank
    // ROM some never poll their input ports at all. The bar is set well below
    // the observed figure so a machine legitimately changing behaviour does not
    // fail it, but far enough above zero to catch a replay that delivers
    // nothing — which is the failure this guard exists for.
    assert!(
        moved.len() >= 30,
        "only {} machine(s) changed state when fed input ({moved:?}) — the \
         determinism tests above may be comparing two identical no-ops",
        moved.len()
    );
    eprintln!(
        "{} of {} machines observably reacted to the generated input",
        moved.len(),
        registry::all().len()
    );
}

/// Analog deltas must arrive one event at a time. Summing a frame's motion into
/// a single delta changes the result, because the machine truncates each delta
/// independently — this asserts the two are actually distinguishable, so the
/// determinism tests above are not passing vacuously.
///
/// Note the deltas here are deliberately fractional. With the default binding
/// sensitivity a real mouse only ever produces whole deltas (SDL's `xrel` is an
/// integer, `DEFAULT_SCALE` is 1.0), and whole deltas sum identically — a
/// four-minute Marble Madness capture contained no case where it mattered. This
/// pins the property for the configurations where it *does*: a non-default
/// sensitivity, or a future sub-unit device.
#[test]
fn summing_a_frames_analog_deltas_is_not_equivalent_to_delivering_them() {
    let all = registry::all();
    let Some(entry) = all
        .iter()
        .find(|e| (e.create_bare)().input_controls().iter().any(is_analog))
    else {
        panic!("no registered machine exposes an analog control");
    };

    let controls = (entry.create_bare)().input_controls();
    let axis = controls.iter().find(|c| is_analog(c)).unwrap();

    // Two 0.6 deltas truncate to 0 each; one 1.2 delta truncates to 1.
    let separate = vec![
        InputEvent::Relative {
            id: axis.id,
            delta: 0.6,
        },
        InputEvent::Relative {
            id: axis.id,
            delta: 0.6,
        },
    ];
    let summed = vec![InputEvent::Relative {
        id: axis.id,
        delta: 1.2,
    }];

    let run = |events: &[InputEvent]| {
        let mut m = (entry.create_bare)();
        for &e in events {
            m.handle_input(e);
        }
        for _ in 0..8 {
            m.run_frame();
        }
        m.save_state().expect("save_state")
    };

    assert_ne!(
        run(&separate),
        run(&summed),
        "{}: delivering two 0.6 deltas separately produced the same state as one \
         1.2 delta, so this machine cannot witness the per-event property — pick \
         a different machine for this test",
        entry.name
    );
}

#[test]
fn binding_rejects_a_control_the_machine_does_not_expose() {
    let all = registry::all();
    let entry = all[0];
    let mut h = Harness::from_machine((entry.create_bare)());
    let movie = Movie {
        header: phosphor_harness::MovieHeader {
            machine: entry.name.into(),
            rom_digest: [0; 32],
            controls: vec!["definitely_not_a_control".into()],
            dip: Vec::new(),
            nvram: None,
            host_sample_rate: 44_100,
            frames: 0,
        },
        records: Vec::new(),
    };
    assert_eq!(
        h.bind_movie(movie),
        Err(MovieError::UnknownControl(
            "definitely_not_a_control".into()
        ))
    );
}

// ---------------------------------------------------------------------------
// ROM identity
// ---------------------------------------------------------------------------

/// The property `rom_digest` exists for, and the one it used to lack: two dumps
/// that differ in a single byte must not fingerprint the same.
///
/// It once hashed the registry's `rom_names` (archive names) as if they were
/// member names, so every lookup missed and the digest was a function of the
/// name list alone. Three genuinely different Mario Bros dumps digested
/// identically. This is that case in miniature.
#[test]
fn digest_separates_two_dumps_that_differ_in_one_byte() {
    let a = RomSet::from_slices(&[("cpu.1a", &[0x00, 0x11, 0x22]), ("gfx.2b", &[0xFF])]);
    let b = RomSet::from_slices(&[("cpu.1a", &[0x00, 0x11, 0x23]), ("gfx.2b", &[0xFF])]);
    assert_ne!(rom_digest(&a), rom_digest(&b));
}

/// A member that is present in one set and absent in the other is a different
/// dump too, and a rename with identical bytes likewise.
#[test]
fn digest_separates_sets_by_membership_and_by_name() {
    let base = RomSet::from_slices(&[("cpu.1a", &[0x00, 0x11])]);
    let extra = RomSet::from_slices(&[("cpu.1a", &[0x00, 0x11]), ("prom.4c", &[0x00])]);
    let renamed = RomSet::from_slices(&[("cpu.1b", &[0x00, 0x11])]);
    assert_ne!(rom_digest(&base), rom_digest(&extra));
    assert_ne!(rom_digest(&base), rom_digest(&renamed));
}

/// The digest must not depend on the order files were inserted. A `RomSet` is
/// backed by a `HashMap`, so an unsorted walk would make a movie replayable in
/// one process and not the next.
#[test]
fn digest_is_independent_of_insertion_order() {
    let forward = RomSet::from_slices(&[
        ("a.1", &[1, 2, 3]),
        ("b.2", &[4, 5]),
        ("c.3", &[6]),
        ("d.4", &[7, 8, 9, 10]),
    ]);
    let reverse = RomSet::from_slices(&[
        ("d.4", &[7, 8, 9, 10]),
        ("c.3", &[6]),
        ("b.2", &[4, 5]),
        ("a.1", &[1, 2, 3]),
    ]);
    assert_eq!(rom_digest(&forward), rom_digest(&reverse));
}

/// Length-prefixing exists so that regrouping the same bytes across members
/// cannot collide. Without it these two sets absorb the identical byte stream.
#[test]
fn digest_separates_two_splits_of_the_same_bytes() {
    let split = RomSet::from_slices(&[("r", &[0xAA]), ("s", &[0xBB, 0xCC])]);
    let other = RomSet::from_slices(&[("r", &[0xAA, 0xBB]), ("s", &[0xCC])]);
    assert_ne!(rom_digest(&split), rom_digest(&other));
}

/// A blank set has no members to fingerprint, so it answers with one constant.
/// The only requirement is that it is stable and is not mistaken for a real
/// dump; machines are separated by the movie's `machine` field, not by this.
#[test]
fn a_blank_set_digests_to_a_stable_value_of_its_own() {
    assert_eq!(rom_digest(&RomSet::blank()), rom_digest(&RomSet::blank()));
    assert_ne!(
        rom_digest(&RomSet::blank()),
        rom_digest(&RomSet::from_slices(&[]))
    );
}

// ---------------------------------------------------------------------------
// ROM-gated
// ---------------------------------------------------------------------------

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

/// The same property, on machines whose game is really running.
#[test]
fn a_recorded_session_replays_identically_on_a_booted_machine() {
    let Some(dir) = roms_dir() else {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
        return;
    };

    // Each machine records and replays two booted instances of itself and shares
    // nothing with the others, so this fans out. An assertion still panics
    // inside its worker and `map_parallel` re-raises the lowest-index one, which
    // is the machine a sequential run would have stopped on.
    let entries = registry::all();
    let checked: Vec<&'static str> = common::map_parallel(&entries, |entry| {
        let name = entry.name;
        let mut machine = booted(&dir, entry)?;

        // Warm up past the self-test *before* recording starts, so the recorded
        // window covers a machine with live state. Replay reproduces the warmup
        // by running the same frames from power-on.
        for _ in 0..BOOTED_WARMUP {
            machine.run_frame();
        }

        let controls = machine.input_controls();
        let plan = input_plan(controls, BOOTED_FRAMES, 0x5EED_0004);

        let mut h = Harness::from_machine(machine);
        let dip = dip_bytes(h.machine());
        let mut rec = MovieRecorder::new(name, [0u8; 32], controls, dip, None);
        // The recorder's frame numbering starts at the recording, not at
        // power-on, so replay must warm up by the same amount before binding.
        for frame_events in &plan {
            for &event in frame_events {
                rec.push_event(event);
                h.machine_mut().handle_input(event);
            }
            h.run_frame();
            rec.advance_frame();
        }
        let recorded = Fingerprint::of(h.machine_mut(), name);
        let movie = rec.finish();

        let mut replay_machine = booted(&dir, entry).expect("ROMs were present a moment ago");
        for _ in 0..BOOTED_WARMUP {
            replay_machine.run_frame();
        }
        let replayed = replay_session(replay_machine, name, movie, BOOTED_FRAMES);

        assert_same(
            name,
            &recorded,
            &replayed,
            "replaying the movie did not reproduce the booted session it recorded",
        );
        Some(name)
    })
    .into_iter()
    .flatten()
    .collect();

    eprintln!(
        "record/replay round-tripped {} booted machine(s)",
        checked.len()
    );
    assert!(
        !checked.is_empty(),
        "the ROM directory {} exists but holds no registered machine's set — \
         this test would otherwise pass having checked nothing",
        dir.display()
    );
}

/// A movie replayed against a different dump boots fine and then diverges. The
/// digest is what turns that into an error instead of a mystery.
#[test]
fn replay_refuses_a_rom_set_the_movie_was_not_recorded_against() {
    let Some(dir) = roms_dir() else {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
        return;
    };

    let all = registry::all();
    let Some(entry) = all.iter().find(|e| booted(&dir, e).is_some()) else {
        eprintln!("skipping: the ROM directory supplies no registered machine");
        return;
    };

    let set = load_rom_set(dir.to_str().unwrap(), entry.rom_names).expect("load_rom_set");
    let real = rom_digest(&set);

    let machine = booted(&dir, entry).expect("just checked");
    let movie = Movie {
        header: phosphor_harness::MovieHeader {
            machine: entry.name.into(),
            rom_digest: [0xEE; 32],
            controls: machine
                .input_controls()
                .iter()
                .map(|c| c.stable_name.to_owned())
                .collect(),
            dip: Vec::new(),
            nvram: None,
            host_sample_rate: 44_100,
            frames: 0,
        },
        records: Vec::new(),
    };
    assert_ne!(real, movie.header.rom_digest, "fixture digest must differ");

    let err = match Harness::from_movie(dir.to_str().unwrap(), movie) {
        Ok(_) => panic!("a mismatched ROM digest must be refused"),
        Err(e) => e,
    };
    assert!(
        err.contains("different ROM set"),
        "expected a ROM mismatch error, got: {err}"
    );
}

#[test]
fn replay_refuses_a_movie_naming_an_unregistered_machine() {
    let Some(dir) = roms_dir() else {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
        return;
    };
    let movie = Movie {
        header: phosphor_harness::MovieHeader {
            machine: "not_a_real_machine".into(),
            rom_digest: [0; 32],
            controls: Vec::new(),
            dip: Vec::new(),
            nvram: None,
            host_sample_rate: 44_100,
            frames: 0,
        },
        records: Vec::new(),
    };
    let err = match Harness::from_movie(dir.to_str().unwrap(), movie) {
        Ok(_) => panic!("an unregistered machine name must be refused"),
        Err(e) => e,
    };
    assert!(
        err.contains("unknown machine"),
        "expected an unknown-machine error, got: {err}"
    );
}

/// `reset()` is a reset button, not a power cycle — and the movie recorder now
/// depends on that being true.
///
/// The frontend used to arm a recording by resetting the live machine, which
/// had been running attract mode. Replay builds a fresh machine from ROM, so
/// the two started from different states and a recorded session replayed
/// differently from the one that was played — on Donkey Kong, visibly so by
/// about frame 1800. Arming now rebuilds from ROM instead.
///
/// This pins the assumption. If someone later makes `reset()` a true power
/// cycle, this fails — and the right response is to come back here and simplify
/// the frontend's arm path, not to delete the test.
#[test]
fn reset_is_not_a_power_cycle_so_arming_must_rebuild() {
    let Some(dir) = roms_dir() else {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
        return;
    };

    // The whole binary's cost is here: 720 frames on one machine and 120 on
    // another, for every registered game, and nothing shared between them. It
    // measured 70.4s where the next slowest test in this file is 15.3s.
    let entries = registry::all();
    let outcomes = common::map_parallel(&entries, |entry| {
        let name = entry.name;

        // What arming in place used to produce: a machine that has run, reset.
        let mut used = booted(&dir, entry)?;
        for _ in 0..600 {
            used.run_frame();
        }
        used.reset();
        for _ in 0..120 {
            used.run_frame();
        }
        let after_use = Fingerprint::of(&mut *used, name);
        drop(used);

        // What replay constructs: a fresh machine from ROM.
        let mut fresh = booted(&dir, entry).expect("ROMs were present a moment ago");
        for _ in 0..120 {
            fresh.run_frame();
        }
        let from_fresh = Fingerprint::of(&mut *fresh, name);

        let differs = after_use.state != from_fresh.state || after_use.frame != from_fresh.frame;
        Some((name, differs))
    });

    let checked = outcomes.iter().flatten().count();
    let differing: Vec<&'static str> = outcomes
        .iter()
        .flatten()
        .filter(|(_, differs)| *differs)
        .map(|(name, _)| *name)
        .collect();

    assert!(checked > 0, "no machine's ROMs were available");
    assert!(
        !differing.is_empty(),
        "every machine now returns to power-on state on reset(), across {checked} \
         checked. If that is a deliberate change, the frontend's movie arm no \
         longer needs to rebuild from ROM and can be simplified — see \
         frontend/src/movie.rs::arm_fresh."
    );
    eprintln!(
        "{} of {checked} machines differ after reset-following-play: {:?}",
        differing.len(),
        differing
    );
}

/// The bug's own demonstration, against the real collection: an entry listing
/// more than one archive, with more than one of them on disk, holds genuinely
/// different dumps of one game. Replaying a movie recorded on one against the
/// other is the failure the digest exists to name, and for a long time it could
/// not see it: all three Mario Bros dumps digested alike.
#[test]
fn two_dumps_of_one_game_do_not_digest_alike() {
    let Some(dir) = roms_dir() else {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
        return;
    };

    let all = registry::all();
    let mut checked = 0usize;
    for entry in &all {
        let present: Vec<&str> = entry
            .rom_names
            .iter()
            .copied()
            .filter(|n| dir.join(format!("{n}.zip")).exists())
            .collect();
        if present.len() < 2 {
            continue;
        }
        // Digest each archive on its own, which is what a recording session
        // does: `create_from_first_rom_set` loads one name at a time.
        let mut seen: Vec<(&str, [u8; 32])> = Vec::new();
        for name in present {
            let path = dir.join(format!("{name}.zip"));
            let Ok(set) = load_rom_set(path.to_str().unwrap(), &[name]) else {
                continue;
            };
            let digest = rom_digest(&set);
            if let Some((other, _)) = seen.iter().find(|(_, d)| *d == digest) {
                panic!(
                    "{}: '{name}.zip' and '{other}.zip' digest identically, so a movie \
                     recorded against one would replay against the other and diverge",
                    entry.name
                );
            }
            seen.push((name, digest));
            checked += 1;
        }
    }

    if checked == 0 {
        eprintln!("skipping: no registered machine has two of its archives on disk");
    } else {
        eprintln!("{checked} archives digested, all distinct");
    }
}
