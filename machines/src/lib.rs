/// Active-high bit manipulation: set bit on press, clear on release.
pub(crate) fn set_bit_active_high(reg: &mut u8, bit: u8, pressed: bool) {
    if pressed {
        *reg |= 1 << bit;
    } else {
        *reg &= !(1 << bit);
    }
}

/// Active-low bit manipulation: clear bit on press, set bit on release.
pub(crate) fn set_bit_active_low(reg: &mut u8, bit: u8, pressed: bool) {
    if pressed {
        *reg &= !(1 << bit);
    } else {
        *reg |= 1 << bit;
    }
}

/// Implements `MachineDebug` for standalone machines (single CPU, flat bus).
///
/// Requires the type to:
/// - Have a `step_cycle()` method returning the instruction-boundary mask
/// - Implement `BusDebug` on `Self`
/// - Have `TIMING` in scope
macro_rules! impl_standalone_debug {
    ($type:ty) => {
        impl phosphor_core::core::machine::MachineDebug for $type {
            fn debug_bus(&self) -> Option<&dyn phosphor_core::core::debug::BusDebug> {
                Some(self)
            }

            fn debug_bus_mut(&mut self) -> Option<&mut dyn phosphor_core::core::debug::BusDebug> {
                Some(self)
            }

            fn cycles_per_frame(&self) -> u64 {
                TIMING.cycles_per_frame()
            }

            fn debug_tick(&mut self) -> u32 {
                self.step_cycle()
            }
        }
    };
}
pub(crate) use impl_standalone_debug;

/// Implements `Renderable`, `AudioSource`, and `MachineDebug` for board-wrapper
/// machines that delegate to a `board` field and a `TIMING` constant.
///
/// # Basic usage
/// ```ignore
/// impl_board_delegation!(PacmanSystem, board, namco_pac::TIMING);
/// ```
///
/// # Optional flags (comma-separated after timing path)
/// - `no_audio` — empty `AudioSource` impl (no audio hardware emulated yet)
/// - `vectors` — delegates `vector_display_list()` to the board
/// - `overlay_stats` — calls `self.overlay_stats_impl()` (define on your type)
macro_rules! impl_board_delegation {
    // Base case: standard audio, no extras
    ($type:ty, $board:ident, $timing:expr) => {
        $crate::impl_board_renderable!($type, $board, $timing);
        $crate::impl_board_audio!($type, $board);
        $crate::impl_board_debug!($type, $board, $timing);
    };
    // With options
    ($type:ty, $board:ident, $timing:expr, $($opt:tt)*) => {
        $crate::impl_board_delegation!(@render $type, $board, $timing, $($opt)*);
        $crate::impl_board_delegation!(@audio $type, $board, $($opt)*);
        $crate::impl_board_delegation!(@debug $type, $board, $timing, $($opt)*);
    };

    // --- Renderable dispatch ---
    // Walk the full option list, gathering the render-relevant flags (and
    // ignoring audio flags like `no_audio`), then emit a single accumulating
    // `impl_board_renderable!`.
    (@render $type:ty, $board:ident, $timing:expr, $($opt:tt)*) => {
        $crate::impl_board_delegation!(@render_gather [$type, $board, $timing] [] $($opt)*);
    };
    // Done: emit the impl with the gathered render flags.
    (@render_gather [$type:ty, $board:ident, $timing:expr] [$($flag:ident)*]) => {
        $crate::impl_board_renderable!($type, $board, $timing $(, $flag)*);
    };
    // Render flags: keep.
    (@render_gather $ctx:tt [$($flag:ident)*] vectors $($rest:tt)*) => {
        $crate::impl_board_delegation!(@render_gather $ctx [$($flag)* vectors] $($rest)*);
    };
    (@render_gather $ctx:tt [$($flag:ident)*] overlay_stats $($rest:tt)*) => {
        $crate::impl_board_delegation!(@render_gather $ctx [$($flag)* overlay_stats] $($rest)*);
    };
    (@render_gather $ctx:tt [$($flag:ident)*] orientation $($rest:tt)*) => {
        $crate::impl_board_delegation!(@render_gather $ctx [$($flag)* orientation] $($rest)*);
    };
    // Anything else (commas, `no_audio`, ...): drop one token and continue.
    (@render_gather $ctx:tt $flags:tt $skip:tt $($rest:tt)*) => {
        $crate::impl_board_delegation!(@render_gather $ctx $flags $($rest)*);
    };

    // --- AudioSource dispatch ---
    (@audio $type:ty, $board:ident, no_audio $($rest:tt)*) => {
        $crate::impl_board_audio!($type);
    };
    (@audio $type:ty, $board:ident, $($rest:tt)*) => {
        $crate::impl_board_audio!($type, $board);
    };

    // --- MachineDebug dispatch ---
    // Skip non-debug options
    (@debug $type:ty, $board:ident, $timing:expr, $opt:ident $($rest:tt)*) => {
        $crate::impl_board_delegation!(@debug $type, $board, $timing, $($rest)*);
    };
    (@debug $type:ty, $board:ident, $timing:expr, , $($rest:tt)*) => {
        $crate::impl_board_delegation!(@debug $type, $board, $timing, $($rest)*);
    };
    (@debug $type:ty, $board:ident, $timing:expr,) => {
        $crate::impl_board_debug!($type, $board, $timing);
    };
}
pub(crate) use impl_board_delegation;

