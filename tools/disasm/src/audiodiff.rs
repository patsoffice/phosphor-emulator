//! `audiodiff` — compare two WAV captures and report where they differ.
//!
//! The sibling of `imgdiff`, and the same bargain: one command, a printed
//! summary, and a non-zero exit when a tolerance is exceeded, so audio can gate
//! in CI the way pixels already do.
//!
//! It exists because the Python it replaces never could gate — it needs numpy
//! and a nix-shell, and a human to read the table. Everything it measures is
//! arithmetic `phosphor_core::audio::analysis` now does, over WAVs `disasm
//! frameshot --audio-out` already writes.
//!
//! # Reading the report
//!
//! The band-energy deltas are the column to read first. They are
//! scale-invariant, so a pure gain difference leaves every one of them at zero
//! and anything large is a filter, mix or source difference. That distinction —
//! "too quiet" versus "wrong shape" — is the one a single spectral-distance
//! number cannot make, and it is the one that says whether to look at an output
//! stage or at a filter.

use std::path::Path;

use phosphor_core::audio::analysis::{
    self, BAND_EDGES_HZ, analyze, envelope_alignment, gain_ratio, remove_dc, stft_distance,
};

/// How to fold a multi-channel WAV down to the mono the analysis works in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelPolicy {
    /// Require the file to already be mono.
    Mono,
    /// Take the left channel.
    Left,
    /// Take the right channel.
    Right,
    /// Average all channels.
    Downmix,
}

impl std::str::FromStr for ChannelPolicy {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "mono" => Ok(Self::Mono),
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "downmix" => Ok(Self::Downmix),
            other => Err(format!(
                "unknown channel policy {other:?} (mono, left, right, downmix)"
            )),
        }
    }
}

/// A decoded capture: mono `f64` samples in `[-1, 1]`, plus where they came from.
#[derive(Clone, Debug)]
pub struct Capture {
    pub samples: Vec<f64>,
    pub sample_rate: f64,
    pub channels: u16,
    pub bits: u16,
}

/// Decode a PCM WAV.
///
/// Accepts 8-bit unsigned, 16/24/32-bit signed integer and 32-bit float PCM,
/// which covers what MAME's `-wavwrite`, Audacity and `disasm --audio-out`
/// produce. Anything else fails with the conversion to run rather than being
/// silently misread — a misread capture would become a fitting target.
pub fn read_wav(path: &Path, policy: ChannelPolicy) -> Result<Capture, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("reading {}: {e}", path.display()))?;
    let ctx = |m: String| format!("{}: {m}", path.display());

    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(ctx("not a RIFF/WAVE file".into()));
    }

    let u16at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
    let u32at = |o: usize| u32::from_le_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);

    // Walk the chunk list rather than assuming `fmt ` then `data`: real files
    // carry LIST/INFO and fact chunks between them.
    let (mut fmt, mut data) = (None, None);
    let mut pos = 12usize;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32at(pos + 4) as usize;
        let body = pos + 8;
        if body + size > bytes.len() {
            // A truncated final chunk is common in interrupted captures; take
            // what is there rather than refusing the whole file.
            if id == b"data" {
                data = Some((body, bytes.len() - body));
            }
            break;
        }
        match id {
            b"fmt " => fmt = Some(body),
            b"data" => data = Some((body, size)),
            _ => {}
        }
        // Chunks are word-aligned: an odd size is followed by a pad byte.
        pos = body + size + (size & 1);
    }

    let fmt = fmt.ok_or_else(|| ctx("no fmt chunk".into()))?;
    let (data_at, data_len) = data.ok_or_else(|| ctx("no data chunk".into()))?;

    let format = u16at(fmt);
    let channels = u16at(fmt + 2);
    let sample_rate = u32at(fmt + 4) as f64;
    let bits = u16at(fmt + 14);

    if channels == 0 {
        return Err(ctx("zero channels".into()));
    }
    if policy == ChannelPolicy::Mono && channels != 1 {
        return Err(ctx(format!(
            "{channels} channels but --channels mono was requested; \
             pass --channels downmix, left or right"
        )));
    }

    // 0x0001 = integer PCM, 0x0003 = IEEE float, 0xFFFE = extensible (the real
    // format lives in the extension's sub-format GUID, whose first two bytes
    // repeat the tag).
    let float = match format {
        1 => false,
        3 => true,
        0xFFFE if fmt + 26 <= bytes.len() => u16at(fmt + 24) == 3,
        other => {
            return Err(ctx(format!(
                "unsupported WAV format tag {other} (need PCM or float; \
                 convert with `ffmpeg -i in.wav -c:a pcm_s16le out.wav`)"
            )));
        }
    };

    let bytes_per = (bits / 8) as usize;
    if bytes_per == 0 {
        return Err(ctx(format!("unsupported bit depth {bits}")));
    }
    let frame = bytes_per * channels as usize;
    let frames = data_len / frame;

    let sample_at = |off: usize| -> f64 {
        let b = &bytes[off..off + bytes_per];
        match (float, bits) {
            (true, 32) => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            (true, 64) => f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
            // 8-bit WAV is unsigned with 128 as the zero point; every wider
            // depth is two's-complement signed.
            (false, 8) => (b[0] as f64 - 128.0) / 128.0,
            (false, 16) => i16::from_le_bytes([b[0], b[1]]) as f64 / -(i16::MIN as f64),
            (false, 24) => {
                let v = ((b[2] as i32) << 24 | (b[1] as i32) << 16 | (b[0] as i32) << 8) >> 8;
                v as f64 / 8_388_608.0
            }
            (false, 32) => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64 / -(i32::MIN as f64),
            _ => 0.0,
        }
    };

    if !float && !matches!(bits, 8 | 16 | 24 | 32) {
        return Err(ctx(format!("unsupported integer bit depth {bits}")));
    }
    if float && !matches!(bits, 32 | 64) {
        return Err(ctx(format!("unsupported float bit depth {bits}")));
    }

    let pick = |ch: usize| ch.min(channels as usize - 1);
    let samples: Vec<f64> = (0..frames)
        .map(|f| {
            let base = data_at + f * frame;
            match policy {
                ChannelPolicy::Mono | ChannelPolicy::Left => sample_at(base),
                ChannelPolicy::Right => sample_at(base + pick(1) * bytes_per),
                ChannelPolicy::Downmix => {
                    let sum: f64 = (0..channels as usize)
                        .map(|c| sample_at(base + c * bytes_per))
                        .sum();
                    sum / channels as f64
                }
            }
        })
        .collect();

    Ok(Capture {
        samples,
        sample_rate,
        channels,
        bits,
    })
}

