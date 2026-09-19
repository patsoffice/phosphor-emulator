//! Color overlays: the gel a cabinet put between the tube and the player.
//!
//! A monochrome board plus a sheet of colored plastic was how an arcade game got
//! color before color tubes were affordable, and several of them are only
//! describable that way: Asteroids Deluxe draws in white and everything a player
//! saw as blue-green was the sheet. The board is not wrong and the picture is
//! not white, so something has to hold the sheet, and this is it.
//!
//! # Why this is presentation and not emulation
//!
//! The sheet is not drawn. It is cabinet artwork, physically in front of the
//! glass, and a machine's `render_frame` is what its board put on the tube. So
//! this composites at presentation, in the frontend, and nothing in
//! `phosphor-core`, `phosphor-machines` or the golden frames knows it exists.
//! Adding an overlay to a machine cannot move a golden hash, which is what keeps
//! those 44 pins measuring emulation rather than decoration. The reference
//! emulator makes the same split, and lets artwork be switched off for the same
//! reason.
//!
//! # Color space
//!
//! The regions multiply the picture, and they do it on **displayed** values, not
//! on radiance. A gel really does attenuate light, so linear light is where a
//! physical transmission belongs, and it would be an easy and wrong improvement
//! to convert these figures into it: they are not physical transmissions. They
//! are numbers somebody chose by eye against a compositor that multiplies
//! encoded color, so they already carry that compositor's response. Asteroids
//! Deluxe's 0.5333 decodes to about 0.25 in linear light, so "fixing" the space
//! without also re-deriving the number turns a cyan sheet into a barely tinted
//! one. Either both move or neither does.
//!
//! What this does get right is *when*: the tint is applied inside the beam
//! shaders, where values are still floating point and may exceed 1.0, rather
//! than after the clip to eight bits. Light is attenuated on its way out of the
//! tube, so a highlight that the display cannot show still loses the same
//! fraction of its red before it is clipped, and not after.

use std::path::PathBuf;

use serde::Deserialize;

/// Overlays that ship with the emulator, by machine name.
///
/// Embedded rather than read from a directory beside the binary, so the picture
/// does not depend on the working directory and a stray copy cannot shadow the
/// committed one. A file in the user's overlay directory still wins; see
/// [`ScreenOverlay::for_machine`].
const BUILT_IN: &[(&str, &str)] = &[("astdelux", include_str!("../overlays/astdelux.toml"))];

/// Edge length of the rasterized sheet, in texels.
///
/// The sheet is sampled with linear filtering and stretched over the tube, so
/// this sets how sharply a region boundary can land: 1/512 of the tube face,
/// under a pixel at any window size anyone plays at. It is square because the
/// bounds are fractions of the tube whatever shape the tube is.
const SIZE: usize = 512;

/// Samples per texel per axis when rasterizing, for antialiased region edges.
///
/// A boundary that does not fall on a texel center is what this is for. Rects
/// with axis-aligned edges barely need it; a disk does, and it costs 16 point
/// tests per texel once at load.
const SUPERSAMPLE: usize = 4;

/// What a region covers.
///
/// These are the reference emulator's two geometric artwork primitives, which is
/// what the committed overlays are transcribed from. It does not carry an image
/// primitive: a scanned photograph of a real bezel is a different kind of thing
/// from a described one, and nothing in the tree wants it yet.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Shape {
    /// The whole bounding box.
    Rect,
    /// The ellipse inscribed in the bounding box.
    Disk,
}

/// One painted area of the sheet.
#[derive(Clone, Copy, Debug, Deserialize)]
pub struct Region {
    pub shape: Shape,
    /// `[left, top, right, bottom]` as fractions of the tube face **as viewed**,
    /// x from the left edge and y from the top. Viewing orientation, not the
    /// machine's native framebuffer orientation: the sheet is glued to the
    /// cabinet and does not turn with a rotated monitor.
    pub bounds: [f32; 4],
    /// `[r, g, b]` transmission, 0 blocking and 1 clear. See the module note on
    /// what space these are in.
    pub color: [f32; 3],
}

