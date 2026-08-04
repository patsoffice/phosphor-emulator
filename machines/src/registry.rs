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
    /// The machine's logical control table — the same slice its
    /// `input_controls()` returns.
    ///
    /// Held here so the registry can be validated without ROMs: `create`
    /// needs a [`RomSet`], which CI does not have, so this is the only way to
    /// check every machine's controls in a test.
    pub controls: &'static [InputControl],
}

impl MachineEntry {
    pub const fn new(
        name: &'static str,
        rom_names: &'static [&'static str],
        create: fn(&RomSet) -> Result<Box<dyn FrontendMachine>, RomLoadError>,
        controls: &'static [InputControl],
    ) -> Self {
        Self {
            name,
            rom_names,
            create,
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
