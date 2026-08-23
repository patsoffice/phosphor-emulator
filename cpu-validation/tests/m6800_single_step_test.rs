use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::m6800::M6800;
use phosphor_cpu_validation::{BusOp, M6800TestCase, TracingBus};

fn run_test_case(tc: &M6800TestCase) {
    let mut cpu = M6800::new();
    let mut bus = TracingBus::new();

    // Load initial state
    cpu.pc = tc.initial.pc;
    cpu.sp = tc.initial.sp;
    cpu.a = tc.initial.a;
    cpu.b = tc.initial.b;
    cpu.x = tc.initial.x;
    cpu.cc = tc.initial.cc;
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
    }

    // Assert registers
    assert_eq!(cpu.pc, tc.final_state.pc, "{}: PC", tc.name);
    assert_eq!(cpu.a, tc.final_state.a, "{}: A", tc.name);
    assert_eq!(cpu.b, tc.final_state.b, "{}: B", tc.name);
    assert_eq!(cpu.x, tc.final_state.x, "{}: X", tc.name);
    assert_eq!(cpu.sp, tc.final_state.sp, "{}: SP", tc.name);
    assert_eq!(cpu.cc, tc.final_state.cc, "{}: CC", tc.name);

    // Assert memory
    for &(addr, expected) in &tc.final_state.ram {
        assert_eq!(
            bus.memory[addr as usize], expected,
            "{}: RAM[0x{:04X}]",
            tc.name, addr
        );
    }

    // Assert total cycle count (internal + bus cycles)
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
fn test_all_opcodes() {
    let test_dir = phosphor_cpu_validation::vector_dir("m6800");
    let test_dir = test_dir.as_path();
    if !phosphor_cpu_validation::require_test_data(
        test_dir,
        "run: cargo run -p phosphor-cpu-validation --bin gen_m6800_tests -- all",
    ) {
        return;
    }

    let mut json_files: Vec<_> = std::fs::read_dir(test_dir)
        .expect("Failed to read test data directory")
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("json") {
                Some(path)
            } else {
                None
            }
        })
        .collect();

    json_files.sort();

    assert!(
        !json_files.is_empty(),
        "No JSON test files found. Run: cargo run -p phosphor-cpu-validation --bin gen_m6800_tests -- all"
    );

    let mut total_tests = 0;
    let mut total_files = 0;

    for json_path in &json_files {
        let json = std::fs::read_to_string(json_path)
            .unwrap_or_else(|e| panic!("Failed to read {:?}: {}", json_path, e));
        let tests: Vec<M6800TestCase> = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("Failed to parse {:?}: {}", json_path, e));

        let file_name = json_path.file_name().unwrap().to_string_lossy();
        assert!(!tests.is_empty(), "Test file {} is empty", file_name);

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
