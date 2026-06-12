//! Motorola 68000 instruction disassembler.
//!
//! Mirrors the two-level line dispatch of `execute_instruction` in
//! `mod.rs` so every implemented encoding disassembles and every
//! unassigned encoding renders as `DC.W $xxxx` (the assembler idiom for
//! "this word is data"). Operands use Motorola syntax with sized
//! mnemonics (`MOVE.W D0,$1234(A0)`); PC-relative operands display their
//! resolved absolute address.

use crate::cpu::disasm::{Disassemble, DisassembledInstruction};
use crate::cpu::m68000::M68000;

/// Operand size, decoded per-encoding (the 68000 has three size-field
/// conventions; callers pick the right one).
#[derive(Clone, Copy, PartialEq)]
enum Sz {
    B,
    W,
    L,
}

/// Big-endian word reader over the instruction byte slice. `pos` is the
/// running byte offset; it becomes `byte_len` when decoding completes.
struct Cur<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cur<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn word(&mut self) -> Option<u16> {
        let hi = *self.bytes.get(self.pos)?;
        let lo = *self.bytes.get(self.pos + 1)?;
        self.pos += 2;
        Some(u16::from_be_bytes([hi, lo]))
    }

    fn long(&mut self) -> Option<u32> {
        let hi = self.word()?;
        let lo = self.word()?;
        Some((u32::from(hi) << 16) | u32::from(lo))
    }
}

/// Build the result, copying the consumed raw bytes.
fn make(
    mnemonic: &'static str,
    operands: String,
    byte_len: usize,
    raw: &[u8],
    target_addr: Option<u32>,
) -> DisassembledInstruction {
    let mut bytes = [0u8; 10];
    let n = byte_len.min(raw.len()).min(10);
    bytes[..n].copy_from_slice(&raw[..n]);
    DisassembledInstruction {
        mnemonic,
        operands,
        byte_len: byte_len as u8,
        bytes,
        target_addr,
    }
}

/// Truncated decode: the byte slice ran out mid-instruction. One opcode
/// word so a disassembly scan stays word-aligned.
fn truncated(raw: &[u8]) -> DisassembledInstruction {
    make("???", String::new(), 2, raw, None)
}

/// Unassigned encoding: render the opcode word as data.
fn dc_w(opcode: u16, raw: &[u8]) -> DisassembledInstruction {
    make("DC.W", format!("${opcode:04X}"), 2, raw, None)
}

