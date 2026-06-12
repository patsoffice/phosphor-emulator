//! MOS 6502 instruction disassembler.

use crate::cpu::disasm::{Disassemble, DisassembledInstruction};
use crate::cpu::m6502::M6502;

/// MOS 6502 addressing modes.
#[derive(Copy, Clone, Debug, PartialEq)]
enum AddrMode {
    /// Implied — no operand (NOP, BRK, RTS, CLC, TAX, etc.)
    Imp,
    /// Accumulator — operates on A (ASL A, LSR A, ROL A, ROR A)
    Acc,
    /// Immediate: #$XX
    Imm,
    /// Zero Page: $XX
    Zp,
    /// Zero Page,X: $XX,X
    Zpx,
    /// Zero Page,Y: $XX,Y
    Zpy,
    /// Absolute: $XXXX
    Abs,
    /// Absolute,X: $XXXX,X
    Abx,
    /// Absolute,Y: $XXXX,Y
    Aby,
    /// Indirect: ($XXXX) — only JMP
    Ind,
    /// (Indirect,X): ($XX,X)
    Izx,
    /// (Indirect),Y: ($XX),Y
    Izy,
    /// Relative: 8-bit signed offset (branches)
    Rel,
    /// Illegal / undefined opcode
    Ill,
}

/// Opcode table entry: mnemonic + addressing mode.
struct Entry(&'static str, AddrMode);

use AddrMode::*;

/// Full 256-entry opcode table for the MOS 6502.
static OPCODES: [Entry; 256] = [
    // 0x00-0x0F
    Entry("BRK", Imp), // 00
    Entry("ORA", Izx), // 01
    Entry("???", Ill), // 02
    Entry("???", Ill), // 03
    Entry("???", Ill), // 04
    Entry("ORA", Zp),  // 05
    Entry("ASL", Zp),  // 06
    Entry("???", Ill), // 07
    Entry("PHP", Imp), // 08
    Entry("ORA", Imm), // 09
    Entry("ASL", Acc), // 0A
    Entry("???", Ill), // 0B
    Entry("???", Ill), // 0C
    Entry("ORA", Abs), // 0D
    Entry("ASL", Abs), // 0E
    Entry("???", Ill), // 0F
    // 0x10-0x1F
    Entry("BPL", Rel), // 10
    Entry("ORA", Izy), // 11
    Entry("???", Ill), // 12
    Entry("???", Ill), // 13
    Entry("???", Ill), // 14
    Entry("ORA", Zpx), // 15
    Entry("ASL", Zpx), // 16
    Entry("???", Ill), // 17
    Entry("CLC", Imp), // 18
    Entry("ORA", Aby), // 19
    Entry("???", Ill), // 1A
    Entry("???", Ill), // 1B
    Entry("???", Ill), // 1C
    Entry("ORA", Abx), // 1D
    Entry("ASL", Abx), // 1E
    Entry("???", Ill), // 1F
    // 0x20-0x2F
    Entry("JSR", Abs), // 20
    Entry("AND", Izx), // 21
    Entry("???", Ill), // 22
    Entry("???", Ill), // 23
    Entry("BIT", Zp),  // 24
    Entry("AND", Zp),  // 25
    Entry("ROL", Zp),  // 26
    Entry("???", Ill), // 27
    Entry("PLP", Imp), // 28
    Entry("AND", Imm), // 29
    Entry("ROL", Acc), // 2A
    Entry("???", Ill), // 2B
    Entry("BIT", Abs), // 2C
    Entry("AND", Abs), // 2D
    Entry("ROL", Abs), // 2E
    Entry("???", Ill), // 2F
    // 0x30-0x3F
    Entry("BMI", Rel), // 30
    Entry("AND", Izy), // 31
    Entry("???", Ill), // 32
    Entry("???", Ill), // 33
    Entry("???", Ill), // 34
    Entry("AND", Zpx), // 35
    Entry("ROL", Zpx), // 36
    Entry("???", Ill), // 37
    Entry("SEC", Imp), // 38
    Entry("AND", Aby), // 39
    Entry("???", Ill), // 3A
    Entry("???", Ill), // 3B
    Entry("???", Ill), // 3C
    Entry("AND", Abx), // 3D
    Entry("ROL", Abx), // 3E
    Entry("???", Ill), // 3F
    // 0x40-0x4F
    Entry("RTI", Imp), // 40
    Entry("EOR", Izx), // 41
    Entry("???", Ill), // 42
    Entry("???", Ill), // 43
    Entry("???", Ill), // 44
    Entry("EOR", Zp),  // 45
    Entry("LSR", Zp),  // 46
    Entry("???", Ill), // 47
    Entry("PHA", Imp), // 48
    Entry("EOR", Imm), // 49
    Entry("LSR", Acc), // 4A
    Entry("???", Ill), // 4B
    Entry("JMP", Abs), // 4C
    Entry("EOR", Abs), // 4D
    Entry("LSR", Abs), // 4E
    Entry("???", Ill), // 4F
    // 0x50-0x5F
    Entry("BVC", Rel), // 50
    Entry("EOR", Izy), // 51
    Entry("???", Ill), // 52
    Entry("???", Ill), // 53
    Entry("???", Ill), // 54
    Entry("EOR", Zpx), // 55
    Entry("LSR", Zpx), // 56
    Entry("???", Ill), // 57
    Entry("CLI", Imp), // 58
    Entry("EOR", Aby), // 59
    Entry("???", Ill), // 5A
    Entry("???", Ill), // 5B
    Entry("???", Ill), // 5C
    Entry("EOR", Abx), // 5D
    Entry("LSR", Abx), // 5E
    Entry("???", Ill), // 5F
    // 0x60-0x6F
    Entry("RTS", Imp), // 60
    Entry("ADC", Izx), // 61
    Entry("???", Ill), // 62
    Entry("???", Ill), // 63
    Entry("???", Ill), // 64
    Entry("ADC", Zp),  // 65
    Entry("ROR", Zp),  // 66
    Entry("???", Ill), // 67
    Entry("PLA", Imp), // 68
    Entry("ADC", Imm), // 69
    Entry("ROR", Acc), // 6A
    Entry("???", Ill), // 6B
    Entry("JMP", Ind), // 6C
    Entry("ADC", Abs), // 6D
    Entry("ROR", Abs), // 6E
    Entry("???", Ill), // 6F
    // 0x70-0x7F
    Entry("BVS", Rel), // 70
    Entry("ADC", Izy), // 71
    Entry("???", Ill), // 72
    Entry("???", Ill), // 73
    Entry("???", Ill), // 74
    Entry("ADC", Zpx), // 75
    Entry("ROR", Zpx), // 76
    Entry("???", Ill), // 77
    Entry("SEI", Imp), // 78
    Entry("ADC", Aby), // 79
    Entry("???", Ill), // 7A
    Entry("???", Ill), // 7B
    Entry("???", Ill), // 7C
    Entry("ADC", Abx), // 7D
    Entry("ROR", Abx), // 7E
    Entry("???", Ill), // 7F
    // 0x80-0x8F
    Entry("???", Ill), // 80
    Entry("STA", Izx), // 81
    Entry("???", Ill), // 82
    Entry("???", Ill), // 83
    Entry("STY", Zp),  // 84
    Entry("STA", Zp),  // 85
    Entry("STX", Zp),  // 86
    Entry("???", Ill), // 87
    Entry("DEY", Imp), // 88
    Entry("???", Ill), // 89
    Entry("TXA", Imp), // 8A
    Entry("???", Ill), // 8B
    Entry("STY", Abs), // 8C
    Entry("STA", Abs), // 8D
    Entry("STX", Abs), // 8E
    Entry("???", Ill), // 8F
    // 0x90-0x9F
    Entry("BCC", Rel), // 90
    Entry("STA", Izy), // 91
    Entry("???", Ill), // 92
    Entry("???", Ill), // 93
    Entry("STY", Zpx), // 94
    Entry("STA", Zpx), // 95
    Entry("STX", Zpy), // 96
    Entry("???", Ill), // 97
    Entry("TYA", Imp), // 98
    Entry("STA", Aby), // 99
    Entry("TXS", Imp), // 9A
    Entry("???", Ill), // 9B
    Entry("???", Ill), // 9C
    Entry("STA", Abx), // 9D
    Entry("???", Ill), // 9E
    Entry("???", Ill), // 9F
    // 0xA0-0xAF
    Entry("LDY", Imm), // A0
    Entry("LDA", Izx), // A1
    Entry("LDX", Imm), // A2
    Entry("???", Ill), // A3
    Entry("LDY", Zp),  // A4
    Entry("LDA", Zp),  // A5
    Entry("LDX", Zp),  // A6
    Entry("???", Ill), // A7
    Entry("TAY", Imp), // A8
    Entry("LDA", Imm), // A9
    Entry("TAX", Imp), // AA
    Entry("???", Ill), // AB
    Entry("LDY", Abs), // AC
    Entry("LDA", Abs), // AD
    Entry("LDX", Abs), // AE
    Entry("???", Ill), // AF
    // 0xB0-0xBF
    Entry("BCS", Rel), // B0
    Entry("LDA", Izy), // B1
    Entry("???", Ill), // B2
    Entry("???", Ill), // B3
    Entry("LDY", Zpx), // B4
    Entry("LDA", Zpx), // B5
    Entry("LDX", Zpy), // B6
    Entry("???", Ill), // B7
    Entry("CLV", Imp), // B8
    Entry("LDA", Aby), // B9
    Entry("TSX", Imp), // BA
    Entry("???", Ill), // BB
    Entry("LDY", Abx), // BC
    Entry("LDA", Abx), // BD
    Entry("LDX", Aby), // BE
    Entry("???", Ill), // BF
    // 0xC0-0xCF
    Entry("CPY", Imm), // C0
    Entry("CMP", Izx), // C1
    Entry("???", Ill), // C2
    Entry("???", Ill), // C3
    Entry("CPY", Zp),  // C4
    Entry("CMP", Zp),  // C5
    Entry("DEC", Zp),  // C6
    Entry("???", Ill), // C7
    Entry("INY", Imp), // C8
    Entry("CMP", Imm), // C9
    Entry("DEX", Imp), // CA
    Entry("???", Ill), // CB
    Entry("CPY", Abs), // CC
    Entry("CMP", Abs), // CD
    Entry("DEC", Abs), // CE
    Entry("???", Ill), // CF
    // 0xD0-0xDF
    Entry("BNE", Rel), // D0
    Entry("CMP", Izy), // D1
    Entry("???", Ill), // D2
    Entry("???", Ill), // D3
    Entry("???", Ill), // D4
    Entry("CMP", Zpx), // D5
    Entry("DEC", Zpx), // D6
    Entry("???", Ill), // D7
    Entry("CLD", Imp), // D8
    Entry("CMP", Aby), // D9
    Entry("???", Ill), // DA
    Entry("???", Ill), // DB
    Entry("???", Ill), // DC
    Entry("CMP", Abx), // DD
    Entry("DEC", Abx), // DE
    Entry("???", Ill), // DF
    // 0xE0-0xEF
    Entry("CPX", Imm), // E0
    Entry("SBC", Izx), // E1
    Entry("???", Ill), // E2
    Entry("???", Ill), // E3
    Entry("CPX", Zp),  // E4
    Entry("SBC", Zp),  // E5
    Entry("INC", Zp),  // E6
    Entry("???", Ill), // E7
    Entry("INX", Imp), // E8
    Entry("SBC", Imm), // E9
    Entry("NOP", Imp), // EA
    Entry("???", Ill), // EB
    Entry("CPX", Abs), // EC
    Entry("SBC", Abs), // ED
    Entry("INC", Abs), // EE
    Entry("???", Ill), // EF
    // 0xF0-0xFF
    Entry("BEQ", Rel), // F0
    Entry("SBC", Izy), // F1
    Entry("???", Ill), // F2
    Entry("???", Ill), // F3
    Entry("???", Ill), // F4
    Entry("SBC", Zpx), // F5
    Entry("INC", Zpx), // F6
    Entry("???", Ill), // F7
    Entry("SED", Imp), // F8
    Entry("SBC", Aby), // F9
    Entry("???", Ill), // FA
    Entry("???", Ill), // FB
    Entry("???", Ill), // FC
    Entry("SBC", Abx), // FD
    Entry("INC", Abx), // FE
    Entry("???", Ill), // FF
];

/// Build a DisassembledInstruction with the given fields and raw bytes copied.
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

impl Disassemble for M6502 {
    fn disassemble(addr: u32, bytes: &[u8]) -> DisassembledInstruction {
        let addr = addr as u16;
        if bytes.is_empty() {
            return make_inst("???", String::new(), 1, &[0], None);
        }

        let opcode = bytes[0];
        let entry = &OPCODES[opcode as usize];
        let get_byte = |off: usize| -> Option<u8> { bytes.get(off).copied() };

        match entry.1 {
            Imp => make_inst(entry.0, String::new(), 1, bytes, None),

            Acc => make_inst(entry.0, "A".to_string(), 1, bytes, None),

            Imm => {
                let Some(data) = get_byte(1) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                make_inst(entry.0, format!("#${:02X}", data), 2, bytes, None)
            }

            Zp => {
                let Some(data) = get_byte(1) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                let target = data as u16;
                make_inst(entry.0, format!("${:02X}", data), 2, bytes, Some(target))
            }

            Zpx => {
                let Some(data) = get_byte(1) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                make_inst(entry.0, format!("${:02X},X", data), 2, bytes, None)
            }

            Zpy => {
                let Some(data) = get_byte(1) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                make_inst(entry.0, format!("${:02X},Y", data), 2, bytes, None)
            }

            Abs => {
                let (Some(lo), Some(hi)) = (get_byte(1), get_byte(2)) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                let target = u16::from_le_bytes([lo, hi]);
                make_inst(entry.0, format!("${:04X}", target), 3, bytes, Some(target))
            }

            Abx => {
                let (Some(lo), Some(hi)) = (get_byte(1), get_byte(2)) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                let addr16 = u16::from_le_bytes([lo, hi]);
                make_inst(entry.0, format!("${:04X},X", addr16), 3, bytes, None)
            }

            Aby => {
                let (Some(lo), Some(hi)) = (get_byte(1), get_byte(2)) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                let addr16 = u16::from_le_bytes([lo, hi]);
                make_inst(entry.0, format!("${:04X},Y", addr16), 3, bytes, None)
            }

            Ind => {
                let (Some(lo), Some(hi)) = (get_byte(1), get_byte(2)) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                let addr16 = u16::from_le_bytes([lo, hi]);
                make_inst(entry.0, format!("(${:04X})", addr16), 3, bytes, None)
            }

            Izx => {
                let Some(data) = get_byte(1) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                make_inst(entry.0, format!("(${:02X},X)", data), 2, bytes, None)
            }

            Izy => {
                let Some(data) = get_byte(1) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                make_inst(entry.0, format!("(${:02X}),Y", data), 2, bytes, None)
            }

            Rel => {
                let Some(offset) = get_byte(1) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                let target = addr.wrapping_add(2).wrapping_add(offset as i8 as u16);
                make_inst(entry.0, format!("${:04X}", target), 2, bytes, Some(target))
            }

            Ill => make_inst("???", String::new(), 1, bytes, None),
        }
    }
}
