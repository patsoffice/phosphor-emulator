# Design: Chunked & TLV Save-State Format

> **Status: rev 4. Stages A, B and D are implemented; C is not.** Rev 1
> proposed replacing the positional binary `Saveable` format wholesale with
> tag-length-value framing. Rev 2 kept that as the end state but split it into
> stages, because the repo's own history shows most of the pain is fixed by the
> *first* stage: per-component chunking. Rev 3 records what Stage A actually
> turned out to be, which differs from rev 2 in one structural way (see
> [What Stage A shipped](#what-stage-a-shipped)), and corrects rev 2's stale
> anchors. `SAVE_VERSION` was 12 by the time Stage A was written, not 5: seven
> further global invalidations had landed since rev 2, every one of them a
> single subsystem changing. Rev 4 does the same for Stage B, which also differs
> from its sketch in one structural way (see
> [What Stage B shipped](#what-stage-b-shipped)), and records Stage B's answer
> to the question Stage C was deferred pending.

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
  the positional encoding (Stage A) or field TLVs (Stage B):

```text
tlv_body  := component_version:u8 | count:u16 | field_tlv{count}
field_tlv := field_tag:u16 | field_len:u32 | field_payload
```

Under TLV, `field_payload` drops the redundant inner length: `[u8;N]` and
`Vec<u8>` write raw bytes, since `field_len` already is the length.

`count` is not in rev 2's sketch and is not optional; see
[What Stage B shipped](#what-stage-b-shipped).

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

**Implemented.** See [What Stage B shipped](#what-stage-b-shipped).

Add `#[save(id=N)]` to the derive and a `#[save_tlv]` struct opt-in; structs
without it keep emitting positional bodies inside their Stage A chunk. Migrate
only where the payoff is concrete:

* Boards with conditional fields — `williams`, `starwars`,
  `atari_system1_sound` — which today append-and-guard.
* Any struct whose version has bumped more than once (`tms5220`, `gottlieb`).
* Any struct being reworked anyway; TLV-ify it as part of that commit.

This is where order-immunity and additive compatibility actually arrive, for the
~10 types that have ever needed them.

### What Stage B shipped

**A TLV body carries a field count, which rev 2's sketch does not have.** This
was found by Gridlee failing to load, and it is the most important thing on this
page. A TLV reader dispatches in a loop, and a loop needs to know when to stop.
Bounding it by the reader's end is only correct when the reader *is* the
struct's own bytes, which holds when a parent framed it. Stage A made all 68
derive sites frame their children, but **49 `Saveable` impls are hand-written
and frame nothing**: they call a child's `save_state` straight into their own
stream. `GridleeSystem` does exactly that with its `M6809`, so the moment the
M6809 became TLV its loop read on into Gridlee's RAM blob.

Four such sites existed (`GridleeSystem`, `IrobotSystem`, `GottliebBoard`,
`AtariSystem1Sound`'s speech section), and hand-fixing them was rejected: the
requirement is invisible at the call site, no compile-time check is possible
because framing is the parent's choice, and the failure is not reliably loud.
Gridlee errored only because the stray bytes happened not to parse as TLVs; had
they parsed, it would have loaded silently wrong.

A `u16` count after the version byte makes a TLV body self-delimiting, so a
struct can be opted in without auditing everyone who embeds it. Two bytes per
TLV struct instance. This does not contradict "children never frame themselves":
a child cannot know its own *tag*, but it always knows how many fields it has.

**An absent field is an error unless it says otherwise.** Rev 2 left this open.
`#[save(id = N)]` is required and `#[save(id = N, default)]` opts into absence,
keeping the field's constructed value. Making absence the default would have
softened, one level down, exactly the property Stage A went out of its way to
establish: rev 2 rejected "a missing chunk means keep current state" as a
correctness hazard, and a missing *field* leaving a device at power-on while the
rest of the machine is at frame N is the same hazard. Additive compatibility
then costs one word, at the moment the field is added.

**The writer emits fields in ascending id order**, not declaration order, so the
bytes are a function of the ids alone and reordering fields in the source is a
no-op on the wire in both directions rather than only for the reader.

**Migrated: the six derive sites whose version had ever moved.** `m6809` (2),
`tms5220` (3), `votrax_sc01` (2), `williams_blitter` (2), `gottlieb` (4),
`tempest` (2). All six are `#[derive(Saveable)]`, so migration is attributes
only. The three conditional-field boards were *not* migrated: they are
hand-written impls, and Stage A already solved their append-and-guard problem at
the chunk level, so field TLV would buy order-immunity for their scalars at the
cost of hand-rolling dispatch loops for about sixty fields.

**The envelope stayed at 13**, which is the whole Stage A payoff made concrete.
Each migrated struct bumped its own `#[save_version]`, so **10 of 33 machines
lose their saves** (Joust, Robotron, Sinistar, Star Wars, Empire Strikes Back,
Gridlee, I Robot, Q\*bert, Road Runner, Tempest) and the other 23 keep
byte-identical ones. Under the pre-Stage-A format this would have been all 33.

**Compile-time checks, with a gap.** The derive rejects `#[save_tlv]` without
`#[save_version]`, `#[save(id)]` or `#[save_retired]` without `#[save_tlv]`, a
missing id, a reserved id (0 or `0xFFFF`), a duplicate id, an id colliding with
`#[save_retired]`, and `#[save_elements]` under TLV. All eight were verified by
writing the bad struct and watching it fail, but **there is no permanent
regression test for them**: that needs `trybuild`, whose expected-output files
are brittle across toolchain bumps, and this repo pins its toolchain precisely
so that local and CI clippy agree. Re-verify by hand if the derive is reworked.

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

**Stage B's verdict on that question, now that it has landed:** split. For a
derive site the ergonomics are good and the sweep *is* automatable, save for
assigning ids, which cannot be automated safely because the ids are the wire
contract. `VotraxSc01` took 71 hand-numbered attributes. For a hand-written
impl nothing changed: it still needs a dispatch loop written by hand, and that
is where 49 of the remaining types are. The count byte does remove the argument
that a mixed codebase is *risky*, since a TLV struct is now safe anywhere, so
the case is purely cost against benefit, and the benefit is still absent for a
component that has never changed.

### What Stage C shipped

**The expensive part was never TLV.** Reading all 49 hand-written impls found
that about two thirds of them were hand-written for one removable reason each,
not because they were irreducibly bespoke, and the largest single reason was
that `AddressSpace16`/`32` did not implement `Saveable`, so 22 boards
hand-enumerated their memory a region at a time. Stage C was therefore re-shaped
into: give the address space its own impl, add the handful of derive features
the remaining impls were blocked on, and convert. **TLV came last and cost
least — it is attributes.**

**The address space saves the bytes the CPU can write, and knows which those
are.** A region is saved when `AccessKind::is_cpu_writable()` and it has
backing, so ROM drops out by construction and "a board forgot a region" stops
being a silent bug with nothing to catch it. `AddressSpace16` also saves its
page table, which carries bank switching; the argument is the same one level up,
since replaying banking from a board's own `load_state` is a call each board has
to remember to make and runs nowhere else on the normal path. The derived
writable set has now matched a hand-written list on **twelve boards
independently**, which is the only evidence that the rule matches what boards
expected.

**Five derive features, each unlocking impls rather than being wanted for its
own sake.** Tuple arrays (`[(u8, u8, u8); N]`), so an expanded palette is saved
rather than rebuilt. Nested arrays (`[[u8; 3]; 2]`). Fieldless enums, which is
three impls that existed for nothing else — and an unrecognised discriminant is
now an error naming the type, where every hand-written impl fell back to variant
zero. `Option<T>` fields, which is a per-variant component expressed as a field
and replaces the chunk-level `read_optional` Stage A had to add by hand.
`#[save_after_load]`, documented as a last resort.

**Two more features were designed and then not built**, which is worth
recording because the reasoning generalises. `Vec<T>` for primitive `T` and
tuples in field position each had exactly one prospective caller, and in both
cases the field's *type* was the actual defect: the mathbox's `Vec<u16>`s are
allocated once at a constant size and never resize, and Star Wars' `(f32, f32)`
is a filter's two-sample history. Fixing the types cost less than adding wire
surface with one caller apiece. **A type the derive cannot encode is sometimes
the type being wrong, not the derive being short.**

**`#[save_elements]` is retired**, as the stage planned. Under TLV a `[u8; N]`
is raw bytes and the field length is the length, so it could not do anything,
and it had no users left.

**"Do we need the post-load methods, or is it state we should save?"** That
question removed most of them. Banking went with the page table. Five palette
rebuilds went with tuple arrays. Two `refresh_dip_pots` calls went because the
POKEY already saves its own pot inputs. What is left for the hook is what a save
deliberately does not carry: a device re-reading a clock from configuration
(`reapply_speech_clock`, `resync_tms_clock`), or a value that must be brought
back into range before something indexes with it (the Slapstic's bank, an ADC
channel, an MB88xx RAM length).

**All 49 hand-written impls are gone.** Three remain in the tree and none of
them is a leftover:

* `AddressSpace16` and `AddressSpace32` — these *are* the mechanism. A map that
  derived its own body would have nothing to say about which regions the CPU can
  write, which is the whole point of the impl.
* `Namco51Wrapper` — a two-variant enum whose live variant is decided by whether
  a 51XX firmware ROM was found, and whose LLE variant cannot be constructed
  from a file at all. A derive has to build the variant the bytes name; here
  that is exactly what must not happen, so the twenty lines that check the
  variant instead are the correct answer rather than a deferred one.

The last five to convert had been classified as irreducibly bespoke, and were
not: re-reading them after the features above landed, the accessor round-trips,
optional components and post-load banking replays had already been answered, and
what remained in each was a field whose type the derive could not encode. **A
census taken before the tools existed goes stale, and the classification is the
thing to re-check, not the conclusion drawn from it.**

#### The rule from here

Rev 2 framed Stage C as migrating *every* type to TLV. That is not what shipped
and is not what should: about half the derive sites are still positional, and
they should stay that way until something else moves them.

**TLV a struct when you are changing it anyway; leave it positional otherwise.**
Order-immunity and additive compatibility are worth their two bytes at the
moment a struct's shape is in motion, and worth nothing to a `Pia6820` that has
been at version 1 since the project began — 28 of 34 versioned components have
never changed once. Stage A's chunk framing is what makes the mixture safe
rather than merely tolerable: a positional struct nested in a TLV parent is
framed by the parent's id, a TLV struct nested in a positional parent by the
parent's ordinal tag, and Stage B's field count means a TLV body is
self-delimiting wherever it lands.

So a mixed codebase is the intended end state, not a half-finished sweep.

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

Worst case measured is 66 bytes. Save/load is not on the hot path.

Stage B's per-*field* TLV was measured rather than assumed. It adds 6 bytes a
field plus 2 a struct, less the 4-byte inner length each `[u8; N]` and `Vec<u8>`
field stops carrying. Against the Stage A numbers above:

| Machine | Stage A | Stage B | Delta |
|---|---|---|---|
| Pac-Man (no migrated component) | 3,701 | 3,701 | 0 |
| Mr. Do! (none) | 9,202 | 9,202 | 0 |
| Marble Madness (none) | 1,081,334 | 1,081,334 | 0 |
| Tempest | 7,716 | 7,718 | +2 |
| Joust | 51,114 | 51,304 | +190 (0.37%) |
| Empire Strikes Back | 23,961 | 24,307 | +346 (1.4%) |
| Q\*bert | 22,551 | 22,993 | +442 (**1.9%**) |

Q\*bert is the worst because it carries the Votrax, whose 71 fields are mostly
one byte each, so the framing is six times the payload for most of them. Still
under 2% of a save, and the file is dominated by video RAM.

The zero rows are the point of Stage A: a machine containing none of the six
migrated components has a byte-identical save.

## Open questions

* **Tag namespace for shared boards** (Joust vs Robotron on `WilliamsBoard`) —
  resolved: the board owns its tag space, stable across every game using it;
  `machine_id` in the header disambiguates the file.
* **Region blobs**: resolved for TLV structs, still open elsewhere. In a
  `#[save_tlv]` body a `[u8; N]` or `Vec<u8>` field writes raw bytes and the
  field length is the length, so `read_bytes_into`'s check is genuinely
  redundant there and `read_raw_into` replaces it. Positional bodies still use
  `write_bytes`, and the memory regions in the hand-written board impls are all
  positional, so that check stays where it is. It goes away for a given board
  only when that board does.
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
* What field TLV buys over it: `core/tests/save_state_tlv_test.rs`.
* Tool behaviour: `tools/disasm/tests/dump_save_cli_test.rs`.
* Version-bump history: `git log -L/SAVE_VERSION/,+1:core/src/core/save_state.rs`.
