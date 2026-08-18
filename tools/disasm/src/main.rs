//! Standalone CPU disassembler CLI.
//!
//! Disassembles a ROM with one of the per-CPU disassemblers in `phosphor-core`,
//! without spinning up the SDL/egui debug UI. Useful for inspecting sound/CPU
//! ROMs (e.g. the Mario Bros 8049 sound program) from the terminal.
//!
//! Disassembly input modes:
//! - `raw`     — a raw, already-extracted ROM file + an explicit `--cpu`.
//! - `rom`     — a `.zip`/directory ROM set + a member filename + `--cpu`.
//! - `machine` — a machine name + region; CPU and origin are resolved from the
//!   [`phosphor_machines::disasm_registry`].
//!
//! Plus graphics/video modes:
//! - `gfxview`   — a machine name + region; decodes a tile/sprite GFX ROM to a
//!   PNG sheet, resolved from the [`phosphor_machines::gfx_registry`].
//! - `frameshot` — boot a registered machine for N frames and dump the rendered
//!   frame to a PNG (a headless screenshot), optionally diffing it against a
//!   reference image (e.g. a MAME snapshot).
//! - `imgdiff`   — compare two RGB PNGs and report the pixel diff percentage,
//!   optionally writing a red-highlight image of the differing pixels.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use phosphor_core::cpu::{Disassemble, hex_bytes};
use phosphor_core::cpu::{I8035, M6502, M6800, M6809, M68000, Mb88xx, Z80};
use phosphor_core::gfx::decode::decode_gfx;
use phosphor_machines::disasm_registry::{self, DisasmCpu};
use phosphor_machines::gfx_registry;
use phosphor_machines::registry;

use phosphor_harness::movie::{Movie, MovieRecord, hex};
use phosphor_harness::{Harness, hash_frame, hash_vectors, load_rom_set, render_oriented};

mod gfxsheet;
mod trace;
use gfxsheet::SheetConfig;
use trace::TraceFormat;

/// Shown by `disasm --help`, above the subcommand list.
///
/// The positional/flag split is not arbitrary and is easy to trip over when
/// moving between `raw`/`rom` and the machine-driven subcommands, so it is
/// stated once here rather than left to be inferred from seven usage lines.
const CLI_LONG_ABOUT: &str = "\
Disassemble a ROM with a chosen CPU (phosphor-core disassemblers), and inspect \
registered machines headlessly.

Argument convention, uniform across subcommands:

  positional   WHERE the bytes come from — a raw file, or a ROM set (`.zip` or a
               directory of loose files), plus a member name where one file must
               be picked out of the set. Positional because it is the argument
               that varies per invocation and benefits from shell completion.

  --flag       WHAT to look at and how to read it — `--cpu`, `--machine`,
               `--region`, `--org`, output and range options.

So the two families read:

  raw/rom      you supply the CPU and origin, because a bare ROM says nothing
               about them:
                 disasm raw --cpu z80 --org 0x8000 sound.bin
                 disasm rom --cpu m6809 roms.zip cpu.6e

  machine/…    you name a registered machine and the CPU, origin, and mapping
               come from its registry entry:
                 disasm machine --machine mariobros --region sound roms/
                 disasm trace   --machine joust --frames 60 --events bank roms/

`--machine` is a flag rather than a positional on purpose: it selects a
registry entry, not a path, and keeps the ROM set in the same positional slot
it occupies everywhere else. Subcommands that only list what is available
(`machine` and `gfxview` with no `--region`) take no ROM set at all.";

