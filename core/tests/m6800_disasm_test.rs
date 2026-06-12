use phosphor_core::cpu::Disassemble;
use phosphor_core::cpu::m6800::M6800;

// =============================================================================
// Helpers
// =============================================================================

fn dis(bytes: &[u8]) -> phosphor_core::cpu::DisassembledInstruction {
    M6800::disassemble(0x0000, bytes)
}

fn dis_at(addr: u32, bytes: &[u8]) -> phosphor_core::cpu::DisassembledInstruction {
    M6800::disassemble(addr, bytes)
}

// =============================================================================
// Inherent — no operands (1 byte)
// =============================================================================

#[test]
fn test_nop() {
    let r = dis(&[0x01]);
    assert_eq!(r.mnemonic, "NOP");
    assert_eq!(r.operands, "");
    assert_eq!(r.byte_len, 1);
    assert_eq!(r.target_addr, None);
}

#[test]
fn test_transfer_and_flag_ops() {
    // TAP, TPA, TAB, TBA
    assert_eq!(dis(&[0x06]).mnemonic, "TAP");
    assert_eq!(dis(&[0x07]).mnemonic, "TPA");
    assert_eq!(dis(&[0x16]).mnemonic, "TAB");
    assert_eq!(dis(&[0x17]).mnemonic, "TBA");
    // SBA, CBA, ABA, DAA
    assert_eq!(dis(&[0x10]).mnemonic, "SBA");
    assert_eq!(dis(&[0x11]).mnemonic, "CBA");
    assert_eq!(dis(&[0x1B]).mnemonic, "ABA");
    assert_eq!(dis(&[0x19]).mnemonic, "DAA");
}

#[test]
fn test_flag_set_clear() {
    assert_eq!(dis(&[0x0A]).mnemonic, "CLV");
    assert_eq!(dis(&[0x0B]).mnemonic, "SEV");
    assert_eq!(dis(&[0x0C]).mnemonic, "CLC");
    assert_eq!(dis(&[0x0D]).mnemonic, "SEC");
    assert_eq!(dis(&[0x0E]).mnemonic, "CLI");
    assert_eq!(dis(&[0x0F]).mnemonic, "SEI");
}

#[test]
fn test_index_stack_register_ops() {
    assert_eq!(dis(&[0x08]).mnemonic, "INX");
    assert_eq!(dis(&[0x09]).mnemonic, "DEX");
    assert_eq!(dis(&[0x30]).mnemonic, "TSX");
    assert_eq!(dis(&[0x31]).mnemonic, "INS");
    assert_eq!(dis(&[0x34]).mnemonic, "DES");
    assert_eq!(dis(&[0x35]).mnemonic, "TXS");
}

#[test]
fn test_stack_ops() {
    assert_eq!(dis(&[0x32]).mnemonic, "PULA");
    assert_eq!(dis(&[0x33]).mnemonic, "PULB");
    assert_eq!(dis(&[0x36]).mnemonic, "PSHA");
    assert_eq!(dis(&[0x37]).mnemonic, "PSHB");
}

#[test]
fn test_control_flow_inherent() {
    assert_eq!(dis(&[0x39]).mnemonic, "RTS");
    assert_eq!(dis(&[0x3B]).mnemonic, "RTI");
    assert_eq!(dis(&[0x3E]).mnemonic, "WAI");
    assert_eq!(dis(&[0x3F]).mnemonic, "SWI");
}

// =============================================================================
// Accumulator A shift/unary inherent ops (1 byte)
// =============================================================================

#[test]
fn test_acc_a_shift_unary() {
    let cases: &[(u8, &str)] = &[
        (0x40, "NEGA"),
        (0x43, "COMA"),
        (0x44, "LSRA"),
        (0x46, "RORA"),
        (0x47, "ASRA"),
        (0x48, "ASLA"),
        (0x49, "ROLA"),
        (0x4A, "DECA"),
        (0x4C, "INCA"),
        (0x4D, "TSTA"),
        (0x4F, "CLRA"),
    ];
    for &(opcode, mnemonic) in cases {
        let r = dis(&[opcode]);
        assert_eq!(r.mnemonic, mnemonic, "opcode 0x{:02X}", opcode);
        assert_eq!(r.byte_len, 1);
        assert_eq!(r.operands, "");
    }
}

// =============================================================================
// Accumulator B shift/unary inherent ops (1 byte)
// =============================================================================

