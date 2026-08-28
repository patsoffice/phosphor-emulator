# Design: Frame-Level Regression Testing

> **Status: implemented.** Tracks beads epic
> `phosphor-emulator-frame-regression-w1pi` and its two children.

## Context

Nothing in the repo pins what any machine is *supposed to look like*. The
suite can tell you that a machine boots (`harness/tests/boot_check_test.rs`
asserts every registered machine leaves its reset PC and lights at least one
pixel), that its save state round-trips, and that its control table is
well-formed. None of that notices a palette entry swapped, a sprite drawn one
line high, a scroll register read from the wrong latch, or a tile bank
selected off by one. Those are found today by a human launching that specific
game.

`tools/script/tests/frameshot_parity.rs` looks like the missing test and is
not. It boots Mr. Do twice — once through `Harness`, once through
`DebugSession` — and asserts the pixels match. That catches wiring drift
between two of *our own* code paths; a change that breaks both identically
still passes.

This matters now because the next two epics — concrete bus dispatch
(`phosphor-emulator-concrete-bus-dispatch-blzz`) and the cycle-accurate CPU
rewrites — are exactly the changes most likely to move pixels while every
existing test stays green.

Everything needed already exists and is unused for this purpose: `Harness`,
`apply_orientation`, `FrontendMachine::render_frame`,
`FrontendMachine::vector_display_list`, and the `disasm frameshot` / `disasm
imgdiff` pair for inspecting a failure.

## Design

A single ROM-gated test, `harness/tests/golden_frame_test.rs`, driven by a
committed data file. For each entry it boots the named machine, runs the
named number of frames (applying any scripted input), renders, hashes, and
compares against the pinned hash.

### Where it lives

`phosphor-harness`, next to `boot_check_test.rs` and `save_state_rom_test.rs`,
for the same reason those are there: this crate already depends on
`phosphor-machines`, already owns the resolve-ROMs-and-boot sequence, and
already exports the `roms_dir()` gating convention. `phosphor-machines`
cannot host it without a dependency cycle.

`png` and `toml` are **dev-**dependencies of `phosphor-harness`, so nothing
downstream (the frontend links this crate) grows a dependency.

### The data file

`harness/tests/golden/frames.toml`, one `[[frame]]` table per machine:

```toml
[[frame]]
machine = "mrdo"
frames = 3100
shows = "Attract-mode demo: Mr. Do digging through the cherry field."
size = [192, 240]
frame = "sha256:1f0a…"
```

Design points, each answering a requirement of the epic:

- **Data, not source.** Refreshing a machine is a diff in this file. A reviewer
  sees which machine moved and by how much (`frames`, `size` and `shows` are
  all human-legible), not a hex literal buried in a `match` arm.
- **`shows` is mandatory prose.** The epic's warning — "pinning a wrong frame
  is worse than no test" — only holds if someone looked. The field is the
  record that someone did, and it is what the next reader diffs against when
  the image changes.
- **`size` is pinned separately** from the hash. A geometry change is the most
  likely single cause of a mass failure, and reading `192, 240` → `224, 288`
  in the diff is faster than decoding two hashes.
- **`press` is optional** input scripting, for machines whose attract mode is
  not representative:

  ```toml
  press = [{ control = "coin", at = 600, hold = 8 },
           { control = "start1", at = 640, hold = 8 }]
  ```

  These resolve through the same `Harness::build` path (by stable control
  name) that `disasm frameshot --press` uses, so a golden entry cannot drift
  from what the tooling can reproduce by hand.
- **`nvram` is an optional CMOS fixture**, a path under `tests/golden/`, loaded
  right after reset the way `disasm frameshot --nvram` does. Only the three
  Williams machines need one, for the reason in *Findings* below.

### What gets hashed

SHA-256 over a length-prefixed canonical encoding, so a buffer that changes
shape without changing bytes still changes the hash.

- **Raster machines** — the frame rendered natively and then passed through
  `apply_orientation`, i.e. what the cabinet displays and what `disasm
  frameshot` writes. Hashing the *oriented* buffer means a machine that
  silently loses its `ROT90` declaration fails here.
- **Vector machines** — `vector_display_list()`, each `VectorLine` encoded as
  four `i32` LE coordinates plus `intensity, r, g, b`.

That second choice is the one the epic asked to be decided and documented.

