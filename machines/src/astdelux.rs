//! Atari Asteroids Deluxe (1980).
//!
//! # Documentation
//!
//! | Document | Source | Pages |
//! |---|---|---|
//! | `Asteroids Deluxe` operator manual TM-143, 1st printing | `arcade-museum.com/manuals-videogames/A/AsteroidsDeluxe.man.pdf` | 46 pages; see the figure list below |
//! | `Asteroids Deluxe Cabaret` drawing package supplement | `arcade-museum.com/manuals-videogames/A/AstDlx-Cabaret-sp.pdf` | 8 sheets |
//!
//! **The manual's printed page numbers run four behind its PDF pages**, so
//! Figure 8 is printed page 11 and PDF page 15. PDF pages are used throughout
//! here. The figures that describe this board's operator switches:
//!
//! | Figure | PDF page | What it gives |
//! |---|---|---|
//! | 6, `Self-Test Procedure` | 11-12 | The four decoded fields behind each digit of the self-test's option display; the naming authority for both switch banks |
//! | 7, `Game Option Settings` | 13 | R5's eight toggles, one option per row, factory settings marked |
//! | 8, `Game Price Settings` | 15 | L8's eight toggles as twelve complete recipes, cabinet/door type against bonus scheme |
//! | 9, `Coin Counter Option Settings` | 17 | **Not L8.** A separate 4-toggle switch at M12 wiring coin mechanisms to counters; it never reaches the CPU |
//!
//! Figure 6 is the one that makes the L8 table tractable: Figure 8 gives whole
//! eight-toggle recipes and never says which toggle carries which meaning, so on
//! its own it would only support modeling the bank as opaque byte recipes. See
//! [`ASTDELUX_L8_DERIVATION`].
//!
//! The drawing package is a different PDF and does **not** show L8 anywhere. Its
//! Sheet 2 Side B (PDF p7) has `OPTIONS INPUT CIRCUITRY`, which is R5: eight
//! switches shorting to ground against 10k pull-ups into a P5 LS253, so a closed
//! R5 toggle reads low. That sense does not transfer to L8, which is wired to the
//! POKEY pot lines instead; see
//! [`refresh_dip_pots`](AsteroidsDeluxeSystem::refresh_dip_pots).

use phosphor_core::audio::SampleRing;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::machine::{
    ActionRole, AudioSource, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, DipSwitches,
    InputConfigurable, InputControl, InputEvent, InputId, InputKind, MachineCore, Nvram,
    Profilable, SaveState,
};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::device::Er2055;
use phosphor_core::device::pokey::Pokey;
use phosphor_macros::Saveable;

use crate::atari_dvg::{self, AtariDvgBoard, AtariDvgBus, Region};
use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};
use crate::set_bit_active_high;
use phosphor_core::cpu::m6502::M6502;

// ---------------------------------------------------------------------------
// ROM definitions (MAME `astdelux` set, revision 3)
// ---------------------------------------------------------------------------

/// Program ROM: 8KB at CPU addresses 0x6000–0x7FFF.
static PROGRAM_ROM: RomRegion = RomRegion {
    size: 0x2000,
    entries: &[
        RomEntry {
            name: "036430-02.d1",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0xa4d7a525],
        },
        RomEntry {
            name: "036431-02.ef1",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0xd4004aae],
        },
        RomEntry {
            name: "036432-02.fh1",
            size: 0x0800,
            offset: 0x1000,
            crc32: &[0x6d720c41],
        },
        RomEntry {
            name: "036433-03.j1",
            size: 0x0800,
            offset: 0x1800,
            crc32: &[0x0dcc0be6],
        },
    ],
};

/// Vector ROM: 4KB at CPU addresses 0x4800–0x57FF.
static VECTOR_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[
        RomEntry {
            name: "036800-02.r2",
            size: 0x0800,
            offset: 0x0000,
            crc32: &[0xbb8cabe1],
        },
        RomEntry {
            name: "036799-01.np2",
            size: 0x0800,
            offset: 0x0800,
            crc32: &[0x7d511572],
        },
    ],
};

// ---------------------------------------------------------------------------
// Input button IDs
// ---------------------------------------------------------------------------

pub const INPUT_COIN: u8 = 0;
pub const INPUT_START1: u8 = 1;
pub const INPUT_START2: u8 = 2;
pub const INPUT_THRUST: u8 = 3;
pub const INPUT_FIRE: u8 = 4;
pub const INPUT_SHIELD: u8 = 5;
pub const INPUT_ROT_LEFT: u8 = 6;
pub const INPUT_ROT_RIGHT: u8 = 7;

// NO SELF-TEST CONTROL, DELIBERATELY. The cabinet has the switch and IN0 bit 7
// is where the mux comment says it lands, but driving that bit does not produce
// a self-test display: the screen stays black through 3000 frames and the 6502's
// PC pins at 0x7DF8, sampled identically six times over while attract-mode PCs
// roam. The routine is waiting on something this board does not yet model. A
// control bound to a key that freezes the game is worse than none, so it is left
// out until the underlying gap is fixed.

/// Typed logical controls. `InputId`s reuse the `INPUT_*` numbering; default
/// bindings mirror the legacy name-matched defaults (thrust = "P1 Up", shield =
/// "P1 Jump", rotate = "P1 Left/Right"). Fire is the primary action (gamepad A);
/// shield is key-only.
const ASTDELUX_CONTROLS: &[InputControl] = &[
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
        id: InputId(INPUT_THRUST as u16),
        stable_name: "thrust",
        label: "Thrust",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_UP,
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
        id: InputId(INPUT_SHIELD as u16),
        stable_name: "shield",
        label: "Shield",
        kind: InputKind::Action(ActionRole::Secondary),
        player: Some(1),
        default_bindings: &[],
    },
    InputControl {
        id: InputId(INPUT_ROT_LEFT as u16),
        stable_name: "rotate_left",
        label: "Rotate Left",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_LEFT,
    },
    InputControl {
        id: InputId(INPUT_ROT_RIGHT as u16),
        stable_name: "rotate_right",
        label: "Rotate Right",
        kind: InputKind::Button,
        player: Some(1),
        default_bindings: crate::input_defaults::P1_RIGHT,
    },
];

