use crate::core::{Bus, BusMaster};
use phosphor_macros::Saveable;

/// Williams Special Chip (SC1/SC2) — Blitter / DMA engine
///
/// Hardware block copy/fill engine used in Williams 2nd-generation arcade boards
/// (Joust, Robotron 2084, Bubbles, Sinistar, etc.). Operates via DMA, halting
/// the CPU during transfers. Reads and writes go through the system bus, so the
/// blitter sees the same address decoding as the CPU (including ROM banking).
///
/// Two variants exist:
/// - **SC1** (VL2001): Has the "XOR 4" bug on width/height registers.
/// - **SC2** (VL2001A): Fixes the XOR 4 bug.
///
/// References:
/// - MAME: `mamedev/mame` `src/mame/midway/williamsblitter.cpp` / `.h`
/// - Sean Riddle: <https://seanriddle.com/blitter.html>
///
/// # Register map (8 write-only registers at $CA00-$CA07)
///
/// Writing to offset 0 (control byte) triggers the blit operation. All other
/// registers should be written first to configure the transfer parameters.
///
/// | Offset | Name        | Description                                           |
/// |--------|-------------|-------------------------------------------------------|
/// | 0      | control     | Control byte — **writing triggers the blit**          |
/// | 1      | solid_color | Source byte for solid fill mode                       |
/// | 2      | src_hi      | Source address high byte                              |
/// | 3      | src_lo      | Source address low byte                               |
/// | 4      | dst_hi      | Destination address high byte                         |
/// | 5      | dst_lo      | Destination address low byte                          |
/// | 6      | width       | Blit width (XOR with size_xor on SC1)                 |
/// | 7      | height      | Blit height (XOR with size_xor on SC1)                |
///
/// Registers are write-only on real hardware. Reading $CA00 does NOT initiate
/// blitting. Registers retain values across blits.
///
/// Reference (Sean Riddle): "Omitting register writes reuses previous values."
///
/// # Control byte bit assignments (from MAME header)
///
/// | Bit | Flag            | Description                                       |
/// |-----|-----------------|---------------------------------------------------|
/// | 0   | SRC_STRIDE_256  | Source uses stride-256 (column-major)              |
/// | 1   | DST_STRIDE_256  | Destination uses stride-256 (column-major)        |
/// | 2   | SLOW            | 0 = fast (1 µs/byte), 1 = slow (2 µs/byte)       |
/// | 3   | FOREGROUND_ONLY | Per-nibble transparency: skip color-0 pixels      |
/// | 4   | SOLID           | Use `solid_color` instead of source data           |
/// | 5   | SHIFT           | Right-shift source pixels by one position (4 bits) |
/// | 6   | NO_ODD          | Suppress lower nibble (D3-D0) writes               |
/// | 7   | NO_EVEN         | Suppress upper nibble (D7-D4) writes               |
///
/// Reference (Sean Riddle) on slow/fast: "Blits from RAM to RAM have to run
/// at half speed, 2 microseconds per byte."
///
/// # Stride
///
/// Stride is controlled per-transfer by bits 0 and 1 of the control byte.
/// When stride-256 is set, columns advance by 256 (moving right across
/// screen) and rows advance by 1 within the 256-byte page (moving down).
/// When stride-1, columns advance by 1 and rows advance by the blit width.
///
/// Reference (Sean Riddle): "successive bytes are displayed below one another;
/// the next pair of pixels to the right of the previous are 256 bytes away."
///
/// MAME row advance for stride-256 wraps within the page:
/// `start = (start & 0xFF00) | ((start + 1) & 0x00FF)`
///
/// # SC1 XOR 4 bug
///
/// Reference (Sean Riddle): "bit 2 of the width and height to be inverted
/// (XOR 4)." Game ROMs compensate for this bug by pre-XORing their values.
/// SC2 fixes this (size_xor = 0).
///
/// After XOR, width/height of 0 is clamped to 1.
/// Reference (Sean Riddle): "Using a width or height of 0 gives the same
/// results as 1."
/// MAME: `if (w == 0) w = 1;`
///
/// # DMA integration
///
/// After triggering, `is_active()` returns `true`. The system should halt the
/// CPU while the blitter is active and call `do_dma_cycle()` once per CPU
/// cycle. Each call consumes **one clock**: a fast blit moves a byte on every
/// one, a slow blit moves a byte and then spends a clock moving nothing, so it
/// costs two clocks a byte. When the blit completes, `is_active()` returns
/// `false`, and the caller's halt line drops with it.
///
/// Reference (Sean Riddle): "CPU completes current instruction, sets BA/BS
/// high. /BABS signal goes low; blitter gains bus control. [...] Upon
/// completion, /HALT deasserts; CPU resumes."
#[derive(Saveable)]
// Bumped to 2 by the `stall` field: a blit saved mid-transfer in slow mode
// reloads owing the same clock it owed, rather than losing or gaining one.
#[save_version(2)]
pub struct WilliamsBlitter {
    // Registers (offsets 0-7, write-only)
    control: u8,
    solid_color: u8,
    src_addr: u16,
    dst_addr: u16,
    width: u8,
    height: u8,

