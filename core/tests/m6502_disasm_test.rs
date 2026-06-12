use phosphor_core::cpu::Disassemble;
use phosphor_core::cpu::m6502::M6502;

// =============================================================================
// Helpers
// =============================================================================

fn dis(bytes: &[u8]) -> phosphor_core::cpu::DisassembledInstruction {
    M6502::disassemble(0x0000, bytes)
}

fn dis_at(addr: u32, bytes: &[u8]) -> phosphor_core::cpu::DisassembledInstruction {
    M6502::disassemble(addr, bytes)
}

// =============================================================================
// Implied — no operands (1 byte)
// =============================================================================

#[test]
fn test_nop() {
    let r = dis(&[0xEA]);
    assert_eq!(r.mnemonic, "NOP");
    assert_eq!(r.operands, "");
    assert_eq!(r.byte_len, 1);
    assert_eq!(r.target_addr, None);
}

#[test]
fn test_brk() {
    let r = dis(&[0x00]);
    assert_eq!(r.mnemonic, "BRK");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_stack_ops() {
    assert_eq!(dis(&[0x48]).mnemonic, "PHA");
    assert_eq!(dis(&[0x68]).mnemonic, "PLA");
    assert_eq!(dis(&[0x08]).mnemonic, "PHP");
    assert_eq!(dis(&[0x28]).mnemonic, "PLP");
}

#[test]
fn test_return_ops() {
    assert_eq!(dis(&[0x60]).mnemonic, "RTS");
    assert_eq!(dis(&[0x40]).mnemonic, "RTI");
}

#[test]
fn test_register_transfers() {
    assert_eq!(dis(&[0xAA]).mnemonic, "TAX");
    assert_eq!(dis(&[0xA8]).mnemonic, "TAY");
    assert_eq!(dis(&[0x8A]).mnemonic, "TXA");
    assert_eq!(dis(&[0x98]).mnemonic, "TYA");
    assert_eq!(dis(&[0xBA]).mnemonic, "TSX");
    assert_eq!(dis(&[0x9A]).mnemonic, "TXS");
}

#[test]
fn test_flag_ops() {
    assert_eq!(dis(&[0x18]).mnemonic, "CLC");
    assert_eq!(dis(&[0x38]).mnemonic, "SEC");
    assert_eq!(dis(&[0x58]).mnemonic, "CLI");
    assert_eq!(dis(&[0x78]).mnemonic, "SEI");
    assert_eq!(dis(&[0xB8]).mnemonic, "CLV");
    assert_eq!(dis(&[0xD8]).mnemonic, "CLD");
    assert_eq!(dis(&[0xF8]).mnemonic, "SED");
}

#[test]
fn test_register_inc_dec() {
    assert_eq!(dis(&[0xE8]).mnemonic, "INX");
    assert_eq!(dis(&[0xC8]).mnemonic, "INY");
    assert_eq!(dis(&[0xCA]).mnemonic, "DEX");
    assert_eq!(dis(&[0x88]).mnemonic, "DEY");
}

// =============================================================================
// Accumulator addressing (1 byte)
// =============================================================================

#[test]
fn test_asl_acc() {
    let r = dis(&[0x0A]);
    assert_eq!(r.mnemonic, "ASL");
    assert_eq!(r.operands, "A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_lsr_acc() {
    let r = dis(&[0x4A]);
    assert_eq!(r.mnemonic, "LSR");
    assert_eq!(r.operands, "A");
}

#[test]
fn test_rol_acc() {
    let r = dis(&[0x2A]);
    assert_eq!(r.mnemonic, "ROL");
    assert_eq!(r.operands, "A");
}

#[test]
fn test_ror_acc() {
    let r = dis(&[0x6A]);
    assert_eq!(r.mnemonic, "ROR");
    assert_eq!(r.operands, "A");
}

// =============================================================================
// Immediate (2 bytes) — #$XX
// =============================================================================

#[test]
fn test_lda_imm() {
    let r = dis(&[0xA9, 0x42]);
    assert_eq!(r.mnemonic, "LDA");
    assert_eq!(r.operands, "#$42");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, None);
}

#[test]
fn test_ldx_imm() {
    let r = dis(&[0xA2, 0xFF]);
    assert_eq!(r.mnemonic, "LDX");
    assert_eq!(r.operands, "#$FF");
}

#[test]
fn test_ldy_imm() {
    let r = dis(&[0xA0, 0x00]);
    assert_eq!(r.mnemonic, "LDY");
    assert_eq!(r.operands, "#$00");
}

#[test]
fn test_alu_imm() {
    let cases: &[(u8, &str)] = &[
        (0x69, "ADC"),
        (0xE9, "SBC"),
        (0x29, "AND"),
        (0x09, "ORA"),
        (0x49, "EOR"),
        (0xC9, "CMP"),
        (0xE0, "CPX"),
        (0xC0, "CPY"),
    ];
    for &(opcode, mnemonic) in cases {
        let r = dis(&[opcode, 0x55]);
        assert_eq!(r.mnemonic, mnemonic, "opcode 0x{:02X}", opcode);
        assert_eq!(r.operands, "#$55");
        assert_eq!(r.byte_len, 2);
    }
}

// =============================================================================
// Zero Page (2 bytes) — $XX
// =============================================================================

#[test]
fn test_lda_zp() {
    let r = dis(&[0xA5, 0x42]);
    assert_eq!(r.mnemonic, "LDA");
    assert_eq!(r.operands, "$42");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0042));
}

