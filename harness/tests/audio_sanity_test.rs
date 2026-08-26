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
//! half-scale DC offset acceptable.
//!
//! # The fixture matters more than the metrics
//!
//! An input-free boot reaches attract mode, and *most arcade boards are silent
//! in attract mode* — they make no sound until a coin goes in. Measuring one
//! and concluding "this machine emits nothing" is a statement about the
//! fixture, not about the machine. Measured directly: booting Pac-Man,
//! Frogger and Tempest input-free yields digital zero for ten solid seconds,
//! while replaying thirty seconds of recorded play on the same builds yields
//! healthy audio — Pac-Man at -4.6 dBFS peak with a DC offset of -0.018.
//!
//! So this suite replays a recorded input movie whenever one exists for the
//! machine, and only falls back to an input-free boot when none does. The
//! fallback still catches a pinned or offset output, which is visible with or
//! without input; it cannot say anything about silence, so it does not try.
//!
//! The fallback cannot *retire* a `known_defect` entry either, for the same
//! reason in the other direction: attract mode drives a different set of voices
//! than play does, so a defect measured from a recording is not observable
//! there. A machine that fell back is reported as unchecked rather than as
//! fixed, so a movie that goes missing costs coverage instead of quietly
//! deleting the entries it used to hold up.
//!
//! # Gating
//!
//! No ROM directory (`PHOSPHOR_ROMS`, else `~/ws/mame-runtime/roms`) → skip, so
//! CI stays green without ROMs, exactly as `boot_check_test` does. A machine
//! whose own ROM set is missing skips individually, as does one whose movie was
//! recorded against a different ROM revision.
//!
//! Movies come from `PHOSPHOR_MOVIES`, else `~/.config/phosphor/movies`, named
//! `<machine>-*.phmi`. They are a local collection, not committed, so a machine
//! without one is normal and not a failure.
//!
//! # Expectations
//!
//! `tests/audio/expectations.toml` holds the thresholds and the per-machine
//! exceptions. A machine that legitimately breaks a default needs an entry with
//! a `reason`, which keeps the exceptions a reviewed claim about hardware
//! rather than a skip list that grows in silence.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

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
    // `toml::from_str` rather than `text.parse()`. As of toml 1.x, `FromStr for
    // Value` parses a single TOML *value* (`42`, `'s'`, `[1, 2]`, `{ x = 1 }`)
    // rather than a document, so a whole file fails with "unexpected content,
    // expected nothing". It still compiles, so only a run catches it.
    let doc: toml::Value =
        toml::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()));
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

/// Where recorded input movies live.
fn movies_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("PHOSPHOR_MOVIES") {
        let p = PathBuf::from(p);
        return p.is_dir().then_some(p);
    }
    let p = PathBuf::from(std::env::var("HOME").ok()?).join(".config/phosphor/movies");
    p.is_dir().then_some(p)
}

/// The movie for a machine, if the local collection has one.
///
/// Movies are named `<machine>-<timestamp>.phmi`; the newest wins so that
/// re-recording a session supersedes the old take without deleting it.
fn movie_for(machine: &str) -> Option<PathBuf> {
    let dir = movies_dir()?;
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().is_some_and(|x| x == "phmi")
                && p.file_stem()
                    .and_then(|s| s.to_str())
                    .is_some_and(|s| s.split('-').next() == Some(machine))
        })
        .collect();
    candidates.sort();
    candidates.pop()
}

/// Which fixture a measurement came from. Reported, because a silence result
/// means something completely different between the two.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fixture {
    /// Replayed recorded play: the machine had input, so silence is a defect.
    Movie,
    /// Input-free boot into attract mode: silence proves nothing.
    Boot,
}

