//! What the conformance-ROM harnesses share, derived from three boards rather
//! than guessed from one.
//!
//! Williams, Road Runner and Toobin' each drive a synthetic program ROM on a
//! machine with no arcade ROMs. `phosphor-emulator-conformance-rom-programme-hl4t.2`
//! predicted four shared parts and asked for the list to be checked against a
//! real second board. Checked against two more, two of the four are here and two
//! are not, and the two that are not are the interesting half:
//!
//! - **The loader generalizes**, exactly as predicted, and is [`load_and_reset`].
//!   Poking an image through `BusDebug::write` and resetting works because
//!   `debug_write` ignores `AccessKind` so a backed `ReadOnly` region takes the
//!   write, and because the CPU fetches its reset vector through the bus. Both
//!   are core properties: the same eight lines serve an M6809 on
//!   `AddressSpace16` and a 68000 on `AddressSpace32`.
//! - **The drift guard generalizes**, and is much the largest of the four at
//!   about ninety lines a board. It is [`assert_binary_matches_source`].
//! - **The result block's wedge guard does NOT generalize past its last two
//!   lines.** This is the prediction the issue got wrong, and it had singled the
//!   wedge guard out as the part most worth standardizing. Williams asserts a
//!   magic byte and a final phase in ten lines. Road Runner's runs to
//!   forty-five, because most of it diagnoses *why* a run wedged: a 68010
//!   exception frame's vector offset, a bounded wait that gave up and which
//!   stage it gave up in, an IRQ3 that fired more times than a one-scanline
//!   pulse can. None of that exists on a 6809 board with a PIA. Extracting the
//!   two lines that are common would cost each board the diagnosis that makes
//!   its failures readable, so the guard stays per board and this note stands in
//!   for it.
//! - **The phase counter does not generalize either**, for a duller reason: the
//!   loop shape is common but the counter is a byte on Williams and a word on
//!   the two 68000 boards, and each board watches different extra state as it
//!   goes. What is left after the differences is a `for` loop over frames.
//!
//! The synchronization primitive, the ROM's storage layout and every expectation
//! were predicted not to generalize, and do not. Nothing here should grow to
//! cover them.

use phosphor_core::core::machine::FrontendMachine;

/// Poke a conformance image into a bare machine and reset it.
///
/// `load_addr` is the link address, and the image is written flat from there
/// through the debug bus. The reset afterwards is what makes the program start:
/// every CPU this is used with fetches its reset vector through the bus, so it
/// picks up the vectors the image just installed rather than anything the board
/// supplies.
pub fn load_and_reset(m: &mut dyn FrontendMachine, machine: &str, image: &[u8], load_addr: u32) {
    {
        let bus = m
            .debug_bus_mut()
            .unwrap_or_else(|| panic!("{machine} exposes no debug bus"));
        for (i, b) in image.iter().enumerate() {
            bus.write(0, load_addr + i as u32, *b);
        }
    }
    m.reset();
}

/// One committed image built from a conformance ROM's source.
///
/// A board contributes more than one when the same source links at more than one
/// address, which Williams does: a source edit that only breaks one of them is
/// exactly what a single-image guard would miss.
pub struct Image {
    /// The committed bytes, as `include_bytes!` gives them.
    pub committed: &'static [u8],
    /// File name under `tests/roms`, for the rebuild instructions.
    pub name: &'static str,
    /// Link address. Starts the `p2bin` range, and is the offset a first
    /// difference is reported at.
    pub base: u32,
    /// Length of the `p2bin` range in bytes.
    pub len: usize,
    /// Fill byte `p2bin` puts in the gaps.
    pub fill: u8,
    /// A `-D` define passed to `asl` for this link address, if it needs one.
    pub define: Option<&'static str>,
}

/// Re-assemble a conformance ROM's source and compare against every committed
/// image built from it.
///
/// The committed images are the one artifact in these suites that no reviewer
/// can check by reading, so this is the only thing standing between an edited
/// source and a stale binary.
///
/// **It therefore must not be able to pass by doing nothing.** `PHOSPHOR_ASM` is
/// exported by the Nix dev shell, and when it is set a missing assembler is a
/// failure rather than a skip; CI has no dev shell, sets nothing, and skips with
/// a printed note. The Williams guard this shape comes from reported green for
/// its entire life because `asl` was on `PATH` nowhere, which is why the trap
/// exists and why it is worth keeping in the shared copy rather than leaving to
/// each board to remember.
///
/// `stem` names the temporary files, so two boards' guards running at once do
/// not read each other's intermediate output.
pub fn assert_binary_matches_source(asm_name: &str, stem: &str, images: &[Image]) {
    use std::path::Path;
    use std::process::Command;

    let roms = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/roms");
    let asm = roms.join(asm_name);
    let tmp = std::env::temp_dir();
    let code = tmp.join(format!("phosphor_{stem}_check.p"));
    let out = tmp.join(format!("phosphor_{stem}_check.bin"));

    let expected = std::env::var_os("PHOSPHOR_ASM").is_some();

    for image in images {
        // asl appends to an existing code file rather than truncating it, so a
        // leftover from an earlier run would be re-read by p2bin.
        let _ = std::fs::remove_file(&code);

        let mut asl = Command::new("asl");
        asl.arg("-q");
        if let Some(d) = image.define {
            asl.args(["-D", d]);
        }
        let assembled = asl.arg("-o").arg(&code).arg(&asm).status();
        let assembled = match assembled {
            Ok(status) => status,
            Err(e) => {
                assert!(
                    !expected,
                    "PHOSPHOR_ASM is set, so `asl` is supposed to be on PATH here, \
                     but running it failed: {e}. The dev shell provides it; a skip \
                     at this point would report green while guarding nothing."
                );
                eprintln!("skipping: `asl` is not on PATH and PHOSPHOR_ASM is unset");
                return;
            }
        };
        assert!(assembled.success(), "asl failed on {}", asm.display());

        let range = format!(
            "0x{:04X}-0x{:04X}",
            image.base,
            image.base as usize + image.len - 1
        );
        let fill = format!("{:#04X}", image.fill);
        let converted = Command::new("p2bin")
            .arg(&code)
            .arg(&out)
            .args(["-r", &range, "-l", &fill])
            .status()
            .expect("p2bin runs when asl did");
        assert!(converted.success(), "p2bin failed on {}", code.display());

        let built = std::fs::read(&out).expect("read re-assembled image");
        let _ = std::fs::remove_file(&code);
        let _ = std::fs::remove_file(&out);

        let define_arg = image.define.map(|d| format!("-D {d} ")).unwrap_or_default();
        let name = image.name;
        let stale = format!(
            "tests/roms/{name} is stale. Rebuild it with\n  \
             asl -q {define_arg}-o out.p {asm_name}\n  \
             p2bin out.p {name} -r {range} -l {fill}"
        );
        assert_eq!(
            built.len(),
            image.committed.len(),
            "re-assembled image is {} bytes, committed is {}. {stale}",
            built.len(),
            image.committed.len()
        );
        let differs = built
            .iter()
            .zip(image.committed)
            .position(|(a, b)| a != b)
            .map(|i| {
                format!(
                    "first difference at ${:04X}: built {:#04X}, committed {:#04X}",
                    image.base as usize + i,
                    built[i],
                    image.committed[i]
                )
            });
        assert!(
            differs.is_none(),
            "{}. {stale}",
            differs.unwrap_or_default()
        );
    }
}
