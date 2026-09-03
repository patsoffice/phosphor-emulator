use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI16, AtomicU64, AtomicUsize, Ordering};

use sdl2::audio::{AudioCallback, AudioDevice, AudioSpecDesired};

/// Number of frames over which to fade in/out (~5.8 ms at 44.1 kHz).
///
/// Frames, not samples: a stereo machine's fade must last the same *time* as a
/// mono one's, not half of it.
const FADE_FRAMES: u32 = 256;

/// Capacity of the transport ring, in frames — about 186 ms at 44.1 kHz.
///
/// Frames for the same reason. The prefill below waits for half of this before
/// starting playback, and that margin is a duration: sizing the ring in samples
/// would silently halve it for a stereo machine, which is exactly the margin
/// that absorbs frame-time jitter.
const RING_CAPACITY_FRAMES: usize = 8192;

// ---------------------------------------------------------------------------
// Lock-free transport
// ---------------------------------------------------------------------------

/// Single-producer / single-consumer sample queue.
///
/// The SDL callback runs on a real-time thread. It used to lock a
/// `Mutex<VecDeque<i16>>` shared with the emulator thread, which meant that if
/// the emulator held the lock when the callback fired, the audio thread blocked
/// and the buffer underran — priority inversion, and the classic cause of
/// intermittent crackle that never reproduces under a debugger. Draining and
/// releasing quickly narrows that window; it cannot remove it.
///
/// Here the emulator thread owns `write` and the callback owns `read`, and
/// neither ever blocks or allocates. Each slot is an atomic rather than a raw
/// cell, which keeps the whole thing in safe Rust: a relaxed 16-bit load or
/// store is a plain machine load or store, and the release/acquire pair on the
/// indices is what actually publishes the samples.
///
/// Indices count monotonically and are masked on use, so a full ring and an
/// empty one are distinguishable without wasting a slot.
pub struct SpscRing {
    /// Power-of-two number of slots.
    slots: Box<[AtomicI16]>,
    /// Producer-owned. Total samples ever written.
    write: AtomicUsize,
    /// Consumer-owned. Total samples ever read.
    read: AtomicUsize,
    /// Producer-owned. Samples the producer could not fit.
    dropped: AtomicU64,
    /// Consumer-owned. Samples the consumer asked for and the ring did not have.
    starved: AtomicU64,
}

impl SpscRing {
    /// Create a ring holding at least `capacity` samples, rounded up to a power
    /// of two.
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(2).next_power_of_two();
        Self {
            slots: (0..capacity).map(|_| AtomicI16::new(0)).collect(),
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
            starved: AtomicU64::new(0),
        }
    }

    /// Total slots.
    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    /// Samples written but not yet consumed. Readable from either thread; the
    /// emulator uses it to steer against the audio clock.
    pub fn len(&self) -> usize {
        let write = self.write.load(Ordering::Acquire);
        let read = self.read.load(Ordering::Acquire);
        write.wrapping_sub(read)
    }

    /// Samples the producer could not fit. Non-zero means the emulator is
    /// outrunning the sound card.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Samples the consumer asked for and the ring did not have. Non-zero means
    /// the sound card is outrunning the emulator — an underrun, heard as the
    /// callback holding its last sample.
    pub fn starved(&self) -> u64 {
        self.starved.load(Ordering::Relaxed)
    }

    /// Append as many of `samples` as fit, oldest first. Returns how many were
    /// taken.
    ///
    /// A short write means the ring is full. The remainder is counted in
    /// [`dropped`](Self::dropped), which suits the emulator — it produces one
    /// frame's audio per frame and has nowhere to park a leftover. A caller
    /// that intends to retry instead will see its retried samples counted as
    /// dropped.
    ///
    /// Producer thread only.
    pub fn push_slice(&self, samples: &[i16]) -> usize {
        let write = self.write.load(Ordering::Relaxed); // we are the only writer
        let read = self.read.load(Ordering::Acquire);
        let free = self.capacity() - write.wrapping_sub(read);
        let n = samples.len().min(free);

        let mask = self.capacity() - 1;
        for (i, &sample) in samples[..n].iter().enumerate() {
            self.slots[write.wrapping_add(i) & mask].store(sample, Ordering::Relaxed);
        }
        // Release: everything stored above is visible before the new index is.
        self.write.store(write.wrapping_add(n), Ordering::Release);

        if n < samples.len() {
            let lost = (samples.len() - n) as u64;
            self.dropped.store(
                self.dropped.load(Ordering::Relaxed) + lost,
                Ordering::Relaxed,
            );
        }
        n
    }

    /// Take up to `out.len()` samples, oldest first. Returns how many were
    /// written to the front of `out`.
    ///
    /// Consumer thread only. No lock, no allocation, no syscall.
    pub fn pop_slice(&self, out: &mut [i16]) -> usize {
        let read = self.read.load(Ordering::Relaxed); // we are the only reader
        let write = self.write.load(Ordering::Acquire);
        let n = out.len().min(write.wrapping_sub(read));

        let mask = self.capacity() - 1;
        for (i, slot) in out[..n].iter_mut().enumerate() {
            *slot = self.slots[read.wrapping_add(i) & mask].load(Ordering::Relaxed);
        }
        // Release: the producer may reuse these slots once it sees the index.
        self.read.store(read.wrapping_add(n), Ordering::Release);

        if n < out.len() {
            let short = (out.len() - n) as u64;
            self.starved.store(
                self.starved.load(Ordering::Relaxed) + short,
                Ordering::Relaxed,
            );
        }
        n
    }
}

