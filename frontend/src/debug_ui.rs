use std::collections::HashSet;

use phosphor_core::core::debug::{BusDebug, DebugCpu, DebugRegister};
use phosphor_core::core::debug_trace::DebugEvent;
use phosphor_core::core::machine::FrontendMachine;
use phosphor_core::core::watchpoint::{DebugAccessSource, WatchpointPhase};
use phosphor_core::core::{DebugRead, WatchpointHit, WatchpointKind};
use phosphor_core::cpu::hex_bytes;

/// Format an address at its natural width: 4 hex digits within the 16-bit
/// range, 6 within 24-bit (M68000), 8 beyond. Keeps 16-bit machines'
/// displays unchanged while letting wide addresses show in full.
fn fmt_addr(addr: u32) -> String {
    if addr <= 0xFFFF {
        format!("{addr:04X}")
    } else if addr <= 0x00FF_FFFF {
        format!("{addr:06X}")
    } else {
        format!("{addr:08X}")
    }
}

/// Format a value as zero-padded hex sized to its access width in **bytes**:
/// 1 byte → 2 digits, 2 → 4, 4 → 8. No `$` prefix; callers add one.
///
/// Note the unit: `WatchpointHit::width` and `DebugEvent::width` are byte
/// counts, while `DebugRegister::width` is a *bit* count and must be divided
/// before being passed here.
fn fmt_hex_value(value: impl Into<u64>, byte_width: u8) -> String {
    let value = value.into();
    match byte_width {
        2 => format!("{value:04X}"),
        4 => format!("{value:08X}"),
        _ => format!("{value:02X}"),
    }
}

/// Execution modes for the debug interface.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RunMode {
    Running,
    Paused,
    StepInstruction,
    StepCycle,
    /// Run one full frame, then pause at the start of the next frame.
    StepFrame,
}

/// Which tab is shown in the bottom half of a CPU column.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BottomTab {
    Disassembly,
    Memory,
}

/// Cached register snapshot for one CPU.
pub struct CpuPanel {
    pub name: String,
    pub registers: Vec<DebugRegister>,
}

/// Cached register snapshot for one peripheral device.
pub struct DevicePanel {
    pub name: String,
    pub registers: Vec<DebugRegister>,
    /// Index in `BusDebug::devices()` order (CPUs occupy the first
    /// indices), used for `reset_device`/`write_device_register` dispatch.
    pub device_index: usize,
}

/// A device control requested by the UI, applied to the machine at the
/// start of the next `execute_frame` (panel drawing has no machine access).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DeviceAction {
    /// Reset the device at this `devices()` index.
    Reset(usize),
    /// Write a register byte on the device at this `devices()` index.
    WriteRegister {
        device_index: usize,
        offset: u16,
        value: u8,
    },
}

/// Persistent state for the debug UI across frames.
///
/// Layout: multi-column, expanding to the right.
///   Column 0: step controls, breakpoints, devices
///   Column 1..N: one per CPU (registers + disassembly/memory)
pub struct DebugState {
    pub active: bool,
    pub run_mode: RunMode,
    /// Global pause toggled by the P key, independent of the debug UI. When the
    /// debug UI is inactive this gates emulation in [`execute_frame`]; while the
    /// debug UI is active it is ignored and `run_mode` governs instead.
    pub global_paused: bool,
    pub cpu_panels: Vec<CpuPanel>,
    pub device_panels: Vec<DevicePanel>,
    pub step_cpu: usize,
    pub cycle_count: u64,
    /// Running count of completed frames (also shown on the F10 overlay).
    pub frame_count: u64,

    // Breakpoints
    /// PC breakpoints per CPU (index = cpu_index).
    pub breakpoints: Vec<HashSet<u32>>,
    /// Hex address input buffer for adding PC breakpoints.
    pub breakpoint_input: String,
    /// Break when cycle_count reaches this value.
    pub cycle_breakpoint: Option<u64>,
    /// Input buffer for cycle breakpoint.
    pub cycle_bp_input: String,
    /// Break at the start of this frame (when frame_count reaches it).
    pub frame_breakpoint: Option<u64>,
    /// Input buffer for the frame breakpoint.
    pub frame_bp_input: String,

    // Watchpoints
    /// Active memory watchpoints: (cpu_index, address, kind).
    pub watchpoints: Vec<(usize, u32, WatchpointKind)>,
    /// Hex address input buffer for adding watchpoints.
    pub watchpoint_input: String,
    /// Whether the next watchpoint should watch reads.
    pub watchpoint_read: bool,
    /// Whether the next watchpoint should watch writes.
    pub watchpoint_write: bool,
    /// Last watchpoint hit (displayed until user continues).
    pub last_watchpoint_hit: Option<WatchpointHit>,
    /// True when the UI has modified watchpoints and they need to be synced to the machine.
    pub watchpoints_dirty: bool,

    // Event trace
    /// Whether event tracing is enabled (UI checkbox, synced to machine).
    pub trace_enabled: bool,
    /// True when `trace_enabled` changed and needs to be synced to the machine.
    pub trace_enabled_dirty: bool,
    /// True when the user requested the trace be cleared.
    pub trace_clear_requested: bool,
    /// Snapshot of the machine's event ring for display (refreshed each
    /// frame while tracing is enabled).
    pub trace_events: Vec<DebugEvent>,

    // Device controls
    /// Device actions requested by the UI, applied on the next frame.
    pub pending_device_actions: Vec<DeviceAction>,
    /// Per-device-panel (offset, value) hex input buffers for register writes.
    pub device_write_inputs: Vec<(String, String)>,

    // Per-CPU column state
    /// Which tab (Disassembly/Memory) is selected per CPU column.
    pub bottom_tabs: Vec<BottomTab>,
    /// Memory viewer address input buffer per CPU.
    pub memory_addr_inputs: Vec<String>,
    /// Pending scroll-to offset per CPU (consumed on next draw).
    pub memory_scroll_to: Vec<Option<f32>>,
    /// 64 KB-aligned base of the memory viewer window per CPU. Always 0
    /// for 16-bit machines; "Go" retargets it for wider address spaces.
    pub memory_view_base: Vec<u32>,

