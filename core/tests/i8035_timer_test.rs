use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::i8035::{I8035, PswFlag};
mod common;
use common::TestBus;

/// Helper: tick the CPU for `n` machine cycles.
fn tick(cpu: &mut I8035, bus: &mut TestBus, n: usize) {
    for _ in 0..n {
        cpu.tick_with_bus(bus, BusMaster::Cpu(0));
    }
}

// =============================================================================
// Timer control instructions
// =============================================================================

#[test]
fn test_strt_t() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x55]); // STRT T
    tick(&mut cpu, &mut bus, 1);
    assert!(cpu.timer_enabled);
    assert!(!cpu.counter_enabled);
}

#[test]
fn test_strt_cnt() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x45]); // STRT CNT
    tick(&mut cpu, &mut bus, 1);
    assert!(cpu.counter_enabled);
    assert!(!cpu.timer_enabled);
}

#[test]
fn test_stop_tcnt() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.timer_enabled = true;
    bus.load(0, &[0x65]); // STOP TCNT
    tick(&mut cpu, &mut bus, 1);
    assert!(!cpu.timer_enabled);
    assert!(!cpu.counter_enabled);
}

#[test]
fn test_strt_t_disables_counter() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.counter_enabled = true;
    bus.load(0, &[0x55]); // STRT T
    tick(&mut cpu, &mut bus, 1);
    assert!(cpu.timer_enabled);
    assert!(!cpu.counter_enabled); // counter disabled
}

#[test]
fn test_strt_cnt_disables_timer() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.timer_enabled = true;
    bus.load(0, &[0x45]); // STRT CNT
    tick(&mut cpu, &mut bus, 1);
    assert!(cpu.counter_enabled);
    assert!(!cpu.timer_enabled); // timer disabled
}

// =============================================================================
// Timer increments every machine cycle
// =============================================================================

#[test]
fn test_timer_increments() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.t = 0x00;
    cpu.timer_enabled = true;
    // Timer uses 5-bit prescaler: T increments every 32 machine cycles.
    // Fill ROM with NOPs (1 cycle each).
    bus.load(0, &[0x00; 96]);
    tick(&mut cpu, &mut bus, 31);
    assert_eq!(cpu.t, 0); // prescaler at 31, T not yet incremented
    tick(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.t, 1); // prescaler wraps at 32, T increments
    tick(&mut cpu, &mut bus, 32);
    assert_eq!(cpu.t, 2); // another 32 cycles, T increments again
}

#[test]
fn test_timer_overflow_sets_flag() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.t = 0xFE;
    cpu.timer_enabled = true;
    bus.load(0, &[0x00; 128]);
    // 32 cycles → T becomes 0xFF
    tick(&mut cpu, &mut bus, 32);
    assert_eq!(cpu.t, 0xFF);
    assert!(!cpu.timer_overflow);
    // 32 more cycles → T wraps to 0x00
    tick(&mut cpu, &mut bus, 32);
    assert_eq!(cpu.t, 0x00);
    assert!(cpu.timer_overflow);
}

#[test]
fn test_timer_overflow_irq_pending() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.t = 0xFF;
    cpu.timer_enabled = true;
    cpu.tcnti_enabled = true;
    bus.load(0, &[0x00; 64]);
    // 32 cycles → prescaler fills, T wraps 0xFF → 0x00
    tick(&mut cpu, &mut bus, 32);
    assert!(cpu.timer_overflow);
    assert!(cpu.timer_irq_pending);
}

// =============================================================================
// Counter mode: counts T1 falling edges
// =============================================================================

#[test]
fn test_counter_falling_edge() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.t = 0x00;
    cpu.counter_enabled = true;
    cpu.t1_prev = true; // T1 was high

    // T1 pin at PORT_T1 (0x111): set to 0 (falling edge)
    bus.memory[0x111] = 0;
    bus.load(0, &[0x00]); // NOP
    tick(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.t, 1); // counted the falling edge
    assert!(!cpu.t1_prev);
}

#[test]
fn test_counter_no_edge() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.t = 0x00;
    cpu.counter_enabled = true;
    cpu.t1_prev = false; // T1 was low

    bus.memory[0x111] = 0; // T1 still low (no edge)
    bus.load(0, &[0x00]); // NOP
    tick(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.t, 0); // no count
}

