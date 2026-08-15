# Design: Debugging Improvements — Headless Tooling & Debugger Usability

> **Status: implemented.** All five features shipped; tracked in beads epic
> `phosphor-emulator-headless-debugging-tvom`. Where the text says
> `TraceHarness` in `tools/disasm/src/harness.rs`, the shipped type is
> `Harness` in the `phosphor-harness` crate (`harness/src/harness.rs`) — it
> was promoted out of the disasm tool so the frontend console and the script
> crate could share it.
>
> This builds on the completed `debug-observability`
> epic (event tracing, rich watchpoints, peek semantics — see
> [`debug-observability.md`](debug-observability.md)) and the `addr32` u32
> debug-surface widening. Two halves: Features 1–4 surface the machinery the
> interactive debugger already has to *headless* workflows (closing gaps that
> recurring bug hunts keep hitting); Feature 5 is usability polish on the
> *interactive* debugger UI and the disasm CLI. The two halves share types
> (event/watchpoint models, trace filters) but are independently shippable.

## Context

The interactive debugger is mature. `MachineDebug` (stepping, watchpoints),
`DebugTrace` (a 4096-event ring adopted by ~40 machines), `DebugCpu`
(`debug_pc`/`debug_disassemble`/`debug_at_instruction_boundary`), and
`BusDebug` (CPU/device discovery, `read`/`peek`) are all live and exercised by
`frontend/src/debug_ui.rs`.

**None of it is reachable without the SDL frontend.** Every hard-bug war story
in `docs/` was cracked with throwaway, hand-rolled instrumentation instead:

- **Dig Dug hang** (`debugging-digdug-hang.md`): a bespoke PC-sampling hang
  detector (8-byte window, 120-frame threshold), per-frame `eprintln` dumps of
  06XX state and game RAM, and manual "watch this address, print the writer's
  PC" tracing.
- **ESB watchdog reset** (`br` issue `phosphor-emulator-j63p`): traced
  *instruction-by-instruction* by hand to find a RAM write loop overrunning the
  stack with `0x4e5f`; the fix needs "break when `0x4e5f` is written" and a
  readable execution trace — neither exists headless.
- **Xevious runaway** (`phosphor-emulator-xevious-8u2a.1.5`): solved via an
  ad-hoc out-of-tree MAME Lua "bus-diff".

Meanwhile the project *already* has excellent headless rigs for the other two
observability domains: `disasm frameshot`/`imgdiff` for **video** and
`tools/sound-reference/` for **audio**. There is no equivalent for **CPU/bus
state** — the one domain where the machinery to build it is already sitting in
`phosphor-core`.

This doc adds that missing rig and the two small core features the war stories
keep needing.

## Scope

Five workstreams, ordered by leverage:

1. **Headless trace/inspect CLI** — boot a registered machine, run N frames,
   and dump the event ring, watchpoint hits, and register/memory snapshots.
   *(mostly harness code; reuses existing traits)*
2. **Instruction-level execution trace** — a MAME-`-trace`-equivalent PC/opcode
   log per CPU. *(pure harness; no per-board instrumentation)*
3. **Conditional / value-match watchpoints** — "break on write of value
   `0x4e5f`", "break when `$8423` changes". *(the one real core-type change)*
4. **First-class hang / idle-loop detection** — promote the hand-rolled Dig Dug
   detector into a reusable core util + a CLI flag. *(small core util + harness)*
5. **Interactive debugger usability (UI + CLI)** — fix the friction, hidden
   couplings, and silent-failure papercuts in the egui debug panel and the
   disasm CLI. *(frontend + CLI polish; a couple of shared types with 1–4)*

Out of scope: reference-comparison-as-CI (bus/frame/audio diffing gated on
MAME) — deliberately deferred.

## Current Architecture (the surface we build on)

Verified code points:

