//! CPU state snapshot types and traits

use crate::core::debug::DebugRegister;

/// Trait for CPU types that can provide state snapshots
pub trait CpuStateTrait {
    type Snapshot;
    fn snapshot(&self) -> Self::Snapshot;
}

/// M6809 CPU state snapshot
#[derive(Debug, Clone, PartialEq)]
pub struct M6809State {
    pub a: u8,   // Accumulator A
    pub b: u8,   // Accumulator B
    pub dp: u8,  // Direct Page register
    pub x: u16,  // Index register X
    pub y: u16,  // Index register Y
    pub u: u16,  // User stack pointer
    pub s: u16,  // Hardware stack pointer
    pub pc: u16, // Program counter
    pub cc: u8,  // Condition codes
}

impl M6809State {
    pub fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "PC",
                value: self.pc as u64,
                width: 16,
            },
            DebugRegister {
                name: "A",
                value: self.a as u64,
                width: 8,
            },
            DebugRegister {
                name: "B",
                value: self.b as u64,
                width: 8,
            },
            DebugRegister {
                name: "X",
                value: self.x as u64,
                width: 16,
            },
            DebugRegister {
                name: "Y",
                value: self.y as u64,
                width: 16,
            },
            DebugRegister {
                name: "U",
                value: self.u as u64,
                width: 16,
            },
            DebugRegister {
                name: "S",
                value: self.s as u64,
                width: 16,
            },
            DebugRegister {
                name: "DP",
                value: self.dp as u64,
                width: 8,
            },
            DebugRegister {
                name: "CC",
                value: self.cc as u64,
                width: 8,
            },
        ]
    }
}

/// M6502 CPU state snapshot
#[derive(Debug, Clone, PartialEq)]
pub struct M6502State {
    pub a: u8,   // Accumulator
    pub x: u8,   // X index register
    pub y: u8,   // Y index register
    pub pc: u16, // Program counter
    pub sp: u8,  // Stack pointer (0xFF based)
    pub p: u8,   // Status register (flags)
}

impl M6502State {
    pub fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "PC",
                value: self.pc as u64,
                width: 16,
            },
            DebugRegister {
                name: "A",
                value: self.a as u64,
                width: 8,
            },
            DebugRegister {
                name: "X",
                value: self.x as u64,
                width: 8,
            },
            DebugRegister {
                name: "Y",
                value: self.y as u64,
                width: 8,
            },
            DebugRegister {
                name: "SP",
                value: self.sp as u64,
                width: 8,
            },
            DebugRegister {
                name: "P",
                value: self.p as u64,
                width: 8,
            },
        ]
    }
}

/// M6800 CPU state snapshot
#[derive(Debug, Clone, PartialEq)]
pub struct M6800State {
    pub a: u8,   // Accumulator A
    pub b: u8,   // Accumulator B
    pub x: u16,  // Index register X
    pub sp: u16, // Stack pointer
    pub pc: u16, // Program counter
    pub cc: u8,  // Condition codes
}

impl M6800State {
    pub fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "PC",
                value: self.pc as u64,
                width: 16,
            },
            DebugRegister {
                name: "A",
                value: self.a as u64,
                width: 8,
            },
            DebugRegister {
                name: "B",
                value: self.b as u64,
                width: 8,
            },
            DebugRegister {
                name: "X",
                value: self.x as u64,
                width: 16,
            },
            DebugRegister {
                name: "SP",
                value: self.sp as u64,
                width: 16,
            },
            DebugRegister {
                name: "CC",
                value: self.cc as u64,
                width: 8,
            },
        ]
    }
}

/// I8035 (MCS-48) CPU state snapshot
#[derive(Debug, Clone, PartialEq)]
pub struct I8035State {
    pub a: u8,    // Accumulator
    pub pc: u16,  // Program counter (12-bit)
    pub psw: u8,  // Program status word (CY, AC, F0, BS, SP[2:0])
    pub f1: bool, // User flag 1 (not in PSW)
    pub t: u8,    // Timer/counter register
    pub dbbb: u8, // BUS port latch
    pub p1: u8,   // Port 1 output latch
    pub p2: u8,   // Port 2 output latch
}

impl I8035State {
    pub fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "PC",
                value: self.pc as u64,
                width: 16,
            },
            DebugRegister {
                name: "A",
                value: self.a as u64,
                width: 8,
            },
            DebugRegister {
                name: "PSW",
                value: self.psw as u64,
                width: 8,
            },
            DebugRegister {
                name: "T",
                value: self.t as u64,
                width: 8,
            },
            DebugRegister {
                name: "P1",
                value: self.p1 as u64,
                width: 8,
            },
            DebugRegister {
                name: "P2",
                value: self.p2 as u64,
                width: 8,
            },
        ]
    }
}