// ---------------------------------------------------------------------------
// AsteroidsDeluxeSystem — Atari DVG board configured for Asteroids Deluxe (1980)
// ---------------------------------------------------------------------------

/// Asteroids Deluxe-specific wrapper around the shared Atari DVG board.
///
/// Adds POKEY sound chip at 0x2C00 and EAROM (ER2055) for high score storage.
///
/// Memory map (15-bit address bus, `addr & 0x7FFF`):
///   0x0000–0x03FF  RAM (1 KB)
///   0x2000–0x2007  IN0 read (buttons, 3 KHz clock, VG_HALT)
///   0x2400–0x2407  IN1 read (coins, start, thrust, rotate)
///   0x2800–0x2803  DSW1 read (DIP switches)
///   0x2C00–0x2C0F  POKEY read/write
///   0x2C40–0x2C7F  EAROM data read
///   0x3000         DVG GO write
///   0x3200–0x323F  EAROM data/address write
///   0x3400         Watchdog reset write
///   0x3600         Explosion sound write
///   0x3A00         EAROM control write
///   0x3C00–0x3C07  Audio latch write (74LS259)
///   0x3E00         Noise reset write
///   0x4000–0x47FF  Vector RAM (2 KB, shared CPU/DVG)
///   0x4800–0x57FF  Vector ROM (4 KB)
///   0x6000–0x7FFF  Program ROM (8 KB)
#[derive(Saveable, phosphor_macros::BusDebug)]
pub struct AsteroidsDeluxeSystem {
    /// The 6502 is held beside the bus view over the board.
    #[debug_cpu("M6502")]
    pub cpu: M6502,

    #[debug_bus]
    pub board: AtariDvgBoard,

    // POKEY sound chip at 0x2C00–0x2C0F
    pokey: Pokey,

    // I/O — active-HIGH inputs (default 0x00 = all released)
    in0: u8,
    in1: u8,
    /// DIP switches (R5, the LEFT switch assembly): default 0x00, which is the
    /// manual's factory setting throughout, being English, 2-4 ships, 1-play
    /// minimum and a bonus ship every 10,000. Read as a byte through the 74LS253
    /// at 0x2800.
    #[save_skip]
    dip_switches: u8,
    /// DIP switches (L8, the CENTER switch assembly): the coinage bank, read
    /// through the POKEY's pot inputs rather than as a byte. See
    /// [`refresh_dip_pots`](Self::refresh_dip_pots).
    ///
    /// **Default 0xFF, not 0x00.** A set bit here is an *open* toggle, and an
    /// open toggle leaves its pot line low, so 0xFF is the byte that reproduces
    /// undriven pots and it is what this machine has been running on all along.
    /// Defaulting to 0x00 would have driven all eight lines high and quietly
    /// changed the game's price from 50c to free play.
    ///
    /// 0xFF is a switch assembly straight out of the bag: every toggle open. It
    /// decodes as 2 coins for 1 play with no bonus coins, which is Figure 8's
    /// "50c per play / no bonus" column, and as right mech x6 and center mech
    /// x2, which are settings no Figure 8 recipe uses.
    #[save_skip]
    dip_l8: u8,

    // EAROM (ER2055): 64-byte non-volatile RAM for high scores
    earom: Er2055,

    // Audio buffer from POKEY
    #[save_skip(default)]
    audio_buffer: SampleRing<i16>,
}

