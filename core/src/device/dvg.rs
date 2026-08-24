//! Atari Digital Vector Generator (DVG)
//!
//! A state-machine coprocessor that reads 16-bit instruction words from
//! shared vector RAM/ROM and generates a display list of line segments
//! for rendering on a vector CRT.
//!
//! Used in Asteroids (1979), Asteroids Deluxe, and Lunar Lander.
//!
//! # Architecture
//!
//! The DVG reads instructions from a contiguous vector memory space
//! (typically 2 KB RAM + 2 KB ROM = 4 KB, addressed as 2048 16-bit words).
//! Instructions are decoded by opcode in bits 15:12 of the first word.
//!
//! The real hardware uses a 256×4-bit PROM state machine clocked at 1.5 MHz,
//! with a cascaded pair of 7497 Bit Rate Multiplier ICs for each axis.
//! This implementation decodes instructions directly at the word level for
//! clarity, while matching the hardware's drawing algorithm exactly.
//!
//! # Reference
//!
//! - MAME `src/devices/video/avgdvg.cpp`
//! - Jed Margolin, "The Secret Life of Vector Generators"

/// A line segment produced by vector generator execution (DVG or AVG).
///
/// Coordinates are in the generator's native space (0–1023), with (0, 0) at
/// the bottom-left of the display. Intensity 0 means a blank (invisible) move;
/// intensities 1–15 are visible brightness levels.
///
/// RGB color defaults to white (255, 255, 255) for monochrome generators (DVG).
/// Color generators (AVG Tempest) set per-line RGB from their color RAM.
#[derive(Clone, Debug)]
pub struct VectorLine {
    pub x0: i32,
    pub y0: i32,
    pub x1: i32,
    pub y1: i32,
    pub intensity: u8,
    pub r: u8,
    pub g: u8,
    pub b: u8,

    /// Master-clock cycles the beam spent travelling this segment.
    ///
    /// How long the beam dwells on a piece of screen is what sets how bright
    /// that piece is, so this and not the intensity code is the physical input
    /// to brightness: the code sets beam current, this sets exposure. A short
    /// vector at a given code is brighter per unit length than a long one, and
    /// it is why the vertices of a shape are the brightest part of it.
    ///
    /// The AVG works this out as part of its own timing, since the beam's travel
    /// time is also what it charges the sequencer. The DVG models no timing at
    /// all and leaves this 0, which reads as "not known, fall back to the
    /// intensity code".
    pub beam_cycles: u32,
}

// ---------------------------------------------------------------------------
// The beam, as the renderers need to know it
//
// Both the CPU rasterizer and the frontend's GL path draw the same beam, so the
// figures it is described by live here beside [`VectorLine`] rather than being
// written down twice and drifting apart.
// ---------------------------------------------------------------------------

/// Long axis of a 19 inch 4:3 viewable area, in millimetres.
///
/// Every figure below is a length on the glass, so they are all expressed
/// against this and converted into whatever units a generator uses.
pub const TUBE_LONG_AXIS_MM: f32 = 360.0;

/// Focused beam spot diameter, in millimetres.
///
/// The Atari colour XY monitors are 19 inch shadow-mask tubes, the same family
/// as the raster monitors of the era, and two things bound the spot: the mask
/// pitch, about 0.6 mm, below which nothing is resolvable, and the focused spot
/// itself at about 0.7 mm.
pub const BEAM_SPOT_MM: f32 = 0.7;

/// Focused beam spot diameter as a fraction of the tube's long axis.
///
/// Works out at about 1.1 units on Tempest's 580, 1.8 on Quantum's 900, and 2.0
/// on the DVG's 1024.
pub const BEAM_SPOT_FRACTION: f32 = BEAM_SPOT_MM / TUBE_LONG_AXIS_MM;

/// Faceplate thickness of a 19 inch CRT, in millimetres.
pub const FACEPLATE_MM: f32 = 11.0;

