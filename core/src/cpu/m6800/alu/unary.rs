use crate::core::{Bus, BusMaster};
use crate::cpu::m68xx::M68xxAlu;
use crate::cpu::m68xx_alu_macros::{m68xx_alu_inherent, m68xx_alu_rmw};
use crate::cpu::m6800::{ExecState, M6800};

impl M6800 {
    // --- Inherent register ops (2 cycles: 1 fetch + 1 internal) ---

    m68xx_alu_inherent! {
        /// NEGA inherent (0x40): Negate A (A = 0 - A, two's complement).
        op_nega => a, perform_neg;

        /// NEGB inherent (0x50): Negate B (B = 0 - B, two's complement).
        op_negb => b, perform_neg;

        /// COMA inherent (0x43): Complement A (A = ~A).
        op_coma => a, perform_com;

        /// COMB inherent (0x53): Complement B (B = ~B).
        op_comb => b, perform_com;
    }

    m68xx_alu_inherent! { @no_operand
        /// CLRA inherent (0x4F): Clear A (A = 0).
        op_clra => a, perform_clr;

        /// CLRB inherent (0x5F): Clear B (B = 0).
        op_clrb => b, perform_clr;
    }

    m68xx_alu_inherent! {
        /// INCA inherent (0x4C): Increment A (A = A + 1).
        op_inca => a, perform_inc;

        /// INCB inherent (0x5C): Increment B (B = B + 1).
        op_incb => b, perform_inc;

        /// DECA inherent (0x4A): Decrement A (A = A - 1).
        op_deca => a, perform_dec;

        /// DECB inherent (0x5A): Decrement B (B = B - 1).
        op_decb => b, perform_dec;
    }

    m68xx_alu_inherent! { @flags_only
        /// TSTA inherent (0x4D): Test A (set flags based on A, no modification).
        op_tsta => a, perform_tst;

        /// TSTB inherent (0x5D): Test B (set flags based on B, no modification).
        op_tstb => b, perform_tst;
    }

    // --- Memory unary ops: indexed (7 cycles) and extended (6 cycles) ---

    m68xx_alu_rmw! {
        /// NEG indexed (0x60).
        op_neg_idx => rmw_indexed, |cpu, val| cpu.perform_neg(val);

        /// NEG extended (0x70).
        op_neg_ext => rmw_extended, |cpu, val| cpu.perform_neg(val);

        /// COM indexed (0x63).
        op_com_idx => rmw_indexed, |cpu, val| cpu.perform_com(val);

        /// COM extended (0x73).
        op_com_ext => rmw_extended, |cpu, val| cpu.perform_com(val);

        /// INC indexed (0x6C).
        op_inc_idx => rmw_indexed, |cpu, val| cpu.perform_inc(val);

        /// INC extended (0x7C).
        op_inc_ext => rmw_extended, |cpu, val| cpu.perform_inc(val);

        /// DEC indexed (0x6A).
        op_dec_idx => rmw_indexed, |cpu, val| cpu.perform_dec(val);

        /// DEC extended (0x7A).
        op_dec_ext => rmw_extended, |cpu, val| cpu.perform_dec(val);

        /// TST indexed (0x6D).
        op_tst_idx => rmw_indexed, |cpu, val| { cpu.perform_tst(val); val };

        /// TST extended (0x7D).
        op_tst_ext => rmw_extended, |cpu, val| { cpu.perform_tst(val); val };

        /// CLR indexed (0x6F).
        op_clr_idx => rmw_indexed, |cpu, _val| cpu.perform_clr();

        /// CLR extended (0x7F).
        op_clr_ext => rmw_extended, |cpu, _val| cpu.perform_clr();
    }
}
