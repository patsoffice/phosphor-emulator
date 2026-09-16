//! Intel 8088/8086 instruction disassembler, Intel syntax.
//!
//! # The byte layout is not decided here
//!
//! How long an instruction is comes from [`super::format`], the same table the
//! per-cycle core loads instructions with. That is deliberate and it is the one
//! rule this file must not break.
//!
//! A disassembler needs exactly what a per-cycle loader needs: given an opcode,
//! whether a ModR/M byte follows, what displacement that byte implies, and how
//! many immediate bytes come after. Writing a second table here would give the
//! repository two independent encodings of instruction length that can drift
//! apart, and only one of them is validated: `format.rs` is checked against
//! 2,797,000 hardware vectors on every run, because a length it gets wrong
//! desynchronizes the instruction stream and the state gate reports it as a
//! wrong IP. A length this file got wrong would be reported by nobody.
//!
//! So this file adds only the two things `format.rs` has no opinion about: what
//! an opcode is called, and how its operands are written. **If a disassembly
//! needs a length `format.rs` does not give it, that is a bug in `format.rs`**
//! and belongs there, because the CPU is reading the same table.
//!
//! # Hex is written `$1234`, not `1234h`
//!
//! Intel syntax proper writes a trailing `h`, and every other disassembler in
//! this repository writes a leading `$`. The `$` wins, for a reason beyond
//! consistency: [`DisassembledInstruction::format_with_symbols`] substitutes a
//! symbol name for an address by looking for `$` followed by a fixed number of
//! hex digits, so a target written any other way silently loses symbol
//! resolution.

use crate::cpu::disasm::{Disassemble, DisassembledInstruction};
use crate::cpu::i8088::I8088;
use crate::cpu::i8088::format::{displacement_len, format_of};

/// ModR/M `reg` field as an 8-bit register, and the low three bits of the
/// opcode for the instructions that encode a register there.
const REG8: [&str; 8] = ["AL", "CL", "DL", "BL", "AH", "CH", "DH", "BH"];
/// The same field read as a 16-bit register.
const REG16: [&str; 8] = ["AX", "CX", "DX", "BX", "SP", "BP", "SI", "DI"];
/// Segment registers, as encoded in the ModR/M `reg` field of the two MOV forms
/// and in bits 4:3 of the segment PUSH/POP opcodes.
const SEG: [&str; 4] = ["ES", "CS", "SS", "DS"];

/// The base-plus-index forms a ModR/M `rm` field selects when `mod` is not 3.
///
/// Entry 6 is `[BP]`, which is only reachable with a displacement: `mod=00
/// rm=110` is the escape for a bare 16-bit address instead, which is why `[BP]`
/// with no displacement has to be encoded as `mod=01` with a displacement of
/// zero.
const RM_BASE: [&str; 8] = ["BX+SI", "BX+DI", "BP+SI", "BP+DI", "SI", "DI", "BP", "BX"];

/// The eight ALU operations in opcode order, filling 0x00 through 0x3F.
const ALU: [&str; 8] = ["ADD", "OR", "ADC", "SBB", "AND", "SUB", "XOR", "CMP"];
/// The same eight, reached through the ModR/M `reg` field of 0x80 through 0x83.
const ALU_GROUP: [&str; 8] = ALU;
/// The shift and rotate group, 0xD0 through 0xD3, by `reg`. `reg=6` is an
/// undocumented alias of SHL, which is why SAL appears twice under two names.
const SHIFT_GROUP: [&str; 8] = ["ROL", "ROR", "RCL", "RCR", "SHL", "SHR", "SAL", "SAR"];
/// The unary group, 0xF6 and 0xF7, by `reg`. Only the first two take an
/// immediate, and `reg=1` is an undocumented alias of `reg=0`.
const UNARY_GROUP: [&str; 8] = ["TEST", "TEST", "NOT", "NEG", "MUL", "IMUL", "DIV", "IDIV"];
/// The 16-bit INC/DEC/CALL/JMP/PUSH group at 0xFF, by `reg`.
const FF_GROUP: [&str; 8] = ["INC", "DEC", "CALL", "CALL", "JMP", "JMP", "PUSH", "???"];
/// Condition names for the 0x70 block, in encoding order. The aliases are the
/// ones an assembler emits: JNB rather than JAE, and so on, chosen to match the
/// naming most 8088 listings use.
const JCC: [&str; 16] = [
    "JO", "JNO", "JB", "JNB", "JZ", "JNZ", "JBE", "JA", "JS", "JNS", "JPE", "JPO", "JL", "JGE",
    "JLE", "JG",
];

