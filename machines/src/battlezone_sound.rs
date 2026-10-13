//! Battlezone (1980) discrete sound: engine, shell and explosion voices.
//!
//! The game board holds one POKEY plus a discrete sound section, mixed on the
//! board and driven into the Regulator/Audio II amplifier (see
//! [`crate::atari_regulator_audio`]). The CPU talks to the discrete side
//! through one latch, the LS273 at M2 written at `$1840`:
//!
//! | Bit | MAME's read of the board | What this models |
//! |---|---|---|
//! | D0 | explosion enable | gates the explosion noise |
//! | D1 | explosion loud (1) / soft (0) | ×33 / ×8.7 stage gain |
//! | D2 | shell enable | gates the shell noise |
//! | D3 | shell loud (1) / soft (0) | ×33 / ×8.7 stage gain |
//! | D4 | engine rev enable | VCO control voltage 2.5 V / 4.1 V |
//! | D5 | sound enable | mutes everything including the POKEY |
//! | D6 | start LED | not modeled (no lamp device) |
//! | D7 | motor enable | gates the engine counters |
//!
//! The reference is MAME's `bzone_a.cpp` (courtesy Al Kossow), read against
//! the part values it carries. What is modeled stage for stage:
//!
//! - the noise source and its two divided clocks (12 kHz / 2 / 2 for shell,
//!   the NAND-of-four-taps / 2 for explosion); the LFSR itself is a maximal
//!   16-bit polynomial rather than the board's three chained LS164s, which is
//!   the same white spectrum from a different stretch of bits,
//! - the shell and explosion RC envelopes (23k into 4.7 uF and 10 uF) and
//!   the custom-filter low-pass (330k into 0.0047 uF, about 100 Hz) with its
//!   loud/soft gains (4066-shorted 33k against 10k: ×33 against ×8.7),
//! - the engine 555's two control voltages (R7/R6 divider at 4.1 V, revved to
//!   2.5 V by the 4066 shorting R5 across R6) with the RCDISC3 slew (1.0 s up
//!   through R8, 0.22 s down through R9), the F4/F5 counter taps (÷12 and ÷10
//!   pulse trains), and the 41 Hz engine-mixer low-pass (4×33k into 0.47 uF),
//! - the final mix at the board's conductance weights (100k shell and
//!   explosion, 41.25k engine, 330k POKEY) into the R29 22k feedback load,
//!   with the reference's output gain; the POKEY enters as a 5 V signal.
//!
//! What is not: the R11 engine-frequency pot is fixed at the reference's 40%
//! adjuster position (the pot is a cabinet trim, not a part value); the VCO
//! frequency uses the ideal 555 control-voltage law rather than the loaded
//! divider; the counters' exact tap phases are pulse trains at the right rates
//! and duties rather than the LS393 waveforms. The POKEY's own pin-37 load is
//! unread (it sits on the Auxiliary PCB's sheet 3 side B, which this drawing
//! package did not include), so the POKEY runs at its normalized output.

use phosphor_core::audio::host_sample_rate;
use phosphor_macros::Saveable;

