# Debugging: Mr. Do! Tilemap Transparency (`TILE_FORCE_LAYER0`)

This documents a rendering bug in the Mr. Do! machine (`mrdo`) and — just as
importantly — a **process** failure: the bug took far longer to find than it
should have, because the answer was in the MAME source the whole time and the
investigation kept substituting empirical pixel-diffing for reading it.

## System Overview

Mr. Do! (Universal 8201) draws two 8×8 tilemaps plus sprites through MAME's
generic `tilemap_t`, with an **indirect** palette:

- **BG tilemap** (`gfx2`, `bgvideoram` @ 0x8000): the playfield dirt and the
  scrolling rainbow band behind the title logo.
- **FG tilemap** (`gfx1`, `fgvideoram` @ 0x8800): text, the logo letters, and
  the tiles that surround/mask the logo.
- `screen_update_mrdo`: `fill(0)` → BG `draw(...,0,0)` → FG `draw(...,0,0)` →
  sprites (plain `transpen`, always on top).
- Both tilemaps: `set_transparent_pen(0)`.
- Tile attribute byte: `color = attr & 0x3f`, code high bit `= (attr & 0x80) << 1`,
  and **`attr & 0x40` → `TILE_FORCE_LAYER0`** in `get_{bg,fg}_tile_info`.

## Symptom

Three visually distinct problems that were all the **same** bug:

1. **Title logo:** the scrolling rainbow band showed too few colors and bled
   into the whole bounding box instead of showing only under the "Mr. Do!"
   letters.
2. **Maze corridors:** dug tunnels (which should be black) filled green.
3. General: the correct behaviour is "value-0 pixels are transparent *except*
   in certain tiles."

## Root Cause

MAME's tilemap decides transparency per pixel from the tile's **flags**, not
from the raw pixel value alone:

- `tilemap.h:336-345`:
  ```cpp
  constexpr u8 TILEMAP_PIXEL_TRANSPARENT = 0x00;
  constexpr u8 TILEMAP_PIXEL_LAYER0      = 0x10;
  ...
  constexpr u8 TILE_FORCE_LAYER0 = TILEMAP_PIXEL_LAYER0; // "no transparency" — render opaque
  ```
- `tilemap.cpp` `tile_draw` (~870): `flagsptr = penmap[pen] | category`, where
  `category |= flags & TILE_FORCE_LAYER0`. `set_transparent_pen(0)` makes only
  raw pen 0 map to `TRANSPARENT`; every other pen is `LAYER0`.
- `draw(..., flags=0)` defaults to `TILEMAP_DRAW_LAYER0` and blits any pixel
  whose flag byte has the `LAYER0` (0x10) bit set.

Because **`TILE_FORCE_LAYER0` *is* the `LAYER0` bit**, a tile with `attr & 0x40`
set has that bit OR'd into *every* pixel's flags — including its pen-0 pixels —
so the whole tile renders opaque (pen `color*4 + 0` is drawn instead of being
transparent).

Mr. Do! uses this three ways at once:
- The rainbow **band** tiles (e.g. `0x27`, `attr 0x66`) are `FORCE_LAYER0`, so
  their value-0 halves emit their `color*4+0` pen — that is where the extra
  rainbow colors come from.
- The **priority-blank** FG tiles surrounding the logo (`0x29`, `attr 0x40`,
  color 0) are `FORCE_LAYER0`, so their value-0 pixels draw pen 0 = **opaque
  black**, masking the band to the logo shape.
- Ordinary maze/dirt value-0 pixels (no `0x40`) stay transparent → black
  corridors.

## Fix

For **both** tilemaps, a pixel is opaque iff `value != 0 || (attr & 0x40)`:

```rust
let bval = self.bg_cache.pixel(bcode, bx & 7, by & 7);
if bval != 0 || battr & 0x40 != 0 {
    p = (battr as u16 & 0x3f) * 4 + bval as u16;
}
```

One rule, applied identically to BG and FG, fixed the logo colors, the logo
masking, and the maze corridors simultaneously. Diffs vs a MAME reference frame
dropped to the scroll-phase residual (~0.5–1.3%).

---

## Process Postmortem — why this took far too long

The one-line answer (`TILE_FORCE_LAYER0 = TILEMAP_PIXEL_LAYER0`) was one `grep`
away from the start. The investigation instead wandered through: a color-keyed
"pen-based" transparency hack, scroll-sign theories, palette-DAC suspicion,
gfx-decode suspicion, and repeated game-state (VRAM) dumps. What went wrong:

