//! A board's schematic transcription as data.
//!
//! See `docs/designs/schematic-transcription.md`. A pin-level netlist per
//! board is the source of truth for its parts, values and junctions; the
//! netlistsvg JSON beside the prose is generated from it, and the lints,
//! constants and offline solver that the design's later rungs describe all
//! read it through this crate.
//!
//! This is a library with a thin binary on top, rather than a binary alone,
//! because those later consumers live outside it.

pub mod lint;
pub mod netlist;
pub mod solve;
pub mod svg;
