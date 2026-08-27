//! An address space saves the bytes the CPU can change, and nothing else.
//!
//! Before this, 22 of the 49 hand-written `Saveable` impls existed largely to
//! enumerate their board's memory two lines a region:
//!
//! ```ignore
//! w.write_bytes(self.board.map.region_data(Region::Ram));
//! w.write_bytes(self.board.map.region_data(Region::VideoRam));
//! ```
//!
//! A board that left a region off that list was silently wrong, and nothing
//! could catch it. Deriving the set from each region's `AccessKind` makes it a
//! property of the map instead of a list someone maintains, so these tests are
//! mostly about that rule holding at its edges: ROM out, I/O out, banked and
//! mirrored windows counted once.

use phosphor_core::core::address_space::AccessKind;
use phosphor_core::core::save_state::{SaveError, StateReader, StateWriter};
use phosphor_core::core::watchpoint::WatchpointKind;
use phosphor_core::core::{AddressSpace16, AddressSpace32};

const RAM: u8 = 1;
const VRAM: u8 = 2;
const ROM: u8 = 3;
const IO: u8 = 4;
const PALETTE: u8 = 5;

/// A board-shaped map: RAM, video RAM, a write-only palette, ROM and an I/O
/// window. Only the first three are state.
fn space() -> AddressSpace16 {
    let mut s = AddressSpace16::new();
    s.region(RAM, "RAM", 0x0000, 0x0400, AccessKind::ReadWrite)
        .region(VRAM, "Video RAM", 0x0400, 0x0400, AccessKind::ReadWrite)
        .region(PALETTE, "Palette", 0x0800, 0x0100, AccessKind::WriteOnly)
        .region(ROM, "Program ROM", 0x8000, 0x0400, AccessKind::ReadOnly)
        .region(IO, "I/O", 0x0900, 0x0100, AccessKind::Io);
    s
}

fn filled() -> AddressSpace16 {
    let mut s = space();
    s.region_data_mut(RAM)[0] = 0xDE;
    s.region_data_mut(RAM)[0x3FF] = 0xAD;
    s.region_data_mut(VRAM)[0] = 0xBE;
    s.region_data_mut(PALETTE)[0] = 0xEF;
    s.region_data_mut(ROM)[0] = 0x11;
    s
}

fn bytes_of(s: &impl phosphor_core::prelude::Saveable) -> Vec<u8> {
    let mut w = StateWriter::new();
    s.save_state(&mut w);
    w.into_vec()
}

fn load_into(s: &mut impl phosphor_core::prelude::Saveable, data: &[u8]) -> Result<(), SaveError> {
    let mut r = StateReader::new(data);
    s.load_state(&mut r)?;
    if r.remaining() != 0 {
        return Err(SaveError::InvalidFormat(format!(
            "{} bytes left unread",
            r.remaining()
        )));
    }
    Ok(())
}

// -- The rule ----------------------------------------------------------------

#[test]
fn writable_regions_round_trip() {
    let mut out = space();
    load_into(&mut out, &bytes_of(&filled())).unwrap();

    assert_eq!(out.region_data(RAM)[0], 0xDE);
    assert_eq!(out.region_data(RAM)[0x3FF], 0xAD);
    assert_eq!(out.region_data(VRAM)[0], 0xBE);
    assert_eq!(out.region_data(PALETTE)[0], 0xEF);
}

/// ROM is `ReadOnly`, so it is excluded by construction rather than by anyone
/// remembering.
///
/// A plain round trip cannot see this, since a restored ROM equals the ROM
/// already there. So the save is taken with one ROM, the load done into a
/// different one: if ROM bytes travelled, the second would be overwritten.
#[test]
fn rom_is_not_saved() {
    let data = bytes_of(&filled()); // ROM[0] == 0x11 here

    let mut out = space();
    out.region_data_mut(ROM)[0] = 0x22;
    load_into(&mut out, &data).unwrap();

    assert_eq!(out.region_data(ROM)[0], 0x22, "ROM bytes must not travel");
    assert_eq!(out.region_data(RAM)[0], 0xDE, "RAM bytes must");

    // And the size accounts for exactly the writable regions plus the page
    // table: version + count, then six bytes of framing per entry.
    let regions: usize = [RAM, VRAM, PALETTE]
        .iter()
        .map(|&id| space().region_data(id).len() + 6)
        .sum();
    assert_eq!(data.len(), 1 + 2 + regions + (PAGE_TABLE_BYTES + 6));
}

