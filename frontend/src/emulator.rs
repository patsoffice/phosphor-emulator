use std::path::Path;
use std::time::{Duration, Instant};

use phosphor_core::core::machine::{FrontendMachine, InputEvent, InputKind};
use sdl2::event::Event;
use sdl2::keyboard::Scancode;

use crate::debug_ui::{self, DebugState, RunMode};
use crate::input::{AxisDir, BindingSet, MouseAxis, PhysicalInput};
use crate::profile::ProfileState;
use crate::settings_ui::{self, SettingsState};
use crate::video::Video;

/// Combined width of all active right-side panels, used when resizing the window.
fn panels_width(debug: &DebugState, profile: &ProfileState, settings: &SettingsState) -> u32 {
    let dw = if debug.active {
        debug.debug_panel_width()
    } else {
        0
    };
    let pw = if profile.active {
        crate::profile::PANEL_WIDTH
    } else {
        0
    };
    // The input and DIP panels are independent side panels that stack.
    let sw = (settings.active as u32 + settings.dip_active as u32) * settings_ui::PANEL_WIDTH;
    dw + pw + sw
}

/// Translate an SDL event into a physical input for rebind capture, if it is a
/// bindable press (key, gamepad button, or mouse button).
fn capture_physical(event: &Event) -> Option<PhysicalInput> {
    match event {
        Event::KeyDown {
            scancode: Some(sc), ..
        } => Some(PhysicalInput::Key(*sc)),
        Event::ControllerButtonDown { button, .. } => Some(PhysicalInput::PadButton(*button)),
        Event::MouseButtonDown { mouse_btn, .. } => {
            Some(PhysicalInput::MouseButtonInput(*mouse_btn))
        }
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    machine: &mut dyn FrontendMachine,
    bindings: &mut BindingSet,
    scale: u32,
    fullscreen: bool,
    save_path: &Path,
    screenshot_dir: &Path,
    machine_name: &str,
    start_in_debug: bool,
    start_in_profile: bool,
    no_mouse_grab: bool,
    record_wav: Option<&str>,
    state: &mut crate::state::State,
) {
    // Enable controller backends before SDL init — needed for Xbox on macOS
    sdl2::hint::set("SDL_JOYSTICK_HIDAPI", "1");
    sdl2::hint::set("SDL_JOYSTICK_HIDAPI_XBOX", "1");
    sdl2::hint::set("SDL_JOYSTICK_MFI", "1");

    // Implement relative mouse mode via cursor warping rather than raw input.
    // On macOS the default raw-input path does not reliably deliver motion
    // deltas from a trackpad (the trackpad is an absolute device, so the locked
    // cursor never reports movement), which breaks trackball/spinner games
    // (Crystal Castles, Missile Command, Tempest, ...). Warp-based relative mode
    // synthesizes deltas uniformly for both mice and trackpads. See
    // https://github.com/libsdl-org/SDL/issues/5340.
    sdl2::hint::set("SDL_MOUSE_RELATIVE_MODE_WARP", "1");

    let sdl_context = sdl2::init().expect("Failed to initialize SDL2");
    let sdl_video = sdl_context.video().expect("Failed to init SDL video");
    let sdl_audio = sdl_context.audio().expect("Failed to init SDL audio");

    // Initialize game controller and joystick subsystems for joypad support
    let controller_subsystem = sdl_context
        .game_controller()
        .expect("Failed to init SDL game controller");
    let joystick_subsystem = sdl_context.joystick().expect("Failed to init SDL joystick");

    // Load community controller database (gamecontrollerdb.txt) if present.
    // Download from: https://github.com/mdqinc/SDL_GameControllerDB
    let db_paths: Vec<std::path::PathBuf> = {
        let mut paths = vec![std::path::PathBuf::from("gamecontrollerdb.txt")];
        if let Some(home) = std::env::var_os("HOME") {
            paths.push(std::path::Path::new(&home).join(".config/phosphor/gamecontrollerdb.txt"));
        }
        paths
    };
    for path in &db_paths {
        if path.exists() {
            match controller_subsystem.load_mappings(path) {
                Ok(n) => eprintln!("Loaded {n} controller mappings from {}", path.display()),
                Err(e) => eprintln!("Failed to load {}: {e}", path.display()),
            }
        }
    }
    let mut controllers: Vec<sdl2::controller::GameController> = Vec::new();
    let num_joysticks = joystick_subsystem.num_joysticks().unwrap_or(0);
    if num_joysticks == 0 {
        eprintln!("No joysticks detected");
    }
    for i in 0..num_joysticks {
        let name = joystick_subsystem
            .name_for_index(i)
            .unwrap_or_else(|_| "unknown".into());
        if controller_subsystem.is_game_controller(i) {
            if let Ok(gc) = controller_subsystem.open(i) {
                eprintln!("Controller {i}: {}", gc.name());
                controllers.push(gc);
            } else {
                eprintln!("Controller {i}: {name} (failed to open)");
            }
        } else {
            eprintln!("Joystick {i}: {name} (not in controller database)");
        }
    }

    let (width, height) = machine.display_size();
    // The native texture stays (width, height); the window is sized to the
    // machine's target display aspect (4:3 tube, rotated to 3:4 for portrait
    // cabinets) so the GPU corrects pixel aspect at presentation time. Rotated
    // displays (e.g. Tempest) additionally swap axes — all handled by
    // `presentation`. `view_aspect` (as-viewed w/h) drives every letterbox.
    let rotated = machine.orientation().swaps_axes();
    let (win_w, win_h, view_aspect) =
        presentation(width, height, machine.display_aspect(), rotated);
    let window_pos = state.window_x.zip(state.window_y);
    let mut video = Video::new(
        &sdl_video,
        "Phosphor Emulator",
        width,
        height,
        win_w,
        win_h,
        scale,
        window_pos,
        fullscreen,
    );
    let mut event_pump = sdl_context.event_pump().expect("Failed to get event pump");

    // Detect vector display machines and create GL renderer.
    let mut vector_renderer = machine
        .vector_display_list()
        .map(|_| crate::vector_gl::VectorRenderer::new());

    let audio_state = crate::audio::init(&sdl_audio, machine.audio_sample_rate());
    let mut audio_started = false;

    let buffer_size = (width * height * 3) as usize;
    let mut framebuffer = vec![0u8; buffer_size];
    let mut audio_scratch = vec![0i16; 2048];
    // Optional live-gameplay audio recording: tee every produced sample here and
    // write the WAV on exit. `Some` only when `--record-wav` was passed.
    let mut audio_recording: Option<Vec<i16>> = record_wav.map(|_| Vec::new());

    let frame_duration = Duration::from_secs_f64(1.0 / machine.frame_rate_hz());
    let mut next_frame_time = Instant::now() + frame_duration;
    let mut throttle = true;
    let mut last_render_time = Instant::now();

    // FPS overlay state (F10 to toggle)
    let mut show_fps = false;
    let mut fps_text = String::new();
    let mut fps_smoothed: f64 = machine.frame_rate_hz();
    let mut fps_last_instant = Instant::now();

    // Profiler state (F8 to toggle)
    let mut profile_state = crate::profile::ProfileState::new();

    // Input settings panel (Tab to toggle); only meaningful for machines with
    // typed controls.
    let mut settings_state = SettingsState::default();
    let has_typed_controls = !machine.input_controls().is_empty();

    // DIP switch panel (` to toggle); only for machines with DIP banks.
    let has_dip = !machine.dip_banks().is_empty();

    // Mouse grab for trackball games (F11 to toggle)
    let has_analog = machine
        .input_controls()
        .iter()
        .any(|c| matches!(c.kind, InputKind::AnalogAxis { .. }));
    let mut mouse_grabbed = false;
    if has_analog && !no_mouse_grab {
        sdl_context.mouse().set_relative_mouse_mode(true);
        mouse_grabbed = true;
    }

    // Debug state
    let has_debug = machine.debug_bus().is_some();
    let mut debug_state = DebugState::new();
    if let Some(bus) = machine.debug_bus() {
        debug_state.refresh(bus);
    }
    if start_in_debug && has_debug {
        debug_state.active = true;
        debug_state.run_mode = RunMode::Paused;
    }
    if start_in_profile {
        machine.set_profiling(true);
        profile_state.start();
    }
    // Resize window if any side panels are active at startup
    {
        let panels = panels_width(&debug_state, &profile_state, &settings_state);
        if panels > 0 {
            video.resize_window(win_w * scale + panels, win_h * scale);
        }
    }

    'main: loop {
        let t0 = Instant::now();

        // Poll all pending SDL events, translate to machine input
        for event in event_pump.poll_iter() {
            // Forward every event to egui first
            video.process_event(event.clone());

            // Rebind capture: while awaiting an input for a control, consume the
            // next bindable press (Esc cancels) instead of routing it to the game.
            if let Some(target) = settings_state.capturing {
                match &event {
                    Event::KeyDown {
                        scancode: Some(Scancode::Escape),
                        ..
                    } => {
                        settings_state.capturing = None;
                        continue;
                    }
                    _ => {
                        if let Some(physical) = capture_physical(&event) {
                            bindings.rebind(target, physical);
                            settings_state.capturing = None;
                            continue;
                        }
                    }
                }
            }

            match event {
                Event::Quit { .. } => break 'main,

                Event::KeyDown {
                    scancode: Some(Scancode::Escape),
                    ..
                } => break 'main,

                // F1: Toggle debug mode
                Event::KeyDown {
                    scancode: Some(Scancode::F1),
                    repeat: false,
                    ..
                } if has_debug => {
                    debug_state.active = !debug_state.active;
                    if debug_state.active {
                        if let Some(bus) = machine.debug_bus() {
                            debug_state.refresh(bus);
                        }
                        debug_state.run_mode = RunMode::Paused;
                    } else {
                        debug_state.run_mode = RunMode::Running;
                    }
                    video.resize_window(
                        win_w * scale + panels_width(&debug_state, &profile_state, &settings_state),
                        win_h * scale,
                    );
                }

                // 7: Step instruction (debug + paused)
                Event::KeyDown {
                    scancode: Some(Scancode::Num7),
                    repeat: false,
                    ..
                } if debug_state.active && debug_state.run_mode == RunMode::Paused => {
                    debug_state.run_mode = RunMode::StepInstruction;
                }

                // 8: Step cycle (debug + paused)
                Event::KeyDown {
                    scancode: Some(Scancode::Num8),
                    repeat: false,
                    ..
                } if debug_state.active && debug_state.run_mode == RunMode::Paused => {
                    debug_state.run_mode = RunMode::StepCycle;
                }

                // 9: Step frame — run one frame, pause at the next frame start
                // (debug + paused)
                Event::KeyDown {
                    scancode: Some(Scancode::Num9),
                    repeat: false,
                    ..
                } if debug_state.active && debug_state.run_mode == RunMode::Paused => {
                    debug_state.run_mode = RunMode::StepFrame;
                }

                // 0: Toggle run <-> pause (running -> paused, otherwise continue)
                Event::KeyDown {
                    scancode: Some(Scancode::Num0),
                    repeat: false,
                    ..
                } if debug_state.active => {
                    if debug_state.run_mode == RunMode::Running {
                        debug_state.run_mode = RunMode::Paused;
                    } else {
                        debug_state.run_mode = RunMode::Running;
                        debug_state.last_watchpoint_hit = None;
                    }
                }

                Event::KeyDown {
                    scancode: Some(Scancode::F5),
                    repeat: false,
                    ..
                } => {
                    machine.reset();
                    debug_state.frame_count = 0;
                }

                // Quick Save (F6)
                Event::KeyDown {
                    scancode: Some(Scancode::F6),
                    repeat: false,
                    ..
                } => {
                    if let Some(data) = machine.save_state() {
                        match std::fs::write(save_path, &data) {
                            Ok(()) => eprintln!("Save state written ({} bytes)", data.len()),
                            Err(e) => eprintln!("Save state failed: {e}"),
                        }
                    } else {
                        eprintln!("Save states not supported for this machine");
                    }
                }

                // Quick Load (F7)
                Event::KeyDown {
                    scancode: Some(Scancode::F7),
                    repeat: false,
                    ..
                } => match std::fs::read(save_path) {
                    Ok(data) => match machine.load_state(&data) {
                        Ok(()) => eprintln!("Save state loaded"),
                        Err(e) => eprintln!("Load state failed: {e}"),
                    },
                    Err(e) => eprintln!("No save file found: {e}"),
                },

                // F8: Toggle profiler
                Event::KeyDown {
                    scancode: Some(Scancode::F8),
                    repeat: false,
                    ..
                } => {
                    if profile_state.active {
                        machine.set_profiling(false);
                        profile_state.stop();
                    } else {
                        machine.set_profiling(true);
                        profile_state.start();
                    }
                    video.resize_window(
                        win_w * scale + panels_width(&debug_state, &profile_state, &settings_state),
                        win_h * scale,
                    );
                }

                // Tab: Toggle input settings panel (machines with typed controls)
                Event::KeyDown {
                    scancode: Some(Scancode::Tab),
                    repeat: false,
                    ..
                } if has_typed_controls => {
                    settings_state.active = !settings_state.active;
                    settings_state.capturing = None;
                    video.resize_window(
                        win_w * scale + panels_width(&debug_state, &profile_state, &settings_state),
                        win_h * scale,
                    );
                }

                // Backtick (`): Toggle DIP switch panel (machines with DIP banks)
                Event::KeyDown {
                    scancode: Some(Scancode::Grave),
                    repeat: false,
                    ..
                } if has_dip => {
                    settings_state.dip_active = !settings_state.dip_active;
                    video.resize_window(
                        win_w * scale + panels_width(&debug_state, &profile_state, &settings_state),
                        win_h * scale,
                    );
                }

                Event::KeyDown {
                    scancode: Some(Scancode::F9),
                    repeat: false,
                    ..
                } => {
                    throttle = !throttle;
                    if throttle {
                        next_frame_time = Instant::now() + frame_duration;
                    }
                }

                Event::KeyDown {
                    scancode: Some(Scancode::F10),
                    repeat: false,
                    ..
                } => {
                    show_fps = !show_fps;
                    fps_smoothed = machine.frame_rate_hz();
                    fps_last_instant = Instant::now();
                }

                // Mouse grab toggle (F11)
                Event::KeyDown {
                    scancode: Some(Scancode::F11),
                    repeat: false,
                    ..
                } => {
                    mouse_grabbed = !mouse_grabbed;
                    sdl_context.mouse().set_relative_mouse_mode(mouse_grabbed);
                }

                // P: Toggle global pause (frontend-level control, not a game input)
                Event::KeyDown {
                    scancode: Some(Scancode::P),
                    repeat: false,
                    ..
                } => {
                    debug_state.global_paused = !debug_state.global_paused;
                    eprintln!(
                        "{}",
                        if debug_state.global_paused {
                            "Paused"
                        } else {
                            "Resumed"
                        }
                    );
                }

                // Screenshot (F12)
                Event::KeyDown {
                    scancode: Some(Scancode::F12),
                    repeat: false,
                    ..
                } => {
                    machine.render_frame(&mut framebuffer);
                    match crate::screenshot::save_screenshot(
                        &framebuffer,
                        width,
                        height,
                        screenshot_dir,
                        machine_name,
                    ) {
                        Ok(path) => eprintln!("Screenshot saved: {}", path.display()),
                        Err(e) => eprintln!("Screenshot failed: {e}"),
                    }
                }

                // Keyboard input — only pass to game if egui doesn't want it
                Event::KeyDown {
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } if !video.wants_keyboard() => {
                    for id in bindings.digital_targets(PhysicalInput::Key(sc)) {
                        machine.handle_input(InputEvent::Button { id, pressed: true });
                    }
                }

                // Releases are dispatched unconditionally — even if egui now
                // wants the keyboard. The key-down above is gated on
                // `!wants_keyboard()`, so if egui grabs focus while a game key is
                // held (e.g. held arrow keys move egui's widget focus, flipping
                // `wants_keyboard()` true), a guarded key-up would be dropped and
                // the button would stick "on". An extra release for a key the
                // game never saw pressed is harmless (idempotent).
                Event::KeyUp {
                    scancode: Some(sc), ..
                } => {
                    for id in bindings.digital_targets(PhysicalInput::Key(sc)) {
                        machine.handle_input(InputEvent::Button { id, pressed: false });
                    }
                }

                // Game controller button press/release (egui never intercepts these)
                Event::ControllerButtonDown { button, .. } => {
                    for id in bindings.digital_targets(PhysicalInput::PadButton(button)) {
                        machine.handle_input(InputEvent::Button { id, pressed: true });
                    }
                }

                Event::ControllerButtonUp { button, .. } => {
                    for id in bindings.digital_targets(PhysicalInput::PadButton(button)) {
                        machine.handle_input(InputEvent::Button { id, pressed: false });
                    }
                }

                // Game controller analog stick → digital directions
                Event::ControllerAxisMotion { axis, value, .. } => {
                    let normalized = value as f32 / 32_768.0;
                    for (id, dir, deadzone) in bindings.pad_axis_targets(axis) {
                        let pressed = match dir {
                            AxisDir::Positive => normalized > deadzone,
                            AxisDir::Negative => normalized < -deadzone,
                        };
                        machine.handle_input(InputEvent::Button { id, pressed });
                    }
                }

                // Controller hotplug
                Event::ControllerDeviceAdded { which, .. } => {
                    if let Ok(gc) = controller_subsystem.open(which) {
                        eprintln!("Controller connected: {}", gc.name());
                        controllers.push(gc);
                    }
                }

                Event::ControllerDeviceRemoved { which, .. } => {
                    controllers.retain(|c| c.instance_id() != which);
                    eprintln!("Controller disconnected");
                }

                // Mouse motion → analog axes (trackball games). When the mouse is
                // grabbed the cursor belongs to the game (it is captured and
                // warped to window center), so route motion unconditionally —
                // egui's `wants_pointer()` would otherwise report the warped
                // cursor as "over an area" and swallow every delta. Press F11 to
                // ungrab (clears `mouse_grabbed`) and interact with egui panels.
                Event::MouseMotion { xrel, yrel, .. } if mouse_grabbed => {
                    for (id, scale) in bindings.mouse_axis_targets(MouseAxis::X) {
                        let delta = xrel as f32 * scale;
                        machine.handle_input(InputEvent::Relative { id, delta });
                    }
                    for (id, scale) in bindings.mouse_axis_targets(MouseAxis::Y) {
                        let delta = yrel as f32 * scale;
                        machine.handle_input(InputEvent::Relative { id, delta });
                    }
                }

                // Mouse buttons → fire (trackball games)
                Event::MouseButtonDown { mouse_btn, .. } if mouse_grabbed => {
                    for id in bindings.digital_targets(PhysicalInput::MouseButtonInput(mouse_btn)) {
                        machine.handle_input(InputEvent::Button { id, pressed: true });
                    }
                }

                Event::MouseButtonUp { mouse_btn, .. } if mouse_grabbed => {
                    for id in bindings.digital_targets(PhysicalInput::MouseButtonInput(mouse_btn)) {
                        machine.handle_input(InputEvent::Button { id, pressed: false });
                    }
                }

                _ => {}
            }
        }

        let t1 = Instant::now();

        // Execute based on debug state
        let frame_executed = debug_ui::execute_frame(machine, &mut debug_state);
        let t2 = Instant::now();

        // Drain audio samples only when a full frame was executed
        if frame_executed && let Some((ref device, ref ring, _)) = audio_state {
            let n = machine.fill_audio(&mut audio_scratch);
            if n > 0 {
                if let Some(rec) = audio_recording.as_mut() {
                    rec.extend_from_slice(&audio_scratch[..n]);
                }
                let mut buf = ring.lock().unwrap();
                const MAX_RING_SIZE: usize = 8192;
                while buf.len() + n > MAX_RING_SIZE {
                    buf.pop_front();
                }
                buf.extend(&audio_scratch[..n]);

                // Start playback after the first batch of real samples is buffered,
                // so the callback never transitions from silence to audio (no pop).
                if !audio_started {
                    device.resume();
                    audio_started = true;
                }
            }
        }
        let t3 = Instant::now();

        // Render: always render when paused (to show debug UI), otherwise respect throttle
        let should_render = throttle
            || debug_state.run_mode == RunMode::Paused
            || last_render_time.elapsed() >= frame_duration;

        if should_render {
            // Vector machines: render GL lines directly (no CPU framebuffer).
            // Falls back to CPU rasterization in debug or profiler mode
            // (side panels need a texture for layout).
            if let Some(ref mut renderer) = vector_renderer
                && let Some(lines) = machine.vector_display_list()
                && !debug_state.active
                && !profile_state.active
                && !settings_state.active
            {
                let ds = machine.display_size();
                let rot =
                    if machine.orientation() == phosphor_core::core::machine::Orientation::ROT270 {
                        270
                    } else {
                        0
                    };
                let paused = debug_state.global_paused;
                if show_fps || paused {
                    let fps = show_fps.then(|| fps_text.clone());
                    let stats = if show_fps {
                        machine.overlay_stats()
                    } else {
                        None
                    };
                    video.present_vectors_with_overlay(
                        renderer,
                        lines,
                        ds,
                        view_aspect,
                        rot,
                        |ctx| {
                            let label = |ui: &mut egui::Ui, text: &str| {
                                ui.label(
                                    egui::RichText::new(text)
                                        .color(egui::Color32::WHITE)
                                        .background_color(egui::Color32::from_black_alpha(160))
                                        .monospace(),
                                );
                            };
                            egui::Window::new("fps_overlay")
                                .title_bar(false)
                                .resizable(false)
                                .fixed_pos(egui::pos2(4.0, 4.0))
                                .frame(egui::Frame::NONE)
                                .show(ctx, |ui| {
                                    ui.set_min_width(120.0);
                                    if let Some(ref f) = fps {
                                        label(ui, f);
                                    }
                                    if let Some(ref s) = stats {
                                        label(ui, s);
                                    }
                                    if paused {
                                        label(ui, "PAUSED");
                                    }
                                });
                        },
                    );
                } else {
                    // Still run egui pass to consume input events (prevents stale
                    // state buildup), but render no UI widgets.
                    video.present_vectors_with_overlay(
                        renderer,
                        lines,
                        ds,
                        view_aspect,
                        rot,
                        |_ctx| {},
                    );
                }
            } else {
                // Raster machine (or debug/profiler mode): CPU framebuffer path.
                machine.render_frame(&mut framebuffer);

                // FPS / PAUSED overlay onto framebuffer (only when no side panels
                // are active). PAUSED shows independent of the FPS toggle.
                if (show_fps || debug_state.global_paused)
                    && !debug_state.active
                    && !profile_state.active
                {
                    let stats = if show_fps {
                        machine.overlay_stats()
                    } else {
                        None
                    };
                    crate::overlay::draw_overlay(
                        &mut framebuffer,
                        width as usize,
                        show_fps.then_some(fps_text.as_str()),
                        stats.as_deref(),
                        debug_state.global_paused,
                    );
                }

                video.update_game_texture(&framebuffer);

                if debug_state.active
                    || profile_state.active
                    || settings_state.active
                    || settings_state.dip_active
                {
                    let bus_ref = machine.debug_bus();
                    let profiling = profile_state.active;
                    let show_settings = settings_state.active;
                    let controls = machine.input_controls();
                    let bindings_ref: &BindingSet = bindings;
                    // Snapshot DIP metadata + live bank bytes before the egui
                    // closure (which must not hold `&mut machine`).
                    let show_dip = settings_state.dip_active;
                    let dip_banks = machine.dip_banks();
                    let dip_values: Vec<u8> = (0..dip_banks.len())
                        .map(|i| machine.dip_bank_value(i))
                        .collect();
                    video.present_with_debug(|ctx, tex_id| {
                        // Profiler side panel (outermost right, drawn first)
                        if profiling {
                            crate::profile::draw_profile_panel(ctx, &profile_state, frame_duration);
                        }
                        // Input settings side panel
                        if show_settings {
                            settings_ui::draw_input_panel(
                                ctx,
                                controls,
                                bindings_ref,
                                &mut settings_state,
                            );
                        }
                        // DIP switch side panel
                        if show_dip {
                            settings_ui::draw_dip_panel(
                                ctx,
                                dip_banks,
                                &dip_values,
                                &mut settings_state,
                            );
                        }
                        if debug_state.active {
                            // Debug panels + game central panel
                            debug_ui::draw_debug_ui(
                                ctx,
                                tex_id,
                                view_aspect,
                                &mut debug_state,
                                bus_ref,
                            );
                        } else {
                            // Game central panel with aspect ratio preservation
                            draw_game_panel(ctx, tex_id, view_aspect);
                        }
                    });

                    // Apply a requested reset-to-defaults after the UI frame.
                    if settings_state.reset_requested {
                        *bindings = crate::input::build_bindings(&*machine);
                        settings_state.reset_requested = false;
                    }
                    // Apply DIP edits recorded by the panel this frame.
                    for change in settings_state.pending_dip_changes.drain(..) {
                        machine.set_dip_option(change.bank, change.option, change.value);
                    }
                } else {
                    video.present_game_only(view_aspect);
                }
            }
            last_render_time = Instant::now();
        }
        let t4 = Instant::now();

        // FPS: exponential moving average (α = 0.05) for a stable readout
        if show_fps {
            let now = Instant::now();
            let dt = now.duration_since(fps_last_instant).as_secs_f64();
            fps_last_instant = now;
            if dt > 0.0 {
                let instant_fps = 1.0 / dt;
                fps_smoothed += 0.05 * (instant_fps - fps_smoothed);
                fps_text = format!("fps: {fps_smoothed:.1}  frame: {}", debug_state.frame_count);
            }
        }

        // Frame throttling
        if debug_state.run_mode == RunMode::Paused || debug_state.global_paused {
            // When paused (debug step-mode or the global P pause), sleep to keep
            // the UI responsive without burning CPU, regardless of the throttle
            // setting.
            std::thread::sleep(Duration::from_millis(16));
        } else if throttle {
            let now = Instant::now();
            if next_frame_time > now {
                std::thread::sleep(next_frame_time - now);
            }
            next_frame_time += frame_duration;

            // If we've fallen more than one frame behind, reset the deadline
            // rather than burst-catching-up (which would cause choppy audio).
            if next_frame_time < Instant::now() {
                next_frame_time = Instant::now() + frame_duration;
            }
        }

        // Record profiling data for this frame
        if profile_state.active {
            let t5 = Instant::now();
            let sub_spans = machine.frame_profile_spans();
            profile_state.record_frame(t1 - t0, t2 - t1, t3 - t2, t4 - t3, t5 - t4, sub_spans);
        }
    }

    // Save window position for next launch (skip in fullscreen, where the
    // reported position is the desktop origin and would clobber the windowed
    // placement).
    if !fullscreen {
        let (wx, wy) = video.window_position();
        state.window_x = Some(wx);
        state.window_y = Some(wy);
    }

    // Flush profiler trace if still recording
    if profile_state.active {
        machine.set_profiling(false);
        profile_state.stop();
    }

    // Signal fade-out, wait for the ramp to complete, then stop the callback.
    if let Some((ref device, _, ref fade_out)) = audio_state {
        fade_out.store(true, std::sync::atomic::Ordering::Relaxed);
        std::thread::sleep(crate::audio::fade_out_duration());
        device.pause();
    }

    // Flush any recorded gameplay audio to the requested WAV.
    if let (Some(path), Some(rec)) = (record_wav, audio_recording) {
        let rate = machine.audio_sample_rate();
        match crate::headless::write_wav(&rec, rate, path) {
            Ok(()) => println!("recorded {} samples @ {rate} Hz to {path}", rec.len()),
            Err(e) => eprintln!("failed to write {path}: {e}"),
        }
    }
}

