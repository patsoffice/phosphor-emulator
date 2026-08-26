//! Atari System 1 sound board: an M6502 with a YM2151 (OPM FM) and a POKEY,
//! talking to the 68010 main CPU through a pair of one-byte latches. The board
//! is shared unchanged across the System 1 catalog (Marble Madness, Road
//! Runner, …); games that carry speech add a TMS5220 behind a VIA6522 at
//! `0x1000-0x100F`, enabled by the `speech` construction flag.
//!
//! The board owns the M6502 and implements [`Bus`] for it (a 16-bit space), in
//! the same shape as the Gottlieb sound board. The main CPU reaches it only
//! through the public latch methods: it writes a command (which pulses the
//! 6502's NMI), polls/reads the response, and drives the 6502's reset line.
//!
//! ## Sound-CPU memory map (mirrors folded out)
//! ```text
//!   0000-0FFF  RAM (mirror 2000)
//!   1000-100F  VIA6522 + TMS5220 speech (only when `speech` is set)
//!   1800-1801  YM2151 (address / data; reads return status)
//!   1810       R command latch (from main, clears the pending flag)
//!              W response latch (to main, raises main IRQ6)
//!   1820       R coin / buffer-status port    1820-1827 W addressable output latch
//!   1870-187F  POKEY
//!   4000-FFFF  ROM
//! ```
//! IRQ = YM2151 timer (or POKEY); NMI = a new command from the main CPU.

use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{Bus, BusMaster, ClockDomainName as Clk, ClockTree, DomainId};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::m6502::M6502;
use phosphor_core::device::pokey::Pokey;
use phosphor_core::device::tms5220::Tms5220;
use phosphor_core::device::via6522::Via6522;
use phosphor_core::device::ym2151::Ym2151;

/// Sound CPU clock: 14.318181 MHz / 8 = 1.789772 MHz. POKEY runs at the same
/// rate; the YM2151 runs at /4 = twice the sound CPU, so its timers advance two
/// chip clocks per sound-CPU cycle.
pub const SOUND_CLOCK_HZ: u32 = 1_789_772;
const YM_CLOCKS_PER_TICK: u32 = 2;

fn audio_sample_rate_hz() -> u32 {
    phosphor_core::audio::host_sample_rate() as u32
}

/// TMS5220 clock base: 14.318181 MHz / 2. Port B bit 4 selects the final
/// divider (÷11 nominal or ÷9), giving the two speech pitches the game uses.
const TMS_CLOCK_BASE_HZ: u32 = 7_159_090;

/// The TMS5220 master clock selected by Port B bit 4: `base / (16 - (5 | pb4<<1))`
/// → ÷11 = 650.8 kHz when clear, ÷9 = 795.5 kHz when set.
fn tms_clock_for_pb4(pb4: bool) -> u32 {
    let sel = 5 | if pb4 { 2 } else { 0 };
    TMS_CLOCK_BASE_HZ / (16 - sel)
}

/// The optional speech hardware: a TMS5220 reached through a VIA6522 mapped at
/// sound `0x1000-0x100F` (the SNDEXT window). Present only on speech games (Road
/// Runner et al.); absent on Marble. The wiring follows the System 1 board:
/// Port A is the TMS data / status byte, and Port B carries the `/WS` (D0) and
/// `/RS` (D1) strobes plus the TMS `/READY` (D2) and `/INT` (D3) status the
/// sound CPU polls.
struct Speech {
    via: Via6522,
    tms: Tms5220,
    /// The board's clock tree, stepped in *sound-CPU* cycles.
    ///
    /// The declaration in [`clock_tree`](crate::atari_system1::clock_tree) is
    /// the same one the registry test checks; only the reference differs.
    /// `set_step_domain` renominates the sound 6502 as the domain ratios are
    /// expressed against, because that is what this section's loop counts in:
    /// [`tick`](Self::tick) runs once per sound-CPU cycle.
    clocks: ClockTree,
    tms_dom: DomainId,
}

