//! Midway SSIO (Super Sound I/O) sound board.
//!
//! # Schematics
//!
//! | Drawing | Source | Pages |
//! |---|---|---|
//! | `SCHEMATIC DRAWING, SUPER SOUND I/O`, `A084-90913-E000`, sheet 9-15 | `arcade-museum.com/manuals-videogames/T/Tron.pdf` | PDF p128 |
//!
//! Transcribed in
//! [`docs/schematics/ssio-audio-output.md`](../../../docs/schematics/ssio-audio-output.md).
//! Read from Tron's manual because the board is shared across MCR II and no
//! Satan's Hollow scan was located; sheet numbers differ per manual, so the
//! part number is the stable name.
//!
//! **The analog half of this board is not modelled.** The duty-cycle volume is
//! an analog chopper with RC smoothing, where [`DUTY_CYCLE_VOLUME`] below is a
//! lookup of its average; the six channels sum through 13k legs into a 27k
//! amplifier rather than as `(s0 + s1) / 2`; and the board's output is stereo
//! where this is mono. See `phosphor-emulator-cg2f`.
//!
//! Self-contained Z80 + 2×AY-8910 sound board used across Midway's MCR I, II,
//! and III arcade platforms. The main CPU communicates via 4-byte command latches
//! and a status byte. Input ports (coins, joystick, DIP switches) are also routed
//! through the SSIO board.
//!
//! # Hardware
//!
//! - Z80 CPU @ 2 MHz (16 MHz / 8)
//! - 2× AY-8910 PSG @ 2 MHz
//! - 16 KB sound ROM (0x0000–0x3FFF)
//! - 1 KB RAM (0x8000–0x83FF, mirrored)
//! - IRQ from 14024 counter chain (~781 Hz)
//!
//! # SSIO Z80 memory map
//!
//! | Address       | R/W | Description                      |
//! |---------------|-----|----------------------------------|
//! | 0x0000–0x3FFF | R   | Sound ROM (16 KB)                |
//! | 0x8000–0x83FF | R/W | RAM (1 KB, mirrored to 0x8FFF)   |
//! | 0x9000–0x9003 | R   | Command latches from main CPU    |
//! | 0xA000        | W   | AY0 address latch                |
//! | 0xA001        | R   | AY0 data read                    |
//! | 0xA002        | W   | AY0 data write                   |
//! | 0xB000        | W   | AY1 address latch                |
//! | 0xB001        | R   | AY1 data read                    |
//! | 0xB002        | W   | AY1 data write                   |
//! | 0xC000–0xCFFF | W   | Status register (main CPU reads) |
//! | 0xD000–0xDFFF | W   | LED control (ignored)            |
//! | 0xE000–0xEFFF | R   | IRQ acknowledge/clear            |
//! | 0xF000–0xFFFF | R   | DIP switches                     |

use crate::audio::{AudioResampler, host_sample_rate};
use crate::core::debug::{DebugRegister, Debuggable};
use crate::core::{Bus, BusMaster};
use crate::cpu::Cpu;
use crate::cpu::z80::Z80;
use crate::device::Ay8910;
use phosphor_macros::Saveable;

use super::Device;

/// SSIO CPU clock: 16 MHz / 8 = 2 MHz.
const SSIO_CLOCK_HZ: u64 = 2_000_000;

/// IRQ interval in SSIO CPU ticks.
///
/// The 14024 7-bit counter is clocked at ~50 kHz (16 MHz / 2 / 160).
/// IRQ fires when bit 6 changes, i.e. every 64 counts at 50 kHz = ~781 Hz.
/// At 2 MHz CPU clock: 2,000,000 / 781.25 ≈ 2560 ticks between IRQs.
const IRQ_INTERVAL: u32 = 2560;

/// Duty-cycle volume lookup table.
///
/// Maps 4-bit duty-cycle register values (0–15) to an 8-bit gain (0–255).
/// Computed from the 82S123 PROM at U12D using MAME's
/// `compute_ay8910_modulation()` algorithm: for each register value, count
/// high→low transitions in the PROM's 160-bit waveform to determine the
/// duty-cycle fraction. Index 0 = maximum volume, index 15 = silence.
const DUTY_CYCLE_VOLUME: [u8; 16] = [
    255, 255, 255, 255, 244, 241, 236, 231, 223, 214, 199, 179, 151, 115, 65, 0,
];

