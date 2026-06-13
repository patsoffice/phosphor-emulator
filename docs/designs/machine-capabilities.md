# Design: Machine Capability Refactor

> **Status: implemented.** `MachineCore` plus the capability traits are
> the final shape, bundled into the object-safe `FrontendMachine` for
> the frontend and registry. The migration-only `Machine` alias trait
> described below is removed; the unqualified `Machine` name no longer
> exists in code.

## Context

`Machine` is the front-end contract for every playable system. The front end is
machine-agnostic: it receives a `Box<dyn Machine>` from
`phosphor_machines::registry`, then drives display, audio, input, debug, save
states, NVRAM, and profiling through trait methods.

That shape has worked well because the SDL2 front end can stay simple. The
cost is that `Machine` has become the place every optional runtime feature is
added. Today `Machine` requires several subtraits and also carries optional
methods directly:

- frame execution: `run_frame`, `reset`, `frame_rate_hz`, `machine_id`
- required capability supertraits: `Renderable`, `AudioSource`,
  `InputReceiver`, `MachineDebug`
- optional persistence: `save_state`, `load_state`, `save_nvram`, `load_nvram`
- optional instrumentation: `set_profiling`, `frame_profile_spans`

This is not a correctness bug. It is architectural drift from a compact
frontend contract toward a mixed runtime/capability object.

## Current Architecture

Important code points:

- `core/src/core/machine.rs` defines `Renderable`, `AudioSource`,
  `InputReceiver`, `MachineDebug`, and `Machine`.
- `machines/src/registry.rs` stores `fn(&RomSet) -> Result<Box<dyn Machine>, _>`.
- `frontend/src/main.rs` uses `Machine` for startup-only concerns:
  `display_size`, `input_map`, `load_nvram`, `save_nvram`, and `reset`.
- `frontend/src/emulator.rs` uses `Machine` for per-frame concerns:
  `run_frame`, `render_frame`, `fill_audio`, `debug_bus`, `debug_tick`,
  watchpoints, profiling, overlay stats, vector display, screen rotation.
- `machines/src/lib.rs` contains helper macros:
  `impl_board_delegation!`, `impl_board_renderable!`,
  `impl_board_audio!`, `impl_board_debug!`, and `machine_save_state!`.
- Some machines use the macros (`JoustSystem`, `PacmanSystem`,
  `QbertSystem`, `AsteroidsSystem`); others implement traits manually
  (`GridleeSystem`, `MissileCommandSystem`, `CrystalCastlesSystem`,
  `GalagaSystem`, `DigDugSystem`).

The existing supertrait split is partial. `Renderable`, `AudioSource`,
`InputReceiver`, and `MachineDebug` are distinct traits, but every
front-end-capable machine must implement all of them because `Machine` has
them as supertraits. Defaults make this cheap for audio/debug, but the type
relationship still says every machine is all capabilities.

## Problems

### Capability Creep

Optional capabilities currently land directly on `Machine`.

`save_nvram`, `load_nvram`, `set_profiling`, and `frame_profile_spans` are not
part of the core execution contract. They are frontend services. As more
services are added, `Machine` becomes less precise and harder to reason about.

Likely future examples:

- rewind buffers
- movie recording
- debugger event tracing
- input configuration metadata
- screenshot or video capture metadata
- per-machine DIP switch configuration

### Contract Ambiguity

`Machine` currently means both:

1. "This object can be run as an emulated system."
2. "This object exposes every frontend-facing capability, possibly as no-op
   defaults."

Those meanings are different. A headless test harness may only need execution.
A frontend may need display, input, audio, save-state, and debug. A future tool
might need debug and memory inspection but no rendering.

### Object-Safety Pressure

The current `Box<dyn Machine>` is convenient. A naive split into independent
traits can make the frontend worse if it forces pervasive downcasting or
generic plumbing.

The design should preserve a single registry object for frontend use while
making capability boundaries explicit.

### Boilerplate Drift

`machines/CLAUDE.md` says board-wrapper forwarding should be explicit one-line
methods, but the code now uses macros for board delegation and save-state
boilerplate. The macros are practical, but the guidance is outdated.

