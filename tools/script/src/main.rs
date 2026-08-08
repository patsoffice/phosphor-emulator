//! `phosphor-script` CLI — a headless Rhai script runner for phosphor machines.
//!
//! Reads Rhai source — from a `.rhai` file, inline via `-e`, or stdin via `-` —
//! builds the engine (see [`phosphor_script::rhai_api`]), optionally pre-binds a
//! machine handle `m` when `--machine` + a rompath are given, evaluates the
//! source, and returns an [`ExitCode`]. Output convention mirrors `disasm`: the
//! script's own `print`/`debug` and any result go to stdout; errors go to stderr
//! and exit non-zero.

use std::io::Read;
use std::path::PathBuf;
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
    /// Run Rhai source from a file, `-e <SOURCE>`, or `-` (stdin). With
    /// `--machine` + a rompath, the script gets a pre-bound machine handle `m`;
    /// otherwise it must call `open(...)` itself.
    Run {
        /// Inline script source, evaluated instead of reading a file.
        #[arg(short = 'e', long = "eval", value_name = "SOURCE")]
        eval: Option<String>,
        /// Machine to pre-open and bind as `m` (registry name).
        #[arg(long)]
        machine: Option<String>,
        /// ROM path (directory or `.zip`) for the pre-bound machine. Same as
        /// the positional form; use this when the positional would be ambiguous.
        #[arg(long = "rompath", value_name = "PATH")]
        rompath_flag: Option<String>,
        /// Without `-e`: `<script.rhai|-> [rompath]`. With `-e`: `[rompath]`.
        #[arg(value_name = "ARGS")]
        args: Vec<String>,
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
            eval,
            machine,
            rompath_flag,
            args,
        } => {
            let run = resolve_run(eval, rompath_flag, args)?;
            let (label, source) = read_source(&run.source)?;
            run_script(&label, &source, machine.as_deref(), run.rompath.as_deref())
        }
    }
}

/// Where a `run`'s Rhai source comes from.
#[derive(Debug, PartialEq, Eq)]
enum Source {
    /// Read from a `.rhai` file.
    File(PathBuf),
    /// Supplied inline on the command line via `-e`.
    Inline(String),
    /// Read from stdin (the `-` positional).
    Stdin,
}

/// A `run` invocation with its positionals resolved.
#[derive(Debug, PartialEq, Eq)]
struct RunArgs {
    source: Source,
    rompath: Option<String>,
}

/// Resolve `run`'s positionals, whose meaning depends on whether the source was
/// given inline.
///
/// The subcommand grew a second way to supply source without being able to grow
/// a second positional slot: `run <script> [rompath]` was already the documented
/// form, so an optional leading `<script>` would bind a lone ROM path to the
/// script. Instead the positionals are collected and interpreted here — with
/// `-e` the sole positional is the rompath, without it the first is the source
/// and the second the rompath. `--rompath` names it explicitly either way.
fn resolve_run(
    eval: Option<String>,
    rompath_flag: Option<String>,
    args: Vec<String>,
) -> Result<RunArgs, String> {
    let mut args = args.into_iter();
    let (source, positional_rompath) = match eval {
        Some(src) => {
            // The one positional left is the rompath. Catch the two ways a
            // second source can be smuggled in there, rather than silently
            // booting a "machine" from a script path.
            match args.next() {
                Some(a) if a == "-" => {
                    return Err("both -e <SOURCE> and `-` (stdin) given; pass exactly one \
                                source"
                        .to_string());
                }
                Some(a) if a.ends_with(".rhai") => {
                    return Err(format!(
                        "both -e <SOURCE> and a script file ({a}) given; pass exactly one source"
                    ));
                }
                rompath => (Source::Inline(src), rompath),
            }
        }
        None => {
            let script = args.next().ok_or_else(|| {
                "no script: pass a <script.rhai> path, `-` to read stdin, or -e <SOURCE>"
                    .to_string()
            })?;
            let source = if script == "-" {
                Source::Stdin
            } else {
                Source::File(PathBuf::from(script))
            };
            (source, args.next())
        }
    };

    if let Some(extra) = args.next() {
        return Err(format!("unexpected extra argument: {extra}"));
    }

    let rompath = match (positional_rompath, rompath_flag) {
        (Some(pos), Some(flag)) => {
            return Err(format!(
                "rompath given twice: {pos} (positional) and {flag} (--rompath)"
            ));
        }
        (rompath, None) | (None, rompath) => rompath,
    };

    Ok(RunArgs { source, rompath })
}

