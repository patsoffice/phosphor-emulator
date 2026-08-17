//! Golden-frame regression: does each machine still draw the same picture?
//!
//! `boot_check_test.rs` asserts a machine reaches its attract mode and lights
//! *something*. This file asserts it lights the *same* something it did when
//! the frame was pinned — the level at which a swapped palette entry, a sprite
//! drawn one line high, a scroll latch read from the wrong register or a tile
//! bank off by one actually shows up.
//!
//! The pins live in `tests/golden/frames.toml` as data, one `[[frame]]` table
//! per machine, so refreshing a machine is a reviewable diff rather than an
//! edited hex literal. Each entry also carries a committed reference PNG at
//! `tests/golden/<machine>.png`, which is not the source of truth (the hash is)
//! but makes a refresh show up in review as a picture. See
//! `docs/designs/frame-regression.md`.
//!
//! # Running
//!
//! ```text
//! cargo test -p phosphor-harness --test golden_frame_test
//! PHOSPHOR_GOLDEN_ONLY=galaga cargo test -p phosphor-harness --test golden_frame_test
//! PHOSPHOR_GOLDEN_UPDATE=1    cargo test -p phosphor-harness --test golden_frame_test
//! ```
//!
//! Update mode recaptures, rewrites `frames.toml` and the reference PNGs, and
//! reports what changed. The human-authored fields (`frames`, `shows`, `press`)
//! round-trip; the hashes and `size` are outputs.
//!
//! # Gating
//!
//! The comparison needs real ROMs, so it skips without a ROM directory
//! (`PHOSPHOR_ROMS`, else `~/ws/mame-runtime/roms`) and skips per machine for a
//! set this collection cannot supply — the convention `boot_check_test.rs` and
//! `save_state_rom_test.rs` already use. The three consistency tests at the
//! bottom of this file need no ROMs, so CI still enforces that every registered
//! machine is pinned, that every pin describes itself, and that every reference
//! PNG matches its hash.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use phosphor_core::core::machine::{FrontendMachine, Orientation};
use phosphor_core::device::dvg::VectorLine;
use phosphor_core::gfx::apply_orientation;
use phosphor_harness::{Harness, PressSpec, roms_dir};
use phosphor_machines::registry;
use sha2::{Digest, Sha256};

/// Frames a newly discovered machine is captured at before a human picks a
/// better number. Past every registered machine's power-on self-test — the
/// slowest (Road Runner initialising a blank EEPROM, Star Wars clearing its RAM
/// test) need a couple of thousand.
const DEFAULT_FRAMES: usize = 1800;

/// Placeholder `shows` text written for a machine update mode has just
/// discovered. `every_entry_is_described` fails while any survives, so a
/// bootstrapped entry cannot be committed without someone looking at the frame.
const TODO_SHOWS: &str = "TODO: describe what this frame shows";

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

/// `harness/tests/golden/`, holding `frames.toml`, the reference PNGs, and the
/// git-ignored `actual/` directory failures are written to.
fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

fn frames_toml() -> PathBuf {
    golden_dir().join("frames.toml")
}

fn reference_png(machine: &str) -> PathBuf {
    golden_dir().join(format!("{machine}.png"))
}

fn actual_png(machine: &str) -> PathBuf {
    golden_dir().join("actual").join(format!("{machine}.png"))
}

// ---------------------------------------------------------------------------
// The pinned data
// ---------------------------------------------------------------------------

/// One scripted button pulse, mirroring `disasm frameshot --press`.
struct Press {
    control: String,
    at: usize,
    hold: usize,
}

/// One pinned frame: what to run, what a human saw when it was pinned, and the
/// fingerprints it has to keep reproducing.
struct Entry {
    machine: String,
    /// Frames from reset before the frame is sampled.
    frames: usize,
    /// What the pinned frame depicts, in prose. Mandatory: the pin is only
    /// worth anything if someone looked at the picture first.
    shows: String,
    /// Scripted input, for machines whose attract mode is not representative.
    press: Vec<Press>,
    /// Optional factory NVRAM fixture, relative to `tests/golden/`, loaded
    /// straight after reset exactly as `disasm frameshot --nvram` does.
    ///
    /// Needed by the Williams machines: with blank battery RAM the board
    /// factory-resets and holds `FACTORY SETTINGS RESTORED` until an operator
    /// presses reset, as on hardware, so they only reach attract mode with an
    /// initialised CMOS.
    nvram: Option<String>,
    /// Oriented display dimensions, pinned separately from the hash because a
    /// geometry change is legible in a diff and a hash change is not.
    size: (u32, u32),
    /// SHA-256 of the oriented RGB frame.
    frame: String,
    /// SHA-256 of the vector display list, for vector machines only.
    vectors: Option<String>,
}

