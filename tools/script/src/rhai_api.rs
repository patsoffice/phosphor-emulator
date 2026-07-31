//! Rhai engine builder and bindings.
//!
//! Registers the `Machine` custom type, the per-method bindings onto the
//! read-first [`crate::session::DebugSession`] surface, the global
//! `open(machine_name, rom_path) -> Machine` function, the `print`/`debug`
//! output hooks, and the runaway guards (`set_max_operations` /
//! `set_max_call_levels`).
//!
//! Rhai's default engine exposes no ambient time or RNG, and this module adds
//! none — the emulator has no wall-clock or RNG in its tick path and replay
//! honesty depends on keeping it that way.

use std::cell::RefCell;
use std::rc::Rc;

use rhai::{Array, Blob, Dynamic, Engine, EvalAltResult, Map};

use phosphor_core::core::debug_hang::HangReport;
use phosphor_core::core::debug_trace::DebugEvent;
use phosphor_core::core::machine::{DipApplyTiming, DipOption, DipSwitchBank};
use phosphor_core::core::watchpoint::{
    DebugAccessSource, WatchpointCondition, WatchpointHit, WatchpointKind,
};

use crate::session::DebugSession;

/// A script-visible machine handle.
///
/// `Rc<RefCell<DebugSession>>` because Rhai requires registered types be
/// `Clone` (the handle is cheap to clone) and, without the `sync` feature,
/// needs no `Send`/`Sync` (the engine is single-threaded). Cloning a handle
/// aliases the same session, so `open()`-ing several machines and holding them
/// all is natural — that's what enables in-repo A/B comparisons.
pub type Machine = Rc<RefCell<DebugSession>>;

/// Runaway guards. Generous enough for real capture scripts, finite so an
/// infinite loop or unbounded recursion always terminates — matters for CI
/// scripts and the future interactive console.
const MAX_OPERATIONS: u64 = 1_000_000_000;
const MAX_CALL_LEVELS: usize = 64;

/// Open a machine into a script-visible [`Machine`] handle — the Rust-side
/// counterpart of the script's global `open`. The CLI uses this to pre-bind
/// `m` before evaluating a script.
pub fn open_machine(machine_name: &str, rom_path: &str) -> Result<Machine, String> {
    DebugSession::open(machine_name, rom_path).map(|s| Rc::new(RefCell::new(s)))
}

/// Build a Rhai [`Engine`] with the v1 machine bindings, stdout `print`/`debug`
/// hooks, and the runaway guards.
pub fn build_engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_max_operations(MAX_OPERATIONS);
    engine.set_max_call_levels(MAX_CALL_LEVELS);
    engine.on_print(|text| println!("{text}"));
    engine.on_debug(|text, source, pos| match source {
        Some(src) => println!("{text}  [{src} @ {pos:?}]"),
        None => println!("{text}  [{pos:?}]"),
    });
    register_machine(&mut engine);
    engine
}

