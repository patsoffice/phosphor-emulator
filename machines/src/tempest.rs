use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::input::{DrainPolicy, RelativeCounter};
use phosphor_core::core::machine::{
    ActionRole, AnalogAxisKind, AudioSource, DefaultBinding, DipApplyTiming, DipChoice, DipOption,
    DipSwitchBank, DipSwitches, FrontendMachine, InputConfigurable, InputControl, InputEvent,
    InputId, InputKind, MachineCore, MouseControl, Nvram, Profilable, SaveState,
};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::device::Er2055;
use phosphor_core::device::Mathbox;
use phosphor_core::device::pokey::Pokey;
use phosphor_macros::Saveable;

use crate::atari_avg::{self, AtariAvgBoard, Region};
use crate::registry::MachineEntry;
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_low;

// ---------------------------------------------------------------------------
// ROM definitions (Tempest rev 3, Revised Hardware)
// ---------------------------------------------------------------------------

/// Program ROM: 20KB at CPU addresses $9000–$DFFF, mirrored at $F000–$FFFF.
static PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x5000,
    entries: &[
        RomEntry {
            name: "136002-133.d1",
            size: 0x1000,
            offset: 0x0000,
            crc32: &[0x1d0cc503],
        },
        RomEntry {
            name: "136002-134.f1",
            size: 0x1000,
            offset: 0x1000,
            crc32: &[0xc88e3524],
        },
        RomEntry {
            name: "136002-235.j1",
            size: 0x1000,
            offset: 0x2000,
            crc32: &[0xa4b2ce3f],
        },
        RomEntry {
            name: "136002-136.lm1",
            size: 0x1000,
            offset: 0x3000,
            crc32: &[0x65a9a9f9],
        },
        RomEntry {
            name: "136002-237.p1",
            size: 0x1000,
            offset: 0x4000,
            crc32: &[0xde4e9e34],
        },
    ],
};

/// Vector ROM: 4KB at AVG address $1000–$1FFF (CPU $3000–$3FFF).
static VECTOR_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "136002-138.np3",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0x9995256d],
    }],
};

// ---------------------------------------------------------------------------
// Input button IDs
// ---------------------------------------------------------------------------

pub const INPUT_COIN1: u8 = 0;
pub const INPUT_COIN2: u8 = 1;
pub const INPUT_COIN3: u8 = 2;
pub const INPUT_FIRE: u8 = 3;
pub const INPUT_ZAP: u8 = 4;
pub const INPUT_START1: u8 = 5;
pub const INPUT_START2: u8 = 6;
pub const INPUT_LEFT: u8 = 7;
pub const INPUT_RIGHT: u8 = 8;

// Typed control id for the spinner (distinct from the 0..=8 digital ids).
const CTRL_SPINNER: InputId = InputId(9);

/// Typed logical controls. `InputId`s reuse the `INPUT_*` numbering for digital
/// controls; default bindings mirror the legacy name-matched defaults. The
/// spinner maps to the mouse X axis; Fire is the primary action (gamepad A),
/// so the superzapper is key-only.
const TEMPEST_CONTROLS: &[InputControl] = &[
    InputControl {
        id: InputId(INPUT_COIN1 as u16),
        stable_name: "coin1",
        label: "Coin",
        kind: InputKind::Coin,
        player: None,
        default_bindings: crate::input_defaults::COIN,
    },
    InputControl {
        id: InputId(INPUT_COIN2 as u16),
        stable_name: "coin2",
        label: "Coin 2",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_COIN3 as u16),
        stable_name: "coin3",
        label: "Coin 3",
        kind: InputKind::Coin,
        player: None,
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_FIRE as u16),
        stable_name: "fire",
        label: "Fire",
        kind: InputKind::Action(ActionRole::Primary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_ZAP as u16),
        stable_name: "superzapper",
        label: "Superzapper",
        kind: InputKind::Action(ActionRole::Secondary),
        player: Some(1),
        default_bindings: &[],
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
        id: InputId(INPUT_LEFT as u16),
        stable_name: "rotate_left",
        label: "Rotate Left",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(INPUT_RIGHT as u16),
        stable_name: "rotate_right",
        label: "Rotate Right",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
    InputControl {
        id: CTRL_SPINNER,
        stable_name: "spinner",
        label: "Spinner",
        kind: InputKind::AnalogAxis {
            axis: AnalogAxisKind::X,
        },
        player: Some(1),
        default_bindings: &[DefaultBinding::Mouse(MouseControl::AxisX)],
    },
];

