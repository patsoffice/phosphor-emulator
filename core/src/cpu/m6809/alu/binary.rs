use crate::core::{Bus, BusMaster};
use crate::cpu::m68xx::{Acc, M68xxAlu};
use crate::cpu::m68xx_alu_macros::m68xx_alu_acc;
use crate::cpu::m6809::{CcFlag, ExecState, M6809};

impl M6809 {
    m68xx_alu_acc! {
        /// SUBA immediate (0x80): Subtracts the immediate operand from accumulator A.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V set if signed overflow occurred (operands had different signs and result sign differs from A).
        /// C set if unsigned borrow occurred (operand > A). H set if borrow from bit 4.
        op_suba_imm => alu_imm, perform_sub, A;

        /// ADDA immediate (0x8B): Adds the immediate operand to accumulator A.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V set if signed overflow occurred (operands had same sign and result sign differs).
        /// C set if unsigned carry out of bit 7. H set if carry from bit 3 to bit 4.
        op_adda_imm => alu_imm, perform_add, A;

        /// CMPA immediate (0x81): Compares accumulator A with the immediate operand (A - M).
        /// Performs subtraction but discards the result; only flags are updated.
        /// N set if result bit 7 is set. Z set if A == operand.
        /// V set if signed overflow occurred. C set if unsigned borrow occurred (operand > A).
        op_cmpa_imm => alu_imm, perform_cmp, A;

        /// SBCA immediate (0x82): Subtracts the immediate operand and carry from accumulator A.
        /// A = A - M - C. Used for multi-byte subtraction chains.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V set if signed overflow occurred. C set if unsigned borrow occurred.
        op_sbca_imm => alu_imm, perform_sbc, A;

        /// ANDA immediate (0x84): Performs bitwise AND of accumulator A with the immediate operand.
        /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
        op_anda_imm => alu_imm, perform_and, A;

        /// BITA immediate (0x85): Bit test A -- performs A AND operand, updates flags but discards result.
        /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
        op_bita_imm => alu_imm, perform_bit, A;

        /// EORA immediate (0x88): Performs bitwise Exclusive OR of accumulator A with the immediate operand.
        /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
        op_eora_imm => alu_imm, perform_eor, A;

        /// ADCA immediate (0x89): Adds the immediate operand and carry to accumulator A.
        /// A = A + M + C. Used for multi-byte addition chains.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V set if signed overflow occurred. C set if unsigned carry out of bit 7.
        /// H set if carry from bit 3 to bit 4.
        op_adca_imm => alu_imm, perform_adc, A;

        /// ORA immediate (0x8A): Performs bitwise OR of accumulator A with the immediate operand.
        /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
        op_ora_imm => alu_imm, perform_or, A;

        /// SUBB immediate (0xC0): Subtracts the immediate operand from accumulator B.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V set if signed overflow occurred (operands had different signs and result sign differs from B).
        /// C set if unsigned borrow occurred (operand > B).
        op_subb_imm => alu_imm, perform_sub, B;

        /// CMPB immediate (0xC1): Compares accumulator B with the immediate operand (B - M).
        /// Performs subtraction but discards the result; only flags are updated.
        /// N set if result bit 7 is set. Z set if B == operand.
        /// V set if signed overflow occurred. C set if unsigned borrow occurred (operand > B).
        op_cmpb_imm => alu_imm, perform_cmp, B;

        /// SBCB immediate (0xC2): Subtracts the immediate operand and carry from accumulator B.
        /// B = B - M - C. Used for multi-byte subtraction chains.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V set if signed overflow occurred. C set if unsigned borrow occurred.
        op_sbcb_imm => alu_imm, perform_sbc, B;

        /// ANDB immediate (0xC4): Performs bitwise AND of accumulator B with the immediate operand.
        /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
        op_andb_imm => alu_imm, perform_and, B;

        /// BITB immediate (0xC5): Bit test B -- performs B AND operand, updates flags but discards result.
        /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
        op_bitb_imm => alu_imm, perform_bit, B;

        /// EORB immediate (0xC8): Performs bitwise Exclusive OR of accumulator B with the immediate operand.
        /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
        op_eorb_imm => alu_imm, perform_eor, B;

        /// ADCB immediate (0xC9): Adds the immediate operand and carry to accumulator B.
        /// B = B + M + C. Used for multi-byte addition chains.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V set if signed overflow occurred. C set if unsigned carry out of bit 7.
        /// H set if carry from bit 3 to bit 4.
        op_adcb_imm => alu_imm, perform_adc, B;

        /// ORB immediate (0xCA): Performs bitwise OR of accumulator B with the immediate operand.
        /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
        op_orb_imm => alu_imm, perform_or, B;

        /// ADDB immediate (0xCB): Adds the immediate operand to accumulator B.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V set if signed overflow occurred (operands had same sign and result sign differs).
        /// C set if unsigned carry out of bit 7. H set if carry from bit 3 to bit 4.
        op_addb_imm => alu_imm, perform_add, B;
    }

