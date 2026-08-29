# Design: Raster Sampling Fidelity

> **Status: proposed; W3 done (2026-08-28) and its finding retracts one of this
> doc's own premises. See [W3](#w3--latched-vs-live-determination-for-the-sprite-layer).**
> Successor to the closed
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

This subsumes per-scanline rendering, and it would rule it out anywhere the
hardware latched. **W3 has since established that nothing in the registry does**
(see the W3 result below), so the principle now points the same way for every
layer on every board.

The original text here claimed `atari_system1` as the in-tree existence proof of
a vblank-latched sprite layer, on the strength of its `mo_shadow` field. That was
wrong: the board's motion-object path is a double-buffered horizontal line
buffer, and `mo_shadow` compensates for our whole-frame render rather than
modelling a circuit. The claim is retracted; the field stays until the renderer
can do without it.

### Live-read vs latched

The risk was thought to be concentrated in the sprite layer. It is not
distributed anywhere, because there is no latched layer:

| State | Hardware behaviour | Per-scanline verdict |
|---|---|---|
| Tilemap VRAM / colour RAM | Address generator reads as the beam scans | **Live** — per-scanline strictly better |
| Scroll registers | Read per scanline (or continuously) | **Live** |
| Palette RAM / PROM latch | Read per *pixel* as a lookup on the pixel value | **Live** — per-scanline is itself an approximation, but captures every case we've seen |
| Tile / sprite bank, flip latch | Read as the beam scans | **Live** |
| Sprite / motion-object list | Object list walked once per scanline off the horizontal counter, into a line buffer displayed on the next line (W3, nine boards read) | **Live**, sampled one line ahead |

The remaining subtlety in the sprite row is the one-line lead, not a latch: the
line a sprite pixel appears on was composited from the object list as it stood
during the *previous* line.

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

**Done for `gottlieb`, 2026-08-28. The `mcr2` half is split out as
`phosphor-emulator-raster-sampling-6kae.6` and deferred, because its blanking
phase is inside a custom LSI and is on no drawing; the reason is at the end of
this section.**

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

### What shipped, on gottlieb

Built as designed, with three corrections to the plan above.

* **There was no existing per-scanline hook.** The design says "the boards
  already have a scanline boundary in `tick()`"; `mcr2` does and `gottlieb` did
  not. Its `run_frame` was a plain cycle loop and its only frame-position test
  was the end-of-frame render. So `gottlieb` gained `begin_scanline`,
  `run_scanlines` and a scanline-outer `run_frame`, mirroring `mcr2`'s shape,
  with the boundary test also in `tick` so the debugger's single-step path
  samples too.
* **The shared helper is `gfx::resolve_indexed_rows`** (`core/src/gfx/palette.rs`),
  row-outer with a `palette_for_row` closure. `mcr2` calls it with a closure that
  returns the same palette for every row, so the duplicated loop is gone and the
  one board that samples per row and the one that does not are visibly different
  at the call site rather than silently identical.
* **Byte-identity was contingent, and held.** The audit's "all but 4 of 2400
  frames" is about *when* the palette is written, and byte-identity actually
  needs the palette *values* to be steady across the pinned frames. They are:
  all 12 golden frames are unchanged, as are boot, save-state and audio.

**The phase this creates, stated out loud.** Tiles and sprites are still
composited once at the frame boundary, which on this board is the end of vblank;
the palette rows come from the visible period *before* that vblank. The palette
is now correctly phased against the beam and the other layers are not, so a
frame where the game rewrites VRAM and palette together during vblank shows the
new tiles against the old palette for one frame. Before this change both layers
were sampled at the same wrong moment and were at least consistent with each
other. This is the price of fixing one layer at a time and it goes away under
W4; it is recorded on `palette_scanline`'s doc comment as well as here.

### Why mcr2 did not ship

Not effort, and not the dirty-tile renderer this section was written to sidestep.
The design assumed a scanline index that maps to a framebuffer row. On this board
it does not, and nothing establishes what it maps to:

* The board steps in **line pairs** across an interlaced 512-line frame:
  `cycles_per_scanline` is 317 for two lines and `total_scanlines` is 256 pairs.
  The index has never had a consumer other than the CTC triggers, so its phase
  against the visible area was never needed and was never fixed.
* The framebuffer is genuinely 480 distinct rows, not 240 doubled: tiles are
  drawn at 2× (`screen_y0 = tile_row * 16 + src_y * 2`) but sprites occupy 32
  *consecutive* rows from an even start (`mcr2.rs:602`), so sprite graphics
  resolve at the full interlaced line rate. Rows therefore belong to alternating
  fields, and a per-row palette has to know which field a row is in.
* That leaves two unknowns: where the 240 visible lines sit inside each 256-line
  field, and which field the even rows belong to. Both are constants that would
  have to be read off the vertical counter's blanking decode.

**The decode is on no drawing, and cannot be.** Tron's manual supplies the 90010
Super CPU Board schematic that Satan's Hollow's lacks, and it settles the
question in the least convenient way: MCR II's counters are inside two Midway
custom LSIs. `G12 MMC02` emits `DV0..DV8` and `VBLNK` from one package; `B12
MMC03` does the horizontal side and `HBLNK`. The comparison that decides where
blanking starts is inside MMC02. There is no decode to read and no gate to
trace. Transcribed, with everything else that was searched and found clean, in
[`../schematics/mcr-video-timing.md`](../schematics/mcr-video-timing.md).

MAME does not know it either: `mcr.cpp` configures the screen with
`set_vblank_time(ATTOSECONDS_IN_USEC(2500))` and the comment `/* not accurate */`.

Guessing either constant would put the split at the wrong rows, and on an
interlaced board a wrong guess does not blur the result, it combs it. So the
mcr2 half needs a source that is not a drawing: a logic capture on a live board
probing `VBLNK` against the vertical counter, a teardown of MMC02, or someone
else's published measurement of the blanking window. It is filed with that
unblock condition as `phosphor-emulator-raster-sampling-6kae.6` and set
**deferred** rather than left open, because nothing in this repository moves it.
The board keeps its whole-frame palette and now says so, at `mcr2.rs`'s
`render_frame`.

---

## W2 — Burgertime background scroll during active display

**Done, 2026-08-28.** The confirm-first step came back the opposite way to the
one this section was hedging against: Burger Time *does* enable the layer, the
effect is real and visible, and it is the enable bit rather than the scroll.

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

### What the confirm step actually found

Measured with `disasm trace --watch 0:0x4004:w`, over the attract loop and again
through a coined, started run so the finding does not rest on the attract-only
scope condition this epic exists to distrust:

* **The layer is enabled.** The register takes exactly two values on this ROM
  set, `$00` and `$13`. `$13` has bit 4 set, so `draw_background` runs.
