//! OpenGL beam renderer for vector display machines (DVG/AVG).
//!
//! Each `VectorLine` is drawn as a quad covering the beam's reach around it,
//! with the fragment shader evaluating the beam profile at every fragment's
//! distance from the segment. So a vector has the width the tube's spot really
//! has, with soft edges and round ends, rather than being one hard pixel.
//! Blending is additive, so crossing beams and the overlapping skirts of
//! neighbouring vectors sum the way the phosphor sums them.
//!
//! This bypasses the CPU framebuffer entirely and draws at the window's native
//! resolution, which is also what lets it use the tube's real spot size: the
//! CPU rasterizer in `atari_dvg.rs` works at display-list resolution, where the
//! spot can fall below what the grid can represent. The two share their figures
//! (see `phosphor_core::device::dvg`) and the same peak convention, so they
//! agree about what they are drawing.

use std::ffi::CString;
use std::mem;
use std::ptr;

use phosphor_core::device::dvg::{
    BEAM_CUTOFF_SIGMAS, HALATION_FRACTION, MIN_SIGMA_PIXELS, VectorLine, beam_sigma_units,
    halation_sigma_units,
};

/// Intensity-to-brightness lookup table (4-bit, 0 = invisible).
/// Matches the table in `atari_dvg.rs` for identical visual output.
const INTENSITY_LUT: [f32; 16] = [
    0.0,
    20.0 / 255.0,
    40.0 / 255.0,
    60.0 / 255.0,
    80.0 / 255.0,
    100.0 / 255.0,
    120.0 / 255.0,
    140.0 / 255.0,
    160.0 / 255.0,
    175.0 / 255.0,
    190.0 / 255.0,
    205.0 / 255.0,
    220.0 / 255.0,
    232.0 / 255.0,
    244.0 / 255.0,
    1.0,
];

const VERTEX_SHADER_SRC: &str = r#"
#version 150
in vec2 position;
in vec4 segment;
in vec3 v_color;
out vec3 f_color;
out vec2 f_pos;
flat out vec4 f_segment;
uniform vec2 display_half_size;
uniform int rotation;
uniform float energy_scale;
void main() {
    // Vector coordinates: 0..display_size, Y=0 at bottom.
    // NDC: -1..1, Y=-1 at bottom (matches vector convention).
    vec2 ndc = (position / display_half_size) - 1.0;
    // Screen-level rotation for portrait vector monitors (270°).
    // Net transform: negate X to match AVG beam-to-screen mapping.
    if (rotation == 270) {
        ndc = vec2(ndc.x, -ndc.y);
    }
    gl_Position = vec4(ndc, 0.0, 1.0);
    // Halation takes its share out of the core, so the core is drawn scaled by
    // what is left; the reduced field the glow is built from gets all of it.
    f_color = v_color * energy_scale;
    // The corner's position interpolates to give each fragment its own place
    // in vector space, which is where the beam profile is evaluated.
    f_pos = position;
    f_segment = segment;
}
"#;

const FRAGMENT_SHADER_SRC: &str = r#"
#version 150
in vec3 f_color;
in vec2 f_pos;
flat in vec4 f_segment;
out vec4 color;
uniform float inv_two_sigma_sq;
void main() {
    // Distance from this fragment to the segment, not to the infinite line, so
    // the ends are round the way a round spot arriving and leaving is round.
    vec2 p0 = f_segment.xy;
    vec2 p1 = f_segment.zw;
    vec2 d = p1 - p0;
    float len_sq = dot(d, d);
    float t = (len_sq > 0.0) ? clamp(dot(f_pos - p0, d) / len_sq, 0.0, 1.0) : 0.0;
    vec2 e = f_pos - (p0 + d * t);

    // The profile peaks at the colour it was given, which is the same
    // convention the CPU rasterizer uses: a full-intensity vector reaches full
    // white along its centre and no further.
    color = vec4(f_color * exp(-dot(e, e) * inv_two_sigma_sq), 1.0);
}
"#;

