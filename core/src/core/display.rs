//! What a viewer chooses about how the picture is drawn.
//!
//! These are properties of the tube and of how it is set up, not of the
//! generator feeding it, so they are not vector-specific: a raster CRT has a
//! brightness control, a focus control and a faceplate that halates, exactly as
//! a vector one does. Both renderers read the same three values.
//!
//! This once said that knobs making sense for only one kind of display, such as
//! scanline depth or a shadow mask, would belong here beside them. The raster
//! renderer arrived and wanted none, which is worth writing down because the
//! obvious thing to do was add them.
//!
//! A mask control has nothing to control: at arcade resolutions one emulated
//! pixel spans one to two and a half mask triads, so the mask sits below the
//! pixel grid and cannot resolve stripes on any board in the registry. A
//! scanline depth control would be a second name for focus, since the gap
//! between lines is whatever the spot does not cover, and how deep the gaps run
//! already varies with the picture on its own: the beam widens with drive, so a
//! bright raster washes its own scanlines out.
//!
//! Three controls, each one a thing an operator could reach on a real cabinet.
//!
//! None of this touches emulation. It changes how a frame is drawn and nothing
//! else, which is why it can be a process-wide value rather than something
//! threaded through a machine: one viewer, one screen, one set of preferences.

use crate::device::crt::{HALATION_FRACTION, HALATION_OFF};

/// The knobs a viewer has, as deviations from what was measured off the tube.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplaySettings {
    /// The brightness control. 1.0 is where an operator would set it: the
    /// brightest thing the machine can draw reads full white and no further, a
    /// full-intensity vector at the beam's top speed or a fully lit raster along
    /// the center of a line. Above that, more of the picture saturates and
    /// blooms.
    pub brightness: f32,

    /// The focus control, as a multiple of the tube's measured spot. 1.0 is the
    /// 0.7 mm spot of a well adjusted 19 inch tube; higher is a softer picture.
    /// A renderer will not draw a spot finer than its own grid can represent,
    /// whatever this says.
    ///
    /// On a raster machine this is also the scanline control, and the reason
    /// there is not a separate one: the gap between lines is whatever the spot
    /// does not cover, so widening the spot fills it in. Focus is also the
    /// control that genuinely varied between real monitors, since a figure
    /// expressed per line pitch barely moves with tube size.
    pub focus: f32,

    /// Fraction of a spot's light that leaves as halation rather than directly.
    ///
    /// The one figure in the beam model with no derivation behind it, and so the
    /// one most worth putting in a viewer's hands. See
    /// [`crate::device::crt::HALATION_FRACTION`].
    pub halation: f32,
}

impl DisplaySettings {
    /// Every knob at the figure the tube gives, and the glow at the default
    /// someone settled on by eye.
    pub const MEASURED: Self = Self {
        brightness: 1.0,
        focus: 1.0,
        halation: HALATION_FRACTION,
    };

    /// Brighter and glowier than the cabinet ever was: a poster rather than a
    /// photograph.
    pub const PUNCHY: Self = Self {
        brightness: 1.6,
        focus: 1.2,
        halation: 0.2,
    };

    /// The same settings with the glow off, for a renderer that cannot afford
    /// it. See [`crate::device::crt::HALATION_OFF`].
    pub fn without_halation(self) -> Self {
        Self {
            halation: HALATION_OFF,
            ..self
        }
    }
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self::MEASURED
    }
}

static SETTINGS: std::sync::RwLock<DisplaySettings> =
    std::sync::RwLock::new(DisplaySettings::MEASURED);

/// The display settings in force.
///
/// Read at the edge, where a board hands a renderer its arguments, so the
/// renderers stay pure functions of what they are given and a test can hand them
/// whatever it likes without racing anything else.
pub fn display_settings() -> DisplaySettings {
    SETTINGS
        .read()
        .map(|s| *s)
        .unwrap_or(DisplaySettings::MEASURED)
}

/// Replace the display settings. Takes effect on the next frame drawn.
pub fn set_display_settings(settings: DisplaySettings) {
    if let Ok(mut s) = SETTINGS.write() {
        *s = settings;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_measured_defaults_are_the_figures_the_model_derived() {
        let s = DisplaySettings::default();
        assert_eq!(
            s.brightness, 1.0,
            "1.0 is the operator's setting, not a gain"
        );
        assert_eq!(s.focus, 1.0, "1.0 is the tube's own spot");
        assert_eq!(s.halation, HALATION_FRACTION);
    }

    #[test]
    fn turning_the_glow_off_leaves_the_other_knobs_alone() {
        let punchy = DisplaySettings::PUNCHY;
        let dark = punchy.without_halation();
        assert_eq!(dark.halation, HALATION_OFF);
        assert_eq!(dark.brightness, punchy.brightness);
        assert_eq!(dark.focus, punchy.focus);
    }
}
