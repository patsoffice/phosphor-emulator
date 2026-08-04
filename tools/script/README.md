# phosphor-script

A headless [Rhai](https://rhai.rs) script runner for driving and inspecting
phosphor machines — the phosphor analogue of MAME's Lua, built on the shared
`phosphor-harness` boot harness and the `core` debug traits.

Rhai is pure Rust (no C dependency), deterministic, and sandboxed (no ambient
time/RNG, built-in operation limits), which keeps the emulator's replay honesty
intact. The trade-off — no MAME-script portability — is accepted.

The crate is split into a **library** (the engine builder + bindings, also
embedded by the in-frontend console) and this **binary**.

## Usage

```
phosphor-script run <script.rhai> --machine <name> <rompath>
phosphor-script run <script.rhai>          # no pre-bound m; the script calls open(...) itself
```

With `--machine <name> <rompath>`, a machine is booted and pre-bound as the
global `m`. Without them, the script opens its own machine(s) via `open(...)` —
which also lets one script open several machines for in-repo A/B comparisons.

```bash
# Snapshot galaga after 3100 frames (writes out.png in the current directory)
cargo run -p phosphor-script -- run tools/script/examples/capture.rhai \
    --machine galaga ~/ws/mame-runtime/roms

# Drive an imperative coin/start timeline
cargo run -p phosphor-script -- run tools/script/examples/coin_start.rhai \
    --machine galaga ~/ws/mame-runtime/roms
```

The script's `print`/`debug` output and any result go to stdout; errors go to
stderr and the process exits non-zero.

## API

The surface observes and drives a machine: memory read/poke, CPU pc/regs,
disassemble, run/step, inputs by stable name, screenshot, watchpoints, event
trace, hang detection, save/load state, and DIP editing (all below). Every
method maps 1:1 onto a `DebugSession` accessor. The one remaining gap is
**register** pokes (memory pokes exist); see the source crate's issues.

### Global functions

| Rhai | Description |
|---|---|
| `open(machine_name, rom_path)` | Boot a machine and return a `Machine` handle. Throws on failure. |

### `Machine` methods (`m`)

| Rhai | DebugSession | Returns |
|---|---|---|
| `m.run_frames(n)` | `run_frames` | `()` — advance `n` whole frames |
| `m.step()` | `step` | `int` — advance one cycle; bitmask of CPUs at an instruction boundary |
| `m.read(cpu, addr)` | `read` | `int` — byte value, or `-1` if unmapped / no debug support |
| `m.pc(cpu)` | `pc` | `int` — program counter, or `-1` if none |
| `m.regs(cpu)` | `regs` | `Map` — register name → value |
| `m.disasm(cpu, addr)` | `disasm` | `String` — one instruction (empty if no debug support) |
| `m.poke(cpu, addr, value)` | `poke` | `bool` — write a debug byte; `false` if the machine has no debug support |
| `m.input(name, on)` | `input` | `()` — immediate button edge, by stable control name |
| `m.input_axis(name, v)` | `input_axis` | `()` — immediate analog value (`-1.0..=1.0`) |
| `m.input_relative(name, d)` | `input_relative` | `()` — immediate relative motion delta (trackball / spinner) |
| `m.screenshot(path)` | `screenshot` | `()` — render current frame to an RGB PNG. Throws on error |
| `m.frame_count()` | `frame_count` | `int` — frames run so far |
| `m.id()` | `machine_id` | `String` — machine's short id |
| `m.display_size()` | `display_size` | `[int; 2]` — native `[width, height]` |
| `m.cpu_count()` | `cpu_count` | `int` — number of CPUs on the debug bus |

`input`/`input_axis`/`input_relative` take a machine's **stable control name**
(e.g. galaga's `coin1`, `p1_start`); see a machine's `input_controls()` for the
list. Unknown names are ignored.

Pick the one the machine actually consumes. Trackball and spinner games (Marble
Madness, Crystal Castles, Missile Command, Quantum, Tempest) accumulate
`input_relative` deltas into a wrapping counter and ignore `input_axis`
entirely; self-centering sticks and yokes (Star Wars, I Robot) take
`input_axis`. Relative deltas accumulate until the machine drains them, so
sustained motion is one call per frame:

```rhai
for i in 0..60 { m.input_relative("p1_trackball_x", 3.0); m.run_frames(1); }
```

`poke` writes to backed RAM; I/O and unmapped addresses are ignored (as a
memory-viewer poke would be). It is an explicit *debug* write, distinct from the
legitimate machine inputs `input` drives — and it records a
`DebugAccessSource::Frontend` event, so with tracing on a poke shows up in
`events()` tagged `frontend`, never masquerading as a hardware store.

### Watchpoints

Set a watchpoint on an address, run frames or step, then drain the hits. The
`kind` is `"read"`, `"write"`, or `"access"` (both).

| Rhai | Fires when | Returns |
|---|---|---|
| `m.watch(addr, kind)` | any matching access | `int` — CPUs watched |
| `m.watch_value(addr, kind, v)` | accessed value `== v` | `int` |
| `m.watch_changed(addr, kind)` | value differs from last | `int` |
| `m.watch_bits(addr, kind, mask, expected)` | `(value & mask) == expected` | `int` |
| `m.watch_cpu(cpu, addr, kind)` | any matching access, one CPU | `()` |
| `m.clear_watchpoints()` | — | `()` |
| `m.hits()` | — | `[Map]` — drains the collected hits |

Each hit is a map: `cpu`, `addr`, `kind`, `value`, `width`, `pc` (`-1` if
unknown), `cycle`, `source`, `region`.

**Watch all CPUs by default.** `watch*` set on *every* CPU, because watchpoints
are scoped per CPU and on multi-CPU boards a video/scroll register is often
written by a *sub*-CPU, not the main one — a single-CPU watch would silently
catch nothing. (On galaga, `0x9100` is written mostly by CPU 1, not CPU 0.) Each
hit's `cpu` field says which CPU fired; use `watch_cpu` to target one
deliberately. `hits()` accumulates across a whole run (hits are drained after
each frame/step so a hot address doesn't overflow the machine's 64-entry queue);
a single frame can still drop hits past 64, so `step()` gives exact capture.

```rhai
m.run_frames(3100);
m.watch(0x9100, "write");
m.run_frames(600);
let hits = m.hits();
print(hits.len() + " writes");            // assert on the count to fail loudly on zero
for h in hits { print("cpu" + h.cpu + " wrote " + h.value); }
```

### Event trace

The bus event trace is **CPU-agnostic**, **region-tagged**, and **mirror-resolving**
— strictly better than watchpoints for "which registers are written, and when
within a frame". (Satan's Hollow writes its palette through a VRAM mirror; an
address-exact watchpoint on the un-mirrored base would miss it, but the trace
records it against the resolved region.)

| Rhai | Returns |
|---|---|
| `m.trace(on)` | `()` — enable/disable recording (enabling starts clean) |
| `m.trace_enabled()` | `bool` |
| `m.events()` | `[Map]` — drains the collected events |

Each event is a map: `cycle`, `kind` (`"mem wr"`, `"io wr"`, `"dev wr"`, …),
`source`, `cpu` (`-1` if n/a), `pc`, `addr`, `value`, `width`, `region`,
`device`, `detail`. Like `hits()`, events accumulate across a run (drained after
each frame/step) and `events()` drains them. Bus-event tracing is wired for
`AddressSpace16/32` boards; a machine without it records nothing.

```rhai
m.run_frames(400);
m.trace(true);
m.run_frames(1);                          // capture exactly one frame's writes
for e in m.events() {
    if e.kind == "mem wr" { print(e.region + " <- " + e.value + " @cycle " + e.cycle); }
}
```

### Hang detection

Per-frame PC sampling that flags a CPU stuck in a tight loop (an idle/boot
hang). `detect_hangs()` uses the Dig Dug defaults (8-byte window, 120-frame
threshold); `detect_hangs(window, threshold)` overrides them.

| Rhai | Returns |
|---|---|
| `m.detect_hangs()` / `m.detect_hangs(window, threshold)` | `()` — enable |
| `m.hangs()` | `[Map]` — drains reports (`cpu`, `pc`, `window_lo`, `window_hi`, `frames_stuck`) |

`run_frames` samples every CPU once per frame; a CPU whose PC stays within
`window` bytes for `threshold` frames reports once, then stays quiet until it
moves on.

### Save state & reset

Snapshot machine state and restore it later — branch a run, or make a long
scripted run cheap to re-enter without replaying from reset. The snapshot is a
`Blob` the script holds in a variable.

| Rhai | Returns |
|---|---|
| `m.save_state()` | `Blob` — a snapshot (throws if the machine has no save-state support) |
| `m.load_state(blob)` | `()` — restore a snapshot (throws on a bad/incompatible blob) |
| `m.reset()` | `()` — power-on reset; zeroes the frame counter and clears the watchpoint/event/hang accumulators |

```rhai
m.run_frames(3100);
let checkpoint = m.save_state();
m.input("coin1", true); m.run_frames(8); m.input("coin1", false);
m.run_frames(600);
m.load_state(checkpoint);        // back to the attract screen, no replay
```

### DIP switches

Read and edit DIP configuration — sweep coinage, lives, difficulty, or bonus
thresholds without recompiling or hand-editing NVRAM.

| Rhai | Returns |
|---|---|
| `m.dip_banks()` | `[Map]` — bank metadata (see below) |
| `m.dip_bank(bank)` | `int` — live byte of a bank |
| `m.set_dip_bank(bank, value)` | `()` — replace a bank's whole byte |
| `m.set_dip_option(bank, option, value)` | `()` — set one option (masked into the byte) |
| `m.set_dip(option_name, choice_label)` | `bool` — set by name; `false` if not found |

`dip_banks()` returns `{ name, options: [{ name, mask, apply, choices:
[{ label, value }] }] }`; `apply` is `"immediate"` or `"on_reset"` (an
`on_reset` option only takes effect after `m.reset()`). `set_dip` is the
ergonomic path — `m.set_dip("Difficulty", "Hard")` — resolving the option and
choice from that metadata.

### Determinism & safety

- No ambient time or RNG — Rhai's default engine exposes neither, and the
  bindings add none.
- Runaway guards: the engine caps total operations and call-nesting depth, so an
  infinite loop or unbounded recursion always terminates.
- Read-first at heart: most of the surface observes and drives legitimate
  inputs. The writes it does allow — `poke`, DIP edits, `load_state`, `reset` —
  are explicit debug operations, not disguised hardware state. A `poke` is
  tagged `DebugAccessSource::Frontend` in the event trace, so it stays
  distinguishable from a hardware write.

## Examples

- [`examples/capture.rhai`](examples/capture.rhai) — a `frameshot` equivalent
  (run frames, screenshot, print PC); the basis for end-to-end validation
  against `disasm frameshot`.
- [`examples/coin_start.rhai`](examples/coin_start.rhai) — an imperative input
  timeline that generalizes the hard-coded
  `machines/examples/asteroid_capture.rs`.

## Packaging

`phosphor-script` ships as its **own binary** rather than as a subcommand of the
`disasm` tool (e.g. `disasm script run …`). Both were viable — the engine lives
in this crate's **library**, so either binary is a thin shell over it — and the
choice is deliberately reversible (epic decision deferred to build time).

Kept separate because:

- Scripting is a distinct capability — programmatically *driving* a machine —
  versus what `disasm` does: disassembly plus one-shot *analysis/capture*
  (`frameshot`, `trace`, `imgdiff`, `gfxview`, `rom`). A tool named for what it
  does is more discoverable than a ninth subcommand under `disasm`.
- `disasm` already carries eight subcommands; folding scripting in would grow an
  already-broad CLI.
- The real reuse point is the **library**, not this binary: the in-frontend
  console (`frontend/src/console_ui.rs`) embeds the engine builder directly, and
  a second binary entry point adds no leverage there.

To reverse this later (if usage shows people want scripting where they already
run `trace`/`frameshot`), add a `script` subcommand to `tools/disasm` that
delegates to the library — the whole body is roughly:

```rust
// disasm script run <script.rhai> [--machine <name> <rompath>]
let engine = phosphor_script::build_engine();
let mut scope = rhai::Scope::new();
if let (Some(name), Some(path)) = (machine, rompath) {
    scope.push("m", phosphor_script::open_machine(name, path)?);
}
engine.run_with_scope(&mut scope, &std::fs::read_to_string(script)?)?;
```

The standalone binary can stay or be retired at that point.
