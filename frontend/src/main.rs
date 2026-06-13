use clap::Parser;
use phosphor_machines::registry;

mod audio;
mod config;
mod debug_ui;
mod emulator;
mod input;
mod overlay;
mod profile;
mod rom_path;
mod screenshot;
mod state;
mod vector_gl;
mod video;

#[derive(Parser)]
#[command(name = "phosphor", about = "Cycle-accurate arcade machine emulator")]
struct Cli {
    /// Machine to emulate (e.g., joust, pacman, robotron)
    machine: Option<String>,

    /// Path to ROM file or directory (overrides config.toml rom_path)
    rom_path: Option<String>,

    /// Window scale factor
    #[arg(long)]
    scale: Option<u32>,

    /// Start with debug UI visible
    #[arg(long)]
    debug: bool,

    /// Start with frame profiler visible
    #[arg(long)]
    profile: bool,

    /// Disable automatic mouse grab for analog input
    #[arg(long)]
    no_mouse_grab: bool,

    /// List available machines and exit
    #[arg(long, short)]
    list: bool,
}

fn main() {
    let cli = Cli::parse();
    let config = config::load();

    if cli.list {
        for entry in registry::all() {
            println!("{}", entry.name);
        }
        return;
    }

    let Some(machine_name) = cli.machine else {
        Cli::parse_from(["phosphor", "--help"]);
        unreachable!();
    };

    let entry = registry::find(&machine_name).unwrap_or_else(|| {
        let names: Vec<_> = registry::all().iter().map(|e| e.name).collect();
        eprintln!("Unknown machine: {machine_name}");
        eprintln!("Available: {}", names.join(", "));
        std::process::exit(1);
    });

    let rom_path = cli.rom_path.or(config.rom_path.clone()).unwrap_or_else(|| {
        eprintln!("ROM path required. Either:");
        eprintln!("  phosphor {machine_name} /path/to/roms");
        if let Some(dir) = config::config_dir() {
            eprintln!("  or set rom_path in {}", dir.join("config.toml").display());
        }
        std::process::exit(1);
    });

    let mut machine = create_from_first_rom_set(entry, &rom_path);

    // Load battery-backed NVRAM from disk (if available)
    let nvram_path = nvram_path_for(&config, &machine_name);
    if let Ok(data) = std::fs::read(&nvram_path) {
        machine.load_nvram(&data);
    }

    let (native_w, native_h) = machine.display_size();
    let scale = cli
        .scale
        .or(config.scale)
        .unwrap_or_else(|| auto_scale(native_w, native_h));

    let save_path = save_path_for(&config, &machine_name);
    let screenshot_dir = screenshot_dir();
    let mut state = state::load();

    // Build input bindings from machine defaults, then overlay any persisted
    // per-machine overrides (only machines with typed controls participate).
    let mut bindings = input::build_bindings(machine.as_ref());
    let machine_id = machine.machine_id().to_string();
    let has_typed_controls = !machine.input_controls().is_empty();
    if has_typed_controls && let Some(saved) = state.input_bindings.get(&machine_id) {
        bindings.apply_overrides(machine.input_controls(), saved);
    }

    machine.reset();
    emulator::run(
        machine.as_mut(),
        &bindings,
        scale,
        &save_path,
        &screenshot_dir,
        &machine_name,
        cli.debug,
        cli.profile,
        cli.no_mouse_grab,
        &mut state,
    );

    // Persist input bindings, but only when they differ from the machine
    // defaults (keeps state.toml free of redundant default entries).
    if has_typed_controls {
        let controls = machine.input_controls();
        let current = bindings.to_serialized(controls);
        let default = input::build_bindings(machine.as_ref()).to_serialized(controls);
        if input::bindings_eq(&current, &default) {
            state.input_bindings.remove(&machine_id);
        } else {
            state.input_bindings.insert(machine_id.clone(), current);
        }
    }
    state::save(&state);

    // Save battery-backed NVRAM to disk on exit
    if let Some(data) = machine.save_nvram()
        && let Err(e) = std::fs::write(&nvram_path, data)
    {
        eprintln!("Warning: failed to save NVRAM: {e}");
    }
}

fn default_data_dir(subdir: &str) -> std::path::PathBuf {
    config::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from(".phosphor"))
        .join(subdir)
}

fn ensure_dir(dir: &std::path::Path) {
    std::fs::create_dir_all(dir).ok();
}

fn save_path_for(config: &config::Config, machine_name: &str) -> std::path::PathBuf {
    let dir = config
        .save_path
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| default_data_dir("save"));
    ensure_dir(&dir);
    dir.join(format!("{machine_name}.sav"))
}

fn nvram_path_for(config: &config::Config, machine_name: &str) -> std::path::PathBuf {
    let dir = config
        .nvram_path
        .as_ref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| default_data_dir("nvram"));
    ensure_dir(&dir);
    dir.join(format!("{machine_name}.nvram"))
}

fn screenshot_dir() -> std::path::PathBuf {
    let dir = default_data_dir("screenshots");
    ensure_dir(&dir);
    dir
}

/// Try each ROM set name in order, returning the first machine that
/// initialises successfully. This ensures that ROM loading *and* machine
/// creation (which validates CRC32s, sizes, and ROM_CONTINUE layouts) both
/// succeed before we commit to a ROM set.
fn create_from_first_rom_set(
    entry: &phosphor_machines::registry::MachineEntry,
    path: &str,
) -> Box<dyn phosphor_core::core::machine::FrontendMachine> {
    let mut last_err = None;
    for name in entry.rom_names {
        let rom_set = match rom_path::load_rom_set(name, path) {
            Ok(set) => set,
            Err(e) => {
                last_err = Some(e);
                continue;
            }
        };
        match (entry.create)(&rom_set) {
            Ok(machine) => return machine,
            Err(e) => last_err = Some(e),
        }
    }
    let err = last_err.unwrap_or_else(|| {
        phosphor_machines::rom_loader::RomLoadError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no ROM names configured",
        ))
    });
    eprintln!("Failed to load ROMs: {err}");
    eprintln!("Tried: {}", entry.rom_names.join(", "));
    std::process::exit(1);
}

/// Pick the largest integer scale that keeps the window under 1200 pixels
/// on its longest axis (fits comfortably on most displays).
fn auto_scale(native_w: u32, native_h: u32) -> u32 {
    let longest = native_w.max(native_h);
    (1200 / longest).max(1)
}
