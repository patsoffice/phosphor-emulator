use phosphor_core::core::{Bus, BusMaster, BusMasterComponent, bus::InterruptState};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::m6809::{CcFlag, M6809};

/// One recorded bus cycle: the address driven, and whether it was a write.
/// A cycle with no bus access at all leaves no entry.
type Access = (u16, bool);

/// Test bus with controllable interrupt lines.
struct InterruptBus {
    memory: [u8; 0x10000],
    irq: bool,
    firq: bool,
    nmi: bool,
    /// Every access in order, so a test can assert the *sequence* rather than
    /// only the total. A cycle count can be right for the wrong reason: two
    /// missing don't-care cycles and two spurious ones cancel exactly.
    trace: Vec<Access>,
}

impl InterruptBus {
    fn new() -> Self {
        Self {
            memory: [0; 0x10000],
            irq: false,
            firq: false,
            nmi: false,
            trace: Vec::new(),
        }
    }

    fn load(&mut self, addr: u16, data: &[u8]) {
        let start = addr as usize;
        self.memory[start..start + data.len()].copy_from_slice(data);
    }

    fn take_trace(&mut self) -> Vec<Access> {
        std::mem::take(&mut self.trace)
    }
}

impl Bus for InterruptBus {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        self.trace.push((addr, false));
        self.memory[addr as usize]
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        self.trace.push((addr, true));
        self.memory[addr as usize] = data;
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            nmi: self.nmi,
            irq: self.irq,
            firq: self.firq,
            irq_vector: 0xFF,
            irq_level: 0,
        }
    }
}

fn tick(cpu: &mut M6809, bus: &mut InterruptBus, n: usize) {
    for _ in 0..n {
        cpu.tick_with_bus(bus, BusMaster::Cpu(0));
    }
}

// ===== IRQ Hardware Interrupt =====

#[test]
fn test_irq_pushes_all_registers_and_vectors() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.a = 0x11;
    cpu.b = 0x22;
    cpu.dp = 0x33;
    cpu.x = 0x4455;
    cpu.y = 0x6677;
    cpu.u = 0x8899;
    cpu.s = 0x0100;
    cpu.pc = 0x0000;
    cpu.cc = 0x00; // I flag clear = IRQ enabled

    // IRQ vector
    bus.memory[0xFFF8] = 0x30;
    bus.memory[0xFFF9] = 0x00;

    // NOP at 0x0000 (will be pre-empted by IRQ during Fetch)
    bus.load(0x0000, &[0x12]);

    // Assert IRQ before ticking
    bus.irq = true;

    // IRQ response: 1 cycle (Fetch detects IRQ) + 18 cycles (2 internal + 12 push
    // + 1 internal + 2 vector + 1 internal) = 19 cycles, matching SWI.
    tick(&mut cpu, &mut bus, 19);

    // PC should be at IRQ handler
    assert_eq!(cpu.pc, 0x3000, "PC should be at IRQ vector");

    // S should be decremented by 12
    assert_eq!(cpu.s, 0x0100 - 12, "S should point to pushed registers");

    // E flag should be set in pushed CC (entire state saved)
    let pushed_cc = bus.memory[0x0100 - 12];
    assert_ne!(
        pushed_cc & (CcFlag::E as u8),
        0,
        "E flag should be set in pushed CC"
    );

    // I flag should be set (IRQ masked after taking IRQ)
    assert_ne!(
        cpu.cc & (CcFlag::I as u8),
        0,
        "I flag should be set after IRQ"
    );

    // F flag should NOT be set by IRQ (only NMI sets both)
    assert_eq!(
        cpu.cc & (CcFlag::F as u8),
        0,
        "F flag should not be set by IRQ"
    );

    // Check pushed register values on stack (top of stack going up):
    // CC, A, B, DP, X(hi), X(lo), Y(hi), Y(lo), U(hi), U(lo), PC(hi), PC(lo)
    let base = 0x0100 - 12;
    assert_eq!(bus.memory[base], pushed_cc, "CC");
    assert_eq!(bus.memory[base + 1], 0x11, "A");
    assert_eq!(bus.memory[base + 2], 0x22, "B");
    assert_eq!(bus.memory[base + 3], 0x33, "DP");
    assert_eq!(bus.memory[base + 4], 0x44, "X high");
    assert_eq!(bus.memory[base + 5], 0x55, "X low");
    assert_eq!(bus.memory[base + 6], 0x66, "Y high");
    assert_eq!(bus.memory[base + 7], 0x77, "Y low");
    assert_eq!(bus.memory[base + 8], 0x88, "U high");
    assert_eq!(bus.memory[base + 9], 0x99, "U low");
    // Pushed PC is the address of the instruction that was NOT executed
    assert_eq!(bus.memory[base + 10], 0x00, "PC high");
    assert_eq!(bus.memory[base + 11], 0x00, "PC low");
}

