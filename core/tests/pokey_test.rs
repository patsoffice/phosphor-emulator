use phosphor_core::device::Pokey;

#[test]
fn test_register_routing() {
    let mut pokey = Pokey::new(44100);
    pokey.write(0x00, 100); // AUDF1
    pokey.write(0x01, 0xAF); // AUDC1

    // Verify side effects via pot scan (indirect)
    pokey.set_pot_input(0, 50);
    pokey.write(0x0B, 0); // POTGO

    // Tick enough times
    for _ in 0..6000 {
        pokey.tick();
    }

    let pot0 = pokey.read(0x00);
    assert_eq!(pot0, 50);
}

#[test]
fn test_poly_counters() {
    let mut pokey = Pokey::new(44100);

    // 17-bit poly: starting at all-1s, zeros propagate from bit 0 but
    // RANDOM reads bits 16:9 — need enough ticks for changes to reach bit 9.
    let r1 = pokey.read(0x0A);
    for _ in 0..20 {
        pokey.tick();
    }
    let r2 = pokey.read(0x0A);
    assert_ne!(r1, r2);

    // Test 9-bit mode (changes visible after just 2 ticks)
    pokey.write(0x08, 0x80); // AUDCTL bit 7 set
    let r3 = pokey.read(0x0A);
    for _ in 0..2 {
        pokey.tick();
    }
    let r4 = pokey.read(0x0A);
    assert_ne!(r3, r4);
}

#[test]
fn test_divider_frequency() {
    let mut pokey = Pokey::new(44100);
    pokey.write(0x08, 0x40); // Ch1 1.79MHz
    pokey.write(0x00, 4); // Period 5
    pokey.write(0x01, 0xAF); // Pure tone, vol 15
    pokey.write(0x09, 0); // Reset

    pokey.drain_audio();
    // Need at least ~41 ticks (1_789_773 / 44100) to produce one output sample
    for _ in 0..50 {
        pokey.tick();
    }

    let samples = pokey.drain_audio();
    assert!(!samples.is_empty());
}

#[test]
fn test_linked_mode() {
    let mut pokey = Pokey::new(44100);
    pokey.write(0x08, 0x50); // Ch1+2 linked, Ch1 1.79MHz
    pokey.write(0x00, 0xFF);
    pokey.write(0x02, 0x00); // Total 255
    pokey.write(0x03, 0xAF); // Ch2 output
    pokey.write(0x09, 0);

    for _ in 0..300 {
        pokey.tick();
    }

    let samples = pokey.drain_audio();
    assert!(!samples.is_empty());
}

#[test]
fn test_volume_only() {
    let mut pokey = Pokey::new(44100);
    pokey.write(0x01, 0x18); // Vol 8, volume-only mode (force output)

    for _ in 0..50 {
        pokey.tick();
    }

    let samples = pokey.drain_audio();
    assert!(!samples.is_empty());
    let val = samples[0];
    assert!((val - 0.133).abs() < 0.01);
}

#[test]
fn test_irq_lifecycle() {
    let mut pokey = Pokey::new(44100);
    pokey.write(0x0E, 0x01); // Enable IRQ1
    pokey.write(0x08, 0x40); // 1.79MHz
    pokey.write(0x00, 2);
    pokey.write(0x09, 0);

    assert_eq!(pokey.read(0x0E) & 0x01, 0x01); // Not pending
    assert!(!pokey.irq());

    for _ in 0..10 {
        pokey.tick();
    }

    assert_eq!(pokey.read(0x0E) & 0x01, 0x00); // Pending
    assert!(pokey.irq());

    pokey.write(0x0E, 0x00); // Disable

    assert_eq!(pokey.read(0x0E) & 0x01, 0x01); // Cleared
    assert!(!pokey.irq());
}

#[test]
fn test_pot_scan() {
    let mut pokey = Pokey::new(44100);
    pokey.set_pot_input(2, 10);
    pokey.set_pot_input(5, 20);

    pokey.write(0x0B, 0); // POTGO
    assert_eq!(pokey.read(0x08), 0xFF);

    for _ in 0..1200 {
        pokey.tick();
    }

    let allpot = pokey.read(0x08);
    assert_eq!(allpot & 0x04, 0); // Pot 2 done
    assert_eq!(allpot & 0x20, 0x20); // Pot 5 not done
    assert_eq!(pokey.read(0x02), 10);

    for _ in 0..1200 {
        pokey.tick();
    }

    let allpot2 = pokey.read(0x08);
    assert_eq!(allpot2 & 0x20, 0); // Pot 5 done
    assert_eq!(pokey.read(0x05), 20);
}

