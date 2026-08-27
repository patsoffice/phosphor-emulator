//! What field TLV buys over positional chunk framing, and what it costs.
//!
//! Stage A framed each nested *component*, which contains a component's change
//! to that component. Inside one, field order was still wire order: adding a
//! field, removing one, or reordering two changed the body and invalidated
//! every save containing it.
//!
//! `#[save_tlv]` frames every field under an explicit id, so the reader
//! dispatches on the id instead of on position. These tests model the three
//! changes that used to be wire breaks and check that they are not, plus the
//! failures that must stay loud.

use phosphor_core::core::save_state::{SaveError, StateReader, StateWriter, save_machine};
use phosphor_macros::Saveable;

/// A device, as first written.
#[derive(Saveable, Default, Debug, PartialEq)]
#[save_version(1)]
#[save_tlv]
struct Chip {
    #[save(id = 1)]
    fifo: [u8; 4],
    #[save(id = 2)]
    head: u16,
    #[save(id = 3)]
    talking: bool,
}

/// The same device with its fields declared in a different order. Same ids, so
/// the wire is unchanged: this is what "order immune" means.
#[derive(Saveable, Default, Debug, PartialEq)]
#[save_version(1)]
#[save_tlv]
struct ChipReordered {
    #[save(id = 3)]
    talking: bool,
    #[save(id = 1)]
    fifo: [u8; 4],
    #[save(id = 2)]
    head: u16,
}

/// The same device after gaining a field. The new one is marked `default`, so
/// saves written before it existed still load.
#[derive(Saveable, Default, Debug, PartialEq)]
#[save_version(1)]
#[save_tlv]
struct ChipGrown {
    #[save(id = 1)]
    fifo: [u8; 4],
    #[save(id = 2)]
    head: u16,
    #[save(id = 3)]
    talking: bool,
    #[save(id = 4, default)]
    rate: u8,
}

/// The same device after a field was dropped. Id 2 is retired, never to be
/// reused; the derive asserts that, and the reader skips it by length.
#[derive(Saveable, Default, Debug, PartialEq)]
#[save_version(1)]
#[save_tlv]
#[save_retired(2)]
struct ChipShrunk {
    #[save(id = 1)]
    fifo: [u8; 4],
    #[save(id = 3)]
    talking: bool,
}

fn chip() -> Chip {
    Chip {
        fifo: [0xDE, 0xAD, 0xBE, 0xEF],
        head: 0x1234,
        talking: true,
    }
}

fn bytes_of(v: &impl phosphor_core::prelude::Saveable) -> Vec<u8> {
    let mut w = StateWriter::new();
    v.save_state(&mut w);
    w.into_vec()
}

fn load_into<T: phosphor_core::prelude::Saveable>(v: &mut T, data: &[u8]) -> Result<(), SaveError> {
    let mut r = StateReader::new(data);
    v.load_state(&mut r)?;
    if r.remaining() != 0 {
        return Err(SaveError::InvalidFormat(format!(
            "{} bytes left unread",
            r.remaining()
        )));
    }
    Ok(())
}

// -- The three changes that used to be wire breaks ---------------------------

#[test]
fn a_tlv_struct_round_trips() {
    let mut out = Chip::default();
    load_into(&mut out, &bytes_of(&chip())).unwrap();
    assert_eq!(out, chip());
}

/// Declaration order is no longer wire order. Under Stage A's ordinal tags this
/// swapped `fifo` and `talking` silently, or failed on their body lengths.
#[test]
fn reordering_fields_does_not_change_the_wire() {
    let data = bytes_of(&chip());

    let mut out = ChipReordered::default();
    load_into(&mut out, &data).unwrap();
    assert_eq!(
        out,
        ChipReordered {
            talking: true,
            fifo: [0xDE, 0xAD, 0xBE, 0xEF],
            head: 0x1234,
        }
    );

    // And byte for byte: the writer emits in ascending id order rather than
    // declaration order, so the bytes are a function of the ids alone.
    assert_eq!(bytes_of(&out), data);
}

