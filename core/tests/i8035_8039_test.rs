use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::i8035::I8035;
mod common;
use common::TestBus;

/// Helper: tick the CPU for `n` machine cycles.
fn tick(cpu: &mut I8035, bus: &mut TestBus, n: usize) {
    for _ in 0..n {
        cpu.tick_with_bus(bus, BusMaster::Cpu(0));
    }
}

// Program used by both tests: stash 0x5A at [R0] and read it back.
//   MOV A,#0x5A   (0x23 0x5A, 2 cycles)
//   MOV R0,#0x50  (0xB8 0x50, 2 cycles)
//   MOV @R0,A     (0xA0, 1 cycle)
//   CLR A         (0x27, 1 cycle)
//   MOV A,@R0     (0xF0, 1 cycle)
const PROG: &[u8] = &[0x23, 0x5A, 0xB8, 0x50, 0xA0, 0x27, 0xF0];
const PROG_TICKS: usize = 7;

#[test]
fn test_8039_constructor_has_128_bytes() {
    assert_eq!(I8035::new().ram_mask, 0x3F);
    assert_eq!(I8035::new_8039().ram_mask, 0x7F);
}

// The 8039 addresses all 128 RAM bytes: 0x50 lands at ram[0x50].
#[test]
fn test_8039_high_ram_round_trip() {
    let mut cpu = I8035::new_8039();
    let mut bus = TestBus::new();
    bus.load(0, PROG);

    tick(&mut cpu, &mut bus, PROG_TICKS);

    assert_eq!(cpu.a, 0x5A);
    assert_eq!(cpu.ram[0x50], 0x5A);
}

// The 8035 masks to 64 bytes: the same program lands at ram[0x10]
// (0x50 & 0x3F) and leaves ram[0x50] alone.
#[test]
fn test_8035_ram_wraps_at_64() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    bus.load(0, PROG);

    tick(&mut cpu, &mut bus, PROG_TICKS);

    assert_eq!(cpu.a, 0x5A);
    assert_eq!(cpu.ram[0x10], 0x5A);
    assert_eq!(cpu.ram[0x50], 0x00);
}
