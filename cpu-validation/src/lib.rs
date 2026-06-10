use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::{Bus, BusMaster};
use serde::{Deserialize, Serialize};

// --- TracingBus: flat 64KB memory with cycle-by-cycle recording ---

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BusOp {
    Read,
    Write,
    Internal,
}

#[derive(Clone, Debug)]
pub struct BusCycle {
    pub addr: u16,
    pub data: u8,
    pub op: BusOp,
}

pub struct TracingBus {
    pub memory: [u8; 0x10000],
    pub cycles: Vec<BusCycle>,
    /// Queue of (port_addr, data, direction) for I/O port reads/writes.
    /// Populated from test case `ports` field; io_read pops 'r' entries.
    pub port_queue: Vec<(u16, u8, char)>,
    pub port_index: usize,
}

impl TracingBus {
    pub fn new() -> Self {
        Self {
            memory: [0; 0x10000],
            cycles: Vec::new(),
            port_queue: Vec::new(),
            port_index: 0,
        }
    }

    pub fn load(&mut self, addr: u16, data: &[u8]) {
        let start = addr as usize;
        self.memory[start..start + data.len()].copy_from_slice(data);
    }

    pub fn clear_cycles(&mut self) {
        self.cycles.clear();
    }
}

impl Default for TracingBus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for TracingBus {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        let data = self.memory[addr as usize];
        self.cycles.push(BusCycle {
            addr,
            data,
            op: BusOp::Read,
        });
        data
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        self.memory[addr as usize] = data;
        self.cycles.push(BusCycle {
            addr,
            data,
            op: BusOp::Write,
        });
    }

    fn io_read(&mut self, _master: BusMaster, _addr: u16) -> u8 {
        // Return next port read value from the queue
        while self.port_index < self.port_queue.len() {
            let (_, data, dir) = self.port_queue[self.port_index];
            self.port_index += 1;
            if dir == 'r' {
                return data;
            }
        }
        0xFF // fallback
    }

    fn io_write(&mut self, _master: BusMaster, _addr: u16, _data: u8) {
        // Advance past the next 'w' entry in the port queue
        while self.port_index < self.port_queue.len() {
            let (_, _, dir) = self.port_queue[self.port_index];
            self.port_index += 1;
            if dir == 'w' {
                return;
            }
        }
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState::default()
    }
}

