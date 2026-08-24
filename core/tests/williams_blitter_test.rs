use phosphor_core::core::{Bus, BusMaster, InterruptState};
use phosphor_core::device::williams_blitter::WilliamsBlitter;

/// Simple flat-memory Bus for blitter unit tests.
struct TestBus {
    mem: Vec<u8>,
}

impl TestBus {
    fn new(size: usize) -> Self {
        Self {
            mem: vec![0u8; size],
        }
    }

    fn make_vram() -> Self {
        Self::new(0x10000) // 64KB address space
    }
}

impl Bus for TestBus {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        *self.mem.get(addr as usize).unwrap_or(&0)
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        if let Some(byte) = self.mem.get_mut(addr as usize) {
            *byte = data;
        }
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState::default()
    }
}

/// Run the blitter to completion, returning the number of DMA cycles consumed.
/// Each cycle transfers one byte; the cycle count reflects timing (1 or 2
/// clock cycles per DMA cycle depending on slow/fast mode).
fn run_to_completion(blitter: &mut WilliamsBlitter, bus: &mut TestBus) -> usize {
    let mut cycles = 0;
    while blitter.is_active() {
        blitter.do_dma_cycle(bus);
        cycles += 1;
        assert!(cycles < 100_000, "blit did not complete");
    }
    cycles
}

/// Run the blitter to completion, returning the total clock cycles consumed
/// (accounts for slow=2, fast=1 per byte).
fn run_to_completion_clocks(blitter: &mut WilliamsBlitter, bus: &mut TestBus) -> usize {
    let mut clocks = 0;
    while blitter.is_active() {
        clocks += blitter.do_dma_cycle(bus) as usize;
        assert!(clocks < 1_000_000, "blit did not complete");
    }
    clocks
}

/// SC1 XOR 4 encoding helper. Games pre-compensate for the SC1 XOR 4 bug
/// by XORing width/height with 4 before writing. `xor4(desired)` gives the
/// register value to write so the blitter recovers `desired` after its
/// internal XOR.
///
/// Reference (Sean Riddle): "bit 2 of the width and height to be inverted
/// (XOR 4)."
/// Source: https://seanriddle.com/blitter.html
fn xor4(val: u8) -> u8 {
    val ^ 4
}

// ===== MAME Control Byte Bits =====
// Reference: MAME `src/mame/midway/williamsblitter.h`
//
// Bit 0 (0x01): SRC_STRIDE_256
// Bit 1 (0x02): DST_STRIDE_256
// Bit 2 (0x04): SLOW
// Bit 3 (0x08): FOREGROUND_ONLY
// Bit 4 (0x10): SOLID
// Bit 5 (0x20): SHIFT
// Bit 6 (0x40): NO_ODD
// Bit 7 (0x80): NO_EVEN

// ===== Construction and Defaults =====

#[test]
fn test_not_active_initially() {
    let blitter = WilliamsBlitter::sc1();
    assert!(!blitter.is_active());
}

#[test]
fn test_sc1_is_default() {
    let a = WilliamsBlitter::new();
    let b = WilliamsBlitter::default();
    let c = WilliamsBlitter::sc1();
    assert_eq!(a.is_active(), b.is_active());
    assert_eq!(a.is_active(), c.is_active());
}

#[test]
fn test_sc2_not_active_initially() {
    let blitter = WilliamsBlitter::sc2();
    assert!(!blitter.is_active());
}

// ===== Blit Trigger =====

#[test]
fn test_write_control_triggers_blit() {
    // Writing to offset 0 ($CA00) triggers the blit.
    // Reference (Sean Riddle): "$CA00 Control Byte: Initiates blit."
    // Source: https://seanriddle.com/blitter.html
    let mut blitter = WilliamsBlitter::sc1();
    blitter.write_register(6, xor4(1)); // width = 1
    blitter.write_register(7, xor4(1)); // height = 1
    blitter.write_register(0, 0x00); // control — triggers blit
    assert!(blitter.is_active());
}

#[test]
fn test_write_height_does_not_trigger_blit() {
    // Only writing offset 0 triggers. Height (offset 7) is just data.
    let mut blitter = WilliamsBlitter::sc1();
    blitter.write_register(6, xor4(1));
    blitter.write_register(7, xor4(1)); // should NOT trigger
    assert!(!blitter.is_active());
}

// ===== Linear Copy (No Stride Bits, Control = 0x00) =====

#[test]
fn test_copy_1x1_linear() {
    // 1-byte copy with stride-1 for both source and dest.
    // Control = 0x00 (no stride bits, fast mode).
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    bus.mem[0x0100] = 0xAB;

    blitter.write_register(2, 0x01); // src = 0x0100
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x02); // dst = 0x0200
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(1)); // width = 1
    blitter.write_register(7, xor4(1)); // height = 1
    blitter.write_register(0, 0x00); // control: fast, no stride

    let cycles = run_to_completion(&mut blitter, &mut bus);
    assert_eq!(cycles, 1);
    assert_eq!(bus.mem[0x0200], 0xAB);
}

#[test]
fn test_copy_4x1_linear() {
    // 4-byte linear copy.
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    bus.mem[0x0100] = 0x11;
    bus.mem[0x0101] = 0x22;
    bus.mem[0x0102] = 0x33;
    bus.mem[0x0103] = 0x44;

    blitter.write_register(2, 0x01); // src = 0x0100
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x02); // dst = 0x0200
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(4)); // width = 4
    blitter.write_register(7, xor4(1)); // height = 1
    blitter.write_register(0, 0x00);

    let cycles = run_to_completion(&mut blitter, &mut bus);
    assert_eq!(cycles, 4);
    assert_eq!(bus.mem[0x0200], 0x11);
    assert_eq!(bus.mem[0x0201], 0x22);
    assert_eq!(bus.mem[0x0202], 0x33);
    assert_eq!(bus.mem[0x0203], 0x44);
}

