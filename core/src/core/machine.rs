use std::time::Duration;

use crate::device::dvg::VectorLine;

use super::address_space16::AddressSpace16;
use super::debug::BusDebug;
use super::debug_trace::DebugTrace;
use super::save_state::SaveError;
use super::watchpoint::{WatchpointHit, WatchpointKind};

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

// ---------------------------------------------------------------------------
// Typed input configuration
//
// Machines expose stable logical controls and consume typed events, so the
// frontend can persist per-machine bindings by stable name rather than by
// display text.
// ---------------------------------------------------------------------------

/// Stable identifier for a logical input control within a machine.
///
/// An `InputId` is paired with a stable string name in [`InputControl`], so
/// persistent bindings survive machine-side renumbering. The frontend keys
/// saved bindings by the control's `stable_name`, never by this numeric value
/// or by display text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct InputId(pub u16);

/// Joystick / D-pad direction for a digital directional control.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// Which analog axis a continuous control drives.
///
/// Trackballs, spinners, wheels, and paddles all reduce to a one-dimensional
/// axis; `X` is horizontal motion, `Y` is vertical.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnalogAxisKind {
    X,
    Y,
}

/// The semantic role of a logical control, used by the frontend to pick
/// sensible default physical bindings and to group the rebinding UI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    /// A generic action button (fire, flap, jump, throw…).
    Button,
    /// Coin / credit insert.
    Coin,
    /// Player start.
    Start,
    /// Operator service / self-test button.
    Service,
    /// One direction of a digital (switch) joystick.
    DigitalDirection { direction: Direction },
    /// A continuous analog axis (trackball, spinner, wheel, paddle).
    AnalogAxis { axis: AnalogAxisKind },
}

/// A keyboard key, mirrored from common SDL scancodes but free of any SDL
/// dependency so `phosphor-core` stays platform-agnostic. The frontend
/// translates these into `sdl2::keyboard::Scancode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[rustfmt::skip]
pub enum KeyId {
    A, B, C, D, E, F, G, H, I, J, K, L, M,
    N, O, P, Q, R, S, T, U, V, W, X, Y, Z,
    Num0, Num1, Num2, Num3, Num4, Num5, Num6, Num7, Num8, Num9,
    Up, Down, Left, Right,
    Space, Enter, Tab, Escape,
    LShift, RShift, LCtrl, RCtrl, LAlt, RAlt,
}

/// A gamepad button, free of SDL dependency. Mirrors the SDL game-controller
/// button set; the frontend translates these into `sdl2::controller::Button`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadButton {
    A,
    B,
    X,
    Y,
    Back,
    Start,
    Guide,
    LeftShoulder,
    RightShoulder,
    LeftStick,
    RightStick,
    DPadUp,
    DPadDown,
    DPadLeft,
    DPadRight,
}

/// A gamepad analog axis, free of SDL dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadAxis {
    LeftX,
    LeftY,
    RightX,
    RightY,
    TriggerLeft,
    TriggerRight,
}

/// Sign of an axis deflection, so a gamepad axis can stand in for a digital input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxisSign {
    Positive,
    Negative,
}

/// A gamepad control referenced by a default binding: either a button or a
/// signed axis deflection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadControl {
    Button(PadButton),
    Axis(PadAxis, AxisSign),
}

/// A mouse control referenced by a default binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseControl {
    Left,
    Right,
    Middle,
    AxisX,
    AxisY,
}

/// A default physical binding suggested by a machine for one of its controls.
///
/// These are portable (SDL-free) descriptors; the frontend resolves them to
/// concrete devices and lets the user override them. A control may suggest
/// several defaults (e.g. keyboard *and* gamepad).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DefaultBinding {
    Key(KeyId),
    Pad(PadControl),
    Mouse(MouseControl),
}

/// A single logical control exposed by a machine.
///
/// The frontend binds physical inputs to these controls and persists the
/// bindings by `stable_name`. `label` is for display only and may change
/// without breaking saved configs.
#[derive(Clone, Copy, Debug)]
pub struct InputControl {
    /// Machine-local identifier, echoed back in [`InputEvent`].
    pub id: InputId,
    /// Stable, machine-unique key for persistence (e.g. "p1_flap"). Never
    /// display text — renaming the label must not change this.
    pub stable_name: &'static str,
    /// Human-readable label for the rebinding UI (e.g. "P1 Flap").
    pub label: &'static str,
    /// Semantic role, used for default bindings and UI grouping.
    pub kind: InputKind,
    /// Owning player (1-based), or `None` for shared / system controls.
    pub player: Option<u8>,
    /// Suggested default physical bindings.
    pub default_bindings: &'static [DefaultBinding],
}

/// An input event from the frontend, targeting a logical control by `InputId`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum InputEvent {
    /// Digital press / release.
    Button { id: InputId, pressed: bool },
    /// Absolute analog position in `-1.0..=1.0` (e.g. an analog stick).
    Absolute { id: InputId, value: f32 },
    /// Relative motion delta (e.g. trackball / spinner / mouse), in device units.
    Relative { id: InputId, delta: f32 },
}

