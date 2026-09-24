//! `netlist derive` against the real transcription: the probe's kill
//! criterion, and the check that the generated file is current.
//!
//! `phosphor-emulator-kfby.2` asks two questions of the shot's three generated
//! constants, and both test one thing, which is whether the generator derives
//! or transcribes. Does it agree with the hand derivation wherever that is
//! believed right? And does it reproduce the two corrections that hand
//! derivation needed, 43 ms to 26 and 468 ms to 760, without being told them?
//!
//! The second is asked more sharply here than as "is the answer not 43". Each
//! superseded figure is what the network gives with one capacitor treated as
//! a short or an open, which was the judgment the prose made. So the same
//! spec is run on those two altered networks too. A generator that is reading
//! the network gives the old answer on the old network and the corrected one
//! on the drawn one; a generator that had been handed an answer would give the
//! same number on all three.

use phosphor_netlist::derive::{self, Spec};
use phosphor_netlist::netlist::{Kind, Netlist, Value};
use std::path::{Path, PathBuf};

fn spec_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/schematics/netlists/zaxxon-sound.derive.toml")
}

fn zaxxon() -> Netlist {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/schematics/netlists/zaxxon-sound.toml");
    Netlist::load(&path).expect("the board's transcription should load")
}

fn spec() -> Spec {
    derive::load_spec(&spec_path()).expect("the spec should load")
}

/// A part's value, out of the transcription, so the hand formulas below read
/// the same numbers the solver does.
fn value(netlist: &Netlist, designator: &str) -> f64 {
    netlist
        .part(designator)
        .unwrap_or_else(|| panic!("{designator} is on the sheet"))
        .value
        .quantity()
        .unwrap_or_else(|| panic!("{designator} carries a quantity"))
}

fn parallel(a: f64, b: f64) -> f64 {
    a * b / (a + b)
}

/// One constant out of generated text.
fn constant(text: &str, name: &str) -> f64 {
    let line = text
        .lines()
        .find(|line| line.contains(&format!("const {name}:")))
        .unwrap_or_else(|| panic!("{name} was generated:\n{text}"));
    let rhs = line.split('=').nth(1).expect("a constant has a value");
    let literal = rhs.split(';').next().expect("a constant ends").trim();
    literal
        .parse()
        .unwrap_or_else(|_| panic!("`{literal}` is a number"))
}

/// Run the spec against a netlist, keeping only the named constants: an
/// altered network may lack a capacitor that another constant asks for.
fn generate(netlist: &Netlist, keep: &[&str]) -> String {
    let mut spec = spec();
    for scenario in &mut spec.scenarios {
        scenario.tau.retain(|t| keep.contains(&t.name.as_str()));
        scenario.share.retain(|s| keep.contains(&s.name.as_str()));
    }
    derive::render(&spec, Path::new("zaxxon-sound.derive.toml"), netlist)
        .unwrap_or_else(|e| panic!("should generate: {e:?}"))
}

/// The file the device compiles is the file the spec writes. Without this the
/// generated constants are a fourth hand copy with a header claiming
/// otherwise, which is the failure the design's decision 4 names.
#[test]
fn the_generated_file_is_current() {
    let generated = derive::generate(&spec_path()).expect("should generate");
    let on_disk = std::fs::read_to_string(&generated.out).expect("the file is committed");
    assert!(
        on_disk == generated.text,
        "{} is stale. Run:\n  cargo run -p phosphor-netlist -- derive \
         docs/schematics/netlists/zaxxon-sound.derive.toml",
        generated.out.display()
    );
}

/// The device no longer names `C88` in a constant of its own: the part reaches
/// it only through `SHOT_C88_TAU`, in the generated module. `lint --device`
/// has to follow that module, or the part this probe made the device model
/// more exactly is the one the lint would call unmodeled.
#[test]
fn the_lint_sees_the_parts_the_device_models_through_generated_constants() {
    let device =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../machines/src/zaxxon_sound.rs");
    let parts = phosphor_netlist::lint::DeviceParts::from_path(&device).expect("readable");
    assert!(parts.named.contains("C88"), "C88 should count as modeled");
    assert!(
        !parts.values.contains_key("C88"),
        "a time constant is not C88's farads"
    );
}

