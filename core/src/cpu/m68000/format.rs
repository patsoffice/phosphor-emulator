//! How many words an instruction is, decided from its opcode alone.
//!
//! A per-clock core cannot run an instruction to find out how long it is. The
//! words after the opcode come out of the prefetch queue, each one leaving a
//! hole that is refilled by a program-space bus cycle, and those cycles have to
//! be issued on their own clocks *before* the instruction's effect is applied.
//! So the count has to be known in advance, from the opcode word and nothing
//! else.
//!
//! # This is a second statement of something the executor already knows
//!
//! Which makes it exactly the shape that drifts. [`extension_words`] says what
//! `decode_ea` and the instruction bodies are about to do, and if the two ever
//! disagree the loader fetches the wrong number of words and the disagreement
//! shows up as a timing bug a long way from its cause.
//!
//! So it is not trusted: the per-cycle gate compares it against the words the
//! executor actually consumed, on every case of both corpora, and that check
//! runs before anything is built on this table. See
//! [`M68000::words_consumed`](super::M68000::words_consumed).
//!
//! The table was written from the opcode map and then corrected by that check,
//! which is the only order that works.

use super::addressing::Size;

/// Whether `opcode` is privileged on the 68000, so that executing it outside
/// supervisor mode takes a privilege violation *instead of* running.
///
/// A loader has to ask this before it fetches anything. The part settles
/// privilege at decode, before its first microcode step and before any
/// prefetch, so a privileged instruction executed in user mode consumes no
/// extension word however many its encoding names: `ORI to SR` in user mode
/// stacks a PC still pointing at its own immediate.
///
/// This was not visible in the documentation-derived corpus at all, whose cases
/// are supervisor-mode throughout. The microcode-derived corpus is half user
/// mode, and it disagreed with [`extension_words`] on 5,516 cases across
/// exactly these five encodings, which is what the table's cross-check is for.
///
/// `MOVE from SR` became privileged on the 68010 and is not listed: this
/// answers for the 68000, and the variant gate belongs with the rest of the
/// 68010 delta rather than buried here.
pub fn privileged(opcode: u16) -> bool {
    matches!(opcode, 0x007C | 0x027C | 0x0A7C)          // ORI/ANDI/EORI to SR
        || opcode & 0xFFC0 == 0x46C0                     // MOVE to SR
        || matches!(opcode, 0x4E70 | 0x4E72 | 0x4E73)    // RESET, STOP, RTE
        || (0x4E60..=0x4E6F).contains(&opcode) // MOVE USP
}

/// Extension words an effective address consumes, mirroring the cases in
/// `decode_ea_inner`.
fn ea_words(mode: u8, reg: u8, size: Size) -> u8 {
    match mode & 7 {
        // d16(An) and d8(An,Xn) each take one.
        5 | 6 => 1,
        7 => match reg & 7 {
            // abs.w, d16(PC), d8(PC,Xn)
            0 | 2 | 3 => 1,
            // abs.l
            1 => 2,
            // #imm: one word for byte and word, two for long
            4 => immediate_words(size),
            // 7.5 and above are unassigned on the 68000
            _ => 0,
        },
        // Dn, An, (An), (An)+, -(An) need none
        _ => 0,
    }
}

/// Words an immediate operand of `size` occupies in the instruction stream. A
/// byte immediate still takes a whole word, with the byte in its low half.
fn immediate_words(size: Size) -> u8 {
    match size {
        Size::Byte | Size::Word => 1,
        Size::Long => 2,
    }
}

/// The two-bit size field shared by the opmode and immediate encodings.
/// `11` is not a size; the callers that can see it handle it before asking.
fn size_from_bits(bits: u16) -> Size {
    match bits & 3 {
        0 => Size::Byte,
        1 => Size::Word,
        _ => Size::Long,
    }
}