/// Parse `frames.toml`. Every malformed field is a panic, not a skip: the file
/// is the test's input, and a typo that silently dropped an entry would be a
/// vacuous pass.
fn load_entries() -> Vec<Entry> {
    let path = frames_toml();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let doc: toml::Value =
        toml::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()));

    let tables = match doc.get("frame") {
        Some(toml::Value::Array(a)) => a.clone(),
        // An absent or empty array is legal to parse and useless to test
        // against, so name it here rather than letting the suite pass on zero
        // entries.
        _ => Vec::new(),
    };

    tables
        .iter()
        .map(|t| {
            let machine = t
                .get("machine")
                .and_then(|v| v.as_str())
                .unwrap_or_else(|| panic!("a [[frame]] entry has no `machine`: {t}"))
                .to_string();
            let field = |name: &str| -> &toml::Value {
                t.get(name)
                    .unwrap_or_else(|| panic!("{machine}: [[frame]] entry has no `{name}`"))
            };
            let usize_field = |name: &str| -> usize {
                field(name)
                    .as_integer()
                    .and_then(|i| usize::try_from(i).ok())
                    .unwrap_or_else(|| panic!("{machine}: `{name}` is not a non-negative integer"))
            };
            let str_field = |name: &str| -> String {
                field(name)
                    .as_str()
                    .unwrap_or_else(|| panic!("{machine}: `{name}` is not a string"))
                    .to_string()
            };

            let frames = usize_field("frames");
            assert!(frames > 0, "{machine}: `frames` must be at least 1");

            let size = match field("size").as_array().map(Vec::as_slice) {
                Some([toml::Value::Integer(w), toml::Value::Integer(h)]) if *w > 0 && *h > 0 => {
                    (*w as u32, *h as u32)
                }
                _ => panic!("{machine}: `size` must be [width, height], both positive"),
            };

            let press = match t.get("press") {
                None => Vec::new(),
                Some(toml::Value::Array(items)) => items
                    .iter()
                    .map(|p| {
                        let control = p
                            .get("control")
                            .and_then(|v| v.as_str())
                            .unwrap_or_else(|| panic!("{machine}: a `press` has no `control`"))
                            .to_string();
                        let num = |name: &str, default: i64| -> usize {
                            let v = p.get(name).and_then(|v| v.as_integer()).unwrap_or(default);
                            usize::try_from(v).unwrap_or_else(|_| {
                                panic!("{machine}: press `{control}` has a negative `{name}`")
                            })
                        };
                        let at = num("at", 0);
                        let hold = num("hold", 8);
                        // A press scheduled at or past the sampled frame never
                        // fires, which reads as "the input did nothing" rather
                        // than as a mis-typed frame number.
                        assert!(
                            at < frames,
                            "{machine}: press `{control}` at frame {at} never fires — the \
                             entry only runs {frames} frames"
                        );
                        assert!(hold > 0, "{machine}: press `{control}` has a zero `hold`");
                        Press { control, at, hold }
                    })
                    .collect(),
                Some(other) => panic!("{machine}: `press` must be an array of tables, got {other}"),
            };

            Entry {
                shows: str_field("shows"),
                frame: str_field("frame"),
                vectors: t.get("vectors").map(|v| {
                    v.as_str()
                        .unwrap_or_else(|| panic!("{machine}: `vectors` is not a string"))
                        .to_string()
                }),
                nvram: t.get("nvram").map(|v| {
                    v.as_str()
                        .unwrap_or_else(|| panic!("{machine}: `nvram` is not a string"))
                        .to_string()
                }),
                machine,
                frames,
                press,
                size,
            }
        })
        .collect()
}

