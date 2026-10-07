//! Time Pilot sound board (Konami, 1982).
//!
//! Self-contained Z80 + 2xAY-8910 as documented in
//! `docs/schematics/timepilot-sound.md`. The main board writes a command byte
//! (strobed by a 0xC000 write) into an LS273 latch the sound CPU reads as AY1
//! port A, and pulses the sound IRQ with main latch bit 2 (rising edge). AY1
//! port B carries a divide-by-5120 timer table. Each of the six AY channels
//! passes through its own RC lowpass whose capacitor the sound program selects
//! with a filter write (the address carries the bits); main latch bit 3 mutes
//! the LA4460 output stage.
//!
//! MAME shares this device (`TIMEPLT_AUDIO`) with Pooyan, Loco-Motion,
//! Tutankham and Roc'n Rope, but none of those run here yet, so the board
//! lives in `phosphor-machines` next to its game rather than in
//! `phosphor-core`'s shared devices.
//!
//! # Sound Z80 memory map
//!
//! | Address       | R/W | Description                            |
//! |---------------|-----|----------------------------------------|
//! | 0x0000-0x2FFF | R   | Sound ROM (tm7 populates 0x0000-0x0FFF)|
//! | 0x3000-0x33FF | R/W | RAM (1 KB, mirrored by 0x0C00)         |
//! | 0x4xxx        | R/W | AY1 data                               |
//! | 0x5xxx        | W   | AY1 address                            |
//! | 0x6xxx        | R/W | AY2 data                               |
//! | 0x7xxx        | W   | AY2 address                            |
//! | 0x8000-0xFFFF | W   | Filter select (address carries 12 bits)|
//!
//! AY1 port A (input) = the command latch; AY1 port B (input) = the timer.

use phosphor_core::audio::{AudioResampler, host_sample_rate};
use phosphor_core::core::debug::{DebugRegister, Debuggable};
use phosphor_core::core::{AccessKind, AddressSpace16, Bus, BusMaster};
use phosphor_core::cpu::Cpu;
use phosphor_core::cpu::z80::Z80;
use phosphor_core::device::Ay8910;
use phosphor_macros::{BusDebug, MemoryRegion, Saveable};

use phosphor_core::device::Device;

/// Debug index of the sound Z80: Time Pilot is one main Z80 (index 0) plus
/// this board, so the index is a property of its place in the machine. It is
/// the same fact that `#[debug_map(cpu = 1)]` below states to the derive.
pub(crate) const SOUND_CPU_INDEX: usize = 1;

/// Regions of the sound Z80's memory space.
///
/// The AY-8910s answer in the memory map (not Z80 I/O space) but carry no
/// bytes, so they are decoded by hand in the `Bus` impl rather than declared
/// here.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum Region {
    Rom = 1,
    Ram = 2,
    /// The filter select window, which carries its data in the address rather
    /// than on the data bus.
    Filter = 3,
}

/// One AY channel's RC lowpass: the capacitor the filter write selected plus
/// the pole's memory. The coefficient is computed per call from the cap bits
/// (MAME `filter_rc LOWPASS_3R` with R1=1K, R2=5.1K, R3=0 and C from the two
/// bit-selected capacitors), because the program switches caps at runtime and
/// has to do that without discarding the state that makes it a filter.
#[derive(Clone, Copy, Debug, Saveable)]
#[save_version(1)]
#[save_tlv]
struct VoiceFilter {
    /// 2 bits: bit 0 = 220 nF, bit 1 = 47 nF, 0 = filter disabled (bypass).
    #[save(id = 1)]
    cap: u8,
    #[save(id = 2)]
    mem: f32,
}

impl VoiceFilter {
    /// `1 - exp(-1/(Req*C*fs))`, MAME `filter_rc_device::recalc` for
    /// LOWPASS_3R, with `Req = R1*(R2+R3)/(R1+R2+R3)`.
    fn coefficient(&self) -> f32 {
        const REQ: f64 = 1000.0 * 5100.0 / 6100.0;
        let c = match self.cap & 3 {
            0 => return 1.0, // filter disabled: bypass
            1 => 220e-9,
            2 => 47e-9,
            _ => 267e-9,
        };
        let fs = host_sample_rate() as f64;
        (1.0 - (-1.0 / (REQ * c) / fs).exp()) as f32
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        self.mem += (x - self.mem) * self.coefficient();
        self.mem
    }
}

