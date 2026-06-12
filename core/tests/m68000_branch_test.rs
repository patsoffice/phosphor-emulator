//! M68000 BRA/BSR/Bcc/DBcc/JMP/JSR/RTS/RTR integration tests.
//!
//! Edge cases per the testing requirements: every condition code taken and
//! not taken, both displacement widths (including backward branches), the
//! DBcc 0 -> -1 underflow, subroutine push/return round trips, and the
//! control addressing modes of JMP/JSR.

mod common;

use common::TestBus68k;
use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::m68000::{M68000, SrFlag};

const M: BusMaster = BusMaster::Cpu(0);

fn setup(words: &[u16]) -> (M68000, TestBus68k) {
    let mut cpu = M68000::new();
    cpu.pc = 0x1000;
    let mut bus = TestBus68k::new();
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    bus.load(0x1000, &bytes);
    (cpu, bus)
}

fn step(cpu: &mut M68000, bus: &mut TestBus68k) {
    let mut ticks = 0;
    while !cpu.tick_with_bus(bus, M) {
        ticks += 1;
        assert!(ticks < 100, "instruction did not complete");
    }
}

// ---------------------------------------------------------------------------
// BRA
// ---------------------------------------------------------------------------

#[test]
fn bra_byte_forward_and_backward() {
    let (mut cpu, mut bus) = setup(&[0x6008]); // BRA.s +8
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x100A, "base is the word after the opcode");

    let (mut cpu, mut bus) = setup(&[0x60FE]); // BRA.s -2 (branch to self)
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1000);
}

#[test]
fn bra_word_form() {
    let (mut cpu, mut bus) = setup(&[0x6000, 0x0100]); // BRA.w +0x100
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1102, "base excludes the displacement word");

    let (mut cpu, mut bus) = setup(&[0x6000, 0xFF00]); // BRA.w -0x100
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x0F02);
}

#[test]
fn bra_to_odd_address_takes_address_error() {
    let (mut cpu, mut bus) = setup(&[0x6003]); // BRA.s +3
    cpu.a[7] = 0x2000;
    bus.load(3 * 4, &0x4000u32.to_be_bytes()); // vector 3 handler
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000, "odd target fetch enters vector 3");
    assert_eq!(cpu.a[7], 0x2000 - 14, "group-0 frame pushed");
}

// ---------------------------------------------------------------------------
// Bcc — every condition
// ---------------------------------------------------------------------------

/// Set the CCR from individual flag bits.
fn ccr(n: bool, z: bool, v: bool, c: bool) -> u16 {
    (n as u16) << 3 | (z as u16) << 2 | (v as u16) << 1 | c as u16
}

#[test]
fn bcc_every_condition_taken_and_not_taken() {
    // (cond, mnemonic, CCR where taken, CCR where not taken)
    let cases: [(u16, &str, u16, u16); 14] = [
        (
            0x2,
            "BHI",
            ccr(false, false, false, false),
            ccr(false, false, false, true),
        ),
        (
            0x3,
            "BLS",
            ccr(false, true, false, false),
            ccr(false, false, false, false),
        ),
        (
            0x4,
            "BCC",
            ccr(false, false, false, false),
            ccr(false, false, false, true),
        ),
        (
            0x5,
            "BCS",
            ccr(false, false, false, true),
            ccr(false, false, false, false),
        ),
        (
            0x6,
            "BNE",
            ccr(false, false, false, false),
            ccr(false, true, false, false),
        ),
        (
            0x7,
            "BEQ",
            ccr(false, true, false, false),
            ccr(false, false, false, false),
        ),
        (
            0x8,
            "BVC",
            ccr(false, false, false, false),
            ccr(false, false, true, false),
        ),
        (
            0x9,
            "BVS",
            ccr(false, false, true, false),
            ccr(false, false, false, false),
        ),
        (
            0xA,
            "BPL",
            ccr(false, false, false, false),
            ccr(true, false, false, false),
        ),
        (
            0xB,
            "BMI",
            ccr(true, false, false, false),
            ccr(false, false, false, false),
        ),
        (
            0xC,
            "BGE",
            ccr(true, false, true, false),
            ccr(true, false, false, false),
        ),
        (
            0xD,
            "BLT",
            ccr(false, false, true, false),
            ccr(true, false, true, false),
        ),
        (
            0xE,
            "BGT",
            ccr(false, false, false, false),
            ccr(false, true, false, false),
        ),
        (
            0xF,
            "BLE",
            ccr(true, false, false, false),
            ccr(false, false, false, false),
        ),
    ];
    for (cond, name, sr_taken, sr_not) in cases {
        let (mut cpu, mut bus) = setup(&[0x6010 | (cond << 8)]); // Bcc.s +0x10
        cpu.sr = (cpu.sr & 0xFF00) | sr_taken;
        step(&mut cpu, &mut bus);
        assert_eq!(cpu.pc, 0x1012, "{name} taken");

        let (mut cpu, mut bus) = setup(&[0x6010 | (cond << 8)]);
        cpu.sr = (cpu.sr & 0xFF00) | sr_not;
        step(&mut cpu, &mut bus);
        assert_eq!(cpu.pc, 0x1002, "{name} not taken");
    }
}

