# Design: Cycle-Accurate Intel 8088

> **Status: implemented, 2026-09-08.** Written to answer one question: is
> converting the I8088 from instruction-level to per-cycle worth doing, and if
> so, how. The recommendation was **yes, and before the M68000**, for the reason
> in [Sequencing](#sequencing-against-the-m68000). It was done, and the answer
> held.
>
> Where it landed: the queue-operation sequence, the bus-cycle sequence and the
> state gate are all exact on 3,007,000 vectors, and the cycle count is
> 3,006,667 of them. Q\*bert runs at 5.87x realtime against the pre-conversion
> core's 5.59x, so the conversion cost no throughput at all: the old core
> retired a whole instruction per `execute_cycle` and so ran roughly an order of
> magnitude more instructions per emulated frame than the hardware does, and
> doing the right amount of work pays for running the bus every T-state.
>
> **What the doc got wrong** is the part worth reading. Four things, recorded in
> the milestone notes below and summarized here:
>
> - "This is a re-timing job, not a rewrite" was optimistic, and qualified once
>   in Decision 5 and then again by events. The bus unit was rewritten twice and
>   the second rewrite regressed both gates by thirty points before the rows
>   caught up.
> - The plan never mentioned the instruction length table, which turned out to
>   be most of M1's work and the piece with the most ways to be quietly wrong.
> - "231 suspension points" was the right count for moving operand accesses and
>   was zero for the fetch front end, which is what M1 actually did.
> - The execution timing was expected to come from a published table. It came
>   from the published *microcode*, transcribed routine by routine; every fitted
>   constant in the timing table was eventually deleted, and each one had been
>   wrong in a way no aggregate could see.

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
runs the whole instruction to completion. Every bus access an instruction makes
therefore happens on one cycle, in the order the interpreter happens to make
them.

> **Correction, 2026-09-04, from reading the code while taking the baseline.**
> The paragraph above originally continued "then parks in `Execute(n)` and burns
> `n` cycles doing nothing", and the third bullet below originally read "the
> cycle total is approximately right and comes from a table". Both are wrong,
> and the truth is worse. `ExecState::Execute(_)` is **never constructed**: the
> only `self.state =` sites in the module are the `Halted` transition in
> `execute.rs`, the two wake-ups and the decrement inside the `Execute` arm
> itself, and `reset`. The state machine is `Fetch` and `Halted`, nothing else,
> which is why the enum carries `#[allow(dead_code)]`. There is no cycle table
> anywhere in `core/src/cpu/i8088/`. **Every instruction retires in exactly one
> `execute_cycle` call**, so the core has no instruction timing at all rather
> than approximate timing. See [Performance](#performance) for what that does to
> the baseline.

Consequences worth naming, because they are what the conversion buys back:

- **No prefetch queue exists at all.** The 8088's four-byte queue is the main
  source of its real cycle counts, and its absence is why the current core can
  only reproduce documented "best case" timings.
- **Bus ordering within an instruction is an artifact of the interpreter**, not
  of the part. Nothing that watches the bus can be trusted at sub-instruction
  resolution.
- **The cycle *total* is not approximately right; it is one, always.** Because
  `gottlieb.rs` calls `execute_cycle` once per 5 MHz CPU cycle, Q\*bert's
  emulated 8088 retires one instruction per 200 ns of board time where the part
  takes anywhere from 2 to over 200 T-states. The CPU is running roughly an
  order of magnitude fast against the video and sound hardware it shares a
  board with. That is a correctness bug in its own right, not just a fidelity
  gap, and fixing it is what will move the golden frame in M5.

What is good and should survive: the decode/execute/addressing split
(`decode.rs`, `addressing.rs`, `execute.rs`) is already organized around
operand resolution rather than around one giant match, and `execute.rs` carries
279 opcodes and 325 unit tests. This is a re-timing job, not a rewrite.

> **Qualified, 2026-09-04, by
> [Decision 5](#decision-5-the-eu-becomes-an-explicit-per-cycle-state-machine).**
> "Not a rewrite" was optimistic. `execute.rs` does get restructured: the 279
> opcodes become pure compute functions and the bus traffic moves out of them
> into a shared operand pipeline. What survives, and it is the part that
> mattered, is the *decomposition*: the opcode semantics, the flag handling and
> the 325 tests all carry over, because none of them is about timing.

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
change. The existing call site keeps its meaning. What has to change to make
that true is inside the CPU, and that is
[Decision 5](#decision-5-the-eu-becomes-an-explicit-per-cycle-state-machine).

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

## Decision 5: the EU becomes an explicit per-cycle state machine

This is the decision the milestones actually fork on, and the first draft of this
document did not ask it: **how does a 4,785-line straight-line interpreter
suspend in the middle of an instruction?**

`execute.rs` reaches the bus from roughly 231 call sites (20 `fetch_byte`, 18
`fetch_word`, 27 `fetch_modrm`, 27 `resolve_modrm`, 12 `read_byte`, 19
`read_word`, 5 `write_byte`, 7 `write_word`, 16 `push16`, 16 `pop16`, and 64
through the four `read_operand`/`write_operand` helpers). Under an outside-in
tick, where the board calls the CPU once per T-state and the CPU returns
afterwards, every one of those is a point the interpreter has to be able to stop
at and resume from. Rust has no stable coroutines, so "stop and resume" means an
explicit state machine.

Three shapes were on the table.

1. **Explicit per-cycle state machines.** Outside-in ticks, with each instruction
   a state machine over a cycle counter. **Chosen.**
2. **Queue only, EU atomic.** Model the BIU and the queue per T-state, leave the
   EU running whole instructions in one go, and charge the datasheet execution
   time afterwards. Cheap, and it buys the prefetch timing that Decision 2 calls
   the main prize. Rejected: every operand read and write would land on a single
   T-state, so steps 3 and 4 of the validation ladder could never pass, and the
   gate would be permanently capped at half its width. A ceiling designed in from
   the start is a check that cannot fail dressed as a milestone.
3. **Inside-out clocking.** Leave the interpreter straight-line and let the bus
   helpers advance time themselves, so that a memory read *is* four T-states and
   running those four ticks the board. This is how the reference cycle-accurate
   8088 emulators are built and it is much less code. Rejected on what it costs
   elsewhere: `Bus` would need a per-T-state hook, `gottlieb.rs` would stop being
   a per-cycle loop and hand the scanline boundary test to the board, the
   debugger's single-step granularity would coarsen from a cycle to an
   instruction, and mid-instruction save states would become impossible because
   the CPU would have no representable state between the start and end of an
   instruction.

Option 1 is the pattern the M6809 and Z80 cores in this workspace already use, so
it is the one a reader of those cores will recognize, and it is the only one of
the three that costs nothing outside `core/src/cpu/i8088/`. Decision 1's
"`gottlieb.rs` needs no board change" and "`Bus` stays as it is" both survive
intact, and so does saving state at an arbitrary cycle.

What it costs is the honest part: `execute.rs` gets restructured, which
[What the core does today](#what-the-core-does-today) optimistically called "a
re-timing job, not a rewrite". The mitigation is that the restructuring is
mostly mechanical rather than per-opcode, because the shape of an 8088
instruction is regular:

```text
fetch opcode  ->  fetch ModR/M  ->  fetch displacement  ->  EA delay
              ->  read operand  ->  compute (pure)  ->  write operand
```

Only the middle step is opcode-specific, and it is pure: it takes resolved
operand values and returns results and flags, touching no bus. So the plan is a
**generic operand pipeline** in `mod.rs` driving the stages above one T-state at
a time, with `execute.rs` reduced to the pure compute step. The 279 opcodes stop
being 279 straight-line routines that each reach the bus and become 279 pure
functions the pipeline calls, which is a far smaller surface than 231 hand-cut
suspension points.

The instructions that do not fit the pipeline get hand-written state machines,
exactly as the M6809's `MUL`, `DAA` and `alu/word.rs` already do here: the string
operations and their `REP` loops, `MUL`/`DIV`/`IMUL`/`IDIV`, `CALL`/`RET`/`RETF`,
`INT`/`IRET`, and `XCHG` with memory. That set is where the risk concentrates and
where the state gate earns its keep.

**Migration order**, since the 2,577,000-vector state gate has to be green at
every commit rather than at the end:

1. Build the pipeline and run *no* opcode through it. The old path stays.
2. Move the fetch and decode front end onto it, so every instruction pays real
   T-states for its opcode, ModR/M and displacement bytes while its execution
   stays atomic. This alone moves every cycle count and is a single, reviewable
   change.
3. Move the ALU and `MOV` families, which are the bulk of the 279 and all fit the
   pipeline unchanged.
4. Hand-convert the awkward set above, one family per commit.

## M1 as built, and what the plan above got wrong

**Landed 2026-09-04.** The loader is a T-state state machine; the executor is
still atomic behind it. Steps 1 and 2 of the migration order were done as one
change, because a pipeline with no opcode running through it compiles to
nothing and cannot be reviewed against anything.

**The numbers.**

| Gate | Before | After |
|---|---|---|
| State, vectors | 2,577,000 across 279 files | **2,757,000 across 297 files**, 0 failed |
| Per-cycle, count only | 0 of 3,007,000 | **576,568 of 3,007,000 (19.17%)** |
| ... from an empty queue | 0 of 1,503,500 | 555,747 (36.96%) |
| ... prefetched | 0 of 1,503,500 | 20,821 (1.38%) |
| Q\*bert throughput | 5.62x realtime | 10.66x realtime |

**The 36.96% against 1.38% split is the argument for M2**, and it is the shape
the design predicted. A prefetched instruction costs the hardware nothing in
bus cycles for its own bytes, while this core charges four T-states for each of
them, so it overcounts badly there: `add dx, sp` takes 8 cycles here against the
hardware's 3. Where memory operands are involved it still undercounts, because
execution is atomic and pays nothing for them: `add byte [ss:bp+di-64h], cl`
takes 12 here against 28. Mean signed error is -20.94 cycles over the 2,430,432
that differ.

**Throughput went up, not down**, which
[Performance](#performance) said to expect and said how to read: the core now
executes roughly a quarter of the instructions per emulated frame that it did,
because each one occupies four T-states per byte instead of one T-state
outright. That is not a win, it is the size of the work the hardware never did.
Q\*bert's golden frame did not move: the main CPU's only interrupt is a VBLANK
NMI asserted on scanlines 240 to 255 (`qbert.rs`, `check_interrupts`), so the
attract sequence advances once per frame regardless of how many instructions
the CPU gets through in between, as long as it keeps up. It still does.

**What the plan got wrong, which is the part worth reading.**

- **"A generic operand pipeline in `mod.rs`" was the wrong first move.** What
  M1 actually needed was a *loader*: a state machine that walks opcode, ModR/M,
  displacement and immediate, and hands the bytes to an unchanged executor.
  That required no change to any of the 279 opcodes, because the executor's
  byte-at-a-time interface (`fetch_byte`, `fetch_modrm`) was already the right
  seam. The pipeline for operand *accesses* is still ahead, but it is a
  separate thing from the fetch front end and the plan ran them together.
- **The plan never mentioned needing a length table, and that is most of the
  work.** A per-cycle core cannot run an instruction to find out how long it
  is: it has to fetch each byte over four T-states first. `format.rs` exists
  because of that, and it is the piece with the most ways to be quietly wrong.
- **"231 suspension points" was the wrong count for this step.** It is the
  right count for moving operand accesses per-cycle, which is still to come.
  For the fetch front end the number of call sites that had to change was zero:
  92 of them lost their `bus` arguments mechanically, and none changed meaning.
- **The state gate was not the tightest check available.** M1 found three
  defects the 2,577,000-vector gate could not see, because it only checks the
  state an instruction leaves behind and all three were about instruction
  *length*: 0x60-0x6F not consuming their `rel8`, 0xF6.1 and 0xF7.1 not
  consuming their immediate, and unimplemented opcodes consuming nothing at
  all. The loader knowing an instruction's length ahead of execution is what
  made the executor's silence measurable. 18 files came off the skip list as a
  result, which is a coverage gain the plan had assigned to M4.

## M2 as built, and what it got wrong

**Landed 2026-09-04.** The BIU and its four-byte queue run alongside the EU.

**The numbers.**

| Gate | After M1 | After M2 |
|---|---|---|
| State, vectors | 2,757,000 across 297 files | **2,797,000 across 301 files**, 0 failed |
| Queue operations, in order | not checked | **3,007,000 of 3,007,000 (100.00%)** |
| Per-cycle, count only | 576,568 (19.17%) | 24,104 (0.80%) |
| Q\*bert throughput | 10.66x realtime | 10.20x realtime |

**The cycle count got much worse, and that is the milestone working.** M1
charged four T-states for every instruction byte, including the bytes of an
instruction that arrived prefetched, which the hardware gets for nothing. That
overcount was partly cancelling the undercount from execution being atomic, and
two errors cancelling is the exact failure mode
[Decision 3](#decision-3-validation) exists to prevent. Removing the wrong cost
exposes the missing one: the mean signed error is now -19.42 cycles, and this
core charges nothing for effective-address calculation or for operand bus
cycles. That is M3.

**The queue-operation check is asserted, not reported.** Equality against the
hardware recording on every vector, no tolerance. It is most of what
[Decision 3](#decision-3-validation) calls step 4 of the ladder; what is missing
is the *position* of each operation in the cycle stream, which cannot mean
anything until the counts are right.

**Throughput cost 4.6%**, 1.489 to 1.558 ms/frame, from running the BIU state
machine on every T-state. Q\*bert's golden frame did not move, for the reason
given under M1.

**What it got wrong.**

- **The two-cycle restart delay was the only constant that needed pointing at,
  and it is in the primary source.** The suite README states it as an
  observable, and the sample trace confirms it. Nothing else about the queue
  needed a number.
- **Four defects, each found by the gate and each a real bug.** In order:
  the queue status for the opcode *behind a prefix* is another `F`, not an `S`,
  which the README says outright and this implementation got wrong first time;
  the recorded trace does **not** carry the next instruction's First Byte, so
  the first comparison helpfully allowed for a trailing event that is not
  there and failed every well-behaved case while printing two identical
  sequences; a taken branch reports its last byte read and *then* the flush, on
  two separate cycles, rather than the flush overwriting the read; and
  0xC0/0xC1/0xC8/0xC9 are RET and RETF and never flushed at all.
- **Detecting a control transfer by comparing addresses does not work**, and
  the vectors are full of the counterexample. A *taken* conditional jump with a
  displacement of zero lands exactly where execution would have gone anyway,
  and the part still flushes. Address equality cannot distinguish a branch not
  taken from a branch taken to the next instruction. Every transfer now says so
  explicitly through `set_ip` and `set_cs`, which is the mechanism rather than a
  proxy for it. This is the clearest instance in the whole conversion of
  "can I point at the part?": the inference was right 98.67% of the time, which
  is exactly the sort of number that gets accepted.
- **The plan said M2 would widen the gate to "queue operation status and queue
  contents".** Contents turned out not to be comparable yet: the suite samples
  `final.queue` *after* the next instruction's first byte has been read, which
  is one queue read past where this replay stops, and reconciling that needs
  the cycle counts M3 brings.

## M3 as built so far

**In progress, 2026-09-04.** The bus is fully modeled; the EU's own clocks are
not.

| Gate | After M2 | Now |
|---|---|---|
| State, vectors | 2,797,000 across 301 files | unchanged, green |
| Queue operations | 3,007,000 (100.00%) | unchanged, still asserted |
| Per-cycle, count only | 24,104 (0.80%) | **580,995 (19.32%)** |
| Q\*bert throughput | 10.20x | 9.45x |

Two errors were found that M2's order-only checks could not see, both about
*position*: every queue refill was taking five T-states rather than four,
because the BIU spent a cycle transitioning out of its restart state before
driving T1; and the gate had been measuring a different span from the recording
since M1, so every cycle-count figure reported for M1 and M2 was against the
wrong span. The suite defines a test as First-Byte to First-Byte, and the
harness was measuring from CPU start to retirement.

Memory operands now reach the bus as MEMR and MEMW cycles through a three-phase
pipeline: read the operand, run the instruction, write the result. `access.rs`
is the table that drives it, and the way it was built is the part worth
copying. It is a second statement of something `execute.rs` already knew, so it
was cross-checked against the executor on all 3,007,000 vectors *before*
anything depended on it, and made load-bearing only once that ran clean. It
found five real disagreements on its first run.

**What is left of M3.** The mean signed error is -20.90 cycles, and none of it
is bus structure any more: it is the cycles the EU spends calculating an
effective address and running its own microcode. Those are datasheet numbers
rather than mechanism, which is why they are a separate step rather than part of
this one, and the positional per-cycle comparison of status, T-state, address
and data cannot mean anything until they land.

## The timing table, and where Table 1-16 stops describing the part

**2026-09-04.** The execution-timing work the M3 comments call for is most of
the way through the instruction set. What is worth writing down is not the
numbers but the two places the method had to change, both found by asking the
recording a question the manual had already answered.

**Table 1-16 is exactly right, and checkable, for most of the set.** Replay only
the cases that begin with a full prefetch queue and carry no prefix, and the
recorded span from an instruction's First Byte to the next one's *is* the
documented clock count: `NOP` 3, `MOV reg, r/m` 2, `PUSH reg` 15, the ALU block
3, the flag instructions 2, thousands of cases each and uniform to the cycle.
That is a much stronger statement than "the numbers look plausible", and it is
what makes the exceptions worth taking seriously.

**The control transfers are the exception.** Measured the same way, they are
three to eight clocks off the published figures, in both directions, and
uniform per row: `JMP short` is documented at 15 and takes 17, `RET` is
documented at 16 plus a transfer and leaves the EU 5 clocks rather than 12,
`INT` is documented at 52 and takes 71. So those rows are measured rather than
transcribed, and what makes that a measurement rather than a fit is that each
number is read off the full-queue half of the suite and then has to predict the
empty-queue half, which reaches the same instruction through a different
sequence of fetches.

**Two mechanism errors turned up on the way, and both were about a single
cycle.** A fetched byte was reaching the EU on T4, where the part delivers it on
the cycle *after* T4; and a segment override was being charged three clocks,
one by the loader and two by the effective-address calculation, where the part
charges two in total. The second is visible only because the recording shows the
same two clocks on register forms, which compute no address at all.

**`LEA` is the instrument that settled the effective-address table.** It is the
only instruction that computes an address and runs no bus cycle, so its span is
its own two clocks plus the EA and nothing else. Every addressing mode lands
exactly on the datasheet's value, asymmetric pairings included. Without that
one instruction the EA table and the operand path could not have been told
apart, and the residual now known to sit in the memory-access path would have
been attributed to the address calculation.

**A per-row residual meter is what made this tractable**, and it is the tool to
reach for next time:
`cpu-validation/tests/i8088_transfer_timing.rs`, `row_residuals`. It replays one
opcode file through the core and prints the histogram of ours-minus-hardware,
so a row is either "+0 on all 5000 cases" or it is not, and a spread is
immediately distinguishable from an offset. Every control transfer now reads +0.

## The residual that was not a timing row

**2026-09-04.** Every memory-operand instruction was a clock or two out, in
both directions depending on the instruction, and it looked like a table
problem. It was not: it was the *shape* of the pipeline around the operand, and
the recording says what the shape is because it carries the cycle each bus cycle
starts on. Asking that question per addressing mode, rather than asking what the
totals came to, is what turned a diffuse residual into two exact statements.

- **The part starts an operand read at the opcode and ModR/M byte plus the
  effective address rounded up to even.** Across both directions of `MOV`, all
  twenty-four memory modes, each uniform within itself.
- **The displacement is fetched during the address calculation.** A `disp8` form
  and a `disp16` form start their operand cycle on the same clock and take the
  same total. So the manual's `base + EA` splits where it looks like it does,
  and this core was subtracting the displacement's fetch from the base as well
  as paying for it in the address phase.
- **The immediate is fetched after the operand access.** `ADD [BX+SI], imm16`
  starts its read on the cycle `MOV reg, [BX+SI]` does. The loader now stops
  before a memory form's immediate and is sent back for it once the operand
  access is done.
- **`LEA` does not round.** It computes the same addresses and lands on the
  datasheet's odd values, so the rounding belongs to the bus request rather
  than to the address. Without that control the correction would have gone into
  the EA table, where it would have been wrong for the one instruction that
  measures the EA directly.

Together those took the cycle count from 48.84% to 56.81% and the bus-cycle
order from 30.63% to 33.44%.

**And one rejected, which is worth as much.** Bus contention, the EU waiting for
a code fetch already in flight before driving T1 of its own cycle, is a real
property of a part with one bus. Modeled here it made the counts *worse*: our
BIU's fetches are not yet scheduled where the part's are, and contention on top
of a mis-scheduled prefetcher turned a clean per-mode residual of 0 or -1 into
noise from -1 to +2. It belongs after the loader reads its bytes when the part
does, not before.

## The signed multiply and divide, and the cost that had to be allowed to go negative

**2026-09-05.** `IMUL` and `IDIV` were the last two families with no execution
time at all, 40,000 vectors at zero, and they had already refuted the obvious
model once. What closed them is one change of assumption, and it is the part
worth keeping.

**The refutation, restated.** Each of `IMUL`'s four sign combinations follows
its own base plus `popcount(|multiplier|)`, and solving the four for
independent per-negation costs gives **minus one clock for negating the
multiplicand**. That was read as proof the model was wrong. It was proof the
model was right and the *constraint* was wrong: these are not costs, they are
the difference a conditional branch makes, and a microcode branch written as
"jump over the negate when the operand is positive" charges the positive case
for a taken short jump and the negative case for the `NEG`. Nothing says the
two come out equal, and nothing says the negate path is the dearer one. Once
the costs are allowed to be negative the four combinations decompose at both
widths and both instructions.

**What was measured.** Recorded spans, register operands, a full queue and no
prefix, grouped by the two signs with the loop's own terms subtracted off.
Every group is a single value.

```text
                        IMUL byte  IMUL word   IDIV byte  IDIV word
  neither negative          79        127         101        165
  left operand negative     90        138         105        169
  right operand negative    93        141         100        164
  both negative             80        128         104        168
```

**The offsets between those rows are identical at both widths**, on both
instructions: the word loop runs twice as long and pays exactly the same sign
correction. That is the check that makes this a measurement rather than a fit.
The byte form has four numbers and four parameters, so it is exactly
determined and predicts nothing; the word form then costs one new constant and
its other three numbers are predictions, and they land.

**Three further cross-checks, each from a population the rule was not read
off.**

- `IMUL` is `MUL` plus exactly ten clocks when nothing needs negating, at both
  widths: 69 to 79 and 117 to 127. Those ten are the two sign tests and the
  check after the loop, all three falling through.
- `MUL` costs one clock more when the product's upper half is zero. `IMUL`
  costs one clock more when the product's upper half is the **sign extension**
  of its lower, which is the same microcode step asking the signed question.
  Substituting the unsigned test leaves the groups split; the signed one closes
  every one of them.
- `IDIV`'s sign correction is `+4` for a negative dividend and `-1` for a
  negative divisor, and those two hold on all three of its populations,
  including the fault path that never reaches the loop.

**`IDIV` has three populations, and the third is a real mechanism rather than
an outlier.** `CORD` checks before it loops and its check is the *unsigned*
one: it leaves for `INT 0` when the quotient would not fit the operand's full
width. A signed quotient has one bit less of room, so a quotient between the
two limits passes the check, runs the whole loop, and only then faults. That
population costs the ordinary divide plus 59 clocks, the same 59 at both widths
and in all four sign combinations. The early fault costs 89 at both widths,
which is `DIV`'s own 79 plus the ten `PREIDIV` costs when neither operand needs
negating.

Two cases in the byte file sat 59 clocks off the rule before this split went
in, and they are the reason it went in: they are quotients of exactly -128,
which the part rejects and a plain range check accepts.

**What this bought.** Cycle count 58.77% to 59.10% of all 3,007,000, with the
modeled population growing by the 40,000 vectors that had no time at all. Mean
signed error -2.50 to +1.05. The residual left on all four opcodes is the same
one `MUL` and `DIV` already carried, and it is not theirs: it is the
single-clock memory-operand tail below.

## The one interrupt window inside an instruction

**2026-09-05.** Interrupts are recognized between instructions, which is the
only point the queue can be redirected without discarding a partial fetch.
There is exactly one exception on this part, and this core did not have it: a
repeated string operation checks between iterations.

The consequence of not having it is not subtle. A `REP MOVSW` with CX at
0xFFFF runs for about a million clocks, and until it finished nothing could
interrupt it. Q\*bert takes a VBLANK NMI every frame.

Two things had to be right, and only the first is obvious.

- **The check goes where the microcode is between iterations**, at the end of a
  string iteration's delay and before the next one begins, so no bus cycle and
  no queue byte is in flight.
- **IP goes back to the start of the instruction**, prefixes included, so the
  handler's `IRET` resumes the repeat with CX part-way down rather than
  returning to whatever follows it. `instr_pos` is the distance back: it
  counted the bytes the microcode consumed, which for a string operation is the
  prefixes and the opcode.

**A deliberate divergence, stated rather than hidden.** The part restores less
than the whole instruction: it remembers one prefix, so a `REP` with a segment
override in front of it comes back without the override and finishes its copy
through the wrong segment. That is a documented defect of the part, and
reproducing it would make a board's interrupt rate decide where its string
moves read from. This core restores every prefix. Nothing in the suite can see
the difference either way, because no trace in the three million vectors
records an interrupt at all, so the check on this is `core/src/cpu/i8088/mod.rs`
and the ROM-gated suites.

## The meter that had never been pointed at most of the instruction set

**2026-09-05.** The per-cycle gate reported 59% on cycle count and had been
creeping up a fraction of a point per session. The per-row residual meter
reported `+0` on row after row, thousands of cases each. Both were true, and
together they hid two wrong constants worth 102,437 vectors.

**The meter took a hand-kept list of 78 opcode files. The gate runs 310.** The
78 were whichever rows somebody had been working on when they added them, so
"most of the rows I looked at read `+0`" licensed nothing at all about the 232
nobody had looked at. The project had been applying its own rule, *never reason
from an aggregate*, to individual rows and not to itself.

`row_residuals` now enumerates the vector directory instead, and ranks every
file by how many cases it gets wrong. The first run of it found:

```text
  40-4F  INC/DEC reg16   +1 on 5000 of 5000, x16 files = 80,000 cases
  06/0E/16/1E PUSH seg   -1 on 5000 of 5000, x4  files = 20,000 cases
  9C     PUSHF           -1 on 2437 of 2437
```

Three constants, 57% of every error in the clean population, none of them
subtle: uniform on every case of every file. `INC`/`DEC` in the single-byte
encoding is a *word* register and costs 2; the arm was charging the 3 that
belongs to the byte-register form in the `0xFE` group, which these opcodes do
not have. And every push form costs the part 11 clocks, where Table 1-16
documents 11 for `PUSH reg` and 10 for the segment pushes and `PUSHF`; the
recording is uniform against all three, so the rows agree with each other and
disagree with the manual.

Clean population, full queue and no prefix: **81.54% to 92.10%**, and 226 of
323 files exact to 247.

**The empty-queue half went down while the prefetched half went up seven
points.** 45.92% to 44.43% against 72.28% to 79.27%. That is two errors
cancelling, caught in the act: those rows were wrong *and* the fetch schedule is
wrong on the same cases, and correcting one exposed the other. It also settles
what the empty-queue rate measures. It is not a measurement of the timing table
at all. It is a measurement of the prefetcher, and it cannot improve until the
loader reads its bytes when the part does.

**Two ways the instrumentation was hiding work, both worth fixing.**

- **`is_modeled` is a hiding place.** An opcode declared unmodeled is excluded
  from the gate's modeled denominator, so its wrong cases never appear as
  failures. The coprocessor escapes and `SALC` are ~17,500 cases sitting behind
  that flag. A row that is hard should be a visible failure, not a smaller
  denominator.
- **The gate has no floor.** It reports a percentage and nothing can fail on
  it, which is why a number that had not moved much in several sessions never
  read as a problem. Once the rows are done it needs a threshold that ratchets.

**What is actually left, ranked, and none of it mysterious.** Every residual
below is uniform or splits into two or three values, which is the signature of a
wrong constant rather than of anything unknowable about the part.

| remaining | cases | shape |
|---|---|---|
| `D8`-`DF` ESC | ~15,000 | `-11`/`-10`, no operand read modeled |
| memory tail, ALU block and unary group | ~25,000 | `-1` read-only, `+1` read-modify-write |
| `FF.2`-`FF.5` indirect transfers | ~8,700 | multi-valued |
| `C6`/`C7` MOV mem, imm | ~3,400 | multi-valued |
| `D6` SALC | 2,524 | `-3`/`-2`, undocumented, no row anywhere |
| `81.7` CMP r/m16, imm16 | 2,496 | `-1` on register forms too, unlike `80.7`/`82.7`/`83.7` |
| `8C` MOV r/m, sreg | 1,609 | `+2` on half |
| `8F` POP r/m16 | 1,441 | spread |
| `D2.x`/`D3.x` shift by CL | ~450 | `-3` on 2% |

The memory tail is one mechanism rather than a row-by-row correction: within the
`0xF6`/`0xF7` group the sign follows write-back, `+1` for `NOT` and `NEG` and
`-1` for the four multiplies and divides, and the `CMP`-immediate rows, which
also do not write back, are `-1` with them.

**The target is exact.** The queue-operation gate is already at 100.00% on all
3,007,000 and the state gate at 2,887,000 of 2,887,000, so the suite is
matchable; there is no evidence of a floor below it, only an unfinished list and
one structural cause behind the empty-queue half.

## The bus-cycle order is two populations, not one number

**2026-09-05.** The bus-cycle comparison has sat around a third since it was
introduced, and the aggregate is the least informative thing about it. Split by
population, the way the cycle count is:

```text
  bus-cycle sequence            34.89%
    empty queue:                 4.36%
    prefetched:                 65.43%
```

A fifteenfold gap between two halves of the same corpus is not a timing
constant, and it is the reason the split is in the gate from the moment the
comparison exists rather than added once the aggregate stopped moving.

**Every empty-queue trace opens mid-fetch**, on the T2 of the fetch already in
flight when the opening First Byte is read:

```text
  0 CODE T2       data=00 q=First:90
  1 PASV T3       data=90
  2 PASV T4
  3 CODE T1 E0442
```

An address is latched on T1 and nowhere else, so that fetch reaches
`recorded_bus_cycles` with no address and is dropped. This replay's window opens
a T-state *earlier* and does record its equivalent, so ours is one entry longer
at the front on **every** empty-queue case, at an address one lower. That is
what nearly every reported difference is.

**The fix is the loader, not the comparison.** It is tempting to decline to
compare a cycle the recording structurally cannot report, and dropping our
leading `Code` entry would lift the empty-queue half a long way while leaving
the prefetched half untouched, which looks like the signature of a correct
allowance. But the two windows differ only because this core takes an
instruction's first byte a T-state after the part does: once
the loader reads on the part's clock, both windows open mid-fetch with no T1 in
either, and any such allowance would be deleting a real code fetch. An
allowance is the right tool for something the recording cannot express and the
wrong tool for something this core does wrongly, and the two are easy to
confuse when only the aggregate is visible.

**What the comparison says beyond that**, and all of it is real:

- **The part fits one more prefetch into its microcode time than this core
  does**, on every memory-operand form: `add byte [ds:di], bl` wants
  `[F F R F F W]` and gets `[F F R F W]`, `neg word [ss:bp+si]` wants
  `[F F R R F W W]` and gets `[F F R R W W]`. This is probably not an
  independent defect: those same rows carry the one-clock memory tail, and a
  four-cycle fetch does not fit in a gap that is a clock too short. Item 5 and
  the bus-cycle order may be one bug.
- **The string operations start their access before the part does**: `LODSB`
  gets `[R F]` where the part runs `[F R]`.
- **`ESC` runs no operand read at all**, and a faulting `IDIV` still reads its
  vector off the bus in no time, both already known.

## The memory tail is two mechanisms, not one

**2026-09-05.** The one-clock memory tail is `-1` on the forms that only read
and `+1` on the read-modify-write forms, and the tidy guess was that the
bus-cycle order's missing prefetch was the same bug: a four-cycle fetch does
not fit in a gap a clock too short. **That guess is wrong**, and the thing that
refuted it was putting the two traces side by side rather than comparing
transaction lists.

`side_by_side` in `i8088_transfer_timing.rs` prints one case cycle by cycle,
ours against the recording, and carries our queue depth. The depth is the point:
a bus-cycle list cannot tell an idle BIU apart from a full queue, and those are
very different bugs.

**`MUL` word with a memory operand, `-1`.** Every cycle matches, T-state for
T-state, for 142 cycles. The part then idles *one more* cycle before its next
First Byte. There is no missing prefetch and no misplaced transaction: the
microcode is one clock short and nothing else is wrong.

**`NEG` word with a memory operand, `+1`.** Identical through cycle 30, and then:

```text
        OURS                        HARDWARE
   28   MemWrite T1 9D3AE           MEMW T1 9D3AE
   29   MemWrite T2                 MEMW T2
   30   MemWrite T3                 PASV T3
   31   MemWrite T4                 <- span already closed
```

The part reads the **next instruction's First Byte on the same cycle as the
final write's T4**. This core finishes the write, then reads on the cycle after.
The queue sits at four bytes throughout, so the BIU is idle for the right
reason and the missing prefetch is not in this case at all.

**The obvious generalization is already refuted, which is why no fix went in
with this.** If the EU could always start the next instruction on the last
write's T4, `MOV [mem], reg` would be a clock shorter too, and `88` and `89`
read `+0` on 88% of their cases. So the overlap is conditional on something not
yet identified, and applying it universally would trade one wrong row for
several. Two errors cancelling is exactly the shape this epic keeps finding, and
a rule fitted to the two rows in front of it is how the next one gets written.

What is established: the tail is **two** mechanisms with two different causes,
and neither is the missing prefetch.

## Three rows the by-mode grouping closed, and one it refused to

**2026-09-05.** Grouping each row's residual by addressing mode, rather than
reading its aggregate, split the memory tail into rows that are simply wrong
and rows that are something else. Uniform across all 24 memory modes and all 8
register ones is the standard; anything less is not a constant.

```text
  NOT and NEG in memory      F6/2 F6/3 F7/2 F7/3   +1 on all 24, +0 on all 8
  MUL IMUL DIV IDIV memory   eight files            -1 on all 24, +0 on all 8
  CMP with an immediate      80/7 81/7 82/7 83/7    -1 on all 24
  MOV [mem], reg             88 and 89              +1 on rm 0 and 3, mod 0 and 1
```

The first three are wrong constants and are now the measured values: `NOT` and
`NEG` in memory cost 7 rather than the table's `16 - 8`; the multiplies and
divides cost one clock beyond their register form, its bus cycles and its
effective address, where Table 1-16's memory rows imply two at both widths;
`CMP` with an immediate in memory costs 7 rather than `10 - 4`.

**`MOV [mem], reg` is not one of them, and that is why the grouping mattered.**
Its `+1` lands only on `BX+SI` and `BP+DI`, and only without a `disp16`. A row
constant cannot express that, and the aggregate would have invited one.

Clean population 92.10% to **95.14%**, files exact on every case 247 to **261
of 323**. Cycle count 61.85% to 63.56%, its prefetched half 79.27% to 82.55%.

**And one change was reverted after it was measured.** The `0x81` register form
reads -1 on all eight register modes for both `ADD` and `CMP`, where `0x80`,
`0x82` and `0x83` read +0; `0x81` is the only one of the four carrying a 16-bit
immediate, so a clock for the extra byte is the obvious reading. Putting 5 in
the row changed nothing at all: `81.0` came back byte-identical, 649 of 2463
wrong either way, and `81.7` kept its `-1` on exactly its register modes. The
unit test confirms the row returns 5, so **the pipeline is not spending this
row for that form** and the missing clock is somewhere between the table and
the span.

A constant that changes no output is not a measurement. It came back out, and
what is left in its place is a comment saying so and a finding on the issue,
which is worth more than a number that looks like progress and is not.

## The escapes, and what a skip list costs

**2026-09-05.** `ESC`, the eight coprocessor escapes `D8`-`DF`, were invisible
to **both** gates at once. The state gate skipped them; the per-cycle gate had
them behind `is_modeled == false`, which takes an opcode out of the denominator
rather than reporting it as wrong. Eighty thousand vectors, unchecked by
anything, and neither number moved when they were wrong.

**The part performs the memory read its ModR/M byte describes**, so a
coprocessor sitting on the bus can see the operand. It reads a *word*: two MEMR
cycles at consecutive addresses on every memory form of every one of the eight
files. The CPU itself does nothing with the value. That is the entire
instruction on a machine with no 8087, and this core was not doing the read.

**The residual said exactly what was missing, once grouped by addressing
mode.** It is two-valued, -10 and -11, and the split is not by opcode or by
operand: it is by the *parity of the effective address*. Every even EA reads
-10, every odd one -11. That is the even-clock bus rounding the pipeline
already applies to every memory operand, so:

```text
  missing = 8 (the word read) + 2 (microcode) + 1 if the EA is odd
```

which is Table 1-16's `8+EA` with one transfer, read for the 8088 as 12 and two
bus cycles. For once the manual's row is right and the recording only confirms
how it splits.

| | before | after |
|---|---|---|
| State gate | 2,887,000 across 310 files, 13 skipped | **2,967,000 across 318, 5 skipped** |
| Cycle count | 63.56% | 64.73% |
| Bus-cycle order | 57.60% | 59.02% |
| Clean population | 95.14% | 96.68% |

**The eighty thousand went from unchecked to asserted**, which is worth more
than the percentage points: a skip list and an `is_modeled` flag are both
places work hides, and this epic has now found the same instruction hiding in
two of them simultaneously. What is left on the state gate's list is five
files, and `D6` is the only one of them that is an instruction rather than a
prefix.

**And the operand table's cross-check paid for itself again.** Declaring the
read in `access.rs` made the table say `ESC` reads memory while the executor's
catch-all arm still only consumed its bytes, and the `debug_assert` that
compares the two fired on every memory form of all eight opcodes:

```text
  opcode DE modrm 2E: the operand table says reads=true writes=false,
                      the executor did 0 reads and 0 writes
```

The release runs used to measure the improvement compile that assert out, so
the numbers were real and the tree was inconsistent underneath them. `ESC` has
a real executor arm now, which reads the operand and discards it, because that
is what the part does. **The ROM-gated `save_state_rom_test` caught the same
defect independently**, by running a game that executes an escape, which is the
standing lesson of this epic: the vector harnesses all start from a clean CPU
and the boards do not.

## The empty-queue deficit has a mechanism, and it is the BIU yielding

**2026-09-05.** The empty-queue half of the cycle count sits at 44.92% against
the prefetched half's 84.55%, about 828,000 vectors, and it had never been
looked at cycle by cycle. The one attempt compared our span index against the
recording's raw trace index, which are different origins for a case that starts
with an empty queue, and produced a confident and meaningless answer.

`side_by_side` does not have that problem: both columns begin on the cycle the
opening First Byte is read, which is where the recorded trace starts and where
this replay starts measuring. Pointed at `MOV AX, [BP+DI+4]` from an empty
queue, ours 30 cycles against the part's 27:

```text
          OURS                          HARDWARE
    11    Code T4              len=1     CODE T1 EE051
    12    Code T1 EE051        len=1     CODE T2
    15    Code T4              len=2     PASV Ti      <- the BIU stops
    16    Code T1 EE052        len=2     PASV Ti      <- and stays stopped
    17    Code T2              len=2     MEMR T1 E863F
    20    MemRead T1 E863F     len=3     PASV T4
```

**The part runs four code fetches before the operand read and this core runs
five.** The part is not idle for lack of work: it has one byte in its queue and
room for three. It declines to start a fetch because the EU's operand access is
about to need the bus. This core starts one, and that pushes the operand read
back a full bus cycle. Three cycles of delay on a three-cycle discrepancy: this
one case is explained exactly.

**This is not the bus contention that was tried and rejected**, and the
distinction is the whole point. That was *the EU waiting for an in-flight code
fetch*, and it made the counts worse because the prefetcher's fetches were not
where the part's are. This is *the BIU declining to begin a fetch while the EU
has a request pending*, which is the opposite direction, and it is testable now
because the fetch schedule is independently confirmed: on a full queue the first
code fetch lands on the same cycle and the same address as the part's, on every
case of every file tried.

**And the population count half-refuted it**, which is why it came before the
implementation. Over all 323 files, empty queue and no prefix:

```text
  touches no memory:  385,487 of 517,697 exact (74.46%)
  touches memory:      81,447 of 453,736 exact (17.95%)

  code fetches before the first data cycle, ours minus the part's:
    -2: 1.7%   -1: 4.6%   +0: 42.5%   +1: 32.2%   +2: 19.0%
```

**The over-fetching is real and it is not one rule.** Only 51% run extra
fetches; 42.5% already agree with the part exactly; and 6.3% run *fewer*. A
blanket "the BIU does not begin a fetch while the EU has a request pending"
would make that 6.3% worse and put the 42.5% at risk, for a partial gain that
would then hide whatever is really going on. That is the shape of the previous
contention attempt's failure, arrived at from the other direction.

**And there is definitely a second cause.** A quarter of the empty-queue cases
that touch no memory at all are wrong, 88,000 of them at exactly `+2`. Nothing
about EU and BIU arbitration over an operand access can reach an instruction
that makes no operand access.

**The single biggest concentration of error in the corpus** is now located:
memory-touching empty-queue cases at 17.95%, about 372,000 vectors.

**And the gap is deterministic.** Grouped by addressing mode over four opcodes,
all 96 groups are uniform, with no spread anywhere:

```text
  8B (reads)    mod=0: +1, except the EA=5 modes which are +0
                mod=1: +1 on all
                mod=2: +2 on all
  89 (writes)   mod=0: +0 on all
                mod=1: +1 on all
                mod=2: +1 at EA=9, +2 at EA=11 and 12
  01, 81.0      the same shape as 8B
```

Three things that rule out the easy answers:

- **It is not the effective address alone.** `mod=1` and `mod=2` carry the same
  EA costs, 9, 11 and 12, and differ by one everywhere.
- **It is not the displacement length alone.** Within `mod=0` a read splits at
  EA 5 against EA 6 and above; within `mod=2` a write splits at EA 9 against
  EA 11 and 12.
- **The operand's direction matters.** `89`, which writes, sits exactly one
  step behind `8B`, which reads, in every mode.

So the excess is a function of how long the address phase runs *and* which way
the operand goes: this core prefetches through the whole phase where the part
stops at some point inside it, and the longer the phase the more extra fetches
this core fits.

## The rule, and the version of it that was wrong

**A bus cycle is four T-states and the BIU cannot abandon one partway.** So the
part will not *begin* a fetch it would still be holding when the EU comes for
the bus: beginning one there delays the operand access by up to a whole bus
cycle. A fetch already underway is not abandoned, which is why this gates the
transition that drives T1 rather than the whole unit.

**The first version suppressed prefetching for the entire address phase, and
the gate rejected it.** It is the reading the single trace invites, and it is
wrong:

```text
                          before   whole phase   fetch would not finish
  bus-cycle order         59.02%     55.47%           67.66%
    empty queue           53.62%     59.63%           60.10%
    prefetched            64.42%     51.31%           75.22%
  cycle count             64.73%     64.05%           64.61%
```

Suppressing the whole phase moved the empty-queue half up six points and the
prefetched half down thirteen, because **the part does slip prefetches into an
address phase that has room for them**: that is the same behavior the bus-cycle
comparison had already reported from the other end, as the part fitting one more
prefetch into its microcode time than this core does. Both numbers had to move
the right way before the rule was the right rule.

Narrowed to "only when the fetch would not finish in time", `remaining < 4`, the
bus-cycle order goes from 59.02% to **67.66%**, its prefetched half from 64.42%
to **75.22%**. The 4 is not fitted: it is the length of a bus cycle, and
`remaining >= 4` is exactly the condition that the fetch completes before the
address phase ends.

**Cycle count did not follow, and that is worth saying plainly.** It moved
64.73% to 64.61%, a tenth of a point down. Putting the fetches in the right
places changes when the queue refills and therefore when later instructions
stall, so counts that were accidentally right become wrong and others become
right. An eight-point gain on the stricter, positional gate against a tenth of a
point on the looser one is a clear net, but it also says the remaining
count-level cause is not this one.

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

Converting an atomic core to per-cycle costs throughput by construction, so the
baseline was taken before any conversion work started. `phosphor-bench` gained
`qbert` in its default machine list, which previously held no I8088 board.

**Baseline, 2026-09-04**, release build, 600 measured frames, 5 reps, fastest
rep reported:

| warmup | emul ms/f | render | audio | total ms/f | fps | realtime | spread |
|---|---|---|---|---|---|---|---|
| 1800 (attract mode) | 2.858 | 0.035 | 0.003 | 2.896 | 345.3 | **5.62x** | 0.5% |
| 120 (self-test) | 2.868 | 0.035 | 0.010 | 2.913 | 343.3 | 5.59x | 1.1% |

After M1, same command and warmup: 1.489 emul, 1.527 total, 655.0 fps,
**10.66x** realtime, 1.2% spread. See
[M1 as built](#m1-as-built-and-what-the-plan-above-got-wrong) for why that is
not a win.

```text
cargo run --release -p phosphor-bench -- --machine qbert --frames 600 --warmup 1800 --reps 5
```

The two warmups agree to within 1%, which says Q\*bert's per-frame cost does not
depend much on whether the board is running self-test or attract code. The
1800-frame figure is the one to quote: it is the same point the golden frame is
pinned at, so the two measurements describe the same machine state.

The rest of the default list on the same host and run, for context: pacman
26.37x, galaga 19.83x, joust 9.10x, marblemadness 6.44x, tempest 4.79x. Q\*bert sits
second-slowest, and unlike tempest its cost is emulation rather than render.

**Read this number with the correction in [What the core does
today](#what-the-core-does-today).** It is not a like-for-like "before". Today's
core retires one instruction per `execute_cycle` call and the board makes five
million of those calls per emulated second, so it is executing roughly an order
of magnitude *more* instructions per emulated frame than the part does. A
per-cycle core executes the right number, spread over more, cheaper ticks. Those
two effects push in opposite directions and there is no way to predict the net
from here. If throughput improves, that is not a free lunch: it is the measure
of how much work the current core was doing that the hardware never did.

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