- `core/src/core/machine.rs`
  - `trait MachineDebug` — `debug_bus`/`debug_bus_mut() -> Option<&dyn BusDebug>`,
    `cycles_per_frame() -> u64`, `debug_tick() -> u32` (bitmask of CPUs at an
    instruction boundary this cycle), `take_watchpoint_hit`,
    `set_watchpoint(cpu, addr, kind)`, `clear_watchpoint`, `clear_all_watchpoints`.
  - `trait FrontendMachine: MachineCore + … + MachineDebug + DebugTrace + …`
    (blanket impl); machines are `Box<dyn FrontendMachine>` built via
    `registry::find(name).create(&romset)`.
- `core/src/core/debug.rs`
  - `trait DebugCpu` — `debug_pc() -> u32`, `debug_at_instruction_boundary() -> bool`,
    `debug_disassemble(addr: u32, bytes: &[u8]) -> DisassembledInstruction`.
  - `trait BusDebug` — `cpus()`, `devices()`, `read(cpu, addr) -> Option<u8>`,
    `peek(cpu, addr) -> DebugRead`.
- `core/src/core/debug_trace.rs`
  - `DebugEvent`/`DebugEventKind`/`DebugTraceBuffer`/`trait DebugTrace`
    (`set_trace_enabled`, `trace_enabled`, `trace_events() -> &[DebugEvent]`,
    `clear_trace_events`).
- `core/src/core/watchpoint.rs`
  - `struct Watchpoint { cpu_index, addr, kind }` — **address-only, no condition.**
  - `Watchpoints::check_read/check_write(cpu, source, cycle, pc, addr, value, width)`
    — already carry `value` + `width`, so a value test is evaluable at the check
    site with no extra plumbing.
- `tools/disasm/src/main.rs`
  - `run_frameshot(...)`: the existing headless harness —
    `registry::find` → `load_rom_set(path, entry.rom_names)` →
    `(entry.create)(&set)` → `reset()` → optional `load_nvram` → per-frame coin
    scripting → `run_frame()` loop. **The new subcommand reuses this verbatim.**

The load-bearing observation: **the CLI already has every primitive to produce
an instruction trace and a hang report** (`debug_tick` + `debug_pc` +
`debug_at_instruction_boundary` + `read` + `debug_disassemble`). Those two
features need *zero* per-board work. Only conditional watchpoints touch a core
type.

## Feature 1: Headless trace/inspect CLI

### Command shape

One new flat subcommand alongside `Frameshot`/`Imgdiff`, named `trace` (it
"runs and observes" — instruction trace, event ring, watchpoints, and hang
detection are flags on the same run so a single invocation can capture all
correlated by cycle):

```
disasm trace --machine mrdo --frames 3100 [FROM] [OBSERVERS] [STOP] [OUTPUT] <path>

  --machine <name>          registered machine (as frameshot)
  --frames <N>              frames to run from reset
  --from-frame <N>          start emitting output only at/after this frame
                            (run fast to N, then observe — cheap "seek")
  --coin-at <F>             pulse coin at frame F (reused from frameshot)
  --nvram <FILE>            load factory NVRAM first (reused from frameshot)

  OBSERVERS (any combination):
  --cpu <name|idx>[,..]     instruction-trace these CPU(s) (default: none)
  --events <kind[,..]|all>  enable DebugTrace and include these event kinds
  --watch <cpu:addr:kind[:cond]>[,..]   set watchpoint(s), log every hit
  --hang                    enable hang/idle-loop detection

  STOP (optional; default: run to --frames):
  --break-pc <cpu:addr>[,..]   stop when a CPU reaches addr
  --stop-on-watch              stop on the first watchpoint hit
  --stop-on-hang               stop when a hang is detected

  OUTPUT:
  --format text|jsonl       default text (human), jsonl (machine/diffable)
  -o <FILE>                 default stdout
```

`kind` in `--watch` is `r`/`w`/`rw`; `cond` is defined in Feature 3.
`addr`/`from-frame` parse with the existing `parse_u32_auto`.