/// Register the `Machine` type, the global `open()`, and the v1 method surface
/// (each entry maps 1:1 onto a [`DebugSession`] accessor).
fn register_machine(engine: &mut Engine) {
    engine.register_type_with_name::<Machine>("Machine");

    // Global constructor: open one — or several — machines.
    engine.register_fn(
        "open",
        |name: &str, path: &str| -> Result<Machine, Box<EvalAltResult>> {
            open_machine(name, path).map_err(|e| e.into())
        },
    );

    // --- Drive ---
    engine.register_fn("run_frames", |m: &mut Machine, n: i64| {
        m.borrow_mut().run_frames(n.max(0) as u64);
    });
    engine.register_fn("step", |m: &mut Machine| -> i64 {
        i64::from(m.borrow_mut().step())
    });
    engine.register_fn(
        "poke",
        |m: &mut Machine, cpu: i64, addr: i64, data: i64| -> bool {
            m.borrow_mut().poke(cpu as usize, addr as u32, data as u8)
        },
    );
    engine.register_fn("input", |m: &mut Machine, name: &str, on: bool| {
        m.borrow_mut().input(name, on);
    });
    engine.register_fn("input_axis", |m: &mut Machine, name: &str, v: f64| {
        m.borrow_mut().input_axis(name, v as f32);
    });

    // --- Inspect (unmapped / missing → -1, matching the read-first contract) ---
    engine.register_fn("read", |m: &mut Machine, cpu: i64, addr: i64| -> i64 {
        m.borrow_mut()
            .read(cpu as usize, addr as u32)
            .map_or(-1, i64::from)
    });
    engine.register_fn("pc", |m: &mut Machine, cpu: i64| -> i64 {
        m.borrow_mut().pc(cpu as usize).map_or(-1, i64::from)
    });
    engine.register_fn("regs", |m: &mut Machine, cpu: i64| -> Map {
        let mut map = Map::new();
        for (name, value) in m.borrow_mut().regs(cpu as usize) {
            map.insert(name.into(), Dynamic::from(value as i64));
        }
        map
    });
    engine.register_fn("disasm", |m: &mut Machine, cpu: i64, addr: i64| -> String {
        m.borrow_mut()
            .disasm(cpu as usize, addr as u32)
            .unwrap_or_default()
    });

    // --- Watchpoints ---
    // `watch*` set on every CPU (see DebugSession::watch for why) and return the
    // CPU count; `hits()` drains the accumulated hits as an array of maps.
    engine.register_fn("cpu_count", |m: &mut Machine| -> i64 {
        m.borrow_mut().cpu_count() as i64
    });
    engine.register_fn(
        "watch",
        |m: &mut Machine, addr: i64, kind: &str| -> Result<i64, Box<EvalAltResult>> {
            watch_all(m, addr, kind, WatchpointCondition::Always)
        },
    );
    engine.register_fn(
        "watch_value",
        |m: &mut Machine, addr: i64, kind: &str, value: i64| -> Result<i64, Box<EvalAltResult>> {
            watch_all(m, addr, kind, WatchpointCondition::Equals(value as u32))
        },
    );
    engine.register_fn(
        "watch_changed",
        |m: &mut Machine, addr: i64, kind: &str| -> Result<i64, Box<EvalAltResult>> {
            watch_all(m, addr, kind, WatchpointCondition::Changed)
        },
    );
    engine.register_fn(
        "watch_bits",
        |m: &mut Machine,
         addr: i64,
         kind: &str,
         mask: i64,
         expected: i64|
         -> Result<i64, Box<EvalAltResult>> {
            watch_all(
                m,
                addr,
                kind,
                WatchpointCondition::Bits {
                    mask: mask as u32,
                    expected: expected as u32,
                },
            )
        },
    );
    engine.register_fn(
        "watch_cpu",
        |m: &mut Machine, cpu: i64, addr: i64, kind: &str| -> Result<(), Box<EvalAltResult>> {
            let k = parse_kind(kind)?;
            let mut s = m.borrow_mut();
            for kind in k {
                s.watch_cpu(cpu as usize, addr as u32, kind, WatchpointCondition::Always);
            }
            Ok(())
        },
    );
    engine.register_fn("clear_watchpoints", |m: &mut Machine| {
        m.borrow_mut().clear_watchpoints();
    });
    engine.register_fn("hits", |m: &mut Machine| -> Array {
        m.borrow_mut().take_hits().iter().map(hit_to_map).collect()
    });

    // --- Event trace ---
    // CPU-agnostic, region-tagged, mirror-resolving bus events; `events()`
    // drains the collected events as an array of maps.
    engine.register_fn("trace", |m: &mut Machine, on: bool| {
        m.borrow_mut().set_trace(on);
    });
    engine.register_fn("trace_enabled", |m: &mut Machine| -> bool {
        m.borrow_mut().trace_enabled()
    });
    engine.register_fn("events", |m: &mut Machine| -> Array {
        m.borrow_mut()
            .take_events()
            .iter()
            .map(event_to_map)
            .collect()
    });

    // --- Hang detection (per-frame PC sampling) ---
    engine.register_fn("detect_hangs", |m: &mut Machine| {
        m.borrow_mut().detect_hangs(8, 120); // Dig Dug defaults
    });
    engine.register_fn(
        "detect_hangs",
        |m: &mut Machine, window: i64, threshold: i64| {
            m.borrow_mut()
                .detect_hangs(window.max(0) as u32, threshold.max(0) as u32);
        },
    );
    engine.register_fn("hangs", |m: &mut Machine| -> Array {
        m.borrow_mut()
            .take_hangs()
            .iter()
            .map(hang_to_map)
            .collect()
    });

    // --- Save state / reset ---
    engine.register_fn(
        "save_state",
        |m: &mut Machine| -> Result<Blob, Box<EvalAltResult>> {
            m.borrow()
                .save_state()
                .ok_or_else(|| "machine does not support save states".into())
        },
    );
    engine.register_fn(
        "load_state",
        |m: &mut Machine, data: Blob| -> Result<(), Box<EvalAltResult>> {
            m.borrow_mut().load_state(&data).map_err(|e| e.into())
        },
    );
    engine.register_fn("reset", |m: &mut Machine| {
        m.borrow_mut().reset();
    });

    // --- DIP switches ---
    // `dip_banks()` exposes the metadata a script needs to find an option;
    // `set_dip(option, choice)` is the ergonomic by-name setter for sweeps.
    engine.register_fn("dip_banks", |m: &mut Machine| -> Array {
        m.borrow().dip_banks().iter().map(dip_bank_to_map).collect()
    });
    engine.register_fn("dip_bank", |m: &mut Machine, bank: i64| -> i64 {
        i64::from(m.borrow().dip_bank_value(bank as usize))
    });
    engine.register_fn("set_dip_bank", |m: &mut Machine, bank: i64, value: i64| {
        m.borrow_mut()
            .set_dip_bank_value(bank as usize, value as u8);
    });
    engine.register_fn(
        "set_dip_option",
        |m: &mut Machine, bank: i64, option: i64, value: i64| {
            m.borrow_mut()
                .set_dip_option(bank as usize, option as usize, value as u8);
        },
    );
    engine.register_fn(
        "set_dip",
        |m: &mut Machine, option: &str, choice: &str| -> bool {
            m.borrow_mut().set_dip_by_name(option, choice)
        },
    );

    // --- Capture ---
    engine.register_fn(
        "screenshot",
        |m: &mut Machine, path: &str| -> Result<(), Box<EvalAltResult>> {
            m.borrow_mut().screenshot(path).map_err(|e| e.into())
        },
    );

    // --- Identity / status ---
    engine.register_fn("frame_count", |m: &mut Machine| -> i64 {
        m.borrow().frame_count() as i64
    });
    engine.register_fn("id", |m: &mut Machine| -> String {
        m.borrow().machine_id()
    });
    engine.register_fn("display_size", |m: &mut Machine| -> Array {
        let (w, h) = m.borrow().display_size();
        vec![Dynamic::from(i64::from(w)), Dynamic::from(i64::from(h))]
    });
}