#[test]
fn test_copy_3x2_linear() {
    // 3 columns x 2 rows, all stride-1.
    // Row advance = width = 3, so rows are contiguous.
    //   Row 0: 0x2000, 0x2001, 0x2002
    //   Row 1: 0x2003, 0x2004, 0x2005
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    for i in 0..6u8 {
        bus.mem[0x0100 + i as usize] = 0xA0 + i;
    }

    blitter.write_register(2, 0x01); // src = 0x0100
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x20); // dst = 0x2000
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(3)); // width = 3
    blitter.write_register(7, xor4(2)); // height = 2
    blitter.write_register(0, 0x00); // no stride bits

    let cycles = run_to_completion(&mut blitter, &mut bus);
    assert_eq!(cycles, 6);
    // Row 0
    assert_eq!(bus.mem[0x2000], 0xA0);
    assert_eq!(bus.mem[0x2001], 0xA1);
    assert_eq!(bus.mem[0x2002], 0xA2);
    // Row 1 (stride-1 row advance = width = 3)
    assert_eq!(bus.mem[0x2003], 0xA3);
    assert_eq!(bus.mem[0x2004], 0xA4);
    assert_eq!(bus.mem[0x2005], 0xA5);
}

// ===== Copy with DST_STRIDE_256 (Screen Mode) =====

#[test]
fn test_copy_3x1_dst_stride_256() {
    // 3-column copy with DST_STRIDE_256: columns advance by 256.
    // Reference (MAME): `dxadv = dst_stride_256 ? 0x100 : 1;`
    // Reference (Sean Riddle): "the next pair of pixels to the right are
    // 256 bytes away."
    // Source: https://seanriddle.com/blitter.html
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    bus.mem[0x0100] = 0x11;
    bus.mem[0x0101] = 0x22;
    bus.mem[0x0102] = 0x33;

    blitter.write_register(2, 0x01); // src = 0x0100
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x20); // dst = 0x2000
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(3)); // width = 3
    blitter.write_register(7, xor4(1)); // height = 1
    blitter.write_register(0, 0x02); // DST_STRIDE_256

    let cycles = run_to_completion(&mut blitter, &mut bus);
    assert_eq!(cycles, 3);
    // Columns at 0x2000, 0x2100, 0x2200 (each +256)
    assert_eq!(bus.mem[0x2000], 0x11);
    assert_eq!(bus.mem[0x2100], 0x22);
    assert_eq!(bus.mem[0x2200], 0x33);
}

#[test]
fn test_copy_2x3_dst_stride_256() {
    // 2 columns x 3 rows with DST_STRIDE_256.
    // Inner loop (columns): advance by 256 (right on screen).
    // Outer loop (rows): advance by +1 within page (down on screen).
    //
    // Reference (MAME): row advance for stride-256:
    //   `dstart = (dstart & 0xFF00) | ((dstart + 1) & 0x00FF)`
    // Reference (Sean Riddle): "successive bytes are displayed below one
    //   another; the next pair of pixels to the right are 256 bytes away."
    // Source: https://seanriddle.com/blitter.html
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();

    // Source: 6 bytes packed linearly
    bus.mem[0x0100] = 0xA1; // row 0, col 0
    bus.mem[0x0101] = 0xA2; // row 0, col 1
    bus.mem[0x0102] = 0xB1; // row 1, col 0
    bus.mem[0x0103] = 0xB2; // row 1, col 1
    bus.mem[0x0104] = 0xC1; // row 2, col 0
    bus.mem[0x0105] = 0xC2; // row 2, col 1

    blitter.write_register(2, 0x01); // src = 0x0100
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x20); // dst = 0x2000
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(2)); // width = 2
    blitter.write_register(7, xor4(3)); // height = 3
    blitter.write_register(0, 0x02); // DST_STRIDE_256

    let cycles = run_to_completion(&mut blitter, &mut bus);
    assert_eq!(cycles, 6);

    // Row 0: dst = 0x2000, 0x2100 (columns at +256)
    assert_eq!(bus.mem[0x2000], 0xA1);
    assert_eq!(bus.mem[0x2100], 0xA2);
    // Row 1: dst = 0x2001, 0x2101 (row advance +1 within page)
    assert_eq!(bus.mem[0x2001], 0xB1);
    assert_eq!(bus.mem[0x2101], 0xB2);
    // Row 2: dst = 0x2002, 0x2102
    assert_eq!(bus.mem[0x2002], 0xC1);
    assert_eq!(bus.mem[0x2102], 0xC2);
}

// ===== Copy with SRC_STRIDE_256 =====

#[test]
fn test_copy_src_stride_256() {
    // Source at stride-256: reads from column-major layout in VRAM.
    // Dest at stride-1: writes go linearly.
    // Control = SRC_STRIDE_256 (0x01).
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();

    // Source in column-major layout (stride-256)
    bus.mem[0x0100] = 0xAA; // row 0, col 0
    bus.mem[0x0200] = 0xBB; // row 0, col 1 (+256)
    bus.mem[0x0101] = 0xCC; // row 1, col 0 (+1 within page)
    bus.mem[0x0201] = 0xDD; // row 1, col 1

    blitter.write_register(2, 0x01); // src = 0x0100
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x30); // dst = 0x3000
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(2)); // width = 2
    blitter.write_register(7, xor4(2)); // height = 2
    blitter.write_register(0, 0x01); // SRC_STRIDE_256 only

    let cycles = run_to_completion(&mut blitter, &mut bus);
    assert_eq!(cycles, 4);

    // Dest is linear, row advance = width = 2
    assert_eq!(bus.mem[0x3000], 0xAA); // src[0x0100]
    assert_eq!(bus.mem[0x3001], 0xBB); // src[0x0200]
    assert_eq!(bus.mem[0x3002], 0xCC); // src[0x0101]
    assert_eq!(bus.mem[0x3003], 0xDD); // src[0x0201]
}

