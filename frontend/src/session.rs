//! Typed session commands for the native frontend.
//!
//! One `SessionCommand` enum, one metadata table, one enablement function,
//! one queue drained at the frame boundary. See
//! `docs/designs/session-commands.md`.

use std::collections::VecDeque;

use phosphor_core::core::machine::FrontendMachine;

use crate::debug_ui::DeviceAction;
use crate::host_keys::HostAction;

/// What a movie is doing right now, derived fresh wherever it is needed
/// rather than stored. A finished playback is `Live`: the "ended" readout
/// shows on the final frame, but the guards treat the take as over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MovieMode {
    Live,
    Recording,
    Replaying,
}

/// Derive the take mode from the live capture and playback state.
pub fn movie_mode(recording: bool, replaying: bool) -> MovieMode {
    if recording {
        MovieMode::Recording
    } else if replaying {
        MovieMode::Replaying
    } else {
        MovieMode::Live
    }
}

/// A session-level action, submitted by hotkeys or panels and executed at a
/// frame boundary. Only existing frontend behavior gets a variant; the
/// catalog never invents emulator features.
#[derive(Clone, Debug)]
pub enum SessionCommand {
    ResetSoft,
    ResetHard,
    TogglePause,
    AdvanceFrame,
    StepInstruction,
    StepCycle,
    StepFrame,
    SaveState,
    LoadState,
    CaptureFrame,
    SetDisplay {
        settings: phosphor_core::core::display::DisplaySettings,
    },
    SetDipOption {
        bank: usize,
        option: usize,
        value: u8,
    },
    SetBreakpoint {
        cpu: usize,
        addr: u32,
    },
    ClearBreakpoint {
        cpu: usize,
        addr: u32,
    },
    SetWatchpoint {
        cpu: usize,
        addr: u32,
        kind: phosphor_core::core::WatchpointKind,
    },
    ClearWatchpoint {
        cpu: usize,
        addr: u32,
        kind: phosphor_core::core::WatchpointKind,
    },
    SetTraceEnabled(bool),
    ClearTrace,
    PokeMemory {
        cpu: usize,
        addr: u32,
        value: u8,
    },
    DeviceControl(DeviceAction),
    MovieRecordToggle,
}

/// Where a command belongs in the UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandCategory {
    Session,
    Execution,
    State,
    Display,
    Input,
    Debugger,
    Capture,
}

/// Stable id, human-readable label, and category for a command. Labels
/// reuse the `HostAction` text so the panels and the key legend agree.
///
/// The label feeds the refusal log and notices. The id and category are
/// read by tests today; per-call-site UI hints are the follow-up that reads
/// them in production (the drain enforces `enable`, the panels do not
/// consult it yet).
#[derive(Clone, Copy, Debug)]
pub struct CommandMeta {
    // Dead until the UI-hints follow-up; kept because the metadata table
    // promises every variant a stable id and a category.
    #[allow(dead_code)]
    pub id: &'static str,
    pub label: &'static str,
    // Same allowance and reason as `id`.
    #[allow(dead_code)]
    pub category: CommandCategory,
}

/// Metadata for a command. The match has no wildcard arm, so a new variant
/// fails to compile until it carries metadata.
pub fn meta(cmd: &SessionCommand) -> CommandMeta {
    match cmd {
        SessionCommand::ResetSoft => CommandMeta {
            id: "session.reset_soft",
            label: HostAction::Reset.label(),
            category: CommandCategory::Session,
        },
        SessionCommand::ResetHard => CommandMeta {
            id: "session.reset_hard",
            label: HostAction::HardReset.label(),
            category: CommandCategory::Session,
        },
        SessionCommand::TogglePause => CommandMeta {
            id: "session.pause",
            label: HostAction::TogglePause.label(),
            category: CommandCategory::Session,
        },
        SessionCommand::AdvanceFrame => CommandMeta {
            id: "session.advance_frame",
            label: HostAction::FrameAdvance.label(),
            category: CommandCategory::Session,
        },
        SessionCommand::StepInstruction => CommandMeta {
            id: "exec.step_instruction",
            label: HostAction::StepInstruction.label(),
            category: CommandCategory::Execution,
        },
        SessionCommand::StepCycle => CommandMeta {
            id: "exec.step_cycle",
            label: HostAction::StepCycle.label(),
            category: CommandCategory::Execution,
        },
        SessionCommand::StepFrame => CommandMeta {
            id: "exec.step_frame",
            label: HostAction::StepFrame.label(),
            category: CommandCategory::Execution,
        },
        SessionCommand::SaveState => CommandMeta {
            id: "state.save",
            label: HostAction::QuickSave.label(),
            category: CommandCategory::State,
        },
        SessionCommand::LoadState => CommandMeta {
            id: "state.load",
            label: HostAction::QuickLoad.label(),
            category: CommandCategory::State,
        },
        SessionCommand::CaptureFrame => CommandMeta {
            id: "capture.frame",
            label: HostAction::Screenshot.label(),
            category: CommandCategory::Capture,
        },
        SessionCommand::SetDisplay { .. } => CommandMeta {
            id: "display.set",
            label: "Display settings",
            category: CommandCategory::Display,
        },
        SessionCommand::SetDipOption { .. } => CommandMeta {
            id: "dip.set_option",
            label: "DIP option",
            category: CommandCategory::Input,
        },
        SessionCommand::SetBreakpoint { .. } => CommandMeta {
            id: "debug.set_breakpoint",
            label: "Set breakpoint",
            category: CommandCategory::Debugger,
        },
        SessionCommand::ClearBreakpoint { .. } => CommandMeta {
            id: "debug.clear_breakpoint",
            label: "Clear breakpoint",
            category: CommandCategory::Debugger,
        },
        SessionCommand::SetWatchpoint { .. } => CommandMeta {
            id: "debug.set_watchpoint",
            label: "Set watchpoint",
            category: CommandCategory::Debugger,
        },
        SessionCommand::ClearWatchpoint { .. } => CommandMeta {
            id: "debug.clear_watchpoint",
            label: "Clear watchpoint",
            category: CommandCategory::Debugger,
        },
        SessionCommand::SetTraceEnabled(..) => CommandMeta {
            id: "debug.set_trace",
            label: "Trace enable",
            category: CommandCategory::Debugger,
        },
        SessionCommand::ClearTrace => CommandMeta {
            id: "debug.clear_trace",
            label: "Clear trace",
            category: CommandCategory::Debugger,
        },
        SessionCommand::PokeMemory { .. } => CommandMeta {
            id: "debug.poke_memory",
            label: "Poke memory",
            category: CommandCategory::Debugger,
        },
        SessionCommand::DeviceControl(..) => CommandMeta {
            id: "debug.device_control",
            label: "Device control",
            category: CommandCategory::Debugger,
        },
        SessionCommand::MovieRecordToggle => CommandMeta {
            id: "movie.toggle_record",
            label: HostAction::MovieRecord.label(),
            category: CommandCategory::Session,
        },
    }
}

