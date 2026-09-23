//! `netlist`: read a board's transcription, check it, and draw it.
//!
//! See `docs/designs/schematic-transcription.md`. The transcription is the
//! source of truth for a board's parts, values and junctions; the netlistsvg
//! JSON beside the prose is a build product of this tool.

use clap::{Parser, Subcommand};
use phosphor_netlist::lint as lints;
use phosphor_netlist::netlist::{self, Netlist};
use phosphor_netlist::solve as solver;
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
    /// Solve the passive network: the operating point, and the natural time
    /// constants a filter-chain model approximates one at a time.
    Solve {
        /// The `.toml` transcription.
        file: PathBuf,
        /// Solve only one subcircuit, by its parts' `group`.
        #[arg(short, long)]
        group: Option<String>,
        /// Hold a net at a voltage, as `net=volts`. This is where a scenario
        /// says what the parts the solver does not model are doing: an op-amp
        /// output, a logic level, a 555's pin 3. A `part.pin` also names its
        /// net, for a net the drawing does not label.
        #[arg(short, long, value_name = "NET=VOLTS")]
        drive: Vec<String>,
        /// Override a rail's voltage, as `net=volts`. Rails whose names read
        /// as a voltage need no flag.
        #[arg(short, long, value_name = "NET=VOLTS")]
        rail: Vec<String>,
    },
    /// Generate netlistsvg input. `docs/schematics/render.sh` turns that into
    /// the SVG a document embeds.
    Svg {
        /// The `.toml` transcription.
        file: PathBuf,
        /// Where to write the JSON. Defaults to stdout.
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Draw only one subcircuit, by its parts' `group`. A net crossing the
        /// group's edge becomes a port of the excerpt.
        #[arg(short, long)]
        group: Option<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let path = match &cli.command {
        Command::Show { file, .. }
        | Command::Svg { file, .. }
        | Command::Lint { file, .. }
        | Command::Solve { file, .. } => file,
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
        Command::Svg { out, group, .. } => match cut(&netlist, group.as_deref()) {
            Some(drawn) => write_svg(&drawn, out.as_deref()),
            None => ExitCode::FAILURE,
        },
        Command::Lint { device, .. } => lint(&netlist, device.as_deref()),
        Command::Solve {
            group, drive, rail, ..
        } => solve(&netlist, group.as_deref(), drive, rail),
    }
}

/// Cut a group out, or take the whole board, reporting a group nobody drew.
fn cut(netlist: &Netlist, group: Option<&str>) -> Option<Netlist> {
    let Some(group) = group else {
        return Some(netlist.clone());
    };
    let subset = netlist.subset(group);
    if subset.parts.is_empty() {
        eprintln!(
            "no parts in group `{group}`. This file has: {}",
            netlist.groups().join(", ")
        );
        return None;
    }
    Some(subset)
}

