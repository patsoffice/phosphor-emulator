use std::time::Instant;

use egui_backend::painter::Painter;
use egui_backend::{DpiScaling, EguiStateHandler, ShaderVersion};
use egui_sdl2_gl as egui_backend;
use sdl2::video::{GLContext, GLProfile, Window};

pub struct Video {
    window: Window,
    _gl_ctx: GLContext,
    painter: Painter,
    egui_state: EguiStateHandler,
    egui_ctx: egui::Context,
    game_texture_id: egui::TextureId,
    native_width: u32,
    native_height: u32,
    rgba_buffer: Vec<u8>,
    start_time: Instant,
}

impl Video {
    /// Create a new Video with separate texture and window dimensions.
    ///
    /// `native_width`/`native_height` define the framebuffer texture size.
    /// `window_width`/`window_height` define the initial window size (may differ
    /// for rotated displays, e.g. portrait vector games).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sdl_video: &sdl2::VideoSubsystem,
        title: &str,
        native_width: u32,
        native_height: u32,
        window_width: u32,
        window_height: u32,
        scale: u32,
        position: Option<(i32, i32)>,
    ) -> Self {
        let gl_attr = sdl_video.gl_attr();
        gl_attr.set_context_profile(GLProfile::Core);
        gl_attr.set_context_version(3, 2);
        gl_attr.set_double_buffer(true);
        gl_attr.set_multisample_samples(4);
        gl_attr.set_framebuffer_srgb_compatible(true);

        let mut builder = sdl_video.window(title, window_width * scale, window_height * scale);
        builder.opengl();
        if let Some((x, y)) = position {
            builder.position(x, y);
        } else {
            builder.position_centered();
        }
        let window = builder.build().expect("Failed to create window");

        let gl_ctx = window
            .gl_create_context()
            .expect("Failed to create GL context");

        let (mut painter, egui_state) =
            egui_backend::with_sdl2(&window, ShaderVersion::Default, DpiScaling::Default);
        let egui_ctx = egui::Context::default();

        // Create initial game texture (black with full alpha)
        let pixel_count = (native_width * native_height) as usize;
        let mut rgba_buffer = vec![0u8; pixel_count * 4];
        for chunk in rgba_buffer.chunks_exact_mut(4) {
            chunk[3] = 255;
        }
        let game_texture_id = painter.new_user_texture_rgba8(
            (native_width as usize, native_height as usize),
            rgba_buffer.clone(),
            false, // nearest-neighbor for crisp pixels
        );

        Self {
            window,
            _gl_ctx: gl_ctx,
            painter,
            egui_state,
            egui_ctx,
            game_texture_id,
            native_width,
            native_height,
            rgba_buffer,
            start_time: Instant::now(),
        }
    }

    /// Convert an RGB24 framebuffer to RGBA8 and upload to the game texture.
    pub fn update_game_texture(&mut self, rgb24: &[u8]) {
        let pixel_count = (self.native_width * self.native_height) as usize;
        debug_assert_eq!(rgb24.len(), pixel_count * 3);

        for i in 0..pixel_count {
            self.rgba_buffer[i * 4] = rgb24[i * 3];
            self.rgba_buffer[i * 4 + 1] = rgb24[i * 3 + 1];
            self.rgba_buffer[i * 4 + 2] = rgb24[i * 3 + 2];
            // alpha stays 255 from initialization
        }

        self.painter
            .update_user_texture_rgba8_data(self.game_texture_id, self.rgba_buffer.clone());
    }

    /// Render the game at the target display `aspect` (no debug panels),
    /// letterboxed with black bars when the window doesn't match the aspect.
    pub fn present_game_only(&mut self, aspect: f32) {
        unsafe {
            gl::ClearColor(0.0, 0.0, 0.0, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
        }

        self.egui_state.input.time = Some(self.start_time.elapsed().as_secs_f64());
        self.egui_ctx.begin_pass(self.egui_state.input.take());

        let tex_id = self.game_texture_id;
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(egui::Color32::BLACK))
            .show(&self.egui_ctx, |ui| {
                let (size, offset) = crate::emulator::fit_aspect(ui.available_size(), aspect);
                ui.add_space(offset.y);
                ui.horizontal(|ui| {
                    ui.add_space(offset.x);
                    ui.image(egui::load::SizedTexture::new(tex_id, size));
                });
            });

        self.finish_frame();
    }

    /// Render the game alongside debug panels. The closure builds the debug UI
    /// and letterboxes the game texture to the target aspect itself.
    pub fn present_with_debug<F>(&mut self, debug_ui_fn: F)
    where
        F: FnOnce(&egui::Context, egui::TextureId),
    {
        unsafe {
            gl::ClearColor(0.1, 0.1, 0.1, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
        }

        self.egui_state.input.time = Some(self.start_time.elapsed().as_secs_f64());
        self.egui_ctx.begin_pass(self.egui_state.input.take());

        debug_ui_fn(&self.egui_ctx, self.game_texture_id);

        self.finish_frame();
    }

    fn finish_frame(&mut self) {
        let egui::FullOutput {
            platform_output,
            textures_delta,
            shapes,
            pixels_per_point,
            ..
        } = self.egui_ctx.end_pass();

        self.egui_state
            .process_output(&self.window, &platform_output);

        let paint_jobs = self.egui_ctx.tessellate(shapes, pixels_per_point);
        self.painter.paint_jobs(None, textures_delta, paint_jobs);
        self.window.gl_swap_window();
    }

    /// Forward an SDL2 event to egui for input processing.
    pub fn process_event(&mut self, event: sdl2::event::Event) {
        self.egui_state
            .process_input(&self.window, event, &mut self.painter);
    }

    /// True if egui wants keyboard events (a text field is focused).
    pub fn wants_keyboard(&self) -> bool {
        self.egui_ctx.wants_keyboard_input()
    }

    /// Render vector lines via OpenGL, then run an egui pass for overlays.
    /// `display_size` is the vector coordinate space dimensions. `view_aspect`
    /// is the as-viewed display aspect ratio (width / height) the beam field is
    /// letterboxed into. `rotation` is screen-level rotation in degrees (0/270).
    pub fn present_vectors_with_overlay(
        &mut self,
        renderer: &mut crate::vector_gl::VectorRenderer,
        lines: &[phosphor_core::device::dvg::VectorLine],
        display_size: (u32, u32),
        view_aspect: f32,
        rotation: i32,
        overlay_fn: impl FnOnce(&egui::Context),
    ) {
        unsafe {
            gl::ClearColor(0.0, 0.0, 0.0, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
        }
        let (w, h) = self.window.size();
        renderer.render(
            lines,
            w,
            h,
            view_aspect,
            display_size.0,
            display_size.1,
            rotation,
        );

        // Run a minimal egui pass for overlay text on top of the vectors.
        self.egui_state.input.time = Some(self.start_time.elapsed().as_secs_f64());
        self.egui_ctx.begin_pass(self.egui_state.input.take());
        overlay_fn(&self.egui_ctx);
        self.finish_frame();
    }

    /// Return the current window position.
    pub fn window_position(&self) -> (i32, i32) {
        self.window.position()
    }

    /// Resize the window.
    pub fn resize_window(&mut self, width: u32, height: u32) {
        self.window
            .set_size(width, height)
            .expect("Failed to resize window");
    }
}
