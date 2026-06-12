pub mod address_space32;
pub mod bus;
pub mod clock;
pub mod component;
pub mod debug;
pub mod debug_trace;
pub mod machine;
pub mod memory_map;
pub mod save_state;
pub mod watchpoint;

pub use address_space32::{AddressMap32, AddressRegion32, RegionTarget};
pub use bus::{Bus, BusMaster, InterruptState};
pub use clock::ClockDivider;
pub use component::BusMasterComponent;
pub use debug::{BusDebug, DebugCpu, DebugDisassembly, DebugRegister, Debuggable};
pub use debug_trace::{DebugEvent, DebugEventKind, DebugTrace, DebugTraceBuffer};
pub use machine::{
    AnalogInput, AudioSource, FrontendMachine, InputButton, InputReceiver, Machine, MachineCore,
    MachineDebug, Nvram, Profilable, ProfileSpan, Renderable, SaveState, TimingConfig,
};
pub use memory_map::{
    AddressMap16, AddressSpace16, DebugRead, DebugWrite, MemoryBacking, MemoryMap,
};
pub use save_state::{SaveError, Saveable, StateReader, StateWriter, load_machine, save_machine};
pub use watchpoint::{
    DebugAccessSource, Watchpoint, WatchpointHit, WatchpointKind, WatchpointPhase, Watchpoints,
};