### Two run loops, chosen by flags

The harness picks its loop from the requested observers, because they need
different granularity:

- **Frame loop** (`--events`/`--watch`/`--hang` only, no instruction trace and
  no `--break-pc`): call `run_frame()` per frame, then drain
  `take_watchpoint_hit()` and (once) `trace_events()`. Cheap, and correct —
  watchpoint hits and trace events are queued *inside* `tick()` during
  `run_frame()`, so nothing is lost. This is the common "what happened over 3000
  frames" case.
- **Cycle loop** (`--cpu` instruction trace or `--break-pc`): call
  `debug_tick()` per cycle. On each returned instruction-boundary bit for a
  traced CPU, read the opcode bytes via `bus.read(cpu, pc..)`, disassemble via
  `debug_disassemble`, and emit a line; poll `take_watchpoint_hit()` and check
  `--break-pc` each cycle. Frame boundaries are tracked with
  `cycles_per_frame()` so `--from-frame`/`--frames` still bound the run.

Both share one `TraceHarness` (a new `tools/disasm/src/harness.rs`) that owns
machine construction, coin/NVRAM scripting, frame accounting, and the output
writer — factored out of today's `run_frameshot` so both subcommands share it.

### Output

Text format (default) is one event per line, cycle-sorted, in the same columnar
spirit as the UI event panel:

```
frame 3100  cyc 12694000  cpu0 pc=1BCC  A2 1BCC  DJNZ $1BCC          ; instr
frame 3100  cyc 12694040  cpu0 pc=1BD0  wdog                          ; event
frame 3100  cyc 12694104  cpu0 pc=0066  mem wr $87CF=$32 [sharedram]  ; watch (=changed)
=== HANG: cpu0 stuck in $1BC8..$1BD0 for 120 frames (pc=1BCC B=08) ===
```

`jsonl` emits one JSON object per line (`{frame, cycle, cpu, pc, kind, addr,
value, region, device, text}`) so a run is greppable, diffable, and feeds
future tooling. Both draw from the *same* `DebugEvent`/`WatchpointHit`/
instruction records — the format is a serializer choice, not a data-model fork.

### Interaction with `ifs0`

The cycle loop uses `debug_tick()`, which shares the known
`phosphor-emulator-ifs0` limitation (render-in-`run_frame` machines don't
refresh their framebuffer under `debug_tick`). The trace tool **does not
render**, so it is unaffected; but a future `trace --frameshot-at N` that both
traces and captures a frame would want `ifs0` fixed first. Noted, not blocking.

## Feature 2: Instruction-level execution trace

This is a *harness* feature, not a core one. The Dig Dug and ESB hunts both
needed "show me what the CPU actually executed"; today that means editing board
code. Instead, the cycle loop above already reconstructs it:

```
for each debug_tick():
    for each cpu bit set in the returned boundary mask, if traced:
        pc    = cpu.debug_pc()
        bytes = (0..MAX_INSN).map(|i| bus.read(cpu_idx, pc + i))  // side-effect-free
        insn  = cpu.debug_disassemble(pc, &bytes)
        emit(frame, cycle, cpu_idx, pc, insn.text)
```

No `DebugEventKind::InstructionExec` variant, no per-board recording, no bloating
the 4096-event ring with millions of instructions. The trace streams straight to
the output writer (file/stdout), so it is unbounded by design and never competes
with the event ring.

Nuances to handle in the harness:

- **Register snapshot columns.** Optionally append a few registers per line
  (`--cpu main:regs`) via `DebugCpu: Debuggable::debug_registers()`, so a trace
  reads like MAME's (`A=.. X=.. flags=..`). Default off (keeps lines short).
- **Volume control.** Full instruction traces are large. `--from-frame` seeks
  cheaply (frame loop until N, then switch to cycle loop); document that
  `--cpu` without a frame window can produce gigabytes.
