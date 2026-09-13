use std::time::Instant;

use egui_backend::painter::Painter;
use egui_backend::{DpiScaling, EguiStateHandler, ShaderVersion};
use egui_sdl2_gl as egui_backend;
use phosphor_core::core::machine::Orientation;
use sdl2::video::{GLContext, GLProfile, Window};

pub struct Video {
    window: Window,
    _gl_ctx: GLContext,
    painter: Painter,
    egui_state: EguiStateHandler,
    egui_ctx: egui::Context,
    game_texture_id: egui::TextureId,
    /// The raster `render_frame` fills, which is what the CRT stage samples.
    native_width: u32,
    native_height: u32,
    /// The picture as displayed: the native pair with the axes swapped when the
    /// machine's orientation swaps them. The game texture and the CRT stage's
    /// framebuffer are this size.
    display_width: u32,
    display_height: u32,
    rgba_buffer: Vec<u8>,
    /// Scratch for the CPU fallback's orientation pass. Untouched while the CRT
    /// stage is carrying the picture.
    oriented: Vec<u8>,
    crt: crate::crt_gl::CrtRenderer,
    /// Whether the CRT stage is carrying the picture, and whether that has been
    /// said. A pass-through stage draws exactly what the direct upload drew, so
    /// a stage that silently never engaged is indistinguishable from one that
    /// works. Say each transition once; these are standing conditions, not
    /// per-frame events.
    crt_active: bool,
    crt_fallback_reported: bool,
    start_time: Instant,
    fullscreen: bool,
}

impl Video {
    /// Create a new Video with separate native, texture and window dimensions.
    ///
    /// `native_width`/`native_height` are the raster the machine renders, which
    /// the CRT stage samples. `display_width`/`display_height` are that raster as
    /// displayed, which sizes the game texture. They differ whenever the
    /// machine's orientation swaps its axes. `window_width`/`window_height`
    /// define the initial window size, which additionally carries the tube's
    /// aspect correction.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sdl_video: &sdl2::VideoSubsystem,
        title: &str,
        native_width: u32,
        native_height: u32,
        display_width: u32,
        display_height: u32,
        window_width: u32,
        window_height: u32,
        scale: u32,
        position: Option<(i32, i32)>,
        fullscreen: bool,
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
        // Desktop fullscreen: borderless at the display's native resolution. The
        // presentation letterbox (egui central panel + fit_aspect) fills it at
        // the correct aspect with black bars, so no window-size math is needed.
        if fullscreen {
            builder.fullscreen_desktop();
        }
        let window = builder.build().expect("Failed to create window");

        let gl_ctx = window
            .gl_create_context()
            .expect("Failed to create GL context");

        let (mut painter, egui_state) =
            egui_backend::with_sdl2(&window, ShaderVersion::Default, DpiScaling::Default);
        let egui_ctx = egui::Context::default();

        // Create initial game texture (black with full alpha)
        let pixel_count = (display_width * display_height) as usize;
        let mut rgba_buffer = vec![0u8; pixel_count * 4];
        for chunk in rgba_buffer.as_chunks_mut::<4>().0 {
            chunk[3] = 255;
        }
        let game_texture_id = painter.new_user_texture_rgba8(
            (display_width as usize, display_height as usize),
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
            display_width,
            display_height,
            rgba_buffer,
            oriented: vec![0u8; pixel_count * 3],
            crt: crate::crt_gl::CrtRenderer::new(),
            crt_active: false,
            crt_fallback_reported: false,
            start_time: Instant::now(),
            fullscreen,
        }
    }

    /// Put the machine's *native* RGB24 frame into the texture egui draws,
    /// applying `orientation` on the way.
    ///
    /// Normally this runs the frame through [`crate::crt_gl::CrtRenderer`],
    /// which renders into that texture rather than uploading pixels into it. The
    /// CPU path below is the fallback for two cases: the painter has not yet
    /// allocated the texture, which is true until it has painted once, and a
    /// framebuffer that will not complete.
    ///
    /// Once the CRT stage takes over, the texture must never be marked dirty
    /// again: `Painter::upload_user_textures` re-uploads a dirty texture's CPU
    /// pixels at the start of the next paint, which would overwrite what the
    /// stage rendered. Uploading is therefore the fallback's job alone.
    ///
    /// The fallback orients through `phosphor_core::gfx::apply_orientation`,
    /// which is the same function the harness applies when it hashes a frame.
    /// That makes it the reference rather than a second implementation: the
    /// shader's transform is the inverse of this one, and the two agreeing is
    /// what the fallback is worth beyond robustness.
    pub fn update_game_texture(&mut self, native_rgb24: &[u8], orientation: Orientation) {
        let native_pixels = (self.native_width * self.native_height) as usize;
        debug_assert_eq!(native_rgb24.len(), native_pixels * 3);

        if let Some(out_tex) = self.painter.get_raw_gl_texture_id(&self.game_texture_id)
            && self.crt.present(
                native_rgb24,
                (self.native_width, self.native_height),
                (self.display_width, self.display_height),
                orientation,
                out_tex,
            )
        {
            if !self.crt_active {
                self.crt_active = true;
                eprintln!("CRT stage active: drawing through the presentation framebuffer");
            }
            return;
        }

        if self.crt_active && !self.crt_fallback_reported {
            self.crt_fallback_reported = true;
            eprintln!("CRT stage unavailable: falling back to a CPU orient and upload");
        }

        let displayed: &[u8] = if orientation == Orientation::NORMAL {
            native_rgb24
        } else {
            phosphor_core::gfx::apply_orientation(
                native_rgb24,
                &mut self.oriented,
                self.native_width as usize,
                self.native_height as usize,
                orientation,
            );
            &self.oriented
        };

        for i in 0..(self.display_width * self.display_height) as usize {
            self.rgba_buffer[i * 4] = displayed[i * 3];
            self.rgba_buffer[i * 4 + 1] = displayed[i * 3 + 1];
            self.rgba_buffer[i * 4 + 2] = displayed[i * 3 + 2];
            // alpha stays 255 from initialization
        }

        self.painter
            .update_user_texture_rgba8_data(self.game_texture_id, self.rgba_buffer.clone());
    }

    /// Render the game at the target display `aspect` (no debug panels),
    /// letterboxed with black bars when the window doesn't match the aspect.
    ///
    /// `overlay_fn` draws the FPS / PAUSED layer over the picture. It runs
    /// inside the same egui pass, which is also what keeps input events drained
    /// on the frames where it draws nothing.
    pub fn present_game_only(&mut self, aspect: f32, overlay_fn: impl FnOnce(&egui::Context)) {
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
        overlay_fn(&self.egui_ctx);

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

    /// Usable width of the display the window is on, if SDL can report it.
    ///
    /// Used to cap panel-driven window growth: a window wider than the screen
    /// puts the extra columns somewhere the user cannot reach, which is worse
    /// than letting the panel's own scrollbars handle the overflow.
    pub fn display_width(&self) -> Option<u32> {
        let subsystem = self.window.subsystem();
        let index = self.window.display_index().ok()?;
        let bounds = subsystem.display_usable_bounds(index).ok()?;
        Some(bounds.width())
    }

    /// Resize the window to make room for side panels. No-op in fullscreen,
    /// where the window stays at the display resolution and panels simply
    /// subtract from the central game area.
    pub fn resize_window(&mut self, width: u32, height: u32) {
        if self.fullscreen {
            return;
        }
        self.window
            .set_size(width, height)
            .expect("Failed to resize window");
    }
}