/// Implements `Renderable` delegating to `board`.
///
/// Accepts zero or more optional-method flags after the timing path and
/// accumulates one delegated method per flag, so options compose without
/// enumerating every combination:
/// - `vectors` — delegate `vector_display_list()` to the board
/// - `overlay_stats` — call `self.overlay_stats_impl()`
/// - `orientation` — delegate `orientation()` to the board (rotating boards
///   provide an inherent `orientation()` reading their DIP/state)
macro_rules! impl_board_renderable {
    // Vector machines: the timing's dimensions are the coordinate extent the
    // display list is expressed in, not a pixel count, and the resolution to
    // rasterize it at is derived from the tube rather than from whatever
    // numeric range the game's data happens to use. See `vector_field_size`.
    ($type:ty, $board:ident, $timing:expr, vector_field $(, $opt:ident)* $(,)?) => {
        impl phosphor_core::core::machine::Renderable for $type {
            fn display_size(&self) -> (u32, u32) {
                let (w, h) = $timing.display_size();
                phosphor_core::device::dvg::raster_size_for_field(w, h)
            }
            fn vector_field_size(&self) -> Option<(u32, u32)> {
                Some($timing.display_size())
            }
            fn display_aspect(&self) -> Option<(u32, u32)> {
                $timing.display_aspect()
            }
            fn render_frame(&self, buffer: &mut [u8]) {
                self.$board.render_frame(buffer);
            }
            $(
                $crate::impl_board_renderable!(@method $board, $opt);
            )*
        }
    };
    // Entry: the always-present methods plus one accumulated method per flag.
    ($type:ty, $board:ident, $timing:expr $(, $opt:ident)* $(,)?) => {
        impl phosphor_core::core::machine::Renderable for $type {
            fn display_size(&self) -> (u32, u32) {
                $timing.display_size()
            }
            fn display_aspect(&self) -> Option<(u32, u32)> {
                $timing.display_aspect()
            }
            fn render_frame(&self, buffer: &mut [u8]) {
                self.$board.render_frame(buffer);
            }
            $(
                $crate::impl_board_renderable!(@method $board, $opt);
            )*
        }
    };
    // Optional method fragments, one per flag.
    (@method $board:ident, vectors) => {
        fn vector_display_list(&self) -> Option<&[phosphor_core::device::dvg::VectorLine]> {
            self.$board.vector_display_list()
        }
    };
    (@method $board:ident, overlay_stats) => {
        fn overlay_stats(&self) -> Option<String> {
            self.overlay_stats_impl()
        }
    };
    (@method $board:ident, orientation) => {
        fn orientation(&self) -> phosphor_core::core::machine::Orientation {
            self.$board.orientation()
        }
    };
}
pub(crate) use impl_board_renderable;

/// Implements `AudioSource` delegating to board (or empty).
macro_rules! impl_board_audio {
    // No audio
    ($type:ty) => {
        impl phosphor_core::core::machine::AudioSource for $type {}
    };
    // Standard: delegate to board
    ($type:ty, $board:ident) => {
        impl phosphor_core::core::machine::AudioSource for $type {
            fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
                self.$board.fill_audio(buffer)
            }
            fn audio_sample_rate(&self) -> u32 {
                phosphor_core::audio::host_sample_rate()
            }
        }
    };
}
pub(crate) use impl_board_audio;