#[test]
fn bcc_word_form_not_taken_skips_displacement() {
    let (mut cpu, mut bus) = setup(&[0x6700, 0x0100]); // BEQ.w +0x100, Z clear
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1004, "falls through past the displacement word");
}

#[test]
fn bcc_does_not_alter_flags() {
    let (mut cpu, mut bus) = setup(&[0x6702]); // BEQ.s +2
    cpu.sr = (cpu.sr & 0xFF00) | 0x1F; // X N Z V C all set
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1004, "taken");
    assert_eq!(cpu.sr & 0x1F, 0x1F, "Bcc never touches the CCR");
}

// ---------------------------------------------------------------------------
// BSR
// ---------------------------------------------------------------------------

#[test]
fn bsr_byte_pushes_return_address() {
    let (mut cpu, mut bus) = setup(&[0x6106]); // BSR.s +6
    cpu.a[7] = 0x2000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1008);
    assert_eq!(cpu.a[7], 0x1FFC, "long pushed");
    assert_eq!(
        &bus.memory[0x1FFC..0x2000],
        &[0x00, 0x00, 0x10, 0x02],
        "return address is the word after the instruction"
    );
}

#[test]
fn bsr_word_return_address_is_past_displacement() {
    let (mut cpu, mut bus) = setup(&[0x6100, 0x0200]); // BSR.w +0x200
    cpu.a[7] = 0x2000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1202);
    assert_eq!(cpu.a[7], 0x1FFC);
    assert_eq!(&bus.memory[0x1FFC..0x2000], &[0x00, 0x00, 0x10, 0x04]);
}

// ---------------------------------------------------------------------------
// DBcc
// ---------------------------------------------------------------------------

#[test]
fn dbcc_condition_true_falls_through_without_decrement() {
    let (mut cpu, mut bus) = setup(&[0x57C8, 0xFFFC]); // DBEQ D0,-4
    cpu.set_flag(SrFlag::Z, true);
    cpu.d[0] = 5;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1004, "condition true: no branch");
    assert_eq!(cpu.d[0], 5, "condition true: no decrement");
}

#[test]
fn dbcc_loops_until_counter_underflows() {
    // DBF D0,-2 branches to itself until D0.w wraps from 0 to -1
    let (mut cpu, mut bus) = setup(&[0x51C8, 0xFFFE]);
    cpu.d[0] = 0xABCD_0002;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1000, "first pass loops");
    assert_eq!(cpu.d[0], 0xABCD_0001);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1000, "second pass loops");
    assert_eq!(cpu.d[0], 0xABCD_0000);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1004, "0 -> -1 underflow falls through");
    assert_eq!(cpu.d[0], 0xABCD_FFFF, "upper word untouched by underflow");
}

// ---------------------------------------------------------------------------
// JMP / JSR
// ---------------------------------------------------------------------------

#[test]
fn jmp_address_indirect() {
    let (mut cpu, mut bus) = setup(&[0x4ED0]); // JMP (A0)
    cpu.a[0] = 0x3000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x3000);
}

#[test]
fn jmp_absolute_short_and_long() {
    let (mut cpu, mut bus) = setup(&[0x4EF8, 0x4000]); // JMP $4000.w
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000);

    let (mut cpu, mut bus) = setup(&[0x4EF9, 0x0000, 0x5000]); // JMP $5000.l
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x5000);
}

#[test]
fn jmp_pc_relative_and_indexed() {
    // JMP $100(PC): base is the extension word address (0x1002)
    let (mut cpu, mut bus) = setup(&[0x4EFA, 0x0100]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1102);

    // JMP $10(A1,D2.w)
    let (mut cpu, mut bus) = setup(&[0x4EF1, 0x2010]);
    cpu.a[1] = 0x3000;
    cpu.d[2] = 0x0000_0020;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x3030);
}

