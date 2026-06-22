//! Konami Scramble sound board.
//!
//! Self-contained Z80 + 1-2×AY-8910 sound board used by Konami's Scramble-type
//! games (Scramble, Super Cobra, …). The main board talks to it through an 8255
//! PPI: port A latches a command byte, and port B's bit 3 (falling edge) pulses
//! the sound CPU's IRQ. The sound CPU reads the command and a free-running timer
//! through AY-8910 #0's input ports. A near-cousin of [`SsioBoard`], which uses
//! 4 direct latches instead of an 8255.
//!
//! [`SsioBoard`]: crate::device::ssio::SsioBoard
//!
//! # Hardware
//!
//! - Z80 CPU @ 14.318 MHz / 8 ≈ 1.79 MHz
//! - 1-2× AY-8910 PSG @ the same clock
//! - 8 KB sound ROM (0x0000–0x1FFF)
//! - 1 KB RAM (0x8000–0x83FF, mirrored)
//! - IRQ pulsed by the main board through the 8255 (auto-cleared when the sound
//!   CPU reads the command latch via AY0 port A)
//!
//! # Sound Z80 memory map
//!
//! | Address       | R/W | Description                          |
//! |---------------|-----|--------------------------------------|
//! | 0x0000–0x1FFF | R   | Sound ROM (8 KB)                     |
//! | 0x8000–0x83FF | R/W | RAM (1 KB, mirrored)                 |
//! | 0x9000–0x9FFF | W   | Discrete filter control (not modeled)|
//!
//! # Sound Z80 I/O map (`konami_ay8910_*`)
//!
//! | Port (A&) | R/W | Description           |
//! |-----------|-----|-----------------------|
//! | 0x10      | W   | AY1 address latch     |
//! | 0x20      | R/W | AY1 data              |
//! | 0x40      | W   | AY0 address latch     |
//! | 0x80      | R/W | AY0 data              |
//!
//! AY0 port A (input) = the command latch; AY0 port B (input) = the timer.

use crate::audio::AudioResampler;
use crate::bus_split;
use crate::core::debug::{DebugRegister, Debuggable};
use crate::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use crate::core::{Bus, BusMaster};
use crate::cpu::Cpu;
use crate::cpu::z80::Z80;
use crate::device::{Ay8910, I8255};

use super::Device;

/// Konami sound master clock (≈ NTSC colorburst × 4).
const KONAMI_SOUND_CLOCK: u64 = 14_318_000;
/// Sound CPU / AY-8910 clock: master / 8 ≈ 1.79 MHz.
const KONAMI_CPU_CLOCK: u64 = KONAMI_SOUND_CLOCK / 8;
/// Output audio sample rate.
const OUTPUT_SAMPLE_RATE: u64 = 44_100;

/// Timer period in master-clock counts: a chained 16·16·2·8·5·2 divider.
const TIMER_PERIOD: u32 = 16 * 16 * 2 * 8 * 5 * 2; // 40960
/// The point in the period where the final divide-by-2 high bit (B7) is set.
const TIMER_HALF: u32 = 16 * 16 * 2 * 8 * 5; // 20480

/// Konami Scramble sound board.
pub struct KonamiSound {
    // Sound CPU (Z80 @ ~1.79 MHz)
    cpu: Z80,

    // 1-2× AY-8910 PSGs (`num_ay` selects how many are populated)
    ay: [Ay8910; 2],
    num_ay: usize,

    // Memory
    rom: Vec<u8>,      // 8 KB sound ROM
    ram: [u8; 0x0400], // 1 KB RAM

    // Command/control interface from the main board (8255 PPI on the real
    // hardware): port A out -> command latch, port B out -> control byte.
    ppi: I8255,
    command: u8, // latched command (8255 port A output)
    control: u8, // 8255 port B output (bit 3 = IRQ clock, bit 4 = mute)

    // IRQ generation (HOLD_LINE-style: set on the control bit-3 falling edge,
    // cleared when the sound CPU reads the command latch).
    irq_pending: bool,
    mute: bool,

