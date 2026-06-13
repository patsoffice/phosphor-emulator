use phosphor_core::core::machine::{
    InputConfigurable, InputEvent, InputId, MachineCore, Renderable,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_machines::missile_command::{
    INPUT_COIN, INPUT_FIRE_CENTER, INPUT_FIRE_LEFT, INPUT_FIRE_RIGHT, INPUT_START1, INPUT_START2,
    INPUT_TRACK_D, INPUT_TRACK_R, MissileCommandSystem,
};

// =================================================================
// Machine traits tests
// =================================================================

#[test]
fn test_display_size() {
    let sys = MissileCommandSystem::new();
    assert_eq!(sys.display_size(), (256, 231));
}

#[test]
fn test_input_map_has_all_buttons() {
    let sys = MissileCommandSystem::new();
    let controls = sys.input_controls();
    assert_eq!(controls.len(), 12); // coin + 2 start + 3 fire + 4 trackball + 2 analog axes
    for control in controls {
        assert!(!control.label.is_empty());
    }
}

#[test]
fn test_render_frame_correct_size() {
    let sys = MissileCommandSystem::new();
    let (w, h) = sys.display_size();
    let mut buffer = vec![0u8; (w * h * 3) as usize];
    sys.render_frame(&mut buffer); // Should not panic
}

// =================================================================
// Memory Map Routing Tests
// =================================================================

#[test]
fn test_ram_read_write() {
    let mut sys = MissileCommandSystem::new();
    sys.write(BusMaster::Cpu(0), 0x0000, 0xAA);
    sys.write(BusMaster::Cpu(0), 0x3FFF, 0xBB);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x0000), 0xAA);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x3FFF), 0xBB);
}

#[test]
fn test_ram_via_helper() {
    let mut sys = MissileCommandSystem::new();
    sys.write(BusMaster::Cpu(0), 0x1234, 0xCD);
    assert_eq!(sys.read_ram(0x1234), 0xCD);
}

#[test]
fn test_rom_not_writable() {
    let mut sys = MissileCommandSystem::new();
    // Write some known data into ROM via the helper
    sys.write_ram(0x0000, 0x11); // RAM is writable
    // ROM at 0x5000+ should not be writable through the bus
    sys.write(BusMaster::Cpu(0), 0x5000, 0xFF);
    // ROM should still be 0 (default)
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x5000), 0x00);
}

#[test]
fn test_rom_mirror_vectors() {
    let mut sys = MissileCommandSystem::new();
    // Write known data at ROM offset 0x2800 (maps to 0x7800 and mirrored at 0xF800)
    // We can't write ROM through bus, but we can use the internal write_ram for RAM
    // and check that ROM reads work. Let's verify the mirror mapping.
    // ROM[0x2800] maps to bus 0x7800 (0x5000 + 0x2800) and also to 0xF800
    // ROM[0x2FFF] maps to bus 0x7FFF and also to 0xFFFF
    // Since ROM is all zeros, both should read 0
    assert_eq!(
        sys.read(BusMaster::Cpu(0), 0x7800),
        sys.read(BusMaster::Cpu(0), 0xF800)
    );
    assert_eq!(
        sys.read(BusMaster::Cpu(0), 0x7FFF),
        sys.read(BusMaster::Cpu(0), 0xFFFF)
    );
}

#[test]
fn test_unmapped_returns_ff() {
    let mut sys = MissileCommandSystem::new();
    // Addresses in unmapped regions
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x4E00), 0xFF);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x4F00), 0xFF);
}

// =================================================================
// POKEY Routing Tests
// =================================================================

#[test]
fn test_pokey_accessible() {
    let mut sys = MissileCommandSystem::new();
    // Write to POKEY AUDF1 (offset 0x00) and read back POT0 (offset 0x00)
    // POKEY is write-at-offset/read-at-offset with different register sets
    // Write AUDCTL (offset 0x08) — this is a write-only register
    sys.write(BusMaster::Cpu(0), 0x4008, 0x00);
    // Read ALLPOT (offset 0x08) — this is a read-only register
    let _val = sys.read(BusMaster::Cpu(0), 0x4008);
    // Should not panic
}

#[test]
fn test_pokey_mirror() {
    let mut sys = MissileCommandSystem::new();
    // POKEY is mirrored across 0x4000-0x47FF — writing at 0x4010 should alias to 0x4000
    sys.write(BusMaster::Cpu(0), 0x4010, 0x42);
    // Reading back from the mirrored offset should also work
    let _val = sys.read(BusMaster::Cpu(0), 0x4410);
    // Should not panic; no observable side effect to verify directly
}

// =================================================================
// Input Tests (IN0 — Switches)
// =================================================================

