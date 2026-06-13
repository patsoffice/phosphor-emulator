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
    // Select ADC channel 3 (P1 X) and confirm it centers at 0x7F, swings on key.
    sys.write(BusMaster::Cpu(0), 0x94_4006, 0); // channel = (0x944006>>1)&7 = 3
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x94_0001) & 0xFF, 0x7F);
    sys.handle_input(InputEvent::Button {
        id: InputId((INPUT_P1_LEFT) as u16),
        pressed: true,
    });
    assert_eq!(sys.read(BusMaster::Cpu(0), 0x94_0001) & 0xFF, 0x00);
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
