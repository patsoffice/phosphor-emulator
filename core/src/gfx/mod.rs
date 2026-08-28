pub mod bitmap;
pub mod decode;
pub mod palette;
pub mod resistor;
pub mod sheet;
pub mod sprite;
pub mod tilemap;

pub use bitmap::render_bitmap_scanline;
pub use decode::{GfxCache, GfxLayout, decode_gfx};
pub use palette::resolve_indexed_rows;
pub use resistor::{
    combine_weights, compute_resistor_net, compute_resistor_weights, compute_resnet_weights,
    pal_nbit,
};
pub use sheet::{Sheet, SheetConfig, grayscale_ramp, render_sheet};
pub use sprite::{SpriteClip, draw_sprite_row, draw_sprite_row_indexed};
pub use tilemap::{
    TileInfo, TilemapConfig, render_scrolled_tilemap_scanline,
    render_scrolled_tilemap_scanline_indexed, render_tilemap_scanline,
    render_tilemap_scanline_indexed,
};

/// MAME-style screen orientation as a composable bitfield.
///
/// Mirrors MAME's `ORIENTATION_*` flags. A machine renders its *native*
/// (unrotated) framebuffer and declares an `Orientation`; the frontend applies
/// the transform centrally via [`apply_orientation`]. Rotation, cocktail flip,
/// and dynamic (DIP-driven) orientation all fold into this one value.
///
/// The three primitive flags compose: a 90° rotation is a transpose plus one
/// mirror. The named `ROT*` constants match the existing `rotate_*` helpers so
/// migrated machines stay pixel-identical (see the anchoring unit tests).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Orientation(u8);

impl Orientation {
    /// Mirror horizontally (swap left ↔ right).
    pub const FLIP_X: u8 = 0x01;
    /// Mirror vertically (swap top ↔ bottom).
    pub const FLIP_Y: u8 = 0x02;
    /// Transpose the X and Y axes (the diagonal part of a 90° rotation).
    pub const SWAP_XY: u8 = 0x04;

    /// No transform; the native framebuffer is presented as-is.
    pub const NORMAL: Orientation = Orientation(0);
    /// Rotate 90° clockwise.
    pub const ROT90: Orientation = Orientation(Self::SWAP_XY | Self::FLIP_X);
    /// Rotate 180°.
    pub const ROT180: Orientation = Orientation(Self::FLIP_X | Self::FLIP_Y);
    /// Rotate 270° clockwise (= 90° counter-clockwise).
    pub const ROT270: Orientation = Orientation(Self::SWAP_XY | Self::FLIP_Y);
    /// Cocktail flip: a 180° rotation for the seated (second) player.
    pub const COCKTAIL: Orientation = Orientation::ROT180;

    /// Construct from raw flag bits (unknown bits are masked off).
    pub const fn from_bits(bits: u8) -> Self {
        Orientation(bits & (Self::FLIP_X | Self::FLIP_Y | Self::SWAP_XY))
    }

    /// The raw flag bits.
    pub const fn bits(self) -> u8 {
        self.0
    }

    /// True if the horizontal axis is mirrored.
    pub const fn flip_x(self) -> bool {
        self.0 & Self::FLIP_X != 0
    }

    /// True if the vertical axis is mirrored.
    pub const fn flip_y(self) -> bool {
        self.0 & Self::FLIP_Y != 0
    }

    /// True if the X and Y axes are transposed.
    pub const fn swap_xy(self) -> bool {
        self.0 & Self::SWAP_XY != 0
    }

    /// Alias of [`swap_xy`](Self::swap_xy). When set, the displayed
    /// width/height are the native height/width — used for dimension swapping
    /// in window/texture sizing.
    pub const fn swaps_axes(self) -> bool {
        self.swap_xy()
    }

    /// Compose with another orientation by XOR-ing their flags, so a base
    /// cabinet orientation can combine with a live cocktail flip (`ROT180`):
    /// e.g. `ROT90.compose(COCKTAIL) == ROT270`.
    pub const fn compose(self, other: Orientation) -> Orientation {
        Orientation(self.0 ^ other.0)
    }
}

