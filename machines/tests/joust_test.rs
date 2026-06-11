use phosphor_core::core::machine::{InputReceiver, MachineCore, Renderable};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::m6809::CcFlag;
use phosphor_machines::joust::JoustSystem;

// =================================================================
// Machine Trait Tests
// =================================================================

#[test]
fn test_display_size() {
    let sys = JoustSystem::new();
    assert_eq!(sys.display_size(), (292, 240));
}

#[test]
fn test_input_map_has_all_buttons() {
    let sys = JoustSystem::new();
    let map = sys.input_map();
    assert_eq!(map.len(), 9); // 8 player buttons + coin
    for button in map {
        assert!(!button.name.is_empty());
    }
}

#[test]
fn test_render_frame_correct_size() {
    let sys = JoustSystem::new();
    let (w, h) = sys.display_size();
    let mut buffer = vec![0u8; (w * h * 3) as usize];
    sys.render_frame(&mut buffer); // Should not panic
}

#[test]
fn test_render_frame_uses_palette() {
    let mut sys = JoustSystem::new();
    // Park both CPUs in BRA * loops so they don't corrupt VRAM during the frame
    sys.board.write_video_ram(0, 0x20); // BRA opcode (main CPU at PC=0)
    sys.board.write_video_ram(1, 0xFE); // offset -2
    sys.write(BusMaster::Cpu(1), 0x0000, 0x20); // sound CPU BRA *
    sys.write(BusMaster::Cpu(1), 0x0001, 0xFE);

    // Set palette entry 5 to a known BBGGGRRR color: R=7, G=0, B=3 => 0b11_000_111 = 0xC7
    sys.write(BusMaster::Cpu(0), 0xC005, 0xC7);

    // Screen pixel (0,0) maps to VRAM at byte_column=3, row=7 (crop offset 6,7).
    // pixel_x=6 is even, so the upper nibble is used.
    // VRAM address = 3 * 256 + 7 = 0x0307
    sys.board.write_video_ram(0x0307, 0x50);

    // Run a full frame so render_scanline populates the scanline buffer
    sys.run_frame();

    let (w, h) = sys.display_size();
    let mut buffer = vec![0u8; (w * h * 3) as usize];
    sys.render_frame(&mut buffer);

    // Pixel at (0,0) should have color from palette entry 5
    // R=7 → 255 (resistor weight), G=0 → 0, B=3 → 255
    assert_eq!(buffer[0], 255); // R
    assert_eq!(buffer[1], 0); // G
    assert_eq!(buffer[2], 255); // B
}

#[test]
fn test_render_frame_two_pixels_per_byte() {
    let mut sys = JoustSystem::new();
    // Park both CPUs in BRA * loops so they don't corrupt VRAM during the frame
    sys.board.write_video_ram(0, 0x20);
    sys.board.write_video_ram(1, 0xFE);
    sys.write(BusMaster::Cpu(1), 0x0000, 0x20);
    sys.write(BusMaster::Cpu(1), 0x0001, 0xFE);

    // Palette entry 3 (BBGGGRRR): R=0, G=7, B=0 => 0b00_111_000 = 0x38
    // Palette entry 9 (BBGGGRRR): R=4, G=4, B=2 => 0b10_100_100 = 0xA4
    sys.write(BusMaster::Cpu(0), 0xC003, 0x38);
    sys.write(BusMaster::Cpu(0), 0xC009, 0xA4);

    // Screen pixels (0,0) and (1,0) share the same VRAM byte at byte_column=3, row=7.
    // pixel_x=6 (even) reads upper nibble, pixel_x=7 (odd) reads lower nibble.
    // Write 0x39 → upper nibble=3 (palette 3), lower nibble=9 (palette 9).
    sys.board.write_video_ram(0x0307, 0x39);

    // Run a full frame so render_scanline populates the scanline buffer
    sys.run_frame();

    let (w, _h) = sys.display_size();
    let mut buffer = vec![0u8; w as usize * 240 * 3];
    sys.render_frame(&mut buffer);

    // Pixel (0,0) = palette entry 3: R=0, G=255, B=0
    assert_eq!(buffer[0], 0);
    assert_eq!(buffer[1], 255);
    assert_eq!(buffer[2], 0);

    // Pixel (1,0) = palette entry 9: R=137, G=137, B=160 (resistor-weighted)
    let px1 = 3; // offset for pixel x=1
    assert_eq!(buffer[px1], 137); // R (RG_LUT[4])
    assert_eq!(buffer[px1 + 1], 137); // G (RG_LUT[4])
    assert_eq!(buffer[px1 + 2], 160); // B (B_LUT[2])
}

#[test]
fn test_render_frame_black_palette() {
    let sys = JoustSystem::new();
    // All palette entries are 0 (black), all video RAM is 0
    let (w, h) = sys.display_size();
    let mut buffer = vec![0xFFu8; (w * h * 3) as usize];
    sys.render_frame(&mut buffer);

    // Every pixel should be black (0,0,0)
    assert!(buffer.iter().all(|&b| b == 0));
}

#[test]
fn test_set_input_active_high() {
    use phosphor_machines::joust::INPUT_P1_LEFT;
    let mut sys = JoustSystem::new();

    // Configure Widget PIA to read Port A data: set DDRA=0 (all input), then CRA.2=1
    sys.write(BusMaster::Cpu(0), 0xC804, 0x00); // DDRA = 0 (all input)
    sys.write(BusMaster::Cpu(0), 0xC805, 0x04); // CRA: bit 2 = 1 (data select)

    // All buttons start released (0x00), Port A should read 0x00
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val, 0x00, "All released should be 0x00 (active-high)");

    // Press P1 Left (button 0 → mux bit 0) — mux defaults to P2 (CB2=0),
    // so P1 controls don't appear until CB2=1. Set CB2 output high first.
    sys.write(BusMaster::Cpu(0), 0xC807, 0x3C); // CRB: bits 5:4:3 = 111 → CB2 output high
    sys.set_input(INPUT_P1_LEFT, true);
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0x01, 0x01, "P1 Left pressed should set bit 0");

    sys.set_input(INPUT_P1_LEFT, false);
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0x01, 0x00, "P1 Left released should clear bit 0");
}

