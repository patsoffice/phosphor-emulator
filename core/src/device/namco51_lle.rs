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
