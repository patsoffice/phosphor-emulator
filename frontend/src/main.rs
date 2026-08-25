use clap::Parser;
use phosphor_core::core::display::DisplaySettings;
use phosphor_machines::registry;

mod audio;
mod config;
mod console_ui;
mod debug_ui;
mod emulator;
mod gfxview;
mod headless;
mod host_keys;
mod input;
mod movie;
mod overlay;
mod profile;
mod screenshot;
mod settings_ui;
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

    /// Start in desktop fullscreen (overrides config; default is windowed)
    #[arg(long)]
    fullscreen: bool,

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

    /// Run with no window: capture <out>.png (final frame) + <out>.wav (audio),
    /// then exit. For screenshots, audio capture, and machine bring-up.
    #[arg(long)]
    headless: bool,

    /// Frames to run in --headless mode (default 600 ≈ 10 s).
    #[arg(long, default_value_t = 600)]
    frames: u32,

    /// Output path prefix for --headless captures (writes <out>.png/.wav).
    #[arg(long, default_value = "/tmp/phosphor_capture")]
    out: String,

    /// Record live gameplay audio to this WAV file (mono 16-bit PCM), written on
    /// exit. Works during normal interactive play, unlike --headless.
    #[arg(long, value_name = "PATH")]
    record_wav: Option<String>,

    /// Replay a recorded input movie (`.phmi`) instead of taking live input.
    ///
    /// Resets to power-on and plays the session back in the window, with sound
    /// and the debugger available — so a bug that only appears behind a coin and
    /// three minutes of play can be reached and then inspected. Live game input
    /// is ignored while a movie plays.
    #[arg(long, value_name = "PATH")]
    movie: Option<std::path::PathBuf>,

    /// Open the interactive GFX viewer (charset/sprite ROM sheets) instead of
    /// running the machine. Cycle regions with ←/→, zoom with +/-, 0 to fit.
    #[arg(long)]
    gfxview: bool,

    /// Region to open first in --gfxview (default: the first registered region).
    #[arg(long, value_name = "NAME")]
    gfx_region: Option<String>,
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

    // Load persisted state up front so per-game config overrides can feed into
    // resolution. Per-game settings are keyed by the machine's registry/CLI
    // name; clone the resolved entry so later mutable use of `state` is clear.
    let mut state = state::load();
    let per_game = state.machine(&machine_name).cloned().unwrap_or_default();

    // Resolution order for every config field: CLI arg > per-game override >
    // global config.toml > built-in default.
    let rom_path = cli
        .rom_path
        .or(per_game.rom_path.clone())
        .or(config.rom_path.clone())
        .unwrap_or_else(|| {
            eprintln!("ROM path required. Either:");
            eprintln!("  phosphor {machine_name} /path/to/roms");
            if let Some(dir) = config::config_dir() {
                eprintln!("  or set rom_path in {}", dir.join("config.toml").display());
            }
            std::process::exit(1);
        });

    // Bring SDL up before the machine exists, so the audio device can be asked
    // what output rate it will grant. Every sound chip resamples to that rate
    // and reads it when it constructs, so it has to be settled first — see
    // `phosphor_core::audio::set_host_sample_rate`. Headless capture never
    // touches SDL, and keeps the default rate.
    let sdl_context = (!cli.headless).then(|| {
        let sdl = emulator::init_sdl();
        match sdl.audio() {
            Ok(sdl_audio) => {
                let rate = audio::granted_output_rate(
                    &sdl_audio,
                    phosphor_core::audio::DEFAULT_HOST_SAMPLE_RATE,
                );
                phosphor_core::audio::set_host_sample_rate(rate);
            }
            // No audio device (headless CI, no sound card). Video still works;
            // the machine keeps the default rate and its samples go nowhere.
            Err(e) => eprintln!("Note: no audio device ({e}); running without sound."),
        }
        sdl
    });

    let (mut machine, rom_digest) = create_from_first_rom_set(entry, &rom_path);

    // GFX viewer: display the machine's decoded charset/sprite sheets.
    if cli.gfxview {
        // Boot the machine briefly first. Machines with a color-PROM palette are
        // already colored at ROM-load, but many (DECO BurgerTime, MCR, Gottlieb,
        // Atari raster) build their palette from palette RAM the game only writes
        // once running — un-booted, that RAM is at its power-on state (e.g.
        // BurgerTime's inverted format decodes zeroed RAM to all-white). Running
        // a couple seconds of attract-mode frames populates it; PROM machines are
        // unaffected.
        const GFXVIEW_BOOT_FRAMES: u32 = 180;
        machine.reset();
        for _ in 0..GFXVIEW_BOOT_FRAMES {
            machine.run_frame();
        }
        if let Err(e) = gfxview::run(&machine_name, machine.as_ref(), cli.gfx_region.as_deref()) {
            eprintln!("gfxview: {e}");
            std::process::exit(1);
        }
        return;
    }

    // Load battery-backed NVRAM from disk (if available)
    let nvram_path = nvram_path_for(&config, per_game.nvram_path.as_deref(), &machine_name);
    if let Ok(data) = std::fs::read(&nvram_path) {
        machine.load_nvram(&data);
    }

    let (native_w, native_h) = machine.display_size();
    // Size the auto-scale off the on-screen presentation (aspect-corrected,
    // rotation-swapped), not the raw native raster, so the longest *visible*
    // axis stays under the cap.
    let rotated = machine.orientation().swaps_axes();
    let (pres_w, pres_h, _) =
        emulator::presentation(native_w, native_h, machine.display_aspect(), rotated);
    let scale = cli
        .scale
        .or(per_game.scale)
        .or(config.scale)
        .unwrap_or_else(|| auto_scale(pres_w, pres_h));

    // CLI --fullscreen forces it on; otherwise fall back through per-game then
    // global config, defaulting to windowed.
    let fullscreen = cli.fullscreen || per_game.fullscreen.or(config.fullscreen).unwrap_or(false);

    let save_path = save_path_for(&config, per_game.save_path.as_deref(), &machine_name);
    let screenshot_dir = screenshot_dir();

    // Build input bindings from machine defaults, then overlay any persisted
    // per-machine overrides (only machines with typed controls participate).
    let mut bindings = input::build_bindings(machine.as_ref());
    let has_typed_controls = !machine.input_controls().is_empty();
    if has_typed_controls && !per_game.input_bindings.is_empty() {
        bindings.apply_overrides(machine.input_controls(), &per_game.input_bindings);
    }

    // Snapshot the machine's power-on DIP bytes, then overlay any persisted
    // per-machine overrides. Applied before reset() so that OnReset options
    // latch when the machine powers on.
    let has_dip = !machine.dip_banks().is_empty();
    let dip_defaults: Vec<u8> = (0..machine.dip_banks().len())
        .map(|i| machine.dip_bank_value(i))
        .collect();
    if has_dip && !per_game.dip_switches.is_empty() {
        for (bank, &byte) in per_game.dip_switches.iter().enumerate() {
            machine.set_dip_bank_value(bank, byte);
        }
    }

    // Display knobs resolve the way the paths above do: the measured figures
    // first, then any global preference, then this game's own. Set before the
    // first frame so the picture opens the way it was left.
    let display_base = config.display.apply_to(DisplaySettings::MEASURED);
    phosphor_core::core::display::set_display_settings(per_game.display.apply_to(display_base));

    machine.reset();

    // Headless capture short-circuits the SDL main loop entirely.
    if cli.headless {
        headless::run(machine.as_mut(), cli.frames, &cli.out);
        return;
    }

    machine = emulator::run(
        sdl_context.expect("SDL is initialized on every non-headless path"),
        machine,
        &mut bindings,
        scale,
        fullscreen,
        &save_path,
        &screenshot_dir,
        &movie_dir(),
        rom_digest,
        &machine_name,
        cli.debug,
        cli.profile,
        cli.no_mouse_grab,
        cli.record_wav.as_deref(),
        cli.movie.as_deref(),
        &|| create_from_first_rom_set(entry, &rom_path).0,
        &mut state,
    );

    // Rebuild this machine's settings entry. Start from the loaded overrides so
    // any per-game config overrides (scale/paths) are preserved, then refresh
    // the runtime-mutated DIP/binding diffs.
    let mut settings = per_game;

    // Persist input bindings only when they differ from the machine defaults
    // (keeps state.toml free of redundant default entries).
    if has_typed_controls {
        let controls = machine.input_controls();
        let current = bindings.to_serialized(controls);
        let default = input::build_bindings(machine.as_ref()).to_serialized(controls);
        settings.input_bindings = if input::bindings_eq(&current, &default) {
            Vec::new()
        } else {
            current
        };
    }

    // Persist DIP bytes, dropping them when they match the machine's power-on
    // defaults (keeps state.toml free of redundant entries).
    if has_dip {
        let current: Vec<u8> = (0..machine.dip_banks().len())
            .map(|i| machine.dip_bank_value(i))
            .collect();
        settings.dip_switches = if current == dip_defaults {
            Vec::new()
        } else {
            current
        };
    }

    // Persist the display knobs against what they were resolved from, so a game
    // left at the global preference writes nothing of its own.
    settings.display = state::DisplayOverrides::diff(
        phosphor_core::core::display::display_settings(),
        display_base,
    );

    state.set_machine(&machine_name, settings);
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