/// Every variant, with representative payloads for the data-carrying ones.
/// The id-uniqueness test below walks this list, so a variant added here
/// without metadata still fails the compile first.
#[cfg(test)]
pub fn all() -> Vec<SessionCommand> {
    use phosphor_core::core::WatchpointKind;
    use phosphor_core::core::display::DisplaySettings;

    vec![
        SessionCommand::ResetSoft,
        SessionCommand::ResetHard,
        SessionCommand::TogglePause,
        SessionCommand::AdvanceFrame,
        SessionCommand::StepInstruction,
        SessionCommand::StepCycle,
        SessionCommand::StepFrame,
        SessionCommand::SaveState,
        SessionCommand::LoadState,
        SessionCommand::CaptureFrame,
        SessionCommand::SetDisplay {
            settings: DisplaySettings::MEASURED,
        },
        SessionCommand::SetDipOption {
            bank: 0,
            option: 0,
            value: 0,
        },
        SessionCommand::SetBreakpoint { cpu: 0, addr: 0 },
        SessionCommand::ClearBreakpoint { cpu: 0, addr: 0 },
        SessionCommand::SetWatchpoint {
            cpu: 0,
            addr: 0,
            kind: WatchpointKind::Write,
        },
        SessionCommand::ClearWatchpoint {
            cpu: 0,
            addr: 0,
            kind: WatchpointKind::Write,
        },
        SessionCommand::SetTraceEnabled(true),
        SessionCommand::ClearTrace,
        SessionCommand::PokeMemory {
            cpu: 0,
            addr: 0,
            value: 0,
        },
        SessionCommand::DeviceControl(DeviceAction::Reset(0)),
        SessionCommand::MovieRecordToggle,
    ]
}

/// Read-only snapshot for enablement, rebuilt before each command in a
/// batch so earlier commands cannot stale the answer.
#[derive(Clone, Debug)]
pub struct EnableCtx {
    pub has_debug_bus: bool,
    pub mode: MovieMode,
    pub save_file_exists: bool,
    /// Option count per DIP bank, in bank order. `SetDipOption` validates
    /// both indices against this; an out-of-range bank would otherwise
    /// record a `(bank, 0)` DIP entry for a bank that does not exist.
    pub dip_option_counts: Vec<usize>,
    pub paused: bool,
    pub debug_open: bool,
}