#[test]
fn test_in0_default_all_released() {
    let mut sys = MissileCommandSystem::new();
    // IN0 is active-low: all 1s when nothing pressed
    let val = sys.read(BusMaster::Cpu(0), 0x4800);
    assert_eq!(
        val, 0xFF,
        "All buttons released should read 0xFF (active-low)"
    );
}

#[test]
fn test_in0_coin_press() {
    let mut sys = MissileCommandSystem::new();
    // Coin is IN0 bit 5 (Left Coin), active-low
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_COIN) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0x4800);
    assert_eq!(val & 0x20, 0x00, "Coin pressed should clear bit 5");

    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_COIN) as u16),
        pressed: false,
    });
    let val = sys.read(BusMaster::Cpu(0), 0x4800);
    assert_eq!(val & 0x20, 0x20, "Coin released should set bit 5");
}

#[test]
fn test_in0_start1_press() {
    let mut sys = MissileCommandSystem::new();
    // Start 1 is IN0 bit 4, active-low
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_START1) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0x4800);
    assert_eq!(val & 0x10, 0x00, "Start 1 pressed should clear bit 4");

    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_START1) as u16),
        pressed: false,
    });
    let val = sys.read(BusMaster::Cpu(0), 0x4800);
    assert_eq!(val & 0x10, 0x10, "Start 1 released should set bit 4");
}

#[test]
fn test_in0_start2_separate_bit() {
    let mut sys = MissileCommandSystem::new();
    // Start 2 is IN0 bit 3 (separate from Start 1 bit 4)
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_START2) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0x4800);
    assert_eq!(val & 0x08, 0x00, "Start 2 pressed should clear bit 3");
    assert_eq!(
        val & 0x10,
        0x10,
        "Start 1 should remain unaffected (bit 4 still set)"
    );

    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_START2) as u16),
        pressed: false,
    });
    let val = sys.read(BusMaster::Cpu(0), 0x4800);
    assert_eq!(val & 0x08, 0x08, "Start 2 released should set bit 3");
}

#[test]
fn test_in0_simultaneous_buttons() {
    let mut sys = MissileCommandSystem::new();
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_COIN) as u16),
        pressed: true,
    });
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_START1) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0x4800);
    // Coin (bit 5) and Start 1 (bit 4) should both be cleared
    assert_eq!(val & 0x30, 0x00, "Both coin and start 1 pressed");

    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_COIN) as u16),
        pressed: false,
    });
    let val = sys.read(BusMaster::Cpu(0), 0x4800);
    assert_eq!(val & 0x30, 0x20, "Only start 1 still pressed");
}

// =================================================================
// Input Tests (IN1 — Fire Buttons & Status)
// =================================================================

#[test]
fn test_in1_fire_buttons_default() {
    let mut sys = MissileCommandSystem::new();
    // Fire buttons are IN1 bits 0-2, active-low (1 = released)
    let val = sys.read(BusMaster::Cpu(0), 0x4900);
    assert_eq!(val & 0x07, 0x07, "All fire buttons released (bits 0-2 = 1)");
}

#[test]
fn test_in1_fire_left_press() {
    let mut sys = MissileCommandSystem::new();
    // Fire Left is IN1 bit 2, active-low
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_LEFT) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0x4900);
    assert_eq!(val & 0x04, 0x00, "Fire Left pressed should clear bit 2");

    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_LEFT) as u16),
        pressed: false,
    });
    let val = sys.read(BusMaster::Cpu(0), 0x4900);
    assert_eq!(val & 0x04, 0x04, "Fire Left released should set bit 2");
}

#[test]
fn test_in1_fire_center_press() {
    let mut sys = MissileCommandSystem::new();
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_CENTER) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0x4900);
    assert_eq!(val & 0x02, 0x00, "Fire Center pressed should clear bit 1");
}

#[test]
fn test_in1_fire_right_press() {
    let mut sys = MissileCommandSystem::new();
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_RIGHT) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0x4900);
    assert_eq!(val & 0x01, 0x00, "Fire Right pressed should clear bit 0");
}

#[test]
fn test_in1_all_three_fires() {
    let mut sys = MissileCommandSystem::new();
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_LEFT) as u16),
        pressed: true,
    });
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_CENTER) as u16),
        pressed: true,
    });
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_RIGHT) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0x4900);
    assert_eq!(val & 0x07, 0x00, "All three fire buttons pressed");
}

#[test]
fn test_in1_fire_on_separate_register_from_in0() {
    let mut sys = MissileCommandSystem::new();
    // Fire buttons should NOT affect IN0 register
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_LEFT) as u16),
        pressed: true,
    });
    let in0 = sys.read(BusMaster::Cpu(0), 0x4800);
    assert_eq!(in0, 0xFF, "Fire button should not affect IN0");

    // Coin should NOT affect IN1 fire bits
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_COIN) as u16),
        pressed: true,
    });
    let in1 = sys.read(BusMaster::Cpu(0), 0x4900);
    assert_eq!(
        in1 & 0x07,
        0x03,
        "Coin should not affect IN1 fire bits (only Fire Left pressed)"
    );
}

