# Design: Motorola 68000 CPU Emulator

## Context

The phosphor-emulator framework has cycle-accurate CPUs (6809, 6502, Z80, 6800, i8088,
i8035, mb88xx) that all plug into a common `Bus`/`Cpu`/`BusMasterComponent` interface. We
want to add a **Motorola 68000** emulator following the same structure, architected so that
**68010 / 68020 / (maybe) 68030** variants can be layered on later without a rewrite.

The 68000 is a 16-bit-data / 32-bit-register, big-endian CPU with a large orthogonal
instruction set, 12 effective-addressing modes, supervisor/user modes, and a 256-entry
vectored exception table. This is a multi-milestone effort.

**Decisions locked in with the user:**

- **Scope now:** *Foundation first* — milestone **M1** only (skeleton + full EA decoder +
  MOVE family + core ALU + reset + system wiring + validation harness). Later milestones
  (M2–M7) are scoped below as a roadmap, not built in this pass.
- **Bus model:** *Word bus* — `Bus<Address = u32, Data = u16>` (16-bit data bus, the real
  68000 transaction width). This is a **new** `Bus` instantiation; no existing CPU uses it.
- **Validation:** *TomHarte 680x0* SingleStepTests JSON suite as the correctness gate,
  reusing the existing i8088/6502/z80 SingleStepTests harness pattern.

## Architecture facts (reference points)

- CPU module shape: `core/src/cpu/<cpu>/` with `mod.rs` (struct, `ExecState`,
  `execute_cycle`, dispatch via `execute_instruction()`), `alu.rs`/category files,
  `disasm.rs`. Closest reference: `core/src/cpu/m6809/` and `core/src/cpu/i8088/`.
- A CPU implements `BusMasterComponent` (`tick_with_bus -> bool`, true at instruction
  boundary), `Cpu` (`reset`, `signal_interrupt`, `is_sleeping`), `CpuStateTrait`
  (`snapshot()`), plus `Debuggable`/`DebugCpu`. See `core/src/cpu/m6809/mod.rs:605-691`.
- `Bus` trait: `core/src/core/bus.rs:11-37` — `type Address: Copy + Into<u64>`, `type Data`.
  `bus_split!` macro (`bus.rs:80-94`) has `u16/u8` and `u32/u8` arms today.
- `machines/src/simple_system.rs` — `SimpleSystem<C>` (u16/u8) and `SimpleSystem32<C>`
  (u32/u8). No word-bus system exists yet.
- Flags: each CPU defines its own `#[repr]` flag enum; shared helpers in
  `core/src/cpu/flags.rs` (`set_flag`, `flag_is_set`, `detect_rising_edge`). The 68000 CCR
  (X N Z V C) is **different** from the m68xx `CcFlag` — needs its own enum.
- State snapshots: `core/src/cpu/state.rs` (one struct per CPU with `debug_registers()`).
- Save-state: `#[derive(Saveable)] #[save_version(N)]` + `#[save_skip(...)]` on temporaries.
- Tests: `core/tests/<cpu>_*_test.rs`, direct `CPU + TestBus` pattern; `TestBus` in
  `core/tests/common/mod.rs` is u16/u8 today. Use flag enums in assertions, never raw hex.
- Validation: `cpu-validation/` (SingleStepTests harnesses + `TracingBus`/`TracingBus20`),
  `cross-validation/` (C++ vs MAME). i8088 harness: `cpu-validation/tests/i8088_single_step_test.rs`.

## Bus / memory model (word bus, the key deviation)

Use **`Bus<Address = u32, Data = u16>`**: reads/writes are 16-bit words at **even** byte
addresses, big-endian (`mem[addr] << 8 | mem[addr+1]`). Underlying storage stays
byte-addressable; only access is word-granular.

- **Word/long access:** CPU memory helpers read/write u16 at even addresses; a long = two
  word accesses. The CPU never issues an odd address to the bus.
- **Byte access:** the CPU reads the containing word and selects the high byte (even addr /
  UDS) or low byte (odd addr / LDS). **Byte writes** do read-modify-write on the containing
  word (replace one byte, write the word back). Document this as a simplification: correct
  for RAM and for the TomHarte state-comparison gate (other byte preserved); not faithful
  for write-only/side-effecting memory-mapped registers — revisit if a real machine needs it.
