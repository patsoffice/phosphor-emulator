use crate::core::debug::{DebugRegister, Debuggable};
use crate::cpu::mb88xx::{Mb88xx, Mb88xxVariant};
use phosphor_macros::Saveable;

/// Namco 51XX custom chip — LLE (low-level emulation) using MB8843 MCU.
///
/// Runs the actual 51XX firmware ROM on an emulated MB8843, replacing the
/// HLE behavioral model. The MCU handles coin counting, credit management,
/// joystick remapping, and input multiplexing autonomously.
///
/// I/O port wiring (active-low from cabinet switches):
///
/// ```text
/// K port ← data from 06XX (command/data writes from Z80)
/// R0 ← IN0[3:0] (P1 joystick: Left, Down, Right, Up)
/// R1 ← IN0[7:4] (P2 joystick: Left, Down, Right, Up)
/// R2 ← IN1[3:0] (P1 Fire, P2 Fire, Start1, Start2)
/// R3 ← IN1[7:4] (Coin1, Coin2, Service, Test)
/// O port → data to 06XX (read responses to Z80)
/// ```
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct Namco51Lle {
    /// The MB8843 MCU running the 51XX firmware.
    #[save(id = 1)]
    pub mcu: Mb88xx,
}

impl Namco51Lle {
    pub fn new() -> Self {
        Self {
            mcu: Mb88xx::new(Mb88xxVariant::Mb8843),
        }
    }

    /// Load the 51XX firmware ROM (1024 bytes).
    pub fn load_rom(&mut self, data: &[u8]) {
        self.mcu.load_rom(data);
    }

    /// Update cabinet input port values on the MCU's R ports.
    /// Call this each MCU tick (or before reading) to keep inputs current.
    ///
    /// `in0` and `in1` are the raw active-low input port bytes.
    pub fn update_inputs(&mut self, in0: u8, in1: u8) {
        self.mcu.set_r_input(0, in0 & 0x0F); // P1 joystick
        self.mcu.set_r_input(1, (in0 >> 4) & 0x0F); // P2 joystick
        self.mcu.set_r_input(2, in1 & 0x0F); // fire/start buttons
        self.mcu.set_r_input(3, (in1 >> 4) & 0x0F); // coins/test
    }

    /// Advance the MCU by one machine cycle (call at 256 kHz rate).
    pub fn tick(&mut self) {
        self.mcu.execute_cycle();
    }

    /// Read the O port output (response data for the Z80 via 06XX).
    pub fn read(&self) -> u8 {
        self.mcu.read_o()
    }

    /// Write command/data to the shared O port register (port_o).
    /// Called when the Z80 writes to the 06XX data port with chip 0 selected.
    ///
    /// Matches MAME's namco_51xx::write() which stores data in m_portO —
    /// a shared register that the MCU reads back via K port (through K_r
    /// callback) and that the Z80 reads via read(). Only port_o is written,
    /// not the internal o_latch (which is the MCU's own OUTO output).
    pub fn write(&mut self, data: u8) {
        self.mcu.port_o = data;
    }

    /// Reset the MCU to power-on state. ROM content is preserved.
    pub fn reset(&mut self) {
        self.mcu.reset();
    }
}

impl Default for Namco51Lle {
    fn default() -> Self {
        Self::new()
    }
}

impl super::Device for Namco51Lle {
    fn name(&self) -> &'static str {
        "Namco 51XX (LLE)"
    }
    fn reset(&mut self) {
        self.reset();
    }
}

impl Debuggable for Namco51Lle {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        self.mcu.debug_registers()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- The port wiring, which is this wrapper's whole job ---------------
    //
    // The 51XX itself is an MB8843 running its own firmware, and that CPU has
    // its own tests. What lives here is the mapping from two cabinet input
    // bytes onto four 4-bit R ports, and the shared O register the Z80 and the
    // MCU pass bytes through. A transposition in either is invisible in a
    // running game except as inputs that do the wrong thing.