/// 12 kHz board noise clock (MAME `BZ_NOISE_CLOCK`).
const NOISE_CLOCK_HZ: f64 = 12_000.0;
/// TTL logic swing into the engine mixer, in volts.
const TTL_V: f64 = 3.4;
/// Custom-filter input divider: R15/R16 1k under R14/R17 22k. The noise
/// envelope enters the filter through it, so the voices peak near 1.8 V
/// soft instead of at the TTL rail.
const VOICE_DIV: f64 = 1_000.0 / 23_000.0;
/// Custom-filter stage gains: R32/R33 330k over R12/R19 33k + R13/R18
/// 10k (soft); the 4066 shorts R12/R19 to 270 ohm when loud.
const VOICE_GAIN_SOFT: f64 = 8.67;
const VOICE_GAIN_LOUD: f64 = 33.1;
/// Envelope RC nodes: the 4066 switch (270 ohm) against (R14+R15 /
/// R17+R16) 23k. The two charge paths differ by half a percent, so both
/// use the switch resistance alone.
const SW_ON_OHM: f64 = 270.0;
const ENV_R_OHM: f64 = 23_000.0;
/// The switch drop the node output is taken through.
const ENV_V_DROP: f64 = (SW_ON_OHM + ENV_R_OHM) / (2.0 * SW_ON_OHM + ENV_R_OHM);
/// Charge target under the noise load: 5 V through the same drop.
const ENV_V_CHG: f64 = 5.0 * ENV_V_DROP;
/// Shell envelope: C9 4.7 uF. Charges in about a millisecond, discharges
/// across 23k over ~110 ms.
const SHELL_C_F: f64 = 4.7e-6;
const SHELL_TAU_CHG_S: f64 = SW_ON_OHM * SHELL_C_F;
const SHELL_TAU_DIS_S: f64 = (SW_ON_OHM + ENV_R_OHM) * SHELL_C_F;
/// Explosion envelope: C14 10 uF, the same shape running slower.
const EXPLO_C_F: f64 = 10e-6;
const EXPLO_TAU_CHG_S: f64 = SW_ON_OHM * EXPLO_C_F;
const EXPLO_TAU_DIS_S: f64 = (SW_ON_OHM + ENV_R_OHM) * EXPLO_C_F;
/// Custom-filter low-pass: R32/R33 330k into C21/C22 0.0047 uF.
const VOICE_LP_HZ: f64 = 1.0 / (std::f64::consts::TAU * 330_000.0 * 0.0047e-6);
/// Engine mixer low-pass: 4×33k into C29 0.47 uF.
const ENGINE_LP_HZ: f64 = 1.0 / (std::f64::consts::TAU * 8_250.0 * 0.47e-6);
/// Engine 555 base frequency at R11 40%: 1.44 / ((R10 + 2·R11) · C11).
const ENGINE_F_BASE_HZ: f64 = 1.44 / ((100_000.0 + 2.0 * 100_000.0) * 0.015e-6);
/// 555 control-voltage law: f = base · (2·Vcc/3) / Vcv.
const CV_NOMINAL_V: f64 = 10.0 / 3.0;
/// Engine CV at rest (5 V across R7 1k + R6 4.7k) and revved (R5 1k + 270 ohm
/// across R6).
const CV_REST_V: f64 = 5.0 * 4_700.0 / 5_700.0;
const CV_REV_V: f64 = 5.0 * 999.0 / 1_999.0;
/// RCDISC3 slew: charge through R8 100k, discharge through R9 22k, C13 10 uF.
const CV_TAU_UP_S: f64 = 100_000.0 * 10e-6;
const CV_TAU_DOWN_S: f64 = 22_000.0 * 10e-6;
/// Final-mix conductances: R25/R28 100k, R26+R20/4 41.25k, R27 330k.
const G_SHELL: f64 = 1.0 / 100_000.0;
const G_EXPLO: f64 = 1.0 / 100_000.0;
const G_ENGINE: f64 = 1.0 / 41_250.0;
const G_POKEY: f64 = 1.0 / 330_000.0;
/// The final mixer's feedback resistor R29 22k loads the summing node, so
/// the node's denominator is the input conductances plus its own.
const R_FEEDBACK: f64 = 22_000.0;
/// The output stage scales the node to full scale (the reference's
/// `DISCRETE_OUTPUT` gain).
const OUT_GAIN: f64 = 48_000.0 / 32_768.0;
/// The POKEY enters the mix as a 5 V signal (the reference scales its
/// full-scale stream by 5.0/32768).
const POKEY_V: f64 = 5.0;

/// Battlezone's discrete voices, driven by the `$1840` latch byte.
#[derive(Saveable)]
#[save_version(1)]
pub struct BattlezoneSound {
    latch: u8,
    /// 16-bit noise LFSR (nonzero seed; the board's is TTL-seeded too).
    lfsr: u32,
    lfsr_acc: f64,
    lfsr_bit: bool,
    shell_div: bool,
    shell_src: bool,
    nand_acc: f64,
    nand_bit: bool,
    shell_cap: f64,
    explo_cap: f64,
    shell_lp: f64,
    explo_lp: f64,
    /// Engine control voltage, slewing toward the rev-selected target.
    cv: f64,
    phase_12: f64,
    phase_10: f64,
    engine_lp: f64,
    sample_rate: u32,
}

