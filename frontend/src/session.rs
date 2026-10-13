//! Typed session commands for the native frontend.
//!
//! One `SessionCommand` enum, one metadata table, one enablement function.
//! The queue, the drain, and the execution state arrive in later slices;
//! this module starts with the vocabulary plus the rules for when each
//! command may run. See `docs/designs/session-commands.md`.

use std::collections::VecDeque;

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
}

impl SessionState {
    /// Enqueue only. Enablement is enforced at drain time; `submit` makes
    /// no availability claim.
    pub fn submit(&mut self, cmd: SessionCommand) {
        self.queue.push_back(cmd);
    }

    /// How many commands are waiting.
    pub fn pending(&self) -> usize {
        self.queue.len()
    }
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
}
