# Design: Session Commands for the Native Frontend

> **Status: accepted, implementation underway on the `session-command`
> branch.** A small typed command layer inside the frontend: one
> `SessionCommand` enum, one `drain` acting through `LoopCtx` and returning
> a small exhaustive `Effects` struct, one queue drained after the event
> poll, and one merged pause state. No new crate, no serialization, no
> journal.
>
> An earlier draft of this doc proposed a `phosphor-session` crate owning a
> second `Harness`, plus a journal. That design was withdrawn on review: the
> frontend already has a session owner in `DebugSession`, the crate graph
> contradicted itself, and the journal had no consumer. This revision keeps
> what was accurate in the diagnosis and fixes the placement.
>
> Related designs: [`input-movie.md`](input-movie.md) (command effects must
> never silently diverge a trace), [`rewind.md`](rewind.md) (rewind stays a
> separate, frontend-local epic), [`crt-rendering.md`](crt-rendering.md)
> (display knobs are presentation), [`machine-capabilities.md`](machine-capabilities.md)
> (the trait surface commands drive).

## Context

The frontend drives a `Box<dyn FrontendMachine>` through one loop in
`frontend/src/emulator.rs`. Session-level actions reach the machine through
two unconnected paths today.

First, one-shot hotkeys: `HostAction` plus the `DEFAULTS` table in
`frontend/src/host_keys.rs`, matched once per SDL press and dispatched by
twenty-two inline match arms (reset, hard reset, quick save/load, screenshot,
pause, frame advance, panel toggles, throttle, FPS, mouse grab, step keys,
movie record).

Second, deferred panel edits: egui panels never hold `&mut machine`, so they
record `pending_*` queues in `DebugState` and `SettingsState`
(`pending_dip_changes`, `pending_tuning`, `pending_host_rebind`,
`pending_device_actions`, `pending_memory_writes`) that `emulator.rs` drains
after the frame. Whole-machine rebuilds use a third shape, boolean flags
(`hard_reset_requested`, `arm_requested`, `pending_resync`).

There are also two pause models: `DebugState::global_paused` when panels are
closed and `RunMode::Paused` when the debugger governs, with `global_step`
consumed unconditionally so a stale step cannot fire late
(`frontend/src/debug_ui.rs:460-474`). The split is user-visible as a bug:
with the debugger open, `global_paused` is never consulted, so Pause shows
PAUSED and idles the loop (`emulator.rs:1625`) while frames keep running, and
`FrameAdvance` is a silent no-op (its `global_step` is discarded on the
debugger path).

Two distinctions shape the whole design:

* Unifying *dispatch* (twenty-two arms into one `apply`) does not need
  execution to move.
* Unifying *state* (pause, step, breakpoints) does, and that move is a
  moderate refactor, not a rewrite: `execute_frame` is already logic-only
  (it takes `machine` and `state`, with no rendering), so moving it with
  `RunMode`, breakpoints, watchpoints, and `frame_count` into a frontend
  session module is bounded work.

The emulation core (`MachineCore`, `FrontendMachine`, bus, CPU, video,
audio) stays deterministic and free of windows, SDL, egui, JSON, string
dispatch, and any command registry.

## Goals

1. Define `SessionCommand` as a typed Rust enum in the frontend. No string
   dispatch, no untyped parameters.
2. Funnel every migrated action through one `drain` on `SessionState`, so
   enablement, execution, and user feedback live in one place.
3. Drain one queue at one stated point in the loop. Panel edits stage
   into `pending_*` vectors that feed that queue ahead of the drain, and
   the rebuild flags service `Effects::rebuild` at the loop top; neither
   sits beside the queue as a parallel path for migrated actions.
4. Merge `global_paused` and `RunMode` into a single pause state, fixing the
   pause-with-debugger-open bug by defining both keys there.
5. Guard recording and replay against every state change outside the input
   stream, including the existing sub-frame desync.

## Non-goals

* No new crate, no serialization dependency, no journal. The journal had no
  consumer: it is not a movie, it is not replayable, and display settings
  already persist per machine in `state.toml`. If a future consumer (a
  control protocol or an external driver) appears, that epic justifies its
  own serialization.
