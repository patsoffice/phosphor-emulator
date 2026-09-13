//! Where the queue refill falls among an instruction's own bus cycles.
//!
//! The read-modify-write families hand their refill to the bus unit before the
//! write, so it lands on a clock of its own between the operand read and the
//! result: `CLR.w (A4)` is a read, a program read, then a write. For a long
//! result the refill sits in front of *both* result words, which is what
//! `NEG.l` records.
//!
//! **`ADDX.l` and `SUBX.l` in their `-(Ay),-(Ax)` form are the exception**, and
//! both vector corpora agree on it: the refill falls *between* the two write
//! words. `phosphor-emulator-7wmg` is the issue, and before it was fixed these
//! two instructions read 82.55% and 82.49% on transfer kinds against the
//! documentation-derived corpus and 82.96% and 83.68% against the
//! microcode-derived one, on 834 cases there and 2,819 here, with the total
//! clock count and the transfer count both already exact. Only a comparison of
//! the sequence could see it.
//!
//! These tests assert the order, not the total, and the last two exist to fail
//! if the exception is ever generalized to the families that do not share it.

use phosphor_core::core::component::BusMasterComponent;
use phosphor_core::core::{Bus, Bus16, BusMaster, bus::InterruptState};
use phosphor_core::cpu::m68000::M68000;

const M: BusMaster = BusMaster::Cpu(0);

/// The program is assembled here; operands live at [`OPERANDS`] and above, so
/// an access can be attributed to one or the other by its address alone.
const PROGRAM: u32 = 0x1000;
const OPERANDS: u32 = 0x3000;

/// One access, in the order the CPU issued it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    Read(u32),
    Write(u32),
}

impl Access {
    fn addr(self) -> u32 {
        match self {
            Access::Read(a) | Access::Write(a) => a,
        }
    }

    /// `R` and `W` for an operand access, `P` for a program read: the queue
    /// refill this file is about. The corpora write a refill as an ordinary
    /// read, so the shape `R.R.R.R.W.P.W` here is the `R.R.R.R.W.R.W` the
    /// issue quotes.
    fn symbol(self) -> &'static str {
        match self {
            Access::Read(a) if a < OPERANDS => "P",
            Access::Read(_) => "R",
            Access::Write(_) => "W",
        }
    }
}

/// A bus that records the order of what it is asked to do.
struct OrderedBus {
    memory: Vec<u8>,
    log: Vec<Access>,
}

impl OrderedBus {
    fn new() -> Self {
        Self {
            memory: vec![0; 0x10000],
            log: Vec::new(),
        }
    }

    fn load(&mut self, addr: u32, bytes: &[u8]) {
        let at = addr as usize;
        self.memory[at..at + bytes.len()].copy_from_slice(bytes);
    }

    /// The access sequence, as `R`/`W`/`P` joined by dots.
    fn shape(&self) -> String {
        self.log
            .iter()
            .map(|a| a.symbol())
            .collect::<Vec<_>>()
            .join(".")
    }

    /// The addresses written, in the order they were driven.
    fn write_addrs(&self) -> Vec<u32> {
        self.log
            .iter()
            .filter(|a| matches!(a, Access::Write(_)))
            .map(|a| a.addr())
            .collect()
    }
}

impl Bus for OrderedBus {
    type Address = u32;
    type Data = u16;

    fn read(&mut self, _master: BusMaster, addr: u32) -> u16 {
        let a = addr & 0xFFFE;
        self.log.push(Access::Read(a));
        let i = a as usize;
        u16::from_be_bytes([self.memory[i], self.memory[i + 1]])
    }

    fn write(&mut self, _master: BusMaster, addr: u32, data: u16) {
        let a = addr & 0xFFFE;
        self.log.push(Access::Write(a));
        let i = a as usize;
        self.memory[i..i + 2].copy_from_slice(&data.to_be_bytes());
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState::default()
    }
}

impl Bus16 for OrderedBus {
    fn read_byte(&mut self, _master: BusMaster, addr: u32) -> u8 {
        self.log.push(Access::Read(addr));
        self.memory[(addr & 0xFFFF) as usize]
    }

    fn write_byte(&mut self, _master: BusMaster, addr: u32, data: u8) {
        self.log.push(Access::Write(addr));
        self.memory[(addr & 0xFFFF) as usize] = data;
    }
}

