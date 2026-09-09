//! Decode the whole `SingleStepTests/m68000` corpus, and check the properties
//! every later comparison rests on.
//!
//! This tests the *oracle*, not the emulator. Nothing here runs a CPU. The
//! per-cycle gate compares this core's bus trace against these vectors, so a
//! misread container would move that gate's number for a reason having nothing
//! to do with the 68000, and it would look exactly like an emulation bug.
//!
//! The decoder itself is checked against hand-built containers in
//! `m68000_bin`'s unit tests, and was checked once against the suite's own
//! `decode.py` output over the 5,000 cases of `NOP` and `MOVE.b`, field for
//! field including every transaction field. What is left for this file is the
//! part that needs the whole corpus: that every file decodes, and that the
//! traces tile.

use phosphor_cpu_validation::m68000_bin;

#[test]
fn test_m68000_bin_corpus_decodes() {
    let dir = phosphor_cpu_validation::vector_dir("m68000/v1");
    let dir = dir.as_path();
    if !phosphor_cpu_validation::require_test_data(
        dir,
        "run: git submodule update --init cpu-validation/test_data/m68000",
    ) {
        return;
    }

    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .expect("read the vector directory")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "bin"))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    assert!(
        !entries.is_empty(),
        "no .json.bin files in {}",
        dir.display()
    );

    let mut files = 0;
    let mut cases = 0;
    let mut untiled = 0;
    let mut untiled_examples = Vec::new();
    let mut zero_length = 0;
    let mut kinds = std::collections::BTreeMap::new();

    for entry in &entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let tests = m68000_bin::decode_file(&entry.path())
            .unwrap_or_else(|e| panic!("decoding {name}: {e}"));

        assert!(!tests.is_empty(), "{name} decoded to no cases");

        for t in &tests {
            let c = &t.case;
            if !c.tiles() {
                untiled += 1;
                if untiled_examples.len() < 20 {
                    untiled_examples.push(format!(
                        "{name}: {} traced {} clocks against length {}",
                        c.name,
                        c.traced_clocks(),
                        c.length
                    ));
                }
            }
            if c.length == 0 {
                zero_length += 1;
            }
            for txn in &c.transactions {
                *kinds.entry(format!("{:?}", txn.kind)).or_insert(0usize) += 1;
            }
        }

        cases += tests.len();
        files += 1;
    }

    eprintln!("\nm68000 corpus: {cases} cases across {files} files");
    eprintln!("  zero-length cases: {zero_length}");
    eprintln!("  transaction kinds:");
    for (kind, count) in &kinds {
        eprintln!("    {kind:<18} {count}");
    }

    // A trace that does not tile its length cannot say which clock a bus cycle
    // starts on, so every positional comparison built on it would be
    // meaningless. If this ever fires, the gate above it is measuring nothing.
    if untiled > 0 {
        for line in &untiled_examples {
            eprintln!("  untiled: {line}");
        }
        panic!("{untiled} of {cases} cases do not tile their length");
    }
}
