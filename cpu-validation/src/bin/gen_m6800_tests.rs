use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::m6800::M6800;
use phosphor_cpu_validation::{BusOp, M6800CpuState, M6800TestCase, TracingBus};
use rand::{Rng, RngExt};

const NUM_TESTS: usize = 1000;
const MAX_TICKS: usize = 200;

// --- Instruction Definition ---

#[derive(Clone, Copy)]
enum InstrSize {
    /// Fixed number of operand bytes after the opcode.
    Fixed(u8),
}

struct InstrDef {
    opcode: u8,
    size: InstrSize,
}

impl InstrDef {
    fn file_stem(&self) -> String {
        format!("{:02x}", self.opcode)
    }

    fn label(&self) -> String {
        format!("0x{:02X}", self.opcode)
    }
}

// --- Instruction Table ---

fn all_instructions() -> Vec<InstrDef> {
    use InstrSize::*;

    let mut v = Vec::new();

    let mut add = |opcodes: &[u8], size: InstrSize| {
        for &op in opcodes {
            v.push(InstrDef { opcode: op, size });
        }
    };

    // ============================================================
    // Inherent (0 operand bytes)
    // ============================================================

    // NOP
    add(&[0x01], Fixed(0));

    // Transfer / Flag / Misc (2 cycles)
    // NOTE: TAP (0x06), CLI (0x0E), SEI (0x0F) excluded — mame4all ONE_MORE_INSN()
    //   executes the next instruction inline, making single-step cross-validation impossible
    // NOTE: TPA (0x07) excluded — phosphor correctly sets CC bits 6-7 to 1 (real hardware),
    //   mame4all does not, causing A register mismatch
    add(
        &[
            0x0A, 0x0B, 0x0C, 0x0D, // CLV, SEV, CLC, SEC
            0x10, 0x11, // SBA, CBA
            0x16, 0x17, // TAB, TBA
            0x19, // DAA
            0x1B, // ABA
        ],
        Fixed(0),
    );

    // 16-bit register ops (4 cycles)
    add(
        &[
            0x08, 0x09, // INX, DEX
            0x30, 0x31, // TSX, INS
            0x34, 0x35, // DES, TXS
        ],
        Fixed(0),
    );

    // Stack push/pull (4 cycles)
    add(&[0x32, 0x33, 0x36, 0x37], Fixed(0)); // PULA, PULB, PSHA, PSHB

    // RTS (5 cycles), RTI (10 cycles)
    add(&[0x39, 0x3B], Fixed(0));

    // SWI (12 cycles)
    add(&[0x3F], Fixed(0));

    // NOTE: WAI (0x3E) excluded — halts until interrupt

    // A-register shift/unary inherent (2 cycles)
    add(
        &[
            0x40, 0x43, 0x44, 0x46, 0x47, 0x48, 0x49, 0x4A, 0x4C, 0x4D, 0x4F,
        ],
        Fixed(0),
    );

    // B-register shift/unary inherent (2 cycles)
    add(
        &[
            0x50, 0x53, 0x54, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x5C, 0x5D, 0x5F,
        ],
        Fixed(0),
    );

    // ============================================================
    // Relative branches (1 operand byte)
    // ============================================================
    add(
        &[
            0x20, // BRA
            0x22, 0x23, 0x24, 0x25, 0x26, 0x27, // BHI, BLS, BCC, BCS, BNE, BEQ
            0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E,
            0x2F, // BVC, BVS, BPL, BMI, BGE, BLT, BGT, BLE
        ],
        Fixed(1),
    );

    // BSR (8 cycles)
    add(&[0x8D], Fixed(1));

    // ============================================================
    // Immediate 8-bit (1 operand byte)
    // ============================================================

    // A-side ALU immediate
    add(
        &[
            0x80, 0x81, 0x82, // SUBA, CMPA, SBCA
            0x84, 0x85, 0x86, // ANDA, BITA, LDAA
            0x88, 0x89, 0x8A, 0x8B, // EORA, ADCA, ORAA, ADDA
        ],
        Fixed(1),
    );

    // B-side ALU immediate
    add(
        &[
            0xC0, 0xC1, 0xC2, // SUBB, CMPB, SBCB
            0xC4, 0xC5, 0xC6, // ANDB, BITB, LDAB
            0xC8, 0xC9, 0xCA, 0xCB, // EORB, ADCB, ORAB, ADDB
        ],
        Fixed(1),
    );

    // ============================================================
    // Immediate 16-bit (2 operand bytes)
    // ============================================================
    add(&[0x8C, 0x8E], Fixed(2)); // CPX, LDS
    add(&[0xCE], Fixed(2)); // LDX

    // ============================================================
    // Direct mode (1 operand byte, page 0)
    // ============================================================

    // A-side direct ALU
    add(
        &[
            0x90, 0x91, 0x92, // SUBA, CMPA, SBCA
            0x94, 0x95, 0x96, 0x97, // ANDA, BITA, LDAA, STAA
            0x98, 0x99, 0x9A, 0x9B, // EORA, ADCA, ORAA, ADDA
        ],
        Fixed(1),
    );

    // 16-bit direct
    add(&[0x9C, 0x9E, 0x9F], Fixed(1)); // CPX, LDS, STS

    // B-side direct ALU
    add(
        &[
            0xD0, 0xD1, 0xD2, // SUBB, CMPB, SBCB
            0xD4, 0xD5, 0xD6, 0xD7, // ANDB, BITB, LDAB, STAB
            0xD8, 0xD9, 0xDA, 0xDB, // EORB, ADCB, ORAB, ADDB
        ],
        Fixed(1),
    );

    // 16-bit direct
    add(&[0xDE, 0xDF], Fixed(1)); // LDX, STX

    // ============================================================
    // Indexed mode (1 operand byte = unsigned offset from X)
    // ============================================================

    // Unary indexed (7 cycles)
    add(
        &[
            0x60, 0x63, 0x64, 0x66, 0x67, 0x68, 0x69, 0x6A, 0x6C, 0x6D, 0x6E, 0x6F,
        ],
        Fixed(1),
    );

    // A-side indexed ALU
    add(
        &[
            0xA0, 0xA1, 0xA2, // SUBA, CMPA, SBCA
            0xA4, 0xA5, 0xA6, 0xA7, // ANDA, BITA, LDAA, STAA
            0xA8, 0xA9, 0xAA, 0xAB, // EORA, ADCA, ORAA, ADDA
        ],
        Fixed(1),
    );

    // 16-bit indexed
    add(&[0xAC, 0xAD, 0xAE, 0xAF], Fixed(1)); // CPX, JSR, LDS, STS

    // B-side indexed ALU
    add(
        &[
            0xE0, 0xE1, 0xE2, // SUBB, CMPB, SBCB
            0xE4, 0xE5, 0xE6, 0xE7, // ANDB, BITB, LDAB, STAB
            0xE8, 0xE9, 0xEA, 0xEB, // EORB, ADCB, ORAB, ADDB
        ],
        Fixed(1),
    );

    // 16-bit indexed
    add(&[0xEE, 0xEF], Fixed(1)); // LDX, STX

    // ============================================================
    // Extended mode (2 operand bytes = 16-bit address)
    // ============================================================

    // Unary extended (6 cycles)
    add(
        &[
            0x70, 0x73, 0x74, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x7C, 0x7D, 0x7E, 0x7F,
        ],
        Fixed(2),
    );

    // A-side extended ALU
    add(
        &[
            0xB0, 0xB1, 0xB2, // SUBA, CMPA, SBCA
            0xB4, 0xB5, 0xB6, 0xB7, // ANDA, BITA, LDAA, STAA
            0xB8, 0xB9, 0xBA, 0xBB, // EORA, ADCA, ORAA, ADDA
        ],
        Fixed(2),
    );

    // 16-bit extended
    add(&[0xBC, 0xBD, 0xBE, 0xBF], Fixed(2)); // CPX, JSR, LDS, STS

    // B-side extended ALU
    add(
        &[
            0xF0, 0xF1, 0xF2, // SUBB, CMPB, SBCB
            0xF4, 0xF5, 0xF6, 0xF7, // ANDB, BITB, LDAB, STAB
            0xF8, 0xF9, 0xFA, 0xFB, // EORB, ADCB, ORAB, ADDB
        ],
        Fixed(2),
    );

    // 16-bit extended
    add(&[0xFE, 0xFF], Fixed(2)); // LDX, STX

    v
}

