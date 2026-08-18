//! Input movies: a deterministic record of everything a human fed a machine.
//!
//! A movie is a flat, frame-indexed log of [`InputEvent`]s from power-on. Replay
//! delivers the same events, in the same order, before the same frames, so a
//! machine driven by a movie reaches byte-identical state — which is what lets
//! the golden-frame suite pin a frame of *gameplay* rather than one of attract
//! mode.
//!
//! # Why per event, not per frame
//!
//! It is tempting to accumulate a frame's analog motion into a single delta.
//! [`RelativeCounter::add_delta`] is `pending += delta as i32` — the truncation
//! happens on *every call*, so two 0.6-deltas contribute 0 while one summed
//! 1.2-delta contributes 1, and on the boards using `DrainPolicy::ClampDrop` the
//! remainder is discarded rather than carried, so that divergence never washes
//! out over later frames.
//!
//! **How much this currently bites, stated honestly.** With the default binding
//! sensitivity it does not. SDL's `xrel` is an integer and `DEFAULT_SCALE` is
//! 1.0, so mouse deltas arrive whole and `trunc(a) + trunc(b) == trunc(a + b)`
//! holds trivially — measured across a four-minute Marble Madness capture, all
//! 35,178 analog records were whole numbers and not one of its 15,954
//! multi-delta frames would have summed differently. The divergence becomes real
//! the moment a sensitivity other than 1.0 is set (`BindingSet::set_scale`,
//! persisted per machine in `state.toml`), or a sub-unit input device appears.
//!
//! So the per-event shape is the conservative choice rather than a currently
//! active fix: it costs little (the record block deflates ~4.6× on a real analog
//! trace) and it is correct under a sensitivity change instead of silently
//! wrong. Float payloads are stored as raw [`f32::to_bits`] for the same reason
//! — a replayed value must truncate exactly as the recorded one did.
//!
//! [`RelativeCounter::add_delta`]: phosphor_core::core::input::RelativeCounter::add_delta
//!
//! # Why this format owns no save state
//!
//! A movie deliberately carries no `Saveable` bytes. That keeps it decoupled
//! from `SAVE_VERSION`, so a committed movie stays valid across save-format
//! changes — the property that makes checking one into the golden suite
//! worthwhile. The cost is that a movie must start from power-on.
//!
//! # Integrity
//!
//! `rom_digest` and the trailing checksum are SHA-256, matching the fingerprint
//! the golden-frame suite already uses for frames. Their job here is to catch
//! replaying against the wrong ROM dump or decoding a truncated file — accidents
//! rather than forgery — but there is no reason to reach for something weaker
//! when `digest` and friends are already linked.

use std::fmt;
use std::io::{Read, Write};

use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use sha2::{Digest, Sha256};

use phosphor_core::core::machine::{FrontendMachine, InputControl, InputEvent, InputId};
use phosphor_machines::rom_loader::RomSet;

/// File magic: "PHosphor Movie Input".
pub const MOVIE_MAGIC: [u8; 4] = *b"PHMI";

/// Format version. Bumped only for envelope changes; a new record kind that old
/// readers must reject also bumps it.
pub const MOVIE_VERSION: u16 = 1;

/// Ceiling on a decompressed record block, so a corrupt or hostile file cannot
/// make the decoder allocate without bound. 64 MiB is roughly six million
/// records — hours of continuous trackball motion, and far past anything worth
/// committing.
const MAX_RECORD_BLOCK: usize = 64 << 20;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a movie could not be decoded, or could not be replayed against a machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MovieError {
    /// The file does not start with [`MOVIE_MAGIC`].
    BadMagic,
    /// The file's version is not one this build understands.
    UnsupportedVersion(u16),
    /// The file ended in the middle of a field.
    Truncated,
    /// The trailing digest does not match the bytes it covers.
    ChecksumMismatch,
    /// The record block did not inflate, or inflated to the wrong size.
    CorruptRecordBlock,
    /// A record carried a kind byte this build does not know.
    UnknownRecordKind(u8),
    /// A string field was not valid UTF-8.
    BadUtf8,
    /// A record indexed past the header's control table.
    ControlOutOfRange { index: u16, len: usize },
    /// The movie was recorded against a different machine.
    MachineMismatch { expected: String, actual: String },
    /// The movie was recorded against a different ROM dump.
    RomMismatch,
    /// The movie names a control the machine does not expose.
    UnknownControl(String),
    /// A record's frame is past the header's declared span.
    FrameOutOfRange { frame: u32, frames: u32 },
    /// Records are not in ascending frame order, which replay's forward cursor
    /// requires.
    FramesNotMonotonic { frame: u32, previous: u32 },
}

impl fmt::Display for MovieError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadMagic => write!(f, "not a movie file (bad magic)"),
            Self::UnsupportedVersion(v) => write!(
                f,
                "movie format version {v}, this build understands {MOVIE_VERSION}"
            ),
            Self::Truncated => write!(f, "movie file ends mid-field"),
            Self::ChecksumMismatch => {
                write!(f, "movie digest does not match its contents (corrupt file)")
            }
            Self::CorruptRecordBlock => write!(f, "movie record block failed to decompress"),
            Self::UnknownRecordKind(k) => write!(f, "unknown movie record kind {k}"),
            Self::BadUtf8 => write!(f, "movie contains a non-UTF-8 string"),
            Self::ControlOutOfRange { index, len } => write!(
                f,
                "movie record targets control index {index}, but its table holds {len}"
            ),
            Self::MachineMismatch { expected, actual } => write!(
                f,
                "movie was recorded for machine '{expected}', not '{actual}'"
            ),
            Self::RomMismatch => write!(
                f,
                "movie was recorded against a different ROM set than the one loaded"
            ),
            Self::UnknownControl(name) => write!(f, "machine has no '{name}' input control"),
            Self::FrameOutOfRange { frame, frames } => write!(
                f,
                "movie record at frame {frame} is past its declared span of {frames} frames"
            ),
            Self::FramesNotMonotonic { frame, previous } => write!(
                f,
                "movie records are out of order: frame {frame} follows frame {previous}"
            ),
        }
    }
}

