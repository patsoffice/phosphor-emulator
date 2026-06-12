# Design: Address Space and MemoryMap Refactor

> **Status: implemented.** All seven phases are complete. The `MemoryMap`
> alias is removed; the final modules are `core::address_space` (shared
> `RegionId`/`AccessKind`/`DebugRead`/`DebugWrite`/`MemoryBacking`),
> `core::address_space16`, `core::address_space32`, and
> `core::watchpoint`. The text below is the design as written, kept as a
> record of the starting point and rationale.

## Context

`MemoryMap` started as a page-table-based decoder for 16-bit address spaces.
It now also owns backing storage, exposes debugger-safe reads and writes,
tracks watchpoints, records region descriptors, supports mirrors, and remaps
pages for bank switching.

That growth is understandable. Emulators need address-space introspection,
side-effect-free debug reads, banked memory, fast hot-path dispatch, and
watchpoints. The current type solves real problems and is widely used.

The name `MemoryMap` now undersells the abstraction. The deeper question is
whether its responsibilities should remain coupled.

The project also has active Motorola 68000 work that will use a 32-bit address
space and a word-wide CPU bus (`Bus<Address = u32, Data = u16>`). That means
this refactor cannot treat wider address spaces as a distant future concern.
The design must support current 16-bit arcade boards and 32-bit/large sparse
address spaces as parallel first-class paths.

The CPU core should remain insulated from this refactor: `M68000` talks only to
the `Bus` trait. `AddressSpace32` is for machines and test harnesses that
implement that bus.

## Current Architecture

Important code points:

- `core/src/core/memory_map.rs` defines:
  - `RegionId`
  - `AccessKind`
  - `PageEntry`
  - `RegionDescriptor`
  - `WatchpointHit`
  - `WatchpointKind`
  - `MemoryMap`
- `MemoryMap` fields currently include:
  - `pages: [PageEntry; 256]`
  - `regions: Vec<RegionDescriptor>`
  - `backing: Vec<u8>`
  - `region_backing: [u32; 256]`
  - `region_lengths: [u32; 256]`
  - watchpoint state
- Builder methods:
  - `region`
  - `backing_region`
  - `mirror`
  - `remap_pages`
- Hot-path helpers:
  - `page`
  - `region_offset`
  - `read_backing`
  - `write_backing`
- Debug/backing helpers:
  - `debug_read`
  - `debug_write`
  - `region_data`
  - `region_data_mut`
  - `load_region`
  - `load_region_at`
- Watchpoint helpers:
  - `check_read_watch`
  - `check_write_watch`
  - `take_hit`
  - `set_watchpoint`
  - `clear_watchpoint`
  - `clear_all_watchpoints`
- Introspection:
  - `regions`
  - `region_at`

Representative use cases:

- Williams board:
  - main and sound maps
  - banked ROM overlay through `backing_region` and `remap_pages`
  - writes to banked ROM address range still go to video RAM by manually using
    `region_data_mut(MainRegion::VideoRam)`
  - DMA VRAM reads bypass bank overlays with `BusMaster::DmaVram`
- Namco Pac board:
  - common map for ROM/RAM/video/color/I/O
  - I/O side effects handled in board `Bus` code
- TKG-04:
  - sprite DMA reads source bytes through `debug_read`, then writes sprite RAM
    through `region_data_mut`
- Atari vector boards:
  - board owns the map, wrappers own game-specific I/O
  - vector devices assemble their own vector memory from region slices
- Galaga and Dig Dug:
  - manual `BusDebug` implementations rather than `MemoryMap`/derive for all
    debug functionality

## Problems

### Naming Does Not Match Responsibility

`MemoryMap` sounds like decode metadata. In practice it is an address-space
container:

- decode table
- region metadata
- memory storage
- debug memory view
- watchpoint manager
- bank-switch table

This makes code harder to explain. New contributors may expect the bus to own
RAM/ROM and the map only to classify addresses.

### Decode and Storage Are Coupled

This coupling is useful for simple backed regions. It also has edge cases:

- writes may intentionally ignore the currently mapped region and target a
  different backing store, as on Williams banked ROM/video RAM
- devices sometimes need direct slices for rendering, DMA, save-state, or
  NVRAM
- some regions have side effects and no backing
- some backing regions have no page mapping until banked in
- mirrored regions are represented by copied page entries rather than first
  class mirror metadata

The current model handles these, but board code must know when to bypass the
abstraction.

### Debug Reads and Bus Reads Are Different

