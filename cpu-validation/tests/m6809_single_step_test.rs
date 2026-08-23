use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::m6809::M6809;
use phosphor_cpu_validation::{BusOp, TestCase, TracingBus};

/// Compare one field, recording a message instead of panicking so a single bad
/// opcode cannot hide every opcode that sorts after it.
fn check<T: PartialEq + std::fmt::Debug>(
    failures: &mut Vec<String>,
    actual: T,
    expected: T,
    what: std::fmt::Arguments<'_>,
) {
    if actual != expected {
        failures.push(format!("{what}: got {actual:?} expected {expected:?}"));
    }
}

/// Replay one test case, returning a description of the first mismatch (if any).
fn run_test_case(tc: &TestCase) -> Option<String> {
    let mut failures = Vec::new();
    let mut cpu = M6809::new();
    let mut bus = TracingBus::new();

    // Load initial state
    cpu.pc = tc.initial.pc;
    cpu.s = tc.initial.s;
    cpu.u = tc.initial.u;
    cpu.a = tc.initial.a;
    cpu.b = tc.initial.b;
    cpu.dp = tc.initial.dp;
    cpu.x = tc.initial.x;
    cpu.y = tc.initial.y;
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

    // Check registers
    let f = &mut failures;
    check(f, cpu.pc, tc.final_state.pc, format_args!("PC"));
    check(f, cpu.a, tc.final_state.a, format_args!("A"));
    check(f, cpu.b, tc.final_state.b, format_args!("B"));
    check(f, cpu.dp, tc.final_state.dp, format_args!("DP"));
    check(f, cpu.x, tc.final_state.x, format_args!("X"));
    check(f, cpu.y, tc.final_state.y, format_args!("Y"));
    check(f, cpu.u, tc.final_state.u, format_args!("U"));
    check(f, cpu.s, tc.final_state.s, format_args!("S"));
    check(f, cpu.cc, tc.final_state.cc, format_args!("CC"));

    // Check memory
    for &(addr, expected) in &tc.final_state.ram {
        check(
            f,
            bus.memory[addr as usize],
            expected,
            format_args!("RAM[0x{addr:04X}]"),
        );
    }

    // Check total cycle count (internal + bus cycles)
    check(
        f,
        total_ticks,
        tc.cycles.len(),
        format_args!("total cycle count"),
    );

    // Check bus cycle details (skip internal cycles)
    let expected_bus: Vec<_> = tc
        .cycles
        .iter()
        .enumerate()
        .filter(|(_, (_, _, op))| op != "internal")
        .collect();

    check(
        f,
        bus.cycles.len(),
        expected_bus.len(),
        format_args!("bus cycle count"),
    );

    for (bus_idx, (exp_idx, (exp_addr, exp_data, exp_op))) in expected_bus.iter().enumerate() {
        let Some(actual) = bus.cycles.get(bus_idx) else {
            break;
        };
        let at = format_args!("cycle {exp_idx} (bus {bus_idx})");
        check(f, actual.addr, *exp_addr, format_args!("{at} addr"));
        check(f, actual.data, *exp_data, format_args!("{at} data"));
        let actual_op = match actual.op {
            BusOp::Read => "read",
            BusOp::Write => "write",
            BusOp::Internal => "internal",
        };
        check(f, actual_op, exp_op.as_str(), format_args!("{at} op"));
    }

    if failures.is_empty() {
        None
    } else {
        Some(format!("{}: {}", tc.name, failures.join("; ")))
    }
}

#[test]
fn test_all_opcodes() {
    let test_dir = phosphor_cpu_validation::vector_dir("m6809");
    let test_dir = test_dir.as_path();
    if !phosphor_cpu_validation::require_test_data(
        test_dir,
        "run: cargo run -p phosphor-cpu-validation --bin gen_m6809_tests -- all",
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
        "No JSON test files found. Run: cargo run -p phosphor-cpu-validation --bin gen_m6809_tests -- all"
    );

    /// Failing cases reported per opcode before truncating the rest.
    const EXAMPLES_PER_OPCODE: usize = 3;

    let mut total_tests = 0;
    let mut total_files = 0;
    // (opcode, failed, total, first few failure descriptions)
    let mut failing_opcodes: Vec<(String, usize, usize, Vec<String>)> = Vec::new();

    for json_path in &json_files {
        let json = std::fs::read_to_string(json_path)
            .unwrap_or_else(|e| panic!("Failed to read {:?}: {}", json_path, e));
        let tests: Vec<TestCase> = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("Failed to parse {:?}: {}", json_path, e));

        let file_name = json_path.file_name().unwrap().to_string_lossy();
        assert!(!tests.is_empty(), "Test file {} is empty", file_name);

        let mut failed = 0;
        let mut examples = Vec::new();
        for tc in &tests {
            if let Some(msg) = run_test_case(tc) {
                failed += 1;
                if examples.len() < EXAMPLES_PER_OPCODE {
                    examples.push(msg);
                }
            }
        }
        if failed > 0 {
            let opcode = json_path
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            failing_opcodes.push((opcode, failed, tests.len(), examples));
        }

        total_tests += tests.len();
        total_files += 1;
    }

    if !failing_opcodes.is_empty() {
        let failed_cases: usize = failing_opcodes.iter().map(|(_, n, _, _)| n).sum();
        let mut report = format!(
            "{} of {} opcode files failed ({} of {} cases):\n",
            failing_opcodes.len(),
            total_files,
            failed_cases,
            total_tests
        );
        for (opcode, failed, total, examples) in &failing_opcodes {
            report.push_str(&format!("\n  {opcode}: {failed}/{total} failed\n"));
            for example in examples {
                report.push_str(&format!("    {example}\n"));
            }
            if failed > &examples.len() {
                report.push_str(&format!("    ... and {} more\n", failed - examples.len()));
            }
        }
        panic!("{report}");
    }

    eprintln!(
        "Validated {} tests across {} opcode files",
        total_tests, total_files
    );
}
