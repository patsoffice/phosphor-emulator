//! Black-box integration tests for Burgertime, exercising the machine through
//! the public `FrontendMachine` interface (Bus / InputConfigurable / DipSwitches
//! / MachineDebug) the frontend uses. White-box unit tests live alongside the
//! implementation in `btime.rs` / `burgertime.rs`.

use phosphor_core::core::machine::{
    DipSwitches, InputConfigurable, InputEvent, InputId, MachineCore, MachineDebug, Renderable,
};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_machines::burgertime::{
    BURGERTIME_MAIN_ROM, BurgertimeSystem, INPUT_COIN1, INPUT_COIN2, INPUT_P1_LEFT, INPUT_START1,
    INPUT_TILT,
};

const CPU: BusMaster = BusMaster::Cpu(0);

fn press(sys: &mut BurgertimeSystem, id: u8, pressed: bool) {
    sys.handle_input(InputEvent::Button {
        id: InputId(id as u16),
        pressed,
    });
}

// =================================================================
// Registration & metadata
// =================================================================

#[test]
fn registered_and_metadata() {
    assert!(phosphor_machines::registry::find("burgertime").is_some());
    let sys = BurgertimeSystem::new();
    assert_eq!(sys.machine_id(), "burgertime");
    assert_eq!(sys.display_size(), (240, 320)); // 3:4 portrait
    assert_eq!(sys.input_controls().len(), 15);
    assert!((sys.frame_rate_hz() - 57.44).abs() < 0.5);
}

#[test]
fn main_rom_region_layout() {
    // Region base 0xB000, size 0x5000; four 4KB chips at 0xC000-0xFFFF.
    assert_eq!(BURGERTIME_MAIN_ROM.size, 0x5000);
    assert_eq!(BURGERTIME_MAIN_ROM.entries.len(), 4);
    assert_eq!(BURGERTIME_MAIN_ROM.entries[0].offset, 0x1000); // -> 0xC000
}

// =================================================================
// Memory map routing (through the Bus)
// =================================================================

#[test]
fn ram_video_color_read_write() {
    let mut sys = BurgertimeSystem::new();
    sys.write(CPU, 0x0042, 0xAB); // work RAM
    sys.write(CPU, 0x1005, 0x12); // video RAM
    sys.write(CPU, 0x1405, 0x34); // color RAM
    assert_eq!(sys.read(CPU, 0x0042), 0xAB);
    assert_eq!(sys.read(CPU, 0x1005), 0x12);
    assert_eq!(sys.read(CPU, 0x1405), 0x34);
}

#[test]
fn rom_is_not_bus_writable() {
    let mut sys = BurgertimeSystem::new();
    sys.write(CPU, 0xC000, 0xFF);
    assert_eq!(sys.read(CPU, 0xC000), 0x00, "ROM writes are ignored");
}

// =================================================================
// X/Y-swap sprite-RAM mirror
// =================================================================

#[test]
fn xy_swap_mirror_reaches_video_ram() {
    let mut sys = BurgertimeSystem::new();
    // swap(off) = 32*(off%32) + off/32, an involution. swap(5) = 160 (0xA0),
    // so 0x1800 + 0xA0 mirrors video-RAM offset 5.
    sys.write(CPU, 0x1000 + 5, 0x7E);
    assert_eq!(sys.read(CPU, 0x1800 + 0xA0), 0x7E, "video mirror");

    // Color-RAM mirror at 0x1C00 likewise.
    sys.write(CPU, 0x1400 + 5, 0x11);
    assert_eq!(sys.read(CPU, 0x1C00 + 0xA0), 0x11, "color mirror");

    // A write through the mirror lands at the swapped offset (swap(1)=32).
    sys.write(CPU, 0x1801, 0x3C);
    assert_eq!(sys.read(CPU, 0x1000 + 32), 0x3C);
}