// ===== Solid Fill =====

#[test]
fn test_solid_fill_3x2_dst_stride_256() {
    // Solid fill with DST_STRIDE_256.
    // Control = SOLID (0x10) | DST_STRIDE_256 (0x02) = 0x12
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();

    blitter.write_register(1, 0x55); // solid_color
    blitter.write_register(4, 0x10); // dst = 0x1000
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(3)); // width = 3
    blitter.write_register(7, xor4(2)); // height = 2
    blitter.write_register(0, 0x12); // SOLID | DST_STRIDE_256

    let cycles = run_to_completion(&mut blitter, &mut bus);
    assert_eq!(cycles, 6);

    // Row 0: columns at +256
    assert_eq!(bus.mem[0x1000], 0x55);
    assert_eq!(bus.mem[0x1100], 0x55);
    assert_eq!(bus.mem[0x1200], 0x55);
    // Row 1: +1 within page
    assert_eq!(bus.mem[0x1001], 0x55);
    assert_eq!(bus.mem[0x1101], 0x55);
    assert_eq!(bus.mem[0x1201], 0x55);
}

#[test]
fn test_solid_fill_linear() {
    // Solid fill with stride-1 (linear).
    // Control = SOLID (0x10) = 0x10
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();

    blitter.write_register(1, 0x77); // solid_color
    blitter.write_register(4, 0x10); // dst = 0x1000
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(3)); // width = 3
    blitter.write_register(7, xor4(1)); // height = 1
    blitter.write_register(0, 0x10); // SOLID only

    run_to_completion(&mut blitter, &mut bus);

    assert_eq!(bus.mem[0x1000], 0x77);
    assert_eq!(bus.mem[0x1001], 0x77);
    assert_eq!(bus.mem[0x1002], 0x77);
}

#[test]
fn test_solid_source_always_advances() {
    // In solid mode, source address still advances (MAME: source += sxadv).
    // Dest should have solid color, not source data.
    // Reference (MAME): `source += sxadv` runs unconditionally.
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();

    bus.mem[0x0100] = 0xFF; // source data (should NOT appear in dest)
    bus.mem[0x0101] = 0xEE;

    blitter.write_register(1, 0x42); // solid_color
    blitter.write_register(2, 0x01); // src = 0x0100
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x20); // dst = 0x2000
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(2)); // width = 2
    blitter.write_register(7, xor4(1)); // height = 1
    blitter.write_register(0, 0x10); // SOLID, stride-1

    run_to_completion(&mut blitter, &mut bus);

    assert_eq!(bus.mem[0x2000], 0x42);
    assert_eq!(bus.mem[0x2001], 0x42);
}

// ===== Foreground-Only (Per-Nibble Transparency) =====

#[test]
fn test_fg_only_both_zero_preserves_dest() {
    // Source 0x00: both nibbles are color 0 → preserve entire dest byte.
    // Control = FOREGROUND_ONLY (0x08).
    //
    // Reference (Sean Riddle): "Color 0 is not copied to the destination,
    // allowing for transparency."
    // Source: https://seanriddle.com/blitter.html
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    bus.mem[0x0100] = 0x00;
    bus.mem[0x0200] = 0xCC;

    blitter.write_register(2, 0x01); // src = 0x0100
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x02); // dst = 0x0200
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(1));
    blitter.write_register(7, xor4(1));
    blitter.write_register(0, 0x08); // FOREGROUND_ONLY

    run_to_completion(&mut blitter, &mut bus);

    assert_eq!(
        bus.mem[0x0200], 0xCC,
        "zero source preserves dest in fg-only mode"
    );
}

#[test]
fn test_fg_only_both_nonzero_writes() {
    // Source 0x42: both nibbles non-zero → write entire byte.
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    bus.mem[0x0100] = 0x42;
    bus.mem[0x0200] = 0xCC;

    blitter.write_register(2, 0x01);
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x02);
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(1));
    blitter.write_register(7, xor4(1));
    blitter.write_register(0, 0x08); // FOREGROUND_ONLY

    run_to_completion(&mut blitter, &mut bus);

    assert_eq!(bus.mem[0x0200], 0x42);
}

#[test]
fn test_fg_only_upper_zero_preserves_upper() {
    // Source 0x0A: upper nibble = 0 (transparent), lower = A.
    // Dest 0xBC → result should be 0xBA.
    //
    // Reference (Sean Riddle): "pixels that are color 0 in the source data
    // remain untouched in the destination"
    // Source: https://seanriddle.com/blitter.html
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    bus.mem[0x0100] = 0x0A;
    bus.mem[0x0200] = 0xBC;

    blitter.write_register(2, 0x01);
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x02);
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(1));
    blitter.write_register(7, xor4(1));
    blitter.write_register(0, 0x08); // FOREGROUND_ONLY

    run_to_completion(&mut blitter, &mut bus);

    assert_eq!(
        bus.mem[0x0200], 0xBA,
        "upper nibble preserved (transparent), lower nibble written"
    );
}

#[test]
fn test_fg_only_lower_zero_preserves_lower() {
    // Source 0xA0: upper = A, lower = 0 (transparent).
    // Dest 0xBC → result should be 0xAC.
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    bus.mem[0x0100] = 0xA0;
    bus.mem[0x0200] = 0xBC;

    blitter.write_register(2, 0x01);
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x02);
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(1));
    blitter.write_register(7, xor4(1));
    blitter.write_register(0, 0x08); // FOREGROUND_ONLY

    run_to_completion(&mut blitter, &mut bus);

    assert_eq!(
        bus.mem[0x0200], 0xAC,
        "upper nibble written, lower nibble preserved (transparent)"
    );
}

// ===== NO_EVEN / NO_ODD (Pixel Suppression) =====

