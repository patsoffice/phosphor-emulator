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
/// - Have a `cpu` field with `at_instruction_boundary()`
/// - Have a `tick()` method
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
                self.tick();
                if self.cpu.at_instruction_boundary() {
                    1
                } else {
                    0
                }
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
/// - `debug_tick_pre` — calls `self.debug_pre_tick()` before `board.tick()` in `debug_tick()`
/// - `bus_addr: Type` — address type for `bus_split!` (default: inferred)
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
    (@render $type:ty, $board:ident, $timing:expr, vectors $($rest:tt)*) => {
        $crate::impl_board_renderable!($type, $board, $timing, vectors);
    };
    (@render $type:ty, $board:ident, $timing:expr, overlay_stats $($rest:tt)*) => {
        $crate::impl_board_renderable!($type, $board, $timing, overlay_stats);
    };
    (@render $type:ty, $board:ident, $timing:expr, $($rest:tt)*) => {
        $crate::impl_board_renderable!($type, $board, $timing);
    };

    // --- AudioSource dispatch ---
    (@audio $type:ty, $board:ident, no_audio $($rest:tt)*) => {
        $crate::impl_board_audio!($type);
    };
    (@audio $type:ty, $board:ident, $($rest:tt)*) => {
        $crate::impl_board_audio!($type, $board);
    };

    // --- MachineDebug dispatch ---
    (@debug $type:ty, $board:ident, $timing:expr, debug_tick_pre $($rest:tt)*) => {
        $crate::impl_board_debug!($type, $board, $timing, debug_tick_pre);
    };
    (@debug $type:ty, $board:ident, $timing:expr, bus_addr: $addr:tt, debug_tick_pre $($rest:tt)*) => {
        $crate::impl_board_debug!($type, $board, $timing, bus_addr: $addr, debug_tick_pre);
    };
    (@debug $type:ty, $board:ident, $timing:expr, bus_addr: $addr:tt $(,)?) => {
        $crate::impl_board_debug!($type, $board, $timing, bus_addr: $addr);
    };
    (@debug $type:ty, $board:ident, $timing:expr, bus_addr: $addr:tt, $($rest:tt)*) => {
        $crate::impl_board_debug!($type, $board, $timing, bus_addr: $addr);
    };
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

/// Implements `Renderable` delegating to board.
macro_rules! impl_board_renderable {
    ($type:ty, $board:ident, $timing:expr) => {
        impl phosphor_core::core::machine::Renderable for $type {
            fn display_size(&self) -> (u32, u32) {
                $timing.display_size()
            }
            fn render_frame(&self, buffer: &mut [u8]) {
                self.$board.render_frame(buffer);
            }
        }
    };
    ($type:ty, $board:ident, $timing:expr, vectors) => {
        impl phosphor_core::core::machine::Renderable for $type {
            fn display_size(&self) -> (u32, u32) {
                $timing.display_size()
            }
            fn render_frame(&self, buffer: &mut [u8]) {
                self.$board.render_frame(buffer);
            }
            fn vector_display_list(&self) -> Option<&[phosphor_core::device::dvg::VectorLine]> {
                self.$board.vector_display_list()
            }
        }
    };
    ($type:ty, $board:ident, $timing:expr, overlay_stats) => {
        impl phosphor_core::core::machine::Renderable for $type {
            fn display_size(&self) -> (u32, u32) {
                $timing.display_size()
            }
            fn render_frame(&self, buffer: &mut [u8]) {
                self.$board.render_frame(buffer);
            }
            fn overlay_stats(&self) -> Option<String> {
                self.overlay_stats_impl()
            }
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
                44100
            }
        }
    };
}
pub(crate) use impl_board_audio;

