//! `netlist derive` against Asteroids' transcription: the mix, the three filters
//! whose voice nets the mix loads, and the check that the generated file is
//! current.
//!
//! The device takes every relative level from `asteroids_sound_derived.rs`
//! (`phosphor-emulator-kfby.6`). These tests hold that file to the spec, and
//! hold the solver's answers to the few hand formulas that are right once the
//! summing node is known to sit at the op-amp's +5 V.

use phosphor_netlist::derive;
use phosphor_netlist::netlist::Netlist;
use std::path::PathBuf;

fn spec_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/schematics/netlists/asteroids-audio.derive.toml")
}

fn asteroids() -> Netlist {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/schematics/netlists/asteroids-audio.toml");
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
         docs/schematics/netlists/asteroids-audio.derive.toml",
        generated.out.display()
    );
}

/// Each voice's weight is minus R86 over its own summing resistor, and thrust
/// and explosion are equal. That equality is the board's; the reference's adder
/// had them at 600 and 1000.
#[test]
fn each_voice_enters_the_mix_at_r86_over_its_summing_resistor() {
    let board = asteroids();
    let text = generated();
    let r86 = value(&board, "R86");
    for (name, resistor) in [
        ("MIX_THUMP", "R83"),
        ("MIX_SAUCER", "R77"),
        ("MIX_LIFE", "R78"),
        ("MIX_SAUCER_FIRE", "R81"),
        ("MIX_SHIP_FIRE", "R84"),
        ("MIX_EXPLOSION", "R82"),
        ("MIX_THRUST", "R102"),
    ] {
        let expected = -r86 / value(&board, resistor);
        let solved = constant(&text, name);
        assert!(
            (solved - expected).abs() < 1e-5,
            "{name}: solved {solved} against -R86/{resistor} = {expected}"
        );
    }
    assert_eq!(
        constant(&text, "MIX_THRUST"),
        constant(&text, "MIX_EXPLOSION")
    );
}

/// The four explosion legs together are a divider from the gates into R82's
/// load at the held summing node, so their gains sum to R82 over R82 plus the
/// legs in parallel, and C24 sees those two resistances in parallel.
#[test]
fn the_explosion_legs_are_loaded_by_r82() {
    let board = asteroids();
    let text = generated();
    let legs = 1.0
        / ["R40", "R43", "R41", "R42"]
            .iter()
            .map(|r| 1.0 / value(&board, r))
            .sum::<f64>();
    let r82 = value(&board, "R82");
    let sum: f64 = (0..4)
        .map(|n| constant(&text, &format!("EXPLOSION_LEG{n}_GAIN")))
        .sum();
    let expected = r82 / (r82 + legs);
    assert!(
        (sum - expected).abs() < 1e-4,
        "legs sum to {sum}, divider gives {expected}"
    );

    let tau = legs * r82 / (legs + r82) * value(&board, "C24");
    let solved = constant(&text, "EXPLOSION_C24_TAU");
    assert!(
        (solved - tau).abs() / tau < 1e-3,
        "solved {solved:e} s against {tau:e} s"
    );
    // And the legs alone, which the device used to use, are well off it.
    assert!(solved / (legs * value(&board, "C24")) < 0.7);
}

/// Thump's R74 and C64 are loaded by R83, so the corner is R74 in parallel
/// with R83 and the DC gain the divider between them.
#[test]
fn thumps_output_filter_is_loaded_by_r83() {
    let board = asteroids();
    let text = generated();
    let (r74, r83, c64) = (
        value(&board, "R74"),
        value(&board, "R83"),
        value(&board, "C64"),
    );
    let tau = r74 * r83 / (r74 + r83) * c64;
    let gain = r83 / (r74 + r83);
    let solved_tau = constant(&text, "THUMP_C64_TAU");
    let solved_gain = constant(&text, "THUMP_C64_GAIN");
    assert!(
        (solved_tau - tau).abs() / tau < 1e-3,
        "{solved_tau:e} against {tau:e}"
    );
    assert!(
        (solved_gain - gain).abs() < 1e-4,
        "{solved_gain} against {gain}"
    );
}