#[derive(Parser)]
#[command(
    name = "disasm",
    about = "Disassemble a ROM with a chosen CPU (phosphor-core disassemblers)",
    long_about = CLI_LONG_ABOUT
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Disassemble a raw, already-extracted ROM file.
    Raw {
        /// CPU whose disassembler to use.
        #[arg(long)]
        cpu: CpuArg,
        /// Load/origin address of the first byte (hex `0x..` or decimal).
        #[arg(long, default_value = "0", value_parser = parse_u32_auto)]
        org: u32,
        #[command(flatten)]
        range: RangeArgs,
        /// Raw ROM file.
        file: PathBuf,
    },
    /// Disassemble a member file of a ROM set (`.zip` or directory).
    Rom {
        /// CPU whose disassembler to use.
        #[arg(long)]
        cpu: CpuArg,
        /// Load/origin address of the first byte (hex `0x..` or decimal).
        #[arg(long, default_value = "0", value_parser = parse_u32_auto)]
        org: u32,
        #[command(flatten)]
        range: RangeArgs,
        /// ROM set: a `.zip` archive or a directory of loose ROM files.
        path: String,
        /// File within the ROM set to disassemble.
        member: String,
    },
    /// Disassemble a known machine's code region (CPU + origin auto-resolved).
    Machine {
        /// Machine CLI name (e.g. `mariobros`).
        #[arg(long)]
        machine: String,
        /// Region to disassemble (e.g. `sound`). Omit to list available regions.
        #[arg(long)]
        region: Option<String>,
        #[command(flatten)]
        range: RangeArgs,
        /// ROM set: a `.zip` archive or a directory of loose ROM files
        /// (not needed when listing regions).
        path: Option<String>,
    },
    /// Decode a machine's graphics ROM region to a PNG tile/sprite sheet.
    Gfxview {
        /// Machine CLI name (e.g. `congobongo`).
        #[arg(long)]
        machine: String,
        /// Region to decode (e.g. `sprites`). Omit to list available regions.
        #[arg(long)]
        region: Option<String>,
        /// Elements per row in the output sheet.
        #[arg(long, default_value_t = 16)]
        cols: usize,
        /// Integer nearest-neighbor upscale factor.
        #[arg(long, default_value_t = 1)]
        scale: usize,
        /// Output PNG path (default: `<machine>_<region>.png`).
        #[arg(short = 'o', long)]
        out: Option<PathBuf>,
        /// ROM set: a `.zip` archive or a directory of loose ROM files
        /// (not needed when listing regions).
        path: Option<String>,
    },
    /// Boot a registered machine for N frames and dump the rendered frame to a
    /// PNG — a headless equivalent of a frontend screenshot, for validating a
    /// machine's video output against a reference (e.g. a MAME snapshot).
    Frameshot {
        /// Machine CLI name (e.g. `mrdo`).
        #[arg(long)]
        machine: String,
        /// Number of frames to run (from reset) before capturing.
        #[arg(long, default_value_t = 0)]
        frames: usize,
        /// Output PNG path (default: `<machine>_f<frames>.png`).
        #[arg(short = 'o', long)]
        out: Option<PathBuf>,
        /// Optional reference PNG to diff the captured frame against.
        #[arg(long)]
        compare: Option<PathBuf>,
        /// Load a factory-initialized NVRAM before running (skips the on-boot
        /// self-test / factory-restore so attract mode is reached sooner).
        #[arg(long)]
        nvram: Option<PathBuf>,
        /// Write the machine's NVRAM to this path after the run (to capture a
        /// factory-initialized fixture for `--nvram`).
        #[arg(long)]
        dump_nvram: Option<PathBuf>,
        /// Pulse the machine's coin input at this frame (to add a credit and
        /// trigger coin-insert speech/SFX).
        #[arg(long)]
        coin_at: Option<usize>,
        /// Write the full run's audio to this path as a 16-bit mono WAV.
        #[arg(long)]
        audio_out: Option<PathBuf>,
        /// ROM set: a `.zip` archive or a directory of loose ROM files.
        path: String,
    },
    /// Boot a registered machine, run N frames, and observe CPU/bus state
    /// headlessly: the board's event ring (`--events`) and memory watchpoints
    /// (`--watch`), correlated by cycle. The headless counterpart of the
    /// interactive debugger's event/watchpoint panels, in text or JSONL.
    Trace {
        /// Machine CLI name (e.g. `joust`).
        #[arg(long)]
        machine: String,
        /// Number of frames to run (from reset).
        #[arg(long, default_value_t = 0)]
        frames: usize,
        /// Start emitting output only at/after this frame (run fast to N with
        /// observers off, then observe — a cheap "seek").
        #[arg(long, default_value_t = 0)]
        from_frame: usize,
        /// Pulse the machine's coin input at this frame.
        #[arg(long)]
        coin_at: Option<usize>,
        /// Script input presses to reach gameplay: comma-separated
        /// `<control>@<frame>[:<hold>]` (e.g. `fire1@120`, `coin@60:8`).
        /// `control` is a stable input name from the machine's control table.
        #[arg(long)]
        press: Option<String>,
        /// Script trackball/spinner motion: comma-separated
        /// `<control>@<frame>[:<frames>][=<delta>]` (e.g.
        /// `p1_trackball_x@120:60=3.0`). One delta is fed per frame; `frames`
        /// defaults to 1 and `delta` to 1.0.
        #[arg(long = "move")]
        motion: Option<String>,
        /// Load a factory-initialized NVRAM before running.
        #[arg(long)]
        nvram: Option<PathBuf>,
        /// Diagnostic: replay a recorded entropy sequence (whitespace- or
        /// comma-separated hex bytes) in place of the machine's PRNG, so a
        /// run can be diffed instruction-for-instruction against a reference
        /// emulator whose PRNG cannot be recomputed.
        #[arg(long)]
        entropy_file: Option<PathBuf>,
        /// Set operator DIP switches before running: comma-separated
        /// `<option>=<choice>` entries using the machine's published names
        /// (e.g. `Coinage=Free Play`, `Lives=5`). `bank<N>=<value>` sets a
        /// whole bank byte. An unknown name is an error listing what is
        /// available.
        #[arg(long)]
        dip: Option<String>,
        /// Enable event tracing and include these event kinds: a comma-separated
        /// list (e.g. `devwrite,bank,watchdog`) or `all`.
        #[arg(long)]
        events: Option<String>,
        /// Set memory watchpoint(s): comma-separated `cpu:addr:kind[:cond]`
        /// specs, kind = `r`/`w`/`rw`. Optional `cond` gates on the value:
        /// `=HEX` equals, `&MASK=HEX` bit test, `chg` changed (hex operands).
        /// E.g. `0:0x87cf:w`, `0:0x4000:w:=4E5F`, `1:0x20:w:chg`.
        #[arg(long)]
        watch: Option<String>,
        /// Instruction-trace these CPU(s): comma-separated `<name|idx>[:regs]`
        /// (e.g. `main`, `0:regs`, `0,1`). `:regs` appends a register snapshot.
        /// Switches to the per-cycle loop; large traces — bound with
        /// `--from-frame`/`--frames`.
        #[arg(long)]
        cpu: Option<String>,
        /// Stop when a CPU reaches an address: comma-separated `<cpu>:<addr>`
        /// (e.g. `0:0xF000`). Also switches to the per-cycle loop.
        #[arg(long)]
        break_pc: Option<String>,
        /// Stop at the first watchpoint hit (per-cycle loop).
        #[arg(long)]
        stop_on_watch: bool,
        /// Detect hangs: report a CPU whose PC stays in a small window for
        /// ~120 frames (a stuck loop), with its registers and recent events.
        #[arg(long)]
        hang: bool,
        /// Stop when a hang is detected.
        #[arg(long)]
        stop_on_hang: bool,
        /// Output format.
        #[arg(long, value_enum, default_value_t = TraceFormat::Text)]
        format: TraceFormat,
        /// Output file (default: stdout).
        #[arg(short = 'o', long)]
        out: Option<PathBuf>,
        /// ROM set: a `.zip` archive or a directory of loose ROM files.
        path: String,
    },
    /// Replay a recorded input movie and capture the resulting frame — the
    /// gameplay counterpart of `frameshot`, which can only reach attract mode.
    /// The machine and its starting conditions come from the movie.
    Replay {
        /// Movie file (`.phmi`) to replay.
        #[arg(long)]
        movie: PathBuf,
        /// Frames to run. Defaults to the movie's own span; running longer is
        /// allowed and simply continues with no further input.
        #[arg(long)]
        frames: Option<usize>,
        /// Output PNG path (default: `<machine>_f<frames>.png`).
        #[arg(short = 'o', long)]
        out: Option<PathBuf>,
        /// Optional reference PNG to diff the captured frame against.
        #[arg(long)]
        compare: Option<PathBuf>,
        /// ROM set: a `.zip` archive or a directory of loose ROM files.
        path: String,
    },
    /// Inspect and verify input movies.
    Movie {
        #[command(subcommand)]
        cmd: MovieCommand,
    },
    /// Compare two RGB PNGs pixel-by-pixel; print the diff percentage and,
    /// optionally, write a highlight image (differing pixels in red).
    Imgdiff {
        /// First PNG.
        a: PathBuf,
        /// Second PNG.
        b: PathBuf,
        /// Optional highlight-image output path.
        #[arg(short = 'o', long)]
        out: Option<PathBuf>,
        /// Per-pixel channel-sum threshold above which a pixel counts as different.
        #[arg(long, default_value_t = 12)]
        threshold: u32,
    },
    /// List the registered machines (the `--machine` values accepted by
    /// `frameshot`/`trace`/`machine`/`gfxview`), with their ROM-set names.
    Machines,
}

