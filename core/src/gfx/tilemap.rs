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

/// Render one scanline of a tilemap into a persistent **indexed** buffer plus a
/// parallel priority buffer.
///
/// The index/priority-buffer sibling of [`render_tilemap_scanline`]: instead of
/// writing RGB, `resolve_index_fn(attr, pixel)` returns `Some((index, priority))`
/// for an opaque pixel — writing the palette index into `index_buf` and the
/// priority into `prio_buf` — or `None` to leave both buffers untouched
/// (transparency). The machine composites the two buffers into RGB in a later
/// pass, applying its own priority rules. Per-tile flip is applied exactly as in
/// [`render_tilemap_scanline`].
#[allow(clippy::too_many_arguments)]
pub fn render_tilemap_scanline_indexed<F, G>(
    config: &TilemapConfig,
    tiles: &GfxCache,
    scanline: usize,
    tile_info_fn: F,
    resolve_index_fn: G,
    index_buf: &mut [u8],
    prio_buf: &mut [u8],
    x_offset: usize,
) where
    F: Fn(usize, usize) -> TileInfo,
    G: Fn(u8, u8) -> Option<(u8, u8)>,
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
            if let Some((index, priority)) = resolve_index_fn(info.attr, pixel_value) {
                let x = screen_x + px;
                index_buf[x] = index;
                prio_buf[x] = priority;
            }
        }
    }
}