impl std::error::Error for MovieError {}

// ---------------------------------------------------------------------------
// ROM identity
// ---------------------------------------------------------------------------

/// SHA-256 over the ROM files a machine will be built from, in the registry's
/// `rom_names` order.
///
/// Replaying a movie against a different dump of the same game is the failure
/// this exists to catch: the machine boots, the frames differ, and without a
/// digest the only symptom is a golden hash that moved for no visible reason.
///
/// Each name and each body is length-prefixed before being absorbed, so two
/// different splits of the same bytes cannot collide.
///
/// A blank set ([`RomSet::blank`], used by the ROM-less registry-driven tests)
/// has no bytes to digest, so each name absorbs a sentinel instead. That still
/// yields a stable per-machine value, which is all those tests need.
pub fn rom_digest(set: &RomSet, rom_names: &[&str]) -> [u8; 32] {
    let mut h = Sha256::new();
    for name in rom_names {
        h.update((name.len() as u64).to_le_bytes());
        h.update(name.as_bytes());
        match set.get(name) {
            Some(bytes) => {
                h.update([1u8]);
                h.update((bytes.len() as u64).to_le_bytes());
                h.update(bytes);
            }
            None => h.update([0u8]),
        }
    }
    h.finalize().into()
}

/// Render a digest as lowercase hex, for `movie info` and error text.
pub fn hex(digest: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

// ---------------------------------------------------------------------------
// Header and records
// ---------------------------------------------------------------------------

/// Everything replay needs to reconstruct the machine the movie was recorded
/// against, before a single record is delivered.
#[derive(Debug, Clone, PartialEq)]
pub struct MovieHeader {
    /// Registry name, e.g. `"marble"`.
    pub machine: String,
    /// [`rom_digest`] of the ROM set at record time.
    pub rom_digest: [u8; 32],
    /// Stable control names. Records address controls by index into this table
    /// rather than by [`InputId`], so a movie survives a machine renumbering its
    /// ids, and by index rather than by name so a long analog trace does not
    /// carry thousands of copies of the same string.
    pub controls: Vec<String>,
    /// Power-on DIP byte per bank, in bank order. Coinage, lives and difficulty
    /// change gameplay, so a movie that did not pin these would not reproduce.
    pub dip: Vec<u8>,
    /// Inline NVRAM image, for the boards that boot differently without one.
    /// Inline rather than a path so a movie is self-contained.
    pub nvram: Option<Vec<u8>>,
    /// Host audio rate at record time. Replay *sets* this before building the
    /// machine rather than rejecting a mismatch: a movie that refused to play on
    /// a 48 kHz host would not be shareable, and headless replay has no real
    /// audio device to disagree with.
    pub host_sample_rate: u32,
    /// Frames the movie spans. Replaying past this is allowed (the machine
    /// simply receives no further input); a *record* past it is a decode error.
    pub frames: u32,
}

/// One recorded event, tagged with the frame it was delivered before.
#[derive(Debug, Clone, PartialEq)]
pub enum MovieRecord {
    Button {
        frame: u32,
        ctl: u16,
        pressed: bool,
    },
    Absolute {
        frame: u32,
        ctl: u16,
        bits: u32,
    },
    Relative {
        frame: u32,
        ctl: u16,
        bits: u32,
    },
    /// `InputConfigurable::release_all_inputs`, kept whole rather than expanded
    /// into a release per control: machines holding conditioned analog state
    /// override it to clear trackball accumulators that a per-control loop does
    /// not touch, so expanding it would silently drop that.
    ReleaseAll {
        frame: u32,
    },
    Dip {
        frame: u32,
        bank: u8,
        value: u8,
    },
    Marker {
        frame: u32,
        label: String,
    },
}

const KIND_BUTTON: u8 = 0;
const KIND_ABSOLUTE: u8 = 1;
const KIND_RELATIVE: u8 = 2;
const KIND_RELEASE_ALL: u8 = 3;
const KIND_DIP: u8 = 4;
const KIND_MARKER: u8 = 5;

impl MovieRecord {
    /// The frame this record is delivered before.
    pub fn frame(&self) -> u32 {
        match *self {
            Self::Button { frame, .. }
            | Self::Absolute { frame, .. }
            | Self::Relative { frame, .. }
            | Self::ReleaseAll { frame }
            | Self::Dip { frame, .. }
            | Self::Marker { frame, .. } => frame,
        }
    }

    /// Build the [`InputEvent`] this record replays, resolving its control index
    /// through `ids`. Returns `None` for records that are not input events
    /// (`Dip`, `Marker`) or that do not address a control (`ReleaseAll`).
    pub fn to_input_event(&self, ids: &[InputId]) -> Option<InputEvent> {
        let ctl = match *self {
            Self::Button { ctl, .. } | Self::Absolute { ctl, .. } | Self::Relative { ctl, .. } => {
                ctl
            }
            _ => return None,
        };
        let id = *ids.get(usize::from(ctl))?;
        Some(match *self {
            Self::Button { pressed, .. } => InputEvent::Button { id, pressed },
            Self::Absolute { bits, .. } => InputEvent::Absolute {
                id,
                value: f32::from_bits(bits),
            },
            Self::Relative { bits, .. } => InputEvent::Relative {
                id,
                delta: f32::from_bits(bits),
            },
            _ => unreachable!("only control-addressing kinds reach here"),
        })
    }
}

/// A decoded movie: its header and every record, in delivery order.
#[derive(Debug, Clone, PartialEq)]
pub struct Movie {
    pub header: MovieHeader,
    pub records: Vec<MovieRecord>,
}

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------
//
// Little-endian throughout, matching the rest of the workspace's binary
// formats. Deliberately hand-rolled rather than built on `StateWriter`: sharing
// an encoder with the save-state format would re-introduce exactly the coupling
// this format exists without.

fn put_u8(out: &mut Vec<u8>, v: u8) {
    out.push(v);
}
fn put_u16(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn put_bytes(out: &mut Vec<u8>, v: &[u8]) {
    put_u32(out, v.len() as u32);
    out.extend_from_slice(v);
}
fn put_str(out: &mut Vec<u8>, v: &str) {
    put_bytes(out, v.as_bytes());
}

/// Cursor over a movie's bytes. Every read is bounds-checked, so a truncated
/// file is a clean `Truncated` rather than a panic.
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], MovieError> {
        let end = self.pos.checked_add(n).ok_or(MovieError::Truncated)?;
        let slice = self.bytes.get(self.pos..end).ok_or(MovieError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, MovieError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, MovieError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, MovieError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn digest(&mut self) -> Result<[u8; 32], MovieError> {
        Ok(self.take(32)?.try_into().unwrap())
    }
    fn bytes(&mut self) -> Result<&'a [u8], MovieError> {
        let len = self.u32()? as usize;
        self.take(len)
    }
    fn string(&mut self) -> Result<String, MovieError> {
        let b = self.bytes()?;
        std::str::from_utf8(b)
            .map(str::to_owned)
            .map_err(|_| MovieError::BadUtf8)
    }
    fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }
}