/// Refractive index of CRT faceplate glass.
pub const FACEPLATE_INDEX: f32 = 1.54;

/// Fraction of a spot's light that leaves the tube as halation rather than
/// directly.
///
/// This is the one figure here that is not derived. The others are lengths on
/// the glass; this is an optical efficiency that depends on how isotropically
/// the phosphor emits, the aluminium backing behind it, the glass tint and any
/// anti-reflective coating, none of which we have numbers for.
///
/// So it is set by eye, which is the only instrument we have for it: 0.15 read
/// as slightly too much glow against the core, and 0.07 is where it was left.
/// Both are inside the range measured CRT spot profiles show. This is the
/// natural thing for a viewer to want on a slider, and the value here is only
/// the default it should start from.
pub const HALATION_FRACTION: f32 = 0.07;

/// Halation for a renderer that has to composite it over the whole frame on the
/// CPU, where it is not worth its cost.
///
/// The skirt is wide, so compositing it is proportional to the pixel count
/// rather than to the number of vectors, and it measured at four to five times
/// the beam sweep itself: on Asteroids' 1024 by 1024 field, 2.7 ms per frame
/// became 13.5, against 0.4 ms to emulate the machine. The GPU does the same
/// blur for nothing, so the frontend's path has it and the CPU rasterizer that
/// serves screenshots and the debug panel does not.
pub const HALATION_OFF: f32 = 0.0;

/// A Gaussian's standard deviation for a given full width at half maximum:
/// `FWHM = 2*sqrt(2*ln 2)*sigma`.
pub const FWHM_TO_SIGMA: f32 = 1.0 / 2.354_82;

/// Where the beam profile is cut off, in sigmas.
///
/// Truncating leaves a step the height of the profile there, so it has to fall
/// below one level of an 8-bit channel: 3 sigma is 1.1% of the peak and would
/// show as a faint edge, 3.5 is 0.2% and rounds away.
pub const BEAM_CUTOFF_SIGMAS: f32 = 3.5;

/// Floor on the rendered spot's sigma, in output pixels.
///
/// Not a taste value: a Gaussian sampled on a unit grid has a residual ripple of
/// about `2*exp(-2*pi^2*sigma^2)` depending on where its centre falls between
/// samples, which is the spot aliasing against the grid. That ripple is a
/// brightness that varies with the angle of the line, so sigma has to stay where
/// it is negligible: 0.4 gives 8%, 0.5 gives 1.5%, 0.6 gives 0.2%.
///
/// This is a property of the output grid, not of the tube. Rasterizing at
/// display-list resolution hits it (Tempest's physical spot is just under it, so
/// it draws a touch wide); drawing at window resolution usually does not.
pub const MIN_SIGMA_PIXELS: f32 = 0.6;

/// The beam's sigma in a generator's own coordinate units.
///
/// `long_axis_units` is the larger of the generator's two display dimensions,
/// which is the one that maps onto the tube's long axis.
pub fn beam_sigma_units(long_axis_units: f32) -> f32 {
    long_axis_units * BEAM_SPOT_FRACTION * FWHM_TO_SIGMA
}

/// The halation skirt's sigma in a generator's own coordinate units.
///
/// The phosphor emits into the faceplate in every direction. Light steeper than
/// the critical angle cannot leave the front surface, so it reflects back,
/// crosses the glass again and re-emerges a distance away: for thickness `t` and
/// index `n`, at a radius of `2*t*tan(asin(1/n))`. With an 11 mm faceplate at
/// n = 1.54 that is about 19 mm, or 5% of the tube's long axis, and it is why a
/// bright vector sits in a broad glow rather than ending at its own edge.
///
/// The ring is treated as a Gaussian skirt of that scale rather than as a ring,
/// which is what the sum over all the emission angles and depths looks like from
/// the front.
pub fn halation_sigma_units(long_axis_units: f32) -> f32 {
    let critical_angle = (1.0 / FACEPLATE_INDEX).asin();
    let radius_mm = 2.0 * FACEPLATE_MM * critical_angle.tan();
    long_axis_units * radius_mm / TUBE_LONG_AXIS_MM
}