/// Implements `MachineDebug` for a board-wrapper machine.
///
/// The machine holds its CPU beside the board, so the machine itself is the
/// `BusDebug` (via `#[debug_bus]`, which merges the board's devices and maps
/// with the machine's CPU) and one cycle is its inherent `step_cycle()`.
macro_rules! impl_board_debug {
    ($type:ty, $board:ident, $timing:expr) => {
        impl phosphor_core::core::machine::MachineDebug for $type {
            fn debug_bus(&self) -> Option<&dyn phosphor_core::core::debug::BusDebug> {
                Some(self)
            }
            fn debug_bus_mut(&mut self) -> Option<&mut dyn phosphor_core::core::debug::BusDebug> {
                Some(self)
            }
            fn cycles_per_frame(&self) -> u64 {
                $timing.cycles_per_frame()
            }
            fn debug_tick(&mut self) -> u32 {
                self.step_cycle()
            }
        }
    };
}
pub(crate) use impl_board_debug;

/// Generates `MachineCore` identity/timing methods inside an `impl MachineCore` block.
///
/// Expands to: `frame_rate_hz()`, `machine_id()`, and `clock_declaration()`.
///
/// # Usage
/// ```ignore
/// impl MachineCore for PacmanSystem {
///     crate::machine_core_metadata!("pacman", namco_pac::TIMING, namco_pac::clock_tree);
///     fn run_frame(&mut self) { ... }
///     fn reset(&mut self) { ... }
/// }
/// ```
///
/// The clock tree is required: `clock_tree_test.rs` asserts every registered
/// machine declares one, because a board whose crystals are only in a comment
/// is the thing that test exists to catch.
macro_rules! machine_core_metadata {
    ($id:expr, $timing:expr, $tree:path) => {
        fn frame_rate_hz(&self) -> f64 {
            $timing.frame_rate_hz()
        }
        fn machine_id(&self) -> &str {
            $id
        }
        $crate::machine_clock_declaration!($timing, $tree);
    };
}
pub(crate) use machine_core_metadata;

/// Generates just `MachineCore::clock_declaration()`, for the machines that
/// hand-write the rest of their identity methods.
macro_rules! machine_clock_declaration {
    ($timing:expr, $tree:path) => {
        fn clock_declaration(&self) -> Option<phosphor_core::core::ClockDeclaration> {
            Some(phosphor_core::core::ClockDeclaration {
                tree: $tree(),
                timing: $timing,
            })
        }
    };
}
pub(crate) use machine_clock_declaration;

/// Implements default-only frontend capabilities for machines without
/// battery-backed RAM, sub-span profiling, or event tracing.
///
/// Expands to empty `Nvram`, `Profilable`, and `DebugTrace` impls. Machines
/// that override any of these capabilities must write that impl by hand instead
/// of using this macro (never hide non-default behavior in macros).
/// `InputConfigurable` and `DipSwitches` are machine-specific, so they are
/// never generated here.
macro_rules! impl_default_frontend_capabilities {
    ($type:ty) => {
        impl phosphor_core::core::machine::Nvram for $type {}
        impl phosphor_core::core::machine::Profilable for $type {}
        impl phosphor_core::core::debug_trace::DebugTrace for $type {}
    };
}
pub(crate) use impl_default_frontend_capabilities;

/// Implements `DebugTrace` by routing to an `AddressSpace16`/`AddressSpace32`
/// field's write-event ring. The field path is given as dot-separated idents
/// (e.g. `map` or `board.map` or `board.main_map`). Use this for boards whose
/// bus writes flow through an `AddressSpace` (the map records region-tagged
/// write events); pair it with `map.trace_bus_io_write` calls in any
/// `Bus::io_write` so the separate I/O space is captured too.
macro_rules! impl_map_debug_trace {
    ($type:ty, $($map:tt).+) => {
        impl phosphor_core::core::debug_trace::DebugTrace for $type {
            fn set_trace_enabled(&mut self, enabled: bool) {
                self.$($map).+.set_trace_enabled(enabled);
            }
            fn trace_enabled(&self) -> bool {
                self.$($map).+.trace_enabled()
            }
            fn trace_events(&mut self) -> &[phosphor_core::core::debug_trace::DebugEvent] {
                self.$($map).+.trace_events()
            }
            fn clear_trace_events(&mut self) {
                self.$($map).+.clear_trace_events();
            }
        }
    };
}
pub(crate) use impl_map_debug_trace;

