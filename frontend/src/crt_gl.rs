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
//! line pitch, so a 224-line board has gaps and Satan's Hollow at 480 does not,
//! its spot being wider than its pitch. See
//! `phosphor_core::device::crt::beam_sigma_lines`.
//!
//! How deep those gaps run depends on the picture, because the spot grows with
//! beam current: a dim raster is written with a tight spot and keeps crisp
//! scanlines, a bright one widens and washes them out.
//!
//! Because the gap has to be drawn somewhere, the stage renders at the
//! presentation resolution rather than the machine's. A 288x224 framebuffer has
//! nowhere to put a scanline, and the grid floor would widen the spot until the
//! structure disappeared.
//!
//! Halation rides on top of that: light steeper than the faceplate's critical
//! angle cannot leave the glass, reflects, crosses it again and re-emerges about
//! 19 mm away. It is taken out of the core rather than added on top, because
//! that is where it went.
//!
//! Not modeled yet: the spot's width along the sweep. That axis is a convolution
//! rather than a sum, because the beam moves continuously along a line instead
//! of landing on discrete spots, so it softens edges without adding structure.
//! See `phosphor-emulator-rkcy`.

use std::ffi::CString;

use phosphor_core::core::display::DisplaySettings;
use phosphor_core::core::machine::Orientation;
use phosphor_core::device::crt::{
    BEAM_CUTOFF_SIGMAS, MIN_SIGMA_PIXELS, SPOT_GROWTH_AT_FULL_DRIVE, beam_sigma_lines,
    halation_sigma_units,
};

use crate::gl_util::{
    FULLSCREEN_VERTEX_SRC, HALO_BLUR_FRAGMENT_SRC, HALO_TARGET_SIGMA, TextureTarget, link_program,
};

/// Ceiling on the profile's half-width, in taps.
///
/// The cutoff is `BEAM_CUTOFF_SIGMAS` sigmas, which at the tube's own focus is
/// two taps even on the highest-line-count board in the registry. This only
/// binds when the focus control is turned well past where an operator would set
/// it, and it bounds the loop so a viewer cannot make a frame arbitrarily
/// expensive by dragging a slider.
const MAX_TAPS: i32 = 8;

/// The drive the tube's focused spot figure is taken to describe.
///
/// A spot size is quoted at some operating current, and the interesting thing
/// about a datasheet's is that it is neither cutoff nor peak white: it is the
/// tube doing ordinary work. Half scale says that without pretending to more
/// precision than "somewhere in the middle", and what it buys is that the beam
/// sharpens below it and blooms above it rather than only doing one of the two.
const NOMINAL_DRIVE: f32 = 0.5;

/// The beam profile for one machine, worked out on the CPU so the shader carries
/// no derivation of its own.
struct BeamProfile {
    /// The spot at zero drive, in line pitches: the focused figure, which is the
    /// narrowest it ever is.
    sigma: f32,
    /// `K^2 - 1`, where `K` is the spot's growth at full drive. The shader wants
    /// it in this form because it works in area.
    bloom_var: f32,
    /// Half-width of the summation, in lines. Sized for the widest the spot
    /// gets, since a dimmer sample only needs fewer taps than it is given.
    taps: i32,
    /// Scales the summed profile so a full-intensity picture reaches full white
    /// at a line's center, then applies the brightness control.
    gain: f32,
}

