# Design: Debug Observability Improvements

## Context

The current debugger is already useful for CPU-level work. It supports:

- pause/continue
- step instruction
- step cycle
- PC breakpoints
- cycle breakpoints
- memory watchpoints for `MemoryMap`-backed address spaces
- CPU register panels
- device register panels
- disassembly around PC
- memory hex viewer

The gaps are mostly around hardware interaction observability. Once CPU
correctness is stable, emulator bugs often live in bus traffic, interrupt
timing, DMA, device register side effects, scanline timing, and cross-CPU
coordination. The current debugger can show state, but it has limited history
and limited attribution.

## Current Architecture

Important code points:

- `core/src/core/debug.rs`
  - `Debuggable`
  - `DebugCpu`
  - `BusDebug`
- `core/src/core/machine.rs`
  - `MachineDebug`
  - `debug_bus`
  - `debug_bus_mut`
  - `cycles_per_frame`
  - `debug_tick`
  - watchpoint forwarding
- `frontend/src/debug_ui.rs`
  - `DebugState`
  - `execute_frame`
  - controls column
  - CPU columns
  - disassembly and memory viewer
- `macros/src/lib.rs`
  - `#[derive(BusDebug)]`
  - `#[debug_cpu]`
  - `#[debug_device]`
  - `#[debug_map]`
- `core/src/core/memory_map.rs`
  - `WatchpointHit`
  - `WatchpointKind`
  - watchpoint storage and checks
- manual `BusDebug` examples:
  - `machines/src/galaga.rs`
  - `machines/src/digdug.rs`

The debugger currently receives a snapshot-oriented view:

```rust
pub trait Debuggable {
    fn debug_registers(&self) -> Vec<DebugRegister>;
}

pub trait DebugCpu: Debuggable {
    fn debug_pc(&self) -> u16;
    fn debug_at_instruction_boundary(&self) -> bool;
    fn debug_disassemble(&self, addr: u16, bytes: &[u8]) -> DisassembledInstruction;
}

pub trait BusDebug {
    fn devices(&self) -> Vec<(&str, &dyn Debuggable)>;
    fn cpus(&self) -> Vec<(&str, &dyn DebugCpu)>;
    fn read(&self, cpu_index: usize, addr: u16) -> Option<u8>;
    fn write(&mut self, cpu_index: usize, addr: u16, data: u8);
    fn write_device_register(&mut self, device_index: usize, offset: u16, data: u8) {}
    fn reset_device(&mut self, device_index: usize) {}
    fn take_watchpoint_hit(&mut self) -> Option<WatchpointHit> { None }
    fn set_watchpoint(&mut self, cpu_index: usize, addr: u16, kind: WatchpointKind) {}
    fn clear_watchpoint(&mut self, cpu_index: usize, addr: u16, kind: WatchpointKind) {}
    fn clear_all_watchpoints(&mut self) {}
    fn memory_map(&self, cpu_index: usize) -> Option<&MemoryMap> { None }
}
```

`execute_frame` drives debug mode by calling `machine.debug_tick()` one cycle at
a time. `debug_tick` returns a bitmask of CPUs that reached instruction
boundaries. PC breakpoints are checked only on those boundaries. Watchpoint hits
are polled after each tick.

## Problems

### Watchpoint Hits Lack Attribution

`WatchpointHit` currently contains:

```rust
pub struct WatchpointHit {
    pub addr: u16,
    pub kind: WatchpointKind,
    pub value: u8,
}
```

It does not record:

- CPU index
- bus master (`Cpu(0)`, `Cpu(1)`, `Dma`, etc.)
- region/device name
- cycle number
- PC at time of access
- whether the access was memory, I/O, or device-register space

The UI can say "write `$4000 = $12`", but not "main Z80 wrote video RAM at
cycle 123456 from PC `$1BCC`."

### Watchpoint Coverage Is Inconsistent

Derived `BusDebug` watchpoints work when a machine has `#[debug_map]` fields
and bus code calls `check_read_watch`/`check_write_watch`. Manual `BusDebug`
implementations do not get this automatically.

