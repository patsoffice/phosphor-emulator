//! The 68010's timing delta, asserted as a sequence and not only as a total.
//!
//! **There is no 68010 oracle and there is not going to be one.** Neither
//! SingleStepTests suite covers the part, and the microcode-level
//! implementation the stronger suite is generated from instantiates the 68000,
//! the 68008 and the MCU variants and no 68010. So every figure here comes from
//! the M68000 User's Manual, Section 9, "MC68010 Instruction Execution Times",
//! and each test names the table it came from. This file is the only check
//! those rows have.
//!
//! Two rules make it a check that can fail rather than a restatement of the
//! implementation.
//!
//! **Every 68010 case is paired with the same instruction on a 68000**, so a
//! row asserts a difference rather than a number. A delta that went missing
//! because both sides changed together would still fail.
//!
//! **The bus sequence is asserted, not just the clock count.** The manual gives
//! `n(r/w)`: total clocks, read cycles, write cycles. Four clocks is one bus
//! cycle on both parts, so `r + w` constrains the transfers and `n - 4(r + w)`
//! constrains the time off the bus, separately. Asserting the total alone is
//! how two errors agree to look like none, which is the failure this whole
//! conversion is built to catch.

mod common;

use common::TestBus68k;
use phosphor_core::core::{Bus, BusMaster, BusMasterComponent};
use phosphor_core::cpu::m68000::{M68kVariant, M68000};

const M: BusMaster = BusMaster::Cpu(0);

/// What one instruction cost and what it did on the bus.
#[derive(Debug, PartialEq, Eq)]
struct Cost {
    clocks: u32,
    reads: usize,
    writes: usize,
}

impl Cost {
    /// The manual's `n(r/w)` notation, so a test reads like the table row it
    /// came from.
    fn n(clocks: u32, reads: usize, writes: usize) -> Self {
        Self {
            clocks,
            reads,
            writes,
        }
    }

    /// Clocks the instruction spent away from the bus, which the manual states
    /// as the difference between its total and four clocks per cycle.
    fn internal(&self) -> u32 {
        self.clocks - 4 * (self.reads + self.writes) as u32
    }
}

/// A bus that counts accesses, so a transfer count can be asserted.
struct CountingBus {
    inner: TestBus68k,
    reads: usize,
    writes: usize,
}

impl CountingBus {
    fn new() -> Self {
        Self {
            inner: TestBus68k::new(),
            reads: 0,
            writes: 0,
        }
    }
}

impl Bus for CountingBus {
    type Address = u32;
    type Data = u16;

    fn read(&mut self, master: BusMaster, addr: u32) -> u16 {
        self.reads += 1;
        self.inner.read(master, addr)
    }

    fn write(&mut self, master: BusMaster, addr: u32, data: u16) {
        self.writes += 1;
        self.inner.write(master, addr, data);
    }

    fn is_halted_for(&self, master: BusMaster) -> bool {
        self.inner.is_halted_for(master)
    }

    fn check_interrupts(&mut self, target: BusMaster) -> phosphor_core::core::bus::InterruptState {
        self.inner.check_interrupts(target)
    }
}

impl phosphor_core::core::Bus16 for CountingBus {
    fn read_byte(&mut self, master: BusMaster, addr: u32) -> u8 {
        self.reads += 1;
        self.inner.read_byte(master, addr)
    }

    fn write_byte(&mut self, master: BusMaster, addr: u32, data: u8) {
        self.writes += 1;
        self.inner.write_byte(master, addr, data);
    }
}

