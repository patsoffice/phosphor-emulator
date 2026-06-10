/// Tests for M6800 stack and interrupt instructions.
///
/// Cycle counts:
/// - PSHA/PSHB: 4 cycles, PULA/PULB: 4 cycles
/// - SWI: 12 cycles (1 fetch + 2 internal + 9 interrupt)
/// - RTI: 10 cycles (1 fetch + 9 execute)
/// - WAI: 9 cycles (1 fetch + 8 execute) then waits
/// - NMI: 1 (fetch boundary) + 9 interrupt = 10 cycles (edge-triggered)
/// - IRQ: 1 (fetch boundary) + 9 interrupt = 10 cycles (level, masked by I)
use phosphor_core::core::{Bus, BusMaster, BusMasterComponent, bus::InterruptState};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::m6800::{CcFlag, M6800};

mod common;
use common::TestBus;

fn tick(cpu: &mut M6800, bus: &mut TestBus, n: usize) {
    for _ in 0..n {
        cpu.tick_with_bus(bus, BusMaster::Cpu(0));
    }
}

/// Bus with controllable interrupt lines for NMI/IRQ testing.
struct InterruptBus {
    memory: [u8; 0x10000],
    irq: bool,
    nmi: bool,
}

impl InterruptBus {
    fn new() -> Self {
        Self {
            memory: [0; 0x10000],
            irq: false,
            nmi: false,
        }
    }

    fn load(&mut self, addr: u16, data: &[u8]) {
        let start = addr as usize;
        self.memory[start..start + data.len()].copy_from_slice(data);
    }
}

impl Bus for InterruptBus {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        self.memory[addr as usize]
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        self.memory[addr as usize] = data;
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            nmi: self.nmi,
            irq: self.irq,
            firq: false,
            irq_vector: 0xFF,
            irq_level: 0,
        }
    }
}

fn tick_int(cpu: &mut M6800, bus: &mut InterruptBus, n: usize) {
    for _ in 0..n {
        cpu.tick_with_bus(bus, BusMaster::Cpu(0));
    }
}

// =============================================================================
// PSHA (0x36) - Push A - 4 cycles
// =============================================================================

#[test]
fn test_psha() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.sp = 0x00FF;
    cpu.a = 0x42;
    bus.load(0, &[0x36]); // PSHA
    tick(&mut cpu, &mut bus, 4);
    assert_eq!(bus.memory[0x00FF], 0x42);
    assert_eq!(cpu.sp, 0x00FE);
}

#[test]
fn test_psha_twice() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.sp = 0x00FF;
    cpu.a = 0x11;
    bus.load(0, &[0x36, 0x36]); // PSHA; PSHA
    tick(&mut cpu, &mut bus, 4); // first push
    cpu.a = 0x22;
    tick(&mut cpu, &mut bus, 4); // second push
    assert_eq!(bus.memory[0x00FF], 0x11);
    assert_eq!(bus.memory[0x00FE], 0x22);
    assert_eq!(cpu.sp, 0x00FD);
}

// =============================================================================
// PSHB (0x37) - Push B - 4 cycles
// =============================================================================

#[test]
fn test_pshb() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.sp = 0x00FF;
    cpu.b = 0xAB;
    bus.load(0, &[0x37]); // PSHB
    tick(&mut cpu, &mut bus, 4);
    assert_eq!(bus.memory[0x00FF], 0xAB);
    assert_eq!(cpu.sp, 0x00FE);
}

// =============================================================================
// PULA (0x32) - Pull A - 4 cycles
// =============================================================================

#[test]
fn test_pula() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.sp = 0x00FE; // one byte on stack
    bus.memory[0x00FF] = 0x77;
    bus.load(0, &[0x32]); // PULA
    tick(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.a, 0x77);
    assert_eq!(cpu.sp, 0x00FF);
}

// =============================================================================
// PULB (0x33) - Pull B - 4 cycles
// =============================================================================

#[test]
fn test_pulb() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.sp = 0x00FE;
    bus.memory[0x00FF] = 0x99;
    bus.load(0, &[0x33]); // PULB
    tick(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.b, 0x99);
    assert_eq!(cpu.sp, 0x00FF);
}

// =============================================================================
// PSHA/PULA roundtrip
// =============================================================================