/// Resolve the save-state directory: per-game override > global config > default.
fn save_path_for(
    config: &config::Config,
    per_game: Option<&str>,
    machine_name: &str,
) -> std::path::PathBuf {
    let dir = per_game
        .map(std::path::PathBuf::from)
        .or_else(|| config.save_path.as_ref().map(std::path::PathBuf::from))
        .unwrap_or_else(|| default_data_dir("save"));
    ensure_dir(&dir);
    dir.join(format!("{machine_name}.sav"))
}

/// Resolve the NVRAM directory: per-game override > global config > default.
fn nvram_path_for(
    config: &config::Config,
    per_game: Option<&str>,
    machine_name: &str,
) -> std::path::PathBuf {
    let dir = per_game
        .map(std::path::PathBuf::from)
        .or_else(|| config.nvram_path.as_ref().map(std::path::PathBuf::from))
        .unwrap_or_else(|| default_data_dir("nvram"));
    ensure_dir(&dir);
    dir.join(format!("{machine_name}.nvram"))
}

fn screenshot_dir() -> std::path::PathBuf {
    let dir = default_data_dir("screenshots");
    ensure_dir(&dir);
    dir
}

/// Where captured input movies are written. Created lazily by the capture
/// itself, so a session that never records leaves no directory behind.
fn movie_dir() -> std::path::PathBuf {
    default_data_dir("movies")
}

