use phosphor_core::core::BusMaster;
/// Tests for M6809 extended addressing mode load/store, direct/extended
/// unary/shift ops, and JMP/JSR extended instructions (Tier 1 ops).
use phosphor_core::core::component::BusMasterComponent;
use phosphor_core::cpu::m6809::{CcFlag, M6809};

mod common;
use common::TestBus;

fn tick(cpu: &mut M6809, bus: &mut TestBus, n: usize) {
    for _ in 0..n {
        cpu.tick_with_bus(bus, BusMaster::Cpu(0));
    }
}

// ============================================================
// Extended 8-bit load/store
// ============================================================

#[test]
fn test_lda_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // LDA $1234 ; opcode=0xB6, addr=0x12,0x34
    bus.load(0, &[0xB6, 0x12, 0x34]);
    bus.memory[0x1234] = 0x42;
    tick(&mut cpu, &mut bus, 5); // 1 fetch + 4 execute
    assert_eq!(cpu.a, 0x42);
    assert_eq!(cpu.pc, 3);
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
}

#[test]
fn test_lda_extended_negative() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0xB6, 0x80, 0x00]);
    bus.memory[0x8000] = 0x80;
    tick(&mut cpu, &mut bus, 5);
    assert_eq!(cpu.a, 0x80);
    assert_eq!(cpu.cc & (CcFlag::N as u8), CcFlag::N as u8);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);
}

#[test]
fn test_lda_extended_zero() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0xB6, 0x10, 0x00]);
    bus.memory[0x1000] = 0x00;
    tick(&mut cpu, &mut bus, 5);
    assert_eq!(cpu.a, 0x00);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), CcFlag::Z as u8);
}

#[test]
fn test_sta_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    cpu.a = 0x55;
    // STA $2000
    bus.load(0, &[0xB7, 0x20, 0x00]);
    tick(&mut cpu, &mut bus, 5); // 1 fetch + 4 execute
    assert_eq!(bus.memory[0x2000], 0x55);
    assert_eq!(cpu.pc, 3);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
}

#[test]
fn test_ldb_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0xF6, 0x30, 0x00]);
    bus.memory[0x3000] = 0xAB;
    tick(&mut cpu, &mut bus, 5);
    assert_eq!(cpu.b, 0xAB);
    assert_eq!(cpu.cc & (CcFlag::N as u8), CcFlag::N as u8);
}

#[test]
fn test_stb_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    cpu.b = 0x00;
    bus.load(0, &[0xF7, 0x40, 0x00]);
    tick(&mut cpu, &mut bus, 5);
    assert_eq!(bus.memory[0x4000], 0x00);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), CcFlag::Z as u8);
}

// ============================================================
// Extended 16-bit load/store
// ============================================================

#[test]
fn test_ldd_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // LDD $1000
    bus.load(0, &[0xFC, 0x10, 0x00]);
    bus.memory[0x1000] = 0x12;
    bus.memory[0x1001] = 0x34;
    tick(&mut cpu, &mut bus, 6); // 1 fetch + 5 execute
    assert_eq!(cpu.a, 0x12);
    assert_eq!(cpu.b, 0x34);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
}

#[test]
fn test_std_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    cpu.a = 0xAB;
    cpu.b = 0xCD;
    bus.load(0, &[0xFD, 0x20, 0x00]);
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x2000], 0xAB);
    assert_eq!(bus.memory[0x2001], 0xCD);
}

#[test]
fn test_ldx_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0xBE, 0x10, 0x00]);
    bus.memory[0x1000] = 0xDE;
    bus.memory[0x1001] = 0xAD;
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(cpu.x, 0xDEAD);
    assert_eq!(cpu.cc & (CcFlag::N as u8), CcFlag::N as u8);
}

#[test]
fn test_stx_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    cpu.x = 0xBEEF;
    bus.load(0, &[0xBF, 0x30, 0x00]);
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x3000], 0xBE);
    assert_eq!(bus.memory[0x3001], 0xEF);
}

