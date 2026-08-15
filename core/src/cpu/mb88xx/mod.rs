/// Fujitsu MB88xx series 4-bit microcontroller emulation.
///
/// The MB88xx family includes several variants sharing the same instruction set
/// but differing in ROM and RAM sizes:
///
/// | Variant  | Program ROM | Data RAM     | Used in          |
/// |----------|-------------|--------------|------------------|
/// | MB8843   | 1024 bytes  | 64 nibbles   | Namco 51XX, 53XX |
/// | MB8844   | 1024 bytes  | 64 nibbles   | Namco 54XX       |
/// | MB8842   | 2048 bytes  | 128 nibbles  | Namco 50XX       |
/// | MB8841   | 2048 bytes  | 128 nibbles  | (larger variant) |
///
/// Architecture:
/// - 4-bit accumulator (A), 4-bit index registers (X, Y)
/// - 4-bit scratch register (SB)
/// - Flags: ST (status/test), ZF (zero), CF (carry), VF (overflow), SF (serial full), IF (IRQ pin)
/// - 4-level hardware call stack storing 10-bit PC + flags
/// - Split PC: PA (page address, 4 bits) + PC (offset, 6 bits)
/// - 8-bit instructions, most execute in 1 machine cycle
/// - I/O: K port (4-bit input), O port (8-bit output), R0-R3 (4-bit bidirectional), P (4-bit output)
/// - Timer: TH:TL (8-bit, prescaled), serial shift register SB
///
/// Clock: external clock / 6 = 1 machine cycle. On Namco Galaga hardware:
/// 18.432 MHz / 12 = 1.536 MHz external → 256 kHz machine cycle rate.
pub mod disasm;

use crate::core::debug::{DebugRegister, Debuggable};
use crate::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use crate::cpu::state::CpuStateTrait;

// ---------------------------------------------------------------------------
// Variant configuration
// ---------------------------------------------------------------------------

/// MB88xx chip variant, determining ROM and RAM sizes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mb88xxVariant {
    /// MB8843: 1024-byte ROM (10-bit address), 64-nibble RAM (6-bit address).
    Mb8843,
    /// MB8844: 1024-byte ROM (10-bit address), 64-nibble RAM (6-bit address).
    Mb8844,
    /// MB8842: 2048-byte ROM (11-bit address), 128-nibble RAM (7-bit address).
    Mb8842,
    /// MB8841: 2048-byte ROM (11-bit address), 128-nibble RAM (7-bit address).
    Mb8841,
}

impl Mb88xxVariant {
    pub const fn rom_size(self) -> usize {
        match self {
            Self::Mb8843 | Self::Mb8844 => 1024,
            Self::Mb8842 | Self::Mb8841 => 2048,
        }
    }

    pub const fn rom_mask(self) -> u16 {
        match self {
            Self::Mb8843 | Self::Mb8844 => 0x3FF,
            Self::Mb8842 | Self::Mb8841 => 0x7FF,
        }
    }

    pub const fn ram_size(self) -> usize {
        match self {
            Self::Mb8843 | Self::Mb8844 => 64,
            Self::Mb8842 | Self::Mb8841 => 128,
        }
    }

    pub const fn ram_mask(self) -> u8 {
        match self {
            Self::Mb8843 | Self::Mb8844 => 0x3F,
            Self::Mb8842 | Self::Mb8841 => 0x7F,
        }
    }

    pub const fn pa_mask(self) -> u8 {
        match self {
            Self::Mb8843 | Self::Mb8844 => 0x0F, // 4-bit PA for 10-bit address space
            Self::Mb8842 | Self::Mb8841 => 0x1F, // 5-bit PA for 11-bit address space
        }
    }
}

// ---------------------------------------------------------------------------
// Interrupt causes (bit flags)
// ---------------------------------------------------------------------------

const INT_CAUSE_SERIAL: u8 = 0x01;
const INT_CAUSE_TIMER: u8 = 0x02;
const INT_CAUSE_EXTERNAL: u8 = 0x04;

/// Timer prescaler divisor (timer increments every 32 machine cycles).
const TIMER_PRESCALE: u8 = 32;

// ---------------------------------------------------------------------------
// CPU state
// ---------------------------------------------------------------------------

/// Fujitsu MB88xx 4-bit microcontroller.
pub struct Mb88xx {
    // --- Registers ---
    /// Program counter offset within page (6 bits, 0-63).
    pub pc: u8,
    /// Page address register (4 or 5 bits depending on variant).
    pub pa: u8,
    /// Hardware call stack (4 entries, 10-bit addresses + 3 flag bits in upper bits).
    pub stack: [u16; 4],
    /// Stack index (0-3, points to next free slot).
    pub si: u8,
    /// Accumulator (4 bits).
    pub a: u8,
    /// Index register X (4 bits).
    pub x: u8,
    /// Index register Y (4 bits).
    pub y: u8,
    /// Status/test flag (1 bit). Used for conditional branching.
    pub st: u8,
    /// Zero flag (1 bit). 1 = result was zero, 0 = result was not zero.
    pub zf: u8,
    /// Carry flag (1 bit). Note: inverted sense — 1 means carry DID occur.
    pub cf: u8,
    /// Timer overflow flag (1 bit).
    pub vf: u8,
    /// Serial full/empty flag (1 bit).
    pub sf: u8,
    /// Interrupt pin state (1 bit).
    pub irq_pin: u8,