    // Discrete output-filter control word (latched, not modeled as audio).
    filter: u16,

    // Audio resampler (mixes the AY outputs)
    resampler: AudioResampler<i16>,

    // Total sound-CPU cycles (drives the timer).
    clock: u64,
}

impl KonamiSound {
    /// Create a board with `num_ay` AY-8910s (1 or 2). Call `load_rom` before
    /// use.
    pub fn new(num_ay: usize) -> Self {
        Self {
            cpu: Z80::new(),
            ay: [Ay8910::new(KONAMI_CPU_CLOCK), Ay8910::new(KONAMI_CPU_CLOCK)],
            num_ay: num_ay.clamp(1, 2),
            rom: vec![0; 0x2000],
            ram: [0; 0x0400],
            ppi: I8255::new(),
            command: 0,
            control: 0,
            irq_pending: false,
            mute: false,
            filter: 0,
            resampler: AudioResampler::new(KONAMI_CPU_CLOCK, OUTPUT_SAMPLE_RATE),
            clock: 0,
        }
    }

    /// Load sound ROM data (up to 8 KB).
    pub fn load_rom(&mut self, data: &[u8]) {
        let len = data.len().min(self.rom.len());
        self.rom[..len].copy_from_slice(&data[..len]);
    }

    // -----------------------------------------------------------------------
    // Main-board interface (the 8255 PPI command/control port)
    // -----------------------------------------------------------------------

    /// Write one of the four 8255 registers (`offset` 0=A, 1=B, 2=C, 3=control).
    /// Port A drives the command latch; port B's bit 3 falling edge pulses the
    /// sound CPU IRQ and bit 4 mutes the board.
    pub fn ppi_write(&mut self, offset: u16, data: u8) {
        self.ppi.write(offset, data);
        self.command = self.ppi.read_output_a();
        self.set_control(self.ppi.read_output_b());
    }

    /// Read one of the four 8255 registers (`offset` 0=A, 1=B, 2=C, 3=control).
    pub fn ppi_read(&self, offset: u16) -> u8 {
        self.ppi.read(offset)
    }

    /// Drive the 8255 port C input pins (a main-board input port, e.g. IN3).
    pub fn set_ppi_portc_input(&mut self, data: u8) {
        self.ppi.set_port_c_input(data);
    }

    /// Apply a new 8255 port-B (control) value: bit 3 high→low pulses the IRQ,
    /// bit 4 is the global mute.
    fn set_control(&mut self, data: u8) {
        let old = self.control;
        self.control = data;
        // The inverse of bit 3 clocks a flip-flop that asserts the sound IRQ.
        if old & 0x08 != 0 && data & 0x08 == 0 {
            self.irq_pending = true;
        }
        self.mute = data & 0x10 != 0;
    }

    /// The free-running timer presented on AY0 port B (`konami_sound_timer_r`):
    /// a chained divider whose top counter bits are mapped to B4–B7, with the
    /// unused low bits pulled high (B0 grounded).
    fn timer(&self) -> u8 {
        let mut cycles = ((self.clock * 8) % TIMER_PERIOD as u64) as u32;
        let hibit = if cycles >= TIMER_HALF {
            cycles -= TIMER_HALF;
            1u8
        } else {
            0
        };
        (hibit << 7)
            | (((cycles >> 14) & 1) as u8) << 6
            | (((cycles >> 13) & 1) as u8) << 5
            | (((cycles >> 11) & 1) as u8) << 4
            | 0x0e
    }

    // -----------------------------------------------------------------------
    // Tick (called at the sound-CPU clock rate)
    // -----------------------------------------------------------------------