#[test]
fn test_set_input_multiple_buttons() {
    use phosphor_machines::joust::{INPUT_P1_FLAP, INPUT_P1_LEFT};
    let mut sys = JoustSystem::new();

    // Set CB2 high to select P1 controls
    sys.write(BusMaster::Cpu(0), 0xC804, 0x00); // DDRA = 0
    sys.write(BusMaster::Cpu(0), 0xC805, 0x04); // CRA data select
    sys.write(BusMaster::Cpu(0), 0xC807, 0x3C); // CRB: CB2 output high (P1)

    // Press P1 Left and P1 Flap simultaneously
    sys.set_input(INPUT_P1_LEFT, true);
    sys.set_input(INPUT_P1_FLAP, true);
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0x05, 0x05, "Left (bit 0) and Flap (bit 2) both set");

    // Release P1 Left, P1 Flap still held
    sys.set_input(INPUT_P1_LEFT, false);
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0x05, 0x04, "Only Flap (bit 2) should remain");

    sys.set_input(INPUT_P1_FLAP, false);
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0x07, 0x00, "All released");
}

#[test]
fn test_set_input_coin() {
    use phosphor_machines::joust::INPUT_COIN;
    let mut sys = JoustSystem::new();

    // Configure ROM PIA Port A as all input, data select
    sys.write(BusMaster::Cpu(0), 0xC80C, 0x00); // DDRA = 0
    sys.write(BusMaster::Cpu(0), 0xC80D, 0x04); // CRA data select

    // Coin triggers ROM PIA Port A bit 4 (active-high)
    sys.set_input(INPUT_COIN, true);
    let val = sys.read(BusMaster::Cpu(0), 0xC80C);
    assert_eq!(val & 0x10, 0x10, "Coin pressed should set bit 4");

    sys.set_input(INPUT_COIN, false);
    let val = sys.read(BusMaster::Cpu(0), 0xC80C);
    assert_eq!(val & 0x10, 0x00, "Coin released should clear bit 4");
}

#[test]
fn test_input_muxing() {
    use phosphor_machines::joust::{INPUT_P1_RIGHT, INPUT_P2_LEFT};
    let mut sys = JoustSystem::new();

    // Configure Widget PIA Port A as input, data select
    sys.write(BusMaster::Cpu(0), 0xC804, 0x00); // DDRA = 0
    sys.write(BusMaster::Cpu(0), 0xC805, 0x04); // CRA data select

    // Press both P1 Right and P2 Left
    sys.set_input(INPUT_P1_RIGHT, true);
    sys.set_input(INPUT_P2_LEFT, true);

    // CB2=0 (default) selects P2: should see P2 Left (bit 0)
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0x03, 0x01, "CB2=0 → P2 selected, P2 Left on bit 0");

    // Set CB2 high → selects P1: should see P1 Right (bit 1)
    sys.write(BusMaster::Cpu(0), 0xC807, 0x3C); // CRB: CB2 output high
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0x03, 0x02, "CB2=1 → P1 selected, P1 Right on bit 1");

    sys.set_input(INPUT_P1_RIGHT, false);
    sys.set_input(INPUT_P2_LEFT, false);
}

// =================================================================
// Memory Map Routing Tests
// =================================================================

#[test]
fn test_video_ram_read_write() {
    let mut sys = JoustSystem::new();
    sys.board.write_video_ram(0x1234, 0xAB);
    assert_eq!(sys.board.read_video_ram(0x1234), 0xAB);
}

#[test]
fn test_video_ram_full_range() {
    let mut sys = JoustSystem::new();
    sys.board.write_video_ram(0x0000, 0x11);
    sys.board.write_video_ram(0xBFFF, 0x22);
    assert_eq!(sys.board.read_video_ram(0x0000), 0x11);
    assert_eq!(sys.board.read_video_ram(0xBFFF), 0x22);
}

#[test]
fn test_video_ram_via_bus() {
    let mut sys = JoustSystem::new();
    sys.write(BusMaster::Cpu(0), 0x1234, 0xCD);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x1234), 0xCD);
    assert_eq!(sys.board.read_video_ram(0x1234), 0xCD);
}

#[test]
fn test_palette_ram_read_write() {
    let mut sys = JoustSystem::new();
    sys.write(BusMaster::Cpu(0), 0xC000, 0xAA);
    sys.write(BusMaster::Cpu(0), 0xC00F, 0xBB);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xC000), 0xAA);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xC00F), 0xBB);
    assert_eq!(sys.board.read_palette(0), 0xAA);
    assert_eq!(sys.board.read_palette(15), 0xBB);
}

#[test]
fn test_cmos_ram_read_write() {
    let mut sys = JoustSystem::new();
    sys.write(BusMaster::Cpu(0), 0xCC00, 0x42);
    sys.write(BusMaster::Cpu(0), 0xCFFF, 0x99);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xCC00), 0xF2);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xCFFF), 0xF9);
}

#[test]
fn test_rom_write_protection() {
    let mut sys = JoustSystem::new();
    sys.board.load_program_rom(0, &[0xAA; 0x3000]);
    // Write to ROM area should be ignored
    sys.write(BusMaster::Cpu(0), 0xD000, 0x55);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xD000), 0xAA);
}

#[test]
fn test_unmapped_returns_ff() {
    let mut sys = JoustSystem::new();
    // Addresses in the unmapped gap
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xC010), 0xFF);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xC100), 0xFF);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xC7FF), 0xFF);
}

// =================================================================
// PIA Routing Tests
// =================================================================

#[test]
fn test_widget_pia_accessible() {
    let mut sys = JoustSystem::new();
    // Write DDRA (ctrl bit 2 = 0 by default, so offset 0 accesses DDR)
    sys.write(BusMaster::Cpu(0), 0xC804, 0xFF); // Set all as outputs
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xC804), 0xFF);
}