/// Time Pilot sound board.
///
/// `BusDebug` is derived rather than left to the machine that owns the board:
/// the sound Z80 is a field of *this* struct, so a derive running on the main
/// board could never see it. The machine merges this tree in with
/// `#[debug_bus]` on its own `sound` field.
#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct TimePilotSound {
    // `index = 1` is `SOUND_CPU_INDEX` spelled as a literal, which is what the
    // attribute parser takes.
    #[debug_cpu("Z80 Sound", index = 1)]
    #[save(id = 1)]
    cpu: Z80,
    /// Everything the sound CPU talks to. Held apart from the CPU so a cycle
    /// dispatches at a concrete bus rather than a trait object -- see
    /// `docs/designs/concrete-bus-dispatch.md`.
    #[debug_bus]
    #[save(id = 2)]
    bus: TimePilotSoundBus,
}

/// The sound Z80's bus: PSGs, memory, the command latch and the timer.
#[derive(BusDebug, Saveable)]
#[save_version(1)]
#[save_tlv]
struct TimePilotSoundBus {
    /// AY1 (index 0) carries the latch/timer on its input ports; AY2 is
    /// audio only. Both deliver per-channel outputs so each channel meets
    /// its own RC lowpass.
    #[save(id = 1)]
    ay: [Ay8910; 2],

    /// The ROM, the RAM and its mirrors, and a named entry for the filter
    /// select window. This is what carries watchpoints and the write-event
    /// ring on the sound side.
    #[debug_map(cpu = 1)]
    #[save(id = 2)]
    map: AddressSpace16,

    /// Latched command from the main board (the LS273 at G6).
    #[save(id = 3)]
    command: u8,

    // IRQ generation (HOLD_LINE-style: set on the main latch bit-2 rising
    // edge, cleared when the sound CPU reads the command latch via AY1
    // port A, which is the acknowledge the sound program performs).
    #[save(id = 4)]
    irq_pending: bool,
    #[save(id = 5)]
    mute: bool,

    /// Last filter select written (12 bits, 2 per channel), for debug/state.
    #[save(id = 6)]
    filter_bits: u16,
    /// Six RC voices, AY1 channels 0-2 then AY2 channels 0-2.
    #[save(id = 7)]
    filters: [VoiceFilter; 6],

    // Audio resampler (mixes the filtered AY channels).
    #[save(id = 8)]
    resampler: AudioResampler<i16>,

    // Total sound-CPU cycles (drives the timer).
    #[save(id = 9)]
    clock: u64,
}

/// The divide-by-5120 timer read on AY1 port B (MAME `portB_r`): a
/// divide-by-512 plus an LS90 bi-quinary divide-by-10, indexed by
/// `(total_cycles / 512) % 10`.
const TIMER_TABLE: [u8; 10] = [0x00, 0x10, 0x20, 0x30, 0x40, 0x90, 0xa0, 0xb0, 0xa0, 0xd0];

impl TimePilotSound {
    /// Create the board. Call `load_rom` before use.
    ///
    /// `cpu_clock_hz` is the sound Z80's rate, which the AY-8910s share. The
    /// caller supplies it rather than this file naming it, so the rate the
    /// board is *stepped* at and the rate its PSGs and resampler are told
    /// cannot be two separately-written numbers.
    pub fn new(cpu_clock_hz: u64) -> Self {
        let mut ay0 = Ay8910::new(cpu_clock_hz);
        let mut ay1 = Ay8910::new(cpu_clock_hz);
        ay0.enable_channel_outputs();
        ay1.enable_channel_outputs();
        Self {
            cpu: Z80::new(),
            bus: TimePilotSoundBus {
                ay: [ay0, ay1],
                map: TimePilotSoundBus::build_map(),
                command: 0,
                irq_pending: false,
                mute: false,
                filter_bits: 0,
                filters: [VoiceFilter { cap: 0, mem: 0.0 }; 6],
                resampler: AudioResampler::new(cpu_clock_hz, host_sample_rate() as u64),
                clock: 0,
            },
        }
    }

