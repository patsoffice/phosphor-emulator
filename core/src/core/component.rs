use super::bus::BusMaster;

/// Extension for components that act as bus masters (CPUs, DMA controllers)
///
/// The bus is a parameter of the *trait* rather than of the method, so a
/// component dispatches to whatever concrete bus it is handed and the optimiser
/// can see through the call, and so an implementor can **narrow what it demands
/// of that bus**. A bound written on the method could only ever be the one this
/// trait declared, which every implementor would then be stuck with: the M68000
/// needs byte strobes that no 8-bit bus has, and says so where it implements
/// this. `?Sized` keeps `&mut dyn Bus` working for the boards that still hand
/// one over.
///
/// The width associated types stay on the component rather than being read off
/// `B`, because they are what a system's bounds are written against: a board
/// says it holds a CPU driving a `u32`/`u16` bus without naming the bus type.
pub trait BusMasterComponent<B: ?Sized> {
    /// Address width of the bus this component drives: `u16` for 8-bit boards,
    /// `u32` for the 16/32-bit ones.
    type Address: Copy + Into<u64>;

    /// Data width of that bus: `u8`, or `u16` for a word-wide bus.
    type Data;

    /// Execute one cycle with bus access. Returns true at instruction boundary.
    fn tick_with_bus(&mut self, bus: &mut B, master_id: BusMaster) -> bool;
}
