use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::m6800::{CcFlag, M6800};
mod common;
use common::TestBus;

/// Helper: tick the CPU for `n` cycles
fn tick(cpu: &mut M6800, bus: &mut TestBus, n: usize) {
    for _ in 0..n {
        cpu.tick_with_bus(bus, BusMaster::Cpu(0));
    }
}

// =============================================================================
// NOP (0x01) - 2 cycles
// =============================================================================

#[test]
fn test_nop() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x01]); // NOP
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.pc, 1);
}

// =============================================================================
// NEG - Negate (two's complement)
// =============================================================================

#[test]
fn test_nega_positive() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x05;
    bus.load(0, &[0x40]); // NEGA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0xFB); // 0 - 5 = 251 (or -5 signed)
    assert_ne!(cpu.cc & (CcFlag::Z as u8), CcFlag::Z as u8); // not zero
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0); // negative
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0); // carry (borrow from 0)
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0); // no overflow
}

#[test]
fn test_nega_zero() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x00;
    bus.load(0, &[0x40]); // NEGA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x00);
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0); // zero
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0); // not negative
    assert_eq!(cpu.cc & (CcFlag::C as u8), 0); // no carry
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0); // no overflow
}

#[test]
fn test_nega_overflow_0x80() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x80;
    bus.load(0, &[0x40]); // NEGA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x80); // -(-128) overflows back to -128
    assert_ne!(cpu.cc & (CcFlag::V as u8), 0); // overflow!
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0); // negative
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0); // carry
}

#[test]
fn test_negb_positive() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.b = 0x01;
    bus.load(0, &[0x50]); // NEGB
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.b, 0xFF);
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0);
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0);
}

// =============================================================================
// COM - Complement (one's complement / bitwise NOT)
// =============================================================================

#[test]
fn test_coma() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x55;
    bus.load(0, &[0x43]); // COMA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0xAA);
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0); // negative
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0); // not zero
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0); // C always set
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0); // V always cleared
}

#[test]
fn test_coma_ff() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0xFF;
    bus.load(0, &[0x43]); // COMA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x00);
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0); // zero
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0); // not negative
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0); // C always set
}

#[test]
fn test_comb() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.b = 0x00;
    bus.load(0, &[0x53]); // COMB
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.b, 0xFF);
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0);
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0);
}

// =============================================================================
// CLR - Clear register
// =============================================================================

#[test]
fn test_clra() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x42;
    cpu.cc = CcFlag::N as u8 | CcFlag::C as u8; // set some flags
    bus.load(0, &[0x4F]); // CLRA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x00);
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0); // Z set
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0); // N cleared
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0); // V cleared
    assert_eq!(cpu.cc & (CcFlag::C as u8), 0); // C cleared
}

#[test]
fn test_clrb() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.b = 0xFF;
    bus.load(0, &[0x5F]); // CLRB
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.b, 0x00);
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0);
}

// =============================================================================
// INC - Increment
// =============================================================================

#[test]
fn test_inca() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x05;
    bus.load(0, &[0x4C]); // INCA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x06);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
}

#[test]
fn test_inca_overflow_7f() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x7F;
    bus.load(0, &[0x4C]); // INCA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x80);
    assert_ne!(cpu.cc & (CcFlag::V as u8), 0); // signed overflow
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0); // negative
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);
}

#[test]
fn test_inca_wrap_ff() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0xFF;
    bus.load(0, &[0x4C]); // INCA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x00);
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0); // zero
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0); // no overflow (0xFF is -1, -1+1=0)
}

#[test]
fn test_inca_does_not_affect_carry() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0xFF;
    cpu.cc = CcFlag::C as u8; // set carry
    bus.load(0, &[0x4C]); // INCA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x00);
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0); // carry preserved
}

#[test]
fn test_incb() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.b = 0x7F;
    bus.load(0, &[0x5C]); // INCB
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.b, 0x80);
    assert_ne!(cpu.cc & (CcFlag::V as u8), 0);
}

// =============================================================================
// DEC - Decrement
// =============================================================================

#[test]
fn test_deca() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x05;
    bus.load(0, &[0x4A]); // DECA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x04);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0);
}

