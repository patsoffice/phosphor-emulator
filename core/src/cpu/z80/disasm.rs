//! Zilog Z80 instruction disassembler.
//!
//! Supports all six prefix groups: unprefixed, CB, ED, DD (IX), FD (IY),
//! and DDCB/FDCB indexed bit operations. Includes undocumented opcodes
//! (SLL, IXH/IXL/IYH/IYL, DDCB/FDCB register writeback).

use crate::cpu::disasm::{Disassemble, DisassembledInstruction};
use crate::cpu::z80::Z80;

/// 8-bit register names by index (0-7). Index 6 = (HL).
const REG8: [&str; 8] = ["B", "C", "D", "E", "H", "L", "(HL)", "A"];

/// 16-bit register pair names for SP group (0-3).
const RP_SP: [&str; 4] = ["BC", "DE", "HL", "SP"];

/// 16-bit register pair names for AF group (PUSH/POP).
const RP_AF: [&str; 4] = ["BC", "DE", "HL", "AF"];

/// Condition code names by index (0-7).
const CC: [&str; 8] = ["NZ", "Z", "NC", "C", "PO", "PE", "P", "M"];

/// ALU operation names by index (0-7).
const ALU_OPS: [&str; 8] = ["ADD", "ADC", "SUB", "SBC", "AND", "XOR", "OR", "CP"];

/// CB-prefix rotation/shift names by index (0-7).
const ROT_OPS: [&str; 8] = ["RLC", "RRC", "RL", "RR", "SLA", "SRA", "SLL", "SRL"];

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

/// Get IX/IY-adjusted register name for index 4 (H) and 5 (L).
fn reg8_ix(index: u8, prefix: u8) -> &'static str {
    match (index, prefix) {
        (4, 0xDD) => "IXH",
        (5, 0xDD) => "IXL",
        (4, 0xFD) => "IYH",
        (5, 0xFD) => "IYL",
        _ => REG8[index as usize],
    }
}

/// Get IX/IY register name from prefix byte.
fn ix_name(prefix: u8) -> &'static str {
    if prefix == 0xDD { "IX" } else { "IY" }
}

/// Get IX/IY-adjusted register pair name for index 2.
fn rp_sp_ix(index: u8, prefix: u8) -> &'static str {
    if index == 2 && (prefix == 0xDD || prefix == 0xFD) {
        ix_name(prefix)
    } else {
        RP_SP[index as usize]
    }
}

/// Get IX/IY-adjusted register pair name for AF group (index 2).
fn rp_af_ix(index: u8, prefix: u8) -> &'static str {
    if index == 2 && (prefix == 0xDD || prefix == 0xFD) {
        ix_name(prefix)
    } else {
        RP_AF[index as usize]
    }
}

/// Format (IX+d) or (IY+d) displacement string.
fn ix_disp(prefix: u8, d: u8) -> String {
    let name = ix_name(prefix);
    let offset = d as i8;
    if offset >= 0 {
        format!("({}+${:02X})", name, offset)
    } else {
        format!("({}-${:02X})", name, (-offset) as u8)
    }
}

// ── Unprefixed opcode disassembly ────────────────────────────────────────────

