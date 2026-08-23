use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::m6502::M6502;
use phosphor_cpu_validation::{BusOp, M6502TestCase, TracingBus};

/// All 151 legal NMOS 6502 opcodes.
const LEGAL_OPCODES: &[u8] = &[
    // BRK
    0x00, // ORA
    0x01, 0x05, 0x09, 0x0D, 0x11, 0x15, 0x19, 0x1D, // ASL
    0x06, 0x0A, 0x0E, 0x16, 0x1E, // PHP
    0x08, // BPL
    0x10, // CLC
    0x18, // JSR
    0x20, // AND
    0x21, 0x25, 0x29, 0x2D, 0x31, 0x35, 0x39, 0x3D, // BIT
    0x24, 0x2C, // ROL
    0x26, 0x2A, 0x2E, 0x36, 0x3E, // PLP
    0x28, // BMI
    0x30, // SEC
    0x38, // RTI
    0x40, // EOR
    0x41, 0x45, 0x49, 0x4D, 0x51, 0x55, 0x59, 0x5D, // LSR
    0x46, 0x4A, 0x4E, 0x56, 0x5E, // PHA
    0x48, // JMP
    0x4C, 0x6C, // BVC
    0x50, // CLI
    0x58, // RTS
    0x60, // ADC
    0x61, 0x65, 0x69, 0x6D, 0x71, 0x75, 0x79, 0x7D, // ROR
    0x66, 0x6A, 0x6E, 0x76, 0x7E, // PLA
    0x68, // BVS
    0x70, // SEI
    0x78, // STA
    0x81, 0x85, 0x8D, 0x91, 0x95, 0x99, 0x9D, // STX
    0x86, 0x8E, 0x96, // STY
    0x84, 0x8C, 0x94, // DEY
    0x88, // TXA
    0x8A, // BCC
    0x90, // TYA
    0x98, // TXS
    0x9A, // LDA
    0xA1, 0xA5, 0xA9, 0xAD, 0xB1, 0xB5, 0xB9, 0xBD, // LDX
    0xA2, 0xA6, 0xAE, 0xB6, 0xBE, // LDY
    0xA0, 0xA4, 0xAC, 0xB4, 0xBC, // TAY
    0xA8, // TAX
    0xAA, // BCS
    0xB0, // CLV
    0xB8, // TSX
    0xBA, // CMP
    0xC1, 0xC5, 0xC9, 0xCD, 0xD1, 0xD5, 0xD9, 0xDD, // CPY
    0xC0, 0xC4, 0xCC, // DEC
    0xC6, 0xCE, 0xD6, 0xDE, // INY
    0xC8, // DEX
    0xCA, // BNE
    0xD0, // CLD
    0xD8, // CPX
    0xE0, 0xE4, 0xEC, // SBC
    0xE1, 0xE5, 0xE9, 0xED, 0xF1, 0xF5, 0xF9, 0xFD, // INC
    0xE6, 0xEE, 0xF6, 0xFE, // INX
    0xE8, // NOP
    0xEA, // BEQ
    0xF0, // SED
    0xF8,
];

fn run_test_case(tc: &M6502TestCase) {
    let mut cpu = M6502::new();
    let mut bus = TracingBus::new();

    // Load initial state
    cpu.pc = tc.initial.pc;
    cpu.sp = tc.initial.s;
    cpu.a = tc.initial.a;
    cpu.x = tc.initial.x;
    cpu.y = tc.initial.y;
    cpu.p = tc.initial.p;
    for &(addr, val) in &tc.initial.ram {
        bus.memory[addr as usize] = val;
    }

    // Execute one instruction, counting total ticks
    let mut total_ticks = 0;
    loop {
        total_ticks += 1;
        if cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)) {
            break;
        }
        if total_ticks > 100 {
            panic!("{}: instruction did not complete in 100 cycles", tc.name);
        }
    }

    // Assert registers
    assert_eq!(cpu.pc, tc.final_state.pc, "{}: PC", tc.name);
    assert_eq!(cpu.a, tc.final_state.a, "{}: A", tc.name);
    assert_eq!(cpu.x, tc.final_state.x, "{}: X", tc.name);
    assert_eq!(cpu.y, tc.final_state.y, "{}: Y", tc.name);
    assert_eq!(cpu.sp, tc.final_state.s, "{}: SP", tc.name);
    assert_eq!(cpu.p, tc.final_state.p, "{}: P", tc.name);

    // Assert memory
    for &(addr, expected) in &tc.final_state.ram {
        assert_eq!(
            bus.memory[addr as usize], expected,
            "{}: RAM[0x{:04X}]",
            tc.name, addr
        );
    }

    // Assert total cycle count
    assert_eq!(
        total_ticks,
        tc.cycles.len(),
        "{}: total cycle count (got {} expected {})",
        tc.name,
        total_ticks,
        tc.cycles.len()
    );

    // Assert bus cycle details (skip internal cycles)
    let expected_bus: Vec<_> = tc
        .cycles
        .iter()
        .enumerate()
        .filter(|(_, (_, _, op))| op != "internal")
        .collect();

    assert_eq!(
        bus.cycles.len(),
        expected_bus.len(),
        "{}: bus cycle count (got {} expected {})",
        tc.name,
        bus.cycles.len(),
        expected_bus.len()
    );

    for (bus_idx, (exp_idx, (exp_addr, exp_data, exp_op))) in expected_bus.iter().enumerate() {
        let actual = &bus.cycles[bus_idx];
        assert_eq!(
            actual.addr, *exp_addr,
            "{}: cycle {} (bus {}) addr",
            tc.name, exp_idx, bus_idx
        );
        assert_eq!(
            actual.data, *exp_data,
            "{}: cycle {} (bus {}) data",
            tc.name, exp_idx, bus_idx
        );
        let actual_op = match actual.op {
            BusOp::Read => "read",
            BusOp::Write => "write",
            BusOp::Internal => "internal",
        };
        assert_eq!(
            actual_op,
            exp_op.as_str(),
            "{}: cycle {} (bus {}) op",
            tc.name,
            exp_idx,
            bus_idx
        );
    }
}

#[test]
fn test_all_legal_opcodes() {
    let test_dir = phosphor_cpu_validation::vector_dir("65x02/6502/v1");
    let test_dir = test_dir.as_path();
    if !phosphor_cpu_validation::require_test_data(
        test_dir,
        "run: git submodule update --init cpu-validation/test_data/65x02",
    ) {
        return;
    }

    let mut total_tests = 0;
    let mut total_files = 0;

    for &opcode in LEGAL_OPCODES {
        let filename = format!("{:02x}.json", opcode);
        let json_path = test_dir.join(&filename);

        assert!(
            json_path.exists(),
            "Missing test file for opcode 0x{:02X}: {:?}",
            opcode,
            json_path
        );

        let json = std::fs::read_to_string(&json_path)
            .unwrap_or_else(|e| panic!("Failed to read {:?}: {}", json_path, e));
        let tests: Vec<M6502TestCase> = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("Failed to parse {:?}: {}", json_path, e));

        assert!(!tests.is_empty(), "Test file {} is empty", filename);

        for tc in &tests {
            run_test_case(tc);
        }

        total_tests += tests.len();
        total_files += 1;
    }

    eprintln!(
        "Validated {} tests across {} opcode files",
        total_tests, total_files
    );
}
