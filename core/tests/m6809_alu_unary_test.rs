use phosphor_core::core::{Bus, BusMaster, BusMasterComponent, bus::InterruptState};
use phosphor_core::cpu::m6809::CcFlag;
use phosphor_core::cpu::m6809::M6809;
mod common;
use common::TestBus;

/// Bus that records every write address, so a test can assert that an
/// instruction performed no store (e.g. TST must not write back).
struct WriteLogBus {
    memory: [u8; 0x10000],
    writes: Vec<u16>,
}

impl WriteLogBus {
    fn new() -> Self {
        Self {
            memory: [0; 0x10000],
            writes: Vec::new(),
        }
    }
    fn load(&mut self, addr: u16, data: &[u8]) {
        let s = addr as usize;
        self.memory[s..s + data.len()].copy_from_slice(data);
    }
}

impl Bus for WriteLogBus {
    type Address = u16;
    type Data = u8;
    fn read(&mut self, _m: BusMaster, addr: u16) -> u8 {
        self.memory[addr as usize]
    }
    fn write(&mut self, _m: BusMaster, addr: u16, data: u8) {
        self.writes.push(addr);
        self.memory[addr as usize] = data;
    }
    fn is_halted_for(&self, _m: BusMaster) -> bool {
        false
    }
    fn check_interrupts(&mut self, _t: BusMaster) -> InterruptState {
        InterruptState::default()
    }
}

#[test]
fn test_negate() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // LDA #$01, NEGA, LDB #$80, NEGB
    bus.load(0, &[0x86, 0x01, 0x40, 0xC6, 0x80, 0x50]);

    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)); // LDA #$01

    // NEGA: 0 - 1 = -1 (0xFF)
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    assert_eq!(cpu.a, 0xFF);
    assert_eq!(cpu.cc & (CcFlag::N as u8), CcFlag::N as u8);
    assert_eq!(cpu.cc & (CcFlag::C as u8), CcFlag::C as u8); // Borrow occurred
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);

    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)); // LDB #$80 (-128)

    // NEGB: 0 - (-128) = +128 (Overflow!)
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    assert_eq!(cpu.b, 0x80); // Result is still 0x80
    assert_eq!(cpu.cc & (CcFlag::V as u8), CcFlag::V as u8); // Overflow set
    assert_eq!(cpu.cc & (CcFlag::N as u8), CcFlag::N as u8);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);
}

#[test]
fn test_complement() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // LDA #$AA, COMA, LDB #$00, COMB
    bus.load(0, &[0x86, 0xAA, 0x43, 0xC6, 0x00, 0x53]);

    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)); // LDA #$AA

    // COMA: ~0xAA = 0x55
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    assert_eq!(cpu.a, 0x55);
    assert_eq!(cpu.cc & (CcFlag::C as u8), CcFlag::C as u8); // C always set
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0); // V always clear
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);

    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)); // LDB #$00

    // COMB: ~0x00 = 0xFF
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    assert_eq!(cpu.b, 0xFF);
    assert_eq!(cpu.cc & (CcFlag::C as u8), CcFlag::C as u8);
    assert_eq!(cpu.cc & (CcFlag::N as u8), CcFlag::N as u8);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);
}

#[test]
fn test_clear() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // LDA #$FF, CLRA, LDB #$42, CLRB
    bus.load(0, &[0x86, 0xFF, 0x4F, 0xC6, 0x42, 0x5F]);

    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)); // LDA #$FF

    // CLRA: A = 0
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    assert_eq!(cpu.a, 0x00);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), CcFlag::Z as u8);
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::C as u8), 0);

    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)); // LDB #$42

    // CLRB: B = 0
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    assert_eq!(cpu.b, 0x00);
    assert_eq!(cpu.cc & (CcFlag::Z as u8), CcFlag::Z as u8);
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::C as u8), 0);
}

