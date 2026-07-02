//! Unit tests for the Star Wars Matrix Processor, divider, and PRNG.
//!
//! The matrix processor is driven by a 1024-step microprogram stored across four
//! 1K×4 PROMs (the `user2` region). These tests synthesize small microprograms
//! with [`set_step`] and drive them against a scratch Math RAM buffer, matching
//! MAME `starwars_m.cpp` semantics.

use phosphor_core::device::starwars_math::StarWarsMath;

const MATH_RAM_LEN: usize = 0x1000;

// Strobe bits (IP15_8).
const LAC: u8 = 0x01;
const READ_ACC: u8 = 0x02;
const M_HALT: u8 = 0x04;
const CLEAR_ACC: u8 = 0x10;
const LDC: u8 = 0x20;
const LDB: u8 = 0x40;
const LDA: u8 = 0x80;

/// Encode one microprogram step into a `user2` PROM image (4 × 0x400 bytes).
///
/// The 16-bit microword is split across four nibble planes; the device
/// re-derives `prom_str` (strobe), `prom_am` (address mode), and `prom_mas`
/// (RAM address low bits) from it.
// `0x000 + step` is kept for visual alignment with the other plane offsets.
#[allow(clippy::identity_op)]
fn set_step(prom: &mut [u8], step: usize, strobe: u8, am: u8, mas: u8) {
    prom[0x000 + step] = (strobe >> 4) & 0xf; // microword bits 15:12
    prom[0x400 + step] = strobe & 0xf; //        bits 11:8
    prom[0x800 + step] = ((am & 1) << 3) | ((mas >> 4) & 0x7); // bit7=AM, 6:4=MAS hi
    prom[0xc00 + step] = mas & 0xf; //           bits 3:0 = MAS lo
}

/// Store a 16-bit word into Math RAM at word address `ma` (big-endian: high byte
/// at the lower address), matching how `run_mproc` reads it back.
fn put_word(ram: &mut [u8], ma: usize, word: u16) {
    ram[ma * 2] = (word >> 8) as u8;
    ram[ma * 2 + 1] = (word & 0xff) as u8;
}

/// Read a 16-bit word from Math RAM at word address `ma`.
fn get_word(ram: &[u8], ma: usize) -> u16 {
    ((ram[ma * 2] as u16) << 8) | ram[ma * 2 + 1] as u16
}

#[test]
fn multiply_accumulate_datapath() {
    // Program: A <- RAM[0], B <- RAM[1], C <- RAM[2] (with MAC), store ACC to
    // RAM[3], halt. Each address is direct (am=1).
    let mut prom = vec![0u8; 0x1000];
    set_step(&mut prom, 0, CLEAR_ACC | LDA, 1, 0);
    set_step(&mut prom, 1, LDB, 1, 1);
    set_step(&mut prom, 2, LDC, 1, 2); // ACC += (A-B)*C
    set_step(&mut prom, 3, READ_ACC, 1, 3);
    set_step(&mut prom, 4, M_HALT, 1, 0);

    let mut math = StarWarsMath::new();
    math.load_proms(&prom);

    let mut ram = vec![0u8; MATH_RAM_LEN];
    put_word(&mut ram, 0, 0x0100); // A = 256
    put_word(&mut ram, 1, 0x0000); // B = 0
    put_word(&mut ram, 2, 0x0100); // C = 256

    math.math_w(0, 0, &mut ram); // mw0: start at step 0 and run

    // ACC = ((A - B) << 1) * C << 1 = (256 * 256) * 4 = 0x40000; upper 16 = 0x0004
    assert_eq!(get_word(&ram, 3), 0x0004);
}

#[test]
fn mac_handles_signed_operands() {
    // A = -2, B = 0, C = 3  ->  ACC = (-2 * 3) * 4 = -24 = 0xFFFFFFE8
    // upper 16 bits (ACC >> 16) = 0xFFFF.
    let mut prom = vec![0u8; 0x1000];
    set_step(&mut prom, 0, CLEAR_ACC | LDA, 1, 0);
    set_step(&mut prom, 1, LDB, 1, 1);
    set_step(&mut prom, 2, LDC, 1, 2);
    set_step(&mut prom, 3, READ_ACC, 1, 3);
    set_step(&mut prom, 4, M_HALT, 1, 0);

    let mut math = StarWarsMath::new();
    math.load_proms(&prom);

    let mut ram = vec![0u8; MATH_RAM_LEN];
    put_word(&mut ram, 0, 0xFFFE); // -2
    put_word(&mut ram, 1, 0x0000);
    put_word(&mut ram, 2, 0x0003);

    math.math_w(0, 0, &mut ram);
    assert_eq!(get_word(&ram, 3), 0xFFFF);
}

