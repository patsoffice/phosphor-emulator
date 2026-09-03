//! Shared audio resampling utilities.
//!
//! Provides a generic Bresenham box-filter downsampler for converting
//! high-frequency audio (at CPU clock rates) to standard output sample rates
//! (e.g., 44.1 kHz). Use `AudioResampler<i16>` for integer pipelines (most
//! devices) or `AudioResampler<f32>` for float pipelines (POKEY, etc.).
//!
//! Output is queued in a [`SampleRing`], which is also available on its own for
//! the machine-level mix buffers that sit downstream of the per-device
//! resamplers.

pub mod analysis;
mod biquad;
mod dc_blocker;
pub mod fir;
mod ring;

pub use biquad::Biquad;
pub use dc_blocker::DcBlocker;
pub use fir::DecimatingFir;
pub use ring::SampleRing;

use crate::core::save_state::{SaveError, StateReader, StateWriter};
use crate::prelude::Saveable;
use std::sync::atomic::{AtomicU32, Ordering};

// ---------------------------------------------------------------------------
// Host output rate
// ---------------------------------------------------------------------------

/// The rate to ask the host for, and the rate used when nothing negotiated one.
pub const DEFAULT_HOST_SAMPLE_RATE: u32 = 44_100;

/// The rate every device resamples to.
///
/// This is process-wide because the thing it describes is process-wide: there
/// is one host audio device, and every sound chip in the machine has to land on
/// its clock. Threading it through each device constructor instead would mean a
/// parameter on every machine factory for a value that can only ever have one
/// answer per run.
///
/// The ordering rule is the price: [`set_host_sample_rate`] must be called
/// before the machine is built, because devices read it when they construct
/// their resamplers. The frontend does this once, after asking the audio device
/// what it will actually grant and before creating the machine.
static HOST_SAMPLE_RATE: AtomicU32 = AtomicU32::new(DEFAULT_HOST_SAMPLE_RATE);

/// The rate every device resamples to. Defaults to
/// [`DEFAULT_HOST_SAMPLE_RATE`] until a host negotiates otherwise.
pub fn host_sample_rate() -> u32 {
    HOST_SAMPLE_RATE.load(Ordering::Relaxed)
}

