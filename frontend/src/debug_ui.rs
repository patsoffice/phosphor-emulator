use std::collections::{HashSet, VecDeque};

use phosphor_core::core::debug::{BusDebug, DebugCpu, DebugRegister};
use phosphor_core::core::debug_trace::{
    DebugEvent, DebugEventKind, EventFilter, SourceFilter, parse_addr_range,
};
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

/// Width of the leftmost (controls) column, which holds fixed-size widgets
/// rather than a listing and so has nothing to size itself to.
pub const CONTROLS_COLUMN_WIDTH: f32 = 260.0;

/// Floor for a CPU column: the register grid and tab row need this much even
/// when the listing below them is narrow.
pub const MIN_CPU_COLUMN: f32 = 260.0;

/// Pixels a listing column needs beyond its glyphs — the scroll area's vertical
/// scrollbar plus the column's own item spacing and margins.
const COLUMN_CHROME: f32 = 30.0;

/// Fallback monospace glyph advance, used for the first frame's layout before
/// the real one has been measured from the live font.
const DEFAULT_CHAR_WIDTH: f32 = 7.0;

/// How many watchpoint hits the UI keeps before dropping the oldest.
///
/// The machine's own queue is shallow (it is drained every frame or step), so
/// without a history here every hit but the newest was lost the moment the next
/// one landed. Bounded because a watchpoint on a hot address fires thousands of
/// times a second.
pub const WATCHPOINT_HISTORY: usize = 256;

/// A memory byte edited in the memory viewer, applied on the next frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MemoryWrite {
    /// CPU whose address space to write into.
    pub cpu_index: usize,
    pub addr: u32,
    pub value: u8,
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
    /// Global pause, independent of the debug UI. When the debug UI is inactive
    /// this gates emulation in [`execute_frame`]; while the debug UI is active
    /// it is ignored and `run_mode` governs instead.
    pub global_paused: bool,
    /// Let exactly one frame through the global pause, then re-hold.
    ///
    /// [`execute_frame`] consumes this on every call, whether or not it is in a
    /// position to honour it. Clearing it only on the path that uses it would
    /// let a request made while the debug panel governs sit there and spend
    /// itself later, jumping a frame the moment the panel closed.
    pub global_step: bool,
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
    /// Recent watchpoint hits, oldest first, capped at [`WATCHPOINT_HISTORY`].
    ///
    /// Every pending hit is drained into here on each break/step, so a burst
    /// within one cycle is kept whole rather than reduced to its first hit.
    pub watchpoint_hits: VecDeque<WatchpointHit>,
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
    /// Display filter for the trace list — the same [`EventFilter`] the CLI's
    /// `--events` builds. Purely a view: the ring keeps recording everything,
    /// so widening the filter reveals events already captured.
    pub trace_filter: EventFilter,
    /// Address-range input buffer for the trace filter (`$1000-$1FFF`).
    pub trace_addr_input: String,

    // Device controls
    /// Device actions requested by the UI, applied on the next frame.
    pub pending_device_actions: Vec<DeviceAction>,
    /// Per-device-panel (offset, value) hex input buffers for register writes.
    pub device_write_inputs: Vec<(String, String)>,
    /// Memory bytes the user edited, applied on the next frame as debug pokes.
    /// Deferred for the same reason device actions are: panel drawing holds no
    /// mutable machine borrow.
    pub pending_memory_writes: Vec<MemoryWrite>,

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
    /// Cell edit in progress per CPU: the address being typed over and its
    /// input buffer. At most one cell per column is editable at a time.
    pub memory_edit: Vec<Option<(u32, String)>>,
    /// Top address of each CPU's disassembly view. `None` until the first
    /// draw anchors it. See [`draw_disassembly_panel`] for why the view is
    /// anchored rather than recentred on PC every frame.
    pub disasm_anchor: Vec<Option<u32>>,
    /// Whether each CPU's disassembly view re-anchors to keep PC on screen.
    pub disasm_follow: Vec<bool>,

    // Layout alignment
    /// Max top-section height from the previous frame (controls/registers).
    /// Used to align the disassembly/memory separator across all columns.
    pub top_section_height: f32,
    /// Widest line, in monospace characters, each CPU column drew on the
    /// previous frame. Columns are sized from this — measure-then-apply on the
    /// next frame, the same one-frame-late trick `top_section_height` uses,
    /// because a column's width has to be chosen before its content is drawn.
    pub column_chars: Vec<usize>,
    /// Monospace glyph advance, measured from the live font each frame so the
    /// column arithmetic follows the user's font scale rather than assuming it.
    pub mono_char_width: f32,

    /// Key names to print on the run/step buttons, refreshed from the live host
    /// bindings each frame. Held as strings so this module stays free of SDL
    /// types — and so a rebound step key relabels its button instead of the
    /// button advertising a key that no longer works.
    pub key_hints: StepKeyHints,
}

