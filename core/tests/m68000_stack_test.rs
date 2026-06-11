//! M68000 LEA/PEA/LINK/UNLK/MOVEM integration tests.
//!
//! Edge cases per the testing requirements: the full-32-bit (unmasked) EA
//! reaching An, PC-relative bases, the LINK A7 / UNLK A7 self-referential
//! stack-pointer cases, a LINK/UNLK frame round trip, the MOVEM
//! predecrement mask reversal, and the 68000 base-register-in-list rules.

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

// ---------------------------------------------------------------------------
// MOVEM — store direction
// ---------------------------------------------------------------------------

#[test]
fn movem_w_store_indirect_ascending() {
    // MOVEM.w D0-D1/A0,(A2): mask bit 0 = D0
    let (mut cpu, mut bus) = setup(&[0x4892, 0x0103]);
    cpu.d[0] = 0x1111_AAAA;
    cpu.d[1] = 0x2222_BBBB;
    cpu.a[0] = 0x3333_CCCC;
    cpu.a[2] = 0x3000;
    step(&mut cpu, &mut bus);
    assert_eq!(
        &bus.memory[0x3000..0x3006],
        &[0xAA, 0xAA, 0xBB, 0xBB, 0xCC, 0xCC],
        "low words stored D0, D1, A0 ascending"
    );
    assert_eq!(cpu.a[2], 0x3000, "base register not updated");
}

#[test]
fn movem_l_store_predecrement_reversed_mask() {
    // MOVEM.l D0/A6,-(A7): predec mask is reversed (bit 0 = A7 … bit 15 = D0)
    let (mut cpu, mut bus) = setup(&[0x48E7, 0x8002]);
    cpu.d[0] = 0x0D00_0D00;
    cpu.a[6] = 0x0A60_0A60;
    cpu.a[7] = 0x2000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[7], 0x1FF8, "two longs pushed");
    assert_eq!(
        &bus.memory[0x1FF8..0x2000],
        &[0x0D, 0x00, 0x0D, 0x00, 0x0A, 0x60, 0x0A, 0x60],
        "block lands in ascending register order (D0 lowest)"
    );
}

#[test]
fn movem_predecrement_stores_initial_base_register() {
    // MOVEM.w A0,-(A0): the 68000 stores the pre-decrement value
    let (mut cpu, mut bus) = setup(&[0x48A0, 0x0080]);
    cpu.a[0] = 0x3000;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[0], 0x2FFE);
    assert_eq!(
        &bus.memory[0x2FFE..0x3000],
        &[0x30, 0x00],
        "initial A0, not the decremented value"
    );
}

// ---------------------------------------------------------------------------
// MOVEM — load direction
// ---------------------------------------------------------------------------

#[test]
fn movem_w_load_sign_extends_into_full_registers() {
    // MOVEM.w (A0),D0/A1
    let (mut cpu, mut bus) = setup(&[0x4C90, 0x0201]);
    cpu.a[0] = 0x3000;
    cpu.d[0] = 0xFFFF_FFFF;
    bus.load(0x3000, &[0x80, 0x00, 0x7F, 0xFF]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xFFFF_8000, "word sign-extends into Dn too");
    assert_eq!(cpu.a[1], 0x0000_7FFF);
}

#[test]
fn movem_l_load_postincrement_updates_base() {
    // MOVEM.l (A0)+,D1/D2
    let (mut cpu, mut bus) = setup(&[0x4CD8, 0x0006]);
    cpu.a[0] = 0x3000;
    bus.load(0x3000, &[0x11, 0x11, 0x11, 0x11, 0x22, 0x22, 0x22, 0x22]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[1], 0x1111_1111);
    assert_eq!(cpu.d[2], 0x2222_2222);
    assert_eq!(cpu.a[0], 0x3008, "base advanced past the block");
}

#[test]
fn movem_postincrement_base_in_list_keeps_final_address() {
    // MOVEM.w (A0)+,D0/A0: the fetched A0 value is discarded on the 68000
    let (mut cpu, mut bus) = setup(&[0x4C98, 0x0101]);
    cpu.a[0] = 0x3000;
    bus.load(0x3000, &[0x12, 0x34, 0x56, 0x78]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0x0000_1234);
    assert_eq!(cpu.a[0], 0x3004, "increment wins over the loaded value");
}

#[test]
fn movem_load_pc_relative_and_no_flags() {
    // MOVEM.w $100(PC),D0 — mask word precedes the displacement word
    let (mut cpu, mut bus) = setup(&[0x4CBA, 0x0001, 0x0100]);
    cpu.sr = (cpu.sr & 0xFF00) | 0x1F;
    bus.load(0x1104, &[0x00, 0x42]); // base 0x1004 + 0x100
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0x0000_0042);
    assert_eq!(cpu.pc, 0x1006);
    assert_eq!(cpu.sr & 0x1F, 0x1F, "MOVEM never alters the CCR");
}
