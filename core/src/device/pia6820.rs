use phosphor_macros::Saveable;

/// MC6821 Peripheral Interface Adapter (PIA)
///
/// The 6820 and 6821 are register-compatible. This implementation covers
/// the full register set: data direction registers, data ports, control
/// registers, interrupt flags, and edge-detected control line inputs.
///
/// Each side (A and B) has:
/// - A data port connected to external hardware
/// - An output register (ORA/ORB) latching CPU writes
/// - A data direction register (DDRA/DDRB): 0=input, 1=output per bit
/// - A control register (CRA/CRB) controlling interrupts and register selection
/// - Two control/interrupt lines (CA1/CA2 or CB1/CB2)
///
/// Register addressing uses RS1:RS0 (2 bits = 4 locations), with CRx bit 2
/// selecting between DDR and data register at offsets 0 and 2.
#[derive(Saveable)]
#[save_version(1)]
pub struct Pia6820 {
    // Port A
    output_a: u8, // Output Register A (ORA) — written by CPU
    ddr_a: u8,    // Data Direction Register A (0=input, 1=output)
    ctrl_a: u8,   // Control Register A (CRA), bits 5:0 only
    input_a: u8,  // External input pins (set by board logic)

    // Port B
    output_b: u8, // Output Register B (ORB) — written by CPU
    ddr_b: u8,    // Data Direction Register B
    ctrl_b: u8,   // Control Register B (CRB), bits 5:0 only
    input_b: u8,  // External input pins (set by board logic)

    // Interrupt flags (bits 7 and 6 of control registers, stored separately)
    irq_a1: bool, // Set by CA1 transition
    irq_a2: bool, // Set by CA2 transition (when CA2 is input)
    irq_b1: bool, // Set by CB1 transition
    irq_b2: bool, // Set by CB2 transition (when CB2 is input)

    // Control line current state (used for edge detection)
    ca1: bool,
    ca2: bool,
    cb1: bool,
    cb2: bool,

    // Write notification (for inter-board communication)
    port_b_written: bool, // Set when CPU writes to Port B data register
}

impl Pia6820 {
    /// Create a new PIA with all registers zeroed (all pins input, no interrupts).
    pub fn new() -> Self {
        Self {
            output_a: 0,
            ddr_a: 0,
            ctrl_a: 0,
            input_a: 0,

            output_b: 0,
            ddr_b: 0,
            ctrl_b: 0,
            input_b: 0,

            irq_a1: false,
            irq_a2: false,
            irq_b1: false,
            irq_b2: false,

            ca1: false,
            ca2: false,
            cb1: false,
            cb2: false,

            port_b_written: false,
        }
    }

    /// Read from PIA register. `offset` is RS1:RS0 (0-3).
    ///
    /// | Offset | CRx.2 | Register            |
    /// |--------|-------|---------------------|
    /// | 0      | 0     | DDRA                |
    /// | 0      | 1     | Port A data         |
    /// | 1      | x     | CRA                 |
    /// | 2      | 0     | DDRB                |
    /// | 2      | 1     | Port B data         |
    /// | 3      | x     | CRB                 |
    ///
    /// Reading a data port clears both IRQ flags for that side.
    pub fn read(&mut self, offset: u16) -> u8 {
        match offset & 0x03 {
            0 => {
                if (self.ctrl_a & 0x04) != 0 {
                    // Data register: input pins where DDR=0, output register where DDR=1
                    self.irq_a1 = false;
                    self.irq_a2 = false;
                    (self.input_a & !self.ddr_a) | (self.output_a & self.ddr_a)
                } else {
                    self.ddr_a
                }
            }
            1 => {
                // CRA: bits 7-6 are read-only interrupt flags
                let flags = ((self.irq_a1 as u8) << 7) | ((self.irq_a2 as u8) << 6);
                flags | (self.ctrl_a & 0x3F)
            }
            2 => {
                if (self.ctrl_b & 0x04) != 0 {
                    self.irq_b1 = false;
                    self.irq_b2 = false;
                    (self.input_b & !self.ddr_b) | (self.output_b & self.ddr_b)
                } else {
                    self.ddr_b
                }
            }
            3 => {
                let flags = ((self.irq_b1 as u8) << 7) | ((self.irq_b2 as u8) << 6);
                flags | (self.ctrl_b & 0x3F)
            }
            _ => unreachable!(),
        }
    }

