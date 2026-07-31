//! Rhai scripting / debugging interface for phosphor machines.
//!
//! A headless, read-first programmable interface for driving and inspecting
//! machines — the phosphor analogue of MAME's Lua, built on the shared
//! [`phosphor_harness::Harness`] and the `core` debug traits.
//!
//! The crate is split into a **library** (this module tree) and a **binary**
//! (`src/main.rs`). The split is deliberate: the deferred in-frontend console
//! embeds the same engine and bindings against a *live* machine, so the engine
//! builder must live in a library rather than be buried in the CLI binary.
//!
//! - [`session`] — `DebugSession`: wraps the `Harness` and adds the read-first
//!   inspection accessors (memory read, CPU pc/regs, disassemble, screenshot).
//! - [`rhai_api`] — the Rhai engine builder, the `Machine` custom type, and the
//!   global `open()` function.

pub mod rhai_api;
pub mod session;

pub use session::DebugSession;
