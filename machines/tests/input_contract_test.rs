//! Registry-wide invariants for machine input tables.
//!
//! The typed input model rests on a few properties that nothing else checks:
//! `InputId`s must be unique within a machine (they are the `handle_input`
//! dispatch key) and `stable_name`s must be unique (they are the persistence
//! key — a duplicate silently makes one control's saved bindings
//! unrecoverable). The DIP side already has the equivalent guard in
//! `assert_dip_banks_valid`; this is its input counterpart.
//!
//! These run without ROMs because `MachineEntry` carries the control table
//! directly — `MachineEntry::create` needs a `RomSet`, which CI does not have.

use phosphor_core::core::machine::{DefaultBinding, InputKind, MouseControl, PadControl};
use phosphor_machines::registry;

#[test]
fn every_machine_declares_input_controls() {
    for entry in registry::all() {
        assert!(
            !entry.controls.is_empty(),
            "{}: no input controls declared",
            entry.name
        );
    }
}

#[test]
fn input_ids_are_unique_within_each_machine() {
    for entry in registry::all() {
        let mut ids: Vec<u16> = entry.controls.iter().map(|c| c.id.0).collect();
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            before,
            ids.len(),
            "{}: duplicate InputId — handle_input dispatches on it, so one \
             control would shadow the other",
            entry.name
        );
    }
}

#[test]
fn stable_names_are_unique_within_each_machine() {
    for entry in registry::all() {
        let mut names: Vec<&str> = entry.controls.iter().map(|c| c.stable_name).collect();
        let before = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            before,
            names.len(),
            "{}: duplicate stable_name — bindings persist by name, so one \
             control's saved binding would be unrecoverable",
            entry.name
        );
    }
}

#[test]
fn control_names_and_labels_are_non_empty() {
    for entry in registry::all() {
        for control in entry.controls {
            assert!(
                !control.stable_name.is_empty(),
                "{}: control with empty stable_name",
                entry.name
            );
            assert!(
                !control.label.is_empty(),
                "{}: control '{}' has an empty label",
                entry.name,
                control.stable_name
            );
        }
    }
}

/// 90-odd `handle_input` ladders across the crate match on `id.0 as u8`, which
/// silently truncates the u16. Nothing exceeds 255 today; this pins the footgun
/// so a machine that grows past it fails here rather than aliasing two controls
/// onto the same dispatch arm.
#[test]
fn input_ids_survive_the_as_u8_dispatch_ladders() {
    for entry in registry::all() {
        for control in entry.controls {
            assert!(
                control.id.0 < 256,
                "{}: control '{}' has InputId {}, which truncates under \
                 `match id.0 as u8`",
                entry.name,
                control.stable_name,
                control.id.0
            );
        }
    }
}

/// Every machine must be coinable out of the box.
///
/// Deliberately per-machine, not per-control. Leaving `coin2`/`coin3` unbound
/// is the established convention here — ~11 machines do it, because a second
/// cabinet coin slot has no natural key and is rebindable when wanted. What
/// would be a real defect is a machine with no reachable coin input at all.
#[test]
fn every_machine_has_a_bound_coin_input() {
    for entry in registry::all() {
        let coins: Vec<_> = entry
            .controls
            .iter()
            .filter(|c| matches!(c.kind, InputKind::Coin))
            .collect();
        if coins.is_empty() {
            continue; // a machine may legitimately have no coin slot modeled
        }
        assert!(
            coins.iter().any(|c| !c.default_bindings.is_empty()),
            "{}: has {} coin control(s) but none is bound by default — the \
             machine cannot be coined without rebinding first",
            entry.name,
            coins.len()
        );
    }
}

/// Player numbering is 1-based where declared; a `Some(0)` would silently sort
/// or group wrong in the settings UI.
#[test]
fn declared_players_are_one_based() {
    for entry in registry::all() {
        for control in entry.controls {
            if let Some(player) = control.player {
                assert!(
                    player >= 1,
                    "{}: control '{}' declares player {}, but numbering is \
                     1-based",
                    entry.name,
                    control.stable_name,
                    player
                );
            }
        }
    }
}

/// A machine with analog controls must have at least one of them reachable.
///
/// Per-machine rather than per-control, for the same reason as the coin test:
/// there is one mouse, so two-player analog cabinets (foodf's P2 stick,
/// marble's P2 trackball) deliberately leave the second player's axes unbound.
/// A machine where *no* analog axis is bound would be genuinely unplayable.
#[test]
fn machines_with_analog_controls_can_drive_at_least_one() {
    for entry in registry::all() {
        let analog: Vec<_> = entry
            .controls
            .iter()
            .filter(|c| matches!(c.kind, InputKind::AnalogAxis { .. }))
            .collect();
        if analog.is_empty() {
            continue;
        }
        // Only a mouse axis or a whole pad axis carries a continuous value;
        // keys and pad buttons can only ever produce a Button.
        let drivable = analog.iter().any(|c| {
            c.default_bindings.iter().any(|b| {
                matches!(
                    b,
                    DefaultBinding::Mouse(MouseControl::AxisX | MouseControl::AxisY)
                        | DefaultBinding::Pad(PadControl::FullAxis(_))
                )
            })
        });
        assert!(
            drivable,
            "{}: declares {} analog control(s), none with an analog default \
             binding — nothing can drive them continuously",
            entry.name,
            analog.len()
        );
    }
}
