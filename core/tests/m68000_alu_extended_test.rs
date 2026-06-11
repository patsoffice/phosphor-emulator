//! M68000 ADDX/SUBX, CMPM, and ABCD/SBCD/NBCD integration tests.
//!
//! The extended instructions exist for multi-precision arithmetic, so the
//! tests focus on the X-flag chain (X consumed as carry/borrow-in, set to C
//! on the way out) and the Z rule (cleared by a non-zero result, never set,
//! so a chained result reports zero only if every limb was zero).

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
        assert!(ticks < 100, "instruction did not complete");
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
// ADDX
// ---------------------------------------------------------------------------

#[test]
fn addx_b_consumes_and_produces_x() {
    let (mut cpu, mut bus) = setup(&[0xD101]); // ADDX.b D1,D0
    cpu.set_flag(SrFlag::X, true);
    cpu.set_flag(SrFlag::Z, true);
    cpu.d[0] = 0x10;
    cpu.d[1] = 0x22;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x33, "0x10 + 0x22 + X");
    assert_flags(&cpu, false, false, false, false, false, "carry-in consumed");

    let (mut cpu, mut bus) = setup(&[0xD101]); // 0xFF + 0x00 + X carries out
    cpu.set_flag(SrFlag::X, true);
    cpu.set_flag(SrFlag::Z, true);
    cpu.d[0] = 0xFF;
    cpu.d[1] = 0x00;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x00);
    assert_flags(
        &cpu,
        true,
        false,
        true,
        false,
        true,
        "0xFF+0+X wraps, Z stays",
    );
}

#[test]
fn addx_zero_result_never_sets_z() {
    // Z starts clear: a zero result must leave it clear (multi-precision rule)
    let (mut cpu, mut bus) = setup(&[0xD101]); // ADDX.b D1,D0
    cpu.set_flag(SrFlag::Z, false);
    cpu.d[0] = 0;
    cpu.d[1] = 0;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0);
    assert!(!cpu.flag_is_set(SrFlag::Z), "zero result must not set Z");
}

#[test]
fn addx_w_overflow_and_upper_bits() {
    let (mut cpu, mut bus) = setup(&[0xD141]); // ADDX.w D1,D0
    cpu.set_flag(SrFlag::Z, true);
    cpu.d[0] = 0xAABB_7FFF;
    cpu.d[1] = 0x0000_0001;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xAABB_8000, "word result merges into D0");
    assert_flags(&cpu, false, true, false, true, false, "0x7FFF+1 overflows");
}

#[test]
fn addx_l_multi_precision_chain() {
    // 64-bit add: 0x00000001_FFFFFFFF + 0x00000000_00000001
    // Low limbs in D0/D2, high limbs in D1/D3.
    let (mut cpu, mut bus) = setup(&[0xD182, 0xD383]); // ADDX.l D2,D0; ADDX.l D3,D1
    cpu.set_flag(SrFlag::X, false);
    cpu.set_flag(SrFlag::Z, true); // chain starts with Z set
    cpu.d[0] = 0xFFFF_FFFF;
    cpu.d[1] = 0x0000_0001;
    cpu.d[2] = 0x0000_0001;
    cpu.d[3] = 0x0000_0000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0, "low limb wraps");
    assert!(cpu.flag_is_set(SrFlag::X), "carry into high limb");
    assert!(cpu.flag_is_set(SrFlag::Z), "zero low limb keeps Z");
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1], 0x0000_0002, "high limb absorbs the carry");
    assert!(!cpu.flag_is_set(SrFlag::Z), "non-zero high limb clears Z");
}

#[test]
fn addx_b_memory_predecrement_form() {
    let (mut cpu, mut bus) = setup(&[0xD109]); // ADDX.b -(A1),-(A0)
    cpu.set_flag(SrFlag::Z, true);
    cpu.a[0] = 0x3002;
    cpu.a[1] = 0x4002;
    bus.load(0x3001, &[0x33]); // destination
    bus.load(0x4001, &[0x44]); // source
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[0], 0x3001);
    assert_eq!(cpu.a[1], 0x4001);
    assert_eq!(bus.memory[0x3001], 0x77, "0x33 + 0x44 written to -(A0)");
    assert!(!cpu.flag_is_set(SrFlag::X));
}

// ---------------------------------------------------------------------------
// SUBX
// ---------------------------------------------------------------------------