    // Layout alignment
    /// Max top-section height from the previous frame (controls/registers).
    /// Used to align the disassembly/memory separator across all columns.
    pub top_section_height: f32,
}

impl DebugState {
    pub fn new() -> Self {
        Self {
            active: false,
            run_mode: RunMode::Running,
            global_paused: false,
            cpu_panels: Vec::new(),
            device_panels: Vec::new(),
            step_cpu: 0,
            cycle_count: 0,
            frame_count: 0,
            breakpoints: Vec::new(),
            breakpoint_input: String::new(),
            cycle_breakpoint: None,
            cycle_bp_input: String::new(),
            frame_breakpoint: None,
            frame_bp_input: String::new(),
            watchpoints: Vec::new(),
            watchpoint_input: String::new(),
            watchpoint_read: false,
            watchpoint_write: true,
            last_watchpoint_hit: None,
            watchpoints_dirty: false,
            trace_enabled: false,
            trace_enabled_dirty: false,
            trace_clear_requested: false,
            trace_events: Vec::new(),
            pending_device_actions: Vec::new(),
            device_write_inputs: Vec::new(),
            bottom_tabs: Vec::new(),
            memory_addr_inputs: Vec::new(),
            memory_scroll_to: Vec::new(),
            memory_view_base: Vec::new(),
            top_section_height: 0.0,
        }
    }

    /// True if any PC, cycle, or memory watchpoint is set.
    pub fn has_any_breakpoints(&self) -> bool {
        self.cycle_breakpoint.is_some()
            || self.breakpoints.iter().any(|s| !s.is_empty())
            || !self.watchpoints.is_empty()
    }

    /// Width (in pixels) needed for the debug panel, based on CPU count.
    pub fn debug_panel_width(&self) -> u32 {
        let n_cpus = self.cpu_panels.len().max(1) as u32;
        260 * (n_cpus + 1)
    }

    /// Refresh cached state from the BusDebug interface.
    pub fn refresh(&mut self, bus: &dyn BusDebug) {
        let cpus = bus.cpus();
        let cpu_names: Vec<&str> = cpus.iter().map(|(name, _)| *name).collect();

        self.cpu_panels = cpus
            .iter()
            .map(|(name, cpu)| CpuPanel {
                name: name.to_string(),
                registers: cpu.debug_registers(),
            })
            .collect();

        // Device panels exclude CPUs (they're already shown in cpu_panels)
        // but keep the devices()-order index for control dispatch.
        self.device_panels = bus
            .devices()
            .iter()
            .enumerate()
            .filter(|(_, (name, _))| !cpu_names.contains(name))
            .map(|(device_index, (name, dev))| DevicePanel {
                name: name.to_string(),
                registers: dev.debug_registers(),
                device_index,
            })
            .collect();

        // Extend per-CPU vectors to match CPU count
        while self.breakpoints.len() < cpus.len() {
            self.breakpoints.push(HashSet::new());
        }
        while self.bottom_tabs.len() < cpus.len() {
            self.bottom_tabs.push(BottomTab::Disassembly);
        }
        while self.memory_addr_inputs.len() < cpus.len() {
            self.memory_addr_inputs.push(String::new());
        }
        while self.memory_scroll_to.len() < cpus.len() {
            self.memory_scroll_to.push(None);
        }
        while self.memory_view_base.len() < cpus.len() {
            self.memory_view_base.push(0);
        }
        while self.device_write_inputs.len() < self.device_panels.len() {
            self.device_write_inputs
                .push((String::new(), String::new()));
        }

        if self.step_cpu >= self.cpu_panels.len() && !self.cpu_panels.is_empty() {
            self.step_cpu = 0;
        }
    }
}

