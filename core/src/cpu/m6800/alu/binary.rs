use crate::core::{Bus, BusMaster};
use crate::cpu::m68xx::{Acc, M68xxAlu};
use crate::cpu::m68xx_alu_macros::m68xx_alu_acc;
use crate::cpu::m6800::M6800;

impl M6800 {
    // --- Direct mode ops (3 cycles: 1 fetch + 1 read addr + 1 read operand) ---

    m68xx_alu_acc! {
        /// SUBA direct (0x90). N, Z, V, C affected.
        op_suba_dir => alu_direct, perform_sub, A;

        /// CMPA direct (0x91). N, Z, V, C affected.
        op_cmpa_dir => alu_direct, perform_cmp, A;

        /// SBCA direct (0x92). N, Z, V, C affected.
        op_sbca_dir => alu_direct, perform_sbc, A;

        /// ANDA direct (0x94). N, Z affected. V cleared.
        op_anda_dir => alu_direct, perform_and, A;

        /// BITA direct (0x95). N, Z affected. V cleared.
        op_bita_dir => alu_direct, perform_bit, A;

        /// EORA direct (0x98). N, Z affected. V cleared.
        op_eora_dir => alu_direct, perform_eor, A;

        /// ADCA direct (0x99). H, N, Z, V, C affected.
        op_adca_dir => alu_direct, perform_adc, A;

        /// ORAA direct (0x9A). N, Z affected. V cleared.
        op_oraa_dir => alu_direct, perform_or, A;

        /// ADDA direct (0x9B). H, N, Z, V, C affected.
        op_adda_dir => alu_direct, perform_add, A;

        /// SUBB direct (0xD0). N, Z, V, C affected.
        op_subb_dir => alu_direct, perform_sub, B;

        /// CMPB direct (0xD1). N, Z, V, C affected.
        op_cmpb_dir => alu_direct, perform_cmp, B;

        /// SBCB direct (0xD2). N, Z, V, C affected.
        op_sbcb_dir => alu_direct, perform_sbc, B;

        /// ANDB direct (0xD4). N, Z affected. V cleared.
        op_andb_dir => alu_direct, perform_and, B;

        /// BITB direct (0xD5). N, Z affected. V cleared.
        op_bitb_dir => alu_direct, perform_bit, B;

        /// EORB direct (0xD8). N, Z affected. V cleared.
        op_eorb_dir => alu_direct, perform_eor, B;

        /// ADCB direct (0xD9). H, N, Z, V, C affected.
        op_adcb_dir => alu_direct, perform_adc, B;

        /// ORAB direct (0xDA). N, Z affected. V cleared.
        op_orab_dir => alu_direct, perform_or, B;

        /// ADDB direct (0xDB). H, N, Z, V, C affected.
        op_addb_dir => alu_direct, perform_add, B;
    }

    // --- Indexed mode ops (5 cycles: 1 fetch + 1 read offset + 2 internal + 1 read operand) ---

    m68xx_alu_acc! {
        /// SUBA indexed (0xA0). N, Z, V, C affected.
        op_suba_idx => alu_indexed, perform_sub, A;

        /// CMPA indexed (0xA1). N, Z, V, C affected.
        op_cmpa_idx => alu_indexed, perform_cmp, A;

        /// SBCA indexed (0xA2). N, Z, V, C affected.
        op_sbca_idx => alu_indexed, perform_sbc, A;

        /// ANDA indexed (0xA4). N, Z affected. V cleared.
        op_anda_idx => alu_indexed, perform_and, A;

        /// BITA indexed (0xA5). N, Z affected. V cleared.
        op_bita_idx => alu_indexed, perform_bit, A;

        /// EORA indexed (0xA8). N, Z affected. V cleared.
        op_eora_idx => alu_indexed, perform_eor, A;

        /// ADCA indexed (0xA9). H, N, Z, V, C affected.
        op_adca_idx => alu_indexed, perform_adc, A;

        /// ORAA indexed (0xAA). N, Z affected. V cleared.
        op_oraa_idx => alu_indexed, perform_or, A;

        /// ADDA indexed (0xAB). H, N, Z, V, C affected.
        op_adda_idx => alu_indexed, perform_add, A;

        /// SUBB indexed (0xE0). N, Z, V, C affected.
        op_subb_idx => alu_indexed, perform_sub, B;

        /// CMPB indexed (0xE1). N, Z, V, C affected.
        op_cmpb_idx => alu_indexed, perform_cmp, B;

        /// SBCB indexed (0xE2). N, Z, V, C affected.
        op_sbcb_idx => alu_indexed, perform_sbc, B;

        /// ANDB indexed (0xE4). N, Z affected. V cleared.
        op_andb_idx => alu_indexed, perform_and, B;

        /// BITB indexed (0xE5). N, Z affected. V cleared.
        op_bitb_idx => alu_indexed, perform_bit, B;

        /// EORB indexed (0xE8). N, Z affected. V cleared.
        op_eorb_idx => alu_indexed, perform_eor, B;

        /// ADCB indexed (0xE9). H, N, Z, V, C affected.
        op_adcb_idx => alu_indexed, perform_adc, B;

        /// ORAB indexed (0xEA). N, Z affected. V cleared.
        op_orab_idx => alu_indexed, perform_or, B;

        /// ADDB indexed (0xEB). H, N, Z, V, C affected.
        op_addb_idx => alu_indexed, perform_add, B;
    }

