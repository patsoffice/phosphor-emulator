//! Marble Madness sound board: an M6502 with a YM2151 (stubbed) and a POKEY,
//! talking to the 68010 main CPU through a pair of one-byte latches.
//!
//! The board owns the M6502 and implements [`Bus`] for it (a 16-bit space), in
//! the same shape as the Gottlieb sound board. The main CPU reaches it only
//! through the public latch methods: it writes a command (which pulses the
//! 6502's NMI), polls/reads the response, and drives the 6502's reset line.
//!
//! ## Sound-CPU memory map (mirrors folded out)
//! ```text
//!   0000-0FFF  RAM (mirror 2000)
//!   1800-1801  YM2151 (address / data; reads return status)
//!   1810       R command latch (from main, clears the pending flag)
//!              W response latch (to main, raises main IRQ6)
//!   1820       R coin / buffer-status port    1820-1827 W addressable output latch
//!   1870-187F  POKEY
//!   4000-FFFF  ROM
//! ```
//! IRQ = YM2151 timer (or POKEY); NMI = a new command from the main CPU.

use phosphor_core::bus_split;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::device::pokey::Pokey;
use phosphor_core::device::ym2151::Ym2151;

/// Sound CPU clock: 14.318181 MHz / 8 = 1.789772 MHz. POKEY runs at the same
/// rate; the YM2151 runs at /4 = twice the sound CPU, so its timers advance two
/// chip clocks per sound-CPU cycle.
pub const SOUND_CLOCK_HZ: u32 = 1_789_772;
const YM_CLOCKS_PER_TICK: u32 = 2;

const AUDIO_SAMPLE_RATE: u32 = 44_100;

pub struct MarbleSound {
    cpu: M6502,
    sound_ram: Box<[u8; 0x1000]>,
    /// ROM mapped at 0x4000-0xFFFF (0x4000-0x7FFF is empty on marble).
    sound_rom: Box<[u8; 0xC000]>,
    pokey: Pokey,
    ym: Ym2151,
    /// Addressable output latch (LS259): bit 0 = YM reset (ignored by the stub),
    /// bits 6-7 = coin counters (Phase 5).
    outlatch: u8,

    // Inter-CPU latches.
    /// Command from the main CPU; `command_pending` is the 68KBUF flag.
    soundlatch: u8,
    command_pending: bool,
    /// Response to the main CPU; `response_pending` raises main IRQ6 (SNDBUF).
    mainlatch: u8,
    response_pending: bool,
    /// One-shot NMI to the 6502, set when a fresh command arrives.
    sound_nmi: bool,

    /// True while the main CPU holds the sound CPU in reset (bankselect bit 7).
    held_reset: bool,
    /// Set when reset is released, so the next tick boots the CPU from its vector.
    reset_pending: bool,

    clock: u64,
}

impl MarbleSound {
    pub fn new() -> Self {
        Self {
            cpu: M6502::new(),
            sound_ram: Box::new([0; 0x1000]),
            sound_rom: Box::new([0xFF; 0xC000]),
            pokey: Pokey::with_clock(SOUND_CLOCK_HZ, AUDIO_SAMPLE_RATE),
            ym: Ym2151::new(),
            outlatch: 0,
            soundlatch: 0,
            command_pending: false,
            mainlatch: 0,
            response_pending: false,
            sound_nmi: false,
            held_reset: true, // held until the main CPU releases it
            reset_pending: false,
            clock: 0,
        }
    }

    /// Load the 64 KB sound region; its 0x4000-0xFFFF window maps to ROM.
    pub fn load_rom(&mut self, sound_image: &[u8]) {
        let src = &sound_image[0x4000..0x10000];
        self.sound_rom.copy_from_slice(src);
    }

    pub fn reset(&mut self) {
        self.pokey.reset();
        self.ym.reset();
        self.outlatch = 0;
        self.soundlatch = 0;
        self.command_pending = false;
        self.mainlatch = 0;
        self.response_pending = false;
        self.sound_nmi = false;
        self.held_reset = true; // back under main-CPU control
        self.reset_pending = false;
        self.clock = 0;
    }

    // -- Inter-CPU latch interface (called by the main bus) ------------------

    /// Main CPU writes a sound command (0xFE0000): latch it, flag it pending,
    /// and pulse the sound CPU's NMI.
    pub fn write_command(&mut self, data: u8) {
        self.soundlatch = data;
        self.command_pending = true;
        self.sound_nmi = true;
    }

    /// Main CPU reads the sound response (0xFC0000), clearing the pending flag.
    pub fn read_response(&mut self) -> u8 {
        self.response_pending = false;
        self.mainlatch
    }

    /// 68KBUF: a command is latched but the sound CPU has not read it yet.
    pub fn command_pending(&self) -> bool {
        self.command_pending
    }

    /// SNDBUF: a response is latched for the main CPU — drives main IRQ6.
    pub fn response_pending(&self) -> bool {
        self.response_pending
    }