/// Restrict a capture to `start..end` seconds.
///
/// The reason this exists: a capture that walks through several effects on a
/// timeline averages them together, and the average describes none of them. A
/// board whose walk is too bright and whose stomp is too dark reads as roughly
/// correct overall. Comparing one effect at a time is the only way the numbers
/// mean anything, and until the scenario runner exists this is how to do it.
pub fn slice_seconds(capture: &Capture, start_s: f64, end_s: f64) -> Result<Capture, String> {
    let to_index = |t: f64| (t.max(0.0) * capture.sample_rate) as usize;
    let (a, b) = (
        to_index(start_s),
        to_index(end_s).min(capture.samples.len()),
    );
    if a >= b {
        return Err(format!(
            "empty range {start_s}..{end_s} s in a {:.3} s capture",
            capture.samples.len() as f64 / capture.sample_rate
        ));
    }
    Ok(Capture {
        samples: capture.samples[a..b].to_vec(),
        sample_rate: capture.sample_rate,
        channels: capture.channels,
        bits: capture.bits,
    })
}

/// Parse a `START:END` range in seconds.
pub fn parse_range(s: &str) -> Result<(f64, f64), String> {
    let (a, b) = s
        .split_once(':')
        .ok_or_else(|| format!("range {s:?} should look like START:END, in seconds"))?;
    let parse = |t: &str, which: &str| -> Result<f64, String> {
        t.trim()
            .parse::<f64>()
            .map_err(|_| format!("{which} of range {s:?} is not a number"))
    };
    Ok((parse(a, "start")?, parse(b, "end")?))
}

/// Sample-wise `a - b`, for isolating what one capture has that another does not.
///
/// The use this exists for: a game's audio cannot be captured one effect at a
/// time, because the music plays throughout. But two runs of the *same* machine
/// on the *same* input schedule, differing only in whether one control is held,
/// are identical until that control matters — verified, not assumed: the
/// pre-input window of two such captures diffs to a multi-resolution STFT
/// distance of exactly 0. Subtracting them therefore cancels the music and
/// leaves the effect.
///
/// That makes a real-gameplay effect comparison possible between two emulators
/// that share no state: difference each side's pair, then compare the two
/// differences. Neither side needs its sound latch poked by hand, so the
/// trigger pattern is whatever the game actually emits.
///
/// Requires a common sample rate and sample alignment, which holds within one
/// emulator's pair and does not across two — so subtract per side, then compare
/// the results spectrally.
pub fn subtract(a: &Capture, b: &Capture) -> Result<Capture, String> {
    if a.sample_rate != b.sample_rate {
        return Err(format!(
            "cannot subtract captures at different rates ({} vs {} Hz); \
             subtract within one emulator's pair, then compare the differences",
            a.sample_rate, b.sample_rate
        ));
    }
    let n = a.samples.len().min(b.samples.len());
    if n == 0 {
        return Err("nothing to subtract: one capture is empty".into());
    }
    Ok(Capture {
        samples: (0..n).map(|i| a.samples[i] - b.samples[i]).collect(),
        sample_rate: a.sample_rate,
        channels: 1,
        bits: a.bits,
    })
}

/// Write a capture as a 16-bit mono WAV.
pub fn write_wav(path: &Path, capture: &Capture) -> Result<(), String> {
    use std::io::Write;
    let pcm: Vec<i16> = capture
        .samples
        .iter()
        .map(|s| (s * -(i16::MIN as f64)).clamp(i16::MIN as f64, i16::MAX as f64) as i16)
        .collect();
    let rate = capture.sample_rate as u32;
    let data_len = (pcm.len() * 2) as u32;
    let mut f = std::io::BufWriter::new(
        std::fs::File::create(path).map_err(|e| format!("creating {}: {e}", path.display()))?,
    );
    let mut put = |b: &[u8]| -> Result<(), String> {
        f.write_all(b)
            .map_err(|e| format!("writing {}: {e}", path.display()))
    };
    put(b"RIFF")?;
    put(&(36 + data_len).to_le_bytes())?;
    put(b"WAVE")?;
    put(b"fmt ")?;
    put(&16u32.to_le_bytes())?;
    put(&1u16.to_le_bytes())?;
    put(&1u16.to_le_bytes())?;
    put(&rate.to_le_bytes())?;
    put(&(rate * 2).to_le_bytes())?;
    put(&2u16.to_le_bytes())?;
    put(&16u16.to_le_bytes())?;
    put(b"data")?;
    put(&data_len.to_le_bytes())?;
    for s in &pcm {
        put(&s.to_le_bytes())?;
    }
    f.flush()
        .map_err(|e| format!("writing {}: {e}", path.display()))
}

