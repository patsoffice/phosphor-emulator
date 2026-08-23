use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::m6809::M6809;
use phosphor_cpu_validation::{BusOp, CpuState, TestCase, TracingBus};
use rand::{Rng, RngExt};

const NUM_TESTS: usize = 1000;
const MAX_TICKS: usize = 200;

// --- Instruction Definition ---

#[derive(Clone, Copy, PartialEq)]
enum InstrPage {
    Page1,
    Page2,
    Page3,
}

#[derive(Clone, Copy)]
enum InstrSize {
    /// Fixed number of operand bytes after the opcode (not counting prefix).
    Fixed(u8),
    /// Indexed mode: postbyte determines variable instruction length.
    Indexed,
}

struct InstrDef {
    page: InstrPage,
    opcode: u8,
    size: InstrSize,
}

impl InstrDef {
    fn prefix_bytes(&self) -> u8 {
        match self.page {
            InstrPage::Page1 => 0,
            InstrPage::Page2 | InstrPage::Page3 => 1,
        }
    }

    fn prefix_byte(&self) -> Option<u8> {
        match self.page {
            InstrPage::Page1 => None,
            InstrPage::Page2 => Some(0x10),
            InstrPage::Page3 => Some(0x11),
        }
    }

    fn file_stem(&self) -> String {
        match self.page {
            InstrPage::Page1 => format!("{:02x}", self.opcode),
            InstrPage::Page2 => format!("10_{:02x}", self.opcode),
            InstrPage::Page3 => format!("11_{:02x}", self.opcode),
        }
    }

    fn label(&self) -> String {
        match self.page {
            InstrPage::Page1 => format!("0x{:02X}", self.opcode),
            InstrPage::Page2 => format!("0x10,0x{:02X}", self.opcode),
            InstrPage::Page3 => format!("0x11,0x{:02X}", self.opcode),
        }
    }
}

// --- Instruction Table ---