    /// Drive the sound CPU's reset line (bankselect bit 7: 1 = run, 0 = hold).
    /// Asserting reset acknowledges any pending response; releasing it boots the
    /// CPU from its reset vector on the next tick.
    pub fn set_reset(&mut self, asserted: bool) {
        if asserted {
            self.response_pending = false;
        } else if self.held_reset {
            self.reset_pending = true;
        }
        self.held_reset = asserted;
    }

    /// Advance the sound board by one sound-CPU cycle (frozen while held reset).
    pub fn tick(&mut self) {
        if self.held_reset {
            return;
        }
        if self.reset_pending {
            self.reset_pending = false;
            bus_split!(self, bus => {
                self.cpu.reset(bus, BusMaster::Cpu(1));
            });
        }
        bus_split!(self, bus => {
            self.cpu.execute_cycle(bus, BusMaster::Cpu(1));
        });
        self.pokey.tick();
        self.ym.tick(YM_CLOCKS_PER_TICK);
        self.clock += 1;
    }

    /// Drain accumulated audio (POKEY only; the YM2151 stub is silent).
    pub fn drain_audio(&mut self) -> Vec<f32> {
        let _ = self.ym.drain_audio();
        self.pokey.drain_audio()
    }

    /// (held_reset, sound-CPU cycles run, command_pending, response_pending) —
    /// headless bring-up diagnostics.
    pub fn debug_state(&self) -> (bool, u64, bool, bool) {
        (
            self.held_reset,
            self.clock,
            self.command_pending,
            self.response_pending,
        )
    }

    /// The 0x1820 status port: coin inputs (idle), plus the buffer-full flags.
    fn read_1820(&self) -> u8 {
        // Coins (bits 0-2, active-low) idle high and bit 7 idle high; bit 3 =
        // command pending (68KBUF), bit 4 = response pending (SNDBUF). Real coin
        // inputs and the self-test (bit 7) toggle land in Phase 5.
        let mut v = 0x87;
        if self.command_pending {
            v |= 0x08;
        }
        if self.response_pending {
            v |= 0x10;
        }
        v
    }
}

impl Default for MarbleSound {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus for MarbleSound {
    type Address = u16;
    type Data = u8;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        if addr >= 0x4000 {
            return self.sound_rom[(addr - 0x4000) as usize];
        }
        if addr & 0x1800 == 0x1800 {
            match addr & 0x70 {
                0x00 => self.ym.read(addr & 1),
                0x10 => {
                    self.command_pending = false; // reading the latch acknowledges it
                    self.soundlatch
                }
                0x20 => self.read_1820(),
                0x70 => self.pokey.read(addr & 0x0F),
                _ => 0xFF,
            }
        } else if addr & 0x1000 == 0 {
            self.sound_ram[(addr & 0x0FFF) as usize]
        } else {
            0xFF
        }
    }

    fn write(&mut self, _master: BusMaster, addr: u16, data: u8) {
        if addr >= 0x4000 {
            return; // ROM
        }
        if addr & 0x1800 == 0x1800 {
            match addr & 0x70 {
                0x00 => self.ym.write(addr & 1, data),
                0x10 => {
                    self.mainlatch = data;
                    self.response_pending = true; // raises main IRQ6
                }
                0x20 => {
                    let bit = (addr & 7) as u8;
                    self.outlatch = (self.outlatch & !(1 << bit)) | ((data & 1) << bit);
                }
                0x70 => self.pokey.write(addr & 0x0F, data),
                _ => {}
            }
        } else if addr & 0x1000 == 0 {
            self.sound_ram[(addr & 0x0FFF) as usize] = data;
        }
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> InterruptState {
        let nmi = self.sound_nmi;
        self.sound_nmi = false;
        InterruptState {
            nmi,
            irq: self.ym.irq() || self.pokey.irq(),
            ..Default::default()
        }
    }
}

impl phosphor_core::core::debug::Debuggable for MarbleSound {
    fn debug_registers(&self) -> Vec<phosphor_core::core::debug::DebugRegister> {
        use phosphor_core::core::debug::DebugRegister;
        vec![
            DebugRegister {
                name: "SND_CLK",
                value: self.clock,
                width: 32,
            },
            DebugRegister {
                name: "CMD",
                value: self.soundlatch as u64,
                width: 8,
            },
            DebugRegister {
                name: "RESP",
                value: self.mainlatch as u64,
                width: 8,
            },
            DebugRegister {
                name: "HELD_RST",
                value: u64::from(self.held_reset),
                width: 1,
            },
        ]
    }
}

impl phosphor_core::device::Device for MarbleSound {
    fn name(&self) -> &'static str {
        "Marble Sound (M6502)"
    }

    fn reset(&mut self) {
        Self::reset(self);
    }
}