Device register accesses are not first-class watchpoints. I/O can trigger
memory-map watchpoints if the board calls the hook, but the hit still looks
like a memory access with no device identity.

DMA/bypass paths can miss watchpoints if they access backing slices directly.

### Debug Memory Access Is Side-Effect-Free But Undifferentiated

`BusDebug::read` returns `Option<u8>`. `None` means unmapped or I/O, but the UI
currently displays `FF` in the memory viewer for missing bytes. This hides
useful distinctions:

- backed memory
- I/O address
- unmapped address
- mirrored address
- banked inactive backing
- board-specific side-effect-free hardware peek

### Device Debugging Is Register Snapshot Only

`Debuggable::debug_registers` is enough for simple devices. Complex devices
often need more:

- named channels or subcomponents
- latched interrupt lines
- FIFO contents
- timers and counters
- decoded mode flags
- device-local memory
- actions such as clear IRQ, trigger DMA, or reset channel

The derive macro can call `Device::write` and `Device::reset`, but the UI does
not currently expose semantic device actions or editable fields.

### No Event History

The debugger has current state but little history. For emulator bugs, recent
events are often the fastest path:

- bus reads/writes
- I/O port accesses
- device register writes
- interrupt assertions/acknowledgements
- DMA cycles
- bank switches
- watchdog clears/resets
- scanline/vblank transitions
- CPU halt/resume

Without a trace ring, debugging requires ad hoc logging in machine code.

### Multi-CPU Stepping Is Coarse

`debug_tick` advances the whole machine one master tick and returns a boundary
bitmask. This is a good primitive, but the UI model is limited:

- step target only selects which CPU boundary ends `StepInstruction`
- other CPUs and devices still advance, which is correct for system time but
  not visually explained
- breakpoints are PC-only and checked at boundaries
- the UI cannot show which CPU caused a watchpoint
- cycle count is a debugger-local count, not necessarily the machine's native
  clock or per-CPU cycle count

## Design Goals

1. Keep basic CPU stepping simple and fast.
2. Add event history without making every board noisy.
3. Attribute accesses to CPU/master, address space, region/device, cycle, and
   PC when available.
4. Make memory-view semantics explicit.
5. Support both derived and manual debug implementations.
6. Keep instrumentation disabled or very cheap when the debugger does not need
   it.
7. Prefer reusable core hooks over ad hoc per-machine debug logging.

## Proposed Architecture

Add a debug event layer beneath the UI:

```text
Debuggable/DebugCpu       current register/state snapshots
DebugMemory               side-effect-free memory/address-space view
DebugTrace                event history capability
DebugControls             optional device actions and editable fields
MachineDebug              stepping + current debug access
```

This can be implemented incrementally. The first useful milestone is event
tracing and richer watchpoint hits.

## Core Types

### Debug Access Source

`DebugAccessSource` is a **shared type, also defined in
`address-space-refactor.md`** (used there by `WatchpointHit`/`Watchpoints`). Same
definition; defined once and reused by events and watchpoints alike:

```rust
pub enum DebugAccessSource {
    Cpu(usize),
    Dma,
    Device(&'static str),
    Frontend,
    Unknown,
}
```

This avoids tying all debug events directly to `BusMaster` while still allowing
conversion from `BusMaster`.

### Debug Event

```rust
pub enum DebugEventKind {
    MemoryRead,
    MemoryWrite,
    IoRead,
    IoWrite,
    DeviceRead,
    DeviceWrite,
    InterruptAssert,
    InterruptClear,
    InterruptAck,
    DmaRead,
    DmaWrite,
    BankSwitch,
    Watchdog,
    Scanline,
    CpuHalt,
    CpuResume,
    Message,
}

pub struct DebugEvent {
    pub cycle: u64,
    pub source: DebugAccessSource,
    pub cpu_index: Option<usize>,
    pub pc: Option<u32>,
    pub kind: DebugEventKind,
    pub addr: Option<u32>,
    pub value: Option<u32>,
    pub width: u8,
    pub region: Option<&'static str>,
    pub device: Option<&'static str>,
    pub detail: Option<&'static str>,
}
```