/// Execute one frame of emulation according to the current run mode.
/// Returns true if a full frame was executed (caller should drain audio).
pub fn execute_frame(machine: &mut dyn FrontendMachine, state: &mut DebugState) -> bool {
    if !state.active {
        // Global pause (P key): hold the machine without running a frame, so no
        // audio is drained. The audio callback repeats its last sample, so the
        // output stays silent rather than buzzing.
        if state.global_paused {
            return false;
        }
        machine.run_frame();
        state.frame_count += 1;
        return true;
    }

    // Sync watchpoints to machine if the UI changed them
    if state.watchpoints_dirty {
        state.watchpoints_dirty = false;
        machine.clear_all_watchpoints();
        for &(cpu_idx, addr, kind) in &state.watchpoints {
            machine.set_watchpoint(cpu_idx, addr, kind);
        }
    }

    // Sync event-trace controls and snapshot the ring for the panel.
    // (The panel draws from the snapshot; events recorded this frame
    // appear on the next draw.)
    if state.trace_enabled_dirty {
        state.trace_enabled_dirty = false;
        machine.set_trace_enabled(state.trace_enabled);
    }
    if state.trace_clear_requested {
        state.trace_clear_requested = false;
        machine.clear_trace_events();
        state.trace_events.clear();
    }
    if state.trace_enabled {
        state.trace_events.clear();
        state.trace_events.extend_from_slice(machine.trace_events());
    }

    // Apply device controls requested by the UI (reset / register write).
    if !state.pending_device_actions.is_empty() {
        if let Some(bus) = machine.debug_bus_mut() {
            for action in &state.pending_device_actions {
                match *action {
                    DeviceAction::Reset(device_index) => bus.reset_device(device_index),
                    DeviceAction::WriteRegister {
                        device_index,
                        offset,
                        value,
                    } => bus.write_device_register(device_index, offset, value),
                }
            }
        }
        state.pending_device_actions.clear();
        // Show the effect immediately, even while paused.
        if let Some(bus) = machine.debug_bus() {
            state.refresh(bus);
        }
    }

    match state.run_mode {
        RunMode::Running => {
            // Frame breakpoint: pause at the start of the target frame, before
            // running any of its cycles.
            if let Some(target) = state.frame_breakpoint
                && state.frame_count >= target
            {
                state.frame_breakpoint = None;
                state.run_mode = RunMode::Paused;
                if let Some(bus) = machine.debug_bus() {
                    state.refresh(bus);
                }
                return false;
            }

            let cpf = machine.cycles_per_frame();
            if cpf > 0 {
                let check_bp = state.has_any_breakpoints();
                for _ in 0..cpf {
                    let boundaries = machine.debug_tick();
                    state.cycle_count += 1;

                    if check_bp {
                        // Cycle breakpoint
                        if let Some(target) = state.cycle_breakpoint
                            && state.cycle_count >= target
                        {
                            state.cycle_breakpoint = None;
                            state.run_mode = RunMode::Paused;
                            if let Some(bus) = machine.debug_bus() {
                                state.refresh(bus);
                            }
                            return false;
                        }

                        // PC breakpoints (only check at instruction boundaries)
                        if boundaries != 0
                            && let Some(bus) = machine.debug_bus()
                        {
                            let cpus = bus.cpus();
                            for (i, (_name, cpu)) in cpus.iter().enumerate() {
                                if (boundaries >> i) & 1 != 0
                                    && let Some(bp_set) = state.breakpoints.get(i)
                                    && bp_set.contains(&cpu.debug_pc())
                                {
                                    state.refresh(bus);
                                    state.run_mode = RunMode::Paused;
                                    return false;
                                }
                            }
                        }

                        // Memory watchpoint hits
                        if let Some(hit) = machine.take_watchpoint_hit() {
                            state.last_watchpoint_hit = Some(hit);
                            state.run_mode = RunMode::Paused;
                            if let Some(bus) = machine.debug_bus() {
                                state.refresh(bus);
                            }
                            return false;
                        }
                    }
                }
            } else {
                machine.run_frame();
            }
            state.frame_count += 1;
            if let Some(bus) = machine.debug_bus() {
                state.refresh(bus);
            }
            true
        }
        RunMode::Paused => {
            if let Some(bus) = machine.debug_bus() {
                state.refresh(bus);
            }
            false
        }
        RunMode::StepInstruction => {
            loop {
                let boundaries = machine.debug_tick();
                state.cycle_count += 1;
                if let Some(hit) = machine.take_watchpoint_hit() {
                    state.last_watchpoint_hit = Some(hit);
                }
                if (boundaries >> state.step_cpu) & 1 != 0 {
                    break;
                }
            }
            if let Some(bus) = machine.debug_bus() {
                state.refresh(bus);
            }
            state.run_mode = RunMode::Paused;
            false
        }
        RunMode::StepCycle => {
            machine.debug_tick();
            state.cycle_count += 1;
            if let Some(hit) = machine.take_watchpoint_hit() {
                state.last_watchpoint_hit = Some(hit);
            }
            if let Some(bus) = machine.debug_bus() {
                state.refresh(bus);
            }
            state.run_mode = RunMode::Paused;
            false
        }
        RunMode::StepFrame => {
            // Run one full frame's worth of cycles, then pause at the next
            // frame's start. Returns true so the frame's audio is drained.
            let cpf = machine.cycles_per_frame();
            if cpf > 0 {
                for _ in 0..cpf {
                    machine.debug_tick();
                    state.cycle_count += 1;
                    if let Some(hit) = machine.take_watchpoint_hit() {
                        state.last_watchpoint_hit = Some(hit);
                    }
                }
            } else {
                machine.run_frame();
            }
            state.frame_count += 1;
            if let Some(bus) = machine.debug_bus() {
                state.refresh(bus);
            }
            state.run_mode = RunMode::Paused;
            true
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn draw_register_grid(ui: &mut egui::Ui, id: &str, registers: &[DebugRegister]) {
    egui::Grid::new(id)
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            for reg in registers {
                ui.label(egui::RichText::new(reg.name).monospace());
                // reg.width is in BITS; fmt_hex_value takes BYTES.
                let value_text = format!("${}", fmt_hex_value(reg.value, reg.width / 8));
                ui.label(egui::RichText::new(value_text).monospace());
                ui.end_row();
            }
        });
}

// ---------------------------------------------------------------------------
// Main layout
// ---------------------------------------------------------------------------

/// Build the debug UI layout. Called as the closure argument to Video::present_with_debug().
///
/// Layout:
///   [Game] | [Controls col] | [CPU 0 col] | [CPU 1 col] | ...
///
/// Each CPU column shows registers at the top and a tabbed disassembly/memory
/// viewer below.
pub fn draw_debug_ui(
    ctx: &egui::Context,
    game_texture_id: egui::TextureId,
    view_aspect: f32,
    state: &mut DebugState,
    bus: Option<&dyn BusDebug>,
) {
    let n_cpus = state.cpu_panels.len();

    // Right panel: multi-column debug layout
    egui::SidePanel::right("debug_panel")
        .default_width(260.0 * (n_cpus + 1).max(2) as f32)
        .resizable(true)
        .show(ctx, |ui| {
            if n_cpus > 0 {
                ui.columns(n_cpus + 1, |cols| {
                    // Use the previous frame's max top-section height for alignment
                    let min_h = state.top_section_height;
                    let h0 = draw_controls_column(&mut cols[0], state, min_h);
                    let mut max_h = h0;
                    for cpu_idx in 0..n_cpus {
                        let h = draw_cpu_column(&mut cols[cpu_idx + 1], state, bus, cpu_idx, min_h);
                        max_h = max_h.max(h);
                    }
                    state.top_section_height = max_h;
                });
            } else {
                draw_controls_column(ui, state, 0.0);
            }
        });

    // Central panel: game framebuffer letterboxed to the target display aspect.
    egui::CentralPanel::default()
        .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
        .show(ctx, |ui| {
            let (size, offset) = crate::emulator::fit_aspect(ui.available_size(), view_aspect);
            ui.add_space(offset.y);
            ui.horizontal(|ui| {
                ui.add_space(offset.x);
                ui.image(egui::load::SizedTexture::new(game_texture_id, size));
            });
        });
}

// ---------------------------------------------------------------------------
// Controls column (leftmost debug column)
// ---------------------------------------------------------------------------

/// Draw the controls column. Returns the natural height of the top section
/// (controls + breakpoints, before padding).
fn draw_controls_column(ui: &mut egui::Ui, state: &mut DebugState, min_top_height: f32) -> f32 {
    let top_y = ui.cursor().top();

    // --- Top section: controls + breakpoints ---
    ui.label(format!("Cycles: {}", state.cycle_count));
    ui.separator();

    let is_paused = state.run_mode == RunMode::Paused;

    ui.horizontal(|ui| {
        // Key `0` toggles run <-> pause (see emulator.rs).
        if state.run_mode == RunMode::Running {
            if ui.button("Pause (0)").clicked() {
                state.run_mode = RunMode::Paused;
            }
        } else if ui.button("Continue (0)").clicked() {
            state.run_mode = RunMode::Running;
            state.last_watchpoint_hit = None;
        }
    });

    ui.horizontal(|ui| {
        if ui
            .add_enabled(is_paused, egui::Button::new("Step Instr (7)"))
            .clicked()
        {
            state.run_mode = RunMode::StepInstruction;
        }
        if ui
            .add_enabled(is_paused, egui::Button::new("Step Cycle (8)"))
            .clicked()
        {
            state.run_mode = RunMode::StepCycle;
        }
        if ui
            .add_enabled(is_paused, egui::Button::new("Step Frame (9)"))
            .clicked()
        {
            state.run_mode = RunMode::StepFrame;
        }
    });

    // Step-CPU target (only for multi-CPU machines)
    if state.cpu_panels.len() > 1 {
        ui.separator();
        ui.label("Step target:");
        for (i, panel) in state.cpu_panels.iter().enumerate() {
            ui.radio_value(&mut state.step_cpu, i, &panel.name);
        }
    }

    // Breakpoints & Watchpoints & Event trace
    draw_breakpoints_panel(ui, state);
    draw_watchpoints_panel(ui, state);
    draw_event_trace_panel(ui, state);

    let natural_height = ui.cursor().top() - top_y;

    // Pad to align with CPU columns
    if natural_height < min_top_height {
        ui.add_space(min_top_height - natural_height);
    }

    // --- Bottom section: devices ---
    ui.separator();
    egui::ScrollArea::vertical()
        .id_salt("ctrl_scroll")
        .show(ui, |ui| {
            for i in 0..state.device_panels.len() {
                let name = state.device_panels[i].name.clone();
                let id = egui::Id::new(format!("dev_{i}"));
                egui::CollapsingHeader::new(egui::RichText::new(name).monospace())
                    .id_salt(id)
                    .default_open(false)
                    .show(ui, |ui| {
                        draw_register_grid(
                            ui,
                            &format!("dev_regs_{i}"),
                            &state.device_panels[i].registers,
                        );
                        draw_device_controls(ui, state, i);
                    });
            }
        });

    natural_height
}

/// Device control row: reset button + register write (offset/value) inputs.
/// Actions queue into `pending_device_actions` and apply on the next frame.
fn draw_device_controls(ui: &mut egui::Ui, state: &mut DebugState, panel_index: usize) {
    let device_index = state.device_panels[panel_index].device_index;

    ui.add_space(2.0);
    ui.horizontal(|ui| {
        if ui
            .button("Reset")
            .on_hover_text("Reset this device to power-on state")
            .clicked()
        {
            state
                .pending_device_actions
                .push(DeviceAction::Reset(device_index));
        }

        let Some((offset_input, value_input)) = state.device_write_inputs.get_mut(panel_index)
        else {
            return;
        };
        // Enter commits from the value field only, matching the pre-existing
        // behaviour — Enter in the offset field does not write.
        let _ = entry_field(ui, "+$", offset_input, 28.0);
        let enter = entry_field(ui, "=$", value_input, 22.0);
        if (ui.button("Write").clicked() || enter)
            && let (Ok(offset), Ok(value)) = (
                u16::from_str_radix(offset_input.trim_start_matches('$'), 16),
                u8::from_str_radix(value_input.trim_start_matches('$'), 16),
            )
        {
            state
                .pending_device_actions
                .push(DeviceAction::WriteRegister {
                    device_index,
                    offset,
                    value,
                });
        }
    });
}

// ---------------------------------------------------------------------------
// Per-CPU column
// ---------------------------------------------------------------------------

/// Draw a CPU column. Returns the natural height of the register section
/// (before padding).
fn draw_cpu_column(
    ui: &mut egui::Ui,
    state: &mut DebugState,
    bus: Option<&dyn BusDebug>,
    cpu_idx: usize,
    min_top_height: f32,
) -> f32 {
    let top_y = ui.cursor().top();

    // --- Top section: registers ---
    if let Some(panel) = state.cpu_panels.get(cpu_idx) {
        egui::CollapsingHeader::new(egui::RichText::new(&panel.name).monospace().strong())
            .id_salt(egui::Id::new(format!("cpu_{cpu_idx}")))
            .default_open(true)
            .show(ui, |ui| {
                draw_register_grid(ui, &format!("cpu_regs_{cpu_idx}"), &panel.registers);
            });
    }

    let natural_height = ui.cursor().top() - top_y;

    // Pad to align with the tallest column
    if natural_height < min_top_height {
        ui.add_space(min_top_height - natural_height);
    }

    // --- Bottom section: disassembly / memory ---
    ui.separator();

    if cpu_idx < state.bottom_tabs.len() {
        ui.horizontal(|ui| {
            ui.selectable_value(
                &mut state.bottom_tabs[cpu_idx],
                BottomTab::Disassembly,
                "Disasm",
            );
            ui.selectable_value(&mut state.bottom_tabs[cpu_idx], BottomTab::Memory, "Memory");
        });
        ui.separator();

        if let Some(bus) = bus {
            match state.bottom_tabs[cpu_idx] {
                BottomTab::Disassembly => draw_disassembly_panel(ui, state, bus, cpu_idx),
                BottomTab::Memory => draw_memory_panel(ui, state, bus, cpu_idx),
            }
        }
    }

    natural_height
}

// ---------------------------------------------------------------------------
// Breakpoints panel (controls column)
// ---------------------------------------------------------------------------

/// Color for an "invalid input" hint shown next to a debug text field.
const INVALID_HINT: egui::Color32 = egui::Color32::from_rgb(220, 90, 90);

/// A labelled monospace entry field. Returns true on the frame the user
/// commits it by pressing Enter in the field.
///
/// This stops short of the commit button because its placement varies: the
/// watchpoint row puts its R/W checkboxes between field and button, and the
/// device-write row has one button serving two fields. Rows where the button
/// does follow the field use [`entry_commit`] instead.
fn entry_field(ui: &mut egui::Ui, label: &str, buf: &mut String, width: f32) -> bool {
    ui.label(label);
    let resp = ui.add(
        egui::TextEdit::singleline(buf)
            .desired_width(width)
            .font(egui::TextStyle::Monospace),
    );
    resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

/// [`entry_field`] plus a commit button beside it. Returns the parsed value on
/// the frame the user commits, by button or by Enter.
///
/// `parse` is supplied per call site rather than fixed to hex because two of
/// these fields (the cycle and frame breakpoints) take decimal counts, and
/// because the hex ones differ in whether they tolerate surrounding
/// whitespace. Clearing the field on success is left to the caller — the
/// memory "Go" field deliberately keeps its text.
fn entry_commit<T>(
    ui: &mut egui::Ui,
    label: &str,
    buf: &mut String,
    width: f32,
    button: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Option<T> {
    let enter = entry_field(ui, label, buf, width);
    if ui.button(button).clicked() || enter {
        parse(buf)
    } else {
        None
    }
}

/// True if a `$`-prefixed hex address field's text is acceptable: empty
/// (nothing typed yet) or a valid hex `u32`. `!hex_input_ok` therefore means
/// "the user typed something that isn't a valid address", which drives the
/// inline "hex?" hint so parse failures aren't silent.
fn hex_input_ok(s: &str) -> bool {
    let t = s.trim().trim_start_matches('$');
    t.is_empty() || u32::from_str_radix(t, 16).is_ok()
}

/// The active step-CPU's name as a header suffix (" — <cpu>"), shown only on
/// multi-CPU machines so it's clear which CPU's space breakpoints/watchpoints
/// target (they follow the "Step target" radio via `step_cpu`).
fn active_cpu_suffix(state: &DebugState) -> String {
    if state.cpu_panels.len() > 1 {
        state
            .cpu_panels
            .get(state.step_cpu)
            .map(|p| format!(" — {}", p.name))
            .unwrap_or_default()
    } else {
        String::new()
    }
}

fn draw_breakpoints_panel(ui: &mut egui::Ui, state: &mut DebugState) {
    ui.separator();
    egui::CollapsingHeader::new(format!("Breakpoints{}", active_cpu_suffix(state)))
        .default_open(true)
        .show(ui, |ui| {
            // PC breakpoint entry (scoped to step_cpu)
            ui.horizontal(|ui| {
                if let Some(addr) =
                    entry_commit(ui, "PC $", &mut state.breakpoint_input, 48.0, "Add", |s| {
                        u32::from_str_radix(s.trim_start_matches('$'), 16).ok()
                    })
                {
                    if let Some(bp_set) = state.breakpoints.get_mut(state.step_cpu) {
                        bp_set.insert(addr);
                    }
                    state.breakpoint_input.clear();
                }
                if !hex_input_ok(&state.breakpoint_input) {
                    ui.colored_label(INVALID_HINT, "hex?");
                }
            });

            // List active PC breakpoints (sorted)
            if let Some(bp_set) = state.breakpoints.get(state.step_cpu) {
                let mut sorted: Vec<u32> = bp_set.iter().copied().collect();
                sorted.sort();
                let mut to_remove = None;
                for addr in &sorted {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("${}", fmt_addr(*addr))).monospace());
                        if ui.small_button("\u{2715}").clicked() {
                            to_remove = Some(*addr);
                        }
                    });
                }
                if let Some(addr) = to_remove {
                    state
                        .breakpoints
                        .get_mut(state.step_cpu)
                        .unwrap()
                        .remove(&addr);
                }
            }

            ui.add_space(4.0);

            // Cycle breakpoint
            ui.horizontal(|ui| {
                if let Some(cycle) =
                    entry_commit(ui, "Cycle:", &mut state.cycle_bp_input, 80.0, "Set", |s| {
                        s.trim().parse::<u64>().ok()
                    })
                {
                    state.cycle_breakpoint = Some(cycle);
                    state.cycle_bp_input.clear();
                }
            });

            if let Some(target) = state.cycle_breakpoint {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("Break @ cycle {}", target)).monospace());
                    if ui.small_button("\u{2715}").clicked() {
                        state.cycle_breakpoint = None;
                    }
                });
            }

            ui.add_space(4.0);

            // Frame breakpoint: pause at the start of the given frame.
            ui.horizontal(|ui| {
                if let Some(frame) =
                    entry_commit(ui, "Frame:", &mut state.frame_bp_input, 80.0, "Set", |s| {
                        s.trim().parse::<u64>().ok()
                    })
                {
                    state.frame_breakpoint = Some(frame);
                    state.frame_bp_input.clear();
                }
            });

            if let Some(target) = state.frame_breakpoint {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(format!("Break @ frame {}", target)).monospace());
                    if ui.small_button("\u{2715}").clicked() {
                        state.frame_breakpoint = None;
                    }
                });
            }
        });
}

