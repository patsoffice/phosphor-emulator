# Contributing to Phosphor Emulator

This is an educational emulator project. Contributions are welcome!

## How to Contribute

### Adding 6809 Instructions

1. Add an `op_*` method in the appropriate submodule (`alu/binary.rs`, `alu/word.rs`, `branch.rs`, `load_store.rs`, etc.)
2. Add dispatch entry in `core/src/cpu/m6809/mod.rs::execute_instruction()`
3. Implement cycle-accurate execution (use match on `cycle`)
4. Add integration test in matching `core/tests/m6809_*_test.rs` file using direct CPU testing

Example (adding a method in `alu.rs`):

```rust
pub(crate) fn op_anda_imm<B: Bus<Address=u16, Data=u8> + ?Sized>(
    &mut self, cycle: u8, bus: &mut B, master: BusMaster
) {
    match cycle {
        0 => {
            let operand = bus.read(master, self.pc);
            self.pc = self.pc.wrapping_add(1);
            self.a &= operand;
            self.set_flag(CcFlag::N, self.a & 0x80 != 0);
            self.set_flag(CcFlag::Z, self.a == 0);
            self.set_flag(CcFlag::V, false);
            self.state = ExecState::Fetch;
        }
        _ => {}
    }
}
```

### Implementing New CPUs

1. Create a new module directory in `core/src/cpu/` (e.g., `m6502/`)
2. Implement `Component`, `BusMasterComponent`, and `Cpu` traits
3. Define a `#[repr(u8)]` flag enum with `From<Flag> for u8`, and delegate `set_flag()` to `cpu::flags::set_flag()`
4. Use `cpu::flags::detect_rising_edge()` for NMI edge detection (don't inline the pattern)
5. Define registers and state machine
6. Add module export in `core/src/cpu/mod.rs`
7. Create system in `machines/src/`

### Adding Peripherals

1. Create device in `core/src/device/`
2. Implement `Component` trait
3. If needs bus access, implement `BusMasterComponent`
4. Add device to appropriate system in `machines/src/`
5. Write integration tests

### Testing Guidelines

- All new instructions MUST have integration tests
- Use direct CPU testing pattern (M6809 + TestBus) for new tests
- Tests should verify registers, memory, PC, and condition codes
- Use descriptive test names: `test_<instruction>_<addressing_mode>`
- Include edge cases (zero, negative, overflow)
- Use `CcFlag::X as u8` in assertions, not raw hex values

## Code Style

- Follow Rust standard formatting (`cargo fmt`)
- Run clippy before submitting (`cargo clippy`)
- Document public APIs with rustdoc comments
- Keep `unsafe` minimal and well-documented
- Use meaningful variable names (no single letters except registers)

## Areas Needing Help

- 6502 CPU implementation
- Z80 CPU implementation
- Peripheral devices (PIA 6820, ACIA 6850, PTM 6840)
- Debugger interface
- Expanding SingleStepTests validation to more opcodes

## Design Decisions

### Generic Bus with Associated Types

The `Bus` trait uses associated types rather than generic parameters:

```rust
pub trait Bus {
    type Address: Copy + Into<u64>;
    type Data;
    // ...
}
```

**Why?** This allows:

- Different CPUs to define their own address/data widths
- Zero runtime overhead (no dynamic dispatch for reads/writes)
- Bus implementations to be stored as trait objects when needed
- Type safety: can't accidentally mix u16 and u32 addresses

### Explicit State Machine

CPU execution uses an explicit `ExecState` enum instead of implicit counters:

```rust
enum ExecState {
    Fetch,
    Execute(u8, u8),  // opcode, cycle
    Halted { return_state: Box<ExecState>, saved_cycle: u8 },
}
```

**Why?** This makes:

- Multi-cycle instruction execution transparent and debuggable
- Halt states (TSC, WAIT) explicit in the type system
- State transitions visible in code rather than implicit
- Easier to implement save states and debugging

### Modular Trait-Based Architecture

All major components (Bus, Cpu, Component) are traits:

**Why?** This enables:

- Testing CPUs without a full system (mock buses)
- Multiple CPU implementations behind a single interface
- Easy addition of new peripherals and systems
- Composition over inheritance (Rust idiom)

### The borrow splits natively, and there is no unsafe

A CPU cycle needs two mutable borrows at once: the CPU's own registers, and the
bus it reads and writes. If a machine struct *is* its own bus, those are two
mutable borrows of one value and the borrow checker refuses.

**The answer is the field layout, not an escape hatch.** A machine holds its CPU
in one field and its bus state in another, so `self.cpu.execute_cycle(&mut
self.board, ..)` borrows two disjoint fields and compiles as ordinary safe Rust.
It also dispatches at a **concrete** bus type rather than through `&mut dyn Bus`,
which is where the performance came from.

```rust
pub fn step_cycle(&mut self) -> u32 {
    self.cpu.execute_cycle(&mut self.board, BusMaster::Cpu(0))
}
```

Form the split **once per frame**, never per cycle: a per-cycle split costs more
than the trait object it replaces. See
[docs/designs/concrete-bus-dispatch.md](docs/designs/concrete-bus-dispatch.md)
for the measurements.

This section used to document the opposite: a `*mut Self` reborrowed into
`&mut dyn Bus` inside an `unsafe` block, presented as the chosen design. That
pattern is gone from the tree and **must not come back**; `core/CLAUDE.md` says
so directly. There is no `unsafe` in `machines/src` at all.

## Troubleshooting

### Build Issues

**Problem:** Compilation errors about trait bounds

```text
error[E0277]: the trait bound `dyn Bus<Address = u16, Data = u8>: Sized` is not satisfied
```

**Solution:** Ensure trait objects use `?Sized` bound:

```rust
impl BusMasterComponent for M6809 {
    type Bus = dyn Bus<Address = u16, Data = u8>;  // Note: trait object
}
```

**Problem:** Borrow checker errors when implementing new systems

**Solution:** Use the split-borrow pattern with controlled `unsafe` (see Design Decisions above)

### Test Failures

**Problem:** Test fails with wrong PC value

```text
thread 'test_load_accumulator_immediate' panicked at 'assertion failed: `(left == right)`
  left: `1`,
 right: `2`', tests/m6809_load_store_test.rs:16:5
```

**Solution:** Check cycle count - you may need more `tick()` calls. Each instruction takes 2-4 cycles.

**Problem:** Memory doesn't contain expected value

**Solution:** Verify instruction execution order and ensure enough cycles for all instructions to complete.

### Runtime Issues

**Problem:** Infinite loop - emulator never completes

**Solution:** The 6809 doesn't auto-halt. Limit cycle count:

```rust
for _ in 0..100 { sys.tick(); }  // Limit execution
```

## Performance Notes

### Design Priorities

1. **Correctness** - Cycle-accurate emulation matching hardware behavior
2. **Clarity** - Readable, maintainable code for educational purposes
3. **Performance** - Fast enough for real-time emulation (future goal)

### Current Characteristics

- **Zero-cost abstractions** - Generic traits compile to static dispatch
- **No heap allocations** in hot paths (instruction execution)
- **Minimal branching** - State machine uses pattern matching
- **Cache-friendly** - Flat arrays for RAM/ROM