#[test]
fn test_sta_zp() {
    let r = dis(&[0x85, 0x80]);
    assert_eq!(r.mnemonic, "STA");
    assert_eq!(r.operands, "$80");
    assert_eq!(r.target_addr, Some(0x0080));
}

#[test]
fn test_zp_memory_ops() {
    let cases: &[(u8, &str)] = &[
        (0x06, "ASL"),
        (0x46, "LSR"),
        (0x26, "ROL"),
        (0x66, "ROR"),
        (0xC6, "DEC"),
        (0xE6, "INC"),
        (0x24, "BIT"),
    ];
    for &(opcode, mnemonic) in cases {
        let r = dis(&[opcode, 0x10]);
        assert_eq!(r.mnemonic, mnemonic, "opcode 0x{:02X}", opcode);
        assert_eq!(r.operands, "$10");
        assert_eq!(r.byte_len, 2);
    }
}

// =============================================================================
// Zero Page,X (2 bytes) — $XX,X
// =============================================================================

#[test]
fn test_lda_zpx() {
    let r = dis(&[0xB5, 0x10]);
    assert_eq!(r.mnemonic, "LDA");
    assert_eq!(r.operands, "$10,X");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, None);
}

#[test]
fn test_sta_zpx() {
    let r = dis(&[0x95, 0x20]);
    assert_eq!(r.mnemonic, "STA");
    assert_eq!(r.operands, "$20,X");
}

#[test]
fn test_sty_zpx() {
    let r = dis(&[0x94, 0x30]);
    assert_eq!(r.mnemonic, "STY");
    assert_eq!(r.operands, "$30,X");
}

// =============================================================================
// Zero Page,Y (2 bytes) — $XX,Y
// =============================================================================

#[test]
fn test_ldx_zpy() {
    let r = dis(&[0xB6, 0x40]);
    assert_eq!(r.mnemonic, "LDX");
    assert_eq!(r.operands, "$40,Y");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, None);
}

#[test]
fn test_stx_zpy() {
    let r = dis(&[0x96, 0x50]);
    assert_eq!(r.mnemonic, "STX");
    assert_eq!(r.operands, "$50,Y");
}

// =============================================================================
// Absolute (3 bytes) — $XXXX (little-endian)
// =============================================================================

#[test]
fn test_lda_abs() {
    let r = dis(&[0xAD, 0x34, 0x12]);
    assert_eq!(r.mnemonic, "LDA");
    assert_eq!(r.operands, "$1234");
    assert_eq!(r.byte_len, 3);
    assert_eq!(r.target_addr, Some(0x1234));
}

#[test]
fn test_sta_abs() {
    let r = dis(&[0x8D, 0x00, 0xCC]);
    assert_eq!(r.mnemonic, "STA");
    assert_eq!(r.operands, "$CC00");
    assert_eq!(r.target_addr, Some(0xCC00));
}