/// Midway SSIO sound board.
///
/// Implements `Bus` for the internal Z80 CPU's memory/IO map, and provides
/// methods for the main board to write command latches and read status/inputs.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct SsioBoard {
    // Sound CPU (Z80 @ 2 MHz)
    #[save(id = 1)]
    cpu: Z80,
    /// Everything the sound CPU talks to. Held apart from the CPU so a cycle
    /// dispatches at a concrete bus rather than a trait object -- see
    /// `docs/designs/concrete-bus-dispatch.md`.
    #[save(id = 2)]
    bus: SsioBus,
}

/// The sound Z80's bus: the two PSGs, memory, and the main-board latches.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
struct SsioBus {
    // 2× AY-8910 PSGs
    #[save(id = 1)]
    ay: [Ay8910; 2],

    // Memory
    /// The sound ROM, which `load_rom` puts back.
    #[save_skip]
    rom: Vec<u8>, // 16 KB sound ROM
    #[save(id = 2)]
    ram: [u8; 0x0400], // 1 KB RAM

    // Communication with main CPU
    #[save(id = 3)]
    data_latch: [u8; 4], // Command latches (main CPU writes, SSIO reads)
    #[save(id = 4)]
    status: u8, // Status byte (SSIO writes, main CPU reads)

    // Input port routing (main CPU reads through SSIO)
    #[save(id = 5)]
    input_ports: [u8; 5], // IP0–IP4 (active-low, idle = 0xFF)
    #[save(id = 6)]
    dip_switches: u8,

    // IRQ generation
    #[save(id = 7)]
    irq_counter: u32,
    #[save(id = 8)]
    irq_pending: bool,

    // Duty-cycle volume modulation
    #[save(id = 9)]
    duty_cycle: [[u8; 3]; 2], // Per-AY, per-channel (4-bit values)
    #[save(id = 10)]
    overall: [u8; 2], // Per-AY overall volume (3-bit)
    #[save(id = 11)]
    mute: bool,

    // Audio resampler (mixes both AY outputs)
    #[save(id = 12)]
    resampler: AudioResampler<i16>,

    // Clock state
    #[save(id = 13)]
    clock: u64,
}

