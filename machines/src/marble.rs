//! Atari Marble Madness (1984) — the project's first 16-bit multi-CPU board.
//!
//! Marble Madness runs on **Atari System 1**. All of the shared hardware — the
//! MC68010, the M6502 sound board, the Slapstic, the EEPROM, and the tilemap +
//! motion-object video pipeline — lives in [`AtariSystem1Board`]; this module is
//! the thin game wrapper (repo board-wrapper pattern, like `JoustSystem` on
//! `WilliamsBoard`). It adds only Marble's cartridge ROM manifest, its slapstic
//! chip (137412-103), and its 45°-mounted **trackball** input at `0xF20000`.
//!
//! The wrapper's [`Bus`] intercepts the trackball (and the unused joystick/ADC
//! window) and forwards every other access to the board.

use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::input::{DrainPolicy, RelativeCounter};
use phosphor_core::core::machine::{
    AnalogAxisKind, DefaultBinding, Direction, InputConfigurable, InputControl, InputEvent,
    InputId, InputKind, MachineCore, MouseControl, Nvram, Profilable, SaveState,
};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::state::M68000State;

use crate::atari_system1::{self, AtariSystem1Board, AtariSystem1Bus};
use crate::disasm_registry::{DisasmCpu, DisasmRegion};
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_low;
use phosphor_core::cpu::m68000::M68000;

// ---------------------------------------------------------------------------
// ROM manifest ("marble" parent set, TTL Rev 2 motherboard BIOS)
// ---------------------------------------------------------------------------

/// All 68010 program chips, concatenated back-to-back in load order, then
/// de-interleaved into the big-endian `maincpu` image by [`load_maincpu_image`].
///
/// Order: BIOS even/odd, then the four cartridge banks (even/odd each), then the
/// two slapstic chips (even/odd). Each chip is 0x4000 bytes; even chip = high
/// byte of the 68k word, odd chip = low byte (standard 16-bit ROM interleave).
pub static MARBLE_PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x30000,
    entries: &[
        // Motherboard BIOS (TTL Rev 2) — holds the reset vectors at 0x000000.
        RomEntry {
            name: "136032.205.l13",
            size: 0x4000,
            offset: 0x00000,
            crc32: &[0x88d0be26],
        },
        RomEntry {
            name: "136032.206.l12",
            size: 0x4000,
            offset: 0x04000,
            crc32: &[0x3c79ef05],
        },
        // Cartridge program banks (even/odd pairs).
        RomEntry {
            name: "136033.623",
            size: 0x4000,
            offset: 0x08000,
            crc32: &[0x284ed2e9],
        },
        RomEntry {
            name: "136033.624",
            size: 0x4000,
            offset: 0x0C000,
            crc32: &[0xd541b021],
        },
        RomEntry {
            name: "136033.625",
            size: 0x4000,
            offset: 0x10000,
            crc32: &[0x563755c7],
        },
        RomEntry {
            name: "136033.626",
            size: 0x4000,
            offset: 0x14000,
            crc32: &[0x860feeb3],
        },
        RomEntry {
            name: "136033.627",
            size: 0x4000,
            offset: 0x18000,
            crc32: &[0xd1dbd439],
        },
        RomEntry {
            name: "136033.628",
            size: 0x4000,
            offset: 0x1C000,
            crc32: &[0x957d6801],
        },
        RomEntry {
            name: "136033.229",
            size: 0x4000,
            offset: 0x20000,
            crc32: &[0xc81d5c14],
        },
        RomEntry {
            name: "136033.630",
            size: 0x4000,
            offset: 0x24000,
            crc32: &[0x687a09f7],
        },
        // Slapstic-banked ROM (even/odd).
        RomEntry {
            name: "136033.107",
            size: 0x4000,
            offset: 0x28000,
            crc32: &[0xf3b8745b],
        },
        RomEntry {
            name: "136033.108",
            size: 0x4000,
            offset: 0x2C000,
            crc32: &[0xe51eecaa],
        },
    ],
};

/// Alphanumerics character ROM (the shared motherboard font, 136032.104.f5):
/// 512 tiles, 8×8, 2bpp.
pub static MARBLE_ALPHA_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[RomEntry {
        name: "136032.104.f5",
        size: 0x2000,
        offset: 0x0000,
        crc32: &[0x7a29dc07],
    }],
};

/// Playfield / motion-object tile ROM ("tiles" region, 0x100000). Two 0x80000
/// banks; bank 1 holds five bitplanes, bank 2 three. The region is
/// `ROMREGION_INVERT | ROMREGION_ERASEFF`: erase to 0xFF, place the chips, then
/// invert the whole buffer (gaps 0xFF→0x00 = erase, data → inverted), see
/// [`MarbleSystem::load_rom_set`]. Bank 2's absent plane 3 thus reads 0, keeping
/// sprite pens in 0-7 rather than forcing pen bit 3.
pub static MARBLE_TILE_ROM: RomRegion = RomRegion {
    size: 0x100000,
    entries: &[
        // Bank 1: planes 0-4, each plane two 0x4000 ROMs, planes 0x10000 apart.
        RomEntry {
            name: "136033.137",
            size: 0x4000,
            offset: 0x00000,
            crc32: &[0x7a45f5c1],
        },
        RomEntry {
            name: "136033.138",
            size: 0x4000,
            offset: 0x04000,
            crc32: &[0x7e954a88],
        },
        RomEntry {
            name: "136033.139",
            size: 0x4000,
            offset: 0x10000,
            crc32: &[0x1eb1bb5f],
        },
        RomEntry {
            name: "136033.140",
            size: 0x4000,
            offset: 0x14000,
            crc32: &[0x8a82467b],
        },
        RomEntry {
            name: "136033.141",
            size: 0x4000,
            offset: 0x20000,
            crc32: &[0x52448965],
        },
        RomEntry {
            name: "136033.142",
            size: 0x4000,
            offset: 0x24000,
            crc32: &[0xb4a70e4f],
        },
        RomEntry {
            name: "136033.143",
            size: 0x4000,
            offset: 0x30000,
            crc32: &[0x7156e449],
        },
        RomEntry {
            name: "136033.144",
            size: 0x4000,
            offset: 0x34000,
            crc32: &[0x4c3e4c79],
        },
        RomEntry {
            name: "136033.145",
            size: 0x4000,
            offset: 0x40000,
            crc32: &[0x9062be7f],
        },
        RomEntry {
            name: "136033.146",
            size: 0x4000,
            offset: 0x44000,
            crc32: &[0x14566dca],
        },
        // Bank 2: planes 0-2, data 0x4000 into each plane slot (tiles 2048+).
        RomEntry {
            name: "136033.149",
            size: 0x4000,
            offset: 0x84000,
            crc32: &[0xb6658f06],
        },
        RomEntry {
            name: "136033.151",
            size: 0x4000,
            offset: 0x94000,
            crc32: &[0x84ee1c80],
        },
        RomEntry {
            name: "136033.153",
            size: 0x4000,
            offset: 0xa4000,
            crc32: &[0xdaa02926],
        },
    ],
};