* No asynchronous event bus, actor system, plugin system, scripting language,
  MCP server, or undo framework.
* No renderer, SDL, or egui migration. `HostAction` and `DEFAULTS` remain
  the shortcut authority, untouched.
* No new emulator features to populate the registry. Concretely: presets
  exist only as the panel's Measured/Punchy buttons over the whole
  `DisplaySettings` struct, so `SetDisplay` covers them without a preset
  command; there is no scanline step (only instruction, cycle, and frame
  steps exist); rewind commands belong to a separate epic (`rewind.md`).
* No `LoadMachine` variant in v1. Boot resolution (CLI args, per-game
  overrides, config, NVRAM adoption, DIP overlay) lives in `main.rs` and
  stays there.
* No savestate format change. Commands call the existing `SaveState` trait.

## Placement

A new module in the frontend, `frontend/src/session.rs`, holding
`SessionCommand`, the metadata table, `Effects`, `LoopCtx`, `MovieMode`,
the queue, and the `drain`. Execution state stays where it runs:
`execute_frame` keeps its shape in `debug_ui.rs` against `DebugState`
(which owns `run_mode`, breakpoints, watchpoints, trace flags, and the
counters), and the drain borrows that state through `LoopCtx` rather than
owning a copy. `debug_ui.rs` keeps panel rendering and the intent types
panels produce (`DeviceAction`, `MemoryWrite`), which convert to commands
at the drain. Panel step and pause buttons stage into `pending_commands`
like every other panel edit, so they pass through the same enablement and
ordering as the hotkeys. Panel toggles,
window sizing, throttle, FPS overlay, mouse grab, key legend, console
visibility, input rebinding UI and `BindingSet` persistence, host-key
rebinding, profiler UI, controller hotplug, focus-loss resync, overlay
selection, and audio setup stay where they are: frontend-local state, not
session actions.

If the Rhai console ever needs to submit commands, only the SDL-free machine
verbs (poke, reset, load, step) move down into `DebugSession`, which already
owns the `Harness` and already exposes them. Rebuild closures,
`MovieCapture`, overlays, and notices stay in the frontend permanently.

## Command catalog (v1)

Every variant below already exists as behavior. The table names the current
call site it migrates.

| Command | Migrates |
|---|---|
| `ResetSoft` | `HostAction::Reset` arm (`machine.reset()`) |
| `ResetHard` | `HostAction::HardReset` arm (rebuild from ROM) |
| `TogglePause` | `HostAction::TogglePause` (everywhere) and `HostAction::ToggleDebugPause` (debugger-modal, open only) |
| `AdvanceFrame` | `HostAction::FrameAdvance` (`global_step`) |
| `StepInstruction`, `StepCycle`, `StepFrame` | debugger-modal step keys and panel buttons |
| `SaveState` | `HostAction::QuickSave` arm |
| `LoadState` | `HostAction::QuickLoad` arm |
| `CaptureFrame` | `HostAction::Screenshot` arm (oriented bytes only) |
| `SetDisplay { settings: DisplaySettings }` | display panel edit (sliders and the Measured/Punchy buttons, live value, persisted at exit) |
| `SetDipOption { bank, option, value }` | `pending_dip_changes` drain |
| `SetBreakpoint`, `ClearBreakpoint` | `DebugState.breakpoints` edits |
| `SetWatchpoint`, `ClearWatchpoint` | watchpoint panel edits |
| `SetTraceEnabled(bool)`, `ClearTrace` | `trace_enabled_dirty`, `trace_clear_requested` |
| `PokeMemory { cpu, addr, value }` | `pending_memory_writes` |
| `DeviceControl(..)` | `pending_device_actions` (carries `DeviceAction`) |
| `MovieRecordToggle` | `HostAction::MovieRecord` arm |