// ---------------------------------------------------------------------------
// TempestSystem — Atari AVG board configured for Tempest (1981)
// ---------------------------------------------------------------------------

/// Tempest arcade game wrapper around the shared Atari AVG board.
///
/// Hardware: MOS 6502 @ 1.512 MHz, Atari AVG (Tempest variant) color vector
/// display, Mathbox coprocessor, dual POKEY sound chips, ER2055 EAROM.
///
/// Memory map (16-bit address bus):
///   $0000–$07FF  RAM (2 KB)
///   $0800–$080F  Color RAM (16 bytes, write-only)
///   $0C00        IN0 read (coins, tilt, test, VG halt, 3KHz clock)
///   $0D00        DSW1 read (pricing options)
///   $0E00        DSW2 read (game options)
///   $2000–$2FFF  Vector RAM (4 KB)
///   $3000–$3FFF  Vector ROM (4 KB)
///   $4000        Coin counters + video invert X/Y
///   $4800        AVG GO
///   $5000        Watchdog clear + IRQ acknowledge
///   $5800        AVG reset
///   $6000–$603F  EAROM write
///   $6040        R: Mathbox status; W: EAROM control
///   $6050        R: EAROM read
///   $6060        R: Mathbox result low
///   $6070        R: Mathbox result high
///   $6080–$609F  W: Mathbox command
///   $60C0–$60CF  R/W: POKEY 1 (sound + spinner input)
///   $60D0–$60DF  R/W: POKEY 2 (sound + button input)
///   $60E0        W: LED control + FLIP (player select)
///   $9000–$DFFF  Program ROM (20 KB)
///   $F000–$FFFF  Program ROM mirror (for vectors)
#[derive(Saveable)]
#[save_version(2)]
pub struct TempestSystem {
    pub board: AtariAvgBoard,

    // Mathbox coprocessor
    mathbox: Mathbox,

    // Dual POKEY sound chips
    pokey1: Pokey,
    pokey2: Pokey,

    // EAROM for high scores
    earom: Er2055,

    // IN0: coins (active-LOW bits 0–2), tilt (3), test (4), diag (5)
    // Bits 6 (VG halt) and 7 (3KHz clock) are generated dynamically.
    in0: u8,

    // POKEY1 input: spinner (bits 0–3) + cabinet (bit 4)
    in1: u8,

    // POKEY2 input: DIP switches (bits 0–2), buttons (bits 3–4),
    //               start buttons (bits 5–6, active-LOW)
    in2: u8,

    // DIP switches
    #[save_skip]
    dsw1: u8,
    #[save_skip]
    dsw2: u8,

    // Player select (FLIP bit from $60E0 write)
    player_select: bool,

    // LS259 output latch at $4000-$400F (bit 2 = flip_x, bit 3 = flip_y)
    outlatch: u8,

    // Spinner accumulator (from set_analog or keyboard, drained into 4-bit counter)
    /// Spinner conditioning: relative motion and the left/right keys feed a
    /// wrapping 4-bit counter the game samples as a signed per-frame delta.
    spinner: RelativeCounter,

    // Audio buffer from dual POKEYs
    #[save_skip(default)]
    audio_buffer: Vec<i16>,
}

/// The spinner counter is read as a signed 4-bit per-frame delta, so a step
/// beyond +-7 aliases into a stall (multiples of 16) or a reversal (8..15).
/// Trackball motion can produce far larger deltas, so the step is clamped and
/// the excess dropped rather than carried — otherwise the player keeps drifting
/// after the pointing device stops. Held keys contribute a fixed 3 per frame,
/// full spinner speed.
fn new_spinner() -> RelativeCounter {
    RelativeCounter::new(0x0F, 3, false, DrainPolicy::ClampDrop { max_step: 7 })
}

