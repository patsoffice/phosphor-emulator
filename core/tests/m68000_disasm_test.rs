//! M68000 disassembler tests, grouped by opcode line: data movement,
//! ALU, shifts, the line-4 misc group, and control flow.

use phosphor_core::M68000;
use phosphor_core::cpu::{Disassemble, DisassembledInstruction};

fn dis(addr: u32, bytes: &[u8]) -> DisassembledInstruction {
    M68000::disassemble(addr, bytes)
}

fn dis0(bytes: &[u8]) -> DisassembledInstruction {
    dis(0x1000, bytes)
}

// ---------------------------------------------------------------------------
// MOVE family
// ---------------------------------------------------------------------------

#[test]
fn move_register_to_register() {
    let r = dis0(&[0x10, 0x01]);
    assert_eq!(r.mnemonic, "MOVE.B");
    assert_eq!(r.operands, "D1,D0");
    assert_eq!(r.byte_len, 2);
}

#[test]
fn move_immediate_word() {
    let r = dis0(&[0x34, 0x3C, 0x12, 0x34]);
    assert_eq!(r.mnemonic, "MOVE.W");
    assert_eq!(r.operands, "#$1234,D2");
    assert_eq!(r.byte_len, 4);
}

#[test]
fn move_long_absolute_source() {
    let r = dis0(&[0x20, 0x39, 0x00, 0xFF, 0x00, 0x42]);
    assert_eq!(r.mnemonic, "MOVE.L");
    assert_eq!(r.operands, "$00FF0042.L,D0");
    assert_eq!(r.byte_len, 6);
}

#[test]
fn move_to_absolute_word_destination() {
    let r = dis0(&[0x31, 0xC0, 0x12, 0x34]);
    assert_eq!(r.mnemonic, "MOVE.W");
    assert_eq!(r.operands, "D0,$1234.W");
    assert_eq!(r.byte_len, 4);
}

#[test]
fn move_longest_instruction_ten_bytes() {
    // MOVE.L #$DEADBEEF,$00FF0000.L — the 10-byte maximum.
    let r = dis0(&[0x23, 0xFC, 0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0xFF, 0x00, 0x00]);
    assert_eq!(r.mnemonic, "MOVE.L");
    assert_eq!(r.operands, "#$DEADBEEF,$00FF0000.L");
    assert_eq!(r.byte_len, 10);
    assert_eq!(
        r.bytes,
        [0x23, 0xFC, 0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0xFF, 0x00, 0x00]
    );
}

#[test]
fn movea_word() {
    let r = dis0(&[0x34, 0x43]);
    assert_eq!(r.mnemonic, "MOVEA.W");
    assert_eq!(r.operands, "D3,A2");
}

#[test]
fn movea_byte_is_unassigned() {
    let r = dis0(&[0x14, 0x43]);
    assert_eq!(r.mnemonic, "DC.W");
}

#[test]
fn move_displacement_source() {
    let r = dis0(&[0x38, 0x29, 0x00, 0x10]);
    assert_eq!(r.mnemonic, "MOVE.W");
    assert_eq!(r.operands, "$10(A1),D4");
}

#[test]
fn move_negative_displacement() {
    let r = dis0(&[0x38, 0x29, 0xFF, 0xF0]);
    assert_eq!(r.operands, "-$10(A1),D4");
}

#[test]
fn move_indexed_source() {
    let r = dis0(&[0x10, 0x32, 0x30, 0x7F]);
    assert_eq!(r.mnemonic, "MOVE.B");
    assert_eq!(r.operands, "$7F(A2,D3.W),D0");
}

#[test]
fn move_pc_relative_resolves_target() {
    // d16(PC) with d16 = 0x10 at address 0x1000: extension word is at
    // 0x1002, so the resolved operand address is 0x1012.
    let r = dis(0x1000, &[0x30, 0x3A, 0x00, 0x10]);
    assert_eq!(r.operands, "$001012(PC),D0");
}

#[test]
fn move_pc_indexed_source() {
    let r = dis0(&[0x30, 0x3B, 0xA0, 0x08]);
    assert_eq!(r.operands, "$8(PC,A2.W),D0");
}

#[test]
fn move_predecrement_postincrement() {
    let r = dis0(&[0x30, 0xE0]);
    assert_eq!(r.mnemonic, "MOVE.W");
    assert_eq!(r.operands, "-(A0),(A0)+");
}