/// Draw the game texture in a central panel at the target display aspect,
/// letterboxed with black bars. Used when a side panel (profiler or debug) is
/// active alongside the game.
fn draw_game_panel(ctx: &egui::Context, tex_id: egui::TextureId, aspect: f32) {
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
        .show(ctx, |ui| {
            let (size, offset) = fit_aspect(ui.available_size(), aspect);
            ui.add_space(offset.y);
            ui.horizontal(|ui| {
                ui.add_space(offset.x);
                ui.image(egui::load::SizedTexture::new(tex_id, size));
            });
        });
}

/// On-screen presentation size (before the integer `--scale`) and the as-viewed
/// aspect ratio (width / height) for a machine's display.
///
/// The native texture keeps `(native_w, native_h)`; the GPU stretches it to the
/// target aspect at presentation time. `display_aspect` is the cabinet monitor
/// aspect as viewed (e.g. `Some((4, 3))` landscape, `Some((3, 4))` portrait), or
/// `None` for square pixels. `rotated` applies screen-level rotation (Tempest),
/// swapping the native axes into viewing orientation before aspect correction.
/// The deficient axis is stretched (never shrunk) so no rendered detail is lost.
pub fn presentation(
    native_w: u32,
    native_h: u32,
    display_aspect: Option<(u32, u32)>,
    rotated: bool,
) -> (u32, u32, f32) {
    // Native size as viewed, after any screen rotation.
    let (vw, vh) = if rotated {
        (native_h, native_w)
    } else {
        (native_w, native_h)
    };
    let native_a = vw as f32 / vh as f32;
    let target = match display_aspect {
        Some((w, h)) => w as f32 / h as f32,
        None => native_a,
    };
    let (pw, ph) = if native_a < target {
        // Too narrow for the tube → widen.
        ((vh as f32 * target).round() as u32, vh)
    } else {
        // Too wide → heighten.
        (vw, (vw as f32 / target).round() as u32)
    };
    (pw, ph, pw as f32 / ph as f32)
}

