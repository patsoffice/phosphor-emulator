//! Capture Phosphor's Asteroids discrete sound through the same timeline as the
//! MAME reference (`/tmp/drive_asteroid_sound.lua`), writing a mono 16-bit WAV.
//! Lets `analyze_asteroid_wav.py` compare Phosphor vs MAME per-effect pitch.
//!
//!   cargo run -p phosphor-machines --example asteroid_capture
//!   /tmp/sndvenv/bin/python /tmp/analyze_asteroid_wav.py /tmp/phosphor_asteroid.wav

use phosphor_machines::asteroids_sound::AsteroidsDiscreteSound;
use phosphor_machines::atari_dvg::TIMING;
use std::fs::File;
use std::io::{BufWriter, Write};

/// Drive the registers for the effect active at emulated time `t` seconds.
/// Matches the segments in the MAME Lua driver.
fn apply(s: &mut AsteroidsDiscreteSound, t: f64) {
    s.write_explosion(0x00);
    s.write_thump(0x00);
    for line in 0..6u8 {
        s.write_audio_latch_bit(line, false);
    }
    if (1.0..3.0).contains(&t) {
        s.write_audio_latch_bit(3, true); // thrust
    } else if (3.0..5.0).contains(&t) {
        s.write_audio_latch_bit(0, true); // saucer (small)
    } else if (5.0..7.0).contains(&t) {
        s.write_audio_latch_bit(0, true); // saucer (large)
        s.write_audio_latch_bit(2, true);
    } else if (7.0..9.0).contains(&t) {
        s.write_audio_latch_bit(5, true); // life
    } else if (9.0..11.0).contains(&t) {
        s.write_explosion(0x80 | (0x0f << 2)); // divider 3, vol 15
    } else if (11.0..13.0).contains(&t) {
        s.write_thump(0x10 | 0x0f); // enable + max data
    } else if (13.0..15.0).contains(&t) {
        s.write_audio_latch_bit(4, true); // ship fire
    } else if (15.0..17.0).contains(&t) {
        s.write_audio_latch_bit(1, true); // saucer fire
    }
}

fn main() {
    let mut s = AsteroidsDiscreteSound::new();
    let sr = s.sample_rate();
    let cycles_per_frame = TIMING.cycles_per_frame();
    let frames = 18 * 60;

    let mut samples: Vec<i16> = Vec::new();
    let mut buf = vec![0i16; 8192];
    for f in 0..frames {
        apply(&mut s, f as f64 / 60.0);
        s.tick(cycles_per_frame);
        loop {
            let n = s.fill_audio(&mut buf);
            if n == 0 {
                break;
            }
            samples.extend_from_slice(&buf[..n]);
        }
    }

    let path = "/tmp/phosphor_asteroid.wav";
    write_wav(path, sr, &samples);
    eprintln!("wrote {} samples at {sr} Hz to {path}", samples.len());
}

fn write_wav(path: &str, sr: u32, samples: &[i16]) {
    let mut w = BufWriter::new(File::create(path).expect("create wav"));
    let data_len = (samples.len() * 2) as u32;
    let mut put = |bytes: &[u8]| w.write_all(bytes).expect("write wav");
    put(b"RIFF");
    put(&(36 + data_len).to_le_bytes());
    put(b"WAVE");
    put(b"fmt ");
    put(&16u32.to_le_bytes());
    put(&1u16.to_le_bytes()); // PCM
    put(&1u16.to_le_bytes()); // mono
    put(&sr.to_le_bytes());
    put(&(sr * 2).to_le_bytes()); // byte rate
    put(&2u16.to_le_bytes()); // block align
    put(&16u16.to_le_bytes()); // bits per sample
    put(b"data");
    put(&data_len.to_le_bytes());
    for &smp in samples {
        put(&smp.to_le_bytes());
    }
}
