use super::{CcFlag, ExecState, InterruptType, M6809};
use crate::core::{Bus, BusMaster};

impl M6809 {
    // Stack operation state management using temp_addr:
    // Low byte (0-7): The post-byte mask (remaining registers to process)
    // High byte (8-15):
    //   Bit 8: "Second byte" flag for 16-bit registers (0=First/Low, 1=Second/High)
    //   Bits 12-15: Current bit index being processed (0-7)

    /// Generic PUSH operation (PSHS/PSHU)
    /// Pushes registers from High ID (PC=7) to Low ID (CC=0).
    /// Stack grows downward.
    /// 16-bit regs: Push Low byte, then High byte (so High is at lower addr).
    /// Timing: 5+n cycles — 1 fetch + 1 read postbyte + 2 /VMA don't-care +
    /// 1 read at the stack pointer + n push bytes. The read is real: the CPU
    /// probes the stack top before it starts writing.
    fn op_push<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
        use_u: bool,
    ) {
        if cycle == 0 {
            // Cycle 0: Read post-byte
            let mask = bus.read(master, self.pc);
            self.pc = self.pc.wrapping_add(1);
            // Initialize state: Mask in low byte, no current bit selected (high byte 0)
            self.temp_addr = mask as u16;
            self.state = ExecState::Execute(opcode, 1);
            return;
        }

        // Cycles 1-2: /VMA don't-care cycles
        if cycle <= 2 {
            self.dummy_vma(bus, master);
            self.state = ExecState::Execute(opcode, cycle + 1);
            return;
        }

        // Cycle 3: probe the stack top, then start pushing
        if cycle == 3 {
            let sp = if use_u { self.u } else { self.s };
            let _ = bus.read(master, sp);
            self.state = if (self.temp_addr & 0xFF) == 0 {
                ExecState::Fetch // empty postbyte: nothing to push
            } else {
                ExecState::Execute(opcode, 4)
            };
            return;
        }

        // Cycle 4+: Process registers
        // Check if we are currently processing a 16-bit register (second byte)
        let mut mask = (self.temp_addr & 0xFF) as u8;
        let state = (self.temp_addr >> 8) as u8;
        let second_byte = (state & 0x01) != 0;
        let mut current_bit = 0;

        // If not in the middle of a 16-bit reg, find next register
        if !second_byte {
            // PSH order: PC(7), U/S(6), Y(5), X(4), DP(3), B(2), A(1), CC(0)
            // Find highest set bit
            for i in (0..=7).rev() {
                if (mask & (1 << i)) != 0 {
                    current_bit = i;
                    break;
                }
            }
        } else {
            // Recover current bit from state if needed, though we can re-derive it
            // Actually we need to know which bit we are on if we are in second byte
            // But since we don't clear the mask bit until finished, the loop above finds it again.
            for i in (0..=7).rev() {
                if (mask & (1 << i)) != 0 {
                    current_bit = i;
                    break;
                }
            }
        }

        // Perform Push
        let sp = if use_u { self.u } else { self.s };
        let write_addr = sp.wrapping_sub(1);

        let val_to_push = match current_bit {
            7 => {
                if second_byte {
                    (self.pc >> 8) as u8
                } else {
                    self.pc as u8
                }
            } // PC
            6 => {
                // U or S
                let val = if use_u { self.s } else { self.u };
                if second_byte {
                    (val >> 8) as u8
                } else {
                    val as u8
                }
            }
            5 => {
                if second_byte {
                    (self.y >> 8) as u8
                } else {
                    self.y as u8
                }
            } // Y
            4 => {
                if second_byte {
                    (self.x >> 8) as u8
                } else {
                    self.x as u8
                }
            } // X
            3 => self.dp, // DP
            2 => self.b,  // B
            1 => self.a,  // A
            0 => self.cc, // CC
            _ => 0,
        };

        bus.write(master, write_addr, val_to_push);

        // Update SP
        if use_u {
            self.u = write_addr;
        } else {
            self.s = write_addr;
        }

        // Update state for next cycle
        let is_16bit = current_bit >= 4; // 4,5,6,7 are 16-bit

        if is_16bit {
            if !second_byte {
                // Just pushed Low byte, next is High byte
                // Keep mask bit set, set second_byte flag
                // We don't strictly need to store current_bit in temp_addr because we re-scan mask
                self.temp_addr |= 0x0100;
            } else {
                // Finished 16-bit reg
                mask &= !(1 << current_bit);
                self.temp_addr = mask as u16; // Clear state
            }
        } else {
            // Finished 8-bit reg
            mask &= !(1 << current_bit);
            self.temp_addr = mask as u16; // Clear state
        }

        // The last push ends the instruction — there is no separate done-check
        // cycle, because the stack-top read at cycle 3 already spent it.
        self.state = if self.temp_addr == 0 {
            ExecState::Fetch
        } else {
            ExecState::Execute(opcode, cycle + 1)
        };
    }

    /// Generic PULL operation (PULS/PULU)
    /// Pulls registers from Low ID (CC=0) to High ID (PC=7).
    /// Stack grows upward (incrementing).
    /// 16-bit regs: Pull High byte, then Low byte.
    /// Timing: 5+n cycles — 1 fetch + 1 read postbyte + 2 /VMA don't-care +
    /// n pull bytes + 1 trailing read at the settled stack pointer. That last
    /// read is real, and is why a pull ends on a bus cycle rather than silence.
    fn op_pull<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        opcode: u8,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
        use_u: bool,
    ) {
        if cycle == 0 {
            let mask = bus.read(master, self.pc);
            self.pc = self.pc.wrapping_add(1);
            self.temp_addr = mask as u16;
            self.state = ExecState::Execute(opcode, 1);
            return;
        }

        // Cycles 1-2: /VMA don't-care cycles
        if cycle <= 2 {
            self.dummy_vma(bus, master);
            self.state = ExecState::Execute(opcode, cycle + 1);
            return;
        }

        let mut mask = (self.temp_addr & 0xFF) as u8;
        let state = (self.temp_addr >> 8) as u8;
        let second_byte = (state & 0x01) != 0; // For pull, 2nd byte is Low
        let mut current_bit = 0;

        if mask == 0 && !second_byte {
            // Trailing read at the settled stack pointer
            let sp = if use_u { self.u } else { self.s };
            let _ = bus.read(master, sp);
            self.state = ExecState::Fetch;
            return;
        }

        // PUL order: CC(0), A(1), B(2), DP(3), X(4), Y(5), U/S(6), PC(7)
        // Find lowest set bit
        for i in 0..=7 {
            if (mask & (1 << i)) != 0 {
                current_bit = i;
                break;
            }
        }

        let sp = if use_u { self.u } else { self.s };
        let val = bus.read(master, sp);

        // Update SP
        let new_sp = sp.wrapping_add(1);
        if use_u {
            self.u = new_sp;
        } else {
            self.s = new_sp;
        }

        // Store value
        let is_16bit = current_bit >= 4;

        if is_16bit {
            // 16-bit: Pull High then Low
            // If !second_byte (first step): val is High byte
            // If second_byte (second step): val is Low byte

            // Helper to update 16-bit reg
            let update_reg = |cpu: &mut M6809, val: u8, is_high: bool| {
                let target_reg = match current_bit {
                    7 => &mut cpu.pc,
                    6 => {
                        if use_u {
                            &mut cpu.s
                        } else {
                            &mut cpu.u
                        }
                    }
                    5 => &mut cpu.y,
                    4 => &mut cpu.x,
                    _ => unreachable!(),
                };
                if is_high {
                    *target_reg = (*target_reg & 0x00FF) | ((val as u16) << 8);
                } else {
                    *target_reg = (*target_reg & 0xFF00) | (val as u16);
                }
            };

            update_reg(self, val, !second_byte);

            if !second_byte {
                // Done High byte, next is Low
                self.temp_addr |= 0x0100;
            } else {
                // Done Low byte
                mask &= !(1 << current_bit);
                self.temp_addr = mask as u16;
            }
        } else {
            // 8-bit reg
            match current_bit {
                3 => self.dp = val,
                2 => self.b = val,
                1 => self.a = val,
                0 => self.cc = val,
                _ => unreachable!(),
            }
            mask &= !(1 << current_bit);
            self.temp_addr = mask as u16;
        }

        self.state = ExecState::Execute(opcode, cycle + 1);
    }

    pub(crate) fn op_pshs<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.op_push(0x34, cycle, bus, master, false);
    }

    pub(crate) fn op_puls<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.op_pull(0x35, cycle, bus, master, false);
    }

    pub(crate) fn op_pshu<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.op_push(0x36, cycle, bus, master, true);
    }

    pub(crate) fn op_pulu<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        self.op_pull(0x37, cycle, bus, master, true);
    }

    // --- SWI / RTI ---

    /// Returns the register value to push for a given SWI push cycle (0-11).
    /// Push order matches PSHS hardware: PC, U, Y, X, DP, B, A, CC.
    /// For 16-bit regs, low byte is pushed first, high byte second
    /// (so high byte ends up at lower address = big-endian in memory).
    fn swi_push_byte(&self, push_cycle: u8) -> u8 {
        match push_cycle {
            0 => self.pc as u8,
            1 => (self.pc >> 8) as u8,
            2 => self.u as u8,
            3 => (self.u >> 8) as u8,
            4 => self.y as u8,
            5 => (self.y >> 8) as u8,
            6 => self.x as u8,
            7 => (self.x >> 8) as u8,
            8 => self.dp,
            9 => self.b,
            10 => self.a,
            11 => self.cc,
            _ => 0,
        }
    }

    /// SWI (0x3F): Software Interrupt.
    /// Sets E flag, pushes all registers onto S stack, sets I+F flags,
    /// then vectors through 0xFFFA/0xFFFB.
    /// 19 cycles total: 1 fetch + 2 internal + 12 push + 1 internal + 2 vector + 1 internal.
    pub(crate) fn op_swi<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                // Don't-care cycle at PC; the E flag is set on it
                self.dummy_at_pc(bus, master, 0);
                self.cc |= CcFlag::E as u8;
                self.state = ExecState::Execute(0x3F, 1);
            }
            1 => {
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(0x3F, 2);
            }
            c @ 2..=13 => {
                // Push 12 register bytes
                self.s = self.s.wrapping_sub(1);
                bus.write(master, self.s, self.swi_push_byte(c - 2));
                self.state = ExecState::Execute(0x3F, c + 1);
            }
            14 => {
                // /VMA don't-care opening the vector fetch; masks set on it
                self.dummy_vma(bus, master);
                self.cc |= CcFlag::I as u8 | CcFlag::F as u8;
                self.state = ExecState::Execute(0x3F, 15);
            }
            15 => {
                // Read vector high byte
                let hi = bus.read(master, 0xFFFA);
                self.temp_addr = (hi as u16) << 8;
                self.state = ExecState::Execute(0x3F, 16);
            }
            16 => {
                // Read vector low byte
                let lo = bus.read(master, 0xFFFB);
                self.pc = self.temp_addr | (lo as u16);
                self.state = ExecState::Execute(0x3F, 17);
            }
            17 => {
                // /VMA don't-care closing the vector fetch
                self.dummy_vma(bus, master);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// SWI2 (0x103F): Software Interrupt 2.
    /// Sets E flag, pushes all registers onto S stack. Does NOT mask interrupts.
    /// Vectors through 0xFFF4/0xFFF5.
    /// 20 cycles total: 2 prefix + 2 internal + 12 push + 1 internal + 2 vector + 1 internal.
    pub(crate) fn op_swi2<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                // Don't-care cycle at PC; the E flag is set on it
                self.dummy_at_pc(bus, master, 0);
                self.cc |= CcFlag::E as u8;
                self.state = ExecState::ExecutePage2(0x3F, 1);
            }
            1 => {
                self.dummy_vma(bus, master);
                self.state = ExecState::ExecutePage2(0x3F, 2);
            }
            c @ 2..=13 => {
                // Push 12 register bytes
                self.s = self.s.wrapping_sub(1);
                bus.write(master, self.s, self.swi_push_byte(c - 2));
                self.state = ExecState::ExecutePage2(0x3F, c + 1);
            }
            14 => {
                // /VMA don't-care opening the vector fetch; SWI2 does NOT mask
                self.dummy_vma(bus, master);
                self.state = ExecState::ExecutePage2(0x3F, 15);
            }
            15 => {
                let hi = bus.read(master, 0xFFF4);
                self.temp_addr = (hi as u16) << 8;
                self.state = ExecState::ExecutePage2(0x3F, 16);
            }
            16 => {
                let lo = bus.read(master, 0xFFF5);
                self.pc = self.temp_addr | (lo as u16);
                self.state = ExecState::ExecutePage2(0x3F, 17);
            }
            17 => {
                // /VMA don't-care closing the vector fetch
                self.dummy_vma(bus, master);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// SWI3 (0x113F): Software Interrupt 3.
    /// Sets E flag, pushes all registers onto S stack. Does NOT mask interrupts.
    /// Vectors through 0xFFF2/0xFFF3.
    /// 20 cycles total: 2 prefix + 2 internal + 12 push + 1 internal + 2 vector + 1 internal.
    pub(crate) fn op_swi3<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                // Don't-care cycle at PC; the E flag is set on it
                self.dummy_at_pc(bus, master, 0);
                self.cc |= CcFlag::E as u8;
                self.state = ExecState::ExecutePage3(0x3F, 1);
            }
            1 => {
                self.dummy_vma(bus, master);
                self.state = ExecState::ExecutePage3(0x3F, 2);
            }
            c @ 2..=13 => {
                // Push 12 register bytes
                self.s = self.s.wrapping_sub(1);
                bus.write(master, self.s, self.swi_push_byte(c - 2));
                self.state = ExecState::ExecutePage3(0x3F, c + 1);
            }
            14 => {
                // /VMA don't-care opening the vector fetch; SWI3 does NOT mask
                self.dummy_vma(bus, master);
                self.state = ExecState::ExecutePage3(0x3F, 15);
            }
            15 => {
                let hi = bus.read(master, 0xFFF2);
                self.temp_addr = (hi as u16) << 8;
                self.state = ExecState::ExecutePage3(0x3F, 16);
            }
            16 => {
                let lo = bus.read(master, 0xFFF3);
                self.pc = self.temp_addr | (lo as u16);
                self.state = ExecState::ExecutePage3(0x3F, 17);
            }
            17 => {
                // /VMA don't-care closing the vector fetch
                self.dummy_vma(bus, master);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    // --- Hardware Interrupt Response ---

    /// Execute hardware interrupt push+vector sequence.
    /// Called from execute_cycle when state is Interrupt(cycle).
    /// Dispatches by self.interrupt_type: Nmi/Irq → interrupt_full, Firq → interrupt_firq.
    pub(crate) fn execute_interrupt<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            // Vector-only phase (used by CWAI completion, cycles 20-23).
            //
            // CWAI has already pushed every register, so waking from it needs
            // nothing but the vector fetch — and the hardware brackets that
            // fetch with a /VMA cycle on each side, exactly as the full
            // NMI/IRQ/FIRQ entry sequences below do at their cycles 14/17 and
            // 5/8. This path used to read the two vector bytes and nothing
            // else, so resuming from CWAI was two cycles short.
            20 => {
                self.dummy_vma(bus, master);
                self.state = ExecState::Interrupt(21);
            }
            21 => {
                // temp_addr has vector base address, read high byte
                let hi = bus.read(master, self.temp_addr);
                self.temp_addr = self.temp_addr.wrapping_add(1);
                self.scratch = hi; // scratch storage for vector high
                self.state = ExecState::Interrupt(22);
            }
            22 => {
                let lo = bus.read(master, self.temp_addr);
                self.pc = ((self.scratch as u16) << 8) | (lo as u16);
                self.state = ExecState::Interrupt(23);
            }
            23 => {
                self.dummy_vma(bus, master);
                self.state = ExecState::Fetch;
            }
            // Full interrupt response (dispatched by type)
            _ => match self.interrupt_type {
                InterruptType::Nmi | InterruptType::Irq => {
                    self.interrupt_full(cycle, bus, master);
                }
                InterruptType::Firq => self.interrupt_firq(cycle, bus, master),
                InterruptType::None => {
                    self.state = ExecState::Fetch;
                }
            },
        }
    }

    /// NMI/IRQ full interrupt response: set E, push all 12 registers, mask, vector.
    /// Matches SWI cycle structure exactly (18 execute cycles after detect):
    /// Cycle 0: internal (set E flag).
    /// Cycle 1: internal.
    /// Cycles 2-13: push 12 register bytes.
    /// Cycle 14: internal (set mask flags).
    /// Cycle 15: read vector high.
    /// Cycle 16: read vector low.
    /// Cycle 17: internal (done).
    /// Total: 1 detect + 18 execute = 19 cycles (same as SWI).
    fn interrupt_full<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                // Second of the two don't-care cycles at PC (the first was the
                // cycle that detected the interrupt); the E flag is set on it
                self.dummy_at_pc(bus, master, 0);
                self.cc |= CcFlag::E as u8;
                self.state = ExecState::Interrupt(1);
            }
            1 => {
                self.dummy_vma(bus, master);
                self.state = ExecState::Interrupt(2);
            }
            c @ 2..=13 => {
                // Push 12 register bytes (matches SWI cycles 2-13)
                self.s = self.s.wrapping_sub(1);
                bus.write(master, self.s, self.swi_push_byte(c - 2));
                self.state = ExecState::Interrupt(c + 1);
            }
            14 => {
                // /VMA don't-care opening the vector fetch; masks set on it
                self.dummy_vma(bus, master);
                let mask = match self.interrupt_type {
                    InterruptType::Nmi => CcFlag::I as u8 | CcFlag::F as u8,
                    _ => CcFlag::I as u8, // IRQ (FIRQ doesn't reach here)
                };
                self.cc |= mask;
                self.state = ExecState::Interrupt(15);
            }
            15 => {
                // Read vector high byte (matches SWI cycle 15)
                let vector = match self.interrupt_type {
                    InterruptType::Nmi => 0xFFFC_u16,
                    _ => 0xFFF8_u16, // IRQ
                };
                let hi = bus.read(master, vector);
                self.temp_addr = (hi as u16) << 8;
                self.state = ExecState::Interrupt(16);
            }
            16 => {
                // Read vector low byte (matches SWI cycle 16)
                let vector_lo = match self.interrupt_type {
                    InterruptType::Nmi => 0xFFFD_u16,
                    _ => 0xFFF9_u16, // IRQ
                };
                let lo = bus.read(master, vector_lo);
                self.pc = self.temp_addr | (lo as u16);
                self.state = ExecState::Interrupt(17);
            }
            17 => {
                // /VMA don't-care closing the vector fetch
                self.dummy_vma(bus, master);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// FIRQ fast interrupt response: clear E, push CC+PC only (3 bytes), mask I+F, vector.
    /// 10 cycles total (1 detect + 9 execute):
    /// Cycle 0: internal (clear E flag).
    /// Cycle 1: internal.
    /// Cycle 2: push PC low.
    /// Cycle 3: push PC high.
    /// Cycle 4: push CC.
    /// Cycle 5: internal (set I+F).
    /// Cycle 6: read vector high.
    /// Cycle 7: read vector low.
    /// Cycle 8: internal (done).
    fn interrupt_firq<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                // Second of the two don't-care cycles at PC (the first was the
                // cycle that detected the interrupt); E cleared on it
                self.dummy_at_pc(bus, master, 0);
                self.cc &= !(CcFlag::E as u8);
                self.state = ExecState::Interrupt(1);
            }
            1 => {
                self.dummy_vma(bus, master);
                self.state = ExecState::Interrupt(2);
            }
            2 => {
                self.s = self.s.wrapping_sub(1);
                bus.write(master, self.s, self.pc as u8); // PC low
                self.state = ExecState::Interrupt(3);
            }
            3 => {
                self.s = self.s.wrapping_sub(1);
                bus.write(master, self.s, (self.pc >> 8) as u8); // PC high
                self.state = ExecState::Interrupt(4);
            }
            4 => {
                self.s = self.s.wrapping_sub(1);
                bus.write(master, self.s, self.cc); // CC
                self.state = ExecState::Interrupt(5);
            }
            5 => {
                // /VMA don't-care opening the vector fetch; masks set on it
                self.dummy_vma(bus, master);
                self.cc |= CcFlag::I as u8 | CcFlag::F as u8;
                self.state = ExecState::Interrupt(6);
            }
            6 => {
                let hi = bus.read(master, 0xFFF6);
                self.temp_addr = (hi as u16) << 8;
                self.state = ExecState::Interrupt(7);
            }
            7 => {
                let lo = bus.read(master, 0xFFF7);
                self.pc = self.temp_addr | (lo as u16);
                self.state = ExecState::Interrupt(8);
            }
            8 => {
                // /VMA don't-care closing the vector fetch
                self.dummy_vma(bus, master);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    /// Handle CWAI wait state: check for interrupts each cycle.
    /// Since all registers are already pushed, just apply mask and vector.
    pub(crate) fn wait_for_interrupt<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        let ints = bus.check_interrupts(master);

        // NMI edge detection
        let nmi_edge = crate::cpu::flags::detect_rising_edge(ints.nmi, &mut self.nmi_previous);

        if nmi_edge {
            self.cc |= CcFlag::I as u8 | CcFlag::F as u8;
            self.temp_addr = 0xFFFC;
            self.state = ExecState::Interrupt(20); // vector-only phase
            return;
        }

        if ints.firq && (self.cc & CcFlag::F as u8) == 0 {
            self.cc |= CcFlag::I as u8 | CcFlag::F as u8;
            self.temp_addr = 0xFFF6;
            self.state = ExecState::Interrupt(20);
            return;
        }

        if ints.irq && (self.cc & CcFlag::I as u8) == 0 {
            self.cc |= CcFlag::I as u8;
            self.temp_addr = 0xFFF8;
            self.state = ExecState::Interrupt(20);
        }
        // Otherwise: stay in WaitForInterrupt
    }

    /// Handle SYNC wait state: check for any interrupt signal each cycle.
    /// If interrupt is not masked: take the interrupt normally (full push response).
    /// If interrupt is masked: just wake up and continue to next instruction.
    pub(crate) fn sync_wait<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        bus: &mut B,
        master: BusMaster,
    ) {
        let ints = bus.check_interrupts(master);

        // NMI edge detection
        let nmi_edge = crate::cpu::flags::detect_rising_edge(ints.nmi, &mut self.nmi_previous);

        // Any interrupt signal wakes up from SYNC
        let any_signal = nmi_edge || ints.firq || ints.irq;
        if !any_signal {
            return; // Stay in SyncWait
        }

        // Determine if the interrupt can be taken (not masked)
        if nmi_edge {
            self.interrupt_type = InterruptType::Nmi;
            self.state = ExecState::Interrupt(0);
        } else if ints.firq && (self.cc & CcFlag::F as u8) == 0 {
            self.interrupt_type = InterruptType::Firq;
            self.state = ExecState::Interrupt(0);
        } else if ints.irq && (self.cc & CcFlag::I as u8) == 0 {
            self.interrupt_type = InterruptType::Irq;
            self.state = ExecState::Interrupt(0);
        } else {
            // Interrupt is masked: just wake up, continue to next instruction
            self.state = ExecState::Fetch;
        }
    }

    // --- CWAI / SYNC opcodes ---

    /// CWAI (0x3C): Clear and Wait for Interrupt.
    /// Cycle 0: Read immediate operand, AND with CC, set E flag.
    /// Cycle 1: don't-care re-driving PC.
    /// Cycle 2: /VMA don't-care at $FFFF.
    /// Cycles 3-14: Push all registers (same as SWI push sequence).
    /// Then enter WaitForInterrupt state.
    /// When interrupt arrives: skip push (already done), just mask + vector.
    ///
    /// The two don't-care cycles between the operand and the pushes are the
    /// hardware's, and this used to go straight from one to the other. They are
    /// cycles the instruction spends rather than addresses it drives wrongly, so
    /// nothing was silently wrong — CWAI was simply two cycles short. Neither
    /// this nor SYNC can be cross-validated, because a single-step vector cannot
    /// express "and then an interrupt arrives", so nothing but a hand-written
    /// timing test covers it.
    pub(crate) fn op_cwai<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                // Read immediate operand, AND with CC, set E flag
                let operand = bus.read(master, self.pc);
                self.pc = self.pc.wrapping_add(1);
                self.cc &= operand;
                self.cc |= CcFlag::E as u8;
                self.state = ExecState::Execute(0x3C, 1);
            }
            1 => {
                // Don't-care re-driving PC, which by now points past the operand.
                self.dummy_at_pc(bus, master, 0);
                self.state = ExecState::Execute(0x3C, 2);
            }
            2 => {
                self.dummy_vma(bus, master);
                self.state = ExecState::Execute(0x3C, 3);
            }
            c @ 3..=14 => {
                // Push all registers (same order as SWI)
                self.s = self.s.wrapping_sub(1);
                bus.write(master, self.s, self.swi_push_byte(c - 3));
                if c == 14 {
                    // All registers pushed, enter wait state
                    self.state = ExecState::WaitForInterrupt;
                } else {
                    self.state = ExecState::Execute(0x3C, c + 1);
                }
            }
            _ => {}
        }
    }

    /// SYNC (0x13): Synchronize to interrupt.
    /// Halts CPU until any interrupt signal is detected.
    /// If interrupt is not masked: take the interrupt normally.
    /// If interrupt is masked: just resume at next instruction.
    pub(crate) fn op_sync<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        if cycle == 0 {
            self.dummy_at_pc(bus, master, 0);
            self.state = ExecState::SyncWait;
        }
    }

    /// RTI (0x3B): Return from Interrupt.
    /// Pulls CC from S stack. If E flag is set in pulled CC, pulls all registers
    /// (A, B, DP, X, Y, U, PC). If E is clear, pulls only PC (fast FIRQ return).
    /// CC is restored from the stack (all flags affected).
    /// Both paths open with a don't-care cycle at PC and, like every pull
    /// sequence, close with a real read at the settled stack pointer.
    /// E=0: 6 cycles (1 fetch + 1 don't-care + 1 pull CC + 2 pull PC + 1 read).
    /// E=1: 15 cycles (1 fetch + 1 don't-care + 12 pulls + 1 read).
    pub(crate) fn op_rti<B: Bus<Address = u16, Data = u8> + ?Sized>(
        &mut self,
        cycle: u8,
        bus: &mut B,
        master: BusMaster,
    ) {
        match cycle {
            0 => {
                self.dummy_at_pc(bus, master, 0);
                self.state = ExecState::Execute(0x3B, 1);
            }
            1 => {
                // Pull CC
                self.cc = bus.read(master, self.s);
                self.s = self.s.wrapping_add(1);
                if self.cc & (CcFlag::E as u8) != 0 {
                    // E set: pull all registers
                    self.state = ExecState::Execute(0x3B, 2);
                } else {
                    // E clear: pull PC only (fast FIRQ return)
                    self.state = ExecState::Execute(0x3B, 20);
                }
            }
            // === E=1 path: pull A, B, DP, X, Y, U, PC ===
            2 => {
                self.a = bus.read(master, self.s);
                self.s = self.s.wrapping_add(1);
                self.state = ExecState::Execute(0x3B, 3);
            }
            3 => {
                self.b = bus.read(master, self.s);
                self.s = self.s.wrapping_add(1);
                self.state = ExecState::Execute(0x3B, 4);
            }
            4 => {
                self.dp = bus.read(master, self.s);
                self.s = self.s.wrapping_add(1);
                self.state = ExecState::Execute(0x3B, 5);
            }
            5 => {
                self.x = (bus.read(master, self.s) as u16) << 8;
                self.s = self.s.wrapping_add(1);
                self.state = ExecState::Execute(0x3B, 6);
            }
            6 => {
                self.x |= bus.read(master, self.s) as u16;
                self.s = self.s.wrapping_add(1);
                self.state = ExecState::Execute(0x3B, 7);
            }
            7 => {
                self.y = (bus.read(master, self.s) as u16) << 8;
                self.s = self.s.wrapping_add(1);
                self.state = ExecState::Execute(0x3B, 8);
            }
            8 => {
                self.y |= bus.read(master, self.s) as u16;
                self.s = self.s.wrapping_add(1);
                self.state = ExecState::Execute(0x3B, 9);
            }
            9 => {
                self.u = (bus.read(master, self.s) as u16) << 8;
                self.s = self.s.wrapping_add(1);
                self.state = ExecState::Execute(0x3B, 10);
            }
            10 => {
                self.u |= bus.read(master, self.s) as u16;
                self.s = self.s.wrapping_add(1);
                self.state = ExecState::Execute(0x3B, 11);
            }
            11 => {
                self.pc = (bus.read(master, self.s) as u16) << 8;
                self.s = self.s.wrapping_add(1);
                self.state = ExecState::Execute(0x3B, 12);
            }
            12 => {
                self.pc |= bus.read(master, self.s) as u16;
                self.s = self.s.wrapping_add(1);
                self.state = ExecState::Execute(0x3B, 13);
            }
            13 => {
                // Trailing read at the settled stack pointer (E=1 done)
                let _ = bus.read(master, self.s);
                self.state = ExecState::Fetch;
            }
            // === E=0 path: pull PC only ===
            20 => {
                self.pc = (bus.read(master, self.s) as u16) << 8;
                self.s = self.s.wrapping_add(1);
                self.state = ExecState::Execute(0x3B, 21);
            }
            21 => {
                self.pc |= bus.read(master, self.s) as u16;
                self.s = self.s.wrapping_add(1);
                self.state = ExecState::Execute(0x3B, 22);
            }
            22 => {
                // Trailing read at the settled stack pointer (E=0 done)
                let _ = bus.read(master, self.s);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }
}
