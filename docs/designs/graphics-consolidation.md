# Design: Graphics Handling Consolidation

> **Status: mostly implemented (rev 2).** Rev 1 audited the graphics/render path
> across ~40 machines and proposed four consolidations. Features 1, 2a and 2c are
> **done**; the 2b and Feature 3 *helpers* are built and partly adopted; Feature 4
> is done bar one housing detail. Rev 2 re-measures adoption, records what landed
> (and where it diverged), retires the parts that turned out not to be worth
> doing, and makes an explicit per-machine call on the remainder instead of
> leaving a standing to-do. Rev 1's "out of scope: rotation" note is deleted —
> that work shipped.

## What this doc is now

Rev 1's thesis — the shared helpers in `core/src/gfx/` are sound but
under-adopted, and the misses are enumerable features rather than fundamental
differences — held up. The helpers were enriched exactly as specified
(`tilemap.rs:55` is rev 1's proposed signature down to the closure types) and
seven machines migrated onto them.

What rev 1 got wrong was the *size of the addressable set*. It assumed the
remaining hand-rollers were blocked by missing helper features. Having built
those features, the honest answer for most of them is that their renderers are
structurally different, not feature-poor. Rev 2's job is to say so, so the
migration stops at the right place instead of drifting into contorting helpers.

## Current adoption

Measured across `machines/src/` (40 registered machines, 34 files):

| Helper | Users | Status |
|---|---|---|
| `decode_gfx` / `decode_gfx_element` | 23 | Comprehensive, unchanged |
| `render_tilemap_scanline` (RGB) | 3 — `mario_bros`, `tkg04`, `namco_pac` | Enriched per 2a |
| `render_tilemap_scanline_indexed` | 3 — `galaga`, `digdug`, `gottlieb` | Added per 2b |
| `render_scrolled_tilemap_scanline` | **0** | Built, unused — see Dead Weight |
| `render_scrolled_tilemap_scanline_indexed` | 1 — `xevious` | Added beyond rev 1's plan |
| `draw_sprite_row` | 5 — `congo_bongo`, `galaxian_video`, `mario_bros`, `tkg04`, `namco_pac` | Unchanged |
| `draw_sprite_row_indexed` | 4 — `galaga`, `digdug`, `xevious`, `gottlieb` | Added per 2b |
| `render_bitmap_scanline` | 2 — `williams`, `gridlee` | Added per Feature 3 |
| `pal_nbit` | 2 — `mcr2`, `btime` | Added per Feature 4 |
| `compute_resistor_weights` / `compute_resnet_weights` | 7 | `compute_resnet_weights` added |
| `compute_resistor_net` | **0** | Built, unused — see Dead Weight |

## Feature status

### Feature 1 — Unify the Namco galaga/digdug/xevious renderers — **done**

`machines/src/namco_video.rs:16` now holds the single `namco_tilemap_offset`;
the four copies in galaga/digdug/pacman are gone. galaga and digdug render
through `render_tilemap_scanline_indexed` + `draw_sprite_row_indexed`.

Xevious went **further than planned**. Rev 1 said "keep Xevious's bg address
logic … the bg scroll migration is the one non-mechanical part and can trail";
instead a `render_scrolled_tilemap_scanline_indexed` helper was added and Xevious
uses it for both layers (`xevious.rs:692,742`). That was the right call and the
plan should have anticipated it: "scrolled" is a general tilemap property, not an
Xevious quirk.

### Feature 2a — Enrich the RGB tilemap helper — **done**

`TileInfo` (`tilemap.rs:21`) with `flip_x`/`flip_y` and a `TileInfo::new(code,
attr)` convenience for the common unflipped case; `resolve_color_fn` returning
`Option<(u8,u8,u8)>` for transparency (`tilemap.rs:55`). Implemented verbatim
from rev 1's proposed signature.

### Feature 2b — Indexed + priority variants — **helpers done, adoption partial**

`render_tilemap_scanline_indexed` (`tilemap.rs:107`) and `draw_sprite_row_indexed`
(`sprite.rs:108`) exist and carry four machines. Rev 1 predicted this would be
"the largest lift"; in practice the helpers were easy and the *migrations* are
where the cost sits. See "The remaining machines" for which ones are worth it
(answer: one).

### Feature 2c — Fix the shadowing wart — **done**

No `fn draw_sprite_row` remains anywhere in `machines/`.

### Feature 3 — Bitmap-scanline helper — **done, and correctly stopped at 2 machines**

`core/src/gfx/bitmap.rs:18`, adopted by `williams` and `gridlee`.

Rev 1 claimed this "captures 4 machines". It captures 2, and the two it missed
were never good fits — a claim rev 1 made without reading their renderers:

* **`missile_command`** (`missile_command.rs:718`) is not a packed-nibble
  unpack. It is bit-planar (`((pix >> 2) & 4) | ((pix << 1) & 2)`) with the
  third colour bit fetched from a *separate scattered address* for scanlines
  ≥ 224 via `get_bit3_addr`. No `(pixels_per_byte, high_first)` description
  covers that.
* **`ccastles`** (`ccastles.rs:839`) applies per-pixel horizontal scroll
  (`hscroll.wrapping_add(x ^ flip)`), so source x is not linear in destination
  x, and composites per pixel against a sprite buffer through a priority PROM.
  The helper assumes a contiguous source row and a linear x mapping.

Both stay hand-rolled. Feature 3 is closed.

### Feature 4 — Consolidate palette resistor-DAC math — **done bar one item**

* `pal_nbit` (`resistor.rs:121`) added; `btime` and `mcr2` migrated. No
  `pal2bit`/`pal3bit` implementations remain (only comments referencing them).
* `galaxian_video.rs:360` migrated off its hand-rolled `resnet_raw_weights` onto
  the new `compute_resnet_weights`.
* `mrdo_palette_rgb` (`mrdo.rs:266`, diode-coupled pot ladder) left standalone as
  designed.
* **Open:** `compute_tkg04_channel` was deduplicated but lives in
  `tkg04.rs:103`, with `mario_bros.rs:46` importing it *across machines*. Rev 1
  said to extract it into `resistor.rs`. The dedup is the important half and it
  happened; the housing is a one-commit tidy that removes a machine→machine
  dependency. Worth doing, low priority.

## Dead weight created by this plan

Two helpers were built on spec and have no production caller. Both are the
predictable cost of adding RGB and indexed siblings of everything rather than on
demand, and both should be resolved rather than left:

* **`render_scrolled_tilemap_scanline`** (non-indexed) — 0 callers; only the
  `_indexed` sibling is used. `foodf` is the one plausible adopter (see below).
  Migrate foodf onto it, or delete it.
* **`compute_resistor_net`** (`resistor.rs:63`) — 0 production callers, exercised
  only by its own unit tests. Rev 1 proposed expressing the DK-family Darlington
  DAC through it; that didn't happen (`compute_tkg04_channel` models the TTL
  output stage itself). Either delete it or state in the module docs that it is
  kept as a reference model.

