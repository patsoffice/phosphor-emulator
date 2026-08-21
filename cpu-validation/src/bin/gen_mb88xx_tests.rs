use std::fs;
use std::path::Path;

use phosphor_core::cpu::mb88xx::{Mb88xx, Mb88xxVariant};
use phosphor_cpu_validation::{Mb88xxCpuState, Mb88xxTestCase};
use rand::{Rng, RngExt};

const NUM_TESTS: usize = 1000;

/// MB8841 variant: 2048-byte ROM, 128-nibble RAM (largest variant).
const ROM_SIZE: usize = 2048;
const RAM_SIZE: usize = 128;

// ---------------------------------------------------------------------------
// Instruction table
// ---------------------------------------------------------------------------

struct InstrDef {
    opcode: u8,
    /// Number of machine cycles (1 or 2).
    cycles: usize,
}

impl InstrDef {
    fn file_stem(&self) -> String {
        format!("{:02x}", self.opcode)
    }

    fn label(&self) -> String {
        format!("0x{:02X}", self.opcode)
    }
}

fn all_instructions() -> Vec<InstrDef> {
    let mut v = Vec::new();

    // 1-cycle instructions: 0x00-0x3C, 0x40-0x5F, 0x70-0xFF
    for op in 0x00..=0x3Cu8 {
        v.push(InstrDef {
            opcode: op,
            cycles: 1,
        });
    }
    for op in 0x40..=0x5Fu8 {
        v.push(InstrDef {
            opcode: op,
            cycles: 1,
        });
    }
    for op in 0x70..=0xFFu8 {
        v.push(InstrDef {
            opcode: op,
            cycles: 1,
        });
    }

    // 2-cycle instructions: 0x3D (JPA), 0x3E (EN), 0x3F (DIS), 0x60-0x6F (CALL/JPL)
    for op in 0x3D..=0x3Fu8 {
        v.push(InstrDef {
            opcode: op,
            cycles: 2,
        });
    }
    for op in 0x60..=0x6Fu8 {
        v.push(InstrDef {
            opcode: op,
            cycles: 2,
        });
    }

    v
}

// ---------------------------------------------------------------------------
// Snapshot helpers
// ---------------------------------------------------------------------------

fn snapshot_cpu(cpu: &Mb88xx) -> Mb88xxCpuState {
    Mb88xxCpuState {
        pc: cpu.pc,
        pa: cpu.pa,
        a: cpu.a,
        x: cpu.x,
        y: cpu.y,
        si: cpu.si,
        st: cpu.st,
        zf: cpu.zf,
        cf: cpu.cf,
        vf: cpu.vf,
        sf: cpu.sf,
        nf: cpu.irq_pin,
        pio: cpu.pio,
        th: cpu.th,
        tl: cpu.tl,
        tp: cpu.tp,
        sb: cpu.sb,
        stack: cpu.stack,
        rom: Vec::new(),
        ram: Vec::new(),
        io: Vec::new(),
    }
}

fn build_rom_sparse(cpu: &Mb88xx, addresses: &[u16]) -> Vec<(u16, u8)> {
    addresses.iter().map(|&a| (a, cpu.peek_rom(a))).collect()
}

fn build_ram_full(cpu: &Mb88xx) -> Vec<(u8, u8)> {
    (0..RAM_SIZE as u8).map(|a| (a, cpu.peek_ram(a))).collect()
}

fn build_io(cpu: &Mb88xx) -> Vec<(u8, u8)> {
    let mut io = Vec::new();
    // K port (index 0) - input
    io.push((0, cpu.k_input));
    // O port (index 1) - output latch
    io.push((1, cpu.read_o()));
    // P port (index 2) - output
    io.push((2, cpu.read_p()));
    // R0-R3 ports (indices 3-6): store input values (MAME reads these via READPORT)
    for i in 0..4u8 {
        io.push((3 + i, cpu.r_input[i as usize]));
    }
    // SI (index 7)
    io.push((7, cpu.si_input));
    io
}

/// Returns true if the opcode is RTS (0x2C) or RTI (0x3C).
fn is_return(opcode: u8) -> bool {
    opcode == 0x2C || opcode == 0x3C
}

/// Returns true if the opcode is CALL (0x60-0x67).
fn is_call(opcode: u8) -> bool {
    matches!(opcode, 0x60..=0x67)
}

// ---------------------------------------------------------------------------
// Test generation
// ---------------------------------------------------------------------------