/// Whether a command may run, or the display-ready reason it may not. A
/// failure enqueues nothing and mutates nothing.
pub fn enable(ctx: &EnableCtx, cmd: &SessionCommand) -> Result<(), String> {
    // Takes first: any state change outside the input stream is refused
    // while recording or replaying. `SetDipOption` is the one exception,
    // and only while recording, because the recorder captures it.
    match ctx.mode {
        MovieMode::Recording => match cmd {
            SessionCommand::ResetSoft
            | SessionCommand::ResetHard
            | SessionCommand::LoadState
            | SessionCommand::PokeMemory { .. }
            | SessionCommand::DeviceControl(..)
            | SessionCommand::StepInstruction
            | SessionCommand::StepCycle => {
                return Err(format!(
                    "{} is unavailable while recording",
                    meta(cmd).label
                ));
            }
            _ => {}
        },
        MovieMode::Replaying => match cmd {
            SessionCommand::SetDipOption { .. } => {
                return Err(format!(
                    "{} is unavailable while replaying",
                    meta(cmd).label
                ));
            }
            SessionCommand::MovieRecordToggle => {
                return Err("Cannot arm a recording while replaying a movie".to_string());
            }
            SessionCommand::ResetSoft
            | SessionCommand::ResetHard
            | SessionCommand::LoadState
            | SessionCommand::PokeMemory { .. }
            | SessionCommand::DeviceControl(..)
            | SessionCommand::StepInstruction
            | SessionCommand::StepCycle => {
                return Err(format!(
                    "{} is unavailable while replaying",
                    meta(cmd).label
                ));
            }
            _ => {}
        },
        MovieMode::Live => {}
    }

    // Capability and state checks.
    match cmd {
        SessionCommand::StepInstruction | SessionCommand::StepCycle => {
            // Debugger-modal, like the keys and the panel buttons: a step
            // queued while the debugger is open but drained after it closes
            // (one event batch can hold both) must be refused, or the armed
            // mode sits in the closed-debugger branch forever and the
            // machine stops running frames.
            if !ctx.debug_open {
                return Err("Stepping needs the debugger open".to_string());
            }
            if !ctx.paused {
                return Err("Stepping needs the emulator paused".to_string());
            }
            Ok(())
        }
        SessionCommand::StepFrame => {
            if !ctx.paused {
                return Err("Stepping needs the emulator paused".to_string());
            }
            Ok(())
        }
        SessionCommand::LoadState => {
            if !ctx.save_file_exists {
                return Err("No saved state to load".to_string());
            }
            Ok(())
        }
        SessionCommand::PokeMemory { .. } | SessionCommand::DeviceControl(..) => {
            if !ctx.has_debug_bus {
                return Err("This machine has no debug bus".to_string());
            }
            Ok(())
        }
        SessionCommand::SetDipOption { bank, option, .. } => {
            let Some(&options) = ctx.dip_option_counts.get(*bank) else {
                if ctx.dip_option_counts.is_empty() {
                    return Err("This machine has no DIP switches".to_string());
                }
                return Err(format!("DIP bank {bank} does not exist"));
            };
            if *option >= options {
                return Err(format!("DIP bank {bank} has no option {option}"));
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// The queued commands waiting for the next drain.
#[derive(Debug, Default)]
pub struct SessionState {
    queue: VecDeque<SessionCommand>,
    /// Whether the open debugger paused a running machine itself. Closing
    /// undoes only that automatic pause, and only if no pause or step
    /// drained while the panel was open: any of those is the user's own
    /// choice, and the close leaves it alone.
    auto_paused: bool,
}

impl SessionState {
    /// Enqueue only. Enablement is enforced at drain time; `submit` makes
    /// no availability claim.
    pub fn submit(&mut self, cmd: SessionCommand) {
        self.queue.push_back(cmd);
    }

    /// How many commands are waiting.
    #[cfg(test)]
    pub fn pending(&self) -> usize {
        self.queue.len()
    }

    /// Record the debugger panel opening or closing against `debug`.
    /// Opening pauses a running machine and remembers doing so; closing
    /// resumes only in that case. A pause or step the user chose inside
    /// the panel clears the flag through the drain, so the close leaves
    /// the user's choice alone instead of overriding it.
    pub fn note_debug_panel(&mut self, open: bool, debug: &mut crate::debug_ui::DebugState) {
        use crate::debug_ui::RunMode;
        if open {
            self.auto_paused = debug.run_mode == RunMode::Running;
            debug.run_mode = RunMode::Paused;
        } else if self.auto_paused {
            self.auto_paused = false;
            debug.run_mode = RunMode::Running;
        }
    }

    /// Run every queued command against `ctx`, in order. Enablement is
    /// re-checked per command against a freshly built snapshot, so an
    /// earlier command cannot stale a later answer. Only `replaying`
    /// crosses the drain boundary (a replay cannot start or end
    /// mid-drain); the recording half is re-derived from the live
    /// capture per command, so a stop unfreezes the mode for the rest
    /// of the batch. A rebuild ends the drain; the rest stays queued
    /// for the new machine.
    pub fn drain(&mut self, ctx: &mut LoopCtx<'_>, replaying: bool) -> (Effects, Vec<Rejection>) {
        let mut effects = Effects {
            resync_input: false,
            rebuild: None,
        };
        let mut rejections = Vec::new();

        while let Some(cmd) = self.queue.pop_front() {
            let snapshot = EnableCtx {
                has_debug_bus: ctx.machine.debug_bus().is_some(),
                mode: movie_mode(ctx.movies.is_recording(), replaying),
                save_file_exists: ctx.save_path.exists(),
                dip_option_counts: ctx
                    .machine
                    .dip_banks()
                    .iter()
                    .map(|b| b.options.len())
                    .collect(),
                paused: paused(ctx.debug),
                debug_open: ctx.debug.active,
            };
            if let Err(reason) = enable(&snapshot, &cmd) {
                rejections.push(Rejection { cmd, reason });
                continue;
            }
            match cmd {
                SessionCommand::ResetSoft => {
                    ctx.machine.reset();
                    ctx.debug.frame_count = 0;
                    effects.resync_input = true;
                }
                SessionCommand::ResetHard => {
                    effects.rebuild = Some(Rebuild::HardReset);
                    break;
                }
                SessionCommand::TogglePause => {
                    use crate::debug_ui::RunMode;
                    // Resolved here, at the drain, so two toggles in one
                    // batch toggle twice rather than resolving to the same
                    // value at submit time. A drained toggle is the user's
                    // own choice, so it clears the debugger's automatic
                    // pause: closing the panel leaves this alone.
                    self.auto_paused = false;
                    ctx.debug.run_mode = if ctx.debug.run_mode == RunMode::Running {
                        log::info!("Paused");
                        RunMode::Paused
                    } else {
                        log::info!("Resumed");
                        RunMode::Running
                    };
                }
                SessionCommand::AdvanceFrame => {
                    // Auto-pause, preserved from the old global key: from
                    // Running the machine holds after the single frame, from
                    // Paused it just steps it. `StepFrame` always re-holds,
                    // so no resume flag is needed. A drained step is the
                    // user's own choice: see `TogglePause` above.
                    self.auto_paused = false;
                    ctx.debug.run_mode = crate::debug_ui::RunMode::StepFrame;
                }
                SessionCommand::SaveState => match ctx.machine.save_state() {
                    Some(data) => match std::fs::write(ctx.save_path, &data) {
                        Ok(()) => {
                            log::info!("Save state written ({} bytes)", data.len());
                            ctx.notices.info("State saved");
                        }
                        Err(e) => {
                            log::error!("save state failed: {e}");
                            ctx.notices.error(format!("Save state failed: {e}"));
                        }
                    },
                    None => {
                        log::warn!("save states not supported for this machine");
                        ctx.notices
                            .error("Save states not supported for this machine");
                    }
                },
                SessionCommand::LoadState => match std::fs::read(ctx.save_path) {
                    Ok(data) => match ctx.machine.load_state(&data) {
                        Ok(()) => {
                            log::info!("State loaded");
                            ctx.notices.info("State loaded");
                            ctx.debug.frame_count = 0;
                            effects.resync_input = true;
                        }
                        Err(e) => {
                            log::error!("load state failed: {e}");
                            ctx.notices.error(format!("Load state failed: {e}"));
                        }
                    },
                    Err(e) => {
                        log::warn!("no save file found: {e}");
                        ctx.notices.error("No saved state to load");
                    }
                },
                SessionCommand::StepInstruction => {
                    // Modal and paused-only, enforced above. Arms the mode;
                    // the next `execute_frame` runs to the selected CPU's
                    // next instruction boundary and re-holds. Arming rather
                    // than running keeps one definition of the step: the
                    // hotkey and the panel button share the `RunMode` path.
                    // A drained step is the user's own choice: see
                    // `TogglePause` above.
                    self.auto_paused = false;
                    ctx.debug.run_mode = crate::debug_ui::RunMode::StepInstruction;
                }
                SessionCommand::StepCycle => {
                    // Same shape: one cycle in the next `execute_frame`,
                    // with its watchpoint drain, so a hit on that cycle
                    // stops there instead of surfacing at the next resume.
                    // A drained step is the user's own choice: see
                    // `TogglePause` above.
                    self.auto_paused = false;
                    ctx.debug.run_mode = crate::debug_ui::RunMode::StepCycle;
                }
                SessionCommand::StepFrame => {
                    // Paused-only, enforced above. Arms the mode; the next
                    // `execute_frame` runs exactly one frame and re-holds.
                    // A drained step is the user's own choice: see
                    // `TogglePause` above.
                    self.auto_paused = false;
                    ctx.debug.run_mode = crate::debug_ui::RunMode::StepFrame;
                }
                SessionCommand::SetDisplay { settings } => {
                    phosphor_core::core::display::set_display_settings(settings);
                }
                SessionCommand::SetBreakpoint { cpu, addr } => {
                    while ctx.debug.breakpoints.len() <= cpu {
                        ctx.debug.breakpoints.push(std::collections::HashSet::new());
                    }
                    if let Some(set) = ctx.debug.breakpoints.get_mut(cpu) {
                        set.insert(addr);
                    }
                }
                SessionCommand::ClearBreakpoint { cpu, addr } => {
                    if let Some(set) = ctx.debug.breakpoints.get_mut(cpu) {
                        set.remove(&addr);
                    }
                }
                SessionCommand::SetWatchpoint { cpu, addr, kind } => {
                    let entry = (cpu, addr, kind);
                    if !ctx.debug.watchpoints.contains(&entry) {
                        ctx.debug.watchpoints.push(entry);
                        ctx.debug.watchpoints_dirty = true;
                    }
                }
                SessionCommand::ClearWatchpoint { cpu, addr, kind } => {
                    ctx.debug
                        .watchpoints
                        .retain(|&(c, a, k)| !(c == cpu && a == addr && k == kind));
                    ctx.debug.watchpoints_dirty = true;
                }
                SessionCommand::SetTraceEnabled(on) => {
                    ctx.debug.trace_enabled = on;
                    ctx.debug.trace_enabled_dirty = true;
                }
                SessionCommand::ClearTrace => {
                    ctx.debug.trace_clear_requested = true;
                }
                SessionCommand::PokeMemory { cpu, addr, value } => {
                    // `poke`, not `write`: a debugger write is tagged
                    // `DebugAccessSource::Frontend`, matching the old
                    // `pending_memory_writes` path in `execute_frame`.
                    if let Some(bus) = ctx.machine.debug_bus_mut() {
                        bus.poke(cpu, addr, value);
                    }
                }
                SessionCommand::DeviceControl(action) => {
                    if let Some(bus) = ctx.machine.debug_bus_mut() {
                        match action {
                            crate::debug_ui::DeviceAction::Reset(device_index) => {
                                bus.reset_device(device_index)
                            }
                            crate::debug_ui::DeviceAction::WriteRegister {
                                device_index,
                                offset,
                                value,
                            } => bus.write_device_register(device_index, offset, value),
                        }
                    }
                }
                SessionCommand::SetDipOption {
                    bank,
                    option,
                    value,
                } => {
                    ctx.machine.set_dip_option(bank, option, value);
                    ctx.movies
                        .push_dip(bank as u8, ctx.machine.dip_bank_value(bank));
                }
                SessionCommand::MovieRecordToggle => {
                    if ctx.movies.is_recording() {
                        // Stopping writes the file and stays enabled in
                        // every mode.
                        match ctx.movies.stop() {
                            Ok(saved) => {
                                log::info!("{}", saved.message);
                                ctx.notices.info(format!(
                                    "Movie saved: {}",
                                    crate::emulator::file_name(&saved.path)
                                ));
                            }
                            Err(e) => {
                                log::error!("{e}");
                                ctx.notices.error(e);
                            }
                        }
                    } else {
                        // Arming rebuilds the machine from ROM; the loop top
                        // services it. Reaching here while replaying is
                        // refused by `enable` above.
                        effects.rebuild = Some(Rebuild::MovieArm);
                        break;
                    }
                }
                SessionCommand::CaptureFrame => {
                    // Through the harness helper, which is the one definition
                    // of "the frame this cabinet displays" and is what the
                    // golden pins hash. Then the cabinet's gel over it,
                    // because a screenshot is meant to be what the player
                    // was looking at.
                    let (sw, sh, mut shot) = phosphor_harness::render_oriented(ctx.machine);
                    if let Some(o) = ctx.overlay {
                        o.apply(&mut shot, sw, sh);
                    }
                    match crate::screenshot::save_screenshot(
                        &shot,
                        sw,
                        sh,
                        ctx.screenshot_dir,
                        ctx.machine_name,
                    ) {
                        Ok(path) => {
                            log::info!("Screenshot saved: {}", path.display());
                            ctx.notices.info(format!(
                                "Screenshot saved: {}",
                                crate::emulator::file_name(&path)
                            ));
                        }
                        Err(e) => {
                            log::error!("screenshot failed: {e}");
                            ctx.notices.error(format!("Screenshot failed: {e}"));
                        }
                    }
                }
            }
        }

        (effects, rejections)
    }
}

/// Everything the drain may touch, borrowed from the loop for the call.
/// The drain applies DIPs, screenshots, IO, and feedback itself; what
/// returns is only what the loop must still do. Debug state travels as one
/// borrow so the drain sees run mode, counters, and flags together.
pub struct LoopCtx<'a> {
    pub machine: &'a mut dyn FrontendMachine,
    pub debug: &'a mut crate::debug_ui::DebugState,
    pub save_path: &'a std::path::Path,
    pub screenshot_dir: &'a std::path::Path,
    pub machine_name: &'a str,
    pub movies: &'a mut crate::movie::MovieCapture,
    pub overlay: Option<&'a crate::screen_overlay::ScreenOverlay>,
    pub notices: &'a mut crate::overlay::Notices,
}

/// Paused in either of the old senses: the debugger closed and holding, or
/// the debugger governing and held.
fn paused(debug: &crate::debug_ui::DebugState) -> bool {
    use crate::debug_ui::RunMode;
    !matches!(debug.run_mode, RunMode::Running)
}

/// The only effects the drain cannot apply itself. Plain struct, public
/// fields, no `Default`: the drain spells out both fields at construction,
/// so a new field breaks the drain until it is considered. A rebuild
/// implies the drain stops.
#[derive(Debug)]
pub struct Effects {
    pub resync_input: bool,
    pub rebuild: Option<Rebuild>,
}

/// A rebuild the loop services at its top, where the machine is not
/// borrowed for the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rebuild {
    HardReset,
    MovieArm,
}

/// A refused or unwired command with its display-ready reason.
#[derive(Debug)]
pub struct Rejection {
    pub cmd: SessionCommand,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// Every hotkey-fed command carries its feeder's label, so the panels
    /// and the key legend agree because they read the same text. If a
    /// command is rewired to another action, or either side is reworded
    /// alone, this pair table fails.
    #[test]
    fn command_labels_match_their_feeding_host_actions() {
        use crate::host_keys::HostAction;

        let fed: &[(SessionCommand, HostAction)] = &[
            (SessionCommand::ResetSoft, HostAction::Reset),
            (SessionCommand::ResetHard, HostAction::HardReset),
            (SessionCommand::TogglePause, HostAction::TogglePause),
            (SessionCommand::AdvanceFrame, HostAction::FrameAdvance),
            (SessionCommand::StepInstruction, HostAction::StepInstruction),
            (SessionCommand::StepCycle, HostAction::StepCycle),
            (SessionCommand::StepFrame, HostAction::StepFrame),
            (SessionCommand::SaveState, HostAction::QuickSave),
            (SessionCommand::LoadState, HostAction::QuickLoad),
            (SessionCommand::CaptureFrame, HostAction::Screenshot),
            (SessionCommand::MovieRecordToggle, HostAction::MovieRecord),
        ];
        for (cmd, action) in fed {
            assert_eq!(
                meta(cmd).label,
                action.label(),
                "{} no longer matches its feeding action",
                meta(cmd).id
            );
        }
    }

    /// Every command has a stable id, and no two share one. The `meta()`
    /// match without a wildcard arm already forces an arm per variant at
    /// compile time; this pins the ids on top.
    #[test]
    fn command_ids_are_unique_and_complete() {
        let cmds = all();
        let mut ids = HashSet::new();
        for cmd in &cmds {
            let m = meta(cmd);
            assert!(!m.id.is_empty(), "empty id");
            assert!(!m.label.is_empty(), "empty label for {}", m.id);
            assert!(ids.insert(m.id), "duplicate command id: {}", m.id);
        }
        assert_eq!(ids.len(), cmds.len());
    }

    /// The take mode is derived, never stored. Recording wins over
    /// replaying (the two cannot be live at once, but the order makes the
    /// refusal tables total), and a finished playback is Live for guard
    /// purposes, which the caller expresses by passing `replaying: false`.
    #[test]
    fn movie_mode_derives_from_live_capture_and_playback() {
        assert_eq!(movie_mode(false, false), MovieMode::Live);
        assert_eq!(movie_mode(true, false), MovieMode::Recording);
        assert_eq!(movie_mode(false, true), MovieMode::Replaying);
        assert_eq!(movie_mode(true, true), MovieMode::Recording);
    }

    fn live() -> EnableCtx {
        EnableCtx {
            has_debug_bus: true,
            mode: MovieMode::Live,
            save_file_exists: true,
            dip_option_counts: vec![1],
            paused: true,
            debug_open: true,
        }
    }

    /// Disabled commands return their reason with nothing enqueued or
    /// mutated: `enable` is a pure function of the snapshot.
    #[test]
    fn disabled_commands_return_reasons() {
        let mut ctx = live();
        assert!(enable(&ctx, &SessionCommand::StepInstruction).is_ok());

        ctx.debug_open = false;
        assert_eq!(
            enable(&ctx, &SessionCommand::StepInstruction),
            Err("Stepping needs the debugger open".to_string())
        );
        assert_eq!(
            enable(&ctx, &SessionCommand::StepCycle),
            Err("Stepping needs the debugger open".to_string())
        );
        assert!(enable(&ctx, &SessionCommand::StepFrame).is_ok());
        ctx.debug_open = true;

        ctx.paused = false;
        assert_eq!(
            enable(&ctx, &SessionCommand::StepFrame),
            Err("Stepping needs the emulator paused".to_string())
        );
        ctx.paused = true;

        ctx.save_file_exists = false;
        assert_eq!(
            enable(&ctx, &SessionCommand::LoadState),
            Err("No saved state to load".to_string())
        );
        ctx.save_file_exists = true;

        ctx.has_debug_bus = false;
        assert_eq!(
            enable(
                &ctx,
                &SessionCommand::PokeMemory {
                    cpu: 0,
                    addr: 0,
                    value: 0
                }
            ),
            Err("This machine has no debug bus".to_string())
        );
        ctx.has_debug_bus = true;

        ctx.mode = MovieMode::Recording;
        assert!(enable(&ctx, &SessionCommand::ResetSoft).is_err());
        assert!(enable(&ctx, &SessionCommand::StepFrame).is_ok());
        assert!(
            enable(
                &ctx,
                &SessionCommand::SetDipOption {
                    bank: 0,
                    option: 0,
                    value: 0
                }
            )
            .is_ok()
        );

        ctx.mode = MovieMode::Replaying;
        assert!(
            enable(
                &ctx,
                &SessionCommand::SetDipOption {
                    bank: 0,
                    option: 0,
                    value: 0
                }
            )
            .is_err()
        );
        assert!(enable(&ctx, &SessionCommand::MovieRecordToggle).is_err());
        assert!(enable(&ctx, &SessionCommand::StepFrame).is_ok());
    }

    /// Out-of-range DIP indices are refused with reasons before anything
    /// mutates: an unchecked bank would otherwise record a `(bank, 0)` DIP
    /// entry for a bank that does not exist.
    #[test]
    fn set_dip_option_validates_bank_and_option() {
        let dip = |bank: usize, option: usize| SessionCommand::SetDipOption {
            bank,
            option,
            value: 0,
        };
        let mut ctx = live();
        ctx.dip_option_counts = vec![2];
        assert!(enable(&ctx, &dip(0, 0)).is_ok());
        assert!(enable(&ctx, &dip(0, 1)).is_ok());
        assert_eq!(
            enable(&ctx, &dip(0, 2)),
            Err("DIP bank 0 has no option 2".to_string())
        );
        assert_eq!(
            enable(&ctx, &dip(1, 0)),
            Err("DIP bank 1 does not exist".to_string())
        );
        ctx.dip_option_counts = vec![];
        assert_eq!(
            enable(&ctx, &dip(0, 0)),
            Err("This machine has no DIP switches".to_string())
        );
    }

    /// `submit` enqueues without judging: the drain enforces.
    #[test]
    fn submit_enqueues_without_enforcement() {
        let mut s = SessionState::default();
        s.submit(SessionCommand::ResetSoft);
        s.submit(SessionCommand::TogglePause);
        assert_eq!(s.pending(), 2);
    }

    /// End to end across frames on a ROM-less machine: pause, step, and a
    /// breakpoint flow through submit and the drain, and the batch order
    /// decides. Two toggles in one batch toggle twice; a step after a
    /// pause in the same batch is accepted because enablement is rebuilt
    /// per command; a step while running and a load with no save file
    /// come back as rejections with reasons, and the machine is untouched.
    /// Take guards, a poke round trip, and a DIP flip run through the same
    /// drain below, in Recording, Replaying, and Live modes.
    #[test]
    fn commands_run_across_frames_on_a_booted_machine() {
        use crate::debug_ui::{DebugState, RunMode};
        use crate::movie::MovieCapture;
        use crate::overlay::Notices;

        let mut machine = (phosphor_machines::registry::all()
            .first()
            .expect("the registry is not empty")
            .create_bare)();
        let mut debug = DebugState::new();
        let mut notices = Notices::default();
        let mut movies = MovieCapture::new(
            std::path::Path::new("/nonexistent-dir"),
            "test",
            "test",
            [0u8; 32],
        );
        let save_path = std::path::Path::new("/nonexistent-dir/quick.save");

        let run = |s: &mut SessionState,
                   debug: &mut DebugState,
                   machine: &mut dyn FrontendMachine,
                   notices: &mut Notices,
                   movies: &mut MovieCapture,
                   replaying: bool| {
            let mut ctx = LoopCtx {
                machine,
                debug,
                save_path,
                screenshot_dir: std::path::Path::new("/nonexistent-dir"),
                machine_name: "test",
                movies,
                overlay: None,
                notices,
            };
            s.drain(&mut ctx, replaying)
        };

        let mut s = SessionState::default();

        // With the debugger closed, a sub-frame step is refused even paused:
        // arming it would park the closed-debugger branch in a mode that
        // never runs frames. The pause itself is accepted.
        s.submit(SessionCommand::TogglePause);
        s.submit(SessionCommand::StepInstruction);
        let (_, rejections) = run(
            &mut s,
            &mut debug,
            machine.as_mut(),
            &mut notices,
            &mut movies,
            false,
        );
        assert_eq!(rejections.len(), 1);
        assert_eq!(meta(&rejections[0].cmd).id, "exec.step_instruction");
        assert_eq!(rejections[0].reason, "Stepping needs the debugger open");
        assert_eq!(debug.run_mode, RunMode::Paused);

        // Pause, then step in the same batch: the step sees the pause.
        // The drain arms the instruction step without running it; the next
        // `execute_frame` runs it to the selected CPU's next instruction
        // boundary and re-holds, exactly like the panel's Instr button.
        // Asserting the armed-but-unrun state pins the contract a
        // run-it-in-the-drain regression would break: that version leaves
        // the mode Paused with cycles already spent.
        // Sub-frame steps are debugger-modal, so the debugger is open for
        // this section, as the hotkeys require. The machine is running
        // again: the closed case above left it paused.
        debug.active = true;
        debug.run_mode = RunMode::Running;
        let cycles_before = debug.cycle_count;
        s.submit(SessionCommand::TogglePause);
        s.submit(SessionCommand::StepInstruction);
        let (effects, rejections) = run(
            &mut s,
            &mut debug,
            machine.as_mut(),
            &mut notices,
            &mut movies,
            false,
        );
        assert!(rejections.is_empty());
        assert!(!effects.resync_input);
        assert_eq!(debug.run_mode, RunMode::StepInstruction);
        assert_eq!(
            debug.cycle_count, cycles_before,
            "the drain arms the step; only execute_frame runs it"
        );
        crate::debug_ui::execute_frame(machine.as_mut(), &mut debug, false);
        assert_eq!(debug.run_mode, RunMode::Paused);
        assert!(debug.cycle_count > cycles_before);

        // Two toggles in one batch toggle twice.
        s.submit(SessionCommand::TogglePause);
        s.submit(SessionCommand::TogglePause);
        run(
            &mut s,
            &mut debug,
            machine.as_mut(),
            &mut notices,
            &mut movies,
            false,
        );
        assert_eq!(debug.run_mode, RunMode::Paused);

        // A breakpoint lands in the set; a step while running and a load
        // with no save file are refused with reasons.
        s.submit(SessionCommand::SetBreakpoint {
            cpu: 0,
            addr: 0x100,
        });
        s.submit(SessionCommand::TogglePause);
        let (_, rejections) = run(
            &mut s,
            &mut debug,
            machine.as_mut(),
            &mut notices,
            &mut movies,
            false,
        );
        assert!(debug.breakpoints.iter().any(|set| set.contains(&0x100)));
        assert!(rejections.is_empty());
        assert_eq!(debug.run_mode, RunMode::Running);

        s.submit(SessionCommand::StepInstruction);
        s.submit(SessionCommand::LoadState);
        let (_, rejections) = run(
            &mut s,
            &mut debug,
            machine.as_mut(),
            &mut notices,
            &mut movies,
            false,
        );
        assert_eq!(rejections.len(), 2);
        assert!(
            rejections.iter().all(|r| !r.reason.is_empty()),
            "every refusal carries its reason"
        );
        assert_eq!(debug.run_mode, RunMode::Running);

        // Take guards through the same drain. In Recording, a reset, a
        // sub-frame step, and a poke are refused with reasons and the
        // machine is untouched; a whole-frame step and a DIP flip stay
        // enabled. The machine is Running here, so pause first: the step
        // and the flip both need it. The mode derives from the live
        // capture, so this section arms a real recording.
        let (nvram, dip) = MovieCapture::starting_conditions(machine.as_mut());
        movies.arm_fresh(machine.as_mut(), nvram, dip);
        let frames_before = debug.frame_count;
        let cycles_before = debug.cycle_count;
        let dip_ok = !machine.dip_banks().is_empty();
        s.submit(SessionCommand::TogglePause);
        s.submit(SessionCommand::ResetSoft);
        s.submit(SessionCommand::StepInstruction);
        s.submit(SessionCommand::PokeMemory {
            cpu: 0,
            addr: 0x200,
            value: 0xAA,
        });
        s.submit(SessionCommand::StepFrame);
        s.submit(SessionCommand::SetDipOption {
            bank: 0,
            option: 0,
            value: 0,
        });
        let (_, rejections) = run(
            &mut s,
            &mut debug,
            machine.as_mut(),
            &mut notices,
            &mut movies,
            false,
        );
        let refused: Vec<&str> = rejections.iter().map(|r| meta(&r.cmd).id).collect();
        for id in [
            "session.reset_soft",
            "exec.step_instruction",
            "debug.poke_memory",
        ] {
            assert!(
                refused.contains(&id),
                "{id} must be refused while recording"
            );
        }
        assert!(
            !refused.contains(&"exec.step_frame"),
            "whole-frame steps stay enabled while recording"
        );
        assert_eq!(
            refused.contains(&"dip.set_option"),
            !dip_ok,
            "a DIP flip is the one state change the recorder captures"
        );
        assert!(
            rejections.iter().all(|r| !r.reason.is_empty()),
            "every refusal carries its reason"
        );
        assert_eq!(debug.frame_count, frames_before);
        assert_eq!(debug.cycle_count, cycles_before);
        // The step armed but did not run: the drain only stages it.
        assert_eq!(debug.run_mode, RunMode::StepFrame);

        // In Replaying, the DIP flip and arming a recording join the
        // refused set; pausing stays available. The previous section left
        // a live recording, so stop it first: the mode derives from the
        // capture, and a stop writes nothing here (the movies directory
        // does not exist, which the stop reports instead of failing).
        s.submit(SessionCommand::MovieRecordToggle);
        run(
            &mut s,
            &mut debug,
            machine.as_mut(),
            &mut notices,
            &mut movies,
            false,
        );
        assert!(!movies.is_recording());
        s.submit(SessionCommand::SetDipOption {
            bank: 0,
            option: 0,
            value: 0,
        });
        s.submit(SessionCommand::MovieRecordToggle);
        s.submit(SessionCommand::TogglePause);
        let (_, rejections) = run(
            &mut s,
            &mut debug,
            machine.as_mut(),
            &mut notices,
            &mut movies,
            true,
        );
        let refused: Vec<&str> = rejections.iter().map(|r| meta(&r.cmd).id).collect();
        assert!(refused.contains(&"dip.set_option"));
        assert!(refused.contains(&"movie.toggle_record"));
        assert!(
            !refused.contains(&"session.pause"),
            "pausing stays available while replaying"
        );

        // AdvanceFrame through the drain: from Running with the debugger
        // closed it arms one frame and holds afterwards, the hold-here
        // contract the resume flag used to break. The closed debugger is
        // the point: the old code only set the flag when
        // `run_mode == Running && !debug.active`, so an open-debugger test
        // passes with the bug back in. State is set explicitly, not
        // inherited from whatever the previous batch left behind.
        debug.active = false;
        debug.run_mode = RunMode::Running;
        let frames_before = debug.frame_count;
        s.submit(SessionCommand::AdvanceFrame);
        let (_, rejections) = run(
            &mut s,
            &mut debug,
            machine.as_mut(),
            &mut notices,
            &mut movies,
            false,
        );
        assert!(rejections.is_empty());
        assert_eq!(debug.run_mode, RunMode::StepFrame);
        crate::debug_ui::execute_frame(machine.as_mut(), &mut debug, false);
        assert_eq!(debug.frame_count, frames_before + 1);
        assert_eq!(debug.run_mode, RunMode::Paused);

        // A poke round trip in Live: the value lands in the machine's own
        // memory, proving dispatch reaches the operation. The probe scans
        // for debugger-backed bytes first, so the test does not assume any
        // machine's map.
        let probe = [0x100u32, 0x200, 0x400, 0x800, 0x1000, 0x2000, 0x4000]
            .into_iter()
            .find(|&addr| {
                machine.debug_bus().is_some_and(|bus| {
                    matches!(
                        bus.peek(0, addr),
                        phosphor_core::core::DebugRead::Backed { .. }
                    )
                })
            })
            .expect("the bare machine exposes backed memory to the debugger");
        let before = machine
            .debug_bus()
            .and_then(|bus| match bus.peek(0, probe) {
                phosphor_core::core::DebugRead::Backed { value, .. } => Some(value as u8),
                _ => None,
            })
            .expect("probed address is backed");
        let poked = before.wrapping_add(1);
        s.submit(SessionCommand::PokeMemory {
            cpu: 0,
            addr: probe,
            value: poked,
        });
        let (_, rejections) = run(
            &mut s,
            &mut debug,
            machine.as_mut(),
            &mut notices,
            &mut movies,
            false,
        );
        assert!(rejections.is_empty());
        let after = machine
            .debug_bus()
            .and_then(|bus| match bus.peek(0, probe) {
                phosphor_core::core::DebugRead::Backed { value, .. } => Some(value as u8),
                _ => None,
            });
        assert_eq!(after, Some(poked), "the poke reached machine memory");

        // Reset, save/load, and capture reach their operations in Live,
        // through a real temp dir. The save lands before the load drains,
        // so per-command enablement sees the file the earlier command
        // wrote. When the machine cannot save, the save writes nothing and
        // the load is refused with its reason instead.
        let tmp =
            std::env::temp_dir().join(format!("phosphor-session-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).expect("temp dir for the session test");
        let save_file = tmp.join("quick.save");
        let _ = std::fs::remove_file(&save_file);
        let supports_save = machine.save_state().is_some();
        s.submit(SessionCommand::ResetSoft);
        s.submit(SessionCommand::SaveState);
        s.submit(SessionCommand::LoadState);
        s.submit(SessionCommand::CaptureFrame);
        let (effects, rejections) = {
            let mut ctx = LoopCtx {
                machine: machine.as_mut(),
                debug: &mut debug,
                save_path: &save_file,
                screenshot_dir: &tmp,
                machine_name: "test",
                movies: &mut movies,
                overlay: None,
                notices: &mut notices,
            };
            s.drain(&mut ctx, false)
        };
        assert_eq!(save_file.exists(), supports_save);
        if supports_save {
            assert!(rejections.is_empty());
            assert!(effects.resync_input);
            assert_eq!(debug.frame_count, 0);
        } else {
            assert_eq!(rejections.len(), 1, "only the load is refused");
            assert!(!rejections[0].reason.is_empty());
        }
        let entries: Vec<_> = std::fs::read_dir(&tmp).expect("temp dir").collect();
        assert_eq!(
            entries.len(),
            if supports_save { 2 } else { 1 },
            "save file plus screenshot, or screenshot alone"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Closing the debugger restores the remembered pause state instead of
    /// forcing a resume: a machine paused before the panel opened stays
    /// paused, and one that was running resumes.
    #[test]
    fn debug_panel_close_restores_prior_pause_state() {
        use crate::debug_ui::{DebugState, RunMode};

        let mut s = SessionState::default();
        let mut debug = DebugState::new();
        debug.run_mode = RunMode::Running;

        s.note_debug_panel(true, &mut debug);
        assert_eq!(debug.run_mode, RunMode::Paused);
        s.note_debug_panel(false, &mut debug);
        assert_eq!(debug.run_mode, RunMode::Running);

        debug.run_mode = RunMode::Paused;
        s.note_debug_panel(true, &mut debug);
        s.note_debug_panel(false, &mut debug);
        assert_eq!(debug.run_mode, RunMode::Paused);
    }

    /// Stopping a recording unfreezes the mode for the rest of the batch: a
    /// command behind the stop sees Live, not Recording. Only `replaying`
    /// crosses the drain boundary; the recording half is re-derived from
    /// the live capture per command.
    #[test]
    fn stop_unfreezes_the_mode_for_the_rest_of_the_batch() {
        use crate::debug_ui::DebugState;
        use crate::movie::MovieCapture;
        use crate::overlay::Notices;

        let mut machine = (phosphor_machines::registry::all()
            .first()
            .expect("the registry is not empty")
            .create_bare)();
        let mut debug = DebugState::new();
        let mut notices = Notices::default();
        let dir = std::env::temp_dir().join(format!("phosphor_drain_mode_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut movies = MovieCapture::new(&dir, "test", "test", [0u8; 32]);
        let save_path = dir.join("quick.save");

        // Arm a real recording so the mode derives Recording.
        let (nvram, dip) = MovieCapture::starting_conditions(machine.as_mut());
        movies.arm_fresh(machine.as_mut(), nvram, dip);
        assert!(movies.is_recording());

        let run = |s: &mut SessionState,
                   debug: &mut DebugState,
                   machine: &mut dyn FrontendMachine,
                   notices: &mut Notices,
                   movies: &mut MovieCapture,
                   replaying: bool| {
            let mut ctx = LoopCtx {
                machine,
                debug,
                save_path: &save_path,
                screenshot_dir: &dir,
                machine_name: "test",
                movies,
                overlay: None,
                notices,
            };
            s.drain(&mut ctx, replaying)
        };

        // [stop, poke]: the poke must not see Recording.
        let mut s = SessionState::default();
        s.submit(SessionCommand::MovieRecordToggle);
        s.submit(SessionCommand::PokeMemory {
            cpu: 0,
            addr: 0x200,
            value: 0xAA,
        });
        let (_, rejections) = run(
            &mut s,
            &mut debug,
            machine.as_mut(),
            &mut notices,
            &mut movies,
            false,
        );
        assert!(
            !rejections.iter().any(|r| r.reason.contains("recording")),
            "the poke behind a stop must see Live: {rejections:?}"
        );

        // [stop, toggle]: the second toggle arms instead of stopping twice.
        let (nvram, dip) = MovieCapture::starting_conditions(machine.as_mut());
        movies.arm_fresh(machine.as_mut(), nvram, dip);
        let mut s = SessionState::default();
        s.submit(SessionCommand::MovieRecordToggle);
        s.submit(SessionCommand::MovieRecordToggle);
        let (effects, rejections) = run(
            &mut s,
            &mut debug,
            machine.as_mut(),
            &mut notices,
            &mut movies,
            false,
        );
        assert!(rejections.is_empty(), "{rejections:?}");
        assert_eq!(effects.rebuild, Some(Rebuild::MovieArm));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Closing the debugger undoes only its own automatic pause: a pause
    /// the user chose inside the panel survives the close, while an
    /// untouched open still resumes a machine that was running.
    #[test]
    fn debugger_close_keeps_a_pause_chosen_inside() {
        use crate::debug_ui::{DebugState, RunMode};
        use crate::movie::MovieCapture;
        use crate::overlay::Notices;

        let mut machine = (phosphor_machines::registry::all()
            .first()
            .expect("the registry is not empty")
            .create_bare)();
        let mut debug = DebugState::new();
        let mut notices = Notices::default();
        let dir = std::env::temp_dir().join(format!("phosphor_debug_close_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut movies = MovieCapture::new(&dir, "test", "test", [0u8; 32]);
        let save_path = dir.join("quick.save");

        let mut s = SessionState::default();
        debug.run_mode = RunMode::Running;
        s.note_debug_panel(true, &mut debug);
        assert_eq!(debug.run_mode, RunMode::Paused);

        // Two drained toggles: resume, then pause again. The pause is the
        // user's choice, so the close must not override it.
        s.submit(SessionCommand::TogglePause);
        s.submit(SessionCommand::TogglePause);
        {
            let mut ctx = LoopCtx {
                machine: machine.as_mut(),
                debug: &mut debug,
                save_path: &save_path,
                screenshot_dir: &dir,
                machine_name: "test",
                movies: &mut movies,
                overlay: None,
                notices: &mut notices,
            };
            let (_, rejections) = s.drain(&mut ctx, false);
            assert!(rejections.is_empty(), "{rejections:?}");
        }
        assert_eq!(debug.run_mode, RunMode::Paused);
        s.note_debug_panel(false, &mut debug);
        assert_eq!(
            debug.run_mode,
            RunMode::Paused,
            "a pause chosen inside the panel survives closing it"
        );

        // Untouched open of a running machine still resumes on close.
        debug.run_mode = RunMode::Running;
        s.note_debug_panel(true, &mut debug);
        s.note_debug_panel(false, &mut debug);
        assert_eq!(debug.run_mode, RunMode::Running);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
