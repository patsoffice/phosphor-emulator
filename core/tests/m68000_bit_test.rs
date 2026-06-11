//! M68000 BTST/BCHG/BCLR/BSET integration tests.
//!
//! Edge cases per the testing requirements: bit number modulo 32 for
//! data-register destinations vs modulo 8 for memory, the Z-before-modify
//! rule, both the dynamic (Dn) and static (immediate) bit-number forms,
//! and the dynamic-BTST immediate source.

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
// BTST
// ---------------------------------------------------------------------------

#[test]
fn btst_dynamic_register_sets_z_from_bit() {
    let (mut cpu, mut bus) = setup(&[0x0300]); // BTST D1,D0
    cpu.d[0] = 0x0000_0010;
    cpu.d[1] = 4;
    step(&mut cpu, &mut bus);
    assert!(!cpu.flag_is_set(SrFlag::Z), "bit 4 is set");
    assert_eq!(cpu.d[0], 0x0000_0010, "BTST never modifies");

    let (mut cpu, mut bus) = setup(&[0x0300]);
    cpu.d[0] = 0x0000_0010;
    cpu.d[1] = 5;
    step(&mut cpu, &mut bus);
    assert!(cpu.flag_is_set(SrFlag::Z), "bit 5 is clear");
}

#[test]
fn btst_register_bit_number_wraps_mod_32() {
    let (mut cpu, mut bus) = setup(&[0x0300]); // BTST D1,D0
    cpu.d[0] = 0x8000_0000;
    cpu.d[1] = 63; // 63 mod 32 = 31
    step(&mut cpu, &mut bus);
    assert!(!cpu.flag_is_set(SrFlag::Z), "bit 31 tested via mod 32");
}

#[test]
fn btst_memory_bit_number_wraps_mod_8() {
    let (mut cpu, mut bus) = setup(&[0x0310]); // BTST D1,(A0)
    cpu.a[0] = 0x3000;
    cpu.d[1] = 15; // 15 mod 8 = 7
    bus.load(0x3000, &[0x80]);
    step(&mut cpu, &mut bus);
    assert!(!cpu.flag_is_set(SrFlag::Z), "bit 7 tested via mod 8");
}

#[test]
fn btst_static_form() {
    let (mut cpu, mut bus) = setup(&[0x0810, 0x0003]); // BTST #3,(A0)
    cpu.a[0] = 0x3000;
    bus.load(0x3000, &[0x08]);
    step(&mut cpu, &mut bus);
    assert!(!cpu.flag_is_set(SrFlag::Z), "bit 3 is set");
    assert_eq!(cpu.pc, 0x1004, "bit-number extension word consumed");
}

#[test]
fn btst_dynamic_immediate_source() {
    let (mut cpu, mut bus) = setup(&[0x033C, 0x0004]); // BTST D1,#$04
    cpu.d[1] = 2;
    step(&mut cpu, &mut bus);
    assert!(!cpu.flag_is_set(SrFlag::Z), "bit 2 of the immediate byte");
}

#[test]
fn btst_does_not_touch_other_flags() {
    let (mut cpu, mut bus) = setup(&[0x0300]); // BTST D1,D0
    cpu.sr = (cpu.sr & 0xFF00) | 0x1B; // X N V C set, Z clear
    cpu.d[0] = 0;
    cpu.d[1] = 0;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.sr & 0x1F, 0x1F, "only Z changes (set), X/N/V/C kept");
}

// ---------------------------------------------------------------------------
// BCHG / BCLR / BSET
// ---------------------------------------------------------------------------

#[test]
fn bchg_register_flips_bit_and_reports_prior_state() {
    let (mut cpu, mut bus) = setup(&[0x0340]); // BCHG D1,D0
    cpu.d[0] = 0x0000_0001;
    cpu.d[1] = 0;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0, "bit 0 flipped off");
    assert!(!cpu.flag_is_set(SrFlag::Z), "Z reflects the bit before");

    let (mut cpu, mut bus) = setup(&[0x0340]);
    cpu.d[0] = 0;
    cpu.d[1] = 31;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0x8000_0000, "bit 31 flipped on");
    assert!(cpu.flag_is_set(SrFlag::Z));
}

#[test]
fn bclr_register_clears_bit() {
    let (mut cpu, mut bus) = setup(&[0x0380]); // BCLR D1,D0
    cpu.d[0] = 0xFFFF_FFFF;
    cpu.d[1] = 16;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0xFFFE_FFFF);
    assert!(!cpu.flag_is_set(SrFlag::Z), "bit was set before clearing");
}

#[test]
fn bset_register_sets_bit() {
    let (mut cpu, mut bus) = setup(&[0x03C0]); // BSET D1,D0
    cpu.d[0] = 0;
    cpu.d[1] = 7;
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.d[0], 0x0000_0080);
    assert!(cpu.flag_is_set(SrFlag::Z), "bit was clear before setting");
}

#[test]
fn static_bclr_memory_modifies_one_byte() {
    let (mut cpu, mut bus) = setup(&[0x0890, 0x0000]); // BCLR #0,(A0)
    cpu.a[0] = 0x3000;
    bus.load(0x3000, &[0xFF, 0xAA]);
    step(&mut cpu, &mut bus);
    assert_eq!(
        &bus.memory[0x3000..0x3002],
        &[0xFE, 0xAA],
        "only the addressed byte changes"
    );
    assert!(!cpu.flag_is_set(SrFlag::Z));
}

#[test]
fn static_bset_memory_via_displacement() {
    let (mut cpu, mut bus) = setup(&[0x08E8, 0x0006, 0x0001]); // BSET #6,1(A0)
    cpu.a[0] = 0x3000;
    bus.load(0x3000, &[0x00, 0x00]);
    step(&mut cpu, &mut bus);
    assert_eq!(&bus.memory[0x3000..0x3002], &[0x00, 0x40]);
    assert!(cpu.flag_is_set(SrFlag::Z), "bit was clear before");
    assert_eq!(cpu.pc, 0x1006, "bit number + displacement words consumed");
}

#[test]
fn bchg_memory_predecrement() {
    let (mut cpu, mut bus) = setup(&[0x0360]); // BCHG D1,-(A0)
    cpu.a[0] = 0x3001;
    cpu.d[1] = 1;
    bus.load(0x3000, &[0x02]);
    step(&mut cpu, &mut bus);
    assert_eq!(cpu.a[0], 0x3000, "predecrement applied");
    assert_eq!(bus.memory[0x3000], 0x00, "bit 1 flipped off");
    assert!(!cpu.flag_is_set(SrFlag::Z));
}
