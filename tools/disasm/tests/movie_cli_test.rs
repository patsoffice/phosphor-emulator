//! End-to-end tests for the movie subcommands, driving the real `disasm`
//! binary.
//!
//! These invoke the compiled CLI rather than calling into its functions, because
//! the thing worth pinning is the *command surface*: that `movie info` works
//! with no ROM set at all, that `movie check` prints a hash comparable with
//! `frames.toml`, and that a malformed movie fails with a readable message
//! instead of a panic.
//!
//! `movie info` needs neither ROMs nor a machine, so its tests run everywhere.
//! `replay` and `movie check` boot a machine and are ROM-gated with the usual
//! convention.

use std::path::{Path, PathBuf};
use std::process::Command;

use phosphor_harness::movie::{MovieHeader, MovieRecorder, rom_digest};
use phosphor_harness::{Harness, Movie, load_rom_set, roms_dir};
use phosphor_machines::registry;

/// The compiled binary under test, provided by cargo to integration tests.
const DISASM: &str = env!("CARGO_BIN_EXE_disasm");

/// A per-process scratch directory, so concurrent test binaries do not collide.
fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("phosphor-movie-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn write_movie(name: &str, movie: &Movie) -> PathBuf {
    let path = scratch().join(name);
    std::fs::write(&path, movie.encode()).expect("write movie");
    path
}

struct Output {
    stdout: String,
    stderr: String,
    ok: bool,
}

fn disasm(args: &[&str]) -> Output {
    let out = Command::new(DISASM)
        .args(args)
        .output()
        .expect("run disasm");
    Output {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        ok: out.status.success(),
    }
}

/// A movie that needs no machine to exist — enough to exercise `movie info`.
fn synthetic_movie() -> Movie {
    let mut rec = MovieRecorder::new(
        "mrdo",
        [0x5A; 32],
        &[],
        vec![0x3F, 0x00],
        Some(vec![1, 2, 3]),
    );
    rec.push_marker("start of the interesting bit");
    rec.advance_frame();
    rec.push_release_all();
    rec.push_dip(0, 0x41);
    rec.advance_frame();
    rec.advance_frame();
    rec.finish()
}

// ---------------------------------------------------------------------------
// movie info — no ROMs, no machine
// ---------------------------------------------------------------------------

#[test]
fn movie_info_describes_a_movie_without_a_rom_set() {
    let path = write_movie("info.phmi", &synthetic_movie());
    let out = disasm(&["movie", "info", path.to_str().unwrap()]);
    assert!(out.ok, "movie info failed: {}{}", out.stdout, out.stderr);

    for expected in [
        "machine:     mrdo",
        "frames:      3",
        "0x3f",
        "3 bytes",
        "release-all",
        "start of the interesting bit",
    ] {
        assert!(
            out.stdout.contains(expected),
            "movie info output missing {expected:?}:\n{}",
            out.stdout
        );
    }
}

#[test]
fn movie_info_reports_a_corrupt_file_readably() {
    let mut bytes = synthetic_movie().encode();
    let mid = bytes.len() / 2;
    bytes[mid] ^= 0xFF;
    let path = scratch().join("corrupt.phmi");
    std::fs::write(&path, &bytes).expect("write");

    let out = disasm(&["movie", "info", path.to_str().unwrap()]);
    assert!(!out.ok, "a corrupt movie must not succeed");
    assert!(
        out.stderr.contains("digest does not match"),
        "expected a readable digest error, got: {}{}",
        out.stdout,
        out.stderr
    );
}

#[test]
fn movie_info_reports_a_missing_file_readably() {
    let out = disasm(&["movie", "info", "/nonexistent/nope.phmi"]);
    assert!(!out.ok);
    assert!(
        out.stderr.contains("reading movie"),
        "expected a readable read error, got: {}{}",
        out.stdout,
        out.stderr
    );
}

// ---------------------------------------------------------------------------
// replay / movie check — ROM-gated
// ---------------------------------------------------------------------------