#[test]
fn test_ldu_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0xFE, 0x10, 0x00]);
    bus.memory[0x1000] = 0xCA;
    bus.memory[0x1001] = 0xFE;
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(cpu.u, 0xCAFE);
}

#[test]
fn test_stu_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    cpu.u = 0x1234;
    bus.load(0, &[0xFF, 0x50, 0x00]);
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x5000], 0x12);
    assert_eq!(bus.memory[0x5001], 0x34);
}

// ============================================================
// Direct-page unary ops (0x00-0x0F)
// ============================================================

#[test]
fn test_neg_direct() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // NEG $10 (DP=0x00)
    bus.load(0, &[0x00, 0x10]);
    bus.memory[0x0010] = 0x01;
    tick(&mut cpu, &mut bus, 6); // 1 fetch + 5 execute (rmw_direct)
    assert_eq!(bus.memory[0x0010], 0xFF);
    assert_eq!(cpu.cc & (CcFlag::N as u8), CcFlag::N as u8);
    assert_eq!(cpu.cc & (CcFlag::C as u8), CcFlag::C as u8);
}

#[test]
fn test_neg_direct_overflow() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x00, 0x10]);
    bus.memory[0x0010] = 0x80;
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x0010], 0x80);
    assert_eq!(cpu.cc & (CcFlag::V as u8), CcFlag::V as u8);
}

#[test]
fn test_com_direct() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x03, 0x20]);
    bus.memory[0x0020] = 0x0F;
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x0020], 0xF0);
    assert_eq!(cpu.cc & (CcFlag::C as u8), CcFlag::C as u8);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
}

#[test]
fn test_inc_direct() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x0C, 0x10]);
    bus.memory[0x0010] = 0x7F;
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x0010], 0x80);
    assert_eq!(cpu.cc & (CcFlag::V as u8), CcFlag::V as u8);
    assert_eq!(cpu.cc & (CcFlag::N as u8), CcFlag::N as u8);
}

#[test]
fn test_dec_direct() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x0A, 0x10]);
    bus.memory[0x0010] = 0x80;
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x0010], 0x7F);
    assert_eq!(cpu.cc & (CcFlag::V as u8), CcFlag::V as u8);
}

#[test]
fn test_tst_direct() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x0D, 0x10]);
    bus.memory[0x0010] = 0x00;
    tick(&mut cpu, &mut bus, 6); // 1 fetch + 5 execute (rmw_direct timing)
    assert_eq!(bus.memory[0x0010], 0x00); // not modified
    assert_eq!(cpu.cc & (CcFlag::Z as u8), CcFlag::Z as u8);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
}

#[test]
fn test_clr_direct() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x0F, 0x10]);
    bus.memory[0x0010] = 0xFF;
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x0010], 0x00);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), CcFlag::Z as u8);
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::C as u8), 0);
}

#[test]
fn test_neg_direct_with_dp() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    cpu.dp = 0x10;
    bus.load(0, &[0x00, 0x20]); // NEG $1020
    bus.memory[0x1020] = 0x05;
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x1020], 0xFB);
}

// ============================================================
// Direct-page shift ops (0x04-0x09)
// ============================================================

#[test]
fn test_lsr_direct() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x04, 0x10]);
    bus.memory[0x0010] = 0x81;
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x0010], 0x40);
    assert_eq!(cpu.cc & (CcFlag::C as u8), CcFlag::C as u8);
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0);
}

#[test]
fn test_asr_direct() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x07, 0x10]);
    bus.memory[0x0010] = 0x80;
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x0010], 0xC0); // sign bit preserved
    assert_eq!(cpu.cc & (CcFlag::N as u8), CcFlag::N as u8);
}

#[test]
fn test_asl_direct() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x08, 0x10]);
    bus.memory[0x0010] = 0xC0;
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x0010], 0x80);
    assert_eq!(cpu.cc & (CcFlag::C as u8), CcFlag::C as u8);
}

