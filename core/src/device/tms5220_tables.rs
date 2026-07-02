//! TMS5220 LPC coefficient ROM tables.
//!
//! These are the fixed on-die tables the TMS5220 (and TMS5220C, which shares
//! them) uses to turn the packed LPC frame indices in a speech stream into the
//! reflection coefficients, pitch period, energy, and glottal excitation the
//! 10-pole lattice synthesizer runs on:
//!
//! * [`ENERGY`] / [`PITCH`] — decode the 4-bit energy and 6-bit pitch indices.
//! * [`K`] — the ten reflection-coefficient tables `K1..K10`; table `n` is
//!   indexed by a [`K_BITS`]`[n]`-wide field, so its length is `1 << K_BITS[n]`
//!   (K1/K2 = 32, K3..K7 = 16, K8..K10 = 8).
//! * [`CHIRP`] — the 8-bit signed glottal-pulse excitation waveform for voiced
//!   frames, indexed by the pitch counter.
//! * [`INTERP_SHIFT`] — the per-interpolation-period right-shift amounts that
//!   step the current coefficients toward the next frame's targets.
//!
//! The device core (the LPC synthesizer) consumes these; nothing references them
//! until that lands, hence the temporary allow below.
#![allow(dead_code)]

/// Number of reflection coefficients (lattice poles).
pub const NUM_K: usize = 10;

/// Bit width of the packed energy index (table has `1 << ENERGY_BITS` entries).
pub const ENERGY_BITS: u8 = 4;

/// Bit width of the packed pitch index (table has `1 << PITCH_BITS` entries).
pub const PITCH_BITS: u8 = 6;

/// Bit width of each packed reflection-coefficient index `K1..K10`.
pub const K_BITS: [u8; NUM_K] = [5, 5, 4, 4, 4, 4, 4, 3, 3, 3];

/// Energy table, indexed by the 4-bit energy field. Index 0 = silence and
/// index 15 = the stop code (both handled by the frame parser).
pub const ENERGY: [u16; 16] = [0, 1, 2, 3, 4, 6, 8, 11, 16, 23, 33, 47, 63, 85, 114, 0];

/// Pitch-period table, indexed by the 6-bit pitch field. Index 0 = unvoiced.
pub const PITCH: [u16; 64] = [
    0, 15, 16, 17, 18, 19, 20, 21, //
    22, 23, 24, 25, 26, 27, 28, 29, //
    30, 31, 32, 33, 34, 35, 36, 37, //
    38, 39, 40, 41, 42, 44, 46, 48, //
    50, 52, 53, 56, 58, 60, 62, 65, //
    68, 70, 72, 76, 78, 80, 84, 86, //
    91, 94, 98, 101, 105, 109, 114, 118, //
    122, 127, 132, 137, 142, 148, 153, 159,
];

const K1: [i16; 32] = [
    -501, -498, -497, -495, -493, -491, -488, -482, //
    -478, -474, -469, -464, -459, -452, -445, -437, //
    -412, -380, -339, -288, -227, -158, -81, -1, //
    80, 157, 226, 287, 337, 379, 411, 436,
];

const K2: [i16; 32] = [
    -328, -303, -274, -244, -211, -175, -138, -99, //
    -59, -18, 24, 64, 105, 143, 180, 215, //
    248, 278, 306, 331, 354, 374, 392, 408, //
    422, 435, 445, 455, 463, 470, 476, 506,
];

const K3: [i16; 16] = [
    -441, -387, -333, -279, -225, -171, -117, -63, //
    -9, 45, 98, 152, 206, 260, 314, 368,
];

const K4: [i16; 16] = [
    -328, -273, -217, -161, -106, -50, 5, 61, //
    116, 172, 228, 283, 339, 394, 450, 506,
];

const K5: [i16; 16] = [
    -328, -282, -235, -189, -142, -96, -50, -3, //
    43, 90, 136, 182, 229, 275, 322, 368,
];

const K6: [i16; 16] = [
    -256, -212, -168, -123, -79, -35, 10, 54, //
    98, 143, 187, 232, 276, 320, 365, 409,
];

const K7: [i16; 16] = [
    -308, -260, -212, -164, -117, -69, -21, 27, //
    75, 122, 170, 218, 266, 314, 361, 409,
];

const K8: [i16; 8] = [-256, -161, -66, 29, 124, 219, 314, 409];

const K9: [i16; 8] = [-256, -176, -96, -15, 65, 146, 226, 307];

const K10: [i16; 8] = [-205, -132, -59, 14, 87, 160, 234, 307];

/// The ten reflection-coefficient tables `K1..K10`. Each is indexed by a
/// [`K_BITS`]-wide field, so `K[n].len() == 1 << K_BITS[n]`.
pub const K: [&[i16]; NUM_K] = [&K1, &K2, &K3, &K4, &K5, &K6, &K7, &K8, &K9, &K10];

/// Glottal-pulse excitation ("chirp") waveform, 8-bit signed, indexed by the
/// pitch counter during voiced frames. Only the first 21 entries are non-zero;
/// the pitch counter saturates within the 52-entry span.
pub const CHIRP: [i8; 52] = [
    0, 3, 15, 40, 76, 108, 113, 80, //
    37, 38, 76, 68, 26, 50, 59, 19, //
    55, 26, 37, 31, 29, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0, 0, 0, 0, 0, //
    0, 0, 0, 0,
];

/// Per-interpolation-period right-shift amounts (interpolation periods 0..7).
/// The delta toward the next frame's target is shifted right by this much each
/// period; 0 means "jump fully to the target".
pub const INTERP_SHIFT: [u8; 8] = [0, 3, 3, 3, 2, 2, 1, 1];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_lengths_match_index_widths() {
        assert_eq!(ENERGY.len(), 1 << ENERGY_BITS);
        assert_eq!(PITCH.len(), 1 << PITCH_BITS);
        assert_eq!(K.len(), NUM_K);
        for (n, table) in K.iter().enumerate() {
            assert_eq!(
                table.len(),
                1 << K_BITS[n],
                "K{} length must be 2^{} entries",
                n + 1,
                K_BITS[n]
            );
        }
        assert_eq!(CHIRP.len(), 52);
        assert_eq!(INTERP_SHIFT.len(), 8);
    }

    #[test]
    fn spot_check_known_values() {
        // Energy: silence at 0, stop-code slot at 15, and a midpoint.
        assert_eq!(ENERGY[0], 0);
        assert_eq!(ENERGY[8], 16);
        assert_eq!(ENERGY[15], 0);

        // Pitch: unvoiced at 0, endpoints of the ramp.
        assert_eq!(PITCH[0], 0);
        assert_eq!(PITCH[1], 15);
        assert_eq!(PITCH[63], 159);

        // Reflection coefficients: first/last of the widest and narrowest tables.
        assert_eq!(K[0][0], -501);
        assert_eq!(K[0][31], 436);
        assert_eq!(K[9][0], -205);
        assert_eq!(K[9][7], 307);

        // Chirp: leading edge and its saturated tail.
        assert_eq!(CHIRP[0], 0);
        assert_eq!(CHIRP[6], 113);
        assert_eq!(CHIRP[20], 29);
        assert_eq!(CHIRP[21], 0);
        assert_eq!(CHIRP[51], 0);

        // Interpolation: full jump on period 0, then decreasing shifts.
        assert_eq!(INTERP_SHIFT[0], 0);
        assert_eq!(INTERP_SHIFT[7], 1);
    }
}
