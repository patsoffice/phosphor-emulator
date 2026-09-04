# Design: Cycle-Accurate Intel 8088

> **Status: proposed.** Written to answer one question: is converting the I8088
> from instruction-level to per-cycle worth doing, and if so, how. The
> recommendation is **yes, and before the M68000**, for the reason in
> [Sequencing](#sequencing-against-the-m68000).

## Context

Five of the seven CPU cores in this workspace are true per-cycle state machines
where one `tick()` is one bus transaction. The I8088 is not. Its README states
the gap plainly under Status: `Timing: Instruction-level (not cycle-accurate)`.
Design priority #1 for this project is cycle-accurate hardware matching, so this
is a gap rather than a preference.

The blast radius is small. Q\*bert is the only shipped board on the I8088 today,
with Reactor and Mad Planets on the roadmap, all three on Gottlieb System 80.
`gottlieb.rs` already clocks the CPU one `execute_cycle` per CPU cycle out of a
declared clock domain (5 MHz, the 15 MHz crystal over three), so the board side
needs no change: it is already calling us at the right rate. Only the meaning of
a call changes.

The sibling effort for the M68000 is `phosphor-emulator-cycle-accurate-m68000-5y6e`.
Its design doc does not exist yet, which this document takes a position on under
[Sequencing](#sequencing-against-the-m68000). The existing
[`m68000-emulator.md`](m68000-emulator.md) describes that core as built, not as
a per-cycle conversion.

## What the core does today

`core/src/cpu/i8088/mod.rs` is an atomic core wearing a per-cycle interface:

```rust
enum ExecState {
    Fetch,
    /// Executing an instruction: (remaining_cycles).
    /// The instruction has already been decoded and its effect applied on the
    /// first cycle; remaining cycles are bus-idle wait states.
    Execute(u16),
    Halted,
}
```

`execute_cycle` in the `Fetch` state consumes prefixes, fetches the opcode and
runs the whole instruction to completion, then parks in `Execute(n)` and burns
`n` cycles doing nothing. Every bus access an instruction makes therefore happens
on one cycle, in the order the interpreter happens to make them, and the
remaining cycles are silent.

Consequences worth naming, because they are what the conversion buys back:

- **No prefetch queue exists at all.** The 8088's four-byte queue is the main
  source of its real cycle counts, and its absence is why the current core can
  only reproduce documented "best case" timings.
- **Bus ordering within an instruction is an artifact of the interpreter**, not
  of the part. Nothing that watches the bus can be trusted at sub-instruction
  resolution.
- **The cycle *total* is approximately right** and comes from a table, which is
  enough for real-time pacing and for the state-only half of validation.

What is good and should survive: the decode/execute/addressing split
(`decode.rs`, `addressing.rs`, `execute.rs`) is already organized around
operand resolution rather than around one giant match, and `execute.rs` carries
279 opcodes and 325 unit tests. This is a re-timing job, not a rewrite.

## Architecture facts (reference points)

- **Bus cycle**: four T-states, T1 through T4, plus wait states Tw inserted
  between T3 and T4. Idle cycles between bus cycles are Ti.
- **External data bus is 8 bits.** Every 16-bit access is two bus cycles. The
  20-bit address is multiplexed onto the same pins and is valid only while ALE
  is asserted in T1.
- **BIU prefetch queue**: four bytes on the 8088 (six on the 8086). The BIU
  refills it whenever there is room and the bus is free; the EU pulls from it.
  A taken jump flushes it.
- **Queue status lines QS0/QS1** report, one cycle late, whether the EU read a
  First byte, a Subsequent byte, or the queue was Emptied.
- **Bus status lines S0-S2** classify each bus cycle as one of INTA, IOR, IOW,
  MEMR, MEMW, HALT, CODE or PASV.

## The oracle: the vectors already carry a per-cycle bus trace

This is the fact that decides the whole design, and it is the reason to prefer
this core over the M68000.

`cpu-validation/test_data/8088/` is SingleStepTests/8088 v2. We consume it today
for state only: `I8088TestCase` in `cpu-validation/src/lib.rs` deserializes
`name`, `bytes`, `initial` and `final`, with a comment saying `cycles, hash, idx
are present but not used for functional validation`. The suite's own README
documents what we are throwing away. Each entry of `cycles` is an 11-field list,
one per CPU cycle:

| # | Field | What it gives us |
|---|---|---|
| 0 | Pin bitfield | bit 0 = ALE, bit 1 = INTR, bit 2 = NMI |
| 1 | Multiplexed bus | the 20-bit bus, a valid address only when ALE is high |
| 2 | Segment status | S3/S4, which segment computed the address |
| 3 | Memory status | i8288 `RAW`: MRDC, AMWC, MWTC, active low |
| 4 | IO status | i8288 `RAW`: IORC, AIOWC, IOWC, active low |
| 5 | BHE | 8086 compatibility, absent on the 8088 |
| 6 | Data bus | valid on T3 (or the last Tw) |
| 7 | Bus status | INTA / IOR / IOW / MEMR / MEMW / HALT / CODE / PASV |
| 8 | T-state | T1..T4, Tw, Ti |
| 9 | Queue op status | F / S / E / - |
| 10 | Queue byte read | valid when field 9 is not `-` |

The `initial` and `final` states also carry a `queue` array, the literal
contents of the prefetch queue before and after. Our `I8088InitialState` already
deserializes it and the harness already ignores it.

Two further points from the suite README that constrain any implementation:

- **Instruction boundaries are defined by the queue, not by the bus.** A test's
  cycles begin when QS reports the First Byte of an instruction (or of a prefix)
  and end when QS reports the First Byte of the *next* one. "There is no
  indication from the CPU when an instruction ends, only when a new one begins."
- **It takes two cycles to begin a fetch after reading from a full queue**, so a
  test starting with a specified queue state opens with two Ti cycles.

So we have, for 2,577,000 vectors, a cycle-exact recording of a real 8088's bus
and queue behavior, including the prefetch timing. That is a stronger oracle than
anything else in this workspace, and it is already on disk.

## Decision 1: one `tick()` is one T-state

Rejected alternative: one `tick()` = one bus cycle. It is closer to how the
other cores read, but the 8088's bus cycle is not a fixed length once wait
states exist, and the vectors are recorded per T-state. Matching the oracle's
resolution exactly means a replay is a direct comparison rather than an
aggregation, and aggregation is where a per-cycle claim usually goes wrong.

`gottlieb.rs` already calls `execute_cycle` once per 5 MHz CPU cycle, and a
T-state *is* one CPU clock, so this needs no board change and no clock-tree
change. The existing call site keeps its meaning.

`Bus<Address = u32, Data = u8>` stays as it is. An 8-bit external bus means every
transaction is already a byte, so nothing about the trait needs to move. This is
the main structural advantage over the M68000, whose `Data = u16` word bus is
entangled with its own open question about byte strobes
(`phosphor-emulator-contained-fidelity-np9x.1`).

## Decision 2: model the prefetch queue

Model it. Without the queue there is no point doing the conversion at all: the
queue is the difference between documented best-case timings and what the part
does, it is directly observable in the oracle through QS0/QS1 and the `queue`
arrays, and it is the thing the current core is missing rather than merely
approximating.

Shape:

- A four-byte queue with head/tail, plus the two-cycle refill latency the README
  describes.
- The BIU runs as its own small state machine alongside the EU, issuing CODE
  fetches when there is room and the EU is not using the bus.
- A taken jump, a `RET`, an interrupt, or any other control transfer flushes it
  and the flush is reported as `E`.
- The EU pulls opcode, ModR/M, displacement and immediate bytes from the queue
  rather than calling `bus.read` directly, which is the single largest change to
  `decode.rs` and `addressing.rs`.

## Decision 3: validation

**Keep the existing state-only gate exactly as it is**, and add a per-cycle
gate beside it rather than replacing it. The 2,577,000-vector state check is the
regression net that keeps the conversion honest instruction by instruction; a
rewrite that breaks it has broken the CPU regardless of how good its bus trace
looks.

The new gate replays the `cycles` array. Start with a deliberately narrow
comparison and widen it as the implementation earns it:

1. **Cycle count only.** Length of our trace against the vector's. Catches
   gross timing errors immediately and needs no bus modeling to be meaningful.
2. **Bus status and T-state per cycle.** Proves the four-state bus cycle and
   wait-state handling.
3. **Address and data on the cycles where they are valid**, that is, address on
   T1 with ALE, data on T3 or the last Tw.
4. **Queue operation status and queue contents.** The prefetch model proper.

Widening in that order matters: each step is a check that can fail on its own,
and a single all-or-nothing comparison against an 11-field trace would fail for
one reason and be read as failing for another.

The 44 currently skipped opcode files stay skipped, and the skip list needs one
addition worth calling out: `0xE4-0xE7` and `0xEC-0xEF` (IN/OUT) are skipped
today because "test vectors embed I/O data in cycle array, not RAM". Once the
harness reads the cycle array, that reason evaporates and those eight files
should come back in. That is a coverage gain the conversion pays for itself
with, and it should be its own issue rather than a footnote.

## Decision 4: reuse

There is no M68000 per-cycle design to transfer from, because that doc is not
written. Nothing here is blocked on it.

Going the other way, the parts of this work that would generalize are thin and
should not be extracted speculatively:

- A per-cycle replay harness over a bus trace is worth sharing *after* a second
  core needs one, not before. `cpu-validation` already has the file-walking
  half factored (`run_vector_suite`).
- The prefetch queue is 8088-specific in its width, its refill latency and its
  flush conditions. The 68000's prefetch is a two-word pipeline with different
  rules. A shared abstraction over both would be a shape with two users and no
  third, which this repo has been bitten by before.

## Sequencing against the M68000

`phosphor-emulator-cycle-accurate-i8088-nvrh` is currently sequenced *after* the
M68000 "so the harder core sets the pattern", and it flags the counter-argument
itself. This doc comes down on the counter-argument. **Do the I8088 first.**

- The M68000 has no per-cycle oracle. SingleStepTests/680x0 is state-only, so
  converting it means building a bus-trace oracle and a bus-trace implementation
  at the same time, and judging each by the other. That is the shape of mistake
  this repo keeps writing down: a check that cannot fail because its subject and
  its standard came from the same place.
- The I8088 has a real oracle already on disk, recorded from hardware.
- The I8088's bus needs no contract change; the M68000's is tangled with the
  byte-strobe question, which is a separate open issue on the same interface.
- One shipped board against four, so a mistake is cheaper to find and cheaper to
  hold.

"The harder core sets the pattern" is a good instinct when the pattern is the
risk. Here the risk is the oracle, and the easier core is the one that has one.

## Performance

Unknown until measured, and it must be measured, because converting an atomic
core to per-cycle costs throughput by construction.

`phosphor-bench` exists and its baseline is recorded on
`phosphor-emulator-headless-benchmarks-m6u2`. Its default machine list is
pacman, galaga, tempest, marble and joust, and **contains no I8088 board**, so a
Q\*bert baseline has to be taken before any conversion work starts. Take it with
`--warmup` raised past the power-on self-test, since the tool's own help warns
that the default 120-frame warmup measures self-test code rather than gameplay.

The number to beat is not "no regression". A per-cycle 8088 that runs Q\*bert
comfortably above real time is a success even if it is several times slower than
today's core, and saying so up front is what stops the benchmark being used to
argue against a correctness fix after the fact.

## Risks

- **`execute.rs` is 4,785 lines and 279 opcodes.** The conversion touches how
  every one of them reaches the bus. Migration must be incremental with the
  state gate green at every step, not a branch that is broken for weeks.
- **Interrupt timing.** INTA cycles are in the oracle's bus status field, and
  the current core checks interrupts at the top of `Fetch`. Real interrupt
  recognition happens at defined points relative to instruction boundaries and
  the queue.
- **Q\*bert is a video board with a scanline hook.** Changing when the CPU
  touches the bus within an instruction can move the picture. The golden frame
  is the check, and a moved frame needs the usual named mechanism rather than a
  recapture.
- **HLT** is skipped in validation because it blocks forever in the harness.
  Its bus behavior (the HALT status line) is unvalidated and will stay so.

## Milestones

Each is an issue under the epic; each ends with the full state gate green.

- **M1. Per-cycle scaffolding.** T-state bus cycle, `tick()` = one T-state, no
  prefetch queue yet: the EU still drives fetches directly but through a bus
  cycle that takes four T-states. Add the cycle-count-only replay gate. Expect
  many mismatches; the deliverable is the harness plus a number.
- **M2. Prefetch queue.** Four-byte queue, refill latency, flush on control
  transfer. Widen the gate to queue operation status and queue contents.
- **M3. Bus status and addressing.** MEMR/MEMW/CODE classification, address on
  T1, data on T3. Widen the gate to bus status, T-state, address and data.
- **M4. I/O and interrupts.** IOR/IOW and INTA cycles. Re-enable the eight
  IN/OUT opcode files the old harness could not read.
- **M5. Board integration.** Q\*bert golden frame and audio, bench numbers
  against the M1 baseline, README status line updated.

## Acceptance

- `docs/designs/cycle-accurate-i8088.md` answers each question the epic asks:
  the bus cycle model, the prefetch queue, validation, reuse, and migration
  order. This document is that deliverable.
- The follow-on issues below are written from it.

## Verification

- The existing 2,577,000-vector state gate stays green throughout.
- The per-cycle gate reports, per milestone, how many vectors match at the
  current comparison width, so progress is a number rather than an impression.
- Q\*bert's golden frame and audio-sanity entries are unchanged, or changed with
  a named mechanism.