#[test]
fn test_deca_to_zero() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x01;
    bus.load(0, &[0x4A]); // DECA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x00);
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0); // zero
}

#[test]
fn test_deca_overflow_80() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x80;
    bus.load(0, &[0x4A]); // DECA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x7F);
    assert_ne!(cpu.cc & (CcFlag::V as u8), 0); // signed overflow
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0); // positive
}

#[test]
fn test_deca_wrap_00() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x00;
    bus.load(0, &[0x4A]); // DECA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0xFF);
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0); // negative
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0); // no overflow
}

#[test]
fn test_decb() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.b = 0x80;
    bus.load(0, &[0x5A]); // DECB
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.b, 0x7F);
    assert_ne!(cpu.cc & (CcFlag::V as u8), 0);
}

// =============================================================================
// TST - Test (set flags, no modification)
// =============================================================================

#[test]
fn test_tsta_zero() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x00;
    bus.load(0, &[0x4D]); // TSTA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x00); // unchanged
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
}

#[test]
fn test_tsta_negative() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x80;
    bus.load(0, &[0x4D]); // TSTA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x80); // unchanged
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
}

#[test]
fn test_tsta_positive() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x42;
    cpu.cc = CcFlag::V as u8 | CcFlag::N as u8; // pre-set V and N
    bus.load(0, &[0x4D]); // TSTA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x42);
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0); // cleared
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0); // cleared
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);
}

#[test]
fn test_tstb() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.b = 0xFF;
    bus.load(0, &[0x5D]); // TSTB
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.b, 0xFF);
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
}

// =============================================================================
// TAB (0x16) / TBA (0x17) - Transfer between A and B
// =============================================================================

#[test]
fn test_tab() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x42;
    cpu.b = 0x00;
    bus.load(0, &[0x16]); // TAB
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.b, 0x42);
    assert_eq!(cpu.a, 0x42); // A unchanged
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0); // V cleared
}

#[test]
fn test_tab_zero() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x00;
    cpu.b = 0xFF;
    bus.load(0, &[0x16]); // TAB
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.b, 0x00);
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0);
}

#[test]
fn test_tab_negative() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x80;
    bus.load(0, &[0x16]); // TAB
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.b, 0x80);
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0);
}

#[test]
fn test_tba() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x00;
    cpu.b = 0x42;
    bus.load(0, &[0x17]); // TBA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x42);
    assert_eq!(cpu.b, 0x42); // B unchanged
}

// =============================================================================
// TAP (0x06) / TPA (0x07) - Transfer A to/from CC
// =============================================================================

#[test]
fn test_tap() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = CcFlag::C as u8 | CcFlag::Z as u8; // 0x05
    bus.load(0, &[0x06]); // TAP
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.cc, 0x05);
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0);
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0);
}

#[test]
fn test_tap_all_flags() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x3F; // all 6 flags set
    bus.load(0, &[0x06]); // TAP
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.cc, 0x3F);
    assert_ne!(cpu.cc & (CcFlag::H as u8), 0);
    assert_ne!(cpu.cc & (CcFlag::I as u8), 0);
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0);
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0);
    assert_ne!(cpu.cc & (CcFlag::V as u8), 0);
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0);
}

#[test]
fn test_tpa() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.cc = CcFlag::N as u8 | CcFlag::C as u8; // 0x09
    bus.load(0, &[0x07]); // TPA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x09 | 0xC0); // bits 6-7 always 1
    assert_eq!(cpu.a, 0xC9);
}

#[test]
fn test_tap_tpa_roundtrip() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x15; // H=0, I=1, N=0, Z=1, V=0, C=1
    bus.load(0, &[0x06, 0x07]); // TAP, TPA
    tick(&mut cpu, &mut bus, 2); // TAP
    assert_eq!(cpu.cc, 0x15);
    tick(&mut cpu, &mut bus, 2); // TPA
    assert_eq!(cpu.a, 0x15 | 0xC0); // bits 6-7 set
}

// =============================================================================
// CLC/SEC/CLV/SEV/CLI/SEI - Flag set/clear
// =============================================================================

