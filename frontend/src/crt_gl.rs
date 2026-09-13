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
//! # The stage owns the cabinet's rotation
//!
//! The source texture is the machine's *native* raster, the one `render_frame`
//! fills, and the pass turns it into the displayed image. That is the whole
//! reason this is worth building: a scanline has to be derived along the tube's
//! line axis, and on a rotated cabinet that is not the screen's. Rotating on the
//! CPU first, as the frontend used to, would hand the beam model a texture whose
//! rows are columns on the 22 raster machines with a turned monitor.
//!
//! It is also one rotation rather than two. A double-applied ROT270 is what
//! `phosphor-emulator-iitc` was, and the shape that allows it is a transform
//! living in two places.
//!
//! The beam profile itself is still to come: this samples the source and writes
//! it out unchanged apart from the orientation, so the picture matches what the
//! CPU rotate and direct upload produced.

use std::ffi::CString;

use phosphor_core::core::machine::Orientation;

use crate::vector_gl::{FULLSCREEN_VERTEX_SRC, link_program};

/// Orient, sample, write. The beam model goes between the sample and the write;
/// everything around it stays as it is.
///
/// The transform is the inverse of `phosphor_core::gfx::apply_orientation`,
/// which maps a source pixel forward to its destination and is the reference a
/// fragment shader has to run backwards. Inverting its three cases gives, for an
/// output coordinate pair scaled to 0..1, a mirror on each flipped axis followed
/// by a component swap, in that order. Mirroring in normalized coordinates lands
/// exactly on texel centers: `1 - (i + 0.5)/n` is `((n - 1 - i) + 0.5)/n`.
const ORIENT_FRAGMENT_SRC: &str = r#"
#version 150
in vec2 uv;
out vec4 color;
uniform sampler2D src;
uniform vec2 flip;
uniform bool swap_xy;
void main() {
    vec2 c = mix(uv, vec2(1.0) - uv, flip);
    color = vec4(texture(src, swap_xy ? c.yx : c.xy).rgb, 1.0);
}
"#;