/// M68000 CPU state snapshot
#[derive(Debug, Clone, PartialEq)]
pub struct M68000State {
    pub d: [u32; 8], // Data registers D0-D7
    pub a: [u32; 8], // Address registers A0-A7 (A7 = active stack pointer)
    pub usp: u32,    // Inactive user stack pointer
    pub ssp: u32,    // Inactive supervisor stack pointer
    pub pc: u32,     // Program counter
    pub sr: u16,     // Status register (system byte + CCR)
}

impl M68000State {
    pub fn debug_registers(&self) -> Vec<DebugRegister> {
        let mut regs = vec![DebugRegister {
            name: "PC",
            value: self.pc as u64,
            width: 32,
        }];
        const D_NAMES: [&str; 8] = ["D0", "D1", "D2", "D3", "D4", "D5", "D6", "D7"];
        const A_NAMES: [&str; 8] = ["A0", "A1", "A2", "A3", "A4", "A5", "A6", "A7"];
        for (i, name) in D_NAMES.iter().enumerate() {
            regs.push(DebugRegister {
                name,
                value: self.d[i] as u64,
                width: 32,
            });
        }
        for (i, name) in A_NAMES.iter().enumerate() {
            regs.push(DebugRegister {
                name,
                value: self.a[i] as u64,
                width: 32,
            });
        }
        regs.push(DebugRegister {
            name: "USP",
            value: self.usp as u64,
            width: 32,
        });
        regs.push(DebugRegister {
            name: "SSP",
            value: self.ssp as u64,
            width: 32,
        });
        regs.push(DebugRegister {
            name: "SR",
            value: self.sr as u64,
            width: 16,
        });
        regs
    }
}

// I8088 state is defined in its own module; re-export here for consistency
pub use super::i8088::I8088State;

// MB88xx state is defined in its own module; re-export here for consistency
pub use super::mb88xx::Mb88xxState;

/// Z80 CPU state snapshot
#[derive(Debug, Clone, PartialEq)]
pub struct Z80State {
    pub a: u8,       // Accumulator
    pub f: u8,       // Flags register
    pub b: u8,       // Register B
    pub c: u8,       // Register C
    pub d: u8,       // Register D
    pub e: u8,       // Register E
    pub h: u8,       // Register H
    pub l: u8,       // Register L
    pub a_prime: u8, // Shadow accumulator
    pub f_prime: u8, // Shadow flags
    pub b_prime: u8, // Shadow B
    pub c_prime: u8, // Shadow C
    pub d_prime: u8, // Shadow D
    pub e_prime: u8, // Shadow E
    pub h_prime: u8, // Shadow H
    pub l_prime: u8, // Shadow L
    pub ix: u16,     // Index register X
    pub iy: u16,     // Index register Y
    pub sp: u16,     // Stack pointer
    pub pc: u16,     // Program counter
    pub i: u8,       // Interrupt vector register
    pub r: u8,       // Memory refresh register
    pub iff1: bool,  // Interrupt flip-flop 1
    pub iff2: bool,  // Interrupt flip-flop 2
    pub im: u8,      // Interrupt mode (0, 1, 2)
    pub memptr: u16, // Hidden WZ register
    pub p: bool,     // LD A,I/R tracker
    pub q: u8,       // Copy of F when flags modified, 0 otherwise
}

impl Z80State {
    pub fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "PC",
                value: self.pc as u64,
                width: 16,
            },
            DebugRegister {
                name: "AF",
                value: ((self.a as u64) << 8) | self.f as u64,
                width: 16,
            },
            DebugRegister {
                name: "BC",
                value: ((self.b as u64) << 8) | self.c as u64,
                width: 16,
            },
            DebugRegister {
                name: "DE",
                value: ((self.d as u64) << 8) | self.e as u64,
                width: 16,
            },
            DebugRegister {
                name: "HL",
                value: ((self.h as u64) << 8) | self.l as u64,
                width: 16,
            },
            DebugRegister {
                name: "IX",
                value: self.ix as u64,
                width: 16,
            },
            DebugRegister {
                name: "IY",
                value: self.iy as u64,
                width: 16,
            },
            DebugRegister {
                name: "SP",
                value: self.sp as u64,
                width: 16,
            },
            DebugRegister {
                name: "I",
                value: self.i as u64,
                width: 8,
            },
            DebugRegister {
                name: "R",
                value: self.r as u64,
                width: 8,
            },
        ]
    }
}