impl Speech {
    fn new() -> Self {
        let mut clocks = crate::atari_system1::clock_tree();
        let sound = clocks.find(Clk::SoundCpu).expect("declared sound domain");
        clocks.set_step_domain(sound);
        let tms_dom = clocks.find(Clk::Speech).expect("declared speech domain");
        let mut speech = Self {
            via: Via6522::new(),
            tms: Tms5220::new(),
            clocks,
            tms_dom,
        };
        speech.select_clock(false);
        speech
    }

    fn reset(&mut self) {
        self.via.reset();
        self.tms.reset();
        self.clocks.reset();
        self.select_clock(false);
    }

    /// Apply the clock Port B bit 4 selects, to the domain and the device
    /// together.
    fn select_clock(&mut self, pb4: bool) {
        let hz = tms_clock_for_pb4(pb4);
        self.clocks.set_domain_hz(self.tms_dom, hz);
        self.tms.set_clock(hz);
    }

    /// The TMS master clock currently selected.
    fn tms_clock_hz(&self) -> u32 {
        self.clocks.hz(self.tms_dom) as u32
    }

    /// Step the TMS at its (variable) master clock for one sound-CPU cycle.
    fn tick(&mut self) {
        if self.clocks.tick(self.tms_dom) {
            self.tms.tick();
        }
    }

    /// Read a VIA register (offset 0-15). Reading Port A/B first refreshes the
    /// input pins the TMS drives, so the CPU sees live status.
    fn read(&mut self, offset: u16) -> u8 {
        match offset & 0x0F {
            // Port B: D2 = TMS /READY, D3 = TMS /INT (both active-low, so a low
            // bit means "ready" / "interrupting").
            0x0 => {
                let ready = if self.tms.ready() { 0 } else { 1 << 2 };
                let intr = if self.tms.int_asserted() { 0 } else { 1 << 3 };
                self.via.set_pb_input(ready | intr);
                self.via.read(0)
            }
            // Port A: the TMS status register (read on the /RS strobe).
            0x1 | 0x0F => {
                self.via.set_pa_input(self.tms.status_r());
                self.via.read(offset)
            }
            _ => self.via.read(offset),
        }
    }

    /// Write a VIA register. A Port A write presents a speech-data byte to the
    /// TMS (latched on /WS on real hardware). A Port B write also drives the
    /// clock-select line (bit 4), which retunes the TMS master clock.
    fn write(&mut self, offset: u16, data: u8) {
        self.via.write(offset, data);
        if matches!(offset & 0x0F, 0x1 | 0x0F) {
            self.tms.data_w(data);
        }
        if offset & 0x0F == 0 {
            // Port B bit 4 (an output pin) selects the TMS master clock. The
            // domain and the device move together, in one call.
            let pb4 = self.via.read_output_b() & 0x10 != 0;
            if tms_clock_for_pb4(pb4) != self.tms_clock_hz() {
                self.select_clock(pb4);
            }
        }
    }

    fn irq(&self) -> bool {
        self.via.irq()
    }

    fn drain_audio(&mut self) -> Vec<f32> {
        self.tms.drain_audio()
    }

    fn save_state(&self, w: &mut StateWriter) {
        self.via.save_state(w);
        self.tms.save_state(w);
        // The selected clock travels as the domain's own ratio, so it no longer
        // needs writing beside the accumulator that used to carry it.
        self.clocks.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.via.load_state(r)?;
        self.tms.load_state(r)?;
        self.clocks.load_state(r)?;
        // Re-sync the TMS resampler to the restored clock (the device's clock is
        // configuration and isn't part of its own save state).
        self.tms.set_clock(self.tms_clock_hz());
        Ok(())
    }
}

pub struct AtariSystem1Sound {
    cpu: M6502,
    /// Everything the sound CPU talks to. Held apart from the CPU so a cycle
    /// dispatches at a concrete bus rather than a trait object -- see
    /// `docs/designs/concrete-bus-dispatch.md`.
    bus: AtariSystem1SoundBus,
}

