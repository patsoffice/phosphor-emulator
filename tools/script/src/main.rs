//! `phosphor-script` CLI — a headless Rhai script runner for phosphor machines.
//!
//! Reads a `.rhai` script, builds the engine (see [`phosphor_script::rhai_api`]),
//! optionally pre-binds a machine handle `m` when `--machine` + a rompath are
//! given, evaluates the script, and returns an [`ExitCode`]. Output convention
//! mirrors `disasm`: the script's own `print`/`debug` and any result go to
//! stdout; errors go to stderr and exit non-zero.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use rhai::Scope;

use phosphor_script::{build_engine, open_machine};

#[derive(Parser)]
#[command(
    name = "phosphor-script",
    about = "Run a Rhai script to drive and inspect a phosphor machine"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run a Rhai script. With `--machine` + a rompath, the script gets a
    /// pre-bound machine handle `m`; otherwise it must call `open(...)` itself.
    Run {
        /// Path to the `.rhai` script to evaluate.
        script: PathBuf,
        /// Machine to pre-open and bind as `m` (registry name).
        #[arg(long)]
        machine: Option<String>,
        /// ROM path (directory or `.zip`) for the pre-bound machine.
        rompath: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run_command(cli.command) {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("phosphor-script: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run_command(cmd: Command) -> Result<String, String> {
    match cmd {
        Command::Run {
            script,
            machine,
            rompath,
        } => run_script(&script, machine.as_deref(), rompath.as_deref()),
    }
}

/// Evaluate `script`, pre-binding `m` when both `machine` and `rompath` are
/// supplied. Returns whatever should be written to stdout (empty here — the
/// script drives its own `print` output).
fn run_script(
    script: &Path,
    machine: Option<&str>,
    rompath: Option<&str>,
) -> Result<String, String> {
    let source = std::fs::read_to_string(script)
        .map_err(|e| format!("reading script {}: {e}", script.display()))?;

    let engine = build_engine();
    let mut scope = Scope::new();

    match (machine, rompath) {
        (Some(name), Some(path)) => {
            scope.push("m", open_machine(name, path)?);
        }
        (None, None) => {}
        _ => {
            return Err(
                "both --machine <name> and <rompath> are required to pre-bind `m` \
                 (or pass neither and let the script call open())"
                    .to_string(),
            );
        }
    }

    engine
        .run_with_scope(&mut scope, &source)
        .map_err(|e| format!("{}: {e}", script.display()))?;
    Ok(String::new())
}
