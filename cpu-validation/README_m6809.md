# M6809 Cross-Validation

Validates phosphor-core's M6809 test vectors against an independent reference
6809 core (`cross-validation/mame0148/src/emu/cpu/m6809/m6809.c`).

## Results

266 opcodes validated, 266,000 test vectors (1,000 per opcode):
**264,090/266,000 tests pass (99.3%)**.

All 1,910 mismatches are the same known divergence, described below. Any
*other* failure is a regression.

### Known divergence: `[n]` extended indirect costs 3 cycles too many

53 indexed opcodes report a handful of cycle-count mismatches each, always with
the reference 3 cycles higher than phosphor. Every one of those cases uses
indexed postbyte `0x9F` — `[n]`, extended indirect.

Table 2 (Indexed Addressing Mode) of the MC6809E datasheet lists extended
indirect, postbyte `10011111`, as **+5 cycles / +2 bytes**. Phosphor charges 5;
the reference core charges 8. Per the project rule that the datasheet wins over
the reference on timings, phosphor is correct here and the vectors are not
regenerated to match.

The divergence scales with how often a random postbyte lands on `0x9F`
(~4/1,000 per indexed opcode), so the exact per-opcode counts shift each time
vectors are regenerated.

### Note: TST memory forms are not bus-trace-validated

`TST` direct/indexed/extended (`0x0D`/`0x6D`/`0x7D`) share read-modify-write
timing but perform no write-back: Figure 17 (Cycle-by-Cycle Performance, sheet
5) gives the RMW group `data(EA) / don't-care($FFFF) / write(EA)` and `TST`
`data(EA) / don't-care($FFFF) / don't-care($FFFF)`. Phosphor models that final
don't-care as an internal cycle with no bus access, the same as every other
/VMA cycle in the core, so the vectors record it as `"internal"`.

The reference core instead re-reads the effective address on that cycle. This
does not show up in cross-validation — bus traces are not compared (see below)
and the cycle *count* is identical — but it is why the self-validation vectors
for these three opcodes must be regenerated from phosphor rather than taken
from the reference.

## Prerequisites

- C++17 compiler (clang++ or g++)
- Git submodules initialized

## Setup

```bash
# From the repository root
git submodule update --init

# Build
make -C cross-validation bin/validate_m6809

# Generate test vectors (must run from cpu-validation/ directory)
cd cpu-validation && cargo run --bin gen_m6809_tests --release -- all
```

## Usage

```bash
# Validate a single opcode
./cross-validation/bin/validate_m6809 cpu-validation/test_data/m6809/86.json

# Validate all opcodes
./cross-validation/bin/validate_m6809 cpu-validation/test_data/m6809/*.json
```

## What It Validates

For each test case, the harness:
1. Sets all CPU registers and 64KB memory to the initial state
2. Executes one instruction using elmerucr/MC6809
3. Compares final registers (PC, A, B, DP, X, Y, U, S, CC)
4. Compares final memory at all accessed addresses
5. Compares total cycle count

Bus-level cycle traces (per-cycle address/data/direction) are not validated
since the reference core does not expose per-cycle bus activity. Only
`m6809_single_step_test.rs` checks those.

## Test Vector Format

Each JSON file contains 1,000 test cases for a single opcode:

```json
{
  "name": "86 42",
  "initial": {
    "pc": 4096, "a": 0, "b": 65, "dp": 0,
    "x": 30010, "y": 1024, "u": 512, "s": 42075, "cc": 75,
    "ram": [[4096, 134], [4097, 66]]
  },
  "final": {
    "pc": 4098, "a": 66, "b": 65, "dp": 0,
    "x": 30010, "y": 1024, "u": 512, "s": 42075, "cc": 73,
    "ram": [[4096, 134], [4097, 66]]
  },
  "cycles": [
    [4096, 134, "read"],
    [4097, 66, "read"]
  ]
}
```

Fields:
- **name** — hex bytes of the instruction
- **initial/final** — full CPU state (all 9 registers + accessed RAM)
- **cycles** — per-cycle bus trace: `[address, data, "read"|"write"|"internal"]`

## Test Generation

The test generator (`gen_m6809_tests.rs`) produces 1,000 randomized test
vectors per opcode:

1. Randomize all 64KB of memory and all 9 CPU registers
2. Clamp PC to a valid range (ensures operand bytes fit in address space)
3. Place the opcode (and page prefix if applicable) at PC
4. Execute with `cpu.tick_with_bus()` until the instruction completes (max 200 cycles)
5. Record all bus cycles and snapshot initial/final state
6. For indexed instructions, skip undefined postbytes
7. For EXG/TFR, skip undefined register codes
8. Retry on timeout (max 10x attempts per vector)

```bash
# Generate a single opcode
cd cpu-validation && cargo run --bin gen_m6809_tests -- 0x86

# Generate all opcodes
cd cpu-validation && cargo run --bin gen_m6809_tests -- all
```

Output: `cpu-validation/test_data/m6809/<opcode>.json` (e.g., `86.json`,
`10_8e.json` for page 2, `11_83.json` for page 3).

## Opcode Coverage

266 opcodes across 3 pages:

| Page | Prefix | Count | Examples |
|------|--------|-------|----------|
| Page 1 | (none) | 238 | LDA, ADDA, BRA, JSR, TFR, EXG |
| Page 2 | 0x10 | 19 | CMPD, CMPY, LDY, STY, LDS, STS, long branches, SWI2 |
| Page 3 | 0x11 | 9 | CMPU, CMPS, SWI3 |

## Excluded Opcodes (2)

These opcodes are excluded because they halt the CPU waiting for an interrupt,
which cannot complete in single-step validation with no interrupt sources:

- **SYNC (0x13)** — halts CPU until any interrupt
- **CWAI (0x3C)** — pushes entire state, masks CC, halts until interrupt

## Self-Validation

Phosphor also validates against its own test vectors as a Rust integration test:

```bash
cargo test -p phosphor-cpu-validation
```

This runs `m6809_single_step_test.rs`, which loads every JSON file and replays
each test case against phosphor-core, asserting registers, memory, cycle count,
and per-cycle bus traces. It collects every mismatch and reports a per-opcode
summary, so one bad opcode does not hide the rest.