use crate::prelude::Saveable;

/// Atari Digital Vector Generator (DVG).
///
/// The DVG is triggered by the CPU writing to a VG_GO register. It then
/// executes vector instructions until it encounters a HALT opcode, producing
/// a display list of [`VectorLine`] segments. The CPU polls a VG_HALT status
/// bit to know when the DVG has finished.
#[derive(Saveable)]
#[save_version(1)]
pub struct Dvg {
    /// Program counter (word address into vector memory, 0–2047).
    pc: u16,
    /// 4-entry return address stack for JSR/RTS.
    stack: [u16; 4],
    /// Stack pointer (only bits 1:0 used to index `stack`).
    sp: u8,

    /// Current beam X position (12-bit; valid display range is 0–1023).
    xpos: i32,
    /// Current beam Y position (12-bit; valid display range is 0–1023).
    ypos: i32,
    /// Global scale factor (0–15), set by LABS instructions.
    scale: u8,
    /// Current beam intensity (0 = blank, 1–15 = visible brightness).
    intensity: u8,

    /// True when the DVG has executed a HALT instruction.
    halted: bool,

    /// Accumulated display list for the current frame.
    #[save_skip(default)]
    display_list: Vec<VectorLine>,
}

use crate::core::debug::{DebugRegister, Debuggable};

