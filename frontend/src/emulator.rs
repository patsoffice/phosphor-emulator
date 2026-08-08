use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use phosphor_core::core::machine::{FrontendMachine, InputKind, Orientation};
use phosphor_script::{DebugSession, Machine};
use sdl2::event::{Event, WindowEvent};
use sdl2::keyboard::{Mod, Scancode};

use crate::console_ui::{self, ConsoleState};
use crate::debug_ui::{self, DebugState, RunMode};
use crate::host_keys::{HostAction, HostBindings};
use crate::input::{self, AxisDir, BindingSet, MouseAxis, PhysicalInput};
use crate::profile::ProfileState;
use crate::settings_ui::{self, SettingsState};
use crate::video::Video;

/// Slack left between a panel-driven window and the edge of the display, so a
/// grown window still shows its own borders rather than running off screen.
const WINDOW_MARGIN: u32 = 64;

/// Combined width of all active right-side panels, used when resizing the window.
fn panels_width(
    debug: &DebugState,
    profile: &ProfileState,
    settings: &SettingsState,
    console: &ConsoleState,
) -> u32 {
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
    dw + pw + sw + console.visible as u32 * settings_ui::PANEL_WIDTH
}

/// Key captions for the debugger's run/step buttons, read from the live host
/// bindings so a rebound key relabels its button.
fn step_key_hints(host: &HostBindings) -> debug_ui::StepKeyHints {
    let key = |action| match host.key_for(action) {
        Some(sc) => crate::host_keys::key_label(sc),
        None => "\u{2014}".to_string(),
    };
    debug_ui::StepKeyHints {
        pause: key(HostAction::ToggleDebugPause),
        step_cycle: key(HostAction::StepCycle),
        step_instruction: key(HostAction::StepInstruction),
        step_frame: key(HostAction::StepFrame),
    }
}

/// Translate an SDL event into a physical input for rebind capture, if it is a
/// bindable press (key, gamepad button, or mouse button).
/// `analog_target` selects what an axis deflection means: a whole axis for an
/// analog control, or one signed direction standing in for a button.
/// `mouse_grabbed` gates mouse-motion capture — ungrabbed, the cursor is
/// travelling toward the Rebind button and would capture itself.
fn capture_physical(
    event: &Event,
    analog_target: bool,
    mouse_grabbed: bool,
) -> Option<PhysicalInput> {
    match event {
        Event::KeyDown {
            scancode: Some(sc), ..
        } => Some(PhysicalInput::Key(*sc)),
        Event::ControllerButtonDown { button, .. } => Some(PhysicalInput::PadButton(*button)),
        Event::MouseButtonDown { mouse_btn, .. } => {
            Some(PhysicalInput::MouseButtonInput(*mouse_btn))
        }
        // A decisive deflection binds the axis. The threshold sits far above
        // the digital deadzone so a resting — or drifting — stick can never
        // capture itself.
        Event::ControllerAxisMotion { axis, value, .. } if value.unsigned_abs() > 24_000 => {
            Some(if analog_target {
                PhysicalInput::PadFullAxis(*axis)
            } else {
                PhysicalInput::PadAxis(
                    *axis,
                    if *value > 0 {
                        AxisDir::Positive
                    } else {
                        AxisDir::Negative
                    },
                )
            })
        }
        Event::MouseMotion { xrel, yrel, .. }
            if analog_target && mouse_grabbed && (xrel.abs() > 8 || yrel.abs() > 8) =>
        {
            Some(PhysicalInput::MouseAxis(if xrel.abs() > yrel.abs() {
                MouseAxis::X
            } else {
                MouseAxis::Y
            }))
        }
        _ => None,
    }
}

/// Live SDL device state, for [`input::resync`].
///
/// `mouse` is `None` while the mouse is ungrabbed, which reports every mouse
/// button as released — ungrabbed means the cursor belongs to the UI.
struct SdlDevices<'a> {
    keyboard: sdl2::keyboard::KeyboardState<'a>,
    controllers: &'a [sdl2::controller::GameController],
    mouse: Option<sdl2::mouse::MouseState>,
}