fn all_instructions() -> Vec<InstrDef> {
    use InstrPage::*;
    use InstrSize::*;

    let mut v = Vec::new();

    let mut add = |page: InstrPage, opcodes: &[u8], size: InstrSize| {
        for &op in opcodes {
            v.push(InstrDef {
                page,
                opcode: op,
                size,
            });
        }
    };

    // ============================================================
    // PAGE 1 — Inherent (0 operand bytes, total 1 byte)
    // ============================================================
    add(
        Page1,
        &[
            0x12, // NOP
            0x19, // DAA
            0x1D, // SEX
            0x39, // RTS
            0x3A, // ABX
            0x3B, // RTI
            0x3D, // MUL
            0x3F, // SWI
        ],
        Fixed(0),
    );

    // A-register inherent
    add(
        Page1,
        &[
            0x40, 0x43, 0x44, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x4C, 0x4D, 0x4F,
        ],
        Fixed(0),
    );

    // B-register inherent
    add(
        Page1,
        &[
            0x50, 0x53, 0x54, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x5C, 0x5D, 0x5F,
        ],
        Fixed(0),
    );

    // ============================================================
    // PAGE 1 — 1 operand byte (total 2 bytes)
    // ============================================================

    // ORCC, ANDCC
    add(Page1, &[0x1A, 0x1C], Fixed(1));

    // EXG, TFR
    add(Page1, &[0x1E, 0x1F], Fixed(1));

    // Short branches
    add(
        Page1,
        &[
            0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D,
            0x2E, 0x2F,
        ],
        Fixed(1),
    );

    // PSHS, PULS, PSHU, PULU
    add(Page1, &[0x34, 0x35, 0x36, 0x37], Fixed(1));

    // BSR
    add(Page1, &[0x8D], Fixed(1));

    // A-ALU immediate (8-bit)
    add(
        Page1,
        &[0x80, 0x81, 0x82, 0x84, 0x85, 0x86, 0x88, 0x89, 0x8A, 0x8B],
        Fixed(1),
    );

    // B-ALU immediate (8-bit)
    add(
        Page1,
        &[0xC0, 0xC1, 0xC2, 0xC4, 0xC5, 0xC6, 0xC8, 0xC9, 0xCA, 0xCB],
        Fixed(1),
    );

    // Direct mode (unary/shift/JMP/CLR)
    add(
        Page1,
        &[
            0x00, 0x03, 0x04, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0C, 0x0D, 0x0E, 0x0F,
        ],
        Fixed(1),
    );

    // A-side direct ALU (0x90-0x9F excl 0x9D JSR)
    add(
        Page1,
        &[
            0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0x9B, 0x9C, 0x9E,
            0x9F,
        ],
        Fixed(1),
    );

    // JSR direct
    add(Page1, &[0x9D], Fixed(1));

    // B-side direct ALU (0xD0-0xDF)
    add(
        Page1,
        &[
            0xD0, 0xD1, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xDB, 0xDC, 0xDD,
            0xDE, 0xDF,
        ],
        Fixed(1),
    );

    // ============================================================
    // PAGE 1 — 2 operand bytes (total 3 bytes)
    // ============================================================

    // LBRA, LBSR
    add(Page1, &[0x16, 0x17], Fixed(2));

    // 16-bit immediate
    add(Page1, &[0x83, 0x8C, 0x8E], Fixed(2)); // SUBD, CMPX, LDX
    add(Page1, &[0xC3, 0xCC, 0xCE], Fixed(2)); // ADDD, LDD, LDU

    // Extended mode (unary/shift/JMP/CLR)
    add(
        Page1,
        &[
            0x70, 0x73, 0x74, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x7C, 0x7D, 0x7E, 0x7F,
        ],
        Fixed(2),
    );

    // A-side extended ALU (0xB0-0xBF excl 0xBD JSR)
    add(
        Page1,
        &[
            0xB0, 0xB1, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xBB, 0xBC, 0xBE,
            0xBF,
        ],
        Fixed(2),
    );

    // JSR extended
    add(Page1, &[0xBD], Fixed(2));

    // B-side extended ALU (0xF0-0xFF)
    add(
        Page1,
        &[
            0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD,
            0xFE, 0xFF,
        ],
        Fixed(2),
    );

    // ============================================================
    // PAGE 1 — Indexed
    // ============================================================

    // LEA
    add(Page1, &[0x30, 0x31, 0x32, 0x33], Indexed);

    // Unary indexed (0x60-0x6F, excl undocumented 0x61, 0x62, 0x65, 0x6B)
    add(
        Page1,
        &[
            0x60, 0x63, 0x64, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x6C, 0x6D, 0x6E, 0x6F,
        ],
        Indexed,
    );

    // A-side indexed ALU (0xA0-0xAF, all 16)
    add(
        Page1,
        &[
            0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD,
            0xAE, 0xAF,
        ],
        Indexed,
    );

    // B-side indexed ALU (0xE0-0xEF, all 16)
    add(
        Page1,
        &[
            0xE0, 0xE1, 0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xEB, 0xEC, 0xED,
            0xEE, 0xEF,
        ],
        Indexed,
    );

    // ============================================================
    // PAGE 2 (prefix 0x10)
    // ============================================================

    // SWI2 (no operand)
    add(Page2, &[0x3F], Fixed(0));

    // Long conditional branches (2 operand bytes = 16-bit offset)
    add(
        Page2,
        &[
            0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E,
            0x2F,
        ],
        Fixed(2),
    );

    // 16-bit immediate (2 operand bytes)
    add(Page2, &[0x83, 0x8C, 0x8E, 0xCE], Fixed(2)); // CMPD, CMPY, LDY, LDS

    // Direct (1 operand byte)
    add(Page2, &[0x93, 0x9C, 0x9E, 0x9F, 0xDE, 0xDF], Fixed(1));

    // Extended (2 operand bytes)
    add(Page2, &[0xB3, 0xBC, 0xBE, 0xBF, 0xFE, 0xFF], Fixed(2));

    // Indexed
    add(Page2, &[0xA3, 0xAC, 0xAE, 0xAF, 0xEE, 0xEF], Indexed);

    // ============================================================
    // PAGE 3 (prefix 0x11)
    // ============================================================

    // SWI3 (no operand)
    add(Page3, &[0x3F], Fixed(0));

    // 16-bit immediate (2 operand bytes)
    add(Page3, &[0x83, 0x8C], Fixed(2)); // CMPU, CMPS

    // Direct (1 operand byte)
    add(Page3, &[0x93, 0x9C], Fixed(1));

    // Extended (2 operand bytes)
    add(Page3, &[0xB3, 0xBC], Fixed(2));

    // Indexed
    add(Page3, &[0xA3, 0xAC], Indexed);

    v
}

// --- Helpers ---

fn snapshot_cpu(cpu: &M6809) -> CpuState {
    CpuState {
        pc: cpu.pc,
        s: cpu.s,
        u: cpu.u,
        a: cpu.a,
        b: cpu.b,
        dp: cpu.dp,
        x: cpu.x,
        y: cpu.y,
        cc: cpu.cc,
        ram: Vec::new(),
    }
}

fn build_ram(memory: &[u8; 0x10000], addresses: &BTreeSet<u16>) -> Vec<(u16, u8)> {
    addresses
        .iter()
        .map(|&addr| (addr, memory[addr as usize]))
        .collect()
}

