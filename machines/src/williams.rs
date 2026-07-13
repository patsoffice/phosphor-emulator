use phosphor_core::audio::AudioResampler;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind, DebugTraceBuffer};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::watchpoint::DebugAccessSource;
use phosphor_core::core::{AccessKind, AddressSpace16};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::CpuStateTrait;
use phosphor_core::cpu::m6800::M6800;
use phosphor_core::cpu::m6809::M6809;
use phosphor_core::cpu::state::{M6800State, M6809State};
use phosphor_core::device::dac::Mc1408Dac;
use phosphor_core::device::hc55516::Hc55516;
use phosphor_core::device::pia6820::Pia6820;
use phosphor_core::device::williams_blitter::WilliamsBlitter;
use phosphor_macros::{BusDebug, DebugTrace, MemoryRegion};

use crate::rom_loader::{RomEntry, RomLoadError, RomRegion, RomSet};

// ---------------------------------------------------------------------------
// Memory map region IDs (machine-specific constants for page table dispatch)
// ---------------------------------------------------------------------------

/// Main CPU (M6809) address space region IDs.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum MainRegion {
    VideoRam = 1,   // 0x0000-0xBFFF (48KB, banked ROM overlay at 0x0000-0x8FFF)
    Palette = 2,    // 0xC000-0xC00F (16-color palette)
    IoPia = 3,      // 0xC800-0xC8FF (Widget PIA + ROM PIA)
    IoBank = 4,     // 0xC900-0xC9FF (ROM bank select register)
    IoBlitter = 5,  // 0xCA00-0xCAFF (SC1 blitter registers)
    IoVideo = 6,    // 0xCB00-0xCBFF (video counter + watchdog)
    Cmos = 7,       // 0xCC00-0xCFFF (1KB battery-backed CMOS)
    ProgramRom = 8, // 0xD000-0xFFFF standard (12KB); 0xE000-0xFFFF on extra-RAM boards (8KB)
    BankedRom = 9,  // (36KB, overlays VIDEO_RAM when bank != 0)
    Sram = 10,      // 0xD000-0xDFFF (4KB work RAM, extra-RAM boards only — e.g. Sinistar)
}

/// Sound CPU (M6800) address space region IDs.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, MemoryRegion)]
pub(crate) enum SoundRegion {
    Ram = 1,   // 0x0000-0x00FF (256 bytes)
    IoPia = 2, // 0x0400-0x04FF (Sound PIA)
    Rom = 3,   // 0xB000-0xFFFF (4KB mirrored)
}

// ---------------------------------------------------------------------------
// Williams gen-1 hardware constants
// ---------------------------------------------------------------------------

pub const TIMING: TimingConfig = TimingConfig {
    cpu_clock_hz: 1_000_000, // E clock = 4 MHz XTAL ÷ 4
    cycles_per_scanline: 64, // 1 MHz / ~15.6 kHz horizontal
    total_scanlines: 260,    // 260 lines per frame
    display_width: 292,      // native display width after cropping
    display_height: 240,     // native display height after cropping
    display_aspect: Some((4, 3)),
};

// ---------------------------------------------------------------------------
// Shared ROM definitions (common to all Williams gen-1 games)
// ---------------------------------------------------------------------------

/// Decoder PROMs: 2 × 512B, identical across all gen-1 boards.
pub static WILLIAMS_DECODER_PROM: RomRegion = RomRegion {
    size: 0x0400,
    entries: &[
        RomEntry {
            name: "decoder_rom_4.3g",
            size: 0x0200,
            offset: 0x0000,
            crc32: &[0xe6631c23],
        },
        RomEntry {
            name: "decoder_rom_6.3c",
            size: 0x0200,
            offset: 0x0200,
            crc32: &[0x83faf25e],
        },
    ],
};

/// SC-1 sound board ROM: 4KB, shared by Joust, Robotron, Bubbles, etc.
pub static WILLIAMS_SOUND_ROM: RomRegion = RomRegion {
    size: 0x1000,
    entries: &[RomEntry {
        name: "video_sound_rom_4_std_780.ic12",
        size: 0x1000,
        offset: 0x0000,
        crc32: &[0xf1835bdd],
    }],
};

// ---------------------------------------------------------------------------
// Board variant configuration
// ---------------------------------------------------------------------------

/// Construction-time hardware variant selection for [`WilliamsBoard`].
///
/// The shared board covers Williams gen-1 games (Joust, Robotron, …) with the
/// standard memory map. A few later games on the same board reorganize memory or
/// add sound hardware; this config selects those deltas at construction. It is a
/// `Copy` value fixed at build time — **not** part of the save-state byte stream.
///
/// Additional fields (blitter window-clip, CVSD speech) are introduced by the
/// issues that implement those features; this struct grows as they land.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WilliamsConfig {
    /// Map 0xD000-0xDFFF as 4KB work RAM (blittable) with program ROM shrunk to
    /// 0xE000-0xFFFF, instead of the standard 12KB program ROM at 0xD000-0xFFFF.
    /// Used by Sinistar (the extra work-RAM memory layout).
    pub extra_sram_dxxx: bool,
    /// Populate the sound ROM as a full 20KB window at 0xB000-0xFFFF (five 4KB
    /// chips, e.g. Sinistar's four speech ROMs + the standard sound ROM), rather
    /// than a single 4KB ROM at 0xF000 mirrored down to 0xB000.
    pub sound_rom_20k: bool,
    /// Fit an HC55516 CVSD speech decoder, driven by the sound PIA's CA2 (data)
    /// and CB2 (clock) lines and mixed with the DAC. Used by Sinistar.
    pub has_cvsd: bool,
}

impl WilliamsConfig {
    /// Standard Williams gen-1 layout (Joust, Robotron, Bubbles, …).
    pub const fn gen1_standard() -> Self {
        Self {
            extra_sram_dxxx: false,
            sound_rom_20k: false,
            has_cvsd: false,
        }
    }

    /// Sinistar (1982): 4KB work RAM at $D000 + 8KB program ROM at $E000, a 20KB
    /// sound ROM window (speech + standard), and the HC55516 CVSD speech decoder.
    /// The blitter window-clip is layered on by a later issue.
    pub const fn sinistar() -> Self {
        Self {
            extra_sram_dxxx: true,
            sound_rom_20k: true,
            has_cvsd: true,
        }
    }
}

impl Default for WilliamsConfig {
    fn default() -> Self {
        Self::gen1_standard()
    }
}

// ---------------------------------------------------------------------------
// WilliamsBoard
// ---------------------------------------------------------------------------

/// Williams gen-1 arcade board hardware.
///
/// Contains all shared hardware: M6809E main CPU @ 1 MHz, M6800 sound CPU,
/// 48KB video RAM, two MC6821 PIAs, Williams SC1 blitter, 1KB battery-backed
/// CMOS RAM, 12KB program ROM, sound board with DAC.
///
/// Game-specific machines (Joust, Robotron, etc.) compose this struct and
/// provide their own ROM definitions and input wiring.
#[derive(BusDebug, DebugTrace)]
pub struct WilliamsBoard {
    // CPUs (debug reads/writes auto-routed through matching #[debug_map])
    #[debug_cpu("M6809 Main")]
    pub(crate) cpu: M6809,
    #[debug_cpu("M6800 Sound")]
    pub(crate) sound_cpu: M6800,

