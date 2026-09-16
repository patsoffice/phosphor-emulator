//! Intel 8088 disassembler, in the shape of the other five.
//!
//! Two kinds of assertion here, and the second is the one worth having.
//!
//! The first is the ordinary one: a known encoding disassembles to known text.
//! That catches a wrong mnemonic or a mangled operand.
//!
//! The second is that **the disassembler's idea of instruction length is the
//! CPU's**. `core/src/cpu/i8088/format.rs` is checked against 2,797,000
//! hardware vectors on every validation run, so it is the authority; this file
//! sweeps all 256 opcodes and asserts the disassembler advances by exactly what
//! that table implies. A disassembler that drifts from it walks out of phase
//! with the instruction stream and every line after the drift is fiction,
//! which is a failure mode that looks like plausible code rather than an error.

use phosphor_core::cpu::disasm::Disassemble;
use phosphor_core::cpu::i8088::I8088;

/// Disassemble at address 0 and return the rendered line.
fn text(bytes: &[u8]) -> String {
    let d = I8088::disassemble(0x0000, bytes);
    if d.operands.is_empty() {
        d.mnemonic.to_string()
    } else {
        format!("{} {}", d.mnemonic, d.operands)
    }
}

/// Disassemble at a given address, for the branch forms.
fn text_at(addr: u32, bytes: &[u8]) -> String {
    let d = I8088::disassemble(addr, bytes);
    format!("{} {}", d.mnemonic, d.operands)
}

fn len(bytes: &[u8]) -> u8 {
    I8088::disassemble(0x0000, bytes).byte_len
}

// --- The ALU block, which is most of the low half of the map ----------------

#[test]
fn the_alu_block_covers_eight_operations_in_six_forms() {
    // ADD, the row at 0x00, in all six of its forms.
    assert_eq!(text(&[0x00, 0xC1]), "ADD CL, AL");
    assert_eq!(text(&[0x01, 0xC1]), "ADD CX, AX");
    assert_eq!(text(&[0x02, 0xC1]), "ADD AL, CL");
    assert_eq!(text(&[0x03, 0xC1]), "ADD AX, CX");
    assert_eq!(text(&[0x04, 0x42]), "ADD AL, $42");
    assert_eq!(text(&[0x05, 0x34, 0x12]), "ADD AX, $1234");

    // The operation comes from bits 5:3, so each row up is the next name.
    assert_eq!(text(&[0x08, 0xC1]), "OR CL, AL");
    assert_eq!(text(&[0x10, 0xC1]), "ADC CL, AL");
    assert_eq!(text(&[0x18, 0xC1]), "SBB CL, AL");
    assert_eq!(text(&[0x20, 0xC1]), "AND CL, AL");
    assert_eq!(text(&[0x28, 0xC1]), "SUB CL, AL");
    assert_eq!(text(&[0x30, 0xC1]), "XOR CL, AL");
    assert_eq!(text(&[0x38, 0xC1]), "CMP CL, AL");
}

#[test]
fn the_leftover_slots_in_each_alu_row_are_segment_pushes_and_bcd_adjusts() {
    assert_eq!(text(&[0x06]), "PUSH ES");
    assert_eq!(text(&[0x07]), "POP ES");
    assert_eq!(text(&[0x0E]), "PUSH CS");
    assert_eq!(text(&[0x16]), "PUSH SS");
    assert_eq!(text(&[0x1E]), "PUSH DS");
    assert_eq!(text(&[0x1F]), "POP DS");
    assert_eq!(text(&[0x27]), "DAA");
    assert_eq!(text(&[0x2F]), "DAS");
    assert_eq!(text(&[0x37]), "AAA");
    assert_eq!(text(&[0x3F]), "AAS");
}

// --- ModR/M addressing, which is where a disassembler earns its keep --------

#[test]
fn every_memory_form_of_the_modrm_byte_is_named() {
    // mod=00: base plus index, no displacement.
    assert_eq!(text(&[0x8B, 0x00]), "MOV AX, [BX+SI]");
    assert_eq!(text(&[0x8B, 0x01]), "MOV AX, [BX+DI]");
    assert_eq!(text(&[0x8B, 0x02]), "MOV AX, [BP+SI]");
    assert_eq!(text(&[0x8B, 0x03]), "MOV AX, [BP+DI]");
    assert_eq!(text(&[0x8B, 0x04]), "MOV AX, [SI]");
    assert_eq!(text(&[0x8B, 0x05]), "MOV AX, [DI]");
    assert_eq!(text(&[0x8B, 0x07]), "MOV AX, [BX]");
}