`debug_read` is side-effect-free backing access. That is correct for the memory
viewer and disassembler, but it is not equivalent to a CPU bus read.

Examples:

- I/O addresses return `None` rather than device register values.
- Banked/mirrored behavior follows current page entries but has no side
  effects.
- Board-specific special reads, such as status bits or latched inputs, are not
  represented unless the board implements custom `BusDebug::read`.

The distinction should be part of the type model instead of only comments.

### Watchpoint Ownership Is Too Narrow

Watchpoints live inside `MemoryMap`, so they are naturally attached to backed
or page-table-dispatched address spaces. Boards manually call
`check_read_watch` and `check_write_watch` after bus operations.

Limitations:

- `WatchpointHit` does not record CPU index or `BusMaster`.
- watchpoint/debug values are byte-sized today, which is too narrow for
  68000 word/long accesses.
- hits from multiple maps can overwrite each other before the UI polls.
- manual `BusDebug` implementations get no watchpoint support by default.
- device register watchpoints are not first class.
- DMA or bypass reads can miss watchpoints if the board does not call hooks.

### Region Metadata Is Page-Range Oriented

`RegionDescriptor` gives name/start/end/access. It does not distinguish:

- canonical mapping versus mirror
- backed region versus I/O region
- active bank versus inactive backing
- side-effect-free debug semantics versus bus semantics
- address-space width

The debug UI can show a memory viewer, but it cannot yet explain the address
space richly.

## Design Goals

1. Make the abstraction names reflect reality.
2. Preserve fast page-table dispatch on the hot path.
3. Separate address decode, memory backing, and watchpoint state enough that
   each can evolve independently.
4. Keep board bus code explicit where hardware side effects matter.
5. Make debug semantics explicit: backing read, bus peek, and bus read are not
   the same thing.
6. Support active 32-bit address-space work without forcing current 16-bit
   machines onto a slower or more complex map.
7. Enable better debugger introspection and event tracing.

## Proposed Architecture

Introduce shared lower-level services plus two concrete address-space
implementations:

```text
AddressMap16       fixed 256 x 256-byte page table for 16-bit boards
AddressMap32       sparse/ranged map for 32-bit and large address spaces
MemoryBacking      named byte storage for RAM/ROM/NVRAM/inactive banks
Watchpoints        address-space watchpoint state and hit queue
AddressSpace16     convenience owner for 16-bit machines
AddressSpace32     convenience owner for 32-bit machines
```

`MemoryMap` can become a temporary compatibility alias or wrapper during
migration, but it should not remain in the final public API. All code should
migrate to the more accurate `AddressSpace16` or `AddressSpace32` names.

### Shared Types

`MemoryBacking`, `Watchpoints`, debug read/write result types, access metadata,
and region descriptors should be shared between the 16-bit and 32-bit
implementations wherever practical. The hot-path address decode should stay
concrete, not hidden behind a trait object.

Use a small common trait only for debugger and tooling code that can tolerate
abstraction:

```rust
pub trait AddressSpaceView {
    type Address: Copy + Into<u64>;

    fn peek_backing(&self, addr: Self::Address) -> DebugRead;
    fn poke_backing(&mut self, addr: Self::Address, data: u8) -> DebugWrite;
    fn region_name_at(&self, addr: Self::Address) -> Option<&'static str>;
}
```

Board bus implementations should use concrete types (`AddressSpace16` or
`AddressSpace32`) directly.

### AddressMap16

`AddressMap16` owns 16-bit decode metadata only:

```rust
pub struct AddressMap16 {
    pages: [PageEntry; 256],
    regions: Vec<RegionDescriptor>,
}
```

Responsibilities:

- map address pages to region IDs
- store region descriptors
- support mirrors
- support page remapping for bank switching
- answer region lookup questions

It should not own bytes or watchpoint hits.

Candidate API:

```rust
impl AddressMap16 {
    pub fn new() -> Self;
    pub fn region(
        &mut self,
        id: impl Into<RegionId>,
        name: &'static str,
        start: u16,
        length: u32,
        access: AccessKind,
    ) -> &mut Self;
    pub fn mirror(&mut self, mirror_start: u16, source_start: u16, length: u32) -> &mut Self;
    pub fn remap_pages(
        &mut self,
        start_page: u8,
        page_count: u8,
        new_region_id: impl Into<RegionId>,
        new_base_offset: u32,
    );
    pub fn page(&self, addr: u16) -> &PageEntry;
    pub fn region_offset(&self, addr: u16) -> usize;
    pub fn regions(&self) -> &[RegionDescriptor];
    pub fn region_at(&self, addr: u16) -> Option<&RegionDescriptor>;
}
```

