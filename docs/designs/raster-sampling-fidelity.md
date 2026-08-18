# Design: Raster Sampling Fidelity

> **Status: proposed.** Successor to the closed
> [`mid-frame-raster-fidelity.md`](mid-frame-raster-fidelity.md) epic
> (`phosphor-emulator-mid-frame-raster-7zzi`). That epic asked "which machines
> exhibit a mid-frame raster effect?", measured the answer, and correctly did
> almost nothing. This doc asks a different question — "at what rate does the
> hardware sample each piece of video state, and do we match it?" — and reaches
> a different answer. Nothing in the prior audit was wrong; the acceptance
> standard changed.

## Why revisit a closed epic

The Phase 0 audit ([`mid-frame-raster-audit.md`](mid-frame-raster-audit.md)) is
sound work and its measurements stand. But its conclusion rests on a scope
condition stated in its own Method section:

> their writes were captured across representative attract-**demo** gameplay
> (the self-playing demo exercises the same registers as a real game)

That parenthetical is an assumption, not a measurement. Nothing covered real
gameplay past the demo loop, later levels, cocktail mode, service mode, or
non-default DIP settings. So "Tier B" does not mean *this machine has no
mid-frame effects*. It means *none appeared in the attract loop*. Nine machines
carry that assumption, and nothing tracks or re-validates it when a ROM set is
added or a board is refactored.

Per-scanline rendering does not make that classification more accurate. It makes
it **unnecessary** — which is the actual reason to do this work, and the reason
it is worth doing on machines whose frames would not change at all today.

Two of the nine classifications are additionally judgement calls about
*visibility* rather than measurements of *absence*:

| Machine | Audit finding | Classified |
|---|---|---|
| burgertime | "scroll written 4× total at scanlines 88–91 across 4 frames (sporadic transitions)" | B |
| shollow (mcr2) | "palette written only in 4 of 2400 frames (screen-transition bulk reloads that span active display because the CPU rewrites 32 entries while the beam scans)" | B |

Both are genuine active-display writes. On hardware those frames show the rows
above the write with the old value and the rows below with the new one; we
render them uniformly. They were classified B because the effect does not repeat
and is not a deliberate raster trick. That is an argument from "nobody will
notice", and `CLAUDE.md` ranks **Correctness first and Performance last**.

## The principle

The goal is not "per-scanline everywhere". It is:

> **Sample each piece of video state at the rate the hardware samples it.**

This subsumes per-scanline rendering and also rules it out where it would be
wrong. `atari_system1` is the in-tree existence proof of both halves at once:
it renders motion objects in per-scanline bands driven by a mid-frame bank
switch, *and* it snapshots sprite RAM at vblank because the hardware's MO
circuit reads a list captured then, not live RAM:

```rust
// machines/src/atari_system1.rs:451
/// captured at the start of vblank: a copy of the sprite RAM plus the band
mo_shadow: Vec<u8>,
```

Rendering that board's sprites per-scanline from live RAM would introduce
tearing a real cabinet never shows. Per-scanline is more accurate only where the
video circuit reads live state.

### Live-read vs latched

The risk is not evenly distributed across layers, and that is what makes this
tractable:

| State | Hardware behaviour | Per-scanline verdict |
|---|---|---|
| Tilemap VRAM / colour RAM | Address generator reads as the beam scans | **Live** — per-scanline strictly better |
| Scroll registers | Read per scanline (or continuously) | **Live** |
| Palette RAM / PROM latch | Read per *pixel* as a lookup on the pixel value | **Live** — per-scanline is itself an approximation, but captures every case we've seen |
| Tile / sprite bank, flip latch | Read as the beam scans | **Live** |
| Sprite / motion-object list | **Board-specific.** Atari DMAs at vblank; some boards read live during hblank | **Must be determined per board** |

Nearly all the uncertainty sits in one layer. That permits taking the safe win
now and quarantining the part that needs research, instead of auditing every
register on every machine before touching anything.

## Current state

Nine render-once machines from the prior audit, with their render entry points:

| Machine | Board | Render structure | Buffer |
|---|---|---|---|
| qbert | gottlieb | `render_frame_internal` (`gottlieb.rs:731`) — layer-outer: fill → sprites/tiles ordered by `bg_priority` | u8 index + priority |
| shollow | mcr2 | `render_frame_internal` (`mcr2.rs:413`) — dirty-gated per-tile, then sprites | u8 index + priority |
| galaga | namco_galaga | `render_video` (`galaga.rs:590`) — fill → starfield → sprites → tilemap | u8 index |
| digdug, xevious | namco_galaga | same shape | u8 index |
| mrdo | mrdo | `render_frame` (`mrdo.rs:562`) — per-pixel whole-frame loop | u16 pen |
| burgertime | btime | `render_visible` (`btime.rs:573`) → `draw_background`/`draw_chars`/`draw_sprites` | native RGB |
| foodf | foodf | `render` (`foodf.rs:740`) — per-pixel whole-frame loop | pf_pen + RGB |
| marble | atari_system1 | `render` — playfield then MO bands | u16 index |

