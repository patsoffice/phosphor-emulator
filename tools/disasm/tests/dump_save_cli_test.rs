//! End-to-end tests for `disasm dump-save`, driving the real binary.
//!
//! The acceptance question for the tool is whether dumping a save from a
//! machine that *has* an optional component and one that does not shows the
//! difference. Joust and Sinistar are the pair: same Williams board, but only
//! Sinistar carries the extra SRAM, the HC55516 CVSD chip and the blitter
//! window-enable. Under the old format those were appended raw and guarded by
//! the board config, so nothing in the file distinguished them from whatever
//! followed.
//!
//! Needs no ROM set: `create_bare` builds real hardware structs with a
//! zero-filled ROM, and a machine's state layout does not depend on ROM
//! contents.

use std::path::PathBuf;
use std::process::Command;

use phosphor_machines::registry;

/// The compiled binary under test, provided by cargo to integration tests.
const DISASM: &str = env!("CARGO_BIN_EXE_disasm");

struct Output {
    stdout: String,
    stderr: String,
    ok: bool,
}

fn disasm(args: &[&str]) -> Output {
    let out = Command::new(DISASM)
        .args(args)
        .output()
        .expect("run disasm");
    Output {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        ok: out.status.success(),
    }
}

fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("phosphor-dumpsave-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// A bare machine's save bytes, as the frontend's quicksave would write them.
fn save_bytes(machine: &str) -> Vec<u8> {
    let entry = registry::find(machine).expect("registered machine");
    (entry.create_bare)().save_state().expect("save state")
}

/// Write save bytes to a file named for the calling test.
///
/// The name has to be per-test: cargo runs these on threads of one process, so
/// a name keyed only on the machine has two tests writing and reading the same
/// path at once, and one of them sees a half-written file.
fn write_save_as(name: &str, data: Vec<u8>) -> PathBuf {
    let path = scratch().join(name);
    std::fs::write(&path, data).expect("write save");
    path
}

fn write_save(test: &str, machine: &str) -> PathBuf {
    write_save_as(&format!("{test}-{machine}.state"), save_bytes(machine))
}

/// Chunk names the dump listed, in order.
fn components(dump: &str) -> Vec<String> {
    dump.lines()
        .filter_map(|l| l.split_once("  0x"))
        .filter_map(|(_, rest)| rest.split_whitespace().nth(1))
        .map(str::to_string)
        .collect()
}

#[test]
fn a_save_file_dumps_its_chunk_tree() {
    let path = write_save("dumps-tree", "joust");
    let out = disasm(&["dump-save", path.to_str().unwrap()]);
    assert!(out.ok, "stderr: {}", out.stderr);

    assert!(out.stdout.contains("machine id    joust"), "{}", out.stdout);
    assert!(out.stdout.contains("checksum"), "{}", out.stdout);
    assert!(out.stdout.contains("load: ok"), "{}", out.stdout);

    let names = components(&out.stdout);
    assert!(names.contains(&"JoustSystem.cpu".to_string()), "{names:?}");
    assert!(
        names.contains(&"WilliamsBoard.blitter".to_string()),
        "nested components should appear too: {names:?}"
    );
}

/// The acceptance criterion: the optional components are visible as chunks in
/// the machine that has them and simply absent in the machine that does not.
#[test]
fn the_optional_components_are_visible_when_present_and_absent_when_not() {
    const OPTIONAL: [&str; 3] = [
        "WilliamsBoard.sram",
        "WilliamsBoard.cvsd",
        "WilliamsBoard.blitter_window",
    ];

    let joust_path = write_save("optional", "joust");
    let sinistar_path = write_save("optional", "sinistar");
    let joust = disasm(&["dump-save", joust_path.to_str().unwrap()]);
    let sinistar = disasm(&["dump-save", sinistar_path.to_str().unwrap()]);
    assert!(joust.ok && sinistar.ok);

    let (with, without) = (components(&sinistar.stdout), components(&joust.stdout));
    for name in OPTIONAL {
        assert!(with.contains(&name.to_string()), "sinistar: {with:?}");
        assert!(!without.contains(&name.to_string()), "joust: {without:?}");
    }

    // Everything else is shared: the two boards differ by exactly those three.
    let shared: Vec<_> = with
        .iter()
        .filter(|n| !OPTIONAL.contains(&n.as_str()))
        .collect();
    assert_eq!(shared.len(), without.len());
}

/// The layout query: with no file, dump what this build expects. This is what
/// a file that will not load gets diffed against.
#[test]
fn a_machine_layout_can_be_dumped_without_a_file() {
    let out = disasm(&["dump-save", "--machine", "sinistar"]);
    assert!(out.ok, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("freshly built"), "{}", out.stdout);
    assert!(
        components(&out.stdout).contains(&"WilliamsBoard.cvsd".to_string()),
        "{}",
        out.stdout
    );
}

// -- Failure modes -----------------------------------------------------------

#[test]
fn a_corrupt_file_is_reported_and_still_walked_as_far_as_it_goes() {
    // Flip a bit deep in the video RAM: the checksum fails, the framing does
    // not, so the tree is still worth printing.
    let mut data = save_bytes("joust");
    let mid = data.len() / 2;
    data[mid] ^= 0xFF;
    let corrupt = write_save_as("corrupt-joust.state", data);

    let out = disasm(&["dump-save", corrupt.to_str().unwrap()]);
    assert!(out.ok, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("MISMATCH"), "{}", out.stdout);
    assert!(
        components(&out.stdout).contains(&"JoustSystem.cpu".to_string()),
        "{}",
        out.stdout
    );
}

/// A file whose body stops part way prints the chunks that were read and then
/// says where it stopped, which is the whole reason to walk by loading.
#[test]
fn a_truncated_file_prints_the_chunks_it_reached_then_the_failure() {
    let mut data = save_bytes("joust");
    data.truncate(data.len() / 2);
    let short = write_save_as("short-joust.state", data);

    let out = disasm(&["dump-save", short.to_str().unwrap()]);
    assert!(out.ok, "stderr: {}", out.stderr);
    assert!(out.stdout.contains("load: FAILED"), "{}", out.stdout);
    assert!(
        components(&out.stdout).contains(&"JoustSystem.cpu".to_string()),
        "the chunks read before the truncation should still be listed: {}",
        out.stdout
    );
}

#[test]
fn a_file_that_is_not_a_save_fails_with_a_message_not_a_panic() {
    let path = scratch().join("not-a-save.bin");
    std::fs::write(&path, b"this is not a save file at all").unwrap();

    let out = disasm(&["dump-save", path.to_str().unwrap()]);
    assert!(!out.ok);
    assert!(out.stderr.contains("bad magic"), "stderr: {}", out.stderr);
}

#[test]
fn an_unknown_machine_lists_where_to_look() {
    let out = disasm(&["dump-save", "--machine", "notagame"]);
    assert!(!out.ok);
    assert!(
        out.stderr.contains("disasm machines"),
        "stderr: {}",
        out.stderr
    );
}

#[test]
fn a_pre_chunk_envelope_is_named_rather_than_parsed() {
    let mut data = save_bytes("joust");
    // Rewrite the envelope version to 12, the last flat-body format.
    data[4..8].copy_from_slice(&12u32.to_le_bytes());
    let old = write_save_as("v12-joust.state", data);

    let out = disasm(&["dump-save", old.to_str().unwrap()]);
    assert!(out.ok, "stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("cannot read envelope version 12"),
        "{}",
        out.stdout
    );
}