/// Tolerances past which `audiodiff` exits non-zero.
///
/// Defaults are deliberately loose: this gates against gross regressions —
/// silence, saturation, a voice vanishing — not against the last percent of
/// fidelity, which needs a trusted reference and belongs to `sndcmp`.
#[derive(Clone, Copy, Debug)]
pub struct Tolerance {
    /// Largest allowed absolute band-ratio delta, in percentage points.
    pub band_pp: f64,
    /// Largest allowed centroid difference, as a fraction of the larger value.
    pub centroid_frac: f64,
    /// Largest allowed RMS difference in dB.
    pub rms_db: f64,
    /// Largest allowed difference between the two captures' discontinuity
    /// ratios. Compared side to side rather than against a fixed ceiling
    /// because a genuinely impulsive effect has a high ratio on both sides and
    /// is not defective; what matters is one side jumping where the other does
    /// not.
    pub step_ratio: f64,
}

impl Default for Tolerance {
    fn default() -> Self {
        Self {
            band_pp: 10.0,
            centroid_frac: 0.25,
            rms_db: 6.0,
            step_ratio: 2.0,
        }
    }
}

/// Compare two captures and render the report.
///
/// Returns the text and whether every tolerance held.
pub fn compare(
    a: &Capture,
    b: &Capture,
    label_a: &str,
    label_b: &str,
    tol: Tolerance,
) -> (String, Verdict) {
    use std::fmt::Write;

    let aa = analyze(&a.samples, a.sample_rate);
    let ba = analyze(&b.samples, b.sample_rate);
    let mut s = String::new();
    let mut verdict = Verdict::default();

    let _ = writeln!(s, "{:<26} {:>14} {:>14}   delta", "", label_a, label_b);
    let _ = writeln!(s, "{}", "-".repeat(72));

    // --- shape of the files themselves ---
    let _ = writeln!(
        s,
        "{:<26} {:>14} {:>14}",
        "duration (s)",
        format!("{:.3}", aa.duration_s()),
        format!("{:.3}", ba.duration_s())
    );
    let _ = writeln!(
        s,
        "{:<26} {:>14} {:>14}",
        "sample rate (Hz)", a.sample_rate as u64, b.sample_rate as u64
    );
    let _ = writeln!(
        s,
        "{:<26} {:>14} {:>14}",
        "channels / bits",
        format!("{}/{}", a.channels, a.bits),
        format!("{}/{}", b.channels, b.bits)
    );

    if a.sample_rate != b.sample_rate {
        let _ = writeln!(
            s,
            "\n  [!] sample rates differ; spectra are still comparable but \
             time-domain\n      measurements are not directly aligned."
        );
    }

    // --- integrity: is either capture defective? ---
    let _ = writeln!(s, "\nintegrity");
    row(
        &mut s,
        "  DC offset",
        aa.integrity.dc_offset,
        ba.integrity.dc_offset,
        5,
    );
    row(
        &mut s,
        "  peak (dBFS)",
        aa.integrity.peak_dbfs,
        ba.integrity.peak_dbfs,
        2,
    );
    row(
        &mut s,
        "  clipped (%)",
        aa.integrity.clipped_fraction * 100.0,
        ba.integrity.clipped_fraction * 100.0,
        3,
    );
    row(
        &mut s,
        "  crest factor",
        aa.integrity.crest_factor,
        ba.integrity.crest_factor,
        2,
    );
    row(
        &mut s,
        "  silent (%)",
        aa.integrity.silent_fraction * 100.0,
        ba.integrity.silent_fraction * 100.0,
        1,
    );

    // --- level ---
    let _ = writeln!(s, "\nlevel");
    row(
        &mut s,
        "  AC RMS (dBFS)",
        aa.level.rms_dbfs,
        ba.level.rms_dbfs,
        2,
    );
    let gain = gain_ratio(&remove_dc(&a.samples), &remove_dc(&b.samples));
    let _ = writeln!(
        s,
        "{:<26} {:>14} {:>14}   {:.2}x ({:+.2} dB)",
        "  gain ratio (a/b)",
        "",
        "",
        gain,
        20.0 * gain.max(1e-10).log10()
    );
    opt_row(&mut s, "  onset (s)", aa.level.onset_s, ba.level.onset_s, 3);

    // Every decay number below is measured inside ONE event, and these three
    // rows are how a reader knows which one. A capture holding a train of
    // footsteps used to have its decay measured across the whole train, which
    // is a number that moves when the steps' relative loudness moves and not
    // when the envelope does. The count is the row to check first: if the two
    // sides found different numbers of events, they are not decaying
    // differently, they are triggering differently, and the T20 delta below is
    // not the difference to chase.
    row(
        &mut s,
        "  events",
        aa.level.events.count() as f64,
        ba.level.events.count() as f64,
        0,
    );
    opt_row(
        &mut s,
        "  event spacing (s)",
        aa.level.events.spacing_s,
        ba.level.events.spacing_s,
        3,
    );
    row(
        &mut s,
        "  event window (s)",
        aa.level.event_window_s,
        ba.level.event_window_s,
        3,
    );
    opt_row(
        &mut s,
        "  attack (s)",
        aa.level.attack_s,
        ba.level.attack_s,
        4,
    );
    opt_row(
        &mut s,
        "  decay T20 (s)",
        aa.level.decay_t20_s,
        ba.level.decay_t20_s,
        3,
    );
    opt_row(
        &mut s,
        "  decay T40 (s)",
        aa.level.decay_t40_s,
        ba.level.decay_t40_s,
        3,
    );
    // The fitted time constant, which is the one to read on a noisy voice: T20
    // and T40 take two points off a curve that a shift-register-gated effect
    // moves around, where this fits the whole decay. The r² is printed with it
    // because a low one means the decay is not a single exponential and the tau
    // should not be quoted at all.
    opt_row(
        &mut s,
        "  decay tau (s)",
        aa.level.decay_tau_s.map(|(t, _)| t),
        ba.level.decay_tau_s.map(|(t, _)| t),
        3,
    );
    opt_row(
        &mut s,
        "  decay fit r2",
        aa.level.decay_tau_s.map(|(_, r)| r),
        ba.level.decay_tau_s.map(|(_, r)| r),
        3,
    );
    // Per-event, where AC RMS above is per-capture: over a train the two answer
    // different questions and only this one is comparable between captures that
    // hold different numbers of events.
    row(
        &mut s,
        "  event energy",
        aa.level.event_energy,
        ba.level.event_energy,
        3,
    );

    // --- discontinuity: the only line here that can see a transient defect ---
    //
    // Everything above and below averages over a window, and Asteroids' thump
    // proved what that costs: it gated a free-running 555 at the output, so
    // every onset connected the oscillator at whatever phase it was passing,
    // and the largest step at the onset was 3636 against the corrected 272.
    // That one sample in ninety thousand moved no RMS, no crest factor, no
    // centroid and no band share, across five windows, while being plainly
    // audible. The ratio is the column to read: a square wave's every edge is a
    // full-swing step, so its ratio is near 1 and the metric does not accuse a
    // sharp waveform of anything.
    let _ = writeln!(s, "\ndiscontinuity");
    row(
        &mut s,
        "  max step",
        aa.discontinuity.max_step,
        ba.discontinuity.max_step,
        5,
    );
    row(
        &mut s,
        "  typical step (99.9%)",
        aa.discontinuity.typical_step,
        ba.discontinuity.typical_step,
        5,
    );
    row(
        &mut s,
        "  ratio max/typical",
        aa.discontinuity.ratio(),
        ba.discontinuity.ratio(),
        2,
    );
    row(
        &mut s,
        "  max step at (s)",
        aa.discontinuity.max_step_s,
        ba.discontinuity.max_step_s,
        4,
    );
    // Where the jump sits relative to each side's own onset, because "at the
    // onset" is the answer that names a gate in the wrong place, and "in the
    // middle" is a different fault entirely.
    opt_row(
        &mut s,
        "  ...after own onset (s)",
        aa.level.onset_s.map(|o| aa.discontinuity.max_step_s - o),
        ba.level.onset_s.map(|o| ba.discontinuity.max_step_s - o),
        4,
    );

    // --- spectrum ---
    let _ = writeln!(s, "\nspectrum");
    row(
        &mut s,
        "  centroid (Hz)",
        aa.spectrum.centroid_hz,
        ba.spectrum.centroid_hz,
        1,
    );
    row(
        &mut s,
        "  rolloff 85% (Hz)",
        aa.spectrum.rolloff_hz,
        ba.spectrum.rolloff_hz,
        1,
    );
    row(
        &mut s,
        "  flatness",
        aa.spectrum.flatness,
        ba.spectrum.flatness,
        4,
    );
    row(
        &mut s,
        "  fundamental (Hz)",
        aa.spectrum.fundamental_hz,
        ba.spectrum.fundamental_hz,
        1,
    );

    // --- band energy: the column to read first ---
    let _ = writeln!(
        s,
        "\nband energy (% of total) — scale-invariant, so a gain error leaves these at 0"
    );
    for (i, ratio) in aa.spectrum.band_ratios.iter().enumerate() {
        let lo = BAND_EDGES_HZ[i];
        let hi = BAND_EDGES_HZ.get(i + 1).copied();
        let name = match hi {
            Some(h) => format!("  {lo:.0}-{h:.0} Hz"),
            None => format!("  {lo:.0}+ Hz"),
        };
        let (pa, pb) = (ratio * 100.0, ba.spectrum.band_ratios[i] * 100.0);
        let delta = pa - pb;
        let flag = if delta.abs() > tol.band_pp {
            verdict
                .differences
                .push(format!("band {lo:.0} Hz energy differs by {delta:+.1} pp"));
            "  [!]"
        } else {
            ""
        };
        let _ = writeln!(s, "{name:<26} {pa:>14.2} {pb:>14.2}   {delta:+.2} pp{flag}");
    }

    // --- one scalar for ranking, explicitly not the only evidence ---
    let (ac, bc) = (remove_dc(&a.samples), remove_dc(&b.samples));
    let d = stft_distance(&ac, &bc, &[256, 1024, 4096]);
    let _ = writeln!(
        s,
        "\nmulti-resolution STFT distance: {d:.4}  (0 = identical)"
    );

    let hop = ((a.sample_rate * 0.005) as usize).max(1);
    let al = envelope_alignment(&ac, &bc, hop, 200);
    // A sustained voice has no timing information in it, so its best offset is
    // whatever the noise favoured. Saying so beside the number is the difference
    // between a diagnostic and a trap: an offset reported bare gets read as a
    // real lag, and a 12 ms slide on a 40 ms effect has already cost this
    // project a full misdiagnosis once.
    //
    // The cutoff is measured rather than chosen. Against a byte-identical copy
    // of itself, every transient tried clears it and is reported bare: Donkey
    // Kong's stomp and jump, Galaxian's fire, Asteroids' ship fire. Asteroids'
    // thrust, a sustained rumble, reaches only 0.028 against its own copy, and
    // 0.021 against the board. So the line falls between "the effect says where
    // it is" and "it does not", which is the distinction worth drawing, and a
    // self-comparison being caveated is correct rather than embarrassing: the
    // offset is genuinely zero and the signal genuinely cannot prove it.
    const DETERMINED: f64 = 0.05;
    let _ = writeln!(
        s,
        "envelope alignment: {:+.3} s  (b relative to a){}",
        al.shift_samples as f64 / a.sample_rate,
        if al.prominence() < DETERMINED {
            format!(
                "\n  WEAKLY DETERMINED: the peak stands only {:.3} above a \
                 typical offset. A sustained voice does not say where it is in \
                 time, so this is not a lag.",
                al.prominence()
            )
        } else {
            String::new()
        }
    );

    // --- verdict, kept in two buckets ---
    //
    // A difference between the captures and a defect in one of them are not the
    // same finding and must not be reported as one. Two byte-identical WAVs
    // that both carry a huge DC offset differ in nothing at all; saying they
    // "differ beyond tolerance" would send someone looking for a regression
    // that is not there, when what they have is one broken capture recorded
    // twice.
    let centroid_max = aa.spectrum.centroid_hz.max(ba.spectrum.centroid_hz);
    if centroid_max > 0.0
        && (aa.spectrum.centroid_hz - ba.spectrum.centroid_hz).abs() / centroid_max
            > tol.centroid_frac
    {
        verdict
            .differences
            .push("spectral centroid differs beyond tolerance".into());
        let _ = writeln!(s, "\n  [!] centroid differs by more than the tolerance");
    }
    if (aa.level.rms_dbfs - ba.level.rms_dbfs).abs() > tol.rms_db {
        verdict
            .differences
            .push(format!("AC RMS differs by more than {} dB", tol.rms_db));
        let _ = writeln!(s, "  [!] AC RMS differs by more than {} dB", tol.rms_db);
    }
    let (ra, rb) = (aa.discontinuity.ratio(), ba.discontinuity.ratio());
    if (ra - rb).abs() > tol.step_ratio {
        let louder = if ra > rb { label_a } else { label_b };
        verdict.differences.push(format!(
            "{louder} jumps where the other does not ({ra:.2} against {rb:.2} \
             max/typical step)"
        ));
        let _ = writeln!(
            s,
            "  [!] {louder} carries a step the other capture does not \
             ({ra:.2} against {rb:.2}); look at where it happens above, not at \
             how big it is"
        );
    }
    for (label, integrity) in [(label_a, &aa.integrity), (label_b, &ba.integrity)] {
        if integrity.is_silent {
            verdict.defects.push(format!("{label} is silent"));
            let _ = writeln!(s, "  [!] {label} is silent");
        }
        if integrity.dc_offset.abs() > 0.05 {
            verdict.defects.push(format!(
                "{label} carries a DC offset of {:.4}",
                integrity.dc_offset
            ));
            let _ = writeln!(
                s,
                "  [!] {label} carries a large DC offset ({:.4})",
                integrity.dc_offset
            );
        }
        if integrity.clipped_fraction > 0.01 {
            verdict.defects.push(format!(
                "{label} clips on {:.1}% of samples",
                integrity.clipped_fraction * 100.0
            ));
            let _ = writeln!(
                s,
                "  [!] {label} clips on {:.1}% of samples",
                integrity.clipped_fraction * 100.0
            );
        }
    }

    let _ = writeln!(s, "\n{}", verdict.summary());
    (s, verdict)
}