    #[test]
    fn the_two_input_bytes_split_into_four_nibbles_in_order() {
        let mut c = Namco51Lle::new();
        c.update_inputs(0x21, 0x43);
        assert_eq!(c.mcu.r_input[0], 0x1, "IN0 low: P1 joystick");
        assert_eq!(c.mcu.r_input[1], 0x2, "IN0 high: P2 joystick");
        assert_eq!(c.mcu.r_input[2], 0x3, "IN1 low: fire and start");
        assert_eq!(c.mcu.r_input[3], 0x4, "IN1 high: coins and test");
    }

    #[test]
    fn each_nibble_is_masked_to_four_bits() {
        // The R ports are four bits wide. A byte leaking through would put
        // cabinet switches on lines the MCU reads as something else.
        let mut c = Namco51Lle::new();
        c.update_inputs(0xFF, 0xFF);
        for port in 0..4 {
            assert_eq!(c.mcu.r_input[port], 0x0F, "port {port}");
        }
    }

    #[test]
    fn the_inputs_are_independent_of_each_other() {
        // Walk one bit at a time across both bytes and check it lands on
        // exactly one line of one port. This is what catches a transposition
        // that a single all-ones write cannot.
        let mut c = Namco51Lle::new();
        for bit in 0..16u32 {
            let (in0, in1) = if bit < 8 {
                (1u8 << bit, 0u8)
            } else {
                (0u8, 1u8 << (bit - 8))
            };
            c.update_inputs(in0, in1);
            let port = (bit / 4) as usize;
            let line = 1u8 << (bit % 4);
            for p in 0..4 {
                let want = if p == port { line } else { 0 };
                assert_eq!(c.mcu.r_input[p], want, "bit {bit} showed up on port {p}");
            }
        }
    }

    #[test]
    fn inputs_are_resampled_on_every_update_rather_than_latched() {
        let mut c = Namco51Lle::new();
        c.update_inputs(0xFF, 0xFF);
        c.update_inputs(0x00, 0x00);
        for port in 0..4 {
            assert_eq!(c.mcu.r_input[port], 0, "port {port} held a stale value");
        }
    }

    // --- The shared O register --------------------------------------------

    #[test]
    fn a_write_lands_where_the_mcu_reads_it_back() {
        // The Z80 writes and the MCU reads the same register; this is the
        // whole command path into the chip. Writing the MCU's own OUTO latch
        // instead would leave the command where nothing looks for it.
        let mut c = Namco51Lle::new();
        c.write(0x37);
        assert_eq!(c.mcu.port_o, 0x37);
        assert_eq!(c.read(), 0x37, "and the Z80 reads the same register back");
    }

    #[test]
    fn a_write_replaces_the_previous_command() {
        let mut c = Namco51Lle::new();
        c.write(0x01);
        c.write(0x02);
        assert_eq!(c.read(), 0x02);
    }

    // --- Reset --------------------------------------------------------------

    #[test]
    fn reset_returns_the_mcu_to_power_on() {
        let mut c = Namco51Lle::new();
        // A ROM of NOPs, so stepping is well defined without the real
        // firmware, which is not redistributable and not needed here.
        c.load_rom(&[0u8; 1024]);
        c.update_inputs(0xFF, 0xFF);
        c.write(0x5A);
        // Ten, not sixty-four: the MB88xx program counter is six bits wide
        // within a page, so a multiple of 64 single-cycle instructions wraps it
        // back to zero and the fixture would look like it had never run.
        for _ in 0..10 {
            c.tick();
        }
        assert_ne!(c.mcu.pc, 0, "the fixture did not actually run");

        c.reset();
        assert_eq!(c.mcu.pc, 0, "reset did not return the MCU to its entry");
        assert_eq!(
            c.read(),
            0,
            "reset left a stale command in the shared O register, which the \
             MCU would read as the Z80's first word after power-on"
        );
    }

    #[test]
    fn reset_keeps_the_firmware_rom() {
        // The ROM is a mask inside the package. A reset line does not erase
        // it, and a reset that did would leave the chip executing zeroes with
        // nothing able to reload it.
        let mut c = Namco51Lle::new();
        let mut rom = [0u8; 1024];
        rom[0] = 0xAB;
        rom[1023] = 0xCD;
        c.load_rom(&rom);
        c.reset();
        assert_eq!(c.mcu.peek_rom(0), 0xAB);
        assert_eq!(c.mcu.peek_rom(1023), 0xCD);
    }
}