#[test]
fn test_no_even_suppresses_upper_nibble() {
    // NO_EVEN (bit 7, 0x80): suppress upper nibble (D7-D4) writes.
    // Source 0xAB, Dest 0xCD → upper kept (C), lower written (B) → 0xCB.
    //
    // Reference (MAME): NO_EVEN = bit 7, suppresses "even" pixel (upper nibble).
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    bus.mem[0x0100] = 0xAB;
    bus.mem[0x0200] = 0xCD;

    blitter.write_register(2, 0x01);
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x02);
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(1));
    blitter.write_register(7, xor4(1));
    blitter.write_register(0, 0x80); // NO_EVEN

    run_to_completion(&mut blitter, &mut bus);

    assert_eq!(
        bus.mem[0x0200], 0xCB,
        "NO_EVEN: upper nibble preserved, lower written"
    );
}

#[test]
fn test_no_odd_suppresses_lower_nibble() {
    // NO_ODD (bit 6, 0x40): suppress lower nibble (D3-D0) writes.
    // Source 0xAB, Dest 0xCD → upper written (A), lower kept (D) → 0xAD.
    //
    // Reference (MAME): NO_ODD = bit 6, suppresses "odd" pixel (lower nibble).
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    bus.mem[0x0100] = 0xAB;
    bus.mem[0x0200] = 0xCD;

    blitter.write_register(2, 0x01);
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x02);
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(1));
    blitter.write_register(7, xor4(1));
    blitter.write_register(0, 0x40); // NO_ODD

    run_to_completion(&mut blitter, &mut bus);

    assert_eq!(
        bus.mem[0x0200], 0xAD,
        "NO_ODD: upper nibble written, lower nibble preserved"
    );
}

#[test]
fn test_no_even_no_odd_no_writes() {
    // Both NO_EVEN (0x80) and NO_ODD (0x40) set: suppress all nibble writes.
    // Dest should be completely unchanged.
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    bus.mem[0x0100] = 0xAB;
    bus.mem[0x0200] = 0xCD;

    blitter.write_register(2, 0x01);
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x02);
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(1));
    blitter.write_register(7, xor4(1));
    blitter.write_register(0, 0xC0); // NO_EVEN | NO_ODD

    run_to_completion(&mut blitter, &mut bus);

    assert_eq!(bus.mem[0x0200], 0xCD, "both nibbles suppressed → no writes");
}

// ===== Shift Mode =====

#[test]
fn test_shift_mode() {
    // Shift mode (bit 5, 0x20): right-shift source data by one pixel (4 bits).
    // A shift register carries the previous raw source byte across bytes.
    //
    // Reference (Sean Riddle): "Shift the source data right one pixel when
    // writing it."
    // Source: https://seanriddle.com/blitter.html
    // Reference (MAME): `pixdata = (pixdata << 8) | rawval;
    //   blit_pixel(dest, (pixdata >> 4) & 0xff);`
    //
    // Source: 0xAB, 0xCD
    //   Byte 0: (0x00 << 8 | 0xAB) >> 4 = 0x0A
    //   Byte 1: (0xAB << 8 | 0xCD) >> 4 = 0xBC
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    bus.mem[0x0100] = 0xAB;
    bus.mem[0x0101] = 0xCD;

    blitter.write_register(2, 0x01); // src = 0x0100
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x02); // dst = 0x0200
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(2)); // width = 2
    blitter.write_register(7, xor4(1)); // height = 1
    blitter.write_register(0, 0x20); // SHIFT

    run_to_completion(&mut blitter, &mut bus);

    assert_eq!(bus.mem[0x0200], 0x0A, "first byte shifted");
    assert_eq!(bus.mem[0x0201], 0xBC, "second byte carries from first");
}

// ===== Solid + Foreground-Only =====

#[test]
fn test_solid_fg_zero_skips() {
    // Solid fill with color=0x00 + FOREGROUND_ONLY: all pixels are color 0,
    // so nothing should be written (all transparent).
    // Control = SOLID (0x10) | FOREGROUND_ONLY (0x08) = 0x18
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    bus.mem[0x0200] = 0xEE; // pre-existing dest data

    blitter.write_register(1, 0x00); // solid_color = 0x00
    blitter.write_register(4, 0x02); // dst = 0x0200
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(1));
    blitter.write_register(7, xor4(1));
    blitter.write_register(0, 0x18); // SOLID | FOREGROUND_ONLY

    run_to_completion(&mut blitter, &mut bus);

    assert_eq!(bus.mem[0x0200], 0xEE, "solid 0x00 + fg-only skips write");
}

// ===== XOR 4: SC1 vs SC2 =====

#[test]
fn test_sc1_xor4_width_height() {
    // SC1 applies XOR 4 internally. Raw register value 4 → XOR 4 → 0 → clamped to 1.
    //
    // Reference (Sean Riddle): "bit 2 of the width and height to be inverted
    // (XOR 4)."
    // Source: https://seanriddle.com/blitter.html
    // Reference (MAME): `int w = m_width ^ m_size_xor; if (w == 0) w = 1;`
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    bus.mem[0x0100] = 0xAB;

    blitter.write_register(2, 0x01); // src = 0x0100
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x02); // dst = 0x0200
    blitter.write_register(5, 0x00);
    blitter.write_register(6, 4); // raw 4 → XOR 4 → 0 → clamped to 1
    blitter.write_register(7, 4);
    blitter.write_register(0, 0x00);

    let cycles = run_to_completion(&mut blitter, &mut bus);
    assert_eq!(cycles, 1, "XOR 4 clamps to 1x1");
    assert_eq!(bus.mem[0x0200], 0xAB);
}

