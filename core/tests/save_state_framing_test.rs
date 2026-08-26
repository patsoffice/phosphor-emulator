//! What chunk framing buys, tested against the failures it exists to prevent.
//!
//! The save format was positional: a machine's body was one flat concatenation
//! of every component's bytes, with no boundary between them. Two consequences
//! drove `phosphor-emulator-tlv-save-state-hc61`:
//!
//! * A component whose body changed misread its own bytes and then kept going
//!   into its *sibling's*, so the damage surfaced as a wrong value somewhere
//!   else entirely, or as nothing at all.
//! * The only way to reject such a file was the global envelope version, which
//!   invalidated every save for every machine, including machines that contain
//!   nothing that changed.
//!
//! These tests model a component rewrite as three definitions of the same
//! device and check where the failure lands. Each of them passes vacuously
//! without framing except by silently corrupting the sibling, which is the
//! point.

use phosphor_core::core::save_state::{
    self, MIN_SUPPORTED_SAVE_VERSION, SAVE_VERSION, SaveError, load_machine, save_machine,
};
use phosphor_core::prelude::Saveable as _;
use phosphor_macros::Saveable;

// Components that do not change across the rewrite.

#[derive(Saveable, Default)]
#[save_version(1)]
struct Cpu {
    pc: u16,
}

#[derive(Saveable, Default)]
#[save_version(1)]
struct Dac {
    level: u8,
}

// The device being rewritten, in three states.

/// Before the rewrite.
#[derive(Saveable, Default)]
#[save_version(1)]
struct AvgV1 {
    pc: u16,
}

/// After the rewrite, with the version bump forgotten. This is the dangerous
/// one: nothing about the bytes says the layout moved.
#[derive(Saveable, Default)]
#[save_version(1)]
struct AvgGrown {
    pc: u16,
    state: u8,
}

/// After the rewrite, done properly.
#[derive(Saveable, Default)]
#[save_version(2)]
struct AvgBumped {
    pc: u16,
    state: u8,
}

// A machine that contains the device, with a sibling *after* it so that a
// component reading past its own end has something to corrupt.

#[derive(Saveable, Default)]
struct VectorMachineV1 {
    cpu: Cpu,
    avg: AvgV1,
    dac: Dac,
}

#[derive(Saveable, Default)]
struct VectorMachineGrown {
    cpu: Cpu,
    avg: AvgGrown,
    dac: Dac,
}

#[derive(Saveable, Default)]
struct VectorMachineBumped {
    cpu: Cpu,
    avg: AvgBumped,
    dac: Dac,
}

/// A machine that contains no AVG at all. Its saves are what the historical
/// bumps destroyed for no reason.
#[derive(Saveable, Default)]
struct RasterMachine {
    cpu: Cpu,
    ram: [u8; 4],
    dac: Dac,
}

fn vector_v1() -> VectorMachineV1 {
    VectorMachineV1 {
        cpu: Cpu { pc: 0x1234 },
        avg: AvgV1 { pc: 0x5678 },
        dac: Dac { level: 0x7F },
    }
}

// -- The rewrite lands on the rewritten component ----------------------------

/// A component that grew a field stops at its own chunk boundary. Unframed it
/// would read the first byte of the DAC's chunk as its new field, leave the
/// stream one byte short, and hand back a machine whose DAC level is garbage
/// with no error at all.
#[test]
fn a_component_that_grew_fails_against_its_own_name() {
    let data = save_machine(&vector_v1(), "vector");

    let mut out = VectorMachineGrown::default();
    let err = load_machine(&mut out, "vector", &data).unwrap_err();

    let msg = err.to_string();
    assert!(msg.contains("VectorMachineGrown.avg"), "{msg}");
    // The components read before it are intact: the failure is localised, not
    // just detected.
    assert_eq!(out.cpu.pc, 0x1234);
}

/// The mirror image: a component that lost a field leaves bytes unread inside
/// its own chunk. Unframed those bytes would become the DAC's version tag.
#[test]
fn a_component_that_shrank_fails_against_its_own_name() {
    let grown = VectorMachineGrown {
        cpu: Cpu { pc: 0x1234 },
        avg: AvgGrown {
            pc: 0x5678,
            state: 3,
        },
        dac: Dac { level: 0x7F },
    };
    let data = save_machine(&grown, "vector");

    let mut out = VectorMachineV1::default();
    let err = load_machine(&mut out, "vector", &data).unwrap_err();

    let msg = err.to_string();
    assert!(msg.contains("VectorMachineV1.avg"), "{msg}");
    assert!(msg.contains("left unread"), "{msg}");
    assert_eq!(out.cpu.pc, 0x1234);
}

/// With the version bumped, the reader says which component is too old rather
/// than which byte offset disagreed.
#[test]
fn a_component_version_bump_names_the_component() {
    let data = save_machine(&vector_v1(), "vector");

    let mut out = VectorMachineBumped::default();
    let err = load_machine(&mut out, "vector", &data).unwrap_err();

    assert_eq!(
        err.to_string(),
        "VectorMachineBumped.avg: invalid format: \
         component version mismatch: expected 2, found 1"
    );
}

/// Inserting a component shifts every ordinal after it, so the machine wants
/// more chunks than the file holds and the last one runs out. Detected however
/// the bodies happen to line up.
#[test]
fn a_component_inserted_in_the_middle_is_detected() {
    #[derive(Saveable, Default)]
    struct WithPia {
        cpu: Cpu,
        pia: Dac,
        avg: AvgV1,
        dac: Dac,
    }

    let data = save_machine(&vector_v1(), "vector");
    let mut out = WithPia::default();
    let err = load_machine(&mut out, "vector", &data).unwrap_err();

    let msg = err.to_string();
    assert!(msg.contains("WithPia."), "{msg}");
}

