//! Unit tests for the discrete circuit skeleton: deterministic primitives, the
//! evaluation model (forward propagation, feedback back-edges, sample-and-hold,
//! event-driven domain), and save/load round-tripping.

use super::*;
use crate::core::save_state::{Saveable, StateReader, StateWriter};

/// A circuit whose board/sim/output rates are equal, so `tick(1)` runs exactly
/// one simulation step — convenient for asserting per-step behavior.
fn builder_1to1(rate: u64) -> DiscreteCircuitBuilder {
    DiscreteCircuitBuilder::new(rate, rate)
}

const RATE: u64 = 48_000;

// -- Math / routing ---------------------------------------------------------

#[test]
fn math_primitives() {
    let mut b = builder_1to1(RATE);
    let two = b.constant("two", 2.0);
    let three = b.constant("three", 3.0);
    let sum = b.add("sum", &[two, three]);
    let prod = b.multiply("prod", two, three);
    let scaled = b.gain("scaled", three, 10.0);
    let limited = b.clamp("limited", scaled, 0.0, 25.0);
    let mut c = b.build();

    c.tick(1);
    assert_eq!(c.value(sum), 5.0);
    assert_eq!(c.value(prod), 6.0);
    assert_eq!(c.value(scaled), 30.0);
    assert_eq!(c.value(limited), 25.0);
}

#[test]
fn forward_propagation_completes_in_one_step() {
    // A 3-deep chain must fully propagate within a single step thanks to the
    // topological evaluation order, not one node per step.
    let mut b = builder_1to1(RATE);
    let input = b.logic_input("IN");
    let g1 = b.gain("g1", input, 1.0);
    let g2 = b.gain("g2", g1, 1.0);
    let g3 = b.gain("g3", g2, 1.0);
    let mut c = b.build();

    c.set_logic(input, true);
    c.tick(1);
    assert_eq!(c.value(g3), 1.0, "value should reach the tail in one step");
}

#[test]
fn inverted_logic_input() {
    let mut b = builder_1to1(RATE);
    let inv = b.inverted_logic_input("NOT_IN");
    let mut c = b.build();

    c.tick(1);
    assert_eq!(c.value(inv), 1.0);
    c.set_logic(inv, true);
    c.tick(1);
    assert_eq!(c.value(inv), 0.0);
}

// -- Edge detector & pulse --------------------------------------------------

#[test]
fn edge_detector_fires_one_step() {
    let mut b = builder_1to1(RATE);
    let line = b.logic_input("LINE");
    let edge = b.edge_detector("EDGE", line);
    let mut c = b.build();

    c.tick(1);
    assert_eq!(c.value(edge), 0.0);
    c.set_logic(line, true);
    c.tick(1);
    assert_eq!(c.value(edge), 1.0, "rising edge");
    c.tick(1);
    assert_eq!(c.value(edge), 0.0, "no edge while held high");
}

#[test]
fn pulse_input_is_one_shot() {
    let mut b = builder_1to1(RATE);
    let p = b.pulse_input("NOISE_RESET");
    let mut c = b.build();

    c.pulse(p);
    c.tick(1);
    assert_eq!(c.value(p), 1.0, "pulse emitted");
    c.tick(1);
    assert_eq!(c.value(p), 0.0, "pulse consumed");
}

// -- Oscillators ------------------------------------------------------------

#[test]
fn fixed_square_phase_and_frequency() {
    // freq = rate/4 -> phase advances 0.25 per step -> +1,-1,-1,+1 repeating.
    let mut b = builder_1to1(RATE);
    let sq = b.fixed_square("SQ", RATE as f64 / 4.0);
    let mut c = b.build();

    let mut seq = Vec::new();
    for _ in 0..8 {
        c.tick(1);
        seq.push(c.value(sq));
    }
    assert_eq!(seq, vec![1.0, -1.0, -1.0, 1.0, 1.0, -1.0, -1.0, 1.0]);
}

