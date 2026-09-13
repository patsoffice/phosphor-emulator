//! The tube, as the renderers need to know it.
//!
//! Lengths on the glass and the figures derived from them. These describe the
//! display, not whatever is driving it: a vector generator and a raster board
//! sweep the same 19 inch shadow-mask tube, with the same spot, the same
//! faceplate and the same halation, and differ only in the pattern they trace
//! on it.
//!
//! That is why these live here rather than beside the Atari vector generator,
//! where they were first written down. Every consumer was a vector machine until
//! the raster CRT stage in the frontend, at which point the home had become
//! actively misleading: `core::display`'s module doc already said in as many
//! words that these are properties of the tube rather than of the generator
//! feeding it.
//!
//! Everything here is shared by the CPU rasterizer and the frontend's GL paths,
//! so it is written down once and cannot drift between them.

/// Long axis of a 19 inch 4:3 viewable area, in millimetres.
///
/// Every length here is measured on the glass, so they are all expressed against
/// this and converted into whatever units a generator or a raster uses.
pub const TUBE_LONG_AXIS_MM: f32 = 360.0;

/// Short axis of the same 4:3 viewable area, in millimetres.
///
/// The axis the lines are stacked along, whatever the cabinet does with the tube
/// afterwards: a vertical cabinet turns the whole assembly, so a rotated game's
/// scanlines run across its *screen* horizontally while still being stacked
/// along the tube's short axis.
pub const TUBE_SHORT_AXIS_MM: f32 = TUBE_LONG_AXIS_MM * 3.0 / 4.0;

/// Focused beam spot diameter, in millimetres.
///
/// The Atari colour XY monitors are 19 inch shadow-mask tubes, the same family
/// as the raster monitors of the era, and two things bound the spot: the mask
/// pitch, about 0.6 mm, below which nothing is resolvable, and the focused spot
/// itself at about 0.7 mm.
pub const BEAM_SPOT_MM: f32 = 0.7;

/// Focused beam spot diameter as a fraction of the tube's long axis.
///
/// Works out at about 1.1 units on Tempest's 580, 1.8 on Quantum's 900, and 2.0
/// on the DVG's 1024.
pub const BEAM_SPOT_FRACTION: f32 = BEAM_SPOT_MM / TUBE_LONG_AXIS_MM;

/// Faceplate thickness of a 19 inch CRT, in millimetres.
pub const FACEPLATE_MM: f32 = 11.0;

/// Refractive index of CRT faceplate glass.
pub const FACEPLATE_INDEX: f32 = 1.54;

/// Fraction of a spot's light that leaves the tube as halation rather than
/// directly.
///
/// This is the one figure here that is not derived. The others are lengths on
/// the glass; this is an optical efficiency that depends on how isotropically
/// the phosphor emits, the aluminium backing behind it, the glass tint and any
/// anti-reflective coating, none of which we have numbers for.
///
/// So it is set by eye, which is the only instrument we have for it: 0.15 read
/// as slightly too much glow against the core, and 0.07 is where it was left.
/// Both are inside the range measured CRT spot profiles show. This is the
/// natural thing for a viewer to want on a slider, and the value here is only
/// the default it should start from.
pub const HALATION_FRACTION: f32 = 0.07;

/// Halation for a renderer that has to composite it over the whole frame on the
/// CPU, where it is not worth its cost.
///
/// The skirt is wide, so compositing it is proportional to the pixel count
/// rather than to the number of vectors, and it measured at four to five times
/// the beam sweep itself: on Asteroids' 1024 by 1024 field, 2.7 ms per frame
/// became 13.5, against 0.4 ms to emulate the machine. The GPU does the same
/// blur for nothing, so the frontend's path has it and the CPU rasterizer that
/// serves screenshots and the debug panel does not.
pub const HALATION_OFF: f32 = 0.0;

/// A Gaussian's standard deviation for a given full width at half maximum:
/// `FWHM = 2*sqrt(2*ln 2)*sigma`.
pub const FWHM_TO_SIGMA: f32 = 1.0 / 2.354_82;

/// Where the beam profile is cut off, in sigmas.
///
/// Truncating leaves a step the height of the profile there, so it has to fall
/// below one level of an 8-bit channel: 3 sigma is 1.1% of the peak and would
/// show as a faint edge, 3.5 is 0.2% and rounds away.
pub const BEAM_CUTOFF_SIGMAS: f32 = 3.5;

/// Floor on the rendered spot's sigma, in output pixels.
///
/// Not a taste value: a Gaussian sampled on a unit grid has a residual ripple of
/// about `2*exp(-2*pi^2*sigma^2)` depending on where its centre falls between
/// samples, which is the spot aliasing against the grid. That ripple is a
/// brightness that varies with the angle of the line, so sigma has to stay where
/// it is negligible: 0.4 gives 8%, 0.5 gives 1.5%, 0.6 gives 0.2%.
///
/// This is a property of the output grid, not of the tube. Rasterizing at
/// display-list resolution hits it (Tempest's physical spot is just under it, so
/// it draws a touch wide); drawing at window resolution usually does not.
pub const MIN_SIGMA_PIXELS: f32 = 0.6;