#[test]
fn test_sc2_no_xor4() {
    // SC2 has no XOR 4 bug (size_xor = 0).
    // Writing raw 4 → effective 4 (no XOR) → 4x4 = 16 bytes.
    let mut blitter = WilliamsBlitter::sc2();
    let mut bus = TestBus::make_vram();

    blitter.write_register(2, 0x01); // src = 0x0100
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x20); // dst = 0x2000
    blitter.write_register(5, 0x00);
    blitter.write_register(6, 4); // SC2: effective 4 (no XOR)
    blitter.write_register(7, 4);
    blitter.write_register(0, 0x00); // linear, fast

    let cycles = run_to_completion(&mut blitter, &mut bus);
    assert_eq!(cycles, 16, "SC2: raw 4 = effective 4x4 = 16 bytes");
}

#[test]
fn test_xor4_helper_round_trips() {
    // Verify xor4 is its own inverse: xor4(xor4(x)) == x
    for v in 0..=255u8 {
        assert_eq!(xor4(xor4(v)), v);
    }
}

// ===== Timing =====

#[test]
fn test_fast_mode_timing() {
    // Fast mode: SLOW bit (0x04) NOT set → 1 clock/byte.
    // Reference (Sean Riddle): "one 1 microsecond per byte."
    // Source: https://seanriddle.com/blitter.html
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();

    blitter.write_register(2, 0x01);
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x20);
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(4)); // width = 4
    blitter.write_register(7, xor4(1)); // height = 1
    blitter.write_register(0, 0x00); // fast (no SLOW bit)

    let clocks = run_to_completion_clocks(&mut blitter, &mut bus);
    assert_eq!(clocks, 4, "4 bytes x 1 clock = 4 clocks");
}

#[test]
fn test_slow_mode_timing() {
    // Slow mode: SLOW bit (0x04) set → 2 clocks/byte.
    // Reference (Sean Riddle): "Blits from RAM to RAM have to run at half
    // speed, 2 microseconds per byte."
    // Source: https://seanriddle.com/blitter.html
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();

    blitter.write_register(2, 0x01);
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x20);
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(4)); // width = 4
    blitter.write_register(7, xor4(1)); // height = 1
    blitter.write_register(0, 0x04); // SLOW

    let clocks = run_to_completion_clocks(&mut blitter, &mut bus);
    assert_eq!(clocks, 8, "4 bytes x 2 clocks = 8 clocks");
}

// ===== Width/Height Counting =====

#[test]
fn test_1_based_counting() {
    // Verify width/height produce exactly W*H DMA cycles (1-based counting).
    // Reference (MAME): `for (int x = 0; x < w; x++)` → exactly w iterations.
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();

    blitter.write_register(2, 0x01);
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x20);
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(5)); // width = 5
    blitter.write_register(7, xor4(3)); // height = 3
    blitter.write_register(0, 0x00);

    let cycles = run_to_completion(&mut blitter, &mut bus);
    assert_eq!(cycles, 15, "5x3 = 15 DMA cycles (1-based)");
}

// ===== Completion and Re-trigger =====

#[test]
fn test_completion_clears_active() {
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();

    blitter.write_register(6, xor4(1));
    blitter.write_register(7, xor4(1));
    blitter.write_register(0, 0x00);

    assert!(blitter.is_active());
    blitter.do_dma_cycle(&mut bus);
    assert!(!blitter.is_active());
}

#[test]
fn test_retrigger_after_completion() {
    // Registers retain values across blits.
    // Reference (Sean Riddle): "Omitting register writes reuses previous values."
    // Source: https://seanriddle.com/blitter.html
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();

    // First blit: copy 0xAA
    bus.mem[0x0100] = 0xAA;
    blitter.write_register(2, 0x01); // src = 0x0100
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x02); // dst = 0x0200
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(1));
    blitter.write_register(7, xor4(1));
    blitter.write_register(0, 0x00);

    run_to_completion(&mut blitter, &mut bus);
    assert_eq!(bus.mem[0x0200], 0xAA);
    assert!(!blitter.is_active());

    // Second blit: only change src and dst, reuse width/height
    bus.mem[0x0300] = 0xBB;
    blitter.write_register(2, 0x03); // src = 0x0300
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x04); // dst = 0x0400
    blitter.write_register(5, 0x00);
    blitter.write_register(0, 0x00); // re-trigger

    run_to_completion(&mut blitter, &mut bus);
    assert_eq!(bus.mem[0x0400], 0xBB);
}

// ===== Edge Cases =====

#[test]
fn test_inactive_dma_returns_zero() {
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    assert_eq!(
        blitter.do_dma_cycle(&mut bus),
        0,
        "Inactive blitter should return 0 cycles"
    );
}

#[test]
fn test_out_of_bounds_safe() {
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::new(256); // Tiny memory

    blitter.write_register(2, 0xFF); // src = 0xFF00 (beyond memory)
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0xFE); // dst = 0xFE00 (beyond memory)
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(1));
    blitter.write_register(7, xor4(1));
    blitter.write_register(0, 0x00);

    // Should not panic
    run_to_completion(&mut blitter, &mut bus);
}

// ===== Stride-256 Page Wrapping =====

#[test]
fn test_dst_stride_256_page_wrapping() {
    // Verify that row advance wraps within the 256-byte page.
    // Reference (MAME): `dstart = (dstart & 0xFF00) | ((dstart + 1) & 0x00FF)`
    //
    // Start at 0x20FE: row 0 = 0x20FE, row 1 = 0x20FF, row 2 = 0x2000 (wraps!)
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    bus.mem[0x0100] = 0x11;
    bus.mem[0x0101] = 0x22;
    bus.mem[0x0102] = 0x33;

    blitter.write_register(2, 0x01); // src = 0x0100
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x20); // dst = 0x20FE
    blitter.write_register(5, 0xFE);
    blitter.write_register(6, xor4(1)); // width = 1
    blitter.write_register(7, xor4(3)); // height = 3
    blitter.write_register(0, 0x02); // DST_STRIDE_256

    run_to_completion(&mut blitter, &mut bus);

    assert_eq!(bus.mem[0x20FE], 0x11, "row 0");
    assert_eq!(bus.mem[0x20FF], 0x22, "row 1");
    assert_eq!(bus.mem[0x2000], 0x33, "row 2 wraps to page start");
}

