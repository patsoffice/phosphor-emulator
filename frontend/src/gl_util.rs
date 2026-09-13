//! What both display renderers need from OpenGL.
//!
//! [`crate::vector_gl`] draws a beam along a display list and [`crate::crt_gl`]
//! sweeps one across a raster, but they are two ways of lighting the same tube
//! and they share the plumbing for it: how a program is built, the triangle that
//! covers the screen for a full-frame pass, and the halation blur, which comes
//! off the same faceplate whatever drew the light going into it.
//!
//! These lived in `vector_gl` while it was the only renderer that had them.
//! Reaching into the vector renderer for them made the raster stage depend on it
//! for something neither owns, which is the same misplacement the tube's own
//! figures had in `phosphor_core::device::dvg` before they moved to
//! `phosphor_core::device::crt`.
//!
//! The division between here and there is the layer, not the subject. That
//! module holds lengths on the glass, which any renderer converts into its own
//! units; this one holds the consequences of drawing with OpenGL, such as a
//! figure that exists only because a kernel has a particular number of taps.

use std::ffi::CString;
use std::ptr;

/// A triangle that covers the screen, its vertices computed from `gl_VertexID`.
///
/// Draw with `DrawArrays(TRIANGLES, 0, 3)` and no vertex buffer. A bound VAO is
/// still required by the core profile even when it feeds nothing.
pub(crate) const FULLSCREEN_VERTEX_SRC: &str = r#"
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
/// cover. Normalizing by the weight sum makes the pass conserve energy whatever
/// sigma works out to, including at the edges where taps fall outside.
pub(crate) const HALO_BLUR_FRAGMENT_SRC: &str = r#"
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

/// Sigma to aim for in the reduced-resolution halation field, in its own pixels.
///
/// A property of the kernel above rather than of the tube: it covers 12 taps
/// either side, and this keeps the profile comfortably inside them while leaving
/// the field small enough that two blur passes over it are nothing. Widen the
/// kernel and this figure moves with it.
pub(crate) const HALO_TARGET_SIGMA: f32 = 3.4;

pub(crate) unsafe fn compile_shader(
    src: &str,
    shader_type: gl::types::GLenum,
) -> gl::types::GLuint {
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

pub(crate) unsafe fn link_program(vertex_src: &str, fragment_src: &str) -> gl::types::GLuint {
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
