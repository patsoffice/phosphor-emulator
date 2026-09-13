//! A bit operation on a data register costs less when the bit is in the low
//! word, and the manual says so.
//!
//! Table 8-8 marks every *register* cell of `BCHG`, `BCLR` and `BSET` with an
//! asterisk whose footnote reads "Indicates maximum value". The documented 8,
//! 10 and 8 are therefore the cost with the addressed bit in the upper word,
//! and the part is two clocks quicker when the bit is in the lower word.
//! `BTST`'s register cells carry no asterisk, so it is fixed.
//!
//! `phosphor-emulator-4sdm` is the issue. Before this, the three modifying
//! operations charged the maximum always and read about 92% exact on clock
//! count against both corpora, each spread +0 to +2 and split roughly evenly,
//! because a random bit number lands in either half.
//!
//! **These tests assert the difference between the halves rather than an
//! absolute**, and `BTST` is here as the control that must not move: it is the
//! row the manual leaves unasterisked, and a fix applied one instruction too
//! wide fails on it. Table 9-14 asterisks the same three, so the 68010 is
//! asserted to behave identically rather than being gated.

use phosphor_core::core::component::BusMasterComponent;
use phosphor_core::core::{Bus, Bus16, BusMaster, bus::InterruptState};
use phosphor_core::cpu::m68000::{M68kVariant, M68000};

const M: BusMaster = BusMaster::Cpu(0);
const PROGRAM: u32 = 0x1000;

/// Clocks and transfers, the `n(r/w)` the manual quotes.
#[derive(Debug, PartialEq, Eq)]
struct Cost {
    clocks: u32,
    reads: usize,
    writes: usize,
}

impl Cost {
    /// The manual's `n(r/w)`: total clocks, read cycles, write cycles.
    fn n(clocks: u32, reads: usize, writes: usize) -> Self {
        Self {
            clocks,
            reads,
            writes,
        }
    }
}

struct CountingBus {
    memory: Vec<u8>,
    reads: usize,
    writes: usize,
}

impl CountingBus {
    fn new() -> Self {
        Self {
            memory: vec![0; 0x10000],
            reads: 0,
            writes: 0,
        }
    }
}

impl Bus for CountingBus {
    type Address = u32;
    type Data = u16;

    fn read(&mut self, _master: BusMaster, addr: u32) -> u16 {
        self.reads += 1;
        let i = (addr & 0xFFFE) as usize;
        u16::from_be_bytes([self.memory[i], self.memory[i + 1]])
    }

    fn write(&mut self, _master: BusMaster, addr: u32, data: u16) {
        self.writes += 1;
        let i = (addr & 0xFFFE) as usize;
        self.memory[i..i + 2].copy_from_slice(&data.to_be_bytes());
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState::default()
    }
}

impl Bus16 for CountingBus {
    fn read_byte(&mut self, _master: BusMaster, addr: u32) -> u8 {
        self.reads += 1;
        self.memory[(addr & 0xFFFF) as usize]
    }

    fn write_byte(&mut self, _master: BusMaster, addr: u32, data: u8) {
        self.writes += 1;
        self.memory[(addr & 0xFFFF) as usize] = data;
    }
}

/// Run one instruction in the steady state and report what it cost.
///
/// A `NOP` runs first and is not measured: the manual's counts assume a full
/// queue, where an instruction refills behind the words it consumes rather
/// than fetching its own opcode.
fn measure(variant: M68kVariant, program: &[u16], bit: u32) -> Cost {
    let mut cpu = M68000::new();
    cpu.variant = variant;
    cpu.set_pc_flush(PROGRAM);
    let mut bus = CountingBus::new();
    let mut words = vec![0x4E71]; // NOP
    words.extend_from_slice(program);
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    bus.memory[PROGRAM as usize..PROGRAM as usize + bytes.len()].copy_from_slice(&bytes);
    cpu.d[1] = bit; // the dynamic form's bit number
    cpu.d[0] = 0x5555_5555; // destination, so every op has something to do

    let mut guard = 0;
    while !cpu.tick_with_bus(&mut bus, M) {
        guard += 1;
        assert!(guard < 100, "the priming NOP did not complete");
    }
    let (reads, writes) = (bus.reads, bus.writes);

    let mut clocks = 0;
    loop {
        clocks += 1;
        assert!(clocks < 200, "instruction did not complete");
        if cpu.tick_with_bus(&mut bus, M) {
            break;
        }
    }
    Cost {
        clocks,
        reads: bus.reads - reads,
        writes: bus.writes - writes,
    }
}