#[test]
fn test_clc() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.cc = CcFlag::C as u8 | CcFlag::N as u8;
    bus.load(0, &[0x0C]); // CLC
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.cc & (CcFlag::C as u8), 0); // C cleared
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0); // N unchanged
}

#[test]
fn test_sec() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.cc = 0;
    bus.load(0, &[0x0D]); // SEC
    tick(&mut cpu, &mut bus, 2);
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0);
}

#[test]
fn test_clv() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.cc = CcFlag::V as u8 | CcFlag::C as u8;
    bus.load(0, &[0x0A]); // CLV
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0); // C unchanged
}

#[test]
fn test_sev() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.cc = 0;
    bus.load(0, &[0x0B]); // SEV
    tick(&mut cpu, &mut bus, 2);
    assert_ne!(cpu.cc & (CcFlag::V as u8), 0);
}

#[test]
fn test_cli() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.cc = CcFlag::I as u8;
    bus.load(0, &[0x0E]); // CLI
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.cc & (CcFlag::I as u8), 0);
}

#[test]
fn test_sei() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.cc = 0;
    bus.load(0, &[0x0F]); // SEI
    tick(&mut cpu, &mut bus, 2);
    assert_ne!(cpu.cc & (CcFlag::I as u8), 0);
}

// =============================================================================
// ABA (0x1B) - Add B to A
// =============================================================================

#[test]
fn test_aba_basic() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x10;
    cpu.b = 0x20;
    bus.load(0, &[0x1B]); // ABA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x30);
    assert_eq!(cpu.cc & (CcFlag::C as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0);
}

#[test]
fn test_aba_carry() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0xFF;
    cpu.b = 0x01;
    bus.load(0, &[0x1B]); // ABA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x00);
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0); // carry
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0); // zero
}

#[test]
fn test_aba_overflow() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x70;
    cpu.b = 0x20;
    bus.load(0, &[0x1B]); // ABA: 0x70 + 0x20 = 0x90 (signed: 112 + 32 = 144, overflow)
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x90);
    assert_ne!(cpu.cc & (CcFlag::V as u8), 0); // overflow
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0); // negative
}

#[test]
fn test_aba_half_carry() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x0F;
    cpu.b = 0x01;
    bus.load(0, &[0x1B]); // ABA: low nibble 0xF + 0x1 > 0xF
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x10);
    assert_ne!(cpu.cc & (CcFlag::H as u8), 0); // half-carry
}

#[test]
fn test_aba_no_half_carry() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x03;
    cpu.b = 0x04;
    bus.load(0, &[0x1B]); // ABA: low nibble 3 + 4 = 7, no half-carry
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x07);
    assert_eq!(cpu.cc & (CcFlag::H as u8), 0);
}

// =============================================================================
// SBA (0x10) - Subtract B from A
// =============================================================================

#[test]
fn test_sba_basic() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x30;
    cpu.b = 0x10;
    bus.load(0, &[0x10]); // SBA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x20);
    assert_eq!(cpu.cc & (CcFlag::C as u8), 0); // no borrow
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
}

#[test]
fn test_sba_borrow() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x00;
    cpu.b = 0x01;
    bus.load(0, &[0x10]); // SBA: 0 - 1 = 0xFF with borrow
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0xFF);
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0); // borrow
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0); // negative
}

#[test]
fn test_sba_overflow() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x80;
    cpu.b = 0x01;
    bus.load(0, &[0x10]); // SBA: -128 - 1 = -129, overflow
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x7F);
    assert_ne!(cpu.cc & (CcFlag::V as u8), 0); // overflow
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0); // positive result
}

#[test]
fn test_sba_equal() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x42;
    cpu.b = 0x42;
    bus.load(0, &[0x10]); // SBA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x00);
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0); // zero
    assert_eq!(cpu.cc & (CcFlag::C as u8), 0); // no borrow
}

// =============================================================================
// CBA (0x11) - Compare A to B (A - B, discard result)
// =============================================================================

#[test]
fn test_cba_equal() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x42;
    cpu.b = 0x42;
    bus.load(0, &[0x11]); // CBA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x42); // A unchanged
    assert_eq!(cpu.b, 0x42); // B unchanged
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0); // zero
    assert_eq!(cpu.cc & (CcFlag::C as u8), 0); // no borrow
}