    // Configuration (set at construction, not via registers)
    size_xor: u8, // 4 for SC1 (VL2001), 0 for SC2 (VL2001A)

    // Window clip (Sinistar). When `window_enable` is set, blits into
    // [clip_address, 0xC000) are suppressed; writes below the clip or to
    // non-video RAM ($C000+, e.g. $DXXX SRAM) always proceed. `clip_address` is
    // construction-time config and `window_enable` is driven at runtime by the
    // $C900 register — both are save-skipped so other games' blitter saves stay
    // byte-identical; the board restores `window_enable` (guarded by config).
    #[save_skip]
    clip_address: u16,
    #[save_skip]
    window_enable: bool,

    // Execution state
    active: bool,
    /// Set after a byte moves in `CTRL_SLOW` mode: the next clock is the second
    /// of that byte's two, and moves nothing. The blit stays `active` across it,
    /// which is what actually holds the CPU halted for the extra microsecond.
    stall: bool,
    x: u16,         // current column within row (0..w)
    w: u16,         // effective width (1-based, post-XOR + clamp)
    h: u16,         // effective height (1-based, post-XOR + clamp)
    rows_done: u16, // number of rows completed
    sstart: u16,    // source row-start address
    dstart: u16,    // dest row-start address
    cur_src: u16,   // current source address
    cur_dst: u16,   // current dest address
    sxadv: u16,     // source per-column advance (1 or 256)
    dxadv: u16,     // dest per-column advance (1 or 256)
    shift_reg: u8,  // previous raw source byte for shift mode
}

use crate::core::debug::{DebugRegister, Debuggable};

impl Debuggable for WilliamsBlitter {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "CTRL",
                value: self.control as u64,
                width: 8,
            },
            DebugRegister {
                name: "SOLID",
                value: self.solid_color as u64,
                width: 8,
            },
            DebugRegister {
                name: "SRC",
                value: self.src_addr as u64,
                width: 16,
            },
            DebugRegister {
                name: "DST",
                value: self.dst_addr as u64,
                width: 16,
            },
            DebugRegister {
                name: "W",
                value: self.width as u64,
                width: 8,
            },
            DebugRegister {
                name: "H",
                value: self.height as u64,
                width: 8,
            },
            DebugRegister {
                name: "ACT",
                value: self.active as u64,
                width: 8,
            },
        ]
    }
}

// Control byte bit positions (from MAME williamsblitter.h)
const CTRL_SRC_STRIDE_256: u8 = 0x01; // Bit 0
const CTRL_DST_STRIDE_256: u8 = 0x02; // Bit 1
const CTRL_SLOW: u8 = 0x04; // Bit 2
const CTRL_FOREGROUND_ONLY: u8 = 0x08; // Bit 3
const CTRL_SOLID: u8 = 0x10; // Bit 4
const CTRL_SHIFT: u8 = 0x20; // Bit 5
const CTRL_NO_ODD: u8 = 0x40; // Bit 6
const CTRL_NO_EVEN: u8 = 0x80; // Bit 7

impl WilliamsBlitter {
    /// Create a new SC1 (VL2001) blitter with the XOR 4 size bug.
    pub fn sc1() -> Self {
        Self::with_size_xor(4)
    }

