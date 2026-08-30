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
        LfsrSpec::toward_zero(24, (10, 23), 0x1A_CFFC),
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

/// The check that "the sequence toggles" cannot make: a register can toggle
/// happily around a 42-state cycle and be a tone rather than noise.
///
/// Both directions are exercised because the direction is what decides the
/// polynomial, and the failure is silent either way.
#[test]
fn cycle_length_separates_a_noise_source_from_a_tone() {
    // Primitive: a maximal 17-bit register visits every non-zero state.
    let good = LfsrSpec::toward_zero(17, (11, 0), 0x1_FFFF);
    assert_eq!(good.cycle_length(), (1 << 17) - 1);

    // The same tap numbers run the other way are a different recurrence, and
    // this pair is not primitive in either arrangement without the inversion.
    let bad = LfsrSpec::toward_zero(16, (6, 14), 0xACE1);
    assert_eq!(
        bad.cycle_length(),
        42,
        "this is the Asteroids thrust defect; if it changes, the finding moved"
    );

    // Inverted feedback shifting the other way is the arrangement the board
    // wires, and it reaches every state but the all-ones lock.
    let real = LfsrSpec {
        width: 16,
        taps: (6, 14),
        seed: 0,
        shift: LfsrShift::TowardHigh,
        invert_feedback: true,
        output: LfsrOutput::Feedback,
    };
    assert_eq!(real.cycle_length(), (1 << 15) - 1);
}

