//! M68000 ADD/ADDA/ADDI, SUB/SUBA/SUBI, CMP/CMPA/CMPI integration tests.
//!
//! Edge cases per the testing requirements: zero results, the sign
//! boundaries 0x7F/0x80, 0x7FFF/0x8000, 0x7FFFFFFF/0x80000000, carry and
//! borrow propagation, and the X-flag rules (ADD/SUB set X = C, CMP never
//! touches X).

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
// ADD — register direction (Dn ⟵ Dn + <ea>)
// ---------------------------------------------------------------------------

#[test]
fn add_b_simple_and_zero() {
    let (mut cpu, mut bus) = setup(&[0xD001]); // ADD.b D1,D0
    cpu.d[0] = 0x10;
    cpu.d[1] = 0x22;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x32);
    assert_flags(&cpu, false, false, false, false, false, "0x10+0x22");

    let (mut cpu, mut bus) = setup(&[0xD001]); // 0x80 + 0x80 = 0x00, C=1 V=1
    cpu.d[0] = 0x80;
    cpu.d[1] = 0x80;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x00);
    assert_flags(&cpu, true, false, true, true, true, "0x80+0x80");
}

#[test]
fn add_b_sign_boundary_7f_plus_1_overflows() {
    let (mut cpu, mut bus) = setup(&[0xD001]); // ADD.b D1,D0
    cpu.d[0] = 0x7F;
    cpu.d[1] = 0x01;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x80);
    assert_flags(&cpu, false, true, false, true, false, "0x7F+1");
}

#[test]
fn add_b_preserves_upper_register_bits() {
    let (mut cpu, mut bus) = setup(&[0xD001]);
    cpu.d[0] = 0xAABB_CCFF;
    cpu.d[1] = 0x0000_0001;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xAABB_CC00, "byte result merges into D0");
    assert!(cpu.flag_is_set(SrFlag::C), "0xFF+1 carries");
    assert!(cpu.flag_is_set(SrFlag::X), "ADD sets X = C");
}

#[test]
fn add_w_sign_boundary_and_carry() {
    let (mut cpu, mut bus) = setup(&[0xD041]); // ADD.w D1,D0
    cpu.d[0] = 0x7FFF;
    cpu.d[1] = 0x0001;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x8000);
    assert_flags(&cpu, false, true, false, true, false, "0x7FFF+1");

    let (mut cpu, mut bus) = setup(&[0xD041]);
    cpu.d[0] = 0xFFFF;
    cpu.d[1] = 0x0001;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x0000);
    assert_flags(&cpu, true, false, true, false, true, "0xFFFF+1 wraps");
}

#[test]
fn add_l_sign_boundary() {
    let (mut cpu, mut bus) = setup(&[0xD081]); // ADD.l D1,D0
    cpu.d[0] = 0x7FFF_FFFF;
    cpu.d[1] = 0x0000_0001;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0x8000_0000);
    assert_flags(&cpu, false, true, false, true, false, "0x7FFFFFFF+1");

    let (mut cpu, mut bus) = setup(&[0xD081]);
    cpu.d[0] = 0xFFFF_FFFF;
    cpu.d[1] = 0x0000_0001;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0x0000_0000);
    assert_flags(&cpu, true, false, true, false, true, "0xFFFFFFFF+1 wraps");
}

#[test]
fn add_w_from_memory_source() {
    let (mut cpu, mut bus) = setup(&[0xD050]); // ADD.w (A0),D0
    cpu.a[0] = 0x4000;
    cpu.d[0] = 0x1111;
    bus.load(0x4000, &[0x22, 0x22]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x3333);
}

#[test]
fn add_w_immediate_source() {
    let (mut cpu, mut bus) = setup(&[0xD07C, 0x0100]); // ADD.w #$100,D0
    cpu.d[0] = 0x0023;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x0123);
    assert_eq!(cpu.pc, 0x1004);
}

// ---------------------------------------------------------------------------
// ADD — memory direction (<ea> ⟵ <ea> + Dn)
// ---------------------------------------------------------------------------

#[test]
fn add_w_to_memory_destination() {
    let (mut cpu, mut bus) = setup(&[0xD150]); // ADD.w D0,(A0)
    cpu.a[0] = 0x4000;
    cpu.d[0] = 0x1111;
    bus.load(0x4000, &[0x22, 0x22]);
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x4000..0x4002], &[0x33, 0x33]);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x1111, "Dn source unchanged");
}

