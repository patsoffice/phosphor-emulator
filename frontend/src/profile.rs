use std::collections::VecDeque;
use std::io::Write;
use std::time::{Duration, Instant};

use phosphor_core::core::machine::ProfileSpan;

// ---------------------------------------------------------------------------
// Per-frame timing data
// ---------------------------------------------------------------------------

/// Raw timing for a single frame, captured at phase boundaries in the main loop.
struct FrameTiming {
    input: Duration,
    emulation: Duration,
    audio: Duration,
    render: Duration,
    idle: Duration,
}

// ---------------------------------------------------------------------------
// Rolling history for egui display
// ---------------------------------------------------------------------------

const HISTORY_LEN: usize = 300;

struct ProfileHistory {
    frames: VecDeque<FrameTiming>,
}

impl ProfileHistory {
    fn new() -> Self {
        Self {
            frames: VecDeque::with_capacity(HISTORY_LEN),
        }
    }

    fn push(&mut self, timing: FrameTiming) {
        if self.frames.len() >= HISTORY_LEN {
            self.frames.pop_front();
        }
        self.frames.push_back(timing);
    }

    fn clear(&mut self) {
        self.frames.clear();
    }
}

// ---------------------------------------------------------------------------
// Chrome Trace Event recorder
// ---------------------------------------------------------------------------

struct TraceEvent {
    name: String,
    category: &'static str,
    /// Absolute start time from recording epoch (nanosecond-exact).
    start: Duration,
    duration: Duration,
    tid: u32,
}

/// Thread IDs for trace tracks.
const TID_FRAME: u32 = 1;
const TID_PHASE: u32 = 2;
const TID_MACHINE: u32 = 3;

struct TraceRecorder {
    events: Vec<TraceEvent>,
    frame_count: u64,
}

impl TraceRecorder {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            frame_count: 0,
        }
    }

    fn record(
        &mut self,
        name: String,
        category: &'static str,
        tid: u32,
        start: Duration,
        duration: Duration,
    ) {
        self.events.push(TraceEvent {
            name,
            category,
            start,
            duration,
            tid,
        });
    }

    fn write_to_file(&self) -> Result<String, std::io::Error> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        let timestamp = now.as_secs();
        let filename = format!("phosphor_profile_{timestamp}.json");

        let mut file = std::fs::File::create(&filename)?;
        write!(file, "{{\"traceEvents\":[")?;

        // Thread/process metadata events for labeling tracks
        let metadata = [
            (0, "process_name", "Phosphor"),
            (TID_FRAME, "thread_name", "Frame"),
            (TID_PHASE, "thread_name", "Phases"),
            (TID_MACHINE, "thread_name", "Machine"),
        ];
        let mut first = true;
        for &(tid, meta_name, label) in &metadata {
            if !first {
                write!(file, ",")?;
            }
            first = false;
            write!(
                file,
                "{{\"name\":\"{meta_name}\",\"ph\":\"M\",\"pid\":1,\"tid\":{tid},\"args\":{{\"name\":\"{label}\"}}}}"
            )?;
        }

        // Thread sort order: Frame on top, then Phases, then Machine
        for &(tid, idx) in &[(TID_FRAME, 1), (TID_PHASE, 2), (TID_MACHINE, 3)] {
            write!(
                file,
                ",{{\"name\":\"thread_sort_index\",\"ph\":\"M\",\"pid\":1,\"tid\":{tid},\"args\":{{\"sort_index\":{idx}}}}}"
            )?;
        }

        for ev in &self.events {
            // Integer microseconds: floor(a) + floor(b) <= floor(a+b),
            // so span boundaries can only gap, never overlap.
            let ts = ev.start.as_micros();
            let dur = ev.duration.as_micros();
            write!(
                file,
                ",{{\"name\":\"{}\",\"cat\":\"{}\",\"ph\":\"X\",\"ts\":{ts},\"dur\":{dur},\"pid\":1,\"tid\":{}}}",
                ev.name, ev.category, ev.tid
            )?;
        }

        write!(file, "]}}")?;
        Ok(filename)
    }

    fn clear(&mut self) {
        self.events.clear();
        self.frame_count = 0;
    }
}

// ---------------------------------------------------------------------------
// Top-level profiling state
// ---------------------------------------------------------------------------

/// Width of the profiler side panel in pixels.
pub const PANEL_WIDTH: u32 = 320;

