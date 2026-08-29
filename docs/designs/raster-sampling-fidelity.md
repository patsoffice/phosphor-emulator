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

### The one board that is wrong today

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
phase mismatch W1 opened on this board is closed: a row's colours and its pixels
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

Seven machines remain: galaga, digdug, xevious, mrdo, burgertime, foodf, marble.

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
  were; a diff there means the migration changed behaviour on a frame where the
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
* **No vblank regression:** assert genuinely vblank-latched state (galaga's
  `update_starfield_at_vblank`) is not re-sampled per line after any W4
  migration. `atari_system1`'s `mo_shadow` is *not* such a case, since W3 found
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
* Latch modelling reference: `machines/src/galaga.rs:828`
  (`update_starfield_at_vblank`), which is a genuine vblank latch.
  `machines/src/atari_system1.rs:547` (`mo_shadow`) is **not** one; see W3.
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