#[derive(Subcommand)]
enum MovieCommand {
    /// Describe a movie: its machine, ROM digest, starting conditions, record
    /// counts per kind, busiest frames and markers.
    ///
    /// Needs neither a ROM set nor the machine — the header is self-describing,
    /// which is what makes a binary format acceptable for the trace itself.
    Info {
        /// Movie file (`.phmi`).
        movie: PathBuf,
    },
    /// Replay a movie and print the resulting frame hash, for CI that has ROMs
    /// but no reference PNG. The hash is the same one `frames.toml` pins, so it
    /// can be compared directly against a golden entry.
    Check {
        /// Movie file (`.phmi`).
        movie: PathBuf,
        /// Frames to run (default: the movie's own span).
        #[arg(long)]
        frames: Option<usize>,
        /// ROM set: a `.zip` archive or a directory of loose ROM files.
        path: String,
    },
}

/// Address-range / instruction-count limits shared by every mode.
///
/// Addresses are absolute (in the same space as `--org`/the region origin).
#[derive(Args)]
struct RangeArgs {
    /// Start disassembling at this address (default: the load origin).
    #[arg(long, value_parser = parse_u32_auto)]
    start: Option<u32>,
    /// Stop at this address, exclusive (hex `0x..` or decimal).
    #[arg(long, value_parser = parse_u32_auto)]
    end: Option<u32>,
    /// Stop after N instructions.
    #[arg(long)]
    count: Option<usize>,
}

/// CPUs with a `phosphor-core` disassembler. Value names are lowercase
/// (`--cpu z80`, `--cpu m68000`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
enum CpuArg {
    I8035,
    Z80,
    M6809,
    M6800,
    M6502,
    M68000,
    Mb88xx,
}

