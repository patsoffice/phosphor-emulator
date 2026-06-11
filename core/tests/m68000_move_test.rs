//! M68000 MOVE / MOVEA / MOVEQ integration tests.
//!
//! Opcode layout reminder for MOVE (lines 0x1 = .b, 0x3 = .w, 0x2 = .l):
//! `00ss rrr mmm MMM RRR` — size, dest reg, dest mode, source mode, source
//! reg. Destination mode 1 (An) selects MOVEA.

mod common;

use common::TestBus68k;
use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::m68000::{M68000, SrFlag};

const M: BusMaster = BusMaster::Cpu(0);

/// CPU at PC=0x1000 with the given opcode words loaded there.
fn setup(words: &[u16]) -> (M68000, TestBus68k) {
    let mut cpu = M68000::new();
    cpu.pc = 0x1000;
    let mut bus = TestBus68k::new();
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    bus.load(0x1000, &bytes);
    (cpu, bus)
}

/// Run one full instruction (tick to the next instruction boundary).
fn step(cpu: &mut M68000, bus: &mut TestBus68k) {
    let mut ticks = 0;
    while !cpu.tick_with_bus(bus, M) {
        ticks += 1;
        assert!(ticks < 100, "instruction did not complete");
    }
}

fn flag(cpu: &M68000, f: SrFlag) -> bool {
    cpu.flag_is_set(f)
}

// ---------------------------------------------------------------------------
// MOVE between data registers, sizes and flags
// ---------------------------------------------------------------------------

#[test]
fn move_w_dn_to_dn_sets_n_clears_vc_keeps_x() {
    let (mut cpu, mut bus) = setup(&[0x3001]); // MOVE.w D1,D0
    cpu.d[1] = 0x8000;
    cpu.set_flag(SrFlag::X, true);
    cpu.set_flag(SrFlag::V, true);
    cpu.set_flag(SrFlag::C, true);
    step(&mut cpu, &mut bus);

    assert_eq!(cpu.d[0] & 0xFFFF, 0x8000);
    assert!(flag(&cpu, SrFlag::N), "negative word sets N");
    assert!(!flag(&cpu, SrFlag::Z));
    assert!(!flag(&cpu, SrFlag::V), "MOVE clears V");
    assert!(!flag(&cpu, SrFlag::C), "MOVE clears C");
    assert!(flag(&cpu, SrFlag::X), "MOVE never touches X");
    assert_eq!(cpu.pc, 0x1002);
}

#[test]
fn move_w_zero_sets_z() {
    let (mut cpu, mut bus) = setup(&[0x3001]); // MOVE.w D1,D0
    cpu.d[1] = 0xABCD_0000; // low word zero
    cpu.d[0] = 0xFFFF_FFFF;
    step(&mut cpu, &mut bus);

    assert_eq!(cpu.d[0], 0xFFFF_0000, "word write preserves upper bits");
    assert!(flag(&cpu, SrFlag::Z));
    assert!(!flag(&cpu, SrFlag::N));
}

#[test]
fn move_b_preserves_upper_bits_and_masks_value() {
    let (mut cpu, mut bus) = setup(&[0x1001]); // MOVE.b D1,D0
    cpu.d[1] = 0x1234_5680; // byte = 0x80 (negative)
    cpu.d[0] = 0xAABB_CCDD;
    step(&mut cpu, &mut bus);

    assert_eq!(cpu.d[0], 0xAABB_CC80);
    assert!(flag(&cpu, SrFlag::N), "byte sign comes from bit 7");
}

#[test]
fn move_l_full_register() {
    let (mut cpu, mut bus) = setup(&[0x2001]); // MOVE.l D1,D0
    cpu.d[1] = 0x7FFF_FFFF;
    step(&mut cpu, &mut bus);

    assert_eq!(cpu.d[0], 0x7FFF_FFFF);
    assert!(!flag(&cpu, SrFlag::N), "long sign comes from bit 31");
}

// ---------------------------------------------------------------------------
// Source addressing modes
// ---------------------------------------------------------------------------

#[test]
fn move_w_from_an_indirect() {
    let (mut cpu, mut bus) = setup(&[0x3010]); // MOVE.w (A0),D0
    cpu.a[0] = 0x4000;
    bus.load(0x4000, &[0x12, 0x34]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x1234);
    assert_eq!(cpu.a[0], 0x4000);
}