// =================================================================
// DECO CPU-7 opcode decryption
// =================================================================

#[test]
fn deco_decrypts_matching_opcode_fetch() {
    let mut sys = BurgertimeSystem::new();
    let mut rom = vec![0u8; 0x5000];
    rom[0x1104] = 0x84; // -> 0xC104, (addr & 0x0104) == 0x0104
    rom[0x1000] = 0x84; // -> 0xC000, (addr & 0x0104) == 0
    sys.board.load_main_rom(&rom);

    // A fresh CPU is in the Fetch state, so `is_sync()` is true and every read
    // behaves as an opcode fetch. Arm decryption with a write, then fetch.
    sys.write(CPU, 0x0000, 0x00);
    assert_eq!(sys.read(CPU, 0xC104), 0x0C, "0x84 bit-permuted");

    // Re-arm; a non-matching address is returned raw (flag still clears).
    sys.write(CPU, 0x0000, 0x00);
    assert_eq!(
        sys.read(CPU, 0xC000),
        0x84,
        "no decrypt off the 0x104 lines"
    );
}

#[test]
fn deco_no_decrypt_without_a_prior_write() {
    let mut sys = BurgertimeSystem::new();
    let mut rom = vec![0u8; 0x5000];
    rom[0x1104] = 0x84;
    sys.board.load_main_rom(&rom);
    // No write has armed the flag: the matching address is returned raw.
    assert_eq!(sys.read(CPU, 0xC104), 0x84);
}

// =================================================================
// GFX planar decode (via the board's decoded caches)
// =================================================================

#[test]
fn char_decode_combines_planes() {
    let mut sys = BurgertimeSystem::new();
    // gfx1 plane thirds at 0 / 0x2000 / 0x4000; char 1's plane-0 bytes are 8..16.
    let mut gfx1 = vec![0u8; 0x6000];
    for b in gfx1.iter_mut().skip(8).take(8) {
        *b = 0xFF;
    }
    sys.board.load_gfx1(&gfx1);
    assert_eq!(sys.board.chars().pixel(1, 0, 0), 1); // plane 0 only -> pen 1
    assert_eq!(sys.board.chars().pixel(1, 7, 7), 1);
    assert_eq!(sys.board.chars().pixel(0, 0, 0), 0); // untouched char
}

#[test]
fn sprite_decode_uses_split_row_halves() {
    let mut sys = BurgertimeSystem::new();
    // Sprite x offsets are [128..135, 0..7]: column 0 <- byte 16, column 8 <- byte 0.
    let mut gfx1 = vec![0u8; 0x6000];
    gfx1[16] = 0x80;
    gfx1[0] = 0x80;
    sys.board.load_gfx1(&gfx1);
    assert_eq!(sys.board.sprites().pixel(0, 0, 0), 1);
    assert_eq!(sys.board.sprites().pixel(0, 8, 0), 1);
    assert_eq!(sys.board.sprites().pixel(0, 1, 0), 0);
}

// =================================================================
// Palette (BGR_233_inverted), verified through the renderer
// =================================================================

fn render(sys: &BurgertimeSystem) -> Vec<u8> {
    let (w, h) = sys.display_size();
    let mut buf = vec![0u8; (w * h * 3) as usize];
    sys.render_frame(&mut buf);
    buf
}

#[test]
fn default_palette_renders_white() {
    // palette_ram = 0 -> inverted 0xFF -> white backdrop (rendered in new()).
    let sys = BurgertimeSystem::new();
    assert!(render(&sys).iter().all(|&c| c == 0xFF));
}

#[test]
fn palette_write_changes_rendered_color() {
    let mut sys = BurgertimeSystem::new();
    // Entry 0 = 0xFF -> inverted 0x00 -> black.
    sys.write(CPU, 0x0C00, 0xFF);
    sys.board.render();
    assert!(render(&sys).chunks_exact(3).all(|p| p == [0, 0, 0]));

    // Entry 0 = 0xF8 -> inverted 0x07 -> R=7 only -> pure red.
    sys.write(CPU, 0x0C00, 0xF8);
    sys.board.render();
    assert!(render(&sys).chunks_exact(3).all(|p| p == [0xFF, 0, 0]));
}

