use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::i8035::I8035;
use phosphor_cpu_validation::{BusOp, I8035CpuState, I8035TestCase, TracingBus};
use rand::{Rng, RngExt};

const NUM_TESTS: usize = 1000;
const MAX_TICKS: usize = 20;

/// I8035 RAM mask for 64-byte internal RAM.
const RAM_SIZE: usize = 64;

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
    // 1-byte instructions (0 operand bytes)
    // ============================================================

    // NOP
    add(&[0x00], Fixed(0));

    // Accumulator unary
    add(
        &[
            0x07, // DEC A
            0x17, // INC A
            0x27, // CLR A
            0x37, // CPL A
            0x47, // SWAP A
            0x57, // DA A
            0x67, // RRC A
            0x77, // RR A
            0xE7, // RL A
            0xF7, // RLC A
        ],
        Fixed(0),
    );

    // Status flag ops
    add(
        &[
            0x97, // CLR C
            0xA7, // CPL C
            0x85, // CLR F0
            0x95, // CPL F0
            0xA5, // CLR F1
            0xB5, // CPL F1
        ],
        Fixed(0),
    );

    // Register INC/DEC
    add(&[0x10, 0x11], Fixed(0)); // INC @Ri
    add(&[0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F], Fixed(0)); // INC Rn
    add(&[0xC8, 0xC9, 0xCA, 0xCB, 0xCC, 0xCD, 0xCE, 0xCF], Fixed(0)); // DEC Rn

    // Register ALU (1-cycle, 0 operand bytes)
    add(&[0x60, 0x61], Fixed(0)); // ADD A,@Ri
    add(&[0x68, 0x69, 0x6A, 0x6B, 0x6C, 0x6D, 0x6E, 0x6F], Fixed(0)); // ADD A,Rn
    add(&[0x70, 0x71], Fixed(0)); // ADDC A,@Ri
    add(&[0x78, 0x79, 0x7A, 0x7B, 0x7C, 0x7D, 0x7E, 0x7F], Fixed(0)); // ADDC A,Rn
    add(&[0x40, 0x41], Fixed(0)); // ORL A,@Ri
    add(&[0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F], Fixed(0)); // ORL A,Rn
    add(&[0x50, 0x51], Fixed(0)); // ANL A,@Ri
    add(&[0x58, 0x59, 0x5A, 0x5B, 0x5C, 0x5D, 0x5E, 0x5F], Fixed(0)); // ANL A,Rn
    add(&[0xD0, 0xD1], Fixed(0)); // XRL A,@Ri
    add(&[0xD8, 0xD9, 0xDA, 0xDB, 0xDC, 0xDD, 0xDE, 0xDF], Fixed(0)); // XRL A,Rn

    // Data movement - register (1-cycle)
    add(&[0xF0, 0xF1], Fixed(0)); // MOV A,@Ri
    add(&[0xF8, 0xF9, 0xFA, 0xFB, 0xFC, 0xFD, 0xFE, 0xFF], Fixed(0)); // MOV A,Rn
    add(&[0xA0, 0xA1], Fixed(0)); // MOV @Ri,A
    add(&[0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD, 0xAE, 0xAF], Fixed(0)); // MOV Rn,A
    add(&[0x20, 0x21], Fixed(0)); // XCH A,@Ri
    add(&[0x28, 0x29, 0x2A, 0x2B, 0x2C, 0x2D, 0x2E, 0x2F], Fixed(0)); // XCH A,Rn
    add(&[0x30, 0x31], Fixed(0)); // XCHD A,@Ri
    add(&[0x42], Fixed(0)); // MOV A,T
    add(&[0x62], Fixed(0)); // MOV T,A
    add(&[0xC7], Fixed(0)); // MOV A,PSW
    add(&[0xD7], Fixed(0)); // MOV PSW,A

    // Control instructions (1-cycle)
    add(&[0xC5, 0xD5], Fixed(0)); // SEL RB0, SEL RB1
    add(&[0xE5, 0xF5], Fixed(0)); // SEL MB0, SEL MB1
    add(&[0x05, 0x15], Fixed(0)); // EN I, DIS I
    add(&[0x25, 0x35], Fixed(0)); // EN TCNTI, DIS TCNTI
    add(&[0x45, 0x55, 0x65], Fixed(0)); // STRT CNT, STRT T, STOP TCNT

    // Returns (1-byte, 2-cycle)
    add(&[0x83, 0x93], Fixed(0)); // RET, RETR

    // Port I/O (1-byte, 2-cycle)
    add(&[0x02], Fixed(0)); // OUTL BUS,A
    add(&[0x08], Fixed(0)); // INS A,BUS
    add(&[0x09, 0x0A], Fixed(0)); // IN A,P1, IN A,P2
    add(&[0x39, 0x3A], Fixed(0)); // OUTL P1,A, OUTL P2,A

    // External memory (1-byte, 2-cycle)
    add(&[0x80, 0x81], Fixed(0)); // MOVX A,@Ri
    add(&[0x90, 0x91], Fixed(0)); // MOVX @Ri,A
    add(&[0xA3], Fixed(0)); // MOVP A,@A
    add(&[0xE3], Fixed(0)); // MOVP3 A,@A
    add(&[0xB3], Fixed(0)); // JMPP @A

    // Expander ports (1-byte, 2-cycle)
    add(&[0x0C, 0x0D, 0x0E, 0x0F], Fixed(0)); // MOVD A,Pp
    add(&[0x3C, 0x3D, 0x3E, 0x3F], Fixed(0)); // MOVD Pp,A
    add(&[0x8C, 0x8D, 0x8E, 0x8F], Fixed(0)); // ORLD Pp,A
    add(&[0x9C, 0x9D, 0x9E, 0x9F], Fixed(0)); // ANLD Pp,A

    // ============================================================
    // 2-byte instructions (1 operand byte)
    // ============================================================

    // Immediate ALU
    add(&[0x03], Fixed(1)); // ADD A,#data
    add(&[0x13], Fixed(1)); // ADDC A,#data
    add(&[0x43], Fixed(1)); // ORL A,#data
    add(&[0x53], Fixed(1)); // ANL A,#data
    add(&[0xD3], Fixed(1)); // XRL A,#data

    // Immediate loads
    add(&[0x23], Fixed(1)); // MOV A,#data
    add(&[0xB0, 0xB1], Fixed(1)); // MOV @Ri,#data
    add(&[0xB8, 0xB9, 0xBA, 0xBB, 0xBC, 0xBD, 0xBE, 0xBF], Fixed(1)); // MOV Rn,#data

    // Port read-modify-write (2-byte: opcode + immediate)
    add(&[0x88, 0x89, 0x8A], Fixed(1)); // ORL BUS/P1/P2,#data
    add(&[0x98, 0x99, 0x9A], Fixed(1)); // ANL BUS/P1/P2,#data

    // Unconditional jumps (2-byte: opcode encodes page bits + addr byte)
    add(&[0x04, 0x24, 0x44, 0x64, 0x84, 0xA4, 0xC4, 0xE4], Fixed(1)); // JMP
    add(&[0x14, 0x34, 0x54, 0x74, 0x94, 0xB4, 0xD4, 0xF4], Fixed(1)); // CALL

    // DJNZ (2-byte)
    add(&[0xE8, 0xE9, 0xEA, 0xEB, 0xEC, 0xED, 0xEE, 0xEF], Fixed(1)); // DJNZ Rn

    // Conditional jumps - flags (2-byte)
    add(&[0xF6], Fixed(1)); // JC
    add(&[0xE6], Fixed(1)); // JNC
    add(&[0xC6], Fixed(1)); // JZ
    add(&[0x96], Fixed(1)); // JNZ
    add(&[0xB6], Fixed(1)); // JF0
    add(&[0x76], Fixed(1)); // JF1

    // Conditional jumps - pins/interrupts (2-byte)
    add(&[0x36], Fixed(1)); // JT0
    add(&[0x26], Fixed(1)); // JNT0
    add(&[0x56], Fixed(1)); // JT1
    add(&[0x46], Fixed(1)); // JNT1
    add(&[0x16], Fixed(1)); // JTF
    add(&[0x86], Fixed(1)); // JNI

    // Bit test jumps (2-byte)
    add(&[0x12, 0x32, 0x52, 0x72, 0x92, 0xB2, 0xD2, 0xF2], Fixed(1)); // JBb

    v
}

