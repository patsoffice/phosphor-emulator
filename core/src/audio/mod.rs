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

pub mod fir;
mod ring;

pub use fir::DecimatingFir;
pub use ring::SampleRing;

use crate::core::save_state::{SaveError, StateReader, StateWriter};
use crate::prelude::Saveable;

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

/// Bresenham box-filter audio resampler.
///
/// Downsamples from an input clock rate to an output sample rate using
/// box-filter averaging. Each `tick()` accumulates a sample; when the
/// Bresenham phase crosses the threshold, the averaged sample is pushed
/// to the internal buffer.
pub struct AudioResampler<T: Sample> {
    sample_accum: T::Accum,
    sample_count: u32,
    sample_phase: u64,
    input_rate: u64,
    output_rate: u64,
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
            sample_accum: T::Accum::default(),
            sample_count: 0,
            sample_phase: 0,
            buffer: SampleRing::new(),
        }
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
    /// Handles both directions: when downsampling (`input_rate > output_rate`)
    /// each input completes at most one box-filtered output; when upsampling
    /// (`output_rate > input_rate`) one input can complete several output
    /// samples, which are sample-and-held from the same average. (The single-emit
    /// [`tick_sample`] only supports downsampling.)
    #[inline]
    pub fn tick(&mut self, sample: T) {
        T::accum_add(&mut self.sample_accum, sample);
        self.sample_count += 1;
        self.sample_phase += self.output_rate;
        if self.sample_phase < self.input_rate {
            return;
        }
        let avg = T::accum_avg(self.sample_accum, self.sample_count);
        self.sample_accum = T::Accum::default();
        self.sample_count = 0;
        // Emit one output per input period consumed: exactly one when
        // downsampling, several (sample-and-hold) when upsampling.
        loop {
            self.sample_phase -= self.input_rate;
            self.buffer.push(avg);
            if self.sample_phase < self.input_rate {
                break;
            }
        }
    }

    /// Accumulate one input sample. If this tick completes an output sample,
    /// returns the box-filtered average without pushing it to the buffer.
    ///
    /// Use this when you need to post-process (e.g., mix with another source)
    /// before calling [`push_sample`].
    #[inline]
    pub fn tick_sample(&mut self, sample: T) -> Option<T> {
        T::accum_add(&mut self.sample_accum, sample);
        self.sample_count += 1;
        self.sample_phase += self.output_rate;

        if self.sample_phase >= self.input_rate {
            self.sample_phase -= self.input_rate;
            let avg = if self.sample_count > 0 {
                T::accum_avg(self.sample_accum, self.sample_count)
            } else {
                T::default()
            };
            self.sample_accum = T::Accum::default();
            self.sample_count = 0;
            Some(avg)
        } else {
            None
        }
    }

    /// Manually push a sample to the output buffer.
    ///
    /// Use after [`tick_sample`] returns `Some` and you've mixed or
    /// post-processed the averaged sample.
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

    /// Drain audio samples into the provided buffer. Returns the number
    /// of samples written.
    pub fn fill_audio(&mut self, buffer: &mut [T]) -> usize {
        self.buffer.pop_front_into(buffer)
    }

    /// Take all buffered samples, leaving the buffer empty.
    pub fn drain_audio(&mut self) -> Vec<T> {
        self.buffer.drain_all()
    }

    /// Clear all runtime state (phase, accumulator, buffer).
    pub fn reset(&mut self) {
        self.sample_accum = T::Accum::default();
        self.sample_count = 0;
        self.sample_phase = 0;
        self.buffer.clear();
    }
}

/// Manual `Saveable` implementation preserving the existing save format:
/// version(1) + accumulator + sample_count + sample_phase.
/// Buffer and rates are transient and not serialized.
impl<T: Sample> Saveable for AudioResampler<T> {
    fn save_state(&self, w: &mut StateWriter) {
        w.write_version(1);
        T::save_accum(&self.sample_accum, w);
        w.write_u32_le(self.sample_count);
        w.write_u64_le(self.sample_phase);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        r.read_version(1)?;
        self.sample_accum = T::load_accum(r)?;
        self.sample_count = r.read_u32_le()?;
        self.sample_phase = r.read_u64_le()?;
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
    fn resampler_upsample_holds_sample_value() {
        // A single input at 3x upsampling holds its value across three outputs.
        let mut r = AudioResampler::<i16>::new(1, 3);
        r.tick(500);
        let out = r.drain_audio();
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|&s| s == 500));
    }

    #[test]
    fn resampler_averages_correctly() {
        // Input rate 4, output rate 1: averages 4 samples into 1
        let mut r = AudioResampler::<i16>::new(4, 1);
        r.tick(100);
        r.tick(200);
        r.tick(300);
        r.tick(400); // average = 250

        let mut buf = [0i16; 4];
        let n = r.fill_audio(&mut buf);
        assert_eq!(n, 1);
        assert_eq!(buf[0], 250);
    }

    #[test]
    fn tick_sample_returns_average_without_pushing() {
        let mut r = AudioResampler::<i16>::new(4, 1);
        assert_eq!(r.tick_sample(100), None);
        assert_eq!(r.tick_sample(200), None);
        assert_eq!(r.tick_sample(300), None);
        assert_eq!(r.tick_sample(400), Some(250));

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
        r.tick(200); // produces one sample: avg 150

        let mut buf = [0i16; 1];
        assert_eq!(r.fill_audio(&mut buf), 1);
        assert_eq!(buf[0], 150);

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
    fn f32_resampler_averages_correctly() {
        let mut r = AudioResampler::<f32>::new(4, 1);
        r.tick(0.1);
        r.tick(0.2);
        r.tick(0.3);
        r.tick(0.4);
        let samples = r.drain_audio();
        assert_eq!(samples.len(), 1);
        assert!((samples[0] - 0.25).abs() < 1e-6);
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
}