/// Adding a field with `default` does not invalidate saves written before it
/// existed. The absent id leaves it at its constructed value.
#[test]
fn a_new_optional_field_reads_a_save_that_predates_it() {
    let mut out = ChipGrown {
        rate: 9,
        ..Default::default()
    };
    load_into(&mut out, &bytes_of(&chip())).unwrap();

    assert_eq!(out.fifo, [0xDE, 0xAD, 0xBE, 0xEF]);
    assert_eq!(out.head, 0x1234);
    assert!(out.talking);
    assert_eq!(out.rate, 9, "an absent field keeps what it was built with");
}

/// The other direction: a build that has never heard of id 4 skips it by
/// length and reads everything else correctly.
#[test]
fn an_older_reader_skips_a_field_a_newer_writer_added() {
    let grown = ChipGrown {
        fifo: [1, 2, 3, 4],
        head: 7,
        talking: true,
        rate: 0x5A,
    };

    let mut out = Chip::default();
    load_into(&mut out, &bytes_of(&grown)).unwrap();
    assert_eq!(
        out,
        Chip {
            fifo: [1, 2, 3, 4],
            head: 7,
            talking: true,
        }
    );
}

/// A retired id is skipped like any other unknown one. The attribute's job is
/// the compile-time assertion that nothing reuses it, not anything at load.
#[test]
fn a_retired_id_is_skipped() {
    let mut out = ChipShrunk::default();
    load_into(&mut out, &bytes_of(&chip())).unwrap();
    assert_eq!(
        out,
        ChipShrunk {
            fifo: [0xDE, 0xAD, 0xBE, 0xEF],
            talking: true,
        }
    );
}

// -- Failures that must stay loud --------------------------------------------

/// A field without `default` is required. Absence is not silently tolerated,
/// because a device left at power-on while the rest of the machine is at frame
/// N is exactly the failure chunk framing exists to make loud.
#[test]
fn a_required_field_that_is_absent_fails_and_names_itself() {
    // ChipShrunk has no id 2; Chip requires it.
    let data = bytes_of(&ChipShrunk::default());

    let mut out = Chip::default();
    let err = load_into(&mut out, &data).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("Chip.head"), "{msg}");
    assert!(msg.contains("id 2"), "{msg}");
}

#[test]
fn a_repeated_id_is_rejected() {
    // Hand-build a body with id 2 twice.
    let mut w = StateWriter::new();
    w.write_version(1);
    w.write_u16_le(4);
    w.write_tlv(1, |w| w.write_raw(&[0u8; 4]));
    w.write_tlv(2, |w| w.write_u16_le(1));
    w.write_tlv(2, |w| w.write_u16_le(2));
    w.write_tlv(3, |w| w.write_bool(true));
    let data = w.into_vec();

    let mut out = Chip::default();
    let err = load_into(&mut out, &data).unwrap_err();
    assert!(err.to_string().contains("appears twice"), "{err}");
}

/// TLV absorbs additive change; the version byte is what catches a field
/// changing meaning. Both are needed, so both are checked.
#[test]
fn the_version_byte_still_rejects_a_semantic_change() {
    #[derive(Saveable, Default)]
    #[save_version(2)]
    #[save_tlv]
    struct ChipV2 {
        #[save(id = 1)]
        fifo: [u8; 4],
        #[save(id = 2)]
        head: u16,
        #[save(id = 3)]
        talking: bool,
    }

    let mut out = ChipV2::default();
    let err = load_into(&mut out, &bytes_of(&chip())).unwrap_err();
    assert!(
        err.to_string().contains("component version mismatch"),
        "{err}"
    );
}

/// A field that widened without a version bump keeps its id but not its length.
/// The payload is bounded, so it fails against the field rather than eating the
/// next one.
#[test]
fn a_widened_field_fails_against_its_own_name() {
    #[derive(Saveable, Default)]
    #[save_version(1)]
    #[save_tlv]
    struct ChipWide {
        #[save(id = 1)]
        fifo: [u8; 4],
        #[save(id = 2)]
        head: u32, // was u16
        #[save(id = 3)]
        talking: bool,
    }

    let mut out = ChipWide::default();
    let err = load_into(&mut out, &bytes_of(&chip())).unwrap_err();
    assert!(err.to_string().contains("ChipWide.head"), "{err}");
}

// -- Wire format -------------------------------------------------------------