/// Fullscreen pass: one oversized triangle generated from the vertex index, so
/// it needs no vertex buffer at all, just a bound (empty) VAO.
const FULLSCREEN_VERTEX_SRC: &str = r#"
#version 150
out vec2 uv;
void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    uv = p;
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}
"#;

/// One axis of the halation blur.
///
/// The taps are fixed and the weights come from a sigma uniform, so the
/// reduced-resolution target is sized to keep sigma near the radius this can
/// cover. Normalising by the weight sum makes the pass conserve energy whatever
/// sigma works out to, including at the edges where taps fall outside.
const HALO_BLUR_FRAGMENT_SRC: &str = r#"
#version 150
in vec2 uv;
out vec4 color;
uniform sampler2D src;
uniform vec2 tap_step;
uniform float inv_two_sigma_sq;
void main() {
    vec3 sum = vec3(0.0);
    float weight_sum = 0.0;
    for (int i = -12; i <= 12; i++) {
        float f = float(i);
        float w = exp(-f * f * inv_two_sigma_sq);
        sum += texture(src, uv + tap_step * f).rgb * w;
        weight_sum += w;
    }
    color = vec4(sum / weight_sum, 1.0);
}
"#;

/// Add the halation field over the core, scaled by the fraction of light that
/// took the long way out. Drawn with additive blending, so this is the `f*halo`
/// half of the composite; the core was already drawn scaled by `1 - f`.
const HALO_COMPOSITE_FRAGMENT_SRC: &str = r#"
#version 150
in vec2 uv;
out vec4 color;
uniform sampler2D src;
uniform float amount;
void main() {
    color = vec4(texture(src, uv).rgb * amount, 1.0);
}
"#;

/// Sigma to aim for in the reduced-resolution halation field, in its own pixels.
///
/// The blur covers 12 taps either side, so this keeps the profile comfortably
/// inside them while leaving the field small enough that two blur passes over it
/// are nothing.
const HALO_TARGET_SIGMA: f32 = 3.4;

/// Offscreen targets for the halation field: two, to ping-pong the separable
/// blur between.
struct HaloTargets {
    fbo: [gl::types::GLuint; 2],
    tex: [gl::types::GLuint; 2],
    width: i32,
    height: i32,
}

/// Per-vertex data: the quad corner and the segment it belongs to, both in
/// vector coordinates, plus the colour the beam peaks at.
///
/// All six vertices of a segment's quad carry the same segment and colour; only
/// the corner differs. That is what lets the fragment shader work out its own
/// distance from the beam's path.
#[repr(C)]
#[derive(Clone, Copy)]
struct Vertex {
    x: f32,
    y: f32,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    r: f32,
    g: f32,
    b: f32,
}

pub struct VectorRenderer {
    program: gl::types::GLuint,
    vao: gl::types::GLuint,
    vbo: gl::types::GLuint,
    uniform_half_size: gl::types::GLint,
    uniform_rotation: gl::types::GLint,
    uniform_inv_two_sigma_sq: gl::types::GLint,
    uniform_energy_scale: gl::types::GLint,
    vertex_buf: Vec<Vertex>,

    blur_program: gl::types::GLuint,
    blur_step: gl::types::GLint,
    blur_inv_two_sigma_sq: gl::types::GLint,
    composite_program: gl::types::GLuint,
    composite_amount: gl::types::GLint,
    /// Empty VAO for the fullscreen passes, which generate their own vertices.
    fullscreen_vao: gl::types::GLuint,
    /// `None` until sized, and left `None` if the targets cannot be made, in
    /// which case the beam still draws and only the glow is missing.
    halo: Option<HaloTargets>,
}