#[test]
fn test_rom_pia_accessible() {
    let mut sys = JoustSystem::new();
    // Write DDRA on ROM PIA
    sys.write(BusMaster::Cpu(0), 0xC80C, 0xFF);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xC80C), 0xFF);
}

#[test]
fn test_pia_addresses_independent() {
    let mut sys = JoustSystem::new();
    // Write different DDR values to each PIA
    sys.write(BusMaster::Cpu(0), 0xC804, 0xAA); // Widget PIA DDRA
    sys.write(BusMaster::Cpu(0), 0xC80C, 0x55); // ROM PIA DDRA
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xC804), 0xAA);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xC80C), 0x55);
}

// =================================================================
// Blitter Integration Tests
// =================================================================

#[test]
fn test_blitter_registers_accessible() {
    // Blitter registers ($CA00-$CA07) are write-only on real hardware.
    // Writing to $CA00 triggers the blit; reads return 0.
    // Reference: https://seanriddle.com/blitter.html
    let mut sys = JoustSystem::new();

    // Write data registers (1-7) without triggering
    sys.write(BusMaster::Cpu(0), 0xCA01, 0x42); // solid_color
    sys.write(BusMaster::Cpu(0), 0xCA06, 1 ^ 4); // width (SC1 XOR 4 → effective 1)
    sys.write(BusMaster::Cpu(0), 0xCA07, 1 ^ 4); // height (SC1 XOR 4 → effective 1)

    // Reads return 0 (write-only registers)
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xCA00), 0);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xCA01), 0);

    // Writing $CA00 triggers the blit — verify CPU gets halted
    sys.write(BusMaster::Cpu(0), 0xCA00, 0x00); // control: fast copy, triggers blit
    assert!(sys.is_halted_for(BusMaster::Cpu(0)));
}

#[test]
fn test_blitter_halts_cpu() {
    let mut sys = JoustSystem::new();
    // Set up an infinite loop at 0xD000: BRA * (2 bytes)
    sys.board.load_program_rom(0, &[0x20, 0xFE]); // BRA *
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]); // Reset vector -> 0xD000
    sys.reset();

    // Run enough cycles for the CPU to settle into the BRA loop
    for _ in 0..20 {
        sys.tick();
    }

    // Record CPU PC — it should be at the BRA instruction
    let pc_before = sys.board.get_cpu_state().pc;

    // Trigger a large blit: 8x8 solid fill so the blitter runs for 64+ DMA cycles.
    // Write data registers first, then $CA00 (control) last to trigger.
    // Width/height use XOR 4 for SC1 bug compensation.
    sys.write(BusMaster::Cpu(0), 0xCA01, 0x42); // solid_color
    sys.write(BusMaster::Cpu(0), 0xCA04, 0x00); // dst_hi
    sys.write(BusMaster::Cpu(0), 0xCA05, 0x00); // dst_lo
    sys.write(BusMaster::Cpu(0), 0xCA06, 8 ^ 4); // width = 8, XOR 4 for SC1
    sys.write(BusMaster::Cpu(0), 0xCA07, 8 ^ 4); // height = 8, XOR 4 for SC1
    sys.write(BusMaster::Cpu(0), 0xCA00, 0x10); // control: SOLID (bit 4), fast, triggers blit

    // The blitter should now be active. Tick once and verify the CPU PC
    // did NOT advance (blitter halts the CPU via is_halted_for).
    sys.tick();
    let pc_during_blit = sys.board.get_cpu_state().pc;
    assert_eq!(
        pc_before, pc_during_blit,
        "CPU PC should not advance while blitter is active"
    );

    // Run enough cycles to complete the blit (8*8 = 64 DMA cycles)
    for _ in 0..100 {
        sys.tick();
    }

    // After blit completes, CPU should resume and PC should be back in the loop
    let pc_after = sys.board.get_cpu_state().pc;
    assert!(
        (0xD000..=0xD002).contains(&pc_after),
        "CPU should resume executing after blit completes, PC=0x{:04X}",
        pc_after
    );
}

#[test]
fn test_blitter_writes_to_video_ram() {
    let mut sys = JoustSystem::new();

    // Write source data in video RAM at address 0x1000
    sys.board.write_video_ram(0x1000, 0xAB);
    sys.board.write_video_ram(0x1001, 0xCD);

    // Configure blitter for a 2-byte copy (1 row, width=1).
    // Write data registers first, then $CA00 last to trigger.
    // Width/height XOR 4 for SC1 bug compensation.
    sys.write(BusMaster::Cpu(0), 0xCA02, 0x10); // src_hi = 0x10
    sys.write(BusMaster::Cpu(0), 0xCA03, 0x00); // src_lo = 0x00
    sys.write(BusMaster::Cpu(0), 0xCA04, 0x20); // dst_hi = 0x20
    sys.write(BusMaster::Cpu(0), 0xCA05, 0x00); // dst_lo = 0x00
    sys.write(BusMaster::Cpu(0), 0xCA06, 2 ^ 4); // width = 2 bytes, XOR 4 for SC1
    sys.write(BusMaster::Cpu(0), 0xCA07, 1 ^ 4); // height = 1 row, XOR 4 for SC1
    sys.write(BusMaster::Cpu(0), 0xCA00, 0x00); // control: fast linear copy, triggers blit

    // Run enough DMA cycles to complete
    for _ in 0..10 {
        sys.tick();
    }

    // Verify destination
    assert_eq!(sys.board.read_video_ram(0x2000), 0xAB);
    assert_eq!(sys.board.read_video_ram(0x2001), 0xCD);
}

// =================================================================
// Interrupt Tests
// =================================================================

#[test]
fn test_no_irq_when_pia_disabled() {
    let mut sys = JoustSystem::new();
    // Load a simple infinite loop
    sys.board.load_program_rom(0, &[0x20, 0xFE]); // BRA *
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();

    // PIA CB1 interrupt not enabled by default, so no IRQ after VBLANK
    for _ in 0..200 {
        sys.tick();
    }
    // CPU should still be running the loop, not stuck in an IRQ handler
    let state = sys.board.get_cpu_state();
    // PC should be in the loop at 0xD000
    assert!(state.pc >= 0xD000 && state.pc <= 0xD002);
}