/// Attach the size suffix to a base mnemonic as a static string.
fn sized(base: &'static str, sz: Sz) -> &'static str {
    macro_rules! table {
        ($($n:literal),* $(,)?) => {
            match (base, sz) {
                $(
                    ($n, Sz::B) => concat!($n, ".B"),
                    ($n, Sz::W) => concat!($n, ".W"),
                    ($n, Sz::L) => concat!($n, ".L"),
                )*
                _ => base,
            }
        };
    }
    table!(
        "MOVE", "MOVEA", "MOVEP", "MOVEM", "ADD", "ADDA", "ADDI", "ADDQ", "ADDX", "SUB", "SUBA",
        "SUBI", "SUBQ", "SUBX", "CMP", "CMPA", "CMPI", "CMPM", "AND", "ANDI", "OR", "ORI", "EOR",
        "EORI", "NEG", "NEGX", "NOT", "CLR", "TST", "EXT", "ASL", "ASR", "LSL", "LSR", "ROL",
        "ROR", "ROXL", "ROXR", "DIVU", "DIVS", "MULU", "MULS", "CHK",
    )
}

/// The standard size field (bits used by MOVE use a different code).
fn size_bits(bits: u16) -> Option<Sz> {
    match bits & 3 {
        0 => Some(Sz::B),
        1 => Some(Sz::W),
        2 => Some(Sz::L),
        _ => None,
    }
}

/// Bcc mnemonics by condition field (0/1 are BRA/BSR on line 6).
const BCC: [&str; 16] = [
    "BRA", "BSR", "BHI", "BLS", "BCC", "BCS", "BNE", "BEQ", "BVC", "BVS", "BPL", "BMI", "BGE",
    "BLT", "BGT", "BLE",
];

/// Scc mnemonics by condition field.
const SCC: [&str; 16] = [
    "ST", "SF", "SHI", "SLS", "SCC", "SCS", "SNE", "SEQ", "SVC", "SVS", "SPL", "SMI", "SGE", "SLT",
    "SGT", "SLE",
];

/// DBcc mnemonics by condition field.
const DBCC: [&str; 16] = [
    "DBT", "DBF", "DBHI", "DBLS", "DBCC", "DBCS", "DBNE", "DBEQ", "DBVC", "DBVS", "DBPL", "DBMI",
    "DBGE", "DBLT", "DBGT", "DBLE",
];

/// Format a signed displacement as `$xx` / `-$xx`.
fn disp(d: i32) -> String {
    if d < 0 {
        format!("-${:X}", -(d as i64))
    } else {
        format!("${d:X}")
    }
}

/// Format the index part of a brief extension word (`Dn.W`, `An.L`, …).
fn index_reg(ext: u16) -> String {
    let kind = if ext & 0x8000 != 0 { 'A' } else { 'D' };
    let reg = (ext >> 12) & 7;
    let size = if ext & 0x0800 != 0 { 'L' } else { 'W' };
    format!("{kind}{reg}.{size}")
}

/// A formatted effective address plus its statically known absolute
/// address (absolute modes and `d16(PC)`), used for jump targets.
struct EaText {
    text: String,
    target: Option<u32>,
}

impl EaText {
    fn new(text: String) -> Self {
        Self { text, target: None }
    }
}

/// Format one effective address, consuming its extension words.
/// `ext_addr` is the address of the next extension word (for resolving
/// PC-relative modes). Returns `None` if the byte slice runs out.
fn ea(mode: u8, reg: u8, sz: Sz, cur: &mut Cur, addr: u32) -> Option<EaText> {
    let ext_addr = addr.wrapping_add(cur.pos as u32);
    Some(match (mode, reg) {
        (0, _) => EaText::new(format!("D{reg}")),
        (1, _) => EaText::new(format!("A{reg}")),
        (2, _) => EaText::new(format!("(A{reg})")),
        (3, _) => EaText::new(format!("(A{reg})+")),
        (4, _) => EaText::new(format!("-(A{reg})")),
        (5, _) => {
            let d = cur.word()? as i16;
            EaText::new(format!("{}(A{reg})", disp(d as i32)))
        }
        (6, _) => {
            let ext = cur.word()?;
            let d = ext as i8;
            EaText::new(format!("{}(A{reg},{})", disp(d as i32), index_reg(ext)))
        }
        (7, 0) => {
            let abs = cur.word()? as i16 as i32 as u32; // sign-extends
            EaText {
                text: format!("${:04X}.W", abs as u16),
                target: Some(abs),
            }
        }
        (7, 1) => {
            let abs = cur.long()?;
            EaText {
                text: format!("${abs:08X}.L"),
                target: Some(abs),
            }
        }
        (7, 2) => {
            let d = cur.word()? as i16;
            let target = ext_addr.wrapping_add(d as i32 as u32);
            EaText {
                text: format!("${target:06X}(PC)"),
                target: Some(target),
            }
        }
        (7, 3) => {
            let ext = cur.word()?;
            let d = ext as i8;
            EaText::new(format!("{}(PC,{})", disp(d as i32), index_reg(ext)))
        }
        (7, 4) => {
            let text = match sz {
                Sz::B => format!("#${:02X}", cur.word()? as u8),
                Sz::W => format!("#${:04X}", cur.word()?),
                Sz::L => format!("#${:08X}", cur.long()?),
            };
            EaText::new(text)
        }
        _ => return None,
    })
}

/// Format a MOVEM register-list mask as `D0-D2/D7/A0-A6`.
///
/// `reversed` is the predecrement form, where bit 0 is A7 and bit 15 is
/// D0 (hardware reverses the mask for that mode).
fn movem_list(mask: u16, reversed: bool) -> String {
    // Normalize to bit 0 = D0 .. bit 15 = A7.
    let mask = if reversed { mask.reverse_bits() } else { mask };
    let name = |i: usize| {
        if i < 8 {
            format!("D{i}")
        } else {
            format!("A{}", i - 8)
        }
    };
    let mut parts: Vec<String> = Vec::new();
    // Runs never span the D/A boundary.
    for half in [0usize, 8] {
        let mut i = half;
        while i < half + 8 {
            if mask & (1 << i) == 0 {
                i += 1;
                continue;
            }
            let start = i;
            while i + 1 < half + 8 && mask & (1 << (i + 1)) != 0 {
                i += 1;
            }
            parts.push(if i == start {
                name(start)
            } else {
                format!("{}-{}", name(start), name(i))
            });
            i += 1;
        }
    }
    parts.join("/")
}

/// Disassemble one instruction. Split out so `mod.rs` can call it from
/// `DebugCpu::debug_disassemble` without the trait in scope.
pub(crate) fn disassemble(addr: u32, bytes: &[u8]) -> DisassembledInstruction {
    let mut cur = Cur::new(bytes);
    let Some(opcode) = cur.word() else {
        return make("???", String::new(), 1, bytes, None);
    };
    match dis_inner(opcode, addr, &mut cur) {
        Some(inst) => inst,
        None => truncated(bytes),
    }
}

/// Decode dispatcher; `None` means the byte slice ran out mid-decode.
fn dis_inner(opcode: u16, addr: u32, cur: &mut Cur) -> Option<DisassembledInstruction> {
    let raw = cur.bytes;
    let opmode = ((opcode >> 6) & 7) as u8;
    let ea_mode = ((opcode >> 3) & 7) as u8;
    let ea_reg = (opcode & 7) as u8;
    let dreg = ((opcode >> 9) & 7) as u8;

    let inst = match (opcode >> 12) & 0xF {
        // ANDI/ORI/EORI to CCR/SR
        0x0 if matches!(opcode, 0x003C | 0x007C | 0x023C | 0x027C | 0x0A3C | 0x0A7C) => {
            let mn = match opcode & 0x0F00 {
                0x0000 => "ORI",
                0x0200 => "ANDI",
                _ => "EORI",
            };
            if opcode & 0x0040 != 0 {
                make(mn, format!("#${:04X},SR", cur.word()?), cur.pos, raw, None)
            } else {
                make(
                    mn,
                    format!("#${:02X},CCR", cur.word()? as u8),
                    cur.pos,
                    raw,
                    None,
                )
            }
        }
        // MOVEP (dynamic-bit-op line, EA mode 001)
        0x0 if opcode & 0x0100 != 0 && ea_mode == 1 => {
            let sz = if opcode & 0x0040 != 0 { Sz::L } else { Sz::W };
            let d = cur.word()? as i16;
            let mem = format!("{}(A{ea_reg})", disp(d as i32));
            let ops = if opcode & 0x0080 != 0 {
                format!("D{dreg},{mem}")
            } else {
                format!("{mem},D{dreg}")
            };
            make(sized("MOVEP", sz), ops, cur.pos, raw, None)
        }
        // Dynamic bit ops (bit number in Dn)
        0x0 if opcode & 0x0100 != 0 => {
            let mn = ["BTST", "BCHG", "BCLR", "BSET"][(opmode & 3) as usize];
            let dst = ea(ea_mode, ea_reg, Sz::B, cur, addr)?;
            make(mn, format!("D{dreg},{}", dst.text), cur.pos, raw, None)
        }
        // Static bit ops (bit number in an extension word)
        0x0 if opcode & 0x0F00 == 0x0800 => {
            let mn = ["BTST", "BCHG", "BCLR", "BSET"][(opmode & 3) as usize];
            let bit = cur.word()?;
            let dst = ea(ea_mode, ea_reg, Sz::B, cur, addr)?;
            make(mn, format!("#{bit},{}", dst.text), cur.pos, raw, None)
        }
        // ORI/ANDI/SUBI/ADDI/EORI/CMPI #imm,<ea>
        0x0 => {
            let mn = match (opcode >> 9) & 7 {
                0 => "ORI",
                1 => "ANDI",
                2 => "SUBI",
                3 => "ADDI",
                5 => "EORI",
                6 => "CMPI",
                _ => return Some(dc_w(opcode, raw)),
            };
            let Some(sz) = size_bits(opcode >> 6) else {
                return Some(dc_w(opcode, raw));
            };
            let imm = ea(7, 4, sz, cur, addr)?;
            let dst = ea(ea_mode, ea_reg, sz, cur, addr)?;
            make(
                sized(mn, sz),
                format!("{},{}", imm.text, dst.text),
                cur.pos,
                raw,
                None,
            )
        }
        // MOVE / MOVEA (source EA extensions precede destination's)
        line @ 0x1..=0x3 => {
            let sz = match line {
                0x1 => Sz::B,
                0x3 => Sz::W,
                _ => Sz::L,
            };
            let dst_mode = opmode;
            let src = ea(ea_mode, ea_reg, sz, cur, addr)?;
            if dst_mode == 1 {
                if sz == Sz::B {
                    return Some(dc_w(opcode, raw)); // MOVEA.B doesn't exist
                }
                make(
                    sized("MOVEA", sz),
                    format!("{},A{dreg}", src.text),
                    cur.pos,
                    raw,
                    None,
                )
            } else {
                let dst = ea(dst_mode, dreg, sz, cur, addr)?;
                make(
                    sized("MOVE", sz),
                    format!("{},{}", src.text, dst.text),
                    cur.pos,
                    raw,
                    None,
                )
            }
        }
        // CHK and LEA share line 4 with the misc group
        0x4 if opcode & 0x01C0 == 0x0180 => {
            let src = ea(ea_mode, ea_reg, Sz::W, cur, addr)?;
            make("CHK", format!("{},D{dreg}", src.text), cur.pos, raw, None)
        }
        0x4 if opcode & 0x01C0 == 0x01C0 => {
            let src = ea(ea_mode, ea_reg, Sz::L, cur, addr)?;
            make("LEA", format!("{},A{dreg}", src.text), cur.pos, raw, None)
        }
        // Line 4 misc: unary ALU group, SR/CCR moves, SWAP/PEA/EXT/MOVEM,
        // TST/TAS/ILLEGAL, and the JMP/JSR + one-word specials.
        0x4 => match (opcode >> 8) & 0xF {
            0x0 if opcode & 0x00C0 == 0x00C0 => {
                let dst = ea(ea_mode, ea_reg, Sz::W, cur, addr)?;
                make("MOVE", format!("SR,{}", dst.text), cur.pos, raw, None)
            }
            0x0 => unary("NEGX", opcode, addr, cur)?,
            // 0x42C0 (MOVE from CCR) is 68010+; data on the 68000
            0x2 if opcode & 0x00C0 == 0x00C0 => dc_w(opcode, raw),
            0x2 => unary("CLR", opcode, addr, cur)?,
            0x4 if opcode & 0x00C0 == 0x00C0 => {
                let src = ea(ea_mode, ea_reg, Sz::W, cur, addr)?;
                make("MOVE", format!("{},CCR", src.text), cur.pos, raw, None)
            }
            0x4 => unary("NEG", opcode, addr, cur)?,
            0x6 if opcode & 0x00C0 == 0x00C0 => {
                let src = ea(ea_mode, ea_reg, Sz::W, cur, addr)?;
                make("MOVE", format!("{},SR", src.text), cur.pos, raw, None)
            }
            0x6 => unary("NOT", opcode, addr, cur)?,
            0x8 if opcode & 0x00C0 == 0 => {
                let dst = ea(ea_mode, ea_reg, Sz::B, cur, addr)?;
                make("NBCD", dst.text, cur.pos, raw, None)
            }
            0x8 if opcode & 0x00F8 == 0x0040 => {
                make("SWAP", format!("D{ea_reg}"), cur.pos, raw, None)
            }
            0x8 if opcode & 0x00C0 == 0x0040 => {
                let src = ea(ea_mode, ea_reg, Sz::L, cur, addr)?;
                make("PEA", src.text, cur.pos, raw, None)
            }
            0x8 if opcode & 0x0038 == 0 && opcode & 0x0080 != 0 => {
                let sz = if opcode & 0x0040 != 0 { Sz::L } else { Sz::W };
                make(sized("EXT", sz), format!("D{ea_reg}"), cur.pos, raw, None)
            }
            // MOVEM store: the mask word precedes the EA extensions;
            // the predecrement mode carries a bit-reversed mask
            0x8 if opcode & 0x0080 != 0 => {
                let sz = if opcode & 0x0040 != 0 { Sz::L } else { Sz::W };
                let mask = cur.word()?;
                let dst = ea(ea_mode, ea_reg, sz, cur, addr)?;
                let list = movem_list(mask, ea_mode == 4);
                make(
                    sized("MOVEM", sz),
                    format!("{list},{}", dst.text),
                    cur.pos,
                    raw,
                    None,
                )
            }
            0xA if opcode == 0x4AFC => make("ILLEGAL", String::new(), cur.pos, raw, None),
            0xA if opcode & 0x00C0 == 0x00C0 => {
                let dst = ea(ea_mode, ea_reg, Sz::B, cur, addr)?;
                make("TAS", dst.text, cur.pos, raw, None)
            }
            0xA => unary("TST", opcode, addr, cur)?,
            // MOVEM load
            0xC if opcode & 0x0080 != 0 => {
                let sz = if opcode & 0x0040 != 0 { Sz::L } else { Sz::W };
                let mask = cur.word()?;
                let src = ea(ea_mode, ea_reg, sz, cur, addr)?;
                make(
                    sized("MOVEM", sz),
                    format!("{},{}", src.text, movem_list(mask, false)),
                    cur.pos,
                    raw,
                    None,
                )
            }
            0xE => match (opcode >> 6) & 3 {
                3 | 2 => {
                    let mn = if (opcode >> 6) & 3 == 3 { "JMP" } else { "JSR" };
                    let dst = ea(ea_mode, ea_reg, Sz::L, cur, addr)?;
                    make(mn, dst.text, cur.pos, raw, dst.target)
                }
                1 => match opcode {
                    0x4E40..=0x4E4F => {
                        make("TRAP", format!("#{}", opcode & 0xF), cur.pos, raw, None)
                    }
                    0x4E50..=0x4E57 => {
                        let d = cur.word()? as i16;
                        make(
                            "LINK",
                            format!("A{ea_reg},#{}", disp(d as i32)),
                            cur.pos,
                            raw,
                            None,
                        )
                    }
                    0x4E58..=0x4E5F => make("UNLK", format!("A{ea_reg}"), cur.pos, raw, None),
                    0x4E60..=0x4E67 => make("MOVE", format!("A{ea_reg},USP"), cur.pos, raw, None),
                    0x4E68..=0x4E6F => make("MOVE", format!("USP,A{ea_reg}"), cur.pos, raw, None),
                    0x4E70 => make("RESET", String::new(), cur.pos, raw, None),
                    0x4E71 => make("NOP", String::new(), cur.pos, raw, None),
                    0x4E72 => make("STOP", format!("#${:04X}", cur.word()?), cur.pos, raw, None),
                    0x4E73 => make("RTE", String::new(), cur.pos, raw, None),
                    0x4E75 => make("RTS", String::new(), cur.pos, raw, None),
                    0x4E76 => make("TRAPV", String::new(), cur.pos, raw, None),
                    0x4E77 => make("RTR", String::new(), cur.pos, raw, None),
                    _ => dc_w(opcode, raw),
                },
                _ => dc_w(opcode, raw),
            },
            _ => dc_w(opcode, raw),
        },
        // DBcc / Scc / ADDQ / SUBQ
        0x5 if opmode & 3 == 3 && ea_mode == 1 => {
            let d = cur.word()? as i16;
            let target = addr.wrapping_add(2).wrapping_add(d as i32 as u32);
            make(
                DBCC[((opcode >> 8) & 0xF) as usize],
                format!("D{ea_reg},${target:06X}"),
                cur.pos,
                raw,
                Some(target),
            )
        }
        0x5 if opmode & 3 == 3 => {
            let dst = ea(ea_mode, ea_reg, Sz::B, cur, addr)?;
            make(
                SCC[((opcode >> 8) & 0xF) as usize],
                dst.text,
                cur.pos,
                raw,
                None,
            )
        }
        0x5 => {
            let mn = if opcode & 0x0100 != 0 { "SUBQ" } else { "ADDQ" };
            let sz = size_bits(opcode >> 6)?;
            let n = if dreg == 0 { 8 } else { dreg };
            let dst = ea(ea_mode, ea_reg, sz, cur, addr)?;
            make(
                sized(mn, sz),
                format!("#{n},{}", dst.text),
                cur.pos,
                raw,
                None,
            )
        }
        // BRA / BSR / Bcc: 8-bit displacement in the opcode, or a word
        // displacement when the 8-bit field is zero
        0x6 => {
            let d8 = opcode as i8;
            let target = if d8 == 0 {
                let d16 = cur.word()? as i16;
                addr.wrapping_add(2).wrapping_add(d16 as i32 as u32)
            } else {
                addr.wrapping_add(2).wrapping_add(d8 as i32 as u32)
            };
            make(
                BCC[((opcode >> 8) & 0xF) as usize],
                format!("${target:06X}"),
                cur.pos,
                raw,
                Some(target),
            )
        }
        // MOVEQ
        0x7 if opcode & 0x0100 == 0 => {
            let val = opcode as i8 as i32;
            make(
                "MOVEQ",
                format!("#{},D{dreg}", disp(val)),
                cur.pos,
                raw,
                None,
            )
        }
        // OR / DIVU / DIVS / SBCD
        0x8 => match opmode {
            3 | 7 => {
                let mn = if opmode == 7 { "DIVS" } else { "DIVU" };
                let src = ea(ea_mode, ea_reg, Sz::W, cur, addr)?;
                make(
                    sized(mn, Sz::W),
                    format!("{},D{dreg}", src.text),
                    cur.pos,
                    raw,
                    None,
                )
            }
            4 if ea_mode < 2 => bcd_pair("SBCD", dreg, ea_mode, ea_reg, cur, raw),
            5 | 6 if ea_mode < 2 => dc_w(opcode, raw), // PACK/UNPK are 68020+
            _ => logical("OR", opcode, addr, cur)?,
        },
        // SUB / SUBA / SUBX
        0x9 => match opmode {
            4..=6 if ea_mode < 2 => xop_pair("SUBX", opcode, dreg, ea_mode, ea_reg, cur, raw)?,
            _ => add_sub("SUB", "SUBA", opcode, addr, cur)?,
        },
        // CMP / CMPA / CMPM / EOR
        0xB => match opmode {
            4..=6 if ea_mode == 1 => {
                let sz = size_bits(opcode >> 6)?;
                make(
                    sized("CMPM", sz),
                    format!("(A{ea_reg})+,(A{dreg})+"),
                    cur.pos,
                    raw,
                    None,
                )
            }
            4..=6 => {
                let sz = size_bits(opcode >> 6)?;
                let dst = ea(ea_mode, ea_reg, sz, cur, addr)?;
                make(
                    sized("EOR", sz),
                    format!("D{dreg},{}", dst.text),
                    cur.pos,
                    raw,
                    None,
                )
            }
            3 | 7 => {
                let sz = if opmode == 7 { Sz::L } else { Sz::W };
                let src = ea(ea_mode, ea_reg, sz, cur, addr)?;
                make(
                    sized("CMPA", sz),
                    format!("{},A{dreg}", src.text),
                    cur.pos,
                    raw,
                    None,
                )
            }
            _ => {
                let sz = size_bits(opcode >> 6)?;
                let src = ea(ea_mode, ea_reg, sz, cur, addr)?;
                make(
                    sized("CMP", sz),
                    format!("{},D{dreg}", src.text),
                    cur.pos,
                    raw,
                    None,
                )
            }
        },
        // AND / MULU / MULS / ABCD / EXG
        0xC => match opmode {
            3 | 7 => {
                let mn = if opmode == 7 { "MULS" } else { "MULU" };
                let src = ea(ea_mode, ea_reg, Sz::W, cur, addr)?;
                make(
                    sized(mn, Sz::W),
                    format!("{},D{dreg}", src.text),
                    cur.pos,
                    raw,
                    None,
                )
            }
            4 if ea_mode < 2 => bcd_pair("ABCD", dreg, ea_mode, ea_reg, cur, raw),
            5 | 6 if ea_mode < 2 => match (opcode >> 3) & 0x1F {
                0x08 => make("EXG", format!("D{dreg},D{ea_reg}"), cur.pos, raw, None),
                0x09 => make("EXG", format!("A{dreg},A{ea_reg}"), cur.pos, raw, None),
                0x11 => make("EXG", format!("D{dreg},A{ea_reg}"), cur.pos, raw, None),
                _ => dc_w(opcode, raw),
            },
            _ => logical("AND", opcode, addr, cur)?,
        },
        // ADD / ADDA / ADDX
        0xD => match opmode {
            4..=6 if ea_mode < 2 => xop_pair("ADDX", opcode, dreg, ea_mode, ea_reg, cur, raw)?,
            _ => add_sub("ADD", "ADDA", opcode, addr, cur)?,
        },
        // Shifts and rotates
        0xE if opmode & 3 == 3 => {
            if opcode & 0x0800 != 0 {
                return Some(dc_w(opcode, raw));
            }
            let mn = shift_name(((opcode >> 9) & 3) as u8, opcode & 0x0100 != 0);
            let dst = ea(ea_mode, ea_reg, Sz::W, cur, addr)?;
            make(mn, dst.text, cur.pos, raw, None)
        }
        0xE => {
            let sz = size_bits(opcode >> 6)?;
            let mn = shift_name(((opcode >> 3) & 3) as u8, opcode & 0x0100 != 0);
            let count = if opcode & 0x0020 != 0 {
                format!("D{dreg}")
            } else {
                let n = if dreg == 0 { 8 } else { dreg };
                format!("#{n}")
            };
            make(
                sized(mn, sz),
                format!("{count},D{ea_reg}"),
                cur.pos,
                raw,
                None,
            )
        }
        // Lines A and F are unassigned on the 68000 (they vector through
        // their dedicated exceptions); everything else left is data.
        _ => dc_w(opcode, raw),
    };
    Some(inst)
}

/// Shift/rotate mnemonic from the 2-bit type field and direction bit.
fn shift_name(kind: u8, left: bool) -> &'static str {
    match (kind, left) {
        (0, false) => "ASR",
        (0, true) => "ASL",
        (1, false) => "LSR",
        (1, true) => "LSL",
        (2, false) => "ROXR",
        (2, true) => "ROXL",
        (3, false) => "ROR",
        _ => "ROL",
    }
}

