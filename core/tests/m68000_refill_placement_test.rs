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
