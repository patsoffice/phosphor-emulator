//! Headless capture: run a machine for a fixed number of frames with no window,
//! writing the final framebuffer to a PNG and the produced audio to a WAV. Used
//! for screenshot/audio regression checks and machine bring-up without SDL.
//!
//! An input movie can drive the run, which is what makes this path usable for
//! verifying an audio change against a recorded session: headless is the only
//! way to exercise the frontend's own drain loop without SDL.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use phosphor_core::core::machine::FrontendMachine;

/// Frames captured when neither `--frames` nor a movie says otherwise, about
/// ten seconds. Referenced by the CLI's help text rather than repeated there.
pub const DEFAULT_FRAMES: u32 = 600;

/// How many frames to capture.
///
/// A movie carries its own length, and it is almost always the length wanted: a
/// capture is of a session, not of the session's first ten seconds. So an
/// explicit `--frames` wins, a movie decides when there is no `--frames`, and
/// [`DEFAULT_FRAMES`] covers neither.
///
/// Running past the end of a movie is legal and sometimes wanted, since the
/// machine keeps going and simply receives no further input.
fn resolve_frames(requested: Option<u32>, movie_frames: Option<u32>) -> u32 {
    requested.or(movie_frames).unwrap_or(DEFAULT_FRAMES)
}

/// Run `machine` for `frames` frames, then write `<out>.png` (final frame) and,
/// if the machine produces audio, `<out>.wav` (16-bit PCM at the machine's
/// channel count).
///
/// `movie_path` replays a recorded session instead of running from power-on with
/// no input. `machine_name` and `rom_digest` are only used to check the movie
/// belongs here.
pub fn run(
    machine: &mut dyn FrontendMachine,
    frames: Option<u32>,
    out: &str,
    movie_path: Option<&Path>,
    machine_name: &str,
    rom_digest: [u8; 32],
) {
    // Playback binds before the first frame and resets to power-on, the same as
    // the windowed path does. A bad movie is fatal rather than a warning, for a
    // sharper version of that path's reason: a capture that quietly ran attract
    // mode instead produces a file with the RIGHT SAMPLE COUNT and the wrong
    // contents, which reads as a successful capture and was mistaken for an
    // audio bug once already.
    let mut playback = movie_path.map(|p| {
        let movie =
            crate::movie::load_for_playback(p, machine_name, rom_digest).unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(1);
            });
        let (movie_frames, records) = (movie.header.frames, movie.records.len());
        let playback = crate::movie::MoviePlayback::bind(movie, machine).unwrap_or_else(|e| {
            eprintln!("binding movie {}: {e}", p.display());
            std::process::exit(1);
        });
        println!(
            "headless: replaying {movie_frames} frame(s), {records} record(s) from {}",
            p.display()
        );
        playback
    });

    let movie_frames = playback.as_ref().map(|pb| pb.progress().1);
    let frames = resolve_frames(frames, movie_frames);
    if let Some(total) = movie_frames
        && frames > total
    {
        println!(
            "headless: capturing {frames} frames past a {total}-frame movie; \
             the machine runs on with no further input"
        );
    }

    let (w, h) = machine.display_size();
    let mut audio: Vec<i16> = Vec::new();
    let mut buf = vec![0i16; 8192];

    for _ in 0..frames {
        // Delivered immediately before the frame runs, which is the point the
        // windowed loop delivers at too. Matching it is what makes a headless
        // capture reproduce the session rather than approximate it.
        if let Some(pb) = &mut playback {
            pb.deliver(machine);
        }
        machine.run_frame();
        if let Some(pb) = &mut playback {
            pb.advance_frame();
        }
        loop {
            let n = machine.fill_audio(&mut buf);
            if n == 0 {
                break;
            }
            audio.extend_from_slice(&buf[..n]);
        }
    }

    let mut rgb = vec![0u8; (w * h * 3) as usize];
    machine.render_frame(&mut rgb);

    let png_path = format!("{out}.png");
    match write_png(&rgb, w, h, &png_path) {
        Ok(()) => {
            let lit = rgb.chunks(3).filter(|p| *p != [0, 0, 0]).count();
            println!(
                "headless: wrote {png_path} ({w}x{h}, {lit}/{} lit pixels)",
                w * h
            );
        }
        Err(e) => eprintln!("headless: PNG write failed: {e}"),
    }

    if !audio.is_empty() {
        let rate = machine.audio_sample_rate();
        let channels = machine.audio_channels();
        let wav_path = format!("{out}.wav");
        match write_wav(&audio, rate, channels, &wav_path) {
            Ok(()) => println!(
                "headless: wrote {wav_path} ({} samples @ {rate} Hz)",
                audio.len()
            ),
            Err(e) => eprintln!("headless: WAV write failed: {e}"),
        }
    }
}

fn write_png(rgb24: &[u8], width: u32, height: u32, path: &str) -> io::Result<()> {
    let file = BufWriter::new(File::create(Path::new(path))?);
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(io::Error::other)?;
    writer.write_image_data(rgb24).map_err(io::Error::other)
}

/// Write interleaved 16-bit PCM as a canonical 44-byte-header WAV.
///
/// `channels` comes from the machine. A stereo capture written with a mono
/// header plays at half speed with the channels alternating.
pub(crate) fn write_wav(samples: &[i16], rate: u32, channels: u32, path: &str) -> io::Result<()> {
    let mut f = BufWriter::new(File::create(Path::new(path))?);
    let channels = channels.clamp(1, 2) as u16;
    let block_align = channels * 2;
    let data_bytes = (samples.len() * 2) as u32;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_bytes).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?; // fmt chunk size
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&channels.to_le_bytes())?; // channels
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * block_align as u32).to_le_bytes())?; // byte rate
    f.write_all(&block_align.to_le_bytes())?; // block align
    f.write_all(&16u16.to_le_bytes())?; // bits per sample
    f.write_all(b"data")?;
    f.write_all(&data_bytes.to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule that makes `--headless --movie` capture the session rather than
    /// its first ten seconds. Before this existed the two flags did not combine
    /// at all, and the bug that hid it was that the output looked plausible.
    #[test]
    fn a_movie_sets_the_frame_count_unless_asked_otherwise() {
        // No movie, no request: the documented default.
        assert_eq!(resolve_frames(None, None), DEFAULT_FRAMES);
        // A movie and no request: the whole movie, however long.
        assert_eq!(resolve_frames(None, Some(2309)), 2309);
        // An explicit request wins, both under and over the movie's length.
        assert_eq!(resolve_frames(Some(120), Some(2309)), 120);
        assert_eq!(resolve_frames(Some(3000), Some(2309)), 3000);
        // An explicit request with no movie is honored rather than defaulted.
        assert_eq!(resolve_frames(Some(1), None), 1);
    }

    /// A movie shorter than the default must not be padded up to it, which is
    /// what `max(requested, movie)` would have done. Worth pinning separately:
    /// it is the case where the wrong rule still produces a plausible file.
    ///
    /// The length is derived from the default rather than written as a literal,
    /// so this keeps testing a *short* movie if the default ever moves.
    #[test]
    fn a_short_movie_is_not_padded_to_the_default() {
        let short = DEFAULT_FRAMES / 10;
        assert_eq!(resolve_frames(None, Some(short)), short);
    }
}
