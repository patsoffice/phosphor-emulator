/// Minimal 4x5 bitmap font for overlay text. Each glyph is 4 pixels wide, 5 rows tall.
/// Bits are MSB-left within each u8 (only top 4 bits used).
const GLYPHS: &[(&[u8; 5], u8)] = &[
    // digits
    (&[0x60, 0x90, 0x90, 0x90, 0x60], b'0'),
    (&[0x20, 0x60, 0x20, 0x20, 0x70], b'1'),
    (&[0x60, 0x90, 0x20, 0x40, 0xF0], b'2'),
    (&[0x60, 0x90, 0x20, 0x90, 0x60], b'3'),
    (&[0x90, 0x90, 0xF0, 0x10, 0x10], b'4'),
    (&[0xF0, 0x80, 0xE0, 0x10, 0xE0], b'5'),
    (&[0x60, 0x80, 0xE0, 0x90, 0x60], b'6'),
    (&[0xF0, 0x10, 0x20, 0x40, 0x40], b'7'),
    (&[0x60, 0x90, 0x60, 0x90, 0x60], b'8'),
    (&[0x60, 0x90, 0x70, 0x10, 0x60], b'9'),
    // letters (uppercase glyphs, matched case-insensitively)
    (&[0x60, 0x90, 0xF0, 0x90, 0x90], b'a'),
    (&[0xE0, 0x90, 0xE0, 0x90, 0xE0], b'b'),
    (&[0x70, 0x80, 0x80, 0x80, 0x70], b'c'),
    (&[0xE0, 0x90, 0x90, 0x90, 0xE0], b'd'),
    (&[0xF0, 0x80, 0xE0, 0x80, 0xF0], b'e'),
    (&[0xF0, 0x80, 0xE0, 0x80, 0x80], b'f'),
    (&[0x70, 0x80, 0xB0, 0x90, 0x60], b'g'),
    (&[0x90, 0x90, 0xF0, 0x90, 0x90], b'h'),
    (&[0xE0, 0x40, 0x40, 0x40, 0xE0], b'i'),
    (&[0x30, 0x10, 0x10, 0x90, 0x60], b'j'),
    (&[0x90, 0xA0, 0xC0, 0xA0, 0x90], b'k'),
    (&[0x80, 0x80, 0x80, 0x80, 0xF0], b'l'),
    (&[0x90, 0xF0, 0xF0, 0x90, 0x90], b'm'),
    (&[0x90, 0xD0, 0xF0, 0xB0, 0x90], b'n'),
    (&[0x60, 0x90, 0x90, 0x90, 0x60], b'o'),
    (&[0xE0, 0x90, 0xE0, 0x80, 0x80], b'p'),
    (&[0x60, 0x90, 0x90, 0xA0, 0x50], b'q'),
    (&[0xE0, 0x90, 0xE0, 0xA0, 0x90], b'r'),
    (&[0x70, 0x80, 0x60, 0x10, 0xE0], b's'),
    (&[0xF0, 0x20, 0x20, 0x20, 0x20], b't'),
    (&[0x90, 0x90, 0x90, 0x90, 0x60], b'u'),
    (&[0x90, 0x90, 0x90, 0x60, 0x60], b'v'),
    (&[0x90, 0x90, 0xF0, 0xF0, 0x90], b'w'),
    (&[0x90, 0x90, 0x60, 0x90, 0x90], b'x'),
    (&[0x90, 0x90, 0x60, 0x20, 0x20], b'y'),
    (&[0xF0, 0x10, 0x20, 0x40, 0xF0], b'z'),
    // punctuation
    (&[0x00, 0x00, 0x00, 0x00, 0x40], b'.'),
    (&[0x10, 0x10, 0x20, 0x40, 0x40], b'/'),
    (&[0x00, 0x40, 0x00, 0x40, 0x00], b':'),
    (&[0x00, 0x00, 0x00, 0x00, 0x00], b' '),
];

const GLYPH_W: usize = 4;
const GLYPH_H: usize = 5;
const LINE_SPACING: usize = 2; // pixels between lines

fn glyph_for(ch: u8) -> &'static [u8; 5] {
    let lower = ch.to_ascii_lowercase();
    for &(data, c) in GLYPHS {
        if c == lower {
            return data;
        }
    }
    // fallback: space
    &[0x00, 0x00, 0x00, 0x00, 0x00]
}

/// Draw a text string onto an RGB24 framebuffer at a given (x, y) position.
fn draw_text(buffer: &mut [u8], width: usize, x0: usize, y0: usize, text: &str) {
    for (ci, ch) in text.bytes().enumerate() {
        let glyph = glyph_for(ch);
        let gx = x0 + ci * (GLYPH_W + 1);

        for (row, &bits) in glyph.iter().enumerate() {
            let py = y0 + row;
            for col in 0..GLYPH_W {
                if bits & (0x80 >> col) != 0 {
                    let px = gx + col;
                    let offset = (py * width + px) * 3;
                    if offset + 2 < buffer.len() {
                        buffer[offset] = 255;
                        buffer[offset + 1] = 255;
                        buffer[offset + 2] = 255;
                    }
                }
            }
        }
    }
}

/// Draw the overlay: an optional FPS line, an optional stats line, and an
/// optional PAUSED line, stacked top-to-bottom. Only the lines that are present
/// are drawn (and consume vertical space), so a PAUSED-only overlay sits at the
/// top regardless of whether the FPS readout is enabled.
pub fn draw_overlay(
    buffer: &mut [u8],
    width: usize,
    fps_text: Option<&str>,
    stats: Option<&str>,
    paused: bool,
) {
    let x0: usize = 2;
    let mut y: usize = 2;
    let line_step = GLYPH_H + LINE_SPACING;

    for text in [fps_text, stats, paused.then_some("PAUSED")]
        .into_iter()
        .flatten()
    {
        draw_text(buffer, width, x0, y, text);
        y += line_step;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit_pixels(buffer: &[u8]) -> usize {
        buffer
            .chunks(3)
            .filter(|p| p.iter().any(|&b| b != 0))
            .count()
    }

    #[test]
    fn paused_renders_with_fps_off() {
        let (w, h) = (64usize, 16usize);
        let mut buf = vec![0u8; w * h * 3];
        draw_overlay(&mut buf, w, None, None, true);
        assert!(
            lit_pixels(&buf) > 0,
            "PAUSED must render even when FPS is off"
        );
    }

    #[test]
    fn empty_overlay_draws_nothing() {
        let (w, h) = (64usize, 16usize);
        let mut buf = vec![0u8; w * h * 3];
        draw_overlay(&mut buf, w, None, None, false);
        assert_eq!(lit_pixels(&buf), 0);
    }

    #[test]
    fn paused_only_sits_at_top_line() {
        // A PAUSED-only overlay must occupy the first line, not be pushed down by
        // the absent FPS/stats lines — so it's identical to drawing "PAUSED" as
        // the top line.
        let (w, h) = (64usize, 16usize);
        let mut top = vec![0u8; w * h * 3];
        let mut paused_only = vec![0u8; w * h * 3];
        draw_overlay(&mut top, w, Some("PAUSED"), None, false);
        draw_overlay(&mut paused_only, w, None, None, true);
        assert_eq!(top, paused_only);
    }
}