impl From<DisasmCpu> for CpuArg {
    fn from(c: DisasmCpu) -> Self {
        match c {
            DisasmCpu::I8035 => CpuArg::I8035,
            DisasmCpu::Z80 => CpuArg::Z80,
            DisasmCpu::M6809 => CpuArg::M6809,
            DisasmCpu::M6800 => CpuArg::M6800,
            DisasmCpu::M6502 => CpuArg::M6502,
            DisasmCpu::M68000 => CpuArg::M68000,
            DisasmCpu::Mb88xx => CpuArg::Mb88xx,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run_command(cli.command) {
        Ok(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("disasm: {msg}");
            ExitCode::FAILURE
        }
    }
}

fn run_command(cmd: Command) -> Result<String, String> {
    match cmd {
        Command::Raw {
            cpu,
            org,
            range,
            file,
        } => {
            let data =
                std::fs::read(&file).map_err(|e| format!("reading {}: {e}", file.display()))?;
            disassemble(cpu, &data, org, &range)
        }
        Command::Rom {
            cpu,
            org,
            range,
            path,
            member,
        } => {
            let set =
                load_rom_set(&path, &[]).map_err(|e| format!("loading ROM set {path}: {e}"))?;
            let data = set.get(&member).ok_or_else(|| {
                format!(
                    "'{member}' not in ROM set; available: {}",
                    set.file_names().join(", ")
                )
            })?;
            disassemble(cpu, data, org, &range)
        }
        Command::Machine {
            machine,
            region,
            range,
            path,
        } => run_machine(&machine, region.as_deref(), &range, path.as_deref()),
        Command::Gfxview {
            machine,
            region,
            cols,
            scale,
            out,
            path,
        } => run_gfxview(
            &machine,
            region.as_deref(),
            cols,
            scale,
            out.as_deref(),
            path.as_deref(),
        ),
        Command::Frameshot {
            machine,
            frames,
            out,
            compare,
            nvram,
            dump_nvram,
            coin_at,
            audio_out,
            path,
        } => run_frameshot(
            &machine,
            frames,
            out.as_deref(),
            compare.as_deref(),
            nvram.as_deref(),
            dump_nvram.as_deref(),
            coin_at,
            audio_out.as_deref(),
            &path,
        ),
        Command::Trace {
            machine,
            frames,
            from_frame,
            coin_at,
            press,
            motion,
            nvram,
            entropy_file,
            dip,
            events,
            watch,
            cpu,
            break_pc,
            stop_on_watch,
            hang,
            stop_on_hang,
            format,
            out,
            path,
        } => trace::run_trace(trace::TraceOptions {
            frames,
            from_frame,
            coin_at,
            press: press.as_deref(),
            motion: motion.as_deref(),
            nvram: nvram.as_deref(),
            entropy_file: entropy_file.as_deref(),
            dip: dip.as_deref(),
            events: events.as_deref(),
            watch: watch.as_deref(),
            cpu: cpu.as_deref(),
            break_pc: break_pc.as_deref(),
            stop_on_watch,
            hang,
            stop_on_hang,
            format,
            out: out.as_deref(),
            ..trace::TraceOptions::new(&machine, &path)
        }),
        Command::Replay {
            movie,
            frames,
            out,
            compare,
            path,
        } => run_replay(&movie, frames, out.as_deref(), compare.as_deref(), &path),
        Command::Movie { cmd } => match cmd {
            MovieCommand::Info { movie } => run_movie_info(&movie),
            MovieCommand::Check {
                movie,
                frames,
                path,
            } => run_movie_check(&movie, frames, &path),
        },
        Command::Imgdiff {
            a,
            b,
            out,
            threshold,
        } => run_imgdiff(&a, &b, out.as_deref(), threshold),
        Command::Machines => Ok(list_machines()),
    }
}

/// Render the registered machines (name + ROM-set names), one per line,
/// alphabetically. This is the discoverable counterpart to the machine list
/// that an unknown `--machine` error prints.
fn list_machines() -> String {
    let mut entries = registry::all();
    entries.sort_by_key(|e| e.name);
    let mut out = format!("{} registered machines:\n", entries.len());
    for e in entries {
        out.push_str(&format!(
            "  {:<12} roms: {}\n",
            e.name,
            e.rom_names.join(", ")
        ));
    }
    out
}

/// Boot a registered machine, run `frames` frames from reset, and write the
/// rendered frame to a PNG. With `--compare`, also report the pixel diff against
/// a reference image.
#[allow(clippy::too_many_arguments)]
fn run_frameshot(
    machine: &str,
    frames: usize,
    out: Option<&Path>,
    compare: Option<&Path>,
    nvram: Option<&Path>,
    dump_nvram: Option<&Path>,
    coin_at: Option<usize>,
    audio_out: Option<&Path>,
    path: &str,
) -> Result<String, String> {
    let mut harness = Harness::build(machine, path, nvram, coin_at, &[], &[])?;
    for _ in 0..frames {
        harness.run_frame();
    }
    let machine_box = harness.machine_mut();

    // Render native, then apply the machine's declared orientation centrally —
    // mirroring the frontend — so the PNG matches what the cabinet displays for
    // machines that declare a rotation/cocktail flip. Shared with the golden
    // suite so both produce identical bytes.
    let (w, h, buf) = phosphor_harness::render_oriented(machine_box);

    // Drain ALL buffered audio. The resampler is a FIFO, so a single fill only
    // returns the oldest samples — loop until empty to cover the whole run.
    let rate = machine_box.audio_sample_rate();
    let mut audio: Vec<i16> = Vec::new();
    let mut chunk = vec![0i16; (rate as usize).max(1)];
    loop {
        let n = machine_box.fill_audio(&mut chunk);
        if n == 0 {
            break;
        }
        audio.extend_from_slice(&chunk[..n]);
    }
    let n_audio = audio.len();
    let peak = audio.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);

    let out_path = out
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{machine}_f{frames}.png")));
    gfxsheet::write_png(&out_path, &buf, w, h)
        .map_err(|e| format!("writing {}: {e}", out_path.display()))?;

    let mut msg = format!(
        "wrote {machine} frame {frames} -> {} ({w}×{h})\n\
         audio: {n_audio} samples @ {rate} Hz, peak {peak}\n",
        out_path.display()
    );

    // Optionally write the whole run's audio as a WAV (for offline analysis /
    // comparison against a MAME `-wavwrite` capture).
    if let Some(ap) = audio_out {
        write_wav(ap, &audio, rate).map_err(|e| format!("writing audio {}: {e}", ap.display()))?;
        msg.push_str(&format!(
            "audio: wrote {n_audio} samples -> {}\n",
            ap.display()
        ));
    }

    // Optionally dump the machine's NVRAM (e.g. to capture a factory fixture).
    if let Some(dnv) = dump_nvram {
        match machine_box.save_nvram() {
            Some(data) => {
                std::fs::write(dnv, data)
                    .map_err(|e| format!("writing nvram {}: {e}", dnv.display()))?;
                msg.push_str(&format!(
                    "nvram: wrote {} bytes -> {}\n",
                    data.len(),
                    dnv.display()
                ));
            }
            None => msg.push_str("nvram: machine has no NVRAM to dump\n"),
        }
    }

    if let Some(reference) = compare {
        let (rw, rh, rgb) = load_png(reference)?;
        if (rw, rh) != (w, h) {
            return Err(format!(
                "reference {} is {rw}×{rh}, rendered frame is {w}×{h}",
                reference.display()
            ));
        }
        let (ndiff, total) = count_diff(&buf, &rgb, 12);
        msg.push_str(&format!(
            "diff vs {}: {ndiff}/{total} ({:.1}%)\n",
            reference.display(),
            100.0 * ndiff as f64 / total as f64
        ));
    }
    Ok(msg)
}

// ---------------------------------------------------------------------------
// Input movies
// ---------------------------------------------------------------------------

/// Read and decode a movie, reporting the path in any error.
fn load_movie(path: &Path) -> Result<Movie, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("reading movie {}: {e}", path.display()))?;
    Movie::decode(&bytes).map_err(|e| format!("reading movie {}: {e}", path.display()))
}

/// Replay a movie and capture the frame it reaches.
///
/// The gameplay counterpart of `frameshot`. Everything about the machine — which
/// one, its ROM digest, NVRAM, DIP bytes and audio rate — comes from the movie,
/// so the only thing the caller supplies is where the ROMs live.
fn run_replay(
    movie_path: &Path,
    frames: Option<usize>,
    out: Option<&Path>,
    compare: Option<&Path>,
    roms: &str,
) -> Result<String, String> {
    let movie = load_movie(movie_path)?;
    let machine = movie.header.machine.clone();
    let span = movie.header.frames as usize;
    // Running past the movie's span is legitimate — it simply continues with no
    // further input, which is how you look at what a recorded session settles
    // into a few seconds later.
    let frames = frames.unwrap_or(span);

    let mut harness = Harness::from_movie(roms, movie)?;
    for _ in 0..frames {
        harness.run_frame();
    }

    let machine_box = harness.machine_mut();
    let (w, h, buf) = render_oriented(machine_box);

    let out_path = out
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{machine}_f{frames}.png")));
    gfxsheet::write_png(&out_path, &buf, w, h)
        .map_err(|e| format!("writing {}: {e}", out_path.display()))?;

    let mut msg = format!(
        "replayed {machine} {frames} frame(s) (movie spans {span}) -> {} ({w}×{h})\n\
         frame: {}\n",
        out_path.display(),
        hash_frame(w, h, &buf)
    );

    if let Some(reference) = compare {
        let (rw, rh, rgb) = load_png(reference)?;
        if (rw, rh) != (w, h) {
            return Err(format!(
                "reference {} is {rw}×{rh}, replayed frame is {w}×{h}",
                reference.display()
            ));
        }
        let (ndiff, total) = count_diff(&buf, &rgb, 12);
        msg.push_str(&format!(
            "diff vs {}: {ndiff}/{total} ({:.1}%)\n",
            reference.display(),
            100.0 * ndiff as f64 / total as f64
        ));
    }
    Ok(msg)
}

