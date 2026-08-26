# Design: Chunked & TLV Save-State Format

> **Status: rev 3. Stages A and D are implemented; B and C are not.** Rev 1
> proposed replacing the positional binary `Saveable` format wholesale with
> tag-length-value framing. Rev 2 kept that as the end state but split it into
> stages, because the repo's own history shows most of the pain is fixed by the
> *first* stage: per-component chunking. Rev 3 records what Stage A actually
> turned out to be, which differs from rev 2 in one structural way (see
> [What Stage A shipped](#what-stage-a-shipped)), and corrects rev 2's stale
> anchors. `SAVE_VERSION` was 12 by the time Stage A was written, not 5: seven
> further global invalidations had landed since rev 2, every one of them a
> single subsystem changing.

## The problem, stated from the git log

Save states back quick save/load (`F6`/`F7`), the frontend's `SaveState`
persistence, headless trace fixtures, and `phosphor-script` inspection. The
format is positional:

```text
header: PHOS | u32 SAVE_VERSION=5 | u32 id_len | machine_id bytes
body:   Saveable::save_state() concatenation in field declaration order
```

`read_header` (`save_state.rs:254`) checks `version != SAVE_VERSION` with exact
equality, so **any** format change invalidates every save on disk. That has
happened four times, and the reason each time was an unrelated refactor:

| Bump | Commit | What changed | Blast radius |
|---|---|---|---|
| 1→2 | `627c460` | type-safe `MemoryMap` regions | Broad — 27 files |
| 2→3 | `554dbb5` | convert hand-rolled impls to `derive` | Broad — 27 files |
| 3→4 | `cd0c917` | AVG runs a PROM state sequencer | **`core/src/device/avg.rs` only** |
| 4→5 | `e476018` | Galaga's three Z80s dispatch to a concrete bus | **Namco Galaga family only** |
| 5→6 | | Williams blitter gains the slow-blit stall flag | **Williams family only** |
| 6→7 | | Mr. Do! gains an output coupling capacitor | **One board** |
| 7→8 | | Gottlieb's `ClockDivider`s become a `ClockTree` | **One board** |
| 8→9 | | Atari System 1 and Star Wars clock domains | **Two boards** |
| 9→10 | | Congo Bongo's sound accumulator | **One board** |
| 10→11 | | Scramble's sound accumulator | **One board** |
| 11→12 | | nine boards' `ClockDivider`s become domains | **Nine boards** |

The pattern is the point. Rewriting the AVG device destroyed every Joust,
Pac-Man and Q\*Bert save on every user's disk — machines that contain no AVG.
A Galaga bus-dispatch refactor did the same, and so did every bump from 6 on.
**Two of the first four global invalidations, and all seven after them, were
single-subsystem changes**, and nothing about the format let the damage be
contained.

The second symptom is conditional fields. `WilliamsBoard::save_state`
(`machines/src/williams.rs:747-777`) is hand-written and emits fields
conditionally, with the workaround documented in its own comments:

```rust
// Variant state appended last (guarded by construction-time config) so
// standard gen-1 saves stay byte-identical to the pre-variant layout.
if self.config.extra_sram_dxxx { w.write_bytes(...) }
if let Some(cvsd) = &self.cvsd { cvsd.save_state(w); }
// Blitter window-enable ... persist it here, guarded.
if self.config.blitter_window_clip.is_some() { w.write_u8(...) }
```

Append-at-the-end-and-hope. `starwars.rs:1530` (optional Slapstic) and
`atari_system1_sound.rs:536` (optional speech board) do the same. A reader has
no way to know whether trailing bytes are a CVSD chip or the next component.

### What this does *not* fix

Rev 1 implied broader stakes than exist. Stated plainly so the cost/benefit is
honest:

* **There are no long-lived fixtures.** No `.state` or `.nvram` file is
  committed anywhere in the repo. The round-trip tests
  (`machines/tests/save_state_tests.rs`, `harness/tests/save_state_rom_test.rs`)
  save and load in-process and never read an old file.
* **NVRAM is unaffected.** `Nvram::save_nvram() -> Option<&[u8]>`
  (`core/src/core/machine.rs:665`) is a raw byte slice written straight to
  `<machine>.nvram` with `std::fs::write` (`frontend/src/main.rs:277`). No
  header, no version, no `Saveable`. The one genuinely long-lived user artifact
  never touches this format.
* **Per-component semantic churn is low.** 28 of 34 `#[save_version(N)]` sites
  are still at 1; only `m6809`, `tms5220`, `votrax_sc01`, `tempest` and
  `gottlieb` have ever bumped.

So the benefit is **quicksave durability across refactors** — a developer
ergonomics and user-annoyance win, not a data-integrity one. That is worth
Stage A. Whether it is worth Stage C is a real question, answered below.

## Architecture

Anchors are named, not line-numbered: rev 2's line numbers were all stale within
a few months.

* `core/src/core/save_state.rs`: `SaveError`, `SAVE_MAGIC`, `SAVE_VERSION`,
  `MIN_SUPPORTED_SAVE_VERSION`, `crc32`, `Saveable`, `StateWriter` (with
  `begin_chunk`/`end_chunk`/`write_tlv`/`write_component`/
  `write_optional_component`), `StateReader` (with
  `sub`/`skip`/`peek_tag`/`read_tag_len`/`read_component`/`read_optional`/
  `read_optional_component`), `ChunkTrace`, `write_header`/`read_header`,
  `save_machine`/`load_machine`/`load_machine_traced`.
* `macros/src/lib.rs`: `derive_saveable`, `#[save_version(N)]`,
  `#[save_skip]`/`(default)`/`(default=expr)`, `#[save_elements]`;
  `gen_field_io`, `gen_array_io`, `gen_array_element_io`, and
  `delegates_to_saveable`, which decides what gets framed.
* `core/src/core/machine.rs`: the `SaveState` trait (`save_state`,
  `load_state`, `load_state_traced`), bundled into `FrontendMachine`.
  `machines/src/lib.rs`: `machine_save_state!`, which generates all three.
* Surface: **68 `#[derive(Saveable)]` sites and 49 hand-written
  `impl Saveable`** (14 in `core/`, 35 in `machines/`). Manual impls include
  `i8035`, `mb88xx`, `m6502` and `m68000` — rev 1 named only the first two.

Per-field encoding: `u8`→1, `u16`→le 2, `u32`→le 4, `bool`→u8, `f32/f64`→le 4/8,
`[u8;N]`→`u32 len + N` (or per-element under `#[save_elements]`), `Vec<u8>`→
`u32 len + bytes`. Since Stage A, a nested `Saveable` field and an array of them
are framed by the parent as `tag:u16 | len:u32 | payload`; everything else stays
inline.

## Goals

1. **Contain invalidation** — a change to one component must not invalidate
   saves for machines that don't contain it.
2. **Order-immune** — source field order must not be wire order (Stage B).
3. **Additive-compatible** — a new field with a default must not invalidate old
   saves (Stage B).
4. **Forward-skippable** — unknown components and fields are skipped by length,
   not failed. Covers the Williams/Starwars conditional-device case.
5. **Fail loudly on partial loads** — see Required vs optional, below.
6. **Zero external dependencies** in `phosphor-core`, little-endian primitives
   unchanged.
7. **Auditable** — a dump tool can list chunks without source order knowledge.

**Non-goals:** JSON/human-readable, compression, encryption, `serde` compat,
cross-language schemas.

Rev 1 also listed "`no_std`-friendly". **Dropped as false**: `phosphor-core` has
no `#![no_std]`, `SaveError::InvalidFormat` carries a `String`, `StateWriter`
owns a `Vec<u8>`, and `machine.rs` uses `std::time::Duration`. The
zero-external-deps goal stands on its own; it does not need a `no_std` claim to
justify rejecting `postcard`.

## Wire format

All multi-byte integers stay little-endian.

```text
file      := header | chunk* | u32 crc32_ieee_le
header    := magic:4 b"PHOS" | file_version:u32 | machine_id: u32 len + utf8 bytes
chunk     := tag:u16 | len:u32 | payload:len bytes
```

* `file_version` replaces exact equality with `if file_version > CURRENT { … }`.
  Envelope changes bump it; component changes do not.
* **CRC covers `header || chunk*`, magic included.** (Rev 1 said "header+chunks"
  in one place and "not including magic" in another; resolved in favour of
  covering everything before the CRC field, which is the simpler rule.)
* A component payload is `component_version:u8 | body`, where `body` is either
  today's positional encoding (Stage A) or field TLVs (Stage B):

```text
field_tlv := field_tag:u16 | field_len:u32 | field_payload
```

Under TLV, `field_payload` drops the redundant inner length: `[u8;N]` and
`Vec<u8>` write raw bytes, since `field_len` already is the length.

### Two rules rev 1 got wrong

**Parents frame children; children never frame themselves.** Rev 1's sketch had
`M6809::save_state` call `w.begin_chunk(0) // tag assigned by machine` — a
struct cannot know the tag its parent filed it under — while its nested-field
case had the parent write the frame (`w.write_tlv(10, |w| self.cpu.save_state(w))`).
Both cannot hold. The rule is: `save_state`/`load_state` write and read a
*payload*, never a frame. `save_machine` and `write_tlv` own all framing.

**Readers must be bounded to their chunk.** Rev 1 said `read_tag_len` returns
`None` at EOF "so `while let` terminates at component boundary via `len`". EOF is
the *file* end; a nested struct looping to EOF reads straight through its own
chunk and consumes its parent's next fields. `StateReader` needs a scoped
sub-reader:

```rust
impl<'a> StateReader<'a> {
    /// Borrow the next `len` bytes as an independent reader; the parent's
    /// cursor advances past them regardless of how much the child consumes.
    fn sub(&mut self, len: u32) -> Result<StateReader<'a>, SaveError>;
    fn read_tag_len(&mut self) -> Result<Option<(u16, u32)>, SaveError>; // None at *this* reader's end
    fn skip(&mut self, n: u32) -> Result<(), SaveError>;
    fn remaining(&self) -> usize;
}

impl StateWriter {
    fn begin_chunk(&mut self, tag: u16) -> ChunkGuard;  // tag + u32 placeholder
    fn end_chunk(&mut self, g: ChunkGuard);             // patch len
    fn write_tlv<F: FnOnce(&mut Self)>(&mut self, tag: u16, f: F);
}
```

`sub` is what makes "skip an unknown component" and "a child that under-reads
doesn't corrupt its parent" both work, and it is the whole reason chunking is
worth anything.

### Required vs optional chunks

Rev 1 said "missing chunk → keep current / default device state". **That is a
correctness hazard for an emulator save state**: a truncated or mis-tagged file
would silently leave the CPU at frame N and a device at power-on, with no error.
Additive compatibility must mean *new fields may be absent*, not *any component
may be absent*.

The machine declares which top-level tags are required. `load_machine` tracks
which required tags it saw and returns `SaveError::InvalidFormat` naming any
that are missing. Optional chunks (the Williams CVSD, the Starwars Slapstic,
the System 1 speech board) are declared optional and may be absent — which is
exactly the case the current format cannot express.

### Tags

Per-struct, `u16`, stable across releases, assigned explicitly with
`#[save(id=N)]`. `0` and `0xFFFF` reserved. A retired tag is never reused and
readers skip it. Rev 1 left "never reuse" to reviewer discipline; add
`#[save_retired(3, 7)]` at the struct level so the derive can assert no live
field collides with a retired id. Name-hashed tags are rejected — renames would
silently break the wire.

`#[save_skip]` keeps current semantics. `#[save_elements]` becomes a no-op alias
under TLV (bulk bytes are the default and `field_len` is the length) and is
retired at the end of Stage C.

## Staged plan

The stages are independently shippable and each is worth landing alone. **Stage
A is the recommended near-term scope.**

### Stage A — envelope chunking and version containment

**Implemented.** See [What Stage A shipped](#what-stage-a-shipped) for how it
differs from this sketch.

1. Add `sub`/`skip`/`read_tag_len`/`begin_chunk`/`end_chunk`/CRC to
   `save_state.rs`.
2. `save_machine` writes one top-level chunk per component instead of a flat
   concatenation; `load_machine` dispatches by tag, `sub`-scopes each component,
   skips unknown tags, and errors on missing required tags.
3. `component_version` is checked **per component** rather than via the global
   `SAVE_VERSION`. A component whose version is newer than the reader
   understands fails *that component*; the machine reports which one.
4. `file_version` becomes a `>` check, bumped for the new envelope.

What this buys: the `cd0c917` and `e476018` class of bump — a single subsystem
changing — stops invalidating unrelated machines' saves. Field order inside a
component is still wire order, so changing a component still invalidates *that
component*, which is correct and honest. Nine of the eleven historical bumps
become non-events for most machines; the other two were genuinely broad and
would still be global.

### What Stage A shipped

Two departures from the sketch above, both forced by the code rather than
chosen for taste.

**"No derive changes" and "one chunk per component" cannot both hold.**
`save_machine` takes a single top-level `Saveable`, the machine wrapper struct,
and there is no component list anywhere for it to iterate. Worse, the machine
wrappers are not uniform: `JoustSystem` and `GalagaSystem` derive and have a
clean `cpu`/`board` split, but `GridleeSystem`, `CrystalCastlesSystem`,
`IrobotSystem`, `FoodFightSystem`, `MarbleSystem`, `DocastleSystem` and
`MissileCommandSystem` hand-write `impl Saveable` and flatten the board's fields
inline, so there is no component structure at their top level to chunk at all.

What shipped instead: **the derive frames every nested component, at every
depth.** One change to the delegate arm of `gen_field_io`, and all 68 derive
sites get containment for free without a line changing in any component body.
The 49 hand-written impls keep flat bodies and can be converted one at a time
with `write_tlv`/`read_component`; three were, below. Scalars and `Vec<u8>`
blobs stay inline; they carry no framing and never did.

This is strictly stronger than framing only the top level, which would have left
a board's contents an opaque blob and left an AVG-class change free to corrupt
its siblings inside that blob.

**Optional chunks needed the three conditional-field boards.** Stage A's issue
claims optional chunks for the Williams CVSD, the Star Wars Slapstic and the
System 1 speech board, but those are nested inside hand-written *board* impls,
not at any top level, so the claim could not be met without touching them.
`williams.rs`, `starwars.rs` and `atari_system1_sound.rs` now frame their
components and declare the per-variant ones optional. This is chunk-level
optionality, not Stage B's field TLV.

**One rule the optional path imposes.** Absence is detected by peeking at the
next tag, so an optional chunk must be followed by another chunk or by the end
of its enclosing reader. Inline scalars after an optional chunk would be read as
a tag, and if they matched, a component would be parsed out of scalar bytes.
There is no way to check this in `read_optional`; it is a rule for the body that
calls it, and the three converted boards obey it.

**What Stage A does not claim.** Ordinal tags renumber when components are
reordered, so a reorder is caught only where the two bodies differ, and two
components that encode alike swap silently. Insertion and removal change the
chunk count and are always caught. Reordering is a wire change and needs the
parent's `#[save_version]` bumped; explicit stable ids that survive it are
Stage B. The limitation is pinned by a test that will start failing when
Stage B lands.

**Cost.** Six bytes per chunk. Measured across ten machines: 24 bytes on
Pac-Man's 3.7 KB save (0.65%), 102 on Empire Strikes Back's 24 KB (0.43%), 42 on
Marble Madness's 1.05 MB (0.004%). Worst case measured was Tempest at 0.86%.

### Stage B — opt-in field TLV, for the structs that need it

Add `#[save(id=N)]` to the derive and a `#[save_tlv]` struct opt-in; structs
without it keep emitting positional bodies inside their Stage A chunk. Migrate
only where the payoff is concrete:

* Boards with conditional fields — `williams`, `starwars`,
  `atari_system1_sound` — which today append-and-guard.
* Any struct whose version has bumped more than once (`tms5220`, `gottlieb`).
* Any struct being reworked anyway; TLV-ify it as part of that commit.

This is where order-immunity and additive compatibility actually arrive, for the
~10 types that have ever needed them.

### Stage C — sweep (not recommended by default)

Migrating the remaining ~107 types. **This is the expensive stage and the case
for it is weak.** The 68 derive sites are mechanical, but the 49 hand-written
impls each need tag assignment and skip arms by hand, and 28 of 34 versioned
components have never changed once. A `Pia6820` that has been at version 1 since
the project began does not need order-immunity.

Do this only if Stage B proves the ergonomics are good and the sweep can be
largely automated. Otherwise a mixed codebase — TLV where it earns its keep,
positional where it doesn't — is a legitimate end state, since Stage A's chunk
framing makes the two interoperate.

### Stage D — tooling

**Implemented**, but not the way this sketched it. "Iterate tag/len and print a
hex preview" cannot work: a body interleaves inline scalars with framed
components, so nothing about the bytes says where a chunk starts, and walking
them speculatively would produce a tree that reads plausibly and is wrong.

`disasm dump-save` instead walks the file by *loading* it into a bare machine of
the id its header names, with the reader recording every chunk it enters
(`StateReader::with_trace`). The names are the reader's (`WilliamsBoard.cvsd`,
not `tag 9`), and a file that fails to load still prints everything read before
it stopped, which is normally the answer. It needs no ROM set, since a machine's
state layout does not depend on ROM contents.

`--machine <name>` with no file saves a freshly built machine and dumps that
instead: the layout this build expects, to diff against a file that will not
load.

## Alternatives considered

### Keep positional, bump `SAVE_VERSION`

The status quo. Costs a global save invalidation per refactor, four times so far,
twice for changes affecting one subsystem. Stage A removes most of that for a
few hundred lines in one file. Rejected.

### Full TLV in one pass (rev 1)

Rev 1's plan. Same end state as Stage C, but sequenced so that the cheapest,
highest-value part (containment) ships behind the most expensive part (migrating
117 types). Restructured, not rejected.

### `serde` + `postcard` / `bincode` / CBOR

`serde` solves order/additive/skip properly and `postcard` is small. Rejected
because `phosphor-core` deliberately has zero external dependencies and exact
little-endian encoding is part of the portability story. Note this rests on the
zero-dep policy alone — not on `no_std`, which `phosphor-core` is not.

### JSON / RON / MessagePack

3–10× larger, slower, and still need stable field ids for rename safety. No.

### Length prefix without tags

Insertion in the middle still breaks old readers, which would consume a `len` as
the next field's value. The tag is the load-bearing part.

## Overhead

Predicted a non-issue; measured, and it is. Framing costs 6 bytes per chunk
(`u16` tag + `u32` len). Stage A's per-machine totals, from
`disasm dump-save --machine <name>`:

| Machine | Save | Chunks | Framing | Share |
|---|---|---|---|---|
| Pac-Man | 3,701 | 4 | 24 | 0.65% |
| Tempest | 7,716 | 11 | 66 | **0.86%** |
| Mr. Do! | 9,202 | 3 | 18 | 0.20% |
| Q\*Bert | 22,551 | 11 | 66 | 0.29% |
| Empire Strikes Back | 23,961 | 17 | 102 | 0.43% |
| Joust | 51,114 | 10 | 60 | 0.12% |
| Sinistar | 55,253 | 13 | 78 | 0.14% |
| Marble Madness | 1,081,334 | 7 | 42 | 0.004% |

Worst case measured is 66 bytes. Save/load is not on the hot path. Stage B's
per-*field* TLV will cost considerably more, and should be measured again then
rather than assumed from these numbers.

## Open questions

* **Tag namespace for shared boards** (Joust vs Robotron on `WilliamsBoard`) —
  resolved: the board owns its tag space, stable across every game using it;
  `machine_id` in the header disambiguates the file.
* **Region blobs**: still open after Stage A. Regions are written with
  `write_bytes` (a `u32` length prefix) and stay inline rather than becoming
  chunks, so `read_bytes_into`'s length check is not yet redundant with an outer
  length and has not been relaxed. It becomes redundant under Stage B.
* **Do components need both a version byte and TLV?** Yes — TLV handles additive
  change, the version byte handles semantic change (`x: u16` → `u32`, or a
  re-interpretation of an existing field). Keep both.
* **Should Stage A bump `component_version` for every component?** Resolved: no.
  No component body changed, so bumping 34 versions would buy nothing. The
  envelope went to 13 and gained an explicit floor,
  `MIN_SUPPORTED_SAVE_VERSION`, so a version 12 or older file is rejected by
  version with a clear message rather than fed to the chunk reader and misread.
  That is one final global invalidation, and the last one: from here a component
  change bumps its own `#[save_version]` and leaves the envelope alone.

## References

* Format: `core/src/core/save_state.rs`.
* Derive: `macros/src/lib.rs`, `derive_saveable` and `delegates_to_saveable`.
* Glue: `machines/src/lib.rs`, `machine_save_state!`;
  `core/src/core/machine.rs`, `SaveState` (and `Nvram`, a raw blob that does
  not use this format at all); `frontend/src/main.rs` writes the NVRAM file.
* Boards with optional components: `machines/src/williams.rs`,
  `machines/src/starwars.rs`, `machines/src/atari_system1_sound.rs`.
* Manual impls: `core/src/cpu/i8035/mod.rs`, `mb88xx/mod.rs`, `m6502/mod.rs`,
  `m68000/mod.rs`, and 35 sites in `machines/src/`.
* What framing buys, and what it does not: `core/tests/save_state_framing_test.rs`.
* Tool behaviour: `tools/disasm/tests/dump_save_cli_test.rs`.
* Version-bump history: `git log -L/SAVE_VERSION/,+1:core/src/core/save_state.rs`.
