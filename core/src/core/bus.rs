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

/// Execute a block with `self` split into a `&mut dyn Bus` reference.
///
/// Machine structs own both CPU(s) and the Bus implementation. Rust's borrow
/// checker cannot see that `cpu.execute_cycle(bus, ...)` only touches
/// CPU-internal fields while `Bus::read/write` only touches memory/device
/// fields — these are disjoint parts of the same struct.
///
/// This macro encapsulates the raw-pointer borrow split so every call site
/// doesn't need its own `unsafe` block and safety comment.
///
/// # Usage
/// ```ignore
/// bus_split!(self, bus => {
///     self.cpu.execute_cycle(bus, BusMaster::Cpu(0));
///     self.sound_cpu.execute_cycle(bus, BusMaster::Cpu(1));
/// });
/// ```
///
/// # Safety
/// The caller's struct must ensure that fields accessed through the `Bus` trait
/// implementation (RAM, ROM, I/O devices) are disjoint from fields accessed by
/// the CPU methods called inside the block (registers, state machine).
#[macro_export]
macro_rules! bus_split {
    ($self:expr, $bus:ident => $body:block) => {{
        let __ptr: *mut _ = $self;
        #[allow(unused_unsafe)]
        let $bus = unsafe { &mut *__ptr as &mut dyn $crate::core::Bus<Address = u16, Data = u8> };
        $body
    }};
    ($self:expr, $bus:ident : u32 => $body:block) => {{
        let __ptr: *mut _ = $self;
        #[allow(unused_unsafe)]
        let $bus = unsafe { &mut *__ptr as &mut dyn $crate::core::Bus<Address = u32, Data = u8> };
        $body
    }};
    ($self:expr, $bus:ident : u32 word => $body:block) => {{
        let __ptr: *mut _ = $self;
        #[allow(unused_unsafe)]
        let $bus = unsafe { &mut *__ptr as &mut dyn $crate::core::Bus<Address = u32, Data = u16> };
        $body
    }};
}