* **Only bit 4 ever changes.** Bits 0–1 are `3` whenever the layer is on and bit
  2 is always `0`, so the mid-screen change is the background *enable*, not the
  scroll this section is named for. That matters because the enable also selects
  whether the chars are drawn transparently over the background or opaquely over
  the backdrop, so the split is a two-layer change rather than a shifted
  backdrop.
* **The writes land in active display.** Attract: scanlines 88, 91 and 201.
  Play: 45, 91 and 201. The visible window is 8..248, so all of them are inside
  it.
* **Value *changes* mid-screen are rarer than writes.** Most writes rewrite the
  value already there. Over 2400 frames of coined play there were 5 changes, all
  at scanline 91 or 201, roughly one frame in 480.

### What shipped

`btime` gained the same scanline hook `gottlieb` did (`begin_scanline`,
`run_scanlines`, a scanline-outer `run_frame`, and the boundary test in `tick`
for the debugger), sampling `bnj_scroll0` per visible row.

`render_visible` then composites in **bands of constant `bnj_scroll0`** rather
than consuming a per-row scroll inside the blitter, because this register picks
which layers are drawn rather than just where. A frame with no mid-screen write
is one band and one composite, exactly as before; a frame with a write is one
composite per value, and there have never been more than two. So the common case
costs nothing and only the rare split frame pays.

**Verified by eye**, since a raster split is not a thing a hash can judge: frame
523 of the attract loop renders with the left third of the screen lacking the
ladder background and the rest carrying it, against frame 522 with none and 524
with all of it. Note the split reads as a *column* boundary, not a row one: the
cabinet is ROT270, so native row 84 of 240 becomes display column 84.

Golden frames byte-identical (the pinned frame is not one of the split ones),
boot, save-state and audio all pass, and the same phase caveat as W1 applies:
chars and sprites still come from end-of-frame video RAM while `bnj_scroll0` now
comes from each row's own moment.

---

## W3 — Latched-vs-live determination for the sprite layer

**Done, 2026-08-28. Research, not code.**

### The answer

**No board in the registry latches its sprite list at vblank.** Every sprite
circuit read for this item walks its object list once per scanline, in step with
the horizontal counter, and lands the result in a line buffer that is displayed
on the next line. Per-scanline sprite rendering from live RAM is correct on all
of them, so the risk W4 was quarantined behind does not exist.

The premise this epic was written on is wrong, and it is worth saying where it
came from. `atari_system1` was taken as the in-tree existence proof of a
vblank-latched sprite layer, on the strength of its `mo_shadow` field. Its
hardware is a double-buffered horizontal line buffer, `mo_shadow` is a
whole-frame-render compensation rather than a hardware model, and the board's
own source comment says so. See "The one board that is wrong today".

### Method

Schematics first, reference driver second. The decisive question turned out to
be the same on every board and answerable from one part of the drawing: **what
drives the object list RAM's address on the video side?** On all nine boards
read it is the horizontal counter, through a mux against the CPU bus. A board
that latched would instead show a DMA engine, a transfer counter driven by the
vertical timing, or a register that triggers a copy. None appears anywhere.

Transcriptions, provenance, parts and net tables, and a per-board "what this
does NOT establish", are in
[`../schematics/sprite-list-scan.md`](../schematics/sprite-list-scan.md).

### Result

| Board | Machines | What drives the list address | Buffer | Verdict |
|---|---|---|---|---|
| `foodf` | foodf | `2H,8H,16H,32H,64H,128H,256H` via 6H/6J LS157 | odd/even, `1VX` | **Live**, 1 line ahead |
| `namco_galaga` | galaga | Namco 04XX, clocked by `1H`/`2H`/`HSYNC`, fed `MATCH` | not located | **Live**, 1 line ahead |
| `btime` | burgertime | `4H..80H`+`4V..80V` via LS153, shared with the tilemap | 93425 x3 | **Live**, per line |
| `mrdo` | mrdo | LS393 counter off `HA`, via A5/B5/C5/D5 LS153 | 6148 pairs | **Live**, per line |
| `mcr2` | shollow | `H3..H8` + `DV0..DV2` via C7/M7/N7 LS157 | 512x4 x2, swapped on `DV0` | **Live**, ≤8-line lag |
| `gottlieb` | qbert | `FORA0..FORA5` against `VV0..VV7`, per line | "line object select RAM" | **Live**, per line |
| `atari_system1` | marble, roadrunner | not traced; implied by the line buffer | 2149-2 x4, `ACS`/`BCS` | **Live**, 1 line ahead |
| `namco_pac` | pacman, mspacman | 3F/3H position RAM into the 2F adder | not read | **Live**, per line |
| `mario_bros` | mariobros | `HPO0..HPO7` into the 5M/4M counter | not read | **Live**, per line |

`mcr2` is the only one that is not a plain per-line scan. Its CPU-visible sprite
RAM (the drawing calls it the *staging RAM*) is copied into a second RAM (the
*object RAM*) at 64 bytes per scanline, continuously, so the whole 512-byte list
refreshes every eight lines; the object RAM is then walked per line into the
line buffers. Still live, with a bounded lag, and still nothing resembling a
vblank capture.

### Not read