// ---------------------------------------------------------------------------
// Watchpoints panel (controls column)
// ---------------------------------------------------------------------------

fn draw_watchpoints_panel(ui: &mut egui::Ui, state: &mut DebugState) {
    egui::CollapsingHeader::new(format!("Watchpoints{}", active_cpu_suffix(state)))
        .default_open(true)
        .show(ui, |ui| {
            // Watchpoint entry: address + R/W checkboxes + Add button
            ui.horizontal(|ui| {
                let enter = entry_field(ui, "$", &mut state.watchpoint_input, 48.0);
                ui.checkbox(&mut state.watchpoint_read, "R");
                ui.checkbox(&mut state.watchpoint_write, "W");
                // Guard the silent no-op: adding with neither R nor W selected
                // would collect an empty kind set and do nothing.
                let has_kind = state.watchpoint_read || state.watchpoint_write;
                let add = ui.add_enabled(has_kind, egui::Button::new("Add")).clicked();
                if (add || (enter && has_kind))
                    && let Ok(addr) =
                        u32::from_str_radix(state.watchpoint_input.trim_start_matches('$'), 16)
                {
                    let kinds: Vec<WatchpointKind> = [
                        state.watchpoint_read.then_some(WatchpointKind::Read),
                        state.watchpoint_write.then_some(WatchpointKind::Write),
                    ]
                    .into_iter()
                    .flatten()
                    .collect();
                    for kind in kinds {
                        let entry = (state.step_cpu, addr, kind);
                        if !state.watchpoints.contains(&entry) {
                            state.watchpoints.push(entry);
                            state.watchpoints_dirty = true;
                        }
                    }
                    state.watchpoint_input.clear();
                }
                if !hex_input_ok(&state.watchpoint_input) {
                    ui.colored_label(INVALID_HINT, "hex?");
                }
            });

            // List active watchpoints
            let mut to_remove = None;
            for (i, &(cpu_idx, addr, kind)) in state.watchpoints.iter().enumerate() {
                ui.horizontal(|ui| {
                    let kind_str = match kind {
                        WatchpointKind::Read => "R",
                        WatchpointKind::Write => "W",
                    };
                    let cpu_label = if state.cpu_panels.len() > 1 {
                        format!("[CPU{}] ", cpu_idx)
                    } else {
                        String::new()
                    };
                    ui.label(
                        egui::RichText::new(format!("{cpu_label}${} {kind_str}", fmt_addr(addr)))
                            .monospace(),
                    );
                    if ui.small_button("\u{2715}").clicked() {
                        to_remove = Some(i);
                    }
                });
            }
            if let Some(idx) = to_remove {
                state.watchpoints.remove(idx);
                state.watchpoints_dirty = true;
            }

            // Display last watchpoint hit with full attribution:
            // who accessed, what, where (region/device), and when (cycle/PC).
            if let Some(hit) = &state.last_watchpoint_hit {
                let kind_str = match hit.kind {
                    WatchpointKind::Read => "read",
                    WatchpointKind::Write => "write",
                };
                let source = format_access_source(hit.source);
                // pre: hit recorded before the write side effect;
                // post: after the read completed (value known).
                let phase_str = match hit.phase {
                    WatchpointPhase::Before => "pre",
                    WatchpointPhase::After => "post",
                };
                let value = fmt_hex_value(hit.value, hit.width);

                let hit_color = egui::Color32::from_rgb(255, 200, 80);
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(format!(
                        "{source} {kind_str} ${} = ${value} ({phase_str})",
                        fmt_addr(hit.addr)
                    ))
                    .monospace()
                    .color(hit_color),
                );

                let location = match (hit.region, hit.device) {
                    (Some(region), Some(device)) => Some(format!("{region} \u{2022} {device}")),
                    (Some(region), None) => Some(region.to_string()),
                    (None, Some(device)) => Some(device.to_string()),
                    (None, None) => None,
                };
                if let Some(location) = location {
                    ui.label(egui::RichText::new(location).monospace().color(hit_color));
                }

                let pc_str = hit
                    .pc
                    .map(|pc| format!("  PC ${}", fmt_addr(pc)))
                    .unwrap_or_default();
                ui.label(
                    egui::RichText::new(format!("cycle {}{pc_str}", hit.cycle))
                        .monospace()
                        .color(hit_color),
                );
            }
        });
}