/// Graphics-mapping PROMs ("proms" region, 0x400). `prom1` (136033.118) at 0x000
/// and `prom2` (136033.119) at 0x200 drive the per-tile bank / bpp / colour /
/// offset lookup. Entries 0-255 are the playfield, 256-511 the motion objects.
pub static MARBLE_PROM: RomRegion = RomRegion {
    size: 0x400,
    entries: &[
        RomEntry {
            name: "136033.118",
            size: 0x200,
            offset: 0x000,
            crc32: &[0x2101b0ed],
        },
        RomEntry {
            name: "136033.119",
            size: 0x200,
            offset: 0x200,
            crc32: &[0x19f6e767],
        },
    ],
};

/// M6502 sound program. 64 KB region with ROM at 0x8000-0xFFFF.
pub static MARBLE_SOUND_ROM: RomRegion = RomRegion {
    size: 0x10000,
    entries: &[
        RomEntry {
            name: "136033.421",
            size: 0x4000,
            offset: 0x8000,
            crc32: &[0x78153dc3],
        },
        RomEntry {
            name: "136033.422",
            size: 0x4000,
            offset: 0xC000,
            crc32: &[0x2e66300e],
        },
    ],
};

/// Build the 0x88000-byte `maincpu` image from the concatenated chips: the
/// 68010 program at 000000-07FFFF and the slapstic ROM at 080000-087FFF, with
/// each even/odd chip pair de-interleaved into big-endian words.
fn load_maincpu_image(rom_set: &RomSet) -> Result<Vec<u8>, RomLoadError> {
    let chips = MARBLE_PROGRAM_ROM.load(rom_set)?;
    let mut image = vec![0u8; 0x88000];
    // (dst_base, even_chip_offset, odd_chip_offset) for each 0x4000-byte pair.
    const PAIRS: [(usize, usize, usize); 6] = [
        (0x00000, 0x00000, 0x04000), // BIOS
        (0x10000, 0x08000, 0x0C000), // cartridge bank 0
        (0x18000, 0x10000, 0x14000), // cartridge bank 1
        (0x20000, 0x18000, 0x1C000), // cartridge bank 2
        (0x28000, 0x20000, 0x24000), // cartridge bank 3
        (0x80000, 0x28000, 0x2C000), // slapstic
    ];
    for (dst, even, odd) in PAIRS {
        for i in 0..0x4000 {
            image[dst + 2 * i] = chips[even + i]; // even address = high byte
            image[dst + 2 * i + 1] = chips[odd + i]; // odd address = low byte
        }
    }
    Ok(image)
}

// ---------------------------------------------------------------------------
// Input IDs
// ---------------------------------------------------------------------------

pub const INPUT_START1: u8 = 0;
pub const INPUT_START2: u8 = 1;
/// Service / self-test switch (F60000 bit 6, active-low `PORT_SERVICE`).
pub const INPUT_SERVICE: u8 = 2;
/// Coin insert (sound port 0x1820 bit 0, active-low).
pub const INPUT_COIN: u8 = 3;
/// P1 trackball direction keys (the keyboard way to roll the ball).
pub const INPUT_P1_TRACK_LEFT: u8 = 4;
pub const INPUT_P1_TRACK_RIGHT: u8 = 5;
pub const INPUT_P1_TRACK_UP: u8 = 6;
pub const INPUT_P1_TRACK_DOWN: u8 = 7;

/// Counter step per frame for a held trackball direction key — a gentle roll
/// in the range of a normal mouse motion (the marble accelerates from sustained
/// input, so a small constant is plenty).
const TRACK_KEY_STEP: i32 = 6;
/// Max counter change applied per frame, so a fast flick can't alias the 8-bit
/// counter (the game reads the delta as a signed byte, valid to ±127).
const TRACK_MAX_STEP: i32 = 100;

/// Typed control ids for the analog trackball axes — a separate `InputId`
/// namespace from the digital buttons above.
const CTRL_P1_TRACK_X: InputId = InputId(10);
const CTRL_P1_TRACK_Y: InputId = InputId(11);
const CTRL_P2_TRACK_X: InputId = InputId(12);
const CTRL_P2_TRACK_Y: InputId = InputId(13);

const MARBLE_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_COIN as u16),
        stable_name: "coin",
        label: "Coin",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(INPUT_START1 as u16),
        stable_name: "p1_start",
        label: "P1 Start",
        kind: InputKind::Start,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_START,
    },
    InputControl {
        id: InputId(INPUT_START2 as u16),
        stable_name: "p2_start",
        label: "P2 Start",
        kind: InputKind::Start,
        player: Some(2),
        default_bindings: crate::input_defaults::P2_START,
    },
    InputControl {
        id: InputId(INPUT_SERVICE as u16),
        stable_name: "service",
        label: "Service / Self-Test",
        kind: InputKind::Service,
        player: None,
        default_bindings: crate::input_defaults::SERVICE,
    },
    // P1 trackball roll via the keyboard / D-pad (digital fallback for the
    // analog axes below), bound to the standard P1 direction defaults.
    InputControl {
        id: InputId(INPUT_P1_TRACK_LEFT as u16),
        stable_name: "p1_track_left",
        label: "P1 Roll Left",
        kind: InputKind::DigitalDirection {
            direction: Direction::Left,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(INPUT_P1_TRACK_RIGHT as u16),
        stable_name: "p1_track_right",
        label: "P1 Roll Right",
        kind: InputKind::DigitalDirection {
            direction: Direction::Right,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
    InputControl {
        id: InputId(INPUT_P1_TRACK_UP as u16),
        stable_name: "p1_track_up",
        label: "P1 Roll Up",
        kind: InputKind::DigitalDirection {
            direction: Direction::Up,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_UP,
    },
    InputControl {
        id: InputId(INPUT_P1_TRACK_DOWN as u16),
        stable_name: "p1_track_down",
        label: "P1 Roll Down",
        kind: InputKind::DigitalDirection {
            direction: Direction::Down,
        },
        player: Some(1),
        default_bindings: crate::input_defaults::P1_DOWN,
    },
    // Trackballs: P1 drives the mouse; P2 is rebindable (no mouse default).
    InputControl {
        id: CTRL_P1_TRACK_X,
        stable_name: "p1_trackball_x",
        label: "P1 Trackball X",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::X,
        },
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::AxisX)],
    },
    InputControl {
        id: CTRL_P1_TRACK_Y,
        stable_name: "p1_trackball_y",
        label: "P1 Trackball Y",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::Y,
        },
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::AxisY)],
    },
    InputControl {
        id: CTRL_P2_TRACK_X,
        stable_name: "p2_trackball_x",
        label: "P2 Trackball X",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::X,
        },
        player: Some(2),
        default_bindings: &[],
    },
    InputControl {
        id: CTRL_P2_TRACK_Y,
        stable_name: "p2_trackball_y",
        label: "P2 Trackball Y",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::Y,
        },
        player: Some(2),
        default_bindings: &[],
    },
];