/// Render one scanline of a **scrolled** tilemap into a persistent indexed buffer
/// plus a priority buffer.
///
/// Generalizes [`render_tilemap_scanline_indexed`] to a map larger than the
/// viewport with pixel scroll offsets: the fixed-grid helper is the
/// `scroll_x == 0 && scroll_y == 0 && map == viewport` special case. The map is
/// `map_cols × map_rows` tiles of `tile_w × tile_h`; the source pixel for screen
/// column `sx` is `tx = (sx + scroll_x) mod (map_cols·tile_w)` (and likewise
/// `ty` for the row), so it wraps toroidally. The caller computes
/// `scroll_x`/`scroll_y`, including any per-line row-scroll, by passing a
/// different value each scanline. `tile_info_fn(map_col, map_row)` is called
/// once per tile column crossed (it must be a pure function of the map cell).
/// Per-tile flip is applied as in [`render_tilemap_scanline`].
///
/// `resolve_index_fn(attr, pixel)` returns `Some((index, priority))` for an
/// opaque pixel (written to `index_buf`/`prio_buf`) or `None` for transparency.
///
/// There is no RGB sibling of this. One was built on spec and deleted for never
/// acquiring a caller; see the module docs.
#[allow(clippy::too_many_arguments)]
pub fn render_scrolled_tilemap_scanline_indexed<F, G>(
    tiles: &GfxCache,
    map_cols: usize,
    map_rows: usize,
    tile_w: usize,
    tile_h: usize,
    scroll_x: i32,
    scroll_y: i32,
    viewport_w: usize,
    scanline: usize,
    tile_info_fn: F,
    resolve_index_fn: G,
    index_buf: &mut [u8],
    prio_buf: &mut [u8],
    x_offset: usize,
) where
    F: Fn(usize, usize) -> TileInfo,
    G: Fn(u8, u8) -> Option<(u8, u8)>,
{
    let map_w = (map_cols * tile_w) as i32;
    let map_h = (map_rows * tile_h) as i32;
    let ty = (scanline as i32 + scroll_y).rem_euclid(map_h) as usize;
    let map_row = ty / tile_h;
    let py = ty % tile_h;

    let mut cur_col = usize::MAX;
    let mut info = TileInfo::default();
    let mut src_py = py;
    for sx in 0..viewport_w {
        let tx = (sx as i32 + scroll_x).rem_euclid(map_w) as usize;
        let map_col = tx / tile_w;
        if map_col != cur_col {
            info = tile_info_fn(map_col, map_row);
            src_py = if info.flip_y { tile_h - 1 - py } else { py };
            cur_col = map_col;
        }
        let px = tx % tile_w;
        let src_px = if info.flip_x { tile_w - 1 - px } else { px };
        let pixel = tiles.pixel(info.code as usize, src_px, src_py);
        if let Some((index, priority)) = resolve_index_fn(info.attr, pixel) {
            let x = x_offset + sx;
            index_buf[x] = index;
            prio_buf[x] = priority;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn indexed_writes_index_priority_with_transparency() {
        // 2x1 tile: left pixel 0 (transparent), right pixel 1.
        let config = TilemapConfig {
            cols: 1,
            rows: 1,
            tile_width: 2,
            tile_height: 1,
        };
        let mut cache = GfxCache::new(1, 2, 1);
        cache.set_pixel(0, 0, 0, 0);
        cache.set_pixel(0, 1, 0, 1);

        let mut index_buf = vec![9u8; 2]; // sentinel
        let mut prio_buf = vec![0u8; 2];
        render_tilemap_scanline_indexed(
            &config,
            &cache,
            0,
            |_c, _r| TileInfo::new(0, 5),
            |attr, pv| (pv != 0).then_some((0x20 + pv, attr)), // priority = attr
            &mut index_buf,
            &mut prio_buf,
            0,
        );
        // Transparent left pixel untouched.
        assert_eq!(index_buf[0], 9);
        assert_eq!(prio_buf[0], 0);
        // Opaque right pixel: index 0x21, priority = attr (5).
        assert_eq!(index_buf[1], 0x21);
        assert_eq!(prio_buf[1], 5);
    }

    #[test]
    fn indexed_applies_flip() {
        let config = TilemapConfig {
            cols: 1,
            rows: 1,
            tile_width: 4,
            tile_height: 1,
        };
        let mut cache = GfxCache::new(1, 4, 1);
        for px in 0..4 {
            cache.set_pixel(0, px, 0, (px + 1) as u8);
        }
        let mut index_buf = vec![0u8; 4];
        let mut prio_buf = vec![0u8; 4];
        render_tilemap_scanline_indexed(
            &config,
            &cache,
            0,
            |_c, _r| TileInfo {
                code: 0,
                attr: 0,
                flip_x: true,
                flip_y: false,
            },
            |_a, pv| Some((pv, 0)),
            &mut index_buf,
            &mut prio_buf,
            0,
        );
        // flip_x reverses: 4,3,2,1.
        assert_eq!(index_buf, vec![4, 3, 2, 1]);
    }

    /// The scrolled helper at zero scroll with map == viewport must agree with
    /// the fixed-grid one, since that is the special case it generalizes.
    ///
    /// Was written against the RGB pair; rewritten against the indexed pair when
    /// the RGB scrolled helper was deleted for having no callers.
    #[test]
    fn scrolled_matches_fixed_grid_at_zero_scroll() {
        let config = TilemapConfig {
            cols: 2,
            rows: 1,
            tile_width: 4,
            tile_height: 2,
        };
        let mut cache = GfxCache::new(2, 4, 2);
        for t in 0..2 {
            for py in 0..2 {
                for px in 0..4 {
                    cache.set_pixel(t, px, py, (t * 10 + py * 4 + px + 1) as u8);
                }
            }
        }
        let info = |col: usize, _row: usize| TileInfo::new(col as u16, col as u8);
        let resolve = |_a: u8, pv: u8| Some((pv, 0));

        let (mut fixed, mut fixed_prio) = (vec![0u8; 8], vec![0u8; 8]);
        render_tilemap_scanline_indexed(
            &config,
            &cache,
            1,
            info,
            resolve,
            &mut fixed,
            &mut fixed_prio,
            0,
        );
        let (mut scrolled, mut scrolled_prio) = (vec![0u8; 8], vec![0u8; 8]);
        render_scrolled_tilemap_scanline_indexed(
            &cache,
            2,
            1,
            4,
            2,
            0,
            0,
            8,
            1,
            info,
            resolve,
            &mut scrolled,
            &mut scrolled_prio,
            0,
        );
        assert_eq!(fixed, scrolled);
    }

    #[test]
    fn scrolled_offset_and_toroidal_wrap() {
        // A 2-column map of 4-wide tiles (map_w = 8) into an 8-wide viewport.
        // Tile 0 pixels = column index (0,1,2,3); tile 1 pixels = 10+col.
        let mut cache = GfxCache::new(2, 4, 1);
        for px in 0..4 {
            cache.set_pixel(0, px, 0, px as u8);
            cache.set_pixel(1, px, 0, (10 + px) as u8);
        }
        let info = |col: usize, _row: usize| TileInfo::new(col as u16, 0);
        let resolve = |_a: u8, pv: u8| Some((pv, 0));
        let mut buf = vec![0u8; 8];
        let mut prio = vec![0u8; 8];
        // scroll_x = 5: screen x0 -> tx 5 (tile 1, px 1 -> 11), ... wrapping past
        // map_w=8 back to tile 0.
        render_scrolled_tilemap_scanline_indexed(
            &cache, 2, 1, 4, 1, 5, 0, 8, 0, info, resolve, &mut buf, &mut prio, 0,
        );
        // tx for sx 0..8 = 5,6,7,0,1,2,3,4 -> pens 11,12,13,0,1,2,3,10... wait tx=4
        // is tile1 px0 = 10.
        assert_eq!(buf, vec![11, 12, 13, 0, 1, 2, 3, 10]);
    }

    #[test]
    fn scrolled_applies_per_tile_flip_and_transparency() {
        // Single 4-wide tile map; flip_x reverses; pen 0 transparent.
        let mut cache = GfxCache::new(1, 4, 1);
        cache.set_pixel(0, 0, 0, 0); // transparent
        cache.set_pixel(0, 1, 0, 7);
        cache.set_pixel(0, 2, 0, 8);
        cache.set_pixel(0, 3, 0, 9);
        let mut buf = vec![5u8; 4]; // sentinel
        let mut prio = vec![0u8; 4];
        render_scrolled_tilemap_scanline_indexed(
            &cache,
            1,
            1,
            4,
            1,
            0,
            0,
            4,
            0,
            |_c, _r| TileInfo {
                code: 0,
                attr: 0,
                flip_x: true,
                flip_y: false,
            },
            |_a, pv| (pv != 0).then_some((pv, 0)),
            &mut buf,
            &mut prio,
            0,
        );
        // flip_x: source order 3,2,1,0 -> pens 9,8,7,0(transparent -> sentinel 5).
        assert_eq!(buf, vec![9, 8, 7, 5]);
    }

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
