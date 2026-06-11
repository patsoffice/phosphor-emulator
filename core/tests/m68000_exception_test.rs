//! M68000 exception-processing integration tests: TRAP/TRAPV, the illegal
//! family (ILLEGAL, line-A, line-F), divide-by-zero, and CHK.
//!
//! Covers the frame layout (SR at the lowest address, then PC), the
//! supervisor switch with SSP swap-in, trace-bit clearing, and which PC
//! each source pushes (next instruction for completed traps, the opcode
//! itself for unexecuted illegal instructions).

mod common;

use common::TestBus68k;
use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::m68000::{M68000, SrFlag};

const M: BusMaster = BusMaster::Cpu(0);

/// Program at 0x1000, supervisor stack at 0x2000, and a handler address
/// planted in the given vector.
fn setup(words: &[u16], vector: u8, handler: u32) -> (M68000, TestBus68k) {
    let mut cpu = M68000::new();
    cpu.pc = 0x1000;
    cpu.a[7] = 0x2000;
    let mut bus = TestBus68k::new();
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    bus.load(0x1000, &bytes);
    bus.load(vector as u32 * 4, &handler.to_be_bytes());
    (cpu, bus)
}

fn step(cpu: &mut M68000, bus: &mut TestBus68k) {
    let mut ticks = 0;
    while !cpu.tick_with_bus(bus, M) {
        ticks += 1;
        assert!(ticks < 200, "instruction did not complete");
    }
}

/// Assert the 6-byte exception frame at `sp`: SR word then PC long.
fn assert_frame(bus: &TestBus68k, sp: u32, sr: u16, pc: u32, ctx: &str) {
    let s = sp as usize;
    let got_sr = u16::from_be_bytes([bus.memory[s], bus.memory[s + 1]]);
    let got_pc = u32::from_be_bytes([
        bus.memory[s + 2],
        bus.memory[s + 3],
        bus.memory[s + 4],
        bus.memory[s + 5],
    ]);
    assert_eq!(got_sr, sr, "{ctx}: stacked SR");
    assert_eq!(got_pc, pc, "{ctx}: stacked PC");
}

// ---------------------------------------------------------------------------
// TRAP / TRAPV
// ---------------------------------------------------------------------------

#[test]
fn trap_pushes_frame_and_vectors() {
    let (mut cpu, mut bus) = setup(&[0x4E40], 32, 0x4000); // TRAP #0
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000, "vector 32 handler entered");
    assert_eq!(cpu.a[7], 0x1FFA, "word + long pushed");
    assert_frame(&bus, 0x1FFA, 0x2700, 0x1002, "TRAP #0");
}

#[test]
fn trap_from_user_mode_switches_to_ssp() {
    let (mut cpu, mut bus) = setup(&[0x4E4F], 47, 0x4000); // TRAP #15
    cpu.set_supervisor(false); // parks 0x2000 as SSP, activates USP
    cpu.a[7] = 0x8000; // user stack
    step(&mut cpu, &mut bus);
    assert!(cpu.flag_is_set(SrFlag::S), "supervisor mode entered");
    assert_eq!(cpu.a[7], 0x1FFA, "frame went to the supervisor stack");
    assert_eq!(cpu.usp, 0x8000, "user SP parked untouched");
    let pushed_sr = u16::from_be_bytes([bus.memory[0x1FFA], bus.memory[0x1FFB]]);
    assert_eq!(pushed_sr & 0x2000, 0, "stacked SR shows user mode");
}

#[test]
fn trap_clears_trace_bit_after_stacking_it() {
    let (mut cpu, mut bus) = setup(&[0x4E41], 33, 0x4000); // TRAP #1
    cpu.sr |= 0x8000; // T set
    step(&mut cpu, &mut bus);
    assert!(!cpu.flag_is_set(SrFlag::T), "handler runs with trace off");
    let pushed_sr = u16::from_be_bytes([bus.memory[0x1FFA], bus.memory[0x1FFB]]);
    assert_eq!(pushed_sr & 0x8000, 0x8000, "stacked SR keeps the T bit");
}

#[test]
fn trapv_taken_only_when_v_set() {
    let (mut cpu, mut bus) = setup(&[0x4E76], 7, 0x4000); // TRAPV
    cpu.set_flag(SrFlag::V, true);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000);
    assert_frame(&bus, 0x1FFA, 0x2702, 0x1002, "TRAPV taken");

    let (mut cpu, mut bus) = setup(&[0x4E76], 7, 0x4000);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1002, "V clear falls through");
    assert_eq!(cpu.a[7], 0x2000, "nothing pushed");
}

// ---------------------------------------------------------------------------
// Illegal family
// ---------------------------------------------------------------------------

#[test]
fn illegal_instruction_vectors_to_4_with_opcode_pc() {
    let (mut cpu, mut bus) = setup(&[0x4AFC], 4, 0x4000); // ILLEGAL
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000);
    assert_frame(
        &bus,
        0x1FFA,
        0x2700,
        0x1000,
        "frame PC is the unexecuted opcode",
    );
}

#[test]
fn line_a_and_line_f_have_dedicated_vectors() {
    let (mut cpu, mut bus) = setup(&[0xA123], 10, 0x4000);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000, "line-A vector 10");
    assert_frame(&bus, 0x1FFA, 0x2700, 0x1000, "line-A");

    let (mut cpu, mut bus) = setup(&[0xFEDC], 11, 0x5000);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x5000, "line-F vector 11");
}

// ---------------------------------------------------------------------------
// Divide-by-zero and CHK
// ---------------------------------------------------------------------------

#[test]
fn divide_by_zero_takes_vector_5() {
    let (mut cpu, mut bus) = setup(&[0x80C1], 5, 0x4000); // DIVU.w D1,D0
    cpu.sr |= 0x000F; // N Z V C set: zero divide clears them
    cpu.d[0] = 0x1234;
    cpu.d[1] = 0;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000);
    assert_eq!(cpu.d[0], 0x1234, "destination untouched");
    assert_eq!(cpu.sr & 0xF, 0, "N/Z/V/C cleared (hardware-verified)");
    // Unlike CHK/TRAP, the zero-divide frame PC is the divide instruction
    // itself, and the flags clear *before* stacking (both hardware-verified
    // by the suite's lone zero-divide vector).
    assert_frame(&bus, 0x1FFA, 0x2700, 0x1000, "zero divide");
}

#[test]
fn chk_traps_on_both_bounds() {
    // CHK.w D1,D0 with D0 negative: vector 6, N set
    let (mut cpu, mut bus) = setup(&[0x4181], 6, 0x4000);
    cpu.d[0] = 0x8000; // -32768
    cpu.d[1] = 0x0100;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000);
    assert!(cpu.flag_is_set(SrFlag::N), "negative operand sets N");

    // D0 greater than the bound: vector 6, N cleared
    let (mut cpu, mut bus) = setup(&[0x4181], 6, 0x4000);
    cpu.set_flag(SrFlag::N, true);
    cpu.d[0] = 0x0200;
    cpu.d[1] = 0x0100;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000);
    assert!(!cpu.flag_is_set(SrFlag::N), "too-large operand clears N");

    // In bounds: no exception
    let (mut cpu, mut bus) = setup(&[0x4181], 6, 0x4000);
    cpu.d[0] = 0x0080;
    cpu.d[1] = 0x0100;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1002, "in-bounds CHK falls through");
}