/// Spelled out, so a framing change nobody meant to make fails here with a byte
/// diff. Note `fifo` carries no inner length: under TLV the field length is the
/// length, and repeating it is what the positional encoding had to do.
#[test]
fn the_tlv_body_is_a_pinned_byte_sequence() {
    #[rustfmt::skip]
    let expected: &[u8] = &[
        0x01,                       // #[save_version(1)]
        0x03, 0x00,                 // field count: what makes the body
                                    //   self-delimiting inside an unframed parent
        0x01, 0x00,                 // id 1: fifo
        0x04, 0x00, 0x00, 0x00,     //   length
        0xDE, 0xAD, 0xBE, 0xEF,     //   raw bytes, no inner u32 length
        0x02, 0x00,                 // id 2: head
        0x02, 0x00, 0x00, 0x00,     //   length
        0x34, 0x12,                 //   little endian
        0x03, 0x00,                 // id 3: talking
        0x01, 0x00, 0x00, 0x00,     //   length
        0x01,                       //   bool as u8
    ];
    assert_eq!(bytes_of(&chip()), expected);
}

// -- Interop with positional structs -----------------------------------------

#[derive(Saveable, Default, Debug, PartialEq)]
#[save_version(1)]
struct Positional {
    level: u8,
}

/// A positional struct nested in a TLV parent is framed by its `#[save(id)]`;
/// a TLV struct nested in a positional parent is framed by the parent's ordinal
/// tag. Both directions have to work, or "opt-in" would mean "all or nothing".
#[test]
fn tlv_and_positional_structs_nest_in_either_direction() {
    #[derive(Saveable, Default, Debug, PartialEq)]
    #[save_version(1)]
    #[save_tlv]
    struct TlvParent {
        #[save(id = 1)]
        chip: Chip,
        #[save(id = 2)]
        dac: Positional,
    }

    #[derive(Saveable, Default, Debug, PartialEq)]
    struct PositionalParent {
        chip: Chip,
        dac: Positional,
    }

    let tlv = TlvParent {
        chip: chip(),
        dac: Positional { level: 0x7F },
    };
    let mut out = TlvParent::default();
    load_into(&mut out, &bytes_of(&tlv)).unwrap();
    assert_eq!(out, tlv);

    let positional = PositionalParent {
        chip: chip(),
        dac: Positional { level: 0x7F },
    };
    let mut out = PositionalParent::default();
    load_into(&mut out, &bytes_of(&positional)).unwrap();
    assert_eq!(out, positional);
}

/// **The bug the field count exists to prevent.**
///
/// 49 `Saveable` impls are hand-written and frame nothing: they call a child's
/// `save_state` straight into their own stream. A TLV child there is handed a
/// reader that runs to the end of the *parent's* bytes, so without the count
/// its dispatch loop keeps going and eats whatever the parent wrote next.
///
/// This is exactly how Gridlee broke when `M6809` was migrated: its
/// hand-written impl writes the CPU and then four memory regions, and the CPU's
/// loop read on into the RAM blob. Reproduced here in miniature so the fix has
/// a guard that does not need ROMs.
#[test]
fn a_tlv_struct_stops_at_its_own_end_inside_a_parent_that_frames_nothing() {
    struct Unframing {
        chip: Chip,
        trailing: Vec<u8>,
    }

    // Deliberately hand-written, and deliberately not framing `chip`.
    impl phosphor_core::prelude::Saveable for Unframing {
        fn save_state(&self, w: &mut StateWriter) {
            self.chip.save_state(w);
            w.write_bytes(&self.trailing);
        }
        fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
            self.chip.load_state(r)?;
            self.trailing = r.read_bytes()?.to_vec();
            Ok(())
        }
    }

    let original = Unframing {
        chip: chip(),
        // Bytes that would parse as plausible TLV headers if the chip's loop
        // ran on into them, so the test fails by corruption and not only by
        // the reader happening to hit something malformed.
        trailing: vec![0x04, 0x00, 0x01, 0x00, 0x00, 0x00, 0x99, 0x77],
    };

    let mut out = Unframing {
        chip: Chip::default(),
        trailing: Vec::new(),
    };
    load_into(&mut out, &bytes_of(&original)).unwrap();

    assert_eq!(out.chip, chip());
    assert_eq!(out.trailing, original.trailing);
}