/// What a comparison concluded, split so the two kinds of finding stay apart.
#[derive(Clone, Debug, Default)]
pub struct Verdict {
    /// Ways the two captures disagree with each other.
    pub differences: Vec<String>,
    /// Ways one capture is defective regardless of the other.
    pub defects: Vec<String>,
}

impl Verdict {
    /// True when nothing was flagged at all.
    pub fn is_clean(&self) -> bool {
        self.differences.is_empty() && self.defects.is_empty()
    }

    /// One line naming what actually failed, suitable for stderr and for a CI
    /// log that only keeps the last line.
    pub fn summary(&self) -> String {
        match (self.differences.len(), self.defects.len()) {
            (0, 0) => "within tolerance".into(),
            (0, d) => format!("CAPTURE DEFECTS ({d}): {}", self.defects.join("; ")),
            (n, 0) => format!("OUT OF TOLERANCE ({n}): {}", self.differences.join("; ")),
            (n, d) => format!(
                "OUT OF TOLERANCE ({n}) AND CAPTURE DEFECTS ({d}): {}; {}",
                self.differences.join("; "),
                self.defects.join("; ")
            ),
        }
    }
}

/// One metric line with an absolute delta.
fn row(s: &mut String, label: &str, a: f64, b: f64, precision: usize) {
    use std::fmt::Write;
    let _ = writeln!(
        s,
        "{label:<26} {a:>14.precision$} {b:>14.precision$}   {:+.precision$}",
        a - b
    );
}