/// **The limit of ordinal tags, stated out loud.**
///
/// Swapping two components renumbers both, so the tags still match and only
/// their bodies disagree. Where the bodies are identical, as a `Cpu` and an
/// `AvgV1` are here (both a version byte and a `u16`), the file loads with the
/// two components' state exchanged, and nothing says so.
///
/// Stage A does not claim order immunity: field order is still wire order, and
/// reordering components is a wire change that needs the parent's
/// `#[save_version]` bumped. Explicit stable `#[save(id = N)]` ids are what
/// close this, in Stage B (`phosphor-emulator-tlv-save-state-hc61.3`). When
/// they land, this test should start failing and be replaced by its opposite.
#[test]
fn swapping_two_identically_shaped_components_is_not_detected() {
    #[derive(Saveable, Default)]
    struct Swapped {
        avg: AvgV1,
        cpu: Cpu,
        dac: Dac,
    }

    let data = save_machine(&vector_v1(), "vector");
    let mut out = Swapped::default();
    load_machine(&mut out, "vector", &data).unwrap();

    // The CPU's program counter came back as the AVG's, and vice versa.
    assert_eq!(out.avg.pc, 0x1234);
    assert_eq!(out.cpu.pc, 0x5678);
}

// -- Containment -------------------------------------------------------------

/// The envelope version is what invalidates *every* machine's saves.
///
/// Versions 1 through 12 were each bumped for a component change because the
/// flat body left no other way to reject a moved layout; two of the first four
/// and every bump from 6 on was a single subsystem. Version 13 is the last of
/// those. A component change now bumps its own `#[save_version]` and leaves
/// this alone, so only machines containing it lose their saves.
///
/// If this test is in your way, the question to answer is whether the *file
/// envelope* changed. If only a component did, the fix is in that component.
#[test]
fn the_envelope_version_is_pinned() {
    assert_eq!(SAVE_VERSION, 13);
    assert_eq!(MIN_SUPPORTED_SAVE_VERSION, 13);
}

/// The wire format, spelled out. A framing change that nobody meant to make
/// fails here with a byte diff rather than in a user's quicksave.
#[test]
fn the_body_of_a_machine_is_a_pinned_byte_sequence() {
    let machine = RasterMachine {
        cpu: Cpu { pc: 0x1234 },
        ram: [0xAA, 0xBB, 0xCC, 0xDD],
        dac: Dac { level: 0x7F },
    };
    let file = save_machine(&machine, "raster");

    // header: magic | file_version | id_len | id, trailer: crc32.
    let header_len = 4 + 4 + 4 + "raster".len();
    let body = &file[header_len..file.len() - 4];

    #[rustfmt::skip]
    let expected: &[u8] = &[
        0x01, 0x00,                 // chunk tag 1: RasterMachine.cpu
        0x03, 0x00, 0x00, 0x00,     //   payload length
        0x01,                       //   Cpu #[save_version(1)]
        0x34, 0x12,                 //   pc, little endian
        0x04, 0x00, 0x00, 0x00,     // ram: inline, u32 length
        0xAA, 0xBB, 0xCC, 0xDD,     //   bytes
        0x02, 0x00,                 // chunk tag 2: RasterMachine.dac
        0x02, 0x00, 0x00, 0x00,     //   payload length
        0x01,                       //   Dac #[save_version(1)]
        0x7F,                       //   level
    ];
    assert_eq!(body, expected);

    // Scalars stay inline; only nested components are framed. Two components
    // at six bytes each is the whole cost of containment for this machine.
    assert_eq!(body.len(), 13 + 2 * 6);
}

#[test]
fn a_machine_round_trips_through_the_whole_envelope() {
    let data = save_machine(&vector_v1(), "vector");
    let mut out = VectorMachineV1::default();
    load_machine(&mut out, "vector", &data).unwrap();
    assert_eq!(
        (out.cpu.pc, out.avg.pc, out.dac.level),
        (0x1234, 0x5678, 0x7F)
    );
}

// -- Arrays of components ----------------------------------------------------

/// An array of components is one chunk, not one per element: the elements are
/// a fixed count and share a fate, so framing each would be overhead with no
/// containment to show for it.
#[test]
fn an_array_of_components_is_framed_once() {
    #[derive(Saveable, Default)]
    struct Quad {
        pokey: [Dac; 4],
    }

    let mut w = save_state::StateWriter::new();
    Quad::default().save_state(&mut w);
    let data = w.into_vec();

    let mut r = save_state::StateReader::new(&data);
    assert_eq!(r.read_tag_len().unwrap(), Some((1, 4 * 2)));
    assert_eq!(r.remaining(), 8);
}

// -- Corruption --------------------------------------------------------------

#[test]
fn a_truncated_file_is_rejected_rather_than_half_loaded() {
    let mut data = save_machine(&vector_v1(), "vector");
    data.truncate(data.len() - 6);

    let mut out = VectorMachineV1::default();
    let err = load_machine(&mut out, "vector", &data).unwrap_err();
    assert!(
        matches!(err, SaveError::InvalidFormat(_) | SaveError::UnexpectedEnd),
        "{err}"
    );
}
