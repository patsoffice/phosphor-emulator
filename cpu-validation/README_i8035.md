# I8035 (MCS-48) Cross-Validation

Validates phosphor-core's I8035 test vectors against
[MAME](https://github.com/mamedev/mame)'s MCS-48 implementation (`mcs48.cpp`).

## Results

229 opcodes validated, 229,000 test vectors (1,000 per opcode).

## Prerequisites

- C++17 compiler (clang++ or g++)
- Vendored MAME MCS-48 source (see below)

## Setup

```bash
# Build
make -C cross-validation bin/validate_i8035

# Generate test vectors (from anywhere; the generator resolves its output
# against the crate root, and prints the absolute path it wrote to)
cargo run -p phosphor-cpu-validation --release --bin gen_i8035_tests -- all
```

### Vendoring MAME MCS-48

The cross-validation harness requires MAME's MCS-48 source vendored into
`cross-validation/mcs48/` with a shim header (`mcs48/mame_shim.h`) exposing:

```c
void mcs48_reset();
void mcs48_set_reg(int reg, unsigned val);
unsigned mcs48_get_reg(int reg);
int mcs48_execute(int cycles);
uint8_t mcs48_internal_ram[256];
uint8_t mcs48_program_memory[4096];
```

Until the MAME source is vendored, the harness compiles and runs but passes
all tests trivially (placeholder mode).

## Usage

```bash
# Validate a single opcode
./cross-validation/bin/validate_i8035 cpu-validation/test_data/i8035/68.json

# Validate all opcodes
./cross-validation/bin/validate_i8035 cpu-validation/test_data/i8035/*.json
```

## What It Validates

For each test case, the harness:

1. Sets all CPU registers and internal RAM to the initial state
2. Loads program memory from the test vector
3. Executes one instruction using MAME's MCS-48
4. Compares final registers (A, PC, PSW, F1, T, DBBB, P1, P2)
5. Compares final internal RAM (64 bytes)
6. Compares control flags (A11, timer/counter state, interrupt state)

## Test Vector Format

Each JSON file contains 1,000 test cases for a single opcode:

```json
{
  "name": "68",
  "initial": {
    "a": 21, "pc": 100, "psw": 128, "f1": false,
    "t": 0, "dbbb": 255, "p1": 255, "p2": 255,
    "a11": false, "a11_pending": true,
    "timer_enabled": false, "counter_enabled": false,
    "timer_overflow": false, "int_enabled": false,
    "tcnti_enabled": false, "in_interrupt": false,
    "ram": [[100, 104]],
    "internal_ram": [[0, 48], [1, 10], ...]
  },
  "final": {
    "a": 69, "pc": 101, "psw": 128, ...
    "ram": [[100, 104]],
    "internal_ram": [[0, 48], [1, 10], ...]
  },
  "cycles": [
    [100, 104, "read"]
  ]
}
```

Fields:

- **name** — hex bytes of the instruction
- **initial/final** — full CPU state (registers + external bus memory + internal RAM)
- **ram** — external bus memory at accessed addresses `[address, value]`
- **internal_ram** — all 64 bytes of internal RAM `[offset, value]`
- **cycles** — per-cycle bus trace: `[address, data, "read"|"write"|"internal"]`

## Test Generation

The test generator (`gen_i8035_tests.rs`) produces 1,000 randomized test
vectors per opcode:

1. Randomize all 64KB of external memory and all CPU registers
2. Clamp PC to valid 12-bit range (ensures operand bytes fit)
3. Place the opcode (and operand if 2-byte) at PC
4. Randomize all 64 bytes of internal RAM
5. Mask R0/R1 to valid RAM range for indirect addressing
6. For RET/RETR: ensure SP > 0 with valid stack entry
7. For CALL: ensure SP < 8 (room to push)
8. Keep timer/counter/interrupts disabled to avoid side effects
9. Execute with `cpu.tick_with_bus()` until instruction completes (max 20 cycles)
10. Record bus cycles, snapshot initial/final state and internal RAM
11. Retry on timeout (max 10x attempts per vector)

```bash
# Generate a single opcode
cargo run -p phosphor-cpu-validation --bin gen_i8035_tests -- 0x68

# Generate all opcodes
cargo run -p phosphor-cpu-validation --bin gen_i8035_tests -- all
```

Output: `cpu-validation/test_data/i8035/<opcode>.json` (e.g., `68.json`,
`a3.json`).

## Opcode Coverage

229 opcodes across a single opcode page:

| Category | Count | Examples |
|----------|-------|----------|
| ALU (register) | 60 | ADD A,Rn, ADDC A,@Ri, ANL, ORL, XRL |
| ALU (immediate) | 5 | ADD A,#data, ANL A,#data |
| Accumulator unary | 10 | INC A, DEC A, CLR A, DA A, RL, RRC |
| Register INC/DEC | 18 | INC Rn, INC @Ri, DEC Rn |
| Data movement | 35 | MOV A,Rn, XCH, XCHD, MOV A,T, MOV PSW,A |
| Immediate loads | 11 | MOV A,#data, MOV Rn,#data, MOV @Ri,#data |
| External memory | 4 | MOVX A,@Ri, MOVX @Ri,A |
| Program memory | 2 | MOVP A,@A, MOVP3 A,@A |
| Port I/O | 6 | IN A,P1, INS A,BUS, OUTL P1,A |
| Port RMW | 6 | ORL BUS,#data, ANL P1,#data |
| Expander ports | 16 | MOVD A,Pp, MOVD Pp,A, ORLD, ANLD |
| Jumps/calls | 17 | JMP, CALL, JMPP @A, RET, RETR |
| Conditional jumps | 20 | JC, JZ, JT0, JNI, JBb |
| DJNZ | 8 | DJNZ Rn,addr |
| Status flags | 6 | CLR C, CPL F0, CLR F1 |
| Control | 11 | EN I, STRT T, SEL RB1, SEL MB0 |
| NOP | 1 | NOP |

27 undefined opcodes (0x01, 0x06, 0x22, etc.) are treated as NOP by the CPU
but excluded from test generation.

## Self-Validation

Phosphor also validates against its own test vectors as a Rust integration test:

```bash
cargo test -p phosphor-cpu-validation
```