    /// Create a new SC2 (VL2001A) blitter without the XOR 4 size bug.
    pub fn sc2() -> Self {
        Self::with_size_xor(0)
    }

    /// Backwards-compatible alias for `sc1()`.
    pub fn new() -> Self {
        Self::sc1()
    }

    /// Create an SC1 blitter with a window-clip address (Sinistar). When the
    /// window is enabled (via [`set_window_enable`](Self::set_window_enable)),
    /// blits into `[clip_address, 0xC000)` are suppressed.
    pub fn sc1_with_clip(clip_address: u16) -> Self {
        let mut b = Self::with_size_xor(4);
        b.clip_address = clip_address;
        b
    }

    /// Enable or disable the blit window clip (Sinistar $C900 bit 2).
    pub fn set_window_enable(&mut self, enable: bool) {
        self.window_enable = enable;
    }

    /// Current window-clip enable state (for save-state / debug).
    pub fn window_enable(&self) -> bool {
        self.window_enable
    }

    fn with_size_xor(size_xor: u8) -> Self {
        Self {
            control: 0,
            solid_color: 0,
            src_addr: 0,
            dst_addr: 0,
            width: 0,
            height: 0,
            size_xor,
            clip_address: 0,
            window_enable: false,
            active: false,
            stall: false,
            x: 0,
            w: 0,
            h: 0,
            rows_done: 0,
            sstart: 0,
            dstart: 0,
            cur_src: 0,
            cur_dst: 0,
            sxadv: 0,
            dxadv: 0,
            shift_reg: 0,
        }
    }

    /// Write to a blitter register (offset 0-7).
    ///
    /// Writing to offset 0 (control byte) triggers the blit operation. All
    /// other registers (1-7) should be configured before writing offset 0.
    ///
    /// Reference (Sean Riddle): "$CA00 Control Byte: Initiates blit; encodes
    /// operation flavor."
    pub fn write_register(&mut self, offset: u16, data: u8) {
        match offset {
            0 => {
                self.control = data;
                self.start_blit();
            }
            1 => self.solid_color = data,
            2 => self.src_addr = (self.src_addr & 0x00FF) | ((data as u16) << 8),
            3 => self.src_addr = (self.src_addr & 0xFF00) | (data as u16),
            4 => self.dst_addr = (self.dst_addr & 0x00FF) | ((data as u16) << 8),
            5 => self.dst_addr = (self.dst_addr & 0xFF00) | (data as u16),
            6 => self.width = data,
            7 => self.height = data,
            _ => {}
        }
    }

