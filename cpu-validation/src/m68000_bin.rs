//! Decoder for the `SingleStepTests/m68000` binary vector container.
//!
//! That suite ships `v1/*.json.bin` rather than JSON, with a `decode.py` in the
//! repository as the reference. This is that decoder in Rust, so the harness
//! reads the vectors directly instead of depending on a Python pre-pass whose
//! output would then have to be stored somewhere.
//!
//! The container is a flat little-endian encoding with a magic word in front of
//! every section, so a misread desynchronizes immediately rather than silently
//! producing plausible values. Every magic is checked.
//!
//! # What this set does differently
//!
//! Three of its conventions differ from the `680x0` set, and all three are
//! reproduced here rather than normalized away, because normalizing would
//! discard the very information that makes this set worth having:
//!
//! - **The posted address is the true word address**, with UDS and LDS carried
//!   as separate signals. The part has no A0 pin, so this is what the bus shows.
//! - **Byte data is bus-positioned**: `0xB3` reads `0xB300` under UDS and
//!   `0x00B3` under LDS. [`crate::BusTxn::byte_value`] normalizes it, which is
//!   why every transaction decoded here sets `data_bus_positioned`.
//! - **`pc` is the next prefetch address**, four ahead of where the case starts
//!   executing. [`M68000BinTestCase::execution_pc`] resolves it.
//!
//! RAM is stored as 16-bit words here, matching the part's access width, and is
//! split back into byte pairs on the way out so callers see the same shape the
//! JSON suite has.

use crate::{BusTxn, BusTxnKind, M68000Regs, M68000TestCase, TxnSize};

const MAGIC_FILE: u32 = 0x1A3F_5D71;
const MAGIC_TEST: u32 = 0xABC1_2367;
const MAGIC_NAME: u32 = 0x89AB_CDEF;
const MAGIC_STATE: u32 = 0x0123_4567;
const MAGIC_TXNS: u32 = 0x4567_89AB;

/// How far ahead of the execution point this suite's `pc` sits.
///
/// Its README: PC comes from the generator's `m_au`, the "next prefetch
/// address", which is "+4 from where the test starts executing". Confirmed
/// against the decoded `NOP` vectors, whose opcode word sits in RAM at
/// `pc - 4` and matches `prefetch[0]`.
pub const PC_PREFETCH_LEAD: u32 = 4;

/// Anything that stops the decode, always with the byte offset it happened at
/// so a container change is localized rather than merely reported.
#[derive(Debug)]
pub struct DecodeError {
    pub offset: usize,
    pub what: String,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "at byte {}: {}", self.offset, self.what)
    }
}

impl std::error::Error for DecodeError {}

type Result<T> = std::result::Result<T, DecodeError>;

/// A little-endian cursor that refuses to read past the end.
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn err<T>(&self, what: impl Into<String>) -> Result<T> {
        Err(DecodeError {
            offset: self.pos,
            what: what.into(),
        })
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.bytes.len() {
            return self.err(format!(
                "wanted {n} bytes, {} remain",
                self.bytes.len() - self.pos
            ));
        }
        let out = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    /// Read a section header: a byte count this decoder does not need, then the
    /// section's magic word.
    fn section(&mut self, expected: u32, what: &str) -> Result<()> {
        let at = self.pos;
        let _len = self.u32()?;
        let magic = self.u32()?;
        if magic != expected {
            return Err(DecodeError {
                offset: at,
                what: format!("{what}: magic 0x{magic:08X}, expected 0x{expected:08X}"),
            });
        }
        Ok(())
    }
}

/// One decoded case, keeping the fields whose meaning differs from the JSON
/// suite's alongside the shared [`M68000TestCase`].
#[derive(Debug, Clone)]
pub struct M68000BinTestCase {
    pub case: M68000TestCase,
}

impl M68000BinTestCase {
    /// The address the case actually begins executing at.
    ///
    /// This suite's `pc` is the next prefetch address, so it runs
    /// [`PC_PREFETCH_LEAD`] ahead of the instruction under test. Every
    /// comparison against this core's PC has to go through this.
    pub fn execution_pc(&self) -> u32 {
        self.case.initial.pc.wrapping_sub(PC_PREFETCH_LEAD)
    }
}