    // Peripheral devices
    #[debug_device("Widget PIA")]
    pub(crate) widget_pia: Pia6820, // 0xC804-0xC807: player inputs
    #[debug_device("ROM PIA")]
    pub(crate) rom_pia: Pia6820, // 0xC80C-0xC80F: ROM bank, video timing
    #[debug_device("Blitter")]
    pub(crate) blitter: WilliamsBlitter, // 0xCA00-0xCA07: DMA blitter

    // I/O registers
    pub(crate) rom_bank: u8, // 0xC900: ROM bank select

    // Sound board
    #[debug_device("Sound PIA")]
    pub(crate) sound_pia: Pia6820, // 0x0400-0x0403: Sound PIA

    // Audio output
    #[debug_device("DAC")]
    pub(crate) dac: Mc1408Dac,
    /// HC55516 CVSD speech decoder (extra-sound boards only, e.g. Sinistar).
    pub(crate) cvsd: Option<Hc55516>,
    pub(crate) resampler: AudioResampler<i16>,

    // Memory maps (page-table dispatch + watchpoints + backing memory)
    // All RAM/ROM storage lives in the AddressSpace16 backing store.
    #[debug_map(cpu = 0)]
    pub(crate) main_map: AddressSpace16,
    #[debug_map(cpu = 1)]
    pub(crate) sound_map: AddressSpace16,

    // Board variant (fixed at construction; not part of save state)
    pub(crate) config: WilliamsConfig,

    // System state
    pub watchdog_counter: u32,
    pub(crate) clock: u64,

    // ROM PIA Port A input (game sets coin/service bits)
    pub(crate) rom_pia_input: u8,

    // Scanline-rendered framebuffer (292 × 240 × RGB24)
    pub(crate) scanline_buffer: Vec<u8>,

    // Debug event ring (observer state — never saved in save states)
    #[debug_events]
    pub(crate) debug_trace: DebugTraceBuffer,
}

impl WilliamsBoard {
    /// Construct a standard Williams gen-1 board (Joust, Robotron, …).
    pub fn new() -> Self {
        Self::with_config(WilliamsConfig::gen1_standard())
    }

    /// Construct a board with a specific hardware variant (see [`WilliamsConfig`]).
    pub fn with_config(config: WilliamsConfig) -> Self {
        Self {
            cpu: M6809::new(),
            sound_cpu: M6800::new(),
            widget_pia: Pia6820::new(),
            rom_pia: Pia6820::new(),
            blitter: WilliamsBlitter::new(),
            rom_bank: 0,
            sound_pia: Pia6820::new(),
            dac: Mc1408Dac::new(),
            cvsd: config.has_cvsd.then(Hc55516::new),
            resampler: AudioResampler::new(1_000_000, 44_100),
            main_map: Self::build_main_map(config),
            sound_map: Self::build_sound_map(config),
            config,
            watchdog_counter: 0,
            clock: 0,
            rom_pia_input: 0,
            scanline_buffer: vec![
                0u8;
                TIMING.display_width as usize * TIMING.display_height as usize * 3
            ],
            debug_trace: DebugTraceBuffer::new(),
        }
    }

    fn build_main_map(config: WilliamsConfig) -> AddressSpace16 {
        use MainRegion::*;
        let mut map = AddressSpace16::new();
        map.region(VideoRam, "Video RAM", 0x0000, 0xC000, AccessKind::ReadWrite)
            .region(Palette, "Palette", 0xC000, 0x100, AccessKind::ReadWrite)
            .region(IoPia, "PIAs", 0xC800, 0x100, AccessKind::Io)
            .region(IoBank, "ROM Bank", 0xC900, 0x100, AccessKind::Io)
            .region(IoBlitter, "Blitter", 0xCA00, 0x100, AccessKind::Io)
            .region(IoVideo, "Video Counter", 0xCB00, 0x100, AccessKind::Io)
            .region(Cmos, "CMOS RAM", 0xCC00, 0x400, AccessKind::ReadWrite);
        if config.extra_sram_dxxx {
            // Extra-RAM boards (Sinistar): 4KB work RAM at 0xD000, 8KB ROM at 0xE000.
            map.region(Sram, "SRAM", 0xD000, 0x1000, AccessKind::ReadWrite)
                .region(
                    ProgramRom,
                    "Program ROM",
                    0xE000,
                    0x2000,
                    AccessKind::ReadOnly,
                );
        } else {
            // Standard layout: 12KB program ROM at 0xD000-0xFFFF.
            map.region(
                ProgramRom,
                "Program ROM",
                0xD000,
                0x3000,
                AccessKind::ReadOnly,
            );
        }
        map.backing_region(BankedRom, "Banked ROM", 0x9000);
        map
    }

    fn build_sound_map(config: WilliamsConfig) -> AddressSpace16 {
        use SoundRegion::*;
        let mut map = AddressSpace16::new();
        map.region(Ram, "Sound RAM", 0x0000, 0x100, AccessKind::ReadWrite)
            .region(IoPia, "Sound PIA", 0x0400, 0x100, AccessKind::Io);
        if config.sound_rom_20k {
            // 20KB sound ROM window (Sinistar: 4 speech ROMs + standard sound ROM).
            map.region(Rom, "Sound ROM", 0xB000, 0x5000, AccessKind::ReadOnly);
        } else {
            // Single 4KB sound ROM at 0xF000, mirrored down to 0xB000.
            map.region(Rom, "Sound ROM", 0xF000, 0x1000, AccessKind::ReadOnly)
                .mirror(0xB000, 0xF000, 0x1000)
                .mirror(0xC000, 0xF000, 0x1000)
                .mirror(0xD000, 0xF000, 0x1000)
                .mirror(0xE000, 0xF000, 0x1000);
        }
        map
    }

    // --- Accessors ---

    pub fn get_cpu_state(&self) -> M6809State {
        self.cpu.snapshot()
    }

    pub fn get_sound_cpu_state(&self) -> M6800State {
        self.sound_cpu.snapshot()
    }

    pub fn read_video_ram(&self, addr: usize) -> u8 {
        let vram = self.main_map.region_data(MainRegion::VideoRam);
        if addr < vram.len() { vram[addr] } else { 0 }
    }

    pub fn write_video_ram(&mut self, addr: usize, data: u8) {
        let vram = self.main_map.region_data_mut(MainRegion::VideoRam);
        if addr < vram.len() {
            vram[addr] = data;
        }
    }

    pub fn read_palette(&self, index: usize) -> u8 {
        if index < 16 {
            self.main_map.region_data(MainRegion::Palette)[index]
        } else {
            0
        }
    }

    pub fn rom_bank(&self) -> u8 {
        self.rom_bank
    }

    pub fn clock(&self) -> u64 {
        self.clock
    }

    pub fn load_cmos(&mut self, data: &[u8]) {
        let cmos = self.main_map.region_data_mut(MainRegion::Cmos);
        let len = data.len().min(cmos.len());
        cmos[..len].copy_from_slice(&data[..len]);
    }

    pub fn save_cmos(&self) -> &[u8] {
        self.main_map.region_data(MainRegion::Cmos)
    }

    // --- ROM loading ---

    /// Load program ROM from a byte slice at the given offset.
    /// Offset is relative to the start of the ROM region: 0 = 0xD000 on the
    /// standard map, or 0xE000 on extra-RAM boards (`extra_sram_dxxx`).
    pub fn load_program_rom(&mut self, offset: usize, data: &[u8]) {
        self.main_map
            .load_region_at(MainRegion::ProgramRom, offset, data);
    }

    /// Load banked ROM from a byte slice at the given offset.
    /// Offset is relative to the start of the banked ROM region (0 = address 0x0000).
    pub fn load_banked_rom(&mut self, offset: usize, data: &[u8]) {
        self.main_map
            .load_region_at(MainRegion::BankedRom, offset, data);
    }

