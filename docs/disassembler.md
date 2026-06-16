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
- `--count <n>` — stop after `n` instructions.

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
# List the regions a machine exposes
disasm machine --machine mariobros ~/mame/roms
#   disasm regions for 'mariobros':
#     main     z80    org 0x0000
#     sound    i8035  org 0x0000

# Disassemble the I8035 sound program
disasm machine --machine mariobros --region sound ~/mame/roms --count 20

# Disassemble the Z80 game program
disasm machine --machine mariobros --region main ~/mame/roms
```

`<path>` resolves the same way the frontend does: a `.zip` file directly, a
directory containing `<machine>.zip`, or a directory of loose ROM files.

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
machines that register regions appear there (Mario Bros is seeded). To expose a
new machine's code ROMs, add one `inventory::submit!` per region next to the
machine's ROM definitions — see the `DisasmRegion` entries in
[`machines/src/mario_bros.rs`](../machines/src/mario_bros.rs) and the note in
[`machines/CLAUDE.md`](../machines/CLAUDE.md). Until then, `raw` and `rom` modes
still work for any ROM with an explicit `--cpu`.
