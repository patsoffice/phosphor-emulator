//! The FPS / stats / PAUSED overlay, drawn as an egui layer above the picture.
//!
//! This used to rasterize a 4x5 bitmap font into the framebuffer, which worked
//! only while the raster path still produced a post-rotation CPU buffer to draw
//! into. Once the cabinet's rotation moved onto the GPU there was no such
//! buffer: the only CPU buffer left is the machine's native raster, and text
//! drawn there would turn with the picture and read sideways on every machine
//! with a rotated monitor.
//!
//! Drawing it in egui instead also makes one overlay serve both display kinds.
//! The vector path had already grown its own copy of exactly this, for exactly
//! this reason, since it never had a CPU buffer to draw into either.
//!
//! It also carries the window's own confirmation of a host action (a
//! screenshot saved, a movie started or written, a state saved) and a REC
//! mark while a movie records. Those used to reach only the terminal, so a
//! hotkey that worked looked exactly like one that did nothing.

use std::time::{Duration, Instant};

/// How long a notice stays up, and the last part of that over which it fades.
const NOTICE_SHOWN: Duration = Duration::from_millis(2500);
const NOTICE_FADE: Duration = Duration::from_millis(500);

/// The window's confirmation of the last host action, shown briefly over the
/// picture. One at a time: a new notice replaces the one on screen, so two
/// quick screenshots show the second.
///
/// Each carries short text meant for the window (a file's name rather than its
/// path); the terminal log keeps the full line.
#[derive(Default)]
pub struct Notices {
    current: Option<Notice>,
}

struct Notice {
    text: String,
    error: bool,
    since: Instant,
}

impl Notices {
    /// Confirm an action that happened.
    pub fn info(&mut self, text: impl Into<String>) {
        self.show(text.into(), false);
    }

    /// Report an action that the user asked for and that did not happen.
    pub fn error(&mut self, text: impl Into<String>) {
        self.show(text.into(), true);
    }

    fn show(&mut self, text: String, error: bool) {
        self.current = Some(Notice {
            text,
            error,
            since: Instant::now(),
        });
    }

    /// The notice to draw at `now`, with its opacity: full until the fade,
    /// then down to nothing over [`NOTICE_FADE`].
    fn visible(&self, now: Instant) -> Option<(&str, bool, f32)> {
        let n = self.current.as_ref()?;
        let age = now.saturating_duration_since(n.since);
        if age >= NOTICE_SHOWN {
            return None;
        }
        let left = NOTICE_SHOWN - age;
        let alpha = (left.as_secs_f32() / NOTICE_FADE.as_secs_f32()).min(1.0);
        Some((&n.text, n.error, alpha))
    }
}

/// Draw the current notice in the lower-left corner and, while a movie
/// records, a REC mark in the upper-right. Drawn in every presentation path,
/// with or without side panels, since the feedback matters most exactly when
/// nothing else on screen changes.
pub fn draw_status(ctx: &egui::Context, notices: &Notices, recording: bool) {
    if let Some((text, error, alpha)) = notices.visible(Instant::now()) {
        let color = if error {
            egui::Color32::from_rgb(255, 120, 110)
        } else {
            egui::Color32::WHITE
        };
        egui::Area::new(egui::Id::new("notice"))
            .anchor(egui::Align2::LEFT_BOTTOM, egui::vec2(4.0, -4.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(text)
                        .color(color.gamma_multiply(alpha))
                        .background_color(egui::Color32::from_black_alpha((160.0 * alpha) as u8))
                        .monospace(),
                );
            });
    }
    if recording {
        egui::Area::new(egui::Id::new("rec"))
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-4.0, 4.0))
            .order(egui::Order::Foreground)
            .interactable(false)
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new("● REC")
                        .color(egui::Color32::from_rgb(255, 70, 60))
                        .background_color(egui::Color32::from_black_alpha(160))
                        .monospace(),
                );
            });
    }
}

/// Draw an optional FPS line, an optional stats line, and an optional PAUSED
/// line, stacked top-to-bottom in the upper-left corner.
///
/// Only the lines that are present are drawn and consume vertical space, so a
/// PAUSED-only overlay sits at the top whether or not the FPS readout is on.
pub fn draw_overlay(ctx: &egui::Context, fps: Option<&str>, stats: Option<&str>, paused: bool) {
    if fps.is_none() && stats.is_none() && !paused {
        return;
    }

    egui::Window::new("fps_overlay")
        .title_bar(false)
        .resizable(false)
        .fixed_pos(egui::pos2(4.0, 4.0))
        .frame(egui::Frame::NONE)
        .show(ctx, |ui| {
            // Wide enough that a changing FPS readout does not resize the
            // window under itself every frame.
            ui.set_min_width(120.0);
            for text in [fps, stats, paused.then_some("PAUSED")]
                .into_iter()
                .flatten()
            {
                ui.label(
                    egui::RichText::new(text)
                        .color(egui::Color32::WHITE)
                        // Against the picture rather than a panel, so the text
                        // needs to carry its own contrast.
                        .background_color(egui::Color32::from_black_alpha(160))
                        .monospace(),
                );
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_notice_holds_then_fades_then_goes() {
        let mut n = Notices::default();
        assert!(
            n.visible(Instant::now()).is_none(),
            "nothing before a notice"
        );
        n.info("Screenshot saved: x.png");
        let t0 = n.current.as_ref().unwrap().since;
        let (text, error, alpha) = n.visible(t0).unwrap();
        assert_eq!(
            (text, error, alpha),
            ("Screenshot saved: x.png", false, 1.0)
        );
        let mid_fade = t0 + NOTICE_SHOWN - NOTICE_FADE / 2;
        let (_, _, alpha) = n.visible(mid_fade).unwrap();
        assert!((alpha - 0.5).abs() < 1e-3, "half faded, alpha {alpha}");
        assert!(
            n.visible(t0 + NOTICE_SHOWN).is_none(),
            "gone after its time"
        );
    }

    #[test]
    fn a_new_notice_replaces_the_one_on_screen() {
        let mut n = Notices::default();
        n.info("first");
        n.error("second");
        let (text, error, _) = n.visible(n.current.as_ref().unwrap().since).unwrap();
        assert_eq!((text, error), ("second", true));
    }
}
