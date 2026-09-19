# CRT rendering for the raster machines

Tracked as `phosphor-emulator-21w8`. The vector machines model a beam: spot size
from the tube, brightness from dwell, halation from the faceplate. The 33 raster
machines draw flat pixel grids. They ran on the same family of tube, and the
figures to do the same for them were already in the tree.

This document records what the tube geometry decides, what was decided by
judgment instead, and why the performance work that was filed under this epic
was closed without being done.

## The figures everything comes from

`phosphor_core::device::dvg` carries the two that matter, measured off a 19 inch
arcade tube:

| Figure | Value | Source |
|---|---|---|
| Focused spot | 0.7 mm | `BEAM_SPOT_MM` |
| Tube long axis | 360 mm | `TUBE_LONG_AXIS_MM` |
| Tube short axis | 270 mm | the long axis at 4:3 |
| Shadow mask pitch | ~0.6 mm | typical for the size |

The short axis carries the lines and the long axis carries the sweep, whatever
the cabinet does with the tube afterwards. A vertical cabinet rotates the whole
assembly, so a rotated game's scanlines run across the *presented* image
horizontally. Deriving anything from the oriented framebuffer rather than from
the native one puts the spacing on the wrong axis.

## Scanlines are derivable, and separate exactly one machine

Line pitch is the short axis over the line count, against the 0.7 mm spot:

| Lines | Machines | Pitch | Spot covers |
|---|---|---|---|
| 192 | docastle, dorunrun, dowild, mrdo | 1.406 mm | 49.8% |
| 224 | 17 machines (pacman, galaga, dkong, xevious, ...) | 1.205 mm | 58.1% |
| 231 | missile | 1.169 mm | 59.9% |
| 232 | ccastles, irobot | 1.164 mm | 60.1% |
| 240 | 8 machines (joust, marble, qbert, robotron, ...) | 1.125 mm | 62.2% |
| 480 | shollow | 0.562 mm | 124.4% |

Both halves of the prediction hold. Low line counts leave real gaps between
lines, and MCR2 at 480 lines overlaps and gets no gaps at all. That falls out of
the arithmetic rather than being a special case, which is the point: a shader
applying scanlines uniformly would be wrong for shollow.

**The model separates one machine from the other 32.** Coverage across the whole
library spans 49.8% to 62.2%, and then shollow. The derivation is still the right
construction, because gap width is then a consequence of the tube rather than a
number someone dialed in, but it buys one binary split and not a spectrum. Do not
build per-machine machinery whose cost only pays back against a wider spread than
this.

### The trap in the input data

`sinistar` reports `display_size` 240x292 with `swaps_axes()` false. It bakes its
rotation into `render_frame` and reports the already-rotated size, which the
`display_size` doc comment in `core::machine` describes. **Its line count is 240,
not 292.** It is the only raster machine for which "height is the line count" is
false, and it is the tube-versus-image confusion sitting in the data that any
implementation reads.

## The shadow mask is below the pixel grid everywhere

One emulated pixel is the long axis over the horizontal pixel count, against the
0.6 mm mask pitch:

| Sweep pixels | Machines | Pixel width | Triads per pixel |
|---|---|---|---|
| 240 | burgertime, docastle, dorunrun, dowild, mrdo | 1.500 mm | 2.50 |
| 256 | galaxian, dkong, foodf, ccastles, qbert, ... | 1.406 mm | 2.34 |
| 288 | pacman, mspacman, galaga, digdug, xevious | 1.250 mm | 2.08 |
| 292 | joust, robotron, sinistar | 1.233 mm | 2.05 |
| 336 | marble, roadrunner | 1.071 mm | 1.79 |
| 512 | shollow | 0.703 mm | 1.17 |

Never below 1, so no board can resolve stripes. The RGB stripe look that CRT
shaders usually reproduce comes from PC monitors at 640x480 and up, where a pixel
is roughly one triad. At arcade resolutions the mask sits below the pixel grid
and reads as slight softening. Building visible stripes would be building a look
these cabinets never had.

shollow is again the near case at 1.17 triads per pixel, and even there a mask
would alias into moire rather than reproduce stripes. If a higher-resolution board
is ever added, this is the calculation to redo rather than assume.

## Does the monitor manufacturer matter?

Mostly no, and the code already says why. Both derivations are ratios, so tube
size cancels: spot size and mask pitch both scale roughly with the tube, and a
25 inch monitor running 224 lines has a proportionally larger pitch and a larger
spot and lands near the same coverage. `dvg.rs` is built this way already. What
gets consumed downstream is `BEAM_SPOT_FRACTION`, the spot over the long axis,
and `halation_sigma_units` takes the long axis as a parameter. The millimeters
are scaffolding for deriving a fraction.