    /// Load sound ROM from a byte slice at the given offset.
    /// Offset is relative to the start of the sound ROM region: 0 = 0xF000 on the
    /// standard map, or 0xB000 on 20KB-sound boards (`sound_rom_20k`).
    pub fn load_sound_rom(&mut self, offset: usize, data: &[u8]) {
        self.sound_map
            .load_region_at(SoundRegion::Rom, offset, data);
    }

    /// Load ROMs from a RomSet using game-specific region definitions.
    pub fn load_rom_regions(
        &mut self,
        rom_set: &RomSet,
        banked_region: &RomRegion,
        program_region: &RomRegion,
        sound_rom_region: &RomRegion,
    ) -> Result<(), RomLoadError> {
        let banked_data = banked_region.load(rom_set)?;
        self.main_map
            .load_region(MainRegion::BankedRom, &banked_data);

        let rom_data = program_region.load(rom_set)?;
        self.main_map.load_region(MainRegion::ProgramRom, &rom_data);

        let sound_data = sound_rom_region.load(rom_set)?;
        self.sound_map.load_region(SoundRegion::Rom, &sound_data);

        Ok(())
    }

    // --- Internal timing/rendering ---

    /// Current scanline number derived from the master clock.
    fn current_scanline(&self) -> u8 {
        let frame_cycle = self.clock % TIMING.cycles_per_frame();
        (frame_cycle / TIMING.cycles_per_scanline) as u8
    }

    /// Render a single scanline from VRAM + palette into the internal scanline buffer.
    /// `scanline` is the raw scanline number (0-259); only call for visible lines (7-246).
    fn render_scanline(&mut self, scanline: usize) {
        const CROP_X: usize = 6;
        const CROP_Y: usize = 7;
        const WIDTH: usize = 292;
        const RG_LUT: [u8; 8] = [0, 38, 81, 118, 137, 174, 217, 255];
        const B_LUT: [u8; 4] = [0, 95, 160, 255];

        let palette = self.main_map.region_data(MainRegion::Palette);
        let vram = self.main_map.region_data(MainRegion::VideoRam);

        // Decode the current palette (16 entries, BBGGGRRR)
        let mut palette_rgb = [(0u8, 0u8, 0u8); 16];
        for (i, rgb) in palette_rgb.iter_mut().enumerate() {
            let entry = palette[i];
            *rgb = (
                RG_LUT[(entry & 0x07) as usize],
                RG_LUT[((entry >> 3) & 0x07) as usize],
                B_LUT[((entry >> 6) & 0x03) as usize],
            );
        }

        let screen_y = scanline - CROP_Y;
        let row_offset = screen_y * WIDTH * 3;

        for screen_x in 0..WIDTH {
            let pixel_x = screen_x + CROP_X;
            let byte_column = pixel_x / 2;
            let vram_addr = byte_column * 256 + scanline;

            let byte = if vram_addr < vram.len() {
                vram[vram_addr]
            } else {
                0
            };

            let color_index = if pixel_x & 1 == 0 {
                (byte >> 4) & 0x0F
            } else {
                byte & 0x0F
            };

            let (r, g, b) = palette_rgb[color_index as usize];
            let pixel_offset = row_offset + screen_x * 3;
            self.scanline_buffer[pixel_offset] = r;
            self.scanline_buffer[pixel_offset + 1] = g;
            self.scanline_buffer[pixel_offset + 2] = b;
        }
    }

    // --- Core tick ---

    pub fn tick(&mut self, bus: &mut dyn Bus<Address = u16, Data = u8>) {
        // Video timing signals on ROM PIA.
        // VA11 (scanline bit 5) → ROM PIA CB1, count240 → ROM PIA CA1.
        // These drive the main CPU's IRQ via ROM PIA interrupt outputs.
        let frame_cycle = self.clock % TIMING.cycles_per_frame();
        if frame_cycle.is_multiple_of(TIMING.cycles_per_scanline) {
            let scanline = (frame_cycle / TIMING.cycles_per_scanline) as u16;

            // Render this scanline from current VRAM + palette before the CPU
            // processes it, matching hardware CRT read timing.
            if (7..=246).contains(&scanline) {
                self.render_scanline(scanline as usize);
            }

            if scanline != 256 {
                // VA11: toggles every 32 scanlines
                self.rom_pia.set_cb1((scanline & 0x20) != 0);
            }
            // count240: asserted from scanline 240 through VBLANK
            self.rom_pia.set_ca1(scanline >= 240);
        }

        // Propagate sound commands from main board ROM PIA to sound board PIA.
        // High two bits are externally pulled high on real hardware.
        // CB1 is held low for 0xFF (silence sentinel), asserted high otherwise to
        // generate an IRQ on the sound CPU.
        if self.rom_pia.take_port_b_written() {
            let command = self.rom_pia.read_output_b() | 0xC0;
            self.sound_pia.set_port_b_input(command);
            self.sound_pia.set_cb1(command != 0xFF);
            if self.debug_trace.enabled() {
                self.debug_trace.record(DebugEvent {
                    value: Some(command as u32),
                    width: 1,
                    device: Some("Sound PIA"),
                    detail: Some("sound command"),
                    ..DebugEvent::new(
                        self.clock,
                        DebugAccessSource::Device("ROM PIA"),
                        DebugEventKind::DeviceWrite,
                    )
                });
            }
        }

        // Latch debug attribution context (cycle + instruction PC) before
        // CPU execution — bus dispatch cannot read CPU state mid-tick.
        // Both watchpoint hits and trace events draw PC from this latch.
        if self.main_map.has_any_watchpoints() || self.debug_trace.enabled() {
            let pc = self
                .cpu
                .at_instruction_boundary()
                .then_some(self.cpu.pc as u32);
            self.main_map.latch_access_context(self.clock, pc);
        }
        if self.sound_map.has_any_watchpoints() || self.debug_trace.enabled() {
            let pc = self
                .sound_cpu
                .at_instruction_boundary()
                .then_some(self.sound_cpu.pc as u32);
            self.sound_map.latch_access_context(self.clock, pc);
        }

        if self.blitter.is_active() {
            self.blitter.do_dma_cycle(bus);
        } else {
            self.cpu.execute_cycle(bus, BusMaster::Cpu(0));
        }
        // Sound CPU runs every cycle (separate bus, not halted by blitter)
        self.sound_cpu.execute_cycle(bus, BusMaster::Cpu(1));

        // DAC is continuously connected to sound PIA Port A output pins
        let dac_byte = self.sound_pia.read_output_a();
        self.dac.write(dac_byte);
        let mut sample = self.dac.sample_i16();

        // CVSD speech (Sinistar): the sound CPU bit-bangs the stream on the
        // sound PIA's CA2 (data) and CB2 (clock) lines. clock_w is edge-
        // triggered internally, so sampling both every cycle is faithful.
        if self.cvsd.is_some() {
            let digit = self.sound_pia.ca2_output();
            let clock = self.sound_pia.cb2_output();
            let cvsd = self.cvsd.as_mut().unwrap();
            cvsd.digit_w(digit);
            cvsd.clock_w(clock);
            // DAC and CVSD share the mono speaker; CVSD is routed at ~0.8.
            let cvsd_scaled = (cvsd.sample_i16() as i32 * 4) / 5;
            sample = (sample as i32 + cvsd_scaled).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        }

        // Bresenham downsample: 1 MHz CPU clock -> 44.1 kHz output
        self.resampler.tick(sample);

        self.clock += 1;
        self.watchdog_counter += 1;
    }