/// Load a [`Source`], returning the label errors should name it by and the Rhai
/// text itself.
fn read_source(source: &Source) -> Result<(String, String), String> {
    match source {
        Source::File(path) => {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("reading script {}: {e}", path.display()))?;
            Ok((path.display().to_string(), text))
        }
        Source::Inline(text) => Ok(("<inline>".to_string(), text.clone())),
        Source::Stdin => {
            let mut text = String::new();
            std::io::stdin()
                .read_to_string(&mut text)
                .map_err(|e| format!("reading script from stdin: {e}"))?;
            Ok(("<stdin>".to_string(), text))
        }
    }
}

/// Evaluate `source`, pre-binding `m` when both `machine` and `rompath` are
/// supplied. `label` names the source in error messages (a path, `<inline>`, or
/// `<stdin>`). Returns whatever should be written to stdout (empty here — the
/// script drives its own `print` output).
fn run_script(
    label: &str,
    source: &str,
    machine: Option<&str>,
    rompath: Option<&str>,
) -> Result<String, String> {
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
        .run_with_scope(&mut scope, source)
        .map_err(|e| format!("{label}: {e}"))?;
    Ok(String::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn file_form_keeps_script_then_rompath() {
        let run = resolve_run(None, None, args(&["capture.rhai", "/roms"])).unwrap();
        assert_eq!(run.source, Source::File(PathBuf::from("capture.rhai")));
        assert_eq!(run.rompath.as_deref(), Some("/roms"));
    }

    #[test]
    fn file_form_without_rompath() {
        let run = resolve_run(None, None, args(&["capture.rhai"])).unwrap();
        assert_eq!(run.source, Source::File(PathBuf::from("capture.rhai")));
        assert_eq!(run.rompath, None);
    }

    #[test]
    fn inline_form_binds_lone_positional_to_rompath() {
        let run = resolve_run(Some("m.reset();".into()), None, args(&["/roms"])).unwrap();
        assert_eq!(run.source, Source::Inline("m.reset();".into()));
        assert_eq!(run.rompath.as_deref(), Some("/roms"));
    }

    #[test]
    fn inline_form_without_rompath() {
        let run = resolve_run(Some("print(1);".into()), None, args(&[])).unwrap();
        assert_eq!(run.source, Source::Inline("print(1);".into()));
        assert_eq!(run.rompath, None);
    }

    #[test]
    fn dash_reads_stdin() {
        let run = resolve_run(None, None, args(&["-", "/roms"])).unwrap();
        assert_eq!(run.source, Source::Stdin);
        assert_eq!(run.rompath.as_deref(), Some("/roms"));
    }

    #[test]
    fn rompath_flag_works_in_both_forms() {
        let run = resolve_run(Some("print(1);".into()), Some("/roms".into()), args(&[])).unwrap();
        assert_eq!(run.rompath.as_deref(), Some("/roms"));

        let run = resolve_run(None, Some("/roms".into()), args(&["capture.rhai"])).unwrap();
        assert_eq!(run.source, Source::File(PathBuf::from("capture.rhai")));
        assert_eq!(run.rompath.as_deref(), Some("/roms"));
    }

    #[test]
    fn missing_source_errors() {
        let err = resolve_run(None, None, args(&[])).unwrap_err();
        assert!(err.contains("no script"), "{err}");
    }

    #[test]
    fn inline_plus_script_file_errors() {
        let err = resolve_run(Some("print(1);".into()), None, args(&["capture.rhai"])).unwrap_err();
        assert!(err.contains("exactly one source"), "{err}");
    }

    #[test]
    fn inline_plus_stdin_errors() {
        let err = resolve_run(Some("print(1);".into()), None, args(&["-"])).unwrap_err();
        assert!(err.contains("exactly one source"), "{err}");
    }

    #[test]
    fn rompath_given_twice_errors() {
        let err = resolve_run(None, Some("/b".into()), args(&["capture.rhai", "/a"])).unwrap_err();
        assert!(err.contains("rompath given twice"), "{err}");
    }

    #[test]
    fn extra_positional_errors() {
        let err = resolve_run(None, None, args(&["capture.rhai", "/roms", "junk"])).unwrap_err();
        assert!(err.contains("unexpected extra argument"), "{err}");
    }
}