/// Apply an [`Orientation`] to an RGB24 buffer.
///
/// Reads the `src_w × src_h` source and writes the transformed image into
/// `dst`. When the orientation swaps axes the destination is `src_h × src_w`;
/// otherwise it keeps the source dimensions. `dst` must hold at least
/// `src_w * src_h * 3` bytes.
///
/// The transform is a transpose (when `SWAP_XY`) followed by the horizontal /
/// vertical mirrors in the transposed space. That ordering makes the named
/// constants match the legacy helpers: `ROT90` == [`rotate_90_ccw`], `ROT270`
/// == [`rotate_270_indexed`] (identity palette), `ROT180` == a full reverse.
pub fn apply_orientation(src: &[u8], dst: &mut [u8], src_w: usize, src_h: usize, o: Orientation) {
    let swap = o.swap_xy();
    let flip_x = o.flip_x();
    let flip_y = o.flip_y();
    // Destination dims equal the (post-transpose) working-space dims.
    let (dst_w, dst_h) = if swap { (src_h, src_w) } else { (src_w, src_h) };
    for ny in 0..src_h {
        for nx in 0..src_w {
            let (sx, sy) = if swap { (ny, nx) } else { (nx, ny) };
            let ox = if flip_x { dst_w - 1 - sx } else { sx };
            let oy = if flip_y { dst_h - 1 - sy } else { sy };
            let si = (ny * src_w + nx) * 3;
            let di = (oy * dst_w + ox) * 3;
            dst[di] = src[si];
            dst[di + 1] = src[si + 1];
            dst[di + 2] = src[si + 2];
        }
    }
}

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

    // A small asymmetric RGB24 image where every pixel is uniquely tagged, so
    // any wrong axis/flip in a transform is caught. Pixel (nx,ny) = (nx+1, ny+1, 0).
    fn tagged_rgb(src_w: usize, src_h: usize) -> Vec<u8> {
        let mut v = vec![0u8; src_w * src_h * 3];
        for ny in 0..src_h {
            for nx in 0..src_w {
                let i = (ny * src_w + nx) * 3;
                v[i] = (nx + 1) as u8;
                v[i + 1] = (ny + 1) as u8;
                v[i + 2] = 0;
            }
        }
        v
    }

    #[test]
    fn orientation_flag_composition() {
        assert_eq!(Orientation::NORMAL.bits(), 0);
        assert!(Orientation::ROT90.swap_xy() && Orientation::ROT90.flip_x());
        assert!(!Orientation::ROT90.flip_y());
        assert!(Orientation::ROT270.swap_xy() && Orientation::ROT270.flip_y());
        assert!(!Orientation::ROT270.flip_x());
        assert_eq!(Orientation::COCKTAIL, Orientation::ROT180);
        assert!(Orientation::ROT180.flip_x() && Orientation::ROT180.flip_y());
        assert!(!Orientation::ROT180.swap_xy());
        // Adding a cocktail (180°) flip advances the rotation by 180°.
        assert_eq!(
            Orientation::ROT90.compose(Orientation::COCKTAIL),
            Orientation::ROT270
        );
        assert_eq!(
            Orientation::ROT270.compose(Orientation::COCKTAIL),
            Orientation::ROT90
        );
        assert_eq!(
            Orientation::NORMAL.compose(Orientation::COCKTAIL),
            Orientation::ROT180
        );
        assert!(Orientation::ROT90.swaps_axes());
        assert!(!Orientation::ROT180.swaps_axes());
    }

    #[test]
    fn apply_normal_is_identity() {
        let (w, h) = (3usize, 2usize);
        let src = tagged_rgb(w, h);
        let mut dst = vec![0u8; w * h * 3];
        apply_orientation(&src, &mut dst, w, h, Orientation::NORMAL);
        assert_eq!(src, dst);
    }

    #[test]
    fn apply_rot90_matches_rotate_90_ccw() {
        let (w, h) = (3usize, 2usize);
        let src = tagged_rgb(w, h);
        let mut expected = vec![0u8; w * h * 3];
        let mut actual = vec![0u8; w * h * 3];
        rotate_90_ccw(&src, &mut expected, w, h);
        apply_orientation(&src, &mut actual, w, h, Orientation::ROT90);
        assert_eq!(expected, actual);
    }

    #[test]
    fn apply_rot270_matches_rotate_270_indexed() {
        let (w, h) = (3usize, 2usize);
        // Identity palette: indexed byte i -> (i,i,i). Build the RGB source to
        // match so apply_orientation (RGB) and rotate_270_indexed agree.
        let palette: Vec<(u8, u8, u8)> = (0..=255).map(|i| (i, i, i)).collect();
        let idx: Vec<u8> = (0..w * h).map(|i| (i * 11 + 1) as u8).collect();
        let src_rgb: Vec<u8> = idx.iter().flat_map(|&i| [i, i, i]).collect();
        let mut expected = vec![0u8; w * h * 3];
        let mut actual = vec![0u8; w * h * 3];
        rotate_270_indexed(&idx, &mut expected, w, h, &palette);
        apply_orientation(&src_rgb, &mut actual, w, h, Orientation::ROT270);
        assert_eq!(expected, actual);
    }

    #[test]
    fn apply_rot180_matches_reverse() {
        let (w, h) = (3usize, 2usize);
        let src = tagged_rgb(w, h);
        // A 180° rotation reverses the pixel sequence.
        let mut expected = vec![0u8; w * h * 3];
        for p in 0..w * h {
            let s = p * 3;
            let d = (w * h - 1 - p) * 3;
            expected[d..d + 3].copy_from_slice(&src[s..s + 3]);
        }
        let mut actual = vec![0u8; w * h * 3];
        apply_orientation(&src, &mut actual, w, h, Orientation::ROT180);
        assert_eq!(expected, actual);
    }

    #[test]
    fn apply_rot90_then_rot270_round_trips() {
        // ROT90 (CW) followed by ROT270 (CCW) restores the original image.
        let (w, h) = (4usize, 3usize);
        let src = tagged_rgb(w, h);
        let mut rot = vec![0u8; w * h * 3];
        apply_orientation(&src, &mut rot, w, h, Orientation::ROT90);
        // The rotated image is h×w; rotate it back.
        let mut back = vec![0u8; w * h * 3];
        apply_orientation(&rot, &mut back, h, w, Orientation::ROT270);
        assert_eq!(src, back);
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