/// Orchestrates frame profiling: captures timing, maintains history for the
/// egui chart, and records Chrome Trace events for external analysis.
pub struct ProfileState {
    pub active: bool,
    history: ProfileHistory,
    recorder: TraceRecorder,
    recording_start: Instant,
    /// Running offset from recording start (exact nanosecond arithmetic).
    frame_offset: Duration,
}

impl ProfileState {
    pub fn new() -> Self {
        Self {
            active: false,
            history: ProfileHistory::new(),
            recorder: TraceRecorder::new(),
            recording_start: Instant::now(),
            frame_offset: Duration::ZERO,
        }
    }

    /// Start a new profiling session.
    pub fn start(&mut self) {
        self.active = true;
        self.history.clear();
        self.recorder.clear();
        self.recording_start = Instant::now();
        self.frame_offset = Duration::ZERO;
    }

    /// Stop profiling and write the trace file.
    pub fn stop(&mut self) {
        self.active = false;
        match self.recorder.write_to_file() {
            Ok(path) => eprintln!(
                "Profile trace written: {path} ({} events)",
                self.recorder.events.len()
            ),
            Err(e) => eprintln!("Failed to write profile trace: {e}"),
        }
    }

    /// Record one frame's timing data.
    pub fn record_frame(
        &mut self,
        input: Duration,
        emulation: Duration,
        audio: Duration,
        render: Duration,
        idle: Duration,
        machine_spans: &[ProfileSpan],
    ) {
        let offset = self.frame_offset;
        self.recorder.frame_count += 1;
        let frame_num = self.recorder.frame_count;

        // Active time excluding idle (for the frame span duration)
        let active = input + emulation + audio + render;

        // Frame-level span on the Frame track (shows active time only)
        self.recorder.record(
            format!("Frame {frame_num}"),
            "frame",
            TID_FRAME,
            offset,
            active,
        );

        // Phase spans on the Phases track (no idle — it's not useful).
        // All arithmetic in Duration (exact nanoseconds) to avoid f64 drift.
        let mut elapsed = Duration::ZERO;
        for (name, dur) in [
            ("input", input),
            ("emulation", emulation),
            ("audio", audio),
            ("render", render),
        ] {
            self.recorder
                .record(name.to_string(), "phase", TID_PHASE, offset + elapsed, dur);
            elapsed += dur;
        }

        // Machine sub-spans on the Machine track
        let emu_start = offset + input;
        let mut sub_elapsed = Duration::ZERO;
        for span in machine_spans {
            self.recorder.record(
                span.name.to_string(),
                "machine",
                TID_MACHINE,
                emu_start + sub_elapsed,
                span.duration,
            );
            sub_elapsed += span.duration;
        }

        self.history.push(FrameTiming {
            input,
            emulation,
            audio,
            render,
            idle,
        });

        // Advance by total frame time (including idle) so frames don't overlap
        self.frame_offset = offset + active + idle;
    }
}

// ---------------------------------------------------------------------------
// Egui profiling side panel
// ---------------------------------------------------------------------------

/// Phase colors for the stacked bar chart.
const COLOR_EMULATION: egui::Color32 = egui::Color32::from_rgb(80, 140, 255); // blue
const COLOR_RENDER: egui::Color32 = egui::Color32::from_rgb(80, 200, 120); // green
const COLOR_AUDIO: egui::Color32 = egui::Color32::from_rgb(255, 180, 60); // orange
const COLOR_INPUT: egui::Color32 = egui::Color32::from_rgb(60, 200, 220); // cyan
const COLOR_IDLE: egui::Color32 = egui::Color32::from_rgb(80, 80, 80); // dark gray

