//! M68000 AND/ANDI, OR/ORI, EOR/EORI, and TST integration tests.
//!
//! Edge cases per the testing requirements: zero results, the sign
//! boundaries 0x80/0x8000/0x80000000, both instruction directions, and the
//! logical X-flag rule (N/Z set, V/C cleared, X never touched).

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

/// Assert N/Z/V/C and that X still has the given value (logical ops never
/// touch X).
fn assert_logical_flags(cpu: &M68000, x: bool, n: bool, z: bool, ctx: &str) {
    assert_eq!(cpu.flag_is_set(SrFlag::X), x, "{ctx}: X must be untouched");
    assert_eq!(cpu.flag_is_set(SrFlag::N), n, "{ctx}: N");
    assert_eq!(cpu.flag_is_set(SrFlag::Z), z, "{ctx}: Z");
    assert!(!cpu.flag_is_set(SrFlag::V), "{ctx}: V always cleared");
    assert!(!cpu.flag_is_set(SrFlag::C), "{ctx}: C always cleared");
}

// ---------------------------------------------------------------------------
// AND — register direction (Dn ⟵ Dn & <ea>)
// ---------------------------------------------------------------------------

#[test]
fn and_b_masks_and_preserves_upper_bits() {
    let (mut cpu, mut bus) = setup(&[0xC001]); // AND.b D1,D0
    cpu.d[0] = 0xAABB_CCF5;
    cpu.d[1] = 0x0000_000F;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xAABB_CC05, "byte result merges into D0");
    assert_logical_flags(&cpu, false, false, false, "0xF5 & 0x0F");
}

#[test]
fn and_b_zero_result_and_x_preserved() {
    let (mut cpu, mut bus) = setup(&[0xC001]); // AND.b D1,D0
    cpu.set_flag(SrFlag::X, true);
    cpu.set_flag(SrFlag::V, true);
    cpu.set_flag(SrFlag::C, true);
    cpu.d[0] = 0xF0;
    cpu.d[1] = 0x0F;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x00);
    assert_logical_flags(&cpu, true, false, true, "0xF0 & 0x0F");
}

#[test]
fn and_w_sign_bit_sets_n() {
    let (mut cpu, mut bus) = setup(&[0xC041]); // AND.w D1,D0
    cpu.d[0] = 0xFFFF;
    cpu.d[1] = 0x8000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x8000);
    assert_logical_flags(&cpu, false, true, false, "0xFFFF & 0x8000");
}

#[test]
fn and_l_full_width() {
    let (mut cpu, mut bus) = setup(&[0xC081]); // AND.l D1,D0
    cpu.d[0] = 0xF0F0_F0F0;
    cpu.d[1] = 0x8FFF_0000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0x80F0_0000);
    assert_logical_flags(&cpu, false, true, false, "long AND");
}

#[test]
fn and_w_memory_source() {
    let (mut cpu, mut bus) = setup(&[0xC050]); // AND.w (A0),D0
    cpu.a[0] = 0x3000;
    cpu.d[0] = 0xFF0F;
    bus.load(0x3000, &[0x0F, 0xFF]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x0F0F);
}

// ---------------------------------------------------------------------------
// AND — memory direction (<ea> ⟵ <ea> & Dn)
// ---------------------------------------------------------------------------

#[test]
fn and_w_to_memory() {
    let (mut cpu, mut bus) = setup(&[0xC150]); // AND.w D0,(A0)
    cpu.a[0] = 0x3000;
    cpu.d[0] = 0x00FF;
    bus.load(0x3000, &[0xAB, 0xCD]);
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x3000..0x3002], &[0x00, 0xCD]);
    assert_logical_flags(&cpu, false, false, false, "0xABCD & 0x00FF");
}

// ---------------------------------------------------------------------------
// OR
// ---------------------------------------------------------------------------

#[test]
fn or_b_combines_bits() {
    let (mut cpu, mut bus) = setup(&[0x8001]); // OR.b D1,D0
    cpu.d[0] = 0xAABB_CC0F;
    cpu.d[1] = 0x0000_00F0;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xAABB_CCFF);
    assert_logical_flags(&cpu, false, true, false, "0x0F | 0xF0");
}

#[test]
fn or_w_zero_result_only_when_both_zero() {
    let (mut cpu, mut bus) = setup(&[0x8041]); // OR.w D1,D0
    cpu.d[0] = 0;
    cpu.d[1] = 0;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0);
    assert_logical_flags(&cpu, false, false, true, "0 | 0");
}

#[test]
fn or_l_to_memory() {
    let (mut cpu, mut bus) = setup(&[0x8190]); // OR.l D0,(A0)
    cpu.a[0] = 0x3000;
    cpu.d[0] = 0x8000_0001;
    bus.load(0x3000, &[0x00, 0x00, 0xFF, 0x00]);
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x3000..0x3004], &[0x80, 0x00, 0xFF, 0x01]);
    assert_logical_flags(&cpu, false, true, false, "long OR to memory");
}

// ---------------------------------------------------------------------------
// EOR (destination form only; Dn destination is legal)
// ---------------------------------------------------------------------------