impl VectorRenderer {
    pub fn new() -> Self {
        let program = unsafe { create_shader_program() };
        let (vao, vbo) = unsafe { create_vertex_objects(program) };
        let uniform_half_size = unsafe {
            let name = std::ffi::CString::new("display_half_size").unwrap();
            gl::GetUniformLocation(program, name.as_ptr())
        };
        let uniform_rotation = unsafe {
            let name = std::ffi::CString::new("rotation").unwrap();
            gl::GetUniformLocation(program, name.as_ptr())
        };
        let uniform_inv_two_sigma_sq = unsafe {
            let name = std::ffi::CString::new("inv_two_sigma_sq").unwrap();
            gl::GetUniformLocation(program, name.as_ptr())
        };

        let uniform_energy_scale = unsafe {
            let name = std::ffi::CString::new("energy_scale").unwrap();
            gl::GetUniformLocation(program, name.as_ptr())
        };

        let (blur_program, composite_program, fullscreen_vao) = unsafe {
            let blur = link_program(FULLSCREEN_VERTEX_SRC, HALO_BLUR_FRAGMENT_SRC);
            let composite = link_program(FULLSCREEN_VERTEX_SRC, HALO_COMPOSITE_FRAGMENT_SRC);
            let mut vao = 0;
            gl::GenVertexArrays(1, &mut vao);
            (blur, composite, vao)
        };
        let uniform = |program, name: &str| unsafe {
            let name = std::ffi::CString::new(name).unwrap();
            gl::GetUniformLocation(program, name.as_ptr())
        };

        Self {
            program,
            vao,
            vbo,
            uniform_half_size,
            uniform_rotation,
            uniform_inv_two_sigma_sq,
            uniform_energy_scale,
            // Six vertices per vector, and a busy frame runs to a couple of
            // thousand vectors.
            vertex_buf: Vec::with_capacity(16384),
            blur_program,
            blur_step: uniform(blur_program, "tap_step"),
            blur_inv_two_sigma_sq: uniform(blur_program, "inv_two_sigma_sq"),
            composite_program,
            composite_amount: uniform(composite_program, "amount"),
            fullscreen_vao,
            halo: None,
        }
    }