    // --- Extended mode ops (4 cycles: 1 fetch + 1 read hi + 1 read lo + 1 read operand) ---

    m68xx_alu_acc! {
        /// SUBA extended (0xB0). N, Z, V, C affected.
        op_suba_ext => alu_extended, perform_sub, A;

        /// CMPA extended (0xB1). N, Z, V, C affected.
        op_cmpa_ext => alu_extended, perform_cmp, A;

        /// SBCA extended (0xB2). N, Z, V, C affected.
        op_sbca_ext => alu_extended, perform_sbc, A;

        /// ANDA extended (0xB4). N, Z affected. V cleared.
        op_anda_ext => alu_extended, perform_and, A;

        /// BITA extended (0xB5). N, Z affected. V cleared.
        op_bita_ext => alu_extended, perform_bit, A;

        /// EORA extended (0xB8). N, Z affected. V cleared.
        op_eora_ext => alu_extended, perform_eor, A;

        /// ADCA extended (0xB9). H, N, Z, V, C affected.
        op_adca_ext => alu_extended, perform_adc, A;

        /// ORAA extended (0xBA). N, Z affected. V cleared.
        op_oraa_ext => alu_extended, perform_or, A;

        /// ADDA extended (0xBB). H, N, Z, V, C affected.
        op_adda_ext => alu_extended, perform_add, A;

        /// SUBB extended (0xF0). N, Z, V, C affected.
        op_subb_ext => alu_extended, perform_sub, B;

        /// CMPB extended (0xF1). N, Z, V, C affected.
        op_cmpb_ext => alu_extended, perform_cmp, B;

        /// SBCB extended (0xF2). N, Z, V, C affected.
        op_sbcb_ext => alu_extended, perform_sbc, B;

        /// ANDB extended (0xF4). N, Z affected. V cleared.
        op_andb_ext => alu_extended, perform_and, B;

        /// BITB extended (0xF5). N, Z affected. V cleared.
        op_bitb_ext => alu_extended, perform_bit, B;

        /// EORB extended (0xF8). N, Z affected. V cleared.
        op_eorb_ext => alu_extended, perform_eor, B;

        /// ADCB extended (0xF9). H, N, Z, V, C affected.
        op_adcb_ext => alu_extended, perform_adc, B;

        /// ORAB extended (0xFA). N, Z affected. V cleared.
        op_orab_ext => alu_extended, perform_or, B;

        /// ADDB extended (0xFB). H, N, Z, V, C affected.
        op_addb_ext => alu_extended, perform_add, B;
    }

    // --- Immediate mode ops (2 cycles: 1 fetch + 1 read operand & execute) ---

    m68xx_alu_acc! {
        /// SUBA immediate (0x80). N, Z, V, C affected.
        op_suba_imm => alu_imm, perform_sub, A;

        /// CMPA immediate (0x81). N, Z, V, C affected.
        op_cmpa_imm => alu_imm, perform_cmp, A;

        /// SBCA immediate (0x82). A = A - M - C. N, Z, V, C affected.
        op_sbca_imm => alu_imm, perform_sbc, A;

        /// ANDA immediate (0x84). N, Z affected. V cleared.
        op_anda_imm => alu_imm, perform_and, A;

        /// BITA immediate (0x85). N, Z affected. V cleared.
        op_bita_imm => alu_imm, perform_bit, A;

        /// EORA immediate (0x88). N, Z affected. V cleared.
        op_eora_imm => alu_imm, perform_eor, A;

        /// ADCA immediate (0x89). A = A + M + C. H, N, Z, V, C affected.
        op_adca_imm => alu_imm, perform_adc, A;

        /// ORAA immediate (0x8A). N, Z affected. V cleared.
        op_oraa_imm => alu_imm, perform_or, A;

        /// ADDA immediate (0x8B). H, N, Z, V, C affected.
        op_adda_imm => alu_imm, perform_add, A;

        /// SUBB immediate (0xC0). N, Z, V, C affected.
        op_subb_imm => alu_imm, perform_sub, B;

        /// CMPB immediate (0xC1). N, Z, V, C affected.
        op_cmpb_imm => alu_imm, perform_cmp, B;

        /// SBCB immediate (0xC2). B = B - M - C. N, Z, V, C affected.
        op_sbcb_imm => alu_imm, perform_sbc, B;

        /// ANDB immediate (0xC4). N, Z affected. V cleared.
        op_andb_imm => alu_imm, perform_and, B;

        /// BITB immediate (0xC5). N, Z affected. V cleared.
        op_bitb_imm => alu_imm, perform_bit, B;

        /// EORB immediate (0xC8). N, Z affected. V cleared.
        op_eorb_imm => alu_imm, perform_eor, B;

        /// ADCB immediate (0xC9). B = B + M + C. H, N, Z, V, C affected.
        op_adcb_imm => alu_imm, perform_adc, B;

        /// ORAB immediate (0xCA). N, Z affected. V cleared.
        op_orab_imm => alu_imm, perform_or, B;

        /// ADDB immediate (0xCB). H, N, Z, V, C affected.
        op_addb_imm => alu_imm, perform_add, B;
    }
}