impl TempestSystem {
    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(Region::Ram, "RAM", 0x0000, 0x0800, AccessKind::ReadWrite)
            .region(
                Region::ColorRam,
                "Color RAM",
                0x0800,
                0x0100,
                AccessKind::ReadWrite,
            )
            .region(Region::Io, "I/O", 0x0C00, 0xF400, AccessKind::Io)
            .region(
                Region::VectorRam,
                "Vector RAM",
                0x2000,
                0x1000,
                AccessKind::ReadWrite,
            )
            .region(
                Region::VectorRom,
                "Vector ROM",
                0x3000,
                0x1000,
                AccessKind::ReadOnly,
            )
            .region(
                Region::ProgramRom,
                "Program ROM",
                0x9000,
                0x5000,
                AccessKind::ReadOnly,
            );
        map
    }

    pub fn new() -> Self {
        Self {
            board: AtariAvgBoard::new(Self::build_map(), 580, 570),
            mathbox: Mathbox::new(),
            pokey1: Pokey::with_clock(1_512_000, 44100),
            pokey2: Pokey::with_clock(1_512_000, 44100),
            earom: Er2055::new(),
            in0: 0xFF,  // coins/tilt/test all active-LOW, default released = all 1s
            in1: 0xF0,  // spinner bits 0-3 = 0, bit 4 = cabinet upright, bits 5-7 unused (1)
            in2: 0xFF,  // difficulty=medium (0x03), rating (0x04), buttons released (0xF8)
            dsw1: 0x00, // 1C/1C, right *1, left *1, no bonus
            dsw2: 0x00, // 1 credit min, English, 20K bonus, 3 lives
            player_select: false,
            outlatch: 0,
            spinner: new_spinner(),
            audio_buffer: Vec::with_capacity(2048),
        }
    }

    fn debug_pre_tick(&mut self) {
        self.pokey1.tick();
        self.pokey2.tick();
    }

    /// Update POKEY pot inputs based on current input state.
    ///
    /// Each IN1/IN2 bit maps to a POKEY pot: 0 if set (fires immediately), 228 if clear.
    fn update_pot_inputs(&mut self) {
        self.spinner.update();

        // Build IN1: spinner bits 0-3 + cabinet bit 4
        self.in1 = (self.in1 & 0xF0) | self.spinner.counter();

        // POKEY1: each POT reads one bit from IN1
        for i in 0..8u8 {
            let val = if self.in1 & (1 << i) != 0 { 0 } else { 228 };
            self.pokey1.set_pot_input(i as usize, val);
        }

        // POKEY2: each POT reads one bit from IN2
        for i in 0..8u8 {
            let val = if self.in2 & (1 << i) != 0 { 0 } else { 228 };
            self.pokey2.set_pot_input(i as usize, val);
        }
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let prog = PROGRAM_ROM.load(rom_set)?;
        self.board.map.load_region(Region::ProgramRom, &prog);
        let vrom = VECTOR_ROM.load(rom_set)?;
        self.board.map.load_region(Region::VectorRom, &vrom);
        Ok(())
    }
}

impl Default for TempestSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus implementation
// ---------------------------------------------------------------------------

impl Bus for TempestSystem {
    type Address = u16;
    type Data = u8;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let data = match self.board.map.page(addr).region_id {
            Region::RAM
            | Region::COLOR_RAM
            | Region::VECTOR_RAM
            | Region::VECTOR_ROM
            | Region::PROGRAM_ROM => self.board.map.read_backing(addr),

            Region::IO => match addr {
                // IN0: coins, tilt, test, diagnostic, VG halt, 3KHz clock
                0x0C00 => {
                    let mut val = self.in0 & 0x3F; // bits 0-5 from latched input
                    // Bit 6: VG halt (active HIGH = AVG is done/halted)
                    if self.board.avg.is_halted() {
                        val |= 0x40;
                    }
                    // Bit 7: 3KHz clock (total_cycles & 0x100)
                    if self.board.clock & 0x100 != 0 {
                        val |= 0x80;
                    }
                    val
                }

                // DSW1 (pricing options)
                0x0D00 => self.dsw1,

                // DSW2 (game options)
                0x0E00 => self.dsw2,

                // Mathbox status (always ready)
                0x6040 => self.mathbox.status_r(),

                // EAROM read (at previously latched address)
                0x6050 => self.earom.read_latched(),

                // Mathbox result low
                0x6060 => self.mathbox.lo_r(),

                // Mathbox result high
                0x6070 => self.mathbox.hi_r(),

                // POKEY 1
                0x60C0..=0x60CF => self.pokey1.read(addr & 0x0F),

                // POKEY 2
                0x60D0..=0x60DF => self.pokey2.read(addr & 0x0F),

                // $F000–$FFFF: mirror of program ROM $D000–$DFFF (for vectors)
                0xF000..=0xFFFF => {
                    let mirror_addr = addr - 0xF000 + 0xD000;
                    self.board.map.read_backing(mirror_addr)
                }

                _ => 0,
            },

            _ => 0,
        };