    /// Advance the board by one sound-CPU clock (≈ 1.79 MHz): present the
    /// command/timer on AY0's input ports, run one Z80 cycle, tick the AYs, and
    /// accumulate audio.
    pub fn tick(&mut self) {
        // AY0 reads the command on port A and the timer on port B.
        self.ay[0].set_port_a(self.command);
        self.ay[0].set_port_b(self.timer());

        bus_split!(self, bus => {
            self.cpu.execute_cycle(bus, BusMaster::Cpu(0));
        });

        self.ay[0].tick();
        if self.num_ay > 1 {
            self.ay[1].tick();
        }

        let mut buf0 = [0i16; 1];
        let mut buf1 = [0i16; 1];
        let n0 = self.ay[0].fill_audio(&mut buf0);
        let n1 = if self.num_ay > 1 {
            self.ay[1].fill_audio(&mut buf1)
        } else {
            0
        };
        if n0 > 0 || n1 > 0 {
            let s0 = if n0 > 0 { buf0[0] as i32 } else { 0 };
            let s1 = if n1 > 0 { buf1[0] as i32 } else { 0 };
            let mixed = if self.mute {
                0
            } else {
                ((s0 + s1) / 2).clamp(-32767, 32767) as i16
            };
            self.resampler.push_sample(mixed);
        }

        self.clock += 1;
    }

    /// Drain accumulated audio samples. Returns the number written.
    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.resampler.fill_audio(buffer)
    }

    /// Reset the board to power-on state.
    pub fn reset(&mut self) {
        bus_split!(self, bus => {
            self.cpu.reset(bus, BusMaster::Cpu(0));
        });
        self.ay[0].reset();
        self.ay[1].reset();
        self.ppi.reset();
        self.ram = [0; 0x0400];
        self.command = 0;
        self.control = 0;
        self.irq_pending = false;
        self.mute = false;
        self.filter = 0;
        self.resampler.reset();
        self.clock = 0;
    }
}

// ---------------------------------------------------------------------------
// Bus implementation (sound Z80's memory + I/O map)
// ---------------------------------------------------------------------------

impl Bus for KonamiSound {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        match addr {
            0x0000..=0x1FFF => self.rom[addr as usize],
            0x8000..=0x8FFF => self.ram[(addr & 0x03FF) as usize],
            _ => 0xFF,
        }
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        match addr {
            0x8000..=0x8FFF => self.ram[(addr & 0x03FF) as usize] = data,
            // Discrete filter control: the *offset* carries the 12 filter bits
            // (6 per AY). Not modeled as audio; latched for debug/state.
            0x9000..=0x9FFF => self.filter = addr & 0x0FFF,
            _ => {}
        }
    }

    fn io_read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        // The port map is `global_mask(0xff)`, so only the low 8 bits decode.
        let port = addr & 0xFF;
        let mut result = 0xFF;
        if self.num_ay > 1 && port & 0x20 != 0 {
            result &= self.ay[1].data_read();
        }
        if port & 0x80 != 0 {
            // Reading AY0 port A (register 14) is the command latch fetch, which
            // acknowledges and clears the held IRQ.
            if self.ay[0].latched_register() == 14 {
                self.irq_pending = false;
            }
            result &= self.ay[0].data_read();
        }
        result
    }

    fn io_write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        let port = addr & 0xFF;
        // AV4,5 → AY1; AV6,7 → AY0 (both pairs can be addressed at once).
        if self.num_ay > 1 {
            if port & 0x10 != 0 {
                self.ay[1].address_write(data);
            } else if port & 0x20 != 0 {
                self.ay[1].data_write(data);
            }
        }
        if port & 0x40 != 0 {
            self.ay[0].address_write(data);
        } else if port & 0x80 != 0 {
            self.ay[0].data_write(data);
        }
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> crate::core::bus::InterruptState {
        crate::core::bus::InterruptState {
            nmi: false,
            irq: self.irq_pending,
            firq: false,
            irq_vector: 0xFF,
            irq_level: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Device trait
// ---------------------------------------------------------------------------

impl Device for KonamiSound {
    fn name(&self) -> &'static str {
        "Konami Sound"
    }

    fn reset(&mut self) {
        self.reset();
    }

    fn tick(&mut self) {
        self.tick();
    }
}

// ---------------------------------------------------------------------------
// Debug support
// ---------------------------------------------------------------------------

impl Debuggable for KonamiSound {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "COMMAND",
                value: self.command as u64,
                width: 8,
            },
            DebugRegister {
                name: "CONTROL",
                value: self.control as u64,
                width: 8,
            },
            DebugRegister {
                name: "IRQ",
                value: self.irq_pending as u64,
                width: 1,
            },
            DebugRegister {
                name: "MUTE",
                value: self.mute as u64,
                width: 1,
            },
            DebugRegister {
                name: "FILTER",
                value: self.filter as u64,
                width: 12,
            },
        ]
    }
}

