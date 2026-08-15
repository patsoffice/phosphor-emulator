//! The MC6809E drives its address bus on every cycle, including the ones it has
//! no memory access to make. These tests pin the exact address each of those
//! don't-care cycles presents, because that is what an address-snooping device
//! on the board sees — Empire Strikes Back's slapstic decides whether to switch
//! banks from it (phosphor-emulator-j63p, phosphor-emulator-vy6l).
//!
//! Every case asserts the whole bus trace, not just the don't-cares, so a cycle
//! that moves relative to a real access fails here too.

use phosphor_core::core::component::BusMasterComponent;
use phosphor_core::core::{Bus, BusMaster, bus::InterruptState};
use phosphor_core::cpu::m6809::M6809;

/// One bus cycle as a device on the board would see it.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Cycle {
    R(u16),
    W(u16, u8),
}

use Cycle::{R, W};

/// Flat 64 KB memory that records the address of every cycle.
struct RecordingBus {
    memory: [u8; 0x10000],
    cycles: Vec<Cycle>,
    irq: bool,
}

impl RecordingBus {
    fn new() -> Self {
        Self {
            memory: [0; 0x10000],
            cycles: Vec::new(),
            irq: false,
        }
    }

    fn load(&mut self, addr: u16, data: &[u8]) {
        let start = addr as usize;
        self.memory[start..start + data.len()].copy_from_slice(data);
    }
}

impl Bus for RecordingBus {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        self.cycles.push(R(addr));
        self.memory[addr as usize]
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        self.cycles.push(W(addr, data));
        self.memory[addr as usize] = data;
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            irq: self.irq,
            ..Default::default()
        }
    }
}

/// Runs one instruction from $0000 and returns its bus trace. `setup` gets the
/// CPU and bus before the first cycle; the program is loaded at $0000.
fn trace(program: &[u8], setup: impl FnOnce(&mut M6809, &mut RecordingBus)) -> Vec<Cycle> {
    let mut cpu = M6809::new();
    let mut bus = RecordingBus::new();
    bus.load(0x0000, program);
    setup(&mut cpu, &mut bus);

    // The first tick fetches; run until the CPU reports the next boundary.
    for _ in 0..64 {
        if cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)) {
            return bus.cycles;
        }
    }
    panic!("instruction did not complete within 64 cycles");
}

// ============================================================
// Address computation: direct and extended
// ============================================================

#[test]
fn direct_address_computation_drives_ffff() {
    // LDA <$10 — the cycle that forms DP:addr is a /VMA cycle.
    let t = trace(&[0x96, 0x10], |_, bus| bus.memory[0x0010] = 0x42);
    assert_eq!(t, vec![R(0x0000), R(0x0001), R(0xFFFF), R(0x0010)]);
}

#[test]
fn extended_address_computation_drives_ffff() {
    // LDA $1234
    let t = trace(&[0xB6, 0x12, 0x34], |_, bus| bus.memory[0x1234] = 0x42);
    assert_eq!(
        t,
        vec![R(0x0000), R(0x0001), R(0x0002), R(0xFFFF), R(0x1234)]
    );
}

// ============================================================
// Inherent operations
// ============================================================

#[test]
fn inherent_alu_drives_pc() {
    // NEGA — its one execute cycle re-drives PC.
    let t = trace(&[0x40], |cpu, _| cpu.a = 0x01);
    assert_eq!(t, vec![R(0x0000), R(0x0001)]);
}

#[test]
fn inherent_tst_drives_ffff_not_pc() {
    // TSTA — TST is the exception among the inherent ops: /VMA, not PC.
    let t = trace(&[0x4D], |cpu, _| cpu.a = 0x01);
    assert_eq!(t, vec![R(0x0000), R(0xFFFF)]);
}

#[test]
fn mul_drives_pc_once_then_ffff() {
    // MUL — one don't-care at PC, then nine /VMA while the multiply runs.
    let t = trace(&[0x3D], |cpu, _| {
        cpu.a = 0x10;
        cpu.b = 0x10;
    });
    let mut expected = vec![R(0x0000), R(0x0001)];
    expected.extend(std::iter::repeat_n(R(0xFFFF), 9));
    assert_eq!(t, expected);
}

// ============================================================
// Read-modify-write: the modify cycle, and TST's missing write-back
// ============================================================