#[test]
fn test_rol_direct() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    cpu.cc |= CcFlag::C as u8; // carry set
    bus.load(0, &[0x09, 0x10]);
    bus.memory[0x0010] = 0x80;
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x0010], 0x01); // old C enters bit 0
    assert_eq!(cpu.cc & (CcFlag::C as u8), CcFlag::C as u8); // old bit 7 → C
}

#[test]
fn test_ror_direct() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    cpu.cc |= CcFlag::C as u8; // carry set
    bus.load(0, &[0x06, 0x10]);
    bus.memory[0x0010] = 0x01;
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x0010], 0x80); // old C enters bit 7
    assert_eq!(cpu.cc & (CcFlag::C as u8), CcFlag::C as u8); // old bit 0 → C
}

// ============================================================
// Extended unary ops (0x70-0x7F)
// ============================================================

#[test]
fn test_neg_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x70, 0x20, 0x00]);
    bus.memory[0x2000] = 0x01;
    tick(&mut cpu, &mut bus, 7); // 1 fetch + 6 execute (rmw_extended)
    assert_eq!(bus.memory[0x2000], 0xFF);
    assert_eq!(cpu.cc & (CcFlag::N as u8), CcFlag::N as u8);
    assert_eq!(cpu.cc & (CcFlag::C as u8), CcFlag::C as u8);
}

#[test]
fn test_com_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x73, 0x20, 0x00]);
    bus.memory[0x2000] = 0xFF;
    tick(&mut cpu, &mut bus, 7);
    assert_eq!(bus.memory[0x2000], 0x00);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), CcFlag::Z as u8);
    assert_eq!(cpu.cc & (CcFlag::C as u8), CcFlag::C as u8);
}

#[test]
fn test_inc_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x7C, 0x20, 0x00]);
    bus.memory[0x2000] = 0xFF;
    tick(&mut cpu, &mut bus, 7);
    assert_eq!(bus.memory[0x2000], 0x00);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), CcFlag::Z as u8);
}

#[test]
fn test_dec_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x7A, 0x20, 0x00]);
    bus.memory[0x2000] = 0x01;
    tick(&mut cpu, &mut bus, 7);
    assert_eq!(bus.memory[0x2000], 0x00);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), CcFlag::Z as u8);
}

#[test]
fn test_tst_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x7D, 0x20, 0x00]);
    bus.memory[0x2000] = 0x80;
    tick(&mut cpu, &mut bus, 7); // 1 fetch + 6 execute (rmw_extended timing)
    assert_eq!(bus.memory[0x2000], 0x80); // not modified
    assert_eq!(cpu.cc & (CcFlag::N as u8), CcFlag::N as u8);
}

#[test]
fn test_clr_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x7F, 0x20, 0x00]);
    bus.memory[0x2000] = 0xAA;
    tick(&mut cpu, &mut bus, 7);
    assert_eq!(bus.memory[0x2000], 0x00);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), CcFlag::Z as u8);
}

// ============================================================
// Extended shift ops (0x74-0x79)
// ============================================================

#[test]
fn test_lsr_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x74, 0x20, 0x00]);
    bus.memory[0x2000] = 0x02;
    tick(&mut cpu, &mut bus, 7);
    assert_eq!(bus.memory[0x2000], 0x01);
    assert_eq!(cpu.cc & (CcFlag::C as u8), 0);
}

#[test]
fn test_asr_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x77, 0x20, 0x00]);
    bus.memory[0x2000] = 0x81;
    tick(&mut cpu, &mut bus, 7);
    assert_eq!(bus.memory[0x2000], 0xC0);
    assert_eq!(cpu.cc & (CcFlag::C as u8), CcFlag::C as u8);
}

#[test]
fn test_asl_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x78, 0x20, 0x00]);
    bus.memory[0x2000] = 0x40;
    tick(&mut cpu, &mut bus, 7);
    assert_eq!(bus.memory[0x2000], 0x80);
    assert_eq!(cpu.cc & (CcFlag::C as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::N as u8), CcFlag::N as u8);
}