/// Run one instruction in the steady state and hand back the recorded order.
///
/// **A `NOP` runs first and is not recorded.** An instruction in the steady
/// state does not fetch its own opcode; it refills behind the words it
/// consumes. Starting from a flushed queue would put the two fetches that fill
/// the queue in front of the sequence under test, so the NOP absorbs them, and
/// the log is cleared at the instruction boundary rather than part way into it.
fn run_one(program: &[u16], setup: impl FnOnce(&mut M68000, &mut OrderedBus)) -> OrderedBus {
    let mut cpu = M68000::new();
    cpu.set_pc_flush(PROGRAM);
    let mut bus = OrderedBus::new();
    let mut words = vec![0x4E71]; // NOP
    words.extend_from_slice(program);
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    bus.load(PROGRAM, &bytes);
    setup(&mut cpu, &mut bus);

    let mut guard = 0;
    while !cpu.tick_with_bus(&mut bus, M) {
        guard += 1;
        assert!(guard < 100, "the priming NOP did not complete");
    }

    bus.log.clear();
    let mut guard = 0;
    while !cpu.tick_with_bus(&mut bus, M) {
        guard += 1;
        assert!(guard < 400, "instruction did not complete");
    }
    bus
}

// ---------------------------------------------------------------------------
// The refill between the two write words: ADDX.l and SUBX.l
// ---------------------------------------------------------------------------

#[test]
fn addx_l_refills_between_its_two_write_words() {
    // ADDX.l -(A2),-(A3), the issue's own example encoding.
    let bus = run_one(&[0xD78A], |cpu, bus| {
        cpu.a[2] = OPERANDS + 0x10; // source
        cpu.a[3] = OPERANDS + 0x20; // destination
        bus.load(OPERANDS + 0x0C, &[0x00, 0x00, 0x00, 0x01]);
        bus.load(OPERANDS + 0x1C, &[0x00, 0x00, 0x00, 0x02]);
    });

    assert_eq!(
        bus.shape(),
        "R.R.R.R.W.P.W",
        "four operand reads, the low result word, the refill, then the high word"
    );
    assert_eq!(
        bus.write_addrs(),
        vec![OPERANDS + 0x1E, OPERANDS + 0x1C],
        "the low half goes back first, so the refill sits between low and high"
    );
}

#[test]
fn subx_l_refills_between_its_two_write_words() {
    // SUBX.l -(A1),-(A7), the issue's other example encoding.
    let bus = run_one(&[0x9F89], |cpu, bus| {
        cpu.a[1] = OPERANDS + 0x110; // source
        cpu.a[7] = OPERANDS + 0x120; // destination
        bus.load(OPERANDS + 0x10C, &[0x00, 0x00, 0x00, 0x01]);
        bus.load(OPERANDS + 0x11C, &[0x00, 0x00, 0x00, 0x05]);
    });

    assert_eq!(bus.shape(), "R.R.R.R.W.P.W");
    assert_eq!(bus.write_addrs(), vec![OPERANDS + 0x11E, OPERANDS + 0x11C]);
}

// ---------------------------------------------------------------------------
// The two controls: this is an exception, not a new rule
// ---------------------------------------------------------------------------

#[test]
fn addx_w_keeps_its_refill_in_front_of_its_single_write() {
    // ADDX.w -(A2),-(A3). A word result is one transaction, so there is no
    // "between" to fall into and the refill stays in front of it. This is why
    // the word and byte forms were already exact and only the long forms moved.
    let bus = run_one(&[0xD74A], |cpu, bus| {
        cpu.a[2] = OPERANDS + 0x10;
        cpu.a[3] = OPERANDS + 0x20;
        bus.load(OPERANDS + 0x0E, &[0x00, 0x01]);
        bus.load(OPERANDS + 0x1E, &[0x00, 0x02]);
    });

    assert_eq!(bus.shape(), "R.R.P.W");
}

#[test]
fn neg_l_keeps_its_refill_in_front_of_both_write_words() {
    // NEG.l (A5). The ordinary read-modify-write rule, which the recorded
    // traces are equally unambiguous about: the program read comes before both
    // words of the long write. **If the ADDX/SUBX placement is ever
    // generalized to the rest of the read-modify-write families, this fails.**
    let bus = run_one(&[0x4495], |cpu, bus| {
        cpu.a[5] = OPERANDS + 0x40;
        bus.load(OPERANDS + 0x40, &[0x00, 0x00, 0x00, 0x07]);
    });

    assert_eq!(
        bus.shape(),
        "R.R.P.W.W",
        "the refill precedes both result words for an ordinary RMW long write"
    );
    assert_eq!(
        bus.write_addrs(),
        vec![OPERANDS + 0x42, OPERANDS + 0x40],
        "still low half first, which is the RMW write order either way"
    );
}

// ---------------------------------------------------------------------------
// Two refills in one instruction land on separate clocks
// ---------------------------------------------------------------------------