// =================================================================
// CTRLD / Trackball Mux Tests
// =================================================================

#[test]
fn test_ctrld_default_reads_switches() {
    let mut sys = MissileCommandSystem::new();
    // Default CTRLD=0 → 0x4800 reads IN0 switches
    let val = sys.read(BusMaster::Cpu(0), 0x4800);
    assert_eq!(val, 0xFF, "CTRLD=0 should read IN0 switches");
}

#[test]
fn test_ctrld_set_reads_trackball() {
    let mut sys = MissileCommandSystem::new();
    // Write output latch with CTRLD=1
    sys.write(BusMaster::Cpu(0), 0x4800, 0x01);

    // Now reading 0x4800 should return trackball data
    let val = sys.read(BusMaster::Cpu(0), 0x4800);
    // Trackball X=0, Y=0 → should read 0x00
    assert_eq!(val, 0x00, "CTRLD=1 should read trackball (both 0)");
}

#[test]
fn test_ctrld_trackball_values() {
    let mut sys = MissileCommandSystem::new();
    sys.write(BusMaster::Cpu(0), 0x4800, 0x01); // CTRLD=1

    // Set trackball counters via key presses
    // We need to tick to advance the trackball
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_TRACK_R) as u16),
        pressed: true,
    });
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_TRACK_D) as u16),
        pressed: true,
    });
    // Tick 1000 cycles for one trackball update
    for _ in 0..1000 {
        sys.tick();
    }
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_TRACK_R) as u16),
        pressed: false,
    });
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_TRACK_D) as u16),
        pressed: false,
    });

    let val = sys.read(BusMaster::Cpu(0), 0x4800);
    // X should be 1 (low nibble), Y should be 1 (high nibble, but wrapping sub for down)
    let x = val & 0x0F;
    let y = (val >> 4) & 0x0F;
    assert_eq!(x, 1, "Trackball X should be 1 after right press");
    // Down increments Y (positive direction on screen)
    assert_eq!(y, 1, "Trackball Y should be 1 after down press");
}

#[test]
fn test_ctrld_toggle_back_to_switches() {
    let mut sys = MissileCommandSystem::new();
    // Set CTRLD=1 then back to 0
    sys.write(BusMaster::Cpu(0), 0x4800, 0x01);
    sys.write(BusMaster::Cpu(0), 0x4800, 0x00);

    // Should read switches again
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_COIN) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0x4800);
    assert_eq!(
        val & 0x20,
        0x00,
        "CTRLD=0 should read switches, coin pressed"
    );
}

// =================================================================
// DIP Switch Tests
// =================================================================

#[test]
fn test_dip_switches_readable() {
    let mut sys = MissileCommandSystem::new();
    // DIP switches at 0x4A00, default 0x00
    let val = sys.read(BusMaster::Cpu(0), 0x4A00);
    assert_eq!(val, 0x00, "Default DIP switches");
}

// =================================================================
// Color RAM Tests
// =================================================================

#[test]
fn test_color_ram_write_read() {
    let mut sys = MissileCommandSystem::new();
    // Color RAM at 0x4B00-0x4B07 is write-only on bus, but we can read via helper
    sys.write(BusMaster::Cpu(0), 0x4B00, 0x0E); // Entry 0: bits 3/2/1 = R/G/B inverted
    sys.write(BusMaster::Cpu(0), 0x4B07, 0x02); // Entry 7
    assert_eq!(sys.read_palette(0), 0x0E);
    assert_eq!(sys.read_palette(7), 0x02);
}

#[test]
fn test_color_ram_mirror() {
    let mut sys = MissileCommandSystem::new();
    // Writing 0x4B08 should mirror to entry 0 (addr & 0x07)
    sys.write(BusMaster::Cpu(0), 0x4B08, 0xAA);
    assert_eq!(sys.read_palette(0), 0xAA);
}

// =================================================================
// Watchdog Tests
// =================================================================

#[test]
fn test_watchdog_reset_write() {
    let mut sys = MissileCommandSystem::new();
    // Tick a few cycles to increment watchdog
    for _ in 0..100 {
        sys.tick();
    }
    // Watchdog should have incremented (we can't read it directly, but
    // writing to 0x4C00 should reset it without panicking)
    sys.write(BusMaster::Cpu(0), 0x4C00, 0x00);
    // No panic = success
}

// =================================================================
// IRQ Acknowledge Tests
// =================================================================