// ---------------------------------------------------------------------------
// MarbleSystem — Atari System 1 board configured for Marble Madness
// ---------------------------------------------------------------------------

/// Atari Marble Madness (System 1) — the shared board plus Marble's trackballs.
#[derive(phosphor_macros::BusDebug)]
pub struct MarbleSystem {
    /// The 68010 is held beside the bus view over the board.
    #[debug_cpu("M68010")]
    pub cpu: M68000,

    #[debug_bus]
    pub board: AtariSystem1Board,

    /// Trackball counters [p1x, p1y, p2x, p2y] — free-running 8-bit counters,
    /// the trackball motion the game samples at 0xF20000.
    trackball: [RelativeCounter; 4],
    /// Per-player 45°-rotated counter pair, latched on the even (X) read so the
    /// paired odd (Y) read sees the same snapshot — see [`Self::trackball_read`].
    trackball_cur: [[u8; 2]; 2],
}

/// The four trackball counters (P1 X, P1 Y, P2 X, P2 Y). Motion is rate-limited
/// but never discarded, so a fast flick keeps rolling after the input stops —
/// the ball has momentum, which is what distinguishes ClampCarry from the
/// ClampDrop spinners. The X axes negate at apply time to match the cabinet's
/// PORT_REVERSE wiring.
fn new_trackball() -> [RelativeCounter; 4] {
    std::array::from_fn(|axis| {
        RelativeCounter::new(
            0xFF,
            TRACK_KEY_STEP,
            axis % 2 == 0,
            DrainPolicy::ClampCarry {
                max_step: TRACK_MAX_STEP,
            },
        )
    })
}

impl MarbleSystem {
    pub fn new() -> Self {
        Self {
            cpu: AtariSystem1Board::new_cpu(),
            board: AtariSystem1Board::new(103, false),
            trackball: new_trackball(),
            trackball_cur: [[0; 2]; 2],
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let image = load_maincpu_image(rom_set)?;
        self.board.load_program(&image);

        // Alpha (text/HUD) font tiles.
        let alpha = MARBLE_ALPHA_ROM.load(rom_set)?;
        self.board.load_alpha(&alpha);

        // Playfield + motion-object tile banks and PROM remap. The tiles region
        // is ROMREGION_INVERT | ROMREGION_ERASEFF: erase to 0xFF, place the
        // chips, then invert the whole buffer — so gaps become 0x00 and chip
        // data is inverted. (Gaps matter: bank 2 has no plane-3 chip, and a 0x00
        // plane 3 keeps its sprite pens in 0-7 — an 0xFF gap would force pen bit
        // 3 and render the marble black.)
        let mut tiles = MARBLE_TILE_ROM.load_erased(rom_set, 0xFF)?;
        for b in tiles.iter_mut() {
            *b = !*b;
        }
        let prom = MARBLE_PROM.load(rom_set)?;
        self.board.load_gfx(&prom, &tiles);

        // M6502 sound program.
        let sound_image = MARBLE_SOUND_ROM.load(rom_set)?;
        self.board.load_sound(&sound_image);
        Ok(())
    }

    // -- Bring-up diagnostics (forwarded to the board) -----------------------

    pub fn get_cpu_state(&self) -> M68000State {
        use phosphor_core::cpu::CpuStateTrait;
        self.cpu.snapshot()
    }

    pub fn clock(&self) -> u64 {
        self.board.clock()
    }

    pub fn sound_debug(&self) -> (bool, u64, bool, bool) {
        self.board.sound_debug()
    }

    pub fn eeprom_debug(&self) -> (usize, u64) {
        self.board.eeprom_debug()
    }

    pub fn video_ram_stats(&self) -> (usize, usize, usize) {
        self.board.video_ram_stats()
    }

    // -- Trackball (Marble-specific input) -----------------------------------

    /// Trackball read (0xF20000-0xF20007: four byte ports). Marble's trackballs
    /// are mounted at 45°, so the hardware returns rotated counter pairs: the X
    /// port yields `x + y`, the paired Y port `x - y`. The even (X) read latches
    /// both from the live counters so the odd (Y) read sees the same snapshot.
    /// Advance the trackball counters once per frame from pending input: held P1
    /// direction keys add a fixed step, then each axis drains a capped amount of
    /// its accumulator (mouse + keys) into the 8-bit counter. The X axes are
    /// reversed to match the cabinet's PORT_REVERSE wiring.
    /// Borrow the CPU and the bus it drives as two disjoint pieces.
    #[inline]
    fn split(&mut self) -> (&mut M68000, MarbleBus<'_>) {
        (
            &mut self.cpu,
            MarbleBus {
                board: &mut self.board,
                trackball_cur: &mut self.trackball_cur,
                trackball: &mut self.trackball,
            },
        )
    }

    /// One CPU cycle. Returns 1 at an instruction boundary (for the debugger,
    /// which steps instructions rather than cycles).
    pub fn step_cycle(&mut self) -> u32 {
        let (cpu, mut bus) = self.split();
        atari_system1::tick(cpu, &mut bus);
        AtariSystem1Board::instruction_boundaries(&self.cpu)
    }

    /// Read the CPU-facing bus, side effects and all. Distinct from the
    /// debugger's `BusDebug::peek`/`poke`, which avoid side effects.
    pub fn bus_read(&mut self, master: BusMaster, addr: u32) -> u16 {
        self.split().1.read(master, addr)
    }

    /// Write the CPU-facing bus, side effects and all. See [`Self::bus_read`].
    pub fn bus_write(&mut self, master: BusMaster, addr: u32, data: u16) {
        self.split().1.write(master, addr, data);
    }

    fn update_trackball(&mut self) {
        for counter in &mut self.trackball {
            counter.update();
        }
    }
}

impl Default for MarbleSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus — Marble ports intercepted, everything else forwarded to the board
// ---------------------------------------------------------------------------

/// The Marble Madness bus: the shared board plus this game's trackballs.
struct MarbleBus<'a> {
    board: &'a mut AtariSystem1Board,
    trackball_cur: &'a mut [[u8; 2]; 2],
    trackball: &'a mut [RelativeCounter; 4],
}

impl AtariSystem1Bus for MarbleBus<'_> {
    #[inline]
    fn board(&mut self) -> &mut AtariSystem1Board {
        self.board
    }
}

