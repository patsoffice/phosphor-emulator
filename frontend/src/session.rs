//! Typed session commands for the native frontend.
//!
//! One `SessionCommand` enum, one metadata table, one enablement function.
//! The queue, the drain, and the execution state arrive in later slices;
//! this module starts with the vocabulary plus the rules for when each
//! command may run. See `docs/designs/session-commands.md`.

use std::collections::VecDeque;

use phosphor_core::core::machine::FrontendMachine;

use crate::debug_ui::DeviceAction;
use crate::host_keys::HostAction;

/// What a movie is doing right now, derived fresh wherever it is needed
/// rather than stored. A finished playback is `Live`: the "ended" readout
/// may still show, but the guards treat the take as over.
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
///
/// Variants are wired slice by slice; until a slice submits one, it is
/// constructed only in tests, which is what the allowance below says.
#[allow(dead_code)]
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
    ClearWatchpoints,
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
/// Read by tests and by later slices; see the enum allowance.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
pub struct CommandMeta {
    pub id: &'static str,
    pub label: &'static str,
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
        SessionCommand::ClearWatchpoints => CommandMeta {
            id: "debug.clear_watchpoints",
            label: "Clear watchpoints",
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
        SessionCommand::ClearWatchpoints,
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
#[derive(Clone, Copy, Debug)]
pub struct EnableCtx {
    pub has_debug_bus: bool,
    pub mode: MovieMode,
    pub save_file_exists: bool,
    pub dip_banks: usize,
    pub paused: bool,
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
        SessionCommand::StepInstruction | SessionCommand::StepCycle | SessionCommand::StepFrame => {
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
        SessionCommand::SetDipOption { .. } => {
            if ctx.dip_banks == 0 {
                return Err("This machine has no DIP switches".to_string());
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
    /// Whether the machine was held before the debugger opened. Closing
    /// restores this instead of forcing a resume.
    debug_was_paused: bool,
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
    /// Opening pauses and remembers whether the machine was already held;
    /// closing restores the remembered state instead of forcing a resume.
    pub fn note_debug_panel(&mut self, open: bool, debug: &mut crate::debug_ui::DebugState) {
        use crate::debug_ui::RunMode;
        if open {
            self.debug_was_paused = debug.run_mode != RunMode::Running;
            debug.run_mode = RunMode::Paused;
        } else if !self.debug_was_paused {
            debug.run_mode = RunMode::Running;
        }
    }

    /// Run every queued command against `ctx`, in order. Enablement is
    /// re-checked per command against a freshly built snapshot, so an
    /// earlier command cannot stale a later answer. A rebuild ends the
    /// drain; the rest stays queued for the new machine. Slice 1 executes
    /// the pure-dispatch commands (reset, save, load, capture); variants
    /// from later slices come back as rejections until they are wired.
    pub fn drain(&mut self, ctx: &mut LoopCtx<'_>, mode: MovieMode) -> (Effects, Vec<Rejection>) {
        let mut effects = Effects::default();
        let mut rejections = Vec::new();

        while let Some(cmd) = self.queue.pop_front() {
            let snapshot = EnableCtx {
                has_debug_bus: ctx.machine.debug_bus().is_some(),
                mode,
                save_file_exists: ctx.save_path.exists(),
                dip_banks: ctx.machine.dip_banks().len(),
                paused: paused(ctx.debug),
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
                    // value at submit time.
                    ctx.debug.run_mode = if ctx.debug.run_mode == RunMode::Running {
                        log::info!("Paused");
                        RunMode::Paused
                    } else {
                        log::info!("Resumed");
                        RunMode::Running
                    };
                }
                SessionCommand::AdvanceFrame => {
                    use crate::debug_ui::RunMode;
                    // Auto-pause, preserved from the old global key: from
                    // Running the machine holds after the single frame,
                    // from Paused it just steps it. With the debugger open
                    // the key is a plain single step (see the NOTE on the
                    // debugger path), so the resume flag is only set with
                    // the debugger closed, where it cannot go stale.
                    if ctx.debug.run_mode == RunMode::Running && !ctx.debug.active {
                        ctx.debug.resume_after_step = true;
                    }
                    ctx.debug.run_mode = RunMode::StepFrame;
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
                    // Paused-only, enforced above. Runs now, at the frame
                    // boundary, rather than arming a mode for the next
                    // frame: the effect is the same single step.
                    ctx.machine.debug_tick();
                }
                SessionCommand::SetDisplay { settings } => {
                    phosphor_core::core::display::set_display_settings(settings);
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
                other => {
                    let reason = format!("{} is not wired yet", meta(&other).label);
                    rejections.push(Rejection { cmd: other, reason });
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
    // Unread until slice 4 wires the recorder push; kept in the context so
    // the drain signature is stable across slices.
    #[allow(dead_code)]
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
/// fields, no Default bypass: a rebuild implies the drain stops.
#[derive(Debug, Default)]
pub struct Effects {
    pub resync_input: bool,
    pub rebuild: Option<Rebuild>,
}

/// A rebuild the loop services at its top, where the machine is not
/// borrowed for the frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rebuild {
    HardReset,
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
            dip_banks: 1,
            paused: true,
        }
    }

    /// Disabled commands return their reason with nothing enqueued or
    /// mutated: `enable` is a pure function of the snapshot.
    #[test]
    fn disabled_commands_return_reasons() {
        let mut ctx = live();
        assert!(enable(&ctx, &SessionCommand::StepInstruction).is_ok());

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

    /// `submit` enqueues without judging: the drain enforces.
    #[test]
    fn submit_enqueues_without_enforcement() {
        let mut s = SessionState::default();
        s.submit(SessionCommand::ResetSoft);
        s.submit(SessionCommand::TogglePause);
        assert_eq!(s.pending(), 2);
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
}
