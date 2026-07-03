pub mod decode;
pub mod resistor;
pub mod sheet;
pub mod sprite;
pub mod tilemap;

pub use decode::{GfxCache, GfxLayout, decode_gfx};
pub use resistor::{combine_weights, compute_resistor_net, compute_resistor_weights};
pub use sheet::{Sheet, SheetConfig, grayscale_ramp, render_sheet};
pub use tilemap::TilemapConfig;

/// Rotate an RGB24 buffer 90° counter-clockwise.
///
/// Transforms a `src_w × src_h` image into a `src_h × src_w` output.
/// Native pixel `(nx, ny)` maps to output pixel `(src_h - 1 - ny, nx)`.
pub fn rotate_90_ccw(src: &[u8], dst: &mut [u8], src_w: usize, src_h: usize) {
    let dst_w = src_h;
    for ny in 0..src_h {
        for nx in 0..src_w {
            let ox = (src_h - 1) - ny;
            let oy = nx;
            let si = (ny * src_w + nx) * 3;
            let di = (oy * dst_w + ox) * 3;
            dst[di] = src[si];
            dst[di + 1] = src[si + 1];
            dst[di + 2] = src[si + 2];
        }
    }
}

/// Rotate an indexed pixel buffer 90° counter-clockwise, applying an RGB palette.
///
/// Performs the same rotation as `rotate_90_ccw` but converts indexed pixels
/// to RGB24 in a single pass. Each source byte is used as an index into
/// `palette` (masked to `palette.len() - 1`).
pub fn rotate_90_ccw_indexed(
    src: &[u8],
    dst: &mut [u8],
    src_w: usize,
    src_h: usize,
    palette: &[(u8, u8, u8)],
) {
    let dst_w = src_h;
    let mask = palette.len() - 1;
    for ny in 0..src_h {
        let ox = (src_h - 1) - ny;
        for nx in 0..src_w {
            let oy = nx;
            let idx = src[ny * src_w + nx] as usize & mask;
            let (r, g, b) = palette[idx];
            let di = (oy * dst_w + ox) * 3;
            dst[di] = r;
            dst[di + 1] = g;
            dst[di + 2] = b;
        }
    }
}

/// Rotate an indexed pixel buffer 90° counter-clockwise with block tiling.
///
/// Same transformation as `rotate_90_ccw_indexed`, but processes the source
/// in `block_size × block_size` tiles. Within each block, destination writes
/// span only `block_size` rows, keeping the working set in L1 cache.
///
/// Both `src_w` and `src_h` are handled correctly regardless of whether they
/// divide evenly by `block_size`.
pub fn rotate_90_ccw_indexed_blocked(
    src: &[u8],
    dst: &mut [u8],
    src_w: usize,
    src_h: usize,
    palette: &[(u8, u8, u8)],
    block_size: usize,
) {
    let dst_w = src_h;
    let mask = palette.len() - 1;

    for by in (0..src_h).step_by(block_size) {
        let y_end = (by + block_size).min(src_h);
        for bx in (0..src_w).step_by(block_size) {
            let x_end = (bx + block_size).min(src_w);
            for ny in by..y_end {
                let ox = (src_h - 1) - ny;
                let src_row = ny * src_w;
                for nx in bx..x_end {
                    let idx = src[src_row + nx] as usize & mask;
                    let (r, g, b) = palette[idx];
                    let di = (nx * dst_w + ox) * 3;
                    dst[di] = r;
                    dst[di + 1] = g;
                    dst[di + 2] = b;
                }
            }
        }
    }
}

