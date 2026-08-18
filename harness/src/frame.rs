//! Rendering a booted machine's frame the way every consumer must agree on.
//!
//! Three places need "the frame this cabinet displays, and its fingerprint": the
//! golden-frame suite, `disasm frameshot`, and movie replay. They must produce
//! *identical* bytes and identical hashes — a `disasm movie check` hash that
//! cannot be compared against `frames.toml` is not worth printing — so the
//! encoding lives here rather than being reimplemented per consumer.
//!
//! The frame is rendered natively and then passed through the machine's declared
//! orientation, mirroring what the frontend does. Hashing the *oriented* buffer
//! is deliberate: a machine that silently loses its rotation declaration changes
//! its hash, which is exactly how Super Cobra's missing `ROT90` was caught.

use phosphor_core::core::machine::{FrontendMachine, Orientation};
use phosphor_core::device::dvg::VectorLine;
use phosphor_core::gfx::apply_orientation;
use sha2::{Digest, Sha256};

/// Render a booted machine to the buffer the cabinet displays.
///
/// Returns `(width, height, rgb)` *after* orientation, so the dimensions are the
/// displayed ones and may be the machine's native pair swapped.
pub fn render_oriented(m: &mut dyn FrontendMachine) -> (u32, u32, Vec<u8>) {
    let (nw, nh) = m.display_size();
    let mut native = vec![0u8; nw as usize * nh as usize * 3];
    m.render_frame(&mut native);

    let orient = m.orientation();
    if orient == Orientation::NORMAL {
        return (nw, nh, native);
    }
    let (dw, dh) = if orient.swaps_axes() {
        (nh, nw)
    } else {
        (nw, nh)
    };
    let mut oriented = vec![0u8; dw as usize * dh as usize * 3];
    apply_orientation(&native, &mut oriented, nw as usize, nh as usize, orient);
    (dw, dh, oriented)
}

/// SHA-256 over a length-prefixed encoding of the frame, so a buffer that
/// changes shape without changing bytes still changes the hash.
///
/// The domain tag and field order are wire format: changing either invalidates
/// every pinned hash in `harness/tests/golden/frames.toml`.
pub fn hash_frame(w: u32, h: u32, rgb: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"phosphor-frame-v1");
    hasher.update(w.to_le_bytes());
    hasher.update(h.to_le_bytes());
    hasher.update(rgb);
    format!("sha256:{:x}", hasher.finalize())
}

/// SHA-256 over the vector display list — for the vector games this, not the
/// rasterised frame, is what the frontend actually draws.
///
/// Same wire-format caution as [`hash_frame`].
pub fn hash_vectors(lines: &[VectorLine]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"phosphor-vectors-v1");
    hasher.update((lines.len() as u32).to_le_bytes());
    for l in lines {
        for c in [l.x0, l.y0, l.x1, l.y1] {
            hasher.update(c.to_le_bytes());
        }
        hasher.update([l.intensity, l.r, l.g, l.b]);
    }
    format!("sha256:{:x}", hasher.finalize())
}