#[test]
fn variable_square_tracks_frequency_input() {
    let mut b = builder_1to1(RATE);
    let freq = b.data_input("FREQ", 1.0);
    let sq = b.variable_square("VSQ", freq);
    let mut c = b.build();

    // Zero frequency: phase frozen, output constant.
    c.set_data(freq, 0.0);
    for _ in 0..4 {
        c.tick(1);
    }
    assert_eq!(c.value(sq), 1.0);

    // rate/4 frequency: toggles like the fixed-square case.
    c.set_data(freq, RATE as f64 / 4.0);
    c.tick(1); // phase 0.25 -> +1
    c.tick(1); // phase 0.50 -> -1
    assert_eq!(c.value(sq), -1.0);
}

// -- LFSR noise -------------------------------------------------------------

fn lfsr_circuit() -> DiscreteCircuit {
    let mut b = builder_1to1(RATE);
    // Clock the LFSR once per step (freq == sim rate).
    let noise = b.lfsr_noise(
        "NOISE",
        RATE as f64,
        LfsrSpec {
            width: 24,
            taps: (10, 23),
            seed: 0x1A_CFFC,
        },
    );
    b.output(noise, OutputGain::unity());
    b.build()
}

fn lfsr_sequence(c: &mut DiscreteCircuit, n: usize) -> Vec<f64> {
    let noise = NodeId(0);
    (0..n)
        .map(|_| {
            c.tick(1);
            c.value(noise)
        })
        .collect()
}

#[test]
fn lfsr_is_deterministic_and_not_constant() {
    let mut c = lfsr_circuit();
    let seq = lfsr_sequence(&mut c, 64);
    // Deterministic: a fresh identical circuit yields the same sequence.
    let mut c2 = lfsr_circuit();
    assert_eq!(seq, lfsr_sequence(&mut c2, 64));
    // Actually toggling (not stuck on one level).
    assert!(seq.iter().any(|&v| v > 0.0) && seq.iter().any(|&v| v < 0.0));
}

#[test]
fn lfsr_reset_restores_seed_sequence() {
    let mut c = lfsr_circuit();
    let before = lfsr_sequence(&mut c, 32);
    c.reset();
    let after = lfsr_sequence(&mut c, 32);
    assert_eq!(before, after, "reset should reload the seed");
}

// -- Evaluation model: feedback back-edge -----------------------------------

#[test]
fn feedback_back_edge_has_one_step_delay() {
    // acc = input + acc(previous): a self-referential node is a back-edge, so
    // it reads its own previous-step value -> integrator (+1 per step).
    let mut b = builder_1to1(RATE);
    let input = b.constant("IN", 1.0);
    let acc_id = b.next_id();
    let acc = b.add("ACC", &[input, acc_id]);
    assert_eq!(acc, acc_id);
    let mut c = b.build();

    for expected in 1..=5 {
        c.tick(1);
        assert_eq!(c.value(acc), expected as f64);
    }
}

// -- Evaluation model: cross-domain sample-and-hold -------------------------

#[test]
fn fixed_frequency_domain_holds_between_updates() {
    // The gain node re-evaluates only every 4th step (12 kHz at 48 kHz sim),
    // holding its slot in between even as its input changes.
    let mut b = builder_1to1(RATE);
    let ext = b.external_source("EXT");
    let g = b.gain("G", ext, 1.0);
    b.set_domain(g, ClockDomain::FixedFrequency(RATE as f64 / 4.0));
    let mut c = b.build();

    c.set_external(ext, 1.0);
    for _ in 0..4 {
        c.tick(1);
    }
    assert_eq!(c.value(g), 1.0, "sampled after becoming due");

    c.set_external(ext, 2.0);
    c.tick(1);
    c.tick(1);
    c.tick(1);
    assert_eq!(c.value(g), 1.0, "held between updates");
    c.tick(1); // 4th step since last update -> due again
    assert_eq!(c.value(g), 2.0, "resampled on the next due step");
}

// -- Evaluation model: event-only domain ------------------------------------