/// Set the rate every subsequently constructed device resamples to.
///
/// Call before building a machine. A device that already exists keeps the rate
/// it was built with — a resampler derives its Bresenham ratio at construction,
/// so changing this afterwards would leave the machine's chips disagreeing
/// about where their samples land. Passing 0 is ignored, since a rate of zero
/// means "no audio" elsewhere in the frontend contract.
pub fn set_host_sample_rate(rate: u32) {
    if rate > 0 {
        HOST_SAMPLE_RATE.store(rate, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Sample trait
// ---------------------------------------------------------------------------

/// Trait abstracting over audio sample types (`i16`, `f32`).
///
/// Each sample type has an associated accumulator type used for precise
/// averaging during downsampling (e.g., `i64` for `i16` to avoid overflow).
pub trait Sample: Copy + Default {
    /// Wider accumulator type used during box-filter averaging.
    type Accum: Copy + Default;

    /// Add a sample value to the accumulator.
    fn accum_add(accum: &mut Self::Accum, sample: Self);

    /// Compute the average from the accumulator and sample count.
    fn accum_avg(accum: Self::Accum, count: u32) -> Self;

    /// Convert to the `f32` the anti-alias filter works in.
    fn to_f32(self) -> f32;

    /// Convert back from the filter's `f32`, saturating rather than wrapping —
    /// a windowed-sinc filter overshoots slightly at a step.
    fn from_f32(value: f32) -> Self;

    /// Save the accumulator to a state writer (format-preserving).
    fn save_accum(accum: &Self::Accum, w: &mut StateWriter);

    /// Load the accumulator from a state reader (format-preserving).
    fn load_accum(r: &mut StateReader) -> Result<Self::Accum, SaveError>;
}

impl Sample for i16 {
    type Accum = i64;

    #[inline]
    fn accum_add(accum: &mut i64, sample: i16) {
        *accum += sample as i64;
    }

    #[inline]
    fn accum_avg(accum: i64, count: u32) -> i16 {
        (accum / count as i64) as i16
    }

    #[inline]
    fn to_f32(self) -> f32 {
        self as f32
    }

    #[inline]
    fn from_f32(value: f32) -> i16 {
        value.round().clamp(i16::MIN as f32, i16::MAX as f32) as i16
    }

    fn save_accum(accum: &i64, w: &mut StateWriter) {
        w.write_i64_le(*accum);
    }

    fn load_accum(r: &mut StateReader) -> Result<i64, SaveError> {
        r.read_i64_le()
    }
}

impl Sample for f32 {
    type Accum = f32;

    #[inline]
    fn accum_add(accum: &mut f32, sample: f32) {
        *accum += sample;
    }

    #[inline]
    fn accum_avg(accum: f32, count: u32) -> f32 {
        accum / count as f32
    }

    #[inline]
    fn to_f32(self) -> f32 {
        self
    }

    #[inline]
    fn from_f32(value: f32) -> f32 {
        value
    }

    fn save_accum(accum: &f32, w: &mut StateWriter) {
        w.write_f32_le(*accum);
    }

    fn load_accum(r: &mut StateReader) -> Result<f32, SaveError> {
        r.read_f32_le()
    }
}

// ---------------------------------------------------------------------------
// AudioResampler<T>
// ---------------------------------------------------------------------------

/// Two-stage audio resampler.
///
/// Stage one is a Bresenham box filter from the input clock down to an
/// intermediate rate of [`fir::DECIMATION`]× the output rate: one add per
/// emulated cycle, which is what keeps the per-cycle path cheap. Stage two is a
/// windowed-sinc [`DecimatingFir`] from there to the output rate, and it runs
/// once per *output* sample.
///
/// The split is what gives the path a real stopband. A box filter alone has its
/// first sidelobe only 13 dB down, so content above the output Nyquist folds
/// back into the audible band as inharmonic grit — see [`fir`] for the detail.
/// Reaching only 4× the output rate keeps the whole audio band inside the box's
/// flat region, and the FIR does the actual anti-aliasing.
///
/// The filter costs about 0.28 ms of group delay and a short start-up transient
/// while its delay line fills. Sample *counts* are unaffected in either
/// direction.
pub struct AudioResampler<T: Sample> {
    sample_accum: T::Accum,
    sample_count: u32,
    sample_phase: u64,
    input_rate: u64,
    output_rate: u64,
    /// `output_rate * fir::DECIMATION`, kept so the hot path does not multiply.
    intermediate_rate: u64,
    fir: DecimatingFir,
    buffer: SampleRing<T>,
}

impl<T: Sample> AudioResampler<T> {
    /// Create a new resampler.
    ///
    /// - `input_rate`: source clock rate in Hz (e.g., 3_072_000 for 3.072 MHz CPU)
    /// - `output_rate`: target sample rate in Hz (e.g., 44_100)
    pub fn new(input_rate: u64, output_rate: u64) -> Self {
        Self {
            input_rate,
            output_rate,
            intermediate_rate: output_rate * fir::DECIMATION as u64,
            sample_accum: T::Accum::default(),
            sample_count: 0,
            sample_phase: 0,
            fir: DecimatingFir::new(),
            buffer: SampleRing::new(),
        }
    }

    /// The host sample rate this resampler targets.
    pub fn output_rate(&self) -> u64 {
        self.output_rate
    }

    /// Number of output samples produced but not yet drained.
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// Output samples dropped because the caller stopped draining. Always zero
    /// in normal operation; see [`SampleRing::overruns`].
    pub fn overruns(&self) -> u64 {
        self.buffer.overruns()
    }

    /// Accumulate one input sample, pushing any completed output samples to the
    /// internal buffer.
    ///
    /// Handles both directions: when the input clock is faster than the
    /// intermediate rate each tick completes at most one stage-one sample; when
    /// it is slower (the TMS5220's 8 kHz, say) one tick completes several, held
    /// from the same average. Either way the anti-alias filter smooths the
    /// result, so the count of output samples per second of input is exact
    /// while the values are filtered rather than held. (The single-step
    /// [`Self::tick_sample`] only supports downsampling.)
    #[inline]
    pub fn tick(&mut self, sample: T) {
        T::accum_add(&mut self.sample_accum, sample);
        self.sample_count += 1;
        self.sample_phase += self.intermediate_rate;
        if self.sample_phase < self.input_rate {
            return;
        }
        let avg = T::accum_avg(self.sample_accum, self.sample_count).to_f32();
        self.sample_accum = T::Accum::default();
        self.sample_count = 0;
        // One stage-one sample per input period consumed. Each is offered to
        // the filter, which returns an output on every DECIMATION-th.
        loop {
            self.sample_phase -= self.input_rate;
            if let Some(out) = self.fir.push(avg) {
                self.buffer.push(T::from_f32(out));
            }
            if self.sample_phase < self.input_rate {
                break;
            }
        }
    }

    /// Accumulate one input sample. If this tick completes an output sample,
    /// returns it without pushing it to the buffer.
    ///
    /// Use this when you need to post-process (e.g., mix with another source)
    /// before calling [`Self::push_sample`]. Advances stage one by a single step, so
    /// it is only correct when the input clock is faster than the intermediate
    /// rate — every caller drives it from a CPU clock.
    #[inline]
    pub fn tick_sample(&mut self, sample: T) -> Option<T> {
        T::accum_add(&mut self.sample_accum, sample);
        self.sample_count += 1;
        self.sample_phase += self.intermediate_rate;

        if self.sample_phase >= self.input_rate {
            self.sample_phase -= self.input_rate;
            let avg = if self.sample_count > 0 {
                T::accum_avg(self.sample_accum, self.sample_count).to_f32()
            } else {
                0.0
            };
            self.sample_accum = T::Accum::default();
            self.sample_count = 0;
            self.fir.push(avg).map(T::from_f32)
        } else {
            None
        }
    }

    /// Manually push a sample to the output buffer.
    ///
    /// Use after [`Self::tick_sample`] returns `Some` and you've mixed or
    /// post-processed the resampled sample. The sample goes straight to the
    /// queue — it is already at the output rate, so it bypasses both stages.
    #[inline]
    pub fn push_sample(&mut self, sample: T) {
        self.buffer.push(sample);
    }

    /// Change the input (source) sample rate, e.g. when the driving clock is
    /// retuned at runtime. Buffered output and the resampling phase are left
    /// intact; the phase self-corrects against the new rate on the next tick.
    pub fn set_input_rate(&mut self, input_rate: u64) {
        self.input_rate = input_rate;
    }

    /// The input (source) sample rate this resampler is converting from.
    ///
    /// Exposed so a board can be checked to be resampling from the same rate it
    /// clocks the source at. Those are two numbers that have to agree and, when
    /// they are written down separately, quietly do not.
    pub fn input_rate(&self) -> u64 {
        self.input_rate
    }

    /// Drain audio samples into the provided buffer. Returns the number
    /// of samples written.
    pub fn fill_audio(&mut self, buffer: &mut [T]) -> usize {
        self.buffer.pop_front_into(buffer)
    }

    /// Take all buffered samples, leaving the buffer empty.
    pub fn drain_audio(&mut self) -> Vec<T> {
        self.buffer.drain_all()
    }

    /// Clear all runtime state (phase, accumulator, filter delay line, buffer).
    pub fn reset(&mut self) {
        self.sample_accum = T::Accum::default();
        self.sample_count = 0;
        self.sample_phase = 0;
        self.fir.reset();
        self.buffer.clear();
    }
}

/// Manual `Saveable` implementation: version(2) + accumulator + sample_count +
/// sample_phase + the anti-alias filter's delay line.
///
/// Version 2 added the filter state. Without it a load would resume with a
/// zeroed delay line and click for the ~0.3 ms the filter takes to refill, and
/// save/load would not round-trip. The output queue and the rates stay
/// transient.
impl<T: Sample> Saveable for AudioResampler<T> {
    fn save_state(&self, w: &mut StateWriter) {
        w.write_version(2);
        T::save_accum(&self.sample_accum, w);
        w.write_u32_le(self.sample_count);
        w.write_u64_le(self.sample_phase);
        for &tap in self.fir.history() {
            w.write_f32_le(tap);
        }
        let pending = self.fir.pending();
        w.write_u8(pending.len() as u8);
        for &sample in pending {
            w.write_f32_le(sample);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        r.read_version(2)?;
        self.sample_accum = T::load_accum(r)?;
        self.sample_count = r.read_u32_le()?;
        self.sample_phase = r.read_u64_le()?;

        let mut history = [0.0f32; fir::TAPS];
        for tap in history.iter_mut() {
            *tap = r.read_f32_le()?;
        }
        let pending_len = r.read_u8()? as usize;
        if pending_len >= fir::DECIMATION {
            return Err(SaveError::InvalidFormat(format!(
                "resampler filter has {pending_len} pending samples, expected under {}",
                fir::DECIMATION
            )));
        }
        let mut pending = [0.0f32; fir::DECIMATION];
        for sample in pending.iter_mut().take(pending_len) {
            *sample = r.read_f32_le()?;
        }
        self.fir.restore(history, &pending[..pending_len])?;

        self.buffer.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- AudioResampler<i16> tests --

    #[test]
    fn resampler_produces_correct_sample_count() {
        let mut r = AudioResampler::<i16>::new(1_000_000, 44_100);
        for _ in 0..1_000_000 {
            r.tick(1000);
        }
        let mut buf = vec![0i16; 50_000];
        let n = r.fill_audio(&mut buf);
        // Should produce approximately 44100 samples (± 1 for rounding)
        assert!(
            (44_099..=44_101).contains(&n),
            "expected ~44100 samples, got {n}"
        );
    }

    #[test]
    fn resampler_upsamples_to_full_output_count() {
        // Upsampling (input 8135 Hz < output 44100 Hz): one second of input must
        // yield ~44100 output samples. A downsample-only resampler would emit
        // only ~8135 here (the TMS5220 "slow/choppy speech" bug).
        let mut r = AudioResampler::<i16>::new(8_135, 44_100);
        for _ in 0..8_135 {
            r.tick(1000);
        }
        let n = r.drain_audio().len();
        assert!(
            (44_090..=44_110).contains(&n),
            "expected ~44100 upsampled samples, got {n}"
        );
    }

    #[test]
    fn resampler_upsample_emits_one_output_per_period() {
        // A single input at 3x upsampling still completes three output periods.
        // The values are filtered rather than sample-and-held, so only the count
        // is pinned here — `resampler_reproduces_a_constant` covers the values.
        let mut r = AudioResampler::<i16>::new(1, 3);
        r.tick(500);
        assert_eq!(r.drain_audio().len(), 3);
    }

    #[test]
    fn resampler_emits_one_output_per_input_period() {
        // Input rate 4, output rate 1: four inputs complete one output.
        let mut r = AudioResampler::<i16>::new(4, 1);
        r.tick(100);
        r.tick(200);
        r.tick(300);
        r.tick(400);

        let mut buf = [0i16; 4];
        assert_eq!(r.fill_audio(&mut buf), 1);
    }

    #[test]
    fn resampler_reproduces_a_constant() {
        // The filter has unit DC gain, so once its delay line has filled a
        // constant input comes back out unchanged. This is the value contract
        // that replaced the old "emits the box average" assertion.
        let mut r = AudioResampler::<i16>::new(1_000_000, 44_100);
        for _ in 0..1_000_000 {
            r.tick(1000);
        }
        let out = r.drain_audio();
        let settled = &out[out.len() / 2..];
        assert!(
            settled.iter().all(|&s| (s - 1000).abs() <= 1),
            "steady state should hold 1000, got {:?}",
            &settled[..8]
        );
    }

    #[test]
    fn tick_sample_returns_the_output_without_pushing() {
        // One `Some` per output period, whatever the filter's start-up value.
        let mut r = AudioResampler::<i16>::new(4, 1);
        assert_eq!(r.tick_sample(100), None);
        assert_eq!(r.tick_sample(200), None);
        assert_eq!(r.tick_sample(300), None);
        assert!(r.tick_sample(400).is_some());

        // Buffer should be empty since tick_sample doesn't push
        let mut buf = [0i16; 4];
        assert_eq!(r.fill_audio(&mut buf), 0);
    }

    #[test]
    fn push_sample_adds_to_buffer() {
        let mut r = AudioResampler::<i16>::new(1_000_000, 44_100);
        r.push_sample(42);
        r.push_sample(-100);

        let mut buf = [0i16; 4];
        let n = r.fill_audio(&mut buf);
        assert_eq!(n, 2);
        assert_eq!(buf[0], 42);
        assert_eq!(buf[1], -100);
    }

    #[test]
    fn fill_audio_drains_buffer() {
        let mut r = AudioResampler::<i16>::new(2, 1);
        r.tick(100);
        r.tick(200); // completes one output period

        let mut buf = [0i16; 1];
        assert_eq!(r.fill_audio(&mut buf), 1);

        // Second call should return 0 (drained)
        assert_eq!(r.fill_audio(&mut buf), 0);
    }

    #[test]
    fn reset_clears_all_state() {
        let mut r = AudioResampler::<i16>::new(2, 1);
        r.tick(100);
        r.tick(200);
        r.reset();

        assert_eq!(r.sample_accum, 0);
        assert_eq!(r.sample_count, 0);
        assert_eq!(r.sample_phase, 0);
        assert!(r.buffer.is_empty());
        assert!(r.fir.history().iter().all(|&t| t == 0.0));
        assert!(r.fir.pending().is_empty());
    }

    #[test]
    fn save_load_round_trip() {
        let mut r = AudioResampler::<i16>::new(1_000_000, 44_100);
        for _ in 0..500 {
            r.tick(1234);
        }

        let mut w = StateWriter::new();
        r.save_state(&mut w);
        let data = w.into_vec();

        let mut r2 = AudioResampler::<i16>::new(1_000_000, 44_100);
        let mut reader = StateReader::new(&data);
        r2.load_state(&mut reader).unwrap();

        assert_eq!(r2.sample_accum, r.sample_accum);
        assert_eq!(r2.sample_count, r.sample_count);
        assert_eq!(r2.sample_phase, r.sample_phase);
        assert_eq!(r2.fir.history(), r.fir.history());
        assert_eq!(r2.fir.pending(), r.fir.pending());
    }

    #[test]
    fn a_loaded_resampler_continues_the_same_waveform() {
        // The filter's delay line is machine state: resuming with a zeroed one
        // would click. Drive two resamplers identically, snapshot one into the
        // other mid-stream, then check they agree from there on.
        let tone = |i: u32| ((i % 37) as i16 - 18) * 400;

        let mut a = AudioResampler::<i16>::new(1_789_000, 44_100);
        for i in 0..20_000 {
            a.tick(tone(i));
        }
        a.drain_audio();

        let mut w = StateWriter::new();
        a.save_state(&mut w);
        let data = w.into_vec();
        let mut b = AudioResampler::<i16>::new(1_789_000, 44_100);
        b.load_state(&mut StateReader::new(&data)).unwrap();

        for i in 20_000..30_000 {
            a.tick(tone(i));
            b.tick(tone(i));
        }
        assert_eq!(a.drain_audio(), b.drain_audio());
    }

    // -- AudioResampler<f32> tests --

    #[test]
    fn f32_resampler_produces_correct_count() {
        let mut r = AudioResampler::<f32>::new(1_000_000, 44_100);
        for _ in 0..1_000_000 {
            r.tick(0.5);
        }
        let samples = r.drain_audio();
        assert!(
            (44_099..=44_101).contains(&samples.len()),
            "expected ~44100 samples, got {}",
            samples.len()
        );
    }

    #[test]
    fn f32_resampler_emits_one_output_per_input_period() {
        let mut r = AudioResampler::<f32>::new(4, 1);
        r.tick(0.1);
        r.tick(0.2);
        r.tick(0.3);
        r.tick(0.4);
        assert_eq!(r.drain_audio().len(), 1);
    }

    #[test]
    fn f32_resampler_reproduces_a_constant() {
        let mut r = AudioResampler::<f32>::new(1_000_000, 44_100);
        for _ in 0..1_000_000 {
            r.tick(0.5);
        }
        let out = r.drain_audio();
        let settled = &out[out.len() / 2..];
        assert!(
            settled.iter().all(|&s| (s - 0.5).abs() < 1e-5),
            "steady state should hold 0.5, got {:?}",
            &settled[..8]
        );
    }

    #[test]
    fn f32_save_load_round_trip() {
        let mut r = AudioResampler::<f32>::new(1_000_000, 44_100);
        for _ in 0..500 {
            r.tick(0.42);
        }

        let mut w = StateWriter::new();
        r.save_state(&mut w);
        let data = w.into_vec();

        let mut r2 = AudioResampler::<f32>::new(1_000_000, 44_100);
        let mut reader = StateReader::new(&data);
        r2.load_state(&mut reader).unwrap();

        assert_eq!(r2.sample_count, r.sample_count);
        assert_eq!(r2.sample_phase, r.sample_phase);
        assert!((r2.sample_accum - r.sample_accum).abs() < 1e-6);
    }

    // -- Anti-aliasing --

    /// Amplitude of `hz` in `samples`, taken at `rate`, via a Hann-windowed
    /// single-bin DFT. Frequencies are chosen to land on exact bins, so the
    /// window is only there to suppress edge effects.
    fn bin_amplitude(samples: &[f32], rate: f64, hz: f64) -> f64 {
        let n = samples.len() as f64;
        let (mut re, mut im, mut window_sum) = (0.0, 0.0, 0.0);
        for (i, &s) in samples.iter().enumerate() {
            let t = i as f64;
            let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * t / n).cos();
            let phase = -2.0 * std::f64::consts::PI * hz * t / rate;
            re += s as f64 * w * phase.cos();
            im += s as f64 * w * phase.sin();
            window_sum += w;
        }
        // Divide by the window's coherent gain, so a unit-amplitude sine on an
        // exact bin reads back as 1.0.
        2.0 * (re * re + im * im).sqrt() / window_sum
    }

    /// Resample a sine at `hz` from `input_rate` to 44.1 kHz and return a
    /// settled second-half slice of the output.
    fn resample_tone(hz: f64, input_rate: u64) -> Vec<f32> {
        let mut r = AudioResampler::<f32>::new(input_rate, 44_100);
        // 0.2 s of input: the first half is discarded so the filter's delay
        // line and the analysis are both looking at steady state.
        let ticks = input_rate / 5;
        for i in 0..ticks {
            let t = i as f64 / input_rate as f64;
            r.tick((2.0 * std::f64::consts::PI * hz * t).sin() as f32);
        }
        let out = r.drain_audio();
        out[out.len() / 2..][..4410].to_vec()
    }

    #[test]
    fn content_above_the_output_nyquist_does_not_fold_back() {
        // The acceptance gate for two-stage decimation. A 30 kHz tone is above
        // the 22.05 kHz output Nyquist; decimation folds it to 14.1 kHz. Under
        // the old single box filter it arrived there at roughly −5 dB, which is
        // the inharmonic grit this filter exists to remove.
        //
        // 1.764 MHz in is 40× the output rate and exactly 10× the intermediate
        // rate, so stage one is a clean length-10 box. Measured rejection is
        // about 91 dB; the gate is set at 60 to leave design headroom.
        const INPUT_RATE: u64 = 1_764_000;
        const ALIAS_HZ: f64 = 14_100.0; // 44_100 − 30_000

        let aliased = bin_amplitude(&resample_tone(30_000.0, INPUT_RATE), 44_100.0, ALIAS_HZ);
        let reference = bin_amplitude(&resample_tone(ALIAS_HZ, INPUT_RATE), 44_100.0, ALIAS_HZ);

        let rejection_db = 20.0 * (aliased / reference).log10();
        assert!(
            rejection_db < -60.0,
            "30 kHz folded to 14.1 kHz at only {rejection_db:.1} dB below \
             an equal in-band tone; expected at least 60 dB of rejection"
        );
    }

    #[test]
    fn audible_content_passes_at_unity() {
        // The other half of the contract: rejecting the stopband is only useful
        // if the passband survives.
        const INPUT_RATE: u64 = 1_764_000;
        for hz in [500.0, 2_000.0, 8_000.0, 15_000.0] {
            let amplitude = bin_amplitude(&resample_tone(hz, INPUT_RATE), 44_100.0, hz);
            let db = 20.0 * amplitude.log10();
            assert!(
                db.abs() < 0.5,
                "{hz} Hz came through at {db:+.2} dB, expected unity"
            );
        }
    }
}
