//! The "save what the CPU can write" rule, checked against every registered
//! machine's real memory map rather than a synthetic one.
//!
//! `AddressSpace16`'s `Saveable` impl derives the saved set from each region's
//! `AccessKind` instead of taking a list from the board. That rule is only
//! useful if it agrees with how boards actually build their maps, and boards do
//! things a hand-written test map does not: ROM pools with no address window,
//! banked regions, mirrors, I/O interleaved with RAM.
//!
//! These run over `create_bare`, so they need no ROM collection.

use phosphor_core::core::address_space::AccessKind;
use phosphor_machines::registry;

/// The rule has to actually select something across the roster, or the two
/// property tests below would hold vacuously on empty sets.
///
/// Not every machine qualifies, and that is a real finding rather than a
/// tolerance: **some boards keep their memory in plain fields rather than in
/// the address space.** Burgertime is one, holding `ram`, `videoram`,
/// `colorram`, `palette_ram` and `sound_ram` directly and registering only ROM
/// in its map. Those boards get nothing from an address space that knows how to
/// save itself, so they are not candidates for that conversion.
#[test]
fn the_rule_selects_regions_across_the_roster() {
    let backing_state = registry::all()
        .iter()
        .filter_map(|e| {
            let machine = (e.create_bare)();
            let map = machine.memory_map(0)?;
            (!map.saved_region_ids().is_empty()).then_some(e.name)
        })
        .count();

    assert!(
        backing_state > 20,
        "only {backing_state} machines' main maps back CPU-writable memory; \
         the rule has probably stopped selecting anything"
    );
}

/// The rule must pick exactly the regions the map marks writable and backed.
/// Stated as a property rather than a fixed list, so it keeps holding as
/// machines are added.
#[test]
fn the_saved_set_is_exactly_the_writable_backed_regions() {
    for entry in registry::all() {
        let machine = (entry.create_bare)();
        let Some(map) = machine.memory_map(0) else {
            continue;
        };
        let saved = map.saved_region_ids();

        for region in map.regions() {
            let writable = region.access.is_cpu_writable();
            let listed = saved.contains(&region.id);
            if writable && !listed {
                // Writable but unlisted is only legitimate when it has no bytes.
                assert!(
                    !map.has_backing(region.id),
                    "{}: region {} ({}) is writable and backed but would not be saved",
                    entry.name,
                    region.id,
                    region.name
                );
            }
            if listed {
                assert!(
                    writable,
                    "{}: region {} ({}) would be saved but is {:?}",
                    entry.name, region.id, region.name, region.access
                );
            }
        }
    }
}

/// ROM is the reason the rule is worth having: it is the bulk of most maps and
/// the thing a hand-written list must remember to leave out.
#[test]
fn no_read_only_region_is_ever_saved() {
    for entry in registry::all() {
        let machine = (entry.create_bare)();
        let Some(map) = machine.memory_map(0) else {
            continue;
        };
        let saved = map.saved_region_ids();

        for region in map.regions() {
            if matches!(region.access, AccessKind::ReadOnly) {
                assert!(
                    !saved.contains(&region.id),
                    "{}: ROM region {} ({}) would be saved",
                    entry.name,
                    region.id,
                    region.name
                );
            }
        }
    }
}

/// Guard against the sweep passing because the registry is empty.
#[test]
fn the_registry_is_not_empty() {
    assert!(registry::all().len() > 20);
}
