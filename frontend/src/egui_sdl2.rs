//! SDL2 event translation for egui, vendored for the `egui_glow` migration.
//!
//! Lifted near-verbatim from `egui_sdl2_gl` 0.33.2 (`src/lib.rs`), which is
//! MIT licensed; the painter half of that crate is gone, this event half
//! stays. Two bugs are fixed while lifting:
//!
//! - Upstream DPI scaling is a no-op: it computes
//!   `dpi_scale * (1.0 / dpi_scale)`, which is always 1.0, so
//!   [`DpiScaling::Default`] never scaled anything (upstream issue 51).
//!   `Default` now means the display DPI over the 96 points-per-inch
//!   baseline, and [`DpiScaling::Custom`] multiplies on top of that.
//! - Upstream never handles focus: SDL `FocusGained`/`FocusLost` now set
//!   `RawInput.focused` and push `Event::WindowFocused`, so egui knows when
//!   the window loses the keyboard.
//!
//! `process_output` carries over unchanged: at egui 0.33
//! `PlatformOutput.commands` with `OutputCommand::CopyText` plus
//! `cursor_icon` is identical to what 0.36 expects.

use egui::{Modifiers, MouseWheelUnit, Pos2, RawInput, ViewportId, pos2, vec2};
use sdl2::{
    event::WindowEvent,
    keyboard::{Keycode, Mod},
    mouse::{Cursor, MouseButton, SystemCursor},
};

pub struct FusedCursor {
    pub cursor: Option<Cursor>,
    pub icon: SystemCursor,
}

impl FusedCursor {
    pub fn new() -> Self {
        Self {
            cursor: Cursor::from_system(SystemCursor::Arrow).ok(),
            icon: SystemCursor::Arrow,
        }
    }
}

impl Default for FusedCursor {
    fn default() -> Self {
        Self::new()
    }
}

/// Retained for API parity with the upstream this was lifted from; only
/// `Default` is wired up.
#[allow(dead_code)]
pub enum DpiScaling {
    /// Scale by the display DPI over the 96 points-per-inch baseline.
    Default,
    /// Multiply the default scale, e.g. 0.8, 1.5, 2.0 and so forth.
    Custom(f32),
}

pub struct EguiStateHandler {
    pub fused_cursor: FusedCursor,
    pub pointer_pos: Pos2,
    pub input: RawInput,
    pub modifiers: Modifiers,
    pub pixels_per_point: f32,
}

impl EguiStateHandler {
    pub fn new(window: &sdl2::video::Window, scale: DpiScaling) -> Self {
        let standard_dpi = 96.0;
        let display_dpi = window
            .subsystem()
            .display_dpi(0)
            .map(|(ddpi, _, _)| ddpi)
            .unwrap_or(standard_dpi);
        let default_scale = display_dpi / standard_dpi;
        let pixels_per_point = match scale {
            DpiScaling::Default => default_scale,
            DpiScaling::Custom(custom) => default_scale * custom,
        };
        let mut state = Self {
            fused_cursor: FusedCursor::default(),
            pointer_pos: Pos2::new(0f32, 0f32),
            input: RawInput {
                focused: true,
                ..Default::default()
            },
            modifiers: Modifiers::default(),
            pixels_per_point,
        };
        state.update_screen_rect(window);
        state
            .input
            .viewports
            .entry(ViewportId::ROOT)
            .or_default()
            .native_pixels_per_point = Some(pixels_per_point);
        state
    }

    fn update_screen_rect(&mut self, window: &sdl2::video::Window) {
        let (width, height) = window.drawable_size();
        let rect = vec2(width as f32, height as f32) / self.pixels_per_point;
        self.input.screen_rect = Some(egui::Rect::from_min_size(Pos2::new(0f32, 0f32), rect));
    }

    pub fn process_input(&mut self, window: &sdl2::video::Window, event: sdl2::event::Event) {
        input_to_egui(window, event, self);
    }

    pub fn process_output(
        &mut self,
        window: &sdl2::video::Window,
        egui_output: &egui::PlatformOutput,
    ) {
        for command in &egui_output.commands {
            if let egui::OutputCommand::CopyText(copied_text) = command
                && !copied_text.is_empty()
            {
                let text = copied_text.clone();
                {
                    let result = window.subsystem().clipboard().set_clipboard_text(&text);
                    if result.is_err() {
                        dbg!("Unable to set clipboard content to SDL clipboard.");
                    }
                }
            }
        }
        translate_cursor(&mut self.fused_cursor, egui_output.cursor_icon);
    }
}