impl AsteroidsDeluxeSystem {
    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(Region::Ram, "RAM", 0x0000, 0x0400, AccessKind::ReadWrite)
            .region(Region::Io, "I/O", 0x2000, 0x2000, AccessKind::Io)
            .region(
                Region::VectorRam,
                "Vector RAM",
                0x4000,
                0x0800,
                AccessKind::ReadWrite,
            )
            .region(
                Region::VectorRom,
                "Vector ROM",
                0x4800,
                0x1000,
                AccessKind::ReadOnly,
            )
            .region(
                Region::ProgramRom,
                "Program ROM",
                0x6000,
                0x2000,
                AccessKind::ReadOnly,
            );
        map
    }

    pub fn new() -> Self {
        let mut sys = Self {
            cpu: M6502::new(),
            // Asteroids Deluxe: VROM at DVG 0x0800, size 0x1000
            board: AtariDvgBoard::new(Self::build_map(), 0x0800, 0x1000),
            pokey: Pokey::with_clock(1_512_000, phosphor_core::audio::host_sample_rate()),
            in0: 0x00,
            in1: 0x00,
            dip_switches: 0x00,
            dip_l8: 0xFF,
            earom: Er2055::new(),
            audio_buffer: SampleRing::with_capacity(1024),
        };
        sys.refresh_dip_pots();
        sys
    }

    /// Drive the L8 DIP switches onto the POKEY's pot inputs.
    ///
    /// L8's eight toggles are wired one per pot line rather than onto a byte the
    /// CPU can read directly, so the game reads them the long way round: strobe
    /// POTGO, poll ALLPOT until the scan finishes, then read POT0-7. That this is
    /// the path was settled by watching which POKEY offsets the ROM reads: it
    /// touches 0x00-0x08, which is the full scan.
    ///
    /// **A closed (On) toggle drives its line HIGH, so a set bit, meaning an
    /// open toggle, leaves the line low.** That is backwards from the obvious
    /// reading and backwards from Missile Command's pot-wired bank, where a
    /// closed switch drives high, so it was measured rather than assumed: the
    /// game prints its decoded price on the attract screen, and stepping the
    /// bank through all four Game Price values reproduces all four of the
    /// manual's prices only under this sense. Under the opposite one the
    /// power-on machine would read free play, where it has always shown
    /// `2 COINS 1 CREDIT`. The tests at the bottom of this file record the
    /// seven bytes that were driven and what the screen said to each.
    fn refresh_dip_pots(&mut self) {
        for n in 0..8 {
            // A set bit is an OPEN toggle, and an open toggle leaves its pot
            // line low. See the doc comment above for how that was measured.
            let level = if self.dip_l8 & (1 << n) != 0 {
                0x00
            } else {
                0x80
            };
            self.pokey.set_pot_input(n, level);
        }
    }

    /// Borrow the CPU and the bus it drives as two disjoint pieces.
    #[inline]
    fn split(&mut self) -> (&mut M6502, AsteroidsDeluxeBus<'_>) {
        (
            &mut self.cpu,
            AsteroidsDeluxeBus {
                board: &mut self.board,
                pokey: &mut self.pokey,
                earom: &mut self.earom,
                in0: self.in0,
                in1: self.in1,
                dip_switches: self.dip_switches,
            },
        )
    }

    /// One CPU cycle. Returns 1 at an instruction boundary (for the debugger,
    /// which steps instructions rather than cycles).
    pub fn step_cycle(&mut self) -> u32 {
        let (cpu, mut bus) = self.split();
        atari_dvg::tick(cpu, &mut bus);
        AtariDvgBoard::instruction_boundaries(&self.cpu)
    }

    /// Read the CPU-facing bus, side effects and all. Distinct from the
    /// debugger's `BusDebug::peek`/`poke`, which avoid side effects.
    pub fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        self.split().1.read(master, addr)
    }

    /// Write the CPU-facing bus, side effects and all. See [`Self::bus_read`].
    pub fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.split().1.write(master, addr, data);
    }

    pub fn load_rom_set(&mut self, rom_set: &RomSet) -> Result<(), RomLoadError> {
        let prog = PROGRAM_ROM.load(rom_set)?;
        self.board.map.load_region(Region::ProgramRom, &prog);
        let vrom = VECTOR_ROM.load(rom_set)?;
        self.board.map.load_region(Region::VectorRom, &vrom);
        Ok(())
    }
}

impl Default for AsteroidsDeluxeSystem {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus implementation
// ---------------------------------------------------------------------------

/// The Asteroids Deluxe bus: the shared DVG board plus this game's I/O -- the
/// input multiplexers, the DIP switches, the POKEY and the high-score EAROM.
struct AsteroidsDeluxeBus<'a> {
    board: &'a mut AtariDvgBoard,
    pokey: &'a mut Pokey,
    earom: &'a mut Er2055,
    in0: u8,
    in1: u8,
    dip_switches: u8,
}

impl AtariDvgBus for AsteroidsDeluxeBus<'_> {
    #[inline]
    fn board(&mut self) -> &mut AtariDvgBoard {
        self.board
    }

    #[inline]
    fn begin_cycle(&mut self) {
        self.pokey.tick();
    }
}