    /// The sound CPU rate this board was built with.
    ///
    /// Read back from the audio path rather than from a stored copy, so it
    /// reports what the resampler actually got.
    pub fn cpu_clock_hz(&self) -> u64 {
        self.bus.resampler.input_rate()
    }

    /// The AY-8910s' chip clock, which is the same signal as the sound CPU's.
    pub fn psg_clock_hz(&self) -> u64 {
        self.bus.ay[0].chip_clock_hz()
    }

    /// Load sound ROM data (tm7, 4 KB at 0x0000; the rest of the window reads
    /// open bus).
    pub fn load_rom(&mut self, data: &[u8]) {
        let region = self.bus.map.region_data_mut(Region::Rom);
        let len = data.len().min(region.len());
        region[..len].copy_from_slice(&data[..len]);
        region[len..].fill(0xFF);
    }

    /// Whether the sound Z80 is between instructions.
    pub fn at_instruction_boundary(&self) -> bool {
        self.cpu.at_instruction_boundary()
    }

    // -----------------------------------------------------------------------
    // Main-board interface (the sound latch, IRQ trigger and mute)
    // -----------------------------------------------------------------------

    /// Latch a command byte from the main board (a 0xC000 write strobes it).
    pub fn write_command(&mut self, data: u8) {
        self.bus.command = data;
    }

    /// Pulse the sound CPU IRQ (the main board calls this on its latch
    /// bit-2 rising edge).
    pub fn pulse_irq(&mut self) {
        self.bus.irq_pending = true;
    }

    /// Mute the output stage (main latch bit 3, the LA4460 mute pin).
    pub fn set_mute(&mut self, mute: bool) {
        self.bus.mute = mute;
    }

    /// Sound IRQ line state, for the board's latch tests.
    #[cfg(test)]
    pub(crate) fn check_interrupts_for_test(
        &mut self,
        target: BusMaster,
    ) -> phosphor_core::core::bus::InterruptState {
        self.bus.check_interrupts(target)
    }

    /// Acknowledge the held IRQ the way the sound program does: latch AY1
    /// register 14 and read the command.
    #[cfg(test)]
    pub(crate) fn acknowledge_for_test(&mut self) {
        self.bus.ay[0].address_write(14);
        self.bus.ay1_data_read();
    }

    // -----------------------------------------------------------------------
    // Tick (called at the sound-CPU clock rate)
    // -----------------------------------------------------------------------

    /// Advance the board by one sound-CPU clock: present the command/timer on
    /// AY1's input ports, run one Z80 cycle, tick the AYs, and filter, mix
    /// and accumulate one audio sample per channel the AYs produced.
    pub fn tick(&mut self) {
        // AY1 reads the command on port A and the timer on port B.
        let b = &mut self.bus;
        b.ay[0].set_port_a(b.command);
        let timer = b.timer();
        b.ay[0].set_port_b(timer);

        // Bus dispatch cannot read CPU state while the CPU is mid-cycle, so the
        // cycle and instruction address a hit is attributed to are latched here.
        if self.bus.map.debug_active() {
            let pc = self
                .cpu
                .at_instruction_boundary()
                .then_some(u32::from(self.cpu.pc));
            self.bus.map.latch_access_context(self.bus.clock, pc);
        }

        self.cpu
            .execute_cycle(&mut self.bus, BusMaster::Cpu(SOUND_CPU_INDEX));

        let b = &mut self.bus;
        b.ay[0].tick();
        b.ay[1].tick();

        // Both AYs share one clock, so all six channels produce the same
        // count; each side is still drained separately so a phase slip degrades
        // to a silent side rather than a stuck one.
        let mut ch = [0i16; 6];
        let n0 = b.ay[0].fill_channel_audio(0, &mut ch[0..1]);
        b.ay[0].fill_channel_audio(1, &mut ch[1..2]);
        b.ay[0].fill_channel_audio(2, &mut ch[2..3]);
        let n1 = b.ay[1].fill_channel_audio(0, &mut ch[3..4]);
        b.ay[1].fill_channel_audio(1, &mut ch[4..5]);
        b.ay[1].fill_channel_audio(2, &mut ch[5..6]);
        if n0 > 0 || n1 > 0 {
            let mut mixed = 0.0f32;
            for (i, filter) in b.filters.iter_mut().enumerate() {
                let produced = if i < 3 { n0 > 0 } else { n1 > 0 };
                let v = if produced { ch[i] as f32 } else { 0.0 };
                mixed += filter.process(v);
            }
            // Each AY channel routes at 0.60 into the mono mix (MAME).
            mixed *= 0.6;
            if b.mute {
                mixed = 0.0;
            }
            b.resampler
                .push_sample(mixed.clamp(-32768.0, 32767.0) as i16);
        }

        b.clock += 1;
    }