/// Measure a machine's audio, or `None` if this collection cannot run it.
fn measure(dir: &Path, machine: &str) -> Option<(Integrity, Fixture)> {
    let _ = rom_set(dir, machine)?;
    let roms = dir.to_str()?;

    // Prefer recorded play. A movie recorded against a different ROM revision
    // is a skip, not a failure — the same judgement `boot_check_test` makes
    // about a ROM set the local collection cannot supply.
    let (mut harness, fixture, frames) = match movie_for(machine) {
        Some(path) => match Harness::build_with_movie(roms, &path) {
            Ok(h) => {
                let span = h
                    .movie()
                    .map(|p| p.movie().header.frames as usize)
                    .unwrap_or(FRAMES);
                (h, Fixture::Movie, span)
            }
            Err(e) => {
                eprintln!("{machine}: movie unusable ({e}); falling back to boot");
                (
                    Harness::build(machine, roms, None, None, &[], &[]).ok()?,
                    Fixture::Boot,
                    FRAMES,
                )
            }
        },
        None => (
            Harness::build(machine, roms, None, None, &[], &[]).ok()?,
            Fixture::Boot,
            FRAMES,
        ),
    };

    let rate = harness.machine().audio_sample_rate().max(1) as usize;
    let mut audio: Vec<i16> = Vec::new();
    let mut chunk = vec![0i16; rate];
    for frame in 0..frames {
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
    Some((Integrity::measure(&pcm_to_f64(&audio)), fixture))
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

    // Every entry in the file reached the parsed maps.
    //
    // This used to assert that at least one map was non-empty, as a proxy for
    // "the parse produced something". That proxy fails the day the last defect
    // is fixed, which is the state the ratchet exists to reach: it turned a
    // finished job into a red suite. Counting the file's own section headers
    // instead checks the same thing and still holds at zero.
    //
    // Headers only at column zero: the file's documentation quotes both names
    // in an indented comment, and a substring count would find those too.
    let text = std::fs::read_to_string(expectations_path()).expect("re-reading expectations");
    let headers = |name: &str| text.lines().filter(|l| l.trim_end() == name).count();
    assert_eq!(
        headers("[[known_defect]]"),
        e.known_defects.len(),
        "known_defect entries in the file did not all parse"
    );
    assert_eq!(
        headers("[[correct]]"),
        e.correct.len(),
        "correct entries in the file did not all parse"
    );
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

/// What one pass over the roster measured.
struct Sweep {
    /// Defects found, per machine. Machines with none are absent.
    found: BTreeMap<String, Vec<Defect>>,
    /// What every measured machine actually read, kept so the sweep can be
    /// reported rather than only judged. The issue this suite feeds is a table
    /// of these numbers, and without them that table has to be assembled by
    /// hand from a run that already computed it. See [`report`].
    measured: BTreeMap<String, Integrity>,
    /// Which fixture each measured machine ran under. A [`Fixture::Boot`]
    /// result describes attract mode, which drives a different set of voices
    /// than play does, so it can confirm neither the presence nor the absence
    /// of a defect that was pinned from a recording.
    fixtures: BTreeMap<String, Fixture>,
    /// Machines actually measured.
    checked: usize,
    /// Machines this collection cannot run.
    skipped: Vec<&'static str>,
}

/// Measure every machine once, then judge. Split from the assertions so both
/// directions of the ratchet read from one sweep.
///
/// **Once per process, not once per test.** Both ratchet tests call this, and
/// libtest runs them on separate threads, so a plain function meant the whole
/// roster was emulated twice over concurrently: the split above bought the
/// shared judgement it describes but not the shared work. Whichever test gets
/// here first does the sweep and the other waits on it.
fn sweep(dir: &Path) -> &'static Sweep {
    static SWEEP: OnceLock<Sweep> = OnceLock::new();
    SWEEP.get_or_init(|| sweep_uncached(dir))
}

fn sweep_uncached(dir: &Path) -> Sweep {
    let e = load_expectations();
    let mut found: BTreeMap<String, Vec<Defect>> = BTreeMap::new();
    let mut measured: BTreeMap<String, Integrity> = BTreeMap::new();
    let mut fixtures: BTreeMap<String, Fixture> = BTreeMap::new();
    let mut checked = 0usize;
    let mut skipped = Vec::new();

    // One machine's measurement cannot see another's, so they run on every
    // core. Judging stays sequential and in registry order, which is what keeps
    // the reported defect list stable between runs.
    let machines: Vec<&'static str> = registry::all().iter().map(|m| m.name).collect();
    let results = common::map_parallel(&machines, |name| measure(dir, name));

    for (entry, result) in machines.iter().zip(results) {
        let Some((integrity, fixture)) = result else {
            skipped.push(*entry);
            continue;
        };
        checked += 1;
        fixtures.insert(entry.to_string(), fixture);
        measured.insert(entry.to_string(), integrity);
        let mut defects = Vec::new();

        if integrity.dc_offset.abs() > e.defaults.max_dc_offset {
            defects.push(Defect::Dc);
        }
        if integrity.clipped_fraction > e.defaults.max_clipped_fraction {
            defects.push(Defect::Clipping);
        }
        // Only a movie can prove silence is wrong. An input-free boot sits in
        // attract mode, where most boards are legitimately silent, so calling
        // that a defect would be measuring the fixture.
        if fixture == Fixture::Movie
            && (integrity.is_silent || integrity.silent_fraction > e.defaults.max_silent_fraction)
        {
            defects.push(Defect::Silence);
        }
        if !defects.is_empty() {
            found.insert(entry.to_string(), defects);
        }
    }
    Sweep {
        found,
        measured,
        fixtures,
        checked,
        skipped,
    }
}

/// Print every machine's measurement, worst offset first.
///
/// Set `PHOSPHOR_AUDIO_REPORT=1` to get it. The sweep already computes these
/// numbers to decide pass or fail; this just stops them being thrown away, so
/// the table in the tracking issue can be regenerated from a run instead of
/// transcribed from one.
///
/// Reported for every machine, not only the failing ones: knowing that a
/// machine sits just under the threshold is what says whether a fix elsewhere
/// moved it, and a machine that is fine is the control group.
fn report(sweep: &Sweep, e: &Expectations) {
    let mut rows: Vec<(&String, &Integrity)> = sweep.measured.iter().collect();
    rows.sort_by(|a, b| {
        b.1.dc_offset
            .abs()
            .partial_cmp(&a.1.dc_offset.abs())
            .unwrap()
    });

    eprintln!(
        "\n{:<14} {:<7} {:>9} {:>9} {:>9} {:>10}  verdict",
        "machine", "fixture", "dc", "clipped", "silent", "peak dBFS"
    );
    for (machine, i) in rows {
        let fixture = sweep.fixtures.get(machine).map_or("-", |f| match f {
            Fixture::Movie => "movie",
            Fixture::Boot => "boot",
        });
        let known = e.known_defects.contains_key(machine);
        let defects = sweep.found.get(machine);
        let verdict = match (defects, known) {
            (None, true) => "LISTED BUT CLEAN".to_string(),
            (None, false) => String::new(),
            (Some(d), known) => {
                let names: Vec<&str> = d.iter().map(|k| k.as_str()).collect();
                format!(
                    "{}{}",
                    if known { "known: " } else { "NEW: " },
                    names.join("+")
                )
            }
        };
        eprintln!(
            "{:<14} {:<7} {:>+9.4} {:>8.2}% {:>8.2}% {:>10.2}  {}",
            machine,
            fixture,
            i.dc_offset,
            i.clipped_fraction * 100.0,
            i.silent_fraction * 100.0,
            i.peak_dbfs,
            verdict
        );
    }
    eprintln!(
        "\nthresholds: dc {:.3}, clipped {:.3}, silent {:.3}\n",
        e.defaults.max_dc_offset, e.defaults.max_clipped_fraction, e.defaults.max_silent_fraction
    );
}

/// No machine may be newly defective.
///
/// Failures are collected before reporting so one bad machine does not hide the
/// rest, the same reason `golden_frame_test` reports every mismatch at once.
#[test]
fn no_machine_emits_newly_defective_audio() {
    let Some(dir) = roms() else { return };
    let e = load_expectations();
    let s = sweep(&dir);
    if std::env::var("PHOSPHOR_AUDIO_REPORT").is_ok() {
        report(s, &e);
    }
    let Sweep {
        found,
        checked,
        skipped,
        ..
    } = s;
    let checked = *checked;

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
    for (machine, defects) in found {
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
    let Sweep {
        found,
        fixtures,
        skipped,
        ..
    } = sweep(&dir);

    let mut fixed: Vec<String> = Vec::new();
    let mut inconclusive: Vec<&str> = Vec::new();
    for (machine, known) in &e.known_defects {
        if skipped.contains(&machine.as_str()) {
            continue;
        }
        // A boot fixture cannot retire an entry. Attract mode drives a
        // different set of voices than play does, so a defect measured from a
        // recording simply is not observable here, and reporting it as fixed would
        // invite deleting an entry that still holds. This is not hypothetical:
        // a movie that goes missing, or one left behind by a format bump,
        // silently turns every one of its machine's entries into a false
        // "fixed".
        if fixtures.get(machine) != Some(&Fixture::Movie) {
            inconclusive.push(machine);
            continue;
        }
        let actual = found.get(machine).map(|v| v.as_slice()).unwrap_or(&[]);
        for kind in &known.kinds {
            if !actual.contains(kind) {
                fixed.push(format!("{machine}: {} is fixed", kind.as_str()));
            }
        }
    }

    if !inconclusive.is_empty() {
        eprintln!(
            "{} machine(s) measured from attract mode, so their known_defect entries \
             were not checked: {}. Record a movie for each to put them back under the \
             ratchet.",
            inconclusive.len(),
            inconclusive.join(", ")
        );
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
