//! The two-word instruction prefetch queue.
//!
//! The part fetches instruction words ahead of the point it is executing at.
//! Two words are held: the one being executed and the one after it. `pc` names
//! the first of them, so the queue's contents are always the words at `pc` and
//! `pc + 2`, which is the invariant every recorded vector asserts in its
//! `prefetch` pair, before and after, on all 1,317,560 cases of the two corpora.
//!
//! # Why this changes what an instruction costs
//!
//! An instruction does not fetch its own opcode: that word was fetched during
//! the instruction before it. What it does instead is *refill*, one program
//! space read for each word it consumed, and the recorded traces contain those
//! refills and no opcode fetch. In steady straight-line code the two accountings
//! give the same number, which is why the documented timing table works. They
//! part company wherever a queue is discarded or left unfilled, and those are
//! exactly the cases this core got wrong while it had no queue: an address error
//! costs two refills at the handler that a per-instruction table cannot express,
//! and `STOP` costs none at all.
//!
//! # When the refill happens
//!
//! Read off the recorded traces rather than assumed, and the rule has two
//! halves.
//!
//! - **A word is normally consumed by refilling the hole first**, so the queue
//!   is full again before the next word leaves it. `ADDI.b #, (d16, A7)` records
//!   two program reads before its operand read, one for the opcode and one for
//!   the immediate word, and a third before its write.
//! - **An instruction about to discard the queue does not refill it.** A taken
//!   `Bcc` with a word displacement is ten clocks, which is two transfers: the
//!   displacement comes out of the queue, no refill is issued for it, and the
//!   two transfers are the refills at the branch target. Refilling first would
//!   make it three and twelve. [`M68000::take_word_no_refill`] is that case, and
//!   every user of it is an instruction whose next act is a flush.
//!
//! The trailing refill is issued by [`M68000::finish_from_bus`], so the default
//! is that it lands at the end of the instruction, which is where `MOVE` puts
//! it: `MOVE.w A6, (A5)` records its write and *then* its refill. The
//! read-modify-write families put it before their write instead and say so by
//! calling [`M68000::refill_prefetch`] at that point.

use super::M68000;
use crate::core::{Bus16, BusMaster};

impl M68000 {
    /// Discard the queue. Every control transfer does this, and it must be
    /// called by the code that sets PC rather than inferred from PC moving:
    /// a taken branch with a zero displacement lands exactly where execution
    /// would have gone anyway and the part still flushes, so no comparison of
    /// addresses can tell the two apart.
    #[inline]
    pub(crate) fn flush_prefetch(&mut self) {
        self.prefetch_len = 0;
    }

    /// Fetch one word into the queue if it has room, from program space at the
    /// address after the words already held.
    ///
    /// The address goes to the bus observer as well as to the read, because an
    /// address-snooping device sees a prefetch exactly as it sees any other
    /// cycle the part drives.
    pub(crate) fn refill_prefetch<B: Bus16 + ?Sized>(&mut self, bus: &mut B, master: BusMaster) {
        if self.prefetch_len >= 2 {
            return;
        }
        let addr = self.mask_addr(self.pc.wrapping_add(2 * u32::from(self.prefetch_len)));
        bus.observe_bus_cycle(master, addr, self.program_cycle(false));
        self.transfers += 1;
        self.prefetch[usize::from(self.prefetch_len)] = bus.read(master, addr);
        self.prefetch_len += 1;
    }

    /// Fill the queue to both words.
    pub(crate) fn fill_prefetch<B: Bus16 + ?Sized>(&mut self, bus: &mut B, master: BusMaster) {
        while self.prefetch_len < 2 {
            self.refill_prefetch(bus, master);
        }
    }

    /// Take the word at `pc` out of the queue, refilling behind it.
    ///
    /// This is the ordinary instruction-stream read: the opcode and every
    /// extension word come through here.
    pub(crate) fn take_word<B: Bus16 + ?Sized>(&mut self, bus: &mut B, master: BusMaster) -> u16 {
        self.fill_prefetch(bus, master);
        self.pop_prefetch()
    }

    /// Take the word at `pc` out of the queue without refilling behind it.
    ///
    /// For an instruction whose next act discards the queue, so a refill would
    /// be a fetch of a word nothing will execute. The recorded costs say the
    /// part does not make it: see the module docs.
    pub(crate) fn take_word_no_refill<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> u16 {
        if self.prefetch_len == 0 {
            self.refill_prefetch(bus, master);
        }
        self.words_without_refill = self.words_without_refill.saturating_add(1);
        self.pop_prefetch()
    }

