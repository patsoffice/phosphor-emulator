# Design: Mid-Frame Raster Fidelity

> **Status: complete — superseded for future work by
> [`raster-sampling-fidelity.md`](raster-sampling-fidelity.md).**
> Tracked as `phosphor-emulator-mid-frame-raster-7zzi`; the epic and all four
> phases are closed, along with `phosphor-emulator-ifs0`. Phase 0's evidence
> record is [`mid-frame-raster-audit.md`](mid-frame-raster-audit.md).
>
> **Outcome.** Phase 0 measured every render-once machine and found **one**
> Tier A case (atari_system1 via Road Runner's mid-frame MO-bank switch), which
> the board already handled with per-scanline bands. Phase 1 (commit `e9380a3`)
> applied the cheap frame-boundary fix to the six machines that cached a render
> in `run_frame`, closing `ifs0` with byte-identical output. **Phases 2 and 3
> were closed as not needed** — no machine measured Tier A requiring migration.
> Notably xevious, this doc's most confident "A?" guess, measured Tier B: its bg
> scroll is written once per frame at a stable scanline ~8, which is
> VBLANK-IRQ-handler latency, not a mid-screen split.
>
> **Why there is a successor.** The plan below was executed faithfully and its
> measurements stand. What changed afterwards is the *acceptance standard*, not
> the evidence: Phase 0 sampled attract-**demo** play only, so "Tier B" means
> "no mid-frame effect appeared in the demo loop", not "this machine has none".
> Nine machines carry that assumption with nothing tracking it. Two of the nine
> (burgertime scroll, shollow palette) were additionally classified B on
> *visibility* grounds despite genuine active-display writes — an argument from
> "nobody will notice", which sits badly against `CLAUDE.md`'s
> Correctness-first ordering. [`raster-sampling-fidelity.md`](raster-sampling-fidelity.md)
> reframes the goal as *sample each register at the rate the hardware samples
> it* — which subsumes per-scanline rendering and also rules it out where the
> hardware latches at vblank.
>
> **One dependency was orphaned.** Phase 3's close reason records that it was
> the only consumer of `graphics-consolidation.md` Feature 2b (indexed+priority
> scanline helpers) from this epic, so that dependency is no longer motivated by
> raster fidelity and must be justified on its own merits.
>
> The original proposal follows unchanged, for the record.

---

A correctness proposal: several machines render the whole frame once at
end-of-frame, sampling video state a single time, so mid-frame palette/scroll/
bank changes are lost. This doc defines how to decide which machines actually
need per-scanline rendering (objectively, using the trace tooling), and how to
migrate the ones that do — while fixing the cheaper cases without paying
per-scanline cost. It also subsumes the `ifs0` debugger-stale-image bug for the
machines it touches.

## Context

Two render strategies coexist in the codebase (see the graphics audit in
[`graphics-consolidation.md`](graphics-consolidation.md)):

- **Per-scanline** (williams, namco_pac, tkg04, mario_bros, congo_bongo,
  galaxian_video): the board renders each scanline *inside `tick()`* at the
  moment that scanline is reached, from live video state.
- **Render-once** (galaga, digdug, xevious, btime, gottlieb, mrdo, foodf,
  mcr2/satans_hollow, atari_system1): the board runs the whole CPU frame, then
  renders the entire image once from the *final* video state.

Render-once is simpler but samples every video register exactly once per frame.
Any change the game makes to palette, scroll, tile/sprite bank, or flip
*during active display* is invisible — only its end-of-frame value is drawn.
Real hardware shows the pre-change rows with the old value and post-change rows
with the new one (split screens, palette bars, mid-screen scroll seams,
status-bar/playfield boundaries). This is a **correctness gap** for a
cycle-accurate emulator (design priority #1).

`atari_system1.rs` already hit this and partially works around it by recording
per-scanline `mo_bank_changes` (line 551) while still compositing once — direct
evidence that at least one machine needs mid-frame fidelity today.

### The two patterns, concretely

Target (namco_pac, `tick()` renders at each scanline boundary):

```rust
// in NamcoPacBoard::tick(), per CPU cycle
if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
    let scanline = (frame_cycle / TIMING.cycles_per_scanline) as u16;
    if scanline < VISIBLE_LINES as u16 {
        self.render_scanline(scanline as usize);   // reads LIVE state
    }
}
// render_frame() later just rotates/copies the persistent scanline_buffer.
```

Source (galaga, one pass after the whole frame):

```rust
fn run_frame(&mut self) {
    for _ in 0..TIMING.cycles_per_frame() { self.tick(); }
    self.update_starfield_at_vblank();
    self.render_video();   // reads FINAL state — mid-frame changes already lost
}
```

### The `ifs0` connection

Because per-scanline machines render *during `tick()`*, they also refresh under
the debugger's `debug_tick()` (which calls `tick()` but never `run_frame()`).
Render-once machines don't — that is exactly `phosphor-emulator-ifs0`
("Debugger shows a stale image for machines that render in run_frame"). Moving
the render into `tick()` fixes both problems with one change. This doc therefore
**closes `ifs0`** for every machine it touches, and generalizes `ifs0`'s fix
option (a) into a correctness-driven plan.

## Design Goals

1. **Correctness first:** machines that exhibit mid-frame raster effects render
   with per-scanline accuracy.
2. **No cost where unneeded:** machines that are genuinely static-per-frame must
   not pay per-scanline rendering cost — but must still refresh under the
   debugger (fix `ifs0`).
3. **Objective classification:** decide which machines need per-scanline via
   measurement, not guesswork.
4. **Pixel-identical where behavior is unchanged:** a static-per-frame machine's
   output must not change; a mid-frame machine's output must move *toward* the
   MAME reference, validated by frameshot/imgdiff.
5. **Reuse the shared helpers** from `graphics-consolidation.md` so the
   migration is mechanical, not a per-machine rewrite.

## The classification: Tier A vs Tier B

Not every render-once machine needs per-scanline rendering. Two outcomes:

- **Tier A — needs per-scanline.** The game writes a video-affecting register
  (palette, scroll, tile/sprite bank, flip) *during active display*, and that
  change is meant to be visible within the frame. Requires true per-scanline
  rendering.
- **Tier B — static per frame.** All video-affecting state is stable during
  active display (writes only during vblank / between frames). End-of-frame
  sampling is already pixel-correct; the only bug is `ifs0`. Fix cheaply by
  moving the single render into a frame-boundary hook in `tick()` — **no
  per-scanline cost**.

Many of the render-once machines are likely Tier B: their sprite RAM is latched
at vblank and their playfields don't split. The classification must be
*measured*, because "looks right in attract mode" is not proof.

### Objective classification method (uses the trace tooling)

The audit is answerable mechanically with the debug infrastructure — the event
ring and watchpoints from [`headless-debugging.md`](headless-debugging.md):

1. Set write-watchpoints (or enable `DebugTrace` filtered to `DeviceWrite`/
   `MemoryWrite`) on the machine's video registers: palette RAM/PROM latch,
   scroll registers, bank latches, flip latch.
2. Run headlessly (via the proposed `disasm trace`), and for each hit compare
   its cycle against the active-display window
   (`scanline < VISIBLE_LINES`, i.e. `cycle % cycles_per_frame <
   VISIBLE_LINES * cycles_per_scanline`).
3. **Any** video-register write inside active display across representative
   gameplay ⇒ **Tier A candidate** (then confirm visually with a MAME
   per-scanline reference frame at the offending frame). **No** active-display
   writes ⇒ **Tier B** (safe to keep single-pass).

This turns "does this game use mid-frame effects?" into a yes/no measurement and
is the first concrete consumer of the headless trace tooling. Record the result
per machine so the decision is auditable.

## Tier B fix — frame-boundary render in `tick()`

For static-per-frame machines, keep the single render pass; just move *when* it
runs so the debugger sees it. Two equivalent options:

- Render on the last visible-line boundary inside `tick()` (mirrors the
  per-scanline hook shape but fires once), or
- Have `run_frame()` and `debug_tick()` share a "frame just completed" signal so
  the cached framebuffer is refreshed on both paths.

The first keeps a single render site and removes the redundant explicit
`render()` in `run_frame` — matching `ifs0`'s preferred option (a). Output is
unchanged (same state sampled at the same frame boundary); only the debugger now
updates. Cost: zero beyond today.

## Tier A migration — true per-scanline rendering

For machines that need mid-frame fidelity, move rendering into `tick()` at
scanline boundaries, following the namco_pac template:

1. Add a persistent `scanline_buffer` (native size) to the board if not present.
2. In `tick()`, at each `cycles_per_scanline` boundary with
   `scanline < VISIBLE_LINES`, call `render_scanline(scanline)` reading **live**
   video state.
3. `render_frame()` becomes the rotate/copy of the persistent buffer (as
   namco_pac/tkg04 already do).
4. Delete the end-of-frame `render_video()`/`render()` call from `run_frame()`.

### Sampling convention

Render scanline *N* from the video state as it stands at the **start of
scanline N** (i.e. after N scanlines of CPU execution). This is the standard
per-scanline granularity used across MAME raster drivers and matches the
existing per-scanline machines. It is an approximation — sub-scanline (mid-line)
changes are still quantized to scanline boundaries — which is the accepted
fidelity level for these boards. Document the convention in each migrated
renderer.

### What stays frame-latched

Not all "once per frame" reads are bugs. State the hardware latches at vblank
must **remain** end-of-frame reads even after migration — e.g. galaga's
starfield scroll (`update_starfield_at_vblank`) and any double-buffered sprite
RAM that hardware reads at vblank. Migration moves *per-scanline-varying* state
to per-scanline sampling; it must not wrongly re-sample vblank-latched state
per line. This is the main correctness risk of the migration and must be
checked against MAME.

### Cost

Per-scanline rendering renders each visible pixel once, same as the single-pass
version — total pixel writes are unchanged. The overhead is re-evaluating
tile-info lookups per scanline (height/tile_height× the per-tile version). For
these small rasters that is negligible; if a profile shows otherwise, it is
exactly the case the deferred cached-tilemap/`DirtyBitset` component targets.

### Priority-compositing machines are the hard tier

mcr2, gottlieb, and atari_system1 composite tiles + sprites through an
**indexed + priority buffer**, not direct RGB, and do it in separate full-frame
passes. Per-scanline priority compositing (tiles then sprites into one scanline,
respecting the priority buffer) is a larger change and **depends on the indexed
+ priority scanline helpers proposed in `graphics-consolidation.md` Feature 2b**.
Sequence these last, after 2b lands. If measurement shows one of them is Tier B,
prefer the cheap frame-boundary fix and skip the compositing rework.

## Per-machine audit (candidate classification — confirm by measurement)

Initial hypotheses; the trace-based method above is authoritative.

| Machine | Likely tier | What to check for active-display writes |
|---|---|---|
| digdug | B (static playfield) | tile/sprite bank, palette |
| galaga | B (starfield latched at vblank) | tilemap/color RAM writes mid-frame |
| xevious | A? (scrolling bg) | bg scroll register writes during display |
| btime (burgertime) | B? | palette latch, X/Y-swap mirror |
| mrdo | B? | fg/bg scroll, palette |
| foodf | B? | playfield scroll, palette |
| gottlieb (qbert) | B? | charram re-decode, palette during display |
| mcr2/satans_hollow | A? (MCR palette effects) | palette RAM, sprite bank mid-frame |
| atari_system1 (marble/roadrunner) | **A (confirmed)** | already tracks per-scanline `mo_bank_changes` |

"?" = must be measured. The point of the audit is to replace these guesses with
evidence before doing any migration.

## Migration Plan

### Phase 0 — Audit (do this first)
1. Enumerate each render-once machine's video registers.
2. Run the trace-based classification (write-watch during active display) across
   representative gameplay for each; record Tier A/B per machine with evidence.

### Phase 1 — Tier B fixes (closes `ifs0`, zero output change)
1. Move the single render into a frame-boundary hook in `tick()` for every
   Tier B machine; delete the `run_frame` render call.
2. Verify frameshot output is byte-identical to pre-change; verify the debugger
   now updates (the `ifs0` acceptance test).

### Phase 2 — Tier A, non-priority machines
1. Migrate each confirmed Tier A direct-RGB machine (e.g. xevious bg) to
   per-scanline rendering via the shared scanline helpers.
2. Validate the split-frame case against a MAME per-scanline reference (not just
   attract mode); confirm vblank-latched state is not wrongly re-sampled.

### Phase 3 — Tier A, priority-compositing machines
1. After `graphics-consolidation.md` Feature 2b (indexed+priority scanline
   helpers) lands, migrate confirmed Tier A priority machines (mcr2,
   atari_system1) to per-scanline compositing.
2. Validate against MAME references at the frames where the effect appears.

Phases 0–1 are low-risk and high-value on their own (they close `ifs0` and
establish the evidence base). Phases 2–3 are done per-machine, only where the
audit proves need.

## Testing

```bash
cargo test -p phosphor-machines
cargo clippy --all-features --all-targets
```

- **Tier B (regression):** `disasm frameshot --compare` each machine against a
  pre-change capture — must be byte-identical (behavior unchanged). Add an
  `ifs0` acceptance check: under `debug_tick`, the rendered framebuffer changes
  across frames (was frozen).
- **Tier A (fidelity):** capture a MAME reference frame at the exact frame where
  the mid-frame effect is visible (the frameshot/imgdiff rig, as used for
  Mr. Do! and Asteroids) and assert the migrated output matches within the
  established tolerance — and improves on the pre-migration (single-sample)
  frame.
- **Convention guard:** a targeted test that a mid-frame palette/scroll write
  changes only the rows below the write, not the whole frame (can be built on a
  synthetic board or a known game frame).
- **No vblank regression:** confirm vblank-latched state (galaga starfield,
  double-buffered sprite RAM) is unchanged after migration.

## Closed Decisions

1. **Measure, don't guess.** Per-scanline rendering is applied only where a
   trace proves active-display video-register writes. This avoids both missed
   bugs and needless per-scanline cost.
2. **Two tiers.** Static-per-frame machines get the cheap frame-boundary fix
   (closing `ifs0`); only mid-frame machines get true per-scanline rendering.
3. **Scanline granularity is the target fidelity** — the same approximation the
   existing per-scanline machines use; sub-scanline changes are quantized to
   scanline boundaries.
4. **Vblank-latched state stays end-of-frame.** Migration moves only
   per-scanline-varying state; re-sampling vblank-latched state per line would
   be a new bug.
5. **Priority machines wait on the indexed scanline helpers** (graphics
   Feature 2b) rather than hand-rolling per-scanline priority compositing.
6. **Pixel-identical acceptance for Tier B; MAME-reference improvement for
   Tier A** — validated with the existing frameshot/imgdiff rig.

## Relationship to other work

- **`phosphor-emulator-ifs0`:** subsumed for every machine this touches. Phase 1
  alone resolves it (its preferred fix option (a)); this doc generalizes it into
  a correctness plan rather than a debugger-only patch. `ifs0` can be closed
  when Phase 1 completes.
- **`graphics-consolidation.md`:** its enriched per-scanline helpers (2a) make
  Tier A direct-RGB migration mechanical; its indexed+priority helpers (2b) are
  a prerequisite for Tier A priority machines (Phase 3). Sequence this doc's
  Phase 2/3 after the corresponding graphics phases.
- **`headless-debugging.md`:** the trace/watchpoint tooling is the classification
  instrument (Phase 0) and its first concrete correctness payoff.
- **Rotation plan (separate):** unaffected — migration preserves each machine's
  final orientation; per-scanline machines already rotate the persistent buffer
  in `render_frame`.
- **Deferred dirty-tracking component:** if per-scanline cost ever shows up in a
  profile, that component (rendering into the persistent scanline/indexed
  buffer) is the mitigation — another reason 2b's indexed buffers are the right
  substrate.
