/// Identifies who is accessing the bus (for multi-CPU/DMA arbitration)
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BusMaster {
    Cpu(usize), // CPU 0, CPU 1, etc.
    Dma,        // DMA reads through the bus (sees ROM banking overlays)
    DmaVram,    // DMA reads directly from video RAM, bypassing banking overlays
                // (used by blitter dest reads for keepmask blending — matches MAME's
                // blit_pixel reading from m_vram[] instead of the address space)
}

/// The control lines a master drives alongside the address for one bus cycle.
///
/// On the 68000 these are literal pins: R/W, the function code on FC2..FC0, and
/// the two data strobes. A device that decodes address lines alone ignores all
/// of them, and the Atari Slapstic is that device. What needs them is a
/// comparison against a recorded per-cycle trace, whose entries carry the
/// function code and the strobes, neither of which is recoverable from the
/// address.
///
/// The privilege bit in particular cannot be inferred from outside. `RTE`,
/// `MOVE to SR` and exception entry all change it *during* an instruction, so
/// the cycles before and after the change name different address spaces, and a
/// consumer reading the status register at the instruction boundary would be
/// asking a question whose answer has already moved.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct BusSignals {
    /// A write cycle rather than a read.
    pub is_write: bool,
    /// Program space (an instruction word) rather than data space.
    pub program: bool,
    /// Supervisor privilege rather than user.
    pub supervisor: bool,
    /// One byte behind a single strobe, rather than a full word behind both.
    pub byte: bool,
    /// An indivisible read-modify-write: the address strobe is held across the
    /// read *and* the write that follows it, so the two are one bus cycle and
    /// no other master can take the bus between them.
    ///
    /// `TAS` is the only instruction that drives one, which is the whole point
    /// of `TAS`: the test and the set cannot be separated by another master.
    /// The read announces the cycle and the write inside it announces nothing,
    /// so a consumer counting cycles counts one. Ten clocks rather than the
    /// usual four, because the part spends two between the halves.
    pub rmw: bool,
}

impl BusSignals {
    /// The 68000 function code FC2..FC0 this cycle drives: 1 user data,
    /// 2 user program, 5 supervisor data, 6 supervisor program.
    ///
    /// FC2 is the privilege bit and FC1/FC0 select the space, which is why
    /// program and data differ by exactly one in each privilege pair. The
    /// remaining codes (0, 3, 4 and 7) name the CPU space the part uses for
    /// interrupt acknowledge, and nothing here drives one.
    pub fn function_code(self) -> u8 {
        let privilege = if self.supervisor { 4 } else { 0 };
        let space = if self.program { 2 } else { 1 };
        privilege | space
    }
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

    /// Observe one bus cycle at its exact address, before it is resolved.
    ///
    /// Every transfer a master drives comes through here, instruction
    /// prefetches included: the Atari Slapstic arms itself by watching the
    /// program *fetch* at a magic address, so a hook that saw only operand
    /// accesses would miss the thing it exists for. The default is a no-op;
    /// machines with address-sequence-sensitive hardware on the bus override
    /// it to drive that hardware's state machine.
    ///
    /// `addr` is the precise byte address, so consecutive byte accesses present
    /// distinct odd and even addresses exactly as the part's pins do.
    /// [`BusSignals`] carries what the part drives alongside the address.
    fn observe_bus_cycle(
        &mut self,
        _master: BusMaster,
        _addr: Self::Address,
        _signals: BusSignals,
    ) {
    }

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
