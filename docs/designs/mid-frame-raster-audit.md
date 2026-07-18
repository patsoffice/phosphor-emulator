# Phase 0 Audit — Mid-Frame Raster Tier A/B Classification

Status: **complete**. This is the evidence record for Phase 0 of the
mid-frame-raster-fidelity epic (`~/mid-frame-raster-fidelity.md`): decide, by
measurement, which render-once machines actually need per-scanline rendering
(Tier A) vs. which are static-per-frame and only need the cheap
frame-boundary fix (Tier B). It also classifies every other registered machine
by render strategy so the whole roster is accounted for.

## Method

The audit is the first consumer of the headless `disasm trace` tooling. Two
observers, both now wired on every raster board:

- **Events** (`--events memwrite`) — CPU-agnostic, region-tagged bus writes.
  Added in this branch for the `AddressSpace16/32` boards (previously only the
  hand-rolled Namco boards emitted events). Because they are CPU-agnostic and
  mirror-resolving, events are the primary instrument (e.g. Satan's Hollow
  writes its palette through the `0xFF80` VRAM mirror, which a single-address
  watchpoint on the base `0xEF80` misses).
- **Watchpoints** (`--watch cpu:addr:kind`) — exact-address, but must target the
  CPU that performs the write. Gotcha found during the audit: on the Namco
  boards the **sub-CPU (cpu 1)**, not the main CPU, writes the video registers.

For each machine the video-affecting registers (palette, scroll, tile/sprite
bank, flip) were enumerated from the board source, then their writes were
captured across representative attract-**demo** gameplay (the self-playing
demo exercises the same registers as a real game). Each write's cycle is mapped
to a position within the frame:

```
frame_cycle = cycle % cycles_per_frame
scanline    = frame_cycle / cycles_per_scanline
in_active_display = displayed_visible_lo <= frame_cycle < displayed_visible_hi
```

A write is a **Tier A candidate** only if it lands inside active display *and*
represents a genuine mid-frame change (the value bit that matters actually
changes, at varying scanlines, repeatedly per frame — a raster split). A
register written once per frame in the vblank handler, only at screen
transitions, or only to acknowledge an interrupt is **Tier B**: the
render-once, end-of-frame sample is already pixel-correct.

## Classification summary

| Category | Meaning | Machines |
|---|---|---|
| **Render-once → Tier A** | needs per-scanline rendering | atari_system1 (**Road Runner** MO-bank) |
| **Render-once → Tier B** | static per frame; only needs the `ifs0` frame-boundary fix | digdug, galaga, xevious, mrdo, burgertime, foodf, qbert, shollow, marble |
| **Per-scanline (already correct)** | renders each scanline in `tick()` from live state; no `ifs0` bug, no migration | joust, robotron, sinistar, pacman, mspacman, dkong, dkongjr, mariobros, congobongo, galaxian, scramble, scobra, frogger, mooncrst, pisces, uniwars, gridlee, ccastles, missile, irobot |
| **Vector (N/A)** | DVG/AVG display list rasterized whole-frame; no scanline hardware | asteroid, astdelux, llander, tempest, starwars, quantum, esb |

## Render-once machines — evidence

Active-display window and per-register write timing, measured over the demo
window noted. "inDisp" = writes during displayed active scanlines; "vblank" =
writes outside it.