/// Describe a movie without booting anything.
fn run_movie_info(movie_path: &Path) -> Result<String, String> {
    let m = load_movie(movie_path)?;
    let h = &m.header;

    let mut buttons = 0usize;
    let mut absolute = 0usize;
    let mut relative = 0usize;
    let mut release_all = 0usize;
    let mut dips = 0usize;
    let mut markers = 0usize;
    for r in &m.records {
        match r {
            MovieRecord::Button { .. } => buttons += 1,
            MovieRecord::Absolute { .. } => absolute += 1,
            MovieRecord::Relative { .. } => relative += 1,
            MovieRecord::ReleaseAll { .. } => release_all += 1,
            MovieRecord::Dip { .. } => dips += 1,
            MovieRecord::Marker { .. } => markers += 1,
        }
    }

    let mut out = format!(
        "{}\n\
         machine:     {}\n\
         rom digest:  {}\n\
         frames:      {}\n\
         host rate:   {} Hz\n\
         dip bytes:   {}\n\
         nvram:       {}\n\
         controls:    {}{}\n\
         records:     {} total\n",
        movie_path.display(),
        h.machine,
        hex(&h.rom_digest),
        h.frames,
        h.host_sample_rate,
        if h.dip.is_empty() {
            "none".to_string()
        } else {
            h.dip
                .iter()
                .map(|b| format!("{b:#04x}"))
                .collect::<Vec<_>>()
                .join(" ")
        },
        match &h.nvram {
            Some(nv) => format!("{} bytes", nv.len()),
            None => "none".to_string(),
        },
        h.controls.len(),
        if h.controls.is_empty() {
            String::new()
        } else {
            format!(" ({})", h.controls.join(", "))
        },
        m.records.len(),
    );

    // A movie that recorded nothing is almost always a mistake — a capture that
    // was armed but never played, or one whose input never reached the machine.
    // Say so rather than printing an empty breakdown and looking healthy.
    if m.records.is_empty() {
        out.push_str("  (no input was recorded)\n");
    }

    for (label, n) in [
        ("button", buttons),
        ("absolute", absolute),
        ("relative", relative),
        ("release-all", release_all),
        ("dip", dips),
        ("marker", markers),
    ] {
        if n > 0 {
            out.push_str(&format!("  {label:<12} {n}\n"));
        }
    }

    // The busiest frames are what a human wants to see: they are where the
    // player was actually doing something.
    let mut hist = m.frame_histogram();
    if !hist.is_empty() {
        let frames_with_input = hist.len();
        hist.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        out.push_str(&format!(
            "input on {frames_with_input} of {} frame(s); busiest:\n",
            h.frames
        ));
        for (frame, n) in hist.iter().take(5) {
            out.push_str(&format!("  frame {frame:<8} {n} record(s)\n"));
        }
    }

    let markers: Vec<(u32, &str)> = m.markers().collect();
    if !markers.is_empty() {
        out.push_str("markers:\n");
        for (frame, label) in markers {
            out.push_str(&format!("  frame {frame:<8} {label}\n"));
        }
    }
    Ok(out)
}

/// Replay a movie and print the frame fingerprint, for CI without a reference
/// PNG. The hash is the one `frames.toml` pins, so it compares directly.
fn run_movie_check(movie_path: &Path, frames: Option<usize>, roms: &str) -> Result<String, String> {
    let movie = load_movie(movie_path)?;
    let machine = movie.header.machine.clone();
    let span = movie.header.frames as usize;
    let frames = frames.unwrap_or(span);

    let mut harness = Harness::from_movie(roms, movie)?;
    for _ in 0..frames {
        harness.run_frame();
    }

    let machine_box = harness.machine_mut();
    let (w, h, buf) = render_oriented(machine_box);
    let mut msg = format!(
        "{machine} @ frame {frames} ({w}×{h})\nframe:   {}\n",
        hash_frame(w, h, &buf)
    );
    // For the vector games the display list, not the rasterised frame, is what
    // the frontend draws — so report both, exactly as the golden suite pins both.
    if let Some(lines) = machine_box.vector_display_list() {
        msg.push_str(&format!(
            "vectors: {} ({} line(s))\n",
            hash_vectors(lines),
            lines.len()
        ));
    }
    Ok(msg)
}

/// Write 16-bit mono PCM samples as a WAV file.
fn write_wav(path: &Path, samples: &[i16], rate: u32) -> std::io::Result<()> {
    use std::io::Write;
    let data_len = (samples.len() * 2) as u32;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?; // PCM fmt chunk size
    f.write_all(&1u16.to_le_bytes())?; // audio format: PCM
    f.write_all(&1u16.to_le_bytes())?; // channels: mono
    f.write_all(&rate.to_le_bytes())?; // sample rate
    f.write_all(&(rate * 2).to_le_bytes())?; // byte rate
    f.write_all(&2u16.to_le_bytes())?; // block align
    f.write_all(&16u16.to_le_bytes())?; // bits per sample
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    f.flush()
}