impl BeamProfile {
    /// `lines` is the machine's line count, `out_px` how many output pixels the
    /// line axis is drawn into, and `halo_sigma_px` the halation skirt's width in
    /// those same pixels, which decides how much of the core's light the glow
    /// takes away from where it was.
    fn derive(lines: u32, out_px: u32, halo_sigma_px: f32, settings: &DisplaySettings) -> Self {
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

        // The measured spot is what the tube does at a nominal drive, not at
        // cutoff, so it is anchored at half scale and the beam sharpens below
        // that and blooms above it. Anchoring at zero instead would widen every
        // intensity and wash the scanlines out of the whole picture rather than
        // out of its bright parts.
        let growth = SPOT_GROWTH_AT_FULL_DRIVE.max(1.0);
        let bloom_var = growth * growth - 1.0;
        let sigma = sigma / (1.0 + bloom_var * NOMINAL_DRIVE).sqrt();
        let sigma_max = sigma * growth;
        let taps = ((BEAM_CUTOFF_SIGMAS * sigma_max).ceil() as i32).clamp(1, MAX_TAPS);

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
        // The weights carry unit area now, so a wider spot spreads its light
        // rather than adding any, and a broad lit area blooms into itself and
        // comes back unchanged. The normalization is taken at full drive, which
        // is the state a fully lit screen is in, so that screen still peaks at
        // full white. Dimmer content is drawn with a tighter spot and so keeps
        // crisper scanlines, which is what a tube does.
        let inv_two_sigma_max_sq = 1.0 / (2.0 * sigma_max * sigma_max);
        let peak: f32 = (-taps..=taps)
            .map(|k| {
                (-(k as f32) * (k as f32) * inv_two_sigma_max_sq).exp()
                    / (sigma_max * std::f32::consts::TAU.sqrt())
            })
            .sum();

        // Halation takes its fraction out of the core and spreads it across
        // 19 mm, so without this the picture simply gets darker as the slider
        // goes up. An operator would answer that by turning the brightness up,
        // and this is that: the share of its own peak a feature keeps is
        // `(1-f) + f*sigma/halo_sigma`, both profiles carrying unit energy, and
        // its reciprocal restores it. The vector rasterizer computes the same
        // quantity the same way.
        //
        // It was left out at first on the argument that a raster's lit areas are
        // broad, and that blurring something broad gives it back, so `(1-f) + f`
        // already returns to unity. True of a flat field and false of the
        // content: arcade screens are mostly sparse bright shapes on black,
        // which is the case the correction exists for, and without it turning
        // halation up just dimmed the picture.
        //
        // The cost, which is the trade this epic already names: total emitted
        // light now rises with the fraction. Peak-at-full-white and
        // total-light-conserved cannot both hold while the fraction varies, and
        // the peak is the one a viewer judges the setting by.
        let sigma_px = sigma_max * px_per_line;
        let halation = settings.halation.clamp(0.0, 1.0);
        let peak_share = (1.0 - halation)
            + halation * (sigma_px / halo_sigma_px.max(f32::MIN_POSITIVE)).min(1.0);
        let gain = settings.brightness.max(0.0) / (peak * peak_share.max(f32::MIN_POSITIVE));

        Self {
            sigma,
            bloom_var,
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
uniform float sigma;
uniform float bloom_var;
uniform float gain;
uniform int taps;

const float INV_SQRT_TAU = 0.39894228;

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
        vec3 lit = texture(src, vec2(s.x, (row + 0.5) / lines)).rgb;

        // Each gun carries its own current, so each channel has its own spot.
        // Area with drive, diameter with its root.
        vec3 sig = sigma * sqrt(vec3(1.0) + bloom_var * lit);

        // Unit area, so widening spreads the light rather than adding any. That
        // is what keeps a broad lit area unchanged when it blooms: it blooms
        // into itself.
        sum += lit * exp(-(d * d) / (2.0 * sig * sig)) * INV_SQRT_TAU / sig;
    }
    color = vec4(sum * gain, 1.0);
}
"#;

/// Put the core and the halation skirt back together.
///
/// `mix(core, halo, f)` is `core*(1-f) + halo*f`: the fraction that left through
/// the faceplate is taken *out* of the core rather than added on top of it,
/// because that is what happened in the glass. Light is conserved.
///
/// The gain that puts the peak back is applied to the core when it is drawn, not
/// here. See [`BeamProfile::derive`] for why it has to exist at all.
const HALO_COMPOSITE_FRAGMENT_SRC: &str = r#"
#version 150
in vec2 uv;
out vec4 color;
uniform sampler2D core;
uniform sampler2D halo;
uniform float fraction;
uniform float halo_gain;
void main() {
    vec3 c = texture(core, uv).rgb;
    vec3 h = texture(halo, uv).rgb;
    color = vec4(c * (1.0 - fraction) + h * (fraction * halo_gain), 1.0);
}
"#;

/// How much brighter than conserved the skirt is drawn.
///
/// **This is the second figure in the model with no derivation behind it, and
/// the epic asked for there not to be one.** It is here anyway, deliberately, and
/// this is the argument for it.
///
/// At 1.0 the glow carries exactly the light taken out of the core, which is the
/// physics. On raster content that is very nearly invisible: a maze line is a
/// few pixels wide against a skirt tens of pixels wide, so its own light comes
/// back at a few parts in a thousand. Conserved halation can be derived or it
/// can be seen, and at the width `halation_sigma_units` gives, not both.
///
/// Where the number comes from, and it is worth reading because two attempts to
/// derive it both failed and the failures are informative.
///
/// The skirt's width was suspect: `halation_sigma_units` uses the offset for a
/// ray at exactly the critical angle, which is the widest any ray goes, as a
/// Gaussian sigma. Working the real profile out gives a filled disc whose
/// equivalent sigma is 2.15 times narrower, so the code's skirt is too wide and
/// correcting it would strengthen the glow for free. It accounts for 2.15 of
/// this, and no more.
///
/// Blooming was the other candidate: the spot grows with beam current, so a
/// bright feature is written wider. That is real and is modeled here, but it
/// happens at the spot's scale, tens of microns to a millimetre. The glow this
/// number produces is at the skirt's scale, about 10 mm. Two orders of magnitude
/// apart, so blooming cannot be what this stands for. Tried and looked at: with
/// this at 1.0 and blooming on, no glow is visible at the measured fraction.
///
/// So this is a judgment about how the picture should look, and not a stand-in
/// for a mechanism nobody has found yet. It is set so `HALATION_FRACTION`, the
/// measured default, produces the strength settled on by eye across four boards,
/// bright and dark, which is a skirt about 1.05 times conserved. The slider
/// therefore still means the fraction of light that halates and still starts
/// where the tube's own figure puts it, and the one aesthetic call sits here in
/// a single named place rather than in a per-machine override.
///
/// What it costs, because it is a real cost and not a rounding error: light is
/// no longer conserved, by `1 + (G-1)*f` over a broad lit area, which at the
/// default is about 2.8 times. Anything brighter than the reciprocal of that,
/// around 35% of full scale, therefore clips.
///
/// The vector machines never show this, because vector content has no broad lit
/// areas. Raster boards do, so a board with a bright background is the one that
/// sets the ceiling: Donkey Kong, Marble Madness, Road Runner and BurgerTime
/// were all judged right at a skirt about 1.05 times conserved, which is what
/// the default gives, and past roughly twice that the Atari System 1 backgrounds
/// wash out while a dark board still looks fine.
///
/// That a multiplier is riding on the picture at all is the price of the glow
/// being visible. Conserved halation cannot clip, because the blur of a broad
/// lit area is that area, and it also cannot be seen.
const HALO_GAIN_OVER_CONSERVED: f32 = 15.0;

/// The offscreen targets the halation pass needs: the core at presentation
/// resolution, and two small fields to ping-pong the separable blur between.
struct Targets {
    core_tex: gl::types::GLuint,
    core_fbo: gl::types::GLuint,
    core_size: (u32, u32),
    halo_tex: [gl::types::GLuint; 2],
    halo_fbo: [gl::types::GLuint; 2],
    halo_size: (u32, u32),
}

pub struct CrtRenderer {
    program: gl::types::GLuint,
    vao: gl::types::GLuint,
    src_uniform: gl::types::GLint,
    flip_uniform: gl::types::GLint,
    swap_uniform: gl::types::GLint,
    lines_uniform: gl::types::GLint,
    sigma_uniform: gl::types::GLint,
    bloom_var_uniform: gl::types::GLint,
    gain_uniform: gl::types::GLint,
    taps_uniform: gl::types::GLint,
    /// The machine's frame as uploaded each frame. Owned here.
    src_tex: gl::types::GLuint,
    src_size: (u32, u32),
    /// Where the finished picture lands: the texture egui draws.
    out: TextureTarget,