/// Draw the profiling panel as a right-side panel (like the debugger).
///
/// Must be called before `CentralPanel` or other side panels that should
/// appear between this panel and the game.
pub fn draw_profile_panel(ctx: &egui::Context, state: &ProfileState, frame_budget: Duration) {
    let budget_ms = frame_budget.as_secs_f64() * 1000.0;

    egui::SidePanel::right("profiler_panel")
        .default_width(PANEL_WIDTH as f32)
        .resizable(false)
        .show(ctx, |ui| {
            ui.heading("Profiler");
            ui.separator();

            // Chart fills available width
            let chart_width = ui.available_width();
            let chart_height = 120.0_f32;
            let max_ms = budget_ms * 2.0; // chart Y axis: 0 to 2x budget

            // Reserve space for chart
            let (response, painter) =
                ui.allocate_painter(egui::vec2(chart_width, chart_height), egui::Sense::hover());
            let rect = response.rect;

            // Background
            painter.rect_filled(rect, 0.0, egui::Color32::from_gray(20));

            // Draw stacked bars
            let n = state.history.frames.len();
            if n > 0 {
                let bar_w = chart_width / HISTORY_LEN as f32;

                for (i, frame) in state.history.frames.iter().enumerate() {
                    let x = rect.left() + i as f32 * bar_w;
                    let mut y = rect.bottom();

                    for &(dur, color) in &[
                        (frame.idle, COLOR_IDLE),
                        (frame.input, COLOR_INPUT),
                        (frame.audio, COLOR_AUDIO),
                        (frame.render, COLOR_RENDER),
                        (frame.emulation, COLOR_EMULATION),
                    ] {
                        let h = (dur.as_secs_f64() * 1000.0 / max_ms) as f32 * chart_height;
                        let top = (y - h).max(rect.top());
                        painter.rect_filled(
                            egui::Rect::from_min_max(egui::pos2(x, top), egui::pos2(x + bar_w, y)),
                            0.0,
                            color,
                        );
                        y = top;
                    }
                }
            }

            // Frame budget line
            let budget_y = rect.bottom() - (budget_ms / max_ms) as f32 * chart_height;
            painter.line_segment(
                [
                    egui::pos2(rect.left(), budget_y),
                    egui::pos2(rect.right(), budget_y),
                ],
                // `1.0_f32` rather than `1.0`: Stroke::new takes `impl Into<f32>`,
                // so a bare literal is inferred through fallback rather than from
                // the bound. rustc's float-literal-f32-fallback lint flags that,
                // and it is slated to become a hard error.
                egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(255, 60, 60)),
            );

            // Budget label
            painter.text(
                egui::pos2(rect.right() - 2.0, budget_y - 2.0),
                egui::Align2::RIGHT_BOTTOM,
                format!("{budget_ms:.1}ms"),
                egui::FontId::monospace(9.0),
                egui::Color32::from_rgb(255, 60, 60),
            );

            // Legend with current averages (vertical layout for side panel)
            ui.add_space(8.0);
            draw_legend(ui, &state.history);

            // Recording indicator
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("● REC")
                        .color(egui::Color32::from_rgb(255, 60, 60))
                        .monospace()
                        .small(),
                );
                let event_count = state.recorder.events.len();
                ui.label(
                    egui::RichText::new(format!("{event_count} events"))
                        .color(egui::Color32::GRAY)
                        .monospace()
                        .small(),
                );
            });
        });
}

/// Draw the color-coded legend with averaged timing values.
fn draw_legend(ui: &mut egui::Ui, history: &ProfileHistory) {
    let n = history.frames.len();
    let (avg_emu, avg_rnd, avg_aud, avg_inp, avg_idl) = if n > 0 {
        let mut emu = 0.0_f64;
        let mut rnd = 0.0_f64;
        let mut aud = 0.0_f64;
        let mut inp = 0.0_f64;
        let mut idl = 0.0_f64;
        for f in &history.frames {
            emu += f.emulation.as_secs_f64();
            rnd += f.render.as_secs_f64();
            aud += f.audio.as_secs_f64();
            inp += f.input.as_secs_f64();
            idl += f.idle.as_secs_f64();
        }
        let d = n as f64;
        (
            emu / d * 1000.0,
            rnd / d * 1000.0,
            aud / d * 1000.0,
            inp / d * 1000.0,
            idl / d * 1000.0,
        )
    } else {
        (0.0, 0.0, 0.0, 0.0, 0.0)
    };

    for &(color, label, value) in &[
        (COLOR_EMULATION, "emu", avg_emu),
        (COLOR_RENDER, "rnd", avg_rnd),
        (COLOR_AUDIO, "aud", avg_aud),
        (COLOR_INPUT, "inp", avg_inp),
        (COLOR_IDLE, "idl", avg_idl),
    ] {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
            ui.painter().rect_filled(rect, 1.0, color);
            ui.label(egui::RichText::new(format!("{label} {value:.2}ms")).monospace());
        });
    }
}
