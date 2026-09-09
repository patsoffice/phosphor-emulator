//! A byte access is one bus cycle, with one strobe.
//!
//! The 68000 has no A0 pin. It puts an even word address on the bus and selects
//! which half a transfer touches with UDS (the even byte, D8-D15) or LDS (the
//! odd byte, D0-D7). So a byte write is a single transfer that drives one half
//! and leaves the other alone; it is emphatically **not** a word read, a byte
//! patched in, and a word written back.
//!
//! That distinction is invisible in RAM and decisive everywhere else: the
//! phantom read is an access the hardware never performs, and on a write-only
//! or side-effecting register it is a bug that shows up as an intermittent,
//! hard-to-localize device misbehavior. These tests are the check on it, and
//! every one of them fails against a read-modify-write implementation.

use phosphor_core::core::component::BusMasterComponent;
use phosphor_core::core::{Bus, Bus16, BusMaster, bus::InterruptState};
use phosphor_core::cpu::m68000::M68000;

/// Every access, in order, exactly as the CPU issued it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Access {
    ReadWord(u32),
    WriteWord(u32, u16),
    ReadByte(u32),
    WriteByte(u32, u8),
}

/// A bus that records what it is asked to do and nothing more.
///
/// Deliberately not backed by word memory with byte helpers layered on top:
/// the point of these tests is *which calls arrive*, so the byte and word
/// paths stay distinguishable all the way down.
struct CountingBus {
    memory: Vec<u8>,
    log: Vec<Access>,
}

impl CountingBus {
    fn new() -> Self {
        Self {
            memory: vec![0; 0x10000],
            log: Vec::new(),
        }
    }

    fn load(&mut self, addr: u32, bytes: &[u8]) {
        let at = addr as usize;
        self.memory[at..at + bytes.len()].copy_from_slice(bytes);
    }

    fn reads(&self) -> usize {
        self.log
            .iter()
            .filter(|a| matches!(a, Access::ReadWord(_) | Access::ReadByte(_)))
            .count()
    }

    /// Accesses to the operand region, ignoring the instruction fetches that
    /// precede them. The test programs all live below 0x1000 and all operands
    /// sit at 0x2000 and above.
    fn operand_accesses(&self) -> Vec<Access> {
        self.log
            .iter()
            .copied()
            .filter(|a| {
                let addr = match a {
                    Access::ReadWord(x) | Access::WriteWord(x, _) => *x,
                    Access::ReadByte(x) | Access::WriteByte(x, _) => *x,
                };
                addr >= 0x2000
            })
            .collect()
    }
}

impl Bus for CountingBus {
    type Address = u32;
    type Data = u16;

    fn read(&mut self, _master: BusMaster, addr: u32) -> u16 {
        let i = (addr & 0xFFFE) as usize;
        self.log.push(Access::ReadWord(addr & 0xFFFE));
        u16::from_be_bytes([self.memory[i], self.memory[i + 1]])
    }

    fn write(&mut self, _master: BusMaster, addr: u32, data: u16) {
        let i = (addr & 0xFFFE) as usize;
        self.log.push(Access::WriteWord(addr & 0xFFFE, data));
        self.memory[i..i + 2].copy_from_slice(&data.to_be_bytes());
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState::default()
    }
}

impl Bus16 for CountingBus {
    fn read_byte(&mut self, _master: BusMaster, addr: u32) -> u8 {
        self.log.push(Access::ReadByte(addr));
        self.memory[(addr & 0xFFFF) as usize]
    }

    fn write_byte(&mut self, _master: BusMaster, addr: u32, data: u8) {
        self.log.push(Access::WriteByte(addr, data));
        self.memory[(addr & 0xFFFF) as usize] = data;
    }
}

/// Run one instruction from address 0 and hand back the bus.
fn run_one(program: &[u8], setup: impl FnOnce(&mut M68000, &mut CountingBus)) -> CountingBus {
    let mut cpu = M68000::new();
    let mut bus = CountingBus::new();
    bus.load(0, program);
    cpu.set_pc_flush(0);
    setup(&mut cpu, &mut bus);

    bus.log.clear();
    for _ in 0..64 {
        if cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)) {
            break;
        }
    }
    bus
}

