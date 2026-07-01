//! TMS5220 voice synthesis processor — status-only stub.
//!
//! Real LPC speech synthesis is a large follow-up (shared with the Star Wars /
//! Votrax speech work). For now this models just enough of the chip's handshake
//! for a host CPU to stream speech data without stalling: the chip always
//! reports **ready** to accept the next byte and **not talking**, and it produces
//! silence.
//!
//! On the Atari System 1 sound board the CPU never touches the TMS directly — it
//! reaches it through a [`crate::device::via6522::Via6522`]: Port A carries the
//! bidirectional data / status byte, and Port B carries the `/WS` and `/RS`
//! strobes plus the `/READY` (D2) and `/INT` (D3) status lines the CPU polls.
//! The board wires those lines to the methods below.

use crate::core::save_state::{SaveError, Saveable, StateReader, StateWriter};

/// TMS5220 (stub). Accepts and discards speech data, is always ready, and never
/// talks — so any "wait until ready" or "wait until speech finishes" poll in the
/// host program completes immediately. Stateless; produces no audio.
pub struct Tms5220;

impl Tms5220 {
    pub fn new() -> Self {
        Self
    }

    /// Reset to the idle power-on state (a no-op for the stateless stub).
    pub fn reset(&mut self) {}

    /// Write a speech-data byte (Port A, latched on the `/WS` strobe). The stub
    /// discards it.
    pub fn data_w(&mut self, _data: u8) {}

    /// Read the status register (Port A, on the `/RS` strobe). Reports an idle
    /// chip: talk-status (D7) clear, so a "wait for speech to finish" poll ends
    /// at once.
    pub fn status_r(&self) -> u8 {
        0x00
    }

    /// `/READY` line: `true` when the chip can accept the next byte. Always ready,
    /// so the host's ready-wait never blocks.
    pub fn ready(&self) -> bool {
        true
    }

    /// `/INT` line: `true` when an interrupt is asserted. Never, for the stub.
    pub fn int_asserted(&self) -> bool {
        false
    }

    /// Drain synthesized audio. Silent (empty) until real LPC speech lands.
    pub fn drain_audio(&mut self) -> Vec<f32> {
        Vec::new()
    }
}

impl Default for Tms5220 {
    fn default() -> Self {
        Self::new()
    }
}

// Stateless stub: nothing to persist, but it still participates in the sound
// board's save-state stream so wiring it up later needs no format change.
impl Saveable for Tms5220 {
    fn save_state(&self, _w: &mut StateWriter) {}
    fn load_state(&mut self, _r: &mut StateReader) -> Result<(), SaveError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_ready_never_talking_or_interrupting() {
        let mut tms = Tms5220::new();
        assert!(tms.ready(), "stub must always be ready to accept data");
        assert!(!tms.int_asserted(), "stub never raises an interrupt");
        // Talk-status (bit 7) clear → any speech-done poll completes at once.
        assert_eq!(tms.status_r() & 0x80, 0);
        // Streaming data never changes readiness (no buffering to fill).
        for b in 0..64u16 {
            tms.data_w(b as u8);
            assert!(tms.ready());
        }
    }

    #[test]
    fn produces_no_audio() {
        let mut tms = Tms5220::new();
        tms.data_w(0x55);
        assert!(tms.drain_audio().is_empty(), "stub is silent");
    }

    #[test]
    fn save_load_is_a_noop_round_trip() {
        let tms = Tms5220::new();
        let mut w = StateWriter::new();
        tms.save_state(&mut w);
        assert!(w.into_vec().is_empty(), "stateless stub writes nothing");

        let mut tms2 = Tms5220::new();
        let mut r = StateReader::new(&[]);
        tms2.load_state(&mut r).unwrap();
    }
}