fn read_state(r: &mut Reader) -> Result<M68000Regs> {
    r.section(MAGIC_STATE, "state")?;

    let mut regs = [0u32; 19];
    for slot in &mut regs {
        *slot = r.u32()?;
    }
    let prefetch = [r.u32()? as u16, r.u32()? as u16];

    let num_ram = r.u32()? as usize;
    let mut ram = Vec::with_capacity(num_ram * 2);
    for _ in 0..num_ram {
        let addr = r.u32()?;
        let word = r.u16()?;
        if addr >= 0x0100_0000 {
            return r.err(format!("RAM address 0x{addr:08X} outside the 24-bit space"));
        }
        // Stored as a word, served as the byte pairs the JSON suite uses.
        ram.push((addr, (word >> 8) as u8));
        ram.push((addr | 1, word as u8));
    }

    Ok(M68000Regs {
        d0: regs[0],
        d1: regs[1],
        d2: regs[2],
        d3: regs[3],
        d4: regs[4],
        d5: regs[5],
        d6: regs[6],
        d7: regs[7],
        a0: regs[8],
        a1: regs[9],
        a2: regs[10],
        a3: regs[11],
        a4: regs[12],
        a5: regs[13],
        a6: regs[14],
        usp: regs[15],
        ssp: regs[16],
        sr: regs[17] as u16,
        pc: regs[18],
        prefetch,
        ram,
    })
}

fn read_transactions(r: &mut Reader) -> Result<(Vec<BusTxn>, u32)> {
    r.section(MAGIC_TXNS, "transactions")?;

    let length = r.u32()?;
    let count = r.u32()? as usize;

    let mut txns = Vec::with_capacity(count);
    for _ in 0..count {
        let tag = r.u8()?;
        let clocks = r.u32()?;

        if tag == 0 {
            txns.push(BusTxn {
                kind: BusTxnKind::Idle,
                clocks,
                fc: 0,
                addr: 0,
                size: TxnSize::Word,
                data: 0,
                uds: false,
                lds: false,
                data_bus_positioned: true,
            });
            continue;
        }

        let kind = match tag {
            1 => BusTxnKind::Write,
            2 => BusTxnKind::Read,
            3 => BusTxnKind::Tas,
            4 => BusTxnKind::ReadAddressError,
            5 => BusTxnKind::WriteAddressError,
            other => return r.err(format!("unknown transaction tag {other}")),
        };

        let fc = r.u32()?;
        let addr = r.u32()?;
        let data = r.u32()?;
        let uds = r.u32()? != 0;
        let lds = r.u32()? != 0;

        // Both strobes is a word; either alone is the corresponding byte.
        let size = if uds && lds {
            TxnSize::Word
        } else {
            TxnSize::Byte
        };

        txns.push(BusTxn {
            kind,
            clocks,
            fc,
            addr,
            size,
            data,
            uds,
            lds,
            // This suite posts the data bus as the part drives it, which is
            // what its own README describes and what the decode tests assert.
            data_bus_positioned: true,
        });
    }

    Ok((txns, length))
}

fn read_test(r: &mut Reader) -> Result<M68000BinTestCase> {
    r.section(MAGIC_TEST, "test")?;

    r.section(MAGIC_NAME, "name")?;
    let len = r.u32()? as usize;
    let name = String::from_utf8(r.take(len)?.to_vec()).map_err(|e| DecodeError {
        offset: r.pos,
        what: format!("name is not UTF-8: {e}"),
    })?;

    let initial = read_state(r)?;
    let final_state = read_state(r)?;
    let (transactions, length) = read_transactions(r)?;

    Ok(M68000BinTestCase {
        case: M68000TestCase {
            name,
            initial,
            final_state,
            length,
            transactions,
        },
    })
}

/// Decode one `.json.bin` file.
///
/// Fails rather than truncating on a short or malformed container: a partial
/// read would quietly shrink the corpus, and a gate that silently validates
/// fewer vectors than it claims is the failure mode this crate exists to avoid.
pub fn decode(bytes: &[u8]) -> Result<Vec<M68000BinTestCase>> {
    let mut r = Reader::new(bytes);

    let magic = r.u32()?;
    if magic != MAGIC_FILE {
        return Err(DecodeError {
            offset: 0,
            what: format!("file magic 0x{magic:08X}, expected 0x{MAGIC_FILE:08X}"),
        });
    }
    let count = r.u32()? as usize;

    let mut tests = Vec::with_capacity(count);
    for i in 0..count {
        tests.push(read_test(&mut r).map_err(|e| DecodeError {
            offset: e.offset,
            what: format!("test {i}: {}", e.what),
        })?);
    }

    if r.pos != bytes.len() {
        return Err(DecodeError {
            offset: r.pos,
            what: format!("{} trailing bytes after {count} tests", bytes.len() - r.pos),
        });
    }

    Ok(tests)
}

