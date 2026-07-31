//! Rhai engine builder and bindings.
//!
//! Registers the `Machine` custom type (`Rc<RefCell<DebugSession>>`), the
//! per-method bindings onto the read-first [`crate::session::DebugSession`]
//! surface, the global `open(machine_name, rom_path) -> Machine` function, the
//! `print`/`debug` output hooks, and the runaway guards
//! (`set_max_operations` / `set_max_call_levels`).
//!
//! Skeleton only — the engine builder and bindings land in a follow-up
//! (`phosphor-emulator-rhai-scripting-yrwn.4`).
