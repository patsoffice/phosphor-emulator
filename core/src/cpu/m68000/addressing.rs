//! M68000 effective-address decoding and word-bus memory access.
//!
//! The 6-bit EA field of an opcode is `mode:3 | reg:3` and selects one of 12
//! addressing modes. [`M68000::decode_ea`] fetches any required extension
//! words (advancing PC) and resolves the operand location to an [`Ea`], so
//! read, write, and read-modify-write paths share a single decode.
//!
//! # Word-bus memory model
//!
//! The 68000 data bus is 16 bits wide: every bus transaction is one word at
//! an even address. These helpers map sized accesses onto that bus:
//!
//! - **Word**: one transaction. **Long**: two transactions (big-endian, high
//!   word first).
//! - **Byte**: one transaction with one strobe. The part has no A0 pin: it
//!   puts the address on the bus and asserts UDS for the even byte (D8-D15) or
//!   LDS for the odd one (D0-D7). A byte write therefore drives one half and
//!   reads nothing, and a device wired to the other half is not accessed at
//!   all. The bus carries this through [`crate::core::Bus16`], whose byte
//!   methods each bus must implement rather than inherit.
//! - **Odd word/long addresses** raise an [`AddressError`] that propagates
//!   out of the instruction handler, aborting the instruction at the
//!   faulting access exactly like hardware (side effects already applied
//!   stay applied); `enter_address_error` then builds the group-0 frame.
//!
//! Effective addresses are computed at the full 32 bits (the 68000 ALU is
//! 32-bit internally — JMP/JSR load the unmasked value into PC, LEA into
//! An) and masked to the physical bus width (24 bits on the 68000/010)
//! only when driven onto the bus.

use super::M68000;
use crate::core::{Bus16, BusMaster};

/// Operand size of an instruction.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Size {
    Byte,
    Word,
    Long,
}

impl Size {
    /// Number of bytes an operand of this size occupies.
    #[inline]
    pub(crate) fn bytes(self) -> u32 {
        match self {
            Size::Byte => 1,
            Size::Word => 2,
            Size::Long => 4,
        }
    }

    /// Mask selecting the low `bytes()` of a u32 value.
    #[inline]
    pub(crate) fn mask(self) -> u32 {
        match self {
            Size::Byte => 0x0000_00FF,
            Size::Word => 0x0000_FFFF,
            Size::Long => 0xFFFF_FFFF,
        }
    }

    /// Sign bit of an operand of this size.
    #[inline]
    pub(crate) fn sign_bit(self) -> u32 {
        match self {
            Size::Byte => 0x0000_0080,
            Size::Word => 0x0000_8000,
            Size::Long => 0x8000_0000,
        }
    }
}

/// A word or long access faulted on an odd address (address error,
/// vector 3). Raised at the access site and propagated out of the
/// instruction handler, aborting the instruction exactly where real
/// hardware does; `enter_address_error` builds the group-0 frame from it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct AddressError {
    /// The faulting (odd) address.
    pub(crate) addr: u32,
    /// True for a write access.
    pub(crate) write: bool,
    /// Program-space fault (control-flow target fetch) vs data operand.
    pub(crate) program: bool,
    /// PC value the frame stacks: `current PC - 2` for operand faults,
    /// `target - 4` for control-transfer faults (empirical, from the
    /// hardware-derived test vectors).
    pub(crate) stacked_pc: u32,
}

pub(crate) type AccessResult<T> = Result<T, AddressError>;

/// A resolved operand location.
///
/// Extension words have already been consumed and any postincrement /
/// predecrement side effects applied; reading and writing through an `Ea`
/// performs no further decoding.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Ea {
    /// Data register Dn.
    DataReg(usize),
    /// Address register An.
    AddrReg(usize),
    /// Memory operand at a full 32-bit effective address (masked to the
    /// physical bus width on access).
    Mem(u32),
    /// Immediate value (already fetched from the instruction stream).
    Imm(u32),
}

/// Documented effective-address calculation time in clock cycles for a
/// *source* operand (M68000UM table 8-1): the extension-word fetches plus
/// the operand read. Long operands add one extra word transaction (4 cycles)
/// over byte/word for every memory mode.
///
/// **No instruction charges from this any more.** Cycle counts come from the
/// transfers an instruction makes plus [`ea_internal`], and this survives as
/// the documented total those two have to add back up to. The test at the
/// bottom of this file is what checks they do, which is the only thing keeping
/// the split honest against the manual.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn ea_cycles(mode: u8, reg: u8, size: Size) -> u32 {
    let long = matches!(size, Size::Long);
    let bw_l = |bw: u32, l: u32| if long { l } else { bw };
    match mode & 7 {
        0 | 1 => 0,          // Dn / An
        2 | 3 => bw_l(4, 8), // (An) / (An)+
        4 => bw_l(6, 10),    // -(An)
        5 => bw_l(8, 12),    // d16(An)
        6 => bw_l(10, 14),   // d8(An,Xn)
        _ => match reg & 7 {
            0 => bw_l(8, 12),  // abs.w
            1 => bw_l(12, 16), // abs.l
            2 => bw_l(8, 12),  // d16(PC)
            3 => bw_l(10, 14), // d8(PC,Xn)
            _ => bw_l(4, 8),   // #imm
        },
    }
}