#[test]
fn test_cba_a_greater() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x50;
    cpu.b = 0x20;
    bus.load(0, &[0x11]); // CBA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x50); // unchanged
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0); // not zero
    assert_eq!(cpu.cc & (CcFlag::C as u8), 0); // no borrow (A >= B)
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0); // positive result
}

#[test]
fn test_cba_a_less() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x10;
    cpu.b = 0x20;
    bus.load(0, &[0x11]); // CBA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x10); // unchanged
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0); // borrow (A < B)
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0); // negative result
}

// =============================================================================
// DAA (0x19) - Decimal Adjust A
// =============================================================================

#[test]
fn test_daa_no_adjustment() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x22; // valid BCD
    bus.load(0, &[0x19]); // DAA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x22); // no adjustment needed
}

#[test]
fn test_daa_low_nibble() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    // Simulate 9 + 5 = 14 (0x0E in hex, needs correction to 0x14 BCD)
    cpu.a = 0x0E;
    bus.load(0, &[0x19]); // DAA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x14); // 0x0E + 0x06 = 0x14
}

#[test]
fn test_daa_with_half_carry() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    // After adding 8+9=17: A=0x11, H=1 (half carry from lower nibble)
    cpu.a = 0x11;
    cpu.cc = CcFlag::H as u8;
    bus.load(0, &[0x19]); // DAA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x17); // 0x11 + 0x06 = 0x17
}

#[test]
fn test_daa_high_nibble() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    // Simulate result with invalid high nibble
    cpu.a = 0xA0; // high nibble > 9
    bus.load(0, &[0x19]); // DAA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x00); // 0xA0 + 0x60 = 0x100, truncated to 0x00
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0); // carry set
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0); // zero
}

#[test]
fn test_daa_with_carry() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x30;
    cpu.cc = CcFlag::C as u8; // carry from previous addition
    bus.load(0, &[0x19]); // DAA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x90); // 0x30 + 0x60 = 0x90
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0); // carry stays set
}

// =============================================================================
// INX (0x08) / DEX (0x09) - Increment/Decrement X (4 cycles)
// =============================================================================

#[test]
fn test_inx() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.x = 0x1000;
    bus.load(0, &[0x08]); // INX
    tick(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.x, 0x1001);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0); // not zero
}

#[test]
fn test_inx_zero() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.x = 0xFFFF;
    bus.load(0, &[0x08]); // INX
    tick(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.x, 0x0000);
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0); // Z set
}

#[test]
fn test_inx_only_affects_z() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.x = 0xFFFF;
    cpu.cc = CcFlag::C as u8 | CcFlag::N as u8; // pre-set other flags
    bus.load(0, &[0x08]); // INX
    tick(&mut cpu, &mut bus, 4);
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0); // C preserved
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0); // N preserved
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0); // Z set
}

#[test]
fn test_dex() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.x = 0x1000;
    bus.load(0, &[0x09]); // DEX
    tick(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.x, 0x0FFF);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);
}

#[test]
fn test_dex_zero() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.x = 0x0001;
    bus.load(0, &[0x09]); // DEX
    tick(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.x, 0x0000);
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0); // Z set
}

#[test]
fn test_dex_wrap() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.x = 0x0000;
    bus.load(0, &[0x09]); // DEX
    tick(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.x, 0xFFFF);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0); // not zero
}

// =============================================================================
// INS (0x31) / DES (0x34) - Increment/Decrement SP (4 cycles)
// =============================================================================

#[test]
fn test_ins() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.sp = 0x00FF;
    bus.load(0, &[0x31]); // INS
    tick(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.sp, 0x0100);
}

#[test]
fn test_des() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.sp = 0x0100;
    bus.load(0, &[0x34]); // DES
    tick(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.sp, 0x00FF);
}

#[test]
fn test_ins_no_flags() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.sp = 0xFFFF;
    cpu.cc = 0;
    bus.load(0, &[0x31]); // INS (wraps to 0x0000)
    tick(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.sp, 0x0000);
    assert_eq!(cpu.cc, 0); // no flags affected
}

// =============================================================================
// TSX (0x30) / TXS (0x35) - Transfer SP<->X (4 cycles)
// =============================================================================

