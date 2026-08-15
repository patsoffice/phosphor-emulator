//! Declarative generators for the M68xx (M6800/M6809) ALU opcode wrappers.
//!
//! Nearly every ALU opcode on these CPUs is a wrapper whose only variation is
//! `(function name, addressing helper, perform_* operation, accumulator)`. The
//! wrappers exist as named functions because the dispatch `match` in each CPU's
//! `mod.rs` refers to them by name; the macros below emit exactly those names,
//! so dispatch is unaffected.
//!
//! None of the generated code contains cycle logic — timing lives entirely in
//! the `alu_*` / `rmw_*` addressing helpers these wrappers forward to.
//!
//! Every row carries its own doc comment (matched as `#[$meta]`), which the
//! generated function keeps: `core/CLAUDE.md` requires each instruction to
//! document its flag behaviour.
//!
//! The expansions name `Bus`, `BusMaster`, `Acc`, `ExecState` and the
//! `M68xxAlu` trait unqualified, so each call site must have them in scope.
//!
//! The M6809 addressing helpers take the opcode byte as their first argument
//! (they need it to rebuild `ExecState::Execute`), the M6800 ones do not. The
//! `@opcode` marker at the head of an invocation selects the former.

/// Accumulator ALU wrappers: fetch an operand via `$helper`, then apply
/// `$perform` to the named accumulator.
///
/// ```ignore
/// impl M6800 {
///     m68xx_alu_acc! {
///         /// SUBA direct (0x90). N, Z, V, C affected.
///         op_suba_dir => alu_direct, perform_sub, A;
///     }
/// }
/// ```
macro_rules! m68xx_alu_acc {
    // M6809 form: the addressing helper also takes the opcode byte.
    (@opcode $(
        $(#[$meta:meta])*
        $name:ident => $helper:ident, $perform:ident, $acc:ident;
    )*) => {
        $(
            $(#[$meta])*
            pub(crate) fn $name<B: Bus<Address = u16, Data = u8> + ?Sized>(
                &mut self,
                opcode: u8,
                cycle: u8,
                bus: &mut B,
                master: BusMaster,
            ) {
                self.$helper(opcode, cycle, bus, master, |cpu, operand| {
                    cpu.$perform(Acc::$acc, operand);
                });
            }
        )*
    };

    // M6800 form (and M6809 immediate mode): no opcode byte.
    ($(
        $(#[$meta:meta])*
        $name:ident => $helper:ident, $perform:ident, $acc:ident;
    )*) => {
        $(
            $(#[$meta])*
            pub(crate) fn $name<B: Bus<Address = u16, Data = u8> + ?Sized>(
                &mut self,
                cycle: u8,
                bus: &mut B,
                master: BusMaster,
            ) {
                self.$helper(cycle, bus, master, |cpu, operand| {
                    cpu.$perform(Acc::$acc, operand);
                });
            }
        )*
    };
}

/// Memory read-modify-write wrappers: `$helper` handles addressing and the
/// bus cycles, the closure column supplies the operation.
///
/// The closure is spelled out per row because the three shapes differ: most
/// ops transform the byte (`|cpu, val| cpu.perform_neg(val)`), TST only sets
/// flags and writes the byte back unchanged, and CLR ignores the byte it read.
///
/// ```ignore
/// impl M6800 {
///     m68xx_alu_rmw! {
///         /// NEG indexed (0x60).
///         op_neg_idx => rmw_indexed, |cpu, val| cpu.perform_neg(val);
///     }
/// }
/// ```
macro_rules! m68xx_alu_rmw {
    // M6809 form: the addressing helper also takes the opcode byte.
    (@opcode $(
        $(#[$meta:meta])*
        $name:ident => $helper:ident, $operation:expr;
    )*) => {
        $(
            $(#[$meta])*
            pub(crate) fn $name<B: Bus<Address = u16, Data = u8> + ?Sized>(
                &mut self,
                opcode: u8,
                cycle: u8,
                bus: &mut B,
                master: BusMaster,
            ) {
                self.$helper(opcode, cycle, bus, master, $operation);
            }
        )*
    };

    // M6800 form: no opcode byte.
    ($(
        $(#[$meta:meta])*
        $name:ident => $helper:ident, $operation:expr;
    )*) => {
        $(
            $(#[$meta])*
            pub(crate) fn $name<B: Bus<Address = u16, Data = u8> + ?Sized>(
                &mut self,
                cycle: u8,
                bus: &mut B,
                master: BusMaster,
            ) {
                self.$helper(cycle, bus, master, $operation);
            }
        )*
    };
}

/// Inherent (register-operand) ALU wrappers. The operand is a register, so the
/// single execute cycle has no memory access to make.
///
/// Three shapes, selected by a marker at the head of the invocation:
/// * default — `reg = perform(reg)` (NEG, COM, INC, DEC, shifts, rotates)
/// * `@no_operand` — `reg = perform()` (CLR, whose `perform_clr` takes nothing)
/// * `@flags_only` — `perform(reg)` with the register left alone (TST)
///
/// An `@m6809` marker ahead of any of the three selects the M6809 form, which
/// takes the bus: that CPU drives its don't-care cycles rather than leaving
/// them silent, so the execute cycle still performs a read. TST drives $FFFF
/// there and everything else re-drives PC — see `M6809::dummy_vma`.
///
/// ```ignore
/// impl M6800 {
///     m68xx_alu_inherent! {
///         /// NEGA inherent (0x40): Negate A.
///         op_nega => a, perform_neg;
///     }
/// }
/// ```
macro_rules! m68xx_alu_inherent {
    // M6809 forms: the execute cycle drives a don't-care bus access.
    (@m6809 @no_operand $(
        $(#[$meta:meta])*
        $name:ident => $reg:ident, $perform:ident;
    )*) => {
        $(
            $(#[$meta])*
            pub(crate) fn $name<B: Bus<Address = u16, Data = u8> + ?Sized>(
                &mut self,
                cycle: u8,
                bus: &mut B,
                master: BusMaster,
            ) {
                if cycle == 0 {
                    self.dummy_at_pc(bus, master, 0);
                    self.$reg = self.$perform();
                    self.state = ExecState::Fetch;
                }
            }
        )*
    };

    (@m6809 @flags_only $(
        $(#[$meta:meta])*
        $name:ident => $reg:ident, $perform:ident;
    )*) => {
        $(
            $(#[$meta])*
            pub(crate) fn $name<B: Bus<Address = u16, Data = u8> + ?Sized>(
                &mut self,
                cycle: u8,
                bus: &mut B,
                master: BusMaster,
            ) {
                if cycle == 0 {
                    self.dummy_vma(bus, master);
                    self.$perform(self.$reg);
                    self.state = ExecState::Fetch;
                }
            }
        )*
    };

    (@m6809 $(
        $(#[$meta:meta])*
        $name:ident => $reg:ident, $perform:ident;
    )*) => {
        $(
            $(#[$meta])*
            pub(crate) fn $name<B: Bus<Address = u16, Data = u8> + ?Sized>(
                &mut self,
                cycle: u8,
                bus: &mut B,
                master: BusMaster,
            ) {
                if cycle == 0 {
                    self.dummy_at_pc(bus, master, 0);
                    self.$reg = self.$perform(self.$reg);
                    self.state = ExecState::Fetch;
                }
            }
        )*
    };

    (@no_operand $(
        $(#[$meta:meta])*
        $name:ident => $reg:ident, $perform:ident;
    )*) => {
        $(
            $(#[$meta])*
            pub(crate) fn $name(&mut self, cycle: u8) {
                if cycle == 0 {
                    self.$reg = self.$perform();
                    self.state = ExecState::Fetch;
                }
            }
        )*
    };

    (@flags_only $(
        $(#[$meta:meta])*
        $name:ident => $reg:ident, $perform:ident;
    )*) => {
        $(
            $(#[$meta])*
            pub(crate) fn $name(&mut self, cycle: u8) {
                if cycle == 0 {
                    self.$perform(self.$reg);
                    self.state = ExecState::Fetch;
                }
            }
        )*
    };

    ($(
        $(#[$meta:meta])*
        $name:ident => $reg:ident, $perform:ident;
    )*) => {
        $(
            $(#[$meta])*
            pub(crate) fn $name(&mut self, cycle: u8) {
                if cycle == 0 {
                    self.$reg = self.$perform(self.$reg);
                    self.state = ExecState::Fetch;
                }
            }
        )*
    };
}

pub(crate) use {m68xx_alu_acc, m68xx_alu_inherent, m68xx_alu_rmw};
