//! `Cpu` and `BusMasterComponent` are generic over the bus, not tied to a
//! `'static` trait object.
//!
//! This is what lets a board hold its CPU beside its bus state and hand the CPU
//! a *borrowed* view of that state — the shape the concrete-bus-dispatch work
//! moves every board to (`docs/designs/concrete-bus-dispatch.md`). A view's
//! lifetime cannot be named by an associated `dyn Bus` type, so if these
//! signatures ever go back to one, this file stops compiling.

use phosphor_core::core::{Bus, BusMaster, BusMasterComponent, bus::InterruptState};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::cpu::m6809::M6809;

/// A machine in the target shape: CPU in one field, bus state in others.
struct SplitMachine<C> {
    cpu: C,
    memory: Box<[u8; 0x10000]>,
    /// Stands in for the game-specific state a real board's view also borrows
    /// (a decode latch, a scroll register), so the view is more than one field.
    write_count: u32,
}

/// The bus the CPU actually talks to: a borrow of the machine's bus fields.
struct MachineBus<'a> {
    memory: &'a mut [u8; 0x10000],
    write_count: &'a mut u32,
}

impl<C> SplitMachine<C> {
    fn new(cpu: C) -> Self {
        Self {
            cpu,
            memory: Box::new([0; 0x10000]),
            write_count: 0,
        }
    }

    /// The borrow-checked split — no raw pointers, no `unsafe`.
    fn split(&mut self) -> (&mut C, MachineBus<'_>) {
        (
            &mut self.cpu,
            MachineBus {
                memory: &mut self.memory,
                write_count: &mut self.write_count,
            },
        )
    }
}

impl Bus for MachineBus<'_> {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        self.memory[addr as usize]
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        *self.write_count += 1;
        self.memory[addr as usize] = data;
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState::default()
    }
}

#[test]
fn m6502_reset_fetches_its_vector_through_a_borrowed_bus_view() {
    let mut machine = SplitMachine::new(M6502::new());
    // Reset vector at 0xFFFC/0xFFFD, little-endian.
    machine.memory[0xFFFC] = 0x34;
    machine.memory[0xFFFD] = 0x12;

    let (cpu, mut bus) = machine.split();
    cpu.reset(&mut bus, BusMaster::Cpu(0));

    assert_eq!(
        machine.cpu.pc, 0x1234,
        "reset should have read the vector through the view"
    );
}

#[test]
fn m6809_reset_fetches_its_vector_through_a_borrowed_bus_view() {
    let mut machine = SplitMachine::new(M6809::new());
    // Reset vector at 0xFFFE/0xFFFF, big-endian.
    machine.memory[0xFFFE] = 0x12;
    machine.memory[0xFFFF] = 0x34;

    let (cpu, mut bus) = machine.split();
    cpu.reset(&mut bus, BusMaster::Cpu(0));

    assert_eq!(machine.cpu.pc, 0x1234);
}

#[test]
fn a_cycle_runs_against_a_borrowed_bus_view() {
    let mut machine = SplitMachine::new(M6809::new());
    machine.memory[0xFFFE] = 0x00;
    machine.memory[0xFFFF] = 0x00;
    // LDA #$42 ; STA $0040
    machine.memory[0x0000..0x0005].copy_from_slice(&[0x86, 0x42, 0xB7, 0x00, 0x40]);

    let (cpu, mut bus) = machine.split();
    cpu.reset(&mut bus, BusMaster::Cpu(0));
    for _ in 0..16 {
        cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    }

    assert_eq!(machine.cpu.a, 0x42);
    assert_eq!(machine.memory[0x0040], 0x42, "STA should reach the view");
    assert_eq!(
        machine.write_count, 1,
        "the view's own state should see the write too"
    );
}

/// The `dyn` path still works — boards that have not been converted yet hand
/// the CPU a `&mut dyn Bus`, and `?Sized` on the bound keeps that legal.
#[test]
fn a_trait_object_bus_still_works() {
    let mut machine = SplitMachine::new(M6809::new());
    machine.memory[0xFFFE] = 0xAB;
    machine.memory[0xFFFF] = 0xCD;

    let (cpu, mut view) = machine.split();
    let bus: &mut dyn Bus<Address = u16, Data = u8> = &mut view;
    cpu.reset(bus, BusMaster::Cpu(0));

    assert_eq!(machine.cpu.pc, 0xABCD);
}