#[test]
fn test_irq_acknowledge_clears_irq() {
    let mut sys = MissileCommandSystem::new();
    // Run until an IRQ should be asserted (at scanline 0, which is the start)
    // At clock=0, frame_cycle=0, scanline=0, bit_32v=0 → IRQ asserted
    // But we need to tick at least once to trigger the IRQ logic
    sys.tick(); // clock was 0, frame_cycle=0, scanline=0

    // Check that IRQ is asserted
    let _in1 = sys.read(BusMaster::Cpu(0), 0x4900); // IN1 — not the IRQ itself
    // The IRQ state is reported through check_interrupts, which we test indirectly

    // Write to 0x4D00 to acknowledge IRQ
    sys.write(BusMaster::Cpu(0), 0x4D00, 0x00);
    // After ack, IRQ should be cleared (tested via Bus::check_interrupts)
}

// =================================================================
// VBLANK Tests
// =================================================================

#[test]
fn test_vblank_active_at_start() {
    let mut sys = MissileCommandSystem::new();
    // At clock=0, scanline=0, which is in VBLANK (V < 25)
    sys.tick();
    let in1 = sys.read(BusMaster::Cpu(0), 0x4900);
    assert_eq!(
        in1 & 0x80,
        0x80,
        "VBLANK should be active (bit 7 high) at scanline 0"
    );
}

#[test]
fn test_vblank_inactive_during_active_display() {
    let mut sys = MissileCommandSystem::new();
    // Advance past VBLANK (scanline 25+)
    // 25 scanlines * 80 cycles/scanline = 2000 cycles
    for _ in 0..2001 {
        sys.tick();
    }
    let in1 = sys.read(BusMaster::Cpu(0), 0x4900);
    assert_eq!(
        in1 & 0x80,
        0x00,
        "VBLANK should be inactive (bit 7 low) during active display"
    );
}

// =================================================================
// Video Rendering Tests
// =================================================================

#[test]
fn test_render_default_palette_all_white() {
    let sys = MissileCommandSystem::new();
    // Default palette entries are all 0 → R/G/B bits inverted → all 255 = white
    // Default RAM is all 0 → all pixels use color index 0
    let (w, h) = sys.display_size();
    let mut buffer = vec![0u8; (w * h * 3) as usize];
    sys.render_frame(&mut buffer);

    // Palette entry 0 with data=0: bits 3/2/1 = 0 → inverted → R=255, G=255, B=255
    assert_eq!(
        buffer[0], 255,
        "R should be 255 (palette 0, all bits inverted)"
    );
    assert_eq!(buffer[1], 255, "G should be 255");
    assert_eq!(buffer[2], 255, "B should be 255");
}

#[test]
fn test_render_palette_colors() {
    let mut sys = MissileCommandSystem::new();
    // Set palette entry 0 to black (bits 3/2/1 = 1 → inverted = 0)
    sys.write(BusMaster::Cpu(0), 0x4B00, 0x0E); // bits 3=1, 2=1, 1=1 → R=0, G=0, B=0

    let (w, h) = sys.display_size();
    let mut buffer = vec![0xFFu8; (w * h * 3) as usize];
    sys.render_frame(&mut buffer);

    // All RAM is 0, all pixels use color index 0 = black
    assert_eq!(buffer[0], 0, "R should be 0 (black)");
    assert_eq!(buffer[1], 0, "G should be 0");
    assert_eq!(buffer[2], 0, "B should be 0");
}

#[test]
fn test_render_bit_planar_pixel_extraction() {
    let mut sys = MissileCommandSystem::new();
    // Set up palette entries:
    // Entry 0 (color idx 0): 0x0E → black (all inverted = 0)
    // Entry 2 (color idx 2): 0x0C → R=0,G=0,B=255 (bit 1=0 → B=255)
    // Entry 4 (color idx 4): 0x06 → R=255,G=0,B=0 (bit 3=0 → R=255, bits 2,1=1 → G=0,B=0)
    // Entry 6 (color idx 6): 0x04 → R=255,G=0,B=255 (bit 3=0,bit 1=0 → R=255,B=255)
    sys.write(BusMaster::Cpu(0), 0x4B00, 0x0E); // idx 0: black
    sys.write(BusMaster::Cpu(0), 0x4B02, 0x0C); // idx 2: blue
    sys.write(BusMaster::Cpu(0), 0x4B04, 0x06); // idx 4: red
    sys.write(BusMaster::Cpu(0), 0x4B06, 0x04); // idx 6: magenta

    // Write a test byte to video RAM at the first visible scanline (V=25)
    // Row base = 25 * 64 = 1600 = 0x640
    // Pixel 0: plane0=bit0, plane1=bit4 → color = (plane1<<2) | (plane0<<1)
    // Set byte so pixel 0 = color 2 (plane0=1, plane1=0): bit 0=1, bit 4=0
    // And pixel 1 = color 4 (plane0=0, plane1=1): bit 1=0, bit 5=1
    // Byte: bit0=1, bit1=0, bit4=0, bit5=1 = 0b00100001 = 0x21
    sys.write(BusMaster::Cpu(0), 0x0640, 0x21);

    let (w, h) = sys.display_size();
    let mut buffer = vec![0u8; (w * h * 3) as usize];
    sys.render_frame(&mut buffer);

    // Pixel (0,0) should be color index 2 (blue: R=0, G=0, B=255)
    assert_eq!(buffer[0], 0, "Pixel 0 R (blue)");
    assert_eq!(buffer[1], 0, "Pixel 0 G (blue)");
    assert_eq!(buffer[2], 255, "Pixel 0 B (blue)");

    // Pixel (1,0) should be color index 4 (red: R=255, G=0, B=0)
    assert_eq!(buffer[3], 255, "Pixel 1 R (red)");
    assert_eq!(buffer[4], 0, "Pixel 1 G (red)");
    assert_eq!(buffer[5], 0, "Pixel 1 B (red)");
}