/// Fit a box of the given aspect ratio inside `available`, centered. Returns the
/// fitted size and the top-left offset of the letterbox/pillarbox bars.
pub fn fit_aspect(available: egui::Vec2, aspect: f32) -> (egui::Vec2, egui::Vec2) {
    let (w, h) = if available.x / available.y > aspect {
        (available.y * aspect, available.y)
    } else {
        (available.x, available.x / aspect)
    };
    (
        egui::Vec2::new(w, h),
        egui::Vec2::new((available.x - w) / 2.0, (available.y - h) / 2.0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square_pixels_when_no_hint() {
        // No aspect hint → present at the native ratio (square pixels).
        let (w, h, a) = presentation(256, 224, None, false);
        assert_eq!((w, h), (256, 224));
        assert!((a - 256.0 / 224.0).abs() < 1e-4);
    }

    #[test]
    fn landscape_hint_widens_deficient_axis() {
        // 256×224 raster on a 4:3 tube: too narrow → widen, never shrink height.
        let (w, h, a) = presentation(256, 224, Some((4, 3)), false);
        assert_eq!(h, 224);
        assert_eq!(w, (224.0_f32 * 4.0 / 3.0).round() as u32); // 299
        // Integer rounding leaves the returned aspect near, not exactly, 4:3.
        assert!((a - 4.0 / 3.0).abs() < 1e-2);
    }

    #[test]
    fn portrait_hint_heightens_deficient_axis() {
        // Burgertime 240×240 square on a rotated 4:3 tube → 3:4, height stretched.
        let (w, h, a) = presentation(240, 240, Some((3, 4)), false);
        assert_eq!(w, 240);
        assert_eq!(h, 320);
        assert!((a - 3.0 / 4.0).abs() < 1e-4);

        // A pre-rotated raster (baked portrait, screen_rotation None) too.
        let (w, h, _) = presentation(224, 256, Some((3, 4)), false);
        assert_eq!((w, h), (224, (224.0_f32 / 0.75).round() as u32)); // 224×299
    }

    #[test]
    fn rotation_swaps_axes_before_aspect() {
        // Tempest: native 580×570 landscape space, screen-rotated to portrait,
        // presented on a 3:4 tube. Aspect is applied in as-viewed orientation.
        let (w, h, a) = presentation(580, 570, Some((3, 4)), true);
        assert!(w < h, "rotated 3:4 must be portrait, got {w}x{h}");
        assert!((a - 3.0 / 4.0).abs() < 1e-2);
    }

    #[test]
    fn fit_aspect_letterboxes_and_centers() {
        // Wider-than-aspect available area → pillarbox (bars left/right).
        let (size, offset) = fit_aspect(egui::Vec2::new(400.0, 300.0), 1.0);
        assert!((size.x - 300.0).abs() < 1e-3 && (size.y - 300.0).abs() < 1e-3);
        assert!((offset.x - 50.0).abs() < 1e-3 && offset.y.abs() < 1e-3);
    }
}
