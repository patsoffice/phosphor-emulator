//! Shared Namco video helpers.
//!
//! Small pieces of video addressing/geometry common to the Namco board families
//! (the Pac-Man board in [`crate::namco_pac`] and the Galaga board in
//! [`crate::namco_galaga`], the latter shared by Galaga/Dig Dug/Xevious). Kept
//! here so the identical logic lives in exactly one place.

/// Map a tile position in the visible 36×28 grid to its byte offset in the
/// 32×32 (0x400-entry) tile/color VRAM.
///
/// The screen is scanned as a 36×28 grid but VRAM is a 32-row layout with the
/// top/bottom two rows folded into the right/left edge columns. The transform is
/// `row += 2; col -= 2`, then: if `col` has bit 5 set (the folded edge columns)
/// the address is `row + ((col & 0x1f) << 5)`, otherwise it is the linear
/// `col + (row << 5)`. Callers clip the returned offset to `< 0x400`.
pub fn namco_tilemap_offset(col: i32, row: i32) -> usize {
    let r = row + 2;
    let c = col - 2;
    if c & 0x20 != 0 {
        (r + ((c & 0x1F) << 5)) as usize
    } else {
        (c + (r << 5)) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_interior_and_folded_edges() {
        // Interior column (col-2 has bit 5 clear): linear col + row*32.
        // col=5,row=5 -> c=3, r=7 -> 3 + 7*32 = 227.
        assert_eq!(namco_tilemap_offset(5, 5), 227);
        // A folded edge column (c & 0x20 set): row + (c & 0x1f)*32.
        // col=1,row=3 -> c=-1 (0xFFFFFFFF, bit5 set), r=5 -> 5 + (0x1f)*32 = 997.
        assert_eq!(namco_tilemap_offset(1, 3), 5 + (0x1F << 5));
    }
}
