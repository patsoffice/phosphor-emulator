/// Describes a single input button that a machine accepts.
pub struct InputButton {
    /// Machine-defined button identifier, passed to `set_input()`.
    pub id: u8,
    /// Human-readable name for display/configuration (e.g., "P1 Left", "Coin").
    pub name: &'static str,
}

/// Describes an analog axis that a machine accepts (trackball, spinner, etc.).
pub struct AnalogInput {
    /// Machine-defined axis identifier, passed to `set_analog()`.
    pub id: u8,
    /// Human-readable name for display/configuration (e.g., "Trackball X").
    pub name: &'static str,
}

use std::time::Duration;

use crate::device::dvg::VectorLine;

use super::debug::BusDebug;
use super::debug_trace::DebugTrace;
use super::memory_map::{MemoryMap, WatchpointHit, WatchpointKind};
use super::save_state::SaveError;

/// A named timing span from machine-level profiling.
///
/// Machines that implement `set_profiling(true)` can capture per-device or
/// per-CPU timing during `run_frame()` and return spans via `frame_profile_spans()`.
pub struct ProfileSpan {
    pub name: &'static str,
    pub duration: Duration,
}

// ---------------------------------------------------------------------------
// Timing configuration
// ---------------------------------------------------------------------------

/// Timing and display configuration for an emulated machine.
///
/// Provides a single source of truth for CPU clock rate, scanline timing,
/// and display dimensions. Derived values ([`cycles_per_frame`](Self::cycles_per_frame),
/// [`frame_rate_hz`](Self::frame_rate_hz)) are computed from these fields to
/// prevent inconsistencies.
pub struct TimingConfig {
    pub cpu_clock_hz: u64,
    pub cycles_per_scanline: u64,
    pub total_scanlines: u64,
    pub display_width: u32,
    pub display_height: u32,
}

impl TimingConfig {
    pub const fn cycles_per_frame(&self) -> u64 {
        self.total_scanlines * self.cycles_per_scanline
    }

    pub const fn frame_rate_hz(&self) -> f64 {
        self.cpu_clock_hz as f64 / self.cycles_per_frame() as f64
    }

    pub const fn display_size(&self) -> (u32, u32) {
        (self.display_width, self.display_height)
    }
}

/// Screen rotation applied at the display level (after vector generation).
///
/// Matches MAME's screen orientation flags. The rotation is applied by the
/// rendering layer, not by the game hardware.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScreenRotation {
    #[default]
    None,
    Rot270,
}

// ---------------------------------------------------------------------------
// Sub-traits
// ---------------------------------------------------------------------------

/// Video output capabilities: display size and frame rendering.
pub trait Renderable {
    /// Native display resolution as (width, height) in pixels.
    fn display_size(&self) -> (u32, u32);

    /// Render the current video state into an RGB24 pixel buffer.
    ///
    /// The buffer must be at least `width * height * 3` bytes (from `display_size()`).
    /// Pixels are stored left-to-right, top-to-bottom, 3 bytes per pixel (R, G, B).
    ///
    /// The machine is responsible for converting its internal video representation
    /// (e.g., 4bpp column-major video RAM + palette) into this standard format.
    fn render_frame(&self, buffer: &mut [u8]);

    /// Optional debug overlay text (e.g., dirty-tracking stats).
    ///
    /// Returns a short string to display below the FPS counter when the
    /// overlay is active. Machines without stats return `None` (the default).
    fn overlay_stats(&self) -> Option<String> {
        None
    }

    /// Return the vector display list for direct GL rendering, if this is
    /// a vector display machine. Raster machines return `None` (the default).
    fn vector_display_list(&self) -> Option<&[VectorLine]> {
        None
    }

    /// Screen rotation applied at the display level.
    ///
    /// Vector machines like Tempest use ROT270 to rotate the AVG output
    /// for portrait display. Default is no rotation.
    fn screen_rotation(&self) -> ScreenRotation {
        ScreenRotation::None
    }
}

/// Audio output capabilities: PCM sample generation.
///
/// Machines without audio hardware can skip implementing this trait
/// (defaults produce silence with a zero sample rate).
pub trait AudioSource {
    /// Fill the buffer with mono i16 PCM samples at the machine's native
    /// sample rate. Returns the number of samples written.
    fn fill_audio(&mut self, _buffer: &mut [i16]) -> usize {
        0 // default: silence
    }

    /// Native audio sample rate in Hz (e.g., 894886 / some divisor).
    fn audio_sample_rate(&self) -> u32 {
        0
    }
}

