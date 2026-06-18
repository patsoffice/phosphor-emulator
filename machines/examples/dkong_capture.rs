//! Capture Phosphor's Donkey Kong discrete walk/jump/stomp effects through the
//! same timeline as the MAME reference (`drive_dkong_sound.lua`), with the DAC
//! held silent, for comparison with `analyze_wav.py --dkong`.
//!
//!   cargo run -p phosphor-machines --example dkong_capture
//!   .../python tools/sound-reference/analyze_wav.py --dkong /tmp/dkong_phosphor.wav

use phosphor_machines::dkong_sound::DkongDiscreteSound;
use std::fs::File;
use std::io::{BufWriter, Write};

const SR: u32 = 44_100; // DkongDiscreteSound runs at 44.1 kHz

/// Drive the latch bits for the effect active at time `t` (seconds), matching
/// the MAME Lua driver: walk held, jump/stomp pulsed to re-trigger.
fn drive(s: &mut DkongDiscreteSound, t: f64) {
    for b in 0..3 {
        s.write_sound_bit(b, false);
    }
    if (1.0..3.0).contains(&t) {
        s.write_sound_bit(0, true); // walk (hold)
    } else if (3.0..5.0).contains(&t) {
        s.write_sound_bit(1, ((t - 3.0) % 0.5) < 0.05); // jump (pulse)
    } else if (5.0..7.0).contains(&t) {
        s.write_sound_bit(2, ((t - 5.0) % 0.5) < 0.05); // stomp (pulse)
    }
}

fn main() {
    let mut s = DkongDiscreteSound::new();
    let total = 7 * SR as usize;
    let mut samples: Vec<i16> = Vec::with_capacity(total);
    let mut buf = vec![0i16; 64];
    for i in 0..total {
        drive(&mut s, i as f64 / SR as f64);
        s.feed_dac(0); // DAC silent; one step -> one sample
        let n = s.fill_audio(&mut buf);
        samples.extend_from_slice(&buf[..n]);
    }

    let path = "/tmp/dkong_phosphor.wav";
    write_wav(path, SR, &samples);
    eprintln!("wrote {} samples at {SR} Hz to {path}", samples.len());
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
    put(&1u16.to_le_bytes());
    put(&1u16.to_le_bytes());
    put(&sr.to_le_bytes());
    put(&(sr * 2).to_le_bytes());
    put(&2u16.to_le_bytes());
    put(&16u16.to_le_bytes());
    put(b"data");
    put(&data_len.to_le_bytes());
    for &smp in samples {
        put(&smp.to_le_bytes());
    }
}
