//! M68000 ASL/ASR, LSL/LSR, ROL/ROR, ROXL/ROXR integration tests.
//!
//! Edge cases per the testing requirements: count 0 (flags still set, X
//! untouched), counts at and beyond the operand width (register counts run
//! modulo 64; ROXx rotates modulo size+1), the ASL sign-change V rule, ASR
//! sign fill, and the X-flag rules (shifts and ROXx set X = C; plain
//! rotates never touch X).

mod common;

use common::TestBus68k;
use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::m68000::{M68000, SrFlag};

const M: BusMaster = BusMaster::Cpu(0);

fn setup(words: &[u16]) -> (M68000, TestBus68k) {
    let mut cpu = M68000::new();
    cpu.pc = 0x1000;
    let mut bus = TestBus68k::new();
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    bus.load(0x1000, &bytes);
    (cpu, bus)
}

fn step(cpu: &mut M68000, bus: &mut TestBus68k) {
    let mut ticks = 0;
    while !cpu.tick_with_bus(bus, M) {
        ticks += 1;
        assert!(ticks < 200, "instruction did not complete");
    }
}

/// Assert the five CCR flags in X, N, Z, V, C order.
fn assert_flags(cpu: &M68000, x: bool, n: bool, z: bool, v: bool, c: bool, ctx: &str) {
    assert_eq!(cpu.flag_is_set(SrFlag::X), x, "{ctx}: X");
    assert_eq!(cpu.flag_is_set(SrFlag::N), n, "{ctx}: N");
    assert_eq!(cpu.flag_is_set(SrFlag::Z), z, "{ctx}: Z");
    assert_eq!(cpu.flag_is_set(SrFlag::V), v, "{ctx}: V");
    assert_eq!(cpu.flag_is_set(SrFlag::C), c, "{ctx}: C");
}

// ---------------------------------------------------------------------------
// LSL / LSR
// ---------------------------------------------------------------------------

#[test]
fn lsl_b_immediate_count() {
    let (mut cpu, mut bus) = setup(&[0xE309]); // LSL.b #1,D1
    cpu.d[1] = 0xAABB_CC41;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1], 0xAABB_CC82, "byte shift merges into D1");
    assert_flags(&cpu, false, true, false, false, false, "0x41 << 1");

    let (mut cpu, mut bus) = setup(&[0xE309]); // bit 7 shifts out
    cpu.d[1] = 0x81;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFF, 0x02);
    assert_flags(&cpu, true, false, false, false, true, "0x81 << 1 carries");
}

#[test]
fn lsl_immediate_count_zero_means_eight() {
    let (mut cpu, mut bus) = setup(&[0xE149]); // LSL.w #8,D1
    cpu.d[1] = 0x0101;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFFFF, 0x0100);
    assert_flags(&cpu, true, false, false, false, true, "bit 8 out last");
}

#[test]
fn lsr_b_zero_fill_and_carry() {
    let (mut cpu, mut bus) = setup(&[0xE209]); // LSR.b #1,D1
    cpu.d[1] = 0x81;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFF, 0x40, "zero fill from the left");
    assert_flags(&cpu, true, false, false, false, true, "bit 0 out");
}

#[test]
fn lsl_register_count_zero_sets_flags_only() {
    let (mut cpu, mut bus) = setup(&[0xE169]); // LSL.w D0,D1
    cpu.set_flag(SrFlag::X, true);
    cpu.set_flag(SrFlag::C, true);
    cpu.d[0] = 0; // count 0
    cpu.d[1] = 0x8000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFFFF, 0x8000, "operand untouched");
    assert_flags(
        &cpu,
        true,
        true,
        false,
        false,
        false,
        "count 0: C=0, X kept",
    );
}

#[test]
fn lsl_register_count_beyond_width_clears() {
    let (mut cpu, mut bus) = setup(&[0xE169]); // LSL.w D0,D1
    cpu.set_flag(SrFlag::X, true);
    cpu.d[0] = 17; // > 16: everything out, C = 0
    cpu.d[1] = 0xFFFF;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFFFF, 0);
    assert_flags(&cpu, false, false, true, false, false, "count 17 on a word");

    let (mut cpu, mut bus) = setup(&[0xE169]); // count == width: bit 0 out last
    cpu.d[0] = 16;
    cpu.d[1] = 0x0001;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFFFF, 0);
    assert_flags(&cpu, true, false, true, false, true, "count == width");
}