// --- Helpers ---

fn snapshot_cpu(cpu: &I8035) -> I8035CpuState {
    I8035CpuState {
        a: cpu.a,
        pc: cpu.pc,
        psw: cpu.psw,
        f1: cpu.f1,
        t: cpu.t,
        dbbb: cpu.dbbb,
        p1: cpu.p1,
        p2: cpu.p2,
        a11: cpu.a11,
        a11_pending: cpu.a11_pending,
        timer_enabled: cpu.timer_enabled,
        counter_enabled: cpu.counter_enabled,
        timer_overflow: cpu.timer_overflow,
        int_enabled: cpu.int_enabled,
        tcnti_enabled: cpu.tcnti_enabled,
        in_interrupt: cpu.in_interrupt,
        ram: Vec::new(),
        internal_ram: Vec::new(),
    }
}

fn build_ram(memory: &[u8; 0x10000], addresses: &BTreeSet<u16>) -> Vec<(u16, u8)> {
    addresses
        .iter()
        .map(|&addr| (addr, memory[addr as usize]))
        .collect()
}

fn build_internal_ram(ram: &[u8; 256]) -> Vec<(u8, u8)> {
    (0..RAM_SIZE as u8).map(|i| (i, ram[i as usize])).collect()
}

/// Returns true if the opcode is RET (0x83) or RETR (0x93).
fn is_return(opcode: u8) -> bool {
    opcode == 0x83 || opcode == 0x93
}

/// Returns true if the opcode is CALL.
fn is_call(opcode: u8) -> bool {
    matches!(
        opcode,
        0x14 | 0x34 | 0x54 | 0x74 | 0x94 | 0xB4 | 0xD4 | 0xF4
    )
}

