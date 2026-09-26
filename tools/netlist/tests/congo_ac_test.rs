//! Congo Bongo's sound board, solved from its transcription
//! (`phosphor-emulator-7z54`): the four drum resonators, and every voice's
//! path to the board's output.
//!
//! None of these figures existed before the netlist. The device synthesizes
//! the drums by ear, and the prose it was checked against read the board at
//! block level and named the wrong filter. What the solve says, with the 3614
//! sections as ideal op-amps (the part is unidentified, which is the
//! assumption every figure here inherits):
//!
//! - each drum is a bridged-T resonator (the rim a twin-T), ringing at 73.4,
//!   265.5, 325.2 and 1079.9 Hz for the bass, the two congas and the rim, with
//!   Q of 10.8, 27.4, 33.6 and 23.3;
//! - each voice reaches SOU at exactly `R20` over its own mixing resistor,
//!   because SJ is the second summer's virtual ground, and each PSG at
//!   `(R18/R16) (R20/R19)`.
//!
//! The diodes in the shapers are open to the solver, so each drum is driven
//! at its post-diode node, "rectified". These are small-signal answers; how
//! hard the shaped pulse drives each resonator, and whether the 3614 clips,
//! is the device's to model.

use phosphor_netlist::netlist::Netlist;
use phosphor_netlist::solve::{AcPoint, Network, Setup};
use std::collections::BTreeMap;
use std::path::PathBuf;

fn congo() -> Netlist {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/schematics/netlists/congo-sound.toml");
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

fn network(board: &Netlist, drives: &[&str]) -> Network {
    let drives: BTreeMap<String, f64> = drives.iter().map(|n| (n.to_string(), 0.0)).collect();
    Network::build(
        board,
        &Setup {
            drives,
            ideal_op_amps: true,
            ..Setup::default()
        },
    )
    .unwrap_or_else(|e| panic!("should build: {e:?}"))
}

/// A log sweep from 20 Hz to 2 kHz at 400 points per decade.
fn sweep(net: &Network, source: &str) -> Vec<AcPoint> {
    let hz: Vec<f64> = (0..=800)
        .map(|i| 20.0 * 10f64.powf(i as f64 / 400.0))
        .collect();
    net.ac(source, &hz).expect("should solve")
}

/// The peak of `node`'s response, its frequency, and the Q from its -3 dB
/// points.
///
/// Found coarse and then refined: a log sweep fine enough for the whole band
/// is still too coarse for a Q above 20, whose -3 dB points are a few percent
/// apart, and an early version of this reported the congas' Q a third low
/// for exactly that reason.
fn resonance(net: &Network, source: &str, node: &str) -> (f64, f64, f64) {
    let coarse = sweep(net, source);
    let gain = |p: &AcPoint| p.at(node).unwrap().abs();
    let center = coarse
        .iter()
        .max_by(|a, b| gain(a).total_cmp(&gain(b)))
        .unwrap()
        .hz;
    let hz: Vec<f64> = (0..=4000)
        .map(|i| center * (0.8 + 0.4 * i as f64 / 4000.0))
        .collect();
    let fine = net.ac(source, &hz).expect("should solve");
    let g: Vec<f64> = fine.iter().map(gain).collect();
    let k = (0..g.len()).max_by(|&a, &b| g[a].total_cmp(&g[b])).unwrap();
    let half = g[k] / std::f64::consts::SQRT_2;
    let lo = (0..k).rev().find(|&i| g[i] < half).unwrap();
    let hi = (k..g.len()).find(|&i| g[i] < half).unwrap();
    (fine[k].hz, g[k], fine[k].hz / (fine[hi].hz - fine[lo].hz))
}

#[test]
fn every_3614_section_on_the_audio_path_that_can_be_solved_is() {
    let net = network(&congo(), &["bass rectified"]);
    let solved: Vec<&str> = net.op_amps.iter().map(|s| s.as_str()).collect();
    for section in ["U13.bass", "U13.conga-low", "U13.conga-high", "U13.rim"] {
        assert!(
            solved.iter().any(|s| s.starts_with(section)),
            "{section} is not among {solved:?}"
        );
    }
    assert!(
        solved.iter().any(|s| s.starts_with("U12.second summer")),
        "{solved:?}"
    );
}

/// Each drum's resonance, from the parts alone: frequency to a percent and Q
/// to three. The ring time constant a device wants is `Q / (pi f)`: 47 ms for
/// the bass, 33 ms for each conga, 6.9 ms for the rim.
#[test]
fn the_drums_ring_at_73_266_325_and_1080_hz() {
    let board = congo();
    for (voice, hz, q) in [
        ("bass", 73.41, 10.83),
        ("conga-low", 265.49, 27.43),
        ("conga-high", 325.17, 33.64),
        ("rim", 1079.94, 23.30),
    ] {
        let source = format!("{voice} rectified");
        let net = network(&board, &[&source]);
        let (peak, _, solved_q) = resonance(&net, &source, &format!("{voice} out"));
        assert!(
            (peak - hz).abs() / hz < 0.01,
            "{voice} rings at {peak} Hz, not {hz}"
        );
        assert!(
            (solved_q - q).abs() / q < 0.03,
            "{voice} has Q {solved_q}, not {q}"
        );
    }
}

/// SJ is U12 pin 13, a virtual ground, so each voice's resonator output
/// reaches SOU at its mixing resistor against `R20` and at nothing else. The
/// prose had this balance depending on the PSG chain's output impedance.
#[test]
fn each_drum_reaches_sou_at_r20_over_its_mixing_resistor() {
    let board = congo();
    let r20 = value(&board, "R20");
    for (voice, mixer) in [
        ("bass", "R30"),
        ("conga-low", "R40"),
        ("conga-high", "R50"),
        ("rim", "R62"),
    ] {
        let source = format!("{voice} rectified");
        let net = network(&board, &[&source]);
        let (peak_hz, _, _) = resonance(&net, &source, &format!("{voice} out"));
        let at = &net.ac(&source, &[peak_hz]).unwrap()[0];
        let ratio = at.at("SOU").unwrap().abs() / at.at(&format!("{voice} out")).unwrap().abs();
        let expected = r20 / value(&board, mixer);
        assert!(
            (ratio - expected).abs() / expected < 0.01,
            "{voice}: {ratio} at SOU per volt of its output, against {expected}"
        );
    }
}

/// And each PSG through both summers: `R18/R16` then `R20/R19`, 0.769.
#[test]
fn a_psg_reaches_sou_through_both_summers() {
    let board = congo();
    let net = network(&board, &["PSG1 out", "PSG2 out"]);
    let sou = net.ac("PSG1 out", &[1000.0]).unwrap()[0]
        .at("SOU")
        .unwrap()
        .abs();
    let expected =
        value(&board, "R18") / value(&board, "R17") * value(&board, "R20") / value(&board, "R19");
    assert!(
        (sou - expected).abs() / expected < 0.01,
        "{sou} against {expected}"
    );
}
