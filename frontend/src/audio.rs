use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI16, AtomicU64, AtomicUsize, Ordering};

use sdl2::audio::{AudioCallback, AudioDevice, AudioSpecDesired};

/// Number of samples over which to fade in/out (~5.8 ms at 44.1 kHz).
const FADE_SAMPLES: u32 = 256;

/// Capacity of the transport ring, in samples — about 186 ms at 44.1 kHz.
const RING_CAPACITY: usize = 8192;

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
        n
    }
}

pub(crate) struct AudioPlayer {
    ring: AudioRing,
    /// Last sample value — held on underrun to avoid pops.
    last_sample: i16,
    fade_in_pos: u32,
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

            if self.fade_in_pos < FADE_SAMPLES {
                let gain = self.fade_in_pos as f32 / FADE_SAMPLES as f32;
                *sample = (raw as f32 * gain) as i16;
                self.fade_in_pos += 1;
            } else if self.fading_out.load(Ordering::Relaxed) {
                if self.fade_out_pos < FADE_SAMPLES {
                    let gain = 1.0 - (self.fade_out_pos as f32 / FADE_SAMPLES as f32);
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
pub fn init(
    sdl_audio: &sdl2::AudioSubsystem,
    sample_rate: u32,
) -> Option<(AudioDevice<AudioPlayer>, AudioRing, FadeOut)> {
    if sample_rate == 0 {
        return None;
    }

    let ring: AudioRing = Arc::new(SpscRing::new(RING_CAPACITY));
    let fade_out: FadeOut = Arc::new(AtomicBool::new(false));

    let desired_spec = AudioSpecDesired {
        freq: Some(sample_rate as i32),
        channels: Some(1),
        samples: Some(1024), // ~23.2 ms at 44100 Hz
    };

    let device = sdl_audio
        .open_playback(None, &desired_spec, |_| AudioPlayer {
            ring: Arc::clone(&ring),
            last_sample: 0,
            fade_in_pos: 0,
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
    // FADE_SAMPLES at 44100 Hz ≈ 5.8 ms; round up to 10 ms for safety.
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