// ---------------------------------------------------------------------------
// Save state support
// ---------------------------------------------------------------------------

impl Saveable for KonamiSound {
    fn save_state(&self, w: &mut StateWriter) {
        w.write_version(1);
        self.cpu.save_state(w);
        self.ay[0].save_state(w);
        self.ay[1].save_state(w);
        self.ppi.save_state(w);
        w.write_bytes(&self.ram);
        w.write_u8(self.command);
        w.write_u8(self.control);
        w.write_bool(self.irq_pending);
        w.write_bool(self.mute);
        w.write_u16_le(self.filter);
        self.resampler.save_state(w);
        w.write_u64_le(self.clock);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        r.read_version(1)?;
        self.cpu.load_state(r)?;
        self.ay[0].load_state(r)?;
        self.ay[1].load_state(r)?;
        self.ppi.load_state(r)?;
        r.read_bytes_into(&mut self.ram)?;
        self.command = r.read_u8()?;
        self.control = r.read_u8()?;
        self.irq_pending = r.read_bool()?;
        self.mute = r.read_bool()?;
        self.filter = r.read_u16_le()?;
        self.resampler.load_state(r)?;
        self.clock = r.read_u64_le()?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn bus_read(b: &mut KonamiSound, addr: u16) -> u8 {
        Bus::read(b, BusMaster::Cpu(0), addr)
    }
    fn bus_write(b: &mut KonamiSound, addr: u16, data: u8) {
        Bus::write(b, BusMaster::Cpu(0), addr, data);
    }
    fn io_read(b: &mut KonamiSound, addr: u16) -> u8 {
        Bus::io_read(b, BusMaster::Cpu(0), addr)
    }
    fn io_write(b: &mut KonamiSound, addr: u16, data: u8) {
        Bus::io_write(b, BusMaster::Cpu(0), addr, data);
    }

    /// Configure the 8255 (port A + B output) the way the main board does.
    fn init_ppi(b: &mut KonamiSound) {
        // Control word: mode 0, ports A & B output, port C input. 0x80 | 0x09.
        b.ppi_write(3, 0x89);
    }

    #[test]
    fn initial_state() {
        let b = KonamiSound::new(2);
        assert_eq!(b.command, 0);
        assert!(!b.irq_pending);
        assert!(!b.mute);
        assert_eq!(b.num_ay, 2);
    }

    #[test]
    fn command_latches_through_ppi_port_a() {
        let mut b = KonamiSound::new(2);
        init_ppi(&mut b);
        b.ppi_write(0, 0x5A); // 8255 port A = command
        assert_eq!(b.command, 0x5A);

        // tick() presents the command on AY0 port A; the sound CPU reads it by
        // latching register 14 (port A, an input — R7 bit 6 = 0 at reset).
        b.rom[0] = 0x76; // HALT
        b.tick();
        io_write(&mut b, 0x40, 14); // AY0 address latch = register 14
        assert_eq!(io_read(&mut b, 0x80), 0x5A);
    }

    #[test]
    fn control_bit3_falling_edge_pulses_irq() {
        let mut b = KonamiSound::new(2);
        init_ppi(&mut b);
        // Raise bit 3, then drop it: the high→low edge asserts the IRQ.
        b.ppi_write(1, 0x08);
        assert!(!b.irq_pending, "no IRQ on the rising edge");
        b.ppi_write(1, 0x00);
        assert!(b.irq_pending, "IRQ asserted on the falling edge");
    }

    #[test]
    fn irq_clears_when_command_latch_is_read() {
        let mut b = KonamiSound::new(2);
        b.irq_pending = true;
        // Select AY0 port A (register 14) as the read target, then read it.
        io_write(&mut b, 0x40, 14);
        let _ = io_read(&mut b, 0x80);
        assert!(!b.irq_pending, "reading the command latch acks the IRQ");
    }

    #[test]
    fn irq_not_cleared_by_reading_other_ay_register() {
        let mut b = KonamiSound::new(2);
        b.irq_pending = true;
        // Reading the timer (port B, register 15) must not ack the command IRQ.
        io_write(&mut b, 0x40, 15);
        let _ = io_read(&mut b, 0x80);
        assert!(b.irq_pending, "timer read must not clear the command IRQ");
    }

    #[test]
    fn control_bit4_mutes() {
        let mut b = KonamiSound::new(2);
        init_ppi(&mut b);
        b.ppi_write(1, 0x10);
        assert!(b.mute);
        b.ppi_write(1, 0x00);
        assert!(!b.mute);
    }

    #[test]
    fn ram_read_write_with_mirror() {
        let mut b = KonamiSound::new(2);
        bus_write(&mut b, 0x8000, 0x55);
        assert_eq!(bus_read(&mut b, 0x8000), 0x55);
        assert_eq!(bus_read(&mut b, 0x8400), 0x55); // 1 KB mirror
    }

    #[test]
    fn ay_register_write_through_io() {
        let mut b = KonamiSound::new(2);
        // AY0: address (port 0x40) = reg 8, data (port 0x80) = 0x0F.
        io_write(&mut b, 0x40, 8);
        io_write(&mut b, 0x80, 0x0F);
        io_write(&mut b, 0x40, 8);
        assert_eq!(io_read(&mut b, 0x80), 0x0F);
        // AY1: address (0x10) = reg 8, data (0x20) = 0x1F.
        io_write(&mut b, 0x10, 8);
        io_write(&mut b, 0x20, 0x1F);
        io_write(&mut b, 0x10, 8);
        assert_eq!(io_read(&mut b, 0x20), 0x1F);
    }

    #[test]
    fn single_ay_ignores_second_chip() {
        let mut b = KonamiSound::new(1);
        // Writes to AY1 (ports 0x10/0x20) are ignored; reads return 0xFF.
        io_write(&mut b, 0x10, 8);
        io_write(&mut b, 0x20, 0x1F);
        assert_eq!(io_read(&mut b, 0x20), 0xFF);
    }

    #[test]
    fn timer_advances_and_is_bounded() {
        let mut b = KonamiSound::new(2);
        b.rom[0] = 0x76; // HALT, so the CPU doesn't run off into garbage
        let t0 = b.timer();
        for _ in 0..6000 {
            b.tick();
        }
        let t1 = b.timer();
        assert_ne!(t0, t1, "timer should advance");
        // B0 is grounded, B1-B3 pulled high.
        assert_eq!(b.timer() & 0x0f, 0x0e);
    }

    #[test]
    fn reset_clears_state() {
        let mut b = KonamiSound::new(2);
        b.command = 0xFF;
        b.irq_pending = true;
        b.mute = true;
        b.clock = 1234;
        b.reset();
        assert_eq!(b.command, 0);
        assert!(!b.irq_pending);
        assert!(!b.mute);
        assert_eq!(b.clock, 0);
    }

    #[test]
    fn save_load_round_trip() {
        let mut b = KonamiSound::new(2);
        init_ppi(&mut b);
        b.ppi_write(0, 0x42);
        b.irq_pending = true;
        b.clock = 9876;
        b.filter = 0x123;

        let mut w = StateWriter::new();
        b.save_state(&mut w);
        let data = w.into_vec();

        let mut b2 = KonamiSound::new(2);
        let mut r = StateReader::new(&data);
        b2.load_state(&mut r).unwrap();
        assert_eq!(b2.command, 0x42);
        assert!(b2.irq_pending);
        assert_eq!(b2.clock, 9876);
        assert_eq!(b2.filter, 0x123);
    }
}
