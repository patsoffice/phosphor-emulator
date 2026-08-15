use super::{CcFlag, ExecState, M6809};
use crate::core::{Bus, BusMaster};
use crate::cpu::m68xx::M68xxAlu;

mod binary;
mod shift;
mod unary;
mod word;

impl M6809 {
    /// Helper to set N, Z, V, C flags for 16-bit arithmetic
    #[inline]
    pub(crate) fn set_flags_arithmetic16(&mut self, result: u16, overflow: bool, carry: bool) {
        self.set_flag(CcFlag::N, result & 0x8000 != 0);
        self.set_flag(CcFlag::Z, result == 0);
        self.set_flag(CcFlag::V, overflow);
        self.set_flag(CcFlag::C, carry);
    }

    /// The alu_imm function is a generic helper method designed to reduce code duplication for Immediate Addressing Mode ALU instructions (like ADDA #$10, ANDB #$FF, etc.).
    ///
    /// In the Motorola 6809, immediate mode instructions always follow a specific pattern.
    #[inline]
    pub(crate) fn alu_imm<B: Bus<Address = u16, Data = u8> + ?Sized, F>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
        operation: F,
    ) where
        F: FnOnce(&mut Self, u8),
    {
        if cycle == 0 {
            // 1. Fetch the operand from memory at PC
            let operand = bus.read(master, self.pc);
            // 2. Advance PC to the next instruction
            self.pc = self.pc.wrapping_add(1);
            // 3. Run the specific ALU logic provided by the caller
            operation(self, operand);
            // 4. Return to Fetch state for the next instruction
            self.state = ExecState::Fetch;
        }
    }

    /// ORCC immediate (0x1A): OR immediate value into CC register.
    /// All CC bits may be set by the OR operand.
    /// 3 total cycles: 1 fetch + 2 exec (read operand + internal apply).
    pub(crate) fn op_orcc<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                self.scratch = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.state = ExecState::Execute(0x1A, 1);
            }
            1 => {
                // Don't-care cycle at PC; the OR is applied on it
                self.dummy_at_pc(bus, master, 0);
                self.cc |= self.scratch;
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// ANDCC immediate (0x1C): AND immediate value into CC register.
    /// Used to clear specific CC bits (e.g., ANDCC #$FE clears C flag).
    /// 3 total cycles: 1 fetch + 2 exec (read operand + internal apply).
    pub(crate) fn op_andcc<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                self.scratch = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.state = ExecState::Execute(0x1C, 1);
            }
            1 => {
                // Don't-care cycle at PC; the AND is applied on it
                self.dummy_at_pc(bus, master, 0);
                self.cc &= self.scratch;
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// Generic helper for Direct Addressing Mode ALU instructions.
    /// Three execute cycles: cycle 0 fetches the address byte and forms DP:addr,
    /// cycle 1 is the address-computation don't-care, cycle 2 reads the operand
    /// and runs the operation.
    #[inline]
    pub(crate) fn alu_direct<B: Bus<Address = u16, Data = u8> + ?Sized, F>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
        operation: F,
    ) where
        F: FnOnce(&mut Self, u8),
    {
        match cycle {
            0 => {
                let addr = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = ((self.dp as u16) << 8) | addr;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(opcode, 2);
            }
            2 => {
                let operand = bus.read(master, self.temp_addr);
                operation(self, operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// Generic helper for Extended Addressing Mode ALU instructions.
    /// Four execute cycles:
    /// Cycle 0: Fetch high byte of address.
    /// Cycle 1: Fetch low byte of address, form effective address.
    /// Cycle 2: Address-computation don't-care.
    /// Cycle 3: Read operand from the effective address and run the operation.
    #[inline]
    pub(crate) fn alu_extended<B: Bus<Address = u16, Data = u8> + ?Sized, F>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
        operation: F,
    ) where
        F: FnOnce(&mut Self, u8),
    {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::Execute(opcode, 2);
            }
            2 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(opcode, 3);
            }
            3 => {
                let operand = bus.read(master, self.temp_addr);
                operation(self, operand);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    // --- Indexed Addressing Mode ---

    /// Returns the value of the index register selected by 2-bit code.
    /// 0=X, 1=Y, 2=U, 3=S.
    #[inline]
    fn indexed_reg_value(&self, sel: u8) -> u16 {
        match sel & 0x03 {
            0 => self.x,
            1 => self.y,
            2 => self.u,
            3 => self.s,
            _ => unreachable!(),
        }
    }

    /// Sets the index register selected by 2-bit code.
    #[inline]
    fn set_indexed_reg(&mut self, sel: u8, val: u16) {
        match sel & 0x03 {
            0 => self.x = val,
            1 => self.y = val,
            2 => self.u = val,
            3 => self.s = val,
            _ => unreachable!(),
        }
    }

    /// Sign-extends a 5-bit value to 16-bit.
    #[inline]
    fn sign_extend_5(val: u8) -> u16 {
        if val & 0x10 != 0 {
            (val as u16) | 0xFFE0
        } else {
            val as u16
        }
    }

    /// How many don't-care cycles an indexed mode spends forming its base
    /// address. Together with the offset bytes it reads from the instruction
    /// stream these account for the "+" in the datasheet cycle counts (e.g.
    /// LDA indexed = 4+). See `indexed_mode_pc_dummies` for which of them
    /// re-drive PC rather than holding $FFFF.
    ///
    /// Every mode spends at least one — `,R` has nothing to compute and still
    /// pays it. An indirect postbyte spends exactly these, then reads its
    /// pointer, then one final /VMA; the count below is the same either way.
    fn indexed_mode_dummies(postbyte: u8) -> u8 {
        if postbyte & 0x80 == 0 {
            return 1 + 1; // 5-bit constant offset
        }
        let base: u8 = match postbyte & 0x0F {
            0x00 => 2, // ,R+
            0x01 => 3, // ,R++
            0x02 => 2, // ,-R
            0x03 => 3, // ,--R
            0x04 => 0, // ,R (no offset)
            0x05 => 1, // B,R
            0x06 => 1, // A,R
            0x08 => 0, // n8,R (bus read already accounts for +1)
            0x09 => 2, // n16,R (bus reads give +2, need +2 more for total +4)
            0x0B => 4, // D,R
            0x0C => 0, // n8,PCR
            0x0D => 3, // n16,PCR (bus reads give +2, need +3 more for total +5)
            0x0F => 0, // [n16] extended indirect
            _ => 0,
        };
        base + 1
    }

    /// Whether a postbyte selects an indirect mode. Only the full postbyte
    /// forms have one: with bit 7 clear the whole low half is a 5-bit constant
    /// offset, so bit 4 there is offset data and not the indirect flag.
    fn indexed_is_indirect(postbyte: u8) -> bool {
        postbyte & 0x90 == 0x90
    }

    /// How many of an indexed mode's don't-care cycles re-drive the program
    /// counter instead of holding $FFFF. They are always the leading ones, and
    /// only `D,R` has two (at PC+0 then PC+1) — it is the one mode whose offset
    /// arrives in registers yet still costs two prefetch-shaped cycles.
    ///
    /// The modes with none are exactly those that already read their offset
    /// from the instruction stream over two bytes (`n16,R`, `n16,PCR`,
    /// `[n16]`) plus `n8,PCR`.
    fn indexed_mode_pc_dummies(postbyte: u8) -> u8 {
        if postbyte & 0x80 == 0 {
            return 1; // 5-bit constant offset
        }
        match postbyte & 0x0F {
            0x0B => 2,                      // D,R
            0x09 | 0x0C | 0x0D | 0x0F => 0, // n16,R / n8,PCR / n16,PCR / [n16]
            _ => 1,
        }
    }

    /// Multi-cycle indexed address resolution state machine.
    ///
    /// Reads the postbyte and any additional offset bytes from memory,
    /// computing the effective address in `self.temp_addr`.
    ///
    /// Returns `true` when the address is ready; `false` if more cycles are
    /// needed. The cycle it returns `true` on is always a don't-care, so the
    /// caller resumes on a cycle of its own that makes a real access.
    ///
    /// The order of those don't-cares is visible to anything that decodes the
    /// address bus. A direct postbyte spends its mode's don't-cares (state 20)
    /// and is done. An indirect one spends the *same* don't-cares first, then
    /// reads the two pointer bytes (states 10 and 11), then holds $FFFF for one
    /// final cycle (state 21):
    ///
    /// ```text
    ///     [base don't-cares]  ptr_hi  ptr_lo  $FFFF
    /// ```
    ///
    /// Uses `self.scratch` for the postbyte and indirect pointer high byte.
    /// Uses `self.indexed_internal` as a countdown for the base don't-cares.
    pub(crate) fn indexed_resolve<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) -> bool {
        self.indexed_resolve_inner(opcode, cycle, bus, master, ExecState::Execute)
    }

    pub(crate) fn indexed_resolve_page2<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) -> bool {
        self.indexed_resolve_inner(opcode, cycle, bus, master, ExecState::ExecutePage2)
    }

    pub(crate) fn indexed_resolve_page3<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) -> bool {
        self.indexed_resolve_inner(opcode, cycle, bus, master, ExecState::ExecutePage3)
    }

    fn indexed_resolve_inner<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
        mk_state: fn(u8, u8) -> ExecState,
    ) -> bool {
        match cycle {
            0 => {
                let postbyte = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.scratch = postbyte;

                // Fix the don't-care plan now: the postbyte alone decides how
                // many of this mode's don't-care cycles re-drive PC, and
                // `self.scratch` is reused for the pointer byte once an
                // indirect resolution starts.
                self.indexed_pc_dummies = Self::indexed_mode_pc_dummies(postbyte);
                self.indexed_pc_offset = 0;

                // The don't-care count is the same whether or not the mode is
                // indirect: state 20 spends them all before state 10 reads the
                // pointer, and the indirect form's extra cycle is the trailing
                // /VMA of state 21.
                self.indexed_internal = Self::indexed_mode_dummies(postbyte);

                if postbyte & 0x80 == 0 {
                    // 5-bit constant offset
                    let reg = self.indexed_reg_value((postbyte >> 5) & 0x03);
                    let offset = Self::sign_extend_5(postbyte & 0x1F);
                    self.temp_addr = reg.wrapping_add(offset);
                    self.state = mk_state(opcode, 20);
                    return false;
                }

                let reg_sel = (postbyte >> 5) & 0x03;
                let indirect = Self::indexed_is_indirect(postbyte);
                let mode = postbyte & 0x0F;
                let reg = self.indexed_reg_value(reg_sel);

                match mode {
                    0x00 if !indirect => {
                        // ,R+ (post-increment by 1, non-indirect only)
                        self.temp_addr = reg;
                        self.set_indexed_reg(reg_sel, reg.wrapping_add(1));
                        self.state = mk_state(opcode, 20);
                    }
                    0x01 => {
                        // ,R++ (post-increment by 2)
                        self.temp_addr = reg;
                        self.set_indexed_reg(reg_sel, reg.wrapping_add(2));
                        self.state = mk_state(opcode, 20);
                    }
                    0x02 if !indirect => {
                        // ,-R (pre-decrement by 1, non-indirect only)
                        let new_reg = reg.wrapping_sub(1);
                        self.set_indexed_reg(reg_sel, new_reg);
                        self.temp_addr = new_reg;
                        self.state = mk_state(opcode, 20);
                    }
                    0x03 => {
                        // ,--R (pre-decrement by 2)
                        let new_reg = reg.wrapping_sub(2);
                        self.set_indexed_reg(reg_sel, new_reg);
                        self.temp_addr = new_reg;
                        self.state = mk_state(opcode, 20);
                    }
                    0x04 => {
                        // ,R (no offset)
                        self.temp_addr = reg;
                        self.state = mk_state(opcode, 20);
                    }
                    0x05 => {
                        // B,R (accumulator B offset)
                        self.temp_addr = reg.wrapping_add(self.b as i8 as i16 as u16);
                        self.state = mk_state(opcode, 20);
                    }
                    0x06 => {
                        // A,R (accumulator A offset)
                        self.temp_addr = reg.wrapping_add(self.a as i8 as i16 as u16);
                        self.state = mk_state(opcode, 20);
                    }
                    0x0B => {
                        // D,R (accumulator D offset)
                        self.temp_addr = reg.wrapping_add(self.get_d());
                        self.state = mk_state(opcode, 20);
                    }
                    // n8,R / n8,PCR need 1 more byte; n16,R / n16,PCR / [n16]
                    // need 2, and cycle 1 splits them by mode.
                    0x08 | 0x09 | 0x0C | 0x0D => self.state = mk_state(opcode, 1),
                    0x0F if indirect => self.state = mk_state(opcode, 1),
                    _ => self.state = ExecState::Fetch,
                }
                false
            }
            1 => {
                let postbyte = self.scratch;
                let mode = postbyte & 0x0F;
                let reg_sel = (postbyte >> 5) & 0x03;

                match mode {
                    0x08 => {
                        // n8,R: read 8-bit signed offset
                        let offset = bus.read(master, self.pc) as i8;
                        self.pc = self.pc.wrapping_add(1);
                        let reg = self.indexed_reg_value(reg_sel);
                        self.temp_addr = reg.wrapping_add(offset as i16 as u16);
                        self.state = mk_state(opcode, 20);
                    }
                    0x0C => {
                        // n8,PCR: read 8-bit signed offset, PC-relative
                        let offset = bus.read(master, self.pc) as i8;
                        self.pc = self.pc.wrapping_add(1);
                        self.temp_addr = self.pc.wrapping_add(offset as i16 as u16);
                        self.state = mk_state(opcode, 20);
                    }
                    0x09 | 0x0D | 0x0F => {
                        // n16,R / n16,PCR / [n16]: read high byte of 16-bit offset
                        let high = bus.read(master, self.pc) as u16;
                        self.pc = self.pc.wrapping_add(1);
                        self.temp_addr = high << 8;
                        self.state = mk_state(opcode, 2);
                    }
                    _ => self.state = ExecState::Fetch,
                }
                false
            }
            2 => {
                let postbyte = self.scratch;
                let mode = postbyte & 0x0F;
                let reg_sel = (postbyte >> 5) & 0x03;

                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                let offset16 = self.temp_addr | low;

                match mode {
                    0x09 => {
                        let reg = self.indexed_reg_value(reg_sel);
                        self.temp_addr = reg.wrapping_add(offset16);
                    }
                    0x0D => {
                        self.temp_addr = self.pc.wrapping_add(offset16);
                    }
                    // [n16] extended indirect: the offset *is* the pointer
                    0x0F => self.temp_addr = offset16,
                    _ => {}
                }

                self.state = mk_state(opcode, 20);
                false
            }
            // Indirect resolution: read the 16-bit pointer at temp_addr. Only
            // reached once state 20 has spent every base don't-care.
            10 => {
                let high = bus.read(master, self.temp_addr);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = high;
                self.state = mk_state(opcode, 11);
                false
            }
            11 => {
                let low = bus.read(master, self.temp_addr) as u16;
                let high = (self.scratch as u16) << 8;
                self.temp_addr = high | low;
                self.state = mk_state(opcode, 21);
                false
            }
            // Don't-care countdown for the base address formation, ahead of any
            // pointer read.
            20 => {
                self.indexed_dummy(bus, master);
                self.indexed_internal -= 1;
                if self.indexed_internal > 0 {
                    self.state = mk_state(opcode, 20);
                    false
                } else if Self::indexed_is_indirect(self.scratch) {
                    self.state = mk_state(opcode, 10);
                    false
                } else {
                    true
                }
            }
            // The one don't-care that follows an indirect pointer read. The
            // mode's PC don't-cares were all spent back in state 20, so this is
            // always a /VMA cycle.
            21 => {
                self.dummy_vma(bus, master);
                true
            }
            _ => false,
        }
    }

    /// Generic helper for Indexed Addressing Mode ALU instructions.
    /// Variable execute cycles: address resolution via postbyte, then operand
    /// read on cycle 50.
    pub(crate) fn alu_indexed<B: Bus<Address = u16, Data = u8> + ?Sized, F>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
        operation: F,
    ) where
        F: FnOnce(&mut Self, u8),
    {
        match cycle {
            50 => {
                let operand = bus.read(master, self.temp_addr);
                operation(self, operand);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve(opcode, cycle, bus, master) {
                    self.state = ExecState::Execute(opcode, 50);
                }
            }
        }
    }

    /// Generic helper for Indexed Addressing Mode read-modify-write instructions.
    /// Used by memory-modify ops in the 0x60-0x6F range (NEG, COM, LSR, etc.).
    /// Cycle 40: read value from EA. Cycle 41: the modify don't-care. Cycle 42:
    /// write back (a second don't-care for TST — see the note on cycle 42).
    pub(crate) fn rmw_indexed<B: Bus<Address = u16, Data = u8> + ?Sized, F>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
        operation: F,
    ) where
        F: FnOnce(&mut Self, u8) -> u8,
    {
        match cycle {
            40 => {
                self.scratch = bus.read(master, self.temp_addr);
                self.state = ExecState::Execute(opcode, 41);
            }
            41 => {
                // The modify cycle. TST re-drives $FFFF here, the rest re-drive
                // PC — the same split the direct and extended helpers make.
                if opcode == 0x6D {
                    self.dummy_vma(bus, master);
                } else {
                    self.dummy_at_pc(bus, master, 0);
                }
                self.state = ExecState::Execute(opcode, 42);
            }
            42 => {
                let result = operation(self, self.scratch);
                // TST indexed (0x6D) shares RMW timing but does NOT write back.
                // The cycle-by-cycle chart gives the RMW group `data(EA) /
                // don't-care($FFFF) / write(EA)` and TST `data(EA) /
                // don't-care($FFFF) / don't-care($FFFF)`, so the final cycle is a
                // /VMA cycle, not a store. Writing back would corrupt destinations
                // where reads and writes decode differently — e.g. banked VRAM
                // where a read returns ROM but the write lands in video RAM.
                if opcode == 0x6D {
                    self.dummy_vma(bus, master);
                } else {
                    bus.write(master, self.temp_addr, result);
                }
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve(opcode, cycle, bus, master) {
                    self.state = ExecState::Execute(opcode, 40);
                }
            }
        }
    }

    /// Generic helper for Direct Addressing Mode read-modify-write instructions.
    /// Used by memory-modify ops in the 0x00-0x0F range (NEG, COM, LSR, etc.).
    /// Cycle 0: fetch address byte, form DP:addr.
    /// Cycle 1: address-computation don't-care.
    /// Cycle 2: read value from EA.
    /// Cycle 3: the modify don't-care.
    /// Cycle 4: write result back (a second don't-care for TST — see the note on cycle 4).
    pub(crate) fn rmw_direct<B: Bus<Address = u16, Data = u8> + ?Sized, F>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
        operation: F,
    ) where
        F: FnOnce(&mut Self, u8) -> u8,
    {
        match cycle {
            0 => {
                let addr = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = ((self.dp as u16) << 8) | addr;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(opcode, 2);
            }
            2 => {
                self.scratch = bus.read(master, self.temp_addr);
                self.state = ExecState::Execute(opcode, 3);
            }
            3 => {
                // The modify cycle; TST re-drives $FFFF, the rest re-drive PC
                if opcode == 0x0D {
                    self.dummy_vma(bus, master);
                } else {
                    self.dummy_at_pc(bus, master, 0);
                }
                self.state = ExecState::Execute(opcode, 4);
            }
            4 => {
                let result = operation(self, self.scratch);
                // TST direct (0x0D) shares RMW timing but does NOT write back — its
                // final cycle is a don't-care /VMA cycle, not a store. See the
                // rmw_indexed note.
                if opcode == 0x0D {
                    self.dummy_vma(bus, master);
                } else {
                    bus.write(master, self.temp_addr, result);
                }
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// Generic helper for Extended Addressing Mode read-modify-write instructions.
    /// Used by memory-modify ops in the 0x70-0x7F range (NEG, COM, LSR, etc.).
    /// Cycle 0: fetch address high byte.
    /// Cycle 1: fetch address low byte.
    /// Cycle 2: address-computation don't-care.
    /// Cycle 3: read value from EA.
    /// Cycle 4: the modify don't-care.
    /// Cycle 5: write result back (a second don't-care for TST — see the note on cycle 5).
    pub(crate) fn rmw_extended<B: Bus<Address = u16, Data = u8> + ?Sized, F>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
        operation: F,
    ) where
        F: FnOnce(&mut Self, u8) -> u8,
    {
        match cycle {
            0 => {
                let high = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr = high << 8;
                self.state = ExecState::Execute(opcode, 1);
            }
            1 => {
                let low = bus.read(master, self.pc) as u16;
                self.pc = self.pc.wrapping_add(1);
                self.temp_addr |= low;
                self.state = ExecState::Execute(opcode, 2);
            }
            2 => {
                // Address-computation don't-care cycle (/VMA)
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(opcode, 3);
            }
            3 => {
                self.scratch = bus.read(master, self.temp_addr);
                self.state = ExecState::Execute(opcode, 4);
            }
            4 => {
                // The modify cycle; TST re-drives $FFFF, the rest re-drive PC
                if opcode == 0x7D {
                    self.dummy_vma(bus, master);
                } else {
                    self.dummy_at_pc(bus, master, 0);
                }
                self.state = ExecState::Execute(opcode, 5);
            }
            5 => {
                let result = operation(self, self.scratch);
                // TST extended (0x7D) shares RMW timing but does NOT write back —
                // its final cycle is a don't-care /VMA cycle, not a store. See the
                // rmw_indexed note.
                if opcode == 0x7D {
                    self.dummy_vma(bus, master);
                } else {
                    bus.write(master, self.temp_addr, result);
                }
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// Generic helper for Page 2 Indexed Addressing Mode ALU instructions.
    /// Same as `alu_indexed` but uses `ExecutePage2` state transitions.
    #[allow(dead_code)]
    pub(crate) fn alu_indexed_page2<B: Bus<Address = u16, Data = u8> + ?Sized, F>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
        operation: F,
    ) where
        F: FnOnce(&mut Self, u8),
    {
        match cycle {
            50 => {
                let operand = bus.read(master, self.temp_addr);
                operation(self, operand);
                self.state = ExecState::Fetch;
            }
            _ => {
                if self.indexed_resolve_page2(opcode, cycle, bus, master) {
                    self.state = ExecState::ExecutePage2(opcode, 50);
                }
            }
        }
    }
}