#### Why vector machines pin the line list, and the raster frame too

For `asteroid`, `astdelux`, `llander`, `tempest`, `quantum`, `starwars`, `esb`
and `irobot`, `vector_display_list()` *is* the output: the frontend feeds it
to GL directly, and `render_frame` is a CPU rasterisation used for headless
capture and for the debug UI. Pinning only the raster fallback would let a
real regression hide — the rasteriser quantises to a 2-D grid and drops
sub-pixel endpoint differences and low-intensity vectors that the GL path
draws.

Pinning only the line list has the opposite gap: it would not notice
`rasterize_vectors` breaking, which is a real code path with real consumers.

So vector entries carry **both** hashes, `vectors` and `frame`, and the
failure message names which one moved. That is the whole reason the two are
separate fields rather than one blended digest.

There is one wrinkle worth stating: a vector display list is only meaningful
on a frame the vector generator actually ran to completion. Both are sampled
at the same instant as the raster frame, after `frames` frames, which is the
same instant `boot_check_test.rs` already asserts is non-empty for the vector
games.

### Reference PNGs

Update mode writes `harness/tests/golden/<machine>.png` alongside the hash and
commits it. It is deliberately **not** the source of truth — the hash is — but
it earns its ~20 KB three times over:

1. A golden refresh shows as an *image* diff in review, which is the only
   review that can catch "the new frame is wrong".
2. On failure the test writes the actual frame to
   `harness/tests/golden/actual/<machine>.png`, so `disasm imgdiff
   golden/<m>.png golden/actual/<m>.png --out /tmp/d.png` gives a highlighted
   picture of exactly which pixels moved, rather than a hex diff.
3. It cannot drift from the hash, because a ROM-less test asserts
   `sha256(decode(<machine>.png)) == frame`. Both are produced by the same
   capture; the check makes the PNG a verified artifact rather than a
   decoration.

Vector machines get a PNG of the rasterised fallback, which is also what their
`frame` hash covers.

### Update mode

```
PHOSPHOR_GOLDEN_UPDATE=1 cargo test -p phosphor-harness --test golden_frame_test
```

Recaptures every entry, rewrites `frames.toml` canonically, rewrites the
reference PNGs, and prints which entries changed. `frames`, `shows` and
`press` round-trip from the existing file — the human-authored fields are
inputs, the hashes and `size` are outputs. Comments outside an entry are not
preserved, which is why the file carries a generated header and all prose
lives in `shows`.

`nvram` round-trips with the other human fields; the fixtures themselves are
dumped once with `disasm frameshot --dump-nvram` and committed.

Update mode passes rather than fails. It is opt-in via an environment
variable, and CI has no ROMs, so it cannot silently neuter the suite there;
the review of the resulting diff is the gate.

### Guards against a vacuous pass

The epic's central risk is a golden suite that passes having checked nothing.
Five guards, three of which run **without ROMs** so CI enforces them:

| Guard | Needs ROMs | Catches |
|---|---|---|
| `frames_toml_covers_every_registered_machine` | no | a machine registered with no pinned frame |
| `every_entry_names_a_registered_machine` | no | an entry left behind by a rename or removal |
| `reference_pngs_match_their_hashes` | no | a hand-edited hash, a stale PNG, an empty golden set |
| ROM-gated: at least one entry ran | yes | a ROM dir present but supplying nothing |
| ROM-gated: no pinned frame is uniform | yes | pinning an all-black screen, which any breakage passes |

The first guard is the one that makes the suite registry-driven: adding a
machine to the registry fails CI until a golden frame is captured for it. That
is the intended pressure — an unpinned machine is an unguarded machine — and
it is the same shape as the `the_registry_is_not_empty` guards on the existing
registry-driven suites.

## Choosing a frame count

The pinned frame has to be *past* the machine's power-on self-test and *in* a
part of the attract loop that is representative. Both bounds are real:

- Too early and the frame is the RAM-test screen or plain black. Galaga needs
  about 3000 frames, Mr. Do 3100 (`examples/capture.rhai` and
  `frameshot_parity.rs` already encode those two).
- The attract loop cycles through title / high scores / demo, so the count
  also picks *which* of those is pinned. A demo-gameplay frame exercises
  sprites, tile banking and scroll; a static title screen mostly exercises the
  tilemap and palette. Prefer the busier frame where the machine offers one.