`PageEntry::base_offset` should become `u32`, not `u16`. Current 16-bit
regions are small enough, but shared backing offsets and banked regions should
not inherit a 16-bit offset limit.

### AddressMap32

`AddressMap32` should not use a flat 32-bit page table. A 256-byte page table
for a 32-bit address space would require 16,777,216 page entries, and even a
24-bit 68000 map would require 65,536 entries. That is unnecessary for sparse
arcade/computer address maps.

Start with a sorted, non-overlapping range map:

```rust
pub struct AddressMap32 {
    regions: Vec<AddressRegion32>,
}

pub struct AddressRegion32 {
    pub id: RegionId,
    pub name: &'static str,
    pub start: u32,
    pub end: u32,
    pub access: AccessKind,
    pub target: RegionTarget,
}

pub enum RegionTarget {
    Backing { region_id: RegionId, base_offset: u32 },
    Io,
    Alias { source_start: u32 },
    Unmapped,
}
```

Lookup is a binary search over sorted regions. For the expected number of
regions, this is simple and fast enough. If profiling later shows address
lookup is hot for 68000-class machines, add a small last-hit cache or replace
the internals with a sparse two-level page table without changing the public
`AddressSpace32` API.

Candidate API:

```rust
impl AddressMap32 {
    pub fn new() -> Self;
    pub fn region(
        &mut self,
        id: impl Into<RegionId>,
        name: &'static str,
        start: u32,
        length: u32,
        access: AccessKind,
    ) -> &mut Self;
    pub fn alias(
        &mut self,
        name: &'static str,
        mirror_start: u32,
        source_start: u32,
        length: u32,
    ) -> &mut Self;
    pub fn remap_range(
        &mut self,
        start: u32,
        length: u32,
        target: RegionTarget,
    );
    pub fn region_at(&self, addr: u32) -> Option<&AddressRegion32>;
    pub fn resolved_offset(&self, addr: u32) -> Option<(RegionId, u32)>;
}
```

The 68000 CPU may use `Bus<Address = u32, Data = u16>`, but ROM/RAM backing
should remain byte-addressable. `AddressSpace32` should provide big-endian
helpers for 68000 boards:

```rust
pub fn read_u8(&self, addr: u32) -> Option<u8>;
pub fn read_u16_be(&self, addr: u32) -> Option<u16>;
pub fn read_u32_be(&self, addr: u32) -> Option<u32>;
pub fn write_u8(&mut self, addr: u32, data: u8) -> bool;
pub fn write_u16_be(&mut self, addr: u32, data: u16) -> bool;
pub fn write_u32_be(&mut self, addr: u32, data: u32) -> bool;
```

It should also provide explicit word-bus adapter helpers for machines whose
`Bus` implementation is word-wide:

```rust
pub fn read_bus_word_be(&self, addr: u32) -> u16;
pub fn write_bus_word_be(&mut self, addr: u32, data: u16);
```

The helpers should define unmapped/default bus values at the board layer when
hardware requires board-specific behavior. A simple validation harness can
return `0xFFFF` for unmapped reads, while a real board may float different
data.

Do not bake Motorola 68000 24-bit address masking into `AddressSpace32`.
Address masking is CPU-variant behavior (`M68000` masks to 24 bits; later
variants may not). `AddressSpace32` should accept the address it is given.

### MemoryBacking

`MemoryBacking` owns storage by region ID:

```rust
pub struct MemoryBacking {
    data: Vec<u8>,
    region_backing: [u32; 256],
    region_lengths: [u32; 256],
}
```

Responsibilities:

- allocate backing for regions that need storage
- provide slices for rendering, save-state, ROM loading, and NVRAM
- provide read/write by resolved region and offset

Candidate API:

```rust
impl MemoryBacking {
    pub fn new() -> Self;
    pub fn allocate(&mut self, region_id: impl Into<RegionId>, length: u32);
    pub fn has_region(&self, region_id: impl Into<RegionId>) -> bool;
    pub fn region_data(&self, region_id: impl Into<RegionId>) -> &[u8];
    pub fn region_data_mut(&mut self, region_id: impl Into<RegionId>) -> &mut [u8];
    pub fn load_region(&mut self, region_id: impl Into<RegionId>, data: &[u8]);
    pub fn load_region_at(&mut self, region_id: impl Into<RegionId>, offset: usize, data: &[u8]);
    pub fn read_region_offset(&self, region_id: RegionId, offset: usize) -> Option<u8>;
    pub fn write_region_offset(&mut self, region_id: RegionId, offset: usize, data: u8) -> bool;
}
```