// ===== fg_only + NO_EVEN/NO_ODD Interaction =====

#[test]
fn test_fg_only_no_even_inverts_transparency() {
    // When fg_only + no_even are BOTH set, transparency inverts for the
    // upper nibble: zero source pixels ARE written, non-zero are suppressed.
    //
    // Reference (MAME blit_pixel):
    //   if (fg_only && !(srcdata & 0xf0)) { if (no_even) keepmask &= 0x0f; }
    //   else { if (!no_even) keepmask &= 0x0f; }
    //
    // Source 0x00, Dest 0xBC, control = FG_ONLY|NO_EVEN = 0x88
    //   Upper: fg_only=T, src=0, no_even=T → write (clear upper) → result upper = 0
    //   Lower: fg_only=T, src=0, no_odd=F → keep → result lower = C
    //   Result: 0x0C
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    bus.mem[0x0100] = 0x00;
    bus.mem[0x0200] = 0xBC;

    blitter.write_register(2, 0x01);
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x02);
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(1));
    blitter.write_register(7, xor4(1));
    blitter.write_register(0, 0x88); // FG_ONLY | NO_EVEN

    run_to_completion(&mut blitter, &mut bus);

    assert_eq!(
        bus.mem[0x0200], 0x0C,
        "fg_only+no_even: zero upper nibble WRITES (clears dest)"
    );
}

#[test]
fn test_fg_only_no_odd_inverts_transparency() {
    // fg_only + no_odd: zero lower nibble is written, non-zero is suppressed.
    // Source 0x00, Dest 0xBC, control = FG_ONLY|NO_ODD = 0x48
    //   Upper: fg_only=T, src=0, no_even=F → keep → B
    //   Lower: fg_only=T, src=0, no_odd=T → write (clear lower) → 0
    //   Result: 0xB0
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    bus.mem[0x0100] = 0x00;
    bus.mem[0x0200] = 0xBC;

    blitter.write_register(2, 0x01);
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x02);
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(1));
    blitter.write_register(7, xor4(1));
    blitter.write_register(0, 0x48); // FG_ONLY | NO_ODD

    run_to_completion(&mut blitter, &mut bus);

    assert_eq!(
        bus.mem[0x0200], 0xB0,
        "fg_only+no_odd: zero lower nibble WRITES (clears dest)"
    );
}

#[test]
fn test_fg_only_no_even_nonzero_suppressed() {
    // fg_only + no_even with non-zero source: upper nibble is SUPPRESSED.
    // Source 0xAB, Dest 0xCD, control = FG_ONLY|NO_EVEN = 0x88
    //   Upper: fg_only=T, src≠0, no_even=T → suppress → keep C
    //   Lower: fg_only=T, src≠0, no_odd=F → write → B
    //   Result: 0xCB
    let mut blitter = WilliamsBlitter::sc1();
    let mut bus = TestBus::make_vram();
    bus.mem[0x0100] = 0xAB;
    bus.mem[0x0200] = 0xCD;

    blitter.write_register(2, 0x01);
    blitter.write_register(3, 0x00);
    blitter.write_register(4, 0x02);
    blitter.write_register(5, 0x00);
    blitter.write_register(6, xor4(1));
    blitter.write_register(7, xor4(1));
    blitter.write_register(0, 0x88); // FG_ONLY | NO_EVEN

    run_to_completion(&mut blitter, &mut bus);

    assert_eq!(
        bus.mem[0x0200], 0xCB,
        "fg_only+no_even: non-zero upper nibble SUPPRESSED"
    );
}

// ===== MAME Cross-Validation =====

/// MAME-reference blit implementation for cross-validation.
/// Directly translates MAME's `blit_core` + `blit_pixel` from
/// `src/mame/midway/williamsblitter.cpp` (commit 0059ff9f) into Rust.
///
/// This is a standalone, synchronous implementation that operates on a
/// flat memory array — the same approach MAME uses internally.
#[allow(clippy::too_many_arguments)]
fn mame_reference_blit(
    mem: &mut [u8],
    control: u8,
    solid_color: u8,
    sstart_reg: u16,
    dstart_reg: u16,
    width_reg: u8,
    height_reg: u8,
    size_xor: u8,
) {
    let no_even = (control >> 7) & 1 != 0;
    let no_odd = (control >> 6) & 1 != 0;
    let shift = (control >> 5) & 1 != 0;
    let solid = (control >> 4) & 1 != 0;
    let fg_only = (control >> 3) & 1 != 0;
    let dst_stride_256 = (control >> 1) & 1 != 0;
    let src_stride_256 = control & 1 != 0;

    let mut w = (width_reg ^ size_xor) as i32;
    let mut h = (height_reg ^ size_xor) as i32;
    if w == 0 {
        w = 1;
    }
    if h == 0 {
        h = 1;
    }

    let sxadv: u16 = if src_stride_256 { 0x100 } else { 1 };
    let syadv: u16 = if src_stride_256 { 1 } else { w as u16 };
    let dxadv: u16 = if dst_stride_256 { 0x100 } else { 1 };
    let dyadv: u16 = if dst_stride_256 { 1 } else { w as u16 };

    let mut pixdata: i32 = 0;
    let mut sstart = sstart_reg;
    let mut dstart = dstart_reg;

    for _y in 0..h {
        let mut source = sstart;
        let mut dest = dstart;

        for _x in 0..w {
            let rawval = mem[source as usize];
            let srcdata: u8 = if shift {
                pixdata = (pixdata << 8) | rawval as i32;
                ((pixdata >> 4) & 0xff) as u8
            } else {
                rawval
            };

            // blit_pixel (MAME)
            let curpix = mem[dest as usize];
            let mut keepmask: u8 = 0xff;

            // even pixel (D7-D4)
            if fg_only && (srcdata & 0xf0) == 0 {
                if no_even {
                    keepmask &= 0x0f;
                }
            } else if !no_even {
                keepmask &= 0x0f;
            }

            // odd pixel (D3-D0)
            if fg_only && (srcdata & 0x0f) == 0 {
                if no_odd {
                    keepmask &= 0xf0;
                }
            } else if !no_odd {
                keepmask &= 0xf0;
            }

            let mut result = curpix & keepmask;
            if solid {
                result |= solid_color & !keepmask;
            } else {
                result |= srcdata & !keepmask;
            }
            mem[dest as usize] = result;

            source = source.wrapping_add(sxadv);
            dest = dest.wrapping_add(dxadv);
        }

        if dst_stride_256 {
            dstart = (dstart & 0xff00) | (dstart.wrapping_add(dyadv) & 0xff);
        } else {
            dstart = dstart.wrapping_add(dyadv);
        }

        if src_stride_256 {
            sstart = (sstart & 0xff00) | (sstart.wrapping_add(syadv) & 0xff);
        } else {
            sstart = sstart.wrapping_add(syadv);
        }
    }
}

