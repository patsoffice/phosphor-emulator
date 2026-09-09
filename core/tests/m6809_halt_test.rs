use phosphor_core::core::{Bus, BusMaster, BusMasterComponent, bus::InterruptState};
use phosphor_core::cpu::CpuControl;
use phosphor_core::cpu::m6809::M6809;

/// Test bus with controllable halt line (simulates TSC/DMA halting).
struct HaltBus {
    memory: [u8; 0x10000],
    halted: bool,
}

impl HaltBus {
    fn new() -> Self {
        Self {
            memory: [0; 0x10000],
            halted: false,
        }
    }

    fn load(&mut self, addr: u16, data: &[u8]) {
        let start = addr as usize;
        self.memory[start..start + data.len()].copy_from_slice(data);
    }
}

impl Bus for HaltBus {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        self.memory[addr as usize]
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        self.memory[addr as usize] = data;
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        self.halted
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState::default()
    }
}

fn tick(cpu: &mut M6809, bus: &mut HaltBus, n: usize) {
    for _ in 0..n {
        cpu.tick_with_bus(bus, BusMaster::Cpu(0));
    }
}

#[test]
fn test_halt_and_resume_continues_execution() {
    let mut cpu = M6809::new();
    let mut bus = HaltBus::new();

    cpu.pc = 0x0000;
    cpu.s = 0x0100;
    cpu.a = 0x00;

    // LDA #$42 (0x86, 0x42) then NOP (0x12)
    bus.load(0x0000, &[0x86, 0x42, 0x12]);

    // Execute LDA #$42: 1 fetch + 1 execute = 2 cycles
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x42, "LDA #$42 should load A");
    assert_eq!(cpu.pc, 0x0002);

    // Assert halt
    bus.halted = true;

    // Tick several times while halted — CPU should not advance
    tick(&mut cpu, &mut bus, 5);
    assert_eq!(cpu.pc, 0x0002, "PC should not advance while halted");
    assert!(cpu.is_sleeping(), "CPU should report sleeping while halted");

    // Release halt
    bus.halted = false;

    // One dead cycle for re-sync (CPU restores state but doesn't execute)
    tick(&mut cpu, &mut bus, 1);

    // Now CPU should resume: fetch NOP at 0x0002
    // NOP = 1 fetch + 1 execute = 2 cycles
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.pc, 0x0003, "PC should advance past NOP after resume");
}

#[test]
fn test_halt_during_multi_cycle_instruction() {
    let mut cpu = M6809::new();
    let mut bus = HaltBus::new();

    cpu.pc = 0x0000;
    cpu.s = 0x0100;
    cpu.a = 0x00;

    // LDA $1000 (extended addressing: 0xB6, 0x10, 0x00) = 5 cycles
    // Then NOP (0x12)
    bus.load(0x0000, &[0xB6, 0x10, 0x00, 0x12]);
    bus.memory[0x1000] = 0xAB;

    // Execute 1 cycle (fetch opcode 0xB6)
    tick(&mut cpu, &mut bus, 1);

    // Assert halt mid-instruction
    bus.halted = true;
    tick(&mut cpu, &mut bus, 5);
    assert!(cpu.is_sleeping());

    // Release halt
    bus.halted = false;

    // One dead cycle for re-sync
    tick(&mut cpu, &mut bus, 1);

    // Remaining 4 cycles of LDA extended (read addr_hi, addr_lo, internal, read data)
    tick(&mut cpu, &mut bus, 4);

    assert_eq!(cpu.a, 0xAB, "LDA extended should complete after resume");
    assert_eq!(cpu.pc, 0x0003);
}

#[test]
fn test_halt_at_fetch_boundary() {
    let mut cpu = M6809::new();
    let mut bus = HaltBus::new();

    cpu.pc = 0x0000;
    cpu.s = 0x0100;
    cpu.a = 0x00;

    // LDA #$55 (0x86, 0x55) then LDA #$AA (0x86, 0xAA)
    bus.load(0x0000, &[0x86, 0x55, 0x86, 0xAA]);

    // Execute first LDA: 2 cycles
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x55);

    // Halt at the fetch boundary (before second instruction)
    bus.halted = true;
    tick(&mut cpu, &mut bus, 3);

    // Release
    bus.halted = false;

    // One dead cycle + execute second LDA (2 cycles)
    tick(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.a, 0xAA, "Second LDA should execute after halt release");
}

#[test]
fn test_multiple_halt_resume_cycles() {
    let mut cpu = M6809::new();
    let mut bus = HaltBus::new();

    cpu.pc = 0x0000;
    cpu.s = 0x0100;
    cpu.a = 0x00;

    // INCA (0x4C) x3 then NOP (0x12)
    bus.load(0x0000, &[0x4C, 0x4C, 0x4C, 0x12]);

    // Execute first INCA: 1 fetch + 1 execute = 2 cycles
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x01);

    // Halt and resume
    bus.halted = true;
    tick(&mut cpu, &mut bus, 3);
    bus.halted = false;
    tick(&mut cpu, &mut bus, 1); // dead cycle

    // Execute second INCA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x02);

    // Halt and resume again
    bus.halted = true;
    tick(&mut cpu, &mut bus, 2);
    bus.halted = false;
    tick(&mut cpu, &mut bus, 1); // dead cycle

    // Execute third INCA
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.a, 0x03, "Three INCAs should give A=3");
}