`SetDisplay` carries the whole `DisplaySettings` struct (brightness, focus,
halation, `core/src/core/display.rs`), because the panel already works that
way: the sliders and the Measured/Punchy preset buttons both replace the
struct at once (`frontend/src/settings_ui.rs:479-499`). `SaveState` and
`LoadState` use the single quick-save slot path held by `LoopCtx`, matching
current behavior. `CaptureFrame` renders oriented bytes via the harness
`render_oriented` helper inside the drain, then applies the cabinet overlay
and writes the PNG through `LoopCtx`, since the overlay is presentation that
golden pins deliberately exclude.

## Metadata and enablement

```rust
pub struct CommandMeta {
    pub id: &'static str,        // e.g. "session.pause"
    pub label: &'static str,     // e.g. HostAction::TogglePause.label()
    pub category: CommandCategory,
}

pub enum CommandCategory { Session, Execution, State, Display, Input, Debugger, Capture }
```

IDs this design reserves: `session.reset_soft`, `session.reset_hard`,
`session.pause`, `session.advance_frame`, `exec.step_instruction`,
`exec.step_cycle`, `exec.step_frame`, `state.save`, `state.load`,
`capture.frame`, `display.set`,
`dip.set_option`, `debug.set_breakpoint`, `debug.clear_breakpoint`,
`debug.set_watchpoint`, `debug.clear_watchpoint`, `debug.set_trace`,
`debug.clear_trace`, `debug.poke_memory`, `debug.device_control`,
`movie.toggle_record`.

`meta(cmd) -> CommandMeta` matches on `self` with no wildcard arm, so a new
variant fails to compile until it has metadata. Shortcut lookup stays in
`host_keys.rs`: each command id documents the `HostAction` that feeds it,
and `DEFAULTS` remains the authority for which chord that is, so rebinds and
conflict reporting keep working unchanged.

Enablement has one function and two call times. Once per frame the loop
builds a read-only snapshot:

```rust
pub struct EnableCtx {
    pub has_debug_bus: bool,
    pub mode: MovieMode,          // derived, see Recording and replay guards
    pub save_file_exists: bool,
    pub dip_banks: usize,         // 0 means no DIP commands
    pub paused: bool,
    pub debug_open: bool,         // sub-frame steps are debugger-modal
}
```

`enable(&EnableCtx, &SessionCommand) -> Result<(), String>` is enforced
inside `drain`. The snapshot is rebuilt before each command in
the batch, not once per drain: commands earlier in the batch change the
state it describes. `[TogglePause, StepInstruction]` must see the paused
state the first command just set, and `[MovieRecordToggle (stop),
PokeMemory]` must see `Live` once the recording has ended. A failure
mutates nothing. `submit` itself only
enqueues and makes no availability claim. Example reasons: stepping
requires pause (`"Stepping needs the emulator paused"`); `LoadState`
requires an existing save file (`"No saved state to load"`); pokes require
a debug bus (`"This machine has no debug bus"`). Render-time UI hints
(disabled items, tooltips) are a follow-up: the panels do not consult
`enable` yet, and the ids and categories are read by tests until they do.
No call site keeps its own copy of the rules in the meantime.

`AdvanceFrame` is exempt from the paused-only stepping rule by definition:
today it pauses a running machine first (`emulator.rs:1099`), and the
migrated command preserves that auto-pause semantic.

`StepInstruction` and `StepCycle` additionally require the debugger open.
A step queued while open but drained after close is refused; without the
refusal the armed mode would sit in the closed-debugger branch forever
with frames stopped and no pause shown. The closed branch re-holds a
leftover mode as a backstop.

`TogglePause` carries no payload. A `SetPaused(bool)` would force the
caller to resolve the toggle at submit time, so two toggles in one batch
would both produce the same value and one would be lost; the drain resolves
it. Both existing bindings feed it: `HostAction::TogglePause` (F5) and
`HostAction::ToggleDebugPause` (F8).

## Execution semantics

The queue lives in one struct, `SessionState`. `SessionState` is the object
`submit` and `drain` are methods on; there are no free functions.

