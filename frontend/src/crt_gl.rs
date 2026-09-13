//! The CRT presentation stage for the raster machines.
//!
//! A raster machine's frame reaches the screen as a texture handed to egui,
//! which lays the debug panels out around it. That leaves no stage of ours
//! anywhere on the path: `ui.image` draws the texture and nothing else happens
//! to it, so there is nowhere a beam model could live.
//!
//! This inserts one. The machine's frame goes into a texture this module owns, a
//! shader pass reads it, and the pass writes into a framebuffer object whose
//! color attachment is the texture egui draws. The picture is therefore finished
//! before egui sees it, which is what lets an effect exist at all, and it is
//! still a texture, which is what keeps the panel layout working.
//!
//! Drawing through an FBO rather than straight at the window is the point of the
//! arrangement rather than an implementation detail. The vector renderer draws to
//! the window and so has no texture to give egui, which is the only reason an
//! open debug panel drops that machine back onto the CPU rasterizer. The same
//! FBO handoff removes that fallback; see `phosphor-emulator-pu41`.
//!
//! # This stage is a pass-through
//!
//! It samples the source and writes it out unchanged, so the picture is
//! identical to the one the direct upload produced. That is deliberate: the
//! plumbing lands first and provably moves no pixel, and the beam profile,
//! scanlines and halation arrive on top of it. A change that rewires the
//! presentation path *and* changes what is drawn gives nothing to bisect when
//! the picture is wrong.
//!
//! Rotation is also still applied on the CPU before the upload, as it was. It
//! moves onto the GPU with the beam profile, because scanlines have to be
//! derived in the tube's axes rather than the screen's, and that move takes the
//! FPS overlay with it.

use std::ffi::CString;

use crate::vector_gl::{FULLSCREEN_VERTEX_SRC, link_program};

/// Sample and write. The beam model replaces this body; everything around it
/// stays as it is.
const PASSTHROUGH_FRAGMENT_SRC: &str = r#"
#version 150
in vec2 uv;
out vec4 color;
uniform sampler2D src;
void main() {
    color = vec4(texture(src, uv).rgb, 1.0);
}
"#;

pub struct CrtRenderer {
    program: gl::types::GLuint,
    vao: gl::types::GLuint,
    src_uniform: gl::types::GLint,
    /// The machine's frame as uploaded each frame. Owned here.
    src_tex: gl::types::GLuint,
    src_size: (u32, u32),
    /// The framebuffer the pass draws through. Owned here; its color attachment
    /// is not, so dropping this deletes the framebuffer and leaves the texture.
    fbo: gl::types::GLuint,
    /// The texture egui draws, borrowed from the painter and attached to `fbo`.
    /// Tracked only to avoid re-attaching an unchanged one every frame.
    attached: Option<gl::types::GLuint>,
}

impl CrtRenderer {
    pub fn new() -> Self {
        unsafe {
            let program = link_program(FULLSCREEN_VERTEX_SRC, PASSTHROUGH_FRAGMENT_SRC);
            let name = CString::new("src").expect("literal has no interior nul");
            let src_uniform = gl::GetUniformLocation(program, name.as_ptr());

            let mut vao = 0;
            gl::GenVertexArrays(1, &mut vao);

            let mut src_tex = 0;
            gl::GenTextures(1, &mut src_tex);
            gl::BindTexture(gl::TEXTURE_2D, src_tex);
            // Nearest, for the same reason the direct upload used it: the source
            // is an exact pixel grid and nothing here wants it interpolated. The
            // beam profile will do its own sampling from these texels.
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
            gl::BindTexture(gl::TEXTURE_2D, 0);

            let mut fbo = 0;
            gl::GenFramebuffers(1, &mut fbo);

            Self {
                program,
                vao,
                src_uniform,
                src_tex,
                src_size: (0, 0),
                fbo,
                attached: None,
            }
        }
    }

    /// Render `rgb24` into `out_tex`, the texture egui will draw.
    ///
    /// Returns false when the framebuffer will not complete, leaving `out_tex`
    /// untouched so the caller can fall back to uploading pixels into it
    /// directly. A driver that refuses the attachment should cost the picture,
    /// not the session.
    pub fn present(
        &mut self,
        rgb24: &[u8],
        width: u32,
        height: u32,
        out_tex: gl::types::GLuint,
    ) -> bool {
        debug_assert_eq!(rgb24.len(), (width as usize) * (height as usize) * 3);
        unsafe {
            if !self.attach(out_tex) {
                return false;
            }
            self.upload_source(rgb24, width, height);

            let mut viewport = [0i32; 4];
            gl::GetIntegerv(gl::VIEWPORT, viewport.as_mut_ptr());

            gl::BindFramebuffer(gl::FRAMEBUFFER, self.fbo);
            gl::Viewport(0, 0, width as i32, height as i32);
            // egui leaves these on from its own pass. None of them belong in a
            // straight copy, and paint_jobs re-enables what it needs next frame.
            gl::Disable(gl::SCISSOR_TEST);
            gl::Disable(gl::BLEND);
            gl::Disable(gl::FRAMEBUFFER_SRGB);

            gl::UseProgram(self.program);
            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, self.src_tex);
            gl::Uniform1i(self.src_uniform, 0);

            // One oversized triangle, its vertices computed from gl_VertexID, so
            // there is no vertex buffer to keep. The bound VAO is still required
            // by the core profile even when it feeds nothing.
            gl::BindVertexArray(self.vao);
            gl::DrawArrays(gl::TRIANGLES, 0, 3);
            gl::BindVertexArray(0);

            gl::BindTexture(gl::TEXTURE_2D, 0);
            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
            gl::Viewport(viewport[0], viewport[1], viewport[2], viewport[3]);
        }
        true
    }

    /// Point the framebuffer at the texture egui draws, if it is not already.
    unsafe fn attach(&mut self, out_tex: gl::types::GLuint) -> bool {
        if self.attached == Some(out_tex) {
            return true;
        }
        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER, self.fbo);
            gl::FramebufferTexture2D(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::TEXTURE_2D,
                out_tex,
                0,
            );
            let complete = gl::CheckFramebufferStatus(gl::FRAMEBUFFER) == gl::FRAMEBUFFER_COMPLETE;
            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
            self.attached = complete.then_some(out_tex);
            complete
        }
    }

    unsafe fn upload_source(&mut self, rgb24: &[u8], width: u32, height: u32) {
        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, self.src_tex);
            // RGB rows are three bytes a pixel, so a row is only 4-byte aligned
            // for particular widths. Say so rather than depending on which.
            gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);
            if self.src_size == (width, height) {
                gl::TexSubImage2D(
                    gl::TEXTURE_2D,
                    0,
                    0,
                    0,
                    width as i32,
                    height as i32,
                    gl::RGB,
                    gl::UNSIGNED_BYTE,
                    rgb24.as_ptr().cast(),
                );
            } else {
                gl::TexImage2D(
                    gl::TEXTURE_2D,
                    0,
                    gl::RGB8 as i32,
                    width as i32,
                    height as i32,
                    0,
                    gl::RGB,
                    gl::UNSIGNED_BYTE,
                    rgb24.as_ptr().cast(),
                );
                self.src_size = (width, height);
            }
            gl::PixelStorei(gl::UNPACK_ALIGNMENT, 4);
        }
    }
}

impl Drop for CrtRenderer {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteFramebuffers(1, &self.fbo);
            gl::DeleteTextures(1, &self.src_tex);
            gl::DeleteVertexArrays(1, &self.vao);
            gl::DeleteProgram(self.program);
        }
    }
}
