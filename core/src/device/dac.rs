use phosphor_macros::Saveable;

#[derive(Saveable)]
#[save_version(1)]
pub struct Mc1408Dac {
    /// Most recent value written by the CPU (0-255 unsigned).
    value: u8,
}

impl Default for Mc1408Dac {
    fn default() -> Self {
        Self { value: 0x80 }
    }
}

impl Mc1408Dac {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called when the sound PIA Port A is written.
    pub fn write(&mut self, data: u8) {
        self.value = data;
    }

    /// Return current output as a signed 16-bit PCM sample.
    /// Maps 0x00 → -32768, 0x80 → 0, 0xFF → +32512.
    pub fn sample_i16(&self) -> i16 {
        ((self.value as i16) - 128) * 256
    }

    /// Reset the DAC to mid-range (silence).
    pub fn reset(&mut self) {
        self.value = 0x80;
    }
}

impl super::Device for Mc1408Dac {
    fn name(&self) -> &'static str {
        "MC1408 DAC"
    }
    fn reset(&mut self) {
        self.reset();
    }
}

use crate::core::debug::{DebugRegister, Debuggable};

impl Debuggable for Mc1408Dac {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![DebugRegister {
            name: "VALUE",
            value: self.value as u64,
            width: 8,
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_powers_up_at_mid_scale_which_is_silence() {
        // An unsigned DAC's silence is its midpoint, not zero. Powering up at
        // 0x00 would slam the output to full negative until the first write.
        let d = Mc1408Dac::new();
        assert_eq!(d.sample_i16(), 0);
    }

    #[test]
    fn the_code_maps_across_the_full_signed_range() {
        let mut d = Mc1408Dac::new();
        for (code, want) in [(0x00u8, -32768i16), (0x80, 0), (0xFF, 32512)] {
            d.write(code);
            assert_eq!(d.sample_i16(), want, "code {code:#04X}");
        }
    }

    #[test]
    fn the_top_of_the_range_falls_short_of_full_scale_by_one_step() {
        // Eight bits around a midpoint are asymmetric: there are 128 codes
        // below it and 127 above. The positive end therefore stops one step
        // short, and that is the part rather than a rounding mistake.
        let mut d = Mc1408Dac::new();
        d.write(0xFF);
        assert_eq!(i32::from(i16::MAX) - i32::from(d.sample_i16()), 255);
    }

    #[test]
    fn every_code_is_one_step_above_the_one_below_it() {
        // Monotonic with a uniform step, which is what makes it a linear DAC.
        let mut d = Mc1408Dac::new();
        let mut previous = None;
        for code in 0..=255u8 {
            d.write(code);
            let s = i32::from(d.sample_i16());
            if let Some(p) = previous {
                assert_eq!(s - p, 256, "step below code {code:#04X}");
            }
            previous = Some(s);
        }
    }

    #[test]
    fn a_write_is_a_level_and_not_a_pulse() {
        // The DAC holds its last code until the next write; nothing decays it.
        let mut d = Mc1408Dac::new();
        d.write(0x20);
        for _ in 0..100 {
            assert_eq!(d.sample_i16(), (0x20 - 128) * 256);
        }
    }

    #[test]
    fn reset_returns_to_silence_rather_than_to_zero() {
        let mut d = Mc1408Dac::new();
        d.write(0x00);
        assert_eq!(d.sample_i16(), -32768);
        d.reset();
        assert_eq!(d.sample_i16(), 0, "reset is mid-scale, not code zero");
    }
}