/// Parse a watch-kind string into the address-space kind(s) to set. `"access"`
/// expands to both a read and a write watchpoint.
fn parse_kind(s: &str) -> Result<Vec<WatchpointKind>, Box<EvalAltResult>> {
    match s.to_ascii_lowercase().as_str() {
        "read" | "r" => Ok(vec![WatchpointKind::Read]),
        "write" | "w" => Ok(vec![WatchpointKind::Write]),
        "access" | "rw" | "both" => Ok(vec![WatchpointKind::Read, WatchpointKind::Write]),
        other => Err(format!(
            "unknown watch kind {other:?}; use \"read\", \"write\", or \"access\""
        )
        .into()),
    }
}

/// Shared body for the `watch*` bindings: set the condition on every CPU for
/// each requested kind, returning the CPU count.
fn watch_all(
    m: &mut Machine,
    addr: i64,
    kind: &str,
    cond: WatchpointCondition,
) -> Result<i64, Box<EvalAltResult>> {
    let kinds = parse_kind(kind)?;
    let mut session = m.borrow_mut();
    let mut count = 0;
    for k in kinds {
        count = session.watch(addr as u32, k, cond);
    }
    Ok(count as i64)
}

/// Human-readable form of a hit's access source.
fn source_str(source: DebugAccessSource) -> String {
    match source {
        DebugAccessSource::Cpu(i) => format!("cpu:{i}"),
        DebugAccessSource::Dma => "dma".to_string(),
        DebugAccessSource::Device(name) => format!("device:{name}"),
        DebugAccessSource::Frontend => "frontend".to_string(),
        DebugAccessSource::Unknown => "unknown".to_string(),
    }
}