#[test]
fn test_counter_rising_edge_no_count() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.t = 0x00;
    cpu.counter_enabled = true;
    cpu.t1_prev = false; // T1 was low

    bus.memory[0x111] = 1; // T1 goes high (rising edge)
    bus.load(0, &[0x00]); // NOP
    tick(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.t, 0); // rising edge doesn't count
    assert!(cpu.t1_prev); // updated state
}

// =============================================================================
// Interrupt enable/disable
// =============================================================================

#[test]
fn test_en_i() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x05]); // EN I
    tick(&mut cpu, &mut bus, 1);
    assert!(cpu.int_enabled);
}

#[test]
fn test_dis_i() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.int_enabled = true;
    bus.load(0, &[0x15]); // DIS I
    tick(&mut cpu, &mut bus, 1);
    assert!(!cpu.int_enabled);
}

#[test]
fn test_en_tcnti() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x25]); // EN TCNTI
    tick(&mut cpu, &mut bus, 1);
    assert!(cpu.tcnti_enabled);
}

#[test]
fn test_dis_tcnti() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.tcnti_enabled = true;
    bus.load(0, &[0x35]); // DIS TCNTI
    tick(&mut cpu, &mut bus, 1);
    assert!(!cpu.tcnti_enabled);
}

// =============================================================================
// External interrupt entry
// =============================================================================

#[test]
fn test_external_interrupt_entry() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.int_enabled = true;
    bus.irq = true;
    // Place a NOP as the instruction that would be fetched
    bus.load(0, &[0x00]);
    // Place a NOP at the interrupt vector
    bus.load(3, &[0x00]);

    // The interrupt should be detected at the instruction boundary.
    // Entry takes 3 cycles: detect + push + vector jump.
    tick(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.pc, 0x003); // external INT vector
    assert!(cpu.in_interrupt);
    // Entry blocks nesting through the in-progress latch alone; the enable
    // set by EN I survives.
    assert!(cpu.int_enabled);
    assert_eq!(cpu.psw & 0x07, 1); // SP pushed
}

#[test]
fn test_external_interrupt_retaken_after_retr_without_en_i() {
    // The Gyruss SFX handler: enable once, and return with RETR each time.
    // A second request after RETR must be taken with no EN I in between.
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    bus.load(0x000, &[0x05, 0x00, 0x00, 0x04, 0x10]); // EN I; NOP; JMP 0x010
    bus.load(0x003, &[0x04, 0x20]); // vector: JMP 0x020
    bus.load(0x010, &[0x04, 0x10]); // idle: JMP 0x010
    bus.load(0x020, &[0x93]); // handler: RETR
    tick(&mut cpu, &mut bus, 4);
    assert!(cpu.int_enabled);

    for request in 0..2 {
        bus.irq = true;
        let mut entered = false;
        for _ in 0..20 {
            tick(&mut cpu, &mut bus, 1);
            if cpu.in_interrupt {
                entered = true;
                break;
            }
        }
        assert!(entered, "request {request} was not taken");
        bus.irq = false;
        for _ in 0..20 {
            tick(&mut cpu, &mut bus, 1);
            if !cpu.in_interrupt {
                break;
            }
        }
        assert!(!cpu.in_interrupt, "request {request}: RETR did not return");
    }
}

// =============================================================================
// Timer interrupt entry
// =============================================================================

#[test]
fn test_timer_interrupt_entry() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.tcnti_enabled = true;
    cpu.timer_irq_pending = true;
    bus.load(0, &[0x00]);
    bus.load(7, &[0x00]);

    tick(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.pc, 0x007); // timer INT vector
    assert!(cpu.in_interrupt);
}

// =============================================================================
// Interrupt priority: external INT > timer
// =============================================================================

#[test]
fn test_interrupt_priority() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.int_enabled = true;
    cpu.tcnti_enabled = true;
    cpu.timer_irq_pending = true;
    bus.irq = true; // both pending
    bus.load(0, &[0x00]);
    bus.load(3, &[0x00]);
    bus.load(7, &[0x00]);

    tick(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.pc, 0x003); // external wins
}