// ---------------------------------------------------------------------------
// Clock tracking
// ---------------------------------------------------------------------------

/// Proportional gain of the clock-tracking loop.
///
/// Chosen for a time constant of `target / (rate · gain)` — about 9 seconds at
/// 44.1 kHz with a 4096-sample setpoint. This loop exists to cancel crystal
/// drift, which is a constant, not to chase per-frame jitter; a fast loop would
/// modulate the frame rate visibly and buy nothing.
const CLOCK_GAIN: f64 = 0.01;

/// How far the frame period may be stretched or squeezed, as a fraction.
///
/// Real crystals differ by tens of ppm, so 0.5% is roughly a hundred times the
/// authority the loop needs — ample headroom — while being far below the point
/// where a speed change is noticeable.
const MAX_TRIM: f64 = 0.005;

/// Multiplier for the emulator's frame period that steers audio production
/// toward the rate the sound card consumes at.
///
/// Video is paced off the host monotonic clock; audio is consumed off the sound
/// card's crystal. Those differ by tens of ppm, and with nothing reconciling
/// them the ring either fills — dropping samples — or drains, holding the last
/// sample. Both are audible, and both recur on a period set by drift rate
/// rather than by anything happening in the game.
///
/// The ring's fill level is a direct measurement of the phase between the two
/// clocks, so it is what the loop steers on: above the setpoint the emulator is
/// ahead and its frames are stretched; below, they are shortened. Returns a
/// number near 1.0.
///
/// Note this trims the *frame period*, not the resampler's output rate. Both
/// close the loop, but a machine emitting a fixed number of samples per frame
/// slightly more often produces audio faster without altering a single sample —
/// so pitch stays exact and only emulation speed moves, by at most 0.5%.
/// Trimming the resampler instead would spread the same audio over more
/// samples and detune the machine by up to 8.6 cents, and would need every
/// device's resampler retuned in step.
pub fn frame_pace_trim(fill: usize, capacity: usize) -> f64 {
    let target = (capacity / 2) as f64;
    let error = (fill as f64 - target) / target;
    1.0 + (CLOCK_GAIN * error).clamp(-MAX_TRIM, MAX_TRIM)
}

pub(crate) struct AudioPlayer {
    ring: AudioRing,
    /// Last sample value — held on underrun to avoid pops.
    last_sample: i16,
    fade_in_pos: u32,
    /// [`FADE_FRAMES`] scaled by the channel count, so the fade lasts the same
    /// time on a stereo machine as on a mono one.
    fade_samples: u32,
    fading_out: Arc<AtomicBool>,
    fade_out_pos: u32,
}