* **digdug, xevious.** Same board family as galaga, and MAME treats their sprite
  RAM identically (Dig Dug carries the same "buffered and delayed by one
  scanline" comment). Family resemblance, not a transcription.
* **tkg04 (dkong, dkongjr).** Nintendo, same family as `mario_bros`. Note that
  MAME documents an 8257 DMA copying sprite data from 0x6900 to the sprite banks
  at 0x7400: that is a CPU-commanded copy *into* the list, the same category as
  System 1's bank swap, not the video circuit latching what it reads.
* **galaxian_video, congo_bongo, docastle, ccastles.** Not read, and none of
  them is blocking anything: they already render per scanline.
* **williams, gridlee, missile, irobot.** No sprite circuit to ask about: these
  are bitmap or framebuffer machines and the CPU draws into memory the beam
  scans.
* **The vector machines.** No scanline hardware.

### The one board that was wrong

> **Closed 2026-08-29 by W4's migration of `atari_system1`.** `mo_shadow` is
> gone, sprites are read live per row, and `phosphor-emulator-x7rn` went with
> it. Kept because the reasoning below is what set that work up, and because the
> retraction in the middle of it still stands. What the last paragraph asked for
> — the measurement — is in "What shipped, on atari_system1" under W4.

`atari_system1` is the only board in the tree that models a vblank latch, and it
is the board whose hardware most clearly does not have one. SP-277 sheet 9A is
titled, on the drawing, "Motion Object Horizontal Line Buffer", and sheet 8B
carries "Motion Object Horizontal Line Buffer Control". Two 1K x 8 2149-2 pairs
with separate chip selects, load lines, clear lines and address counters: one
written while the other is displayed.

Read the field's own comment (`atari_system1.rs:535`) and it does not actually
claim a hardware latch:

> Both games double-buffer the display list — they rebuild it during vblank and
> publish it with a bank swap — so the live sprite RAM at the frame boundary
> already holds the *next* scanout's list.

That is a description of what the **software** does, and the snapshot exists
because we render the whole frame at the frame boundary, which is at the *end*
of vblank, after the game has already rebuilt the list. For a whole-frame
renderer the snapshot is right and removing it would be a regression. The epic's
prose promoted it into a claim about the circuit, and that is what needs
retracting.

What the snapshot cannot represent is a mid-frame write to the **active** MO
bank, which changes what the beam draws from the next line on. MAME carries
`update_partial(m_screen->vpos() + 2)` in `spriteram_w` for exactly this, gated
on the write landing in the active bank, with the comment "Road Runner needs
this to work". That is a second emulator's author saying Road Runner does it;
**nobody has measured it here**, and the golden frame may not move at all. Filed
as `phosphor-emulator-x7rn`, with the measurement as its first step. It is a
W4-shaped fix, not a W3 one, and it is not a reason to delete `mo_shadow` before
the renderer can do without it.

### What this changes downstream

* W4's sprite-layer risk is closed. Sprites may be rendered per scanline from
  live RAM on every board here.
* The correct sampling point is **the previous scanline**, not the current one.
  A per-scanline sprite renderer that samples line N's state for line N is one
  line early: a much smaller error than a whole-frame render, but still not the
  hardware. Note that the one-line delay is already baked into some of our
  sprite Y constants the way it is in MAME's (galaga's `sy = 256 - y + 1`), so a
  W4 migration that adds the delay without removing the constant would double
  it. Check the constant on each board as it moves.
* `mcr2` needs its ≤8-line staging lag stated in the source if anyone ever
  renders its sprites per scanline. W4 already excludes mcr2 for other reasons.

---

## W4 — Scanline-outer restructure of the tilemap layer

For the remaining Tier B machines, invert the render nesting from layer-outer to
scanline-outer. W3 found every layer to be live-read, sprites included, so this
covers tilemap, scroll, palette, bank, flip **and** the sprite layer, sampling
the object list one line ahead of the row being composited, per W3's result.

### The safety property that was claimed here, and why it is false

> **Retracted, 2026-08-29, by the first board migrated. Kept, struck, because
> two work items were planned against it.**

The original claim: the audit establishes that these machines do not write video
registers during active display, therefore per-scanline rendering must produce
~~byte-identical output on every golden-frame pin~~, and ~~any hash change is a
bug and must not be recaptured~~.

That holds for one reading of "per-scanline" and not the other, and the
distinction was never drawn:

* **(a) Sample per scanline, composite at the frame boundary.** Byte-identical
  whenever nothing changed mid-visible-period. This is what W1 and W2 shipped,
  and both were byte-identical exactly as promised. It does not scale to the
  tilemap: you cannot snapshot video RAM 240 times a frame.
* **(b) Genuinely composite row `r` when the beam is at row `r`.** This is what
  "invert the nesting" means, and it **cannot** be byte-identical. Every board
  here composited at the frame boundary, which is the *end* of vblank, so the
  presented picture contained that frame's own vblank writes. Under (b) the beam
  has already passed, so it does not. The picture goes **one vblank older**,
  which is what the beam actually drew, and every pin whose game animates during
  vblank moves.

W4 is (b). The pins are expected to move, they are reviewed by eye before
recapture, and the hash is a change-detector rather than a correctness oracle
for this work item. What replaces it as the correctness argument is below.

### What proves a moved pin right, since the hash cannot

Per board, all three:

1. **The picture, by eye**, before recapture: before/after/imgdiff.
2. **The mechanism, proven not assumed.** If the only change is the sampling
   moment, then the new frame `N` must equal the *old* frame `N-1` exactly,
   wherever the game confines its video writes to vblank. That is a decisive,
   cheap check: capture `N-1` with the old code and diff at threshold 0.
3. **The test pair** W1 and W2 established: one test that a mid-frame write
   splits the picture at a stated row, one test that the frame loop reaches the
   hook. Each made to fail once before being kept.

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

### What shipped, on gottlieb (qbert), 2026-08-29

The first of the eight, and the one that settled the fork above.

`begin_scanline` now draws the row as well as sampling the palette:
`render_scanline` clears the row, reads `video_control` bit 0 for the layer
order, and runs `render_tiles_scanline` and `render_sprites_scanline` in that
order out of live video RAM, sprite RAM and the sprite bank. The frame-boundary
render in `end_cycle` is gone; `render_frame` only resolves indices to RGB. The
phase mismatch W1 opened on this board is closed: a row's colors and its pixels
now come from the same moment.

**The pin moved, and the mechanism was proven rather than assumed.** With the
new code, frame 1800 is **byte-identical to the old code's frame 1799**: 0 of
61440 pixels differ at threshold 0. Against the old frame 1800 it differs by
150 pixels (0.24%), and every one of them is on the three animating sprites;
pyramid, title and discs are untouched. So the restructure changed the sampling
moment and nothing else, exactly as (b) predicts. `shows` needed no rewrite:
nothing it claims about the frame's content is refuted.

**Perf, from `phosphor-bench --machine qbert --frames 900 --warmup 1800
--reps 5` in release:** 2.838 ms/frame before, 2.855 after. **+0.6%**, against a
run-to-run spread of ±2.2% and ±2.9%, so it is not distinguishable from zero at
this sample size.

Note the cost model in "Testing" below does not apply to this board. Its tile
pass was *already* scanline-inner, so tile-info lookups did not increase at all.
The added work is the sprite list: 64 entries walked once per visible row rather
than once per frame, 15360 loop iterations of which almost all `continue`
immediately. A per-row candidate list would remove it and is not worth the
complexity at 0.6%; revisit if a later board shows more.

**Two things the migration turned up that the plan did not predict:**

* **Empty graphics caches now panic.** `count().max(1)` was papering over a
  zero-entry cache, harmless while nothing rendered on a bare board and an
  out-of-bounds index once every scanline draws. Both tile and sprite passes now
  return early on an empty cache. Q*Bert leaves the ROM tile cache empty in
  normal operation, so this is not only a test-only path.
* **A pre-existing sprite bug became visible.** Five sprite slots are parked at
  `(0, 0)` with live codes, and `sy - 13` / `sx - 4` put their bottom-right
  corner inside the framebuffer at native `(0..5, 0..2)`. It is in the previous
  reference PNG too, so W4 did not cause it. Filed as `phosphor-emulator-z04w`
  rather than fixed, because the reference emulator hides it behind a constant
  its own source calls a guess, and the board's real object-enable term
  (`ENBUF`, gated with VBLANK and HBLANK) is not in the transcribed part of the
  schematic. `qbert` gained `disasm gfxview` regions along the way, which is
  what proved the all-zero slots draw a blank sprite and these five do not.