/// Implements `MachineDebug` delegating to board.
macro_rules! impl_board_debug {
    ($type:ty, $board:ident, $timing:expr) => {
        impl phosphor_core::core::machine::MachineDebug for $type {
            fn debug_bus(&self) -> Option<&dyn phosphor_core::core::debug::BusDebug> {
                Some(&self.$board)
            }
            fn debug_bus_mut(&mut self) -> Option<&mut dyn phosphor_core::core::debug::BusDebug> {
                Some(&mut self.$board)
            }
            fn cycles_per_frame(&self) -> u64 {
                $timing.cycles_per_frame()
            }
            fn debug_tick(&mut self) -> u32 {
                phosphor_core::bus_split!(self, bus => {
                    self.$board.tick(bus);
                });
                self.$board.debug_tick_boundaries()
            }
        }
    };
    ($type:ty, $board:ident, $timing:expr, debug_tick_pre) => {
        impl phosphor_core::core::machine::MachineDebug for $type {
            fn debug_bus(&self) -> Option<&dyn phosphor_core::core::debug::BusDebug> {
                Some(&self.$board)
            }
            fn debug_bus_mut(&mut self) -> Option<&mut dyn phosphor_core::core::debug::BusDebug> {
                Some(&mut self.$board)
            }
            fn cycles_per_frame(&self) -> u64 {
                $timing.cycles_per_frame()
            }
            fn debug_tick(&mut self) -> u32 {
                self.debug_pre_tick();
                phosphor_core::bus_split!(self, bus => {
                    self.$board.tick(bus);
                });
                self.$board.debug_tick_boundaries()
            }
        }
    };
    ($type:ty, $board:ident, $timing:expr, bus_addr: $addr:tt) => {
        impl phosphor_core::core::machine::MachineDebug for $type {
            fn debug_bus(&self) -> Option<&dyn phosphor_core::core::debug::BusDebug> {
                Some(&self.$board)
            }
            fn debug_bus_mut(&mut self) -> Option<&mut dyn phosphor_core::core::debug::BusDebug> {
                Some(&mut self.$board)
            }
            fn cycles_per_frame(&self) -> u64 {
                $timing.cycles_per_frame()
            }
            fn debug_tick(&mut self) -> u32 {
                phosphor_core::bus_split!(self, bus : $addr => {
                    self.$board.tick(bus);
                });
                self.$board.debug_tick_boundaries()
            }
        }
    };
    ($type:ty, $board:ident, $timing:expr, bus_addr: $addr:tt, debug_tick_pre) => {
        impl phosphor_core::core::machine::MachineDebug for $type {
            fn debug_bus(&self) -> Option<&dyn phosphor_core::core::debug::BusDebug> {
                Some(&self.$board)
            }
            fn debug_bus_mut(&mut self) -> Option<&mut dyn phosphor_core::core::debug::BusDebug> {
                Some(&mut self.$board)
            }
            fn cycles_per_frame(&self) -> u64 {
                $timing.cycles_per_frame()
            }
            fn debug_tick(&mut self) -> u32 {
                self.debug_pre_tick();
                phosphor_core::bus_split!(self, bus : $addr => {
                    self.$board.tick(bus);
                });
                self.$board.debug_tick_boundaries()
            }
        }
    };
}
pub(crate) use impl_board_debug;

/// Generates `MachineCore` identity/timing methods inside an `impl MachineCore` block.
///
/// Expands to: `frame_rate_hz()`, `machine_id()`.
///
/// # Usage
/// ```ignore
/// impl MachineCore for PacmanSystem {
///     crate::machine_core_metadata!("pacman", namco_pac::TIMING);
///     fn run_frame(&mut self) { ... }
///     fn reset(&mut self) { ... }
/// }
/// ```
macro_rules! machine_core_metadata {
    ($id:expr, $timing:expr) => {
        fn frame_rate_hz(&self) -> f64 {
            $timing.frame_rate_hz()
        }
        fn machine_id(&self) -> &str {
            $id
        }
    };
}
pub(crate) use machine_core_metadata;

/// Implements default-only frontend capabilities for machines without
/// battery-backed RAM, sub-span profiling, event tracing, or DIP switches.
///
/// Expands to empty `Nvram`, `Profilable`, `DebugTrace`, and `DipSwitches`
/// impls. Machines that override any of these capabilities must write that
/// impl by hand instead of using this macro (never hide non-default behavior
/// in macros). `InputConfigurable` is always machine-specific (typed
/// controls), so it is never generated here.
macro_rules! impl_default_frontend_capabilities {
    ($type:ty) => {
        impl phosphor_core::core::machine::Nvram for $type {}
        impl phosphor_core::core::machine::Profilable for $type {}
        impl phosphor_core::core::debug_trace::DebugTrace for $type {}
        impl phosphor_core::core::machine::DipSwitches for $type {}
    };
}
pub(crate) use impl_default_frontend_capabilities;

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

pub mod astdelux;
pub mod asteroids;
pub mod atari_avg;
pub mod atari_dvg;
pub mod ccastles;
pub mod digdug;
pub mod donkey_kong;
pub mod donkey_kong_jr;
pub mod foodf;
pub mod galaga;
pub mod gottlieb;
pub mod gridlee;
pub(crate) mod input_defaults;
pub mod joust;
pub mod llander;
pub mod mcr2;
pub mod missile_command;
pub mod mspacman;
pub mod namco_galaga;
pub mod namco_pac;
pub mod pacman;
pub mod qbert;
pub mod registry;
pub mod robotron;
pub mod rom_loader;
pub mod satans_hollow;
pub mod simple_system;
pub mod tempest;
pub mod tkg04;
pub mod williams;

pub use astdelux::AsteroidsDeluxeSystem;
pub use asteroids::AsteroidsSystem;
pub use atari_dvg::AtariDvgBoard;
pub use ccastles::CrystalCastlesSystem;
pub use digdug::DigDugSystem;
pub use donkey_kong::DkongSystem;
pub use donkey_kong_jr::DkongJrSystem;
pub use foodf::FoodFightSystem;
pub use gridlee::GridleeSystem;
pub use joust::JoustSystem;
pub use llander::LunarLanderSystem;
pub use missile_command::MissileCommandSystem;
pub use mspacman::MsPacmanSystem;
pub use pacman::PacmanSystem;
pub use qbert::QbertSystem;
pub use robotron::RobotronSystem;
pub use satans_hollow::SatansHollowSystem;
pub use simple_system::{
    Simple6502System, Simple6800System, Simple6809System, SimpleI8035System, SimpleI8088System,
    SimpleSystem, SimpleSystem32, SimpleZ80System,
};
pub use tempest::TempestSystem;
