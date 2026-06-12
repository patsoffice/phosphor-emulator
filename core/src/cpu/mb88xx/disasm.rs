//! Fujitsu MB88xx series MCU instruction disassembler.

use crate::cpu::disasm::{Disassemble, DisassembledInstruction};
use crate::cpu::mb88xx::Mb88xx;

/// Build a DisassembledInstruction with the given fields.
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

impl Disassemble for Mb88xx {
    fn disassemble(addr: u32, bytes: &[u8]) -> DisassembledInstruction {
        let addr = addr as u16;
        if bytes.is_empty() {
            return make_inst("???", String::new(), 1, &[0], None);
        }

        let op = bytes[0];
        let arg = bytes.get(1).copied();

        match op {
            // 0x00-0x0F: single-byte inherent instructions
            0x00 => make_inst("NOP", String::new(), 1, bytes, None),
            0x01 => make_inst("OUTO", String::new(), 1, bytes, None),
            0x02 => make_inst("OUTP", String::new(), 1, bytes, None),
            0x03 => make_inst("OUT", String::new(), 1, bytes, None),
            0x04 => make_inst("TAY", String::new(), 1, bytes, None),
            0x05 => make_inst("TATH", String::new(), 1, bytes, None),
            0x06 => make_inst("TATL", String::new(), 1, bytes, None),
            0x07 => make_inst("TAS", String::new(), 1, bytes, None),
            0x08 => make_inst("ICY", String::new(), 1, bytes, None),
            0x09 => make_inst("ICM", String::new(), 1, bytes, None),
            0x0A => make_inst("STIC", String::new(), 1, bytes, None),
            0x0B => make_inst("X", String::new(), 1, bytes, None),
            0x0C => make_inst("ROL", String::new(), 1, bytes, None),
            0x0D => make_inst("L", String::new(), 1, bytes, None),
            0x0E => make_inst("ADC", String::new(), 1, bytes, None),
            0x0F => make_inst("AND", String::new(), 1, bytes, None),

            // 0x10-0x1F
            0x10 => make_inst("DAA", String::new(), 1, bytes, None),
            0x11 => make_inst("DAS", String::new(), 1, bytes, None),
            0x12 => make_inst("INK", String::new(), 1, bytes, None),
            0x13 => make_inst("IN", String::new(), 1, bytes, None),
            0x14 => make_inst("TYA", String::new(), 1, bytes, None),
            0x15 => make_inst("TTHA", String::new(), 1, bytes, None),
            0x16 => make_inst("TTLA", String::new(), 1, bytes, None),
            0x17 => make_inst("TSA", String::new(), 1, bytes, None),
            0x18 => make_inst("DCY", String::new(), 1, bytes, None),
            0x19 => make_inst("DCM", String::new(), 1, bytes, None),
            0x1A => make_inst("STDC", String::new(), 1, bytes, None),
            0x1B => make_inst("XX", String::new(), 1, bytes, None),
            0x1C => make_inst("ROR", String::new(), 1, bytes, None),
            0x1D => make_inst("ST", String::new(), 1, bytes, None),
            0x1E => make_inst("SBC", String::new(), 1, bytes, None),
            0x1F => make_inst("OR", String::new(), 1, bytes, None),

            // 0x20-0x2F
            0x20 => make_inst("SETR", String::new(), 1, bytes, None),
            0x21 => make_inst("SETC", String::new(), 1, bytes, None),
            0x22 => make_inst("RSTR", String::new(), 1, bytes, None),
            0x23 => make_inst("RSTC", String::new(), 1, bytes, None),
            0x24 => make_inst("TSTR", String::new(), 1, bytes, None),
            0x25 => make_inst("TSTI", String::new(), 1, bytes, None),
            0x26 => make_inst("TSTV", String::new(), 1, bytes, None),
            0x27 => make_inst("TSTS", String::new(), 1, bytes, None),
            0x28 => make_inst("TSTC", String::new(), 1, bytes, None),
            0x29 => make_inst("TSTZ", String::new(), 1, bytes, None),
            0x2A => make_inst("STS", String::new(), 1, bytes, None),
            0x2B => make_inst("LS", String::new(), 1, bytes, None),
            0x2C => make_inst("RTS", String::new(), 1, bytes, None),
            0x2D => make_inst("NEG", String::new(), 1, bytes, None),
            0x2E => make_inst("C", String::new(), 1, bytes, None),
            0x2F => make_inst("EOR", String::new(), 1, bytes, None),

            // 0x30-0x33: SBIT n
            0x30..=0x33 => make_inst("SBIT", format!("{}", op & 3), 1, bytes, None),
            // 0x34-0x37: RBIT n
            0x34..=0x37 => make_inst("RBIT", format!("{}", op & 3), 1, bytes, None),
            // 0x38-0x3B: TBIT n
            0x38..=0x3B => make_inst("TBIT", format!("{}", op & 3), 1, bytes, None),

            // 0x3C: RTI
            0x3C => make_inst("RTI", String::new(), 1, bytes, None),

            // 0x3D: JPA imm (2 bytes)
            0x3D => {
                let Some(a) = arg else {
                    return make_inst("JPA", String::from("???"), 2, bytes, None);
                };
                make_inst("JPA", format!("#${:02X}", a), 2, bytes, None)
            }
            // 0x3E: EN imm (2 bytes)
            0x3E => {
                let Some(a) = arg else {
                    return make_inst("EN", String::from("???"), 2, bytes, None);
                };
                make_inst("EN", format!("#${:02X}", a), 2, bytes, None)
            }
            // 0x3F: DIS imm (2 bytes)
            0x3F => {
                let Some(a) = arg else {
                    return make_inst("DIS", String::from("???"), 2, bytes, None);
                };
                make_inst("DIS", format!("#${:02X}", a), 2, bytes, None)
            }

            // 0x40-0x43: SETD n
            0x40..=0x43 => make_inst("SETD", format!("{}", op & 3), 1, bytes, None),
            // 0x44-0x47: RSTD n
            0x44..=0x47 => make_inst("RSTD", format!("{}", op & 3), 1, bytes, None),
            // 0x48-0x4B: TSTD n (tests R2 bit n)
            0x48..=0x4B => make_inst("TSTD", format!("{}", (op & 3) + 8), 1, bytes, None),
            // 0x4C-0x4F: TBA n
            0x4C..=0x4F => make_inst("TBA", format!("{}", op & 3), 1, bytes, None),

            // 0x50-0x53: XD n
            0x50..=0x53 => make_inst("XD", format!("{}", op & 3), 1, bytes, None),
            // 0x54-0x57: XYD n
            0x54..=0x57 => make_inst("XYD", format!("{}", (op & 3) + 4), 1, bytes, None),

            // 0x58-0x5F: LXI n
            0x58..=0x5F => make_inst("LXI", format!("#${:01X}", op & 7), 1, bytes, None),

            // 0x60-0x67: CALL addr (2 bytes)
            0x60..=0x67 => {
                let Some(a) = arg else {
                    return make_inst("CALL", String::from("???"), 2, bytes, None);
                };
                let target = (((op & 7) as u16) << 8) | a as u16;
                make_inst("CALL", format!("${:04X}", target), 2, bytes, Some(target))
            }
            // 0x68-0x6F: JPL addr (2 bytes)
            0x68..=0x6F => {
                let Some(a) = arg else {
                    return make_inst("JPL", String::from("???"), 2, bytes, None);
                };
                let target = (((op & 7) as u16) << 8) | a as u16;
                make_inst("JPL", format!("${:04X}", target), 2, bytes, Some(target))
            }

            // 0x70-0x7F: AI n
            0x70..=0x7F => make_inst("AI", format!("#${:01X}", op & 0x0F), 1, bytes, None),

            // 0x80-0x8F: LYI n
            0x80..=0x8F => make_inst("LYI", format!("#${:01X}", op & 0x0F), 1, bytes, None),

            // 0x90-0x9F: LI n
            0x90..=0x9F => make_inst("LI", format!("#${:01X}", op & 0x0F), 1, bytes, None),

            // 0xA0-0xAF: CYI n
            0xA0..=0xAF => make_inst("CYI", format!("#${:01X}", op & 0x0F), 1, bytes, None),

            // 0xB0-0xBF: CI n
            0xB0..=0xBF => make_inst("CI", format!("#${:01X}", op & 0x0F), 1, bytes, None),

            // 0xC0-0xFF: JMP addr (within current page)
            _ => {
                let target = (addr & !0x3F) | (op & 0x3F) as u16;
                make_inst("JMP", format!("${:04X}", target), 1, bytes, Some(target))
            }
        }
    }
}