/// 256 pages, each a region id and a `u32` offset.
const PAGE_TABLE_BYTES: usize = 256 * 5;

/// Entries in a saved body: the writable regions, plus the page table.
fn entry_count(data: &[u8]) -> u16 {
    u16::from_le_bytes([data[1], data[2]])
}

/// An I/O window has no bytes at all, so it cannot be saved even though a board
/// might list it. The entry count is the check: three regions and the page
/// table, not four regions.
#[test]
fn io_regions_are_not_saved() {
    assert_eq!(entry_count(&bytes_of(&filled())), 4);
}

/// A region added to the map since the save was written is *missing*, not
/// optional: leaving it at power-on while the rest of the machine is at frame N
/// is the failure this format exists to make loud.
#[test]
fn a_region_the_map_has_but_the_file_lacks_is_an_error() {
    let data = bytes_of(&filled());

    let mut grown = space();
    grown.region(6, "Extra RAM", 0x0A00, 0x0100, AccessKind::ReadWrite);
    let err = load_into(&mut grown, &data).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("Extra RAM"), "{msg}");
    assert!(msg.contains("absent"), "{msg}");
}

/// The other direction, and a **narrowing** that came with the page table.
///
/// An unknown region's *bytes* are still skipped by length. But if that region
/// is mapped into the address space, the page table names it, and a page naming
/// a region this map does not have is refused: mapping it would send every read
/// through it somewhere arbitrary.
///
/// So a map that differs structurally is now rejected outright rather than
/// partially applied. That is stricter than region-skipping alone implied, and
/// it is the right way round: two maps that disagree about what is mapped where
/// are not the same machine. Machines that legitimately differ this way (a board
/// variant with extra RAM) already differ by `machine_id`, which the envelope
/// checks first.
#[test]
fn a_map_that_maps_a_region_this_one_lacks_is_rejected() {
    let mut extra = filled();
    extra.region(6, "Extra RAM", 0x0A00, 0x0100, AccessKind::ReadWrite);
    extra.region_data_mut(6)[0] = 0x99;
    let data = bytes_of(&extra);

    let mut plain = space();
    let err = load_into(&mut plain, &data).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("page 0x0A"), "{msg}");
    assert!(msg.contains("region 6"), "{msg}");
}

/// A region that changed size is caught against its own name rather than
/// silently truncating or running into the next region.
#[test]
fn a_region_that_changed_size_fails_against_its_own_name() {
    let data = bytes_of(&filled());

    let mut resized = AddressSpace16::new();
    resized
        .region(RAM, "RAM", 0x0000, 0x0800, AccessKind::ReadWrite) // was 0x400
        .region(VRAM, "Video RAM", 0x0400, 0x0400, AccessKind::ReadWrite)
        .region(PALETTE, "Palette", 0x0800, 0x0100, AccessKind::WriteOnly);
    let err = load_into(&mut resized, &data).unwrap_err();
    assert!(err.to_string().contains("RAM"), "{err}");
}

// -- Self-delimiting ---------------------------------------------------------

/// The count matters for the same reason it does for a `#[save_tlv]` body: 49
/// hand-written impls frame nothing, so a map inside one is handed a reader
/// that runs to the end of its parent's bytes.
#[test]
fn a_map_stops_at_its_own_end_inside_a_parent_that_frames_nothing() {
    struct Unframing {
        map: AddressSpace16,
        trailing: Vec<u8>,
    }

    impl phosphor_core::prelude::Saveable for Unframing {
        fn save_state(&self, w: &mut StateWriter) {
            self.map.save_state(w);
            w.write_bytes(&self.trailing);
        }
        fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
            self.map.load_state(r)?;
            self.trailing = r.read_bytes()?.to_vec();
            Ok(())
        }
    }

    let original = Unframing {
        map: filled(),
        // Bytes that would read as a plausible region header if the map's loop
        // ran on past its own end.
        trailing: vec![0x01, 0x00, 0x04, 0x00, 0x00, 0x00, 0x77, 0x88],
    };
    let mut out = Unframing {
        map: space(),
        trailing: Vec::new(),
    };
    load_into(&mut out, &bytes_of(&original)).unwrap();

    assert_eq!(out.map.region_data(RAM)[0], 0xDE);
    assert_eq!(out.trailing, original.trailing);
}

