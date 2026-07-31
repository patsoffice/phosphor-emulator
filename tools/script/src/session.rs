//! `DebugSession`: a booted machine plus read-first inspection accessors.
//!
//! Wraps the shared [`phosphor_harness::Harness`] (boot + frame stepping) and
//! layers on the read-only "observe + drive" surface the Rhai bindings expose:
//! memory read, CPU pc/regs, disassemble, `run_frames`/`step`, inputs by stable
//! name, and screenshot-to-PNG.
//!
//! Skeleton only — the accessors land in a follow-up
//! (`phosphor-emulator-rhai-scripting-yrwn.3`).
