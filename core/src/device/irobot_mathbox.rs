//! Atari I, Robot mathbox — a microcoded AM2901 bit-slice coprocessor.
//!
//! Dave Sherman's custom 3D engine: four 4-bit AM2901 ALUs forming a 16-bit
//! datapath, sequenced by a 1K×54-bit microcode ROM (thirteen 256×4 PROMs). It
//! performs the 3×3 matrix transforms, light/vector shading, and perspective
//! projection, and writes the resulting point/line/polygon display list into
//! the video communication RAM for the polygon rasterizer to draw.
//!
//! This is a full microcode interpreter — it decodes the real microcode PROMs
//! into an op table and executes them, matching MAME `src/mame/atari/irobot_m.cpp`
//! (`load_oproms` + `irmb_run`).
//!
//! Note the Battlezone/Tempest mathbox ([`crate::device::mathbox`]) is built
//! from the *same* four AM2901 slices, but we emulate it fixed-function
//! (hardcoded rotate/divide/distance) rather than running its microcode, so it
//! is not a consumer of a shared 2901 model. The genuinely machine-independent
//! atom — the 2901 combinational ALU (source mux + 8 functions + carry/overflow)
//! — is isolated here as the pure [`alu`] function. The register file, Q, and
//! shift-link wiring stay fused into [`IrobotMathbox::run`] because they are
//! board-specific and because MAME keeps a 17th register bit after up-shifts
//! (see `run`) that a tidy 16-bit datapath would drop — matching MAME bit-for-bit
//! matters more than the abstraction, since it is our cross-validation oracle.
//!
//! The mathbox also owns the three memories the 6809 reaches through its paged
//! `0x2000-0x3FFF` window ([`sharedmem_r`](IrobotMathbox::sharedmem_r) /
//! [`sharedmem_w`](IrobotMathbox::sharedmem_w)): the mathbox scratch RAM, the
//! paged mathbox ROM, and the double-buffered comm RAM (the display list).

use phosphor_macros::Saveable;

// Microcode op flags (MAME `irobot_state::FL_*`).
const FL_MULT: u32 = 0x01;
const FL_SHIFT: u32 = 0x02;
const FL_MBMEMDEC: u32 = 0x04;
const FL_ADDEN: u32 = 0x08;
const FL_DPSEL: u32 = 0x10;
const FL_CARRY: u32 = 0x20;
const FL_DIV: u32 = 0x40;
const FL_MBRW: u32 = 0x80;

const MATHBOX_RAM_WORDS: usize = 0x1000; // 8 KB scratch RAM
const MATHBOX_ROM_WORDS: usize = 0x6000; // 48 KB paged ROM
const COMRAM_WORDS: usize = 0x800; // per comm-RAM bank
const NUM_OPS: usize = 1024;

/// One decoded microcode instruction.
#[derive(Clone, Copy, Default)]
struct IrmbOp {
    areg: usize,    // ALU A register index
    breg: usize,    // ALU B register index
    func: u32,      // ALU/source/dest/jump function bits
    nxtadd: usize,  // jump target op index
    cycles: u32,    // 12 MHz cycles
    flags: u32,     // FL_* bits
    ramsel: u32,    // memory select (3 = comm RAM)
    diradd: u32,    // direct address bits (pre-masked)
    latchmask: u32, // mask applied to the address latch
    diren: bool,    // direct (RAM) addressing enabled
}

/// I, Robot AM2901 microcoded mathbox.
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
#[save_after_load(clamp_stack)]
pub struct IrobotMathbox {
    /// The decoded microcode ops and the ROM they come from, both rebuilt by
    /// [`load_rom`](Self::load_rom).
    #[save_skip]
    ops: Vec<IrmbOp>, // 1024 decoded microcode ops
    #[save_skip]
    rom: Vec<u16>, // big-endian 16-bit mathbox ROM words
    /// The two word memories. Boxed fixed-size arrays rather than `Vec`s: both
    /// are allocated once at their constant size and never resize, and a fixed
    /// array is a shape the save-state derive can encode.
    #[save(id = 1)]
    ram: Box<[u16; MATHBOX_RAM_WORDS]>, // mathbox scratch RAM
    #[save(id = 2)]
    comram: Box<[[u16; COMRAM_WORDS]; 2]>, // double-buffered display-list RAM
    #[save(id = 3)]
    regs: [u32; 16], // AM2901 working registers
    #[save(id = 4)]
    latch: u16, // address latch
    /// Microcode subroutine stack, holding op indices in `0..NUM_OPS`.
    #[save(id = 5)]
    stack: [u16; 16],
    // CPU-window paging latches (driven from out0 / statwr).
    #[save(id = 6)]
    outx: u8,
    #[save(id = 7)]
    mpage: u8,
    #[save(id = 8)]
    commbank: u8,
    // Set when a run completes; the status register reports/clears it.
    #[save(id = 9)]
    running: bool,
}