// =================================================================
// Inputs
// =================================================================

#[test]
fn p1_left_is_active_low_on_in0() {
    let mut sys = BurgertimeSystem::new();
    assert_eq!(sys.read(CPU, 0x4000), 0xFF, "idle high");
    press(&mut sys, INPUT_P1_LEFT, true);
    assert_eq!(sys.read(CPU, 0x4000) & 0x02, 0, "P1 Left clears IN0 bit 1");
    press(&mut sys, INPUT_P1_LEFT, false);
    assert_eq!(sys.read(CPU, 0x4000) & 0x02, 0x02, "released -> bit set");
}

#[test]
fn start_and_tilt_are_active_low_on_system() {
    let mut sys = BurgertimeSystem::new();
    press(&mut sys, INPUT_START1, true);
    assert_eq!(
        sys.read(CPU, 0x4002) & 0x01,
        0,
        "start1 clears system bit 0"
    );
    press(&mut sys, INPUT_TILT, true);
    assert_eq!(sys.read(CPU, 0x4002) & 0x04, 0, "tilt clears system bit 2");
}

#[test]
fn coins_are_active_high_and_latch_the_irq() {
    let mut sys = BurgertimeSystem::new();
    assert!(!sys.check_interrupts(CPU).irq);

    press(&mut sys, INPUT_COIN1, true);
    assert_ne!(sys.read(CPU, 0x4002) & 0x40, 0, "coin1 sets system bit 6");
    assert!(sys.check_interrupts(CPU).irq, "coin1 edge asserts IRQ");

    // Vectoring through 0xFFFE acknowledges the HOLD_LINE IRQ.
    sys.read(CPU, 0xFFFE);
    assert!(!sys.check_interrupts(CPU).irq);

    press(&mut sys, INPUT_COIN2, true);
    assert_ne!(sys.read(CPU, 0x4002) & 0x80, 0, "coin2 sets system bit 7");
    assert!(sys.check_interrupts(CPU).irq, "coin2 edge asserts IRQ");
}

// =================================================================
// DIP switches
// =================================================================

#[test]
fn dip_defaults_and_ports() {
    let mut sys = BurgertimeSystem::new();
    // Read the raw ports (mask off the live VBLANK bit on DSW1).
    assert_eq!(sys.read(CPU, 0x4003) & 0x7F, 0x1F);
    assert_eq!(sys.read(CPU, 0x4004), 0x0B);
    // ...and through the DipSwitches API.
    assert_eq!(sys.dip_bank_value(0), 0x1F);
    assert_eq!(sys.dip_bank_value(1), 0x0B);

    // Switching DSW2 "Lives" to 5 (option 0 -> 0x00) leaves the byte's other bits.
    sys.set_dip_option(1, 0, 0x00);
    assert_eq!(sys.dip_bank_value(1) & 0x01, 0x00);
    assert_eq!(sys.dip_bank_value(1) & 0x0E, 0x0A);
}

// =================================================================
// Live VBLANK bit
// =================================================================

#[test]
fn vblank_bit_toggles_on_0x4003() {
    let mut sys = BurgertimeSystem::new();
    // clock = 0 -> scanline 0 -> outside the visible [8,248) window -> VBLANK set.
    assert_ne!(sys.read(CPU, 0x4003) & 0x80, 0, "vblank at scanline 0");

    // Advance into the visible window (scanline 100 = 100 * 96 cycles).
    for _ in 0..100 * 96 {
        sys.debug_tick();
    }
    assert_eq!(sys.read(CPU, 0x4003) & 0x80, 0, "no vblank mid-screen");
}