#[test]
fn eor_b_data_register_destination() {
    let (mut cpu, mut bus) = setup(&[0xB101]); // EOR.b D0,D1
    cpu.d[0] = 0xFF;
    cpu.d[1] = 0xAABB_CC0F;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1], 0xAABB_CCF0, "byte result merges into D1");
    assert_logical_flags(&cpu, false, true, false, "0x0F ^ 0xFF");
}

#[test]
fn eor_w_self_clears_register_and_sets_z() {
    let (mut cpu, mut bus) = setup(&[0xB540]); // EOR.w D2,D0
    cpu.d[2] = 0x1234;
    cpu.d[0] = 0x1234;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0);
    assert_logical_flags(&cpu, false, false, true, "x ^ x = 0");
}

#[test]
fn eor_l_to_memory() {
    let (mut cpu, mut bus) = setup(&[0xB190]); // EOR.l D0,(A0)
    cpu.a[0] = 0x3000;
    cpu.d[0] = 0xFFFF_FFFF;
    bus.load(0x3000, &[0x12, 0x34, 0x56, 0x78]);
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x3000..0x3004], &[0xED, 0xCB, 0xA9, 0x87]);
    assert_logical_flags(&cpu, false, true, false, "long EOR to memory");
}

// ---------------------------------------------------------------------------
// Immediate forms — ORI / ANDI / EORI
// ---------------------------------------------------------------------------

#[test]
fn andi_b_immediate_to_register() {
    let (mut cpu, mut bus) = setup(&[0x0200, 0x000F]); // ANDI.b #$0F,D0
    cpu.set_flag(SrFlag::X, true);
    cpu.d[0] = 0xF5;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x05);
    assert_logical_flags(&cpu, true, false, false, "ANDI.b");
    assert_eq!(cpu.pc, 0x1004);
}

#[test]
fn ori_w_immediate_to_memory() {
    let (mut cpu, mut bus) = setup(&[0x0050, 0x8001]); // ORI.w #$8001,(A0)
    cpu.a[0] = 0x3000;
    bus.load(0x3000, &[0x00, 0x10]);
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x3000..0x3002], &[0x80, 0x11]);
    assert_logical_flags(&cpu, false, true, false, "ORI.w to memory");
}

#[test]
fn eori_l_immediate_to_register() {
    let (mut cpu, mut bus) = setup(&[0x0A80, 0xFFFF, 0xFFFF]); // EORI.l #$FFFFFFFF,D0
    cpu.d[0] = 0x8000_0000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0x7FFF_FFFF);
    assert_logical_flags(&cpu, false, false, false, "EORI.l");
    assert_eq!(cpu.pc, 0x1006, "two immediate extension words consumed");
}

#[test]
fn andi_to_ccr_encoding_is_not_executed_as_andi() {
    // ANDI to CCR (0x023C) is its own instruction: it must clear the flag
    // byte here, not decode as a byte ANDI with an immediate destination.
    let (mut cpu, mut bus) = setup(&[0x023C, 0x0000]);
    cpu.sr = 0x271F;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.sr, 0x2700, "ANDI #0,CCR clears the CCR");
    assert_eq!(cpu.pc, 0x1004, "immediate word consumed");
}

// ---------------------------------------------------------------------------
// TST
// ---------------------------------------------------------------------------

#[test]
fn tst_b_sign_and_zero() {
    let (mut cpu, mut bus) = setup(&[0x4A00]); // TST.b D0
    cpu.set_flag(SrFlag::X, true);
    cpu.d[0] = 0x80;
    step(&mut cpu, &mut bus);
    assert_logical_flags(&cpu, true, true, false, "TST.b 0x80");

    let (mut cpu, mut bus) = setup(&[0x4A00]);
    cpu.d[0] = 0xAABB_CC00;
    step(&mut cpu, &mut bus);
    assert_logical_flags(&cpu, false, false, true, "TST.b only low byte");
}

#[test]
fn tst_w_memory_operand() {
    let (mut cpu, mut bus) = setup(&[0x4A50]); // TST.w (A0)
    cpu.a[0] = 0x3000;
    bus.load(0x3000, &[0x80, 0x00]);
    step(&mut cpu, &mut bus);
    assert_logical_flags(&cpu, false, true, false, "TST.w 0x8000");
    assert_eq!(
        &bus.memory[0x3000..0x3002],
        &[0x80, 0x00],
        "TST never writes"
    );
}

#[test]
fn tst_l_full_width() {
    let (mut cpu, mut bus) = setup(&[0x4A80]); // TST.l D0
    cpu.d[0] = 0x8000_0000;
    step(&mut cpu, &mut bus);
    assert_logical_flags(&cpu, false, true, false, "TST.l 0x80000000");

    let (mut cpu, mut bus) = setup(&[0x4A80]);
    cpu.d[0] = 0;
    step(&mut cpu, &mut bus);
    assert_logical_flags(&cpu, false, false, true, "TST.l 0");
}

#[test]
fn tst_clears_v_and_c() {
    let (mut cpu, mut bus) = setup(&[0x4A40]); // TST.w D0
    cpu.set_flag(SrFlag::V, true);
    cpu.set_flag(SrFlag::C, true);
    cpu.d[0] = 1;
    step(&mut cpu, &mut bus);
    assert_logical_flags(&cpu, false, false, false, "TST clears V/C");
}
