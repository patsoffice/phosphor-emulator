//! ROM-gated audio sanity: does each machine emit something usable?
//!
//! The audio counterpart of `golden_frame_test`'s guards, and deliberately the
//! cheap half of the discrete-sound fidelity work
//! (`docs/designs/discrete-sound-fidelity.md`, Phase 2). It needs no MAME, no
//! reference capture and no human listening — only ROMs and the arithmetic in
//! [`phosphor_core::audio::analysis`].
//!
//! It asserts three things that are true of *any* correct audio path, without
//! knowing what the machine is supposed to sound like:
//!
//! 1. the output is not sitting on a large DC offset;
//! 2. it is not permanently saturated;
//! 3. it is not permanently silent, unless the machine says it should be.
//!
//! Each is a defect rather than a difference — no reference could make a
//! half-scale DC offset acceptable. Those three would have caught both halves
//! of `…-audio-dc-offset-g7p4` (Donkey Kong's offset and Joust's saturation) on
//! the day they landed.
//!
//! # Gating
//!
//! No ROM directory (`PHOSPHOR_ROMS`, else `~/ws/mame-runtime/roms`) → skip, so
//! CI stays green without ROMs, exactly as `boot_check_test` does. A machine
//! whose own ROM set is missing skips individually.
//!
//! # Expectations
//!
//! `tests/audio/expectations.toml` holds the thresholds and the per-machine
//! exceptions. A machine that legitimately breaks a default needs an entry with
//! a `reason`, which keeps the exceptions a reviewed claim about hardware
//! rather than a skip list that grows in silence.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use phosphor_core::audio::analysis::{Integrity, pcm_to_f64};
use phosphor_harness::{Harness, load_rom_set, roms_dir};
use phosphor_machines::registry;
use phosphor_machines::rom_loader::RomSet;

/// Frames to run before measuring.
///
/// Long enough for a board to get past reset and into whatever its attract mode
/// does, short enough that the whole registry stays a few minutes. At 60 Hz
/// this is ten seconds.
const FRAMES: usize = 600;

/// Frames to discard from the front before measuring.
///
/// A board's first moments include power-on transients — filters settling from
/// zero, a DAC latch that has not been written yet — which are not what the
/// machine sounds like and would trip the DC check on their own.
const SETTLE_FRAMES: usize = 60;

// ---------------------------------------------------------------------------
// Expectations file
// ---------------------------------------------------------------------------

/// The three defects this suite knows how to see.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Defect {
    Dc,
    Clipping,
    Silence,
}