#[test]
fn test_acc_b_shift_unary() {
    let cases: &[(u8, &str)] = &[
        (0x50, "NEGB"),
        (0x53, "COMB"),
        (0x54, "LSRB"),
        (0x56, "RORB"),
        (0x57, "ASRB"),
        (0x58, "ASLB"),
        (0x59, "ROLB"),
        (0x5A, "DECB"),
        (0x5C, "INCB"),
        (0x5D, "TSTB"),
        (0x5F, "CLRB"),
    ];
    for &(opcode, mnemonic) in cases {
        let r = dis(&[opcode]);
        assert_eq!(r.mnemonic, mnemonic, "opcode 0x{:02X}", opcode);
        assert_eq!(r.byte_len, 1);
        assert_eq!(r.operands, "");
    }
}

// =============================================================================
// 8-bit immediate (2 bytes)
// =============================================================================

#[test]
fn test_ldaa_imm() {
    let r = dis(&[0x86, 0x42]);
    assert_eq!(r.mnemonic, "LDAA");
    assert_eq!(r.operands, "#$42");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, None);
}

#[test]
fn test_ldab_imm() {
    let r = dis(&[0xC6, 0xFF]);
    assert_eq!(r.mnemonic, "LDAB");
    assert_eq!(r.operands, "#$FF");
    assert_eq!(r.byte_len, 2);
}

#[test]
fn test_alu_a_imm() {
    let cases: &[(u8, &str)] = &[
        (0x80, "SUBA"),
        (0x81, "CMPA"),
        (0x82, "SBCA"),
        (0x84, "ANDA"),
        (0x85, "BITA"),
        (0x88, "EORA"),
        (0x89, "ADCA"),
        (0x8A, "ORAA"),
        (0x8B, "ADDA"),
    ];
    for &(opcode, mnemonic) in cases {
        let r = dis(&[opcode, 0x55]);
        assert_eq!(r.mnemonic, mnemonic, "opcode 0x{:02X}", opcode);
        assert_eq!(r.operands, "#$55");
        assert_eq!(r.byte_len, 2);
    }
}

#[test]
fn test_alu_b_imm() {
    let cases: &[(u8, &str)] = &[
        (0xC0, "SUBB"),
        (0xC1, "CMPB"),
        (0xC2, "SBCB"),
        (0xC4, "ANDB"),
        (0xC5, "BITB"),
        (0xC8, "EORB"),
        (0xC9, "ADCB"),
        (0xCA, "ORAB"),
        (0xCB, "ADDB"),
    ];
    for &(opcode, mnemonic) in cases {
        let r = dis(&[opcode, 0xAA]);
        assert_eq!(r.mnemonic, mnemonic, "opcode 0x{:02X}", opcode);
        assert_eq!(r.operands, "#$AA");
        assert_eq!(r.byte_len, 2);
    }
}

// =============================================================================
// 16-bit immediate (3 bytes)
// =============================================================================

#[test]
fn test_cpx_imm() {
    let r = dis(&[0x8C, 0x12, 0x34]);
    assert_eq!(r.mnemonic, "CPX");
    assert_eq!(r.operands, "#$1234");
    assert_eq!(r.byte_len, 3);
    assert_eq!(r.target_addr, None);
}

#[test]
fn test_lds_imm() {
    let r = dis(&[0x8E, 0x01, 0x00]);
    assert_eq!(r.mnemonic, "LDS");
    assert_eq!(r.operands, "#$0100");
    assert_eq!(r.byte_len, 3);
}

#[test]
fn test_ldx_imm() {
    let r = dis(&[0xCE, 0xFF, 0x00]);
    assert_eq!(r.mnemonic, "LDX");
    assert_eq!(r.operands, "#$FF00");
    assert_eq!(r.byte_len, 3);
}

// =============================================================================
// Direct addressing (2 bytes)
// =============================================================================

#[test]
fn test_ldaa_dir() {
    let r = dis(&[0x96, 0x42]);
    assert_eq!(r.mnemonic, "LDAA");
    assert_eq!(r.operands, "$42");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0042));
}

#[test]
fn test_staa_dir() {
    let r = dis(&[0x97, 0x80]);
    assert_eq!(r.mnemonic, "STAA");
    assert_eq!(r.operands, "$80");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0080));
}

#[test]
fn test_ldab_dir() {
    let r = dis(&[0xD6, 0x00]);
    assert_eq!(r.mnemonic, "LDAB");
    assert_eq!(r.operands, "$00");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0000));
}

