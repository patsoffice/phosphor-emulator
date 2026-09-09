use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::CpuControl;
use phosphor_core::cpu::z80::Z80;
mod common;
use common::TestBus;

fn run_instruction(cpu: &mut Z80, bus: &mut TestBus) -> u32 {
    let mut cycles = 0;
    loop {
        let done = cpu.tick_with_bus(bus, BusMaster::Cpu(0));
        cycles += 1;
        if done {
            return cycles;
        }
    }
}

/// Run a single T-state (tick)
fn tick(cpu: &mut Z80, bus: &mut TestBus) -> bool {
    cpu.tick_with_bus(bus, BusMaster::Cpu(0))
}

// ============================================================
// NMI — Non-Maskable Interrupt
// ============================================================

#[test]
fn test_nmi_basic() {
    let mut cpu = Z80::new();
    let mut bus = TestBus::new();
    cpu.pc = 0x0100;
    cpu.sp = 0x1000;
    cpu.iff1 = true;
    cpu.iff2 = true;

    // Load a NOP at 0x0100
    bus.load(0x0100, &[0x00]); // NOP
    // Load a NOP at the NMI vector
    bus.load(0x0066, &[0x00]); // NOP

    // Execute the NOP first
    run_instruction(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x0101);

    // Now trigger NMI (edge-triggered)
    bus.nmi = true;

    // Execute the NMI response
    let cycles = run_instruction(&mut cpu, &mut bus);
    assert_eq!(cycles, 11, "NMI response should be 11 T-states");
    assert_eq!(cpu.pc, 0x0066, "PC should jump to NMI vector");
    assert_eq!(cpu.sp, 0x0FFE, "SP should be decremented by 2");
    // Check pushed return address
    assert_eq!(bus.memory[0x0FFF], 0x01, "Return address high byte");
    assert_eq!(bus.memory[0x0FFE], 0x01, "Return address low byte");
    assert!(!cpu.iff1, "IFF1 should be cleared");
    assert!(cpu.iff2, "IFF2 should be preserved");
}

#[test]
fn test_nmi_edge_triggered() {
    let mut cpu = Z80::new();
    let mut bus = TestBus::new();
    cpu.pc = 0x0100;
    cpu.sp = 0x1000;

    // Load NOPs
    bus.load(0x0100, &[0x00, 0x00, 0x00]);
    bus.load(0x0066, &[0x00]);

    // Set NMI high BEFORE first instruction (no edge yet)
    bus.nmi = true;
    run_instruction(&mut cpu, &mut bus); // NOP at 0x0100

    // NMI should have been taken (rising edge detected during first fetch)
    assert_eq!(cpu.pc, 0x0066, "NMI should be taken on first rising edge");

    // Reset to check that NMI doesn't re-trigger while held high
    cpu.pc = 0x0100;
    cpu.sp = 0x1000;
    // NMI is still high — no new edge
    run_instruction(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x0101, "NMI should not re-trigger without edge");

    // Release and re-assert for a new edge
    bus.nmi = false;
    run_instruction(&mut cpu, &mut bus); // NOP
    bus.nmi = true;
    run_instruction(&mut cpu, &mut bus); // Should take NMI
    assert_eq!(cpu.pc, 0x0066, "NMI should trigger on new rising edge");
}

#[test]
fn test_nmi_preserved_iff2() {
    // IFF2 should be preserved across NMI so RETN can restore IFF1
    let mut cpu = Z80::new();
    let mut bus = TestBus::new();
    cpu.pc = 0x0100;
    cpu.sp = 0x1000;
    cpu.iff1 = true;
    cpu.iff2 = true;

    bus.load(0x0100, &[0x00]); // NOP
    bus.load(0x0066, &[0xED, 0x45]); // RETN at NMI handler

    // Execute NOP first so PC advances to 0x0101
    run_instruction(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x0101);

    // Trigger NMI (edge-triggered)
    bus.nmi = true;
    run_instruction(&mut cpu, &mut bus); // NMI response
    assert!(!cpu.iff1, "IFF1 should be cleared by NMI");
    assert!(cpu.iff2, "IFF2 should be preserved");

    // Execute RETN — should restore IFF1 from IFF2
    bus.nmi = false; // Clear NMI to avoid re-trigger
    run_instruction(&mut cpu, &mut bus); // RETN
    assert!(cpu.iff1, "IFF1 should be restored from IFF2 by RETN");
    assert_eq!(cpu.pc, 0x0101, "Should return to address after NMI trigger");
}

// ============================================================
// IRQ — Maskable Interrupt (IM1)
// ============================================================

