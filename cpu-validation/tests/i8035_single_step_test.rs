//! I8035 (MCS-48) single-step validation against generated vectors.
//!
//! Same standing as the MB88xx suite: the vectors come from the CPU they are
//! replayed against, so on freshly generated data most of this is a round trip.
//! What it is actually for is the local case, where `cpu-validation/test_data/`
//! persists across commits and a change is replayed against vectors the
//! previous code produced.
//!
//! Two assertions here are not pure round trips even on same-session vectors,
//! because the replay path differs from the generation path:
//!
//! - External memory is loaded only at the addresses the vector recorded, not
//!   randomized wholesale, so an access the generator failed to record shows up
//!   as a wrong result rather than passing silently.
//! - The bus trace is compared cycle by cycle, so two errors that cancel in the
//!   tick total still fail.
//!
//! The oracle proper is `cross-validation/bin/validate_i8035`.

use phosphor_core::core::{BusMaster, BusMasterComponent};
use phosphor_core::cpu::i8035::I8035;
use phosphor_cpu_validation::{BusOp, I8035TestCase, Mismatches, TracingBus};

/// Matches the generator's own ceiling. An instruction that has not finished by
/// here did not finish at all, and the tick-count check reports it.
const MAX_TICKS: usize = 20;

fn run_case(tc: &I8035TestCase) -> Option<String> {
    let s = &tc.initial;
    let mut cpu = I8035::new();
    let mut bus = TracingBus::new();

    cpu.a = s.a;
    cpu.pc = s.pc;
    cpu.psw = s.psw;
    cpu.f1 = s.f1;
    cpu.t = s.t;
    cpu.dbbb = s.dbbb;
    cpu.p1 = s.p1;
    cpu.p2 = s.p2;
    cpu.a11 = s.a11;
    cpu.a11_pending = s.a11_pending;
    cpu.timer_enabled = s.timer_enabled;
    cpu.counter_enabled = s.counter_enabled;
    cpu.timer_overflow = s.timer_overflow;
    cpu.int_enabled = s.int_enabled;
    cpu.tcnti_enabled = s.tcnti_enabled;
    cpu.in_interrupt = s.in_interrupt;

    for &(addr, val) in &s.internal_ram {
        cpu.ram[addr as usize] = val;
    }
    for &(addr, val) in &s.ram {
        bus.memory[addr as usize] = val;
    }

    // The three port-reading opcodes need the bus port queue seeded, or
    // TracingBus::io_read falls back to 0xFF and A comes out wrong on exactly
    // those three. The vector format carries no ports array, so rebuild the
    // queue from the opcode under test and the initial latches, which is what
    // the generator seeded it with.
    match bus.memory[s.pc as usize] {
        0x08 => bus.port_queue.push((0x100, s.dbbb, 'r')), // INS A,BUS
        0x09 => bus.port_queue.push((0x101, s.p1, 'r')),   // IN A,P1
        0x0A => bus.port_queue.push((0x102, s.p2, 'r')),   // IN A,P2
        _ => {}
    }

    let mut ticks = 0;
    while ticks < MAX_TICKS {
        ticks += 1;
        if cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)) {
            break;
        }
    }

    let mut m = Mismatches::default();
    let f = &tc.final_state;
    m.check(cpu.a, f.a, format_args!("A"));
    m.check(cpu.pc, f.pc, format_args!("PC"));
    m.check(cpu.psw, f.psw, format_args!("PSW"));
    m.check(cpu.f1, f.f1, format_args!("F1"));
    m.check(cpu.t, f.t, format_args!("T"));
    m.check(cpu.dbbb, f.dbbb, format_args!("DBBB"));
    m.check(cpu.p1, f.p1, format_args!("P1"));
    m.check(cpu.p2, f.p2, format_args!("P2"));
    m.check(cpu.a11, f.a11, format_args!("A11"));
    m.check(cpu.a11_pending, f.a11_pending, format_args!("A11 pending"));
    m.check(
        cpu.timer_enabled,
        f.timer_enabled,
        format_args!("timer enabled"),
    );
    m.check(
        cpu.counter_enabled,
        f.counter_enabled,
        format_args!("counter enabled"),
    );
    m.check(
        cpu.timer_overflow,
        f.timer_overflow,
        format_args!("timer overflow"),
    );
    m.check(
        cpu.int_enabled,
        f.int_enabled,
        format_args!("interrupts enabled"),
    );
    m.check(
        cpu.tcnti_enabled,
        f.tcnti_enabled,
        format_args!("TCNTI enabled"),
    );
    m.check(
        cpu.in_interrupt,
        f.in_interrupt,
        format_args!("in interrupt"),
    );

    for &(addr, expected) in &f.internal_ram {
        m.check(
            cpu.ram[addr as usize],
            expected,
            format_args!("internal RAM[0x{addr:02X}]"),
        );
    }
    for &(addr, expected) in &f.ram {
        m.check(
            bus.memory[addr as usize],
            expected,
            format_args!("RAM[0x{addr:04X}]"),
        );
    }

    // Total ticks, then the bus cycles in order. The trace is the part that
    // catches a pair of errors whose effect on the total cancels.
    m.check(ticks, tc.cycles.len(), format_args!("total cycle count"));

    let expected_bus: Vec<_> = tc
        .cycles
        .iter()
        .enumerate()
        .filter(|(_, (_, _, op))| op != "internal")
        .collect();
    m.check(
        bus.cycles.len(),
        expected_bus.len(),
        format_args!("bus cycle count"),
    );

    for (bus_idx, (exp_idx, (exp_addr, exp_data, exp_op))) in expected_bus.iter().enumerate() {
        let Some(actual) = bus.cycles.get(bus_idx) else {
            break;
        };
        let at = format_args!("cycle {exp_idx} (bus {bus_idx})");
        m.check(actual.addr, *exp_addr, format_args!("{at} addr"));
        m.check(actual.data, *exp_data, format_args!("{at} data"));
        let actual_op = match actual.op {
            BusOp::Read => "read",
            BusOp::Write => "write",
            BusOp::Internal => "internal",
        };
        m.check(actual_op, exp_op.as_str(), format_args!("{at} op"));
    }

    m.into_report(&tc.name)
}

#[test]
fn test_all_opcodes() {
    phosphor_cpu_validation::run_vector_suite::<I8035TestCase, _>(
        "i8035",
        "run: cargo run -p phosphor-cpu-validation --bin gen_i8035_tests -- all",
        run_case,
    );
}
