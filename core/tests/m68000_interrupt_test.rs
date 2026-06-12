//! M68000 interrupt and address-error integration tests.
//!
//! Covers boundary sampling, level masking, the level-7 NMI edge rule,
//! autovectors vs device-supplied vectors, STOP wake-up, and the
//! seven-word address-error frame.

mod common;

use common::TestBus68k;
use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::m68000::{M68000, SrFlag};

const M: BusMaster = BusMaster::Cpu(0);

fn setup(words: &[u16]) -> (M68000, TestBus68k) {
    let mut cpu = M68000::new();
    cpu.pc = 0x1000;
    cpu.a[7] = 0x2000;
    let mut bus = TestBus68k::new();
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    bus.load(0x1000, &bytes);
    (cpu, bus)
}

fn step(cpu: &mut M68000, bus: &mut TestBus68k) {
    let mut ticks = 0;
    while !cpu.tick_with_bus(bus, M) {
        ticks += 1;
        assert!(ticks < 200, "execution did not reach a boundary");
    }
}

// ---------------------------------------------------------------------------
// Level masking and entry
// ---------------------------------------------------------------------------

#[test]
fn interrupt_taken_when_level_above_mask() {
    let (mut cpu, mut bus) = setup(&[0x4E71]); // NOP
    cpu.set_interrupt_mask(2);
    bus.load((24 + 3) * 4, &0x4000u32.to_be_bytes()); // autovector level 3
    bus.irq_level = 3;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000, "level 3 > mask 2: taken");
    assert_eq!(cpu.interrupt_mask(), 3, "mask raised to the taken level");
    assert_eq!(cpu.a[7], 0x1FFA, "frame pushed");
    let pushed_pc = u32::from_be_bytes([
        bus.memory[0x1FFC],
        bus.memory[0x1FFD],
        bus.memory[0x1FFE],
        bus.memory[0x1FFF],
    ]);
    assert_eq!(pushed_pc, 0x1000, "the NOP never executed");
}

#[test]
fn interrupt_masked_at_or_below_mask_level() {
    let (mut cpu, mut bus) = setup(&[0x4E71]); // NOP
    cpu.set_interrupt_mask(3);
    bus.irq_level = 3;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1002, "level 3 at mask 3: not taken, NOP ran");
    assert_eq!(cpu.a[7], 0x2000, "nothing pushed");
}

#[test]
fn level_7_is_edge_triggered() {
    // Mask 7 cannot mask the NMI edge
    let (mut cpu, mut bus) = setup(&[0x4E71, 0x4E71, 0x4E71]);
    cpu.set_interrupt_mask(7);
    bus.load((24 + 7) * 4, &0x4000u32.to_be_bytes());
    bus.load(0x4000, &[0x4E, 0x71, 0x4E, 0x71]); // handler: NOPs
    bus.irq_level = 7;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000, "NMI taken despite mask 7");

    // Level 7 held: no retrigger on later boundaries
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4002, "handler NOP ran, no re-entry");
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4004, "still no re-entry while level 7 holds");
}

#[test]
fn device_supplied_vector_overrides_autovector() {
    let (mut cpu, mut bus) = setup(&[0x4E71]);
    cpu.set_interrupt_mask(0);
    bus.load(0x40 * 4, &0x5000u32.to_be_bytes()); // vector 0x40
    bus.irq_level = 2;
    bus.irq_vector = 0x40;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x5000, "device vector used instead of 24+2");
}

#[test]
fn interrupt_enters_supervisor_from_user_mode() {
    let (mut cpu, mut bus) = setup(&[0x4E71]);
    cpu.set_supervisor(false);
    cpu.a[7] = 0x8000; // user stack
    cpu.set_interrupt_mask(0);
    bus.load((24 + 1) * 4, &0x4000u32.to_be_bytes());
    bus.irq_level = 1;
    step(&mut cpu, &mut bus);
    assert!(cpu.flag_is_set(SrFlag::S));
    assert_eq!(cpu.a[7], 0x1FFA, "frame on the supervisor stack");
    assert_eq!(cpu.usp, 0x8000, "user SP parked");
    let pushed_sr = u16::from_be_bytes([bus.memory[0x1FFA], bus.memory[0x1FFB]]);
    assert_eq!(pushed_sr & 0x2000, 0, "stacked SR shows user mode");
}