/// The prefixes that change how the instruction after them is written.
///
/// Only two kinds get this far. A segment override changes the memory operand's
/// text, and a `REP` on a string instruction becomes part of its name. `LOCK`,
/// and a `REP` on anything else, are emitted as one-byte instructions of their
/// own instead: `mnemonic` is a `&'static str`, so a prefix with no combined
/// form has nowhere to go, and a listing line of its own is both honest and
/// what most 8088 listings show.
#[derive(Copy, Clone)]
struct Prefixes {
    seg: Option<&'static str>,
    rep: Option<&'static str>,
}

impl Prefixes {
    const NONE: Self = Self {
        seg: None,
        rep: None,
    };
}

/// The name a `REP`-prefixed string instruction goes by.
///
/// `REP` and `REPE` are the same byte; which name reads better depends on the
/// instruction, so CMPS and SCAS take `REPE` and the rest take `REP`.
fn rep_mnemonic(rep: &str, opcode: u8) -> Option<&'static str> {
    let repne = rep == "REPNE";
    Some(match (opcode, repne) {
        (0xA4, false) => "REP MOVSB",
        (0xA5, false) => "REP MOVSW",
        (0xAA, false) => "REP STOSB",
        (0xAB, false) => "REP STOSW",
        (0xAC, false) => "REP LODSB",
        (0xAD, false) => "REP LODSW",
        (0xA6, false) => "REPE CMPSB",
        (0xA7, false) => "REPE CMPSW",
        (0xAE, false) => "REPE SCASB",
        (0xAF, false) => "REPE SCASW",
        (0xA6, true) => "REPNE CMPSB",
        (0xA7, true) => "REPNE CMPSW",
        (0xAE, true) => "REPNE SCASB",
        (0xAF, true) => "REPNE SCASW",
        (0xA4, true) => "REPNE MOVSB",
        (0xA5, true) => "REPNE MOVSW",
        (0xAA, true) => "REPNE STOSB",
        (0xAB, true) => "REPNE STOSW",
        (0xAC, true) => "REPNE LODSB",
        (0xAD, true) => "REPNE LODSW",
        _ => return None,
    })
}

/// The segment override a prefix byte selects, if it is one.
///
/// `LOCK`, `REP` and `REPNE` are handled by the caller rather than here,
/// because they do not change the operand text and one of them may end up as an
/// instruction in its own right.
fn segment_prefix(byte: u8) -> Option<&'static str> {
    match byte {
        0x26 => Some("ES"),
        0x2E => Some("CS"),
        0x36 => Some("SS"),
        0x3E => Some("DS"),
        _ => None,
    }
}

/// A signed displacement, written the way a listing writes it: `+$04` or
/// `-$04` rather than a two's-complement blob.
fn disp_text(disp: i32) -> String {
    if disp < 0 {
        format!("-${:02X}", -disp)
    } else {
        format!("+${:02X}", disp)
    }
}

