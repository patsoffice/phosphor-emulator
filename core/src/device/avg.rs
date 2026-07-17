//! Atari Analog Vector Generator (AVG) — Tempest, Quantum, and Star Wars variants
//!
//! A state-machine coprocessor that reads instructions from shared vector
//! RAM/ROM and generates a display list of colored line segments for rendering
//! on a color vector CRT.
//!
//! Three variants are implemented (selected by [`AvgVariant`]): Tempest (1981,
//! byte-addressed decode), Quantum (1982, word-addressed decode with 12-bit
//! normalization, Quantum color weights, and an X/Y coordinate swap), and Star
//! Wars (1983, byte-addressed like Tempest but with no XOR-1 swap and a simple
//! `color111`/8-bit-intensity color path). Other variants (Battle Zone, Major
//! Havoc) differ further in color decoding and coordinate handling.
//!
//! # Architecture
//!
//! The AVG reads 4 bytes per instruction from a contiguous vector memory space.
//! The real hardware uses a 256×4-bit PROM state machine that sequences through
//! handlers 0–3 (latching DVY, opcode, DVX, intensity) then dispatches to
//! handlers 4–7 (strobe0–strobe3) based on the 3-bit opcode.
//!
//! This implementation decodes instructions directly at the word level for
//! clarity, while matching the hardware's handler behavior exactly.
//!
//! # Byte addressing
//!
//! Tempest byte-addresses vector memory with an XOR-1 swap, reading the high
//! byte of each 16-bit word first (at even PC), then the low byte (at odd PC),
//! i.e. it addresses bytes as `(pc ^ 1)`. Star Wars uses the same byte-addressed
//! decode but *without* the swap (it reads bytes in native order). Quantum reads
//! whole big-endian words instead.
//!
//! # Instruction sizes
//!
//! - Op 0 (VCTR): 4-byte instruction (two 16-bit words)
//! - Ops 1–7: 2-byte instructions (one 16-bit word)
//!
//! The PROM state machine determines handler sequencing per opcode.
//! SVEC (op 2) packs DVX and int_latch into the low byte of its
//! single word (handler 3 reads it), with 4-bit DVX/DVY precision.
//!
//! # Tempest-specific behavior
//!
//! - Color RAM: 16 entries, looked up by color index in strobe3
//! - Color vs intensity select: bit 11 of DVY in strobe2
//! - Coordinate rotation (ROT270): output swaps X/Y axes
//! - Continuous loop: jump to address 0 triggers a frame flush
//!
//! # Reference
//!
//! - Atari avgdvg hardware (avg_device, avg_tempest_device)
//! - Jed Margolin, "The Secret Life of Vector Generators"

use super::dvg::VectorLine;
use crate::core::debug::{DebugRegister, Debuggable};
use crate::core::save_state::{SaveError, Saveable};

/// Which game's AVG decode/color/coordinate rules to apply.
///
/// The Analog Vector Generator was customised per game in its instruction
/// encoding, normalization precision, color decode, and coordinate handling.
/// This selects between the variants implemented here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum AvgVariant {
    /// Tempest (1981): byte-addressed (XOR-1) decode, 13-bit normalization,
    /// Tempest color weights, no coordinate swap.
    #[default]
    Tempest,
    /// Quantum (1982): word-addressed decode (`op = word >> 13`), 12-bit
    /// normalization, Quantum color weights, X/Y swap in the vector generator.
    Quantum,
    /// Star Wars (1983): byte-addressed decode like Tempest but with **no**
    /// XOR-1 byte swap, 13-bit normalization, and a simpler color path — the
    /// STAT strobe latches an 8-bit intensity plus a 3-bit `color111` index
    /// (no color RAM lookup).
    StarWars,
}

/// Atari AVG (Tempest / Quantum / Star Wars variants).
///
/// The AVG runs continuously (not halt-based like DVG). Each frame is
/// delineated by a jump to address 0, which flushes the accumulated
/// display list. The caller triggers execution via [`Avg::go`] + [`Avg::execute`].
pub struct Avg {
    /// Selected game variant (decode, color, coordinate rules).
    variant: AvgVariant,

    /// Program counter (byte address into vector memory).
    pc: u16,
    /// 4-entry return address stack (byte addresses).
    stack: [u16; 4],
    /// Stack pointer (only bits 1:0 used).
    sp: u8,

    /// Current beam X position (fixed-point, pixel << 16).
    xpos: i32,
    /// Current beam Y position (fixed-point).
    ypos: i32,

    /// Previous beam position for line segment generation (fixed-point).
    prev_x: i32,
    prev_y: i32,
    has_prev: bool,

    /// Analog scale factor (8-bit).
    scale: u8,
    /// Binary scale factor (3-bit).
    bin_scale: u8,
    /// Current color index (4-bit).
    color: u8,
    /// Current intensity (4-bit).
    intensity: u8,

    /// Center coordinates in fixed-point.
    xcenter: i32,
    ycenter: i32,

    /// DAC sign XOR values (0x200 for standard AVG).
    xdac_xor: u16,
    ydac_xor: u16,

    /// Axis flipping (set via $4000 write on Tempest).
    flip_x: bool,
    flip_y: bool,

    /// True when the AVG has halted.
    halted: bool,

    /// Accumulated display list for the current frame.
    display_list: Vec<VectorLine>,
}

/// One decoded AVG instruction (fields common to all variants).
struct Instr {
    /// 3-bit opcode (0 VCTR, 1 HALT, 2 SVEC, 3 STAT, 4 CNTR, 5 JSR, 6 RTS, 7 JMP).
    op: u8,
    /// Y delta / operand (13-bit).
    dvy: u16,
    /// X delta (13-bit Tempest, 12-bit Quantum).
    dvx: u16,
    /// Intensity latch (4-bit).
    int_latch: u8,
    /// DVY bit 12 (selects scale vs color/intensity in STAT).
    dvy12: u8,
    /// True for Tempest SVEC (8-bit timer path); always false for Quantum.
    is_short: bool,
}