pub struct CrtRenderer {
    program: gl::types::GLuint,
    vao: gl::types::GLuint,
    src_uniform: gl::types::GLint,
    flip_uniform: gl::types::GLint,
    swap_uniform: gl::types::GLint,
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
            let program = link_program(FULLSCREEN_VERTEX_SRC, ORIENT_FRAGMENT_SRC);
            let uniform = |name: &str| {
                let name = CString::new(name).expect("literal has no interior nul");
                gl::GetUniformLocation(program, name.as_ptr())
            };
            let src_uniform = uniform("src");
            let flip_uniform = uniform("flip");
            let swap_uniform = uniform("swap_xy");

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
                flip_uniform,
                swap_uniform,
                src_tex,
                src_size: (0, 0),
                fbo,
                attached: None,
            }
        }
    }

    /// Render the machine's native frame into `out_tex`, the texture egui draws,
    /// applying `orientation` on the way.
    ///
    /// `src` is the size of the buffer `render_frame` fills and `dst` is the
    /// displayed size, which is `src` with the axes swapped when the orientation
    /// swaps them.
    ///
    /// Returns false when the framebuffer will not complete, leaving `out_tex`
    /// untouched so the caller can fall back to orienting and uploading on the
    /// CPU. A driver that refuses the attachment should cost the picture, not the
    /// session.
    pub fn present(
        &mut self,
        rgb24: &[u8],
        src: (u32, u32),
        dst: (u32, u32),
        orientation: Orientation,
        out_tex: gl::types::GLuint,
    ) -> bool {
        let (src_w, src_h) = src;
        let (dst_w, dst_h) = dst;
        debug_assert_eq!(rgb24.len(), (src_w as usize) * (src_h as usize) * 3);
        debug_assert_eq!(
            if orientation.swaps_axes() {
                (src_h, src_w)
            } else {
                (src_w, src_h)
            },
            dst,
            "displayed size must be the native size under the declared orientation"
        );
        unsafe {
            if !self.attach(out_tex) {
                return false;
            }
            self.upload_source(rgb24, src_w, src_h);

            let mut viewport = [0i32; 4];
            gl::GetIntegerv(gl::VIEWPORT, viewport.as_mut_ptr());

            gl::BindFramebuffer(gl::FRAMEBUFFER, self.fbo);
            gl::Viewport(0, 0, dst_w as i32, dst_h as i32);
            // egui leaves these on from its own pass. None of them belong in a
            // straight copy, and paint_jobs re-enables what it needs next frame.
            gl::Disable(gl::SCISSOR_TEST);
            gl::Disable(gl::BLEND);
            gl::Disable(gl::FRAMEBUFFER_SRGB);

            gl::UseProgram(self.program);
            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, self.src_tex);
            gl::Uniform1i(self.src_uniform, 0);
            gl::Uniform2f(
                self.flip_uniform,
                orientation.flip_x() as i32 as f32,
                orientation.flip_y() as i32 as f32,
            );
            gl::Uniform1i(self.swap_uniform, orientation.swaps_axes() as i32);

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

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::gfx::apply_orientation;

    /// `ORIENT_FRAGMENT_SRC`'s transform, written out in Rust.
    ///
    /// Kept line for line equivalent to the shader so the mapping can be checked
    /// without a GL context. Nearest filtering makes the sampled texel the floor
    /// of the scaled coordinate, which is what the indexing here reproduces.
    fn shader_source_texel(
        out: (u32, u32),
        dst: (u32, u32),
        src: (u32, u32),
        o: Orientation,
    ) -> (u32, u32) {
        let u = (out.0 as f32 + 0.5) / dst.0 as f32;
        let v = (out.1 as f32 + 0.5) / dst.1 as f32;
        let c = (
            if o.flip_x() { 1.0 - u } else { u },
            if o.flip_y() { 1.0 - v } else { v },
        );
        let (s, t) = if o.swaps_axes() {
            (c.1, c.0)
        } else {
            (c.0, c.1)
        };
        (
            (s * src.0 as f32).floor() as u32,
            (t * src.1 as f32).floor() as u32,
        )
    }

    /// The shader runs `apply_orientation` backwards: the CPU function maps a
    /// source pixel forward to where it lands, a fragment shader asks what lands
    /// on a given output pixel. Getting that inverse subtly wrong is how a
    /// picture comes out mirrored rather than obviously broken, and it is the
    /// same class of error as the double-applied ROT270 in
    /// `phosphor-emulator-iitc`, so pin it against the real function.
    ///
    /// Non-square and odd dimensions on purpose: a square source hides a
    /// transpose and an even one hides some off-by-ones.
    #[test]
    fn the_shader_transform_inverts_apply_orientation() {
        const W: usize = 7;
        const H: usize = 5;

        // Every pixel distinguishable, so a wrong source lands on a wrong value.
        let mut src = vec![0u8; W * H * 3];
        for y in 0..H {
            for x in 0..W {
                let i = (y * W + x) * 3;
                src[i] = (x + 1) as u8;
                src[i + 1] = (y + 1) as u8;
                src[i + 2] = 0x5A;
            }
        }

        // All eight flag combinations, not just the named rotations: `compose`
        // can produce any of them from a cocktail flip over a rotated cabinet.
        for bits in 0..8u8 {
            let o = Orientation::from_bits(bits);
            let (dw, dh) = if o.swaps_axes() { (H, W) } else { (W, H) };

            let mut dst = vec![0u8; dw * dh * 3];
            apply_orientation(&src, &mut dst, W, H, o);

            for oy in 0..dh {
                for ox in 0..dw {
                    let (sx, sy) = shader_source_texel(
                        (ox as u32, oy as u32),
                        (dw as u32, dh as u32),
                        (W as u32, H as u32),
                        o,
                    );
                    let sampled = &src[(sy as usize * W + sx as usize) * 3..][..3];
                    let expected = &dst[(oy * dw + ox) * 3..][..3];
                    assert_eq!(
                        sampled,
                        expected,
                        "orientation {:#05b} at output ({ox},{oy}): shader reads source \
                         ({sx},{sy}), which is not what apply_orientation put there",
                        o.bits()
                    );
                }
            }
        }
    }
}