/// Implements `DebugTrace` delegating to a board field, for board-wrapper
/// machines whose board embeds a `DebugTraceBuffer` (via `#[derive(DebugTrace)]`).
macro_rules! impl_board_debug_trace {
    ($type:ty, $board:ident) => {
        impl phosphor_core::core::debug_trace::DebugTrace for $type {
            fn set_trace_enabled(&mut self, enabled: bool) {
                phosphor_core::core::debug_trace::DebugTrace::set_trace_enabled(
                    &mut self.$board,
                    enabled,
                );
            }
            fn trace_enabled(&self) -> bool {
                phosphor_core::core::debug_trace::DebugTrace::trace_enabled(&self.$board)
            }
            fn trace_events(&mut self) -> &[phosphor_core::core::debug_trace::DebugEvent] {
                phosphor_core::core::debug_trace::DebugTrace::trace_events(&mut self.$board)
            }
            fn clear_trace_events(&mut self) {
                phosphor_core::core::debug_trace::DebugTrace::clear_trace_events(&mut self.$board);
            }
        }
    };
}
pub(crate) use impl_board_debug_trace;

/// Generates standard `save_state()`/`load_state()` methods inside an
/// `impl SaveState` block.
///
/// The no-arg form delegates to `MachineCore::machine_id()`, so `MachineCore`
/// must be in scope (and implemented) at the expansion site.
///
/// # Usage
/// ```ignore
/// impl SaveState for PacmanSystem {
///     crate::machine_save_state!();
/// }
/// ```
macro_rules! machine_save_state {
    () => {
        fn save_state(&self) -> Option<Vec<u8>> {
            Some(phosphor_core::core::save_state::save_machine(
                self,
                self.machine_id(),
            ))
        }
        fn load_state(
            &mut self,
            data: &[u8],
        ) -> Result<(), phosphor_core::core::save_state::SaveError> {
            let id = self.machine_id().to_string();
            phosphor_core::core::save_state::load_machine(self, &id, data)
        }
    };
}
pub(crate) use machine_save_state;

/// Implements `DipSwitches` from the bank table plus the field each bank lives
/// in. The `DipSwitchBank`/`DipOption`/`DipChoice` table itself is per-game
/// hardware data and stays hand-written in the machine file — only the
/// three-method accessor triple is generated.
///
/// # Usage
/// ```ignore
/// // One bank, held in a plain field or a board field.
/// crate::impl_dip_switches!(DkongSystem, DKONG_DIP_BANKS, board.dsw0);
/// // Two banks.
/// crate::impl_dip_switches!(DigDugSystem, DIGDUG_DIP_BANKS, board.dswa, board.dswb);
/// // Banks sharing an input port with live signals: reads mask, writes merge
/// // so the non-DIP bits of the port survive.
/// crate::impl_dip_switches!(
///     GalaxianSystem, GALAXIAN_DIP_BANKS,
///     board.in0 & DIP0_MASK, board.in1 & DIP1_MASK, board.in2 & DIP2_MASK
/// );
/// ```
///
/// A machine whose accessors do anything else keeps its hand-written impl:
/// `quantum.rs` recomputes pot values on write, `pisces.rs` looks the port up
/// per hardware config, `burgertime.rs` masks a live VBLANK bit out of one bank
/// but clobbers rather than merges on write.
macro_rules! impl_dip_switches {
    // One bank.
    ($type:ty, $banks:expr, $($f0:ident).+ $(,)?) => {
        impl phosphor_core::core::machine::DipSwitches for $type {
            fn dip_banks(&self) -> &'static [phosphor_core::core::machine::DipSwitchBank] {
                $banks
            }

            fn dip_bank_value(&self, bank: usize) -> u8 {
                if bank == 0 { self.$($f0).+ } else { 0 }
            }

            fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
                if bank == 0 {
                    self.$($f0).+ = value;
                }
            }
        }
    };

    // Two banks.
    ($type:ty, $banks:expr, $($f0:ident).+, $($f1:ident).+ $(,)?) => {
        impl phosphor_core::core::machine::DipSwitches for $type {
            fn dip_banks(&self) -> &'static [phosphor_core::core::machine::DipSwitchBank] {
                $banks
            }

            fn dip_bank_value(&self, bank: usize) -> u8 {
                match bank {
                    0 => self.$($f0).+,
                    1 => self.$($f1).+,
                    _ => 0,
                }
            }

            fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
                match bank {
                    0 => self.$($f0).+ = value,
                    1 => self.$($f1).+ = value,
                    _ => {}
                }
            }
        }
    };

    // Two banks sharing input ports with live signals.
    ($type:ty, $banks:expr, $($f0:ident).+ & $m0:expr, $($f1:ident).+ & $m1:expr $(,)?) => {
        impl phosphor_core::core::machine::DipSwitches for $type {
            fn dip_banks(&self) -> &'static [phosphor_core::core::machine::DipSwitchBank] {
                $banks
            }

            fn dip_bank_value(&self, bank: usize) -> u8 {
                match bank {
                    0 => self.$($f0).+ & $m0,
                    1 => self.$($f1).+ & $m1,
                    _ => 0,
                }
            }

            fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
                match bank {
                    0 => self.$($f0).+ = (self.$($f0).+ & !$m0) | (value & $m0),
                    1 => self.$($f1).+ = (self.$($f1).+ & !$m1) | (value & $m1),
                    _ => {}
                }
            }
        }
    };

    // Three banks sharing input ports with live signals.
    (
        $type:ty, $banks:expr,
        $($f0:ident).+ & $m0:expr,
        $($f1:ident).+ & $m1:expr,
        $($f2:ident).+ & $m2:expr $(,)?
    ) => {
        impl phosphor_core::core::machine::DipSwitches for $type {
            fn dip_banks(&self) -> &'static [phosphor_core::core::machine::DipSwitchBank] {
                $banks
            }

            fn dip_bank_value(&self, bank: usize) -> u8 {
                match bank {
                    0 => self.$($f0).+ & $m0,
                    1 => self.$($f1).+ & $m1,
                    2 => self.$($f2).+ & $m2,
                    _ => 0,
                }
            }

            fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
                match bank {
                    0 => self.$($f0).+ = (self.$($f0).+ & !$m0) | (value & $m0),
                    1 => self.$($f1).+ = (self.$($f1).+ & !$m1) | (value & $m1),
                    2 => self.$($f2).+ = (self.$($f2).+ & !$m2) | (value & $m2),
                    _ => {}
                }
            }
        }
    };
}
pub(crate) use impl_dip_switches;