- **Odd-address detection:** word/long access with `addr & 1 != 0` → address-error exception
  (vector 3). Checked in the EA layer (exception itself lands in M5).
- **24-bit masking:** mask effective addresses to 24 bits (`addr & 0x00FF_FFFF`) for M68000;
  gate on `variant` so 68020+ can use full 32 bits later.

**New infrastructure required (because no word bus exists yet):**

1. `core/src/core/bus.rs` — add a `bus_split!` arm: `u32 word => &mut dyn Bus<Address=u32, Data=u16>`.
2. `machines/src/simple_system.rs` — add `SimpleSystem68k<C>` (byte-addressable
   `Vec<u8>` storage, `read/write` serving u16 over even addresses, byte helpers for test
   loading). Alias `pub type Simple68000System = SimpleSystem68k<M68000>;`.
3. `cpu-validation/src/lib.rs` — add `TracingBus68k` (u32/u16 tracing bus, byte storage)
   mirroring `TracingBus20`.
4. `core/tests/common/mod.rs` — add `TestBus68k` (u32/u16, byte storage + `load()` helper);
   the existing `TestBus` (u16/u8) is unchanged.

## Register & flag model

`M68000` struct in `core/src/cpu/m68000/mod.rs`, `#[derive(Saveable)] #[save_version(1)]`,
field order = save layout (model `core/src/cpu/m6809/mod.rs:30-69`):

```rust
pub struct M68000 {
    pub d: [u32; 8],        // D0-D7 data registers
    pub a: [u32; 8],        // A0-A6; a[7] = ACTIVE stack pointer
    pub usp: u32,           // inactive user SP (valid while in supervisor mode)
    pub ssp: u32,           // inactive supervisor SP (valid while in user mode)
    pub pc: u32,
    pub sr: u16,            // high byte = system byte, low byte = CCR
    pub variant: M68kVariant,
    pub(crate) nmi_previous: bool,   // level-7 edge detect
    pub(crate) stopped: bool,        // STOP instruction
    pub(crate) halted: bool,         // double bus fault / external halt
    #[save_skip(default = ExecState::Fetch)] pub(crate) state: ExecState,
    #[save_skip(default)] pub(crate) opcode: u16,
}

pub enum M68kVariant { M68000, M68010, M68020, M68030 }   // only M68000 has behavior now
```

- **A7 swap:** keep active SP in `a[7]`; the other lives in `usp`/`ssp` per SR bit 13 (S).
  `set_supervisor(on)` swaps when the mode bit changes. Call from every SR/CCR write (RTE,
  MOVE to SR, ANDI/ORI/EORI to SR, exception entry, reset).
- **Flag enum** in `core/src/cpu/m68000/flags.rs` (keep clear of shared `m68xx.rs`):

```rust
#[repr(u16)]
pub enum SrFlag {
    C = 1<<0, V = 1<<1, Z = 1<<2, N = 1<<3, X = 1<<4,     // CCR (low byte)
    I0 = 1<<8, I1 = 1<<9, I2 = 1<<10,                      // interrupt mask (level 0-7)
    S = 1<<13, T = 1<<15,                                  // supervisor, trace (T1)
}
```

  Per-CPU `set_flag`/`flag_is_set` wrappers delegate to `cpu::flags::set_flag`
  (`core/CLAUDE.md` convention). Add `interrupt_mask()`/`set_interrupt_mask()`. **X flag** is
  the subtle one: arithmetic sets X = C; logical/MOVE leave X untouched; ADDX/SUBX/ROXL
  consume it. Document X in every instruction doc comment.
- `M68000State` in `core/src/cpu/state.rs` (D0-7, A0-7, USP, SSP, PC, SR + `debug_registers()`),
  re-exported at `core/src/cpu/mod.rs:22-25`.

## Effective-address layer (the heart — bulk of M1)

`core/src/cpu/m68000/addressing.rs`. 6-bit EA field = `mode:3 | reg:3`, 12 modes: `Dn`, `An`,
`(An)`, `(An)+`, `-(An)`, `d16(An)`, `d8(An,Xn)`, `abs.w`, `abs.l`, `d16(PC)`, `d8(PC,Xn)`,
`#imm`. Resolved-operand enum so read/write/RMW share one decode (model m6809 `alu.rs:24-43`,
i8088 `addressing.rs`):

