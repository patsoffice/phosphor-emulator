use crate::core::debug::{DebugRegister, Debuggable};
use crate::cpu::mb88xx::{Mb88xx, Mb88xxVariant};
use phosphor_macros::Saveable;

/// Namco 54XX custom chip: an MB8844 running the explosion-sound firmware.
///
/// The chip is a noise and envelope generator. It takes one command byte from
/// the Z80 through the 06XX and drives three four-bit output channels, each of
/// which meets a binary-weighted resistor ladder and a band-pass filter on the
/// board. The analog side is transcribed in
/// `docs/schematics/namco-54xx-explosion.md` and modeled in
/// `machines/src/namco_wsg_output.rs`; what lives here is the MCU and its port
/// wiring.
///
/// # The command path
///
/// A write splits across two of the MCU's input ports and raises its interrupt:
///
/// ```text
/// K  ← the command's high nibble
/// R0 ← the command's low nibble
/// IRQ asserted, so the firmware services the write rather than polling
/// ```
///
/// # The output ports
///
/// Twelve pins in three groups of four, matching the three ladders:
///
/// ```text
/// O[3:0] → channel 1
/// O[7:4] → channel 2
/// R1     → channel 3
/// ```
///
/// The O port is eight pins driving two ladders at once, not one port shared in
/// time. `OUTO` writes a nibble at a time with the carry choosing the half, so
/// each channel holds while the other is written and nothing here needs to
/// latch it.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct Namco54Lle {
    /// The MB8844 MCU running the 54XX firmware.
    #[save(id = 1)]
    pub mcu: Mb88xx,
    /// The last command byte written through the 06XX.
    #[save(id = 2)]
    latched_cmd: u8,
    /// The three channel codes as the ladders see them: each holds its last
    /// value until the firmware writes that channel again.
    #[save(id = 3)]
    channels: [u8; 3],
    /// The `OUTO` count last seen, so a fresh write is told from the same value
    /// still sitting on the port.
    #[save(id = 4)]
    last_o_seq: u32,
}

impl Namco54Lle {
    pub fn new() -> Self {
        Self {
            mcu: Mb88xx::new(Mb88xxVariant::Mb8844),
            latched_cmd: 0,
            channels: [0; 3],
            last_o_seq: 0,
        }
    }

    /// Load the 54XX firmware ROM (1024 bytes, `54xx.bin`).
    pub fn load_rom(&mut self, data: &[u8]) {
        self.mcu.load_rom(data);
    }

    /// Accept a command byte from the Z80 through the 06XX.
    ///
    /// The nibbles land on two different ports because the MB8844's K port is
    /// four bits wide. Nothing here raises the interrupt: see
    /// [`set_chip_select`](Self::set_chip_select) for why the firmware needs a
    /// held line rather than an edge.
    pub fn write(&mut self, data: u8) {
        self.latched_cmd = data;
        self.mcu.set_k(data >> 4);
        self.mcu.set_r_input(0, data & 0x0F);
    }

    /// Follow the 06XX's chip-select line into the MCU's interrupt pin.
    ///
    /// **The firmware polls this pin rather than taking an interrupt from it.**
    /// It disables interrupts and sits in a two-instruction loop, `TSTI` then a
    /// conditional jump back, until the line reads high; only then does it
    /// fetch the command and start a sound. So the line has to be *held* for
    /// as long as the 06XX asserts it, and a one-cycle pulse on a write is
    /// invisible: the chip stays in that loop forever, running, with every
    /// register looking healthy.
    pub fn set_chip_select(&mut self, asserted: bool) {
        self.mcu.set_irq(asserted);
    }

    /// Advance the MCU by one machine cycle and latch whatever it put on its
    /// output ports.
    pub fn tick(&mut self) {
        self.mcu.execute_cycle();

        // Channel 3 is its own port and can simply be read.
        self.channels[2] = self.mcu.read_r_output(1) & 0x0F;

        // Channels 1 and 2 share the O port in time: `OUTO` puts the level on
        // the low four bits and the carry on bit 4, and bit 4 says which of the
        // two ladders the level is for. So each write updates one channel and
        // the other holds, which is why these are latches here.
        //
        // This has to read the raw port write rather than the MCU's eight-bit
        // O register. That register is kept the way the *51XX* wants it, with
        // one nibble filled per write, and a byte assembled that way carries
        // neither channel's value.
        if self.mcu.o_pla_seq != self.last_o_seq {
            self.last_o_seq = self.mcu.o_pla_seq;
            let v = self.mcu.o_pla;
            let which = usize::from(v & 0x10 != 0);
            self.channels[which] = v & 0x0F;
        }
    }

    /// The three channel codes, in the order the schematic numbers the ladders.
    pub fn channels(&self) -> [u8; 3] {
        self.channels
    }

    /// Reset the MCU to power-on state. ROM content is preserved.
    pub fn reset(&mut self) {
        self.mcu.reset();
        self.latched_cmd = 0;
        self.channels = [0; 3];
        self.last_o_seq = 0;
    }
}

