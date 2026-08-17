//! Congo Bongo discrete percussion, synthesized with the [`DiscreteCircuit`]
//! framework. The board's two SN76489A PSGs enter the circuit as an external
//! source and are summed with five analog-style percussion voices — gorilla,
//! bass drum, low/high conga, and rim — each triggered by a bit of the sound
//! CPU's i8255 PPI (`congo_sound_b_w`/`congo_sound_c_w` in MAME `zaxxon_a.cpp`).
//!
//! In real hardware those voices are board-level analog circuits; MAME plays
//! recorded WAV samples of them instead. We synthesize them, with the
//! oscillator frequencies and envelope decays calibrated against those sample
//! recordings (`~/Downloads/congo/*.wav`): bass ~24 ms punch, congas ~60-95 ms,
//! rim ~24 ms click, gorilla a ~230 ms swelling growl.
//!
//! Each voice triggers on the falling edge of its PPI bit (MAME "start on
//! bit → 0"); the board feeds that as an active-high gate, so the components
//! retrigger on the rising edge.

use std::f64::consts::TAU;

use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::device::{
    CustomComponent, DiscreteCircuit, DiscreteCircuitBuilder, ExternalSourceId, FilterMode,
    LogicInputId, OutputGain,
};

fn sample_rate() -> u64 {
    phosphor_core::audio::host_sample_rate() as u64
}

/// PSG headroom: the music/effects are attenuated so the percussion has room to
/// sit above them without the output clamp swallowing it.
const PSG_GAIN: f64 = 0.45;

// Per-voice tuning (frequencies in Hz, decays as "time to fall to ~10%" in ms,
// from the reference WAVs). These are the obvious knobs to adjust by ear.
const BASS_FREQ: f64 = 90.0;
const BASS_PITCH_DROP: f64 = 1.3; // starts ~2.3x high, bends down to BASS_FREQ
const BASS_DECAY_MS: f64 = 55.0;
const BASS_GAIN: f64 = 0.95;

const CONGA_LOW_FREQ: f64 = 160.0;
const CONGA_LOW_DECAY_MS: f64 = 95.0;
const CONGA_HIGH_FREQ: f64 = 250.0;
const CONGA_HIGH_DECAY_MS: f64 = 110.0;
const CONGA_GAIN: f64 = 0.7;

const RIM_DECAY_MS: f64 = 22.0;
const RIM_LP_HZ: f64 = 1_800.0;
const RIM_GAIN: f64 = 0.55;

const GORILLA_ATTACK_MS: f64 = 90.0;
const GORILLA_DECAY_MS: f64 = 230.0;
const GORILLA_TREMOLO_HZ: f64 = 28.0;
const GORILLA_LP_HZ: f64 = 700.0;
const GORILLA_GAIN: f64 = 0.85;

/// Convert a "decay to 10%" time in ms to an exponential time constant (s).
fn decay_tau(ms: f64) -> f64 {
    ms / 1000.0 / std::f64::consts::LN_10
}

const ENV_FLOOR: f64 = 1e-4;

// ---------------------------------------------------------------------------
// Voice components
// ---------------------------------------------------------------------------

/// A damped sine "drum" (bass, congas): a pitched membrane hit that decays
/// exponentially. `pitch_drop` bends the frequency down as the envelope falls
/// (a punchy bass-drum sweep); 0 for the steadier congas.
struct DrumVoice {
    freq: f64,
    tau: f64,
    pitch_drop: f64,
    phase: f64,
    env: f64,
    last_gate: f64,
}

impl DrumVoice {
    fn new(freq: f64, decay_ms: f64, pitch_drop: f64) -> Self {
        Self {
            freq,
            tau: decay_tau(decay_ms),
            pitch_drop,
            phase: 0.0,
            env: 0.0,
            last_gate: 0.0,
        }
    }
}

