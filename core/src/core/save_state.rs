//! Binary save-state serialization framework.
//!
//! Provides [`StateWriter`] and [`StateReader`] for encoding/decoding machine
//! state into a compact binary format with no external dependencies. All
//! multi-byte values are stored in little-endian order so save files are
//! portable across architectures. Each component that participates in save
//! states implements the [`Saveable`] trait.

/// Errors that can occur during save-state operations.
#[derive(Debug)]
pub enum SaveError {
    /// Ran out of data while reading a field.
    UnexpectedEnd,
    /// Header magic, version, or structure is invalid.
    InvalidFormat(String),
    /// Save file was created by a different machine.
    MachineMismatch { expected: String, found: String },
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SaveError::UnexpectedEnd => write!(f, "unexpected end of save data"),
            SaveError::InvalidFormat(msg) => write!(f, "invalid format: {msg}"),
            SaveError::MachineMismatch { expected, found } => {
                write!(f, "machine mismatch: expected {expected}, found {found}")
            }
        }
    }
}

// -- File format constants ---------------------------------------------------

/// Magic bytes at the start of every save file.
pub const SAVE_MAGIC: &[u8; 4] = b"PHOS";

/// Current save-state format version.
///
/// Bumped to 5 when the Galaga-family boards moved their three Z80s out of the
/// board and into the machine wrapper (for concrete bus dispatch), which moved
/// the CPU block ahead of the RAM block in those machines' state. Older files
/// are rejected by version rather than misread.
///
/// Bumped to 6 when the Williams blitter gained the stall flag that makes a
/// slow blit cost two clocks a byte, which adds a byte to every Williams
/// machine's state. The blitter's own component tag went 1 to 2 with it; the
/// global bump is what gives an old file a clear rejection instead of a
/// component-level one.
///
/// Bumped to 7 when Mr. Do! gained the output coupling capacitor its PSGs
/// reached the speaker without, which adds the blocker's state to that board's
/// stream.
///
/// Bumped to 8 when Gottlieb System 80's two `ClockDivider` fields became a
/// `ClockTree` owned by its sound board, moving those bytes out of the end of
/// the board's block and into the middle of the sound board's.
///
/// Bumped to 9 when the last two hand-rolled clock accumulators, Atari System
/// 1's TMS5220 clock-select and Star Wars' `tms_clock_acc`, became clock-tree
/// domains. Each replaced a rate plus an accumulator with the domain's own
/// saved ratio.
///
/// The format is positional, so every board whose field layout changes costs a
/// bump like those. `phosphor-emulator-tlv-save-state-hc61` Stage A is what
/// makes that cost go away.
pub const SAVE_VERSION: u32 = 9;

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
}

impl Default for StateWriter {
    fn default() -> Self {
        Self::new()
    }
}

// -- StateReader -------------------------------------------------------------

/// Reads primitive values from a byte slice in little-endian order.
#[derive(Debug)]
pub struct StateReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> StateReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
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
    if version != SAVE_VERSION {
        return Err(SaveError::InvalidFormat(format!(
            "unsupported version {version}"
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

/// Serialize a `Saveable` struct with the standard machine header.
pub fn save_machine(saveable: &impl Saveable, machine_id: &str) -> Vec<u8> {
    let mut w = StateWriter::new();
    write_header(&mut w, machine_id);
    saveable.save_state(&mut w);
    w.into_vec()
}

/// Deserialize a `Saveable` struct, validating the machine header first.
pub fn load_machine(
    saveable: &mut impl Saveable,
    machine_id: &str,
    data: &[u8],
) -> Result<(), SaveError> {
    let mut r = read_header(data, machine_id)?;
    saveable.load_state(&mut r)
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
}
