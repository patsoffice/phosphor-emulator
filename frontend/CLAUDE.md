# phosphor-frontend

SDL2-based display, audio, and input handling. This is the only crate with external C dependencies.

## Structure

- `main.rs` - Entry point, CLI arg parsing, machine instantiation, ROM/state management
- `emulator.rs` - Main loop, frame timing, machine dispatch, SDL event handling
- `video.rs` - SDL2 texture rendering, egui integration, GL context setup
- `audio.rs` - SDL2 audio callback
- `input.rs` - Physical-input binding layer: `PhysicalInput`/`InputBinding`/`BindingSet`, built from each machine's typed `InputControl` default bindings; SDL events dispatch as `InputEvent`s to `machine.handle_input()`
- `overlay.rs` - FPS counter, debug overlay
- `debug_ui.rs` - CPU debug panels, breakpoints, disassembly, memory viewer (egui)
- `console_ui.rs` - interactive Rhai console side panel (Ctrl+`` ` `` to toggle); captures a typed command in the egui closure, and `emulator.rs` evaluates it against the *live* machine after the frame via the shared `phosphor-script` engine (the emulator owns its machine as an `Rc<RefCell<DebugSession>>` so the console binds the same handle)
- `settings_ui.rs` - egui settings panels (F12); input rebinding panel with click-to-capture, persisted per machine in `state.toml`
- `state.rs` - Auto-saved session state (`state.toml`): window position + a unified per-machine `[machines.<name>]` section (`MachineSettings`: per-game config overrides for scale/ROM/NVRAM/save paths, plus diff-only DIP + input-binding state). Keyed by registry/CLI name; migrates legacy top-level `input_bindings`/`dip_switches` maps on load
- `vector_gl.rs` - OpenGL vector display renderer (for DVG machines)

ROM path resolution (directory / ZIP / loose files) lives in the shared `phosphor-harness` crate (`load_rom_set`), used here and by the disasm tools.

## Dependencies

- Requires SDL2: `brew install sdl2`
- `.cargo/config.toml` configures the Homebrew library path for aarch64-apple-darwin
- Uses egui/GL for debug UI panels and vector display rendering
- ROM archive (ZIP) loading is handled by `phosphor-harness`
- Embeds `phosphor-script` (and `rhai`) for the interactive console, binding its engine to the live machine
