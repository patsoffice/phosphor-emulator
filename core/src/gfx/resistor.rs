//! Resistor-weighted DAC palette computation.
//!
//! Many arcade machines generate RGB palette values by driving resistor networks
//! from PROM data. This module provides two computation models:
//!
//! - **Linear weights** ([`compute_resistor_weights`] + [`combine_weights`]):
//!   Pre-compute per-bit weights auto-scaled so all-bits-on = 255, then combine
//!   via linear sum. Works well when the pulldown resistance is large relative to
//!   the DAC resistors (e.g., Namco Pac-Man, Crystal Castles).
//!
//! - **Exact voltage divider** ([`compute_resistor_net`]): Computes the actual
//!   conductance ratio for each combination of bits, including the pulldown.
//!   Physically accurate but NOT auto-scaled to 255. Used when the pulldown is
//!   strong enough that the linear approximation breaks down (e.g., DK/TKG-04).

/// Compute per-bit weights for a resistor-weighted DAC, auto-scaled so that
/// all-bits-on produces 255.
///
/// Each element in `resistors` is a resistance value in ohms (weakest-first by
/// convention, but any order works). The returned `Vec` has the same length as
/// `resistors`, with `weights[i]` corresponding to `resistors[i]`.
///
/// - Without pulldown: `weight_i = (1/R_i) / sum(1/R_j) * 255`
/// - With pulldown: `weight_i = (pd/(R_i+pd)) / sum(pd/(R_j+pd)) * 255`
pub fn compute_resistor_weights(resistors: &[f64], pulldown: Option<f64>) -> Vec<f64> {
    match pulldown {
        None => {
            let total: f64 = resistors.iter().map(|r| 1.0 / r).sum();
            resistors
                .iter()
                .map(|r| (1.0 / r) / total * 255.0)
                .collect()
        }
        Some(pd) => {
            let raw: Vec<f64> = resistors.iter().map(|r| pd / (r + pd)).collect();
            let sum: f64 = raw.iter().sum();
            raw.iter().map(|w| w / sum * 255.0).collect()
        }
    }
}

/// Combine pre-computed weights with individual bit values (0 or 1) into an
/// 8-bit color value.
///
/// `weights` and `bits` must have the same length. Returns
/// `round(sum(weight_i * bit_i))` clamped to 0–255.
pub fn combine_weights(weights: &[f64], bits: &[u8]) -> u8 {
    let val: f64 = weights.iter().zip(bits).map(|(w, &b)| *w * b as f64).sum();
    val.round().min(255.0) as u8
}

/// Compute an 8-bit color value from the exact resistor voltage divider.
///
/// Unlike the linear weight model, this accounts for the nonlinear interaction
/// between the pulldown and active-bit conductances. The result is NOT auto-scaled:
/// all-bits-on gives a value < 255 when a pulldown is present.
///
/// `bits` contains the analog drive level for each resistor (typically 0.0 or 1.0).
/// `resistors` contains the corresponding resistance values in ohms.
/// `pulldown` is the pulldown resistance to ground in ohms.
///
/// Formula: `V = sum(bit_i/R_i) / (sum(bit_i/R_i) + 1/pulldown)`, scaled to 0–255.
pub fn compute_resistor_net(bits: &[f64], resistors: &[f64], pulldown: f64) -> u8 {
    let active: f64 = bits.iter().zip(resistors).map(|(b, r)| b / r).sum();
    let total = active + 1.0 / pulldown;
    let voltage = if total > 0.0 { active / total } else { 0.0 };
    (voltage * 255.0).round().min(255.0) as u8
}