#[test]
fn mod00_rm110_is_a_bare_address_and_not_bp() {
    // The escape that forces [BP] to be encoded with a zero displacement.
    assert_eq!(text(&[0x8B, 0x06, 0x34, 0x12]), "MOV AX, [$1234]");
    assert_eq!(len(&[0x8B, 0x06, 0x34, 0x12]), 4);

    // ... and [BP] itself, which needs mod=01 and a displacement of zero.
    assert_eq!(text(&[0x8B, 0x46, 0x00]), "MOV AX, [BP+$00]");
    assert_eq!(len(&[0x8B, 0x46, 0x00]), 3);
}

#[test]
fn displacements_are_signed_and_written_as_such() {
    assert_eq!(text(&[0x8B, 0x47, 0x04]), "MOV AX, [BX+$04]");
    // A byte displacement is sign-extended, so 0xFC is -4 and not +252.
    assert_eq!(text(&[0x8B, 0x47, 0xFC]), "MOV AX, [BX-$04]");
    assert_eq!(text(&[0x8B, 0x87, 0x00, 0x01]), "MOV AX, [BX+$100]");
    assert_eq!(text(&[0x8B, 0x87, 0x00, 0xFF]), "MOV AX, [BX-$100]");
}

#[test]
fn a_segment_override_qualifies_the_memory_operand_it_precedes() {
    assert_eq!(text(&[0x26, 0x8B, 0x07]), "MOV AX, ES:[BX]");
    assert_eq!(text(&[0x2E, 0x8B, 0x07]), "MOV AX, CS:[BX]");
    assert_eq!(text(&[0x36, 0x8B, 0x07]), "MOV AX, SS:[BX]");
    assert_eq!(text(&[0x3E, 0x8B, 0x07]), "MOV AX, DS:[BX]");
    // The prefix is part of the instruction's length.
    assert_eq!(len(&[0x26, 0x8B, 0x07]), 3);
    // A register operand has no segment to override, so none is shown.
    assert_eq!(text(&[0x26, 0x8B, 0xC3]), "MOV AX, BX");
}

// --- The groups, where the mnemonic comes from the ModR/M reg field ---------

#[test]
fn the_immediate_alu_group_reads_its_operation_from_the_reg_field() {
    assert_eq!(text(&[0x80, 0xC3, 0x05]), "ADD BL, $05");
    assert_eq!(text(&[0x80, 0xEB, 0x05]), "SUB BL, $05");
    assert_eq!(text(&[0x80, 0xFB, 0x05]), "CMP BL, $05");
    assert_eq!(text(&[0x81, 0xC3, 0x34, 0x12]), "ADD BX, $1234");
    // 0x82 is an undocumented alias of 0x80 and takes the same byte.
    assert_eq!(text(&[0x82, 0xC3, 0x05]), "ADD BL, $05");
}

#[test]
fn the_sign_extending_form_shows_the_word_it_becomes() {
    // 0x83 stores one byte and sign-extends it, so a listing that showed the
    // stored byte would print $FF for an operand the CPU treats as $FFFF.
    assert_eq!(text(&[0x83, 0xC3, 0x01]), "ADD BX, $0001");
    assert_eq!(text(&[0x83, 0xC3, 0xFF]), "ADD BX, $FFFF");
    assert_eq!(len(&[0x83, 0xC3, 0xFF]), 3);
}

#[test]
fn the_shift_group_distinguishes_by_one_from_by_cl() {
    assert_eq!(text(&[0xD0, 0xE0]), "SHL AL, 1");
    assert_eq!(text(&[0xD1, 0xE0]), "SHL AX, 1");
    assert_eq!(text(&[0xD2, 0xE0]), "SHL AL, CL");
    assert_eq!(text(&[0xD3, 0xE0]), "SHL AX, CL");
    assert_eq!(text(&[0xD0, 0xC0]), "ROL AL, 1");
    assert_eq!(text(&[0xD0, 0xF8]), "SAR AL, 1");
}

