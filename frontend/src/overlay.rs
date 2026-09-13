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
