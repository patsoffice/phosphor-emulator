//! OpenGL line renderer for vector display machines (DVG/AVG).
//!
//! Renders `VectorLine` segments directly as GL_LINES with additive blending,
//! bypassing the CPU framebuffer entirely. Lines are drawn at the window's
//! native resolution, so scaling has zero performance impact.

use std::ffi::CString;
use std::mem;
use std::ptr;

use phosphor_core::device::dvg::VectorLine;

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
in vec3 v_color;
out vec3 f_color;
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
}
"#;

const FRAGMENT_SHADER_SRC: &str = r#"
#version 150
in vec3 f_color;
out vec4 color;
void main() {
    color = vec4(f_color, 1.0);
}
"#;

/// Per-vertex data: x, y (vector coords), r, g, b (0.0-1.0).
#[repr(C)]
struct Vertex {
    x: f32,
    y: f32,
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
    vertex_buf: Vec<Vertex>,
    point_start: usize, // index where point vertices begin in vertex_buf
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

        Self {
            program,
            vao,
            vbo,
            uniform_half_size,
            uniform_rotation,
            vertex_buf: Vec::with_capacity(2048),
            point_start: 0,
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
        // Build vertex data from display list.
        // Lines go first, then point vertices (for zero-length vectors / dots).
        self.vertex_buf.clear();
        self.point_start = 0;
        for line in lines {
            if line.intensity == 0 {
                continue;
            }
            if line.x0 == line.x1 && line.y0 == line.y1 {
                continue; // collect points in second pass
            }
            let brightness = INTENSITY_LUT[(line.intensity & 0xF) as usize];
            let r = brightness * (line.r as f32 / 255.0);
            let g = brightness * (line.g as f32 / 255.0);
            let b = brightness * (line.b as f32 / 255.0);
            self.vertex_buf.push(Vertex {
                x: line.x0 as f32,
                y: line.y0 as f32,
                r,
                g,
                b,
            });
            self.vertex_buf.push(Vertex {
                x: line.x1 as f32,
                y: line.y1 as f32,
                r,
                g,
                b,
            });
        }
        self.point_start = self.vertex_buf.len();
        for line in lines {
            if line.intensity == 0 {
                continue;
            }
            if line.x0 != line.x1 || line.y0 != line.y1 {
                continue;
            }
            let brightness = INTENSITY_LUT[(line.intensity & 0xF) as usize];
            let r = brightness * (line.r as f32 / 255.0);
            let g = brightness * (line.g as f32 / 255.0);
            let b = brightness * (line.b as f32 / 255.0);
            self.vertex_buf.push(Vertex {
                x: line.x0 as f32,
                y: line.y0 as f32,
                r,
                g,
                b,
            });
        }

        if self.vertex_buf.is_empty() {
            return;
        }

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

        unsafe {
            gl::Viewport(vp_x, vp_y, vp_w as i32, vp_h as i32);
            gl::UseProgram(self.program);
            gl::Uniform2f(
                self.uniform_half_size,
                display_w as f32 / 2.0,
                display_h as f32 / 2.0,
            );
            gl::Uniform1i(self.uniform_rotation, rotation);
            gl::BindVertexArray(self.vao);

            // Upload vertex data.
            gl::BindBuffer(gl::ARRAY_BUFFER, self.vbo);
            gl::BufferData(
                gl::ARRAY_BUFFER,
                (self.vertex_buf.len() * mem::size_of::<Vertex>()) as gl::types::GLsizeiptr,
                self.vertex_buf.as_ptr() as *const _,
                gl::DYNAMIC_DRAW,
            );

            // Additive blending: crossing lines appear brighter (matches
            // the CPU rasterizer's saturating_add behavior).
            gl::Enable(gl::BLEND);
            gl::BlendFunc(gl::ONE, gl::ONE);
            gl::Enable(gl::LINE_SMOOTH);

            // Draw lines.
            if self.point_start > 0 {
                gl::DrawArrays(gl::LINES, 0, self.point_start as i32);
            }

            // Draw zero-length vectors as points (bullets, dots).
            let point_count = self.vertex_buf.len() - self.point_start;
            if point_count > 0 {
                gl::PointSize(2.0);
                gl::DrawArrays(gl::POINTS, self.point_start as i32, point_count as i32);
            }

            // Restore state for egui.
            gl::Disable(gl::LINE_SMOOTH);
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

        // color (vec3): offset 8
        let color_attr = gl::GetAttribLocation(program, c"v_color".as_ptr());
        if color_attr >= 0 {
            gl::EnableVertexAttribArray(color_attr as u32);
            gl::VertexAttribPointer(
                color_attr as u32,
                3,
                gl::FLOAT,
                gl::FALSE,
                stride,
                (2 * mem::size_of::<f32>()) as *const _,
            );
        }

        gl::BindVertexArray(0);
        (vao, vbo)
    }
}