#[test]
fn the_unary_group_takes_an_immediate_only_for_test() {
    // This is the rule format.rs encodes as ByteIfTest/WordIfTest, seen from
    // the operand side: TEST carries one and the other six do not.
    assert_eq!(text(&[0xF6, 0xC3, 0x05]), "TEST BL, $05");
    assert_eq!(len(&[0xF6, 0xC3, 0x05]), 3);
    assert_eq!(text(&[0xF6, 0xD3]), "NOT BL");
    assert_eq!(len(&[0xF6, 0xD3]), 2);
    assert_eq!(text(&[0xF7, 0xC3, 0x34, 0x12]), "TEST BX, $1234");
    assert_eq!(len(&[0xF7, 0xC3, 0x34, 0x12]), 4);
    assert_eq!(text(&[0xF7, 0xF3]), "DIV BX");
    // reg=1 is the undocumented TEST alias and takes its immediate too.
    assert_eq!(len(&[0xF6, 0xCB, 0x05]), 3);
}

#[test]
fn the_ff_group_marks_its_far_forms() {
    assert_eq!(text(&[0xFF, 0xC3]), "INC BX");
    assert_eq!(text(&[0xFF, 0xCB]), "DEC BX");
    assert_eq!(text(&[0xFF, 0xD3]), "CALL BX");
    assert_eq!(text(&[0xFF, 0x1F]), "CALL FAR [BX]");
    assert_eq!(text(&[0xFF, 0x2F]), "JMP FAR [BX]");
    assert_eq!(text(&[0xFF, 0xF3]), "PUSH BX");
    assert_eq!(text(&[0xFE, 0xC3]), "INC BL");
    assert_eq!(text(&[0xFE, 0xCB]), "DEC BL");
}

// --- Control flow, and the targets a listing needs to be navigable ----------

#[test]
fn a_relative_branch_resolves_its_target_from_the_end_of_the_instruction() {
    // rel8 counts from the byte after the instruction, so +0 is the next one.
    let d = I8088::disassemble(0x0100, &[0xEB, 0x00]);
    assert_eq!(d.target_addr, Some(0x0102));
    assert_eq!(text_at(0x0100, &[0xEB, 0x00]), "JMP $0102");
    // Backwards.
    assert_eq!(text_at(0x0100, &[0xEB, 0xFE]), "JMP $0100");
    // rel16.
    assert_eq!(text_at(0x0100, &[0xE9, 0x00, 0x01]), "JMP $0203");
    assert_eq!(text_at(0x0100, &[0xE8, 0x00, 0x01]), "CALL $0203");
}

#[test]
fn a_branch_wraps_inside_its_segment_rather_than_out_of_it() {
    // A rel8 cannot leave the segment it is in, so the arithmetic wraps at
    // 16 bits and the segment base above is preserved. A plain add would
    // carry into the base and name an address the CPU can never reach.
    // At offset $FFFF, two bytes of instruction and a rel8 of +2 land at
    // offset $0003 of the same segment, not at $30003 in the next one.
    let d = I8088::disassemble(0x2FFFF, &[0xEB, 0x02]);
    assert_eq!(
        d.target_addr,
        Some(0x20003),
        "a forward branch off the top of a segment wraps to its bottom"
    );
}

#[test]
fn every_conditional_jump_is_named() {
    let names = [
        "JO", "JNO", "JB", "JNB", "JZ", "JNZ", "JBE", "JA", "JS", "JNS", "JPE", "JPO", "JL", "JGE",
        "JLE", "JG",
    ];
    for (i, name) in names.iter().enumerate() {
        let opcode = 0x70 + i as u8;
        let d = I8088::disassemble(0x0100, &[opcode, 0x00]);
        assert_eq!(d.mnemonic, *name, "opcode {opcode:#04X}");
        assert_eq!(d.target_addr, Some(0x0102));
    }
}