#[test]
fn moveq_positive_and_negative() {
    let r = dis0(&[0x72, 0x42]);
    assert_eq!(r.mnemonic, "MOVEQ");
    assert_eq!(r.operands, "#$42,D1");

    let r = dis0(&[0x76, 0xFF]);
    assert_eq!(r.operands, "#-$1,D3");
}

#[test]
fn movep_both_directions() {
    let r = dis0(&[0x05, 0x09, 0x00, 0x08]);
    assert_eq!(r.mnemonic, "MOVEP.W");
    assert_eq!(r.operands, "$8(A1),D2");

    let r = dis0(&[0x05, 0xC9, 0x00, 0x08]);
    assert_eq!(r.mnemonic, "MOVEP.L");
    assert_eq!(r.operands, "D2,$8(A1)");
}

// ---------------------------------------------------------------------------
// Immediates and bit ops (line 0)
// ---------------------------------------------------------------------------

#[test]
fn ori_to_ccr_and_andi_to_sr() {
    let r = dis0(&[0x00, 0x3C, 0x00, 0x0F]);
    assert_eq!(r.mnemonic, "ORI");
    assert_eq!(r.operands, "#$0F,CCR");

    let r = dis0(&[0x02, 0x7C, 0x27, 0x00]);
    assert_eq!(r.mnemonic, "ANDI");
    assert_eq!(r.operands, "#$2700,SR");
}

#[test]
fn addi_word() {
    let r = dis0(&[0x06, 0x42, 0x01, 0x00]);
    assert_eq!(r.mnemonic, "ADDI.W");
    assert_eq!(r.operands, "#$0100,D2");
}

#[test]
fn cmpi_byte_memory() {
    let r = dis0(&[0x0C, 0x10, 0x00, 0x41]);
    assert_eq!(r.mnemonic, "CMPI.B");
    assert_eq!(r.operands, "#$41,(A0)");
}

#[test]
fn subi_long() {
    let r = dis0(&[0x04, 0x81, 0x00, 0x01, 0x00, 0x00]);
    assert_eq!(r.mnemonic, "SUBI.L");
    assert_eq!(r.operands, "#$00010000,D1");
    assert_eq!(r.byte_len, 6);
}

#[test]
fn dynamic_bit_ops() {
    let r = dis0(&[0x03, 0x10]);
    assert_eq!(r.mnemonic, "BTST");
    assert_eq!(r.operands, "D1,(A0)");

    let r = dis0(&[0x03, 0xC0]);
    assert_eq!(r.mnemonic, "BSET");
    assert_eq!(r.operands, "D1,D0");
}

#[test]
fn static_bit_ops() {
    let r = dis0(&[0x08, 0xC0, 0x00, 0x07]);
    assert_eq!(r.mnemonic, "BSET");
    assert_eq!(r.operands, "#7,D0");

    let r = dis0(&[0x08, 0x90, 0x00, 0x03]);
    assert_eq!(r.mnemonic, "BCLR");
    assert_eq!(r.operands, "#3,(A0)");
}

// ---------------------------------------------------------------------------
// ALU lines (8, 9, B, C, D)
// ---------------------------------------------------------------------------

#[test]
fn add_both_directions() {
    let r = dis0(&[0xD2, 0x58]);
    assert_eq!(r.mnemonic, "ADD.W");
    assert_eq!(r.operands, "(A0)+,D1");

    let r = dis0(&[0xD5, 0xA3]);
    assert_eq!(r.mnemonic, "ADD.L");
    assert_eq!(r.operands, "D2,-(A3)");
}

#[test]
fn adda_long() {
    let r = dis0(&[0xD5, 0xC1]);
    assert_eq!(r.mnemonic, "ADDA.L");
    assert_eq!(r.operands, "D1,A2");
}

#[test]
fn addx_subx_pairs() {
    let r = dis0(&[0xD1, 0x01]);
    assert_eq!(r.mnemonic, "ADDX.B");
    assert_eq!(r.operands, "D1,D0");

    let r = dis0(&[0x91, 0x09]);
    assert_eq!(r.mnemonic, "SUBX.B");
    assert_eq!(r.operands, "-(A1),-(A0)");
}

#[test]
fn cmp_cmpa_cmpm() {
    let r = dis0(&[0xB0, 0x41]);
    assert_eq!(r.mnemonic, "CMP.W");
    assert_eq!(r.operands, "D1,D0");

    let r = dis0(&[0xB1, 0xC9]);
    assert_eq!(r.mnemonic, "CMPA.L");
    assert_eq!(r.operands, "A1,A0");

    let r = dis0(&[0xB3, 0x08]);
    assert_eq!(r.mnemonic, "CMPM.B");
    assert_eq!(r.operands, "(A0)+,(A1)+");
}