Event strings should be static initially to avoid allocation in hot paths.
This gives up preformatted dynamic messages such as `"bank 3 -> 7"` or
runtime-generated device labels, but those values should usually be modeled as
structured fields (`addr`, `value`, `region`, `device`, `kind`) instead. If a
single dynamic field becomes necessary later, change only `detail` to
`Option<Cow<'static, str>>` or use interned strings.

### Debug Trace Buffer

```rust
pub struct DebugTraceBuffer {
    events: VecDeque<DebugEvent>,
    capacity: usize,
    enabled: bool,
}
```

`DebugTraceBuffer` is a shared component embedded in each board/system that
supports tracing:

```rust
pub struct WilliamsBoard {
    debug_trace: DebugTraceBuffer,
    // ...
}
```

This keeps ring-buffer behavior, filtering, capacity, enable/disable state,
and tests in one reusable type without forcing boards to share one global
object. Boards should be able to check one branch:

```rust
if self.debug_trace.enabled() {
    self.debug_trace.record(...);
}
```

Avoid passing `&mut DebugTraceBuffer` through every CPU/device hot-path method
initially. Boards should record important integration events at bus/device
boundaries. Device-internal tracing can be added later where it proves useful.

### Rich Watchpoint Hit

`WatchpointHit` is the **canonical shared type defined in
`address-space-refactor.md`** (see "Canonical `WatchpointHit`" there) — this doc
does not redefine it. Shown here for reference:

```rust
pub struct WatchpointHit {
    pub cpu_index: usize,
    pub source: DebugAccessSource,   // shared enum, also defined in address-space-refactor.md
    pub cycle: u64,
    pub pc: Option<u32>,
    pub addr: u32,
    pub kind: WatchpointKind,
    pub phase: WatchpointPhase,
    pub value: u32,
    pub width: u8,
    pub region: Option<&'static str>,
    pub device: Option<&'static str>,
}

// This doc owns the phase semantics:
pub enum WatchpointPhase {
    Before,
    After,
}
```

Division of ownership: `address-space-refactor.md` owns the structural move
(extracting `Watchpoints` into `core::watchpoint`, the `VecDeque` hit queue, and
u32 widening of `addr`/`value`); this doc owns *populating* the observability
metadata (`source`/`cycle`/`pc`/`region`/`device`) at the bus/board boundary and
the `WatchpointPhase` semantics below.

The UI can display a short form while preserving detail for an event panel.
Watchpoints should pause before write side effects by default so the user can
inspect pre-write state. Read watchpoints should fire after the read by default
because the value is known only after the access. A later UI can expose phase
selection if needed.

### Debug Memory Result

Add a richer read API while preserving the old `read` for compatibility. The
result type is the **canonical `DebugRead`/`DebugWrite` defined in
`address-space-refactor.md`** — this doc no longer defines a separate
`DebugMemoryValue`. `DebugRead` carries `value: u32` + `width` (so it serves
byte/word/long accesses for the 68000), plus a `region_id`:

```rust
// canonical — see address-space-refactor.md
pub enum DebugRead {
    Backed { value: u32, width: u8, region_id: RegionId },
    Io,
    Unmapped,
}

pub trait DebugMemory {
    fn peek(&self, cpu_index: usize, addr: u32) -> DebugRead;
    fn poke(&mut self, cpu_index: usize, addr: u32, data: u8) -> DebugWrite;
}
```

`BusDebug::read` can remain as a convenience:

```rust
fn read(&self, cpu_index: usize, addr: u16) -> Option<u8> {
    match self.peek(cpu_index, addr.into()) {
        DebugRead::Backed { value, .. } => Some(value as u8),
        _ => None,
    }
}
```

## Trait Changes

### BusDebug

Extend `BusDebug` with richer memory peek defaults. Do not add trace storage
or trace lifecycle methods here; those belong to `DebugTrace`.