/// The kill criterion's first question. The hand derivations read one
/// capacitor at a time and the generator solves both at once, so they should
/// differ by about the coupling between the two modes and no more. Each mode
/// keeps 97.7 % of its energy in its own capacitor, and node Y moves 4 % of
/// node X in the fast one, so a few percent is the expected gap and a factor
/// would be a finding.
#[test]
fn the_generated_values_agree_with_the_hand_derivations_to_within_the_coupling() {
    let n = zaxxon();
    let text = generate(&n, &["SHOT_C88_TAU", "SHOT_C89_TAU", "SHOT_C89_X_PER_Y"]);
    let up = value(&n, "R145") + value(&n, "R146");
    let r147 = value(&n, "R147");
    let r148 = value(&n, "R148");

    // What `shot_pitch_r`, `shot_vca_release_r` and `shot_pitch_from_y` wrote.
    let cases = [
        ("SHOT_C88_TAU", value(&n, "C88") * parallel(up, r147), 0.03),
        (
            "SHOT_C89_TAU",
            value(&n, "C89") * parallel(r148, r147 + up),
            0.03,
        ),
        ("SHOT_C89_X_PER_Y", up / (up + r147), 0.04),
    ];
    for (name, hand, within) in cases {
        let solved = constant(&text, name);
        let off = (solved - hand).abs() / hand;
        assert!(
            off < within,
            "{name}: solved {solved:.5} against the hand derivation's {hand:.5}, {:.1} % out",
            off * 100.0
        );
    }
}

/// The two networks the superseded readings assumed, as altered copies of the
/// drawn one. `C89` removed is "node Y is not held", which put `R148` in node
/// X's path. `C88` replaced by a milliohm is "node X is held", which left
/// `R145`/`R146` out of node Y's.
fn with_c89_open() -> Netlist {
    let mut n = zaxxon();
    n.parts.retain(|p| p.designator != "C89");
    for net in &mut n.nets {
        net.on.retain(|e| e.part != "C89");
    }
    n
}

fn with_c88_shorted() -> Netlist {
    let mut n = zaxxon();
    let c88 = n
        .parts
        .iter_mut()
        .find(|p| p.designator == "C88")
        .expect("C88 is on the sheet");
    c88.kind = Kind::R;
    c88.value = Value::Ohms(1e-3);
    n
}

/// The kill criterion's second question, for the first correction. `43 ms` was
/// `C88` against `R145`/`R146` in parallel with `R147 + R148`. The generator
/// gives that number when, and only when, the network it is handed has no
/// `C89`, and gives 26 ms on the network the sheet draws.
#[test]
fn the_generator_gives_43_ms_only_on_the_network_that_reading_assumed() {
    let drawn = zaxxon();
    let up = value(&drawn, "R145") + value(&drawn, "R146");
    let old = value(&drawn, "C88") * parallel(up, value(&drawn, "R147") + value(&drawn, "R148"));
    assert!((old - 43e-3).abs() < 1e-3, "the old reading was {old} s");

    let wrong = constant(
        &generate(&with_c89_open(), &["SHOT_C88_TAU"]),
        "SHOT_C88_TAU",
    );
    let right = constant(&generate(&drawn, &["SHOT_C88_TAU"]), "SHOT_C88_TAU");
    eprintln!(
        "C88's mode: {:.2} ms by hand then, {:.2} ms with C89 open, {:.2} ms as drawn",
        old * 1e3,
        wrong * 1e3,
        right * 1e3
    );
    assert!(
        (wrong - old).abs() / old < 0.01,
        "with C89 open the generator should reproduce {:.2} ms, and gives {:.2}",
        old * 1e3,
        wrong * 1e3
    );
    assert!(
        (right - old).abs() / old > 0.3,
        "on the drawn network it should not: {:.2} ms",
        right * 1e3
    );
}

/// And the second. `468 ms` was `C89` against `R147` in parallel with `R148`,
/// node X taken as held. The generator gives it with `C88` shorted and 777 ms
/// on the drawn network.
#[test]
fn the_generator_gives_468_ms_only_on_the_network_that_reading_assumed() {
    let drawn = zaxxon();
    let old = value(&drawn, "C89") * parallel(value(&drawn, "R147"), value(&drawn, "R148"));
    assert!((old - 468e-3).abs() < 1e-3, "the old reading was {old} s");

    let wrong = constant(
        &generate(&with_c88_shorted(), &["SHOT_C89_TAU"]),
        "SHOT_C89_TAU",
    );
    let right = constant(&generate(&drawn, &["SHOT_C89_TAU"]), "SHOT_C89_TAU");
    eprintln!(
        "C89's mode: {:.1} ms by hand then, {:.1} ms with C88 shorted, {:.1} ms as drawn",
        old * 1e3,
        wrong * 1e3,
        right * 1e3
    );
    assert!(
        (wrong - old).abs() / old < 0.01,
        "with C88 shorted the generator should reproduce {:.1} ms, and gives {:.1}",
        old * 1e3,
        wrong * 1e3
    );
    assert!(
        (right - old).abs() / old > 0.3,
        "on the drawn network it should not: {:.1} ms",
        right * 1e3
    );
}
