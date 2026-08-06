# CPU Implementation Guidelines

## File Organization (per CPU)

All CPUs have:
- `mod.rs` - State machine, opcode dispatch via `execute_instruction()` (i8088 uses `execute_cycle()`)
- `alu.rs` - Flag helpers, addressing mode helpers, module re-exports
- `disasm.rs` - Disassembler for debug output

**M6809/M6800** (M68xx family — nested `alu/` subdirectory):
- `alu/binary.rs` - ADD, SUB, CMP, ADC, SBC, AND, ORA, EOR, BIT
- `alu/unary.rs` - NEG, COM, CLR, INC, DEC, TST
- `alu/shift.rs` - ASL, ASR, LSR, ROL, ROR
- `branch.rs`, `load_store.rs`, `stack.rs`
- M6809 also has: `transfer.rs`, `alu/word.rs` (16-bit ops)

**M6502** (flat layout — no `alu/` subdirectory):
- `binary.rs`, `unary.rs`, `shift.rs` at CPU root level
- `branch.rs`, `load_store.rs`, `stack.rs`

**Z80** (different instruction categories):
- `bit.rs` (BIT/SET/RES), `block.rs` (LDIR/CPIR/INIR/OTIR family)
- `branch.rs`, `load_store.rs`, `stack.rs`

**I8088** (pipeline-based execution):
- `decode.rs`, `execute.rs`, `addressing.rs`, `registers.rs`, `flags.rs`

**I8035** (minimal):
- `branch.rs`, `load_store.rs`

## Shared Modules

- `flags.rs` - `set_flag()`, `flag_is_set()`, `detect_rising_edge()` — shared by all CPUs
- `m68xx.rs` - `M68xxAlu` trait — shared ALU operations for M6800/M6809 family
- `m68xx_alu_macros.rs` - `m68xx_alu_acc!` / `m68xx_alu_rmw!` / `m68xx_alu_inherent!` — table
  generators for the M6800/M6809 ALU opcode wrappers

## M68xx ALU Opcode Tables

The M6800/M6809 `alu/{binary,unary,shift}.rs` files are macro tables, one row per opcode:

```rust
m68xx_alu_acc! { @opcode
    /// SUBA direct (0x90): ... N, Z, V, C affected.
    op_suba_direct => alu_direct, perform_sub, A;
}
```

- Add an opcode by adding a row, not a function — the macro emits the same name `mod.rs` dispatches to
- The doc comment is part of the row and is required (see the flag conventions in `core/CLAUDE.md`)
- `@opcode` selects the M6809 helper signature (opcode byte ahead of the cycle counter); M6800 omits it
- `m68xx_alu_inherent!` takes `@no_operand` (CLR) and `@flags_only` (TST) markers
- Anything with its own cycle state machine (MUL, DAA, ABX, all of M6809 `alu/word.rs`) stays hand-written

## Adding a New Instruction

1. Implement the operation in the appropriate `alu/*.rs` or `load_store.rs` file
2. Add the opcode dispatch entry in `mod.rs` `execute_instruction()`
3. Add integration tests in `tests/<cpu>_*_test.rs`
4. Update the CPU's `README.md` opcode count

## README Maintenance

- Each CPU directory has its own README.md documenting architecture, instruction set, and resources
- Update when adding instructions, addressing modes, or changing opcode counts