/// Check if an indexed postbyte is a defined addressing mode per the M6809 datasheet.
/// Undefined modes: 0x07, 0x0A, 0x0E; ,R+ and ,-R with indirect; [n16] with non-zero
/// register bits or without indirect.
fn is_valid_indexed_postbyte(postbyte: u8) -> bool {
    if postbyte & 0x80 == 0 {
        return true; // 5-bit offset, always valid
    }
    let indirect = postbyte & 0x10 != 0;
    let mode = postbyte & 0x0F;
    match mode {
        0x00 | 0x02 => !indirect, // ,R+ and ,-R: non-indirect only
        0x01 | 0x03 | 0x04 | 0x05 | 0x06 | 0x08 | 0x09 | 0x0B | 0x0C | 0x0D => true,
        0x0F => indirect && (postbyte & 0x60 == 0), // [n16]: indirect, reg bits must be 00
        _ => false,                                 // 0x07, 0x0A, 0x0E: always undefined
    }
}

/// Compute total instruction byte count for an indexed postbyte.
/// Returns: prefix_bytes + 1 (opcode) + 1 (postbyte) + extra offset bytes.
fn indexed_total_bytes(prefix_bytes: u8, postbyte: u8) -> u8 {
    let base = prefix_bytes + 2; // prefix + opcode + postbyte
    if postbyte & 0x80 == 0 {
        base // 5-bit constant offset, no extra bytes
    } else {
        let extra = match postbyte & 0x0F {
            0x08 | 0x0C => 1,        // 8-bit offset or PC-relative 8-bit
            0x09 | 0x0D | 0x0F => 2, // 16-bit offset, PC-relative 16-bit, or extended indirect
            _ => 0,                  // register offsets, auto-inc/dec, no extra bytes
        };
        base + extra
    }
}

// --- Test Generation ---

fn generate_opcode(rng: &mut impl Rng, instr: &InstrDef) -> Vec<TestCase> {
    let mut tests = Vec::with_capacity(NUM_TESTS);

    // Leave room for the maximum possible instruction size
    let max_pc = match instr.size {
        InstrSize::Fixed(n) => 0x10000u32 - (instr.prefix_bytes() as u32 + 1 + n as u32),
        InstrSize::Indexed => 0x10000u32 - (instr.prefix_bytes() as u32 + 5), // worst case: opcode + postbyte + 2-byte offset
    } as u16;

    let mut attempts = 0;
    while tests.len() < NUM_TESTS {
        attempts += 1;
        if attempts > NUM_TESTS * 10 {
            eprintln!(
                "Warning: only generated {} tests for {} (too many timeouts)",
                tests.len(),
                instr.label()
            );
            break;
        }

        let mut cpu = M6809::new();
        let mut bus = TracingBus::new();

        // Fill entire 64KB with random data
        rng.fill(&mut bus.memory[..]);

        // Randomize all registers
        cpu.a = rng.random();
        cpu.b = rng.random();
        cpu.dp = rng.random();
        cpu.x = rng.random();
        cpu.y = rng.random();
        cpu.u = rng.random();
        cpu.s = rng.random();
        cpu.cc = rng.random();
        cpu.pc = rng.random_range(0..=max_pc);

        // Place instruction bytes at PC
        let pc = cpu.pc;
        let mut offset = 0u16;
        if let Some(prefix) = instr.prefix_byte() {
            bus.memory[pc.wrapping_add(offset) as usize] = prefix;
            offset += 1;
        }
        bus.memory[pc.wrapping_add(offset) as usize] = instr.opcode;

        // For indexed instructions, skip undefined postbytes
        if matches!(instr.size, InstrSize::Indexed) {
            let postbyte_pos = pc.wrapping_add(offset + 1) as usize;
            if !is_valid_indexed_postbyte(bus.memory[postbyte_pos]) {
                continue;
            }
        }

        // For EXG/TFR, skip undefined register codes
        if instr.opcode == 0x1E || instr.opcode == 0x1F {
            let operand = bus.memory[pc.wrapping_add(offset + 1) as usize];
            let r1 = operand >> 4;
            let r2 = operand & 0x0F;
            let valid = |r: u8| matches!(r, 0..=5 | 8..=11);
            if !valid(r1) || !valid(r2) {
                continue;
            }
        }

        // Snapshot pre-execution memory
        let pre_memory = bus.memory;

        // Snapshot initial CPU state
        let mut initial = snapshot_cpu(&cpu);

        // Execute one instruction with cycle limit
        let mut all_cycles: Vec<(u16, u8, BusOp)> = Vec::new();
        let mut ticks = 0;
        let mut completed = false;
        loop {
            let before = bus.cycles.len();
            let done = cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
            ticks += 1;
            if bus.cycles.len() > before {
                for c in &bus.cycles[before..] {
                    all_cycles.push((c.addr, c.data, c.op));
                }
            } else {
                all_cycles.push((0xFFFF, 0, BusOp::Internal));
            }
            if done {
                completed = true;
                break;
            }
            if ticks >= MAX_TICKS {
                break;
            }
        }

        if !completed {
            continue; // Discard and retry (e.g., undefined indexed postbyte)
        }

        // Snapshot final CPU state
        let mut final_state = snapshot_cpu(&cpu);

        // Collect all accessed addresses (skip internal cycle sentinel 0xFFFF)
        let addresses: BTreeSet<u16> = all_cycles
            .iter()
            .filter(|(_, _, op)| *op != BusOp::Internal)
            .map(|&(addr, _, _)| addr)
            .collect();

        // Build ram fields from pre/post memory
        initial.ram = build_ram(&pre_memory, &addresses);
        final_state.ram = build_ram(&bus.memory, &addresses);

        // Build cycles array
        let cycles: Vec<(u16, u8, String)> = all_cycles
            .iter()
            .map(|&(addr, data, op)| {
                let op_str = match op {
                    BusOp::Read => "read".to_string(),
                    BusOp::Write => "write".to_string(),
                    BusOp::Internal => "internal".to_string(),
                };
                (addr, data, op_str)
            })
            .collect();

        // Build name from instruction bytes at PC
        let total_bytes = match instr.size {
            InstrSize::Fixed(n) => instr.prefix_bytes() + 1 + n,
            InstrSize::Indexed => {
                let postbyte_offset = instr.prefix_bytes() + 1;
                let postbyte = pre_memory[pc.wrapping_add(postbyte_offset as u16) as usize];
                indexed_total_bytes(instr.prefix_bytes(), postbyte)
            }
        };
        let name = (0..total_bytes as u16)
            .map(|i| format!("{:02x}", pre_memory[pc.wrapping_add(i) as usize]))
            .collect::<Vec<_>>()
            .join(" ");

        tests.push(TestCase {
            name,
            initial,
            final_state,
            cycles,
        });
    }

    tests
}