#[test]
fn test_stab_dir() {
    let r = dis(&[0xD7, 0xFF]);
    assert_eq!(r.mnemonic, "STAB");
    assert_eq!(r.operands, "$FF");
    assert_eq!(r.target_addr, Some(0x00FF));
}

#[test]
fn test_cpx_dir() {
    let r = dis(&[0x9C, 0x10]);
    assert_eq!(r.mnemonic, "CPX");
    assert_eq!(r.operands, "$10");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0010));
}

#[test]
fn test_lds_dir() {
    let r = dis(&[0x9E, 0x50]);
    assert_eq!(r.mnemonic, "LDS");
    assert_eq!(r.operands, "$50");
}

#[test]
fn test_sts_dir() {
    let r = dis(&[0x9F, 0x60]);
    assert_eq!(r.mnemonic, "STS");
    assert_eq!(r.operands, "$60");
}

#[test]
fn test_ldx_dir() {
    let r = dis(&[0xDE, 0x20]);
    assert_eq!(r.mnemonic, "LDX");
    assert_eq!(r.operands, "$20");
}

#[test]
fn test_stx_dir() {
    let r = dis(&[0xDF, 0x30]);
    assert_eq!(r.mnemonic, "STX");
    assert_eq!(r.operands, "$30");
}

// =============================================================================
// Indexed addressing (2 bytes) — $XX,X
// =============================================================================

#[test]
fn test_ldaa_idx() {
    let r = dis(&[0xA6, 0x05]);
    assert_eq!(r.mnemonic, "LDAA");
    assert_eq!(r.operands, "$05,X");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, None);
}

#[test]
fn test_staa_idx() {
    let r = dis(&[0xA7, 0x00]);
    assert_eq!(r.mnemonic, "STAA");
    assert_eq!(r.operands, "$00,X");
}

#[test]
fn test_ldab_idx() {
    let r = dis(&[0xE6, 0xFF]);
    assert_eq!(r.mnemonic, "LDAB");
    assert_eq!(r.operands, "$FF,X");
}

#[test]
fn test_stab_idx() {
    let r = dis(&[0xE7, 0x10]);
    assert_eq!(r.mnemonic, "STAB");
    assert_eq!(r.operands, "$10,X");
}

#[test]
fn test_jmp_idx() {
    let r = dis(&[0x6E, 0x20]);
    assert_eq!(r.mnemonic, "JMP");
    assert_eq!(r.operands, "$20,X");
    assert_eq!(r.target_addr, None);
}

#[test]
fn test_jsr_idx() {
    let r = dis(&[0xAD, 0x30]);
    assert_eq!(r.mnemonic, "JSR");
    assert_eq!(r.operands, "$30,X");
    assert_eq!(r.target_addr, None);
}

#[test]
fn test_memory_rmw_idx() {
    let cases: &[(u8, &str)] = &[
        (0x60, "NEG"),
        (0x63, "COM"),
        (0x64, "LSR"),
        (0x66, "ROR"),
        (0x67, "ASR"),
        (0x68, "ASL"),
        (0x69, "ROL"),
        (0x6A, "DEC"),
        (0x6C, "INC"),
        (0x6D, "TST"),
        (0x6F, "CLR"),
    ];
    for &(opcode, mnemonic) in cases {
        let r = dis(&[opcode, 0x42]);
        assert_eq!(r.mnemonic, mnemonic, "opcode 0x{:02X}", opcode);
        assert_eq!(r.operands, "$42,X");
        assert_eq!(r.byte_len, 2);
    }
}

// =============================================================================
// Extended addressing (3 bytes) — $XXXX
// =============================================================================

#[test]
fn test_ldaa_ext() {
    let r = dis(&[0xB6, 0x12, 0x34]);
    assert_eq!(r.mnemonic, "LDAA");
    assert_eq!(r.operands, "$1234");
    assert_eq!(r.byte_len, 3);
    assert_eq!(r.target_addr, Some(0x1234));
}

#[test]
fn test_staa_ext() {
    let r = dis(&[0xB7, 0xCC, 0x00]);
    assert_eq!(r.mnemonic, "STAA");
    assert_eq!(r.operands, "$CC00");
    assert_eq!(r.target_addr, Some(0xCC00));
}

#[test]
fn test_ldab_ext() {
    let r = dis(&[0xF6, 0x80, 0x00]);
    assert_eq!(r.mnemonic, "LDAB");
    assert_eq!(r.operands, "$8000");
    assert_eq!(r.target_addr, Some(0x8000));
}

