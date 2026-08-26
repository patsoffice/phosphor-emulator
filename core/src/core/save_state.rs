//! Binary save-state serialization framework.
//!
//! Provides [`StateWriter`] and [`StateReader`] for encoding/decoding machine
//! state into a compact binary format with no external dependencies. All
//! multi-byte values are stored in little-endian order so save files are
//! portable across architectures. Each component that participates in save
//! states implements the [`Saveable`] trait.
//!
//! # Chunk framing
//!
//! ```text
//! file      := header | body | u32 crc32_ieee_le
//! header    := magic:4 b"PHOS" | file_version:u32 | u32 id_len | machine_id utf8
//! body      := <the top-level Saveable's payload>
//! chunk     := tag:u16 | len:u32 | payload:len bytes
//! ```
//!
//! A component payload is `component_version:u8 | fields`, where the fields are
//! written in declaration order. Scalars are inline; every *nested component* is
//! wrapped in a chunk by its parent.
//!
//! Two rules make that work:
//!
//! * **Parents frame children; children never frame themselves.** A struct
//!   cannot know the tag its parent filed it under, so [`Saveable::save_state`]
//!   and [`Saveable::load_state`] write and read a *payload*, never a frame.
//!   [`StateWriter::write_tlv`] and [`StateReader::read_component`] own all
//!   framing.
//! * **Readers are bounded to their chunk.** [`StateReader::sub`] hands a child
//!   an independent reader over exactly its own bytes, and the parent's cursor
//!   advances past them however much the child consumes. A child that misreads
//!   its own body therefore cannot walk into its parent's next field, and an
//!   unknown chunk can be skipped by length.
//!
//! Chunk tags are ordinals assigned by the parent in declaration order, so field
//! order is still wire order. Inserting or removing a component changes the
//! chunk count and is always caught; *reordering* renumbers both components and
//! is caught only where their bodies differ. Reordering is a wire change: bump
//! the parent's `#[save_version]` when you do it. Explicit stable ids are
//! Stage B (`phosphor-emulator-tlv-save-state-hc61.3`).

/// Errors that can occur during save-state operations.
#[derive(Debug)]
pub enum SaveError {
    /// Ran out of data while reading a field.
    UnexpectedEnd,
    /// Header magic, version, or structure is invalid.
    InvalidFormat(String),
    /// Save file was created by a different machine.
    MachineMismatch { expected: String, found: String },
    /// A failure inside a named component chunk. Nests, so the message reads as
    /// a path from the machine down to the component that actually failed.
    Component {
        path: String,
        source: Box<SaveError>,
    },
}

impl SaveError {
    /// Wrap `self` as having occurred inside the component named `path`.
    fn in_component(self, path: &str) -> SaveError {
        SaveError::Component {
            path: path.to_string(),
            source: Box::new(self),
        }
    }
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::UnexpectedEnd => write!(f, "unexpected end of save data"),
            SaveError::InvalidFormat(msg) => write!(f, "invalid format: {msg}"),
            SaveError::MachineMismatch { expected, found } => {
                write!(f, "machine mismatch: expected {expected}, found {found}")
            }
            SaveError::Component { path, source } => write!(f, "{path}: {source}"),
        }
    }
}

// -- File format constants ---------------------------------------------------

/// Magic bytes at the start of every save file.
pub const SAVE_MAGIC: &[u8; 4] = b"PHOS";

/// Current save-state *envelope* version.
///
/// Versions 1 through 12 were bumped for component changes, because the body was
/// one flat positional concatenation and there was no other way to reject a file
/// whose layout had moved. Two of the first four bumps, and every bump from 6
/// on, was a single-subsystem change that invalidated every save for every
/// machine on disk, including machines that do not contain the changed part.
///
/// Bumped to 13 for chunk framing (`phosphor-emulator-tlv-save-state-hc61`
/// Stage A). This is the last global invalidation: from here on a component
/// change bumps that component's `#[save_version]`, the envelope stays at 13,
/// and only machines that actually contain the component lose their saves. No
/// component versions were bumped for this change, because no component body
/// changed; the floor below rejects every older file outright.
pub const SAVE_VERSION: u32 = 13;

/// Oldest envelope this build can read.
///
/// Equal to [`SAVE_VERSION`] because version 13 replaced the flat body with a
/// chunked one, so a version 12 file has no chunk boundaries to parse. Keeping
/// an explicit floor means such a file is rejected by version with a clear
/// message instead of being fed to the chunk reader and misread. Future envelope
/// bumps that stay readable leave this at 13.
pub const MIN_SUPPORTED_SAVE_VERSION: u32 = 13;

// -- CRC-32 (IEEE 802.3, reflected) ------------------------------------------

const fn crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut bit = 0;
        while bit < 8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            bit += 1;
        }
        table[i] = c;
        i += 1;
    }
    table
}

static CRC32_TABLE: [u32; 256] = crc32_table();

/// CRC-32/ISO-HDLC of `data`, the checksum stored in the save file trailer.
pub fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c = CRC32_TABLE[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
    }
    !c
}

// -- Chunk tags --------------------------------------------------------------

/// Tag `0` is reserved so that a run of zero bytes cannot look like a chunk.
pub const TAG_RESERVED_ZERO: u16 = 0;
/// Tag `0xFFFF` is reserved so that a run of `0xFF` bytes cannot look like one.
pub const TAG_RESERVED_MAX: u16 = 0xFFFF;