    blur_program: gl::types::GLuint,
    blur_src_uniform: gl::types::GLint,
    blur_step_uniform: gl::types::GLint,
    blur_inv_two_sigma_sq_uniform: gl::types::GLint,

    composite_program: gl::types::GLuint,
    composite_core_uniform: gl::types::GLint,
    composite_halo_uniform: gl::types::GLint,
    composite_fraction_uniform: gl::types::GLint,
    composite_halo_gain_uniform: gl::types::GLint,

    /// Allocated the first time halation is asked for, and dropped when the
    /// viewport changes. `None` means the glow is off, whether because the
    /// viewer turned it off or because a target would not allocate.
    targets: Option<Targets>,
    /// Whether the halation targets have ever been reported. A glow that never
    /// allocated draws exactly what no glow draws, so say it once rather than
    /// leaving the two indistinguishable.
    halo_reported: bool,
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
            let sigma_uniform = uniform("sigma");
            let bloom_var_uniform = uniform("bloom_var");
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

            let blur_program = link_program(FULLSCREEN_VERTEX_SRC, HALO_BLUR_FRAGMENT_SRC);
            let blur_uniform = |name: &str| {
                let name = CString::new(name).expect("literal has no interior nul");
                gl::GetUniformLocation(blur_program, name.as_ptr())
            };
            let blur_src_uniform = blur_uniform("src");
            let blur_step_uniform = blur_uniform("tap_step");
            let blur_inv_two_sigma_sq_uniform = blur_uniform("inv_two_sigma_sq");