#[test]
fn test_psha_pula_roundtrip() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.sp = 0x00FF;
    cpu.a = 0x55;
    bus.load(0, &[0x36, 0x32]); // PSHA; PULA
    tick(&mut cpu, &mut bus, 4); // PSHA
    assert_eq!(cpu.sp, 0x00FE);
    cpu.a = 0x00; // clobber A
    tick(&mut cpu, &mut bus, 4); // PULA
    assert_eq!(cpu.a, 0x55);
    assert_eq!(cpu.sp, 0x00FF);
}

#[test]
fn test_pshb_pulb_roundtrip() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.sp = 0x00FF;
    cpu.b = 0xCC;
    bus.load(0, &[0x37, 0x33]); // PSHB; PULB
    tick(&mut cpu, &mut bus, 4);
    cpu.b = 0x00;
    tick(&mut cpu, &mut bus, 4);
    assert_eq!(cpu.b, 0xCC);
    assert_eq!(cpu.sp, 0x00FF);
}

#[test]
fn test_push_both_pull_swapped() {
    // Push A then B, pull into B then A — swap A and B via stack
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.sp = 0x00FF;
    cpu.a = 0x11;
    cpu.b = 0x22;
    bus.load(0, &[0x36, 0x37, 0x32, 0x33]); // PSHA; PSHB; PULA; PULB
    tick(&mut cpu, &mut bus, 4); // PSHA (pushes 0x11)
    tick(&mut cpu, &mut bus, 4); // PSHB (pushes 0x22)
    tick(&mut cpu, &mut bus, 4); // PULA (pulls 0x22 into A)
    tick(&mut cpu, &mut bus, 4); // PULB (pulls 0x11 into B)
    assert_eq!(cpu.a, 0x22);
    assert_eq!(cpu.b, 0x11);
    assert_eq!(cpu.sp, 0x00FF);
}

// =============================================================================
// SWI (0x3F) - Software Interrupt - 12 cycles
// =============================================================================

#[test]
fn test_swi_pushes_all_and_jumps_to_vector() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.sp = 0x00FF;
    cpu.a = 0xAA;
    cpu.b = 0xBB;
    cpu.x = 0x1234;
    cpu.cc = 0x00; // I=0
    // SWI at address 0x0000
    bus.load(0, &[0x3F]);
    // SWI vector at 0xFFFA-0xFFFB = 0x2000
    bus.memory[0xFFFA] = 0x20;
    bus.memory[0xFFFB] = 0x00;

    tick(&mut cpu, &mut bus, 12);

    // PC should be at SWI vector
    assert_eq!(cpu.pc, 0x2000);
    // I flag should be set
    assert_ne!(cpu.cc & (CcFlag::I as u8), 0);
    // SP should have decremented by 7
    assert_eq!(cpu.sp, 0x00F8);
    // Stack should contain (top to bottom):
    // 0xFF: PCL (0x01 — after SWI opcode)
    // 0xFE: PCH (0x00)
    // 0xFD: XL (0x34)
    // 0xFC: XH (0x12)
    // 0xFB: A (0xAA)
    // 0xFA: B (0xBB)
    // 0xF9: CC (original, before I was set = 0x00)
    assert_eq!(bus.memory[0x00FF], 0x01); // PCL
    assert_eq!(bus.memory[0x00FE], 0x00); // PCH
    assert_eq!(bus.memory[0x00FD], 0x34); // XL
    assert_eq!(bus.memory[0x00FC], 0x12); // XH
    assert_eq!(bus.memory[0x00FB], 0xAA); // A
    assert_eq!(bus.memory[0x00FA], 0xBB); // B
    assert_eq!(bus.memory[0x00F9], 0x00); // CC (I was 0 before push)
}

// =============================================================================
// RTI (0x3B) - Return from Interrupt - 10 cycles
// =============================================================================

#[test]
fn test_rti_restores_all() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    // Simulate stack frame as SWI would leave it
    cpu.sp = 0x00F8;
    bus.memory[0x00F9] = 0x05; // CC (C=1, Z=1)
    bus.memory[0x00FA] = 0xBB; // B
    bus.memory[0x00FB] = 0xAA; // A
    bus.memory[0x00FC] = 0x12; // XH
    bus.memory[0x00FD] = 0x34; // XL
    bus.memory[0x00FE] = 0x30; // PCH
    bus.memory[0x00FF] = 0x00; // PCL

    cpu.cc = CcFlag::I as u8; // I flag set (will be restored to 0x05)
    bus.load(0, &[0x3B]); // RTI
    tick(&mut cpu, &mut bus, 10);

    assert_eq!(cpu.cc, 0x05);
    assert_eq!(cpu.b, 0xBB);
    assert_eq!(cpu.a, 0xAA);
    assert_eq!(cpu.x, 0x1234);
    assert_eq!(cpu.pc, 0x3000);
    assert_eq!(cpu.sp, 0x00FF);
}