There is no nondeterminism to worry about — no wall clock and no RNG in the
tick path — so a fixed frame count is reproducible. That is the same property
`frameshot_parity.rs` relies on.

## Findings from the first capture

Looking at 39 frames turned up two bugs no existing test could see, plus one
boot behaviour that changes how three machines are pinned. They are recorded
here because they are the argument for the epic.

**Super Cobra rendered 90° off.** `impl_board_delegation!(ScobraSystem, board,
TIMING)` omitted the `orientation` flag its Scramble sibling one screen away
passes, so Scobra fell back to `Orientation::NORMAL` and drew a portrait
cabinet's raster unrotated. Fixed here, with
`scobra_declares_the_same_rotation_as_scramble` beside the existing Scramble
tests so it cannot come back.

**Tempest's headless render is 90° off** (`phosphor-emulator-iitc`).
`AtariAvgBoard::render_frame` rasterises with `flip_y = false`, which
`rasterize_vectors` documents as already-screen-space for ROT270; every caller
then applies the declared `ROT270` on top. The frontend escapes it because its
GL shader's `rotation == 270` branch only negates Y instead of rotating, so
the live game looks right and the two paths silently disagree —
`frameshot_parity.rs` cannot see it because both of its sides go through
`render_frame`. Not fixed here: the fix is a decision about where the AVG's
screen mapping belongs, and it needs its own change. Tempest's entry pins the
current, wrong-way-up frame and says so in `shows`, which still guards
everything else about Tempest and will fail loudly when the orientation is
corrected. *(Fixed 2026-08-25 under `phosphor-emulator-iitc`. It did fail
loudly, but not until 2026-08-27: the comparison was `#[ignore]`d at the time,
so the upright frame was only pinned when it came back on.)*

**Joust, Robotron and Sinistar sit on `FACTORY SETTINGS RESTORED` from a cold
boot** — Joust still shows it at 10,000 frames, about 166 emulated seconds.
This one is not a defect: with blank battery RAM the board factory-resets and
holds that message until an operator presses the reset button, which is what
the hardware does. It still matters for pinning. `boot_check_test.rs` passes
them because the message is lit pixels, and a frame pinned on it would guard
about as little, so the three entries load a committed `nvram` fixture and pin
the attract-mode frame that follows.

The two bugs are the epic's thesis in miniature: both are *rendering* bugs in
machines whose boot checks, save-state round trips and control tables were all
green.

## Cost

Roughly 4 s per 3000 frames per machine on release, ~3× that under the dev
profile the test suite uses. Across the roster that is a few minutes, which is
why this is ROM-gated and local rather than part of every `cargo test`. It
sits with `boot_check_test.rs`, which already spends a comparable budget.

Since `ea633f7` the captures run one machine per core, so the whole roster is
26.9 s of wall time against 173.5 s sequential. The reference PNGs are 429 KB
of the tree after the png 0.18 re-encode in `b81e5d7`.

## What this does not do

- **No tolerance.** The comparison is exact. A machine whose output legitimately
  varies frame to frame would need a tolerance, and none of the 40 do — the
  frame at N is a pure function of the ROM set and N.
- **No audio.** `save_state_rom_test.rs` already compares audio across a
  restore; pinning an absolute audio hash would break on every resampler
  change, and the audio path has its own epic
  (`phosphor-emulator-audio-output-path-oe0b`).
- **It does not say the frame is *right*,** only that it has not changed. The
  `shows` field and the committed PNG are what carry the human judgement that
  it was right when pinned.

## Review: what the pins have actually done

> Answers `phosphor-emulator-gr27`, which asked what the comparison has caught
> against what it has cost in recaptures, and whether to re-enable, narrow or
> retire it. Written 2026-08-27 over the twelve days from the first capture.
> The outcome was keep both hashes and narrow nothing, and the reason is not
> the one the issue expected.

### The cost, measured

Four recapture events, thirteen machine-recaptures, ten distinct machines.
Twenty-nine of the thirty-nine pinned machines have never been recaptured at
all.

