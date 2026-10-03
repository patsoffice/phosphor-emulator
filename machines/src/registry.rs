//! Machine registry for automatic front-end discovery.
//!
//! Each front-end-capable machine self-registers via [`inventory::submit!`]
//! with a [`MachineEntry`] containing its CLI name, its ROM revisions, and
//! factory functions. The front-end discovers available machines at runtime
//! without any central list.

use phosphor_core::core::machine::{FrontendMachine, InputControl};

use crate::rom_loader::{RomLoadError, RomSet};

/// One ROM revision a machine can boot: something the emulator loads its own
/// way, with its own loader config.
///
/// The first name is the MAME set name; the rest are aliases: other archive
/// names the *same* loader accepts. A revision says "this dump needs this
/// config"; an alias says "this archive also satisfies the one config".
/// Aliases make no claim the dumps hold the same code.
pub struct Revision {
    /// Archive names this revision's loader accepts, in preference order.
    /// The first is the MAME set name: the `--rom-set` value and the
    /// save-state and movie tag.
    pub names: &'static [&'static str],
    /// NVRAM filename stem for this revision. `None` means the machine's own
    /// name, so revisions share one NVRAM file unless a machine says otherwise.
    pub nvram_group: Option<&'static str>,
}

impl Revision {
    /// MAME set name for this revision: the first name.
    pub fn set(&self) -> &'static str {
        self.names[0]
    }

    /// Alias archive names: everything after [`set`](Self::set).
    pub fn aliases(&self) -> &'static [&'static str] {
        &self.names[1..]
    }
}

/// Factory: build the machine from a loaded ROM set, with `rev` selecting
/// the revision to construct. `rev` must be below `revisions.len()`; only
/// that revision is attempted, never the rest in turn.
pub type CreateFn = fn(&RomSet, usize) -> Result<Box<dyn FrontendMachine>, RomLoadError>;

/// Describes a front-end-capable arcade machine.
pub struct MachineEntry {
    /// CLI name used to select this machine (e.g., "joust").
    pub name: &'static str,
    /// ROM revisions this machine boots, in preference order: the first is
    /// the default, and declaration order is the order to try them in.
    pub revisions: &'static [Revision],
    /// Factory: build the machine from a loaded ROM set (see [`CreateFn`]).
    pub create: CreateFn,
    /// Factory: construct the machine with **no ROMs loaded**, for the default
    /// revision.
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
    /// Factory: the ROM-less counterpart of [`create`](Self::create) for one
    /// revision. The blank build carries that revision's identity, so revision
    /// plumbing is testable with no ROMs at all. `rev` must be below
    /// `revisions.len()`, as with [`create`](Self::create).
    pub create_bare_revision: fn(usize) -> Box<dyn FrontendMachine>,
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
        revisions: &'static [Revision],
        create: CreateFn,
        create_bare: fn() -> Box<dyn FrontendMachine>,
        create_bare_revision: fn(usize) -> Box<dyn FrontendMachine>,
        controls: &'static [InputControl],
    ) -> Self {
        assert!(
            !revisions.is_empty(),
            "a machine needs at least one revision"
        );
        Self {
            name,
            revisions,
            create,
            create_bare,
            create_bare_revision,
            controls,
        }
    }

    /// The default revision: the first declared.
    pub fn default_revision(&self) -> &Revision {
        &self.revisions[0]
    }

    /// Revision index for a MAME set name or alias, or `None` when this
    /// machine knows no archive by that name.
    pub fn find_revision(&self, name: &str) -> Option<usize> {
        self.revisions.iter().position(|r| r.names.contains(&name))
    }

    /// Every archive name this machine accepts, revision by revision. For
    /// presence checks and "tried" messages; loading itself goes revision
    /// by revision instead.
    pub fn archive_names(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        for r in self.revisions {
            out.extend(r.names.iter().copied());
        }
        out
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