/// Record a short real session and return the movie plus where it was written.
fn record_against_roms(dir: &Path) -> Option<(String, PathBuf)> {
    let all = registry::all();
    let entry = all.iter().find(|e| {
        e.rom_names
            .iter()
            .any(|n| dir.join(format!("{n}.zip")).exists())
    })?;

    let roms = dir.to_str().unwrap();
    let set = load_rom_set(roms, entry.rom_names).ok()?;
    let digest = rom_digest(&set, entry.rom_names);

    let mut h = Harness::build(entry.name, roms, None, None, &[], &[]).ok()?;
    let controls = h.machine().input_controls();
    let dip: Vec<u8> = (0..h.machine().dip_banks().len())
        .map(|b| h.machine().dip_bank_value(b))
        .collect();
    let mut rec = MovieRecorder::new(entry.name, digest, controls, dip, None);

    // A short, entirely unremarkable session: the point is that replay
    // reproduces it, not that it reaches anything in particular.
    for frame in 0..30 {
        if frame == 10
            && let Some(c) = controls.iter().find(|c| c.stable_name == "coin")
        {
            let e = phosphor_core::core::machine::InputEvent::Button {
                id: c.id,
                pressed: true,
            };
            rec.push_event(e);
            h.machine_mut().handle_input(e);
        }
        h.run_frame();
        rec.advance_frame();
    }
    let movie = rec.finish();
    Some((entry.name.to_string(), write_movie("booted.phmi", &movie)))
}

#[test]
fn movie_check_prints_a_frame_hash_comparable_with_frames_toml() {
    let Some(dir) = roms_dir() else {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
        return;
    };
    let Some((machine, path)) = record_against_roms(&dir) else {
        eprintln!("skipping: the ROM directory supplies no registered machine");
        return;
    };

    let out = disasm(&[
        "movie",
        "check",
        path.to_str().unwrap(),
        dir.to_str().unwrap(),
    ]);
    assert!(out.ok, "movie check failed: {}{}", out.stdout, out.stderr);
    assert!(out.stdout.contains(&machine), "output:\n{}", out.stdout);
    assert!(
        out.stdout.contains("frame:   sha256:"),
        "expected a frames.toml-shaped hash, got:\n{}",
        out.stdout
    );

    // Deterministic: the same movie checked twice prints the same hash.
    let again = disasm(&[
        "movie",
        "check",
        path.to_str().unwrap(),
        dir.to_str().unwrap(),
    ]);
    assert_eq!(out.stdout, again.stdout, "movie check is not deterministic");
}

#[test]
fn replay_writes_a_png_of_the_frame_it_reaches() {
    let Some(dir) = roms_dir() else {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
        return;
    };
    let Some((_, path)) = record_against_roms(&dir) else {
        eprintln!("skipping: the ROM directory supplies no registered machine");
        return;
    };

    let png = scratch().join("replay.png");
    let out = disasm(&[
        "replay",
        "--movie",
        path.to_str().unwrap(),
        "-o",
        png.to_str().unwrap(),
        dir.to_str().unwrap(),
    ]);
    assert!(out.ok, "replay failed: {}{}", out.stdout, out.stderr);
    assert!(png.exists(), "replay did not write {}", png.display());
    assert!(
        std::fs::metadata(&png).unwrap().len() > 0,
        "replay wrote an empty PNG"
    );
}

#[test]
fn replay_refuses_a_movie_recorded_against_other_roms() {
    let Some(dir) = roms_dir() else {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
        return;
    };
    let all = registry::all();
    let Some(entry) = all.iter().find(|e| {
        e.rom_names
            .iter()
            .any(|n| dir.join(format!("{n}.zip")).exists())
    }) else {
        eprintln!("skipping: the ROM directory supplies no registered machine");
        return;
    };

    let movie = Movie {
        header: MovieHeader {
            machine: entry.name.to_string(),
            rom_digest: [0xEE; 32],
            controls: Vec::new(),
            dip: Vec::new(),
            nvram: None,
            host_sample_rate: 44_100,
            frames: 1,
        },
        records: Vec::new(),
    };
    let path = write_movie("wrongroms.phmi", &movie);

    let out = disasm(&[
        "movie",
        "check",
        path.to_str().unwrap(),
        dir.to_str().unwrap(),
    ]);
    assert!(!out.ok, "a mismatched ROM digest must not succeed");
    assert!(
        out.stderr.contains("different ROM set"),
        "expected a ROM mismatch error, got: {}{}",
        out.stdout,
        out.stderr
    );
}