/// Header of the generated `frames.toml`. Update mode rewrites the whole file,
/// so this is the only place a comment survives — everything a reader needs to
/// know about an individual entry belongs in its `shows`.
const FRAMES_TOML_HEADER: &str = "\
# Golden frames: what each machine is supposed to draw.
#
# Generated by `PHOSPHOR_GOLDEN_UPDATE=1 cargo test -p phosphor-harness --test
# golden_frame_test`, which recaptures every entry and rewrites this file. The
# human-authored fields round-trip and the machine-authored ones do not:
#
#   machine  registry name
#   frames   frames from reset before the frame is sampled  (human)
#   shows    what the pinned frame depicts, in prose        (human)
#   press    optional scripted input, as disasm --press     (human)
#   nvram    optional factory CMOS fixture, relative to here (human)
#   size     oriented display dimensions                    (captured)
#   frame    SHA-256 of the oriented RGB frame              (captured)
#   vectors  SHA-256 of the vector display list             (captured, vector games)
#
# A refresh should show up here as a hash change with a reviewed image diff in
# tests/golden/<machine>.png beside it. See docs/designs/frame-regression.md.
#
# Every registered machine must appear, as a [[frame]] or — when no frame can
# be captured at all — as an [[unpinned]] with a reason.
";

/// A registered machine deliberately left without a pinned frame.
///
/// The only reason that has come up is a ROM set no available collection can
/// supply, but the shape is general: an unpinned machine is an unguarded one,
/// so the opt-out costs a written justification rather than silence.
struct Unpinned {
    machine: String,
    reason: String,
}

/// Parse the `[[unpinned]]` opt-outs. Same panic-on-malformed policy as
/// [`load_entries`].
fn load_unpinned() -> Vec<Unpinned> {
    let path = frames_toml();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let doc: toml::Value =
        toml::from_str(&text).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()));
    let Some(toml::Value::Array(items)) = doc.get("unpinned") else {
        return Vec::new();
    };
    items
        .iter()
        .map(|t| {
            let get = |name: &str| -> String {
                t.get(name)
                    .and_then(|v| v.as_str())
                    .unwrap_or_else(|| panic!("an [[unpinned]] entry has no `{name}`: {t}"))
                    .to_string()
            };
            Unpinned {
                machine: get("machine"),
                reason: get("reason"),
            }
        })
        .collect()
}