impl Default for Namco54Lle {
    fn default() -> Self {
        Self::new()
    }
}

impl super::Device for Namco54Lle {
    fn name(&self) -> &'static str {
        "Namco 54XX (LLE)"
    }
    fn reset(&mut self) {
        self.reset();
    }
    fn write(&mut self, _offset: u16, data: u8) {
        self.write(data);
    }
}

impl Debuggable for Namco54Lle {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        let mut regs = self.mcu.debug_registers();
        regs.push(DebugRegister {
            name: "CMD",
            value: self.latched_cmd as u64,
            width: 8,
        });
        for (i, code) in self.channels.iter().enumerate() {
            regs.push(DebugRegister {
                name: ["EXPL0", "EXPL1", "EXPL2"][i],
                value: *code as u64,
                width: 4,
            });
        }
        regs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ROM of NOPs, so the port wiring can be asserted without the firmware,
    /// which is not redistributable.
    fn chip() -> Namco54Lle {
        let mut c = Namco54Lle::new();
        c.load_rom(&[0u8; 1024]);
        c
    }

    // --- The command path -------------------------------------------------

    #[test]
    fn a_command_splits_across_two_ports_high_nibble_first() {
        // The K port is four bits wide, so a byte cannot arrive on it. Getting
        // the halves the wrong way round would leave the firmware reading a
        // command it never received, silently.
        let mut c = chip();
        c.write(0x7A);
        assert_eq!(c.mcu.k_input, 0x7, "K takes the high nibble");
        assert_eq!(c.mcu.r_input[0], 0xA, "R0 takes the low nibble");
    }

    #[test]
    fn the_chip_select_line_reaches_the_interrupt_pin_and_is_held() {
        // The firmware vectors off this line and then polls it with `TSTI`
        // inside the handler, so it has to be a level the board holds, not an
        // edge synthesized on a write. Pulsing it instead leaves the chip
        // spinning in that loop forever, running, with every register healthy.
        let mut c = chip();
        assert_eq!(c.mcu.irq_pin, 0);
        c.write(0x10);
        assert_eq!(c.mcu.irq_pin, 0, "a write alone must not assert it");
        c.set_chip_select(true);
        assert_ne!(c.mcu.irq_pin, 0);
        c.set_chip_select(false);
        assert_eq!(c.mcu.irq_pin, 0);
    }

    // --- The output latches ------------------------------------------------

    /// One `OUTO` write, standing in for the instruction: the level on the low
    /// four bits and the carry on bit 4.
    fn outo(c: &mut Namco54Lle, carry: bool, level: u8) {
        c.mcu.o_pla = (u8::from(carry) << 4) | (level & 0x0F);
        c.mcu.o_pla_seq = c.mcu.o_pla_seq.wrapping_add(1);
        c.tick();
    }

    #[test]
    fn bit_four_of_an_o_write_selects_which_channel_it_is_for() {
        // Channels 1 and 2 share the port in time, and the carry says which one
        // the level belongs to. Treating the byte as two nibbles instead gives
        // both ladders a number neither channel ever had.
        let mut c = chip();
        outo(&mut c, false, 5);
        assert_eq!(c.channels(), [5, 0, 0]);
        outo(&mut c, true, 0xC);
        assert_eq!(
            c.channels(),
            [5, 0xC, 0],
            "writing channel 2 must not disturb channel 1"
        );
        outo(&mut c, false, 3);
        assert_eq!(c.channels(), [3, 0xC, 0]);
    }

    #[test]
    fn channel_three_comes_off_its_own_port() {
        let mut c = chip();
        c.mcu.r_output[1] = 0x09;
        c.tick();
        assert_eq!(c.channels()[2], 9);
    }

    // --- Reset --------------------------------------------------------------

    #[test]
    fn reset_silences_every_channel_and_returns_the_mcu_to_its_entry() {
        let mut c = chip();
        c.write(0x5A);
        c.mcu.r_output[1] = 0x0F;
        outo(&mut c, false, 0x0F);
        for _ in 0..10 {
            c.tick();
        }
        assert_ne!(c.mcu.pc, 0, "the fixture did not actually run");
        assert_ne!(c.channels(), [0; 3]);

        c.reset();
        assert_eq!(c.mcu.pc, 0);
        assert_eq!(
            c.channels(),
            [0; 3],
            "a reset that left a channel driven would hold a tone through it"
        );
    }

    #[test]
    fn reset_keeps_the_firmware_rom() {
        let mut c = Namco54Lle::new();
        let mut rom = [0u8; 1024];
        rom[0] = 0xAB;
        rom[1023] = 0xCD;
        c.load_rom(&rom);
        c.reset();
        assert_eq!(c.mcu.peek_rom(0), 0xAB);
        assert_eq!(c.mcu.peek_rom(1023), 0xCD);
    }
}
