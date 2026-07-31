//! `phosphor-script` CLI — a headless Rhai script runner for phosphor machines.
//!
//! Skeleton only: the `run` subcommand is wired for argument parsing but not yet
//! implemented. The engine, bindings, and `run` body land in follow-ups
//! (`phosphor-emulator-rhai-scripting-yrwn.4` / `.5`).

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

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
    match cli.command {
        Command::Run { .. } => {
            eprintln!("phosphor-script: `run` is not yet implemented");
            ExitCode::FAILURE
        }
    }
}