/// The same for a metric that may be absent, which is meaningful rather than
/// missing — a sustained tone genuinely has no decay time.
fn opt_row(s: &mut String, label: &str, a: Option<f64>, b: Option<f64>, precision: usize) {
    use std::fmt::Write;
    let fmt = |v: Option<f64>| match v {
        Some(v) => format!("{v:.precision$}"),
        None => "-".to_string(),
    };
    let delta = match (a, b) {
        (Some(a), Some(b)) => format!("{:+.precision$}", a - b),
        _ => "-".to_string(),
    };
    let _ = writeln!(s, "{label:<26} {:>14} {:>14}   {delta}", fmt(a), fmt(b));
}

/// Analyze one capture on its own, for when there is nothing to compare against.
pub fn describe(capture: &Capture, label: &str) -> String {
    use std::fmt::Write;
    let a = analyze(&capture.samples, capture.sample_rate);
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{label}: {:.3} s, {} Hz, {} ch, {}-bit",
        a.duration_s(),
        capture.sample_rate as u64,
        capture.channels,
        capture.bits
    );
    let _ = writeln!(
        s,
        "  DC {:.5}  peak {:.2} dBFS  clipped {:.3}%  crest {:.2}  silent {:.1}%",
        a.integrity.dc_offset,
        a.integrity.peak_dbfs,
        a.integrity.clipped_fraction * 100.0,
        a.integrity.crest_factor,
        a.integrity.silent_fraction * 100.0
    );
    let _ = writeln!(
        s,
        "  AC RMS {:.2} dBFS  centroid {:.1} Hz  rolloff {:.1} Hz  flatness {:.4}  f0 {:.1} Hz",
        a.level.rms_dbfs,
        a.spectrum.centroid_hz,
        a.spectrum.rolloff_hz,
        a.spectrum.flatness,
        a.spectrum.fundamental_hz
    );
    // The event line before the decay numbers, not after, because it is what
    // says whether they describe one effect or a train of them.
    let fmt = |v: Option<f64>, p: usize| match v {
        Some(v) => format!("{v:.*}", p),
        None => "-".to_string(),
    };
    let _ = writeln!(
        s,
        "  events {} spaced {} s  window {:.3} s  attack {} s",
        a.level.events.count(),
        fmt(a.level.events.spacing_s, 3),
        a.level.event_window_s,
        fmt(a.level.attack_s, 4)
    );
    let _ = writeln!(
        s,
        "  per event: T20 {} s  T40 {} s  tau {} s (r2 {})",
        fmt(a.level.decay_t20_s, 3),
        fmt(a.level.decay_t40_s, 3),
        fmt(a.level.decay_tau_s.map(|(t, _)| t), 3),
        fmt(a.level.decay_tau_s.map(|(_, r)| r), 3)
    );
    let bands: Vec<String> = a
        .spectrum
        .band_ratios
        .iter()
        .map(|r| format!("{:.1}", r * 100.0))
        .collect();
    let _ = writeln!(s, "  bands (%): {}", bands.join(" / "));
    let _ = writeln!(
        s,
        "  max step {:.5} at {:.4} s, {:.2}x the typical step",
        a.discontinuity.max_step,
        a.discontinuity.max_step_s,
        a.discontinuity.ratio()
    );
    s
}