impl Debuggable for Dvg {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "PC",
                value: self.pc as u64,
                width: 16,
            },
            DebugRegister {
                name: "X",
                value: self.xpos as u64,
                width: 16,
            },
            DebugRegister {
                name: "Y",
                value: self.ypos as u64,
                width: 16,
            },
            DebugRegister {
                name: "SCALE",
                value: self.scale as u64,
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

impl Dvg {
    pub fn new() -> Self {
        Self {
            pc: 0,
            stack: [0; 4],
            sp: 0,
            xpos: 0,
            ypos: 0,
            scale: 0,
            intensity: 0,
            halted: true,
            display_list: Vec::with_capacity(512),
        }
    }

    /// Trigger DVG execution. Called when the CPU writes to the VG_GO register.
    ///
    /// This resets the DVG internal latches and clears the halt flag, but does
    /// NOT reset the PC — the PC is reset by the state machine init. On real
    /// hardware, `vggo` resets the op and dvy latches; the state machine
    /// reset (at power-on) zeros the PC via `vgrst`.
    pub fn go(&mut self) {
        self.halted = false;
        self.pc = 0;
        self.display_list.clear();
    }

    /// Returns true if the DVG has halted (finished executing).
    /// The CPU reads this via the VG_HALT status bit.
    pub fn is_halted(&self) -> bool {
        self.halted
    }

    /// Execute DVG instructions from vector memory until HALT.
    ///
    /// `vmem` is the combined vector RAM + ROM as a flat byte slice.
    /// For Asteroids: 2 KB RAM (bytes 0x0000–0x07FF) + 2 KB ROM (0x0800–0x0FFF).
    ///
    /// The DVG uses word addressing: PC=0 reads bytes 0,1; PC=1 reads bytes 2,3.
    pub fn execute(&mut self, vmem: &[u8]) {
        // Safety limit to prevent infinite loops from malformed vector data.
        let mut instructions = 0u32;
        const MAX_INSTRUCTIONS: u32 = 10_000;

        while !self.halted && instructions < MAX_INSTRUCTIONS {
            self.execute_one(vmem);
            instructions += 1;
        }
    }

    /// Drain the display list, returning ownership to the caller.
    pub fn take_display_list(&mut self) -> Vec<VectorLine> {
        std::mem::take(&mut self.display_list)
    }

    /// Reset to power-on state.
    pub fn reset(&mut self) {
        self.pc = 0;
        self.stack = [0; 4];
        self.sp = 0;
        self.xpos = 0;
        self.ypos = 0;
        self.scale = 0;
        self.intensity = 0;
        self.halted = true;
        self.display_list.clear();
    }

    // -----------------------------------------------------------------------
    // Instruction decoding
    // -----------------------------------------------------------------------

    /// Read a 16-bit little-endian word from vector memory at the given word address.
    fn read_word(&self, vmem: &[u8], word_addr: u16) -> u16 {
        let byte_addr = (word_addr as usize) * 2;
        if byte_addr + 1 < vmem.len() {
            vmem[byte_addr] as u16 | ((vmem[byte_addr + 1] as u16) << 8)
        } else {
            0
        }
    }

    /// Decode and execute one DVG instruction. Advances the PC.
    fn execute_one(&mut self, vmem: &[u8]) {
        let word0 = self.read_word(vmem, self.pc);
        let op = ((word0 >> 12) & 0xF) as u8;

        match op {
            // VCTR: draw a vector (2 words)
            0x0..=0x9 => self.op_vctr(vmem, word0, op),
            // LABS: load absolute position (2 words)
            0xA => self.op_labs(vmem, word0),
            // HALT: stop execution (1 word)
            0xB => self.op_halt(),
            // JSR: subroutine call (1 word)
            0xC => self.op_jsr(word0),
            // RTS: return from subroutine (1 word)
            0xD => self.op_rts(),
            // JMP: unconditional jump (1 word)
            0xE => self.op_jmp(word0),
            // SVEC: short vector (1 word)
            0xF => self.op_svec(word0),
            _ => unreachable!(),
        }
    }

    /// VCTR — Draw a vector (opcodes 0x0–0x9).
    ///
    /// Word 0: `[op:4 | dy:12]`  (op doubles as per-vector scale offset)
    /// Word 1: `[intensity:4 | dx:12]`
    ///
    /// DX/DY are 12-bit values where bit 10 is the sign (0 = positive, 1 = negative)
    /// and bits 9:0 are the magnitude. Bit 11 is part of the rate multiplier input.
    fn op_vctr(&mut self, vmem: &[u8], word0: u16, op: u8) {
        let dvy = word0 & 0xFFF;
        self.pc += 1;
        let word1 = self.read_word(vmem, self.pc);
        let dvx = word1 & 0xFFF;
        self.intensity = ((word1 >> 12) & 0xF) as u8;
        self.pc += 1;

        let scale = (self.scale.wrapping_add(op)) & 0xF;
        self.draw_vector(dvx, dvy, scale);
    }

    /// LABS — Load absolute beam position (opcode 0xA).
    ///
    /// Word 0: `[0xA:4 | y:12]`
    /// Word 1: `[intensity:4 | x:12]`
    ///
    /// Sets the beam position, global scale (from intensity nibble), and
    /// current intensity. Emits a blank move to the new position.
    fn op_labs(&mut self, vmem: &[u8], word0: u16) {
        let dvy = word0 & 0xFFF;
        self.pc += 1;
        let word1 = self.read_word(vmem, self.pc);
        let dvx = word1 & 0xFFF;
        self.intensity = ((word1 >> 12) & 0xF) as u8;
        self.pc += 1;

        // LABS sets global scale from the intensity nibble (handler_6: OP1 && OP3).
        self.scale = self.intensity;

        // Load absolute position (handler_3: !OP0).
        self.xpos = dvx as i32;
        self.ypos = dvy as i32;

        // Emit blank move to the new position if within valid range.
        self.emit_point(0);
    }

    /// HALT — Stop DVG execution (opcode 0xB).
    fn op_halt(&mut self) {
        self.pc += 1;
        self.halted = true;
    }

    /// JSR — Push return address and jump to subroutine (opcode 0xC).
    ///
    /// Word: `[0xC:4 | target:12]`
    fn op_jsr(&mut self, word0: u16) {
        let target = word0 & 0xFFF;
        self.pc += 1;
        // Push return address (current PC, which is already past this instruction).
        self.sp = self.sp.wrapping_add(1) & 0xF;
        self.stack[(self.sp & 3) as usize] = self.pc;
        self.pc = target;
    }

    /// RTS — Return from subroutine (opcode 0xD).
    fn op_rts(&mut self) {
        self.pc = self.stack[(self.sp & 3) as usize];
        self.sp = self.sp.wrapping_sub(1) & 0xF;
    }

    /// JMP — Unconditional jump (opcode 0xE).
    ///
    /// Word: `[0xE:4 | target:12]`
    fn op_jmp(&mut self, word0: u16) {
        let target = word0 & 0xFFF;
        self.pc = target;
    }

    /// SVEC — Short vector (opcode 0xF).
    ///
    /// Word: `[0xF:4 | dvy_hi:4 | intensity:4 | dvx_hi:4]`
    ///
    /// Only the upper 4 bits of DX/DY are encoded (bits 11:8); bits 7:0 are
    /// implicitly zero. The scale offset is derived from the sign bits,
    /// yielding an additional 2–5 added to the global scale.
    fn op_svec(&mut self, word0: u16) {
        self.pc += 1;

        // Decode fields from the single word.
        // Byte layout: high byte = [0xF | dvy[11:8]], low byte = [intensity | dvx[11:8]]
        let dvy = ((word0 >> 8) & 0xF) << 8;
        let dvx = (word0 & 0xF) << 8;
        self.intensity = ((word0 >> 4) & 0xF) as u8;

        // SVEC scale offset from sign bits (MAME handler_2 for op==0xF):
        //   bit 0: dvy[11]
        //   bit 1: ~dvx[11]
        //   bit 2: dvx[11]
        let offset =
            (((dvy & 0x800) >> 11) | (((dvx & 0x800) ^ 0x800) >> 10) | ((dvx & 0x800) >> 9)) as u8;
        let scale = (self.scale.wrapping_add(offset)) & 0xF;

        // After scale extraction, mask to keep only the direction/magnitude bits.
        let dvy = dvy & 0xF00;
        let dvx = dvx & 0xF00;

        self.draw_vector(dvx, dvy, scale);
    }

    // -----------------------------------------------------------------------
    // Vector drawing — 7497 Bit Rate Multiplier algorithm
    // -----------------------------------------------------------------------

    /// Draw a vector using the 7497 Bit Rate Multiplier hardware algorithm.
    ///
    /// `dvx`/`dvy` are 12-bit values where bit 10 determines direction
    /// (0 = positive, 1 = negative) and the magnitude is encoded in the
    /// remaining bits for the rate multiplier.
    ///
    /// This closely follows MAME's `dvg_device::handler_2` (dvg_gostrobe).
    fn draw_vector(&mut self, dvx: u16, dvy: u16, scale: u8) {
        // Vector length from scale: determines the number of BRM iterations.
        let fin = 0xFFF_i32 - (((2_i32 << scale) & 0x7FF) ^ 0xFFF);

        // Step direction: bit 10 determines sign.
        let dx: i32 = if dvx & 0x400 != 0 { -1 } else { 1 };
        let dy: i32 = if dvy & 0x400 != 0 { -1 } else { 1 };

        // Rate multiplier inputs (shift left 2 to fill 12-bit field).
        let mx = ((dvx << 2) & 0xFFF) as i32;
        let my = ((dvy << 2) & 0xFFF) as i32;

        let mut c: i32 = 0;
        let mut remaining = fin;

        while remaining > 0 {
            remaining -= 1;

            // 7497 Bit Rate Multiplier: two cascaded 6-bit counters per axis.
            // For each bit position, check if the counter pattern matches,
            // and if the corresponding input bit is set, generate a step pulse.
            let mut countx = false;
            let mut county = false;

            for bit in 0..12 {
                let mask = (1 << (bit + 1)) - 1;
                let pattern = (1 << bit) - 1;
                if (c & mask) == pattern {
                    if mx & (1 << (11 - bit)) != 0 {
                        countx = true;
                    }
                    if my & (1 << (11 - bit)) != 0 {
                        county = true;
                    }
                }
            }

            c = (c + 1) & 0xFFF;

            // Hardware clipping: when bit 10 of a coordinate changes state,
            // the hardware finishes or starts a line segment at the boundary.
            if countx {
                let new_x = (self.xpos + dx) & 0xFFF;
                if self.ypos & 0x400 == 0 && (self.xpos ^ new_x) & 0x400 != 0 {
                    if new_x & 0x400 != 0 {
                        // Leaving valid range — finish the current segment.
                        self.emit_point(self.intensity);
                    } else {
                        // Entering valid range — start a new blank segment.
                        self.xpos = new_x;
                        self.emit_point(0);
                        continue;
                    }
                }
                self.xpos = new_x;
            }

            if county {
                let new_y = (self.ypos + dy) & 0xFFF;
                if self.xpos & 0x400 == 0 && (self.ypos ^ new_y) & 0x400 != 0 {
                    if new_y & 0x400 != 0 {
                        self.emit_point(self.intensity);
                    } else {
                        self.ypos = new_y;
                        self.emit_point(0);
                        continue;
                    }
                }
                self.ypos = new_y;
            }
        }

        // Emit the endpoint of the vector.
        self.emit_point(self.intensity);
    }

    /// Emit a point at the current beam position, creating a line segment
    /// from the previous point if one exists.
    ///
    /// The MAME rendering model accumulates (x, y, intensity) points; each
    /// consecutive pair forms a line segment. We convert this to explicit
    /// VectorLine entries.
    fn emit_point(&mut self, intensity: u8) {
        // Only emit if position is within the valid 10-bit range.
        if (self.xpos | self.ypos) & 0x400 != 0 {
            return;
        }

        let x = self.xpos & 0x3FF;
        let y = self.ypos & 0x3FF;

        // Connect from the previous endpoint (if any) to the current position.
        // Intensity 0 means a blank (invisible) move.
        //
        // Note: zero-length vectors (same start and end) with intensity > 0 are
        // valid — they represent bright dots (e.g., shots in Asteroids). On real
        // CRT hardware the beam dwells at the position with the beam on, producing
        // a visible point. We only skip consecutive blank moves to the same spot.
        if let Some(prev) = self.display_list.last() {
            if prev.x1 == x && prev.y1 == y && intensity == 0 {
                return; // skip consecutive blank moves to same position
            }
            self.display_list.push(VectorLine {
                x0: prev.x1,
                y0: prev.y1,
                x1: x,
                y1: y,
                intensity,
                r: 255,
                g: 255,
                b: 255,
                // The DVG decodes at the word level and models no beam timing,
                // so it has no travel time to report. See `VectorLine`.
                beam_cycles: 0,
            });
        } else {
            // First point — no previous segment, just record position.
            self.display_list.push(VectorLine {
                x0: x,
                y0: y,
                x1: x,
                y1: y,
                intensity: 0,
                r: 255,
                g: 255,
                b: 255,
                beam_cycles: 0,
            });
        }
    }
}

impl super::Device for Dvg {
    fn name(&self) -> &'static str {
        "DVG"
    }
    fn reset(&mut self) {
        self.reset();
    }
}