    /// Drain accumulated audio samples. Returns the number written.
    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.bus.resampler.fill_audio(buffer)
    }

    /// Reset the board to power-on state.
    pub fn reset(&mut self) {
        self.cpu
            .reset(&mut self.bus, BusMaster::Cpu(SOUND_CPU_INDEX));
        self.bus.ay[0].reset();
        self.bus.ay[1].reset();
        self.bus.map.region_data_mut(Region::Ram).fill(0);
        self.bus.command = 0;
        self.bus.irq_pending = false;
        self.bus.mute = false;
        self.bus.filter_bits = 0;
        self.bus.filters = [VoiceFilter { cap: 0, mem: 0.0 }; 6];
        self.bus.resampler.reset();
        self.bus.clock = 0;
    }
}

// ---------------------------------------------------------------------------
// Bus implementation (sound Z80's memory map)
// ---------------------------------------------------------------------------

impl TimePilotSoundBus {
    /// Lay out the sound Z80's memory space.
    ///
    /// The 1 KB RAM is decoded on ten address lines and answers repeatedly
    /// across 0x3000-0x3FFF, so the repeats are declared as mirrors: a
    /// watchpoint is set on the address the CPU puts on the bus, and the
    /// program is free to use any of them.
    fn build_map() -> AddressSpace16 {
        let mut map = AddressSpace16::new();
        map.region(
            Region::Rom,
            "Sound ROM",
            0x0000,
            0x3000,
            AccessKind::ReadOnly,
        )
        .region(
            Region::Ram,
            "Sound RAM",
            0x3000,
            0x0400,
            AccessKind::ReadWrite,
        )
        .region(
            Region::Filter,
            "Filter select",
            0x8000,
            0x8000,
            AccessKind::Io,
        );
        map.mirror(0x3400, 0x3000, 0x0400);
        map.mirror(0x3800, 0x3000, 0x0400);
        map.mirror(0x3c00, 0x3000, 0x0400);
        map
    }

    /// True if `addr` lands in the RAM window, mirrors included.
    #[inline]
    fn is_ram(&self, addr: u16) -> bool {
        self.map.page(addr).region_id == Region::RAM
    }
}

impl Bus for TimePilotSoundBus {
    type Address = u16;
    type Data = u8;

    fn read(&mut self, master: BusMaster, addr: u16) -> u8 {
        // Fast path: ROM and RAM, which is most of what this bus answers. The
        // AY pages and the filter window carry no bytes, so `fast_read`
        // declines and the match below answers. See `AddressSpace16::fast_read`.
        if let Some(data) = self.map.fast_read(addr) {
            return data;
        }
        let data = match addr & 0xF000 {
            0x4000 => self.ay1_data_read(),
            0x6000 => self.ay[1].data_read(),
            _ => match self.map.page(addr).region_id {
                Region::ROM | Region::RAM => self.map.read_backing(addr),
                _ => 0xFF,
            },
        };
        self.map.watch_read(SOUND_CPU_INDEX, master, addr, data);
        data
    }

    fn write(&mut self, master: BusMaster, addr: u16, data: u8) {
        // Before the side effect, so a hit's metadata snapshot is pre-write.
        self.map.watch_write(SOUND_CPU_INDEX, master, addr, data);
        match addr & 0xF000 {
            0x4000 => self.ay[0].data_write(data),
            0x5000 => self.ay[0].address_write(data),
            0x6000 => self.ay[1].data_write(data),
            0x7000 => self.ay[1].address_write(data),
            _ => {
                if self.is_ram(addr) {
                    self.map.write_backing(addr, data);
                } else if self.map.page(addr).region_id == Region::FILTER {
                    // The *offset* carries the filter bits (2 per channel).
                    self.filter_write(addr & 0x0FFF);
                }
            }
        }
    }