    /// Write to PIA register. `offset` is RS1:RS0 (0-3).
    ///
    /// Writing to a data port stores the value in ORA/ORB. Only bits where
    /// the corresponding DDR bit is 1 actually drive the output pins.
    /// Writing to a control register only affects bits 5:0 (bits 7:6 are
    /// read-only interrupt flags).
    pub fn write(&mut self, offset: u16, data: u8) {
        match offset & 0x03 {
            0 => {
                if (self.ctrl_a & 0x04) != 0 {
                    self.output_a = data;
                } else {
                    self.ddr_a = data;
                }
            }
            1 => {
                self.ctrl_a = data & 0x3F;
            }
            2 => {
                if (self.ctrl_b & 0x04) != 0 {
                    self.output_b = data;
                    self.port_b_written = true;
                } else {
                    self.ddr_b = data;
                }
            }
            3 => {
                self.ctrl_b = data & 0x3F;
            }
            _ => unreachable!(),
        }
    }

    /// Set external input pins for Port A (called by board/system logic).
    pub fn set_port_a_input(&mut self, data: u8) {
        self.input_a = data;
    }

    /// Set external input pins for Port B (called by board/system logic).
    pub fn set_port_b_input(&mut self, data: u8) {
        self.input_b = data;
    }

    /// Update CA1 control line. Performs edge detection and may set irq_a1.
    ///
    /// CA1 is always an input. CRA bit 1 selects the active edge:
    /// - Bit 1 = 0: flag set on falling edge
    /// - Bit 1 = 1: flag set on rising edge
    pub fn set_ca1(&mut self, state: bool) {
        let rising = state && !self.ca1;
        let falling = !state && self.ca1;
        self.ca1 = state;

        let trigger_on_rising = (self.ctrl_a & 0x02) != 0;
        if (trigger_on_rising && rising) || (!trigger_on_rising && falling) {
            self.irq_a1 = true;
        }
    }

    /// Update CB1 control line. Performs edge detection and may set irq_b1.
    ///
    /// CB1 is always an input. CRB bit 1 selects the active edge:
    /// - Bit 1 = 0: flag set on falling edge
    /// - Bit 1 = 1: flag set on rising edge
    pub fn set_cb1(&mut self, state: bool) {
        let rising = state && !self.cb1;
        let falling = !state && self.cb1;
        self.cb1 = state;

        let trigger_on_rising = (self.ctrl_b & 0x02) != 0;
        if (trigger_on_rising && rising) || (!trigger_on_rising && falling) {
            self.irq_b1 = true;
        }
    }

    /// Update CA2 control line (when configured as input). May set irq_a2.
    ///
    /// Only triggers when CA2 is configured as input (CRA bit 5 = 0).
    /// CRA bit 4 selects the active edge:
    /// - Bit 4 = 0: flag set on falling edge
    /// - Bit 4 = 1: flag set on rising edge
    pub fn set_ca2(&mut self, state: bool) {
        if (self.ctrl_a & 0x20) != 0 {
            return; // CA2 is output mode, ignore
        }

        let rising = state && !self.ca2;
        let falling = !state && self.ca2;
        self.ca2 = state;

        let trigger_on_rising = (self.ctrl_a & 0x10) != 0;
        if (trigger_on_rising && rising) || (!trigger_on_rising && falling) {
            self.irq_a2 = true;
        }
    }

    /// Update CB2 control line (when configured as input). May set irq_b2.
    ///
    /// Only triggers when CB2 is configured as input (CRB bit 5 = 0).
    /// CRB bit 4 selects the active edge:
    /// - Bit 4 = 0: flag set on falling edge
    /// - Bit 4 = 1: flag set on rising edge
    pub fn set_cb2(&mut self, state: bool) {
        if (self.ctrl_b & 0x20) != 0 {
            return; // CB2 is output mode, ignore
        }

        let rising = state && !self.cb2;
        let falling = !state && self.cb2;
        self.cb2 = state;

        let trigger_on_rising = (self.ctrl_b & 0x10) != 0;
        if (trigger_on_rising && rising) || (!trigger_on_rising && falling) {
            self.irq_b2 = true;
        }
    }

    /// Check if IRQA output is asserted.
    ///
    /// IRQA = (irq_a1 AND CRA.0) OR (irq_a2 AND CRA.3 AND NOT CRA.5)
    ///
    /// irq_a2 only contributes when CA2 is input (bit 5=0) and enabled (bit 3=1).
    pub fn irq_a(&self) -> bool {
        let a1_active = self.irq_a1 && (self.ctrl_a & 0x01) != 0;
        let a2_active = self.irq_a2 && (self.ctrl_a & 0x20) == 0 && (self.ctrl_a & 0x08) != 0;
        a1_active || a2_active
    }

    /// Check if IRQB output is asserted.
    ///
    /// IRQB = (irq_b1 AND CRB.0) OR (irq_b2 AND CRB.3 AND NOT CRB.5)
    ///
    /// irq_b2 only contributes when CB2 is input (bit 5=0) and enabled (bit 3=1).
    pub fn irq_b(&self) -> bool {
        let b1_active = self.irq_b1 && (self.ctrl_b & 0x01) != 0;
        let b2_active = self.irq_b2 && (self.ctrl_b & 0x20) == 0 && (self.ctrl_b & 0x08) != 0;
        b1_active || b2_active
    }

