use super::decode::GfxCache;

/// Clipping and wraparound parameters for sprite drawing.
pub struct SpriteClip {
    /// Minimum visible X (inclusive).
    pub x_min: i32,
    /// Maximum visible X (exclusive).
    pub x_max: i32,
    /// If set, draw a second copy at `sx + offset` for edge wraparound.
    pub wrap_offset: Option<i32>,
}

/// Draw one row of a single sprite into an RGB24 scanline buffer.
///
/// This handles horizontal flip, clipping, transparency, and optional X
/// wraparound for a single sprite on a single scanline. The machine's
/// sprite-iteration loop (which determines priority order, attribute
/// parsing, and position calculation) calls this for each visible sprite.
#[allow(clippy::too_many_arguments)]
pub fn draw_sprite_row<F, G>(
    sprites: &GfxCache,
    code: u16,
    src_py: usize,
    sx: i32,
    flip_x: bool,
    is_transparent: F,
    color_fn: G,
    buffer: &mut [u8],
    clip: &SpriteClip,
) where
    F: Fn(u8) -> bool,
    G: Fn(u8) -> (u8, u8, u8),
{
    let sprite_w = sprites.width();
    draw_row_inner(
        sprites,
        code,
        src_py,
        sx,
        flip_x,
        &is_transparent,
        &color_fn,
        buffer,
        clip,
        sprite_w,
    );
    if let Some(offset) = clip.wrap_offset {
        let sx_wrap = sx + offset;
        if sx_wrap + sprite_w as i32 > clip.x_min && sx_wrap < clip.x_max {
            draw_row_inner(
                sprites,
                code,
                src_py,
                sx_wrap,
                flip_x,
                &is_transparent,
                &color_fn,
                buffer,
                clip,
                sprite_w,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_row_inner(
    sprites: &GfxCache,
    code: u16,
    src_py: usize,
    sx: i32,
    flip_x: bool,
    is_transparent: &dyn Fn(u8) -> bool,
    color_fn: &dyn Fn(u8) -> (u8, u8, u8),
    buffer: &mut [u8],
    clip: &SpriteClip,
    sprite_w: usize,
) {
    for px in 0..sprite_w {
        let draw_x = sx + px as i32;
        if draw_x < clip.x_min || draw_x >= clip.x_max {
            continue;
        }
        let src_px = if flip_x { sprite_w - 1 - px } else { px };
        let pixel_value = sprites.pixel(code as usize, src_px, src_py);
        if is_transparent(pixel_value) {
            continue;
        }
        let (r, g, b) = color_fn(pixel_value);
        let off = draw_x as usize * 3;
        buffer[off] = r;
        buffer[off + 1] = g;
        buffer[off + 2] = b;
    }
}

/// Draw one row of a single sprite into a persistent **indexed** scanline buffer
/// plus a parallel priority buffer.
///
/// The index/priority-buffer sibling of [`draw_sprite_row`]: instead of writing
/// RGB, it writes a palette index into `index_buf` and a priority value into
/// `prio_buf` for each opaque pixel (both are 1 byte per pixel). The machine
/// composites these buffers into RGB in a later pass, applying its own priority
/// rules — this helper writes unconditionally where the pixel is opaque, so the
/// caller's draw order / priority resolution is preserved. Handles horizontal
/// flip, clipping, and optional X wraparound exactly like [`draw_sprite_row`].
#[allow(clippy::too_many_arguments)]
pub fn draw_sprite_row_indexed<F, G>(
    sprites: &GfxCache,
    code: u16,
    src_py: usize,
    sx: i32,
    flip_x: bool,
    is_transparent: F,
    resolve_index_fn: G,
    index_buf: &mut [u8],
    prio_buf: &mut [u8],
    clip: &SpriteClip,
) where
    F: Fn(u8) -> bool,
    G: Fn(u8) -> (u8, u8),
{
    let sprite_w = sprites.width();
    draw_row_inner_indexed(
        sprites,
        code,
        src_py,
        sx,
        flip_x,
        &is_transparent,
        &resolve_index_fn,
        index_buf,
        prio_buf,
        clip,
        sprite_w,
    );
    if let Some(offset) = clip.wrap_offset {
        let sx_wrap = sx + offset;
        if sx_wrap + sprite_w as i32 > clip.x_min && sx_wrap < clip.x_max {
            draw_row_inner_indexed(
                sprites,
                code,
                src_py,
                sx_wrap,
                flip_x,
                &is_transparent,
                &resolve_index_fn,
                index_buf,
                prio_buf,
                clip,
                sprite_w,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_row_inner_indexed(
    sprites: &GfxCache,
    code: u16,
    src_py: usize,
    sx: i32,
    flip_x: bool,
    is_transparent: &dyn Fn(u8) -> bool,
    resolve_index_fn: &dyn Fn(u8) -> (u8, u8),
    index_buf: &mut [u8],
    prio_buf: &mut [u8],
    clip: &SpriteClip,
    sprite_w: usize,
) {
    for px in 0..sprite_w {
        let draw_x = sx + px as i32;
        if draw_x < clip.x_min || draw_x >= clip.x_max {
            continue;
        }
        let src_px = if flip_x { sprite_w - 1 - px } else { px };
        let pixel_value = sprites.pixel(code as usize, src_px, src_py);
        if is_transparent(pixel_value) {
            continue;
        }
        let (index, priority) = resolve_index_fn(pixel_value);
        index_buf[draw_x as usize] = index;
        prio_buf[draw_x as usize] = priority;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_wrap_clip(x_max: i32) -> SpriteClip {
        SpriteClip {
            x_min: 0,
            x_max,
            wrap_offset: None,
        }
    }

    #[test]
    fn draw_sprite_row_basic() {
        let mut cache = GfxCache::new(1, 4, 1);
        cache.set_pixel(0, 0, 0, 0); // transparent
        cache.set_pixel(0, 1, 0, 1);
        cache.set_pixel(0, 2, 0, 2);
        cache.set_pixel(0, 3, 0, 3);

        let mut buffer = vec![0u8; 10 * 3];

        draw_sprite_row(
            &cache,
            0,
            0,
            2,     // sx = 2
            false, // no flip
            |pv| pv == 0,
            |pv| (pv * 50, pv * 50, pv * 50),
            &mut buffer,
            &no_wrap_clip(10),
        );

        // px=2 is transparent (pixel value 0), so stays 0
        assert_eq!(buffer[6], 0);
        // px=3 has pixel value 1 -> RGB 50,50,50
        assert_eq!(buffer[9], 50);
        // px=4 has pixel value 2 -> RGB 100,100,100
        assert_eq!(buffer[12], 100);
        // px=5 has pixel value 3 -> RGB 150,150,150
        assert_eq!(buffer[15], 150);
    }

    #[test]
    fn draw_sprite_row_with_flip() {
        let mut cache = GfxCache::new(1, 4, 1);
        cache.set_pixel(0, 0, 0, 1);
        cache.set_pixel(0, 1, 0, 2);
        cache.set_pixel(0, 2, 0, 3);
        cache.set_pixel(0, 3, 0, 0); // transparent

        let mut buffer = vec![0u8; 10 * 3];

        draw_sprite_row(
            &cache,
            0,
            0,
            0,
            true, // flipped
            |pv| pv == 0,
            |pv| (pv * 50, 0, 0),
            &mut buffer,
            &no_wrap_clip(10),
        );

        // Flipped: src_px for draw px=0 is 3 (transparent), px=1 is 2, px=2 is 1, px=3 is 0
        assert_eq!(buffer[0], 0); // transparent
        assert_eq!(buffer[3], 150); // pixel value 3
        assert_eq!(buffer[6], 100); // pixel value 2
        assert_eq!(buffer[9], 50); // pixel value 1
    }

    #[test]
    fn draw_sprite_row_with_wrap() {
        let mut cache = GfxCache::new(1, 4, 1);
        for px in 0..4 {
            cache.set_pixel(0, px, 0, 1);
        }

        let mut buffer = vec![0u8; 10 * 3];

        let clip = SpriteClip {
            x_min: 0,
            x_max: 10,
            wrap_offset: Some(-10),
        };
        draw_sprite_row(
            &cache,
            0,
            0,
            8,
            false,
            |_| false,
            |_| (255, 0, 0),
            &mut buffer,
            &clip,
        );

        // Primary at 8,9 (10,11 clipped)
        assert_eq!(buffer[24], 255);
        assert_eq!(buffer[27], 255);
        // Wrap copy at -2,-1,0,1 -> only 0,1 visible
        assert_eq!(buffer[0], 255);
        assert_eq!(buffer[3], 255);
        // Middle should be untouched
        assert_eq!(buffer[15], 0);
    }

    #[test]
    fn draw_sprite_row_indexed_writes_index_and_priority() {
        let mut cache = GfxCache::new(1, 4, 1);
        cache.set_pixel(0, 0, 0, 0); // transparent
        cache.set_pixel(0, 1, 0, 1);
        cache.set_pixel(0, 2, 0, 2);
        cache.set_pixel(0, 3, 0, 3);

        let mut index_buf = vec![9u8; 8]; // sentinel
        let mut prio_buf = vec![0u8; 8];

        draw_sprite_row_indexed(
            &cache,
            0,
            0,
            2, // sx = 2
            false,
            |pv| pv == 0,             // pixel 0 transparent
            |pv| (0x40 + pv, pv & 1), // index = 0x40+pv, priority = pv&1
            &mut index_buf,
            &mut prio_buf,
            &no_wrap_clip(8),
        );

        // px=2 transparent -> untouched sentinel.
        assert_eq!(index_buf[2], 9);
        assert_eq!(prio_buf[2], 0);
        // px=3,4,5 get pixel values 1,2,3.
        assert_eq!(index_buf[3], 0x41);
        assert_eq!(prio_buf[3], 1);
        assert_eq!(index_buf[4], 0x42);
        assert_eq!(prio_buf[4], 0);
        assert_eq!(index_buf[5], 0x43);
        assert_eq!(prio_buf[5], 1);
    }

    #[test]
    fn draw_sprite_row_indexed_flips() {
        let mut cache = GfxCache::new(1, 4, 1);
        cache.set_pixel(0, 0, 0, 1);
        cache.set_pixel(0, 1, 0, 2);
        cache.set_pixel(0, 2, 0, 3);
        cache.set_pixel(0, 3, 0, 0); // transparent

        let mut index_buf = vec![0u8; 8];
        let mut prio_buf = vec![0u8; 8];
        draw_sprite_row_indexed(
            &cache,
            0,
            0,
            0,
            true, // flipped
            |pv| pv == 0,
            |pv| (pv, 0),
            &mut index_buf,
            &mut prio_buf,
            &no_wrap_clip(8),
        );
        // Flipped: draw px0 = src3 (transparent), px1 = src2 (3), px2 = src1 (2), px3 = src0 (1)
        assert_eq!(index_buf[0], 0);
        assert_eq!(index_buf[1], 3);
        assert_eq!(index_buf[2], 2);
        assert_eq!(index_buf[3], 1);
    }
}