/// Compare two RGB PNGs and (optionally) write a highlight image.
fn run_imgdiff(a: &Path, b: &Path, out: Option<&Path>, threshold: u32) -> Result<String, String> {
    let (aw, ah, ap) = load_png(a)?;
    let (bw, bh, bp) = load_png(b)?;
    if (aw, ah) != (bw, bh) {
        return Err(format!("size mismatch: {aw}×{ah} vs {bw}×{bh}"));
    }
    let (ndiff, total) = count_diff(&ap, &bp, threshold);

    if let Some(out_path) = out {
        // Dim the matching pixels, paint differing pixels solid red.
        let mut hi = vec![0u8; ap.len()];
        for (i, (pa, pb)) in ap.chunks_exact(3).zip(bp.chunks_exact(3)).enumerate() {
            if channel_sum_delta(pa, pb) > threshold {
                hi[i * 3] = 255;
            } else {
                hi[i * 3] = pa[0] / 3;
                hi[i * 3 + 1] = pa[1] / 3;
                hi[i * 3 + 2] = pa[2] / 3;
            }
        }
        gfxsheet::write_png(out_path, &hi, aw, ah)
            .map_err(|e| format!("writing {}: {e}", out_path.display()))?;
    }

    Ok(format!(
        "diff: {ndiff}/{total} ({:.2}%)\n",
        100.0 * ndiff as f64 / total as f64
    ))
}

#[inline]
fn channel_sum_delta(a: &[u8], b: &[u8]) -> u32 {
    (a[0].abs_diff(b[0]) as u32) + (a[1].abs_diff(b[1]) as u32) + (a[2].abs_diff(b[2]) as u32)
}

/// Count pixels whose per-channel absolute-difference sum exceeds `threshold`.
fn count_diff(a: &[u8], b: &[u8], threshold: u32) -> (usize, usize) {
    let total = a.len() / 3;
    let ndiff = a
        .chunks_exact(3)
        .zip(b.chunks_exact(3))
        .filter(|(pa, pb)| channel_sum_delta(pa, pb) > threshold)
        .count();
    (ndiff, total)
}

/// Decode an 8-bit RGB/RGBA PNG into a tightly-packed RGB byte buffer.
fn load_png(path: &Path) -> Result<(u32, u32, Vec<u8>), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("opening {}: {e}", path.display()))?;
    let mut reader = png::Decoder::new(file)
        .read_info()
        .map_err(|e| format!("reading {}: {e}", path.display()))?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("decoding {}: {e}", path.display()))?;
    let channels = match info.color_type {
        png::ColorType::Rgb => 3,
        png::ColorType::Rgba => 4,
        other => {
            return Err(format!(
                "{}: unsupported color type {other:?}",
                path.display()
            ));
        }
    };
    let mut rgb = Vec::with_capacity((info.width * info.height * 3) as usize);
    for px in buf[..info.buffer_size()].chunks_exact(channels) {
        rgb.extend_from_slice(&px[..3]);
    }
    Ok((info.width, info.height, rgb))
}

fn run_machine(
    machine: &str,
    region: Option<&str>,
    range: &RangeArgs,
    path: Option<&str>,
) -> Result<String, String> {
    // With no --region, list what's available (no ROM files required).
    let Some(region) = region else {
        return Ok(list_regions(machine));
    };

    let r = disasm_registry::find(machine, region).ok_or_else(|| {
        let avail: Vec<&str> = disasm_registry::regions_for(machine)
            .iter()
            .map(|x| x.region)
            .collect();
        if avail.is_empty() {
            format!("no disasm regions registered for machine '{machine}'")
        } else {
            format!(
                "machine '{machine}' has no region '{region}'; available: {}",
                avail.join(", ")
            )
        }
    })?;

    let path = path.ok_or("a ROM path is required to disassemble a region")?;

    // The machine registry knows the MAME ZIP names to look for in a rompath dir.
    let rom_names: Vec<&str> = registry::find(machine)
        .map(|e| e.rom_names.to_vec())
        .unwrap_or_default();

    let set = load_rom_set(path, &rom_names).map_err(|e| format!("loading ROM set {path}: {e}"))?;
    let data = (r.load)(&set).map_err(|e| format!("assembling region '{region}': {e}"))?;
    disassemble(CpuArg::from(r.cpu), &data, r.org, range)
}

/// Render the available disasm regions for `machine`, with CPU/origin/size.
fn list_regions(machine: &str) -> String {
    let regions = disasm_registry::regions_for(machine);
    if regions.is_empty() {
        return format!("no disasm regions registered for machine '{machine}'\n");
    }
    let mut out = format!("disasm regions for '{machine}':\n");
    for r in regions {
        out.push_str(&format!(
            "  {:<8} {:<6} org 0x{:04X}  {} bytes (0x{:X})\n",
            r.region,
            r.cpu.name(),
            r.org,
            r.size,
            r.size,
        ));
    }
    out
}