// ---------------------------------------------------------------------------
// Event trace panel (controls column)
// ---------------------------------------------------------------------------

/// Format `source` for trace/watchpoint display ("CPU0", "DMA", device name).
fn format_access_source(source: DebugAccessSource) -> String {
    match source {
        DebugAccessSource::Cpu(i) => format!("CPU{i}"),
        DebugAccessSource::Dma => "DMA".to_string(),
        DebugAccessSource::Device(name) => name.to_string(),
        DebugAccessSource::Frontend => "frontend".to_string(),
        DebugAccessSource::Unknown => "?".to_string(),
    }
}

/// One-line rendering of a trace event:
/// `   123456 CPU0  bank   $C900=$03 PC $D042 ROM Bank — banked ROM mapped…`
fn format_trace_event(e: &DebugEvent) -> String {
    let mut line = format!(
        "{:>10} {:<5} {:<7}",
        e.cycle,
        format_access_source(e.source),
        e.kind.label()
    );
    if let Some(addr) = e.addr {
        line.push_str(&format!(" ${}", fmt_addr(addr)));
    }
    if let Some(value) = e.value {
        line.push_str(&format!("=${}", fmt_hex_value(value, e.width)));
    }
    if let Some(pc) = e.pc {
        line.push_str(&format!(" PC ${}", fmt_addr(pc)));
    }
    if let Some(location) = e.device.or(e.region) {
        line.push_str(&format!(" {location}"));
    }
    if let Some(detail) = e.detail {
        line.push_str(&format!(" \u{2014} {detail}"));
    }
    line
}