/// Bytes of framing a chunk costs: `u16` tag plus `u32` length.
pub const CHUNK_HEADER_LEN: usize = 6;

// -- Saveable trait ----------------------------------------------------------

/// A component whose mutable state can be captured and restored.
pub trait Saveable {
    fn save_state(&self, w: &mut StateWriter);
    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError>;
}

// -- StateWriter -------------------------------------------------------------

/// Appends primitive values to an internal `Vec<u8>` in little-endian order.
pub struct StateWriter {
    data: Vec<u8>,
}

impl StateWriter {
    pub fn new() -> Self {
        Self {
            data: Vec::with_capacity(64 * 1024),
        }
    }

    pub fn write_u8(&mut self, v: u8) {
        self.data.push(v);
    }

    pub fn write_u16_le(&mut self, v: u16) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    pub fn write_u32_le(&mut self, v: u32) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    pub fn write_u64_le(&mut self, v: u64) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    pub fn write_i16_le(&mut self, v: i16) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    pub fn write_i32_le(&mut self, v: i32) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    pub fn write_i64_le(&mut self, v: i64) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    pub fn write_f32_le(&mut self, v: f32) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    pub fn write_f64_le(&mut self, v: f64) {
        self.data.extend_from_slice(&v.to_le_bytes());
    }

    pub fn write_bool(&mut self, v: bool) {
        self.data.push(v as u8);
    }

    /// Write a length-prefixed byte slice (u32 LE length + data).
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.write_u32_le(bytes.len() as u32);
        self.data.extend_from_slice(bytes);
    }

    /// Write a component version tag. Each `Saveable` implementation should
    /// call this first in `save_state()` so format changes can be detected.
    pub fn write_version(&mut self, version: u8) {
        self.data.push(version);
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.data
    }

    // -- Chunk framing -------------------------------------------------------

    /// Open a chunk: writes `tag` and a placeholder length.
    ///
    /// The returned guard must be handed back to [`Self::end_chunk`], which
    /// patches the length. Prefer [`Self::write_tlv`], which cannot be
    /// mismatched.
    #[must_use = "a chunk opened with begin_chunk must be closed with end_chunk"]
    pub fn begin_chunk(&mut self, tag: u16) -> ChunkGuard {
        assert!(
            tag != TAG_RESERVED_ZERO && tag != TAG_RESERVED_MAX,
            "chunk tag {tag:#06x} is reserved"
        );
        self.write_u16_le(tag);
        let len_pos = self.data.len();
        self.write_u32_le(0);
        ChunkGuard { len_pos }
    }

    /// Close a chunk, patching in the payload length written since it opened.
    pub fn end_chunk(&mut self, g: ChunkGuard) {
        let len = self.data.len() - (g.len_pos + 4);
        let len = u32::try_from(len).expect("chunk payload exceeds u32");
        self.data[g.len_pos..g.len_pos + 4].copy_from_slice(&len.to_le_bytes());
    }

    /// Write everything `f` emits as one `tag | len | payload` chunk.
    pub fn write_tlv<F: FnOnce(&mut Self)>(&mut self, tag: u16, f: F) {
        let g = self.begin_chunk(tag);
        f(self);
        self.end_chunk(g);
    }

    /// Frame a nested component under `tag`.
    pub fn write_component<T: Saveable + ?Sized>(&mut self, tag: u16, component: &T) {
        self.write_tlv(tag, |w| component.save_state(w));
    }

    /// Frame a component that this hardware configuration may not have.
    ///
    /// `None` writes nothing at all, which is what makes an absent component
    /// distinguishable from the next component's bytes. Read it back with
    /// [`StateReader::read_optional_component`].
    pub fn write_optional_component<T: Saveable>(&mut self, tag: u16, component: Option<&T>) {
        if let Some(c) = component {
            self.write_component(tag, c);
        }
    }
}

/// Records where a chunk's length field is so [`StateWriter::end_chunk`] can
/// patch it once the payload is known.
#[derive(Debug)]
pub struct ChunkGuard {
    len_pos: usize,
}

impl Default for StateWriter {
    fn default() -> Self {
        Self::new()
    }
}

// -- StateReader -------------------------------------------------------------

/// Reads primitive values from a byte slice in little-endian order.
///
/// A reader is bounded to its own bytes: [`Self::sub`] hands a child an
/// independent reader over exactly one chunk's payload, so a child cannot read
/// past its own chunk however wrong its body turns out to be.
#[derive(Debug)]
pub struct StateReader<'a> {
    data: &'a [u8],
    pos: usize,
    /// Offset of `data[0]` within the whole file, so diagnostics and the chunk
    /// dumper can name an absolute position rather than a chunk-relative one.
    base: usize,
    /// Optional recorder for the chunk tree; `None` on every ordinary load.
    trace: Option<&'a std::cell::RefCell<ChunkTrace>>,
}

