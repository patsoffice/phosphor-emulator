//! The solver against the real transcription, on the question rung 5 exists
//! to answer.
//!
//! `docs/designs/schematic-transcription.md` names one question first: node X
//! is written in `machines/src/zaxxon_sound.rs` as a superposition of two
//! one-pole sections, and the justification is that the two poles are 26 ms
//! and 760 ms, twenty-nine to one, "which is exactly the separation that lets
//! a two-capacitor network be written as two independent RCs". That was an
//! assertion and nothing had ever checked it.
//!
//! These tests are the check, and they are here rather than beside the solver
//! because they are about the board. Each pins one of the device's three
//! numbers against the whole network solved at once, so a later change to
//! either the transcription or the solver that moves the answer has to say so.
//!
//! **The device no longer computes those three numbers.** `shot_pitch_r`,
//! `shot_vca_release_r` and `shot_pitch_from_y` were replaced by constants
//! `netlist derive` solves out of this same network; see
//! `zaxxon_derive_test.rs`. The tests here still stand, because the device
//! still writes node X as two one-pole sections, and it is the one-capacitor
//! reading landing within a few percent of the network that says it may.

use phosphor_netlist::netlist::Netlist;
use phosphor_netlist::solve::{Mode, Network, Setup};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// The board's transcription, from the crate rather than from a cwd.
fn zaxxon() -> Netlist {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/schematics/netlists/zaxxon-sound.toml");
    Netlist::load(&path).expect("the board's transcription should load")
}

/// The shot voice's passive network, with the one-shot's output held high.
///
/// High is the recovery phase, which is when these two time constants are what
/// the voice is doing: `Qbar` returns to +5 V, `D10` goes reverse biased, and
/// node Y climbs back through `C89` while node X follows it through `R147`.
fn shot() -> Network {
    let netlist = zaxxon().subset("shot");
    let setup = Setup {
        drives: BTreeMap::from([("Qbar".to_string(), 5.0)]),
        ..Setup::default()
    };
    Network::build(&netlist, &setup).expect("the shot voice should extract")
}

/// The mode nearest a stated time constant, which is how a mode is identified:
/// by where it sits, never by its position in the list.
fn near(modes: &[Mode], tau: f64) -> &Mode {
    modes
        .iter()
        .min_by(|a, b| {
            (a.tau / tau)
                .ln()
                .abs()
                .total_cmp(&(b.tau / tau).ln().abs())
        })
        .expect("the network has modes")
}

/// How much of a mode appears at a node.
fn share(mode: &Mode, net: &str) -> f64 {
    mode.shape
        .iter()
        .find(|(name, _)| name == net)
        .map_or(0.0, |(_, v)| *v)
}

/// A part's value, out of the transcription.
///
/// **Both sides of every comparison below come from this file**, which is the
/// point. An earlier draft of these tests typed the device's figures in as
/// literals, so the test failed if the transcription moved and said nothing if
/// somebody edited `zaxxon_sound.rs`. That is the epic's own "copies drift"
/// failure mode, reintroduced by the rung built to close doors. What is being
/// asserted here is the *relationship*: that reading one capacitor at a time
/// out of these values lands where solving them together does.
fn value(netlist: &Netlist, designator: &str) -> f64 {
    netlist
        .part(designator)
        .unwrap_or_else(|| panic!("{designator} is on the sheet"))
        .value
        .quantity()
        .unwrap_or_else(|| panic!("{designator} carries a quantity"))
}

/// Two resistances in parallel.
fn parallel(a: f64, b: f64) -> f64 {
    a * b / (a + b)
}

/// The three figures `zaxxon_sound.rs` derived by reading one capacitor at a
/// time, recomputed from the same transcription the solver reads: the corner
/// `shot_pitch_r` gives, the recovery `shot_vca_release_r` gives, and the
/// share `shot_pitch_from_y` gives.
fn one_rc_at_a_time() -> (f64, f64, f64) {
    let n = zaxxon();
    let up = value(&n, "R145") + value(&n, "R146");
    let r147 = value(&n, "R147");
    (
        // C88 against R145/R146 in parallel with R147: node Y is an AC ground
        // at this rate, so R148 is not in the path.
        value(&n, "C88") * parallel(up, r147),
        // C89 against R148 in parallel with R147 plus R145/R146: C88 is an
        // open at this rate, so node X is a divider rather than a ground.
        value(&n, "C89") * parallel(value(&n, "R148"), r147 + up),
        // How much of node Y reaches node X.
        up / (up + r147),
    )
}