```rust
pub struct SessionState {
    queue: VecDeque<SessionCommand>,
    debug_was_paused: bool, // pause state held before the debugger opened
}

pub struct LoopCtx<'a> {
    pub machine: &'a mut dyn FrontendMachine,
    pub debug: &'a mut crate::debug_ui::DebugState,
    pub save_path: &'a Path,
    pub screenshot_dir: &'a Path,
    pub machine_name: &'a str,
    pub movies: &'a mut MovieCapture,
    pub overlay: Option<&'a ScreenOverlay>,
    pub notices: &'a mut crate::overlay::Notices,
}

// The only effects the drain cannot apply itself. Plain struct, public
// fields, no Default: the drain spells out both fields at construction, so
// a new field breaks the drain until it is considered. A rebuild implies
// the drain stops; there is no separate stop flag.
pub struct Effects {
    pub resync_input: bool,
    pub rebuild: Option<Rebuild>,  // HardReset or MovieArm, serviced at loop top
}

pub struct Rejection {
    pub cmd: SessionCommand,
    pub reason: String,
}

impl SessionState {
    pub fn submit(&mut self, cmd: SessionCommand);  // enqueue only
    pub fn drain(&mut self, ctx: &mut LoopCtx<'_>, mode: MovieMode) -> (Effects, Vec<Rejection>);
}

pub enum MovieMode { Live, Recording, Replaying }

pub fn movie_mode(recording: bool, replaying: bool) -> MovieMode {
    if recording {
        MovieMode::Recording
    } else if replaying {
        MovieMode::Replaying
    } else {
        MovieMode::Live
    }
}
```

`SessionState` owns no execution state: `run_mode`, breakpoints,
watchpoints (`watchpoints_suspended` included), trace flags, and the
counters live on `DebugState`, and the drain reaches them through
`LoopCtx::debug`. The loop passes only whether a replay is bound (a replay
cannot start or end mid-drain); the drain re-derives the mode per command
from the live capture and that flag, so a stop earlier in the batch
unfreezes the mode for the commands behind it.

The drain acts through `LoopCtx`: it pushes DIPs to the recorder, renders
and writes screenshots (overlay applied), performs file IO, and emits log
lines and overlay notices through the `Notices` handle. What returns is
only what the loop must still do: resync input and service a rebuild at
the loop top. Rejected commands return as `Rejection`s with their reasons;
a rebuild ends the drain, and anything behind it stays queued for the new
machine. The loop reports each rejection through `notices.error(reason)`,
like every other failure: a user who presses F3 mid-take sees why nothing
happened.

Rules:

* `submit` only enqueues. The drain
  runs at exactly one point in the loop, after the event poll and before
  input resync, movie delivery, and `execute_frame`:

```text
event poll -> command drain -> input resync -> movie deliver -> execute_frame
```

Draining before the poll would add a frame of latency to every hotkey;
draining after delivery would reorder commands against recorded input. The
order above is the whole rule.

* `ResetSoft` and `LoadState` set `resync_input` (the dead-held-key bug
  `emulator.rs:880` guards against) and zero the frame counter through
  `LoopCtx::frame_count`. A set `resync_input` feeds that same iteration's
  resync step, immediately after the drain; a one-iteration-late resync
  reopens the dead-key bug. User feedback (the log line plus
  `notices.info`/`error` every current arm emits) is emitted by the drain
  through `LoopCtx::notices`, stated once per command instead of per arm.
* `ResetHard` and arming `MovieRecordToggle` return `rebuild`, serviced at
  loop top exactly where `hard_reset_requested` and `arm_requested` are
  serviced today. A rebuild ends the drain; commands behind it stay queued
  for the new machine. Those flags dissolve into the queue rather than
  sitting beside it. NVRAM and DIP carry-over keeps current semantics.
* `SetDipOption` validates against `dip_banks()` metadata and pushes the
  byte to the recorder through `LoopCtx::movies` inside the drain,
  preserving the rule that a recording carries every DIP flip.
* `SaveState`/`LoadState` perform file IO inside the drain (replacing the
  inline arms) and surface `SaveError` text through the notice.