/// Counts how many times it is evaluated. Doubles as a `CustomComponent`
/// save/load exercise.
struct StepCounter {
    n: i64,
}

impl CustomComponent for StepCounter {
    fn reset(&mut self) {
        self.n = 0;
    }
    fn step(&mut self, _inputs: &[f64], _dt: f64) -> f64 {
        self.n += 1;
        self.n as f64
    }
    fn save_state(&self, w: &mut StateWriter) {
        w.write_i64_le(self.n);
    }
    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.n = r.read_i64_le()?;
        Ok(())
    }
}

#[test]
fn event_only_domain_evaluates_only_on_input_change() {
    let mut b = builder_1to1(RATE);
    let ext = b.external_source("EXT");
    let counter = b.custom("COUNTER", vec![ext.into()], Box::new(StepCounter { n: 0 }));
    b.set_domain(counter, ClockDomain::EventOnly);
    let mut c = b.build();

    c.tick(1); // first step: generation advanced past 0 -> evaluates once
    assert_eq!(c.value(counter), 1.0);
    c.tick(1);
    c.tick(1);
    assert_eq!(c.value(counter), 1.0, "no input change -> not re-evaluated");

    c.set_external(ext, 9.0); // bumps the input generation
    c.tick(1);
    assert_eq!(c.value(counter), 2.0, "re-evaluated after an input event");
}

// -- Save / load ------------------------------------------------------------

/// A circuit exercising oscillator phase, LFSR state, a custom component, an
/// integrator (value-slot / back-edge state), and the resampler.
fn mixed_circuit() -> DiscreteCircuit {
    let mut b = DiscreteCircuitBuilder::new(RATE, RATE);
    let sq = b.fixed_square("SQ", 3_000.0);
    let noise = b.lfsr_noise(
        "NOISE",
        12_000.0,
        LfsrSpec {
            width: 17,
            taps: (0, 2),
            seed: 0x1_2345,
        },
    );
    let counter = b.custom("CNT", vec![], Box::new(StepCounter { n: 0 }));
    let mix = b.add("MIX", &[sq, noise]);
    let out = b.gain("OUT", mix, 0.4);
    b.output(out, OutputGain::unity());
    // Keep the counter live so it participates in the schedule.
    b.set_domain(counter, ClockDomain::BoardCycle);
    b.build()
}

#[test]
fn save_load_round_trip_preserves_runtime_state() {
    let mut c1 = mixed_circuit();
    for _ in 0..500 {
        c1.tick(1);
    }
    // Drain the resampler's transient buffer (not part of save state) so both
    // circuits start the post-load comparison with empty output buffers.
    let mut discard = vec![0i16; 8192];
    while c1.fill_audio(&mut discard) > 0 {}

    let mut w = StateWriter::new();
    c1.save_state(&mut w);
    let data = w.into_vec();

    let mut c2 = mixed_circuit();
    let mut r = StateReader::new(&data);
    c2.load_state(&mut r).unwrap();

    // Every node slot restored identically.
    for i in 0..5u16 {
        assert_eq!(c1.value(NodeId(i)), c2.value(NodeId(i)), "node {i}");
    }

    // And the two run in lock-step afterward (oscillator, LFSR, resampler).
    for _ in 0..200 {
        c1.tick(1);
        c2.tick(1);
    }
    let mut a = vec![0i16; 4096];
    let mut b = vec![0i16; 4096];
    let na = c1.fill_audio(&mut a);
    let nb = c2.fill_audio(&mut b);
    assert_eq!(na, nb);
    assert_eq!(a[..na], b[..nb]);
}

// -- Output drain -----------------------------------------------------------

#[test]
fn audio_drains_after_running() {
    let mut c = mixed_circuit();
    // ~one frame at 60 Hz.
    c.tick(RATE / 60);
    let mut out = vec![0i16; 4096];
    let n = c.fill_audio(&mut out);
    assert!(n > 0, "expected drained samples, got {n}");
    // A second drain yields nothing (buffer emptied).
    assert_eq!(c.fill_audio(&mut out), 0);
}