```rust
pub trait BusDebug {
    fn devices(&self) -> Vec<(&str, &dyn Debuggable)>;
    fn cpus(&self) -> Vec<(&str, &dyn DebugCpu)>;

    fn read(&self, cpu_index: usize, addr: u16) -> Option<u8>;
    fn write(&mut self, cpu_index: usize, addr: u16, data: u8);

    fn peek(&self, cpu_index: usize, addr: u32) -> DebugRead {
        if addr <= 0xFFFF {
            match self.read(cpu_index, addr as u16) {
                Some(value) => DebugRead::Backed { value: value as u32, width: 1, region_id: 0 },
                None => DebugRead::Unmapped,
            }
        } else {
            DebugRead::Unmapped
        }
    }
}
```

### DebugTrace

Add event tracing as a separate capability rather than folding it into
`MachineDebug` or `BusDebug`:

```rust
pub trait DebugTrace {
    fn set_trace_enabled(&mut self, enabled: bool) {}
    fn trace_events(&self) -> &[DebugEvent] { &[] }
    fn clear_trace_events(&mut self) {}
}
```

`MachineDebug` stays focused on stepping and current debug access. `BusDebug`
stays focused on discovering CPUs/devices/address spaces. The frontend-facing
machine bundle should include `DebugTrace` with default no-op behavior.

## Address Width (u16 → u32)

The new event/peek/watchpoint types above already use `u32` addresses and
`Option<u32>` PCs. But the **existing debug-trait surface is still `u16`**, and
*nothing in either this doc or `address-space-refactor.md` widens it*. Until it
is widened, a 68000 debugger cannot show a real 24/32-bit PC, cannot disassemble
above `0xFFFF`, and cannot watch or peek a 32-bit address. This is the actual
gate for a usable 68000 debugger; it is called out here because it is otherwise
unowned.

Current `u16` surface (verified in `core/src/core/debug.rs` and
`frontend/src/debug_ui.rs`):

- `DebugCpu::debug_pc(&self) -> u16`
- `DebugCpu::debug_disassemble(&self, addr: u16, bytes: &[u8]) -> DisassembledInstruction`
- `BusDebug::read(&self, cpu_index, addr: u16) -> Option<u8>`
- `BusDebug::write(&mut self, cpu_index, addr: u16, data: u8)`
- `BusDebug::set_watchpoint`/`clear_watchpoint(cpu_index, addr: u16, kind)`
- `frontend/src/debug_ui.rs`: `execute_frame`'s PC-breakpoint checks, the memory
  hex viewer, and the disassembly scan all index with `u16`.

Target: widen all of the above to `u32`.

- `debug_pc -> u32`, `debug_disassemble(addr: u32, …)`.
- `BusDebug::read/write` and the watchpoint setters take `addr: u32`. Keep
  thin `u16` convenience shims during migration so 16-bit boards are untouched
  (`addr as u32` widening is lossless; existing boards keep working).
- Memory viewer / disassembly / PC-breakpoint UI compute and display `u32`
  addresses; 16-bit machines simply never exceed `0xFFFF`.

This is a wide but mechanical change. It is **not** required for the 16-bit
observability features (tracing, rich watchpoints, peek semantics) and can land
independently — but it **is** a prerequisite for the 68000 debugger and is
sequenced into the 32-bit enablement epic (alongside `AddressSpace32`), not this
doc's 16-bit-focused phases.

## Instrumentation Strategy

### Board-Level Instrumentation

Board bus methods are the best first hook point because they already know:

- bus master
- address
- data
- region
- device
- side effects

Example:

```rust
fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
    let region_id = self.map.page(addr).region_id;
    self.trace_bus_write(master, addr, data, region_id);
    ...
}
```

Helper:

```rust
fn trace_bus_write(&mut self, master: BusMaster, addr: u16, data: u8, region_id: RegionId) {
    if !self.debug_trace.enabled() {
        return;
    }
    self.debug_trace.record(DebugEvent {
        cycle: self.clock,
        source: master.into(),
        cpu_index: master.cpu_index(),
        pc: self.debug_pc_for(master),
        kind: DebugEventKind::MemoryWrite,
        addr: Some(addr as u32),
        value: Some(data as u32),
        width: 8,
        region: self.region_name(region_id),
        device: None,
        detail: None,
    });
}
```

