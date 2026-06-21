//! Capture Phosphor's Galaxian discrete sound voices through the same timeline
//! as the MAME reference (`drive_galaxian_sound.lua`), for comparison with
//! `analyze_wav.py --galaxian`.
//!
//!   cargo run -p phosphor-machines --example galaxian_capture
//!   .../python tools/sound-reference/analyze_wav.py --galaxian \
//!       /tmp/galaxian_ref.wav /tmp/galaxian_phosphor.wav

use phosphor_core::device::GalaxianSound;
use std::fs::File;
use std::io::{BufWriter, Write};

const SR: u32 = 44_100;
const CPU_HZ: u64 = 3_072_000;
const CYCLES_PER_FRAME: u64 = 192 * 264; // 50688, ~60.606 Hz

fn set_lfo(s: &mut GalaxianSound, v: u8) {
    for b in 0..4 {
        s.lfo_freq_w(b, (v >> b) & 1);
    }
}

/// Drive the sound registers for time `t` (seconds), matching the MAME Lua
/// driver exactly: one voice per 2 s window.
fn drive(s: &mut GalaxianSound, t: f64) {
    // Baseline: everything off.
    s.pitch_w(0);
    set_lfo(s, 0);
    for line in 0..8 {
        s.sound_w(line, 0);
    }

    if (1.0..3.0).contains(&t) {
        // Background melody: steady pitch, mixer volume on.
        s.pitch_w(0xB0);
        s.sound_w(6, 1); // VOL1
        s.sound_w(7, 1); // VOL2
    } else if (3.0..5.0).contains(&t) {
        // Wolf-whistle: FS1/2/3 swept by the LFO DAC (0->15).
        s.sound_w(0, 1);
        s.sound_w(1, 1);
        s.sound_w(2, 1);
        s.sound_w(6, 1);
        s.sound_w(7, 1);
        let frac = (t - 3.0) / 2.0;
        set_lfo(s, (frac * 15.999) as u8);
    } else if (5.0..7.0).contains(&t) {
        // Fire / shoot: pulse FIRE to re-trigger.
        s.sound_w(5, if (t - 5.0) % 0.6 < 0.1 { 1 } else { 0 });
    } else if (7.0..9.0).contains(&t) {
        // Hit / explosion: pulse HIT.
        s.sound_w(3, if (t - 7.0) % 0.6 < 0.1 { 1 } else { 0 });
    }
}

fn main() {
    let mut s = GalaxianSound::new(SR);
    let frame_hz = CPU_HZ as f64 / CYCLES_PER_FRAME as f64;
    let frames = (10.0 * frame_hz) as usize;
    let mut samples: Vec<i16> = Vec::with_capacity(10 * SR as usize);
    let mut buf = vec![0i16; 2048];
    for f in 0..frames {
        drive(&mut s, f as f64 / frame_hz);
        s.tick(CYCLES_PER_FRAME);
        loop {
            let n = s.fill_audio(&mut buf);
            samples.extend_from_slice(&buf[..n]);
            if n < buf.len() {
                break;
            }
        }
    }

    let path = "/tmp/galaxian_phosphor.wav";
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
