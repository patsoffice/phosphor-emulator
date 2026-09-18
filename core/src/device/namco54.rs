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
/// Three ladders, fed from two ports:
///
/// ```text
/// O port, low nibble  (pins 7-4)   → the 150k leg, the 168 Hz filter
/// O port, high nibble (pins 11-8)  → the 47k leg, the 452 Hz filter
/// R port #1, R7-R4    (pins 20-17) → the 100k leg, the 2.5 kHz filter
/// ```
///
/// The O port is **two independently latched nibbles**, not one: `OUTO` writes
/// the accumulator to the low nibble or the high one according to the carry
/// flag, so each holds while the other is written. That is a documented mode of
/// the part rather than a trick, which is why one instruction can drive two
/// ladders. This model keeps them as two latches and takes bit 4 of the raw
/// `OUTO` value, which is the carry, as the selector.
///
/// Which port feeds which ladder is read from the MB8844's package pinout laid
/// over the pin numbers on the Xevious sheet; both the pinout and the nibble
/// rule are transcribed in `docs/schematics/namco-54xx-explosion.md`. See
/// [`Namco54Lle::channels`], which reports the three in ladder order rather
/// than port order.
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
    /// Machine cycles left holding the interrupt line down for this command.
    #[save(id = 5)]
    irq_hold: u8,
}

/// How long the board holds the 54XX's interrupt line down after a command.
///
/// The 06XX's interface clock is 64H, which is 18.432 MHz over 6 over 64, or
/// 48 kHz, so one of its cycles is about 21 us. At the MCU's 256 kHz machine
/// cycle that is five, and it has to be long enough for the firmware to see
/// the line with `TSTI` before it is released.
const IRQ_HOLD_CYCLES: u8 = 5;

impl Namco54Lle {
    pub fn new() -> Self {
        Self {
            mcu: Mb88xx::new(Mb88xxVariant::Mb8844),
            latched_cmd: 0,
            channels: [0; 3],
            last_o_seq: 0,
            irq_hold: 0,
        }
    }

    /// Load the 54XX firmware ROM (1024 bytes, `54xx.bin`).
    pub fn load_rom(&mut self, data: &[u8]) {
        self.mcu.load_rom(data);
    }

    /// Accept a command byte from the Z80 through the 06XX.
    ///
    /// The nibbles land on two different ports because the MB8844's K port is
    /// four bits wide, and the interrupt line is asserted here and released
    /// [`IRQ_HOLD_CYCLES`] later, the way the board's 06XX drives it. The hold
    /// is what lets the firmware see the line with `TSTI`, which it polls
    /// inside its own handler rather than relying on the vector alone.
    ///
    /// Which edge of that pulse the MCU latches on is a known discrepancy
    /// against the reference; see [`Mb88xx::set_irq`]. It does not change this
    /// chip's sound either way.
    pub fn write(&mut self, data: u8) {
        self.latched_cmd = data;
        self.mcu.set_k(data >> 4);
        self.mcu.set_r_input(0, data & 0x0F);
        self.mcu.set_irq(true);
        self.irq_hold = IRQ_HOLD_CYCLES;
    }

