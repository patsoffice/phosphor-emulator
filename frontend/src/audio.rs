use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use sdl2::audio::{AudioCallback, AudioDevice, AudioSpecDesired};

/// Number of samples over which to fade in/out (~5.8 ms at 44.1 kHz).
const FADE_SAMPLES: u32 = 256;

pub(crate) struct AudioPlayer {
    buffer: Arc<Mutex<VecDeque<i16>>>,
    /// Scratch space for draining samples under the lock.
    drain: Vec<i16>,
    /// Last sample value — held on underrun to avoid pops.
    last_sample: i16,
    fade_in_pos: u32,
    fading_out: Arc<AtomicBool>,
    fade_out_pos: u32,
}

impl AudioCallback for AudioPlayer {
    type Channel = i16;
    fn callback(&mut self, out: &mut [i16]) {
        // Drain available samples under the lock, then release it.
        let available = {
            let mut buf = self.buffer.lock().unwrap();
            let n = out.len().min(buf.len());
            self.drain.clear();
            self.drain.extend(buf.drain(..n));
            n
        };

        // Process drained samples (lock is released).
        for (i, sample) in out.iter_mut().enumerate() {
            let raw = if i < available {
                let s = self.drain[i];
                self.last_sample = s;
                s
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
pub type AudioRing = Arc<Mutex<VecDeque<i16>>>;

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

    let ring: AudioRing = Arc::new(Mutex::new(VecDeque::with_capacity(8192)));
    let fade_out: FadeOut = Arc::new(AtomicBool::new(false));

    let desired_spec = AudioSpecDesired {
        freq: Some(sample_rate as i32),
        channels: Some(1),
        samples: Some(1024), // ~23.2 ms at 44100 Hz
    };

    let device = sdl_audio
        .open_playback(None, &desired_spec, |spec| AudioPlayer {
            buffer: Arc::clone(&ring),
            drain: Vec::with_capacity(spec.samples as usize),
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
