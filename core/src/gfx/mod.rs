//! Shared graphics helpers: tile/sprite decode, tilemap and sprite row passes,
//! palette DAC models and screen orientation.
//!
//! # Helpers land with their first caller
//!
//! Two helpers here were once built on spec, as the RGB or generic sibling of
//! one that was actually needed, and neither ever acquired a caller:
//! `tilemap::render_scrolled_tilemap_scanline` and
//! `resistor::compute_resistor_net`. Both are now deleted, and each module
//! records why in its own docs.
//!
//! An unused helper is worse than no helper. It is tested, so it looks
//! exercised; it is documented, so it looks adopted; and the next person
//! building a sibling reads it as precedent. That is how the second one got
//! built. **Add a helper in the same commit as the code that calls it.**

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
    DARLINGTON_BIAS_R, DARLINGTON_RESISTORS, EMITTER_BIAS_R, EMITTER_RESISTORS, combine_weights,
    compute_resistor_weights, compute_resnet_weights, compute_ttl_dac_channel,
    normalize_palette_per_channel, pal_nbit,
};
pub use sheet::{Sheet, SheetConfig, grayscale_ramp, render_sheet};
pub use sprite::{SpriteClip, draw_sprite_row, draw_sprite_row_indexed};
pub use tilemap::{
    TileInfo, TilemapConfig, render_scrolled_tilemap_scanline_indexed, render_tilemap_scanline,
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
/// constants match the legacy helper and the obvious mappings: `ROT90` ==
/// [`rotate_90_ccw`], `ROT270` sends native `(nx, ny)` to `(ny, src_w - 1 - nx)`,
/// and `ROT180` is a full reverse.
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn apply_rot270_maps_native_pixels_explicitly() {
        // Native (nx, ny) lands at (ny, w - 1 - nx) in the h-wide output.
        let (w, h) = (3usize, 2usize);
        let src = tagged_rgb(w, h);
        let mut expected = vec![0u8; w * h * 3];
        for ny in 0..h {
            for nx in 0..w {
                let s = (ny * w + nx) * 3;
                let d = ((w - 1 - nx) * h + ny) * 3;
                expected[d..d + 3].copy_from_slice(&src[s..s + 3]);
            }
        }
        let mut actual = vec![0u8; w * h * 3];
        apply_orientation(&src, &mut actual, w, h, Orientation::ROT270);
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
}