/// Disassemble an unprefixed instruction (or DD/FD-prefixed).
/// `prefix` is 0 for unprefixed, 0xDD for IX, 0xFD for IY.
/// `base` is the full byte slice starting at the prefix (or opcode if unprefixed).
/// `off` is the offset to the opcode byte within `base`.
fn disasm_main(addr: u16, base: &[u8], off: usize, prefix: u8) -> DisassembledInstruction {
    let op = base[off];
    let x = (op >> 6) & 3;
    let y = (op >> 3) & 7;
    let z = op & 7;
    let p = y >> 1;
    let q = y & 1;

    // Helper to read byte at offset from opcode
    let get = |i: usize| base.get(off + 1 + i).copied();
    let has_ix = prefix == 0xDD || prefix == 0xFD;
    let pfx_len = if has_ix { 1u8 } else { 0u8 };

    match (x, z) {
        // ── x=0 ──────────────────────────────────────────────────────────
        (0, 0) => match y {
            0 => make_inst("NOP", String::new(), pfx_len + 1, base, None),
            1 => make_inst("EX", "AF,AF'".into(), pfx_len + 1, base, None),
            2 => {
                let Some(d) = get(0) else {
                    return make_inst("???", String::new(), 1, base, None);
                };
                let target = addr
                    .wrapping_add((off + 2) as u16)
                    .wrapping_add(d as i8 as u16);
                make_inst(
                    "DJNZ",
                    format!("${:04X}", target),
                    pfx_len + 2,
                    base,
                    Some(target),
                )
            }
            3 => {
                let Some(d) = get(0) else {
                    return make_inst("???", String::new(), 1, base, None);
                };
                let target = addr
                    .wrapping_add((off + 2) as u16)
                    .wrapping_add(d as i8 as u16);
                make_inst(
                    "JR",
                    format!("${:04X}", target),
                    pfx_len + 2,
                    base,
                    Some(target),
                )
            }
            4..=7 => {
                let Some(d) = get(0) else {
                    return make_inst("???", String::new(), 1, base, None);
                };
                let target = addr
                    .wrapping_add((off + 2) as u16)
                    .wrapping_add(d as i8 as u16);
                make_inst(
                    "JR",
                    format!("{},${:04X}", CC[(y - 4) as usize], target),
                    pfx_len + 2,
                    base,
                    Some(target),
                )
            }
            _ => unreachable!(),
        },
        (0, 1) => {
            if q == 0 {
                // LD rr, nn
                let (Some(lo), Some(hi)) = (get(0), get(1)) else {
                    return make_inst("???", String::new(), 1, base, None);
                };
                let val = u16::from_le_bytes([lo, hi]);
                make_inst(
                    "LD",
                    format!("{},${:04X}", rp_sp_ix(p, prefix), val),
                    pfx_len + 3,
                    base,
                    None,
                )
            } else {
                // ADD HL, rr
                make_inst(
                    "ADD",
                    format!(
                        "{},{}",
                        if has_ix { ix_name(prefix) } else { "HL" },
                        rp_sp_ix(p, prefix)
                    ),
                    pfx_len + 1,
                    base,
                    None,
                )
            }
        }
        (0, 2) => {
            // Indirect loads
            match (p, q) {
                (0, 0) => make_inst("LD", "(BC),A".into(), pfx_len + 1, base, None),
                (1, 0) => make_inst("LD", "(DE),A".into(), pfx_len + 1, base, None),
                (2, 0) => {
                    let (Some(lo), Some(hi)) = (get(0), get(1)) else {
                        return make_inst("???", String::new(), 1, base, None);
                    };
                    let a = u16::from_le_bytes([lo, hi]);
                    let rn = if has_ix { ix_name(prefix) } else { "HL" };
                    make_inst(
                        "LD",
                        format!("(${:04X}),{}", a, rn),
                        pfx_len + 3,
                        base,
                        Some(a),
                    )
                }
                (3, 0) => {
                    let (Some(lo), Some(hi)) = (get(0), get(1)) else {
                        return make_inst("???", String::new(), 1, base, None);
                    };
                    let a = u16::from_le_bytes([lo, hi]);
                    make_inst("LD", format!("(${:04X}),A", a), pfx_len + 3, base, Some(a))
                }
                (0, 1) => make_inst("LD", "A,(BC)".into(), pfx_len + 1, base, None),
                (1, 1) => make_inst("LD", "A,(DE)".into(), pfx_len + 1, base, None),
                (2, 1) => {
                    let (Some(lo), Some(hi)) = (get(0), get(1)) else {
                        return make_inst("???", String::new(), 1, base, None);
                    };
                    let a = u16::from_le_bytes([lo, hi]);
                    let rn = if has_ix { ix_name(prefix) } else { "HL" };
                    make_inst(
                        "LD",
                        format!("{},(${:04X})", rn, a),
                        pfx_len + 3,
                        base,
                        Some(a),
                    )
                }
                (3, 1) => {
                    let (Some(lo), Some(hi)) = (get(0), get(1)) else {
                        return make_inst("???", String::new(), 1, base, None);
                    };
                    let a = u16::from_le_bytes([lo, hi]);
                    make_inst("LD", format!("A,(${:04X})", a), pfx_len + 3, base, Some(a))
                }
                _ => unreachable!(),
            }
        }
        (0, 3) => {
            let rn = rp_sp_ix(p, prefix);
            if q == 0 {
                make_inst("INC", rn.into(), pfx_len + 1, base, None)
            } else {
                make_inst("DEC", rn.into(), pfx_len + 1, base, None)
            }
        }
        (0, 4) => {
            // INC r
            if y == 6 && has_ix {
                let Some(d) = get(0) else {
                    return make_inst("???", String::new(), 1, base, None);
                };
                make_inst("INC", ix_disp(prefix, d), pfx_len + 2, base, None)
            } else {
                make_inst("INC", reg8_ix(y, prefix).into(), pfx_len + 1, base, None)
            }
        }
        (0, 5) => {
            // DEC r
            if y == 6 && has_ix {
                let Some(d) = get(0) else {
                    return make_inst("???", String::new(), 1, base, None);
                };
                make_inst("DEC", ix_disp(prefix, d), pfx_len + 2, base, None)
            } else {
                make_inst("DEC", reg8_ix(y, prefix).into(), pfx_len + 1, base, None)
            }
        }
        (0, 6) => {
            // LD r, n
            if y == 6 && has_ix {
                let (Some(d), Some(n)) = (get(0), get(1)) else {
                    return make_inst("???", String::new(), 1, base, None);
                };
                make_inst(
                    "LD",
                    format!("{},${:02X}", ix_disp(prefix, d), n),
                    pfx_len + 3,
                    base,
                    None,
                )
            } else {
                let Some(n) = get(0) else {
                    return make_inst("???", String::new(), 1, base, None);
                };
                make_inst(
                    "LD",
                    format!("{},${:02X}", reg8_ix(y, prefix), n),
                    pfx_len + 2,
                    base,
                    None,
                )
            }
        }
        (0, 7) => {
            let mnem = match y {
                0 => "RLCA",
                1 => "RRCA",
                2 => "RLA",
                3 => "RRA",
                4 => "DAA",
                5 => "CPL",
                6 => "SCF",
                7 => "CCF",
                _ => unreachable!(),
            };
            make_inst(mnem, String::new(), pfx_len + 1, base, None)
        }

        // ── x=1: LD r,r' / HALT ──────────────────────────────────────────
        (1, _) => {
            if y == 6 && z == 6 {
                make_inst("HALT", String::new(), pfx_len + 1, base, None)
            } else if has_ix && (y == 6 || z == 6) {
                // One operand is (IX/IY+d)
                let Some(d) = get(0) else {
                    return make_inst("???", String::new(), 1, base, None);
                };
                let disp = ix_disp(prefix, d);
                if y == 6 {
                    make_inst(
                        "LD",
                        format!("{},{}", disp, REG8[z as usize]),
                        pfx_len + 2,
                        base,
                        None,
                    )
                } else {
                    make_inst(
                        "LD",
                        format!("{},{}", REG8[y as usize], disp),
                        pfx_len + 2,
                        base,
                        None,
                    )
                }
            } else {
                make_inst(
                    "LD",
                    format!("{},{}", reg8_ix(y, prefix), reg8_ix(z, prefix)),
                    pfx_len + 1,
                    base,
                    None,
                )
            }
        }

        // ── x=2: ALU A, r ────────────────────────────────────────────────
        (2, _) => {
            let mnem = ALU_OPS[y as usize];
            if z == 6 && has_ix {
                let Some(d) = get(0) else {
                    return make_inst("???", String::new(), 1, base, None);
                };
                let operand = if y == 0 || y == 1 {
                    format!("A,{}", ix_disp(prefix, d))
                } else {
                    ix_disp(prefix, d)
                };
                make_inst(mnem, operand, pfx_len + 2, base, None)
            } else {
                let r = reg8_ix(z, prefix);
                let operand = if y == 0 || y == 1 {
                    format!("A,{}", r)
                } else {
                    r.into()
                };
                make_inst(mnem, operand, pfx_len + 1, base, None)
            }
        }

        // ── x=3 ──────────────────────────────────────────────────────────
        (3, 0) => {
            // RET cc
            make_inst("RET", CC[y as usize].into(), pfx_len + 1, base, None)
        }
        (3, 1) => {
            if q == 0 {
                // POP rr
                make_inst("POP", rp_af_ix(p, prefix).into(), pfx_len + 1, base, None)
            } else {
                match p {
                    0 => make_inst("RET", String::new(), pfx_len + 1, base, None),
                    1 => make_inst("EXX", String::new(), pfx_len + 1, base, None),
                    2 => {
                        let rn = if has_ix { ix_name(prefix) } else { "HL" };
                        make_inst("JP", format!("({})", rn), pfx_len + 1, base, None)
                    }
                    3 => {
                        let rn = if has_ix { ix_name(prefix) } else { "HL" };
                        make_inst("LD", format!("SP,{}", rn), pfx_len + 1, base, None)
                    }
                    _ => unreachable!(),
                }
            }
        }
        (3, 2) => {
            // JP cc, nn
            let (Some(lo), Some(hi)) = (get(0), get(1)) else {
                return make_inst("???", String::new(), 1, base, None);
            };
            let target = u16::from_le_bytes([lo, hi]);
            make_inst(
                "JP",
                format!("{},${:04X}", CC[y as usize], target),
                pfx_len + 3,
                base,
                Some(target),
            )
        }
        (3, 3) => match y {
            0 => {
                let (Some(lo), Some(hi)) = (get(0), get(1)) else {
                    return make_inst("???", String::new(), 1, base, None);
                };
                let target = u16::from_le_bytes([lo, hi]);
                make_inst(
                    "JP",
                    format!("${:04X}", target),
                    pfx_len + 3,
                    base,
                    Some(target),
                )
            }
            1 => {
                // CB prefix — handled by caller
                unreachable!("CB prefix should be handled before disasm_main")
            }
            2 => {
                let Some(n) = get(0) else {
                    return make_inst("???", String::new(), 1, base, None);
                };
                make_inst("OUT", format!("(${:02X}),A", n), pfx_len + 2, base, None)
            }
            3 => {
                let Some(n) = get(0) else {
                    return make_inst("???", String::new(), 1, base, None);
                };
                make_inst("IN", format!("A,(${:02X})", n), pfx_len + 2, base, None)
            }
            4 => {
                let rn = if has_ix { ix_name(prefix) } else { "HL" };
                make_inst("EX", format!("(SP),{}", rn), pfx_len + 1, base, None)
            }
            5 => make_inst("EX", "DE,HL".into(), pfx_len + 1, base, None),
            6 => make_inst("DI", String::new(), pfx_len + 1, base, None),
            7 => make_inst("EI", String::new(), pfx_len + 1, base, None),
            _ => unreachable!(),
        },
        (3, 4) => {
            // CALL cc, nn
            let (Some(lo), Some(hi)) = (get(0), get(1)) else {
                return make_inst("???", String::new(), 1, base, None);
            };
            let target = u16::from_le_bytes([lo, hi]);
            make_inst(
                "CALL",
                format!("{},${:04X}", CC[y as usize], target),
                pfx_len + 3,
                base,
                Some(target),
            )
        }
        (3, 5) => {
            if q == 0 {
                // PUSH rr
                make_inst("PUSH", rp_af_ix(p, prefix).into(), pfx_len + 1, base, None)
            } else {
                match p {
                    0 => {
                        let (Some(lo), Some(hi)) = (get(0), get(1)) else {
                            return make_inst("???", String::new(), 1, base, None);
                        };
                        let target = u16::from_le_bytes([lo, hi]);
                        make_inst(
                            "CALL",
                            format!("${:04X}", target),
                            pfx_len + 3,
                            base,
                            Some(target),
                        )
                    }
                    1 => {
                        // DD prefix — handled by caller
                        unreachable!("DD prefix should be handled before disasm_main")
                    }
                    2 => {
                        // ED prefix — handled by caller
                        unreachable!("ED prefix should be handled before disasm_main")
                    }
                    3 => {
                        // FD prefix — handled by caller
                        unreachable!("FD prefix should be handled before disasm_main")
                    }
                    _ => unreachable!(),
                }
            }
        }
        (3, 6) => {
            // ALU A, n
            let Some(n) = get(0) else {
                return make_inst("???", String::new(), 1, base, None);
            };
            let mnem = ALU_OPS[y as usize];
            let operand = if y == 0 || y == 1 {
                format!("A,${:02X}", n)
            } else {
                format!("${:02X}", n)
            };
            make_inst(mnem, operand, pfx_len + 2, base, None)
        }
        (3, 7) => {
            // RST p
            let target = (y * 8) as u16;
            make_inst(
                "RST",
                format!("${:02X}", target),
                pfx_len + 1,
                base,
                Some(target),
            )
        }
        _ => unreachable!(),
    }
}