        self.board.map.watch_read(0, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.board.map.watch_write(0, master, addr, data);
        self.board.trace_main_write(addr, data);

        match self.board.map.page(addr).region_id {
            Region::RAM | Region::COLOR_RAM | Region::VECTOR_RAM => {
                self.board.map.write_backing(addr, data)
            }

            Region::IO => match addr {
                // Color RAM ($0800–$080F)
                0x0800..=0x080F => self.board.map.write_backing(addr, data),

                // LS259 output latch (coin counters, flip, LEDs)
                // A0-A2 select bit, D0 provides value
                0x4000..=0x400F => {
                    let bit = (addr & 7) as u8;
                    if data & 1 != 0 {
                        self.outlatch |= 1 << bit;
                    } else {
                        self.outlatch &= !(1 << bit);
                    }
                    // Bit 2 = flip_x, bit 3 = flip_y
                    self.board
                        .avg
                        .set_flip(self.outlatch & 0x04 != 0, self.outlatch & 0x08 != 0);
                }

                // AVG GO
                0x4800 => self.board.trigger_avg(),

                // Watchdog clear + IRQ acknowledge
                0x5000 => {
                    self.board.watchdog_frame_count = 0;
                    self.board.irq_pending = false;
                }

                // AVG reset
                0x5800 => self.board.avg.reset(),

                // EAROM write ($6000–$603F)
                0x6000..=0x603F => self.earom.latch(addr & 0x3F, data),

                // EAROM control
                0x6040 => {
                    let clock = data & 0x01 != 0;
                    let c1 = data & 0x04 == 0; // bit 2 inverted → C1
                    let c2 = data & 0x02 != 0; // bit 1 → C2
                    let cs1 = data & 0x08 != 0;
                    self.earom.write_control(clock, cs1, c1, c2);
                }

                // Mathbox command ($6080–$609F)
                0x6080..=0x609F => self.mathbox.go_w((addr & 0x1F) as u8, data),

                // POKEY 1
                0x60C0..=0x60CF => self.pokey1.write(addr & 0x0F, data),

                // POKEY 2
                0x60D0..=0x60DF => self.pokey2.write(addr & 0x0F, data),

                // LED control + FLIP
                0x60E0 => {
                    self.player_select = data & 0x04 != 0;
                }

                _ => {}
            },

            _ => {}
        }
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            nmi: false,
            irq: self.board.irq_pending,
            firq: false,
            irq_vector: 0,
            irq_level: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Machine traits (MachineCore + capabilities)
// ---------------------------------------------------------------------------

impl phosphor_core::core::machine::Renderable for TempestSystem {
    fn display_size(&self) -> (u32, u32) {
        atari_avg::TIMING.display_size()
    }

    fn display_aspect(&self) -> Option<(u32, u32)> {
        atari_avg::TIMING.display_aspect()
    }
    fn render_frame(&self, buffer: &mut [u8]) {
        self.board.render_frame(buffer);
    }
    fn vector_display_list(&self) -> Option<&[phosphor_core::device::dvg::VectorLine]> {
        self.board.vector_display_list()
    }
    fn orientation(&self) -> phosphor_core::core::machine::Orientation {
        phosphor_core::core::machine::Orientation::ROT270
    }
}

impl AudioSource for TempestSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        let n = buffer.len().min(self.audio_buffer.len());
        buffer[..n].copy_from_slice(&self.audio_buffer[..n]);
        self.audio_buffer.drain(..n);
        n
    }

    fn audio_sample_rate(&self) -> u32 {
        44100
    }
}