/// The headline. `shot_pitch_r` reads 26 ms and `shot_vca_release_r` reads
/// 760 ms, each derived by treating the other capacitor as a short or as an
/// open. Solving both capacitors together gives 25.9 ms and 777 ms, so the
/// separation argument holds and the approximation costs under three percent.
///
/// **That is a door closed rather than a residual found**, which is the result
/// this rung was built to be able to produce. The shot's head sits an octave
/// above the board's and this is not where it is.
#[test]
fn the_two_pole_superposition_on_node_x_is_within_three_percent_of_the_network() {
    let modes = shot().modes().expect("should solve");

    let fast = near(&modes, 26e-3);
    let slow = near(&modes, 760e-3);
    let (device_fast, device_slow, _) = one_rc_at_a_time();

    let fast_error = (fast.tau - device_fast).abs() / device_fast;
    let slow_error = (slow.tau - device_slow).abs() / device_slow;
    assert!(
        fast_error < 0.03,
        "the fast pole is {:.2} ms and reading C88 alone gives {:.2}: {:.1} % out",
        fast.tau * 1e3,
        device_fast * 1e3,
        fast_error * 100.0
    );
    assert!(
        slow_error < 0.03,
        "the slow pole is {:.1} ms and reading C89 alone gives {:.1}: {:.1} % out",
        slow.tau * 1e3,
        device_slow * 1e3,
        slow_error * 100.0
    );
}

/// Why the separation argument works, which is a stronger claim than that its
/// answer is close. `shot_pitch_r` drops `R148` on the reading that at 4 Hz
/// `C89` holds node Y to an AC ground. If that is right, node Y barely moves in
/// the fast mode, and it does not: four percent, against the node the mode
/// lives on.
#[test]
fn node_y_is_an_ac_ground_in_the_fast_mode_which_is_what_drops_r148() {
    let modes = shot().modes().expect("should solve");
    let fast = near(&modes, 26e-3);
    assert!(
        share(fast, "node X").abs() > 0.99,
        "the fast mode lives on node X: {:?}",
        fast.shape
    );
    assert!(
        share(fast, "node Y").abs() < 0.10,
        "node Y is held by C89 at this rate: {:?}",
        fast.shape
    );
}

/// And the other half of the same argument, which is the one the device turns
/// into a number. `shot_vca_release_r` keeps `R145`/`R146` on the reading that
/// at 0.2 Hz `C88` is an open, so node X is not a ground but a divider that
/// node Y drags with it. `shot_pitch_from_y` calls that share 0.560. Solving
/// the network gives 0.579 for the same quantity, from no divider arithmetic
/// at all.
#[test]
fn node_x_takes_the_share_of_node_y_the_device_gives_it() {
    let modes = shot().modes().expect("should solve");
    let slow = near(&modes, 760e-3);
    assert!(
        share(slow, "node Y").abs() > 0.99,
        "the slow mode lives on node Y: {:?}",
        slow.shape
    );
    let coupled = share(slow, "node X");
    let (_, _, device) = one_rc_at_a_time();
    assert!(
        (coupled - device).abs() / device < 0.05,
        "node X takes {coupled:.3} of the slow mode against the divider's {device:.3}"
    );
}

/// The two readings this device replaced, checked against the network rather
/// than against each other. `R148` in the fast path gave 43 ms and leaving
/// `R145`/`R146` out of the slow one gave 468 ms, and both were carried for a
/// while. Neither is within a factor of the network's answer, so the solver
/// confirms the corrections independently of the recording that motivated them.
#[test]
fn the_two_superseded_readings_are_refuted_by_the_network() {
    let modes = shot().modes().expect("should solve");
    let fast = near(&modes, 26e-3).tau;
    let slow = near(&modes, 760e-3).tau;
    assert!(
        (fast - 43e-3).abs() / 43e-3 > 0.3,
        "43 ms put R148 in the fast path; the network says {} ms",
        fast * 1e3
    );
    assert!(
        (slow - 468e-3).abs() / 468e-3 > 0.3,
        "468 ms left R145/R146 out of the recovery; the network says {} ms",
        slow * 1e3
    );
}