impl BattlezoneSound {
    pub fn new() -> Self {
        Self {
            latch: 0,
            lfsr: 0xACE1,
            lfsr_acc: 0.0,
            lfsr_bit: false,
            shell_div: false,
            shell_src: false,
            nand_acc: 0.0,
            nand_bit: false,
            shell_cap: 0.0,
            explo_cap: 0.0,
            shell_lp: 0.0,
            explo_lp: 0.0,
            cv: CV_REST_V,
            phase_12: 0.0,
            phase_10: 0.0,
            engine_lp: 0.0,
            sample_rate: host_sample_rate(),
        }
    }

    /// The `$1840` sounds latch (M2 LS273).
    pub fn write_latch(&mut self, data: u8) {
        self.latch = data;
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    fn explo_on(&self) -> bool {
        self.latch & 0x01 != 0
    }
    fn explo_loud(&self) -> bool {
        self.latch & 0x02 != 0
    }
    fn shell_on(&self) -> bool {
        self.latch & 0x04 != 0
    }
    fn shell_loud(&self) -> bool {
        self.latch & 0x08 != 0
    }
    fn rev_on(&self) -> bool {
        self.latch & 0x10 != 0
    }
    fn sound_on(&self) -> bool {
        self.latch & 0x20 != 0
    }
    fn motor_on(&self) -> bool {
        self.latch & 0x80 != 0
    }

    /// One-pole low-pass step toward `x` with corner `hz`.
    fn lp(state: &mut f64, x: f64, hz: f64, dt: f64) -> f64 {
        let a = 1.0 - (-2.0 * std::f64::consts::PI * hz * dt).exp();
        *state += a * (x - *state);
        *state
    }

    /// Slew toward `target` with separate up/down time constants.
    fn slew(state: &mut f64, target: f64, tau_up: f64, tau_down: f64, dt: f64) {
        let tau = if target > *state { tau_up } else { tau_down };
        let a = 1.0 - (-dt / tau).exp();
        *state += a * (target - *state);
    }

    /// One step of the envelope RC node: enabled, the cap charges toward
    /// 5 V (4.94 V under the noise load) in about a millisecond; released,
    /// it discharges across 23k. The noise bit gates the output, never the
    /// cap, so a release decays instead of chopping.
    fn envelope(
        cap: &mut f64,
        enabled: bool,
        bit: bool,
        tau_chg: f64,
        tau_dis: f64,
        dt: f64,
    ) -> f64 {
        if enabled {
            if bit {
                Self::slew(cap, ENV_V_CHG, tau_chg, tau_chg, dt);
                *cap * ENV_V_DROP
            } else {
                Self::slew(cap, 5.0, tau_chg, tau_chg, dt);
                0.0
            }
        } else if bit {
            Self::slew(cap, 0.0, tau_dis, tau_dis, dt);
            *cap * ENV_V_DROP
        } else {
            0.0
        }
    }

    /// Render `pokey.len()` host-rate samples of the mixed voices.
    ///
    /// `pokey` is the POKEY's normalized output (0..1 full scale); with the
    /// sound enable off the whole mix is silent. Returns i16 samples at the
    /// same full scale the amplifier model takes.
    pub fn mix_frame(&mut self, pokey: &[f32]) -> Vec<i16> {
        let dt = 1.0 / f64::from(self.sample_rate);
        let mut out = Vec::with_capacity(pokey.len());
        for &p in pokey {
            // Noise clocks: the LFSR at 6 kHz, the shell tap at /2 more, the
            // explosion NAND of four register bits at /2.
            self.lfsr_acc += (NOISE_CLOCK_HZ / 2.0) * dt;
            while self.lfsr_acc >= 1.0 {
                self.lfsr_acc -= 1.0;
                let fb =
                    ((self.lfsr >> 15) ^ (self.lfsr >> 13) ^ (self.lfsr >> 12) ^ (self.lfsr >> 10))
                        & 1;
                self.lfsr = ((self.lfsr << 1) | fb) & 0xFFFF;
                if self.lfsr == 0 {
                    self.lfsr = 0xACE1;
                }
                self.lfsr_bit = self.lfsr & 1 != 0;
                // Shell tap: the LFSR divided by two more.
                self.shell_div = !self.shell_div;
                if self.shell_div {
                    self.shell_src = self.lfsr_bit;
                }
                // Explosion tap: NAND of bits 11-14 (low unless all four are
                // set), divided by two.
                let nand = (self.lfsr >> 11) & 0xF != 0xF;
                self.nand_acc += 0.5;
                if self.nand_acc >= 1.0 {
                    self.nand_acc = 0.0;
                    self.nand_bit = nand;
                }
            }

            // The envelope RC nodes feed the custom filters through the
            // input divider. The reference clamps the filter at the
            // op-amp rail; the loud peak near 7 V never reaches it.
            let (shell_en, shell_bit) = (self.shell_on(), self.shell_src);
            let shell_in = Self::envelope(
                &mut self.shell_cap,
                shell_en,
                shell_bit,
                SHELL_TAU_CHG_S,
                SHELL_TAU_DIS_S,
                dt,
            );
            let (explo_en, explo_bit) = (self.explo_on(), self.nand_bit);
            let explo_in = Self::envelope(
                &mut self.explo_cap,
                explo_en,
                explo_bit,
                EXPLO_TAU_CHG_S,
                EXPLO_TAU_DIS_S,
                dt,
            );

            let shell_gain = if self.shell_loud() {
                VOICE_GAIN_LOUD
            } else {
                VOICE_GAIN_SOFT
            };
            let shell_v = Self::lp(
                &mut self.shell_lp,
                shell_in * VOICE_DIV * shell_gain,
                VOICE_LP_HZ,
                dt,
            );
            let explo_gain = if self.explo_loud() {
                VOICE_GAIN_LOUD
            } else {
                VOICE_GAIN_SOFT
            };
            let explo_v = Self::lp(
                &mut self.explo_lp,
                explo_in * VOICE_DIV * explo_gain,
                VOICE_LP_HZ,
                dt,
            );

            // Engine: the CV slews toward the rev-selected target, the 555
            // runs the control-voltage law off it, and the counters divide to
            // pulse trains the mixer low-passes. Silent without motor enable.
            let cv_target = if self.rev_on() { CV_REV_V } else { CV_REST_V };
            Self::slew(&mut self.cv, cv_target, CV_TAU_UP_S, CV_TAU_DOWN_S, dt);
            let fvco = ENGINE_F_BASE_HZ * CV_NOMINAL_V / self.cv;
            self.phase_12 = (self.phase_12 + fvco * dt / 12.0) % 1.0;
            self.phase_10 = (self.phase_10 + fvco * dt / 10.0) % 1.0;
            // QD (bit 3) and ripple (terminal count) of each counter: F4
            // counts 4..15, F5 6..15.
            let pulses = f64::from(u8::from(self.phase_12 < 8.0 / 12.0))
                + f64::from(u8::from(self.phase_12 > 11.0 / 12.0))
                + f64::from(u8::from(self.phase_10 < 8.0 / 10.0))
                + f64::from(u8::from(self.phase_10 > 9.0 / 10.0));
            let engine_v = if self.motor_on() {
                Self::lp(&mut self.engine_lp, TTL_V * pulses / 4.0, ENGINE_LP_HZ, dt)
            } else {
                Self::lp(&mut self.engine_lp, 0.0, ENGINE_LP_HZ, dt)
            };

            // The final mixer's Millman node: every voice sinks its node
            // voltage through its input resistor into the R29 feedback
            // load, and the output stage scales the node to full scale.
            // A plain average over the inputs would leave R29 out and run
            // every voice about 8 dB low.
            let node = (shell_v * G_SHELL
                + explo_v * G_EXPLO
                + engine_v * G_ENGINE
                + f64::from(p) * POKEY_V * G_POKEY)
                / (G_SHELL + G_EXPLO + G_ENGINE + G_POKEY + 1.0 / R_FEEDBACK);
            let mixed = node * OUT_GAIN;
            let sample = if self.sound_on() { mixed } else { 0.0 };
            out.push((sample.clamp(-1.0, 1.0) * 32767.0) as i16);
        }
        out
    }
}

impl Default for BattlezoneSound {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn silent(n: usize) -> Vec<f32> {
        vec![0.0; n]
    }