    /// Read the current output value of Port A (ORA masked by DDRA).
    ///
    /// Returns only the bits the CPU is actively driving (DDR=1).
    /// Useful for board logic that needs to read what the CPU outputs.
    pub fn read_output_a(&self) -> u8 {
        self.output_a & self.ddr_a
    }

    /// Read the current output value of Port B (ORB masked by DDRB).
    pub fn read_output_b(&self) -> u8 {
        self.output_b & self.ddr_b
    }

    /// Read CA2 output state (when configured as output by CRA bits 5:4:3).
    ///
    /// Mirrors [`cb2_output`](Self::cb2_output) for the Port A control line:
    /// - CRA bits 5:4 = 11: direct control, output = CRA bit 3
    /// - CRA bits 5:4 = 10: handshake/pulse mode, returns stored state
    ///
    /// Returns false if CA2 is configured as input (CRA bit 5 = 0).
    pub fn ca2_output(&self) -> bool {
        if (self.ctrl_a & 0x20) == 0 {
            return false; // CA2 is input
        }
        if (self.ctrl_a & 0x10) != 0 {
            // Direct output mode: output = bit 3
            (self.ctrl_a & 0x08) != 0
        } else {
            // Handshake/pulse mode: return stored state
            self.ca2
        }
    }

    /// Read CB2 output state (when configured as output by CRB bits 5:4:3).
    ///
    /// Returns the driven level when CB2 is in output mode:
    /// - CRB bits 5:4 = 11: direct control, output = CRB bit 3
    /// - CRB bits 5:4 = 10: handshake/pulse mode, returns stored state
    ///
    /// Returns false if CB2 is configured as input (CRB bit 5 = 0).
    pub fn cb2_output(&self) -> bool {
        if (self.ctrl_b & 0x20) == 0 {
            return false; // CB2 is input
        }
        if (self.ctrl_b & 0x10) != 0 {
            // Direct output mode: output = bit 3
            (self.ctrl_b & 0x08) != 0
        } else {
            // Handshake/pulse mode: return stored state
            self.cb2
        }
    }

    /// Check if Port B data register was written since last check.
    /// Clears the flag after reading (one-shot notification).
    ///
    /// Used by board logic to detect when the CPU sends a command
    /// via Port B, so the board can propagate it to another device.
    pub fn take_port_b_written(&mut self) -> bool {
        let was_written = self.port_b_written;
        self.port_b_written = false;
        was_written
    }

    /// Reset the PIA to its power-on state (all registers zeroed, all pins input).
    pub fn reset(&mut self) {
        self.output_a = 0;
        self.ddr_a = 0;
        self.ctrl_a = 0;
        self.input_a = 0;
        self.output_b = 0;
        self.ddr_b = 0;
        self.ctrl_b = 0;
        self.input_b = 0;
        self.irq_a1 = false;
        self.irq_a2 = false;
        self.irq_b1 = false;
        self.irq_b2 = false;
        self.ca1 = false;
        self.ca2 = false;
        self.cb1 = false;
        self.cb2 = false;
        self.port_b_written = false;
    }
}

impl Default for Pia6820 {
    fn default() -> Self {
        Self::new()
    }
}

impl super::Device for Pia6820 {
    fn name(&self) -> &'static str {
        "PIA 6820"
    }
    fn reset(&mut self) {
        self.reset();
    }
    fn read(&mut self, offset: u16) -> u8 {
        self.read(offset)
    }
    fn write(&mut self, offset: u16, data: u8) {
        self.write(offset, data);
    }
}

use crate::core::debug::{DebugRegister, Debuggable};

impl Debuggable for Pia6820 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "ORA",
                value: self.output_a as u64,
                width: 8,
            },
            DebugRegister {
                name: "DDRA",
                value: self.ddr_a as u64,
                width: 8,
            },
            DebugRegister {
                name: "CRA",
                value: self.ctrl_a as u64,
                width: 8,
            },
            DebugRegister {
                name: "IN_A",
                value: self.input_a as u64,
                width: 8,
            },
            DebugRegister {
                name: "ORB",
                value: self.output_b as u64,
                width: 8,
            },
            DebugRegister {
                name: "DDRB",
                value: self.ddr_b as u64,
                width: 8,
            },
            DebugRegister {
                name: "CRB",
                value: self.ctrl_b as u64,
                width: 8,
            },
            DebugRegister {
                name: "IN_B",
                value: self.input_b as u64,
                width: 8,
            },
        ]
    }
}