// =============================================================================
// SWI + RTI roundtrip
// =============================================================================

#[test]
fn test_swi_rti_roundtrip() {
    let mut cpu = M6800::new();
    let mut bus = TestBus::new();
    cpu.sp = 0x00FF;
    cpu.a = 0x11;
    cpu.b = 0x22;
    cpu.x = 0x5678;
    cpu.cc = CcFlag::C as u8 | CcFlag::Z as u8; // some flags set

    // At 0x0000: SWI
    bus.load(0, &[0x3F]);
    // SWI vector → 0x1000
    bus.memory[0xFFFA] = 0x10;
    bus.memory[0xFFFB] = 0x00;
    // At 0x1000: RTI
    bus.load(0x1000, &[0x3B]);

    let original_cc = cpu.cc;

    // Execute SWI (12 cycles)
    tick(&mut cpu, &mut bus, 12);
    assert_eq!(cpu.pc, 0x1000);
    assert_ne!(cpu.cc & (CcFlag::I as u8), 0); // I set by SWI

    // Execute RTI (10 cycles)
    tick(&mut cpu, &mut bus, 10);
    assert_eq!(cpu.a, 0x11);
    assert_eq!(cpu.b, 0x22);
    assert_eq!(cpu.x, 0x5678);
    assert_eq!(cpu.pc, 0x0001); // after the SWI opcode
    assert_eq!(cpu.cc, original_cc); // flags restored (I cleared)
    assert_eq!(cpu.sp, 0x00FF);
}

// =============================================================================
// WAI (0x3E) - Wait for Interrupt - 9 cycles + wait state
// =============================================================================

#[test]
fn test_wai_pushes_and_waits() {
    let mut cpu = M6800::new();
    let mut bus = InterruptBus::new();
    cpu.sp = 0x00FF;
    cpu.a = 0xAA;
    cpu.b = 0xBB;
    cpu.x = 0x1234;
    cpu.cc = 0x00;
    bus.load(0, &[0x3E]); // WAI

    // Execute WAI (9 cycles: 1 fetch + 8 execute)
    tick_int(&mut cpu, &mut bus, 9);

    // CPU should be in wait state (is_sleeping returns true)
    assert!(cpu.is_sleeping());

    // SP decremented by 7
    assert_eq!(cpu.sp, 0x00F8);

    // Stack should contain all registers
    assert_eq!(bus.memory[0x00FF], 0x01); // PCL (after WAI opcode)
    assert_eq!(bus.memory[0x00FE], 0x00); // PCH
    assert_eq!(bus.memory[0x00FD], 0x34); // XL
    assert_eq!(bus.memory[0x00FC], 0x12); // XH
    assert_eq!(bus.memory[0x00FB], 0xAA); // A
    assert_eq!(bus.memory[0x00FA], 0xBB); // B
    assert_eq!(bus.memory[0x00F9], 0x00); // CC
}

#[test]
fn test_wai_resumes_on_irq() {
    let mut cpu = M6800::new();
    let mut bus = InterruptBus::new();
    cpu.sp = 0x00FF;
    cpu.cc = 0x00; // I=0 (IRQ enabled)
    bus.load(0, &[0x3E]); // WAI
    // IRQ vector → 0x2000
    bus.memory[0xFFF8] = 0x20;
    bus.memory[0xFFF9] = 0x00;
    // Handler at 0x2000: NOP
    bus.load(0x2000, &[0x01]);

    // Execute WAI
    tick_int(&mut cpu, &mut bus, 9);
    assert!(cpu.is_sleeping());

    // Tick a few times while waiting — still sleeping
    tick_int(&mut cpu, &mut bus, 3);
    assert!(cpu.is_sleeping());

    // Assert IRQ
    bus.irq = true;
    // One tick to detect IRQ and start vector fetch
    tick_int(&mut cpu, &mut bus, 1);
    assert!(!cpu.is_sleeping());

    // 2 more cycles for vector read (cycles 7 and 8 of interrupt sequence)
    tick_int(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.pc, 0x2000);
    assert_ne!(cpu.cc & (CcFlag::I as u8), 0); // I set
}

