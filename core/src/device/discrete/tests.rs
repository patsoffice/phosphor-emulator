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
fn fixed_triangle_shape() {
    // freq = rate/4 -> phase advances 0.25/step: peak at phase 0.5, troughs at 0/1.
    let mut b = builder_1to1(RATE);
    let tri = b.triangle("TRI", RATE as f64 / 4.0);
    let mut c = b.build();

    let mut seq = Vec::new();
    for _ in 0..4 {
        c.tick(1);
        seq.push(c.value(tri));
    }
    // phases 0.25, 0.50, 0.75, 1.00 -> 0.0, +1.0, 0.0, -1.0
    assert!((seq[0] - 0.0).abs() < 1e-9, "{seq:?}");
    assert!((seq[1] - 1.0).abs() < 1e-9, "{seq:?}");
    assert!((seq[2] - 0.0).abs() < 1e-9, "{seq:?}");
    assert!((seq[3] + 1.0).abs() < 1e-9, "{seq:?}");
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

// ===========================================================================
// Phase 2: mixers and filters
// ===========================================================================

fn step_n(c: &mut DiscreteCircuit, n: usize) {
    for _ in 0..n {
        c.tick(1);
    }
}

// -- RC filters -------------------------------------------------------------

#[test]
fn rc_low_pass_approaches_target() {
    // tau = 1 ms; alpha per step at 48 kHz.
    let mut b = builder_1to1(RATE);
    let one = b.constant("ONE", 1.0);
    let lp = b.rc_low_pass("LP", one, 1_000.0, 1e-6); // R*C = 1 ms
    let mut c = b.build();

    step_n(&mut c, 48); // ~1 tau
    let at_tau = c.value(lp);
    assert!(
        (0.55..0.72).contains(&at_tau),
        "expected ~63% after one tau, got {at_tau}"
    );
    step_n(&mut c, 48_000); // ~1 s -> fully settled
    assert!((c.value(lp) - 1.0).abs() < 0.01, "should settle to target");
}

#[test]
fn rc_high_pass_blocks_dc_passes_transient() {
    let mut b = builder_1to1(RATE);
    let one = b.constant("ONE", 1.0);
    let hp = b.rc_high_pass("HP", one, 1_000.0, 1e-6);
    let mut c = b.build();

    c.tick(1); // the input step is a transient -> passes through
    assert!(c.value(hp) > 0.9, "step transient passes");
    step_n(&mut c, 48_000); // steady DC -> blocked
    assert!(c.value(hp).abs() < 0.01, "DC blocked");
}

#[test]
fn rc_envelope_charges_fast_discharges_slow() {
    let mut b = builder_1to1(RATE);
    let gate = b.logic_input("GATE");
    let env = b.rc_envelope("ENV", gate, 0.0005, 0.01); // 0.5 ms up, 10 ms down
    let mut c = b.build();

    c.set_logic(gate, true);
    step_n(&mut c, 240); // ~10 charge taus -> fully charged
    assert!(c.value(env) > 0.99, "charged: {}", c.value(env));

    c.set_logic(gate, false);
    step_n(&mut c, 240); // ~0.5 discharge tau -> still well above zero
    let v = c.value(env);
    assert!(
        (0.5..0.72).contains(&v),
        "slow discharge should leave ~e^-0.5, got {v}"
    );
}

// -- Second-order filter ----------------------------------------------------

#[test]
fn second_order_dc_response() {
    let mut b = builder_1to1(RATE);
    let one = b.constant("ONE", 1.0);
    let lp = b.second_order("LP", one, FilterMode::LowPass, 1_000.0, 0.707);
    let bp = b.band_pass("BP", one, 1_000.0, 0.707);
    let hp = b.second_order("HP", one, FilterMode::HighPass, 1_000.0, 0.707);
    let mut c = b.build();

    step_n(&mut c, 48_000);
    assert!((c.value(lp) - 1.0).abs() < 0.02, "LP passes DC");
    assert!(c.value(bp).abs() < 0.02, "BP blocks DC");
    assert!(c.value(hp).abs() < 0.02, "HP blocks DC");
}

#[test]
fn band_pass_is_frequency_selective() {
    fn energy_at(freq_hz: f64) -> f64 {
        let mut b = builder_1to1(RATE);
        let sq = b.fixed_square("SQ", freq_hz);
        let bp = b.band_pass("BP", sq, 2_000.0, 4.0);
        let mut c = b.build();
        step_n(&mut c, 2_000); // warm up
        let mut e = 0.0;
        for _ in 0..4_000 {
            c.tick(1);
            let v = c.value(bp);
            e += v * v;
        }
        e
    }
    let centered = energy_at(2_000.0);
    let far = energy_at(200.0);
    assert!(
        centered > far * 2.0,
        "centered energy {centered} should dominate off-band {far}"
    );
}

// -- Mixers -----------------------------------------------------------------

#[test]
fn resistor_mixer_weights_by_conductance() {
    let mut b = builder_1to1(RATE);
    let hi = b.constant("HI", 1.0);
    let lo = b.constant("LO", 0.0);
    // Equal resistors -> simple average.
    let avg = b.resistor_mixer("AVG", &[(hi, 1_000.0), (lo, 1_000.0)], None);
    // 1k vs 3k -> hi weighted 3:1 -> 0.75.
    let weighted = b.resistor_mixer("W", &[(hi, 1_000.0), (lo, 3_000.0)], None);
    // Same taps plus a 1k load to ground pulls the result down.
    let loaded = b.resistor_mixer("L", &[(hi, 1_000.0), (lo, 3_000.0)], Some(1_000.0));
    let mut c = b.build();

    c.tick(1);
    assert!((c.value(avg) - 0.5).abs() < 1e-9);
    assert!((c.value(weighted) - 0.75).abs() < 1e-9);
    assert!((c.value(loaded) - 0.428_571).abs() < 1e-4);
}

#[test]
fn diode_mixer_takes_max_minus_drop() {
    let mut b = builder_1to1(RATE);
    let a = b.constant("A", 0.3);
    let d = b.constant("D", 0.8);
    let neg = b.constant("N", -0.2);
    let pos_mix = b.diode_mixer("POS", &[a, d], 0.1);
    let neg_mix = b.diode_mixer("NEG", &[a, neg], 0.1);
    let mut c = b.build();

    c.tick(1);
    assert!((c.value(pos_mix) - 0.7).abs() < 1e-9, "max 0.8 - drop 0.1");
    assert!((c.value(neg_mix) - 0.2).abs() < 1e-9, "max 0.3 - drop 0.1");
}

// -- DAC ladder -------------------------------------------------------------

#[test]
fn dac_ladder_is_monotonic_with_correct_endpoints() {
    let mut b = builder_1to1(RATE);
    let code = b.data_input("CODE", 1.0);
    let dac = b.dac_r2r("DAC", code, 8, 5.0);
    let mut c = b.build();

    let sample = |c: &mut DiscreteCircuit, v: f64| {
        c.set_data(code, v);
        c.tick(1);
        c.value(dac)
    };
    assert!((sample(&mut c, 0.0)).abs() < 1e-9, "code 0 -> 0 V");
    assert!(
        (sample(&mut c, 255.0) - 5.0).abs() < 1e-9,
        "full scale -> vref"
    );

    let mut prev = f64::NEG_INFINITY;
    for code_val in [0.0, 1.0, 64.0, 100.0, 200.0, 255.0] {
        let out = sample(&mut c, code_val);
        assert!(out > prev, "monotonic at code {code_val}: {out} > {prev}");
        prev = out;
    }
}

// -- External source feeding an analog stage --------------------------------

#[test]
fn external_source_feeds_filter() {
    let mut b = builder_1to1(RATE);
    let ext = b.external_source("CHIP");
    let lp = b.rc_low_pass("LP", ext, 1_000.0, 1e-7); // fast: tau = 0.1 ms
    let mut c = b.build();

    c.set_external(ext, 0.8);
    step_n(&mut c, 4_800); // ~48 taus
    assert!(
        (c.value(lp) - 0.8).abs() < 0.01,
        "tracks the external stream"
    );
    c.set_external(ext, -0.4);
    step_n(&mut c, 4_800);
    assert!(
        (c.value(lp) + 0.4).abs() < 0.01,
        "follows a new sample value"
    );
}

// -- Save / load of analog state --------------------------------------------

fn filter_circuit() -> DiscreteCircuit {
    let mut b = DiscreteCircuitBuilder::new(RATE, RATE);
    let sq = b.fixed_square("SQ", 1_500.0);
    let lp = b.rc_low_pass("LP", sq, 2_200.0, 1e-7);
    let hp = b.rc_high_pass("HP", lp, 2_200.0, 1e-7);
    let bp = b.band_pass("BP", sq, 1_500.0, 3.0);
    let env = b.rc_envelope("ENV", lp, 0.001, 0.01);
    let mix = b.resistor_mixer("MIX", &[(hp, 1_000.0), (bp, 1_000.0), (env, 2_000.0)], None);
    b.output(mix, OutputGain::unity());
    b.build()
}

#[test]
fn save_load_preserves_filter_state() {
    let mut c1 = filter_circuit();
    step_n(&mut c1, 500);
    let mut discard = vec![0i16; 8192];
    while c1.fill_audio(&mut discard) > 0 {}

    let mut w = StateWriter::new();
    c1.save_state(&mut w);
    let data = w.into_vec();

    let mut c2 = filter_circuit();
    let mut r = StateReader::new(&data);
    c2.load_state(&mut r).unwrap();

    for i in 0..6u16 {
        assert_eq!(c1.value(NodeId(i)), c2.value(NodeId(i)), "node {i}");
    }

    step_n(&mut c1, 200);
    step_n(&mut c2, 200);
    let mut a = vec![0i16; 4096];
    let mut bb = vec![0i16; 4096];
    let na = c1.fill_audio(&mut a);
    let nb = c2.fill_audio(&mut bb);
    assert_eq!(na, nb);
    assert_eq!(a[..na], bb[..nb]);
}