This should be implemented first on one representative machine:

- Williams: multi-CPU, DMA blitter, bank switching, PIAs
- Namco Pac: simpler raster board, I/O latch and WSG
- Galaga/Dig Dug later because they have manual debug implementations

Williams is the best pilot because it exercises the most debug needs.

### MemoryMap/AddressSpace Integration

After `Watchpoints` are split from `MemoryMap`, watchpoint checks can take
source metadata:

```rust
self.main_space.check_write_watch(
    cpu_index,
    DebugAccessSource::from(master),
    self.clock,
    pc,
    addr,
    data,
);
```

This keeps watchpoint hits aligned with event tracing.

### Device-Level Instrumentation

Do not force every device to accept a debug sink immediately. Start with board
bus events for device register reads/writes. Add device-internal events only
where they are valuable:

- PIA IRQ assert/clear
- CTC timer edge
- DMA channel transfer
- DVG/AVG start/stop
- blitter start/finish/DMA cycle
- Namco custom chip transaction

Use explicit calls rather than a blanket trait until patterns settle.

## Frontend UI Changes

### Event Trace Panel

Add a tab or collapsible panel in the controls column:

Columns:

- cycle
- source
- PC
- kind
- address
- value
- region/device
- detail

Controls:

- enable/disable tracing
- clear
- pause on event kind
- filter by CPU/source
- filter by address range
- filter by kind

Keep capacity modest initially, e.g. 4096 events.

### Rich Watchpoint Display

Current:

```text
Write $4000 = $12
```

Target:

```text
CPU0 write $4000 = $12 Video RAM at cycle 123456 PC $1BCC
```

For multi-CPU machines, the CPU/source field is essential.

### Memory Viewer Semantics

Use `DebugRead`:

- backed memory: show byte normally
- I/O: show `--` or colored marker
- unmapped: show blank/`..`
- optional tooltip: region name and access kind

Do not silently display `FF` for every missing byte; that looks like real bus
data.

### Device Panels

Short-term improvement:

- expose reset button per device using `reset_device`
- expose editable byte write by offset using `write_device_register`

Long-term:

```rust
pub struct DebugAction {
    pub name: &'static str,
    pub kind: DebugActionKind,
}

pub trait DebugControllable: Debuggable {
    fn debug_actions(&self) -> &[DebugAction] { &[] }
    fn run_debug_action(&mut self, action_index: usize, value: Option<u32>) {}
}
```

Do this only after event tracing, because tracing will make device action needs
clearer.

## Macro Support

Update debug macro support in two parts:

1. `#[derive(BusDebug)]` continues generating current `devices`, `cpus`,
   `read`, and `write`.
2. `#[derive(BusDebug)]` generates `peek` by using
   `AddressSpace::debug_peek`/`MemoryMap::debug_read`.
3. Add a separate `#[derive(DebugTrace)]` or extend macro support with a
   trace-specific derive for structs that have a trace buffer field:

```rust
#[debug_events]
debug_trace: DebugTraceBuffer,
```

Generated methods:

```rust
fn trace_events(&self) -> &[DebugEvent] {
    self.debug_trace.as_slice()
}

fn clear_trace_events(&mut self) {
    self.debug_trace.clear();
}

fn set_trace_enabled(&mut self, enabled: bool) {
    self.debug_trace.set_enabled(enabled);
}
```

Manual machine implementations can implement `DebugTrace` directly.

## Migration Plan

### Phase 1: Rich Watchpoint Metadata

1. Extend `WatchpointHit` with CPU/source/cycle/PC/phase fields.
2. Add compatibility constructors or defaults so existing code compiles.
3. Update `MemoryMap::check_read_watch` and `check_write_watch` or add new
   methods that accept metadata.
4. Update board bus code gradually. For boards not migrated, fill unknown
   metadata.
