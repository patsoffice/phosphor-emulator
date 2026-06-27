//! M68000 SR/CCR/USP move, RTE, STOP, and RESET integration tests.
//!
//! Covers the privilege boundary (supervisor-only instructions raise
//! vector 8 from user mode with the frame PC at the violating
//! instruction), the TRAP/RTE round trip, the user/supervisor SP swap on
//! SR writes, and the unimplemented-bit masking of SR/CCR loads.

mod common;

use common::TestBus68k;
use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::m68000::{M68kVariant, M68000, SrFlag};

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
        assert!(ticks < 200, "instruction did not complete");
    }
}

/// Drop to user mode with a user stack at 0x8000 (SSP stays parked).
fn enter_user_mode(cpu: &mut M68000) {
    cpu.set_supervisor(false);
    cpu.a[7] = 0x8000;
}

// ---------------------------------------------------------------------------
// MOVE from/to SR and CCR
// ---------------------------------------------------------------------------

#[test]
fn move_from_sr_is_unprivileged_on_68000() {
    let (mut cpu, mut bus) = setup(&[0x40C0]); // MOVE SR,D0
    enter_user_mode(&mut cpu);
    cpu.set_flag(SrFlag::C, true);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x0701, "user-mode SR readable (68000)");
    assert_eq!(cpu.pc, 0x1002, "no privilege violation");
}

#[test]
fn move_from_sr_is_privileged_on_68010() {
    // From user mode the 68010 vectors to the privilege handler instead of
    // exposing SR (the 68000 reads it freely — see the test above).
    let (mut cpu, mut bus) = setup(&[0x40C0]); // MOVE SR,D0
    cpu.variant = M68kVariant::M68010;
    bus.load(8 * 4, &0x4000u32.to_be_bytes()); // vector 8 handler
    enter_user_mode(&mut cpu);
    cpu.d[0] = 0xDEAD;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000, "privilege violation vectored");
    assert_eq!(cpu.d[0], 0xDEAD, "SR not written to the destination");
}

#[test]
fn move_from_sr_in_supervisor_mode_still_works_on_68010() {
    let (mut cpu, mut bus) = setup(&[0x40C0]); // MOVE SR,D0 (supervisor)
    cpu.variant = M68kVariant::M68010;
    cpu.set_flag(SrFlag::C, true);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0] & 0xFFFF, 0x2701, "supervisor reads SR normally");
    assert_eq!(cpu.pc, 0x1002, "no violation in supervisor mode");
}

#[test]
fn move_to_ccr_masks_unimplemented_bits() {
    let (mut cpu, mut bus) = setup(&[0x44FC, 0xFFFF]); // MOVE #$FFFF,CCR
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.sr & 0x00FF, 0x001F, "only X/N/Z/V/C stick");
    assert_eq!(cpu.sr & 0xFF00, 0x2700, "system byte untouched");
}

#[test]
fn move_to_sr_loads_whole_register_and_swaps_sp() {
    // MOVE #$0000,SR drops to user mode
    let (mut cpu, mut bus) = setup(&[0x46FC, 0x0000]);
    cpu.usp = 0x8000;
    step(&mut cpu, &mut bus);
    assert!(!cpu.flag_is_set(SrFlag::S), "user mode entered");
    assert_eq!(cpu.a[7], 0x8000, "USP swapped in");
    assert_eq!(cpu.ssp, 0x2000, "SSP parked");
}

#[test]
fn move_to_sr_in_user_mode_violates() {
    let (mut cpu, mut bus) = setup(&[0x46FC, 0x2700]); // MOVE #$2700,SR
    bus.load(8 * 4, &0x4000u32.to_be_bytes()); // vector 8 handler
    enter_user_mode(&mut cpu);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000, "privilege violation vector");
    assert_eq!(cpu.a[7], 0x1FFA, "frame on the supervisor stack");
    let pc = u32::from_be_bytes([
        bus.memory[0x1FFC],
        bus.memory[0x1FFD],
        bus.memory[0x1FFE],
        bus.memory[0x1FFF],
    ]);
    assert_eq!(pc, 0x1000, "frame PC is the violating instruction");
}

// ---------------------------------------------------------------------------
// ANDI/ORI/EORI to CCR and SR
// ---------------------------------------------------------------------------

#[test]
fn logical_immediates_to_ccr() {
    let (mut cpu, mut bus) = setup(&[0x023C, 0x0015]); // ANDI #$15,CCR
    cpu.sr = (cpu.sr & 0xFF00) | 0x1F;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.sr & 0xFF, 0x15, "ANDI keeps X and Z and C");

    let (mut cpu, mut bus) = setup(&[0x003C, 0x000A]); // ORI #$0A,CCR
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.sr & 0xFF, 0x0A, "ORI sets N and V");

    let (mut cpu, mut bus) = setup(&[0x0A3C, 0x001F]); // EORI #$1F,CCR
    cpu.sr = (cpu.sr & 0xFF00) | 0x11;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.sr & 0xFF, 0x0E, "EORI flips all five bits");
}