/// How many words follow the opcode, for the instruction `opcode` encodes.
///
/// Counts the words the *executor consumes from the instruction stream*, which
/// is what the loader has to fetch, and is not always what a disassembler would
/// print: an encoding this core treats as a bounded no-op consumes nothing even
/// where the bit pattern looks like it names an addressing mode.
pub fn extension_words(opcode: u16) -> u8 {
    let opmode = (opcode >> 6) & 7;
    let ea_mode = ((opcode >> 3) & 7) as u8;
    let ea_reg = (opcode & 7) as u8;

    match (opcode >> 12) & 0xF {
        0x0 => line_0(opcode, opmode, ea_mode, ea_reg),
        // MOVE and MOVEA: source EA then destination EA, both at the
        // instruction's size. The destination's mode and register are the
        // other way round in the encoding.
        0x1..=0x3 => {
            let size = match (opcode >> 12) & 3 {
                1 => Size::Byte,
                3 => Size::Word,
                _ => Size::Long,
            };
            let dst_mode = ((opcode >> 6) & 7) as u8;
            let dst_reg = ((opcode >> 9) & 7) as u8;
            // A byte access to an address register is illegal and this core
            // treats it as a bounded no-op that consumes nothing.
            if size == Size::Byte && (ea_mode == 1 || dst_mode == 1) {
                return 0;
            }
            ea_words(ea_mode, ea_reg, size) + ea_words(dst_mode, dst_reg, size)
        }
        0x4 => line_4(opcode, ea_mode, ea_reg),
        0x5 => {
            if opmode & 3 == 3 {
                if ea_mode == 1 {
                    1 // DBcc takes a displacement word
                } else {
                    ea_words(ea_mode, ea_reg, Size::Byte) // Scc
                }
            } else {
                // ADDQ/SUBQ. An An destination is legal except at byte size,
                // and takes no extension word either way.
                ea_words(ea_mode, ea_reg, size_from_bits(opmode))
            }
        }
        // Bcc/BRA/BSR: an 8-bit displacement of zero selects the word form.
        // The 0xFF escape to a long displacement is 68020 and up.
        0x6 => u8::from(opcode & 0xFF == 0),
        // MOVEQ, and the unassigned half of line 7
        0x7 => 0,
        0x8 | 0x9 | 0xB | 0xC | 0xD => line_alu(opcode, opmode, ea_mode, ea_reg),
        // The one-bit memory shift form, which is size bits 11 with bit 11
        // clear. Every other line 0xE encoding is the register form, whose
        // shift count is in the opcode, or is unassigned.
        0xE if opmode & 3 == 3 && opcode & 0x0800 == 0 => ea_words(ea_mode, ea_reg, Size::Word),
        // Line A and line F vector through their own exceptions, the line 0xE
        // register form carries its count in the opcode, and the remaining
        // encodings are unassigned. None of them consumes a word.
        _ => 0,
    }
}

fn line_0(opcode: u16, opmode: u16, ea_mode: u8, ea_reg: u8) -> u8 {
    // ANDI/ORI/EORI to CCR and to SR each take one immediate word.
    if matches!(opcode, 0x003C | 0x007C | 0x023C | 0x027C | 0x0A3C | 0x0A7C) {
        return 1;
    }
    if opcode & 0x0100 != 0 {
        // Dynamic bit ops, with the bit number in Dn. EA mode 001 is MOVEP,
        // which takes a displacement word.
        return if ea_mode == 1 {
            1
        } else {
            bit_op_ea_words(opcode, ea_mode, ea_reg)
        };
    }
    if opcode & 0x0F00 == 0x0800 {
        // Static bit ops: the bit number is an extension word ahead of the EA.
        return 1 + bit_op_ea_words(opcode, ea_mode, ea_reg);
    }
    // ORI/ANDI/SUBI/ADDI/EORI/CMPI: the literal, then the destination EA.
    if opmode & 3 == 3 {
        return 0; // size 11 is unassigned here
    }
    let size = size_from_bits(opmode);
    immediate_words(size) + ea_words(ea_mode, ea_reg, size)
}