#[test]
fn move_w_from_postincrement() {
    let (mut cpu, mut bus) = setup(&[0x3018]); // MOVE.w (A0)+,D0
    cpu.a[0] = 0x4000;
    bus.load(0x4000, &[0x12, 0x34]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x1234);
    assert_eq!(cpu.a[0], 0x4002, "(An)+ increments after use");
}

#[test]
fn move_b_from_postincrement_a7_steps_by_two() {
    let (mut cpu, mut bus) = setup(&[0x101F]); // MOVE.b (A7)+,D0
    cpu.a[7] = 0x4000;
    bus.load(0x4000, &[0x55]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x55);
    assert_eq!(cpu.a[7], 0x4002, "A7 byte step keeps SP word-aligned");
}

#[test]
fn move_w_from_predecrement() {
    let (mut cpu, mut bus) = setup(&[0x3020]); // MOVE.w -(A0),D0
    cpu.a[0] = 0x4002;
    bus.load(0x4000, &[0x12, 0x34]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x1234);
    assert_eq!(cpu.a[0], 0x4000, "-(An) decrements before use");
}

#[test]
fn move_w_from_displacement() {
    let (mut cpu, mut bus) = setup(&[0x3028, 0xFFF0]); // MOVE.w -16(A0),D0
    cpu.a[0] = 0x4010;
    bus.load(0x4000, &[0x12, 0x34]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x1234);
    assert_eq!(cpu.pc, 0x1004, "one extension word consumed");
}

#[test]
fn move_w_from_indexed() {
    // MOVE.w 4(A0,D2.w),D0 — brief extension: D2.w, disp8 = +4
    let (mut cpu, mut bus) = setup(&[0x3030, 0x2004]);
    cpu.a[0] = 0x4000;
    cpu.d[2] = 0x10;
    bus.load(0x4014, &[0x12, 0x34]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x1234);
}

#[test]
fn move_w_from_absolute_short_and_long() {
    let (mut cpu, mut bus) = setup(&[0x3038, 0x4000]); // MOVE.w $4000.w,D0
    bus.load(0x4000, &[0x12, 0x34]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x1234);

    let (mut cpu, mut bus) = setup(&[0x3039, 0x0012, 0x3456]); // MOVE.w $123456.l,D0
    bus.load(0x12_3456, &[0xBE, 0xEF]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0xBEEF);
    assert_eq!(cpu.pc, 0x1006, "two extension words consumed");
}

#[test]
fn move_w_from_pc_relative() {
    // MOVE.w $10(PC),D0 — base is the extension word address (0x1002)
    let (mut cpu, mut bus) = setup(&[0x303A, 0x0010]);
    bus.load(0x1012, &[0x12, 0x34]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x1234);
}

#[test]
fn move_w_from_pc_indexed() {
    // MOVE.w 2(PC,D3.w),D0 — base 0x1002, index 0x10, disp +2
    let (mut cpu, mut bus) = setup(&[0x303B, 0x3002]);
    cpu.d[3] = 0x10;
    bus.load(0x1014, &[0x12, 0x34]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x1234);
}

#[test]
fn move_from_immediate_all_sizes() {
    let (mut cpu, mut bus) = setup(&[0x103C, 0x0080]); // MOVE.b #$80,D0
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFF, 0x80);
    assert!(flag(&cpu, SrFlag::N));
    assert_eq!(cpu.pc, 0x1004);

    let (mut cpu, mut bus) = setup(&[0x303C, 0x1234]); // MOVE.w #$1234,D0
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x1234);

    let (mut cpu, mut bus) = setup(&[0x203C, 0xDEAD, 0xBEEF]); // MOVE.l #$DEADBEEF,D0
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xDEAD_BEEF);
    assert_eq!(cpu.pc, 0x1006);
}

// ---------------------------------------------------------------------------
// Destination addressing modes
// ---------------------------------------------------------------------------

#[test]
fn move_w_to_an_indirect() {
    let (mut cpu, mut bus) = setup(&[0x3081]); // MOVE.w D1,(A0)
    cpu.a[0] = 0x4000;
    cpu.d[1] = 0xBEEF;
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x4000..0x4002], &[0xBE, 0xEF]);
}

#[test]
fn move_w_to_postincrement_and_predecrement() {
    let (mut cpu, mut bus) = setup(&[0x30C1]); // MOVE.w D1,(A0)+
    cpu.a[0] = 0x4000;
    cpu.d[1] = 0xBEEF;
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x4000..0x4002], &[0xBE, 0xEF]);
    assert_eq!(cpu.a[0], 0x4002);

    let (mut cpu, mut bus) = setup(&[0x3101]); // MOVE.w D1,-(A0)
    cpu.a[0] = 0x4002;
    cpu.d[1] = 0xBEEF;
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x4000..0x4002], &[0xBE, 0xEF]);
    assert_eq!(cpu.a[0], 0x4000);
}

