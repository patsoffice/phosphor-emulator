//! Road Runner (Atari System 1) conformance ROM: the loader, on a 68000 board.
//!
//! Design: `docs/designs/roadrunner-video-conformance.md`.
//!
//! This is the skeleton step of the Road Runner conformance epic and it asserts
//! **nothing about video**. Everything the Williams harness does rests on
//! properties of `AddressSpace16` and an M6809; the 32-bit half was only ever
//! checked by reading the code. This file runs it: a machine built with
//! `MachineEntry::create_bare` and no arcade ROMs at all takes an assembled
//! 68000 image poked into its program-ROM window through `BusDebug::write`
//! (`AddressSpace32::debug_write` ignores `AccessKind`, so the `ReadOnly` region
//! takes the write), and `M68000::reset` fetches both reset vectors out of it
//! through the bus.
//!
//! Three things are measured rather than inferred: the stack pointer the CPU was
//! handed, a checksum of the whole image read back through the *real* bus, and
//! the number of vblank edges the program survived. The last is the watchdog
//! assertion: this board reboots after 8 frames without a strobe to `880001`
//! (`roadrunner.rs:773-775`), and a reboot clears the result block, so a program
//! that did not feed it cannot reach the target count.
//!
//! ROM-less, so it runs in CI.

use phosphor_core::core::machine::FrontendMachine;
use phosphor_machines::registry;

/// The assembled test program, a flat 8 KB image loaded at `0x000000`.
///
/// Built from `tests/roms/roadrunner_video.asm` with `asl` and `p2bin`, both in
/// the Nix dev shell; the exact commands are at the top of the source and in
/// [`the_committed_binary_matches_its_source`], which re-assembles and compares
/// so the two cannot drift.
const PROGRAM: &[u8] = include_bytes!("roms/roadrunner_video.bin");

/// Where the image is poked, and where the 68000 looks for its reset vectors.
const LOAD_ADDR: u32 = 0x00_0000;
/// The `p2bin -r` window, and therefore the range the program checksums.
const IMAGE_LEN: usize = 0x2000;

const MACHINE: &str = "roadrunner";

/// Frames to run before giving up. The program spends its first frame on entry
/// and the image checksum and then rides [`VB_TARGET`] vblank edges, so twice
/// that is a wide margin which still fails fast on a wedge.
const MAX_FRAMES: usize = 32;

// --- Result block, mirroring the equates in the assembly --------------------

const RES: u32 = 0x40_0000;
const R_MAGIC: u32 = RES;
const R_PHASE: u32 = RES + 2;
const R_TRAP: u32 = RES + 4;
const R_TRAPV: u32 = RES + 6;
const R_SSP: u32 = RES + 8;
const R_CKSUM: u32 = RES + 12;
const R_VBCOUNT: u32 = RES + 14;
const RESLEN: u32 = 16;

const MAGIC: u16 = 0x5A5A;
const TRAPPED: u16 = 0xDEAD;
const FINAL_PHASE: u16 = 5;

/// Vector 0 of the image: the supervisor stack pointer `cpu.reset` fetches
/// through the bus, parked in work RAM well clear of the result block.
const STACK_TOP: u32 = 0x40_1F00;

/// Vblank edges the program rides before declaring itself done. Double the
/// board's 8-frame watchdog timeout, on purpose.
const VB_TARGET: u16 = 16;

// --- Harness ----------------------------------------------------------------

struct Run {
    results: Vec<u8>,
    frames: usize,
}

fn peek(m: &dyn FrontendMachine, addr: u32) -> u8 {
    m.debug_bus()
        .expect("machine exposes a debug bus")
        .read(0, addr)
        .unwrap_or_else(|| panic!("{addr:#08X} is not readable through the debug bus"))
}

fn run() -> Run {
    let entry = registry::find(MACHINE).unwrap_or_else(|| panic!("{MACHINE} is not registered"));
    let mut m = (entry.create_bare)();

    {
        let bus = m
            .debug_bus_mut()
            .unwrap_or_else(|| panic!("{MACHINE} exposes no debug bus"));
        for (i, b) in PROGRAM.iter().enumerate() {
            bus.write(0, LOAD_ADDR + i as u32, *b);
        }
    }
    // The 68000 fetches the supervisor stack pointer from 0 and the program
    // counter from 4 through the bus, so this picks up the vectors the image
    // just installed.
    m.reset();

    let mut frames = 0;
    for _ in 0..MAX_FRAMES {
        m.run_frame();
        frames += 1;
        if word(&*m, R_MAGIC) == MAGIC {
            break;
        }
    }

    let results = (0..RESLEN).map(|i| peek(&*m, RES + i)).collect();
    Run { results, frames }
}