#[test]
fn test_irq_masked_does_not_fire() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.pc = 0x0000;
    cpu.cc = CcFlag::I as u8; // IRQ masked
    cpu.s = 0x0100;

    bus.memory[0xFFF8] = 0x30;
    bus.memory[0xFFF9] = 0x00;

    // NOP at 0x0000
    bus.load(0x0000, &[0x12, 0x12]);

    bus.irq = true;

    // Execute NOP (2 cycles: 1 fetch + 1 execute)
    tick(&mut cpu, &mut bus, 2);

    // Should have executed NOP, not taken IRQ
    assert_eq!(cpu.pc, 0x0001, "Should have advanced past NOP");
    assert_eq!(cpu.s, 0x0100, "Stack should be unchanged");
}

#[test]
fn test_irq_then_rti_roundtrip() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.a = 0xAA;
    cpu.b = 0xBB;
    cpu.dp = 0x10;
    cpu.x = 0x1234;
    cpu.y = 0x5678;
    cpu.u = 0x9ABC;
    cpu.s = 0x0100;
    cpu.pc = 0x0000;
    cpu.cc = 0x00; // IRQ enabled

    // IRQ vector points to RTI instruction
    bus.memory[0xFFF8] = 0x40;
    bus.memory[0xFFF9] = 0x00;
    bus.load(0x4000, &[0x3B]); // RTI

    // NOP at 0x0000
    bus.load(0x0000, &[0x12]);

    bus.irq = true;

    // IRQ response: 19 cycles (matching SWI)
    tick(&mut cpu, &mut bus, 19);
    assert_eq!(cpu.pc, 0x4000, "Should be at IRQ handler");

    // Deassert IRQ before RTI
    bus.irq = false;

    // RTI with E=1: 1 fetch + 1 internal + 12 pulls + 1 internal = 15 cycles
    tick(&mut cpu, &mut bus, 15);

    // All registers should be restored
    assert_eq!(cpu.a, 0xAA, "A restored");
    assert_eq!(cpu.b, 0xBB, "B restored");
    assert_eq!(cpu.dp, 0x10, "DP restored");
    assert_eq!(cpu.x, 0x1234, "X restored");
    assert_eq!(cpu.y, 0x5678, "Y restored");
    assert_eq!(cpu.u, 0x9ABC, "U restored");
    assert_eq!(cpu.pc, 0x0000, "PC restored to interrupted instruction");
    assert_eq!(cpu.s, 0x0100, "S restored");
}

// ===== FIRQ Hardware Interrupt =====