* `SetDisplay` applies the struct through `set_display_settings` inside
  the drain, with no range check: the struct defines no valid range, only
  the panel's sliders clamp (brightness 0.1 to 3.0, focus 0.5 to 4.0,
  halation 0.0 to 0.4). An out-of-range value degrades the picture rather
  than breaking anything. There is one command: no staging
  area, no commit event, no restore-on-close. The value lands one frame
  after the panel edit (today the panel applies it directly during the
  draw); persistence stays as today, a diff of the live value at exit
  (`main.rs:526-531`), so a slider moved by drag, keyboard arrows, or track
  click persists the same way. (The current write-only `display_changed`
  flag is deleted by the migration, since exit-diff persistence never reads
  it.)

## Pause and step unification

`global_paused`/`global_step` are deleted and `RunMode` governs everywhere:
running, paused, and the step modes, with the breakpoints, watchpoints,
trace flags, `cycle_count`, and `frame_count` it already owned staying on
`DebugState`. Nothing moves into `session.rs`; the merge deletes the
second pause model instead of relocating it. Preserved contract, stated
outright:

* Paused means no frame executes and no audio drains, however pause was
  reached.
* A step-once request executes exactly one unit (instruction, cycle, or
  frame) and re-holds; it is consumed unconditionally so a stale request
  cannot fire late, as today.
* Instruction and cycle steps and their keys stay debugger-modal: with the
  debugger closed those keys still reach the machine.
* Pause with the debugger open sets paused (today: nothing). `FrameAdvance`
  with the debugger open advances one whole frame (today: nothing). The
  auto-pause-then-step semantic of `FrameAdvance` holds in both cases.
* There is still one pause command, fed by two keys: F5 submits
  `TogglePause` everywhere; F8 submits it only while the debugger is open.
  F8 is a debugger-modal key, and its documented rule is that a machine
  binding the same key still reaches it whenever the debugger is closed
  (`frontend/src/host_keys.rs:76-88`), which is also why modal keys are
  excluded from shadow reporting (`host_keys.rs:448`). Making F8 a global
  pause would silently take the key from every machine that binds it and
  break the `HostAction`/`DEFAULTS` non-goal, so the modal gate stays.
  Today `ToggleDebugPanel` also forces pause on open and
  resume on close (`emulator.rs:804-812`). Migrated behavior: opening the
  debugger pauses a running machine (unchanged) and records having done so
  in `auto_paused`; closing resumes only in that case. A pause or step the
  user chose inside the panel clears the flag through the drain, so the
  close leaves the user's choice alone instead of overriding it: closing
  undoes only the automatic pause.

## Recording and replay guards

The structural fix comes first. While recording or replaying, every
whole-frame advance calls `run_frame()`, never `debug_tick()`: the
`Running` branch, `StepFrame`, and `AdvanceFrame` with the debugger open.
With the debugger closed the loop already calls `run_frame()`; with it
open, `Running` and `StepFrame` run `cycles_per_frame()` calls to
`debug_tick()` instead (`debug_ui.rs:546-595`, `633-646`), and on boards
like Dig Dug those are different code (`run_frame_scanline_outer` versus
`tick_frame_boundary`, `machines/src/digdug.rs:1398-1433`). Routing all
three through `run_frame()` makes recording and replay share one code
path, and breakpoints cannot fire because nothing checks them. The
debugger during a take is observe-only; the suspension notice says so. `cycle_count`
still advances by `cycles_per_frame()` per frame on this path, so cycle
breakpoints set after the take point at the right time; that arithmetic
rests on the same frame-path equivalence assumption the separate test
checks, and is exact only where the paths agree.

On top of that, while recording or replaying, any state change outside the
input stream is disabled, with reasons. `SetDipOption` is the single
exception, and only while recording, because the recorder captures it.
Concretely disabled in both modes: `ResetSoft`, `ResetHard`, `LoadState`,
`PokeMemory`, `DeviceControl`, `StepInstruction`, `StepCycle`. Arming a
recording (`MovieRecordToggle` while not recording) is also disabled while
replaying, since arming rebuilds the machine; stopping a recording stays
enabled in every mode. The frame breakpoint (checked at the start of a
frame, `debug_ui.rs:535`) is safe and stays enabled in both modes.