impl SsioBoard {
    /// Create a new SSIO board. Call `load_rom()` before use.
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        Self {
            cpu: Z80::new(),
            bus: SsioBus {
                ay: [Ay8910::new(SSIO_CLOCK_HZ), Ay8910::new(SSIO_CLOCK_HZ)],
                rom: vec![0; 0x4000],
                ram: [0; 0x0400],
                data_latch: [0; 4],
                status: 0,
                input_ports: [0xFF; 5],
                dip_switches: 0,
                irq_counter: 0,
                irq_pending: false,
                duty_cycle: [[0; 3]; 2],
                overall: [0; 2],
                mute: false,
                resampler: AudioResampler::new(SSIO_CLOCK_HZ, host_sample_rate() as u64),
                clock: 0,
            },
        }
    }

    /// Load sound ROM data. `data` should be up to 16 KB.
    pub fn load_rom(&mut self, data: &[u8]) {
        let len = data.len().min(self.bus.rom.len());
        self.bus.rom[..len].copy_from_slice(&data[..len]);
    }

    // -----------------------------------------------------------------------
    // Main CPU interface (called by the MCR board)
    // -----------------------------------------------------------------------

    /// Write a command byte to one of the 4 latches (main CPU → SSIO).
    ///
    /// `latch` is 0–3, corresponding to addresses 0x1C–0x1F in the MCR I/O map.
    pub fn latch_write(&mut self, latch: u8, data: u8) {
        self.bus.data_latch[(latch & 3) as usize] = data;
    }

    /// Read the status byte (SSIO → main CPU).
    pub fn status_read(&self) -> u8 {
        self.bus.status
    }

    /// Read an input port value. `port` is 0–4.
    pub fn input_port(&self, port: usize) -> u8 {
        if port < 5 {
            self.bus.input_ports[port]
        } else {
            0xFF
        }
    }

    /// Set an input port value. `port` is 0–4.
    pub fn set_input_port(&mut self, port: usize, value: u8) {
        if port < 5 {
            self.bus.input_ports[port] = value;
        }
    }

    /// Set the DIP switch register.
    pub fn set_dip_switches(&mut self, value: u8) {
        self.bus.dip_switches = value;
    }

    // -----------------------------------------------------------------------
    // Tick (called at the SSIO CPU clock rate)
    // -----------------------------------------------------------------------

    /// Advance the SSIO board by one CPU tick (at 2 MHz).
    ///
    /// Runs the Z80 and both AY-8910s, handles IRQ generation, and
    /// accumulates audio.
    pub fn tick(&mut self) {
        // IRQ generation
        self.bus.irq_counter += 1;
        if self.bus.irq_counter >= IRQ_INTERVAL {
            self.bus.irq_counter = 0;
            self.bus.irq_pending = true;
        }

        // Execute one Z80 cycle
        self.cpu.execute_cycle(&mut self.bus, BusMaster::Cpu(0));

        // Tick both AY-8910s
        self.bus.ay[0].tick();
        self.bus.ay[1].tick();

        // Audio resampling: mix both AY outputs
        let mut buf0 = [0i16; 1];
        let mut buf1 = [0i16; 1];
        let n0 = self.bus.ay[0].fill_audio(&mut buf0);
        let n1 = self.bus.ay[1].fill_audio(&mut buf1);

        // When either AY produces a sample, mix and push through the resampler.
        // Both AYs run at the same clock, so they produce samples in lockstep.
        if n0 > 0 || n1 > 0 {
            let s0 = if n0 > 0 { buf0[0] as i32 } else { 0 };
            let s1 = if n1 > 0 { buf1[0] as i32 } else { 0 };
            let mixed = if self.bus.mute {
                0
            } else {
                ((s0 + s1) / 2).clamp(-32767, 32767) as i16
            };
            self.bus.resampler.push_sample(mixed);
        }

        self.bus.clock += 1;
    }

    /// Drain accumulated audio samples into the provided buffer.
    /// Returns the number of samples written.
    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.bus.resampler.fill_audio(buffer)
    }

    /// Reset the SSIO board to power-on state.
    pub fn reset(&mut self) {
        self.cpu.reset(&mut self.bus, BusMaster::Cpu(0));
        self.bus.ay[0].reset();
        self.bus.ay[1].reset();
        self.bus.ram = [0; 0x0400];
        self.bus.data_latch = [0; 4];
        self.bus.status = 0;
        self.bus.irq_counter = 0;
        self.bus.irq_pending = false;
        self.bus.duty_cycle = [[0; 3]; 2];
        self.bus.overall = [0; 2];
        self.bus.mute = false;
        self.bus.resampler.reset();
        self.bus.clock = 0;
    }
}

// ---------------------------------------------------------------------------
// Bus implementation (SSIO Z80's memory/IO map)
// ---------------------------------------------------------------------------

impl SsioBus {
    // -----------------------------------------------------------------------
    // AY-8910 port callbacks (duty-cycle volume control)
    // -----------------------------------------------------------------------

    /// Process AY-8910 port writes for duty-cycle volume modulation.
    ///
    /// Called after data_write to the AY when the target register is 14 or 15
    /// (Port A or Port B output). Updates per-channel gain on the AY chips.
    fn update_duty_cycle_volumes(&mut self, ay_idx: usize) {
        let port_a = self.ay[ay_idx].port_a_read();
        let port_b = self.ay[ay_idx].port_b_read();

        // Port A: channel 0 duty = low nibble, channel 1 duty = high nibble
        self.duty_cycle[ay_idx][0] = port_a & 0x0F;
        self.duty_cycle[ay_idx][1] = (port_a >> 4) & 0x0F;

        // Port B: channel 2 duty = low nibble, overall = bits 4-6
        self.duty_cycle[ay_idx][2] = port_b & 0x0F;
        self.overall[ay_idx] = (port_b >> 4) & 0x07;

        // AY1 port B bit 7 controls global mute
        if ay_idx == 1 {
            self.mute = port_b & 0x80 != 0;
        }

        // Gain comes purely from the PROM-derived duty-cycle table + mute flag.
        // Overall volume is stored but NOT used (matches MAME behavior).
        for ch in 0..3 {
            let gain = if self.mute {
                0
            } else {
                DUTY_CYCLE_VOLUME[self.duty_cycle[ay_idx][ch] as usize]
            };
            self.ay[ay_idx].set_channel_gain(ch, gain);
        }
    }
}

impl Bus for SsioBus {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        match addr {
            // ROM: 0x0000–0x3FFF
            0x0000..=0x3FFF => self.rom[addr as usize],

            // RAM: 0x8000–0x8FFF (1 KB mirrored)
            0x8000..=0x8FFF => self.ram[(addr & 0x03FF) as usize],

