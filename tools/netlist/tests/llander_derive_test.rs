//! `netlist derive` against Lunar Lander's transcription: the throttle's eight
//! settings, and the check that the generated file is current.
//!
//! The throttle is three switched resistors into one node with `C15` on it, so
//! the same parts set the volume and the corner (`phosphor-emulator-b72s`).
//! The device takes both from `llander_sound_derived.rs`. These tests hold that
//! file to the spec, and hold the solver's answer to the one hand formula that
//! is believed right once the load the prose left out is put back.

use phosphor_netlist::derive;
use phosphor_netlist::netlist::Netlist;
use std::path::PathBuf;

fn spec_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/schematics/netlists/llander-audio.derive.toml")
}

fn llander() -> Netlist {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/schematics/netlists/llander-audio.toml");
    Netlist::load(&path).expect("the board's transcription should load")
}

fn value(netlist: &Netlist, designator: &str) -> f64 {
    netlist
        .part(designator)
        .unwrap_or_else(|| panic!("{designator} is on the sheet"))
        .value
        .quantity()
        .unwrap_or_else(|| panic!("{designator} carries a quantity"))
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

fn generated() -> String {
    derive::generate(&spec_path())
        .unwrap_or_else(|e| panic!("should generate: {e:?}"))
        .text
}

/// The file the device compiles is the file the spec writes.
#[test]
fn the_generated_file_is_current() {
    let generated = derive::generate(&spec_path()).expect("should generate");
    let on_disk = std::fs::read_to_string(&generated.out).expect("the file is committed");
    assert!(
        on_disk == generated.text,
        "{} is stale. Run:\n  cargo run -p phosphor-netlist -- derive \
         docs/schematics/netlists/llander-audio.derive.toml",
        generated.out.display()
    );
}

/// Throttle 1 is `R18` alone, which the hand formula can do once it has both
/// things the prose's 10.6 Hz left out: the `4066`'s on-resistance in series
/// with the leg, and the band-pass input's `R22` plus `R26` to +5 V in parallel
/// with it. With those the formula and the solver should agree to within the
/// band-pass capacitors' small share of the mode, which is what they cost a
/// reading of `C15` alone.
#[test]
fn throttle_1_is_r18_against_the_band_pass_input() {
    let board = llander();
    let text = generated();
    let leg = value(&board, "R18") + 80.0;
    let load = value(&board, "R22") + value(&board, "R26");
    let c15 = value(&board, "C15");

    let tau = leg * load / (leg + load) * c15;
    let gain = load / (leg + load);
    let solved_tau = constant(&text, "THROTTLE_1_C15_TAU");
    let solved_gain = constant(&text, "THROTTLE_1_GAIN");
    assert!(
        (solved_tau - tau).abs() / tau < 0.005,
        "solved {solved_tau:e} s against the formula's {tau:e} s"
    );
    assert!(
        (solved_gain - gain).abs() < 1e-3,
        "solved {solved_gain} against the formula's {gain}"
    );

    // And the prose's figure, the leg alone, is the one this replaces.
    let prose_tau = value(&board, "R18") * c15;
    assert!(
        solved_tau / prose_tau < 0.8,
        "the load should shorten throttle 1's time constant by a quarter or more"
    );
}

/// Every setting closes a set of legs whose parallel resistance falls as the
/// setting rises (15k, 8.2k, their parallel, 3.9k, and so on), so the time
/// constant falls and the gain rises monotonically. Neither is linear in the
/// setting, and throttle 0 drives nothing.
#[test]
fn the_throttle_is_monotonic_and_throttle_0_is_silent() {
    let text = generated();
    let tau: Vec<f64> = (0..8)
        .map(|n| constant(&text, &format!("THROTTLE_{n}_C15_TAU")))
        .collect();
    let gain: Vec<f64> = (0..8)
        .map(|n| constant(&text, &format!("THROTTLE_{n}_GAIN")))
        .collect();
    assert_eq!(gain[0], 0.0, "no leg is closed at throttle 0");
    for n in 1..8 {
        assert!(tau[n] < tau[n - 1], "tau should fall: {tau:?}");
        assert!(gain[n] > gain[n - 1], "gain should rise: {gain:?}");
    }
    // Compressed, not linear: throttle 1's DC gain is most of throttle 7's.
    assert!(gain[1] / gain[7] > 0.75, "{gain:?}");
}
