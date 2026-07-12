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

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use phosphor_core::cpu::Disassemble;
use phosphor_core::cpu::{I8035, M6502, M6800, M6809, M68000, Mb88xx, Z80};
use phosphor_core::gfx::decode::decode_gfx;
use phosphor_machines::disasm_registry::{self, DisasmCpu};
use phosphor_machines::gfx_registry;
use phosphor_machines::registry;
use phosphor_machines::rom_loader::{RomLoadError, RomSet};

mod gfxsheet;
use gfxsheet::SheetConfig;

#[derive(Parser)]
#[command(
    name = "disasm",
    about = "Disassemble a ROM with a chosen CPU (phosphor-core disassemblers)"
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
        /// ROM set: a `.zip` archive or a directory of loose ROM files.
        path: String,
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
            path,
        } => run_frameshot(&machine, frames, out.as_deref(), compare.as_deref(), &path),
        Command::Imgdiff {
            a,
            b,
            out,
            threshold,
        } => run_imgdiff(&a, &b, out.as_deref(), threshold),
    }
}

/// Boot a registered machine, run `frames` frames from reset, and write the
/// rendered frame to a PNG. With `--compare`, also report the pixel diff against
/// a reference image.
fn run_frameshot(
    machine: &str,
    frames: usize,
    out: Option<&Path>,
    compare: Option<&Path>,
    path: &str,
) -> Result<String, String> {
    let entry = registry::find(machine).ok_or_else(|| {
        let avail: Vec<&str> = registry::all().iter().map(|e| e.name).collect();
        format!("unknown machine '{machine}'; available: {}", avail.join(", "))
    })?;

    let set = load_rom_set(path, entry.rom_names)
        .map_err(|e| format!("loading ROM set {path}: {e}"))?;
    let mut machine_box =
        (entry.create)(&set).map_err(|e| format!("creating machine '{machine}': {e}"))?;

    machine_box.reset();
    for _ in 0..frames {
        machine_box.run_frame();
    }

    let (w, h) = machine_box.display_size();
    let mut buf = vec![0u8; (w * h * 3) as usize];
    machine_box.render_frame(&mut buf);

    let out_path = out
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("{machine}_f{frames}.png")));
    gfxsheet::write_png(&out_path, &buf, w, h)
        .map_err(|e| format!("writing {}: {e}", out_path.display()))?;

    let mut msg = format!(
        "wrote {machine} frame {frames} -> {} ({w}×{h})\n",
        out_path.display()
    );
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

/// Compare two RGB PNGs and (optionally) write a highlight image.
fn run_imgdiff(
    a: &Path,
    b: &Path,
    out: Option<&Path>,
    threshold: u32,
) -> Result<String, String> {
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
        other => return Err(format!("{}: unsupported color type {other:?}", path.display())),
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
        let hex: Vec<String> = data[offset..stop]
            .iter()
            .map(|b| format!("{b:02X}"))
            .collect();
        out.push_str(&format!("{addr:06X}  {:<23} {insn}\n", hex.join(" ")));
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

/// Resolve a ROM-set path into a [`RomSet`].
///
/// Adapted from the frontend's `rom_path::load_rom_set`. Resolution order:
/// 1. `path` ends with `.zip` → load that archive.
/// 2. `path` is a directory containing `{rom_name}.zip` (any provided name) → load it.
/// 3. `path` is a directory of loose files → [`RomSet::from_directory`].
fn load_rom_set(path: &str, rom_names: &[&str]) -> Result<RomSet, RomLoadError> {
    let p = Path::new(path);

    if p.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("zip"))
    {
        return load_from_zip(p);
    }

    if p.is_dir() {
        for name in rom_names {
            let zip_path = p.join(format!("{name}.zip"));
            if zip_path.exists() {
                return load_from_zip(&zip_path);
            }
        }
        return RomSet::from_directory(p);
    }

    Err(RomLoadError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("ROM path not found: {}", p.display()),
    )))
}

/// Extract every file from a ZIP archive into a [`RomSet`].
fn load_from_zip(path: &Path) -> Result<RomSet, RomLoadError> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let mut archive = zip::ZipArchive::new(reader).map_err(zip_err)?;

    let mut entries = Vec::with_capacity(archive.len());
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(zip_err)?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let mut data = Vec::with_capacity(entry.size() as usize);
        entry.read_to_end(&mut data)?;
        entries.push((name, data));
    }
    Ok(RomSet::from_entries(entries))
}

fn zip_err(e: zip::result::ZipError) -> RomLoadError {
    RomLoadError::Io(std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("ZIP error: {e}"),
    ))
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