// --- Test Generation ---

fn generate_opcode(rng: &mut impl Rng, instr: &InstrDef) -> Vec<I8035TestCase> {
    let mut tests = Vec::with_capacity(NUM_TESTS);

    let InstrSize::Fixed(operand_bytes) = instr.size;
    let total_instr_bytes = 1u32 + operand_bytes as u32;
    // PC is 12-bit (0x000-0xFFF), instruction must fit within that range
    let max_pc = (0x1000u32 - total_instr_bytes) as u16;

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

        let mut cpu = I8035::new();
        let mut bus = TracingBus::new();

        // Fill entire 64KB with random data (used for program memory + I/O)
        rng.fill(&mut bus.memory[..]);

        // Randomize CPU registers
        cpu.a = rng.random();
        cpu.pc = rng.random_range(0..=max_pc);
        cpu.t = rng.random();
        cpu.f1 = rng.random_bool(0.5);
        cpu.dbbb = rng.random();
        cpu.p1 = rng.random();
        cpu.p2 = rng.random();
        cpu.a11 = rng.random_bool(0.5);
        cpu.a11_pending = rng.random_bool(0.5);
        cpu.timer_overflow = rng.random_bool(0.3);

        // Randomize PSW: [CY, AC, F0, BS, 1, SP2..SP0]
        // Keep SP in valid range for the opcode
        let psw_upper = rng.random::<u8>() & 0xF0;
        if is_return(instr.opcode) {
            // RET/RETR need SP > 0 (there must be something to pop)
            let sp = rng.random_range(1..=7u8);
            cpu.psw = psw_upper | sp;
        } else if is_call(instr.opcode) {
            // CALL needs SP < 8 (room to push)
            let sp = rng.random_range(0..=6u8);
            cpu.psw = psw_upper | sp;
        } else {
            cpu.psw = psw_upper | rng.random_range(0..=7u8);
        }

        // Randomize internal RAM (first 64 bytes)
        for i in 0..RAM_SIZE {
            cpu.ram[i] = rng.random();
        }

        // For indirect addressing, R0/R1 must point within RAM range
        let bank_offset = if cpu.psw & 0x10 != 0 { 0x18 } else { 0x00 };
        cpu.ram[bank_offset] &= cpu.ram_mask;
        cpu.ram[bank_offset + 1] &= cpu.ram_mask;

        // For RET/RETR, ensure valid stack entry exists
        if is_return(instr.opcode) {
            let sp = cpu.psw & 0x07;
            let stack_addr = 2 * (sp - 1) + 8;
            // Low byte: return PC[7:0] — within 12-bit range
            cpu.ram[stack_addr as usize] = rng.random();
            // High byte: PSW[7:4] | PC[11:8]
            cpu.ram[(stack_addr + 1) as usize] = rng.random();
        }

        // Keep timer and counter disabled to avoid side effects during test
        cpu.timer_enabled = false;
        cpu.counter_enabled = false;
        // Keep interrupts disabled to avoid interrupt preemption
        cpu.int_enabled = false;
        cpu.tcnti_enabled = false;
        cpu.in_interrupt = false;

        // For port-read opcodes, populate the TracingBus port queue with the
        // current latch value so io_read returns it instead of the 0xFF fallback.
        bus.port_queue.clear();
        bus.port_index = 0;
        match instr.opcode {
            0x08 => bus.port_queue.push((0x100, cpu.dbbb, 'r')), // INS A,BUS
            0x09 => bus.port_queue.push((0x101, cpu.p1, 'r')),   // IN A,P1
            0x0A => bus.port_queue.push((0x102, cpu.p2, 'r')),   // IN A,P2
            _ => {}
        }

        // Place opcode at PC
        let pc = cpu.pc;
        bus.memory[pc as usize] = instr.opcode;

        // Snapshot pre-execution memory and internal RAM
        let pre_memory = bus.memory;
        let pre_internal_ram = cpu.ram;

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

        // Collect all accessed external bus addresses
        let addresses: BTreeSet<u16> = all_cycles
            .iter()
            .filter(|(_, _, op)| *op != BusOp::Internal)
            .map(|&(addr, _, _)| addr)
            .collect();

        // Build ram fields from pre/post external memory
        initial.ram = build_ram(&pre_memory, &addresses);
        final_state.ram = build_ram(&bus.memory, &addresses);

        // Build internal RAM snapshots (all 64 bytes)
        initial.internal_ram = build_internal_ram(&pre_internal_ram);
        final_state.internal_ram = build_internal_ram(&cpu.ram);

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

        tests.push(I8035TestCase {
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
        eprintln!("Usage: gen_i8035_tests <opcode | all>");
        eprintln!("Examples:");
        eprintln!("  gen_i8035_tests 68        # opcode 0x68 (ADD A,R0)");
        eprintln!("  gen_i8035_tests all");
        std::process::exit(1);
    }

    let out_dir = Path::new("test_data/i8035");
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
