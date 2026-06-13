use phosphor_core::core::machine::{
    InputConfigurable, InputEvent, InputId, MachineCore, Renderable,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::m6809::CcFlag;
use phosphor_machines::robotron::RobotronSystem;

// =================================================================
// Machine traits tests
// =================================================================

#[test]
fn test_display_size() {
    let sys = RobotronSystem::new();
    assert_eq!(sys.display_size(), (292, 240));
}

#[test]
fn test_input_map_has_all_buttons() {
    let sys = RobotronSystem::new();
    let controls = sys.input_controls();
    assert_eq!(controls.len(), 11); // 4 move + 4 fire + 2 start + coin
    for control in controls {
        assert!(!control.label.is_empty());
    }
}

#[test]
fn test_render_frame_correct_size() {
    let sys = RobotronSystem::new();
    let (w, h) = sys.display_size();
    let mut buffer = vec![0u8; (w * h * 3) as usize];
    sys.render_frame(&mut buffer); // Should not panic
}

// =================================================================
// Move Stick Input Tests (Widget PIA Port A bits 0-3)
// =================================================================

#[test]
fn test_move_stick_wiring() {
    use phosphor_machines::robotron::*;
    let mut sys = RobotronSystem::new();

    // Configure Widget PIA Port A as all input, data select
    sys.write(BusMaster::Cpu(0), 0xC804, 0x00); // DDRA = 0 (all input)
    sys.write(BusMaster::Cpu(0), 0xC805, 0x04); // CRA: bit 2 = 1 (data select)

    // All released → Port A should read 0x00
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val, 0x00, "All released should be 0x00 (active-high)");

    // Press Move Up → bit 0
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_MOVE_UP) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0x01, 0x01, "Move Up should set bit 0");

    // Press Move Down → bit 1
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_MOVE_DOWN) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0x03, 0x03, "Move Up+Down should set bits 0-1");

    // Press Move Left → bit 2
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_MOVE_LEFT) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0x07, 0x07, "Move Up+Down+Left should set bits 0-2");

    // Press Move Right → bit 3
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_MOVE_RIGHT) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0x0F, 0x0F, "All directions should set bits 0-3");

    // Release all
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_MOVE_UP) as u16),
        pressed: false,
    });
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_MOVE_DOWN) as u16),
        pressed: false,
    });
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_MOVE_LEFT) as u16),
        pressed: false,
    });
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_MOVE_RIGHT) as u16),
        pressed: false,
    });
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0x0F, 0x00, "All released should clear bits 0-3");
}

// =================================================================
// Fire Stick Input Tests (Port A bits 6-7, Port B bits 0-1)
// =================================================================

#[test]
fn test_fire_stick_port_a_wiring() {
    use phosphor_machines::robotron::*;
    let mut sys = RobotronSystem::new();

    // Configure Widget PIA Port A as all input, data select
    sys.write(BusMaster::Cpu(0), 0xC804, 0x00); // DDRA = 0
    sys.write(BusMaster::Cpu(0), 0xC805, 0x04); // CRA data select

    // Fire Up → Port A bit 6
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_UP) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0x40, 0x40, "Fire Up should set Port A bit 6");

    // Fire Down → Port A bit 7
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_DOWN) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0xC0, 0xC0, "Fire Up+Down should set Port A bits 6-7");

    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_UP) as u16),
        pressed: false,
    });
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_DOWN) as u16),
        pressed: false,
    });
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0xC0, 0x00, "Released should clear bits 6-7");
}

#[test]
fn test_fire_stick_port_b_wiring() {
    use phosphor_machines::robotron::*;
    let mut sys = RobotronSystem::new();

    // Configure Widget PIA Port B as all input, data select
    sys.write(BusMaster::Cpu(0), 0xC806, 0x00); // DDRB = 0
    sys.write(BusMaster::Cpu(0), 0xC807, 0x04); // CRB data select

    // Fire Left → Port B bit 0
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_LEFT) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0xC806);
    assert_eq!(val & 0x01, 0x01, "Fire Left should set Port B bit 0");

    // Fire Right → Port B bit 1
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_RIGHT) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0xC806);
    assert_eq!(
        val & 0x03,
        0x03,
        "Fire Left+Right should set Port B bits 0-1"
    );

    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_LEFT) as u16),
        pressed: false,
    });
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_RIGHT) as u16),
        pressed: false,
    });
    let val = sys.read(BusMaster::Cpu(0), 0xC806);
    assert_eq!(val & 0x03, 0x00, "Released should clear bits 0-1");
}