#[test]
fn test_firq_pushes_cc_and_pc_only() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.a = 0x11;
    cpu.b = 0x22;
    cpu.s = 0x0100;
    cpu.pc = 0x0000;
    cpu.cc = 0x00; // F flag clear = FIRQ enabled

    // FIRQ vector
    bus.memory[0xFFF6] = 0x50;
    bus.memory[0xFFF7] = 0x00;

    bus.load(0x0000, &[0x12]); // NOP

    bus.firq = true;

    // FIRQ response: 1 cycle (Fetch detects) + 9 cycles (2 internal + 3 push
    // + 1 internal + 2 vector + 1 internal) = 10 cycles.
    tick(&mut cpu, &mut bus, 10);

    assert_eq!(cpu.pc, 0x5000, "PC should be at FIRQ vector");

    // S decremented by 3 (CC + PC)
    assert_eq!(cpu.s, 0x0100 - 3, "S should be decremented by 3");

    // E flag should be CLEAR in pushed CC
    let pushed_cc = bus.memory[0x0100 - 3];
    assert_eq!(
        pushed_cc & (CcFlag::E as u8),
        0,
        "E flag should be clear in pushed CC"
    );

    // Both I and F should be set after FIRQ
    assert_ne!(
        cpu.cc & (CcFlag::I as u8),
        0,
        "I flag should be set after FIRQ"
    );
    assert_ne!(
        cpu.cc & (CcFlag::F as u8),
        0,
        "F flag should be set after FIRQ"
    );

    // Check pushed values: CC at top, then PC(hi), PC(lo)
    let base = 0x0100 - 3;
    assert_eq!(bus.memory[base], pushed_cc, "CC");
    assert_eq!(bus.memory[base + 1], 0x00, "PC high");
    assert_eq!(bus.memory[base + 2], 0x00, "PC low");

    // A and B should be unchanged (FIRQ doesn't push them)
    assert_eq!(cpu.a, 0x11, "A unchanged");
    assert_eq!(cpu.b, 0x22, "B unchanged");
}

#[test]
fn test_firq_masked_does_not_fire() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.pc = 0x0000;
    cpu.cc = CcFlag::F as u8; // FIRQ masked
    cpu.s = 0x0100;

    bus.load(0x0000, &[0x12]);

    bus.firq = true;

    tick(&mut cpu, &mut bus, 2); // NOP

    assert_eq!(cpu.pc, 0x0001, "Should execute NOP, not take FIRQ");
    assert_eq!(cpu.s, 0x0100, "Stack unchanged");
}

#[test]
fn test_firq_then_rti_fast_return() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.a = 0xAA;
    cpu.b = 0xBB;
    cpu.s = 0x0100;
    cpu.pc = 0x0000;
    cpu.cc = 0x00; // FIRQ enabled

    // FIRQ vector points to RTI
    bus.memory[0xFFF6] = 0x40;
    bus.memory[0xFFF7] = 0x00;
    bus.load(0x4000, &[0x3B]); // RTI

    bus.load(0x0000, &[0x12]); // NOP

    bus.firq = true;
    tick(&mut cpu, &mut bus, 10); // FIRQ response

    bus.firq = false;

    // RTI with E=0: 1 fetch + 1 internal + 1 pull CC + 1 internal + 2 pull PC = 6 cycles
    tick(&mut cpu, &mut bus, 6);

    assert_eq!(cpu.pc, 0x0000, "PC restored");
    assert_eq!(cpu.s, 0x0100, "S restored");
    // CC should be restored (E clear, I and F clear from original)
    assert_eq!(cpu.cc & (CcFlag::I as u8), 0, "I flag restored to clear");
    assert_eq!(cpu.cc & (CcFlag::F as u8), 0, "F flag restored to clear");
}

// ===== NMI Hardware Interrupt =====

#[test]
fn test_nmi_pushes_all_and_masks_both() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.a = 0x11;
    cpu.b = 0x22;
    cpu.dp = 0x33;
    cpu.x = 0x4455;
    cpu.y = 0x6677;
    cpu.u = 0x8899;
    cpu.s = 0x0100;
    cpu.pc = 0x0000;
    cpu.cc = 0x00;

    // NMI vector
    bus.memory[0xFFFC] = 0x60;
    bus.memory[0xFFFD] = 0x00;

    bus.load(0x0000, &[0x12]); // NOP

    bus.nmi = true;

    // NMI response: 19 cycles (same as IRQ)
    tick(&mut cpu, &mut bus, 19);

    assert_eq!(cpu.pc, 0x6000, "PC should be at NMI vector");
    assert_eq!(cpu.s, 0x0100 - 12, "S decremented by 12");

    // Both I and F should be set (NMI masks both)
    assert_ne!(cpu.cc & (CcFlag::I as u8), 0, "I flag set after NMI");
    assert_ne!(cpu.cc & (CcFlag::F as u8), 0, "F flag set after NMI");

    // E flag set in pushed CC
    let pushed_cc = bus.memory[0x0100 - 12];
    assert_ne!(pushed_cc & (CcFlag::E as u8), 0, "E set in pushed CC");
}