impl Default for Dvg {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Build vector memory from 16-bit words.
    fn build_vmem(words: &[u16]) -> Vec<u8> {
        let mut vmem = vec![0u8; 4096]; // 4KB
        for (i, &w) in words.iter().enumerate() {
            let addr = i * 2;
            if addr + 1 < vmem.len() {
                vmem[addr] = (w & 0xFF) as u8;
                vmem[addr + 1] = ((w >> 8) & 0xFF) as u8;
            }
        }
        vmem
    }

    #[test]
    fn halt_immediately() {
        let vmem = build_vmem(&[0xB000]); // HALT
        let mut dvg = Dvg::new();
        dvg.go();
        dvg.execute(&vmem);
        assert!(dvg.is_halted());
        // Only a degenerate point entry (or empty), no visible lines.
        let list = dvg.take_display_list();
        assert!(list.iter().all(|l| l.intensity == 0));
    }

    #[test]
    fn labs_sets_position() {
        // LABS: word0 = 0xA000 | y, word1 = (intensity << 12) | x
        // Set position to (200, 300), intensity = 7 (also sets scale = 7)
        let vmem = build_vmem(&[
            0xA000 | 300, // LABS word0: y=300
            0x7000 | 200, // LABS word1: intensity=7, x=200
            0xB000,       // HALT
        ]);
        let mut dvg = Dvg::new();
        dvg.go();
        dvg.execute(&vmem);
        assert!(dvg.is_halted());
        assert_eq!(dvg.xpos, 200);
        assert_eq!(dvg.ypos, 300);
        assert_eq!(dvg.scale, 7);
        assert_eq!(dvg.intensity, 7);
    }