| Event | Machines | Why the picture moved |
|---|---|---|
| `b22534b` vector generator runs in emulated time | quantum, starwars, tempest | animation phase, and a different attract level from a moved RNG seed |
| `9ba89f9` a slow Williams blit costs two cycles a byte | robotron, sinistar | colour-cycle phase, and a different moment of the same text screen |
| `1461ee4` MCR II's dot clock is master/2 | shollow | the attract loop reaches the SHIELD screen instead of the title |
| `35048ac` vector renderer rework, and iitc with it | the seven vector machines | f32 display-list coordinates, the `display_size` split, beam width, dwell, halation, vertex dots, and tempest's orientation fix |

Three of the four were timing fixes, and in each the picture moved because the
machine had got somewhere else in its attract loop, not because the board drew
differently. That is exactly the complaint the issue was filed over, and it is
real: a hash cannot tell those apart, and the reviewer has to.

One of the thirteen, tempest, was a correction to what a machine draws.

### The catches

In twelve days, no comparison failure has revealed an unknown defect. Every red
run has been a change someone had just made on purpose.

Two real bugs were found *while pinning*, which is a different thing: Super
Cobra rotated 90 degrees, fixed in `1ea6381`, and Tempest's double-applied
`ROT270`, filed as `phosphor-emulator-iitc` and fixed nine days later. Both came
from a human looking at 39 pictures once. That yield is spent.

The i32 beam-flip overflow (`e5a16b1`) was found by a debug-build panic, and the
Quantum runaway frame beside it by reading frame counts during that
investigation. The pins saw neither, because the beam only leaves the screen in
states the pinned frame does not reach.

### The column the question did not have

Eleven commits cite an *unchanged* golden frame as their evidence. That, not the
failure, is what the suite is actually used for:

- `98831fd` CPUs take the bus as a type parameter, `e7b1d18` and `0820f4f` the
  concrete-bus dispatch refactor across every machine
- `facba15` frame rendering and hashing moved into the harness, i.e. a refactor
  of the hashing path itself
- `671b6e6` Donkey Kong sprite fetch wrap: "All 40 golden frames pass unchanged,
  which is the acceptance test that matters: it proves normal play never reached
  this path"
- `e5a16b1` the beam-flip fix, `ba9c701` the MCR CTC cascade, `d49fcd3` the last
  hand-rolled clock accumulators

A null result is only worth something when the instrument could have moved, so
these split in two. For the bus and rendering refactors, the sprite-fetch wrap
and the beam-flip fix, a mistake lands directly on pixels and "byte-identical"
is a real acceptance test. For `7b2d041`, `60c1467` and `e525097`, all sound
clocks, the commits argue structurally that sound cannot reach video, and the
unchanged frame confirms an argument already made rather than making it.
`27f46f0` is the honest edge: Food Fight's IRQ1 scanlines moved, its new tests
fail against the old positions, and the picture did not move at all, so there
the pin was simply insensitive.

Roughly eight of the eleven are load-bearing. That is still more work than the
suite has done in either of the other two columns.

### Why the narrowing proposed in the issue was not done

The issue suggested pinning only the `vectors` hash for the vector machines,
since it is renderer-independent and is what the frontend draws. The history
refutes it on both counts.

It would save nothing. Across every vector recapture there has ever been, ten
machine-recaptures over `b22534b` and `35048ac`, the frame hash and the vectors
hash moved *together* every single time. Neither has ever moved alone. Dropping
one would have shortened the diff by a line and avoided no recapture.

And it would have cost the one real catch in the set. `phosphor-emulator-iitc`
was a defect in `render_frame`, the raster fallback, which the vectors hash
cannot see: it is orientation-free by construction, which is precisely why the
old `shows` prose said it "pins the real output" while the committed picture was
sideways.

So both hashes stay. The recapture cost is not caused by having two hashes; it
is caused by pinning a frame sampled from a free-running attract loop, and it
would survive the narrowing untouched.

### What this means for the conformance-ROM programme

`phosphor-emulator-conformance-rom-programme-hl4t` argues that golden frames
carry two jobs, pinning artwork and standing in as the timing guard, and do the
second badly. That is right, and this review adds a third job that neither
document named: saying that a change moved no picture at all. Conformance ROMs
do not take that job either, because they draw their own patterns and know
nothing about a machine's artwork.

It also sharpens what the programme can expect to save. Taking the timing job
away does not reduce the recapture cost, because a timing fix still moves the
attract phase and still forces a recapture. What it buys is that the recapture
stops being the only evidence: the conformance ROM says what the timing now is,
derived, while the golden frame goes on saying only that the picture is
different.
