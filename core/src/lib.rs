// Doc links to private items are allowed on purpose. These are workspace-internal
// crates that are never published, and their docs are read with
// `--document-private-items`, where such a link resolves and is useful. Rewriting
// them as plain text to satisfy a lint aimed at published crates would only make
// the cross-references harder to follow.
#![allow(rustdoc::private_intra_doc_links)]

extern crate self as phosphor_core;

pub mod audio;
pub mod core;
pub mod cpu;
pub mod device;
pub mod dirty_bitset;
pub mod gfx;

pub use cpu::m68000::M68000;

pub mod prelude {
    pub use crate::core::machine::{
        DefaultBinding, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, DipSwitches,
        FrontendMachine, InputConfigurable, InputControl, InputEvent, InputId, InputKind, KeyId,
        MachineCore, MouseControl, Nvram, PadControl, Profilable, SaveState,
    };
    pub use crate::core::{
        Bus, BusMaster, BusMasterComponent, SaveError, Saveable, StateReader, StateWriter,
        bus::InterruptState,
    };
    pub use crate::cpu::Cpu;
    pub use phosphor_macros::Saveable;
}