#[test]
fn the_loop_block_and_the_far_forms_are_named() {
    assert_eq!(text_at(0x0100, &[0xE0, 0x00]), "LOOPNZ $0102");
    assert_eq!(text_at(0x0100, &[0xE1, 0x00]), "LOOPZ $0102");
    assert_eq!(text_at(0x0100, &[0xE2, 0x00]), "LOOP $0102");
    assert_eq!(text_at(0x0100, &[0xE3, 0x00]), "JCXZ $0102");
    // A far pointer is offset first, then segment, and is four bytes.
    assert_eq!(text(&[0xEA, 0x34, 0x12, 0x00, 0xF0]), "JMP $F000:$1234");
    assert_eq!(len(&[0xEA, 0x34, 0x12, 0x00, 0xF0]), 5);
    assert_eq!(text(&[0x9A, 0x34, 0x12, 0x00, 0xF0]), "CALL $F000:$1234");
}

#[test]
fn returns_distinguish_near_from_far_and_with_from_without_a_pop() {
    assert_eq!(text(&[0xC3]), "RET");
    assert_eq!(text(&[0xC2, 0x04, 0x00]), "RET $0004");
    assert_eq!(text(&[0xCB]), "RETF");
    assert_eq!(text(&[0xCA, 0x04, 0x00]), "RETF $0004");
    // The undocumented aliases one encoding below each.
    assert_eq!(text(&[0xC1]), "RET");
    assert_eq!(text(&[0xC9]), "RETF");
}

// --- Moves, ports and the rest ----------------------------------------------

#[test]
fn moves_with_the_register_in_the_opcode_are_named() {
    assert_eq!(text(&[0xB0, 0x42]), "MOV AL, $42");
    assert_eq!(text(&[0xB7, 0x42]), "MOV BH, $42");
    assert_eq!(text(&[0xB8, 0x34, 0x12]), "MOV AX, $1234");
    assert_eq!(text(&[0xBF, 0x34, 0x12]), "MOV DI, $1234");
    assert_eq!(len(&[0xB8, 0x34, 0x12]), 3);
}

#[test]
fn the_accumulator_direct_moves_name_an_address_and_not_a_displacement() {
    assert_eq!(text(&[0xA0, 0x34, 0x12]), "MOV AL, [$1234]");
    assert_eq!(text(&[0xA1, 0x34, 0x12]), "MOV AX, [$1234]");
    assert_eq!(text(&[0xA2, 0x34, 0x12]), "MOV [$1234], AL");
    assert_eq!(text(&[0xA3, 0x34, 0x12]), "MOV [$1234], AX");
}

#[test]
fn segment_register_moves_use_only_the_low_two_bits_of_the_reg_field() {
    assert_eq!(text(&[0x8E, 0xD8]), "MOV DS, AX");
    assert_eq!(text(&[0x8C, 0xD8]), "MOV AX, DS");
    assert_eq!(text(&[0x8E, 0xC0]), "MOV ES, AX");
    // The top bit is ignored by the hardware rather than faulting, so this
    // names the same register as the encoding four below it.
    assert_eq!(text(&[0x8E, 0xF8]), "MOV DS, AX");
}

#[test]
fn ports_are_named_by_immediate_and_by_dx() {
    assert_eq!(text(&[0xE4, 0x21]), "IN AL, $21");
    assert_eq!(text(&[0xE5, 0x21]), "IN AX, $21");
    assert_eq!(text(&[0xE6, 0x21]), "OUT $21, AL");
    assert_eq!(text(&[0xE7, 0x21]), "OUT $21, AX");
    assert_eq!(text(&[0xEC]), "IN AL, DX");
    assert_eq!(text(&[0xEE]), "OUT DX, AL");
}

#[test]
fn the_single_byte_instructions_are_named() {
    for (bytes, want) in [
        (0x90u8, "NOP"),
        (0x98, "CBW"),
        (0x99, "CWD"),
        (0x9C, "PUSHF"),
        (0x9D, "POPF"),
        (0x9E, "SAHF"),
        (0x9F, "LAHF"),
        (0xA4, "MOVSB"),
        (0xA5, "MOVSW"),
        (0xAA, "STOSB"),
        (0xD7, "XLAT"),
        (0xF4, "HLT"),
        (0xF5, "CMC"),
        (0xF8, "CLC"),
        (0xF9, "STC"),
        (0xFA, "CLI"),
        (0xFB, "STI"),
        (0xFC, "CLD"),
        (0xFD, "STD"),
        (0xCF, "IRET"),
    ] {
        assert_eq!(text(&[bytes]), want, "opcode {bytes:#04X}");
        assert_eq!(len(&[bytes]), 1, "opcode {bytes:#04X}");
    }
    // 0x90 is really XCHG AX,AX, and the rest of that row is a real exchange.
    assert_eq!(text(&[0x91]), "XCHG AX, CX");
}