/// Compute resistor-network per-bit weights, treating each bit as a Thevenin
/// voltage divider with every *other* bit grounded.
///
/// When only bit `i` is driven high, its output is `max · R0/(R1+R0)`, where
/// `R1` is bit `i`'s resistor to the supply and `R0` is the parallel combination
/// of the pulldown and every *other* bit's resistor to ground. Returns one weight
/// per resistor; callers linearly combine the weights of the active bits
/// (optionally applying a shared cross-network autoscale). A resistance of `0.0`
/// marks an absent/open connection (treated as a `1e12 Ω` conductance floor).
///
/// This differs from the two simpler models:
/// - [`compute_resistor_weights`] approximates each bit independently against
///   only the pulldown, ignoring the loading of the other bits.
/// - [`compute_resistor_net`] returns a single value for a fixed bit vector,
///   not per-bit weights for linear combination.
pub fn compute_resnet_weights(resistors: &[f64], pulldown: f64, max: f64) -> Vec<f64> {
    // Open connection => 1e12 Ω conductance floor.
    let pd_g = if pulldown == 0.0 {
        1e-12
    } else {
        1.0 / pulldown
    };
    resistors
        .iter()
        .enumerate()
        .map(|(bit, _)| {
            // Conductance to ground: pulldown plus every *other* bit.
            let mut g0 = pd_g;
            let mut g1 = 1e-12; // no pullup
            for (j, &r) in resistors.iter().enumerate() {
                if r != 0.0 {
                    if j == bit {
                        g1 += 1.0 / r;
                    } else {
                        g0 += 1.0 / r;
                    }
                }
            }
            let r0 = 1.0 / g0;
            let r1 = 1.0 / g1;
            max * r0 / (r1 + r0)
        })
        .collect()
}