When a recording or replay starts, breakpoints and watchpoints already set
are suspended, and the user gets a notice saying so; silently inert
breakpoints read as broken. Watchpoints need real work: they fire in bus
dispatch (`core/src/core/address_space16.rs:719-723`), so `run_frame()`
keeps queueing hits for the whole take, and draining them afterwards would
pause on hits thousands of frames old. Suspension therefore calls
`clear_all_watchpoints()` on the machine and drains and discards any
already-queued hits; on return to `Live` it sets `watchpoints_dirty` so the
UI set is re-applied. Breakpoints need none of this, since only the loop
ever consults them.

One ordering rule keeps the rebuild fix from defeating suspension. A
recording that starts runs: arming rebuilds the machine (setting
`watchpoints_dirty`), the poll runs, the drain suspends (clearing
watchpoints, setting `watchpoints_suspended`), and then `execute_frame`
sees the dirty flag and would immediately re-apply every watchpoint for
the whole take (`debug_ui.rs:477-483`). `SetWatchpoint` during a take
reaches the same state. The rule: while `watchpoints_suspended`, the dirty
sync does nothing and leaves the flag set; the return to `Live` applies
the watchpoints, as above.

The suspension check runs in the loop body every iteration, before the
event poll, comparing the derived mode against
`DebugState::watchpoints_suspended`. That placement covers every
transition, including the two that happen outside the drain: arming,
serviced at the loop top, and a playback finishing inside `execute_frame`.
Both are observed on the next iteration before any work runs.

One rebuild rule rides along, fixing an existing bug: every rebuild
replaces the machine without re-applying watchpoints or trace settings, so
after a hard reset today the UI lists watchpoints the new machine does not
have (`hard_reset_requested` never sets `watchpoints_dirty`,
`emulator.rs:577`). With rebuilds through one path, the rule is one line:
after any rebuild, set `watchpoints_dirty` and `trace_enabled_dirty`.

Two lifecycle rules close the remaining holes. The movie mode is derived
(`movie_mode` above), never stored: `Replaying` holds only while a playback
is bound and not finished. The playback is unbound when it finishes, so
game input and resync are no longer suppressed and reset, load, and poke
work again; the "ended" readout shows on the final frame. Stopping a
recording returns to `Live` immediately. (Keeping the readout visible past
the final frame, and releasing movie-held inputs on unbind, landed after
this design as phosphor-emulator-czrq: the unbind parks a sticky readout,
releases through the recording tee, and resyncs from the physical devices
on the next iteration.)

Console evaluation keeps working while recording or replaying, but its
mutating verbs are locked. A blanket refusal would also block reads, which
are what observe-only debugging of a replay consists of. The lock is
structural, not a per-method convention: the mutating verbs live on
`SessionMut` and are reachable only through `DebugSession::mutate`, which
refuses while locked, so a verb added there is locked without its own
check. While locked, every mutating verb is refused except
the pure reads: `read`, `pc`, `regs`, `disasm`, `cpu_count`,
`device_names`, `device_registers`, `trace_enabled`. Everything else is
locked by default, including the easy-to-miss ones: `input`,
`input_axis`, `input_relative` (game input injected past the recording
tee, worse than a poke because the movie records a session in which those
presses never happened), `set_dip_option`, `set_dip_bank_value`,
`set_dip_by_name` (DIP flips that never reach the recorder), `load_movie`
(binds a different movie), and `take_hits`, `take_events` (consume queues
the debugger UI also drains). Each refusal carries its reason through the
verb's own return shape, so a locked call reports why into the console
output instead of silently doing nothing. The loop sets the flag from the
derived mode each frame (`frontend/src/emulator.rs:1663-1667`).
This touches `phosphor-script`, but the change is small and lands where the
verbs already live.

Three behavior changes in that section are deliberate: reset and load
mid-take are allowed today and silently diverge the
trace; pokes and device controls during replay are allowed today and do the
same; DIP flips during replay apply today and diverge. The guards forbid all
of them.

These guards are interim. Breaking into a replay is how a desync gets
diagnosed, and the guard removes exactly that. The intended fix is
frame-position tracking: record how many cycles into the current frame the
machine is, resume only the remaining cycles on continue, and advance the
movie counter when the frame boundary is crossed. That earns its own beads
issue; the guard text says "suspended while recording", nothing about the
future.

