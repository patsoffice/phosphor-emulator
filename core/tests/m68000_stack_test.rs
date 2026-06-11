//! M68000 LEA/PEA/LINK/UNLK integration tests.
//!
//! Edge cases per the testing requirements: the full-32-bit (unmasked) EA
//! reaching An, PC-relative bases, the LINK A7 / UNLK A7 self-referential
//! stack-pointer cases, and a LINK/UNLK frame round trip.

mod common;

use common::TestBus68k;
use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::m68000::M68000;

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
// LEA
// ---------------------------------------------------------------------------

#[test]
fn lea_displacement_loads_computed_address() {
    let (mut cpu, mut bus) = setup(&[0x43E8, 0x0100]); // LEA $100(A0),A1
    cpu.a[0] = 0x3000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[1], 0x3100);
    assert_eq!(cpu.a[0], 0x3000, "source register untouched");
}

#[test]
fn lea_absolute_short_keeps_full_sign_extension() {
    let (mut cpu, mut bus) = setup(&[0x45F8, 0x8000]); // LEA $8000.w,A2
    step(&mut cpu, &mut bus);
    assert_eq!(
        cpu.a[2], 0xFFFF_8000,
        "An receives the unmasked 32-bit address"
    );
}

#[test]
fn lea_pc_relative_base_is_extension_word() {
    let (mut cpu, mut bus) = setup(&[0x47FA, 0x0100]); // LEA $100(PC),A3
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[3], 0x1102);
}

#[test]
fn lea_indexed_and_flags_untouched() {
    let (mut cpu, mut bus) = setup(&[0x41F1, 0x2004]); // LEA 4(A1,D2.w),A0
    cpu.sr = (cpu.sr & 0xFF00) | 0x1F;
    cpu.a[1] = 0x3000;
    cpu.d[2] = 0x10;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[0], 0x3014);
    assert_eq!(cpu.sr & 0x1F, 0x1F, "LEA never alters the CCR");
}

// ---------------------------------------------------------------------------
// PEA
// ---------------------------------------------------------------------------

#[test]
fn pea_pushes_effective_address() {
    let (mut cpu, mut bus) = setup(&[0x4850]); // PEA (A0)
    cpu.a[0] = 0x0012_3456;
    cpu.a[7] = 0x2000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[7], 0x1FFC);
    assert_eq!(&bus.memory[0x1FFC..0x2000], &[0x00, 0x12, 0x34, 0x56]);
}

#[test]
fn pea_pc_relative() {
    let (mut cpu, mut bus) = setup(&[0x487A, 0xFF00]); // PEA -$100(PC)
    cpu.a[7] = 0x2000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[7], 0x1FFC);
    assert_eq!(
        &bus.memory[0x1FFC..0x2000],
        &[0x00, 0x00, 0x0F, 0x02],
        "base 0x1002 - 0x100"
    );
}

// ---------------------------------------------------------------------------
// LINK / UNLK
// ---------------------------------------------------------------------------

#[test]
fn link_builds_frame_and_reserves_locals() {
    let (mut cpu, mut bus) = setup(&[0x4E56, 0xFFF8]); // LINK A6,#-8
    cpu.a[6] = 0xAAAA_AAAA;
    cpu.a[7] = 0x2000;
    step(&mut cpu, &mut bus);
    assert_eq!(
        &bus.memory[0x1FFC..0x2000],
        &[0xAA, 0xAA, 0xAA, 0xAA],
        "old frame pointer saved"
    );
    assert_eq!(cpu.a[6], 0x1FFC, "frame pointer at the saved slot");
    assert_eq!(cpu.a[7], 0x1FF4, "8 bytes of locals reserved");
}

#[test]
fn link_a7_pushes_decremented_sp() {
    let (mut cpu, mut bus) = setup(&[0x4E57, 0xFFFC]); // LINK A7,#-4
    cpu.a[7] = 0x2000;
    step(&mut cpu, &mut bus);
    assert_eq!(
        &bus.memory[0x1FFC..0x2000],
        &[0x00, 0x00, 0x1F, 0xFC],
        "the pushed value is the post-push SP"
    );
    assert_eq!(cpu.a[7], 0x1FF8, "frame slot + displacement");
}

#[test]
fn unlk_collapses_frame() {
    let (mut cpu, mut bus) = setup(&[0x4E5E]); // UNLK A6
    cpu.a[6] = 0x1FFC;
    cpu.a[7] = 0x1FF4; // locals below the frame
    bus.load(0x1FFC, &[0xAA, 0xAA, 0xAA, 0xAA]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[6], 0xAAAA_AAAA, "old frame pointer restored");
    assert_eq!(cpu.a[7], 0x2000, "stack back above the frame slot");
}

#[test]
fn unlk_a7_loads_popped_value() {
    let (mut cpu, mut bus) = setup(&[0x4E5F]); // UNLK A7
    cpu.a[7] = 0x1FFC;
    bus.load(0x1FFC, &[0x00, 0x00, 0x30, 0x00]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[7], 0x3000, "the load wins over the pop increment");
}

#[test]
fn link_unlk_round_trip() {
    // LINK A6,#-4 then UNLK A6 restores both registers
    let (mut cpu, mut bus) = setup(&[0x4E56, 0xFFFC, 0x4E5E]);
    cpu.a[6] = 0xDEAD_BEEF;
    cpu.a[7] = 0x2000;
    step(&mut cpu, &mut bus);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[6], 0xDEAD_BEEF);
    assert_eq!(cpu.a[7], 0x2000);
}