impl Defect {
    fn as_str(self) -> &'static str {
        match self {
            Self::Dc => "dc",
            Self::Clipping => "clipping",
            Self::Silence => "silence",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Defaults {
    max_dc_offset: f64,
    max_clipped_fraction: f64,
    max_silent_fraction: f64,
}

/// A machine that really is like this, with the hardware reason.
#[derive(Debug, Clone)]
struct Correct {
    silent: bool,
}

/// A machine that is broken in a known way, tracked by an issue.
#[derive(Debug, Clone, Default)]
struct KnownDefect {
    kinds: Vec<Defect>,
}

struct Expectations {
    defaults: Defaults,
    correct: BTreeMap<String, Correct>,
    known_defects: BTreeMap<String, KnownDefect>,
}

fn expectations_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/audio/expectations.toml")
}

/// Parse the expectations file, checking as it goes that every entry carries
/// the justification its kind requires.
fn load_expectations() -> Expectations {
    let path = expectations_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let doc: toml::Value = text
        .parse()
        .unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()));
    let table = doc.as_table().expect("expectations must be a table");

    let d = table
        .get("defaults")
        .and_then(|v| v.as_table())
        .expect("expectations.toml needs a [defaults] table");
    let num = |t: &toml::value::Table, k: &str| -> f64 {
        t.get(k)
            .and_then(|v| v.as_float())
            .unwrap_or_else(|| panic!("[defaults] needs a float `{k}`"))
    };
    let defaults = Defaults {
        max_dc_offset: num(d, "max_dc_offset"),
        max_clipped_fraction: num(d, "max_clipped_fraction"),
        max_silent_fraction: num(d, "max_silent_fraction"),
    };

    let array = |key: &str| -> Vec<toml::value::Table> {
        table
            .get(key)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .map(|v| {
                        v.as_table()
                            .unwrap_or_else(|| panic!("[[{key}]] entries must be tables"))
                            .clone()
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let machine_of = |t: &toml::value::Table, key: &str| -> String {
        t.get("machine")
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| panic!("every [[{key}]] needs a `machine`"))
            .to_string()
    };
    let flag =
        |t: &toml::value::Table, k: &str| t.get(k).and_then(|v| v.as_bool()).unwrap_or(false);

    let mut correct = BTreeMap::new();
    for t in array("correct") {
        let name = machine_of(&t, "correct");
        assert!(
            t.get("reason")
                .and_then(|v| v.as_str())
                .is_some_and(|r| r.len() > 20),
            "[[correct]] {name} needs a `reason` describing the hardware behaviour"
        );
        correct.insert(
            name,
            Correct {
                silent: flag(&t, "silent"),
            },
        );
    }

    let mut known_defects = BTreeMap::new();
    for t in array("known_defect") {
        let name = machine_of(&t, "known_defect");
        assert!(
            t.get("reason")
                .and_then(|v| v.as_str())
                .is_some_and(|r| r.len() > 20),
            "[[known_defect]] {name} needs a `reason` describing what was measured"
        );
        assert!(
            t.get("issue")
                .and_then(|v| v.as_str())
                .is_some_and(|i| i.starts_with("phosphor-emulator-")),
            "[[known_defect]] {name} needs an `issue` tracking the fix — an untracked \
             known defect is just a silenced test"
        );
        let mut kinds = Vec::new();
        if flag(&t, "dc") {
            kinds.push(Defect::Dc);
        }
        if flag(&t, "clipping") {
            kinds.push(Defect::Clipping);
        }
        if flag(&t, "silence") {
            kinds.push(Defect::Silence);
        }
        assert!(
            !kinds.is_empty(),
            "[[known_defect]] {name} names no defect kind (dc, clipping, silence)"
        );
        known_defects.insert(name, KnownDefect { kinds });
    }

    for name in known_defects.keys() {
        assert!(
            !correct.contains_key(name),
            "{name} is listed as both correct and defective"
        );
    }

    Expectations {
        defaults,
        correct,
        known_defects,
    }
}

// ---------------------------------------------------------------------------
// ROM plumbing, mirroring boot_check_test
// ---------------------------------------------------------------------------

fn roms() -> Option<PathBuf> {
    let dir = roms_dir();
    if dir.is_none() {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
    }
    dir
}

fn rom_set(dir: &Path, machine: &str) -> Option<RomSet> {
    let entry = registry::find(machine).unwrap_or_else(|| panic!("{machine} is not registered"));
    if !entry
        .rom_names
        .iter()
        .any(|n| dir.join(format!("{n}.zip")).exists())
    {
        return None;
    }
    load_rom_set(dir.to_str().unwrap(), entry.rom_names).ok()
}

/// Boot a machine and measure its audio, or `None` if this collection cannot
/// supply its ROMs.
fn measure(dir: &Path, machine: &str) -> Option<Integrity> {
    let _ = rom_set(dir, machine)?;
    let mut harness = Harness::build(machine, dir.to_str().unwrap(), None, None, &[], &[]).ok()?;

    let rate = harness.machine().audio_sample_rate().max(1) as usize;
    let mut audio: Vec<i16> = Vec::new();
    let mut chunk = vec![0i16; rate];
    for frame in 0..FRAMES {
        harness.run_frame();
        let m = harness.machine_mut();
        loop {
            let n = m.fill_audio(&mut chunk);
            if n == 0 {
                break;
            }
            // Discard the settling window rather than never producing it: the
            // ring has to be drained either way or it overruns.
            if frame >= SETTLE_FRAMES {
                audio.extend_from_slice(&chunk[..n]);
            }
        }
    }

    // A machine that declares no audio at all (rate 0) has nothing to measure
    // and is not a failure.
    if audio.is_empty() {
        return None;
    }
    Some(Integrity::measure(&pcm_to_f64(&audio)))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Guard against a vacuous suite: everything below sweeps `registry::all()`.
#[test]
fn the_registry_is_not_empty() {
    assert!(registry::all().len() > 30);
}

#[test]
fn the_expectations_file_parses_and_has_sane_defaults() {
    let e = load_expectations();
    assert!(e.defaults.max_dc_offset > 0.0 && e.defaults.max_dc_offset < 1.0);
    assert!(e.defaults.max_clipped_fraction >= 0.0 && e.defaults.max_clipped_fraction < 1.0);
    assert!(e.defaults.max_silent_fraction > 0.0 && e.defaults.max_silent_fraction <= 1.0);
    // Parsing enforces reasons and issue links; reaching here means it held.
    assert!(!e.known_defects.is_empty() || !e.correct.is_empty());
}

/// An entry that outlives its machine is stale documentation, and stale
/// documentation about which machines are allowed to sound broken is worse
/// than none.
#[test]
fn every_expectation_names_a_registered_machine() {
    let e = load_expectations();
    for name in e.correct.keys().chain(e.known_defects.keys()) {
        assert!(
            registry::find(name).is_some(),
            "expectations.toml has an entry for {name}, which is not registered"
        );
    }
}

/// Measure every machine once, then judge. Split from the assertions so both
/// directions of the ratchet read from one sweep.
fn sweep(dir: &Path) -> (BTreeMap<String, Vec<Defect>>, usize, Vec<&'static str>) {
    let e = load_expectations();
    let mut found: BTreeMap<String, Vec<Defect>> = BTreeMap::new();
    let mut checked = 0usize;
    let mut skipped = Vec::new();

    for entry in registry::all() {
        let Some(integrity) = measure(dir, entry.name) else {
            skipped.push(entry.name);
            continue;
        };
        checked += 1;
        let mut defects = Vec::new();

        if integrity.dc_offset.abs() > e.defaults.max_dc_offset {
            defects.push(Defect::Dc);
        }
        if integrity.clipped_fraction > e.defaults.max_clipped_fraction {
            defects.push(Defect::Clipping);
        }
        if integrity.is_silent || integrity.silent_fraction > e.defaults.max_silent_fraction {
            defects.push(Defect::Silence);
        }
        if !defects.is_empty() {
            found.insert(entry.name.to_string(), defects);
        }
    }
    (found, checked, skipped)
}

/// No machine may be newly defective.
///
/// Failures are collected before reporting so one bad machine does not hide the
/// rest, the same reason `golden_frame_test` reports every mismatch at once.
#[test]
fn no_machine_emits_newly_defective_audio() {
    let Some(dir) = roms() else { return };
    let e = load_expectations();
    let (found, checked, skipped) = sweep(&dir);

    if !skipped.is_empty() {
        eprintln!(
            "skipped {} machine(s) with no ROM set: {}",
            skipped.len(),
            skipped.join(", ")
        );
    }
    assert!(
        checked > 0,
        "no machine could be checked — the ROM directory supplied nothing"
    );

    let mut unexpected: Vec<String> = Vec::new();
    for (machine, defects) in &found {
        let allowed_silent = e.correct.get(machine).is_some_and(|c| c.silent);
        let known: &[Defect] = e
            .known_defects
            .get(machine)
            .map(|k| k.kinds.as_slice())
            .unwrap_or(&[]);
        for d in defects {
            if *d == Defect::Silence && allowed_silent {
                continue;
            }
            if known.contains(d) {
                continue;
            }
            unexpected.push(format!("{machine}: {}", describe(*d)));
        }
    }

    assert!(
        unexpected.is_empty(),
        "{} newly defective machine audio path(s) out of {checked} checked:\n  {}\n\n\
         If this is a real regression, fix it. If the board genuinely behaves this way, \
         add a [[correct]] entry with the hardware reason. If it is a bug you are not \
         fixing now, add a [[known_defect]] entry with an issue.",
        unexpected.len(),
        unexpected.join("\n  ")
    );
}

fn describe(d: Defect) -> &'static str {
    match d {
        Defect::Dc => "large DC offset — a stuck source, wrong bias, or missing coupling capacitor",
        Defect::Clipping => "output pinned at the rail — excessive gain or missing attenuation",
        Defect::Silence => "emits no audio at all",
    }
}

/// The ratchet's other direction: a machine listed as defective must still be
/// defective.
///
/// Without this the list would be a skip list — entries would outlive their
/// bugs and quietly re-absorb a regression years later. Fixing a machine is
/// supposed to fail this test once, until its entry is deleted.
#[test]
fn every_known_defect_is_still_present() {
    let Some(dir) = roms() else { return };
    let e = load_expectations();
    let (found, _, skipped) = sweep(&dir);

    let mut fixed: Vec<String> = Vec::new();
    for (machine, known) in &e.known_defects {
        if skipped.contains(&machine.as_str()) {
            continue;
        }
        let actual = found.get(machine).map(|v| v.as_slice()).unwrap_or(&[]);
        for kind in &known.kinds {
            if !actual.contains(kind) {
                fixed.push(format!("{machine}: {} is fixed", kind.as_str()));
            }
        }
    }

    assert!(
        fixed.is_empty(),
        "{} known defect(s) no longer reproduce:\n  {}\n\n\
         Good news — delete the corresponding entries from \
         tests/audio/expectations.toml so the fix is locked in and cannot regress.",
        fixed.len(),
        fixed.join("\n  ")
    );
}