impl Bus for AsteroidsDeluxeBus<'_> {
    type Address = u16;
    type Data = u8;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let addr = addr & 0x7FFF; // 15-bit address bus

        let data = match self.board.map.page(addr).region_id {
            Region::RAM | Region::VECTOR_RAM | Region::VECTOR_ROM | Region::PROGRAM_ROM => {
                self.board.map.read_backing(addr)
            }

            Region::IO => match addr {
                // IN0: 0x2000–0x2007 — 74LS251 8:1 multiplexer (same as Asteroids).
                //   Bit 0: unused     Bit 1: 3 KHz clock     Bit 2: VG_HALT
                //   Bit 3: Shield     Bit 4: Fire
                //   Bit 5: Diagnostic Bit 6: Tilt     Bit 7: Self-test
                0x2000..=0x2007 => {
                    let offset = (addr & 7) as u8;
                    let mut val = self.in0;
                    if self.board.clock & 0x100 != 0 {
                        val |= 0x02;
                    } else {
                        val &= !0x02;
                    }
                    if !self.board.dvg.is_halted() {
                        val |= 0x04;
                    } else {
                        val &= !0x04;
                    }
                    ((val >> offset) & 1) << 7
                }

                // IN1: 0x2400–0x2407 — 74LS251 8:1 multiplexer (same as Asteroids).
                0x2400..=0x2407 => {
                    let offset = (addr & 7) as u8;
                    ((self.in1 >> offset) & 1) << 7
                }

                // R5: 0x2800-0x2803, a 74LS253 dual 4:1 multiplexer.
                //
                // AB0 and AB1 pick one of the four toggle PAIRS, and the two
                // halves of the mux put that pair on DB0 and DB1: the drawing
                // package's `OPTIONS INPUT CIRCUITRY` says "switch toggles 1, 3,
                // 5 and 7 are read on data line DB0 and toggles 2, 4, 6 and 8
                // are read on DB1". So each address returns its pair as a plain
                // two-bit field, which is why every site in the ROM that reads
                // one masks it with `AND #$03`.
                //
                // It used to return the second toggle on DB7. That put every
                // EVEN toggle where the ROM's mask throws it away, so half the
                // bank reached nothing: French and Spanish were unselectable,
                // and so was half of every other option. Toggle inputs are "on"
                // when pulled to ground, which is the closed-reads-0 sense the
                // bank table already carried.
                // AB0/AB1 count the pairs DOWNWARDS: 0x2800 selects toggles 7-8
                // and 0x2803 selects toggles 1-2. Measured, not guessed. With
                // the pairs ascending, setting the Language bits moved the bonus
                // threshold and setting the Bonus Life bits changed the language,
                // which is the same two options swapped end for end.
                0x2800..=0x2803 => {
                    let pair = 6 - (addr & 3) as u8 * 2;
                    (self.dip_switches >> pair) & 0x03
                }

                // POKEY: 0x2C00–0x2C0F
                0x2C00..=0x2C0F => self.pokey.read(addr & 0x0F),

                // EAROM data read: 0x2C40–0x2C7F
                0x2C40..=0x2C7F => self.earom.read(addr & 0x3F),

                _ => 0,
            },

            _ => 0,
        };

        self.board.map.watch_read(0, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        let addr = addr & 0x7FFF; // 15-bit address bus

        self.board.map.watch_write(0, master, addr, data);
        self.board.trace_main_write(addr, data);

        match self.board.map.page(addr).region_id {
            Region::RAM | Region::VECTOR_RAM => self.board.map.write_backing(addr, data),

            Region::IO => match addr {
                // POKEY: 0x2C00–0x2C0F
                0x2C00..=0x2C0F => self.pokey.write(addr & 0x0F, data),

                0x3000 => self.board.trigger_dvg(),

                // EAROM data/address write: 0x3200–0x323F
                // Offset selects 6-bit address, data byte is the value to write.
                0x3200..=0x323F => self.earom.latch(addr & 0x3F, data),

                0x3400 => self.board.watchdog_frame_count = 0,
                0x3600 => { /* explosion sound stub */ }

                // EAROM control: 0x3A00
                // Bit 0: CK (clock), Bit 1: !C1, Bit 2: C2, Bit 3: CS1
                0x3A00 => {
                    let clock = data & 0x01 != 0;
                    let c1 = data & 0x02 == 0; // bit 1 inverted
                    let c2 = data & 0x04 != 0; // bit 2
                    let cs1 = data & 0x08 != 0;
                    self.earom.write_control(clock, cs1, c1, c2);
                }

                0x3C00..=0x3C07 => { /* audio latch stub */ }
                0x3E00 => { /* noise reset stub */ }
                _ => {}
            },

            _ => {}
        }
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            nmi: self.board.nmi_pending,
            irq: self.pokey.irq(),
            firq: false,
            irq_vector: 0,
            irq_level: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Machine traits (MachineCore + capabilities)
// ---------------------------------------------------------------------------

crate::impl_board_renderable!(AsteroidsDeluxeSystem, board, atari_dvg::TIMING, vectors);

impl AudioSource for AsteroidsDeluxeSystem {
    fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.audio_buffer.pop_front_into(buffer)
    }

    fn audio_sample_rate(&self) -> u32 {
        phosphor_core::audio::host_sample_rate()
    }
}

impl InputConfigurable for AsteroidsDeluxeSystem {
    fn input_controls(&self) -> &'static [InputControl] {
        ASTDELUX_CONTROLS
    }

    fn handle_input(&mut self, event: InputEvent) {
        let InputEvent::Button { id, pressed } = event else {
            return;
        };
        match id.0 as u8 {
            // IN1 (active-HIGH)
            INPUT_COIN => set_bit_active_high(&mut self.in1, 0, pressed),
            INPUT_START1 => set_bit_active_high(&mut self.in1, 3, pressed),
            INPUT_START2 => set_bit_active_high(&mut self.in1, 4, pressed),
            INPUT_THRUST => set_bit_active_high(&mut self.in1, 5, pressed),
            INPUT_ROT_RIGHT => set_bit_active_high(&mut self.in1, 6, pressed),
            INPUT_ROT_LEFT => set_bit_active_high(&mut self.in1, 7, pressed),

            // IN0 (active-HIGH)
            INPUT_FIRE => set_bit_active_high(&mut self.in0, 4, pressed),
            INPUT_SHIELD => set_bit_active_high(&mut self.in0, 3, pressed),

            _ => {}
        }
    }
}

crate::impl_board_debug!(AsteroidsDeluxeSystem, board, atari_dvg::TIMING);

impl MachineCore for AsteroidsDeluxeSystem {
    crate::machine_core_metadata!("astdelux", atari_dvg::TIMING, atari_dvg::clock_tree);

    fn run_frame(&mut self) {
        // The POKEY is clocked per cycle by the bus view's `begin_cycle` hook.
        let (cpu, mut bus) = self.split();
        atari_dvg::run_frame(cpu, &mut bus);

        // Drain POKEY audio samples
        let samples = self.pokey.drain_audio();
        self.audio_buffer
            .extend(samples.iter().map(|&s| (s * 32767.0) as i16));

        // Clear NMI at frame boundary to avoid stale edges.
        self.board.nmi_pending = false;

        // Watchdog
        self.board.watchdog_frame_count += 1;
        if self.board.watchdog_frame_count >= 8 {
            self.reset();
        }
    }

    fn reset(&mut self) {
        self.board.reset();
        self.pokey.reset();
        // The pots are wiring, not state: a reset must leave the DIP switches
        // still driving them, exactly as the cabinet's do. `Pokey::reset` clears
        // the pot inputs, so this has to come after it.
        self.refresh_dip_pots();
        let (cpu, mut bus) = self.split();
        cpu.reset(&mut bus, BusMaster::Cpu(0));
    }
}

impl SaveState for AsteroidsDeluxeSystem {
    crate::machine_save_state!();
}

impl Nvram for AsteroidsDeluxeSystem {
    fn save_nvram(&self) -> Option<&[u8]> {
        Some(self.earom.snapshot())
    }

