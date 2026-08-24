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
    BEAM_CUTOFF_SIGMAS, MIN_SIGMA_PIXELS, VectorLine, beam_sigma_units,
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
    f_color = v_color;
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
    vertex_buf: Vec<Vertex>,
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

        Self {
            program,
            vao,
            vbo,
            uniform_half_size,
            uniform_rotation,
            uniform_inv_two_sigma_sq,
            // Six vertices per vector, and a busy frame runs to a couple of
            // thousand vectors.
            vertex_buf: Vec::with_capacity(16384),
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

        // Each segment becomes a quad covering the beam's reach around it: the
        // segment grown by the profile's cutoff in every direction, including
        // past the ends, where the round cap lives. The fragment shader does
        // the rest.
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

        if self.vertex_buf.is_empty() {
            return;
        }

        unsafe {
            gl::Viewport(vp_x, vp_y, vp_w as i32, vp_h as i32);
            gl::UseProgram(self.program);
            gl::Uniform2f(
                self.uniform_half_size,
                display_w as f32 / 2.0,
                display_h as f32 / 2.0,
            );
            gl::Uniform1i(self.uniform_rotation, rotation);
            gl::Uniform1f(self.uniform_inv_two_sigma_sq, 1.0 / (2.0 * sigma * sigma));
            gl::BindVertexArray(self.vao);

            // Upload vertex data.
            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
            gl::BufferData(
                gl::ARRAY_BUFFER,
                (self.vertex_buf.len() * mem::size_of::<Vertex>()) as gl::types::GLsizeiptr,
                self.vertex_buf.as_ptr() as *const _,
                gl::DYNAMIC_DRAW,
            );

            // Additive blending: where two beams cross, the light adds, and
            // the overlapping skirts of neighbouring vectors sum the way the
            // phosphor sums them.
            gl::Enable(gl::BLEND);
            gl::BlendFunc(gl::ONE, gl::ONE);

            // These are quads now, not lines. Whichever way a segment runs
            // decides the winding of its triangles, so culling would drop half
            // of them, and a depth test against whatever egui last left in the
            // buffer would drop the rest.
            gl::Disable(gl::CULL_FACE);
            gl::Disable(gl::DEPTH_TEST);

            // One quad per vector, two triangles each. Zero-length vectors are
            // in here too: their quad is a square and the profile makes it a
            // round dot, so bullets need no separate pass.
            gl::DrawArrays(gl::TRIANGLES, 0, self.vertex_buf.len() as i32);

            // Restore state for egui.
            gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
            gl::BindVertexArray(0);
            gl::UseProgram(0);
        }
    }
}

impl Drop for VectorRenderer {
    fn drop(&mut self) {
        unsafe {
            gl::DeleteBuffers(1, &self.vbo);
            gl::DeleteVertexArrays(1, &self.vao);
            gl::DeleteProgram(self.program);
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
    unsafe {
        let vs = compile_shader(VERTEX_SHADER_SRC, gl::VERTEX_SHADER);
        let fs = compile_shader(FRAGMENT_SHADER_SRC, gl::FRAGMENT_SHADER);

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
