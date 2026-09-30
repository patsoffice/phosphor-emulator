//! The audio half of Atari's Regulator/Audio II PCB, 035435-02, which Missile
//! Command (rev B) and Tempest (rev E) both drive. Transcribed in
//! [`docs/schematics/atari-pokey-audio-output.md`](../../docs/schematics/atari-pokey-audio-output.md).
//!
//! Per channel: R14 10k and R27 1k divide the game board's output by 11, C6
//! 0.22 uF couples it into pin 1 of a TDA2002A, R9 220 ohm and R11 10 ohm with
//! C4 470 uF set its gain, and C9 couples the output into the speaker. The two
//! revisions differ in C9 alone: 1000 uF on rev B, 3300 uF on rev E.
//!
//! Crystal Castles drives 035435-01 rev F (SP-241 sheet 2A), transcribed in
//! [`docs/schematics/ccastles-audio-output.md`](../../docs/schematics/ccastles-audio-output.md):
//! the same parts in the same places, read against the table above, with C9 at
//! 3300 uF.
//!
//! What is modeled is the three frequency-shaping stages, each first order:
//!
//! - C6 against the divider's 909 ohm and the TDA2002A's input resistance, a
//!   high-pass near 4.8 Hz;
//! - C4 in the gain leg, which makes the gain 23 in the audio band and 1 at DC,
//!   a low shelf with its corner near 34 Hz;
//! - C9 against the speaker, a high-pass at 19.9 Hz on rev B and 6.0 Hz on
//!   rev E, taking the speaker as a nominal 8 ohm resistance.
//!
//! What is not: the divider and the gain of 23 are a scale, which the game
//! boards' normalization absorbs. C7 at pin 1, R12 with C5 in the feedback and
//! the R10/C3 Boucherot cell all act above 150 kHz. Clipping is not modeled:
//! read off the Fairchild data sheet's curves, the output clips near 4.4 V peak
//! at light load and nearer 3 V at the bridge's heaviest, and the transcription
//! doc shows the recorded movies exceed that on their loud passages.
//!
//! The board has two channels, fed an antiphase pair. Every cabinet wiring
//! diagram read (Missile Command's cabaret and sit-down, Tempest's upright)
//! bridges one speaker across SPKR 1 and SPKR 2 and leaves both returns
//! unconnected, so the speaker hears the difference of the two channels: the
//! pair in phase, and one channel of this model up to a scale. In that bridge
//! the speaker current flows through both output capacitors in series, into the
//! speaker in parallel with the volume rheostat, which the C9 stage above (one
//! capacitor into 8 ohm) does not yet describe.

use phosphor_core::audio::DcBlocker;
use phosphor_macros::Saveable;

/// R14 in series and R27 to ground: the input divider.
const R14: f64 = 10_000.0;
const R27: f64 = 1_000.0;
/// C6, the input coupling capacitor into pin 1.
const C6: f64 = 0.22e-6;
/// The TDA2002A's input resistance at pin 1: 150 kOhm typical, 70 kOhm minimum,
/// at 1 kHz (SGS-Thomson TDA2002 data sheet, electrical characteristics).
const TDA2002_RI: f64 = 150_000.0;
/// R9 from the output to the gain node, R11 from the gain node to C4, and C4
/// on to ground: gain `1 + R9 / (R11 + 1/(s C4))`.
const R9: f64 = 220.0;
const R11: f64 = 10.0;
const C4: f64 = 470e-6;
/// The speaker, taken as a nominal 8 ohm resistance.
const SPEAKER_OHMS: f64 = 8.0;

/// C9 on revision B of the board, Missile Command's.
pub const C9_REV_B: f64 = 1000e-6;
/// C9 on revision E of the board, Tempest's.
pub const C9_REV_E: f64 = 3300e-6;
/// C9 on revision F of the board, Quantum's (SP-221 sheet 2A).
pub const C9_REV_F: f64 = 3300e-6;
/// C9 on 035435-01 revision F, Crystal Castles'.
pub const C9_01_REV_F: f64 = 3300e-6;