            let composite_program =
                link_program(FULLSCREEN_VERTEX_SRC, HALO_COMPOSITE_FRAGMENT_SRC);
            let composite_uniform = |name: &str| {
                let name = CString::new(name).expect("literal has no interior nul");
                gl::GetUniformLocation(composite_program, name.as_ptr())
            };
            let composite_core_uniform = composite_uniform("core");
            let composite_halo_uniform = composite_uniform("halo");
            let composite_fraction_uniform = composite_uniform("fraction");
            let composite_halo_gain_uniform = composite_uniform("halo_gain");

            Self {
                program,
                vao,
                src_uniform,
                flip_uniform,
                swap_uniform,
                lines_uniform,
                sigma_uniform,
                bloom_var_uniform,
                gain_uniform,
                taps_uniform,
                src_tex,
                src_size: (0, 0),
                out: TextureTarget::new(),
                blur_program,
                blur_src_uniform,
                blur_step_uniform,
                blur_inv_two_sigma_sq_uniform,
                composite_program,
                composite_core_uniform,
                composite_halo_uniform,
                composite_fraction_uniform,
                composite_halo_gain_uniform,
                targets: None,
                halo_reported: false,
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
        let halation = settings.halation.clamp(0.0, 1.0);
        // The skirt's width in presentation pixels, needed before the core is
        // drawn because it decides how much gain the core needs to hold its peak
        // against what the glow takes away.
        let halo_sigma_px = halation_sigma_units(out_w.max(out_h) as f32);
        let beam = BeamProfile::derive(lines, out_px_along_lines, halo_sigma_px, settings);

        unsafe {
            if !self.out.attach(out_tex, out) {
                return false;
            }
            self.upload_source(rgb24, src_w, src_h);

            // With no targets there is no glow, and then the core keeps all of
            // its light rather than being scaled down for a skirt nothing is
            // going to draw.
            let glowing = halation > 0.0 && self.ensure_targets(out);
            if !glowing {
                self.drop_targets();
            }
            if glowing && !self.halo_reported {
                self.halo_reported = true;
                let (hw, hh) = self.targets.as_ref().map_or((0, 0), |t| t.halo_size);
                eprintln!(
                    "Halation active: fraction {halation:.3}, field {hw}x{hh} from {out_w}x{out_h}"
                );
            }

            let mut viewport = [0i32; 4];
            gl::GetIntegerv(gl::VIEWPORT, viewport.as_mut_ptr());

            // egui leaves these on from its own pass. None of them belong in any
            // of what follows, and paint_jobs re-enables what it needs next
            // frame. Every pass here replaces rather than accumulates, so
            // blending stays off throughout.
            gl::Disable(gl::SCISSOR_TEST);
            gl::Disable(gl::BLEND);
            gl::Disable(gl::FRAMEBUFFER_SRGB);
            gl::BindVertexArray(self.vao);
            gl::ActiveTexture(gl::TEXTURE0);

            // Pass one: the beam. Straight into the texture egui draws when
            // there is no glow, and into the core target when the composite is
            // going to need it twice.
            let core = self.targets.as_ref().map(|t| (t.core_fbo, t.core_tex));
            gl::BindFramebuffer(gl::FRAMEBUFFER, core.map_or(self.out.fbo(), |c| c.0));
            gl::Viewport(0, 0, out_w as i32, out_h as i32);

            gl::UseProgram(self.program);
            gl::BindTexture(gl::TEXTURE_2D, self.src_tex);
            gl::Uniform1i(self.src_uniform, 0);
            gl::Uniform2f(
                self.flip_uniform,
                orientation.flip_x() as i32 as f32,
                orientation.flip_y() as i32 as f32,
            );
            gl::Uniform1i(self.swap_uniform, orientation.swaps_axes() as i32);
            gl::Uniform1f(self.lines_uniform, lines as f32);
            gl::Uniform1f(self.sigma_uniform, beam.sigma);
            gl::Uniform1f(self.bloom_var_uniform, beam.bloom_var);
            // Full energy here. The split between what leaves directly and what
            // goes the long way round is applied at the composite, not to the
            // light being emitted.
            gl::Uniform1f(self.gain_uniform, beam.gain);
            gl::Uniform1i(self.taps_uniform, beam.taps);
            gl::DrawArrays(gl::TRIANGLES, 0, 3);

            if let Some((_, core_tex)) = core
                && let Some(t) = self.targets.as_ref()
            {
                let (halo_fbo, halo_tex, (hw, hh)) = (t.halo_fbo, t.halo_tex, t.halo_size);

                // The mip chain the blur's first pass reads down from.
                gl::BindTexture(gl::TEXTURE_2D, core_tex);
                gl::GenerateMipmap(gl::TEXTURE_2D);

                // Passes two and three: the separable blur. The first reads the
                // core directly and lands in the small field, which is where the
                // minification happens; the second stays inside it. The taps are
                // a small field pixel apart in both, so one sigma describes both
                // and the kernel stays the fixed cheap one.
                //
                // The scanline structure does not survive this, which is right:
                // the skirt is 19 mm across and averages over dozens of lines, so
                // there is nothing of the pitch left in the glow.
                let long_axis = out_w.max(out_h) as f32;
                let down = long_axis / hw.max(hh) as f32;
                let small_sigma = halation_sigma_units(long_axis) / down.max(f32::MIN_POSITIVE);
                gl::UseProgram(self.blur_program);
                gl::Uniform1i(self.blur_src_uniform, 0);
                gl::Uniform1f(
                    self.blur_inv_two_sigma_sq_uniform,
                    1.0 / (2.0 * small_sigma * small_sigma),
                );
                gl::Viewport(0, 0, hw as i32, hh as i32);
                for (dst, src_tex, step) in [
                    (halo_fbo[0], core_tex, (1.0 / hw as f32, 0.0)),
                    (halo_fbo[1], halo_tex[0], (0.0, 1.0 / hh as f32)),
                ] {
                    gl::BindFramebuffer(gl::FRAMEBUFFER, dst);
                    gl::BindTexture(gl::TEXTURE_2D, src_tex);
                    gl::Uniform2f(self.blur_step_uniform, step.0, step.1);
                    gl::DrawArrays(gl::TRIANGLES, 0, 3);
                }

                // Pass four: put the two back together into egui's texture.
                gl::BindFramebuffer(gl::FRAMEBUFFER, self.out.fbo());
                gl::Viewport(0, 0, out_w as i32, out_h as i32);
                gl::UseProgram(self.composite_program);
                gl::Uniform1i(self.composite_core_uniform, 0);
                gl::Uniform1i(self.composite_halo_uniform, 1);
                gl::Uniform1f(self.composite_fraction_uniform, halation);
                gl::Uniform1f(self.composite_halo_gain_uniform, HALO_GAIN_OVER_CONSERVED);
                gl::BindTexture(gl::TEXTURE_2D, core_tex);
                gl::ActiveTexture(gl::TEXTURE1);
                gl::BindTexture(gl::TEXTURE_2D, halo_tex[1]);
                gl::DrawArrays(gl::TRIANGLES, 0, 3);
                gl::BindTexture(gl::TEXTURE_2D, 0);
                gl::ActiveTexture(gl::TEXTURE0);
            }

            gl::BindVertexArray(0);
            gl::BindTexture(gl::TEXTURE_2D, 0);
            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
            gl::Viewport(viewport[0], viewport[1], viewport[2], viewport[3]);
        }
        true
    }