All nine already refresh correctly under the debugger — Phase 1 of the prior
epic (commit `e9380a3`) moved the render into a frame-boundary hook shared by
`run_frame` and `debug_tick`. `phosphor-emulator-ifs0` is closed and stays
closed; nothing here reopens it.

Note the structural pattern: these renderers are **layer-outer, scanline-inner**.
Even the ones already using the shared scanline helpers (galaga, digdug,
xevious, gottlieb call `render_tilemap_scanline_indexed`) call them from a
`for scanline` loop *inside* a whole-frame layer pass. Converting to true
per-scanline means inverting that nesting to scanline-outer, layer-inner. That
is the bulk of the labour and it is not mechanical.

---

## W1 — Per-scanline palette on indexed-buffer machines

**The cheapest real accuracy win in this doc, and it fixes the shollow case.**

`gottlieb.rs:873` and `mcr2.rs:539` have a byte-for-byte identical final pass:

```rust
pub fn render_frame(&self, buffer: &mut [u8]) {
    let mask = self.palette_rgb.len() - 1;
    for (i, &idx) in self.pixel_buffer.iter().enumerate() {
        let (r, g, b) = self.palette_rgb[idx as usize & mask];
        buffer[i * 3] = r;
        buffer[i * 3 + 1] = g;
        buffer[i * 3 + 2] = b;
    }
}
```

A flat iteration over the index buffer against a single palette. Because these
boards composite into a **palette-index** buffer and only resolve colour at the
very end, the palette can be made per-scanline **without touching tile or sprite
rendering at all**.

### Design

1. Add a per-scanline palette snapshot to the board:
   ```rust
   /// Palette as it stood at the start of each visible scanline. The hardware
   /// resolves pen → RGB as the beam passes, so a palette write during active
   /// display affects only the rows below it.
   palette_scanline: Vec<[(u8, u8, u8); N]>,   // N = 16 (gottlieb) / 64 (mcr2)
   ```
2. In the existing per-scanline hook (the boards already have a scanline
   boundary in `tick()` for their frame-boundary render), copy `palette_rgb`
   into `palette_scanline[scanline]`.
3. Change `render_frame` to iterate row-outer and index the row's snapshot:
   ```rust
   for y in 0..height {
       let pal = &self.palette_scanline[y];
       for x in 0..width { /* same body, pal instead of self.palette_rgb */ }
   }
   ```
4. Save-state: the snapshot is derived state rebuilt every frame — mark it
   `#[save_skip]`. On load the first frame resolves against a stale snapshot for
   one frame; seed it from `palette_rgb` in `load_state` to avoid that.

### Cost

Memory: gottlieb 240 rows × 16 entries × 3 bytes = **11.5 KB**; mcr2 480 rows ×
64 × 3 = **92 KB**. Per-frame work: one 48-byte (gottlieb) or 192-byte (mcr2)
`copy_from_slice` per scanline. Negligible. Zero change to the render loops.

### Applicability

gottlieb and mcr2 today. `atari_system1` uses a `u16` index buffer and the same
technique applies, but its palette is larger and its resolve pass differs —
treat it as a follow-on, not part of W1.

This also neatly sidesteps a conflict: mcr2 is simultaneously the machine that
most needs a mid-frame fix and the one whose renderer least fits per-scanline
rendering (dirty-gated per-tile cache at 2× upscale — see
[`graphics-consolidation.md`](graphics-consolidation.md), which recommends
against migrating it on structural grounds that still hold). W1 fixes mcr2's
actual defect without touching the structure that makes it a bad migration
target.

**Acceptance:** golden frames byte-identical (the audit says these boards write
palette during vblank in all but 4 of 2400 frames). A targeted test writes the
palette at a known active-display scanline and asserts only rows below it
change.

---

## W2 — Burgertime background scroll during active display

The audit measured 4 writes to `0x4004` at scanlines 88–91 across 4 frames.
That register is `bnj_scroll0` (`btime.rs:274`), which drives three things in
`draw_background` (`btime.rs:701`):