#[test]
fn add_l_to_predecrement_destination() {
    let (mut cpu, mut bus) = setup(&[0xD1A0]); // ADD.l D0,-(A0)
    cpu.a[0] = 0x4004;
    cpu.d[0] = 0x0000_0001;
    bus.load(0x4000, &[0xFF, 0xFF, 0xFF, 0xFF]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[0], 0x4000);
    assert_eq!(&bus.memory[0x4000..0x4004], &[0x00, 0x00, 0x00, 0x00]);
    assert!(cpu.flag_is_set(SrFlag::Z));
    assert!(cpu.flag_is_set(SrFlag::C));
    assert!(cpu.flag_is_set(SrFlag::X));
}

// ---------------------------------------------------------------------------
// ADDA
// ---------------------------------------------------------------------------

#[test]
fn adda_w_sign_extends_and_sets_no_flags() {
    let (mut cpu, mut bus) = setup(&[0xD4FC, 0xFFFF]); // ADDA.w #$FFFF,A2 (= -1)
    cpu.a[2] = 0x0000_1000;
    cpu.sr &= 0xFF00; // clear CCR
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[2], 0x0000_0FFF, "word operand sign-extends to -1");
    assert_eq!(cpu.sr & 0x001F, 0, "ADDA sets no flags");
}

#[test]
fn adda_l_full_width() {
    let (mut cpu, mut bus) = setup(&[0xD5C1]); // ADDA.l D1,A2
    cpu.a[2] = 0x0001_0000;
    cpu.d[1] = 0x0002_0000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[2], 0x0003_0000);
}

// ---------------------------------------------------------------------------
// SUB / SUBA
// ---------------------------------------------------------------------------

#[test]
fn sub_b_borrow_and_sign_boundary() {
    let (mut cpu, mut bus) = setup(&[0x9001]); // SUB.b D1,D0
    cpu.d[0] = 0x00;
    cpu.d[1] = 0x01;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0xFF);
    assert_flags(&cpu, true, true, false, false, true, "0-1 borrows");

    let (mut cpu, mut bus) = setup(&[0x9001]); // 0x80 - 1 = 0x7F, V=1
    cpu.d[0] = 0x80;
    cpu.d[1] = 0x01;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x7F);
    assert_flags(&cpu, false, false, false, true, false, "0x80-1 overflows");
}

#[test]
fn sub_w_zero_result() {
    let (mut cpu, mut bus) = setup(&[0x9041]); // SUB.w D1,D0
    cpu.d[0] = 0x1234;
    cpu.d[1] = 0x1234;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0);
    assert_flags(&cpu, false, false, true, false, false, "x-x = 0");
}

#[test]
fn sub_w_sign_boundary_8000() {
    let (mut cpu, mut bus) = setup(&[0x9041]); // SUB.w D1,D0
    cpu.d[0] = 0x8000;
    cpu.d[1] = 0x0001;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x7FFF);
    assert_flags(&cpu, false, false, false, true, false, "0x8000-1 overflows");
}

#[test]
fn sub_l_sign_boundary() {
    let (mut cpu, mut bus) = setup(&[0x9081]); // SUB.l D1,D0
    cpu.d[0] = 0x8000_0000;
    cpu.d[1] = 0x0000_0001;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0x7FFF_FFFF);
    assert_flags(&cpu, false, false, false, true, false, "0x80000000-1");
}

#[test]
fn sub_w_to_memory_destination() {
    let (mut cpu, mut bus) = setup(&[0x9150]); // SUB.w D0,(A0)
    cpu.a[0] = 0x4000;
    cpu.d[0] = 0x0001;
    bus.load(0x4000, &[0x00, 0x00]);
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x4000..0x4002], &[0xFF, 0xFF]);
    assert!(cpu.flag_is_set(SrFlag::C), "borrow sets C");
    assert!(cpu.flag_is_set(SrFlag::X), "SUB sets X = C");
}

#[test]
fn suba_w_sign_extends_and_sets_no_flags() {
    let (mut cpu, mut bus) = setup(&[0x94FC, 0x8000]); // SUBA.w #$8000,A2
    cpu.a[2] = 0x0000_1000;
    cpu.sr &= 0xFF00;
    step(&mut cpu, &mut bus);
    // 0x1000 - (-0x8000) = 0x9000
    assert_eq!(cpu.a[2], 0x0000_9000);
    assert_eq!(cpu.sr & 0x001F, 0, "SUBA sets no flags");
}

// ---------------------------------------------------------------------------
// CMP / CMPA
// ---------------------------------------------------------------------------

#[test]
fn cmp_w_sets_flags_keeps_operands_and_x() {
    let (mut cpu, mut bus) = setup(&[0xB041]); // CMP.w D1,D0 (D0 - D1)
    cpu.d[0] = 0x1000;
    cpu.d[1] = 0x2000;
    cpu.set_flag(SrFlag::X, true);
    step(&mut cpu, &mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x1000, "CMP discards the result");
    assert_eq!(cpu.d[1] & 0xFFFF, 0x2000);
    assert_flags(&cpu, true, true, false, false, true, "0x1000-0x2000");
}