impl SdlDevices<'_> {
    /// Pads a binding restricted to `slot` may read; every pad when it is not
    /// restricted.
    fn pads_for(
        &self,
        slot: Option<u8>,
    ) -> impl Iterator<Item = &sdl2::controller::GameController> {
        self.controllers
            .iter()
            .enumerate()
            .filter(move |(i, _)| slot.is_none_or(|want| pad_slot_of(*i) == want))
            .map(|(_, c)| c)
    }
}

/// Slot number for the pad at `index` in connection order. 1-based, matching
/// `InputControl::player`.
fn pad_slot_of(index: usize) -> u8 {
    index as u8 + 1
}

impl input::DeviceState for SdlDevices<'_> {
    fn key_pressed(&self, scancode: Scancode) -> bool {
        self.keyboard.is_scancode_pressed(scancode)
    }

    fn pad_button_pressed(&self, button: sdl2::controller::Button, slot: Option<u8>) -> bool {
        self.pads_for(slot).any(|c| c.button(button))
    }

    fn pad_axis(&self, axis: sdl2::controller::Axis, slot: Option<u8>) -> f32 {
        // Among the pads allowed to drive this binding, whichever is pushing
        // hardest wins.
        self.pads_for(slot)
            .map(|c| f32::from(c.axis(axis)) / 32_768.0)
            .max_by(|a, b| a.abs().total_cmp(&b.abs()))
            .unwrap_or(0.0)
    }

    fn mouse_button_pressed(&self, button: sdl2::mouse::MouseButton) -> bool {
        self.mouse
            .as_ref()
            .is_some_and(|m| m.is_mouse_button_pressed(button))
    }
}