#[test]
fn test_jmp_abs() {
    let r = dis(&[0x4C, 0x00, 0xF0]);
    assert_eq!(r.mnemonic, "JMP");
    assert_eq!(r.operands, "$F000");
    assert_eq!(r.byte_len, 3);
    assert_eq!(r.target_addr, Some(0xF000));
}

#[test]
fn test_jsr_abs() {
    let r = dis(&[0x20, 0x00, 0x10]);
    assert_eq!(r.mnemonic, "JSR");
    assert_eq!(r.operands, "$1000");
    assert_eq!(r.target_addr, Some(0x1000));
}

#[test]
fn test_abs_memory_ops() {
    let cases: &[(u8, &str)] = &[
        (0x0E, "ASL"),
        (0x4E, "LSR"),
        (0x2E, "ROL"),
        (0x6E, "ROR"),
        (0xCE, "DEC"),
        (0xEE, "INC"),
        (0x2C, "BIT"),
    ];
    for &(opcode, mnemonic) in cases {
        let r = dis(&[opcode, 0x00, 0x20]);
        assert_eq!(r.mnemonic, mnemonic, "opcode 0x{:02X}", opcode);
        assert_eq!(r.operands, "$2000");
        assert_eq!(r.byte_len, 3);
    }
}

// =============================================================================
// Absolute,X (3 bytes) — $XXXX,X
// =============================================================================

#[test]
fn test_lda_abx() {
    let r = dis(&[0xBD, 0x00, 0x40]);
    assert_eq!(r.mnemonic, "LDA");
    assert_eq!(r.operands, "$4000,X");
    assert_eq!(r.byte_len, 3);
    assert_eq!(r.target_addr, None);
}

#[test]
fn test_sta_abx() {
    let r = dis(&[0x9D, 0x00, 0x50]);
    assert_eq!(r.mnemonic, "STA");
    assert_eq!(r.operands, "$5000,X");
}

// =============================================================================
// Absolute,Y (3 bytes) — $XXXX,Y
// =============================================================================

#[test]
fn test_lda_aby() {
    let r = dis(&[0xB9, 0x00, 0x60]);
    assert_eq!(r.mnemonic, "LDA");
    assert_eq!(r.operands, "$6000,Y");
    assert_eq!(r.byte_len, 3);
    assert_eq!(r.target_addr, None);
}

#[test]
fn test_sta_aby() {
    let r = dis(&[0x99, 0x00, 0x70]);
    assert_eq!(r.mnemonic, "STA");
    assert_eq!(r.operands, "$7000,Y");
}

#[test]
fn test_ldx_aby() {
    let r = dis(&[0xBE, 0x00, 0x80]);
    assert_eq!(r.mnemonic, "LDX");
    assert_eq!(r.operands, "$8000,Y");
}

// =============================================================================
// Indirect (3 bytes) — ($XXXX) — JMP only
// =============================================================================

#[test]
fn test_jmp_ind() {
    let r = dis(&[0x6C, 0xFC, 0xFF]);
    assert_eq!(r.mnemonic, "JMP");
    assert_eq!(r.operands, "($FFFC)");
    assert_eq!(r.byte_len, 3);
    assert_eq!(r.target_addr, None); // runtime-dependent
}

// =============================================================================
// (Indirect,X) — ($XX,X) (2 bytes)
// =============================================================================

#[test]
fn test_lda_izx() {
    let r = dis(&[0xA1, 0x40]);
    assert_eq!(r.mnemonic, "LDA");
    assert_eq!(r.operands, "($40,X)");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, None);
}

#[test]
fn test_sta_izx() {
    let r = dis(&[0x81, 0x50]);
    assert_eq!(r.mnemonic, "STA");
    assert_eq!(r.operands, "($50,X)");
}

#[test]
fn test_alu_izx() {
    let cases: &[(u8, &str)] = &[
        (0x01, "ORA"),
        (0x21, "AND"),
        (0x41, "EOR"),
        (0x61, "ADC"),
        (0xC1, "CMP"),
        (0xE1, "SBC"),
    ];
    for &(opcode, mnemonic) in cases {
        let r = dis(&[opcode, 0x60]);
        assert_eq!(r.mnemonic, mnemonic, "opcode 0x{:02X}", opcode);
        assert_eq!(r.operands, "($60,X)");
        assert_eq!(r.byte_len, 2);
    }
}