/// The destination EA of a bit operation.
///
/// A data register destination is a long operation and every other destination
/// is a byte one, but the distinction does not reach the word count: only an
/// immediate source would care about the size, and a bit op's destination is
/// never an immediate except for the dynamic `BTST`, whose immediate is a byte
/// in a single word.
fn bit_op_ea_words(opcode: u16, ea_mode: u8, ea_reg: u8) -> u8 {
    let is_btst = (opcode >> 6) & 3 == 0;
    let dynamic = opcode & 0x0100 != 0;
    // Mode 7 submodes this core rejects as a bounded no-op, matching op_bitop.
    let reg7_limit = match (is_btst, dynamic) {
        (false, _) => 2,
        (true, false) => 4,
        (true, true) => 5,
    };
    if ea_mode == 1 || (ea_mode == 7 && ea_reg >= reg7_limit) {
        return 0;
    }
    ea_words(ea_mode, ea_reg, Size::Byte)
}

fn line_4(opcode: u16, ea_mode: u8, ea_reg: u8) -> u8 {
    // CHK and LEA share the line by bits 8..6.
    if opcode & 0x01C0 == 0x0180 {
        return ea_words(ea_mode, ea_reg, Size::Word); // CHK
    }
    if opcode & 0x01C0 == 0x01C0 {
        return ea_words(ea_mode, ea_reg, Size::Long); // LEA
    }
    match (opcode >> 8) & 0xF {
        // MOVE from SR, and the NEGX group
        0x0 | 0x2 | 0x4 | 0x6 => {
            let size = if opcode & 0x00C0 == 0x00C0 {
                Size::Word // the SR/CCR moves are word-sized
            } else {
                size_from_bits(opcode >> 6)
            };
            ea_words(ea_mode, ea_reg, size)
        }
        0x8 => {
            if opcode & 0x00C0 == 0 {
                ea_words(ea_mode, ea_reg, Size::Byte) // NBCD
            } else if opcode & 0x00F8 == 0x0040 {
                0 // SWAP Dn
            } else if opcode & 0x00C0 == 0x0040 {
                ea_words(ea_mode, ea_reg, Size::Long) // PEA
            } else if opcode & 0x0038 == 0 && opcode & 0x0080 != 0 {
                0 // EXT
            } else if opcode & 0x0080 != 0 {
                1 + ea_words(ea_mode, ea_reg, Size::Word) // MOVEM store
            } else {
                0
            }
        }
        0xA => {
            if opcode == 0x4AFC {
                0 // ILLEGAL
            } else if opcode & 0x00C0 == 0x00C0 {
                ea_words(ea_mode, ea_reg, Size::Byte) // TAS
            } else {
                ea_words(ea_mode, ea_reg, size_from_bits(opcode >> 6)) // TST
            }
        }
        0xC if opcode & 0x0080 != 0 => 1 + ea_words(ea_mode, ea_reg, Size::Word), // MOVEM load
        0xE => match (opcode >> 6) & 3 {
            // JMP and JSR resolve a control-transfer EA.
            2 | 3 => ea_words(ea_mode, ea_reg, Size::Long),
            1 => match opcode {
                0x4E50..=0x4E57 => 1, // LINK takes a displacement word
                0x4E72 => 1,          // STOP takes its SR immediate
                _ => 0,
            },
            _ => 0,
        },
        _ => 0,
    }
}