#[test]
fn andi_to_sr_can_drop_to_user_mode() {
    let (mut cpu, mut bus) = setup(&[0x027C, 0xDFFF]); // ANDI #$DFFF,SR (clear S)
    cpu.usp = 0x8000;
    step(&mut cpu, &mut bus);
    assert!(!cpu.flag_is_set(SrFlag::S));
    assert_eq!(cpu.a[7], 0x8000, "swap happened through write_sr");
}

#[test]
fn ori_to_sr_in_user_mode_violates() {
    let (mut cpu, mut bus) = setup(&[0x007C, 0x0700]); // ORI #$0700,SR
    bus.load(8 * 4, &0x4000u32.to_be_bytes());
    enter_user_mode(&mut cpu);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000, "privilege violation");
}

// ---------------------------------------------------------------------------
// MOVE USP
// ---------------------------------------------------------------------------

#[test]
fn move_usp_both_directions() {
    let (mut cpu, mut bus) = setup(&[0x4E61]); // MOVE A1,USP
    cpu.a[1] = 0x0000_8888;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.usp, 0x0000_8888);

    let (mut cpu, mut bus) = setup(&[0x4E6A]); // MOVE USP,A2
    cpu.usp = 0x0000_9999;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[2], 0x0000_9999);
}

#[test]
fn move_usp_in_user_mode_violates() {
    let (mut cpu, mut bus) = setup(&[0x4E61]); // MOVE A1,USP
    bus.load(8 * 4, &0x4000u32.to_be_bytes());
    enter_user_mode(&mut cpu);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000);
}

// ---------------------------------------------------------------------------
// RTE
// ---------------------------------------------------------------------------

#[test]
fn trap_rte_round_trip() {
    // TRAP #0 at 0x1000; handler at 0x4000 is a lone RTE
    let (mut cpu, mut bus) = setup(&[0x4E40]);
    bus.load(32 * 4, &0x4000u32.to_be_bytes());
    bus.load(0x4000, &[0x4E, 0x73]); // RTE
    cpu.set_flag(SrFlag::C, true);

    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000, "into the handler");
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1002, "RTE resumes after the TRAP");
    assert_eq!(cpu.a[7], 0x2000, "stack balanced");
    assert!(cpu.flag_is_set(SrFlag::C), "CCR restored from the frame");
}

#[test]
fn rte_returns_to_user_mode() {
    // Hand-built frame: SR = 0x0010 (user mode, X set), PC = 0x3000
    let (mut cpu, mut bus) = setup(&[0x4E73]); // RTE
    cpu.usp = 0x8000;
    cpu.a[7] = 0x1FFA;
    bus.load(0x1FFA, &[0x00, 0x10, 0x00, 0x00, 0x30, 0x00]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x3000);
    assert!(!cpu.flag_is_set(SrFlag::S), "dropped to user mode");
    assert_eq!(cpu.a[7], 0x8000, "USP active");
    assert_eq!(cpu.ssp, 0x2000, "SSP above the consumed frame");
    assert!(cpu.flag_is_set(SrFlag::X));
}

#[test]
fn rte_in_user_mode_violates() {
    let (mut cpu, mut bus) = setup(&[0x4E73]);
    bus.load(8 * 4, &0x4000u32.to_be_bytes());
    enter_user_mode(&mut cpu);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000);
}

// ---------------------------------------------------------------------------
// STOP / RESET / NOP
// ---------------------------------------------------------------------------

#[test]
fn stop_loads_sr_and_sleeps() {
    let (mut cpu, mut bus) = setup(&[0x4E72, 0x2300]); // STOP #$2300
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.sr, 0x2300, "immediate loaded into SR");
    assert_eq!(cpu.pc, 0x1004);
    assert!(cpu.is_sleeping(), "waiting for an interrupt");
}

#[test]
fn stop_in_user_mode_violates() {
    let (mut cpu, mut bus) = setup(&[0x4E72, 0x2700]);
    bus.load(8 * 4, &0x4000u32.to_be_bytes());
    enter_user_mode(&mut cpu);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000);
    assert!(!cpu.is_sleeping(), "violation, not a stop");
}

#[test]
fn reset_instruction_is_long_supervisor_noop() {
    let (mut cpu, mut bus) = setup(&[0x4E70]); // RESET
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1002, "CPU state unaffected");
    assert_eq!(cpu.sr, 0x2700);

    let (mut cpu, mut bus) = setup(&[0x4E70]);
    bus.load(8 * 4, &0x4000u32.to_be_bytes());
    enter_user_mode(&mut cpu);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000, "privileged in user mode");
}

#[test]
fn nop_advances_pc_only() {
    let (mut cpu, mut bus) = setup(&[0x4E71]); // NOP
    cpu.sr = (cpu.sr & 0xFF00) | 0x1F;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1002);
    assert_eq!(cpu.sr & 0x1F, 0x1F);
}
