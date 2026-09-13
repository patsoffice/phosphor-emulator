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
//! # What it draws
//!
//! A line of the raster is the tube's spot swept along, and the picture at a
//! point is the sum of the lines near it. Whether that leaves a visible gap
//! between lines is not a setting: it falls out of the spot measured against the
//! line pitch, so the 224-line boards dip to about a quarter between lines and
//! Satan's Hollow at 480 does not dip at all. See
//! `phosphor_core::device::crt::beam_sigma_lines`.
//!
//! Because the gap has to be drawn somewhere, the stage renders at the
//! presentation resolution rather than the machine's. A 288x224 framebuffer has
//! nowhere to put a scanline, and the grid floor would widen the spot until the
//! structure disappeared.
//!
//! Not modeled yet: the spot's width along the sweep. That axis is a convolution
//! rather than a sum, because the beam moves continuously along a line instead
//! of landing on discrete spots, so it softens edges without adding structure.
//! Halation is `phosphor-emulator-21w8.4`.

use std::ffi::CString;

use phosphor_core::core::display::DisplaySettings;
use phosphor_core::core::machine::Orientation;
use phosphor_core::device::crt::{BEAM_CUTOFF_SIGMAS, MIN_SIGMA_PIXELS, beam_sigma_lines};

use crate::vector_gl::{FULLSCREEN_VERTEX_SRC, link_program};

/// Ceiling on the profile's half-width, in taps.
///
/// The cutoff is `BEAM_CUTOFF_SIGMAS` sigmas, which at the tube's own focus is
/// two taps even on the highest-line-count board in the registry. This only
/// binds when the focus control is turned well past where an operator would set
/// it, and it bounds the loop so a viewer cannot make a frame arbitrarily
/// expensive by dragging a slider.
const MAX_TAPS: i32 = 8;

/// The beam profile for one machine, worked out on the CPU so the shader carries
/// no derivation of its own.
struct BeamProfile {
    /// `1 / (2 * sigma^2)`, with sigma in line pitches.
    inv_two_sigma_sq: f32,
    /// Half-width of the summation, in lines.
    taps: i32,
    /// Scales the summed profile so a full-intensity picture reaches full white
    /// at a line's center, then applies the brightness control.
    gain: f32,
}

impl BeamProfile {
    /// `lines` is the machine's line count and `out_px` how many output pixels
    /// the line axis is drawn into.
    fn derive(lines: u32, out_px: u32, settings: &DisplaySettings) -> Self {
        let lines = lines.max(1) as f32;
        let sigma = beam_sigma_lines(lines) * settings.focus.max(0.0);

        // The floor is a property of the output grid rather than of the tube, so
        // it is applied in output pixels and converted back. Below it a Gaussian
        // sampled on a grid ripples with where its center falls between samples,
        // which would read as scanlines that shimmer as the window is resized.
        // A window too small to resolve the pitch therefore gets a smooth
        // picture rather than an aliased one, which is also what it had before
        // any of this existed.
        let px_per_line = (out_px as f32 / lines).max(f32::MIN_POSITIVE);
        let sigma = (sigma * px_per_line).max(MIN_SIGMA_PIXELS) / px_per_line;

        let inv_two_sigma_sq = 1.0 / (2.0 * sigma * sigma);
        let taps = ((BEAM_CUTOFF_SIGMAS * sigma).ceil() as i32).clamp(1, MAX_TAPS);

        // A full-intensity picture is every line at 1.0, so the profile summed
        // over a line's center is what a fully lit screen reaches there. Divide
        // by it and that point is full white, which is where an operator sets
        // the brightness. The troughs between lines fall wherever the spot and
        // the pitch put them, which is the whole point: a 224-line board dips to
        // about a quarter, and a 480-line board does not dip at all.
        //
        // Note what this does *not* hold constant. Average brightness falls when
        // the gaps are real, exactly as it did on the tube. Peak-at-full-white
        // and total-light-conserved cannot both hold while the pitch varies, and
        // the same choice is already made for the vector beam.
        let peak: f32 = (-taps..=taps)
            .map(|k| (-(k as f32) * (k as f32) * inv_two_sigma_sq).exp())
            .sum();
        let gain = settings.brightness.max(0.0) / peak;

        Self {
            inv_two_sigma_sq,
            taps,
            gain,
        }
    }
}