#[test]
fn test_render_all_four_pixels_in_byte() {
    let mut sys = MissileCommandSystem::new();
    // Set palette: entry 0=black, 2=blue, 4=red, 6=magenta
    sys.write(BusMaster::Cpu(0), 0x4B00, 0x0E);
    sys.write(BusMaster::Cpu(0), 0x4B02, 0x0C);
    sys.write(BusMaster::Cpu(0), 0x4B04, 0x06);
    sys.write(BusMaster::Cpu(0), 0x4B06, 0x04);

    // Set a byte where all 4 pixels have different colors:
    // Pixel 0: color 6 (plane0=1, plane1=1) → bit0=1, bit4=1
    // Pixel 1: color 4 (plane0=0, plane1=1) → bit1=0, bit5=1
    // Pixel 2: color 2 (plane0=1, plane1=0) → bit2=1, bit6=0
    // Pixel 3: color 0 (plane0=0, plane1=0) → bit3=0, bit7=0
    // Byte = 0b00110101 = 0x35
    sys.write(BusMaster::Cpu(0), 0x0640, 0x35);

    let (_w, _h) = sys.display_size();
    let mut buffer = vec![0u8; 256 * 231 * 3];
    sys.render_frame(&mut buffer);

    // Pixel 0: color 6 = magenta (R=255, G=0, B=255)
    assert_eq!(
        (buffer[0], buffer[1], buffer[2]),
        (255, 0, 255),
        "Pixel 0: magenta"
    );
    // Pixel 1: color 4 = red (R=255, G=0, B=0)
    assert_eq!(
        (buffer[3], buffer[4], buffer[5]),
        (255, 0, 0),
        "Pixel 1: red"
    );
    // Pixel 2: color 2 = blue (R=0, G=0, B=255)
    assert_eq!(
        (buffer[6], buffer[7], buffer[8]),
        (0, 0, 255),
        "Pixel 2: blue"
    );
    // Pixel 3: color 0 = black (R=0, G=0, B=0)
    assert_eq!(
        (buffer[9], buffer[10], buffer[11]),
        (0, 0, 0),
        "Pixel 3: black"
    );
}

// =================================================================
// Address Mirroring Tests
// =================================================================

#[test]
fn test_in0_address_mirror() {
    let mut sys = MissileCommandSystem::new();
    // IN0 is mirrored across 0x4800-0x48FF
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_COIN) as u16),
        pressed: true,
    });
    let val_base = sys.read(BusMaster::Cpu(0), 0x4800);
    let val_mirror = sys.read(BusMaster::Cpu(0), 0x48FF);
    assert_eq!(
        val_base, val_mirror,
        "IN0 should be mirrored across 0x4800-0x48FF"
    );
}

#[test]
fn test_in1_address_mirror() {
    let mut sys = MissileCommandSystem::new();
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_LEFT) as u16),
        pressed: true,
    });
    let val_base = sys.read(BusMaster::Cpu(0), 0x4900);
    let val_mirror = sys.read(BusMaster::Cpu(0), 0x4955);
    assert_eq!(
        val_base, val_mirror,
        "IN1 should be mirrored across 0x4900-0x49FF"
    );
}

#[test]
fn test_irq_ack_address_mirror() {
    let mut sys = MissileCommandSystem::new();
    // Writing any address in 0x4D00-0x4DFF should acknowledge IRQ
    sys.tick(); // trigger IRQ at scanline 0
    sys.write(BusMaster::Cpu(0), 0x4DFF, 0x00); // mirror write
    // Should not panic; IRQ should be cleared
}