fn generate_opcode(rng: &mut impl Rng, instr: &InstrDef) -> Vec<Mb88xxTestCase> {
    let mut tests = Vec::with_capacity(NUM_TESTS);

    while tests.len() < NUM_TESTS {
        let mut cpu = Mb88xx::new(Mb88xxVariant::Mb8841);

        // Randomize ROM
        for addr in 0..ROM_SIZE as u16 {
            cpu.poke_rom(addr, rng.random());
        }

        // Randomize RAM (nibbles)
        for addr in 0..RAM_SIZE as u8 {
            cpu.poke_ram(addr, rng.random_range(0..=0x0Fu8));
        }

        // Randomize registers
        cpu.a = rng.random_range(0..=0x0Fu8);
        cpu.x = rng.random_range(0..=0x0Fu8);
        cpu.y = rng.random_range(0..=0x0Fu8);
        cpu.st = rng.random_range(0..=1u8);
        cpu.zf = rng.random_range(0..=1u8);
        cpu.cf = rng.random_range(0..=1u8);
        cpu.vf = rng.random_range(0..=1u8);
        cpu.sf = rng.random_range(0..=1u8);
        cpu.irq_pin = rng.random_range(0..=1u8);
        cpu.sb = rng.random_range(0..=0x0Fu8);
        cpu.th = rng.random_range(0..=0x0Fu8);
        cpu.tl = rng.random_range(0..=0x0Fu8);

        // PIO = 0: disable timer and all interrupts for clean single-step
        cpu.pio = 0;
        cpu.tp = 0;

        // Randomize R port inputs/outputs
        for i in 0..4 {
            cpu.r_input[i] = rng.random_range(0..=0x0Fu8);
            cpu.r_output[i] = rng.random_range(0..=0x0Fu8);
        }
        cpu.k_input = rng.random_range(0..=0x0Fu8);
        cpu.p_output = rng.random_range(0..=0x0Fu8);
        cpu.o_latch = rng.random();
        cpu.si_input = rng.random_range(0..=1u8);

        // Set PC to a random position that fits the instruction
        let max_pc_offset = if instr.cycles == 2 { 0x3E } else { 0x3F };
        cpu.pc = rng.random_range(0..=max_pc_offset);
        cpu.pa = rng.random_range(0..=0x1Fu8); // 5-bit PA for MB8841

        // Stack: randomize with constraints
        if is_return(instr.opcode) {
            // Need at least one entry on stack (si > 0)
            cpu.si = rng.random_range(1..=3u8);
        } else if is_call(instr.opcode) {
            // Need room on stack (si < 4)
            cpu.si = rng.random_range(0..=3u8);
        } else {
            cpu.si = rng.random_range(0..=3u8);
        }

        for i in 0..4 {
            // Stack entries: 10-bit PC + 3 flag bits in upper bits
            cpu.stack[i] = rng.random_range(0..=0xFFFFu16);
        }

        // Place opcode at current PC
        let full_pc = ((cpu.pa as u16) << 6) | cpu.pc as u16;
        cpu.poke_rom(full_pc, instr.opcode);

        // For EN (0x3E): constrain operand to avoid MAME fatalerror on
        // unsupported serial modes. With pio=0, operand becomes new PIO directly.
        // Serial bits 4-5 must be 0x00 or 0x20 (not 0x10 or 0x30).
        if instr.opcode == 0x3E {
            let next_pc = (full_pc + 1) & 0x7FF;
            let mut operand = cpu.peek_rom(next_pc);
            operand &= !0x10; // Clear bit 4 to ensure serial bits are 00 or 20
            cpu.poke_rom(next_pc, operand);
        }

        // Collect ROM addresses touched: current PC and potentially PC+1
        let mut rom_addrs: Vec<u16> = vec![full_pc];
        if instr.cycles == 2 {
            // 2-cycle instructions read an operand byte at PC+1
            let next_pc = (full_pc + 1) & 0x7FF;
            rom_addrs.push(next_pc);
        }
        // For JPA (0x3D), the second cycle reads from the address after
        // the original PC+1 (the PA byte), but doesn't use inc_pc on it.
        // Actually it reads from get_pc() which is already incremented.
        // The operand for 0x3D is at the incremented PC position.

        // For JMP (0xC0-0xFF), JMPP @A (0xB3-like, but MB88 doesn't have that)
        // no additional ROM reads beyond the opcode itself.

        // Snapshot initial state
        let mut initial = snapshot_cpu(&cpu);
        initial.rom = build_rom_sparse(&cpu, &rom_addrs);
        initial.ram = build_ram_full(&cpu);
        initial.io = build_io(&cpu);

        // Execute instruction
        cpu.execute_cycle();
        if instr.cycles == 2 {
            cpu.execute_cycle();
        }

        // Snapshot final state
        let mut final_state = snapshot_cpu(&cpu);
        final_state.rom = build_rom_sparse(&cpu, &rom_addrs);
        final_state.ram = build_ram_full(&cpu);
        final_state.io = build_io(&cpu);

        // Build test name from opcode bytes
        let name = if instr.cycles == 2 {
            let next_pc = (full_pc + 1) & 0x7FF;
            format!(
                "{:02x} {:02x}",
                instr.opcode,
                initial
                    .rom
                    .iter()
                    .find(|(a, _)| *a == next_pc)
                    .map(|(_, v)| *v)
                    .unwrap_or(0)
            )
        } else {
            format!("{:02x}", instr.opcode)
        };

        tests.push(Mb88xxTestCase {
            name,
            initial,
            final_state,
            cycles: instr.cycles,
        });
    }

    tests
}

fn generate_and_write(rng: &mut impl Rng, instr: &InstrDef, out_dir: &Path) {
    let tests = generate_opcode(rng, instr);
    let out_path = out_dir.join(format!("{}.json", instr.file_stem()));
    let json = serde_json::to_string(&tests).expect("Failed to serialize test cases");
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
        eprintln!("Usage: gen_mb88xx_tests <opcode | all>");
        eprintln!("Examples:");
        eprintln!("  gen_mb88xx_tests 3d        # opcode 0x3D (JPA)");
        eprintln!("  gen_mb88xx_tests all");
        std::process::exit(1);
    }

    let out_dir = Path::new("test_data/mb88xx");
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