impl MarbleBus<'_> {
    /// Read one of the four rotated trackball counters. The even (X) read
    /// latches both axes so the paired odd (Y) read sees the same snapshot.
    fn trackball_read(&mut self, addr: u32) -> u16 {
        let offset = ((addr >> 1) & 3) as usize;
        let player = (offset >> 1) & 1;
        let which = offset & 1;
        if which == 0 {
            let (x, y) = (
                self.trackball[player * 2].counter(),
                self.trackball[player * 2 + 1].counter(),
            );
            self.trackball_cur[player][0] = x.wrapping_add(y);
            self.trackball_cur[player][1] = x.wrapping_sub(y);
        }
        self.trackball_cur[player][which] as u16
    }
}

impl Bus for MarbleBus<'_> {
    type Address = u32;
    type Data = u16;

    fn is_halted_for(&self, master: BusMaster) -> bool {
        self.board.bus_is_halted_for(master)
    }

    fn observe_data_access(&mut self, master: BusMaster, addr: u32, is_write: bool) {
        self.board.bus_observe_data_access(master, addr, is_write);
    }

    fn read(&mut self, master: BusMaster, addr: u32) -> u16 {
        match addr {
            0xF2_0000..=0xF2_0007 => {
                let v = self.trackball_read(addr);
                self.board.note_read(master, addr, v);
                v
            }
            // Joystick/ADC window is unused by Marble; the port reads 0x00FF.
            0xF4_0000..=0xF4_001F => {
                self.board.note_read(master, addr, 0x00FF);
                0x00FF
            }
            _ => self.board.bus_read(master, addr),
        }
    }

    fn write(&mut self, master: BusMaster, addr: u32, data: u16) {
        self.board.bus_write(master, addr, data);
    }

    fn check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        self.board.bus_check_interrupts(target)
    }
}

// ---------------------------------------------------------------------------
// Capability traits
// ---------------------------------------------------------------------------

// Renderable / AudioSource / MachineDebug delegate to the board (24-bit,
// 16-bit-data bus).
crate::impl_board_delegation!(MarbleSystem, board, atari_system1::TIMING, split_cpu);

impl MachineCore for MarbleSystem {
    crate::machine_core_metadata!("marble", atari_system1::TIMING);

    fn run_frame(&mut self) {
        // Fold this frame's trackball input into the counters the game samples.
        self.update_trackball();

        {
            let (cpu, mut bus) = self.split();
            atari_system1::run_frame(cpu, &mut bus);
        }

        // Watchdog: System 1 reboots after 8 VBLANKs without a strobe to
        // 0x880001. The game kicks it every frame; if it stops, reset.
        if self.board.advance_watchdog() {
            self.reset();
        }

        self.board.end_frame_audio();
    }

    fn reset(&mut self) {
        self.board.reset();
        self.trackball = new_trackball();
        self.trackball_cur = [[0; 2]; 2];

        let (cpu, mut bus) = self.split();
        cpu.reset(&mut bus, BusMaster::Cpu(0));
    }
}

impl InputConfigurable for MarbleSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        MARBLE_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Button { id, pressed } => match id.0 as u8 {
                INPUT_START1 => set_bit_active_low(&mut self.board.f60000_buttons, 0, pressed),
                INPUT_START2 => set_bit_active_low(&mut self.board.f60000_buttons, 1, pressed),
                INPUT_SERVICE => set_bit_active_low(&mut self.board.f60000_buttons, 6, pressed),
                // Coins are read on the sound board's 0x1820 port.
                INPUT_COIN => self.board.sound.set_coin(0, pressed),
                // P1 keyboard trackball roll — held direction keys.
                INPUT_P1_TRACK_LEFT => self.trackball[0].set_held(false, pressed),
                INPUT_P1_TRACK_RIGHT => self.trackball[0].set_held(true, pressed),
                INPUT_P1_TRACK_UP => self.trackball[1].set_held(false, pressed),
                INPUT_P1_TRACK_DOWN => self.trackball[1].set_held(true, pressed),
                _ => {}
            },
            InputEvent::Relative { id, delta } => {
                // Mouse motion → pending sub-counter movement, drained per frame.
                let d = delta as i32;
                if id == CTRL_P1_TRACK_X {
                    self.trackball[0].add_delta(d as f32);
                } else if id == CTRL_P1_TRACK_Y {
                    self.trackball[1].add_delta(d as f32);
                } else if id == CTRL_P2_TRACK_X {
                    self.trackball[2].add_delta(d as f32);
                } else if id == CTRL_P2_TRACK_Y {
                    self.trackball[3].add_delta(d as f32);
                }
            }
            InputEvent::Absolute { .. } => {}
        }
    }

    /// Also clears conditioned analog state: the digital releases above cannot
    /// reach accumulated motion or a held deflection.
    fn release_all_inputs(&mut self) {
        phosphor_core::core::machine::release_all_controls(self);
        for c in &mut self.trackball {
            c.release_all();
        }
    }
}

impl SaveState for MarbleSystem {
    crate::machine_save_state!();
}

impl Saveable for MarbleSystem {
    fn save_state(&self, w: &mut StateWriter) {
        // The CPU first, which is where the board wrote it when it owned it.
        self.cpu.save_state(w);
        self.board.save_state(w);
        for counter in &self.trackball {
            w.write_u8(counter.counter());
        }
        w.write_bytes(&self.trackball_cur[0]);
        w.write_bytes(&self.trackball_cur[1]);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        self.board.load_state(r)?;
        for counter in &mut self.trackball {
            counter.set_counter(r.read_u8()?);
        }
        r.read_bytes_into(&mut self.trackball_cur[0])?;
        r.read_bytes_into(&mut self.trackball_cur[1])?;
        Ok(())
    }
}

// The 2804 EEPROM is the machine's battery-backed store; the frontend persists
// it through the Nvram trait (high scores, config, the boot game-id byte).
impl Nvram for MarbleSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.board.nvram())
    }

    fn load_nvram(&mut self, data: &[u8]) {
        self.board.load_nvram(data);
    }
}

// No sub-span profiling, no event tracing.
impl Profilable for MarbleSystem {}
crate::impl_map_debug_trace!(MarbleSystem, board.map);

// Marble Madness has no operator DIP switches — coinage and game options live
// in the EEPROM and the sound-board config. The all-default trait exposes no banks.
impl phosphor_core::core::machine::DipSwitches for MarbleSystem {}

// ---------------------------------------------------------------------------
// Registry + disassembly
// ---------------------------------------------------------------------------

crate::register_machine!(MarbleSystem, "marble", &["marble"], MARBLE_CONTROLS);