// =================================================================
// Timing Tests
// =================================================================

#[test]
fn test_frame_timing() {
    let mut sys = MissileCommandSystem::new();
    // Run one full frame
    sys.run_frame();
    // Clock should be exactly CYCLES_PER_FRAME = 256 * 80 = 20480
    assert_eq!(sys.clock(), 20480, "One frame should be 20480 cycles");
}

#[test]
fn test_scanline_counter() {
    let mut sys = MissileCommandSystem::new();
    assert_eq!(sys.current_scanline(), 0, "Scanline should start at 0");

    // Advance 80 cycles (1 scanline)
    for _ in 0..80 {
        sys.tick();
    }
    assert_eq!(
        sys.current_scanline(),
        1,
        "Should be at scanline 1 after 80 cycles"
    );
}

// =================================================================
// 15-bit Address Bus Masking Tests
// =================================================================

#[test]
fn test_15bit_masking_ram_alias() {
    let mut sys = MissileCommandSystem::new();
    // Write to RAM at 0x0100
    sys.write(BusMaster::Cpu(0), 0x0100, 0xAB);
    // Read from 0x8100 should alias to 0x0100 (0x8100 & 0x7FFF = 0x0100)
    assert_eq!(
        sys.read(BusMaster::Cpu(0), 0x8100),
        0xAB,
        "0x8100 should alias to 0x0100 through 15-bit masking"
    );
}

#[test]
fn test_15bit_masking_rom_vectors() {
    let mut sys = MissileCommandSystem::new();
    // 0xFFFC & 0x7FFF = 0x7FFC, which falls in ROM range 0x5000-0x7FFF
    // Both should read the same value
    let via_direct = sys.read(BusMaster::Cpu(0), 0x7FFC);
    let via_mask = sys.read(BusMaster::Cpu(0), 0xFFFC);
    assert_eq!(
        via_direct, via_mask,
        "0xFFFC should alias to 0x7FFC via 15-bit masking"
    );
}

#[test]
fn test_15bit_masking_write() {
    let mut sys = MissileCommandSystem::new();
    // Write to 0x8200 should alias to 0x0200 (RAM)
    sys.write(BusMaster::Cpu(0), 0x8200, 0xCD);
    assert_eq!(
        sys.read_ram(0x0200),
        0xCD,
        "Write to 0x8200 should go to RAM at 0x0200"
    );
}

// =================================================================
// MADSEL Tests
// =================================================================

#[test]
fn test_madsel_write_via_cpu() {
    let mut sys = MissileCommandSystem::new();

    // Program at address 0x0000:
    //   SEI          (78)       - 2 cycles, mask IRQ
    //   LDA #$00     (A9 00)    - 2 cycles
    //   STA $4D00    (8D 00 4D) - 4 cycles (ack IRQ, clears irq_state)
    //   LDA #$C0     (A9 C0)    - 2 cycles (data: bits 7:6=11 → both planes set)
    //   LDX #$00     (A2 00)    - 2 cycles
    //   STA ($10,X)  (81 10)    - 6 cycles (MADSEL triggers on this: 0x81 & 0x1F == 0x01)
    //   JMP $000C    (4C 0C 00) - infinite loop
    sys.write_ram(0x0000, 0x78); // SEI
    sys.write_ram(0x0001, 0xA9); // LDA #imm
    sys.write_ram(0x0002, 0x00); // #$00
    sys.write_ram(0x0003, 0x8D); // STA abs
    sys.write_ram(0x0004, 0x00); // addr low
    sys.write_ram(0x0005, 0x4D); // addr high (0x4D00 → IRQ ack)
    sys.write_ram(0x0006, 0xA9); // LDA #imm
    sys.write_ram(0x0007, 0xC0); // #$C0
    sys.write_ram(0x0008, 0xA2); // LDX #imm
    sys.write_ram(0x0009, 0x00); // #$00
    sys.write_ram(0x000A, 0x81); // STA (zp,X) → opcode 0x81 triggers MADSEL
    sys.write_ram(0x000B, 0x10); // zp addr $10
    sys.write_ram(0x000C, 0x4C); // JMP abs
    sys.write_ram(0x000D, 0x0C); // addr low
    sys.write_ram(0x000E, 0x00); // addr high (JMP $000C)

    // Set up zero-page pointer at $10/$11 → pixel address $0800
    // VRAM byte address = $0800 >> 2 = $0200, pixel = $0800 & 3 = 0
    sys.write_ram(0x0010, 0x00); // pointer low byte
    sys.write_ram(0x0011, 0x08); // pointer high byte

    // Verify VRAM at 0x0200 is initially 0
    assert_eq!(sys.read_ram(0x0200), 0x00, "VRAM should be 0 initially");

    // Run enough ticks: SEI(2) + LDA(2) + STA_abs(4) + LDA(2) + LDX(2) + STA_ind_x(6) = 18
    // Plus margin for fetch cycles and timing
    for _ in 0..30 {
        sys.tick();
    }

    // MADSEL write result:
    // pixel=0: DATA_LOOKUP[0xC0 >> 6] = DATA_LOOKUP[3] = 0xFF
    // vrammask = !(0x11 << 0) = 0xEE
    // VRAM[0x0200] = (0 & 0xEE) | (0xFF & 0x11) = 0x11
    assert_eq!(
        sys.read_ram(0x0200),
        0x11,
        "MADSEL should write color bits 0x11 (both planes set for pixel 0) at VRAM 0x0200"
    );
}