### What shipped, on btime (burgertime), 2026-08-29

The second of the eight, and the one that shows a W4 migration does not have to
move a pin.

`begin_scanline` now draws instead of sampling. `render_scanline` builds one
256-pixel native row out of the live `bnj_scroll0`, video RAM, color RAM, flip
latch and palette, then crops it to the visible `[8,248)` columns and resolves it
into the framebuffer. The band compositing W2 introduced is gone, and so is the
per-row `bnj_scroll0` sample array it fed: a row reads the register when it is
drawn. The palette came along for free, since the row resolves where it is drawn,
so a palette write partway down the screen now colors only the rows below it.

Two loops needed an index rather than a filter, or the row passes would have cost
240x the whole-frame ones:

* **Chars.** A cell's top edge is `8 * (off % 32)`, so the grid row is fixed by
  the native row and only the column varies. 32 cells per row instead of 1024.
* **Background.** Same shape at 16 pixels, via `bg_tile_row`, which inverts the
  flipped case (`240 - 16k`) as the one multiple of 16 in `[240 - y, 240 - y +
  16)`. 16 tiles per column block instead of 256.

Sprites keep the full eight-entry sweep; `blit_tile_row` rejects the ones that
are not on the line, and eight is not worth an index for.

**The pin did not move. Byte-identical at frames 1200, 1799 and 1800**, 0 of
57600 pixels each, against a control confirming the game is animating (176
pixels between old 1799 and old 1800). No recapture.

**Why it did not move, measured rather than assumed.** This board's frame
boundary is the end of scanline 271, so the old whole-frame render fired after
the *trailing* vblank; the new rows are drawn from scanline 8 onward. The two can
only differ for state written after the row that displays it. Traced with
`disasm trace --watch` on the sprite table through the X/Y-swap mirror, Burger
Time's per-frame updates land at scanlines 9, 29, 30, 31, 55 and 56, at the top
of *active display*, and every object is written before the beam reaches the row
it occupies. So the beam sees what the frame-boundary render saw.

That is a property of this game's update timing on this ROM set, not a
guarantee, and W2 already found the counterexample on the same board:
`bnj_scroll0` is written at scanlines 88, 91 and 201, which do split the picture,
on frames the golden pin does not sample. The mid-frame test still proves the
split.

**Perf:** 1.264 ms/frame before, 1.254 after. Inside the ±2.3% and ±3.7%
run-to-run spread, so no measurable change. The row passes add per-row work and
take away a 64 KB `native` allocation and a full 256x256 draw per frame, of which
only 240x240 was ever cropped out.

**The test that had to be replaced, not weakened.**
`a_load_seeds_every_row_from_the_restored_register` asserted a load seeded the
per-row `bnj_scroll0` samples. Those samples no longer exist, so the test was
replaced by one that saves a board with the background on, loads it into a board
drawn with the background off, and asserts the next scan draws the background:
the same concern (a restored register reaches the picture with no stale per-row
state between) expressed against the structure that now exists. The board's
`save_after_load` hook is gone with it.

`begin_scanline` also became `pub`: the picture only exists once the beam has
passed, so an integration test that wants a frame without running CPU cycles has
to step the beam itself.

### What shipped, on namco_galaga (galaga), 2026-08-29

The third of the eight, and the one that had to answer this doc's own
no-vblank-regression clause.

The board already had the house hook (`begin_scanline`, `run_scanlines`, the
boundary test in `tick`); what was whole-frame was the render, which lives on
each game wrapper rather than the board. `GalagaSystem` gained
`begin_scanline_render`, called from the frame loop and from
`tick_frame_boundary` for the debugger, and `render_scanline` runs backdrop,
starfield, sprites and tilemap for one row. `run_frame` is scanline-outer and
re-forms the CPU/bus split 264 times a frame instead of once; a per-*cycle*
split cost about 6% on this board when it was measured, and this is 1/192nd of
that frequency.

**`update_starfield_at_vblank` was an empty function.** This doc cited it as the
latch-modelling reference and the work item's acceptance criteria named it as
one of the two things that must not become per-line. Its body was four lines of
comment saying nothing further was needed. There was no latch to preserve, so it
is deleted, and what replaces it is a real one.

**The real once-a-frame state is `star_frame`.** The starfield is a free-running
shift register whose output position is a function of how many times it has been
clocked since the frame began: `pre_vis` (which the scroll index perturbs by -4
to +3) then 224 rows of 256 clocks, then `post_vis`. Reading the scroll index
per row would not recolour a row, it would move every star below it. Whether the
05XX re-reads its control latch per line is on no drawing, because the Galaga
video sheet names 4M as the starfield generator and it is a Namco custom LSI:
the same dead end MMC02 is for `mcr2`. So the four control bits are latched at
row 0, exactly reproducing the whole-frame semantics including the register not
advancing at all on a frame where the field is disabled, and the question is
left open rather than guessed at. Rule: can I point at the part? Not here.

**The pin moved by exactly one frame, proven.** New frame 1800 is byte-identical
to the old code's frame 1799 (0 of 64512 pixels), and differs from old 1800 by
217 pixels, which are *the same 217* that separate old 1799 from old 1800. Most
of them are stars: the field scrolls every frame, so a one-frame shift moves all
of it. The LFSR sequence survived the split into pre/row/post exactly, which was
the part most able to go wrong. Recaptured after review; `shows` needed no
rewrite.

**Perf, and the first board where the cost is visible at all.** 2.266 ms/frame
before, 2.289 after: **+1.0%**, against a ±1.8% and ±1.9% spread at nine
repetitions. The first attempt measured +2.8%, because the 64 sprite slots were
being decoded 224 times each to draw at most 32 rows of any one of them. A
Y-range early-out on the slot, taken after the attribute bytes are read so the
per-row sampling is unchanged, brought it to +1.0%. Worth carrying to the
remaining boards: the row passes want an index or an early-out wherever a layer
iterates a list.