    fn load_nvram(&mut self, data: &[u8]) {
        self.earom.load_from(data);
    }
}

impl Profilable for AsteroidsDeluxeSystem {}
/// The two operator switch assemblies on the Asteroids Deluxe PCB.
///
/// Both tables are transcribed from the operator manual, which describes them
/// twice over: once as toggle positions (Figure 7 for R5, Figure 8 for L8) and
/// once as the decoded fields the game prints on its own self-test screen
/// (Figure 6). The two descriptions agree, which is what makes the field
/// decomposition below an assertion of the manual's rather than an invention.
/// See [`ASTDELUX_L8_DERIVATION`] for the arithmetic.
///
/// Both banks decode the same way: toggle *n* is bit *n* - 1, and a closed (On)
/// toggle reads as 0. For R5 that is the schematic's doing: its switches short
/// to ground against 10k pull-ups into the P5 mux. For L8 it was measured off
/// the running game rather than carried across, since two banks on one board are
/// free to disagree and Missile Command's two do. Note that agreeing at the byte
/// costs L8 the opposite wiring at the pots; see
/// [`refresh_dip_pots`](AsteroidsDeluxeSystem::refresh_dip_pots).
const ASTDELUX_DIP_BANKS: &[DipSwitchBank] = &[
    DipSwitchBank {
        name: "R5 (Game Options)",
        options: &[
            DipOption {
                name: "Language",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "English",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "German",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "French",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "Spanish",
                        value: 0x03,
                    },
                ],
            },
            DipOption {
                name: "Lives",
                mask: 0x0C,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "2-4",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "3-5",
                        value: 0x04,
                    },
                    DipChoice {
                        label: "4-6",
                        value: 0x08,
                    },
                    DipChoice {
                        label: "5-7",
                        value: 0x0C,
                    },
                ],
            },
            DipOption {
                name: "Minimum Plays",
                mask: 0x10,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "1",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "2",
                        value: 0x10,
                    },
                ],
            },
            // NOT IN THE MANUAL. Figure 7 marks toggle 6 `Not Used`, and Figure 6
            // agrees from the other side: this bit lands in the self-test's
            // minimum-plays digit, whose legend reads `0 or 2 = 1-play minimum,
            // 1 or 3 = 2-play minimum`, the 2s bit provably changing nothing.
            // Left in place because removing it is a behavior question for the R5
            // bank rather than part of adding L8; tracked separately.
            DipOption {
                name: "Difficulty",
                mask: 0x20,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Easy",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Hard",
                        value: 0x20,
                    },
                ],
            },
            DipOption {
                name: "Bonus Life",
                mask: 0xC0,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "10000",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "12000",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "15000",
                        value: 0x80,
                    },
                    DipChoice {
                        label: "None",
                        value: 0xC0,
                    },
                ],
            },
        ],
    },
    DipSwitchBank {
        name: "L8 (Coinage)",
        options: &[
            DipOption {
                name: "Game Price",
                mask: 0x03,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "Free Play",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "1 Coin/2 Plays",
                        value: 0x01,
                    },
                    DipChoice {
                        label: "1 Coin/1 Play",
                        value: 0x02,
                    },
                    DipChoice {
                        label: "2 Coins/1 Play",
                        value: 0x03,
                    },
                ],
            },
            DipOption {
                name: "Right Coin Mech",
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
            // Figure 6: "Both these settings affect the left mech in a 2-mech door."
            DipOption {
                name: "Center Coin Mech",
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
            // A "coin" is 25c in the U.S. and 1 DM in Germany, so these read as
            // counts rather than as cash. Figure 6 spells the field out as
            // "0, 5, 6 or 7 = No bonus coins", so four of the eight values are the
            // same setting; all four are listed because the power-on byte lands on
            // 7, and an option the default cannot name fails the table validator.
            DipOption {
                name: "Coin Bonus Adder",
                mask: 0xE0,
                apply: DipApplyTiming::Immediate,
                choices: &[
                    DipChoice {
                        label: "None",
                        value: 0x00,
                    },
                    DipChoice {
                        label: "Every 2 Coins, +1",
                        value: 0x20,
                    },
                    DipChoice {
                        label: "Every 4 Coins, +1",
                        value: 0x40,
                    },
                    DipChoice {
                        label: "Every 4 Coins, +2",
                        value: 0x60,
                    },
                    DipChoice {
                        label: "Every 5 Coins, +1",
                        value: 0x80,
                    },
                    DipChoice {
                        label: "None (5)",
                        value: 0xA0,
                    },
                    DipChoice {
                        label: "None (6)",
                        value: 0xC0,
                    },
                    DipChoice {
                        label: "None (7)",
                        value: 0xE0,
                    },
                ],
            },
        ],
    },
];