impl AudioCallback for AudioPlayer {
    type Channel = i16;
    fn callback(&mut self, out: &mut [i16]) {
        // Straight into the output buffer: no lock, no allocation, no scratch.
        let available = self.ring.pop_slice(out);

        for (i, sample) in out.iter_mut().enumerate() {
            let raw = if i < available {
                self.last_sample = *sample;
                *sample
            } else {
                // Underrun: hold last sample instead of jumping to zero
                self.last_sample
            };

            if self.fade_in_pos < self.fade_samples {
                let gain = self.fade_in_pos as f32 / self.fade_samples as f32;
                *sample = (raw as f32 * gain) as i16;
                self.fade_in_pos += 1;
            } else if self.fading_out.load(Ordering::Relaxed) {
                if self.fade_out_pos < self.fade_samples {
                    let gain = 1.0 - (self.fade_out_pos as f32 / self.fade_samples as f32);
                    *sample = (raw as f32 * gain) as i16;
                    self.fade_out_pos += 1;
                } else {
                    *sample = 0;
                }
            } else {
                *sample = raw;
            }
        }
    }
}

/// Shared audio ring buffer. The emulator thread pushes samples in;
/// the SDL audio callback thread pops them out.
pub type AudioRing = Arc<SpscRing>;

/// Handle for signalling the audio callback to fade out before shutdown.
pub type FadeOut = Arc<AtomicBool>;

/// A callback that produces nothing, for the rate probe below.
struct Silence;

impl AudioCallback for Silence {
    type Channel = i16;
    fn callback(&mut self, out: &mut [i16]) {
        out.fill(0);
    }
}

/// Ask the audio device what output rate it will actually grant.
///
/// Every sound chip has to resample to the host's clock, and the devices read
/// that rate when they construct — so it has to be known before the machine is
/// built, which is before the real playback device is opened. SDL offers no way
/// to query the rate without opening a device, so this opens one, reads the
/// spec it was granted, and closes it again. The device is never unpaused, so
/// nothing is heard.
///
/// Falls back to `preferred` if the device cannot be opened at all; the caller
/// will fail on the real open a moment later and report it properly there.
pub fn granted_output_rate(sdl_audio: &sdl2::AudioSubsystem, preferred: u32) -> u32 {
    let desired = AudioSpecDesired {
        freq: Some(preferred as i32),
        channels: Some(1),
        samples: Some(1024),
    };
    match sdl_audio.open_playback(None, &desired, |_| Silence) {
        Ok(probe) => probe.spec().freq as u32,
        Err(_) => preferred,
    }
}

/// Initialize SDL2 audio playback.
///
/// Returns the audio device (must be kept alive), a shared ring buffer
/// for feeding samples, and a fade-out signal for clean shutdown.
///
/// If `sample_rate` is 0, returns `None` (machine has no audio).
///
/// `sample_rate` is expected to be the rate [`granted_output_rate`] already
/// negotiated, so the device opens at exactly what the machine is producing.
/// `channels` is the machine's [`audio_channels`], 1 for almost every board and
/// 2 for the one that is genuinely stereo. The transport below carries a flat
/// stream of samples either way, because that is what SDL wants: a stereo
/// machine writes interleaved frames and the callback hands them straight on.
///
/// [`audio_channels`]: phosphor_core::core::machine::AudioSource::audio_channels
pub fn init(
    sdl_audio: &sdl2::AudioSubsystem,
    sample_rate: u32,
    channels: u32,
) -> Option<(AudioDevice<AudioPlayer>, AudioRing, FadeOut)> {
    if sample_rate == 0 {
        return None;
    }

    let channels = channels.clamp(1, 2);
    let ring: AudioRing = Arc::new(SpscRing::new(RING_CAPACITY_FRAMES * channels as usize));
    let fade_out: FadeOut = Arc::new(AtomicBool::new(false));

    let desired_spec = AudioSpecDesired {
        freq: Some(sample_rate as i32),
        channels: Some(channels as u8),
        samples: Some(1024), // ~23.2 ms at 44100 Hz
    };

    let fade_samples = FADE_FRAMES * channels;
    let device = sdl_audio
        .open_playback(None, &desired_spec, |_| AudioPlayer {
            ring: Arc::clone(&ring),
            last_sample: 0,
            fade_in_pos: 0,
            fade_samples,
            fading_out: Arc::clone(&fade_out),
            fade_out_pos: 0,
        })
        .expect("Failed to open SDL audio device");

    // The machine's chips were built against `sample_rate`; if the device came
    // back on a different one anyway, everything plays off-pitch. That should
    // not happen — the rate was negotiated from this same device — so say so
    // rather than drifting silently.
    let granted = device.spec().freq as u32;
    if granted != sample_rate {
        eprintln!(
            "Warning: audio device opened at {granted} Hz but the machine was \
             built for {sample_rate} Hz; playback will be off-pitch by \
             {:.1}%.",
            (granted as f64 / sample_rate as f64 - 1.0) * 100.0
        );
    }

    // Device starts paused; the emulator loop resumes it after the first
    // frame of audio has been buffered.
    Some((device, ring, fade_out))
}

