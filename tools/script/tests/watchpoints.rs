//! End-to-end: watchpoints fire during `run_frame` on a real machine, and the
//! all-CPU watch catches sub-CPU writes — the per-CPU-scoping trap the design
//! warned about.
//!
//! On galaga, video/work-RAM addresses `0x9100` and `0x9800` are written by
//! both the main CPU (index 0) and a sub-CPU (index 1). A watch scoped to CPU 0
//! alone would silently miss the sub-CPU's writes (in fact the majority of
//! them). `DebugSession::watch` watches every CPU for exactly this reason, and
//! each hit carries its `cpu_index` so a script can still tell them apart.
//!
//! ROM-gated: skips cleanly when no ROM dir is present.

use phosphor_core::core::watchpoint::{WatchpointCondition, WatchpointKind};
use phosphor_harness::roms_dir;
use phosphor_script::DebugSession;

const MACHINE: &str = "galaga";

#[test]
fn all_cpu_watch_catches_sub_cpu_writes() {
    let Some(roms) = roms_dir() else {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
        return;
    };
    let path = roms.to_str().unwrap();

    let mut session = DebugSession::open(MACHINE, path).expect("open galaga");
    assert_eq!(session.cpu_count(), 3, "galaga has three Z80s");

    session.run_frames(3100); // past the power-on self-test, into attract

    let n = session.watch(0x9100, WatchpointKind::Write, WatchpointCondition::Always);
    assert_eq!(n, 3, "watch set on every CPU");
    session.watch(0x9800, WatchpointKind::Write, WatchpointCondition::Always);
    session.run_frames(600);

    let hits = session.take_hits();
    assert!(!hits.is_empty(), "expected watchpoint hits during attract");

    let mut per_cpu = [0usize; 3];
    for h in &hits {
        assert_eq!(h.kind, WatchpointKind::Write);
        assert!(
            h.addr == 0x9100 || h.addr == 0x9800,
            "unexpected hit address {:#06x}",
            h.addr
        );
        assert!(h.cpu_index < 3);
        per_cpu[h.cpu_index] += 1;
    }

    // The whole point of watching all CPUs: both the main CPU and a sub-CPU
    // wrote these addresses. A CPU-0-only watch would have silently missed
    // CPU 1 — which here writes the majority of the hits.
    assert!(per_cpu[0] > 0, "main CPU (0) should have written");
    assert!(
        per_cpu[1] > 0,
        "sub-CPU (1) should have written — the per-CPU-scoping trap"
    );
}