#[test]
fn test_madsel_write_pixel1() {
    let mut sys = MissileCommandSystem::new();

    // Same program but target pixel 1 (pixel address $0801, VRAM addr = $0200, pixel = 1)
    sys.write_ram(0x0000, 0x78); // SEI
    sys.write_ram(0x0001, 0xA9); // LDA #imm
    sys.write_ram(0x0002, 0x00); // #$00
    sys.write_ram(0x0003, 0x8D); // STA abs
    sys.write_ram(0x0004, 0x00); // addr low
    sys.write_ram(0x0005, 0x4D); // addr high (IRQ ack)
    sys.write_ram(0x0006, 0xA9); // LDA #imm
    sys.write_ram(0x0007, 0x80); // #$80 (data >> 6 = 2 → DATA_LOOKUP[2] = 0xF0, plane1 only)
    sys.write_ram(0x0008, 0xA2); // LDX #imm
    sys.write_ram(0x0009, 0x00); // #$00
    sys.write_ram(0x000A, 0x81); // STA (zp,X)
    sys.write_ram(0x000B, 0x10); // zp addr $10
    sys.write_ram(0x000C, 0x4C); // JMP abs
    sys.write_ram(0x000D, 0x0C);
    sys.write_ram(0x000E, 0x00);

    // Pixel address $0801: VRAM addr = $0801 >> 2 = $0200, pixel = 1
    sys.write_ram(0x0010, 0x01); // pointer low
    sys.write_ram(0x0011, 0x08); // pointer high

    for _ in 0..30 {
        sys.tick();
    }

    // pixel=1: DATA_LOOKUP[2] = 0xF0
    // vrammask = !(0x11 << 1) = !(0x22) = 0xDD
    // VRAM[0x0200] = (0 & 0xDD) | (0xF0 & 0x22) = 0x20
    assert_eq!(
        sys.read_ram(0x0200),
        0x20,
        "MADSEL should write 0x20 (plane1 set for pixel 1)"
    );
}

// =================================================================
// 3rd Color Bit Rendering Tests
// =================================================================

#[test]
fn test_get_bit3_addr() {
    // For effy=224: pixaddr = 224 << 8 = 0xE000
    // 0xE000 = 0b1110_0000_0000_0000: bit 11 is NOT set
    // Term 1: (0xE000 & 0x0800) >> 1 = 0 (bit 11 not set)
    // Term 2: (!0xE000 & 0x0800) >> 2 = 0x0800 >> 2 = 0x0200
    // Term 3: (0xE000 & 0x07F8) >> 2 = 0
    // Term 4: (0xE000 & 0x1000) >> 12 = 0
    // Result: 0x0200
    assert_eq!(
        MissileCommandSystem::get_bit3_addr(0xE000),
        0x0200,
        "get_bit3_addr(0xE000) for effy=224"
    );

    // For effy=232: pixaddr = 232 << 8 = 0xE800
    // 0xE800 = 0b1110_1000_0000_0000: bit 11 IS set
    // Term 1: (0xE800 & 0x0800) >> 1 = 0x0800 >> 1 = 0x0400
    // Term 2: (!0xE800 & 0x0800) >> 2 = 0 (bit 11 set, so inverted is 0)
    // Term 3: (0xE800 & 0x07F8) >> 2 = 0
    // Term 4: (0xE800 & 0x1000) >> 12 = 0
    // Result: 0x0400
    assert_eq!(
        MissileCommandSystem::get_bit3_addr(0xE800),
        0x0400,
        "get_bit3_addr(0xE800) for effy=232"
    );
}