fn encode_header(h: &MovieHeader) -> Vec<u8> {
    let mut out = Vec::new();
    put_str(&mut out, &h.machine);
    out.extend_from_slice(&h.rom_digest);
    put_u32(&mut out, h.controls.len() as u32);
    for c in &h.controls {
        put_str(&mut out, c);
    }
    put_bytes(&mut out, &h.dip);
    match &h.nvram {
        Some(nv) => {
            put_u8(&mut out, 1);
            put_bytes(&mut out, nv);
        }
        None => put_u8(&mut out, 0),
    }
    put_u32(&mut out, h.host_sample_rate);
    put_u32(&mut out, h.frames);
    out
}

fn decode_header(c: &mut Cursor<'_>) -> Result<MovieHeader, MovieError> {
    let machine = c.string()?;
    let rom_digest = c.digest()?;
    let control_count = c.u32()? as usize;
    // A machine's control table is tens of entries. Reject an absurd count
    // before allocating for it; the cursor would catch it too, but not before
    // reserving.
    if control_count > u16::MAX as usize {
        return Err(MovieError::Truncated);
    }
    let mut controls = Vec::with_capacity(control_count);
    for _ in 0..control_count {
        controls.push(c.string()?);
    }
    let dip = c.bytes()?.to_vec();
    let nvram = match c.u8()? {
        0 => None,
        _ => Some(c.bytes()?.to_vec()),
    };
    let host_sample_rate = c.u32()?;
    let frames = c.u32()?;
    Ok(MovieHeader {
        machine,
        rom_digest,
        controls,
        dip,
        nvram,
        host_sample_rate,
        frames,
    })
}

fn encode_record(out: &mut Vec<u8>, r: &MovieRecord) {
    match r {
        MovieRecord::Button {
            frame,
            ctl,
            pressed,
        } => {
            put_u8(out, KIND_BUTTON);
            put_u32(out, *frame);
            put_u16(out, *ctl);
            put_u8(out, u8::from(*pressed));
        }
        MovieRecord::Absolute { frame, ctl, bits } => {
            put_u8(out, KIND_ABSOLUTE);
            put_u32(out, *frame);
            put_u16(out, *ctl);
            put_u32(out, *bits);
        }
        MovieRecord::Relative { frame, ctl, bits } => {
            put_u8(out, KIND_RELATIVE);
            put_u32(out, *frame);
            put_u16(out, *ctl);
            put_u32(out, *bits);
        }
        MovieRecord::ReleaseAll { frame } => {
            put_u8(out, KIND_RELEASE_ALL);
            put_u32(out, *frame);
        }
        MovieRecord::Dip { frame, bank, value } => {
            put_u8(out, KIND_DIP);
            put_u32(out, *frame);
            put_u8(out, *bank);
            put_u8(out, *value);
        }
        MovieRecord::Marker { frame, label } => {
            put_u8(out, KIND_MARKER);
            put_u32(out, *frame);
            put_str(out, label);
        }
    }
}

fn decode_record(c: &mut Cursor<'_>) -> Result<MovieRecord, MovieError> {
    let kind = c.u8()?;
    let frame = c.u32()?;
    Ok(match kind {
        KIND_BUTTON => MovieRecord::Button {
            frame,
            ctl: c.u16()?,
            pressed: c.u8()? != 0,
        },
        KIND_ABSOLUTE => MovieRecord::Absolute {
            frame,
            ctl: c.u16()?,
            bits: c.u32()?,
        },
        KIND_RELATIVE => MovieRecord::Relative {
            frame,
            ctl: c.u16()?,
            bits: c.u32()?,
        },
        KIND_RELEASE_ALL => MovieRecord::ReleaseAll { frame },
        KIND_DIP => MovieRecord::Dip {
            frame,
            bank: c.u8()?,
            value: c.u8()?,
        },
        KIND_MARKER => MovieRecord::Marker {
            frame,
            label: c.string()?,
        },
        other => return Err(MovieError::UnknownRecordKind(other)),
    })
}

fn deflate(raw: &[u8]) -> Vec<u8> {
    let mut e = DeflateEncoder::new(Vec::new(), Compression::default());
    e.write_all(raw).expect("writing to a Vec cannot fail");
    e.finish().expect("finishing into a Vec cannot fail")
}