    // --- Peripheral control ---
    /// PIO enable register (8 bits). Controls which interrupt sources are enabled
    /// and whether serial/timer are active.
    /// Bit 2: external IRQ enable
    /// Bit 1: timer IRQ enable
    /// Bit 0: serial IRQ enable
    /// Bit 4-5: serial mode
    /// Bit 6: external counter enable
    /// Bit 7: internal timer enable
    pub pio: u8,

    // --- Timer ---
    /// Timer high nibble (4 bits).
    pub th: u8,
    /// Timer low nibble (4 bits).
    pub tl: u8,
    /// Timer prescaler (counts up to TIMER_PRESCALE).
    pub tp: u8,
    /// External counter pin state.
    pub ctr: u8,

    // --- Serial ---
    /// Serial buffer (4 bits).
    pub sb: u8,
    /// Serial bit count.
    pub sb_count: u16,

    // --- O port ---
    /// O port internal output register (8 bits, PLA-mapped or direct).
    /// Updated by write_pla (OUTO instruction). Equivalent to MAME's m_o_output.
    pub o_latch: u8,
    /// Shared external O port register. Updated asynchronously from o_latch
    /// after each execute_cycle, matching MAME's m_portO which is updated
    /// via O_w_sync between scheduler timeslices. Z80 reads/writes go through
    /// this register. K port reads also use this in dynamic_k mode.
    pub port_o: u8,

    // --- Interrupt state ---
    /// Pending interrupt causes (bit flags).
    pub pending_irq: u8,
    /// Currently inside an IRQ handler.
    pub in_irq: bool,

    // --- Internal memory ---
    /// Program ROM (1024 or 2048 bytes).
    rom: Vec<u8>,
    /// Data RAM (64 or 128 nibbles, stored as bytes with upper 4 bits unused).
    ram: Vec<u8>,

    // --- Configuration ---
    /// Chip variant.
    variant: Mb88xxVariant,
    /// ROM address mask.
    rom_mask: u16,
    /// RAM address mask.
    ram_mask: u8,
    /// PA register mask.
    pa_mask: u8,

    // --- I/O port callbacks (external state set by wrapper) ---
    /// K port input (4 bits, active-low, set externally).
    pub k_input: u8,
    /// R port inputs (4 × 4 bits, set externally, read by firmware).
    pub r_input: [u8; 4],
    /// R port outputs (4 × 4 bits, written by firmware, read externally).
    pub r_output: [u8; 4],
    /// P port output (4 bits, written by firmware).
    pub p_output: u8,

    /// Serial input bit (set externally).
    pub si_input: u8,
    /// Serial output bit (written by firmware).
    pub so_output: u8,

    /// R/W input for dynamic K port mode (set externally by 06XX).
    /// When `dynamic_k` is true, INK computes K = (rw_input << 3) | (o_latch & 0x07)
    /// instead of reading the pre-set k_input, matching MAME's K_r() callback
    /// which reads m_portO (= o_latch) at the moment the MCU accesses K.
    pub rw_input: u8,
    /// When true, INK reads K dynamically from rw_input and o_latch.
    /// This matches the 51XX hardware wiring where K3=R/W and K2-K0=portO.
    pub dynamic_k: bool,

    // --- Execution state ---
    /// Whether we are in cycle 2 of a 2-cycle instruction.
    second_cycle: bool,
    /// Opcode being executed (for 2-cycle instructions).
    pending_opcode: u8,
}

impl Mb88xx {
    pub fn new(variant: Mb88xxVariant) -> Self {
        Self {
            pc: 0,
            pa: 0,
            stack: [0; 4],
            si: 0,
            a: 0,
            x: 0,
            y: 0,
            st: 1, // ST starts at 1 on reset
            zf: 0,
            cf: 0,
            vf: 0,
            sf: 0,
            irq_pin: 0,
            pio: 0,
            th: 0,
            tl: 0,
            tp: 0,
            ctr: 0,
            sb: 0,
            sb_count: 0,
            o_latch: 0,
            port_o: 0,
            pending_irq: 0,
            in_irq: false,
            rom: vec![0; variant.rom_size()],
            ram: vec![0; variant.ram_size()],
            variant,
            rom_mask: variant.rom_mask(),
            ram_mask: variant.ram_mask(),
            pa_mask: variant.pa_mask(),
            k_input: 0,
            r_input: [0; 4],
            r_output: [0; 4],
            p_output: 0,
            si_input: 0,
            so_output: 0,
            rw_input: 0,
            dynamic_k: false,
            second_cycle: false,
            pending_opcode: 0,
        }
    }

    /// Load program ROM data. Truncates or pads to the variant's ROM size.
    pub fn load_rom(&mut self, data: &[u8]) {
        let len = data.len().min(self.rom.len());
        self.rom[..len].copy_from_slice(&data[..len]);
    }

    /// Read a byte from program ROM (public, for test/debug).
    pub fn peek_rom(&self, addr: u16) -> u8 {
        self.rom[(addr & self.rom_mask) as usize]
    }

    /// Write a byte to program ROM (public, for test/debug).
    pub fn poke_rom(&mut self, addr: u16, val: u8) {
        self.rom[(addr & self.rom_mask) as usize] = val;
    }

    /// Read a nibble from data RAM (public, for test/debug).
    pub fn peek_ram(&self, addr: u8) -> u8 {
        self.ram[(addr & self.ram_mask) as usize] & 0x0F
    }