/// The sound 6502's bus: POKEY, YM2151, optional speech, memory, and the
/// inter-CPU latches.
struct AtariSystem1SoundBus {
    sound_ram: Box<[u8; 0x1000]>,
    /// ROM mapped at 0x4000-0xFFFF (0x4000-0x7FFF is empty on marble).
    sound_rom: Box<[u8; 0xC000]>,
    pokey: Pokey,
    ym: Ym2151,
    /// TMS5220 speech behind a VIA6522 at `0x1000-0x100F` — present only on
    /// speech games (Road Runner et al.), `None` on Marble.
    speech: Option<Speech>,
    /// Addressable output latch (LS259): bit 0 = YM reset (unused here),
    /// bits 6-7 = coin counters (Phase 5).
    outlatch: u8,
    /// Coin switches read at 0x1820 (active-low, bits 0-2): a set bit here means
    /// that coin mech is currently pressed, so its 0x1820 bit reads 0.
    coin_inputs: u8,

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

impl AtariSystem1Sound {
    /// Build the sound board. `speech` maps the VIA6522 + TMS5220 speech window
    /// at `0x1000-0x100F` (Road Runner et al.); leave it off for Marble.
    pub fn new(speech: bool) -> Self {
        Self {
            cpu: M6502::new(),
            bus: AtariSystem1SoundBus {
                sound_ram: Box::new([0; 0x1000]),
                sound_rom: Box::new([0xFF; 0xC000]),
                pokey: Pokey::with_clock(SOUND_CLOCK_HZ, audio_sample_rate_hz()),
                ym: Ym2151::new(),
                speech: speech.then(Speech::new),
                outlatch: 0,
                coin_inputs: 0,
                soundlatch: 0,
                command_pending: false,
                mainlatch: 0,
                response_pending: false,
                sound_nmi: false,
                held_reset: true, // held until the main CPU releases it
                reset_pending: false,
                clock: 0,
            },
        }
    }

    /// Load the 64 KB sound region; its 0x4000-0xFFFF window maps to ROM.
    pub fn load_rom(&mut self, sound_image: &[u8]) {
        let src = &sound_image[0x4000..0x10000];
        self.bus.sound_rom.copy_from_slice(src);
    }

    pub fn reset(&mut self) {
        self.bus.pokey.reset();
        self.bus.ym.reset();
        if let Some(speech) = &mut self.bus.speech {
            speech.reset();
        }
        self.bus.outlatch = 0;
        self.bus.coin_inputs = 0;
        self.bus.soundlatch = 0;
        self.bus.command_pending = false;
        self.bus.mainlatch = 0;
        self.bus.response_pending = false;
        self.bus.sound_nmi = false;
        self.bus.held_reset = true; // back under main-CPU control
        self.bus.reset_pending = false;
        self.bus.clock = 0;
    }

    // -- Inter-CPU latch interface (called by the main bus) ------------------

    /// Main CPU writes a sound command (0xFE0000): latch it, flag it pending,
    /// and pulse the sound CPU's NMI.
    pub fn write_command(&mut self, data: u8) {
        self.bus.soundlatch = data;
        self.bus.command_pending = true;
        // The NMI is a falling edge on /NMI. While the sound CPU is held in
        // reset its edge detector is cleared, so a command latched during reset
        // raises 68KBUF but generates no NMI — the CPU picks it up by polling
        // 0x1820 once it has booted. Only a command latched while the CPU is
        // running produces an edge.
        if !self.bus.held_reset {
            self.bus.sound_nmi = true;
        }
    }

    /// Main CPU reads the sound response (0xFC0000), clearing the pending flag.
    pub fn read_response(&mut self) -> u8 {
        self.bus.response_pending = false;
        self.bus.mainlatch
    }

    /// 68KBUF: a command is latched but the sound CPU has not read it yet.
    pub fn command_pending(&self) -> bool {
        self.bus.command_pending
    }