// =================================================================
// Start Button Tests (Port A bits 4-5)
// =================================================================

#[test]
fn test_start_buttons_wiring() {
    use phosphor_machines::robotron::*;
    let mut sys = RobotronSystem::new();

    // Configure Widget PIA Port A as all input, data select
    sys.write(BusMaster::Cpu(0), 0xC804, 0x00);
    sys.write(BusMaster::Cpu(0), 0xC805, 0x04);

    // P1 Start → bit 4 (note: swapped from Joust where P1=bit5)
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_P1_START) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0x10, 0x10, "P1 Start should set bit 4");

    // P2 Start → bit 5 (note: swapped from Joust where P2=bit4)
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_P2_START) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0x30, 0x30, "P1+P2 Start should set bits 4-5");

    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_P1_START) as u16),
        pressed: false,
    });
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_P2_START) as u16),
        pressed: false,
    });
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0x30, 0x00, "Released should clear bits 4-5");
}

// =================================================================
// Coin Input Test (ROM PIA Port A bit 4)
// =================================================================

#[test]
fn test_coin_wiring() {
    use phosphor_machines::robotron::INPUT_COIN;
    let mut sys = RobotronSystem::new();

    // Configure ROM PIA Port A as all input, data select
    sys.write(BusMaster::Cpu(0), 0xC80C, 0x00); // DDRA = 0
    sys.write(BusMaster::Cpu(0), 0xC80D, 0x04); // CRA data select

    // Coin → ROM PIA Port A bit 4
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_COIN) as u16),
        pressed: true,
    });
    let val = sys.read(BusMaster::Cpu(0), 0xC80C);
    assert_eq!(val & 0x10, 0x10, "Coin pressed should set bit 4");

    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_COIN) as u16),
        pressed: false,
    });
    let val = sys.read(BusMaster::Cpu(0), 0xC80C);
    assert_eq!(val & 0x10, 0x00, "Coin released should clear bit 4");
}

// =================================================================
// Memory Map Routing Tests
// =================================================================

#[test]
fn test_video_ram_read_write() {
    let mut sys = RobotronSystem::new();
    sys.board.write_video_ram(0x1234, 0xAB);
    assert_eq!(sys.board.read_video_ram(0x1234), 0xAB);
}

#[test]
fn test_video_ram_via_bus() {
    let mut sys = RobotronSystem::new();
    sys.write(BusMaster::Cpu(0), 0x1234, 0xCD);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x1234), 0xCD);
    assert_eq!(sys.board.read_video_ram(0x1234), 0xCD);
}

#[test]
fn test_palette_ram_read_write() {
    let mut sys = RobotronSystem::new();
    sys.write(BusMaster::Cpu(0), 0xC000, 0xAA);
    sys.write(BusMaster::Cpu(0), 0xC00F, 0xBB);
    assert_eq!(sys.board.read_palette(0), 0xAA);
    assert_eq!(sys.board.read_palette(15), 0xBB);
}

#[test]
fn test_rom_write_protection() {
    let mut sys = RobotronSystem::new();
    sys.board.load_program_rom(0, &[0xAA; 0x3000]);
    sys.write(BusMaster::Cpu(0), 0xD000, 0x55);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xD000), 0xAA);
}

#[test]
fn test_unmapped_returns_ff() {
    let mut sys = RobotronSystem::new();
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xC010), 0xFF);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0xC100), 0xFF);
}

// =================================================================
// Bank Switching Tests
// =================================================================

#[test]
fn test_rom_bank_select() {
    let mut sys = RobotronSystem::new();
    sys.write(BusMaster::Cpu(0), 0xC900, 0x03);
    assert_eq!(sys.board.rom_bank(), 0x03);
}

#[test]
fn test_bank_switch_enabled_reads_banked_rom() {
    let mut sys = RobotronSystem::new();
    sys.board.load_banked_rom(0x1000, &[0xAA]);
    sys.board.write_video_ram(0x1000, 0xBB);

    sys.write(BusMaster::Cpu(0), 0xC900, 0x01);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x1000), 0xAA);
}