    /// Returns true if the blitter is currently executing a DMA transfer.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Execute one clock of the blit, and return the clocks it consumed: 1
    /// while active, 0 when idle.
    ///
    /// **One clock, not one byte.** A fast blit moves a byte on every clock; a
    /// slow one moves a byte and then spends a clock moving nothing, so it
    /// costs two clocks a byte. The caller steps this device once per CPU cycle
    /// and holds the CPU halted while `is_active()`, so the stall clock is what
    /// charges the second microsecond. Charging it here rather than at the
    /// caller is what keeps all blit timing in this file: the board previously
    /// discarded the returned cost, and `CTRL_SLOW` cost nothing at all.
    ///
    /// Which of a slow byte's two clocks moves it is not determined by anything
    /// available: the CPU is halted throughout and cannot observe the
    /// difference. The byte moves on the first, so it is visible to the
    /// renderer as early as the hardware could manage.
    ///
    /// Reference (Sean Riddle): "Blits from RAM to RAM have to run at half
    /// speed, 2 microseconds per byte."
    ///
    /// The blitter shares the CPU's address bus, so reads go through the same
    /// address decoding (including ROM banking overlays).
    ///
    /// Reference (MAME): The inner loop reads source, optionally shifts,
    /// calls `blit_pixel()` which reads dest, computes keepmask for per-nibble
    /// transparency and pixel suppression, then writes the result.
    pub fn do_dma_cycle(&mut self, bus: &mut dyn Bus<Address = u16, Data = u8>) -> u8 {
        if !self.active {
            return 0;
        }

        // The second clock of a slow byte. Nothing moves; the blit only ends
        // here if the byte before it was the last one.
        if self.stall {
            self.stall = false;
            if self.rows_done >= self.h {
                self.active = false;
            }
            return 1;
        }

        // Timing: MAME bit 2 (SLOW). 0 = fast (one clock a byte), 1 = slow (two).
        let slow = (self.control & CTRL_SLOW) != 0;

        // Step 1: Read source byte.
        // MAME: `const u8 rawval = m_remap[space.read_byte(source)];`
        // We skip the PROM remap (board-level concern, not blitter logic).
        let raw_src = bus.read(BusMaster::Dma, self.cur_src);

        // Step 2: Apply shift if enabled — right-shift by one pixel (4 bits).
        // MAME: `pixdata = (pixdata << 8) | rawval; blit_pixel(dest, (pixdata >> 4) & 0xff);`
        // Reference (Sean Riddle): "Shift the source data right one pixel when
        // writing it."
        let src_byte = if (self.control & CTRL_SHIFT) != 0 {
            let combined = ((self.shift_reg as u16) << 8) | (raw_src as u16);
            self.shift_reg = raw_src;
            ((combined >> 4) & 0xFF) as u8
        } else {
            raw_src
        };

        // Step 3: Pixel write with keepmask (matches MAME blit_pixel).
        //
        // keepmask starts at 0xFF (keep all dest bits). For each nibble,
        // the keepmask is cleared (allowing writes) based on the interaction
        // of FOREGROUND_ONLY and NO_EVEN/NO_ODD flags.
        //
        // MAME truth table (per nibble, shown for even/upper):
        //   fg_only=T, src=0, no_even=T → WRITE (clear dest nibble)
        //   fg_only=T, src=0, no_even=F → KEEP  (transparent)
        //   fg_only=T, src≠0, no_even=T → KEEP  (suppressed)
        //   fg_only=T, src≠0, no_even=F → WRITE (normal)
        //   fg_only=F, -----, no_even=T → KEEP  (suppressed)
        //   fg_only=F, -----, no_even=F → WRITE (normal)
        //
        // When fg_only + no_even/no_odd are both set, the transparency
        // behavior inverts: zero pixels ARE written, non-zero are suppressed.
        //
        // Reference (Sean Riddle): "Color 0 is not copied to the destination,
        // allowing for transparency."
        // Reference (MAME): `blit_pixel()` in `williamsblitter.cpp`
        let fg_only = (self.control & CTRL_FOREGROUND_ONLY) != 0;
        let no_even = (self.control & CTRL_NO_EVEN) != 0;
        let no_odd = (self.control & CTRL_NO_ODD) != 0;

        // Dest read bypasses ROM banking — on real hardware the blitter has direct
        // VRAM access for read-modify-write. MAME: `int curpix = m_vram[dstaddr];`
        let dst_byte = bus.read(BusMaster::DmaVram, self.cur_dst);
        let mut keepmask: u8 = 0xFF;

        // Even pixel (upper nibble D7-D4)
        if fg_only && (src_byte & 0xF0) == 0 {
            // Source is transparent (color 0). Normally keep dest, but
            // NO_EVEN inverts this — write zeros to clear the nibble.
            if no_even {
                keepmask &= 0x0F;
            }
        } else if !no_even {
            keepmask &= 0x0F;
        }

        // Odd pixel (lower nibble D3-D0)
        if fg_only && (src_byte & 0x0F) == 0 {
            // Source is transparent. NO_ODD inverts — write zeros.
            if no_odd {
                keepmask &= 0xF0;
            }
        } else if !no_odd {
            keepmask &= 0xF0;
        }

        // Apply solid color substitution at write time (MAME does this in
        // blit_pixel, not at source read).
        let effective_src = if (self.control & CTRL_SOLID) != 0 {
            self.solid_color
        } else {
            src_byte
        };

        let result = (dst_byte & keepmask) | (effective_src & !keepmask);
        // Window clip (Sinistar): when enabled, suppress the final write into
        // [clip_address, 0xC000). Writes below the clip or to non-video RAM
        // ($C000+, e.g. the $DXXX SRAM) always proceed. The address advance
        // below still happens either way (the pixel is consumed).
        if !self.window_enable || self.cur_dst < self.clip_address || self.cur_dst >= 0xC000 {
            bus.write(BusMaster::Dma, self.cur_dst, result);
        }

        // Step 4: Advance addresses.
        // MAME: `source += sxadv; dest += dxadv;` — always, regardless of
        // solid mode.
        self.cur_src = self.cur_src.wrapping_add(self.sxadv);
        self.cur_dst = self.cur_dst.wrapping_add(self.dxadv);

        // Step 5: Advance column/row counters.
        self.x += 1;
        if self.x >= self.w {
            // End of row — advance to next row.
            self.x = 0;
            self.rows_done += 1;

            if self.rows_done >= self.h {
                // The last byte has moved. A slow blit still owes a clock, and
                // the stall branch above ends it; a fast one is done now.
                if slow {
                    self.stall = true;
                } else {
                    self.active = false;
                }
                return 1;
            }

            // Row advance: MAME wraps within page for stride-256.
            // MAME: `dstart = (dstart & 0xff00) | ((dstart + dyadv) & 0xff);`
            // dyadv = 1 for stride-256, = w for stride-1.
            if (self.control & CTRL_DST_STRIDE_256) != 0 {
                self.dstart = (self.dstart & 0xFF00) | (self.dstart.wrapping_add(1) & 0x00FF);
            } else {
                self.dstart = self.dstart.wrapping_add(self.w);
            }
            if (self.control & CTRL_SRC_STRIDE_256) != 0 {
                self.sstart = (self.sstart & 0xFF00) | (self.sstart.wrapping_add(1) & 0x00FF);
            } else {
                self.sstart = self.sstart.wrapping_add(self.w);
            }
            self.cur_src = self.sstart;
            self.cur_dst = self.dstart;
        }

        self.stall = slow;
        1
    }