    /// Make sure the halation targets exist at `w` by `h`, rebuilding them if
    /// the window has changed size. Leaves `self.halo` as `None` if the driver
    /// will not give us a complete framebuffer, which costs the glow and
    /// nothing else.
    fn ensure_halo_targets(&mut self, w: i32, h: i32) {
        if let Some(t) = &self.halo
            && t.width == w
            && t.height == h
        {
            return;
        }
        self.drop_halo_targets();

        unsafe {
            let mut fbo = [0u32; 2];
            let mut tex = [0u32; 2];
            gl::GenFramebuffers(2, fbo.as_mut_ptr());
            gl::GenTextures(2, tex.as_mut_ptr());

            for i in 0..2 {
                gl::BindTexture(gl::TEXTURE_2D, tex[i]);
                // Half float: the field is a sum of overlapping beams and would
                // clip long before the eye does at 8 bits per channel.
                gl::TexImage2D(
                    gl::TEXTURE_2D,
                    0,
                    gl::RGBA16F as i32,
                    w,
                    h,
                    0,
                    gl::RGBA,
                    gl::HALF_FLOAT,
                    ptr::null(),
                );
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
                // Clamp, so the blur's outermost taps do not wrap the glow
                // around to the far edge of the screen.
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);

                gl::BindFramebuffer(gl::FRAMEBUFFER, fbo[i]);
                gl::FramebufferTexture2D(
                    gl::FRAMEBUFFER,
                    gl::COLOR_ATTACHMENT0,
                    gl::TEXTURE_2D,
                    tex[i],
                    0,
                );
                if gl::CheckFramebufferStatus(gl::FRAMEBUFFER) != gl::FRAMEBUFFER_COMPLETE {
                    gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
                    gl::DeleteFramebuffers(2, fbo.as_ptr());
                    gl::DeleteTextures(2, tex.as_ptr());
                    return;
                }
            }
            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
            gl::BindTexture(gl::TEXTURE_2D, 0);

            self.halo = Some(HaloTargets {
                fbo,
                tex,
                width: w,
                height: h,
            });
        }
    }

    fn drop_halo_targets(&mut self) {
        if let Some(t) = self.halo.take() {
            unsafe {
                gl::DeleteFramebuffers(2, t.fbo.as_ptr());
                gl::DeleteTextures(2, t.tex.as_ptr());
            }
        }
    }

    /// Fill the vertex buffer with one quad per vector, each grown by `radius`
    /// so the beam profile has room to fall off inside it.
    fn build_quads(&mut self, lines: &[VectorLine], radius: f32) {
        self.vertex_buf.clear();
        for line in lines {
            if line.intensity == 0 {
                continue;
            }
            let brightness = INTENSITY_LUT[(line.intensity & 0xF) as usize];
            let (r, g, b) = (
                brightness * (line.r as f32 / 255.0),
                brightness * (line.g as f32 / 255.0),
                brightness * (line.b as f32 / 255.0),
            );

            let (x0, y0) = (line.x0 as f32, line.y0 as f32);
            let (x1, y1) = (line.x1 as f32, line.y1 as f32);
            let (dx, dy) = (x1 - x0, y1 - y0);
            let len = (dx * dx + dy * dy).sqrt();
            // A zero-length vector is a dot: the beam arrived and did not
            // travel. Any direction will do, the quad is square either way.
            let (ux, uy) = if len > 0.0 {
                (dx / len, dy / len)
            } else {
                (1.0, 0.0)
            };
            let (nx, ny) = (-uy, ux);

            let corner = |along: f32, across: f32, px: f32, py: f32| Vertex {
                x: px + ux * along + nx * across,
                y: py + uy * along + ny * across,
                x0,
                y0,
                x1,
                y1,
                r,
                g,
                b,
            };
            let a = corner(-radius, radius, x0, y0);
            let bb = corner(-radius, -radius, x0, y0);
            let c = corner(radius, -radius, x1, y1);
            let d = corner(radius, radius, x1, y1);

            self.vertex_buf.extend([a, bb, c, a, c, d]);
        }
    }

    /// Upload the current vertex buffer and draw it.
    ///
    /// # Safety
    /// A GL context must be current and `self.program` in use.
    unsafe fn draw_quads(&self, sigma: f32, energy_scale: f32) {
        unsafe {
            gl::Uniform1f(self.uniform_inv_two_sigma_sq, 1.0 / (2.0 * sigma * sigma));
            gl::Uniform1f(self.uniform_energy_scale, energy_scale);
            gl::BindVertexArray(self.vao);
            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
            gl::BufferData(
                gl::ARRAY_BUFFER,
                (self.vertex_buf.len() * mem::size_of::<Vertex>()) as gl::types::GLsizeiptr,
                self.vertex_buf.as_ptr() as *const _,
                gl::DYNAMIC_DRAW,
            );
            gl::DrawArrays(gl::TRIANGLES, 0, self.vertex_buf.len() as i32);
        }
    }

    /// Render vector lines directly to the current framebuffer.
    ///
    /// `viewport_w`/`viewport_h` are the full window size; the beam field is
    /// drawn into a centered sub-viewport of `view_aspect` (width / height) so
    /// pixel aspect is corrected and the field is letterboxed, not stretched to
    /// fill. `display_w`/`display_h` are the vector coordinate space dimensions
    /// (e.g. 1024×1024 for DVG, 580×570 for Tempest AVG).
    /// `rotation` is the screen-level rotation in degrees (0 or 270).
    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        lines: &[VectorLine],
        viewport_w: u32,
        viewport_h: u32,
        view_aspect: f32,
        display_w: u32,
        display_h: u32,
        rotation: i32,
    ) {
        // Centered sub-viewport at the target display aspect (letterbox the
        // beam field rather than stretch it to fill the window).
        let (win_w, win_h) = (viewport_w as f32, viewport_h as f32);
        let (vp_w, vp_h) = if win_w / win_h > view_aspect {
            (win_h * view_aspect, win_h)
        } else {
            (win_w, win_w / view_aspect)
        };
        let vp_x = ((win_w - vp_w) / 2.0) as i32;
        let vp_y = ((win_h - vp_h) / 2.0) as i32;

        // The spot, in the generator's own units.
        //
        // Unlike the CPU rasterizer, this draws at window resolution rather
        // than display-list resolution, so a unit is usually several pixels and
        // the grid can carry the tube's real spot size. The floor still applies
        // when the window is small enough that it cannot, and it is expressed
        // in units here by converting through the pixels-per-unit of the
        // viewport we just worked out.
        let long_axis = display_w.max(display_h) as f32;
        let px_per_unit = vp_w.max(vp_h) / long_axis;
        let sigma =
            beam_sigma_units(long_axis).max(MIN_SIGMA_PIXELS / px_per_unit.max(f32::MIN_POSITIVE));
        let radius = BEAM_CUTOFF_SIGMAS * sigma;

        // Halation, sized in this viewport's pixels. The reduced field is scaled
        // so the skirt comes out near the blur's tap count whatever the window
        // size, which is what keeps the blur a fixed, cheap kernel.
        let halo_sigma_px = halation_sigma_units(long_axis) * px_per_unit;
        let down = (halo_sigma_px / HALO_TARGET_SIGMA).round().max(1.0);
        let small_w = ((vp_w / down).ceil() as i32).max(1);
        let small_h = ((vp_h / down).ceil() as i32).max(1);
        self.ensure_halo_targets(small_w, small_h);

        // The source for the glow is the same beam drawn into the reduced
        // field, where the core spot is far under a pixel and would alias into
        // flicker. Widen it to what that grid can hold: the skirt it is about to
        // be blurred by is so much broader that the difference disappears into
        // it (30.1 against 29 units on Tempest).
        let units_per_small_px = down / px_per_unit;
        let source_sigma = sigma.max(MIN_SIGMA_PIXELS * units_per_small_px);
        let source_radius = BEAM_CUTOFF_SIGMAS * source_sigma;

        // Halation is light that left by way of the faceplate rather than
        // straight out, so it comes out of the core rather than adding to it.
        // With no targets to draw it into there is no glow, and then the core
        // keeps all of its light.
        let (core_scale, halo_amount) = match self.halo {
            Some(_) => (1.0 - HALATION_FRACTION, HALATION_FRACTION),
            None => (1.0, 0.0),
        };

        let half_size = (display_w as f32 / 2.0, display_h as f32 / 2.0);

        // Pass one: the glow's source, into the reduced field.
        if halo_amount > 0.0 {
            self.build_quads(lines, source_radius);
            if !self.vertex_buf.is_empty()
                && let Some(t) = self.halo.as_ref()
            {
                let (fbo, tex, sw, sh) = (t.fbo, t.tex, t.width, t.height);
                unsafe {
                    gl::Disable(gl::CULL_FACE);
                    gl::Disable(gl::DEPTH_TEST);
                    gl::BindFramebuffer(gl::FRAMEBUFFER, fbo[0]);
                    gl::Viewport(0, 0, sw, sh);
                    gl::ClearColor(0.0, 0.0, 0.0, 1.0);
                    gl::Clear(gl::COLOR_BUFFER_BIT);
                    gl::Enable(gl::BLEND);
                    gl::BlendFunc(gl::ONE, gl::ONE);

                    gl::UseProgram(self.program);
                    gl::Uniform2f(self.uniform_half_size, half_size.0, half_size.1);
                    gl::Uniform1i(self.uniform_rotation, rotation);
                    self.draw_quads(source_sigma, 1.0);

                    // Separable blur, ping-ponging between the two targets. The
                    // passes replace rather than add, so blending is off.
                    gl::Disable(gl::BLEND);
                    gl::UseProgram(self.blur_program);
                    let small_sigma = halo_sigma_px / down;
                    gl::Uniform1f(
                        self.blur_inv_two_sigma_sq,
                        1.0 / (2.0 * small_sigma * small_sigma),
                    );
                    gl::BindVertexArray(self.fullscreen_vao);
                    gl::ActiveTexture(gl::TEXTURE0);

                    for (pass, step) in [
                        (1usize, (1.0 / sw as f32, 0.0)),
                        (0, (0.0, 1.0 / sh as f32)),
                    ] {
                        gl::BindFramebuffer(gl::FRAMEBUFFER, fbo[pass]);
                        gl::BindTexture(gl::TEXTURE_2D, tex[1 - pass]);
                        gl::Uniform2f(self.blur_step, step.0, step.1);
                        gl::DrawArrays(gl::TRIANGLES, 0, 3);
                    }

                    gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
                }
            }
        }

        // Pass two: the core, straight to the screen as before.
        self.build_quads(lines, radius);
        if self.vertex_buf.is_empty() {
            return;
        }

        unsafe {
            gl::Viewport(vp_x, vp_y, vp_w as i32, vp_h as i32);
            gl::UseProgram(self.program);
            gl::Uniform2f(self.uniform_half_size, half_size.0, half_size.1);
            gl::Uniform1i(self.uniform_rotation, rotation);

            // Additive blending: where two beams cross, the light adds, and
            // the overlapping skirts of neighbouring vectors sum the way the
            // phosphor sums them.
            gl::Enable(gl::BLEND);
            gl::BlendFunc(gl::ONE, gl::ONE);

            // These are quads, not lines. Whichever way a segment runs decides
            // the winding of its triangles, so culling would drop half of them,
            // and a depth test against whatever egui last left in the buffer
            // would drop the rest.
            gl::Disable(gl::CULL_FACE);
            gl::Disable(gl::DEPTH_TEST);

            // One quad per vector, two triangles each. Zero-length vectors are
            // in here too: their quad is a square and the profile makes it a
            // round dot, so bullets need no separate pass.
            self.draw_quads(sigma, core_scale);

            // Pass three: add the glow over it.
            if halo_amount > 0.0
                && let Some(t) = self.halo.as_ref()
            {
                gl::UseProgram(self.composite_program);
                gl::Uniform1f(self.composite_amount, halo_amount);
                gl::BindVertexArray(self.fullscreen_vao);
                gl::ActiveTexture(gl::TEXTURE0);
                gl::BindTexture(gl::TEXTURE_2D, t.tex[0]);
                gl::DrawArrays(gl::TRIANGLES, 0, 3);
                gl::BindTexture(gl::TEXTURE_2D, 0);
            }

            // Restore state for egui.
            gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
            gl::BindVertexArray(0);
            gl::UseProgram(0);
        }
    }
}