/// Render `entries` back to the canonical `frames.toml` text, sorted by machine.
fn render_frames_toml(entries: &[Entry], unpinned: &[Unpinned]) -> String {
    let mut sorted: Vec<&Entry> = entries.iter().collect();
    sorted.sort_by(|a, b| a.machine.cmp(&b.machine));
    let mut unpinned: Vec<&Unpinned> = unpinned.iter().collect();
    unpinned.sort_by(|a, b| a.machine.cmp(&b.machine));

    let quote = |s: &str| toml::Value::String(s.to_string()).to_string();
    let mut out = String::from(FRAMES_TOML_HEADER);
    for u in unpinned {
        out.push_str("\n[[unpinned]]\n");
        let _ = writeln!(out, "machine = {}", quote(&u.machine));
        let _ = writeln!(out, "reason = {}", quote(&u.reason));
    }
    for e in sorted {
        out.push_str("\n[[frame]]\n");
        let _ = writeln!(out, "machine = {}", quote(&e.machine));
        let _ = writeln!(out, "frames = {}", e.frames);
        let _ = writeln!(out, "shows = {}", quote(&e.shows));
        if !e.press.is_empty() {
            out.push_str("press = [\n");
            for p in &e.press {
                let _ = writeln!(
                    out,
                    "    {{ control = {}, at = {}, hold = {} }},",
                    quote(&p.control),
                    p.at,
                    p.hold
                );
            }
            out.push_str("]\n");
        }
        if let Some(nv) = &e.nvram {
            let _ = writeln!(out, "nvram = {}", quote(nv));
        }
        let _ = writeln!(out, "size = [{}, {}]", e.size.0, e.size.1);
        let _ = writeln!(out, "frame = {}", quote(&e.frame));
        if let Some(v) = &e.vectors {
            let _ = writeln!(out, "vectors = {}", quote(v));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Capture and fingerprint
// ---------------------------------------------------------------------------

/// SHA-256 over a length-prefixed encoding of the frame, so a buffer that
/// changes shape without changing bytes still changes the hash.
fn hash_frame(w: u32, h: u32, rgb: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"phosphor-frame-v1");
    hasher.update(w.to_le_bytes());
    hasher.update(h.to_le_bytes());
    hasher.update(rgb);
    format!("sha256:{:x}", hasher.finalize())
}

/// SHA-256 over the vector display list — for the vector games this, not the
/// rasterised frame, is what the frontend actually draws.
fn hash_vectors(lines: &[VectorLine]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"phosphor-vectors-v1");
    hasher.update((lines.len() as u32).to_le_bytes());
    for l in lines {
        for c in [l.x0, l.y0, l.x1, l.y1] {
            hasher.update(c.to_le_bytes());
        }
        hasher.update([l.intensity, l.r, l.g, l.b]);
    }
    format!("sha256:{:x}", hasher.finalize())
}

/// Render the booted machine the way `disasm frameshot` and the frontend do:
/// native buffer, then the machine's declared orientation applied centrally.
/// Hashing the *oriented* frame means a machine that loses its rotation
/// declaration fails here.
fn render_oriented(m: &mut dyn FrontendMachine) -> (u32, u32, Vec<u8>) {
    let (nw, nh) = m.display_size();
    let mut native = vec![0u8; nw as usize * nh as usize * 3];
    m.render_frame(&mut native);
    let orient = m.orientation();
    if orient == Orientation::NORMAL {
        return (nw, nh, native);
    }
    let (dw, dh) = if orient.swaps_axes() {
        (nh, nw)
    } else {
        (nw, nh)
    };
    let mut oriented = vec![0u8; dw as usize * dh as usize * 3];
    apply_orientation(&native, &mut oriented, nw as usize, nh as usize, orient);
    (dw, dh, oriented)
}

/// What a run of one entry produced.
struct Capture {
    size: (u32, u32),
    rgb: Vec<u8>,
    frame: String,
    vectors: Option<String>,
    /// Vectors on the sampled frame, for the "is this pin worth anything"
    /// guard and for the failure message.
    vector_count: usize,
}

/// Boot the entry's machine and run it to its pinned frame, or `None` when this
/// ROM collection cannot supply it.
///
/// Every ROM-load failure is a skip for the reason `boot_check_test.rs` spells
/// out: a local collection often holds *a* ZIP for a game but not the revision
/// a machine's ROM table names. Everything past the load is asserted normally.
fn capture(dir: &Path, entry: &Entry) -> Option<Capture> {
    let name = entry.machine.as_str();
    let reg = registry::find(name)
        .unwrap_or_else(|| panic!("{name} is pinned in frames.toml but is not registered"));
    if !reg
        .rom_names
        .iter()
        .any(|n| dir.join(format!("{n}.zip")).exists())
    {
        eprintln!("skipping {name}: no ROM set in {}", dir.display());
        return None;
    }

    let presses: Vec<PressSpec> = entry
        .press
        .iter()
        .map(|p| PressSpec {
            control: p.control.clone(),
            at: p.at,
            hold: p.hold,
        })
        .collect();
    // A missing NVRAM fixture is a hard error, not a skip: it is committed
    // beside the hash, so its absence means the pin is unreproducible rather
    // than that this collection is short a ROM.
    let nvram = entry.nvram.as_ref().map(|n| {
        let p = golden_dir().join(n);
        assert!(
            p.is_file(),
            "{name}: `nvram = {n:?}` but {} does not exist",
            p.display()
        );
        p
    });
    let mut harness = match Harness::build(
        name,
        dir.to_str().unwrap(),
        nvram.as_deref(),
        None,
        &presses,
        &[],
    ) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("skipping {name}: {e}");
            return None;
        }
    };
    for _ in 0..entry.frames {
        harness.run_frame();
    }

    let vectors = harness
        .machine()
        .vector_display_list()
        .map(|l| (hash_vectors(l), l.len()));
    let (w, h, rgb) = render_oriented(harness.machine_mut());
    Some(Capture {
        frame: hash_frame(w, h, &rgb),
        size: (w, h),
        rgb,
        vector_count: vectors.as_ref().map_or(0, |(_, n)| *n),
        vectors: vectors.map(|(h, _)| h),
    })
}

