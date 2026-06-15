//! egui settings panels: the input-rebinding panel and the DIP switch panel.

use phosphor_core::core::machine::{
    ActionRole, DipApplyTiming, DipSwitchBank, InputControl, InputId, InputKind,
};

use crate::input::BindingSet;

/// Width of the settings side panel in pixels.
pub const PANEL_WIDTH: u32 = 340;

/// A DIP option edit recorded by the DIP panel and applied to the machine
/// after the egui frame — the panel never holds `&mut machine`.
#[derive(Clone, Copy)]
pub struct PendingDipChange {
    /// Index into the machine's `dip_banks()`.
    pub bank: usize,
    /// Index into that bank's `options`.
    pub option: usize,
    /// The chosen value's masked bits (passed straight to `set_dip_option`).
    pub value: u8,
}

/// UI state for the settings panels, owned by the emulator loop.
#[derive(Default)]
pub struct SettingsState {
    /// Whether the input-rebinding panel is visible.
    pub active: bool,
    /// Control awaiting a captured physical input (rebind in progress).
    pub capturing: Option<InputId>,
    /// Set when the user asks to reset bindings to machine defaults; the
    /// emulator loop consumes it (rebuilds the binding set) and clears it.
    pub reset_requested: bool,
    /// Whether the DIP switch panel is visible.
    pub dip_active: bool,
    /// DIP option edits recorded this frame; the emulator loop applies them via
    /// `set_dip_option` after the UI frame and clears the buffer.
    pub pending_dip_changes: Vec<PendingDipChange>,
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
                        for &idx in &display_order(controls) {
                            let control = &controls[idx];
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

/// Display order for the rebinding grid: group controls by player, and within a
/// player push the ranked action buttons (Primary, Secondary, Tertiary) after
/// the directions/start so the action ladder reads top-to-bottom. Stable, so
/// controls sharing a key keep their machine-declared order. Shared/system
/// controls (no player, e.g. coin) sort last.
fn display_order(controls: &[InputControl]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..controls.len()).collect();
    order.sort_by_key(|&i| {
        let c = &controls[i];
        let player = c.player.map_or(u16::MAX, u16::from);
        let role_rank = match c.kind {
            InputKind::Action(ActionRole::Primary) => 1,
            InputKind::Action(ActionRole::Secondary) => 2,
            InputKind::Action(ActionRole::Tertiary) => 3,
            _ => 0,
        };
        (player, role_rank)
    });
    order
}

/// Draw the DIP switch side panel.
///
/// `banks` is the machine's static metadata and `bank_values` the live byte of
/// each bank (same order), snapshotted before this frame. The selected choice
/// per option is decoded by matching `choice.value` against the bank byte
/// masked by the option. Edits are recorded in `state.pending_dip_changes`; the
/// emulator loop applies them after the frame, avoiding a mutable machine
/// borrow here.
pub fn draw_dip_panel(
    ctx: &egui::Context,
    banks: &[DipSwitchBank],
    bank_values: &[u8],
    state: &mut SettingsState,
) {
    egui::SidePanel::right("dip_settings_panel")
        .default_width(PANEL_WIDTH as f32)
        .resizable(false)
        .show(ctx, |ui| {
            ui.heading("DIP Switches");
            ui.separator();
            ui.label(
                "Options marked “(applies on reset)” take effect after the next machine reset.",
            );
            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| {
                for (bank_idx, bank) in banks.iter().enumerate() {
                    let bank_value = bank_values.get(bank_idx).copied().unwrap_or(0);
                    egui::CollapsingHeader::new(bank.name)
                        .default_open(true)
                        .show(ui, |ui| {
                            egui::Grid::new(("dip_grid", bank_idx))
                                .num_columns(2)
                                .striped(true)
                                .show(ui, |ui| {
                                    for (opt_idx, opt) in bank.options.iter().enumerate() {
                                        let mut name = opt.name.to_string();
                                        if matches!(opt.apply, DipApplyTiming::OnReset) {
                                            name.push_str(" (applies on reset)");
                                        }
                                        ui.label(name);

                                        let current = bank_value & opt.mask;
                                        let selected_label = opt
                                            .choices
                                            .iter()
                                            .find(|c| c.value == current)
                                            .map(|c| c.label)
                                            .unwrap_or("(custom)");

                                        egui::ComboBox::from_id_salt(("dip", bank_idx, opt_idx))
                                            .selected_text(selected_label)
                                            .show_ui(ui, |ui| {
                                                for choice in opt.choices {
                                                    if ui
                                                        .selectable_label(
                                                            choice.value == current,
                                                            choice.label,
                                                        )
                                                        .clicked()
                                                    {
                                                        state.pending_dip_changes.push(
                                                            PendingDipChange {
                                                                bank: bank_idx,
                                                                option: opt_idx,
                                                                value: choice.value,
                                                            },
                                                        );
                                                    }
                                                }
                                            });
                                        ui.end_row();
                                    }
                                });
                        });
                }
            });
        });
}