The refactor should embrace shallow code-generation/helper macros as the local
pattern. A generic registration or frontend-adapter wrapper would make the
common case look declarative, but current machines differ enough that the
wrapper would quickly need escape hatches for vector display, overlay stats,
debug pre-tick hooks, alternate bus address widths, no-audio systems, NVRAM,
and machine-specific reset/input wiring.

Macros fit the current shape better because they remove repetitive trait
forwarding while leaving hardware behavior visible in each machine file. The
boundary should be strict: macros may generate obvious delegation and standard
save-state/core metadata methods, but they should not hide non-default machine
behavior.

## Design Goals

1. Keep the frontend machine-agnostic.
2. Make the core execution contract small and stable.
3. Represent optional services as explicit capabilities.
4. Avoid runtime downcasting in normal frontend code.
5. Keep machine implementations readable.
6. Preserve current behavior during migration.
7. Make future capabilities additive without expanding the core trait.

## Proposed Architecture

Introduce a small core runtime trait and a separate frontend-facing trait that
bundles capabilities intentionally.

### Core Runtime Trait

Rename the narrow execution contract to `MachineCore`:

```rust
pub trait MachineCore {
    fn run_frame(&mut self);
    fn reset(&mut self);

    fn frame_rate_hz(&self) -> f64 {
        60.0
    }

    fn machine_id(&self) -> &str {
        ""
    }
}
```

This trait is the minimum contract for "an emulated system that advances in
frames."

### Capability Traits

Keep the existing capability traits, but make optional services first-class:

```rust
pub trait Renderable {
    fn display_size(&self) -> (u32, u32);
    fn render_frame(&self, buffer: &mut [u8]);
    fn overlay_stats(&self) -> Option<String> { None }
    fn vector_display_list(&self) -> Option<&[VectorLine]> { None }
    fn screen_rotation(&self) -> ScreenRotation { ScreenRotation::None }
}

pub trait AudioSource {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize { 0 }
    fn audio_sample_rate(&self) -> u32 { 0 }
}

pub trait InputReceiver {
    fn set_input(&mut self, button: u8, pressed: bool);
    fn input_map(&self) -> &[InputButton];
    fn set_analog(&mut self, axis: u8, delta: i32) {}
    fn analog_map(&self) -> &[AnalogInput] { &[] }
}

pub trait SaveState {
    fn save_state(&self) -> Option<Vec<u8>> { None }
    fn load_state(&mut self, data: &[u8]) -> Result<(), SaveError> {
        Err(SaveError::InvalidFormat("save states not supported".into()))
    }
}

pub trait Nvram {
    fn save_nvram(&self) -> Option<&[u8]> { None }
    fn load_nvram(&mut self, data: &[u8]) {}
}

pub trait Profilable {
    fn set_profiling(&mut self, enabled: bool) {}
    fn frame_profile_spans(&self) -> &[ProfileSpan] { &[] }
}
```

`Nvram` is part of the frontend bundle because the frontend owns NVRAM file
loading/saving, but most machines naturally use the default no-NVRAM behavior.
`Profilable` is also part of the frontend bundle because every machine can be
profiled at least at the frame level; machines that do not provide subspans use
the default empty slice.

`MachineDebug` can remain as-is initially. Event tracing should be a separate
`DebugTrace` capability as described in `debug-observability.md`; profiling
should remain separate from tracing for now because frame profile spans and
event history have different UI consumption patterns.

### Frontend Bundle Trait

Define one object-safe bundle for SDL frontend use:

```rust
pub trait FrontendMachine:
    MachineCore
    + Renderable
    + AudioSource
    + InputReceiver
    + MachineDebug
    + SaveState
    + Nvram
    + Profilable
{
}

impl<T> FrontendMachine for T
where
    T: MachineCore
        + Renderable
        + AudioSource
        + InputReceiver
        + MachineDebug
        + SaveState
        + Nvram
        + Profilable
{
}
```

This keeps the frontend simple:

```rust
pub struct MachineEntry {
    pub create: fn(&RomSet) -> Result<Box<dyn FrontendMachine>, RomLoadError>,
}
```

