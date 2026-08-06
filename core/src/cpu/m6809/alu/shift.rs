use crate::core::{Bus, BusMaster};
use crate::cpu::m68xx::M68xxAlu;
use crate::cpu::m68xx_alu_macros::{m68xx_alu_inherent, m68xx_alu_rmw};
use crate::cpu::m6809::{ExecState, M6809};

impl M6809 {
    m68xx_alu_inherent! {
        /// ASLA/LSLA inherent (0x48): Arithmetic/Logical Shift Left A.
        /// Shifts all bits left one position. Bit 7 goes to C, 0 enters bit 0.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V = N XOR C (post-shift). C set to old bit 7.
        op_asla => a, perform_asl;

        /// ASLB/LSLB inherent (0x58): Arithmetic/Logical Shift Left B.
        /// Shifts all bits left one position. Bit 7 goes to C, 0 enters bit 0.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V = N XOR C (post-shift). C set to old bit 7.
        op_aslb => b, perform_asl;

        /// ASRA inherent (0x47): Arithmetic Shift Right A.
        /// Shifts all bits right one position. Bit 7 is preserved (sign extension).
        /// Bit 0 goes to C.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V = N XOR C (post-shift). C set to old bit 0.
        op_asra => a, perform_asr;

        /// ASRB inherent (0x57): Arithmetic Shift Right B.
        /// Shifts all bits right one position. Bit 7 is preserved (sign extension).
        /// Bit 0 goes to C.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V = N XOR C (post-shift). C set to old bit 0.
        op_asrb => b, perform_asr;

        /// LSRA inherent (0x44): Logical Shift Right A.
        /// Shifts all bits right one position. 0 enters bit 7, bit 0 goes to C.
        /// N always cleared. Z set if result is zero.
        /// V = N XOR C = C (since N=0). C set to old bit 0.
        op_lsra => a, perform_lsr;

        /// LSRB inherent (0x54): Logical Shift Right B.
        /// Shifts all bits right one position. 0 enters bit 7, bit 0 goes to C.
        /// N always cleared. Z set if result is zero.
        /// V = N XOR C = C (since N=0). C set to old bit 0.
        op_lsrb => b, perform_lsr;

        /// ROLA inherent (0x49): Rotate Left A through Carry.
        /// Old bit 7 goes to C, old C enters bit 0, other bits shift left.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V = N XOR C (post-rotate). C set to old bit 7.
        op_rola => a, perform_rol;

        /// ROLB inherent (0x59): Rotate Left B through Carry.
        /// Old bit 7 goes to C, old C enters bit 0, other bits shift left.
        /// N set if result bit 7 is set. Z set if result is zero.
        /// V = N XOR C (post-rotate). C set to old bit 7.
        op_rolb => b, perform_rol;

        /// RORA inherent (0x46): Rotate Right A through Carry.
        /// Old bit 0 goes to C, old C enters bit 7, other bits shift right.
        /// N set if result bit 7 is set (i.e., old C was set). Z set if result is zero.
        /// V = N XOR C (post-rotate). C set to old bit 0.
        op_rora => a, perform_ror;

        /// RORB inherent (0x56): Rotate Right B through Carry.
        /// Old bit 0 goes to C, old C enters bit 7, other bits shift right.
        /// N set if result bit 7 is set (i.e., old C was set). Z set if result is zero.
        /// V = N XOR C (post-rotate). C set to old bit 0.
        op_rorb => b, perform_ror;
    }

    // --- Direct addressing mode (memory shift ops, 0x04-0x09) ---

    m68xx_alu_rmw! { @opcode
        /// LSR direct (0x04): Logical Shift Right memory byte at DP:addr.
        op_lsr_direct => rmw_direct, |cpu, val| cpu.perform_lsr(val);

        /// ROR direct (0x06): Rotate Right memory byte at DP:addr through Carry.
        op_ror_direct => rmw_direct, |cpu, val| cpu.perform_ror(val);

        /// ASR direct (0x07): Arithmetic Shift Right memory byte at DP:addr.
        op_asr_direct => rmw_direct, |cpu, val| cpu.perform_asr(val);

        /// ASL direct (0x08): Arithmetic Shift Left memory byte at DP:addr.
        op_asl_direct => rmw_direct, |cpu, val| cpu.perform_asl(val);

        /// ROL direct (0x09): Rotate Left memory byte at DP:addr through Carry.
        op_rol_direct => rmw_direct, |cpu, val| cpu.perform_rol(val);
    }

    // --- Extended addressing mode (memory shift ops, 0x74-0x79) ---

    m68xx_alu_rmw! { @opcode
        /// LSR extended (0x74): Logical Shift Right memory byte at 16-bit address.
        op_lsr_extended => rmw_extended, |cpu, val| cpu.perform_lsr(val);

        /// ROR extended (0x76): Rotate Right memory byte at 16-bit address through Carry.
        op_ror_extended => rmw_extended, |cpu, val| cpu.perform_ror(val);

        /// ASR extended (0x77): Arithmetic Shift Right memory byte at 16-bit address.
        op_asr_extended => rmw_extended, |cpu, val| cpu.perform_asr(val);

        /// ASL extended (0x78): Arithmetic Shift Left memory byte at 16-bit address.
        op_asl_extended => rmw_extended, |cpu, val| cpu.perform_asl(val);

        /// ROL extended (0x79): Rotate Left memory byte at 16-bit address through Carry.
        op_rol_extended => rmw_extended, |cpu, val| cpu.perform_rol(val);
    }

    // --- Indexed addressing mode (memory shift ops, 0x64-0x69) ---

    m68xx_alu_rmw! { @opcode
        /// LSR indexed (0x64): Logical Shift Right memory byte at indexed EA.
        op_lsr_indexed => rmw_indexed, |cpu, val| cpu.perform_lsr(val);

        /// ROR indexed (0x66): Rotate Right memory byte at indexed EA through Carry.
        op_ror_indexed => rmw_indexed, |cpu, val| cpu.perform_ror(val);

        /// ASR indexed (0x67): Arithmetic Shift Right memory byte at indexed EA.
        op_asr_indexed => rmw_indexed, |cpu, val| cpu.perform_asr(val);

        /// ASL indexed (0x68): Arithmetic Shift Left memory byte at indexed EA.
        op_asl_indexed => rmw_indexed, |cpu, val| cpu.perform_asl(val);

        /// ROL indexed (0x69): Rotate Left memory byte at indexed EA through Carry.
        op_rol_indexed => rmw_indexed, |cpu, val| cpu.perform_rol(val);
    }
}