// =============================================================================
// (Indirect),Y — ($XX),Y (2 bytes)
// =============================================================================

#[test]
fn test_lda_izy() {
    let r = dis(&[0xB1, 0x70]);
    assert_eq!(r.mnemonic, "LDA");
    assert_eq!(r.operands, "($70),Y");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, None);
}

#[test]
fn test_sta_izy() {
    let r = dis(&[0x91, 0x80]);
    assert_eq!(r.mnemonic, "STA");
    assert_eq!(r.operands, "($80),Y");
}

#[test]
fn test_alu_izy() {
    let cases: &[(u8, &str)] = &[
        (0x11, "ORA"),
        (0x31, "AND"),
        (0x51, "EOR"),
        (0x71, "ADC"),
        (0xD1, "CMP"),
        (0xF1, "SBC"),
    ];
    for &(opcode, mnemonic) in cases {
        let r = dis(&[opcode, 0x90]);
        assert_eq!(r.mnemonic, mnemonic, "opcode 0x{:02X}", opcode);
        assert_eq!(r.operands, "($90),Y");
        assert_eq!(r.byte_len, 2);
    }
}

// =============================================================================
// Relative branches (2 bytes)
// =============================================================================

#[test]
fn test_branch_forward() {
    let r = dis_at(0x1000, &[0xD0, 0x10]); // BNE
    assert_eq!(r.mnemonic, "BNE");
    assert_eq!(r.operands, "$1012");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x1012));
}

#[test]
fn test_branch_backward() {
    let r = dis_at(0x1000, &[0xF0, 0xF0]); // BEQ offset -16
    assert_eq!(r.mnemonic, "BEQ");
    assert_eq!(r.operands, "$0FF2");
    assert_eq!(r.target_addr, Some(0x0FF2));
}

#[test]
fn test_branch_self() {
    let r = dis_at(0x1000, &[0xD0, 0xFE]); // BNE offset -2 → branch to self
    assert_eq!(r.operands, "$1000");
    assert_eq!(r.target_addr, Some(0x1000));
}

#[test]
fn test_all_branch_mnemonics() {
    let cases: &[(u8, &str)] = &[
        (0x10, "BPL"),
        (0x30, "BMI"),
        (0x50, "BVC"),
        (0x70, "BVS"),
        (0x90, "BCC"),
        (0xB0, "BCS"),
        (0xD0, "BNE"),
        (0xF0, "BEQ"),
    ];
    for &(opcode, mnemonic) in cases {
        let r = dis_at(0x0100, &[opcode, 0x20]);
        assert_eq!(r.mnemonic, mnemonic, "opcode 0x{:02X}", opcode);
        assert_eq!(r.operands, "$0122");
        assert_eq!(r.target_addr, Some(0x0122));
    }
}

#[test]
fn test_branch_wrap_forward() {
    let r = dis_at(0xFFFE, &[0xD0, 0x10]);
    assert_eq!(r.target_addr, Some(0x0010));
}

#[test]
fn test_branch_wrap_backward() {
    let r = dis_at(0x0000, &[0xD0, 0xF0]);
    assert_eq!(r.target_addr, Some(0xFFF2));
}

// =============================================================================
// Little-endian byte order for absolute addresses
// =============================================================================

#[test]
fn test_abs_little_endian() {
    // LDA $ABCD stored as AD CD AB
    let r = dis(&[0xAD, 0xCD, 0xAB]);
    assert_eq!(r.operands, "$ABCD");
    assert_eq!(r.target_addr, Some(0xABCD));
}

// =============================================================================
// Illegal opcodes
// =============================================================================

#[test]
fn test_illegal_opcodes() {
    let illegals = [0x02, 0x03, 0x04, 0x07, 0x0B, 0x0C, 0x0F, 0x80, 0x89];
    for &opcode in &illegals {
        let r = dis(&[opcode]);
        assert_eq!(r.mnemonic, "???", "opcode 0x{:02X}", opcode);
        assert_eq!(r.byte_len, 1);
    }
}