    // --- Reset ---

    pub fn reset(&mut self) {
        // Reset peripherals first so bus is in a known state
        self.widget_pia.reset();
        self.rom_pia.reset();
        self.sound_pia.reset();
        self.blitter.reset();
        self.rom_bank = 0;
        // Ensure pages 0x00-0x8F point to VIDEO_RAM (undo any bank switch)
        self.main_map
            .remap_pages(0x00, 0x90, MainRegion::VideoRam, 0);
        self.dac.reset();
        if let Some(cvsd) = &mut self.cvsd {
            cvsd.reset();
        }
        self.resampler.reset();
        self.watchdog_counter = 0;
        self.clock = 0;
        self.rom_pia_input = 0;
        self.scanline_buffer.fill(0);
        // CMOS RAM and video RAM NOT cleared (battery-backed / not cleared by hardware)
        // CPU resets are done by the game wrapper via bus_split! since Bus is on the wrapper.
    }

    // --- Debug helpers ---

    /// Returns a bitmask of CPUs at instruction boundaries.
    /// Bit 0 = main CPU (M6809), bit 1 = sound CPU (M6800).
    pub fn debug_tick_boundaries(&self) -> u32 {
        let mut result = 0;
        if self.cpu.at_instruction_boundary() {
            result |= 1;
        }
        if self.sound_cpu.at_instruction_boundary() {
            result |= 2;
        }
        result
    }

    // --- Capability-trait helpers (called by game wrappers) ---

    pub fn render_frame(&self, buffer: &mut [u8]) {
        buffer.copy_from_slice(&self.scanline_buffer);
    }

    pub fn fill_audio(&mut self, buffer: &mut [i16]) -> usize {
        self.resampler.fill_audio(buffer)
    }
}

impl Saveable for WilliamsBoard {
    fn save_state(&self, w: &mut StateWriter) {
        // CPUs
        self.cpu.save_state(w);
        self.sound_cpu.save_state(w);
        // RAM
        w.write_bytes(self.main_map.region_data(MainRegion::VideoRam));
        w.write_bytes(&self.main_map.region_data(MainRegion::Palette)[..16]);
        w.write_bytes(self.main_map.region_data(MainRegion::Cmos));
        w.write_bytes(self.sound_map.region_data(SoundRegion::Ram));
        // Peripherals
        self.widget_pia.save_state(w);
        self.rom_pia.save_state(w);
        self.sound_pia.save_state(w);
        self.blitter.save_state(w);
        self.dac.save_state(w);
        // I/O & timing
        w.write_u8(self.rom_bank);
        self.resampler.save_state(w);
        w.write_u32_le(self.watchdog_counter);
        w.write_u64_le(self.clock);
        w.write_u8(self.rom_pia_input);
        // Variant state appended last (guarded by construction-time config) so
        // standard gen-1 saves stay byte-identical to the pre-variant layout.
        if self.config.extra_sram_dxxx {
            w.write_bytes(self.main_map.region_data(MainRegion::Sram));
        }
        if let Some(cvsd) = &self.cvsd {
            cvsd.save_state(w);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        // CPUs
        self.cpu.load_state(r)?;
        self.sound_cpu.load_state(r)?;
        // RAM
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::VideoRam))?;
        r.read_bytes_into(&mut self.main_map.region_data_mut(MainRegion::Palette)[..16])?;
        r.read_bytes_into(self.main_map.region_data_mut(MainRegion::Cmos))?;
        r.read_bytes_into(self.sound_map.region_data_mut(SoundRegion::Ram))?;
        // Peripherals
        self.widget_pia.load_state(r)?;
        self.rom_pia.load_state(r)?;
        self.sound_pia.load_state(r)?;
        self.blitter.load_state(r)?;
        self.dac.load_state(r)?;
        // I/O & timing
        self.rom_bank = r.read_u8()?;
        self.resampler.load_state(r)?;
        self.watchdog_counter = r.read_u32_le()?;
        self.clock = r.read_u64_le()?;
        self.rom_pia_input = r.read_u8()?;
        // Variant state (see save_state).
        if self.config.extra_sram_dxxx {
            r.read_bytes_into(self.main_map.region_data_mut(MainRegion::Sram))?;
        }
        if let Some(cvsd) = &mut self.cvsd {
            cvsd.load_state(r)?;
        }
        Ok(())
    }
}

impl Default for WilliamsBoard {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Bus dispatch helpers — Williams gen-1 memory map
// Called from game wrapper Bus impls (JoustSystem, RobotronSystem).
// ---------------------------------------------------------------------------

impl WilliamsBoard {
    /// Device that owns a main-board I/O address, for watchpoint attribution.
    fn main_device(addr: u16) -> Option<&'static str> {
        match addr {
            0xC804..=0xC807 => Some("Widget PIA"),
            0xC80C..=0xC80F => Some("ROM PIA"),
            0xCA00..=0xCA07 => Some("Blitter"),
            _ => None,
        }
    }