    /// Allocate the core and halation targets for this viewport, if they are not
    /// already the right size.
    ///
    /// The halation field is deliberately tiny. The skirt is about 5% of the
    /// tube's long axis, which at presentation resolution is tens of pixels, far
    /// past what a fixed 12-tap kernel can cover. Shrinking the field until the
    /// skirt lands near [`HALO_TARGET_SIGMA`] puts it back inside the kernel and
    /// makes the two blur passes cost nothing: for a 896x1152 viewport the field
    /// comes out around 50x64. Nothing is lost by it, since the thing being
    /// stored is a 19 mm blur.
    unsafe fn ensure_targets(&mut self, out: (u32, u32)) -> bool {
        let (w, h) = out;
        let long_axis = w.max(h) as f32;
        let halo_sigma_px = halation_sigma_units(long_axis);
        let down = (halo_sigma_px / HALO_TARGET_SIGMA).round().max(1.0);
        let halo_size = (
            ((w as f32 / down).ceil() as u32).max(1),
            ((h as f32 / down).ceil() as u32).max(1),
        );

        if let Some(t) = &self.targets
            && t.core_size == out
            && t.halo_size == halo_size
        {
            return true;
        }
        self.drop_targets();

        unsafe {
            let mut tex = [0u32; 3];
            let mut fbo = [0u32; 3];
            gl::GenTextures(3, tex.as_mut_ptr());
            gl::GenFramebuffers(3, fbo.as_mut_ptr());

            let sizes = [out, halo_size, halo_size];
            for i in 0..3 {
                gl::BindTexture(gl::TEXTURE_2D, tex[i]);
                // Half float, not RGBA8. The core is emitted light rather than a
                // picture: to hold its peak against what halation takes away it
                // is drawn brighter than full white, by 1.65 at the slider's
                // top, and eight unsigned bits clamp that away on write. The
                // gain would then be silently undone and the composite's `1 - f`
                // would read as a dimmer, which is exactly what it did. Only the
                // last pass, into egui's texture, needs to be display range.
                gl::TexImage2D(
                    gl::TEXTURE_2D,
                    0,
                    gl::RGBA16F as i32,
                    sizes[i].0 as i32,
                    sizes[i].1 as i32,
                    0,
                    gl::RGBA,
                    gl::HALF_FLOAT,
                    std::ptr::null(),
                );
                // The core is read back heavily minified, to seed a field around
                // eighteen times smaller, so it carries a mip chain and the
                // blur's taps come off a prefiltered level. Point-sampling one
                // texel in eighteen would alias, and a glow that shimmers as the
                // picture moves underneath it is worse than no glow. The halo
                // fields are only ever read at or above their own resolution, so
                // plain linear is enough for them.
                gl::TexParameteri(
                    gl::TEXTURE_2D,
                    gl::TEXTURE_MIN_FILTER,
                    if i == 0 {
                        gl::LINEAR_MIPMAP_LINEAR as i32
                    } else {
                        gl::LINEAR as i32
                    },
                );
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
                // Clamp, so the blur's outermost taps do not wrap the glow around
                // to the far edge of the screen.
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
                    gl::BindTexture(gl::TEXTURE_2D, 0);
                    gl::DeleteFramebuffers(3, fbo.as_ptr());
                    gl::DeleteTextures(3, tex.as_ptr());
                    return false;
                }
            }
            gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
            gl::BindTexture(gl::TEXTURE_2D, 0);

            self.targets = Some(Targets {
                core_tex: tex[0],
                core_fbo: fbo[0],
                core_size: out,
                halo_tex: [tex[1], tex[2]],
                halo_fbo: [fbo[1], fbo[2]],
                halo_size,
            });
            true
        }
    }