#[test]
fn test_irq_im1_basic() {
    let mut cpu = Z80::new();
    let mut bus = TestBus::new();
    cpu.pc = 0x0200;
    cpu.sp = 0x1000;
    cpu.iff1 = true;
    cpu.iff2 = true;
    cpu.im = 1;

    bus.load(0x0200, &[0x00]); // NOP
    bus.load(0x0038, &[0x00]); // NOP at IM1 vector

    // Assert IRQ
    bus.irq = true;

    let cycles = run_instruction(&mut cpu, &mut bus);
    assert_eq!(cycles, 13, "IRQ IM1 response should be 13 T-states");
    assert_eq!(cpu.pc, 0x0038, "PC should jump to IM1 vector (0x0038)");
    assert_eq!(cpu.sp, 0x0FFE, "SP should be decremented by 2");
    assert_eq!(bus.memory[0x0FFF], 0x02, "Return address high byte");
    assert_eq!(bus.memory[0x0FFE], 0x00, "Return address low byte");
    assert!(!cpu.iff1, "IFF1 should be cleared");
    assert!(!cpu.iff2, "IFF2 should be cleared");
}

#[test]
fn test_irq_masked() {
    let mut cpu = Z80::new();
    let mut bus = TestBus::new();
    cpu.pc = 0x0200;
    cpu.sp = 0x1000;
    cpu.iff1 = false; // Interrupts disabled
    cpu.im = 1;

    bus.load(0x0200, &[0x00, 0x00]);
    bus.irq = true;

    // IRQ should be ignored since IFF1 is false
    run_instruction(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x0201, "IRQ should be masked");
}

#[test]
fn test_irq_im0_acts_like_im1() {
    // IM0 with no data bus device behaves like RST 38h (same as IM1)
    let mut cpu = Z80::new();
    let mut bus = TestBus::new();
    cpu.pc = 0x0200;
    cpu.sp = 0x1000;
    cpu.iff1 = true;
    cpu.im = 0;

    bus.load(0x0200, &[0x00]);
    bus.load(0x0038, &[0x00]);
    bus.irq = true;

    run_instruction(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x0038, "IM0 should jump to 0x0038 (RST 38h)");
}

// ============================================================
// IRQ — IM2 (Vectored)
// ============================================================

#[test]
fn test_irq_im2() {
    let mut cpu = Z80::new();
    let mut bus = TestBus::new();
    cpu.pc = 0x0200;
    cpu.sp = 0x1000;
    cpu.iff1 = true;
    cpu.iff2 = true;
    cpu.im = 2;
    cpu.i = 0x80; // I register = 0x80

    // Vector table at 0x80FF: address 0x1234
    bus.memory[0x80FF] = 0x34; // Low byte
    bus.memory[0x8100] = 0x12; // High byte

    bus.load(0x0200, &[0x00]);
    bus.load(0x1234, &[0x00]); // ISR at 0x1234

    bus.irq = true;

    let cycles = run_instruction(&mut cpu, &mut bus);
    assert_eq!(cycles, 19, "IRQ IM2 response should be 19 T-states");
    assert_eq!(cpu.pc, 0x1234, "PC should jump to vector table entry");
    assert_eq!(cpu.sp, 0x0FFE);
    assert!(!cpu.iff1);
    assert!(!cpu.iff2);
}

// ============================================================
// EI delay — interrupts deferred for one instruction after EI
// ============================================================

#[test]
fn test_ei_delay() {
    let mut cpu = Z80::new();
    let mut bus = TestBus::new();
    cpu.pc = 0x0100;
    cpu.sp = 0x1000;
    cpu.iff1 = false;
    cpu.iff2 = false;
    cpu.im = 1;

    // EI followed by NOP — IRQ should not be taken until after NOP
    bus.load(0x0100, &[0xFB, 0x00, 0x00]); // EI, NOP, NOP
    bus.load(0x0038, &[0x00]);

    // Assert IRQ before executing EI
    bus.irq = true;

    // Execute EI — sets IFF1/IFF2, ei_delay
    run_instruction(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x0101);
    assert!(cpu.iff1, "IFF1 should be set by EI");

    // Execute NOP — IRQ should not be taken yet (EI delay)
    run_instruction(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x0102, "NOP should execute normally (EI delay)");

    // Now IRQ should be taken
    run_instruction(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x0038, "IRQ should be taken after EI delay expires");
}

#[test]
fn test_di_prevents_irq() {
    let mut cpu = Z80::new();
    let mut bus = TestBus::new();
    cpu.pc = 0x0100;
    cpu.sp = 0x1000;
    cpu.iff1 = true;
    cpu.im = 1;

    // DI followed by NOP
    bus.load(0x0100, &[0xF3, 0x00]); // DI, NOP
    bus.load(0x0038, &[0x00]);

    // Execute DI first (without IRQ asserted) — clears IFF1/IFF2
    run_instruction(&mut cpu, &mut bus);
    assert!(!cpu.iff1);

    // Now assert IRQ — should be masked since IFF1 is false
    bus.irq = true;

    // NOP should execute without interruption
    run_instruction(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x0102, "IRQ should be masked after DI");
}

// ============================================================
// HALT — wake up on interrupt
// ============================================================