fn draw_event_trace_panel(ui: &mut egui::Ui, state: &mut DebugState) {
    egui::CollapsingHeader::new("Event Trace")
        .default_open(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.checkbox(&mut state.trace_enabled, "Record").changed() {
                    state.trace_enabled_dirty = true;
                }
                if ui.button("Clear").clicked() {
                    state.trace_clear_requested = true;
                }
                ui.label(format!("{}", state.trace_events.len()));
            });

            if state.trace_events.is_empty() {
                return;
            }

            // Virtualized list (the ring holds thousands of events),
            // pinned to the newest entries while recording.
            let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
            egui::ScrollArea::vertical()
                .id_salt("trace_scroll")
                .max_height(200.0)
                .stick_to_bottom(true)
                .show_rows(ui, row_height, state.trace_events.len(), |ui, row_range| {
                    for event in &state.trace_events[row_range] {
                        ui.label(egui::RichText::new(format_trace_event(event)).monospace());
                    }
                });
        });
}

// ---------------------------------------------------------------------------
// Disassembly panel (per-CPU column)
// ---------------------------------------------------------------------------

/// Disassemble `count` instructions starting at `start_addr`.
fn disassemble_from(
    bus: &dyn BusDebug,
    cpu_index: usize,
    cpu: &dyn DebugCpu,
    start_addr: u32,
    count: usize,
) -> Vec<(u32, Vec<u8>, String)> {
    let mut result = Vec::with_capacity(count);
    let mut addr = start_addr;
    for _ in 0..count {
        let mut bytes = [0u8; 10];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = bus
                .read(cpu_index, addr.wrapping_add(i as u32))
                .unwrap_or(0);
        }
        let insn = cpu.debug_disassemble(addr, &bytes);
        let text = format!("{insn}");
        let raw = bytes[..insn.byte_len as usize].to_vec();
        result.push((addr, raw, text));
        addr = addr.wrapping_add(u32::from(insn.byte_len));
    }
    result
}

