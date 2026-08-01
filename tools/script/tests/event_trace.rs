//! End-to-end: bus event tracing produces region-tagged, CPU-agnostic write
//! events on a real board — the property that makes the event trace a better
//! instrument than an address-exact watchpoint for "which registers are
//! written, and where".
//!
//! Satan's Hollow is an AddressSpace16 board with full event tracing wired
//! (unlike the Namco boards, which still use an older hand-rolled path).
//!
//! ROM-gated: skips cleanly when no ROM dir is present.

use phosphor_core::core::debug_trace::DebugEventKind;
use phosphor_core::core::watchpoint::DebugAccessSource;
use phosphor_harness::roms_dir;
use phosphor_script::DebugSession;

const MACHINE: &str = "shollow";

#[test]
fn event_trace_captures_region_tagged_writes() {
    let Some(roms) = roms_dir() else {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
        return;
    };
    let path = roms.to_str().unwrap();

    let mut session = DebugSession::open(MACHINE, path).expect("open shollow");
    session.run_frames(400); // into attract

    assert!(!session.trace_enabled());
    session.set_trace(true);
    assert!(session.trace_enabled());
    session.run_frames(1);
    let events = session.take_events();

    assert!(!events.is_empty(), "expected trace events during attract");
    // Region-tagging: the event trace names the region an address falls in.
    assert!(
        events.iter().any(|e| e.region.is_some()),
        "expected some region-tagged events"
    );
    // CPU-agnostic write capture (both memory and I/O writes are recorded).
    assert!(
        events.iter().any(|e| matches!(
            e.kind,
            DebugEventKind::MemoryWrite | DebugEventKind::IoWrite
        )),
        "expected write events"
    );

    // take_events drains.
    assert!(session.take_events().is_empty());

    // Disabling trace stops recording.
    session.set_trace(false);
    session.run_frames(1);
    assert!(session.take_events().is_empty());
}

#[test]
fn frontend_poke_is_tagged_in_the_trace() {
    // Follow-up to the memory-poke capability: a script/console poke must
    // surface in the event trace as a DebugAccessSource::Frontend write, so it
    // is distinguishable from a hardware store instead of masquerading as one.
    let Some(roms) = roms_dir() else {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
        return;
    };
    let path = roms.to_str().unwrap();

    // mrdo is a derived-BusDebug board; the poke routes through the address
    // space's tagged `poke`, which the derive now wires up for every board.
    let mut session = DebugSession::open("mrdo", path).expect("open mrdo");
    session.run_frames(200);

    session.set_trace(true);
    assert!(session.poke(0, 0x8000, 0xAA), "mrdo has debug support");
    let events = session.take_events();

    let frontend: Vec<_> = events
        .iter()
        .filter(|e| e.source == DebugAccessSource::Frontend)
        .collect();
    assert_eq!(frontend.len(), 1, "exactly one Frontend-tagged poke event");
    let e = frontend[0];
    assert_eq!(e.kind, DebugEventKind::MemoryWrite);
    assert_eq!(e.addr, Some(0x8000));
    assert_eq!(e.value, Some(0xAA));
    assert!(e.region.is_some(), "the poke event is region-tagged");
    // A hardware write to the same RAM would be tagged Cpu(_), not Frontend —
    // that is the whole point.
}
