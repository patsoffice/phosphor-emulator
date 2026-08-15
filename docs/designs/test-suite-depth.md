# Design: Making the Existing Test Suite Bite

> **Status: implemented.** Tracks beads epic
> `phosphor-emulator-test-suite-depth-nzth` and its three children.

## Context

The workspace has ~3,165 passing tests. Passing is not the same as
*guarding*: several of the highest-value guarantees are either vacuous
(they compare two things that cannot differ) or opt-in (a hand-maintained
list that adding a machine does not extend).

This design fixes the tests that already exist. It deliberately does not
add new test *categories* — golden-frame hashing is a separate epic
(`phosphor-emulator-frame-regression-w1pi`).

The pattern to copy already exists: `machines/tests/input_contract_test.rs`
iterates `registry::all()`, so a newly registered machine is covered the
moment it is registered, and a `the_registry_is_not_empty` guard stops the
whole file passing vacuously if the registry ever comes back empty.

## Problems

### 1. The save-state round trip cannot fail

`machines/tests/save_state_tests.rs` did:

```rust
let sys = create();
let saved = sys.save_state().unwrap();
let mut sys2 = create();
sys2.load_state(&saved).unwrap();
assert_eq!(saved, sys2.save_state().unwrap());
```

Both machines sit at power-on defaults. Every field that only diverges
after execution serializes identically on both sides, so the test passes
whether or not `save_state` actually captured it. A device runtime counter
omitted from a board's `Saveable` impl — the exact bug the suite exists to
catch — passes on all 19 machines.

### 2. Contract coverage is a hand-maintained list

The same file named 19 machines out of ~41 registered. Star Wars, Tempest,
Quantum, Marble Madness, Road Runner, I Robot, Mr. Do, Congo Bongo, Mario
Bros., Frogger, Scramble and the Galaxian family were absent — not by
decision, but because adding a machine does not add a row.

The structural reason is that `MachineEntry::create` needs a `RomSet`,
which CI does not have. `input_contract_test` sidesteps this by testing
only the static control table that `MachineEntry` carries directly; a test
that needs a live machine had no ROM-less way to get one.

### 3. Ten diagnostic programs assert nothing

`machines/examples/*_boot_check.rs` encode real knowledge. `xevious_boot_check`
reports whether the main CPU released the sub and sound CPUs from reset —
which is how you know the 50XX start-up protection handshake succeeded.
`starwars_boot_check` reports whether the AVG display list is non-empty and
frame-to-frame stable — which is how you know the dual-6809 boot completed
and the Matrix Processor is feeding geometry.

They `println!` their verdict, and nothing runs them.

## Design

### A ROM-less constructor on `MachineEntry`

```rust
pub struct MachineEntry {
    pub name: &'static str,
    pub rom_names: &'static [&'static str],
    pub create: fn(&RomSet) -> Result<Box<dyn FrontendMachine>, RomLoadError>,
    pub create_bare: fn() -> Box<dyn FrontendMachine>,   // new
    pub controls: &'static [InputControl],
}
```

`create_bare` is the machine's constructor with the `load_rom_set` step
omitted: real hardware structs, real devices, zero-filled ROM. It cannot
run a game, and it is not meant to — it exists so that registry-driven
tests can reach *behaviour* (rendering, DIP accessors, save state,
`run_frame`) instead of only static metadata.

`register_machine!` emits it for all three of its arms, so the common case
costs nothing. The three hand-written registrations (`quantum`, `starwars`,
`esb`) grow a two-line bare factory each.

Running a bare machine executes whatever a zero-filled ROM decodes to. That
is fine for the purpose: the CPU still runs, the video and audio devices
still tick, and their runtime counters still diverge — which is all a
save-state exerciser needs. Where a test wants a machine that has really
booted, it goes through `create` and is ROM-gated.

*Alternative considered:* give every machine an `#[cfg(test)]`
constructor and hand-list them. Rejected — that is the failure mode this
epic is fixing.

*Alternative considered:* have the test synthesize a plausible `RomSet`.
Rejected — ROM layouts are per-machine, so this is the hand-maintained list
again, wearing a hat.

### Registry-driven contract tests

`machines/tests/machine_contract_test.rs` iterates `registry::all()` and
constructs each machine with `create_bare`. It carries the same
`the_registry_is_not_empty` guard as the input contract file.

What it pins:

- **Identity** — `machine_id()` is non-empty and unique across the
  registry. It is the save-file key, so a duplicate lets one machine load
  another's state.
