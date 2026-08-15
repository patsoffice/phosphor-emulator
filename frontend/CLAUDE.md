# phosphor-frontend

SDL2-based display, audio, and input handling. This is the only crate with external C dependencies.

## Structure

- `main.rs` - Entry point, CLI arg parsing, machine instantiation, ROM/state management
- `emulator.rs` - Main loop, frame timing, machine dispatch, SDL event handling
- `video.rs` - SDL2 texture rendering, egui integration, GL context setup
- `audio.rs` - SDL2 audio callback
- `input.rs` - Physical-input binding layer: `PhysicalInput`/`InputBinding`/`BindingSet`, built from each machine's typed `InputControl` default bindings; SDL events dispatch as `InputEvent`s to `machine.handle_input()`
- `host_keys.rs` - Rebindable *emulator* hotkeys (as opposed to game input): `HostAction`/`HostBindings` plus the `DEFAULTS` table. `DEFAULTS` is the authority for which key does what — `emulator.rs` dispatches on `HostAction`, so a comment there naming a concrete key is only a hint and goes stale silently. `HostBindings` maps one *bare* scancode per action by design, so the one modifier chord (Ctrl+`` ` ``, the console toggle) stays hardcoded in `emulator.rs` and is not rebindable. Also reports hotkeys that shadow a machine's own controls
- `overlay.rs` - FPS counter, debug overlay
- `debug_ui.rs` - CPU debug panels, breakpoints, disassembly, memory viewer (egui)
- `console_ui.rs` - interactive Rhai console side panel (Ctrl+`` ` `` to toggle); captures a typed command in the egui closure, and `emulator.rs` evaluates it against the *live* machine after the frame via the shared `phosphor-script` engine (the emulator owns its machine as an `Rc<RefCell<DebugSession>>` so the console binds the same handle)
- `profile.rs` - Profiler side panel (F8): per-frame input/emulation/audio/render/idle timings, machine-supplied `ProfileSpan` sub-spans, and Chrome Trace Event export
- `settings_ui.rs` - egui settings panels: the input rebinding panel (Tab) with click-to-capture for both machine controls and host hotkeys, the DIP switch panel (`` ` ``), and the key legend (`?`). Bindings persist per machine in `state.toml`
- `state.rs` - Auto-saved session state (`state.toml`): window position + a unified per-machine `[machines.<name>]` section (`MachineSettings`: per-game config overrides for scale/ROM/NVRAM/save paths, plus diff-only DIP + input-binding state). Keyed by registry/CLI name; migrates legacy top-level `input_bindings`/`dip_switches` maps on load
- `config.rs` - Persistent user config from `~/.config/phosphor/config.toml` (`rom_path`, `nvram_path`, `save_path`). Distinct from `state.rs`, which is auto-saved session state rather than user-authored preferences
- `vector_gl.rs` - OpenGL vector display renderer (for DVG/AVG machines)
- `gfxview.rs` - Interactive charset/sprite viewer (`--gfxview`), showing the sheets a machine exposes via `MachineCore::gfx_sheets` — the caches it already decoded from ROM, so any tile-based machine is viewable with no per-machine registration
- `screenshot.rs` - Writes an RGB24 framebuffer to a timestamped PNG (F12)
- `headless.rs` - Windowless capture: run N frames, write the final framebuffer to a PNG and produced audio to a WAV, for bring-up and regression checks without SDL. The frame-stepping/ROM-loading half lives in `phosphor-harness`

ROM path resolution (directory / ZIP / loose files) lives in the shared `phosphor-harness` crate (`load_rom_set`), used here and by the disasm tools.

## Dependencies

- Requires SDL2. The Nix dev shell (`nix develop`) provides it and is the source of truth for the build environment; outside Nix on macOS, `brew install sdl2`
- `.cargo/config.toml` configures the Homebrew library path for aarch64-apple-darwin
- Uses egui/GL for debug UI panels and vector display rendering
- ROM archive (ZIP) loading is handled by `phosphor-harness`
- Embeds `phosphor-script` (and `rhai`) for the interactive console, binding its engine to the live machine
