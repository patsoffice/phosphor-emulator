//! egui settings panels. Currently hosts the input-rebinding panel; the DIP
//! switch panel will live here too.

use phosphor_core::core::machine::{InputControl, InputId, InputKind};

use crate::input::BindingSet;

/// Width of the settings side panel in pixels.
pub const PANEL_WIDTH: u32 = 340;

/// UI state for the settings panels, owned by the emulator loop.
#[derive(Default)]
pub struct SettingsState {
    /// Whether the panel is visible.
    pub active: bool,
    /// Control awaiting a captured physical input (rebind in progress).
    pub capturing: Option<InputId>,
    /// Set when the user asks to reset bindings to machine defaults; the
    /// emulator loop consumes it (rebuilds the binding set) and clears it.
    pub reset_requested: bool,
}

/// Draw the input-rebinding side panel.
///
/// Reads the current `bindings` for display and records the user's intent in
/// `state` (a capture request or a reset request); the emulator loop applies
/// those to the binding set after the frame, avoiding a mutable borrow here.
pub fn draw_input_panel(
    ctx: &egui::Context,
    controls: &[InputControl],
    bindings: &BindingSet,
    state: &mut SettingsState,
) {
    egui::SidePanel::right("input_settings_panel")
        .default_width(PANEL_WIDTH as f32)
        .resizable(false)
        .show(ctx, |ui| {
            ui.heading("Input Bindings");
            ui.separator();
            if ui.button("Reset to defaults").clicked() {
                state.reset_requested = true;
                state.capturing = None;
            }
            ui.label("Click a binding, then press a key or button (Esc cancels).");
            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| {
                egui::Grid::new("bindings_grid")
                    .num_columns(2)
                    .striped(true)
                    .show(ui, |ui| {
                        for control in controls {
                            ui.label(control.label);

                            let bound: Vec<String> = bindings
                                .physical_for(control.id)
                                .map(|p| p.display_name())
                                .collect();

                            if matches!(control.kind, InputKind::AnalogAxis { .. }) {
                                // Analog axes (trackball / spinner) are not rebindable here.
                                ui.label(if bound.is_empty() {
                                    "—".to_string()
                                } else {
                                    bound.join(", ")
                                });
                            } else {
                                let capturing = state.capturing == Some(control.id);
                                let text = if capturing {
                                    "press input…".to_string()
                                } else if bound.is_empty() {
                                    "(unbound)".to_string()
                                } else {
                                    bound.join(", ")
                                };
                                if ui.button(text).clicked() {
                                    state.capturing =
                                        if capturing { None } else { Some(control.id) };
                                }
                            }
                            ui.end_row();
                        }
                    });
            });
        });
}