impl Avg {
    /// Create a new AVG with beam center derived from visible area dimensions.
    ///
    /// Beam center: `xcenter = (visible_width / 2) << 16`,
    ///              `ycenter = (visible_height / 2) << 16`.
    /// For Tempest (visible area 0..580 x 0..570): xcenter=290<<16, ycenter=285<<16.
    pub fn new(visible_width: i32, visible_height: i32) -> Self {
        Self::with_variant(AvgVariant::Tempest, visible_width, visible_height)
    }

    /// Create a new AVG for a specific game variant.
    ///
    /// See [`Avg::new`] for the beam-center derivation; the variant selects the
    /// instruction decode, normalization precision, color decode, and
    /// coordinate handling.
    pub fn with_variant(variant: AvgVariant, visible_width: i32, visible_height: i32) -> Self {
        let xcenter = (visible_width / 2) << 16;
        let ycenter = (visible_height / 2) << 16;
        Self {
            variant,
            pc: 0,
            stack: [0; 4],
            sp: 0,
            xpos: xcenter,
            ypos: ycenter,
            prev_x: xcenter,
            prev_y: ycenter,
            has_prev: false,
            scale: 0,
            bin_scale: 0,
            color: 0,
            intensity: 0,
            xcenter,
            ycenter,
            xdac_xor: 0x200,
            ydac_xor: 0x200,
            flip_x: false,
            flip_y: false,
            halted: true,
            display_list: Vec::with_capacity(2048),
        }
    }

    /// Trigger AVG execution (CPU writes to AVG GO register).
    pub fn go(&mut self) {
        self.pc = 0;
        self.sp = 0;
        self.halted = false;
    }

    /// Returns true if the AVG has halted.
    pub fn is_halted(&self) -> bool {
        self.halted
    }

    /// Debug: return (scale, bin_scale, color, intensity).
    pub fn debug_state(&self) -> (u8, u8, u8, u8) {
        (self.scale, self.bin_scale, self.color, self.intensity)
    }

    /// Set axis flipping (controlled by hardware register).
    pub fn set_flip(&mut self, flip_x: bool, flip_y: bool) {
        self.flip_x = flip_x;
        self.flip_y = flip_y;
    }

    /// Reset to power-on state.
    pub fn reset(&mut self) {
        self.pc = 0;
        self.sp = 0;
        self.stack = [0; 4];
        self.scale = 0;
        self.bin_scale = 0;
        self.color = 0;
        self.intensity = 0;
        self.halted = true;
        self.has_prev = false;
        self.xpos = self.xcenter;
        self.ypos = self.ycenter;
        self.prev_x = self.xcenter;
        self.prev_y = self.ycenter;
        self.display_list.clear();
    }

    /// Execute AVG instructions until halt or frame boundary (jump to address 0).
    ///
    /// `vmem` is the combined vector RAM + ROM (8 KB for Tempest).
    /// `color_ram` is the 16-entry color RAM for Tempest color lookup.
    ///
    /// Returns true if a frame was completed.
    pub fn execute(&mut self, vmem: &[u8], color_ram: &[u8; 16]) -> bool {
        let mut instructions = 0u32;
        const MAX_INSTRUCTIONS: u32 = 50_000;

        while !self.halted && instructions < MAX_INSTRUCTIONS {
            // --- Decode (variant-specific) ---
            let Instr {
                op,
                dvy,
                dvx,
                int_latch,
                dvy12,
                is_short,
            } = match self.variant {
                AvgVariant::Tempest => self.decode_tempest(vmem),
                AvgVariant::Quantum => self.decode_quantum(vmem),
                AvgVariant::StarWars => self.decode_starwars(vmem),
            };

            // --- Execute. Op meanings (0/2 VCTR/SVEC, 1 HALT, 3 STAT, 4 CNTR,
            // 5 JSR, 6 RTS, 7 JMP) are shared across variants; only the vector
            // draw math, normalization, and STAT color latch differ. ---
            match op {
                0 | 2 => match self.variant {
                    AvgVariant::Tempest => {
                        let (norm_dvx, norm_dvy, timer) = self.normalize(dvx, dvy, is_short);
                        let timer = self.apply_bin_scale(timer, is_short);
                        self.draw_vector(norm_dvx, norm_dvy, timer, is_short, int_latch, color_ram);
                    }
                    AvgVariant::Quantum => {
                        let (norm_dvx, norm_dvy, timer) = self.normalize_quantum(dvx, dvy);
                        let timer = self.apply_bin_scale_quantum(timer);
                        self.draw_quantum(norm_dvx, norm_dvy, timer, int_latch, color_ram);
                    }
                    AvgVariant::StarWars => {
                        // Star Wars shares Tempest's 13-bit normalization and
                        // strobe3 position math; only color/intensity differ.
                        let (norm_dvx, norm_dvy, timer) = self.normalize(dvx, dvy, is_short);
                        let timer = self.apply_bin_scale(timer, is_short);
                        self.draw_starwars(norm_dvx, norm_dvy, timer, is_short, int_latch);
                    }
                },
                1 => self.halted = true,
                3 => {
                    if dvy12 != 0 {
                        self.scale = (dvy & 0xFF) as u8;
                        self.bin_scale = ((dvy >> 8) & 7) as u8;
                    } else if self.variant == AvgVariant::StarWars {
                        // Star Wars strobe2 latches an 8-bit intensity and a
                        // 4-bit color index together, ungated (no 0x800 select).
                        self.intensity = (dvy & 0xFF) as u8;
                        self.color = ((dvy >> 8) & 0xF) as u8;
                    } else if dvy & 0x800 != 0 {
                        self.color = (dvy & 0xF) as u8;
                        // Quantum latches color and intensity together (its
                        // strobe2); Tempest latches only color here.
                        if self.variant == AvgVariant::Quantum {
                            self.intensity = ((dvy >> 4) & 0xF) as u8;
                        }
                    } else if self.variant == AvgVariant::Tempest {
                        self.intensity = ((dvy >> 4) & 0xF) as u8;
                    }
                }
                4 => {
                    self.xpos = self.xcenter;
                    self.ypos = self.ycenter;
                    self.add_point(self.xpos, self.ypos, 0, [0, 0, 0]);
                }
                5 => {
                    self.stack[(self.sp & 3) as usize] = self.pc;
                    self.sp = self.sp.wrapping_add(1) & 0xF;
                    self.pc = dvy << 1;
                    if dvy == 0 {
                        self.halted = true;
                        return true;
                    }
                }
                6 => {
                    self.sp = self.sp.wrapping_sub(1) & 0xF;
                    self.pc = self.stack[(self.sp & 3) as usize];
                }
                7 => {
                    self.pc = dvy << 1;
                    if dvy == 0 {
                        self.halted = true;
                        return true;
                    }
                }
                _ => {}
            }

            instructions += 1;
        }
        false
    }