#[test]
fn eor_register_to_ea() {
    let r = dis0(&[0xB7, 0x41]);
    assert_eq!(r.mnemonic, "EOR.W");
    assert_eq!(r.operands, "D3,D1");
}

#[test]
fn and_or_directions() {
    let r = dis0(&[0xC2, 0x10]);
    assert_eq!(r.mnemonic, "AND.B");
    assert_eq!(r.operands, "(A0),D1");

    let r = dis0(&[0x85, 0x78, 0x12, 0x34]);
    assert_eq!(r.mnemonic, "OR.W");
    assert_eq!(r.operands, "D2,$1234.W");
}

#[test]
fn mul_div() {
    let r = dis0(&[0xC4, 0xC1]);
    assert_eq!(r.mnemonic, "MULU.W");
    assert_eq!(r.operands, "D1,D2");

    let r = dis0(&[0x87, 0xFC, 0x00, 0x0A]);
    assert_eq!(r.mnemonic, "DIVS.W");
    assert_eq!(r.operands, "#$000A,D3");
}

#[test]
fn bcd_pairs() {
    let r = dis0(&[0xC1, 0x01]);
    assert_eq!(r.mnemonic, "ABCD");
    assert_eq!(r.operands, "D1,D0");

    let r = dis0(&[0x81, 0x09]);
    assert_eq!(r.mnemonic, "SBCD");
    assert_eq!(r.operands, "-(A1),-(A0)");
}

#[test]
fn exg_forms() {
    let r = dis0(&[0xC3, 0x42]);
    assert_eq!(r.mnemonic, "EXG");
    assert_eq!(r.operands, "D1,D2");

    let r = dis0(&[0xC3, 0x4A]);
    assert_eq!(r.operands, "A1,A2");

    let r = dis0(&[0xC3, 0x8A]);
    assert_eq!(r.operands, "D1,A2");
}

// ---------------------------------------------------------------------------
// Shifts and rotates (line E)
// ---------------------------------------------------------------------------

#[test]
fn shift_register_immediate_count() {
    let r = dis0(&[0xE7, 0x41]);
    assert_eq!(r.mnemonic, "ASL.W");
    assert_eq!(r.operands, "#3,D1");
}

#[test]
fn shift_register_count_of_zero_means_eight() {
    let r = dis0(&[0xE0, 0x17]);
    assert_eq!(r.mnemonic, "ROXR.B");
    assert_eq!(r.operands, "#8,D7");
}

#[test]
fn shift_register_by_register() {
    let r = dis0(&[0xE4, 0xAB]);
    assert_eq!(r.mnemonic, "LSR.L");
    assert_eq!(r.operands, "D2,D3");
}

#[test]
fn shift_memory_forms() {
    let r = dis0(&[0xE1, 0xF8, 0x12, 0x34]);
    assert_eq!(r.mnemonic, "ASL");
    assert_eq!(r.operands, "$1234.W");

    let r = dis0(&[0xE6, 0xD0]);
    assert_eq!(r.mnemonic, "ROR");
    assert_eq!(r.operands, "(A0)");
}

// ---------------------------------------------------------------------------
// Unassigned encodings and truncation
// ---------------------------------------------------------------------------

#[test]
fn line_a_and_f_render_as_data() {
    let r = dis0(&[0xA0, 0x00]);
    assert_eq!(r.mnemonic, "DC.W");
    assert_eq!(r.operands, "$A000");
    assert_eq!(r.byte_len, 2);

    let r = dis0(&[0xFF, 0xFF]);
    assert_eq!(r.operands, "$FFFF");
}

#[test]
fn truncated_input() {
    // Less than one opcode word.
    let r = dis0(&[0x30]);
    assert_eq!(r.mnemonic, "???");
    assert_eq!(r.byte_len, 1);

    // Opcode wants an extension word the slice doesn't have; one opcode
    // word is consumed so a scan stays word-aligned.
    let r = dis0(&[0x30, 0x3C]);
    assert_eq!(r.mnemonic, "???");
    assert_eq!(r.byte_len, 2);
}

#[test]
fn display_format_pads_mnemonic() {
    let r = dis0(&[0x10, 0x01]);
    assert_eq!(format!("{r}"), "MOVE.B D1,D0");
}