            // Command latches: 0x9000–0x9FFF (4 latches mirrored)
            0x9000..=0x9FFF => self.data_latch[(addr & 0x03) as usize],

            // AY0 data read: 0xA000–0xAFFF, offset 1
            0xA000..=0xAFFF if (addr & 0x03) == 1 => self.ay[0].data_read(),

            // AY1 data read: 0xB000–0xBFFF, offset 1
            0xB000..=0xBFFF if (addr & 0x03) == 1 => self.ay[1].data_read(),

            // IRQ acknowledge: 0xE000–0xEFFF
            0xE000..=0xEFFF => {
                self.irq_pending = false;
                0xFF
            }

            // DIP switches: 0xF000–0xFFFF
            0xF000..=0xFFFF => self.dip_switches,

            _ => 0xFF,
        }
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        match addr {
            // RAM: 0x8000–0x8FFF (1 KB mirrored)
            0x8000..=0x8FFF => self.ram[(addr & 0x03FF) as usize] = data,

            // AY0: 0xA000–0xAFFF
            0xA000..=0xAFFF => match addr & 0x03 {
                0 => self.ay[0].address_write(data),
                2 => {
                    self.ay[0].data_write(data);
                    self.update_duty_cycle_volumes(0);
                }
                _ => {}
            },

            // AY1: 0xB000–0xBFFF
            0xB000..=0xBFFF => match addr & 0x03 {
                0 => self.ay[1].address_write(data),
                2 => {
                    self.ay[1].data_write(data);
                    self.update_duty_cycle_volumes(1);
                }
                _ => {}
            },

            // Status write: 0xC000–0xCFFF
            0xC000..=0xCFFF => self.status = data,

            // LED control: 0xD000–0xDFFF (ignored)
            0xD000..=0xDFFF => {}

            _ => {}
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

impl Device for SsioBoard {
    fn name(&self) -> &'static str {
        "SSIO"
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

impl Debuggable for SsioBoard {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "STATUS",
                value: self.bus.status as u64,
                width: 8,
            },
            DebugRegister {
                name: "LATCH0",
                value: self.bus.data_latch[0] as u64,
                width: 8,
            },
            DebugRegister {
                name: "LATCH1",
                value: self.bus.data_latch[1] as u64,
                width: 8,
            },
            DebugRegister {
                name: "LATCH2",
                value: self.bus.data_latch[2] as u64,
                width: 8,
            },
            DebugRegister {
                name: "LATCH3",
                value: self.bus.data_latch[3] as u64,
                width: 8,
            },
            DebugRegister {
                name: "IRQ_CTR",
                value: self.bus.irq_counter as u64,
                width: 16,
            },
            DebugRegister {
                name: "IRQ",
                value: self.bus.irq_pending as u64,
                width: 1,
            },
            DebugRegister {
                name: "MUTE",
                value: self.bus.mute as u64,
                width: 1,
            },
        ]
    }
}