/// Duration to sleep after signalling fade-out, allowing the callback
/// to ramp down before the device is paused.
pub fn fade_out_duration() -> std::time::Duration {
    // FADE_FRAMES at 44100 Hz ≈ 5.8 ms; round up to 10 ms for safety.
    std::time::Duration::from_millis(10)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capacity_rounds_up_to_a_power_of_two() {
        assert_eq!(SpscRing::new(1000).capacity(), 1024);
        assert_eq!(SpscRing::new(8192).capacity(), 8192);
    }

    /// The transport must hold the same *duration* whatever the channel count.
    ///
    /// The ring is sized in frames and allocated in samples, and the prefill
    /// that keeps the callback from starving waits for half of it. Sizing it in
    /// samples instead halves that margin for a stereo machine, which is a
    /// jitter margin quietly disappearing rather than a visible failure.
    #[test]
    fn the_ring_holds_the_same_duration_in_mono_and_stereo() {
        let ms = |channels: usize| {
            let ring = SpscRing::new(RING_CAPACITY_FRAMES * channels);
            (ring.capacity() / channels) as f64 * 1000.0 / 44_100.0
        };
        assert!((ms(1) - ms(2)).abs() < 1e-9, "{} vs {}", ms(1), ms(2));
        assert!((ms(1) - 185.8).abs() < 0.5, "{} ms", ms(1));
    }

    #[test]
    fn samples_come_back_in_order() {
        let ring = SpscRing::new(8);
        assert_eq!(ring.push_slice(&[1, 2, 3]), 3);
        assert_eq!(ring.len(), 3);

        let mut out = [0i16; 2];
        assert_eq!(ring.pop_slice(&mut out), 2);
        assert_eq!(out, [1, 2]);

        let mut rest = [0i16; 8];
        assert_eq!(ring.pop_slice(&mut rest), 1);
        assert_eq!(rest[0], 3);
        assert_eq!(ring.pop_slice(&mut rest), 0);
        assert!(ring.len() == 0);
    }

    #[test]
    fn a_full_ring_holds_capacity_and_counts_what_it_turned_away() {
        let ring = SpscRing::new(4);
        assert_eq!(ring.push_slice(&[1, 2, 3, 4, 5, 6]), 4);
        assert_eq!(ring.len(), 4, "every slot is usable");
        assert_eq!(ring.dropped(), 2);

        // Draining makes room again.
        let mut out = [0i16; 2];
        ring.pop_slice(&mut out);
        assert_eq!(ring.push_slice(&[7, 8]), 2);

        let mut all = [0i16; 8];
        assert_eq!(ring.pop_slice(&mut all), 4);
        assert_eq!(&all[..4], &[3, 4, 7, 8]);
    }

    #[test]
    fn indices_wrap_the_backing_store_without_reordering() {
        // Push and drain far past capacity so the masked indices wrap many
        // times; every sample must still arrive exactly once, in order.
        let ring = SpscRing::new(8);
        let mut expected = 0i16;
        for round in 0..100i16 {
            let batch: Vec<i16> = (0..5).map(|i| round * 5 + i).collect();
            assert_eq!(ring.push_slice(&batch), 5, "round {round} should fit");

            let mut out = [0i16; 5];
            assert_eq!(ring.pop_slice(&mut out), 5);
            for got in out {
                assert_eq!(got, expected);
                expected += 1;
            }
        }
    }

    #[test]
    fn the_trim_is_neutral_at_the_setpoint_and_signed_correctly() {
        let cap = 8192;
        assert_eq!(frame_pace_trim(cap / 2, cap), 1.0);
        assert!(
            frame_pace_trim(cap * 3 / 4, cap) > 1.0,
            "a full ring means the emulator is ahead, so frames get longer"
        );
        assert!(
            frame_pace_trim(cap / 4, cap) < 1.0,
            "a draining ring means the emulator is behind, so frames get shorter"
        );
    }

    #[test]
    fn the_trim_never_exceeds_its_authority() {
        let cap = 8192;
        for fill in [0, 1, cap / 8, cap - 1, cap] {
            let trim = frame_pace_trim(fill, cap);
            assert!(
                (trim - 1.0).abs() <= MAX_TRIM + f64::EPSILON,
                "fill {fill} produced {trim}, beyond the ±{MAX_TRIM} authority"
            );
        }
    }

    #[test]
    fn the_clock_loop_settles_against_a_mismatched_consumer() {
        // The drift test: run the loop against a sound card whose crystal is
        // deliberately far worse than any real one and assert it holds — never
        // filling (which drops samples) and never emptying (which underruns).
        const CAPACITY: usize = 8192;
        const RATE: f64 = 44_100.0;
        const FRAME_HZ: f64 = 60.0;
        // 200 ppm fast. Real crystals are tens of ppm.
        const CONSUMER_RATE: f64 = RATE * 1.0002;

        let samples_per_frame = RATE / FRAME_HZ;
        let nominal_period = 1.0 / FRAME_HZ;

        let mut fill = (CAPACITY / 2) as f64;
        let (mut low, mut high) = (f64::MAX, f64::MIN);

        // Twenty minutes of frames; the first minute is settling time and is
        // not held against the loop.
        for frame in 0..(20 * 60 * FRAME_HZ as usize) {
            let period = nominal_period * frame_pace_trim(fill as usize, CAPACITY);
            fill += samples_per_frame;
            fill -= CONSUMER_RATE * period;
            fill = fill.clamp(0.0, CAPACITY as f64);
            if frame > 60 * FRAME_HZ as usize {
                low = low.min(fill);
                high = high.max(fill);
            }
        }

        assert!(low > 0.0, "ring emptied — that is an underrun");
        assert!(
            high < CAPACITY as f64,
            "ring filled — that is a dropped sample"
        );

        // A proportional loop holds a standing offset proportional to the drift
        // it is cancelling. What matters is that the offset is small next to
        // the ring, so both margins survive.
        let offset = (fill - (CAPACITY / 2) as f64).abs();
        assert!(
            offset < CAPACITY as f64 * 0.1,
            "settled {offset:.0} samples off the setpoint, expected well under \
             {}",
            CAPACITY as f64 * 0.1
        );
    }

    #[test]
    fn a_producer_and_a_consumer_on_two_threads_agree() {
        // The property that matters: run both ends concurrently and check that
        // the consumer sees every sample the producer was told it accepted, in
        // order and unmangled. A torn slot or a mis-ordered index shows up as a
        // gap in the sequence.
        const TOTAL: i32 = 200_000;
        let ring = Arc::new(SpscRing::new(64));

        let producer = {
            let ring = Arc::clone(&ring);
            std::thread::spawn(move || {
                let mut next = 0i32;
                let mut accepted = Vec::new();
                let mut retried = 0u64;
                while next < TOTAL {
                    let batch: Vec<i16> = (next..(next + 32).min(TOTAL))
                        .map(|v| (v % 30_000) as i16)
                        .collect();
                    let n = ring.push_slice(&batch);
                    accepted.extend_from_slice(&batch[..n]);
                    retried += (batch.len() - n) as u64;
                    next += n as i32;
                    if n == 0 {
                        std::thread::yield_now();
                    }
                }
                (accepted, retried)
            })
        };

        let consumer = {
            let ring = Arc::clone(&ring);
            std::thread::spawn(move || {
                let mut got = Vec::new();
                let mut buf = [0i16; 48];
                while (got.len() as i32) < TOTAL {
                    let n = ring.pop_slice(&mut buf);
                    if n == 0 {
                        std::thread::yield_now();
                        continue;
                    }
                    got.extend_from_slice(&buf[..n]);
                }
                got
            })
        };

        let (sent, retried) = producer.join().unwrap();
        let got = consumer.join().unwrap();
        assert_eq!(sent.len(), TOTAL as usize);
        assert_eq!(got, sent, "consumer saw a different stream than was sent");
        // This producer retries what it could not fit, so every "dropped"
        // sample was in fact re-sent — which pins the documented semantic:
        // the counter is short writes, not lost audio, and only equals lost
        // audio for a caller that does not retry (the emulator).
        assert_eq!(ring.dropped(), retried);
    }
}