#[test]
fn rmw_modify_cycle_drives_pc_and_writes_back() {
    // NEG <$10 — read, don't-care at PC, write.
    let t = trace(&[0x00, 0x10], |_, bus| bus.memory[0x0010] = 0x01);
    assert_eq!(
        t,
        vec![
            R(0x0000),
            R(0x0001),
            R(0xFFFF),
            R(0x0010),
            R(0x0002),
            W(0x0010, 0xFF),
        ]
    );
}

#[test]
fn tst_direct_ends_on_two_ffff_cycles() {
    // TST <$10 shares RMW timing but stores nothing: its last two cycles are
    // don't-cares at $FFFF, which is what commit 9706bbc had made silent.
    let t = trace(&[0x0D, 0x10], |_, bus| bus.memory[0x0010] = 0x01);
    assert_eq!(
        t,
        vec![
            R(0x0000),
            R(0x0001),
            R(0xFFFF),
            R(0x0010),
            R(0xFFFF),
            R(0xFFFF),
        ]
    );
}

#[test]
fn tst_indexed_ends_on_two_ffff_cycles() {
    let t = trace(&[0x6D, 0x84], |cpu, bus| {
        cpu.x = 0x2000;
        bus.memory[0x2000] = 0x01;
    });
    assert_eq!(
        t,
        vec![
            R(0x0000),
            R(0x0001),
            R(0x0002), // ,R has a single don't-care, and it re-drives PC
            R(0x2000),
            R(0xFFFF),
            R(0xFFFF),
        ]
    );
}

// ============================================================
// Indexed addressing: the PC/$FFFF split is per mode
// ============================================================

#[test]
fn indexed_5bit_offset_drives_pc_then_ffff() {
    // LDA 0,X via the 5-bit postbyte: one don't-care at PC, then one /VMA.
    // Those two are the alt-start and alt-valid accesses ESB's slapstic needs.
    let t = trace(&[0xA6, 0x00], |cpu, bus| {
        cpu.x = 0x2000;
        bus.memory[0x2000] = 0x42;
    });
    assert_eq!(
        t,
        vec![R(0x0000), R(0x0001), R(0x0002), R(0xFFFF), R(0x2000)]
    );
}

#[test]
fn indexed_no_offset_drives_pc_only() {
    // LDA ,X (postbyte $84). Its whole overhead is one re-fetch at PC — there
    // is no /VMA cycle to be had, so the base don't-care is the PC one.
    let t = trace(&[0xA6, 0x84], |cpu, bus| {
        cpu.x = 0x2000;
        bus.memory[0x2000] = 0x42;
    });
    assert_eq!(t, vec![R(0x0000), R(0x0001), R(0x0002), R(0x2000)]);
}

#[test]
fn indexed_accumulator_d_offset_drives_pc_twice() {
    // LDA D,X (postbyte $8B) — the one mode with two PC don't-cares, at PC+0
    // and PC+1, followed by three /VMA.
    let t = trace(&[0xA6, 0x8B], |cpu, bus| {
        cpu.x = 0x2000;
        cpu.a = 0x00;
        cpu.b = 0x10;
        bus.memory[0x2010] = 0x42;
    });
    assert_eq!(
        t,
        vec![
            R(0x0000),
            R(0x0001),
            R(0x0002),
            R(0x0003),
            R(0xFFFF),
            R(0xFFFF),
            R(0xFFFF),
            R(0x2010),
        ]
    );
}

#[test]
fn indexed_16bit_offset_drives_ffff_only() {
    // LDA $0010,X (postbyte $89): the offset already came from the instruction
    // stream, so none of the don't-cares re-drive PC.
    let t = trace(&[0xA6, 0x89, 0x00, 0x10], |cpu, bus| {
        cpu.x = 0x2000;
        bus.memory[0x2010] = 0x42;
    });
    assert_eq!(
        t,
        vec![
            R(0x0000),
            R(0x0001),
            R(0x0002),
            R(0x0003),
            R(0xFFFF),
            R(0xFFFF),
            R(0xFFFF),
            R(0x2010),
        ]
    );
}

// ============================================================
// Indexed indirect: don't-cares first, pointer read, one /VMA
// ============================================================
//
// An indirect postbyte forms its base address, spends that mode's don't-care
// cycles, *then* reads the two pointer bytes, then holds $FFFF for one final
// cycle. The don't-cares never trail the pointer read — a slapstic watching the
// address bus decides on exactly that ordering (phosphor-emulator-5dgc).

