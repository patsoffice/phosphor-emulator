# Design: Checkpoint Rewind

> **Status: proposed.** A bounded ring of `save_state()` checkpoints in the
> frontend, so a player can undo a recent mistake. Granularity is the checkpoint
> interval, **not** one frame.
>
> Split out of an earlier `deterministic-timeline` draft that bundled this with
> an input-movie format. The two share no types, no traits and no crates — this
> is entirely `phosphor-frontend`, that is `phosphor-harness` +
> `phosphor-disasm` — so they are separate epics. See
> [`input-movie.md`](input-movie.md) for the other half and for the one
> interaction between them.

## Context

The only way to revisit a past moment today is `F5` reset plus re-running. At
60 Hz that is tens of seconds to get back through a self-test and attract loop
before play even starts, so a misjudged jump in Marble Madness or a cheap death
in Robotron costs the whole session.

This is a **player affordance**, in the spirit of a fighting game's training
mode. It is deliberately *not* pitched as a debugging tool: the debugger already
has stepping, breakpoints, watchpoints and a 4096-event trace ring, and a
checkpoint ring adds nothing to that story.

The machinery it needs already exists and is already tested.
`machines/tests/save_state_tests.rs` and `harness/tests/save_state_rom_test.rs`
prove that `load_state` followed by N frames reproduces the state and framebuffer
of a machine that reached the same point by running — which is exactly the
property a rewind depends on. No new determinism work is required.

## Goals

1. Let a player step back through recent play and resume from there.
2. Bounded, predictable memory cost across a roster with very different
   save-state sizes.
3. No cost when not rewinding beyond one `save_state()` per checkpoint interval.
4. Change no machine, no board and no core trait.

## Non-goals

* **Frame-accurate seek.** See below.
* Branching or persistent history.
* Debugging use. That is the debugger's job.
* Rewinding across a machine change or a `SAVE_VERSION` change.

## What it is, honestly

A ring of `save_state()` checkpoints. Holding Rewind steps backward through
them; releasing resumes live from the checkpoint reached. Forward history past
that point is discarded, like a tape.

```text
timeline:  ──●────●────●────●────●──▶  live head
                            ↑ hold Rewind walks back, checkpoint by checkpoint
                              release → resume live, forward history dropped
```

**Granularity is the checkpoint interval.** An earlier draft called this
"frame-accurate rewind" and listed frame granularity as a goal while specifying
60-frame checkpoint jumps with no resimulation; those cannot both be true, and
the honest description is the one above.

Frame-accurate seek *is* achievable later — load the nearest checkpoint and
replay an input movie forward to the target frame — but that requires a movie to
always be recording, which it is not during normal play. It is a possible
follow-on once [`input-movie.md`](input-movie.md) exists, and explicitly not
this design.

## Sizing

Budget the ring in **bytes, not entries**.

A Williams save state is about 50 KB — `save_state` writes VideoRam (48 KB),
16 palette bytes, CMOS, sound RAM and optional SRAM, and no ROM
(`machines/src/williams.rs:750-769`). But that is a per-board number, and boards
carrying large persisted pixel buffers will differ substantially. A fixed entry
count would give one machine a generous history and another an unbounded memory
footprint.

```rust
// frontend/src/rewind.rs — SDL-free, unit-tested
const REWIND_BUDGET_BYTES: usize = 64 << 20;
const CHECKPOINT_INTERVAL: u32 = 30;   // ~0.5 s at 60 Hz — see below

struct Checkpoint { frame: u32, bytes: Vec<u8> }

struct RewindRing {
    checkpoints: VecDeque<Checkpoint>,
    budget: usize,
    used: usize,
}
```

A machine with a compact save state gets a long history; a fat one gets a
shorter history rather than blowing the budget. Both tunables are global, not
per machine.

**Measure before fixing the interval.** A registry-driven test reporting
`save_state().len()` for every machine costs almost nothing and turns
`CHECKPOINT_INTERVAL` from a guess into a decision — it sets how many seconds of
history the budget buys on the worst machine, and whether ~0.5 s is the right
trade against `save_state()` call cost. This is Phase 1.

## Frontend integration