    /// Shift the queue down by one word and advance `pc` past it.
    ///
    /// The caller has already ensured slot 0 holds a fetched word.
    fn pop_prefetch(&mut self) -> u16 {
        debug_assert!(self.prefetch_len > 0, "the queue must hold the word at pc");
        let word = self.prefetch[0];
        self.prefetch[0] = self.prefetch[1];
        self.prefetch_len -= 1;
        self.pc = self.pc.wrapping_add(2);
        self.words_consumed = self.words_consumed.saturating_add(1);
        word
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::WordBus;
    use super::*;
    use crate::core::Bus;

    const M: BusMaster = BusMaster::Cpu(0);

    fn cpu_at(pc: u32) -> (M68000, WordBus) {
        let mut cpu = M68000::new();
        let mut bus = WordBus::new();
        cpu.set_pc_flush(pc);
        bus.load(pc, &[0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
        (cpu, bus)
    }

    #[test]
    fn a_full_queue_holds_the_words_at_pc_and_pc_plus_two() {
        let (mut cpu, mut bus) = cpu_at(0x1000);
        cpu.fill_prefetch(&mut bus, M);
        assert_eq!(cpu.prefetch, [0x1122, 0x3344]);
        assert_eq!(cpu.pc, 0x1000, "filling the queue does not consume");
        assert_eq!(cpu.transfers, 2, "an empty queue costs two fetches");
    }

    #[test]
    fn taking_a_word_refills_behind_it() {
        let (mut cpu, mut bus) = cpu_at(0x1000);
        assert_eq!(cpu.take_word(&mut bus, M), 0x1122);
        assert_eq!(cpu.pc, 0x1002);
        assert_eq!(cpu.transfers, 2, "two fetches to fill an empty queue");
        // The queue now holds the word at the new pc, and taking the next one
        // refills the hole first: one more fetch, and the invariant holds.
        assert_eq!(cpu.take_word(&mut bus, M), 0x3344);
        assert_eq!(cpu.pc, 0x1004);
        assert_eq!(cpu.transfers, 3);
        assert_eq!(cpu.prefetch[0], 0x5566, "queue[0] is the word at pc");
    }

    #[test]
    fn taking_without_a_refill_costs_nothing_while_the_queue_has_a_word() {
        let (mut cpu, mut bus) = cpu_at(0x1000);
        cpu.fill_prefetch(&mut bus, M);
        cpu.transfers = 0;
        assert_eq!(cpu.take_word_no_refill(&mut bus, M), 0x1122);
        assert_eq!(cpu.transfers, 0, "the word was already fetched");
        assert_eq!(cpu.take_word_no_refill(&mut bus, M), 0x3344);
        assert_eq!(cpu.transfers, 0, "so was the second");
        // A third take finds the queue empty and has to fetch, which is the
        // only case where suppressing the refill still costs a transfer.
        assert_eq!(cpu.take_word_no_refill(&mut bus, M), 0x5566);
        assert_eq!(cpu.transfers, 1);
    }

    #[test]
    fn a_flush_discards_the_queue_and_the_next_take_costs_two() {
        let (mut cpu, mut bus) = cpu_at(0x1000);
        cpu.fill_prefetch(&mut bus, M);
        cpu.transfers = 0;
        cpu.set_pc_flush(0x1004);
        assert_eq!(cpu.prefetch_len, 0);
        cpu.fill_prefetch(&mut bus, M);
        assert_eq!(cpu.prefetch, [0x5566, 0x7788]);
        assert_eq!(cpu.transfers, 2);
    }

    #[test]
    fn a_prefetch_is_presented_to_the_bus_observer() {
        // The Slapstic snoops the address bus, and a prefetch is a cycle the
        // part drives like any other. Counting observations proves the queue
        // presents its fetches rather than reading behind the board's back.
        struct Snoop {
            inner: WordBus,
            seen: Vec<u32>,
        }
        impl Bus for Snoop {
            type Address = u32;
            type Data = u16;
            fn read(&mut self, m: BusMaster, addr: u32) -> u16 {
                self.inner.read(m, addr)
            }
            fn write(&mut self, m: BusMaster, addr: u32, data: u16) {
                self.inner.write(m, addr, data);
            }
            fn is_halted_for(&self, _m: BusMaster) -> bool {
                false
            }
            fn check_interrupts(&mut self, _t: BusMaster) -> crate::core::bus::InterruptState {
                crate::core::bus::InterruptState::default()
            }
            fn observe_bus_cycle(&mut self, _m: BusMaster, addr: u32, _s: crate::core::BusSignals) {
                self.seen.push(addr);
            }
        }
        impl Bus16 for Snoop {
            fn read_byte(&mut self, m: BusMaster, addr: u32) -> u8 {
                self.inner.read_byte(m, addr)
            }
            fn write_byte(&mut self, m: BusMaster, addr: u32, data: u8) {
                self.inner.write_byte(m, addr, data);
            }
        }

        let (mut cpu, inner) = cpu_at(0x1000);
        let mut bus = Snoop {
            inner,
            seen: Vec::new(),
        };
        // Two fetches to fill an empty queue; the third arrives with the
        // *second* take, because a take fills the hole before it pops and the
        // first one finds no hole. That one-word lag is the queue: the fetch
        // an instruction pays for is the refill behind the word it consumed,
        // and the last of them is issued when the instruction finishes.
        cpu.fill_prefetch(&mut bus, M);
        cpu.take_word(&mut bus, M);
        assert_eq!(bus.seen, vec![0x1000, 0x1002]);
        cpu.take_word(&mut bus, M);
        assert_eq!(bus.seen, vec![0x1000, 0x1002, 0x1004]);
    }
}
