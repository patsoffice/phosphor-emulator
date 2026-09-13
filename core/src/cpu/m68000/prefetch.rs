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
use super::addressing::{Abort, AccessResult};
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
    /// **This suspends like any other access the body drives**, which is what
    /// gives a second refill in one body a clock of its own. It was infallible
    /// until `phosphor-emulator-d31l`, and the cost of that was measurable: it
    /// called `flush_pending` and drove on the clock it stood on, so two
    /// refills in one body attempt landed together. `MOVEM.l #, (xxx).l`
    /// recorded three leading program reads at clocks 0, 4 and 8 and this core
    /// drove the second and third both at 4.
    ///
    /// Outside a body there is nothing to unwind to and `can_suspend` is
    /// false, so the loader's refills cannot fail; they go through
    /// [`Self::refill_prefetch_outside_body`], which says so in one place.
    pub(crate) fn refill_prefetch<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        if self.prefetch_len >= 2 {
            return Ok(());
        }
        // Already fetched on an earlier attempt at this body: the word goes
        // back into the hole the unwind reopened, and the bus is not asked
        // again. This is why an extension-word fetch is safe to unwind past.
        if self.replay_refill() {
            return Ok(());
        }
        // The part drives one bus cycle every four clocks, so a refill that
        // follows another cycle this body already drove belongs on the next
        // clock, and unwinding is the only way to put it there. Same rule as
        // `read_word_in`, and the reason this function is fallible.
        if self.must_suspend() {
            return Err(Abort::Suspend);
        }
        // A fetch the body wants *now* cannot overtake cycles it handed over
        // earlier, and it needs the word before it can go on. Where the body
        // can be unwound it waits for those to drain and runs again, so each
        // keeps its own clock, and an idle step deferred before this refill
        // survives to be driven in order rather than being flushed away.
        if self.pending_pos < self.pending_len {
            if self.can_suspend() {
                return Err(Abort::Suspend);
            }
            self.flush_pending(bus, master);
        }
        let addr = self.mask_addr(self.pc.wrapping_add(2 * u32::from(self.prefetch_len)));
        bus.observe_bus_cycle(master, addr, self.program_cycle(false));
        self.transfers += 1;
        self.tick_cycles += 1;
        let word = bus.read(master, addr);
        self.prefetch[usize::from(self.prefetch_len)] = word;
        self.prefetch_len += 1;
        self.log_cycle(super::ReplayedCycle::Refill(word));
        Ok(())
    }

    /// Drive a refill from outside an instruction body.
    ///
    /// The loader is the only caller. `can_suspend` is false outside a body,
    /// because there is nothing to unwind to, so the fallible form cannot
    /// return `Err` here; the assertion states that rather than leaving each
    /// call site to discard a result it does not understand.
    pub(crate) fn refill_prefetch_outside_body<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        debug_assert!(
            !self.can_suspend(),
            "a refill inside a body must go through the fallible form so it can take its own clock"
        );
        let _ = self.refill_prefetch(bus, master);
    }

    /// Fill the queue to both words.
    pub(crate) fn fill_prefetch<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        while self.prefetch_len < 2 {
            self.refill_prefetch(bus, master)?;
        }
        Ok(())
    }

    /// As [`Self::fill_prefetch`], from outside a body, where it cannot fail.
    pub(crate) fn fill_prefetch_outside_body<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        debug_assert!(
            !self.can_suspend(),
            "a fill inside a body must go through the fallible form"
        );
        let _ = self.fill_prefetch(bus, master);
    }

    /// Take the word at `pc` out of the queue, refilling behind it.
    ///
    /// This is the ordinary instruction-stream read: the opcode and every
    /// extension word come through here.
    pub(crate) fn take_word<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<u16> {
        self.fill_prefetch(bus, master)?;
        Ok(self.pop_prefetch())
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
    ) -> AccessResult<u16> {
        if self.prefetch_len == 0 {
            self.refill_prefetch(bus, master)?;
        }
        self.words_without_refill = self.words_without_refill.saturating_add(1);
        Ok(self.pop_prefetch())
    }

    /// Take a word out of the queue, leaving the hole for whoever called to
    /// fill on a clock of its own.
    ///
    /// Not [`Self::take_word_no_refill`], which counts the word as one the
    /// instruction deliberately consumed without a refill. This hole *is*
    /// refilled; the caller is only choosing when, and
    /// [`format::suppresses_refill`](super::format::suppresses_refill) must go
    /// on saying so.
    ///
    /// Two callers. The loader takes the opcode this way and issues the refill
    /// behind it on the next clock. And `MOVE` to an absolute long destination
    /// takes the second of its two address words this way, because the part
    /// writes with the address that word completes *before* prefetching again:
    /// the refill behind it is handed over after the write rather than driven
    /// in front of it.
    pub(crate) fn take_word_deferred_refill(&mut self) -> u16 {
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
        cpu.fill_prefetch(&mut bus, M).unwrap();
        assert_eq!(cpu.prefetch, [0x1122, 0x3344]);
        assert_eq!(cpu.pc, 0x1000, "filling the queue does not consume");
        assert_eq!(cpu.transfers, 2, "an empty queue costs two fetches");
    }

    #[test]
    fn taking_a_word_refills_behind_it() {
        let (mut cpu, mut bus) = cpu_at(0x1000);
        assert_eq!(cpu.take_word(&mut bus, M).unwrap(), 0x1122);
        assert_eq!(cpu.pc, 0x1002);
        assert_eq!(cpu.transfers, 2, "two fetches to fill an empty queue");
        // The queue now holds the word at the new pc, and taking the next one
        // refills the hole first: one more fetch, and the invariant holds.
        assert_eq!(cpu.take_word(&mut bus, M).unwrap(), 0x3344);
        assert_eq!(cpu.pc, 0x1004);
        assert_eq!(cpu.transfers, 3);
        assert_eq!(cpu.prefetch[0], 0x5566, "queue[0] is the word at pc");
    }

    #[test]
    fn taking_without_a_refill_costs_nothing_while_the_queue_has_a_word() {
        let (mut cpu, mut bus) = cpu_at(0x1000);
        cpu.fill_prefetch(&mut bus, M).unwrap();
        cpu.transfers = 0;
        assert_eq!(cpu.take_word_no_refill(&mut bus, M).unwrap(), 0x1122);
        assert_eq!(cpu.transfers, 0, "the word was already fetched");
        assert_eq!(cpu.take_word_no_refill(&mut bus, M).unwrap(), 0x3344);
        assert_eq!(cpu.transfers, 0, "so was the second");
        // A third take finds the queue empty and has to fetch, which is the
        // only case where suppressing the refill still costs a transfer.
        assert_eq!(cpu.take_word_no_refill(&mut bus, M).unwrap(), 0x5566);
        assert_eq!(cpu.transfers, 1);
    }

    #[test]
    fn a_flush_discards_the_queue_and_the_next_take_costs_two() {
        let (mut cpu, mut bus) = cpu_at(0x1000);
        cpu.fill_prefetch(&mut bus, M).unwrap();
        cpu.transfers = 0;
        cpu.set_pc_flush(0x1004);
        assert_eq!(cpu.prefetch_len, 0);
        cpu.fill_prefetch(&mut bus, M).unwrap();
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
        cpu.fill_prefetch(&mut bus, M).unwrap();
        cpu.take_word(&mut bus, M).unwrap();
        assert_eq!(bus.seen, vec![0x1000, 0x1002]);
        cpu.take_word(&mut bus, M).unwrap();
        assert_eq!(bus.seen, vec![0x1000, 0x1002, 0x1004]);
    }
}