/// ABCD/SBCD operand pair: `Dy,Dx` or `-(Ay),-(Ax)`.
fn bcd_pair(
    mn: &'static str,
    dreg: u8,
    ea_mode: u8,
    ea_reg: u8,
    cur: &Cur,
    raw: &[u8],
) -> DisassembledInstruction {
    let ops = if ea_mode == 0 {
        format!("D{ea_reg},D{dreg}")
    } else {
        format!("-(A{ea_reg}),-(A{dreg})")
    };
    make(mn, ops, cur.pos, raw, None)
}

/// ADDX/SUBX operand pair (sized form of the BCD pair shape).
fn xop_pair(
    mn: &'static str,
    opcode: u16,
    dreg: u8,
    ea_mode: u8,
    ea_reg: u8,
    cur: &Cur,
    raw: &[u8],
) -> Option<DisassembledInstruction> {
    let sz = size_bits(opcode >> 6)?;
    let ops = if ea_mode == 0 {
        format!("D{ea_reg},D{dreg}")
    } else {
        format!("-(A{ea_reg}),-(A{dreg})")
    };
    Some(make(sized(mn, sz), ops, cur.pos, raw, None))
}

/// Sized single-EA unary forms (NEGX/CLR/NEG/NOT/TST).
fn unary(
    mn: &'static str,
    opcode: u16,
    addr: u32,
    cur: &mut Cur,
) -> Option<DisassembledInstruction> {
    let raw = cur.bytes;
    let sz = size_bits(opcode >> 6)?;
    let dst = ea(((opcode >> 3) & 7) as u8, (opcode & 7) as u8, sz, cur, addr)?;
    Some(make(sized(mn, sz), dst.text, cur.pos, raw, None))
}

