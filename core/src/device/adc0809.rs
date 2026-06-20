//! National Semiconductor ADC0808/ADC0809 — 8-channel 8-bit successive-
//! approximation A/D converter with an analog multiplexer.
//!
//! The host selects one of eight input channels (ADD A/B/C) and pulses START;
//! after the conversion the 8-bit result is read from the data bus and the chip
//! asserts EOC. On the real part a conversion takes ~64 clocks; this model
//! samples the selected channel immediately on START (the result is stable by
//! the time software reads it), which matches observable behavior for the
//! polled use in Atari I, Robot (analog flight stick on channels 0/1).
//!
//! Mirrors MAME `src/devices/machine/adc0808.cpp`. Generic and reusable: the
//! consumer feeds channel values via [`set_input`](Adc0809::set_input) and wires
//! [`address_offset_start_w`](Adc0809::address_offset_start_w) / [`data_r`](Adc0809::data_r)
//! to the bus.

use phosphor_macros::Saveable;

/// 8-channel ADC0808/ADC0809.
#[derive(Saveable)]
#[save_version(1)]
pub struct Adc0809 {
    /// Analog input value per channel (host-provided).
    inputs: [u8; 8],
    /// Selected multiplexer channel (0-7).
    address: u8,
    /// Successive-approximation result register (last conversion).
    sar: u8,
}

impl Default for Adc0809 {
    fn default() -> Self {
        Self::new()
    }
}

impl Adc0809 {
    pub fn new() -> Self {
        Self {
            inputs: [0; 8],
            address: 0,
            sar: 0xff,
        }
    }

    /// Reset the latches. Conversion result powers up at 0xFF (MAME).
    pub fn reset(&mut self) {
        self.address = 0;
        self.sar = 0xff;
    }

    /// Set the analog value present on input channel `channel` (0-7).
    pub fn set_input(&mut self, channel: usize, value: u8) {
        self.inputs[channel & 7] = value;
    }

    /// Select the channel and start a conversion (MAME `address_offset_start_w`):
    /// `offset` is the channel address. The result is sampled immediately.
    pub fn address_offset_start_w(&mut self, offset: u16) {
        self.address = (offset & 7) as u8;
        self.sar = self.inputs[self.address as usize];
    }

    /// Read the conversion result.
    pub fn data_r(&self) -> u8 {
        self.sar
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_selected_channel() {
        let mut adc = Adc0809::new();
        adc.set_input(0, 0x40);
        adc.set_input(1, 0xC8);
        adc.address_offset_start_w(0);
        assert_eq!(adc.data_r(), 0x40);
        adc.address_offset_start_w(1);
        assert_eq!(adc.data_r(), 0xC8);
    }

    #[test]
    fn channel_address_masks_to_three_bits() {
        let mut adc = Adc0809::new();
        adc.set_input(2, 0x55);
        // offset 0x0A (mirror bits set) still selects channel 2.
        adc.address_offset_start_w(0x0A);
        assert_eq!(adc.data_r(), 0x55);
    }

    #[test]
    fn powers_up_at_ff() {
        let adc = Adc0809::new();
        assert_eq!(adc.data_r(), 0xff);
    }
}
