//! Data East `btime.cpp` board (shared hardware).
//!
//! `BtimeBoard` models the hardware common to every game on Data East's
//! `btime.cpp` family (Burgertime, Bump'n'Jump, Lock'n'Chase, Zoar,
//! Disco No.1, …). Per-game wrappers (see `burgertime.rs`) own a `board`
//! field plus a [`BtimeConfig`] describing the variation points and forward
//! the `MachineCore`/capability traits to the board.
//!
//! Pass 1 is **video first, audio stubbed**: the main DECO CPU-7 (an NMOS
//! 6502 with runtime opcode encryption), the memory map, and video/input state
//! live here. The sound M6502 + 2× AY-3-8910 are deferred (see the Burgertime
//! epic §10); the sound-latch write is stored but otherwise inert.
//!
//! This file is the initial scaffold (issue `burgertime-z6c.1`). The DECO CPU-7
//! opcode decryption and X/Y-swap sprite-RAM mirror land in `.2`, GFX decode and
//! the `BGR_233_inverted` palette in `.3`, and the full renderer in `.4`.

use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::m6502::M6502;
use phosphor_macros::{BusDebug, MemoryRegion};

#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
enum Region {
    /// Main-CPU program ROM at 0xB000-0xFFFF (0xB000-0xBFFF is an unused gap;
    /// the physical ROMs sit at 0xC000-0xFFFF with the vectors at 0xFFFA-0xFFFF).
    Main = 1,
}

// ---------------------------------------------------------------------------
// Timing
// ---------------------------------------------------------------------------
// Main CPU: 12 MHz / 2 / 2 / 2 = 1.5 MHz.
// Screen (btime.cpp `set_raw`): pixel clock 6 MHz, HTOTAL 384, VTOTAL 272,
// visible 240x240, orientation ROT270. Frame rate: 6e6 / (384 * 272) ≈ 57.44 Hz.
// CPU cycles per scanline: 1_500_000 / (57.44 * 272) ≈ 96.
pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 1_500_000, // 12 MHz / 8
    cycles_per_scanline: 96, // HTOTAL 384 pixel clocks / 4
    total_scanlines: 272,    // VTOTAL
    display_width: 240,      // visible width (post-ROT270)
    display_height: 240,     // visible height
};

// ---------------------------------------------------------------------------
// Per-game configuration
// ---------------------------------------------------------------------------

/// Per-game configuration for the shared `btime.cpp` board.
///
/// Only Burgertime's variant is implemented in this pass. Sibling games differ
/// in their opcode encryption (DECO CPU-7 vs. CPU-6/222 vs. none), palette
/// source (decoded `BGR_233_inverted` vs. PROM), background-tilemap presence,
/// and audio NMI wiring; those become fields here as each sibling is added.
pub struct BtimeConfig {
    /// Machine id — also the save-state tag and CLI name.
    pub name: &'static str,
}

// ---------------------------------------------------------------------------
// BtimeBoard
// ---------------------------------------------------------------------------

/// Shared Data East `btime.cpp` hardware (Burgertime configuration in pass 1).
///
/// Memory map (main CPU):
///   0x0000-0x07FF  Work RAM
///   0x0C00-0x0C0F  Palette RAM (16 entries)
///   0x1000-0x13FF  Video RAM (char codes; sprite RAM aliased into column 0)
///   0x1400-0x17FF  Color RAM
///   0x1800-0x1BFF  Video RAM via X/Y-swap mirror (added in `.2`)
///   0x1C00-0x1FFF  Color RAM via X/Y-swap mirror (added in `.2`)
///   0x4000         IN0 (P1)      0x4001  IN1 (P2)      0x4002  system
///   0x4003         DSW1 (bit7 = live VBLANK)           0x4004  DSW2
///   0xB000-0xFFFF  Program ROM
#[derive(BusDebug)]
pub struct BtimeBoard {
    #[debug_cpu("M6502 (DECO CPU-7)")]
    pub(crate) cpu: M6502,

    #[debug_map(cpu = 0)]
    pub(crate) main_map: AddressSpace16,

    // Work / video memory (kept as flat arrays, not in the AddressSpace16).
    ram: [u8; 0x0800],
    videoram: [u8; 0x0400],
    colorram: [u8; 0x0400],
    palette_ram: [u8; 16],
    /// RGB expansion of `palette_ram` (rebuilt on palette writes in `.3`).
    palette_rgb: [(u8, u8, u8); 16],

    // DECO CPU-7 decryption state: any main-CPU write arms decryption of the
    // next opcode fetch. The decrypt itself is wired in `.2`.
    main_had_written: bool,