Rewind is **frontend-local**. It needs `machine.save_state()`,
`machine.load_state()` and the frontend's own frame counter — nothing else. In
particular it does *not* need `Harness`, which `phosphor-frontend` does not use
at all today (`sak fs grep 'Harness' frontend/src` finds nothing) and which
rewind gives no reason to introduce.

* **Capture** — after each frame, if `frame % CHECKPOINT_INTERVAL == 0`, push a
  checkpoint and evict oldest until within budget. Also push at frame 0 after
  `reset()`, so rewind-to-start always exists.
* **Rewind** — `HostAction::Rewind` as a *hold* binding (default `R` / `Pad
  Back`). While held, step back one checkpoint per repeat interval, `load_state`
  it, set the frame counter, and truncate the ring past it.
* **Resync after load.** `input::resync` must run after every `load_state`. Its
  own doc comment states the requirement: a state load "rewrite[s] the machine's
  port bits with no corresponding key event"
  (`frontend/src/input.rs:700-710`), so without it a direction held across a
  rewind goes dead until the player releases and re-presses it.
* **Overlay** — show the rewound-to frame and the seconds of history held, so
  the granularity is visible rather than surprising.

## What rewind does not restore

Host wall clock and audio transport. The frame pacer resumes from
`Instant::now()`; the SPSC ring keeps draining and will briefly hold or drop
samples across the jump. Neither is in `Saveable` and neither should be — the
model stores *machine* time, not host time.

The ring is **not persisted**. It is volatile session state; persisting it would
bloat `state.toml` with binary blobs valid for exactly one `SAVE_VERSION`.

## Interaction with input movies

[`input-movie.md`](input-movie.md) is a separate epic and neither blocks the
other. The only overlap is that a rewind mid-take would break a movie's "flat
forward trace from reset" invariant.

**v1 rule: rewind is disabled while a recording is armed.** One guard, added by
whichever epic lands second. Truncating an in-progress movie to the checkpoint
frame is the obvious later refinement — and because a rewind lands exactly on a
checkpoint boundary, that truncation is exact when someone wants it.

## Testing

`frontend/src/rewind.rs #[cfg(test)]`, against a fake `SaveState` machine:

* byte-budget eviction — a ring fed oversized states holds fewer entries and
  never exceeds the budget;
* checkpoint selection walks strictly backward and stops at frame 0;
* truncate semantics — resuming discards forward history;
* a checkpoint is always present at frame 0 after reset.

Beyond unit tests, the property rewind relies on is already covered by
`save_state_tests.rs` and `save_state_rom_test.rs`; this epic adds no new
determinism obligations.

Manual acceptance: rewind during play on a Williams machine (large save state, a
CMOS fixture, and two CPUs) and on Tempest (analog control, so the `resync` path
matters), confirming input stays live across the jump.

## Phasing

1. **Save-state size survey** — a registry-driven test reporting
   `save_state().len()` per machine; pick `CHECKPOINT_INTERVAL` from it.
2. **`frontend/src/rewind.rs`** — `RewindRing` plus unit tests, no wiring.
3. **Wiring** — capture hook in the frame loop, `HostAction::Rewind`,
   `resync`-after-load, settings-panel entry, overlay readout.

Phase 1 is worth running on its own even if the rest waits: it is a
five-line test that answers a question nobody has asked about the roster.

## Open questions

* **Checkpoint interval.** Decided by the Phase 1 survey, not up front.
* **Hold vs. toggle.** Hold is the proposal. Toggle is cheaper on a gamepad but
  collides with the existing pause chord; hold first, toggle is one binding
  change if it proves awkward.
* **Audio across a jump.** The ring will hold or drop briefly on `load_state`.
  Whether that wants a short fade — the transport already has fade-in/fade-out
  ramps — is a question for the first person to hear it.

## References

* Property this relies on: `machines/tests/save_state_tests.rs`,
  `harness/tests/save_state_rom_test.rs`.
* Save-state size reference: `machines/src/williams.rs:750-769`.
* Resync-after-state-load requirement: `frontend/src/input.rs:700-710`.
* Companion epic: [`input-movie.md`](input-movie.md).