/// Input handling: buttons and analog axes.
pub trait InputReceiver {
    /// Handle an input event. `button` is a machine-defined ID from `input_map()`.
    /// `pressed` is true for key-down, false for key-up.
    ///
    /// Called per-event, not per-frame. The frontend may call this multiple times
    /// between frames as input events arrive. Each call latches the button state
    /// so that `run_frame()` sees the accumulated input.
    fn set_input(&mut self, button: u8, pressed: bool);

    /// Get the list of input buttons this machine accepts.
    /// The frontend uses this to build key mappings and display configuration UI.
    fn input_map(&self) -> &[InputButton];

    /// Handle an analog input event. `axis` is a machine-defined ID from `analog_map()`.
    /// `delta` is a signed motion value (e.g., mouse dx/dy in pixels).
    ///
    /// Called per-event as motion occurs. The machine accumulates deltas internally.
    fn set_analog(&mut self, _axis: u8, _delta: i32) {}

    /// Get the list of analog axes this machine accepts.
    /// The frontend uses this to determine whether to capture mouse/trackball motion.
    fn analog_map(&self) -> &[AnalogInput] {
        &[]
    }
}

/// Debug/inspection capabilities for interactive debugging.
///
/// Machines without debug support can skip implementing this trait
/// (defaults return None / 0, disabling the debugger).
pub trait MachineDebug {
    /// Access bus debug capabilities (shared ref — reads, device/CPU discovery).
    fn debug_bus(&self) -> Option<&dyn BusDebug> {
        None
    }

    /// Access bus debug capabilities (mutable ref — writes).
    fn debug_bus_mut(&mut self) -> Option<&mut dyn BusDebug> {
        None
    }

    /// Number of clock ticks per frame (used by debug UI for cycle counting in run mode).
    fn cycles_per_frame(&self) -> u64 {
        0
    }

    /// Advance one cycle. Returns bitmask of CPUs at instruction boundaries.
    /// Bit 0 = CPU 0, bit 1 = CPU 1, etc.
    fn debug_tick(&mut self) -> u32 {
        0
    }

    /// Consume a pending watchpoint hit from the last tick, if any.
    ///
    /// The debugger polls this after each `debug_tick()`. When `Some` is
    /// returned, the debugger pauses execution and displays the hit.
    ///
    /// Default: delegates to `BusDebug::take_watchpoint_hit()` via `debug_bus_mut()`.
    fn take_watchpoint_hit(&mut self) -> Option<WatchpointHit> {
        self.debug_bus_mut()
            .and_then(|bus| bus.take_watchpoint_hit())
    }

    /// Set a memory watchpoint in the address space of `cpu_index`.
    ///
    /// Default: delegates to `BusDebug::set_watchpoint()` via `debug_bus_mut()`.
    fn set_watchpoint(&mut self, cpu_index: usize, addr: u16, kind: WatchpointKind) {
        if let Some(bus) = self.debug_bus_mut() {
            bus.set_watchpoint(cpu_index, addr, kind);
        }
    }

    /// Clear a memory watchpoint in the address space of `cpu_index`.
    ///
    /// Default: delegates to `BusDebug::clear_watchpoint()` via `debug_bus_mut()`.
    fn clear_watchpoint(&mut self, cpu_index: usize, addr: u16, kind: WatchpointKind) {
        if let Some(bus) = self.debug_bus_mut() {
            bus.clear_watchpoint(cpu_index, addr, kind);
        }
    }

    /// Clear all memory watchpoints across all address spaces.
    ///
    /// Default: delegates to `BusDebug::clear_all_watchpoints()` via `debug_bus_mut()`.
    fn clear_all_watchpoints(&mut self) {
        if let Some(bus) = self.debug_bus_mut() {
            bus.clear_all_watchpoints();
        }
    }

    /// Get the memory map for a CPU's address space (for region introspection).
    ///
    /// Default: delegates to `BusDebug::memory_map()` via `debug_bus()`.
    fn memory_map(&self, cpu_index: usize) -> Option<&MemoryMap> {
        self.debug_bus()?.memory_map(cpu_index)
    }
}

/// Save-state capability: snapshot and restore complete machine state.
///
/// Machines without save-state support use the defaults (no snapshot,
/// load returns an error).
pub trait SaveState {
    /// Capture complete machine state for later restoration.
    /// Returns `None` if this machine does not support save states.
    fn save_state(&self) -> Option<Vec<u8>> {
        None
    }

    /// Restore machine state from a previous `save_state()` snapshot.
    fn load_state(&mut self, _data: &[u8]) -> Result<(), SaveError> {
        Err(SaveError::InvalidFormat("save states not supported".into()))
    }
}

/// Battery-backed RAM persistence.
///
/// The frontend owns NVRAM file loading/saving; machines with battery-backed
/// RAM expose its contents here. Machines without NVRAM use the defaults.
pub trait Nvram {
    /// Return battery-backed RAM contents for saving, or None if this machine has none.
    fn save_nvram(&self) -> Option<&[u8]> {
        None
    }