#[test]
fn test_halt_basic() {
    let mut cpu = Z80::new();
    let mut bus = TestBus::new();
    cpu.pc = 0x0100;

    bus.load(0x0100, &[0x76]); // HALT

    run_instruction(&mut cpu, &mut bus);
    assert!(cpu.halted, "CPU should be halted");
    assert!(cpu.is_sleeping(), "is_sleeping should return true");
    // PC points past HALT instruction (standard Z80 behavior)
    assert_eq!(cpu.pc, 0x0101);
}

#[test]
fn test_halt_executes_nops() {
    let mut cpu = Z80::new();
    let mut bus = TestBus::new();
    cpu.pc = 0x0100;

    bus.load(0x0100, &[0x76]); // HALT

    // Execute HALT
    run_instruction(&mut cpu, &mut bus);
    assert!(cpu.halted);

    // Execute more cycles — should keep executing 4T NOPs at HALT address
    let cycles = run_instruction(&mut cpu, &mut bus);
    assert_eq!(cycles, 4, "HALT should re-execute as 4T NOP");
    assert!(cpu.halted, "Should still be halted");
}

#[test]
fn test_halt_wake_on_nmi() {
    let mut cpu = Z80::new();
    let mut bus = TestBus::new();
    cpu.pc = 0x0100;
    cpu.sp = 0x1000;

    bus.load(0x0100, &[0x76]); // HALT
    bus.load(0x0066, &[0x00]); // NOP at NMI vector

    // Execute HALT
    run_instruction(&mut cpu, &mut bus);
    assert!(cpu.halted);

    // Trigger NMI to wake up
    bus.nmi = true;
    run_instruction(&mut cpu, &mut bus); // NMI response
    assert!(!cpu.halted, "CPU should be woken from HALT");
    assert_eq!(cpu.pc, 0x0066, "Should jump to NMI vector");
    // Return address should be HALT + 1 (past the HALT instruction)
    assert_eq!(bus.memory[0x0FFF], 0x01, "Return addr high");
    assert_eq!(bus.memory[0x0FFE], 0x01, "Return addr low = 0x0101");
}

#[test]
fn test_halt_wake_on_irq() {
    let mut cpu = Z80::new();
    let mut bus = TestBus::new();
    cpu.pc = 0x0100;
    cpu.sp = 0x1000;
    cpu.iff1 = true;
    cpu.im = 1;

    bus.load(0x0100, &[0x76]); // HALT
    bus.load(0x0038, &[0x00]); // NOP at IM1 vector

    // Execute HALT
    run_instruction(&mut cpu, &mut bus);
    assert!(cpu.halted);

    // Trigger IRQ
    bus.irq = true;
    run_instruction(&mut cpu, &mut bus); // IRQ response
    assert!(!cpu.halted);
    assert_eq!(cpu.pc, 0x0038);
    // Return address should be 0x0101 (past HALT)
    assert_eq!(bus.memory[0x0FFF], 0x01);
    assert_eq!(bus.memory[0x0FFE], 0x01);
}

// ============================================================
// NMI has higher priority than IRQ
// ============================================================

#[test]
fn test_nmi_priority_over_irq() {
    let mut cpu = Z80::new();
    let mut bus = TestBus::new();
    cpu.pc = 0x0100;
    cpu.sp = 0x1000;
    cpu.iff1 = true;
    cpu.im = 1;

    bus.load(0x0100, &[0x00]);
    bus.load(0x0038, &[0x00]);
    bus.load(0x0066, &[0x00]);

    // Assert both NMI and IRQ simultaneously
    bus.nmi = true;
    bus.irq = true;

    run_instruction(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x0066, "NMI should take priority over IRQ");
}

// ============================================================
// Interrupt during prefix chain
// ============================================================

#[test]
fn test_no_interrupt_during_prefix() {
    // Interrupts should not be accepted between DD and the following instruction
    let mut cpu = Z80::new();
    let mut bus = TestBus::new();
    cpu.pc = 0x0100;
    cpu.sp = 0x1000;
    cpu.iff1 = true;
    cpu.im = 1;
    cpu.ix = 0x0000;

    // DD 21 34 12 → LD IX, 0x1234
    bus.load(0x0100, &[0xDD, 0x21, 0x34, 0x12]);
    bus.load(0x0038, &[0x00]);

    // Execute DD prefix (4 T-states) — sets prefix_pending=true
    for _ in 0..4 {
        tick(&mut cpu, &mut bus);
    }

    // Assert IRQ while prefix_pending is true
    bus.irq = true;

    // Continue executing — Fetch should skip interrupt check due to prefix_pending
    // Complete the LD IX, 0x1234 instruction
    loop {
        if tick(&mut cpu, &mut bus) {
            break;
        }
    }

    assert_eq!(cpu.ix, 0x1234, "LD IX,nn should complete despite IRQ");
    // IRQ should be taken on the next instruction, not between DD and 21
}