// --- JSON test vector types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    pub name: String,
    pub initial: CpuState,
    #[serde(rename = "final")]
    pub final_state: CpuState,
    pub cycles: Vec<(u16, u8, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuState {
    pub pc: u16,
    pub s: u16,
    pub u: u16,
    pub a: u8,
    pub b: u8,
    pub dp: u8,
    pub x: u16,
    pub y: u16,
    pub cc: u8,
    pub ram: Vec<(u16, u8)>,
}

// --- M6502 JSON test vector types (SingleStepTests/65x02 format) ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct M6502TestCase {
    pub name: String,
    pub initial: M6502CpuState,
    #[serde(rename = "final")]
    pub final_state: M6502CpuState,
    pub cycles: Vec<(u16, u8, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct M6502CpuState {
    pub pc: u16,
    pub s: u8,
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub p: u8,
    pub ram: Vec<(u16, u8)>,
}

// --- Z80 JSON test vector types (SingleStepTests/z80 format) ---

#[derive(Debug, Clone, Deserialize)]
pub struct Z80TestCase {
    pub name: String,
    pub initial: Z80CpuState,
    #[serde(rename = "final")]
    pub final_state: Z80CpuState,
    pub cycles: Vec<(Option<u16>, Option<u8>, String)>,
    #[serde(default)]
    pub ports: Vec<(u16, u8, String)>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Z80CpuState {
    pub pc: u16,
    pub sp: u16,
    pub a: u8,
    pub b: u8,
    pub c: u8,
    pub d: u8,
    pub e: u8,
    pub f: u8,
    pub h: u8,
    pub l: u8,
    pub i: u8,
    pub r: u8,
    pub ei: u8,
    pub wz: u16,
    pub ix: u16,
    pub iy: u16,
    #[serde(rename = "af_")]
    pub af_prime: u16,
    #[serde(rename = "bc_")]
    pub bc_prime: u16,
    #[serde(rename = "de_")]
    pub de_prime: u16,
    #[serde(rename = "hl_")]
    pub hl_prime: u16,
    pub im: u8,
    pub p: u8,
    pub q: u8,
    pub iff1: u8,
    pub iff2: u8,
    pub ram: Vec<(u16, u8)>,
}

// --- M6800 JSON test vector types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct M6800TestCase {
    pub name: String,
    pub initial: M6800CpuState,
    #[serde(rename = "final")]
    pub final_state: M6800CpuState,
    pub cycles: Vec<(u16, u8, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct M6800CpuState {
    pub pc: u16,
    pub sp: u16,
    pub a: u8,
    pub b: u8,
    pub x: u16,
    pub cc: u8,
    pub ram: Vec<(u16, u8)>,
}

// --- I8035 (MCS-48) JSON test vector types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct I8035TestCase {
    pub name: String,
    pub initial: I8035CpuState,
    #[serde(rename = "final")]
    pub final_state: I8035CpuState,
    pub cycles: Vec<(u16, u8, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct I8035CpuState {
    pub a: u8,
    pub pc: u16,
    pub psw: u8,
    pub f1: bool,
    pub t: u8,
    pub dbbb: u8,
    pub p1: u8,
    pub p2: u8,
    pub a11: bool,
    pub a11_pending: bool,
    pub timer_enabled: bool,
    pub counter_enabled: bool,
    pub timer_overflow: bool,
    pub int_enabled: bool,
    pub tcnti_enabled: bool,
    pub in_interrupt: bool,
    /// External bus memory (program memory + I/O mapped via io_read/io_write).
    pub ram: Vec<(u16, u8)>,
    /// Internal CPU RAM (64 bytes for 8035). Sparse (addr, value) pairs.
    pub internal_ram: Vec<(u8, u8)>,
}

// --- I8088 JSON test vector types (SingleStepTests/8088 v2 format) ---
//
// The 8088 test format uses 20-bit physical addresses and a sparse final
// state: only *changed* registers appear in the final state. We deserialize
// final regs as `Option<T>` and fall back to the initial value for comparison.

/// A single 8088 test vector.
#[derive(Debug, Clone, Deserialize)]
pub struct I8088TestCase {
    pub name: String,
    pub bytes: Vec<u8>,
    pub initial: I8088InitialState,
    #[serde(rename = "final")]
    pub final_state: I8088FinalState,
    // cycles, hash, idx are present but not used for functional validation
}

/// Full initial CPU state (all registers present).
#[derive(Debug, Clone, Deserialize)]
pub struct I8088InitialState {
    pub regs: I8088Regs,
    pub ram: Vec<(u32, u8)>,
    #[serde(default)]
    pub queue: Vec<u8>,
}

/// Sparse final CPU state (only changed registers present).
#[derive(Debug, Clone, Deserialize)]
pub struct I8088FinalState {
    pub regs: I8088SparseRegs,
    pub ram: Vec<(u32, u8)>,
    #[serde(default)]
    pub queue: Vec<u8>,
}

/// Full register set for initial state.
#[derive(Debug, Clone, Deserialize)]
pub struct I8088Regs {
    pub ax: u16,
    pub bx: u16,
    pub cx: u16,
    pub dx: u16,
    pub cs: u16,
    pub ss: u16,
    pub ds: u16,
    pub es: u16,
    pub sp: u16,
    pub bp: u16,
    pub si: u16,
    pub di: u16,
    pub ip: u16,
    pub flags: u16,
}

/// Sparse register set for final state — only changed values present.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct I8088SparseRegs {
    pub ax: Option<u16>,
    pub bx: Option<u16>,
    pub cx: Option<u16>,
    pub dx: Option<u16>,
    pub cs: Option<u16>,
    pub ss: Option<u16>,
    pub ds: Option<u16>,
    pub es: Option<u16>,
    pub sp: Option<u16>,
    pub bp: Option<u16>,
    pub si: Option<u16>,
    pub di: Option<u16>,
    pub ip: Option<u16>,
    pub flags: Option<u16>,
}

/// Per-opcode metadata from metadata.json.
/// Some opcodes have nested `reg` sub-keys for ModR/M group opcodes.
#[derive(Debug, Clone, Deserialize)]
pub struct I8088OpcodeMetadata {
    pub status: Option<String>,
    #[serde(default)]
    pub flags: Option<String>,
    #[serde(default, rename = "flags-mask")]
    pub flags_mask: Option<u16>,
    /// Nested per-reg metadata for group opcodes (80, D0, F6, etc.)
    #[serde(default)]
    pub reg: Option<std::collections::HashMap<String, I8088SubOpcodeMetadata>>,
}

/// Sub-opcode metadata within a ModR/M group.
#[derive(Debug, Clone, Deserialize)]
pub struct I8088SubOpcodeMetadata {
    pub status: Option<String>,
    #[serde(default)]
    pub flags: Option<String>,
    #[serde(default, rename = "flags-mask")]
    pub flags_mask: Option<u16>,
}

/// Top-level metadata.json structure.
#[derive(Debug, Clone, Deserialize)]
pub struct I8088Metadata {
    pub version: String,
    pub cpu: String,
    pub opcodes: std::collections::HashMap<String, I8088OpcodeMetadata>,
}

impl I8088Metadata {
    /// Look up the flags mask for a given opcode file stem (e.g. "D0.4", "00").
    /// Returns 0xFFFF if no mask is specified (all flags defined).
    pub fn flags_mask_for(&self, file_stem: &str) -> u16 {
        // File stems like "D0.4" → opcode "D0", sub "4"
        if let Some((opcode, sub)) = file_stem.split_once('.') {
            if let Some(meta) = self.opcodes.get(opcode) {
                // Check nested reg metadata first
                if let Some(reg_map) = &meta.reg {
                    if let Some(sub_meta) = reg_map.get(sub) {
                        return sub_meta.flags_mask.unwrap_or(0xFFFF);
                    }
                }
                // Fall back to parent flags_mask
                return meta.flags_mask.unwrap_or(0xFFFF);
            }
        }
        // Simple opcode like "00"
        if let Some(meta) = self.opcodes.get(file_stem) {
            return meta.flags_mask.unwrap_or(0xFFFF);
        }
        0xFFFF
    }
}

// --- 1MB TracingBus for 8088 (20-bit address space) ---

/// A bus with 1MB of memory for 8088 validation (20-bit physical addresses).
pub struct TracingBus20 {
    pub memory: Box<[u8; 0x10_0000]>,
}

impl TracingBus20 {
    pub fn new() -> Self {
        Self {
            memory: Box::new([0; 0x10_0000]),
        }
    }
}

impl Default for TracingBus20 {
    fn default() -> Self {
        Self::new()
    }
}

// --- M68000 JSON test vector types (SingleStepTests/680x0 format) ---
//
// Each test holds a full flat register file before and after one
// instruction. A7 is implicit: the SR supervisor bit selects whether `ssp`
// or `usp` is the active stack pointer. `pc` is the address of the
// instruction under test and `prefetch` holds the two words the real CPU
// has already fetched from `pc`/`pc+2` (they are not necessarily present in
// `ram`). RAM is sparse byte (address, value) pairs.

/// A single 68000 test vector.
#[derive(Debug, Clone, Deserialize)]
pub struct M68000TestCase {
    pub name: String,
    pub initial: M68000Regs,
    #[serde(rename = "final")]
    pub final_state: M68000Regs,
    /// Documented execution length in clock cycles (not compared yet:
    /// the harness is state-only).
    pub length: u32,
    // `transactions` (per-cycle bus trace) is present but unused
}

/// Full 68000 register file + memory state (initial and final use the same
/// shape; the final state is complete, not sparse).
#[derive(Debug, Clone, Deserialize)]
pub struct M68000Regs {
    pub d0: u32,
    pub d1: u32,
    pub d2: u32,
    pub d3: u32,
    pub d4: u32,
    pub d5: u32,
    pub d6: u32,
    pub d7: u32,
    pub a0: u32,
    pub a1: u32,
    pub a2: u32,
    pub a3: u32,
    pub a4: u32,
    pub a5: u32,
    pub a6: u32,
    pub usp: u32,
    pub ssp: u32,
    pub sr: u16,
    pub pc: u32,
    /// The two instruction words already in the prefetch queue.
    pub prefetch: [u16; 2],
    /// Sparse byte memory: (24-bit address, value) pairs.
    pub ram: Vec<(u32, u8)>,
}

impl M68000Regs {
    /// Data registers as an array (mirrors `M68000::d`).
    pub fn d(&self) -> [u32; 8] {
        [
            self.d0, self.d1, self.d2, self.d3, self.d4, self.d5, self.d6, self.d7,
        ]
    }

    /// Address registers A0-A6 (A7 lives in `usp`/`ssp` per the SR S bit).
    pub fn a(&self) -> [u32; 7] {
        [
            self.a0, self.a1, self.a2, self.a3, self.a4, self.a5, self.a6,
        ]
    }

    /// True if the SR supervisor bit selects SSP as the active A7.
    pub fn is_supervisor(&self) -> bool {
        self.sr & 0x2000 != 0
    }

    /// The active stack pointer (what `M68000::a[7]` should hold).
    pub fn active_sp(&self) -> u32 {
        if self.is_supervisor() {
            self.ssp
        } else {
            self.usp
        }
    }
}

// --- MB88XX JSON test vector types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mb88xxTestCase {
    pub name: String,
    pub initial: Mb88xxCpuState,
    #[serde(rename = "final")]
    pub final_state: Mb88xxCpuState,
    pub cycles: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mb88xxCpuState {
    pub pc: u8,
    pub pa: u8,
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub si: u8,
    pub st: u8,
    pub zf: u8,
    pub cf: u8,
    pub vf: u8,
    pub sf: u8,
    pub nf: u8,
    pub pio: u8,
    pub th: u8,
    pub tl: u8,
    pub tp: u8,
    pub sb: u8,
    pub stack: [u16; 4],
    pub rom: Vec<(u16, u8)>,
    pub ram: Vec<(u8, u8)>,
    pub io: Vec<(u8, u8)>,
}

impl Bus for TracingBus20 {
    type Address = u32;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u32) -> u8 {
        self.memory[(addr & 0xF_FFFF) as usize]
    }

    fn write(&mut self, _master: BusMaster, addr: u32, data: u8) {
        self.memory[(addr & 0xF_FFFF) as usize] = data;
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState::default()
    }
}

// --- 16MB word bus for 68000 (24-bit address space, 16-bit data) ---

/// A bus with 16 MB of byte memory for 68000 validation, served as 16-bit
/// big-endian words at even addresses (the 68000 bus transaction width).
pub struct TracingBus68k {
    pub memory: Box<[u8]>,
    /// Masked word addresses written through the `Bus` trait. Harnesses
    /// reuse one 16 MB bus across thousands of test cases and zero only the
    /// touched words between cases instead of memsetting the whole array.
    pub dirty_writes: Vec<u32>,
}

impl TracingBus68k {
    pub fn new() -> Self {
        Self {
            memory: vec![0; 0x100_0000].into_boxed_slice(),
            dirty_writes: Vec::new(),
        }
    }
}

impl Default for TracingBus68k {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for TracingBus68k {
    type Address = u32;
    type Data = u16;

    fn read(&mut self, _master: BusMaster, addr: u32) -> u16 {
        let i = (addr & 0x00FF_FFFE) as usize;
        u16::from_be_bytes([self.memory[i], self.memory[i + 1]])
    }

    fn write(&mut self, _master: BusMaster, addr: u32, data: u16) {
        let i = (addr & 0x00FF_FFFE) as usize;
        self.memory[i..i + 2].copy_from_slice(&data.to_be_bytes());
        self.dirty_writes.push(i as u32);
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState::default()
    }
}