// -- Banking and mirrors -----------------------------------------------------

/// A bank window is a second view of bytes already counted. Saving it twice
/// would duplicate a region id, so the set is keyed on the backing region.
#[test]
fn a_remapped_window_does_not_double_count_its_region() {
    let mut banked = filled();
    banked.remap_pages(0x00, 0x04, VRAM, 0);

    let data = bytes_of(&banked);
    assert_eq!(
        entry_count(&data),
        4,
        "remapping pages must not change which regions are saved"
    );

    let mut out = space();
    load_into(&mut out, &data).unwrap();
    assert_eq!(out.region_data(VRAM)[0], 0xBE);
}

// -- Banking -----------------------------------------------------------------

/// **Where a window currently points is state.**
///
/// `remap_pages` is how a board banks, and the page table is the only record of
/// where it points. Boards used to rebuild it from their own bank register in
/// `load_state`, which is a call each of them had to remember to make and which
/// runs nowhere else on the normal path. A board that forgot came back with the
/// CPU reading through pages aimed where they were aimed before the load.
#[test]
fn banking_survives_a_round_trip() {
    let mut banked = filled();
    // Point the bottom four pages at the second 1 KB of video RAM.
    banked.remap_pages(0x00, 0x04, VRAM, 0x100);
    banked.region_data_mut(VRAM)[0x100] = 0x5A;

    let mut out = space();
    // The destination is unbanked, so a load that did not carry the page table
    // would leave page 0 pointing at RAM.
    assert_eq!(out.region_at(0x0000).map(|r| r.id), Some(RAM));

    load_into(&mut out, &bytes_of(&banked)).unwrap();

    assert_eq!(
        out.region_at(0x0000).map(|r| r.id),
        Some(VRAM),
        "the bank window must come back pointing where it pointed"
    );
    assert_eq!(
        out.debug_read(0x0000),
        Some(0x5A),
        "and at the right offset within it"
    );
}

/// A page pointing at a region this map does not have would send every read
/// through it somewhere arbitrary, so it is refused rather than mapped.
#[test]
fn a_page_naming_an_unknown_region_is_rejected() {
    let mut extra = filled();
    extra.region(9, "Foreign", 0x0B00, 0x0100, AccessKind::ReadWrite);
    extra.remap_pages(0x00, 0x01, 9, 0);
    let data = bytes_of(&extra);

    let mut plain = space();
    let err = load_into(&mut plain, &data).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("page 0x00"), "{msg}");
    assert!(msg.contains("does not have"), "{msg}");
}

/// Watchpoints belong to whoever is debugging, not to the machine, so a load
/// leaves them where they were set rather than restoring the saved machine's.
#[test]
fn a_load_does_not_disturb_watchpoints() {
    let data = bytes_of(&filled());

    let mut out = space();
    out.set_watchpoint(0, 0x0010, WatchpointKind::Write);
    load_into(&mut out, &data).unwrap();

    assert!(out.has_any_watchpoints());
}

// -- 32-bit sibling ----------------------------------------------------------

#[test]
fn the_32_bit_space_follows_the_same_rule() {
    let build = || {
        let mut s = AddressSpace32::new();
        s.region(RAM, "RAM", 0x0000, 0x1000, AccessKind::ReadWrite)
            .region(ROM, "Program ROM", 0x10_0000, 0x1000, AccessKind::ReadOnly);
        s
    };

    let mut src = build();
    src.region_data_mut(RAM)[0] = 0x5A;
    src.region_data_mut(ROM)[0] = 0x11;
    let data = bytes_of(&src);

    // One region, not two.
    assert_eq!(u16::from_le_bytes([data[1], data[2]]), 1);

    let mut out = build();
    load_into(&mut out, &data).unwrap();
    assert_eq!(out.region_data(RAM)[0], 0x5A);
}