#[test]
fn indexed_indirect_no_offset_reads_pointer_after_its_dont_care() {
    // LDA [,X] (postbyte $94): one don't-care at PC, the pointer, one /VMA.
    let t = trace(&[0xA6, 0x94], |cpu, bus| {
        cpu.x = 0x2000;
        bus.load(0x2000, &[0x30, 0x00]);
        bus.memory[0x3000] = 0x42;
    });
    assert_eq!(
        t,
        vec![
            R(0x0000),
            R(0x0001),
            R(0x0002), // the mode's don't-care, ahead of the pointer
            R(0x2000), // pointer high
            R(0x2001), // pointer low
            R(0xFFFF), // the single trailing /VMA
            R(0x3000),
        ]
    );
}

#[test]
fn indexed_indirect_accumulator_offset_spends_both_dont_cares_first() {
    // LDA [B,X] (postbyte $95): B,R's two don't-cares both precede the pointer.
    let t = trace(&[0xA6, 0x95], |cpu, bus| {
        cpu.x = 0x2000;
        cpu.b = 0x10;
        bus.load(0x2010, &[0x30, 0x00]);
        bus.memory[0x3000] = 0x42;
    });
    assert_eq!(
        t,
        vec![
            R(0x0000),
            R(0x0001),
            R(0x0002),
            R(0xFFFF),
            R(0x2010),
            R(0x2011),
            R(0xFFFF),
            R(0x3000),
        ]
    );
}

#[test]
fn indexed_indirect_d_offset_keeps_both_pc_dont_cares_ahead_of_the_pointer() {
    // LDA [D,X] (postbyte $9B) — the mode with two PC don't-cares. Both land
    // before the pointer read, which is where the old ordering differed most.
    let t = trace(&[0xA6, 0x9B], |cpu, bus| {
        cpu.x = 0x2000;
        cpu.a = 0x00;
        cpu.b = 0x10;
        bus.load(0x2010, &[0x30, 0x00]);
        bus.memory[0x3000] = 0x42;
    });
    assert_eq!(
        t,
        vec![
            R(0x0000),
            R(0x0001),
            R(0x0002), // PC+0
            R(0x0003), // PC+1
            R(0xFFFF),
            R(0xFFFF),
            R(0xFFFF),
            R(0x2010),
            R(0x2011),
            R(0xFFFF),
            R(0x3000),
        ]
    );
}

#[test]
fn indexed_indirect_auto_increment_spends_four_dont_cares_first() {
    // LDA [,X++] (postbyte $91): four don't-cares, then the pointer, then /VMA.
    let t = trace(&[0xA6, 0x91], |cpu, bus| {
        cpu.x = 0x2000;
        bus.load(0x2000, &[0x30, 0x00]);
        bus.memory[0x3000] = 0x42;
    });
    assert_eq!(
        t,
        vec![
            R(0x0000),
            R(0x0001),
            R(0x0002),
            R(0xFFFF),
            R(0xFFFF),
            R(0xFFFF),
            R(0x2000),
            R(0x2001),
            R(0xFFFF),
            R(0x3000),
        ]
    );
}

#[test]
fn indexed_indirect_16bit_offset_drives_ffff_only() {
    // LDA [$0010,X] (postbyte $99): the offset came from the instruction
    // stream, so none of the three leading don't-cares re-drive PC.
    let t = trace(&[0xA6, 0x99, 0x00, 0x10], |cpu, bus| {
        cpu.x = 0x2000;
        bus.load(0x2010, &[0x30, 0x00]);
        bus.memory[0x3000] = 0x42;
    });
    assert_eq!(
        t,
        vec![
            R(0x0000),
            R(0x0001),
            R(0x0002),
            R(0x0003),
            R(0xFFFF),
            R(0xFFFF),
            R(0xFFFF),
            R(0x2010),
            R(0x2011),
            R(0xFFFF),
            R(0x3000),
        ]
    );
}

#[test]
fn extended_indirect_brackets_its_pointer_read_with_one_ffff_each_side() {
    // LDA [$2000] (postbyte $9F).
    let t = trace(&[0xA6, 0x9F, 0x20, 0x00], |_, bus| {
        bus.load(0x2000, &[0x30, 0x00]);
        bus.memory[0x3000] = 0x42;
    });
    assert_eq!(
        t,
        vec![
            R(0x0000),
            R(0x0001),
            R(0x0002),
            R(0x0003),
            R(0xFFFF),
            R(0x2000),
            R(0x2001),
            R(0xFFFF),
            R(0x3000),
        ]
    );
}