#[test]
fn subx_b_consumes_borrow() {
    let (mut cpu, mut bus) = setup(&[0x9101]); // SUBX.b D1,D0
    cpu.set_flag(SrFlag::X, true);
    cpu.set_flag(SrFlag::Z, true);
    cpu.d[0] = 0x10;
    cpu.d[1] = 0x0F;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x00, "0x10 - 0x0F - X");
    assert_flags(&cpu, false, false, true, false, false, "zero keeps Z set");
}

#[test]
fn subx_b_borrow_propagates() {
    let (mut cpu, mut bus) = setup(&[0x9101]); // SUBX.b D1,D0
    cpu.set_flag(SrFlag::X, true);
    cpu.d[0] = 0x00;
    cpu.d[1] = 0x00;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0xFF, "0 - 0 - X borrows");
    assert!(cpu.flag_is_set(SrFlag::X), "borrow out");
    assert!(cpu.flag_is_set(SrFlag::N));
    assert!(cpu.flag_is_set(SrFlag::C));
}

#[test]
fn subx_l_memory_predecrement_same_register() {
    // SUBX.l -(A0),-(A0): both operands through A0, which decrements twice
    let (mut cpu, mut bus) = setup(&[0x9188]); // SUBX.l -(A0),-(A0)
    cpu.a[0] = 0x3008;
    bus.load(0x3000, &[0x00, 0x00, 0x00, 0x10]); // destination (after 2nd dec)
    bus.load(0x3004, &[0x00, 0x00, 0x00, 0x01]); // source (after 1st dec)
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[0], 0x3000, "A0 decremented twice");
    assert_eq!(
        &bus.memory[0x3000..0x3004],
        &[0x00, 0x00, 0x00, 0x0F],
        "0x10 - 0x01"
    );
}

// ---------------------------------------------------------------------------
// CMPM
// ---------------------------------------------------------------------------

#[test]
fn cmpm_b_postincrements_both_and_leaves_x() {
    let (mut cpu, mut bus) = setup(&[0xB108]); // CMPM.b (A0)+,(A0)+... actually (Ay)+,(Ax)+
    cpu.set_flag(SrFlag::X, true);
    cpu.a[0] = 0x3000;
    bus.load(0x3000, &[0x10, 0x10]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[0], 0x3002, "same register increments twice");
    assert_flags(&cpu, true, false, true, false, false, "equal bytes, X kept");
}

#[test]
fn cmpm_w_flags_from_difference() {
    let (mut cpu, mut bus) = setup(&[0xB549]); // CMPM.w (A1)+,(A2)+
    cpu.a[1] = 0x4000;
    cpu.a[2] = 0x3000;
    bus.load(0x3000, &[0x00, 0x01]); // dst = 1
    bus.load(0x4000, &[0x00, 0x02]); // src = 2
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[1], 0x4002);
    assert_eq!(cpu.a[2], 0x3002);
    assert_flags(&cpu, false, true, false, false, true, "1 - 2 borrows");
    assert_eq!(
        &bus.memory[0x3000..0x3002],
        &[0x00, 0x01],
        "CMPM never writes"
    );
}

#[test]
fn cmpm_l_compares_longs() {
    let (mut cpu, mut bus) = setup(&[0xB388]); // CMPM.l (A0)+,(A1)+
    cpu.a[0] = 0x4000;
    cpu.a[1] = 0x3000;
    bus.load(0x3000, &[0x80, 0x00, 0x00, 0x00]); // dst
    bus.load(0x4000, &[0x00, 0x00, 0x00, 0x01]); // src
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[0], 0x4004);
    assert_eq!(cpu.a[1], 0x3004);
    assert!(cpu.flag_is_set(SrFlag::V), "0x80000000 - 1 overflows");
}

// ---------------------------------------------------------------------------
// ABCD / SBCD / NBCD
// ---------------------------------------------------------------------------

#[test]
fn abcd_decimal_carry_chain() {
    let (mut cpu, mut bus) = setup(&[0xC101]); // ABCD D1,D0
    cpu.set_flag(SrFlag::Z, true);
    cpu.d[0] = 0x45;
    cpu.d[1] = 0x26;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x71, "45 + 26 = 71 in BCD");
    assert!(!cpu.flag_is_set(SrFlag::X));
    assert!(!cpu.flag_is_set(SrFlag::Z), "non-zero result clears Z");

    let (mut cpu, mut bus) = setup(&[0xC101]); // 99 + 01 = 00 carry 1
    cpu.set_flag(SrFlag::Z, true);
    cpu.d[0] = 0x99;
    cpu.d[1] = 0x01;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x00);
    assert!(cpu.flag_is_set(SrFlag::X), "decimal carry out");
    assert!(cpu.flag_is_set(SrFlag::C));
    assert!(cpu.flag_is_set(SrFlag::Z), "zero result keeps Z");
}