#[test]
fn test_increment() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // LDA #$7F, INCA, LDB #$FF, INCB
    bus.load(0, &[0x86, 0x7F, 0x4C, 0xC6, 0xFF, 0x5C]);

    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)); // LDA #$7F

    // INCA: 0x7F + 1 = 0x80 (signed overflow: positive -> negative)
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    assert_eq!(cpu.a, 0x80);
    assert_eq!(
        cpu.cc & (CcFlag::V as u8),
        CcFlag::V as u8,
        "Overflow should be set (0x7F -> 0x80)"
    );
    assert_eq!(
        cpu.cc & (CcFlag::N as u8),
        CcFlag::N as u8,
        "Negative should be set"
    );
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);

    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)); // LDB #$FF

    // INCB: 0xFF + 1 = 0x00 (wraps to zero)
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    assert_eq!(cpu.b, 0x00);
    assert_eq!(
        cpu.cc & (CcFlag::Z as u8),
        CcFlag::Z as u8,
        "Zero should be set"
    );
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0);
    assert_eq!(
        cpu.cc & (CcFlag::V as u8),
        0,
        "Overflow should be clear (0xFF -> 0x00 is not signed overflow)"
    );
}

#[test]
fn test_decrement() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // LDA #$80, DECA, LDB #$01, DECB
    bus.load(0, &[0x86, 0x80, 0x4A, 0xC6, 0x01, 0x5A]);

    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)); // LDA #$80

    // DECA: 0x80 - 1 = 0x7F (signed overflow: negative -> positive)
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    assert_eq!(cpu.a, 0x7F);
    assert_eq!(
        cpu.cc & (CcFlag::V as u8),
        CcFlag::V as u8,
        "Overflow should be set (0x80 -> 0x7F)"
    );
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0, "Negative should be clear");
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0);

    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)); // LDB #$01

    // DECB: 0x01 - 1 = 0x00
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    assert_eq!(cpu.b, 0x00);
    assert_eq!(
        cpu.cc & (CcFlag::Z as u8),
        CcFlag::Z as u8,
        "Zero should be set"
    );
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0);
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0);
}

#[test]
fn test_test_register() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // LDA #$80, TSTA, LDB #$00, TSTB
    bus.load(0, &[0x86, 0x80, 0x4D, 0xC6, 0x00, 0x5D]);

    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)); // LDA #$80

    // TSTA: test A (0x80 is negative, not zero)
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    assert_eq!(cpu.a, 0x80, "A should be unchanged");
    assert_eq!(
        cpu.cc & (CcFlag::N as u8),
        CcFlag::N as u8,
        "Negative should be set"
    );
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0, "Zero should be clear");
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0, "Overflow always clear");

    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0)); // LDB #$00

    // TSTB: test B (0x00 is zero, not negative)
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    assert_eq!(cpu.b, 0x00, "B should be unchanged");
    assert_eq!(
        cpu.cc & (CcFlag::Z as u8),
        CcFlag::Z as u8,
        "Zero should be set"
    );
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0, "Negative should be clear");
    assert_eq!(cpu.cc & (CcFlag::V as u8), 0, "Overflow always clear");
}

/// TST <memory> must NOT write back. On a real MC6809 the memory TST forms
/// (direct 0x0D, indexed 0x6D, extended 0x7D) read the operand, set flags, and
/// spend their remaining cycles on dummy VMA reads — they issue no store. A
/// spurious write-back is invisible in plain RAM (same value) but corrupts any
/// address where reads and writes decode differently: on Williams hardware a
/// banked read returns ROM while the write lands in video RAM, so `TST ,U+`
/// walking a ROM message table smeared ROM bytes into VRAM (regression: the
/// Joust boot-test "vertical strip" at frame 447).
#[test]
fn test_tst_memory_does_not_write_back() {
    // Direct: TST $40
    {
        let mut cpu = M6809::new();
        let mut bus = WriteLogBus::new();
        bus.load(0, &[0x0D, 0x40]);
        bus.memory[0x0040] = 0x80; // negative, non-zero
        for _ in 0..6 {
            cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
        }
        assert!(
            bus.writes.is_empty(),
            "TST direct must not write, got {:?}",
            bus.writes
        );
        assert_eq!(bus.memory[0x0040], 0x80, "operand unchanged");
        assert_eq!(cpu.cc & (CcFlag::N as u8), CcFlag::N as u8, "N set");
        assert_eq!(cpu.cc & (CcFlag::Z as u8), 0, "Z clear");
    }
    // Extended: TST $1234
    {
        let mut cpu = M6809::new();
        let mut bus = WriteLogBus::new();
        bus.load(0, &[0x7D, 0x12, 0x34]);
        bus.memory[0x1234] = 0x00; // zero
        for _ in 0..7 {
            cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
        }
        assert!(
            bus.writes.is_empty(),
            "TST extended must not write, got {:?}",
            bus.writes
        );
        assert_eq!(cpu.cc & (CcFlag::Z as u8), CcFlag::Z as u8, "Z set");
    }
    // Indexed: LDX #$0050 ; TST ,X
    {
        let mut cpu = M6809::new();
        let mut bus = WriteLogBus::new();
        bus.load(0, &[0x8E, 0x00, 0x50, 0x6D, 0x84]);
        bus.memory[0x0050] = 0x01;
        for _ in 0..9 {
            cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
        }
        assert!(
            bus.writes.is_empty(),
            "TST indexed must not write, got {:?}",
            bus.writes
        );
        assert_eq!(bus.memory[0x0050], 0x01, "operand unchanged");
    }
}