impl Drop for VectorRenderer {
    fn drop(&mut self) {
        self.drop_halo_targets();
        unsafe {
            gl::DeleteBuffers(1, &self.vbo);
            gl::DeleteVertexArrays(1, &self.vao);
            gl::DeleteVertexArrays(1, &self.fullscreen_vao);
            gl::DeleteProgram(self.program);
            gl::DeleteProgram(self.blur_program);
            gl::DeleteProgram(self.composite_program);
        }
    }
}

// ---------------------------------------------------------------------------
// GL helpers
// ---------------------------------------------------------------------------

unsafe fn compile_shader(src: &str, shader_type: gl::types::GLenum) -> gl::types::GLuint {
    unsafe {
        let shader = gl::CreateShader(shader_type);
        let c_src = CString::new(src).unwrap();
        gl::ShaderSource(shader, 1, &c_src.as_ptr(), ptr::null());
        gl::CompileShader(shader);

        let mut success = gl::FALSE as gl::types::GLint;
        gl::GetShaderiv(shader, gl::COMPILE_STATUS, &mut success);
        if success != gl::TRUE as gl::types::GLint {
            let mut len = 0;
            gl::GetShaderiv(shader, gl::INFO_LOG_LENGTH, &mut len);
            let mut buf = vec![0u8; len as usize];
            gl::GetShaderInfoLog(shader, len, ptr::null_mut(), buf.as_mut_ptr() as *mut _);
            let msg = String::from_utf8_lossy(&buf);
            panic!("Shader compilation failed: {msg}");
        }
        shader
    }
}