| Machine | Board | Video registers | Measurement | Tier |
|---|---|---|---|---|
| **digdug** | namco_galaga | LS259 `0xA000-0xA007` (bg page/color/flip) | never written during demo (frames 2600–3400) or boot; static | **B** |
| **galaga** | namco_galaga | LS259 `0xA000-0xA007` (starfield ctrl/flip) | 0 writes over 1500 frames (all CPUs); starfield scroll is internal + vblank-latched (`update_starfield_at_vblank`) | **B** |
| **xevious** | namco_galaga | scroll `0xD000/0xD020` (bg X/Y, **cpu 1**), flip `0xD070` | scroll = exactly 1 write/frame at scanline ~8 (vblank-IRQ-handler latency, not a split); flip = vblank only | **B** |
| **mrdo** | mrdo | scroll `0xF000-0xFFFF`, flip `0x9800` | written only at init (frames 1–3); static during demo (2100–2900); code: "single VBLANK IRQ, no mid-frame raster" | **B** |
| **burgertime** | btime | flip `0x4002`, scroll `0x4004`, palette `0x0C00-0x0C0F` | scroll written 4× total at scanlines 88–91 across 4 frames (sporadic transitions); palette/flip vblank | **B** |
| **foodf** | foodf | palette `0x950000-0x9501FF`, flip `0x948000` | palette = 179 200 writes, **all vblank**; `0x948000` written in active display but flip **bit 0 constant = 0** (values 0x06/0x10/0x14 are IRQ acks on bits 2/3) | **B** |
| **qbert** | gottlieb | video_ctrl `0x5803`, sprite_bank `0x5804`, palette `0x5000-0x57FF` | 800 video_ctrl + 25 600 palette writes, **all vblank** (0 in active display) | **B** |
| **shollow** | mcr2 | palette RAM `0xEF80-0xEFFF` (+ `0xFF80` mirror) | palette written only in 4 of 2400 frames (screen-transition bulk reloads that span active display because the CPU rewrites 32 entries while the beam scans); no repeating per-frame raster color-bar | **B** |
| **marble** | atari_system1 | H/V scroll `0x800000/0x820000`, bank `0x860001`, priority `0x840001` | scroll = ~1 write/frame, **all vblank**; no mid-frame bank switching in Marble Madness | **B** |
| **roadrunner** | atari_system1 | motion-object bank `0x860001` bits 5–3 | **Tier A.** The game reprograms the MO bank mid-frame from its programmable scanline (SLIP) interrupt; `bankselect_w` logs `(scanline, mo_bank)` and `render_motion_objects` renders each scanline band with its live bank, clipping sprites at the boundary. Confirmed by code + tests `bankselect_logs_midframe_mo_bank_changes`, `motion_objects_follow_midframe_bank_switch`. Board already implements the per-band mitigation. | **A** |

Notes:
- `atari_system1` is **Tier A because of Road Runner**, even though Marble
  Madness (same board) is Tier B. The board already tracks per-scanline
  `mo_bank_changes`, so the Tier A case is handled today; no migration needed
  for Phase 2/3.
- The nine Tier B machines are the targets of **Phase 1** (move the single
  render into a `tick()` frame-boundary hook; closes `ifs0`; byte-identical
  output).

## Per-scanline machines (already mid-frame-correct)

These render each scanline inside `tick()` from live VRAM/registers, so
mid-frame changes are already honored and there is no `ifs0` stale-image bug.
No action.

- **williams** (joust, robotron, sinistar): `render_scanline` at each scanline
  boundary (`williams.rs:472`).
- **namco_pac** (pacman, mspacman), **tkg04** (dkong, dkongjr),
  **mario_bros**, **congo_bongo**, **galaxian_video** (galaxian, scramble,
  scobra, frogger, mooncrst, pisces, uniwars): same per-scanline hook.
- **gridlee, ccastles, missile, irobot**: raster bitmap/framebuffer games that
  already latch and render per-scanline (gridlee palette-bank per scanline;
  ccastles hscroll/vscroll/palette per scanline; missile 8-entry palette per
  scanline; irobot polygon framebuffer + 64-entry palette with a 32V scanline
  IRQ). All Tier-A-equivalent and already satisfied.

## Vector machines (not raster)

DVG/AVG games render a vector display list, rasterized once per frame from an
atomically-generated list; `total_scanlines = 1`, no scanline hardware, so the
raster Tier A/B split does not apply.

- **asteroid, astdelux, llander** (Atari DVG), **tempest, quantum, starwars,
  esb** (Atari AVG). `esb` (Empire Strikes Back) runs on **Star Wars AVG vector
  hardware**, not Williams.

## Follow-ups filed

- Unify the Namco boards (galaga/xevious/digdug) onto `AddressSpace16` and
  retire their hand-rolled `trace_bus_write` — uniform event path; rich tags
  can move into well-named region descriptors.
- Marble Madness motion-object (MOB) rendering is incorrect (observed during
  the audit; separate from the raster classification).
