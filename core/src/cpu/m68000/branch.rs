//! Control flow: BRA / BSR / Bcc (line 0x6), DBcc (line 0x5), and
//! JMP / JSR / RTS / RTR (line 0x4).
//!
//! Branch displacements are relative to the address of the word following
//! the opcode (where the optional 16-bit displacement word lives), for both
//! the 8-bit and 16-bit forms. Conditions are evaluated by
//! [`M68000::cc_true`]; none of these instructions alter the CCR except
//! RTR, which exists to restore it.
//!
//! A control-flow target at an odd address raises an address error
//! (vector 3) at the target fetch, aborting before PC is loaded; the
//! frame stacks `target - 4` and a program-space function code (empirical,
//! from the hardware-derived test vectors).

use super::M68000;
use super::addressing::{AccessResult, AddressError, Ea, Size, sext8, sext16};
use crate::core::{Bus16, BusMaster};

/// Documented JMP timing per control addressing mode (M68000UM table 8-1);
/// JSR is uniformly 8 cycles more for the return-address push.
/// Clocks a jump spends off the bus computing its target, by addressing mode.
///
/// Irregular in a way the operand modes are not, because a jump has no operand
/// transfer to overlap the work with: `(An)` has nothing to fetch and so pays
/// all four clocks of the load, `abs.l` pays none because its two extension
/// words cover it, and the indexed modes pay six for the index add on top.
fn jump_internal(mode: u8, reg: u8) -> u32 {
    match mode & 7 {
        2 => 4, // (An)
        5 => 2, // d16(An)
        6 => 6, // d8(An,Xn)
        _ => match reg & 7 {
            0 => 2, // abs.w
            1 => 0, // abs.l
            2 => 2, // d16(PC)
            _ => 6, // d8(PC,Xn)
        },
    }
}

impl M68000 {
    /// Load a new PC; an odd target raises the address error a real 68000
    /// takes on the target fetch (program-space read, stacked PC =
    /// target - 4) and PC is left for the exception entry to set.
    #[inline]
    pub(crate) fn set_pc_checked(&mut self, target: u32) -> AccessResult<()> {
        if target & 1 != 0 {
            return Err(AddressError {
                addr: target,
                write: false,
                program: true,
                stacked_pc: target.wrapping_sub(4),
            });
        }
        self.pc = target;
        Ok(())
    }