// ---------------------------------------------------------------------------
// PNG
// ---------------------------------------------------------------------------

fn write_png(path: &Path, rgb: &[u8], w: u32, h: u32) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .unwrap_or_else(|e| panic!("creating {}: {e}", parent.display()));
    }
    let file =
        std::fs::File::create(path).unwrap_or_else(|e| panic!("creating {}: {e}", path.display()));
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()
        .and_then(|mut w| w.write_image_data(rgb))
        .unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
}

/// Decode an RGB8 PNG back into a tight RGB buffer.
fn read_png(path: &Path) -> (u32, u32, Vec<u8>) {
    let file =
        std::fs::File::open(path).unwrap_or_else(|e| panic!("opening {}: {e}", path.display()));
    let mut reader = png::Decoder::new(std::io::BufReader::new(file))
        .read_info()
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .unwrap_or_else(|e| panic!("decoding {}: {e}", path.display()));
    assert_eq!(
        info.color_type,
        png::ColorType::Rgb,
        "{}: reference PNGs are RGB8",
        path.display()
    );
    buf.truncate(info.buffer_size());
    (info.width, info.height, buf)
}

// ---------------------------------------------------------------------------
// The ROM-gated comparison
// ---------------------------------------------------------------------------

/// Restrict the run to one machine, for bisecting a failure or capturing a
/// single refresh: `PHOSPHOR_GOLDEN_ONLY=galaga`.
fn only() -> Option<String> {
    std::env::var("PHOSPHOR_GOLDEN_ONLY")
        .ok()
        .filter(|s| !s.is_empty())
}

fn updating() -> bool {
    std::env::var_os("PHOSPHOR_GOLDEN_UPDATE").is_some_and(|v| !v.is_empty() && v != "0")
}