/// An overlay as the TOML file describes it.
#[derive(Clone, Debug, Deserialize)]
struct OverlayFile {
    #[serde(default)]
    name: String,
    #[serde(default, rename = "region")]
    regions: Vec<Region>,
}

/// A cabinet's color overlay, rasterized and ready to multiply a picture.
#[derive(Clone)]
pub struct ScreenOverlay {
    /// The name the file gives it, for the log line and the UI.
    pub name: String,
    /// RGB8, [`SIZE`] by [`SIZE`], row 0 at the **top** of the tube as viewed.
    pixels: Vec<u8>,
}

impl ScreenOverlay {
    /// Edge length of [`pixels`](Self::pixels), in texels.
    pub const SIZE: u32 = SIZE as u32;

    /// The overlay for a machine, if it has one.
    ///
    /// A file at `<config>/overlays/<machine>.toml` wins over the built-in, so
    /// an overlay can be tried or corrected without a rebuild, and a machine
    /// with no built-in can be given one. A file that will not parse is a
    /// warning and no overlay, never a failure to start: an unreadable piece of
    /// decoration is not a reason to refuse to run the game.
    pub fn for_machine(machine: &str) -> Option<Self> {
        if let Some(path) = user_overlay_path(machine)
            && path.exists()
        {
            match std::fs::read_to_string(&path) {
                Ok(text) => return Self::parse(&text, &path.display().to_string()),
                Err(e) => log::warn!("overlay {}: {e}", path.display()),
            }
        }
        let (_, text) = BUILT_IN.iter().find(|(name, _)| *name == machine)?;
        Self::parse(text, machine)
    }

    /// Parse and rasterize one overlay's TOML. `origin` names it in warnings.
    fn parse(text: &str, origin: &str) -> Option<Self> {
        let file: OverlayFile = match toml::from_str(text) {
            Ok(f) => f,
            Err(e) => {
                log::warn!("overlay {origin}: {e}");
                return None;
            }
        };
        if file.regions.is_empty() {
            log::warn!("overlay {origin}: no regions, ignoring");
            return None;
        }
        Some(Self {
            name: file.name,
            pixels: rasterize(&file.regions),
        })
    }

    /// The rasterized sheet, RGB8, row 0 at the top as viewed.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Multiply an RGB24 image by the sheet, in place.
    ///
    /// This is the CPU path, for a screenshot: the GL renderers sample the same
    /// sheet as a texture instead. The two agree because both stretch it over
    /// the whole picture and both multiply, but this one is nearest-sampled and
    /// after the clip to eight bits, which is all a screenshot can be.
    ///
    /// `rgb` is `width * height * 3` bytes in viewing orientation.
    pub fn apply(&self, rgb: &mut [u8], width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        for y in 0..height as usize {
            // Texel centers, so a one-region sheet is exact and a boundary lands
            // where the bounds put it rather than half a texel off.
            let sy = ((y as f32 + 0.5) / height as f32 * SIZE as f32) as usize;
            let sy = sy.min(SIZE - 1);
            for x in 0..width as usize {
                let sx = ((x as f32 + 0.5) / width as f32 * SIZE as f32) as usize;
                let sx = sx.min(SIZE - 1);
                let s = (sy * SIZE + sx) * 3;
                let d = (y * width as usize + x) * 3;
                for c in 0..3 {
                    let Some(out) = rgb.get_mut(d + c) else {
                        return;
                    };
                    *out = ((*out as u32 * self.pixels[s + c] as u32) / 255) as u8;
                }
            }
        }
    }
}

/// Where a user's own overlay for this machine would be.
fn user_overlay_path(machine: &str) -> Option<PathBuf> {
    Some(
        crate::config::config_dir()?
            .join("overlays")
            .join(format!("{machine}.toml")),
    )
}

