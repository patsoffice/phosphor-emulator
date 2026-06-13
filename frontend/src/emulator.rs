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
    save_path: &Path,
    screenshot_dir: &Path,
    machine_name: &str,
    start_in_debug: bool,
    start_in_profile: bool,
    no_mouse_grab: bool,
    state: &mut crate::state::State,
) {
    // Enable controller backends before SDL init — needed for Xbox on macOS
    sdl2::hint::set("SDL_JOYSTICK_HIDAPI", "1");
    sdl2::hint::set("SDL_JOYSTICK_HIDAPI_XBOX", "1");
    sdl2::hint::set("SDL_JOYSTICK_MFI", "1");

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
    // Swap window dimensions for rotated displays (e.g., Tempest portrait).
    let (win_w, win_h) =
        if machine.screen_rotation() != phosphor_core::core::machine::ScreenRotation::None {
            (height, width)
        } else {
            (width, height)
        };
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

    // Input settings panel (F12 to toggle); only meaningful for machines with
    // typed controls.
    let mut settings_state = SettingsState::default();
    let has_typed_controls = !machine.input_controls().is_empty();

    // DIP switch panel (backtick to toggle); only for machines with DIP banks.
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
            video.resize_window(width * scale + panels, height * scale);
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
                        width * scale + panels_width(&debug_state, &profile_state, &settings_state),
                        height * scale,
                    );
                }

                // F2: Step instruction (debug + paused)
                Event::KeyDown {
                    scancode: Some(Scancode::F2),
                    repeat: false,
                    ..
                } if debug_state.active && debug_state.run_mode == RunMode::Paused => {
                    debug_state.run_mode = RunMode::StepInstruction;
                }

                // F3: Step cycle (debug + paused)
                Event::KeyDown {
                    scancode: Some(Scancode::F3),
                    repeat: false,
                    ..
                } if debug_state.active && debug_state.run_mode == RunMode::Paused => {
                    debug_state.run_mode = RunMode::StepCycle;
                }

                // F4: Continue (resume running)
                Event::KeyDown {
                    scancode: Some(Scancode::F4),
                    repeat: false,
                    ..
                } if debug_state.active => {
                    debug_state.run_mode = RunMode::Running;
                }

                Event::KeyDown {
                    scancode: Some(Scancode::F5),
                    repeat: false,
                    ..
                } => {
                    machine.reset();
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
                        width * scale + panels_width(&debug_state, &profile_state, &settings_state),
                        height * scale,
                    );
                }

                // F12: Toggle input settings panel (machines with typed controls)
                Event::KeyDown {
                    scancode: Some(Scancode::F12),
                    repeat: false,
                    ..
                } if has_typed_controls => {
                    settings_state.active = !settings_state.active;
                    settings_state.capturing = None;
                    video.resize_window(
                        width * scale + panels_width(&debug_state, &profile_state, &settings_state),
                        height * scale,
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
                        width * scale + panels_width(&debug_state, &profile_state, &settings_state),
                        height * scale,
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
                } => {
                    if !video.wants_keyboard() {
                        for id in bindings.digital_targets(PhysicalInput::Key(sc)) {
                            machine.handle_input(InputEvent::Button { id, pressed: true });
                        }
                    }
                }

                Event::KeyUp {
                    scancode: Some(sc), ..
                } => {
                    if !video.wants_keyboard() {
                        for id in bindings.digital_targets(PhysicalInput::Key(sc)) {
                            machine.handle_input(InputEvent::Button { id, pressed: false });
                        }
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

                // Mouse motion → analog axes (trackball games)
                Event::MouseMotion { xrel, yrel, .. }
                    if !video.wants_pointer() && mouse_grabbed =>
                {
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
                Event::MouseButtonDown { mouse_btn, .. } => {
                    if !video.wants_pointer() && mouse_grabbed {
                        for id in
                            bindings.digital_targets(PhysicalInput::MouseButtonInput(mouse_btn))
                        {
                            machine.handle_input(InputEvent::Button { id, pressed: true });
                        }
                    }
                }

                Event::MouseButtonUp { mouse_btn, .. } => {
                    if !video.wants_pointer() && mouse_grabbed {
                        for id in
                            bindings.digital_targets(PhysicalInput::MouseButtonInput(mouse_btn))
                        {
                            machine.handle_input(InputEvent::Button { id, pressed: false });
                        }
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
                let rot = match machine.screen_rotation() {
                    phosphor_core::core::machine::ScreenRotation::Rot270 => 270,
                    _ => 0,
                };
                if show_fps {
                    let fps = fps_text.clone();
                    let stats = machine.overlay_stats();
                    video.present_vectors_with_overlay(renderer, lines, ds, rot, |ctx| {
                        egui::Window::new("fps_overlay")
                            .title_bar(false)
                            .resizable(false)
                            .fixed_pos(egui::pos2(4.0, 4.0))
                            .frame(egui::Frame::NONE)
                            .show(ctx, |ui| {
                                ui.set_min_width(120.0);
                                ui.label(
                                    egui::RichText::new(&fps)
                                        .color(egui::Color32::WHITE)
                                        .background_color(egui::Color32::from_black_alpha(160))
                                        .monospace(),
                                );
                                if let Some(ref s) = stats {
                                    ui.label(
                                        egui::RichText::new(s)
                                            .color(egui::Color32::WHITE)
                                            .background_color(egui::Color32::from_black_alpha(160))
                                            .monospace(),
                                    );
                                }
                            });
                    });
                } else {
                    // Still run egui pass to consume input events (prevents stale
                    // state buildup), but render no UI widgets.
                    video.present_vectors_with_overlay(renderer, lines, ds, rot, |_ctx| {});
                }
            } else {
                // Raster machine (or debug/profiler mode): CPU framebuffer path.
                machine.render_frame(&mut framebuffer);

                // FPS overlay onto framebuffer (only when no side panels are active)
                if show_fps && !debug_state.active && !profile_state.active {
                    let stats = machine.overlay_stats();
                    crate::overlay::draw_overlay(
                        &mut framebuffer,
                        width as usize,
                        &fps_text,
                        stats.as_deref(),
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
                    video.present_with_debug(|ctx, tex_id, native_size| {
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
                                native_size,
                                &mut debug_state,
                                bus_ref,
                            );
                        } else {
                            // Game central panel with aspect ratio preservation
                            draw_game_panel(ctx, tex_id, native_size);
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
                    video.present_game_only();
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
                fps_text = format!("fps: {fps_smoothed:.1}");
            }
        }

        // Frame throttling
        if debug_state.run_mode == RunMode::Paused {
            // When paused, sleep to keep UI responsive without burning CPU
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

    // Save window position for next launch
    let (wx, wy) = video.window_position();
    state.window_x = Some(wx);
    state.window_y = Some(wy);

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
}

/// Draw the game texture in a central panel with aspect ratio preservation.
/// Used when a side panel (profiler or debug) is active alongside the game.
fn draw_game_panel(ctx: &egui::Context, tex_id: egui::TextureId, native_size: (u32, u32)) {
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
        .show(ctx, |ui| {
            let available = ui.available_size();
            let (nw, nh) = native_size;
            let aspect = nw as f32 / nh as f32;
            let (display_w, display_h) = if available.x / available.y > aspect {
                (available.y * aspect, available.y)
            } else {
                (available.x, available.x / aspect)
            };
            let offset_x = (available.x - display_w) / 2.0;
            let offset_y = (available.y - display_h) / 2.0;
            ui.add_space(offset_y);
            ui.horizontal(|ui| {
                ui.add_space(offset_x);
                ui.image(egui::load::SizedTexture::new(
                    tex_id,
                    egui::Vec2::new(display_w, display_h),
                ));
            });
        });
}