fn inflate(packed: &[u8], raw_len: usize) -> Result<Vec<u8>, MovieError> {
    if raw_len > MAX_RECORD_BLOCK {
        return Err(MovieError::CorruptRecordBlock);
    }
    let mut out = Vec::with_capacity(raw_len);
    DeflateDecoder::new(packed)
        // Bound the reader independently of the declared length, so a file that
        // lies about `raw_len` still cannot run the allocator away.
        .take(raw_len as u64 + 1)
        .read_to_end(&mut out)
        .map_err(|_| MovieError::CorruptRecordBlock)?;
    if out.len() != raw_len {
        return Err(MovieError::CorruptRecordBlock);
    }
    Ok(out)
}

impl Movie {
    /// Serialise to the on-disk form:
    ///
    /// ```text
    /// magic:4 | version:u16
    /// header_len:u32   | header
    /// records_raw:u32  | records_deflated:u32 | deflate(record_count:u32 | records)
    /// trailer: sha256(all preceding bytes):32
    /// ```
    ///
    /// Both sections are length-prefixed so a reader can skip either wholesale —
    /// which is what lets `movie info` describe a file whose record kinds it does
    /// not all understand.
    ///
    /// The record block is deflated because analog traces dominate a movie's
    /// size: a trackball game emits on the order of ten records per frame, so a
    /// minute of Marble Madness is ~36k records where a minute of a button-only
    /// game is a few hundred. The boot phase costs nothing either way — it emits
    /// no records at all, so seeking past a 3000-frame self-test is free.
    pub fn encode(&self) -> Vec<u8> {
        let header = encode_header(&self.header);

        let mut raw = Vec::new();
        put_u32(&mut raw, self.records.len() as u32);
        for r in &self.records {
            encode_record(&mut raw, r);
        }
        let packed = deflate(&raw);

        let mut out = Vec::with_capacity(header.len() + packed.len() + 64);
        out.extend_from_slice(&MOVIE_MAGIC);
        put_u16(&mut out, MOVIE_VERSION);
        put_bytes(&mut out, &header);
        put_u32(&mut out, raw.len() as u32);
        put_bytes(&mut out, &packed);

        let mut h = Sha256::new();
        h.update(&out);
        let digest: [u8; 32] = h.finalize().into();
        out.extend_from_slice(&digest);
        out
    }

    /// Parse the on-disk form, verifying magic, version, digest, and that no
    /// record falls outside the header's declared span or control table.
    pub fn decode(bytes: &[u8]) -> Result<Self, MovieError> {
        // The digest covers everything before itself, so split it off first.
        let body_len = bytes.len().checked_sub(32).ok_or(MovieError::Truncated)?;
        let (body, trailer) = bytes.split_at(body_len);
        let mut h = Sha256::new();
        h.update(body);
        let actual: [u8; 32] = h.finalize().into();
        if actual.as_slice() != trailer {
            return Err(MovieError::ChecksumMismatch);
        }

        let mut c = Cursor::new(body);
        if c.take(4)? != MOVIE_MAGIC {
            return Err(MovieError::BadMagic);
        }
        let version = c.u16()?;
        if version != MOVIE_VERSION {
            return Err(MovieError::UnsupportedVersion(version));
        }

        let header_bytes = c.bytes()?;
        let header = decode_header(&mut Cursor::new(header_bytes))?;

        let raw_len = c.u32()? as usize;
        let packed = c.bytes()?;
        let raw = inflate(packed, raw_len)?;

        let mut rc = Cursor::new(&raw);
        let count = rc.u32()? as usize;
        // Every record is at least 5 bytes (kind + frame), so a count exceeding
        // the remaining bytes could only come from a corrupt file.
        if count > rc.remaining() {
            return Err(MovieError::Truncated);
        }
        let mut records = Vec::with_capacity(count);
        for _ in 0..count {
            records.push(decode_record(&mut rc)?);
        }

        let movie = Self { header, records };
        movie.validate()?;
        Ok(movie)
    }

    /// Check the invariants a decoded movie must hold before it is replayed:
    /// every control index addresses the header's table, and no record sits past
    /// the declared frame span.
    ///
    /// Checked at decode rather than at delivery so a malformed movie fails once,
    /// up front, naming the problem — instead of replaying most of a session and
    /// then diverging.
    fn validate(&self) -> Result<(), MovieError> {
        let mut prev = 0u32;
        for r in &self.records {
            // Replay walks the records with a single forward cursor, so a movie
            // whose frames go backwards would silently drop everything after the
            // step. Reject it here rather than deliver a partial session.
            if r.frame() < prev {
                return Err(MovieError::FramesNotMonotonic {
                    frame: r.frame(),
                    previous: prev,
                });
            }
            prev = r.frame();

            if self.header.frames != 0 && r.frame() >= self.header.frames {
                return Err(MovieError::FrameOutOfRange {
                    frame: r.frame(),
                    frames: self.header.frames,
                });
            }
            let ctl = match *r {
                MovieRecord::Button { ctl, .. }
                | MovieRecord::Absolute { ctl, .. }
                | MovieRecord::Relative { ctl, .. } => ctl,
                _ => continue,
            };
            if usize::from(ctl) >= self.header.controls.len() {
                return Err(MovieError::ControlOutOfRange {
                    index: ctl,
                    len: self.header.controls.len(),
                });
            }
        }
        Ok(())
    }

    /// Records grouped by frame, as `(frame, count)` in ascending frame order.
    /// The human view behind `disasm movie info`.
    pub fn frame_histogram(&self) -> Vec<(u32, usize)> {
        let mut out: Vec<(u32, usize)> = Vec::new();
        for r in &self.records {
            match out.last_mut() {
                Some((f, n)) if *f == r.frame() => *n += 1,
                _ => out.push((r.frame(), 1)),
            }
        }
        out
    }

    /// Author-placed bookmarks, in frame order.
    pub fn markers(&self) -> impl Iterator<Item = (u32, &str)> {
        self.records.iter().filter_map(|r| match r {
            MovieRecord::Marker { frame, label } => Some((*frame, label.as_str())),
            _ => None,
        })
    }
}

