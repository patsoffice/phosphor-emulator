//! The host output rate is negotiated, not hardcoded.
//!
//! Every sound device used to build its resampler against a literal `44_100`.
//! On a host that grants 48 kHz that made the whole machine play about 8% sharp
//! and drain its ring 8% too fast. Devices now read
//! [`phosphor_core::audio::host_sample_rate`] when they construct, and the
//! frontend sets it from the spec the audio device actually granted, before the
//! machine is built.
//!
//! This file lives on its own so it gets its own test binary: the rate is
//! process-wide, so a test that changes it would otherwise perturb every other
//! test sharing the process. Everything here sets the same rate, once, before
//! constructing anything.

use phosphor_machines::registry;

/// A rate that is not the default, so nothing can pass by accident.
const NEGOTIATED: u32 = 48_000;

/// Set the rate exactly once, before any machine in this binary is built.
///
/// Devices read the rate at construction, so a machine built before this ran
/// would carry the old one. `LazyLock` gives us a barrier every test can wait
/// on regardless of which one the harness starts first.
static RATE: std::sync::LazyLock<u32> = std::sync::LazyLock::new(|| {
    phosphor_core::audio::set_host_sample_rate(NEGOTIATED);
    NEGOTIATED
});

/// Guard against a vacuous suite — the sweep below iterates `registry::all()`.
#[test]
fn the_registry_is_not_empty() {
    assert!(registry::all().len() > 30);
}

#[test]
fn the_default_rate_is_the_one_devices_used_to_hardcode() {
    assert_eq!(phosphor_core::audio::DEFAULT_HOST_SAMPLE_RATE, 44_100);
}

#[test]
fn a_zero_rate_is_ignored() {
    // Zero means "no audio" in the frontend's `AudioSource` contract, so it must
    // never become the rate devices resample to — that would divide by zero in
    // the Bresenham ratio.
    // Settle the rate first: another test in this binary may still be about to
    // negotiate, and reading across that would be a race rather than a check.
    let before = *RATE;
    phosphor_core::audio::set_host_sample_rate(0);
    assert_eq!(phosphor_core::audio::host_sample_rate(), before);
}

#[test]
fn every_machine_reports_the_negotiated_rate() {
    let rate = *RATE;
    for entry in registry::all() {
        let sys = (entry.create_bare)();
        let reported = sys.audio_sample_rate();
        assert!(
            reported == 0 || reported == rate,
            "{}: reports {reported} Hz, expected the negotiated {rate} Hz \
             (or 0 for a machine with no audio)",
            entry.name
        );
    }
}

#[test]
fn a_second_of_frames_produces_a_second_of_samples() {
    let rate = *RATE;

    // One POKEY machine and one Namco WSG machine: two different chips, two
    // different clock rates, both resampling to the negotiated output.
    for name in ["tempest", "galaga"] {
        let entry = registry::find(name).expect("machine should be registered");
        let mut sys = (entry.create_bare)();
        sys.reset();

        let frames = sys.frame_rate_hz().round() as usize;
        let mut produced = 0usize;
        let mut chunk = vec![0i16; 8192];
        for _ in 0..frames {
            sys.run_frame();
            loop {
                let n = sys.fill_audio(&mut chunk);
                if n == 0 {
                    break;
                }
                produced += n;
            }
        }

        // A frame count rounded to a whole number is not exactly one second, so
        // allow a couple of percent. The point is that it tracks the negotiated
        // rate rather than the old hardcoded 44.1 kHz, which would land 8% low.
        let expected = rate as f64 * frames as f64 / sys.frame_rate_hz();
        let error = (produced as f64 - expected).abs() / expected;
        assert!(
            error < 0.02,
            "{name}: produced {produced} samples in {frames} frames, expected \
             about {expected:.0} at {rate} Hz ({:.1}% off)",
            error * 100.0
        );
    }
}