/// Every pinned machine whose ROMs are present must still draw its pinned frame.
///
/// Failures are collected rather than asserted one at a time: when a shared
/// renderer changes, "these nine machines moved and these thirty-one did not"
/// is the diagnosis, and stopping at the first one hides it.
#[test]
fn every_pinned_machine_still_draws_its_frame() {
    let mut entries = load_entries();
    let update = updating();
    let only = only();

    let Some(dir) = roms_dir() else {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
        return;
    };

    // In update mode, a registered machine with no entry yet is captured at the
    // default frame count with placeholder prose — `every_entry_is_described`
    // then fails until a human has looked at the picture and written it up.
    if update {
        let unpinned = load_unpinned();
        let known: BTreeSet<&str> = entries
            .iter()
            .map(|e| e.machine.as_str())
            .chain(unpinned.iter().map(|u| u.machine.as_str()))
            .collect();
        let new: Vec<String> = registry::all()
            .iter()
            .map(|m| m.name.to_string())
            .filter(|n| !known.contains(n.as_str()))
            // Respect PHOSPHOR_GOLDEN_ONLY here as well: a discovered machine
            // the loop below skips would otherwise be written back with empty
            // hashes.
            .filter(|n| only.as_deref().is_none_or(|m| m == n))
            .collect();
        for machine in new {
            eprintln!("discovered {machine}: capturing at {DEFAULT_FRAMES} frames");
            entries.push(Entry {
                machine,
                frames: DEFAULT_FRAMES,
                shows: TODO_SHOWS.to_string(),
                press: Vec::new(),
                nvram: None,
                size: (0, 0),
                frame: String::new(),
                vectors: None,
            });
        }
    }

    let mut checked = Vec::new();
    let mut skipped = Vec::new();
    let mut changed = Vec::new();
    let mut failures = String::new();

    for entry in &mut entries {
        if only.as_deref().is_some_and(|m| m != entry.machine) {
            continue;
        }
        let name = entry.machine.clone();
        let Some(cap) = capture(&dir, entry) else {
            skipped.push(name);
            continue;
        };

        // A uniform frame is worth nothing as a pin: every possible breakage
        // reproduces it. This fires in update mode too, so a blank frame can
        // never be captured in the first place.
        assert!(
            cap.rgb.iter().any(|&b| b != cap.rgb[0]),
            "{name}: the frame at {} frames is a single flat colour — pinning it \
             would guard nothing. Pick a frame count past the machine's power-on \
             self-test.",
            entry.frames
        );
        if cap.vectors.is_some() {
            assert!(
                cap.vector_count > 0,
                "{name}: the vector display list is empty at {} frames, so its \
                 `vectors` hash would pin nothing",
                entry.frames
            );
        }

        if update {
            let was = (entry.frame.clone(), entry.vectors.clone(), entry.size);
            let now = (cap.frame.clone(), cap.vectors.clone(), cap.size);
            if was != now {
                changed.push(name.clone());
            }
            entry.size = cap.size;
            entry.frame = cap.frame;
            entry.vectors = cap.vectors;
            write_png(&reference_png(&name), &cap.rgb, cap.size.0, cap.size.1);
            checked.push(name);
            continue;
        }

        let mut problems = Vec::new();
        if cap.size != entry.size {
            problems.push(format!(
                "display is {}×{}, pinned {}×{}",
                cap.size.0, cap.size.1, entry.size.0, entry.size.1
            ));
        }
        if cap.frame != entry.frame {
            problems.push(format!("frame hash {}, pinned {}", cap.frame, entry.frame));
        }
        match (&cap.vectors, &entry.vectors) {
            (Some(got), Some(want)) if got != want => problems.push(format!(
                "vector list hash {got} ({} vectors), pinned {want}",
                cap.vector_count
            )),
            (Some(_), None) => {
                problems.push("machine now exposes a vector display list, none pinned".into())
            }
            (None, Some(_)) => problems.push(
                "a vector display list is pinned but the machine no longer exposes one".into(),
            ),
            _ => {}
        }

        if !problems.is_empty() {
            // Write the frame we actually got so the failure is inspectable as
            // a picture, not only as a hex diff.
            let actual = actual_png(&name);
            write_png(&actual, &cap.rgb, cap.size.0, cap.size.1);
            let _ = writeln!(
                failures,
                "\n{name} ({} frames — pinned as: {})\n  {}\n  \
                 wrote {}\n  compare: cargo run -p phosphor-disasm --bin disasm -- \
                 imgdiff {} {} --out /tmp/{name}_diff.png",
                entry.frames,
                entry.shows,
                problems.join("\n  "),
                actual.display(),
                reference_png(&name).display(),
                actual.display(),
            );
        }
        checked.push(name);
    }

    eprintln!(
        "golden frames: checked {}, skipped {} with no ROM set: {skipped:?}",
        checked.len(),
        skipped.len()
    );

    // A ROM directory that supplies nothing would otherwise pass having
    // compared zero frames.
    assert!(
        !checked.is_empty(),
        "the ROM directory {} exists but supplied no pinned machine's set{}",
        dir.display(),
        match &only {
            Some(m) => format!(" (PHOSPHOR_GOLDEN_ONLY={m})"),
            None => String::new(),
        }
    );

    if update {
        // A machine discovered above whose ROMs this collection cannot supply
        // never captured anything; writing it back would pin an empty hash.
        let kept: Vec<Entry> = entries
            .into_iter()
            .filter(|e| !e.frame.is_empty())
            .collect();
        let path = frames_toml();
        std::fs::write(&path, render_frames_toml(&kept, &load_unpinned()))
            .unwrap_or_else(|e| panic!("writing {}: {e}", path.display()));
        eprintln!(
            "updated {} ({} entr{} changed: {changed:?})",
            path.display(),
            changed.len(),
            if changed.len() == 1 { "y" } else { "ies" }
        );
        return;
    }

    assert!(
        failures.is_empty(),
        "{} of {} pinned machine(s) no longer draw their pinned frame:\n{failures}\n\
         If the change is intended, recapture with PHOSPHOR_GOLDEN_UPDATE=1 and \
         review the image diff.",
        failures.matches("\n  wrote ").count(),
        checked.len()
    );
}

// ---------------------------------------------------------------------------
// Consistency of the pinned data — no ROMs needed, so CI enforces these
// ---------------------------------------------------------------------------