impl Default for IrobotMathbox {
    fn default() -> Self {
        Self::new()
    }
}

impl IrobotMathbox {
    pub fn new() -> Self {
        Self {
            ops: vec![IrmbOp::default(); NUM_OPS],
            rom: vec![0; MATHBOX_ROM_WORDS],
            ram: Box::new([0; MATHBOX_RAM_WORDS]),
            comram: Box::new([[0; COMRAM_WORDS]; 2]),
            regs: [0; 16],
            latch: 0,
            stack: [0; 16],
            outx: 0,
            mpage: 0,
            commbank: 0,
            running: false,
        }
    }

    /// Load the assembled big-endian mathbox ROM (`MATHBOX_ROM_WORDS` words) and
    /// decode the microcode PROMs (the bytes *after* the 32-byte text-color PROM,
    /// i.e. `proms[0x20..]`).
    pub fn load(&mut self, microcode: &[u8], rom: &[u16]) {
        let n = rom.len().min(self.rom.len());
        self.rom[..n].copy_from_slice(&rom[..n]);
        self.decode_microcode(microcode);
    }

    /// Decode the 1024-entry op table from the microcode PROMs
    /// (`irobot_state::load_oproms`). Each op's fields are scattered across 13
    /// 256×4 PROM planes at 0x400-byte strides.
    // `0x0000 + i` is kept for visual alignment with the other plane offsets.
    #[allow(clippy::identity_op)]
    fn decode_microcode(&mut self, mb: &[u8]) {
        let g = |off: usize| -> u32 { *mb.get(off).unwrap_or(&0) as u32 };
        for i in 0..NUM_OPS {
            let mut func = (g(0x0800 + i) & 0x0f) << 5;
            func |= (g(0x0c00 + i) & 0x0f) << 1;
            func |= (g(0x1000 + i) & 0x08) >> 3;
            let time = g(0x1000 + i) & 0x03;
            let mut flags = (g(0x1000 + i) & 0x04) >> 2;
            let mut nxtadd = (g(0x1400 + i) & 0x0c) >> 2;
            let mut diradd = g(0x1400 + i) & 0x03;
            nxtadd |= (g(0x1800 + i) & 0x0f) << 6;
            nxtadd |= (g(0x1c00 + i) & 0x0f) << 2;
            diradd |= (g(0x2000 + i) & 0x0f) << 2;
            func |= (g(0x2400 + i) & 0x0e) << 9;
            flags |= (g(0x2400 + i) & 0x01) << 1;
            flags |= (g(0x2800 + i) & 0x0f) << 2;
            flags |= (g(0x2c00 + i) & 0x01) << 6;
            flags |= (g(0x2c00 + i) & 0x08) << 4;
            let ramsel = (g(0x2c00 + i) & 0x06) >> 1;
            diradd |= (g(0x3000 + i) & 0x03) << 6;

            if flags & FL_SHIFT != 0 {
                func |= 0x200;
            }

            let cycles = if time == 3 { 2 } else { 3 + time };

            // Precompute the hardwired address bits and the latch mask.
            let (mut dirmask, mut latchmask) = if ramsel == 0 {
                (0x00fc, 0x3000)
            } else {
                (0x0000, 0x3ffc)
            };
            if ramsel & 2 != 0 {
                latchmask |= 0x0003;
            } else {
                dirmask |= 0x0003;
            }

            // A register comes from plane 0, B from plane 0x400.
            self.ops[i] = IrmbOp {
                areg: (g(0x0000 + i) & 0x0f) as usize,
                breg: (g(0x0400 + i) & 0x0f) as usize,
                func,
                nxtadd: nxtadd as usize,
                cycles,
                flags,
                ramsel,
                diradd: diradd & dirmask,
                latchmask,
                diren: ramsel == 0,
            };
        }
    }

    /// Reset control state (registers and memories are RAM-like and preserved,
    /// matching MAME `machine_reset`).
    pub fn reset(&mut self) {
        self.latch = 0;
        self.stack = [0; 16];
        self.running = false;
    }