What does not cancel is focus quality, which is where chassis and manufacturers
genuinely differ. A Wells-Gardner and an Electrohome driving the same board are
not the same picture, and neither is the same chassis before and after someone
adjusts it. That belongs on the `focus` knob, which is already documented as a
multiple of the tube's measured spot with 1.0 being a well adjusted 19 inch, and
it argues against a per-machine monitor table: the same title shipped on
different monitors depending on cabinet, region, kit conversion and whatever the
operator had in the shop. Monitor is a property of a cabinet, not of a game.

It does move one conclusion's confidence, and asymmetrically.

- The 32 machines showing gaps is robust. Closing the gaps on a 224-line board
  needs a 1.20 mm spot, 1.7 times the assumed figure, which is a badly defocused
  monitor and is reachable with the `focus` knob if someone wants it.
- **shollow being gap-free is marginal.** It needs the spot under 0.5625 mm to
  start showing gaps, only 20 percent tighter than assumed, which a well focused
  tube plausibly reaches.

So shollow is the marginal case in both derivations and in opposite directions:
about 25 percent of margin on the scanline side, about 17 percent on the mask
side, where 1.17 triads per pixel reaches 1.0 at a 0.70 mm mask pitch. The one
machine the per-machine derivation exists to separate is the one machine whose
answers a plausible monitor swap could flip. That does not change what gets
built, since deriving per machine still beats a uniform shader and the knob
covers the spread, but it is why the shollow notes are worth keeping rather than
rounding off.

## Color overlays are the exception to "monitor is not a property of a game"

The paragraph above rules out a per-machine monitor table, and that ruling
stands. A color overlay is not a counterexample to it, because it is not the
monitor.

The overlay is a sheet of colored plastic in front of the glass, and it is how a
monochrome board shipped a color game before color tubes were cheap. Asteroids
Deluxe draws in white exactly as Asteroids does, and every blue-green thing a
player ever saw on one was the sheet. That makes it per game in the way a
chassis is not: the sheet was cut for that game's artwork and shipped in that
game's cabinet, and a kit conversion that swapped the boards swapped the sheet
with them. There is no equivalent of "whatever the operator had in the shop",
because a Lunar Lander gel in an Asteroids Deluxe cabinet is not a different
picture, it is the wrong game's picture.

So the two facts sit in different places for the same reason: focus quality
varies per cabinet and belongs on a knob, and a gel does not vary at all and
belongs in a file keyed by the machine. `frontend/overlays/*.toml` holds them,
in the reference emulator's vocabulary (regions with normalized bounds and a
color, multiplied over the picture) and its format changed from XML to TOML to
match the rest of this tree's data.

Three things follow, and the second is the one worth remembering.

- **It composites inside the beam shaders, not after them.** The CRT stage's
  intermediates are `RGBA16F` and deliberately carry values above 1.0; only the
  last write into egui's texture clips to eight bits. Light is attenuated on its
  way out of the tube, so a highlight the display cannot show still gives up the
  same fraction of its red, and it gives it up before the clip rather than
  after. It also lands the tint exactly once on both the direct light and the
  halation skirt for nothing, since the skirt is built from the already-tinted
  core and a constant per-channel factor commutes with a blur.
- **The numbers are transmissions of displayed value, not of radiance, and
  converting them to linear light without re-deriving them is a bug that will
  look like a fix.** They were chosen by eye against a compositor that
  multiplies encoded color. Asteroids Deluxe's 0.5333 decodes to about 0.25 in
  linear light, so moving the space alone turns a cyan sheet into a barely
  tinted one. Either both move or neither does.
- **The golden pins never see it**, by the same argument as the CRT stage below:
  the sheet is not drawn by the board. Adding an overlay to a machine cannot
  move a hash. The interactive screenshot does apply it, because that one is a
  picture of what the player was looking at; `--headless` deliberately does not,
  because that one exists to be diffed against a pin.

## The decisions

Three forks the epic deliberately left open, resolved with the requester on
2026-09-12.

### 1. The GPU owns the effects, and screenshots read back from it

The CRT stage is a shader between the uploaded native texture and egui. `F12`
changes from saving the CPU RGB24 buffer to reading back the effect FBO, so a
screenshot shows what shipped.

The consequence that decided it: **golden frames capture `render_frame` plus
orientation** (`harness/src/frame.rs`), not the GL path. A presentation-stage
effect is invisible to them, so the 39 pins do not churn. The epic was filed
believing any CRT effect moves all of them at once, which is true only of an
effect in the CPU render path. The pins go on testing what the board draws,
independently of what the tube does to it, and that separation is worth having on
its own.

The rejected alternative was putting the effects in `render_frame` so every
consumer agrees. It costs 32 recaptures up front and pays an O(pixels) cost on
every machine on every frame, against the vector path's measurement of a
full-frame blur at four to five times the beam sweep itself.

### 2. The stage renders at the presentation surface, with sigma floored

The shader runs at whatever the window and FBO actually are, and floors the spot
sigma where that grid cannot represent it. `machines/src/atari_dvg.rs` already
does exactly this against the ripple bound `2*exp(-2*pi^2*sigma^2)`, which is
brightness varying with position, and the reasoning and probably the constants
carry over.