/// Key captions for the run/step buttons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepKeyHints {
    pub pause: String,
    pub step_cycle: String,
    pub step_instruction: String,
    pub step_frame: String,
}

impl Default for StepKeyHints {
    /// The factory keys, so the buttons read sensibly before the emulator loop
    /// has pushed the live bindings in (and in tests).
    fn default() -> Self {
        Self {
            pause: "0".to_string(),
            step_cycle: "7".to_string(),
            step_instruction: "8".to_string(),
            step_frame: "9".to_string(),
        }
    }
}

impl DebugState {
    pub fn new() -> Self {
        Self {
            active: false,
            run_mode: RunMode::Running,
            global_paused: false,
            global_step: false,
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
            watchpoint_hits: VecDeque::new(),
            watchpoints_dirty: false,
            trace_enabled: false,
            trace_enabled_dirty: false,
            trace_clear_requested: false,
            trace_events: Vec::new(),
            trace_filter: EventFilter::all(),
            trace_addr_input: String::new(),
            pending_device_actions: Vec::new(),
            device_write_inputs: Vec::new(),
            pending_memory_writes: Vec::new(),
            bottom_tabs: Vec::new(),
            memory_addr_inputs: Vec::new(),
            memory_scroll_to: Vec::new(),
            memory_view_base: Vec::new(),
            memory_edit: Vec::new(),
            disasm_anchor: Vec::new(),
            disasm_follow: Vec::new(),
            top_section_height: 0.0,
            column_chars: Vec::new(),
            mono_char_width: DEFAULT_CHAR_WIDTH,
            key_hints: StepKeyHints::default(),
        }
    }

    /// Record a watchpoint hit, dropping the oldest past [`WATCHPOINT_HISTORY`].
    pub fn push_watchpoint_hit(&mut self, hit: WatchpointHit) {
        if self.watchpoint_hits.len() >= WATCHPOINT_HISTORY {
            self.watchpoint_hits.pop_front();
        }
        self.watchpoint_hits.push_back(hit);
    }

    /// The most recent watchpoint hit — the one that caused the current break.
    pub fn last_watchpoint_hit(&self) -> Option<&WatchpointHit> {
        self.watchpoint_hits.back()
    }

    /// True if any PC, cycle, or memory watchpoint is set.
    pub fn has_any_breakpoints(&self) -> bool {
        self.cycle_breakpoint.is_some()
            || self.breakpoints.iter().any(|s| !s.is_empty())
            || !self.watchpoints.is_empty()
    }

    /// Width a CPU column needs to show its selected tab's widest line whole.
    ///
    /// Driven by what the column actually drew last frame, so a Memory tab (a
    /// 16-byte row plus its ASCII gutter, ~74 monospace columns) gets roughly
    /// twice the width of a Disassembly tab, and neither is padded to the
    /// other's size. Falls back to [`MIN_CPU_COLUMN`] before the first draw and
    /// whenever the content is narrower than the registers above it.
    pub fn cpu_column_width(&self, cpu_idx: usize) -> f32 {
        let chars = self.column_chars.get(cpu_idx).copied().unwrap_or(0) as f32;
        (chars * self.mono_char_width + COLUMN_CHROME).max(MIN_CPU_COLUMN)
    }