// ---------------------------------------------------------------------------
// ASL / ASR
// ---------------------------------------------------------------------------

#[test]
fn asl_sets_v_on_sign_change() {
    let (mut cpu, mut bus) = setup(&[0xE301]); // ASL.b #1,D1
    cpu.d[1] = 0x40; // 0100_0000 -> 1000_0000: sign flips
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFF, 0x80);
    assert_flags(&cpu, false, true, false, true, false, "ASL sign change");

    let (mut cpu, mut bus) = setup(&[0xE301]); // no sign change
    cpu.d[1] = 0x21;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFF, 0x42);
    assert_flags(
        &cpu,
        false,
        false,
        false,
        false,
        false,
        "ASL no sign change",
    );
}

#[test]
fn asl_v_with_count_beyond_width() {
    let (mut cpu, mut bus) = setup(&[0xE161]); // ASL.w D0,D1
    cpu.d[0] = 20;
    cpu.d[1] = 0x0001; // any non-zero source overflowed at some point
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFFFF, 0);
    assert_flags(&cpu, false, false, true, true, false, "ASL count > width");
}

#[test]
fn asr_sign_fills() {
    let (mut cpu, mut bus) = setup(&[0xE201]); // ASR.b #1,D1
    cpu.d[1] = 0x81;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFF, 0xC0, "sign fill");
    assert_flags(&cpu, true, true, false, false, true, "bit 0 out");
}

#[test]
fn asr_count_beyond_width_keeps_sign() {
    // Beyond the operand width the result saturates to the sign, but C and
    // X are cleared (the sign does NOT keep shifting into C) — hardware
    // behavior verified against the SingleStepTests vectors.
    let (mut cpu, mut bus) = setup(&[0xE061]); // ASR.w D0,D1
    cpu.d[0] = 40;
    cpu.d[1] = 0x8000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFFFF, 0xFFFF, "negative saturates to all-ones");
    assert_flags(
        &cpu,
        false,
        true,
        false,
        false,
        false,
        "C/X clear past width",
    );

    let (mut cpu, mut bus) = setup(&[0xE061]);
    cpu.d[0] = 40;
    cpu.d[1] = 0x7FFF;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFFFF, 0, "positive saturates to zero");
    assert_flags(&cpu, false, false, true, false, false, "zero fill");
}

#[test]
fn asr_count_equal_to_width_carries_sign() {
    let (mut cpu, mut bus) = setup(&[0xE061]); // ASR.w D0,D1
    cpu.d[0] = 16;
    cpu.d[1] = 0x8000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFFFF, 0xFFFF);
    assert_flags(
        &cpu,
        true,
        true,
        false,
        false,
        true,
        "sign is the last bit out",
    );
}

// ---------------------------------------------------------------------------
// ROL / ROR
// ---------------------------------------------------------------------------

#[test]
fn rol_wraps_and_never_touches_x() {
    let (mut cpu, mut bus) = setup(&[0xE319]); // ROL.b #1,D1
    cpu.set_flag(SrFlag::X, true);
    cpu.d[1] = 0x81;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFF, 0x03, "MSB wraps to LSB");
    assert_flags(
        &cpu,
        true,
        false,
        false,
        false,
        true,
        "C = wrapped bit, X kept",
    );
}

#[test]
fn ror_wraps_to_msb() {
    let (mut cpu, mut bus) = setup(&[0xE219]); // ROR.b #1,D1
    cpu.d[1] = 0x01;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFF, 0x80, "LSB wraps to MSB");
    assert_flags(&cpu, false, true, false, false, true, "C = wrapped bit");
}

#[test]
fn rol_register_count_multiple_of_width() {
    let (mut cpu, mut bus) = setup(&[0xE179]); // ROL.w D0,D1
    cpu.d[0] = 16; // full rotation: value unchanged, C = LSB
    cpu.d[1] = 0x8001;
    step(&mut cpu, &mut bus);
    assert_eq!(
        cpu.d[1] & 0xFFFF,
        0x8001,
        "full rotation restores the value"
    );
    assert_flags(&cpu, false, true, false, false, true, "C = result LSB");
}