#[test]
fn cmp_x_flag_never_touched_either_way() {
    // X stays clear even when the compare borrows
    let (mut cpu, mut bus) = setup(&[0xB001]); // CMP.b D1,D0
    cpu.d[0] = 0x00;
    cpu.d[1] = 0x01;
    step(&mut cpu, &mut bus);
    assert!(cpu.flag_is_set(SrFlag::C), "borrow sets C");
    assert!(!cpu.flag_is_set(SrFlag::X), "CMP leaves X clear");
}

#[test]
fn cmp_b_equal_sets_z() {
    let (mut cpu, mut bus) = setup(&[0xB001]); // CMP.b D1,D0
    cpu.d[0] = 0x42;
    cpu.d[1] = 0x42;
    step(&mut cpu, &mut bus);
    assert!(cpu.flag_is_set(SrFlag::Z));
    assert!(!cpu.flag_is_set(SrFlag::C));
}

#[test]
fn cmp_l_signed_overflow() {
    let (mut cpu, mut bus) = setup(&[0xB081]); // CMP.l D1,D0
    cpu.d[0] = 0x8000_0000;
    cpu.d[1] = 0x0000_0001;
    step(&mut cpu, &mut bus);
    // 0x80000000 - 1 = 0x7FFFFFFF: V set (negative minus positive gave positive)
    assert!(cpu.flag_is_set(SrFlag::V));
    assert!(!cpu.flag_is_set(SrFlag::N));
}

#[test]
fn cmpa_w_sign_extends_source() {
    let (mut cpu, mut bus) = setup(&[0xB4FC, 0xFFFF]); // CMPA.w #$FFFF,A2
    cpu.a[2] = 0xFFFF_FFFF; // A2 == -1 == sign-extended source
    step(&mut cpu, &mut bus);
    assert!(cpu.flag_is_set(SrFlag::Z), "full 32-bit compare is equal");

    let (mut cpu, mut bus) = setup(&[0xB4FC, 0xFFFF]);
    cpu.a[2] = 0x0000_FFFF; // not equal once the source sign-extends
    step(&mut cpu, &mut bus);
    assert!(!cpu.flag_is_set(SrFlag::Z));
}

#[test]
fn cmpa_l_compares_full_register() {
    let (mut cpu, mut bus) = setup(&[0xB5C1]); // CMPA.l D1,A2
    cpu.a[2] = 0x0000_2000;
    cpu.d[1] = 0x0000_2000;
    step(&mut cpu, &mut bus);
    assert!(cpu.flag_is_set(SrFlag::Z));
}

// ---------------------------------------------------------------------------
// ADDI / SUBI / CMPI
// ---------------------------------------------------------------------------

#[test]
fn addi_all_sizes_to_data_register() {
    let (mut cpu, mut bus) = setup(&[0x0600, 0x0001]); // ADDI.b #1,D0
    cpu.d[0] = 0x7F;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x80);
    assert!(cpu.flag_is_set(SrFlag::V), "0x7F+1 overflows");
    assert_eq!(cpu.pc, 0x1004);

    let (mut cpu, mut bus) = setup(&[0x0640, 0x8000]); // ADDI.w #$8000,D0
    cpu.d[0] = 0x8000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x0000);
    assert!(cpu.flag_is_set(SrFlag::C));
    assert!(cpu.flag_is_set(SrFlag::V), "neg+neg gave pos");
    assert!(cpu.flag_is_set(SrFlag::X));

    let (mut cpu, mut bus) = setup(&[0x0680, 0x1111, 0x2222]); // ADDI.l #$11112222,D0
    cpu.d[0] = 0x1111_1111;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0x2222_3333);
    assert_eq!(cpu.pc, 0x1006, "two immediate words consumed");
}

#[test]
fn addi_w_to_memory() {
    let (mut cpu, mut bus) = setup(&[0x0650, 0x0001]); // ADDI.w #1,(A0)
    cpu.a[0] = 0x4000;
    bus.load(0x4000, &[0x7F, 0xFF]);
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x4000..0x4002], &[0x80, 0x00]);
    assert!(cpu.flag_is_set(SrFlag::V), "0x7FFF+1 overflows");
}

#[test]
fn subi_w_borrow_sets_x() {
    let (mut cpu, mut bus) = setup(&[0x0440, 0x0001]); // SUBI.w #1,D0
    cpu.d[0] = 0x0000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0xFFFF);
    assert_flags(&cpu, true, true, false, false, true, "0-1");
}