#[test]
fn test_rol_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x79, 0x20, 0x00]);
    bus.memory[0x2000] = 0x80;
    tick(&mut cpu, &mut bus, 7);
    assert_eq!(bus.memory[0x2000], 0x00);
    assert_eq!(cpu.cc & (CcFlag::C as u8), CcFlag::C as u8);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), CcFlag::Z as u8);
}

#[test]
fn test_ror_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x76, 0x20, 0x00]);
    bus.memory[0x2000] = 0x01;
    tick(&mut cpu, &mut bus, 7);
    assert_eq!(bus.memory[0x2000], 0x00);
    assert_eq!(cpu.cc & (CcFlag::C as u8), CcFlag::C as u8);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), CcFlag::Z as u8);
}

// ============================================================
// JMP direct and extended
// ============================================================

#[test]
fn test_jmp_direct() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // JMP $30 (DP=0x00 → jump to 0x0030)
    bus.load(0, &[0x0E, 0x30]);
    tick(&mut cpu, &mut bus, 3); // 1 fetch + 2 execute
    assert_eq!(cpu.pc, 0x0030);
}

#[test]
fn test_jmp_direct_with_dp() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    cpu.dp = 0x80;
    bus.load(0, &[0x0E, 0x50]);
    tick(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.pc, 0x8050);
}

#[test]
fn test_jmp_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // JMP $1234
    bus.load(0, &[0x7E, 0x12, 0x34]);
    tick(&mut cpu, &mut bus, 4); // 1 fetch + 3 execute
    assert_eq!(cpu.pc, 0x1234);
}

// ============================================================
// JSR extended
// ============================================================

#[test]
fn test_jsr_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    cpu.s = 0x4000; // stack pointer
    // JSR $2000
    bus.load(0, &[0xBD, 0x20, 0x00]);
    tick(&mut cpu, &mut bus, 8); // 1 fetch + 7 execute
    assert_eq!(cpu.pc, 0x2000);
    // Return address (0x0003) should be on stack
    // JSR pushes PC low first (at S-1=0x3FFF), then PC high (at S-2=0x3FFE)
    assert_eq!(bus.memory[0x3FFF], 0x03); // PC low
    assert_eq!(bus.memory[0x3FFE], 0x00); // PC high
    assert_eq!(cpu.s, 0x3FFE);
}

#[test]
fn test_jsr_extended_and_rts() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    cpu.s = 0x4000;
    // JSR $0010 at address 0x0000
    bus.load(0x0000, &[0xBD, 0x00, 0x10]);
    // RTS at address 0x0010
    bus.memory[0x0010] = 0x39; // RTS
    // Execute JSR
    tick(&mut cpu, &mut bus, 8);
    assert_eq!(cpu.pc, 0x0010);
    // Execute RTS
    tick(&mut cpu, &mut bus, 5); // 1 fetch + 4 execute
    assert_eq!(cpu.pc, 0x0003); // return to address after JSR
}

// ============================================================
// Multi-instruction sequences
// ============================================================

#[test]
fn test_lda_extended_sta_extended() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // LDA $1000; STA $2000
    bus.load(0, &[0xB6, 0x10, 0x00, 0xB7, 0x20, 0x00]);
    bus.memory[0x1000] = 0x77;
    tick(&mut cpu, &mut bus, 5); // LDA extended
    assert_eq!(cpu.a, 0x77);
    tick(&mut cpu, &mut bus, 5); // STA extended
    assert_eq!(bus.memory[0x2000], 0x77);
    assert_eq!(cpu.pc, 6);
}

#[test]
fn test_clr_direct_then_inc_direct() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.memory[0x0010] = 0xAA;
    // CLR $10; INC $10
    bus.load(0, &[0x0F, 0x10, 0x0C, 0x10]);
    tick(&mut cpu, &mut bus, 6); // CLR direct
    assert_eq!(bus.memory[0x0010], 0x00);
    tick(&mut cpu, &mut bus, 6); // INC direct
    assert_eq!(bus.memory[0x0010], 0x01);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);
}
