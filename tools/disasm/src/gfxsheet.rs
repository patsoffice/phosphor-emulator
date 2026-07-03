//! Tile/sprite sheet compositing and PNG export for the `gfxview` subcommand.
//!
//! A decoded [`GfxCache`] holds `count` elements, each a `width × height` grid
//! of palette indices. This module lays those elements out in a column-major
//! grid, colors each index through a palette (or a grayscale ramp for machines
//! with no color PROM), nearest-neighbor upscales by an integer factor, and
//! writes the result as an 8-bit RGB PNG.

use std::fs::File;
use std::io::{self, BufWriter};
use std::path::Path;

use phosphor_core::gfx::GfxCache;

/// Grid layout knobs for [`render_sheet`].
pub struct SheetConfig {
    /// Elements per row in the output grid.
    pub cols: usize,
    /// Integer nearest-neighbor upscale factor (`1` = native resolution).
    pub scale: usize,
}

/// A composited RGB24 sheet, ready to hand to [`write_png`].
pub struct Sheet {
    /// Row-major `width * height * 3` bytes.
    pub rgb: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// Lay a decoded [`GfxCache`] out into an RGB24 grid image.
///
/// Elements fill the grid left-to-right, top-to-bottom in `cfg.cols` columns.
/// Each pixel's palette index selects an RGB color via `palette[idx % len]`
/// (the modulo tolerates a palette smaller than the index space rather than
/// panicking). Cells past `cache.count()` — the ragged tail of the last row —
/// stay black. `palette` must be non-empty.
pub fn render_sheet(cache: &GfxCache, palette: &[(u8, u8, u8)], cfg: &SheetConfig) -> Sheet {
    let cols = cfg.cols.max(1);
    let scale = cfg.scale.max(1);
    let tw = cache.width();
    let th = cache.height();
    let count = cache.count();
    let pal_len = palette.len().max(1);

    let rows = count.div_ceil(cols);
    let img_w = cols * tw * scale;
    let img_h = rows * th * scale;
    let mut rgb = vec![0u8; img_w * img_h * 3];

    for code in 0..count {
        let base_x = (code % cols) * tw * scale;
        let base_y = (code / cols) * th * scale;
        for py in 0..th {
            for px in 0..tw {
                let idx = cache.pixel(code, px, py) as usize;
                let (r, g, b) = palette[idx % pal_len];
                // Replicate this source pixel across a scale × scale block.
                for sy in 0..scale {
                    let oy = base_y + py * scale + sy;
                    let ox = base_x + px * scale;
                    let row_start = (oy * img_w + ox) * 3;
                    for sx in 0..scale {
                        let o = row_start + sx * 3;
                        rgb[o] = r;
                        rgb[o + 1] = g;
                        rgb[o + 2] = b;
                    }
                }
            }
        }
    }

    Sheet {
        rgb,
        width: img_w as u32,
        height: img_h as u32,
    }
}

/// Build an evenly-spaced grayscale palette of `levels` entries (min 2).
///
/// Used as the fallback for regions whose machine has no color PROM: index 0
/// is black, the top index is white, the rest are linearly interpolated.
pub fn grayscale_ramp(levels: usize) -> Vec<(u8, u8, u8)> {
    let n = levels.max(2);
    (0..n)
        .map(|i| {
            let v = (i * 255 / (n - 1)) as u8;
            (v, v, v)
        })
        .collect()
}

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

    /// Build a synthetic cache: element `code`, pixel `(px,py)` → index
    /// `(code + px + py) & mask`, so tests can predict every pixel.
    fn synthetic(count: usize, w: usize, h: usize, mask: u8) -> GfxCache {
        let mut cache = GfxCache::new(count, w, h);
        for code in 0..count {
            for py in 0..h {
                for px in 0..w {
                    let v = ((code + px + py) as u8) & mask;
                    cache.set_pixel(code, px, py, v);
                }
            }
        }
        cache
    }

    fn px(sheet: &Sheet, x: usize, y: usize) -> (u8, u8, u8) {
        let o = (y * sheet.width as usize + x) * 3;
        (sheet.rgb[o], sheet.rgb[o + 1], sheet.rgb[o + 2])
    }