    fn rms(samples: &[i16]) -> f64 {
        (samples.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / samples.len() as f64).sqrt()
    }

    /// With the sound enable off, nothing reaches the output, even with every
    /// voice enabled: D5 mutes the POKEY with the rest.
    #[test]
    fn sound_enable_off_mutes_everything() {
        let mut s = BattlezoneSound::new();
        s.write_latch(0x80 | 0x04 | 0x01); // motor, shell, explosion; no D5
        let pokey = vec![1.0; 441];
        let out = s.mix_frame(&pokey);
        assert!(out.iter().all(|&v| v == 0));
    }

    /// Motor enable alone starts the engine voice: the mix is silent without
    /// it and audible with it.
    #[test]
    fn motor_enable_starts_the_engine() {
        let mut s = BattlezoneSound::new();
        s.write_latch(0x20); // sound on, motor off
        let out = s.mix_frame(&silent(4410));
        assert_eq!(rms(&out), 0.0);
        s.write_latch(0x20 | 0x80); // motor on
        let out = s.mix_frame(&silent(4410));
        assert!(rms(&out) > 100.0, "engine rms = {}", rms(&out));
    }

    /// Explosion loud outweighs explosion soft by the custom-filter ratio.
    #[test]
    fn explosion_loud_outweighs_soft() {
        let mut s = BattlezoneSound::new();
        s.write_latch(0x20 | 0x01); // explosion, soft
        let soft = s.mix_frame(&silent(22050));
        let mut s = BattlezoneSound::new();
        s.write_latch(0x20 | 0x01 | 0x02); // explosion, loud
        let loud = s.mix_frame(&silent(22050));
        assert!(
            rms(&loud) > rms(&soft) * 2.0,
            "loud {} against soft {}",
            rms(&loud),
            rms(&soft)
        );
    }