/// The design's second question, and the answer is narrower than the question.
///
/// Node A peaks at 2.07 V in the running device and the board's has to sit near
/// 0.5 V. With the 555 parked at its loaded output high the solver puts the
/// node at 2.22 V, so the **resistive reading is right**: `R153`, `R154`,
/// `R155` and the `R157`/`R158` leg divide as the device says they do, to a
/// couple of percent. Decision 6's table then reads solver-agrees-with-device
/// and both-disagree-with-board, which is its fourth row: a part property, or
/// something outside the passive network. It does not say which, and this test
/// does not pretend otherwise.
#[test]
fn node_a_bounds_where_the_device_puts_it_so_the_gap_is_not_in_the_resistors() {
    let netlist = zaxxon().subset("shot");
    let setup = Setup {
        drives: BTreeMap::from([
            // The 555's own output high on +12 V, loaded.
            ("U18 out".to_string(), 10.3),
            // U19 section A at the floor it reaches late in the voice.
            ("net 21".to_string(), 0.1),
        ]),
        ..Setup::default()
    };
    let network = Network::build(&netlist, &setup).expect("should extract");
    let dc = network.dc().expect("should solve");
    let a = dc
        .iter()
        .find(|(net, _)| net == "node A")
        .expect("node A is in the network")
        .1;
    assert!(
        (a - 2.2).abs() < 0.1,
        "node A bounds at {a:.3} V, against the device's traced 2.07"
    );
    assert!(
        a > 1.5,
        "the board needs about 0.5 V here and the resistors cannot give it: {a:.3} V"
    );
}

/// The solver's answers rest on every unmodeled pin being open, so the list of
/// them is part of the result rather than an appendix. These three are the ones
/// the shot's question turns on: if either op-amp input drew current, or if the
/// 555's control pin loaded node A, the modes above would be about a different
/// network.
#[test]
fn the_pins_the_answer_rests_on_are_reported_open() {
    let network = shot();
    for pin in ["U19.12", "U20.3", "U18.5"] {
        assert!(
            network.opened.contains(&pin.to_string()),
            "{pin} should be reported open: {:?}",
            network.opened
        );
    }
}

/// `C94` is the part this epic exists because of, and the honest answer about
/// it is that the solver has nothing to say. The MB4391's rolloff pin is the
/// only thing at the other end of it, that part has no datasheet, and a
/// capacitor to an open pin carries no current. So it is excluded **by name**
/// rather than quietly grounded, which is decision 7's line between "we
/// genuinely do not know" and "we did not check".
#[test]
fn the_rolloff_capacitor_is_excluded_by_name_rather_than_assumed_away() {
    let network = shot();
    assert!(
        network.excluded.iter().any(|line| line.contains("C94")),
        "{:?}",
        network.excluded
    );
    assert!(
        !network.capacitors().contains(&"C94".to_string()),
        "a capacitor on an open pin is not a mode"
    );
}

/// Whether the one-shot's output is held or left open moves the fast pole by
/// half a percent, because either way `C88` sees hundreds of ohms where the
/// rest of the network has megohms. Worth pinning: it is the one drive the
/// headline result is taken with, and a reader is owed the knowledge that it
/// does not carry the answer.
#[test]
fn holding_the_one_shots_output_barely_moves_the_answer() {
    let netlist = zaxxon().subset("shot");
    let open = Network::build(&netlist, &Setup::default()).expect("should extract");
    let driven = shot();

    let a = near(&open.modes().expect("should solve"), 26e-3).tau;
    let b = near(&driven.modes().expect("should solve"), 26e-3).tau;
    assert!(
        (a - b).abs() / a < 0.01,
        "the fast pole is {} ms open and {} ms driven",
        a * 1e3,
        b * 1e3
    );
}