#[test]
fn test_nmi_cannot_be_masked() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.pc = 0x0000;
    cpu.cc = CcFlag::I as u8 | CcFlag::F as u8; // Both masks set
    cpu.s = 0x0100;

    bus.memory[0xFFFC] = 0x60;
    bus.memory[0xFFFD] = 0x00;

    bus.load(0x0000, &[0x12]);

    bus.nmi = true;

    tick(&mut cpu, &mut bus, 19);

    // NMI should fire despite masks
    assert_eq!(cpu.pc, 0x6000, "NMI should fire even with I+F masked");
    assert_eq!(cpu.s, 0x0100 - 12, "Stack should show push occurred");
}

#[test]
fn test_nmi_edge_triggered() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.pc = 0x0000;
    cpu.cc = 0x00;
    cpu.s = 0x0100;

    bus.memory[0xFFFC] = 0x40;
    bus.memory[0xFFFD] = 0x00;
    // NOP sled at handler
    bus.load(0x4000, &[0x12, 0x12, 0x12, 0x12]);

    bus.load(0x0000, &[0x12, 0x12]);

    // Assert NMI
    bus.nmi = true;

    // First NMI fires
    tick(&mut cpu, &mut bus, 19);
    assert_eq!(cpu.pc, 0x4000, "First NMI should fire");

    // NMI stays high - should NOT re-trigger (edge-triggered)
    tick(&mut cpu, &mut bus, 2); // Execute NOP at handler
    assert_eq!(cpu.pc, 0x4001, "Should execute NOP, not retrigger NMI");

    // Deassert then reassert for second NMI
    bus.nmi = false;
    tick(&mut cpu, &mut bus, 2); // Another NOP
    assert_eq!(cpu.pc, 0x4002);

    // Need to clear I+F to allow NMI to push again (NMI itself masks them)
    // Actually NMI is non-maskable - it always fires on edge regardless of I/F
    bus.nmi = true;
    // New rising edge should trigger another NMI
    tick(&mut cpu, &mut bus, 19);
    assert_eq!(cpu.pc, 0x4000, "Second NMI should fire on re-assertion");
}

// ===== Interrupt Priority =====

#[test]
fn test_interrupt_priority_nmi_over_firq_over_irq() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.pc = 0x0000;
    cpu.cc = 0x00; // All interrupts enabled
    cpu.s = 0x0100;

    bus.memory[0xFFFC] = 0x60; // NMI -> 0x6000
    bus.memory[0xFFFD] = 0x00;
    bus.memory[0xFFF6] = 0x50; // FIRQ -> 0x5000
    bus.memory[0xFFF7] = 0x00;
    bus.memory[0xFFF8] = 0x40; // IRQ -> 0x4000
    bus.memory[0xFFF9] = 0x00;

    bus.load(0x0000, &[0x12]);

    // All three asserted simultaneously
    bus.nmi = true;
    bus.firq = true;
    bus.irq = true;

    tick(&mut cpu, &mut bus, 19);

    // NMI should win (highest priority)
    assert_eq!(cpu.pc, 0x6000, "NMI should have highest priority");
}

// ===== CWAI (0x3C) =====

#[test]
fn test_cwai_ands_cc_sets_e_and_pushes() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.a = 0x11;
    cpu.b = 0x22;
    cpu.dp = 0x33;
    cpu.x = 0x4455;
    cpu.y = 0x6677;
    cpu.u = 0x8899;
    cpu.s = 0x0100;
    cpu.pc = 0x0000;
    cpu.cc = CcFlag::I as u8 | CcFlag::F as u8 | CcFlag::N as u8; // I+F+N set

    // CWAI #$EF = AND CC with 0xEF (clears I flag)
    bus.load(0x0000, &[0x3C, 0xEF]);

    // CWAI: 1 fetch + 15 execute (1 read imm + 2 don't-care + 12 push)
    // = 16 cycles before the wait state.
    tick(&mut cpu, &mut bus, 16);

    // CPU should be sleeping (WaitForInterrupt)
    assert!(cpu.is_sleeping(), "CPU should be in wait state");

    // S decremented by 12
    assert_eq!(cpu.s, 0x0100 - 12, "All registers pushed");

    // CC on stack should have: E set, I cleared (ANDed with 0xEF), F+N kept
    let pushed_cc = bus.memory[0x0100 - 12];
    assert_ne!(pushed_cc & (CcFlag::E as u8), 0, "E should be set");
    assert_eq!(
        pushed_cc & (CcFlag::I as u8),
        0,
        "I should be cleared by AND"
    );
    assert_ne!(pushed_cc & (CcFlag::F as u8), 0, "F should be kept");
    assert_ne!(pushed_cc & (CcFlag::N as u8), 0, "N should be kept");
}