    /// Drain the display list, returning ownership to the caller.
    pub fn take_display_list(&mut self) -> Vec<VectorLine> {
        self.has_prev = false;
        std::mem::take(&mut self.display_list)
    }

    // -----------------------------------------------------------------------
    // Instruction decode and execute
    // -----------------------------------------------------------------------

    /// Read one big-endian 16-bit word at byte address `addr` (Quantum decode).
    ///
    /// Quantum's 68000 writes vector RAM as big-endian words, and the AVG reads
    /// whole words (no XOR-1 byte swap), so the word at even PC is `[hi, lo]`.
    fn read_word_be(vmem: &[u8], addr: u16) -> u16 {
        let i = addr as usize;
        let hi = vmem.get(i).copied().unwrap_or(0);
        let lo = vmem.get(i + 1).copied().unwrap_or(0);
        u16::from_be_bytes([hi, lo])
    }

    /// Decode one Tempest instruction at the current PC, advancing PC.
    ///
    /// The PROM state machine reads bytes in handler order 1,0 (high byte first
    /// via XOR-1), then 3,2 for the 4-byte VCTR. VCTR (op 0) is 4 bytes, SVEC
    /// (op 2) packs DVX/int_latch into its single word, all others are 2 bytes.
    fn decode_tempest(&mut self, vmem: &[u8]) -> Instr {
        self.decode_byte_addressed(vmem, true)
    }

    /// Decode one Star Wars instruction at the current PC, advancing PC.
    ///
    /// Star Wars uses the same byte-addressed 13-bit layout as Tempest, but the
    /// AVG reads vector memory *without* the XOR-1 byte swap (`starwars_data`),
    /// so the bytes are consumed in native order.
    fn decode_starwars(&mut self, vmem: &[u8]) -> Instr {
        self.decode_byte_addressed(vmem, false)
    }

    /// Shared byte-addressed decode for Tempest (`swap = true`, XOR-1 addressing)
    /// and Star Wars (`swap = false`, native addressing).
    ///
    /// The PROM state machine reads bytes in handler order 1,0 (op/high byte
    /// first), then 3,2 for the 4-byte VCTR. VCTR (op 0) is 4 bytes, SVEC (op 2)
    /// packs DVX/int_latch into its single word, all others are 2 bytes.
    fn decode_byte_addressed(&mut self, vmem: &[u8], swap: bool) -> Instr {
        let rd = |addr: u16| -> u8 {
            let idx = if swap {
                (addr as usize) ^ 1
            } else {
                addr as usize
            };
            vmem.get(idx).copied().unwrap_or(0)
        };
        let hi0 = rd(self.pc);
        let lo0 = rd(self.pc.wrapping_add(1));
        let dvy12 = (hi0 >> 4) & 1;
        let op = hi0 >> 5;

        let (dvy, dvx, int_latch) = if op == 0 {
            let dvy = (u16::from(dvy12) << 12) | (u16::from(hi0 & 0xF) << 8) | u16::from(lo0);
            let hi1 = rd(self.pc.wrapping_add(2));
            let lo1 = rd(self.pc.wrapping_add(3));
            self.pc = self.pc.wrapping_add(4);
            let il = hi1 >> 4;
            let dx = (u16::from(il & 1) << 12) | (u16::from(hi1 & 0xF) << 8) | u16::from(lo1);
            (dvy, dx, il)
        } else if op == 2 {
            let dvy = (u16::from(dvy12) << 12) | (u16::from(hi0 & 0xF) << 8);
            let il = lo0 >> 4;
            let dx = (u16::from(il & 1) << 12) | (u16::from(lo0 & 0xF) << 8);
            self.pc = self.pc.wrapping_add(2);
            (dvy, dx, il)
        } else {
            let dvy = (u16::from(dvy12) << 12) | (u16::from(hi0 & 0xF) << 8) | u16::from(lo0);
            self.pc = self.pc.wrapping_add(2);
            (dvy, 0u16, 0u8)
        };

        Instr {
            op,
            dvy,
            dvx,
            int_latch,
            dvy12,
            is_short: op == 2,
        }
    }

    /// Decode one Quantum instruction at the current PC, advancing PC.
    ///
    /// Quantum reads whole 16-bit words: `op = word >> 13`, `dvy12 = bit 12`,
    /// `dvy = word & 0x1FFF`. VCTR (op 0) reads a second word for
    /// `int_latch = word >> 12` and `dvx = word & 0xFFF`; all others are one
    /// word. There is no SVEC, so `is_short` is always false.
    fn decode_quantum(&mut self, vmem: &[u8]) -> Instr {
        let word0 = Self::read_word_be(vmem, self.pc);
        let op = (word0 >> 13) as u8;
        let dvy12 = ((word0 >> 12) & 1) as u8;
        let dvy = word0 & 0x1FFF;

        let (dvx, int_latch) = if op == 0 {
            let word1 = Self::read_word_be(vmem, self.pc.wrapping_add(2));
            self.pc = self.pc.wrapping_add(4);
            (word1 & 0x0FFF, (word1 >> 12) as u8)
        } else {
            self.pc = self.pc.wrapping_add(2);
            (0u16, 0u8)
        };

        Instr {
            op,
            dvy,
            dvx,
            int_latch,
            dvy12,
            is_short: false,
        }
    }