/// Orient, then lay the beam down line by line.
///
/// # The orientation half
///
/// The transform is the inverse of `phosphor_core::gfx::apply_orientation`,
/// which maps a source pixel forward to its destination and is the reference a
/// fragment shader has to run backwards. Inverting its three cases gives, for an
/// output coordinate pair scaled to 0..1, a mirror on each flipped axis followed
/// by a component swap, in that order. Mirroring in normalized coordinates lands
/// exactly on texel centers: `1 - (i + 0.5)/n` is `((n - 1 - i) + 0.5)/n`.
///
/// # The beam half
///
/// Everything after the orientation happens in *source* space, where the y axis
/// is the tube's line axis by construction, whatever the cabinet did with the
/// monitor. That is the whole reason the rotation had to move onto the GPU: in
/// screen space this sum would run along the wrong axis on every rotated
/// machine, and it would be the aspect-corrected axis rather than the tube's.
///
/// A line is a Gaussian in `y` of sigma `beam_sigma_lines`, and the picture is
/// the sum of the lines near this point. Nothing corresponding happens in `x`:
/// the beam sweeps continuously along a line rather than landing on discrete
/// spots, so that axis is a convolution rather than a sum, and it produces
/// softening with no structure. That asymmetry is why scanlines exist and why
/// there is no vertical counterpart to them. The horizontal half is not modeled
/// here yet; at these resolutions the spot is about half a source pixel across,
/// so it softens edges without changing the geometry.
///
/// Sampling past the first or last line reads the edge line again, since the
/// source texture clamps. The alternative, treating outside as dark, dims the
/// top and bottom rows by up to a quarter once sigma is floored on a small
/// window, which reads as a defect rather than as the overscan it stands in for.
const BEAM_FRAGMENT_SRC: &str = r#"
#version 150
in vec2 uv;
out vec4 color;
uniform sampler2D src;
uniform vec2 flip;
uniform bool swap_xy;
uniform float lines;
uniform float inv_two_sigma_sq;
uniform float gain;
uniform int taps;
void main() {
    vec2 c = mix(uv, vec2(1.0) - uv, flip);
    vec2 s = swap_xy ? c.yx : c.xy;

    // Position along the line axis, in line pitches. Line k spans [k, k+1) and
    // is emitted at its center, k + 0.5.
    float y = s.y * lines;
    float center = floor(y);

    vec3 sum = vec3(0.0);
    for (int k = -taps; k <= taps; ++k) {
        float row = center + float(k);
        float d = y - (row + 0.5);
        sum += texture(src, vec2(s.x, (row + 0.5) / lines)).rgb
             * exp(-d * d * inv_two_sigma_sq);
    }
    color = vec4(sum * gain, 1.0);
}
"#;