// Disassemblable code regions for the standalone `disasm` tool.
// `main`  — the MC68010 program image (000000-07FFFF, de-interleaved).
// `sound` — the M6502 sound program (8000-FFFF).
inventory::submit! {
    DisasmRegion {
        machine: "marble",
        region: "main",
        cpu: DisasmCpu::M68000,
        org: 0,
        size: 0x80000,
        load: |rs| load_maincpu_image(rs).map(|mut v| { v.truncate(0x80000); v }),
    }
}
inventory::submit! {
    DisasmRegion {
        machine: "marble",
        region: "sound",
        cpu: DisasmCpu::M6502,
        org: 0x8000,
        size: 0x8000,
        load: |rs| MARBLE_SOUND_ROM.load(rs).map(|v| v[0x8000..0x10000].to_vec()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atari_system1::{GfxBank, Region, VBLANK_SCANLINE, build_tile_gfx, irgb4444_to_rgb};
    use phosphor_core::core::machine::Renderable;
    use phosphor_core::cpu::m68000::M68kVariant;
    use phosphor_core::gfx::decode::GfxCache;

    const TIMING: phosphor_core::core::TimingConfig = atari_system1::TIMING;

    #[test]
    fn map_decodes_documented_windows() {
        let sys = MarbleSystem::new();
        let map = &sys.board.map;
        assert_eq!(map.region_at(0x00_0000).unwrap().id, Region::Rom.into());
        // The slapstic window (080000-087FFF) is not a map region — it is decoded
        // in the bus and banked by the slapstic.
        assert!(map.region_at(0x08_0000).is_none());
        assert_eq!(map.region_at(0x40_0000).unwrap().id, Region::Ram.into());
        assert_eq!(map.region_at(0x90_0000).unwrap().id, Region::CartRam.into());
        assert_eq!(
            map.region_at(0xA0_0000).unwrap().id,
            Region::Playfield.into()
        );
        assert_eq!(map.region_at(0xA0_2000).unwrap().id, Region::Mob.into());
        assert_eq!(map.region_at(0xA0_3000).unwrap().id, Region::Alpha.into());
        assert_eq!(map.region_at(0xB0_0000).unwrap().id, Region::Palette.into());
    }

    #[test]
    fn cpu_is_a_68010() {
        let sys = MarbleSystem::new();
        assert_eq!(sys.cpu.variant, M68kVariant::M68010);
    }

    #[test]
    fn reset_loads_ssp_and_pc_from_bios_vectors() {
        let mut sys = MarbleSystem::new();
        // Reset vectors live in the BIOS at 0x000000: SSP = 0x00401000,
        // PC = 0x00000400.
        let rom = sys.board.map.region_data_mut(Region::Rom);
        rom[0..8].copy_from_slice(&[0x00, 0x40, 0x10, 0x00, 0x00, 0x00, 0x04, 0x00]);
        sys.reset();
        let st = sys.get_cpu_state();
        assert_eq!(st.a[7], 0x0040_1000);
        assert_eq!(st.pc, 0x0000_0400);
    }

    #[test]
    fn ram_word_access_round_trips() {
        let mut sys = MarbleSystem::new();
        sys.bus_write(BusMaster::Cpu(0), 0x40_0000, 0xBEEF);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0x40_0000), 0xBEEF);
    }

    #[test]
    fn palette_and_video_ram_round_trip() {
        let mut sys = MarbleSystem::new();
        sys.bus_write(BusMaster::Cpu(0), 0xB0_0000, 0x0ABC);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0xB0_0000), 0x0ABC);
        sys.bus_write(BusMaster::Cpu(0), 0xA0_3000, 0x1234);
        assert_eq!(sys.bus_read(BusMaster::Cpu(0), 0xA0_3000), 0x1234);
    }

    #[test]
    fn control_latches_and_acks() {
        let mut sys = MarbleSystem::new();
        sys.bus_write(BusMaster::Cpu(0), 0x80_0000, 0x0040);
        sys.bus_write(BusMaster::Cpu(0), 0x82_0000, 0x0020);
        sys.bus_write(BusMaster::Cpu(0), 0x86_0000, 0x00AC);
        assert_eq!(sys.board.xscroll, 0x0040);
        assert_eq!(sys.board.yscroll, 0x0020);
        assert_eq!(sys.board.bankselect, 0xAC);

        // VBLANK IRQ4 asserts, then 0x8A0001 acks it.
        sys.board.video_int = true;
        assert_eq!(sys.board.interrupt_level(), 4);
        sys.bus_write(BusMaster::Cpu(0), 0x8A_0000, 0x0000);
        assert!(!sys.board.video_int);
        let st = sys.split().1.check_interrupts(BusMaster::Cpu(0));
        assert_eq!(st.irq_level, 0);
        assert_eq!(st.irq_vector, 0xFF);
    }

    #[test]
    fn watchdog_strobe_resets_count() {
        let mut sys = MarbleSystem::new();
        sys.board.watchdog_count = 5;
        sys.bus_write(BusMaster::Cpu(0), 0x88_0000, 0x0000);
        assert_eq!(sys.board.watchdog_count, 0);
    }

    #[test]
    fn f60000_reports_vblank_and_start_buttons() {
        let mut sys = MarbleSystem::new();
        // Outside VBLANK (clock at 0 → scanline 0): bit 4 set, bit 7 clear.
        assert_eq!(sys.board.read_f60000() & 0x0090, 0x0010);
        // Inside VBLANK: bit 4 clears.
        sys.board.clock = (VBLANK_SCANLINE as u64) * TIMING.cycles_per_scanline;
        assert_eq!(sys.board.read_f60000() & 0x0010, 0x0000);

        // Start1 is active-low bit 0.
        sys.board.clock = 0;
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_START1 as u16),
            pressed: true,
        });
        assert_eq!(sys.board.read_f60000() & 0x0001, 0x0000);
    }

    #[test]
    fn trackball_reads_rotated_counters() {
        let mut sys = MarbleSystem::new();
        // Move P1's trackball: X +10 (reversed → counter −10 = 246), Y +3. The
        // pending motion is drained into the counters once per frame.
        sys.handle_input(InputEvent::Relative {
            id: CTRL_P1_TRACK_X,
            delta: 10.0,
        });
        sys.handle_input(InputEvent::Relative {
            id: CTRL_P1_TRACK_Y,
            delta: 3.0,
        });
        sys.update_trackball();
        assert_eq!(
            sys.trackball.each_ref().map(|c| c.counter()),
            [246, 3, 0, 0]
        );

        // The 45° rotation: X port = x+y, Y port = x-y. The even read latches
        // both, so the odd read sees the same snapshot.
        let xport = sys.split().1.trackball_read(0xF2_0000); // P1 X
        let yport = sys.split().1.trackball_read(0xF2_0002); // P1 Y
        assert_eq!(xport, 246u16.wrapping_add(3) & 0xFF);
        assert_eq!(yport, 246u16.wrapping_sub(3) & 0xFF);

        // P2 ports are independent and idle at zero.
        assert_eq!(sys.split().1.trackball_read(0xF2_0004), 0);
        assert_eq!(sys.split().1.trackball_read(0xF2_0006), 0);
    }

    #[test]
    fn keyboard_rolls_the_trackball() {
        let mut sys = MarbleSystem::new();
        // Holding "roll right" steps the (reversed) X counter each frame.
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_P1_TRACK_RIGHT as u16),
            pressed: true,
        });
        sys.update_trackball();
        assert_eq!(sys.trackball[0].counter(), (256 - TRACK_KEY_STEP) as u8);
        sys.update_trackball();
        assert_eq!(sys.trackball[0].counter(), (256 - 2 * TRACK_KEY_STEP) as u8);
        // Releasing stops the roll.
        sys.handle_input(InputEvent::Button {
            id: InputId(INPUT_P1_TRACK_RIGHT as u16),
            pressed: false,
        });
        let held = sys.trackball[0].counter();
        sys.update_trackball();
        assert_eq!(sys.trackball[0].counter(), held, "no motion once released");
    }

    #[test]
    fn fast_trackball_flick_is_capped_to_avoid_aliasing() {
        let mut sys = MarbleSystem::new();
        // A huge one-frame delta is applied a capped step at a time so the 8-bit
        // counter never jumps more than ±127 (which the game would misread).
        sys.handle_input(InputEvent::Relative {
            id: CTRL_P1_TRACK_Y,
            delta: 500.0,
        });
        sys.update_trackball();
        assert_eq!(sys.trackball[1].counter(), TRACK_MAX_STEP as u8);
        sys.update_trackball();
        assert_eq!(sys.trackball[1].counter(), (2 * TRACK_MAX_STEP) as u8);
    }

    /// Boot a hand-assembled 68010 program on the full board and prove the core
    /// runs it, services the autovectored VBLANK IRQ, and stores into RAM —
    /// exercising the 68010 exception frame end-to-end inside the machine.
    #[test]
    fn synthetic_program_boots_and_takes_vblank_irq() {
        let mut sys = MarbleSystem::new();
        {
            let rom = sys.board.map.region_data_mut(Region::Rom);
            // Reset vectors: SSP = 0x00401000, PC = 0x00000400.
            rom[0x00..0x08].copy_from_slice(&[0x00, 0x40, 0x10, 0x00, 0x00, 0x00, 0x04, 0x00]);
            // VBLANK is IRQ4 → autovector 28 (0x70) → handler at 0x500.
            rom[28 * 4..28 * 4 + 4].copy_from_slice(&[0x00, 0x00, 0x05, 0x00]);

            // Main @ 0x400:
            //   move #$2000, sr        ; supervisor, interrupt mask = 0
            //   loop: addq.l #1, $400000
            //         bra.s loop
            let main: &[u8] = &[
                0x46, 0xFC, 0x20, 0x00, // move #$2000, sr
                0x52, 0xB9, 0x00, 0x40, 0x00, 0x00, // addq.l #1, $00400000
                0x60, 0xF8, // bra.s loop
            ];
            rom[0x400..0x400 + main.len()].copy_from_slice(main);

            // IRQ4 handler @ 0x500:
            //   addq.l #1, $400010     ; bump the interrupt counter
            //   move.b #0, $8A0001     ; ack VBLANK IRQ4
            //   rte
            let handler: &[u8] = &[
                0x52, 0xB9, 0x00, 0x40, 0x00, 0x10, // addq.l #1, $00400010
                0x13, 0xFC, 0x00, 0x00, 0x00, 0x8A, 0x00, 0x01, // move.b #0, $008A0001
                0x4E, 0x73, // rte
            ];
            rom[0x500..0x500 + handler.len()].copy_from_slice(handler);
        }

        sys.reset();
        assert_eq!(sys.get_cpu_state().pc, 0x0000_0400);

        // Three frames stays under the 8-frame watchdog timeout.
        for _ in 0..3 {
            sys.run_frame();
        }

        // CPU is still executing inside the ROM main loop.
        let pc = sys.get_cpu_state().pc;
        assert!(pc < 0x8_0000, "PC {pc:#08X} escaped ROM");

        // The "alive" counter advanced → the core actually ran code.
        let ram = sys.board.map.region_data(Region::Ram);
        let alive = u32::from_be_bytes([ram[0], ram[1], ram[2], ram[3]]);
        assert!(alive > 0, "main loop never ran");

        // The interrupt counter advanced → the autovectored VBLANK IRQ was
        // taken and RTE returned cleanly through the 68010 frame.
        let taken = u32::from_be_bytes([ram[0x10], ram[0x11], ram[0x12], ram[0x13]]);
        assert!(taken > 0, "VBLANK IRQ was never serviced");
    }

    #[test]
    fn disasm_regions_registered() {
        use crate::disasm_registry::{find, regions_for};
        assert_eq!(
            regions_for("marble")
                .iter()
                .map(|r| r.region)
                .collect::<Vec<_>>(),
            vec!["main", "sound"],
        );
        let main = find("marble", "main").expect("main region");
        assert_eq!(main.cpu, DisasmCpu::M68000);
        assert_eq!((main.org, main.size), (0, 0x80000));
        let sound = find("marble", "sound").expect("sound region");
        assert_eq!(sound.cpu, DisasmCpu::M6502);
        assert_eq!((sound.org, sound.size), (0x8000, 0x8000));
    }

    #[test]
    fn irgb4444_decodes_like_hardware() {
        // Black: all nibbles 0.
        assert_eq!(irgb4444_to_rgb(0x0000), (0, 0, 0));
        // Zero intensity forces black regardless of RGB.
        assert_eq!(irgb4444_to_rgb(0x0FFF), (0, 0, 0));
        // Full intensity + full white: (0xFF * 0xFF) >> 8 = 254.
        assert_eq!(irgb4444_to_rgb(0xFFFF), (254, 254, 254));
        // Full intensity, pure red.
        assert_eq!(irgb4444_to_rgb(0xFF00), (254, 0, 0));
        // Half intensity (0x8→0x88), full green: (0x88 * 0xFF) >> 8 = 135.
        assert_eq!(irgb4444_to_rgb(0x80F0), (0, 135, 0));
    }

    #[test]
    fn alpha_layer_composites_palette_through_the_cell() {
        let mut sys = MarbleSystem::new();
        // Palette entry 0 (color 0, pen 0) = IRGB full-intensity red.
        let palette = sys.board.map.region_data_mut(Region::Palette);
        palette[0] = 0xFF;
        palette[1] = 0x00;
        // Top-left alpha cell: code 0, color 0, opaque (bit 13) so pen 0 draws.
        // (The default font cache is all pen 0, so the opaque flag is what makes
        // the cell visible — this exercises the cell decode + palette path.)
        let alpha = sys.board.map.region_data_mut(Region::Alpha);
        alpha[0] = 0x20;
        alpha[1] = 0x00;

        let (w, h) = sys.display_size();
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.render_frame(&mut buf);
        assert_eq!(&buf[0..3], &[254, 0, 0], "opaque pen 0 → palette entry 0");
    }

    #[test]
    fn alpha_transparent_pen0_stays_black() {
        let mut sys = MarbleSystem::new();
        // Paint palette entry 0 red, but leave the alpha cell transparent
        // (no opaque bit): pen 0 must NOT draw — background stays black.
        let palette = sys.board.map.region_data_mut(Region::Palette);
        palette[0] = 0xFF;
        palette[1] = 0x00;
        // Alpha cell defaults to 0 (code 0, color 0, not opaque).

        let (w, h) = sys.display_size();
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.render_frame(&mut buf);
        assert_eq!(&buf[0..3], &[0, 0, 0], "transparent pen 0 shows background");
    }

    #[test]
    fn playfield_prom_lookup_decodes_bank_offset_color() {
        let mut prom = vec![0u8; 0x400];
        // Entry 0 → bank 1 (PROM1 bit4 clear), offset 5; 4bpp (PROM2 bit4 clear),
        // colour 0xA (negative-logic low nibble: ~0xC5 & 0xF = 0xA).
        prom[0x000] = 0xE5;
        prom[0x200] = 0xC5;
        let tiles = vec![0u8; 0x100000];
        let gfx = build_tile_gfx(&prom, &tiles);

        // offset 5 | bank-id 1 (first decoded) | colour 0xA.
        assert_eq!(gfx.lookup[0], 0x0005 | (1 << 8) | (0xA << 12));
        assert_eq!(gfx.banks.len(), 2, "blank placeholder + one decoded bank");
        assert_eq!(gfx.banks[1].bpp, 4);
    }

    #[test]
    fn playfield_prom_selects_bpp_and_remaps_unmapped() {
        let mut prom = vec![0u8; 0x400];
        // Entry 0: 6bpp (PROM2 planes 4+5 enabled), bank 1.
        prom[0x000] = 0xE0; // bank 1
        prom[0x200] = 0xF0; // bit4|bit5 set → 6bpp; low nibble 0 → colour 0
        // Entry 1: no bank bits clear anywhere → unmapped → remapped to bank 1.
        prom[0x001] = 0xF0; // all PROM1 bank bits set
        prom[0x201] = 0xC0; // PROM2 bank bits set, planes off (4bpp)
        let tiles = vec![0u8; 0x100000];
        let gfx = build_tile_gfx(&prom, &tiles);

        assert_eq!(gfx.banks[1].bpp, 6, "plane 4+5 enable → 6bpp");
        // Unmapped entry falls back to bank 1 with offset/colour zeroed.
        assert_eq!(gfx.lookup[1], 1 << 8);
    }

    #[test]
    fn playfield_pixel_composites_below_alpha() {
        let mut sys = MarbleSystem::new();
        // One synthetic 4bpp bank: tile 0, pixel (0,0) = pen 3.
        let mut cache = GfxCache::new(1, 8, 8);
        cache.set_pixel(0, 0, 0, 3);
        sys.board.playfield.banks.push(GfxBank { cache, bpp: 4 });
        // Playfield cell (0,0) → lookup[0]: bank id 1, offset 0, colour 0.
        sys.board.playfield.lookup[0] = 1 << 8;
        // Palette entry 0x203 (= 0x100 gfx base + (0x20<<3) + pen 3, landing in
        // the playfield bank at 0x200) = IRGB pure green.
        let palette = sys.board.map.region_data_mut(Region::Palette);
        palette[0x203 * 2] = 0xF0;
        palette[0x203 * 2 + 1] = 0xF0;

        let (w, h) = sys.display_size();
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.render_frame(&mut buf);
        // Alpha cell 0 is transparent, so the playfield pixel shows through.
        assert_eq!(&buf[0..3], &[0, 254, 0]);
    }

    /// Place one synthetic sprite at screen (0,0) and check it composites over
    /// the playfield in the motion-object palette bank.
    fn sprite_test_system(pen: u8) -> MarbleSystem {
        let mut sys = MarbleSystem::new();
        // Sprite gfx bank 1: tile 0, pixel (0,0) = `pen`.
        let mut cache = GfxCache::new(1, 8, 8);
        cache.set_pixel(0, 0, 0, pen);
        sys.board.playfield.banks.push(GfxBank { cache, bpp: 4 });
        // Colour byte 0 → bank 1, offset 0, palcolor 0.
        sys.board.playfield.mo_lookup[0] = 1 << 8;
        // Active bank 0, entry 0 (split layout): word[0] positions the sprite at
        // screen (0,0) — xpos 0, and ypos_raw 248 cancels the −256 yscroll and
        // the −8 height to land at y=0. words 1-3 stay 0 (colour:code 0, no
        // priority, link 0 → list ends after this entry).
        let mob = sys.board.map.region_data_mut(Region::Mob);
        mob[0] = 0x1F; // word[0] = 0x1F00
        mob[1] = 0x00;
        sys
    }

    #[test]
    fn motion_object_composites_over_playfield() {
        let mut sys = sprite_test_system(5);
        // Sprite pen 5, palcolor 0 → motion bank index 0x105 = pure green.
        let palette = sys.board.map.region_data_mut(Region::Palette);
        palette[0x105 * 2] = 0xF0;
        palette[0x105 * 2 + 1] = 0xF0;

        let (w, h) = sys.display_size();
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.render_frame(&mut buf);
        assert_eq!(
            &buf[0..3],
            &[0, 254, 0],
            "low-priority sprite drawn opaquely"
        );
    }

    #[test]
    fn high_priority_sprite_blends_through_translucent_bank() {
        let mut sys = sprite_test_system(5);
        // Mark the sprite high priority (word[2] bit 15).
        let mob = sys.board.map.region_data_mut(Region::Mob);
        mob[0x100] = 0x80; // word[2] = 0x8000
        // The playfield pen under it is 0 (blank bank), so the blend index is
        // 0x300 + (0<<4) + 5 = 0x305. Colour it pure green.
        let palette = sys.board.map.region_data_mut(Region::Palette);
        palette[0x305 * 2] = 0xF0;
        palette[0x305 * 2 + 1] = 0xF0;

        let (w, h) = sys.display_size();
        let mut buf = vec![0u8; (w * h * 3) as usize];
        sys.render_frame(&mut buf);
        assert_eq!(
            &buf[0..3],
            &[0, 254, 0],
            "high-priority sprite uses the translucent bank"
        );
    }

    #[test]
    fn slip_interrupt_fires_at_timer_scanline() {
        let mut sys = MarbleSystem::new();
        // Active bank 0, entry 0: a timer entry (word[1] = 0xFFFF), word[0]
        // chosen so the interrupt lands on scanline 100:
        // ypos = 256 - (0x1260>>5) - 1*8 - 1 = 256 - 147 - 8 - 1 = 100.
        let mob = sys.board.map.region_data_mut(Region::Mob);
        mob[0] = 0x12; // word[0] = 0x1260 (height 1, Y field 147)
        mob[1] = 0x60;
        mob[0x80] = 0xFF; // word[1] = 0xFFFF marks a timer
        mob[0x81] = 0xFF;

        assert!(
            sys.board.timer_irq_at_scanline(100),
            "fires on the target scanline"
        );
        assert!(
            !sys.board.timer_irq_at_scanline(99),
            "not on adjacent scanlines"
        );
        assert!(!sys.board.timer_irq_at_scanline(101));

        // A non-timer sprite at the same entry must not arm the interrupt.
        let mob = sys.board.map.region_data_mut(Region::Mob);
        mob[0x80] = 0x01; // word[1] no longer 0xFFFF
        mob[0x81] = 0x23;
        assert!(
            !sys.board.timer_irq_at_scanline(100),
            "ordinary sprites don't fire IRQ3"
        );

        // The state bit drives IRQ3 and the 0x2E0000 status read together.
        sys.board.scanline_int = true;
        assert_eq!(sys.board.interrupt_level(), 3);
        assert_eq!(sys.board.int3_state(), 0x0080);
        sys.board.scanline_int = false;
        assert_eq!(sys.board.int3_state(), 0x0000);
    }

    #[test]
    fn slapstic_banks_the_window_through_the_bus() {
        let mut sys = MarbleSystem::new();
        // Distinct marker word at offset 0 of each 8 KB bank.
        for b in 0..4u8 {
            sys.board.slapstic_rom[b as usize * 0x2000] = 0x10 + b;
        }
        // The CPU snoops each data access onto the slapstic via
        // `observe_data_access`, then performs the read; reproduce that pairing.
        let read = |sys: &mut MarbleSystem, a| {
            let mut bus = sys.split().1;
            bus.observe_data_access(BusMaster::Cpu(0), a, false);
            bus.read(BusMaster::Cpu(0), a)
        };

        // Power-on bank is 3; the arming read (offset 0) returns its marker.
        assert_eq!(read(&mut sys, 0x08_0000), 0x1300);
        // Direct-select bank 0 (offset 0x40 → byte 0x80), then read its marker.
        read(&mut sys, 0x08_0080);
        assert_eq!(read(&mut sys, 0x08_0000), 0x1000);
        assert_eq!(sys.board.slapstic.current_bank(), 0);
    }

    #[test]
    fn eeprom_writes_gated_by_unlock_and_relock() {
        let mut sys = MarbleSystem::new();
        let w = |sys: &mut MarbleSystem, a, d| sys.bus_write(BusMaster::Cpu(0), a, d);
        let r = |sys: &mut MarbleSystem, a| sys.bus_read(BusMaster::Cpu(0), a);

        // Locked: the write is dropped (still reads the erased 0xFF).
        w(&mut sys, 0xF0_0000, 0x0042);
        assert_eq!(r(&mut sys, 0xF0_0000), 0x00FF);

        // Unlock (8C0001), then one write sticks...
        w(&mut sys, 0x8C_0001, 0x0001);
        w(&mut sys, 0xF0_0000, 0x0042);
        assert_eq!(r(&mut sys, 0xF0_0000), 0x0042);

        // ...and the 2804 re-locked, so the next write without an unlock is dropped.
        w(&mut sys, 0xF0_0002, 0x0099);
        assert_eq!(r(&mut sys, 0xF0_0002), 0x00FF);
        assert!(!sys.board.eeprom_unlocked);
    }

    #[test]
    fn nvram_exposes_the_eeprom() {
        let mut sys = MarbleSystem::new();
        sys.board.eeprom[0x6E] = 0x42; // the boot game-id byte lives around here
        assert_eq!(Nvram::save_nvram(&sys).unwrap()[0x6E], 0x42);

        let mut sys2 = MarbleSystem::new();
        let snapshot = Nvram::save_nvram(&sys).unwrap().to_vec();
        Nvram::load_nvram(&mut sys2, &snapshot);
        assert_eq!(sys2.board.eeprom[0x6E], 0x42);
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = MarbleSystem::new();
        sys.board.map.region_data_mut(Region::Ram)[0x100] = 0xAB;
        sys.board.map.region_data_mut(Region::Playfield)[0x10] = 0xCD;
        sys.board.map.region_data_mut(Region::Alpha)[0x20] = 0xEF;
        sys.board.eeprom[0x30] = 0x99;
        // Drive the slapstic to a non-default bank so its state is exercised.
        // Bank 1's select offset is 0x50 (word) → byte address 0x0800A0. The
        // CPU snoops accesses onto the chip via `observe_data_access`.
        sys.split()
            .1
            .observe_data_access(BusMaster::Cpu(0), 0x08_0000, false); // arm
        sys.split()
            .1
            .observe_data_access(BusMaster::Cpu(0), 0x08_00A0, false); // select bank 1
        assert_eq!(sys.board.slapstic.current_bank(), 1);
        sys.board.xscroll = 0x1234;
        sys.board.bankselect = 0x5A;
        sys.board.video_int = true;
        sys.board.clock = 99_999;
        sys.board.watchdog_count = 4;

        let data = SaveState::save_state(&sys).expect("save");
        let cpu_snap = sys.get_cpu_state();

        let mut sys2 = MarbleSystem::new();
        SaveState::load_state(&mut sys2, &data).unwrap();

        assert_eq!(sys2.get_cpu_state(), cpu_snap);
        assert_eq!(sys2.board.map.region_data(Region::Ram)[0x100], 0xAB);
        assert_eq!(sys2.board.map.region_data(Region::Playfield)[0x10], 0xCD);
        assert_eq!(sys2.board.map.region_data(Region::Alpha)[0x20], 0xEF);
        assert_eq!(sys2.board.eeprom[0x30], 0x99);
        assert_eq!(sys2.board.slapstic.current_bank(), 1);
        assert_eq!(sys2.board.xscroll, 0x1234);
        assert_eq!(sys2.board.bankselect, 0x5A);
        assert!(sys2.board.video_int);
        assert_eq!(sys2.board.clock, 99_999);
        assert_eq!(sys2.board.watchdog_count, 4);
    }
}