    // I/O latches
    pub(crate) main_irq: bool, // coin-insertion IRQ (HOLD_LINE approximation)
    flip_screen: bool,         // 0x4002 write bit0
    bnj_scroll0: u8,           // 0x4004 write (bit4 -> background enable)
    sound_latch: u8,           // 0x4003 write — stored; sound CPU/IRQ deferred (§10)

    // Input ports (active-low players, active-high coins) and DIP banks.
    // Mutated directly by the wrapper's `handle_input` (same-crate access, per
    // the joust.rs pattern).
    pub(crate) p1: u8,
    pub(crate) p2: u8,
    pub(crate) system: u8,
    dsw1: u8,
    dsw2: u8,

    // Per-game configuration (identity + future variation points).
    config: BtimeConfig,

    clock: u64,
}

impl BtimeBoard {
    pub fn new(config: BtimeConfig) -> Self {
        let mut main_map = AddressSpace16::new();
        main_map.region(
            Region::Main,
            "Program ROM",
            0xB000,
            0x5000,
            AccessKind::ReadOnly,
        );

        Self {
            cpu: M6502::new(),
            main_map,
            ram: [0; 0x0800],
            videoram: [0; 0x0400],
            colorram: [0; 0x0400],
            palette_ram: [0; 16],
            palette_rgb: [(0, 0, 0); 16],
            main_had_written: false,
            main_irq: false,
            flip_screen: false,
            bnj_scroll0: 0,
            sound_latch: 0,
            // Players idle (active-low = all bits high).
            p1: 0xFF,
            p2: 0xFF,
            // Start/tilt idle high (bits 0-2), coin bits (6-7) low.
            system: 0x07,
            // DSW defaults refined with the full bank tables in `.5`.
            // dsw1 bit4 ("Leave Off") must be set or boot locks; bit7 excluded
            // (live VBLANK, injected on read).
            dsw1: 0x10,
            dsw2: 0x00,
            config,
            clock: 0,
        }
    }

    /// Machine id (identity comes from the per-game [`BtimeConfig`]).
    pub fn machine_id(&self) -> &str {
        self.config.name
    }

    /// Load the assembled main-CPU program ROM (region base 0xB000; the physical
    /// ROMs occupy 0xC000-0xFFFF, so `data` is 0x5000 bytes with a 0x1000 gap).
    pub fn load_main_rom(&mut self, data: &[u8]) {
        self.main_map.load_region(Region::Main, data);
    }

    // --- Core tick ---

    pub fn tick(&mut self, bus: &mut dyn Bus<Address = u16, Data = u8>) {
        if self.main_map.has_any_watchpoints() {
            let pc = self
                .cpu
                .at_instruction_boundary()
                .then_some(self.cpu.pc as u32);
            self.main_map.latch_access_context(self.clock, pc);
        }

        self.cpu.execute_cycle(bus, BusMaster::Cpu(0));
        self.clock += 1;
    }

    pub fn reset(&mut self) {
        self.main_had_written = false;
        self.main_irq = false;
        self.clock = 0;
        // CPU reset is driven by the wrapper via `bus_split!` (Bus lives there).
    }

    /// Returns a bitmask of CPUs at instruction boundaries. Bit 0 = main CPU.
    pub fn debug_tick_boundaries(&self) -> u32 {
        if self.cpu.at_instruction_boundary() {
            1
        } else {
            0
        }
    }

    // --- Capability-trait helpers (called by the game wrapper) ---

    /// Render the visible frame. Pass 1 clears to the backdrop color; the full
    /// char/sprite/background renderer with ROT270 lands in `.4`.
    pub fn render_frame(&self, buffer: &mut [u8]) {
        let (r, g, b) = self.palette_rgb[0];
        for px in buffer.chunks_exact_mut(3) {
            px[0] = r;
            px[1] = g;
            px[2] = b;
        }
    }

    // --- Bus (main CPU only in pass 1; sound CPU added in §10) ---