pub fn input_to_egui(
    window: &sdl2::video::Window,
    event: sdl2::event::Event,
    state: &mut EguiStateHandler,
) {
    use sdl2::event::Event::*;

    let pixels_per_point = state.pixels_per_point;
    if event.get_window_id() != Some(window.id()) {
        return;
    }
    match event {
        // Handle when window Resized and SizeChanged.
        Window {
            win_event: WindowEvent::Resized(_, _) | WindowEvent::SizeChanged(_, _),
            ..
        } => {
            state.update_screen_rect(window);
        }

        Window {
            win_event: WindowEvent::FocusGained,
            ..
        } => {
            state.input.focused = true;
            state.input.events.push(egui::Event::WindowFocused(true));
        }

        Window {
            win_event: WindowEvent::FocusLost,
            ..
        } => {
            state.input.focused = false;
            state.input.events.push(egui::Event::WindowFocused(false));
        }

        //MouseButonLeft pressed is the only one needed by egui
        MouseButtonDown { mouse_btn, .. } => {
            let mouse_btn = match mouse_btn {
                MouseButton::Left => Some(egui::PointerButton::Primary),
                MouseButton::Middle => Some(egui::PointerButton::Middle),
                MouseButton::Right => Some(egui::PointerButton::Secondary),
                _ => None,
            };
            if let Some(pressed) = mouse_btn {
                state.input.events.push(egui::Event::PointerButton {
                    pos: state.pointer_pos,
                    button: pressed,
                    pressed: true,
                    modifiers: state.modifiers,
                });
            }
        }

        //MouseButonLeft pressed is the only one needed by egui
        MouseButtonUp { mouse_btn, .. } => {
            let mouse_btn = match mouse_btn {
                MouseButton::Left => Some(egui::PointerButton::Primary),
                MouseButton::Middle => Some(egui::PointerButton::Middle),
                MouseButton::Right => Some(egui::PointerButton::Secondary),
                _ => None,
            };
            if let Some(released) = mouse_btn {
                state.input.events.push(egui::Event::PointerButton {
                    pos: state.pointer_pos,
                    button: released,
                    pressed: false,
                    modifiers: state.modifiers,
                });
            }
        }

        MouseMotion { x, y, .. } => {
            state.pointer_pos = pos2(x as f32 / pixels_per_point, y as f32 / pixels_per_point);
            state
                .input
                .events
                .push(egui::Event::PointerMoved(state.pointer_pos));
        }

        KeyUp {
            keycode,
            keymod,
            repeat,
            ..
        } => {
            let key_code = match keycode {
                Some(key_code) => key_code,
                _ => return,
            };
            let key = match translate_virtual_key_code(key_code) {
                Some(key) => key,
                _ => return,
            };
            state.modifiers = Modifiers {
                alt: (keymod & Mod::LALTMOD == Mod::LALTMOD)
                    || (keymod & Mod::RALTMOD == Mod::RALTMOD),
                ctrl: (keymod & Mod::LCTRLMOD == Mod::LCTRLMOD)
                    || (keymod & Mod::RCTRLMOD == Mod::RCTRLMOD),
                shift: (keymod & Mod::LSHIFTMOD == Mod::LSHIFTMOD)
                    || (keymod & Mod::RSHIFTMOD == Mod::RSHIFTMOD),
                mac_cmd: keymod & Mod::LGUIMOD == Mod::LGUIMOD,

                //TOD: Test on both windows and mac
                command: (keymod & Mod::LCTRLMOD == Mod::LCTRLMOD)
                    || (keymod & Mod::LGUIMOD == Mod::LGUIMOD),
            };

            state.input.events.push(egui::Event::Key {
                key,
                pressed: false,
                repeat,
                modifiers: state.modifiers,
                // TODO: implement support for physical_key
                physical_key: None,
            });
        }

        KeyDown {
            keycode,
            keymod,
            repeat,
            ..
        } => {
            let key_code = match keycode {
                Some(key_code) => key_code,
                _ => return,
            };

            let key = match translate_virtual_key_code(key_code) {
                Some(key) => key,
                _ => return,
            };
            state.modifiers = Modifiers {
                alt: (keymod & Mod::LALTMOD == Mod::LALTMOD)
                    || (keymod & Mod::RALTMOD == Mod::RALTMOD),
                ctrl: (keymod & Mod::LCTRLMOD == Mod::LCTRLMOD)
                    || (keymod & Mod::RCTRLMOD == Mod::RCTRLMOD),
                shift: (keymod & Mod::LSHIFTMOD == Mod::LSHIFTMOD)
                    || (keymod & Mod::RSHIFTMOD == Mod::RSHIFTMOD),
                mac_cmd: keymod & Mod::LGUIMOD == Mod::LGUIMOD,

                //TOD: Test on both windows and mac
                command: (keymod & Mod::LCTRLMOD == Mod::LCTRLMOD)
                    || (keymod & Mod::LGUIMOD == Mod::LGUIMOD),
            };

            state.input.events.push(egui::Event::Key {
                key,
                pressed: true,
                repeat,
                modifiers: state.modifiers,
                // TODO: implement support for physical_key
                physical_key: None,
            });

            if state.modifiers.command && key == egui::Key::C {
                // println!("copy event");
                state.input.events.push(egui::Event::Copy);
            } else if state.modifiers.command && key == egui::Key::X {
                // println!("cut event");
                state.input.events.push(egui::Event::Cut);
            } else if state.modifiers.command && key == egui::Key::V {
                // println!("paste");
                if let Ok(contents) = window.subsystem().clipboard().clipboard_text() {
                    state.input.events.push(egui::Event::Text(contents));
                }
            }
        }

        TextInput { text, .. } => {
            state.input.events.push(egui::Event::Text(text));
        }

        MouseWheel { x, y, .. } => {
            let delta = vec2(x as f32 * 8.0, y as f32 * 8.0);
            state.input.events.push(egui::Event::MouseWheel {
                unit: MouseWheelUnit::Point,
                delta,
                // SDL reports discrete ticks with no gesture info, which is
                // the smoothing path; Start/End would wrongly track a touch.
                phase: egui::TouchPhase::Move,
                modifiers: state.modifiers,
            });
        }

        _ => {
            //dbg!(event);
        }
    }
}