unsafe fn create_shader_program() -> gl::types::GLuint {
    unsafe { link_program(VERTEX_SHADER_SRC, FRAGMENT_SHADER_SRC) }
}

unsafe fn link_program(vertex_src: &str, fragment_src: &str) -> gl::types::GLuint {
    unsafe {
        let vs = compile_shader(vertex_src, gl::VERTEX_SHADER);
        let fs = compile_shader(fragment_src, gl::FRAGMENT_SHADER);

        let program = gl::CreateProgram();
        gl::AttachShader(program, vs);
        gl::AttachShader(program, fs);
        gl::LinkProgram(program);

        let mut success = gl::FALSE as gl::types::GLint;
        gl::GetProgramiv(program, gl::LINK_STATUS, &mut success);
        if success != gl::TRUE as gl::types::GLint {
            let mut len = 0;
            gl::GetProgramiv(program, gl::INFO_LOG_LENGTH, &mut len);
            let mut buf = vec![0u8; len as usize];
            gl::GetProgramInfoLog(program, len, ptr::null_mut(), buf.as_mut_ptr() as *mut _);
            let msg = String::from_utf8_lossy(&buf);
            panic!("Shader link failed: {msg}");
        }

        gl::DeleteShader(vs);
        gl::DeleteShader(fs);
        program
    }
}