/// Convert a watchpoint hit into a script-visible map.
fn hit_to_map(hit: &WatchpointHit) -> Dynamic {
    let mut map = Map::new();
    map.insert("cpu".into(), (hit.cpu_index as i64).into());
    map.insert("addr".into(), (hit.addr as i64).into());
    let kind = match hit.kind {
        WatchpointKind::Read => "read",
        WatchpointKind::Write => "write",
    };
    map.insert("kind".into(), kind.into());
    map.insert("value".into(), (hit.value as i64).into());
    map.insert("width".into(), (i64::from(hit.width)).into());
    map.insert("pc".into(), hit.pc.map_or(-1i64, |p| p as i64).into());
    map.insert("cycle".into(), (hit.cycle as i64).into());
    map.insert("source".into(), source_str(hit.source).into());
    map.insert("region".into(), hit.region.unwrap_or("").into());
    Dynamic::from_map(map)
}

/// Convert a bus trace event into a script-visible map. Absent optional fields
/// become `-1` (numbers) or `""` (strings).
fn event_to_map(event: &DebugEvent) -> Dynamic {
    let mut map = Map::new();
    map.insert("cycle".into(), (event.cycle as i64).into());
    map.insert("kind".into(), event.kind.label().into());
    map.insert("source".into(), source_str(event.source).into());
    map.insert(
        "cpu".into(),
        event.cpu_index.map_or(-1i64, |c| c as i64).into(),
    );
    map.insert("pc".into(), event.pc.map_or(-1i64, |p| p as i64).into());
    map.insert("addr".into(), event.addr.map_or(-1i64, |a| a as i64).into());
    map.insert(
        "value".into(),
        event.value.map_or(-1i64, |v| v as i64).into(),
    );
    map.insert("width".into(), (i64::from(event.width)).into());
    map.insert("region".into(), event.region.unwrap_or("").into());
    map.insert("device".into(), event.device.unwrap_or("").into());
    map.insert("detail".into(), event.detail.unwrap_or("").into());
    Dynamic::from_map(map)
}

/// Convert a DIP bank's static metadata into a script-visible map:
/// `{ name, options: [{ name, mask, apply, choices: [{ label, value }] }] }`.
fn dip_bank_to_map(bank: &DipSwitchBank) -> Dynamic {
    let mut map = Map::new();
    map.insert("name".into(), bank.name.into());
    let options: Array = bank.options.iter().map(dip_option_to_map).collect();
    map.insert("options".into(), Dynamic::from_array(options));
    Dynamic::from_map(map)
}

fn dip_option_to_map(option: &DipOption) -> Dynamic {
    let mut map = Map::new();
    map.insert("name".into(), option.name.into());
    map.insert("mask".into(), i64::from(option.mask).into());
    let apply = match option.apply {
        DipApplyTiming::Immediate => "immediate",
        DipApplyTiming::OnReset => "on_reset",
    };
    map.insert("apply".into(), apply.into());
    let choices: Array = option
        .choices
        .iter()
        .map(|c| {
            let mut cm = Map::new();
            cm.insert("label".into(), c.label.into());
            cm.insert("value".into(), i64::from(c.value).into());
            Dynamic::from_map(cm)
        })
        .collect();
    map.insert("choices".into(), Dynamic::from_array(choices));
    Dynamic::from_map(map)
}