    /// Normalize DVX/DVY (strobe0) — shift both axes together until EITHER
    /// is normalized (sign bit differs from MSB).
    fn normalize(&self, mut dvx: u16, mut dvy: u16, is_short: bool) -> (u16, u16, u16) {
        let mut timer: u16 = 0;
        let op1_bit: u16 = if is_short { 0x80 } else { 0 };

        // Continue while BOTH axes need normalization (AND condition).
        // Stop when EITHER axis is normalized or after 16 iterations.
        let mut i = 0;
        while (((dvy ^ (dvy << 1)) & 0x1000) == 0)
            && (((dvx ^ (dvx << 1)) & 0x1000) == 0)
            && (i < 16)
        {
            dvy = (dvy & 0x1000) | ((dvy << 1) & 0x1FFF);
            dvx = (dvx & 0x1000) | ((dvx << 1) & 0x1FFF);
            timer >>= 1;
            timer |= 0x4000 | op1_bit;
            i += 1;
        }

        // SVEC: mask timer to 8 bits
        if is_short {
            timer &= 0xFF;
        }

        (dvx, dvy, timer)
    }

    /// Apply binary scale to the timer (strobe1).
    fn apply_bin_scale(&self, mut timer: u16, is_short: bool) -> u16 {
        let op1_bit: u16 = if is_short { 0x80 } else { 0 };

        for _ in 0..self.bin_scale {
            timer >>= 1;
            timer |= 0x4000 | op1_bit;
        }

        // SVEC: mask timer to 8 bits again after bin_scale
        if is_short {
            timer &= 0xFF;
        }

        timer
    }

    /// Draw a vector from the current beam position using normalized DVX/DVY.
    /// Matches MAME's `avg_common_strobe3` + `tempest_strobe3`.
    fn draw_vector(
        &mut self,
        dvx: u16,
        dvy: u16,
        timer: u16,
        is_short: bool,
        int_latch: u8,
        color_ram: &[u8; 16],
    ) {
        // SVEC uses 8-bit timer, VCTR uses 16-bit timer
        let cycles: i32 = if is_short {
            0x100_i32 - i32::from(timer & 0xFF)
        } else {
            0x8000_i32 - i32::from(timer)
        };

        // Scale factor: complement of 8-bit scale
        let scale_factor: i32 = i32::from(self.scale) ^ 0xFF;

        // DAC conversion: upper 10 bits of 13-bit value, XOR for sign, center at 0
        let dx = ((i32::from(dvx >> 3) ^ i32::from(self.xdac_xor)) - 0x200)
            .wrapping_mul(cycles)
            .wrapping_mul(scale_factor)
            >> 4;
        let dy = ((i32::from(dvy >> 3) ^ i32::from(self.ydac_xor)) - 0x200)
            .wrapping_mul(cycles)
            .wrapping_mul(scale_factor)
            >> 4;
        self.xpos = self.xpos.wrapping_add(dx);
        self.ypos = self.ypos.wrapping_sub(dy);

        // Tempest color RAM lookup
        let data = color_ram[(self.color & 0xF) as usize];
        let bit3 = (!data >> 3) & 1;
        let bit2 = (!data >> 2) & 1;
        let bit1 = (!data >> 1) & 1;
        let bit0 = !data & 1;
        let r = bit1
            .wrapping_mul(0xF3)
            .wrapping_add(bit0.wrapping_mul(0x0C));
        let g = bit3.wrapping_mul(0xF3);
        let b = bit2.wrapping_mul(0xF3);

        // Effective intensity: when int_latch bits 3:1 == 001 (DATEA signal), use stored intensity
        // from STAT register; otherwise use int_latch bits 3:1 as direct intensity.
        let eff_intensity = if (int_latch >> 1) == 1 {
            self.intensity
        } else {
            int_latch & 0xE
        };

        // Apply flipping
        let mut x = self.xpos;
        let mut y = self.ypos;
        if self.flip_x {
            x += (self.xcenter - x) << 1;
        }
        if self.flip_y {
            y += (self.ycenter - y) << 1;
        }

        self.add_point(x, y, eff_intensity, [r, g, b]);
    }

    /// Normalize DVX/DVY for Quantum (`quantum_strobe0`): 12-bit precision
    /// (sign at bit 11), shifting both axes together until either normalizes.
    fn normalize_quantum(&self, mut dvx: u16, mut dvy: u16) -> (u16, u16, u16) {
        let mut timer: u16 = 0;
        let mut i = 0;
        while (((dvy ^ (dvy << 1)) & 0x800) == 0) && (((dvx ^ (dvx << 1)) & 0x800) == 0) && (i < 16)
        {
            dvy = (dvy << 1) & 0xFFF;
            dvx = (dvx << 1) & 0xFFF;
            timer >>= 1;
            timer |= 0x2000;
            i += 1;
        }
        (dvx, dvy, timer)
    }

    /// Apply binary scale to the Quantum timer (`quantum_strobe1`).
    fn apply_bin_scale_quantum(&self, mut timer: u16) -> u16 {
        for _ in 0..self.bin_scale {
            timer >>= 1;
            timer |= 0x2000;
        }
        timer
    }