The important change is conceptual: the SDL frontend asks for the full bundle,
but the core execution trait no longer accumulates optional services.

### Compatibility Alias

During migration, keep the existing `Machine` name only as a compatibility
alias or transitional trait:

```rust
pub trait Machine: FrontendMachine {}

impl<T> Machine for T where T: FrontendMachine {}
```

After all call sites move to `FrontendMachine` and `MachineCore`, remove the
alias unless there is a concrete need for it. `FrontendMachine` is intentionally
more specific: it says "this is the full SDL/frontend contract", not "this is
the minimum emulated hardware model." The unqualified `Machine` name should not
be reused during the refactor because it invites confusion between the narrow
core trait and the full frontend bundle.

### Deferred Capabilities

DIP switch configuration and input remapping were deferred past the main
capability refactor as larger semantic changes. **Input configuration is now
implemented** (see the status note below); **DIP switches remain to be done.**

#### DIP Switches

DIP switches should be a frontend capability with default empty behavior,
similar to NVRAM but with structured metadata and mutation:

```rust
pub trait DipSwitches {
    fn dip_switches(&self) -> &[DipSwitchBank] { &[] }
    fn set_dip_switch(&mut self, bank: usize, option: usize, value: u8) {}
}

pub struct DipSwitchBank {
    pub name: &'static str,
    pub value: u8,
    pub options: &'static [DipOption],
}

pub struct DipOption {
    pub name: &'static str,
    pub mask: u8,
    pub choices: &'static [DipChoice],
    pub apply: DipApplyTiming,
}

pub struct DipChoice {
    pub label: &'static str,
    pub value: u8,
}

pub enum DipApplyTiming {
    Immediate,
    OnReset,
}
```

Most arcade machines have DIP switches, but simple/test systems do not.
Defaults keep those systems painless. Machines remain responsible for mapping
selected bits into their board-specific `dip_switches`, `dsw1`, `dsw2`, or
input-port state.

#### Input Configuration