    pub fn set_outx(&mut self, outx: u8) {
        self.outx = outx;
    }
    pub fn set_mpage(&mut self, mpage: u8) {
        self.mpage = mpage;
    }
    pub fn set_commbank(&mut self, commbank: u8) {
        self.commbank = commbank & 1;
    }

    /// True while a started run is reported as not-yet-complete.
    pub fn running(&self) -> bool {
        self.running
    }
    /// Clear the running flag (the status register clears it on read).
    pub fn clear_running(&mut self) {
        self.running = false;
    }

    /// Bring every stack entry into `0..NUM_OPS` after a load.
    ///
    /// The entries are op indices, and `run` uses one directly as the next op
    /// after an RTS. The mask is here because a save is an input; the hand
    /// written impl applied the same one on the way in.
    fn clamp_stack(&mut self) {
        for slot in self.stack.iter_mut() {
            *slot &= (NUM_OPS - 1) as u16;
        }
    }

    /// Read the comm-RAM display list (consumed by the polygon rasterizer).
    pub fn comram(&self, bank: usize) -> &[u16] {
        &self.comram[bank & 1]
    }

    // -- Big-endian byte views over the 16-bit word memories -----------------

    fn ram_byte(&self, addr: usize) -> u8 {
        let w = self.ram[(addr >> 1) & (MATHBOX_RAM_WORDS - 1)];
        if addr & 1 == 0 {
            (w >> 8) as u8
        } else {
            w as u8
        }
    }
    fn ram_byte_w(&mut self, addr: usize, data: u8) {
        let w = &mut self.ram[(addr >> 1) & (MATHBOX_RAM_WORDS - 1)];
        if addr & 1 == 0 {
            *w = (*w & 0x00ff) | ((data as u16) << 8);
        } else {
            *w = (*w & 0xff00) | data as u16;
        }
    }
    fn rom_byte(&self, addr: usize) -> u8 {
        let w = self.rom.get(addr >> 1).copied().unwrap_or(0);
        if addr & 1 == 0 {
            (w >> 8) as u8
        } else {
            w as u8
        }
    }
    fn comram_byte(&self, bank: usize, addr: usize) -> u8 {
        let w = self.comram[bank & 1][(addr >> 1) & (COMRAM_WORDS - 1)];
        if addr & 1 == 0 {
            (w >> 8) as u8
        } else {
            w as u8
        }
    }
    fn comram_byte_w(&mut self, bank: usize, addr: usize, data: u8) {
        let w = &mut self.comram[bank & 1][(addr >> 1) & (COMRAM_WORDS - 1)];
        if addr & 1 == 0 {
            *w = (*w & 0x00ff) | ((data as u16) << 8);
        } else {
            *w = (*w & 0xff00) | data as u16;
        }
    }

    /// 6809 read through the paged `0x2000-0x3FFF` window (`sharedmem_r`).
    /// `offset` is the address within the window (0..0x2000). The page is
    /// selected by `outx` (mathbox ROM low/high, comm RAM, or scratch RAM).
    pub fn sharedmem_r(&self, offset: u16) -> u8 {
        let off = offset as usize & 0x1fff;
        match self.outx {
            0 => self.rom_byte(((self.mpage as usize & 1) << 13) + off),
            1 => self.rom_byte(0x4000 + ((self.mpage as usize & 3) << 13) + off),
            2 => self.comram_byte(self.commbank as usize, off & 0xfff),
            3 => self.ram_byte(off),
            _ => 0xff,
        }
    }

    /// 6809 write through the paged window (`sharedmem_w`). Only comm RAM and
    /// scratch RAM are writable; the ROM pages ignore writes.
    pub fn sharedmem_w(&mut self, offset: u16, data: u8) {
        let off = offset as usize & 0x1fff;
        match self.outx {
            2 => self.comram_byte_w(self.commbank as usize, off & 0xfff, data),
            3 => self.ram_byte_w(off, data),
            _ => {}
        }
    }

    // -- Microcode memory access (irmb_din / irmb_dout) ----------------------

    fn irmb_din(&self, op: usize) -> u32 {
        let o = &self.ops[op];
        if o.flags & FL_MBMEMDEC == 0 && o.flags & FL_MBRW != 0 {
            let ad = o.diradd | (self.latch as u32 & o.latchmask);
            if o.diren || self.latch & 0x6000 == 0 {
                self.ram[(ad & 0xfff) as usize] as u32
            } else if self.latch & 0x4000 != 0 {
                self.rom[((ad + 0x2000) as usize).min(MATHBOX_ROM_WORDS - 1)] as u32
            } else {
                self.rom[(ad & 0x1fff) as usize] as u32
            }
        } else {
            0
        }
    }

