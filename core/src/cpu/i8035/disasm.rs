//! Intel 8035/8048 (MCS-48) instruction disassembler.

use crate::cpu::disasm::{Disassemble, DisassembledInstruction};
use crate::cpu::i8035::I8035;

/// MCS-48 addressing modes.
#[derive(Copy, Clone, Debug, PartialEq)]
enum AddrMode {
    /// No operand (NOP, RET, CLR A, etc.)
    Inh,
    /// 8-bit immediate: #data
    Imm,
    /// 8-bit page-relative address (conditional jumps): addr within current page
    Addr8,
    /// 11-bit address (JMP/CALL): 3 bits from opcode + 8-bit operand
    Addr11,
    /// Register Rn (n encoded in low 3 bits of opcode)
    Rn,
    /// Indirect @Ri (i encoded in low bit of opcode)
    Ri,
    /// Register Rn + 8-bit immediate
    RnImm,
    /// Indirect @Ri + 8-bit immediate
    RiImm,
    /// Illegal / undefined opcode
    Ill,
}

/// Opcode table entry: mnemonic + addressing mode.
struct Entry(&'static str, AddrMode);

use AddrMode::*;

/// Full 256-entry opcode table for the MCS-48 instruction set.
static OPCODES: [Entry; 256] = [
    // 0x00-0x0F
    Entry("NOP", Inh),        // 00
    Entry("???", Ill),        // 01
    Entry("OUTL BUS,A", Inh), // 02
    Entry("ADD", Imm),        // 03: ADD A,#data
    Entry("JMP", Addr11),     // 04
    Entry("EN I", Inh),       // 05
    Entry("???", Ill),        // 06
    Entry("DEC A", Inh),      // 07
    Entry("INS A,BUS", Inh),  // 08
    Entry("IN A,P1", Inh),    // 09
    Entry("IN A,P2", Inh),    // 0A
    Entry("???", Ill),        // 0B
    Entry("MOVD A,P4", Inh),  // 0C
    Entry("MOVD A,P5", Inh),  // 0D
    Entry("MOVD A,P6", Inh),  // 0E
    Entry("MOVD A,P7", Inh),  // 0F
    // 0x10-0x1F
    Entry("INC", Ri),      // 10: INC @R0
    Entry("INC", Ri),      // 11: INC @R1
    Entry("JB0", Addr8),   // 12
    Entry("ADDC", Imm),    // 13: ADDC A,#data
    Entry("CALL", Addr11), // 14
    Entry("DIS I", Inh),   // 15
    Entry("JTF", Addr8),   // 16
    Entry("INC A", Inh),   // 17
    Entry("INC", Rn),      // 18: INC R0
    Entry("INC", Rn),      // 19: INC R1
    Entry("INC", Rn),      // 1A: INC R2
    Entry("INC", Rn),      // 1B: INC R3
    Entry("INC", Rn),      // 1C: INC R4
    Entry("INC", Rn),      // 1D: INC R5
    Entry("INC", Rn),      // 1E: INC R6
    Entry("INC", Rn),      // 1F: INC R7
    // 0x20-0x2F
    Entry("XCH", Ri),       // 20: XCH A,@R0
    Entry("XCH", Ri),       // 21: XCH A,@R1
    Entry("???", Ill),      // 22
    Entry("MOV", Imm),      // 23: MOV A,#data
    Entry("JMP", Addr11),   // 24
    Entry("EN TCNTI", Inh), // 25
    Entry("JNT0", Addr8),   // 26
    Entry("CLR A", Inh),    // 27
    Entry("XCH", Rn),       // 28: XCH A,R0
    Entry("XCH", Rn),       // 29: XCH A,R1
    Entry("XCH", Rn),       // 2A: XCH A,R2
    Entry("XCH", Rn),       // 2B: XCH A,R3
    Entry("XCH", Rn),       // 2C: XCH A,R4
    Entry("XCH", Rn),       // 2D: XCH A,R5
    Entry("XCH", Rn),       // 2E: XCH A,R6
    Entry("XCH", Rn),       // 2F: XCH A,R7
    // 0x30-0x3F
    Entry("XCHD", Ri),       // 30: XCHD A,@R0
    Entry("XCHD", Ri),       // 31: XCHD A,@R1
    Entry("JB1", Addr8),     // 32
    Entry("???", Ill),       // 33
    Entry("CALL", Addr11),   // 34
    Entry("DIS TCNTI", Inh), // 35
    Entry("JT0", Addr8),     // 36
    Entry("CPL A", Inh),     // 37
    Entry("???", Ill),       // 38
    Entry("OUTL P1,A", Inh), // 39
    Entry("OUTL P2,A", Inh), // 3A
    Entry("???", Ill),       // 3B
    Entry("MOVD P4,A", Inh), // 3C
    Entry("MOVD P5,A", Inh), // 3D
    Entry("MOVD P6,A", Inh), // 3E
    Entry("MOVD P7,A", Inh), // 3F
    // 0x40-0x4F
    Entry("ORL", Ri),       // 40: ORL A,@R0
    Entry("ORL", Ri),       // 41: ORL A,@R1
    Entry("MOV A,T", Inh),  // 42
    Entry("ORL", Imm),      // 43: ORL A,#data
    Entry("JMP", Addr11),   // 44
    Entry("STRT CNT", Inh), // 45
    Entry("JNT1", Addr8),   // 46
    Entry("SWAP A", Inh),   // 47
    Entry("ORL", Rn),       // 48: ORL A,R0
    Entry("ORL", Rn),       // 49: ORL A,R1
    Entry("ORL", Rn),       // 4A: ORL A,R2
    Entry("ORL", Rn),       // 4B: ORL A,R3
    Entry("ORL", Rn),       // 4C: ORL A,R4
    Entry("ORL", Rn),       // 4D: ORL A,R5
    Entry("ORL", Rn),       // 4E: ORL A,R6
    Entry("ORL", Rn),       // 4F: ORL A,R7
    // 0x50-0x5F
    Entry("ANL", Ri),      // 50: ANL A,@R0
    Entry("ANL", Ri),      // 51: ANL A,@R1
    Entry("JB2", Addr8),   // 52
    Entry("ANL", Imm),     // 53: ANL A,#data
    Entry("CALL", Addr11), // 54
    Entry("STRT T", Inh),  // 55
    Entry("JT1", Addr8),   // 56
    Entry("DA A", Inh),    // 57
    Entry("ANL", Rn),      // 58: ANL A,R0
    Entry("ANL", Rn),      // 59: ANL A,R1
    Entry("ANL", Rn),      // 5A: ANL A,R2
    Entry("ANL", Rn),      // 5B: ANL A,R3
    Entry("ANL", Rn),      // 5C: ANL A,R4
    Entry("ANL", Rn),      // 5D: ANL A,R5
    Entry("ANL", Rn),      // 5E: ANL A,R6
    Entry("ANL", Rn),      // 5F: ANL A,R7
    // 0x60-0x6F
    Entry("ADD", Ri),        // 60: ADD A,@R0
    Entry("ADD", Ri),        // 61: ADD A,@R1
    Entry("MOV T,A", Inh),   // 62
    Entry("???", Ill),       // 63
    Entry("JMP", Addr11),    // 64
    Entry("STOP TCNT", Inh), // 65
    Entry("???", Ill),       // 66
    Entry("RRC A", Inh),     // 67
    Entry("ADD", Rn),        // 68: ADD A,R0
    Entry("ADD", Rn),        // 69: ADD A,R1
    Entry("ADD", Rn),        // 6A: ADD A,R2
    Entry("ADD", Rn),        // 6B: ADD A,R3
    Entry("ADD", Rn),        // 6C: ADD A,R4
    Entry("ADD", Rn),        // 6D: ADD A,R5
    Entry("ADD", Rn),        // 6E: ADD A,R6
    Entry("ADD", Rn),        // 6F: ADD A,R7
    // 0x70-0x7F
    Entry("ADDC", Ri),     // 70: ADDC A,@R0
    Entry("ADDC", Ri),     // 71: ADDC A,@R1
    Entry("JB3", Addr8),   // 72
    Entry("???", Ill),     // 73
    Entry("CALL", Addr11), // 74
    Entry("???", Ill),     // 75
    Entry("JF1", Addr8),   // 76
    Entry("RR A", Inh),    // 77
    Entry("ADDC", Rn),     // 78: ADDC A,R0
    Entry("ADDC", Rn),     // 79: ADDC A,R1
    Entry("ADDC", Rn),     // 7A: ADDC A,R2
    Entry("ADDC", Rn),     // 7B: ADDC A,R3
    Entry("ADDC", Rn),     // 7C: ADDC A,R4
    Entry("ADDC", Rn),     // 7D: ADDC A,R5
    Entry("ADDC", Rn),     // 7E: ADDC A,R6
    Entry("ADDC", Rn),     // 7F: ADDC A,R7
    // 0x80-0x8F
    Entry("MOVX", Ri),       // 80: MOVX A,@R0
    Entry("MOVX", Ri),       // 81: MOVX A,@R1
    Entry("???", Ill),       // 82
    Entry("RET", Inh),       // 83
    Entry("JMP", Addr11),    // 84
    Entry("CLR F0", Inh),    // 85
    Entry("JNI", Addr8),     // 86
    Entry("???", Ill),       // 87
    Entry("ORL BUS,#", Imm), // 88
    Entry("ORL P1,#", Imm),  // 89
    Entry("ORL P2,#", Imm),  // 8A
    Entry("???", Ill),       // 8B
    Entry("ORLD P4,A", Inh), // 8C
    Entry("ORLD P5,A", Inh), // 8D
    Entry("ORLD P6,A", Inh), // 8E
    Entry("ORLD P7,A", Inh), // 8F
    // 0x90-0x9F
    Entry("MOVX", Ri),       // 90: MOVX @R0,A
    Entry("MOVX", Ri),       // 91: MOVX @R1,A
    Entry("JB4", Addr8),     // 92
    Entry("RETR", Inh),      // 93
    Entry("CALL", Addr11),   // 94
    Entry("CPL F0", Inh),    // 95
    Entry("JNZ", Addr8),     // 96
    Entry("CLR C", Inh),     // 97
    Entry("ANL BUS,#", Imm), // 98
    Entry("ANL P1,#", Imm),  // 99
    Entry("ANL P2,#", Imm),  // 9A
    Entry("???", Ill),       // 9B
    Entry("ANLD P4,A", Inh), // 9C
    Entry("ANLD P5,A", Inh), // 9D
    Entry("ANLD P6,A", Inh), // 9E
    Entry("ANLD P7,A", Inh), // 9F
    // 0xA0-0xAF
    Entry("MOV", Ri),        // A0: MOV @R0,A
    Entry("MOV", Ri),        // A1: MOV @R1,A
    Entry("???", Ill),       // A2
    Entry("MOVP A,@A", Inh), // A3
    Entry("JMP", Addr11),    // A4
    Entry("CLR F1", Inh),    // A5
    Entry("???", Ill),       // A6
    Entry("CPL C", Inh),     // A7
    Entry("MOV", Rn),        // A8: MOV R0,A
    Entry("MOV", Rn),        // A9: MOV R1,A
    Entry("MOV", Rn),        // AA: MOV R2,A
    Entry("MOV", Rn),        // AB: MOV R3,A
    Entry("MOV", Rn),        // AC: MOV R4,A
    Entry("MOV", Rn),        // AD: MOV R5,A
    Entry("MOV", Rn),        // AE: MOV R6,A
    Entry("MOV", Rn),        // AF: MOV R7,A
    // 0xB0-0xBF
    Entry("MOV", RiImm),   // B0: MOV @R0,#data
    Entry("MOV", RiImm),   // B1: MOV @R1,#data
    Entry("JB5", Addr8),   // B2
    Entry("JMPP @A", Inh), // B3
    Entry("CALL", Addr11), // B4
    Entry("CPL F1", Inh),  // B5
    Entry("JF0", Addr8),   // B6
    Entry("???", Ill),     // B7
    Entry("MOV", RnImm),   // B8: MOV R0,#data
    Entry("MOV", RnImm),   // B9: MOV R1,#data
    Entry("MOV", RnImm),   // BA: MOV R2,#data
    Entry("MOV", RnImm),   // BB: MOV R3,#data
    Entry("MOV", RnImm),   // BC: MOV R4,#data
    Entry("MOV", RnImm),   // BD: MOV R5,#data
    Entry("MOV", RnImm),   // BE: MOV R6,#data
    Entry("MOV", RnImm),   // BF: MOV R7,#data
    // 0xC0-0xCF
    Entry("???", Ill),       // C0
    Entry("???", Ill),       // C1
    Entry("???", Ill),       // C2
    Entry("???", Ill),       // C3
    Entry("JMP", Addr11),    // C4
    Entry("SEL RB0", Inh),   // C5
    Entry("JZ", Addr8),      // C6
    Entry("MOV A,PSW", Inh), // C7
    Entry("DEC", Rn),        // C8: DEC R0
    Entry("DEC", Rn),        // C9: DEC R1
    Entry("DEC", Rn),        // CA: DEC R2
    Entry("DEC", Rn),        // CB: DEC R3
    Entry("DEC", Rn),        // CC: DEC R4
    Entry("DEC", Rn),        // CD: DEC R5
    Entry("DEC", Rn),        // CE: DEC R6
    Entry("DEC", Rn),        // CF: DEC R7
    // 0xD0-0xDF
    Entry("XRL", Ri),        // D0: XRL A,@R0
    Entry("XRL", Ri),        // D1: XRL A,@R1
    Entry("JB6", Addr8),     // D2
    Entry("XRL", Imm),       // D3: XRL A,#data
    Entry("CALL", Addr11),   // D4
    Entry("SEL RB1", Inh),   // D5
    Entry("???", Ill),       // D6
    Entry("MOV PSW,A", Inh), // D7
    Entry("XRL", Rn),        // D8: XRL A,R0
    Entry("XRL", Rn),        // D9: XRL A,R1
    Entry("XRL", Rn),        // DA: XRL A,R2
    Entry("XRL", Rn),        // DB: XRL A,R3
    Entry("XRL", Rn),        // DC: XRL A,R4
    Entry("XRL", Rn),        // DD: XRL A,R5
    Entry("XRL", Rn),        // DE: XRL A,R6
    Entry("XRL", Rn),        // DF: XRL A,R7
    // 0xE0-0xEF
    Entry("???", Ill),        // E0
    Entry("???", Ill),        // E1
    Entry("???", Ill),        // E2
    Entry("MOVP3 A,@A", Inh), // E3
    Entry("JMP", Addr11),     // E4
    Entry("SEL MB0", Inh),    // E5
    Entry("JNC", Addr8),      // E6
    Entry("RL A", Inh),       // E7
    Entry("DJNZ", RnImm),     // E8: DJNZ R0,addr — uses RnImm to get register + operand byte
    Entry("DJNZ", RnImm),     // E9: DJNZ R1,addr
    Entry("DJNZ", RnImm),     // EA: DJNZ R2,addr
    Entry("DJNZ", RnImm),     // EB: DJNZ R3,addr
    Entry("DJNZ", RnImm),     // EC: DJNZ R4,addr
    Entry("DJNZ", RnImm),     // ED: DJNZ R5,addr
    Entry("DJNZ", RnImm),     // EE: DJNZ R6,addr
    Entry("DJNZ", RnImm),     // EF: DJNZ R7,addr
    // 0xF0-0xFF
    Entry("MOV", Ri),      // F0: MOV A,@R0
    Entry("MOV", Ri),      // F1: MOV A,@R1
    Entry("JB7", Addr8),   // F2
    Entry("???", Ill),     // F3
    Entry("CALL", Addr11), // F4
    Entry("SEL MB1", Inh), // F5
    Entry("JC", Addr8),    // F6
    Entry("RLC A", Inh),   // F7
    Entry("MOV", Rn),      // F8: MOV A,R0
    Entry("MOV", Rn),      // F9: MOV A,R1
    Entry("MOV", Rn),      // FA: MOV A,R2
    Entry("MOV", Rn),      // FB: MOV A,R3
    Entry("MOV", Rn),      // FC: MOV A,R4
    Entry("MOV", Rn),      // FD: MOV A,R5
    Entry("MOV", Rn),      // FE: MOV A,R6
    Entry("MOV", Rn),      // FF: MOV A,R7
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

impl Disassemble for I8035 {
    fn disassemble(addr: u32, bytes: &[u8]) -> DisassembledInstruction {
        let addr = addr as u16;
        if bytes.is_empty() {
            return make_inst("???", String::new(), 1, &[0], None);
        }

        let opcode = bytes[0];
        let entry = &OPCODES[opcode as usize];

        // Helper: safely read byte at offset, or return "???" if not available
        let get_byte = |off: usize| -> Option<u8> { bytes.get(off).copied() };

        match entry.1 {
            Inh => {
                // Full mnemonic is in the table (e.g., "CLR A", "OUTL BUS,A")
                make_inst(entry.0, String::new(), 1, bytes, None)
            }
            Imm => {
                let Some(data) = get_byte(1) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                // Determine operand format based on the mnemonic
                let operands = match opcode {
                    // ALU with accumulator: ADD A,#data etc.
                    0x03 | 0x13 | 0x43 | 0x53 | 0xD3 => format!("A,#${:02X}", data),
                    // MOV A,#data
                    0x23 => format!("A,#${:02X}", data),
                    // ORL A,#data
                    // Port read-modify-write: ORL BUS,# / ORL P1,# etc.
                    0x88 | 0x98 => format!("#${:02X}", data),
                    0x89 | 0x99 => format!("#${:02X}", data),
                    0x8A | 0x9A => format!("#${:02X}", data),
                    _ => format!("A,#${:02X}", data),
                };
                make_inst(entry.0, operands, 2, bytes, None)
            }
            Addr8 => {
                // Page-relative 8-bit address: target is in the same 256-byte page
                let Some(data) = get_byte(1) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                let target = (addr & 0xFF00) | data as u16;
                make_inst(entry.0, format!("${:04X}", target), 2, bytes, Some(target))
            }
            Addr11 => {
                // 11-bit address: bits 7:5 of opcode provide addr[10:8], operand byte is addr[7:0]
                let Some(data) = get_byte(1) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                let a10_8 = ((opcode as u16 >> 5) & 0x07) << 8;
                let target = a10_8 | data as u16;
                make_inst(entry.0, format!("${:04X}", target), 2, bytes, Some(target))
            }
            Rn => {
                let n = opcode & 0x07;
                let operands = format_rn_operands(opcode, n);
                make_inst(entry.0, operands, 1, bytes, None)
            }
            Ri => {
                let i = opcode & 0x01;
                let operands = format_ri_operands(opcode, i);
                make_inst(entry.0, operands, 1, bytes, None)
            }
            RnImm => {
                let Some(data) = get_byte(1) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                let n = opcode & 0x07;
                // DJNZ Rn,addr
                if entry.0 == "DJNZ" {
                    let target = (addr & 0xFF00) | data as u16;
                    return make_inst(
                        entry.0,
                        format!("R{},${:04X}", n, target),
                        2,
                        bytes,
                        Some(target),
                    );
                }
                // MOV Rn,#data
                make_inst(entry.0, format!("R{},#${:02X}", n, data), 2, bytes, None)
            }
            RiImm => {
                let Some(data) = get_byte(1) else {
                    return make_inst("???", String::new(), 1, bytes, None);
                };
                let i = opcode & 0x01;
                // MOV @Ri,#data
                make_inst(entry.0, format!("@R{},#${:02X}", i, data), 2, bytes, None)
            }
            Ill => make_inst("???", String::new(), 1, bytes, None),
        }
    }
}

/// Format operands for Rn-addressed instructions based on the instruction type.
fn format_rn_operands(opcode: u8, n: u8) -> String {
    match opcode & 0xF8 {
        // ALU A,Rn
        0x68 | 0x78 | 0x48 | 0x58 | 0xD8 => format!("A,R{}", n),
        // MOV A,Rn
        0xF8 => format!("A,R{}", n),
        // MOV Rn,A
        0xA8 => format!("R{},A", n),
        // XCH A,Rn
        0x28 => format!("A,R{}", n),
        // INC Rn
        0x18 => format!("R{}", n),
        // DEC Rn
        0xC8 => format!("R{}", n),
        _ => format!("R{}", n),
    }
}

/// Format operands for @Ri-addressed instructions based on the instruction type.
fn format_ri_operands(opcode: u8, i: u8) -> String {
    match opcode & 0xFE {
        // ALU A,@Ri
        0x60 | 0x70 | 0x40 | 0x50 | 0xD0 => format!("A,@R{}", i),
        // MOV A,@Ri
        0xF0 => format!("A,@R{}", i),
        // MOV @Ri,A
        0xA0 => format!("@R{},A", i),
        // XCH A,@Ri
        0x20 => format!("A,@R{}", i),
        // XCHD A,@Ri
        0x30 => format!("A,@R{}", i),
        // INC @Ri
        0x10 => format!("@R{}", i),
        // MOVX A,@Ri
        0x80 => format!("A,@R{}", i),
        // MOVX @Ri,A
        0x90 => format!("@R{},A", i),
        _ => format!("@R{}", i),
    }
}