    /// Draw a vector for Quantum (`quantum_strobe3`): 12-bit DAC, Quantum color
    /// weights, and the vector generator's X/Y coordinate swap.
    fn draw_quantum(
        &mut self,
        dvx: u16,
        dvy: u16,
        timer: u16,
        int_latch: u8,
        color_ram: &[u8; 16],
    ) {
        let cycles: i32 = 0x4000_i32 - i32::from(timer);
        let scale_factor: i32 = i32::from(self.scale) ^ 0xFF;

        // 12-bit DAC: upper 10 bits of the 12-bit value (>> 2), XOR for sign.
        let dx = ((i32::from((dvx & 0xFFF) >> 2) ^ i32::from(self.xdac_xor)) - 0x200)
            .wrapping_mul(cycles)
            .wrapping_mul(scale_factor)
            >> 4;
        let dy = ((i32::from((dvy & 0xFFF) >> 2) ^ i32::from(self.ydac_xor)) - 0x200)
            .wrapping_mul(cycles)
            .wrapping_mul(scale_factor)
            >> 4;
        self.xpos = self.xpos.wrapping_add(dx);
        self.ypos = self.ypos.wrapping_sub(dy);

        // Quantum color: 16-entry color RAM (low byte holds the 4 active bits,
        // inverted). r = bit3·0xCE, g = bit1·0xAA + bit0·0x54, b = bit2·0xCE.
        let data = color_ram[(self.color & 0xF) as usize];
        let bit3 = (!data >> 3) & 1;
        let bit2 = (!data >> 2) & 1;
        let bit1 = (!data >> 1) & 1;
        let bit0 = !data & 1;
        let r = bit3.wrapping_mul(0xCE);
        let g = bit1
            .wrapping_mul(0xAA)
            .wrapping_add(bit0.wrapping_mul(0x54));
        let b = bit2.wrapping_mul(0xCE);

        // Intensity: int_latch==2 (DATEA) selects the stored STAT intensity.
        let eff_intensity = if int_latch == 2 {
            self.intensity
        } else {
            int_latch
        };

        // Emit the point in beam/screen space (with flipping), matching the
        // Tempest path. MAME's quantum_strobe3 also transposes X/Y here, but
        // that compensates for MAME's true-rotation screen pipeline; this
        // engine's vector renderer applies only a Y-flip for portrait monitors,
        // so the portrait orientation comes from the portrait display buffer
        // instead (see the Quantum machine's TIMING).
        let mut x = self.xpos;
        let mut y = self.ypos;
        if self.flip_x {
            x += (self.xcenter - x) << 1;
        }
        if self.flip_y {
            y += (self.ycenter - y) << 1;
        }
        // Emit directly in screen space. The Quantum machine presents this as a
        // pre-rotated portrait display (display_size already portrait,
        // Orientation::NORMAL), so no transpose/flip is applied here — unlike
        // MAME, whose AVG transposes X/Y to feed a true-rotation screen.
        self.add_point(x, y, eff_intensity, [r, g, b]);
    }

    /// Draw a vector for Star Wars (`starwars_strobe3`): the same 13-bit strobe3
    /// position math as Tempest, but a `color111` color (no color RAM) and a
    /// combined intensity of `(int_latch >> 1) * intensity`.
    fn draw_starwars(&mut self, dvx: u16, dvy: u16, timer: u16, is_short: bool, int_latch: u8) {
        let cycles: i32 = if is_short {
            0x100_i32 - i32::from(timer & 0xFF)
        } else {
            0x8000_i32 - i32::from(timer)
        };
        let scale_factor: i32 = i32::from(self.scale) ^ 0xFF;

        let dx = ((i32::from(dvx >> 3) ^ i32::from(self.xdac_xor)) - 0x200)
            .wrapping_mul(cycles)
            .wrapping_mul(scale_factor)
            >> 4;
        let dy = ((i32::from(dvy >> 3) ^ i32::from(self.ydac_xor)) - 0x200)
            .wrapping_mul(cycles)
            .wrapping_mul(scale_factor)
            >> 4;
        self.xpos = self.xpos.wrapping_add(dx);
        // Star Wars is a normal (ROT0) monitor, so the display list uses the
        // Y-up convention (higher Y = higher on screen) the renderers expect for
        // unrotated games — the opposite of Tempest's `ypos -= dy`.
        self.ypos = self.ypos.wrapping_add(dy);

        // color111: low 3 bits index one-bit-per-channel RGB (bit2=R, 1=G, 0=B).
        let c = self.color;
        let r = if (c >> 2) & 1 != 0 { 0xFF } else { 0 };
        let g = if (c >> 1) & 1 != 0 { 0xFF } else { 0 };
        let b = if c & 1 != 0 { 0xFF } else { 0 };

        // MAME's strobe3 intensity is `((int_latch >> 1) * intensity) >> 3`, an
        // 8-bit brightness. The renderer's LUT is 4-bit, so shift down by a
        // further 4 (total >> 7) and clamp — preserving relative brightness.
        let brightness = (u32::from(int_latch >> 1) * u32::from(self.intensity)) >> 3;
        let eff_intensity = ((brightness >> 4).min(15)) as u8;

        // Star Wars does not flip the beam (its strobe3 emits xpos/ypos directly).
        self.add_point(self.xpos, self.ypos, eff_intensity, [r, g, b]);
    }

    /// Add a point to the display list, creating a line from the previous point.
    ///
    /// Coordinates are stored unclamped — the rasterizer handles clipping.
    fn add_point(&mut self, x: i32, y: i32, intensity: u8, rgb: [u8; 3]) {
        // Convert from fixed-point to pixel coordinates (no clamping).
        let px = x >> 16;
        let py = y >> 16;

        if self.has_prev {
            let prev_px = self.prev_x >> 16;
            let prev_py = self.prev_y >> 16;

            if intensity == 0 && prev_px == px && prev_py == py {
                self.prev_x = x;
                self.prev_y = y;
                return;
            }

            self.display_list.push(VectorLine {
                x0: prev_px,
                y0: prev_py,
                x1: px,
                y1: py,
                intensity,
                r: rgb[0],
                g: rgb[1],
                b: rgb[2],
            });
        } else {
            self.display_list.push(VectorLine {
                x0: px,
                y0: py,
                x1: px,
                y1: py,
                intensity: 0,
                r: 0,
                g: 0,
                b: 0,
            });
        }

        self.prev_x = x;
        self.prev_y = y;
        self.has_prev = true;
    }
}

impl Debuggable for Avg {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "PC",
                value: self.pc as u64,
                width: 16,
            },
            DebugRegister {
                name: "X",
                value: (self.xpos >> 16) as u64,
                width: 16,
            },
            DebugRegister {
                name: "Y",
                value: (self.ypos >> 16) as u64,
                width: 16,
            },
            DebugRegister {
                name: "SCALE",
                value: self.scale as u64,
                width: 8,
            },
            DebugRegister {
                name: "COLOR",
                value: self.color as u64,
                width: 8,
            },
            DebugRegister {
                name: "INTEN",
                value: self.intensity as u64,
                width: 8,
            },
            DebugRegister {
                name: "HALT",
                value: self.halted as u64,
                width: 8,
            },
        ]
    }
}

impl Saveable for Avg {
    fn save_state(&self, w: &mut crate::core::save_state::StateWriter) {
        w.write_u16_le(self.pc);
        for &s in &self.stack {
            w.write_u16_le(s);
        }
        w.write_u8(self.sp);
        w.write_i32_le(self.xpos);
        w.write_i32_le(self.ypos);
        w.write_u8(self.scale);
        w.write_u8(self.bin_scale);
        w.write_u8(self.color);
        w.write_u8(self.intensity);
        w.write_bool(self.flip_x);
        w.write_bool(self.flip_y);
        w.write_bool(self.halted);
    }

