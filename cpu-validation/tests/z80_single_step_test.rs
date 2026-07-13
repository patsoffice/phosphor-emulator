use std::path::Path;

use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::z80::Z80;
use phosphor_cpu_validation::{TracingBus, Z80CpuState, Z80TestCase};

fn load_initial_state(cpu: &mut Z80, s: &Z80CpuState) {
    cpu.a = s.a;
    cpu.f = s.f;
    cpu.b = s.b;
    cpu.c = s.c;
    cpu.d = s.d;
    cpu.e = s.e;
    cpu.h = s.h;
    cpu.l = s.l;
    cpu.i = s.i;
    cpu.r = s.r;
    cpu.ix = s.ix;
    cpu.iy = s.iy;
    cpu.sp = s.sp;
    cpu.pc = s.pc;
    cpu.memptr = s.wz;
    cpu.iff1 = s.iff1 != 0;
    cpu.iff2 = s.iff2 != 0;
    cpu.im = s.im;
    cpu.ei_delay = s.ei != 0;
    cpu.p = s.p != 0;
    cpu.q = s.q;
    cpu.halted = false;

    // Shadow registers: stored as 16-bit pairs in JSON
    cpu.a_prime = (s.af_prime >> 8) as u8;
    cpu.f_prime = s.af_prime as u8;
    cpu.b_prime = (s.bc_prime >> 8) as u8;
    cpu.c_prime = s.bc_prime as u8;
    cpu.d_prime = (s.de_prime >> 8) as u8;
    cpu.e_prime = s.de_prime as u8;
    cpu.h_prime = (s.hl_prime >> 8) as u8;
    cpu.l_prime = s.hl_prime as u8;
}

fn run_test_case(tc: &Z80TestCase) -> Option<String> {
    let mut cpu = Z80::new();
    let mut bus = TracingBus::new();

    load_initial_state(&mut cpu, &tc.initial);

    // Load initial RAM
    for &(addr, val) in &tc.initial.ram {
        bus.memory[addr as usize] = val;
    }

    // Load port data for I/O instructions
    for &(addr, data, ref dir) in &tc.ports {
        let d = dir.chars().next().unwrap_or('r');
        bus.port_queue.push((addr, data, d));
    }

    // Execute one instruction, counting total ticks
    let mut total_ticks = 0;
    loop {
        total_ticks += 1;
        if cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)) {
            break;
        }
        if total_ticks > 200 {
            return Some(format!(
                "{}: instruction did not complete in 200 cycles",
                tc.name
            ));
        }
    }

    let fs = &tc.final_state;

    // Check registers — return first mismatch
    macro_rules! check {
        ($got:expr, $exp:expr, $name:expr) => {
            if $got != $exp {
                return Some(format!(
                    "{}: {} (got 0x{:X} exp 0x{:X})",
                    tc.name, $name, $got as u64, $exp as u64
                ));
            }
        };
    }

    check!(cpu.a, fs.a, "A");
    check!(cpu.f, fs.f, "F");
    check!(cpu.b, fs.b, "B");
    check!(cpu.c, fs.c, "C");
    check!(cpu.d, fs.d, "D");
    check!(cpu.e, fs.e, "E");
    check!(cpu.h, fs.h, "H");
    check!(cpu.l, fs.l, "L");
    check!(cpu.i, fs.i, "I");
    check!(cpu.r, fs.r, "R");
    check!(cpu.ix, fs.ix, "IX");
    check!(cpu.iy, fs.iy, "IY");
    check!(cpu.sp, fs.sp, "SP");
    check!(cpu.pc, fs.pc, "PC");
    check!(cpu.memptr, fs.wz, "WZ");
    check!(cpu.iff1 as u8, if fs.iff1 != 0 { 1 } else { 0 }, "IFF1");
    check!(cpu.iff2 as u8, if fs.iff2 != 0 { 1 } else { 0 }, "IFF2");
    check!(cpu.im, fs.im, "IM");
    check!(cpu.ei_delay as u8, if fs.ei != 0 { 1 } else { 0 }, "EI");
    check!(cpu.p as u8, if fs.p != 0 { 1 } else { 0 }, "P");
    check!(cpu.q, fs.q, "Q");

    // Shadow registers
    let af_prime = ((cpu.a_prime as u16) << 8) | cpu.f_prime as u16;
    let bc_prime = ((cpu.b_prime as u16) << 8) | cpu.c_prime as u16;
    let de_prime = ((cpu.d_prime as u16) << 8) | cpu.e_prime as u16;
    let hl_prime = ((cpu.h_prime as u16) << 8) | cpu.l_prime as u16;
    check!(af_prime, fs.af_prime, "AF'");
    check!(bc_prime, fs.bc_prime, "BC'");
    check!(de_prime, fs.de_prime, "DE'");
    check!(hl_prime, fs.hl_prime, "HL'");

    // Check memory
    for &(addr, expected) in &fs.ram {
        if bus.memory[addr as usize] != expected {
            return Some(format!(
                "{}: RAM[0x{:04X}] (got 0x{:02X} exp 0x{:02X})",
                tc.name, addr, bus.memory[addr as usize], expected
            ));
        }
    }

    // Check total cycle count
    if total_ticks != tc.cycles.len() {
        return Some(format!(
            "{}: cycles (got {} exp {})",
            tc.name,
            total_ticks,
            tc.cycles.len()
        ));
    }

    None
}

#[test]
fn test_all_z80_opcodes() {
    let test_dir = Path::new("test_data/z80/v1");
    if !test_dir.exists() {
        panic!(
            "No SingleStepTests data. Run: git submodule update --init cpu-validation/test_data/z80"
        );
    }

    let mut entries: Vec<_> = std::fs::read_dir(test_dir)
        .expect("Failed to read test directory")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    let mut total_tests = 0;
    let mut total_files = 0;
    let mut failed_tests = 0;
    let mut failed_files = std::collections::BTreeSet::new();
    let mut first_failures: Vec<String> = Vec::new();

    for entry in &entries {
        let filename = entry.file_name();
        let filename_str = filename.to_string_lossy();

        let json_path = entry.path();
        let json = std::fs::read_to_string(&json_path)
            .unwrap_or_else(|e| panic!("Failed to read {:?}: {}", json_path, e));
        let tests: Vec<Z80TestCase> = serde_json::from_str(&json)
            .unwrap_or_else(|e| panic!("Failed to parse {:?}: {}", json_path, e));

        assert!(!tests.is_empty(), "Test file {} is empty", filename_str);

        for tc in &tests {
            if let Some(err) = run_test_case(tc) {
                failed_tests += 1;
                if !failed_files.contains(&filename_str.to_string()) {
                    failed_files.insert(filename_str.to_string());
                    if first_failures.len() < 50 {
                        first_failures.push(err);
                    }
                }
            }
        }

        total_tests += tests.len();
        total_files += 1;
    }

    eprintln!(
        "\nZ80 SingleStepTests: {} passed, {} failed across {} files",
        total_tests - failed_tests,
        failed_tests,
        total_files
    );

    if !first_failures.is_empty() {
        eprintln!("\nFirst failure per file ({} files):", failed_files.len());
        for err in &first_failures {
            eprintln!("  {}", err);
        }
    }

    if failed_tests > 0 {
        panic!(
            "{} tests failed across {} files (out of {} tests in {} files)",
            failed_tests,
            failed_files.len(),
            total_tests,
            total_files
        );
    }
}
