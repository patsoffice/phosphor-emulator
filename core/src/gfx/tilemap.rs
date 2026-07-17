use super::decode::GfxCache;

/// Configuration describing a tilemap's dimensions.
pub struct TilemapConfig {
    /// Number of tile columns.
    pub cols: usize,
    /// Number of tile rows.
    pub rows: usize,
    /// Tile width in pixels (typically 8).
    pub tile_width: usize,
    /// Tile height in pixels (typically 8).
    pub tile_height: usize,
}

/// Per-tile placement info returned by a tilemap's `tile_info_fn`.
///
/// `code` selects the decoded tile in the [`GfxCache`], `attr` is the color /
/// bank attribute handed to the resolver, and `flip_x` / `flip_y` mirror the
/// tile horizontally / vertically before the pixel lookup.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TileInfo {
    pub code: u16,
    pub attr: u8,
    pub flip_x: bool,
    pub flip_y: bool,
}

impl TileInfo {
    /// An unflipped tile — the common case.
    pub fn new(code: u16, attr: u8) -> Self {
        TileInfo {
            code,
            attr,
            flip_x: false,
            flip_y: false,
        }
    }
}

/// Render one scanline of a tilemap into an RGB24 buffer.
///
/// For each tile column that intersects the given scanline, calls
/// `tile_info_fn(col, row)` to get a [`TileInfo`] (code, attribute, and per-tile
/// flip), applies the flip, reads the pre-decoded pixel from `tiles`, and calls
/// `resolve_color_fn(attribute, pixel_value)`. The resolver returns
/// `Some((r, g, b))` for an opaque pixel or `None` to leave the destination
/// untouched (transparency) — this covers both raw-pixel transparency
/// (`pixel == 0`) and LUT-based transparency, since the closure sees both the
/// attribute and the pixel.
///
/// The result is written into `buffer` starting at byte offset
/// `x_offset * 3`. The buffer must be large enough for the full tile row
/// width; callers typically pass a slice already offset to the correct
/// scanline row in their framebuffer.
pub fn render_tilemap_scanline<F, G>(
    config: &TilemapConfig,
    tiles: &GfxCache,
    scanline: usize,
    tile_info_fn: F,
    resolve_color_fn: G,
    buffer: &mut [u8],
    x_offset: usize,
) where
    F: Fn(usize, usize) -> TileInfo,
    G: Fn(u8, u8) -> Option<(u8, u8, u8)>,
{
    let tile_row = scanline / config.tile_height;
    let py = scanline % config.tile_height;

    for tile_col in 0..config.cols {
        let info = tile_info_fn(tile_col, tile_row);
        let screen_x = x_offset + tile_col * config.tile_width;
        let src_py = if info.flip_y {
            config.tile_height - 1 - py
        } else {
            py
        };

        for px in 0..config.tile_width {
            let src_px = if info.flip_x {
                config.tile_width - 1 - px
            } else {
                px
            };
            let pixel_value = tiles.pixel(info.code as usize, src_px, src_py);
            if let Some((r, g, b)) = resolve_color_fn(info.attr, pixel_value) {
                let off = (screen_x + px) * 3;
                buffer[off] = r;
                buffer[off + 1] = g;
                buffer[off + 2] = b;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_single_tile_scanline() {
        // 2x1 tilemap, 4x2 tiles, scanline 0
        let config = TilemapConfig {
            cols: 2,
            rows: 1,
            tile_width: 4,
            tile_height: 2,
        };

        // Build a cache with 2 tiles (4x2 each)
        let mut cache = GfxCache::new(2, 4, 2);
        // Tile 0: all pixels = 1
        for py in 0..2 {
            for px in 0..4 {
                cache.set_pixel(0, px, py, 1);
            }
        }
        // Tile 1: all pixels = 2
        for py in 0..2 {
            for px in 0..4 {
                cache.set_pixel(1, px, py, 2);
            }
        }

        let mut buffer = vec![0u8; 8 * 3]; // 8 pixels wide

        render_tilemap_scanline(
            &config,
            &cache,
            0,                                                // scanline 0
            |col, _row| TileInfo::new(col as u16, col as u8), // tile 0 at col 0, tile 1 at col 1
            |_attr, pv| Some((pv * 80, pv * 80, pv * 80)),    // simple grayscale
            &mut buffer,
            0,
        );

        // First 4 pixels should be tile 0 (pixel value 1 -> RGB 80,80,80)
        for px in 0..4 {
            assert_eq!(buffer[px * 3], 80);
            assert_eq!(buffer[px * 3 + 1], 80);
            assert_eq!(buffer[px * 3 + 2], 80);
        }
        // Next 4 pixels should be tile 1 (pixel value 2 -> RGB 160,160,160)
        for px in 4..8 {
            assert_eq!(buffer[px * 3], 160);
            assert_eq!(buffer[px * 3 + 1], 160);
            assert_eq!(buffer[px * 3 + 2], 160);
        }
    }

    #[test]
    fn transparency_leaves_destination_untouched() {
        // A single 2x1 tile whose left pixel is 0 (transparent) and right is 1.
        let config = TilemapConfig {
            cols: 1,
            rows: 1,
            tile_width: 2,
            tile_height: 1,
        };
        let mut cache = GfxCache::new(1, 2, 1);
        cache.set_pixel(0, 0, 0, 0);
        cache.set_pixel(0, 1, 0, 1);

        // Pre-fill with a sentinel; the transparent pixel must survive.
        let mut buffer = vec![9u8; 2 * 3];
        render_tilemap_scanline(
            &config,
            &cache,
            0,
            |_c, _r| TileInfo::new(0, 0),
            |_attr, pv| (pv != 0).then_some((pv, pv, pv)), // pixel 0 = transparent
            &mut buffer,
            0,
        );
        assert_eq!(&buffer[0..3], &[9, 9, 9]); // untouched (transparent)
        assert_eq!(&buffer[3..6], &[1, 1, 1]); // opaque
    }

    #[test]
    fn per_tile_flip_mirrors_lookup() {
        // One 4x2 tile with a unique value per pixel so flips are observable.
        let config = TilemapConfig {
            cols: 1,
            rows: 1,
            tile_width: 4,
            tile_height: 2,
        };
        let mut cache = GfxCache::new(1, 4, 2);
        for py in 0..2 {
            for px in 0..4 {
                cache.set_pixel(0, px, py, (py * 4 + px + 1) as u8);
            }
        }
        let render = |flip_x: bool, flip_y: bool, scanline: usize| -> Vec<u8> {
            let mut buf = vec![0u8; 4 * 3];
            render_tilemap_scanline(
                &config,
                &cache,
                scanline,
                |_c, _r| TileInfo {
                    code: 0,
                    attr: 0,
                    flip_x,
                    flip_y,
                },
                |_attr, pv| Some((pv, pv, pv)),
                &mut buf,
                0,
            );
            (0..4).map(|px| buf[px * 3]).collect()
        };
        // No flip, row 0: values 1,2,3,4.
        assert_eq!(render(false, false, 0), vec![1, 2, 3, 4]);
        // flip_x reverses within the row: 4,3,2,1.
        assert_eq!(render(true, false, 0), vec![4, 3, 2, 1]);
        // flip_y swaps rows: scanline 0 now reads source row 1 (5,6,7,8).
        assert_eq!(render(false, true, 0), vec![5, 6, 7, 8]);
        // Both flips: source row 1 reversed (8,7,6,5).
        assert_eq!(render(true, true, 0), vec![8, 7, 6, 5]);
    }
}
