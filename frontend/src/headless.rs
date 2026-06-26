//! Headless capture: run a machine for a fixed number of frames with no window,
//! writing the final framebuffer to a PNG and the produced audio to a WAV. Used
//! for screenshot/audio regression checks and machine bring-up without SDL.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use phosphor_core::core::machine::FrontendMachine;

/// Run `machine` for `frames` frames, then write `<out>.png` (final frame) and,
/// if the machine produces audio, `<out>.wav` (mono 16-bit PCM).
pub fn run(machine: &mut dyn FrontendMachine, frames: u32, out: &str) {
    let (w, h) = machine.display_size();
    let mut audio: Vec<i16> = Vec::new();
    let mut buf = vec![0i16; 8192];

    for _ in 0..frames {
        machine.run_frame();
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
        let wav_path = format!("{out}.wav");
        match write_wav(&audio, rate, &wav_path) {
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

/// Write mono 16-bit PCM as a canonical 44-byte-header WAV.
pub(crate) fn write_wav(samples: &[i16], rate: u32, path: &str) -> io::Result<()> {
    let mut f = BufWriter::new(File::create(Path::new(path))?);
    let data_bytes = (samples.len() * 2) as u32;
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_bytes).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?; // fmt chunk size
    f.write_all(&1u16.to_le_bytes())?; // PCM
    f.write_all(&1u16.to_le_bytes())?; // mono
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 2).to_le_bytes())?; // byte rate
    f.write_all(&2u16.to_le_bytes())?; // block align
    f.write_all(&16u16.to_le_bytes())?; // bits per sample
    f.write_all(b"data")?;
    f.write_all(&data_bytes.to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    Ok(())
}