    /// Write a nibble to data RAM (public, for test/debug).
    pub fn poke_ram(&mut self, addr: u8, val: u8) {
        self.ram[(addr & self.ram_mask) as usize] = val & 0x0F;
    }

    /// Get the variant of this CPU.
    pub fn variant(&self) -> Mb88xxVariant {
        self.variant
    }

    /// Reset to power-on state. ROM content is preserved.
    pub fn reset(&mut self) {
        self.pc = 0;
        self.pa = 0;
        self.stack = [0; 4];
        self.si = 0;
        self.a = 0;
        self.x = 0;
        self.y = 0;
        self.st = 1;
        self.zf = 0;
        self.cf = 0;
        self.vf = 0;
        self.sf = 0;
        self.pio = 0;
        self.th = 0;
        self.tl = 0;
        self.tp = 0;
        self.sb = 0;
        self.sb_count = 0;
        self.o_latch = 0;
        self.port_o = 0;
        self.pending_irq = 0;
        self.in_irq = false;
        self.second_cycle = false;
        self.pending_opcode = 0;
    }

    // --- Address helpers ---

    /// Full program counter: PA << 6 | PC.
    #[inline]
    fn get_pc(&self) -> u16 {
        ((self.pa as u16) << 6) | self.pc as u16
    }

    /// Effective data RAM address: X << 4 | Y.
    #[inline]
    fn get_ea(&self) -> u8 {
        (self.x << 4) | self.y
    }

    /// Increment PC (6-bit), wrapping into PA.
    /// PA is not masked here — the ROM address mask is applied when reading,
    /// and PA itself wraps: straight-line execution off the end of the address
    /// space runs back round to page 0, as the counter does in hardware.
    #[inline]
    fn inc_pc(&mut self) {
        self.pc += 1;
        if self.pc >= 0x40 {
            self.pc = 0;
            self.pa = self.pa.wrapping_add(1);
        }
    }

    // --- Memory access ---

    /// Read a byte from program ROM.
    #[inline]
    fn read_rom(&self, addr: u16) -> u8 {
        self.rom[(addr & self.rom_mask) as usize]
    }

    /// Read a nibble from data RAM.
    #[inline]
    fn read_ram(&self, addr: u8) -> u8 {
        self.ram[(addr & self.ram_mask) as usize] & 0x0F
    }

    /// Write a nibble to data RAM.
    #[inline]
    fn write_ram(&mut self, addr: u8, val: u8) {
        self.ram[(addr & self.ram_mask) as usize] = val & 0x0F;
    }

    // --- I/O port access (for use by wrapper devices) ---

    /// Read the shared external O port register (port_o).
    /// This is what external devices (Z80 via 06XX) see, matching MAME's m_portO.
    pub fn read_o(&self) -> u8 {
        self.port_o
    }

    /// Synchronize port_o from the internal o_latch.
    /// Call after execute_cycle() to propagate MCU OUTO writes to the shared
    /// register, matching MAME's O_w_sync which defers o_output → m_portO
    /// updates to between scheduler timeslices.
    pub fn sync_port_o(&mut self) {
        self.port_o = self.o_latch;
    }

    /// Read the P port output latch.
    pub fn read_p(&self) -> u8 {
        self.p_output
    }

    /// Read an R port output latch (0-3).
    pub fn read_r_output(&self, port: usize) -> u8 {
        self.r_output[port & 3]
    }

    /// Set the K port input value (called by wrapper).
    pub fn set_k(&mut self, val: u8) {
        self.k_input = val & 0x0F;
    }

    /// Set an R port input value (called by wrapper).
    pub fn set_r_input(&mut self, port: usize, val: u8) {
        self.r_input[port & 3] = val & 0x0F;
    }

    /// Signal an external IRQ (rising edge).
    pub fn set_irq(&mut self, state: bool) {
        let new_state = state as u8;
        // Rising edge: trigger if IRQ was low and is now high, and external IRQ is enabled
        if self.irq_pin == 0 && new_state != 0 && (self.pio & INT_CAUSE_EXTERNAL) != 0 {
            self.pending_irq |= INT_CAUSE_EXTERNAL;
        }
        self.irq_pin = new_state;
    }

    /// Signal the external counter/timer pin (falling edge triggers count).
    pub fn set_tc(&mut self, state: bool) {
        let new_state = state as u8;
        if self.ctr != 0 && new_state == 0 && (self.pio & 0x40) != 0 {
            self.increment_timer();
        }
        self.ctr = new_state;
    }

    // --- Timer ---

    fn increment_timer(&mut self) {
        self.tl = (self.tl + 1) & 0x0F;
        if self.tl == 0 {
            self.th = (self.th + 1) & 0x0F;
            if self.th == 0 {
                self.vf = 1;
                self.pending_irq |= INT_CAUSE_TIMER;
            }
        }
    }

    // --- PIO enable ---

    fn pio_enable(&mut self, new_pio: u8) {
        self.pio = new_pio;
    }

    // --- O port write (PLA-mapped for 8-bit mode) ---

    fn write_pla(&mut self, index: u8) {
        // 8-bit PLA mode (default): write nibble to high or low half
        // based on bit 4 of the index (carry flag in OUTO instruction).
        // Matches MAME: `(index & 0x0f) << shift` — mask data nibble first
        // to avoid u8 overflow when CF=1 and shift=4.
        let shift = if index & 0x10 != 0 { 4 } else { 0 };
        let mask = 0x0F << shift;
        self.port_o = (self.port_o & !mask) | ((index & 0x0F) << shift);
    }

