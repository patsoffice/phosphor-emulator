# Motorola 68000 CPU

Per-clock emulation of the Motorola 68000: 16-bit data bus, 32-bit registers,
big-endian, 24-bit address space. Validated against two independently generated
vector suites, one for state and one for the per-cycle bus trace (see
[Validation](#validation)). Architected so the 68010/68020/68030 can be layered
on later via the `M68kVariant` gate; only 68000 behavior is implemented.

**Status: instruction set complete; timing per-clock.** Every 68000 instruction
is implemented and every vector of the state suite is compared, the exact
mid-instruction address-error abort included. The debugger-facing disassembler
covers the full instruction set in Motorola syntax.

The per-clock conversion is tracked as its own epic
(`docs/designs/cycle-accurate-m68000.md`), whose milestones are also numbered
M1 to M7 and are **not** the M1-M7 of the original implementation: M1 to M5 of
that epic have landed, M6 is the 68010 timing delta and M7 is board
integration.

## Status

| Metric           | Value                                                  |
|------------------|--------------------------------------------------------|
| Instructions     | complete (74 mnemonics)                                |
| Addressing modes | 12 of 12                                               |
| Integration tests| 320 (+ 57 unit tests)                                  |
| State validation | 1,000,058/1,000,060 SingleStepTests vectors (124 files)|
| Timing           | Per-clock, one bus cycle per four clocks               |
| Timing validation| 99.79% exact on length, 97.39% on transfer placement   |

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

## Instruction Set (complete)

| Category   | Instructions                             | Notes                                                                     |
|------------|------------------------------------------|---------------------------------------------------------------------------|
| Move       | MOVE, MOVEA, MOVEQ, MOVEP, SWAP, EXG     | All source/dest EA modes; MOVEP moves alternating peripheral bytes        |
| Arithmetic | ADD/A/I/Q, SUB/A/I/Q                     | Both directions; ADDA/SUBA full-width, no flags; ADDQ data 1-8; X = C     |
| Compare    | CMP, CMPA, CMPI, CMPM, TST, CHK          | Flags only; never alter X; CHK trap entry lands in M5                     |
| Logical    | AND, ANDI, OR, ORI, EOR, EORI, NOT       | N/Z set, V/C cleared, X untouched                                         |
| Extended   | ADDX, SUBX, NEGX                         | Consume X as carry/borrow-in; Z cleared but never set                     |
| BCD        | ABCD, SBCD, NBCD                         | Hardware-exact undefined N/V/C (per-nibble correction adder)              |
| Unary      | NEG, CLR, EXT, Scc, TAS                  | Scc never alters the CCR; TAS sets bit 7 after testing                    |
| Mul/Div    | MULU, MULS, DIVU, DIVS                   | Divide overflow: V set, C cleared, N/Z/Dn unchanged; ÷0 trap lands in M5  |
| Shifts     | ASL, ASR, LSL, LSR, ROL, ROR, ROXL, ROXR | Register count mod 64; one-bit memory forms; ROL/ROR never touch X        |
| Branches   | BRA, BSR, Bcc, DBcc                      | 8/16-bit displacements from the word after the opcode; all 14 conditions  |
| Jumps      | JMP, JSR, RTS, RTR                       | Control EA modes; RTR restores the five CCR bits; none alter the CCR else |
| Bit ops    | BTST, BCHG, BCLR, BSET                   | Dynamic + static forms; long mod 32 on Dn, byte mod 8 in memory; Z only   |
| Stack/addr | LEA, PEA, LINK, UNLK, MOVEM              | Full 32-bit EAs; MOVEM predec mask reversal, word loads sign-extend       |
| Exceptions | TRAP, TRAPV, ILLEGAL, RTE                | Line-A/F + divide-zero/CHK/privilege vectors; 68000 short frame           |
| System     | STOP, RESET, NOP, MOVE SR/CCR/USP        | Privileged set raises vector 8 from user mode; ANDI/ORI/EORI to CCR/SR    |

All sizes (.b/.w/.l) where the 68000 defines them. Line-A, line-F, and
ILLEGAL vector through their exceptions; the remaining unassigned
encodings inside implemented lines execute as bounded NOPs.

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

### Execution model: one tick is one clock, and a body can run twice

One `tick()` is one clock of the 68000, and the part drives one bus cycle every
four clocks and overlaps none of them. A loader takes the opcode out of the
prefetch queue and burns the addressing mode's arithmetic in front of the
instruction body; a bus unit runs the cycles nothing in the instruction waits on
(a write, whose value is already decided, and a queue refill) one per four
clocks behind it:

```rust
enum ExecState {
    Fetch,                  // ready to take the next opcode from the queue
    Lead(u32),              // an addressing mode's arithmetic, before any cycle
    LoadWait(u32),          // waiting out the refill behind the opcode
    BodyWait(u32),          // waiting out the cycles the body's last run made
    DrainPending { .. },    // running the cycles a suspended body handed over
    TrailingRefill { .. },  // the refills the instruction still owes
    Execute(u32),           // internal time with nothing on the bus
    Stopped,                // STOP executed, waiting for an interrupt
    Halted,                 // double bus fault / external halt
}
```

**An instruction body that needs a second bus cycle is unwound and run again.**
A body is straight-line code and cannot wait in the middle of itself, so a cycle
it has not run before, arriving on a clock that already has one, restores the
registers to where the body found them, spends the clocks of the cycles it did
run, and runs the body again. The second run is served its earlier cycles from a
replay log instead of from the bus, so it reaches the next one having made no
access twice and runs it on a clock of its own. Nothing is read twice, so a
device with a read side effect sees one access; nothing is written twice,
because writes go to the bus unit and the log says which are already there.

`MOVEM`'s load direction is the exception, and the only one: it moves up to
thirty-two words, more than the log holds, so it **commits** instead. Each
register it loads is declared real, the body state is re-captured with it and
the log is emptied, and a cursor outside that state says which register to
re-enter at. See `stack::MovemLoad`.

Exception entry is a body in its own right, with its own state capture and
replay log, because the instruction that faulted is over and there is nothing
left to unwind it to.

**What an instruction costs is charged from what it does on the bus**, not
looked up: four clocks for every transfer it performs plus a declared internal
time, through `finish_from_bus`. A wrong transfer count therefore moves the
clock count with it and cannot hide behind a right total.

**The prefetch queue is real** (`prefetch.rs`): two words, holding the word at
`pc` and the word after it, refilled behind every word consumed and discarded by
every control transfer. An instruction does not fetch its own opcode; it refills
behind it, which is why the recorded traces contain no opcode fetch and why a
taken branch costs the two fetches at its target. `pc` is private so that every
control transfer has to declare its flush through `set_pc_flush`: a taken branch
with a zero displacement lands where execution would have gone anyway, so no
comparison of addresses can tell a flush from a fall-through.

What is left is named rather than described, and each piece has an issue: a
queue refill the body drives itself cannot suspend, so two program reads can
land on one clock (`phosphor-emulator-d31l`); `ADDX.l`/`SUBX.l` put their refill
in front of both write words where the part puts it between them
(`phosphor-emulator-7wmg`); and the bit operations on a `Dn` destination are
data-dependent in a way this core does not yet model
(`phosphor-emulator-4sdm`). The 68010's timing delta is not built at all: a
68010 currently charges 68000 timings.

### Word bus

The bus interface is `Bus<Address = u32, Data = u16>`: one transaction = one
big-endian word at an even address (the real 68000 transaction width). No
other CPU in the workspace uses this instantiation; `SimpleSystem68k`,
`TestBus68k`, and `TracingBus68k` provide word-bus harnesses.

- Longs are two word transactions, high word first.
- **A byte access is one transaction with one strobe.** The part has no A0
  pin: it puts the address on the bus and asserts UDS for the even byte
  (D8-D15) or LDS for the odd one (D0-D7). A byte write drives one half and
  performs no read, so a write-only register sees exactly one access, and a
  device wired to the other half is not accessed at all. The `Bus16` trait
  carries this; its byte methods are required rather than provided, so no
  bus can inherit a read-modify-write by forgetting to override one.
- Word/long access at an odd address aborts the instruction at the
  faulting access and enters the vector-3 address-error exception, exactly
  like hardware (see Exceptions below). Branch/jump/return targets at odd
  addresses fault on the target fetch.
- Effective addresses are computed at the full 32 bits — JMP/JSR load the
  unmasked value into PC, matching hardware — and masked to 24 bits only
  at the bus (`variant`-gated for 68020+).

### Supervisor/user stack switching

`a[7]` always holds the active SP; `set_supervisor(on)` swaps it with the
parked `usp`/`ssp` exactly once per S-bit change. Every SR system-byte
write path (MOVE to SR, ANDI/ORI/EORI to SR, STOP, RTE, exception entry)
goes through `write_sr`/`set_supervisor`.

### Exceptions and interrupts

All exception entry funnels through `exception(vector, pushed_pc)`: copy
SR, force supervisor mode (SP swap), clear trace, push the 68000 short
frame (SR at the lowest address, then PC), vector. The stacked PC is the
next instruction for completed traps (TRAP/TRAPV/CHK), the unexecuted
opcode for illegal-instruction and privilege violations, and the divide
instruction itself for zero divide (hardware-verified quirk, along with
its clearing of N/Z/V/C before stacking).

- Interrupts are sampled at instruction boundaries from
  `InterruptState.irq_level`: level 7 is an edge-triggered NMI, levels 1-6
  are taken while above the SR mask; entry raises the mask to the taken
  level. The vector is the autovector `24 + level` unless the device
  supplies one via `irq_vector` (0xFF, the default, means autovector).
  STOP wakes through the same path.
- Address errors abort the instruction at the faulting access (side
  effects already applied stay applied) and push the seven-word group-0
  frame: a status word carrying the opcode's upper bits above R/W, I/N,
  and the function code; the faulting address; the instruction register;
  SR; and a stacked PC of `current PC - 2` for operand faults or
  `target - 4` for control transfers. Per-instruction microcode quirks
  (JSR's pushless fault, MOVE's destination rules, the low-word-first
  descending long writes of MOVE.l/-(An), ADDX/SUBX, and MOVEM) are
  modeled exactly as the hardware vectors record them. An address error
  while pushing the frame is a double bus fault: the processor halts.
- Trace (T bit) exceptions are not implemented — the suite contains no
  trace vectors; revisit if a machine needs the T bit.
- On the 68010+ (`M68kVariant::M68010`), group-1/2 exceptions stack the
  four-word format $0 frame: a vector-offset word (`vector × 4`, format
  nibble 0) above the SR+PC short frame, which RTE pops back off. The
  68000 short frame is unchanged. The 68010 group-0 bus/address-error
  frame (the larger format $8) is *not* modeled — see "68010 support".

### 68010 support

`M68kVariant::M68010` selects the 68010 used by Atari System 1 (Marble
Madness et al.). The variant gate (`is_68010_plus`) currently covers the
two 68010 behaviors that matter for a System 1 board running supervisor
code from a vector table at address 0:

- **Exception stack frame** — the format $0 vector-offset word (above);
  exception entry and RTE stay self-consistent.
- **`MOVE from SR` privileged** — vectors to the privilege handler from
  user mode (the 68000 leaves it unprivileged).

Everything else is byte-for-byte the 68000. The following 68010 additions
are **not** implemented; none are exercised by a supervisor-mode game that
leaves VBR at 0, but Phase 1 boot bring-up should watch for them and split
out a follow-up if the ROM hits one:

- **VBR (vector base register)** — fixed at 0. Vectors are fetched from
  `vector × 4` with no VBR offset.
- **`MOVEC` / `MOVES` / `RTD`** — decode to bounded NOPs (the 68000
  illegal-encoding behavior), not the real 68010 instructions. `MOVEC` is
  the usual way a program would change VBR/SFC/DFC.
- **`MOVE from CCR`** (0x42C0) — still decoded as the 68000 CLR size-11
  hole, not the 68010 instruction.
- **Group-0 (bus/address-error) format $8 frame** — faults still push the
  68000 seven-word frame. Only matters when the CPU actually faults, which
  a working game avoids.

## File Structure

```text
core/src/cpu/m68000/
  mod.rs         -- M68000 struct, M68kVariant, ExecState, the loader, the bus
                    unit, the body replay log, dispatch, reset, traits
  flags.rs       -- SrFlag, interrupt mask, set_supervisor SP-swap, cc_true
  prefetch.rs    -- the two-word queue: refill, flush, and where a refill goes
  format.rs      -- per-encoding tables the loader needs before the body runs:
                    extension words, which fetches are suppressed, leading
                    internal time
  addressing.rs  -- Size/Ea, decode_ea, sized word-bus access, ea_cycles
  move_ops.rs    -- MOVE, MOVEA, MOVEQ, MOVEP, SWAP, EXG, MOVE SR/CCR/USP
  alu.rs         -- shared flag cores: add/sub, extended addx/subx, logical
  alu/binary.rs  -- ADD/SUB/CMP families, AND/OR/EOR + immediates, TST,
                    ADDX/SUBX, CMPM, ABCD/SBCD/NBCD
  alu/unary.rs   -- NEG/NEGX/NOT/CLR, EXT, Scc, TAS
  alu/muldiv.rs  -- MULU/MULS, DIVU/DIVS, CHK
  alu/shift.rs   -- ASL/ASR, LSL/LSR, ROL/ROR, ROXL/ROXR
  branch.rs      -- BRA/BSR/Bcc, DBcc, JMP/JSR/RTS/RTR
  bit.rs         -- BTST/BCHG/BCLR/BSET (dynamic + static forms)
  stack.rs       -- LEA/PEA, LINK/UNLK, MOVEM
  exception.rs   -- exception entry, traps, privilege, RTE/STOP/RESET,
                    interrupts, address error
  disasm.rs      -- Disassemble + DebugCpu disassembly (Motorola syntax)
```

## Validation

```bash
cargo test -p phosphor-core            # unit + per-group integration tests
cargo test -p phosphor-cpu-validation --release --test m68000_single_step_test
cargo test -p phosphor-cpu-validation --release --test m68000_cycle_test
```

**Two suites, generated independently, and they check different things.** A
single generated oracle has the failure mode where subject and standard come
from the same place, with nothing to distinguish a correct implementation from
one that agrees with its source's mistakes. Where the two agree and this core
differs, this core is wrong. Where they disagree, the question is settled
against the part's own microcode, and the reason is recorded at the code it
governs, never as a fitted constant.

- **`SingleStepTests/680x0` is the state gate** (registers, SR, PC, RAM;
  documentation-derived, verified by use). Every file is enabled and every
  vector compared: **1,000,058 passed, 0 failed** across all 124 files,
  including every address-error, divide-by-zero, CHK-trap and
  privilege-violation vector. The only two skips are known-bad vectors in ASL.b
  whose expected state is unrelated to the executed instruction (suite
  generation glitches).
- **`SingleStepTests/m68000` is the per-cycle gate**, generated from a
  microcode-level implementation of the part, and it carries the bus trace:
  each instruction's total clock count and its ordered transactions with
  explicit idle time. The gate compares six rungs, each with a ratcheting
  floor: clock count, transfer kinds, transfer count, the clock each transfer
  lands on, its address, size and data, and its function code. Its `pc` is the
  generator's next-prefetch address, which runs ahead of the execution point
  and is reconciled rather than compared directly.

Both gates are reported split by population, because an aggregate held up by
the cases that touch no memory says nothing about the ones that do.

The state suite's vectors capture real-hardware behavior for the "undefined"
flag cases, and this core matches them exactly: the BCD instructions model
the per-nibble correction adder, divide overflow sets V and clears C while
leaving N/Z and the register untouched, and ASR with a count past the
operand width clears C/X rather than holding the sign. MAME 0.148 differs
on several of these; where they disagree, the hardware-verified vectors
win.

## Resources

- [M68000 User's Manual (M68000UM)](https://www.nxp.com/docs/en/reference-manual/MC68000UM.pdf) — instruction set, timing tables
- [M68000 Family Programmer's Reference Manual (M68000PRM)](https://www.nxp.com/docs/en/reference-manual/M68000PRM.pdf) — per-instruction flag semantics
- [SingleStepTests/680x0](https://github.com/SingleStepTests/680x0): state vectors (submodule at `cpu-validation/test_data/680x0`)
- [SingleStepTests/m68000](https://github.com/SingleStepTests/m68000): per-cycle bus traces (submodule at `cpu-validation/test_data/m68000`)
- [docs/designs/m68000-emulator.md](../../../../docs/designs/m68000-emulator.md): design doc and milestone roadmap
- [docs/designs/cycle-accurate-m68000.md](../../../../docs/designs/cycle-accurate-m68000.md): the per-clock conversion, its oracles and each milestone as built