/// Registers a machine with the front-end registry: builds the ROM-set factory
/// and its ROM-less counterpart, and submits the [`registry::MachineEntry`] in
/// one line.
///
/// Both factories run the same constructor; only the ROM-set one goes on to
/// `load_rom_set`. See [`registry::MachineEntry::create_bare`] for why the
/// ROM-less one exists.
///
/// The four arguments are the wrapper type, the CLI name, the ROM set names to
/// try for ZIP lookup, and the machine's static control table. The control
/// table has to be named explicitly — it is a separate const whose name tracks
/// neither the type nor the CLI name (`CONGO_CONTROLS`, `DKONG_CONTROLS`), and
/// it cannot be an associated const because `InputConfigurable` is a supertrait
/// of the `dyn FrontendMachine` the registry stores.
///
/// # Usage
/// ```ignore
/// crate::register_machine!(JoustSystem, "joust", &["joust"], JOUST_CONTROLS);
///
/// // Constructor takes an argument (a hardware variant, say):
/// crate::register_machine!(
///     new = PiscesSystem::new(&PISCES), "pisces", &["pisces"], GALAXIAN_CONTROLS
/// );
///
/// // Machines whose ROM set varies by revision: each config is tried in turn
/// // and the first that loads wins.
/// crate::register_machine!(
///     GalagaSystem, "galaga", &["galaga", "galagao", "galagamw"],
///     namco_galaga::NAMCO_GALAGA_CONTROLS, configs = ALL_CONFIGS
/// );
/// ```
///
/// Machines whose factory does anything else — a constructor argument, a reset
/// after load, a non-standard loader — keep their hand-written factory and
/// `inventory::submit!`. Per `machines/CLAUDE.md`, macros generate obvious
/// delegation only; machine-specific behavior stays visible in the machine file.
macro_rules! register_machine {
    // Constructor takes an argument (hardware variant, ROM config); still just
    // construct-then-`load_rom_set`.
    (new = $ctor:expr, $name:expr, $rom_names:expr, $controls:expr) => {
        ::inventory::submit! {
            $crate::registry::MachineEntry::new($name, $rom_names, {
                fn create(
                    rom_set: &$crate::rom_loader::RomSet,
                ) -> Result<
                    Box<dyn phosphor_core::core::machine::FrontendMachine>,
                    $crate::rom_loader::RomLoadError,
                > {
                    let mut sys = $ctor;
                    sys.load_rom_set(rom_set)?;
                    Ok(Box::new(sys))
                }
                create
            }, {
                fn create_bare() -> Box<dyn phosphor_core::core::machine::FrontendMachine> {
                    let mut sys = $ctor;
                    let _ = sys.load_rom_set(&$crate::rom_loader::RomSet::blank());
                    Box::new(sys)
                }
                create_bare
            }, $controls)
        }
    };

    // Standard: `Type::new()`, then `load_rom_set`.
    ($type:ty, $name:expr, $rom_names:expr, $controls:expr) => {
        ::inventory::submit! {
            $crate::registry::MachineEntry::new($name, $rom_names, {
                fn create(
                    rom_set: &$crate::rom_loader::RomSet,
                ) -> Result<
                    Box<dyn phosphor_core::core::machine::FrontendMachine>,
                    $crate::rom_loader::RomLoadError,
                > {
                    let mut sys = <$type>::new();
                    sys.load_rom_set(rom_set)?;
                    Ok(Box::new(sys))
                }
                create
            }, {
                fn create_bare() -> Box<dyn phosphor_core::core::machine::FrontendMachine> {
                    let mut sys = <$type>::new();
                    let _ = sys.load_rom_set(&$crate::rom_loader::RomSet::blank());
                    Box::new(sys)
                }
                create_bare
            }, $controls)
        }
    };

    // ROM set varies by revision: try each config, first one that loads wins.
    ($type:ty, $name:expr, $rom_names:expr, $controls:expr, configs = $configs:expr) => {
        ::inventory::submit! {
            $crate::registry::MachineEntry::new($name, $rom_names, {
                fn create(
                    rom_set: &$crate::rom_loader::RomSet,
                ) -> Result<
                    Box<dyn phosphor_core::core::machine::FrontendMachine>,
                    $crate::rom_loader::RomLoadError,
                > {
                    let mut last_err = None;
                    for config in $configs {
                        let mut sys = <$type>::new();
                        match sys.load_roms(rom_set, config) {
                            Ok(()) => return Ok(Box::new(sys)),
                            Err(e) => last_err = Some(e),
                        }
                    }
                    Err(last_err.unwrap())
                }
                create
            }, {
                fn create_bare() -> Box<dyn phosphor_core::core::machine::FrontendMachine> {
                    let mut sys = <$type>::new();
                    // Any config will do: a blank set has no revision to match.
                    if let Some(config) = $configs.first() {
                        let _ = sys.load_roms(&$crate::rom_loader::RomSet::blank(), config);
                    }
                    Box::new(sys)
                }
                create_bare
            }, $controls)
        }
    };
}
pub(crate) use register_machine;