impl InputConfigurable for TempestSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        TEMPEST_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        match event {
            InputEvent::Button { id, pressed } => match id.0 as u8 {
                // IN0: coins and tilt (active-LOW)
                INPUT_COIN1 => set_bit_active_low(&mut self.in0, 2, pressed),
                INPUT_COIN2 => set_bit_active_low(&mut self.in0, 1, pressed),
                INPUT_COIN3 => set_bit_active_low(&mut self.in0, 0, pressed),

                // IN2 bits 3-4: fire/zap buttons (active-LOW in buttons port)
                // Button port bit 1 (fire) → IN2 bit 4
                // Button port bit 0 (zap) → IN2 bit 3
                INPUT_FIRE => set_bit_active_low(&mut self.in2, 4, pressed),
                INPUT_ZAP => set_bit_active_low(&mut self.in2, 3, pressed),

                // IN2 bits 5-6: start buttons (active-LOW)
                INPUT_START1 => set_bit_active_low(&mut self.in2, 5, pressed),
                INPUT_START2 => set_bit_active_low(&mut self.in2, 6, pressed),

                // Digital spinner via left/right keys
                INPUT_LEFT => self.spinner.set_held(false, pressed),
                INPUT_RIGHT => self.spinner.set_held(true, pressed),

                _ => {}
            },
            InputEvent::Relative { id, delta } => {
                if id == CTRL_SPINNER {
                    self.spinner.add_delta(delta);
                }
            }
            InputEvent::Absolute { .. } => {}
        }
    }

    /// Also clears conditioned analog state: the digital releases above cannot
    /// reach accumulated motion or a held deflection.
    fn release_all_inputs(&mut self) {
        phosphor_core::core::machine::release_all_controls(self);
        self.spinner.release_all();
    }
}

crate::impl_board_debug!(TempestSystem, board, atari_avg::TIMING, debug_tick_pre);

impl MachineCore for TempestSystem {
    crate::machine_core_metadata!("tempest", atari_avg::TIMING);

    fn run_frame(&mut self) {
        self.update_pot_inputs();

        bus_split!(self, bus => {
            for _ in 0..atari_avg::TIMING.cycles_per_frame() {
                self.pokey1.tick();
                self.pokey2.tick();
                self.board.tick(bus);
            }
        });

        // Mix dual POKEY audio
        let samples1 = self.pokey1.drain_audio();
        let samples2 = self.pokey2.drain_audio();
        let len = samples1.len().min(samples2.len());
        for i in 0..len {
            let mixed = (samples1[i] + samples2[i]) * 0.5;
            self.audio_buffer.push((mixed * 32767.0) as i16);
        }

        // Watchdog
        self.board.watchdog_frame_count += 1;
        if self.board.watchdog_frame_count >= 8 {
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.board.reset();
        self.mathbox.reset();
        self.pokey1.reset();
        self.pokey2.reset();
        self.earom.reset();
        self.player_select = false;
        self.outlatch = 0;
        self.spinner = new_spinner();
        bus_split!(self, bus => {
            self.board.cpu.reset(bus, BusMaster::Cpu(0));
        });
    }
}

impl SaveState for TempestSystem {
    crate::machine_save_state!();
}

impl Nvram for TempestSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.earom.snapshot())
    }

    fn load_nvram(&mut self, data: &[u8]) {
        self.earom.load_from(data);
    }
}