/// Render a capture's STFT as a log-magnitude spectrogram PNG.
///
/// Time runs left to right and frequency bottom to top, with a
/// perceptually-ordered colormap so a bright band reads as loud. Needs no
/// plotting library — an STFT, a colormap and the existing PNG writer — which
/// also means `imgdiff` can then diff two spectrograms.
pub fn spectrogram(capture: &Capture, out: &Path, height: u32) -> Result<String, String> {
    const WINDOW: usize = 1024;
    let ac = remove_dc(&capture.samples);
    let hop = (ac.len() / 800).max(WINDOW / 4);
    let frames = analysis::fft::stft(&ac, WINDOW, hop);
    if frames.is_empty() {
        return Err("capture is shorter than one analysis window".into());
    }

    let width = frames.len() as u32;
    let bins = frames[0].len();
    let mut rgb = vec![0u8; (width * height * 3) as usize];

    // One reference level for the whole image, never per frame: normalizing
    // each column separately would make a silent passage look as loud as a
    // fortissimo one, which is the single worst thing a spectrogram can do.
    //
    // The reference is a high percentile rather than the maximum. A capture
    // with one loud click and an otherwise quiet body — which is most arcade
    // attract-mode audio — would put its entire body more than 80 dB below the
    // max and render as a black rectangle with one bright column. The
    // percentile is set by the loud *content* instead of by the loudest single
    // bin, so the body of the capture stays visible.
    let mut all: Vec<f64> = frames.iter().flat_map(|f| f.iter().copied()).collect();
    all.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let peak = all[(all.len() as f64 * 0.999) as usize % all.len()].max(1e-12);
    const RANGE_DB: f64 = 80.0;

    // Log frequency axis spanning 20 Hz to Nyquist, so each octave gets equal
    // height. A linear axis would spend half the image on the top octave, where
    // arcade audio has least to say; mapping bin index logarithmically instead
    // of frequency overcorrects the other way, handing an eighth of the image
    // to the first two bins.
    // Start at the first non-DC bin rather than at 20 Hz: below one bin width
    // there is nothing to resolve, and stretching bin 0 over the bottom tenth
    // of the image paints a flat band that looks like content but is one number
    // repeated.
    let f_min = capture.sample_rate / WINDOW as f64;
    let f_max = (capture.sample_rate / 2.0).max(f_min * 2.0);
    let hz_to_bin = |hz: f64| (hz * WINDOW as f64 / capture.sample_rate).round() as usize;

    for (x, frame) in frames.iter().enumerate() {
        for y in 0..height {
            let frac = 1.0 - y as f64 / height as f64;
            let hz = f_min * (f_max / f_min).powf(frac);
            let bin = hz_to_bin(hz);
            let mag = frame[bin.min(bins - 1)];
            let db = 20.0 * (mag / peak).max(1e-12).log10();
            let level = ((db + RANGE_DB) / RANGE_DB).clamp(0.0, 1.0);
            let (r, g, b) = magma(level);
            let i = ((y * width + x as u32) * 3) as usize;
            rgb[i] = r;
            rgb[i + 1] = g;
            rgb[i + 2] = b;
        }
    }

    crate::gfxsheet::write_png(out, &rgb, width, height)
        .map_err(|e| format!("writing {}: {e}", out.display()))?;
    Ok(format!(
        "wrote {} ({width}x{height}, {:.0} ms/column)",
        out.display(),
        hop as f64 / capture.sample_rate * 1000.0
    ))
}