// ---------------------------------------------------------------------------
// Recording
// ---------------------------------------------------------------------------

/// Accumulates a movie from a live session.
///
/// The recorder is a passive sink: something else decides when an event happens
/// and when a frame ends. Headlessly that is [`Harness`](crate::Harness); in the
/// frontend it is a wrapper over `InputConfigurable` that tees every call here
/// before forwarding it to the machine, so no input path can be recorded
/// incompletely without also bypassing the trait.
///
/// A recording always starts from power-on. There is no seek and no initial
/// snapshot, because carrying one would tie the movie to `SAVE_VERSION` and cost
/// it the durability that makes committing one worthwhile.
pub struct MovieRecorder {
    machine: String,
    rom_digest: [u8; 32],
    controls: Vec<String>,
    /// `InputId` → index into `controls`. Built once from the machine's control
    /// table; input ids are machine-local and small, but not necessarily dense
    /// or zero-based, so this is a map rather than an index.
    index_of: Vec<(InputId, u16)>,
    dip: Vec<u8>,
    nvram: Option<Vec<u8>>,
    host_sample_rate: u32,
    records: Vec<MovieRecord>,
    frame: u32,
    /// Events whose `InputId` was not in the machine's control table. Should
    /// always be zero — a non-zero count means something is synthesising ids
    /// outside the table, which would replay as silence.
    unmapped: usize,
}

impl MovieRecorder {
    /// Start a recording against a machine that has just been reset.
    ///
    /// `controls` is the machine's `input_controls()` table, `dip` its power-on
    /// bank bytes in bank order, and `nvram` the image loaded after reset (if
    /// any). All three are captured now because replay must reconstruct the same
    /// starting conditions before delivering a single record.
    pub fn new(
        machine: impl Into<String>,
        rom_digest: [u8; 32],
        controls: &[InputControl],
        dip: Vec<u8>,
        nvram: Option<Vec<u8>>,
    ) -> Self {
        Self {
            machine: machine.into(),
            rom_digest,
            controls: controls.iter().map(|c| c.stable_name.to_owned()).collect(),
            index_of: controls
                .iter()
                .enumerate()
                .map(|(i, c)| (c.id, i as u16))
                .collect(),
            dip,
            nvram,
            host_sample_rate: phosphor_core::audio::host_sample_rate(),
            records: Vec::new(),
            frame: 0,
            unmapped: 0,
        }
    }

    fn index(&self, id: InputId) -> Option<u16> {
        self.index_of
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, n)| *n)
    }

    /// Record one delivered [`InputEvent`].
    ///
    /// Call this for *every* event, in delivery order — never one accumulated
    /// value per frame. See the module docs for why summing analog deltas does
    /// not replay faithfully.
    pub fn push_event(&mut self, event: InputEvent) {
        // A zero relative delta is a provable no-op — both `RelativeCounter` and
        // `AnalogAxis` apply it as `pending += 0` — so recording it would only
        // cost space and overstate how much input a session actually contained.
        // They are not rare: the frontend emits an X and a Y event for every
        // mouse motion, so any straight-line movement records a zero on the other
        // axis. On a real four-minute trackball capture that was 4,173 of 35,178
        // analog records, 12% of the file.
        //
        // Deliberately only `Relative`. An `Absolute` 0.0 is a *position* — a
        // centred stick — and dropping it would lose a real state change.
        if let InputEvent::Relative { delta, .. } = event
            && delta == 0.0
        {
            return;
        }
        let id = match event {
            InputEvent::Button { id, .. }
            | InputEvent::Absolute { id, .. }
            | InputEvent::Relative { id, .. } => id,
        };
        let Some(ctl) = self.index(id) else {
            self.unmapped += 1;
            return;
        };
        let frame = self.frame;
        self.records.push(match event {
            InputEvent::Button { pressed, .. } => MovieRecord::Button {
                frame,
                ctl,
                pressed,
            },
            InputEvent::Absolute { value, .. } => MovieRecord::Absolute {
                frame,
                ctl,
                bits: value.to_bits(),
            },
            InputEvent::Relative { delta, .. } => MovieRecord::Relative {
                frame,
                ctl,
                bits: delta.to_bits(),
            },
        });
    }

    /// Record a `release_all_inputs()` call.
    pub fn push_release_all(&mut self) {
        self.records
            .push(MovieRecord::ReleaseAll { frame: self.frame });
    }

    /// Record a mid-session DIP change.
    pub fn push_dip(&mut self, bank: u8, value: u8) {
        self.records.push(MovieRecord::Dip {
            frame: self.frame,
            bank,
            value,
        });
    }

    /// Record an author-placed bookmark at the current frame.
    pub fn push_marker(&mut self, label: impl Into<String>) {
        self.records.push(MovieRecord::Marker {
            frame: self.frame,
            label: label.into(),
        });
    }

    /// Note that a frame has completed. Subsequent events belong to the next one.
    pub fn advance_frame(&mut self) {
        self.frame += 1;
    }

    /// Frames recorded so far.
    pub fn frame(&self) -> u32 {
        self.frame
    }

    /// Records captured so far.
    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Events dropped because their `InputId` was not in the control table.
    /// Always zero in a correct session; exposed so a test can assert it.
    pub fn unmapped(&self) -> usize {
        self.unmapped
    }

    /// Finish the recording into a [`Movie`] ready to encode.
    pub fn finish(self) -> Movie {
        Movie {
            header: MovieHeader {
                machine: self.machine,
                rom_digest: self.rom_digest,
                controls: self.controls,
                dip: self.dip,
                nvram: self.nvram,
                host_sample_rate: self.host_sample_rate,
                frames: self.frame,
            },
            records: self.records,
        }
    }
}

// ---------------------------------------------------------------------------
// Playback
// ---------------------------------------------------------------------------

