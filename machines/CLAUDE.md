# phosphor-machines

Arcade and system board implementations. Each machine implements the `Bus` trait to connect CPUs to memory, I/O, and peripherals.

## Adding a New Machine

- Implement the `Bus` trait for your system struct
- Use the borrow-splitting `unsafe` pattern for `tick()` (CPU and bus access disjoint memory)
- ROM loading goes through `rom_loader.rs` utilities (ZIP extraction is handled by the frontend's `rom_path.rs`)
- Register it with one `crate::register_machine!(JoustSystem, "joust", &["joust"], JOUST_CONTROLS);` — wrapper type, CLI name, ROM set names, control table. The macro emits the factory and the `inventory::submit!`; don't hand-write either. Two extra arms: `new = Type::new(arg)` when the constructor takes a hardware variant, and `configs = ALL_CONFIGS` when several ROM revisions are tried in turn. A factory that does anything more (a reset after load, a non-standard loader) stays hand-written — see `starwars.rs` and `quantum.rs`
- Video rendering is per-scanline during `run_frame()`
- Optional: to make the machine's code ROMs disassemblable by the `disasm` tool, add one `inventory::submit! { DisasmRegion { ... } }` per code region (see `disasm_registry.rs` and the entries in `mario_bros.rs`) — maps a region name to its CPU, origin, and a `RomRegion` loader. Purely additive; doesn't touch `MachineEntry`/`FrontendMachine`.

## Machine Traits

Every frontend-playable machine implements `MachineCore` (`run_frame`/`reset`/`frame_rate_hz`/`machine_id`) plus the capability traits `Renderable`, `AudioSource`, `InputConfigurable`, `MachineDebug`, `SaveState`, `Nvram`, and `Profilable`. A blanket impl in phosphor-core bundles all of these into the object-safe `FrontendMachine` that the registry and frontend use — never implement `FrontendMachine` directly.

Typical layout:

```rust
impl MachineCore for PacmanSystem {
    crate::machine_core_metadata!("pacman", namco_pac::TIMING);
    fn run_frame(&mut self) { /* game-specific */ }
    fn reset(&mut self) { /* game-specific */ }
}

impl SaveState for PacmanSystem {
    crate::machine_save_state!();
}

impl InputConfigurable for PacmanSystem {
    fn input_controls(&self) -> &'static [InputControl] { /* typed control table */ }
    fn handle_input(&mut self, event: InputEvent) { /* apply to hardware state */ }
}

// No battery RAM and no sub-span profiling → default empty impls
crate::impl_default_frontend_capabilities!(PacmanSystem);
```

Machines with battery-backed RAM write `impl Nvram` by hand (plus `impl Profilable for X {}`); machines with sub-span profiling write `impl Profilable` by hand (see `qbert.rs`).

### Input

Input is fully typed (the old name-matched `InputReceiver` / `InputButton` model is gone). Each machine defines a `&'static [InputControl]` table — stable name, label, `InputKind`, owning player, and `default_bindings` — and implements `InputConfigurable::handle_input(InputEvent)` to apply events to its hardware input state (port bits, trackball accumulators, etc.). Reuse the shared default-binding constants in `input_defaults.rs` (P1/P2 directions, coin/start, fire, …) so defaults stay consistent; give analog axes `InputId`s distinct from the digital controls (a single `InputId` namespace). The frontend builds key/pad/mouse bindings from `default_bindings`; nothing keys off display text.

## Board Wrapper Pattern

Games sharing hardware (e.g. Joust/Robotron on Williams, Pac-Man/Ms. Pac-Man on Namco Pac) use a two-level structure:

1. **Board struct** (e.g. `WilliamsBoard`, `NamcoPacBoard`) — owns CPUs, memory, and devices. Provides inherent methods: `render_frame()`, `fill_audio()`, `tick()`, etc.
2. **Game wrapper struct** (e.g. `JoustSystem`) — owns a `board` field plus game-specific state. Implements `MachineCore` and the capability traits, forwarding to the board.

Obvious delegation is macro-generated, not hand-written. `impl_board_delegation!(Type, board, TIMING, ...)` expands to `Renderable` + `AudioSource` + `MachineDebug` impls with optional flags (`no_audio`, `vectors`, `overlay_stats`, `debug_tick_pre`, `bus_addr: T`); standalone single-CPU machines use `impl_standalone_debug!`. The boundary is strict: macros may generate obvious delegation and standard save-state/core-metadata methods, but machine-specific behavior (`run_frame`, `reset`, `handle_input`, NVRAM mapping, profiling) belongs in normal trait impls in the machine file — don't widen the macro option language to hide it.

See `joust.rs` (macro-based) and `gridlee.rs` (manual impls) as reference examples.

## Shared Board Modules

Games sharing hardware use a shared board struct. When adding a new game on existing hardware, use the appropriate board:

- `williams.rs` - Williams 2nd-gen (M6809 + M6800 sound)
- `namco_pac.rs` - Namco Pac-Man (Z80 + Namco WSG)
- `namco_galaga.rs` - Namco Galaga (Z80 + Namco audio)
- `tkg04.rs` - Nintendo TKG-04 (Z80 + I8035 + DMA)
- `mcr2.rs` - Bally Midway MCR II (Z80 + SSIO + CTC)
- `atari_dvg.rs` - Atari DVG vector (M6502 + DVG)
- `gottlieb.rs` - Gottlieb System 80 (I8088 + M6502 sound)

## Reference Examples

- `joust.rs` - Reference for Board Wrapper Pattern (Williams board)
- `simple6502.rs`, `simple6800.rs`, `simple6809.rs`, `simplez80.rs` - Minimal test harnesses