/// A count larger than the body is a malformed file, not a silent short read.
#[test]
fn a_count_that_overruns_the_body_is_rejected() {
    let mut w = StateWriter::new();
    w.write_version(1);
    w.write_u16_le(9); // claims nine fields
    w.write_tlv(1, |w| w.write_raw(&[0u8; 4]));
    let data = w.into_vec();

    let mut out = Chip::default();
    let err = load_into(&mut out, &data).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("declares 9 fields"), "{msg}");
    assert!(msg.contains("ran out after 1"), "{msg}");
}

// -- Types the derive can encode ---------------------------------------------

/// An expanded palette is `[(u8, u8, u8); N]`, and boards hold several. They
/// were unsupported, which is why every one of them was either `#[save_skip]`
/// with a rebuild call bolted onto `load_state`, or hand-written.
#[test]
fn an_array_of_primitive_tuples_round_trips() {
    #[derive(Saveable, Default, Debug, PartialEq)]
    #[save_version(1)]
    #[save_tlv]
    struct Video {
        #[save(id = 1)]
        palette_rgb: [(u8, u8, u8); 3],
        #[save(id = 2)]
        mixed: [(u16, bool); 2],
    }

    let src = Video {
        palette_rgb: [(1, 2, 3), (0xAA, 0xBB, 0xCC), (0xFF, 0, 0x7F)],
        mixed: [(0x1234, true), (0x5678, false)],
    };

    let mut out = Video::default();
    load_into(&mut out, &bytes_of(&src)).unwrap();
    assert_eq!(out, src);
}

/// Tuple elements are written in order with no framing of their own: the array
/// length and the tuple arity are both fixed by the type, so nothing else has to
/// carry them.
#[test]
fn a_tuple_array_is_written_flat() {
    #[derive(Saveable, Default)]
    #[save_version(1)]
    #[save_tlv]
    struct Palette {
        #[save(id = 1)]
        rgb: [(u8, u8, u8); 2],
    }

    let data = bytes_of(&Palette {
        rgb: [(1, 2, 3), (4, 5, 6)],
    });
    #[rustfmt::skip]
    let expected: &[u8] = &[
        0x01,                   // version
        0x01, 0x00,             // one field
        0x01, 0x00,             // id 1
        0x06, 0x00, 0x00, 0x00, // six bytes: two tuples of three
        1, 2, 3, 4, 5, 6,
    ];
    assert_eq!(data, expected);
}

/// A register file indexed by chip and then by channel is `[[u8; 3]; 2]`, and a
/// sound board holds several. Like a tuple array it is written flat, because
/// both dimensions are fixed by the type.
#[test]
fn an_array_of_arrays_round_trips_flat() {
    #[derive(Saveable, Default, Debug, PartialEq)]
    #[save_version(1)]
    #[save_tlv]
    struct Ssio {
        #[save(id = 1)]
        duty_cycle: [[u8; 3]; 2],
        #[save(id = 2)]
        deep: [[[u16; 2]; 1]; 2],
    }

    let src = Ssio {
        duty_cycle: [[1, 2, 3], [4, 5, 6]],
        deep: [[[0x1234, 0x5678]], [[0x9ABC, 0xDEF0]]],
    };

    let mut out = Ssio::default();
    load_into(&mut out, &bytes_of(&src)).unwrap();
    assert_eq!(out, src);

    let data = bytes_of(&Ssio {
        duty_cycle: [[1, 2, 3], [4, 5, 6]],
        ..Default::default()
    });
    assert_eq!(
        &data[9..15],
        &[1, 2, 3, 4, 5, 6],
        "the outer array's elements follow one another with no framing"
    );
}

/// A CPU's interrupt phase, a 68000's family member and a Slapstic's sequence
/// step are all fieldless enums with an explicit byte mapping, and each was a
/// hand-written impl for that reason alone.
#[test]
fn a_fieldless_enum_round_trips_on_its_discriminant() {
    #[derive(Saveable, Debug, PartialEq)]
    #[repr(u8)]
    enum Interrupt {
        None = 0,
        Nmi = 1,
        Irq = 2,
    }

    /// No `#[repr]` and no discriminants: Rust counts from zero, and so does
    /// the wire.
    #[derive(Saveable, Debug, PartialEq)]
    enum Step {
        Idle,
        Active,
        Commit,
    }

    for (src, byte) in [
        (Interrupt::None, 0u8),
        (Interrupt::Nmi, 1),
        (Interrupt::Irq, 2),
    ] {
        let data = bytes_of(&src);
        assert_eq!(data, vec![byte]);
        let mut out = Interrupt::None;
        load_into(&mut out, &data).unwrap();
        assert_eq!(out, src);
    }

    assert_eq!(bytes_of(&Step::Commit), vec![2u8]);
    let mut out = Step::Idle;
    load_into(&mut out, &[1]).unwrap();
    assert_eq!(out, Step::Active);
}