#[test]
fn test_cwai_wakes_on_irq() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.s = 0x0100;
    cpu.pc = 0x0000;
    cpu.cc = CcFlag::I as u8; // IRQ masked initially

    // CWAI #$EF = clears I flag (bit 4)
    bus.load(0x0000, &[0x3C, 0xEF]);

    // IRQ vector
    bus.memory[0xFFF8] = 0x40;
    bus.memory[0xFFF9] = 0x00;

    // Execute CWAI (push phase)
    tick(&mut cpu, &mut bus, 16);
    assert!(cpu.is_sleeping(), "Should be waiting");

    // Assert IRQ
    bus.irq = true;

    // CWAI completion: 1 cycle to notice the request, then the vector fetch
    // bracketed by a /VMA cycle on each side = 1 + 4 = 5 cycles.
    tick(&mut cpu, &mut bus, 5);

    assert!(!cpu.is_sleeping(), "Should be awake");
    assert_eq!(cpu.pc, 0x4000, "Should be at IRQ handler");

    // I flag should be set (masked after servicing)
    assert_ne!(cpu.cc & (CcFlag::I as u8), 0, "I flag should be set");
}

#[test]
fn test_cwai_wakes_on_firq() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.s = 0x0100;
    cpu.pc = 0x0000;
    cpu.cc = CcFlag::F as u8; // FIRQ masked initially

    // CWAI #$BF = clears F flag (bit 6)
    bus.load(0x0000, &[0x3C, 0xBF]);

    // FIRQ vector
    bus.memory[0xFFF6] = 0x50;
    bus.memory[0xFFF7] = 0x00;

    tick(&mut cpu, &mut bus, 16);
    assert!(cpu.is_sleeping());

    bus.firq = true;

    tick(&mut cpu, &mut bus, 5);

    assert_eq!(cpu.pc, 0x5000, "Should be at FIRQ handler");
    // CWAI always pushes all with E set, even for FIRQ
    assert_eq!(
        cpu.s,
        0x0100 - 12,
        "All registers pushed (CWAI always pushes all)"
    );
}

#[test]
fn test_cwai_wakes_on_nmi() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.s = 0x0100;
    cpu.pc = 0x0000;
    cpu.cc = CcFlag::I as u8 | CcFlag::F as u8; // Both masked

    // CWAI #$FF = no flags cleared (keep all masks, just wait)
    bus.load(0x0000, &[0x3C, 0xFF]);

    // NMI vector
    bus.memory[0xFFFC] = 0x60;
    bus.memory[0xFFFD] = 0x00;

    tick(&mut cpu, &mut bus, 16);
    assert!(cpu.is_sleeping());

    bus.nmi = true;

    tick(&mut cpu, &mut bus, 5);

    assert_eq!(cpu.pc, 0x6000, "Should be at NMI handler");
    // NMI masks both I and F
    assert_ne!(cpu.cc & (CcFlag::I as u8), 0, "I set after NMI");
    assert_ne!(cpu.cc & (CcFlag::F as u8), 0, "F set after NMI");
}