    // --- Core execution ---

    /// Execute one machine cycle. Call this at the MB88xx machine cycle rate
    /// (external clock / 6).
    pub fn execute_cycle(&mut self) {
        if self.second_cycle {
            self.second_cycle = false;
            self.execute_second_cycle();
            self.burn_cycles(1);
            return;
        }

        // Fetch opcode
        let opcode = self.read_rom(self.get_pc());
        self.inc_pc();

        // Execute the instruction (may set second_cycle for 2-cycle ops)
        self.execute_instruction(opcode);

        // Update timer, interrupts
        self.burn_cycles(1);
    }

    /// Returns true if at an instruction boundary (not mid-instruction).
    pub fn at_instruction_boundary(&self) -> bool {
        !self.second_cycle
    }

    /// Update timer prescaler and process pending interrupts.
    fn burn_cycles(&mut self, cycles: u8) {
        // Internal timer
        if self.pio & 0x80 != 0 {
            self.tp += cycles;
            while self.tp >= TIMER_PRESCALE {
                self.tp -= TIMER_PRESCALE;
                self.increment_timer();
            }
        }

        // Process pending interrupts only at instruction boundaries.
        // 2-cycle instructions split across two execute_cycle() calls set
        // second_cycle=true after the first half. Processing an IRQ here
        // would overwrite PA:PC with the vector address, but the pending
        // second cycle would then read its immediate operand from the
        // wrong location (the vector instead of the original code).
        // MAME executes both cycles atomically, so interrupts only fire
        // after complete instructions.
        if !self.second_cycle && (self.pending_irq & self.pio) != 0 && !self.in_irq {
            self.in_irq = true;
            let int_pc = self.get_pc();

            // Push PC + flags onto stack
            self.stack[self.si as usize] =
                int_pc | (self.cf as u16) << 15 | (self.zf as u16) << 14 | (self.st as u16) << 13;
            self.si = (self.si + 1) & 3;

            // Jump to interrupt vector
            if self.pending_irq & self.pio & INT_CAUSE_EXTERNAL != 0 {
                self.pc = 0x02;
            } else if self.pending_irq & self.pio & INT_CAUSE_TIMER != 0 {
                self.pc = 0x04;
            } else if self.pending_irq & self.pio & INT_CAUSE_SERIAL != 0 {
                self.pc = 0x06;
            }

            self.pa = 0x00;
            self.st = 1;
            self.pending_irq = 0;
        }
    }

    /// Execute second cycle of a 2-cycle instruction.
    fn execute_second_cycle(&mut self) {
        let opcode = self.pending_opcode;
        match opcode {
            // jpa imm: PA = arg, PC = A * 4
            0x3D => {
                self.pa = self.read_rom(self.get_pc()) & self.pa_mask;
                self.pc = (self.a * 4) & 0x3F;
                self.st = 1;
            }
            // en imm: enable PIO bits
            0x3E => {
                let arg = self.read_rom(self.get_pc());
                self.inc_pc();
                self.pio_enable(self.pio | arg);
                self.st = 1;
            }
            // dis imm: disable PIO bits
            0x3F => {
                let arg = self.read_rom(self.get_pc());
                self.inc_pc();
                self.pio_enable(self.pio & !arg);
                self.st = 1;
            }
            // call imm (0x60-0x67): conditional call
            0x60..=0x67 => {
                let arg = self.read_rom(self.get_pc());
                self.inc_pc();
                if self.st != 0 {
                    self.stack[self.si as usize] = self.get_pc();
                    self.si = (self.si + 1) & 3;
                    self.pc = arg & 0x3F;
                    self.pa = ((opcode & 7) << 2 | (arg >> 6)) & self.pa_mask;
                }
                self.st = 1;
            }
            // jpl imm (0x68-0x6F): conditional long jump
            0x68..=0x6F => {
                let arg = self.read_rom(self.get_pc());
                self.inc_pc();
                if self.st != 0 {
                    self.pc = arg & 0x3F;
                    self.pa = ((opcode & 7) << 2 | (arg >> 6)) & self.pa_mask;
                }
                self.st = 1;
            }
            _ => {}
        }
    }