    /// MUL inherent (0x3D): Multiplies A and B (unsigned), result in D (A=high, B=low).
    /// Z set if 16-bit result is zero. C set if bit 7 of B (low byte) is set.
    /// 11 total cycles: 1 fetch + 10 exec (9 internal + compute).
    pub(crate) fn op_mul(&mut self, cycle: u8) {
        match cycle {
            0..=8 => {
                // Internal cycles (multiply computation)
                self.state = ExecState::Execute(0x3D, cycle + 1);
            }
            9 => {
                let result = (self.a as u16) * (self.b as u16);
                self.a = (result >> 8) as u8;
                self.b = (result & 0xFF) as u8;
                self.set_flag(CcFlag::Z, result == 0);
                self.set_flag(CcFlag::C, self.b & 0x80 != 0);
                self.state = ExecState::Fetch;
            }
            _ => {}
        }
    }

    // --- Direct addressing mode (A register) ---

    m68xx_alu_acc! { @opcode
        /// SUBA direct (0x90): Subtracts the memory operand at DP:addr from accumulator A.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V set if signed overflow occurred. C set if unsigned borrow occurred. H set if borrow from bit 4.
        op_suba_direct => alu_direct, perform_sub, A;

        /// CMPA direct (0x91): Compares accumulator A with the memory operand at DP:addr.
        /// Performs subtraction but discards the result; only flags are updated.
        /// N set if result bit 7 is set. Z set if A == operand.
        /// V set if signed overflow occurred. C set if unsigned borrow occurred.
        op_cmpa_direct => alu_direct, perform_cmp, A;

        /// SBCA direct (0x92): Subtracts the memory operand and carry from accumulator A.
        /// A = A - M - C. N set if result bit 7 is set. Z set if result is zero.
        /// V set if signed overflow occurred. C set if unsigned borrow occurred.
        op_sbca_direct => alu_direct, perform_sbc, A;

        /// ANDA direct (0x94): Performs bitwise AND of accumulator A with the memory operand at DP:addr.
        /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
        op_anda_direct => alu_direct, perform_and, A;

        /// BITA direct (0x95): Bit test A -- performs A AND operand at DP:addr, updates flags but discards result.
        /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
        op_bita_direct => alu_direct, perform_bit, A;

        /// EORA direct (0x98): Performs bitwise Exclusive OR of accumulator A with the memory operand at DP:addr.
        /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
        op_eora_direct => alu_direct, perform_eor, A;

        /// ADCA direct (0x99): Adds the memory operand and carry to accumulator A.
        /// A = A + M + C. N set if result bit 7 is set. Z set if result is zero.
        /// V set if signed overflow occurred. C set if unsigned carry out of bit 7.
        /// H set if carry from bit 3 to bit 4.
        op_adca_direct => alu_direct, perform_adc, A;

        /// ORA direct (0x9A): Performs bitwise OR of accumulator A with the memory operand at DP:addr.
        /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
        op_ora_direct => alu_direct, perform_or, A;

        /// ADDA direct (0x9B): Adds the memory operand at DP:addr to accumulator A.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V set if signed overflow occurred. C set if unsigned carry out of bit 7.
        /// H set if carry from bit 3 to bit 4.
        op_adda_direct => alu_direct, perform_add, A;
    }

    // --- Direct addressing mode (B register) ---

    m68xx_alu_acc! { @opcode
        /// SUBB direct (0xD0): Subtracts the memory operand at DP:addr from accumulator B.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V set if signed overflow occurred. C set if unsigned borrow occurred.
        op_subb_direct => alu_direct, perform_sub, B;

        /// CMPB direct (0xD1): Compares accumulator B with the memory operand at DP:addr.
        /// Performs subtraction but discards the result; only flags are updated.
        /// N set if result bit 7 is set. Z set if B == operand.
        /// V set if signed overflow occurred. C set if unsigned borrow occurred.
        op_cmpb_direct => alu_direct, perform_cmp, B;

        /// SBCB direct (0xD2): Subtracts the memory operand and carry from accumulator B.
        /// B = B - M - C. N set if result bit 7 is set. Z set if result is zero.
        /// V set if signed overflow occurred. C set if unsigned borrow occurred.
        op_sbcb_direct => alu_direct, perform_sbc, B;

        /// ANDB direct (0xD4): Performs bitwise AND of accumulator B with the memory operand at DP:addr.
        /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
        op_andb_direct => alu_direct, perform_and, B;

        /// BITB direct (0xD5): Bit test B -- performs B AND operand at DP:addr, updates flags but discards result.
        /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
        op_bitb_direct => alu_direct, perform_bit, B;

        /// EORB direct (0xD8): Performs bitwise Exclusive OR of accumulator B with the memory operand at DP:addr.
        /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
        op_eorb_direct => alu_direct, perform_eor, B;

        /// ADCB direct (0xD9): Adds the memory operand and carry to accumulator B.
        /// B = B + M + C. N set if result bit 7 is set. Z set if result is zero.
        /// V set if signed overflow occurred. C set if unsigned carry out of bit 7.
        /// H set if carry from bit 3 to bit 4.
        op_adcb_direct => alu_direct, perform_adc, B;

        /// ORB direct (0xDA): Performs bitwise OR of accumulator B with the memory operand at DP:addr.
        /// N set if result bit 7 is set. Z set if result is zero. V always cleared.
        op_orb_direct => alu_direct, perform_or, B;

        /// ADDB direct (0xDB): Adds the memory operand at DP:addr to accumulator B.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V set if signed overflow occurred. C set if unsigned carry out of bit 7.
        /// H set if carry from bit 3 to bit 4.
        op_addb_direct => alu_direct, perform_add, B;
    }