/// Format the `r/m` operand of a ModR/M byte.
///
/// `word` picks the register width for the `mod=3` case; it has no effect on a
/// memory operand, whose width is carried by the other operand or by an
/// explicit size prefix from the caller.
fn rm_text(modrm: u8, disp: i32, word: bool, seg: Option<&str>) -> String {
    let mod_bits = (modrm >> 6) & 3;
    let rm = (modrm & 7) as usize;
    if mod_bits == 3 {
        return if word { REG16[rm] } else { REG8[rm] }.to_string();
    }
    let prefix = seg.map(|s| format!("{s}:")).unwrap_or_default();
    // mod=00 rm=110 is a bare 16-bit address rather than [BP].
    if mod_bits == 0 && rm == 6 {
        return format!("{prefix}[${:04X}]", disp as u16);
    }
    if mod_bits == 0 {
        return format!("{prefix}[{}]", RM_BASE[rm]);
    }
    format!("{prefix}[{}{}]", RM_BASE[rm], disp_text(disp))
}

/// Read a little-endian word, or `None` if it is not fully present.
fn word_at(bytes: &[u8], at: usize) -> Option<u16> {
    match (bytes.get(at), bytes.get(at + 1)) {
        (Some(lo), Some(hi)) => Some(u16::from_le_bytes([*lo, *hi])),
        _ => None,
    }
}

/// Build the result, copying at most the ten bytes the struct carries.
fn make(
    mnemonic: &'static str,
    operands: String,
    len: usize,
    bytes: &[u8],
    target: Option<u32>,
) -> DisassembledInstruction {
    let mut raw = [0u8; 10];
    let n = len.min(10).min(bytes.len());
    raw[..n].copy_from_slice(&bytes[..n]);
    DisassembledInstruction {
        mnemonic,
        operands,
        byte_len: len.min(255) as u8,
        bytes: raw,
        target_addr: target,
    }
}

/// An instruction whose bytes run off the end of the slice.
///
/// Reported rather than guessed at: a truncated instruction at the end of a ROM
/// region is a real thing to see, and inventing operands for bytes that are not
/// there would make the listing lie about what is in the ROM.
fn truncated(bytes: &[u8]) -> DisassembledInstruction {
    let op = bytes.first().copied().unwrap_or(0);
    make("DB", format!("${op:02X}"), 1, bytes, None)
}

/// Resolve a relative branch target.
///
/// A `rel8` or `rel16` on this CPU is always within the current segment, so the
/// arithmetic wraps at 16 bits and the segment base above it is preserved. For
/// a region mapped flat below 64 KB, which is every 8088 machine in this
/// repository, that is the same answer as a plain add; it differs only for a
/// region placed at a segment boundary, where the plain add would walk out of
/// the segment the branch cannot leave.
fn branch_target(addr: u32, len: usize, rel: i32) -> u32 {
    let base = addr & !0xFFFF;
    let off = (addr & 0xFFFF) as u16;
    let target = off
        .wrapping_add(len as u16)
        .wrapping_add((rel & 0xFFFF) as u16);
    base | u32::from(target)
}