pub mod astdelux;
pub mod asteroids;
pub mod asteroids_sound;
pub mod atari_avg;
pub mod atari_dvg;
pub mod atari_system1;
pub mod atari_system1_sound;
pub mod btime;
pub mod burgertime;
pub mod ccastles;
pub mod congo_bongo;
pub mod congo_sound;
pub mod digdug;
pub mod disasm_registry;
pub mod dkong_sound;
pub mod docastle;
pub mod donkey_kong;
pub mod donkey_kong_jr;
pub mod foodf;
pub mod frogger;
pub mod galaga;
pub mod galaxian;
pub mod galaxian_video;
pub mod gfx_registry;
pub mod gottlieb;
pub mod gridlee;
pub(crate) mod input_defaults;
pub mod irobot;
pub mod joust;
pub mod llander;
pub mod llander_sound;
pub mod marble;
pub mod mario_bros;
pub mod mcr2;
pub mod missile_command;
pub mod mooncresta;
pub mod mrdo;
pub mod mspacman;
pub mod namco_galaga;
pub mod namco_pac;
pub mod namco_video;
pub mod pacman;
pub mod pisces;
pub mod qbert;
pub mod quantum;
pub mod registry;
pub mod roadrunner;
pub mod robotron;
pub mod rom_loader;
pub mod satans_hollow;
pub mod scramble;
pub mod simple_system;
pub mod sinistar;
pub mod starwars;
pub mod tempest;
pub mod tkg04;
pub mod williams;
pub mod xevious;
pub mod z80dma;