/// Paint the regions onto a clear sheet.
///
/// Clear is white, because the sheet multiplies: an area no region covers has to
/// leave the picture alone, and that is a transmission of 1 rather than of 0.
/// Regions paint in file order, so a later one covers an earlier one where they
/// overlap, which is the order the reference emulator's elements composite in.
fn rasterize(regions: &[Region]) -> Vec<u8> {
    let mut out = vec![255u8; SIZE * SIZE * 3];
    let step = 1.0 / SUPERSAMPLE as f32;
    for ty in 0..SIZE {
        for tx in 0..SIZE {
            // Coverage-weighted transmission at this texel, starting clear.
            let mut acc = [1.0f32; 3];
            for region in regions {
                let mut hits = 0u32;
                for sy in 0..SUPERSAMPLE {
                    for sx in 0..SUPERSAMPLE {
                        let u = (tx as f32 + (sx as f32 + 0.5) * step) / SIZE as f32;
                        let v = (ty as f32 + (sy as f32 + 0.5) * step) / SIZE as f32;
                        if covers(region, u, v) {
                            hits += 1;
                        }
                    }
                }
                if hits == 0 {
                    continue;
                }
                // Partial coverage mixes toward the region's color rather than
                // replacing, which is what antialiases the edge.
                let cover = hits as f32 / (SUPERSAMPLE * SUPERSAMPLE) as f32;
                for (a, &c) in acc.iter_mut().zip(region.color.iter()) {
                    *a += (c - *a) * cover;
                }
            }
            let o = (ty * SIZE + tx) * 3;
            for (out, a) in out[o..o + 3].iter_mut().zip(acc.iter()) {
                *out = (a.clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
    }
    out
}

/// Whether a region covers the point `(u, v)` in tube fractions.
fn covers(region: &Region, u: f32, v: f32) -> bool {
    let [left, top, right, bottom] = region.bounds;
    let (w, h) = (right - left, bottom - top);
    if w <= 0.0 || h <= 0.0 {
        return false;
    }
    match region.shape {
        Shape::Rect => u >= left && u < right && v >= top && v < bottom,
        Shape::Disk => {
            // Normalized offset from the bounding box's center; inside the
            // inscribed ellipse when that offset is within the unit circle.
            let dx = (u - (left + w * 0.5)) / (w * 0.5);
            let dy = (v - (top + h * 0.5)) / (h * 0.5);
            dx * dx + dy * dy <= 1.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Read a texel's three channels as fractions.
    fn texel(o: &ScreenOverlay, u: f32, v: f32) -> [f32; 3] {
        let x = ((u * SIZE as f32) as usize).min(SIZE - 1);
        let y = ((v * SIZE as f32) as usize).min(SIZE - 1);
        let i = (y * SIZE + x) * 3;
        [
            o.pixels[i] as f32 / 255.0,
            o.pixels[i + 1] as f32 / 255.0,
            o.pixels[i + 2] as f32 / 255.0,
        ]
    }

    /// Every overlay that ships has to parse, or it is decoration that silently
    /// does nothing. `for_machine` swallows a parse failure by design, so this
    /// is the check that the committed files are not the ones being swallowed.
    #[test]
    fn every_built_in_overlay_parses() {
        assert!(!BUILT_IN.is_empty(), "no built-in overlays to check");
        for (machine, text) in BUILT_IN {
            let o = ScreenOverlay::parse(text, machine)
                .unwrap_or_else(|| panic!("{machine}: built-in overlay does not parse"));
            assert!(!o.name.is_empty(), "{machine}: overlay has no name");
            assert_eq!(o.pixels.len(), SIZE * SIZE * 3);
        }
    }

    /// Asteroids Deluxe is one cyan sheet over the whole tube: red cut to about
    /// half, green and blue untouched. It draws in white only, so this is the
    /// entire reason its picture is not white.
    #[test]
    fn astdelux_cuts_red_across_the_whole_tube() {
        let o = ScreenOverlay::for_machine("astdelux").expect("astdelux has an overlay");
        for (u, v) in [(0.01, 0.01), (0.5, 0.5), (0.99, 0.99), (0.99, 0.01)] {
            let [r, g, b] = texel(&o, u, v);
            assert!((r - 0.5333).abs() < 0.01, "red at ({u},{v}) is {r}");
            assert!((g - 1.0).abs() < 0.01, "green at ({u},{v}) is {g}");
            assert!((b - 1.0).abs() < 0.01, "blue at ({u},{v}) is {b}");
        }
    }

    /// A machine with no overlay gets none, rather than a clear sheet. The
    /// difference matters: `None` lets the renderers skip the sample entirely.
    #[test]
    fn a_machine_without_an_overlay_has_none() {
        assert!(ScreenOverlay::for_machine("asteroid").is_none());
        assert!(ScreenOverlay::for_machine("no_such_machine").is_none());
    }

    /// Bands, which is the shape of every overlay that is not a single tint:
    /// the reference's Battlezone artwork is a red strip over a green field.
    /// Transcribed here rather than shipped, because we do not emulate it.
    #[test]
    fn regions_land_where_their_bounds_put_them() {
        let text = r#"
            name = "Bands"
            [[region]]
            shape = "rect"
            bounds = [0.0, 0.0, 1.0, 0.2]
            color = [1.0, 0.125, 0.125]
            [[region]]
            shape = "rect"
            bounds = [0.0, 0.2, 1.0, 1.0]
            color = [0.125, 1.0, 0.125]
        "#;
        let o = ScreenOverlay::parse(text, "bands").expect("parses");

        // Well inside the top strip: red passes, green and blue are cut.
        let [r, g, _] = texel(&o, 0.5, 0.1);
        assert!(r > 0.9 && g < 0.2, "top strip is {r}, {g}");
        // Well inside the field below it: the other way round.
        let [r, g, _] = texel(&o, 0.5, 0.6);
        assert!(r < 0.2 && g > 0.9, "lower field is {r}, {g}");
    }

    /// A disk is the ellipse inside its bounds, not the bounds.
    #[test]
    fn a_disk_leaves_the_corners_of_its_box_clear() {
        let text = r#"
            name = "Spot"
            [[region]]
            shape = "disk"
            bounds = [0.0, 0.0, 1.0, 1.0]
            color = [0.0, 0.0, 0.0]
        "#;
        let o = ScreenOverlay::parse(text, "spot").expect("parses");
        assert!(
            texel(&o, 0.5, 0.5)[0] < 0.01,
            "the middle should be blocked"
        );
        assert!(texel(&o, 0.02, 0.02)[0] > 0.99, "a corner should be clear");
    }

    /// Nothing painted leaves the picture alone. A sheet that defaulted to black
    /// would blank every machine that has an overlay covering only part of it.
    #[test]
    fn an_uncovered_area_passes_the_picture_unchanged() {
        let text = r#"
            name = "Half"
            [[region]]
            shape = "rect"
            bounds = [0.0, 0.0, 0.5, 1.0]
            color = [0.0, 0.0, 0.0]
        "#;
        let o = ScreenOverlay::parse(text, "half").expect("parses");
        assert!(texel(&o, 0.75, 0.5).iter().all(|&c| c > 0.99));

        let mut rgb = vec![200u8; 4 * 3];
        o.apply(&mut rgb, 4, 1);
        assert_eq!(&rgb[0..3], &[0, 0, 0], "left half is blocked");
        assert_eq!(&rgb[9..12], &[200, 200, 200], "right half is untouched");
    }

    /// Malformed input is a warning and no overlay, not a panic. This is
    /// reachable from a user's own file, so it has to be survivable.
    #[test]
    fn a_file_that_will_not_parse_yields_no_overlay() {
        assert!(ScreenOverlay::parse("this is not toml {{", "bad").is_none());
        assert!(ScreenOverlay::parse("name = \"empty\"", "empty").is_none());
        assert!(
            ScreenOverlay::parse(
                "name = \"x\"\n[[region]]\nshape = \"hexagon\"\nbounds = [0,0,1,1]\ncolor = [1,1,1]",
                "shape"
            )
            .is_none()
        );
    }
}