#[test]
fn test_stab_ext() {
    let r = dis(&[0xF7, 0xFF, 0xFF]);
    assert_eq!(r.mnemonic, "STAB");
    assert_eq!(r.operands, "$FFFF");
}

#[test]
fn test_jmp_ext() {
    let r = dis(&[0x7E, 0xF0, 0x00]);
    assert_eq!(r.mnemonic, "JMP");
    assert_eq!(r.operands, "$F000");
    assert_eq!(r.byte_len, 3);
    assert_eq!(r.target_addr, Some(0xF000));
}

#[test]
fn test_jsr_ext() {
    let r = dis(&[0xBD, 0x10, 0x00]);
    assert_eq!(r.mnemonic, "JSR");
    assert_eq!(r.operands, "$1000");
    assert_eq!(r.target_addr, Some(0x1000));
}

#[test]
fn test_memory_rmw_ext() {
    let cases: &[(u8, &str)] = &[
        (0x70, "NEG"),
        (0x73, "COM"),
        (0x74, "LSR"),
        (0x76, "ROR"),
        (0x77, "ASR"),
        (0x78, "ASL"),
        (0x79, "ROL"),
        (0x7A, "DEC"),
        (0x7C, "INC"),
        (0x7D, "TST"),
        (0x7F, "CLR"),
    ];
    for &(opcode, mnemonic) in cases {
        let r = dis(&[opcode, 0x20, 0x00]);
        assert_eq!(r.mnemonic, mnemonic, "opcode 0x{:02X}", opcode);
        assert_eq!(r.operands, "$2000");
        assert_eq!(r.byte_len, 3);
        assert_eq!(r.target_addr, Some(0x2000));
    }
}

#[test]
fn test_16bit_ext() {
    // CPX, LDS, STS, LDX, STX in extended mode
    let r = dis(&[0xBC, 0x40, 0x00]);
    assert_eq!(r.mnemonic, "CPX");
    assert_eq!(r.operands, "$4000");
    assert_eq!(r.byte_len, 3);

    let r = dis(&[0xBE, 0x50, 0x00]);
    assert_eq!(r.mnemonic, "LDS");
    assert_eq!(r.operands, "$5000");

    let r = dis(&[0xBF, 0x60, 0x00]);
    assert_eq!(r.mnemonic, "STS");
    assert_eq!(r.operands, "$6000");

    let r = dis(&[0xFE, 0x70, 0x00]);
    assert_eq!(r.mnemonic, "LDX");
    assert_eq!(r.operands, "$7000");

    let r = dis(&[0xFF, 0x80, 0x00]);
    assert_eq!(r.mnemonic, "STX");
    assert_eq!(r.operands, "$8000");
}

// =============================================================================
// Relative branches (2 bytes) — signed offset from PC+2
// =============================================================================

#[test]
fn test_bra_forward() {
    // BRA at $1000, offset +$10 → target $1012
    let r = dis_at(0x1000, &[0x20, 0x10]);
    assert_eq!(r.mnemonic, "BRA");
    assert_eq!(r.operands, "$1012");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x1012));
}

#[test]
fn test_bra_backward() {
    // BRA at $1000, offset -$10 (0xF0) → target $0FF2
    let r = dis_at(0x1000, &[0x20, 0xF0]);
    assert_eq!(r.mnemonic, "BRA");
    assert_eq!(r.operands, "$0FF2");
    assert_eq!(r.target_addr, Some(0x0FF2));
}

#[test]
fn test_bra_self() {
    // BRA offset -2 (0xFE) → branch to self
    let r = dis_at(0x1000, &[0x20, 0xFE]);
    assert_eq!(r.operands, "$1000");
    assert_eq!(r.target_addr, Some(0x1000));
}

#[test]
fn test_conditional_branches() {
    let cases: &[(u8, &str)] = &[
        (0x22, "BHI"),
        (0x23, "BLS"),
        (0x24, "BCC"),
        (0x25, "BCS"),
        (0x26, "BNE"),
        (0x27, "BEQ"),
        (0x28, "BVC"),
        (0x29, "BVS"),
        (0x2A, "BPL"),
        (0x2B, "BMI"),
        (0x2C, "BGE"),
        (0x2D, "BLT"),
        (0x2E, "BGT"),
        (0x2F, "BLE"),
    ];
    for &(opcode, mnemonic) in cases {
        let r = dis_at(0x0100, &[opcode, 0x20]);
        assert_eq!(r.mnemonic, mnemonic, "opcode 0x{:02X}", opcode);
        assert_eq!(r.operands, "$0122");
        assert_eq!(r.byte_len, 2);
        assert_eq!(r.target_addr, Some(0x0122));
    }
}