// =================================================================
// Reset Tests
// =================================================================

#[test]
fn test_reset_loads_vector_from_rom() {
    let mut sys = JoustSystem::new();
    sys.board.load_program_rom(0x2FFE, &[0xD1, 0x00]);
    sys.reset();
    assert_eq!(sys.board.get_cpu_state().pc, 0xD100);
}

#[test]
fn test_reset_masks_interrupts() {
    let mut sys = JoustSystem::new();
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();
    let state = sys.board.get_cpu_state();
    assert_ne!(state.cc & (CcFlag::I as u8), 0);
    assert_ne!(state.cc & (CcFlag::F as u8), 0);
}

#[test]
fn test_reset_preserves_cmos() {
    let mut sys = JoustSystem::new();
    sys.board.load_cmos(&[0x42; 1024]);
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();
    assert_eq!(sys.board.save_cmos()[0], 0x42);
}

#[test]
fn test_reset_preserves_video_ram() {
    let mut sys = JoustSystem::new();
    sys.board.write_video_ram(0x0100, 0xAB);
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();
    assert_eq!(sys.board.read_video_ram(0x0100), 0xAB);
}

// =================================================================
// CMOS Persistence Tests
// =================================================================

#[test]
fn test_cmos_load_save_roundtrip() {
    let mut sys = JoustSystem::new();
    let data = [0xAB; 1024];
    sys.board.load_cmos(&data);
    assert_eq!(sys.board.save_cmos(), &data);
}

// =================================================================
// ROM Loading Tests
// =================================================================

#[test]
fn test_load_program_rom_slice() {
    let mut sys = JoustSystem::new();
    sys.board.load_program_rom(0, &[0x12, 0x34]);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xD000), 0x12);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xD001), 0x34);
}

#[test]
fn test_load_rom_set_by_name_fallback() {
    use phosphor_machines::joust::JOUST_PROGRAM_ROM;
    use phosphor_machines::rom_loader::RomSet;

    // Use MAME filenames with test data — CRC32 won't match but
    // load_skip_checksums falls back to name-based matching.
    let rom_set = RomSet::from_slices(&[
        ("joust_rom_10b_3006-22.a7", &[0x11u8; 0x1000]),
        ("joust_rom_11b_3006-23.c7", &[0x22u8; 0x1000]),
        ("joust_rom_12b_3006-24.e7", &[0x33u8; 0x1000]),
    ]);
    let rom_data = JOUST_PROGRAM_ROM.load_skip_checksums(&rom_set).unwrap();
    let mut sys = JoustSystem::new();
    sys.board.load_program_rom(0, &rom_data);

    // Verify ROM contents at start of each region
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xD000), 0x11);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xE000), 0x22);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xF000), 0x33);
}

// =================================================================
// I/O Register Tests
// =================================================================

#[test]
fn test_rom_bank_select() {
    let mut sys = JoustSystem::new();
    sys.write(BusMaster::Cpu(0), 0xC900, 0x03);
    assert_eq!(sys.board.rom_bank(), 0x03);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xC900), 0x03);
}

// =================================================================
// Bank Switching Tests
// =================================================================

#[test]
fn test_bank_switch_disabled_reads_video_ram() {
    let mut sys = JoustSystem::new();
    sys.board.load_banked_rom(0x1000, &[0xAA]);
    sys.board.write_video_ram(0x1000, 0xBB);

    // Bank 0 (default): reads should return video RAM
    assert_eq!(sys.board.rom_bank(), 0);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x1000), 0xBB);
}

#[test]
fn test_bank_switch_enabled_reads_banked_rom() {
    let mut sys = JoustSystem::new();
    sys.board.load_banked_rom(0x1000, &[0xAA]);
    sys.board.write_video_ram(0x1000, 0xBB);

    // Enable ROM bank
    sys.write(BusMaster::Cpu(0), 0xC900, 0x01);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x1000), 0xAA);
}

#[test]
fn test_bank_switch_writes_always_to_video_ram() {
    let mut sys = JoustSystem::new();
    sys.board.load_banked_rom(0x2000, &[0xAA]);

    // Enable ROM bank and write through it
    sys.write(BusMaster::Cpu(0), 0xC900, 0x01);
    sys.write(BusMaster::Cpu(0), 0x2000, 0xCC);

    // Video RAM should have the written value
    assert_eq!(sys.board.read_video_ram(0x2000), 0xCC);

    // Disable bank: read should return the written video RAM value
    sys.write(BusMaster::Cpu(0), 0xC900, 0x00);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x2000), 0xCC);

    // Re-enable bank: ROM should still be intact (write went to video RAM, not ROM)
    sys.write(BusMaster::Cpu(0), 0xC900, 0x01);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x2000), 0xAA);
}

#[test]
fn test_upper_video_ram_unaffected_by_bank() {
    let mut sys = JoustSystem::new();
    sys.board.write_video_ram(0x9000, 0x55);
    sys.board.write_video_ram(0xBFFF, 0x66);

    // Enable ROM bank
    sys.write(BusMaster::Cpu(0), 0xC900, 0x01);

    // Upper video RAM should still return video RAM data
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x9000), 0x55);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xBFFF), 0x66);
}

#[test]
fn test_bank_switch_boundary_addresses() {
    let mut sys = JoustSystem::new();
    sys.board.load_banked_rom(0x8FFF, &[0xDD]);
    sys.board.write_video_ram(0x8FFF, 0xEE);
    sys.board.write_video_ram(0x9000, 0xFF);

    sys.write(BusMaster::Cpu(0), 0xC900, 0x01);

    // 0x8FFF should return ROM data (within banked range)
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x8FFF), 0xDD);
    // 0x9000 should return video RAM data (outside banked range)
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x9000), 0xFF);
}