5. Make writes pause before side effects by default and reads pause after
   values are known.
6. Update debug UI watchpoint display.

### Phase 2: DebugTrace Capability and Buffer

1. Add `DebugEvent`, `DebugEventKind`, `DebugAccessSource`,
   `DebugTraceBuffer`, and `DebugTrace`.
2. Add default `DebugTrace` implementation to the frontend-facing machine
   bundle.
3. Add `#[debug_events]` derive support.
4. Embed `DebugTraceBuffer` in Williams board as pilot.
5. Add event panel to debug UI.

### Phase 3: Memory Peek Semantics

1. Use the canonical `DebugRead`/`DebugWrite` from `address-space-refactor.md`
   (no separate `DebugMemoryValue`).
2. Add `BusDebug::peek` defaulting through old `read`.
3. Teach `AddressSpace16`/`AddressSpace32` to return the backed/I/O/unmapped result.
4. Update memory viewer to display missing bytes distinctly.

### Phase 4: Device Debug Controls

1. Expose reset buttons and register writes in UI using existing
   `reset_device` and `write_device_register`.
2. Add semantic actions only after the trace panel shows repeated needs.

### Phase 5: Broaden Instrumentation

Instrument more machines by priority:

1. Williams: blitter, bank switching, PIAs, dual CPU.
2. Namco Pac: I/O latches, WSG writes, watchdog.
3. TKG-04: DMA and sound CPU interactions.
4. Atari vector: vector generator start/stop, watchdog, IRQ/NMI.
5. Galaga/Dig Dug: 06XX/custom-chip transactions and multi-Z80 traffic.

## Testing

Core tests:

- event ring capacity and wraparound
- tracing disabled records nothing
- tracing enabled preserves order
- watchpoint hit metadata is preserved
- write watchpoints pause before mutation
- read watchpoints report values after access
- multiple watchpoint hits do not overwrite silently
- `DebugRead` distinguishes backed/I/O/unmapped

Machine tests:

- Williams write to ROM bank emits `BankSwitch`
- Williams blitter DMA emits DMA events when tracing enabled
- Namco Pac WSG write emits device write event
- manual `BusDebug` machines still compile and show no events by default
- debug events are not included in machine save-state round trips

Frontend tests are harder because the UI is interactive, but the data model can
be tested by constructing `DebugState` and applying event/watchpoint updates.

Run:

```bash
cargo test -p phosphor-core
cargo test -p phosphor-machines
cargo test -p phosphor-frontend
cargo clippy --all-features --all-targets
```

## Closed Decisions

1. Event tracing should be a separate `DebugTrace` capability. `MachineDebug`
   remains focused on stepping/current debug access, and `BusDebug` remains
   focused on CPU/device/address-space discovery.
2. Event detail strings should be static (`&'static str`) initially. Dynamic
   data should be represented structurally; `detail` can move to
   `Cow<'static, str>` later if needed.
3. Watchpoints should support phase. Writes pause before the side effect by
   default; reads pause after the value is known by default.
4. Use a shared `DebugTraceBuffer` component embedded in boards/systems. This
   centralizes ring behavior without introducing global sharing or threading a
   mutable trace object through every hot-path call.
5. Debug events are not saved in machine save states. They are observer state,
   not emulated hardware state.
6. `WatchpointHit`, `DebugAccessSource`, and `DebugRead`/`DebugWrite` are
   **canonical types owned by `address-space-refactor.md`**; this doc consumes
   them and does not define a parallel `DebugMemoryValue`. This doc owns the
   observability metadata population and `WatchpointPhase` semantics.
7. Widening the debug-trait surface (`DebugCpu`/`BusDebug`/`debug_ui`) from `u16`
   to `u32` is a real prerequisite for a 68000 debugger and is scheduled in the
   32-bit enablement epic, not this doc's 16-bit-focused phases.

## Recommendation

Implement event tracing and richer watchpoint metadata before adding more UI
controls. The current debugger can show state; the missing piece is explaining
how the state changed. A small, disabled-by-default event ring gives the project
that capability without compromising normal emulation speed.
