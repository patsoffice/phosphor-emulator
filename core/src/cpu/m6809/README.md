# Motorola 6809 CPU

Cycle-accurate emulation of the Motorola 6809E microprocessor, implementing all 285 opcodes across 3 opcode pages. Cross-validated against the reference core in `cross-validation/mame0148/src/emu/cpu/m6809/m6809.c` with 266,000 test vectors.

## Status

| Metric | Value |
|--------|-------|
| Opcodes | 285 (238 page 0 + 38 page 2 + 9 page 3) |
| Unit tests | 459 |
| Cross-validation | 264,132/266,000 (99.3%) |
| Timing | Cycle-accurate |

The 1,868 cross-validation mismatches are all one known divergence in extended-indirect indexed addressing, where the datasheet and the reference core disagree on the cycle count; see `cpu-validation/README_m6809.md`.

## Bus behaviour on don't-care cycles

The 6809E never tri-states its address bus. On a cycle with no memory access to
make it keeps driving an address, and a device that decodes addresses reacts to
it just as it would to a real access — so these cycles are modelled as reads,
not as silence.

Which address depends on where the cycle falls:

| Cycle | Address |
|-------|---------|
| Address computation (direct, extended, most indexed overhead) | `$FFFF` |
| The read-modify-write modify cycle | PC |
| The trailing cycle of a 16-bit ALU op (ADDD/SUBD/CMPx) | PC |
| Inherent register ops (NEGA, INCB, CLRA, shifts, DAA, SEX, NOP) | PC |
| Inherent TST (TSTA/TSTB) and TST's memory forms | `$FFFF` |
| Branch offset settle, TFR/EXG, LEA, vector fetch brackets | `$FFFF` |
| Interrupt/SWI entry, first two cycles | PC |
| Indexed overhead, leading cycles of `,R` / `n8,R` / `D,R` and friends | PC, then `$FFFF` |

Pull sequences (RTS, RTI, PULS/PULU) close on a *real* read at the settled
stack pointer, and push sequences (PSHS/PSHU) open on a real read at the stack
top — neither is a don't-care cycle.

`core/tests/m6809_dont_care_cycle_test.rs` pins these instruction by
instruction. Getting them wrong is not academic: Empire Strikes Back's slapstic
validates its alternate-banking sequence on a `$FFFF` don't-care cycle, and
would not reach bank 2 without one.

## Registers

| Register | Size | Description |
|----------|------|-------------|
| A, B | 8-bit | Accumulators (combine as 16-bit D) |
| X, Y | 16-bit | Index registers |
| U | 16-bit | User stack pointer |
| S | 16-bit | System stack pointer |
| PC | 16-bit | Program counter |
| DP | 8-bit | Direct page register |
| CC | 8-bit | Condition codes (E, F, H, I, N, Z, V, C) |

## Instruction Set

285 opcodes across 3 pages (238 page 0, 38 page 1/0x10, 9 page 2/0x11):

| Category | Count | Details |
|----------|-------|---------|
| ALU (A register) | 9 | ADDA, SUBA, CMPA, SBCA, ADCA, ANDA, BITA, EORA, ORA -- imm/direct/indexed/extended |
| ALU (B register) | 9 | ADDB, SUBB, CMPB, SBCB, ADCB, ANDB, BITB, EORB, ORB -- imm/direct/indexed/extended |
| ALU (16-bit) | 3 | ADDD, SUBD, CMPX -- imm/direct/indexed/extended |
| Unary (inherent) | 13 | NEG, COM, CLR, INC, DEC, TST (A & B), MUL |
| Unary (memory) | 6 | NEG, COM, CLR, INC, DEC, TST -- direct/indexed/extended |
| Shift/Rotate (inherent) | 10 | ASL, ASR, LSR, ROL, ROR (A & B) |
| Shift/Rotate (memory) | 5 | ASL, ASR, LSR, ROL, ROR -- direct/indexed/extended |
| Load/Store | 5 imm + 10 per mode | LDA, LDB, LDD, LDX, LDU, STA, STB, STD, STX, STU |
| LEA | 4 | LEAX, LEAY, LEAS, LEAU |
| Branch | 16 | BRA, BRN, BHI, BLS, BCC, BCS, BNE, BEQ, BVC, BVS, BPL, BMI, BGE, BLT, BGT, BLE |
| Jump/Subroutine | 10 | BSR, LBRA, LBSR, JSR, JMP, RTS |
| Transfer/Stack | 6 | TFR, EXG, PSHS, PULS, PSHU, PULU |
| Interrupt | 4 | SWI, RTI, CWAI, SYNC |
| Misc inherent | 6 | NOP, SEX, ABX, DAA, ORCC, ANDCC |
| Page 2 (0x10) | 38 | Long branches, CMPD, CMPY, LDY, STY, LDS, STS, SWI2 |
| Page 3 (0x11) | 9 | CMPU, CMPS, SWI3 |
| Undocumented | 15 | Aliases for compatibility |