Sub-frame stepping under the debugger deserves its own accounting. Cycle
and instruction steps return `false` from the frame function without
advancing the movie counter (`debug_ui.rs:608-632`,
`emulator.rs:1246-1254`), so after one such step every later record sits at
a cycle offset that whole-frame replay never reproduces. That is an
existing unguarded bug, not a flaw in this design; the guard above stops
new takes from hitting it, and a separate beads issue should track the
debugger-recording interaction itself.

## Behavior changes, complete list

1. Soft reset, quick save, and quick load move from inline mid-batch
   execution (`emulator.rs:877, 915, 934`) to the post-poll drain, which
   reorders them against game input arriving in the same batch. Save is
   read-only and unaffected in practice; reset and load change order and
   this is accepted.
2. Reset, load, pokes, device controls, and replay-time DIP flips are now
   rejected while recording or replaying, as listed above.
3. Sub-frame steps are rejected while recording or replaying, and the
   `Running` branch runs through `run_frame()` in both modes; breakpoints
   and watchpoints are suspended with a notice.
4. Pause and frame advance gain meanings with the debugger open, and closing
   the debugger undoes only its own automatic pause, as defined above.
5. Display changes land one frame after the panel edit; today the panel
   applies them directly during the draw.
6. Breakpoints and watchpoints stay suspended for the rest of a replay,
   until the playback finishes and the mode returns to `Live`.
7. Live game input reaches the machine after a replay finishes, because
   the playback is unbound at the end; today it stays suppressed for the
   rest of the session.
8. Mutating console methods are refused while recording or replaying;
   today they work.
9. Watchpoints and trace settings survive a rebuild; today they are
   silently lost while the UI keeps listing them.
10. A sub-frame step queued while the debugger is open but drained after
    it closes is refused; previously the armed mode hung the machine with
    frames stopped and no pause shown. (The hang was introduced with the
    queue; before it, closing the panel forced Running and cleared the
    mode.)

## Migration slices

Prelude: movie integrity ships first, on its own. The guards, routing
takes through `run_frame()`, watchpoint suspension, the console lock, and
unbinding at replay end fix existing bugs: today a recording made with
the debugger open, or with a mid-take reset or console poke, saves
without complaint and does not replay. None of that needs the command
layer; it is a handful of checks in the current hotkey arms and
`execute_frame`, plus the lock in `phosphor-script`. It lands as its own
change with its own tests, before slice 1. The refactor then only has to
keep those guards working, and the state merge in slice 3 is verified
against guards that are already tested.

Frame-path equivalence is a separate beads issue, not a slice of this
epic. The guard routes takes through `run_frame()` regardless of its
result, so no outcome changes this design; what the test measures is
whether the debugger is faithful in normal play, which is worth knowing
independently and must not stall this epic on machine bugs. It is listed
under Tests for completeness and tracked on its own.

1. `session.rs` with the enum, metadata, `Effects`, `LoopCtx`, `SessionState`,
   and the queue, wired for pure-dispatch commands only: reset (soft and
   hard), save, load, and screenshot against the existing `DebugState`.
   Pause is deliberately left for slice 3, so the buggy pause behavior is
   never pinned in slice 1 tests. Unit-tested headlessly against
   `create_bare` machines.
2. One step action and the display command; their hotkey and panel call
   sites call `submit`.
3. The execution-state merge: `global_paused`/`global_step` are deleted
   and `RunMode` governs with the debugger closed or open, with
   `execute_frame` keeping its shape in `debug_ui.rs`. This is
   the risk-bearing slice and moves deliberately after slices 1 and 2, so
   dispatch unification is already proven before state unification starts.
4. Remaining debugger, DIP, trace, and movie commands. The panel step and
   pause buttons stage into `pending_commands`; the other `pending_*`
   vectors stay as panel-to-queue staging, drained into the single queue
   ahead of the drain. `hard_reset_requested` and `arm_requested` stay as
   the loop-top rebuild servicing that `Effects::rebuild` feeds, not as
   parallel staging paths. `pending_host_rebind` and `pending_tuning`
   stay: rebinding is frontend-local and permanently outside the layer.