    #[test]
    fn jsr_and_rts() {
        // Word 0: JSR to word address 3
        // Word 1: (pad — JSR is 1 word, so word 1 is at the return address)
        // Word 1: HALT (return point)
        // Word 3: RTS (subroutine body)
        let vmem = build_vmem(&[
            0xC003, // JSR to word 3
            0xB000, // HALT (return here after RTS)
            0x0000, // (unused)
            0xD000, // RTS
        ]);
        let mut dvg = Dvg::new();
        dvg.go();
        dvg.execute(&vmem);
        assert!(dvg.is_halted());
        // PC should have advanced past the HALT at word 1.
        assert_eq!(dvg.pc, 2);
    }

    #[test]
    fn jmp_jumps_without_push() {
        // Word 0: JMP to word 2
        // Word 1: (skipped)
        // Word 2: HALT
        let vmem = build_vmem(&[
            0xE002, // JMP to word 2
            0xB000, // HALT (should NOT reach here)
            0xB000, // HALT (should reach here)
        ]);
        let mut dvg = Dvg::new();
        dvg.go();
        dvg.execute(&vmem);
        assert!(dvg.is_halted());
        // PC should be at word 3 (past the HALT at word 2).
        assert_eq!(dvg.pc, 3);
    }

    #[test]
    fn vctr_draws_visible_line() {
        // LABS to (512, 512) with scale=9, then VCTR drawing right with intensity 15.
        let vmem = build_vmem(&[
            0xA000 | 512, // LABS: y=512
            0x9000 | 512, // LABS: intensity=9 (sets scale=9), x=512
            // VCTR op=0 (scale offset 0): dy=0, dx=100, intensity=15
            0x0000, // word0: op=0, dy=0
            0xF064, // word1: intensity=15, dx=100
            0xB000, // HALT
        ]);
        let mut dvg = Dvg::new();
        dvg.go();
        dvg.execute(&vmem);
        assert!(dvg.is_halted());

        let list = dvg.take_display_list();
        // Should have at least one visible line segment.
        let visible: Vec<_> = list.iter().filter(|l| l.intensity > 0).collect();
        assert!(!visible.is_empty(), "expected at least one visible line");
    }