pub use astdelux::AsteroidsDeluxeSystem;
pub use asteroids::AsteroidsSystem;
pub use atari_dvg::AtariDvgBoard;
pub use atari_system1::AtariSystem1Board;
pub use burgertime::BurgertimeSystem;
pub use ccastles::CrystalCastlesSystem;
pub use digdug::DigDugSystem;
pub use donkey_kong::DkongSystem;
pub use donkey_kong_jr::DkongJrSystem;
pub use foodf::FoodFightSystem;
pub use galaxian::GalaxianSystem;
pub use gridlee::GridleeSystem;
pub use irobot::IrobotSystem;
pub use joust::JoustSystem;
pub use llander::LunarLanderSystem;
pub use marble::MarbleSystem;
pub use mario_bros::MarioBrosSystem;
pub use missile_command::MissileCommandSystem;
pub use mspacman::MsPacmanSystem;
pub use pacman::PacmanSystem;
pub use qbert::QbertSystem;
pub use quantum::QuantumSystem;
pub use roadrunner::RoadRunnerSystem;
pub use robotron::RobotronSystem;
pub use satans_hollow::SatansHollowSystem;
pub use simple_system::{
    Simple6502System, Simple6800System, Simple6809System, SimpleI8035System, SimpleI8088System,
    SimpleSystem, SimpleSystem32, SimpleZ80System,
};
pub use sinistar::SinistarSystem;
pub use tempest::TempestSystem;
pub use xevious::XeviousSystem;

/// Shared DIP-table validator for machine tests.
///
/// Asserts that, for each bank: options occupy disjoint bits, every choice fits
/// its option's mask, and the live `defaults[bank]` byte decomposes into a
/// defined choice for every option (so a table can't silently drift from the
/// machine's historical power-on byte).
///
/// Public (rather than `#[cfg(test)] pub(crate)`) because the registry-driven
/// contract test in `machines/tests/` runs it over every machine's live
/// power-on values — the in-crate `dip_test_suite!` callers only reach the
/// machines that remembered to invoke the macro.
pub fn assert_dip_banks_valid(
    banks: &[phosphor_core::core::machine::DipSwitchBank],
    defaults: &[u8],
) {
    assert_eq!(banks.len(), defaults.len(), "default count != bank count");
    for (bank_idx, bank) in banks.iter().enumerate() {
        let mut covered = 0u8;
        for opt in bank.options {
            assert_eq!(covered & opt.mask, 0, "overlapping masks in {}", bank.name);
            covered |= opt.mask;
            for choice in opt.choices {
                assert_eq!(
                    choice.value & !opt.mask,
                    0,
                    "choice {} escapes mask of {}",
                    choice.label,
                    opt.name
                );
            }
        }
        let default = defaults[bank_idx];
        for opt in bank.options {
            let selected = default & opt.mask;
            assert!(
                opt.choices.iter().any(|c| c.value == selected),
                "{} default 0x{default:02X} has no choice for {} (slice 0x{selected:02X})",
                bank.name,
                opt.name
            );
        }
    }
}