#[test]
fn move_b_to_memory_preserves_neighbor_byte() {
    let (mut cpu, mut bus) = setup(&[0x1081]); // MOVE.b D1,(A0)
    cpu.a[0] = 0x4001; // odd byte address
    cpu.d[1] = 0x42;
    bus.load(0x4000, &[0xAA, 0xBB]);
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x4000..0x4002], &[0xAA, 0x42]);
}

#[test]
fn move_w_to_displacement_indexed_and_absolute() {
    let (mut cpu, mut bus) = setup(&[0x3141, 0x0010]); // MOVE.w D1,16(A0)
    cpu.a[0] = 0x4000;
    cpu.d[1] = 0xBEEF;
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x4010..0x4012], &[0xBE, 0xEF]);

    // MOVE.w D1,4(A0,D2.w)
    let (mut cpu, mut bus) = setup(&[0x3181, 0x2004]);
    cpu.a[0] = 0x4000;
    cpu.d[2] = 0x10;
    cpu.d[1] = 0xBEEF;
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x4014..0x4016], &[0xBE, 0xEF]);

    let (mut cpu, mut bus) = setup(&[0x31C1, 0x4000]); // MOVE.w D1,$4000.w
    cpu.d[1] = 0xBEEF;
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x4000..0x4002], &[0xBE, 0xEF]);

    let (mut cpu, mut bus) = setup(&[0x33C1, 0x0012, 0x3456]); // MOVE.w D1,$123456.l
    cpu.d[1] = 0xBEEF;
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x12_3456..0x12_3458], &[0xBE, 0xEF]);
}

#[test]
fn move_l_memory_to_memory() {
    // MOVE.l (A0),(A1) — mem-to-mem in one instruction
    let (mut cpu, mut bus) = setup(&[0x2290]);
    cpu.a[0] = 0x4000;
    cpu.a[1] = 0x5000;
    bus.load(0x4000, &[0xDE, 0xAD, 0xBE, 0xEF]);
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x5000..0x5004], &[0xDE, 0xAD, 0xBE, 0xEF]);
}

// ---------------------------------------------------------------------------
// MOVEA
// ---------------------------------------------------------------------------

#[test]
fn movea_w_sign_extends_and_sets_no_flags() {
    let (mut cpu, mut bus) = setup(&[0x327C, 0x8000]); // MOVEA.w #$8000,A1
    cpu.sr &= 0xFF00; // clear CCR
    step(&mut cpu, &mut bus);

    assert_eq!(cpu.a[1], 0xFFFF_8000, "MOVEA.w sign-extends");
    assert_eq!(cpu.sr & 0x001F, 0, "MOVEA sets no flags (even N)");
}

#[test]
fn movea_w_positive_clears_upper_bits() {
    let (mut cpu, mut bus) = setup(&[0x327C, 0x7FFF]); // MOVEA.w #$7FFF,A1
    cpu.a[1] = 0xDEAD_BEEF;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[1], 0x0000_7FFF);
}

#[test]
fn movea_l_full_value_and_zero_sets_no_flags() {
    let (mut cpu, mut bus) = setup(&[0x227C, 0x00FE, 0xDCBA]); // MOVEA.l #$FEDCBA,A1
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[1], 0x00FE_DCBA);

    let (mut cpu, mut bus) = setup(&[0x227C, 0x0000, 0x0000]); // MOVEA.l #0,A1
    cpu.sr &= 0xFF00;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[1], 0);
    assert!(!flag(&cpu, SrFlag::Z), "MOVEA never sets Z");
}

#[test]
fn movea_from_an_source() {
    let (mut cpu, mut bus) = setup(&[0x3249]); // MOVEA.w A1,A1 (low word of A1)
    cpu.a[1] = 0x1234_8001;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[1], 0xFFFF_8001, "An word source sign-extends too");
}

// ---------------------------------------------------------------------------
// An as MOVE source (word size reads the low word)
// ---------------------------------------------------------------------------

#[test]
fn move_w_from_address_register() {
    let (mut cpu, mut bus) = setup(&[0x3009]); // MOVE.w A1,D0
    cpu.a[1] = 0x1234_5678;
    cpu.d[0] = 0xFFFF_FFFF;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xFFFF_5678);
}