/// Why L8 is modeled as four fields rather than as twelve whole-byte recipes.
///
/// Figure 8 alone would not support the decomposition. It is a grid of coin-door
/// type against bonus scheme, and each cell gives a *complete* eight-toggle
/// recipe, "Straight 25c Door, 50c per play, no bonus" and so on. Nothing in it
/// says which toggles carry which meaning, so reading fields out of it would be
/// inference dressed up as transcription.
///
/// Figure 6 supplies the missing half. The self-test prints four digits, and its
/// legend names them: Coin Bonus Adder, Center Mech Multiplier, Right Mech
/// Multiplier, Game Price, with the values each digit can take. Those are the
/// fields, asserted by the manual outright.
///
/// The two figures then check against each other, and every one of Figure 8's
/// twelve recipes lands exactly, which is what rules out an off-by-one in the
/// toggle-to-bit mapping or a flipped sense:
///
/// | Figure 8 cell | byte | Game Price | Bonus Adder | arithmetic |
/// |---|---|---|---|---|
/// | 50c, no bonus | 0x03 | 3 = 2 coins/play | 0 = none | n/a |
/// | 50c, $1.00 = 3 plays | 0x63 | 3 = 2 coins/play | 3 = every 4 coins, +2 | $1.00 = 4 coins, +2 = 6 = 3 plays |
/// | 50c, $.50/$.75/$1.00 = 1/2/3 plays | 0x23 | 3 = 2 coins/play | 1 = every 2 coins, +1 | 2 -> 3 = 1 play; 3 -> 4 = 2; 4 -> 6 = 3 |
/// | 25c, no bonus | 0x02 | 2 = 1 coin/play | 0 = none | n/a |
/// | 25c, $.50 = 3 plays | 0x22 | 2 = 1 coin/play | 1 = every 2 coins, +1 | $.50 = 2 coins, +1 = 3 = 3 plays |
/// | 25c, $1.00 = 5 plays | 0x42 | 2 = 1 coin/play | 2 = every 4 coins, +1 | $1.00 = 4 coins, +1 = 5 = 5 plays |
///
/// The 25c/$1.00-door rows are the same six with bit 2 set, which is Right Mech
/// x4, a dollar slot counting as four quarters. Toggles 4 and 5 are On in all
/// twelve, so Figure 8 asserts nothing about them and Figure 6 is the only
/// source for the x5/x6 and center-mech settings.
///
/// This is a doc-only item; the tests below check the same six rows.
#[allow(dead_code)]
const ASTDELUX_L8_DERIVATION: () = ();

/// R5 is read directly at 0x2800 through the 74LS253. L8 reaches the CPU through
/// the POKEY's pot inputs, so setting it has to re-drive them: the same shape as
/// `quantum.rs` and `missile_command.rs`, which is why this is hand-written
/// rather than `impl_dip_switches!`.
impl DipSwitches for AsteroidsDeluxeSystem {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        ASTDELUX_DIP_BANKS
    }

    fn dip_bank_value(&self, bank: usize) -> u8 {
        match bank {
            0 => self.dip_switches,
            1 => self.dip_l8,
            _ => 0,
        }
    }

    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        match bank {
            0 => self.dip_switches = value,
            1 => {
                self.dip_l8 = value;
                self.refresh_dip_pots();
            }
            _ => {}
        }
    }
}
crate::impl_board_debug_trace!(AsteroidsDeluxeSystem, board);

// ---------------------------------------------------------------------------
// Machine registry
// ---------------------------------------------------------------------------