#[test]
fn test_bank_switch_address_zero() {
    let mut sys = JoustSystem::new();
    sys.board.load_banked_rom(0, &[0x42]);
    sys.board.write_video_ram(0, 0x99);

    sys.write(BusMaster::Cpu(0), 0xC900, 0x01);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x0000), 0x42);

    sys.write(BusMaster::Cpu(0), 0xC900, 0x00);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x0000), 0x99);
}

#[test]
fn test_bank_switch_toggle() {
    let mut sys = JoustSystem::new();
    sys.board.load_banked_rom(0x4000, &[0x11]);
    sys.board.write_video_ram(0x4000, 0x22);

    // Off -> On -> Off -> On
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x4000), 0x22);
    sys.write(BusMaster::Cpu(0), 0xC900, 0x01);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x4000), 0x11);
    sys.write(BusMaster::Cpu(0), 0xC900, 0x00);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x4000), 0x22);
    sys.write(BusMaster::Cpu(0), 0xC900, 0x01);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x4000), 0x11);
}

#[test]
fn test_blitter_reads_banked_rom() {
    let mut sys = JoustSystem::new();
    // Put source data in video RAM
    sys.board.write_video_ram(0x1000, 0xAB);
    sys.board.write_video_ram(0x1001, 0xCD);

    // Load different data into banked ROM at same address
    sys.board.load_banked_rom(0x1000, &[0x11, 0x22]);

    // Enable ROM bank — blitter shares the CPU's bus, so it sees ROM overlay
    sys.write(BusMaster::Cpu(0), 0xC900, 0x01);

    // Configure blitter to copy from 0x1000 to 0x9000 (2 bytes, dest in upper VRAM).
    // Write data registers first, then $CA00 last to trigger.
    sys.write(BusMaster::Cpu(0), 0xCA02, 0x10); // src_hi
    sys.write(BusMaster::Cpu(0), 0xCA03, 0x00); // src_lo
    sys.write(BusMaster::Cpu(0), 0xCA04, 0x90); // dst_hi
    sys.write(BusMaster::Cpu(0), 0xCA05, 0x00); // dst_lo
    sys.write(BusMaster::Cpu(0), 0xCA06, 2 ^ 4); // width = 2 bytes, XOR 4 for SC1
    sys.write(BusMaster::Cpu(0), 0xCA07, 1 ^ 4); // height = 1 row, XOR 4 for SC1
    sys.write(BusMaster::Cpu(0), 0xCA00, 0x00); // control: fast linear copy, triggers blit

    // Run DMA cycles
    for _ in 0..10 {
        sys.tick();
    }

    // Blitter should have read from banked ROM (0x11, 0x22), not video RAM (0xAB, 0xCD)
    assert_eq!(sys.board.read_video_ram(0x9000), 0x11);
    assert_eq!(sys.board.read_video_ram(0x9001), 0x22);
}

#[test]
fn test_blitter_reads_video_ram_when_bank_disabled() {
    let mut sys = JoustSystem::new();
    // Put source data in video RAM
    sys.board.write_video_ram(0x1000, 0xAB);
    sys.board.write_video_ram(0x1001, 0xCD);

    // Load different data into banked ROM at same address
    sys.board.load_banked_rom(0x1000, &[0x11, 0x22]);

    // Bank disabled (default) — blitter reads from video RAM
    assert_eq!(sys.board.rom_bank(), 0);

    // Configure blitter to copy from 0x1000 to 0x9000 (2 bytes).
    // Write data registers first, then $CA00 last to trigger.
    sys.write(BusMaster::Cpu(0), 0xCA02, 0x10); // src_hi
    sys.write(BusMaster::Cpu(0), 0xCA03, 0x00); // src_lo
    sys.write(BusMaster::Cpu(0), 0xCA04, 0x90); // dst_hi
    sys.write(BusMaster::Cpu(0), 0xCA05, 0x00); // dst_lo
    sys.write(BusMaster::Cpu(0), 0xCA06, 2 ^ 4); // width = 2 bytes, XOR 4 for SC1
    sys.write(BusMaster::Cpu(0), 0xCA07, 1 ^ 4); // height = 1 row, XOR 4 for SC1
    sys.write(BusMaster::Cpu(0), 0xCA00, 0x00); // control: fast linear copy, triggers blit

    // Run DMA cycles
    for _ in 0..10 {
        sys.tick();
    }

    // Blitter should have read from video RAM (0xAB, 0xCD)
    assert_eq!(sys.board.read_video_ram(0x9000), 0xAB);
    assert_eq!(sys.board.read_video_ram(0x9001), 0xCD);
}

/// Verify blitter dest reads bypass ROM banking for keepmask blending.
///
/// On real hardware (and MAME), the blitter reads destination pixels directly
/// from VRAM (`m_vram[dstaddr]`), not through the address space. This means
/// ROM banking does NOT affect dest reads, only source reads.
///
/// Without this fix, a transparency blit with ROM banking active would read
/// ROM data instead of VRAM data for the keepmask blend, corrupting output.
#[test]
fn test_blitter_dest_read_bypasses_rom_banking() {
    let mut sys = JoustSystem::new();

    // Set up VRAM at dest address 0x2000 with known content
    sys.board.write_video_ram(0x2000, 0xEE);

    // Put different data in banked ROM at same address
    sys.board.load_banked_rom(0x2000, &[0x77]);

    // Source data in upper VRAM (never banked)
    sys.board.write_video_ram(0x9000, 0x0F); // upper nibble = 0 (transparent), lower = 0xF

    // Enable ROM banking
    sys.write(BusMaster::Cpu(0), 0xC900, 0x01);

    // Blit 1 byte from 0x9000 to 0x2000 with FOREGROUND_ONLY (per-nibble transparency)
    // Control = 0x08 (FOREGROUND_ONLY)
    // Upper nibble of source (0x0_) is zero → keep dest upper nibble
    // Lower nibble of source (_F) is non-zero → write source lower nibble
    sys.write(BusMaster::Cpu(0), 0xCA02, 0x90); // src_hi
    sys.write(BusMaster::Cpu(0), 0xCA03, 0x00); // src_lo
    sys.write(BusMaster::Cpu(0), 0xCA04, 0x20); // dst_hi
    sys.write(BusMaster::Cpu(0), 0xCA05, 0x00); // dst_lo
    sys.write(BusMaster::Cpu(0), 0xCA06, 1 ^ 4); // width = 1, XOR 4 for SC1
    sys.write(BusMaster::Cpu(0), 0xCA07, 1 ^ 4); // height = 1, XOR 4 for SC1
    sys.write(BusMaster::Cpu(0), 0xCA00, 0x08); // control: FOREGROUND_ONLY, triggers blit

    // Run DMA cycles
    for _ in 0..10 {
        sys.tick();
    }

    // Result should blend with VRAM (0xEE), not ROM (0x77):
    // Upper nibble: transparent → keep VRAM 0xE_
    // Lower nibble: non-zero → write source _F
    // Expected: 0xEF (VRAM upper + source lower)
    // Bug would produce: 0x7F (ROM upper + source lower)
    assert_eq!(sys.board.read_video_ram(0x2000), 0xEF);
}

