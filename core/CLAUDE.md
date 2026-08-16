# phosphor-core

CPU implementations, Bus trait, and peripheral devices. No external C dependencies (only depends on phosphor-macros).

## CPU Architecture Rules

- Instructions go in per-CPU source files; organization varies by CPU family (see `src/cpu/CLAUDE.md`)
- Opcode dispatch is in `src/cpu/<cpu>/mod.rs` via `execute_instruction()` (i8088 uses `execute_cycle()`)
- M68xx inherent-mode instructions use `if cycle == 0 { ... }` pattern
- M68xx immediate-mode instructions use `alu_imm()` helper
- Always transition to `ExecState::Fetch` when instruction completes

## Flag Conventions

- Use each CPU's flag enum (`CcFlag`, `StatusFlag`, `PswFlag`, `Flag`), never raw hex values
- All instruction doc comments must document flag behavior
- M68xx: use `set_flags_arithmetic()` for add/sub, `set_flags_logical()` for AND/OR/EOR/TST, `set_flags_shift_left()`/`set_flags_shift_right()` for shifts — via the `M68xxAlu` trait
- M68xx: V flag for shift/rotate = N XOR C (post-operation)
- Per-CPU `set_flag()` wrappers delegate to shared `cpu::flags::set_flag()` — add new CPUs the same way
- NMI edge detection uses shared `cpu::flags::detect_rising_edge()` — don't inline the pattern

## CPU-Specific Notes

- M6800 follows identical patterns to M6809 (same flag helpers, addressing mode helpers, state machine)
- M6800 has no DP register (direct mode always page 0), no Y/U registers, no multi-byte opcode prefixes

## Testing

- Tests go in `tests/<cpu>_*_test.rs`, grouped by category (e.g., `m6809_alu_binary_test.rs`)
- Use direct CPU + TestBus pattern, not Simple*System:

```rust
let mut cpu = M6809::new();
let mut bus = TestBus::new();
bus.load(0, &[0x86, 0x42]);  // LDA #$42
cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
assert_eq!(cpu.a, 0x42);
```

## Gotchas

- Tests failing with wrong PC values often need more `tick_with_bus()` calls (each cycle = one tick)
- A machine holds its CPU in a separate field from its bus state, so the borrow splits natively and the cycle dispatches at a concrete type (`docs/designs/concrete-bus-dispatch.md`). Don't reintroduce a raw-pointer split
- `BusMasterComponent`/`Cpu` take the bus as a method type parameter (`B: Bus<Address = Self::Address, Data = Self::Data> + ?Sized`), not an associated `dyn Bus` type — that is what lets a board pass a borrowed bus view. `?Sized` keeps `&mut dyn Bus` usable for a caller that wants dynamic dispatch; neither trait is object-safe
