//! Disassembly region registry for the standalone `disasm` tool.
//!
//! The main [`crate::registry`] maps a machine name to a factory, but it does
//! not record *which CPU runs which ROM region* — knowledge the standalone
//! disassembler needs to dump, say, the Mario Bros sound program without the
//! caller having to know it is an I8035 at origin `0`.
//!
//! Each machine that wants its code ROMs to be disassemblable self-registers one
//! [`DisasmRegion`] per region via [`inventory::submit!`], the same pattern the
//! machine registry uses. The registry is additive: adding a region touches only
//! the machine's own source file, never the [`crate::registry::MachineEntry`] or
//! [`phosphor_core::core::machine::FrontendMachine`] surface.

use crate::rom_loader::{RomLoadError, RomSet};

/// The CPU that executes a given code region, by family.
///
/// Mirrors the per-CPU disassemblers in `phosphor-core`. The standalone tool
/// maps each variant to the corresponding `phosphor_core::cpu::*` type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisasmCpu {
    I8035,
    Z80,
    M6809,
    M6800,
    M6502,
    M68000,
    Mb88xx,
}

impl DisasmCpu {
    /// Lowercase canonical name (matches the tool's `--cpu` value names).
    pub fn name(self) -> &'static str {
        match self {
            DisasmCpu::I8035 => "i8035",
            DisasmCpu::Z80 => "z80",
            DisasmCpu::M6809 => "m6809",
            DisasmCpu::M6800 => "m6800",
            DisasmCpu::M6502 => "m6502",
            DisasmCpu::M68000 => "m68000",
            DisasmCpu::Mb88xx => "mb88xx",
        }
    }
}

/// A disassemblable code region of a machine.
pub struct DisasmRegion {
    /// Machine CLI name, matching [`crate::registry::MachineEntry::name`].
    pub machine: &'static str,
    /// Region name (e.g. `"main"`, `"sound"`).
    pub region: &'static str,
    /// CPU that executes this region.
    pub cpu: DisasmCpu,
    /// Load/origin address of the first byte (for relative branch targets).
    pub org: u32,
    /// Assemble the region's bytes from a loaded ROM set.
    pub load: fn(&RomSet) -> Result<Vec<u8>, RomLoadError>,
}

inventory::collect!(DisasmRegion);

/// All registered disasm regions for `machine`, sorted by region name.
pub fn regions_for(machine: &str) -> Vec<&'static DisasmRegion> {
    let mut v: Vec<_> = inventory::iter::<DisasmRegion>
        .into_iter()
        .filter(|r| r.machine == machine)
        .collect();
    v.sort_by_key(|r| r.region);
    v
}

/// Look up one `(machine, region)` disasm region.
pub fn find(machine: &str, region: &str) -> Option<&'static DisasmRegion> {
    inventory::iter::<DisasmRegion>
        .into_iter()
        .find(|r| r.machine == machine && r.region == region)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mario_bros_regions_registered() {
        let regions = regions_for("mariobros");
        assert_eq!(
            regions.iter().map(|r| r.region).collect::<Vec<_>>(),
            vec!["main", "sound"],
            "expected Mario Bros to register 'main' and 'sound' (sorted)"
        );

        let sound = find("mariobros", "sound").expect("sound region registered");
        assert_eq!(sound.cpu, DisasmCpu::I8035);
        assert_eq!(sound.org, 0);

        let main = find("mariobros", "main").expect("main region registered");
        assert_eq!(main.cpu, DisasmCpu::Z80);
    }

    #[test]
    fn unknown_machine_has_no_regions() {
        assert!(regions_for("does-not-exist").is_empty());
        assert!(find("mariobros", "nope").is_none());
    }
}