impl<'a> StateReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            base: 0,
            trace: None,
        }
    }

    /// A reader that records the chunk tree it walks into `trace`.
    pub fn with_trace(data: &'a [u8], trace: &'a std::cell::RefCell<ChunkTrace>) -> Self {
        Self {
            data,
            pos: 0,
            base: 0,
            trace: Some(trace),
        }
    }

    /// Read exactly `n` bytes, advancing the cursor.
    fn take(&mut self, n: usize) -> Result<&'a [u8], SaveError> {
        if self.pos + n > self.data.len() {
            return Err(SaveError::UnexpectedEnd);
        }
        let slice = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    pub fn read_u8(&mut self) -> Result<u8, SaveError> {
        Ok(self.take(1)?[0])
    }

    pub fn read_u16_le(&mut self) -> Result<u16, SaveError> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub fn read_u32_le(&mut self) -> Result<u32, SaveError> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn read_u64_le(&mut self) -> Result<u64, SaveError> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    pub fn read_i16_le(&mut self) -> Result<i16, SaveError> {
        let b = self.take(2)?;
        Ok(i16::from_le_bytes([b[0], b[1]]))
    }

    pub fn read_i32_le(&mut self) -> Result<i32, SaveError> {
        let b = self.take(4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn read_i64_le(&mut self) -> Result<i64, SaveError> {
        let b = self.take(8)?;
        Ok(i64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    pub fn read_f32_le(&mut self) -> Result<f32, SaveError> {
        let b = self.take(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn read_f64_le(&mut self) -> Result<f64, SaveError> {
        let b = self.take(8)?;
        Ok(f64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    pub fn read_bool(&mut self) -> Result<bool, SaveError> {
        Ok(self.read_u8()? != 0)
    }

    /// Read a length-prefixed byte blob into `buf`.
    /// Returns an error if the encoded length does not match `buf.len()`.
    pub fn read_bytes_into(&mut self, buf: &mut [u8]) -> Result<(), SaveError> {
        let len = self.read_u32_le()? as usize;
        if len != buf.len() {
            return Err(SaveError::InvalidFormat(format!(
                "expected {} bytes, got {len}",
                buf.len()
            )));
        }
        let slice = self.take(len)?;
        buf.copy_from_slice(slice);
        Ok(())
    }

    /// Read a length-prefixed byte blob, returning a borrowed slice.
    pub fn read_bytes(&mut self) -> Result<&'a [u8], SaveError> {
        let len = self.read_u32_le()? as usize;
        self.take(len)
    }

    /// Read and validate a component version tag. Returns an error if the
    /// version does not match `expected`.
    pub fn read_version(&mut self, expected: u8) -> Result<(), SaveError> {
        let found = self.read_u8()?;
        if found != expected {
            return Err(SaveError::InvalidFormat(format!(
                "component version mismatch: expected {expected}, found {found}"
            )));
        }
        Ok(())
    }

    // -- Chunk framing -------------------------------------------------------

    /// Bytes left in *this* reader, which for a sub-reader is the rest of its
    /// own chunk, not the rest of the file.
    pub fn remaining(&self) -> usize {
        self.data.len() - self.pos
    }

    /// Offset of the cursor within the whole file.
    pub fn offset(&self) -> usize {
        self.base + self.pos
    }

    /// Discard `n` bytes.
    pub fn skip(&mut self, n: u32) -> Result<(), SaveError> {
        self.take(n as usize)?;
        Ok(())
    }

    /// Borrow the next `len` bytes as an independent reader.
    ///
    /// This reader's cursor advances past them regardless of how much the child
    /// consumes, which is what stops a child that misreads its own body from
    /// corrupting its parent's next field.
    pub fn sub(&mut self, len: u32) -> Result<StateReader<'a>, SaveError> {
        let start = self.pos;
        let slice = self.take(len as usize)?;
        Ok(StateReader {
            data: slice,
            pos: 0,
            base: self.base + start,
            trace: self.trace,
        })
    }

    /// Tag of the next chunk without consuming it, or `None` at this reader's
    /// end. Errors if fewer than a chunk header's worth of bytes remain.
    pub fn peek_tag(&self) -> Result<Option<u16>, SaveError> {
        if self.remaining() == 0 {
            return Ok(None);
        }
        if self.remaining() < CHUNK_HEADER_LEN {
            return Err(SaveError::UnexpectedEnd);
        }
        Ok(Some(u16::from_le_bytes([
            self.data[self.pos],
            self.data[self.pos + 1],
        ])))
    }

    /// Read the next chunk header, or `None` at this reader's end.
    pub fn read_tag_len(&mut self) -> Result<Option<(u16, u32)>, SaveError> {
        if self.remaining() == 0 {
            return Ok(None);
        }
        let tag = self.read_u16_le()?;
        let len = self.read_u32_le()?;
        if len as usize > self.remaining() {
            return Err(SaveError::InvalidFormat(format!(
                "chunk {tag} claims {len} bytes but only {} remain",
                self.remaining()
            )));
        }
        Ok(Some((tag, len)))
    }

    /// Read one chunk that must be present and must carry `tag`, handing `f` a
    /// reader bounded to its payload.
    ///
    /// `f` must consume the whole payload; bytes left over mean the reader and
    /// the writer disagree about the body, which is reported against `name`
    /// rather than allowed to surface as a wrong value somewhere downstream.
    pub fn read_component<F>(&mut self, tag: u16, name: &str, f: F) -> Result<(), SaveError>
    where
        F: FnOnce(&mut StateReader<'a>) -> Result<(), SaveError>,
    {
        let at = self.offset();
        let (found, len) = self
            .read_tag_len()?
            .ok_or(SaveError::UnexpectedEnd)
            .map_err(|e| e.in_component(name))?;
        if found != tag {
            return Err(SaveError::InvalidFormat(format!(
                "expected chunk tag {tag} at offset {at}, found {found}"
            ))
            .in_component(name));
        }
        self.enter_trace(tag, name, at, len);
        let mut child = self.sub(len)?;
        let result = f(&mut child);
        self.exit_trace();
        result.map_err(|e| e.in_component(name))?;
        if child.remaining() != 0 {
            return Err(SaveError::InvalidFormat(format!(
                "{} of {len} bytes left unread",
                child.remaining()
            ))
            .in_component(name));
        }
        Ok(())
    }

    /// Read a chunk that this hardware configuration may or may not have.
    ///
    /// # An optional chunk must be followed by a chunk or by nothing
    ///
    /// Absence is detected by peeking at the next tag, so whatever follows an
    /// optional chunk has to be readable as one. Put the optional components at
    /// the end of a body, or frame every field after them. Inline scalars after
    /// an optional chunk would be read as a tag, and if they happened to equal
    /// `tag` the component would be parsed out of scalar bytes. There is no way
    /// to check that here; it is a rule for the body that calls this.
    ///
    /// `present` says whether *this build* has the component. All four
    /// combinations are handled explicitly, because the failure that matters is
    /// the one where the file and the configuration disagree:
    ///
    /// * present in both: read it.
    /// * in the file only: skip it, and say so in the trace. Another
    ///   configuration of the same board wrote it.
    /// * in the configuration only: error. Loading would otherwise leave a live
    ///   device at power-on while the rest of the machine is at frame N.
    /// * in neither: nothing to do.
    pub fn read_optional<F>(
        &mut self,
        tag: u16,
        name: &str,
        present: bool,
        f: F,
    ) -> Result<(), SaveError>
    where
        F: FnOnce(&mut StateReader<'a>) -> Result<(), SaveError>,
    {
        let in_file = self.peek_tag()? == Some(tag);
        match (in_file, present) {
            (true, true) => self.read_component(tag, name, f),
            (true, false) => {
                let at = self.offset();
                let (_, len) = self.read_tag_len()?.ok_or(SaveError::UnexpectedEnd)?;
                self.record_skipped(tag, name, at, len);
                self.skip(len)
            }
            (false, true) => Err(SaveError::InvalidFormat(format!(
                "this configuration has the component but the file has no chunk {tag}"
            ))
            .in_component(name)),
            (false, false) => Ok(()),
        }
    }

    /// [`Self::read_optional`] for the common case of an `Option<T>` field.
    pub fn read_optional_component<T: Saveable>(
        &mut self,
        tag: u16,
        name: &str,
        slot: Option<&mut T>,
    ) -> Result<(), SaveError> {
        match slot {
            Some(c) => self.read_optional(tag, name, true, |r| c.load_state(r)),
            None => self.read_optional(tag, name, false, |_| Ok(())),
        }
    }

    fn enter_trace(&self, tag: u16, name: &str, offset: usize, len: u32) {
        if let Some(t) = self.trace {
            t.borrow_mut().enter(tag, name, offset, len);
        }
    }

    fn exit_trace(&self) {
        if let Some(t) = self.trace {
            t.borrow_mut().exit();
        }
    }

    fn record_skipped(&self, tag: u16, name: &str, offset: usize, len: u32) {
        if let Some(t) = self.trace {
            let mut t = t.borrow_mut();
            t.enter(tag, name, offset, len);
            t.mark_skipped();
            t.exit();
        }
    }
}

// -- Chunk trace -------------------------------------------------------------

/// One chunk seen while reading, in the order it was reached.
#[derive(Debug, Clone)]
pub struct ChunkEvent {
    /// Nesting depth; 0 is a chunk in the machine's own body.
    pub depth: usize,
    pub tag: u16,
    /// `Struct.field` path the parent filed this chunk under.
    pub name: String,
    /// Offset of the chunk header within the file.
    pub offset: usize,
    /// Payload length, not counting the six-byte header.
    pub len: u32,
    /// The chunk was in the file but not in this build's configuration.
    pub skipped: bool,
}

/// Chunk tree recorded by a [`StateReader::with_trace`] load.
///
/// The trace is built by an ordinary load, so it reflects what the *reader*
/// makes of the file rather than a guess from its bytes. A load that fails part
/// way leaves the chunks read so far in place, which is the point: the last
/// entry is where it stopped.
#[derive(Debug, Default)]
pub struct ChunkTrace {
    events: Vec<ChunkEvent>,
    depth: usize,
}

impl ChunkTrace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> &[ChunkEvent] {
        &self.events
    }

    fn enter(&mut self, tag: u16, name: &str, offset: usize, len: u32) {
        self.events.push(ChunkEvent {
            depth: self.depth,
            tag,
            name: name.to_string(),
            offset,
            len,
            skipped: false,
        });
        self.depth += 1;
    }

    fn mark_skipped(&mut self) {
        if let Some(e) = self.events.last_mut() {
            e.skipped = true;
        }
    }

    fn exit(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }
}

// -- Header helpers ----------------------------------------------------------

/// Write the save-file header (magic + version + machine id).
pub fn write_header(w: &mut StateWriter, machine_id: &str) {
    w.data.extend_from_slice(SAVE_MAGIC);
    w.write_u32_le(SAVE_VERSION);
    let id_bytes = machine_id.as_bytes();
    w.write_u32_le(id_bytes.len() as u32);
    w.data.extend_from_slice(id_bytes);
}

/// Validate the header and return a reader positioned after it.
pub fn read_header<'a>(data: &'a [u8], expected_id: &str) -> Result<StateReader<'a>, SaveError> {
    let mut r = StateReader::new(data);

    let magic = r.take(4)?;
    if magic != SAVE_MAGIC {
        return Err(SaveError::InvalidFormat("bad magic".into()));
    }

    let version = r.read_u32_le()?;
    if version > SAVE_VERSION {
        return Err(SaveError::InvalidFormat(format!(
            "save version {version} is newer than this build understands (max {SAVE_VERSION})"
        )));
    }
    if version < MIN_SUPPORTED_SAVE_VERSION {
        return Err(SaveError::InvalidFormat(format!(
            "save version {version} predates chunk framing (minimum {MIN_SUPPORTED_SAVE_VERSION})"
        )));
    }

    let id_len = r.read_u32_le()? as usize;
    let id_bytes = r.take(id_len)?;
    let found_id = std::str::from_utf8(id_bytes)
        .map_err(|_| SaveError::InvalidFormat("non-UTF8 machine id".into()))?;

    if found_id != expected_id {
        return Err(SaveError::MachineMismatch {
            expected: expected_id.to_string(),
            found: found_id.to_string(),
        });
    }

    Ok(r)
}

/// Serialize a `Saveable` struct with the standard machine header and a
/// trailing CRC-32 over everything before it, magic included.
pub fn save_machine(saveable: &impl Saveable, machine_id: &str) -> Vec<u8> {
    let mut w = StateWriter::new();
    write_header(&mut w, machine_id);
    saveable.save_state(&mut w);
    let mut data = w.into_vec();
    let sum = crc32(&data);
    data.extend_from_slice(&sum.to_le_bytes());
    data
}

/// Split the CRC trailer off, without checking it.
fn split_trailer(data: &[u8]) -> Result<(&[u8], u32), SaveError> {
    if data.len() < 4 {
        return Err(SaveError::UnexpectedEnd);
    }
    let (body, trailer) = data.split_at(data.len() - 4);
    Ok((
        body,
        u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]),
    ))
}

/// Split the CRC trailer off and verify it, returning the covered bytes.
fn verify_crc(data: &[u8]) -> Result<&[u8], SaveError> {
    let (body, found) = split_trailer(data)?;
    let want = crc32(body);
    if found != want {
        return Err(SaveError::InvalidFormat(format!(
            "checksum mismatch: stored {found:#010x}, computed {want:#010x}"
        )));
    }
    Ok(body)
}

/// Deserialize a `Saveable` struct, validating checksum and header first.
pub fn load_machine(
    saveable: &mut impl Saveable,
    machine_id: &str,
    data: &[u8],
) -> Result<(), SaveError> {
    let body = verify_crc(data)?;
    let mut r = read_header(body, machine_id)?;
    saveable.load_state(&mut r)?;
    finish(&r)
}

/// Deserialize while recording the chunk tree, for `disasm dump-save`.
///
/// **The checksum is not enforced here.** A traced load exists to walk a file
/// that does *not* load, and a corrupt or truncated one is the case most worth
/// walking; refusing it up front would leave the tool useful only on files that
/// were already fine. The caller reports the checksum itself. The trailer is
/// still split off so the body ends where the writer meant it to.
///
/// Whatever the outcome, `trace` holds the chunks read before it stopped.
pub fn load_machine_traced<'a>(
    saveable: &mut impl Saveable,
    machine_id: &str,
    data: &'a [u8],
    trace: &'a std::cell::RefCell<ChunkTrace>,
) -> Result<(), SaveError> {
    let (body, _crc) = split_trailer(data)?;
    // Re-validate the header, then rebuild a traced reader over the same body.
    read_header(body, machine_id)?;
    let mut r = StateReader::with_trace(body, trace);
    r.skip(4)?; // magic
    r.read_u32_le()?; // file version
    let id_len = r.read_u32_le()?;
    r.skip(id_len)?;
    saveable.load_state(&mut r)?;
    finish(&r)
}

/// Reject bytes left after the machine's own state, which mean the reader
/// stopped short of what the writer emitted.
fn finish(r: &StateReader<'_>) -> Result<(), SaveError> {
    if r.remaining() != 0 {
        return Err(SaveError::InvalidFormat(format!(
            "{} bytes left after machine state",
            r.remaining()
        )));
    }
    Ok(())
}

// -- Tests -------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_reader_round_trip() {
        let mut w = StateWriter::new();
        w.write_u8(0xAB);
        w.write_u16_le(0x1234);
        w.write_u32_le(0xDEAD_BEEF);
        w.write_u64_le(0x0102_0304_0506_0708);
        w.write_i16_le(-1234);
        w.write_i64_le(-42);
        w.write_f32_le(std::f32::consts::PI);
        w.write_f64_le(std::f64::consts::E);
        w.write_bool(true);
        w.write_bool(false);
        w.write_bytes(&[1, 2, 3, 4, 5]);

        let data = w.into_vec();
        let mut r = StateReader::new(&data);

        assert_eq!(r.read_u8().unwrap(), 0xAB);
        assert_eq!(r.read_u16_le().unwrap(), 0x1234);
        assert_eq!(r.read_u32_le().unwrap(), 0xDEAD_BEEF);
        assert_eq!(r.read_u64_le().unwrap(), 0x0102_0304_0506_0708);
        assert_eq!(r.read_i16_le().unwrap(), -1234);
        assert_eq!(r.read_i64_le().unwrap(), -42);
        assert!((r.read_f32_le().unwrap() - std::f32::consts::PI).abs() < f32::EPSILON);
        assert!((r.read_f64_le().unwrap() - std::f64::consts::E).abs() < f64::EPSILON);
        assert!(r.read_bool().unwrap());
        assert!(!r.read_bool().unwrap());

        let blob = r.read_bytes().unwrap();
        assert_eq!(blob, &[1, 2, 3, 4, 5]);
    }

    #[test]
    fn read_bytes_into_round_trip() {
        let mut w = StateWriter::new();
        let src = [0xCA, 0xFE, 0xBA, 0xBE];
        w.write_bytes(&src);

        let data = w.into_vec();
        let mut r = StateReader::new(&data);
        let mut dst = [0u8; 4];
        r.read_bytes_into(&mut dst).unwrap();
        assert_eq!(dst, src);
    }

    #[test]
    fn read_bytes_into_length_mismatch() {
        let mut w = StateWriter::new();
        w.write_bytes(&[1, 2, 3]);

        let data = w.into_vec();
        let mut r = StateReader::new(&data);
        let mut dst = [0u8; 5];
        assert!(r.read_bytes_into(&mut dst).is_err());
    }

    #[test]
    fn reader_unexpected_end() {
        let mut r = StateReader::new(&[0x01]);
        assert!(r.read_u8().is_ok());
        assert!(matches!(r.read_u8(), Err(SaveError::UnexpectedEnd)));
    }

    #[test]
    fn header_round_trip() {
        let mut w = StateWriter::new();
        write_header(&mut w, "joust");
        w.write_u8(0xFF);

        let data = w.into_vec();
        let mut r = read_header(&data, "joust").unwrap();
        assert_eq!(r.read_u8().unwrap(), 0xFF);
    }

    #[test]
    fn header_machine_mismatch() {
        let mut w = StateWriter::new();
        write_header(&mut w, "joust");
        let data = w.into_vec();

        let err = read_header(&data, "pacman").unwrap_err();
        assert!(matches!(err, SaveError::MachineMismatch { .. }));
    }

    #[test]
    fn header_bad_magic() {
        let data = b"BAD!\x02\x00\x00\x00\x05\x00\x00\x00joust";
        let err = read_header(data, "joust").unwrap_err();
        assert!(matches!(err, SaveError::InvalidFormat(_)));
    }

    // -- CRC ----------------------------------------------------------------

    #[test]
    fn crc32_matches_the_published_check_value() {
        // The check value every CRC-32/ISO-HDLC implementation is defined by.
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        assert_eq!(crc32(b""), 0);
    }

    /// A single flipped bit anywhere in the file must be caught. Checked at
    /// three positions because a checksum applied to the wrong span (body only,
    /// or header only) still passes for bits outside that span.
    #[test]
    fn crc_catches_a_flipped_bit_anywhere() {
        let mut w = StateWriter::new();
        write_header(&mut w, "joust");
        w.write_bytes(&[0x11; 64]);
        let mut data = w.into_vec();
        let sum = crc32(&data);
        data.extend_from_slice(&sum.to_le_bytes());

        assert!(verify_crc(&data).is_ok());
        for pos in [0, 6, data.len() - 6] {
            let mut corrupt = data.clone();
            corrupt[pos] ^= 0x01;
            assert!(
                verify_crc(&corrupt).is_err(),
                "flipping byte {pos} went undetected"
            );
        }
    }

    // -- Envelope version ---------------------------------------------------

    fn header_with_version(version: u32, id: &str) -> Vec<u8> {
        let mut data = Vec::new();
        data.extend_from_slice(SAVE_MAGIC);
        data.extend_from_slice(&version.to_le_bytes());
        data.extend_from_slice(&(id.len() as u32).to_le_bytes());
        data.extend_from_slice(id.as_bytes());
        data
    }

    #[test]
    fn a_pre_chunk_file_is_rejected_by_the_version_floor() {
        let data = header_with_version(MIN_SUPPORTED_SAVE_VERSION - 1, "joust");
        let err = read_header(&data, "joust").unwrap_err();
        assert!(
            err.to_string().contains("predates chunk framing"),
            "unexpected message: {err}"
        );
    }

    #[test]
    fn a_newer_file_is_rejected_as_newer() {
        let data = header_with_version(SAVE_VERSION + 1, "joust");
        let err = read_header(&data, "joust").unwrap_err();
        assert!(
            err.to_string().contains("newer than this build"),
            "unexpected message: {err}"
        );
    }

    // -- Chunk framing ------------------------------------------------------

    #[test]
    fn write_tlv_frames_the_payload_it_wrote() {
        let mut w = StateWriter::new();
        w.write_tlv(7, |w| {
            w.write_u32_le(0xAABB_CCDD);
            w.write_u8(9);
        });
        let data = w.into_vec();

        assert_eq!(data.len(), CHUNK_HEADER_LEN + 5);
        let mut r = StateReader::new(&data);
        assert_eq!(r.read_tag_len().unwrap(), Some((7, 5)));
    }

    #[test]
    #[should_panic(expected = "reserved")]
    fn tag_zero_is_rejected_at_write_time() {
        let mut w = StateWriter::new();
        w.write_tlv(TAG_RESERVED_ZERO, |_| {});
    }

    #[test]
    #[should_panic(expected = "reserved")]
    fn tag_ffff_is_rejected_at_write_time() {
        let mut w = StateWriter::new();
        w.write_tlv(TAG_RESERVED_MAX, |_| {});
    }

    /// The whole reason chunking is worth anything: a child that reads less
    /// than it was given must not leave the parent's cursor inside the child's
    /// bytes. Without `sub`, the parent's next read here returns 0x22, not the
    /// marker.
    #[test]
    fn a_child_that_under_reads_does_not_move_the_parents_cursor() {
        let mut w = StateWriter::new();
        w.write_tlv(1, |w| {
            w.write_u8(0x11);
            w.write_u8(0x22);
            w.write_u8(0x33);
        });
        w.write_u32_le(0xDEAD_BEEF);
        let data = w.into_vec();

        let mut r = StateReader::new(&data);
        let (tag, len) = r.read_tag_len().unwrap().unwrap();
        assert_eq!((tag, len), (1, 3));
        let mut child = r.sub(len).unwrap();
        assert_eq!(child.read_u8().unwrap(), 0x11); // child stops early
        assert_eq!(child.remaining(), 2);

        assert_eq!(r.read_u32_le().unwrap(), 0xDEAD_BEEF);
        assert_eq!(r.remaining(), 0);
    }

    /// The other half: a child that reads too much is stopped at its own
    /// boundary rather than eating its parent's next field.
    #[test]
    fn a_child_that_over_reads_hits_its_own_end_not_its_parents() {
        let mut w = StateWriter::new();
        w.write_tlv(1, |w| w.write_u8(0x11));
        w.write_u32_le(0xDEAD_BEEF);
        let data = w.into_vec();

        let mut r = StateReader::new(&data);
        let (_, len) = r.read_tag_len().unwrap().unwrap();
        let mut child = r.sub(len).unwrap();
        assert_eq!(child.read_u8().unwrap(), 0x11);
        assert!(matches!(child.read_u8(), Err(SaveError::UnexpectedEnd)));

        assert_eq!(r.read_u32_le().unwrap(), 0xDEAD_BEEF);
    }

    #[test]
    fn read_component_names_a_tag_mismatch() {
        let mut w = StateWriter::new();
        w.write_tlv(2, |w| w.write_u8(0));
        let data = w.into_vec();

        let mut r = StateReader::new(&data);
        let err = r
            .read_component(1, "Board.blitter", |r| r.read_u8().map(|_| ()))
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Board.blitter"), "unexpected message: {msg}");
        assert!(msg.contains("found 2"), "unexpected message: {msg}");
    }

    #[test]
    fn read_component_rejects_a_body_it_did_not_finish() {
        let mut w = StateWriter::new();
        w.write_tlv(1, |w| {
            w.write_u8(1);
            w.write_u8(2);
        });
        let data = w.into_vec();

        let mut r = StateReader::new(&data);
        let err = r
            .read_component(1, "Board.dac", |r| r.read_u8().map(|_| ()))
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Board.dac"), "unexpected message: {msg}");
        assert!(msg.contains("left unread"), "unexpected message: {msg}");
    }

    #[test]
    fn a_component_error_reads_as_a_path() {
        let mut w = StateWriter::new();
        w.write_tlv(1, |w| w.write_tlv(1, |w| w.write_u8(3)));
        let data = w.into_vec();

        let mut r = StateReader::new(&data);
        let err = r
            .read_component(1, "Machine.board", |r| {
                r.read_component(1, "Board.pia", |r| r.read_version(1))
            })
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "Machine.board: Board.pia: invalid format: \
             component version mismatch: expected 1, found 3"
        );
    }

    #[test]
    fn read_tag_len_rejects_a_length_past_this_readers_end() {
        let mut data = Vec::new();
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&99u32.to_le_bytes());
        data.push(0);

        let mut r = StateReader::new(&data);
        let err = r.read_tag_len().unwrap_err();
        assert!(
            err.to_string().contains("claims 99 bytes"),
            "unexpected message: {err}"
        );
    }

    // -- Optional components ------------------------------------------------

    #[derive(Debug, Default, PartialEq)]
    struct Cvsd(u8);

    impl Saveable for Cvsd {
        fn save_state(&self, w: &mut StateWriter) {
            w.write_u8(self.0);
        }
        fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
            self.0 = r.read_u8()?;
            Ok(())
        }
    }

    /// A board with the optional chip writes it; the same board without writes
    /// nothing at all. The field after it is framed too, which is what lets the
    /// reader tell an absent chunk from the next one.
    fn board_bytes(cvsd: Option<&Cvsd>) -> Vec<u8> {
        let mut w = StateWriter::new();
        w.write_optional_component(4, cvsd);
        w.write_tlv(5, |w| w.write_u32_le(0xFEED_FACE));
        w.into_vec()
    }

    fn read_trailing_marker(r: &mut StateReader<'_>) -> u32 {
        let mut marker = 0;
        r.read_component(5, "Board.marker", |r| {
            marker = r.read_u32_le()?;
            Ok(())
        })
        .unwrap();
        marker
    }

    #[test]
    fn an_optional_component_round_trips_present_and_absent() {
        for chip in [Some(Cvsd(0x5A)), None] {
            let data = board_bytes(chip.as_ref());
            let mut slot = chip.as_ref().map(|_| Cvsd::default());
            let mut r = StateReader::new(&data);
            r.read_optional_component(4, "Board.cvsd", slot.as_mut())
                .unwrap();
            assert_eq!(read_trailing_marker(&mut r), 0xFEED_FACE);
            assert_eq!(slot, chip);
        }
    }

    /// The absent case must not merely happen to work: it must be *shorter*.
    /// A discriminant byte would make both files the same length and would not
    /// be skippable by a reader that does not know the field.
    #[test]
    fn an_absent_optional_component_occupies_no_bytes() {
        let with = board_bytes(Some(&Cvsd(0x5A)));
        let without = board_bytes(None);
        assert_eq!(with.len() - without.len(), CHUNK_HEADER_LEN + 1);
    }

    #[test]
    fn an_optional_component_in_the_file_but_not_the_config_is_skipped() {
        let data = board_bytes(Some(&Cvsd(0x5A)));
        let mut r = StateReader::new(&data);
        r.read_optional_component::<Cvsd>(4, "Board.cvsd", None)
            .unwrap();
        assert_eq!(read_trailing_marker(&mut r), 0xFEED_FACE);
    }

    /// The case that must never load quietly: this build has the chip, the file
    /// does not. Restoring anyway would leave a live device at power-on.
    #[test]
    fn an_optional_component_the_config_has_but_the_file_lacks_is_an_error() {
        let data = board_bytes(None);
        let mut slot = Cvsd::default();
        let mut r = StateReader::new(&data);
        let err = r
            .read_optional_component(4, "Board.cvsd", Some(&mut slot))
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Board.cvsd"), "unexpected message: {msg}");
        assert!(msg.contains("no chunk 4"), "unexpected message: {msg}");
    }

    // -- Machine envelope ---------------------------------------------------

    struct Marker(u32);

    impl Saveable for Marker {
        fn save_state(&self, w: &mut StateWriter) {
            w.write_u32_le(self.0);
        }
        fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
            self.0 = r.read_u32_le()?;
            Ok(())
        }
    }

    #[test]
    fn save_machine_round_trips_through_the_crc_trailer() {
        let data = save_machine(&Marker(0x1234_5678), "joust");
        let mut out = Marker(0);
        load_machine(&mut out, "joust", &data).unwrap();
        assert_eq!(out.0, 0x1234_5678);
    }

    #[test]
    fn load_machine_rejects_a_corrupt_file() {
        let mut data = save_machine(&Marker(1), "joust");
        let last = data.len() - 8;
        data[last] ^= 0xFF;
        let err = load_machine(&mut Marker(0), "joust", &data).unwrap_err();
        assert!(
            err.to_string().contains("checksum mismatch"),
            "unexpected message: {err}"
        );
    }

    /// A reader that stops short of what the writer emitted has misread
    /// something; saying so beats restoring a machine that is partly at
    /// power-on.
    #[test]
    fn load_machine_rejects_bytes_left_after_the_machine_state() {
        struct Short;
        impl Saveable for Short {
            fn save_state(&self, w: &mut StateWriter) {
                w.write_u32_le(1);
                w.write_u32_le(2);
            }
            fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
                r.read_u32_le()?;
                Ok(())
            }
        }
        let data = save_machine(&Short, "joust");
        let err = load_machine(&mut Short, "joust", &data).unwrap_err();
        assert!(
            err.to_string().contains("4 bytes left after machine state"),
            "unexpected message: {err}"
        );
    }

    // -- Chunk trace --------------------------------------------------------

    #[test]
    fn a_traced_load_records_the_chunk_tree() {
        struct Machine;
        impl Saveable for Machine {
            fn save_state(&self, w: &mut StateWriter) {
                w.write_tlv(1, |w| w.write_u16_le(0x1234));
                w.write_tlv(2, |w| {
                    w.write_tlv(1, |w| w.write_u8(7));
                    w.write_optional_component(2, Some(&Cvsd(9)));
                });
            }
            fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
                r.read_component(1, "Machine.cpu", |r| r.read_u16_le().map(|_| ()))?;
                r.read_component(2, "Machine.board", |r| {
                    r.read_component(1, "Board.pia", |r| r.read_u8().map(|_| ()))?;
                    // This build has no CVSD, so the chunk is skipped.
                    r.read_optional_component::<Cvsd>(2, "Board.cvsd", None)
                })
            }
        }

        let data = save_machine(&Machine, "joust");
        let trace = std::cell::RefCell::new(ChunkTrace::new());
        load_machine_traced(&mut Machine, "joust", &data, &trace).unwrap();

        let trace = trace.borrow();
        let seen: Vec<_> = trace
            .events()
            .iter()
            .map(|e| (e.depth, e.tag, e.name.as_str(), e.len, e.skipped))
            .collect();
        assert_eq!(
            seen,
            vec![
                (0, 1, "Machine.cpu", 2, false),
                (0, 2, "Machine.board", 14, false),
                (1, 1, "Board.pia", 1, false),
                (1, 2, "Board.cvsd", 1, true),
            ]
        );
        // Offsets are absolute, so a hex dump of the file lands on the header.
        let header_len = SAVE_MAGIC.len() + 4 + 4 + "joust".len();
        assert_eq!(trace.events()[0].offset, header_len);
    }
}