    /// SNDBUF: a response is latched for the main CPU — drives main IRQ6.
    pub fn response_pending(&self) -> bool {
        self.bus.response_pending
    }

    /// Press/release a coin switch (`bit` 0-2 → coin 1-3), read back at 0x1820.
    pub fn set_coin(&mut self, bit: u8, pressed: bool) {
        let mask = 1u8 << (bit & 0x07);
        if pressed {
            self.bus.coin_inputs |= mask;
        } else {
            self.bus.coin_inputs &= !mask;
        }
    }

    /// Drive the sound CPU's reset line (bankselect bit 7: 1 = run, 0 = hold).
    /// Asserting reset acknowledges any pending response; releasing it boots the
    /// CPU from its reset vector on the next tick.
    pub fn set_reset(&mut self, asserted: bool) {
        if asserted {
            self.bus.response_pending = false;
        } else if self.bus.held_reset {
            self.bus.reset_pending = true;
        }
        self.bus.held_reset = asserted;
    }

    /// Advance the sound board by one sound-CPU cycle (frozen while held reset).
    pub fn tick(&mut self) {
        if self.bus.held_reset {
            return;
        }
        if self.bus.reset_pending {
            self.bus.reset_pending = false;
            self.cpu.reset(&mut self.bus, BusMaster::Cpu(1));
        }
        self.cpu.execute_cycle(&mut self.bus, BusMaster::Cpu(1));
        self.bus.pokey.tick();
        self.bus.ym.tick(YM_CLOCKS_PER_TICK);
        if let Some(speech) = &mut self.bus.speech {
            speech.tick();
        }
        self.bus.clock += 1;
    }

    /// Drain and mix the sound board's audio: the POKEY (unipolar `[0, 1]`, 0 at
    /// silence) plus the YM2151 FM voices (bipolar). Both streams resample to the
    /// same host rate, so they line up sample-for-sample. The result still
    /// carries the POKEY's DC; the machine removes it before output.
    pub fn drain_audio(&mut self) -> Vec<f32> {
        /// FM mix gain. The YM core normalises its eight-channel sum to full
        /// scale, so typical music sits well below 1.0; lift it to a healthy
        /// level in the mix. Tunable by ear.
        const YM_MIX: f32 = 3.0;
        /// Speech mix gain. The TMS output is already ~full-scale bipolar; keep
        /// it at parity with the music. Tunable by ear vs MAME.
        const SPEECH_MIX: f32 = 1.0;

        let ym = self.bus.ym.drain_audio();
        let pokey = self.bus.pokey.drain_audio();
        let speech = self
            .bus
            .speech
            .as_mut()
            .map(Speech::drain_audio)
            .unwrap_or_default();
        let n = pokey.len().max(ym.len()).max(speech.len());
        let mut out = vec![0.0f32; n];
        for (i, slot) in out.iter_mut().enumerate() {
            *slot = pokey.get(i).copied().unwrap_or(0.0)
                + ym.get(i).copied().unwrap_or(0.0) * YM_MIX
                + speech.get(i).copied().unwrap_or(0.0) * SPEECH_MIX;
        }
        out
    }