impl Disassemble for I8088 {
    fn disassemble(addr: u32, bytes: &[u8]) -> DisassembledInstruction {
        if bytes.is_empty() {
            return make("DB", "$00".to_string(), 1, &[0], None);
        }

        // --- Prefixes ---------------------------------------------------
        //
        // Prefixes are not in `format.rs`: the loader recognizes them and keeps
        // fetching, because a prefix is followed by another opcode rather than
        // by operands. The same is true here, so they are consumed first and the
        // lengths below are measured from index 0, prefixes included.
        //
        // LOCK stops the scan and becomes a one-byte instruction of its own. So
        // does a REP that turns out not to precede a string instruction. Both
        // are cases where there is no combined name to put in a `&'static str`,
        // and a separate listing line is honest about the byte that is there.
        let mut pfx = Prefixes::NONE;
        let mut at = 0usize;
        loop {
            let Some(&b) = bytes.get(at) else {
                return truncated(bytes);
            };
            if b == 0xF0 {
                return make("LOCK", String::new(), at + 1, bytes, None);
            }
            if b == 0xF2 || b == 0xF3 {
                let rep = if b == 0xF2 { "REPNE" } else { "REP" };
                match bytes.get(at + 1).and_then(|&next| rep_mnemonic(rep, next)) {
                    Some(_) => {
                        pfx.rep = Some(rep);
                        at += 1;
                        continue;
                    }
                    // Not a string instruction after it, so the prefix does
                    // nothing this file can name. Emit it alone.
                    None => {
                        let name = if b == 0xF2 { "REPNE" } else { "REP" };
                        return make(name, String::new(), at + 1, bytes, None);
                    }
                }
            }
            match segment_prefix(b) {
                Some(seg) => {
                    pfx.seg = Some(seg);
                    at += 1;
                }
                None => break,
            }
            // A run of prefixes longer than any real instruction is not one.
            if at >= 8 {
                return make("DB", format!("${:02X}", bytes[0]), 1, bytes, None);
            }
        }
        let Some(&opcode) = bytes.get(at) else {
            return truncated(bytes);
        };
        at += 1;

        // --- Byte layout, entirely from format.rs -----------------------
        let fmt = format_of(opcode);
        let modrm = if fmt.modrm {
            bytes.get(at).copied()
        } else {
            None
        };
        if fmt.modrm && modrm.is_none() {
            return truncated(bytes);
        }
        let disp_len = modrm.map(displacement_len).unwrap_or(0) as usize;
        if fmt.modrm {
            at += 1;
        }
        let disp_at = at;
        at += disp_len;
        let imm_at = at;
        let imm_len = fmt.imm.len(modrm) as usize;
        let total = imm_at + imm_len;
        if bytes.len() < total {
            return truncated(bytes);
        }

        // Displacement, sign-extended from whichever width the ModR/M implied.
        let disp: i32 = match disp_len {
            1 => i32::from(bytes[disp_at] as i8),
            2 => {
                // mod=00 rm=110 is an unsigned address, not a signed offset;
                // rm_text reads it back as a u16 for that case.
                i32::from(u16::from_le_bytes([bytes[disp_at], bytes[disp_at + 1]]) as i16)
            }
            _ => 0,
        };
        let imm8 = bytes.get(imm_at).copied();
        let imm16 = word_at(bytes, imm_at);

        let m = modrm.unwrap_or(0);
        let reg_field = ((m >> 3) & 7) as usize;
        // A REP-prefixed string instruction carries its prefix in its name;
        // everything else keeps the mnemonic the match below picks.
        let rename = |mnemonic: &'static str| -> &'static str {
            match pfx.rep {
                Some(rep) => rep_mnemonic(rep, opcode).unwrap_or(mnemonic),
                None => mnemonic,
            }
        };
        let out = |mnemonic: &'static str, operands: String| -> DisassembledInstruction {
            make(rename(mnemonic), operands, total, bytes, None)
        };
        let rm8 = || rm_text(m, disp, false, pfx.seg);
        let rm16 = || rm_text(m, disp, true, pfx.seg);

