//! Motorola M6809 instruction disassembler.
//!
//! Supports all three instruction pages (base, 0x10 prefix, 0x11 prefix),
//! with complex indexed post-byte decoding, TFR/EXG register pairs,
//! and PSH/PUL register bitmask formatting.

use crate::cpu::disasm::{Disassemble, DisassembledInstruction};
use crate::cpu::m6809::M6809;

/// M6809 addressing modes.
#[derive(Copy, Clone, Debug, PartialEq)]
enum AddrMode {
    /// No operand bytes (NOP, RTS, NEGA, etc.)
    Inh,
    /// 8-bit immediate: #$XX
    Imb,
    /// 16-bit immediate: #$XXXX
    Imw,
    /// Direct page: $XX — DP:offset address
    Dir,
    /// Extended: $XXXX — full 16-bit address
    Ext,
    /// Indexed: complex post-byte decoded addressing
    Idx,
    /// Relative byte: 8-bit signed branch offset
    Reb,
    /// Relative word: 16-bit signed branch offset
    Rew,
    /// TFR/EXG register pair post-byte
    R1,
    /// PSHS/PULS system stack register bitmask
    R2,
    /// PSHU/PULU user stack register bitmask
    R3,
    /// Illegal / undefined opcode
    Ill,
}

/// Opcode table entry: mnemonic + addressing mode.
struct Entry(&'static str, AddrMode);

use AddrMode::*;

// ── Page 1 opcode table (no prefix) ─────────────────────────────────────────

static PAGE1: [Entry; 256] = [
    // 0x00-0x0F: Direct-page unary/shift
    Entry("NEG", Dir), // 00
    Entry("???", Ill), // 01
    Entry("???", Ill), // 02
    Entry("COM", Dir), // 03
    Entry("LSR", Dir), // 04
    Entry("???", Ill), // 05
    Entry("ROR", Dir), // 06
    Entry("ASR", Dir), // 07
    Entry("ASL", Dir), // 08
    Entry("ROL", Dir), // 09
    Entry("DEC", Dir), // 0A
    Entry("???", Ill), // 0B
    Entry("INC", Dir), // 0C
    Entry("TST", Dir), // 0D
    Entry("JMP", Dir), // 0E
    Entry("CLR", Dir), // 0F
    // 0x10-0x1F: Prefix / misc
    Entry("???", Ill),   // 10 (page 2 prefix — handled before table lookup)
    Entry("???", Ill),   // 11 (page 3 prefix — handled before table lookup)
    Entry("NOP", Inh),   // 12
    Entry("SYNC", Inh),  // 13
    Entry("???", Ill),   // 14
    Entry("???", Ill),   // 15
    Entry("LBRA", Rew),  // 16
    Entry("LBSR", Rew),  // 17
    Entry("???", Ill),   // 18
    Entry("DAA", Inh),   // 19
    Entry("ORCC", Imb),  // 1A
    Entry("???", Ill),   // 1B
    Entry("ANDCC", Imb), // 1C
    Entry("SEX", Inh),   // 1D
    Entry("EXG", R1),    // 1E
    Entry("TFR", R1),    // 1F
    // 0x20-0x2F: Short branches
    Entry("BRA", Reb), // 20
    Entry("BRN", Reb), // 21
    Entry("BHI", Reb), // 22
    Entry("BLS", Reb), // 23
    Entry("BHS", Reb), // 24
    Entry("BLO", Reb), // 25
    Entry("BNE", Reb), // 26
    Entry("BEQ", Reb), // 27
    Entry("BVC", Reb), // 28
    Entry("BVS", Reb), // 29
    Entry("BPL", Reb), // 2A
    Entry("BMI", Reb), // 2B
    Entry("BGE", Reb), // 2C
    Entry("BLT", Reb), // 2D
    Entry("BGT", Reb), // 2E
    Entry("BLE", Reb), // 2F
    // 0x30-0x3F: LEA, stack, misc
    Entry("LEAX", Idx), // 30
    Entry("LEAY", Idx), // 31
    Entry("LEAS", Idx), // 32
    Entry("LEAU", Idx), // 33
    Entry("PSHS", R2),  // 34
    Entry("PULS", R2),  // 35
    Entry("PSHU", R3),  // 36
    Entry("PULU", R3),  // 37
    Entry("???", Ill),  // 38
    Entry("RTS", Inh),  // 39
    Entry("ABX", Inh),  // 3A
    Entry("RTI", Inh),  // 3B
    Entry("CWAI", Imb), // 3C
    Entry("MUL", Inh),  // 3D
    Entry("???", Ill),  // 3E
    Entry("SWI", Inh),  // 3F
    // 0x40-0x4F: A register inherent
    Entry("NEGA", Inh), // 40
    Entry("???", Ill),  // 41
    Entry("???", Ill),  // 42
    Entry("COMA", Inh), // 43
    Entry("LSRA", Inh), // 44
    Entry("???", Ill),  // 45
    Entry("RORA", Inh), // 46
    Entry("ASRA", Inh), // 47
    Entry("ASLA", Inh), // 48
    Entry("ROLA", Inh), // 49
    Entry("DECA", Inh), // 4A
    Entry("???", Ill),  // 4B
    Entry("INCA", Inh), // 4C
    Entry("TSTA", Inh), // 4D
    Entry("???", Ill),  // 4E
    Entry("CLRA", Inh), // 4F
    // 0x50-0x5F: B register inherent
    Entry("NEGB", Inh), // 50
    Entry("???", Ill),  // 51
    Entry("???", Ill),  // 52
    Entry("COMB", Inh), // 53
    Entry("LSRB", Inh), // 54
    Entry("???", Ill),  // 55
    Entry("RORB", Inh), // 56
    Entry("ASRB", Inh), // 57
    Entry("ASLB", Inh), // 58
    Entry("ROLB", Inh), // 59
    Entry("DECB", Inh), // 5A
    Entry("???", Ill),  // 5B
    Entry("INCB", Inh), // 5C
    Entry("TSTB", Inh), // 5D
    Entry("???", Ill),  // 5E
    Entry("CLRB", Inh), // 5F
    // 0x60-0x6F: Indexed unary/shift
    Entry("NEG", Idx), // 60
    Entry("???", Ill), // 61
    Entry("???", Ill), // 62
    Entry("COM", Idx), // 63
    Entry("LSR", Idx), // 64
    Entry("???", Ill), // 65
    Entry("ROR", Idx), // 66
    Entry("ASR", Idx), // 67
    Entry("ASL", Idx), // 68
    Entry("ROL", Idx), // 69
    Entry("DEC", Idx), // 6A
    Entry("???", Ill), // 6B
    Entry("INC", Idx), // 6C
    Entry("TST", Idx), // 6D
    Entry("JMP", Idx), // 6E
    Entry("CLR", Idx), // 6F
    // 0x70-0x7F: Extended unary/shift
    Entry("NEG", Ext), // 70
    Entry("???", Ill), // 71
    Entry("???", Ill), // 72
    Entry("COM", Ext), // 73
    Entry("LSR", Ext), // 74
    Entry("???", Ill), // 75
    Entry("ROR", Ext), // 76
    Entry("ASR", Ext), // 77
    Entry("ASL", Ext), // 78
    Entry("ROL", Ext), // 79
    Entry("DEC", Ext), // 7A
    Entry("???", Ill), // 7B
    Entry("INC", Ext), // 7C
    Entry("TST", Ext), // 7D
    Entry("JMP", Ext), // 7E
    Entry("CLR", Ext), // 7F
    // 0x80-0x8F: A-register / 16-bit immediate
    Entry("SUBA", Imb), // 80
    Entry("CMPA", Imb), // 81
    Entry("SBCA", Imb), // 82
    Entry("SUBD", Imw), // 83
    Entry("ANDA", Imb), // 84
    Entry("BITA", Imb), // 85
    Entry("LDA", Imb),  // 86
    Entry("???", Ill),  // 87
    Entry("EORA", Imb), // 88
    Entry("ADCA", Imb), // 89
    Entry("ORA", Imb),  // 8A
    Entry("ADDA", Imb), // 8B
    Entry("CMPX", Imw), // 8C
    Entry("BSR", Reb),  // 8D
    Entry("LDX", Imw),  // 8E
    Entry("???", Ill),  // 8F
    // 0x90-0x9F: A-register direct
    Entry("SUBA", Dir), // 90
    Entry("CMPA", Dir), // 91
    Entry("SBCA", Dir), // 92
    Entry("SUBD", Dir), // 93
    Entry("ANDA", Dir), // 94
    Entry("BITA", Dir), // 95
    Entry("LDA", Dir),  // 96
    Entry("STA", Dir),  // 97
    Entry("EORA", Dir), // 98
    Entry("ADCA", Dir), // 99
    Entry("ORA", Dir),  // 9A
    Entry("ADDA", Dir), // 9B
    Entry("CMPX", Dir), // 9C
    Entry("JSR", Dir),  // 9D
    Entry("LDX", Dir),  // 9E
    Entry("STX", Dir),  // 9F
    // 0xA0-0xAF: A-register indexed
    Entry("SUBA", Idx), // A0
    Entry("CMPA", Idx), // A1
    Entry("SBCA", Idx), // A2
    Entry("SUBD", Idx), // A3
    Entry("ANDA", Idx), // A4
    Entry("BITA", Idx), // A5
    Entry("LDA", Idx),  // A6
    Entry("STA", Idx),  // A7
    Entry("EORA", Idx), // A8
    Entry("ADCA", Idx), // A9
    Entry("ORA", Idx),  // AA
    Entry("ADDA", Idx), // AB
    Entry("CMPX", Idx), // AC
    Entry("JSR", Idx),  // AD
    Entry("LDX", Idx),  // AE
    Entry("STX", Idx),  // AF
    // 0xB0-0xBF: A-register extended
    Entry("SUBA", Ext), // B0
    Entry("CMPA", Ext), // B1
    Entry("SBCA", Ext), // B2
    Entry("SUBD", Ext), // B3
    Entry("ANDA", Ext), // B4
    Entry("BITA", Ext), // B5
    Entry("LDA", Ext),  // B6
    Entry("STA", Ext),  // B7
    Entry("EORA", Ext), // B8
    Entry("ADCA", Ext), // B9
    Entry("ORA", Ext),  // BA
    Entry("ADDA", Ext), // BB
    Entry("CMPX", Ext), // BC
    Entry("JSR", Ext),  // BD
    Entry("LDX", Ext),  // BE
    Entry("STX", Ext),  // BF
    // 0xC0-0xCF: B-register / 16-bit immediate
    Entry("SUBB", Imb), // C0
    Entry("CMPB", Imb), // C1
    Entry("SBCB", Imb), // C2
    Entry("ADDD", Imw), // C3
    Entry("ANDB", Imb), // C4
    Entry("BITB", Imb), // C5
    Entry("LDB", Imb),  // C6
    Entry("???", Ill),  // C7
    Entry("EORB", Imb), // C8
    Entry("ADCB", Imb), // C9
    Entry("ORB", Imb),  // CA
    Entry("ADDB", Imb), // CB
    Entry("LDD", Imw),  // CC
    Entry("???", Ill),  // CD
    Entry("LDU", Imw),  // CE
    Entry("???", Ill),  // CF
    // 0xD0-0xDF: B-register direct
    Entry("SUBB", Dir), // D0
    Entry("CMPB", Dir), // D1
    Entry("SBCB", Dir), // D2
    Entry("ADDD", Dir), // D3
    Entry("ANDB", Dir), // D4
    Entry("BITB", Dir), // D5
    Entry("LDB", Dir),  // D6
    Entry("STB", Dir),  // D7
    Entry("EORB", Dir), // D8
    Entry("ADCB", Dir), // D9
    Entry("ORB", Dir),  // DA
    Entry("ADDB", Dir), // DB
    Entry("LDD", Dir),  // DC
    Entry("STD", Dir),  // DD
    Entry("LDU", Dir),  // DE
    Entry("STU", Dir),  // DF
    // 0xE0-0xEF: B-register indexed
    Entry("SUBB", Idx), // E0
    Entry("CMPB", Idx), // E1
    Entry("SBCB", Idx), // E2
    Entry("ADDD", Idx), // E3
    Entry("ANDB", Idx), // E4
    Entry("BITB", Idx), // E5
    Entry("LDB", Idx),  // E6
    Entry("STB", Idx),  // E7
    Entry("EORB", Idx), // E8
    Entry("ADCB", Idx), // E9
    Entry("ORB", Idx),  // EA
    Entry("ADDB", Idx), // EB
    Entry("LDD", Idx),  // EC
    Entry("STD", Idx),  // ED
    Entry("LDU", Idx),  // EE
    Entry("STU", Idx),  // EF
    // 0xF0-0xFF: B-register extended
    Entry("SUBB", Ext), // F0
    Entry("CMPB", Ext), // F1
    Entry("SBCB", Ext), // F2
    Entry("ADDD", Ext), // F3
    Entry("ANDB", Ext), // F4
    Entry("BITB", Ext), // F5
    Entry("LDB", Ext),  // F6
    Entry("STB", Ext),  // F7
    Entry("EORB", Ext), // F8
    Entry("ADCB", Ext), // F9
    Entry("ORB", Ext),  // FA
    Entry("ADDB", Ext), // FB
    Entry("LDD", Ext),  // FC
    Entry("STD", Ext),  // FD
    Entry("LDU", Ext),  // FE
    Entry("STU", Ext),  // FF
];

// ── Page 2 / Page 3 lookup (sparse — match is cleaner than 256-entry tables) ─

/// Look up a page 2 (0x10 prefix) opcode.
fn page2_entry(opcode: u8) -> (&'static str, AddrMode) {
    match opcode {
        // Long conditional branches
        0x21 => ("LBRN", Rew),
        0x22 => ("LBHI", Rew),
        0x23 => ("LBLS", Rew),
        0x24 => ("LBHS", Rew),
        0x25 => ("LBLO", Rew),
        0x26 => ("LBNE", Rew),
        0x27 => ("LBEQ", Rew),
        0x28 => ("LBVC", Rew),
        0x29 => ("LBVS", Rew),
        0x2A => ("LBPL", Rew),
        0x2B => ("LBMI", Rew),
        0x2C => ("LBGE", Rew),
        0x2D => ("LBLT", Rew),
        0x2E => ("LBGT", Rew),
        0x2F => ("LBLE", Rew),
        // SWI2
        0x3F => ("SWI2", Inh),
        // CMPD (imm/dir/idx/ext)
        0x83 => ("CMPD", Imw),
        0x93 => ("CMPD", Dir),
        0xA3 => ("CMPD", Idx),
        0xB3 => ("CMPD", Ext),
        // CMPY (imm/dir/idx/ext)
        0x8C => ("CMPY", Imw),
        0x9C => ("CMPY", Dir),
        0xAC => ("CMPY", Idx),
        0xBC => ("CMPY", Ext),
        // LDY / STY
        0x8E => ("LDY", Imw),
        0x9E => ("LDY", Dir),
        0x9F => ("STY", Dir),
        0xAE => ("LDY", Idx),
        0xAF => ("STY", Idx),
        0xBE => ("LDY", Ext),
        0xBF => ("STY", Ext),
        // LDS / STS
        0xCE => ("LDS", Imw),
        0xDE => ("LDS", Dir),
        0xDF => ("STS", Dir),
        0xEE => ("LDS", Idx),
        0xEF => ("STS", Idx),
        0xFE => ("LDS", Ext),
        0xFF => ("STS", Ext),
        _ => ("???", Ill),
    }
}

/// Look up a page 3 (0x11 prefix) opcode.
fn page3_entry(opcode: u8) -> (&'static str, AddrMode) {
    match opcode {
        0x3F => ("SWI3", Inh),
        // CMPU (imm/dir/idx/ext)
        0x83 => ("CMPU", Imw),
        0x93 => ("CMPU", Dir),
        0xA3 => ("CMPU", Idx),
        0xB3 => ("CMPU", Ext),
        // CMPS (imm/dir/idx/ext)
        0x8C => ("CMPS", Imw),
        0x9C => ("CMPS", Dir),
        0xAC => ("CMPS", Idx),
        0xBC => ("CMPS", Ext),
        _ => ("???", Ill),
    }
}

// ── Indexed post-byte decoder ────────────────────────────────────────────────

/// Index register names for post-byte bits 6-5.
const IDX_REGS: [&str; 4] = ["X", "Y", "U", "S"];

/// Decode an indexed addressing post-byte.
/// `rest` starts at the post-byte position.
/// Returns (operand_string, total_bytes_consumed) where total includes the post-byte.
fn decode_indexed(rest: &[u8]) -> (String, u8) {
    if rest.is_empty() {
        return ("???".into(), 0);
    }

    let pb = rest[0];
    let reg = IDX_REGS[((pb >> 5) & 0x03) as usize];

    // Bit 7 = 0: 5-bit constant offset (-16 to +15), decimal format
    if pb & 0x80 == 0 {
        let raw = pb & 0x1F;
        let offset: i8 = if raw & 0x10 != 0 {
            (raw | 0xE0) as i8
        } else {
            raw as i8
        };
        return (format!("{},{}", offset, reg), 1);
    }

    // Extended indirect [$XXXX] — post-byte 0bRR1_1111
    if pb & 0x1F == 0x1F {
        if rest.len() < 3 {
            return ("???".into(), 1);
        }
        let addr = u16::from_be_bytes([rest[1], rest[2]]);
        return (format!("[${:04X}]", addr), 3);
    }

    let indirect = pb & 0x10 != 0;
    let mode = pb & 0x0F;

    // Modes 0x00 (,R+) and 0x02 (,-R) have no indirect form
    if indirect && matches!(mode, 0x00 | 0x02) {
        return ("???".into(), 1);
    }

    let (inner, extra) = match mode {
        0x00 => (format!(",{}+", reg), 0),
        0x01 => (format!(",{}++", reg), 0),
        0x02 => (format!(",-{}", reg), 0),
        0x03 => (format!(",--{}", reg), 0),
        0x04 => (format!(",{}", reg), 0),
        0x05 => (format!("B,{}", reg), 0),
        0x06 => (format!("A,{}", reg), 0),
        0x08 => {
            if rest.len() < 2 {
                return ("???".into(), 1);
            }
            (format!("${:02X},{}", rest[1], reg), 1)
        }
        0x09 => {
            if rest.len() < 3 {
                return ("???".into(), 1);
            }
            let off = u16::from_be_bytes([rest[1], rest[2]]);
            (format!("${:04X},{}", off, reg), 2)
        }
        0x0B => (format!("D,{}", reg), 0),
        0x0C => {
            if rest.len() < 2 {
                return ("???".into(), 1);
            }
            (format!("${:02X},PCR", rest[1]), 1)
        }
        0x0D => {
            if rest.len() < 3 {
                return ("???".into(), 1);
            }
            let off = u16::from_be_bytes([rest[1], rest[2]]);
            (format!("${:04X},PCR", off), 2)
        }
        _ => return ("???".into(), 1), // illegal: 0x07, 0x0A, 0x0E, 0x0F(non-indirect)
    };

    let total = 1 + extra;
    if indirect {
        (format!("[{}]", inner), total)
    } else {
        (inner, total)
    }
}

// ── TFR/EXG register pair decoder ────────────────────────────────────────────

/// TFR/EXG register names indexed by 4-bit register ID.
const R1_REGS: [&str; 16] = [
    "D", "X", "Y", "U", "S", "PC", "?", "?", "A", "B", "CC", "DP", "?", "?", "?", "?",
];

fn decode_r1(postbyte: u8) -> String {
    let src = (postbyte >> 4) as usize;
    let dst = (postbyte & 0x0F) as usize;
    format!("{},{}", R1_REGS[src], R1_REGS[dst])
}

// ── PSH/PUL register bitmask decoder ─────────────────────────────────────────

/// PSHS/PULS register names: bit 7 (PC) down to bit 0 (CC).
const R2_REGS: [&str; 8] = ["PC", "U", "Y", "X", "DP", "B", "A", "CC"];

/// PSHU/PULU register names: bit 7 (PC) down to bit 0 (CC).
const R3_REGS: [&str; 8] = ["PC", "S", "Y", "X", "DP", "B", "A", "CC"];

fn decode_push_pull(postbyte: u8, names: &[&str; 8]) -> String {
    let mut regs = Vec::new();
    for (i, name) in names.iter().enumerate() {
        if postbyte & (0x80 >> i) != 0 {
            regs.push(*name);
        }
    }
    regs.join(",")
}

// ── Instruction builder ──────────────────────────────────────────────────────

fn make_inst(
    mnemonic: &'static str,
    operands: String,
    byte_len: u8,
    raw: &[u8],
    target_addr: Option<u16>,
) -> DisassembledInstruction {
    let mut bytes = [0u8; 10];
    let n = (byte_len as usize).min(raw.len()).min(10);
    bytes[..n].copy_from_slice(&raw[..n]);
    DisassembledInstruction {
        mnemonic,
        operands,
        byte_len,
        bytes,
        target_addr: target_addr.map(u32::from),
    }
}

// ── Disassemble implementation ───────────────────────────────────────────────

impl Disassemble for M6809 {
    fn disassemble(addr: u32, bytes: &[u8]) -> DisassembledInstruction {
        let addr = addr as u16;
        if bytes.is_empty() {
            return make_inst("???", String::new(), 1, &[0], None);
        }

        // Determine page, mnemonic, addressing mode, and prefix length
        let (mnemonic, mode, prefix_len): (&str, AddrMode, u8) = match bytes[0] {
            0x10 => {
                if bytes.len() < 2 {
                    return make_inst("???", String::new(), 1, bytes, None);
                }
                let (m, am) = page2_entry(bytes[1]);
                (m, am, 1)
            }
            0x11 => {
                if bytes.len() < 2 {
                    return make_inst("???", String::new(), 1, bytes, None);
                }
                let (m, am) = page3_entry(bytes[1]);
                (m, am, 1)
            }
            op => {
                let e = &PAGE1[op as usize];
                (e.0, e.1, 0)
            }
        };

        let op_start = (prefix_len + 1) as usize; // first operand byte index
        let get = |off: usize| bytes.get(op_start + off).copied();

        match mode {
            Inh => make_inst(mnemonic, String::new(), prefix_len + 1, bytes, None),

            Imb => {
                let Some(data) = get(0) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                make_inst(
                    mnemonic,
                    format!("#${:02X}", data),
                    prefix_len + 2,
                    bytes,
                    None,
                )
            }

            Imw => {
                let (Some(hi), Some(lo)) = (get(0), get(1)) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                let val = u16::from_be_bytes([hi, lo]);
                make_inst(
                    mnemonic,
                    format!("#${:04X}", val),
                    prefix_len + 3,
                    bytes,
                    None,
                )
            }

            Dir => {
                let Some(data) = get(0) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                make_inst(
                    mnemonic,
                    format!("${:02X}", data),
                    prefix_len + 2,
                    bytes,
                    Some(data as u16),
                )
            }

            Ext => {
                let (Some(hi), Some(lo)) = (get(0), get(1)) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                let target = u16::from_be_bytes([hi, lo]);
                make_inst(
                    mnemonic,
                    format!("${:04X}", target),
                    prefix_len + 3,
                    bytes,
                    Some(target),
                )
            }

            Idx => {
                let rest = &bytes[op_start..];
                let (operand, idx_len) = decode_indexed(rest);
                make_inst(mnemonic, operand, prefix_len + 1 + idx_len, bytes, None)
            }

            Reb => {
                let Some(offset) = get(0) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                let byte_len = prefix_len + 2;
                let target = addr
                    .wrapping_add(byte_len as u16)
                    .wrapping_add(offset as i8 as u16);
                make_inst(
                    mnemonic,
                    format!("${:04X}", target),
                    byte_len,
                    bytes,
                    Some(target),
                )
            }

            Rew => {
                let (Some(hi), Some(lo)) = (get(0), get(1)) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                let byte_len = prefix_len + 3;
                let offset = i16::from_be_bytes([hi, lo]) as u16;
                let target = addr.wrapping_add(byte_len as u16).wrapping_add(offset);
                make_inst(
                    mnemonic,
                    format!("${:04X}", target),
                    byte_len,
                    bytes,
                    Some(target),
                )
            }

            R1 => {
                let Some(data) = get(0) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                make_inst(mnemonic, decode_r1(data), prefix_len + 2, bytes, None)
            }

            R2 => {
                let Some(data) = get(0) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                make_inst(
                    mnemonic,
                    decode_push_pull(data, &R2_REGS),
                    prefix_len + 2,
                    bytes,
                    None,
                )
            }

            R3 => {
                let Some(data) = get(0) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                make_inst(
                    mnemonic,
                    decode_push_pull(data, &R3_REGS),
                    prefix_len + 2,
                    bytes,
                    None,
                )
            }

            Ill => {
                let len = if prefix_len > 0 { prefix_len + 1 } else { 1 };
                make_inst("???", String::new(), len, bytes, None)
            }
        }
    }
}