#[test]
fn test_wai_resumes_on_nmi() {
    let mut cpu = M6800::new();
    let mut bus = InterruptBus::new();
    cpu.sp = 0x00FF;
    cpu.cc = CcFlag::I as u8; // I=1 (IRQ masked) — NMI should still work
    bus.load(0, &[0x3E]); // WAI
    // NMI vector → 0x3000
    bus.memory[0xFFFC] = 0x30;
    bus.memory[0xFFFD] = 0x00;

    // Execute WAI
    tick_int(&mut cpu, &mut bus, 9);
    assert!(cpu.is_sleeping());

    // Assert NMI (edge-triggered)
    bus.nmi = true;
    tick_int(&mut cpu, &mut bus, 1); // detect NMI edge
    assert!(!cpu.is_sleeping());

    // 2 more cycles for vector read
    tick_int(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.pc, 0x3000);
}

#[test]
fn test_wai_irq_masked_stays_waiting() {
    let mut cpu = M6800::new();
    let mut bus = InterruptBus::new();
    cpu.sp = 0x00FF;
    cpu.cc = CcFlag::I as u8; // I=1 (IRQ masked)
    bus.load(0, &[0x3E]); // WAI

    tick_int(&mut cpu, &mut bus, 9);
    assert!(cpu.is_sleeping());

    // Assert IRQ while masked — should stay sleeping
    bus.irq = true;
    tick_int(&mut cpu, &mut bus, 5);
    assert!(cpu.is_sleeping());
}

// =============================================================================
// NMI Hardware Interrupt (edge-triggered)
// =============================================================================

#[test]
fn test_nmi_pushes_and_vectors() {
    let mut cpu = M6800::new();
    let mut bus = InterruptBus::new();
    cpu.sp = 0x00FF;
    cpu.a = 0xAA;
    cpu.b = 0xBB;
    cpu.x = 0x1234;
    cpu.cc = CcFlag::I as u8; // I=1 — NMI ignores mask
    // NOP at 0x0000
    bus.load(0, &[0x01, 0x01]);
    // NMI vector → 0x4000
    bus.memory[0xFFFC] = 0x40;
    bus.memory[0xFFFD] = 0x00;

    // Execute first NOP (2 cycles)
    tick_int(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.pc, 1);

    // Assert NMI before next instruction fetch
    bus.nmi = true;

    // Next fetch detects NMI edge → enters interrupt sequence
    // 1 cycle for fetch (NMI detected) + 9 cycles for interrupt = 10 cycles
    tick_int(&mut cpu, &mut bus, 10);

    assert_eq!(cpu.pc, 0x4000);
    assert_eq!(cpu.sp, 0x00F8);
    assert_ne!(cpu.cc & (CcFlag::I as u8), 0);

    // Stack: PCL, PCH, XL, XH, A, B, CC
    assert_eq!(bus.memory[0x00FF], 0x01); // PCL (was at PC=1)
    assert_eq!(bus.memory[0x00FE], 0x00); // PCH
    assert_eq!(bus.memory[0x00FD], 0x34); // XL
    assert_eq!(bus.memory[0x00FC], 0x12); // XH
    assert_eq!(bus.memory[0x00FB], 0xAA); // A
    assert_eq!(bus.memory[0x00FA], 0xBB); // B
}

#[test]
fn test_nmi_edge_only_once() {
    let mut cpu = M6800::new();
    let mut bus = InterruptBus::new();
    cpu.sp = 0x00FF;
    bus.load(0, &[0x01, 0x01, 0x01, 0x01]); // NOPs
    bus.memory[0xFFFC] = 0x00;
    bus.memory[0xFFFD] = 0x10; // NMI vector → 0x0010
    bus.load(0x10, &[0x01, 0x01, 0x01, 0x01]); // NOPs at handler

    // Assert NMI
    bus.nmi = true;

    // First NOP fetch + NMI detection + interrupt sequence = 10 cycles
    tick_int(&mut cpu, &mut bus, 10);
    assert_eq!(cpu.pc, 0x0010);

    // Keep NMI high — should NOT re-trigger (edge-triggered, already latched)
    tick_int(&mut cpu, &mut bus, 2); // NOP at handler
    assert_eq!(cpu.pc, 0x0011); // continues normally, no re-trigger
}