impl CustomComponent for DrumVoice {
    fn reset(&mut self) {
        self.phase = 0.0;
        self.env = 0.0;
        self.last_gate = 0.0;
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let gate = inputs[0];
        if gate >= 0.5 && self.last_gate < 0.5 {
            self.env = 1.0;
            self.phase = 0.0;
        }
        self.last_gate = gate;
        if self.env < ENV_FLOOR {
            return 0.0;
        }
        let f = self.freq * (1.0 + self.pitch_drop * self.env);
        let out = self.phase.sin() * self.env;
        self.phase += TAU * f * dt;
        if self.phase >= TAU {
            self.phase -= TAU;
        }
        self.env *= (-dt / self.tau).exp();
        out
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.write_f64_le(self.phase);
        w.write_f64_le(self.env);
        w.write_f64_le(self.last_gate);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.phase = r.read_f64_le()?;
        self.env = r.read_f64_le()?;
        self.last_gate = r.read_f64_le()?;
        Ok(())
    }
}

/// A noise voice (rim, gorilla): white noise shaped by an attack/decay envelope,
/// with optional tremolo for the gorilla's growl. `one_shot` ignores retriggers
/// while still sounding (matches the gorilla's "start if not playing").
struct NoiseVoice {
    attack_s: f64,
    tau: f64,
    tremolo_hz: f64,
    one_shot: bool,
    lfsr: u32,
    env: f64,
    attacking: bool,
    trem_phase: f64,
    last_gate: f64,
}

impl NoiseVoice {
    const SEED: u32 = 0x1_2345;

    fn rim() -> Self {
        Self::new(0.0, RIM_DECAY_MS, 0.0, false)
    }

    fn gorilla() -> Self {
        Self::new(
            GORILLA_ATTACK_MS,
            GORILLA_DECAY_MS,
            GORILLA_TREMOLO_HZ,
            true,
        )
    }

    fn new(attack_ms: f64, decay_ms: f64, tremolo_hz: f64, one_shot: bool) -> Self {
        Self {
            attack_s: attack_ms / 1000.0,
            tau: decay_tau(decay_ms),
            tremolo_hz,
            one_shot,
            lfsr: Self::SEED,
            env: 0.0,
            attacking: false,
            trem_phase: 0.0,
            last_gate: 0.0,
        }
    }

    fn next_noise(&mut self) -> f64 {
        // 17-bit Galois LFSR.
        let feedback = self.lfsr & 1;
        self.lfsr >>= 1;
        if feedback != 0 {
            self.lfsr ^= 0x1_2000;
        }
        if self.lfsr & 1 != 0 { 1.0 } else { -1.0 }
    }
}

impl CustomComponent for NoiseVoice {
    fn reset(&mut self) {
        self.lfsr = Self::SEED;
        self.env = 0.0;
        self.attacking = false;
        self.trem_phase = 0.0;
        self.last_gate = 0.0;
    }

    fn step(&mut self, inputs: &[f64], dt: f64) -> f64 {
        let gate = inputs[0];
        if gate >= 0.5 && self.last_gate < 0.5 && !(self.one_shot && self.env > 0.01) {
            if self.attack_s > 0.0 {
                self.env = 0.0;
                self.attacking = true;
            } else {
                self.env = 1.0;
                self.attacking = false;
            }
        }
        self.last_gate = gate;
        if self.env < ENV_FLOOR && !self.attacking {
            return 0.0;
        }

        let noise = self.next_noise();
        if self.attacking {
            self.env += dt / self.attack_s;
            if self.env >= 1.0 {
                self.env = 1.0;
                self.attacking = false;
            }
        } else {
            self.env *= (-dt / self.tau).exp();
        }

        let trem = if self.tremolo_hz > 0.0 {
            let t = 0.6 + 0.4 * self.trem_phase.sin();
            self.trem_phase += TAU * self.tremolo_hz * dt;
            if self.trem_phase >= TAU {
                self.trem_phase -= TAU;
            }
            t
        } else {
            1.0
        };
        noise * self.env * trem
    }