This avoids choosing an upscale factor, which would have been a second underived
number. A screenshot's resolution follows the window size, which is the accepted
cost.

It also largely dissolves `21w8.2`. That issue exists because a 288x224
framebuffer has nowhere to put a scanline, by analogy with `vector_field_size`
splitting coordinate extent from rasterization grid. On the GPU the output grid
is the window, which already exists, so there is no conflation left to split.
What survives is the check the issue asks for: confirm nothing else derives from
`display_size` on a raster board before treating it as native-only.

### 3. The CPU path gets nothing

See the next section.

## The CPU rasterizer performance record

`21w8.7` (f32 accumulator to 8.8 fixed point), `21w8.8` (fuse the sweep and the
convert), and `21w8.9` (SIMD) were filed under this epic but are about the *vector*
CPU rasterizer, not the raster path. All three were closed without being done.

### Why, and why not for the obvious reason

The obvious reason is wrong. The CPU vector rasterizer is **not** dead code under
decision 1: `frontend/src/emulator.rs` falls back to it whenever any debug,
profiler, settings or console panel is open, because the side panels need a
texture for layout. It runs interactively on exactly the path used when debugging
a vector machine.

It is closed because that path has the headroom. All four vector machines call
`display_settings().without_halation()` in `render_frame`, so the live fallback
pays the beam sweep at roughly 1.4 ms on Tempest and not the 13.5 ms blur. That is
about 8% of a 60 Hz budget.

### The measurement, which is the part worth keeping

From optimizing the vector rasterizer on 2026-08-27. Two changes that should have
been large if the loops were ALU-bound were not:

```
first cut                                          4.934 ms/frame (tempest)
window each row to the segment stadium             1.509   (algorithmic)
replace about 2.2M exp() calls with a lookup table 1.320   (-0.19)
reuse the accumulator instead of a 4 MB alloc      1.362   (noise, no gain)
```

Removing 2.2 million transcendental calls bought 0.19 ms and removing a per-frame
4 MB allocation bought nothing measurable, while the two algorithmic changes
bought 3.4 ms and 3.9 ms. The Asteroids accumulator is 1024 x 1024 x 3 f32 =
12.6 MB and a frame traverses it two to three times, which is 25 to 38 MB of
traffic per frame.

**"Memory-bound" is an inference from those two measurements and from the buffer
size. It was never profiled, and with all three issues closed it now never will
be.** Do not cite it as established. Anyone reviving this work confirms it first,
because if it is wrong all three were wrong together.

### The reason 21w8.7 would have been wrong anyway

8.8 fixed point in `u16` tops out at 255.996, and the accumulator's headroom
*above* full scale is load-bearing. The brightness control is set so a
full-intensity vector at the beam's top speed already reads full white, and more
dwell past that only saturates; that is the display blooming, and it is correct.
The clamp to 255 happens once at quantization, and **the halation composite reads
the accumulator before that clamp.** Saturating the accumulator itself would
quietly under-feed the halo from exactly the bloomed regions that should glow
most.

None of the four invariants the issue listed as its safety net would catch it,
because all four sit at or below full scale: a full-intensity vector peaks at full
white, equal-length segments emit equal light at any angle, splitting a vector
conserves its light, and halation puts light beyond the core reach without
creating any.

### If any of it is ever revived

`21w8.8` first. Removing a redundant full read of 12.6 MB is defensible as
structure and not only as speed, and unlike the accumulator width it risks no
precision. It is a partial fuse: the halation composite reads a reduced-resolution
field with per-column bilinear taps and cannot ride along with the sweep.

`21w8.9` last or never. It needs a NEON path beside any SSE2 one, since
`aarch64-apple-darwin` is a supported target and `std::simd` is nightly while the
flake pins stable.

## The better fix, which is filed separately

The CPU fallback exists only because egui needs a texture for panel layout. Under
decision 1 the raster CRT stage renders into an FBO, and an FBO is a texture. The
same mechanism lets the GL vector renderer draw into one and hand it to egui,
which removes the CPU fallback from the live path entirely and gives the debug
view the real renderer instead of a second-best one. It shares its machinery with
the halation stage below.

## What the halation stage inherits

`frontend/src/vector_gl.rs` already owns a ping-pong FBO pair, a separable blur
program and a composite program, driven by `halation_sigma_units()`, which derives
the width from the faceplate critical angle and applies unchanged to a raster
tube. A raster halation stage differs only in what fills the first FBO.

Two things carry over with it. Composite the halo as light taken from the core
rather than added to it, and raise the brightness gain to hold the peak. Note that
those two facts together mean total emitted light rises with the halation
fraction: peak-at-full-white and total-light-conserved cannot both hold while the
fraction varies, and a test asserting both shipped once and had to be corrected.

The halating fraction remains the one figure with no derivation behind it. It sits
on the display panel at 0.07, set by eye. Expect no second such figure; two would
mean a missing mechanism.