// ---------------------------------------------------------------------------
// Line 4 misc: unary group, SR/CCR moves, SWAP/PEA/EXT/MOVEM, specials
// ---------------------------------------------------------------------------

#[test]
fn unary_sized_forms() {
    let r = dis0(&[0x40, 0x00]);
    assert_eq!(r.mnemonic, "NEGX.B");
    assert_eq!(r.operands, "D0");

    let r = dis0(&[0x42, 0x50]);
    assert_eq!(r.mnemonic, "CLR.W");
    assert_eq!(r.operands, "(A0)");

    let r = dis0(&[0x44, 0x83]);
    assert_eq!(r.mnemonic, "NEG.L");
    assert_eq!(r.operands, "D3");

    let r = dis0(&[0x46, 0x05]);
    assert_eq!(r.mnemonic, "NOT.B");
    assert_eq!(r.operands, "D5");

    let r = dis0(&[0x4A, 0x12]);
    assert_eq!(r.mnemonic, "TST.B");
    assert_eq!(r.operands, "(A2)");
}

#[test]
fn sr_and_ccr_moves() {
    let r = dis0(&[0x40, 0xC0]);
    assert_eq!(r.mnemonic, "MOVE");
    assert_eq!(r.operands, "SR,D0");

    let r = dis0(&[0x44, 0xC1]);
    assert_eq!(r.operands, "D1,CCR");

    let r = dis0(&[0x46, 0xFC, 0x27, 0x00]);
    assert_eq!(r.operands, "#$2700,SR");
}

#[test]
fn move_from_ccr_is_68010_only() {
    let r = dis0(&[0x42, 0xC0]);
    assert_eq!(r.mnemonic, "DC.W");
}

#[test]
fn nbcd_swap_pea_ext() {
    let r = dis0(&[0x48, 0x02]);
    assert_eq!(r.mnemonic, "NBCD");
    assert_eq!(r.operands, "D2");

    let r = dis0(&[0x48, 0x44]);
    assert_eq!(r.mnemonic, "SWAP");
    assert_eq!(r.operands, "D4");

    let r = dis0(&[0x48, 0x78, 0x12, 0x34]);
    assert_eq!(r.mnemonic, "PEA");
    assert_eq!(r.operands, "$1234.W");

    let r = dis0(&[0x48, 0x86]);
    assert_eq!(r.mnemonic, "EXT.W");
    assert_eq!(r.operands, "D6");

    let r = dis0(&[0x48, 0xC6]);
    assert_eq!(r.mnemonic, "EXT.L");
}

#[test]
fn movem_store_predecrement_reverses_mask() {
    // MOVEM.W D0-D2/A5,-(A0): the predecrement form carries a
    // bit-reversed mask (bit 15 = D0).
    let r = dis0(&[0x48, 0xA0, 0xE0, 0x04]);
    assert_eq!(r.mnemonic, "MOVEM.W");
    assert_eq!(r.operands, "D0-D2/A5,-(A0)");
}

#[test]
fn movem_load_normal_mask() {
    let r = dis0(&[0x4C, 0xD9, 0xFF, 0x01]);
    assert_eq!(r.mnemonic, "MOVEM.L");
    assert_eq!(r.operands, "(A1)+,D0/A0-A7");
}

#[test]
fn tas_and_illegal() {
    let r = dis0(&[0x4A, 0xC1]);
    assert_eq!(r.mnemonic, "TAS");
    assert_eq!(r.operands, "D1");

    let r = dis0(&[0x4A, 0xFC]);
    assert_eq!(r.mnemonic, "ILLEGAL");
    assert_eq!(r.operands, "");
}

#[test]
fn lea_and_chk() {
    let r = dis0(&[0x4B, 0xF9, 0x00, 0xFF, 0x00, 0x00]);
    assert_eq!(r.mnemonic, "LEA");
    assert_eq!(r.operands, "$00FF0000.L,A5");

    let r = dis0(&[0x45, 0x81]);
    assert_eq!(r.mnemonic, "CHK");
    assert_eq!(r.operands, "D1,D2");
}

#[test]
fn one_word_specials() {
    assert_eq!(dis0(&[0x4E, 0x70]).mnemonic, "RESET");
    assert_eq!(dis0(&[0x4E, 0x71]).mnemonic, "NOP");
    assert_eq!(dis0(&[0x4E, 0x73]).mnemonic, "RTE");
    assert_eq!(dis0(&[0x4E, 0x75]).mnemonic, "RTS");
    assert_eq!(dis0(&[0x4E, 0x76]).mnemonic, "TRAPV");
    assert_eq!(dis0(&[0x4E, 0x77]).mnemonic, "RTR");
    // RTD is 68010+
    assert_eq!(dis0(&[0x4E, 0x74]).mnemonic, "DC.W");
}