/// The cycle length must describe the register that actually runs, or it is a
/// guarantee about nothing. Walk a live circuit's output far enough to see the
/// sequence come round, and check it comes round where the spec says.
#[test]
fn the_computed_cycle_is_the_one_the_node_runs() {
    let spec = LfsrSpec::toward_zero(16, (6, 14), 0xACE1);
    let period = spec.cycle_length() as usize;

    // Clock the register once per simulation step so the emitted sequence is
    // the register's own, with no resampling between.
    let rate = 48_000.0;
    let mut b = DiscreteCircuitBuilder::new(rate as u64, rate as u64);
    let noise = b.lfsr_noise("N", rate, spec);
    b.output(noise, OutputGain::unity());
    let mut c = b.build();

    // Past the transient before sampling: the seed is not on the cycle.
    let seq = lfsr_sequence(&mut c, 64 + 3 * period);
    let tail = &seq[64..];
    assert_eq!(
        tail[..period],
        tail[period..2 * period],
        "the running node does not repeat at the computed cycle length"
    );
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
        LfsrSpec::toward_zero(17, (0, 2), 0x1_2345),
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

// ===========================================================================
// Phase 3: NE555 / op-amp analog primitives
// ===========================================================================

/// Higher sim rate so the per-step 555 charge/discharge faithfully tracks
/// oscillators in the low-kHz range (the documented simplification).
const SIM: u64 = 192_000;

// -- NE555 astable ----------------------------------------------------------

/// Drive an astable square output and recover its frequency from rising edges.
fn astable_freq(r1: f64, r2: f64, c: f64) -> f64 {
    let mut b = builder_1to1(SIM);
    let osc = b.ne555_astable("OSC", None, r1, r2, c, 5.0, 3.8, Output555::Square);
    let mut c_ = b.build();
    step_n(&mut c_, 1_000); // settle into steady oscillation

    let steps = 192_000usize; // 1 s of simulation
    let mut prev = c_.value(osc);
    let mut edges = 0u32;
    for _ in 0..steps {
        c_.tick(1);
        let cur = c_.value(osc);
        if prev < 1.0 && cur >= 1.0 {
            edges += 1;
        }
        prev = cur;
    }
    edges as f64 / (steps as f64 / SIM as f64)
}

#[test]
fn ne555_astable_oscillates_near_freq_of_555() {
    let (r1, r2, c) = (1_000.0, 1_000.0, 0.1e-6);
    // MAME's FREQ_OF_555 estimate for the classic astable.
    let expected = 1.49 / ((r1 + 2.0 * r2) * c);
    let measured = astable_freq(r1, r2, c);
    assert!(
        (measured - expected).abs() < 0.1 * expected,
        "555 astable freq {measured:.1} Hz should be within 10% of {expected:.1} Hz"
    );
}

#[test]
fn ne555_astable_frequency_scales_with_components() {
    // Doubling C (slower RC) roughly halves the frequency.
    let fast = astable_freq(1_000.0, 1_000.0, 0.1e-6);
    let slow = astable_freq(1_000.0, 1_000.0, 0.2e-6);
    let ratio = fast / slow;
    assert!(
        (1.8..2.2).contains(&ratio),
        "doubling C should roughly halve freq, got ratio {ratio:.3}"
    );
}

#[test]
fn ne555_astable_control_voltage_shifts_frequency() {
    // A lower control voltage lowers the threshold, so the cap reaches it
    // sooner each cycle -> a higher oscillation frequency.
    fn cv_freq(cv: f64) -> f64 {
        let mut b = builder_1to1(SIM);
        let cv_in = b.constant("CV", cv);
        let osc = b.ne555_astable(
            "OSC",
            Some(cv_in),
            1_000.0,
            1_000.0,
            0.1e-6,
            5.0,
            3.8,
            Output555::Square,
        );
        let mut c = b.build();
        step_n(&mut c, 1_000);
        let mut prev = c.value(osc);
        let mut edges = 0u32;
        let steps = 192_000usize;
        for _ in 0..steps {
            c.tick(1);
            let cur = c.value(osc);
            if prev < 1.0 && cur >= 1.0 {
                edges += 1;
            }
            prev = cur;
        }
        edges as f64 / (steps as f64 / SIM as f64)
    }
    let low = cv_freq(2.0);
    let high = cv_freq(3.5);
    assert!(
        low > high * 1.1,
        "lower CV {low:.1} Hz should oscillate faster than higher CV {high:.1} Hz"
    );
}

// -- NE555 constant-current VCO ---------------------------------------------

/// Frequency of the CC VCO's capacitor sawtooth at a given control voltage,
/// recovered by counting the sharp resets.
fn cc_freq(vin: f64) -> f64 {
    let mut b = builder_1to1(SIM);
    let vin_in = b.data_input("VIN", 1.0);
    let cc = b.ne555_cc(
        "CC",
        vin_in,
        None,
        100e3,
        0.01e-6,
        0.0,
        5.0,
        5.0,
        0.7,
        Feed555::Capacitor,
        Output555::Capacitor,
    );
    let mut c = b.build();
    c.set_data(vin_in, vin);
    step_n(&mut c, 2_000);

    let steps = 192_000usize;
    let mut prev = c.value(cc);
    let mut resets = 0u32;
    for _ in 0..steps {
        c.tick(1);
        let cur = c.value(cc);
        if prev - cur > 0.5 {
            resets += 1;
        }
        prev = cur;
    }
    resets as f64 / (steps as f64 / SIM as f64)
}

#[test]
fn ne555_cc_frequency_scales_with_control_voltage() {
    // The control voltage sets both the charging current and the cap's voltage
    // ceiling (vin + junction); it must clear the 2/3·Vcc threshold to run.
    // Within the oscillating band a higher vin means a smaller current, a
    // slower ramp, and a lower frequency.
    let fast = cc_freq(3.0);
    let slow = cc_freq(4.0);
    assert!(fast > 100.0, "expected an audible CC VCO, got {fast:.1} Hz");
    assert!(
        fast > slow * 1.2,
        "raising vin should lower the CC VCO freq: {fast:.1} Hz vs {slow:.1} Hz"
    );
}

/// A discharge resistor is what gives the square output a duty worth tapping.
///
/// With an ideal discharge the cap snaps back in one simulation step, so the
/// square is high for all but one step of every period: a pulse, not a square.
/// Asteroids' thump tapped the capacitor instead for exactly that reason, and
/// the voice measured 17 Hz dull because a sawtooth's harmonics fall as 1/n²
/// where a square's fall as 1/n.
#[test]
fn ne555_cc_discharge_resistor_gives_the_square_a_duty_cycle() {
    let duty_of = |r_disch: f64| -> f64 {
        let mut b = builder_1to1(SIM);
        let vin_in = b.constant("VIN", 3.5);
        let cc = b.ne555_cc(
            "CC",
            vin_in,
            None,
            22e3,
            0.22e-6,
            r_disch,
            5.0,
            5.0,
            0.8,
            Feed555::Capacitor,
            Output555::Square,
        );
        let mut c = b.build();
        step_n(&mut c, 5_000); // past the first ramp from a cold cap
        let mut high = 0usize;
        const N: usize = 200_000;
        for _ in 0..N {
            c.tick(1);
            if c.value(cc) > 0.0 {
                high += 1;
            }
        }
        high as f64 / N as f64
    };

    // Ideal: high essentially all the time, one step low per period.
    assert!(
        duty_of(0.0) > 0.98,
        "ideal discharge should leave a near-100% duty, got {}",
        duty_of(0.0)
    );

    // With the board's 18k on the discharge pin, a real square emerges.
    let duty = duty_of(18e3);
    assert!(
        (0.5..0.95).contains(&duty),
        "18k discharge should give a usable duty, got {duty}"
    );
}

/// With the source on the capacitor, the discharge relaxes toward `i·r_disch`
/// and not toward ground, because the current source keeps feeding the resistor
/// while the pin sinks it. Raising the discharge resistance therefore raises
/// that asymptote as well as slowing the decay, and both push the duty the same
/// way.
#[test]
fn ne555_cc_discharge_asymptote_follows_the_current_source() {
    let period_of = |r_disch: f64| -> f64 {
        let mut b = builder_1to1(SIM);
        let vin_in = b.constant("VIN", 3.5);
        let cc = b.ne555_cc(
            "CC",
            vin_in,
            None,
            22e3,
            0.22e-6,
            r_disch,
            5.0,
            5.0,
            0.8,
            Feed555::Capacitor,
            Output555::Square,
        );
        let mut c = b.build();
        step_n(&mut c, 5_000);
        // Count rising edges over a fixed span to get the period.
        let mut edges = 0usize;
        let mut prev = c.value(cc);
        const N: usize = 200_000;
        for _ in 0..N {
            c.tick(1);
            let v = c.value(cc);
            if prev <= 0.0 && v > 0.0 {
                edges += 1;
            }
            prev = v;
        }
        N as f64 / SIM as f64 / edges as f64
    };

    // A larger discharge resistor lengthens the low phase, so the period grows.
    let short = period_of(18e3);
    let long = period_of(47e3);
    assert!(
        long > short,
        "a larger discharge resistor must lengthen the period: {short} then {long}"
    );
}

/// With the source on the *discharge pin* instead, the pin swallows its current
/// and the cap empties through the resistor toward ground. Checked against the
/// closed form rather than against the other variant alone, because the point is
/// that one of these is arithmetic off the parts and the other is not.
///
/// Values are Asteroids' saucer fire at the top of its sweep: R56 3.3 k from
/// +12 V with the follower holding the emitter at the 5 V the control cap starts
/// at, R57 680 Ω, C35 1 µF, a 5 V timer. The charge is 785.7 µs either way; the
/// discharge is 471.4 µs here and 1450 µs with the source on the cap, because
/// that arrangement's asymptote is i·R57 = 1.44 V against a 1.667 V trigger and
/// the last stretch crawls.
#[test]
fn ne555_cc_source_on_the_discharge_pin_empties_the_cap_toward_ground() {
    const FINE: u64 = 1_920_000;
    let period_of = |feed: Feed555| -> f64 {
        let mut b = builder_1to1(FINE);
        let vin_in = b.constant("VIN", 5.0);
        let cc = b.ne555_cc(
            "CC",
            vin_in,
            None,
            3.3e3,
            1e-6,
            680.0,
            5.0,
            12.0,
            0.0,
            feed,
            Output555::Square,
        );
        let mut c = b.build();
        step_n(&mut c, 20_000); // past the first ramp from a cold cap
        let mut edges = 0usize;
        let mut prev = c.value(cc);
        const N: usize = 1_920_000;
        for _ in 0..N {
            c.tick(1);
            let v = c.value(cc);
            if prev <= 0.0 && v > 0.0 {
                edges += 1;
            }
            prev = v;
        }
        N as f64 / FINE as f64 / edges as f64
    };

    let i = (12.0 - 5.0) / 3.3e3;
    let (threshold, trigger): (f64, f64) = (5.0 * 2.0 / 3.0, 5.0 / 3.0);
    let t_charge = 1e-6 * (threshold - trigger) / i;
    let t_ground = 680.0 * 1e-6 * (threshold / trigger).ln();
    let expected = t_charge + t_ground;

    let pin = period_of(Feed555::DischargePin);
    assert!(
        (pin - expected).abs() / expected < 0.02,
        "expected {:.1} µs from the parts, got {:.1}",
        expected * 1e6,
        pin * 1e6
    );

    // And the other arrangement is a different oscillator, not a rounding
    // difference: its asymptote sits just under the trigger.
    let asymptote = i * 680.0;
    assert!(asymptote < trigger);
    let cap = period_of(Feed555::Capacitor);
    let t_asymptote = 680.0 * 1e-6 * ((threshold - asymptote) / (trigger - asymptote)).ln();
    assert!(
        (cap - (t_charge + t_asymptote)).abs() / cap < 0.02,
        "expected {:.1} µs, got {:.1}",
        (t_charge + t_asymptote) * 1e6,
        cap * 1e6
    );
    assert!(cap > pin * 1.5, "{cap} against {pin}");
}

#[test]
fn ne555_cc_capacitor_stays_between_trigger_and_threshold() {
    let mut b = builder_1to1(SIM);
    let vin_in = b.constant("VIN", 3.5);
    let cc = b.ne555_cc(
        "CC",
        vin_in,
        None,
        100e3,
        0.01e-6,
        0.0,
        5.0,
        5.0,
        0.7,
        Feed555::Capacitor,
        Output555::Capacitor,
    );
    let mut c = b.build();
    step_n(&mut c, 2_000);
    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for _ in 0..20_000 {
        c.tick(1);
        let v = c.value(cc);
        lo = lo.min(v);
        hi = hi.max(v);
    }
    // Oscillates within the 1/3..2/3 Vcc band (1.667 V .. 3.333 V).
    assert!(
        lo >= 1.6 && hi <= 3.4,
        "cap swing {lo:.3}..{hi:.3} V out of band"
    );
    assert!(
        hi - lo > 1.0,
        "cap should actually swing, got {:.3} V",
        hi - lo
    );
}

// -- Op-amp band-pass -------------------------------------------------------

/// Output energy of the band-pass when driven by a unit sine at `freq_hz`.
fn bandpass_energy_at(freq_hz: f64) -> f64 {
    let mut b = builder_1to1(SIM);
    let drive = b.external_source("DRIVE");
    // Galaxian HIT band-pass values (fc ~ 168 Hz).
    // Wide rails so the swept-sine response isn't clipped (we measure shape).
    let bp = b.op_amp_band_pass(
        "BP",
        drive,
        &[150e3, 22e3],
        470e3,
        0.01e-6,
        0.01e-6,
        0.0,
        -100.0,
        100.0,
    );
    let mut c = b.build();

    let dt = 1.0 / SIM as f64;
    let w = std::f64::consts::TAU * freq_hz;
    let mut t = 0.0;
    let drive_sine = |c: &mut DiscreteCircuit, t: f64| {
        c.set_external(drive, (w * t).sin());
        c.tick(1);
    };
    for _ in 0..SIM as usize / 4 {
        drive_sine(&mut c, t); // warm up ~0.25 s
        t += dt;
    }
    let mut e = 0.0;
    for _ in 0..SIM as usize / 4 {
        drive_sine(&mut c, t);
        t += dt;
        let v = c.value(bp);
        e += v * v;
    }
    e
}

#[test]
fn op_amp_band_pass_peaks_near_center_frequency() {
    // fc = 1/(2*pi*sqrt(rTotal*rF*c1*c2)); rTotal = 150k||22k.
    let r_total = 1.0 / (1.0 / 150e3 + 1.0 / 22e3);
    let fc = 1.0 / (std::f64::consts::TAU * (r_total * 470e3 * 0.01e-6_f64 * 0.01e-6).sqrt());
    assert!((150.0..190.0).contains(&fc), "fc sanity {fc:.1} Hz");

    let at_center = bandpass_energy_at(fc);
    let below = bandpass_energy_at(fc / 8.0);
    let above = bandpass_energy_at(fc * 8.0);
    assert!(
        at_center > below * 3.0 && at_center > above * 3.0,
        "band-pass center energy {at_center:.3} should dominate {below:.3}/{above:.3}"
    );
}

// -- Gated RC discharge -----------------------------------------------------

#[test]
fn rc_disc5_decays_with_rc_after_input_drops() {
    let (r, c_val) = (1_000.0, 1e-6); // tau = 1 ms = 192 steps at 192 kHz
    let mut b = builder_1to1(SIM);
    let input = b.data_input("IN", 1.0);
    let gate = b.logic_input("GATE");
    let rc = b.rc_disc5("RC", input, gate, r, c_val);
    let mut c = b.build();

    // Gate enabled, input high -> cap jumps to (5 - 0.7 diode drop).
    c.set_logic(gate, true);
    c.set_data(input, 5.0);
    step_n(&mut c, 50);
    let charged = c.value(rc);
    assert!(
        (charged - 4.3).abs() < 0.05,
        "diode-dropped charge {charged:.3} V"
    );

    // Drop the input: the cap decays toward 0 with tau = R*C.
    c.set_data(input, 0.0);
    let tau_steps = (r * c_val * SIM as f64).round() as usize; // ~192
    step_n(&mut c, tau_steps);
    let after_tau = c.value(rc) / charged;
    assert!(
        (0.30..0.42).contains(&after_tau),
        "after one tau the cap should be ~e^-1 of its peak, got {after_tau:.3}"
    );
}

#[test]
fn rc_disc5_mutes_output_when_gate_low() {
    let mut b = builder_1to1(SIM);
    let input = b.data_input("IN", 1.0);
    let gate = b.logic_input("GATE");
    let rc = b.rc_disc5("RC", input, gate, 1_000.0, 1e-6);
    let mut c = b.build();

    c.set_data(input, 5.0); // gate left low
    step_n(&mut c, 100);
    assert_eq!(c.value(rc), 0.0, "output muted while the gate is low");
}

// -- Save / load of the new primitive state ---------------------------------

fn analog_555_circuit() -> DiscreteCircuit {
    let mut b = DiscreteCircuitBuilder::new(SIM, SIM);
    let cv = b.constant("CV", 3.0);
    let ast = b.ne555_astable(
        "AST",
        Some(cv),
        1_000.0,
        1_000.0,
        0.1e-6,
        5.0,
        3.8,
        Output555::Square,
    );
    let vin = b.constant("VIN", 1.0);
    let cc = b.ne555_cc(
        "CC",
        vin,
        None,
        100e3,
        0.01e-6,
        0.0,
        5.0,
        5.0,
        0.7,
        Feed555::Capacitor,
        Output555::Capacitor,
    );
    let gate = b.fixed_square("GATE", 400.0);
    let rc = b.rc_disc5("RC", ast, gate, 1_000.0, 1e-6);
    let bp = b.op_amp_band_pass(
        "BP",
        cc,
        &[150e3, 22e3],
        470e3,
        0.01e-6,
        0.01e-6,
        0.0,
        -100.0,
        100.0,
    );
    let mix = b.add("MIX", &[rc, bp]);
    b.output(mix, OutputGain::linear(0.1));
    b.build()
}

#[test]
fn save_load_preserves_analog_555_state() {
    let mut c1 = analog_555_circuit();
    step_n(&mut c1, 3_000);
    let mut discard = vec![0i16; 8192];
    while c1.fill_audio(&mut discard) > 0 {}

    let mut w = StateWriter::new();
    c1.save_state(&mut w);
    let data = w.into_vec();

    let mut c2 = analog_555_circuit();
    let mut r = StateReader::new(&data);
    c2.load_state(&mut r).unwrap();

    for i in 0..8u16 {
        assert_eq!(c1.value(NodeId(i)), c2.value(NodeId(i)), "node {i}");
    }

    step_n(&mut c1, 500);
    step_n(&mut c2, 500);
    let mut a = vec![0i16; 4096];
    let mut bb = vec![0i16; 4096];
    let na = c1.fill_audio(&mut a);
    let nb = c2.fill_audio(&mut bb);
    assert_eq!(na, nb);
    assert_eq!(a[..na], bb[..nb]);
}

// -- 74LS629 VCO ------------------------------------------------------------

/// One bench-measured oscillator: the parts around it, and the nine control
/// voltages and frequencies measured through it.
struct Ls629Bench {
    what: &'static str,
    part: Ls629,
    points: [(f64, f64); 9],
}

/// The four 74LS629 halves on a Donkey Kong Jr. sound board that were measured
/// on a breadboard in 2010, with the parts each one is wired to. The endpoints
/// of each sweep are the measured output levels of the 74LS04 driving it, which
/// is why they are 0.13-0.15 V and 4.14-4.16 V rather than 0 V and 5 V.
///
/// These measurements are not ours; see `derive::ls629_frequency`. They are the
/// only check on that surface, because the '629's own datasheet publishes no
/// frequency law for it.
fn ls629_bench() -> [Ls629Bench; 4] {
    [
        Ls629Bench {
            what: "5K pin 7, walking's tone (C18 22n, R10 10k, C17 3.3u, R33 1k)",
            part: Ls629 {
                c: 22e-9,
                r_freq: 10_000.0,
                c_freq_in: 0.0,
                v_rng: 5.0,
                r_rng: 1_000.0,
            },
            points: [
                (0.151, 3_139.0),
                (0.25, 2_883.0),
                (0.5, 2_820.0),
                (0.75, 3_336.0),
                (1.0, 3_805.0),
                (2.0, 6_498.0),
                (3.0, 9_796.0),
                (4.0, 13_440.0),
                (4.14, 13_980.0),
            ],
        },
        Ls629Bench {
            what: "5K pin 10, walking's counter clock (C19 4.7n, R11 20k, C16 3.3u, R33 1k)",
            part: Ls629 {
                c: 4.7e-9,
                r_freq: 20_000.0,
                c_freq_in: 0.0,
                v_rng: 5.0,
                r_rng: 1_000.0,
            },
            points: [
                (0.135, 14_450.0),
                (0.25, 13_320.0),
                (0.5, 12_980.0),
                (0.75, 15_150.0),
                (1.0, 17_270.0),
                (2.0, 28_230.0),
                (3.0, 41_910.0),
                (4.0, 56_950.0),
                (4.15, 59_400.0),
            ],
        },
        Ls629Bench {
            what: "8L pin 10, the jump voice (C22 220n, R13 47k || R12 10k, R35 1k)",
            part: Ls629 {
                c: 0.22e-6,
                // The two inverters feeding this pin arrive through their own
                // resistors, so the pin sees them in parallel.
                r_freq: 47_000.0 * 10_000.0 / 57_000.0,
                c_freq_in: 0.0,
                v_rng: 5.0,
                r_rng: 1_000.0,
            },
            points: [
                (0.151, 313.0),
                (0.25, 288.0),
                (0.5, 275.0),
                (0.75, 324.0),
                (1.0, 370.0),
                (2.0, 635.0),
                (3.0, 965.0),
                (4.0, 1_325.0),
                (4.14, 1_378.0),
            ],
        },
        Ls629Bench {
            what: "7P pin 7, the falling voice (C37 120n, R14 30k, C26 47u, R34 1k)",
            part: Ls629 {
                c: 0.12e-6,
                r_freq: 30_000.0,
                c_freq_in: 0.0,
                v_rng: 5.0,
                r_rng: 1_000.0,
            },
            points: [
                (0.134, 570.0),
                (0.25, 538.0),
                (0.5, 489.0),
                (0.75, 560.0),
                (1.0, 636.0),
                (2.0, 1_003.0),
                (3.0, 1_484.0),
                (4.0, 2_016.0),
                (4.16, 2_111.0),
            ],
        },
    ]
}

/// Modelled frequency of one oscillator held at a fixed control voltage.
fn ls629_freq_at(part: Ls629, v_control: f64) -> f64 {
    let mut b = builder_1to1(RATE);
    let fc = b.constant("FC", v_control);
    let vco = b.ls629_vco("VCO", fc, part);
    let mut c = b.build();
    // Enough steps for a control-pin capacitor to settle, if one is fitted.
    step_n(&mut c, 400_000);
    c.value(vco)
}

/// The residual against all four measured oscillators, in percent, modelled
/// minus measured. Written down because the surface is a global fit over the
/// whole '62x family rather than a fit to any one of these circuits, so the
/// error is real, systematic, and worth knowing the shape of before reading
/// anything into a pitch this model produces.
///
/// ```text
///   control V   0.13-0.15   0.25    0.5    0.75      1      2      3      4  4.1-4.2
///   5K pin 7        +12.5  +19.0  +19.1    +5.1   +0.9   -1.2   +1.3   +0.8    +0.5
///   5K pin 10       +15.5  +21.3  +21.0    +6.5   +0.3   -3.0   -0.2   +0.5    +0.2
///   8L pin 10       +12.7  +19.0  +22.2    +8.6   +4.6   +3.0   +4.8   +4.1    +3.9
///   7P pin 7        +15.2  +18.2  +25.8   +11.5   +3.7   -1.3   +0.4   +1.2    +0.8
/// ```
///
/// Two things follow, and the second is the one that matters for the boards.
///
/// The error is a *band*, not a scale: within 5 % from 1 V up, and up to 26 %
/// high below it. The low end is where the surface is least constrained and
/// where the part's own datasheet warns the output may be unstable, so this is
/// the family's difficult region rather than a fitting mistake.
///
/// But every one of these oscillators is driven by a 74LS04 and so spends
/// almost all its time at one *end* of the sweep, not in the middle: the
/// endpoints above are the measured output levels of that inverter. So the
/// residual in play is +0.2 % to +3.9 % with the control line high, and +12.5 %
/// to +15.5 % with it low. The middle columns are passed through while a
/// control capacitor slews, not sat in. Only the 8L half is out by more than a
/// percent or two at the top of its range, and it is out by about 4 %
/// throughout, which is a fixed sharpness in the jump voice rather than a
/// distortion of its sweep.
#[test]
fn ls629_matches_the_bench_measurements() {
    for bench in ls629_bench() {
        for (v, measured) in bench.points {
            let modelled = ls629_freq_at(bench.part, v);
            let error = (modelled - measured) / measured;
            // Per band, so a regression in either is visible on its own and the
            // low-voltage gap is recorded rather than buried in one loose bound.
            let bound = if v >= 1.0 { 0.05 } else { 0.26 };
            assert!(
                error.abs() <= bound,
                "{}: {v} V modelled {modelled:.0} Hz vs measured {measured:.0} Hz \
                 ({:+.1} %, bound {:.0} %)",
                bench.what,
                error * 100.0,
                bound * 100.0,
            );
        }
    }
}

#[test]
fn ls629_series_resistors_are_dividers_not_decoration() {
    // The check that the pin impedance is modelled at all. R10 and R11 differ
    // only in value, and the two oscillators they feed differ by far more than
    // their capacitors alone explain: dropping the divider would make the ratio
    // of these two frequencies exactly the ratio of their capacitors.
    let bench = ls629_bench();
    let (a, b) = (bench[0].part, bench[1].part);
    let (fa, fb) = (ls629_freq_at(a, 4.0), ls629_freq_at(b, 4.0));
    let cap_ratio = a.c / b.c;
    let measured_ratio = fb / fa;
    assert!(
        (measured_ratio - cap_ratio).abs() > 0.3,
        "the 20 k series resistor moved nothing: ratio {measured_ratio:.3} \
         against the capacitors' {cap_ratio:.3}"
    );
    // And the direction: more series resistance is less control voltage at the
    // pin, so a lower frequency once the capacitors are divided out.
    assert!(measured_ratio < cap_ratio);
}

#[test]
fn ls629_range_input_lowers_the_frequency() {
    // The sense of the range pin, which is the one thing about this part that
    // is easy to invert: it sets the threshold the timing capacitor runs down
    // to, so more range voltage is a longer ramp and a slower oscillator.
    let base = ls629_bench()[0].part;
    let low = ls629_freq_at(Ls629 { v_rng: 1.0, ..base }, 2.0);
    let high = ls629_freq_at(Ls629 { v_rng: 5.0, ..base }, 2.0);
    assert!(
        high < low,
        "raising range from 1 V to 5 V moved {low:.0} Hz to {high:.0} Hz"
    );
}

#[test]
fn ls629_control_capacitor_slews_without_moving_the_settled_pitch() {
    // C17 and its kind are what make a pitch glide rather than step. They must
    // not change where it arrives: a capacitor to ground is not a divider.
    let mut bare = ls629_bench()[0].part;
    bare.c_freq_in = 0.0;
    let slewed = Ls629 {
        c_freq_in: 3.3e-6,
        ..bare
    };

    let settled_bare = ls629_freq_at(bare, 4.14);
    let settled_slewed = ls629_freq_at(slewed, 4.14);
    assert!(
        (settled_bare - settled_slewed).abs() / settled_bare < 1e-6,
        "{settled_bare:.1} Hz settled against {settled_slewed:.1} Hz"
    );

    // And that it takes time to get there. τ = (10k || 90k) · 3.3 µF ≈ 30 ms, so
    // one millisecond in the oscillator is nowhere near its final pitch while
    // the bare one is there on the first step.
    let mut b = builder_1to1(RATE);
    let fc = b.constant("FC", 4.14);
    let bare_id = b.ls629_vco("BARE", fc, bare);
    let slewed_id = b.ls629_vco("SLEWED", fc, slewed);
    let mut c = b.build();
    c.tick(1);
    assert!((c.value(bare_id) - settled_bare).abs() / settled_bare < 1e-9);
    step_n(&mut c, RATE as usize / 1_000);
    let early = c.value(slewed_id);
    assert!(
        early < settled_slewed * 0.5,
        "after 1 ms the slewed oscillator was already at {early:.0} Hz of \
         {settled_slewed:.0} Hz"
    );
}