#[test]
fn test_cwai_then_rti_roundtrip() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.a = 0xAA;
    cpu.b = 0xBB;
    cpu.dp = 0x10;
    cpu.x = 0x1234;
    cpu.y = 0x5678;
    cpu.u = 0x9ABC;
    cpu.s = 0x0100;
    cpu.pc = 0x0000;
    cpu.cc = CcFlag::I as u8; // IRQ masked

    // CWAI #$EF (clear I) then NOP
    bus.load(0x0000, &[0x3C, 0xEF, 0x12]);

    // IRQ handler = RTI
    bus.memory[0xFFF8] = 0x40;
    bus.memory[0xFFF9] = 0x00;
    bus.load(0x4000, &[0x3B]); // RTI

    // Execute CWAI
    tick(&mut cpu, &mut bus, 16);

    // Assert IRQ, wake up and vector
    bus.irq = true;
    tick(&mut cpu, &mut bus, 5);
    assert_eq!(cpu.pc, 0x4000);

    // Deassert IRQ
    bus.irq = false;

    // RTI with E=1: 1 fetch + 1 internal + 12 pulls + 1 internal = 15 cycles
    tick(&mut cpu, &mut bus, 15);

    // All registers restored
    assert_eq!(cpu.a, 0xAA);
    assert_eq!(cpu.b, 0xBB);
    assert_eq!(cpu.dp, 0x10);
    assert_eq!(cpu.x, 0x1234);
    assert_eq!(cpu.y, 0x5678);
    assert_eq!(cpu.u, 0x9ABC);
    assert_eq!(cpu.s, 0x0100);
    // PC restored to after CWAI instruction
    assert_eq!(cpu.pc, 0x0002, "PC should be after CWAI #imm");
}

// ===== SYNC (0x13) =====

#[test]
fn test_sync_sleeps_until_interrupt() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.pc = 0x0000;
    cpu.cc = 0x00; // Interrupts enabled
    cpu.s = 0x0100;

    bus.load(0x0000, &[0x13, 0x12]); // SYNC, NOP

    // Execute SYNC: 1 fetch + 1 execute = 2 cycles
    tick(&mut cpu, &mut bus, 2);

    assert!(cpu.is_sleeping(), "Should be sleeping after SYNC");
    assert_eq!(cpu.s, 0x0100, "Stack unchanged by SYNC");
}

#[test]
fn test_sync_wakes_on_unmasked_irq() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.pc = 0x0000;
    cpu.cc = 0x00;
    cpu.s = 0x0100;
    cpu.a = 0xAA;

    bus.load(0x0000, &[0x13, 0x12]); // SYNC, NOP

    // IRQ vector -> handler
    bus.memory[0xFFF8] = 0x40;
    bus.memory[0xFFF9] = 0x00;
    bus.load(0x4000, &[0x12]); // NOP at handler

    tick(&mut cpu, &mut bus, 2); // Execute SYNC
    assert!(cpu.is_sleeping());

    bus.irq = true;

    // SYNC wakes + starts full IRQ response: 19 cycles
    tick(&mut cpu, &mut bus, 19);

    assert_eq!(cpu.pc, 0x4000, "Should be at IRQ handler");
    assert_eq!(cpu.s, 0x0100 - 12, "Full register push for IRQ");
}

#[test]
fn test_sync_masked_interrupt_just_wakes() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.pc = 0x0000;
    cpu.cc = CcFlag::I as u8; // IRQ masked
    cpu.s = 0x0100;

    bus.load(0x0000, &[0x13, 0x12, 0x12]); // SYNC, NOP, NOP

    tick(&mut cpu, &mut bus, 2); // Execute SYNC
    assert!(cpu.is_sleeping());

    bus.irq = true;

    // Masked IRQ wakes from SYNC but doesn't take interrupt
    tick(&mut cpu, &mut bus, 1);
    assert!(!cpu.is_sleeping(), "Should wake up from SYNC");

    // CPU should continue at next instruction (NOP at 0x0001)
    tick(&mut cpu, &mut bus, 2); // Execute NOP
    assert_eq!(cpu.pc, 0x0002, "Should continue after SYNC");
    assert_eq!(cpu.s, 0x0100, "Stack unchanged");
}

#[test]
fn test_sync_wakes_on_nmi() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.pc = 0x0000;
    cpu.cc = CcFlag::I as u8 | CcFlag::F as u8; // Both masked
    cpu.s = 0x0100;

    bus.load(0x0000, &[0x13]);

    bus.memory[0xFFFC] = 0x60;
    bus.memory[0xFFFD] = 0x00;

    tick(&mut cpu, &mut bus, 2); // SYNC
    assert!(cpu.is_sleeping());

    bus.nmi = true;

    // NMI from SYNC: full response (19 cycles)
    tick(&mut cpu, &mut bus, 19);
    assert_eq!(cpu.pc, 0x6000, "NMI should fire from SYNC");
}

