# M6809 Cross-Validation

Validates phosphor-core's M6809 test vectors against an independent reference
6809 core (`cross-validation/mame0148/src/emu/cpu/m6809/m6809.c`).

## Results

266 opcodes validated, 266,000 test vectors (1,000 per opcode):
**264,723/266,000 tests pass (99.5%)**.

Every one of the 1,277 mismatches belongs to one of the two known divergences
below, and in both of them phosphor is the side that matches the documented
part. Anything outside those two shapes is a regression:

| Divergence | Opcodes | Mismatches | Shape |
|---|---|---|---|
| `[n]` extended indirect costs 3 cycles too many | 48 indexed | 274 | `cycles`, reference exactly 3 higher |
| mixed-size `TFR`/`EXG` | `0x1e`, `0x1f` | 1,003 | one register, reference holds 255 |

Both totals shift each time vectors are regenerated, because both depend on a
random postbyte value — expect the counts, not just the pass rate, to move by a
few dozen. Note that the per-opcode tally keys off the *first* instruction
byte, so page-2 and page-3 opcodes are all lumped under `0x10` and `0x11`.

### Known divergence: `[n]` extended indirect costs 3 cycles too many

The indexed opcodes report a handful of cycle-count mismatches each, always
with the reference 3 cycles higher than phosphor. Every one of those cases uses
indexed postbyte `0x9F` — `[n]`, extended indirect.

Table 2 (Indexed Addressing Mode) of the MC6809E datasheet lists extended
indirect, postbyte `10011111`, as **+5 cycles / +2 bytes**. Phosphor charges 5;
the reference core charges 8. Per the project rule that the datasheet wins over
the reference on timings, phosphor is correct here and the vectors are not
regenerated to match.

The divergence scales with how often a random postbyte lands on `0x9F`
(~4/1,000 per indexed opcode), so the exact per-opcode counts shift each time
vectors are regenerated.

### Known divergence: mixed-size `TFR`/`EXG` fills with the wrong value

`TFR` and `EXG` (`0x1f`, `0x1e`) report ~500 register mismatches each. Every one
of them names one 8-bit and one 16-bit register — about 48% of the postbytes
the generator emits, since it only rejects the codes with no register behind
them. Same-size transfers agree completely.

Motorola documents mismatched sizes as undefined, but the part does transfer.
The registers hang off a 16-bit internal path; a register drives only as many
of those bits as it is wide and the bits above it read back as ones, and a
narrow destination latches only the low bits it can hold. So `TFR A,X` gives
`X = $FF:A` and `TFR X,A` gives `A = X.low`. The reference core instead drives
the whole path with `$FF`, which loses the source value entirely and zeroes the
high byte of a 16-bit destination:

| | phosphor | reference |
|---|---|---|
| `TFR B,D` with `B=$CD` | `D = $FFCD` | `D = $00FF` |
| `TFR X,A` with `X=$1234` | `A = $34` | `A = $FF` |

Phosphor follows the hardware behaviour documented at
<https://www.6809.org.uk/dragon/illegal-opcodes.shtml>, so the vectors are not
regenerated to match. It is pinned by `core/tests/m6809_transfer_test.rs`.

A handful of mixed-size vectors pass anyway — a 16→8 transfer whose low byte
happens to be `$FF` agrees with the reference by coincidence — so the count
runs slightly under the number of mixed-size postbytes in the file.

### Note: don't-care cycles are bus cycles, and are self-validated only

The MC6809E drives its address bus on every cycle, including the ones with no
memory access to make. Phosphor models that: a don't-care cycle either holds
$FFFF (the /VMA cycle proper) or re-drives the program counter, depending on
where in the instruction it falls. `TST` direct/indexed/extended
(`0x0D`/`0x6D`/`0x7D`) are the clearest case — they share read-modify-write
timing but store nothing, and Figure 17 (Cycle-by-Cycle Performance, sheet 5)
gives the RMW group `data(EA) / don't-care($FFFF) / write(EA)` against `TST`'s
`data(EA) / don't-care($FFFF) / don't-care($FFFF)`.

None of this is visible to cross-validation: bus traces are not compared (see
below) and the cycle *counts* are unchanged. It is checked by
`m6809_single_step_test.rs` against vectors generated from phosphor itself, and
pinned instruction by instruction in
`core/tests/m6809_dont_care_cycle_test.rs`.

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
7. For EXG/TFR, skip the register codes with no register behind them (6, 7,
   12-15); mismatched register *sizes* are kept, and are the second known
   divergence above
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
