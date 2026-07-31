# phosphor-script

A headless [Rhai](https://rhai.rs) script runner for driving and inspecting
phosphor machines — the phosphor analogue of MAME's Lua, built on the shared
`phosphor-harness` boot harness and the `core` debug traits.

Rhai is pure Rust (no C dependency), deterministic, and sandboxed (no ambient
time/RNG, built-in operation limits), which keeps the emulator's replay honesty
intact. The trade-off — no MAME-script portability — is accepted.

The crate is split into a **library** (the engine builder + bindings, reusable
by a future in-frontend console) and this **binary**.

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

v1 is **read-first**: a script can observe machine state and drive inputs, but
not poke memory/registers (pokes, watchpoints, event-trace, save-state, and DIP
editing are deferred to v2). Every method maps 1:1 onto a `DebugSession`
accessor.

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
| `m.input(name, on)` | `input` | `()` — immediate button edge, by stable control name |
| `m.input_axis(name, v)` | `input_axis` | `()` — immediate analog value (`-1.0..=1.0`) |
| `m.screenshot(path)` | `screenshot` | `()` — render current frame to an RGB PNG. Throws on error |
| `m.frame_count()` | `frame_count` | `int` — frames run so far |
| `m.id()` | `machine_id` | `String` — machine's short id |
| `m.display_size()` | `display_size` | `[int; 2]` — native `[width, height]` |

`input`/`input_axis` take a machine's **stable control name** (e.g. galaga's
`coin1`, `p1_start`); see a machine's `input_controls()` for the list. Unknown
names are ignored.

### Determinism & safety

- No ambient time or RNG — Rhai's default engine exposes neither, and the
  bindings add none.
- Runaway guards: the engine caps total operations and call-nesting depth, so an
  infinite loop or unbounded recursion always terminates.
- Read-first ⇒ replay-honest by construction: a script can only observe and
  drive legitimate inputs.

## Examples

- [`examples/capture.rhai`](examples/capture.rhai) — a `frameshot` equivalent
  (run frames, screenshot, print PC); the basis for end-to-end validation
  against `disasm frameshot`.
- [`examples/coin_start.rhai`](examples/coin_start.rhai) — an imperative input
  timeline that generalizes the hard-coded
  `machines/examples/asteroid_capture.rs`.
</content>