#[test]
fn test_cpu_reads_banked_rom() {
    let mut sys = JoustSystem::new();
    // Place known value in banked ROM at address 0x1000
    sys.board.load_banked_rom(0x1000, &[0x42]);

    // Program at 0xD000:
    //   LDA #$01       -- enable ROM bank
    //   STA $C900
    //   LDB $1000      -- read from banked ROM
    //   STB $9100      -- store to upper video RAM (always accessible)
    //   BRA *
    sys.board.load_program_rom(
        0,
        &[
            0x86, 0x01, // LDA #$01
            0xB7, 0xC9, 0x00, // STA $C900
            0xF6, 0x10, 0x00, // LDB $1000
            0xF7, 0x91, 0x00, // STB $9100
            0x20, 0xFE, // BRA *
        ],
    );
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();

    for _ in 0..60 {
        sys.tick();
    }

    assert_eq!(sys.board.read_video_ram(0x9100), 0x42);
    assert_eq!(sys.board.rom_bank(), 0x01);
}

#[test]
fn test_reset_clears_bank_preserves_rom() {
    let mut sys = JoustSystem::new();
    sys.board.load_banked_rom(0x0000, &[0xAA]);
    sys.write(BusMaster::Cpu(0), 0xC900, 0x01);
    assert_eq!(sys.board.rom_bank(), 0x01);

    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();

    // Bank register should be 0 after reset
    assert_eq!(sys.board.rom_bank(), 0x00);

    // Banked ROM data should be preserved
    sys.write(BusMaster::Cpu(0), 0xC900, 0x01);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x0000), 0xAA);
}

#[test]
fn test_load_banked_rom_all_chips() {
    let mut sys = JoustSystem::new();
    // Load each 4KB chip with a distinct byte
    for i in 0..9u8 {
        let offset = i as usize * 0x1000;
        sys.board.load_banked_rom(offset, &[i + 1; 0x1000]);
    }

    sys.write(BusMaster::Cpu(0), 0xC900, 0x01);

    // Verify first byte of each 4KB chip
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x0000), 0x01); // chip 1
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x1000), 0x02); // chip 2
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x2000), 0x03); // chip 3
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x3000), 0x04); // chip 4
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x4000), 0x05); // chip 5
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x5000), 0x06); // chip 6
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x6000), 0x07); // chip 7
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x7000), 0x08); // chip 8
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x8000), 0x09); // chip 9
}

#[test]
fn test_video_counter_read() {
    let mut sys = JoustSystem::new();
    sys.board.load_program_rom(0, &[0x20, 0xFE]); // BRA *
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();

    // At cycle 0, scanline = 0, video counter = 0 & 0xFC = 0
    let val = sys.read(BusMaster::Cpu(0), 0xCB00);
    assert_eq!(val, 0x00);

    // Advance past first scanline boundary (64 cycles)
    for _ in 0..64 {
        sys.tick();
    }
    // At cycle 64, scanline = 1, video counter = 1 & 0xFC = 0
    let val = sys.read(BusMaster::Cpu(0), 0xCB00);
    assert_eq!(val, 0x00);

    // Advance to scanline 4 (cycle 256)
    for _ in 0..192 {
        sys.tick();
    }
    let val = sys.read(BusMaster::Cpu(0), 0xCB42); // Any address in 0xCB00-0xCBFF works
    assert_eq!(val, 0x04);
}

#[test]
fn test_watchdog_reset_on_write() {
    let mut sys = JoustSystem::new();
    // Advance watchdog counter
    for _ in 0..100 {
        sys.tick();
    }
    // Writing 0x39 to 0xCBFF resets watchdog (MAME: williams_m.cpp:251)
    sys.write(BusMaster::Cpu(0), 0xCBFF, 0x39);
    assert_eq!(sys.board.watchdog_counter, 0);
}

#[test]
fn test_watchdog_ignores_non_0x39() {
    let mut sys = JoustSystem::new();
    for _ in 0..100 {
        sys.tick();
    }
    let before = sys.board.watchdog_counter;
    // Writing any value other than 0x39 does NOT reset watchdog
    sys.write(BusMaster::Cpu(0), 0xCBFF, 0x00);
    assert_eq!(sys.board.watchdog_counter, before);
    sys.write(BusMaster::Cpu(0), 0xCBFF, 0xFF);
    assert_eq!(sys.board.watchdog_counter, before);
}

// =================================================================
// Integration: Execute a Small Program
// =================================================================

#[test]
fn test_execute_simple_program() {
    let mut sys = JoustSystem::new();
    sys.board.load_program_rom(
        0,
        &[
            0x86, 0x42, // LDA #$42
            0xB7, 0x01, 0x00, // STA $0100
            0xF6, 0x01, 0x00, // LDB $0100
            0xF7, 0x01, 0x01, // STB $0101
            0x20, 0xFE, // BRA *
        ],
    );
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();

    for _ in 0..50 {
        sys.tick();
    }

    assert_eq!(sys.board.read_video_ram(0x0100), 0x42);
    assert_eq!(sys.board.read_video_ram(0x0101), 0x42);
    let state = sys.board.get_cpu_state();
    assert_eq!(state.a, 0x42);
    assert_eq!(state.b, 0x42);
}