fn line_alu(opcode: u16, opmode: u16, ea_mode: u8, ea_reg: u8) -> u8 {
    let line = (opcode >> 12) & 0xF;
    match opmode {
        // MULU/MULS and DIVU/DIVS on lines 8 and C, CMPA/ADDA/SUBA elsewhere.
        3 | 7 => {
            // Word for two unrelated reasons that happen to agree: the
            // multiply and divide sources on lines 8 and C are word operands
            // whichever opmode names them, and opmode 3 elsewhere is the word
            // form of ADDA, SUBA and CMPA. Only opmode 7 outside those two
            // lines reads a long.
            let size = if line == 0x8 || line == 0xC || opmode == 3 {
                Size::Word
            } else {
                Size::Long
            };
            ea_words(ea_mode, ea_reg, size)
        }
        4..=6 => {
            // The extended-arithmetic encodings take no extension word: ADDX,
            // SUBX, ABCD, SBCD and CMPM all name registers, and EXG likewise.
            let extended = match line {
                0x8 | 0xC => opmode == 4 && ea_mode < 2 || (5..=6).contains(&opmode) && ea_mode < 2,
                0x9 | 0xD => ea_mode < 2,
                0xB => ea_mode == 1, // CMPM
                _ => false,
            };
            if extended {
                return 0;
            }
            ea_words(ea_mode, ea_reg, size_from_bits(opmode))
        }
        _ => ea_words(ea_mode, ea_reg, size_from_bits(opmode)),
    }
}

/// Whether `opcode` consumes its extension words *without* refilling behind
/// them, because its next act is to discard the queue.
///
/// The recorded costs are what say so, and they are unambiguous: a taken `Bcc`
/// with a word displacement is ten clocks, which is two transfers, and both of
/// them are at the branch target. Refilling behind the displacement would make
/// it three transfers and twelve clocks. The words behind a control transfer
/// are on the path not taken and the part does not fetch them.
///
/// A loader has to ask this before it issues anything, because it is hoisting
/// exactly those fetches out of the instruction body. The four bodies that
/// declare it today are the branch family, `DBcc`, `JMP`/`JSR` and `STOP`.
pub fn suppresses_refill(opcode: u16) -> bool {
    match (opcode >> 12) & 0xF {
        // Bcc, BRA and BSR, whose word form takes a displacement word.
        0x6 => true,
        // DBcc is the size-11 encoding of line 5 with an An effective address.
        0x5 => (opcode >> 6) & 3 == 3 && (opcode >> 3) & 7 == 1,
        0x4 => match opcode {
            // STOP consumes its immediate and halts without refilling at all.
            0x4E72 => true,
            // JMP and JSR resolve a control-transfer address.
            0x4E80..=0x4EFF => matches!((opcode >> 6) & 3, 2 | 3),
            _ => false,
        },
        _ => false,
    }
}

/// The extended-arithmetic encodings, whose low six bits name registers rather
/// than an addressing mode: `ADDX`, `SUBX`, `ABCD`, `SBCD`, `CMPM` and `EXG`.
fn extended_arithmetic(opcode: u16) -> bool {
    let opmode = (opcode >> 6) & 7;
    let ea_mode = ((opcode >> 3) & 7) as u8;
    if !(4..=6).contains(&opmode) {
        return false;
    }
    match (opcode >> 12) & 0xF {
        0x8 | 0x9 | 0xC | 0xD => ea_mode < 2,
        0xB => ea_mode == 1, // CMPM
        _ => false,
    }
}

