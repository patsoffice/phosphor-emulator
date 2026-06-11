//! M68000 NEG/NEGX/NOT/CLR/EXT/Scc integration tests.
//!
//! Edge cases per the testing requirements: zero, the sign boundaries
//! 0x80/0x8000/0x80000000 (NEG of the most negative value overflows), the
//! NEGX borrow chain, and the per-family X-flag rules.

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
// NEG
// ---------------------------------------------------------------------------

#[test]
fn neg_b_basic_and_carry() {
    let (mut cpu, mut bus) = setup(&[0x4400]); // NEG.b D0
    cpu.d[0] = 0x01;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0xFF);
    assert_flags(
        &cpu,
        true,
        true,
        false,
        false,
        true,
        "NEG of non-zero sets C and X",
    );
}

#[test]
fn neg_b_zero_clears_carry() {
    let (mut cpu, mut bus) = setup(&[0x4400]); // NEG.b D0
    cpu.set_flag(SrFlag::X, true);
    cpu.d[0] = 0x00;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x00);
    assert_flags(&cpu, false, false, true, false, false, "NEG of zero");
}

#[test]
fn neg_b_most_negative_overflows() {
    let (mut cpu, mut bus) = setup(&[0x4400]); // NEG.b D0
    cpu.d[0] = 0x80;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x80, "-(-128) wraps to -128");
    assert_flags(&cpu, true, true, false, true, true, "NEG 0x80 overflows");
}

#[test]
fn neg_w_and_l_sign_boundaries() {
    let (mut cpu, mut bus) = setup(&[0x4440]); // NEG.w D0
    cpu.d[0] = 0xAABB_8000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xAABB_8000, "word result merges, -0x8000 wraps");
    assert!(cpu.flag_is_set(SrFlag::V));

    let (mut cpu, mut bus) = setup(&[0x4480]); // NEG.l D0
    cpu.d[0] = 0x8000_0000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0x8000_0000);
    assert!(cpu.flag_is_set(SrFlag::V));
}

#[test]
fn neg_w_memory_operand() {
    let (mut cpu, mut bus) = setup(&[0x4450]); // NEG.w (A0)
    cpu.a[0] = 0x3000;
    bus.load(0x3000, &[0x00, 0x05]);
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x3000..0x3002], &[0xFF, 0xFB], "-(5)");
}

// ---------------------------------------------------------------------------
// NEGX
// ---------------------------------------------------------------------------

#[test]
fn negx_consumes_x_and_follows_z_rule() {
    let (mut cpu, mut bus) = setup(&[0x4000]); // NEGX.b D0
    cpu.set_flag(SrFlag::X, true);
    cpu.set_flag(SrFlag::Z, true);
    cpu.d[0] = 0x00;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0xFF, "0 - 0 - X");
    assert_flags(
        &cpu,
        true,
        true,
        false,
        false,
        true,
        "borrow out, Z cleared",
    );

    // Zero result with Z initially clear must leave Z clear
    let (mut cpu, mut bus) = setup(&[0x4000]);
    cpu.set_flag(SrFlag::Z, false);
    cpu.d[0] = 0x00;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x00);
    assert!(!cpu.flag_is_set(SrFlag::Z), "zero result must not set Z");
}

#[test]
fn negx_multi_precision_negation() {
    // Negate the 32-bit value 0x00000001 stored as two words in D0/D1:
    // low word first sets the borrow, high word consumes it.
    let (mut cpu, mut bus) = setup(&[0x4040, 0x4041]); // NEGX.w D0; NEGX.w D1
    cpu.set_flag(SrFlag::Z, true);
    cpu.d[0] = 0x0001; // low word
    cpu.d[1] = 0x0000; // high word
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0xFFFF, "-(1) low word");
    assert!(cpu.flag_is_set(SrFlag::X), "borrow into high word");
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1] & 0xFFFF, 0xFFFF, "0 - 0 - X high word");
    assert!(!cpu.flag_is_set(SrFlag::Z), "non-zero limb cleared Z");
}

// ---------------------------------------------------------------------------
// NOT
// ---------------------------------------------------------------------------

#[test]
fn not_complements_and_keeps_x() {
    let (mut cpu, mut bus) = setup(&[0x4600]); // NOT.b D0
    cpu.set_flag(SrFlag::X, true);
    cpu.d[0] = 0xAABB_CCF0;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xAABB_CC0F, "byte complement merges");
    assert_flags(&cpu, true, false, false, false, false, "NOT 0xF0");

    let (mut cpu, mut bus) = setup(&[0x4680]); // NOT.l D0
    cpu.d[0] = 0xFFFF_FFFF;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0);
    assert_flags(
        &cpu,
        false,
        false,
        true,
        false,
        false,
        "NOT all-ones is zero",
    );
}