#[test]
fn abcd_consumes_x_as_carry_in() {
    let (mut cpu, mut bus) = setup(&[0xC101]); // ABCD D1,D0
    cpu.set_flag(SrFlag::X, true);
    cpu.d[0] = 0x12;
    cpu.d[1] = 0x34;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x47, "12 + 34 + X = 47 in BCD");
}

#[test]
fn abcd_memory_predecrement_form() {
    let (mut cpu, mut bus) = setup(&[0xC109]); // ABCD -(A1),-(A0)
    cpu.a[0] = 0x3001;
    cpu.a[1] = 0x4001;
    bus.load(0x3000, &[0x55]); // destination
    bus.load(0x4000, &[0x45]); // source
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[0], 0x3000);
    assert_eq!(cpu.a[1], 0x4000);
    assert_eq!(bus.memory[0x3000], 0x00, "55 + 45 = 100 in BCD");
    assert!(cpu.flag_is_set(SrFlag::X), "decimal carry out");
}

#[test]
fn sbcd_decimal_borrow() {
    let (mut cpu, mut bus) = setup(&[0x8101]); // SBCD D1,D0
    cpu.set_flag(SrFlag::Z, true);
    cpu.d[0] = 0x42;
    cpu.d[1] = 0x17;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x25, "42 - 17 = 25 in BCD");
    assert!(!cpu.flag_is_set(SrFlag::X));

    let (mut cpu, mut bus) = setup(&[0x8101]); // 00 - 01 = 99 borrow 1
    cpu.d[0] = 0x00;
    cpu.d[1] = 0x01;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x99);
    assert!(cpu.flag_is_set(SrFlag::X), "decimal borrow out");
    assert!(cpu.flag_is_set(SrFlag::C));
}

#[test]
fn sbcd_consumes_x_as_borrow_in() {
    let (mut cpu, mut bus) = setup(&[0x8101]); // SBCD D1,D0
    cpu.set_flag(SrFlag::X, true);
    cpu.d[0] = 0x50;
    cpu.d[1] = 0x25;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x24, "50 - 25 - X = 24 in BCD");
}

#[test]
fn nbcd_negates_in_decimal() {
    let (mut cpu, mut bus) = setup(&[0x4800]); // NBCD D0
    cpu.set_flag(SrFlag::Z, true);
    cpu.d[0] = 0x42;
    step(&mut cpu, &mut bus);
    assert_eq!(
        cpu.d[0] & 0xFF,
        0x58,
        "0 - 42 = 58 in BCD (ten's complement)"
    );
    assert!(
        cpu.flag_is_set(SrFlag::X),
        "borrow out for non-zero operand"
    );
    assert!(cpu.flag_is_set(SrFlag::C));
    assert!(!cpu.flag_is_set(SrFlag::Z));
}

#[test]
fn nbcd_of_zero_with_x_clear() {
    let (mut cpu, mut bus) = setup(&[0x4800]); // NBCD D0
    cpu.set_flag(SrFlag::Z, true);
    cpu.d[0] = 0x00;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x00, "0 - 0 = 0");
    assert!(!cpu.flag_is_set(SrFlag::X), "no borrow");
    assert!(!cpu.flag_is_set(SrFlag::C));
    assert!(cpu.flag_is_set(SrFlag::Z), "zero result keeps Z");
}

#[test]
fn nbcd_with_x_set() {
    let (mut cpu, mut bus) = setup(&[0x4800]); // NBCD D0
    cpu.set_flag(SrFlag::X, true);
    cpu.d[0] = 0x00;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x99, "0 - 0 - X = 99 in BCD");
    assert!(cpu.flag_is_set(SrFlag::X), "borrow out");
}

#[test]
fn nbcd_memory_operand() {
    let (mut cpu, mut bus) = setup(&[0x4810]); // NBCD (A0)
    cpu.a[0] = 0x3000;
    bus.load(0x3000, &[0x01]);
    step(&mut cpu, &mut bus);
    assert_eq!(bus.memory[0x3000], 0x99, "0 - 1 = 99 in BCD");
}