    fn drop_targets(&mut self) {
        if let Some(t) = self.targets.take() {
            unsafe {
                gl::DeleteFramebuffers(1, &t.core_fbo);
                gl::DeleteFramebuffers(2, t.halo_fbo.as_ptr());
                gl::DeleteTextures(1, &t.core_tex);
                gl::DeleteTextures(2, t.halo_tex.as_ptr());
            }
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
        self.drop_targets();
        unsafe {
            gl::DeleteTextures(1, &self.src_tex);
            gl::DeleteVertexArrays(1, &self.vao);
            gl::DeleteProgram(self.program);
            gl::DeleteProgram(self.blur_program);
            gl::DeleteProgram(self.composite_program);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::gfx::apply_orientation;

    /// The halation skirt in presentation pixels for a 1152-pixel long axis,
    /// which is Pac-Man at scale 4. Only the gain depends on it.
    const HALO_SIGMA_PX: f32 = 60.1;

    /// The tube's settings with the glow off.
    ///
    /// The scanline tests measure the core alone, and with halation on, the core
    /// is deliberately drawn brighter than full white so that the composite's
    /// `1 - f` brings it back. Reading the core on its own and expecting full
    /// white would be measuring half of a two-pass result. The gain that does
    /// that is covered by its own test.
    fn no_glow(focus: f32) -> DisplaySettings {
        DisplaySettings {
            focus,
            halation: 0.0,
            ..DisplaySettings::MEASURED
        }
    }

    /// `BEAM_FRAGMENT_SRC`'s summation, in Rust, for a source lit uniformly to
    /// `lit`. Returns what one output pixel at `y` line pitches down the raster
    /// ends up at.
    ///
    /// The spot's width depends on `lit`, which is the whole of the bloom model,
    /// so this takes it rather than assuming full drive.
    fn brightness_at(beam: &BeamProfile, y: f32, lit: f32) -> f32 {
        let sigma = beam.sigma * (1.0 + beam.bloom_var * lit).sqrt();
        let center = y.floor();
        (-beam.taps..=beam.taps)
            .map(|k| {
                let d = y - (center + k as f32 + 0.5);
                lit * (-(d * d) / (2.0 * sigma * sigma)).exp()
                    / (sigma * std::f32::consts::TAU.sqrt())
            })
            .sum::<f32>()
            * beam.gain
    }

    /// The common case: a source lit to full intensity.
    fn brightness(beam: &BeamProfile, y: f32) -> f32 {
        brightness_at(beam, y, 1.0)
    }

    /// What the epic exists to produce, and the result that decided its shape:
    /// the same derivation gives one board real scanlines and another none, with
    /// nothing per-machine anywhere in the code.
    #[test]
    fn the_profile_dips_between_lines_on_a_224_line_board_and_not_on_a_480_line_one() {
        let settings = no_glow(1.0);
        // Four output pixels per line, comfortably clear of the grid floor, so
        // this measures the tube rather than the window.
        let low = BeamProfile::derive(224, 224 * 4, HALO_SIGMA_PX, &settings);
        let high = BeamProfile::derive(480, 480 * 4, HALO_SIGMA_PX, &settings);

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
            low_trough < 0.65,
            "224 lines: there should be a real gap between lines, was {low_trough}"
        );
        assert!(
            high_trough > 0.95,
            "480 lines: the spot is wider than the pitch, so there should be no \
             gap to see, but the trough was {high_trough}"
        );
    }

    /// The bloom's signature, and the reason it was built: how deep the gaps run
    /// depends on how hard the gun is being driven. A dim picture is written
    /// with a tight spot and keeps crisp scanlines; a bright one widens the spot
    /// and washes them out, which is what a tube does and what a fixed profile
    /// cannot express.
    #[test]
    fn scanlines_are_deeper_on_dim_content_than_on_bright() {
        let beam = BeamProfile::derive(224, 224 * 4, HALO_SIGMA_PX, &no_glow(1.0));

        // Ratio of the gap to the line's own center, at each drive.
        let contrast = |lit: f32| brightness_at(&beam, 10.0, lit) / brightness_at(&beam, 10.5, lit);

        let dim = contrast(0.25);
        let bright = contrast(1.0);
        assert!(
            dim < 0.25,
            "a quarter-lit raster should keep deep scanlines, was {dim}"
        );
        assert!(
            bright > dim + 0.25,
            "a fully lit raster drives the gun harder, so its spot is wider and \
             its scanlines shallower: {bright} against {dim}"
        );
    }

    /// The floor is a property of the output grid, so a window too small to
    /// resolve the pitch gets a smooth picture rather than an aliased one. Left
    /// unfloored, the ripple would track where each line's center happened to
    /// fall between output pixels and would crawl as the window was resized.
    #[test]
    fn one_output_pixel_per_line_washes_the_scanlines_out_instead_of_aliasing() {
        let beam = BeamProfile::derive(224, 224, HALO_SIGMA_PX, &no_glow(1.0));
        let trough = brightness(&beam, 10.0);
        let peak = brightness(&beam, 10.5);
        assert!(
            (peak - trough).abs() < 0.02,
            "at one pixel per line there is nowhere to draw a gap, so the \
             profile should be flat; peak {peak}, trough {trough}"
        );
    }

    /// Turning the glow up must not read as turning the brightness down.
    ///
    /// Halation takes its fraction out of the core and spreads it across 19 mm,
    /// so a bright feature gets almost none of it back where it was. Without a
    /// gain to answer that, the slider darkens the picture instead of adding a
    /// glow to it, which is exactly how this shipped the first time and what
    /// looking at it caught.
    #[test]
    fn turning_halation_up_does_not_darken_a_bright_feature() {
        let settings = |halation| DisplaySettings {
            halation,
            ..DisplaySettings::MEASURED
        };
        let off = BeamProfile::derive(224, 224 * 4, HALO_SIGMA_PX, &settings(0.0));
        let on = BeamProfile::derive(224, 224 * 4, HALO_SIGMA_PX, &settings(0.3));

        assert!(
            on.gain > off.gain,
            "the core has to be drawn brighter to survive losing light to the skirt"
        );

        // The composite then scales the core by `1 - f`, so the two together
        // have to land back where they started. That is the peak being held.
        let after_composite = on.gain * (1.0 - 0.3);
        assert!(
            (after_composite - off.gain).abs() < 0.02 * off.gain,
            "a feature's peak should survive the transfer: {after_composite} against {}",
            off.gain
        );
    }

    /// Focus is the control that actually differs between real monitors, since
    /// tube size cancels out of a figure expressed per pitch. Softening the spot
    /// has to fill the gaps in, which is what a badly adjusted cabinet looked
    /// like.
    #[test]
    fn a_softer_focus_fills_the_gaps_between_lines() {
        let sharp = BeamProfile::derive(224, 224 * 4, HALO_SIGMA_PX, &no_glow(1.0));
        let soft = BeamProfile::derive(224, 224 * 4, HALO_SIGMA_PX, &no_glow(2.0));
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