pub fn translate_virtual_key_code(key: Keycode) -> Option<egui::Key> {
    Some(match key {
        Keycode::Left => egui::Key::ArrowLeft,
        Keycode::Up => egui::Key::ArrowUp,
        Keycode::Right => egui::Key::ArrowRight,
        Keycode::Down => egui::Key::ArrowDown,

        Keycode::Escape => egui::Key::Escape,
        Keycode::Tab => egui::Key::Tab,
        Keycode::Backspace => egui::Key::Backspace,
        Keycode::Space => egui::Key::Space,
        Keycode::Return => egui::Key::Enter,

        Keycode::Insert => egui::Key::Insert,
        Keycode::Home => egui::Key::Home,
        Keycode::Delete => egui::Key::Delete,
        Keycode::End => egui::Key::End,
        Keycode::PageDown => egui::Key::PageDown,
        Keycode::PageUp => egui::Key::PageUp,

        Keycode::Kp0 | Keycode::Num0 => egui::Key::Num0,
        Keycode::Kp1 | Keycode::Num1 => egui::Key::Num1,
        Keycode::Kp2 | Keycode::Num2 => egui::Key::Num2,
        Keycode::Kp3 | Keycode::Num3 => egui::Key::Num3,
        Keycode::Kp4 | Keycode::Num4 => egui::Key::Num4,
        Keycode::Kp5 | Keycode::Num5 => egui::Key::Num5,
        Keycode::Kp6 | Keycode::Num6 => egui::Key::Num6,
        Keycode::Kp7 | Keycode::Num7 => egui::Key::Num7,
        Keycode::Kp8 | Keycode::Num8 => egui::Key::Num8,
        Keycode::Kp9 | Keycode::Num9 => egui::Key::Num9,

        Keycode::A => egui::Key::A,
        Keycode::B => egui::Key::B,
        Keycode::C => egui::Key::C,
        Keycode::D => egui::Key::D,
        Keycode::E => egui::Key::E,
        Keycode::F => egui::Key::F,
        Keycode::G => egui::Key::G,
        Keycode::H => egui::Key::H,
        Keycode::I => egui::Key::I,
        Keycode::J => egui::Key::J,
        Keycode::K => egui::Key::K,
        Keycode::L => egui::Key::L,
        Keycode::M => egui::Key::M,
        Keycode::N => egui::Key::N,
        Keycode::O => egui::Key::O,
        Keycode::P => egui::Key::P,
        Keycode::Q => egui::Key::Q,
        Keycode::R => egui::Key::R,
        Keycode::S => egui::Key::S,
        Keycode::T => egui::Key::T,
        Keycode::U => egui::Key::U,
        Keycode::V => egui::Key::V,
        Keycode::W => egui::Key::W,
        Keycode::X => egui::Key::X,
        Keycode::Y => egui::Key::Y,
        Keycode::Z => egui::Key::Z,

        _ => {
            return None;
        }
    })
}

pub fn translate_cursor(fused: &mut FusedCursor, cursor_icon: egui::CursorIcon) {
    let tmp_icon = match cursor_icon {
        egui::CursorIcon::Crosshair => SystemCursor::Crosshair,
        egui::CursorIcon::Default => SystemCursor::Arrow,
        egui::CursorIcon::Grab => SystemCursor::Hand,
        egui::CursorIcon::Grabbing => SystemCursor::SizeAll,
        egui::CursorIcon::Move => SystemCursor::SizeAll,
        egui::CursorIcon::PointingHand => SystemCursor::Hand,
        egui::CursorIcon::ResizeHorizontal => SystemCursor::SizeWE,
        egui::CursorIcon::ResizeNeSw => SystemCursor::SizeNESW,
        egui::CursorIcon::ResizeNwSe => SystemCursor::SizeNWSE,
        egui::CursorIcon::ResizeVertical => SystemCursor::SizeNS,
        egui::CursorIcon::Text => SystemCursor::IBeam,
        egui::CursorIcon::NotAllowed | egui::CursorIcon::NoDrop => SystemCursor::No,
        egui::CursorIcon::Wait => SystemCursor::Wait,
        //There doesn't seem to be a suitable SDL equivalent...
        _ => SystemCursor::Arrow,
    };

    if tmp_icon != fused.icon {
        fused.cursor = Cursor::from_system(tmp_icon).ok();
        fused.icon = tmp_icon;
        if let Some(cursor) = &fused.cursor {
            cursor.set();
        }
    }
}