/// Decode one file from disk.
pub fn decode_file(path: &std::path::Path) -> Result<Vec<M68000BinTestCase>> {
    let bytes = std::fs::read(path).map_err(|e| DecodeError {
        offset: 0,
        what: format!("{}: {e}", path.display()),
    })?;
    decode(&bytes).map_err(|e| DecodeError {
        offset: e.offset,
        what: format!("{}: {}", path.display(), e.what),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // A container built by hand, so the decoder is checked against the format
    // rather than against whatever the vectors happen to contain. Every field
    // here is a value the decoder has to place correctly for the gate above it
    // to mean anything.

    #[derive(Default)]
    struct Builder(Vec<u8>);

    impl Builder {
        fn u8(&mut self, v: u8) -> &mut Self {
            self.0.push(v);
            self
        }
        fn u16(&mut self, v: u16) -> &mut Self {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        fn u32(&mut self, v: u32) -> &mut Self {
            self.0.extend_from_slice(&v.to_le_bytes());
            self
        }
        /// A section header: a length this decoder ignores, then the magic.
        fn section(&mut self, magic: u32) -> &mut Self {
            self.u32(0).u32(magic)
        }
        fn state(&mut self, pc: u32, prefetch: [u32; 2], ram: &[(u32, u16)]) -> &mut Self {
            self.section(MAGIC_STATE);
            // d0-d7, a0-a6, usp, ssp, sr, then pc: nineteen in all.
            for i in 0..18 {
                self.u32(i);
            }
            self.u32(pc);
            self.u32(prefetch[0]).u32(prefetch[1]);
            self.u32(ram.len() as u32);
            for &(addr, word) in ram {
                self.u32(addr).u16(word);
            }
            self
        }
    }

    /// One case: a `NOP` shaped like the real vectors, with a single four-clock
    /// word read.
    fn one_case_container() -> Vec<u8> {
        let mut b = Builder::default();
        b.u32(MAGIC_FILE).u32(1);
        b.section(MAGIC_TEST);
        b.section(MAGIC_NAME).u32(3);
        b.0.extend_from_slice(b"NOP");
        b.state(911852, [20081, 55472], &[(911848, 0x4E71)]);
        b.state(911854, [55472, 12907], &[(911848, 0x4E71)]);
        b.section(MAGIC_TXNS).u32(4).u32(1);
        b.u8(2).u32(4).u32(2).u32(911852).u32(12907).u32(1).u32(1);
        b.0
    }

    #[test]
    fn a_container_decodes_to_its_cases() {
        let tests = decode(&one_case_container()).expect("container decodes");
        assert_eq!(tests.len(), 1);
        let tc = &tests[0].case;
        assert_eq!(tc.name, "NOP");
        assert_eq!(tc.length, 4);
        assert_eq!(tc.transactions.len(), 1);
    }

    /// RAM is stored as words here and served as the byte pairs the JSON suite
    /// uses, so a caller never sees the difference. The opcode `0x4E71` has to
    /// come back as `0x4E` then `0x71`, in that order, or every loaded
    /// instruction would be byte-swapped.
    #[test]
    fn word_ram_is_served_as_big_endian_byte_pairs() {
        let tests = decode(&one_case_container()).unwrap();
        let ram = &tests[0].case.initial.ram;
        assert_eq!(ram[0], (911848, 0x4E));
        assert_eq!(ram[1], (911849, 0x71));
    }

    /// This suite's `pc` is the next prefetch address, four ahead of where the
    /// case starts executing. The invariant that pins it: the opcode word in
    /// RAM at `execution_pc` is `prefetch[0]`.
    #[test]
    fn pc_leads_the_execution_point_by_one_prefetch() {
        let tests = decode(&one_case_container()).unwrap();
        let tc = &tests[0];
        assert_eq!(tc.case.initial.pc, 911852);
        assert_eq!(tc.execution_pc(), 911848);

        let ram = &tc.case.initial.ram;
        let hi = ram
            .iter()
            .find(|&&(a, _)| a == tc.execution_pc())
            .unwrap()
            .1;
        let lo = ram
            .iter()
            .find(|&&(a, _)| a == tc.execution_pc() | 1)
            .unwrap()
            .1;
        assert_eq!(
            u16::from_be_bytes([hi, lo]),
            tc.case.initial.prefetch[0],
            "the word at the execution point is the head of the queue"
        );
    }

    /// A transfer's width comes from the strobes rather than from a size field:
    /// both is a word, either alone is that half's byte.
    #[test]
    fn the_strobes_decide_the_transfer_width() {
        let mut b = Builder::default();
        b.u32(MAGIC_FILE).u32(1);
        b.section(MAGIC_TEST);
        b.section(MAGIC_NAME).u32(1);
        b.0.extend_from_slice(b"x");
        b.state(8, [0, 0], &[]);
        b.state(8, [0, 0], &[]);
        b.section(MAGIC_TXNS).u32(12).u32(3);
        b.u8(2).u32(4).u32(1).u32(100).u32(0xB300).u32(1).u32(0); // UDS byte
        b.u8(1).u32(4).u32(1).u32(200).u32(0x00B3).u32(0).u32(1); // LDS byte
        b.u8(2).u32(4).u32(1).u32(300).u32(0x1234).u32(1).u32(1); // word

        let tests = decode(&b.0).unwrap();
        let t = &tests[0].case.transactions;

        assert_eq!(t[0].size, TxnSize::Byte);
        assert_eq!(t[0].byte_address(), 100, "UDS selects the even byte");
        assert_eq!(t[0].byte_value(), 0xB3);

        assert_eq!(t[1].size, TxnSize::Byte);
        assert_eq!(t[1].byte_address(), 201, "LDS selects the odd byte");
        assert_eq!(t[1].byte_value(), 0xB3);

        assert_eq!(t[2].size, TxnSize::Word);
        assert!(t[2].uds && t[2].lds);
    }

    /// Idle entries carry a duration and no transfer fields, and the five
    /// transfer kinds map to the tags the reference decoder uses. Getting a tag
    /// wrong would silently relabel reads as writes.
    #[test]
    fn every_transaction_tag_maps_to_its_kind() {
        let mut b = Builder::default();
        b.u32(MAGIC_FILE).u32(1);
        b.section(MAGIC_TEST);
        b.section(MAGIC_NAME).u32(1);
        b.0.extend_from_slice(b"x");
        b.state(0, [0, 0], &[]);
        b.state(0, [0, 0], &[]);
        b.section(MAGIC_TXNS).u32(22).u32(6);
        b.u8(0).u32(2);
        for tag in 1..=5u8 {
            b.u8(tag).u32(4).u32(1).u32(64).u32(0).u32(1).u32(1);
        }

        let tests = decode(&b.0).unwrap();
        let kinds: Vec<_> = tests[0].case.transactions.iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                BusTxnKind::Idle,
                BusTxnKind::Write,
                BusTxnKind::Read,
                BusTxnKind::Tas,
                BusTxnKind::ReadAddressError,
                BusTxnKind::WriteAddressError,
            ]
        );
        assert!(!tests[0].case.transactions[0].is_transfer());
    }

    /// A wrong magic is an error, not a plausible misread. Every section is
    /// checked, so a container change desynchronizes at the section that moved
    /// rather than producing values that look real.
    #[test]
    fn a_bad_file_magic_is_rejected() {
        let mut bytes = one_case_container();
        bytes[0] ^= 0xFF;
        assert!(decode(&bytes).is_err());
    }

    #[test]
    fn a_bad_section_magic_is_rejected() {
        let good = one_case_container();
        // Corrupt the state magic, which sits past the file and test headers
        // and the three-byte name.
        for offset in 0..good.len() - 4 {
            if good[offset..offset + 4] == MAGIC_STATE.to_le_bytes() {
                let mut bytes = good.clone();
                bytes[offset] ^= 0xFF;
                assert!(decode(&bytes).is_err(), "corrupt state magic must fail");
                return;
            }
        }
        panic!("no state magic found to corrupt");
    }

    /// A truncated container fails rather than returning the cases it managed
    /// to read. A partial decode would shrink the corpus silently, and a gate
    /// that validates fewer vectors than it reports is the defect this crate
    /// exists to avoid.
    #[test]
    fn a_truncated_container_fails_rather_than_returning_a_short_corpus() {
        let good = one_case_container();
        let truncated = &good[..good.len() - 8];
        assert!(decode(truncated).is_err());
    }

    /// And trailing bytes fail too, which is what would happen if a case's
    /// encoding grew a field and this decoder kept reading the old shape.
    #[test]
    fn trailing_bytes_are_rejected() {
        let mut bytes = one_case_container();
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        assert!(decode(&bytes).is_err());
    }

    /// A RAM address outside the 24-bit space means the decode has
    /// desynchronized, since the part cannot address one.
    #[test]
    fn an_out_of_range_ram_address_is_rejected() {
        let mut b = Builder::default();
        b.u32(MAGIC_FILE).u32(1);
        b.section(MAGIC_TEST);
        b.section(MAGIC_NAME).u32(1);
        b.0.extend_from_slice(b"x");
        b.state(0, [0, 0], &[(0x0200_0000, 0)]);
        assert!(decode(&b.0).is_err());
    }
}