> **Status: implemented.** The `InputConfigurable` capability replaced the
> name-matched `InputReceiver` model, which (along with `InputButton`,
> `AnalogInput`, and the frontend's display-name key matching) has been deleted.
> All machines expose typed `input_controls()` and consume `InputEvent`s via
> `handle_input()`; the frontend builds a `BindingSet` from each control's
> `default_bindings` and persists per-machine overrides (rebindable via the F12
> settings panel). The shape below is the original proposal; the as-built
> design differs in a few details:
>
> - `input_controls()` returns `&'static [InputControl]` (const tables, not a
>   borrowed slice). `InputId` is `InputId(pub u16)`.
> - `InputEvent` is `Button` / `Absolute` / `Relative` (no `Analog`); the
>   bridge-era `Absolute` is currently unused by machines.
> - `InputKind::DigitalDirection { direction }` carries only a `Direction`
>   (the redundant `DigitalAxis` field was dropped); `AnalogAxisKind` is `X`/`Y`.
> - Core stays SDL-free: `DefaultBinding` is `Key(KeyId)` / `Pad(PadControl)` /
>   `Mouse(MouseControl)` with portable enums; the frontend
>   ([frontend/src/input.rs](../../frontend/src/input.rs)) owns the SDL
>   `PhysicalInput` / `InputBinding` / `BindingSet` types and translates the
>   portable descriptors to SDL scancodes/buttons/axes.
> - Shared default bindings live in
>   [machines/src/input_defaults.rs](../../machines/src/input_defaults.rs) so
>   every machine's defaults come from one place.
> - Trackball / spinner / analog-stick controls use `InputId`s distinct from the
>   digital ids (one `InputId` namespace, unlike the old split button/analog ids).

The current input model is intentionally not expanded in the main refactor,
but it should be replaced afterward. The existing `InputButton { id, name }`
and name-matching defaults are brittle because display text is being used as
configuration identity.

The follow-up should introduce typed logical controls with stable names:

```rust
pub trait InputConfigurable {
    fn input_controls(&self) -> &[InputControl];
    fn handle_input(&mut self, event: InputEvent);
}

pub struct InputControl {
    pub id: InputId,
    pub stable_name: &'static str,
    pub label: &'static str,
    pub kind: InputKind,
    pub player: Option<u8>,
    pub default_bindings: &'static [DefaultBinding],
}

pub enum InputKind {
    Button,
    DigitalDirection { axis: DigitalAxis, direction: Direction },
    AnalogAxis { axis: AnalogAxisKind },
    Service,
    Coin,
    Start,
}

pub enum InputEvent {
    Button { id: InputId, pressed: bool },
    Analog { id: InputId, value: f32 },
    Relative { id: InputId, delta: f32 },
}
```

The frontend should bind physical inputs to stable logical controls, not to
machine-specific display names:

```rust
pub enum PhysicalInput {
    Key(Scancode),
    ControllerButton { controller: ControllerSelector, button: Button },
    ControllerAxis { controller: ControllerSelector, axis: Axis, direction: AxisDirection },
    MouseButton(MouseButton),
    MouseAxis { axis: MouseAxis },
}

pub struct InputBinding {
    pub physical: PhysicalInput,
    pub target: InputId,
    pub scale: f32,
    pub deadzone: f32,
}
```

This supports persistent per-machine/per-controller mappings, multiple
bindings per action, analog scaling/inversion, relative trackball/spinner
motion, and combo controls without substring hacks. Machines still own the
hardware semantics: active-high/active-low bits, multiplexed inputs, input
ports, PIA wiring, and trackball counters.

## Implementation Plan

### Phase 1: Introduce Traits Without Behavior Changes

Edit `core/src/core/machine.rs`:

1. Add `MachineCore`.
2. Add `SaveState`, `Nvram`, and `Profilable`.
3. Add `FrontendMachine` blanket impl.
4. Change `Machine` into a compatibility alias trait for `FrontendMachine`.

Then update `core/src/core/mod.rs` and `core/src/lib.rs` re-exports.

Expected compile fallout should be small because existing machine types already
have the required methods on `Machine`; the methods will need to move into the
new trait impls.

### Phase 2: Split Machine Implementations

For each machine:

1. Change `impl Machine for FooSystem` to `impl MachineCore for FooSystem`.
2. Move `save_state`/`load_state` into `impl SaveState`.
3. Move `save_nvram`/`load_nvram` into `impl Nvram` only when overridden.
4. Move `set_profiling`/`frame_profile_spans` into `impl Profilable` only when
   overridden.
5. Add empty/default impls where needed:

```rust
impl SaveState for PacmanSystem {
    crate::machine_save_state!("pacman", namco_pac::TIMING);
}

impl Nvram for PacmanSystem {}
impl Profilable for PacmanSystem {}
```

The empty impls are explicit but may feel repetitive. If they become noise,
add a small macro:

```rust
crate::impl_default_frontend_capabilities!(PacmanSystem);
```

Do not hide non-default behavior in that macro.

`Profilable` should normally be implemented for every frontend machine, even
when it only uses default no-op methods. `Nvram` should also be available
through the frontend bundle, but only machines with actual battery-backed RAM
need non-default implementations.

### Phase 3: Refine Helper Macros

Replace `machine_save_state!` with a macro scoped to `impl SaveState` plus a
separate macro for the core identity/timing methods:

```rust
macro_rules! machine_core_metadata {
    ($id:expr, $timing:expr) => {
        fn frame_rate_hz(&self) -> f64 { $timing.frame_rate_hz() }
        fn machine_id(&self) -> &str { $id }
    };
}

macro_rules! machine_save_state {
    () => {
        fn save_state(&self) -> Option<Vec<u8>> {
            Some(phosphor_core::core::save_state::save_machine(
                self,
                self.machine_id(),
            ))
        }

        fn load_state(&mut self, data: &[u8]) -> Result<(), SaveError> {
            let id = self.machine_id().to_string();
            phosphor_core::core::save_state::load_machine(self, &id, data)
        }
    };
}
```

This prevents save-state support from being entangled with core identity.

Keep the board-delegation macros as the accepted implementation pattern for
machines that are thin wrappers around a board. Refine them around the new
capability traits instead of replacing them with a generic frontend wrapper:

1. `impl_board_renderable!` continues to generate display delegation, including
   vector-list or overlay-stat variants.
2. `impl_board_audio!` continues to generate either board audio delegation or
   an empty no-audio implementation.
3. `impl_board_debug!` continues to generate debug bus/tick delegation,
   including `debug_tick_pre` and explicit bus address variants.
4. New small macros may cover default `Nvram`/`Profilable`/`SaveState`
   implementations, but only for behavior that is truly default.

Do not introduce a generic `FrontendMachineAdapter<T>` as part of this
refactor. It would move boilerplate out of macros, but it would also require a
driver trait broad enough to mirror the frontend bundle and machine-specific
exceptions. That shifts complexity rather than reducing it.

### Phase 4: Rename Frontend Types

Update:

- `machines/src/registry.rs`: `Box<dyn Machine>` to
  `Box<dyn FrontendMachine>`.
- `frontend/src/main.rs`: local return type to `Box<dyn FrontendMachine>`.
- `frontend/src/emulator.rs`: argument type to `&mut dyn FrontendMachine`.

At this point the frontend remains just as machine-agnostic as it is now.

### Phase 5: Update Documentation

Update:

- `README.md` architecture section
- `CLAUDE.md` workspace crate descriptions if needed
- `machines/CLAUDE.md` board-wrapper pattern

The machines guidance should acknowledge that delegation macros are now the
accepted local pattern when they stay shallow and obvious. It should also call
out the rule that machine-specific behavior belongs in normal Rust methods or
trait impls, not inside a widening macro option language.

### Phase 6: Add Post-Refactor Capabilities

After the core split lands and the frontend uses `FrontendMachine`, add the
larger semantic capabilities:

1. `DipSwitches` for structured DIP switch metadata and settings. *(Not yet
   implemented.)*
2. `InputConfigurable` for stable logical controls and persistent bindings.
   **Implemented** — see the Input Configuration status note above.

These should not block the main refactor because they require frontend UI,
config-file, and per-machine metadata work.

## Migration Order

Recommended order:

1. `core/src/core/machine.rs` trait additions and re-exports.
2. `machines/src/lib.rs` macro split.
3. Convert one macro-based machine (`PacmanSystem`) as the pilot.
4. Convert one manual machine (`GridleeSystem`) as the pilot.
5. Convert remaining machines mechanically.
6. Update frontend and registry type names.
7. Remove the `Machine` compatibility alias once all code uses
   `MachineCore`/`FrontendMachine`.
8. Add `DipSwitches` and `InputConfigurable` as follow-up capabilities.

## Testing

Run after phase 1 and again after full migration:

```bash
cargo test -p phosphor-core
cargo test -p phosphor-machines
cargo test -p phosphor-frontend
cargo clippy --all-features --all-targets
```

Add at least one compile-time smoke test in `machines/tests` that constructs a
registered machine type as `&dyn FrontendMachine` if the registry type change
does not already cover this.

## Closed Decisions

1. Use `FrontendMachine` for the full frontend bundle. Keep `Machine` only as a
   temporary compatibility alias during migration, then remove it unless a
   concrete new use appears.
2. `Nvram` remains a default-empty frontend capability because not every
   machine has battery-backed RAM. `Profilable` remains a default-empty
   frontend capability because every machine can be profiled at least at frame
   granularity.
3. Keep profiling as a separate frontend capability for now. It may share
   internal instrumentation primitives with `DebugTraceBuffer` later, but it
   should not merge into `DebugTrace` immediately.
4. DIP switches and input configuration are accepted follow-up capabilities
   after the main machine trait refactor, not part of the first split.
5. Embrace the existing code-generation macro direction. Refine the macros
   around the new capability traits and keep them shallow; do not replace them
   with a generic registration/frontend-adapter wrapper.

## Recommendation

Do the refactor. Refactor cost is low enough for this project, and the design
will make the next optional frontend feature cleaner. Keep a single
frontend-facing trait object named `FrontendMachine`, but split the source
traits so `MachineCore` stays small and optional capabilities stop expanding
it. Keep the shallow delegation macros as the implementation pattern for
board-wrapper machines, and add DIP switch and typed input configuration
capabilities after this foundation is in place.
