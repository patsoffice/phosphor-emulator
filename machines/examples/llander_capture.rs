//! Capture Phosphor's Lunar Lander discrete sound through the same timeline as
//! the MAME reference (`tools/sound-reference/drive_llander_sound.lua`), writing
//! a mono 16-bit WAV for comparison with `analyze_wav.py --llander`.
//!
//!   cargo run -p phosphor-machines --example llander_capture
//!   .../python tools/sound-reference/analyze_wav.py --llander /tmp/llander_phosphor.wav

use phosphor_machines::atari_dvg::TIMING;
use phosphor_machines::llander_sound::LunarLanderDiscreteSound;
use std::fs::File;
use std::io::{BufWriter, Write};

/// 0x3C00 register value for the effect active at emulated time `t` seconds.
/// Matches the segments in the MAME Lua driver.
fn register_at(t: f64) -> u8 {
    if (1.0..3.0).contains(&t) {
        0x07 // thrust full
    } else if (3.0..5.0).contains(&t) {
        0x02 // thrust low
    } else if (5.0..7.0).contains(&t) {
        0x10 // 3 kHz tone
    } else if (7.0..9.0).contains(&t) {
        0x20 // 6 kHz tone
    } else if (9.0..11.0).contains(&t) {
        0x0f // thrust 7 + explosion
    } else {
        0x00
    }
}

fn main() {
    let mut s = LunarLanderDiscreteSound::new();
    let sr = s.sample_rate();
    let cycles_per_frame = TIMING.cycles_per_frame();
    let frames = 12 * 60;

    let mut samples: Vec<i16> = Vec::new();
    let mut buf = vec![0i16; 8192];
    for f in 0..frames {
        s.write_sound_register(register_at(f as f64 / 60.0));
        s.tick(cycles_per_frame);
        loop {
            let n = s.fill_audio(&mut buf);
            if n == 0 {
                break;
            }
            samples.extend_from_slice(&buf[..n]);
        }
    }

    let path = "/tmp/llander_phosphor.wav";
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