// ── CB prefix disassembly ────────────────────────────────────────────────────

fn disasm_cb(base: &[u8], off: usize) -> DisassembledInstruction {
    let op = base[off];
    let x = (op >> 6) & 3;
    let y = (op >> 3) & 7;
    let z = op & 7;

    match x {
        0 => {
            // Rotate/shift: RLC r .. SRL r
            make_inst(ROT_OPS[y as usize], REG8[z as usize].into(), 2, base, None)
        }
        1 => {
            // BIT b, r
            make_inst("BIT", format!("{},{}", y, REG8[z as usize]), 2, base, None)
        }
        2 => {
            // RES b, r
            make_inst("RES", format!("{},{}", y, REG8[z as usize]), 2, base, None)
        }
        3 => {
            // SET b, r
            make_inst("SET", format!("{},{}", y, REG8[z as usize]), 2, base, None)
        }
        _ => unreachable!(),
    }
}

// ── DDCB/FDCB prefix disassembly ─────────────────────────────────────────────

/// Disassemble DDCB/FDCB: format is [DD/FD, CB, d, op] = 4 bytes total.
fn disasm_index_cb(base: &[u8], prefix: u8) -> DisassembledInstruction {
    // base[0]=DD/FD, base[1]=CB, base[2]=d, base[3]=op
    if base.len() < 4 {
        return make_inst("???", String::new(), 1, base, None);
    }
    let d = base[2];
    let op = base[3];
    let x = (op >> 6) & 3;
    let y = (op >> 3) & 7;
    let z = op & 7;
    let disp = ix_disp(prefix, d);

    match x {
        0 => {
            // Rotate/shift (IX/IY+d) with optional register writeback
            let mnem = ROT_OPS[y as usize];
            if z == 6 {
                make_inst(mnem, disp, 4, base, None)
            } else {
                // Undocumented: result also stored in register z
                make_inst(
                    mnem,
                    format!("{},{}", disp, REG8[z as usize]),
                    4,
                    base,
                    None,
                )
            }
        }
        1 => {
            // BIT b, (IX/IY+d) — z is always treated as 6 (no writeback)
            make_inst("BIT", format!("{},{}", y, disp), 4, base, None)
        }
        2 => {
            // RES b, (IX/IY+d) with optional register writeback
            if z == 6 {
                make_inst("RES", format!("{},{}", y, disp), 4, base, None)
            } else {
                make_inst(
                    "RES",
                    format!("{},{},{}", y, disp, REG8[z as usize]),
                    4,
                    base,
                    None,
                )
            }
        }
        3 => {
            // SET b, (IX/IY+d) with optional register writeback
            if z == 6 {
                make_inst("SET", format!("{},{}", y, disp), 4, base, None)
            } else {
                make_inst(
                    "SET",
                    format!("{},{},{}", y, disp, REG8[z as usize]),
                    4,
                    base,
                    None,
                )
            }
        }
        _ => unreachable!(),
    }
}