- **Multi-CPU.** The boundary bitmask already distinguishes CPUs; each traced
  CPU is labeled `cpuN`. This is exactly what the Dig Dug (3× Z80) and ESB
  (6809 + matrix) hunts needed.

Because the disassemblers and `debug_pc` are already u32 (per the addr32 epic),
this works for the M68000 machines too.

## Feature 3: Conditional / value-match watchpoints

The only real core-type change. Today `Watchpoint` is `{cpu_index, addr, kind}`
and fires on *any* access. The ESB bug needed "fire only when `0x4e5f` is
written"; variable-tracking hunts need "fire only when the value *changes*."
The value is already available at the check site, so this is a small, localized
change.

### Core types (`core/src/core/watchpoint.rs`)

```rust
/// Extra predicate on a watchpoint's value. `Always` preserves today's
/// behavior (fire on any access).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum WatchpointCondition {
    Always,
    /// Fire when (value & width-mask) == v.
    Equals(u32),
    /// Fire when (value & mask) == expected — bit tests / any-of ranges.
    Bits { mask: u32, expected: u32 },
    /// Fire when value differs from the last one observed at this addr/kind.
    Changed,
}

pub struct Watchpoint {
    pub cpu_index: usize,
    pub addr: u32,
    pub kind: WatchpointKind,
    pub condition: WatchpointCondition,   // new
    last_value: Option<u32>,              // new; state for `Changed`
}
```

`Watchpoints::check()` already receives `value`/`width`; it gains one
predicate evaluation before queuing the hit, and updates `last_value` for
`Changed`. `Changed` correctly needs `&mut`, which `check_*` already has.

### Trait/API plumbing

- Extend `Watchpoints::set` to take a `WatchpointCondition`; keep a
  `set_always` (or a defaulted arg) so existing callers are untouched.
- `BusDebug` / `MachineDebug`: add
  `set_watchpoint_cond(cpu, addr, kind, condition)`. Keep the current
  `set_watchpoint(cpu, addr, kind)` as a shim for `Always`. Only the
  `#[debug_map]` derive in `macros/src/lib.rs` and `AddressSpace16` need
  updating — **not** the 14 boards, since they get watchpoints via the derive.
- `--watch cpu:addr:kind:cond` grammar: `=1F` → `Equals`, `&F0=50` → `Bits`,
  `chg` → `Changed`, absent → `Always`.
- UI (`debug_ui.rs`): a later, optional condition field on the watchpoint add
  form. The CLI is the priority consumer; the UI change can trail.

### Save-state