/// The clocks an addressing mode spends *away* from the bus.
///
/// [`ea_cycles`] is a mode's documented total, and every transfer inside it is
/// four clocks, so whatever is left over is address arithmetic the part does
/// with the bus idle. There are only two such costs on this machine: two clocks
/// to predecrement an address register, and two to add an index register. Every
/// other mode computes its address inside time it is already spending on a
/// transfer, which is why its documented cost is a whole number of bus cycles.
///
/// This is the half of the timing table that survives once cycle counts are
/// charged from bus activity rather than looked up: the transfers are counted
/// as they happen, and this is what has to be added to them.
pub(crate) fn ea_internal(mode: u8, reg: u8) -> u32 {
    match mode & 7 {
        4 => 2,                 // -(An): the predecrement
        6 => 2,                 // d8(An,Xn): the index add
        7 if reg & 7 == 3 => 2, // d8(PC,Xn): likewise
        _ => 0,
    }
}

/// Sign-extend a byte to 32 bits.
#[inline]
pub(crate) fn sext8(v: u8) -> u32 {
    v as i8 as i32 as u32
}

/// Sign-extend a word to 32 bits.
#[inline]
pub(crate) fn sext16(v: u16) -> u32 {
    v as i16 as i32 as u32
}

impl M68000 {
    /// Build the operand-fault error for an odd word/long access: the
    /// stacked PC is the current PC minus one word (empirical rule — it
    /// tracks how many extension words the instruction had consumed).
    #[inline]
    fn operand_fault(&self, addr: u32, write: bool) -> AddressError {
        AddressError {
            addr,
            write,
            program: false,
            stacked_pc: self.pc.wrapping_sub(2),
        }
    }