// ── ED prefix disassembly ────────────────────────────────────────────────────

fn disasm_ed(addr: u16, base: &[u8]) -> DisassembledInstruction {
    // base[0]=ED, base[1]=opcode
    if base.len() < 2 {
        return make_inst("???", String::new(), 1, base, None);
    }
    let op = base[1];
    let x = (op >> 6) & 3;
    let y = (op >> 3) & 7;
    let z = op & 7;
    let p = y >> 1;
    let q = y & 1;

    // Helper to get byte at offset from ED opcode
    let get = |i: usize| base.get(2 + i).copied();

    // Only x=1 and x=2 have valid instructions
    match x {
        1 => match z {
            0 => {
                // IN r,(C) / IN (C) for y=6
                if y == 6 {
                    make_inst("IN", "(C)".into(), 2, base, None)
                } else {
                    make_inst("IN", format!("{},(C)", REG8[y as usize]), 2, base, None)
                }
            }
            1 => {
                // OUT (C),r / OUT (C),0 for y=6
                if y == 6 {
                    make_inst("OUT", "(C),0".into(), 2, base, None)
                } else {
                    make_inst("OUT", format!("(C),{}", REG8[y as usize]), 2, base, None)
                }
            }
            2 => {
                // SBC/ADC HL,rr
                if q == 0 {
                    make_inst("SBC", format!("HL,{}", RP_SP[p as usize]), 2, base, None)
                } else {
                    make_inst("ADC", format!("HL,{}", RP_SP[p as usize]), 2, base, None)
                }
            }
            3 => {
                // LD (nn),rr / LD rr,(nn)
                let (Some(lo), Some(hi)) = (get(0), get(1)) else {
                    return make_inst("???", String::new(), 1, base, None);
                };
                let a = u16::from_le_bytes([lo, hi]);
                if q == 0 {
                    make_inst(
                        "LD",
                        format!("(${:04X}),{}", a, RP_SP[p as usize]),
                        4,
                        base,
                        Some(a),
                    )
                } else {
                    make_inst(
                        "LD",
                        format!("{},(${:04X})", RP_SP[p as usize], a),
                        4,
                        base,
                        Some(a),
                    )
                }
            }
            4 => {
                // NEG (all y values)
                make_inst("NEG", String::new(), 2, base, None)
            }
            5 => {
                // RETN / RETI
                if y == 1 {
                    make_inst("RETI", String::new(), 2, base, None)
                } else {
                    make_inst("RETN", String::new(), 2, base, None)
                }
            }
            6 => {
                // IM 0/1/2
                let mode = match y {
                    0 | 4 => "0",
                    1 | 5 => "0", // undocumented: same as IM 0
                    2 | 6 => "1",
                    3 | 7 => "2",
                    _ => unreachable!(),
                };
                make_inst("IM", mode.into(), 2, base, None)
            }
            7 => {
                // Assorted: LD I,A / LD R,A / LD A,I / LD A,R / RRD / RLD / NOP / NOP
                match y {
                    0 => make_inst("LD", "I,A".into(), 2, base, None),
                    1 => make_inst("LD", "R,A".into(), 2, base, None),
                    2 => make_inst("LD", "A,I".into(), 2, base, None),
                    3 => make_inst("LD", "A,R".into(), 2, base, None),
                    4 => make_inst("RRD", String::new(), 2, base, None),
                    5 => make_inst("RLD", String::new(), 2, base, None),
                    6 | 7 => make_inst("NOP", String::new(), 2, base, None),
                    _ => unreachable!(),
                }
            }
            _ => unreachable!(),
        },
        2 => {
            // Block instructions: y >= 4 and z <= 3
            if y >= 4 && z <= 3 {
                let mnem = match (y, z) {
                    (4, 0) => "LDI",
                    (4, 1) => "CPI",
                    (4, 2) => "INI",
                    (4, 3) => "OUTI",
                    (5, 0) => "LDD",
                    (5, 1) => "CPD",
                    (5, 2) => "IND",
                    (5, 3) => "OUTD",
                    (6, 0) => "LDIR",
                    (6, 1) => "CPIR",
                    (6, 2) => "INIR",
                    (6, 3) => "OTIR",
                    (7, 0) => "LDDR",
                    (7, 1) => "CPDR",
                    (7, 2) => "INDR",
                    (7, 3) => "OTDR",
                    _ => unreachable!(),
                };
                make_inst(mnem, String::new(), 2, base, None)
            } else {
                // ED NOP
                make_inst("NOP", String::new(), 2, base, None)
            }
        }
        _ => {
            // x=0 and x=3: ED NOP
            let _ = addr; // suppress unused warning
            make_inst("NOP", String::new(), 2, base, None)
        }
    }
}