// ---------------------------------------------------------------------------
// Save state support
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::save_state::{Saveable, StateReader, StateWriter};

    /// Helper: call Bus::read (disambiguates from Device::read).
    fn bus_read(ssio: &mut SsioBoard, addr: u16) -> u8 {
        Bus::read(&mut ssio.bus, BusMaster::Cpu(0), addr)
    }

    /// Helper: call Bus::write (disambiguates from Device::write).
    fn bus_write(ssio: &mut SsioBoard, addr: u16, data: u8) {
        Bus::write(&mut ssio.bus, BusMaster::Cpu(0), addr, data);
    }

    #[test]
    fn initial_state() {
        let ssio = SsioBoard::new();
        assert_eq!(ssio.bus.status, 0);
        assert!(!ssio.bus.irq_pending);
        assert!(!ssio.bus.mute);
        assert_eq!(ssio.bus.data_latch, [0; 4]);
        for port in &ssio.bus.input_ports {
            assert_eq!(*port, 0xFF);
        }
    }

    #[test]
    fn latch_write_read() {
        let mut ssio = SsioBoard::new();
        ssio.latch_write(0, 0x42);
        ssio.latch_write(3, 0xAB);

        // SSIO Z80 would read these at 0x9000–0x9003
        assert_eq!(ssio.bus.data_latch[0], 0x42);
        assert_eq!(ssio.bus.data_latch[3], 0xAB);

        // Verify through Bus::read
        assert_eq!(bus_read(&mut ssio, 0x9000), 0x42);
        assert_eq!(bus_read(&mut ssio, 0x9003), 0xAB);
    }

    #[test]
    fn ram_read_write_with_mirror() {
        let mut ssio = SsioBoard::new();
        bus_write(&mut ssio, 0x8000, 0x55);
        assert_eq!(bus_read(&mut ssio, 0x8000), 0x55);
        // Mirrored: 0x8400 maps to same location as 0x8000
        assert_eq!(bus_read(&mut ssio, 0x8400), 0x55);
        assert_eq!(bus_read(&mut ssio, 0x8800), 0x55);
    }

    #[test]
    fn status_write_by_sound_cpu() {
        let mut ssio = SsioBoard::new();
        // Sound CPU writes status via 0xC000 range
        bus_write(&mut ssio, 0xC000, 0x77);
        assert_eq!(ssio.status_read(), 0x77);
    }

    #[test]
    fn dip_switch_read() {
        let mut ssio = SsioBoard::new();
        ssio.set_dip_switches(0xAB);
        assert_eq!(bus_read(&mut ssio, 0xF000), 0xAB);
        assert_eq!(bus_read(&mut ssio, 0xFABC), 0xAB);
    }

    #[test]
    fn irq_clears_on_read() {
        let mut ssio = SsioBoard::new();
        ssio.bus.irq_pending = true;

        // Reading 0xE000 should clear IRQ
        let _ = bus_read(&mut ssio, 0xE000);
        assert!(!ssio.bus.irq_pending);
    }

    #[test]
    fn irq_fires_after_interval() {
        let mut ssio = SsioBoard::new();
        // Load a minimal ROM with HALT instruction (0x76) to prevent crash
        ssio.bus.rom[0] = 0x76; // HALT

        for _ in 0..IRQ_INTERVAL {
            ssio.tick();
        }
        assert!(ssio.bus.irq_pending);
    }

    #[test]
    fn ay_register_write_through_bus() {
        let mut ssio = SsioBoard::new();

        // Write AY0: address latch = register 7 (mixer)
        bus_write(&mut ssio, 0xA000, 7);
        // Write AY0: data = 0x3E (enable tone A only)
        bus_write(&mut ssio, 0xA002, 0x3E);
        // Read back through AY0 data port
        bus_write(&mut ssio, 0xA000, 7); // Re-latch address
        assert_eq!(bus_read(&mut ssio, 0xA001), 0x3E);

        // Same for AY1
        bus_write(&mut ssio, 0xB000, 8);
        bus_write(&mut ssio, 0xB002, 0x0F);
        bus_write(&mut ssio, 0xB000, 8);
        assert_eq!(bus_read(&mut ssio, 0xB001), 0x0F);
    }

    #[test]
    fn reset_clears_state() {
        let mut ssio = SsioBoard::new();
        ssio.bus.data_latch[0] = 0xFF;
        ssio.bus.status = 0x42;
        ssio.bus.irq_pending = true;
        ssio.bus.mute = true;

        ssio.reset();

        assert_eq!(ssio.bus.data_latch, [0; 4]);
        assert_eq!(ssio.bus.status, 0);
        assert!(!ssio.bus.irq_pending);
        assert!(!ssio.bus.mute);
    }

    #[test]
    fn save_load_round_trip() {
        let mut ssio = SsioBoard::new();
        ssio.latch_write(0, 0x42);
        ssio.bus.status = 0xAB;
        ssio.bus.irq_counter = 1234;
        ssio.bus.irq_pending = true;
        ssio.bus.duty_cycle[0][1] = 5;
        ssio.bus.overall[1] = 3;
        ssio.bus.mute = true;

        let mut w = StateWriter::new();
        ssio.save_state(&mut w);
        let data = w.into_vec();

        let mut ssio2 = SsioBoard::new();
        let mut r = StateReader::new(&data);
        ssio2.load_state(&mut r).unwrap();

        assert_eq!(ssio2.bus.data_latch[0], 0x42);
        assert_eq!(ssio2.bus.status, 0xAB);
        assert_eq!(ssio2.bus.irq_counter, 1234);
        assert!(ssio2.bus.irq_pending);
        assert_eq!(ssio2.bus.duty_cycle[0][1], 5);
        assert_eq!(ssio2.bus.overall[1], 3);
        assert!(ssio2.bus.mute);
    }
}