/// The smallest raster long axis, in pixels, on which the tube's spot is still
/// representable.
///
/// A generator's coordinate units have no size of their own, so the spot's size
/// in *raster* pixels depends only on how many pixels the long axis has:
/// `sigma_px = raster_long * BEAM_SPOT_FRACTION * FWHM_TO_SIGMA`, with the
/// generator's own extent cancelling out. Setting that equal to
/// [`MIN_SIGMA_PIXELS`], below which the spot aliases against the grid, gives
/// about 727 pixels.
///
/// So this is a floor and not a quality target: under it the beam has to be
/// drawn wider than the tube's, and detail is lost that the generator computed.
/// Above it there is more to gain, and how much more is a cost decision that
/// belongs with the rest of the display settings.
pub const MIN_RASTER_LONG_AXIS: f32 = MIN_SIGMA_PIXELS / (BEAM_SPOT_FRACTION * FWHM_TO_SIGMA);

/// The beam's sigma in a generator's own coordinate units.
///
/// `long_axis_units` is the larger of the generator's two display dimensions,
/// which is the one that maps onto the tube's long axis.
pub fn beam_sigma_units(long_axis_units: f32) -> f32 {
    long_axis_units * BEAM_SPOT_FRACTION * FWHM_TO_SIGMA
}

/// The beam's sigma for a raster tube, in units of one line's pitch.
///
/// A raster machine stacks `lines` lines across the tube's short axis, so the
/// pitch is `TUBE_SHORT_AXIS_MM / lines` and this is the spot measured against
/// it. Expressing it per pitch rather than in millimetres is what makes the
/// scanline structure fall out of the arithmetic: the spot covers a fixed
/// fraction of the gap it has to bridge, and whether there is a visible gap at
/// all is then a consequence rather than a setting.
///
/// It also means tube size very nearly cancels. A larger tube has a
/// proportionally larger spot *and* a larger pitch, so what actually varies
/// between real monitors is focus quality, which is a viewer control rather than
/// a per-machine figure.
///
/// Worked through the registry: 224 lines (Pac-Man, Galaga, Donkey Kong) give
/// 0.25, a spot covering 58% of the pitch at half maximum, so the gaps between
/// lines are real. 480 lines (Satan's Hollow) give 0.53, covering 124%, so the
/// lines overlap and there are no gaps to see. The second is why a shader
/// applying scanlines uniformly would be wrong.
pub fn beam_sigma_lines(lines: f32) -> f32 {
    lines * BEAM_SPOT_MM * FWHM_TO_SIGMA / TUBE_SHORT_AXIS_MM
}

/// The halation skirt's sigma in a generator's own coordinate units.
///
/// The phosphor emits into the faceplate in every direction. Light steeper than
/// the critical angle cannot leave the front surface, so it reflects back,
/// crosses the glass again and re-emerges a distance away: for thickness `t` and
/// index `n`, at a radius of `2*t*tan(asin(1/n))`. With an 11 mm faceplate at
/// n = 1.54 that is about 19 mm, or 5% of the tube's long axis, and it is why a
/// bright vector sits in a broad glow rather than ending at its own edge.
///
/// The ring is treated as a Gaussian skirt of that scale rather than as a ring,
/// which is what the sum over all the emission angles and depths looks like from
/// the front.
pub fn halation_sigma_units(long_axis_units: f32) -> f32 {
    let critical_angle = (1.0 / FACEPLATE_INDEX).asin();
    let radius_mm = 2.0 * FACEPLATE_MM * critical_angle.tan();
    long_axis_units * radius_mm / TUBE_LONG_AXIS_MM
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scanline result the raster CRT work rests on, pinned as arithmetic so
    /// a change to the tube figures has to face it. The first is most of the
    /// registry and the second is the one machine that differs, and the model
    /// earns its keep only by telling them apart.
    #[test]
    fn the_spot_bridges_the_pitch_on_a_high_line_count_board_and_not_a_low_one() {
        // Coverage is the spot's full width at half maximum against the pitch,
        // which is the sigma here converted back through FWHM_TO_SIGMA.
        let coverage = |lines: f32| beam_sigma_lines(lines) / FWHM_TO_SIGMA;

        // 224 lines: gaps, at 58% of the pitch covered.
        assert!(
            (coverage(224.0) - 0.581).abs() < 0.002,
            "224-line coverage was {}",
            coverage(224.0)
        );
        // 480 lines: no gaps, the lines overlapping at 124%.
        assert!(
            (coverage(480.0) - 1.244).abs() < 0.002,
            "480-line coverage was {}",
            coverage(480.0)
        );
        assert!(
            coverage(224.0) < 1.0 && coverage(480.0) > 1.0,
            "the model has to separate these two, not merely scale between them"
        );
    }

    /// Pitch and spot both scale with the tube, so a figure expressed per pitch
    /// does not move when the glass gets bigger. This is what makes a single set
    /// of constants defensible across a cabinet population that was never one
    /// monitor.
    #[test]
    fn sigma_per_pitch_is_proportional_to_the_line_count_alone() {
        let a = beam_sigma_lines(224.0);
        let b = beam_sigma_lines(448.0);
        assert!(
            (b - 2.0 * a).abs() < 1e-6,
            "doubling the lines should double the spot measured per pitch"
        );
    }
}
