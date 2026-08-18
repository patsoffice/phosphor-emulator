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
//! That would not replay faithfully. [`RelativeCounter::add_delta`] is
//! `pending += delta as i32` — the truncation happens on *every call*, so two
//! 0.6-deltas contribute 0 while one summed 1.2-delta contributes 1. On the
//! boards using `DrainPolicy::ClampDrop` the remainder is discarded rather than
//! carried, so that divergence never washes out over later frames. Records are
//! therefore one-to-one with delivered events, and float payloads are stored as
//! raw [`f32::to_bits`] so a replayed value truncates identically.
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

use phosphor_core::core::machine::{InputEvent, InputId};
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
        for r in &self.records {
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
