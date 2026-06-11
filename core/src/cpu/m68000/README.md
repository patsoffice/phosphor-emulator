# Motorola 68000 CPU

Instruction-level emulation of the Motorola 68000 — 16-bit data bus, 32-bit
registers, big-endian, 24-bit address space. Validated against
[SingleStepTests/680x0](https://github.com/SingleStepTests/680x0)
(state-only). Architected so the 68010/68020/68030 can be layered on later
via the `M68kVariant` gate; only 68000 behavior is implemented.

**Status: M3 complete — control flow.** Full effective-address decoder,
MOVE family, the complete integer ALU with multiply/divide and all
shifts/rotates, and the branch/jump/subroutine family. Milestones M4-M7
(bit ops, MOVEM/LEA/stack frames, exceptions, disassembler) are tracked as
beads issues under `phosphor-emulator-m68000-emulator-puk`.

## Status

| Metric           | Value                                                  |
|------------------|--------------------------------------------------------|
| Instructions     | 52 of ~80 mnemonics (M1-M3)                            |
| Addressing modes | 12 of 12                                               |
| Integration tests| 171 (+ 48 unit tests)                                  |
| Validation       | 575,801/575,801 SingleStepTests vectors (93 files)     |
| Timing           | Approximate documented cycle counts (state-accurate)   |

## Registers

| Register | Size   | Description                                             |
|----------|--------|---------------------------------------------------------|
| D0-D7    | 32-bit | Data registers (byte/word writes preserve upper bits)   |
| A0-A6    | 32-bit | Address registers (no partial-width writes)             |
| A7       | 32-bit | Active stack pointer (USP or SSP per the SR S bit)      |
| USP/SSP  | 32-bit | User / supervisor stack pointers (inactive one parked)  |
| PC       | 32-bit | Program counter (addresses masked to 24 bits on bus)    |
| SR       | 16-bit | System byte (T, S, I2-I0 mask) + CCR (X N Z V C)        |

The X (extend) flag is the subtle CCR bit: arithmetic (ADD/SUB/NEG and the
shifts) sets X = C; data movement, logical ops, compares, and plain rotates
leave it untouched; the extended ops (ADDX/SUBX/NEGX/ABCD/SBCD/NBCD/ROXx)
consume it as carry-in and set X = C on the way out. See `flags.rs` for the
full rules; every instruction doc comment states which rule it follows.

## Instruction Set (M1-M3)

| Category   | Instructions                             | Notes                                                                     |
|------------|------------------------------------------|---------------------------------------------------------------------------|
| Move       | MOVE, MOVEA, MOVEQ                       | All source/dest EA modes; MOVEA sign-extends word sources, sets no flags  |
| Arithmetic | ADD, ADDA, ADDI, SUB, SUBA, SUBI         | Both directions; ADDA/SUBA full-width, no flags; X = C                    |
| Compare    | CMP, CMPA, CMPI, CMPM, TST, CHK          | Flags only; never alter X; CHK trap entry lands in M5                     |
| Logical    | AND, ANDI, OR, ORI, EOR, EORI, NOT       | N/Z set, V/C cleared, X untouched                                         |
| Extended   | ADDX, SUBX, NEGX                         | Consume X as carry/borrow-in; Z cleared but never set                     |
| BCD        | ABCD, SBCD, NBCD                         | Hardware-exact undefined N/V/C (per-nibble correction adder)              |
| Unary      | NEG, CLR, EXT, Scc                       | Scc never alters the CCR                                                  |
| Mul/Div    | MULU, MULS, DIVU, DIVS                   | Divide overflow: V set, C cleared, N/Z/Dn unchanged; ÷0 trap lands in M5  |
| Shifts     | ASL, ASR, LSL, LSR, ROL, ROR, ROXL, ROXR | Register count mod 64; one-bit memory forms; ROL/ROR never touch X        |
| Branches   | BRA, BSR, Bcc, DBcc                      | 8/16-bit displacements from the word after the opcode; all 14 conditions  |
| Jumps      | JMP, JSR, RTS, RTR                       | Control EA modes; RTR restores the five CCR bits; none alter the CCR else |

All sizes (.b/.w/.l) where the 68000 defines them. Unimplemented opcodes
execute as 4-cycle NOPs until the illegal-instruction/line-A/line-F
exceptions land in M5; ANDI/ORI/EORI to CCR/SR are deferred with them.

## Addressing Modes

All 12 of the 68000's effective-address modes are decoded by
`addressing.rs` into a resolved `Ea` so read/write/RMW share one decode:

| Mode          | Syntax                 | Notes                                             |
|---------------|------------------------|---------------------------------------------------|
| Register      | `Dn`, `An`             | Byte access to An is illegal                      |
| Indirect      | `(An)`                 |                                                   |
| Postincrement | `(An)+`                | A7 byte accesses step by 2 (SP stays aligned)     |
| Predecrement  | `-(An)`                | Decrements before use; same A7 rule               |
| Displacement  | `d16(An)`              | Sign-extended 16-bit displacement                 |
| Indexed       | `d8(An,Xn)`            | Brief extension word; scale ignored on 68000/010  |
| Absolute      | `abs.w`, `abs.l`       | abs.w sign-extends                                |
| PC-relative   | `d16(PC)`, `d8(PC,Xn)` | Base = extension word address                     |
| Immediate     | `#imm`                 | 1 word (byte/word) or 2 words (long)              |

## Architecture

### Execution model: atomic, not per-cycle

Instructions decode and apply their full effect on the first cycle
(i8088-style), then burn the remaining documented cycles as bus-idle wait
states via `ExecState::Execute(n)`:

```rust
enum ExecState {
    Fetch,        // ready to fetch the next opcode
    Execute(u32), // burning remaining documented cycles
    Stopped,      // STOP instruction (M5), waiting for interrupt
    Halted,       // double bus fault / external halt
}
```

Per-cycle bus traces and exact prefetch behavior are not modeled. Cycle
counts follow the documented tables approximately — good enough for
real-time pacing and the state-only validation gate; refine if a machine
needs cycle-exact timing.

### Word bus

The bus interface is `Bus<Address = u32, Data = u16>`: one transaction = one
big-endian word at an even address (the real 68000 transaction width). No
other CPU in the workspace uses this instantiation; `SimpleSystem68k`,
`TestBus68k`, and `TracingBus68k` provide word-bus harnesses.

- Longs are two word transactions, high word first.
- Byte reads fetch the containing word and select the UDS/LDS half. **Byte
  writes read-modify-write the containing word** — correct for RAM and
  state validation, not faithful for side-effecting memory-mapped
  registers. Revisit if a real machine needs strobe-accurate byte writes.
- Word/long access at an odd address flags `address_error`; the actual
  vector-3 exception lands in M5. Until then the access is forced even so
  execution stays deterministic. Branch/jump/return targets at odd
  addresses flag the same way (the fault would hit the target fetch).
- Effective addresses are computed at the full 32 bits — JMP/JSR load the
  unmasked value into PC, matching hardware — and masked to 24 bits only
  at the bus (`variant`-gated for 68020+).

### Supervisor/user stack switching

`a[7]` always holds the active SP; `set_supervisor(on)` swaps it with the
parked `usp`/`ssp` exactly once per S-bit change. Every future SR
system-byte write path (MOVE to SR, RTE, exception entry) must go through
it.

## File Structure

```text
core/src/cpu/m68000/
  mod.rs         -- M68000 struct, M68kVariant, ExecState, dispatch, reset, traits
  flags.rs       -- SrFlag, interrupt mask, set_supervisor SP-swap, cc_true
  addressing.rs  -- Size/Ea, decode_ea, sized word-bus access, ea_cycles
  move_ops.rs    -- MOVE, MOVEA, MOVEQ
  alu.rs         -- shared flag cores: add/sub, extended addx/subx, logical
  alu/binary.rs  -- ADD/SUB/CMP families, AND/OR/EOR + immediates, TST,
                    ADDX/SUBX, CMPM, ABCD/SBCD/NBCD
  alu/unary.rs   -- NEG/NEGX/NOT/CLR, EXT, Scc
  alu/muldiv.rs  -- MULU/MULS, DIVU/DIVS, CHK
  alu/shift.rs   -- ASL/ASR, LSL/LSR, ROL/ROR, ROXL/ROXR
  branch.rs      -- BRA/BSR/Bcc, DBcc, JMP/JSR/RTS/RTR
```

Planned (per the design doc): `bit.rs`, `stack.rs` (M4), `exception.rs`
(M5), `disasm.rs` (M6).

## Validation

```bash
cargo test -p phosphor-core            # unit + per-group integration tests
cargo test -p phosphor-cpu-validation --release --test m68000_single_step_test
```

The TomHarte/SingleStepTests 680x0 suite is the correctness gate
(state-only: registers, SR, PC, RAM; cycles and bus transactions are not
compared). The harness gates files to implemented instructions and skips
encodings that land in later milestones (ADDQ/SUBQ) plus address-error,
divide-by-zero, and CHK-trap cases (exception entry lands in M5). Current
M1-M3 result: **575,801 passed, 0 failed** across all 93
implemented-instruction files.

The suite's vectors capture real-hardware behavior for the "undefined"
flag cases, and this core matches them exactly: the BCD instructions model
the per-nibble correction adder, divide overflow sets V and clears C while
leaving N/Z and the register untouched, and ASR with a count past the
operand width clears C/X rather than holding the sign. MAME 0.148 differs
on several of these; where they disagree, the hardware-verified vectors
win.

## Resources

- [M68000 User's Manual (M68000UM)](https://www.nxp.com/docs/en/reference-manual/MC68000UM.pdf) — instruction set, timing tables
- [M68000 Family Programmer's Reference Manual (M68000PRM)](https://www.nxp.com/docs/en/reference-manual/M68000PRM.pdf) — per-instruction flag semantics
- [SingleStepTests/680x0](https://github.com/SingleStepTests/680x0) — validation vectors (submodule at `cpu-validation/test_data/680x0`)
- [docs/designs/m68000-emulator.md](../../../../docs/designs/m68000-emulator.md) — design doc and milestone roadmap