/// The hand-written impls all ended in a `_ =>` arm that turned an unrecognised
/// byte into variant zero, so a corrupt save resumed as a plausible one. The
/// derive names the type instead.
#[test]
fn an_unknown_discriminant_fails_and_names_its_type() {
    #[derive(Saveable, Debug, PartialEq)]
    #[repr(u8)]
    enum Variant {
        M68000 = 0,
        M68010 = 1,
    }

    let mut out = Variant::M68000;
    let err = load_into(&mut out, &[7]).unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("Variant"), "{msg}");
    assert!(msg.contains('7'), "{msg}");
    assert_eq!(out, Variant::M68000, "the value is left alone");
}

/// An enum nested in a TLV struct is a field like any other: framed under its
/// id, with the byte as the whole payload.
#[test]
fn an_enum_nests_in_a_tlv_struct() {
    #[derive(Saveable, Debug, PartialEq, Default)]
    #[repr(u8)]
    enum Phase {
        #[default]
        Idle = 0,
        Running = 3,
    }

    #[derive(Saveable, Debug, PartialEq, Default)]
    #[save_version(1)]
    #[save_tlv]
    struct Cpu {
        #[save(id = 1)]
        pc: u16,
        #[save(id = 2)]
        phase: Phase,
    }

    let src = Cpu {
        pc: 0x1234,
        phase: Phase::Running,
    };
    let mut out = Cpu::default();
    load_into(&mut out, &bytes_of(&src)).unwrap();
    assert_eq!(out, src);
}

// -- The post-load hook ------------------------------------------------------

/// `#[save_after_load]` runs its methods in order, after the body and after
/// every `#[save_skip(default…)]` has been applied, so a hook sees the finished
/// state rather than a half-restored one.
#[test]
fn after_load_hooks_run_in_order_on_the_finished_state() {
    #[derive(Saveable, Default, Debug, PartialEq)]
    #[save_version(1)]
    #[save_tlv]
    #[save_after_load(clamp, note)]
    struct Chip {
        #[save(id = 1)]
        bank: u8,
        #[save_skip(default = 9)]
        scratch: u8,
        #[save_skip]
        trace: Vec<u8>,
    }

    impl Chip {
        fn clamp(&mut self) {
            self.bank &= 3;
            self.trace.push(1);
        }
        fn note(&mut self) {
            // Reads `scratch`, so this fails unless the skip default ran first.
            self.trace.push(self.scratch);
        }
    }

    // A byte no writer of this struct can emit, which is the case the hook is
    // for: a save is an input.
    let data: &[u8] = &[
        0x01, // version
        0x01, 0x00, // one field
        0x01, 0x00, // id 1
        0x01, 0x00, 0x00, 0x00, // one byte
        0xFF,
    ];

    let mut out = Chip::default();
    load_into(&mut out, data).unwrap();
    assert_eq!(out.bank, 3, "clamp ran");
    assert_eq!(out.trace, vec![1, 9], "in order, after the skip defaults");
}

/// A machine whose components are a mix loads end to end through the real
/// envelope, checksum included.
#[test]
fn a_mixed_machine_round_trips_through_the_envelope() {
    #[derive(Saveable, Default, Debug, PartialEq)]
    struct Machine {
        chip: Chip,
        dac: Positional,
        clock: u64,
    }

    let machine = Machine {
        chip: chip(),
        dac: Positional { level: 3 },
        clock: 0x0102_0304_0506_0708,
    };
    let data = save_machine(&machine, "test");

    let mut out = Machine::default();
    phosphor_core::core::save_state::load_machine(&mut out, "test", &data).unwrap();
    assert_eq!(out, machine);
}
