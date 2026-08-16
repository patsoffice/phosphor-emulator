//! Generic test harness systems for CPU validation and experimentation.
//!
//! Provides flat-bus systems with no I/O devices — just a CPU and RAM.

use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::component::BusMasterComponent;
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::i8035::I8035;
use phosphor_core::cpu::i8088::I8088;
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::cpu::m6800::M6800;
use phosphor_core::cpu::m6809::M6809;
use phosphor_core::cpu::m68000::M68000;
use phosphor_core::cpu::z80::Z80;

// ---------------------------------------------------------------------------
// SimpleSystem<C> — 16-bit address space (64 KB flat RAM)
// ---------------------------------------------------------------------------

pub struct SimpleSystem<C>
where
    C: Cpu + BusMasterComponent<Address = u16, Data = u8> + 'static,
{
    pub cpu: C,
    ram: [u8; 0x10000],
    clock: u64,
}

impl<C> Default for SimpleSystem<C>
where
    C: Cpu + Default + BusMasterComponent<Address = u16, Data = u8> + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<C> SimpleSystem<C>
where
    C: Cpu + Default + BusMasterComponent<Address = u16, Data = u8> + 'static,
{
    pub fn new() -> Self {
        Self {
            cpu: C::default(),
            ram: [0; 0x10000],
            clock: 0,
        }
    }
}

impl<C> SimpleSystem<C>
where
    C: Cpu + BusMasterComponent<Address = u16, Data = u8> + 'static,
{
    pub fn tick(&mut self) {
        bus_split!(self, bus => {
            self.cpu.tick_with_bus(bus, BusMaster::Cpu(0));
        });
        self.clock += 1;
    }

    /// Run one frame (16 667 cycles — 1 MHz CPU at 60 Hz).
    pub fn run_frame(&mut self) {
        for _ in 0..16667 {
            self.tick();
        }
    }

    /// Load bytes into RAM at `offset`.
    pub fn load_program(&mut self, offset: usize, data: &[u8]) {
        if offset + data.len() <= self.ram.len() {
            self.ram[offset..offset + data.len()].copy_from_slice(data);
        }
    }

    pub fn get_cpu_state(&self) -> C::Snapshot {
        self.cpu.snapshot()
    }

    pub fn read_ram(&self, addr: usize) -> u8 {
        self.ram.get(addr).copied().unwrap_or(0)
    }

    pub fn write_ram(&mut self, addr: usize, data: u8) {
        if let Some(cell) = self.ram.get_mut(addr) {
            *cell = data;
        }
    }

    pub fn clock(&self) -> u64 {
        self.clock
    }
}

impl<C> Bus for SimpleSystem<C>
where
    C: Cpu + BusMasterComponent<Address = u16, Data = u8> + 'static,
{
    type Address = u16;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        self.ram[addr as usize]
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        self.ram[addr as usize] = data;
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState::default()
    }
}

// Type aliases for 16-bit CPUs
pub type Simple6502System = SimpleSystem<M6502>;
pub type Simple6800System = SimpleSystem<M6800>;
pub type Simple6809System = SimpleSystem<M6809>;
pub type SimpleZ80System = SimpleSystem<Z80>;
pub type SimpleI8035System = SimpleSystem<I8035>;

// ---------------------------------------------------------------------------
// SimpleSystem32<C> — 32-bit address space (1 MB flat RAM)
// ---------------------------------------------------------------------------

pub struct SimpleSystem32<C>
where
    C: Cpu + BusMasterComponent<Address = u32, Data = u8> + 'static,
{
    pub cpu: C,
    ram: Vec<u8>,
    clock: u64,
}

impl<C> Default for SimpleSystem32<C>
where
    C: Cpu + Default + BusMasterComponent<Address = u32, Data = u8> + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<C> SimpleSystem32<C>
where
    C: Cpu + Default + BusMasterComponent<Address = u32, Data = u8> + 'static,
{
    pub fn new() -> Self {
        Self {
            cpu: C::default(),
            ram: vec![0; 0x10_0000], // 1 MB
            clock: 0,
        }
    }
}

impl<C> SimpleSystem32<C>
where
    C: Cpu + BusMasterComponent<Address = u32, Data = u8> + 'static,
{
    pub fn tick(&mut self) {
        bus_split!(self, bus: u32 => {
            self.cpu.tick_with_bus(bus, BusMaster::Cpu(0));
        });
        self.clock += 1;
    }

    /// Run one frame (16 667 cycles — 1 MHz CPU at 60 Hz).
    pub fn run_frame(&mut self) {
        for _ in 0..16667 {
            self.tick();
        }
    }

    /// Load bytes into RAM at `offset`.
    pub fn load_program(&mut self, offset: usize, data: &[u8]) {
        if offset + data.len() <= self.ram.len() {
            self.ram[offset..offset + data.len()].copy_from_slice(data);
        }
    }

    pub fn get_cpu_state(&self) -> C::Snapshot {
        self.cpu.snapshot()
    }

    pub fn read_ram(&self, addr: usize) -> u8 {
        self.ram.get(addr).copied().unwrap_or(0)
    }

    pub fn write_ram(&mut self, addr: usize, data: u8) {
        if let Some(cell) = self.ram.get_mut(addr) {
            *cell = data;
        }
    }

    pub fn clock(&self) -> u64 {
        self.clock
    }
}