/// Clocks at which each access was driven, relative to the instruction's first.
fn access_clocks(program: &[u16], setup: impl FnOnce(&mut M68000, &mut OrderedBus)) -> Vec<u32> {
    let mut cpu = M68000::new();
    cpu.set_pc_flush(PROGRAM);
    let mut bus = OrderedBus::new();
    let mut words = vec![0x4E71]; // NOP, to reach the steady state
    words.extend_from_slice(program);
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    bus.load(PROGRAM, &bytes);
    setup(&mut cpu, &mut bus);

    let mut guard = 0;
    while !cpu.tick_with_bus(&mut bus, M) {
        guard += 1;
        assert!(guard < 100, "the priming NOP did not complete");
    }

    // One tick is one clock. Record the clock each access was driven on by
    // watching the log grow.
    bus.log.clear();
    let mut clocks = Vec::new();
    let mut seen = 0;
    for clock in 0..400u32 {
        let done = cpu.tick_with_bus(&mut bus, M);
        while seen < bus.log.len() {
            clocks.push(clock);
            seen += 1;
        }
        if done {
            break;
        }
    }
    clocks
}

#[test]
fn two_program_reads_in_one_instruction_do_not_share_a_clock() {
    // `MOVEM.l #, (xxx).l` consumes three instruction-stream words before its
    // first write: the opcode, the register mask and two address words. The
    // refills behind them are three separate program reads, and the part drives
    // one bus cycle every four clocks, so they are four clocks apart.
    //
    // **This is `phosphor-emulator-d31l`.** While `refill_prefetch` was
    // infallible it could not suspend, so the second and third refills were
    // driven on one clock and every transfer behind them was early. The
    // recorded trace is [R0 R4 R8 W24 ...] and this core produced
    // [R0 R4 R4 W24 ...] on 2,231 cases of the microcode-derived corpus.
    let clocks = access_clocks(&[0x48F9, 0x0000, 0x4000, 0x0001], |cpu, _| {
        cpu.d[0] = 0x1234_5678;
    });

    assert!(
        clocks.len() >= 3,
        "expected at least three accesses, got {clocks:?}"
    );
    let leading = &clocks[..3];
    assert_eq!(
        leading,
        [0, 4, 8],
        "the three leading program reads take a clock each, four apart"
    );

    // And no two accesses anywhere in the instruction share a clock, which is
    // the general form of the same rule.
    for pair in clocks.windows(2) {
        assert_ne!(
            pair[0], pair[1],
            "two bus cycles on clock {} in {clocks:?}",
            pair[0]
        );
    }
}

// ---------------------------------------------------------------------------
// An indexed MOVEM's index add falls between its two fetches
// ---------------------------------------------------------------------------

#[test]
fn an_indexed_movem_spends_its_index_add_between_its_two_fetches() {
    // `MOVEM.w D0/D1,(d8,A0,Xn)`. The register mask is fetched, then the part
    // spends two clocks adding the index, and only then fetches the mode's
    // extension word. The reference emulation's per-cycle listing charges
    // exactly that: `alu_ext`, `m_icount -= 2`, then the extension-word read.
    //
    // **This is `phosphor-emulator-eemf`.** Declaring those two clocks at the
    // finish instead left them at the end of the instruction and put every
    // transfer after the mask two clocks early: recorded [R0 R6 W22 ...]
    // against ours [R0 R4 W20 ...] on 588 cases.
    let bus = run_one(&[0x48B0, 0x0003, 0x0010], |cpu, _| {
        cpu.a[0] = OPERANDS;
        cpu.d[0] = 0x10; // even index, so the destination does not fault
    });

    assert_eq!(
        bus.shape(),
        "P.P.W.W.P",
        "the mask fetch, the extension fetch, two result words, then the refill"
    );

    // The placement is the point: four clocks between the first two fetches
    // would mean the index add had not been spent between them.
    let clocks = access_clocks(&[0x48B0, 0x0003, 0x0010], |cpu, _| {
        cpu.a[0] = OPERANDS;
        cpu.d[0] = 0x10;
    });
    assert_eq!(
        clocks[1] - clocks[0],
        6,
        "two clocks of index add sit between the two fetches, not four \
         clocks of nothing: got {clocks:?}"
    );
}

#[test]
fn a_non_indexed_movem_has_no_gap_between_its_two_fetches() {
    // The control: `MOVEM.w D0/D1,(d16,A0)` has no index to add, so its two
    // fetches are an ordinary bus cycle apart. **A gap inserted for every
    // MOVEM rather than for the indexed modes fails here.**
    let clocks = access_clocks(&[0x48A8, 0x0003, 0x0010], |cpu, _| {
        cpu.a[0] = OPERANDS;
    });
    assert_eq!(
        clocks[1] - clocks[0],
        4,
        "four clocks, one bus cycle, no index add: got {clocks:?}"
    );
}
