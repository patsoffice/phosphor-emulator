use crate::core::{Bus, BusMaster};
use crate::cpu::m68xx::M68xxAlu;
use crate::cpu::m68xx_alu_macros::{m68xx_alu_inherent, m68xx_alu_rmw};
use crate::cpu::m6800::{ExecState, M6800};

impl M6800 {
    // --- Inherent register ops (2 cycles: 1 fetch + 1 internal) ---

    m68xx_alu_inherent! {
        /// ASLA inherent (0x48): Arithmetic Shift Left A.
        op_asla => a, perform_asl;

        /// ASLB inherent (0x58): Arithmetic Shift Left B.
        op_aslb => b, perform_asl;

        /// ASRA inherent (0x47): Arithmetic Shift Right A.
        op_asra => a, perform_asr;

        /// ASRB inherent (0x57): Arithmetic Shift Right B.
        op_asrb => b, perform_asr;

        /// LSRA inherent (0x44): Logical Shift Right A.
        op_lsra => a, perform_lsr;

        /// LSRB inherent (0x54): Logical Shift Right B.
        op_lsrb => b, perform_lsr;

        /// ROLA inherent (0x49): Rotate Left A through Carry.
        op_rola => a, perform_rol;

        /// ROLB inherent (0x59): Rotate Left B through Carry.
        op_rolb => b, perform_rol;

        /// RORA inherent (0x46): Rotate Right A through Carry.
        op_rora => a, perform_ror;

        /// RORB inherent (0x56): Rotate Right B through Carry.
        op_rorb => b, perform_ror;
    }

    // --- Memory shift/rotate ops: indexed (7 cycles) and extended (6 cycles) ---

    m68xx_alu_rmw! {
        /// ASL indexed (0x68).
        op_asl_idx => rmw_indexed, |cpu, val| cpu.perform_asl(val);

        /// ASL extended (0x78).
        op_asl_ext => rmw_extended, |cpu, val| cpu.perform_asl(val);

        /// ASR indexed (0x67).
        op_asr_idx => rmw_indexed, |cpu, val| cpu.perform_asr(val);

        /// ASR extended (0x77).
        op_asr_ext => rmw_extended, |cpu, val| cpu.perform_asr(val);

        /// LSR indexed (0x64).
        op_lsr_idx => rmw_indexed, |cpu, val| cpu.perform_lsr(val);

        /// LSR extended (0x74).
        op_lsr_ext => rmw_extended, |cpu, val| cpu.perform_lsr(val);

        /// ROL indexed (0x69).
        op_rol_idx => rmw_indexed, |cpu, val| cpu.perform_rol(val);

        /// ROL extended (0x79).
        op_rol_ext => rmw_extended, |cpu, val| cpu.perform_rol(val);

        /// ROR indexed (0x66).
        op_ror_idx => rmw_indexed, |cpu, val| cpu.perform_ror(val);

        /// ROR extended (0x76).
        op_ror_ext => rmw_extended, |cpu, val| cpu.perform_ror(val);
    }
}