/// Decode a machine's GFX region to a PNG sheet (or list regions with no `--region`).
///
/// Mirrors [`run_machine`]: resolve the region via [`gfx_registry`], reuse
/// [`load_rom_set`] with the machine's registry ROM names, assemble the bytes,
/// then decode + composite + write. The palette comes from the region's color
/// PROM when it has one, else a grayscale ramp sized to the layout's bit depth.
fn run_gfxview(
    machine: &str,
    region: Option<&str>,
    cols: usize,
    scale: usize,
    out: Option<&Path>,
    path: Option<&str>,
) -> Result<String, String> {
    // With no --region, list what's available (no ROM files required).
    let Some(region) = region else {
        return Ok(list_gfx_regions(machine));
    };

    let r = gfx_registry::find(machine, region).ok_or_else(|| {
        let avail: Vec<&str> = gfx_registry::regions_for(machine)
            .iter()
            .map(|x| x.region)
            .collect();
        if avail.is_empty() {
            format!("no gfx regions registered for machine '{machine}'")
        } else {
            format!(
                "machine '{machine}' has no gfx region '{region}'; available: {}",
                avail.join(", ")
            )
        }
    })?;

    let path = path.ok_or("a ROM path is required to decode a region")?;

    // The machine registry knows the MAME ZIP names to look for in a rompath dir.
    let rom_names: Vec<&str> = registry::find(machine)
        .map(|e| e.rom_names.to_vec())
        .unwrap_or_default();

    let set = load_rom_set(path, &rom_names).map_err(|e| format!("loading ROM set {path}: {e}"))?;
    let bytes = (r.load)(&set).map_err(|e| format!("assembling gfx region '{region}': {e}"))?;

    let cache = decode_gfx(&bytes, 0, r.count as usize, r.layout);

    // Palette from the color PROM where the region has one; otherwise a
    // grayscale ramp with one level per bit-plane combination (2^planes).
    let palette = match r.palette {
        Some(build) => build(&set).map_err(|e| format!("building palette for '{region}': {e}"))?,
        None => gfxsheet::grayscale_ramp(1 << r.layout.plane_offsets.len()),
    };

    let sheet = gfxsheet::render_sheet(&cache, &palette, &SheetConfig { cols, scale });

    let out_path = out
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{machine}_{region}.png")));
    gfxsheet::write_png(&out_path, &sheet.rgb, sheet.width, sheet.height)
        .map_err(|e| format!("writing {}: {e}", out_path.display()))?;

    Ok(format!(
        "wrote {region} sheet: {} tiles ({}×{} each) -> {} ({}×{})\n",
        r.count,
        r.width,
        r.height,
        out_path.display(),
        sheet.width,
        sheet.height,
    ))
}

/// Render the available gfx regions for `machine`, with dimensions and palette.
fn list_gfx_regions(machine: &str) -> String {
    let regions = gfx_registry::regions_for(machine);
    if regions.is_empty() {
        return format!("no gfx regions registered for machine '{machine}'\n");
    }
    let mut out = format!("gfx regions for '{machine}':\n");
    for r in regions {
        out.push_str(&format!(
            "  {:<8} {:>3}×{:<3}  {:>4} tiles  {}\n",
            r.region,
            r.width,
            r.height,
            r.count,
            if r.palette.is_some() {
                "PROM palette"
            } else {
                "grayscale"
            },
        ));
    }
    out
}

/// Validate the range against `org`/`data`, then disassemble.
fn disassemble(cpu: CpuArg, data: &[u8], org: u32, range: &RangeArgs) -> Result<String, String> {
    let start = range.start.unwrap_or(org);
    if start < org {
        return Err(format!(
            "--start 0x{start:04X} is below the load origin 0x{org:04X}"
        ));
    }
    if let Some(end) = range.end
        && end <= start
    {
        return Err(format!(
            "--end 0x{end:04X} must be greater than --start 0x{start:04X}"
        ));
    }
    let begin = (start - org) as usize;
    if begin > data.len() {
        return Err(format!(
            "--start 0x{start:04X} is past the end of the region (org 0x{org:04X}, {} bytes)",
            data.len()
        ));
    }
    Ok(dispatch(cpu, data, org, start, range.end, range.count))
}

/// Pick the concrete CPU type and disassemble.
fn dispatch(
    cpu: CpuArg,
    data: &[u8],
    org: u32,
    start: u32,
    end: Option<u32>,
    count: Option<usize>,
) -> String {
    match cpu {
        CpuArg::I8035 => run::<I8035>(data, org, start, end, count),
        CpuArg::Z80 => run::<Z80>(data, org, start, end, count),
        CpuArg::M6809 => run::<M6809>(data, org, start, end, count),
        CpuArg::M6800 => run::<M6800>(data, org, start, end, count),
        CpuArg::M6502 => run::<M6502>(data, org, start, end, count),
        CpuArg::M68000 => run::<M68000>(data, org, start, end, count),
        CpuArg::Mb88xx => run::<Mb88xx>(data, org, start, end, count),
    }
}

/// Disassemble `data` one instruction at a time, returning the listing.
///
/// Disassembly begins at `start` (byte offset `start - org`) and runs until EOF,
/// the `end` address (exclusive), or `count` instructions — whichever comes
/// first. Each line is `AADDR  HEX BYTES  MNEMONIC operands`. The byte slice
/// handed to the disassembler shrinks toward EOF; the per-CPU disassemblers
/// already return `"???"` for a too-short slice, and the `.max(1)` step guards
/// against any zero-length result wedging the loop. The caller (`disassemble`)
/// guarantees `org <= start` and `start - org <= data.len()`.
fn run<T: Disassemble>(
    data: &[u8],
    org: u32,
    start: u32,
    end: Option<u32>,
    count: Option<usize>,
) -> String {
    let mut out = String::new();
    let mut offset = (start - org) as usize;
    let mut emitted = 0usize;
    while offset < data.len() {
        let addr = org.wrapping_add(offset as u32);
        if end.is_some_and(|e| addr >= e) {
            break;
        }
        if count.is_some_and(|max| emitted >= max) {
            break;
        }
        let insn = T::disassemble(addr, &data[offset..]);
        let len = (insn.byte_len.max(1)) as usize;
        let stop = (offset + len).min(data.len());
        let hex = hex_bytes(&data[offset..stop]);
        out.push_str(&format!("{addr:06X}  {hex:<23} {insn}\n"));
        offset += len;
        emitted += 1;
    }
    out
}