fn generate_and_write(rng: &mut impl Rng, instr: &InstrDef, out_dir: &Path) {
    let tests = generate_opcode(rng, instr);
    let out_path = out_dir.join(format!("{}.json", instr.file_stem()));
    let json = serde_json::to_string_pretty(&tests).expect("Failed to serialize test cases");
    fs::write(&out_path, json).expect("Failed to write output file");
    println!(
        "Generated {} tests for {} -> {}",
        tests.len(),
        instr.label(),
        out_path.display()
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: gen_m6809_tests <opcode | all>");
        eprintln!("Examples:");
        eprintln!("  gen_m6809_tests 86        # page 1 opcode 0x86");
        eprintln!("  gen_m6809_tests 10_8e     # page 2 opcode 0x8E");
        eprintln!("  gen_m6809_tests 11_83     # page 3 opcode 0x83");
        eprintln!("  gen_m6809_tests all");
        std::process::exit(1);
    }

    // Resolved against the crate root, not the current directory: the command
    // the validators print is run from the repo root, and a relative literal
    // there wrote the vectors one level above where the validators read them.
    let out_dir = phosphor_cpu_validation::vector_dir("m6809");
    let out_dir = out_dir.as_path();
    fs::create_dir_all(out_dir).expect("Failed to create output directory");

    let all = all_instructions();
    let mut rng = rand::rng();

    if args[1] == "all" {
        for instr in &all {
            generate_and_write(&mut rng, instr, out_dir);
        }
        println!("Generated tests for {} opcodes", all.len());
    } else {
        // Parse "86", "10_8e", "11_83" format (also handles "0x86" prefix)
        let arg = args[1].trim_start_matches("0x").trim_start_matches("0X");
        let (page, op_str) = if let Some(rest) = arg.strip_prefix("10_") {
            (InstrPage::Page2, rest)
        } else if let Some(rest) = arg.strip_prefix("11_") {
            (InstrPage::Page3, rest)
        } else {
            (InstrPage::Page1, arg)
        };
        let opcode = u8::from_str_radix(op_str, 16).unwrap_or_else(|_| {
            eprintln!("Invalid hex opcode: {}", args[1]);
            std::process::exit(1);
        });

        let instr = all
            .iter()
            .find(|i| i.page == page && i.opcode == opcode)
            .unwrap_or_else(|| {
                eprintln!("Opcode {} not found in instruction table", args[1]);
                std::process::exit(1);
            });

        generate_and_write(&mut rng, instr, out_dir);
    }
}