pub struct CrtRenderer {
    program: gl::types::GLuint,
    vao: gl::types::GLuint,
    src_uniform: gl::types::GLint,
    flip_uniform: gl::types::GLint,
    swap_uniform: gl::types::GLint,
    lines_uniform: gl::types::GLint,
    inv_two_sigma_sq_uniform: gl::types::GLint,
    gain_uniform: gl::types::GLint,
    taps_uniform: gl::types::GLint,
    /// Size of the attached texture as this stage last set it, so a window
    /// resize reallocates it and an unchanged one costs nothing.
    out_size: (u32, u32),
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
            let program = link_program(FULLSCREEN_VERTEX_SRC, BEAM_FRAGMENT_SRC);
            let uniform = |name: &str| {
                let name = CString::new(name).expect("literal has no interior nul");
                gl::GetUniformLocation(program, name.as_ptr())
            };
            let src_uniform = uniform("src");
            let flip_uniform = uniform("flip");
            let swap_uniform = uniform("swap_xy");
            let lines_uniform = uniform("lines");
            let inv_two_sigma_sq_uniform = uniform("inv_two_sigma_sq");
            let gain_uniform = uniform("gain");
            let taps_uniform = uniform("taps");

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
                lines_uniform,
                inv_two_sigma_sq_uniform,
                gain_uniform,
                taps_uniform,
                out_size: (0, 0),
                src_tex,
                src_size: (0, 0),
                fbo,
                attached: None,
            }
        }
    }

    /// Render the machine's native frame into `out_tex`, the texture egui draws,
    /// applying `orientation` and the beam profile on the way.
    ///
    /// `src` is the size of the buffer `render_frame` fills. `out` is the
    /// resolution to draw at, which is the presentation surface rather than the
    /// machine's own size: a 288x224 raster has nowhere to put a scanline, so
    /// drawing at native resolution would floor the spot into invisibility. It
    /// carries the displayed aspect, so it is the native size with the axes
    /// swapped when the orientation swaps them, scaled up.
    ///
    /// Returns false when the framebuffer will not complete, leaving `out_tex`
    /// untouched so the caller can fall back to orienting and uploading on the
    /// CPU. A driver that refuses the attachment should cost the picture, not the
    /// session.
    pub fn present(
        &mut self,
        rgb24: &[u8],
        src: (u32, u32),
        out: (u32, u32),
        orientation: Orientation,
        settings: &DisplaySettings,
        out_tex: gl::types::GLuint,
    ) -> bool {
        let (src_w, src_h) = src;
        let (out_w, out_h) = out;
        debug_assert_eq!(rgb24.len(), (src_w as usize) * (src_h as usize) * 3);

        // The source's rows are the tube's lines, always: the orientation is
        // applied after this point, so a rotated cabinet does not move them.
        // Which *output* axis they end up along is what the swap decides, and
        // that is the axis whose resolution decides whether the pitch is
        // representable.
        let lines = src_h;
        let out_px_along_lines = if orientation.swaps_axes() {
            out_w
        } else {
            out_h
        };
        let beam = BeamProfile::derive(lines, out_px_along_lines, settings);

        unsafe {
            if !self.attach(out_tex, out) {
                return false;
            }
            self.upload_source(rgb24, src_w, src_h);

            let mut viewport = [0i32; 4];
            gl::GetIntegerv(gl::VIEWPORT, viewport.as_mut_ptr());

            gl::BindFramebuffer(gl::FRAMEBUFFER, self.fbo);
            gl::Viewport(0, 0, out_w as i32, out_h as i32);
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
            gl::Uniform1f(self.lines_uniform, lines as f32);
            gl::Uniform1f(self.inv_two_sigma_sq_uniform, beam.inv_two_sigma_sq);
            gl::Uniform1f(self.gain_uniform, beam.gain);
            gl::Uniform1i(self.taps_uniform, beam.taps);

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

    /// Point the framebuffer at the texture egui draws, sized to `out`.
    ///
    /// The texture belongs to egui's painter, which allocated it at the
    /// machine's displayed size to upload pixels into. This stage renders into
    /// it instead, at the presentation resolution, so it is reallocated here and
    /// again whenever the window changes. The painter's own record of its size
    /// goes stale, which is harmless: that field is read only when uploading a
    /// dirty texture, and a texture this stage is driving is never dirty. Should
    /// the stage ever fall back, the upload reallocates it to the painter's size
    /// and the two agree again.
    unsafe fn attach(&mut self, out_tex: gl::types::GLuint, out: (u32, u32)) -> bool {
        if self.attached == Some(out_tex) && self.out_size == out {
            return true;
        }
        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, out_tex);
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGBA8 as i32,
                out.0 as i32,
                out.1 as i32,
                0,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                std::ptr::null(),
            );
            gl::BindTexture(gl::TEXTURE_2D, 0);

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
            self.out_size = out;
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

    /// `BEAM_FRAGMENT_SRC`'s summation, in Rust, for a source lit uniformly to
    /// full intensity. Returns what one output pixel at `y` line pitches down
    /// the raster ends up at.
    fn brightness(beam: &BeamProfile, y: f32) -> f32 {
        let center = y.floor();
        (-beam.taps..=beam.taps)
            .map(|k| {
                let d = y - (center + k as f32 + 0.5);
                (-d * d * beam.inv_two_sigma_sq).exp()
            })
            .sum::<f32>()
            * beam.gain
    }

    /// What the epic exists to produce, and the result that decided its shape:
    /// the same derivation gives one board real scanlines and another none, with
    /// nothing per-machine anywhere in the code.
    #[test]
    fn the_profile_dips_between_lines_on_a_224_line_board_and_not_on_a_480_line_one() {
        let settings = DisplaySettings::MEASURED;
        // Four output pixels per line, comfortably clear of the grid floor, so
        // this measures the tube rather than the window.
        let low = BeamProfile::derive(224, 224 * 4, &settings);
        let high = BeamProfile::derive(480, 480 * 4, &settings);

        // A line's center is at k + 0.5, the gap between two lines at k.
        for beam in [&low, &high] {
            assert!(
                (brightness(beam, 10.5) - 1.0).abs() < 1e-3,
                "a fully lit source should reach full white along a line's center"
            );
        }

        let low_trough = brightness(&low, 10.0);
        let high_trough = brightness(&high, 10.0);

        assert!(
            (low_trough - 0.256).abs() < 0.01,
            "224 lines: the gap between lines should fall to about a quarter, was {low_trough}"
        );
        assert!(
            high_trough > 0.97,
            "480 lines: the spot is wider than the pitch, so there should be no \
             gap to see, but the trough was {high_trough}"
        );
    }

    /// The floor is a property of the output grid, so a window too small to
    /// resolve the pitch gets a smooth picture rather than an aliased one. Left
    /// unfloored, the ripple would track where each line's center happened to
    /// fall between output pixels and would crawl as the window was resized.
    #[test]
    fn one_output_pixel_per_line_washes_the_scanlines_out_instead_of_aliasing() {
        let beam = BeamProfile::derive(224, 224, &DisplaySettings::MEASURED);
        let trough = brightness(&beam, 10.0);
        let peak = brightness(&beam, 10.5);
        assert!(
            (peak - trough).abs() < 0.02,
            "at one pixel per line there is nowhere to draw a gap, so the \
             profile should be flat; peak {peak}, trough {trough}"
        );
    }

    /// Focus is the control that actually differs between real monitors, since
    /// tube size cancels out of a figure expressed per pitch. Softening the spot
    /// has to fill the gaps in, which is what a badly adjusted cabinet looked
    /// like.
    #[test]
    fn a_softer_focus_fills_the_gaps_between_lines() {
        let sharp = BeamProfile::derive(224, 224 * 4, &DisplaySettings::MEASURED);
        let soft = BeamProfile::derive(
            224,
            224 * 4,
            &DisplaySettings {
                focus: 2.0,
                ..DisplaySettings::MEASURED
            },
        );
        assert!(
            brightness(&soft, 10.0) > brightness(&sharp, 10.0) + 0.2,
            "turning the focus control up should wash the scanlines out"
        );
    }

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