Note the sprite Y already carries W3's one-line line-buffer delay as `256 - y +
1`, so no sampling lead was added on top of it.

### What shipped, on namco_galaga (digdug), 2026-08-29

The fourth, and the first one that was genuinely mechanical: the same
`begin_scanline_render` / `render_scanline` shape as galaga, deliberately
written to match line for line so the three can be lifted onto the board later.

Background, foreground and sprites all became row passes; the sprite slot got
galaga's Y-range early-out; both tilemap passes got the empty-cache guard. There
is no backdrop fill, because Dig Dug's background tilemap is opaque and covers
every pixel of the row. No starfield here, so no once-a-frame state at all: every
layer on this machine is read live per row.

**The pin moved by exactly one frame, proven.** New frame 1800 is byte-identical
to the old code's frame 1799 (0 of 64512 pixels), and the 619 pixels that differ
from old 1800 are *the same 619* that separate old 1799 from old 1800. By eye
they are the flashing namco logo and the animating characters. Recaptured;
`shows` needed no rewrite.

**Perf:** 3.169 ms/frame before, 3.196 after, **+0.9%**, against ±2.8% and ±3.5%
at nine repetitions. Consistent with galaga's +1.0%, and inside the noise here.

One trap worth recording for the next board, hit while writing the test: on this
machine the playfield byte is *both* the tile code and, in its high nibble, the
colour, so a fixture that changes colour also changes which tile is fetched. The
first version of the test allocated a one-entry cache and indexed out of bounds
the moment the colour changed.

### What shipped, on namco_galaga (xevious), 2026-08-29

The fifth, and the first board whose pinned frame shows a **real mid-frame
split** rather than a one-frame phase shift. It is also the first with a
measurable perf cost.

Same shape as galaga and digdug. This board is the first in the epic with real
scroll registers, and they are now read per row, so a scroll write partway down
the screen splits the layer there.

**The pin is a third case.** Neither byte-identical nor one frame older:

| band | vs old 1799 | vs old 1800 |
|---|---|---|
| native rows 0-7 | **0 / 2304** | 784 / 2304 |
| native rows 8-223 | 19284 / 62208 | **536 / 62208** |

The top 8 rows carry the *previous* frame's scroll; everything below is current.
Traced with `disasm trace --events devwrite`: frame 1800 writes `$D001` (bg
scroll X) and `$D020` (bg scroll Y) at **scanline 8**, inside active display, so
the beam has already painted rows 0-7. The residual 536 in the lower band is the
sprites, whose registers are written at **scanline 231**, in vblank, so they are
one frame older like galaga's. Both halves are what the beam sees, and together
they account for the whole difference.

**The phase was checked against the reference before believing the seam.** Our
model puts the visible window at scanlines 0..223; the reference's namco screen
configs agree for galaga, xevious and digdug (`set_raw(..., 264, 0, 224)`). The
one config in that file using `264, 16, 224+16` is Bosco, which is not in this
registry. So scanline 8 really is eight lines into active display and the seam is
hardware behaviour, not a phase error on our side.

**Perf: +6%, the largest of the epic**, and the measurement is the weakest.
Under a quiet host the pre-migration baseline was 2.381 ms/frame (±0.7%) and the
first migrated version 2.704 (±4.8%), so +13.6%. Two output-neutral fixes brought
that down, each verified to move zero pixels:

* a shared `prio_scratch`, because both gfx helpers document `prio_buf` as
  write-only and the per-call `[0u8; 288]` went from once per layer per frame to
  once per layer *per row*, plus once per sprite tile per row;
* a single reach-pass over the 64 sprite slots' Y registers, because the loop was
  re-taking three region borrows per slot per row: 43,000 map lookups a frame
  against 192.

A back-to-back A/B afterwards measured 2.528 against 2.688, about **+6%**, but
the host had become noisy (±16%) and those numbers are not comparable with the
quiet baseline. `region_data` is O(1) and the scrolled tilemap helper has no
amortizable per-row setup, so the remainder is not yet attributed; **a profile,
not more guessing, is the next step** if it matters. Two scrolled layers rather
than one is the obvious suspect.

### The namco_galaga row drive, lifted onto the board (W6, 2026-08-29)

With all three games per-scanline, `begin_scanline_render`, `tick_frame_boundary`
and the scanline-outer `run_frame` were identical in `galaga.rs`, `digdug.rs` and
`xevious.rs` down to the comments. They are now provided methods on
`namco_galaga::ScanlineGame`, and each game supplies only `render_scanline`,
`split` and `board`.

The obstacle was that the renderer lives on the game wrapper while the clock and
the tick loop live on the board, and `run_cycles` is generic over the *bus view*
rather than the wrapper, so the board could not reach a game's renderer. A
generic associated type (`type Bus<'a>: NamcoGalagaBus`) closes that: each game
names its own bus view, which is why the three bus structs became `pub(crate)`
and the trait itself is `pub(crate)` rather than `pub`.

`render_scanline` stays per game and should: the layer sets and orders genuinely
differ (galaga is backdrop, starfield, sprites, tilemap; digdug is opaque
background, foreground, sprites, with no backdrop fill; xevious is background,
sprites, foreground, with two scrolled layers).

**Verified as a pure refactor.** All three golden pins byte-identical, nothing
recaptured. The frame-loop half of each game's W4 test pair still fails when the
now-*shared* drive is made to draw every row, so one mutation falsifies all three
at once, which is the evidence that the lifted code is the code under test.

**Perf: no measurable change, and the measurement is the story.** A back-to-back
A/B put galaga at +7.3% and digdug at +7.4%, consistent enough to look real. A
tighter A/B on galaga (21 repetitions instead of 9) put the same comparison at
**-2.8%**. Two pairs disagreeing in sign means the host cannot resolve a
difference of that size, not that the refactor is free and not that it costs 7%;
the honest reading is that nothing was measured. The generated code is
monomorphized per game and the pins are byte-identical, which is the stronger
argument here anyway.

Three machines remain: mrdo, foodf, marble, plus the boards W5 turned up.

### What shipped, on mrdo, 2026-08-29

The sixth, and the first of the two boards this doc warned would "need more
care" for being per-pixel whole-frame loops rather than layer passes. It needed
less than the warning suggested: because the loop was already per-pixel, its
tile lookups did not multiply when the nesting inverted, and the restructure was
the same shape as the others.

The board gained the house hook it did not have — `tick` tests the scanline
boundary, `run_scanlines` hoists that test out, `run_frame` is scanline-outer,
and `begin_scanline` draws. `render_scanline` builds one 240-pixel native row of
pen indices out of the live BG and FG video RAM, sprite RAM, the two BG scroll
registers and the flip latch, then resolves it against the palette into a
persistent framebuffer. `render_frame` only copies. The `vec![0u16; 240*192]`
the whole-frame render allocated each time is gone, replaced by a `[u16; 240]`
per row.

`VBLANK_IRQ_LINE` is now derived as `VISIBLE_TOP_LINE + VISIBLE_HEIGHT` rather
than written as 224. Same value, but the relation it encodes — the IRQ fires on
the first line past the visible window — is what `begin_scanline`'s guard also
depends on, and the two should not be able to drift apart.