#[test]
fn interrupts_name_their_vector() {
    assert_eq!(text(&[0xCC]), "INT 3");
    assert_eq!(text(&[0xCD, 0x21]), "INT $21");
    assert_eq!(text(&[0xCE]), "INTO");
    assert_eq!(len(&[0xCD, 0x21]), 2);
}

// --- Prefixes that are not part of the next instruction's name --------------

#[test]
fn a_rep_prefix_joins_the_string_instruction_it_precedes() {
    assert_eq!(text(&[0xF3, 0xA4]), "REP MOVSB");
    assert_eq!(text(&[0xF3, 0xA5]), "REP MOVSW");
    assert_eq!(text(&[0xF3, 0xAA]), "REP STOSB");
    // CMPS and SCAS are the ones a listing calls REPE rather than REP.
    assert_eq!(text(&[0xF3, 0xA6]), "REPE CMPSB");
    assert_eq!(text(&[0xF2, 0xA6]), "REPNE CMPSB");
    assert_eq!(text(&[0xF2, 0xAE]), "REPNE SCASB");
    assert_eq!(len(&[0xF3, 0xA4]), 2, "the prefix is part of the length");
}

#[test]
fn a_prefix_with_no_combined_name_gets_a_line_of_its_own() {
    // `mnemonic` is a `&'static str`, so LOCK and a REP that precedes
    // something other than a string instruction cannot be folded into the
    // name. Emitting the byte alone keeps the length honest and lets the next
    // call disassemble what follows.
    assert_eq!(text(&[0xF0, 0x00, 0xC1]), "LOCK");
    assert_eq!(len(&[0xF0, 0x00, 0xC1]), 1);
    assert_eq!(text(&[0xF3, 0x90]), "REP");
    assert_eq!(len(&[0xF3, 0x90]), 1);
}

// --- Robustness -------------------------------------------------------------

#[test]
fn an_instruction_running_off_the_end_is_reported_and_not_invented() {
    // A truncated instruction at the end of a ROM region is a real thing to
    // see. Inventing operands for bytes that are not there would make the
    // listing lie about the ROM's contents.
    assert_eq!(text(&[0xB8]), "DB $B8", "MOV AX,imm16 with no immediate");
    assert_eq!(text(&[0x8B]), "DB $8B", "a ModR/M byte that is not there");
    assert_eq!(
        text(&[0x8B, 0x47]),
        "DB $8B",
        "a displacement that is not there"
    );
    assert_eq!(text(&[]), "DB $00");
    for bytes in [vec![0xB8u8], vec![0x8B], vec![0x8B, 0x47]] {
        assert_eq!(len(&bytes), 1, "a truncated instruction advances one byte");
    }
}