impl Saveable for MarbleSound {
    fn save_state(&self, w: &mut StateWriter) {
        self.cpu.save_state(w);
        self.pokey.save_state(w);
        self.ym.save_state(w);
        w.write_bytes(self.sound_ram.as_ref());
        w.write_u8(self.outlatch);
        w.write_u8(self.soundlatch);
        w.write_bool(self.command_pending);
        w.write_u8(self.mainlatch);
        w.write_bool(self.response_pending);
        w.write_bool(self.sound_nmi);
        w.write_bool(self.held_reset);
        w.write_bool(self.reset_pending);
        w.write_u64_le(self.clock);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.cpu.load_state(r)?;
        self.pokey.load_state(r)?;
        self.ym.load_state(r)?;
        r.read_bytes_into(self.sound_ram.as_mut())?;
        self.outlatch = r.read_u8()?;
        self.soundlatch = r.read_u8()?;
        self.command_pending = r.read_bool()?;
        self.mainlatch = r.read_u8()?;
        self.response_pending = r.read_bool()?;
        self.sound_nmi = r.read_bool()?;
        self.held_reset = r.read_bool()?;
        self.reset_pending = r.read_bool()?;
        self.clock = r.read_u64_le()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const M: BusMaster = BusMaster::Cpu(1);

    /// Build a sound board with a tiny hand-assembled 6502 program that, on NMI,
    /// reads the command latch and echoes (command + 1) back to the main CPU.
    fn board_with_echo_program() -> MarbleSound {
        let mut image = vec![0xFFu8; 0x10000];
        // Main program @ 0x8000: CLI; loop forever (the work happens in the NMI).
        let main = [0x58u8, 0x4C, 0x00, 0x80]; // CLI ; JMP $8000
        image[0x8000..0x8000 + main.len()].copy_from_slice(&main);
        // NMI handler @ 0x9000:
        //   LDA $1810   ; read command (clears 68KBUF)
        //   CLC ; ADC #1 ; STA $1810  ; write response (raises SNDBUF)
        //   RTI
        let nmi = [
            0xADu8, 0x10, 0x18, // LDA $1810
            0x18, 0x69, 0x01, // CLC ; ADC #$01
            0x8D, 0x10, 0x18, // STA $1810
            0x40, // RTI
        ];
        image[0x9000..0x9000 + nmi.len()].copy_from_slice(&nmi);
        // Vectors: RESET = $8000, NMI = $9000.
        image[0xFFFC] = 0x00;
        image[0xFFFD] = 0x80;
        image[0xFFFA] = 0x00;
        image[0xFFFB] = 0x90;

        let mut snd = MarbleSound::new();
        snd.load_rom(&image);
        snd
    }

    fn run(snd: &mut MarbleSound, cycles: usize) {
        for _ in 0..cycles {
            snd.tick();
        }
    }

    #[test]
    fn held_in_reset_until_released() {
        let mut snd = board_with_echo_program();
        run(&mut snd, 100);
        assert_eq!(snd.clock, 0, "frozen while held in reset");
        snd.set_reset(false); // release
        run(&mut snd, 10);
        assert!(snd.clock > 0, "runs once released");
    }

    #[test]
    fn command_response_handshake() {
        let mut snd = board_with_echo_program();
        snd.set_reset(false); // release the sound CPU
        run(&mut snd, 50); // let it reach the CLI loop

        // Main sends a command → 68KBUF pending, NMI pulsed.
        snd.write_command(0x42);
        assert!(snd.command_pending(), "command latched (68KBUF)");

        // The NMI handler reads the command (clears 68KBUF) and writes the
        // response (raises SNDBUF).
        run(&mut snd, 60);
        assert!(!snd.command_pending(), "sound CPU consumed the command");
        assert!(snd.response_pending(), "response latched for the main CPU");

        // Main reads the response (command + 1) and clears SNDBUF.
        assert_eq!(snd.read_response(), 0x43);
        assert!(!snd.response_pending());
    }

    #[test]
    fn reset_assert_acknowledges_response() {
        let mut snd = board_with_echo_program();
        snd.response_pending = true;
        snd.mainlatch = 0x55;
        snd.set_reset(true); // assert reset
        assert!(!snd.response_pending(), "reset clears the pending response");
    }

    #[test]
    fn save_load_round_trip() {
        let mut snd = board_with_echo_program();
        snd.set_reset(false);
        run(&mut snd, 30);
        snd.sound_ram[0x100] = 0x77;
        snd.soundlatch = 0x12;
        snd.command_pending = true;

        let mut w = StateWriter::new();
        snd.save_state(&mut w);
        let bytes = w.into_vec();

        let mut snd2 = board_with_echo_program();
        let mut r = StateReader::new(&bytes);
        snd2.load_state(&mut r).unwrap();
        assert_eq!(snd2.sound_ram[0x100], 0x77);
        assert_eq!(snd2.soundlatch, 0x12);
        assert!(snd2.command_pending);
    }
}