#[test]
fn jmp_to_odd_address_takes_address_error() {
    let (mut cpu, mut bus) = setup(&[0x4ED0]); // JMP (A0)
    cpu.a[0] = 0x3001;
    cpu.a[7] = 0x2000;
    bus.load(3 * 4, &0x4000u32.to_be_bytes()); // vector 3 handler
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000, "odd target fetch enters vector 3");
    assert_eq!(cpu.a[7], 0x2000 - 14, "group-0 frame pushed");
}

#[test]
fn jsr_to_odd_address_faults_before_pushing() {
    let (mut cpu, mut bus) = setup(&[0x4E90]); // JSR (A0)
    cpu.a[0] = 0x3001;
    cpu.a[7] = 0x2000;
    bus.load(3 * 4, &0x4000u32.to_be_bytes());
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000);
    assert_eq!(
        cpu.a[7],
        0x2000 - 14,
        "only the exception frame: the return address never pushed"
    );
}

#[test]
fn jsr_pushes_return_address_past_extension_words() {
    let (mut cpu, mut bus) = setup(&[0x4EA8, 0x0100]); // JSR $100(A0)
    cpu.a[0] = 0x3000;
    cpu.a[7] = 0x2000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x3100);
    assert_eq!(cpu.a[7], 0x1FFC);
    assert_eq!(
        &bus.memory[0x1FFC..0x2000],
        &[0x00, 0x00, 0x10, 0x04],
        "return address follows the displacement word"
    );
}

#[test]
fn jsr_indirect_then_rts_round_trip() {
    let (mut cpu, mut bus) = setup(&[0x4E90]); // JSR (A0)
    cpu.a[0] = 0x3000;
    cpu.a[7] = 0x2000;
    bus.load(0x3000, &[0x4E, 0x75]); // RTS
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x3000, "into the subroutine");
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x1002, "RTS returns past the JSR");
    assert_eq!(cpu.a[7], 0x2000, "stack balanced");
}

// ---------------------------------------------------------------------------
// RTS / RTR
// ---------------------------------------------------------------------------

#[test]
fn rts_pops_return_address() {
    let (mut cpu, mut bus) = setup(&[0x4E75]); // RTS
    cpu.a[7] = 0x1FFC;
    bus.load(0x1FFC, &[0x00, 0x00, 0x34, 0x56]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x3456);
    assert_eq!(cpu.a[7], 0x2000);
}

#[test]
fn rts_to_odd_address_takes_address_error() {
    let (mut cpu, mut bus) = setup(&[0x4E75]); // RTS
    cpu.a[7] = 0x1FFC;
    bus.load(0x1FFC, &[0x00, 0x00, 0x34, 0x57]);
    bus.load(3 * 4, &0x4000u32.to_be_bytes());
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x4000, "odd return address enters vector 3");
}

#[test]
fn rtr_restores_ccr_and_returns() {
    let (mut cpu, mut bus) = setup(&[0x4E77]); // RTR
    cpu.a[7] = 0x1FFA;
    // Stacked CCR word with every bit set, then the return address
    bus.load(0x1FFA, &[0xFF, 0xFF, 0x00, 0x00, 0x34, 0x56]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.pc, 0x3456);
    assert_eq!(cpu.a[7], 0x2000, "word + long popped");
    assert_eq!(
        cpu.sr & 0x00FF,
        0x001F,
        "only the five implemented CCR bits load"
    );
    assert_eq!(cpu.sr & 0xFF00, 0x2700, "system byte untouched");
}

#[test]
fn rtr_clears_ccr_from_zero_word() {
    let (mut cpu, mut bus) = setup(&[0x4E77]); // RTR
    cpu.sr = (cpu.sr & 0xFF00) | 0x1F;
    cpu.a[7] = 0x1FFA;
    bus.load(0x1FFA, &[0x00, 0x00, 0x00, 0x00, 0x10, 0x00]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.sr & 0x00FF, 0x0000, "stacked zeros clear the CCR");
    assert_eq!(cpu.pc, 0x1000);
}

#[test]
fn dbcc_does_not_alter_flags() {
    let (mut cpu, mut bus) = setup(&[0x51C8, 0x0010]); // DBF D0,+0x10
    cpu.sr = (cpu.sr & 0xFF00) | 0x1F;
    cpu.d[0] = 1;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.sr & 0x1F, 0x1F, "DBcc never touches the CCR");
    assert_eq!(cpu.pc, 0x1012, "branch taken from the displacement base");
}