    fn load_state(
        &mut self,
        r: &mut crate::core::save_state::StateReader,
    ) -> Result<(), SaveError> {
        self.pc = r.read_u16_le()?;
        for s in &mut self.stack {
            *s = r.read_u16_le()?;
        }
        self.sp = r.read_u8()?;
        self.xpos = r.read_i32_le()?;
        self.ypos = r.read_i32_le()?;
        self.scale = r.read_u8()?;
        self.bin_scale = r.read_u8()?;
        self.color = r.read_u8()?;
        self.intensity = r.read_u8()?;
        self.flip_x = r.read_bool()?;
        self.flip_y = r.read_bool()?;
        self.halted = r.read_bool()?;
        self.has_prev = false;
        self.display_list.clear();
        Ok(())
    }
}

impl super::Device for Avg {
    fn name(&self) -> &'static str {
        "AVG"
    }
    fn reset(&mut self) {
        self.reset();
    }
}

impl Default for Avg {
    fn default() -> Self {
        Self::new(1024, 1024)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build vector memory from 16-bit words stored as little-endian byte pairs.
    ///
    /// Bytes are stored directly at their physical addresses. For the Tempest
    /// decode the AVG applies the XOR-1 byte swap (reading high byte first);
    /// the Star Wars decode reads them in native order.
    fn build_vmem(bytes: &[u8]) -> Vec<u8> {
        let mut vmem = vec![0u8; 8192]; // 8KB
        for (i, &b) in bytes.iter().enumerate() {
            vmem[i] = b;
        }
        vmem
    }

    /// Helper: encode a 16-bit AVG word as [low_byte, high_byte] for build_vmem.
    fn word(val: u16) -> [u8; 2] {
        [(val & 0xFF) as u8, (val >> 8) as u8]
    }

    fn default_color_ram() -> [u8; 16] {
        // All white (inverted bits → r=0xFF, g=0xF3, b=0xF3)
        [0x00; 16]
    }

    #[test]
    fn new_starts_halted() {
        let avg = Avg::new(1024, 1024);
        assert!(avg.is_halted());
    }

    #[test]
    fn go_clears_halt() {
        let mut avg = Avg::new(1024, 1024);
        avg.go();
        assert!(!avg.is_halted());
        assert_eq!(avg.pc, 0);
    }

    #[test]
    fn halt_instruction() {
        // HALT: op=1 → high byte = 0b001_0_0000 = 0x20
        // 2-byte instruction: only word 0 needed.
        let w0 = word(0x2000); // HALT
        let vmem = build_vmem(&[w0[0], w0[1]]);
        let color_ram = default_color_ram();
        let mut avg = Avg::new(1024, 1024);
        avg.go();
        avg.execute(&vmem, &color_ram);
        assert!(avg.is_halted());
    }

    #[test]
    fn reset_clears_state() {
        let mut avg = Avg::new(1024, 1024);
        avg.go();
        avg.color = 5;
        avg.intensity = 12;
        avg.scale = 0x80;
        avg.reset();
        assert!(avg.is_halted());
        assert_eq!(avg.color, 0);
        assert_eq!(avg.intensity, 0);
        assert_eq!(avg.scale, 0);
        assert!(avg.display_list.is_empty());
    }

    #[test]
    fn frame_boundary_on_jsr_to_zero() {
        // JSR to address 0 = frame boundary.
        // JSR: op=5 → high byte = 0b101_0_0000 = 0xA0, dvy=0 → target=0
        // This is a 2-byte instruction.
        let w0 = word(0xA000); // JSR, dvy=0
        let vmem = build_vmem(&[w0[0], w0[1]]);
        let color_ram = default_color_ram();
        let mut avg = Avg::new(1024, 1024);
        avg.go();
        let frame = avg.execute(&vmem, &color_ram);
        assert!(frame, "expected frame boundary on JSR to address 0");
    }

    #[test]
    fn frame_boundary_on_jmp_to_zero() {
        // JMP to address 0 = frame boundary.
        // JMP: op=7 → high byte = 0b111_0_0000 = 0xE0, dvy=0 → target=0
        let w0 = word(0xE000); // JMP, dvy=0
        let vmem = build_vmem(&[w0[0], w0[1]]);
        let color_ram = default_color_ram();
        let mut avg = Avg::new(1024, 1024);
        avg.go();
        let frame = avg.execute(&vmem, &color_ram);
        assert!(frame, "expected frame boundary on JMP to address 0");
    }

    #[test]
    fn two_byte_instruction_advances_pc_by_2() {
        // CNTR (op=4) is a 2-byte instruction. After it, PC should be 2.
        // Then a HALT at byte offset 2 should be reached.
        // CNTR: op=4 → high byte = 0b100_0_0000 = 0x80
        // HALT: op=1 → 0x2000 (also 2-byte)
        let w0 = word(0x8000); // CNTR (2 bytes)
        let w1 = word(0x2000); // HALT (2 bytes)
        let vmem = build_vmem(&[w0[0], w0[1], w1[0], w1[1]]);
        let color_ram = default_color_ram();
        let mut avg = Avg::new(1024, 1024);
        avg.go();
        avg.execute(&vmem, &color_ram);
        assert!(
            avg.is_halted(),
            "CNTR should advance PC by 2, reaching HALT at offset 2"
        );
    }

    #[test]
    fn stat_advances_pc_by_2() {
        // STAT (op=3) is a 2-byte instruction. After it, PC should be 2.
        // STAT with dvy12=0, bit11=0: sets intensity.
        // STAT: op=3 → high byte = 0b011_0_0000 = 0x60, dvy12=0
        // Set intensity to 0xA: DVY bits [7:4] = 0xA → low byte = 0xA0
        let w0 = word(0x6000 | 0x00A0); // STAT: set intensity to 0xA
        let w1 = word(0x2000); // HALT at offset 2
        let vmem = build_vmem(&[w0[0], w0[1], w1[0], w1[1]]);
        let color_ram = default_color_ram();
        let mut avg = Avg::new(1024, 1024);
        avg.go();
        avg.execute(&vmem, &color_ram);
        assert!(avg.is_halted());
        assert_eq!(avg.intensity, 0xA);
    }

    #[test]
    fn color_ram_decode() {
        // Verify color RAM decode matches Tempest hardware.
        // color_ram[0] = 0x05, ~0x05 = 0xFA
        // bit0 = 0xFA & 1 = 0
        // bit1 = (0xFA >> 1) & 1 = 1
        // bit2 = (0xFA >> 2) & 1 = 0
        // bit3 = (0xFA >> 3) & 1 = 1
        // r = 1 * 0xF3 + 0 * 0x0C = 0xF3
        // g = 1 * 0xF3 = 0xF3
        // b = 0 * 0xF3 = 0
        let data: u8 = 0x05;
        let bit3 = (!data >> 3) & 1;
        let bit2 = (!data >> 2) & 1;
        let bit1 = (!data >> 1) & 1;
        let bit0 = !data & 1;
        let r = bit1
            .wrapping_mul(0xF3)
            .wrapping_add(bit0.wrapping_mul(0x0C));
        let g = bit3.wrapping_mul(0xF3);
        let b = bit2.wrapping_mul(0xF3);
        assert_eq!(r, 0xF3);
        assert_eq!(g, 0xF3);
        assert_eq!(b, 0);
    }

    #[test]
    fn vctr_then_halt_produces_display_list() {
        // VCTR (op=0) draws a vector, then HALT stops. Display list should
        // contain the drawn vector even though execute returns false.
        //
        // VCTR word 0: op=0, dvy12=0, DVY = 0x200 → 0x0200
        //   high byte = 0b000_0_0010 = 0x02, low byte = 0x00
        //   word = 0x0200
        // VCTR word 1: int_latch=0x8 (intensity=8), DVX = 0x200
        //   high byte = 0b1000_0010 = 0x82, low byte = 0x00
        //   word = 0x8200
        // HALT: 0x2000 (2-byte instruction)
        let vmem = build_vmem(&[
            0x00, 0x02, 0x00, 0x82, // VCTR: DVY=0x200, DVX=0x200, intensity=8
            0x00, 0x20, // HALT
        ]);
        let color_ram = default_color_ram();
        let mut avg = Avg::new(1024, 1024);
        avg.go();
        let frame = avg.execute(&vmem, &color_ram);
        assert!(!frame, "HALT should not signal frame boundary");
        assert!(avg.is_halted(), "AVG should be halted after HALT");

        let display_list = avg.take_display_list();
        assert!(
            !display_list.is_empty(),
            "display list should contain vectors drawn before HALT"
        );
    }

    // --- Quantum variant -------------------------------------------------

    /// Build Quantum vector memory from 16-bit words stored big-endian
    /// (`[hi, lo]`), matching how the 68000 writes vector RAM.
    fn build_vmem_be(words: &[u16]) -> Vec<u8> {
        let mut vmem = vec![0u8; 8192];
        for (i, &w) in words.iter().enumerate() {
            vmem[i * 2] = (w >> 8) as u8;
            vmem[i * 2 + 1] = (w & 0xFF) as u8;
        }
        vmem
    }

    #[test]
    fn quantum_halt_decodes_op_from_high_bits() {
        // HALT is op 1: word >> 13 == 1 → 0x2000.
        let vmem = build_vmem_be(&[0x2000]);
        let mut avg = Avg::with_variant(AvgVariant::Quantum, 900, 600);
        avg.go();
        avg.execute(&vmem, &[0u8; 16]);
        assert!(avg.is_halted());
    }

    #[test]
    fn quantum_jmp_to_zero_is_frame_boundary() {
        // JMP is op 7: word >> 13 == 7 → 0xE000, dvy = 0 → target 0.
        let vmem = build_vmem_be(&[0xE000]);
        let mut avg = Avg::with_variant(AvgVariant::Quantum, 900, 600);
        avg.go();
        assert!(avg.execute(&vmem, &[0u8; 16]));
    }

    #[test]
    fn quantum_stat_sets_scale_and_color() {
        // STAT is op 3 (0x6000). With dvy12 (bit 12) set: scale = dvy & 0xFF,
        // bin_scale = (dvy >> 8) & 7.
        //   word = 0x6000 | (1<<12) | (3<<8) | 0x80 = 0x7380
        // Then STAT with bit 11 set latches color (dvy & 0xF) and intensity.
        //   word = 0x6000 | 0x800 | (0xA<<4) | 0x5 = 0x68A5
        let vmem = build_vmem_be(&[0x7380, 0x68A5, 0x2000]);
        let mut avg = Avg::with_variant(AvgVariant::Quantum, 900, 600);
        avg.go();
        avg.execute(&vmem, &[0u8; 16]);
        assert!(avg.is_halted());
        assert_eq!(avg.scale, 0x80);
        assert_eq!(avg.bin_scale, 3);
        assert_eq!(avg.color, 0x5);
        assert_eq!(avg.intensity, 0xA);
    }

    #[test]
    fn quantum_vctr_reads_two_words_and_draws() {
        // VCTR (op 0): word0 carries dvy, word1 carries int_latch + dvx.
        // word0 = dvy = 0x0400; word1 = (int_latch=0x8 << 12) | dvx(0x400) = 0x8400.
        let vmem = build_vmem_be(&[0x0400, 0x8400, 0x2000]);
        let mut avg = Avg::with_variant(AvgVariant::Quantum, 900, 600);
        avg.go();
        let frame = avg.execute(&vmem, &[0u8; 16]);
        assert!(!frame);
        assert!(avg.is_halted());
        let list = avg.take_display_list();
        assert!(!list.is_empty(), "VCTR should have produced a line");
    }

    #[test]
    fn quantum_color_decode_weights() {
        // Quantum: r = bit3·0xCE, g = bit1·0xAA + bit0·0x54, b = bit2·0xCE,
        // where bitN is the inverted low nibble of the color RAM entry.
        // color_ram[0] = 0x0A (~0x0A = ...0101): bit0=1, bit1=0, bit2=1, bit3=0.
        //   r = 0, g = 0x54, b = 0xCE.
        let mut color_ram = [0u8; 16];
        color_ram[0] = 0x0A;
        // The first draw point only seeds the beam (intensity forced to 0), so
        // emit two VCTRs: the second produces the first lit line.
        // word0 dvy=0x0400, word1 int_latch=0x8, dvx=0x400.
        let vmem = build_vmem_be(&[0x0400, 0x8400, 0x0400, 0x8400, 0x2000]);
        let mut avg = Avg::with_variant(AvgVariant::Quantum, 900, 600);
        avg.go();
        avg.execute(&vmem, &color_ram);
        let list = avg.take_display_list();
        let drawn = list.iter().find(|l| l.intensity != 0).expect("a lit line");
        assert_eq!((drawn.r, drawn.g, drawn.b), (0x00, 0x54, 0xCE));
    }

    #[test]
    fn stat_sets_scale() {
        // STAT with dvy12=1: sets scale and bin_scale.
        // STAT is a 2-byte instruction.
        // op=3, dvy12=1 → high byte bits: 011_1_YYYY
        // scale = DVY & 0xFF = low byte
        // bin_scale = (DVY >> 8) & 7 = high byte bits 2:0
        //
        // Set scale=0x80, bin_scale=3:
        //   DVY = (3 << 8) | 0x80 = 0x0380
        //   dvy12=1 → bit 4 of high byte set
        //   high byte = 0b011_1_0011 = 0x73
        //   low byte = 0x80
        //   word = 0x7380
        let w0 = word(0x7380); // STAT: dvy12=1, scale=0x80, bin_scale=3
        let w1 = word(0x2000); // HALT at offset 2
        let vmem = build_vmem(&[w0[0], w0[1], w1[0], w1[1]]);
        let color_ram = default_color_ram();
        let mut avg = Avg::new(1024, 1024);
        avg.go();
        avg.execute(&vmem, &color_ram);
        assert!(avg.is_halted());
        assert_eq!(avg.scale, 0x80);
        assert_eq!(avg.bin_scale, 3);
    }

    // --- Star Wars variant -----------------------------------------------
    //
    // Star Wars is byte-addressed like Tempest but reads vector memory WITHOUT
    // the XOR-1 swap, so the op/high byte sits at the even address. In these
    // tests each word is laid out high-byte-first.

    #[test]
    fn starwars_no_xor_byte_order() {
        // A Star Wars HALT is [0x20, 0x00] (op byte first). Decoded as Tempest
        // the same physical bytes read the high byte from the odd address
        // (0x00) → op 0 (VCTR), so it does not halt.
        let mut sw = Avg::with_variant(AvgVariant::StarWars, 1024, 1024);
        sw.go();
        sw.execute(&build_vmem(&[0x20, 0x00]), &default_color_ram());
        assert!(sw.is_halted(), "Star Wars decodes [0x20,0x00] as HALT");

        let mut tempest = Avg::new(1024, 1024);
        tempest.go();
        tempest.execute(&build_vmem(&[0x20, 0x00]), &default_color_ram());
        assert!(
            !tempest.is_halted(),
            "Tempest reads the swapped byte order and never reaches HALT"
        );
    }

    #[test]
    fn starwars_stat_latches_intensity_and_color() {
        // STAT (op 3), dvy = 0x01F0 → intensity = 0xF0, color = 1.
        //   hi0 = op<<5 | (dvy>>8 & 0xF) = 0x60 | 0x01 = 0x61, lo0 = 0xF0.
        let vmem = build_vmem(&[0x61, 0xF0, 0x20, 0x00]); // STAT, HALT
        let mut sw = Avg::with_variant(AvgVariant::StarWars, 1024, 1024);
        sw.go();
        sw.execute(&vmem, &default_color_ram());
        assert!(sw.is_halted());
        assert_eq!(sw.intensity, 0xF0);
        assert_eq!(sw.color, 1);
    }

    #[test]
    fn starwars_vctr_draws_color111_line() {
        // STAT sets intensity=0xF0, color=1 (blue via color111); CNTR seeds the
        // beam at center; VCTR draws the first lit line; HALT stops.
        //   STAT: [0x61, 0xF0]
        //   CNTR: op 4 -> [0x80, 0x00]
        //   VCTR: word0 dvy=0x100 -> [0x01, 0x00];
        //         word1 int_latch=2, dvx=0x100 -> [0x21, 0x00]
        //   HALT: [0x20, 0x00]
        let vmem = build_vmem(&[
            0x61, 0xF0, // STAT
            0x80, 0x00, // CNTR
            0x01, 0x00, 0x21, 0x00, // VCTR
            0x20, 0x00, // HALT
        ]);
        let mut sw = Avg::with_variant(AvgVariant::StarWars, 1024, 1024);
        sw.go();
        sw.execute(&vmem, &default_color_ram());
        assert!(sw.is_halted());

        let list = sw.take_display_list();
        let drawn = list
            .iter()
            .find(|l| l.intensity != 0)
            .expect("a lit line from the VCTR");
        // color111(1): r = bit2 = 0, g = bit1 = 0, b = bit0 = 0xFF.
        assert_eq!((drawn.r, drawn.g, drawn.b), (0x00, 0x00, 0xFF));
    }

    #[test]
    fn starwars_color111_selects_rgb_channels() {
        // color = 4 -> color111 red (bit2 set). Verify via a lit VCTR.
        //   STAT: dvy = 0x04F0 -> hi0 = 0x60 | 0x04 = 0x64, lo0 = 0xF0.
        let vmem = build_vmem(&[
            0x64, 0xF0, // STAT: intensity=0xF0, color=4
            0x80, 0x00, // CNTR
            0x01, 0x00, 0x21, 0x00, // VCTR
            0x20, 0x00, // HALT
        ]);
        let mut sw = Avg::with_variant(AvgVariant::StarWars, 1024, 1024);
        sw.go();
        sw.execute(&vmem, &default_color_ram());
        let list = sw.take_display_list();
        let drawn = list.iter().find(|l| l.intensity != 0).expect("a lit line");
        assert_eq!((drawn.r, drawn.g, drawn.b), (0xFF, 0x00, 0x00));
    }
}