#[test]
fn test_bank_switch_disabled_reads_video_ram() {
    let mut sys = RobotronSystem::new();
    sys.board.load_banked_rom(0x1000, &[0xAA]);
    sys.board.write_video_ram(0x1000, 0xBB);

    assert_eq!(sys.board.rom_bank(), 0);
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x1000), 0xBB);
}

// =================================================================
// Blitter Integration Tests
// =================================================================

#[test]
fn test_blitter_writes_to_video_ram() {
    let mut sys = RobotronSystem::new();

    sys.board.write_video_ram(0x1000, 0xAB);
    sys.board.write_video_ram(0x1001, 0xCD);

    // Configure blitter for a 2-byte copy
    sys.write(BusMaster::Cpu(0), 0xCA02, 0x10); // src_hi
    sys.write(BusMaster::Cpu(0), 0xCA03, 0x00); // src_lo
    sys.write(BusMaster::Cpu(0), 0xCA04, 0x20); // dst_hi
    sys.write(BusMaster::Cpu(0), 0xCA05, 0x00); // dst_lo
    sys.write(BusMaster::Cpu(0), 0xCA06, 2 ^ 4); // width = 2, XOR 4 for SC1
    sys.write(BusMaster::Cpu(0), 0xCA07, 1 ^ 4); // height = 1, XOR 4 for SC1
    sys.write(BusMaster::Cpu(0), 0xCA00, 0x00); // control: triggers blit

    for _ in 0..10 {
        sys.tick();
    }

    assert_eq!(sys.board.read_video_ram(0x2000), 0xAB);
    assert_eq!(sys.board.read_video_ram(0x2001), 0xCD);
}

// =================================================================
// Reset Tests
// =================================================================

#[test]
fn test_reset_loads_vector_from_rom() {
    let mut sys = RobotronSystem::new();
    sys.board.load_program_rom(0x2FFE, &[0xD1, 0x00]);
    sys.reset();
    assert_eq!(sys.board.get_cpu_state().pc, 0xD100);
}

#[test]
fn test_reset_masks_interrupts() {
    let mut sys = RobotronSystem::new();
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();
    let state = sys.board.get_cpu_state();
    assert_ne!(state.cc & (CcFlag::I as u8), 0);
    assert_ne!(state.cc & (CcFlag::F as u8), 0);
}

#[test]
fn test_reset_clears_input_state() {
    use phosphor_machines::robotron::*;
    let mut sys = RobotronSystem::new();

    // Configure PIAs for reading
    sys.write(BusMaster::Cpu(0), 0xC804, 0x00);
    sys.write(BusMaster::Cpu(0), 0xC805, 0x04);
    sys.write(BusMaster::Cpu(0), 0xC806, 0x00);
    sys.write(BusMaster::Cpu(0), 0xC807, 0x04);

    // Set some inputs
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_MOVE_UP) as u16),
        pressed: true,
    });
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_LEFT) as u16),
        pressed: true,
    });

    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();

    // After reset, PIAs are re-initialized so we need to configure again
    sys.write(BusMaster::Cpu(0), 0xC804, 0x00);
    sys.write(BusMaster::Cpu(0), 0xC805, 0x04);
    sys.write(BusMaster::Cpu(0), 0xC806, 0x00);
    sys.write(BusMaster::Cpu(0), 0xC807, 0x04);

    let port_a = sys.read(BusMaster::Cpu(0), 0xC804);
    let port_b = sys.read(BusMaster::Cpu(0), 0xC806);
    assert_eq!(port_a, 0x00, "Port A should be cleared after reset");
    assert_eq!(port_b, 0x00, "Port B should be cleared after reset");
}

// =================================================================
// Sound Board Tests
// =================================================================

#[test]
fn test_sound_cpu_reset_loads_vector() {
    let mut sys = RobotronSystem::new();
    sys.board.load_sound_rom(0x0FFE, &[0xF0, 0x80]);
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();
    assert_eq!(sys.board.get_sound_cpu_state().pc, 0xF080);
}

#[test]
fn test_sound_cpu_executes_independently() {
    let mut sys = RobotronSystem::new();
    // Sound ROM at 0xF000: LDA #$42, STA $0010, BRA *
    sys.board.load_sound_rom(
        0,
        &[
            0x86, 0x42, // LDA #$42
            0xB7, 0x00, 0x10, // STA $0010
            0x20, 0xFE, // BRA *
        ],
    );
    sys.board.load_sound_rom(0x0FFE, &[0xF0, 0x00]);
    sys.board.load_program_rom(0, &[0x20, 0xFE]); // BRA *
    sys.board.load_program_rom(0x2FFE, &[0xD0, 0x00]);
    sys.reset();

    for _ in 0..50 {
        sys.tick();
    }

    assert_eq!(sys.read(BusMaster::Cpu(1), 0x0010), 0x42);
}