/// Try each ROM set name in order, returning the first machine that
/// initialises successfully. This ensures that ROM loading *and* machine
/// creation (which validates CRC32s, sizes, and ROM_CONTINUE layouts) both
/// succeed before we commit to a ROM set.
fn create_from_first_rom_set(
    entry: &phosphor_machines::registry::MachineEntry,
    path: &str,
) -> (
    Box<dyn phosphor_core::core::machine::FrontendMachine>,
    [u8; 32],
) {
    // Surface the most common failure — a ROM path that doesn't exist on disk —
    // with a clear, dedicated message before we even try each ROM name. Without
    // this, a missing directory only shows up as a generic "I/O error: ROM path
    // not found" buried under cargo's own output, which is easy to miss.
    if !std::path::Path::new(path).exists() {
        eprintln!("ROM path not found: {path}");
        eprintln!(
            "  Expected a ROM directory or .zip for '{}' here.",
            entry.name
        );
        eprintln!(
            "  Pass a path explicitly:  phosphor {} /path/to/roms",
            entry.name
        );
        if let Some(dir) = config::config_dir() {
            eprintln!("  or fix rom_path in {}", dir.join("config.toml").display());
        }
        std::process::exit(1);
    }

    let mut last_err = None;
    for name in entry.rom_names {
        let rom_set = match phosphor_harness::load_rom_set(path, &[name]) {
            Ok(set) => set,
            Err(e) => {
                last_err = Some(e);
                continue;
            }
        };
        match (entry.create)(&rom_set) {
            // Digest the set the machine was actually built from, so a captured
            // movie can refuse to replay against a different dump.
            Ok(machine) => {
                let digest = phosphor_harness::movie::rom_digest(&rom_set);
                return (machine, digest);
            }
            Err(e) => last_err = Some(e),
        }
    }
    let err = last_err.unwrap_or_else(|| {
        phosphor_machines::rom_loader::RomLoadError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no ROM names configured",
        ))
    });
    eprintln!(
        "Failed to load ROMs for '{}' from {path}: {err}",
        entry.name
    );
    eprintln!("  Tried ROM set names: {}", entry.rom_names.join(", "));
    std::process::exit(1);
}

/// Pick the largest integer scale that keeps the window under 1200 pixels
/// on its longest axis (fits comfortably on most displays). Takes the on-screen
/// presentation size (aspect-corrected), not the native raster.
fn auto_scale(pres_w: u32, pres_h: u32) -> u32 {
    let longest = pres_w.max(pres_h);
    (1200 / longest).max(1)
}