// ---------------------------------------------------------------------------
// MOVEQ
// ---------------------------------------------------------------------------

#[test]
fn moveq_sign_extends_negative_literal() {
    let (mut cpu, mut bus) = setup(&[0x70FF]); // MOVEQ #-1,D0
    cpu.d[0] = 0;
    cpu.set_flag(SrFlag::X, true);
    cpu.set_flag(SrFlag::V, true);
    cpu.set_flag(SrFlag::C, true);
    step(&mut cpu, &mut bus);

    assert_eq!(cpu.d[0], 0xFFFF_FFFF, "8-bit literal sign-extends to 32");
    assert!(flag(&cpu, SrFlag::N));
    assert!(!flag(&cpu, SrFlag::Z));
    assert!(!flag(&cpu, SrFlag::V), "MOVEQ clears V");
    assert!(!flag(&cpu, SrFlag::C), "MOVEQ clears C");
    assert!(flag(&cpu, SrFlag::X), "MOVEQ never touches X");
    assert_eq!(cpu.pc, 0x1002);
}

#[test]
fn moveq_positive_and_zero() {
    let (mut cpu, mut bus) = setup(&[0x7242]); // MOVEQ #$42,D1
    cpu.d[1] = 0xDEAD_BEEF;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1], 0x0000_0042, "whole register is written");
    assert!(!flag(&cpu, SrFlag::N));
    assert!(!flag(&cpu, SrFlag::Z));

    let (mut cpu, mut bus) = setup(&[0x7400]); // MOVEQ #0,D2
    cpu.d[2] = 1;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[2], 0);
    assert!(flag(&cpu, SrFlag::Z));
}

#[test]
fn moveq_boundary_values() {
    let (mut cpu, mut bus) = setup(&[0x707F]); // MOVEQ #$7F,D0 (max positive)
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0x0000_007F);
    assert!(!flag(&cpu, SrFlag::N));

    let (mut cpu, mut bus) = setup(&[0x7080]); // MOVEQ #-128,D0
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xFFFF_FF80);
    assert!(flag(&cpu, SrFlag::N));
}

// ---------------------------------------------------------------------------
// SWAP
// ---------------------------------------------------------------------------

#[test]
fn swap_exchanges_halves_and_sets_n_from_new_bit_31() {
    let (mut cpu, mut bus) = setup(&[0x4840]); // SWAP D0
    cpu.d[0] = 0x1234_8765;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0x8765_1234);
    assert!(flag(&cpu, SrFlag::N), "new upper word has bit 15 set");
    assert!(!flag(&cpu, SrFlag::Z));
}

#[test]
fn swap_zero_sets_z_and_leaves_x() {
    let (mut cpu, mut bus) = setup(&[0x4847]); // SWAP D7
    cpu.set_flag(SrFlag::X, true);
    cpu.set_flag(SrFlag::C, true);
    cpu.d[7] = 0;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[7], 0);
    assert!(flag(&cpu, SrFlag::Z));
    assert!(!flag(&cpu, SrFlag::C), "C cleared by the logical rule");
    assert!(flag(&cpu, SrFlag::X), "X untouched by data movement");
}

// ---------------------------------------------------------------------------
// EXG
// ---------------------------------------------------------------------------

#[test]
fn exg_data_registers() {
    let (mut cpu, mut bus) = setup(&[0xC342]); // EXG D1,D2
    cpu.d[1] = 0x1111_1111;
    cpu.d[2] = 0x2222_2222;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1], 0x2222_2222);
    assert_eq!(cpu.d[2], 0x1111_1111);
}

#[test]
fn exg_address_registers() {
    let (mut cpu, mut bus) = setup(&[0xC34A]); // EXG A1,A2
    cpu.a[1] = 0xAAAA_0001;
    cpu.a[2] = 0xBBBB_0002;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[1], 0xBBBB_0002);
    assert_eq!(cpu.a[2], 0xAAAA_0001);
}

#[test]
fn exg_data_with_address_register_and_no_flags() {
    let (mut cpu, mut bus) = setup(&[0xC38A]); // EXG D1,A2
    cpu.sr = (cpu.sr & 0xFF00) | 0x1F;
    cpu.d[1] = 0x8000_0000;
    cpu.a[2] = 0x0000_0000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1], 0x0000_0000);
    assert_eq!(cpu.a[2], 0x8000_0000);
    assert_eq!(cpu.sr & 0x1F, 0x1F, "EXG never alters the CCR");
}