/// Disassemble a window around `pc`. Returns (lines, index_of_pc_line).
fn disassemble_around_pc(
    bus: &dyn BusDebug,
    cpu_index: usize,
    cpu: &dyn DebugCpu,
    pc: u32,
    before: usize,
    after: usize,
) -> (Vec<(u32, Vec<u8>, String)>, usize) {
    // Try scanning from several start points before PC to find one that aligns
    let max_instr = before + after + 40;
    for offset in (48u32..=64).rev() {
        // Saturating: near address 0 the scan just starts at 0 rather
        // than wrapping to the top of a 4 GB space.
        let scan_start = pc.saturating_sub(offset);
        let all = disassemble_from(bus, cpu_index, cpu, scan_start, max_instr);
        if let Some(pc_idx) = all.iter().position(|(addr, _, _)| *addr == pc) {
            let start = pc_idx.saturating_sub(before);
            let end = (pc_idx + after + 1).min(all.len());
            let slice = all[start..end].to_vec();
            let pc_offset = pc_idx - start;
            return (slice, pc_offset);
        }
    }
    // Fallback: just disassemble forward from PC
    let forward = disassemble_from(bus, cpu_index, cpu, pc, after + 1);
    (forward, 0)
}

fn draw_disassembly_panel(
    ui: &mut egui::Ui,
    state: &mut DebugState,
    bus: &dyn BusDebug,
    cpu_idx: usize,
) {
    let cpus = bus.cpus();
    if cpu_idx >= cpus.len() {
        ui.label("No CPU available");
        return;
    }
    let (_name, cpu) = &cpus[cpu_idx];
    let pc = cpu.debug_pc();

    let (lines, pc_idx) = disassemble_around_pc(bus, cpu_idx, *cpu, pc, 8, 16);

    egui::ScrollArea::vertical()
        .id_salt(format!("disasm_{cpu_idx}"))
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            for (i, (addr, raw_bytes, text)) in lines.iter().enumerate() {
                let is_pc = i == pc_idx;
                let is_bp = state
                    .breakpoints
                    .get(cpu_idx)
                    .is_some_and(|bp| bp.contains(addr));

                let bp_marker = if is_bp { "\u{25CF} " } else { "  " };
                let hex = hex_bytes(raw_bytes);
                let line_text = format!("{bp_marker}{}  {hex:<12} {text}", fmt_addr(*addr));

                let mut label = egui::RichText::new(line_text).monospace();
                if is_pc {
                    label = label
                        .background_color(egui::Color32::from_rgb(60, 60, 120))
                        .color(egui::Color32::WHITE);
                } else if is_bp {
                    label = label.color(egui::Color32::from_rgb(255, 80, 80));
                }

                if ui
                    .add(egui::Label::new(label).sense(egui::Sense::click()))
                    .clicked()
                    && let Some(bp_set) = state.breakpoints.get_mut(cpu_idx)
                {
                    if bp_set.contains(addr) {
                        bp_set.remove(addr);
                    } else {
                        bp_set.insert(*addr);
                    }
                }
            }
        });
}

// ---------------------------------------------------------------------------
// Memory viewer panel (per-CPU column)
// ---------------------------------------------------------------------------

