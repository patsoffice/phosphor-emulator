use phosphor_core::core::{Bus, BusMaster, bus::InterruptState};

/// Minimal bus for testing: flat 64KB read/write memory, no peripherals.
pub struct TestBus {
    pub memory: [u8; 0x10000],
    pub nmi: bool,
    pub irq: bool,
}

impl TestBus {
    pub fn new() -> Self {
        Self {
            memory: [0; 0x10000],
            nmi: false,
            irq: false,
        }
    }

    pub fn load(&mut self, addr: u16, data: &[u8]) {
        let start = addr as usize;
        self.memory[start..start + data.len()].copy_from_slice(data);
    }
}

impl Bus for TestBus {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        self.memory[addr as usize]
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        self.memory[addr as usize] = data;
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }
    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            nmi: self.nmi,
            irq: self.irq,
            firq: false,
            irq_vector: 0xFF,
            irq_level: 0,
        }
    }
}

/// Minimal word bus for 68000 testing: flat 16 MB byte memory served as
/// 16-bit big-endian words at even addresses, no peripherals.
#[allow(dead_code)] // not every test binary that includes `common` uses it
pub struct TestBus68k {
    pub memory: Vec<u8>,
    pub irq_level: u8,
}

#[allow(dead_code)]
impl TestBus68k {
    pub fn new() -> Self {
        Self {
            memory: vec![0; 0x100_0000],
            irq_level: 0,
        }
    }

    /// Load bytes at a byte address.
    pub fn load(&mut self, addr: u32, data: &[u8]) {
        let start = (addr & 0x00FF_FFFF) as usize;
        self.memory[start..start + data.len()].copy_from_slice(data);
    }
}

impl Bus for TestBus68k {
    type Address = u32;
    type Data = u16;

    fn read(&mut self, _master: BusMaster, addr: u32) -> u16 {
        let i = (addr & 0x00FF_FFFE) as usize;
        u16::from_be_bytes([self.memory[i], self.memory[i + 1]])
    }

    fn write(&mut self, _master: BusMaster, addr: u32, data: u16) {
        let i = (addr & 0x00FF_FFFE) as usize;
        self.memory[i..i + 2].copy_from_slice(&data.to_be_bytes());
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            irq_level: self.irq_level,
            ..Default::default()
        }
    }
}