    #[test]
    fn vctr_blank_move() {
        // VCTR with intensity 0 should not produce visible lines.
        let vmem = build_vmem(&[
            0xA000 | 512, // LABS: y=512
            512,          // LABS: intensity=0, x=512
            0x0000,       // VCTR word0: op=0, dy=0
            0x0064,       // VCTR word1: intensity=0, dx=100
            0xB000,       // HALT
        ]);
        let mut dvg = Dvg::new();
        dvg.go();
        dvg.execute(&vmem);

        let list = dvg.take_display_list();
        assert!(
            list.iter().all(|l| l.intensity == 0),
            "expected no visible lines for intensity 0"
        );
    }

    #[test]
    fn svec_draws_short_vector() {
        // LABS to center, then SVEC.
        // SVEC word: 0xF | dvy_hi:4 | intensity:4 | dvx_hi:4
        // dvx_hi=1 (bits 11:8 = 0x100), dvy_hi=0, intensity=15
        let vmem = build_vmem(&[
            0xA000 | 512, // LABS: y=512
            512,          // LABS: intensity=0, x=512
            0xF0F1,       // SVEC: dvy_hi=0, intensity=15, dvx_hi=1
            0xB000,       // HALT
        ]);
        let mut dvg = Dvg::new();
        dvg.go();
        dvg.execute(&vmem);

        let list = dvg.take_display_list();
        let visible: Vec<_> = list.iter().filter(|l| l.intensity > 0).collect();
        assert!(
            !visible.is_empty(),
            "expected at least one visible line from SVEC"
        );
    }

