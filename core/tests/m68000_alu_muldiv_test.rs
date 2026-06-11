//! M68000 MULU/MULS, DIVU/DIVS, and CHK integration tests.
//!
//! Edge cases per the testing requirements: zero operands, the 16-bit sign
//! boundaries (0x7FFF/0x8000 as signed vs unsigned operands), division
//! overflow (V set, destination unchanged), and the divide-by-zero and CHK
//! exception entries (frame details in m68000_exception_test.rs).

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
        assert!(ticks < 500, "instruction did not complete");
    }
}

// ---------------------------------------------------------------------------
// MULU
// ---------------------------------------------------------------------------

#[test]
fn mulu_basic_product_fills_long() {
    let (mut cpu, mut bus) = setup(&[0xC0C1]); // MULU.w D1,D0
    cpu.d[0] = 0xAABB_0100; // only the low word participates
    cpu.d[1] = 0x0000_0100;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0x0001_0000, "0x100 * 0x100 replaces all of D0");
    assert!(!cpu.flag_is_set(SrFlag::N));
    assert!(!cpu.flag_is_set(SrFlag::Z));
}

#[test]
fn mulu_treats_operands_as_unsigned() {
    let (mut cpu, mut bus) = setup(&[0xC0C1]); // MULU.w D1,D0
    cpu.d[0] = 0xFFFF;
    cpu.d[1] = 0xFFFF;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xFFFE_0001, "0xFFFF^2 unsigned");
    assert!(cpu.flag_is_set(SrFlag::N), "bit 31 of the product");
}

#[test]
fn mulu_zero_sets_z_and_keeps_x() {
    let (mut cpu, mut bus) = setup(&[0xC0C1]); // MULU.w D1,D0
    cpu.set_flag(SrFlag::X, true);
    cpu.d[0] = 0x1234;
    cpu.d[1] = 0;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0);
    assert!(cpu.flag_is_set(SrFlag::Z));
    assert!(cpu.flag_is_set(SrFlag::X), "multiply never touches X");
}

// ---------------------------------------------------------------------------
// MULS
// ---------------------------------------------------------------------------

#[test]
fn muls_treats_operands_as_signed() {
    let (mut cpu, mut bus) = setup(&[0xC1C1]); // MULS.w D1,D0
    cpu.d[0] = 0xFFFF; // -1
    cpu.d[1] = 0xFFFF; // -1
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 1, "-1 * -1 = 1");
    assert!(!cpu.flag_is_set(SrFlag::N));

    let (mut cpu, mut bus) = setup(&[0xC1C1]);
    cpu.d[0] = 0x8000; // -32768
    cpu.d[1] = 0x0002;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xFFFF_0000u32, "-32768 * 2");
    assert!(cpu.flag_is_set(SrFlag::N));
}

#[test]
fn muls_memory_operand() {
    let (mut cpu, mut bus) = setup(&[0xC1D0]); // MULS.w (A0),D0
    cpu.a[0] = 0x3000;
    cpu.d[0] = 0x0003;
    bus.load(0x3000, &[0xFF, 0xFE]); // -2
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xFFFF_FFFA, "3 * -2 = -6");
}

// ---------------------------------------------------------------------------
// DIVU
// ---------------------------------------------------------------------------

#[test]
fn divu_quotient_low_remainder_high() {
    let (mut cpu, mut bus) = setup(&[0x80C1]); // DIVU.w D1,D0
    cpu.d[0] = 100_007;
    cpu.d[1] = 10;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 10_000, "quotient in the low word");
    assert_eq!(cpu.d[0] >> 16, 7, "remainder in the high word");
    assert!(!cpu.flag_is_set(SrFlag::N));
    assert!(!cpu.flag_is_set(SrFlag::Z));
    assert!(!cpu.flag_is_set(SrFlag::V));
    assert_eq!(cpu.pc, 0x1002, "no exception taken");
}

#[test]
fn divu_overflow_sets_v_and_preserves_dn() {
    let (mut cpu, mut bus) = setup(&[0x80C1]); // DIVU.w D1,D0
    cpu.d[0] = 0x0001_0000; // quotient would be 0x10000
    cpu.d[1] = 1;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0x0001_0000, "Dn unchanged on overflow");
    assert!(cpu.flag_is_set(SrFlag::V));
    assert_eq!(cpu.pc, 0x1002, "overflow is not a trap");
}