    /// Device that owns a sound-board I/O address, for watchpoint attribution.
    fn sound_device(addr: u16) -> Option<&'static str> {
        match addr {
            0x0400..=0x0403 => Some("Sound PIA"),
            _ => None,
        }
    }

    /// Record a main-board bus event. Callers gate on
    /// `self.debug_trace.enabled()` so event construction is skipped when
    /// tracing is off.
    fn trace_main_access(
        &mut self,
        master: BusMaster,
        kind: DebugEventKind,
        addr: u16,
        value: u8,
        detail: Option<&'static str>,
    ) {
        // The latched PC belongs to the main CPU; DMA carries no PC.
        let pc = match master {
            BusMaster::Cpu(0) => self.main_map.latched_pc(),
            _ => None,
        };
        self.debug_trace.record(DebugEvent {
            cpu_index: Some(0),
            pc,
            addr: Some(addr as u32),
            value: Some(value as u32),
            width: 1,
            region: self.main_map.region_at(addr).map(|r| r.name),
            device: Self::main_device(addr),
            detail,
            ..DebugEvent::new(self.clock, master.into(), kind)
        });
    }

    /// Record a sound-board bus event (see [`trace_main_access`](Self::trace_main_access)).
    fn trace_sound_access(
        &mut self,
        master: BusMaster,
        kind: DebugEventKind,
        addr: u16,
        value: u8,
    ) {
        let pc = match master {
            BusMaster::Cpu(1) => self.sound_map.latched_pc(),
            _ => None,
        };
        self.debug_trace.record(DebugEvent {
            cpu_index: Some(1),
            pc,
            addr: Some(addr as u32),
            value: Some(value as u32),
            width: 1,
            region: self.sound_map.region_at(addr).map(|r| r.name),
            device: Self::sound_device(addr),
            ..DebugEvent::new(self.clock, master.into(), kind)
        });
    }

    pub(crate) fn bus_read(&mut self, master: BusMaster, addr: u16) -> u8 {
        if master == BusMaster::Cpu(1) {
            // Sound board
            let data = match self.sound_map.page(addr).region_id {
                SoundRegion::IO_PIA if (0x0400..=0x0403).contains(&addr) => {
                    self.sound_pia.read(addr - 0x0400)
                }
                SoundRegion::RAM | SoundRegion::ROM => self.sound_map.read_backing(addr),
                _ => 0xFF,
            };
            self.sound_map
                .watch_read_with(1, master, addr, data, Self::sound_device(addr));
            // Trace device reads only — memory reads (instruction fetches)
            // would drown the ring.
            if self.debug_trace.enabled()
                && self.sound_map.page(addr).region_id == SoundRegion::IO_PIA
            {
                self.trace_sound_access(master, DebugEventKind::DeviceRead, addr, data);
            }
            return data;
        }

        // DmaVram reads bypass ROM banking — the blitter reads dest
        // directly from VRAM for keepmask blending.
        if master == BusMaster::DmaVram && addr <= 0x8FFF {
            let data = self.main_map.region_data(MainRegion::VideoRam)[addr as usize];
            self.main_map.watch_read(0, master, addr, data);
            if self.debug_trace.enabled() {
                self.trace_main_access(master, DebugEventKind::DmaRead, addr, data, None);
            }
            return data;
        }

        // Main board — backed regions use page-table dispatch (banking
        // is handled by remap_pages, so read_backing follows automatically)
        let data = match self.main_map.page(addr).region_id {
            MainRegion::PALETTE if addr <= 0xC00F => {
                self.main_map.region_data(MainRegion::Palette)[(addr & 0x0F) as usize]
            }
            MainRegion::IO_PIA => match addr {
                0xC804..=0xC807 => self.widget_pia.read(addr - 0xC804),
                0xC80C..=0xC80F => self.rom_pia.read(addr - 0xC80C),
                _ => 0xFF,
            },
            MainRegion::IO_BANK => self.rom_bank,
            MainRegion::IO_BLITTER => 0, // write-only on real hardware
            MainRegion::IO_VIDEO => self.current_scanline() & 0xFC,
            MainRegion::VIDEO_RAM
            | MainRegion::BANKED_ROM
            | MainRegion::CMOS
            | MainRegion::PROGRAM_ROM
            | MainRegion::SRAM => self.main_map.read_backing(addr),
            _ => 0xFF,
        };
        self.main_map
            .watch_read_with(0, master, addr, data, Self::main_device(addr));
        // Trace DMA reads and device reads only — CPU memory reads
        // (instruction fetches) would drown the ring.
        if self.debug_trace.enabled() {
            if matches!(master, BusMaster::Dma | BusMaster::DmaVram) {
                self.trace_main_access(master, DebugEventKind::DmaRead, addr, data, None);
            } else if matches!(
                self.main_map.page(addr).region_id,
                MainRegion::IO_PIA
                    | MainRegion::IO_BANK
                    | MainRegion::IO_BLITTER
                    | MainRegion::IO_VIDEO
            ) {
                self.trace_main_access(master, DebugEventKind::DeviceRead, addr, data, None);
            }
        }
        data
    }

    pub(crate) fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        if master == BusMaster::Cpu(1) {
            // Sound board — check the watchpoint before the side effect so
            // the hit records pre-write state (WatchpointPhase::Before).
            self.sound_map
                .watch_write_with(1, master, addr, data, Self::sound_device(addr));
            if self.debug_trace.enabled() {
                let kind = if self.sound_map.page(addr).region_id == SoundRegion::IO_PIA {
                    DebugEventKind::DeviceWrite
                } else {
                    DebugEventKind::MemoryWrite
                };
                self.trace_sound_access(master, kind, addr, data);
            }
            match self.sound_map.page(addr).region_id {
                SoundRegion::RAM => self.sound_map.write_backing(addr, data),
                SoundRegion::IO_PIA if (0x0400..=0x0403).contains(&addr) => {
                    self.sound_pia.write(addr - 0x0400, data);
                }
                _ => {} // ROM or unmapped: ignored
            }
            return;
        }

        // Main board — watchpoint check precedes the side effect (see above).
        self.main_map
            .watch_write_with(0, master, addr, data, Self::main_device(addr));
        if self.debug_trace.enabled() {
            let (kind, detail) = if matches!(master, BusMaster::Dma | BusMaster::DmaVram) {
                (DebugEventKind::DmaWrite, None)
            } else {
                match self.main_map.page(addr).region_id {
                    MainRegion::IO_BANK => (
                        DebugEventKind::BankSwitch,
                        Some(if data != 0 {
                            "banked ROM mapped at $0000-$8FFF"
                        } else {
                            "video RAM mapped at $0000-$8FFF"
                        }),
                    ),
                    MainRegion::IO_VIDEO if addr == 0xCBFF && data == 0x39 => {
                        (DebugEventKind::Watchdog, Some("watchdog cleared"))
                    }
                    MainRegion::IO_PIA | MainRegion::IO_BLITTER | MainRegion::IO_VIDEO => {
                        (DebugEventKind::DeviceWrite, None)
                    }
                    _ => (DebugEventKind::MemoryWrite, None),
                }
            };
            self.trace_main_access(master, kind, addr, data, detail);
        }
        match self.main_map.page(addr).region_id {
            // Writes always go to video RAM, even when banked ROM is overlaid
            MainRegion::VIDEO_RAM | MainRegion::BANKED_ROM => {
                self.main_map.region_data_mut(MainRegion::VideoRam)[addr as usize] = data;
            }
            MainRegion::PALETTE if addr <= 0xC00F => {
                self.main_map.region_data_mut(MainRegion::Palette)[(addr & 0x0F) as usize] = data;
            }
            MainRegion::IO_PIA => match addr {
                0xC804..=0xC807 => self.widget_pia.write(addr - 0xC804, data),
                0xC80C..=0xC80F => self.rom_pia.write(addr - 0xC80C, data),
                _ => {}
            },
            MainRegion::IO_BANK => {
                self.rom_bank = data;
                // Bank switching: remap pages 0x00-0x8F
                if data != 0 {
                    self.main_map
                        .remap_pages(0x00, 0x90, MainRegion::BankedRom, 0);
                } else {
                    self.main_map
                        .remap_pages(0x00, 0x90, MainRegion::VideoRam, 0);
                }
            }
            MainRegion::IO_BLITTER if (0xCA00..=0xCA07).contains(&addr) => {
                self.blitter.write_register(addr - 0xCA00, data);
            }
            MainRegion::IO_VIDEO if addr == 0xCBFF && data == 0x39 => {
                self.watchdog_counter = 0;
            }
            // Only lower 4 bits valid on Williams 5114/6514 SRAM
            MainRegion::CMOS => self.main_map.write_backing(addr, data | 0xF0),
            // Extra-RAM work RAM at 0xD000-0xDFFF (writable by CPU and blitter DMA)
            MainRegion::SRAM => self.main_map.write_backing(addr, data),
            _ => {} // ROM or unmapped: ignored
        }
    }

    pub(crate) fn bus_is_halted_for(&self, master: BusMaster) -> bool {
        match master {
            BusMaster::Cpu(0) => self.blitter.is_active(),
            _ => false,
        }
    }

    pub(crate) fn bus_check_interrupts(&mut self, target: BusMaster) -> InterruptState {
        match target {
            // Only ROM PIA interrupts are wired to the main CPU IRQ line
            // via INPUT_MERGER_ANY_HIGH. Widget PIA IRQs are not connected.
            // FIRQ is not used on Williams gen-1 hardware.
            BusMaster::Cpu(0) => InterruptState {
                nmi: false,
                irq: self.rom_pia.irq_a() || self.rom_pia.irq_b(),
                firq: false,
                ..Default::default()
            },
            BusMaster::Cpu(1) => InterruptState {
                nmi: false,
                irq: self.sound_pia.irq_a() || self.sound_pia.irq_b(),
                firq: false,
                ..Default::default()
            },
            _ => InterruptState::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::cpu::CpuStateTrait;

    #[test]
    fn board_save_load_round_trip() {
        let mut board = WilliamsBoard::new();

        // Set known state across various subsystems
        board.write_video_ram(0, 0xAA);
        board.write_video_ram(0x5FFF, 0xBB);
        board.main_map.region_data_mut(MainRegion::Palette)[3] = 0x42;
        board.sound_map.region_data_mut(SoundRegion::Ram)[0x10] = 0xCD;
        board.rom_bank = 5;
        board.clock = 123_456;
        board.watchdog_counter = 789;
        board.rom_pia_input = 0x10;
        // Run a few ticks to accumulate some resampler state
        for _ in 0..100 {
            board.dac.write(0xA0);
            board.resampler.tick(board.dac.sample_i16());
        }

        // Write CMOS data
        board.main_map.region_data_mut(MainRegion::Cmos)[0] = 0xF1;
        board.main_map.region_data_mut(MainRegion::Cmos)[100] = 0xF9;

        // Save
        let mut w = StateWriter::new();
        board.save_state(&mut w);
        let data = w.into_vec();

        // Mutate everything
        let mut board2 = WilliamsBoard::new();
        board2.write_video_ram(0, 0xFF);
        board2.write_video_ram(0x5FFF, 0xFF);
        board2.main_map.region_data_mut(MainRegion::Palette)[3] = 0x00;
        board2.rom_bank = 0;
        board2.clock = 0;
        board2.watchdog_counter = 0;

        // Load
        let mut r = StateReader::new(&data);
        board2.load_state(&mut r).unwrap();

        // Verify CPU state matches
        assert_eq!(
            board.cpu.snapshot(),
            board2.cpu.snapshot(),
            "main CPU state mismatch"
        );
        assert_eq!(
            board.sound_cpu.snapshot(),
            board2.sound_cpu.snapshot(),
            "sound CPU state mismatch"
        );

        // Verify RAM
        assert_eq!(board2.read_video_ram(0), 0xAA);
        assert_eq!(board2.read_video_ram(0x5FFF), 0xBB);
        assert_eq!(board2.main_map.region_data(MainRegion::Palette)[3], 0x42);
        assert_eq!(board2.sound_map.region_data(SoundRegion::Ram)[0x10], 0xCD);

        // Verify CMOS
        assert_eq!(board2.main_map.region_data(MainRegion::Cmos)[0], 0xF1);
        assert_eq!(board2.main_map.region_data(MainRegion::Cmos)[100], 0xF9);

        // Verify I/O & timing
        assert_eq!(board2.rom_bank, 5);
        assert_eq!(board2.clock, 123_456);
        assert_eq!(board2.watchdog_counter, 789);
        assert_eq!(board2.rom_pia_input, 0x10);
    }

    #[test]
    fn board_save_load_preserves_rom_unchanged() {
        let mut board = WilliamsBoard::new();
        board.main_map.region_data_mut(MainRegion::ProgramRom)[0] = 0xDE;
        board.main_map.region_data_mut(MainRegion::BankedRom)[0] = 0xAD;
        board.sound_map.region_data_mut(SoundRegion::Rom)[0] = 0xBE;

        let mut w = StateWriter::new();
        board.save_state(&mut w);
        let data = w.into_vec();

        // Load into a board with different ROM contents — ROM should NOT be overwritten
        let mut board2 = WilliamsBoard::new();
        board2.main_map.region_data_mut(MainRegion::ProgramRom)[0] = 0x11;
        board2.main_map.region_data_mut(MainRegion::BankedRom)[0] = 0x22;
        board2.sound_map.region_data_mut(SoundRegion::Rom)[0] = 0x33;

        let mut r = StateReader::new(&data);
        board2.load_state(&mut r).unwrap();

        assert_eq!(
            board2.main_map.region_data(MainRegion::ProgramRom)[0],
            0x11,
            "program ROM should be untouched"
        );
        assert_eq!(
            board2.main_map.region_data(MainRegion::BankedRom)[0],
            0x22,
            "banked ROM should be untouched"
        );
        assert_eq!(
            board2.sound_map.region_data(SoundRegion::Rom)[0],
            0x33,
            "sound ROM should be untouched"
        );
    }

    mod variant_config {
        use super::*;

        /// Sinistar-style variant: 4KB SRAM at 0xD000, 8KB program ROM at 0xE000.
        const SINISTAR: WilliamsConfig = WilliamsConfig {
            extra_sram_dxxx: true,
            sound_rom_20k: true,
            has_cvsd: true,
        };

        #[test]
        fn extra_ram_maps_sram_at_dxxx_and_rom_at_exxx() {
            let mut board = WilliamsBoard::with_config(SINISTAR);

            // 0xD000-0xDFFF is CPU-writable work RAM.
            board.bus_write(BusMaster::Cpu(0), 0xD000, 0x5A);
            board.bus_write(BusMaster::Cpu(0), 0xDFFF, 0xA5);
            assert_eq!(board.bus_read(BusMaster::Cpu(0), 0xD000), 0x5A);
            assert_eq!(board.bus_read(BusMaster::Cpu(0), 0xDFFF), 0xA5);

            // Program ROM now lives at 0xE000-0xFFFF and is read-only.
            board.load_program_rom(0x0000, &[0x7E]); // -> 0xE000
            board.load_program_rom(0x1000, &[0x3F]); // -> 0xF000
            assert_eq!(board.bus_read(BusMaster::Cpu(0), 0xE000), 0x7E);
            assert_eq!(board.bus_read(BusMaster::Cpu(0), 0xF000), 0x3F);
            board.bus_write(BusMaster::Cpu(0), 0xE000, 0x11); // ignored (ROM)
            assert_eq!(board.bus_read(BusMaster::Cpu(0), 0xE000), 0x7E);
        }

        #[test]
        fn blitter_dma_can_write_dxxx_sram() {
            // Window-enable never blocks blits to non-video RAM ($DXXX SRAM).
            let mut board = WilliamsBoard::with_config(SINISTAR);
            board.bus_write(BusMaster::Dma, 0xD500, 0x3C);
            assert_eq!(board.bus_read(BusMaster::Cpu(0), 0xD500), 0x3C);
            assert_eq!(board.main_map.region_data(MainRegion::Sram)[0x500], 0x3C);
        }

        #[test]
        fn standard_config_keeps_program_rom_at_dxxx() {
            let mut board = WilliamsBoard::new();
            board.load_program_rom(0x0000, &[0x99]); // -> 0xD000
            assert_eq!(board.bus_read(BusMaster::Cpu(0), 0xD000), 0x99);
            board.bus_write(BusMaster::Cpu(0), 0xD000, 0x22); // ignored (ROM)
            assert_eq!(board.bus_read(BusMaster::Cpu(0), 0xD000), 0x99);
        }

        #[test]
        fn sound_rom_20k_window_is_not_mirrored() {
            let mut board = WilliamsBoard::with_config(SINISTAR);
            board.load_sound_rom(0x0000, &[0xB0]); // -> 0xB000 (speech)
            board.load_sound_rom(0x4000, &[0xF0]); // -> 0xF000 (standard sound ROM)
            assert_eq!(board.bus_read(BusMaster::Cpu(1), 0xB000), 0xB0);
            assert_eq!(board.bus_read(BusMaster::Cpu(1), 0xF000), 0xF0);
        }

        #[test]
        fn standard_sound_rom_mirrors_f000_down_to_b000() {
            let mut board = WilliamsBoard::new();
            board.load_sound_rom(0x0000, &[0x77]); // -> 0xF000, mirrored
            assert_eq!(board.bus_read(BusMaster::Cpu(1), 0xF000), 0x77);
            assert_eq!(board.bus_read(BusMaster::Cpu(1), 0xB000), 0x77);
        }

        #[test]
        fn sinistar_sram_and_cvsd_round_trip_through_save_state() {
            let mut board = WilliamsBoard::with_config(SINISTAR);
            board.main_map.region_data_mut(MainRegion::Sram)[0x000] = 0xDE;
            board.main_map.region_data_mut(MainRegion::Sram)[0xFFF] = 0xAD;
            // Drive the CVSD to a non-trivial internal state.
            let cvsd = board.cvsd.as_mut().unwrap();
            cvsd.reset();
            for i in 0..24 {
                cvsd.digit_w(i % 3 == 0);
                cvsd.clock_w(true);
                cvsd.clock_w(false);
            }
            let cvsd_sample = board.cvsd.as_ref().unwrap().sample_i16();

            let mut w = StateWriter::new();
            board.save_state(&mut w);
            let data = w.into_vec();

            let mut board2 = WilliamsBoard::with_config(SINISTAR);
            let mut r = StateReader::new(&data);
            board2.load_state(&mut r).unwrap();

            assert_eq!(board2.main_map.region_data(MainRegion::Sram)[0x000], 0xDE);
            assert_eq!(board2.main_map.region_data(MainRegion::Sram)[0xFFF], 0xAD);
            assert_eq!(board2.cvsd.as_ref().unwrap().sample_i16(), cvsd_sample);
        }

        #[test]
        fn standard_save_appends_no_variant_bytes() {
            // Regression guard: the standard gen-1 save layout is unchanged — the
            // only difference for an extra-RAM board is exactly the 4KB SRAM
            // appended at the end, so pre-variant Joust/Robotron saves stay valid.
            let save_len = |cfg| {
                let board = WilliamsBoard::with_config(cfg);
                let mut w = StateWriter::new();
                board.save_state(&mut w);
                w.into_vec().len()
            };
            let standard = save_len(WilliamsConfig::gen1_standard());
            let sinistar = save_len(SINISTAR);
            // Serialized size of one 4KB region write (data + framing), so the
            // check is robust to the writer's length-prefix format.
            let sram_block = {
                let mut w = StateWriter::new();
                w.write_bytes(&[0u8; 0x1000]);
                w.into_vec().len()
            };
            // Sinistar also appends CVSD device state.
            let cvsd_block = {
                let mut w = StateWriter::new();
                Hc55516::new().save_state(&mut w);
                w.into_vec().len()
            };
            assert_eq!(
                sinistar - standard,
                sram_block + cvsd_block,
                "only the appended SRAM + CVSD blocks should differ"
            );
        }

        #[test]
        fn cvsd_present_only_for_configured_boards() {
            assert!(WilliamsBoard::new().cvsd.is_none());
            assert!(WilliamsBoard::with_config(SINISTAR).cvsd.is_some());
        }
    }

    mod watchpoint_metadata {
        use super::*;
        use phosphor_core::core::watchpoint::{DebugAccessSource, WatchpointKind, WatchpointPhase};

        #[test]
        fn write_hit_carries_latched_context_and_region() {
            let mut board = WilliamsBoard::new();
            board.clock = 4242;
            board
                .main_map
                .set_watchpoint(0, 0x1234, WatchpointKind::Write);
            board
                .main_map
                .latch_access_context(board.clock, Some(0xD0_42));

            board.bus_write(BusMaster::Cpu(0), 0x1234, 0x56);

            let hit = board.main_map.take_hit().unwrap();
            assert_eq!(hit.cpu_index, 0);
            assert_eq!(hit.source, DebugAccessSource::Cpu(0));
            assert_eq!(hit.cycle, 4242);
            assert_eq!(hit.pc, Some(0xD0_42));
            assert_eq!(hit.phase, WatchpointPhase::Before);
            assert_eq!(hit.value, 0x56);
            assert_eq!(hit.region, Some("Video RAM"));
            assert_eq!(hit.device, None);
        }

        #[test]
        fn io_hits_attribute_owning_device() {
            let mut board = WilliamsBoard::new();
            board
                .main_map
                .set_watchpoint(0, 0xC804, WatchpointKind::Write);
            board
                .main_map
                .set_watchpoint(0, 0xC80C, WatchpointKind::Read);
            board
                .sound_map
                .set_watchpoint(1, 0x0400, WatchpointKind::Write);

            board.bus_write(BusMaster::Cpu(0), 0xC804, 0x01);
            board.bus_read(BusMaster::Cpu(0), 0xC80C);
            board.bus_write(BusMaster::Cpu(1), 0x0400, 0x02);

            assert_eq!(
                board.main_map.take_hit().unwrap().device,
                Some("Widget PIA")
            );
            assert_eq!(board.main_map.take_hit().unwrap().device, Some("ROM PIA"));
            assert_eq!(
                board.sound_map.take_hit().unwrap().device,
                Some("Sound PIA")
            );
        }

        #[test]
        fn blitter_vram_bypass_read_fires_watchpoint() {
            let mut board = WilliamsBoard::new();
            board
                .main_map
                .set_watchpoint(0, 0x2000, WatchpointKind::Read);
            board.write_video_ram(0x2000, 0x99);

            // DmaVram reads bypass page-table dispatch but must still hit.
            let data = board.bus_read(BusMaster::DmaVram, 0x2000);
            assert_eq!(data, 0x99);

            let hit = board.main_map.take_hit().unwrap();
            assert_eq!(hit.source, DebugAccessSource::Dma);
            assert_eq!(hit.pc, None, "DMA access carries no PC");
            assert_eq!(hit.value, 0x99);
        }

        #[test]
        fn write_hit_recorded_before_side_effect_applies() {
            // The hit's region lookup must reflect pre-write decode: a bank
            // switch write that remaps pages still reports the I/O region.
            let mut board = WilliamsBoard::new();
            board
                .main_map
                .set_watchpoint(0, 0xC900, WatchpointKind::Write);

            board.bus_write(BusMaster::Cpu(0), 0xC900, 0x01);

            let hit = board.main_map.take_hit().unwrap();
            assert_eq!(hit.phase, WatchpointPhase::Before);
            assert_eq!(hit.region, Some("ROM Bank"));
            // The side effect still happened after the hit was queued.
            assert_eq!(board.rom_bank, 0x01);
        }
    }

    mod debug_events {
        use super::*;
        use phosphor_core::core::debug_trace::{DebugEventKind, DebugTrace};
        use phosphor_core::core::watchpoint::DebugAccessSource;

        #[test]
        fn tracing_disabled_records_nothing() {
            let mut board = WilliamsBoard::new();
            board.bus_write(BusMaster::Cpu(0), 0xC900, 0x01);
            board.bus_read(BusMaster::Cpu(0), 0xC80C);
            assert!(board.debug_trace.is_empty());
        }

        #[test]
        fn bank_switch_write_emits_bank_switch_event() {
            let mut board = WilliamsBoard::new();
            board.debug_trace.set_enabled(true);
            board.clock = 99;
            board
                .main_map
                .latch_access_context(board.clock, Some(0xD042));

            board.bus_write(BusMaster::Cpu(0), 0xC900, 0x03);

            let events = board.debug_trace.events();
            assert_eq!(events.len(), 1);
            let e = &events[0];
            assert_eq!(e.kind, DebugEventKind::BankSwitch);
            assert_eq!(e.cycle, 99);
            assert_eq!(e.pc, Some(0xD042));
            assert_eq!(e.source, DebugAccessSource::Cpu(0));
            assert_eq!(e.addr, Some(0xC900));
            assert_eq!(e.value, Some(0x03));
            assert_eq!(e.region, Some("ROM Bank"));
            assert_eq!(e.detail, Some("banked ROM mapped at $0000-$8FFF"));
        }

        #[test]
        fn watchdog_clear_emits_watchdog_event() {
            let mut board = WilliamsBoard::new();
            board.debug_trace.set_enabled(true);
            board.bus_write(BusMaster::Cpu(0), 0xCBFF, 0x39);

            let e = board.debug_trace.events()[0];
            assert_eq!(e.kind, DebugEventKind::Watchdog);
            assert_eq!(e.detail, Some("watchdog cleared"));
        }

        #[test]
        fn pia_write_emits_device_write_with_device_name() {
            let mut board = WilliamsBoard::new();
            board.debug_trace.set_enabled(true);
            board.bus_write(BusMaster::Cpu(0), 0xC804, 0x55);
            board.bus_write(BusMaster::Cpu(1), 0x0400, 0xAA);

            let events = board.debug_trace.events();
            assert_eq!(events[0].kind, DebugEventKind::DeviceWrite);
            assert_eq!(events[0].device, Some("Widget PIA"));
            assert_eq!(events[0].cpu_index, Some(0));
            assert_eq!(events[1].kind, DebugEventKind::DeviceWrite);
            assert_eq!(events[1].device, Some("Sound PIA"));
            assert_eq!(events[1].cpu_index, Some(1));
        }

        #[test]
        fn blitter_dma_accesses_emit_dma_events() {
            let mut board = WilliamsBoard::new();
            board.debug_trace.set_enabled(true);
            board.write_video_ram(0x2000, 0x77);

            board.bus_read(BusMaster::DmaVram, 0x2000); // keepmask dest read
            board.bus_write(BusMaster::Dma, 0x2001, 0x42); // blit write

            let events = board.debug_trace.events();
            assert_eq!(events[0].kind, DebugEventKind::DmaRead);
            assert_eq!(events[0].value, Some(0x77));
            assert_eq!(events[0].pc, None, "DMA events carry no PC");
            assert_eq!(events[1].kind, DebugEventKind::DmaWrite);
            assert_eq!(events[1].value, Some(0x42));
        }

        #[test]
        fn device_reads_traced_memory_reads_not() {
            let mut board = WilliamsBoard::new();
            board.debug_trace.set_enabled(true);

            board.bus_read(BusMaster::Cpu(0), 0x1234); // video RAM: not traced
            board.bus_read(BusMaster::Cpu(0), 0xC80C); // ROM PIA: traced

            let events = board.debug_trace.events();
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].kind, DebugEventKind::DeviceRead);
            assert_eq!(events[0].device, Some("ROM PIA"));
        }

        #[test]
        fn save_state_excludes_debug_events() {
            let mut board = WilliamsBoard::new();
            board.debug_trace.set_enabled(true);
            board.bus_write(BusMaster::Cpu(0), 0xC900, 0x01);
            assert!(!board.debug_trace.is_empty());

            let mut w = StateWriter::new();
            board.save_state(&mut w);
            let data = w.into_vec();

            // Loading must not restore or disturb observer state.
            let mut board2 = WilliamsBoard::new();
            let mut r = StateReader::new(&data);
            board2.load_state(&mut r).unwrap();
            assert!(board2.debug_trace.is_empty());
            assert!(!board2.debug_trace.enabled());
        }

        #[test]
        fn derive_provides_debug_trace_capability() {
            let mut board = WilliamsBoard::new();
            let trace: &mut dyn DebugTrace = &mut board;

            assert!(!trace.trace_enabled());
            trace.set_trace_enabled(true);
            assert!(trace.trace_enabled());

            board.bus_write(BusMaster::Cpu(0), 0xC900, 0x01);
            let trace: &mut dyn DebugTrace = &mut board;
            assert_eq!(trace.trace_events().len(), 1);
            trace.clear_trace_events();
            assert!(trace.trace_events().is_empty());
        }
    }

    mod debug_peek {
        use super::*;
        use phosphor_core::core::DebugRead;
        use phosphor_core::core::debug::BusDebug;

        #[test]
        fn derived_peek_reports_backed_io_unmapped_per_map() {
            let mut board = WilliamsBoard::new();
            board.write_video_ram(0x2000, 0x5A);

            let bus: &dyn BusDebug = &board;

            // Main CPU space: backed video RAM, I/O at the PIAs
            assert_eq!(
                bus.peek(0, 0x2000),
                DebugRead::Backed {
                    value: 0x5A,
                    width: 1,
                    region_id: MainRegion::VIDEO_RAM
                }
            );
            assert_eq!(bus.peek(0, 0xC804), DebugRead::Io);

            // Sound CPU space: I/O at the PIA, unmapped between regions
            assert_eq!(bus.peek(1, 0x0400), DebugRead::Io);
            assert_eq!(bus.peek(1, 0x0500), DebugRead::Unmapped);

            // Unknown CPU index and >16-bit addresses are unmapped
            assert_eq!(bus.peek(2, 0x0000), DebugRead::Unmapped);
            assert_eq!(bus.peek(0, 0x1_0000), DebugRead::Unmapped);
        }
    }

    mod device_controls {
        use super::*;
        use phosphor_core::core::debug::BusDebug;

        /// devices() index of the Widget PIA (after the two CPU entries).
        const WIDGET_PIA: usize = 2;

        #[test]
        fn devices_order_matches_dispatch_indices() {
            let board = WilliamsBoard::new();
            let names: Vec<&str> = board.devices().iter().map(|(name, _)| *name).collect();
            assert_eq!(
                names,
                vec![
                    "M6809 Main",
                    "M6800 Sound",
                    "Widget PIA",
                    "ROM PIA",
                    "Blitter",
                    "Sound PIA",
                    "DAC",
                ]
            );
        }

        /// Program a PIA's port A as all-output and write a value to it,
        /// going through the derived `write_device_register` dispatch.
        fn write_pia_port_a(board: &mut WilliamsBoard, device_index: usize, value: u8) {
            board.write_device_register(device_index, 0, 0xFF); // DDRA: all output
            board.write_device_register(device_index, 1, 0x04); // CRA: select ORA
            board.write_device_register(device_index, 0, value); // ORA
        }

        #[test]
        fn write_device_register_reaches_the_device() {
            let mut board = WilliamsBoard::new();
            write_pia_port_a(&mut board, WIDGET_PIA, 0x5A);
            assert_eq!(board.widget_pia.read_output_a(), 0x5A);
        }

        #[test]
        fn reset_device_resets_only_that_device() {
            let mut board = WilliamsBoard::new();
            write_pia_port_a(&mut board, WIDGET_PIA, 0x5A);
            write_pia_port_a(&mut board, WIDGET_PIA + 1, 0xA5); // ROM PIA

            board.reset_device(WIDGET_PIA);

            assert_eq!(board.widget_pia.read_output_a(), 0x00);
            assert_eq!(
                board.rom_pia.read_output_a(),
                0xA5,
                "other devices untouched"
            );
        }

        #[test]
        fn cpu_and_out_of_range_indices_are_ignored() {
            let mut board = WilliamsBoard::new();
            // Indices 0/1 are CPUs; 99 is out of range. All must no-op.
            board.write_device_register(0, 0, 0xFF);
            board.write_device_register(1, 0, 0xFF);
            board.write_device_register(99, 0, 0xFF);
            board.reset_device(0);
            board.reset_device(99);
        }
    }
}