## Addressing Modes

| Mode | Syntax | Description |
|------|--------|-------------|
| Inherent | `INCA` | Register-only, no operand |
| Immediate | `LDA #$42` | Operand follows opcode |
| Direct | `LDA $10` | Zero-page (DP:offset) |
| Extended | `LDA $1234` | Full 16-bit address |
| Indexed | `LDA ,X` | Base register + offset (23 sub-modes) |

The indexed mode supports constant offsets (5/8/16-bit), auto-increment/decrement (1 or 2), accumulator offsets (A/B/D), and PC-relative addressing, each with optional indirection.

## Architecture

### State Machine

The CPU uses a cycle-accurate state machine:

```rust
enum ExecState {
    Fetch,                // Read next opcode
    Execute(u8, u8),      // Page 0 opcode at cycle N
    ExecutePage2(u8, u8), // Page 2 (0x10 prefix)
    ExecutePage3(u8, u8), // Page 3 (0x11 prefix)
    Halted { .. },        // TSC/RDY asserted
    Interrupt(u8),        // Hardware interrupt sequence
    WaitForInterrupt,     // CWAI wait state
    SyncWait,             // SYNC wait state
}
```

### Interrupts

- **NMI** -- Edge-triggered, pushes entire state, vectors through $FFFC
- **FIRQ** -- Level-triggered, masked by F flag, pushes CC+PC only, vectors through $FFF6
- **IRQ** -- Level-triggered, masked by I flag, pushes entire state, vectors through $FFF8
- **SWI/SWI2/SWI3** -- Software interrupts with separate vectors
- **CWAI** -- Pre-pushes state, then waits for interrupt
- **SYNC** -- Waits for any interrupt edge

### Bus Halting

Supports TSC (three-state control) via the `Bus::is_halted_for()` trait method, used for DMA arbitration (e.g., Williams blitter).

## File Structure

```text
core/src/cpu/m6809/
  mod.rs         -- M6809 struct, state machine, dispatch
  alu.rs         -- Flag helpers, addressing mode helpers
  alu/binary.rs  -- ADD, SUB, CMP, SBC, ADC, AND, BIT, EOR, ORA
  alu/shift.rs   -- ASL, ASR, LSR, ROL, ROR
  alu/unary.rs   -- NEG, COM, CLR, INC, DEC, TST, MUL
  alu/word.rs    -- ADDD, SUBD, CMPX, CMPY, CMPD, CMPU, CMPS
  branch.rs      -- Branches, BSR, LBRA, LBSR, JMP, JSR
  load_store.rs  -- LDA/B/D/X/Y/U/S, STA/B/D/X/Y/U/S, LEA
  stack.rs       -- PSHS/U, PULS/U, SWI/2/3, RTI, CWAI, SYNC
  transfer.rs    -- TFR, EXG
  disasm.rs      -- Instruction disassembler
```

## Resources

- [6809 Programmer's Reference](http://www.6809.org.uk/dragon/pdf/6809.pdf) -- Official Motorola datasheet
- [elmerucr/MC6809](https://github.com/elmerucr/MC6809) -- Independent reference emulator (cross-validation)
- [MAME 6809 Core](https://github.com/mamedev/mame/tree/master/src/devices/cpu/m6809) -- Reference implementation
- [Cross-validation details](../../cpu-validation/README_6809.md)