/// Every registered machine is accounted for: pinned, or explicitly unpinned
/// with a written reason.
///
/// This is what makes the suite registry-driven: adding a machine fails here
/// until a golden frame is captured for it, because an unpinned machine is an
/// unguarded machine. The opt-out exists because a frame cannot always be
/// captured (a ROM set no collection can supply), and it costs a sentence
/// rather than silence.
#[test]
fn frames_toml_covers_every_registered_machine() {
    let pinned: BTreeSet<String> = load_entries().into_iter().map(|e| e.machine).collect();
    let unpinned = load_unpinned();

    for u in &unpinned {
        assert!(
            registry::find(&u.machine).is_some(),
            "{}: listed as [[unpinned]] but not registered",
            u.machine
        );
        assert!(
            !pinned.contains(&u.machine),
            "{}: is both pinned and [[unpinned]] — one of the two is stale",
            u.machine
        );
        assert!(
            u.reason.len() > 30,
            "{}: [[unpinned]] `reason` has to say why a frame cannot be \
             captured, not just that one is missing: {:?}",
            u.machine,
            u.reason
        );
    }

    let excused: BTreeSet<&str> = unpinned.iter().map(|u| u.machine.as_str()).collect();
    let missing: Vec<&str> = registry::all()
        .iter()
        .map(|m| m.name)
        .filter(|n| !pinned.contains(*n) && !excused.contains(n))
        .collect();
    assert!(
        missing.is_empty(),
        "registered machines with no golden frame: {missing:?}\n\
         Capture them with PHOSPHOR_GOLDEN_UPDATE=1 cargo test -p phosphor-harness \
         --test golden_frame_test, or add an [[unpinned]] entry saying why one \
         cannot be captured"
    );
    assert!(
        !registry::all().is_empty(),
        "the registry is empty, so this test would pass having checked nothing"
    );
}

/// Every entry names a distinct registered machine and describes itself.
#[test]
fn every_entry_is_described() {
    let entries = load_entries();
    assert!(
        !entries.is_empty(),
        "frames.toml holds no [[frame]] entries, so the golden suite would pass \
         having compared nothing"
    );

    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for e in &entries {
        assert!(
            registry::find(&e.machine).is_some(),
            "{}: pinned in frames.toml but not registered — a rename or removal \
             left this entry behind",
            e.machine
        );
        *seen.entry(e.machine.as_str()).or_default() += 1;
        assert_ne!(
            e.shows, TODO_SHOWS,
            "{}: still carries the placeholder description. Look at \
             tests/golden/{}.png and write down what it shows — an undescribed \
             pin cannot be reviewed.",
            e.machine, e.machine
        );
        assert!(
            e.shows.len() > 15,
            "{}: `shows` is too terse to be a description: {:?}",
            e.machine,
            e.shows
        );
        assert!(
            e.frame.starts_with("sha256:") && e.frame.len() == 71,
            "{}: `frame` is not a sha256 digest: {:?}",
            e.machine,
            e.frame
        );
    }
    let dupes: Vec<&&str> = seen
        .iter()
        .filter(|(_, n)| **n > 1)
        .map(|(m, _)| m)
        .collect();
    assert!(
        dupes.is_empty(),
        "duplicate [[frame]] entries for {dupes:?}"
    );
}

/// Every reference PNG decodes to exactly the frame its entry pins.
///
/// The PNG is a review aid, not the source of truth, so it has to be provably
/// the same capture as the hash beside it. This is also the check that makes a
/// hand-edited hash fail without ROMs.
#[test]
fn reference_pngs_match_their_hashes() {
    let entries = load_entries();
    let mut checked = 0;
    for e in &entries {
        let path = reference_png(&e.machine);
        assert!(
            path.exists(),
            "{}: no reference PNG at {}. Recapture with PHOSPHOR_GOLDEN_UPDATE=1.",
            e.machine,
            path.display()
        );
        let (w, h, rgb) = read_png(&path);
        assert_eq!(
            (w, h),
            e.size,
            "{}: reference PNG is {w}×{h}, entry pins {}×{}",
            e.machine,
            e.size.0,
            e.size.1
        );
        assert_eq!(
            hash_frame(w, h, &rgb),
            e.frame,
            "{}: reference PNG does not match the pinned `frame` hash — one of \
             the two was edited by hand, or the PNG is stale",
            e.machine
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "no reference PNGs were checked, so this test passed having verified nothing"
    );
}