#[test]
fn rotate_count_zero_clears_c_only() {
    let (mut cpu, mut bus) = setup(&[0xE179]); // ROL.w D0,D1
    cpu.set_flag(SrFlag::C, true);
    cpu.set_flag(SrFlag::X, true);
    cpu.d[0] = 0;
    cpu.d[1] = 0x8000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFFFF, 0x8000);
    assert_flags(&cpu, true, true, false, false, false, "count 0 clears C");
}

// ---------------------------------------------------------------------------
// ROXL / ROXR
// ---------------------------------------------------------------------------

#[test]
fn roxl_rotates_through_x() {
    let (mut cpu, mut bus) = setup(&[0xE311]); // ROXL.b #1,D1
    cpu.set_flag(SrFlag::X, true);
    cpu.d[1] = 0x80;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFF, 0x01, "old X enters at the bottom");
    assert_flags(
        &cpu,
        true,
        false,
        false,
        false,
        true,
        "old MSB lands in X/C",
    );
}

#[test]
fn roxr_rotates_through_x() {
    let (mut cpu, mut bus) = setup(&[0xE211]); // ROXR.b #1,D1
    cpu.set_flag(SrFlag::X, true);
    cpu.d[1] = 0x01;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFF, 0x80, "old X enters at the top");
    assert_flags(&cpu, true, true, false, false, true, "old LSB lands in X/C");
}

#[test]
fn roxl_count_zero_copies_x_to_c() {
    let (mut cpu, mut bus) = setup(&[0xE171]); // ROXL.w D0,D1
    cpu.set_flag(SrFlag::X, true);
    cpu.set_flag(SrFlag::C, false);
    cpu.d[0] = 0;
    cpu.d[1] = 0x1234;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFFFF, 0x1234);
    assert_flags(&cpu, true, false, false, false, true, "count 0: C = X");
}

#[test]
fn roxl_full_span_rotation_is_identity() {
    // The rotation is (size+1) bits wide, so a word count of 17 restores
    // both the operand and X.
    let (mut cpu, mut bus) = setup(&[0xE171]); // ROXL.w D0,D1
    cpu.set_flag(SrFlag::X, true);
    cpu.d[0] = 17;
    cpu.d[1] = 0x1234;
    step(&mut cpu, &mut bus);
    assert_eq!(
        cpu.d[1] & 0xFFFF,
        0x1234,
        "17-bit rotation by 17 is identity"
    );
    assert_flags(&cpu, true, false, false, false, true, "X restored, C = X");
}

// ---------------------------------------------------------------------------
// Memory forms
// ---------------------------------------------------------------------------

#[test]
fn asl_memory_shifts_one_word() {
    let (mut cpu, mut bus) = setup(&[0xE1D0]); // ASL.w (A0)
    cpu.a[0] = 0x3000;
    bus.load(0x3000, &[0x40, 0x00]); // sign changes
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x3000..0x3002], &[0x80, 0x00]);
    assert_flags(&cpu, false, true, false, true, false, "memory ASL");
}

#[test]
fn lsr_memory_shifts_one_word() {
    let (mut cpu, mut bus) = setup(&[0xE2D0]); // LSR.w (A0)
    cpu.a[0] = 0x3000;
    bus.load(0x3000, &[0x00, 0x01]);
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x3000..0x3002], &[0x00, 0x00]);
    assert_flags(
        &cpu,
        true,
        false,
        true,
        false,
        true,
        "bit 0 out, zero result",
    );
}

#[test]
fn roxr_memory_uses_x() {
    let (mut cpu, mut bus) = setup(&[0xE4D0]); // ROXR.w (A0)
    cpu.set_flag(SrFlag::X, true);
    cpu.a[0] = 0x3000;
    bus.load(0x3000, &[0x00, 0x00]);
    step(&mut cpu, &mut bus);
    assert_eq!(
        &bus.memory[0x3000..0x3002],
        &[0x80, 0x00],
        "X enters at the top"
    );
    assert_flags(&cpu, false, true, false, false, false, "X consumed");
}