    fn irmb_dout(&mut self, op: usize, d: u32) {
        let (ramsel, mbmemdec, diren, diradd, latchmask) = {
            let o = &self.ops[op];
            (
                o.ramsel,
                o.flags & FL_MBMEMDEC,
                o.diren,
                o.diradd,
                o.latchmask,
            )
        };
        // Write to the *other* comm-RAM bank (the display list being built).
        if ramsel == 3 {
            let bank = (self.commbank ^ 1) as usize;
            self.comram[bank][(self.latch & 0x7ff) as usize] = d as u16;
        }
        if mbmemdec == 0 {
            let ad = diradd | (self.latch as u32 & latchmask);
            if diren || self.latch & 0x6000 == 0 {
                self.ram[(ad & 0xfff) as usize] = d as u16;
            }
        }
    }

    /// Run the mathbox microcode to completion. Returns the accumulated 12 MHz
    /// cycle count (used to schedule the completion FIRQ). Sets the running flag.
    pub fn run(&mut self) -> u32 {
        let mut icount: u32 = 0;
        let mut prev = 0usize;
        let mut cur = 0usize;
        let mut q: u32 = 0;
        let mut nflag: u32 = 0;
        let mut cflag: u32 = 0;
        let mut sp = 0usize;

        // Terminate when the last-executed op asserts both DPSEL and CARRY. A
        // generous iteration cap guards against malformed/undecoded microcode
        // (e.g. an all-zero op table) hanging the emulator; real runs are at
        // most a few thousand instructions.
        let mut guard = 0u32;
        while self.ops[prev].flags & (FL_DPSEL | FL_CARRY) != (FL_DPSEL | FL_CARRY) {
            guard += 1;
            if guard > 1_000_000 {
                break;
            }
            let cur_op = self.ops[cur];
            let prev_flags = self.ops[prev].flags;
            icount += cur_op.cycles;

            // Modify the raw function code for the MULT and DIV special cases.
            let mut fu = cur_op.func;
            if prev_flags & FL_MULT == 0 || q & 1 != 0 {
                fu ^= 0x02;
            } else {
                fu |= 0x02;
            }
            if prev_flags & FL_DIV != 0 || nflag != 0 {
                fu ^= 0x08;
            } else {
                fu |= 0x08;
            }

            // Carry-in select (COMPUTE_CI).
            let ci = if cur_op.flags & FL_DPSEL != 0 {
                cflag
            } else {
                let mut ci = 0;
                if cur_op.flags & FL_CARRY != 0 {
                    ci = 1;
                }
                if prev_flags & FL_DIV == 0 && nflag == 0 {
                    ci = 1;
                }
                ci
            };

            // Resolve the ALU source operands (low 3 bits of `fu`).
            let areg = self.regs[cur_op.areg];
            let breg = self.regs[cur_op.breg];
            let source = fu & 0x07;
            let din = if source >= 5 { self.irmb_din(cur) } else { 0 };
            let (r, s) = match source {
                0 => (areg, q),
                1 => (areg, breg),
                2 => (0, q),
                3 => (0, breg),
                4 => (0, areg),
                5 => (din, areg),
                6 => (din, q),
                _ => (din, 0),
            };
            let (result, cf, vflag) = alu((fu >> 3) & 0x07, r, s, ci);
            cflag = cf;

            let zresult = result & 0xffff;
            nflag = zresult >> 15;

            prev = cur; // prevop = curop

            // Destination (high bits of `fu`). Each arm performs the register /
            // Q side effects and yields the Y bus value.
            let sel = fu >> 6;
            let dest_code = sel & 0x0f;
            let dnum = dest_code & 0x07;
            let is_shift = dest_code >= 0x0c;
            let carry_fill = (cur_op.flags & FL_CARRY) << 10; // 0x8000 when set
            let y = match dnum {
                0 => {
                    q = zresult;
                    zresult
                }
                1 => zresult,
                2 => {
                    self.regs[cur_op.breg] = zresult;
                    areg
                }
                3 => {
                    self.regs[cur_op.breg] = zresult;
                    zresult
                }
                4 => {
                    if is_shift {
                        self.regs[cur_op.breg] = (zresult >> 1) | ((nflag ^ vflag) << 15);
                        q = (q >> 1) | ((zresult & 0x01) << 15);
                    } else {
                        self.regs[cur_op.breg] = (zresult >> 1) | carry_fill;
                        q = (q >> 1) | carry_fill;
                    }
                    zresult
                }
                5 => {
                    if is_shift {
                        self.regs[cur_op.breg] = (zresult >> 1) | ((nflag ^ vflag) << 15);
                    } else {
                        self.regs[cur_op.breg] = (zresult >> 1) | carry_fill;
                    }
                    zresult
                }
                6 => {
                    if is_shift {
                        self.regs[cur_op.breg] = (zresult << 1) | ((q & 0x8000) >> 15);
                        q = (q << 1) & 0xffff;
                    } else {
                        self.regs[cur_op.breg] = zresult << 1;
                        q = ((q << 1) & 0xffff) | (nflag ^ 1);
                    }
                    zresult
                }
                _ => {
                    if is_shift {
                        self.regs[cur_op.breg] = (zresult << 1) | ((q & 0x8000) >> 15);
                    } else {
                        self.regs[cur_op.breg] = zresult << 1;
                    }
                    zresult
                }
            };

            // Jump type (bits 4-6 of `sel`) selects the next op.
            cur = match (sel >> 4) & 0x07 {
                0 => cur + 1,
                1 => {
                    if cflag != 0 {
                        cur_op.nxtadd
                    } else {
                        cur + 1
                    }
                }
                2 => {
                    if zresult == 0 {
                        cur_op.nxtadd
                    } else {
                        cur + 1
                    }
                }
                3 => {
                    if nflag == 0 {
                        cur_op.nxtadd
                    } else {
                        cur + 1
                    }
                }
                4 => {
                    if nflag != 0 {
                        cur_op.nxtadd
                    } else {
                        cur + 1
                    }
                }
                5 => cur_op.nxtadd,
                6 => {
                    // JSR
                    self.stack[sp] = (cur + 1) as u16;
                    sp = (sp + 1) & 15;
                    cur_op.nxtadd
                }
                _ => {
                    // RTS
                    sp = (sp.wrapping_sub(1)) & 15;
                    self.stack[sp] as usize
                }
            };
            cur &= NUM_OPS - 1;

            // Write-back and address-latch update use the just-executed op.
            if self.ops[prev].flags & FL_MBRW == 0 {
                self.irmb_dout(prev, y);
            }
            if self.ops[prev].flags & FL_ADDEN == 0 {
                self.latch = if self.ops[prev].flags & FL_MBRW != 0 {
                    self.irmb_din(prev) as u16
                } else {
                    y as u16
                };
            }
        }

        self.running = true;
        icount
    }
}