    /// BRA / BSR / Bcc `<label>` (line 0x6): PC-relative branch. The low
    /// opcode byte is the 8-bit displacement; zero selects the 16-bit form
    /// (one extension word). Condition 0 is BRA (always taken), condition 1
    /// is BSR, which pushes the return address — the word after the whole
    /// instruction — before branching.
    ///
    /// Flags: none.
    /// Cycles: taken 10 (BSR 18); not taken 8 (byte) / 12 (word).
    pub(crate) fn op_bcc<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        // The displacement base is the address of the word after the opcode.
        let base = self.pc;
        let disp8 = opcode as u8;
        let (disp, word_form) = if disp8 == 0 {
            (sext16(self.read_imm_word(bus, master)), true)
        } else {
            // disp8 == 0xFF selects a 32-bit displacement on 68020+ only;
            // the 68000 takes it as -1.
            (sext8(disp8), false)
        };
        let cond = ((opcode >> 8) & 0xF) as u8;
        match cond {
            // BSR: the return address is past the displacement word
            1 => {
                self.push_long(bus, master, self.pc)?;
                self.set_pc_checked(base.wrapping_add(disp))?;
                // A taken branch costs the same whichever displacement it
                // used, so the byte form, having one fetch fewer, spends four
                // more clocks off the bus than the word form does.
                self.finish_from_bus(if word_form { 2 } else { 6 });
            }
            // BRA (condition 0 encodes T) and taken Bcc
            _ if self.cc_true(cond) => {
                self.set_pc_checked(base.wrapping_add(disp))?;
                self.finish_from_bus(if word_form { 2 } else { 6 });
            }
            // Not taken: nothing is redirected, and the two forms differ only
            // by the extension word, which counts itself.
            _ => self.finish_from_bus(4),
        }
        Ok(())
    }

    /// DBcc Dn,`<label>` (line 0x5, size bits 11, EA mode 001): loop
    /// primitive. If the condition holds, fall through. Otherwise decrement
    /// the low word of Dn (upper word untouched) and branch back unless the
    /// counter wrapped from 0 to -1.
    ///
    /// Flags: none.
    /// Cycles: condition true 12; loop taken 10; counter expired 14.
    pub(crate) fn op_dbcc<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let base = self.pc;
        let disp = sext16(self.read_imm_word(bus, master));
        let cond = ((opcode >> 8) & 0xF) as u8;
        if self.cc_true(cond) {
            // Condition satisfied: the loop is abandoned without touching the
            // counter, and the displacement word has already been fetched.
            self.finish_from_bus(4);
            return Ok(());
        }
        let reg = (opcode & 7) as usize;
        let counter = (self.d[reg] as u16).wrapping_sub(1);
        self.d[reg] = (self.d[reg] & 0xFFFF_0000) | counter as u32;
        if counter == 0xFFFF {
            // Counter ran out: two clocks more than the looping case, spent
            // recognizing the underflow rather than redirecting.
            self.finish_from_bus(6);
        } else {
            self.set_pc_checked(base.wrapping_add(disp))?;
            self.finish_from_bus(2);
        }
        Ok(())
    }

    /// JMP `<ea>` / JSR `<ea>` (0x4EC0 / 0x4E80): load PC from a
    /// control-mode effective address. JSR first pushes the return address —
    /// the word after the extension words, which is where PC sits once the
    /// EA is decoded.
    ///
    /// Flags: none.
    pub(crate) fn op_jmp_jsr<B: Bus16 + ?Sized>(
        &mut self,
        opcode: u16,
        bus: &mut B,
        master: BusMaster,
        call: bool,
    ) -> AccessResult<()> {
        let ea_mode = ((opcode >> 3) & 7) as u8;
        let ea_reg = (opcode & 7) as u8;
        // Control addressing only: register direct, (An)+/-(An), and #imm
        // are illegal here (the exception lands with full illegal coverage).
        if !(matches!(ea_mode, 2 | 5 | 6) || (ea_mode == 7 && ea_reg < 4)) {
            self.finish_from_bus(0);
            return Ok(());
        }
        // The size only governs operand access, which never happens for an
        // address-only decode; control modes have no side effects.
        let Ea::Mem(target) = self.decode_ea(bus, master, ea_mode, ea_reg, Size::Word) else {
            unreachable!("control addressing modes always resolve to memory");
        };
        // JSR faults on an odd target *before* pushing the return address
        // (unlike BSR, which pushes first) — hardware-verified.
        let return_pc = self.pc;
        self.set_pc_checked(target)?;
        if call {
            self.push_long(bus, master, return_pc)?;
        }
        // JSR's push is two counted transfers, so a call and a jump spend the
        // same time off the bus.
        self.finish_from_bus(jump_internal(ea_mode, ea_reg));
        Ok(())
    }

    /// RTS (0x4E75): pop the return address into PC.
    ///
    /// Flags: none. 16 cycles.
    pub(crate) fn op_rts<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let target = self.pop_long(bus, master)?;
        self.set_pc_checked(target)?;
        // The opcode and the long pop are counted; four clocks are left to
        // redirect to the popped address.
        self.finish_from_bus(4);
        Ok(())
    }

    /// RTR (0x4E77): pop a word into the CCR, then pop the return address
    /// into PC. Pairs with a MOVE SR,-(SP) / PEA-style prologue to restore
    /// caller flags.
    ///
    /// Flags: X/N/Z/V/C loaded from the stacked word (only the five
    /// implemented CCR bits; the system byte is untouched). 20 cycles.
    pub(crate) fn op_rtr<B: Bus16 + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) -> AccessResult<()> {
        let ccr = self.pop_word(bus, master)?;
        self.sr = (self.sr & 0xFF00) | (ccr & 0x001F);
        let target = self.pop_long(bus, master)?;
        self.set_pc_checked(target)?;
        // As RTS, plus the counted word pop that restored the flags.
        self.finish_from_bus(4);
        Ok(())
    }
}