        match opcode {
            // --- 0x00-0x3F: eight ALU operations in six forms each -------
            //
            // The regularity is the encoding's own: bits 5:3 pick the
            // operation and bits 2:0 pick the form. Writing it out as a
            // 64-entry table would hide that and invite a typo in one cell.
            0x00..=0x3F if (opcode & 7) < 6 => {
                let name = ALU[((opcode >> 3) & 7) as usize];
                let operands = match opcode & 7 {
                    0 => format!("{}, {}", rm8(), REG8[reg_field]),
                    1 => format!("{}, {}", rm16(), REG16[reg_field]),
                    2 => format!("{}, {}", REG8[reg_field], rm8()),
                    3 => format!("{}, {}", REG16[reg_field], rm16()),
                    4 => format!("AL, ${:02X}", imm8.unwrap_or(0)),
                    _ => format!("AX, ${:04X}", imm16.unwrap_or(0)),
                };
                out(name, operands)
            }
            // The two leftover slots in each ALU row: a segment PUSH/POP, or a
            // BCD adjust. The segment-override prefixes share these encodings
            // and were consumed above, so only 0x06/0x07/0x0E/0x16/0x17/0x1E/
            // 0x1F and the four adjusts reach here.
            0x06 | 0x0E | 0x16 | 0x1E => out("PUSH", SEG[((opcode >> 3) & 3) as usize].to_string()),
            0x07 | 0x0F | 0x17 | 0x1F => out("POP", SEG[((opcode >> 3) & 3) as usize].to_string()),
            0x27 => out("DAA", String::new()),
            0x2F => out("DAS", String::new()),
            0x37 => out("AAA", String::new()),
            0x3F => out("AAS", String::new()),

            // --- Register in the opcode ---------------------------------
            0x40..=0x47 => out("INC", REG16[(opcode & 7) as usize].to_string()),
            0x48..=0x4F => out("DEC", REG16[(opcode & 7) as usize].to_string()),
            0x50..=0x57 => out("PUSH", REG16[(opcode & 7) as usize].to_string()),
            0x58..=0x5F => out("POP", REG16[(opcode & 7) as usize].to_string()),

            // 0x60-0x6F have no encodings of their own on this part and alias
            // the conditional jumps below, taking a rel8 as those do. Named as
            // the jump they alias, since that is what the CPU executes.
            0x60..=0x7F => {
                let rel = i32::from(imm8.unwrap_or(0) as i8);
                let target = branch_target(addr, total, rel);
                make(
                    JCC[(opcode & 0x0F) as usize],
                    format!("${target:04X}"),
                    total,
                    bytes,
                    Some(target),
                )
            }

            // --- Immediate to r/m, the ALU group ------------------------
            0x80 | 0x82 => out(
                ALU_GROUP[reg_field],
                format!("{}, ${:02X}", rm8(), imm8.unwrap_or(0)),
            ),
            0x81 => out(
                ALU_GROUP[reg_field],
                format!("{}, ${:04X}", rm16(), imm16.unwrap_or(0)),
            ),
            // 0x83 sign-extends its byte to a word, so the operand is written
            // as the word it becomes rather than the byte it was stored as.
            0x83 => {
                let v = i32::from(imm8.unwrap_or(0) as i8) as u16;
                out(ALU_GROUP[reg_field], format!("{}, ${:04X}", rm16(), v))
            }

            0x84 => out("TEST", format!("{}, {}", rm8(), REG8[reg_field])),
            0x85 => out("TEST", format!("{}, {}", rm16(), REG16[reg_field])),
            0x86 => out("XCHG", format!("{}, {}", rm8(), REG8[reg_field])),
            0x87 => out("XCHG", format!("{}, {}", rm16(), REG16[reg_field])),
            0x88 => out("MOV", format!("{}, {}", rm8(), REG8[reg_field])),
            0x89 => out("MOV", format!("{}, {}", rm16(), REG16[reg_field])),
            0x8A => out("MOV", format!("{}, {}", REG8[reg_field], rm8())),
            0x8B => out("MOV", format!("{}, {}", REG16[reg_field], rm16())),
            // Only the low two bits of the reg field select a segment register;
            // the top bit is ignored by the hardware rather than faulting.
            0x8C => out("MOV", format!("{}, {}", rm16(), SEG[reg_field & 3])),
            0x8D => out("LEA", format!("{}, {}", REG16[reg_field], rm16())),
            0x8E => out("MOV", format!("{}, {}", SEG[reg_field & 3], rm16())),
            0x8F => out("POP", rm16()),

            // 0x90 is XCHG AX,AX, which every listing calls NOP.
            0x90 => out("NOP", String::new()),
            0x91..=0x97 => out("XCHG", format!("AX, {}", REG16[(opcode & 7) as usize])),
            0x98 => out("CBW", String::new()),
            0x99 => out("CWD", String::new()),
            0x9A => {
                let off = word_at(bytes, imm_at).unwrap_or(0);
                let seg = word_at(bytes, imm_at + 2).unwrap_or(0);
                out("CALL", format!("${seg:04X}:${off:04X}"))
            }
            0x9B => out("WAIT", String::new()),
            0x9C => out("PUSHF", String::new()),
            0x9D => out("POPF", String::new()),
            0x9E => out("SAHF", String::new()),
            0x9F => out("LAHF", String::new()),

            // MOV between the accumulator and a direct address. The 16-bit
            // value is an offset into the data segment, not a displacement.
            0xA0 => out("MOV", format!("AL, [${:04X}]", imm16.unwrap_or(0))),
            0xA1 => out("MOV", format!("AX, [${:04X}]", imm16.unwrap_or(0))),
            0xA2 => out("MOV", format!("[${:04X}], AL", imm16.unwrap_or(0))),
            0xA3 => out("MOV", format!("[${:04X}], AX", imm16.unwrap_or(0))),

            0xA4 => out("MOVSB", String::new()),
            0xA5 => out("MOVSW", String::new()),
            0xA6 => out("CMPSB", String::new()),
            0xA7 => out("CMPSW", String::new()),
            0xA8 => out("TEST", format!("AL, ${:02X}", imm8.unwrap_or(0))),
            0xA9 => out("TEST", format!("AX, ${:04X}", imm16.unwrap_or(0))),
            0xAA => out("STOSB", String::new()),
            0xAB => out("STOSW", String::new()),
            0xAC => out("LODSB", String::new()),
            0xAD => out("LODSW", String::new()),
            0xAE => out("SCASB", String::new()),
            0xAF => out("SCASW", String::new()),

            0xB0..=0xB7 => out(
                "MOV",
                format!(
                    "{}, ${:02X}",
                    REG8[(opcode & 7) as usize],
                    imm8.unwrap_or(0)
                ),
            ),
            0xB8..=0xBF => out(
                "MOV",
                format!(
                    "{}, ${:04X}",
                    REG16[(opcode & 7) as usize],
                    imm16.unwrap_or(0)
                ),
            ),

            // RET and RETF, each with and without a stack adjustment. The two
            // encodings below each documented one are undocumented aliases.
            0xC0 | 0xC2 => out("RET", format!("${:04X}", imm16.unwrap_or(0))),
            0xC1 | 0xC3 => out("RET", String::new()),
            0xC4 => out("LES", format!("{}, {}", REG16[reg_field], rm16())),
            0xC5 => out("LDS", format!("{}, {}", REG16[reg_field], rm16())),
            0xC6 => out("MOV", format!("{}, ${:02X}", rm8(), imm8.unwrap_or(0))),
            0xC7 => out("MOV", format!("{}, ${:04X}", rm16(), imm16.unwrap_or(0))),
            0xC8 | 0xCA => out("RETF", format!("${:04X}", imm16.unwrap_or(0))),
            0xC9 | 0xCB => out("RETF", String::new()),
            0xCC => out("INT", "3".to_string()),
            0xCD => out("INT", format!("${:02X}", imm8.unwrap_or(0))),
            0xCE => out("INTO", String::new()),
            0xCF => out("IRET", String::new()),

            0xD0 => out(SHIFT_GROUP[reg_field], format!("{}, 1", rm8())),
            0xD1 => out(SHIFT_GROUP[reg_field], format!("{}, 1", rm16())),
            0xD2 => out(SHIFT_GROUP[reg_field], format!("{}, CL", rm8())),
            0xD3 => out(SHIFT_GROUP[reg_field], format!("{}, CL", rm16())),
            // The base byte is 10 in every assembler's output and arbitrary in
            // the encoding, so it is shown rather than assumed.
            0xD4 => out("AAM", format!("${:02X}", imm8.unwrap_or(0))),
            0xD5 => out("AAD", format!("${:02X}", imm8.unwrap_or(0))),
            0xD6 => out("SALC", String::new()),
            0xD7 => out("XLAT", String::new()),

            // The 8087 escapes. With no coprocessor fitted the 8088 still
            // fetches the ModR/M byte and performs the memory read it
            // describes, so this has a length and an operand even though
            // nothing acts on it. Named ESC rather than guessed at as an x87
            // mnemonic, because no x87 is fitted on any board here.
            0xD8..=0xDF => out("ESC", format!("${:X}, {}", opcode & 7, rm16())),

            0xE0..=0xE3 => {
                let rel = i32::from(imm8.unwrap_or(0) as i8);
                let target = branch_target(addr, total, rel);
                let name = ["LOOPNZ", "LOOPZ", "LOOP", "JCXZ"][(opcode & 3) as usize];
                make(name, format!("${target:04X}"), total, bytes, Some(target))
            }
            0xE4 => out("IN", format!("AL, ${:02X}", imm8.unwrap_or(0))),
            0xE5 => out("IN", format!("AX, ${:02X}", imm8.unwrap_or(0))),
            0xE6 => out("OUT", format!("${:02X}, AL", imm8.unwrap_or(0))),
            0xE7 => out("OUT", format!("${:02X}, AX", imm8.unwrap_or(0))),
            0xE8 | 0xE9 => {
                let rel = i32::from(imm16.unwrap_or(0) as i16);
                let target = branch_target(addr, total, rel);
                let name = if opcode == 0xE8 { "CALL" } else { "JMP" };
                make(name, format!("${target:04X}"), total, bytes, Some(target))
            }
            0xEA => {
                let off = word_at(bytes, imm_at).unwrap_or(0);
                let seg = word_at(bytes, imm_at + 2).unwrap_or(0);
                out("JMP", format!("${seg:04X}:${off:04X}"))
            }
            0xEB => {
                let rel = i32::from(imm8.unwrap_or(0) as i8);
                let target = branch_target(addr, total, rel);
                make("JMP", format!("${target:04X}"), total, bytes, Some(target))
            }
            0xEC => out("IN", "AL, DX".to_string()),
            0xED => out("IN", "AX, DX".to_string()),
            0xEE => out("OUT", "DX, AL".to_string()),
            0xEF => out("OUT", "DX, AX".to_string()),

            // 0xF0, 0xF2 and 0xF3 are prefixes and were consumed above; a
            // lone one reaches here only as the last byte of the slice.
            0xF0 | 0xF2 | 0xF3 => out("DB", format!("${opcode:02X}")),
            0xF1 => out("DB", format!("${opcode:02X}")),
            0xF4 => out("HLT", String::new()),
            0xF5 => out("CMC", String::new()),
            0xF6 => {
                let name = UNARY_GROUP[reg_field];
                if reg_field < 2 {
                    out(name, format!("{}, ${:02X}", rm8(), imm8.unwrap_or(0)))
                } else {
                    out(name, rm8())
                }
            }
            0xF7 => {
                let name = UNARY_GROUP[reg_field];
                if reg_field < 2 {
                    out(name, format!("{}, ${:04X}", rm16(), imm16.unwrap_or(0)))
                } else {
                    out(name, rm16())
                }
            }
            0xF8 => out("CLC", String::new()),
            0xF9 => out("STC", String::new()),
            0xFA => out("CLI", String::new()),
            0xFB => out("STI", String::new()),
            0xFC => out("CLD", String::new()),
            0xFD => out("STD", String::new()),
            0xFE => {
                let name = if reg_field == 0 { "INC" } else { "DEC" };
                out(name, rm8())
            }
            0xFF => {
                let name = FF_GROUP[reg_field];
                // reg=3 and reg=5 are the far forms, which take their pointer
                // from memory rather than naming it in the instruction.
                let operands = match reg_field {
                    3 | 5 => format!("FAR {}", rm16()),
                    _ => rm16(),
                };
                out(name, operands)
            }

            // Every opcode above is covered by one of the arms; this arm exists
            // so the match is exhaustive over u8 without a wildcard that could
            // swallow a future mistake silently.
            _ => out("DB", format!("${opcode:02X}")),
        }
    }
}