    /// The shell voice sounds when enabled and stops when released.
    #[test]
    fn shell_enable_gates_the_shell_voice() {
        let mut s = BattlezoneSound::new();
        s.write_latch(0x20 | 0x04 | 0x08); // shell, loud
        let on = s.mix_frame(&silent(22050));
        assert!(rms(&on) > 100.0, "shell rms = {}", rms(&on));
        s.write_latch(0x20); // released
        let off = s.mix_frame(&silent(22050));
        assert!(
            rms(&off) < rms(&on) / 4.0,
            "released {} against held {}",
            rms(&off),
            rms(&on)
        );
    }

    /// The POKEY reaches the mix on its own: an alternating full-scale input
    /// with every discrete voice off still produces output.
    #[test]
    fn pokey_reaches_the_mix_alone() {
        let mut s = BattlezoneSound::new();
        s.write_latch(0x20); // sound on, all voices off
        let pokey: Vec<f32> = (0..4410).map(|i| (i % 2) as f32).collect();
        let out = s.mix_frame(&pokey);
        assert!(rms(&out) > 100.0, "pokey rms = {}", rms(&out));
    }

    /// The final mix carries the reference's structure: a steady full-scale
    /// POKEY alone lands on the loaded-node value, about 7844, not on the
    /// unloaded average near 2100.
    #[test]
    fn pokey_only_full_scale_matches_loaded_node() {
        let mut s = BattlezoneSound::new();
        s.write_latch(0x20); // sound on, all voices off
        let out = s.mix_frame(&vec![1.0; 441]);
        let g_sum = G_SHELL + G_EXPLO + G_ENGINE + G_POKEY + 1.0 / R_FEEDBACK;
        let expected = POKEY_V * G_POKEY / g_sum * OUT_GAIN * 32767.0;
        assert!(
            out.iter().all(|&v| (f64::from(v) - expected).abs() <= 2.0),
            "pokey-only {expected:.1}, got {:?}",
            &out[..8]
        );
    }

    /// A held shell settles where the reference puts it: the RC node near
    /// 4.9 V, chopped by the noise tap, through the divider and the soft
    /// stage gain. A TTL-scaled voice would sit near 8800 instead.
    #[test]
    fn shell_settles_at_divided_node_level() {
        let mut s = BattlezoneSound::new();
        s.write_latch(0x20 | 0x04); // shell, soft
        let out = s.mix_frame(&silent(44100));
        let tail = &out[out.len() / 2..];
        let mean = tail.iter().map(|&v| f64::from(v)).sum::<f64>() / tail.len() as f64;
        let g_sum = G_SHELL + G_EXPLO + G_ENGINE + G_POKEY + 1.0 / R_FEEDBACK;
        let expected = 0.5 * ENV_V_CHG * ENV_V_DROP * VOICE_DIV * VOICE_GAIN_SOFT * G_SHELL / g_sum
            * OUT_GAIN
            * 32767.0;
        assert!(
            (mean - expected).abs() <= 400.0,
            "shell mean {mean:.0}, want {expected:.0}"
        );
    }
}