5. Headless integration test: boot via `create_bare`, run commands across
   frames, verify session state; verify gameplay input and record/replay
   behavior unchanged.

No big-bang frontend rewrite. Call sites change shape one slice at a time.

## Tests

* `all()` enumerates every variant (data-carrying variants included, with
  representative payloads); ids are unique and every variant carries
  complete metadata (the wildcard-free `meta()` match enforces the latter
  at compile time, the test pins the former).
* Dispatch reaches the correct operation on a ROM-less machine; disabled
  commands come back in `drain`'s `Rejection` list with the expected
  reason and state unmodified.
* The drain is the only effect path: the Recording batch asserts the frame
  and cycle counters are unchanged by refused commands, and the poke round
  trip asserts an accepted command lands in machine memory.
* Recording and replay guards reject each listed command with its reason;
  `StepFrame` and `AdvanceFrame` stay accepted where their enablement holds
  (paused where required); `SetDipOption` stays accepted while recording
  only. A PC, cycle, or watchpoint hit while recording does not pause the
  machine mid-frame, with the control case in the same test: the same
  breakpoint, not recording, does pause.
* Sub-frame steps are refused with the debugger closed, and the drain arms
  without running: the integration test asserts the armed mode and an
  unchanged cycle count after the drain, then runs `execute_frame` and
  asserts the hold. `AdvanceFrame` is covered through the drain the same
  way: arm, one frame, hold.
* Frame-path equivalence (separate beads issue): a registry-driven test in
  `phosphor-harness` runs N frames via `run_frame()` and N frames via
  `cycles_per_frame() x debug_tick()` per machine and compares
  `save_state()` bytes plus the drained audio. ROM-gated via
  `PHOSPHOR_ROMS`: on blank ROMs the CPUs spin on filler opcodes and both
  paths agree without exercising video, interrupts, or sound timing, so a
  ROM-less version proves nothing. Sensitivity is proven by showing the
  test fails against a deliberate divergence. Failures are classified
  after diagnosis, not in advance: a divergence is not excused as a
  renderer artifact until someone has shown it is one.
* The `HostAction` round trip lives in the frontend crate, since the mapping
  lives there.
* Gameplay input preserved: existing `input` tests plus an unchanged
  record/replay pass.

## Gates

`cargo fmt --check`, `./scripts/test.sh` (or the affected
`cargo test -p` selection), and the exact CI clippy line
`cargo clippy --workspace --all-features --all-targets --locked -- -D warnings`.
Because this touches recording and replay, also run the movie suites
with ROMs present (`PHOSPHOR_ROMS=~/ws/mame-runtime/roms cargo test -p
phosphor-harness`, and the same for `phosphor-frontend`); without
`PHOSPHOR_ROMS` those suites skip and report green without testing
anything. Finish with one manual pass: record with the debugger open,
replay it, and check it by eye. No automated test covers that path.

## Open questions

* Whether the Rhai console ever submits commands; if so, only the SDL-free
  verbs move to `DebugSession`, per Placement. (The console lock is the
  interim: mutating verbs are refused during takes, reads stay available.)
* Whether `CaptureFrame` should also serve `headless.rs`, whose movie path
  must be checked first.
* Render-time UI hints from `enable()`, filed as a follow-up task; the
  drain enforces, the panels do not consult yet.

## References

* Hotkeys and defaults: `frontend/src/host_keys.rs`
* Main loop and all current call sites: `frontend/src/emulator.rs`
* Execution gate and step paths: `frontend/src/debug_ui.rs:460-474`,
  `531-601`, `602-653`
* Pause idling: `frontend/src/emulator.rs:1625`
* Movie counter gating: `frontend/src/emulator.rs:1246-1254`
* Display knobs: `core/src/core/display.rs`
* Machine contract: `core/src/core/machine.rs`
* Existing session owner: `tools/script/src/session.rs`, `rhai_api.rs:109`
* Companion designs: `input-movie.md`, `rewind.md`, `crt-rendering.md`