    /// Execute a single-cycle instruction (or start a 2-cycle instruction).
    fn execute_instruction(&mut self, opcode: u8) {
        match opcode {
            // 0x00: NOP
            0x00 => {
                self.st = 1;
            }

            // 0x01: OUTO — write PLA output (O port)
            0x01 => {
                self.write_pla((self.cf << 4) | self.a);
                self.st = 1;
            }

            // 0x02: OUTP — write A to P port
            0x02 => {
                self.p_output = self.a;
                self.st = 1;
            }

            // 0x03: OUT — write A to R port selected by Y[3:2]
            0x03 => {
                let port = (self.y & 3) as usize;
                self.r_output[port] = self.a;
                self.st = 1;
            }

            // 0x04: TAY — transfer A to Y
            0x04 => {
                self.y = self.a;
                self.st = 1;
            }

            // 0x05: TATH — transfer A to TH
            0x05 => {
                self.th = self.a;
                self.st = 1;
            }

            // 0x06: TATL — transfer A to TL
            0x06 => {
                self.tl = self.a;
                self.st = 1;
            }

            // 0x07: TAS — transfer A to SB
            0x07 => {
                self.sb = self.a;
                self.st = 1;
            }

            // 0x08: ICY — increment Y
            0x08 => {
                self.y = self.y.wrapping_add(1);
                self.st = if self.y & 0x10 != 0 { 0 } else { 1 };
                self.y &= 0x0F;
                self.zf = if self.y == 0 { 1 } else { 0 };
            }

            // 0x09: ICM — increment memory
            0x09 => {
                let ea = self.get_ea();
                let mut val = self.read_ram(ea).wrapping_add(1);
                self.st = if val & 0x10 != 0 { 0 } else { 1 };
                val &= 0x0F;
                self.zf = if val == 0 { 1 } else { 0 };
                self.write_ram(ea, val);
            }

            // 0x0A: STIC — store A, then increment Y
            0x0A => {
                let ea = self.get_ea();
                self.write_ram(ea, self.a);
                self.y = self.y.wrapping_add(1);
                self.st = if self.y & 0x10 != 0 { 0 } else { 1 };
                self.y &= 0x0F;
                self.zf = if self.y == 0 { 1 } else { 0 };
            }

            // 0x0B: X — exchange A with memory
            0x0B => {
                let ea = self.get_ea();
                let val = self.read_ram(ea);
                self.write_ram(ea, self.a);
                self.a = val;
                self.zf = if self.a == 0 { 1 } else { 0 };
                self.st = 1;
            }

            // 0x0C: ROL — rotate left through carry
            0x0C => {
                self.a = (self.a << 1) | self.cf;
                self.st = if self.a & 0x10 != 0 { 0 } else { 1 };
                self.cf = self.st ^ 1;
                self.a &= 0x0F;
                self.zf = if self.a == 0 { 1 } else { 0 };
            }

            // 0x0D: L — load A from memory
            0x0D => {
                self.a = self.read_ram(self.get_ea());
                self.zf = if self.a == 0 { 1 } else { 0 };
                self.st = 1;
            }

            // 0x0E: ADC — add memory + carry to A
            0x0E => {
                let ea = self.get_ea();
                let val = self.read_ram(ea);
                let result = val.wrapping_add(self.a).wrapping_add(self.cf);
                self.st = if result & 0x10 != 0 { 0 } else { 1 };
                self.cf = self.st ^ 1;
                self.a = result & 0x0F;
                self.zf = if self.a == 0 { 1 } else { 0 };
            }

            // 0x0F: AND — A &= memory
            0x0F => {
                self.a &= self.read_ram(self.get_ea());
                self.zf = if self.a == 0 { 1 } else { 0 };
                self.st = self.zf ^ 1;
            }

            // 0x10: DAA — decimal adjust after addition
            0x10 => {
                if self.cf != 0 || self.a > 9 {
                    self.a += 6;
                }
                self.st = if self.a & 0x10 != 0 { 0 } else { 1 };
                self.cf = self.st ^ 1;
                self.a &= 0x0F;
            }

            // 0x11: DAS — decimal adjust after subtraction
            0x11 => {
                if self.cf != 0 || self.a > 9 {
                    self.a += 10;
                }
                self.st = if self.a & 0x10 != 0 { 0 } else { 1 };
                self.cf = self.st ^ 1;
                self.a &= 0x0F;
            }

            // 0x12: INK — read K port to A
            0x12 => {
                // In dynamic_k mode, compute K at execution time from the
                // current port_o and rw_input, matching MAME's K_r() callback
                // which reads m_portO at the moment the MCU accesses K.
                // port_o is the shared external register (updated between
                // execute_cycle calls), not the internal o_latch.
                let k = if self.dynamic_k {
                    (self.rw_input << 3) | (self.port_o & 0x07)
                } else {
                    self.k_input
                };
                self.a = k & 0x0F;
                self.zf = if self.a == 0 { 1 } else { 0 };
                self.st = 1;
            }

            // 0x13: IN — read R port (selected by Y[1:0]) to A
            0x13 => {
                let port = (self.y & 3) as usize;
                self.a = self.r_input[port] & 0x0F;
                self.zf = if self.a == 0 { 1 } else { 0 };
                self.st = 1;
            }

            // 0x14: TYA — transfer Y to A
            0x14 => {
                self.a = self.y;
                self.zf = if self.a == 0 { 1 } else { 0 };
                self.st = 1;
            }

            // 0x15: TTHA — transfer TH to A
            0x15 => {
                self.a = self.th;
                self.zf = if self.a == 0 { 1 } else { 0 };
                self.st = 1;
            }

            // 0x16: TTLA — transfer TL to A
            0x16 => {
                self.a = self.tl;
                self.zf = if self.a == 0 { 1 } else { 0 };
                self.st = 1;
            }

            // 0x17: TSA — transfer SB to A
            0x17 => {
                self.a = self.sb;
                self.zf = if self.a == 0 { 1 } else { 0 };
                self.st = 1;
            }

            // 0x18: DCY — decrement Y
            0x18 => {
                self.y = self.y.wrapping_sub(1);
                self.st = if self.y & 0x10 != 0 { 0 } else { 1 };
                self.y &= 0x0F;
            }

            // 0x19: DCM — decrement memory
            0x19 => {
                let ea = self.get_ea();
                let mut val = self.read_ram(ea).wrapping_sub(1);
                self.st = if val & 0x10 != 0 { 0 } else { 1 };
                val &= 0x0F;
                self.zf = if val == 0 { 1 } else { 0 };
                self.write_ram(ea, val);
            }

            // 0x1A: STDC — store A, then decrement Y
            0x1A => {
                let ea = self.get_ea();
                self.write_ram(ea, self.a);
                self.y = self.y.wrapping_sub(1);
                self.st = if self.y & 0x10 != 0 { 0 } else { 1 };
                self.y &= 0x0F;
                self.zf = if self.y == 0 { 1 } else { 0 };
            }

            // 0x1B: XX — exchange A with X
            0x1B => {
                std::mem::swap(&mut self.x, &mut self.a);
                self.zf = if self.a == 0 { 1 } else { 0 };
                self.st = 1;
            }

            // 0x1C: ROR — rotate right through carry
            0x1C => {
                self.a |= self.cf << 4;
                self.st = if (self.a << 4) & 0x10 != 0 { 0 } else { 1 };
                self.cf = self.st ^ 1;
                self.a >>= 1;
                self.a &= 0x0F;
                self.zf = if self.a == 0 { 1 } else { 0 };
            }

            // 0x1D: ST — store A to memory
            0x1D => {
                self.write_ram(self.get_ea(), self.a);
                self.st = 1;
            }

            // 0x1E: SBC — subtract A + carry from memory
            0x1E => {
                let ea = self.get_ea();
                let val = self.read_ram(ea);
                let result = val.wrapping_sub(self.a).wrapping_sub(self.cf);
                self.st = if result & 0x10 != 0 { 0 } else { 1 };
                self.cf = self.st ^ 1;
                self.a = result & 0x0F;
                self.zf = if self.a == 0 { 1 } else { 0 };
            }

            // 0x1F: OR — A |= memory
            0x1F => {
                self.a |= self.read_ram(self.get_ea());
                self.zf = if self.a == 0 { 1 } else { 0 };
                self.st = self.zf ^ 1;
            }

            // 0x20: SETR — set bit in R port (bit = Y[1:0], port = Y[3:2])
            0x20 => {
                let port = (self.y >> 2) as usize & 3;
                let bit = self.y & 3;
                let val = self.r_input[port] & 0x0F;
                self.r_output[port] = val | (1 << bit);
                self.st = 1;
            }

            // 0x21: SETC — set carry flag
            0x21 => {
                self.cf = 1;
                self.st = 1;
            }

            // 0x22: RSTR — reset bit in R port
            0x22 => {
                let port = (self.y >> 2) as usize & 3;
                let bit = self.y & 3;
                let val = self.r_input[port] & 0x0F;
                self.r_output[port] = val & !(1 << bit);
                self.st = 1;
            }

            // 0x23: RSTC — reset carry flag
            0x23 => {
                self.cf = 0;
                self.st = 1;
            }

            // 0x24: TSTR — test bit in R port
            0x24 => {
                let port = (self.y >> 2) as usize & 3;
                let bit = self.y & 3;
                let val = self.r_input[port] & 0x0F;
                self.st = if val & (1 << bit) != 0 { 0 } else { 1 };
            }

            // 0x25: TSTI — test IRQ pin
            0x25 => {
                self.st = self.irq_pin ^ 1;
            }

            // 0x26: TSTV — test and clear overflow flag
            0x26 => {
                self.st = self.vf ^ 1;
                self.vf = 0;
            }

            // 0x27: TSTS — test and clear serial flag
            0x27 => {
                self.st = self.sf ^ 1;
                if self.sf != 0 {
                    self.sb_count = 0;
                }
                self.sf = 0;
            }

            // 0x28: TSTC — test carry flag
            0x28 => {
                self.st = self.cf ^ 1;
            }

            // 0x29: TSTZ — test zero flag
            0x29 => {
                self.st = self.zf ^ 1;
            }

            // 0x2A: STS — store SB to memory
            0x2A => {
                let ea = self.get_ea();
                self.write_ram(ea, self.sb);
                self.zf = if self.sb == 0 { 1 } else { 0 };
                self.st = 1;
            }

            // 0x2B: LS — load SB from memory
            0x2B => {
                let ea = self.get_ea();
                self.sb = self.read_ram(ea);
                self.zf = if self.sb == 0 { 1 } else { 0 };
                self.st = 1;
            }

            // 0x2C: RTS — return from subroutine
            0x2C => {
                self.si = (self.si.wrapping_sub(1)) & 3;
                self.pc = (self.stack[self.si as usize] & 0x3F) as u8;
                self.pa = ((self.stack[self.si as usize] >> 6) & self.pa_mask as u16) as u8;
                self.st = 1;
            }

            // 0x2D: NEG — negate A
            0x2D => {
                self.a = (!self.a).wrapping_add(1) & 0x0F;
                self.st = if self.a == 0 { 0 } else { 1 };
            }

            // 0x2E: C — compare A with memory
            0x2E => {
                let ea = self.get_ea();
                let val = self.read_ram(ea);
                let result = val.wrapping_sub(self.a);
                self.cf = if result & 0x10 != 0 { 1 } else { 0 };
                let masked = result & 0x0F;
                self.st = if masked == 0 { 0 } else { 1 };
                self.zf = self.st ^ 1;
            }

            // 0x2F: EOR — A ^= memory
            0x2F => {
                self.a ^= self.read_ram(self.get_ea());
                self.st = if self.a == 0 { 0 } else { 1 };
                self.zf = self.st ^ 1;
            }

            // 0x30-0x33: SBIT n — set bit n in memory
            0x30..=0x33 => {
                let ea = self.get_ea();
                let val = self.read_ram(ea);
                self.write_ram(ea, val | (1 << (opcode & 3)));
                self.st = 1;
            }

            // 0x34-0x37: RBIT n — reset bit n in memory
            0x34..=0x37 => {
                let ea = self.get_ea();
                let val = self.read_ram(ea);
                self.write_ram(ea, val & !(1 << (opcode & 3)));
                self.st = 1;
            }

            // 0x38-0x3B: TBIT n — test bit n in memory
            0x38..=0x3B => {
                let ea = self.get_ea();
                let val = self.read_ram(ea);
                self.st = if val & (1 << (opcode & 3)) != 0 { 0 } else { 1 };
            }

            // 0x3C: RTI — return from interrupt
            0x3C => {
                self.in_irq = false;
                self.si = (self.si.wrapping_sub(1)) & 3;
                let sp_val = self.stack[self.si as usize];
                self.pc = (sp_val & 0x3F) as u8;
                self.pa = ((sp_val >> 6) & self.pa_mask as u16) as u8;
                self.st = ((sp_val >> 13) & 1) as u8;
                self.zf = ((sp_val >> 14) & 1) as u8;
                self.cf = ((sp_val >> 15) & 1) as u8;
            }

            // 0x3D: JPA imm — jump indirect via A (2 cycles)
            // 0x3E: EN imm — enable PIO bits (2 cycles)
            // 0x3F: DIS imm — disable PIO bits (2 cycles)
            0x3D..=0x3F => {
                self.pending_opcode = opcode;
                self.second_cycle = true;
            }

            // 0x40-0x43: SETD n — set bit n in R0
            0x40..=0x43 => {
                let val = self.r_input[0] & 0x0F;
                self.r_output[0] = val | (1 << (opcode & 3));
                self.st = 1;
            }

            // 0x44-0x47: RSTD n — reset bit n in R0
            0x44..=0x47 => {
                let val = self.r_input[0] & 0x0F;
                self.r_output[0] = val & !(1 << (opcode & 3));
                self.st = 1;
            }

            // 0x48-0x4B: TSTD n — test bit n in R2
            0x48..=0x4B => {
                let val = self.r_input[2] & 0x0F;
                self.st = if val & (1 << (opcode & 3)) != 0 { 0 } else { 1 };
            }

            // 0x4C-0x4F: TBA n — test bit n of A
            0x4C..=0x4F => {
                self.st = if self.a & (1 << (opcode & 3)) != 0 {
                    0
                } else {
                    1
                };
            }

            // 0x50-0x53: XD n — exchange A with RAM[n]
            0x50..=0x53 => {
                let addr = opcode & 3;
                let val = self.read_ram(addr);
                self.write_ram(addr, self.a);
                self.a = val;
                self.zf = if self.a == 0 { 1 } else { 0 };
                self.st = 1;
            }

            // 0x54-0x57: XYD n — exchange Y with RAM[n+4]
            0x54..=0x57 => {
                let addr = (opcode & 3) + 4;
                let val = self.read_ram(addr);
                self.write_ram(addr, self.y);
                self.y = val;
                self.zf = if self.y == 0 { 1 } else { 0 };
                self.st = 1;
            }

            // 0x58-0x5F: LXI n — load X with immediate (3 bits)
            0x58..=0x5F => {
                self.x = opcode & 7;
                self.zf = if self.x == 0 { 1 } else { 0 };
                self.st = 1;
            }

            // 0x60-0x67: CALL imm — conditional call (2 cycles)
            // 0x68-0x6F: JPL imm — conditional long jump (2 cycles)
            0x60..=0x6F => {
                self.pending_opcode = opcode;
                self.second_cycle = true;
            }

            // 0x70-0x7F: AI n — add immediate to A
            0x70..=0x7F => {
                let imm = opcode & 0x0F;
                let result = (self.a).wrapping_add(imm);
                self.st = if result & 0x10 != 0 { 0 } else { 1 };
                self.cf = self.st ^ 1;
                self.a = result & 0x0F;
                self.zf = if self.a == 0 { 1 } else { 0 };
            }

            // 0x80-0x8F: LYI n — load Y with immediate (4 bits)
            0x80..=0x8F => {
                self.y = opcode & 0x0F;
                self.zf = if self.y == 0 { 1 } else { 0 };
                self.st = 1;
            }

            // 0x90-0x9F: LI n — load A with immediate (4 bits)
            0x90..=0x9F => {
                self.a = opcode & 0x0F;
                self.zf = if self.a == 0 { 1 } else { 0 };
                self.st = 1;
            }

            // 0xA0-0xAF: CYI n — compare Y with immediate
            0xA0..=0xAF => {
                let imm = opcode & 0x0F;
                let result = imm.wrapping_sub(self.y);
                self.cf = if result & 0x10 != 0 { 1 } else { 0 };
                let masked = result & 0x0F;
                self.st = if masked == 0 { 0 } else { 1 };
                self.zf = self.st ^ 1;
            }

            // 0xB0-0xBF: CI n — compare A with immediate
            0xB0..=0xBF => {
                let imm = opcode & 0x0F;
                let result = imm.wrapping_sub(self.a);
                self.cf = if result & 0x10 != 0 { 1 } else { 0 };
                let masked = result & 0x0F;
                self.st = if masked == 0 { 0 } else { 1 };
                self.zf = self.st ^ 1;
            }

            // 0xC0-0xFF: JMP — conditional short jump within page
            _ => {
                if self.st != 0 {
                    self.pc = opcode & 0x3F;
                }
                self.st = 1;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// State snapshot for debug UI
// ---------------------------------------------------------------------------

/// MB88xx CPU state snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct Mb88xxState {
    pub pc: u16,
    pub a: u8,
    pub x: u8,
    pub y: u8,
    pub st: u8,
    pub zf: u8,
    pub cf: u8,
    pub sb: u8,
    pub pio: u8,
}

impl Mb88xxState {
    pub fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "PC",
                value: self.pc as u64,
                width: 16,
            },
            DebugRegister {
                name: "A",
                value: self.a as u64,
                width: 4,
            },
            DebugRegister {
                name: "X",
                value: self.x as u64,
                width: 4,
            },
            DebugRegister {
                name: "Y",
                value: self.y as u64,
                width: 4,
            },
            DebugRegister {
                name: "ST",
                value: self.st as u64,
                width: 1,
            },
            DebugRegister {
                name: "CF",
                value: self.cf as u64,
                width: 1,
            },
            DebugRegister {
                name: "ZF",
                value: self.zf as u64,
                width: 1,
            },
            DebugRegister {
                name: "SB",
                value: self.sb as u64,
                width: 4,
            },
            DebugRegister {
                name: "PIO",
                value: self.pio as u64,
                width: 8,
            },
        ]
    }
}