// =============================================================================
// Edge cases
// =============================================================================

#[test]
fn test_empty_bytes() {
    let r = M6502::disassemble(0x0000, &[]);
    assert_eq!(r.mnemonic, "???");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_truncated_2byte() {
    let r = dis(&[0xA9]); // LDA immediate needs 2 bytes
    assert_eq!(r.mnemonic, "???");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_truncated_3byte() {
    let r = dis(&[0xAD, 0x34]); // LDA absolute needs 3 bytes
    assert_eq!(r.mnemonic, "???");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_raw_bytes_captured() {
    let r = dis(&[0xAD, 0x34, 0x12, 0xFF]);
    assert_eq!(r.bytes[0], 0xAD);
    assert_eq!(r.bytes[1], 0x34);
    assert_eq!(r.bytes[2], 0x12);
    assert_eq!(r.byte_len, 3);
}

// =============================================================================
// Display formatting
// =============================================================================

#[test]
fn test_display_implied() {
    assert_eq!(format!("{}", dis(&[0xEA])), "NOP");
}

#[test]
fn test_display_accumulator() {
    assert_eq!(format!("{}", dis(&[0x0A])), "ASL   A");
}

#[test]
fn test_display_imm() {
    assert_eq!(format!("{}", dis(&[0xA9, 0x42])), "LDA   #$42");
}

#[test]
fn test_display_zp() {
    assert_eq!(format!("{}", dis(&[0xA5, 0x42])), "LDA   $42");
}

#[test]
fn test_display_zpx() {
    assert_eq!(format!("{}", dis(&[0xB5, 0x10])), "LDA   $10,X");
}

#[test]
fn test_display_zpy() {
    assert_eq!(format!("{}", dis(&[0xB6, 0x20])), "LDX   $20,Y");
}

#[test]
fn test_display_abs() {
    assert_eq!(format!("{}", dis(&[0xAD, 0x34, 0x12])), "LDA   $1234");
}

#[test]
fn test_display_abx() {
    assert_eq!(format!("{}", dis(&[0xBD, 0x00, 0x40])), "LDA   $4000,X");
}

#[test]
fn test_display_aby() {
    assert_eq!(format!("{}", dis(&[0xB9, 0x00, 0x60])), "LDA   $6000,Y");
}

#[test]
fn test_display_ind() {
    assert_eq!(format!("{}", dis(&[0x6C, 0xFC, 0xFF])), "JMP   ($FFFC)");
}

#[test]
fn test_display_izx() {
    assert_eq!(format!("{}", dis(&[0xA1, 0x40])), "LDA   ($40,X)");
}

#[test]
fn test_display_izy() {
    assert_eq!(format!("{}", dis(&[0xB1, 0x70])), "LDA   ($70),Y");
}

#[test]
fn test_display_rel() {
    assert_eq!(format!("{}", dis_at(0x1000, &[0xD0, 0x10])), "BNE   $1012");
}

// =============================================================================
// Symbol resolution
// =============================================================================

#[test]
fn test_symbols_branch_match() {
    let r = dis_at(0x1000, &[0xD0, 0x10]);
    let output = r.format_with_symbols(|addr| if addr == 0x1012 { Some("loop") } else { None });
    assert_eq!(output, "BNE   loop");
}

#[test]
fn test_symbols_abs_match() {
    let r = dis(&[0xAD, 0x00, 0xCC]);
    let output = r.format_with_symbols(|addr| {
        if addr == 0xCC00 {
            Some("PIA_0_A")
        } else {
            None
        }
    });
    assert_eq!(output, "LDA   PIA_0_A");
}

#[test]
fn test_symbols_zp_match() {
    let r = dis(&[0xA5, 0x80]);
    let output = r.format_with_symbols(|addr| {
        if addr == 0x0080 {
            Some("player_x")
        } else {
            None
        }
    });
    assert_eq!(output, "LDA   player_x");
}

#[test]
fn test_symbols_no_match() {
    let r = dis(&[0xAD, 0x34, 0x12]);
    let output = r.format_with_symbols(|_| None);
    assert_eq!(output, "LDA   $1234");
}

#[test]
fn test_symbols_no_target() {
    let r = dis(&[0xEA]);
    let output = r.format_with_symbols(|_| None);
    assert_eq!(output, "NOP");
}

// =============================================================================
// Full opcode coverage sweep
// =============================================================================

#[test]
fn test_all_opcodes_have_valid_byte_len() {
    let mut buf = [0u8; 3];
    for opcode in 0x00..=0xFFu8 {
        buf[0] = opcode;
        buf[1] = 0x00;
        buf[2] = 0x00;
        let r = M6502::disassemble(0x0000, &buf);
        assert!(
            r.byte_len >= 1 && r.byte_len <= 3,
            "opcode 0x{:02X}: unexpected byte_len {}",
            opcode,
            r.byte_len
        );
    }
}

#[test]
fn test_implemented_opcodes_decode_correctly() {
    let implemented: &[u8] = &[
        0x00, // BRK
        0x01, 0x05, 0x06, 0x08, 0x09, 0x0A, 0x0D, 0x0E, // ORA/ASL/PHP
        0x10, 0x11, 0x15, 0x16, 0x18, 0x19, 0x1D, 0x1E, // BPL/ORA/CLC
        0x20, 0x21, 0x24, 0x25, 0x26, 0x28, 0x29, 0x2A, 0x2C, 0x2D,
        0x2E, // JSR/AND/BIT/ROL/PLP
        0x30, 0x31, 0x35, 0x36, 0x38, 0x39, 0x3D, 0x3E, // BMI/AND/SEC
        0x40, 0x41, 0x45, 0x46, 0x48, 0x49, 0x4A, 0x4C, 0x4D, 0x4E, // RTI/EOR/LSR/PHA/JMP
        0x50, 0x51, 0x55, 0x56, 0x58, 0x59, 0x5D, 0x5E, // BVC/EOR/CLI
        0x60, 0x61, 0x65, 0x66, 0x68, 0x69, 0x6A, 0x6C, 0x6D, 0x6E, // RTS/ADC/ROR/PLA/JMP
        0x70, 0x71, 0x75, 0x76, 0x78, 0x79, 0x7D, 0x7E, // BVS/ADC/SEI
        0x81, 0x84, 0x85, 0x86, 0x88, 0x8A, 0x8C, 0x8D, 0x8E, // STA/STY/STX/DEY/TXA
        0x90, 0x91, 0x94, 0x95, 0x96, 0x98, 0x99, 0x9A, 0x9D, // BCC/STA/STY/STX/TYA/TXS
        0xA0, 0xA1, 0xA2, 0xA4, 0xA5, 0xA6, 0xA8, 0xA9, 0xAA, 0xAC, 0xAD,
        0xAE, // LDY/LDA/LDX/TAY/TAX
        0xB0, 0xB1, 0xB4, 0xB5, 0xB6, 0xB8, 0xB9, 0xBA, 0xBC, 0xBD,
        0xBE, // BCS/LDA/LDX/LDY/CLV/TSX
        0xC0, 0xC1, 0xC4, 0xC5, 0xC6, 0xC8, 0xC9, 0xCA, 0xCC, 0xCD,
        0xCE, // CPY/CMP/DEC/INY/DEX
        0xD0, 0xD1, 0xD5, 0xD6, 0xD8, 0xD9, 0xDD, 0xDE, // BNE/CMP/DEC/CLD
        0xE0, 0xE1, 0xE4, 0xE5, 0xE6, 0xE8, 0xE9, 0xEA, 0xEC, 0xED,
        0xEE, // CPX/SBC/INC/INX/NOP
        0xF0, 0xF1, 0xF5, 0xF6, 0xF8, 0xF9, 0xFD, 0xFE, // BEQ/SBC/INC/SED
    ];

    let mut buf = [0u8; 3];
    for &opcode in implemented {
        buf[0] = opcode;
        buf[1] = 0x00;
        buf[2] = 0x00;
        let r = M6502::disassemble(0x0000, &buf);
        assert_ne!(
            r.mnemonic, "???",
            "opcode 0x{:02X} should decode to a known mnemonic",
            opcode
        );
    }
}