// ---------------------------------------------------------------------------
// STOP wake-up
// ---------------------------------------------------------------------------

#[test]
fn stop_wakes_on_interrupt() {
    let (mut cpu, mut bus) = setup(&[0x4E72, 0x2200]); // STOP #$2200 (mask 2)
    bus.load((24 + 5) * 4, &0x4000u32.to_be_bytes());
    step(&mut cpu, &mut bus);
    assert!(cpu.is_sleeping(), "stopped, waiting");

    // A masked level does not wake it
    bus.irq_level = 1;
    for _ in 0..4 {
        cpu.tick_with_bus(&mut bus, M);
    }
    assert!(cpu.is_sleeping(), "level 1 at mask 2 stays asleep");

    // An unmasked level wakes into the handler
    bus.irq_level = 5;
    step(&mut cpu, &mut bus);
    assert!(!cpu.is_sleeping());
    assert_eq!(cpu.pc, 0x4000, "woke into the level-5 handler");
    let pushed_pc = u32::from_be_bytes([
        bus.memory[0x1FFC],
        bus.memory[0x1FFD],
        bus.memory[0x1FFE],
        bus.memory[0x1FFF],
    ]);
    assert_eq!(pushed_pc, 0x1004, "frame resumes after the STOP");
}

// ---------------------------------------------------------------------------
// Address error
// ---------------------------------------------------------------------------

#[test]
fn odd_jump_target_takes_vector_3_with_full_frame() {
    let (mut cpu, mut bus) = setup(&[0x4ED0]); // JMP (A0)
    cpu.a[0] = 0x3001;
    bus.load(3 * 4, &0x4000u32.to_be_bytes());
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000, "vector 3 handler entered");
    assert_eq!(cpu.a[7], 0x2000 - 14, "seven-word group-0 frame");

    let m = &bus.memory;
    // Status word: opcode upper bits ride along on the internal bus; a
    // jump-target fault is a program-space read with I/N set (low 5 bits
    // 0x1E) — all hardware-verified against the suite vectors.
    let status = u16::from_be_bytes([m[0x1FF2], m[0x1FF3]]);
    assert_eq!(status, (0x4ED0 & 0xFFE0) | 0x1E, "program-space read fault");
    let fault = u32::from_be_bytes([m[0x1FF4], m[0x1FF5], m[0x1FF6], m[0x1FF7]]);
    assert_eq!(fault, 0x3001, "faulting access address");
    let ir = u16::from_be_bytes([m[0x1FF8], m[0x1FF9]]);
    assert_eq!(ir, 0x4ED0, "instruction register");
    let pc = u32::from_be_bytes([m[0x1FFC], m[0x1FFD], m[0x1FFE], m[0x1FFF]]);
    assert_eq!(pc, 0x3001 - 4, "control-transfer faults stack target - 4");
}

#[test]
fn odd_pc_fetch_takes_vector_3() {
    let (mut cpu, mut bus) = setup(&[]);
    cpu.pc = 0x1001;
    bus.load(3 * 4, &0x4000u32.to_be_bytes());
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000);
    assert_eq!(cpu.a[7], 0x2000 - 14, "frame pushed for the fetch fault");
}

#[test]
fn odd_write_aborts_instruction_and_records_write_fault() {
    let (mut cpu, mut bus) = setup(&[0x3080]); // MOVE.w D0,(A0)
    cpu.a[0] = 0x3001;
    cpu.d[0] = 0x1234;
    bus.load(3 * 4, &0x4000u32.to_be_bytes());
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000);
    let status = u16::from_be_bytes([bus.memory[0x1FF2], bus.memory[0x1FF3]]);
    assert_eq!(status & 0x10, 0, "R/W bit clear for a write fault");
    assert_eq!(
        &bus.memory[0x3000..0x3002],
        &[0x00, 0x00],
        "the faulting write never happened (instruction aborted)"
    );
}