#[test]
fn test_bsr() {
    // BSR at $2000, offset +$50 → target $2052
    let r = dis_at(0x2000, &[0x8D, 0x50]);
    assert_eq!(r.mnemonic, "BSR");
    assert_eq!(r.operands, "$2052");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x2052));
}

#[test]
fn test_bsr_backward() {
    // BSR at $2000, offset -$80 (0x80) → target $1F82
    let r = dis_at(0x2000, &[0x8D, 0x80]);
    assert_eq!(r.mnemonic, "BSR");
    assert_eq!(r.operands, "$1F82");
    assert_eq!(r.target_addr, Some(0x1F82));
}

// =============================================================================
// Relative branch wrapping around address space
// =============================================================================

#[test]
fn test_branch_wrap_forward() {
    // BRA at $FFFE, offset +$10 → wraps to $0010
    let r = dis_at(0xFFFE, &[0x20, 0x10]);
    assert_eq!(r.operands, "$0010");
    assert_eq!(r.target_addr, Some(0x0010));
}

#[test]
fn test_branch_wrap_backward() {
    // BRA at $0000, offset -$10 (0xF0) → wraps to $FFF2
    let r = dis_at(0x0000, &[0x20, 0xF0]);
    assert_eq!(r.operands, "$FFF2");
    assert_eq!(r.target_addr, Some(0xFFF2));
}

// =============================================================================
// Illegal opcodes
// =============================================================================