**The pin moved by exactly one frame, proven.** New frame 1800 is byte-identical
to the old code's frame 1799 (0 of 46080 pixels), and differs from old 1800 by
73 pixels, which are *the same 73* that separate old 1799 from old 1800. By eye
they are Mr. Do himself, one pixel further along beside the 1000-point bonus;
title, INSTRUCTION, the cherry field and the score line are untouched.
Recaptured, one hash in `frames.toml`, one of 39 machines moved, and `shows`
needed no rewrite.

**Why it moved by exactly one frame, traced not assumed.** Mr. Do! takes one
VBLANK IRQ at scanline 224 and does its whole per-frame video update inside the
first two lines of vertical blanking: `disasm trace --watch` puts frame 1800's
writes at `$9000` on **scanline 224** and `$900C` on **scanline 225**, sprite
slots 0..3 only, with no write to either tilemap, either scroll register or the
flip latch anywhere in frames 1799 or 1800. The last visible row is drawn at
scanline 223, so the beam had passed before the first write landed. That is the
whole 73-pixel difference and there is nothing left over.

**The watchpoint blind spot did *not* bite here, and it was checked rather than
assumed.** The scroll registers at `$F000`/`$F800` are decoded in this board's
`Bus` impl and belong to no mapped region, which is the shape that returned
silence on xevious (`phosphor-emulator-gcny`). Here it does not: `watch_write`
is called on the raw address before the decode, so the probe sees them. The
positive control proves it — over frames 0..200 the trace shows `$F800` written
once a frame from `pc=701E`. So the empty result over frames 1700..1801 is a
measurement and not a blind spot.

**Mr. Do! does write its scroll during active display**, just not on the pinned
frame: the same trace puts a `$F800` write at **scanline 145** of frame 3, well
inside the visible window. Like Burger Time, this board's byte-for-byte agreement
is a property of what the attract loop happens to be doing at frame 1800, not a
property of the migration.

**No sprite sampling lead was added, and the constant does not carry one.**
Galaga folds W3's one-line line-buffer delay into `256 - y + 1`; this board's
`256 - rawY` does not. The object sheet shows two pairs of 6148s on the output
path, which is a line buffer's shape, but which pair buffers and how the two
alternate was read as chips rather than as a traced path
(`docs/schematics/sprite-list-scan.md`). Adding a lead would move every sprite
pixel on the strength of a guess, so the constant is left alone and the question
stays open. Can I point at the part? Not for the alternation.

**The empty-cache guard moved rather than multiplied.** The whole-frame render
already had one; it is now at the top of `render_scanline`, where a bare board
reaches it 192 times a frame instead of once. It is load-bearing, not defensive:
`boots_and_runs_frames_without_panicking` (and the registry-wide
`machine_contract_test`, which runs frames on ROM-less machines) fails with
`index out of bounds: the len is 0` when it is removed. A separate guard on the
sprite pass was written and then deleted: with an empty cache the sprite RAM is
also all zeros, every slot's Y byte is 0 and the pass returns before it can
index anything, so nothing could ever trip it. A check that cannot fail is not a
check.

**Perf: a small real cost whose size the host could not resolve.** Two
back-to-back A/B pairs, `--frames 900 --warmup 1800`: at 9 repetitions 1.237 →
1.292 ms/frame (**+4.4%**, spreads ±4.3% and ±10.1%), at 15 repetitions 1.246 →
1.267 (**+1.7%**, spreads ±15.5% and ±11.8%). Both pairs positive, so unlike the
W6 measurement this is not "nothing was measured" — but a 2.6x disagreement in
magnitude against spreads that wide means the number is bounded, not known. The
internal split moves as expected and is the clearer signal: render drops from
0.215 to 0.005 ms/frame (it is a `memcpy` now) while emulation rises from 1.03
to 1.27, because the drawing moved inside `run_frame`.

Not attributed, and worth saying where it is *not*: the per-row sprite sweep is
64 slots x 192 rows = 12,288 Y-byte reads a frame, against the two tilemaps'
92,160 per-pixel map fetches — and that second figure did not change in the
migration, because the whole-frame loop was per-pixel too. Filed as
`phosphor-emulator-3me5`, raised by the owner from the sprite side and widened
to both layers: a row pass should iterate the units that vary along the row,
which for a tilemap is 32 tiles rather than 240 pixels and for sprites is the
candidates rather than the slots. A profile comes before choosing between them.

### Row passes iterate tiles, not pixels (mrdo, 2026-08-29)

Raised by the owner from the sprite side immediately after the migration above,
then widened by them to the tilemaps, which turned out to be where the cost
was. Filed as `phosphor-emulator-3me5`; the tilemap half is done on mrdo and the
sprite half and the other boards are not.

`draw_tilemaps_row` now caches the map fetch and the decoded 8-pixel tile line
until the column index moves, and hoists what the row fixes. `by` depends only
on `ey` and the Y scroll, so `(by / 8) * 32` and `by & 7` never varied along the
row at all; the attribute byte, the code byte and the tile's line
(`GfxCache::row_slice`) now come once per tile. Two layers x 240 pixels x 192
rows = 92,160 map fetches a frame becomes about 11,900.

**Cached on the column index, not on a span**, which is the part worth copying.
Span arithmetic would have to special-case the flipscreen mirror
(`ex = 255 - ax`, so the source walks *backwards* along the row) and the
scrolled background's wrap at 256. Comparing this pixel's column index against
the previous one is correct in both, with neither case appearing in the code.

**Output-neutral, verified**: 0 of 46080 pixels against the pin recaptured
minutes earlier, and the golden suite passes with no recapture.

The reversed walk is what a cache like this is most able to get wrong, and
nothing covered it — before the optimization either.
`flipscreen_mirrors_the_tilemaps_across_both_axes` closes that: the visible
window is symmetric under the 255-complement (x 8..248 maps onto itself, and so
does y 32..224), so the flipped picture must be the *exact* 180° rotation of the
unflipped one, asserted pixel by pixel over the whole frame. Sprites are
excluded because the hardware does not flip them. Falsified by dropping the flip
from the column sampling.

**Perf: -4.2% and -2.4%** on two back-to-back A/B pairs at 15 and 21 repetitions
(1.296 → 1.241 and 1.242 → 1.207 ms/frame), host spreads ±6.7% to ±59.8%. Both
pairs negative, so a real improvement whose magnitude the host cannot resolve —
the same position the migration's own +1.7%/+4.4% was left in, and the two
nearly cancel: against the whole-frame renderer this board started the day with,
per-scanline rendering plus this costs about nothing.

That cancellation is not attribution. It is consistent with the per-pixel map
fetch being the dominant cost, which is what the 92,160-against-12,288 count
predicted, but a profile is still what would prove it and still has not been
run. The rule went into `machines/CLAUDE.md`: a row pass iterates the units that
vary along the row, and must not precompute a per-frame index of live state,
because that is a latch and it reintroduces what per-scanline rendering exists
to remove.

### What shipped, on foodf, 2026-08-29

