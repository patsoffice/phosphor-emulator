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

use rhai::{Array, Dynamic, Engine, EvalAltResult, Map};

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
