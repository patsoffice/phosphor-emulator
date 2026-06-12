use phosphor_core::audio::AudioResampler;
use phosphor_core::core::bus::InterruptState;
use phosphor_core::core::memory_map::{AccessKind, MemoryMap};
use phosphor_core::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use phosphor_core::core::{Bus, BusMaster, TimingConfig};
use phosphor_core::cpu::CpuStateTrait;
use phosphor_core::cpu::m6800::M6800;
use phosphor_core::cpu::m6809::M6809;
use phosphor_core::cpu::state::{M6800State, M6809State};
use phosphor_core::device::dac::Mc1408Dac;
use phosphor_core::device::pia6820::Pia6820;
use phosphor_core::device::williams_blitter::WilliamsBlitter;
use phosphor_macros::{BusDebug, MemoryRegion};

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
    ProgramRom = 8, // 0xD000-0xFFFF (12KB program ROM)
    BankedRom = 9,  // (36KB, overlays VIDEO_RAM when bank != 0)
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
#[derive(BusDebug)]
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
    pub(crate) resampler: AudioResampler<i16>,

    // Memory maps (page-table dispatch + watchpoints + backing memory)
    // All RAM/ROM storage lives in the MemoryMap backing store.
    #[debug_map(cpu = 0)]
    pub(crate) main_map: MemoryMap,
    #[debug_map(cpu = 1)]
    pub(crate) sound_map: MemoryMap,

    // System state
    pub watchdog_counter: u32,
    pub(crate) clock: u64,

    // ROM PIA Port A input (game sets coin/service bits)
    pub(crate) rom_pia_input: u8,

    // Scanline-rendered framebuffer (292 × 240 × RGB24)
    pub(crate) scanline_buffer: Vec<u8>,
}

impl WilliamsBoard {
    pub fn new() -> Self {
        Self {
            cpu: M6809::new(),
            sound_cpu: M6800::new(),
            widget_pia: Pia6820::new(),
            rom_pia: Pia6820::new(),
            blitter: WilliamsBlitter::new(),
            rom_bank: 0,
            sound_pia: Pia6820::new(),
            dac: Mc1408Dac::new(),
            resampler: AudioResampler::new(1_000_000, 44_100),
            main_map: Self::build_main_map(),
            sound_map: Self::build_sound_map(),
            watchdog_counter: 0,
            clock: 0,
            rom_pia_input: 0,
            scanline_buffer: vec![
                0u8;
                TIMING.display_width as usize * TIMING.display_height as usize * 3
            ],
        }
    }

    fn build_main_map() -> MemoryMap {
        use MainRegion::*;
        let mut map = MemoryMap::new();
        map.region(VideoRam, "Video RAM", 0x0000, 0xC000, AccessKind::ReadWrite)
            .region(Palette, "Palette", 0xC000, 0x100, AccessKind::ReadWrite)
            .region(IoPia, "PIAs", 0xC800, 0x100, AccessKind::Io)
            .region(IoBank, "ROM Bank", 0xC900, 0x100, AccessKind::Io)
            .region(IoBlitter, "Blitter", 0xCA00, 0x100, AccessKind::Io)
            .region(IoVideo, "Video Counter", 0xCB00, 0x100, AccessKind::Io)
            .region(Cmos, "CMOS RAM", 0xCC00, 0x400, AccessKind::ReadWrite)
            .region(
                ProgramRom,
                "Program ROM",
                0xD000,
                0x3000,
                AccessKind::ReadOnly,
            )
            .backing_region(BankedRom, "Banked ROM", 0x9000);
        map
    }

    fn build_sound_map() -> MemoryMap {
        use SoundRegion::*;
        let mut map = MemoryMap::new();
        map.region(Ram, "Sound RAM", 0x0000, 0x100, AccessKind::ReadWrite)
            .region(IoPia, "Sound PIA", 0x0400, 0x100, AccessKind::Io)
            .region(Rom, "Sound ROM", 0xF000, 0x1000, AccessKind::ReadOnly)
            .mirror(0xB000, 0xF000, 0x1000)
            .mirror(0xC000, 0xF000, 0x1000)
            .mirror(0xD000, 0xF000, 0x1000)
            .mirror(0xE000, 0xF000, 0x1000);
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
    /// Offset is relative to the start of the ROM region (0 = address 0xD000).
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
    /// Offset is relative to the start of the sound ROM region (0 = address 0xF000).
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
        }

        // Latch watchpoint attribution context (cycle + instruction PC)
        // before CPU execution — bus dispatch cannot read CPU state mid-tick.
        if self.main_map.has_any_watchpoints() {
            let pc = self
                .cpu
                .at_instruction_boundary()
                .then_some(self.cpu.pc as u32);
            self.main_map.latch_access_context(self.clock, pc);
        }
        if self.sound_map.has_any_watchpoints() {
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

        // Bresenham downsample: 1 MHz CPU clock -> 44.1 kHz output
        self.resampler.tick(self.dac.sample_i16());

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

    // --- Machine trait helpers (called by game wrappers) ---

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
            return data;
        }

        // DmaVram reads bypass ROM banking — the blitter reads dest
        // directly from VRAM for keepmask blending.
        if master == BusMaster::DmaVram && addr <= 0x8FFF {
            let data = self.main_map.region_data(MainRegion::VideoRam)[addr as usize];
            self.main_map.watch_read(0, master, addr, data);
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
            | MainRegion::PROGRAM_ROM => self.main_map.read_backing(addr),
            _ => 0xFF,
        };
        self.main_map
            .watch_read_with(0, master, addr, data, Self::main_device(addr));
        data
    }

    pub(crate) fn bus_write(&mut self, master: BusMaster, addr: u16, data: u8) {
        if master == BusMaster::Cpu(1) {
            // Sound board — check the watchpoint before the side effect so
            // the hit records pre-write state (WatchpointPhase::Before).
            self.sound_map
                .watch_write_with(1, master, addr, data, Self::sound_device(addr));
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
}