fn draw_memory_panel(
    ui: &mut egui::Ui,
    state: &mut DebugState,
    bus: &dyn BusDebug,
    cpu_idx: usize,
) {
    let row_height = ui.text_style_height(&egui::TextStyle::Monospace) + 2.0;

    // Navigation bar
    if cpu_idx < state.memory_addr_inputs.len() {
        ui.horizontal(|ui| {
            // Unlike the breakpoint/watchpoint fields, this one keeps its text
            // after a jump so the address stays visible.
            if let Some(addr) = entry_commit(
                ui,
                "$",
                &mut state.memory_addr_inputs[cpu_idx],
                48.0,
                "Go",
                |s| u32::from_str_radix(s.trim_start_matches('$'), 16).ok(),
            ) {
                // Retarget the 64 KB window to the address's high bits and
                // scroll to its row within the window.
                state.memory_view_base[cpu_idx] = addr & 0xFFFF_0000;
                let target_row = (addr & 0xFFFF) >> 4;
                state.memory_scroll_to[cpu_idx] = Some(target_row as f32 * row_height);
            }
        });
    }

    ui.separator();

    // Hex dump with virtual scrolling: a 64 KB window starting at the
    // CPU's view base (always 0 for 16-bit machines, so the window is the
    // whole address space).
    let view_base = state.memory_view_base.get(cpu_idx).copied().unwrap_or(0);
    let total_rows: usize = 4096;

    let mut scroll = egui::ScrollArea::vertical()
        .id_salt(format!("mem_{cpu_idx}"))
        .auto_shrink([false; 2]);

    // Apply pending scroll-to (from Go button), then clear it
    if let Some(offset) = state
        .memory_scroll_to
        .get_mut(cpu_idx)
        .and_then(|s| s.take())
    {
        scroll = scroll.vertical_scroll_offset(offset);
    }

    scroll.show_rows(ui, row_height, total_rows, |ui, row_range| {
        for row in row_range {
            let base_addr = view_base + (row as u32) * 16;
            let mut hex_part = String::with_capacity(52);
            let mut ascii_part = String::with_capacity(16);

            for col in 0..16u32 {
                let addr = base_addr + col;
                if col == 8 {
                    hex_part.push(' ');
                }
                // Label non-backed cells instead of showing a fake bus
                // value: `--` = mapped I/O, `..` = unmapped.
                match bus.peek(cpu_idx, addr) {
                    DebugRead::Backed { value, .. } => {
                        let byte = value as u8;
                        hex_part.push_str(&format!("{byte:02X} "));
                        ascii_part.push(if byte.is_ascii_graphic() || byte == b' ' {
                            byte as char
                        } else {
                            '.'
                        });
                    }
                    DebugRead::Io => {
                        hex_part.push_str("-- ");
                        ascii_part.push('-');
                    }
                    DebugRead::Unmapped => {
                        hex_part.push_str(".. ");
                        ascii_part.push(' ');
                    }
                }
            }

            let line = format!("{}  {} |{}|", fmt_addr(base_addr), hex_part, ascii_part);
            ui.label(egui::RichText::new(line).monospace());
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_hex_value_pads_to_the_access_width() {
        // Width is in BYTES: 1 -> 2 digits, 2 -> 4, 4 -> 8.
        assert_eq!(fmt_hex_value(0x5u32, 1), "05");
        assert_eq!(fmt_hex_value(0x5u32, 2), "0005");
        assert_eq!(fmt_hex_value(0x5u32, 4), "00000005");
        assert_eq!(fmt_hex_value(0xDEADBEEFu32, 4), "DEADBEEF");
        // A value wider than its stated width is never truncated, only
        // under-padded — the format width is a minimum.
        assert_eq!(fmt_hex_value(0x1234u32, 1), "1234");
        // Unexpected widths (0, 3, 8) fall back to the byte form rather than
        // to a bare {:X}, so a column never loses its leading zeroes.
        assert_eq!(fmt_hex_value(0x7u32, 0), "07");
        assert_eq!(fmt_hex_value(0x7u32, 3), "07");
    }

    /// The register grid keys on a *bit* width and divides by 8 before
    /// formatting. 32-bit registers (M68000 PC/D/A/USP/SSP, i8088 CS:IP) used
    /// to fall through to a bare `{:X}` and render unpadded; they are now
    /// zero-padded to 8 digits like every other width.
    #[test]
    fn register_bit_width_maps_to_a_padded_byte_width() {
        let render = |value: u64, bit_width: u8| fmt_hex_value(value, bit_width / 8);
        assert_eq!(render(0x5, 8), "05");
        assert_eq!(render(0x5, 16), "0005");
        assert_eq!(render(0x5, 32), "00000005");
        assert_eq!(render(0x1A2B3C, 32), "001A2B3C");
    }

    #[test]
    fn hex_input_ok_flags_only_nonempty_garbage() {
        // Empty / whitespace: no hint (nothing typed yet).
        assert!(hex_input_ok(""));
        assert!(hex_input_ok("   "));
        // Valid hex, with or without the `$` prefix the field trims.
        assert!(hex_input_ok("1BCC"));
        assert!(hex_input_ok("$87cf"));
        assert!(hex_input_ok("ffff"));
        // Non-empty and not hex → not ok → the "hex?" hint shows.
        assert!(!hex_input_ok("xyz"));
        assert!(!hex_input_ok("$zz"));
        assert!(!hex_input_ok("12g4"));
    }

    fn panel(name: &str) -> CpuPanel {
        CpuPanel {
            name: name.to_string(),
            registers: Vec::new(),
        }
    }

    #[test]
    fn active_cpu_suffix_only_labels_multicpu() {
        let mut s = DebugState::new();
        // Single CPU: no suffix (which CPU is unambiguous).
        s.cpu_panels.push(panel("M6809"));
        assert_eq!(active_cpu_suffix(&s), "");

        // Multi-CPU: the panels name the active step-CPU, so it's clear which
        // CPU's address space breakpoints/watchpoints target.
        s.cpu_panels.push(panel("Z80 Sound"));
        s.step_cpu = 0;
        assert_eq!(active_cpu_suffix(&s), " — M6809");
        s.step_cpu = 1;
        assert_eq!(active_cpu_suffix(&s), " — Z80 Sound");
    }
}