#[test]
fn test_stimer_reset() {
    let mut pokey = Pokey::new(44100);
    pokey.write(0x08, 0x40); // 1.79MHz
    pokey.write(0x00, 10); // Period 11
    pokey.write(0x0E, 0x01); // Enable IRQ1
    pokey.write(0x09, 0); // Reset

    for _ in 0..5 {
        pokey.tick();
    }
    pokey.write(0x09, 0); // Reset again

    for _ in 0..10 {
        pokey.tick();
    }
    assert!(!pokey.irq()); // Should not have fired

    pokey.tick();
    pokey.tick(); // 12 ticks from reset
    assert!(pokey.irq());
}

#[test]
fn test_high_pass_filter() {
    let mut pokey = Pokey::new(44100);
    // Ch1 filtered by Ch3 (AUDCTL bit 2)
    // Use 1.79MHz for both Ch1 and Ch3 so HPF flip-flop captures happen at
    // the master clock rate rather than the slow 64 kHz base clock.
    pokey.write(0x08, 0x64); // 1.79MHz Ch1 + 1.79MHz Ch3 + HPF Ch1

    // Ch1: Pure tone, vol 15
    pokey.write(0x00, 10);
    pokey.write(0x01, 0xAF);

    // Ch3: Pure tone, vol 0 (modulator only, different period)
    pokey.write(0x04, 20);
    pokey.write(0x05, 0xA0);

    pokey.write(0x09, 0);

    // We expect Ch1 output to be modulated by Ch3's HPF flip-flop.
    // Need enough ticks for multiple Ch3 underflows (period 21 at 1.79 MHz).
    for _ in 0..500 {
        pokey.tick();
    }
    let samples = pokey.drain_audio();
    assert!(!samples.is_empty());

    // Check for variation in output (HPF should modulate the signal)
    let min = samples.iter().fold(1.0_f32, |a, &b| a.min(b));
    let max = samples.iter().fold(0.0_f32, |a, &b| a.max(b));
    assert!(max > min);
}

#[test]
fn test_peripheral_stubs() {
    let mut pokey = Pokey::new(44100);

    // KBCODE
    pokey.set_kbcode(0x42);
    assert_eq!(pokey.read(0x09), 0x42);

    // SERIN
    pokey.set_serin(0x55);
    assert_eq!(pokey.read(0x0D), 0x55);

    // SEROUT
    pokey.write(0x0D, 0xAA);
    assert_eq!(pokey.read_serout(), 0xAA);

    // SKSTAT
    // Initial 0xFF
    assert_eq!(pokey.read(0x0F), 0xFF);
}

#[test]
fn test_audio_drain() {
    let mut pokey = Pokey::new(44100);
    pokey.write(0x01, 0x08); // Vol 8
    for _ in 0..100 {
        pokey.tick();
    }

    let s1 = pokey.drain_audio();
    assert!(!s1.is_empty());

    let s2 = pokey.drain_audio();
    assert!(s2.is_empty());
}

#[test]
fn test_pot_scan_stops_at_228() {
    let mut pokey = Pokey::new(44100);
    pokey.set_pot_input(0, 255); // Target > 228: will never complete
    pokey.set_pot_input(1, 100); // Target < 228: will complete normally
    pokey.write(0x0B, 0); // POTGO

    // Tick well beyond 228 * 114 = 25992 master clocks
    for _ in 0..30000 {
        pokey.tick();
    }

    // Pot 1 completed (100 < 228)
    assert_eq!(pokey.read(0x08) & 0x02, 0, "pot 1 should be done");
    // Pot 0 never completed (255 > 228), ALLPOT bit still set
    assert_ne!(
        pokey.read(0x08) & 0x01,
        0,
        "pot 0 should NOT be done (target > 228)"
    );

    // Further ticking should not advance pot 0's counter (scanning stopped)
    let counter_before = pokey.read(0x00);
    for _ in 0..10000 {
        pokey.tick();
    }
    let counter_after = pokey.read(0x00);
    assert_eq!(
        counter_before, counter_after,
        "scanning should have stopped at 228"
    );
}