#[test]
fn divu_by_zero_takes_exception_and_preserves_dn() {
    let (mut cpu, mut bus) = setup(&[0x80C1]); // DIVU.w D1,D0
    cpu.a[7] = 0x2000;
    cpu.sr |= 0x000F; // N Z V C set: zero divide clears them
    bus.load(5 * 4, &0x4000u32.to_be_bytes()); // vector 5 handler
    cpu.d[0] = 1234;
    cpu.d[1] = 0;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000, "vector-5 handler entered");
    assert_eq!(cpu.d[0], 1234, "Dn unchanged");
    assert_eq!(cpu.sr & 0xF, 0, "N/Z/V/C cleared (hardware-verified)");
    assert_eq!(cpu.a[7], 0x1FFA, "frame pushed");
}

#[test]
fn divu_zero_quotient_sets_z() {
    let (mut cpu, mut bus) = setup(&[0x80C1]); // DIVU.w D1,D0
    cpu.d[0] = 5;
    cpu.d[1] = 10;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0);
    assert_eq!(cpu.d[0] >> 16, 5, "remainder");
    assert!(cpu.flag_is_set(SrFlag::Z));
}

// ---------------------------------------------------------------------------
// DIVS
// ---------------------------------------------------------------------------

#[test]
fn divs_signed_quotient_and_remainder() {
    let (mut cpu, mut bus) = setup(&[0x81C1]); // DIVS.w D1,D0
    cpu.d[0] = (-100i32) as u32;
    cpu.d[1] = 3;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, (-33i16) as u16 as u32, "-100 / 3 = -33");
    assert_eq!(
        cpu.d[0] >> 16,
        (-1i16) as u16 as u32,
        "remainder keeps the dividend's sign"
    );
    assert!(cpu.flag_is_set(SrFlag::N));
}

#[test]
fn divs_overflow_sets_v_only() {
    let (mut cpu, mut bus) = setup(&[0x81C1]); // DIVS.w D1,D0
    cpu.d[0] = 0x0004_0000; // 262144 / 1 overflows i16
    cpu.d[1] = 1;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0x0004_0000, "Dn unchanged on overflow");
    assert!(cpu.flag_is_set(SrFlag::V));
}

#[test]
fn divs_most_negative_by_minus_one() {
    let (mut cpu, mut bus) = setup(&[0x81C1]); // DIVS.w D1,D0
    cpu.d[0] = 0x8000_0000;
    cpu.d[1] = 0xFFFF; // -1
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0, "0x80000000 / -1 produces 0");
    assert!(cpu.flag_is_set(SrFlag::Z));
    assert!(!cpu.flag_is_set(SrFlag::V));
}

// ---------------------------------------------------------------------------
// CHK
// ---------------------------------------------------------------------------

#[test]
fn chk_in_bounds_does_not_trap() {
    let (mut cpu, mut bus) = setup(&[0x4181]); // CHK.w D1,D0
    cpu.d[0] = 50;
    cpu.d[1] = 100; // bound
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1002, "0 <= 50 <= 100: no trap");
    assert!(!cpu.flag_is_set(SrFlag::Z));
}

#[test]
fn chk_negative_traps_with_n_set() {
    let (mut cpu, mut bus) = setup(&[0x4181]); // CHK.w D1,D0
    cpu.a[7] = 0x2000;
    bus.load(6 * 4, &0x4000u32.to_be_bytes()); // vector 6 handler
    cpu.d[0] = 0xFFFF; // -1
    cpu.d[1] = 100;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000, "negative value traps");
    assert!(cpu.flag_is_set(SrFlag::N), "N set on the negative path");
}

#[test]
fn chk_above_bound_traps_with_n_clear() {
    let (mut cpu, mut bus) = setup(&[0x4181]); // CHK.w D1,D0
    cpu.a[7] = 0x2000;
    bus.load(6 * 4, &0x4000u32.to_be_bytes()); // vector 6 handler
    cpu.set_flag(SrFlag::N, true);
    cpu.d[0] = 101;
    cpu.d[1] = 100;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000, "above bound traps");
    assert!(
        !cpu.flag_is_set(SrFlag::N),
        "N cleared on the too-large path"
    );
}

#[test]
fn chk_zero_sets_z() {
    let (mut cpu, mut bus) = setup(&[0x4181]); // CHK.w D1,D0
    cpu.d[0] = 0;
    cpu.d[1] = 100;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1002, "in bounds: no trap");
    assert!(cpu.flag_is_set(SrFlag::Z), "Z from the checked word");
}