// =============================================================================
// Interrupts blocked during interrupt (in_interrupt)
// =============================================================================

#[test]
fn test_interrupt_blocked_during_interrupt() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.int_enabled = true;
    cpu.in_interrupt = true; // already in interrupt
    bus.irq = true;
    bus.load(0, &[0x00]); // NOP
    tick(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.pc, 1); // NOP executed, no interrupt taken
}

// =============================================================================
// RETR clears in_interrupt and restores PSW
// =============================================================================

#[test]
fn test_retr_reenables_interrupts() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    // Simulate interrupt context
    cpu.in_interrupt = true;
    cpu.ram[8] = 0x10; // return PC lo
    cpu.ram[9] = 0x00; // return PC hi + PSW upper nibble
    cpu.psw = 0x01; // SP = 1
    bus.load(0, &[0x93]); // RETR
    tick(&mut cpu, &mut bus, 2);
    assert!(!cpu.in_interrupt); // cleared
    assert_eq!(cpu.pc, 0x010);
}

// =============================================================================
// SEL MB0/MB1 — 1 cycle
// =============================================================================

#[test]
fn test_sel_mb0() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.a11_pending = true;
    bus.load(0, &[0xE5]); // SEL MB0
    tick(&mut cpu, &mut bus, 1);
    assert!(!cpu.a11_pending);
}

#[test]
fn test_sel_mb1() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0xF5]); // SEL MB1
    tick(&mut cpu, &mut bus, 1);
    assert!(cpu.a11_pending);
}

// =============================================================================
// SEL RB0/RB1 — 1 cycle
// =============================================================================

#[test]
fn test_sel_rb0() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.psw = PswFlag::BS as u8;
    bus.load(0, &[0xC5]); // SEL RB0
    tick(&mut cpu, &mut bus, 1);
    assert_eq!(cpu.psw & (PswFlag::BS as u8), 0);
}

#[test]
fn test_sel_rb1() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0xD5]); // SEL RB1
    tick(&mut cpu, &mut bus, 1);
    assert_ne!(cpu.psw & (PswFlag::BS as u8), 0);
}

// =============================================================================
// Full interrupt + RETR sequence
// =============================================================================

#[test]
fn test_full_interrupt_sequence() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.int_enabled = true;
    cpu.a = 0x42;
    cpu.psw = PswFlag::CY as u8; // CY set, SP=0
    bus.irq = true;

    // Main code at 0x000: NOP (will be preempted by interrupt)
    bus.load(0, &[0x00]);
    // ISR at 0x003: RETR
    bus.load(3, &[0x93]);

    // Interrupt entry: 3 cycles (detect + push + vector)
    tick(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.pc, 0x003);
    assert!(cpu.in_interrupt);
    // PSW should be saved on stack with CY
    let saved_psw_hi = cpu.ram[9] & 0xF0;
    assert_eq!(saved_psw_hi, PswFlag::CY as u8);

    // Clear IRQ so it doesn't re-trigger
    bus.irq = false;

    // Execute RETR at 0x003: 2 cycles
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.pc, 0x000); // returns to where it was
    assert!(!cpu.in_interrupt);
    assert_ne!(cpu.psw & (PswFlag::CY as u8), 0); // CY restored
}

// =============================================================================
// Timer-driven interrupt + ISR + RETR
// =============================================================================

#[test]
fn test_timer_interrupt_full_sequence() {
    let mut cpu = I8035::new();
    let mut bus = TestBus::new();
    cpu.tcnti_enabled = true;
    cpu.timer_irq_pending = true;

    // Main code at 0x000: just NOPs
    bus.load(0, &[0x00, 0x00, 0x00]);
    // Timer ISR at 0x007: RETR
    bus.load(7, &[0x93]);

    // Interrupt entry: 3 cycles (detect + push + vector)
    tick(&mut cpu, &mut bus, 3);
    assert_eq!(cpu.pc, 0x007);
    assert!(cpu.in_interrupt);
    assert!(!cpu.timer_irq_pending); // cleared

    // Execute RETR: 2 cycles
    tick(&mut cpu, &mut bus, 2);
    assert_eq!(cpu.pc, 0x000); // returns
    assert!(!cpu.in_interrupt);
}