/// Whether `opcode` resolves an address *before* it fetches anything.
///
/// Most encodings do. Three groups do not, and they are the whole difficulty:
///
/// - The line 0 immediate forms fetch their literal first, so the destination's
///   address arithmetic lands between two fetches instead of in front of them.
/// - The static bit operations fetch their bit number the same way.
/// - `MOVEM` fetches its register mask the same way.
///
/// Everything else with an effective address resolves it first, `MOVE`'s source
/// included, and the instructions whose low six bits are a displacement, a
/// literal, a register pair or a vector number resolve nothing at all.
fn resolves_address_before_first_fetch(opcode: u16) -> bool {
    let opmode = (opcode >> 6) & 7;
    let ea_mode = ((opcode >> 3) & 7) as u8;
    match (opcode >> 12) & 0xF {
        0x0 => {
            // The CCR and SR immediates name no address, and the ALU
            // immediates and static bit ops fetch a word before resolving one.
            if opcode & 0x0100 == 0 {
                return false;
            }
            ea_mode != 1 // mode 1 here is MOVEP, whose An is not an EA
        }
        // MOVE and MOVEA resolve their source before fetching.
        0x1..=0x3 => true,
        0x4 => {
            // MOVEM fetches its mask first; the one-word specials, ILLEGAL,
            // SWAP and EXT name no address at all.
            !(opcode & 0xFB80 == 0x4880
                || (0x4E40..=0x4E7F).contains(&opcode)
                || opcode == 0x4AFC
                || opcode & 0x0FF8 == 0x0840
                || opcode & 0x0FB8 == 0x0880)
        }
        // DBcc's low bits are its counter register.
        0x5 => !(opmode & 3 == 3 && ea_mode == 1),
        // Branch displacements and MOVEQ literals.
        0x6 | 0x7 => false,
        0x8 | 0x9 | 0xB | 0xC | 0xD => !extended_arithmetic(opcode),
        // Only the memory shift form resolves an address.
        0xE => opmode & 3 == 3 && opcode & 0x0800 == 0,
        // Lines A and F carry a vector.
        _ => false,
    }
}

/// Clocks the part spends on address arithmetic before its first bus cycle.
///
/// Two addressing modes cost time no transfer hides: the predecrement, and the
/// index add that `d8(An,Xn)` and `d8(PC,Xn)` perform. The part spends them
/// immediately before the bus cycle the mode itself causes, which for an
/// instruction that resolves its address first is the front of the
/// instruction, and for one that fetches a literal first is *not*. A loader
/// burning this up front has to ask which, and that is the whole of
/// [`resolves_address_before_first_fetch`].
///
/// The extended-arithmetic memory forms are the other trap and go the other
/// way: `ADDX -(Ay),-(Ax)` encodes its operands with an EA field of 001, which
/// reads as a plain address register and resolves nothing, while the
/// instruction really does perform two predecrements and really does spend the
/// clocks. The encoding does not say what the instruction does.
///
/// # Why this one is not cross-checked the way its neighbors are
///
/// [`extension_words`] and [`suppresses_refill`] are asserted against what the
/// executor did, on every case of both corpora, before anything uses them.
/// This cannot be: it says *where* internal time sits, and the executor does
/// not place internal time anywhere. It charges the whole of it at the end, so
/// there is nothing to compare against and any check would be measuring the
/// shape of this core's code rather than the part's sequence.
///
/// An attempt at one was written and thrown away for exactly that reason. It
/// recorded the first effective address each instruction resolved, which agrees
/// with this table only where the implementation happens to resolve addresses
/// in the order the part does: it read 2 for a static bit operation and for
/// `MOVEM`, both of which fetch a word before resolving anything, and 0 for
/// the long `ADDX` memory form, which resolves its operands without going
/// through the shared decode at all.
///
/// It did earn its keep before being deleted. It found two real errors here
/// that survived a reading of the opcode map: `EXG`, whose `An` field is a
/// plain register, and `CMPM`, whose `An` field is a *post*increment.
///
/// **Rung 3 of the per-cycle gate is the check.** Internal time in the wrong
/// place moves transfers off the clocks the recording puts them on, so a wrong
/// row here makes that rung worse rather than better, on the cases it touches
/// and nowhere else.
pub fn leading_internal(opcode: u16) -> u8 {
    // A jump works its target out with the bus idle, and all of it precedes
    // the fetch, so its whole internal time is leading time. See
    // [`jump_internal`].
    if (0x4E80..=0x4EFF).contains(&opcode) {
        return jump_internal(opcode);
    }
    if extended_arithmetic(opcode) {
        // Only some of these predecrement, and the EA field cannot tell them
        // apart: an `An` field means `-(Ay)` for ADDX, SUBX, ABCD and SBCD,
        // `(Ay)+` for CMPM, and a plain register for EXG.
        let predecrements = match (opcode >> 12) & 0xF {
            0x9 | 0xD => (opcode >> 3) & 7 == 1, // SUBX, ADDX
            0x8 | 0xC => (opcode >> 6) & 7 == 4 && (opcode >> 3) & 7 == 1, // SBCD, ABCD
            _ => false,                          // CMPM postincrements; EXG does neither
        };
        return if predecrements { 2 } else { 0 };
    }
    if !resolves_address_before_first_fetch(opcode) {
        return 0;
    }
    match ((opcode >> 3) & 7, opcode & 7) {
        (4, _) | (6, _) => 2, // -(An) and d8(An,Xn)
        (7, 3) => 2,          // d8(PC,Xn)
        _ => 0,
    }
}