impl Profilable for TempestSystem {}
/// DIP switch metadata for Tempest's two banks: DSW1 (pricing, read at $0D00,
/// byte `dsw1`) and DSW2 (game options, read at $0E00, byte `dsw2`). Choice
/// bits and labels follow MAME's `tempest` layout; option defaults OR to the
/// historical 0x00 for both banks.
const TEMPEST_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "DSW1 (Pricing)",
        options: &[
            DipOption {
                name: "Coinage",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "1 Coin/1 Credit",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "2 Coins/1 Credit",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "Free Play",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "1 Coin/2 Credits",
                        value: 0x03,
                    },
                ],
            },
            DipOption {
                name: "Right Coin",
                mask: 0x0C,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "x1",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "x4",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "x5",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "x6",
                        value: 0x0C,
                    },
                ],
            },
            DipOption {
                name: "Left Coin",
                mask: 0x10,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "x1",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "x2",
                        value: 0x10,
                    },
                ],
            },
            DipOption {
                name: "Bonus Coins",
                mask: 0xE0,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "None",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "1 each 2",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "1 each 4 (+demo)",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "2 each 4 (+demo)",
                        value: 0x60,
                    },
                    DipChoice {
                        label: "1 each 5",
                        value: 0x80,
                    },
                    DipChoice {
                        label: "1 each 3",
                        value: 0xA0,
                    },
                    DipChoice {
                        label: "Freeze Mode",
                        value: 0xC0,
                    },
                    DipChoice {
                        label: "Freeze Mode",
                        value: 0xE0,
                    },
                ],
            },
        ],
    },
    DipSwitchBank {
        name: "DSW2 (Game)",
        options: &[
            DipOption {
                name: "Minimum",
                mask: 0x01,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "1 Credit",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "2 Credits",
                        value: 0x01,
                    },
                ],
            },
            DipOption {
                name: "Language",
                mask: 0x06,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "English",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "French",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "German",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "Spanish",
                        value: 0x06,
                    },
                ],
            },
            DipOption {
                name: "Bonus Life",
                mask: 0x38,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "20000",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "10000",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "30000",
                        value: 0x10,
                    },
                    DipChoice {
                        label: "40000",
                        value: 0x18,
                    },
                    DipChoice {
                        label: "50000",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "60000",
                        value: 0x28,
                    },
                    DipChoice {
                        label: "70000",
                        value: 0x30,
                    },
                    DipChoice {
                        label: "None",
                        value: 0x38,
                    },
                ],
            },
            DipOption {
                name: "Lives",
                mask: 0xC0,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "3",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "4",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "5",
                        value: 0x80,
                    },
                    DipChoice {
                        label: "2",
                        value: 0xC0,
                    },
                ],
            },
        ],
    },
];

impl DipSwitches for TempestSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        TEMPEST_DIP_BANKS
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        match bank {
            0 => self.dsw1,
            1 => self.dsw2,
            _ => 0,
        }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        match bank {
            0 => self.dsw1 = value,
            1 => self.dsw2 = value,
            _ => {}
        }
    }
}
crate::impl_board_debug_trace!(TempestSystem, board);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

fn create_machine(rom_set: &RomSet) -> Result<Box<dyn FrontendMachine>, RomLoadError> {
    let mut sys = TempestSystem::new();
    sys.load_rom_set(rom_set)?;
    Ok(Box::new(sys))
}