impl<C> Bus for SimpleSystem32<C>
where
    C: Cpu + BusMasterComponent<Address = u32, Data = u8> + 'static,
{
    type Address = u32;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u32) -> u8 {
        self.ram[addr as usize]
    }

    fn write(&mut self, _master: BusMaster, addr: u32, data: u8) {
        self.ram[addr as usize] = data;
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState::default()
    }
}

// Type aliases for 32-bit CPUs
pub type SimpleI8088System = SimpleSystem32<I8088>;

// ---------------------------------------------------------------------------
// SimpleSystem68k<C> — word bus (16 MB flat RAM, 24-bit address space)
// ---------------------------------------------------------------------------

/// Test harness for word-bus CPUs (68000 family): byte-addressable RAM
/// serving 16-bit big-endian words at even addresses.
pub struct SimpleSystem68k<C>
where
    C: Cpu + BusMasterComponent<Address = u32, Data = u16> + 'static,
{
    pub cpu: C,
    ram: Vec<u8>,
    clock: u64,
}

impl<C> Default for SimpleSystem68k<C>
where
    C: Cpu + Default + BusMasterComponent<Address = u32, Data = u16> + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<C> SimpleSystem68k<C>
where
    C: Cpu + Default + BusMasterComponent<Address = u32, Data = u16> + 'static,
{
    pub fn new() -> Self {
        Self {
            cpu: C::default(),
            ram: vec![0; 0x100_0000], // 16 MB (full 24-bit address space)
            clock: 0,
        }
    }
}

impl<C> SimpleSystem68k<C>
where
    C: Cpu + BusMasterComponent<Address = u32, Data = u16> + 'static,
{
    pub fn tick(&mut self) {
        bus_split!(self, bus: u32 word => {
            self.cpu.tick_with_bus(bus, BusMaster::Cpu(0));
        });
        self.clock += 1;
    }

    /// Run one frame (16 667 cycles — 1 MHz CPU at 60 Hz).
    pub fn run_frame(&mut self) {
        for _ in 0..16667 {
            self.tick();
        }
    }

    /// Reset the CPU through the bus (loads SSP/PC from vectors 0/1).
    pub fn reset(&mut self) {
        bus_split!(self, bus: u32 word => {
            self.cpu.reset(bus, BusMaster::Cpu(0));
        });
    }

    /// Load bytes into RAM at `offset`.
    pub fn load_program(&mut self, offset: usize, data: &[u8]) {
        if offset + data.len() <= self.ram.len() {
            self.ram[offset..offset + data.len()].copy_from_slice(data);
        }
    }

    pub fn get_cpu_state(&self) -> C::Snapshot {
        self.cpu.snapshot()
    }

    pub fn read_ram(&self, addr: usize) -> u8 {
        self.ram.get(addr).copied().unwrap_or(0)
    }

    pub fn write_ram(&mut self, addr: usize, data: u8) {
        if let Some(cell) = self.ram.get_mut(addr) {
            *cell = data;
        }
    }

    pub fn clock(&self) -> u64 {
        self.clock
    }
}

impl<C> Bus for SimpleSystem68k<C>
where
    C: Cpu + BusMasterComponent<Address = u32, Data = u16> + 'static,
{
    type Address = u32;
    type Data = u16;

    fn read(&mut self, _master: BusMaster, addr: u32) -> u16 {
        let i = (addr & 0x00FF_FFFE) as usize;
        u16::from_be_bytes([self.ram[i], self.ram[i + 1]])
    }

    fn write(&mut self, _master: BusMaster, addr: u32, data: u16) {
        let i = (addr & 0x00FF_FFFE) as usize;
        self.ram[i..i + 2].copy_from_slice(&data.to_be_bytes());
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState::default()
    }
}

// Type aliases for word-bus CPUs
pub type Simple68000System = SimpleSystem68k<M68000>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_68000_system_reset_and_word_bus() {
        let mut sys = Simple68000System::new();
        // Vector 0 (SSP) = $00010000, vector 1 (PC) = $00000400
        sys.load_program(0, &[0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00]);
        sys.reset();

        let state = sys.get_cpu_state();
        assert_eq!(state.a[7], 0x0001_0000);
        assert_eq!(state.pc, 0x0000_0400);

        // Word bus serves big-endian words at even addresses (24-bit masked)
        sys.write_ram(0x2000, 0xBE);
        sys.write_ram(0x2001, 0xEF);
        let word = Bus::read(&mut sys, BusMaster::Cpu(0), 0xFF00_2001);
        assert_eq!(word, 0xBEEF);
    }
}