    pub(crate) fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        let a = addr as usize;
        let data = match addr {
            0x0000..=0x07FF => self.ram[a & 0x07FF],
            0x0C00..=0x0C0F => self.palette_ram[a & 0x0F],
            0x1000..=0x13FF => self.videoram[a & 0x03FF],
            0x1400..=0x17FF => self.colorram[a & 0x03FF],
            // 0x1800/0x1C00 X/Y-swap mirrors: wired in `.2`.
            0x4000 => self.p1,
            0x4001 => self.p2,
            0x4002 => self.system,
            // 0x4003 bit7 is a live VBLANK bit injected on read in `.5`.
            0x4003 => self.dsw1 & 0x7F,
            0x4004 => self.dsw2,
            0xB000..=0xFFFF => self.main_map.read_backing(addr),
            _ => 0,
        };
        // DECO CPU-7 opcode decryption hook is added in `.2`.
        self.main_map.watch_read(0, master, addr, data);
        data
    }

    pub(crate) fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        self.main_map.watch_write(0, master, addr, data);
        // Any main-CPU write arms DECO CPU-7 decryption of the next opcode fetch.
        self.main_had_written = true;

        let a = addr as usize;
        match addr {
            0x0000..=0x07FF => self.ram[a & 0x07FF] = data,
            0x0C00..=0x0C0F => self.palette_ram[a & 0x0F] = data,
            0x1000..=0x13FF => self.videoram[a & 0x03FF] = data,
            0x1400..=0x17FF => self.colorram[a & 0x03FF] = data,
            // 0x1800/0x1C00 X/Y-swap mirrors: wired in `.2`.
            0x4002 => self.flip_screen = data & 1 != 0,
            0x4003 => self.sound_latch = data, // sound CPU/IRQ deferred (§10)
            0x4004 => self.bnj_scroll0 = data,
            _ => {}
        }
    }

    pub(crate) fn bus_is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    pub(crate) fn bus_check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        InterruptState {
            nmi: false,
            irq: self.main_irq,
            firq: false,
            irq_vector: 0,
            irq_level: 0,
        }
    }
}

impl Saveable for BtimeBoard {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        w.write_bytes(&self.ram);
        w.write_bytes(&self.videoram);
        w.write_bytes(&self.colorram);
        w.write_bytes(&self.palette_ram);
        w.write_bool(self.main_had_written);
        w.write_bool(self.main_irq);
        w.write_bool(self.flip_screen);
        w.write_u8(self.bnj_scroll0);
        w.write_u8(self.sound_latch);
        w.write_u8(self.p1);
        w.write_u8(self.p2);
        w.write_u8(self.system);
        w.write_u8(self.dsw1);
        w.write_u8(self.dsw2);
        w.write_u64_le(self.clock);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        r.read_bytes_into(&mut self.ram)?;
        r.read_bytes_into(&mut self.videoram)?;
        r.read_bytes_into(&mut self.colorram)?;
        r.read_bytes_into(&mut self.palette_ram)?;
        self.main_had_written = r.read_bool()?;
        self.main_irq = r.read_bool()?;
        self.flip_screen = r.read_bool()?;
        self.bnj_scroll0 = r.read_u8()?;
        self.sound_latch = r.read_u8()?;
        self.p1 = r.read_u8()?;
        self.p2 = r.read_u8()?;
        self.system = r.read_u8()?;
        self.dsw1 = r.read_u8()?;
        self.dsw2 = r.read_u8()?;
        self.clock = r.read_u64_le()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> BtimeBoard {
        BtimeBoard::new(BtimeConfig { name: "btime-test" })
    }

    #[test]
    fn timing_frame_rate_is_btime() {
        // ~57.44 Hz from 6 MHz / (384 * 272).
        let hz = TIMING.frame_rate_hz();
        assert!((hz - 57.44).abs() < 0.5, "frame rate {hz} not ~57.44");
        assert_eq!(TIMING.cycles_per_frame(), 96 * 272);
    }

    #[test]
    fn ram_read_write_roundtrip() {
        let mut b = board();
        b.bus_write(BusMaster::Cpu(0), 0x0042, 0xAB);
        assert_eq!(b.bus_read(BusMaster::Cpu(0), 0x0042), 0xAB);
    }

    #[test]
    fn video_and_color_ram_roundtrip() {
        let mut b = board();
        b.bus_write(BusMaster::Cpu(0), 0x1005, 0x12);
        b.bus_write(BusMaster::Cpu(0), 0x1405, 0x34);
        assert_eq!(b.bus_read(BusMaster::Cpu(0), 0x1005), 0x12);
        assert_eq!(b.bus_read(BusMaster::Cpu(0), 0x1405), 0x34);
    }

    #[test]
    fn any_write_arms_deco_decryption() {
        let mut b = board();
        assert!(!b.main_had_written);
        b.bus_write(BusMaster::Cpu(0), 0x0000, 0x00);
        assert!(b.main_had_written);
    }

    #[test]
    fn coin_irq_reported_through_interrupts() {
        let mut b = board();
        assert!(!b.bus_check_interrupts(BusMaster::Cpu(0)).irq);
        b.main_irq = true;
        assert!(b.bus_check_interrupts(BusMaster::Cpu(0)).irq);
    }
}