// ── Main entry point ─────────────────────────────────────────────────────────

impl Disassemble for Z80 {
    fn disassemble(addr: u32, bytes: &[u8]) -> DisassembledInstruction {
        let addr = addr as u16;
        if bytes.is_empty() {
            return make_inst("???", String::new(), 1, &[0], None);
        }

        match bytes[0] {
            // CB prefix
            0xCB => {
                if bytes.len() < 2 {
                    return make_inst("???", String::new(), 1, bytes, None);
                }
                disasm_cb(bytes, 1)
            }
            // ED prefix
            0xED => disasm_ed(addr, bytes),
            // DD prefix (IX)
            0xDD => {
                if bytes.len() < 2 {
                    return make_inst("???", String::new(), 1, bytes, None);
                }
                match bytes[1] {
                    0xCB => disasm_index_cb(bytes, 0xDD),
                    // DD DD, DD FD, DD ED — just a NOP-like prefix, re-decode
                    0xDD | 0xFD | 0xED => make_inst("NOP", String::new(), 1, bytes, None),
                    _ => disasm_main(addr, bytes, 1, 0xDD),
                }
            }
            // FD prefix (IY)
            0xFD => {
                if bytes.len() < 2 {
                    return make_inst("???", String::new(), 1, bytes, None);
                }
                match bytes[1] {
                    0xCB => disasm_index_cb(bytes, 0xFD),
                    0xDD | 0xFD | 0xED => make_inst("NOP", String::new(), 1, bytes, None),
                    _ => disasm_main(addr, bytes, 1, 0xFD),
                }
            }
            // Unprefixed
            _ => disasm_main(addr, bytes, 0, 0),
        }
    }
}