#[test]
fn test_sync_wakes_on_firq() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.pc = 0x0000;
    cpu.cc = 0x00; // FIRQ enabled
    cpu.s = 0x0100;

    bus.load(0x0000, &[0x13]);

    bus.memory[0xFFF6] = 0x50;
    bus.memory[0xFFF7] = 0x00;

    tick(&mut cpu, &mut bus, 2); // SYNC
    assert!(cpu.is_sleeping());

    bus.firq = true;

    // FIRQ from SYNC: fast response (10 cycles)
    tick(&mut cpu, &mut bus, 10);
    assert_eq!(cpu.pc, 0x5000, "FIRQ should fire from SYNC");
    assert_eq!(cpu.s, 0x0100 - 3, "FIRQ pushes CC+PC only");
}

/// A CPU sitting at an instruction boundary has not committed to running the
/// instruction at its PC: it samples the interrupt lines first and may vector
/// instead. `debug_servicing_interrupt` is what lets an observer tell the two
/// apart, one cycle later.
///
/// Regression test for phosphor-emulator-rk20, where `disasm trace --cpu`
/// logged the un-executed instruction and so shifted every later instruction
/// index — invisible in one trace, but it corrupts a diff against another
/// emulator.
#[test]
fn boundary_is_provisional_until_the_interrupt_decision() {
    use phosphor_core::core::debug::DebugCpu;

    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.pc = 0x0000;
    cpu.cc = 0x00; // IRQ enabled
    cpu.s = 0x0100;

    // NOP (12) at 0x0000, then the instruction that must NOT run.
    bus.load(0x0000, &[0x12, 0x12]);
    bus.memory[0xFFF8] = 0x40; // IRQ vector -> 0x4000
    bus.memory[0xFFF9] = 0x00;

    tick(&mut cpu, &mut bus, 2); // execute the first NOP

    // At the boundary the CPU is ready to fetch 0x0001 and is not yet
    // servicing anything — this is exactly the state a tracer would log.
    assert!(cpu.debug_at_instruction_boundary());
    assert!(!cpu.debug_servicing_interrupt());
    assert_eq!(cpu.debug_pc(), 0x0001);

    // Assert IRQ. The very next cycle is the boundary's fetch cycle, where the
    // CPU samples the line and vectors instead of executing 0x0001.
    bus.irq = true;
    tick(&mut cpu, &mut bus, 1);

    assert!(
        cpu.debug_servicing_interrupt(),
        "CPU vectored, so the instruction at the boundary never ran"
    );
    assert_eq!(
        cpu.debug_pc(),
        0x0001,
        "PC is still the un-executed address"
    );

    // Finish the 19-cycle IRQ sequence and land in the handler.
    tick(&mut cpu, &mut bus, 18);
    assert_eq!(cpu.pc, 0x4000);
    assert!(!cpu.debug_servicing_interrupt(), "sequence complete");
}

/// The counterpart: with no interrupt pending, the boundary is confirmed and
/// the instruction really does execute. Without this, suppressing on
/// `debug_servicing_interrupt` could silently drop every instruction.
#[test]
fn boundary_is_confirmed_when_no_interrupt_is_taken() {
    use phosphor_core::core::debug::DebugCpu;

    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.pc = 0x0000;
    cpu.cc = CcFlag::I as u8; // IRQ masked
    cpu.s = 0x0100;
    bus.load(0x0000, &[0x12, 0x12]);
    bus.irq = true; // asserted but masked

    tick(&mut cpu, &mut bus, 2);
    assert!(cpu.debug_at_instruction_boundary());

    tick(&mut cpu, &mut bus, 1);
    assert!(
        !cpu.debug_servicing_interrupt(),
        "masked IRQ must not vector, so the boundary is real"
    );
}