#[test]
fn test_illegal_opcode_0x00() {
    let r = dis(&[0x00]);
    assert_eq!(r.mnemonic, "???");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_illegal_opcode_0x83() {
    let r = dis(&[0x83]);
    assert_eq!(r.mnemonic, "???");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_illegal_opcode_0x87() {
    let r = dis(&[0x87]);
    assert_eq!(r.mnemonic, "???");
    assert_eq!(r.byte_len, 1);
}

// =============================================================================
// Edge cases
// =============================================================================

#[test]
fn test_empty_bytes() {
    let r = M6800::disassemble(0x0000, &[]);
    assert_eq!(r.mnemonic, "???");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_truncated_2byte_imm() {
    // LDAA immediate needs 2 bytes, only 1 provided
    let r = dis(&[0x86]);
    assert_eq!(r.mnemonic, "???");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_truncated_3byte_ext() {
    // LDAA extended needs 3 bytes, only 2 provided
    let r = dis(&[0xB6, 0x12]);
    assert_eq!(r.mnemonic, "???");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_truncated_3byte_imm16() {
    // CPX immediate needs 3 bytes, only 1 provided
    let r = dis(&[0x8C]);
    assert_eq!(r.mnemonic, "???");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_raw_bytes_captured() {
    let r = dis(&[0xB6, 0x12, 0x34, 0xFF]);
    assert_eq!(r.bytes[0], 0xB6);
    assert_eq!(r.bytes[1], 0x12);
    assert_eq!(r.bytes[2], 0x34);
    assert_eq!(r.byte_len, 3);
}

// =============================================================================
// Display formatting
// =============================================================================

#[test]
fn test_display_inherent() {
    let r = dis(&[0x01]);
    assert_eq!(format!("{}", r), "NOP");
}

#[test]
fn test_display_inherent_acc() {
    let r = dis(&[0x40]);
    assert_eq!(format!("{}", r), "NEGA");
}

#[test]
fn test_display_imm8() {
    let r = dis(&[0x86, 0x42]);
    assert_eq!(format!("{}", r), "LDAA  #$42");
}

#[test]
fn test_display_imm16() {
    let r = dis(&[0x8C, 0x12, 0x34]);
    assert_eq!(format!("{}", r), "CPX   #$1234");
}

#[test]
fn test_display_dir() {
    let r = dis(&[0x96, 0x42]);
    assert_eq!(format!("{}", r), "LDAA  $42");
}

#[test]
fn test_display_idx() {
    let r = dis(&[0xA6, 0x05]);
    assert_eq!(format!("{}", r), "LDAA  $05,X");
}

#[test]
fn test_display_ext() {
    let r = dis(&[0xB6, 0x12, 0x34]);
    assert_eq!(format!("{}", r), "LDAA  $1234");
}

#[test]
fn test_display_rel() {
    let r = dis_at(0x1000, &[0x20, 0x10]);
    assert_eq!(format!("{}", r), "BRA   $1012");
}

// =============================================================================
// Symbol resolution
// =============================================================================

#[test]
fn test_symbols_branch_match() {
    let r = dis_at(0x1000, &[0x20, 0x10]);
    let output = r.format_with_symbols(|addr| if addr == 0x1012 { Some("loop") } else { None });
    assert_eq!(output, "BRA   loop");
}

#[test]
fn test_symbols_ext_match() {
    let r = dis(&[0xB6, 0xCC, 0x00]);
    let output = r.format_with_symbols(|addr| {
        if addr == 0xCC00 {
            Some("PIA_0_A")
        } else {
            None
        }
    });
    assert_eq!(output, "LDAA  PIA_0_A");
}

#[test]
fn test_symbols_dir_match() {
    let r = dis(&[0x96, 0x80]);
    let output = r.format_with_symbols(|addr| {
        if addr == 0x0080 {
            Some("counter")
        } else {
            None
        }
    });
    assert_eq!(output, "LDAA  counter");
}

#[test]
fn test_symbols_no_match() {
    let r = dis(&[0xB6, 0x12, 0x34]);
    let output = r.format_with_symbols(|_| None);
    assert_eq!(output, "LDAA  $1234");
}

#[test]
fn test_symbols_no_target() {
    let r = dis(&[0x01]);
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
        let r = M6800::disassemble(0x0000, &buf);
        assert!(
            r.byte_len == 1 || r.byte_len == 2 || r.byte_len == 3,
            "opcode 0x{:02X}: unexpected byte_len {}",
            opcode,
            r.byte_len
        );
    }
}

#[test]
fn test_implemented_opcodes_decode_correctly() {
    // Every opcode that the M6800 CPU actually implements should decode
    // to a known (non-"???") mnemonic.
    let implemented: &[u8] = &[
        0x01, // NOP
        0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, // Transfer/flag
        0x10, 0x11, 0x16, 0x17, 0x19, 0x1B, // Acc arithmetic
        0x20, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E,
        0x2F, // Branches
        0x30, 0x31, 0x32, 0x33, 0x34, 0x35, 0x36, 0x37, 0x39, 0x3B, 0x3E, 0x3F, // Stack/ctrl
        0x40, 0x43, 0x44, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x4C, 0x4D, 0x4F, // Acc A unary
        0x50, 0x53, 0x54, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x5C, 0x5D, 0x5F, // Acc B unary
        0x60, 0x63, 0x64, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x6C, 0x6D, 0x6E, 0x6F, // Indexed RMW
        0x70, 0x73, 0x74, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x7C, 0x7D, 0x7E,
        0x7F, // Extended RMW
        0x80, 0x81, 0x82, 0x84, 0x85, 0x86, 0x88, 0x89, 0x8A, 0x8B, 0x8C, 0x8D,
        0x8E, // A imm + 16-bit imm
        0x90, 0x91, 0x92, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0x9B, 0x9C, 0x9E,
        0x9F, // A dir + 16-bit dir
        0xA0, 0xA1, 0xA2, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD, 0xAE,
        0xAF, // A idx + 16-bit idx
        0xB0, 0xB1, 0xB2, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xBB, 0xBC, 0xBD, 0xBE,
        0xBF, // A ext + 16-bit ext
        0xC0, 0xC1, 0xC2, 0xC4, 0xC5, 0xC6, 0xC8, 0xC9, 0xCA, 0xCB, 0xCE, // B imm + LDX imm
        0xD0, 0xD1, 0xD2, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xDB, 0xDE, 0xDF, // B dir
        0xE0, 0xE1, 0xE2, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xEB, 0xEE, 0xEF, // B idx
        0xF0, 0xF1, 0xF2, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA, 0xFB, 0xFE, 0xFF, // B ext
    ];

    let mut buf = [0u8; 3];
    for &opcode in implemented {
        buf[0] = opcode;
        buf[1] = 0x00;
        buf[2] = 0x00;
        let r = M6800::disassemble(0x0000, &buf);
        assert_ne!(
            r.mnemonic, "???",
            "opcode 0x{:02X} should decode to a known mnemonic",
            opcode
        );
    }
}