/// Slot of the pad that produced `event`, if it came from one.
///
/// Slots follow connection order and are stable while a pad stays plugged in:
/// unplugging one shifts the pads after it down, which is the same renumbering
/// a player would expect from the cabinet's point of view.
fn event_pad_slot(event: &Event, controllers: &[sdl2::controller::GameController]) -> Option<u8> {
    let which = match event {
        Event::ControllerButtonDown { which, .. }
        | Event::ControllerButtonUp { which, .. }
        | Event::ControllerAxisMotion { which, .. } => *which,
        _ => return None,
    };
    controllers
        .iter()
        .position(|c| c.instance_id() == which)
        .map(pad_slot_of)
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    mut machine: Box<dyn FrontendMachine>,
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
) -> Box<dyn FrontendMachine> {
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

    // `render_frame` fills a native (pre-orientation) framebuffer; the frontend
    // applies the machine's declared orientation centrally (see the raster path
    // below). For a machine declaring a 90°/270° rotation the *displayed* texture
    // swaps axes; the window is then sized to the machine's target display aspect
    // (4:3 tube, rotated to 3:4 for portrait cabinets) so the GPU corrects pixel
    // aspect at presentation time. `view_aspect` (as-viewed w/h) drives every
    // letterbox. Machines that still bake rotation report already-rotated dims
    // and return NORMAL, so the displayed size equals the native size for them.
    let (width, height) = machine.display_size();
    let rotated = machine.orientation().swaps_axes();
    let (disp_w, disp_h) = if rotated {
        (height, width)
    } else {
        (width, height)
    };
    let (win_w, win_h, view_aspect) =
        presentation(width, height, machine.display_aspect(), rotated);
    let window_pos = state.window_x.zip(state.window_y);
    let mut video = Video::new(
        &sdl_video,
        "Phosphor Emulator",
        disp_w,
        disp_h,
        win_w,
        win_h,
        scale,
        window_pos,
        fullscreen,
    );
    let mut event_pump = sdl_context.event_pump().expect("Failed to get event pump");
    // Latched pad-axis state, so a stick resting inside its deadzone stops
    // re-asserting "released" over whatever the player is holding.
    let mut dispatch_state = input::DispatchState::default();
    let mut host_bindings: HostBindings = state.host_bindings.clone();

    // Warn about machine controls a hotkey shadows. The frontend arms match
    // first, so without this the control is simply dead with nothing logged.
    {
        let machine_keys: Vec<Scancode> = bindings
            .all_physical()
            .filter_map(|p| match p {
                PhysicalInput::Key(sc) => Some(sc),
                _ => None,
            })
            .collect();
        for (action, key) in crate::host_keys::conflicts(&host_bindings, &machine_keys) {
            eprintln!(
                "Note: {key:?} is the '{}' hotkey, so {machine_name} cannot see it. \
                 Rebind either side in the settings panel (F12).",
                action.label()
            );
        }
    }

    // Detect vector display machines and create GL renderer.
    let mut vector_renderer = machine
        .vector_display_list()
        .map(|_| crate::vector_gl::VectorRenderer::new());

    let audio_state = crate::audio::init(&sdl_audio, machine.audio_sample_rate());
    let mut audio_started = false;

    let buffer_size = (width * height * 3) as usize;
    // Native buffer that `render_frame` fills, plus a second buffer for the
    // post-orientation image (same pixel count, axes possibly swapped). The
    // second buffer is only touched when the machine declares a non-NORMAL
    // orientation, keeping the common path zero-copy.
    let mut framebuffer = vec![0u8; buffer_size];
    let mut oriented = vec![0u8; buffer_size];
    let mut audio_scratch = vec![0i16; 2048];
    // Optional live-gameplay audio recording: tee every produced sample here and
    // write the WAV on exit. `Some` only when `--record-wav` was passed.
    let mut audio_recording: Option<Vec<i16>> = record_wav.map(|_| Vec::new());

    let frame_duration = Duration::from_secs_f64(1.0 / machine.frame_rate_hz());
    let mut next_frame_time = Instant::now() + frame_duration;
    let mut throttle = true;
    let mut last_render_time = Instant::now();
    // Combined panel width the window was last sized for. The debug panel's
    // columns size themselves to their content, so this changes when a tab
    // does — not only when a panel opens or closes.
    let mut last_panels_width: u32 = 0;

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

    // Interactive Rhai console panel (Ctrl+` to toggle).
    let mut console_state = ConsoleState::default();

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
        let panels = panels_width(
            &debug_state,
            &profile_state,
            &settings_state,
            &console_state,
        );
        if panels > 0 {
            video.resize_window(win_w * scale + panels, win_h * scale);
        }
    }

    // Move the machine into a session so the console can drive the *live*
    // machine through the same bindings the headless runner uses. The emulator
    // reaches the machine via `sess.machine_mut()` each frame; the console holds
    // a clone of the same `Rc` and evaluates scripts after the frame.
    let session: Machine = Rc::new(RefCell::new(DebugSession::from_machine(machine)));

    // Console engine: reuse the script crate's engine builder, but route its
    // print/debug output into the console scrollback instead of stdout.
    let console_output: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let mut console_engine = phosphor_script::build_engine();
    {
        let out = console_output.clone();
        console_engine.on_print(move |text| out.borrow_mut().push(text.to_string()));
        let out = console_output.clone();
        console_engine.on_debug(move |text, _src, _pos| out.borrow_mut().push(text.to_string()));
    }
    let mut console_scope = rhai::Scope::new();
    console_scope.push("m", Rc::clone(&session));

    'main: loop {
        let t0 = Instant::now();
        let mut sess = session.borrow_mut();
        let machine: &mut dyn FrontendMachine = sess.machine_mut();

        // Set when something invalidates the machine's idea of held input
        // (reset, state load, focus regain, controller unplug). Reconciled once
        // after the event batch, because reading live device state needs
        // `event_pump` back from `poll_iter`'s borrow.
        let mut needs_resync = false;

        // Poll all pending SDL events, translate to machine input
        for event in event_pump.poll_iter() {
            // Forward every event to egui first
            video.process_event(event.clone());

            // Hotkey capture: claim the next key press for the host action the
            // settings panel is waiting on. Must precede the hotkey arms, or
            // pressing e.g. F5 would reset instead of binding.
            if let Some(action) = settings_state.capturing_host {
                match &event {
                    Event::KeyDown {
                        scancode: Some(Scancode::Escape),
                        ..
                    } => {
                        settings_state.capturing_host = None;
                        continue;
                    }
                    Event::KeyDown {
                        scancode: Some(sc),
                        repeat: false,
                        ..
                    } => {
                        settings_state.pending_host_rebind.push((action, *sc));
                        settings_state.capturing_host = None;
                        continue;
                    }
                    _ => {}
                }
            }

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
                        let analog_target = machine.input_controls().iter().any(|c| {
                            c.id == target && matches!(c.kind, InputKind::AnalogAxis { .. })
                        });
                        if let Some(physical) =
                            capture_physical(&event, analog_target, mouse_grabbed)
                        {
                            bindings.rebind(machine.input_controls(), target, physical);
                            settings_state.capturing = None;
                            continue;
                        }
                    }
                }
            }

            // While the console is open, keyboard goes to it (egui already
            // received the event above). Suppress game input and other hotkeys,
            // but still allow closing the console and quitting.
            if console_state.visible {
                match &event {
                    Event::Quit { .. } => break 'main,
                    Event::KeyDown {
                        scancode: Some(Scancode::Grave),
                        keymod,
                        repeat: false,
                        ..
                    } if keymod.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD) => {
                        console_state.toggle();
                        video.resize_window(
                            win_w * scale
                                + panels_width(
                                    &debug_state,
                                    &profile_state,
                                    &settings_state,
                                    &console_state,
                                ),
                            win_h * scale,
                        );
                    }
                    Event::KeyDown {
                        scancode: Some(Scancode::Escape),
                        repeat: false,
                        ..
                    } => {
                        console_state.toggle();
                        video.resize_window(
                            win_w * scale
                                + panels_width(
                                    &debug_state,
                                    &profile_state,
                                    &settings_state,
                                    &console_state,
                                ),
                            win_h * scale,
                        );
                    }
                    _ => {}
                }
                continue;
            }

            match event {
                Event::Quit { .. } => break 'main,

                Event::KeyDown {
                    scancode: Some(sc), ..
                } if host_bindings.action_for(sc) == Some(HostAction::Quit) => break 'main,

                // F1: Toggle debug mode
                Event::KeyDown {
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } if has_debug
                    && host_bindings.action_for(sc) == Some(HostAction::ToggleDebugPanel) =>
                {
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
                        win_w * scale
                            + panels_width(
                                &debug_state,
                                &profile_state,
                                &settings_state,
                                &console_state,
                            ),
                        win_h * scale,
                    );
                }

                // 7: Step instruction (debug + paused)
                Event::KeyDown {
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } if debug_state.active
                    && debug_state.run_mode == RunMode::Paused
                    && host_bindings.action_for(sc) == Some(HostAction::StepInstruction) =>
                {
                    debug_state.run_mode = RunMode::StepInstruction;
                }

                // 8: Step cycle (debug + paused)
                Event::KeyDown {
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } if debug_state.active
                    && debug_state.run_mode == RunMode::Paused
                    && host_bindings.action_for(sc) == Some(HostAction::StepCycle) =>
                {
                    debug_state.run_mode = RunMode::StepCycle;
                }

                // 9: Step frame — run one frame, pause at the next frame start
                // (debug + paused)
                Event::KeyDown {
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } if debug_state.active
                    && debug_state.run_mode == RunMode::Paused
                    && host_bindings.action_for(sc) == Some(HostAction::StepFrame) =>
                {
                    debug_state.run_mode = RunMode::StepFrame;
                }

                // 0: Toggle run <-> pause (running -> paused, otherwise continue)
                Event::KeyDown {
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } if debug_state.active
                    && host_bindings.action_for(sc) == Some(HostAction::ToggleDebugPause) =>
                {
                    if debug_state.run_mode == RunMode::Running {
                        debug_state.run_mode = RunMode::Paused;
                    } else {
                        // The watchpoint hit history survives a resume so a
                        // sequence of hits accumulates across breaks; the
                        // panel's Clear button empties it.
                        debug_state.run_mode = RunMode::Running;
                    }
                }

                // ?: Toggle the key legend (works with no other panel open)
                Event::KeyDown {
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } if host_bindings.action_for(sc) == Some(HostAction::ToggleKeyLegend) => {
                    settings_state.legend_visible = !settings_state.legend_visible;
                }

                Event::KeyDown {
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } if host_bindings.action_for(sc) == Some(HostAction::Reset) => {
                    machine.reset();
                    debug_state.frame_count = 0;
                    // reset() clears the machine's port bits, but a key held
                    // across it produces no new KeyDown — without this the
                    // input is dead until the user releases and re-presses.
                    needs_resync = true;
                }

                // Quick Save (F6)
                Event::KeyDown {
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } if host_bindings.action_for(sc) == Some(HostAction::QuickSave) => {
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
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } if host_bindings.action_for(sc) == Some(HostAction::QuickLoad) => {
                    match std::fs::read(save_path) {
                        Ok(data) => match machine.load_state(&data) {
                            Ok(()) => {
                                eprintln!("Save state loaded");
                                // Port bits live inside the snapshot, so the
                                // restored state can contradict what is physically
                                // held right now.
                                needs_resync = true;
                            }
                            Err(e) => eprintln!("Load state failed: {e}"),
                        },
                        Err(e) => eprintln!("No save file found: {e}"),
                    }
                }

                // F8: Toggle profiler
                Event::KeyDown {
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } if host_bindings.action_for(sc) == Some(HostAction::ToggleProfiler) => {
                    if profile_state.active {
                        machine.set_profiling(false);
                        profile_state.stop();
                    } else {
                        machine.set_profiling(true);
                        profile_state.start();
                    }
                    video.resize_window(
                        win_w * scale
                            + panels_width(
                                &debug_state,
                                &profile_state,
                                &settings_state,
                                &console_state,
                            ),
                        win_h * scale,
                    );
                }

                // Tab: Toggle input settings panel (machines with typed controls)
                Event::KeyDown {
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } if has_typed_controls
                    && host_bindings.action_for(sc) == Some(HostAction::ToggleSettingsPanel) =>
                {
                    settings_state.active = !settings_state.active;
                    settings_state.capturing = None;
                    video.resize_window(
                        win_w * scale
                            + panels_width(
                                &debug_state,
                                &profile_state,
                                &settings_state,
                                &console_state,
                            ),
                        win_h * scale,
                    );
                }

                // Ctrl+` : Toggle the interactive Rhai console.
                Event::KeyDown {
                    scancode: Some(Scancode::Grave),
                    keymod,
                    repeat: false,
                    ..
                } if keymod.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD) => {
                    console_state.toggle();
                    video.resize_window(
                        win_w * scale
                            + panels_width(
                                &debug_state,
                                &profile_state,
                                &settings_state,
                                &console_state,
                            ),
                        win_h * scale,
                    );
                }

                // Backtick (`): Toggle DIP switch panel (machines with DIP banks)
                Event::KeyDown {
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } if has_dip
                    && host_bindings.action_for(sc) == Some(HostAction::ToggleDipPanel) =>
                {
                    settings_state.dip_active = !settings_state.dip_active;
                    video.resize_window(
                        win_w * scale
                            + panels_width(
                                &debug_state,
                                &profile_state,
                                &settings_state,
                                &console_state,
                            ),
                        win_h * scale,
                    );
                }

                Event::KeyDown {
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } if host_bindings.action_for(sc) == Some(HostAction::ToggleThrottle) => {
                    throttle = !throttle;
                    if throttle {
                        next_frame_time = Instant::now() + frame_duration;
                    }
                }

                Event::KeyDown {
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } if host_bindings.action_for(sc) == Some(HostAction::ToggleFps) => {
                    show_fps = !show_fps;
                    fps_smoothed = machine.frame_rate_hz();
                    fps_last_instant = Instant::now();
                }

                // Mouse grab toggle (F11)
                Event::KeyDown {
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } if host_bindings.action_for(sc) == Some(HostAction::ToggleMouseGrab) => {
                    mouse_grabbed = !mouse_grabbed;
                    sdl_context.mouse().set_relative_mouse_mode(mouse_grabbed);
                    // Ungrabbing stops mouse events reaching the game, so a
                    // button held at that moment would never see its release.
                    if !mouse_grabbed {
                        machine.release_all_inputs();
                    }
                }

                // P: Toggle global pause (frontend-level control, not a game input)
                Event::KeyDown {
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } if host_bindings.action_for(sc) == Some(HostAction::TogglePause) => {
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
                    scancode: Some(sc),
                    repeat: false,
                    ..
                } if host_bindings.action_for(sc) == Some(HostAction::Screenshot) => {
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
                    // An unplugged pad sends no button-up for whatever it was
                    // holding. Clear everything, then re-assert from the pads
                    // that are still connected.
                    machine.release_all_inputs();
                    needs_resync = true;
                }

                // Focus loss strands every held input — the window stops
                // receiving key/button releases entirely.
                Event::Window {
                    win_event: WindowEvent::FocusLost,
                    ..
                } => machine.release_all_inputs(),

                Event::Window {
                    win_event: WindowEvent::FocusGained,
                    ..
                } => needs_resync = true,

                // Game input — last, so every hotkey above keeps precedence.
                other => {
                    input::dispatch(
                        &other,
                        bindings,
                        machine,
                        input::DispatchCtx {
                            egui_wants_keyboard: video.wants_keyboard(),
                            mouse_grabbed,
                            pad_slot: event_pad_slot(&other, &controllers),
                        },
                        &mut dispatch_state,
                    );
                }
            }
        }

        // Reconcile the machine with the physical devices, now that
        // `event_pump` is free to be queried for their live state.
        if needs_resync {
            let devices = SdlDevices {
                mouse: mouse_grabbed.then(|| event_pump.mouse_state()),
                keyboard: event_pump.keyboard_state(),
                controllers: &controllers,
            };
            input::resync(bindings, machine, &devices);
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
                // Drive the GL shader's rotation uniform from the declared
                // orientation flags. Only ROT270 (portrait vector monitors, e.g.
                // Tempest) is exercised today; the shader special-cases it.
                let rot = orientation_degrees(machine.orientation());
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

                // Apply the machine's declared orientation centrally. NORMAL is
                // the zero-copy common path: unmigrated machines bake rotation
                // into render_frame and return NORMAL, so `framebuffer` is already
                // the displayed image. Migrated machines render native and let
                // `apply_orientation` produce the displayed (post-rotation) buffer.
                let orient = machine.orientation();
                let display_fb: &mut [u8] = if orient == Orientation::NORMAL {
                    &mut framebuffer
                } else {
                    phosphor_core::gfx::apply_orientation(
                        &framebuffer,
                        &mut oriented,
                        width as usize,
                        height as usize,
                        orient,
                    );
                    &mut oriented
                };

                // FPS / PAUSED overlay onto the displayed buffer (only when no
                // side panels are active). PAUSED shows independent of FPS.
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
                        display_fb,
                        disp_w as usize,
                        show_fps.then_some(fps_text.as_str()),
                        stats.as_deref(),
                        debug_state.global_paused,
                    );
                }

                video.update_game_texture(display_fb);

                if debug_state.active
                    || profile_state.active
                    || settings_state.active
                    || settings_state.dip_active
                    || settings_state.legend_visible
                    || console_state.visible
                {
                    let bus_ref = machine.debug_bus();
                    let profiling = profile_state.active;
                    let show_settings = settings_state.active;
                    let show_legend = settings_state.legend_visible;
                    let controls = machine.input_controls();
                    let bindings_ref: &BindingSet = bindings;
                    let host_bindings_ref = &host_bindings;
                    // Snapshot DIP metadata + live bank bytes before the egui
                    // closure (which must not hold `&mut machine`).
                    let show_dip = settings_state.dip_active;
                    let show_console = console_state.visible;
                    let dip_banks = machine.dip_banks();
                    let dip_values: Vec<u8> = (0..dip_banks.len())
                        .map(|i| machine.dip_bank_value(i))
                        .collect();
                    // Relabel the run/step buttons from the live bindings, so a
                    // rebound step key is what the button advertises.
                    debug_state.key_hints = step_key_hints(&host_bindings);
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
                                host_bindings_ref,
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
                        if show_console {
                            console_ui::draw_console_panel(ctx, &mut console_state);
                        }
                        // Floating legend, drawn after the side panels so it
                        // lays over them rather than being clipped by one.
                        if show_legend {
                            settings_ui::draw_key_legend(
                                ctx,
                                controls,
                                bindings_ref,
                                host_bindings_ref,
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

                    // The debug panel sizes its columns to what they drew, so
                    // switching a column to the Memory tab (a ~74-column hex
                    // row) changes how much room the panel needs. Grow the
                    // window to match instead of making the user scroll — but
                    // never past the display, where the extra width would be
                    // unreachable and the listings' own scrollbars take over.
                    if debug_state.active {
                        let wanted = panels_width(
                            &debug_state,
                            &profile_state,
                            &settings_state,
                            &console_state,
                        );
                        if wanted != last_panels_width {
                            last_panels_width = wanted;
                            let max_w = video
                                .display_width()
                                .unwrap_or(u32::MAX)
                                .saturating_sub(WINDOW_MARGIN);
                            video.resize_window((win_w * scale + wanted).min(max_w), win_h * scale);
                        }
                    }

                    // Apply a requested reset-to-defaults after the UI frame.
                    if settings_state.reset_requested {
                        *bindings = crate::input::build_bindings(&*machine);
                        settings_state.reset_requested = false;
                    }
                    // Apply DIP edits recorded by the panel this frame.
                    for change in settings_state.pending_dip_changes.drain(..) {
                        machine.set_dip_option(change.bank, change.option, change.value);
                    }
                    // Hotkey rebinds and resets recorded by the panel.
                    if settings_state.host_reset_requested {
                        host_bindings.reset();
                        settings_state.host_reset_requested = false;
                    }
                    for (action, sc) in settings_state.pending_host_rebind.drain(..) {
                        host_bindings.rebind(action, sc);
                    }
                    // Same for analog sensitivity / deadzone edits.
                    for change in settings_state.pending_tuning.drain(..) {
                        if let Some(scale) = change.scale {
                            bindings.set_scale(change.target, scale);
                        }
                        if let Some(deadzone) = change.deadzone {
                            bindings.set_deadzone(change.target, deadzone);
                        }
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

        // Release the machine borrow, then evaluate any console command against
        // the now-free session. Its output shows on the next frame's panel draw.
        drop(sess);
        if let Some(cmd) = console_state.take_pending() {
            let result = console_engine.eval_with_scope::<rhai::Dynamic>(&mut console_scope, &cmd);
            for line in console_output.borrow_mut().drain(..) {
                console_state.push_output(&line);
            }
            match result {
                Ok(value) if !value.is_unit() => console_state.push_output(&value.to_string()),
                Ok(_) => {}
                Err(err) => console_state.push_output(&format!("error: {err}")),
            }
        }
    }

    // Reclaim the machine from the session (drop the console handle so the Rc is
    // unique) for shutdown bookkeeping and to hand back to the caller.
    drop(console_scope);
    let mut machine = Rc::try_unwrap(session)
        .ok()
        .expect("session still referenced at shutdown")
        .into_inner()
        .into_machine();

    // Save window position for next launch (skip in fullscreen, where the
    // reported position is the desktop origin and would clobber the windowed
    // placement).
    if !fullscreen {
        let (wx, wy) = video.window_position();
        state.window_x = Some(wx);
        state.window_y = Some(wy);
    }
    // Hotkey overrides are global, so they persist here rather than in the
    // per-machine section main.rs writes.
    {
        state.host_bindings = host_bindings.clone();
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

    machine
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

/// Clockwise screen rotation in degrees for the pure `ROT*` orientations, used
/// to drive the vector shader's rotation uniform. Non-rotation flag combinations
/// (bare mirrors) fall back to `0` — the vector path only rotates.
fn orientation_degrees(o: Orientation) -> i32 {
    match (o.swap_xy(), o.flip_x(), o.flip_y()) {
        (true, true, false) => 90,
        (false, true, true) => 180,
        (true, false, true) => 270,
        _ => 0,
    }
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