inventory::submit! {
MachineEntry::new("tempest", &["tempest"], create_machine, TEMPEST_CONTROLS) }

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::cpu::CpuStateTrait;

    #[test]
    fn dip_defaults_and_metadata() {
        use phosphor_core::core::machine::Renderable;
        let sys = TempestSystem::new();
        assert_eq!(sys.dip_bank_value(0), 0x00);
        assert_eq!(sys.dip_bank_value(1), 0x00);
        crate::assert_dip_banks_valid(sys.dip_banks(), &[0x00, 0x00]);
        // Rotated (ROT270) vector cabinet: 4:3 tube presented portrait as 3:4.
        assert_eq!(sys.display_aspect(), Some((3, 4)));
    }

    #[test]
    fn declares_rot270_orientation() {
        use phosphor_core::core::machine::{Orientation, Renderable};
        // Tempest's portrait vector monitor is declared, not baked: the frontend
        // rotates the AVG output via the GL shader driven by these flags.
        let sys = TempestSystem::new();
        assert_eq!(sys.orientation(), Orientation::ROT270);
        assert!(sys.orientation().swaps_axes());
    }

    #[test]
    fn set_dip_option_masks_only_its_bits() {
        let mut sys = TempestSystem::new();
        // DSW2 Lives is bank 1, option 3 (mask 0xC0); pick "5" (0x80).
        sys.set_dip_option(1, 3, 0x80);
        assert_eq!(sys.dip_bank_value(1), 0x80);
        assert_eq!(sys.dip_bank_value(0), 0x00); // pricing bank untouched
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = TempestSystem::new();

        // Set known state
        sys.board.map.region_data_mut(Region::Ram)[0x100] = 0xAA;
        sys.board.map.region_data_mut(Region::VectorRam)[0x200] = 0xBB;
        sys.board.map.region_data_mut(Region::ColorRam)[0] = 0x05;
        sys.in0 = 0xFB;
        sys.in2 = 0x57;
        sys.board.clock = 75_000;
        sys.board.irq_counter = 3000;
        sys.board.irq_pending = true;
        sys.board.watchdog_frame_count = 5;
        sys.player_select = true;
        sys.spinner.set_counter(7);
        sys.earom.load_from(&{
            let mut d = [0u8; 64];
            d[0] = 0x42;
            d[63] = 0xEF;
            d
        });

        // Save
        let data = sys.save_state().expect("save_state should return Some");
        let cpu_snap = sys.board.cpu.snapshot();

        // Mutate everything
        let mut sys2 = TempestSystem::new();
        sys2.board.map.region_data_mut(Region::Ram)[0x100] = 0xFF;
        sys2.in0 = 0x00;
        sys2.board.clock = 999;

        // Load
        sys2.load_state(&data).unwrap();

        // Verify CPU
        assert_eq!(sys2.board.cpu.snapshot(), cpu_snap);

        // Verify memory
        assert_eq!(sys2.board.map.region_data(Region::Ram)[0x100], 0xAA);
        assert_eq!(sys2.board.map.region_data(Region::VectorRam)[0x200], 0xBB);
        assert_eq!(sys2.board.map.region_data(Region::ColorRam)[0], 0x05);

        // Verify I/O and timing state
        assert_eq!(sys2.in0, 0xFB);
        assert_eq!(sys2.in2, 0x57);
        assert_eq!(sys2.board.clock, 75_000);
        assert_eq!(sys2.board.irq_counter, 3000);
        assert!(sys2.board.irq_pending);
        assert_eq!(sys2.board.watchdog_frame_count, 5);
        assert!(sys2.player_select);
        assert_eq!(sys2.spinner.counter(), 7);

        // Verify EAROM
        assert_eq!(sys2.earom.read(0), 0x42);
        assert_eq!(sys2.earom.read(63), 0xEF);
    }

    #[test]
    fn save_does_not_include_rom() {
        let mut sys = TempestSystem::new();
        sys.board.map.region_data_mut(Region::ProgramRom)[0] = 0xDE;
        sys.board.map.region_data_mut(Region::VectorRom)[0] = 0xAD;

        let data = sys.save_state().unwrap();

        let mut sys2 = TempestSystem::new();
        sys2.load_state(&data).unwrap();

        assert_eq!(sys2.board.map.region_data(Region::ProgramRom)[0], 0x00);
        assert_eq!(sys2.board.map.region_data(Region::VectorRom)[0], 0x00);
    }

    #[test]
    fn mathbox_accessible() {
        let mut sys = TempestSystem::new();

        // Write mathbox register 0 low byte
        sys.write(BusMaster::Cpu(0), 0x6080, 0x42);
        // Read result low
        let lo = sys.read(BusMaster::Cpu(0), 0x6060);
        assert_eq!(lo, 0x42);
    }

    #[test]
    fn earom_write_read() {
        let mut sys = TempestSystem::new();

        // Latch address 0x05 with data 0xAB
        sys.write(BusMaster::Cpu(0), 0x6005, 0xAB);

        // Tempest $6040 bits: 0=CK, 1=C2, 2=!C1, 3=CS1

        // Erase address 5: C1=0(bit2=1), C2=1(bit1=1), CS1=1(bit3=1)
        sys.write(BusMaster::Cpu(0), 0x6040, 0x0F); // clock high
        sys.write(BusMaster::Cpu(0), 0x6040, 0x0E); // clock low

        // Write 0xAB: C1=0(bit2=1), C2=0(bit1=0), CS1=1(bit3=1)
        sys.write(BusMaster::Cpu(0), 0x6040, 0x0D); // clock high
        sys.write(BusMaster::Cpu(0), 0x6040, 0x0C); // clock low

        // Read: C1=1(bit2=0), CS1=1(bit3=1), falling edge loads data register
        sys.write(BusMaster::Cpu(0), 0x6040, 0x09); // clock high
        sys.write(BusMaster::Cpu(0), 0x6040, 0x08); // falling edge → read

        // Read data register via EAROM read port
        let val = sys.read(BusMaster::Cpu(0), 0x6050);
        assert_eq!(val, 0xAB);
    }
}