unsafe fn create_vertex_objects(
    program: gl::types::GLuint,
) -> (gl::types::GLuint, gl::types::GLuint) {
    unsafe {
        let mut vao = 0;
        gl::GenVertexArrays(1, &mut vao);
        gl::BindVertexArray(vao);

        let mut vbo = 0;
        gl::GenBuffers(1, &mut vbo);
        gl::BindBuffer(gl::ARRAY_BUFFER, vbo);

        let stride = mem::size_of::<Vertex>() as gl::types::GLsizei;

        // position (vec2): offset 0
        let pos_attr = gl::GetAttribLocation(program, c"position".as_ptr());
        if pos_attr >= 0 {
            gl::EnableVertexAttribArray(pos_attr as u32);
            gl::VertexAttribPointer(
                pos_attr as u32,
                2,
                gl::FLOAT,
                gl::FALSE,
                stride,
                ptr::null(),
            );
        }

        // segment (vec4): offset 8
        let seg_attr = gl::GetAttribLocation(program, c"segment".as_ptr());
        if seg_attr >= 0 {
            gl::EnableVertexAttribArray(seg_attr as u32);
            gl::VertexAttribPointer(
                seg_attr as u32,
                4,
                gl::FLOAT,
                gl::FALSE,
                stride,
                (2 * mem::size_of::<f32>()) as *const _,
            );
        }

        // color (vec3): offset 24
        let color_attr = gl::GetAttribLocation(program, c"v_color".as_ptr());
        if color_attr >= 0 {
            gl::EnableVertexAttribArray(color_attr as u32);
            gl::VertexAttribPointer(
                color_attr as u32,
                3,
                gl::FLOAT,
                gl::FALSE,
                stride,
                (6 * mem::size_of::<f32>()) as *const _,
            );
        }

        gl::BindVertexArray(0);
        (vao, vbo)
    }
}