/// Piecewise-linear approximation of the magma colormap.
///
/// Perceptually ordered — monotonic in lightness — so "brighter is louder"
/// survives being printed in greyscale or looked at by someone colour-blind,
/// which a rainbow map does not.
fn magma(t: f64) -> (u8, u8, u8) {
    const STOPS: [(f64, f64, f64, f64); 6] = [
        (0.0, 0.0, 0.0, 0.02),
        (0.2, 0.18, 0.07, 0.35),
        (0.4, 0.45, 0.12, 0.51),
        (0.6, 0.75, 0.21, 0.42),
        (0.8, 0.96, 0.49, 0.25),
        (1.0, 0.99, 0.99, 0.75),
    ];
    let t = t.clamp(0.0, 1.0);
    let hi = STOPS
        .iter()
        .position(|s| t <= s.0)
        .unwrap_or(STOPS.len() - 1);
    let lo = hi.saturating_sub(1);
    let (t0, r0, g0, b0) = STOPS[lo];
    let (t1, r1, g1, b1) = STOPS[hi];
    let k = if (t1 - t0).abs() < f64::EPSILON {
        0.0
    } else {
        (t - t0) / (t1 - t0)
    };
    let mix = |a: f64, b: f64| ((a + (b - a) * k) * 255.0).round().clamp(0.0, 255.0) as u8;
    (mix(r0, r1), mix(g0, g1), mix(b0, b1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a WAV in memory. Mirrors the writer in `main.rs` but parameterized,
    /// so the reader is tested against the layouts it will actually meet rather
    /// than only against our own output.
    fn wav(
        channels: u16,
        bits: u16,
        rate: u32,
        format: u16,
        data: &[u8],
        extra_chunk: bool,
    ) -> Vec<u8> {
        let mut v = Vec::new();
        let fmt_size = 16u32;
        let extra: &[u8] = if extra_chunk {
            // A LIST chunk between fmt and data, which real files carry and a
            // reader that assumes a fixed layout would trip over.
            b"LIST\x04\x00\x00\x00INFO"
        } else {
            b""
        };
        let data_len = data.len() as u32;
        v.extend_from_slice(b"RIFF");
        v.extend_from_slice(&(36 + extra.len() as u32 + data_len).to_le_bytes());
        v.extend_from_slice(b"WAVE");
        v.extend_from_slice(b"fmt ");
        v.extend_from_slice(&fmt_size.to_le_bytes());
        v.extend_from_slice(&format.to_le_bytes());
        v.extend_from_slice(&channels.to_le_bytes());
        v.extend_from_slice(&rate.to_le_bytes());
        let block = (bits / 8) * channels;
        v.extend_from_slice(&(rate * block as u32).to_le_bytes());
        v.extend_from_slice(&block.to_le_bytes());
        v.extend_from_slice(&bits.to_le_bytes());
        v.extend_from_slice(extra);
        v.extend_from_slice(b"data");
        v.extend_from_slice(&data_len.to_le_bytes());
        v.extend_from_slice(data);
        v
    }

    fn write_temp(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("phosphor-audiodiff-{name}.wav"));
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(bytes).unwrap();
        p
    }

    fn pcm16(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn reads_16_bit_mono() {
        let p = write_temp(
            "mono16",
            &wav(
                1,
                16,
                44100,
                1,
                &pcm16(&[0, 16384, -16384, i16::MIN]),
                false,
            ),
        );
        let c = read_wav(&p, ChannelPolicy::Mono).unwrap();
        assert_eq!(c.sample_rate, 44100.0);
        assert_eq!(c.channels, 1);
        assert_eq!(c.bits, 16);
        assert_eq!(c.samples.len(), 4);
        assert_eq!(c.samples[0], 0.0);
        assert!((c.samples[1] - 0.5).abs() < 1e-4);
        assert_eq!(c.samples[3], -1.0);
    }

    /// The policy is the whole point of the flag: silently taking channel 0,
    /// which the Python did, is not good enough for a level comparison.
    #[test]
    fn channel_policy_selects_and_downmixes() {
        // Interleaved stereo: left full scale positive, right full scale negative.
        let data = pcm16(&[16384, -16384, 16384, -16384]);
        let p = write_temp("stereo16", &wav(2, 16, 22050, 1, &data, false));

        let left = read_wav(&p, ChannelPolicy::Left).unwrap();
        assert!((left.samples[0] - 0.5).abs() < 1e-4);

        let right = read_wav(&p, ChannelPolicy::Right).unwrap();
        assert!((right.samples[0] + 0.5).abs() < 1e-4);

        let mixed = read_wav(&p, ChannelPolicy::Downmix).unwrap();
        assert!(mixed.samples[0].abs() < 1e-9, "L+R should cancel");
        assert_eq!(mixed.samples.len(), 2, "two frames, not four samples");

        // Asking for mono on a stereo file is an error rather than a silent
        // channel pick.
        assert!(read_wav(&p, ChannelPolicy::Mono).is_err());
    }

    #[test]
    fn reads_8_bit_unsigned_and_24_and_32_bit() {
        // 8-bit WAV is unsigned around 128.
        let p = write_temp("mono8", &wav(1, 8, 8000, 1, &[128, 255, 0], false));
        let c = read_wav(&p, ChannelPolicy::Mono).unwrap();
        assert_eq!(c.samples[0], 0.0);
        assert!(c.samples[1] > 0.99);
        assert_eq!(c.samples[2], -1.0);

        // 24-bit little-endian signed.
        let mut d24 = Vec::new();
        d24.extend_from_slice(&[0x00, 0x00, 0x00]); // 0
        d24.extend_from_slice(&[0x00, 0x00, 0x40]); // +2^22 = half scale
        let p = write_temp("mono24", &wav(1, 24, 8000, 1, &d24, false));
        let c = read_wav(&p, ChannelPolicy::Mono).unwrap();
        assert_eq!(c.samples[0], 0.0);
        assert!((c.samples[1] - 0.5).abs() < 1e-6, "{}", c.samples[1]);

        // 32-bit float.
        let d32: Vec<u8> = [0.0f32, 0.25, -1.0]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        let p = write_temp("monof32", &wav(1, 32, 8000, 3, &d32, false));
        let c = read_wav(&p, ChannelPolicy::Mono).unwrap();
        assert_eq!(c.samples[0], 0.0);
        assert!((c.samples[1] - 0.25).abs() < 1e-7);
        assert_eq!(c.samples[2], -1.0);
    }

    /// Real captures carry LIST/INFO chunks between `fmt ` and `data`.
    #[test]
    fn skips_unknown_chunks() {
        let p = write_temp(
            "withlist",
            &wav(1, 16, 44100, 1, &pcm16(&[16384, -16384]), true),
        );
        let c = read_wav(&p, ChannelPolicy::Mono).unwrap();
        assert_eq!(c.samples.len(), 2);
        assert!((c.samples[0] - 0.5).abs() < 1e-4);
    }

    /// A format we cannot decode must fail loudly with the conversion to run,
    /// never be misread — a misread capture would become a fitting target.
    #[test]
    fn unsupported_formats_fail_with_advice() {
        // Format tag 0x0011 is IMA ADPCM.
        let p = write_temp("adpcm", &wav(1, 4, 44100, 0x11, &[0u8; 16], false));
        let err = read_wav(&p, ChannelPolicy::Mono).unwrap_err();
        assert!(err.contains("unsupported"), "{err}");
        assert!(err.contains("ffmpeg"), "should say how to convert: {err}");

        let p = write_temp("notwav", b"this is not a wav file at all");
        assert!(read_wav(&p, ChannelPolicy::Mono).is_err());
    }

    /// A capture interrupted mid-write leaves a data chunk shorter than its
    /// header claims. Take what is there rather than refusing the file.
    #[test]
    fn truncated_data_chunk_reads_what_is_present() {
        let mut bytes = wav(1, 16, 44100, 1, &pcm16(&[1000; 100]), false);
        bytes.truncate(bytes.len() - 100);
        let p = write_temp("truncated", &bytes);
        let c = read_wav(&p, ChannelPolicy::Mono).unwrap();
        assert_eq!(c.samples.len(), 50);
    }

    /// Identical captures must produce a clean verdict and a zero distance —
    /// the property that makes this usable as a gate.
    #[test]
    fn identical_captures_are_clean() {
        let rate = 8000.0;
        let sig: Vec<f64> = (0..8000)
            .map(|i| 0.5 * (std::f64::consts::TAU * 440.0 * i as f64 / rate).sin())
            .collect();
        let cap = || Capture {
            samples: sig.clone(),
            sample_rate: rate,
            channels: 1,
            bits: 16,
        };
        let (report, verdict) = compare(&cap(), &cap(), "a", "b", Tolerance::default());
        assert!(verdict.is_clean(), "{}", verdict.summary());
        assert!(report.contains("STFT distance: 0.0000"), "{report}");
        assert_eq!(verdict.summary(), "within tolerance");
    }

    /// A defect present in both captures is not a difference between them.
    /// Reporting it as one would send someone hunting a regression that is not
    /// there.
    #[test]
    fn a_shared_defect_reports_as_a_defect_not_a_difference() {
        let biased = Capture {
            samples: vec![0.6; 8000],
            sample_rate: 8000.0,
            channels: 1,
            bits: 16,
        };
        let other = Capture {
            samples: vec![0.6; 8000],
            sample_rate: 8000.0,
            channels: 1,
            bits: 16,
        };
        let (_, verdict) = compare(&biased, &other, "a", "b", Tolerance::default());
        assert!(!verdict.is_clean());
        assert!(verdict.differences.is_empty(), "{:?}", verdict.differences);
        assert_eq!(verdict.defects.len(), 2, "{:?}", verdict.defects);
        assert!(verdict.summary().starts_with("CAPTURE DEFECTS"));
    }

    /// A gain difference must not move the band ratios, so it is caught by the
    /// RMS tolerance and leaves the bands clean. This is the distinction the
    /// whole report is built around.
    #[test]
    fn a_pure_gain_difference_does_not_move_the_bands() {
        let rate = 8000.0;
        let loud: Vec<f64> = (0..8000)
            .map(|i| 0.5 * (std::f64::consts::TAU * 300.0 * i as f64 / rate).sin())
            .collect();
        let quiet: Vec<f64> = loud.iter().map(|s| s * 0.25).collect();
        let mk = |s: Vec<f64>| Capture {
            samples: s,
            sample_rate: rate,
            channels: 1,
            bits: 16,
        };
        let (_, verdict) = compare(&mk(loud), &mk(quiet), "loud", "quiet", Tolerance::default());
        assert!(
            !verdict.differences.iter().any(|d| d.contains("band")),
            "a gain change moved a band ratio: {:?}",
            verdict.differences
        );
        assert!(
            verdict.differences.iter().any(|d| d.contains("RMS")),
            "a 12 dB gain change should trip the RMS tolerance: {:?}",
            verdict.differences
        );
    }

    /// A filter difference does move the bands, which is the other half of the
    /// same claim.
    #[test]
    fn a_spectral_difference_moves_the_bands() {
        let rate = 8000.0;
        let mk = |f: f64| Capture {
            samples: (0..8000)
                .map(|i| 0.5 * (std::f64::consts::TAU * f * i as f64 / rate).sin())
                .collect(),
            sample_rate: rate,
            channels: 1,
            bits: 16,
        };
        let (_, verdict) = compare(&mk(100.0), &mk(2000.0), "low", "high", Tolerance::default());
        assert!(
            verdict.differences.iter().any(|d| d.contains("band")),
            "moving a tone from 100 Hz to 2 kHz must move the bands: {:?}",
            verdict.differences
        );
    }

    #[test]
    fn channel_policy_parses_from_the_cli() {
        assert_eq!(
            "downmix".parse::<ChannelPolicy>().unwrap(),
            ChannelPolicy::Downmix
        );
        assert!("quadraphonic".parse::<ChannelPolicy>().is_err());
    }

    /// The colormap must be monotonic in lightness, which is what makes
    /// "brighter is louder" survive greyscale printing and colour blindness.
    #[test]
    fn magma_is_monotonic_in_lightness() {
        let luma = |t: f64| {
            let (r, g, b) = magma(t);
            0.2126 * r as f64 + 0.7152 * g as f64 + 0.0722 * b as f64
        };
        let mut last = -1.0;
        for i in 0..=50 {
            let l = luma(i as f64 / 50.0);
            assert!(l > last, "lightness fell at t={}", i as f64 / 50.0);
            last = l;
        }
        // Out-of-range input is clamped rather than wrapping to a dark colour.
        assert_eq!(magma(-1.0), magma(0.0));
        assert_eq!(magma(2.0), magma(1.0));
    }
}