impl CpuStateTrait for Mb88xx {
    type Snapshot = Mb88xxState;

    fn snapshot(&self) -> Mb88xxState {
        Mb88xxState {
            pc: self.get_pc(),
            a: self.a,
            x: self.x,
            y: self.y,
            st: self.st,
            zf: self.zf,
            cf: self.cf,
            sb: self.sb,
            pio: self.pio,
        }
    }
}

// ---------------------------------------------------------------------------
// Debuggable
// ---------------------------------------------------------------------------

impl Debuggable for Mb88xx {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        self.snapshot().debug_registers()
    }
}

// ---------------------------------------------------------------------------
// Save state
// ---------------------------------------------------------------------------

const SAVE_VERSION: u8 = 2;

impl Saveable for Mb88xx {
    fn save_state(&self, w: &mut StateWriter) {
        w.write_version(SAVE_VERSION);
        w.write_u8(self.pc);
        w.write_u8(self.pa);
        for &s in &self.stack {
            w.write_u16_le(s);
        }
        w.write_u8(self.si);
        w.write_u8(self.a);
        w.write_u8(self.x);
        w.write_u8(self.y);
        w.write_u8(self.st);
        w.write_u8(self.zf);
        w.write_u8(self.cf);
        w.write_u8(self.vf);
        w.write_u8(self.sf);
        w.write_u8(self.irq_pin);
        w.write_u8(self.pio);
        w.write_u8(self.th);
        w.write_u8(self.tl);
        w.write_u8(self.tp);
        w.write_u8(self.ctr);
        w.write_u8(self.sb);
        w.write_u16_le(self.sb_count);
        w.write_u8(self.o_latch);
        w.write_u8(self.port_o);
        w.write_u8(self.pending_irq);
        w.write_bool(self.in_irq);
        w.write_bytes(&self.ram);
        w.write_u8(self.k_input);
        for &r in &self.r_input {
            w.write_u8(r);
        }
        for &r in &self.r_output {
            w.write_u8(r);
        }
        w.write_u8(self.p_output);
        w.write_u8(self.si_input);
        w.write_u8(self.so_output);
        w.write_bool(self.second_cycle);
        w.write_u8(self.pending_opcode);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        r.read_version(SAVE_VERSION)?;
        self.pc = r.read_u8()?;
        self.pa = r.read_u8()?;
        for s in &mut self.stack {
            *s = r.read_u16_le()?;
        }
        self.si = r.read_u8()?;
        self.a = r.read_u8()?;
        self.x = r.read_u8()?;
        self.y = r.read_u8()?;
        self.st = r.read_u8()?;
        self.zf = r.read_u8()?;
        self.cf = r.read_u8()?;
        self.vf = r.read_u8()?;
        self.sf = r.read_u8()?;
        self.irq_pin = r.read_u8()?;
        self.pio = r.read_u8()?;
        self.th = r.read_u8()?;
        self.tl = r.read_u8()?;
        self.tp = r.read_u8()?;
        self.ctr = r.read_u8()?;
        self.sb = r.read_u8()?;
        self.sb_count = r.read_u16_le()?;
        self.o_latch = r.read_u8()?;
        self.port_o = r.read_u8()?;
        self.pending_irq = r.read_u8()?;
        self.in_irq = r.read_bool()?;
        let ram_data = r.read_bytes()?;
        let len = ram_data.len().min(self.ram.len());
        self.ram[..len].copy_from_slice(&ram_data[..len]);
        self.k_input = r.read_u8()?;
        for ri in &mut self.r_input {
            *ri = r.read_u8()?;
        }
        for ro in &mut self.r_output {
            *ro = r.read_u8()?;
        }
        self.p_output = r.read_u8()?;
        self.si_input = r.read_u8()?;
        self.so_output = r.read_u8()?;
        self.second_cycle = r.read_bool()?;
        self.pending_opcode = r.read_u8()?;
        Ok(())
    }
}