fn corner_hz(ohms: f64, farads: f64) -> f32 {
    (1.0 / (std::f64::consts::TAU * ohms * farads)) as f32
}

/// One channel of the Regulator/Audio II board's amplifier, normalized to unity
/// gain in the audio band.
#[derive(Clone, Debug, Saveable)]
#[save_version(1)]
pub struct RegulatorAudioII {
    input_coupling: DcBlocker,
    gain_shelf: DcBlocker,
    output_coupling: DcBlocker,
}

impl RegulatorAudioII {
    /// A channel with output coupling capacitor `c9` (see [`C9_REV_B`] and
    /// [`C9_REV_E`]), running at `sample_rate`.
    pub fn new(c9: f64, sample_rate: u32) -> Self {
        let divider = R14 * R27 / (R14 + R27);
        Self {
            input_coupling: DcBlocker::with_cutoff(
                corner_hz(divider + TDA2002_RI, C6),
                sample_rate,
            ),
            gain_shelf: DcBlocker::with_cutoff(corner_hz(R11, C4), sample_rate),
            output_coupling: DcBlocker::with_cutoff(corner_hz(SPEAKER_OHMS, c9), sample_rate),
        }
    }

    /// One sample from the game board's output to the speaker, scaled so the
    /// audio band passes at unity.
    pub fn process(&mut self, x: f32) -> f32 {
        let coupled = self.input_coupling.process(x);
        // The gain is 1 + (R9/R11) times a high-pass at C4's corner: 1 at DC
        // and 1 + R9/R11 = 23 above it. Divided by 23, so the band is unity.
        let ratio = (R9 / R11) as f32;
        let shaped = (coupled + ratio * self.gain_shelf.process(coupled)) / (1.0 + ratio);
        self.output_coupling.process(shaped)
    }

    pub fn reset(&mut self) {
        self.input_coupling.reset();
        self.gain_shelf.reset();
        self.output_coupling.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steady_gain(amp: &mut RegulatorAudioII, hz: f64, rate: u32) -> f64 {
        let n = rate as usize * 4;
        let mut peak = 0.0f64;
        for i in 0..n {
            let t = i as f64 / rate as f64;
            let y = amp.process((std::f64::consts::TAU * hz * t).sin() as f32) as f64;
            if i > n / 2 {
                peak = peak.max(y.abs());
            }
        }
        peak
    }

    /// The band passes at unity, and the low end falls through the shelf and
    /// the two couplings: at 10 Hz on rev B the shelf is most of the way down
    /// and C9's 19.9 Hz corner takes more.
    #[test]
    fn unity_in_the_band_and_falling_below_it() {
        let rate = 44_100;
        let mut amp = RegulatorAudioII::new(C9_REV_B, rate);
        let band = steady_gain(&mut amp, 1_000.0, rate);
        assert!((band - 1.0).abs() < 0.01, "1 kHz: {band}");
        let mut amp = RegulatorAudioII::new(C9_REV_B, rate);
        let low = steady_gain(&mut amp, 10.0, rate);
        assert!(low < 0.25, "10 Hz: {low}");
    }

    /// Rev E's larger output capacitor passes more of the low end than rev B's.
    #[test]
    fn rev_e_keeps_more_low_end_than_rev_b() {
        let rate = 44_100;
        let b = steady_gain(&mut RegulatorAudioII::new(C9_REV_B, rate), 20.0, rate);
        let e = steady_gain(&mut RegulatorAudioII::new(C9_REV_E, rate), 20.0, rate);
        assert!(e > b * 1.2, "20 Hz: rev E {e} against rev B {b}");
    }

    /// A constant input settles to nothing: every stage is AC-coupled.
    #[test]
    fn dc_does_not_reach_the_speaker() {
        let mut amp = RegulatorAudioII::new(C9_REV_E, 44_100);
        let mut y = 1.0;
        for _ in 0..44_100 * 3 {
            y = amp.process(1.0);
        }
        assert!(y.abs() < 1e-3, "{y}");
    }
}