/// Rotate an indexed pixel buffer 270° CW (= 90° CCW), applying an RGB palette.
///
/// Transforms a `src_w × src_h` image into a `src_h × src_w` output.
/// Native pixel `(nx, ny)` maps to output pixel `(ny, src_w - 1 - nx)`.
///
/// This is the opposite direction of `rotate_90_ccw_indexed` and is used
/// for MAME ROT270 games (e.g., Q*Bert on the Gottlieb platform).
pub fn rotate_270_indexed(
    src: &[u8],
    dst: &mut [u8],
    src_w: usize,
    src_h: usize,
    palette: &[(u8, u8, u8)],
) {
    let dst_w = src_h;
    let mask = palette.len() - 1;
    for ny in 0..src_h {
        let src_row = ny * src_w;
        let ox = ny;
        for nx in 0..src_w {
            let oy = src_w - 1 - nx;
            let idx = src[src_row + nx] as usize & mask;
            let (r, g, b) = palette[idx];
            let di = (oy * dst_w + ox) * 3;
            dst[di] = r;
            dst[di + 1] = g;
            dst[di + 2] = b;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocked_rotation_matches_naive_aligned() {
        // 16×16 — divides evenly by block_size=4
        let src_w = 16;
        let src_h = 16;
        let palette: Vec<(u8, u8, u8)> = (0..=255).map(|i| (i, i, i)).collect();
        let src: Vec<u8> = (0..src_w * src_h).map(|i| (i & 0xFF) as u8).collect();

        let mut dst_naive = vec![0u8; src_w * src_h * 3];
        let mut dst_blocked = vec![0u8; src_w * src_h * 3];

        rotate_90_ccw_indexed(&src, &mut dst_naive, src_w, src_h, &palette);
        rotate_90_ccw_indexed_blocked(&src, &mut dst_blocked, src_w, src_h, &palette, 4);

        assert_eq!(dst_naive, dst_blocked);
    }

    #[test]
    fn blocked_rotation_matches_naive_unaligned() {
        // 13×7 — does not divide evenly by block_size=4
        let src_w = 13;
        let src_h = 7;
        let palette: Vec<(u8, u8, u8)> = (0..=255).map(|i| (i, i, i)).collect();
        let src: Vec<u8> = (0..src_w * src_h).map(|i| (i & 0xFF) as u8).collect();

        let mut dst_naive = vec![0u8; src_w * src_h * 3];
        let mut dst_blocked = vec![0u8; src_w * src_h * 3];

        rotate_90_ccw_indexed(&src, &mut dst_naive, src_w, src_h, &palette);
        rotate_90_ccw_indexed_blocked(&src, &mut dst_blocked, src_w, src_h, &palette, 4);

        assert_eq!(dst_naive, dst_blocked);
    }

    #[test]
    fn blocked_rotation_matches_mcr2_dimensions() {
        // 512×480 with block_size=16 (actual MCR2 dimensions)
        let src_w = 512;
        let src_h = 480;
        let palette: Vec<(u8, u8, u8)> = (0..=255).map(|i| (i, i / 2, i / 3)).collect();
        let src: Vec<u8> = (0..src_w * src_h).map(|i| (i % 64) as u8).collect();

        let mut dst_naive = vec![0u8; src_w * src_h * 3];
        let mut dst_blocked = vec![0u8; src_w * src_h * 3];

        rotate_90_ccw_indexed(&src, &mut dst_naive, src_w, src_h, &palette);
        rotate_90_ccw_indexed_blocked(&src, &mut dst_blocked, src_w, src_h, &palette, 16);

        assert_eq!(dst_naive, dst_blocked);
    }

    #[test]
    fn blocked_rotation_block_size_1_matches_naive() {
        // block_size=1 should degenerate to the same result
        let src_w = 5;
        let src_h = 3;
        let palette: Vec<(u8, u8, u8)> = (0..=255).map(|i| (i, 255 - i, i / 2)).collect();
        let src: Vec<u8> = (0..src_w * src_h).map(|i| (i * 17) as u8).collect();

        let mut dst_naive = vec![0u8; src_w * src_h * 3];
        let mut dst_blocked = vec![0u8; src_w * src_h * 3];

        rotate_90_ccw_indexed(&src, &mut dst_naive, src_w, src_h, &palette);
        rotate_90_ccw_indexed_blocked(&src, &mut dst_blocked, src_w, src_h, &palette, 1);

        assert_eq!(dst_naive, dst_blocked);
    }
}