    /// (held_reset, sound-CPU cycles run, command_pending, response_pending) —
    /// headless bring-up diagnostics.
    pub fn debug_state(&self) -> (bool, u64, bool, bool) {
        (
            self.bus.held_reset,
            self.bus.clock,
            self.bus.command_pending,
            self.bus.response_pending,
        )
    }
}

impl Default for AtariSystem1Sound {
    /// Marble-style board: no speech.
    fn default() -> Self {
        Self::new(false)
    }
}

impl AtariSystem1SoundBus {
    /// The 0x1820 status port: coin inputs, plus the buffer-full flags.
    fn read_1820(&self) -> u8 {
        // Coins (bits 0-2, active-low) and bit 7 idle high; a pressed coin mech
        // pulls its bit low. Bit 3 = command pending (68KBUF), bit 4 = response
        // pending (SNDBUF). The self-test bit (7) toggle lands with the operator
        // service switch.
        let mut v = 0x87 & !(self.coin_inputs & 0x07);
        if self.command_pending {
            v |= 0x08;
        }
        if self.response_pending {
            v |= 0x10;
        }
        v
    }
}

impl Bus for AtariSystem1SoundBus {
    type Address = u16;
    type Data = u8;

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn read(&mut self, _master: BusMaster, addr: u16) -> u8 {
        if addr >= 0x4000 {
            return self.sound_rom[(addr - 0x4000) as usize];
        }
        // Speech window (VIA6522 bridge to the TMS5220), present only on speech
        // games. The board wires the TMS status onto the VIA's port pins.
        if let Some(speech) = &mut self.speech
            && (0x1000..=0x100F).contains(&addr)
        {
            return speech.read(addr & 0x0F);
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
        // Speech window (VIA6522 bridge to the TMS5220), present only on speech
        // games.
        if let Some(speech) = &mut self.speech
            && (0x1000..=0x100F).contains(&addr)
        {
            speech.write(addr & 0x0F, data);
            return;
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
            irq: self.ym.irq() || self.pokey.irq() || self.speech.as_ref().is_some_and(|s| s.irq()),
            ..Default::default()
        }
    }
}

impl phosphor_core::core::debug::Debuggable for AtariSystem1Sound {
    fn debug_registers(&self) -> Vec<phosphor_core::core::debug::DebugRegister> {
        use phosphor_core::core::debug::DebugRegister;
        vec![
            DebugRegister {
                name: "SND_CLK",
                value: self.bus.clock,
                width: 32,
            },
            DebugRegister {
                name: "CMD",
                value: self.bus.soundlatch as u64,
                width: 8,
            },
            DebugRegister {
                name: "RESP",
                value: self.bus.mainlatch as u64,
                width: 8,
            },
            DebugRegister {
                name: "HELD_RST",
                value: u64::from(self.bus.held_reset),
                width: 1,
            },
        ]
    }
}

impl phosphor_core::device::Device for AtariSystem1Sound {
    fn name(&self) -> &'static str {
        "Atari System 1 Sound (M6502)"
    }