/// MAME-reference single-pixel computation for exhaustive truth-table testing.
/// Returns the resulting byte after applying keepmask logic.
fn mame_reference_pixel(
    fg_only: bool,
    no_even: bool,
    no_odd: bool,
    solid: bool,
    solid_color: u8,
    srcdata: u8,
    curpix: u8,
) -> u8 {
    let mut keepmask: u8 = 0xff;

    // even pixel (D7-D4)
    if fg_only && (srcdata & 0xf0) == 0 {
        if no_even {
            keepmask &= 0x0f;
        }
    } else if !no_even {
        keepmask &= 0x0f;
    }

    // odd pixel (D3-D0)
    if fg_only && (srcdata & 0x0f) == 0 {
        if no_odd {
            keepmask &= 0xf0;
        }
    } else if !no_odd {
        keepmask &= 0xf0;
    }

    let mut result = curpix & keepmask;
    if solid {
        result |= solid_color & !keepmask;
    } else {
        result |= srcdata & !keepmask;
    }
    result
}

#[test]
fn test_exhaustive_pixel_truth_table() {
    // Exhaustively test all combinations of pixel-level flags against
    // the MAME reference implementation.
    //
    // Tests 16 flag combos × 256 src values × 4 dst values × 2 solid_colors
    // = 32768 test cases.
    let dst_values = [0x00u8, 0xCC, 0xFF, 0x37];
    let solid_colors = [0x00u8, 0x55];

    for fg_only in [false, true] {
        for no_even in [false, true] {
            for no_odd in [false, true] {
                for solid in [false, true] {
                    for &dst in &dst_values {
                        for &sc in &solid_colors {
                            for src in 0..=255u8 {
                                let expected = mame_reference_pixel(
                                    fg_only, no_even, no_odd, solid, sc, src, dst,
                                );

                                // Run our blitter with a 1x1 blit
                                let mut blitter = WilliamsBlitter::sc1();
                                let mut bus = TestBus::make_vram();
                                bus.mem[0x0100] = src;
                                bus.mem[0x0200] = dst;

                                let control = (no_even as u8) << 7
                                    | (no_odd as u8) << 6
                                    | (solid as u8) << 4
                                    | (fg_only as u8) << 3;

                                blitter.write_register(1, sc);
                                blitter.write_register(2, 0x01); // src = 0x0100
                                blitter.write_register(3, 0x00);
                                blitter.write_register(4, 0x02); // dst = 0x0200
                                blitter.write_register(5, 0x00);
                                blitter.write_register(6, xor4(1));
                                blitter.write_register(7, xor4(1));
                                blitter.write_register(0, control);

                                run_to_completion(&mut blitter, &mut bus);

                                assert_eq!(
                                    bus.mem[0x0200],
                                    expected,
                                    "pixel mismatch: fg={}, no_even={}, no_odd={}, solid={}, \
                                     sc=0x{:02X}, src=0x{:02X}, dst=0x{:02X}: \
                                     got 0x{:02X}, expected 0x{:02X}",
                                    fg_only,
                                    no_even,
                                    no_odd,
                                    solid,
                                    sc,
                                    src,
                                    dst,
                                    bus.mem[0x0200],
                                    expected
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn test_cross_validate_full_blit_against_mame() {
    // Cross-validate our cycle-by-cycle blitter against the MAME reference
    // for various control bytes, source patterns, and blit dimensions.
    //
    // Tests stride-1, stride-256, shift, solid, fg_only, and combinations.
    let test_configs: &[(u8, u8, u8, &str)] = &[
        // (control, width, height, description)
        (0x00, 3, 2, "linear copy"),
        (0x02, 2, 3, "dst_stride_256 copy"),
        (0x01, 2, 2, "src_stride_256 copy"),
        (0x03, 2, 2, "both strides"),
        (0x08, 3, 1, "fg_only linear"),
        (0x0A, 2, 2, "fg_only dst_stride_256"),
        (0x10, 3, 2, "solid linear"),
        (0x12, 2, 3, "solid dst_stride_256"),
        (0x20, 4, 1, "shift linear"),
        (0x22, 3, 2, "shift dst_stride_256"),
        (0x40, 2, 1, "no_odd"),
        (0x80, 2, 1, "no_even"),
        (0xC0, 2, 1, "no_even + no_odd"),
        (0x48, 2, 2, "fg_only + no_odd"),
        (0x88, 2, 2, "fg_only + no_even"),
        (0xC8, 2, 2, "fg_only + no_even + no_odd"),
        (0x18, 2, 2, "solid + fg_only"),
        (0x30, 3, 1, "shift + solid"),
    ];

    let src_patterns: &[u8] = &[0x00, 0x0F, 0xF0, 0xFF, 0xAB, 0x50, 0x05, 0x12];
    let dst_fills: &[u8] = &[0x00, 0xCC, 0xFF, 0x39];
    let solid_color: u8 = 0x55;
    let size_xor: u8 = 4; // SC1

    for &(control, w, h, desc) in test_configs {
        for &src_pat in src_patterns {
            for &dst_fill in dst_fills {
                // Prepare reference memory
                let mut ref_mem = vec![0u8; 0x10000];

                // Source data at 0x0100 — fill enough for any stride pattern
                for i in 0..256 {
                    // For stride-256 source, data is at 0x0100, 0x0200, etc.
                    for col in 0..8 {
                        let addr = 0x0100 + col * 256 + i;
                        if addr < 0x10000 {
                            ref_mem[addr] = src_pat;
                        }
                    }
                }
                // Also fill linearly
                for i in 0..64 {
                    ref_mem[0x0100 + i] = src_pat;
                }

                // Dest pre-fill at 0x4000 — cover stride-256 and linear areas
                for i in 0..256 {
                    for col in 0..8 {
                        let addr = 0x4000 + col * 256 + i;
                        if addr < 0x10000 {
                            ref_mem[addr] = dst_fill;
                        }
                    }
                }
                for i in 0..64 {
                    ref_mem[0x4000 + i] = dst_fill;
                }

                // Clone for our blitter
                let blit_mem = ref_mem.clone();

                // Encode width/height for SC1
                let w_reg = xor4(w);
                let h_reg = xor4(h);

                // Run MAME reference
                mame_reference_blit(
                    &mut ref_mem,
                    control,
                    solid_color,
                    0x0100,
                    0x4000,
                    w_reg,
                    h_reg,
                    size_xor,
                );

                // Run our blitter
                let mut blitter = WilliamsBlitter::sc1();
                let mut bus = TestBus { mem: blit_mem };
                blitter.write_register(1, solid_color);
                blitter.write_register(2, 0x01); // src_hi = 0x01
                blitter.write_register(3, 0x00); // src_lo = 0x00
                blitter.write_register(4, 0x40); // dst_hi = 0x40
                blitter.write_register(5, 0x00); // dst_lo = 0x00
                blitter.write_register(6, w_reg);
                blitter.write_register(7, h_reg);
                blitter.write_register(0, control);
                run_to_completion(&mut blitter, &mut bus);

                // Compare all memory
                for (addr, &ref_val) in ref_mem.iter().enumerate().take(0x10000usize) {
                    assert_eq!(
                        bus.mem[addr], ref_val,
                        "MAME mismatch at 0x{:04X}: ours=0x{:02X}, mame=0x{:02X} \
                         [{}] ctrl=0x{:02X} src=0x{:02X} dst=0x{:02X} {}x{}",
                        addr, bus.mem[addr], ref_val, desc, control, src_pat, dst_fill, w, h
                    );
                }
            }
        }
    }
}

// ===== Window Clip (Sinistar) =====

/// Program and run a 1x1 linear copy (control 0x00) from `src` to `dst`.
fn blit_1x1(blitter: &mut WilliamsBlitter, bus: &mut TestBus, src: u16, dst: u16, value: u8) {
    bus.mem[src as usize] = value;
    blitter.write_register(2, (src >> 8) as u8);
    blitter.write_register(3, src as u8);
    blitter.write_register(4, (dst >> 8) as u8);
    blitter.write_register(5, dst as u8);
    blitter.write_register(6, xor4(1));
    blitter.write_register(7, xor4(1));
    blitter.write_register(0, 0x00);
    run_to_completion(blitter, bus);
}

#[test]
fn window_clip_suppresses_blits_inside_clip_region() {
    let mut blitter = WilliamsBlitter::sc1_with_clip(0x7400);
    blitter.set_window_enable(true);
    let mut bus = TestBus::make_vram();
    // 0x7400 <= dst < 0xC000 is suppressed while the window is enabled.
    blit_1x1(&mut blitter, &mut bus, 0x0100, 0x8000, 0xAB);
    assert_eq!(
        bus.mem[0x8000], 0x00,
        "blit into clip region must be suppressed"
    );
}

#[test]
fn window_clip_allows_below_clip_and_non_vram() {
    let mut blitter = WilliamsBlitter::sc1_with_clip(0x7400);
    blitter.set_window_enable(true);
    let mut bus = TestBus::make_vram();
    // Below the clip address -> written.
    blit_1x1(&mut blitter, &mut bus, 0x0100, 0x0500, 0x11);
    assert_eq!(bus.mem[0x0500], 0x11);
    // Non-video RAM (>= 0xC000, e.g. Sinistar's $DXXX SRAM) -> always written.
    blit_1x1(&mut blitter, &mut bus, 0x0101, 0xD000, 0x22);
    assert_eq!(bus.mem[0xD000], 0x22);
}

#[test]
fn window_clip_disabled_writes_everywhere() {
    let mut blitter = WilliamsBlitter::sc1_with_clip(0x7400);
    // window_enable defaults to false.
    let mut bus = TestBus::make_vram();
    blit_1x1(&mut blitter, &mut bus, 0x0100, 0x8000, 0x33);
    assert_eq!(
        bus.mem[0x8000], 0x33,
        "disabled clip must write into the region"
    );
}

#[test]
fn reset_clears_window_enable_but_keeps_clip() {
    let mut blitter = WilliamsBlitter::sc1_with_clip(0x7400);
    blitter.set_window_enable(true);
    blitter.reset();
    assert!(!blitter.window_enable(), "reset clears window_enable");
    // Clip address is preserved: a blit into the region is still suppressed
    // once re-enabled.
    blitter.set_window_enable(true);
    let mut bus = TestBus::make_vram();
    blit_1x1(&mut blitter, &mut bus, 0x0100, 0x8000, 0x44);
    assert_eq!(bus.mem[0x8000], 0x00);
}