/// **The acceptance criterion of the byte-strobe fix.** A write-only register
/// must see exactly one access for a byte write, and no read.
///
/// `MOVE.b D0,(A0)` with A0 at an even address: one write, on the upper half,
/// and the bus is never read at that address.
#[test]
fn a_byte_write_is_one_transfer_and_never_reads() {
    // 10 80 = MOVE.b D0,(A0)
    let bus = run_one(&[0x10, 0x80], |cpu, _| {
        cpu.d[0] = 0xAB;
        cpu.a[0] = 0x2000;
    });

    assert_eq!(
        bus.operand_accesses(),
        vec![Access::WriteByte(0x2000, 0xAB)],
        "a byte write is one strobed transfer, not a read-modify-write"
    );
}

/// The same at an odd address, which is the half a byte-wide peripheral on
/// D0-D7 actually answers.
#[test]
fn an_odd_byte_write_addresses_the_lower_half() {
    let bus = run_one(&[0x10, 0x80], |cpu, _| {
        cpu.d[0] = 0xCD;
        cpu.a[0] = 0x2001;
    });

    assert_eq!(
        bus.operand_accesses(),
        vec![Access::WriteByte(0x2001, 0xCD)],
        "the odd byte address reaches the bus as it stands"
    );
}

/// A byte write must not disturb the byte beside it. Under a
/// read-modify-write this passes for RAM and hides the phantom read; here it
/// pins the memory result while the test above pins the access count.
#[test]
fn a_byte_write_leaves_its_neighbor_alone() {
    let mut bus = run_one(&[0x10, 0x80], |cpu, bus| {
        cpu.d[0] = 0xAB;
        cpu.a[0] = 0x2000;
        bus.load(0x2000, &[0x11, 0x22]);
    });

    assert_eq!(bus.memory[0x2000], 0xAB, "the addressed byte changed");
    assert_eq!(bus.memory[0x2001], 0x22, "its neighbor did not");
    let _ = &mut bus;
}

/// A byte *read* is also one transfer, and it is a byte transfer rather than a
/// word read the CPU then halves.
#[test]
fn a_byte_read_is_one_byte_transfer() {
    let bus = run_one(&[0x10, 0x10], |cpu, bus| {
        // 10 10 = MOVE.b (A0),D0
        cpu.a[0] = 0x2001;
        bus.load(0x2000, &[0x11, 0x22]);
    });

    assert_eq!(
        bus.operand_accesses(),
        vec![Access::ReadByte(0x2001)],
        "a byte read asserts one strobe and takes one cycle"
    );
}

/// Word accesses are untouched by any of this: they still go through the
/// word path, at an even address, in one transfer.
#[test]
fn a_word_write_still_uses_the_word_path() {
    let bus = run_one(&[0x30, 0x80], |cpu, _| {
        // 30 80 = MOVE.w D0,(A0)
        cpu.d[0] = 0x1234;
        cpu.a[0] = 0x2000;
    });

    assert_eq!(
        bus.operand_accesses(),
        vec![Access::WriteWord(0x2000, 0x1234)],
        "a word transfer asserts both strobes and is one word access"
    );
}

/// The read-modify-write this replaced would show up here as an extra read on
/// the operand, so count them: a store reads nothing at its destination.
#[test]
fn storing_a_byte_performs_no_operand_read_at_all() {
    let bus = run_one(&[0x10, 0x80], |cpu, _| {
        cpu.d[0] = 0x5A;
        cpu.a[0] = 0x2000;
    });

    let operand_reads = bus
        .operand_accesses()
        .into_iter()
        .filter(|a| matches!(a, Access::ReadWord(_) | Access::ReadByte(_)))
        .count();
    assert_eq!(operand_reads, 0, "a byte store reads nothing");
    assert!(bus.reads() > 0, "but the instruction itself was fetched");
}