// --- Helpers ---

fn snapshot_cpu(cpu: &M6800) -> M6800CpuState {
    M6800CpuState {
        pc: cpu.pc,
        sp: cpu.sp,
        a: cpu.a,
        b: cpu.b,
        x: cpu.x,
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

// --- Test Generation ---

fn generate_opcode(rng: &mut impl Rng, instr: &InstrDef) -> Vec<M6800TestCase> {
    let mut tests = Vec::with_capacity(NUM_TESTS);

    let InstrSize::Fixed(operand_bytes) = instr.size;
    let total_instr_bytes = 1u32 + operand_bytes as u32;
    let max_pc = (0x10000u32 - total_instr_bytes) as u16;

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

        let mut cpu = M6800::new();
        let mut bus = TracingBus::new();

        // Fill entire 64KB with random data
        rng.fill(&mut bus.memory[..]);

        // Randomize all registers
        cpu.a = rng.random();
        cpu.b = rng.random();
        cpu.x = rng.random();
        cpu.sp = rng.random();
        cpu.cc = rng.random();
        cpu.pc = rng.random_range(0..=max_pc);

        // Place opcode at PC
        let pc = cpu.pc;
        bus.memory[pc as usize] = instr.opcode;

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
            continue;
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
        let name = (0..total_instr_bytes as u16)
            .map(|i| format!("{:02x}", pre_memory[pc.wrapping_add(i) as usize]))
            .collect::<Vec<_>>()
            .join(" ");

        tests.push(M6800TestCase {
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
        eprintln!("Usage: gen_m6800_tests <opcode | all>");
        eprintln!("Examples:");
        eprintln!("  gen_m6800_tests 86        # opcode 0x86 (LDAA imm)");
        eprintln!("  gen_m6800_tests all");
        std::process::exit(1);
    }

    let out_dir = Path::new("test_data/m6800");
    fs::create_dir_all(out_dir).expect("Failed to create output directory");

    let all = all_instructions();
    let mut rng = rand::rng();

    if args[1] == "all" {
        for instr in &all {
            generate_and_write(&mut rng, instr, out_dir);
        }
        println!("Generated tests for {} opcodes", all.len());
    } else {
        let arg = args[1].trim_start_matches("0x").trim_start_matches("0X");
        let opcode = u8::from_str_radix(arg, 16).unwrap_or_else(|_| {
            eprintln!("Invalid hex opcode: {}", args[1]);
            std::process::exit(1);
        });

        let instr = all.iter().find(|i| i.opcode == opcode).unwrap_or_else(|| {
            eprintln!("Opcode 0x{:02X} not found in instruction table", opcode);
            std::process::exit(1);
        });

        generate_and_write(&mut rng, instr, out_dir);
    }
}
