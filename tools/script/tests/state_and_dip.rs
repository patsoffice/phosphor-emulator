//! End-to-end: save/load state round-trips exactly, and a DIP edit changes the
//! live bank byte, on a real machine.
//!
//! ROM-gated: skips cleanly when no ROM dir is present.

use phosphor_harness::roms_dir;
use phosphor_script::DebugSession;

const MACHINE: &str = "galaga";

/// A signature of galaga's readable RAM (backing regions only; unmapped gaps
/// read `None` and are skipped consistently).
fn ram_signature(session: &mut DebugSession) -> Vec<u8> {
    (0x8000u32..0x9c00)
        .filter_map(|a| session.read(0, a))
        .collect()
}

#[test]
fn save_state_round_trips_and_dip_edits_apply() {
    let Some(roms) = roms_dir() else {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
        return;
    };
    let path = roms.to_str().unwrap();

    let mut session = DebugSession::open(MACHINE, path).expect("open galaga");
    session.run_frames(3100); // into attract

    // --- Save / load state ---
    let snapshot = session.save_state().expect("galaga supports save states");
    let at_snapshot = ram_signature(&mut session);

    session.run_frames(90);
    let advanced = ram_signature(&mut session);
    // Guard: the attract RAM must actually change, or the round-trip below
    // would pass trivially.
    assert_ne!(
        at_snapshot, advanced,
        "attract RAM should animate over 90 frames"
    );

    session.load_state(&snapshot).expect("load_state");
    let restored = ram_signature(&mut session);
    assert_eq!(
        restored, at_snapshot,
        "load_state must restore the exact snapshot"
    );

    // --- DIP editing ---
    let banks = session.dip_banks();
    assert_eq!(banks[0].name, "DSWA");
    assert!(
        banks[0].options.iter().any(|o| o.name == "Difficulty"),
        "DSWA exposes a Difficulty option"
    );
    assert!(session.set_dip_by_name("Difficulty", "Hard"));
    assert_eq!(
        session.dip_bank_value(0) & 0x03,
        0x01,
        "Difficulty=Hard sets the low DSWA bits"
    );
}