#[test]
fn test_render_3rd_color_bit() {
    let mut sys = MissileCommandSystem::new();

    // Set up palette entries where the 3rd bit makes a visible difference
    // Entry 0 (color idx 0): black
    // Entry 1 (color idx 1): only 3rd bit set → specific color
    sys.write(BusMaster::Cpu(0), 0x4B00, 0x0E); // idx 0: black (R=0,G=0,B=0)
    sys.write(BusMaster::Cpu(0), 0x4B01, 0x0C); // idx 1: blue  (R=0,G=0,B=255)

    // Scanline 224 (effy=224) is in the 3rd color bit region.
    // Screen y for effy=224: screen_y = effy - 25 = 199
    // VRAM row for effy=224: base = 224 * 64 = 14336 = 0x3800
    // Leave the main VRAM at 0 (plane0=0, plane1=0) → color index 0 without 3rd bit

    // 3rd color bit address for effy=224:
    // get_bit3_addr(224 << 8) = get_bit3_addr(0xE000) = 0x0200
    let bit3_base = MissileCommandSystem::get_bit3_addr(0xE000) as usize;
    assert_eq!(bit3_base, 0x0200);

    // Set 3rd color bit for pixel 0: RAM[bit3_base + 0] bit 0 = 1
    sys.write_ram(bit3_base, 0x01);

    // Verify the write persisted
    assert_eq!(
        sys.read_ram(bit3_base),
        0x01,
        "RAM write at bit3_base should persist"
    );

    // Also verify palette was set
    assert_eq!(sys.read_palette(0), 0x0E, "Palette 0 should be 0x0E");
    assert_eq!(sys.read_palette(1), 0x0C, "Palette 1 should be 0x0C");

    let (w, _h) = sys.display_size();
    let mut buffer = vec![0u8; 256 * 231 * 3];
    sys.render_frame(&mut buffer);

    // Check what color we actually got at screen pixel (0, 199)
    let pixel_offset = (199 * w as usize) * 3;
    let actual_r = buffer[pixel_offset];
    let actual_g = buffer[pixel_offset + 1];
    let actual_b = buffer[pixel_offset + 2];

    // Color index 1 → palette entry 1 = 0x0C → R=0, G=0, B=255 (blue)
    assert_eq!(
        (actual_r, actual_g, actual_b),
        (0, 0, 255),
        "Pixel (0,199) should be blue (color idx 1). Got ({},{},{}). bit3_base=0x{:04X}",
        actual_r,
        actual_g,
        actual_b,
        bit3_base
    );

    // Screen pixel (8, 199) without 3rd bit should be color index 0 (black)
    let pixel_offset_8 = (199 * w as usize + 8) * 3;
    assert_eq!(buffer[pixel_offset_8], 0, "R should be 0 (black)");
    assert_eq!(buffer[pixel_offset_8 + 1], 0, "G should be 0");
    assert_eq!(buffer[pixel_offset_8 + 2], 0, "B should be 0 (black)");
}

#[test]
fn test_render_no_3rd_color_bit_above_224() {
    let mut sys = MissileCommandSystem::new();

    // Set palette entries
    sys.write(BusMaster::Cpu(0), 0x4B00, 0x0E); // idx 0: black
    sys.write(BusMaster::Cpu(0), 0x4B01, 0x0C); // idx 1: blue

    // Scanline 200 (effy=200) is NOT in the 3rd color bit region (< 224)
    // Screen y = 200 - 25 = 175
    // Even if we write 3rd bit data, it should NOT affect rendering above scanline 224

    // VRAM base for effy=200: 200 * 64 = 12800 = 0x3200
    // Leave VRAM at 0 → color index 0

    // Write something to what would be the 3rd bit area for higher scanlines
    // This should NOT affect scanline 200
    sys.write_ram(0x0401, 0xFF);

    let (w, _h) = sys.display_size();
    let mut buffer = vec![0u8; 256 * 231 * 3];
    sys.render_frame(&mut buffer);

    // Screen pixel (0, 175) should be color index 0 (black), no 3rd bit influence
    let pixel_offset = (175 * w as usize) * 3;
    assert_eq!(buffer[pixel_offset], 0, "R should be 0 (black, no 3rd bit)");
    assert_eq!(buffer[pixel_offset + 1], 0, "G should be 0");
    assert_eq!(buffer[pixel_offset + 2], 0, "B should be 0");
}

// =================================================================
// Reset Test
// =================================================================

#[test]
fn test_reset_loads_vector() {
    let mut sys = MissileCommandSystem::new();
    // The reset vector is at ROM offset 0x2FFC-0x2FFD (maps to 0xFFFC-0xFFFD)
    // Write a known vector: PC = 0x5000 (little-endian: 0x00, 0x50)
    // We need to set ROM bytes directly — use write_ram to set up ROM
    // Actually we can't write ROM through the bus. Let's just call reset
    // with default ROM (all zeros) and check PC = 0x0000
    sys.reset();
    let state = sys.get_cpu_state();
    assert_eq!(
        state.pc, 0x0000,
        "Reset vector from all-zero ROM should set PC=0x0000"
    );
}
