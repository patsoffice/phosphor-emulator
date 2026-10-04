//! The small-signal solve against Lunar Lander's transcription, with its three
//! LM324 sections as ideal op-amps (`phosphor-emulator-kfby.5`).
//!
//! Every figure here was once hand arithmetic, and one of them was wrong for
//! want of a part: `C27` across `R31` was in the transcription the whole time
//! and two analyses of the mixer left it out. These tests hold the solver's
//! answers, read from the netlist alone, against the formulas the device
//! builds its legs from, so the device and the drawing are compared through
//! something that cannot forget a capacitor.

use phosphor_netlist::netlist::Netlist;
use phosphor_netlist::solve::{Cx, Network, Setup};
use std::collections::BTreeMap;
use std::f64::consts::TAU;
use std::path::PathBuf;

fn llander() -> Netlist {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/schematics/netlists/lunarlander-audio.toml");
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

/// Full throttle, the explosion switch as given, op-amps ideal.
fn network(netlist: &Netlist, explosion: bool) -> Network {
    let drives: BTreeMap<String, f64> = [
        ("noise out", 3.8),
        ("AUD0", 5.0),
        ("AUD1", 5.0),
        ("AUD2", 5.0),
        ("AUD3", if explosion { 5.0 } else { 0.0 }),
    ]
    .into_iter()
    .map(|(net, v)| (net.to_string(), v))
    .collect();
    Network::build(
        netlist,
        &Setup {
            drives,
            ideal_op_amps: true,
            ..Setup::default()
        },
    )
    .unwrap_or_else(|e| panic!("should build: {e:?}"))
}

/// `node` per volt of the common node at `hz`, which is how the device's legs
/// are parameterized: each takes the common node as its input.
fn from_common(net: &Network, node: &str, hz: f64) -> Cx {
    let point = &net.ac("noise out", &[hz]).expect("should solve")[0];
    point.at(node).unwrap() / point.at("common node").unwrap()
}

/// The summing amp's feedback, `R31` with `C27` across it, in ohms.
fn feedback(board: &Netlist, hz: f64) -> Cx {
    let r31 = value(board, "R31");
    let c27 = value(board, "C27");
    Cx::new(r31, 0.0) / Cx::new(1.0, TAU * hz * r31 * c27)
}

#[test]
fn all_three_sections_are_solved_as_ideal_op_amps() {
    let net = network(&llander(), false);
    assert_eq!(net.op_amps.len(), 3, "{:?} {:?}", net.op_amps, net.excluded);
}

/// The band-pass: its peak falls at 89.5 Hz and its gain there, from the
/// common node, is `R27/(2 R22)` reduced by the `R22`/`R26` divider's loading,
/// the 2.87 the device's `op_amp_band_pass` produces from the same parts.
#[test]
fn the_band_pass_peaks_at_89_5_hz_with_a_gain_of_2_87() {
    let board = llander();
    let net = network(&board, false);
    let sweep: Vec<f64> = (800..=1000).map(|t| t as f64 / 10.0).collect();
    let points = net.ac("noise out", &sweep).expect("should solve");
    let (peak_hz, peak) = points
        .iter()
        .map(|p| {
            (
                p.hz,
                (p.at("band-pass out").unwrap() / p.at("common node").unwrap()).abs(),
            )
        })
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap();
    assert!((peak_hz - 89.5).abs() < 0.3, "the peak is at {peak_hz} Hz");
    assert!((peak - 2.87).abs() < 0.01, "the peak gain is {peak}");
}

/// The thrust leg at the band center: the band-pass, then `R28` into the
/// summing amp with `C27` across its feedback. 3.68, where the figure the
/// doc first carried, with `R31` read as a flat 10k, was 4.22.
#[test]
fn the_thrust_leg_at_89_5_hz_includes_c27() {
    let board = llander();
    let net = network(&board, false);
    let hz = 89.5;
    let solved = from_common(&net, "AUDIO1", hz).abs();
    let band_pass = from_common(&net, "band-pass out", hz).abs();
    let formula = band_pass * feedback(&board, hz).abs() / value(&board, "R28");
    assert!(
        (solved - formula).abs() / formula < 1e-3,
        "solved {solved}, formula {formula}"
    );
    assert!((solved - 3.68).abs() < 0.01, "{solved}");
    let without_c27 = band_pass * value(&board, "R31") / value(&board, "R28");
    assert!(
        without_c27 - solved > 0.5,
        "C27 should take the leg from {without_c27} down to {solved}"
    );
}

/// The explosion leg: `R21` in series with `C91` into the same summing amp.
/// Its contribution is the difference between AUDIO1 with the switch closed
/// and open, each per volt of the common node, and it should be the device's
/// formula at every frequency: a band-pass flat at `C91/C27` = 0.47 between
/// 159 Hz and 2258 Hz, which is not the high-pass the doc first described.
///
/// The formula includes the closed `4066` section's on-resistance in series
/// with `R21`, because the solver does and the first run of this test said
/// so: without it the two part 2.6 % at 2258 Hz, where `R21` dominates the
/// leg. The device omits it, which costs the leg about 0.4 dB at the top of
/// its band.
#[test]
fn the_explosion_leg_is_the_c91_over_c27_band_pass() {
    let board = llander();
    let open = network(&board, false);
    let closed = network(&board, true);
    let switch = board
        .part("P5")
        .and_then(|p| p.switches.iter().find(|s| s.control == "13"))
        .and_then(|s| s.ohms)
        .expect("the explosion switch carries its on-resistance");
    let (r21, c91) = (value(&board, "R21") + switch, value(&board, "C91"));
    for hz in [159.0, 600.0, 2258.0, 5000.0] {
        let solved = (from_common(&closed, "AUDIO1", hz) - from_common(&open, "AUDIO1", hz)).abs();
        let leg = Cx::new(r21, 0.0) + Cx::new(1.0, 0.0) / Cx::new(0.0, TAU * hz * c91);
        let formula = (feedback(&board, hz) / leg).abs();
        assert!(
            (solved - formula).abs() / formula < 0.01,
            "at {hz} Hz: solved {solved}, formula {formula}"
        );
    }
    let plateau =
        (from_common(&closed, "AUDIO1", 600.0) - from_common(&open, "AUDIO1", 600.0)).abs();
    assert!((plateau - 0.44).abs() < 0.01, "{plateau}");
}

/// Something the device does not model, found by the same solve: closing the
/// explosion switch loads the common node through `R21` and `C91`, and takes
/// it down by about a third of a decibel at 159 Hz. Pinned so that a device
/// change that starts to model it has a figure to meet, and so that the size
/// of what is being left out is on record.
#[test]
fn the_explosion_switch_loads_the_common_node_slightly() {
    let board = llander();
    let at = |explosion: bool| {
        network(&board, explosion)
            .ac("noise out", &[159.0])
            .unwrap()[0]
            .at("common node")
            .unwrap()
            .abs()
    };
    let db = 20.0 * (at(true) / at(false)).log10();
    assert!((-0.6..-0.1).contains(&db), "{db} dB");
}