#[test]
fn subi_l_to_memory_via_absolute() {
    // SUBI.l #1,$4000.w — immediate words come before the EA word
    let (mut cpu, mut bus) = setup(&[0x04B8, 0x0000, 0x0001, 0x4000]);
    bus.load(0x4000, &[0x00, 0x00, 0x00, 0x00]);
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x4000..0x4004], &[0xFF, 0xFF, 0xFF, 0xFF]);
    assert_eq!(cpu.pc, 0x1008);
}

#[test]
fn cmpi_sets_flags_without_writing() {
    let (mut cpu, mut bus) = setup(&[0x0C40, 0x1234]); // CMPI.w #$1234,D0
    cpu.d[0] = 0x1234;
    cpu.set_flag(SrFlag::X, true);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x1234, "CMPI does not write");
    assert!(cpu.flag_is_set(SrFlag::Z));
    assert!(cpu.flag_is_set(SrFlag::X), "CMPI leaves X");

    let (mut cpu, mut bus) = setup(&[0x0C50, 0x0042]); // CMPI.w #$42,(A0)
    cpu.a[0] = 0x4000;
    bus.load(0x4000, &[0x00, 0x41]);
    step(&mut cpu, &mut bus);
    assert_eq!(
        &bus.memory[0x4000..0x4002],
        &[0x00, 0x41],
        "memory untouched"
    );
    assert!(cpu.flag_is_set(SrFlag::C), "0x41 < 0x42 borrows");
    assert!(cpu.flag_is_set(SrFlag::N));
}

#[test]
fn cmpi_b_boundary_values() {
    let (mut cpu, mut bus) = setup(&[0x0C00, 0x0080]); // CMPI.b #$80,D0
    cpu.d[0] = 0x7F;
    step(&mut cpu, &mut bus);
    // 0x7F - 0x80: borrow, V set (pos - neg gave neg)
    assert!(cpu.flag_is_set(SrFlag::C));
    assert!(cpu.flag_is_set(SrFlag::V));
    assert!(cpu.flag_is_set(SrFlag::N));
}

// ---------------------------------------------------------------------------
// ADDQ / SUBQ
// ---------------------------------------------------------------------------

#[test]
fn addq_b_carry_and_data_field_zero_means_eight() {
    let (mut cpu, mut bus) = setup(&[0x5200]); // ADDQ.b #1,D0
    cpu.d[0] = 0xFF;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x00);
    assert_flags(&cpu, true, false, true, false, true, "0xFF + 1 wraps");

    let (mut cpu, mut bus) = setup(&[0x5041]); // ADDQ.w #8,D1 (data field 0)
    cpu.d[1] = 0x0010;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1], 0x0018, "data field 0 encodes 8");
}

#[test]
fn addq_l_sign_boundary_overflows() {
    let (mut cpu, mut bus) = setup(&[0x5280]); // ADDQ.l #1,D0
    cpu.d[0] = 0x7FFF_FFFF;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0x8000_0000);
    assert_flags(&cpu, false, true, false, true, false, "max positive + 1");
}

#[test]
fn addq_to_address_register_is_full_width_and_flagless() {
    let (mut cpu, mut bus) = setup(&[0x544B]); // ADDQ.w #2,A3
    cpu.sr = (cpu.sr & 0xFF00) | 0x1F;
    cpu.a[3] = 0xFFFF_FFFF;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[3], 0x0000_0001, "word size still adjusts all 32 bits");
    assert_eq!(cpu.sr & 0x1F, 0x1F, "An destination never alters the CCR");
}

#[test]
fn subq_b_borrow_sets_n_c_x() {
    let (mut cpu, mut bus) = setup(&[0x5300]); // SUBQ.b #1,D0
    cpu.d[0] = 0x00;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0xFF);
    assert_flags(&cpu, true, true, false, false, true, "0 - 1 borrows");
}

#[test]
fn subq_w_memory_destination() {
    let (mut cpu, mut bus) = setup(&[0x5350]); // SUBQ.w #1,(A0)
    cpu.a[0] = 0x4000;
    bus.load(0x4000, &[0x00, 0x01]);
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x4000..0x4002], &[0x00, 0x00]);
    assert!(cpu.flag_is_set(SrFlag::Z));
}

#[test]
fn subq_from_address_register() {
    let (mut cpu, mut bus) = setup(&[0x5589]); // SUBQ.l #2,A1
    cpu.a[1] = 0x0000_0001;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[1], 0xFFFF_FFFF, "wraps below zero, no flags");
    assert!(!cpu.flag_is_set(SrFlag::C));
}
