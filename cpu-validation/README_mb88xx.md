# MB88xx Cross-Validation

Validates phosphor-core's MB88xx test vectors against MAME 0.148's MB88xx
implementation (see [cross-validation/README.md](../../cross-validation/README.md)).

## Results

256 opcodes validated, 256,000 test vectors (1,000 per opcode).

## Prerequisites

- C++17 compiler (clang++ or g++)
- MAME 0.148 shallow clone (see cross-validation/README.md)

## Setup

```bash
# Build
make -C cross-validation bin/validate_mb88xx

# Generate test vectors (from anywhere; the generator resolves its output
# against the crate root, and prints the absolute path it wrote to)
cargo run -p phosphor-cpu-validation --release --bin gen_mb88xx_tests -- all
```

## Usage

```bash
# Validate a single opcode
./cross-validation/bin/validate_mb88xx cpu-validation/test_data/mb88xx/00.json

# Validate all opcodes
./cross-validation/bin/validate_mb88xx cpu-validation/test_data/mb88xx/*.json
```

## Opcode Coverage

256 opcodes across a single opcode page (MB8841 variant: 2048-byte ROM,
128-nibble RAM):

| Group | Opcodes | Cycles |
|-------|---------|--------|
| `0x00-0x3C` | 61 | 1 |
| `0x40-0x5F` | 32 | 1 |
| `0x70-0xFF` | 144 | 1 |
| `0x3D-0x3F` (JPA, EN, DIS) | 3 | 2 |
| `0x60-0x6F` (CALL/JPL) | 16 | 2 |

## Self-Validation

Phosphor also validates against its own test vectors as a Rust integration test,
`tests/mb88xx_single_step_test.rs`:

```bash
cargo test -p phosphor-cpu-validation --test mb88xx_single_step_test
```