fn word(m: &dyn FrontendMachine, addr: u32) -> u16 {
    u16::from_be_bytes([peek(m, addr), peek(m, addr + 1)])
}

impl Run {
    fn word(&self, addr: u32) -> u16 {
        let o = (addr - RES) as usize;
        u16::from_be_bytes([self.results[o], self.results[o + 1]])
    }

    fn long(&self, addr: u32) -> u32 {
        ((self.word(addr) as u32) << 16) | self.word(addr + 2) as u32
    }

    /// Fail loudly and early if the program never finished, and say why.
    ///
    /// A zero result block is a wedge, not a pass. `R_PHASE` says how far the
    /// program got and `R_TRAP` says whether an exception is the reason, which
    /// is the difference between "the loader never worked" and "the program ran
    /// and then fell over", and those two want completely different next steps.
    fn assert_completed(&self) {
        if self.word(R_TRAP) == TRAPPED {
            panic!(
                "the conformance program took a stray exception at phase {}: \
                 the 68010 frame's vector-offset word was {:#06X} (vector {}). \
                 Every vector but reset points at the handler that recorded this.",
                self.word(R_PHASE),
                self.word(R_TRAPV),
                self.word(R_TRAPV) / 4
            );
        }
        assert_eq!(
            self.word(R_MAGIC),
            MAGIC,
            "the conformance program did not finish in {} frames (reached phase \
             {}, expected {FINAL_PHASE}). A zero result block is a wedge, not a \
             pass; phase 0 means it never executed at all, and anything past 3 \
             with a low R_VBCOUNT ({}) means the watchdog rebooted it.",
            self.frames,
            self.word(R_PHASE),
            self.word(R_VBCOUNT)
        );
        assert_eq!(self.word(R_PHASE), FINAL_PHASE, "phase mismatch");
    }
}

// ---------------------------------------------------------------------------
// The loader
// ---------------------------------------------------------------------------

/// A 68000 program executes out of poked ROM on a machine with no arcade ROMs.
///
/// The precondition for everything below and the whole point of this step: the
/// Williams loading mechanism rests on `AddressSpace16` and an M6809, and until
/// this passed, the rest of the epic was speculation.
#[test]
fn the_conformance_program_runs_to_completion() {
    run().assert_completed();
}

/// `cpu.reset` fetched the supervisor stack pointer out of the poked image.
///
/// The program records `A7` before touching it. The program counter half of the
/// reset fetch is self-evident from the program running at all; this is the
/// other half, and it fails rather than wanders if vector 0 did not arrive.
#[test]
fn the_reset_vectors_come_from_the_poked_image() {
    let r = run();
    r.assert_completed();
    assert_eq!(
        r.long(R_SSP),
        STACK_TOP,
        "the stack pointer the CPU started with should be vector 0 of the image"
    );
}

/// The CPU reads the whole 8 KB image back through the bus it actually drives.
///
/// The poke went in through the debug bus, which is not the bus the program
/// runs on. This is the assertion that every byte arrived at the right address:
/// the program sums 4096 big-endian words with 16-bit wraparound and the test
/// sums the committed file the same way. A load at the wrong offset, a short
/// image, or a poke that silently dropped `ReadOnly` writes all move it.
///
/// The image is padded with `$A5` rather than zero precisely so this stays a
/// real check: a ROM-less board's program ROM is already zero-filled, so with a
/// zero pad a truncated load would sum to the same value as a complete one. It
/// did, when this was first written with `-l 0x00`.
#[test]
fn the_whole_image_reads_back_through_the_cpu_bus() {
    let expected = PROGRAM.chunks_exact(2).fold(0u16, |acc, w| {
        acc.wrapping_add(u16::from_be_bytes([w[0], w[1]]))
    });
    let r = run();
    r.assert_completed();
    assert_eq!(
        r.word(R_CKSUM),
        expected,
        "the CPU's checksum of 000000-001FFF should match the committed image"
    );
}

