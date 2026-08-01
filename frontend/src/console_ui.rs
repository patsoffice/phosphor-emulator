//! Interactive Rhai console side panel (Ctrl+` to toggle).
//!
//! A near-clone of the DIP side panel: a right `SidePanel` with a scrollback of
//! prior output and a single-line input. The typed command is captured here and
//! the emulator loop evaluates it against the *live* machine after the egui
//! frame — the same snapshot-before / act-after pattern the DIP panel uses to
//! avoid holding `&mut machine` inside the closure.

use crate::settings_ui::PANEL_WIDTH;

/// Cap on scrollback lines kept in memory.
const MAX_SCROLLBACK: usize = 500;

/// Interactive console state, owned by the emulator loop.
#[derive(Default)]
pub struct ConsoleState {
    /// Whether the panel is shown. While shown, the emulator routes keyboard
    /// input to the console (game input and other hotkeys are suppressed).
    pub visible: bool,
    /// The current input line.
    input: String,
    /// Scrollback: echoed commands and their output.
    output: Vec<String>,
    /// A command submitted this frame, for the loop to evaluate after the egui
    /// closure (when `&mut machine` is available again).
    pending: Option<String>,
    /// Set when the panel was just opened, so the input line grabs focus.
    just_opened: bool,
}

impl ConsoleState {
    /// Toggle visibility; opening requests input focus.
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
        if self.visible {
            self.just_opened = true;
        }
    }

    /// Take the command submitted this frame, if any.
    pub fn take_pending(&mut self) -> Option<String> {
        self.pending.take()
    }

    /// Append output (splitting on newlines), bounding the scrollback.
    pub fn push_output(&mut self, text: &str) {
        for line in text.lines() {
            self.output.push(line.to_string());
        }
        if self.output.len() > MAX_SCROLLBACK {
            let drop = self.output.len() - MAX_SCROLLBACK;
            self.output.drain(0..drop);
        }
    }
}

/// Draw the console side panel. Records a submitted command into
/// `state.pending`; the emulator loop evaluates it after the frame.
pub fn draw_console_panel(ctx: &egui::Context, state: &mut ConsoleState) {
    egui::SidePanel::right("rhai_console_panel")
        .default_width(PANEL_WIDTH as f32)
        .resizable(false)
        .show(ctx, |ui| {
            ui.heading("Console (Rhai)");
            ui.separator();
            ui.label(
                "`m` is the live machine — e.g. m.read(0, 0x8000), m.poke(0, a, v), m.watch(a, \"write\").",
            );
            ui.separator();

            // Reserve a row for the input; the scrollback fills the rest.
            let input_row = 30.0;
            let scroll_height = (ui.available_height() - input_row).max(0.0);
            egui::ScrollArea::vertical()
                .max_height(scroll_height)
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for line in &state.output {
                        ui.label(egui::RichText::new(line).monospace());
                    }
                });

            ui.separator();
            ui.horizontal(|ui| {
                ui.label(">");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut state.input)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace),
                );
                if state.just_opened {
                    resp.request_focus();
                    state.just_opened = false;
                }
                let submitted = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if submitted {
                    let cmd = state.input.trim().to_string();
                    state.input.clear();
                    if !cmd.is_empty() {
                        state.output.push(format!("> {cmd}"));
                        state.pending = Some(cmd);
                    }
                    // Keep focus so the next command can be typed immediately.
                    resp.request_focus();
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_opens_and_requests_focus() {
        let mut c = ConsoleState::default();
        assert!(!c.visible);
        c.toggle();
        assert!(c.visible);
        assert!(c.just_opened);
        c.toggle();
        assert!(!c.visible);
    }

    #[test]
    fn take_pending_is_one_shot() {
        let mut c = ConsoleState::default();
        assert!(c.take_pending().is_none());
        c.pending = Some("m.read(0, 0)".to_string());
        assert_eq!(c.take_pending().as_deref(), Some("m.read(0, 0)"));
        assert!(c.take_pending().is_none());
    }

    #[test]
    fn push_output_splits_newlines() {
        let mut c = ConsoleState::default();
        c.push_output("a\nb\nc");
        assert_eq!(c.output, vec!["a", "b", "c"]);
    }

    #[test]
    fn scrollback_is_bounded() {
        let mut c = ConsoleState::default();
        for i in 0..MAX_SCROLLBACK + 50 {
            c.push_output(&format!("line {i}"));
        }
        assert_eq!(c.output.len(), MAX_SCROLLBACK);
        assert_eq!(c.output[0], "line 50"); // oldest 50 dropped
    }
}
