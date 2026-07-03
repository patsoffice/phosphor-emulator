//! Graphics region registry for the standalone `gfxview` tool.
//!
//! The main [`crate::registry`] maps a machine name to a factory, but it does
//! not record *which ROM bytes hold which tile/sprite sheet*, how those bytes
//! are bit-packed, or which color PROM tints them — knowledge the standalone
//! GFX viewer needs to decode a sheet offline without a running machine.
//!
//! Each machine that wants its graphics ROMs to be viewable self-registers one
//! [`GfxRegion`] per sheet via [`inventory::submit!`], the same pattern the
//! [`crate::disasm_registry`] uses for code regions. The registry is additive:
//! adding a region touches only the machine's own source file, never the
//! [`crate::registry::MachineEntry`] or
//! [`phosphor_core::core::machine::FrontendMachine`] surface.

use crate::rom_loader::{RomLoadError, RomSet};
use phosphor_core::gfx::GfxLayout;

/// Assembles a region's raw graphics bytes from a loaded ROM set.
pub type GfxLoad = fn(&RomSet) -> Result<Vec<u8>, RomLoadError>;

/// Builds a region's RGB palette from the machine's color PROM(s).
pub type PaletteLoad = fn(&RomSet) -> Result<Vec<(u8, u8, u8)>, RomLoadError>;

/// A decodable graphics region (tile or sprite sheet) of a machine.
///
/// The viewer feeds [`Self::load`]'s bytes plus [`Self::layout`] to
/// `phosphor_core::gfx::decode_gfx`, producing a `count`-element sheet of
/// `width × height` indexed tiles, then colors each index through
/// [`Self::palette`] (or a grayscale ramp when the machine has no color PROM).
pub struct GfxRegion {
    /// Machine CLI name, matching [`crate::registry::MachineEntry::name`].
    pub machine: &'static str,
    /// Region name (e.g. `"fg"`, `"bg"`, `"sprites"`).
    pub region: &'static str,
    /// Number of tiles/sprites to decode from the region.
    pub count: u32,
    /// Element width in pixels (e.g. 8 for tiles, 16/32 for sprites).
    pub width: u32,
    /// Element height in pixels.
    pub height: u32,
    /// Bit-plane layout describing how the ROM bytes pack into pixels.
    pub layout: &'static GfxLayout<'static>,
    /// Assemble the region's graphics bytes from a loaded ROM set.
    pub load: GfxLoad,
    /// Build the region's RGB palette from the machine's color PROM(s).
    ///
    /// `None` for machines with no color PROM (e.g. Williams, which drives
    /// color from a RAM palette): the viewer falls back to a grayscale ramp.
    pub palette: Option<PaletteLoad>,
}

inventory::collect!(GfxRegion);

/// All registered gfx regions for `machine`, sorted by region name.
pub fn regions_for(machine: &str) -> Vec<&'static GfxRegion> {
    let mut v: Vec<_> = inventory::iter::<GfxRegion>
        .into_iter()
        .filter(|r| r.machine == machine)
        .collect();
    v.sort_by_key(|r| r.region);
    v
}

/// Look up one `(machine, region)` gfx region.
pub fn find(machine: &str, region: &str) -> Option<&'static GfxRegion> {
    inventory::iter::<GfxRegion>
        .into_iter()
        .find(|r| r.machine == machine && r.region == region)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test-only registrations under a fake machine name. Real machines register
    // their sheets in their own source files (see gfxview child tasks); this
    // fixture keeps the registry's own behavior testable in isolation.
    static TEST_LAYOUT: GfxLayout<'static> = GfxLayout {
        plane_offsets: &[0, 1],
        x_offsets: &[0, 2, 4, 6, 8, 10, 12, 14],
        y_offsets: &[0, 16, 32, 48, 64, 80, 96, 112],
        char_increment: 128,
    };

    inventory::submit! {
        GfxRegion {
            machine: "gfx-registry-test",
            region: "sprites",
            count: 64,
            width: 16,
            height: 16,
            layout: &TEST_LAYOUT,
            load: |_rs| Ok(vec![0u8; 0x2000]),
            palette: Some(|_rs| Ok(vec![(0, 0, 0), (255, 255, 255)])),
        }
    }
    inventory::submit! {
        GfxRegion {
            machine: "gfx-registry-test",
            region: "tiles",
            count: 256,
            width: 8,
            height: 8,
            layout: &TEST_LAYOUT,
            load: |_rs| Ok(vec![0u8; 0x1000]),
            // No color PROM → viewer uses a grayscale ramp.
            palette: None,
        }
    }

    #[test]
    fn regions_registered_and_sorted() {
        let regions = regions_for("gfx-registry-test");
        assert_eq!(
            regions.iter().map(|r| r.region).collect::<Vec<_>>(),
            vec!["sprites", "tiles"],
            "expected both regions registered, sorted by region name"
        );
    }

    #[test]
    fn region_dimensions_and_palette() {
        let sprites = find("gfx-registry-test", "sprites").expect("sprites registered");
        assert_eq!((sprites.count, sprites.width, sprites.height), (64, 16, 16));
        assert!(
            sprites.palette.is_some(),
            "sprites region carries a PROM palette"
        );

        let tiles = find("gfx-registry-test", "tiles").expect("tiles registered");
        assert_eq!((tiles.count, tiles.width, tiles.height), (256, 8, 8));
        assert!(
            tiles.palette.is_none(),
            "PROM-less region falls back to grayscale (palette None)"
        );
    }

    #[test]
    fn load_matches_declared_size() {
        // The declared count/width/height describe the decoded sheet; the raw
        // loader yields the ROM bytes the decoder reads from.
        let rs = RomSet::from_slices(&[]);
        let sprites = find("gfx-registry-test", "sprites").unwrap();
        assert_eq!((sprites.load)(&rs).unwrap().len(), 0x2000);
    }

    #[test]
    fn unknown_machine_has_no_regions() {
        assert!(regions_for("does-not-exist").is_empty());
        assert!(find("gfx-registry-test", "nope").is_none());
    }
}
