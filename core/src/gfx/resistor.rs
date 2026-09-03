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

// ---------------------------------------------------------------------------
// TTL DAC into an amplifier and an inverting monitor
// ---------------------------------------------------------------------------

/// Ladder and bias for the two 3-bit channels driven through a Darlington pair.
pub const DARLINGTON_RESISTORS: [f64; 3] = [1000.0, 470.0, 220.0];
/// Pullup bias to VCC for [`DARLINGTON_RESISTORS`].
pub const DARLINGTON_BIAS_R: f64 = 470.0;
/// Ladder for the 2-bit channel driven through an emitter follower alone.
pub const EMITTER_RESISTORS: [f64; 2] = [470.0, 220.0];
/// Pullup bias to VCC for [`EMITTER_RESISTORS`].
pub const EMITTER_BIAS_R: f64 = 680.0;

/// Compute one color channel through a TTL-driven DAC, an amplifier stage and
/// an inverting monitor input.
///
/// This is a *different physical model* from the three above, and the reason it
/// exists is that they do not fit. The others treat a PROM output as an ideal
/// voltage source into a resistor network. Here the PROM's own output stage is
/// part of the circuit: an MB7052 drives its ladder to real TTL levels through a
/// real source impedance, the ladder sits on a VCC pullup bias rather than a
/// pulldown, and what the ladder feeds is a transistor amplifier and then a
/// monitor that inverts and clips. Modelling that as a divider gets the black
/// level wrong, which is visible rather than merely inaccurate — see
/// [`normalize_palette_per_channel`].
///
/// `raw_bits` are non-inverted PROM bit values: `0.0` is TTL low (the active
/// state, since the chain inverts) and `1.0` is TTL high. Returns a raw
/// floating-point intensity, *not* clamped to 0-255, for
/// [`normalize_palette_per_channel`] to scale.
///
/// # Which boards
///
/// Nintendo's, through a Sanyo EZV20 monitor: the TKG-04 board that Donkey Kong
/// and Donkey Kong Jr. run on, and Mario Bros., which has its own board and
/// shares only this. That distinction is why this lives here rather than in a
/// machine file — Mario Bros. used to reach across into `tkg04.rs` for it, which
/// read as though it ran on that board.
pub fn compute_ttl_dac_channel(
    raw_bits: &[f64],
    resistors: &[f64],
    bias_r: f64,
    is_darlington: bool,
) -> f64 {
    const VCC: f64 = 5.0;
    const V_BIAS: f64 = 5.0;
    const V_OL: f64 = 0.05; // TTL low output voltage
    const V_OH: f64 = 4.0; // TTL high output voltage
    const TTL_H_RES: f64 = 50.0; // TTL high-state output impedance (Ω)

    let mut r_total: f64 = 0.0;
    let mut v: f64 = 0.0;

    // First pass: low inputs (raw bit = 0, PROM output driving to vOL)
    for (&bit, &r) in raw_bits.iter().zip(resistors) {
        if r != 0.0 && bit == 0.0 {
            r_total += 1.0 / r;
            v += V_OL / r;
        }
    }

    // Bias pullup to VCC
    r_total += 1.0 / bias_r;
    v += V_BIAS / bias_r;

    // Second pass: high inputs (raw bit = 1, TTL high through R + output impedance)
    for (&bit, &r) in raw_bits.iter().zip(resistors) {
        if r != 0.0 && bit != 0.0 {
            let r_eff = r + TTL_H_RES;
            r_total += 1.0 / r_eff;
            v += V_OH / r_eff;
        }
    }

    // Node voltage (Thévenin equivalent)
    let v_node = v / r_total;

    // Amplifier stage
    let v_amp = if is_darlington {
        v_node.max(0.7) // Darlington: minimum output ≈ 0.7 V
    } else {
        (v_node - 0.7).max(0.0) // Emitter follower: base-emitter drop ≈ 0.7 V
    };

    // SANYO EZV20 monitor: inverting circuit with diode clipping
    let v_inv = VCC - v_amp;
    let v_clip = (v_inv - 0.7).clamp(0.0, VCC - 1.4);
    v_clip / (VCC - 1.4) * 255.0
}

/// Scale a raw palette to 0-255 **per channel**, from the range the palette
/// itself spans.
///
/// `forced_black` marks pens a board writes as black by some rule outside the
/// DAC. Those are excluded from the range as well as from the output, because
/// they are not a channel output and their zeros would otherwise drag every
/// channel's minimum to 0 and defeat the black-level adjustment. A board with no
/// such rule passes `|_| false`.
///
/// # Why per channel and not one global scale
///
/// The three channels do not reach the monitor on a common baseline. Red and
/// green leave an NPN into an A564 PNP whose base-emitter drops cancel (Donkey
/// Kong and Donkey Kong Jr.: Q9 into Q13, Q10 into Q14; Mario Bros.: Q5 into Q7,
/// Q8 into Q9), while blue has only the NPN (Q11, and Mario Bros.' Q6), so blue
/// sits a whole 0.7 V follower drop above the other two along its entire range.
/// That pedestal is in the hardware and [`compute_ttl_dac_channel`] reproduces
/// it correctly; what removes it is the monitor, which DC-restores each channel
/// to black during the back porch, which is inherently per channel. A single
/// global gain cannot subtract a per-channel offset.
///
/// Donkey Kong is where the difference is visible rather than merely wrong. A
/// pen with every PROM bit inactive came out (4, 4, 56) instead of black, which
/// put a dark blue rectangle over the ladders in attract mode: the board's solid
/// mask sprites, whose whole job is to hide Kong behind the girder as he climbs.
pub fn normalize_palette_per_channel(
    raw: &[(f64, f64, f64); 256],
    forced_black: impl Fn(usize) -> bool,
) -> [(u8, u8, u8); 256] {
    let mut lo = [f64::MAX; 3];
    let mut hi = [f64::MIN; 3];
    for (i, &(r, g, b)) in raw.iter().enumerate() {
        if forced_black(i) {
            continue;
        }
        for (channel, v) in [r, g, b].into_iter().enumerate() {
            lo[channel] = lo[channel].min(v);
            hi[channel] = hi[channel].max(v);
        }
    }

    let normalize = |v: f64, channel: usize| -> u8 {
        let span = hi[channel] - lo[channel];
        if span <= 0.0 {
            return 0;
        }
        (((v - lo[channel]) / span) * 255.0)
            .round()
            .clamp(0.0, 255.0) as u8
    };

    let mut out = [(0u8, 0u8, 0u8); 256];
    for (i, (o, &(r, g, b))) in out.iter_mut().zip(raw.iter()).enumerate() {
        if forced_black(i) {
            continue;
        }
        *o = (normalize(r, 0), normalize(g, 1), normalize(b, 2));
    }
    out
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