/// The same instruction with the bit low and with the bit high, on the 68000.
fn halves(program: &[u16]) -> (Cost, Cost) {
    (
        measure(M68kVariant::M68000, program, 3),
        measure(M68kVariant::M68000, program, 19),
    )
}

/// As [`halves`], for the static form, whose bit number is an extension word.
fn halves_static(opcode: u16) -> (Cost, Cost) {
    (
        measure(M68kVariant::M68000, &[opcode, 3], 0),
        measure(M68kVariant::M68000, &[opcode, 19], 0),
    )
}

// ---------------------------------------------------------------------------
// The three asterisked rows: the documented figure is the upper-word cost
// ---------------------------------------------------------------------------

#[test]
fn bchg_on_a_register_is_two_clocks_quicker_in_the_low_word() {
    // BCHG D1,D0. Table 8-8 dynamic register: 8(1/0)*.
    let (low, high) = halves(&[0x0340]);
    assert_eq!(
        high.clocks, 8,
        "the documented maximum, bit in the upper word"
    );
    assert_eq!(low.clocks, 6, "two less with the bit in the lower word");
    assert_eq!((high.reads, high.writes), (1, 0), "n(r/w) is 8(1/0)");
    assert_eq!(
        (low.reads, low.writes),
        (1, 0),
        "the transfer count is fixed"
    );
}

#[test]
fn bclr_on_a_register_is_two_clocks_quicker_in_the_low_word() {
    // BCLR D1,D0. Table 8-8 dynamic register: 10(1/0)*.
    let (low, high) = halves(&[0x0380]);
    assert_eq!(high.clocks, 10);
    assert_eq!(low.clocks, 8);
    assert_eq!((high.reads, high.writes), (1, 0), "n(r/w) is 10(1/0)");
}

#[test]
fn bset_on_a_register_is_two_clocks_quicker_in_the_low_word() {
    // BSET D1,D0. Table 8-8 dynamic register: 8(1/0)*.
    let (low, high) = halves(&[0x03C0]);
    assert_eq!(high.clocks, 8);
    assert_eq!(low.clocks, 6);
}

#[test]
fn the_static_forms_split_the_same_way_over_their_extension_word() {
    // Table 8-8 asterisks the static register cells too: BCHG 12(2/0)*,
    // BCLR 14(2/0)*, BSET 12(2/0)*. The extension word is the second read.
    for (opcode, name, max) in [
        (0x0840u16, "BCHG", 12),
        (0x0880, "BCLR", 14),
        (0x08C0, "BSET", 12),
    ] {
        let (low, high) = halves_static(opcode);
        assert_eq!(
            high.clocks, max,
            "{name} #, D0 with the bit in the upper word"
        );
        assert_eq!(
            low.clocks,
            max - 2,
            "{name} #, D0 with the bit in the lower word"
        );
        assert_eq!((high.reads, high.writes), (2, 0), "{name} static is n(2/0)");
    }
}

// ---------------------------------------------------------------------------
// The control: BTST is the row with no asterisk
// ---------------------------------------------------------------------------

#[test]
fn btst_on_a_register_costs_the_same_in_either_half() {
    // BTST D1,D0, Table 8-8 dynamic register: 6(1/0), no asterisk. **This is
    // the test that fails if the split is applied one instruction too wide.**
    let (low, high) = halves(&[0x0300]);
    assert_eq!(high.clocks, 6, "fixed at the documented figure");
    assert_eq!(low.clocks, 6, "and the low word is not cheaper");
}

#[test]
fn static_btst_on_a_register_costs_the_same_in_either_half() {
    // BTST #,D0: 10(2/0), also unasterisked.
    let (low, high) = halves_static(0x0800);
    assert_eq!(high.clocks, 10);
    assert_eq!(low.clocks, 10);
    assert_eq!((high.reads, high.writes), (2, 0));
}

// ---------------------------------------------------------------------------
// The 68010 asterisks the same three rows, so it is not gated
// ---------------------------------------------------------------------------