// =============================================================================
// IRQ Hardware Interrupt (level-sensitive, masked by I)
// =============================================================================

#[test]
fn test_irq_when_enabled() {
    let mut cpu = M6800::new();
    let mut bus = InterruptBus::new();
    cpu.sp = 0x00FF;
    cpu.cc = 0x00; // I=0 (IRQ enabled)
    bus.load(0, &[0x01, 0x01]); // NOPs
    // IRQ vector → 0x5000
    bus.memory[0xFFF8] = 0x50;
    bus.memory[0xFFF9] = 0x00;

    // Execute first NOP
    tick_int(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.pc, 1);

    // Assert IRQ
    bus.irq = true;

    // Next fetch + interrupt = 10 cycles
    tick_int(&mut cpu, &mut bus, 10);
    assert_eq!(cpu.pc, 0x5000);
    assert_ne!(cpu.cc & (CcFlag::I as u8), 0); // I set
    assert_eq!(cpu.sp, 0x00F8);
}

#[test]
fn test_irq_masked_no_interrupt() {
    let mut cpu = M6800::new();
    let mut bus = InterruptBus::new();
    cpu.sp = 0x00FF;
    cpu.cc = CcFlag::I as u8; // I=1 (IRQ masked)
    bus.load(0, &[0x01, 0x01]); // NOPs
    bus.memory[0xFFF8] = 0x50;
    bus.memory[0xFFF9] = 0x00;

    bus.irq = true;

    // Execute NOPs — IRQ should be ignored
    tick_int(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.pc, 1);
    tick_int(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.pc, 2);
    assert_eq!(cpu.sp, 0x00FF); // no push
}

#[test]
fn test_irq_after_cli() {
    let mut cpu = M6800::new();
    let mut bus = InterruptBus::new();
    cpu.sp = 0x00FF;
    cpu.cc = CcFlag::I as u8; // I=1 (masked)
    // SEI already set; CLI at 0x00, then NOP
    bus.load(0, &[0x0E, 0x01]); // CLI; NOP
    bus.memory[0xFFF8] = 0x60;
    bus.memory[0xFFF9] = 0x00;

    bus.irq = true;

    // Execute CLI (2 cycles) — clears I flag
    tick_int(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.cc & (CcFlag::I as u8), 0); // I cleared

    // Now NOP fetch will detect IRQ since I=0
    // fetch + interrupt = 10 cycles
    tick_int(&mut cpu, &mut bus, 10);
    assert_eq!(cpu.pc, 0x6000);
}

// =============================================================================
// NMI + RTI roundtrip
// =============================================================================

#[test]
fn test_nmi_rti_roundtrip() {
    let mut cpu = M6800::new();
    let mut bus = InterruptBus::new();
    cpu.sp = 0x00FF;
    cpu.a = 0x11;
    cpu.b = 0x22;
    cpu.x = 0xABCD;
    cpu.cc = 0x00; // I=0

    // Main program: NOP NOP NOP
    bus.load(0, &[0x01, 0x01, 0x01]);
    // NMI vector → 0x2000
    bus.memory[0xFFFC] = 0x20;
    bus.memory[0xFFFD] = 0x00;
    // Handler: RTI
    bus.load(0x2000, &[0x3B]);

    // Execute first NOP
    tick_int(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.pc, 1);

    // Assert NMI
    bus.nmi = true;

    // NMI interrupt (10 cycles)
    tick_int(&mut cpu, &mut bus, 10);
    assert_eq!(cpu.pc, 0x2000);

    // Deassert NMI to avoid re-trigger on RTI return
    bus.nmi = false;

    // RTI (10 cycles) — restores all registers
    tick_int(&mut cpu, &mut bus, 10);
    assert_eq!(cpu.a, 0x11);
    assert_eq!(cpu.b, 0x22);
    assert_eq!(cpu.x, 0xABCD);
    assert_eq!(cpu.pc, 1); // returns to where NMI interrupted
    assert_eq!(cpu.cc, 0x00); // I restored to 0
    assert_eq!(cpu.sp, 0x00FF);
}