Adding both siblings of a helper before either has a caller is the pattern to
avoid; new gfx helpers should land with their first migration in the same commit.

## The remaining machines

Rev 1's "Deltas summary" table listed these as *blocked* on missing helper
features. That framing is now inverted: the features exist, nothing is blocked,
and what remains is migration labour on the machines with the most idiosyncratic
render paths. An explicit call on each, from reading the renderers:

| Machine | Verdict | Why |
|---|---|---|
| **`congo_bongo`** fg tilemap (`congo_bongo.rs:970`) | **Migrate** | The one clean remaining fit. Already per-scanline, 32 columns of 8×8, `tiles.pixel(code, px, py)` + palette lookup with pixel-0 transparency. `fg_bank`/`pal_offset` capture into the closures. Sprites already use `draw_sprite_row`. |
| **`foodf`** (`foodf.rs:740`) | **Migrate if it retires the dead helper** | Column-scan tile order (`col * rows + row`) expresses fine in `tile_info_fn`; the −8 scroll needs `render_scrolled_tilemap_scanline`, which currently has no users. Do these two together or not at all. |
| **`atari_system1`** playfield/MO (`:806`, `:853`) | **Do not migrate** | Two independent blockers. Its index buffer is `[u16]` (10-bit pens, 0x000–0x3FF) and the helper writes `u8`; and it selects a *different `GfxCache` per tile* (`self.playfield.banks.get(bank_id)`) while the helper takes one `&GfxCache`. Supporting either would contort the helper — Closed Decision 4. |
| **`mcr2`** (`:424`) | **Do not migrate** | Confirms rev 1's hedge. Renders per *tile*, gated on `tile_dirty.is_dirty()`, at 2× upscale, writing pixel and priority buffers as 2×2 blocks. The scanline helper's shape (iterate columns of one scanline) is simply the wrong loop. |
| **`galaxian_video`** tilemap (`:490`) | **Do not migrate** | **Per-column independent vertical scroll** — `eff_y = (mame_y + objram[col*2]) & 0xff`, so `tile_row` and `py` differ per column. The helper computes both once, outside the column loop. Structural mismatch. Its sprites already use `draw_sprite_row`. |
| **`mrdo`** (`:562`) | **Do not migrate** | Per-*pixel* whole-frame loop into a `[u16]` pen buffer, sampling two scrolled tilemaps with per-pixel flip mirroring and a `TILE_FORCE_LAYER0` opacity rule. Same `u16` blocker as atari_system1, plus per-pixel geometry. |
| **`missile_command`**, **`ccastles`** bitmap | **Do not migrate** | See Feature 3. |
| **`btime`** (`:648`) | **Leave alone** | Its local `blit_tile` already serves chars, sprites and background from one place — the right factoring for this machine. Its tilemap is stored transposed (`x = 31 - off/32`) and drawn tile-wise, not scanline-wise. Nothing to gain. |

