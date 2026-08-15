use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::m6809::M6809;
mod common;
use common::TestBus;

fn tick(cpu: &mut M6809, bus: &mut TestBus, n: usize) {
    for _ in 0..n {
        cpu.tick_with_bus(bus, BusMaster::Cpu(0));
    }
}

#[test]
fn test_tfr_8bit() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // LDA #$42, TFR A,B
    // TFR op: 1F, operand: 89 (A=8, B=9)
    bus.load(0, &[0x86, 0x42, 0x1F, 0x89]);

    tick(&mut cpu, &mut bus, 2); // LDA #$42
    assert_eq!(cpu.a, 0x42);
    assert_eq!(cpu.b, 0x00);

    tick(&mut cpu, &mut bus, 6); // TFR A,B (6 cycles)
    assert_eq!(cpu.b, 0x42);
    assert_eq!(cpu.a, 0x42); // Source unchanged
}

#[test]
fn test_tfr_16bit() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // LDX #$1234, TFR X,Y
    // TFR op: 1F, operand: 12 (X=1, Y=2)
    bus.load(0, &[0x8E, 0x12, 0x34, 0x1F, 0x12]);

    tick(&mut cpu, &mut bus, 3); // LDX #$1234
    assert_eq!(cpu.x, 0x1234);

    tick(&mut cpu, &mut bus, 6); // TFR X,Y (6 cycles)
    assert_eq!(cpu.y, 0x1234);
}

#[test]
fn test_exg_8bit() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // LDA #$AA, LDB #$55, EXG A,B
    // EXG op: 1E, operand: 89
    bus.load(0, &[0x86, 0xAA, 0xC6, 0x55, 0x1E, 0x89]);

    tick(&mut cpu, &mut bus, 2); // LDA #$AA
    tick(&mut cpu, &mut bus, 2); // LDB #$55
    assert_eq!(cpu.a, 0xAA);
    assert_eq!(cpu.b, 0x55);

    tick(&mut cpu, &mut bus, 8); // EXG A,B (8 cycles)
    assert_eq!(cpu.a, 0x55);
    assert_eq!(cpu.b, 0xAA);
}

/// TFR from an 8-bit register into a 16-bit one fills the high byte with $FF:
/// the 8-bit register only drives the low half of the transfer path and the
/// rest reads back as ones. Motorola documents this combination as undefined,
/// but the part transfers rather than ignoring the instruction.
#[test]
fn test_tfr_8bit_to_16bit_fills_high_byte_with_ff() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // LDX #$1234 ; LDA #$42 ; TFR A,X   (postbyte $81 = A -> X)
    bus.load(0, &[0x8E, 0x12, 0x34, 0x86, 0x42, 0x1F, 0x81]);

    tick(&mut cpu, &mut bus, 3); // LDX
    tick(&mut cpu, &mut bus, 2); // LDA
    assert_eq!(cpu.x, 0x1234);

    tick(&mut cpu, &mut bus, 6); // TFR A,X
    assert_eq!(cpu.x, 0xFF42, "A in the low byte, $FF in the high byte");
    assert_eq!(cpu.a, 0x42, "source unchanged");
}

/// TFR from a 16-bit register into an 8-bit one keeps only the low byte —
/// the destination cannot latch the high half of the transfer path.
#[test]
fn test_tfr_16bit_to_8bit_keeps_low_byte() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // LDX #$1234 ; TFR X,A   (postbyte $18 = X -> A)
    bus.load(0, &[0x8E, 0x12, 0x34, 0x1F, 0x18]);

    tick(&mut cpu, &mut bus, 3); // LDX
    tick(&mut cpu, &mut bus, 6); // TFR X,A
    assert_eq!(cpu.a, 0x34, "low byte of X");
    assert_eq!(cpu.x, 0x1234, "source unchanged");
}

/// EXG applies the same rule in both directions at once: the 8-bit register
/// receives the low byte of the 16-bit one, and the 16-bit register receives
/// the 8-bit value with $FF above it.
#[test]
fn test_exg_mixed_sizes() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // LDX #$1234 ; LDA #$42 ; EXG A,X   (postbyte $81)
    bus.load(0, &[0x8E, 0x12, 0x34, 0x86, 0x42, 0x1E, 0x81]);

    tick(&mut cpu, &mut bus, 3); // LDX
    tick(&mut cpu, &mut bus, 2); // LDA
    tick(&mut cpu, &mut bus, 8); // EXG A,X

    assert_eq!(cpu.a, 0x34, "A gets the low byte of X");
    assert_eq!(cpu.x, 0xFF42, "X gets $FF:A");
}

/// A register code with nothing behind it (6, 7, 12-15) drives no bits, so it
/// reads as all ones — `TFR <7>,Y` is an `LDY #$FFFF` in two bytes. As a
/// destination it latches nothing.
#[test]
fn test_tfr_register_code_with_no_register() {
    // Source: code 7 -> Y
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x8E, 0x12, 0x34, 0x1F, 0x72]); // LDX #$1234 ; TFR <7>,Y
    tick(&mut cpu, &mut bus, 3);
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(cpu.y, 0xFFFF, "an absent source reads as all ones");

    // Destination: X -> code 7, discarded, and X itself untouched
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x8E, 0x12, 0x34, 0x1F, 0x17]); // LDX #$1234 ; TFR X,<7>
    tick(&mut cpu, &mut bus, 3);
    tick(&mut cpu, &mut bus, 6);
    assert_eq!(cpu.x, 0x1234, "source unchanged, result discarded");
}