#[test]
fn test_stimer_does_not_reset_base_clocks() {
    let mut pokey = Pokey::new(44100);
    // Use base-clock mode (not 1.79 MHz) for Ch1
    pokey.write(0x08, 0x00); // AUDCTL: 64 kHz base, no special modes
    pokey.write(0x00, 0); // AUDF1 = 0 (period 1, fires every base tick)
    pokey.write(0x01, 0xAF); // AUDC1: pure tone, vol 15
    pokey.write(0x0E, 0x01); // Enable IRQ1
    pokey.write(0x09, 0); // STIMER

    // Tick partway through a 64 kHz period (28 master clocks)
    for _ in 0..14 {
        pokey.tick();
    }

    // Write STIMER again -- should NOT reset the base clock counters
    pokey.write(0x09, 0);

    // If base clocks were NOT reset, the next 64 kHz tick arrives after
    // the remaining ~14 clocks. If they WERE reset, it would take 28.
    let mut ticks_to_irq = 0;
    for _ in 0..40 {
        pokey.tick();
        ticks_to_irq += 1;
        if pokey.irq() {
            break;
        }
    }

    assert!(
        ticks_to_irq < 20,
        "base clock should not have been reset by STIMER (took {} ticks)",
        ticks_to_irq
    );
}

#[test]
fn test_skrest_preserves_non_error_bits() {
    let mut pokey = Pokey::new(44100);

    // Initial SKSTAT is 0xFF (all bits set = no errors)
    assert_eq!(pokey.read(0x0F), 0xFF);

    // Writing SKREST when SKSTAT is already 0xFF should keep it 0xFF
    pokey.write(0x0A, 0);
    assert_eq!(pokey.read(0x0F), 0xFF);
}

#[test]
fn test_default_impl() {
    let pokey = Pokey::default();
    assert!(!pokey.irq());
    assert_eq!(pokey.read_serout(), 0);
}

/// Count how many times the (0.0 / positive) output sample stream flips state.
fn count_transitions(samples: &[f32]) -> usize {
    samples
        .windows(2)
        .filter(|w| (w[0] > 0.05) != (w[1] > 0.05))
        .count()
}

/// Regression test for the channel output-flip-flop model. The polynomial that
/// gates/samples a channel must be read at the *channel* underflow rate, not at
/// the master clock. The previous implementation recomputed
/// `div_out && poly_bit` every master clock, so a noise channel toggled at ~1.79
/// MHz instead of at its (slow) timer rate — making all distortion/noise sounds
/// far too harsh. Here the output is latched only on underflow, so the number of
/// transitions stays near the (rare) underflow count rather than exploding.
#[test]
fn test_noise_output_latched_at_channel_rate() {
    // Resample 1:1 with the master clock so each tick yields one observable
    // output sample (no box-filter smoothing).
    let mut pokey = Pokey::with_clock(1_789_773, 1_789_773);
    pokey.write(0x0F, 0x03); // SKCTL: take poly counters out of reset
    pokey.write(0x08, 0x40); // AUDCTL: Ch1 clocked at 1.79 MHz (ticks every master clock)
    pokey.write(0x00, 200); // AUDF1: long period (201 ticks) -> rare underflows
    pokey.write(0x01, 0x0F); // AUDC1: 17-bit poly noise (poly5-gated), vol 15
    pokey.write(0x09, 0); // STIMER reset

    pokey.drain_audio();
    for _ in 0..4020 {
        pokey.tick();
    }
    let samples = pokey.drain_audio();
    let transitions = count_transitions(&samples);

    // ~20 underflows occur over this window, each of which may (poly5-gated)
    // update the latched output. The old continuous-AND model produced hundreds.
    assert!(
        (1..=40).contains(&transitions),
        "noise output should latch at the channel rate, got {transitions} transitions"
    );
}

/// Guard the working path: a pure-tone channel must still toggle exactly once
/// per timer period (this path was already correct and must not regress).
#[test]
fn test_pure_tone_frequency_unchanged() {
    let mut pokey = Pokey::with_clock(1_789_773, 1_789_773);
    pokey.write(0x08, 0x40); // AUDCTL: Ch1 at 1.79 MHz
    pokey.write(0x00, 50); // AUDF1: period 51 master clocks
    pokey.write(0x01, 0xAF); // AUDC1: pure tone, vol 15
    pokey.write(0x09, 0); // STIMER reset

    pokey.drain_audio();
    for _ in 0..5100 {
        pokey.tick();
    }
    let samples = pokey.drain_audio();
    let transitions = count_transitions(&samples);

    // 5100 / 51 = 100 toggles expected (one transition per period).
    assert!(
        (96..=104).contains(&transitions),
        "pure tone should toggle once per period (~100), got {transitions}"
    );
}