#[test]
fn bic_relative_addressing() {
    // LAC from a BIC-relative source, store to a direct destination.
    // BIC = 0x10 -> source word address = (mas&3) | (BIC<<2) = 0x40.
    let mut prom = vec![0u8; 0x1000];
    set_step(&mut prom, 0, LAC, 0, 0); // am=0: BIC-relative
    set_step(&mut prom, 1, READ_ACC, 1, 0x10); // am=1: direct MA 0x10
    set_step(&mut prom, 2, M_HALT, 1, 0);

    let mut math = StarWarsMath::new();
    math.load_proms(&prom);

    let mut ram = vec![0u8; MATH_RAM_LEN];
    put_word(&mut ram, 0x40, 0x1234); // source at BIC-relative address

    math.math_w(2, 0x10, &mut ram); // mw2: BIC bits 7:0 = 0x10
    math.math_w(1, 0x00, &mut ram); // mw1: BIC bit 8 = 0
    math.math_w(0, 0, &mut ram); // run

    assert_eq!(get_word(&ram, 0x10), 0x1234);
}

#[test]
fn divider_restoring_division() {
    // dividend = 0x4000, divisor = 0x8000. The 15-step restoring divider yields
    // a deterministic quotient of 0x2000 (matching the schematic algorithm).
    let mut math = StarWarsMath::new();
    let mut ram = vec![0u8; MATH_RAM_LEN];

    math.math_w(6, 0x40, &mut ram); // dvddh
    math.math_w(7, 0x00, &mut ram); // dvddl
    math.math_w(4, 0x80, &mut ram); // dvsrh (latches dividend, clears quotient)
    math.math_w(5, 0x00, &mut ram); // dvsrl (triggers division)

    assert_eq!(math.div_reh_r(), 0x20);
    assert_eq!(math.div_rel_r(), 0x00);
}

#[test]
fn prng_is_deterministic_and_nonconstant() {
    let mut a = StarWarsMath::new();
    let mut b = StarWarsMath::new();

    let seq_a: Vec<u8> = (0..16).map(|_| a.prng_r()).collect();
    let seq_b: Vec<u8> = (0..16).map(|_| b.prng_r()).collect();

    // Same seed (0) -> identical stream.
    assert_eq!(seq_a, seq_b);
    // Self-starting LFSR: the stream is not stuck at a single value.
    assert!(seq_a.iter().any(|&v| v != seq_a[0]));
}

#[test]
fn math_run_flag_lifecycle() {
    // A tiny program still leaves MATH_RUN asserted for a few CPU cycles.
    let mut prom = vec![0u8; 0x1000];
    set_step(&mut prom, 0, LDA, 1, 0);
    set_step(&mut prom, 1, M_HALT, 1, 0);

    let mut math = StarWarsMath::new();
    math.load_proms(&prom);
    let mut ram = vec![0u8; MATH_RAM_LEN];

    assert!(!math.math_run());
    math.math_w(0, 0, &mut ram);
    assert!(math.math_run(), "MATH_RUN asserted after a run");

    // Ticking CPU cycles eventually clears the busy flag.
    for _ in 0..10_000 {
        if !math.math_run() {
            break;
        }
        math.tick();
    }
    assert!(!math.math_run(), "MATH_RUN clears after the busy window");
}

#[test]
fn reset_clears_dynamic_state_but_keeps_proms() {
    let mut prom = vec![0u8; 0x1000];
    set_step(&mut prom, 0, CLEAR_ACC | LDA, 1, 0);
    set_step(&mut prom, 1, LDB, 1, 1);
    set_step(&mut prom, 2, LDC, 1, 2);
    set_step(&mut prom, 3, READ_ACC, 1, 3);
    set_step(&mut prom, 4, M_HALT, 1, 0);

    let mut math = StarWarsMath::new();
    math.load_proms(&prom);
    let mut ram = vec![0u8; MATH_RAM_LEN];
    put_word(&mut ram, 0, 0x0100);
    put_word(&mut ram, 2, 0x0100);

    // Advance the PRNG and run the divider so there is state to clear.
    let _ = math.prng_r();
    math.math_w(6, 0x40, &mut ram);
    math.math_w(4, 0x80, &mut ram);
    math.math_w(5, 0x00, &mut ram);

    math.reset();
    assert!(!math.math_run());
    assert_eq!(math.div_reh_r(), 0x00);
    assert_eq!(math.div_rel_r(), 0x00);

    // PROMs survive reset: the program still computes correctly afterwards.
    let mut ram2 = vec![0u8; MATH_RAM_LEN];
    put_word(&mut ram2, 0, 0x0100);
    put_word(&mut ram2, 2, 0x0100);
    math.math_w(0, 0, &mut ram2);
    assert_eq!(get_word(&ram2, 3), 0x0004);
}
