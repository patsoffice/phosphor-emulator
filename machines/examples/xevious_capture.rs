//! Render a Xevious frame to a PNG for visual comparison against MAME.
//!
//!   cargo run -p phosphor-machines --example xevious_capture -- <roms-dir> [frames] [out.png]
//! where <roms-dir> holds the extracted `xevious` ROM files (unzip xevious.zip).

use std::fs::File;
use std::io::BufWriter;
use std::path::Path;

use phosphor_core::core::machine::{MachineCore, Renderable};
use phosphor_machines::rom_loader::RomSet;
use phosphor_machines::xevious::XeviousSystem;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/xevious".to_string());
    let frames: u32 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000);
    let out = std::env::args()
        .nth(3)
        .unwrap_or_else(|| "/tmp/xevious.png".to_string());

    let rom_set = RomSet::from_directory(Path::new(&dir)).expect("load rom set");
    let mut sys = XeviousSystem::new();
    sys.load_rom_set(&rom_set).expect("load roms");
    sys.reset();

    // With XEV_PLAY set, insert a coin and press start after boot so the frame
    // reaches actual gameplay (scrolling terrain) instead of the attract title.
    let play = std::env::var("XEV_PLAY").is_ok();
    use phosphor_core::core::machine::{InputConfigurable, InputEvent, InputId};
    let press = |sys: &mut XeviousSystem, id: u8, pressed: bool| {
        sys.handle_input(InputEvent::Button {
            id: InputId(id as u16),
            pressed,
        });
    };
    for f in 0..frames {
        if play {
            match f {
                800 => press(&mut sys, 12, true),  // COIN1 down
                810 => press(&mut sys, 12, false), // COIN1 up
                840 => press(&mut sys, 10, true),  // START1 down
                850 => press(&mut sys, 10, false), // START1 up
                _ => {}
            }
        }
        sys.run_frame();
    }

    let (w, h) = sys.display_size();
    let mut buf = vec![0u8; (w * h * 3) as usize];
    sys.render_frame(&mut buf);

    let file = BufWriter::new(File::create(&out).expect("create png"));
    let mut enc = png::Encoder::new(file, w, h);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()
        .expect("png header")
        .write_image_data(&buf)
        .expect("png data");
    eprintln!("wrote {w}x{h} frame (after {frames} frames) to {out}");
}