### Watchpoints

`Watchpoints` should be independent from memory backing and live in a new
module, `core/src/core/watchpoint.rs`. They are debug/observation state, not
address decode or memory storage. Keeping them separate lets manual
`BusDebug` implementations and future device-register watchpoints reuse the
same machinery.

```rust
pub struct Watchpoints {
    watched_addrs: Vec<Watchpoint>,
    // Optional fast filters can differ between 16-bit and 32-bit users.
    pending_hits: VecDeque<WatchpointHit>,
}
```

Candidate types:

```rust
pub struct Watchpoint {
    pub cpu_index: usize,
    pub addr: u32,
    pub kind: WatchpointKind,
}
```

#### Canonical `WatchpointHit` (shared with `debug-observability.md`)

`WatchpointHit` is touched by both this refactor and `debug-observability.md`.
To stop the two designs from diverging, **this is the single canonical shape**;
`debug-observability.md` references it rather than defining its own. Division of
ownership:

- **This doc (address-space)** owns the *structural* move: extracting
  `Watchpoints` into `core::watchpoint`, the `VecDeque` hit queue (replacing the
  single `Option<WatchpointHit>` slot in today's `MemoryMap`), and widening
  `addr`/`value` to `u32` + `width`.
- **`debug-observability.md`** owns *populating* the observability metadata
  (`source`, `cycle`, `pc`, `phase`, `region`, `device`) at the bus/board
  boundary, and the `WatchpointPhase` semantics.

```rust
pub struct WatchpointHit {
    pub cpu_index: usize,
    pub source: DebugAccessSource,   // see shared enum below
    pub cycle: u64,                  // populated by debug-observability
    pub pc: Option<u32>,             // populated by debug-observability
    pub addr: u32,
    pub kind: WatchpointKind,
    pub phase: WatchpointPhase,      // Before | After; see debug-observability
    pub value: u32,
    pub width: u8,
    pub region: Option<&'static str>,
    pub device: Option<&'static str>,
}
```

#### Shared `DebugAccessSource`

A small debug-facing source enum, shared by `Watchpoints`, `DebugEvent`
(`debug-observability.md`), and manual `BusDebug` implementations — used instead
of the core-specific `BusMaster` so generic watchpoint/event code need not depend
on it (provide `From<BusMaster>`):

```rust
pub enum DebugAccessSource {
    Cpu(usize),
    Dma,
    Device(&'static str),
    Frontend,
    Unknown,
}
```

The important changes:

- hits are queued, not single-slot overwritten
- CPU/source is recorded
- the type can be used by manual `BusDebug` implementations
- device register watchpoints can be added later without living inside memory
  backing

### AddressSpace16 and AddressSpace32

`AddressSpace16` composes the common 16-bit case:

```rust
pub struct AddressSpace16 {
    map: AddressMap16,
    backing: MemoryBacking,
    watchpoints: Watchpoints,
}
```

This type preserves the ergonomic API that boards use today:

```rust
impl AddressSpace16 {
    pub fn region(..., access: AccessKind) -> &mut Self;
    pub fn backing_region(...);
    pub fn mirror(...);
    pub fn remap_pages(...);

    pub fn page(&self, addr: u16) -> &PageEntry;
    pub fn read_backing(&self, addr: u16) -> u8;
    pub fn write_backing(&mut self, addr: u16, data: u8);
    pub fn debug_peek(&self, addr: u16) -> DebugRead;
    pub fn debug_poke(&mut self, addr: u16, data: u8) -> DebugWrite;
    pub fn region_data(...);
    pub fn region_data_mut(...);
}
```

`AddressSpace32` composes the wide/sparse case:

```rust
pub struct AddressSpace32 {
    map: AddressMap32,
    backing: MemoryBacking,
    watchpoints: Watchpoints,
}
```

It should expose the same conceptual operations using `u32` addresses and
range-based mapping, plus big-endian helpers for 16-bit/32-bit CPU bus access.

### Debug Access Semantics

Replace the ambiguous `debug_read`/`debug_write` names with explicit methods.
**`DebugRead`/`DebugWrite` are the canonical memory-result types**, shared with
`debug-observability.md` (which drops its earlier parallel `DebugMemoryValue` in
favor of these):

```rust
pub enum DebugRead {
    Backed {
        value: u32,
        width: u8,
        region_id: RegionId,
    },
    Io,
    Unmapped,
}

pub enum DebugWrite {
    Backed {
        old: u32,
        new: u32,
        width: u8,
        region_id: RegionId,
        access: AccessKind,
    },
    IoIgnored,
    UnmappedIgnored,
}
```

Suggested method names:

- `peek_backing(addr)` for side-effect-free memory view
- `poke_backing(addr, data)` for side-effect-free backing edit
- reserve `debug_bus_peek` or `peek_bus` for board-provided side-effect-free
  hardware peeks that include I/O state

This gives the debugger room to label memory cells as backed, I/O, or
unmapped instead of displaying everything as `FF`. Debug writes should be
allowed to modify ROM backing. This is useful for patching behavior while
debugging, and the richer result type lets the UI distinguish "patched Program
ROM" from "edited RAM".

Byte writes, word writes, and long writes should be distinct address-space
operations. The current M68000 M1 design uses read-modify-write for byte writes
on a word bus as a validation-friendly simplification. That behavior belongs
in the 68000 bus adapter or CPU memory helper, not as a universal
`AddressSpace32` rule.

## Migration Plan

### Phase 1: Rename Conceptually, Preserve API

Add `AddressSpace16` as a new struct with the current `MemoryMap` internals.
Then add a temporary alias:

```rust
pub type MemoryMap = AddressSpace16;
```

This is mostly naming and documentation, but it lets new code use the better
name without breaking existing machines. The alias is migration-only; the end
state should remove it and update all current users.

Update comments and docs to describe `AddressSpace16` as the composed
address-space container.

### Phase 2: Extract AddressMap16 Internals

Move:

- `pages`
- `regions`
- `region`
- `mirror`
- `remap_pages`
- `page`
- `region_offset`
- `regions`
- `region_at`

into `AddressMap16`.

`AddressSpace16` delegates to `self.map`.

Tests in `core/src/core/memory_map.rs` should be split:

- `AddressMap16` tests
- composed `AddressSpace16` tests

### Phase 3: Extract MemoryBacking

Move:

- `backing`
- `region_backing`
- `region_lengths`
- `region_data`
- `region_data_mut`
- `load_region`
- `load_region_at`

into `MemoryBacking`.

`AddressSpace16::region` should allocate backing when `AccessKind` indicates a
backed region. `AddressSpace16::backing_region` should allocate backing and
add a descriptor through `AddressMap16`.

This phase should preserve all board call sites.

### Phase 4: Extract Watchpoints to a New Module

Move watchpoint state and logic into `core/src/core/watchpoint.rs`.

Change board bus code from:

```rust
self.map.check_read_watch(addr, data);
```

to either:

```rust
self.map.watch_read(cpu_index, master, addr, data);
```

or:

```rust
self.watchpoints.check_read(cpu_index, master, addr, data, region_id);
```

The first form is less disruptive. The second form is cleaner if `Watchpoints`
becomes shared by more than memory maps.

Update `BusDebug` derive to route watchpoint operations through
`AddressSpace16::watchpoints`.

### Phase 5: Add AddressSpace32

Add `AddressMap32` and `AddressSpace32` for active 68000-class work. This
should happen before the old `MemoryMap` alias becomes entrenched under the new
names.

Initial implementation:

- sorted non-overlapping ranges
- byte-addressable `MemoryBacking`
- `u32` address APIs
- big-endian read/write helpers
- explicit word-bus adapter helpers for `Bus<Address = u32, Data = u16>`
- shared `Watchpoints`
- shared `DebugRead`/`DebugWrite`

Do not force existing 16-bit machines through `AddressSpace32`.

### Phase 6: Improve Debug Read/Write Results

Replace or supplement:

```rust
fn read(&self, cpu_index: usize, addr: u16) -> Option<u8>;
fn write(&mut self, cpu_index: usize, addr: u16, data: u8);
```

with a richer debug memory API in the debug refactor. Until then, keep the old
methods and implement them in terms of `peek_backing`.

### Phase 7: Remove MemoryMap Alias

After all code has migrated:

- remove `pub type MemoryMap = AddressSpace16`
- rename `memory_map.rs` to the new module split
- update public re-exports
- update README and CLAUDE guidance

## Board Migration Notes

### Williams

Williams should keep explicit special cases:

- reads from `DmaVram` bypass bank overlays
- writes to banked ROM address ranges target video RAM
- `rom_bank` writes change active page mapping

The new design should make that explicit rather than trying to encode it all
inside `AddressSpace16`.

### TKG-04

`trigger_sprite_dma` currently reads through `debug_read`. After the rename it
should use `peek_backing` or a board-specific DMA source helper. DMA source
reads are not debugger reads; they are side-effect-free backing reads because
the DMA source is memory, not I/O.

### Atari Vector

Vector devices assemble private vector address spaces from region slices. That
is a good use of `MemoryBacking`/`AddressSpace16::region_data`; keep it.

### Galaga and Dig Dug

Manual `BusDebug` implementations should eventually gain reusable
`Watchpoints`, even if they do not use `AddressSpace16` or `AddressSpace32`
for all memory.

### 68000-Class Machines

68000 work should use `AddressSpace32` from the start. It should not build on
`AddressSpace16` or a flat 32-bit page table. The CPU bus can remain word-wide
(`Bus<Address = u32, Data = u16>`), while the backing store remains
byte-addressable and exposes big-endian helpers.

`M68000` itself should not depend on `AddressSpace32`; it should only depend on
`Bus<Address = u32, Data = u16>`. `SimpleSystem68k`, `TestBus68k`, and
`TracingBus68k` can either stay as minimal flat-memory harnesses or become
early `AddressSpace32` consumers. If they migrate, they should use the
word-bus adapter helpers and keep TomHarte sparse RAM setup straightforward.

The M68000 24-bit effective-address mask remains CPU/variant behavior. Do not
hide it inside `AddressSpace32`, or 68020+ support will inherit the wrong
address semantics.

## Testing

Keep existing tests, but reorganize them by responsibility:

- `AddressMap16`:
  - unmapped defaults
  - region page entries
  - mirrors
  - remaps
  - descriptors
- `AddressMap32`:
  - sparse range lookup
  - aliases
  - remaps
  - overlapping range rejection
  - unmapped gaps
- `MemoryBacking`:
  - allocation
  - slices
  - load whole region
  - load region at offset
  - missing backing behavior
- `AddressSpace16`:
  - backed reads/writes through active pages
  - banked region switching
  - mirror reads
  - debug peek/poke result variants
- `AddressSpace32`:
  - backed reads/writes through sparse ranges
  - big-endian byte/word/long helpers
  - word-bus adapter helpers
  - ROM debug patching
  - aliases and remaps
  - debug peek/poke result variants
  - no implicit 24-bit masking
- `Watchpoints`:
  - exact address matching
  - read/write separation
  - multiple hits queued
  - CPU/source metadata
  - value width metadata for byte/word/long accesses
  - clear by address/kind
  - clear all

Run:

```bash
cargo test -p phosphor-core memory_map
cargo test -p phosphor-machines
cargo clippy --all-features --all-targets
```

## Closed Decisions

1. `MemoryMap` is migration-only. All code should migrate to
   `AddressSpace16` or `AddressSpace32`; the alias should be removed.
2. `Watchpoints` should live in a new `core::watchpoint` module.
3. Debug writes should be allowed to modify ROM backing by default. The
   `DebugWrite` result should include `AccessKind` so the UI can show ROM
   patching explicitly.
4. Use parallel concrete implementations, not one generic map:
   `AddressSpace16` for current 16-bit boards and `AddressSpace32` for active
   68000-class 32-bit address spaces.
5. The `WatchpointHit`, `DebugAccessSource`, and `DebugRead`/`DebugWrite` types
   defined here are **canonical and shared with `debug-observability.md`**; that
   doc consumes them rather than defining parallel shapes. This doc owns the
   structural move (queue, `core::watchpoint`, u32 widening of `addr`/`value`);
   `debug-observability.md` owns populating the observability metadata
   (`source`/`cycle`/`pc`/`phase`/`region`/`device`).

## Recommendation

Split the implementation and keep ergonomic composed types. The current
behavior is useful and should not be discarded, but the final names should be
explicit:

- `AddressMap16` decodes 16-bit page-table address spaces.
- `AddressMap32` decodes sparse 32-bit address spaces.
- `MemoryBacking` stores bytes.
- `Watchpoints` observe accesses.
- `AddressSpace16` and `AddressSpace32` are the convenience wrappers.

This turns a naming problem and a mild responsibility problem into a foundation
that supports both existing arcade boards and active 68000 work without making
either path pay for the other's constraints.