#[test]
fn no_opcode_panics_and_none_reports_a_zero_length() {
    // Every opcode, with enough trailing bytes that nothing is truncated.
    for opcode in 0..=255u8 {
        for modrm in [0x00u8, 0x47, 0x87, 0xC3, 0x06] {
            let bytes = [opcode, modrm, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
            let d = I8088::disassemble(0x1000, &bytes);
            assert!(
                d.byte_len > 0,
                "opcode {opcode:#04X} modrm {modrm:#04X} advanced zero bytes, \
                 which would hang a listing"
            );
            assert!(
                !d.mnemonic.is_empty(),
                "opcode {opcode:#04X} has an empty mnemonic"
            );
        }
    }
}

// --- The load-bearing one: length agrees with the CPU's own table -----------

#[test]
fn the_disassembler_advances_by_exactly_what_the_cpu_fetches() {
    // `format.rs` is the authority on instruction length: it is what the
    // per-cycle core loads with, and it is checked against 2,797,000 hardware
    // vectors. This recomputes the expected length from the same rules the
    // table states and holds the disassembler to it for every opcode and a
    // spread of ModR/M bytes covering all four mod forms and the direct
    // escape.
    //
    // Prefix bytes are excluded: they are not in that table, and this file
    // covers them separately.
    const PREFIXES: [u8; 7] = [0x26, 0x2E, 0x36, 0x3E, 0xF0, 0xF2, 0xF3];

    for opcode in 0..=255u8 {
        if PREFIXES.contains(&opcode) {
            continue;
        }
        for modrm in [
            0x00u8, // mod=00, no displacement
            0x06,   // mod=00 rm=110, the direct-address escape: 2 bytes
            0x47,   // mod=01, one displacement byte
            0x87,   // mod=10, two displacement bytes
            0xC3,   // mod=11, a register operand
            0xC8,   // mod=11 reg=1, the undocumented TEST alias
            0xD0,   // mod=11 reg=2, a group member with no immediate
        ] {
            let bytes = [opcode, modrm, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
            let d = I8088::disassemble(0x1000, &bytes);

            // The same arithmetic format.rs describes: opcode, then an
            // optional ModR/M, then the displacement that byte implies, then
            // the immediate the opcode (and for two groups, the reg field)
            // calls for.
            let expected = expected_len(opcode, modrm);
            assert_eq!(
                d.byte_len, expected,
                "opcode {opcode:#04X} with modrm {modrm:#04X} disassembled as \
                 {} {} at {} bytes, but the CPU fetches {expected}",
                d.mnemonic, d.operands, d.byte_len
            );
        }
    }
}

/// Instruction length as the 8088's own instruction loader computes it.
///
/// Deliberately written out here from the documented encoding rather than
/// calling into `format.rs`, which is `pub(crate)` and therefore not reachable
/// from an integration test. That turns out to be the better test anyway: a
/// check that called the same table the code under test calls would agree with
/// it however wrong both were.
fn expected_len(opcode: u8, modrm: u8) -> u8 {
    let (has_modrm, imm) = match opcode {
        0x00..=0x3F => match opcode & 7 {
            0..=3 => (true, 0),
            4 => (false, 1),
            5 => (false, 2),
            _ => (false, 0),
        },
        0x40..=0x5F => (false, 0),
        0x60..=0x7F => (false, 1),
        0x80 | 0x82 | 0x83 => (true, 1),
        0x81 => (true, 2),
        0x84..=0x8F => (true, 0),
        0x90..=0x99 => (false, 0),
        0x9A => (false, 4),
        0x9B..=0x9F => (false, 0),
        0xA0..=0xA3 => (false, 2),
        0xA4..=0xA7 => (false, 0),
        0xA8 => (false, 1),
        0xA9 => (false, 2),
        0xAA..=0xAF => (false, 0),
        0xB0..=0xB7 => (false, 1),
        0xB8..=0xBF => (false, 2),
        0xC0 | 0xC2 | 0xC8 | 0xCA => (false, 2),
        0xC1 | 0xC3 | 0xC9 | 0xCB => (false, 0),
        0xC4 | 0xC5 => (true, 0),
        0xC6 => (true, 1),
        0xC7 => (true, 2),
        0xCC => (false, 0),
        0xCD => (false, 1),
        0xCE | 0xCF => (false, 0),
        0xD0..=0xD3 => (true, 0),
        0xD4 | 0xD5 => (false, 1),
        0xD6 | 0xD7 => (false, 0),
        0xD8..=0xDF => (true, 0),
        0xE0..=0xE7 => (false, 1),
        0xE8 | 0xE9 => (false, 2),
        0xEA => (false, 4),
        0xEB => (false, 1),
        0xEC..=0xEF => (false, 0),
        0xF0..=0xF5 => (false, 0),
        // The unary group: an immediate only when reg is 0 or 1, both TEST.
        0xF6 => (true, if (modrm >> 3) & 7 <= 1 { 1 } else { 0 }),
        0xF7 => (true, if (modrm >> 3) & 7 <= 1 { 2 } else { 0 }),
        0xF8..=0xFD => (false, 0),
        0xFE | 0xFF => (true, 0),
    };

    let disp = if has_modrm {
        match (modrm >> 6) & 3 {
            0 => {
                if modrm & 7 == 6 {
                    2
                } else {
                    0
                }
            }
            1 => 1,
            2 => 2,
            _ => 0,
        }
    } else {
        0
    };

    1 + u8::from(has_modrm) + disp + imm
}