/// A decoded movie bound to a specific machine's control table, with a forward
/// cursor over its records.
///
/// Binding happens once, up front: every control name in the header is resolved
/// to the machine's live [`InputId`], so a missing control is an error before
/// the first frame runs rather than silent no-input halfway through.
pub struct MoviePlayer {
    movie: Movie,
    /// Control index → this machine's `InputId`, parallel to `header.controls`.
    ids: Vec<InputId>,
    cursor: usize,
}

impl MoviePlayer {
    /// Bind `movie` to a machine's control table.
    pub fn bind(movie: Movie, controls: &[InputControl]) -> Result<Self, MovieError> {
        let ids = movie
            .header
            .controls
            .iter()
            .map(|name| {
                controls
                    .iter()
                    .find(|c| c.stable_name == *name)
                    .map(|c| c.id)
                    .ok_or_else(|| MovieError::UnknownControl(name.clone()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            movie,
            ids,
            cursor: 0,
        })
    }

    /// The records scheduled for `frame`, advancing the cursor past them.
    ///
    /// Frames must be requested in ascending order — which is how both
    /// [`Harness::run_frame`](crate::Harness::run_frame) and the cycle-granular
    /// debug loop drive it. Records for a skipped frame are discarded rather
    /// than delivered late, so a caller that jumps forward does not get a burst
    /// of stale input.
    pub fn take_frame(&mut self, frame: u32) -> &[MovieRecord] {
        while self
            .movie
            .records
            .get(self.cursor)
            .is_some_and(|r| r.frame() < frame)
        {
            self.cursor += 1;
        }
        let start = self.cursor;
        while self
            .movie
            .records
            .get(self.cursor)
            .is_some_and(|r| r.frame() == frame)
        {
            self.cursor += 1;
        }
        &self.movie.records[start..self.cursor]
    }

    /// Control index → `InputId` table, for turning a record into an event.
    pub fn ids(&self) -> &[InputId] {
        &self.ids
    }

    pub fn header(&self) -> &MovieHeader {
        &self.movie.header
    }

    pub fn movie(&self) -> &Movie {
        &self.movie
    }

    /// Whether every record has been delivered.
    pub fn finished(&self) -> bool {
        self.cursor >= self.movie.records.len()
    }

    /// Rewind the cursor to the start, for replaying the same movie twice
    /// against a freshly reset machine.
    pub fn rewind(&mut self) {
        self.cursor = 0;
    }

    /// Deliver this movie's records for `frame` to `machine`, in the order they
    /// were recorded.
    ///
    /// A free-standing method rather than [`Harness`](crate::Harness) internals
    /// because two callers drive frames themselves: the harness, and the
    /// frontend, whose machine reference is borrowed out of a session and so
    /// cannot reach back into the object holding the player. Sharing the body
    /// keeps the delivery order — which is load-bearing — defined once.
    ///
    /// Order matters twice over. Across records, because a press and its release
    /// in the same frame must arrive that way round. Within analog records,
    /// because each truncates independently inside the machine, which is why a
    /// movie stores every delta rather than a per-frame sum.
    pub fn deliver(&mut self, machine: &mut dyn FrontendMachine, frame: u32) {
        let records: Vec<MovieRecord> = self.take_frame(frame).to_vec();
        if records.is_empty() {
            return;
        }
        let ids = self.ids.clone();
        for record in &records {
            match record {
                MovieRecord::ReleaseAll { .. } => machine.release_all_inputs(),
                MovieRecord::Dip { bank, value, .. } => {
                    machine.set_dip_bank_value(*bank as usize, *value)
                }
                // Markers are author bookmarks; they change no machine state.
                MovieRecord::Marker { .. } => {}
                _ => {
                    if let Some(event) = record.to_input_event(&ids) {
                        machine.handle_input(event);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> MovieHeader {
        MovieHeader {
            machine: "marble".into(),
            rom_digest: [0xAB; 32],
            controls: vec!["track_x".into(), "coin".into()],
            dip: vec![0x40, 0x00],
            nvram: Some(vec![1, 2, 3]),
            host_sample_rate: 48_000,
            frames: 100,
        }
    }

    fn movie() -> Movie {
        Movie {
            header: header(),
            records: vec![
                MovieRecord::Relative {
                    frame: 0,
                    ctl: 0,
                    bits: 0.6f32.to_bits(),
                },
                MovieRecord::Button {
                    frame: 3,
                    ctl: 1,
                    pressed: true,
                },
                MovieRecord::ReleaseAll { frame: 5 },
                MovieRecord::Dip {
                    frame: 7,
                    bank: 0,
                    value: 0x41,
                },
                MovieRecord::Marker {
                    frame: 9,
                    label: "level 1".into(),
                },
            ],
        }
    }

    /// Re-stamp the trailing digest so a deliberately corrupted body reaches the
    /// check under test instead of tripping the digest first.
    fn restamp(bytes: &mut [u8]) {
        let n = bytes.len() - 32;
        let mut h = Sha256::new();
        h.update(&bytes[..n]);
        let d: [u8; 32] = h.finalize().into();
        bytes[n..].copy_from_slice(&d);
    }

    #[test]
    fn round_trips_every_record_kind() {
        let m = movie();
        assert_eq!(Movie::decode(&m.encode()).expect("decode"), m);
    }

    #[test]
    fn round_trips_a_header_with_no_nvram() {
        let m = Movie {
            header: MovieHeader {
                nvram: None,
                ..header()
            },
            records: Vec::new(),
        };
        assert_eq!(Movie::decode(&m.encode()).expect("decode"), m);
    }

    #[test]
    fn round_trips_an_empty_movie() {
        let m = Movie {
            header: MovieHeader {
                frames: 0,
                ..header()
            },
            records: Vec::new(),
        };
        assert_eq!(Movie::decode(&m.encode()).expect("decode"), m);
    }

    /// The reason floats are stored as bits rather than re-parsed: a replayed
    /// delta must truncate to the same integer the recorded one did, and
    /// `RelativeCounter` truncates per event.
    #[test]
    fn float_payloads_survive_bit_exact() {
        for v in [0.6f32, -0.6, 1.2, f32::MIN_POSITIVE, -0.0, 1e-30, 1e30] {
            let m = Movie {
                header: MovieHeader {
                    frames: 2,
                    ..header()
                },
                records: vec![MovieRecord::Relative {
                    frame: 0,
                    ctl: 0,
                    bits: v.to_bits(),
                }],
            };
            let decoded = Movie::decode(&m.encode()).expect("decode");
            let MovieRecord::Relative { bits, .. } = decoded.records[0] else {
                panic!("wrong kind");
            };
            assert_eq!(bits, v.to_bits(), "{v} did not survive");
        }
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = movie().encode();
        bytes[0] = b'X';
        restamp(&mut bytes);
        assert_eq!(Movie::decode(&bytes), Err(MovieError::BadMagic));
    }

    #[test]
    fn rejects_an_unsupported_version() {
        let mut bytes = movie().encode();
        bytes[4..6].copy_from_slice(&99u16.to_le_bytes());
        restamp(&mut bytes);
        assert_eq!(
            Movie::decode(&bytes),
            Err(MovieError::UnsupportedVersion(99))
        );
    }

    #[test]
    fn rejects_a_corrupted_body() {
        let mut bytes = movie().encode();
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xFF;
        assert_eq!(Movie::decode(&bytes), Err(MovieError::ChecksumMismatch));
    }

    #[test]
    fn rejects_truncation_at_every_prefix() {
        let bytes = movie().encode();
        for cut in 0..bytes.len() {
            assert!(
                Movie::decode(&bytes[..cut]).is_err(),
                "a {cut}-byte prefix decoded"
            );
        }
    }

    #[test]
    fn rejects_a_control_index_past_the_table() {
        let m = Movie {
            header: header(),
            records: vec![MovieRecord::Button {
                frame: 0,
                ctl: 9,
                pressed: true,
            }],
        };
        assert_eq!(
            Movie::decode(&m.encode()),
            Err(MovieError::ControlOutOfRange { index: 9, len: 2 })
        );
    }

    #[test]
    fn rejects_a_record_past_the_declared_span() {
        let m = Movie {
            header: MovieHeader {
                frames: 4,
                ..header()
            },
            records: vec![MovieRecord::ReleaseAll { frame: 4 }],
        };
        assert_eq!(
            Movie::decode(&m.encode()),
            Err(MovieError::FrameOutOfRange {
                frame: 4,
                frames: 4
            })
        );
    }

    #[test]
    fn to_input_event_resolves_through_the_id_table() {
        let ids = [InputId(7), InputId(9)];
        assert_eq!(
            MovieRecord::Relative {
                frame: 0,
                ctl: 0,
                bits: 1.5f32.to_bits()
            }
            .to_input_event(&ids),
            Some(InputEvent::Relative {
                id: InputId(7),
                delta: 1.5
            })
        );
        assert_eq!(
            MovieRecord::Button {
                frame: 0,
                ctl: 1,
                pressed: false
            }
            .to_input_event(&ids),
            Some(InputEvent::Button {
                id: InputId(9),
                pressed: false
            })
        );
        // Non-input records carry no event.
        assert_eq!(
            MovieRecord::ReleaseAll { frame: 0 }.to_input_event(&ids),
            None
        );
        assert_eq!(
            MovieRecord::Dip {
                frame: 0,
                bank: 0,
                value: 1
            }
            .to_input_event(&ids),
            None
        );
    }

    #[test]
    fn frame_histogram_groups_consecutive_records() {
        let m = Movie {
            header: MovieHeader {
                frames: 10,
                ..header()
            },
            records: vec![
                MovieRecord::ReleaseAll { frame: 0 },
                MovieRecord::ReleaseAll { frame: 0 },
                MovieRecord::ReleaseAll { frame: 4 },
            ],
        };
        assert_eq!(m.frame_histogram(), vec![(0, 2), (4, 1)]);
    }

    #[test]
    fn markers_are_listed_in_order() {
        let m = movie();
        assert_eq!(m.markers().collect::<Vec<_>>(), vec![(9, "level 1")]);
    }

    // -----------------------------------------------------------------------
    // Recorder / player
    // -----------------------------------------------------------------------

    use phosphor_core::core::machine::InputKind;

    const TRACK_X: InputId = InputId(11);
    const COIN: InputId = InputId(4);

    /// A two-control table with deliberately sparse, non-zero-based ids, so a
    /// recorder that assumed `InputId` *was* the index would fail here.
    const CONTROLS: &[InputControl] = &[
        InputControl {
            id: TRACK_X,
            stable_name: "track_x",
            label: "Trackball X",
            kind: InputKind::Button,
            player: Some(1),
            default_bindings: &[],
        },
        InputControl {
            id: COIN,
            stable_name: "coin",
            label: "Coin",
            kind: InputKind::Coin,
            player: None,
            default_bindings: &[],
        },
    ];

    fn recorder() -> MovieRecorder {
        MovieRecorder::new("marble", [7; 32], CONTROLS, vec![0x40], None)
    }

    #[test]
    fn recorder_maps_sparse_input_ids_to_table_indices() {
        let mut r = recorder();
        r.push_event(InputEvent::Button {
            id: COIN,
            pressed: true,
        });
        r.push_event(InputEvent::Relative {
            id: TRACK_X,
            delta: 2.5,
        });
        let m = r.finish();
        assert_eq!(
            m.records,
            vec![
                MovieRecord::Button {
                    frame: 0,
                    ctl: 1,
                    pressed: true
                },
                MovieRecord::Relative {
                    frame: 0,
                    ctl: 0,
                    bits: 2.5f32.to_bits()
                },
            ]
        );
    }

    #[test]
    fn recorder_tags_records_with_the_frame_they_were_delivered_before() {
        let mut r = recorder();
        r.push_event(InputEvent::Button {
            id: COIN,
            pressed: true,
        });
        r.advance_frame();
        r.advance_frame();
        r.push_release_all();
        r.push_marker("here");
        let m = r.finish();
        assert_eq!(m.records[0].frame(), 0);
        assert_eq!(m.records[1].frame(), 2);
        assert_eq!(m.records[2].frame(), 2);
        // `frames` is the span, so it counts the frames advanced through.
        assert_eq!(m.header.frames, 2);
    }

    #[test]
    fn recorder_skips_zero_relative_deltas_but_keeps_zero_absolutes() {
        let mut r = recorder();
        r.push_event(InputEvent::Relative {
            id: TRACK_X,
            delta: 0.0,
        });
        r.push_event(InputEvent::Relative {
            id: TRACK_X,
            delta: -0.0,
        });
        assert!(
            r.is_empty(),
            "a zero delta is a no-op and must not be stored"
        );

        // A zero *position* is a centred stick, which is a real state change.
        r.push_event(InputEvent::Absolute {
            id: TRACK_X,
            value: 0.0,
        });
        assert_eq!(r.len(), 1);

        // Non-zero deltas are unaffected, including sub-unit ones.
        r.push_event(InputEvent::Relative {
            id: TRACK_X,
            delta: 0.6,
        });
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn recorder_counts_events_outside_the_control_table_instead_of_recording_them() {
        let mut r = recorder();
        r.push_event(InputEvent::Button {
            id: InputId(999),
            pressed: true,
        });
        assert_eq!(r.unmapped(), 1);
        assert!(r.is_empty());
    }

    /// The property the whole format exists for: what was recorded is what is
    /// delivered, in order, against the same frames.
    #[test]
    fn a_recorded_session_replays_as_the_same_events() {
        let mut r = recorder();
        r.push_event(InputEvent::Relative {
            id: TRACK_X,
            delta: 0.6,
        });
        r.push_event(InputEvent::Relative {
            id: TRACK_X,
            delta: 0.6,
        });
        r.advance_frame();
        r.push_event(InputEvent::Button {
            id: COIN,
            pressed: true,
        });
        r.advance_frame();
        r.push_event(InputEvent::Button {
            id: COIN,
            pressed: false,
        });
        r.advance_frame();

        let encoded = r.finish().encode();
        let decoded = Movie::decode(&encoded).expect("decode");
        let mut p = MoviePlayer::bind(decoded, CONTROLS).expect("bind");

        // `take_frame` borrows the player, so the id table is lifted out first —
        // the same shape `Harness::apply_movie_input` uses.
        let ids = p.ids().to_vec();

        // Frame 0: both analog deltas, separately. Summing them would be the
        // bug this format is shaped to avoid.
        let f0: Vec<InputEvent> = p
            .take_frame(0)
            .iter()
            .filter_map(|r| r.to_input_event(&ids))
            .collect();
        assert_eq!(
            f0,
            vec![
                InputEvent::Relative {
                    id: TRACK_X,
                    delta: 0.6
                },
                InputEvent::Relative {
                    id: TRACK_X,
                    delta: 0.6
                },
            ]
        );
        assert_eq!(p.take_frame(1).len(), 1);
        assert_eq!(p.take_frame(2).len(), 1);
        assert!(p.finished());
    }

    #[test]
    fn player_returns_nothing_for_a_frame_with_no_records() {
        let mut r = recorder();
        r.advance_frame();
        r.advance_frame();
        r.push_release_all();
        r.advance_frame();
        let mut p = MoviePlayer::bind(r.finish(), CONTROLS).expect("bind");
        assert!(p.take_frame(0).is_empty());
        assert!(p.take_frame(1).is_empty());
        assert_eq!(p.take_frame(2).len(), 1);
    }

    /// A caller that jumps forward must not receive a burst of stale input for
    /// frames it skipped.
    #[test]
    fn player_discards_records_for_skipped_frames() {
        let mut r = recorder();
        r.push_release_all();
        r.advance_frame();
        r.push_release_all();
        r.advance_frame();
        r.push_release_all();
        r.advance_frame();
        let mut p = MoviePlayer::bind(r.finish(), CONTROLS).expect("bind");
        assert_eq!(p.take_frame(2).len(), 1);
        assert!(p.finished());
    }

    #[test]
    fn binding_fails_when_the_machine_lacks_a_recorded_control() {
        let m = Movie {
            header: MovieHeader {
                controls: vec!["not_a_control".into()],
                ..header()
            },
            records: Vec::new(),
        };
        assert_eq!(
            MoviePlayer::bind(m, CONTROLS).err(),
            Some(MovieError::UnknownControl("not_a_control".into()))
        );
    }

    #[test]
    fn rejects_records_that_go_backwards_in_time() {
        let m = Movie {
            header: header(),
            records: vec![
                MovieRecord::ReleaseAll { frame: 5 },
                MovieRecord::ReleaseAll { frame: 3 },
            ],
        };
        assert_eq!(
            Movie::decode(&m.encode()),
            Err(MovieError::FramesNotMonotonic {
                frame: 3,
                previous: 5
            })
        );
    }

    /// A long analog trace is the case the record block is compressed for.
    #[test]
    fn a_long_analog_trace_compresses_well() {
        let records: Vec<MovieRecord> = (0..20_000)
            .map(|i| MovieRecord::Relative {
                frame: i / 10,
                ctl: 0,
                bits: ((i % 7) as f32).to_bits(),
            })
            .collect();
        let m = Movie {
            header: MovieHeader {
                frames: 2_000,
                ..header()
            },
            records,
        };
        let encoded = m.encode();
        // 20k records at 11 raw bytes each is ~220 KB; deflate should cut that
        // several-fold on a trace this repetitive.
        assert!(
            encoded.len() < 60_000,
            "20k records encoded to {} bytes",
            encoded.len()
        );
        assert_eq!(Movie::decode(&encoded).expect("decode"), m);
    }
}