#[test]
fn jmp_indirect_lands_on_the_trailing_ffff() {
    // JMP [,X] (0x6E $94) — JMP has no execute cycle of its own, so the last
    // cycle of the whole instruction is the address formation's final /VMA.
    let t = trace(&[0x6E, 0x94], |cpu, bus| {
        cpu.x = 0x2000;
        bus.load(0x2000, &[0x30, 0x00]);
    });
    assert_eq!(
        t,
        vec![
            R(0x0000),
            R(0x0001),
            R(0x0002),
            R(0x2000),
            R(0x2001),
            R(0xFFFF),
        ]
    );
}

#[test]
fn store_indirect_holds_ffff_between_the_pointer_and_the_write() {
    // STA [,X] (0xA7 $94) — the trailing /VMA sits between the pointer read and
    // the store, not after it.
    let t = trace(&[0xA7, 0x94], |cpu, bus| {
        cpu.x = 0x2000;
        cpu.a = 0x42;
        bus.load(0x2000, &[0x30, 0x00]);
    });
    assert_eq!(
        t,
        vec![
            R(0x0000),
            R(0x0001),
            R(0x0002),
            R(0x2000),
            R(0x2001),
            R(0xFFFF),
            W(0x3000, 0x42),
        ]
    );
}

// ============================================================
// Branches, subroutine calls, stack
// ============================================================

#[test]
fn short_branch_drives_ffff() {
    let t = trace(&[0x20, 0x02], |_, _| {}); // BRA +2
    assert_eq!(t, vec![R(0x0000), R(0x0001), R(0xFFFF)]);
}

#[test]
fn jsr_extended_drives_ffff_pc_ffff_before_pushing() {
    let t = trace(&[0xBD, 0x12, 0x34], |cpu, _| cpu.s = 0x2000);
    assert_eq!(
        t,
        vec![
            R(0x0000),
            R(0x0001),
            R(0x0002),
            R(0xFFFF),
            R(0x0003),
            R(0xFFFF),
            W(0x1FFF, 0x03),
            W(0x1FFE, 0x00),
        ]
    );
}

#[test]
fn rts_opens_at_pc_and_closes_on_the_settled_stack() {
    let t = trace(&[0x39], |cpu, bus| {
        cpu.s = 0x2000;
        bus.load(0x2000, &[0x12, 0x34]);
    });
    assert_eq!(
        t,
        vec![R(0x0000), R(0x0001), R(0x2000), R(0x2001), R(0x2002)]
    );
}

#[test]
fn pshs_probes_the_stack_top_before_writing() {
    // PSHS A
    let t = trace(&[0x34, 0x02], |cpu, _| {
        cpu.s = 0x2000;
        cpu.a = 0x42;
    });
    assert_eq!(
        t,
        vec![
            R(0x0000),
            R(0x0001),
            R(0xFFFF),
            R(0xFFFF),
            R(0x2000),
            W(0x1FFF, 0x42),
        ]
    );
}

#[test]
fn puls_closes_on_the_settled_stack() {
    // PULS A
    let t = trace(&[0x35, 0x02], |cpu, bus| {
        cpu.s = 0x2000;
        bus.memory[0x2000] = 0x42;
    });
    assert_eq!(
        t,
        vec![
            R(0x0000),
            R(0x0001),
            R(0xFFFF),
            R(0xFFFF),
            R(0x2000),
            R(0x2001),
        ]
    );
}

// ============================================================
// Interrupt entry
// ============================================================

#[test]
fn irq_entry_drives_pc_twice_then_ffff() {
    // The cycle that detects the interrupt is itself a don't-care at PC, so the
    // entry opens PC, PC, /VMA — then twelve pushes, a /VMA, the vector, a /VMA.
    let mut cpu = M6809::new();
    let mut bus = RecordingBus::new();
    bus.load(0x0000, &[0x12]); // NOP, never executed
    bus.load(0xFFF8, &[0x40, 0x00]); // IRQ vector
    cpu.s = 0x2000;
    bus.irq = true;

    // Exactly the 19 cycles of the entry: 1 detect + 18 execute.
    for _ in 0..19 {
        cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    }
    assert_eq!(cpu.pc, 0x4000, "IRQ never vectored");

    let t = bus.cycles;
    assert_eq!(&t[..3], &[R(0x0000), R(0x0000), R(0xFFFF)]);
    assert!(
        t[3..15].iter().all(|c| matches!(c, W(_, _))),
        "expected 12 pushes, got {:?}",
        &t[3..15]
    );
    assert_eq!(
        &t[15..19],
        &[R(0xFFFF), R(0xFFF8), R(0xFFF9), R(0xFFFF)],
        "vector fetch should be bracketed by /VMA cycles"
    );
    assert_eq!(t.len(), 19, "IRQ entry is 19 cycles");
}
