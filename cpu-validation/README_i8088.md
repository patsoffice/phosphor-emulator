# I8088 Cross-Validation

Validates phosphor-core's I8088 implementation against
[SingleStepTests/8088](https://github.com/SingleStepTests/8088), a
community-standard test suite with about 9,200 randomized test vectors per
opcode file on average, testing all registers, flags, and memory state.

## Results

279 opcode files validated out of 323 total (44 skipped):
**2,577,000/2,577,000 tests pass (100%)**.

## Prerequisites

- Git submodules initialized (`8088` test data)

## Setup

```bash
# From the repository root
git submodule update --init

# The test data is located at:
# cpu-validation/test_data/8088/v2/*.json.gz
```

## Usage

```bash
# Run validation (all 279 opcodes, 2.577M tests)
cargo test -p phosphor-cpu-validation --test i8088_single_step_test -- --nocapture

# Expected output:
#   I8088 SingleStepTests: 2577000 passed, 0 failed across 279 files (44 skipped)
```

## What It Validates

For each of the ~1000 test cases per opcode, the harness:

1. Sets all CPU registers (AX, BX, CX, DX, SP, BP, SI, DI, CS, DS, SS, ES, IP, FLAGS) and memory
2. Executes one instruction using phosphor-core's I8088
3. Compares all final registers and FLAGS (with per-opcode flags mask for undefined bits)
4. Compares final memory at all accessed addresses
5. Handles divide-error edge cases: masks pushed FLAGS bytes on the stack when the 8088's internal division microcode leaves arithmetic flags in an undefined state

## Test Vector Format

Each gzipped JSON file contains ~1000 test cases for a single opcode. File naming
uses uppercase hex (e.g., `A0.json.gz` for MOV AL, [imm16]). Sub-opcodes use
dot notation (e.g., `F6.6.json.gz` for DIV r/m8).

```json
{
  "name": "mov al, [0x1234]",
  "bytes": [160, 52, 18],
  "initial": {
    "regs": {
      "ax": 0, "bx": 0, "cx": 0, "dx": 0,
      "cs": 0, "ss": 12288, "ds": 0, "es": 0,
      "sp": 512, "bp": 0, "si": 0, "di": 0,
      "ip": 256, "flags": 61698
    },
    "ram": [[256, 160], [257, 52], [258, 18], [4660, 66]]
  },
  "final": {
    "regs": { "ax": 66, ... , "ip": 259, "flags": 61698 },
    "ram": [[256, 160], [257, 52], [258, 18], [4660, 66]]
  }
}
```

Fields:

- **name** — human-readable instruction description
- **bytes** — raw instruction bytes
- **initial/final** — full CPU state (registers + memory)
- **regs** — all 14 CPU registers including FLAGS
- **ram** — memory contents as `[physical_address, value]` pairs

### Metadata

Each opcode file includes a `flags_mask` field in its metadata indicating which
FLAGS bits are defined for that instruction. Undefined flag bits are masked during
comparison, matching the Intel datasheet's "undefined" designation.

Examples:
- Most instructions: `0xFFFF` (all flags defined)
- DIV/IDIV: `0xF72A` (all arithmetic flags undefined)
- AAM: `0xF7EE` (CF, AF, OF undefined)

## Skipped Opcodes (44 files)

| Category | Count | Opcodes | Reason |
|----------|-------|---------|--------|
| Prefixes | 8 | 0x26, 0x2E, 0x36, 0x3E, 0xF0-0xF3 | Consumed during decode, no standalone execution |
| IN/OUT | 8 | 0xE4-0xE7, 0xEC-0xEF | I/O data embedded in cycles array, not initial RAM |
| HLT | 1 | 0xF4 | Halts CPU forever (no interrupt source in test harness) |
| FPU ESC | 8 | 0xD8-0xDF | 8087 coprocessor escape codes (no FPU emulated) |
| Undocumented | 19 | 0xD6, 0x0F, 0x60-0x6F, 0xC0, 0xC1, 0xC8, 0xC9, etc. | Hardware aliases, undefined behavior |

## Divide Error Handling

The test harness includes special handling for divide-error cases (INT 0 triggered
by DIV/IDIV overflow or divide-by-zero, and AAM with base=0):

**Pushed FLAGS masking**: When a divide error fires, the 8088 pushes FLAGS to the
stack. However, the internal division microcode modifies arithmetic flags (CF, PF,
AF, ZF, SF, OF) unpredictably before the push. The harness detects divide errors
(SP decreased by 6 + `flags_mask != 0xFFFF`) and masks the pushed FLAGS bytes on
the stack using the same per-opcode `flags_mask`.

**IDIV REP prefix quirk**: The undocumented behavior where REP/REPNE prefix negates
the IDIV quotient is correctly handled and validated.

**IDIV range quirk**: The 8088 treats quotient = -128 (byte) or -32768 (word) as a
divide error, making the valid range -127..=127 and -32767..=32767 respectively.