```rust
pub(crate) enum Ea { DataReg(usize), AddrReg(usize), Mem(u32), Imm(u32) }
pub(crate) enum Size { Byte, Word, Long }
```

Helpers (generic over `B: Bus<Address=u32, Data=u16> + ?Sized`):

- `read_imm_word(bus) -> u16` — fetch one extension word at PC, `pc += 2` (prefetch primitive).
- `read_word_at` / `read_long_at` / `read_byte_at` + writes — big-endian over the word bus;
  byte write = RMW; word/long check `addr & 1` for address error.
- `decode_ea(bus, mode, reg, size) -> Ea` — fetch required extension words (d16, brief
  extension with index reg + sign-extended disp8 — 68000 ignores scale, gate on `variant`;
  abs.w sign-extended, abs.l two words; PC-relative; immediate 1–2 words by size); compute
  masked address.
- `ea_read(bus, ea, size) -> u32`, `ea_write(bus, ea, size, val)`.

Critical correctness details (test these hardest):

- `(An)+` / `-(An)` adjust timing (predecrement before, postincrement after); **A7 with byte
  size adjusts by 2** (keeps SP word-aligned).
- `An` reads full 32 bits even for word size; MOVEA word→long **sign-extends**, byte size is
  illegal for An.
- Writing `Dn` byte/word **preserves upper bits**.

## Instruction decode / dispatch