#[test]
fn test_execute_palette_write_program() {
    let mut sys = JoustSystem::new();
    // Program that writes 0xE3 to palette entry 0 (address 0xC000)
    sys.board.load_program_rom(
        0,
        &[
            0x86, 0xE3, // LDA #$E3
            0xB7, 0xC0, 0x00, // STA $C000
            0x20, 0xFE, // BRA *
        ],
    );
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();

    for _ in 0..30 {
        sys.tick();
    }

    assert_eq!(sys.board.read_palette(0), 0xE3);
}

#[test]
fn test_execute_cmos_write_program() {
    let mut sys = JoustSystem::new();
    // Program that writes 0x99 to CMOS at 0xCC00
    sys.board.load_program_rom(
        0,
        &[
            0x86, 0x99, // LDA #$99
            0xB7, 0xCC, 0x00, // STA $CC00
            0x20, 0xFE, // BRA *
        ],
    );
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();

    for _ in 0..30 {
        sys.tick();
    }

    assert_eq!(sys.board.save_cmos()[0], 0xF9);
}

#[test]
fn test_execute_rom_bank_program() {
    let mut sys = JoustSystem::new();
    // Program that writes 0x03 to ROM bank select at 0xC900
    sys.board.load_program_rom(
        0,
        &[
            0x86, 0x03, // LDA #$03
            0xB7, 0xC9, 0x00, // STA $C900
            0x20, 0xFE, // BRA *
        ],
    );
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();

    for _ in 0..30 {
        sys.tick();
    }

    assert_eq!(sys.board.rom_bank(), 0x03);
}

#[test]
fn test_clock_advances() {
    let mut sys = JoustSystem::new();
    sys.board.load_program_rom(0, &[0x20, 0xFE]); // BRA *
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();

    assert_eq!(sys.board.clock(), 0);
    for _ in 0..100 {
        sys.tick();
    }
    assert_eq!(sys.board.clock(), 100);
}

#[test]
fn test_default_impl() {
    let sys = JoustSystem::default();
    assert_eq!(sys.display_size(), (292, 240));
    assert_eq!(sys.board.clock(), 0);
}

// =================================================================
// Sound Board Tests
// =================================================================

#[test]
fn test_sound_cpu_reset_loads_vector() {
    let mut sys = JoustSystem::new();
    // Reset vector at 0xFFFE-0xFFFF in sound ROM (offset 0x0FFE-0x0FFF)
    sys.board.load_sound_rom(0x0FFE, &[0xF0, 0x80]); // Reset vector = 0xF080
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();
    assert_eq!(sys.board.get_sound_cpu_state().pc, 0xF080);
}

#[test]
fn test_sound_cpu_reset_masks_interrupts() {
    let mut sys = JoustSystem::new();
    sys.board.load_sound_rom(0x0FFE, &[0xF0, 0x00]);
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();
    let state = sys.board.get_sound_cpu_state();
    assert_ne!(state.cc & (CcFlag::I as u8), 0);
    assert_ne!(state.cc & (CcFlag::F as u8), 0);
}

#[test]
fn test_sound_cpu_executes_independently() {
    let mut sys = JoustSystem::new();
    // Sound ROM at 0xF000: LDA #$42, STA $0010, BRA *
    sys.board.load_sound_rom(
        0,
        &[
            0x86, 0x42, // LDA #$42
            0xB7, 0x00, 0x10, // STA $0010
            0x20, 0xFE, // BRA *
        ],
    );
    sys.board.load_sound_rom(0x0FFE, &[0xF0, 0x00]); // Reset vector -> 0xF000
    // Main CPU: BRA *
    sys.board.load_program_rom(0, &[0x20, 0xFE]);
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();

    for _ in 0..50 {
        sys.tick();
    }

    // Sound CPU should have written 0x42 to sound RAM at 0x0010
    assert_eq!(sys.read(BusMaster::Cpu(1), 0x0010), 0x42);
    assert_eq!(sys.board.get_sound_cpu_state().a, 0x42);
}

#[test]
fn test_sound_ram_isolated_from_main_cpu() {
    let mut sys = JoustSystem::new();
    // Write to address 0x0010 via main CPU (hits video RAM)
    sys.write(BusMaster::Cpu(0), 0x0010, 0xAA);
    // Write to address 0x0010 via sound CPU (hits sound RAM)
    sys.write(BusMaster::Cpu(1), 0x0010, 0xBB);

    // They should be in separate memory spaces
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x0010), 0xAA);
    assert_eq!(sys.read(BusMaster::Cpu(1), 0x0010), 0xBB);
}

#[test]
fn test_sound_rom_readable() {
    let mut sys = JoustSystem::new();
    sys.board.load_sound_rom(0, &[0xDE, 0xAD]);
    assert_eq!(sys.read(BusMaster::Cpu(1), 0xF000), 0xDE);
    assert_eq!(sys.read(BusMaster::Cpu(1), 0xF001), 0xAD);
}

#[test]
fn test_sound_rom_mirrored_at_0xb000() {
    let mut sys = JoustSystem::new();
    sys.board.load_sound_rom(0, &[0xDE, 0xAD]);
    // 4KB ROM mirrored via incomplete address decoding at 0xB000, 0xC000, 0xD000, 0xE000
    assert_eq!(sys.read(BusMaster::Cpu(1), 0xB000), 0xDE);
    assert_eq!(sys.read(BusMaster::Cpu(1), 0xB001), 0xAD);
    assert_eq!(sys.read(BusMaster::Cpu(1), 0xC000), 0xDE);
    assert_eq!(sys.read(BusMaster::Cpu(1), 0xD000), 0xDE);
    assert_eq!(sys.read(BusMaster::Cpu(1), 0xE000), 0xDE);
}