/// The program rides twice the watchdog's timeout without being rebooted.
///
/// `RoadRunnerSystem::run_frame` resets the machine after 8 frames with no write
/// to `880001`. A reboot re-enters at the reset vector, which clears the result
/// block, so the count cannot creep past 8 unless every strobe landed: delete
/// `PetDog` from the vblank loop and this reads back somewhere below 8 with the
/// magic word absent, which is the failure the design doc records.
///
/// The frame count is asserted alongside it because the count alone would also
/// be satisfied by a poll loop that saw 16 edges inside one frame. One edge per
/// frame is the property; the total is only the symptom.
#[test]
fn the_program_outlives_the_watchdog() {
    let r = run();
    r.assert_completed();
    assert_eq!(
        r.word(R_VBCOUNT),
        VB_TARGET,
        "vblank edges observed; the watchdog reboots at 8 frames, so anything \
         short of this means a strobe was missed"
    );
    assert_eq!(
        r.frames, VB_TARGET as usize,
        "one vblank edge per frame: the program takes its first edge in the \
         frame it boots in and finishes on the {VB_TARGET}th, so the run should \
         be exactly {VB_TARGET} frames. More means an edge was missed, fewer \
         means the edge detector is retriggering inside one blank."
    );
}

// ---------------------------------------------------------------------------
// The committed binary cannot drift from its source
// ---------------------------------------------------------------------------

/// Re-assemble `roadrunner_video.asm` and compare against the committed binary.
///
/// The committed image is the one artifact here that no reviewer can check by
/// reading, so this is the only thing standing between an edited source and a
/// stale binary. It therefore must not be allowed to pass by doing nothing:
/// `PHOSPHOR_ASM` is exported by the Nix dev shell, and when it is set a missing
/// assembler is a failure rather than a skip. CI has no dev shell, sets nothing,
/// and skips with a printed note.
///
/// The Williams guard this is copied from reported green for its entire life
/// because `asl` was on `PATH` nowhere, which is why the trap exists and why it
/// is re-demonstrated rather than inherited on faith.
#[test]
fn the_committed_binary_matches_its_source() {
    use std::path::Path;
    use std::process::Command;

    let roms = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/roms");
    let asm = roms.join("roadrunner_video.asm");
    let tmp = std::env::temp_dir();
    let code = tmp.join("phosphor_roadrunner_video_check.p");
    let out = tmp.join("phosphor_roadrunner_video_check.bin");
    // asl appends to an existing code file rather than truncating it, so a
    // leftover from an earlier run would be re-read by p2bin.
    let _ = std::fs::remove_file(&code);

    let expected = std::env::var_os("PHOSPHOR_ASM").is_some();

    let assembled = Command::new("asl")
        .arg("-q")
        .arg("-o")
        .arg(&code)
        .arg(&asm)
        .status();
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

    let range = format!("0x0000-0x{:04X}", IMAGE_LEN - 1);
    let converted = Command::new("p2bin")
        .arg(&code)
        .arg(&out)
        .args(["-r", &range, "-l", "0xA5"])
        .status()
        .expect("p2bin runs when asl did");
    assert!(converted.success(), "p2bin failed on {}", code.display());

    let built = std::fs::read(&out).expect("read re-assembled image");
    let _ = std::fs::remove_file(&code);
    let _ = std::fs::remove_file(&out);

    let stale = format!(
        "tests/roms/roadrunner_video.bin is stale. Rebuild it with\n  \
         asl -q -o out.p roadrunner_video.asm\n  \
         p2bin out.p roadrunner_video.bin -r {range} -l 0xA5"
    );
    assert_eq!(
        built.len(),
        PROGRAM.len(),
        "re-assembled image is {} bytes, committed is {}. {stale}",
        built.len(),
        PROGRAM.len()
    );
    let differs = built
        .iter()
        .zip(PROGRAM)
        .position(|(a, b)| a != b)
        .map(|i| {
            format!(
                "first difference at ${:06X}: built {:#04X}, committed {:#04X}",
                i, built[i], PROGRAM[i]
            )
        });
    assert!(
        differs.is_none(),
        "{}. {stale}",
        differs.unwrap_or_default()
    );
}

/// The image is exactly the `p2bin -r` window, with both reset vectors in it.
///
/// A wrong window produces a short file rather than a wrong one, and a wrong
/// origin produces a vector pointing outside the image; neither is visible by
/// reading the binary.
#[test]
fn the_image_carries_its_reset_vectors() {
    assert_eq!(
        PROGRAM.len(),
        IMAGE_LEN,
        "expected an 8 KB 000000-001FFF image"
    );
    let long = |o: usize| u32::from_be_bytes(PROGRAM[o..o + 4].try_into().unwrap());
    assert_eq!(
        long(0),
        STACK_TOP,
        "vector 0 is the supervisor stack pointer"
    );
    let entry = long(4);
    assert!(
        (0x400..IMAGE_LEN as u32).contains(&entry),
        "vector 1 should point at code inside the image, past the 1 KB vector \
         table, but it is {entry:#08X}"
    );
}
