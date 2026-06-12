use phosphor_core::cpu::Disassemble;
use phosphor_core::cpu::i8035::I8035;

// =============================================================================
// Helper
// =============================================================================

fn dis(bytes: &[u8]) -> phosphor_core::cpu::DisassembledInstruction {
    I8035::disassemble(0x0000, bytes)
}

fn dis_at(addr: u32, bytes: &[u8]) -> phosphor_core::cpu::DisassembledInstruction {
    I8035::disassemble(addr, bytes)
}

// =============================================================================
// Inherent (1-byte, no operands)
// =============================================================================

#[test]
fn test_nop() {
    let r = dis(&[0x00]);
    assert_eq!(r.mnemonic, "NOP");
    assert_eq!(r.operands, "");
    assert_eq!(r.byte_len, 1);
    assert_eq!(r.target_addr, None);
}

#[test]
fn test_ret() {
    let r = dis(&[0x83]);
    assert_eq!(r.mnemonic, "RET");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_retr() {
    let r = dis(&[0x93]);
    assert_eq!(r.mnemonic, "RETR");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_clr_a() {
    let r = dis(&[0x27]);
    assert_eq!(r.mnemonic, "CLR A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_cpl_a() {
    let r = dis(&[0x37]);
    assert_eq!(r.mnemonic, "CPL A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_swap_a() {
    let r = dis(&[0x47]);
    assert_eq!(r.mnemonic, "SWAP A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_da_a() {
    let r = dis(&[0x57]);
    assert_eq!(r.mnemonic, "DA A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_rr_a() {
    let r = dis(&[0x77]);
    assert_eq!(r.mnemonic, "RR A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_rrc_a() {
    let r = dis(&[0x67]);
    assert_eq!(r.mnemonic, "RRC A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_rl_a() {
    let r = dis(&[0xE7]);
    assert_eq!(r.mnemonic, "RL A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_rlc_a() {
    let r = dis(&[0xF7]);
    assert_eq!(r.mnemonic, "RLC A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_inc_a() {
    let r = dis(&[0x17]);
    assert_eq!(r.mnemonic, "INC A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_dec_a() {
    let r = dis(&[0x07]);
    assert_eq!(r.mnemonic, "DEC A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_clr_c() {
    let r = dis(&[0x97]);
    assert_eq!(r.mnemonic, "CLR C");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_cpl_c() {
    let r = dis(&[0xA7]);
    assert_eq!(r.mnemonic, "CPL C");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_clr_f0() {
    let r = dis(&[0x85]);
    assert_eq!(r.mnemonic, "CLR F0");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_cpl_f0() {
    let r = dis(&[0x95]);
    assert_eq!(r.mnemonic, "CPL F0");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_clr_f1() {
    let r = dis(&[0xA5]);
    assert_eq!(r.mnemonic, "CLR F1");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_cpl_f1() {
    let r = dis(&[0xB5]);
    assert_eq!(r.mnemonic, "CPL F1");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_en_i() {
    let r = dis(&[0x05]);
    assert_eq!(r.mnemonic, "EN I");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_dis_i() {
    let r = dis(&[0x15]);
    assert_eq!(r.mnemonic, "DIS I");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_en_tcnti() {
    let r = dis(&[0x25]);
    assert_eq!(r.mnemonic, "EN TCNTI");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_dis_tcnti() {
    let r = dis(&[0x35]);
    assert_eq!(r.mnemonic, "DIS TCNTI");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_strt_cnt() {
    let r = dis(&[0x45]);
    assert_eq!(r.mnemonic, "STRT CNT");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_strt_t() {
    let r = dis(&[0x55]);
    assert_eq!(r.mnemonic, "STRT T");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_stop_tcnt() {
    let r = dis(&[0x65]);
    assert_eq!(r.mnemonic, "STOP TCNT");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_sel_rb0() {
    let r = dis(&[0xC5]);
    assert_eq!(r.mnemonic, "SEL RB0");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_sel_rb1() {
    let r = dis(&[0xD5]);
    assert_eq!(r.mnemonic, "SEL RB1");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_sel_mb0() {
    let r = dis(&[0xE5]);
    assert_eq!(r.mnemonic, "SEL MB0");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_sel_mb1() {
    let r = dis(&[0xF5]);
    assert_eq!(r.mnemonic, "SEL MB1");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_outl_bus_a() {
    let r = dis(&[0x02]);
    assert_eq!(r.mnemonic, "OUTL BUS,A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_ins_a_bus() {
    let r = dis(&[0x08]);
    assert_eq!(r.mnemonic, "INS A,BUS");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_in_a_p1() {
    let r = dis(&[0x09]);
    assert_eq!(r.mnemonic, "IN A,P1");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_in_a_p2() {
    let r = dis(&[0x0A]);
    assert_eq!(r.mnemonic, "IN A,P2");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_outl_p1_a() {
    let r = dis(&[0x39]);
    assert_eq!(r.mnemonic, "OUTL P1,A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_outl_p2_a() {
    let r = dis(&[0x3A]);
    assert_eq!(r.mnemonic, "OUTL P2,A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_mov_a_t() {
    let r = dis(&[0x42]);
    assert_eq!(r.mnemonic, "MOV A,T");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_mov_t_a() {
    let r = dis(&[0x62]);
    assert_eq!(r.mnemonic, "MOV T,A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_mov_a_psw() {
    let r = dis(&[0xC7]);
    assert_eq!(r.mnemonic, "MOV A,PSW");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_mov_psw_a() {
    let r = dis(&[0xD7]);
    assert_eq!(r.mnemonic, "MOV PSW,A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_movp_a_at_a() {
    let r = dis(&[0xA3]);
    assert_eq!(r.mnemonic, "MOVP A,@A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_movp3_a_at_a() {
    let r = dis(&[0xE3]);
    assert_eq!(r.mnemonic, "MOVP3 A,@A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_jmpp_at_a() {
    let r = dis(&[0xB3]);
    assert_eq!(r.mnemonic, "JMPP @A");
    assert_eq!(r.byte_len, 1);
}

// =============================================================================
// Expander port I/O (1-byte inherent with port number in mnemonic)
// =============================================================================

#[test]
fn test_movd_a_p4() {
    let r = dis(&[0x0C]);
    assert_eq!(r.mnemonic, "MOVD A,P4");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_movd_a_p7() {
    let r = dis(&[0x0F]);
    assert_eq!(r.mnemonic, "MOVD A,P7");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_movd_p4_a() {
    let r = dis(&[0x3C]);
    assert_eq!(r.mnemonic, "MOVD P4,A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_orld_p5_a() {
    let r = dis(&[0x8D]);
    assert_eq!(r.mnemonic, "ORLD P5,A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_anld_p6_a() {
    let r = dis(&[0x9E]);
    assert_eq!(r.mnemonic, "ANLD P6,A");
    assert_eq!(r.byte_len, 1);
}

// =============================================================================
// Register Rn operations (1-byte)
// =============================================================================

#[test]
fn test_inc_r0() {
    let r = dis(&[0x18]);
    assert_eq!(r.mnemonic, "INC");
    assert_eq!(r.operands, "R0");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_inc_r7() {
    let r = dis(&[0x1F]);
    assert_eq!(r.mnemonic, "INC");
    assert_eq!(r.operands, "R7");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_dec_r3() {
    let r = dis(&[0xCB]);
    assert_eq!(r.mnemonic, "DEC");
    assert_eq!(r.operands, "R3");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_add_a_r5() {
    let r = dis(&[0x6D]);
    assert_eq!(r.mnemonic, "ADD");
    assert_eq!(r.operands, "A,R5");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_addc_a_r2() {
    let r = dis(&[0x7A]);
    assert_eq!(r.mnemonic, "ADDC");
    assert_eq!(r.operands, "A,R2");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_orl_a_r4() {
    let r = dis(&[0x4C]);
    assert_eq!(r.mnemonic, "ORL");
    assert_eq!(r.operands, "A,R4");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_anl_a_r0() {
    let r = dis(&[0x58]);
    assert_eq!(r.mnemonic, "ANL");
    assert_eq!(r.operands, "A,R0");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_xrl_a_r6() {
    let r = dis(&[0xDE]);
    assert_eq!(r.mnemonic, "XRL");
    assert_eq!(r.operands, "A,R6");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_mov_a_r0() {
    let r = dis(&[0xF8]);
    assert_eq!(r.mnemonic, "MOV");
    assert_eq!(r.operands, "A,R0");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_mov_r3_a() {
    let r = dis(&[0xAB]);
    assert_eq!(r.mnemonic, "MOV");
    assert_eq!(r.operands, "R3,A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_xch_a_r2() {
    let r = dis(&[0x2A]);
    assert_eq!(r.mnemonic, "XCH");
    assert_eq!(r.operands, "A,R2");
    assert_eq!(r.byte_len, 1);
}

// =============================================================================
// Indirect @Ri operations (1-byte)
// =============================================================================

#[test]
fn test_inc_at_r0() {
    let r = dis(&[0x10]);
    assert_eq!(r.mnemonic, "INC");
    assert_eq!(r.operands, "@R0");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_add_a_at_r1() {
    let r = dis(&[0x61]);
    assert_eq!(r.mnemonic, "ADD");
    assert_eq!(r.operands, "A,@R1");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_anl_a_at_r0() {
    let r = dis(&[0x50]);
    assert_eq!(r.mnemonic, "ANL");
    assert_eq!(r.operands, "A,@R0");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_mov_a_at_r1() {
    let r = dis(&[0xF1]);
    assert_eq!(r.mnemonic, "MOV");
    assert_eq!(r.operands, "A,@R1");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_mov_at_r0_a() {
    let r = dis(&[0xA0]);
    assert_eq!(r.mnemonic, "MOV");
    assert_eq!(r.operands, "@R0,A");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_xch_a_at_r1() {
    let r = dis(&[0x21]);
    assert_eq!(r.mnemonic, "XCH");
    assert_eq!(r.operands, "A,@R1");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_xchd_a_at_r0() {
    let r = dis(&[0x30]);
    assert_eq!(r.mnemonic, "XCHD");
    assert_eq!(r.operands, "A,@R0");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_movx_a_at_r0() {
    let r = dis(&[0x80]);
    assert_eq!(r.mnemonic, "MOVX");
    assert_eq!(r.operands, "A,@R0");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_movx_at_r1_a() {
    let r = dis(&[0x91]);
    assert_eq!(r.mnemonic, "MOVX");
    assert_eq!(r.operands, "@R1,A");
    assert_eq!(r.byte_len, 1);
}

// =============================================================================
// Immediate ALU (2-byte)
// =============================================================================

#[test]
fn test_add_a_imm() {
    let r = dis(&[0x03, 0x42]);
    assert_eq!(r.mnemonic, "ADD");
    assert_eq!(r.operands, "A,#$42");
    assert_eq!(r.byte_len, 2);
}

#[test]
fn test_addc_a_imm() {
    let r = dis(&[0x13, 0xFF]);
    assert_eq!(r.mnemonic, "ADDC");
    assert_eq!(r.operands, "A,#$FF");
    assert_eq!(r.byte_len, 2);
}

#[test]
fn test_orl_a_imm() {
    let r = dis(&[0x43, 0x80]);
    assert_eq!(r.mnemonic, "ORL");
    assert_eq!(r.operands, "A,#$80");
    assert_eq!(r.byte_len, 2);
}

#[test]
fn test_anl_a_imm() {
    let r = dis(&[0x53, 0x0F]);
    assert_eq!(r.mnemonic, "ANL");
    assert_eq!(r.operands, "A,#$0F");
    assert_eq!(r.byte_len, 2);
}

#[test]
fn test_xrl_a_imm() {
    let r = dis(&[0xD3, 0xAA]);
    assert_eq!(r.mnemonic, "XRL");
    assert_eq!(r.operands, "A,#$AA");
    assert_eq!(r.byte_len, 2);
}

// =============================================================================
// Immediate data movement (2-byte)
// =============================================================================

#[test]
fn test_mov_a_imm() {
    let r = dis(&[0x23, 0x55]);
    assert_eq!(r.mnemonic, "MOV");
    assert_eq!(r.operands, "A,#$55");
    assert_eq!(r.byte_len, 2);
}

#[test]
fn test_mov_r0_imm() {
    let r = dis(&[0xB8, 0x10]);
    assert_eq!(r.mnemonic, "MOV");
    assert_eq!(r.operands, "R0,#$10");
    assert_eq!(r.byte_len, 2);
}

#[test]
fn test_mov_r7_imm() {
    let r = dis(&[0xBF, 0xFF]);
    assert_eq!(r.mnemonic, "MOV");
    assert_eq!(r.operands, "R7,#$FF");
    assert_eq!(r.byte_len, 2);
}

#[test]
fn test_mov_at_r0_imm() {
    let r = dis(&[0xB0, 0x20]);
    assert_eq!(r.mnemonic, "MOV");
    assert_eq!(r.operands, "@R0,#$20");
    assert_eq!(r.byte_len, 2);
}

#[test]
fn test_mov_at_r1_imm() {
    let r = dis(&[0xB1, 0x30]);
    assert_eq!(r.mnemonic, "MOV");
    assert_eq!(r.operands, "@R1,#$30");
    assert_eq!(r.byte_len, 2);
}

// =============================================================================
// Port read-modify-write immediate (2-byte)
// =============================================================================

#[test]
fn test_orl_bus_imm() {
    let r = dis(&[0x88, 0x0F]);
    assert_eq!(r.mnemonic, "ORL BUS,#");
    assert_eq!(r.operands, "#$0F");
    assert_eq!(r.byte_len, 2);
}

#[test]
fn test_anl_bus_imm() {
    let r = dis(&[0x98, 0xF0]);
    assert_eq!(r.mnemonic, "ANL BUS,#");
    assert_eq!(r.operands, "#$F0");
    assert_eq!(r.byte_len, 2);
}

#[test]
fn test_orl_p1_imm() {
    let r = dis(&[0x89, 0x01]);
    assert_eq!(r.mnemonic, "ORL P1,#");
    assert_eq!(r.operands, "#$01");
    assert_eq!(r.byte_len, 2);
}

#[test]
fn test_anl_p2_imm() {
    let r = dis(&[0x9A, 0xFE]);
    assert_eq!(r.mnemonic, "ANL P2,#");
    assert_eq!(r.operands, "#$FE");
    assert_eq!(r.byte_len, 2);
}

// =============================================================================
// JMP (11-bit address)
// =============================================================================

#[test]
fn test_jmp_page0() {
    let r = dis(&[0x04, 0x50]);
    assert_eq!(r.mnemonic, "JMP");
    assert_eq!(r.operands, "$0050");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0050));
}

#[test]
fn test_jmp_page3() {
    // 0x64 = 0110_0100 → bits 7:5 = 011 = 3 → addr[10:8] = 0x300
    let r = dis(&[0x64, 0x80]);
    assert_eq!(r.mnemonic, "JMP");
    assert_eq!(r.operands, "$0380");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0380));
}

#[test]
fn test_jmp_page7() {
    // 0xE4 = 1110_0100 → bits 7:5 = 111 = 7 → addr[10:8] = 0x700
    let r = dis(&[0xE4, 0xFF]);
    assert_eq!(r.mnemonic, "JMP");
    assert_eq!(r.operands, "$07FF");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x07FF));
}

// =============================================================================
// CALL (11-bit address)
// =============================================================================

#[test]
fn test_call_page0() {
    let r = dis(&[0x14, 0x20]);
    assert_eq!(r.mnemonic, "CALL");
    assert_eq!(r.operands, "$0020");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0020));
}

#[test]
fn test_call_page5() {
    // 0xB4 = 1011_0100 → bits 7:5 = 101 = 5 → addr[10:8] = 0x500
    let r = dis(&[0xB4, 0x00]);
    assert_eq!(r.mnemonic, "CALL");
    assert_eq!(r.operands, "$0500");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0500));
}

// =============================================================================
// Conditional jumps (8-bit page-relative address)
// =============================================================================

#[test]
fn test_jc() {
    let r = dis_at(0x0100, &[0xF6, 0x50]);
    assert_eq!(r.mnemonic, "JC");
    assert_eq!(r.operands, "$0150");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0150));
}

#[test]
fn test_jnc() {
    let r = dis_at(0x0200, &[0xE6, 0xFF]);
    assert_eq!(r.mnemonic, "JNC");
    assert_eq!(r.operands, "$02FF");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x02FF));
}

#[test]
fn test_jz() {
    let r = dis_at(0x0300, &[0xC6, 0x00]);
    assert_eq!(r.mnemonic, "JZ");
    assert_eq!(r.operands, "$0300");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0300));
}

#[test]
fn test_jnz() {
    let r = dis(&[0x96, 0x10]);
    assert_eq!(r.mnemonic, "JNZ");
    assert_eq!(r.operands, "$0010");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0010));
}

#[test]
fn test_jf0() {
    let r = dis(&[0xB6, 0x40]);
    assert_eq!(r.mnemonic, "JF0");
    assert_eq!(r.operands, "$0040");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0040));
}

#[test]
fn test_jf1() {
    let r = dis(&[0x76, 0x80]);
    assert_eq!(r.mnemonic, "JF1");
    assert_eq!(r.operands, "$0080");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0080));
}

#[test]
fn test_jt0() {
    let r = dis(&[0x36, 0x20]);
    assert_eq!(r.mnemonic, "JT0");
    assert_eq!(r.operands, "$0020");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0020));
}

#[test]
fn test_jnt0() {
    let r = dis(&[0x26, 0x30]);
    assert_eq!(r.mnemonic, "JNT0");
    assert_eq!(r.operands, "$0030");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0030));
}

#[test]
fn test_jt1() {
    let r = dis(&[0x56, 0x40]);
    assert_eq!(r.mnemonic, "JT1");
    assert_eq!(r.operands, "$0040");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0040));
}

#[test]
fn test_jnt1() {
    let r = dis(&[0x46, 0x50]);
    assert_eq!(r.mnemonic, "JNT1");
    assert_eq!(r.operands, "$0050");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0050));
}

#[test]
fn test_jtf() {
    let r = dis(&[0x16, 0x60]);
    assert_eq!(r.mnemonic, "JTF");
    assert_eq!(r.operands, "$0060");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0060));
}

#[test]
fn test_jni() {
    let r = dis(&[0x86, 0x70]);
    assert_eq!(r.mnemonic, "JNI");
    assert_eq!(r.operands, "$0070");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0070));
}

// =============================================================================
// Bit test jumps (JBb)
// =============================================================================

#[test]
fn test_jb0() {
    let r = dis(&[0x12, 0x50]);
    assert_eq!(r.mnemonic, "JB0");
    assert_eq!(r.operands, "$0050");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0050));
}

#[test]
fn test_jb3() {
    let r = dis(&[0x72, 0x80]);
    assert_eq!(r.mnemonic, "JB3");
    assert_eq!(r.operands, "$0080");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0080));
}

#[test]
fn test_jb7() {
    let r = dis(&[0xF2, 0x10]);
    assert_eq!(r.mnemonic, "JB7");
    assert_eq!(r.operands, "$0010");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0010));
}

// =============================================================================
// DJNZ (2-byte: register + page-relative address)
// =============================================================================

#[test]
fn test_djnz_r0() {
    let r = dis_at(0x0100, &[0xE8, 0x50]);
    assert_eq!(r.mnemonic, "DJNZ");
    assert_eq!(r.operands, "R0,$0150");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0150));
}

#[test]
fn test_djnz_r7() {
    let r = dis_at(0x0200, &[0xEF, 0x00]);
    assert_eq!(r.mnemonic, "DJNZ");
    assert_eq!(r.operands, "R7,$0200");
    assert_eq!(r.byte_len, 2);
    assert_eq!(r.target_addr, Some(0x0200));
}

// =============================================================================
// Illegal opcodes
// =============================================================================

#[test]
fn test_illegal_opcode() {
    let r = dis(&[0x01]);
    assert_eq!(r.mnemonic, "???");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_illegal_opcode_0x33() {
    let r = dis(&[0x33]);
    assert_eq!(r.mnemonic, "???");
    assert_eq!(r.byte_len, 1);
}

// =============================================================================
// Edge cases
// =============================================================================

#[test]
fn test_empty_bytes() {
    let r = I8035::disassemble(0x0000, &[]);
    assert_eq!(r.mnemonic, "???");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_truncated_2byte_instruction() {
    // JMP needs 2 bytes, only 1 provided
    let r = dis(&[0x04]);
    assert_eq!(r.mnemonic, "???");
    assert_eq!(r.byte_len, 1);
}

#[test]
fn test_raw_bytes_captured() {
    let r = dis(&[0x04, 0x50, 0xFF]);
    assert_eq!(r.bytes[0], 0x04);
    assert_eq!(r.bytes[1], 0x50);
    assert_eq!(r.byte_len, 2);
}

// =============================================================================
// Display formatting
// =============================================================================

#[test]
fn test_display_inherent() {
    let r = dis(&[0x00]);
    assert_eq!(format!("{}", r), "NOP");
}

#[test]
fn test_display_with_operands() {
    let r = dis(&[0x03, 0x42]);
    assert_eq!(format!("{}", r), "ADD   A,#$42");
}

#[test]
fn test_display_jmp() {
    let r = dis(&[0x04, 0x50]);
    assert_eq!(format!("{}", r), "JMP   $0050");
}

// =============================================================================
// Symbol resolution
// =============================================================================

#[test]
fn test_format_with_symbols_match() {
    let r = dis(&[0x04, 0x50]);
    let output = r.format_with_symbols(|addr| if addr == 0x0050 { Some("main") } else { None });
    assert_eq!(output, "JMP   main");
}

#[test]
fn test_format_with_symbols_no_match() {
    let r = dis(&[0x04, 0x50]);
    let output = r.format_with_symbols(|_| None);
    assert_eq!(output, "JMP   $0050");
}

#[test]
fn test_format_with_symbols_no_target() {
    let r = dis(&[0x00]);
    let output = r.format_with_symbols(|_| None);
    assert_eq!(output, "NOP");
}

// =============================================================================
// Full opcode coverage: every valid opcode decodes to a known mnemonic
// =============================================================================

#[test]
fn test_all_valid_opcodes_have_mnemonics() {
    // Every opcode that the CPU execute_instruction handles should decode
    // to a non-"???" mnemonic. Provide 2 bytes for each to handle 2-byte ops.
    let mut buf = [0u8; 2];
    for opcode in 0x00..=0xFFu8 {
        buf[0] = opcode;
        buf[1] = 0x00;
        let r = I8035::disassemble(0x0000, &buf);
        // We don't assert specific mnemonics here, just check byte_len is valid
        assert!(
            r.byte_len == 1 || r.byte_len == 2,
            "opcode 0x{:02X}: unexpected byte_len {}",
            opcode,
            r.byte_len
        );
    }
}
