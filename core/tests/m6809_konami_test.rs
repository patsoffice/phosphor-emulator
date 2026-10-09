use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::m6809::{CcFlag, M6809};
mod common;
use common::TestBus;

fn tick(cpu: &mut M6809, bus: &mut TestBus, n: usize) {
    for _ in 0..n {
        cpu.tick_with_bus(bus, BusMaster::Cpu(0));
    }
}

// Konami-1 opcode cipher, byte values transcribed from MAME
// `konami1::read_opcode` (Olivier Galibert): the fetch XOR is selected by
// address bits 1 and 3, mapping (addr & 0xA) 0x0/0x2/0x8/0xA to
// 0x22/0x82/0x28/0x88. Data reads are never decoded.

// Twelve encrypted NOPs (0x12) covering all four cipher cases twice, plus
// both parities of the ignored address bits 0 and 2:
//
// addr   addr&0xA  xor   byte
// 0x100    0x0     0x22  0x30
// 0x101    0x0     0x22  0x30
// 0x102    0x2     0x82  0x90
// 0x103    0x2     0x82  0x90
// 0x104    0x0     0x22  0x30
// 0x105    0x0     0x22  0x30
// 0x106    0x2     0x82  0x90
// 0x107    0x2     0x82  0x90
// 0x108    0x8     0x28  0x3A
// 0x109    0x8     0x28  0x3A
// 0x10A    0xA     0x88  0x9A
// 0x10B    0xA     0x88  0x9A
#[test]
fn test_konami_nop_all_cipher_cases() {
    let mut cpu = M6809::new();
    cpu.set_konami_decryption(true);
    let mut bus = TestBus::new();
    bus.load(
        0x100,
        &[
            0x30, 0x30, 0x90, 0x90, 0x30, 0x30, 0x90, 0x90, 0x3A, 0x3A, 0x9A, 0x9A,
        ],
    );
    cpu.pc = 0x100;

    tick(&mut cpu, &mut bus, 24); // 12 NOPs x 2 cycles

    assert_eq!(cpu.pc, 0x10C);
}

// The operand byte is a data read: stored plain, never decoded. LDA #$42
// with a plain 0x42 must load 0x42, not 0x42 ^ 0x22.
#[test]
fn test_konami_operand_passes_through() {
    let mut cpu = M6809::new();
    cpu.set_konami_decryption(true);
    let mut bus = TestBus::new();
    // 0x86 at 0x200 (xor 0x22) -> 0xA4; operand stays 0x42.
    bus.load(0x200, &[0xA4, 0x42]);
    cpu.pc = 0x200;

    tick(&mut cpu, &mut bus, 2);

    assert_eq!(cpu.a, 0x42);
    assert_eq!(cpu.pc, 0x202);
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);
}

// Decryption is off unless enabled: a plain NOP runs as a NOP.
#[test]
fn test_konami_disabled_by_default() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x12]);

    tick(&mut cpu, &mut bus, 2);

    assert_eq!(cpu.pc, 1);
}

// The page-2 second byte is an opcode byte too: LDD #$1234 then
// CMPD #$1234 (0x10 0x83), both opcode bytes encrypted, operands plain.
#[test]
fn test_konami_page2_prefix_decrypted() {
    let mut cpu = M6809::new();
    cpu.set_konami_decryption(true);
    let mut bus = TestBus::new();
    // 0xCC at 0x300 (xor 0x22) -> 0xEE; 0x10 at 0x303 (xor 0x82) -> 0x92;
    // 0x83 at 0x304 (xor 0x22) -> 0xA1.
    bus.load(0x300, &[0xEE, 0x12, 0x34, 0x92, 0xA1, 0x12, 0x34]);
    cpu.pc = 0x300;

    tick(&mut cpu, &mut bus, 3); // LDD #$1234
    assert_eq!(cpu.pc, 0x303);
    tick(&mut cpu, &mut bus, 5); // CMPD #$1234 (2 prefix + 3 execute)

    assert_eq!(cpu.pc, 0x307);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), CcFlag::Z as u8);
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0);
}
