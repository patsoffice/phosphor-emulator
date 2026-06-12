//! Motorola M6800 instruction disassembler.

use crate::cpu::disasm::{Disassemble, DisassembledInstruction};
use crate::cpu::m6800::M6800;

/// M6800 addressing modes.
#[derive(Copy, Clone, Debug, PartialEq)]
enum AddrMode {
    /// No operand bytes (NOP, TAP, NEGA, etc.)
    Inh,
    /// 8-bit immediate: #$XX
    Imm8,
    /// 16-bit immediate: #$XXXX
    Imm16,
    /// Direct (zero page): $XX — address in $0000-$00FF
    Dir,
    /// Indexed: $XX,X — 8-bit unsigned offset from index register X
    Idx,
    /// Extended: $XXXX — full 16-bit address
    Ext,
    /// Relative: 8-bit signed offset from PC (branches)
    Rel,
    /// Illegal / undefined opcode
    Ill,
}

/// Opcode table entry: mnemonic + addressing mode.
struct Entry(&'static str, AddrMode);

use AddrMode::*;

/// Full 256-entry opcode table for the Motorola M6800.
static OPCODES: [Entry; 256] = [
    // 0x00-0x0F
    Entry("???", Ill), // 00
    Entry("NOP", Inh), // 01
    Entry("???", Ill), // 02
    Entry("???", Ill), // 03
    Entry("???", Ill), // 04
    Entry("???", Ill), // 05
    Entry("TAP", Inh), // 06
    Entry("TPA", Inh), // 07
    Entry("INX", Inh), // 08
    Entry("DEX", Inh), // 09
    Entry("CLV", Inh), // 0A
    Entry("SEV", Inh), // 0B
    Entry("CLC", Inh), // 0C
    Entry("SEC", Inh), // 0D
    Entry("CLI", Inh), // 0E
    Entry("SEI", Inh), // 0F
    // 0x10-0x1F
    Entry("SBA", Inh), // 10
    Entry("CBA", Inh), // 11
    Entry("???", Ill), // 12
    Entry("???", Ill), // 13
    Entry("???", Ill), // 14
    Entry("???", Ill), // 15
    Entry("TAB", Inh), // 16
    Entry("TBA", Inh), // 17
    Entry("???", Ill), // 18
    Entry("DAA", Inh), // 19
    Entry("???", Ill), // 1A
    Entry("ABA", Inh), // 1B
    Entry("???", Ill), // 1C
    Entry("???", Ill), // 1D
    Entry("???", Ill), // 1E
    Entry("???", Ill), // 1F
    // 0x20-0x2F
    Entry("BRA", Rel), // 20
    Entry("???", Ill), // 21
    Entry("BHI", Rel), // 22
    Entry("BLS", Rel), // 23
    Entry("BCC", Rel), // 24
    Entry("BCS", Rel), // 25
    Entry("BNE", Rel), // 26
    Entry("BEQ", Rel), // 27
    Entry("BVC", Rel), // 28
    Entry("BVS", Rel), // 29
    Entry("BPL", Rel), // 2A
    Entry("BMI", Rel), // 2B
    Entry("BGE", Rel), // 2C
    Entry("BLT", Rel), // 2D
    Entry("BGT", Rel), // 2E
    Entry("BLE", Rel), // 2F
    // 0x30-0x3F
    Entry("TSX", Inh),  // 30
    Entry("INS", Inh),  // 31
    Entry("PULA", Inh), // 32
    Entry("PULB", Inh), // 33
    Entry("DES", Inh),  // 34
    Entry("TXS", Inh),  // 35
    Entry("PSHA", Inh), // 36
    Entry("PSHB", Inh), // 37
    Entry("???", Ill),  // 38
    Entry("RTS", Inh),  // 39
    Entry("???", Ill),  // 3A
    Entry("RTI", Inh),  // 3B
    Entry("???", Ill),  // 3C
    Entry("???", Ill),  // 3D
    Entry("WAI", Inh),  // 3E
    Entry("SWI", Inh),  // 3F
    // 0x40-0x4F
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
    // 0x50-0x5F
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
    // 0x60-0x6F
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
    // 0x70-0x7F
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
    // 0x80-0x8F
    Entry("SUBA", Imm8), // 80
    Entry("CMPA", Imm8), // 81
    Entry("SBCA", Imm8), // 82
    Entry("???", Ill),   // 83
    Entry("ANDA", Imm8), // 84
    Entry("BITA", Imm8), // 85
    Entry("LDAA", Imm8), // 86
    Entry("???", Ill),   // 87
    Entry("EORA", Imm8), // 88
    Entry("ADCA", Imm8), // 89
    Entry("ORAA", Imm8), // 8A
    Entry("ADDA", Imm8), // 8B
    Entry("CPX", Imm16), // 8C
    Entry("BSR", Rel),   // 8D
    Entry("LDS", Imm16), // 8E
    Entry("???", Ill),   // 8F
    // 0x90-0x9F
    Entry("SUBA", Dir), // 90
    Entry("CMPA", Dir), // 91
    Entry("SBCA", Dir), // 92
    Entry("???", Ill),  // 93
    Entry("ANDA", Dir), // 94
    Entry("BITA", Dir), // 95
    Entry("LDAA", Dir), // 96
    Entry("STAA", Dir), // 97
    Entry("EORA", Dir), // 98
    Entry("ADCA", Dir), // 99
    Entry("ORAA", Dir), // 9A
    Entry("ADDA", Dir), // 9B
    Entry("CPX", Dir),  // 9C
    Entry("???", Ill),  // 9D
    Entry("LDS", Dir),  // 9E
    Entry("STS", Dir),  // 9F
    // 0xA0-0xAF
    Entry("SUBA", Idx), // A0
    Entry("CMPA", Idx), // A1
    Entry("SBCA", Idx), // A2
    Entry("???", Ill),  // A3
    Entry("ANDA", Idx), // A4
    Entry("BITA", Idx), // A5
    Entry("LDAA", Idx), // A6
    Entry("STAA", Idx), // A7
    Entry("EORA", Idx), // A8
    Entry("ADCA", Idx), // A9
    Entry("ORAA", Idx), // AA
    Entry("ADDA", Idx), // AB
    Entry("CPX", Idx),  // AC
    Entry("JSR", Idx),  // AD
    Entry("LDS", Idx),  // AE
    Entry("STS", Idx),  // AF
    // 0xB0-0xBF
    Entry("SUBA", Ext), // B0
    Entry("CMPA", Ext), // B1
    Entry("SBCA", Ext), // B2
    Entry("???", Ill),  // B3
    Entry("ANDA", Ext), // B4
    Entry("BITA", Ext), // B5
    Entry("LDAA", Ext), // B6
    Entry("STAA", Ext), // B7
    Entry("EORA", Ext), // B8
    Entry("ADCA", Ext), // B9
    Entry("ORAA", Ext), // BA
    Entry("ADDA", Ext), // BB
    Entry("CPX", Ext),  // BC
    Entry("JSR", Ext),  // BD
    Entry("LDS", Ext),  // BE
    Entry("STS", Ext),  // BF
    // 0xC0-0xCF
    Entry("SUBB", Imm8), // C0
    Entry("CMPB", Imm8), // C1
    Entry("SBCB", Imm8), // C2
    Entry("???", Ill),   // C3
    Entry("ANDB", Imm8), // C4
    Entry("BITB", Imm8), // C5
    Entry("LDAB", Imm8), // C6
    Entry("???", Ill),   // C7
    Entry("EORB", Imm8), // C8
    Entry("ADCB", Imm8), // C9
    Entry("ORAB", Imm8), // CA
    Entry("ADDB", Imm8), // CB
    Entry("???", Ill),   // CC
    Entry("???", Ill),   // CD
    Entry("LDX", Imm16), // CE
    Entry("???", Ill),   // CF
    // 0xD0-0xDF
    Entry("SUBB", Dir), // D0
    Entry("CMPB", Dir), // D1
    Entry("SBCB", Dir), // D2
    Entry("???", Ill),  // D3
    Entry("ANDB", Dir), // D4
    Entry("BITB", Dir), // D5
    Entry("LDAB", Dir), // D6
    Entry("STAB", Dir), // D7
    Entry("EORB", Dir), // D8
    Entry("ADCB", Dir), // D9
    Entry("ORAB", Dir), // DA
    Entry("ADDB", Dir), // DB
    Entry("???", Ill),  // DC
    Entry("???", Ill),  // DD
    Entry("LDX", Dir),  // DE
    Entry("STX", Dir),  // DF
    // 0xE0-0xEF
    Entry("SUBB", Idx), // E0
    Entry("CMPB", Idx), // E1
    Entry("SBCB", Idx), // E2
    Entry("???", Ill),  // E3
    Entry("ANDB", Idx), // E4
    Entry("BITB", Idx), // E5
    Entry("LDAB", Idx), // E6
    Entry("STAB", Idx), // E7
    Entry("EORB", Idx), // E8
    Entry("ADCB", Idx), // E9
    Entry("ORAB", Idx), // EA
    Entry("ADDB", Idx), // EB
    Entry("???", Ill),  // EC
    Entry("???", Ill),  // ED
    Entry("LDX", Idx),  // EE
    Entry("STX", Idx),  // EF
    // 0xF0-0xFF
    Entry("SUBB", Ext), // F0
    Entry("CMPB", Ext), // F1
    Entry("SBCB", Ext), // F2
    Entry("???", Ill),  // F3
    Entry("ANDB", Ext), // F4
    Entry("BITB", Ext), // F5
    Entry("LDAB", Ext), // F6
    Entry("STAB", Ext), // F7
    Entry("EORB", Ext), // F8
    Entry("ADCB", Ext), // F9
    Entry("ORAB", Ext), // FA
    Entry("ADDB", Ext), // FB
    Entry("???", Ill),  // FC
    Entry("???", Ill),  // FD
    Entry("LDX", Ext),  // FE
    Entry("STX", Ext),  // FF
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

impl Disassemble for M6800 {
    fn disassemble(addr: u32, bytes: &[u8]) -> DisassembledInstruction {
        let addr = addr as u16;
        if bytes.is_empty() {
            return make_inst("???", String::new(), 1, &[0], None);
        }

        let opcode = bytes[0];
        let entry = &OPCODES[opcode as usize];
        let get_byte = |off: usize| -> Option<u8> { bytes.get(off).copied() };

        match entry.1 {
            Inh => make_inst(entry.0, String::new(), 1, bytes, None),

            Imm8 => {
                let Some(data) = get_byte(1) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                make_inst(entry.0, format!("#${:02X}", data), 2, bytes, None)
            }

            Imm16 => {
                let (Some(hi), Some(lo)) = (get_byte(1), get_byte(2)) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                let val = u16::from_be_bytes([hi, lo]);
                make_inst(entry.0, format!("#${:04X}", val), 3, bytes, None)
            }

            Dir => {
                let Some(data) = get_byte(1) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                let target = data as u16;
                make_inst(entry.0, format!("${:02X}", data), 2, bytes, Some(target))
            }

            Idx => {
                let Some(offset) = get_byte(1) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                make_inst(entry.0, format!("${:02X},X", offset), 2, bytes, None)
            }

            Ext => {
                let (Some(hi), Some(lo)) = (get_byte(1), get_byte(2)) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                let target = u16::from_be_bytes([hi, lo]);
                make_inst(entry.0, format!("${:04X}", target), 3, bytes, Some(target))
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