// =================================================================
// Watchdog Tests
// =================================================================

#[test]
fn test_watchdog_reset_on_write() {
    let mut sys = RobotronSystem::new();
    for _ in 0..100 {
        sys.tick();
    }
    sys.write(BusMaster::Cpu(0), 0xCBFF, 0x39);
    assert_eq!(sys.board.watchdog_counter, 0);
}

#[test]
fn test_watchdog_ignores_non_0x39() {
    let mut sys = RobotronSystem::new();
    for _ in 0..100 {
        sys.tick();
    }
    let before = sys.board.watchdog_counter;
    sys.write(BusMaster::Cpu(0), 0xCBFF, 0x00);
    assert_eq!(sys.board.watchdog_counter, before);
}

// =================================================================
// Integration: Execute a Small Program
// =================================================================

#[test]
fn test_execute_simple_program() {
    let mut sys = RobotronSystem::new();
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
fn test_clock_advances() {
    let mut sys = RobotronSystem::new();
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
    let sys = RobotronSystem::default();
    assert_eq!(sys.display_size(), (292, 240));
    assert_eq!(sys.board.clock(), 0);
}

// =================================================================
// Twin-Stick Simultaneous Input Test
// =================================================================

#[test]
fn test_twin_stick_simultaneous_input() {
    use phosphor_machines::robotron::*;
    let mut sys = RobotronSystem::new();

    // Configure both PIA ports as input
    sys.write(BusMaster::Cpu(0), 0xC804, 0x00);
    sys.write(BusMaster::Cpu(0), 0xC805, 0x04);
    sys.write(BusMaster::Cpu(0), 0xC806, 0x00);
    sys.write(BusMaster::Cpu(0), 0xC807, 0x04);

    // Move up-right + fire down-left simultaneously
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_MOVE_UP) as u16),
        pressed: true,
    });
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_MOVE_RIGHT) as u16),
        pressed: true,
    });
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_DOWN) as u16),
        pressed: true,
    });
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_LEFT) as u16),
        pressed: true,
    });

    let port_a = sys.read(BusMaster::Cpu(0), 0xC804);
    let port_b = sys.read(BusMaster::Cpu(0), 0xC806);

    // Port A: move up (bit 0) + move right (bit 3) + fire down (bit 7)
    assert_eq!(port_a & 0x09, 0x09, "Move Up+Right on bits 0,3");
    assert_eq!(port_a & 0x80, 0x80, "Fire Down on bit 7");

    // Port B: fire left (bit 0)
    assert_eq!(port_b & 0x01, 0x01, "Fire Left on bit 0");

    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_MOVE_UP) as u16),
        pressed: false,
    });
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_MOVE_RIGHT) as u16),
        pressed: false,
    });
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_DOWN) as u16),
        pressed: false,
    });
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_FIRE_LEFT) as u16),
        pressed: false,
    });
}

// =================================================================
// No Mux Required Test (unlike Joust)
// =================================================================

#[test]
fn test_no_mux_required() {
    use phosphor_machines::robotron::*;
    let mut sys = RobotronSystem::new();

    // Configure Widget PIA Port A as input, data select
    sys.write(BusMaster::Cpu(0), 0xC804, 0x00);
    sys.write(BusMaster::Cpu(0), 0xC805, 0x04);

    // Robotron inputs should appear regardless of CB2 state (no mux)
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_MOVE_UP) as u16),
        pressed: true,
    });

    // CB2 = 0 (default) — inputs still visible
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0x01, 0x01, "Input visible with CB2=0");

    // CB2 = 1 — inputs still visible (no mux to change behavior)
    sys.write(BusMaster::Cpu(0), 0xC807, 0x3C); // CRB: CB2 output high
    let val = sys.read(BusMaster::Cpu(0), 0xC804);
    assert_eq!(val & 0x01, 0x01, "Input visible with CB2=1 (no mux)");

    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_MOVE_UP) as u16),
        pressed: false,
    });
}