    #[test]
    fn sheet_dimensions_account_for_grid_and_scale() {
        // 5 tiles of 8×8, 2 cols → 3 rows; scale 3.
        let cache = synthetic(5, 8, 8, 0x3);
        let pal = vec![(0, 0, 0); 4];
        let sheet = render_sheet(&cache, &pal, &SheetConfig { cols: 2, scale: 3 });
        assert_eq!(sheet.width, (2 * 8 * 3) as u32);
        assert_eq!(sheet.height, (3 * 8 * 3) as u32);
        assert_eq!(
            sheet.rgb.len(),
            sheet.width as usize * sheet.height as usize * 3
        );
    }

    #[test]
    fn pixels_map_through_palette_at_native_scale() {
        // Two 2×2 tiles side by side (cols=2), scale 1 → 4×2 image.
        let cache = synthetic(2, 2, 2, 0x3);
        let pal = vec![
            (10, 0, 0), // idx 0
            (20, 0, 0), // idx 1
            (30, 0, 0), // idx 2
            (40, 0, 0), // idx 3
        ];
        let sheet = render_sheet(&cache, &pal, &SheetConfig { cols: 2, scale: 1 });
        assert_eq!((sheet.width, sheet.height), (4, 2));

        // Tile 0, pixel (0,0): index (0+0+0)=0 → (10,0,0).
        assert_eq!(px(&sheet, 0, 0), (10, 0, 0));
        // Tile 0, pixel (1,1): index (0+1+1)=2 → (30,0,0).
        assert_eq!(px(&sheet, 1, 1), (30, 0, 0));
        // Tile 1 sits at grid column 1 → sheet x offset 2. Pixel (0,0):
        // index (1+0+0)=1 → (20,0,0).
        assert_eq!(px(&sheet, 2, 0), (20, 0, 0));
    }

    #[test]
    fn scale_replicates_each_pixel_into_a_block() {
        let cache = synthetic(1, 1, 1, 0x1); // single 1×1 tile, index 0
        let pal = vec![(7, 8, 9)];
        let sheet = render_sheet(&cache, &pal, &SheetConfig { cols: 1, scale: 4 });
        assert_eq!((sheet.width, sheet.height), (4, 4));
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(px(&sheet, x, y), (7, 8, 9), "block pixel ({x},{y})");
            }
        }
    }

    #[test]
    fn ragged_tail_cells_stay_black() {
        // 3 tiles in a 2-col grid → last cell (row 1, col 1) is empty/black.
        let cache = synthetic(3, 2, 2, 0x3);
        let pal = vec![(255, 255, 255); 4];
        let sheet = render_sheet(&cache, &pal, &SheetConfig { cols: 2, scale: 1 });
        assert_eq!((sheet.width, sheet.height), (4, 4));
        // Bottom-right 2×2 quadrant is the unused 4th cell.
        assert_eq!(px(&sheet, 3, 3), (0, 0, 0));
        assert_eq!(px(&sheet, 2, 2), (0, 0, 0));
    }

    #[test]
    fn index_wraps_when_palette_smaller_than_index_space() {
        // Index space is 0..4 but palette has 2 entries → idx % 2.
        let cache = synthetic(1, 2, 2, 0x3);
        let pal = vec![(0, 0, 0), (1, 1, 1)];
        let sheet = render_sheet(&cache, &pal, &SheetConfig { cols: 1, scale: 1 });
        // Pixel (1,1): index 2 → 2 % 2 = 0.
        assert_eq!(px(&sheet, 1, 1), (0, 0, 0));
        // Pixel (1,0): index 1 → 1 % 2 = 1.
        assert_eq!(px(&sheet, 1, 0), (1, 1, 1));
    }

    #[test]
    fn grayscale_ramp_spans_black_to_white() {
        let ramp = grayscale_ramp(4);
        assert_eq!(
            ramp,
            vec![(0, 0, 0), (85, 85, 85), (170, 170, 170), (255, 255, 255)]
        );
        // Degenerate levels clamp to a 2-entry black→white ramp.
        assert_eq!(grayscale_ramp(1), vec![(0, 0, 0), (255, 255, 255)]);
    }

    #[test]
    fn write_png_emits_a_valid_png_signature() {
        let cache = synthetic(2, 2, 2, 0x3);
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