    /// Load battery-backed RAM contents from a previous save.
    fn load_nvram(&mut self, _data: &[u8]) {}
}

/// Frame-level profiling instrumentation.
///
/// Every machine can be profiled at frame granularity by the frontend;
/// machines that capture per-device sub-spans override these methods.
pub trait Profilable {
    /// Enable or disable internal sub-span profiling.
    ///
    /// Machines that support fine-grained timing should start/stop capturing
    /// per-device or per-CPU measurements when this is called.
    fn set_profiling(&mut self, _enabled: bool) {}

    /// Return sub-span timing from the last `run_frame()` call.
    ///
    /// Machines that override `set_profiling` can report detailed breakdowns
    /// (e.g., main CPU, sound CPU, scanline rendering, blitter DMA).
    fn frame_profile_spans(&self) -> &[ProfileSpan] {
        &[]
    }
}

// ---------------------------------------------------------------------------
// Core machine trait
// ---------------------------------------------------------------------------

/// Minimum contract for an emulated system that advances in frames.
///
/// This is the core execution trait: it carries no display, audio, input,
/// or persistence concerns. Optional frontend services live in capability
/// traits ([`SaveState`], [`Nvram`], [`Profilable`], etc.) and are bundled
/// for frontend use by [`FrontendMachine`].
pub trait MachineCore {
    /// Run one frame of emulation (advance the clock by one frame's worth of cycles).
    fn run_frame(&mut self);

    /// Reset the machine to its initial power-on state.
    fn reset(&mut self);

    /// Native frame rate in Hz (e.g., 60.10 for Joust, 61.04 for Missile Command).
    /// Used by the frontend for real-time frame throttling.
    fn frame_rate_hz(&self) -> f64 {
        60.0
    }

    /// Short identifier for this machine type (e.g., "joust", "pacman").
    /// Used to validate save files against the correct machine.
    fn machine_id(&self) -> &str {
        ""
    }
}

// ---------------------------------------------------------------------------
// Frontend bundle trait
// ---------------------------------------------------------------------------

/// The full machine contract for the SDL frontend.
///
/// The frontend is machine-agnostic: it receives a `Box<dyn FrontendMachine>`
/// from the registry and drives display, audio, input, debugging, save
/// states, NVRAM, and profiling through trait methods.
///
/// This trait is implemented automatically (blanket impl) for any type that
/// implements [`MachineCore`] plus all the capability traits. Machines never
/// implement it directly.
pub trait FrontendMachine:
    MachineCore
    + Renderable
    + AudioSource
    + InputReceiver
    + MachineDebug
    + DebugTrace
    + SaveState
    + Nvram
    + Profilable
{
}

impl<T> FrontendMachine for T where
    T: MachineCore
        + Renderable
        + AudioSource
        + InputReceiver
        + MachineDebug
        + DebugTrace
        + SaveState
        + Nvram
        + Profilable
{
}

/// Compatibility alias for [`FrontendMachine`] during the capability-trait
/// migration. New code should use [`FrontendMachine`] (frontend bundle) or
/// [`MachineCore`] (execution contract) directly.
pub trait Machine: FrontendMachine {}

impl<T> Machine for T where T: FrontendMachine {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smoke test: a type implementing `MachineCore` + all capability traits
    /// gets `FrontendMachine` via the blanket impl and coerces to the
    /// object-safe `dyn FrontendMachine`.
    #[test]
    fn blanket_impl_provides_frontend_machine() {
        struct Dummy;

        impl MachineCore for Dummy {
            fn run_frame(&mut self) {}
            fn reset(&mut self) {}
        }
        impl Renderable for Dummy {
            fn display_size(&self) -> (u32, u32) {
                (1, 1)
            }
            fn render_frame(&self, _buffer: &mut [u8]) {}
        }
        impl AudioSource for Dummy {}
        impl InputReceiver for Dummy {
            fn set_input(&mut self, _button: u8, _pressed: bool) {}
            fn input_map(&self) -> &[InputButton] {
                &[]
            }
        }
        impl MachineDebug for Dummy {}
        impl DebugTrace for Dummy {}
        impl SaveState for Dummy {}
        impl Nvram for Dummy {}
        impl Profilable for Dummy {}

        let mut dummy = Dummy;
        let machine: &mut dyn FrontendMachine = &mut dummy;
        machine.run_frame();
        assert_eq!(machine.frame_rate_hz(), 60.0);
        assert!(machine.save_state().is_none());
        assert!(machine.save_nvram().is_none());
        assert!(machine.frame_profile_spans().is_empty());
    }
}