Match on the opcode line `(opcode >> 12) & 0xF`, then sub-decode (two-level, mirroring
m6809's big match at `core/src/cpu/m6809/mod.rs:198+`):

```text
0x1/0x2/0x3 -> MOVE.b / MOVE.l(+MOVEA) / MOVE.w(+MOVEA)   <-- M1
0x7 -> MOVEQ                                               <-- M1
0x0 -> ORI/ANDI/SUBI/ADDI/EORI/CMPI, BTST/BCHG/BCLR/BSET imm+dynamic, MOVEP
0x4 -> "misc": NEG/CLR/NOT/TST/EXT/SWAP/PEA/MOVEM/LEA/CHK/JMP/JSR/TRAP/RTS/RTE/RTR/LINK/UNLK/NOP/STOP/RESET
0x5 -> ADDQ/SUBQ/Scc/DBcc        0x6 -> BRA/BSR/Bcc
0x8 -> OR/DIVU/DIVS/SBCD          0x9 -> SUB/SUBA/SUBX
0xB -> CMP/CMPA/CMPM/EOR          0xC -> AND/MULU/MULS/ABCD/EXG
0xD -> ADD/ADDA/ADDX             0xE -> shifts/rotates
0xA / 0xF -> line-A / line-F exception (vectors 10 / 11)
```

**Execution model: atomic (i8088-style), not per-cycle (m6809-style).** 68000 instructions
are multi-word with variable extension words; a per-cycle state machine would be enormous.
The TomHarte gate compares **final state** and the harness ticks until the boundary
(`cpu-validation/tests/i8088_single_step_test.rs:82-93`), so atomic execution is sufficient.

```rust
enum ExecState { Fetch, Execute(u32), Stopped, Halted }
```

On `Fetch`: sample interrupts → read 16-bit opcode word (`pc += 2`) → decode+execute
atomically (resolving extension words via the EA layer) → `Execute(cycles)` to burn the
remaining documented cycles. A cycle-cost helper returns approximate 68000 timing; exact
counts can be refined later (state correctness first). Model `core/src/cpu/i8088/mod.rs:155-201`.

## Module layout `core/src/cpu/m68000/` (M68xx-style nested `alu/`)

```text
mod.rs          struct, M68kVariant, ExecState, execute_cycle, execute_instruction (dispatch),
                reset, BusMasterComponent/Cpu/CpuStateTrait/Debuggable/DebugCpu impls
flags.rs        SrFlag, set_flag/flag_is_set, interrupt-mask + set_supervisor SP-swap, cc_true()
addressing.rs   Size, Ea, decode_ea, ea_read/ea_write, word/long/byte mem access, odd-addr check
move_ops.rs     MOVE / MOVEA / MOVEQ  (+ MOVE to/from SR/CCR/USP, MOVEP, EXG, SWAP later)
alu/binary.rs   ADD/SUB/CMP/AND/OR/EOR (+ X/BCD variants later)   <-- ADD/SUB/CMP core in M1
alu/unary.rs    NEG/NEGX/NOT/CLR/EXT/TST/Scc                       (M2)
alu/muldiv.rs   MULU/MULS/DIVU/DIVS/CHK                            (M2)
alu/shift.rs    ASL/ASR/LSL/LSR/ROL/ROR/ROXL/ROXR                 (M2)
bit.rs          BTST/BSET/BCLR/BCHG                               (M4)
branch.rs       BRA/BSR/Bcc/DBcc/JMP/JSR/RTS/RTR                  (M3)
stack.rs        LEA/PEA/LINK/UNLK/MOVEM + exception push/pop      (M4)
exception.rs    vector table, reset frame, TRAP/CHK/RTE/interrupts/privilege/address error (M5)
disasm.rs       Disassemble + DebugCpu                            (M6)
README.md       registers, instruction set, opcode count, resources, model caveats
```

Submodules `mod`-included and re-exported from `mod.rs`/`addressing.rs` the way
`m6809/alu.rs:5-8` re-exports its submodules.

## Interrupts & exceptions (designed in M1, implemented M5)

- **Reset** (`Cpu::reset`, model `core/src/cpu/m6809/mod.rs:616-621`): S=1, T=0, mask=7; load
  SSP from vector 0 (`0x000`, long), PC from vector 1 (`0x004`, long).
- **`exception(bus, vector)`**: copy SR; set S=1 (swap A7), clear T; push 68000 short frame
  (PC long then SR word on SSP); load PC from `vector*4`. **Quarantine frame construction
  behind `match self.variant` from day one** — 68010+ use format/vector frames and the
  68000's bus/address-error 14-byte frame differs; only the 68000 short frame is built now.
- **Interrupts:** taken when level 7 (NMI) or level > mask (SR bits 8-10); autovector =
  `24 + level`, else device-supplied vector. Sample at instruction boundary.
- **`InterruptState` extension** (`core/src/core/bus.rs:39-56`): add `pub irq_level: u8`
  (0 = none, 1–7 = priority), default 0. Reuse `irq_vector` for device-supplied vector.
  Mechanical fan-out: `Default` covers most; explicit constructors (`core/tests/common/mod.rs`,
  the simple systems) gain `irq_level: 0`. One-pass change.

## Validation (TomHarte 680x0 — the gate)

Reuse the i8088 SingleStepTests pattern (`cpu-validation/tests/i8088_single_step_test.rs`):

1. `cpu-validation/src/lib.rs` — add `M68000TestCase`/`InitialState`/`Regs` serde structs
   (D0-7, A0-7, USP, SSP, SR, PC, `ram: [(u32,u8)]`, prefetch) + `TracingBus68k` (u32/u16).
2. `cpu-validation/tests/m68000_single_step_test.rs` — load initial state, tick until
   `tick_with_bus` returns true, compare final registers/RAM (state-only; defer cycle/prefetch
   trace matching). Use a `should_skip` opcode list (like `i8088_single_step_test.rs:14-46`)
   so the suite **passes incrementally** — M1 enables only MOVE/MOVEA/MOVEQ/ADD/SUB/CMP.
3. Test data under `cpu-validation/test_data/680x0/` (large, gzipped per-opcode JSON;
   downloaded). Apply the suite's per-test undefined-CCR mask.

Secondary (later, optional tie-breaker): MAME 68000 cross-validation under
`cross-validation/m68000_0148/`, mirroring the existing m6809 shim.

Also add focused hand-written `core/tests/m68000_move_test.rs` /
`m68000_alu_test.rs` for the M1 instructions — `core/CLAUDE.md` requires integration tests
per instruction; TomHarte is the broad gate, these pin specific edge cases (zero, sign
boundary 0x7F/0x80, 0x7FFF/0x8000, 0x7FFFFFFF/0x80000000, X-flag, An sign-extension).

## Milestone roadmap (only M1 built in the first pass)

- **M1 (first pass):** word-bus infra (`bus_split!` arm, `SimpleSystem68k`, `TracingBus68k`,
  `TestBus68k`) · struct + `M68kVariant` + `ExecState` + flags + `set_supervisor` ·
  **full EA decoder** · MOVE/MOVEA/MOVEQ · ADD/SUB/CMP (reg + immediate) · reset · CPU
  registration (`cpu/mod.rs`, `lib.rs`, `state.rs`) · `Simple68000System` · TomHarte harness
  gated to M1 opcodes · focused integration tests · README skeleton.
- **M2:** full integer ALU (ADDX/SUBX/ABCD/SBCD/NBCD/CMPM, ANDI/ORI/EORI, TST), unary
  (NEG/NEGX/NOT/CLR/EXT/Scc), MUL/DIV/CHK, all shifts/rotates.
- **M3:** branches/jumps/subroutines (BRA/BSR/Bcc/DBcc/JMP/JSR/RTS/RTR).
- **M4:** bit ops, MOVEM, LEA/PEA/LINK/UNLK, SWAP/EXG, ADDQ/SUBQ.
- **M5:** exceptions/interrupts/privilege/address error/STOP/RESET/RTE.
- **M6:** disassembler.
- **M7:** widen TomHarte skip-list to full coverage; optional MAME cross-validation.

Every milestone builds, passes `cargo clippy --all-features --all-targets` and `cargo fmt`,
ships tests, and updates `m68000/README.md` opcode count.

## Critical files

**Create:** `core/src/cpu/m68000/{mod,flags,addressing,move_ops}.rs`,
`core/src/cpu/m68000/alu/{binary}.rs`, `core/src/cpu/m68000/README.md`,
`cpu-validation/tests/m68000_single_step_test.rs`,
`core/tests/m68000_move_test.rs`, `core/tests/m68000_alu_test.rs`.

**Modify:** `core/src/cpu/mod.rs` (register module + state re-export),
`core/src/cpu/state.rs` (add `M68000State`), `core/src/core/bus.rs`
(`bus_split!` word arm + `InterruptState.irq_level`), `core/src/lib.rs` (re-export `M68000`),
`machines/src/simple_system.rs` (`SimpleSystem68k` + `Simple68000System`),
`cpu-validation/src/lib.rs` (`TracingBus68k` + 68000 serde structs),
`core/tests/common/mod.rs` (`TestBus68k`), `README.md` (roadmap checkbox).

## Highest-risk areas

1. **EA layer** — predecrement/postincrement timing, A7-byte-adjusts-by-2, sign-extension
   rules, An word-write sign-extension. Everything depends on it; build and test first.
2. **Word-bus byte writes (RMW)** — correct for RAM/TomHarte, not for side-effecting MMIO;
   documented caveat.
3. **X flag + BCD** — set/consumed/preserved differently across ADD/ADDX/ABCD/ROXL/logical.
4. **Exception frame variant divergence** — gate behind `match variant` now (68000 only).
5. **Atomic-execution cycle fidelity** — per-cycle bus traces / exact timings not modeled;
   fine for state-level validation and `Simple68000System`; note in README.
6. **`InterruptState` blast radius** — adding `irq_level` touches every explicit constructor.

## Issue tracking

Tracked as beads epic `phosphor-emulator-m68000-emulator-puk` with 14 dependency-wired
children, each sized to one reviewable commit:

- **M1 foundation** (`.1`–`.8`): skeleton+registration, flags/SR model, EA decoder,
  word-bus systems & test buses, MOVE family, core ALU (ADD/SUB/CMP), TomHarte validation
  harness, README.
- **Follow-ups** (`.9`–`.14`): M2 full ALU/shifts, M3 branches, M4 bit ops/MOVEM,
  M5 exceptions/interrupts, M6 disassembler, M7 full validation coverage.

`blocks` dependencies gate the DAG so only `.1` (skeleton) is initially ready; the rest
unblock as their prerequisites land.

## Verification

- `cargo build` (workspace) and `cargo clippy --all-features --all-targets` — no warnings.
- `cargo fmt --check`.
- `cargo test -p phosphor-core m68000` — focused integration tests pass.
- `cargo test -p phosphor-cpu-validation m68000_single_step` — TomHarte suite passes for the
  M1-enabled opcodes (rest in skip-list).
- Sanity-run a tiny program through `Simple68000System` (reset vector → MOVE/ADD → assert
  register state) to confirm end-to-end stepping.