#[test]
fn not_w_memory_operand() {
    let (mut cpu, mut bus) = setup(&[0x4650]); // NOT.w (A0)
    cpu.a[0] = 0x3000;
    bus.load(0x3000, &[0x12, 0x34]);
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x3000..0x3002], &[0xED, 0xCB]);
    assert!(cpu.flag_is_set(SrFlag::N));
}

// ---------------------------------------------------------------------------
// CLR
// ---------------------------------------------------------------------------

#[test]
fn clr_zeroes_and_sets_z() {
    let (mut cpu, mut bus) = setup(&[0x4200]); // CLR.b D0
    cpu.set_flag(SrFlag::X, true);
    cpu.set_flag(SrFlag::N, true);
    cpu.d[0] = 0xAABB_CCDD;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xAABB_CC00, "byte clear preserves upper bits");
    assert_flags(&cpu, true, false, true, false, false, "CLR.b");

    let (mut cpu, mut bus) = setup(&[0x4280]); // CLR.l D0
    cpu.d[0] = 0xDEAD_BEEF;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0);
}

#[test]
fn clr_w_memory_operand() {
    let (mut cpu, mut bus) = setup(&[0x4250]); // CLR.w (A0)
    cpu.a[0] = 0x3000;
    bus.load(0x3000, &[0xAB, 0xCD, 0xEF, 0x01]);
    step(&mut cpu, &mut bus);
    assert_eq!(
        &bus.memory[0x3000..0x3004],
        &[0x00, 0x00, 0xEF, 0x01],
        "only the addressed word is cleared"
    );
}

// ---------------------------------------------------------------------------
// EXT
// ---------------------------------------------------------------------------

#[test]
fn ext_w_sign_extends_byte_to_word() {
    let (mut cpu, mut bus) = setup(&[0x4880]); // EXT.w D0
    cpu.d[0] = 0xAABB_CC85;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xAABB_FF85, "negative byte fills the word");
    assert!(cpu.flag_is_set(SrFlag::N));

    let (mut cpu, mut bus) = setup(&[0x4880]);
    cpu.d[0] = 0xAABB_CC7F;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xAABB_007F, "positive byte clears the word top");
    assert!(!cpu.flag_is_set(SrFlag::N));
}

#[test]
fn ext_l_sign_extends_word_to_long() {
    let (mut cpu, mut bus) = setup(&[0x48C0]); // EXT.l D0
    cpu.d[0] = 0xAABB_8000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xFFFF_8000);
    assert!(cpu.flag_is_set(SrFlag::N));

    let (mut cpu, mut bus) = setup(&[0x48C0]);
    cpu.set_flag(SrFlag::X, true);
    cpu.d[0] = 0x0000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0);
    assert!(cpu.flag_is_set(SrFlag::Z));
    assert!(cpu.flag_is_set(SrFlag::X), "EXT never touches X");
}

// ---------------------------------------------------------------------------
// Scc
// ---------------------------------------------------------------------------

#[test]
fn scc_writes_ff_or_00_by_condition() {
    let (mut cpu, mut bus) = setup(&[0x54C0]); // SCC D0 (carry clear)
    cpu.set_flag(SrFlag::C, false);
    cpu.d[0] = 0xAABB_CC12;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xAABB_CCFF, "condition true writes 0xFF");

    let (mut cpu, mut bus) = setup(&[0x54C0]); // SCC D0 with C set
    cpu.set_flag(SrFlag::C, true);
    cpu.d[0] = 0xAABB_CC12;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xAABB_CC00, "condition false writes 0x00");
}

#[test]
fn st_and_sf_constants() {
    let (mut cpu, mut bus) = setup(&[0x50C0]); // ST D0 (always)
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0xFF);

    let (mut cpu, mut bus) = setup(&[0x51C0]); // SF D0 (never)
    cpu.d[0] = 0xFF;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x00);
}

#[test]
fn seq_memory_destination_and_no_flag_change() {
    let (mut cpu, mut bus) = setup(&[0x57D0]); // SEQ (A0)
    cpu.set_flag(SrFlag::Z, true);
    cpu.set_flag(SrFlag::X, true);
    cpu.a[0] = 0x3000;
    step(&mut cpu, &mut bus);
    assert_eq!(bus.memory[0x3000], 0xFF, "Z set: SEQ writes 0xFF");
    let sr_before = cpu.sr;
    assert!(cpu.flag_is_set(SrFlag::Z), "Scc never alters the CCR");
    assert_eq!(cpu.sr, sr_before);
}

#[test]
fn sgt_signed_condition() {
    let (mut cpu, mut bus) = setup(&[0x5EC0]); // SGT D0
    cpu.set_flag(SrFlag::N, true);
    cpu.set_flag(SrFlag::V, true); // N == V, Z clear -> GT
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0xFF);

    let (mut cpu, mut bus) = setup(&[0x5EC0]);
    cpu.set_flag(SrFlag::Z, true); // Z set -> not GT
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x00);
}
