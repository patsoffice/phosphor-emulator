/// Identifies who is accessing the bus (for multi-CPU/DMA arbitration)
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BusMaster {
    Cpu(usize), // CPU 0, CPU 1, etc.
    Dma,        // DMA reads through the bus (sees ROM banking overlays)
    DmaVram,    // DMA reads directly from video RAM, bypassing banking overlays
                // (used by blitter dest reads for keepmask blending — matches MAME's
                // blit_pixel reading from m_vram[] instead of the address space)
}

/// Generic bus interface supporting halt/arbitration (TSC, RDY, BUSREQ, etc.)
pub trait Bus {
    type Address: Copy + Into<u64>; // u16 for 8-bit, u32 for 16/32-bit
    type Data; // u8 or u16

    fn read(&mut self, master: BusMaster, addr: Self::Address) -> Self::Data;
    fn write(&mut self, master: BusMaster, addr: Self::Address, data: Self::Data);

    /// Read from I/O port address space (separate from memory on Z80).
    /// Default maps to memory read; override for CPUs with separate I/O.
    fn io_read(&mut self, master: BusMaster, addr: Self::Address) -> Self::Data {
        self.read(master, addr)
    }

    /// Write to I/O port address space (separate from memory on Z80).
    /// Default maps to memory write; override for CPUs with separate I/O.
    fn io_write(&mut self, master: BusMaster, addr: Self::Address, data: Self::Data) {
        self.write(master, addr, data)
    }

    /// Observe a *data* bus access (read or write) at its exact address before
    /// it is resolved. Opcode prefetches are excluded. The default is a no-op;
    /// machines with address-sequence-sensitive hardware on the bus (e.g. an
    /// Atari Slapstic, which snoops every address line) override this to drive
    /// that hardware's state machine. `addr` is the precise byte address — for
    /// a byte access this is the unaligned address, matching what the chip's
    /// address pins actually see.
    fn observe_data_access(&mut self, _master: BusMaster, _addr: Self::Address, _is_write: bool) {}

    /// Check if the bus is halted for this master (TSC/RDY/BUSREQ).
    /// Returns true if the master must pause before the next bus cycle.
    fn is_halted_for(&self, master: BusMaster) -> bool;

    /// Generic interrupt query. CPUs pick what they need.
    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState;
}

#[derive(Clone, Copy, Debug)]
pub struct InterruptState {
    pub nmi: bool,
    pub irq: bool,
    pub firq: bool,     // 6809-specific; ignored by other CPUs
    pub irq_vector: u8, // Byte placed on data bus during Z80 IRQ ACK (IM2 vectoring)
    pub irq_level: u8,  // 68000 interrupt priority: 0 = none, 1-7 = level (7 = NMI)
}

impl Default for InterruptState {
    fn default() -> Self {
        Self {
            nmi: false,
            irq: false,
            firq: false,
            irq_vector: 0xFF,
            irq_level: 0,
        }
    }
}