/// Sanity check that `WriteLogBus` actually observes stores: a real RMW op
/// (INC extended) must record exactly one write to its operand. Guards the
/// TST test above from silently passing because writes go unrecorded.
#[test]
fn test_inc_extended_does_write_back() {
    let mut cpu = M6809::new();
    let mut bus = WriteLogBus::new();
    bus.load(0, &[0x7C, 0x12, 0x34]); // INC $1234
    bus.memory[0x1234] = 0x0F;
    for _ in 0..7 {
        cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    }
    assert_eq!(bus.writes, vec![0x1234], "INC must write back exactly once");
    assert_eq!(bus.memory[0x1234], 0x10, "operand incremented");
}

/// DAA must report the carry of the whole BCD addition, not just of its own
/// adjustment step: 99 + 99 = 198, and the hundreds digit lives in C.
///
/// `ADDA #$99` on A=$99 leaves A=$32 with H and C set. The adjustment then
/// adds $66, giving $98 with no carry out of bit 7 — so if DAA recomputed C
/// from its own addition it would wrongly clear it and lose the hundreds
/// digit. The datasheet defines C as "set if a carry is generated *or* if the
/// carry bit was set before the operation".
#[test]
fn test_daa_keeps_incoming_carry() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    // LDA #$99 ; ADDA #$99 ; DAA
    bus.load(0, &[0x86, 0x99, 0x8B, 0x99, 0x19]);

    for _ in 0..4 {
        cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    }
    assert_eq!(cpu.a, 0x32, "binary sum before adjustment");
    assert_eq!(cpu.cc & (CcFlag::H as u8), CcFlag::H as u8, "H set by ADDA");
    assert_eq!(cpu.cc & (CcFlag::C as u8), CcFlag::C as u8, "C set by ADDA");
    assert_eq!(cpu.cc & (CcFlag::V as u8), CcFlag::V as u8, "V set by ADDA");

    for _ in 0..2 {
        cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    }
    assert_eq!(cpu.a, 0x98, "BCD 98");
    assert_eq!(
        cpu.cc & (CcFlag::C as u8),
        CcFlag::C as u8,
        "C stays set — it is the hundreds digit of 198"
    );
    assert_eq!(
        cpu.cc & (CcFlag::V as u8),
        0,
        "V cleared (documented undefined)"
    );
}

/// DAA sets C when the adjustment itself carries out of bit 7.
#[test]
fn test_daa_sets_carry_from_adjustment() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x19]); // DAA
    cpu.a = 0xAA; // both nibbles > 9, so the correction is $66
    cpu.cc = 0;

    for _ in 0..2 {
        cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    }
    assert_eq!(cpu.a, 0x10, "$AA + $66 = $110, truncated to $10");
    assert_eq!(cpu.cc & (CcFlag::C as u8), CcFlag::C as u8, "C set");
    assert_eq!(cpu.cc & (CcFlag::N as u8), 0, "N clear");
    assert_eq!(cpu.cc & (CcFlag::Z as u8), 0, "Z clear");
}

/// DAA with nothing to adjust and no carry in leaves C clear — the sticky
/// carry above must not degrade into an unconditional set.
#[test]
fn test_daa_valid_bcd_leaves_carry_clear() {
    let mut cpu = M6809::new();
    let mut bus = TestBus::new();
    bus.load(0, &[0x19]); // DAA
    cpu.a = 0x12;
    cpu.cc = 0;

    for _ in 0..2 {
        cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
    }
    assert_eq!(cpu.a, 0x12, "already valid BCD, unchanged");
    assert_eq!(cpu.cc & (CcFlag::C as u8), 0, "C clear");
}