The seventh, the second of the two "per-pixel whole-frame loop" boards, and the
first in the epic that is **faster** after the migration rather than slower.

Written row-outer *and* tile-outer from the start, per the rule the previous
entry added, so this one commit carries both changes and its number is their
sum. mrdo's separate measurements are the reference for how the two decompose.

`begin_scanline` now does both kinds of scanline-boundary work: the raster
interrupt latches, which used to re-test the frame position inside `begin_cycle`
on every cycle, and the row. `render_scanline` composites the playfield and then
the sprites into one 256-pixel row and resolves it into a persistent
framebuffer; `render` only copies. `run_scanlines` hoists the boundary test and
`run_frame` is scanline-outer.

Three whole-frame scratch structures are gone. The old render allocated
`vec![0u8; 256*224]` for the playfield pens and `vec![false; 256*224]` for the
sprite priority mask on **every frame** — 114 KB of allocation and zeroing —
plus it fetched the playfield map word once per pixel. All three are per row
now: two 256-byte stack arrays and 32 map fetches a row instead of 256.

**The `claimed` mask is per row, and that is exact rather than an
approximation.** Food Fight's sprites use first-opaque-pixel-wins priority, so
the mask matters; but a claim is indexed by pixel and every pixel a sprite
writes on a line belongs to that line, so claims never cross rows. The sprite
pass also inverts the placement instead of walking all 16 of a sprite's rows and
discarding 15: `dy = (ypos + row) & 0xFF` has exactly one solution for `row`, so
asking which row lands on this scanline is one modular subtraction.

**The pin did not move — and the usual check could not have told us that.**
Old frame 1799 and old frame 1800 are byte-identical, so "new N equals old N-1"
and "new N equals old N" say the same thing at the pin and neither would mean
anything. Mapping the cadence first is what made the result readable: old 1798
against old 1799 differs by 1131 pixels, so **frame 1799 is the discriminator**,
and there:

| comparison | result |
|---|---|
| new 1799 vs old 1798 | **0 / 57344** |
| new 1799 vs old 1799 | 1131 / 57344 |
| new 1800 vs old 1800 | 0 / 57344 |

Exactly one frame older, proven against a 1131-pixel control, and the pin
happens to sit inside a static pair so it does not move. No recapture.

**Why, traced not assumed.** On frame 1799 the object list is written at
scanlines 224 and 228 and the palette at 228 — all inside vertical blanking,
which starts at 224 — so the beam had already passed. The playfield is not
written at all in frames 1798-1800, and the probe for it was verified to fire
(it does, during the RAM self-test at frames 17-19) before that emptiness was
read as a measurement.

**But this board does write a live-read video register during active display**,
which is worth knowing even though it does not split the picture here.
`digital_w` sets the flip latch on every call, and the IRQ1 handler calls it
from inside the visible window — measured at scanlines 10, 53, 116 and 181 of
frame 1799. It writes bit 0 clear every time, so the flip never changes and no
seam appears. This is the board in the epic most built to be reprogrammed
mid-frame (IRQ1 fires at scanlines 32, 96 and 160), and it declines to.

**Perf: -11.3% and -11.1%** on two back-to-back A/B pairs at 15 and 21
repetitions (1.833 → 1.625 and 1.844 → 1.639 ms/frame). The first well-resolved
number in the epic: both pairs agree in sign *and* magnitude, and the gap is far
outside the 0.6% wobble between the two "before" runs. Render falls from 0.309
to 0.008 ms/frame and emulation rises 1.52 to 1.62.

Attributed as far as the counts allow: the two per-frame heap allocations and
the per-pixel map fetch are the removals, and they are large. The sprite pass is
not a win and may be a small loss — it gains a per-row candidate test (48 slots
x 224 rows) that the whole-frame version did not need, and pays it back only on
sprites parked off-screen, which the old code decoded in full.

Two existing tests moved rather than weakened: both drove `begin_cycle` to
observe the 32V interrupt latch, and now drive `begin_scanline`, where it lives.
The frame-loop test picks up what that gave away, asserting the real loop
reaches scanline 32 and raises the request.

Two machines remain: marble, plus the boards W5 turned up.

### What shipped, on atari_system1 (marble and roadrunner), 2026-08-29

The eighth and last of this work item's own list, both machines on the board at
once, and the one where `mo_shadow` retires. It is also the board that showed
the epic's model was still incomplete.

**A fourth case: one layer moves and the other does not.** The pins are neither
byte-identical, nor one frame older, nor split by a mid-frame write. Marble's
pin is byte-identical and Road Runner's moves by 14612 pixels spread over all
240 rows, and the *same* change produced both.

The reason is that the old renderer sampled **two different moments in one
picture**:

* motion objects came from `mo_shadow`, snapshotted at the **start** of vblank,
  which is the list the beam had just scanned out — correct for the frame;
* everything else — playfield, scroll, alpha, palette — was read live at the
  **frame boundary**, the *end* of vblank, by which point the game had already
  published the next frame's values.

Per-scanline rendering puts every layer at the beam, so the sprite layer does
not move at all and the playfield moves back by one vblank wherever the game
scrolls. Marble's attract playfield does not scroll, so nothing moves; Road
Runner's does.

**Measured, and the mechanism traced rather than inferred.** Road Runner writes
its X scroll once a frame at **scanline 241, inside vblank**, stepping
`0x11D → 0x11A → 0x117` — three pixels a frame. The beam of frame 1800 reads
what vblank 1799 published, `0x11A`; the old render read it after vblank 1800
published `0x117`. A three-pixel shift of the playfield is the whole 14612-pixel
difference, and by eye the two pictures are the same scene with the canyon and
road three pixels apart, the alpha text and the character unmoved.

| comparison | marble | roadrunner |
|---|---|---|
| new 1800 vs old 1800 | **0 / 80640** | 14612, all 240 rows |
| new 1800 vs old 1799 | 467, in three 17-row bands | 207, rows 96-125 only |
| control: old 1799 vs old 1800 | 467 | 14798 |

The two residuals against `old 1799` are the sprite layer being one publish
newer than *that* frame's snapshot, which is right: marble's three 17-row bands
are its three animating objects, and Road Runner's rows 96-125 is its character.
Every band is accounted for.

**`mo_shadow` is gone, and so is the machinery around it**:
`snapshot_motion_objects`, `mo_shadow_bands`, the `mo_bank_changes` band log,
and the per-band compositing loop that replayed them. A row reads the live
sprite RAM and the live bank when it is drawn. The one-line delay the band log
applied by hand (`line + 1`) now falls out of the structure, because a row is
drawn at the *start* of its scanline and a write during scanline N is first seen
by row N+1.

