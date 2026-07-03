//! PNG export for the `gfxview` subcommand.
//!
//! The sheet compositing itself (grid layout, integer scale, palette mapping)
//! lives in [`phosphor_core::gfx::sheet`] so the frontend's interactive viewer
//! shares one implementation; this module only adds the disasm-side PNG encode
//! (the `png` crate is not a core dependency).

pub use phosphor_core::gfx::sheet::{SheetConfig, grayscale_ramp, render_sheet};

use std::fs::File;
use std::io::{self, BufWriter};
use std::path::Path;

/// Write an RGB24 buffer as an 8-bit RGB PNG.
///
/// PNG-encode block adapted from `phosphor-frontend`'s `screenshot.rs` (copied
/// rather than adding a frontend → disasm dependency for ~10 lines).
pub fn write_png(path: &Path, rgb24: &[u8], width: u32, height: u32) -> io::Result<()> {
    let file = File::create(path)?;
    let writer = BufWriter::new(file);

    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);

    let mut png_writer = encoder.write_header().map_err(io::Error::other)?;
    png_writer
        .write_image_data(rgb24)
        .map_err(io::Error::other)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::gfx::GfxCache;

    #[test]
    fn write_png_emits_a_valid_png_signature() {
        // Compositing itself is covered in phosphor_core::gfx::sheet; here we
        // only exercise the disasm-side PNG encode end to end.
        let mut cache = GfxCache::new(2, 2, 2);
        for code in 0..2 {
            for py in 0..2 {
                for px in 0..2 {
                    cache.set_pixel(code, px, py, ((code + px + py) & 0x3) as u8);
                }
            }
        }
        let pal = grayscale_ramp(4);
        let sheet = render_sheet(&cache, &pal, &SheetConfig { cols: 2, scale: 2 });

        let path =
            std::env::temp_dir().join(format!("phosphor_gfxsheet_{}.png", std::process::id()));
        write_png(&path, &sheet.rgb, sheet.width, sheet.height).expect("png written");

        let bytes = std::fs::read(&path).expect("read back");
        assert_eq!(&bytes[..8], b"\x89PNG\r\n\x1a\n", "PNG magic");
        let _ = std::fs::remove_file(&path);
    }
}