/// The reset sequence occupies the bus before the first opcode fetch: an
/// internal cycle, the two vector reads, and a final internal cycle. A CPU
/// that skips them starts executing early relative to every other device on
/// the board — which is invisible on its own but shifts the phase of anything
/// clocked independently, such as a periodic interrupt divider.
///
/// phosphor-emulator-cjra. Measured against MAME's cycle-stepped core, whose
/// INTERRUPT_VECTOR sequence (base6x09.lst) is these same four cycles.
#[test]
fn reset_sequence_costs_four_cycles_before_the_first_opcode() {
    use phosphor_core::core::debug::DebugCpu;

    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();
    bus.memory[0xFFFE] = 0x30; // reset vector -> 0x3000
    bus.memory[0xFFFF] = 0x00;
    bus.load(0x3000, &[0x12]); // NOP

    cpu.reset(&mut bus, BusMaster::Cpu(0));
    assert_eq!(cpu.pc, 0x3000, "reset() still yields the PC immediately");

    // Four cycles in which the CPU must not be ready to fetch.
    for i in 0..4 {
        assert!(
            !cpu.debug_at_instruction_boundary(),
            "still in the reset sequence at cycle {i}, so not a boundary"
        );
        tick(&mut cpu, &mut bus, 1);
    }

    assert!(
        cpu.debug_at_instruction_boundary(),
        "reset sequence complete — ready to fetch the first opcode"
    );
    assert_eq!(cpu.pc, 0x3000, "and it has not executed anything yet");

    tick(&mut cpu, &mut bus, 1); // fetch the NOP
    assert_eq!(cpu.pc, 0x3001);
}

/// CWAI and the path that resumes from it spend the cycles the hardware spends,
/// on the addresses it drives.
///
/// Both halves were short by two, and neither was catchable any other way: a
/// single-step vector cannot express "and then an interrupt arrives", so CWAI
/// and SYNC are excluded from cross-validation entirely.
///
/// This pins the sequence rather than the count. A total can be right for the
/// wrong reason — drop two don't-care cycles and add two elsewhere and it
/// balances — and the whole content of this fix is *which* cycles those are.
#[test]
fn cwai_and_its_wake_drive_the_addresses_the_hardware_drives() {
    let mut cpu = M6809::new();
    let mut bus = InterruptBus::new();

    cpu.s = 0x0100;
    cpu.pc = 0x0000;
    cpu.cc = CcFlag::I as u8; // IRQ masked until the operand clears it
    bus.load(0x0000, &[0x3C, 0xEF]); // CWAI #$EF
    bus.memory[0xFFF8] = 0x40;
    bus.memory[0xFFF9] = 0x00;

    tick(&mut cpu, &mut bus, 16);
    assert!(cpu.is_sleeping(), "should have reached the wait state");

    let mut expected: Vec<(u16, bool)> = vec![
        (0x0000, false), // opcode fetch
        (0x0001, false), // the immediate operand
        (0x0002, false), // don't-care re-driving PC, now past the operand
        (0xFFFF, false), // /VMA don't-care
    ];
    // Twelve pushes, S predecrementing from 0x0100.
    expected.extend((1..=12).map(|n| (0x0100 - n as u16, true)));
    assert_eq!(bus.take_trace(), expected, "CWAI entry");

    // The wait itself touches nothing: the hardware burns cycles here without
    // driving an address, and so does this.
    tick(&mut cpu, &mut bus, 4);
    assert!(cpu.is_sleeping(), "still waiting with no request asserted");
    assert_eq!(
        bus.take_trace(),
        Vec::new(),
        "the wait state is off the bus"
    );

    bus.irq = true;
    tick(&mut cpu, &mut bus, 5);
    assert!(!cpu.is_sleeping(), "should have woken");
    assert_eq!(cpu.pc, 0x4000, "should be at the IRQ handler");

    // One cycle to notice the request, which touches nothing, then the vector
    // fetch bracketed by a /VMA cycle on each side — exactly as the full
    // NMI/IRQ/FIRQ entry sequences bracket theirs.
    assert_eq!(
        bus.take_trace(),
        vec![
            (0xFFFF, false),
            (0xFFF8, false),
            (0xFFF9, false),
            (0xFFFF, false),
        ],
        "CWAI wake"
    );
}