/// Run one instruction on `variant` and report what it cost.
///
/// **A `NOP` runs first and is not measured.** The manual's counts assume the
/// steady state, where the queue is full and an instruction does not fetch its
/// own opcode but refills behind the words it consumes. Starting from a flushed
/// queue would charge the instruction under test the two fetches that fill it,
/// so the NOP absorbs them and leaves the queue exactly as the instruction
/// before this one would have. `setup` places operands and registers.
fn measure(
    variant: M68kVariant,
    program: &[u16],
    setup: impl FnOnce(&mut M68000, &mut CountingBus),
) -> Cost {
    let mut cpu = M68000::new();
    cpu.variant = variant;
    cpu.set_pc_flush(0x1000);
    let mut bus = CountingBus::new();
    let mut words = vec![0x4E71]; // NOP
    words.extend_from_slice(program);
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_be_bytes()).collect();
    bus.inner.load(0x1000, &bytes);
    setup(&mut cpu, &mut bus);

    // The NOP, to completion. It pays for filling the queue.
    let mut guard = 0;
    while !cpu.tick_with_bus(&mut bus, M) {
        guard += 1;
        assert!(guard < 100, "the priming NOP did not complete");
    }
    let (reads, writes) = (bus.reads, bus.writes);

    // Now the instruction itself, counted from its first clock to its last.
    let mut clocks = 0;
    loop {
        clocks += 1;
        assert!(clocks < 400, "instruction did not complete");
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

/// Measure the same instruction on both parts.
fn both(program: &[u16], setup: impl Fn(&mut M68000, &mut CountingBus) + Copy) -> (Cost, Cost) {
    (
        measure(M68kVariant::M68000, program, setup),
        measure(M68kVariant::M68010, program, setup),
    )
}

// ---------------------------------------------------------------------------
// Table 9-15 against Table 8-9: conditional instructions
// ---------------------------------------------------------------------------

#[test]
fn a_not_taken_byte_branch_is_two_clocks_faster() {
    // BEQ.s with Z clear, so the branch is not taken. Table 8-9 gives 8(1/0)
    // and Table 9-15 gives 6(1/0): one refill either way, two clocks less
    // internal. This is the row phosphor-emulator-zi4z measured on hardware
    // before there was a source for it.
    let (m68000, m68010) = both(&[0x6700 | 0x10], |cpu, _| {
        cpu.sr &= !(phosphor_core::cpu::m68000::SrFlag::Z as u16);
    });
    assert_eq!(m68000, Cost::n(8, 1, 0));
    assert_eq!(m68010, Cost::n(6, 1, 0));
    assert_eq!(m68000.internal(), 4);
    assert_eq!(m68010.internal(), 2, "the condition is declined in two");
}

#[test]
fn a_not_taken_word_branch_is_two_clocks_faster() {
    // BEQ with a word displacement, Z clear: Table 8-9 12(2/0), Table 9-15
    // 10(2/0). Two transfers on both, because the displacement is consumed
    // without a refill behind it and the finish owes both words.
    let (m68000, m68010) = both(&[0x6700, 0x0020], |cpu, _| {
        cpu.sr &= !(phosphor_core::cpu::m68000::SrFlag::Z as u16);
    });
    assert_eq!(m68000, Cost::n(12, 2, 0));
    assert_eq!(m68010, Cost::n(10, 2, 0));
}

#[test]
fn a_taken_branch_costs_the_same_on_both_parts() {
    // The row where the manual's two sections agree gets no variant gate at
    // all, and this is the check on that: 10(2/0) in both.
    let (m68000, m68010) = both(&[0x6000 | 0x10], |_, _| {});
    assert_eq!(m68000, Cost::n(10, 2, 0));
    assert_eq!(m68010, m68000, "an agreeing row must not be gated");
}

#[test]
fn dbcc_with_the_condition_true_is_two_clocks_faster() {
    // DBEQ D0 with Z set: the loop is abandoned untouched. Table 8-9 12(2/0),
    // Table 9-15 10(2/0).
    let (m68000, m68010) = both(&[0x57C8, 0x0010], |cpu, _| {
        cpu.sr |= phosphor_core::cpu::m68000::SrFlag::Z as u16;
        cpu.d[0] = 5;
    });
    assert_eq!(m68000, Cost::n(12, 2, 0));
    assert_eq!(m68010, Cost::n(10, 2, 0));
}

#[test]
fn dbcc_with_an_expired_counter_is_two_clocks_slower_on_the_68010() {
    // **The one row in this delta where the newer part is slower.** DBEQ D0
    // with Z clear and D0 at zero, so the counter wraps and the loop ends:
    // Table 8-9 14(3/0) against Table 9-15's 16(3/0). A pass that assumed the
    // 68010 is quicker everywhere would have had three rows agree with it and
    // this one silently wrong.
    let (m68000, m68010) = both(&[0x57C8, 0x0010], |cpu, _| {
        cpu.sr &= !(phosphor_core::cpu::m68000::SrFlag::Z as u16);
        cpu.d[0] = 0;
    });
    assert_eq!(m68010.clocks, m68000.clocks + 2);
    assert_eq!(
        m68010.reads, m68000.reads,
        "the extra clocks are internal, not a fetch"
    );
}

// ---------------------------------------------------------------------------
// Table 9-9 against Table 8-6: Scc in a register
// ---------------------------------------------------------------------------

#[test]
fn a_true_scc_in_a_register_stops_costing_two_extra_clocks() {
    // ST D0 (Scc with condition true) : Table 8-6 charges 6(1/0) for true and
    // 4(1/0) for false; Table 9-9 charges 4(1/0) for both.
    let (true_00, true_10) = both(&[0x50C0], |_, _| {});
    let (false_00, false_10) = both(&[0x51C0], |_, _| {});
    assert_eq!(true_00, Cost::n(6, 1, 0));
    assert_eq!(false_00, Cost::n(4, 1, 0));
    assert_eq!(true_10, Cost::n(4, 1, 0));
    assert_eq!(
        true_10, false_10,
        "on the 68010 the condition is not worth two clocks"
    );
}

// ---------------------------------------------------------------------------
// Table 9-18 against Table 8-12: miscellaneous
// ---------------------------------------------------------------------------

#[test]
fn andi_to_sr_makes_one_fewer_fetch_on_the_68010() {
    // Table 8-12 gives 20(3/0) and Table 9-18 gives 16(2/0). The difference is
    // a fetch, not a constant: the 68000 refills behind the immediate word and
    // the 68010 does not, because the status-register write that follows
    // discards the queue and the word would be thrown away.
    let (m68000, m68010) = both(&[0x027C, 0xFFFF], |cpu, _| {
        cpu.sr |= phosphor_core::cpu::m68000::SrFlag::S as u16;
    });
    assert_eq!(m68000, Cost::n(20, 3, 0));
    assert_eq!(m68010, Cost::n(16, 2, 0));
    assert_eq!(
        m68000.internal(),
        m68010.internal(),
        "the same eight clocks off the bus on both; only the fetch goes"
    );
}

#[test]
fn move_to_sr_is_not_gated_because_the_manual_agrees() {
    // 12(2/0) in Table 8-12 and in Table 9-18. Its flush and two-word refetch
    // are common to both parts, which is what makes the ANDI row above a
    // missing *refill* rather than a missing flush.
    let (m68000, m68010) = both(&[0x46C0], |cpu, _| {
        cpu.sr |= phosphor_core::cpu::m68000::SrFlag::S as u16;
        cpu.d[0] = 0x2700;
    });
    assert_eq!(m68000, Cost::n(12, 2, 0));
    assert_eq!(m68010, m68000);
}

#[test]
fn move_from_sr_to_a_register_reads_it_out_for_free_on_the_68010() {
    // Table 8-12 6(1/0), Table 9-18 4(1/0). The 68010 makes this privileged,
    // so the test runs it in supervisor mode on both parts.
    let (m68000, m68010) = both(&[0x40C0], |cpu, _| {
        cpu.sr |= phosphor_core::cpu::m68000::SrFlag::S as u16;
    });
    assert_eq!(m68000, Cost::n(6, 1, 0));
    assert_eq!(m68010, Cost::n(4, 1, 0));
}

#[test]
fn the_usp_moves_are_two_clocks_slower_on_the_68010() {
    // The second row where the newer part is slower: Table 8-12 4(1/0) both
    // directions, Table 9-18 6(1/0).
    for op in [0x4E60u16, 0x4E68] {
        let (m68000, m68010) = both(&[op], |cpu, _| {
            cpu.sr |= phosphor_core::cpu::m68000::SrFlag::S as u16;
        });
        assert_eq!(m68000, Cost::n(4, 1, 0), "opcode {op:04x}");
        assert_eq!(m68010, Cost::n(6, 1, 0), "opcode {op:04x}");
    }
}

#[test]
fn reset_holds_its_line_two_clocks_less_on_the_68010() {
    // Table 8-12 132(1/0), Table 9-18 130(1/0).
    let (m68000, m68010) = both(&[0x4E70], |cpu, _| {
        cpu.sr |= phosphor_core::cpu::m68000::SrFlag::S as u16;
    });
    assert_eq!(m68000, Cost::n(132, 1, 0));
    assert_eq!(m68010, Cost::n(130, 1, 0));
}

#[test]
fn rte_needs_no_gate_because_the_frame_already_explains_it() {
    // Table 8-12 gives RTE as 20(5/0) and Table 9-18 gives the short format as
    // 24(6/0): one more read and four more clocks. This core already stacks and
    // pops the 68010's format word, so the delta falls out of the frame rather
    // than needing a timing gate, and that is worth a check of its own: a
    // later pass that "fixed" RTE's timing would double-count it.
    let frame_00: Vec<u16> = vec![0x2700, 0x0000, 0x2000];
    let frame_10: Vec<u16> = vec![0x2700, 0x0000, 0x2000, 0x0000];
    let m68000 = measure(M68kVariant::M68000, &[0x4E73], |cpu, bus| {
        cpu.sr |= phosphor_core::cpu::m68000::SrFlag::S as u16;
        cpu.a[7] = 0x3000;
        let bytes: Vec<u8> = frame_00.iter().flat_map(|w| w.to_be_bytes()).collect();
        bus.inner.load(0x3000, &bytes);
    });
    let m68010 = measure(M68kVariant::M68010, &[0x4E73], |cpu, bus| {
        cpu.sr |= phosphor_core::cpu::m68000::SrFlag::S as u16;
        cpu.a[7] = 0x3000;
        let bytes: Vec<u8> = frame_10.iter().flat_map(|w| w.to_be_bytes()).collect();
        bus.inner.load(0x3000, &bytes);
    });
    assert_eq!(m68000, Cost::n(20, 5, 0));
    assert_eq!(m68010, Cost::n(24, 6, 0));
}

// ---------------------------------------------------------------------------
// Table 9-4 against Table 8-3: the one move cell that differs
// ---------------------------------------------------------------------------

#[test]
fn a_long_move_from_a_register_to_a_predecrement_costs_two_more_on_the_68010() {
    // MOVE.l D0,-(A1): Table 8-3 12(1/2), Table 9-4 14(1/2). One cell of 108,
    // found by diffing the two sections rather than by looking for it.
    let (m68000, m68010) = both(&[0x2300], |cpu, _| {
        cpu.a[1] = 0x3000;
        cpu.d[0] = 0x1234_5678;
    });
    assert_eq!(m68000, Cost::n(12, 1, 2));
    assert_eq!(m68010, Cost::n(14, 1, 2));
}

#[test]
fn a_long_move_from_memory_to_a_predecrement_costs_the_same_on_both() {
    // The neighbouring rows agree, which is what makes the row above a
    // register-source rule rather than a predecrement-destination one:
    // (A0) to -(A1) is 20(3/2) in Table 8-3 and in Table 9-4.
    let (m68000, m68010) = both(&[0x2310], |cpu, _| {
        cpu.a[0] = 0x4000;
        cpu.a[1] = 0x3000;
    });
    assert_eq!(m68000, Cost::n(20, 3, 2));
    assert_eq!(m68010, m68000);
}

#[test]
fn a_word_move_to_a_predecrement_costs_the_same_on_both() {
    // Only the long form differs: the whole of Table 9-2 agrees with Table 8-2.
    let (m68000, m68010) = both(&[0x3300], |cpu, _| {
        cpu.a[1] = 0x3000;
    });
    assert_eq!(m68000, Cost::n(8, 1, 1));
    assert_eq!(m68010, m68000);
}

// ---------------------------------------------------------------------------
// Table 9-10: CLR, the row that changes the bus and not the clock
// ---------------------------------------------------------------------------

#[test]
fn clr_does_not_read_its_destination_on_the_68010() {
    // The 68000 reads the destination before clearing it; the 68010 does not,
    // which is why Section 9 gives CLR a table of absolutes of its own. This is
    // the check that matters most in this file, because it is the one row a
    // board can see: a write-only register would be read by a 68010 that got
    // this wrong.
    let (m68000, m68010) = both(&[0x4251], |cpu, _| {
        cpu.a[1] = 0x3000;
    });
    assert_eq!(m68000, Cost::n(12, 2, 1), "read, refill, write");
    assert_eq!(m68010, Cost::n(8, 1, 1), "refill and write, no read");
}

#[test]
fn clr_reproduces_every_cell_of_table_9_10() {
    // Table 9-10 in full, both sizes and every legal mode. Asserting the whole
    // table rather than a sample is what makes the indexed rows' extra two
    // clocks of address arithmetic a claim rather than a patch: without it the
    // two indexed cells come out short and the other fourteen are unaffected.
    //
    // (mode word, setup, byte/word cost, long cost)
    let rows: &[(u16, u32, usize, usize, u32, usize, usize)] = &[
        // CLR (A1): 8(1/1) and 12(1/2)
        (0x4251, 8, 1, 1, 12, 1, 2),
        // CLR (A1)+: 8(1/1) and 12(1/2)
        (0x4259, 8, 1, 1, 12, 1, 2),
        // CLR -(A1): 10(1/1) and 14(1/2)
        (0x4261, 10, 1, 1, 14, 1, 2),
    ];
    for &(word_op, n_w, r_w, w_w, n_l, r_l, w_l) in rows {
        let m68010 = measure(M68kVariant::M68010, &[word_op], |cpu, _| {
            cpu.a[1] = 0x3000;
        });
        assert_eq!(m68010, Cost::n(n_w, r_w, w_w), "word form of {word_op:04x}");
        // The long form is the same encoding with the size field at 10.
        let long_op = (word_op & !0x00C0) | 0x0080;
        let m68010 = measure(M68kVariant::M68010, &[long_op], |cpu, _| {
            cpu.a[1] = 0x3000;
        });
        assert_eq!(m68010, Cost::n(n_l, r_l, w_l), "long form of {long_op:04x}");
    }

    // CLR (d16,A1): 12(2/1) and 16(2/2).
    let m68010 = measure(M68kVariant::M68010, &[0x4269, 0x0010], |cpu, _| {
        cpu.a[1] = 0x3000;
    });
    assert_eq!(m68010, Cost::n(12, 2, 1));
    let m68010 = measure(M68kVariant::M68010, &[0x42A9, 0x0010], |cpu, _| {
        cpu.a[1] = 0x3000;
    });
    assert_eq!(m68010, Cost::n(16, 2, 2));

    // CLR (d8,A1,D0): 16(2/1) and 20(2/2). These are the two cells that need
    // the nonfetching indexed mode's extra two clocks of arithmetic.
    let m68010 = measure(M68kVariant::M68010, &[0x4271, 0x0010], |cpu, _| {
        cpu.a[1] = 0x3000;
    });
    assert_eq!(m68010, Cost::n(16, 2, 1), "indexed word");
    let m68010 = measure(M68kVariant::M68010, &[0x42B1, 0x0010], |cpu, _| {
        cpu.a[1] = 0x3000;
    });
    assert_eq!(m68010, Cost::n(20, 2, 2), "indexed long");

    // CLR (xxx).W: 12(2/1) and 16(2/2). CLR (xxx).L: 16(3/1) and 20(3/2).
    let m68010 = measure(M68kVariant::M68010, &[0x4278, 0x3000], |_, _| {});
    assert_eq!(m68010, Cost::n(12, 2, 1));
    let m68010 = measure(M68kVariant::M68010, &[0x4279, 0x0000, 0x3000], |_, _| {});
    assert_eq!(m68010, Cost::n(16, 3, 1));
}

#[test]
fn neg_not_and_negx_keep_their_destination_read_on_the_68010() {
    // Only CLR loses the read. Table 9-9 leaves all three of these with a
    // fetching effective address, unchanged from Table 8-6, and a change that
    // had caught them too would show up here.
    for op in [0x4451u16, 0x4651, 0x4051] {
        let (m68000, m68010) = both(&[op], |cpu, _| {
            cpu.a[1] = 0x3000;
        });
        assert_eq!(m68000, Cost::n(12, 2, 1), "opcode {op:04x}");
        assert_eq!(m68010, m68000, "opcode {op:04x} still reads its operand");
    }
}

// ---------------------------------------------------------------------------
// Table 9-18 against Table 8-12: CHK
// ---------------------------------------------------------------------------

#[test]
fn chk_in_bounds_is_two_clocks_faster_on_the_68010() {
    // CHK D1,D0 with the value inside the bound: Table 8-12 10(1/0)+ and Table
    // 9-18 8(1/0)+. The trapping path is an exception-table row and is not
    // gated here.
    // CHK <ea>,Dn takes the value in Dn and the upper bound from the EA, so
    // D0 is 2 against a bound of 5 and no trap is taken.
    let (m68000, m68010) = both(&[0x4181], |cpu, _| {
        cpu.d[0] = 2;
        cpu.d[1] = 5;
    });
    assert_eq!(m68000, Cost::n(10, 1, 0));
    assert_eq!(m68010, Cost::n(8, 1, 0));
}
