use super::bus::{Bus, BusMaster};

/// Extension for components that act as bus masters (CPUs, DMA controllers)
///
/// The bus is a type parameter on each method rather than an associated type,
/// so a component dispatches to whatever concrete bus it is handed and the
/// optimiser can see through the call. `?Sized` keeps `&mut dyn Bus` working
/// for the boards that still hand one over.
pub trait BusMasterComponent {
    /// Address width of the bus this component drives: `u16` for 8-bit boards,
    /// `u32` for the 16/32-bit ones.
    type Address: Copy + Into<u64>;

    /// Data width of that bus: `u8`, or `u16` for a word-wide bus.
    type Data;

    /// Execute one cycle with bus access. Returns true at instruction boundary.
    fn tick_with_bus<B: Bus<Address = Self::Address, Data = Self::Data> + ?Sized>(
        &mut self,
        bus: &mut B,
        master_id: BusMaster,
    ) -> bool;
}