/// Solve the passive network and report both what it says and what it assumed
/// to say it. The assumptions are not an appendix: a time constant computed
/// with the wrong pin held open is a wrong answer that looks like a right one,
/// so the list of opened pins prints before the numbers do.
fn solve(netlist: &Netlist, group: Option<&str>, drive: &[String], rail: &[String]) -> ExitCode {
    let Some(netlist) = cut(netlist, group) else {
        return ExitCode::FAILURE;
    };

    let mut setup = solver::Setup::default();
    for (flag, given, into) in [
        ("--drive", drive, &mut setup.drives),
        ("--rail", rail, &mut setup.rails),
    ] {
        for text in given {
            let Some((name, volts)) = text.rsplit_once('=') else {
                eprintln!("{flag} wants `net=volts`, and got `{text}`");
                return ExitCode::FAILURE;
            };
            let Ok(volts) = volts.trim().parse::<f64>() else {
                eprintln!("{flag} `{text}`: `{volts}` is not a number of volts");
                return ExitCode::FAILURE;
            };
            // A net the drawing does not label is still reachable by a pin on
            // it, which is how a scenario names an op-amp's own output.
            let name = match name.rsplit_once('.') {
                Some((part, pin)) => match netlist.net_of(part, pin) {
                    Some(net) => net.name.clone(),
                    None => name.to_string(),
                },
                None => name.to_string(),
            };
            into.insert(name, volts);
        }
    }

    let network = match solver::Network::build(&netlist, &setup) {
        Ok(network) => network,
        Err(problems) => {
            for problem in &problems {
                eprintln!("{problem}");
            }
            return ExitCode::FAILURE;
        }
    };

    println!("held:");
    for held in &network.held {
        let kind = if held.rail { "rail " } else { "drive" };
        println!("  {kind} {:<24} {:>9.4} V", held.net, held.volts);
    }
    println!(
        "\nnetwork: {} unknown nodes, {} resistors, {} capacitors",
        network.free.len(),
        network.resistors().len(),
        network.capacitors().len()
    );
    println!("  resistors:  {}", network.resistors().join(", "));
    println!("  capacitors: {}", network.capacitors().join(", "));

    // What the answer rests on. Printed before the answer, because a reader
    // whose question is about one of these pins has learned everything they
    // needed and can stop.
    println!("\nassumed open ({} pins):", network.opened.len());
    println!("  {}", network.opened.join(", "));
    if !network.excluded.is_empty() {
        println!("\nleft out of the network:");
        for line in &network.excluded {
            println!("  {line}");
        }
    }

    match network.dc() {
        Ok(dc) => {
            println!("\noperating point:");
            let mut dc = dc;
            dc.sort_by(|a, b| b.1.total_cmp(&a.1));
            for (net, volts) in &dc {
                println!("  {net:<24} {volts:>9.4} V");
            }
        }
        Err(problem) => println!("\noperating point: {problem}"),
    }

    match network.modes() {
        Ok(modes) if modes.is_empty() => {
            println!("\nno capacitors in the analyzed network, so it has no modes.");
        }
        Ok(modes) => {
            println!("\nnatural modes, one per capacitor:");
            for mode in &modes {
                println!("\n  tau = {}", seconds(mode.tau));
                for (net, share) in &mode.shape {
                    // Below a percent a node is not taking part in the mode,
                    // and printing it would bury the two that are.
                    if share.abs() < 0.01 {
                        continue;
                    }
                    println!("    {net:<24} {share:>7.3}");
                }
            }
        }
        Err(problem) => println!("\nnatural modes: {problem}"),
    }

    ExitCode::SUCCESS
}

/// A time constant in the unit it reads best in. These span microseconds to
/// seconds on one board, and a column of exponents is not comparable by eye.
fn seconds(tau: f64) -> String {
    if tau >= 1.0 {
        format!("{tau:.4} s")
    } else if tau >= 1e-3 {
        format!("{:.4} ms", tau * 1e3)
    } else if tau >= 1e-6 {
        format!("{:.4} us", tau * 1e6)
    } else {
        format!("{:.4} ns", tau * 1e9)
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
        // How much of the device the value check can actually see. A coverage
        // figure belongs beside a clean report, because "nothing disagrees"
        // and "nothing was compared" print the same way otherwise.
        println!(
            "  {} of them carry a literal this can hold against the sheet; {} constants name \
             several parts and are not checked{}",
            parts.values.len(),
            parts.compound.len(),
            if parts.compound.is_empty() {
                String::new()
            } else {
                format!(" ({})", parts.compound.join(", "))
            }
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
    let symbols = netlist.parts.len();
    let parts = netlist.part_count();
    if parts == symbols {
        println!("  parts:    {parts}");
    } else {
        // A run of bypass capacitors is drawn once and is still a run of
        // parts, so say both numbers rather than letting either stand alone.
        println!("  parts:    {parts} ({symbols} symbols; the rest share one)");
    }
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
