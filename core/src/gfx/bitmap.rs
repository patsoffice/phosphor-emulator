//! Packed linear-framebuffer scanline unpacking.
//!
//! Several machines store the screen as a packed bitmap — N pixels per byte of
//! a small palette index — rather than as tiles. This helper unpacks one row of
//! such a framebuffer into an RGB24 buffer; the caller supplies the packed row
//! (gathering it first if its VRAM is column-major) and a `resolve` closure that
//! maps each pixel value to RGB (capturing its palette or per-line palette
//! latch).

/// Unpack one scanline of a packed linear framebuffer into an RGB24 buffer.
///
/// `src` holds the packed bytes for the row. Each byte packs `pixels_per_byte`
/// pixels of `8 / pixels_per_byte` bits each; `high_first` selects whether the
/// most-significant field is the leftmost pixel. `resolve` maps each unpacked
/// pixel value to an `(r, g, b)` triple. Pixels are written left-to-right
/// starting `x_offset` pixels into `buffer` (i.e. at byte `x_offset * 3`), one
/// per packed field, so `src.len() * pixels_per_byte` pixels are produced.
pub fn render_bitmap_scanline<G>(
    src: &[u8],
    pixels_per_byte: usize,
    high_first: bool,
    resolve: G,
    buffer: &mut [u8],
    x_offset: usize,
) where
    G: Fn(u8) -> (u8, u8, u8),
{
    debug_assert!(
        matches!(pixels_per_byte, 1 | 2 | 4 | 8),
        "pixels_per_byte must divide 8"
    );
    let bits = 8 / pixels_per_byte;
    let mask = ((1u16 << bits) - 1) as u8;
    let mut out_x = x_offset;
    for &byte in src {
        for i in 0..pixels_per_byte {
            // Field index `i` counts from the left; pick its bit position.
            let shift = if high_first {
                (pixels_per_byte - 1 - i) * bits
            } else {
                i * bits
            };
            let value = (byte >> shift) & mask;
            let (r, g, b) = resolve(value);
            let off = out_x * 3;
            buffer[off] = r;
            buffer[off + 1] = g;
            buffer[off + 2] = b;
            out_x += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Grayscale resolver: pixel value n -> (n, n, n).
    fn gray(v: u8) -> (u8, u8, u8) {
        (v, v, v)
    }

    #[test]
    fn two_per_byte_high_first() {
        // 0x12 -> [1, 2]; 0x34 -> [3, 4].
        let mut buf = vec![0u8; 4 * 3];
        render_bitmap_scanline(&[0x12, 0x34], 2, true, gray, &mut buf, 0);
        let got: Vec<u8> = (0..4).map(|p| buf[p * 3]).collect();
        assert_eq!(got, vec![1, 2, 3, 4]);
    }

    #[test]
    fn two_per_byte_low_first() {
        // Low nibble first: 0x12 -> [2, 1]; 0x34 -> [4, 3].
        let mut buf = vec![0u8; 4 * 3];
        render_bitmap_scanline(&[0x12, 0x34], 2, false, gray, &mut buf, 0);
        let got: Vec<u8> = (0..4).map(|p| buf[p * 3]).collect();
        assert_eq!(got, vec![2, 1, 4, 3]);
    }

    #[test]
    fn four_per_byte_high_first() {
        // 2bpp, high field first: 0b11_10_01_00 -> [3, 2, 1, 0].
        let mut buf = vec![0u8; 4 * 3];
        render_bitmap_scanline(&[0b11_10_01_00], 4, true, gray, &mut buf, 0);
        let got: Vec<u8> = (0..4).map(|p| buf[p * 3]).collect();
        assert_eq!(got, vec![3, 2, 1, 0]);
    }

    #[test]
    fn x_offset_and_resolve_are_respected() {
        let mut buf = vec![9u8; 4 * 3]; // sentinel
        // Write 2 pixels at offset 2; the resolver doubles the value.
        render_bitmap_scanline(&[0x35], 2, true, |v| (v * 2, 0, 0), &mut buf, 2);
        assert_eq!(&buf[0..6], &[9, 9, 9, 9, 9, 9]); // untouched
        assert_eq!(buf[6], 6); // pixel value 3 -> 6
        assert_eq!(buf[9], 10); // pixel value 5 -> 10
    }
}