#[test]
fn trap_link_unlk_usp_stop() {
    let r = dis0(&[0x4E, 0x4F]);
    assert_eq!(r.mnemonic, "TRAP");
    assert_eq!(r.operands, "#15");

    let r = dis0(&[0x4E, 0x56, 0xFF, 0xFC]);
    assert_eq!(r.mnemonic, "LINK");
    assert_eq!(r.operands, "A6,#-$4");

    let r = dis0(&[0x4E, 0x5E]);
    assert_eq!(r.mnemonic, "UNLK");
    assert_eq!(r.operands, "A6");

    let r = dis0(&[0x4E, 0x61]);
    assert_eq!(r.operands, "A1,USP");
    let r = dis0(&[0x4E, 0x69]);
    assert_eq!(r.operands, "USP,A1");

    let r = dis0(&[0x4E, 0x72, 0x27, 0x00]);
    assert_eq!(r.mnemonic, "STOP");
    assert_eq!(r.operands, "#$2700");
}

#[test]
fn jmp_jsr_with_static_targets() {
    let r = dis0(&[0x4E, 0xB9, 0x00, 0x00, 0x12, 0x34]);
    assert_eq!(r.mnemonic, "JSR");
    assert_eq!(r.operands, "$00001234.L");
    assert_eq!(r.target_addr, Some(0x1234));

    let r = dis0(&[0x4E, 0xD0]);
    assert_eq!(r.mnemonic, "JMP");
    assert_eq!(r.operands, "(A0)");
    assert_eq!(r.target_addr, None);

    let r = dis(0x1000, &[0x4E, 0xFA, 0x00, 0x10]);
    assert_eq!(r.mnemonic, "JMP");
    assert_eq!(r.operands, "$001012(PC)");
    assert_eq!(r.target_addr, Some(0x1012));
}

// ---------------------------------------------------------------------------
// Line 5: ADDQ/SUBQ, Scc, DBcc
// ---------------------------------------------------------------------------

#[test]
fn addq_subq() {
    // Data field 0 encodes 8.
    let r = dis0(&[0x50, 0x41]);
    assert_eq!(r.mnemonic, "ADDQ.W");
    assert_eq!(r.operands, "#8,D1");

    let r = dis0(&[0x53, 0x13]);
    assert_eq!(r.mnemonic, "SUBQ.B");
    assert_eq!(r.operands, "#1,(A3)");
}

#[test]
fn scc_forms() {
    let r = dis0(&[0x57, 0xC0]);
    assert_eq!(r.mnemonic, "SEQ");
    assert_eq!(r.operands, "D0");
}

#[test]
fn dbcc_resolves_target() {
    // DBF D2 with displacement -4 from 0x1000: target = 0x1002 - 4.
    let r = dis(0x1000, &[0x51, 0xCA, 0xFF, 0xFC]);
    assert_eq!(r.mnemonic, "DBF");
    assert_eq!(r.operands, "D2,$000FFE");
    assert_eq!(r.target_addr, Some(0x0FFE));
    assert_eq!(r.byte_len, 4);
}

// ---------------------------------------------------------------------------
// Line 6: branches
// ---------------------------------------------------------------------------

#[test]
fn bra_short_form() {
    let r = dis(0x1000, &[0x60, 0x10]);
    assert_eq!(r.mnemonic, "BRA");
    assert_eq!(r.operands, "$001012");
    assert_eq!(r.target_addr, Some(0x1012));
    assert_eq!(r.byte_len, 2);
}

#[test]
fn bsr_word_form() {
    // 8-bit displacement of zero selects the word form.
    let r = dis(0x1000, &[0x61, 0x00, 0x02, 0x00]);
    assert_eq!(r.mnemonic, "BSR");
    assert_eq!(r.operands, "$001202");
    assert_eq!(r.target_addr, Some(0x1202));
    assert_eq!(r.byte_len, 4);
}

#[test]
fn bcc_backward_branch() {
    let r = dis(0x1000, &[0x66, 0xF0]);
    assert_eq!(r.mnemonic, "BNE");
    assert_eq!(r.target_addr, Some(0x0FF2));

    let r = dis(0x1000, &[0x6F, 0x08]);
    assert_eq!(r.mnemonic, "BLE");
}
