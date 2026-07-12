# Standalone Disassembler (`disasm`)

`disasm` is a command-line tool that disassembles a ROM with any of the per-CPU
disassemblers in `phosphor-core`, without launching the SDL/egui debug UI. It's
the quickest way to read a sound or CPU ROM from the terminal — for example, the
Mario Bros 8049 sound program — and to diff against MAME or a datasheet while
hunting a CPU/sound bug.

It lives in [`tools/disasm`](../tools/disasm) as the `phosphor-disasm` crate and
depends only on `phosphor-core` and `phosphor-machines` (no SDL2/C toolchain).

## Building and running

```bash
# Build just the tool
cargo build -p phosphor-disasm

# Run via cargo (note the `--` separating cargo args from disasm args)
cargo run -p phosphor-disasm --bin disasm -- <MODE> ...

# Or invoke the built binary directly
target/debug/disasm <MODE> ...
```

Use `disasm --help` (and `disasm <mode> --help`) for the full option list.

## Supported CPUs

`--cpu` accepts: `i8035`, `z80`, `m6809`, `m6800`, `m6502`, `m68000`, `mb88xx`.
These map to the disassemblers in `core/src/cpu/*/disasm.rs`.

## Modes

### `raw` — a raw, extracted ROM file

You already have the exact ROM bytes on disk and know the CPU and load address.

```bash
disasm raw --cpu i8035 --org 0 sound.rom
disasm raw --cpu z80 --org 0x0000 --count 40 program.bin
```

- `--org <addr>` — address of the first byte (hex `0x..` or decimal; default `0`).
  Used for branch/jump target resolution.

All modes also accept the range options below.

### `rom` — a member file of a ROM set

Point at a `.zip` archive or a directory of loose ROM files and name the member
to disassemble — no manual extraction needed. The CPU is still explicit.

```bash
disasm rom --cpu i8035 ~/mame/roms/mario.zip tma1-c-6k_e.6k --count 8
```

If the member name is wrong, the tool lists the files actually present in the set.

### `machine` — a known machine's code region

The most convenient mode: name a machine and a region, and the CPU + origin are
resolved automatically from the machine's registered disasm regions. Point at the
same rompath/zip you'd pass to the emulator.

```bash
# List the regions a machine exposes (no ROM path needed)
disasm machine --machine mariobros
#   disasm regions for 'mariobros':
#     main     z80    org 0x0000  24576 bytes (0x6000)
#     sound    i8035  org 0x0000  4096 bytes (0x1000)

# Disassemble the I8035 sound program
disasm machine --machine mariobros --region sound ~/mame/roms --count 20

# Disassemble the Z80 game program
disasm machine --machine mariobros --region main ~/mame/roms
```

Listing the regions (omit `--region`) needs no ROM files; a `<path>` is only
required to disassemble a region. `<path>` resolves the same way the frontend
does: a `.zip` file directly, a directory containing `<machine>.zip`, or a
directory of loose ROM files.

## Selecting a range

By default the whole region is disassembled. These options (available in every
mode) narrow the output and compose with each other:

- `--start <addr>` — begin at this address instead of the origin.
- `--end <addr>` — stop at this address (exclusive). An instruction straddling
  the boundary is still printed.
- `--count <n>` — stop after `n` instructions.

Addresses are absolute — the same space as `--org` / the region origin.

```bash
# Just the routine at 0x0146-0x01FF of the Mario sound ROM
disasm machine --machine mariobros --region sound ~/mame/roms --start 0x0146 --end 0x0200

# 40 instructions from a fixed point
disasm raw --cpu z80 --org 0 program.bin --start 0x0130 --count 40
```

## Banked ROM

Bank switching (e.g. Crystal Castles' and Williams' program ROM) is a *runtime*
operation: the game writes a bank register to repoint an address window at a
different ROM. The standalone tool has no runtime state, so it can't know which
bank is live — the live debug UI is the bank-aware option there, since it reads
through the bus.

Instead, each bank is registered as its own region. Crystal Castles, for
example, exposes both banks (which share the `0xA000-0xDFFF` window) plus the
fixed ROM:

```bash
disasm machine --machine ccastles
#   disasm regions for 'ccastles':
#     bank0    m6502  org 0xA000  16384 bytes (0x4000)
#     bank1    m6502  org 0xA000  16384 bytes (0x4000)
#     fixed    m6502  org 0xE000  8192 bytes (0x2000)

disasm machine --machine ccastles --region bank1 ~/mame/roms
```

For a `raw`/`rom` dump of a banked image, use `--org` to set the window base and
`--start`/`--end` to carve out a single bank's bytes.

## Graphics: `gfxview`

`gfxview` decodes a machine's tile/sprite GFX ROM region into a PNG sheet, using
the same bit-plane layout and color PROM the runtime renderer uses (both come
from the [`gfx_registry`](../machines/src/gfx_registry.rs)), so the sheet is what
the machine actually draws with — a fast way to confirm a planar decode and
palette before wiring up rendering.