1. **Stopped reading the source one symbol short.** `set_transparent_pen` and
   `tile_draw` were read and (correctly) understood to key transparency off the
   raw pixel value. `TILE_FORCE_LAYER0` was seen in `get_tile_info` and
   **assumed to be a redundant no-op** ("everything is already LAYER0") *without
   reading its definition*. That definition was the entire bug. **Chase every
   unfamiliar constant to its definition before dismissing it.**

2. **Substituted pixel-diffing for reading.** Each contradiction ("val-0 is
   opaque here but transparent there") triggered a new hypothesis tested by
   render→PNG→diff, instead of returning to the code to resolve the
   contradiction. Empirical diffing is good for *confirming* a fix; it is a poor
   way to *derive* a mechanism, and it is slow.

3. **A coincidentally-correct hack anchored the investigation.** "Draw when
   `color*4+val != 0`" (opaque when color ≠ 0) matched the logo by eye and was
   accepted as "the fix." It was wrong — it also made maze corridors opaque —
   and it hid the real rule for a long time. **A fix that passes the eyeball
   test but that you can't derive from the reference is a red flag, not a
   conclusion.**

4. **Re-verified known-good facts.** Game-state/VRAM was dumped and confirmed to
   match MAME several times over. Once established, re-checking it repeatedly
   was wasted effort and noise.

5. **Treated three symptoms as three bugs.** The logo band, the logo mask, and
   the maze corridors are one mechanism. Looking for the *shared* cause earlier
   (they all involve value-0 opacity) would have pointed straight at the tile
   flag.

**Takeaway:** when validating against a reference emulator, read the reference's
render path to the leaf definitions *first*, form the mechanism from the code,
and use pixel comparison only to confirm. Every attribute bit the reference
reads is load-bearing until its definition proves otherwise.

## Tooling

This investigation produced the headless render/compare tooling now in the
`disasm` CLI (`frameshot`, `imgdiff`) — full reference in
[`docs/disassembler.md`](disassembler.md). The loop that eventually pinned the
bug was: capture the same frame from MAME and from our emulator, diff them, and
zoom into the differing region.

### 1. Capture a MAME reference frame (headless)

MAME renders offscreen and a small Lua autoboot script snapshots at an exact
frame, then exits a few frames later so the PNG flushes:

```bash
cat > snap.lua <<'LUA'
local n = 0
emu.register_frame_done(function()
  n = n + 1
  if n == 3100 then manager.machine.video:snapshot() end
  if n == 3108 then manager.machine:exit() end
end)
LUA

SDL_VIDEODRIVER=offscreen mame mrdot -rompath ~/mame/roms -sound none -video soft \
    -snapname mame_3100 -snapshot_directory . -autoboot_script snap.lua \
    -seconds_to_run 60
```

### 2. Capture our frame and diff it in one shot

```bash
disasm frameshot --machine mrdo --frames 3100 --compare mame_3100.png \
    -o mine_3100.png ~/mame/roms
#   wrote mrdo frame 3100 -> mine_3100.png (192×240)
#   diff vs mame_3100.png: 233/46080 (0.5%)
```

### 3. Localize the difference

`imgdiff -o` writes a highlight image (matches dimmed, differences solid red) so
the *shape* and *location* of the disagreement is obvious — that is what showed
the problem was confined to the maze corridors / logo band, not the whole frame:

```bash
disasm imgdiff mine_3100.png mame_3100.png -o diff.png
#   diff: 233/46080 (0.51%)
```

### 4. Inspect the reference at the pixel/VRAM level

When the pixels disagreed, MAME's Lua console read back the exact tile data and
output pixel, which is how the tile attributes (the `0x40` bit) and the rendered
colors were compared against ours — e.g. dump BG VRAM and read a screen pixel:

```lua
local sp = manager.machine.devices[":maincpu"].spaces["program"]
print(string.format("%02x/%02x", sp:read_u8(0x8000 + row*32 + col),   -- BG attr
                                  sp:read_u8(0x8000 + row*32 + col + 0x400)))  -- BG code
local px = manager.machine.screens[":screen"]:pixel(x, y)             -- 0xAARRGGBB
```

The lesson from the process postmortem applies here too: these tools *confirm* a
hypothesis quickly, but the hypothesis itself should come from reading the
reference's render path — not from staring at diffs.
