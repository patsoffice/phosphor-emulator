# M6800 Cross-Validation

Validates phosphor-core's M6800 test vectors against
[mame4all](https://github.com/mamedev/mame)'s M6800 implementation, extracted
as a standalone reference emulator.

## Results

192 opcodes validated, 192,000 test vectors (1,000 per opcode):
**191,996 passed (99.998%), 4 failed** (all BRA branch-to-self, see below).

## Prerequisites

- C++17 compiler (clang++ or g++)

## Setup

```bash
# Build
make -C cross-validation bin/validate_m6800

# Generate test vectors (from anywhere; the generator resolves its output
# against the crate root, and prints the absolute path it wrote to)
cargo run -p phosphor-cpu-validation --release --bin gen_m6800_tests -- all
```

## Usage

```bash
# Validate a single opcode
./cross-validation/bin/validate_m6800 cpu-validation/test_data/m6800/80.json

# Validate all opcodes
./cross-validation/bin/validate_m6800 cpu-validation/test_data/m6800/*.json
```

## What It Validates

For each test case, the harness:
1. Sets all CPU registers and 64KB memory to the initial state
2. Executes one instruction using mame4all's M6800
3. Compares final registers (PC, A, B, X, SP, CC & 0x3F)
4. Compares final memory at all accessed addresses
5. Compares total cycle count

CC bits 6-7 are undefined on real M6800 hardware (always read as 1), so the
harness masks CC with 0x3F before comparison.

Bus-level cycle traces (per-cycle address/data/direction) are not validated
since mame4all does not expose per-cycle bus activity.

## Excluded Opcodes (5)

These opcodes are excluded from cross-validation due to mame4all implementation
quirks that make single-step comparison impossible. In each case, phosphor's
behavior matches the M6800 datasheet.

### TAP (0x06), CLI (0x0E), SEI (0x0F) — `ONE_MORE_INSN`

Mame4all's implementations of these three opcodes call the `ONE_MORE_INSN()`
macro, which fetches and executes the *next* instruction inline within the same
`m6800_execute()` call. This is an optimization for interrupt timing edge cases:
when TAP/CLI/SEI changes the interrupt mask, the effect shouldn't take hold
until after the following instruction.

This means mame4all executes 2 instructions instead of 1, making single-step
cross-validation impossible. Phosphor handles interrupt masking through its own
state machine without executing extra instructions.

### TPA (0x07) — CC bits 6-7

On real M6800 hardware, CC register bits 6-7 always read as 1. Phosphor
correctly implements `A = CC | 0xC0` for TPA. Mame4all implements `A = CC`
without the mask, causing the A register to differ after execution.

### WAI (0x3E) — Halts CPU

WAI halts the CPU until an interrupt arrives. Since cross-validation uses
single-step execution with no interrupt sources, this opcode cannot complete.

## Phosphor Bugs Found and Fixed

Cross-validation uncovered three bugs in phosphor's M6800 implementation.

### 1. Right-shift V flag (12 opcodes, ~6,000 failures)

**Affected:** LSR (0x44, 0x54, 0x64, 0x74), ASR (0x47, 0x57, 0x67, 0x77),
ROR (0x46, 0x56, 0x66, 0x76)

**Bug:** `set_flags_shift()` set V = N XOR C for *all* shift/rotate operations.
On the M6800, right shifts (LSR, ASR, ROR) do **not** modify the V flag — it
is left unchanged. Only left shifts (ASL, ROL) set V = N XOR C.

**Evidence:** Mame4all uses `CLR_NZC` (preserves V) for right shifts vs
`CLR_NZVC` (clears all four flags) for left shifts. The M6800 datasheet flag
columns show `-` (unchanged) for V on right-shift instructions.

**Fix:** Split `set_flags_shift` into `set_flags_shift_left` (sets N, Z, C,
V=N^C) and `set_flags_shift_right` (sets N, Z, C only). Updated
`perform_lsr`, `perform_asr`, `perform_ror` to use the right-shift variant.

### 2. TST C flag (4 opcodes, ~2,000 failures)

**Affected:** TSTA (0x4D), TSTB (0x5D), TST indexed (0x6D), TST extended (0x7D)

**Bug:** `perform_tst()` called `set_flags_logical()` which clears V but did
not clear C.

**Evidence:** Mame4all uses `CLR_NZVC; SET_NZ8()` for TST. The M6800 datasheet
shows C = 0 for TST.

**Fix:** Added `self.set_flag(CcFlag::C, false)` after `set_flags_logical(val)`
in `perform_tst()`.

### 3. DAA V flag (1 opcode, 514 failures)

**Affected:** DAA (0x19)

**Bug:** `op_daa()` set N, Z, and C flags but left V unchanged.

**Evidence:** Mame4all uses `CLR_NZV` which clears V before setting N and Z.
The M6800 datasheet lists V as "undefined" for DAA, but clearing it matches
all known reference implementations.

**Fix:** Added `self.set_flag(CcFlag::V, false)` to `op_daa()`.

## Known Remaining Failures

### BRA (0x20) — 4 of 1,000 tests fail

All 4 failures involve branch offset `0xFE` (branch-to-self). Mame4all detects
this pattern and invokes the `EAT_CYCLES` macro, which consumes the entire
remaining cycle budget in a single call. This is a performance optimization for
busy-wait loops — real hardware would execute the branch repeatedly, but
mame4all fast-forwards to save host CPU time.

The optimization causes mame4all to report a different cycle count than the
expected 4 cycles. Phosphor correctly executes the branch as a normal 4-cycle
instruction. These failures are harmless and represent a mame4all optimization,
not a hardware accuracy difference.