Omit `--region` to list a machine's registered gfx regions (no ROM path needed):

```bash
disasm gfxview --machine mrdo
#   gfx regions for 'mrdo':
#     bg         8×8     512 tiles  PROM palette
#     fg         8×8     512 tiles  PROM palette
#     sprites   16×16    128 tiles  PROM palette
```

With a region and a ROM path it writes the sheet:

```bash
# Sprite sheet, 16 per row, 3× upscaled
disasm gfxview --machine mrdo --region sprites --cols 16 --scale 3 \
    -o mrdo_sprites.png ~/mame/roms
```

- `--cols <n>` — elements per row in the sheet (default 16).
- `--scale <n>` — integer nearest-neighbor upscale (default 1).
- `-o/--out <path>` — output PNG (default `<machine>_<region>.png`).

Regions with no color PROM (e.g. Williams, which colors from RAM) fall back to a
grayscale ramp sized to the layout's bit depth.

## Video: `frameshot` and `imgdiff`

`frameshot` boots a registered machine, runs it for N frames from reset, and
writes the rendered frame to a PNG — a headless screenshot, with no SDL/egui
window. It's the fastest way to validate a machine's video output against a
reference (e.g. a MAME snapshot) in a loop.

```bash
# Boot Mr. Do! for 200 frames, dump the title screen
disasm frameshot --machine mrdo --frames 200 -o mrdo_200.png ~/mame/roms

# Compare directly against a MAME snapshot while capturing
disasm frameshot --machine mrdo --frames 3100 --compare mame_3100.png \
    -o mine_3100.png ~/mame/roms
#   wrote mrdo frame 3100 -> mine_3100.png (192×240)
#   diff vs mame_3100.png: 233/46080 (0.5%)
```

`imgdiff` compares two already-captured RGB PNGs (any size, as long as they
match), reporting the fraction of differing pixels and — with `-o` — writing a
highlight image that dims the matching pixels and paints the differences red, so
you can see *where* they disagree at a glance.

```bash
disasm imgdiff mine_3100.png mame_3100.png -o diff.png
#   diff: 233/46080 (0.51%)

# --threshold sets the per-pixel channel-sum delta that counts as "different"
disasm imgdiff a.png b.png --threshold 24
```

The `--compare`/`imgdiff` percentage is the workhorse of the render-vs-MAME
loop: capture a MAME snapshot at frame N (its Lua `emu.register_frame_done` +
`manager.machine.video:snapshot()`, headless via `SDL_VIDEODRIVER=offscreen mame
<set> -video soft`), then iterate `frameshot --compare` until the residual is
just scroll/animation phase.

## Output format

Each line is:

```text
000000  34 00                   CALL  $0100
ADDRESS HEX BYTES               MNEMONIC OPERANDS
```

- **ADDRESS** — `--org` plus the byte offset, 6 hex digits.
- **HEX BYTES** — the raw instruction bytes (`byte_len` of them).
- **MNEMONIC/OPERANDS** — the same rendering the in-app debug UI shows.

Bytes past the end of the ROM, or an undecodable opcode, render as `???` with a
one-byte step, so the listing always terminates.

## Debugging workflow

This tool exists to debug sound/CPU ROMs. A typical loop (the Mario Bros sound
bug that motivated it):

1. **Dump the suspect ROM.** `disasm machine --machine mariobros --region sound
   ~/mame/roms > sound.lst`.
2. **Diff against ground truth.** Compare `sound.lst` to MAME's debugger
   disassembly (`mame mario -debug`, then `dasm`) or a datasheet. Divergence in
   mnemonics/operands points at a disassembler bug; divergence in *behavior* with
   matching disassembly points at an execution bug.
3. **Cross-reference addresses.** Because `--org` resolves branch/jump targets,
   you can follow `CALL`/`JMP` targets to the routine of interest and set a
   breakpoint there in the live debug UI (F1 → run to address).
4. **Re-dump after a fix** and confirm the bytes/decoding line up.

Because `disasm` shares the exact disassemblers used by the debug UI, what you
read here is what the emulator decodes at runtime.

## Adding a machine to `machine` mode

`machine` mode is backed by the disasm registry in
[`machines/src/disasm_registry.rs`](../machines/src/disasm_registry.rs). Only
machines that register regions appear there (Mario Bros and Crystal Castles are
seeded). To expose a new machine's code ROMs, add one `inventory::submit!` per
region next to the machine's ROM definitions — see the flat `DisasmRegion`
entries in [`machines/src/mario_bros.rs`](../machines/src/mario_bros.rs), the
region-per-bank entries in
[`machines/src/ccastles.rs`](../machines/src/ccastles.rs), and the note in
[`machines/CLAUDE.md`](../machines/CLAUDE.md). For banked ROM, register one
region per bank (same `org`, a `load` closure that slices that bank, and a
distinct name). Until a machine is registered, `raw` and `rom` modes still work
for any ROM with an explicit `--cpu`.