#[test]
fn the_68010_splits_the_same_way_because_table_9_14_asterisks_the_same_rows() {
    for (opcode, name) in [(0x0340u16, "BCHG"), (0x0380, "BCLR"), (0x03C0, "BSET")] {
        let low = measure(M68kVariant::M68010, &[opcode], 3);
        let high = measure(M68kVariant::M68010, &[opcode], 19);
        assert_eq!(
            high.clocks - low.clocks,
            2,
            "{name} splits by two clocks on the 68010 as well"
        );
        assert_eq!(
            low.clocks,
            measure(M68kVariant::M68000, &[opcode], 3).clocks,
            "{name} is not a variant difference: Table 9-14 matches Table 8-8"
        );
    }
}

// ---------------------------------------------------------------------------
// An immediate operand has no bus cycle for the test to hide behind
// ---------------------------------------------------------------------------

/// Measure a `BTST` against a memory or immediate operand.
fn btst_against(program: &[u16], setup: impl FnOnce(&mut M68000, &mut CountingBus)) -> Cost {
    let mut cpu = M68000::new();
    cpu.set_pc_flush(PROGRAM);
    let mut bus = CountingBus::new();
    let mut words = vec![0x4E71]; // NOP, to reach the steady state
    words.extend_from_slice(program);
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    bus.memory[PROGRAM as usize..PROGRAM as usize + bytes.len()].copy_from_slice(&bytes);
    cpu.d[1] = 3;
    setup(&mut cpu, &mut bus);

    let mut guard = 0;
    while !cpu.tick_with_bus(&mut bus, M) {
        guard += 1;
        assert!(guard < 100, "the priming NOP did not complete");
    }
    let (reads, writes) = (bus.reads, bus.writes);
    let mut clocks = 0;
    loop {
        clocks += 1;
        assert!(clocks < 200, "instruction did not complete");
        if cpu.tick_with_bus(&mut bus, M) {
            break;
        }
    }
    Cost {
        clocks,
        reads: bus.reads - reads,
        writes: bus.writes - writes,
    }
}

#[test]
fn btst_against_an_immediate_costs_two_clocks_more_than_the_manual_composes() {
    // BTST D1,#imm. **Both corpora record 10(2/0) and the manual composes 8**,
    // from Table 8-8's 4(1/0)+ plus Table 8-1's 4(1/0) for #<data>, which is
    // what this core charged. Two independently generated traces agreeing
    // against a composed figure settles it against the manual: 138 cases on
    // one corpus and 58 on the other, every one at 10 with no spread.
    //
    // The mechanism is the one `src_form_internal` already models for ADD: an
    // operand out of the prefetch queue runs no data bus cycle, so the test
    // needs a step of its own. See phosphor-emulator-cvux.
    let cost = btst_against(&[0x033C, 0x0004], |_, _| {});
    assert_eq!(cost, Cost::n(10, 2, 0), "BTST D1,#imm is 10(2/0)");
}

#[test]
fn btst_against_memory_still_matches_the_composed_figure() {
    // **The controls, and they are what make the row above an immediate rule
    // rather than a BTST one.** Every other BTST operand mode matches Table
    // 8-8's 4(1/0)+ plus its Table 8-1 effective-address time exactly, and a
    // change applied to BTST generally rather than to the immediate mode fails
    // here.
    for (program, name, clocks, reads) in [
        (vec![0x0311u16], "BTST D1,(A0)", 8, 2),
        (vec![0x0319], "BTST D1,(A0)+", 8, 2),
        (vec![0x0321], "BTST D1,-(A0)", 10, 2),
        (vec![0x0328, 0x0002], "BTST D1,(d16,A0)", 12, 3),
        (vec![0x0330, 0x0002], "BTST D1,(d8,A0,Xn)", 14, 3),
        (vec![0x0338, 0x3000], "BTST D1,(xxx).w", 12, 3),
        (vec![0x0339, 0x0000, 0x3000], "BTST D1,(xxx).l", 16, 4),
    ] {
        let cost = btst_against(&program, |cpu, _| {
            cpu.a[0] = 0x3000;
        });
        assert_eq!(
            cost,
            Cost::n(clocks, reads, 0),
            "{name} must keep the composed figure"
        );
    }
}
