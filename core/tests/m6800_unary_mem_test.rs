/// Tests for M6800 memory unary operations (indexed and extended addressing).
///
/// RMW cycle counts: indexed = 7 cycles, extended = 6 cycles.
use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::m6800::{CcFlag, M6800};

mod common;
use common::TestBus;

fn tick(cpu: &mut M6800, bus: &mut TestBus, n: usize) {
    for _ in 0..n {
        cpu.tick_with_bus(bus, BusMaster::Cpu(0));
    }
}

// ---- NEG memory ----

#[test]
fn test_neg_idx() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.x = 0x0100;
    bus.memory[0x0105] = 0x01;
    bus.load(0, &[0x60, 0x05]); // NEG 5,X
    tick(&mut cpu, &mut bus, 7);
    assert_eq!(bus.memory[0x0105], 0xFF); // 0 - 1 = 0xFF
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0);
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0);
}

#[test]
fn test_neg_ext() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    bus.memory[0x2000] = 0x80;
    bus.load(0, &[0x70, 0x20, 0x00]); // NEG $2000
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x2000], 0x80); // 0 - 0x80 = 0x80 (overflow)
    assert_ne!(cpu.cc & (CcFlag::V as u8), 0);
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0);
}

#[test]
fn test_neg_ext_zero() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    bus.memory[0x2000] = 0x00;
    bus.load(0, &[0x70, 0x20, 0x00]); // NEG $2000
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x2000], 0x00);
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::C as u8), 0); // no borrow on zero
}

// ---- COM memory ----

#[test]
fn test_com_idx() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.x = 0x0100;
    bus.memory[0x0100] = 0x0F;
    bus.load(0, &[0x63, 0x00]); // COM 0,X
    tick(&mut cpu, &mut bus, 7);
    assert_eq!(bus.memory[0x0100], 0xF0);
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0); // V cleared
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0); // C always set
}

#[test]
fn test_com_ext() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    bus.memory[0x3000] = 0xFF;
    bus.load(0, &[0x73, 0x30, 0x00]); // COM $3000
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x3000], 0x00);
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0);
    assert_ne!(cpu.cc & (CcFlag::C as u8), 0); // C always set
}

// ---- INC memory ----

#[test]
fn test_inc_idx() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.x = 0x0200;
    bus.memory[0x0210] = 0x7F;
    bus.load(0, &[0x6C, 0x10]); // INC $10,X
    tick(&mut cpu, &mut bus, 7);
    assert_eq!(bus.memory[0x0210], 0x80);
    assert_ne!(cpu.cc & (CcFlag::V as u8), 0); // 0x7F→0x80 overflow
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0);
}

#[test]
fn test_inc_ext() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    bus.memory[0x4000] = 0xFF;
    bus.load(0, &[0x7C, 0x40, 0x00]); // INC $4000
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x4000], 0x00);
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0);
}

// ---- DEC memory ----

#[test]
fn test_dec_idx() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.x = 0x0100;
    bus.memory[0x0105] = 0x80;
    bus.load(0, &[0x6A, 0x05]); // DEC 5,X
    tick(&mut cpu, &mut bus, 7);
    assert_eq!(bus.memory[0x0105], 0x7F);
    assert_ne!(cpu.cc & (CcFlag::V as u8), 0); // 0x80→0x7F overflow
}

#[test]
fn test_dec_ext() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    bus.memory[0x5000] = 0x01;
    bus.load(0, &[0x7A, 0x50, 0x00]); // DEC $5000
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x5000], 0x00);
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0);
}

// ---- TST memory ----

#[test]
fn test_tst_idx_zero() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.x = 0x0100;
    bus.memory[0x0100] = 0x00;
    bus.load(0, &[0x6D, 0x00]); // TST 0,X
    tick(&mut cpu, &mut bus, 7);
    assert_eq!(bus.memory[0x0100], 0x00); // unchanged
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
}

#[test]
fn test_tst_ext_negative() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    bus.memory[0x1000] = 0x80;
    bus.load(0, &[0x7D, 0x10, 0x00]); // TST $1000
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x1000], 0x80); // unchanged
    assert_ne!(cpu.cc & (CcFlag::N as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);
}

// ---- CLR memory ----

#[test]
fn test_clr_idx() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.x = 0x0100;
    bus.memory[0x010A] = 0xFF;
    // Set some flags that CLR should clear
    cpu.cc |= CcFlag::N as u8 | CcFlag::V as u8 | CcFlag::C as u8;
    bus.load(0, &[0x6F, 0x0A]); // CLR $0A,X
    tick(&mut cpu, &mut bus, 7);
    assert_eq!(bus.memory[0x010A], 0x00);
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::C as u8), 0);
}

#[test]
fn test_clr_ext() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    bus.memory[0x6000] = 0xAB;
    bus.load(0, &[0x7F, 0x60, 0x00]); // CLR $6000
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x6000], 0x00);
    assert_ne!(cpu.cc & (CcFlag::Z as u8), 0);
}

// ---- Multi-instruction sequence ----

#[test]
fn test_inc_dec_roundtrip_idx() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.x = 0x0100;
    bus.memory[0x0100] = 0x42;
    // INC 0,X; DEC 0,X → should restore original value
    bus.load(0, &[0x6C, 0x00, 0x6A, 0x00]);
    tick(&mut cpu, &mut bus, 7); // INC 0,X
    assert_eq!(bus.memory[0x0100], 0x43);
    tick(&mut cpu, &mut bus, 7); // DEC 0,X
    assert_eq!(bus.memory[0x0100], 0x42);
    assert_eq!(cpu.pc, 4);
}

#[test]
fn test_neg_com_relationship_ext() {
    // NEG(x) = COM(x) + 1 for non-zero values
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    bus.memory[0x1000] = 0x01;
    bus.load(0, &[0x70, 0x10, 0x00]); // NEG $1000
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(bus.memory[0x1000], 0xFF); // NEG(1) = 0xFF

    // Reset and try COM on the same value
    let mut cpu2 = M6800::new();
    let mut bus2 = TestBus::new();
    bus2.memory[0x1000] = 0x01;
    bus2.load(0, &[0x73, 0x10, 0x00]); // COM $1000
    tick(&mut cpu2, &mut bus2, 6);
    assert_eq!(bus2.memory[0x1000], 0xFE); // COM(1) = 0xFE = NEG(1) - 1
}