crate::register_machine!(
    AsteroidsDeluxeSystem,
    "astdelux",
    &["astdelux"],
    ASTDELUX_CONTROLS
);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atari_dvg::Region;
    use phosphor_core::cpu::CpuStateTrait;

    #[test]
    fn dip_default_and_metadata() {
        let sys = AsteroidsDeluxeSystem::new();
        assert_eq!(sys.dip_bank_value(0), 0x00);
        assert_eq!(sys.dip_bank_value(1), 0xFF);
        crate::assert_dip_banks_valid(
            sys.dip_banks(),
            &[sys.dip_bank_value(0), sys.dip_bank_value(1)],
        );
    }

    #[test]
    fn set_dip_option_masks_only_its_bits() {
        let mut sys = AsteroidsDeluxeSystem::new();
        // Bonus Life is option 4 (mask 0xC0); pick "None" (0xC0).
        sys.set_dip_option(0, 4, 0xC0);
        assert_eq!(sys.dip_bank_value(0), 0xC0);
    }

    /// What the 74LS253 puts on the bus for each of R5's four toggle pairs.
    ///
    /// The pattern is chosen so a single read distinguishes all three ways this
    /// decode has been wrong. `0xE4` is the four pairs holding 3, 2, 1, 0 from
    /// the top of the byte down, so the four addresses must read 3, 2, 1, 0:
    ///
    /// * putting the second toggle of a pair on DB7 instead of DB1 makes every
    ///   read 0x00 or 0x81, because the ROM masks with `AND #$03`;
    /// * counting the pairs upwards instead of downwards reads 0, 1, 2, 3, which
    ///   is Language and Bonus Life swapped end for end.
    ///
    /// Both were live at once, so R5 delivered one working option out of five.
    #[test]
    fn the_dip_mux_reads_pairs_from_the_top_of_the_byte_down() {
        let mut sys = AsteroidsDeluxeSystem::new();
        sys.set_dip_bank_value(0, 0b11_10_01_00);
        for (offset, expect) in [(0, 3), (1, 2), (2, 1), (3, 0)] {
            let got = sys.bus_read(BusMaster::Cpu(0), 0x2800 + offset);
            assert_eq!(got, expect, "read of 0x{:04X}", 0x2800 + offset);
        }

        // Only DB0 and DB1 are driven; nothing else on the byte is.
        sys.set_dip_bank_value(0, 0xFF);
        for offset in 0..4 {
            let got = sys.bus_read(BusMaster::Cpu(0), 0x2800 + offset);
            assert_eq!(
                got,
                0x03,
                "read of 0x{:04X} with every toggle set",
                0x2800 + offset
            );
        }
    }

    /// L8 reaches the CPU only through the POKEY's pot inputs, so setting the
    /// bank has to re-drive them. Without that the switches are settable and
    /// have no effect, which is the failure this bank exists to fix.
    #[test]
    fn setting_l8_drives_the_pokey_pots() {
        let mut sys = AsteroidsDeluxeSystem::new();
        // The power-on byte is all toggles open, which is every pot line low:
        // exactly the undriven state this machine ran on before the bank
        // existed. A default of 0x00 would drive all eight high instead.
        for n in 0..8 {
            assert_eq!(sys.pokey.pot_input(n), 0x00, "pot {n} at power-on");
        }

        // Figure 8's "25c per play, Straight 25c Door, $1.00 = 5 plays". A set
        // bit is an open toggle, so it drives its line LOW.
        sys.set_dip_bank_value(1, 0x42);
        for n in 0..8 {
            let expect = if 0x42 & (1 << n) != 0 { 0x00 } else { 0x80 };
            assert_eq!(sys.pokey.pot_input(n), expect, "pot {n} after set");
        }

        // And a reset must not drop the wiring on the floor. `Pokey::reset`
        // clears the pot inputs, so this fails if the refresh runs before it.
        sys.reset();
        assert_eq!(sys.pokey.pot_input(1), 0x00);
        assert_eq!(sys.pokey.pot_input(6), 0x00);
        assert_eq!(sys.pokey.pot_input(0), 0x80);
    }

    /// Figure 8's twelve toggle recipes against the four fields Figure 6 names.
    ///
    /// This is the check that makes the decomposition a transcription rather
    /// than an inference: the recipes and the field legend are independent
    /// tables in the manual, and every row has to reproduce exactly.
    #[test]
    fn l8_choices_reproduce_the_manual_price_recipes() {
        let l8 = &ASTDELUX_DIP_BANKS[1];
        assert_eq!(l8.name, "L8 (Coinage)");

        let choice = |option: &str, label: &str| -> u8 {
            let o = l8.options.iter().find(|o| o.name == option).unwrap();
            o.choices.iter().find(|c| c.label == label).unwrap().value
        };

        let price_50c = choice("Game Price", "2 Coins/1 Play");
        let price_25c = choice("Game Price", "1 Coin/1 Play");
        let straight_door = choice("Right Coin Mech", "x1");
        let dollar_door = choice("Right Coin Mech", "x4");
        let no_bonus = choice("Coin Bonus Adder", "None");
        let every_2_plus_1 = choice("Coin Bonus Adder", "Every 2 Coins, +1");
        let every_4_plus_1 = choice("Coin Bonus Adder", "Every 4 Coins, +1");
        let every_4_plus_2 = choice("Coin Bonus Adder", "Every 4 Coins, +2");

        // Toggle n is bit n-1 and a closed (On) toggle reads 0, so a recipe's
        // byte is the sum of the bits its *open* toggles claim.
        let recipe = |open: &[u8]| -> u8 { open.iter().fold(0u8, |b, t| b | 1 << (t - 1)) };

        // --- 50c per play, Straight 25c Door (Figure 8, upper block, row 1) ---
        assert_eq!(price_50c | straight_door | no_bonus, recipe(&[2, 1]));
        assert_eq!(
            price_50c | straight_door | every_4_plus_2,
            recipe(&[7, 6, 2, 1]),
            "$1.00 = 3 plays"
        );
        assert_eq!(
            price_50c | straight_door | every_2_plus_1,
            recipe(&[6, 2, 1]),
            "$.50/$.75/$1.00 = 1/2/3 plays"
        );

        // --- 25c per play, Straight 25c Door (Figure 8, lower block, row 1) ---
        assert_eq!(price_25c | straight_door | no_bonus, recipe(&[2]));
        assert_eq!(
            price_25c | straight_door | every_2_plus_1,
            recipe(&[6, 2]),
            "$.50 = 3 plays"
        );
        assert_eq!(
            price_25c | straight_door | every_4_plus_1,
            recipe(&[7, 2]),
            "$1.00 = 5 plays"
        );

        // The 25c/$1.00-door rows are the six above with toggle 3 opened, which
        // is Right Mech x4: a dollar slot counting as four quarters.
        assert_eq!(dollar_door, recipe(&[3]));

        // Toggles 4 and 5 are On in all twelve recipes, so every byte above
        // leaves bits 3 and 4 clear.
        for byte in [
            price_50c | straight_door | no_bonus,
            price_25c | dollar_door | every_4_plus_1,
        ] {
            assert_eq!(byte & 0x18, 0, "toggles 4 and 5 are On throughout Figure 8");
        }
    }

    /// What the running game printed for each byte that was driven into it.
    ///
    /// The table above is the manual's; this is the machine's own reading of it,
    /// taken off the attract screen, which prints the decoded price and, after
    /// coins, the credit count, end to end through the real POKEY pot scan. Each
    /// row was a prediction before it was an observation, and the polarity is
    /// what they collectively pin: under the opposite sense every one inverts.
    ///
    /// | bank | Figure 8 cell | screen said |
    /// |---|---|---|
    /// | 0xFF | (power-on, all toggles open) | `2 COINS 1 CREDIT` |
    /// | 0x02 | 25c/play, straight door, no bonus | `1 COIN 1 CREDIT`; 2 coins -> `CREDITS 2` |
    /// | 0x01 | n/a | `1 COIN 2 CREDITS` |
    /// | 0x00 | n/a | no price line and no credit line, which is free play |
    /// | 0x22 | 25c/play, `$.50 = 3 plays` | 2 coins -> `CREDITS 3` |
    /// | 0x42 | 25c/play, `$1.00 = 5 plays` | 4 coins -> `CREDITS 5` |
    /// | 0x63 | 50c/play, `$1.00 = 3 plays` | 4 coins -> `CREDITS 3` |
    ///
    /// The last three are the Coin Bonus Adder doing arithmetic the manual
    /// states in cash and the game does in coins. Not covered: the two mech
    /// multipliers, since this machine models one coin input and there is no
    /// right or center mech to multiply.
    #[test]
    fn l8_bytes_match_what_the_game_displayed() {
        let sys = AsteroidsDeluxeSystem::new();
        assert_eq!(sys.dip_bank_value(1), 0xFF);

        let l8 = &ASTDELUX_DIP_BANKS[1];
        let selected = |byte: u8, option: &str| -> &str {
            let o = l8.options.iter().find(|o| o.name == option).unwrap();
            let slice = byte & o.mask;
            o.choices.iter().find(|c| c.value == slice).unwrap().label
        };

        // 0xFF, the power-on byte: what the attract screen has always shown.
        assert_eq!(selected(0xFF, "Game Price"), "2 Coins/1 Play");
        assert_eq!(selected(0xFF, "Coin Bonus Adder"), "None (7)");
        // The whole Game Price field, all four of them read off the screen.
        assert_eq!(selected(0x02, "Game Price"), "1 Coin/1 Play");
        assert_eq!(selected(0x01, "Game Price"), "1 Coin/2 Plays");
        assert_eq!(selected(0x00, "Game Price"), "Free Play");
        // The three bonus recipes, each checked by counting credits.
        assert_eq!(selected(0x22, "Coin Bonus Adder"), "Every 2 Coins, +1");
        assert_eq!(selected(0x42, "Coin Bonus Adder"), "Every 4 Coins, +1");
        assert_eq!(selected(0x63, "Coin Bonus Adder"), "Every 4 Coins, +2");
        // ...all three at the price the same screen reported.
        assert_eq!(selected(0x22, "Game Price"), "1 Coin/1 Play");
        assert_eq!(selected(0x42, "Game Price"), "1 Coin/1 Play");
        assert_eq!(selected(0x63, "Game Price"), "2 Coins/1 Play");
    }

    #[test]
    fn save_load_round_trip() {
        let mut sys = AsteroidsDeluxeSystem::new();

        // Set known state
        sys.board.map.region_data_mut(Region::Ram)[0x100] = 0xAA;
        sys.board.map.region_data_mut(Region::VectorRam)[0x200] = 0xBB;
        sys.in0 = 0x18;
        sys.in1 = 0xE8;
        sys.board.clock = 75_000;
        sys.board.nmi_counter = 3000;
        sys.board.nmi_pending = true;
        sys.board.watchdog_frame_count = 5;
        sys.earom.load_from(&{
            let mut d = [0u8; 64];
            d[0] = 0x42;
            d[63] = 0xEF;
            d
        });

        // Save
        let data = sys.save_state().expect("save_state should return Some");
        let cpu_snap = sys.cpu.snapshot();

        // Mutate everything
        let mut sys2 = AsteroidsDeluxeSystem::new();
        sys2.board.map.region_data_mut(Region::Ram)[0x100] = 0xFF;
        sys2.in0 = 0xFF;
        sys2.board.clock = 999;

        // Load
        sys2.load_state(&data).unwrap();

        // Verify CPU
        assert_eq!(sys2.cpu.snapshot(), cpu_snap);

        // Verify memory
        assert_eq!(sys2.board.map.region_data(Region::Ram)[0x100], 0xAA);
        assert_eq!(sys2.board.map.region_data(Region::VectorRam)[0x200], 0xBB);

        // Verify I/O and timing state
        assert_eq!(sys2.in0, 0x18);
        assert_eq!(sys2.in1, 0xE8);
        assert_eq!(sys2.board.clock, 75_000);
        assert_eq!(sys2.board.nmi_counter, 3000);
        assert!(sys2.board.nmi_pending);
        assert_eq!(sys2.board.watchdog_frame_count, 5);

        // Verify EAROM
        assert_eq!(sys2.earom.read(0), 0x42);
        assert_eq!(sys2.earom.read(63), 0xEF);
    }

    #[test]
    fn save_does_not_include_rom() {
        let mut sys = AsteroidsDeluxeSystem::new();
        sys.board.map.region_data_mut(Region::ProgramRom)[0] = 0xDE;
        sys.board.map.region_data_mut(Region::VectorRom)[0] = 0xAD;

        let data = sys.save_state().unwrap();

        // Load into a fresh system (ROMs are zeroed)
        let mut sys2 = AsteroidsDeluxeSystem::new();
        sys2.load_state(&data).unwrap();

        // ROMs should remain at their default (zeroed), not overwritten
        assert_eq!(sys2.board.map.region_data(Region::ProgramRom)[0], 0x00);
        assert_eq!(sys2.board.map.region_data(Region::VectorRom)[0], 0x00);
    }

    #[test]
    fn earom_write_read() {
        let mut sys = AsteroidsDeluxeSystem::new();

        // Latch address 0x05 with data 0xAB
        sys.bus_write(BusMaster::Cpu(0), 0x3205, 0xAB);

        // Astdelux $3A00 bits: 0=CK, 1=!C1, 2=C2, 3=CS1

        // Erase address 5: C1=0(bit1=1), C2=1(bit2=1), CS1=1(bit3=1)
        sys.bus_write(BusMaster::Cpu(0), 0x3A00, 0x0F); // clock high
        sys.bus_write(BusMaster::Cpu(0), 0x3A00, 0x0E); // clock low

        // Write 0xAB: C1=0(bit1=1), C2=0(bit2=0), CS1=1(bit3=1)
        sys.bus_write(BusMaster::Cpu(0), 0x3A00, 0x0B); // clock high
        sys.bus_write(BusMaster::Cpu(0), 0x3A00, 0x0A); // clock low

        // Read: C1=1(bit1=0), CS1=1(bit3=1), falling edge loads data register
        sys.bus_write(BusMaster::Cpu(0), 0x3A00, 0x09); // clock high
        sys.bus_write(BusMaster::Cpu(0), 0x3A00, 0x08); // falling edge → read

        // Read data register
        let val = sys.bus_read(BusMaster::Cpu(0), 0x2C45);
        assert_eq!(val, 0xAB);
    }
}