**`phosphor-emulator-x7rn` is fixed by the same change.** A write to the active
motion-object bank during active display now changes the rows below it, which no
whole-frame render could show. That issue predicted this exactly: "once the
board renders sprites per scanline from live RAM, the snapshot and this bug both
go away together."

**The ratchet in `roadrunner_video_timing_test.rs` fired as designed.** Two tests
written in advance against a conformance ROM asserted the *defect* and carried
the correct answer in their failure messages. They came back with "121 rows kept
the red they were drawn with and 119 came out green" and "screen row 50 is RED"
— the two stated correct answers — and are now rewritten to assert the split
positively, so they discriminate in the other direction. That is an independent
confirmation of this work from a source that knew nothing about how it would be
implemented.

**Perf: about -5% on marble and -3 to -4% on Road Runner**, on two back-to-back
A/B pairs (marble 3.265 → 3.092 and 3.225 → 3.053; roadrunner 3.456 → 3.305 and
3.484 → 3.368 ms/frame), the second pair on a quiet host at ±0.7% to ±4.4%.
Marble's -5.3% is identical across both pairs. Render collapses from 0.69 to
0.013 ms/frame and emulation rises from 2.55 to 3.05.

Three per-frame allocations went away — a 161 KB index buffer, a 161 KB motion
bitmap and the `mo_shadow` copy of the whole MO region — along with the
1024-entry palette decode (now a one-entry memo on the palette index) and the
per-pixel playfield and alpha cell fetches, which became per tile. Against that,
the motion-object list is a linked chain and cannot be indexed, so it is walked
once per row instead of once per band; the early-out reads `word[0]`, which
carries both Y and height, and decides the row before touching the rest.

No sprite sampling lead was added. W3 established from the SP-277 sheets that
the object path is a doubled horizontal line buffer, so the list for row `r` is
read while the beam is on `r - 1`; but the `ypos` expression carries no such
term, the sheets do not establish the buffers' phase, and the reference driver's
own `+2` is documented there as a kludge over the `+1` it calls correct.

---

## Testing

```bash
cargo test -p phosphor-machines
cargo test -p phosphor-harness --test golden_frame_test   # the gate
cargo clippy --all-features --all-targets
cargo run --release -p phosphor-bench -- --roms <path>    # before/after, W4 only
```

* **Golden frames are the primary gate and must not be recaptured** for W1 and
  W2. Both were expected to be byte-identical on the pinned frames and both
  were; a diff there means the migration changed behavior on a frame where the
  hardware did not.
* **W4 is the exception, and it is not a loophole.** Its pins are expected to
  move by exactly one frame and no more. Recapture only after the three checks
  in "What proves a moved pin right" have all passed, and say in the commit
  which frame the new picture equals. A pin that moves by something other than
  one frame of animation is a bug, not a recapture.
* **Per-work-item targeted tests** proving the mid-frame case: write the
  register at a known active-display scanline on a synthetic or ROM-booted
  board, assert only rows below the write differ. Without this, a migration can
  be byte-identical *and* still not honour mid-frame changes — the golden
  frames alone cannot distinguish the two.
* **No vblank regression:** assert genuinely once-a-frame state is not
  re-sampled per line after any W4 migration. On galaga that is `star_frame`,
  the starfield control latch, and the test is
  `the_starfield_controls_are_not_resampled_per_line`. It is *not*
  `update_starfield_at_vblank`, which this doc named until the galaga migration
  found it was an empty function; see that section.
  `atari_system1`'s `mo_shadow` is *not* such a case either, since W3 found
  the hardware reads live, but it must not be re-sampled per line either while
  the board still renders whole-frame, because the game rebuilds the list during
  vblank. Retire it as part of the board's W4 migration, not before.
* **Perf:** W1 and W2 need no measurement. W4 changes tile-info lookups from
  per-tile to per-tile-per-scanline (8× for 8-pixel-tall tiles — on a 36×28
  tilemap, 1008 → 8064 lookups per frame). Expected negligible; confirm with
  `phosphor-bench` in release, per `CLAUDE.md`.

## Risks

1. ~~**W4 introduces tearing on latched sprite layers.**~~ **Closed by W3.**
   There are no latched sprite layers in the registry. The residual risk is much
   smaller and has a different shape: sampling the object list for line N
   *at* line N rather than at line N-1 is one line early. Nine boards were read;
   digdug, xevious and the TKG-04 boards were not, so a board outside that set
   still deserves a look before its sprites move per scanline.
2. **Byte-identical is not proof of correctness.** A migration that renders
   per-scanline but accidentally samples state once still passes the golden
   gate. Hence the mandatory targeted mid-frame test per work item. Under W4 the
   converse also bites: the gate now *always* fails on an animating pin, so it
   can no longer distinguish the intended one-frame shift from a real
   regression. That is what the equals-the-old-frame-`N-1` check is for.
3. **W2 may be a non-issue.** Guarded by the confirm-first step.
4. **Scope creep into the per-scanline machines.** The 19 already-correct boards
   are out of scope; W3's research covers them only for completeness.

## Sequencing

`W1 → W2 → W3 → W4`, and each is independently shippable. **W3 is done**; W1 and
W2 are not, and W3 did not depend on them.

W1 and W2 are small and fix things that are wrong now. W3 was cheap research
that de-risked everything after it. W4 is the bulk of the labour, and the
constraint it was waiting on is gone: no board needs its sprite layer left
whole-frame on latch grounds.

A reasonable stopping point is still after W3, now that W3 has landed: the two
known-wrong cases fixed by W1 and W2, the read side documented, and per-scanline
adopted as the default for new machines, without a nine-machine restructure.
W4 is worth doing, but it is the part to defer if effort is constrained. The one
defect W3 turned up that W4 would fix (`phosphor-emulator-x7rn`, Road Runner's
mid-frame writes to the active MO bank) is filed and does not block anything.

## Standing convention (adopt regardless)

Add to `machines/CLAUDE.md`: **new raster machines render per-scanline from live
state by default**; a whole-frame render needs a comment justifying it, and any
state the hardware latches at vblank is modelled as an explicit snapshot rather
than read live. This is what stops the ratio drifting back.

W3 adds a corollary worth writing down with it: **a snapshot that exists to
compensate for whole-frame rendering is not a hardware latch, and its comment
must say which it is.** `mo_shadow` was read as the second, by this epic's own
prose, because its comment described the first without ever naming the
distinction.

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
* W3's transcriptions:
  [`../schematics/sprite-list-scan.md`](../schematics/sprite-list-scan.md).
* Once-a-frame state reference: `galaga`'s `star_frame`, the starfield control
  latch, with the reason it cannot be read per row on the field itself.
  `update_starfield_at_vblank` used to be cited here and was an empty function;
  it is deleted. `machines/src/atari_system1.rs` (`mo_shadow`) is a
  whole-frame-render compensation rather than a hardware latch; see W3.
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