/// Convert a hang report into a script-visible map.
fn hang_to_map(report: &HangReport) -> Dynamic {
    let mut map = Map::new();
    map.insert("cpu".into(), (report.cpu_index as i64).into());
    map.insert("pc".into(), (report.pc as i64).into());
    map.insert("window_lo".into(), (report.window_lo as i64).into());
    map.insert("window_hi".into(), (report.window_hi as i64).into());
    map.insert(
        "frames_stuck".into(),
        (i64::from(report.frames_stuck)).into(),
    );
    Dynamic::from_map(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rhai::Scope;

    use phosphor_core::core::machine::InputEvent;

    use crate::test_support::{COIN_ID, stub_session};

    /// Build the engine and a pre-bound `m` in a scope, over a stub machine.
    fn engine_with_m(has_debug: bool) -> (Engine, Scope<'static>, Machine) {
        let (session, _rec) = stub_session(has_debug);
        let m: Machine = Rc::new(RefCell::new(session));
        let mut scope = Scope::new();
        scope.push("m", m.clone());
        (build_engine(), scope, m)
    }

    #[test]
    fn run_frames_advances_frame_count() {
        let (engine, mut scope, _m) = engine_with_m(true);
        let frames = engine
            .eval_with_scope::<i64>(&mut scope, "m.run_frames(3); m.frame_count()")
            .unwrap();
        assert_eq!(frames, 3);
    }

    #[test]
    fn read_returns_the_seed() {
        let (engine, mut scope, _m) = engine_with_m(true);
        let v = engine
            .eval_with_scope::<i64>(&mut scope, "m.read(0, 0x10)")
            .unwrap();
        assert_eq!(v, 0x11);
    }

    #[test]
    fn read_of_unmapped_is_minus_one() {
        // has_debug = false → no debug bus → read yields None → -1.
        let (engine, mut scope, _m) = engine_with_m(false);
        let v = engine
            .eval_with_scope::<i64>(&mut scope, "m.read(0, 0x10)")
            .unwrap();
        assert_eq!(v, -1);
    }

    #[test]
    fn poke_then_read_via_script() {
        let (engine, mut scope, _m) = engine_with_m(true);
        let v = engine
            .eval_with_scope::<i64>(&mut scope, "m.poke(0, 0x20, 0xEE); m.read(0, 0x20)")
            .unwrap();
        assert_eq!(v, 0xEE);
        // poke reports whether the machine has a debug bus.
        assert!(
            engine
                .eval_with_scope::<bool>(&mut scope, "m.poke(0, 0x21, 1)")
                .unwrap()
        );
    }

    #[test]
    fn poke_without_debug_returns_false_via_script() {
        let (engine, mut scope, _m) = engine_with_m(false);
        assert!(
            !engine
                .eval_with_scope::<bool>(&mut scope, "m.poke(0, 0x20, 0xEE)")
                .unwrap()
        );
    }

    #[test]
    fn watchpoint_via_script() {
        let (engine, mut scope, _m) = engine_with_m(true);
        let n = engine
            .eval_with_scope::<i64>(&mut scope, r#"m.watch(0x40, "write")"#)
            .unwrap();
        assert_eq!(n, 1); // stub exposes one CPU
        engine
            .run_with_scope(&mut scope, "m.poke(0, 0x40, 0x99);")
            .unwrap();

        let hits = engine
            .eval_with_scope::<Array>(&mut scope, "m.hits()")
            .unwrap();
        assert_eq!(hits.len(), 1);
        let hit = hits[0].clone().cast::<Map>();
        assert_eq!(hit["addr"].as_int().unwrap(), 0x40);
        assert_eq!(hit["value"].as_int().unwrap(), 0x99);
        assert_eq!(hit["cpu"].as_int().unwrap(), 0);
        assert_eq!(hit["kind"].clone().into_string().unwrap(), "write");

        // hits() drains — a second call is empty.
        assert!(
            engine
                .eval_with_scope::<Array>(&mut scope, "m.hits()")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn watch_bad_kind_throws() {
        let (engine, mut scope, _m) = engine_with_m(true);
        let err = engine
            .eval_with_scope::<i64>(&mut scope, r#"m.watch(0x40, "bogus")"#)
            .unwrap_err();
        assert!(matches!(*err, EvalAltResult::ErrorRuntime(..)));
    }

    #[test]
    fn event_trace_via_script() {
        let (engine, mut scope, _m) = engine_with_m(true);
        engine
            .run_with_scope(&mut scope, "m.trace(true); m.run_frames(2);")
            .unwrap();
        let events = engine
            .eval_with_scope::<Array>(&mut scope, "m.events()")
            .unwrap();
        assert_eq!(events.len(), 2);
        let e = events[0].clone().cast::<Map>();
        assert_eq!(e["addr"].as_int().unwrap(), 0x1234);
        assert_eq!(e["kind"].clone().into_string().unwrap(), "mem wr");
        assert_eq!(e["region"].clone().into_string().unwrap(), "test-ram");
        // events() drains — a second call is empty.
        assert!(
            engine
                .eval_with_scope::<Array>(&mut scope, "m.events()")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn hang_detection_via_script() {
        let (engine, mut scope, _m) = engine_with_m(true);
        engine
            .run_with_scope(&mut scope, "m.detect_hangs(8, 3); m.run_frames(5);")
            .unwrap();
        let hangs = engine
            .eval_with_scope::<Array>(&mut scope, "m.hangs()")
            .unwrap();
        assert_eq!(hangs.len(), 1);
        let h = hangs[0].clone().cast::<Map>();
        assert_eq!(h["cpu"].as_int().unwrap(), 0);
        assert_eq!(h["pc"].as_int().unwrap(), 0x1234);
        assert!(h["frames_stuck"].as_int().unwrap() >= 3);
    }

    #[test]
    fn save_load_state_via_script() {
        let (engine, mut scope, _m) = engine_with_m(true);
        let restored = engine
            .eval_with_scope::<i64>(
                &mut scope,
                r#"
                    m.poke(0, 0x50, 0xAA);
                    let snap = m.save_state();
                    m.poke(0, 0x50, 0xBB);
                    m.load_state(snap);
                    m.read(0, 0x50)
                "#,
            )
            .unwrap();
        assert_eq!(restored, 0xAA);
    }

    #[test]
    fn dip_editing_via_script() {
        let (engine, mut scope, _m) = engine_with_m(true);

        let banks = engine
            .eval_with_scope::<Array>(&mut scope, "m.dip_banks()")
            .unwrap();
        assert_eq!(banks.len(), 1);
        let bank = banks[0].clone().cast::<Map>();
        assert_eq!(bank["name"].clone().into_string().unwrap(), "TEST");

        assert!(
            engine
                .eval_with_scope::<bool>(&mut scope, r#"m.set_dip("Lives", "5")"#)
                .unwrap()
        );
        assert_eq!(
            engine
                .eval_with_scope::<i64>(&mut scope, "m.dip_bank(0) & 0x03")
                .unwrap(),
            0x01
        );
    }

    #[test]
    fn input_reaches_handle_input() {
        let (session, rec) = stub_session(true);
        let m: Machine = Rc::new(RefCell::new(session));
        let mut scope = Scope::new();
        scope.push("m", m.clone());
        build_engine()
            .run_with_scope(
                &mut scope,
                r#"m.input("coin", true); m.run_frames(1); m.input("coin", false);"#,
            )
            .unwrap();

        assert_eq!(
            rec.borrow().inputs,
            vec![
                InputEvent::Button {
                    id: COIN_ID,
                    pressed: true
                },
                InputEvent::Button {
                    id: COIN_ID,
                    pressed: false
                },
            ]
        );
        assert_eq!(m.borrow().frame_count(), 1);
    }

    #[test]
    fn pc_regs_disasm_and_display_size_bindings() {
        let (engine, mut scope, _m) = engine_with_m(true);

        assert_eq!(
            engine
                .eval_with_scope::<i64>(&mut scope, "m.pc(0)")
                .unwrap(),
            0x1234
        );

        let regs = engine
            .eval_with_scope::<Map>(&mut scope, "m.regs(0)")
            .unwrap();
        assert_eq!(regs["A"].as_int().unwrap(), 0x42);
        assert_eq!(regs["PC"].as_int().unwrap(), 0x1234);

        assert_eq!(
            engine
                .eval_with_scope::<String>(&mut scope, "m.disasm(0, 0x1000)")
                .unwrap(),
            "NOP"
        );

        let size = engine
            .eval_with_scope::<Array>(&mut scope, "m.display_size()")
            .unwrap();
        assert_eq!(size[0].as_int().unwrap(), 4);
        assert_eq!(size[1].as_int().unwrap(), 3);

        assert_eq!(
            engine
                .eval_with_scope::<String>(&mut scope, "m.id()")
                .unwrap(),
            "stub"
        );
        assert_eq!(
            engine
                .eval_with_scope::<i64>(&mut scope, "m.step()")
                .unwrap(),
            1
        );
    }

    #[test]
    fn max_operations_aborts_an_infinite_loop() {
        let (mut engine, mut scope, _m) = engine_with_m(true);
        // Tighten the guard so the abort is fast; the default is a generous
        // backstop that would take far too long to hit in a unit test.
        engine.set_max_operations(10_000);
        let err = engine
            .run_with_scope(&mut scope, "let i = 0; while true { i += 1; }")
            .unwrap_err();
        assert!(
            matches!(*err, EvalAltResult::ErrorTooManyOperations(_)),
            "expected ErrorTooManyOperations, got {err:?}"
        );
    }

    #[test]
    fn open_of_unknown_machine_throws() {
        // Exercises the `open` binding's error path without ROM files: the
        // registry lookup fails, so the script call throws.
        let engine = build_engine();
        let err = engine
            .run(r#"let m = open("definitely_not_a_machine", "/no/such/path");"#)
            .unwrap_err();
        assert!(
            matches!(*err, EvalAltResult::ErrorRuntime(..)),
            "expected a runtime error from open(), got {err:?}"
        );
    }
}