    fn is_halted_for(&self, _master: BusMaster) -> bool {
        false
    }

    fn check_interrupts(&mut self, _target: BusMaster) -> phosphor_core::core::bus::InterruptState {
        phosphor_core::core::bus::InterruptState {
            nmi: false,
            irq: self.irq_pending,
            firq: false,
            irq_vector: 0xFF,
            irq_level: 0,
        }
    }
}

impl TimePilotSoundBus {
    /// The divide-by-5120 timer presented on AY1 port B.
    fn timer(&self) -> u8 {
        TIMER_TABLE[((self.clock / 512) % 10) as usize]
    }

    /// Read AY1's data port. Reading port A (register 14) is the command-latch
    /// fetch, which acknowledges and clears the held IRQ.
    fn ay1_data_read(&mut self) -> u8 {
        if self.ay[0].latched_register() == 14 {
            self.irq_pending = false;
        }
        self.ay[0].data_read()
    }

    /// Apply a filter select (MAME `filter_w`): 12 bits, 2 per AY channel,
    /// AY2 first. Selecting no capacitor disables that voice's filter, which
    /// also zeroes its memory.
    fn filter_write(&mut self, bits: u16) {
        self.filter_bits = bits;
        let caps = [
            ((bits >> 6) & 3) as u8,
            ((bits >> 8) & 3) as u8,
            ((bits >> 10) & 3) as u8,
            (bits & 3) as u8,
            ((bits >> 2) & 3) as u8,
            ((bits >> 4) & 3) as u8,
        ];
        for (filter, cap) in self.filters.iter_mut().zip(caps) {
            filter.cap = cap;
            if cap == 0 {
                filter.mem = 0.0;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Device trait
// ---------------------------------------------------------------------------

impl Device for TimePilotSound {
    fn name(&self) -> &'static str {
        "Time Pilot Sound"
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

impl Debuggable for TimePilotSound {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "COMMAND",
                value: self.bus.command as u64,
                width: 8,
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
            DebugRegister {
                name: "FILTER",
                value: self.bus.filter_bits as u64,
                width: 12,
            },
        ]
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::save_state::{Saveable, StateReader, StateWriter};

    /// The rate a Time Pilot board supplies: its 14.318181 MHz sound crystal
    /// over eight. Nothing here depends on the exact value, but using the real
    /// one keeps the device's tests honest about what it is fed.
    const TEST_CPU_CLOCK: u64 = 14_318_181 / 8;

    fn sound_bus() -> TimePilotSoundBus {
        TimePilotSound::new(TEST_CPU_CLOCK).bus
    }

    #[test]
    fn ram_round_trips_through_its_mirrors() {
        let mut b = sound_bus();
        b.write(BusMaster::Cpu(SOUND_CPU_INDEX), 0x3000, 0xab);
        for addr in [0x3000, 0x3400, 0x3800, 0x3c00] {
            assert_eq!(
                b.read(BusMaster::Cpu(SOUND_CPU_INDEX), addr),
                0xab,
                "mirror at {addr:#06x}"
            );
        }
    }

    #[test]
    fn rom_tail_reads_open_bus_after_load() {
        let mut s = TimePilotSound::new(TEST_CPU_CLOCK);
        s.load_rom(&[0x11; 0x1000]);
        assert_eq!(s.bus.read(BusMaster::Cpu(SOUND_CPU_INDEX), 0x0000), 0x11);
        assert_eq!(
            s.bus.read(BusMaster::Cpu(SOUND_CPU_INDEX), 0x1000),
            0xFF,
            "past tm7 the window is unpopulated"
        );
    }

    #[test]
    fn ay_pages_decode_by_high_nibble() {
        let mut b = sound_bus();
        // AY1 address then data: R7 = 0x3F (ports A/B input).
        b.write(BusMaster::Cpu(SOUND_CPU_INDEX), 0x5ABC, 7);
        b.write(BusMaster::Cpu(SOUND_CPU_INDEX), 0x4DEF, 0x3F);
        b.write(BusMaster::Cpu(SOUND_CPU_INDEX), 0x5ABC, 14);
        // AY2 data write lands on AY2, not AY1.
        b.write(BusMaster::Cpu(SOUND_CPU_INDEX), 0x7AAA, 8);
        b.write(BusMaster::Cpu(SOUND_CPU_INDEX), 0x6BBB, 0x0F);
        assert_eq!(b.ay[0].latched_register(), 14);
        // AY1 port A returns the command latch once the tick presents it.
        let mut s = TimePilotSound::new(TEST_CPU_CLOCK);
        s.write_command(0x5A);
        s.tick();
        s.bus.write(BusMaster::Cpu(SOUND_CPU_INDEX), 0x5000, 14);
        assert_eq!(s.bus.read(BusMaster::Cpu(SOUND_CPU_INDEX), 0x4000), 0x5A);
    }

    #[test]
    fn latch_read_clears_the_held_irq() {
        let mut s = TimePilotSound::new(TEST_CPU_CLOCK);
        s.pulse_irq();
        let state = s.bus.check_interrupts(BusMaster::Cpu(SOUND_CPU_INDEX));
        assert!(state.irq, "pulsed IRQ is pending");
        // A data read with any other register latched leaves it pending.
        s.bus.write(BusMaster::Cpu(SOUND_CPU_INDEX), 0x5000, 0);
        s.bus.read(BusMaster::Cpu(SOUND_CPU_INDEX), 0x4000);
        assert!(s.bus.irq_pending);
        // Reading port A (register 14) acknowledges it.
        s.bus.write(BusMaster::Cpu(SOUND_CPU_INDEX), 0x5000, 14);
        s.bus.read(BusMaster::Cpu(SOUND_CPU_INDEX), 0x4000);
        assert!(!s.bus.irq_pending);
    }

    #[test]
    fn timer_walks_the_ls90_table() {
        let mut s = TimePilotSound::new(TEST_CPU_CLOCK);
        for (i, expected) in TIMER_TABLE.iter().enumerate() {
            s.bus.clock = i as u64 * 512;
            assert_eq!(s.bus.timer(), *expected, "step {i}");
        }
        s.bus.clock = 10 * 512;
        assert_eq!(s.bus.timer(), TIMER_TABLE[0], "table wraps");
    }

    #[test]
    fn filter_write_routes_two_bits_per_channel() {
        let mut b = sound_bus();
        b.write(BusMaster::Cpu(SOUND_CPU_INDEX), 0x8E4C, 0);
        assert_eq!(b.filter_bits, 0x0E4C);
        // AY2 channels take bits 0-5, AY1 channels bits 6-11.
        let caps: Vec<u8> = b.filters.iter().map(|f| f.cap).collect();
        assert_eq!(caps, vec![1, 2, 3, 0, 3, 0]);
        // Selecting no capacitor zeroes that voice's memory.
        b.filters[3].mem = 0.5;
        b.write(BusMaster::Cpu(SOUND_CPU_INDEX), 0x8000, 0);
        assert_eq!(b.filters[3].mem, 0.0);
    }

    #[test]
    fn disabled_filter_passes_audio_through() {
        let mut f = VoiceFilter { cap: 0, mem: 0.0 };
        assert_eq!(f.process(0.25), 0.25);
        // An enabled filter moves toward the input asymptotically, never past.
        f.cap = 1;
        f.mem = 0.0;
        let y = f.process(1.0);
        assert!(y > 0.0 && y < 1.0, "one pole step, got {y}");
    }

    #[test]
    fn save_round_trips_command_irq_and_filters() {
        let mut s = TimePilotSound::new(TEST_CPU_CLOCK);
        s.write_command(0xA5);
        s.pulse_irq();
        s.set_mute(true);
        s.bus.write(BusMaster::Cpu(SOUND_CPU_INDEX), 0x8123, 0);
        let mut w = StateWriter::new();
        s.save_state(&mut w);
        let bytes = w.into_vec();

        let mut s2 = TimePilotSound::new(TEST_CPU_CLOCK);
        let mut r = StateReader::new(&bytes);
        s2.load_state(&mut r).unwrap();
        assert_eq!(s2.bus.command, 0xA5);
        assert!(s2.bus.irq_pending);
        assert!(s2.bus.mute);
        assert_eq!(s2.bus.filter_bits, 0x0123);
    }
}