/// Typed, rebindable input configuration.
///
/// Machines expose stable logical controls via [`input_controls`](Self::input_controls)
/// and consume typed events via [`handle_input`](Self::handle_input), letting the
/// frontend persist per-machine bindings by stable name rather than by display
/// text.
pub trait InputConfigurable {
    /// The logical controls this machine exposes (stable names, kinds, and
    /// default physical bindings).
    fn input_controls(&self) -> &'static [InputControl];

    /// Handle a typed input event, applying it to the machine's hardware input
    /// state.
    fn handle_input(&mut self, event: InputEvent);
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
    fn set_watchpoint(&mut self, cpu_index: usize, addr: u32, kind: WatchpointKind) {
        if let Some(bus) = self.debug_bus_mut() {
            bus.set_watchpoint(cpu_index, addr, kind);
        }
    }

    /// Clear a memory watchpoint in the address space of `cpu_index`.
    ///
    /// Default: delegates to `BusDebug::clear_watchpoint()` via `debug_bus_mut()`.
    fn clear_watchpoint(&mut self, cpu_index: usize, addr: u32, kind: WatchpointKind) {
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
    fn memory_map(&self, cpu_index: usize) -> Option<&AddressSpace16> {
        self.debug_bus()?.memory_map(cpu_index)
    }
}

// ---------------------------------------------------------------------------
// DIP switches
//
// Machines expose static metadata describing their DIP switch banks and own
// the live byte(s); the frontend reads/writes whole bank bytes or individual
// options, persisting selections by stable bank/option position.
// ---------------------------------------------------------------------------

/// When a DIP switch change takes hardware effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DipApplyTiming {
    /// The change is visible to the game on the next bus read of the bank.
    Immediate,
    /// The change only takes effect after the machine is reset (the game
    /// latches the value at power-on / reset and ignores later edits).
    OnReset,
}

/// One selectable value of a [`DipOption`], paired with its display label.
#[derive(Clone, Copy, Debug)]
pub struct DipChoice {
    /// Human-readable label for this setting (e.g. "3 Lives", "Hard").
    pub label: &'static str,
    /// The bits this choice sets within the owning option's `mask`. Only the
    /// masked bits are significant; the rest of the bank byte is untouched.
    pub value: u8,
}

/// A single logical DIP setting within a bank (e.g. "Lives", "Difficulty").
#[derive(Clone, Copy, Debug)]
pub struct DipOption {
    /// Human-readable name of the setting.
    pub name: &'static str,
    /// Which bits of the bank byte this option occupies.
    pub mask: u8,
    /// The selectable values for this option.
    pub choices: &'static [DipChoice],
    /// When edits to this option take effect.
    pub apply: DipApplyTiming,
}

/// Static metadata describing one physical DIP switch bank (a single byte).
///
/// The bank's live byte value is owned by the machine, not stored here; the
/// frontend reads it via [`DipSwitches::dip_bank_value`].
#[derive(Clone, Copy, Debug)]
pub struct DipSwitchBank {
    /// Human-readable name of the bank (e.g. "DSW1").
    pub name: &'static str,
    /// The options packed into this bank's byte.
    pub options: &'static [DipOption],
}

/// User-settable DIP switch configuration.
///
/// Machines expose static bank metadata via [`dip_banks`](Self::dip_banks) and
/// own the live byte(s), surfaced through [`dip_bank_value`](Self::dip_bank_value)
/// and mutated via [`set_dip_bank_value`](Self::set_dip_bank_value). Machines
/// remain responsible for mapping the live byte(s) into their board-specific
/// input-port state. Systems without DIP switches use the defaults (no banks).
pub trait DipSwitches {
    /// Static metadata for each DIP bank this machine exposes, in bank order.
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        &[]
    }

    /// Current live byte value of bank `bank` (0 if out of range).
    fn dip_bank_value(&self, _bank: usize) -> u8 {
        0
    }

    /// Replace the entire live byte value of bank `bank` (no-op if out of range).
    fn set_dip_bank_value(&mut self, _bank: usize, _value: u8) {}

    /// Set a single option within a bank, masking `value` into the bank byte.
    ///
    /// The default merges `value` into the live bank byte using the option's
    /// `mask`, leaving other options' bits untouched; it is a no-op if the
    /// bank or option index is out of range.
    fn set_dip_option(&mut self, bank: usize, option: usize, value: u8) {
        let Some(mask) = self
            .dip_banks()
            .get(bank)
            .and_then(|b| b.options.get(option))
            .map(|o| o.mask)
        else {
            return;
        };
        let merged = (self.dip_bank_value(bank) & !mask) | (value & mask);
        self.set_dip_bank_value(bank, merged);
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
    + InputConfigurable
    + DipSwitches
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
        + InputConfigurable
        + DipSwitches
        + MachineDebug
        + DebugTrace
        + SaveState
        + Nvram
        + Profilable
{
}

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
        impl InputConfigurable for Dummy {
            fn input_controls(&self) -> &'static [InputControl] {
                &[]
            }
            fn handle_input(&mut self, _event: InputEvent) {}
        }
        impl MachineDebug for Dummy {}
        impl DebugTrace for Dummy {}
        impl DipSwitches for Dummy {}
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
