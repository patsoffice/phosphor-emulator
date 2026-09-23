//! `netlist`: read a board's transcription, check it, and draw it.
//!
//! See `docs/designs/schematic-transcription.md`. The transcription is the
//! source of truth for a board's parts, values and junctions; the netlistsvg
//! JSON beside the prose is a build product of this tool.

use clap::{Parser, Subcommand};
use phosphor_netlist::lint as lints;
use phosphor_netlist::netlist::{self, Netlist};
use phosphor_netlist::svg;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "netlist", about = "Read, check and draw a board transcription")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Load a transcription and report what it holds.
    Show {
        /// The `.toml` transcription.
        file: PathBuf,
        /// Also list every part, with its value as a number.
        #[arg(short, long)]
        parts: bool,
    },
    /// Check a transcription, and ask which parts the device does not model.
    Lint {
        /// The `.toml` transcription.
        file: PathBuf,
        /// The device source whose constant names declare which parts it
        /// models, such as `machines/src/zaxxon_sound.rs`. Without it the
        /// checks that compare the two cannot run.
        #[arg(short, long)]
        device: Option<PathBuf>,
    },
    /// Generate netlistsvg input. `docs/schematics/render.sh` turns that into
    /// the SVG a document embeds.
    Svg {
        /// The `.toml` transcription.
        file: PathBuf,
        /// Where to write the JSON. Defaults to stdout.
        #[arg(short, long)]
        out: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let path = match &cli.command {
        Command::Show { file, .. } | Command::Svg { file, .. } | Command::Lint { file, .. } => file,
    };
    let netlist = match Netlist::load(path) {
        Ok(netlist) => netlist,
        Err(error) => {
            // A transcription that will not load is a usage error rather than
            // a diagnostic: it is the answer to what was asked, it must not be
            // suppressible, and it reads as prose.
            eprint!("{error}");
            return ExitCode::FAILURE;
        }
    };

    match &cli.command {
        Command::Show { parts, .. } => {
            show(&netlist, *parts);
            ExitCode::SUCCESS
        }
        Command::Svg { out, .. } => write_svg(&netlist, out.as_deref()),
        Command::Lint { device, .. } => lint(&netlist, device.as_deref()),
    }
}

/// Run the lints and report. Exits non-zero when something is a problem, so
/// this can gate a change; the parts-not-modeled list is a question rather
/// than a defect and does not fail the run.
fn lint(netlist: &Netlist, device: Option<&Path>) -> ExitCode {
    let parts = match device {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(source) => Some(lints::DeviceParts::from_source(&source)),
            Err(e) => {
                eprintln!("{}: cannot read device source: {e}", path.display());
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };

    if let Some(parts) = &parts {
        println!(
            "device declares {} designators, from the names of its constants",
            parts.named.len()
        );
    } else {
        println!(
            "no device source given, so the two checks that compare a sheet against a \
             device are skipped. Pass --device."
        );
    }
    println!(
        "loader-enforced, so not re-checked here: a drawn pin is wired, nc or unread; \
         no duplicate designator; no pin on two nets."
    );

    let findings = lints::run(netlist, parts.as_ref());
    if findings.is_empty() {
        println!("\nnothing to report.");
        return ExitCode::SUCCESS;
    }

    let mut problems = 0;
    let mut last: Option<&str> = None;
    for finding in &findings {
        if last != Some(finding.lint) {
            println!("\n{}:", finding.lint);
            last = Some(finding.lint);
        }
        println!(
            "  {} {:<8} {}",
            finding.severity.tag(),
            finding.subject,
            finding.detail
        );
        if finding.severity == lints::Severity::Problem {
            problems += 1;
        }
    }

    println!("\n{} findings, {problems} of them problems", findings.len());
    if problems == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Emit netlistsvg input, to a file or to stdout.
fn write_svg(netlist: &Netlist, out: Option<&Path>) -> ExitCode {
    let json = svg::render(netlist);
    let text = format!("{}\n", serde_json::to_string_pretty(&json).unwrap());
    let Some(path) = out else {
        print!("{text}");
        return ExitCode::SUCCESS;
    };
    match std::fs::write(path, &text) {
        Ok(()) => {
            println!("{}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}: cannot write: {e}", path.display());
            ExitCode::FAILURE
        }
    }
}

/// What the transcription holds, on stdout so a script can read it.
fn show(netlist: &Netlist, list_parts: bool) {
    let board = &netlist.board;
    println!("{}", board.name);
    if let Some(assembly) = &board.assembly {
        println!("  assembly: {assembly}");
    }
    if let Some(excerpt) = &board.excerpt {
        println!("  excerpt:  {excerpt}");
    }
    if let Some(sheets) = &board.sheets {
        println!("  sheets:   {sheets}");
    }
    if let Some(dpi) = board.dpi {
        println!("  read at:  {dpi} dpi");
    }
    if let Some(source) = &board.source {
        println!("  source:   {source}");
    }
    if let Some(prose) = &board.prose {
        println!("  argument: {prose}");
    }
    println!("  parts:    {}", netlist.parts.len());
    println!("  nets:     {}", netlist.nets.len());

    if list_parts {
        println!("\n  parts:");
        for part in &netlist.parts {
            // The value printed as a number is the property the whole format
            // exists for: nothing downstream parses it back out of the label.
            let si = match part.value.quantity() {
                Some(q) => format!("{q:>12.6e}"),
                None => format!("{:>12}", "-"),
            };
            println!(
                "    {:<6} {:<4} {si}  {}",
                part.designator,
                part.kind.to_string(),
                part.value.label()
            );
            if let Some(note) = &part.note {
                for line in note.trim().lines() {
                    println!("        {line}");
                }
            }
        }
    }

    let dead: Vec<String> = netlist
        .parts
        .iter()
        .flat_map(|part| {
            part.nc
                .iter()
                .map(move |nc| format!("{}.{}: {}", part.designator, nc.pin, nc.why))
        })
        .collect();
    if !dead.is_empty() {
        println!("\n  pins drawn and open:");
        for line in &dead {
            println!("    {line}");
        }
    }

    // What is not read yet, said out loud. A transcription's value is that
    // "you did not transcribe this" is a question it can be asked, and that
    // only works if the answer is printed rather than inferred.
    let unread: Vec<String> = netlist
        .parts
        .iter()
        .flat_map(|part| {
            part.unread
                .iter()
                .map(move |u| format!("{}.{}: {}", part.designator, u.pin, u.why))
        })
        .collect();
    let partial: Vec<&str> = netlist
        .parts
        .iter()
        .filter(|part| part.read == netlist::Read::Partial)
        .map(|part| part.designator.as_str())
        .collect();
    if !unread.is_empty() || !partial.is_empty() {
        println!(
            "\n  NOT READ YET: {} pins, and {} parts whose reading is incomplete",
            unread.len(),
            partial.len()
        );
        for line in &unread {
            println!("    {line}");
        }
        let list_only: Vec<&&str> = partial
            .iter()
            .filter(|designator| {
                !unread
                    .iter()
                    .any(|u| u.starts_with(&format!("{designator}.")))
            })
            .collect();
        for designator in list_only {
            println!("    {designator}: the pin list itself is incomplete");
        }
    }

    let noted: Vec<String> = netlist
        .nets
        .iter()
        .filter_map(|net| {
            net.note
                .as_ref()
                .map(|note| format!("{}: {note}", net.name))
        })
        .collect();
    if !noted.is_empty() {
        println!("\n  nets with a reading behind them:");
        for line in &noted {
            println!("    {line}");
        }
    }
}
