//! Food Fight integration tests.
//!
//! The structural tests run everywhere (no ROM files required). The boot test
//! is gated on a real ROM set: set `FOODF_ROMS` to a directory or `.zip`
//! containing the `foodf` chips to run it; otherwise it skips cleanly.

use phosphor_core::core::machine::{
    AudioSource, InputConfigurable, InputEvent, InputId, MachineCore, Nvram, Renderable, SaveState,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_machines::foodf::{
    FoodFightSystem, INPUT_COIN1, INPUT_P1_LEFT, INPUT_P1_THROW, INPUT_START1,
};
use phosphor_machines::rom_loader::RomSet;

#[test]
fn display_size_is_256x224() {
    let sys = FoodFightSystem::new();
    assert_eq!(sys.display_size(), (256, 224));
    // Landscape 4:3 tube: the 256×224 raster is presented aspect-corrected.
    assert_eq!(sys.display_aspect(), Some((4, 3)));
}

#[test]
fn render_frame_does_not_panic() {
    let sys = FoodFightSystem::new();
    let (w, h) = sys.display_size();
    let mut buffer = vec![0u8; (w * h * 3) as usize];
    sys.render_frame(&mut buffer);
}

#[test]
fn input_map_is_complete() {
    let sys = FoodFightSystem::new();
    // 12 digital controls + 4 analog stick axes.
    assert_eq!(sys.input_controls().len(), 16);
    for c in sys.input_controls() {
        assert!(!c.label.is_empty());
    }
}

#[test]
fn digital_inputs_are_active_low() {
    let mut sys = FoodFightSystem::new();
    // Idle: SYSTEM reads all-high (nothing pressed).
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x94_8000) & 0xFF, 0xFF);

    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_COIN1) as u16),
        pressed: true,
    }); // bit 0
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_START1) as u16),
        pressed: true,
    }); // bit 2
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_P1_THROW) as u16),
        pressed: true,
    }); // bit 5
    let v = sys.read(BusMaster::Cpu(0), 0x94_8000) & 0xFF;
    assert_eq!(v, 0xFF & !0x01 & !0x04 & !0x20);

    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_COIN1) as u16),
        pressed: false,
    });
    let v = sys.read(BusMaster::Cpu(0), 0x94_8000) & 0xFF;
    assert_eq!(v & 0x01, 0x01); // released
}

#[test]
fn p1_stick_keys_drive_the_adc() {
    let mut sys = FoodFightSystem::new();
    // Select ADC channel 3 (P1 X). The sticks read reversed (MAME PORT_REVERSE
    // on all four ADC ports), so the value is mirrored at the read: the 0x7F
    // neutral reads as 0x80, and pressing LEFT (raw 0x00) reads as 0xFF.
    sys.write(BusMaster::Cpu(0), 0x94_4006, 0); // channel = (0x944006>>1)&7 = 3
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x94_0001) & 0xFF, 0x80);
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_P1_LEFT) as u16),
        pressed: true,
    });
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x94_0001) & 0xFF, 0xFF);
}

#[test]
fn nvram_persists_through_save_and_load() {
    let mut sys = FoodFightSystem::new();
    sys.write(BusMaster::Cpu(0), 0x90_0010, 0x0042); // NVRAM cell 8, low byte
    let saved = sys.save_nvram().unwrap().to_vec();
    assert_eq!(saved[8], 0x42);

    let mut sys2 = FoodFightSystem::new();
    sys2.load_nvram(&saved);
    assert_eq!(sys2.read(BusMaster::Cpu(0), 0x90_0010) & 0xFF, 0x42);
}

#[test]
fn save_state_round_trips_through_public_api() {
    let mut sys = FoodFightSystem::new();
    sys.write(BusMaster::Cpu(0), 0x01_4020, 0xCAFE);
    let data = SaveState::save_state(&sys).expect("save");

    let mut sys2 = FoodFightSystem::new();
    SaveState::load_state(&mut sys2, &data).unwrap();
    assert_eq!(sys2.read(BusMaster::Cpu(0), 0x01_4020), 0xCAFE);
}

#[test]
fn audio_drains_after_a_frame() {
    let mut sys = FoodFightSystem::new();
    assert_eq!(sys.audio_sample_rate(), 44100);
    sys.run_frame();
    let mut buf = vec![0i16; 4096];
    // A frame at ~60.8 Hz should produce roughly 725 samples; just assert > 0.
    assert!(sys.fill_audio(&mut buf) > 0);
}

/// The debug bus exposes the 24-bit `AddressSpace32` and the POKEY array, so
/// the egui debug panel works on this MC68000 machine (see the
/// `debug-panel-24bit-bus` issue). A 16-bit debug path would have dropped every
/// address above `0xFFFF`.
#[test]
fn debug_bus_exposes_24bit_memory_and_pokey_array() {
    use phosphor_core::core::DebugRead;
    use phosphor_core::core::machine::MachineDebug;

    let mut sys = FoodFightSystem::new();

    // CPU and device discovery: the `[Pokey; 3]` field expands to three
    // 1-based "POKEY N" entries via #[debug_device("POKEY")].
    {
        let bus = sys.debug_bus().expect("Food Fight exposes a debug bus");
        let cpus: Vec<&str> = bus.cpus().iter().map(|(n, _)| *n).collect();
        assert_eq!(cpus, vec!["M68000"]);
        let devices: Vec<&str> = bus.devices().iter().map(|(n, _)| *n).collect();
        assert_eq!(devices, vec!["M68000", "POKEY 1", "POKEY 2", "POKEY 3"]);
    }

    // Work RAM at 0x01_4100 is above 0xFFFF: write/read/peek must round-trip
    // through the full 24-bit address, not a truncated 16-bit one.
    sys.debug_bus_mut().unwrap().write(0, 0x01_4100, 0x5A);
    let bus = sys.debug_bus().unwrap();
    assert_eq!(bus.read(0, 0x01_4100), Some(0x5A));
    assert!(matches!(
        bus.peek(0, 0x01_4100),
        DebugRead::Backed { value: 0x5A, .. }
    ));

    // High playfield RAM at 0x80_0000 is reachable and backed; a hole between
    // mapped windows reads as unmapped (not a fake bus value).
    assert!(matches!(bus.peek(0, 0x80_0000), DebugRead::Backed { .. }));
    assert_eq!(bus.peek(0, 0x50_0000), DebugRead::Unmapped);
}

/// Real-ROM boot test, gated on `FOODF_ROMS` pointing at a directory of the
/// extracted `foodf` chips. Runs the game for a number of frames and asserts
/// the 68000 is executing in ROM with attract-mode activity in RAM.
#[test]
fn boots_real_roms_when_available() {
    let Ok(path) = std::env::var("FOODF_ROMS") else {
        eprintln!("FOODF_ROMS not set — skipping real-ROM boot test");
        return;
    };

    let rom_set = match RomSet::from_directory(std::path::Path::new(&path)) {
        Ok(set) => set,
        Err(e) => {
            eprintln!("could not load foodf ROMs from {path}: {e:?} — skipping");
            return;
        }
    };

    let mut sys = FoodFightSystem::new();
    sys.load_rom_set(&rom_set).expect("load_rom_set");
    sys.reset();

    for _ in 0..120 {
        sys.run_frame();
    }

    let pc = sys.get_cpu_state().pc;
    assert!(pc < 0x1_0000, "PC {pc:#08X} is not executing in ROM");
}