/// Generates a machine's standard DIP test suite from its expected power-on
/// bytes, one per bank.
///
/// ```ignore
/// crate::dip_test_suite!(PacmanSystem, &[0xC9]);
/// crate::dip_test_suite!(DigDugSystem, &[0x99, 0x24]);
/// ```
///
/// Expands to a private `#[cfg(test)]` module with three tests:
///
/// * `dip_defaults_and_metadata` — the power-on bytes match, the table passes
///   [`assert_dip_banks_valid`], and an out-of-range bank reads 0. The length
///   of the expected slice pins the bank count.
/// * `set_dip_option_masks_only_its_bits` — data-driven over the table: every
///   choice of every option is selectable, reads back as itself, and leaves
///   every other option's bits alone. This is where the suite earns its keep;
///   the hand-written version of this test picks one option and one choice.
/// * `dip_bank_values_round_trip` — every bit the bank's options claim
///   survives a write, no bit the caller didn't ask for is set, and an
///   out-of-range write disturbs nothing.
///
/// The round-trip check is scoped to the union of the bank's option masks
/// rather than the whole byte, so a machine whose DIPs share an input port
/// with live signals (reads mask, writes merge) satisfies it too — while a
/// machine that silently dropped a write would still fail.
///
/// Machine-specific DIP facts stay hand-written next to the invocation — see
/// `digdug.rs`, which additionally asserts that its banks map all 8 bits.
#[cfg(test)]
macro_rules! dip_test_suite {
    ($type:ty, $expected:expr) => {
        #[cfg(test)]
        mod generated_dip_tests {
            use super::*;
            use phosphor_core::core::machine::DipSwitches;

            #[test]
            fn dip_defaults_and_metadata() {
                let sys = <$type>::new();
                let expected: &[u8] = $expected;
                for (bank, &want) in expected.iter().enumerate() {
                    assert_eq!(sys.dip_bank_value(bank), want, "bank {bank} power-on byte");
                }
                $crate::assert_dip_banks_valid(sys.dip_banks(), expected);
                assert_eq!(
                    sys.dip_bank_value(expected.len()),
                    0,
                    "out-of-range bank must read 0"
                );
            }

            #[test]
            fn set_dip_option_masks_only_its_bits() {
                let banks = <$type>::new().dip_banks();
                for (bank_idx, bank) in banks.iter().enumerate() {
                    for (opt_idx, opt) in bank.options.iter().enumerate() {
                        for choice in opt.choices {
                            let mut sys = <$type>::new();
                            let before = sys.dip_bank_value(bank_idx);
                            sys.set_dip_option(bank_idx, opt_idx, choice.value);
                            let after = sys.dip_bank_value(bank_idx);
                            assert_eq!(
                                after & opt.mask,
                                choice.value,
                                "{}: {} = {} did not take",
                                bank.name,
                                opt.name,
                                choice.label
                            );
                            assert_eq!(
                                after & !opt.mask,
                                before & !opt.mask,
                                "{}: {} = {} disturbed bits outside its mask",
                                bank.name,
                                opt.name,
                                choice.label
                            );
                            // Other banks are untouched by an edit to this one.
                            for other in (0..banks.len()).filter(|&b| b != bank_idx) {
                                assert_eq!(
                                    sys.dip_bank_value(other),
                                    <$type>::new().dip_bank_value(other),
                                    "{}: {} = {} disturbed bank {other}",
                                    bank.name,
                                    opt.name,
                                    choice.label
                                );
                            }
                        }
                    }
                }
            }

            #[test]
            fn dip_bank_values_round_trip() {
                let banks = <$type>::new().dip_banks();
                for (bank, table) in banks.iter().enumerate() {
                    // Every bit the table claims as an option must survive a
                    // write; bits it does not claim may belong to live inputs
                    // sharing the port, so they are not asserted on.
                    let owned = table.options.iter().fold(0u8, |acc, opt| acc | opt.mask);
                    for probe in [0x00u8, 0xFF, 0x55, 0xAA] {
                        let mut sys = <$type>::new();
                        sys.set_dip_bank_value(bank, probe);
                        let got = sys.dip_bank_value(bank);
                        assert_eq!(
                            got & owned,
                            probe & owned,
                            "bank {bank}: writing 0x{probe:02X} did not round-trip \
                             (read 0x{got:02X}, table owns 0x{owned:02X})"
                        );
                        assert_eq!(
                            got & !probe,
                            0,
                            "bank {bank}: writing 0x{probe:02X} set bits that were not asked for \
                             (read 0x{got:02X})"
                        );
                        // Other banks keep their power-on bytes.
                        for other in (0..banks.len()).filter(|&b| b != bank) {
                            assert_eq!(
                                sys.dip_bank_value(other),
                                <$type>::new().dip_bank_value(other),
                                "bank {bank} write disturbed bank {other}"
                            );
                        }
                        // An out-of-range write disturbs nothing.
                        sys.set_dip_bank_value(banks.len() + 4, 0xFF);
                        assert_eq!(
                            sys.dip_bank_value(bank),
                            got,
                            "bank {bank} disturbed by an out-of-range write"
                        );
                    }
                }
            }
        }
    };
}
#[cfg(test)]
pub(crate) use dip_test_suite;

/// Assert that no two coin controls share a default physical binding. Binding a
/// single coin key to more than one coin slot inserts a coin into each, so one
/// key press would award several credits.
#[cfg(test)]
pub(crate) fn assert_no_coin_binding_collision(
    controls: &[phosphor_core::core::machine::InputControl],
) {
    use phosphor_core::core::machine::InputKind;
    let coins: Vec<_> = controls
        .iter()
        .filter(|c| matches!(c.kind, InputKind::Coin))
        .collect();
    for (i, a) in coins.iter().enumerate() {
        for b in &coins[i + 1..] {
            for binding in a.default_bindings {
                assert!(
                    !b.default_bindings.contains(binding),
                    "coin controls '{}' and '{}' share default binding {binding:?}: \
                     one press would award multiple credits",
                    a.stable_name,
                    b.stable_name,
                );
            }
        }
    }
}