* bits 0–1 → horizontal scroll, `-((bnj_scroll0 & 0x03) << 8)`
* bit 2 → background tile-bank offset
* bit 4 → background layer enable (`btime.rs:576`)

### Confirm before building

`bnj_` is the Bump 'n' Jump register set; this board is shared. **First
establish whether Burger Time enables the background layer at all** — if
`bnj_scroll0 & 0x10` is never set on this ROM set, `draw_background` returns
early and the four writes are invisible regardless of when they land. If so,
close W2 as a non-issue and record that.

If the layer *is* enabled, honour the write per scanline: sample `bnj_scroll0`
into a per-scanline array in the frame hook and have `draw_background` consume
the row's value. This is the same shape as W1 and does not require restructuring
`btime`'s tile blitter.

**Acceptance:** golden frame unchanged for the 2396 frames with no mid-frame
write; a targeted test asserting a mid-frame scroll write splits the background.

---

## W3 — Latched-vs-live determination for the sprite layer

**Prerequisite for any sprite-layer per-scanline work. Research, not code.**

The prior audit traced *when the CPU writes*. That does not answer *when the
video circuit reads*, and it is the read side that decides whether per-scanline
sampling is correct. Determine per board, from schematics and MAME's driver:

* Does the sprite/MO circuit read a list captured at vblank (DMA, shadow RAM,
  double buffer), or does it read sprite RAM live during hblank per scanline?
* If latched: at which point in the frame, and is the capture a full copy or a
  per-line fetch?

Boards to cover: `namco_galaga` (galaga/digdug/xevious), `btime`, `mrdo`,
`foodf`, `gottlieb`, `mcr2`, `atari_system1` (known latched — use as the
reference), plus the already-per-scanline boards for completeness.

Record the result in a table in this doc. Two outcomes per board:

* **Live** → sprites may be rendered per-scanline from live RAM (W4).
* **Latched** → model the latch explicitly, `mo_shadow`-style, and keep
  whole-frame sprite compositing. **This is an accuracy improvement in its own
  right**, independent of W4: a board that reads a vblank-captured list but
  renders from live RAM at end-of-frame is wrong today in the opposite
  direction, and nobody has checked.

W3 is worth doing even if W4 never happens.

---

## W4 — Scanline-outer restructure of the tilemap layer

For the remaining Tier B machines, invert the render nesting from layer-outer to
scanline-outer for the **live-read layers only** (tilemap, scroll, palette,
bank, flip). Sprites follow W3's per-board answer.

### The safety property that makes this attractive

The audit establishes that these machines do not write video registers during
active display in the measured window. Therefore per-scanline rendering must
produce **byte-identical output** on every golden-frame pin. `golden_frame_test`
(SHA-256 of the oriented frame for every registered machine) becomes an exact
regression gate: **any hash change is a bug, not an expected improvement, and
must not be recaptured.** That is an unusually strong position for a
correctness refactor — the accuracy guarantee is obtained at zero output risk,
verified by tests that already exist.

### Per-machine shape

1. Add a persistent scanline buffer if the board lacks one (most have an index
   or native buffer already).
2. In the frame hook, at each visible scanline boundary, call
   `render_scanline(n)` reading live state.
3. Inside `render_scanline`, run the layers in the board's existing order for
   that one row — the enriched helpers from
   [`graphics-consolidation.md`](graphics-consolidation.md) (`TileInfo` +
   `Option`-returning resolver, indexed and scrolled variants) make the inner
   loops free.
4. `render_frame` becomes the copy/resolve of the persistent buffer.
5. Delete the whole-frame layer passes.

Note that layer *order* can itself be a mid-frame-variable register — gottlieb
picks tiles-over-sprites vs sprites-over-tiles from `video_control & 0x01`
(`gottlieb.rs:732`), read once per frame today. Under scanline-outer rendering
that becomes per-row for free.

### Estimated cost

Roughly 50–100 lines per machine, nine machines. The helpers exist; the
restructure is the work. `mrdo` and `foodf` are per-pixel whole-frame loops
rather than layer passes and will need more care.

### Explicitly out of scope

* **mcr2** — dirty-gated per-tile cache at 2× upscale. W1 addresses its actual
  defect; a per-scanline restructure would fight its dirty-tracking design for
  no measured benefit.
* **Vector machines** — no scanline hardware.
* **Machines already rendering per-scanline** — the 19 listed in the prior
  audit. No action.

---

## Testing

```bash
cargo test -p phosphor-machines
cargo test -p phosphor-harness --test golden_frame_test   # the gate
cargo clippy --all-features --all-targets
cargo run --release -p phosphor-bench -- --roms <path>    # before/after, W4 only
```