    fn reset(&mut self) {
        Self::reset(self);
    }
}

// Chunk tags for this board's components, assigned explicitly because the impl
// is hand-written. Stable for the board; a retired tag is never reused.
//
// The speech board is only on the speech games, so its tag may be absent. It is
// last in the body, with nothing after it: an optional chunk can only be
// followed by another chunk or by the end of the board's own bytes, because
// absence is detected by peeking at the next tag.
const A1S_TAG_CPU: u16 = 1;
const A1S_TAG_POKEY: u16 = 2;
const A1S_TAG_YM: u16 = 3;
const A1S_TAG_SPEECH: u16 = 4;

impl Saveable for AtariSystem1Sound {
    fn save_state(&self, w: &mut StateWriter) {
        w.write_component(A1S_TAG_CPU, &self.cpu);
        w.write_component(A1S_TAG_POKEY, &self.bus.pokey);
        w.write_component(A1S_TAG_YM, &self.bus.ym);
        w.write_bytes(self.bus.sound_ram.as_ref());
        w.write_u8(self.bus.outlatch);
        w.write_u8(self.bus.coin_inputs);
        w.write_u8(self.bus.soundlatch);
        w.write_bool(self.bus.command_pending);
        w.write_u8(self.bus.mainlatch);
        w.write_bool(self.bus.response_pending);
        w.write_bool(self.bus.sound_nmi);
        w.write_bool(self.bus.held_reset);
        w.write_bool(self.bus.reset_pending);
        w.write_u64_le(self.bus.clock);
        // Speech state only exists on speech games. It is now a chunk, so a
        // Marble save that has none is distinguishable from a Road Runner save
        // that does, rather than the two differing only in trailing length.
        // `Speech` keeps inherent save/load rather than a `Saveable` impl, so
        // the frame goes on by hand.
        if let Some(speech) = &self.bus.speech {
            w.write_tlv(A1S_TAG_SPEECH, |w| speech.save_state(w));
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        r.read_component(A1S_TAG_CPU, "AtariSystem1Sound.cpu", |r| {
            self.cpu.load_state(r)
        })?;
        r.read_component(A1S_TAG_POKEY, "AtariSystem1Sound.pokey", |r| {
            self.bus.pokey.load_state(r)
        })?;
        r.read_component(A1S_TAG_YM, "AtariSystem1Sound.ym", |r| {
            self.bus.ym.load_state(r)
        })?;
        r.read_bytes_into(self.bus.sound_ram.as_mut())?;
        self.bus.outlatch = r.read_u8()?;
        self.bus.coin_inputs = r.read_u8()?;
        self.bus.soundlatch = r.read_u8()?;
        self.bus.command_pending = r.read_bool()?;
        self.bus.mainlatch = r.read_u8()?;
        self.bus.response_pending = r.read_bool()?;
        self.bus.sound_nmi = r.read_bool()?;
        self.bus.held_reset = r.read_bool()?;
        self.bus.reset_pending = r.read_bool()?;
        self.bus.clock = r.read_u64_le()?;
        let has_speech = self.bus.speech.is_some();
        r.read_optional(
            A1S_TAG_SPEECH,
            "AtariSystem1Sound.speech",
            has_speech,
            |r| {
                self.bus
                    .speech
                    .as_mut()
                    .expect("guarded by has_speech")
                    .load_state(r)
            },
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a sound board with a tiny hand-assembled 6502 program that, on NMI,
    /// reads the command latch and echoes (command + 1) back to the main CPU.
    fn board_with_echo_program() -> AtariSystem1Sound {
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

        let mut snd = AtariSystem1Sound::new(false);
        snd.load_rom(&image);
        snd
    }

    fn run(snd: &mut AtariSystem1Sound, cycles: usize) {
        for _ in 0..cycles {
            snd.tick();
        }
    }

    #[test]
    fn coin_switch_pulls_1820_bit_low() {
        let mut snd = board_with_echo_program();
        // Coins idle high (active-low).
        assert_eq!(snd.bus.read_1820() & 0x07, 0x07);
        // Pressing coin 1 clears bit 0; the other coin bits stay high.
        snd.set_coin(0, true);
        assert_eq!(snd.bus.read_1820() & 0x07, 0x06);
        // Releasing restores it.
        snd.set_coin(0, false);
        assert_eq!(snd.bus.read_1820() & 0x07, 0x07);
    }

    #[test]
    fn held_in_reset_until_released() {
        let mut snd = board_with_echo_program();
        run(&mut snd, 100);
        assert_eq!(snd.bus.clock, 0, "frozen while held in reset");
        snd.set_reset(false); // release
        run(&mut snd, 10);
        assert!(snd.bus.clock > 0, "runs once released");
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
    fn command_latched_during_reset_raises_no_nmi() {
        // The main CPU latches a command while the sound CPU is still held in
        // reset (this is exactly the boot handshake: command written, then the
        // reset line released a cycle later). The reset holds the 6502's /NMI
        // edge detector clear, so no NMI edge is produced — the freshly booted
        // CPU must pick the command up by polling 0x1820 (68KBUF), not service a
        // spurious NMI before it has run its init. See the boot desync this
        // guards against: an early NMI corrupts the first command/response.
        let mut snd = board_with_echo_program();
        assert!(snd.bus.held_reset, "starts held in reset");
        snd.write_command(0x00);
        assert!(snd.command_pending(), "68KBUF set for the poll path");
        assert!(!snd.bus.sound_nmi, "no NMI edge while held in reset");

        // A command latched once the CPU is running does produce an NMI edge.
        snd.set_reset(false);
        run(&mut snd, 20);
        snd.write_command(0x10);
        assert!(snd.bus.sound_nmi, "running CPU sees the NMI edge");
    }

    #[test]
    fn reset_assert_acknowledges_response() {
        let mut snd = board_with_echo_program();
        snd.bus.response_pending = true;
        snd.bus.mainlatch = 0x55;
        snd.set_reset(true); // assert reset
        assert!(!snd.response_pending(), "reset clears the pending response");
    }

    #[test]
    fn save_load_round_trip() {
        let mut snd = board_with_echo_program();
        snd.set_reset(false);
        run(&mut snd, 30);
        snd.bus.sound_ram[0x100] = 0x77;
        snd.bus.soundlatch = 0x12;
        snd.bus.command_pending = true;

        let mut w = StateWriter::new();
        snd.save_state(&mut w);
        let bytes = w.into_vec();

        let mut snd2 = board_with_echo_program();
        let mut r = StateReader::new(&bytes);
        snd2.load_state(&mut r).unwrap();
        assert_eq!(snd2.bus.sound_ram[0x100], 0x77);
        assert_eq!(snd2.bus.soundlatch, 0x12);
        assert!(snd2.bus.command_pending);
    }

    const M: BusMaster = BusMaster::Cpu(1);

    #[test]
    fn no_speech_window_when_speech_disabled() {
        // Marble (speech=false): the 0x1000-0x100F window is not decoded and
        // reads back the open-bus 0xFF, exactly as before the speech wiring.
        let mut snd = AtariSystem1Sound::new(false);
        assert!(snd.bus.speech.is_none());
        assert_eq!(Bus::read(&mut snd.bus, M, 0x1000), 0xFF);
        assert_eq!(Bus::read(&mut snd.bus, M, 0x100F), 0xFF);
    }

    #[test]
    fn speech_port_b_reports_tms_ready() {
        // Road Runner-style board (speech=true): reading VIA Port B (offset 0)
        // returns the TMS status lines — D2 = /READY (low = ready), D3 = /INT
        // (high = idle). An idle TMS is ready and not interrupting, and DDRB
        // powers up as all-input, so the port reads 0x08.
        let mut snd = AtariSystem1Sound::new(true);
        assert!(snd.bus.speech.is_some());
        let pb = Bus::read(&mut snd.bus, M, 0x1000);
        assert_eq!(pb & (1 << 2), 0, "TMS /READY low → ready");
        assert_eq!(pb & (1 << 3), 1 << 3, "TMS /INT high → idle");
    }

    #[test]
    fn speech_streams_data_without_stalling() {
        // Drive the VIA the way a speech routine's poll loop would: set Port A
        // to output, write a byte, pulse /WS on Port B. These are command writes
        // (SPEAK EXTERNAL is never sent), so nothing enters the FIFO and the
        // idle TMS stays ready throughout — the loop never blocks.
        let mut snd = AtariSystem1Sound::new(true);
        Bus::write(&mut snd.bus, M, 0x1003, 0xFF); // DDRA = all output
        for byte in 0..32u16 {
            Bus::write(&mut snd.bus, M, 0x1001, byte as u8); // ORA = data
            Bus::write(&mut snd.bus, M, 0x1000, 0x00); // /WS low (strobe)
            Bus::write(&mut snd.bus, M, 0x1000, 0x01); // /WS high
            assert_eq!(
                Bus::read(&mut snd.bus, M, 0x1000) & (1 << 2),
                0,
                "still ready"
            );
        }
        // The stub VIA emulates no interrupt sources, so it never raises the
        // sound CPU's IRQ.
        assert!(!snd.bus.speech.as_ref().unwrap().irq());
    }

    #[test]
    fn speech_state_round_trips_through_save_load() {
        let mut snd = AtariSystem1Sound::new(true);
        // Latch some VIA register state through the speech path.
        Bus::write(&mut snd.bus, M, 0x1002, 0xFF); // DDRB
        Bus::write(&mut snd.bus, M, 0x1000, 0x5A); // ORB
        Bus::write(&mut snd.bus, M, 0x100B, 0x40); // ACR (raw register file)

        let mut w = StateWriter::new();
        snd.save_state(&mut w);
        let bytes = w.into_vec();

        let mut snd2 = AtariSystem1Sound::new(true);
        let mut r = StateReader::new(&bytes);
        snd2.load_state(&mut r).unwrap();
        // ORB drives all 8 pins (DDRB=0xFF), so Port B reads back 0x5A.
        assert_eq!(Bus::read(&mut snd2.bus, M, 0x1000), 0x5A);
        assert_eq!(Bus::read(&mut snd2.bus, M, 0x100B), 0x40);
    }

    #[test]
    fn tms_clock_select_matches_the_divider() {
        assert_eq!(tms_clock_for_pb4(false), 650_826); // ÷11 nominal
        assert_eq!(tms_clock_for_pb4(true), 795_454); // ÷9
    }

    #[test]
    fn speech_port_b_bit4_retunes_the_tms_clock() {
        let mut sp = Speech::new();
        assert_eq!(sp.tms_clock_hz(), 650_826, "powers up at the ÷11 clock");
        sp.write(0x2, 0x10); // DDRB: Port B bit 4 = output
        sp.write(0x0, 0x10); // ORB bit 4 high → ÷9 clock
        assert_eq!(sp.tms_clock_hz(), 795_454);
        sp.write(0x0, 0x00); // ORB bit 4 low → back to nominal
        assert_eq!(sp.tms_clock_hz(), 650_826);
    }

    /// The retune reaches the device as well as the domain, and both survive a
    /// save/load. The device cannot restore its own clock (it save-skips it),
    /// which is exactly the gap the old `_applied` shadow fields existed to
    /// paper over.
    #[test]
    fn a_retuned_tms_clock_reloads_retuned() {
        let mut sp = Speech::new();
        sp.write(0x2, 0x10);
        sp.write(0x0, 0x10); // select the ÷9 clock
        assert_eq!(sp.tms_clock_hz(), 795_454);

        let mut w = StateWriter::new();
        sp.save_state(&mut w);
        let data = w.into_vec();

        let mut restored = Speech::new();
        assert_eq!(restored.tms_clock_hz(), 650_826);
        let mut r = StateReader::new(&data);
        restored.load_state(&mut r).unwrap();
        assert_eq!(restored.tms_clock_hz(), 795_454);
    }

    /// The TMS is stepped against the *sound* CPU, not the main one.
    ///
    /// `Speech::tick` runs once per sound-CPU cycle, so its tree renominates
    /// the sound 6502 as the stepping domain. Against the main CPU the ratio
    /// would be four times too slow.
    #[test]
    fn the_speech_domain_is_stepped_against_the_sound_cpu() {
        let sp = Speech::new();
        let sound = sp.clocks.find(Clk::SoundCpu).expect("sound domain");
        assert_eq!(sp.clocks.step_domain(), Some(sound));
        // 650826 / 1789772.625, which is what the hand-rolled accumulator did
        // against a truncated 1789772.
        let (num, den) = sp.clocks.domain(sp.tms_dom).step_ratio();
        assert_eq!(
            (num as f64 / den as f64 * 1_000_000.0).round(),
            363_636.0,
            "the TMS runs at 0.3636 of the sound CPU"
        );
    }

    #[test]
    fn speech_ticks_drain_the_fifo() {
        let mut sp = Speech::new();
        sp.write(0x1, 0x60); // SPEAK EXTERNAL command (Port A)
        for _ in 0..16 {
            sp.write(0x1, 0xA5); // fill the 16-byte FIFO
        }
        // A full FIFO in speak-external mode reports not-ready (Port B D2 high).
        assert_ne!(sp.read(0x0) & (1 << 2), 0, "full FIFO → not ready");
        // Ticking the TMS synthesizes speech, consuming the FIFO over time.
        for _ in 0..400_000 {
            sp.tick();
        }
        assert_eq!(sp.read(0x0) & (1 << 2), 0, "drained FIFO → ready again");
    }
}
