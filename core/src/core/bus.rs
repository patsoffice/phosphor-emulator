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

/// A 16-bit-data bus that can be driven one byte at a time.
///
/// The 68000 has no A0 pin. It puts an even word address on the bus and selects
/// which half of it a transfer touches with two strobes: UDS for the even byte
/// on D8-D15, LDS for the odd byte on D0-D7, both together for a word. A byte
/// access is therefore **one bus cycle**, not a word cycle with the unwanted
/// half discarded, and a peripheral wired to only one half is not accessed at
/// all by a transfer that does not assert its strobe.
///
/// # Why these are required rather than provided
///
/// A default implementation would have to read the containing word, patch a
/// byte and write it back. That is correct for RAM and wrong for every
/// side-effecting register: the read is a phantom access the hardware never
/// performs, and the write puts a stale value back into the neighboring byte.
/// A board that forgot to override such a default would inherit exactly that
/// bug in silence, which is the failure this trait exists to prevent. Each bus
/// states its strobe behavior; a genuinely RAM-backed one says so by calling
/// [`rmw_byte`].
pub trait Bus16: Bus<Address = u32, Data = u16> {
    /// Read the byte at `addr`, asserting UDS for an even address and LDS for
    /// an odd one.
    fn read_byte(&mut self, master: BusMaster, addr: u32) -> u8;

    /// Write `data` to the byte at `addr`, asserting the one strobe that byte
    /// sits behind. Exactly one bus cycle, and no read.
    fn write_byte(&mut self, master: BusMaster, addr: u32, data: u8);
}

/// Select the byte at `addr` out of the word that contains it.
///
/// The helper for a bus whose backing store really is word-wide memory, so
/// that the byte-selecting half of [`Bus16::read_byte`] is written once.
#[inline]
pub fn select_byte(word: u16, addr: u32) -> u8 {
    if addr & 1 == 0 {
        (word >> 8) as u8
    } else {
        word as u8
    }
}

/// Read-modify-write a byte into the word that contains it.
///
/// **Only correct for memory.** This performs the phantom read that
/// [`Bus16`] exists to eliminate, so it is right for RAM and wrong for
/// anything that reacts to being read. It is a free function rather than a
/// default method so that a bus reaches for it deliberately, and so that
/// reaching for it is visible in that bus's source.
#[inline]
pub fn rmw_byte<B: Bus<Address = u32, Data = u16> + ?Sized>(
    bus: &mut B,
    master: BusMaster,
    addr: u32,
    data: u8,
) {
    let word_addr = addr & !1;
    let word = bus.read(master, word_addr);
    let merged = if addr & 1 == 0 {
        (word & 0x00FF) | ((data as u16) << 8)
    } else {
        (word & 0xFF00) | data as u16
    };
    bus.write(master, word_addr, merged);
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