- **Registration** — `rom_names` is non-empty, and CLI names are unique.
- **Display** — `display_size()` is non-zero and sane; `render_frame`
  writes into exactly `w * h * 3` bytes and no further (a machine that
  overruns its declared size panics here); `display_aspect()`, when
  declared, has non-zero terms; `orientation()` carries no undefined bits.
- **Timing** — `frame_rate_hz()` is finite and in a plausible CRT range.
- **DIPs** — every machine's bank table passes `assert_dip_banks_valid`
  against its own power-on values, and `set_dip_option` masks only its own
  bits. This was previously per-machine via `dip_test_suite!`, so a machine
  that never invoked the macro was unchecked.
- **Execution** — `run_frame` and `reset` do not panic on a bare machine.

`assert_dip_banks_valid` moves from `pub(crate)` to `pub` so an
integration test can call it. It keeps its in-crate callers.

### A save-state round trip that can fail

The replacement protocol, in `machines/tests/save_state_tests.rs`:

```text
a = create();  a.run(WARMUP);
snapshot = a.save_state();
a.run(REPLAY);
fingerprint_a = (a.save_state(), a.framebuffer())

b = create();  b.run(WARMUP + DIVERGE);   // deliberately a different history
b.load_state(snapshot);
b.run(REPLAY);
fingerprint_b = (b.save_state(), b.framebuffer())

assert_eq!(fingerprint_a, fingerprint_b);
```

Two properties make this bite where the old one could not:

1. **`b` has a different history before the load.** If both sides were
   freshly constructed, an unsaved field would hold the same value on both
   and the comparison would be blind to it. Running `b` a different number
   of frames guarantees every unsaved field differs at load time.
2. **The comparison happens after `REPLAY` more frames.** An unsaved field
   that survives the load then feeds the next frames' computation, so the
   divergence propagates into fields that *are* saved, and into the
   rendered frame. Comparing immediately after the load would only re-read
   the same bytes that were just written.

The framebuffer is part of the fingerprint because video state can diverge
without reaching any serialized byte.

This runs registry-driven over every machine via `create_bare`. The
existing negative tests (corrupt machine id, truncated data) become
registry-driven too.

*Acceptance:* deleting a field from a board's `Saveable` impl fails the
test. Verified by hand against Namco Pac's `NamcoWsg` (see Validation).

### Boot checks become ROM-gated tests

`harness/tests/boot_check_test.rs` holds the assertions; the examples stay
as the interactive front end (thumbnails, per-frame traces, tuning the
frame count during bring-up).

It lives in `phosphor-harness` rather than `phosphor-machines` because
harness already depends on machines, already owns the
resolve-ROMs-and-boot sequence, and already exports the `roms_dir()`
convention shared by the disasm and script end-to-end tests. Putting the
tests in `machines/tests/` would need a `machines → harness` dev-dependency
cycle for no gain.

Gating: `roms_dir()` returns `None` → the whole file skips. A machine whose
ROM set is missing from an otherwise-present ROM directory skips
individually, so a partial ROM collection still runs what it can.

Promoted verdicts:

| Machine | Assertion |
|---|---|
| `starwars`, `esb` | AVG display list non-empty on every frame of a tail window; vectors within the coordinate space |
| `xevious` | sub and sound CPUs released from reset (the 50XX handshake), video RAM populated |
| `marble`, `roadrunner` | 68010 left the reset vector, stayed in mapped space, populated video RAM |
| `galaxian`, `mooncrst`, `pisces`, `uniwars` | framebuffer non-blank after the attract intro |

The five capture programs (`asteroid_capture`, `dkong_capture`,
`llander_capture`, `xevious_capture`, `galaxian_capture`) are not promoted.
They dump a WAV or PNG for an external analyzer and have no pass/fail
verdict to move; asserting on their pixel content is the golden-frame
epic's job.

## Validation

Beyond "the tests pass", each fix was checked against the failure it is
supposed to catch, by hand-breaking the code and confirming a red test:

- Removed one field from a device's `Saveable` impl → the save-state
  exerciser fails; the old round trip stays green.
- Made a machine report a `display_size` one row short → the render
  contract test fails.
- Pointed a boot check at a machine held in reset → the boot test fails.

## Non-goals

- Golden-frame hashing (separate epic).
- New coverage for the untested devices (`contained-fidelity` epic).
- Making CI acquire ROMs. Every ROM-dependent test skips cleanly without
  them; the registry-driven tests deliberately need no ROMs at all.