    // --- Extended addressing mode (A register) ---

    m68xx_alu_acc! { @opcode
        /// SUBA extended (0xB0)
        op_suba_extended => alu_extended, perform_sub, A;

        /// CMPA extended (0xB1)
        op_cmpa_extended => alu_extended, perform_cmp, A;

        /// SBCA extended (0xB2)
        op_sbca_extended => alu_extended, perform_sbc, A;

        /// ANDA extended (0xB4)
        op_anda_extended => alu_extended, perform_and, A;

        /// BITA extended (0xB5)
        op_bita_extended => alu_extended, perform_bit, A;

        /// EORA extended (0xB8)
        op_eora_extended => alu_extended, perform_eor, A;

        /// ADCA extended (0xB9)
        op_adca_extended => alu_extended, perform_adc, A;

        /// ORA extended (0xBA)
        op_ora_extended => alu_extended, perform_or, A;

        /// ADDA extended (0xBB): Adds the memory operand at the 16-bit address to accumulator A.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V set if signed overflow occurred. C set if unsigned carry out of bit 7.
        /// H set if carry from bit 3 to bit 4.
        op_adda_extended => alu_extended, perform_add, A;
    }

    // --- Extended addressing mode (B register) ---

    m68xx_alu_acc! { @opcode
        /// SUBB extended (0xF0)
        op_subb_extended => alu_extended, perform_sub, B;

        /// CMPB extended (0xF1)
        op_cmpb_extended => alu_extended, perform_cmp, B;

        /// SBCB extended (0xF2)
        op_sbcb_extended => alu_extended, perform_sbc, B;

        /// ANDB extended (0xF4)
        op_andb_extended => alu_extended, perform_and, B;

        /// BITB extended (0xF5)
        op_bitb_extended => alu_extended, perform_bit, B;

        /// EORB extended (0xF8)
        op_eorb_extended => alu_extended, perform_eor, B;

        /// ADCB extended (0xF9)
        op_adcb_extended => alu_extended, perform_adc, B;

        /// ORB extended (0xFA)
        op_orb_extended => alu_extended, perform_or, B;

        /// ADDB extended (0xFB)
        op_addb_extended => alu_extended, perform_add, B;
    }

    // --- Indexed addressing mode (A register) ---

    m68xx_alu_acc! { @opcode
        /// SUBA indexed (0xA0)
        op_suba_indexed => alu_indexed, perform_sub, A;

        /// CMPA indexed (0xA1)
        op_cmpa_indexed => alu_indexed, perform_cmp, A;

        /// SBCA indexed (0xA2)
        op_sbca_indexed => alu_indexed, perform_sbc, A;

        /// ANDA indexed (0xA4)
        op_anda_indexed => alu_indexed, perform_and, A;

        /// BITA indexed (0xA5)
        op_bita_indexed => alu_indexed, perform_bit, A;

        /// EORA indexed (0xA8)
        op_eora_indexed => alu_indexed, perform_eor, A;

        /// ADCA indexed (0xA9)
        op_adca_indexed => alu_indexed, perform_adc, A;

        /// ORA indexed (0xAA)
        op_ora_indexed => alu_indexed, perform_or, A;

        /// ADDA indexed (0xAB)
        op_adda_indexed => alu_indexed, perform_add, A;
    }

    // --- Indexed addressing mode (B register) ---

    m68xx_alu_acc! { @opcode
        /// SUBB indexed (0xE0)
        op_subb_indexed => alu_indexed, perform_sub, B;

        /// CMPB indexed (0xE1)
        op_cmpb_indexed => alu_indexed, perform_cmp, B;

        /// SBCB indexed (0xE2)
        op_sbcb_indexed => alu_indexed, perform_sbc, B;

        /// ANDB indexed (0xE4)
        op_andb_indexed => alu_indexed, perform_and, B;

        /// BITB indexed (0xE5)
        op_bitb_indexed => alu_indexed, perform_bit, B;

        /// EORB indexed (0xE8)
        op_eorb_indexed => alu_indexed, perform_eor, B;

        /// ADCB indexed (0xE9)
        op_adcb_indexed => alu_indexed, perform_adc, B;

        /// ORB indexed (0xEA)
        op_orb_indexed => alu_indexed, perform_or, B;

        /// ADDB indexed (0xEB)
        op_addb_indexed => alu_indexed, perform_add, B;
    }
}
