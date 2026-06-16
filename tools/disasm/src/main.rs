//! Standalone CPU disassembler CLI.
//!
//! Disassembles a ROM with one of the per-CPU disassemblers in `phosphor-core`,
//! without spinning up the SDL/egui debug UI. Useful for inspecting sound/CPU
//! ROMs (e.g. the Mario Bros 8049 sound program) from the terminal.
//!
//! Three input modes:
//! - `raw`     — a raw, already-extracted ROM file + an explicit `--cpu`.
//! - `rom`     — a `.zip`/directory ROM set + a member filename + `--cpu`.
//! - `machine` — a machine name + region; CPU and origin are resolved from the
//!   [`phosphor_machines::disasm_registry`].

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use phosphor_core::cpu::Disassemble;
use phosphor_core::cpu::{I8035, M6502, M6800, M6809, M68000, Mb88xx, Z80};
use phosphor_machines::disasm_registry::{self, DisasmCpu};
use phosphor_machines::registry;
use phosphor_machines::rom_loader::{RomLoadError, RomSet};

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
        /// Stop after N instructions.
        #[arg(long)]
        count: Option<usize>,
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
        /// Stop after N instructions.
        #[arg(long)]
        count: Option<usize>,
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
        /// Stop after N instructions.
        #[arg(long)]
        count: Option<usize>,
        /// ROM set: a `.zip` archive or a directory of loose ROM files.
        path: String,
    },
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
            count,
            file,
        } => {
            let data =
                std::fs::read(&file).map_err(|e| format!("reading {}: {e}", file.display()))?;
            Ok(dispatch(cpu, &data, org, count))
        }
        Command::Rom {
            cpu,
            org,
            count,
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
            Ok(dispatch(cpu, data, org, count))
        }
        Command::Machine {
            machine,
            region,
            count,
            path,
        } => run_machine(&machine, region.as_deref(), count, &path),
    }
}

fn run_machine(
    machine: &str,
    region: Option<&str>,
    count: Option<usize>,
    path: &str,
) -> Result<String, String> {
    // With no --region, list what's available rather than guessing.
    let Some(region) = region else {
        let regions = disasm_registry::regions_for(machine);
        if regions.is_empty() {
            return Err(format!(
                "no disasm regions registered for machine '{machine}'"
            ));
        }
        let mut out = format!("disasm regions for '{machine}':\n");
        for r in regions {
            out.push_str(&format!(
                "  {:<8} {:<6} org 0x{:04X}\n",
                r.region,
                r.cpu.name(),
                r.org
            ));
        }
        return Ok(out);
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

    // The machine registry knows the MAME ZIP names to look for in a rompath dir.
    let rom_names: Vec<&str> = registry::find(machine)
        .map(|e| e.rom_names.to_vec())
        .unwrap_or_default();

    let set = load_rom_set(path, &rom_names).map_err(|e| format!("loading ROM set {path}: {e}"))?;
    let data = (r.load)(&set).map_err(|e| format!("assembling region '{region}': {e}"))?;
    Ok(dispatch(CpuArg::from(r.cpu), &data, r.org, count))
}

/// Pick the concrete CPU type and disassemble.
fn dispatch(cpu: CpuArg, data: &[u8], org: u32, count: Option<usize>) -> String {
    match cpu {
        CpuArg::I8035 => run::<I8035>(data, org, count),
        CpuArg::Z80 => run::<Z80>(data, org, count),
        CpuArg::M6809 => run::<M6809>(data, org, count),
        CpuArg::M6800 => run::<M6800>(data, org, count),
        CpuArg::M6502 => run::<M6502>(data, org, count),
        CpuArg::M68000 => run::<M68000>(data, org, count),
        CpuArg::Mb88xx => run::<Mb88xx>(data, org, count),
    }
}

/// Disassemble `data` one instruction at a time, returning the listing.
///
/// Each line is `AADDR  HEX BYTES  MNEMONIC operands`. The byte slice handed to
/// the disassembler shrinks toward EOF; the per-CPU disassemblers already return
/// `"???"` for a too-short slice, and the `.max(1)` step guards against any
/// zero-length result wedging the loop.
fn run<T: Disassemble>(data: &[u8], org: u32, count: Option<usize>) -> String {
    let mut out = String::new();
    let mut offset = 0usize;
    let mut emitted = 0usize;
    while offset < data.len() {
        if count.is_some_and(|max| emitted >= max) {
            break;
        }
        let addr = org.wrapping_add(offset as u32);
        let insn = T::disassemble(addr, &data[offset..]);
        let len = (insn.byte_len.max(1)) as usize;
        let end = (offset + len).min(data.len());
        let hex: Vec<String> = data[offset..end]
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
        let out = run::<M6809>(&[0x86, 0x42], 0x1000, None);
        assert!(out.contains("001000"), "address column: {out}");
        assert!(out.contains("86 42"), "hex bytes: {out}");
        assert!(out.contains("LDA"), "mnemonic: {out}");
        assert!(out.contains("#$42"), "operand: {out}");
    }

    #[test]
    fn steps_by_instruction_length_and_advances_addr() {
        // LDA #$42 (2 bytes), NOP (1 byte) → two lines, addresses 0 then 2.
        let out = run::<M6809>(&[0x86, 0x42, 0x12], 0x0000, None);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2, "expected two instructions: {out}");
        assert!(lines[0].starts_with("000000"));
        assert!(lines[1].starts_with("000002"));
    }

    #[test]
    fn count_limits_output() {
        let out = run::<M6809>(&[0x12, 0x12, 0x12, 0x12], 0, Some(2));
        assert_eq!(out.lines().count(), 2);
    }

    #[test]
    fn short_and_garbage_slice_terminates() {
        // A single trailing byte must not loop forever.
        let out = run::<Z80>(&[0xFF], 0, None);
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
            let out = dispatch(cpu, &[0x00, 0x00, 0x00, 0x00], 0, Some(1));
            assert!(!out.is_empty(), "{cpu:?} produced no output");
        }
    }

    #[test]
    fn machine_mode_resolves_mario_sound_cpu() {
        // Registry wiring smoke test — no ROM files needed.
        let r = disasm_registry::find("mariobros", "sound").expect("region registered");
        assert_eq!(CpuArg::from(r.cpu), CpuArg::I8035);
    }
}
