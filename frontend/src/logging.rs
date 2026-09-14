//! Terminal logging for the `phosphor` binary.
//!
//! The frontend is an interactive program, not a service, and its output is
//! read live next to the window it describes. So the format is cargo's rather
//! than a log file's: an `info!` is the bare message, because that level is
//! reserved here for confirming something the user just asked for ("Screenshot
//! saved: …") and a `[INFO phosphor_frontend::emulator]` in front of it is
//! only noise. `warn!` and `error!` take a colored prefix so they stand out in
//! a scrollback, and `debug!` carries its module path, since that is the level
//! you filter by target when you are chasing something.
//!
//! What goes at which level:
//!
//! - `error!` : an operation the user asked for did not happen.
//! - `warn!`  : degraded, but the emulator carries on (no audio device, a
//!   config file that would not parse, a GL stage that fell back).
//! - `info!`  : confirmation of a user action. Shown by default.
//! - `debug!` : internal state a developer would want. Off by default.
//! - `trace!` : per-frame or hotter. Compiled out of release builds by the
//!   workspace's `release_max_level_debug`; see the root Cargo.toml.
//!
//! Fatal startup errors (an unknown machine, a missing ROM path) stay on
//! `eprintln!` in `main`. They are the program's usage output, not diagnostics:
//! they must not be suppressible by `RUST_LOG`, and they read as prose rather
//! than as a record.

use std::io::Write;

/// Install the terminal logger. Called once, from `main`.
///
/// Defaults to `info`, so a stock run shows confirmations and problems and
/// nothing else. `RUST_LOG` overrides in the usual way, including per-module:
/// `RUST_LOG=phosphor_frontend::video=debug`.
pub fn init() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format(|buf, record| {
            let level = record.level();
            let style = buf.default_level_style(level);
            match level {
                log::Level::Info => writeln!(buf, "{}", record.args()),
                log::Level::Warn => writeln!(buf, "{style}warning{style:#}: {}", record.args()),
                log::Level::Error => writeln!(buf, "{style}error{style:#}: {}", record.args()),
                // Debug and trace are developer-facing, so they keep the target
                // that `RUST_LOG=<target>=debug` selects on.
                _ => writeln!(
                    buf,
                    "{style}{level:?}{style:#} [{}] {}",
                    record.target(),
                    record.args()
                ),
            }
        })
        .init();
}