/// Expand a `bits`-bit color component to a full 8-bit value by replicating the
/// bit pattern into the high bits. For example a 3-bit value `abc` becomes
/// `abcabcab`, and a 2-bit value `ab` becomes `abababab`, so the minimum maps to
/// `0x00` and the maximum to `0xFF`.
///
/// `bits` must be in `1..=8`; upper bits of `value` outside the field are masked.
pub fn pal_nbit(value: u8, bits: u32) -> u8 {
    debug_assert!((1..=8).contains(&bits), "pal_nbit: bits must be 1..=8");
    let mask = ((1u16 << bits) - 1) as u8;
    let value = value & mask;
    let mut result: u8 = 0;
    let mut shift = 8_i32 - bits as i32;
    while shift > -(bits as i32) {
        if shift >= 0 {
            result |= value << shift;
        } else {
            result |= value >> (-shift);
        }
        shift -= bits as i32;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pal_nbit_matches_bit_replication() {
        // 3-bit: 0 -> 0x00, 7 -> 0xFF, and the classic (x<<5)|(x<<2)|(x>>1).
        for x in 0..=7u8 {
            assert_eq!(pal_nbit(x, 3), (x << 5) | (x << 2) | (x >> 1));
        }
        // 2-bit: (x<<6)|(x<<4)|(x<<2)|x.
        for x in 0..=3u8 {
            assert_eq!(pal_nbit(x, 2), (x << 6) | (x << 4) | (x << 2) | x);
        }
        // Endpoints and out-of-field bits masked.
        assert_eq!(pal_nbit(0, 3), 0x00);
        assert_eq!(pal_nbit(7, 3), 0xFF);
        assert_eq!(pal_nbit(0, 2), 0x00);
        assert_eq!(pal_nbit(3, 2), 0xFF);
        assert_eq!(pal_nbit(0xF8 | 0x05, 3), pal_nbit(0x05, 3)); // high bits ignored
        // 1-bit and 4-bit sanity.
        assert_eq!(pal_nbit(1, 1), 0xFF);
        assert_eq!(pal_nbit(0, 1), 0x00);
        assert_eq!(pal_nbit(0xF, 4), 0xFF);
        assert_eq!(pal_nbit(0x9, 4), 0x99);
    }

    #[test]
    fn resnet_weights_single_resistor_divider() {
        // One bit, R=PD=470, max=224: weight = 224·PD/(R+PD) = 224·470/940 = 112.
        let w = compute_resnet_weights(&[470.0], 470.0, 224.0);
        assert_eq!(w.len(), 1);
        assert!((w[0] - 112.0).abs() < 1e-6, "got {}", w[0]);
    }

    #[test]
    fn resnet_weights_account_for_other_bits() {
        // Two bits [470, 220] Ω, 470 Ω pulldown, max 224. Bit 0 sees the 220 Ω
        // bit grounded in parallel with the pulldown: R0 = 1/(1/470+1/220),
        // weight0 = 224·R0/(470+R0).
        let w = compute_resnet_weights(&[470.0, 220.0], 470.0, 224.0);
        let r0 = 1.0 / (1.0 / 470.0 + 1.0 / 220.0);
        let expect0 = 224.0 * r0 / (470.0 + r0);
        assert!((w[0] - expect0).abs() < 1e-6, "got {}", w[0]);
        // Distinct per-bit weights (220 Ω bit is stronger than the 470 Ω bit).
        assert!(w[1] > w[0]);
    }

    #[test]
    fn namco_weights_match_original() {
        // Namco Pac-Man / Galaga: 3-bit R/G (1K/470/220Ω, no pulldown)
        let w = compute_resistor_weights(&[1000.0, 470.0, 220.0], None);
        assert_eq!(w.len(), 3);
        // All bits on should sum to 255
        assert_eq!(combine_weights(&w, &[1, 1, 1]), 255);
        // Individual bits: verify against original compute_resistor_scale output
        assert_eq!(combine_weights(&w, &[1, 0, 0]), 33);
        assert_eq!(combine_weights(&w, &[0, 1, 0]), 71);
        assert_eq!(combine_weights(&w, &[0, 0, 1]), 151);
    }

    #[test]
    fn namco_2bit_blue_weights() {
        // Namco Pac-Man / Galaga: 2-bit B (470/220Ω, no pulldown)
        let w = compute_resistor_weights(&[470.0, 220.0], None);
        assert_eq!(combine_weights(&w, &[1, 1]), 255);
        assert_eq!(combine_weights(&w, &[1, 0]), 81);
        assert_eq!(combine_weights(&w, &[0, 1]), 174);
    }

    #[test]
    fn crystal_castles_weights_match_original() {
        // Crystal Castles: 3-bit (22K/10K/4.7KΩ + 1K pulldown)
        let w = compute_resistor_weights(&[22_000.0, 10_000.0, 4_700.0], Some(1_000.0));
        // Original constants: [36, 75, 144]
        assert_eq!(combine_weights(&w, &[1, 0, 0]), 36);
        assert_eq!(combine_weights(&w, &[0, 1, 0]), 75);
        assert_eq!(combine_weights(&w, &[0, 0, 1]), 144);
        assert_eq!(combine_weights(&w, &[1, 1, 1]), 255);
    }

    #[test]
    fn tkg04_darlington_3bit_match() {
        // TKG-04 Darlington: 1K/470/220Ω + 470Ω pulldown, all bits on → 200
        assert_eq!(
            compute_resistor_net(&[1.0, 1.0, 1.0], &[1000.0, 470.0, 220.0], 470.0),
            200
        );
        // All bits off → 0
        assert_eq!(
            compute_resistor_net(&[0.0, 0.0, 0.0], &[1000.0, 470.0, 220.0], 470.0),
            0
        );
        // Single bit (weakest, 1KΩ)
        assert_eq!(
            compute_resistor_net(&[1.0, 0.0, 0.0], &[1000.0, 470.0, 220.0], 470.0),
            82
        );
    }

    #[test]
    fn tkg04_emitter_2bit_match() {
        // TKG-04 Emitter follower: 470/220Ω + 680Ω pulldown, all bits on → 209
        assert_eq!(
            compute_resistor_net(&[1.0, 1.0], &[470.0, 220.0], 680.0),
            209
        );
        assert_eq!(compute_resistor_net(&[0.0, 0.0], &[470.0, 220.0], 680.0), 0);
    }

    #[test]
    fn all_zeros_is_black() {
        let w = compute_resistor_weights(&[1000.0, 470.0, 220.0], None);
        assert_eq!(combine_weights(&w, &[0, 0, 0]), 0);

        let w2 = compute_resistor_weights(&[22_000.0, 10_000.0, 4_700.0], Some(1_000.0));
        assert_eq!(combine_weights(&w2, &[0, 0, 0]), 0);
    }
}