#[test]
fn test_tsx() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.sp = 0x00FF;
    bus.load(0, &[0x30]); // TSX: X = SP + 1
    tick(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.x, 0x0100);
}

#[test]
fn test_txs() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.x = 0x0100;
    bus.load(0, &[0x35]); // TXS: SP = X - 1
    tick(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.sp, 0x00FF);
}

#[test]
fn test_tsx_txs_roundtrip() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.sp = 0x01FF;
    bus.load(0, &[0x30, 0x35]); // TSX, TXS
    tick(&mut cpu, &mut bus, 4); // TSX: X = 0x01FF + 1 = 0x0200
    assert_eq!(cpu.x, 0x0200);
    tick(&mut cpu, &mut bus, 4); // TXS: SP = 0x0200 - 1 = 0x01FF
    assert_eq!(cpu.sp, 0x01FF); // back to original
}

#[test]
fn test_tsx_no_flags() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.sp = 0x1000;
    cpu.cc = CcFlag::N as u8;
    bus.load(0, &[0x30]); // TSX
    tick(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.cc, CcFlag::N as u8); // flags unchanged
}

// =============================================================================
// Cycle count verification
// =============================================================================

#[test]
fn test_2_cycle_inherent_pc_advance() {
    // All 2-cycle inherent ops should advance PC by 1
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x42;
    bus.load(0, &[0x4F, 0x4C, 0x4A, 0x4D]); // CLRA, INCA, DECA, TSTA
    tick(&mut cpu, &mut bus, 2); // CLRA
    assert_eq!(cpu.pc, 1);
    tick(&mut cpu, &mut bus, 2); // INCA
    assert_eq!(cpu.pc, 2);
    tick(&mut cpu, &mut bus, 2); // DECA
    assert_eq!(cpu.pc, 3);
    tick(&mut cpu, &mut bus, 2); // TSTA
    assert_eq!(cpu.pc, 4);
}

#[test]
fn test_4_cycle_inherent_pc_advance() {
    // 4-cycle inherent ops should advance PC by 1
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.x = 0x1000;
    bus.load(0, &[0x08, 0x09]); // INX, DEX
    tick(&mut cpu, &mut bus, 4); // INX
    assert_eq!(cpu.pc, 1);
    assert_eq!(cpu.x, 0x1001);
    tick(&mut cpu, &mut bus, 4); // DEX
    assert_eq!(cpu.pc, 2);
    assert_eq!(cpu.x, 0x1000);
}

// =============================================================================
// Multi-instruction sequences
// =============================================================================

#[test]
fn test_clra_inca_sequence() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x4F, 0x4C, 0x4C, 0x4C]); // CLRA, INCA, INCA, INCA
    tick(&mut cpu, &mut bus, 2); // CLRA
    assert_eq!(cpu.a, 0);
    tick(&mut cpu, &mut bus, 2); // INCA
    assert_eq!(cpu.a, 1);
    tick(&mut cpu, &mut bus, 2); // INCA
    assert_eq!(cpu.a, 2);
    tick(&mut cpu, &mut bus, 2); // INCA
    assert_eq!(cpu.a, 3);
}

#[test]
fn test_tab_negb_tba() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.a = 0x05;
    bus.load(0, &[0x16, 0x50, 0x17]); // TAB, NEGB, TBA
    tick(&mut cpu, &mut bus, 2); // TAB: B = 5
    assert_eq!(cpu.b, 0x05);
    tick(&mut cpu, &mut bus, 2); // NEGB: B = -5 = 0xFB
    assert_eq!(cpu.b, 0xFB);
    tick(&mut cpu, &mut bus, 2); // TBA: A = 0xFB
    assert_eq!(cpu.a, 0xFB);
}

#[test]
fn test_sec_clc_toggle() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x0D, 0x0C, 0x0D]); // SEC, CLC, SEC
    tick(&mut cpu, &mut bus, 2); // SEC
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0);
    tick(&mut cpu, &mut bus, 2); // CLC
    assert_eq!(cpu.cc & (CcFlag::C as u8), 0);
    tick(&mut cpu, &mut bus, 2); // SEC
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0);
}