/// Clocks `JMP` or `JSR` spends off the bus working its target out, all of
/// which precede its first fetch.
///
/// **A jump pays for address arithmetic that every other instruction gets
/// free**, and the reason is visible one microcode step at a time.
/// `LEA (d16,An),An` adds the displacement to the address register in the same
/// step that puts the *next* fetch's address on the bus, so the add costs
/// nothing. `JMP (d16,An)` cannot: the address that add produces **is** the
/// next fetch's address, so the step that computes it cannot also drive it, and
/// it costs a step. Two clocks for a displacement add, two more for an index
/// add, two for sign-extending an 8-bit displacement or a short absolute
/// address, and nothing at all for a long absolute address, which arrives ready
/// to use in its two extension words.
///
/// This is one function rather than a constant in the loader and another in the
/// body, because the two have to agree exactly: the loader burns this in front
/// of the instruction and the finish charges what is left, and a disagreement
/// would show up as a clock the instruction never spends or spends twice.
///
/// Illegal modes read zero. `JMP D0` is not a jump and the body treats it as a
/// bounded no-op, so burning time in front of it would be time the part does
/// not spend.
pub fn jump_internal(opcode: u16) -> u8 {
    let mode = ((opcode >> 3) & 7) as u8;
    let reg = (opcode & 7) as u8;
    match mode {
        2 => 0, // (An)
        5 => 2, // d16(An)
        6 => 6, // d8(An,Xn)
        7 => match reg {
            0 => 2, // abs.w
            1 => 0, // abs.l
            2 => 2, // d16(PC)
            3 => 6, // d8(PC,Xn)
            _ => 0, // not a control mode
        },
        _ => 0, // register direct, postincrement, predecrement: not control
    }
}

/// How many words the instruction consumes *before* it touches an operand.
///
/// This is what decides whether a loader may issue the refill behind the opcode
/// in front of the instruction body. The refill is not owed to the opcode as
/// such: the part fetches it while taking the next word, so an instruction that
/// reads an operand before taking another word has not refilled anything by the
/// time that read happens, and an instruction that faults on that read never
/// refills at all.
///
/// For every encoding but `MOVE` the answer is all of them, because the
/// extension words, the immediate and `MOVEM`'s mask are all consumed before the
/// first operand cycle. `MOVE` is the one instruction with two effective
/// addresses, and its destination's words are consumed *after* the source has
/// been read, so only the source's count here.
///
/// Getting this wrong is visible and was: counting `MOVE`'s destination words
/// put an extra read in front of the exception frame on 1,023 recorded cases
/// that fault on a source read, which is a transfer the part had not made.
pub fn words_before_operand(opcode: u16) -> u8 {
    match (opcode >> 12) & 0xF {
        0x1..=0x3 => {
            let size = match (opcode >> 12) & 3 {
                1 => Size::Byte,
                3 => Size::Word,
                _ => Size::Long,
            };
            let src_mode = ((opcode >> 3) & 7) as u8;
            let src_reg = (opcode & 7) as u8;
            let dst_mode = ((opcode >> 6) & 7) as u8;
            // A byte access to an address register is illegal and consumes
            // nothing, matching `extension_words`.
            if size == Size::Byte && (src_mode == 1 || dst_mode == 1) {
                return 0;
            }
            ea_words(src_mode, src_reg, size)
        }
        _ => extension_words(opcode),
    }
}
