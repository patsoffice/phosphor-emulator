//! Resolving an indexed framebuffer to RGB24.
//!
//! Boards that composite into a palette-*index* buffer and only look up colour
//! at the very end share this final pass. Keeping it here rather than in each
//! board is what lets the palette become per-scanline on one board without the
//! others' copy of the loop drifting away from it.

/// Resolve an indexed pixel buffer into RGB24, one row at a time.
///
/// `indices` is `width * rows` palette indices in row-major order, and `buffer`
/// receives `indices.len() * 3` bytes. `palette_for_row` supplies the palette to
/// resolve row `y` against, which is the whole point of the row-outer shape: the
/// hardware looks a pixel up as the beam passes it, so a palette write partway
/// down the screen affects only the rows below it.
///
/// The index is masked with `palette.len() - 1`, so a palette whose length is
/// not a power of two will alias. Every caller so far has 16 or 64 entries.
pub fn resolve_indexed_rows<'p, F>(
    indices: &[u8],
    width: usize,
    palette_for_row: F,
    buffer: &mut [u8],
) where
    F: Fn(usize) -> &'p [(u8, u8, u8)],
{
    debug_assert!(width > 0 && indices.len().is_multiple_of(width));
    for (y, row) in indices.chunks_exact(width).enumerate() {
        let palette = palette_for_row(y);
        let mask = palette.len() - 1;
        let out = &mut buffer[y * width * 3..(y + 1) * width * 3];
        for (x, &idx) in row.iter().enumerate() {
            let (r, g, b) = palette[idx as usize & mask];
            out[x * 3] = r;
            out[x * 3 + 1] = g;
            out[x * 3 + 2] = b;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: (u8, u8, u8) = (255, 0, 0);
    const GREEN: (u8, u8, u8) = (0, 255, 0);

    fn pal(fill: (u8, u8, u8)) -> [(u8, u8, u8); 2] {
        [(0, 0, 0), fill]
    }

    #[test]
    fn one_palette_for_every_row_matches_a_flat_resolve() {
        let indices = [1u8, 0, 0, 1];
        let palette = pal(RED);
        let mut buf = [0u8; 4 * 3];
        resolve_indexed_rows(&indices, 2, |_| &palette[..], &mut buf);
        assert_eq!(&buf[0..3], &[255, 0, 0]);
        assert_eq!(&buf[3..6], &[0, 0, 0]);
        assert_eq!(&buf[9..12], &[255, 0, 0]);
    }

    /// The reason this helper is row-outer: a per-row palette must actually
    /// reach only its own row.
    #[test]
    fn each_row_resolves_against_its_own_palette() {
        let indices = [1u8, 1, 1, 1, 1, 1]; // 2 wide, 3 rows, all index 1
        let palettes = [pal(RED), pal(GREEN), pal(RED)];
        let mut buf = [0u8; 6 * 3];
        resolve_indexed_rows(&indices, 2, |y| &palettes[y][..], &mut buf);
        assert_eq!(&buf[0..3], &[255, 0, 0], "row 0 is red");
        assert_eq!(&buf[6..9], &[0, 255, 0], "row 1 is green");
        assert_eq!(&buf[12..15], &[255, 0, 0], "row 2 is red again");
    }

    #[test]
    fn the_index_is_masked_into_the_palette() {
        let indices = [3u8]; // out of range for a 2-entry palette
        let palette = pal(RED);
        let mut buf = [0u8; 3];
        resolve_indexed_rows(&indices, 1, |_| &palette[..], &mut buf);
        assert_eq!(&buf[..], &[255, 0, 0], "3 & 1 == 1");
    }
}