    /// Width (in pixels) needed for the whole debug panel: the controls column
    /// plus each CPU column at the width its selected tab needs.
    pub fn debug_panel_width(&self) -> u32 {
        let cpu_total: f32 = (0..self.cpu_panels.len().max(1))
            .map(|i| self.cpu_column_width(i))
            .sum();
        (CONTROLS_COLUMN_WIDTH + cpu_total).ceil() as u32
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
        while self.memory_edit.len() < cpus.len() {
            self.memory_edit.push(None);
        }
        while self.disasm_anchor.len() < cpus.len() {
            self.disasm_anchor.push(None);
        }
        while self.disasm_follow.len() < cpus.len() {
            self.disasm_follow.push(true);
        }
        while self.column_chars.len() < cpus.len() {
            self.column_chars.push(0);
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

/// Move every pending watchpoint hit from the machine into the UI's history.
/// Returns true if at least one hit was taken (the caller's cue to pause).
///
/// Drains rather than taking one: a single cycle can queue several hits (two
/// watchpoints on one address, a word access spanning two watched bytes), and
/// leaving the rest queued would surface them later attributed to whatever
/// cycle happened to drain them next.
fn drain_watchpoint_hits(machine: &mut dyn FrontendMachine, state: &mut DebugState) -> bool {
    let mut any = false;
    while let Some(hit) = machine.take_watchpoint_hit() {
        state.push_watchpoint_hit(hit);
        any = true;
    }
    any
}

/// Execute one frame of emulation according to the current run mode.
/// Returns true if a full frame was executed (caller should drain audio).
pub fn execute_frame(machine: &mut dyn FrontendMachine, state: &mut DebugState) -> bool {
    // Taken unconditionally: see `DebugState::global_step`.
    let step_once = std::mem::take(&mut state.global_step);

    if !state.active {
        // Global pause: hold the machine without running a frame, so no audio is
        // drained. The audio callback repeats its last sample, so the output
        // stays silent rather than buzzing.
        if state.global_paused && !step_once {
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

    // Apply device controls and memory edits requested by the UI.
    if !state.pending_device_actions.is_empty() || !state.pending_memory_writes.is_empty() {
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
            // `poke`, not `write`: a memory-viewer edit is a debugger write and
            // is tagged `DebugAccessSource::Frontend`, so it shows up in the
            // event trace as a frontend poke instead of masquerading as a
            // hardware store.
            for w in &state.pending_memory_writes {
                bus.poke(w.cpu_index, w.addr, w.value);
            }
        }
        state.pending_device_actions.clear();
        state.pending_memory_writes.clear();
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
                        if drain_watchpoint_hits(machine, state) {
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
                drain_watchpoint_hits(machine, state);
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
            drain_watchpoint_hits(machine, state);
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
                    drain_watchpoint_hits(machine, state);
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

    // Right panel: multi-column debug layout, each CPU column sized to the tab
    // it is showing rather than to an equal share of the panel. `ui.columns`
    // would force a Memory tab (~74 monospace columns) and a Disassembly tab
    // (~35) to the same width, so one of them always ends up scrolling
    // sideways. Widths come from what each column drew last frame.
    egui::SidePanel::right("debug_panel")
        .exact_width(state.debug_panel_width() as f32)
        .show(ctx, |ui| {
            // Measure the live monospace advance; the column arithmetic is in
            // characters, so this is what converts it to pixels. Laid out over
            // ten glyphs and divided, so per-glyph rounding does not compound
            // across a 74-column memory row.
            let font = egui::TextStyle::Monospace.resolve(ui.style());
            let sample = ui.painter().layout_no_wrap(
                "0000000000".to_string(),
                font,
                egui::Color32::PLACEHOLDER,
            );
            state.mono_char_width = sample.size().x / 10.0;

            if n_cpus > 0 {
                // Use the previous frame's max top-section height for alignment
                let min_h = state.top_section_height;
                let full_height = ui.available_height();
                let layout = egui::Layout::top_down(egui::Align::Min);
                let mut max_h: f32 = 0.0;
                ui.horizontal_top(|ui| {
                    let size = egui::vec2(CONTROLS_COLUMN_WIDTH, full_height);
                    max_h = ui
                        .allocate_ui_with_layout(size, layout, |ui| {
                            ui.set_min_width(CONTROLS_COLUMN_WIDTH);
                            draw_controls_column(ui, state, min_h)
                        })
                        .inner;

                    for cpu_idx in 0..n_cpus {
                        let width = state.cpu_column_width(cpu_idx);
                        let h = ui
                            .allocate_ui_with_layout(egui::vec2(width, full_height), layout, |ui| {
                                ui.set_min_width(width);
                                draw_cpu_column(ui, state, bus, cpu_idx, min_h)
                            })
                            .inner;
                        max_h = max_h.max(h);
                    }
                });
                state.top_section_height = max_h;
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

    let keys = state.key_hints.clone();

    ui.horizontal(|ui| {
        // The pause key toggles run <-> pause (see emulator.rs).
        if state.run_mode == RunMode::Running {
            if ui.button(format!("Pause ({})", keys.pause)).clicked() {
                state.run_mode = RunMode::Paused;
            }
        } else if ui.button(format!("Continue ({})", keys.pause)).clicked() {
            // The hit history deliberately survives a resume — see
            // `draw_watchpoint_hits`.
            state.run_mode = RunMode::Running;
        }
    });

    // Ordered by increasing granularity, matching the key row (see
    // `host_keys::DEFAULTS`).
    ui.horizontal(|ui| {
        let step = |ui: &mut egui::Ui, label: String| {
            ui.add_enabled(is_paused, egui::Button::new(label))
                .clicked()
        };
        if step(ui, format!("Cycle ({})", keys.step_cycle)) {
            state.run_mode = RunMode::StepCycle;
        }
        if step(ui, format!("Instr ({})", keys.step_instruction)) {
            state.run_mode = RunMode::StepInstruction;
        }
        if step(ui, format!("Frame ({})", keys.step_frame)) {
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

            draw_watchpoint_hits(ui, state);
        });
}

/// Colour for watchpoint-hit text (the newest hit; older ones are dimmed).
const HIT_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 200, 80);

/// The newest hit in full — who accessed, what, where (region/device), and when
/// (cycle/PC) — followed by the older hits one line each, newest first.
///
/// The history is not cleared on Continue: the point of keeping it is to see a
/// sequence of hits build up across several resumes. "Clear" empties it.
fn draw_watchpoint_hits(ui: &mut egui::Ui, state: &mut DebugState) {
    let Some(hit) = state.last_watchpoint_hit().copied() else {
        return;
    };

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

    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(format!(
            "{source} {kind_str} ${} = ${value} ({phase_str})",
            fmt_addr(hit.addr)
        ))
        .monospace()
        .color(HIT_COLOR),
    );

    let location = match (hit.region, hit.device) {
        (Some(region), Some(device)) => Some(format!("{region} \u{2022} {device}")),
        (Some(region), None) => Some(region.to_string()),
        (None, Some(device)) => Some(device.to_string()),
        (None, None) => None,
    };
    if let Some(location) = location {
        ui.label(egui::RichText::new(location).monospace().color(HIT_COLOR));
    }

    let pc_str = hit
        .pc
        .map(|pc| format!("  PC ${}", fmt_addr(pc)))
        .unwrap_or_default();
    ui.label(
        egui::RichText::new(format!("cycle {}{pc_str}", hit.cycle))
            .monospace()
            .color(HIT_COLOR),
    );

    // Older hits. The machine's queue is drained every frame/step, so this is
    // the only place they survive.
    let total = state.watchpoint_hits.len();
    ui.horizontal(|ui| {
        let capped = if total >= WATCHPOINT_HISTORY { "+" } else { "" };
        ui.label(format!("History: {total}{capped}"));
        if ui.button("Clear").clicked() {
            state.watchpoint_hits.clear();
        }
    });
    if total < 2 {
        return;
    }

    let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
    // Newest first, and the newest is already shown in full above.
    let older: Vec<&WatchpointHit> = state.watchpoint_hits.iter().rev().skip(1).collect();
    egui::ScrollArea::vertical()
        .id_salt("wp_history")
        .max_height(120.0)
        .show_rows(ui, row_height, older.len(), |ui, row_range| {
            for hit in &older[row_range] {
                ui.label(
                    egui::RichText::new(format_watchpoint_hit(hit))
                        .monospace()
                        .weak(),
                );
            }
        });
}

/// One-line rendering of a past watchpoint hit:
/// `  12694104 CPU0  wr $87CF=$32 PC $0066 sharedram`
fn format_watchpoint_hit(hit: &WatchpointHit) -> String {
    let kind = match hit.kind {
        WatchpointKind::Read => "rd",
        WatchpointKind::Write => "wr",
    };
    let mut line = format!(
        "{:>10} {:<5} {kind} ${}=${}",
        hit.cycle,
        format_access_source(hit.source),
        fmt_addr(hit.addr),
        fmt_hex_value(hit.value, hit.width),
    );
    if let Some(pc) = hit.pc {
        line.push_str(&format!(" PC ${}", fmt_addr(pc)));
    }
    if let Some(location) = hit.device.or(hit.region) {
        line.push_str(&format!(" {location}"));
    }
    line
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
            });

            draw_trace_filter(ui, state);

            // The filter is a view, so the ring's own length is still worth
            // showing: "shown/recorded" makes it obvious when a filter is
            // hiding events rather than none having been captured.
            let shown: Vec<usize> = state
                .trace_events
                .iter()
                .enumerate()
                .filter(|(_, e)| state.trace_filter.accepts(e))
                .map(|(i, _)| i)
                .collect();
            ui.label(format!(
                "{} / {} events",
                shown.len(),
                state.trace_events.len()
            ));

            if shown.is_empty() {
                return;
            }

            // Virtualized list (the ring holds thousands of events),
            // pinned to the newest entries while recording.
            let row_height = ui.text_style_height(&egui::TextStyle::Monospace);
            egui::ScrollArea::vertical()
                .id_salt("trace_scroll")
                .max_height(200.0)
                .stick_to_bottom(true)
                .show_rows(ui, row_height, shown.len(), |ui, row_range| {
                    for &i in &shown[row_range] {
                        let text = format_trace_event(&state.trace_events[i]);
                        ui.label(egui::RichText::new(text).monospace());
                    }
                });
        });
}

/// Filter controls for the event trace: source, address range, and kinds.
///
/// Filtering happens here rather than at record time so it is non-destructive —
/// narrowing to one kind and widening again shows the events all along, no
/// re-run needed. The predicates are `phosphor-core`'s [`EventFilter`], the same
/// type `disasm trace --events` builds, so a kind is called the same thing here
/// as on the command line.
fn draw_trace_filter(ui: &mut egui::Ui, state: &mut DebugState) {
    let n_cpus = state.cpu_panels.len();
    let header = if state.trace_filter.is_unfiltered() {
        "Filter".to_string()
    } else {
        "Filter (active)".to_string()
    };
    egui::CollapsingHeader::new(header)
        .id_salt("trace_filter")
        .default_open(false)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("Source:");
                egui::ComboBox::from_id_salt("trace_source")
                    .selected_text(state.trace_filter.source.label())
                    .show_ui(ui, |ui| {
                        let mut option = |ui: &mut egui::Ui, value: SourceFilter| {
                            let label = value.label();
                            ui.selectable_value(&mut state.trace_filter.source, value, label);
                        };
                        option(ui, SourceFilter::Any);
                        for cpu in 0..n_cpus {
                            option(ui, SourceFilter::Cpu(cpu));
                        }
                        option(ui, SourceFilter::Dma);
                        option(ui, SourceFilter::Device);
                        option(ui, SourceFilter::Frontend);
                    });
            });

            // Address range: `$1234` or `$1000-$1FFF`. Empty clears the filter,
            // so there is no separate "off" button to forget about.
            ui.horizontal(|ui| {
                let commit = entry_field(ui, "Addr $", &mut state.trace_addr_input, 88.0);
                let apply = ui.button("Set").clicked() || commit;
                if apply {
                    state.trace_filter.addr = if state.trace_addr_input.trim().is_empty() {
                        None
                    } else {
                        parse_addr_range(&state.trace_addr_input).ok()
                    };
                }
                if let Some((lo, hi)) = state.trace_filter.addr {
                    let text = if lo == hi {
                        format!("${}", fmt_addr(lo))
                    } else {
                        format!("${}-${}", fmt_addr(lo), fmt_addr(hi))
                    };
                    ui.label(egui::RichText::new(text).monospace());
                }
            });
            if !state.trace_addr_input.trim().is_empty()
                && parse_addr_range(&state.trace_addr_input).is_err()
            {
                ui.colored_label(INVALID_HINT, "hex, or $LO-$HI?");
            }

            ui.horizontal(|ui| {
                ui.label("Kinds:");
                if ui.small_button("All").clicked() {
                    state.trace_filter.select_all_kinds();
                }
                if ui.small_button("None").clicked() {
                    state.trace_filter.select_no_kinds();
                }
            });
            // Two columns: 17 kinds in one column would push the device list
            // off the bottom of the controls column.
            egui::Grid::new("trace_kinds")
                .num_columns(2)
                .show(ui, |ui| {
                    for pair in DebugEventKind::ALL.chunks(2) {
                        for kind in pair {
                            let mut on = state.trace_filter.accepts_kind(*kind);
                            if ui.checkbox(&mut on, kind.label()).changed() {
                                state.trace_filter.set_kind(*kind, on);
                            }
                        }
                        ui.end_row();
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

/// Instructions listed from the anchor. Generous enough that stepping stays
/// inside the window (and so keeps the view still) for a long run of
/// instructions before it has to re-anchor.
const DISASM_ROWS: usize = 48;

/// How many instructions of lead-in to keep above PC when re-anchoring, so a
/// jump lands PC just below the top rather than flush against it.
const DISASM_LEAD_IN: usize = 4;

/// Draw a CPU's disassembly, listed from a sticky anchor address.
///
/// The view is **anchored**, not recentred. Disassembling around PC every frame
/// made the listing jump on every step: each step shifted every line by one row
/// while the scroll offset stayed put, so the instruction under the cursor moved
/// even though the user had not scrolled. Instead the window is listed from
/// `disasm_anchor` and only re-anchors when PC leaves it — so stepping moves the
/// highlight, not the text, and the listing shifts once per window rather than
/// once per instruction.
///
/// "Follow PC" off pins the anchor entirely, for reading a routine while the
/// machine runs elsewhere; "Go to PC" re-anchors on demand.
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

    let mut follow = state.disasm_follow.get(cpu_idx).copied().unwrap_or(true);
    let mut recenter = false;
    ui.horizontal(|ui| {
        if ui
            .checkbox(&mut follow, "Follow PC")
            .on_hover_text("Re-anchor the listing when PC leaves the window")
            .changed()
            && follow
        {
            // Turning following back on should show where execution actually
            // is, not wherever the pinned view was left.
            recenter = true;
        }
        // ASCII label: egui's default font has no U+2192, so an arrow here
        // renders as a missing-glyph box.
        if ui
            .button("Go to PC")
            .on_hover_text("Anchor the listing at the current PC")
            .clicked()
        {
            recenter = true;
        }
    });
    if let Some(slot) = state.disasm_follow.get_mut(cpu_idx) {
        *slot = follow;
    }

    // Re-anchor when asked, when there is no anchor yet, or when following and
    // PC has left the listed window.
    let anchor = state.disasm_anchor.get(cpu_idx).copied().flatten();
    let mut lines = match anchor {
        Some(anchor) if !recenter => disassemble_from(bus, cpu_idx, *cpu, anchor, DISASM_ROWS),
        _ => Vec::new(),
    };
    let mut pc_idx = lines.iter().position(|(addr, _, _)| *addr == pc);
    if lines.is_empty() || (follow && pc_idx.is_none()) {
        let (window, idx) =
            disassemble_around_pc(bus, cpu_idx, *cpu, pc, DISASM_LEAD_IN, DISASM_ROWS);
        if let Some(slot) = state.disasm_anchor.get_mut(cpu_idx) {
            *slot = window.first().map(|(addr, _, _)| *addr);
        }
        pc_idx = Some(idx);
        lines = window;
    }

    // Pad the byte column to the widest instruction actually listed rather than
    // to a fixed 12 columns. A window of short instructions otherwise carries
    // several columns of dead space between the bytes and the mnemonic — on a
    // 260px CPU column that was enough to push the operand onto a second row.
    let hex: Vec<String> = lines.iter().map(|(_, raw, _)| hex_bytes(raw)).collect();
    let hex_width = hex.iter().map(String::len).max().unwrap_or(0);

    // Report the widest line so the column can be sized to it next frame.
    // Measured on the formatted line, so an M68000's long operands widen the
    // column exactly as much as they need and a Z80's do not.
    let widest = lines
        .iter()
        .enumerate()
        .map(|(i, (addr, _, text))| {
            2 + fmt_addr(*addr).len() + 2 + hex_width.max(hex[i].len()) + 1 + text.chars().count()
        })
        .max()
        .unwrap_or(0);
    if let Some(slot) = state.column_chars.get_mut(cpu_idx) {
        *slot = widest;
    }

    // One row per instruction, always: wrapped lines made the listing ragged and
    // gave instructions inconsistent heights, which defeats the point of holding
    // the view still while stepping. Anything past the column's width is reached
    // by scrolling sideways instead.
    egui::ScrollArea::both()
        .id_salt(format!("disasm_{cpu_idx}"))
        .auto_shrink([false; 2])
        .show(ui, |ui| {
            for (i, (addr, _raw_bytes, text)) in lines.iter().enumerate() {
                let is_pc = pc_idx == Some(i);
                let is_bp = state
                    .breakpoints
                    .get(cpu_idx)
                    .is_some_and(|bp| bp.contains(addr));

                let bp_marker = if is_bp { "\u{25CF} " } else { "  " };
                let bytes = &hex[i];
                let line_text =
                    format!("{bp_marker}{}  {bytes:<hex_width$} {text}", fmt_addr(*addr));

                let mut label = egui::RichText::new(line_text).monospace();
                if is_pc {
                    label = label
                        .background_color(egui::Color32::from_rgb(60, 60, 120))
                        .color(egui::Color32::WHITE);
                } else if is_bp {
                    label = label.color(egui::Color32::from_rgb(255, 80, 80));
                }

                if ui
                    .add(
                        egui::Label::new(label)
                            .wrap_mode(egui::TextWrapMode::Extend)
                            .sense(egui::Sense::click()),
                    )
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

    // Report the row width so the column can be sized to it next frame:
    // address, two spaces, 16 `XX ` cells, the mid-row gap, and ` |ascii|`.
    let addr_chars = fmt_addr(view_base + (total_rows as u32 - 1) * 16).len();
    if let Some(slot) = state.column_chars.get_mut(cpu_idx) {
        *slot = addr_chars + 2 + 16 * 3 + 1 + 2 + 16 + 1;
    }

    // Scrolls in BOTH directions, and that is load-bearing rather than a
    // convenience: a 16-byte row plus its ASCII gutter is far wider than a
    // ~260px CPU column, and the row is now built from per-cell widgets in a
    // `horizontal` layout, which neither wraps nor clips. Without a horizontal
    // scroll area to clip it, the row overruns the neighbouring CPU column and
    // widens the whole debug panel enough to squeeze the game view off screen.
    let mut scroll = egui::ScrollArea::both()
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
            draw_memory_row(ui, state, bus, cpu_idx, view_base + (row as u32) * 16);
        }
    });
}

/// One 16-byte hex-dump row, with each backed byte individually clickable.
///
/// Clicking a byte opens an inline edit; Enter commits it as a debug poke and
/// Escape cancels. The row is built from per-cell widgets rather than one
/// formatted label so a byte can become a text field in place — item spacing is
/// zeroed and every cell carries its own trailing space, which keeps the columns
/// aligned exactly as the single-label version did.
///
/// The layout does not wrap or clip, so the caller must draw it inside a
/// horizontally scrolling area (see [`draw_memory_panel`]).
fn draw_memory_row(
    ui: &mut egui::Ui,
    state: &mut DebugState,
    bus: &dyn BusDebug,
    cpu_idx: usize,
    base_addr: u32,
) {
    let mono = |text: String| egui::RichText::new(text).monospace();
    let mut ascii_part = String::with_capacity(16);
    // Recorded during the row and applied after it, so the cell loop does not
    // hold a mutable borrow of the edit slot while drawing.
    let mut start_edit: Option<u32> = None;
    let mut commit: Option<(u32, u8)> = None;
    let mut cancel = false;

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.label(mono(format!("{}  ", fmt_addr(base_addr))));

        for col in 0..16u32 {
            let addr = base_addr + col;
            if col == 8 {
                ui.label(mono(" ".to_string()));
            }

            // Label non-backed cells instead of showing a fake bus value:
            // `--` = mapped I/O, `..` = unmapped. Neither is editable — a
            // debug poke to them is ignored by the bus anyway.
            let byte = match bus.peek(cpu_idx, addr) {
                DebugRead::Backed { value, .. } => {
                    let byte = value as u8;
                    ascii_part.push(if byte.is_ascii_graphic() || byte == b' ' {
                        byte as char
                    } else {
                        '.'
                    });
                    Some(byte)
                }
                DebugRead::Io => {
                    ui.label(mono("-- ".to_string()));
                    ascii_part.push('-');
                    continue;
                }
                DebugRead::Unmapped => {
                    ui.label(mono(".. ".to_string()));
                    ascii_part.push(' ');
                    continue;
                }
            };
            let Some(byte) = byte else { continue };

            let editing = state
                .memory_edit
                .get(cpu_idx)
                .and_then(|e| e.as_ref())
                .is_some_and(|(a, _)| *a == addr);

            if editing {
                let buf = state.memory_edit[cpu_idx].as_mut().map(|(_, b)| b).unwrap();
                let resp = ui.add(
                    egui::TextEdit::singleline(buf)
                        .desired_width(22.0)
                        .font(egui::TextStyle::Monospace)
                        .char_limit(2),
                );
                resp.request_focus();
                if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    cancel = true;
                } else if resp.lost_focus() {
                    // Enter commits a parsable byte; anything else (including
                    // clicking away) just closes the editor unchanged.
                    if ui.input(|i| i.key_pressed(egui::Key::Enter))
                        && let Ok(value) = u8::from_str_radix(buf.trim(), 16)
                    {
                        commit = Some((addr, value));
                    }
                    cancel = true;
                }
                ui.label(mono(" ".to_string()));
            } else {
                let cell = ui.add(
                    egui::Label::new(mono(format!("{byte:02X} "))).sense(egui::Sense::click()),
                );
                if cell
                    .on_hover_text(format!("${} — click to edit", fmt_addr(addr)))
                    .clicked()
                {
                    start_edit = Some(addr);
                }
            }
        }

        ui.label(mono(format!(" |{ascii_part}|")));
    });

    if let Some((addr, value)) = commit {
        state.pending_memory_writes.push(MemoryWrite {
            cpu_index: cpu_idx,
            addr,
            value,
        });
    }
    if let Some(slot) = state.memory_edit.get_mut(cpu_idx) {
        if let Some(addr) = start_edit {
            *slot = Some((addr, String::new()));
        } else if cancel {
            *slot = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ROM-less machine, purely as something `execute_frame` can be handed.
    /// The tests below assert about frame accounting, not about what it draws.
    fn bare_machine() -> Box<dyn FrontendMachine> {
        let entry = *phosphor_machines::registry::all()
            .first()
            .expect("the registry is not empty");
        (entry.create_bare)()
    }

    #[test]
    fn frame_advance_lets_exactly_one_frame_through_the_global_pause() {
        let mut m = bare_machine();
        let mut s = DebugState::new();
        s.global_paused = true;

        // Paused with no request: nothing runs, and the frame counter is the
        // check that would catch a frame slipping through.
        assert!(!execute_frame(&mut *m, &mut s));
        assert_eq!(s.frame_count, 0);

        // One request, one frame.
        s.global_step = true;
        assert!(execute_frame(&mut *m, &mut s));
        assert_eq!(s.frame_count, 1);

        // And it re-holds rather than running free.
        assert!(!execute_frame(&mut *m, &mut s));
        assert_eq!(s.frame_count, 1);
    }

    #[test]
    fn a_step_request_cannot_outlive_the_frame_it_asked_for() {
        // Requested while the debug panel governs, so `execute_frame` is in no
        // position to honour it. If it were left set instead of consumed, the
        // machine would jump a frame the moment the panel closed.
        let mut m = bare_machine();
        let mut s = DebugState::new();
        s.global_paused = true;
        s.active = true;
        s.run_mode = RunMode::Paused;
        s.global_step = true;

        execute_frame(&mut *m, &mut s);
        assert!(!s.global_step, "a stale request would spend itself later");

        s.active = false;
        assert!(!execute_frame(&mut *m, &mut s));
        assert_eq!(s.frame_count, 0, "closing the panel must not run a frame");
    }

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