#[test]
fn test_sound_rom_write_protection() {
    let mut sys = JoustSystem::new();
    sys.board.load_sound_rom(0, &[0x42]);
    sys.write(BusMaster::Cpu(1), 0xF000, 0xFF);
    assert_eq!(sys.read(BusMaster::Cpu(1), 0xF000), 0x42);
}

#[test]
fn test_sound_unmapped_returns_ff() {
    let mut sys = JoustSystem::new();
    assert_eq!(sys.read(BusMaster::Cpu(1), 0x0100), 0xFF);
    assert_eq!(sys.read(BusMaster::Cpu(1), 0x0500), 0xFF);
    assert_eq!(sys.read(BusMaster::Cpu(1), 0x1000), 0xFF);
    assert_eq!(sys.read(BusMaster::Cpu(1), 0xAFFF), 0xFF);
}

#[test]
fn test_sound_pia_accessible() {
    let mut sys = JoustSystem::new();
    // Write DDRA on sound PIA (ctrl bit 2 = 0 by default)
    sys.write(BusMaster::Cpu(1), 0x0400, 0xFF);
    assert_eq!(sys.read(BusMaster::Cpu(1), 0x0400), 0xFF);
}

#[test]
fn test_sound_command_propagation() {
    let mut sys = JoustSystem::new();

    // Configure ROM PIA Port B as all output
    sys.write(BusMaster::Cpu(0), 0xC80E, 0xFF); // ROM PIA DDRB
    sys.write(BusMaster::Cpu(0), 0xC80F, 0x04); // CRB: bit 2 = 1 (data select)

    // Configure sound PIA Port B as all input (default) with CRB data select
    sys.write(BusMaster::Cpu(1), 0x0403, 0x04); // Sound PIA CRB: data select

    // Write a sound command to ROM PIA Port B
    sys.write(BusMaster::Cpu(0), 0xC80E, 0x42);

    // Tick once to propagate
    sys.tick();

    // Sound PIA Port B should have the command with high bits pulled up (| 0xC0)
    let command = sys.read(BusMaster::Cpu(1), 0x0402);
    assert_eq!(command, 0xC2);
}

#[test]
fn test_sound_cpu_not_halted_during_blit() {
    let mut sys = JoustSystem::new();

    // Sound ROM: LDA #$42, STA $0050, BRA *
    sys.board.load_sound_rom(
        0,
        &[
            0x86, 0x42, // LDA #$42
            0xB7, 0x00, 0x50, // STA $0050
            0x20, 0xFE, // BRA *
        ],
    );
    sys.board.load_sound_rom(0x0FFE, &[0xF0, 0x00]);
    // Main CPU: BRA *
    sys.board.load_program_rom(0, &[0x20, 0xFE]);
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();

    // Trigger a large blit to halt main CPU
    sys.write(BusMaster::Cpu(0), 0xCA01, 0x00);
    sys.write(BusMaster::Cpu(0), 0xCA04, 0x90);
    sys.write(BusMaster::Cpu(0), 0xCA05, 0x00);
    sys.write(BusMaster::Cpu(0), 0xCA06, 8 ^ 4);
    sys.write(BusMaster::Cpu(0), 0xCA07, 8 ^ 4);
    sys.write(BusMaster::Cpu(0), 0xCA00, 0x10); // SOLID blit

    // Tick during blit — sound CPU should still execute
    for _ in 0..50 {
        sys.tick();
    }

    assert_eq!(sys.read(BusMaster::Cpu(1), 0x0050), 0x42);
}

// =================================================================
// Scanline / Video Timing Tests
// =================================================================

#[test]
fn test_scanline_va11_signal() {
    let mut sys = JoustSystem::new();
    sys.board.load_program_rom(0, &[0x20, 0xFE]); // BRA *
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();

    // Enable ROM PIA CB1 interrupt: set CRB bit 0 = 1 (interrupt enable)
    // and bit 1 = 1 (rising edge trigger), bit 2 = 1 (data select)
    sys.write(BusMaster::Cpu(0), 0xC80E, 0x00); // DDRB = 0
    sys.write(BusMaster::Cpu(0), 0xC80F, 0x07); // CRB: enable CB1 IRQ, rising edge, data select

    // Advance past scanline 32 boundary: clock must reach 32*64 = 2048.
    // tick() checks clock at entry, so the (N+1)th call processes clock=N.
    for _ in 0..2049 {
        sys.tick();
    }
    // At scanline 32: VA11 = (32 & 0x20) != 0 = true → CB1 goes high (rising edge)
    // ROM PIA irq_b1 should be set → CRB bit 7 high
    let crb = sys.read(BusMaster::Cpu(0), 0xC80F);
    assert_ne!(
        crb & 0x80,
        0,
        "CB1 IRQ flag should be set after VA11 rising edge at scanline 32"
    );
}

#[test]
fn test_scanline_count240_signal() {
    let mut sys = JoustSystem::new();
    sys.board.load_program_rom(0, &[0x20, 0xFE]); // BRA *
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();

    // Enable ROM PIA CA1 interrupt: set CRA bit 0 = 1, bit 1 = 1 (rising edge), bit 2 = 1
    sys.write(BusMaster::Cpu(0), 0xC80C, 0x00); // DDRA = 0
    sys.write(BusMaster::Cpu(0), 0xC80D, 0x07); // CRA: enable CA1 IRQ, rising edge, data select

    // Advance past scanline 240 boundary: clock must reach 240*64 = 15360.
    for _ in 0..15361 {
        sys.tick();
    }
    // count240 should have asserted CA1 at scanline 240 → CRA bit 7 high
    let cra = sys.read(BusMaster::Cpu(0), 0xC80D);
    assert_ne!(
        cra & 0x80,
        0,
        "CA1 IRQ flag should be set at scanline 240 (count240)"
    );
}

#[test]
fn test_irq_routing_rom_pia_only() {
    let mut sys = JoustSystem::new();
    // With no PIA interrupts enabled, main CPU should have no IRQ
    let state = sys.check_interrupts(BusMaster::Cpu(0));
    assert!(!state.irq);
    assert!(
        !state.firq,
        "FIRQ should never be asserted on Williams gen-1"
    );
}