/// Parse a `u32` accepting a `0x`/`0X` hex prefix or plain decimal.
fn parse_u32_auto(s: &str) -> Result<u32, String> {
    let t = s.trim();
    let (radix, digits) = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => (16, hex),
        None => (10, t),
    };
    u32::from_str_radix(digits, radix).map_err(|e| format!("invalid number '{s}': {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole ROM, no range/count limits.
    fn all<T: Disassemble>(data: &[u8], org: u32) -> String {
        run::<T>(data, org, org, None, None)
    }

    #[test]
    fn parse_u32_hex_and_decimal() {
        assert_eq!(parse_u32_auto("0x1000").unwrap(), 4096);
        assert_eq!(parse_u32_auto("0X1000").unwrap(), 4096);
        assert_eq!(parse_u32_auto("4096").unwrap(), 4096);
        assert_eq!(parse_u32_auto("0").unwrap(), 0);
        assert!(parse_u32_auto("zzz").is_err());
    }

    #[test]
    fn m6809_known_instruction() {
        // Matches core/tests/m6809_disasm_test.rs: LDA #$42.
        let out = all::<M6809>(&[0x86, 0x42], 0x1000);
        assert!(out.contains("001000"), "address column: {out}");
        assert!(out.contains("86 42"), "hex bytes: {out}");
        assert!(out.contains("LDA"), "mnemonic: {out}");
        assert!(out.contains("#$42"), "operand: {out}");
    }

    #[test]
    fn steps_by_instruction_length_and_advances_addr() {
        // LDA #$42 (2 bytes), NOP (1 byte) → two lines, addresses 0 then 2.
        let out = all::<M6809>(&[0x86, 0x42, 0x12], 0x0000);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "expected two instructions: {out}");
        assert!(lines[0].starts_with("000000"));
        assert!(lines[1].starts_with("000002"));
    }

    #[test]
    fn count_limits_output() {
        let out = run::<M6809>(&[0x12, 0x12, 0x12, 0x12], 0, 0, None, Some(2));
        assert_eq!(out.lines().count(), 2);
    }

    #[test]
    fn start_skips_to_address() {
        // org 0x1000; --start 0x1002 skips the first 2-byte instruction.
        let out = run::<M6809>(&[0x86, 0x42, 0x12, 0x12], 0x1000, 0x1002, None, None);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "two NOPs from 0x1002: {out}");
        assert!(lines[0].starts_with("001002"));
        assert!(lines[1].starts_with("001003"));
    }

    #[test]
    fn end_stops_disassembly() {
        // end 0x0002 (exclusive) → only the instruction starting at 0x0000.
        let out = run::<M6809>(&[0x12, 0x12, 0x12], 0, 0, Some(0x0002), None);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "addrs 0 and 1 are < end 2: {out}");
        assert!(lines.last().unwrap().starts_with("000001"));
    }

    #[test]
    fn range_validation_errors() {
        let range = |start, end| RangeArgs {
            start,
            end,
            count: None,
        };
        // start below org
        assert!(
            disassemble(
                CpuArg::M6809,
                &[0x12; 4],
                0x1000,
                &range(Some(0x0FFF), None)
            )
            .is_err()
        );
        // end <= start
        assert!(
            disassemble(
                CpuArg::M6809,
                &[0x12; 4],
                0x1000,
                &range(Some(0x1002), Some(0x1002))
            )
            .is_err()
        );
        // start past end of region
        assert!(
            disassemble(
                CpuArg::M6809,
                &[0x12; 4],
                0x1000,
                &range(Some(0x2000), None)
            )
            .is_err()
        );
        // valid
        assert!(disassemble(CpuArg::M6809, &[0x12; 4], 0x1000, &range(None, None)).is_ok());
    }

    #[test]
    fn short_and_garbage_slice_terminates() {
        // A single trailing byte must not loop forever.
        let out = all::<Z80>(&[0xFF], 0);
        assert_eq!(out.lines().count(), 1);
    }

    #[test]
    fn dispatch_covers_every_cpu() {
        for cpu in [
            CpuArg::I8035,
            CpuArg::Z80,
            CpuArg::M6809,
            CpuArg::M6800,
            CpuArg::M6502,
            CpuArg::M68000,
            CpuArg::Mb88xx,
        ] {
            // Should produce at least one line and never panic.
            let out = dispatch(cpu, &[0x00, 0x00, 0x00, 0x00], 0, 0, None, Some(1));
            assert!(!out.is_empty(), "{cpu:?} produced no output");
        }
    }

    #[test]
    fn machine_mode_resolves_mario_sound_cpu() {
        // Registry wiring smoke test — no ROM files needed.
        let r = disasm_registry::find("mariobros", "sound").expect("region registered");
        assert_eq!(CpuArg::from(r.cpu), CpuArg::I8035);
    }

    #[test]
    fn machines_command_lists_registered_names_and_roms() {
        let out = list_machines();
        // A few known machines and their ROM-set names appear, sorted.
        assert!(out.contains("joust"), "{out}");
        assert!(out.contains("mariobros"), "{out}");
        assert!(out.contains("esb"), "{out}");
        assert!(out.contains("registered machines"), "{out}");
        // Names are alphabetized.
        let names: Vec<&str> = out
            .lines()
            .skip(1)
            .filter_map(|l| l.split_whitespace().next())
            .collect();
        assert!(
            names.windows(2).all(|w| w[0] <= w[1]),
            "machines must be sorted: {names:?}"
        );
    }

    #[test]
    fn list_regions_reports_detail_without_roms() {
        // Listing works with no ROM path (uses the registry's size field).
        let out = list_regions("ccastles");
        assert!(out.contains("bank0"), "{out}");
        assert!(out.contains("bank1"), "{out}");
        assert!(out.contains("fixed"), "{out}");
        assert!(out.contains("m6502"), "{out}");
        assert!(out.contains("0xA000"), "bank origin: {out}");
        assert!(out.contains("0x4000"), "bank size: {out}");

        // run_machine with no region and no path just lists.
        let listed = run_machine(
            "mariobros",
            None,
            &RangeArgs {
                start: None,
                end: None,
                count: None,
            },
            None,
        )
        .unwrap();
        assert!(
            listed.contains("main") && listed.contains("sound"),
            "{listed}"
        );
    }

    #[test]
    fn gfxview_unknown_machine_lists_nothing() {
        // No --region → listing path; unknown machine has no gfx regions.
        let out = run_gfxview("does-not-exist", None, 16, 1, None, None).unwrap();
        assert!(out.contains("no gfx regions registered"), "{out}");
        assert_eq!(list_gfx_regions("does-not-exist"), out);
    }

    #[test]
    fn gfxview_unknown_region_errors_helpfully() {
        // Decoding an unregistered region fails before any ROM is required.
        let err = run_gfxview("does-not-exist", Some("sprites"), 16, 1, None, None).unwrap_err();
        assert!(err.contains("no gfx regions registered"), "{err}");
    }
}
