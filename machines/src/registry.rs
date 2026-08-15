//! Machine registry for automatic front-end discovery.
//!
//! Each front-end-capable machine self-registers via [`inventory::submit!`]
//! with a [`MachineEntry`] containing its CLI name, MAME ROM set name, and a
//! factory function. The front-end discovers available machines at runtime
//! without any central list.

use phosphor_core::core::machine::{FrontendMachine, InputControl};

use crate::rom_loader::{RomLoadError, RomSet};

/// Describes a front-end-capable arcade machine.
pub struct MachineEntry {
    /// CLI name used to select this machine (e.g., "joust").
    pub name: &'static str,
    /// MAME ROM set names to try for ZIP lookup, in priority order.
    pub rom_names: &'static [&'static str],
    /// Factory: construct a FrontendMachine from a loaded ROM set.
    pub create: fn(&RomSet) -> Result<Box<dyn FrontendMachine>, RomLoadError>,
    /// Factory: construct the machine with **no ROMs loaded**.
    ///
    /// The same constructor [`create`](Self::create) uses, with the
    /// `load_rom_set` step omitted: real hardware structs, real devices,
    /// zero-filled ROM. Such a machine cannot run its game — a zero-filled
    /// ROM decodes to whatever the CPU makes of it — but it is a complete,
    /// tickable machine, which is the point: registry-driven tests can reach
    /// *behavior* (rendering, DIP accessors, save state, `run_frame`) rather
    /// than only the static metadata on this struct.
    ///
    /// Exists because `create` needs a [`RomSet`] and CI has none. Tests that
    /// need a machine which has really booted go through `create` and gate
    /// themselves on a ROM directory being present.
    pub create_bare: fn() -> Box<dyn FrontendMachine>,
    /// The machine's logical control table — the same slice its
    /// `input_controls()` returns.
    ///
    /// Held here so the control table can be validated without constructing
    /// anything at all.
    pub controls: &'static [InputControl],
}

impl MachineEntry {
    pub const fn new(
        name: &'static str,
        rom_names: &'static [&'static str],
        create: fn(&RomSet) -> Result<Box<dyn FrontendMachine>, RomLoadError>,
        create_bare: fn() -> Box<dyn FrontendMachine>,
        controls: &'static [InputControl],
    ) -> Self {
        Self {
            name,
            rom_names,
            create,
            create_bare,
            controls,
        }
    }
}

inventory::collect!(MachineEntry);

/// Return all registered front-end-capable machines, sorted by name.
pub fn all() -> Vec<&'static MachineEntry> {
    let mut entries: Vec<_> = inventory::iter::<MachineEntry>.into_iter().collect();
    entries.sort_by_key(|e| e.name);
    entries
}

/// Look up a machine by its CLI name.
pub fn find(name: &str) -> Option<&'static MachineEntry> {
    inventory::iter::<MachineEntry>
        .into_iter()
        .find(|e| e.name == name)
}
