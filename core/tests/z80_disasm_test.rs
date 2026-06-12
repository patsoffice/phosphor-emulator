//! Z80 disassembler tests.

use phosphor_core::cpu::disasm::{Disassemble, DisassembledInstruction};
use phosphor_core::cpu::z80::Z80;

fn dis(addr: u32, bytes: &[u8]) -> DisassembledInstruction {
    Z80::disassemble(addr, bytes)
}

// ── Unprefixed: misc / control ───────────────────────────────────────────────

#[test]
fn nop() {
    let r = dis(0, &[0x00]);
    assert_eq!(r.mnemonic, "NOP");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn halt() {
    let r = dis(0, &[0x76]);
    assert_eq!(r.mnemonic, "HALT");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn di_ei() {
    assert_eq!(dis(0, &[0xF3]).mnemonic, "DI");
    assert_eq!(dis(0, &[0xFB]).mnemonic, "EI");
}

#[test]
fn ex_af_af_prime() {
    let r = dis(0, &[0x08]);
    assert_eq!(r.mnemonic, "EX");
    assert_eq!(r.operands, "AF,AF'");
}

#[test]
fn ex_de_hl() {
    let r = dis(0, &[0xEB]);
    assert_eq!(format!("{}", r), "EX    DE,HL");
}

#[test]
fn exx() {
    assert_eq!(dis(0, &[0xD9]).mnemonic, "EXX");
}

#[test]
fn ex_sp_hl() {
    let r = dis(0, &[0xE3]);
    assert_eq!(r.operands, "(SP),HL");
}

// ── Accumulator rotates & misc ───────────────────────────────────────────────

#[test]
fn accumulator_rotates() {
    assert_eq!(dis(0, &[0x07]).mnemonic, "RLCA");
    assert_eq!(dis(0, &[0x0F]).mnemonic, "RRCA");
    assert_eq!(dis(0, &[0x17]).mnemonic, "RLA");
    assert_eq!(dis(0, &[0x1F]).mnemonic, "RRA");
}

#[test]
fn daa_cpl_scf_ccf() {
    assert_eq!(dis(0, &[0x27]).mnemonic, "DAA");
    assert_eq!(dis(0, &[0x2F]).mnemonic, "CPL");
    assert_eq!(dis(0, &[0x37]).mnemonic, "SCF");
    assert_eq!(dis(0, &[0x3F]).mnemonic, "CCF");
}

// ── 8-bit loads ──────────────────────────────────────────────────────────────

#[test]
fn ld_r_r() {
    // LD B,C = 0x41
    let r = dis(0, &[0x41]);
    assert_eq!(format!("{}", r), "LD    B,C");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn ld_r_n() {
    // LD B,0x42 = 0x06 0x42
    let r = dis(0, &[0x06, 0x42]);
    assert_eq!(format!("{}", r), "LD    B,$42");
    assert_eq!(r.byte_len, 2);
}

#[test]
fn ld_r_hl() {
    // LD A,(HL) = 0x7E
    let r = dis(0, &[0x7E]);
    assert_eq!(r.operands, "A,(HL)");
}

#[test]
fn ld_hl_r() {
    // LD (HL),B = 0x70
    let r = dis(0, &[0x70]);
    assert_eq!(r.operands, "(HL),B");
}

#[test]
fn ld_hl_n() {
    // LD (HL),n = 0x36 0x55
    let r = dis(0, &[0x36, 0x55]);
    assert_eq!(r.operands, "(HL),$55");
}

#[test]
fn ld_a_bc_de() {
    let r = dis(0, &[0x0A]);
    assert_eq!(r.operands, "A,(BC)");
    let r = dis(0, &[0x1A]);
    assert_eq!(r.operands, "A,(DE)");
}

#[test]
fn ld_bc_de_a() {
    let r = dis(0, &[0x02]);
    assert_eq!(r.operands, "(BC),A");
    let r = dis(0, &[0x12]);
    assert_eq!(r.operands, "(DE),A");
}

#[test]
fn ld_a_nn_indirect() {
    // LD A,(nn) = 0x3A lo hi
    let r = dis(0, &[0x3A, 0x34, 0x12]);
    assert_eq!(r.operands, "A,($1234)");
    assert_eq!(r.target_addr, Some(0x1234));
    assert_eq!(r.byte_len, 3);
}

#[test]
fn ld_nn_a() {
    // LD (nn),A = 0x32 lo hi
    let r = dis(0, &[0x32, 0x00, 0x80]);
    assert_eq!(r.operands, "($8000),A");
    assert_eq!(r.target_addr, Some(0x8000));
}

// ── 16-bit loads ─────────────────────────────────────────────────────────────

#[test]
fn ld_rr_nn() {
    // LD BC,1234 = 0x01 0x34 0x12
    let r = dis(0, &[0x01, 0x34, 0x12]);
    assert_eq!(r.operands, "BC,$1234");
    assert_eq!(r.byte_len, 3);

    // LD SP,FFFF
    let r = dis(0, &[0x31, 0xFF, 0xFF]);
    assert_eq!(r.operands, "SP,$FFFF");
}

#[test]
fn ld_hl_nn_indirect() {
    // LD HL,(nn) = 0x2A lo hi
    let r = dis(0, &[0x2A, 0x00, 0x40]);
    assert_eq!(r.operands, "HL,($4000)");
    assert_eq!(r.target_addr, Some(0x4000));
}

#[test]
fn ld_nn_hl() {
    // LD (nn),HL = 0x22 lo hi
    let r = dis(0, &[0x22, 0x00, 0x50]);
    assert_eq!(r.operands, "($5000),HL");
}

#[test]
fn ld_sp_hl() {
    let r = dis(0, &[0xF9]);
    assert_eq!(format!("{}", r), "LD    SP,HL");
}

// ── Stack ────────────────────────────────────────────────────────────────────

#[test]
fn push_pop() {
    assert_eq!(dis(0, &[0xC5]).operands, "BC");
    assert_eq!(dis(0, &[0xC5]).mnemonic, "PUSH");
    assert_eq!(dis(0, &[0xD1]).operands, "DE");
    assert_eq!(dis(0, &[0xD1]).mnemonic, "POP");
    assert_eq!(dis(0, &[0xF5]).operands, "AF");
    assert_eq!(dis(0, &[0xE1]).operands, "HL");
}

// ── 16-bit arithmetic ────────────────────────────────────────────────────────

#[test]
fn inc_dec_rr() {
    assert_eq!(format!("{}", dis(0, &[0x03])), "INC   BC");
    assert_eq!(format!("{}", dis(0, &[0x0B])), "DEC   BC");
    assert_eq!(format!("{}", dis(0, &[0x23])), "INC   HL");
    assert_eq!(format!("{}", dis(0, &[0x33])), "INC   SP");
}

#[test]
fn add_hl_rr() {
    let r = dis(0, &[0x09]);
    assert_eq!(r.operands, "HL,BC");
    assert_eq!(r.mnemonic, "ADD");
    let r = dis(0, &[0x29]);
    assert_eq!(r.operands, "HL,HL");
}

// ── 8-bit INC/DEC ────────────────────────────────────────────────────────────

#[test]
fn inc_dec_r() {
    assert_eq!(format!("{}", dis(0, &[0x04])), "INC   B");
    assert_eq!(format!("{}", dis(0, &[0x0D])), "DEC   C");
    assert_eq!(format!("{}", dis(0, &[0x34])), "INC   (HL)");
    assert_eq!(format!("{}", dis(0, &[0x3D])), "DEC   A");
}

// ── ALU A,r / A,n ────────────────────────────────────────────────────────────

#[test]
fn alu_a_r() {
    // ADD A,B = 0x80
    assert_eq!(format!("{}", dis(0, &[0x80])), "ADD   A,B");
    // ADC A,C = 0x89
    assert_eq!(format!("{}", dis(0, &[0x89])), "ADC   A,C");
    // SUB D = 0x92
    assert_eq!(format!("{}", dis(0, &[0x92])), "SUB   D");
    // SBC E = 0x9B
    assert_eq!(format!("{}", dis(0, &[0x9B])), "SBC   E");
    // AND H = 0xA4
    assert_eq!(format!("{}", dis(0, &[0xA4])), "AND   H");
    // XOR L = 0xAD
    assert_eq!(format!("{}", dis(0, &[0xAD])), "XOR   L");
    // OR (HL) = 0xB6
    assert_eq!(format!("{}", dis(0, &[0xB6])), "OR    (HL)");
    // CP A = 0xBF
    assert_eq!(format!("{}", dis(0, &[0xBF])), "CP    A");
}

#[test]
fn alu_a_n() {
    // ADD A,$42
    assert_eq!(format!("{}", dis(0, &[0xC6, 0x42])), "ADD   A,$42");
    // SUB $10
    assert_eq!(format!("{}", dis(0, &[0xD6, 0x10])), "SUB   $10");
    // CP $FF
    assert_eq!(format!("{}", dis(0, &[0xFE, 0xFF])), "CP    $FF");
    // AND $0F
    assert_eq!(format!("{}", dis(0, &[0xE6, 0x0F])), "AND   $0F");
}

// ── Branches / jumps / calls ─────────────────────────────────────────────────

#[test]
fn jp_nn() {
    let r = dis(0, &[0xC3, 0x34, 0x12]);
    assert_eq!(r.mnemonic, "JP");
    assert_eq!(r.operands, "$1234");
    assert_eq!(r.target_addr, Some(0x1234));
    assert_eq!(r.byte_len, 3);
}

#[test]
fn jp_cc_nn() {
    let r = dis(0, &[0xC2, 0x00, 0x80]);
    assert_eq!(r.operands, "NZ,$8000");
    assert_eq!(r.target_addr, Some(0x8000));

    let r = dis(0, &[0xCA, 0x00, 0x80]);
    assert_eq!(r.operands, "Z,$8000");
}

#[test]
fn jp_hl() {
    let r = dis(0, &[0xE9]);
    assert_eq!(format!("{}", r), "JP    (HL)");
}

#[test]
fn jr_e() {
    // JR +5 from address 0x100: target = 0x100 + 2 + 5 = 0x107
    let r = dis(0x100, &[0x18, 0x05]);
    assert_eq!(r.mnemonic, "JR");
    assert_eq!(r.target_addr, Some(0x0107));
    assert_eq!(r.byte_len, 2);
}

#[test]
fn jr_backward() {
    // JR -10 from address 0x100: target = 0x100 + 2 + (-10) = 0xF8
    let r = dis(0x100, &[0x18, 0xF6]);
    assert_eq!(r.target_addr, Some(0x00F8));
}

#[test]
fn jr_cc_e() {
    // JR NZ,+0 from 0x200
    let r = dis(0x200, &[0x20, 0x00]);
    assert_eq!(r.operands, "NZ,$0202");
    assert_eq!(r.target_addr, Some(0x0202));

    // JR Z
    let r = dis(0x200, &[0x28, 0x10]);
    assert_eq!(r.operands, "Z,$0212");

    // JR NC
    let r = dis(0x200, &[0x30, 0xFE]);
    assert_eq!(r.operands, "NC,$0200");

    // JR C
    let r = dis(0, &[0x38, 0x05]);
    assert_eq!(r.operands, "C,$0007");
}

#[test]
fn djnz() {
    let r = dis(0x300, &[0x10, 0xFE]);
    assert_eq!(r.mnemonic, "DJNZ");
    assert_eq!(r.target_addr, Some(0x0300));
}

#[test]
fn call_nn() {
    let r = dis(0, &[0xCD, 0x00, 0x10]);
    assert_eq!(r.mnemonic, "CALL");
    assert_eq!(r.operands, "$1000");
    assert_eq!(r.target_addr, Some(0x1000));
}

#[test]
fn call_cc_nn() {
    let r = dis(0, &[0xC4, 0x00, 0x20]);
    assert_eq!(r.operands, "NZ,$2000");

    let r = dis(0, &[0xFC, 0x00, 0x30]);
    assert_eq!(r.operands, "M,$3000");
}

#[test]
fn ret() {
    let r = dis(0, &[0xC9]);
    assert_eq!(r.mnemonic, "RET");
    assert_eq!(r.operands, "");
}

#[test]
fn ret_cc() {
    let r = dis(0, &[0xC0]);
    assert_eq!(r.mnemonic, "RET");
    assert_eq!(r.operands, "NZ");

    let r = dis(0, &[0xF8]);
    assert_eq!(r.operands, "M");
}

#[test]
fn rst() {
    let r = dis(0, &[0xC7]);
    assert_eq!(r.mnemonic, "RST");
    assert_eq!(r.operands, "$00");
    assert_eq!(r.target_addr, Some(0x0000));

    let r = dis(0, &[0xFF]);
    assert_eq!(r.operands, "$38");
    assert_eq!(r.target_addr, Some(0x0038));
}

// ── I/O ──────────────────────────────────────────────────────────────────────

#[test]
fn in_out_n() {
    let r = dis(0, &[0xDB, 0xFE]);
    assert_eq!(format!("{}", r), "IN    A,($FE)");

    let r = dis(0, &[0xD3, 0x01]);
    assert_eq!(format!("{}", r), "OUT   ($01),A");
}

// ── CB prefix: rotate/shift/bit ──────────────────────────────────────────────

#[test]
fn cb_rotate_shift() {
    // RLC B = CB 00
    assert_eq!(format!("{}", dis(0, &[0xCB, 0x00])), "RLC   B");
    // RRC C = CB 09
    assert_eq!(format!("{}", dis(0, &[0xCB, 0x09])), "RRC   C");
    // RL D = CB 12
    assert_eq!(format!("{}", dis(0, &[0xCB, 0x12])), "RL    D");
    // RR E = CB 1B
    assert_eq!(format!("{}", dis(0, &[0xCB, 0x1B])), "RR    E");
    // SLA H = CB 24
    assert_eq!(format!("{}", dis(0, &[0xCB, 0x24])), "SLA   H");
    // SRA L = CB 2D
    assert_eq!(format!("{}", dis(0, &[0xCB, 0x2D])), "SRA   L");
    // SLL A = CB 37 (undocumented)
    assert_eq!(format!("{}", dis(0, &[0xCB, 0x37])), "SLL   A");
    // SRL (HL) = CB 3E
    assert_eq!(format!("{}", dis(0, &[0xCB, 0x3E])), "SRL   (HL)");
}

#[test]
fn cb_bit() {
    // BIT 0,B = CB 40
    assert_eq!(format!("{}", dis(0, &[0xCB, 0x40])), "BIT   0,B");
    // BIT 7,A = CB 7F
    assert_eq!(format!("{}", dis(0, &[0xCB, 0x7F])), "BIT   7,A");
    // BIT 3,(HL) = CB 5E
    assert_eq!(format!("{}", dis(0, &[0xCB, 0x5E])), "BIT   3,(HL)");
}

#[test]
fn cb_res_set() {
    // RES 0,B = CB 80
    assert_eq!(format!("{}", dis(0, &[0xCB, 0x80])), "RES   0,B");
    // SET 7,A = CB FF
    assert_eq!(format!("{}", dis(0, &[0xCB, 0xFF])), "SET   7,A");
}

#[test]
fn cb_byte_len() {
    let r = dis(0, &[0xCB, 0x00]);
    assert_eq!(r.byte_len, 2);
}

// ── ED prefix ────────────────────────────────────────────────────────────────

#[test]
fn ed_in_out_c() {
    // IN B,(C) = ED 40
    assert_eq!(format!("{}", dis(0, &[0xED, 0x40])), "IN    B,(C)");
    // IN A,(C) = ED 78
    assert_eq!(format!("{}", dis(0, &[0xED, 0x78])), "IN    A,(C)");
    // IN (C) = ED 70 (undocumented: flags only)
    assert_eq!(format!("{}", dis(0, &[0xED, 0x70])), "IN    (C)");

    // OUT (C),B = ED 41
    assert_eq!(format!("{}", dis(0, &[0xED, 0x41])), "OUT   (C),B");
    // OUT (C),0 = ED 71 (undocumented)
    assert_eq!(format!("{}", dis(0, &[0xED, 0x71])), "OUT   (C),0");
}

#[test]
fn ed_sbc_adc_hl() {
    // SBC HL,BC = ED 42
    assert_eq!(format!("{}", dis(0, &[0xED, 0x42])), "SBC   HL,BC");
    // ADC HL,DE = ED 5A
    assert_eq!(format!("{}", dis(0, &[0xED, 0x5A])), "ADC   HL,DE");
    // SBC HL,SP = ED 72
    assert_eq!(format!("{}", dis(0, &[0xED, 0x72])), "SBC   HL,SP");
}

#[test]
fn ed_ld_nn_rr() {
    // LD (1234),BC = ED 43 34 12
    let r = dis(0, &[0xED, 0x43, 0x34, 0x12]);
    assert_eq!(r.operands, "($1234),BC");
    assert_eq!(r.target_addr, Some(0x1234));
    assert_eq!(r.byte_len, 4);

    // LD SP,(5678) = ED 7B 78 56
    let r = dis(0, &[0xED, 0x7B, 0x78, 0x56]);
    assert_eq!(r.operands, "SP,($5678)");
    assert_eq!(r.target_addr, Some(0x5678));
}

#[test]
fn ed_neg() {
    assert_eq!(dis(0, &[0xED, 0x44]).mnemonic, "NEG");
    // Undocumented mirrors
    assert_eq!(dis(0, &[0xED, 0x4C]).mnemonic, "NEG");
    assert_eq!(dis(0, &[0xED, 0x54]).mnemonic, "NEG");
}

#[test]
fn ed_retn_reti() {
    assert_eq!(dis(0, &[0xED, 0x45]).mnemonic, "RETN");
    assert_eq!(dis(0, &[0xED, 0x4D]).mnemonic, "RETI");
    // Undocumented mirrors
    assert_eq!(dis(0, &[0xED, 0x55]).mnemonic, "RETN");
}

#[test]
fn ed_im() {
    assert_eq!(dis(0, &[0xED, 0x46]).operands, "0");
    assert_eq!(dis(0, &[0xED, 0x56]).operands, "1");
    assert_eq!(dis(0, &[0xED, 0x5E]).operands, "2");
}

#[test]
fn ed_ld_i_r() {
    assert_eq!(dis(0, &[0xED, 0x47]).operands, "I,A");
    assert_eq!(dis(0, &[0xED, 0x4F]).operands, "R,A");
    assert_eq!(dis(0, &[0xED, 0x57]).operands, "A,I");
    assert_eq!(dis(0, &[0xED, 0x5F]).operands, "A,R");
}

#[test]
fn ed_rrd_rld() {
    assert_eq!(dis(0, &[0xED, 0x67]).mnemonic, "RRD");
    assert_eq!(dis(0, &[0xED, 0x6F]).mnemonic, "RLD");
}

#[test]
fn ed_block_ops() {
    assert_eq!(dis(0, &[0xED, 0xA0]).mnemonic, "LDI");
    assert_eq!(dis(0, &[0xED, 0xA1]).mnemonic, "CPI");
    assert_eq!(dis(0, &[0xED, 0xA2]).mnemonic, "INI");
    assert_eq!(dis(0, &[0xED, 0xA3]).mnemonic, "OUTI");
    assert_eq!(dis(0, &[0xED, 0xA8]).mnemonic, "LDD");
    assert_eq!(dis(0, &[0xED, 0xA9]).mnemonic, "CPD");
    assert_eq!(dis(0, &[0xED, 0xAA]).mnemonic, "IND");
    assert_eq!(dis(0, &[0xED, 0xAB]).mnemonic, "OUTD");
    assert_eq!(dis(0, &[0xED, 0xB0]).mnemonic, "LDIR");
    assert_eq!(dis(0, &[0xED, 0xB1]).mnemonic, "CPIR");
    assert_eq!(dis(0, &[0xED, 0xB2]).mnemonic, "INIR");
    assert_eq!(dis(0, &[0xED, 0xB3]).mnemonic, "OTIR");
    assert_eq!(dis(0, &[0xED, 0xB8]).mnemonic, "LDDR");
    assert_eq!(dis(0, &[0xED, 0xB9]).mnemonic, "CPDR");
    assert_eq!(dis(0, &[0xED, 0xBA]).mnemonic, "INDR");
    assert_eq!(dis(0, &[0xED, 0xBB]).mnemonic, "OTDR");
}

#[test]
fn ed_nop_undefined() {
    // Undefined ED opcodes act as 2-byte NOP
    let r = dis(0, &[0xED, 0x00]);
    assert_eq!(r.mnemonic, "NOP");
    assert_eq!(r.byte_len, 2);
}

// ── DD prefix (IX) ───────────────────────────────────────────────────────────

#[test]
fn dd_ld_ix_nn() {
    // LD IX,1234 = DD 21 34 12
    let r = dis(0, &[0xDD, 0x21, 0x34, 0x12]);
    assert_eq!(r.operands, "IX,$1234");
    assert_eq!(r.byte_len, 4);
}

#[test]
fn dd_add_ix_rr() {
    // ADD IX,BC = DD 09
    let r = dis(0, &[0xDD, 0x09]);
    assert_eq!(r.operands, "IX,BC");

    // ADD IX,IX = DD 29
    let r = dis(0, &[0xDD, 0x29]);
    assert_eq!(r.operands, "IX,IX");
}

#[test]
fn dd_inc_dec_ix() {
    assert_eq!(format!("{}", dis(0, &[0xDD, 0x23])), "INC   IX");
    assert_eq!(format!("{}", dis(0, &[0xDD, 0x2B])), "DEC   IX");
}

#[test]
fn dd_push_pop_ix() {
    assert_eq!(dis(0, &[0xDD, 0xE5]).operands, "IX");
    assert_eq!(dis(0, &[0xDD, 0xE5]).mnemonic, "PUSH");
    assert_eq!(dis(0, &[0xDD, 0xE1]).operands, "IX");
    assert_eq!(dis(0, &[0xDD, 0xE1]).mnemonic, "POP");
}

#[test]
fn dd_ld_ix_disp_r() {
    // LD (IX+5),B = DD 70 05
    let r = dis(0, &[0xDD, 0x70, 0x05]);
    assert_eq!(r.operands, "(IX+$05),B");
    assert_eq!(r.byte_len, 3);
}

#[test]
fn dd_ld_r_ix_disp() {
    // LD A,(IX+$10) = DD 7E 10
    let r = dis(0, &[0xDD, 0x7E, 0x10]);
    assert_eq!(r.operands, "A,(IX+$10)");
}

#[test]
fn dd_ld_ix_disp_n() {
    // LD (IX+3),42 = DD 36 03 2A
    let r = dis(0, &[0xDD, 0x36, 0x03, 0x2A]);
    assert_eq!(r.operands, "(IX+$03),$2A");
    assert_eq!(r.byte_len, 4);
}

#[test]
fn dd_alu_ix_disp() {
    // ADD A,(IX+2) = DD 86 02
    let r = dis(0, &[0xDD, 0x86, 0x02]);
    assert_eq!(r.operands, "A,(IX+$02)");
    assert_eq!(r.mnemonic, "ADD");

    // CP (IX-1) = DD BE FF
    let r = dis(0, &[0xDD, 0xBE, 0xFF]);
    assert_eq!(r.operands, "(IX-$01)");
    assert_eq!(r.mnemonic, "CP");
}

#[test]
fn dd_inc_dec_ix_disp() {
    // INC (IX+0) = DD 34 00
    let r = dis(0, &[0xDD, 0x34, 0x00]);
    assert_eq!(r.operands, "(IX+$00)");
    assert_eq!(r.mnemonic, "INC");

    // DEC (IX-5) = DD 35 FB
    let r = dis(0, &[0xDD, 0x35, 0xFB]);
    assert_eq!(r.operands, "(IX-$05)");
    assert_eq!(r.mnemonic, "DEC");
}

#[test]
fn dd_ex_sp_ix() {
    let r = dis(0, &[0xDD, 0xE3]);
    assert_eq!(r.operands, "(SP),IX");
}

#[test]
fn dd_jp_ix() {
    let r = dis(0, &[0xDD, 0xE9]);
    assert_eq!(format!("{}", r), "JP    (IX)");
}

#[test]
fn dd_ld_sp_ix() {
    let r = dis(0, &[0xDD, 0xF9]);
    assert_eq!(r.operands, "SP,IX");
}

#[test]
fn dd_ld_nn_ix() {
    // LD (nn),IX = DD 22 lo hi
    let r = dis(0, &[0xDD, 0x22, 0x00, 0x40]);
    assert_eq!(r.operands, "($4000),IX");
    assert_eq!(r.target_addr, Some(0x4000));
}

#[test]
fn dd_ld_ix_nn_indirect() {
    // LD IX,(nn) = DD 2A lo hi
    let r = dis(0, &[0xDD, 0x2A, 0x00, 0x50]);
    assert_eq!(r.operands, "IX,($5000)");
    assert_eq!(r.target_addr, Some(0x5000));
}

// ── FD prefix (IY) ───────────────────────────────────────────────────────────

#[test]
fn fd_ld_iy_nn() {
    let r = dis(0, &[0xFD, 0x21, 0x78, 0x56]);
    assert_eq!(r.operands, "IY,$5678");
}

#[test]
fn fd_ld_iy_disp_r() {
    let r = dis(0, &[0xFD, 0x77, 0x0A]);
    assert_eq!(r.operands, "(IY+$0A),A");
}

#[test]
fn fd_alu_iy_disp() {
    // SUB (IY+3) = FD 96 03
    let r = dis(0, &[0xFD, 0x96, 0x03]);
    assert_eq!(r.operands, "(IY+$03)");
    assert_eq!(r.mnemonic, "SUB");
}

#[test]
fn fd_push_pop_iy() {
    assert_eq!(dis(0, &[0xFD, 0xE5]).operands, "IY");
    assert_eq!(dis(0, &[0xFD, 0xE1]).operands, "IY");
}

// ── Undocumented IXH/IXL/IYH/IYL ────────────────────────────────────────────

#[test]
fn dd_ixh_ixl() {
    // INC IXH = DD 24
    assert_eq!(format!("{}", dis(0, &[0xDD, 0x24])), "INC   IXH");
    // DEC IXL = DD 2D
    assert_eq!(format!("{}", dis(0, &[0xDD, 0x2D])), "DEC   IXL");
    // LD IXH,n = DD 26 42
    assert_eq!(dis(0, &[0xDD, 0x26, 0x42]).operands, "IXH,$42");
    // LD A,IXL = DD 7D
    assert_eq!(dis(0, &[0xDD, 0x7D]).operands, "A,IXL");
    // LD IXH,IXL = DD 65
    assert_eq!(dis(0, &[0xDD, 0x65]).operands, "IXH,IXL");
    // ADD A,IXH = DD 84
    assert_eq!(dis(0, &[0xDD, 0x84]).operands, "A,IXH");
}

#[test]
fn fd_iyh_iyl() {
    // INC IYH = FD 24
    assert_eq!(format!("{}", dis(0, &[0xFD, 0x24])), "INC   IYH");
    // LD A,IYL = FD 7D
    assert_eq!(dis(0, &[0xFD, 0x7D]).operands, "A,IYL");
    // XOR IYH = FD AC
    assert_eq!(dis(0, &[0xFD, 0xAC]).operands, "IYH");
    assert_eq!(dis(0, &[0xFD, 0xAC]).mnemonic, "XOR");
}

// ── DDCB / FDCB (indexed bit operations) ─────────────────────────────────────

#[test]
fn ddcb_bit() {
    // BIT 0,(IX+5) = DD CB 05 46
    let r = dis(0, &[0xDD, 0xCB, 0x05, 0x46]);
    assert_eq!(r.mnemonic, "BIT");
    assert_eq!(r.operands, "0,(IX+$05)");
    assert_eq!(r.byte_len, 4);
}

#[test]
fn ddcb_res_set() {
    // RES 3,(IX+2) = DD CB 02 9E
    let r = dis(0, &[0xDD, 0xCB, 0x02, 0x9E]);
    assert_eq!(format!("{}", r), "RES   3,(IX+$02)");

    // SET 7,(IX-1) = DD CB FF FE
    let r = dis(0, &[0xDD, 0xCB, 0xFF, 0xFE]);
    assert_eq!(format!("{}", r), "SET   7,(IX-$01)");
}

#[test]
fn ddcb_rotate() {
    // RLC (IX+3) = DD CB 03 06
    let r = dis(0, &[0xDD, 0xCB, 0x03, 0x06]);
    assert_eq!(format!("{}", r), "RLC   (IX+$03)");

    // SRL (IX+0) = DD CB 00 3E
    let r = dis(0, &[0xDD, 0xCB, 0x00, 0x3E]);
    assert_eq!(format!("{}", r), "SRL   (IX+$00)");
}

#[test]
fn ddcb_undoc_writeback() {
    // RLC (IX+1),B = DD CB 01 00 (result also stored in B)
    let r = dis(0, &[0xDD, 0xCB, 0x01, 0x00]);
    assert_eq!(format!("{}", r), "RLC   (IX+$01),B");

    // SET 0,(IX+2),C = DD CB 02 C1
    let r = dis(0, &[0xDD, 0xCB, 0x02, 0xC1]);
    assert_eq!(format!("{}", r), "SET   0,(IX+$02),C");

    // RES 1,(IX+3),D = DD CB 03 8A
    let r = dis(0, &[0xDD, 0xCB, 0x03, 0x8A]);
    assert_eq!(format!("{}", r), "RES   1,(IX+$03),D");
}

#[test]
fn fdcb_bit() {
    // BIT 5,(IY+10) = FD CB 0A 6E
    let r = dis(0, &[0xFD, 0xCB, 0x0A, 0x6E]);
    assert_eq!(r.operands, "5,(IY+$0A)");
}

#[test]
fn fdcb_negative_disp() {
    // SET 0,(IY-3) = FD CB FD C6
    let r = dis(0, &[0xFD, 0xCB, 0xFD, 0xC6]);
    assert_eq!(r.operands, "0,(IY-$03)");
}

// ── Edge cases ───────────────────────────────────────────────────────────────

#[test]
fn empty_bytes() {
    let r = dis(0, &[]);
    assert_eq!(r.mnemonic, "???");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn truncated_cb() {
    let r = dis(0, &[0xCB]);
    assert_eq!(r.mnemonic, "???");
}

#[test]
fn truncated_dd() {
    let r = dis(0, &[0xDD]);
    assert_eq!(r.mnemonic, "???");
}

#[test]
fn truncated_ed() {
    let r = dis(0, &[0xED]);
    assert_eq!(r.mnemonic, "???");
}

#[test]
fn dd_dd_is_nop() {
    // DD DD treated as a NOP prefix (1 byte)
    let r = dis(0, &[0xDD, 0xDD]);
    assert_eq!(r.mnemonic, "NOP");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn dd_fd_is_nop() {
    let r = dis(0, &[0xDD, 0xFD]);
    assert_eq!(r.mnemonic, "NOP");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn dd_ed_is_nop() {
    // DD ED: the DD is ignored, treated as NOP
    let r = dis(0, &[0xDD, 0xED]);
    assert_eq!(r.mnemonic, "NOP");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn wrapping_branch_target() {
    // JR from 0xFFFF with offset +1: wraps to 0x0001
    let r = dis(0xFFFF, &[0x18, 0x01]);
    assert_eq!(r.target_addr, Some(0x0002));
}

// ── Display / format_with_symbols ────────────────────────────────────────────

#[test]
fn display_no_operands() {
    assert_eq!(format!("{}", dis(0, &[0x00])), "NOP");
    assert_eq!(format!("{}", dis(0, &[0x76])), "HALT");
}

#[test]
fn display_with_operands() {
    let r = dis(0, &[0xC3, 0x34, 0x12]);
    assert_eq!(format!("{}", r), "JP    $1234");
}

#[test]
fn format_with_symbols_4digit() {
    let r = dis(0, &[0xCD, 0x00, 0x10]);
    let s = r.format_with_symbols(|a| if a == 0x1000 { Some("INIT") } else { None });
    assert_eq!(s, "CALL  INIT");
}

#[test]
fn format_with_symbols_2digit() {
    let r = dis(0, &[0xFF]); // RST $38
    let s = r.format_with_symbols(|a| {
        if a == 0x0038 {
            Some("RST38_HANDLER")
        } else {
            None
        }
    });
    assert_eq!(s, "RST   RST38_HANDLER");
}

// ── Bytes stored correctly ───────────────────────────────────────────────────

#[test]
fn bytes_1byte() {
    let r = dis(0, &[0x00, 0xFF, 0xFF]);
    assert_eq!(r.bytes[0], 0x00);
    assert_eq!(r.byte_len, 1);
}

#[test]
fn bytes_3byte() {
    let r = dis(0, &[0xC3, 0x34, 0x12]);
    assert_eq!(r.bytes[..3], [0xC3, 0x34, 0x12]);
    assert_eq!(r.byte_len, 3);
}

#[test]
fn bytes_4byte_ddcb() {
    let r = dis(0, &[0xDD, 0xCB, 0x05, 0x46]);
    assert_eq!(r.bytes[..4], [0xDD, 0xCB, 0x05, 0x46]);
    assert_eq!(r.byte_len, 4);
}
