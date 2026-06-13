pub mod address_space;
pub mod address_space16;
pub mod address_space32;
pub mod bus;
pub mod clock;
pub mod component;
pub mod debug;
pub mod debug_trace;
pub mod machine;
pub mod save_state;
pub mod watchpoint;

pub use address_space::{AccessKind, DebugRead, DebugWrite, MemoryBacking, RegionId, UNMAPPED};
pub use address_space16::{AddressMap16, AddressSpace16, PageEntry, RegionDescriptor};
pub use address_space32::{AddressMap32, AddressRegion32, AddressSpace32, RegionTarget};
pub use bus::{Bus, BusMaster, InterruptState};
pub use clock::ClockDivider;
pub use component::BusMasterComponent;
pub use debug::{BusDebug, DebugCpu, DebugDisassembly, DebugRegister, Debuggable};
pub use debug_trace::{DebugEvent, DebugEventKind, DebugTrace, DebugTraceBuffer};
pub use machine::{
    AnalogAxisKind, AudioSource, AxisSign, DefaultBinding, Direction, FrontendMachine,
    InputConfigurable, InputControl, InputEvent, InputId, InputKind, KeyId, MachineCore,
    MachineDebug, MouseControl, Nvram, PadAxis, PadButton, PadControl, Profilable, ProfileSpan,
    Renderable, SaveState, TimingConfig,
};
pub use save_state::{SaveError, Saveable, StateReader, StateWriter, load_machine, save_machine};
pub use watchpoint::{
    DebugAccessSource, Watchpoint, WatchpointHit, WatchpointKind, WatchpointPhase, Watchpoints,
};