* **Golden frames are the primary gate and must not be recaptured** for W1, W2
  or W4. Every one of these changes is expected to be byte-identical on the
  pinned frames; a diff means the migration changed behaviour on a frame where
  the hardware did not.
* **Per-work-item targeted tests** proving the mid-frame case: write the
  register at a known active-display scanline on a synthetic or ROM-booted
  board, assert only rows below the write differ. Without this, a migration can
  be byte-identical *and* still not honour mid-frame changes — the golden
  frames alone cannot distinguish the two.
* **No vblank regression:** assert vblank-latched state (galaga's
  `update_starfield_at_vblank`, `atari_system1`'s `mo_shadow`) is not
  re-sampled per line after any W4 migration. This is the main way W4 could
  introduce a new bug.
* **Perf:** W1 and W2 need no measurement. W4 changes tile-info lookups from
  per-tile to per-tile-per-scanline (8× for 8-pixel-tall tiles — on a 36×28
  tilemap, 1008 → 8064 lookups per frame). Expected negligible; confirm with
  `phosphor-bench` in release, per `CLAUDE.md`.

## Risks

1. **W4 introduces tearing on latched sprite layers.** Mitigated by making W3 a
   hard prerequisite for any sprite-layer change. This is the main reason not to
   do a blanket sweep.
2. **Byte-identical is not proof of correctness.** A migration that renders
   per-scanline but accidentally samples state once still passes the golden
   gate. Hence the mandatory targeted mid-frame test per work item.
3. **W2 may be a non-issue.** Guarded by the confirm-first step.
4. **Scope creep into the per-scanline machines.** The 19 already-correct boards
   are out of scope; W3's research covers them only for completeness.

## Sequencing

`W1 → W2 → W3 → W4`, and each is independently shippable.

W1 and W2 are small and fix things that are wrong now. W3 is cheap research that
de-risks everything after it and improves accuracy on its own. W4 is the bulk of
the labour and should not start until W3 has answered the latch question for the
board in hand.

A reasonable stopping point is after W3: the two known-wrong cases fixed, the
latch behaviour documented and modelled, and per-scanline adopted as the default
for new machines — without a nine-machine restructure. W4 is worth doing, but it
is the part to defer if effort is constrained.

## Standing convention (adopt regardless)

Add to `machines/CLAUDE.md`: **new raster machines render per-scanline from live
state by default**; a whole-frame render needs a comment justifying it, and any
state the hardware latches at vblank is modelled as an explicit snapshot rather
than read live. This is what stops the ratio drifting back.

## Open questions

* **Is palette per-scanline or per-pixel enough?** Hardware resolves pen → RGB
  per pixel. Scanline granularity captures every case observed so far, but a
  game that rewrites the palette mid-*line* would still be quantised. Accepted
  for now, consistent with Closed Decision 3 of the prior epic.
* **Does Burger Time enable the BnJ background layer?** Decides whether W2 is
  real. First step of W2.
* **Should `render_frame`'s index→RGB pass become a shared helper?** gottlieb
  and mcr2 have identical implementations and W1 changes both the same way. A
  `gfx::resolve_indexed_scanline` would dedupe them — worth doing as part of W1
  rather than editing the same loop twice.
* **atari_system1 `u16` palette snapshot** — same technique, larger palette,
  different resolve pass. Follow-on to W1; not scoped here.

## References

* Prior epic: `phosphor-emulator-mid-frame-raster-7zzi` (closed),
  [`mid-frame-raster-fidelity.md`](mid-frame-raster-fidelity.md),
  [`mid-frame-raster-audit.md`](mid-frame-raster-audit.md).
* Latch modelling reference: `machines/src/atari_system1.rs:451` (`mo_shadow`),
  `machines/src/galaga.rs:828` (`update_starfield_at_vblank`).
* Identical index→RGB passes: `machines/src/gottlieb.rs:873`,
  `machines/src/mcr2.rs:539`.
* Burgertime background: `machines/src/btime.rs:274` (`bnj_scroll0`), `:576`
  (enable gate), `:701` (`draw_background`).
* Scanline helpers: `core/src/gfx/tilemap.rs:55,107,162,216`,
  `core/src/gfx/sprite.rs:20,108`; see
  [`graphics-consolidation.md`](graphics-consolidation.md).
* Frame-boundary hook precedent: `machines/src/galaga.rs:814`
  (`tick_frame_boundary`), commit `e9380a3`.
* Gate: `harness/tests/golden_frame_test.rs`, `harness/tests/golden/frames.toml`.