    /// Read one word at `addr`; odd addresses raise the address error.
    pub(crate) fn read_word_at<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        addr: u32,
    ) -> AccessResult<u16> {
        self.read_word_in(bus, master, addr, false)
    }

    /// As [`Self::read_word_at`], naming program space rather than data.
    ///
    /// Only an operand reached through a PC-relative mode does this. The
    /// address is formed from PC, so the part drives the program function code
    /// for it, exactly as it does for a prefetch: `ADD.w (d16,PC),D0` reads its
    /// operand at code 6 in supervisor mode and 2 in user, where the same
    /// instruction through `(An)` reads at 5 and 1.
    fn read_word_in<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        addr: u32,
        program: bool,
    ) -> AccessResult<u16> {
        if addr & 1 != 0 {
            return Err(self.operand_fault(addr, false));
        }
        // A read cannot overtake a cycle the instruction has already decided
        // on, so anything outstanding is driven first. It loses its clock and
        // keeps its order, which is the right way round: a wrong position is a
        // rung-3 miss, a wrong order is a wrong program.
        self.flush_pending(bus, master);
        let a = self.mask_addr(addr);
        let cycle = if program {
            self.program_cycle(false)
        } else {
            self.data_cycle(false, false)
        };
        bus.observe_bus_cycle(master, a, cycle);
        self.transfers += 1;
        Ok(bus.read(master, a))
    }

    /// Write one word at `addr`; odd addresses raise the address error.
    pub(crate) fn write_word_at<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        addr: u32,
        data: u16,
    ) -> AccessResult<()> {
        if addr & 1 != 0 {
            return Err(self.operand_fault(addr, true));
        }
        // Handed to the bus unit where there is room: nothing in the
        // instruction is waiting on a write, so the body carries on and the
        // cycle runs on the clock the part would drive it on. The fault check
        // above stays here, because the address is known now and an abort has
        // to happen where the instruction can still see it.
        let a = self.mask_addr(addr);
        let signals = self.data_cycle(true, false);
        self.hand_over(
            bus,
            master,
            super::PendingCycle::Write {
                addr: a,
                data,
                byte: false,
                signals,
            },
        );
        Ok(())
    }

    /// Read a long word as two word transactions (big-endian, high first).
    pub(crate) fn read_long_at<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        addr: u32,
    ) -> AccessResult<u32> {
        self.read_long_in(bus, master, addr, false)
    }

    /// As [`Self::read_long_at`], in the space the caller names. Both halves
    /// name the same one.
    fn read_long_in<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        addr: u32,
        program: bool,
    ) -> AccessResult<u32> {
        let hi = self.read_word_in(bus, master, addr, program)?;
        let lo = self.read_word_in(bus, master, addr.wrapping_add(2), program)?;
        Ok(((hi as u32) << 16) | lo as u32)
    }

    /// Write a long word as two word transactions, **low half first**, at the
    /// higher address and then the lower one.
    ///
    /// This is what the read-modify-write families do, and it is not a quirk of
    /// one addressing mode: the part reads a long destination upwards and
    /// writes it back downwards, for a plain indirect, a displacement and a
    /// predecrement alike. The address register that walked up during the read
    /// is still pointing at the second word when the result is ready, so the
    /// write starts from there and steps back.
    ///
    /// Only the order differs. The addresses, the values, the transfer count
    /// and the memory afterwards are identical either way, so nothing but a
    /// per-transfer comparison against a recorded trace, or a device watching
    /// the address bus, can tell the two apart.
    pub(crate) fn write_long_low_half_first<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        addr: u32,
        data: u32,
    ) -> AccessResult<()> {
        self.write_word_at(bus, master, addr.wrapping_add(2), data as u16)?;
        self.write_word_at(bus, master, addr, (data >> 16) as u16)
    }

    /// Write a long word as two word transactions (big-endian, high first).
    pub(crate) fn write_long_at<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        addr: u32,
        data: u32,
    ) -> AccessResult<()> {
        self.write_word_at(bus, master, addr, (data >> 16) as u16)?;
        self.write_word_at(bus, master, addr.wrapping_add(2), data as u16)
    }

    /// Read one byte as a single bus cycle, asserting UDS for an even address
    /// and LDS for an odd one.
    ///
    /// The address pins carry the exact byte address, which is what an
    /// address-snooping device on the bus sees.
    pub(crate) fn read_byte_at<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        addr: u32,
    ) -> u8 {
        self.read_byte_in(bus, master, addr, false)
    }

    /// As [`Self::read_byte_at`], in the space the caller names.
    fn read_byte_in<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        addr: u32,
        program: bool,
    ) -> u8 {
        self.flush_pending(bus, master);
        let a = self.mask_addr(addr);
        let cycle = if program {
            let mut c = self.program_cycle(false);
            c.byte = true;
            c
        } else {
            self.data_cycle(false, true)
        };
        bus.observe_bus_cycle(master, a, cycle);
        self.transfers += 1;
        bus.read_byte(master, a)
    }

    /// Write one byte as a single bus cycle, asserting the one strobe that byte
    /// sits behind.
    ///
    /// The part performs no read here. It puts the word address on the bus,
    /// drives the byte onto the half its strobe selects, and that is the whole
    /// transfer: a device wired to the other half is not accessed at all.
    pub(crate) fn write_byte_at<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        addr: u32,
        data: u8,
    ) {
        let a = self.mask_addr(addr);
        let signals = self.data_cycle(true, true);
        self.hand_over(
            bus,
            master,
            super::PendingCycle::Write {
                addr: a,
                data: u16::from(data),
                byte: true,
                signals,
            },
        );
    }

    /// Push a word onto the active stack (A7 predecrements by 2).
    pub(crate) fn push_word<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        value: u16,
    ) -> AccessResult<()> {
        self.a[7] = self.a[7].wrapping_sub(2);
        self.write_word_at(bus, master, self.a[7], value)
    }

    /// Push a long word onto the active stack (A7 predecrements by 4).
    pub(crate) fn push_long<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        value: u32,
    ) -> AccessResult<()> {
        self.a[7] = self.a[7].wrapping_sub(4);
        self.write_long_at(bus, master, self.a[7], value)
    }

    /// Pop a word from the active stack (A7 postincrements by 2).
    pub(crate) fn pop_word<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<u16> {
        let value = self.read_word_at(bus, master, self.a[7])?;
        self.a[7] = self.a[7].wrapping_add(2);
        Ok(value)
    }

    /// Pop a long word from the active stack (A7 postincrements by 4).
    pub(crate) fn pop_long<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<u32> {
        let value = self.read_long_at(bus, master, self.a[7])?;
        self.a[7] = self.a[7].wrapping_add(4);
        Ok(value)
    }

    /// Take one word out of the prefetch queue and advance PC.
    ///
    /// The opcode and every extension word come through here. It is not a bus
    /// read: the word was fetched into the queue earlier, and what this costs
    /// is the refill behind it (see [`super::prefetch`]). PC is invariantly
    /// even (every control transfer to an odd address faults before it is
    /// fetched from), so this cannot raise an address error.
    pub(crate) fn read_imm_word<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> u16 {
        debug_assert!(self.pc & 1 == 0, "instruction stream PC must be even");
        self.take_word(bus, master)
    }

    /// Take one word out of the queue without refilling behind it, for an
    /// instruction that is about to flush the queue anyway.
    ///
    /// See [`super::prefetch`] for the recorded costs this comes from: a taken
    /// `Bcc` with a word displacement is ten clocks and two transfers, and both
    /// of those transfers are at the branch target.
    pub(crate) fn read_imm_word_no_refill<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> u16 {
        debug_assert!(self.pc & 1 == 0, "instruction stream PC must be even");
        self.take_word_no_refill(bus, master)
    }

    /// The postincrement/predecrement step for `(An)+` / `-(An)`: the
    /// operand size in bytes, except byte-sized accesses through A7 step by
    /// 2 to keep the stack pointer word-aligned.
    #[inline]
    pub(crate) fn step_for(&self, reg: usize, size: Size) -> u32 {
        if reg == 7 && size == Size::Byte {
            2
        } else {
            size.bytes()
        }
    }

    /// Resolve the index portion of a brief extension word (`d8(An,Xn)` /
    /// `d8(PC,Xn)`): Dn or An, sign-extended word or full long, scaled on
    /// 68020+ (the 68000/010 ignore the scale field).
    fn index_value(&self, ext: u16) -> u32 {
        let reg = ((ext >> 12) & 7) as usize;
        let raw = if ext & 0x8000 != 0 {
            self.a[reg]
        } else {
            self.d[reg]
        };
        let index = if ext & 0x0800 != 0 {
            raw
        } else {
            sext16(raw as u16)
        };
        let scale = match self.variant {
            super::M68kVariant::M68000 | super::M68kVariant::M68010 => 0,
            super::M68kVariant::M68020 | super::M68kVariant::M68030 => (ext >> 9) & 3,
        };
        index << scale
    }

    /// Decode a 6-bit effective-address field (`mode`, `reg`) for an operand
    /// of `size`, fetching extension words and applying postincrement /
    /// predecrement side effects.
    pub(crate) fn decode_ea<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        mode: u8,
        reg: u8,
        size: Size,
    ) -> Ea {
        self.decode_ea_inner(bus, master, mode, reg, size, true)
    }

    /// As [`Self::decode_ea`], for an instruction that will discard the
    /// prefetch queue as soon as the address is resolved.
    ///
    /// `JMP` and `JSR` are the users. Their extension words come out of the
    /// queue with no refill behind them, because the words behind them are on
    /// the path not taken: `JMP (d16, An)` is ten clocks, which is two
    /// transfers, and both are at the target.
    pub(crate) fn decode_ea_no_refill<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        mode: u8,
        reg: u8,
        size: Size,
    ) -> Ea {
        self.decode_ea_inner(bus, master, mode, reg, size, false)
    }

    /// One extension word, refilling behind it or not as the caller declares.
    #[inline]
    fn ext_word<B: Bus16 + ?Sized>(&mut self, bus: &mut B, master: BusMaster, refill: bool) -> u16 {
        if refill {
            self.read_imm_word(bus, master)
        } else {
            self.read_imm_word_no_refill(bus, master)
        }
    }

    fn decode_ea_inner<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        mode: u8,
        reg: u8,
        size: Size,
        refill: bool,
    ) -> Ea {
        // A PC-relative operand is fetched from PROGRAM space, because its
        // address is formed from PC. Recorded here rather than carried in the
        // resolved `Ea`, which would put a second variant through every match
        // on it for a property only the read that immediately follows can use.
        // `ea_read` takes it and clears it, so nothing else can inherit it.
        self.ea_program_space = mode & 7 == 7 && matches!(reg & 7, 2 | 3);
        let reg = (reg & 7) as usize;
        match mode & 7 {
            // Dn — data register direct
            0 => Ea::DataReg(reg),
            // An — address register direct
            1 => Ea::AddrReg(reg),
            // (An) — address register indirect
            2 => Ea::Mem(self.a[reg]),
            // (An)+ — postincrement: use the current address, then advance
            3 => {
                let addr = self.a[reg];
                self.a[reg] = addr.wrapping_add(self.step_for(reg, size));
                Ea::Mem(addr)
            }
            // -(An) — predecrement: retreat first, then use the new address
            4 => {
                self.a[reg] = self.a[reg].wrapping_sub(self.step_for(reg, size));
                Ea::Mem(self.a[reg])
            }
            // d16(An) — indirect with 16-bit signed displacement
            5 => {
                let disp = sext16(self.ext_word(bus, master, refill));
                Ea::Mem(self.a[reg].wrapping_add(disp))
            }
            // d8(An,Xn) — indirect with index register and 8-bit displacement
            6 => {
                let ext = self.ext_word(bus, master, refill);
                let addr = self.a[reg]
                    .wrapping_add(sext8(ext as u8))
                    .wrapping_add(self.index_value(ext));
                Ea::Mem(addr)
            }
            // Mode 7 submodes, selected by the register field
            _ => match reg {
                // abs.w — sign-extended 16-bit absolute address
                0 => Ea::Mem(sext16(self.ext_word(bus, master, refill))),
                // abs.l — full 32-bit absolute address (two words, high first)
                1 => {
                    let hi = self.ext_word(bus, master, refill) as u32;
                    let lo = self.ext_word(bus, master, refill) as u32;
                    Ea::Mem((hi << 16) | lo)
                }
                // d16(PC) — PC-relative; base is the extension word address
                2 => {
                    let base = self.pc;
                    let disp = sext16(self.ext_word(bus, master, refill));
                    Ea::Mem(base.wrapping_add(disp))
                }
                // d8(PC,Xn) — PC-relative with index; same base convention
                3 => {
                    let base = self.pc;
                    let ext = self.ext_word(bus, master, refill);
                    let addr = base
                        .wrapping_add(sext8(ext as u8))
                        .wrapping_add(self.index_value(ext));
                    Ea::Mem(addr)
                }
                // #imm — 1 extension word for byte/word, 2 for long
                4 => {
                    let value = match size {
                        Size::Byte => self.ext_word(bus, master, refill) as u32 & 0xFF,
                        Size::Word => self.ext_word(bus, master, refill) as u32,
                        Size::Long => {
                            let hi = self.ext_word(bus, master, refill) as u32;
                            let lo = self.ext_word(bus, master, refill) as u32;
                            (hi << 16) | lo
                        }
                    };
                    Ea::Imm(value)
                }
                // 7.5-7.7 are unassigned on the 68000 (illegal instruction,
                // lands in M5); decode as immediate-zero to stay deterministic
                _ => Ea::Imm(0),
            },
        }
    }

    /// Read an operand of `size` through a resolved [`Ea`]. Register and
    /// immediate operands are masked to the operand size; sign extension is
    /// the consumer's job (MOVEA/ADDA/CMPA).
    pub(crate) fn ea_read<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        ea: Ea,
        size: Size,
    ) -> AccessResult<u32> {
        Ok(match ea {
            Ea::DataReg(r) => self.d[r] & size.mask(),
            Ea::AddrReg(r) => {
                debug_assert!(size != Size::Byte, "byte access to An is illegal");
                self.a[r] & size.mask()
            }
            Ea::Mem(addr) => {
                // Taken, not copied: only the operand the decode just resolved
                // may be a program-space fetch, and a stack pop or a vector
                // read that follows must not inherit it.
                let program = std::mem::take(&mut self.ea_program_space);
                match size {
                    Size::Byte => self.read_byte_in(bus, master, addr, program) as u32,
                    Size::Word => self.read_word_in(bus, master, addr, program)? as u32,
                    Size::Long => self.read_long_in(bus, master, addr, program)?,
                }
            }
            Ea::Imm(v) => v & size.mask(),
        })
    }

    /// Write an operand through a resolved [`Ea`], refilling the prefetch
    /// queue first.
    ///
    /// This is where the read-modify-write families put their last refill, and
    /// the traces are unambiguous about it: `CLR.w (A4)` records an operand
    /// read, then a program read, then its write, and `NEG.l (A5)+` records the
    /// program read before *both* words of its long write. `MOVE` is the
    /// exception and writes through [`Self::ea_write`] instead, because it
    /// records its refill after the write; so does `MOVEP`, which has no
    /// program read of its own between its byte writes.
    ///
    /// A register destination writes nothing to the bus, so the refill lands in
    /// the same place it would have anyway: at the end of the instruction.
    pub(crate) fn ea_write_rmw<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        ea: Ea,
        size: Size,
        value: u32,
    ) -> AccessResult<()> {
        // Handed over rather than driven, so it lands on its own clock between
        // the operand read and the write: `CLR.w (A4)` is recorded as a read,
        // a program read and a write, on clocks 0, 4 and 8.
        let signals = self.program_cycle(false);
        self.hand_over(bus, master, super::PendingCycle::Refill { signals });
        // A long result goes back low half first: see
        // [`Self::write_long_low_half_first`] for why, and why only a
        // per-transfer comparison can see it. `MOVE` does not come through
        // here, and its own destination order is its business.
        match ea {
            Ea::Mem(addr) if size == Size::Long => {
                self.write_long_low_half_first(bus, master, addr, value)
            }
            _ => self.ea_write(bus, master, ea, size, value),
        }
    }

    /// Write an operand of `size` through a resolved [`Ea`].
    ///
    /// Byte/word writes to Dn preserve the upper register bits; word writes
    /// to An sign-extend to the full 32 bits (address registers have no
    /// partial-width writes).
    pub(crate) fn ea_write<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
        ea: Ea,
        size: Size,
        value: u32,
    ) -> AccessResult<()> {
        match ea {
            Ea::DataReg(r) => {
                self.d[r] = (self.d[r] & !size.mask()) | (value & size.mask());
            }
            Ea::AddrReg(r) => {
                debug_assert!(size != Size::Byte, "byte access to An is illegal");
                self.a[r] = match size {
                    Size::Word => sext16(value as u16),
                    _ => value,
                };
            }
            Ea::Mem(addr) => match size {
                Size::Byte => self.write_byte_at(bus, master, addr, value as u8),
                Size::Word => return self.write_word_at(bus, master, addr, value as u16),
                Size::Long => return self.write_long_at(bus, master, addr, value),
            },
            Ea::Imm(_) => debug_assert!(false, "write to immediate operand"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::WordBus;
    use super::*;

    const M: BusMaster = BusMaster::Cpu(0);

    /// Every addressing mode's documented cost, less the clocks it spends away
    /// from the bus, must be a whole number of four-clock transfers.
    ///
    /// This is the consistency check on splitting [`ea_cycles`] into transfers
    /// plus [`ea_internal`]. If a mode ever fails it, the split is wrong for
    /// that mode and any instruction charging its time from bus activity would
    /// silently mis-time it. The recorded traces say the same thing from the
    /// other side: no case in either corpus has a length shorter than four
    /// clocks per transfer.
    #[test]
    fn every_mode_costs_a_whole_number_of_bus_cycles_plus_its_internal_time() {
        for size in [Size::Byte, Size::Word, Size::Long] {
            for mode in 0..8u8 {
                // Mode 7 selects one of five encodings by register; the others
                // ignore it.
                let regs: &[u8] = if mode == 7 { &[0, 1, 2, 3, 4] } else { &[0] };
                for &reg in regs {
                    let total = ea_cycles(mode, reg, size);
                    let internal = ea_internal(mode, reg);
                    assert!(
                        total >= internal,
                        "mode {mode} reg {reg} {size:?}: internal {internal} exceeds total {total}"
                    );
                    assert_eq!(
                        (total - internal) % 4,
                        0,
                        "mode {mode} reg {reg} {size:?}: {total} less {internal} internal \
                         is not a whole number of four-clock transfers"
                    );
                }
            }
        }
    }

    fn setup() -> (M68000, WordBus) {
        (M68000::new(), WordBus::new())
    }

    // --- Memory access primitives ---

    #[test]
    fn word_access_is_big_endian() {
        let (mut cpu, mut bus) = setup();
        bus.load(0x1000, &[0x12, 0x34]);
        assert_eq!(cpu.read_word_at(&mut bus, M, 0x1000).unwrap(), 0x1234);

        // A write is handed to the bus unit rather than driven, so memory does
        // not change until that unit runs the cycle. Inside an instruction the
        // finish drives it; here the test has to say so.
        cpu.write_word_at(&mut bus, M, 0x2000, 0xBEEF).unwrap();
        assert_eq!(
            &bus.memory[0x2000..0x2002],
            &[0, 0],
            "the cycle has not run yet"
        );
        cpu.flush_pending(&mut bus, M);
        assert_eq!(&bus.memory[0x2000..0x2002], &[0xBE, 0xEF]);
    }

    #[test]
    fn long_access_is_two_words_high_first() {
        let (mut cpu, mut bus) = setup();
        bus.load(0x1000, &[0x12, 0x34, 0x56, 0x78]);
        assert_eq!(cpu.read_long_at(&mut bus, M, 0x1000).unwrap(), 0x1234_5678);

        cpu.write_long_at(&mut bus, M, 0x2000, 0xDEAD_BEEF).unwrap();
        cpu.flush_pending(&mut bus, M);
        assert_eq!(&bus.memory[0x2000..0x2004], &[0xDE, 0xAD, 0xBE, 0xEF]);
    }

    #[test]
    fn byte_read_selects_high_or_low_byte() {
        let (mut cpu, mut bus) = setup();
        bus.load(0x1000, &[0xAB, 0xCD]);
        assert_eq!(cpu.read_byte_at(&mut bus, M, 0x1000), 0xAB, "even = UDS");
        assert_eq!(
            cpu.read_byte_at(&mut bus, M, 0x1001),
            0xCD,
            "odd = LDS, byte access is never misaligned"
        );
    }

    #[test]
    fn byte_write_preserves_other_byte_of_word() {
        let (mut cpu, mut bus) = setup();
        bus.load(0x1000, &[0xAB, 0xCD]);
        cpu.write_byte_at(&mut bus, M, 0x1000, 0x11);
        cpu.flush_pending(&mut bus, M);
        assert_eq!(&bus.memory[0x1000..0x1002], &[0x11, 0xCD]);
        cpu.write_byte_at(&mut bus, M, 0x1001, 0x22);
        cpu.flush_pending(&mut bus, M);
        assert_eq!(&bus.memory[0x1000..0x1002], &[0x11, 0x22]);
    }

    #[test]
    fn odd_word_access_raises_address_error() {
        let (mut cpu, mut bus) = setup();
        cpu.set_pc_flush(0x0C04); // pretend one extension word was consumed
        let err = cpu.read_word_at(&mut bus, M, 0x1001).unwrap_err();
        assert_eq!(err.addr, 0x1001);
        assert!(!err.write);
        assert!(!err.program);
        assert_eq!(err.stacked_pc, 0x0C02, "operand faults stack PC - 2");

        let err = cpu.write_word_at(&mut bus, M, 0x1001, 0).unwrap_err();
        assert!(err.write);

        let err = cpu.read_long_at(&mut bus, M, 0x1003).unwrap_err();
        assert_eq!(err.addr, 0x1003, "the first (odd) word transaction faults");
    }

    #[test]
    fn read_imm_word_advances_pc() {
        let (mut cpu, mut bus) = setup();
        cpu.set_pc_flush(0x1000);
        bus.load(0x1000, &[0x4E, 0x71]);
        assert_eq!(cpu.read_imm_word(&mut bus, M), 0x4E71);
        assert_eq!(cpu.pc, 0x1002);
    }

    // --- decode_ea: register and indirect modes ---

    #[test]
    fn decode_register_direct_modes() {
        let (mut cpu, mut bus) = setup();
        assert_eq!(cpu.decode_ea(&mut bus, M, 0, 3, Size::Word), Ea::DataReg(3));
        assert_eq!(cpu.decode_ea(&mut bus, M, 1, 5, Size::Long), Ea::AddrReg(5));
    }

    #[test]
    fn decode_address_indirect() {
        let (mut cpu, mut bus) = setup();
        cpu.a[2] = 0x3000;
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 2, 2, Size::Word),
            Ea::Mem(0x3000)
        );
        assert_eq!(cpu.a[2], 0x3000, "plain indirect must not adjust An");
    }

    #[test]
    fn decode_postincrement_steps_by_size() {
        let (mut cpu, mut bus) = setup();
        for (size, step) in [(Size::Byte, 1), (Size::Word, 2), (Size::Long, 4)] {
            cpu.a[1] = 0x3000;
            let ea = cpu.decode_ea(&mut bus, M, 3, 1, size);
            assert_eq!(ea, Ea::Mem(0x3000), "address is pre-increment value");
            assert_eq!(cpu.a[1], 0x3000 + step, "step for {size:?}");
        }
    }

    #[test]
    fn decode_predecrement_steps_by_size() {
        let (mut cpu, mut bus) = setup();
        for (size, step) in [(Size::Byte, 1), (Size::Word, 2), (Size::Long, 4)] {
            cpu.a[1] = 0x3000;
            let ea = cpu.decode_ea(&mut bus, M, 4, 1, size);
            assert_eq!(
                ea,
                Ea::Mem(0x3000 - step),
                "address is post-decrement value"
            );
            assert_eq!(cpu.a[1], 0x3000 - step, "step for {size:?}");
        }
    }

    #[test]
    fn a7_byte_postincrement_and_predecrement_step_by_two() {
        let (mut cpu, mut bus) = setup();
        cpu.a[7] = 0x3000;
        let ea = cpu.decode_ea(&mut bus, M, 3, 7, Size::Byte);
        assert_eq!(ea, Ea::Mem(0x3000));
        assert_eq!(cpu.a[7], 0x3002, "A7 byte (An)+ keeps SP word-aligned");

        cpu.a[7] = 0x3000;
        let ea = cpu.decode_ea(&mut bus, M, 4, 7, Size::Byte);
        assert_eq!(ea, Ea::Mem(0x2FFE));
        assert_eq!(cpu.a[7], 0x2FFE, "A7 byte -(An) keeps SP word-aligned");
    }

    #[test]
    fn decode_displacement_16() {
        let (mut cpu, mut bus) = setup();
        cpu.a[0] = 0x3000;
        cpu.set_pc_flush(0x1000);
        bus.load(0x1000, &[0x00, 0x10]); // +0x10
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 5, 0, Size::Word),
            Ea::Mem(0x3010)
        );
        assert_eq!(cpu.pc, 0x1002, "one extension word consumed");

        // Negative displacement
        cpu.set_pc_flush(0x1000);
        bus.load(0x1000, &[0xFF, 0xF0]); // -0x10
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 5, 0, Size::Word),
            Ea::Mem(0x2FF0)
        );
    }

    #[test]
    fn decode_indexed_word_index_sign_extends() {
        let (mut cpu, mut bus) = setup();
        cpu.a[0] = 0x3000;
        cpu.d[2] = 0xFFFF_FFF0; // low word = -0x10 when sign-extended
        cpu.set_pc_flush(0x1000);
        // Brief extension: D2.w index (D/A=0, reg=2, W/L=0), disp8 = +4
        bus.load(0x1000, &[0x20, 0x04]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 6, 0, Size::Word),
            Ea::Mem(0x3000 - 0x10 + 4)
        );
    }

    #[test]
    fn decode_indexed_long_index_uses_full_register() {
        let (mut cpu, mut bus) = setup();
        cpu.a[0] = 0x0010_0000;
        cpu.d[2] = 0x0000_1000;
        cpu.set_pc_flush(0x1000);
        // Brief extension: D2.l index (W/L=1), disp8 = 0
        bus.load(0x1000, &[0x28, 0x00]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 6, 0, Size::Word),
            Ea::Mem(0x0010_1000)
        );
    }

    #[test]
    fn decode_indexed_address_register_index() {
        let (mut cpu, mut bus) = setup();
        cpu.a[0] = 0x3000;
        cpu.a[3] = 0x0000_0100;
        cpu.set_pc_flush(0x1000);
        // Brief extension: A3.l index (D/A=1, reg=3, W/L=1), disp8 = -2
        bus.load(0x1000, &[0xB8, 0xFE]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 6, 0, Size::Word),
            Ea::Mem(0x3000 + 0x100 - 2)
        );
    }

    #[test]
    fn decode_indexed_negative_disp8() {
        let (mut cpu, mut bus) = setup();
        cpu.a[1] = 0x3000;
        cpu.d[0] = 0;
        cpu.set_pc_flush(0x1000);
        // D0.w index = 0, disp8 = -0x80 (most negative)
        bus.load(0x1000, &[0x00, 0x80]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 6, 1, Size::Word),
            Ea::Mem(0x3000 - 0x80)
        );
    }

    #[test]
    fn scale_field_ignored_on_68000() {
        let (mut cpu, mut bus) = setup();
        cpu.a[0] = 0x3000;
        cpu.d[1] = 0x10;
        cpu.set_pc_flush(0x1000);
        // D1.l index with scale bits = 3 (×8 on 68020+): 68000 ignores scale
        bus.load(0x1000, &[0x1E, 0x00]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 6, 0, Size::Word),
            Ea::Mem(0x3010)
        );

        // Same encoding on a 68020 applies the scale
        let (mut cpu, mut bus) = setup();
        cpu.variant = super::super::M68kVariant::M68020;
        cpu.a[0] = 0x3000;
        cpu.d[1] = 0x10;
        cpu.set_pc_flush(0x1000);
        bus.load(0x1000, &[0x1E, 0x00]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 6, 0, Size::Word),
            Ea::Mem(0x3000 + (0x10 << 3))
        );
    }

    // --- decode_ea: mode 7 submodes ---

    #[test]
    fn decode_absolute_short_sign_extends() {
        let (mut cpu, mut bus) = setup();
        cpu.set_pc_flush(0x1000);
        bus.load(0x1000, &[0x20, 0x00]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 7, 0, Size::Word),
            Ea::Mem(0x2000)
        );

        // $8000 sign-extends to the full $FFFF8000 (the bus masks to
        // $FF8000 on access; JMP/LEA would see all 32 bits)
        cpu.set_pc_flush(0x1000);
        bus.load(0x1000, &[0x80, 0x00]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 7, 0, Size::Word),
            Ea::Mem(0xFFFF_8000)
        );
    }

    #[test]
    fn decode_absolute_long() {
        let (mut cpu, mut bus) = setup();
        cpu.set_pc_flush(0x1000);
        bus.load(0x1000, &[0x00, 0x12, 0x34, 0x56]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 7, 1, Size::Word),
            Ea::Mem(0x0012_3456)
        );
        assert_eq!(cpu.pc, 0x1004, "two extension words consumed");
    }

    #[test]
    fn decode_pc_relative_base_is_extension_word_address() {
        let (mut cpu, mut bus) = setup();
        cpu.set_pc_flush(0x1000); // extension word lives here
        bus.load(0x1000, &[0x01, 0x00]); // +0x100
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 7, 2, Size::Word),
            Ea::Mem(0x1100)
        );
    }

    #[test]
    fn decode_pc_indexed() {
        let (mut cpu, mut bus) = setup();
        cpu.set_pc_flush(0x1000);
        cpu.d[4] = 0x20;
        // Brief extension: D4.l index, disp8 = +6; base = 0x1000
        bus.load(0x1000, &[0x48, 0x06]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 7, 3, Size::Word),
            Ea::Mem(0x1026)
        );
    }

    #[test]
    fn decode_immediate_by_size() {
        let (mut cpu, mut bus) = setup();
        cpu.set_pc_flush(0x1000);
        bus.load(0x1000, &[0x12, 0x34]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 7, 4, Size::Byte),
            Ea::Imm(0x34),
            "byte immediate is the low byte of one extension word"
        );
        assert_eq!(cpu.pc, 0x1002);

        cpu.set_pc_flush(0x1000);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 7, 4, Size::Word),
            Ea::Imm(0x1234)
        );

        cpu.set_pc_flush(0x1000);
        bus.load(0x1000, &[0x12, 0x34, 0x56, 0x78]);
        assert_eq!(
            cpu.decode_ea(&mut bus, M, 7, 4, Size::Long),
            Ea::Imm(0x1234_5678)
        );
        assert_eq!(cpu.pc, 0x1004);
    }

    #[test]
    fn decode_keeps_full_32_bit_address_and_bus_access_masks() {
        let (mut cpu, mut bus) = setup();
        cpu.a[0] = 0xFF12_3456;
        let ea = cpu.decode_ea(&mut bus, M, 2, 0, Size::Word);
        assert_eq!(ea, Ea::Mem(0xFF12_3456), "EA computed at full width");
        // mask_addr (separately unit-tested) truncates to 24 bits at the
        // word/byte access layer, so reads through the full-width EA work.
        cpu.ea_write(&mut bus, M, ea, Size::Word, 0xBEEF).unwrap();
        assert_eq!(cpu.ea_read(&mut bus, M, ea, Size::Word).unwrap(), 0xBEEF);
    }

    // --- ea_read / ea_write ---

    #[test]
    fn ea_read_data_register_masks_to_size() {
        let (mut cpu, mut bus) = setup();
        cpu.d[1] = 0x8765_4321;
        assert_eq!(
            cpu.ea_read(&mut bus, M, Ea::DataReg(1), Size::Byte)
                .unwrap(),
            0x21
        );
        assert_eq!(
            cpu.ea_read(&mut bus, M, Ea::DataReg(1), Size::Word)
                .unwrap(),
            0x4321
        );
        assert_eq!(
            cpu.ea_read(&mut bus, M, Ea::DataReg(1), Size::Long)
                .unwrap(),
            0x8765_4321
        );
    }

    #[test]
    fn ea_read_address_register_masks_to_size() {
        let (mut cpu, mut bus) = setup();
        cpu.a[2] = 0x8765_4321;
        assert_eq!(
            cpu.ea_read(&mut bus, M, Ea::AddrReg(2), Size::Word)
                .unwrap(),
            0x4321
        );
        assert_eq!(
            cpu.ea_read(&mut bus, M, Ea::AddrReg(2), Size::Long)
                .unwrap(),
            0x8765_4321
        );
    }

    #[test]
    fn ea_write_data_register_preserves_upper_bits() {
        let (mut cpu, mut bus) = setup();
        cpu.d[3] = 0xAABB_CCDD;
        cpu.ea_write(&mut bus, M, Ea::DataReg(3), Size::Byte, 0x11)
            .unwrap();
        assert_eq!(cpu.d[3], 0xAABB_CC11);
        cpu.ea_write(&mut bus, M, Ea::DataReg(3), Size::Word, 0x2222)
            .unwrap();
        assert_eq!(cpu.d[3], 0xAABB_2222);
        cpu.ea_write(&mut bus, M, Ea::DataReg(3), Size::Long, 0x3333_3333)
            .unwrap();
        assert_eq!(cpu.d[3], 0x3333_3333);
    }

    #[test]
    fn ea_write_address_register_word_sign_extends() {
        let (mut cpu, mut bus) = setup();
        cpu.a[4] = 0xAABB_CCDD;
        cpu.ea_write(&mut bus, M, Ea::AddrReg(4), Size::Word, 0x8000)
            .unwrap();
        assert_eq!(cpu.a[4], 0xFFFF_8000, "negative word fills upper bits");
        cpu.ea_write(&mut bus, M, Ea::AddrReg(4), Size::Word, 0x7FFF)
            .unwrap();
        assert_eq!(cpu.a[4], 0x0000_7FFF, "positive word clears upper bits");
    }

    #[test]
    fn ea_memory_round_trip_all_sizes() {
        let (mut cpu, mut bus) = setup();
        let ea = Ea::Mem(0x4000);

        cpu.ea_write(&mut bus, M, ea, Size::Long, 0x1122_3344)
            .unwrap();
        assert_eq!(
            cpu.ea_read(&mut bus, M, ea, Size::Long).unwrap(),
            0x1122_3344
        );
        assert_eq!(cpu.ea_read(&mut bus, M, ea, Size::Word).unwrap(), 0x1122);
        assert_eq!(cpu.ea_read(&mut bus, M, ea, Size::Byte).unwrap(), 0x11);

        cpu.ea_write(&mut bus, M, Ea::Mem(0x4001), Size::Byte, 0xFF)
            .unwrap();
        assert_eq!(
            cpu.ea_read(&mut bus, M, ea, Size::Long).unwrap(),
            0x11FF_3344,
            "byte write into the middle of the long"
        );
    }

    #[test]
    fn ea_read_immediate_masks_to_size() {
        let (mut cpu, mut bus) = setup();
        assert_eq!(
            cpu.ea_read(&mut bus, M, Ea::Imm(0x1234_5678), Size::Byte)
                .unwrap(),
            0x78
        );
        assert_eq!(
            cpu.ea_read(&mut bus, M, Ea::Imm(0x1234_5678), Size::Word)
                .unwrap(),
            0x5678
        );
    }

    #[test]
    fn size_helpers() {
        assert_eq!(Size::Byte.bytes(), 1);
        assert_eq!(Size::Word.bytes(), 2);
        assert_eq!(Size::Long.bytes(), 4);
        assert_eq!(Size::Byte.mask(), 0xFF);
        assert_eq!(Size::Word.mask(), 0xFFFF);
        assert_eq!(Size::Long.mask(), 0xFFFF_FFFF);
        assert_eq!(Size::Byte.sign_bit(), 0x80);
        assert_eq!(Size::Word.sign_bit(), 0x8000);
        assert_eq!(Size::Long.sign_bit(), 0x8000_0000);
    }
}