    /// Advance the MCU by one machine cycle and latch whatever it put on its
    /// output ports.
    pub fn tick(&mut self) {
        // Release the line once the hold has run out, so the next command can
        // pulse it again. The hold is what gives the firmware time to see the
        // line with `TSTI` while it finishes the routine it was in.
        if self.irq_hold > 0 {
            self.irq_hold -= 1;
            if self.irq_hold == 0 {
                self.mcu.set_irq(false);
            }
        }

        self.mcu.execute_cycle();

        // R-Port #1 is R7-R4, pins 20-17, and drives the ladder with the 100k
        // series resistor, which is the highest of the three filters.
        self.channels[2] = self.mcu.read_r_output(1) & 0x0F;

        // Channels 1 and 2 are the O port's two nibbles, O3-O0 on pins 7-4 and
        // O7-O4 on pins 11-8. `OUTO` writes the accumulator to one of them
        // according to the carry, which arrives here as bit 4, so each write
        // updates one channel and the other holds its last value. That is why
        // these are latches rather than reads.
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

    /// The three channel codes **in ladder order**: the 100k series leg first,
    /// then the 47k, then the 150k, matching the order
    /// `docs/schematics/namco-54xx-explosion.md` tabulates them.
    ///
    /// The ports do not arrive in that order, and that is the whole hazard. The
    /// sheet shows three groups of four output pins and names none of them,
    /// because the 54XX is drawn as a custom block with pin numbers; the groups
    /// run in order down the package (R7-R4, then O7-O4, then O3-O0) while the
    /// ladders they feed do not (100k, then 47k, then 150k). Reading the
    /// MB8844's pinout against those pin numbers is what settles it: pins 20-17
    /// are R-Port #1 on the 100k leg, pins 11-8 the O port's high nibble on the
    /// 47k, pins 7-4 its low nibble on the 150k.
    ///
    /// **Getting this backwards is most of what a wrong explosion sounds like.**
    /// It puts the busy, loud O-port channel through the 2.5 kHz filter and
    /// leaves the mostly-idle R1 port driving the 168 Hz one, so the sound
    /// comes out thin and high with no body, while every register in the chip
    /// reads correctly.
    pub fn channels(&self) -> [u8; 3] {
        [self.channels[2], self.channels[1], self.channels[0]]
    }

    /// Reset the MCU to power-on state. ROM content is preserved.
    pub fn reset(&mut self) {
        self.mcu.reset();
        self.latched_cmd = 0;
        self.channels = [0; 3];
        self.last_o_seq = 0;
        self.irq_hold = 0;
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
        // Ladder order, matching `channels`, so a debugger row and the mixer
        // are talking about the same leg.
        for (i, code) in self.channels().iter().enumerate() {
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
    fn a_command_holds_the_interrupt_line_then_releases_it() {
        // The board pulses this line rather than holding it: asserted on the
        // write, released about 21 us later. Dropping the hold would leave the
        // firmware's `TSTI` poll with nothing to see.
        let mut c = chip();
        assert_eq!(c.mcu.irq_pin, 0);
        c.write(0x10);
        assert_ne!(c.mcu.irq_pin, 0, "the write should assert the line");
        for _ in 0..IRQ_HOLD_CYCLES - 1 {
            c.tick();
            assert_ne!(c.mcu.irq_pin, 0, "released early");
        }
        c.tick();
        assert_eq!(c.mcu.irq_pin, 0, "the line should be back up by now");
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
    fn the_o_port_drives_the_two_lower_ladders_and_r1_the_top_one() {
        // `channels` reports in ladder order, 100k series first. The O port's
        // two halves are the 150k and 47k legs, the low and middle filters,
        // and R1 is the 100k leg at the top. Wiring these in port order
        // instead puts the busy channels through the 2.5 kHz filter and leaves
        // the quiet one driving 168 Hz, which sounds thin with nothing wrong
        // anywhere a register can show it.
        let mut c = chip();
        outo(&mut c, false, 5);
        assert_eq!(
            c.channels(),
            [0, 0, 5],
            "O with bit 4 clear is the 150k leg"
        );
        outo(&mut c, true, 0xC);
        assert_eq!(
            c.channels(),
            [0, 0xC, 5],
            "O with bit 4 set is the 47k leg, and must not disturb the other"
        );
        c.mcu.r_output[1] = 0x9;
        c.tick();
        assert_eq!(c.channels(), [9, 0xC, 5], "R1 is the 100k leg");
    }

    #[test]
    fn the_top_ladder_comes_off_its_own_port() {
        let mut c = chip();
        c.mcu.r_output[1] = 0x09;
        c.tick();
        assert_eq!(c.channels()[0], 9, "R1 is the 100k leg, reported first");
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
