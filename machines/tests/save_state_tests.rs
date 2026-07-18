//! Save-state round-trip tests for all machines.
//!
//! Verifies that every machine can save and load state consistently:
//! - save → load → save produces identical bytes (round-trip)
//! - corrupted machine IDs are rejected

use phosphor_core::core::machine::SaveState;

/// Generate standard save-state round-trip tests for a machine.
///
/// `$create` must be an expression producing a machine with
/// `save_state()` / `load_state()` support.
macro_rules! save_state_tests {
    ($mod_name:ident, $create:expr) => {
        mod $mod_name {
            use super::*;

            #[test]
            fn save_load_round_trip() {
                let sys = $create;
                let saved = sys.save_state().expect("save_state returned None");
                assert!(!saved.is_empty(), "save data should not be empty");

                // Load into a fresh instance and re-save
                let mut sys2 = $create;
                sys2.load_state(&saved).expect("load_state failed");
                let saved2 = sys2.save_state().expect("second save returned None");
                assert_eq!(saved, saved2, "round-trip should produce identical bytes");
            }

            #[test]
            fn rejects_corrupted_machine_id() {
                let mut sys = $create;
                let saved = sys.save_state().expect("save_state returned None");

                // Corrupt a byte in the machine_id string (offset 12 is within the id)
                let mut bad = saved.clone();
                if bad.len() > 12 {
                    bad[12] ^= 0xFF;
                }
                assert!(
                    sys.load_state(&bad).is_err(),
                    "should reject corrupted machine ID"
                );
            }

            #[test]
            fn rejects_truncated_data() {
                let mut sys = $create;
                let saved = sys.save_state().expect("save_state returned None");

                // Truncate to just the header
                let truncated = &saved[..8.min(saved.len())];
                assert!(
                    sys.load_state(truncated).is_err(),
                    "should reject truncated save data"
                );
            }
        }
    };
}

// Williams board machines
save_state_tests!(joust, phosphor_machines::JoustSystem::new());
save_state_tests!(robotron, phosphor_machines::RobotronSystem::new());
save_state_tests!(sinistar, phosphor_machines::SinistarSystem::new());

// Namco Pac board machines
save_state_tests!(pacman, phosphor_machines::PacmanSystem::new());
save_state_tests!(mspacman, phosphor_machines::MsPacmanSystem::new());

// Nintendo TKG-04 board machines
save_state_tests!(donkey_kong, phosphor_machines::DkongSystem::new());
save_state_tests!(donkey_kong_jr, phosphor_machines::DkongJrSystem::new());

// MCR-II board machines
save_state_tests!(satans_hollow, phosphor_machines::SatansHollowSystem::new());

// Gottlieb System 80 machines
save_state_tests!(qbert, phosphor_machines::QbertSystem::new());

// Atari DVG vector machines
save_state_tests!(asteroids, phosphor_machines::AsteroidsSystem::new());
save_state_tests!(astdelux, phosphor_machines::AsteroidsDeluxeSystem::new());
save_state_tests!(llander, phosphor_machines::LunarLanderSystem::new());

// Namco Galaga board machines. Their RAM lives in the shared board map's
// regions, which the board serializes generically, so a round trip here is
// what catches a save/load region-order mismatch.
save_state_tests!(digdug, phosphor_machines::DigDugSystem::new());
save_state_tests!(galaga, phosphor_machines::galaga::GalagaSystem::new());

// Data East btime board machines
save_state_tests!(burgertime, phosphor_machines::BurgertimeSystem::new());

// Standalone machines
save_state_tests!(
    missile_command,
    phosphor_machines::MissileCommandSystem::new()
);
save_state_tests!(gridlee, phosphor_machines::GridleeSystem::new());
save_state_tests!(ccastles, phosphor_machines::CrystalCastlesSystem::new());
