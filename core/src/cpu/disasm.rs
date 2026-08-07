//! Common disassembly types and trait for all CPU implementations.

use std::fmt;

/// Result of disassembling a single instruction.
#[derive(Debug, Clone, PartialEq)]
pub struct DisassembledInstruction {
    /// The instruction mnemonic (e.g., "LDA", "LD", "MOV")
    pub mnemonic: &'static str,
    /// Formatted operand string (e.g., "#$42", "$1234,X", "(HL)")
    pub operands: String,
    /// Total byte length of the instruction (opcode + operands)
    pub byte_len: u8,
    /// The raw bytes of the instruction (valid entries: 0..byte_len).
    /// 10 bytes covers the longest supported instruction (the M68000's
    /// `MOVE.L #imm,abs.l`).
    pub bytes: [u8; 10],
    /// Resolved absolute address for branches, jumps, and calls
    pub target_addr: Option<u32>,
}

impl DisassembledInstruction {
    /// Format with symbol substitution. If the resolver returns a name for
    /// `target_addr`, it replaces the hex address in the output.
    pub fn format_with_symbols<'a>(&self, resolve: impl Fn(u32) -> Option<&'a str>) -> String {
        if let Some(target) = self.target_addr
            && let Some(name) = resolve(target)
        {
            // Try the hex widths the operand formatters emit, widest
            // first; skip widths the target doesn't fit in.
            let mut substituted = self.operands.clone();
            for digits in [8usize, 6, 4, 2] {
                if digits < 8 && (target >> (digits * 4)) != 0 {
                    continue;
                }
                let hex = format!("${target:0digits$X}");
                let replaced = self.operands.replace(&hex, name);
                if replaced != self.operands {
                    substituted = replaced;
                    break;
                }
            }
            if self.operands.is_empty() {
                return self.mnemonic.to_string();
            }
            return format!(
                "{:<width$}{}",
                self.mnemonic,
                substituted,
                width = mnemonic_pad(self.mnemonic)
            );
        }
        format!("{}", self)
    }
}

/// Space-separated uppercase hex of a byte slice, as shown in the raw-bytes
/// column of a disassembly line: `[0x8E, 0x01]` renders as `"8E 01"`.
///
/// Only the join is shared. The full line differs between the callers — the
/// CLI pads the column to 23 and prefixes a 6-digit address, the debug UI pads
/// to 12 and prefixes a breakpoint marker — so they format their own lines
/// around this.
pub fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Mnemonic column width: 6 for the classic short mnemonics, one space
/// past the mnemonic for the longer sized forms ("MOVEA.W").
fn mnemonic_pad(mnemonic: &str) -> usize {
    (mnemonic.len() + 1).max(6)
}

impl fmt::Display for DisassembledInstruction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.operands.is_empty() {
            write!(f, "{}", self.mnemonic)
        } else {
            write!(
                f,
                "{:<width$}{}",
                self.mnemonic,
                self.operands,
                width = mnemonic_pad(self.mnemonic)
            )
        }
    }
}

/// Trait for CPU disassemblers. Takes a byte slice starting at the instruction
/// and returns a structured disassembly result.
///
/// The `bytes` slice must contain enough data to decode one instruction.
/// A minimum of 10 bytes is sufficient for all supported CPUs.
/// If the slice is too short, the disassembler returns a partial result
/// with the mnemonic set to `"???"` and `byte_len` set to 1 (one opcode
/// word, 2, on the M68000).
pub trait Disassemble {
    /// Disassemble one instruction from the given byte slice.
    /// `addr` is the address of the first byte (needed for relative
    /// branch targets); 16-bit CPUs decode it modulo their 64 KB space.
    fn disassemble(addr: u32, bytes: &[u8]) -> DisassembledInstruction;
}