Net remaining work: **congo_bongo's fg tilemap**, and **foodf paired with
retiring `render_scrolled_tilemap_scanline`**. Everything else is closed.

## Remaining plan

1. Delete the stray `</content>` artifact and this doc's stale rotation section
   *(done in rev 2)*.
2. Migrate `congo_bongo` fg to `render_tilemap_scanline`; frameshot-compare.
3. Decide foodf: migrate to `render_scrolled_tilemap_scanline`, or delete that
   helper. Do not leave it unused.
4. Decide `compute_resistor_net`: delete, or document as a reference model.
5. Move `compute_tkg04_channel` from `tkg04.rs` to `resistor.rs`, dropping the
   `mario_bros` → `tkg04` dependency.

Steps 2–5 are independent, each one commit, each frameshot-validated.

## Testing

```bash
cargo test -p phosphor-core       # helper unit tests (flip, transparency, bitmap, DAC)
cargo test -p phosphor-machines   # per-machine render/palette tests
cargo test -p phosphor-harness --test golden_frame_test   # the real regression gate
cargo clippy --all-features --all-targets
```

Rev 1 named `disasm frameshot --compare` as the regression gate; it exists
(`tools/disasm/src/main.rs:489,585`) and is the right tool for an ad-hoc
before/after on one machine. But the committed gate is now
`harness/tests/golden_frame_test.rs`, which pins a SHA-256 of every registered
machine's oriented frame in `harness/tests/golden/frames.toml`. **Pixel-identical
remains the acceptance bar**: a pure refactor that changes a golden hash is a
bug, and the hash must not be recaptured to make a migration pass.

## Closed Decisions

Rev 1's six, all of which survived implementation:

1. **Enrich, don't fork** — proven; `TileInfo` + `Option` transparency carried
   seven machines with three call-site updates.
2. **Transparency lives in the resolve closure.**
3. **Indexed+priority is a separate variant, not a flag.**
4. **2× upscale and dirty-marking stay caller-side** — and mcr2 consequently
   stays hand-rolled entirely, which is the correct application of this rule.
5. **Bespoke palette ladders stay bespoke** (`mrdo`).
6. **Pixel-identical is the acceptance bar.**

Added in rev 2:

7. **A `u8` index buffer is a deliberate limit of the indexed helpers.**
   `atari_system1` and `mrdo` use `u16` pen buffers. Widening the helpers (or
   generic-ing over the index type) to capture two machines that also fail on
   other axes is not worth it. If a third `u16` machine appears, revisit.
8. **New gfx helpers land with their first caller in the same commit.** Both
   unused helpers came from building a sibling on spec.
9. **"Scrolled" is a first-class tilemap property**, not a per-machine quirk —
   ratified after the fact by the Xevious outcome.

## Relationship to other work

* **Rotation — shipped, no longer relevant here.** Rev 1 listed rotation
  generalization as out-of-scope pending "a separate existing plan". That
  landed: `ScreenRotation` no longer exists; `Orientation`
  (`core/src/gfx/mod.rs:33`) is a bitfield applied centrally by the frontend via
  `apply_orientation`, machines declare it per frame, and rev 1's cited
  `mrdo.rs:597` inline ROT270 hand-transform is gone (`mrdo.rs:662,674`).
  Machines now render native and the rotate step happens once, centrally —
  exactly the composition rev 1 hoped for.
* **Mid-frame raster fidelity** — [`mid-frame-raster-fidelity.md`](mid-frame-raster-fidelity.md)
  and [`mid-frame-raster-audit.md`](mid-frame-raster-audit.md) own the
  render-once-vs-per-scanline question. Nothing here changed any machine's
  strategy, so this work neither fixed nor worsened that hazard; the enriched
  scanline helpers make that migration easier where it is undertaken.
* **Dirty-tracking component** — still separate, still only mcr2 uses
  `DirtyBitset`. Rev 1 suggested 2b's indexed buffers would be the substrate for
  it; mcr2's decision to stay hand-rolled weakens that argument, so the
  dirty-tracking proposal should not assume this work de-risked it.