    fn save_state(&self, w: &mut StateWriter) {
        w.write_u32_le(self.lfsr);
        w.write_f64_le(self.env);
        w.write_bool(self.attacking);
        w.write_f64_le(self.trem_phase);
        w.write_f64_le(self.last_gate);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.lfsr = r.read_u32_le()?;
        self.env = r.read_f64_le()?;
        self.attacking = r.read_bool()?;
        self.trem_phase = r.read_f64_le()?;
        self.last_gate = r.read_f64_le()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Circuit
// ---------------------------------------------------------------------------

struct CongoInputs {
    psg: ExternalSourceId,
    gorilla: LogicInputId,
    bass: LogicInputId,
    conga_low: LogicInputId,
    conga_high: LogicInputId,
    rim: LogicInputId,
}

fn build_circuit() -> (DiscreteCircuit, CongoInputs) {
    let mut b = DiscreteCircuitBuilder::new(sample_rate(), sample_rate());

    let psg = b.external_source("PSG");
    let gorilla_g = b.logic_input("GORILLA");
    let bass_g = b.logic_input("BASS");
    let conga_low_g = b.logic_input("CONGA_LOW");
    let conga_high_g = b.logic_input("CONGA_HIGH");
    let rim_g = b.logic_input("RIM");

    // Gorilla: swelling growl → low-pass to a rumble.
    let gorilla_raw = b.custom(
        "GORILLA",
        vec![gorilla_g.into()],
        Box::new(NoiseVoice::gorilla()),
    );
    let gorilla_lp = b.second_order(
        "GORILLA_LP",
        gorilla_raw,
        FilterMode::LowPass,
        GORILLA_LP_HZ,
        0.707,
    );
    let gorilla = b.gain("GORILLA_OUT", gorilla_lp, GORILLA_GAIN);

    // Bass drum: pitch-dropping damped sine.
    let bass_raw = b.custom(
        "BASS",
        vec![bass_g.into()],
        Box::new(DrumVoice::new(BASS_FREQ, BASS_DECAY_MS, BASS_PITCH_DROP)),
    );
    let bass = b.gain("BASS_OUT", bass_raw, BASS_GAIN);

    // Congas: steadier pitched hits.
    let conga_low_raw = b.custom(
        "CONGA_LOW",
        vec![conga_low_g.into()],
        Box::new(DrumVoice::new(CONGA_LOW_FREQ, CONGA_LOW_DECAY_MS, 0.0)),
    );
    let conga_low = b.gain("CONGA_LOW_OUT", conga_low_raw, CONGA_GAIN);

    let conga_high_raw = b.custom(
        "CONGA_HIGH",
        vec![conga_high_g.into()],
        Box::new(DrumVoice::new(CONGA_HIGH_FREQ, CONGA_HIGH_DECAY_MS, 0.0)),
    );
    let conga_high = b.gain("CONGA_HIGH_OUT", conga_high_raw, CONGA_GAIN);

    // Rim: bright noise click.
    let rim_raw = b.custom("RIM", vec![rim_g.into()], Box::new(NoiseVoice::rim()));
    let rim_lp = b.second_order("RIM_LP", rim_raw, FilterMode::LowPass, RIM_LP_HZ, 0.707);
    let rim = b.gain("RIM_OUT", rim_lp, RIM_GAIN);

    let mix = b.add(
        "MIX",
        &[psg.into(), gorilla, bass, conga_low, conga_high, rim],
    );
    b.output(mix, OutputGain::unity());

    (
        b.build(),
        CongoInputs {
            psg,
            gorilla: gorilla_g,
            bass: bass_g,
            conga_low: conga_low_g,
            conga_high: conga_high_g,
            rim: rim_g,
        },
    )
}

// ---------------------------------------------------------------------------
// Device
// ---------------------------------------------------------------------------

/// Congo Bongo percussion: the PSG mix summed with the five synthesized voices.
pub struct CongoSound {
    circuit: DiscreteCircuit,
    ids: CongoInputs,
}

impl Default for CongoSound {
    fn default() -> Self {
        Self::new()
    }
}

impl CongoSound {
    pub fn new() -> Self {
        let (circuit, ids) = build_circuit();
        Self { circuit, ids }
    }

    /// Feed one box-filtered PSG sample (the SN76489A mix) and advance the
    /// circuit one step, producing one output sample.
    pub fn feed_psg(&mut self, sample: i16) {
        self.circuit
            .set_external(self.ids.psg, sample as f64 / 32767.0 * PSG_GAIN);
        self.circuit.tick(1);
    }

    /// Update the percussion gates from the PPI port B/C output latches. Each
    /// voice is active while its bit is low (MAME "start on bit → 0").
    pub fn set_triggers(&mut self, port_b: u8, port_c: u8) {
        self.circuit.set_logic(self.ids.gorilla, port_b & 0x02 == 0);
        self.circuit.set_logic(self.ids.bass, port_c & 0x01 == 0);
        self.circuit
            .set_logic(self.ids.conga_low, port_c & 0x02 == 0);
        self.circuit
            .set_logic(self.ids.conga_high, port_c & 0x04 == 0);
        self.circuit.set_logic(self.ids.rim, port_c & 0x08 == 0);
    }

    /// Drain produced mono `i16` samples.
    pub fn fill_audio(&mut self, out: &mut [i16]) -> usize {
        self.circuit.fill_audio(out)
    }

    pub fn reset(&mut self) {
        self.circuit.reset();
    }
}

impl Saveable for CongoSound {
    fn save_state(&self, w: &mut StateWriter) {
        self.circuit.save_state(w);
    }
    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.circuit.load_state(r)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms(samples: &[i16]) -> f64 {
        let sum: f64 = samples.iter().map(|&s| (s as f64).powi(2)).sum();
        (sum / samples.len().max(1) as f64).sqrt()
    }

    /// Drive the circuit for `ms` with the given trigger ports and return the
    /// rendered samples.
    fn render(snd: &mut CongoSound, ms: u64, port_b: u8, port_c: u8) -> Vec<i16> {
        snd.set_triggers(port_b, port_c);
        let n = (sample_rate() * ms / 1000) as usize;
        for _ in 0..n {
            snd.feed_psg(0); // silent PSG so we measure percussion only
        }
        let mut out = vec![0i16; n + 64];
        let got = snd.fill_audio(&mut out);
        out.truncate(got);
        out
    }

    #[test]
    fn idle_is_silent() {
        let mut snd = CongoSound::new();
        let out = render(&mut snd, 20, 0xff, 0xff); // all bits high = no trigger
        assert_eq!(rms(&out), 0.0);
    }

    #[test]
    fn each_voice_makes_sound_on_its_bit() {
        // (port_b, port_c) with exactly one trigger bit pulled low.
        for (name, b, c) in [
            ("gorilla", 0xfd, 0xff),    // port B bit 1
            ("bass", 0xff, 0xfe),       // port C bit 0
            ("conga_low", 0xff, 0xfd),  // port C bit 1
            ("conga_high", 0xff, 0xfb), // port C bit 2
            ("rim", 0xff, 0xf7),        // port C bit 3
        ] {
            let mut snd = CongoSound::new();
            // Edge: start high (idle), then pull the bit low to trigger.
            snd.set_triggers(0xff, 0xff);
            snd.feed_psg(0);
            let out = render(&mut snd, 60, b, c);
            assert!(
                rms(&out) > 50.0,
                "{name} should be audible (rms={})",
                rms(&out)
            );
        }
    }

    #[test]
    fn psg_passes_through() {
        let mut snd = CongoSound::new();
        snd.set_triggers(0xff, 0xff);
        for _ in 0..1000 {
            snd.feed_psg(20_000);
        }
        let mut out = vec![0i16; 1100];
        let n = snd.fill_audio(&mut out);
        assert!(rms(&out[..n]) > 1000.0, "PSG mix reaches the output");
    }

    #[test]
    fn save_load_round_trip() {
        let mut snd = CongoSound::new();
        let _ = render(&mut snd, 10, 0xff, 0xfe); // trigger bass (port C bit 0)

        let mut w = StateWriter::new();
        snd.save_state(&mut w);
        let bytes = w.into_vec();

        let mut restored = CongoSound::new();
        let mut r = StateReader::new(&bytes);
        restored.load_state(&mut r).unwrap();
        // Both continue identically from the saved point.
        let a = render(&mut snd, 10, 0xff, 0xff);
        let b = render(&mut restored, 10, 0xff, 0xff);
        assert_eq!(a, b);
    }
}