/// AM2901 ALU operation. `op` is the 3-bit operation select (ADD / SUBR / SUB /
/// OR / AND / IAND / XOR / IXOR); `r`/`s` are the resolved operands and `ci` the
/// carry-in. Returns `(result, carry_out, overflow)`. Arithmetic is done in
/// `u32` exactly as MAME's `ADD`/`SUB`/`SUBR` macros to preserve carry/overflow.
fn alu(op: u32, r: u32, s: u32, ci: u32) -> (u32, u32, u32) {
    match op {
        0 => {
            // ADD
            let result = r.wrapping_add(s).wrapping_add(ci);
            let cflag = (result >> 16) & 1;
            let vflag = (((r & 0x7fff) + (s & 0x7fff) + ci) >> 15) ^ cflag;
            (result, cflag, vflag)
        }
        1 => {
            // SUBR: S - R + CI - 1
            let result = (r ^ 0xffff).wrapping_add(s).wrapping_add(ci);
            let cflag = (result >> 16) & 1;
            let vflag = (((s & 0x7fff) + ((r ^ 0xffff) & 0x7fff) + ci) >> 15) ^ cflag;
            (result, cflag, vflag)
        }
        2 => {
            // SUB: R - S + CI - 1
            let result = r.wrapping_add(s ^ 0xffff).wrapping_add(ci);
            let cflag = (result >> 16) & 1;
            let vflag = (((r & 0x7fff) + ((s ^ 0xffff) & 0x7fff) + ci) >> 15) ^ cflag;
            (result, cflag, vflag)
        }
        3 => (r | s, 0, 0),
        4 => (r & s, 0, 0),
        5 => ((r ^ 0xffff) & s, 0, 0), // IAND
        6 => (r ^ s, 0, 0),
        _ => ((r ^ s) ^ 0xffff, 0, 0), // IXOR
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::save_state::{Saveable, StateReader, StateWriter};

    #[test]
    fn alu_add_sets_carry_and_overflow() {
        // 0x8000 + 0x8000 = carry out, signed overflow (two negatives → positive).
        let (result, c, v) = alu(0, 0x8000, 0x8000, 0);
        assert_eq!(result & 0xffff, 0);
        assert_eq!(c, 1);
        assert_eq!(v, 1);
        // 0x0001 + 0x0001 = 2, no carry/overflow.
        assert_eq!(alu(0, 1, 1, 0), (2, 0, 0));
    }

    #[test]
    fn alu_sub_and_logic() {
        // SUB R - S: 5 - 3 = 2 (CI=1 completes the two's-complement subtract).
        let (result, _c, _v) = alu(2, 5, 3, 1);
        assert_eq!(result & 0xffff, 2);
        // Logic ops clear carry/overflow.
        assert_eq!(alu(3, 0xf0, 0x0f, 0), (0xff, 0, 0)); // OR
        assert_eq!(alu(4, 0xff, 0x0f, 0), (0x0f, 0, 0)); // AND
        assert_eq!(alu(6, 0xaa, 0xff, 0), (0x55, 0, 0)); // XOR
    }

    #[test]
    fn decode_extracts_op_fields() {
        // Craft a microcode plane image where op 0 has known nibbles.
        let mut mb = vec![0u8; 0x3400];
        mb[0x0000] = 0x0a; // areg
        mb[0x0400] = 0x05; // breg
        mb[0x2c00] = 0x08; // FL_MBRW (bit3 << 4 = 0x80)
        let mut box_ = IrobotMathbox::new();
        box_.decode_microcode(&mb);
        assert_eq!(box_.ops[0].areg, 0x0a);
        assert_eq!(box_.ops[0].breg, 0x05);
        assert!(box_.ops[0].flags & FL_MBRW != 0);
    }

    #[test]
    fn shared_window_pages_ram_rom_and_comram() {
        let mut mb = IrobotMathbox::new();
        // ROM word 0 = 0x1234 (hi byte 0x12 at even, lo 0x34 at odd).
        mb.rom[0] = 0x1234;
        mb.set_outx(0);
        mb.set_mpage(0);
        assert_eq!(mb.sharedmem_r(0), 0x12);
        assert_eq!(mb.sharedmem_r(1), 0x34);

        // Scratch RAM via outx 3, big-endian byte writes assemble a word.
        mb.set_outx(3);
        mb.sharedmem_w(0x10, 0xAB);
        mb.sharedmem_w(0x11, 0xCD);
        assert_eq!(mb.ram[8], 0xABCD);
        assert_eq!(mb.sharedmem_r(0x10), 0xAB);

        // Comm RAM via outx 2 targets the selected bank.
        mb.set_outx(2);
        mb.set_commbank(1);
        mb.sharedmem_w(0x20, 0xEE);
        assert_eq!(mb.comram[1][0x10], 0xEE00);
    }

    #[test]
    fn run_terminates_on_dpsel_carry_entry() {
        // If op 0 already carries the DPSEL|CARRY terminator, run executes no
        // instructions and reports completion.
        let mut mb = IrobotMathbox::new();
        mb.ops[0].flags = FL_DPSEL | FL_CARRY;
        let icount = mb.run();
        assert_eq!(icount, 0);
        assert!(mb.running());
        // The status register clears the running flag on read.
        mb.clear_running();
        assert!(!mb.running());
    }

    #[test]
    fn save_state_round_trips_dynamic_state() {
        let mut mb = IrobotMathbox::new();
        mb.ram[0x100] = 0xBEEF;
        mb.comram[0][0x20] = 0x1357;
        mb.regs[3] = 0x0042;
        mb.latch = 0x0aaa;
        mb.set_outx(2);
        mb.set_commbank(1);
        mb.running = true;

        let mut w = StateWriter::new();
        mb.save_state(&mut w);
        let bytes = w.into_vec();

        let mut mb2 = IrobotMathbox::new();
        let mut r = StateReader::new(&bytes);
        mb2.load_state(&mut r).unwrap();
        assert_eq!(mb2.ram[0x100], 0xBEEF);
        assert_eq!(mb2.comram[0][0x20], 0x1357);
        assert_eq!(mb2.regs[3], 0x0042);
        assert_eq!(mb2.latch, 0x0aaa);
        assert_eq!(mb2.outx, 2);
        assert_eq!(mb2.commbank, 1);
        assert!(mb2.running());
    }
}