/// AND/OR: opmode 0-2 is `<ea>,Dn`, 4-6 is `Dn,<ea>`.
fn logical(
    mn: &'static str,
    opcode: u16,
    addr: u32,
    cur: &mut Cur,
) -> Option<DisassembledInstruction> {
    let raw = cur.bytes;
    let opmode = (opcode >> 6) & 7;
    let dreg = (opcode >> 9) & 7;
    let sz = size_bits(opcode >> 6)?;
    let operand = ea(((opcode >> 3) & 7) as u8, (opcode & 7) as u8, sz, cur, addr)?;
    let ops = if opmode & 4 == 0 {
        format!("{},D{dreg}", operand.text)
    } else {
        format!("D{dreg},{}", operand.text)
    };
    Some(make(sized(mn, sz), ops, cur.pos, raw, None))
}

/// ADD/SUB plus their An-destination ADDA/SUBA forms (opmode 3/7).
fn add_sub(
    mn: &'static str,
    mn_a: &'static str,
    opcode: u16,
    addr: u32,
    cur: &mut Cur,
) -> Option<DisassembledInstruction> {
    let raw = cur.bytes;
    let opmode = (opcode >> 6) & 7;
    let dreg = (opcode >> 9) & 7;
    if opmode == 3 || opmode == 7 {
        let sz = if opmode == 7 { Sz::L } else { Sz::W };
        let src = ea(((opcode >> 3) & 7) as u8, (opcode & 7) as u8, sz, cur, addr)?;
        return Some(make(
            sized(mn_a, sz),
            format!("{},A{dreg}", src.text),
            cur.pos,
            raw,
            None,
        ));
    }
    let sz = size_bits(opcode >> 6)?;
    let operand = ea(((opcode >> 3) & 7) as u8, (opcode & 7) as u8, sz, cur, addr)?;
    let ops = if opmode & 4 == 0 {
        format!("{},D{dreg}", operand.text)
    } else {
        format!("D{dreg},{}", operand.text)
    };
    Some(make(sized(mn, sz), ops, cur.pos, raw, None))
}

impl Disassemble for M68000 {
    fn disassemble(addr: u32, bytes: &[u8]) -> DisassembledInstruction {
        self::disassemble(addr, bytes)
    }
}