Watchpoints, like trace events, are observer state and are **not** serialized
(consistent with `debug-observability.md`'s closed decision #5).

## Feature 4: Hang / idle-loop detection

Promote the Dig Dug PC-sampling detector into a reusable util so both the CLI
(`--hang`) and the frontend F10 overlay can use one implementation.

### Core util (`core/src/core/debug_hang.rs`, new)

```rust
/// Detects a CPU spinning in a tight loop by watching its PC stay within a
/// small window across many frames — distinguishing a real hang from a
/// legitimate wait (EAROM/self-test) via a frame threshold.
pub struct HangDetector {
    window: u32,            // PC-span treated as "the same loop" (default 8)
    threshold_frames: u32,  // consecutive frames in-window to report (default 120)
    // per-CPU rolling min/max PC + in-window frame count
}

pub struct HangReport {
    pub cpu_index: usize,
    pub pc: u32,
    pub window_lo: u32,
    pub window_hi: u32,
    pub frames_stuck: u32,
}

impl HangDetector {
    pub fn observe(&mut self, cpu_index: usize, pc: u32) -> Option<HangReport>;
    pub fn reset(&mut self);
}
```

The harness calls `observe(cpu, cpu.debug_pc())` once per frame per CPU. On a
report it prints the PC, the offending register set (`debug_registers()`), and —
when `--events` is on — the tail of the event ring (the immediate causal
context, which is exactly how the Dig Dug hang was localized). With
`--stop-on-hang` the run halts there.

This is pure `DebugCpu`; no board changes. The frontend can later feed the same
detector from its run loop to drive an on-overlay "HANG?" indicator, replacing
any ad-hoc detection.

## Feature 5: Interactive debugger usability (UI + CLI)

A code review of `frontend/src/debug_ui.rs` (1,251 lines), the keybinding
dispatch in `frontend/src/emulator.rs`, and the disasm CLI arg definitions
(`tools/disasm/src/main.rs`) surfaced concrete friction. The debugger is
*capable* — this is about making that capability usable without prior
knowledge of the code. Findings are severity-ranked (🔴 high / 🟡 medium /
⚪ low); each names the code site and a fix. (Review method was static; the
live SDL window was not driven — worth a pass with screenshots before the UI
work lands.)

### UI — high severity

🔴 **Step/continue hotkeys are non-mnemonic (`Num7`/`8`/`9`/`0`).** Step Instr
= 7, Step Cycle = 8, Step Frame = 9, Continue = 0 (`emulator.rs:276-308`;
buttons labeled `"Step Instr (7)"` at `debug_ui.rs:551-570`). No mnemonic or
spatial logic, and it diverges from every convention users bring (F-keys, or
`n`/`s`/`c`). **Fix:** remap to conventional keys (e.g. `F6` continue,
`F7`/`F8` step instr/frame, or letters `c`/`n`/`f`) and update the button
labels to match.

🔴 **No dedicated Pause key.** `Num0` only *continues* (`emulator.rs:304-308`);
the Pause direction of the toggle at `debug_ui.rs:540-549` is button-only. Once
running with the panel open, pausing requires the mouse. **Fix:** make one key
toggle run↔pause.

🔴 **Hidden coupling: breakpoints & watchpoints silently target `step_cpu`.**
"Add" uses `state.step_cpu` for both PC breakpoints (`debug_ui.rs:748`) and
watchpoints (`:866`), but only the separate *Step target* radio (`:573-579`)
controls it, and neither the Breakpoints nor Watchpoints panel says which CPU
it edits. On a multi-Z80 board (Dig Dug) this is a real trap. **Fix:** label
those panels with the active CPU, or give them their own CPU selector.

### UI — medium severity

🟡 **Silent input failures everywhere.** Every hex/decimal field discards parse
errors with no feedback: breakpoint (`debug_ui.rs:744`), cycle (`:788`), frame
(`:816`), watchpoint (`:854`), memory "Go" (`:1177`), device write (`:652`). A
typo just clears or no-ops. Additionally, adding a watchpoint with **both R and
W unchecked** collects an empty kind set and silently does nothing
(`:858-871`). **Fix:** red-border/hint on invalid parse; disable Add when no
kind is selected.

🟡 **Memory viewer is read-only.** The hex grid peeks (`debug_ui.rs:1226`) but
can't poke, though `BusDebug::write` exists and device-register write is
already wired. Inline editing is a natural, low-cost add and a common need
(e.g. force a game variable to reproduce a state). **Fix:** editable hex cells
routing through `BusDebug::write`.

🟡 **Event Trace panel has no filtering** (`debug_ui.rs:1003-1034`) — only
Record/Clear/count. On a busy board the ring is dominated by `mem rd`/`mem wr`
noise. **Fix:** filter by kind/source/address — and share that filter model
with the Feature 1 CLI `--events` flag so both surfaces select events the same
way.

### UI — low severity

⚪ **Only the *last* watchpoint hit is shown** (`last_watchpoint_hit`,
`debug_ui.rs:905`) though the core keeps a hit *queue* (`Watchpoints::pending_hits`);
rapid successive hits are visually lost. **Fix:** show a short scrollable hit
history.

⚪ **No on-screen key legend.** Debug controls are scattered across F1 (toggle),
F5 (reset), `` ` `` (DIP), Num7-0 (step), P (pause) with no in-app cheat-sheet.
**Fix:** a `?` popover in the controls column listing the active keys.

⚪ **Disassembly window can jump while stepping.** `disassemble_around_pc`
(`debug_ui.rs:1067`) heuristically scans back 48-64 bytes to realign; a bad
decode falls back to forward-only from PC, dropping the "before" context.
Inherent to variable-length ISAs, but the jitter is a papercut; **Fix
(optional):** anchor the view and only re-center when PC leaves the window.

### CLI usability

The CLI is already in good shape — consistent `--machine`/`--region`/`path`,
`parse_u32_auto` (hex or decimal), region-listing when `--region` is omitted,
and machine-listing on an unknown machine. **The new `trace` command must
follow these same patterns.** Remaining items:

🟡 **No discoverable "list machines" command.** The machine list is only
learnable by *triggering* an unknown-`--machine` error. **Fix:** a `disasm
machines`/`list` subcommand (regions already list on omit).

⚪ **Mixed positional/flag conventions.** `raw`/`rom` use positional
`file`/`member` + `--cpu`; `frameshot`/`gfxview`/`machine` use `--machine` +
positional `path`. Consistent within each group but a small learning bump;
document the raw/rom-vs-machine split in `--help`.

### Highest-value subset

The three 🔴 items plus the 🟡 input-validation item are all low-effort,
high-return and should land first: conventional step/pause keys, a visible
breakpoint/watchpoint CPU target, and input validation. They remove the
sharpest "I didn't know it worked that way / nothing happened" edges.

## Migration Plan

### Phase 1 — Trace harness + event/watchpoint logging (Feature 1, frame loop)

1. Extract `TraceHarness` from `run_frameshot` (`tools/disasm/src/harness.rs`):
   machine construction, NVRAM, coin scripting, frame accounting, output writer.
   Re-point `frameshot` at it (no behavior change).
2. Add the `trace` subcommand with `--events`/`--watch`/`--from-frame` on the
   frame loop; text + jsonl writers over `DebugEvent`/`WatchpointHit`.
3. Tests: run a trace-instrumented machine (e.g. Williams/`joust`) headless,
   assert bank-switch/device-write events appear and ordering holds.

### Phase 2 — Instruction trace + break-pc (Feature 2, cycle loop)

1. Add the cycle loop driven by `debug_tick()` + boundary bitmask.
2. `--cpu` instruction trace (opcode bytes via `bus.read`, `debug_disassemble`),
   optional `:regs`; `--break-pc`; `--stop-on-watch`.
3. Tests: trace a few hundred cycles of a known machine, assert the PC stream is
   monotonic across a known basic block and that `--break-pc` stops on the
   target.

### Phase 3 — Conditional watchpoints (Feature 3)

1. Add `WatchpointCondition` + `Watchpoint.condition`/`last_value`; evaluate in
   `Watchpoints::check`.
2. Plumb `set_watchpoint_cond` through `BusDebug`/`MachineDebug` + the
   `#[debug_map]` derive; keep `set_watchpoint` as the `Always` shim.
3. Wire `--watch` condition grammar; (optional) UI field later.
4. Tests: `Equals`/`Bits`/`Changed` fire/don't-fire against a synthetic access
   stream; back-compat (no condition == `Always`); not serialized in save state.

### Phase 4 — Hang detection (Feature 4)

1. Add `HangDetector`/`HangReport` core util + unit tests (in-window vs.
   threshold vs. legitimate wait).
2. Wire `--hang`/`--stop-on-hang` into the harness (both loops).
3. (Optional, follow-up) feed the frontend overlay from the same detector.

### Phase 5 — Debugger usability (Feature 5)

Independent of Phases 1–4; the UI subset can land in any order. Suggested
sequence within the phase (highest-value first):

1. Remap step/continue keys + add a pause toggle (`emulator.rs`, `debug_ui.rs`
   button labels).
2. Show the active CPU on the Breakpoints/Watchpoints panels (or add a
   selector).
3. Input validation on all fields + guard the empty-kind watchpoint.
4. Editable memory cells (route through `BusDebug::write`).
5. Event-trace filtering (share the filter model with Feature 1's `--events`).
6. Watchpoint hit history, `?` key-legend popover, disasm view anchoring.
7. CLI `machines`/`list` subcommand; document raw/rom-vs-machine in `--help`.

## Testing

```bash
cargo test -p phosphor-core        # WatchpointCondition, HangDetector
cargo test -p phosphor-machines    # trace/watch events per machine
cargo build -p phosphor-disasm     # CLI
cargo clippy --all-features --all-targets
```

Core tests:

- `WatchpointCondition`: each variant fires exactly when expected; `Changed`
  tracks `last_value`; width-masking of `Equals`/`Bits`.
- `HangDetector`: reports after `threshold_frames` in-window; resets on a PC
  jump outside the window; does not report a legitimate multi-frame wait shorter
  than the threshold.
- Watchpoints/trace events remain absent from save-state round trips.

CLI/harness tests (over a real registered machine, ROM-gated like the frameshot
tests already are, or a synthetic registry entry):

- frame-loop trace emits expected event kinds in cycle order;
- cycle-loop instruction trace produces a monotonic PC stream over a known
  block and honors `--break-pc`;
- `--from-frame` suppresses output before the seek point;
- jsonl output parses and carries the structured fields.

Feature 5 (usability): the egui panels are interactive and not unit-testable
directly, but the `DebugState` data model is — cover the behavior changes at
that layer (e.g. an empty-kind watchpoint Add records nothing; a bad hex parse
leaves state unchanged and flags an error; the run/pause toggle transitions
`RunMode` correctly). The keybinding remap and CLI `list` subcommand are
verified by inspection / a `--help` snapshot. A manual pass with screenshots
should confirm the UI-facing items before they land.

## Closed Decisions

1. **One `trace` subcommand, not three.** Instruction trace, event ring,
   watchpoints, and hang detection are observers on a single run so their output
   correlates by cycle. Splitting them would force multiple runs and lose
   correlation.
2. **Instruction tracing lives in the harness, not the event ring.** It streams
   to the output writer via existing `debug_tick`/`debug_pc`/`debug_disassemble`
   — no `InstructionExec` event kind, no per-board recording, no ring pollution,
   and it works for every machine including the M68000 for free.
3. **Reuse the frameshot boot harness.** Extract `TraceHarness` and share it;
   don't fork machine construction/scripting.
4. **Conditional watchpoints are the only core-type change** and reuse the
   `value`/`width` already flowing into `Watchpoints::check`; `Always` keeps
   today's behavior and the derive shields the 14 boards from churn.
5. **`HangDetector` is a shared core util**, so the CLI and the frontend overlay
   converge on one implementation instead of re-deriving the Dig Dug detector.
6. **Observer state is never serialized** (watchpoints, trace events, hang
   state) — consistent with the debug-observability epic.
7. **jsonl is a first-class output format**, so headless runs are diffable and
   feed future analysis (the same spirit as the audio/video rigs), without
   committing to reference-comparison CI here.

## Relationship to open issues

- `phosphor-emulator-ifs0` (stale debugger image in `run_frame` renderers) is
  *related* but independent; the trace tool doesn't render, so it's unblocked.
  Fixing `ifs0` is a prerequisite only for a future combined trace+frameshot.
- `phosphor-emulator-j63p` (ESB watchdog reset) is the motivating case for
  Features 2 + 3 and should be revisited once they land (instruction trace of
  the `0xEA00–0xED00` write loop + a `--watch 0:<stack>:w:=4E5F`).
</content>
</invoke>