    /// Initialize blit execution state from the configured registers.
    ///
    /// Applies the size_xor (SC1 XOR 4 bug) to width and height, clamps
    /// zero to one, and configures stride advances from control bits.
    fn start_blit(&mut self) {
        // Width/height: XOR with size_xor, then clamp 0 → 1.
        // MAME: `int w = m_width ^ m_size_xor; if (w == 0) w = 1;`
        let mut w = (self.width ^ self.size_xor) as u16;
        let mut h = (self.height ^ self.size_xor) as u16;
        if w == 0 {
            w = 1;
        }
        if h == 0 {
            h = 1;
        }

        // Stride from control bits.
        // MAME: `sxadv = src_stride_256 ? 0x100 : 1;`
        let sxadv = if (self.control & CTRL_SRC_STRIDE_256) != 0 {
            256
        } else {
            1
        };
        let dxadv = if (self.control & CTRL_DST_STRIDE_256) != 0 {
            256
        } else {
            1
        };

        self.active = true;
        self.stall = false;
        self.w = w;
        self.h = h;
        self.x = 0;
        self.rows_done = 0;
        self.sstart = self.src_addr;
        self.dstart = self.dst_addr;
        self.cur_src = self.src_addr;
        self.cur_dst = self.dst_addr;
        self.sxadv = sxadv;
        self.dxadv = dxadv;
        self.shift_reg = 0;
    }

    /// Reset the blitter to idle state, preserving SC1/SC2 variant configuration.
    pub fn reset(&mut self) {
        self.control = 0;
        self.solid_color = 0;
        self.src_addr = 0;
        self.dst_addr = 0;
        self.width = 0;
        self.height = 0;
        // size_xor and clip_address preserved (construction-time configuration)
        self.window_enable = false;
        self.active = false;
        self.stall = false;
        self.x = 0;
        self.w = 0;
        self.h = 0;
        self.rows_done = 0;
        self.sstart = 0;
        self.dstart = 0;
        self.cur_src = 0;
        self.cur_dst = 0;
        self.sxadv = 0;
        self.dxadv = 0;
        self.shift_reg = 0;
    }
}

impl super::Device for WilliamsBlitter {
    fn name(&self) -> &'static str {
        "Williams Blitter"
    }
    fn reset(&mut self) {
        self.reset();
    }
    fn write(&mut self, offset: u16, data: u8) {
        self.write_register(offset, data);
    }
}

impl Default for WilliamsBlitter {
    fn default() -> Self {
        Self::sc1()
    }
}