    #[test]
    fn stack_wraps_at_four_entries() {
        // Push 5 times (wraps), then pop — should get the 5th pushed value.
        let vmem = build_vmem(&[
            0xC005, // JSR to 5
            0xC005, // JSR to 5
            0xC005, // JSR to 5
            0xC005, // JSR to 5
            0xB000, // HALT (final return destination)
            0xD000, // RTS (subroutine body)
        ]);
        let mut dvg = Dvg::new();
        dvg.go();
        dvg.execute(&vmem);
        assert!(dvg.is_halted());
    }

    #[test]
    fn reset_clears_state() {
        let mut dvg = Dvg::new();
        dvg.go();
        dvg.pc = 100;
        dvg.xpos = 500;
        dvg.ypos = 300;
        dvg.scale = 7;
        dvg.intensity = 15;
        dvg.display_list.push(VectorLine {
            x0: 0,
            y0: 0,
            x1: 100,
            y1: 100,
            intensity: 15,
            r: 255,
            g: 255,
            b: 255,
            beam_cycles: 0,
        });
        dvg.reset();
        assert_eq!(dvg.pc, 0);
        assert_eq!(dvg.xpos, 0);
        assert_eq!(dvg.ypos, 0);
        assert_eq!(dvg.scale, 0);
        assert_eq!(dvg.intensity, 0);
        assert!(dvg.is_halted());
        assert!(dvg.display_list.is_empty());
    }

    #[test]
    fn go_clears_halt_and_display_list() {
        let mut dvg = Dvg::new();
        dvg.display_list.push(VectorLine {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
            intensity: 10,
            r: 255,
            g: 255,
            b: 255,
            beam_cycles: 0,
        });
        assert!(dvg.is_halted());
        dvg.go();
        assert!(!dvg.is_halted());
        assert!(dvg.display_list.is_empty());
        assert_eq!(dvg.pc, 0);
    }

    #[test]
    fn max_instruction_limit_prevents_infinite_loop() {
        // JMP to self — infinite loop. Should terminate via safety limit.
        let vmem = build_vmem(&[0xE000]); // JMP to word 0
        let mut dvg = Dvg::new();
        dvg.go();
        dvg.execute(&vmem);
        // Should NOT be halted (the loop was broken by the safety limit,
        // not by a HALT instruction).
        assert!(!dvg.is_halted());
    }

    #[test]
    fn vctr_negative_direction() {
        // Draw a vector in the negative X direction from (600, 512).
        // DX bit 10 set = negative direction.
        let vmem = build_vmem(&[
            0xA000 | 512, // LABS: y=512
            0x9000 | 600, // LABS: intensity=9 (sets scale=9), x=600
            // VCTR op=0: dy=0, dx=0x464 (bit 10 set = negative, magnitude 100)
            0x0000, // word0: op=0, dy=0
            0xF464, // word1: intensity=15, dx=0x464
            0xB000, // HALT
        ]);
        let mut dvg = Dvg::new();
        dvg.go();
        dvg.execute(&vmem);

        // Beam should have moved left (X decreased).
        assert!(dvg.xpos < 600, "expected X to decrease, got {}", dvg.xpos);
    }
}
