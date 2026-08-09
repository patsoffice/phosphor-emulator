//! Atari Star Wars "Matrix Processor" — the 3D math coprocessor.
//!
//! Unlike the fixed-function Battlezone/Tempest [`Mathbox`](crate::device::mathbox)
//! or the microcoded AM2901 [`IrobotMathbox`](crate::device::irobot_mathbox), the
//! Star Wars box is a **PROM-sequenced multiply-accumulate engine**. Four 1K×4
//! PROMs form a 1024-step microprogram of strobe bits that gate a hardwired
//! A/B/C/ACC datapath (a serial 74LS384 subtractor-multiplier-accumulator). Each
//! step computes `ACC += (A − B) · C` and streams 16-bit words in and out of the
//! shared **Math RAM** ($5000–$5FFF, addressed as 1K big-endian words).
//!
//! The same silicon also carries two unrelated blocks the CPU reaches at
//! $4700–$4707: a hardware **restoring divider** and a 23-bit **LFSR PRNG**.
//!
//! # Registers ($4700–$4707 writes)
//!
//! - `0` mw0   — set PROM start address (`data << 2`) and **run** the processor
//! - `1` mw1   — BIC bit 8
//! - `2` mw2   — BIC bits 7:0
//! - `4` dvsrh — divisor high; latches dividend into the shift register
//! - `5` dvsrl — divisor low; **triggers** the 15-step restoring division
//! - `6` dvddh — dividend high
//! - `7` dvddl — dividend low
//!
//! Reads: $4700 = quotient high, $4701 = quotient low, $4703 = PRNG.
//! IN1 bit 7 (`MATH_RUN`) reflects [`StarWarsMath::math_run`].
//!
//! # Reference
//!
//! MAME `src/mame/atari/starwars_m.cpp` (`run_mproc`, `starwars_math_w`,
//! `starwars_div_*`, `starwars_prng_r`).

use crate::core::debug::{DebugRegister, Debuggable};
use crate::core::save_state::{SaveError, Saveable, StateReader, StateWriter};

// Matrix processor strobe bits (IP15_8 = PROM_STR), MAME `starwars_m.cpp`.
const LAC: u8 = 0x01; // load accumulator from RAM word (clears lsb)
const READ_ACC: u8 = 0x02; // store accumulator to RAM word
const M_HALT: u8 = 0x04; // stop the microprogram
const INC_BIC: u8 = 0x08; // increment the block-index counter
const CLEAR_ACC: u8 = 0x10; // zero the accumulator
const LDC: u8 = 0x20; // load C and perform ACC += (A-B)*C
const LDB: u8 = 0x40; // load B from RAM word
const LDA: u8 = 0x80; // load A from RAM word

/// Number of PROM microprogram steps (1K×4 PROMs → 1024 entries).
const PROM_STEPS: usize = 1024;
/// Shared Math RAM size in bytes ($5000–$5FFF).
const MATH_RAM_LEN: usize = 0x1000;
/// Instruction cap matching MAME's `M_STOP` runaway guard.
const M_STOP_LIMIT: u32 = 100_000;

/// Atari Star Wars Matrix Processor + divider + PRNG.
pub struct StarWarsMath {
    /// Strobe bits per step (IP15_8), decoded from the mathbox PROMs.
    prom_str: Vec<u8>,
    /// RAM-address low bits per step (IP6_0).
    prom_mas: Vec<u8>,
    /// Address-mode bit per step (IP7): 0 = BIC-relative, 1 = direct.
    prom_am: Vec<u8>,

    /// PROM microprogram address counter (top two bits page the counter).
    mpa: u16,
    /// Block index counter (9 bits), addresses Math RAM in BIC-relative mode.
    bic: u16,
    /// Datapath registers (signed 16-bit).
    a: i16,
    b: i16,
    c: i16,
    /// 32-bit accumulator (only the upper 16 bits are read/written to RAM).
    acc: i32,

    /// True while the matrix processor is "running" (IN1 bit 7).
    math_run: bool,

    /// Diagnostic only: a recorded PRNG sequence to hand out from `$4703`
    /// instead of the LFSR, and how far through it we are.
    ///
    /// This exists to make a run reproducible against a *reference emulator*
    /// whose PRNG we cannot predict — MAME returns `machine().rand()` here,
    /// a machine-wide LCG that other code also draws from, so its sequence
    /// can be recorded but not recomputed. Replaying the recording puts both
    /// emulators on identical values and restores instruction-level lockstep.
    /// Never populated during normal emulation; not saved in save states.
    prng_replay: Vec<u8>,
    prng_replay_pos: usize,
    /// Remaining CPU cycles before `math_run` clears (mptime / 8).
    busy_cycles: u32,

    // Restoring divider.
    divisor: u16,
    dividend: u16,
    dvd_shift: u16,
    quotient_shift: u16,

    /// 23-bit LFSR pseudo-random generator (taps 4 and 22, inverted feedback).
    prng: u32,
}

impl Default for StarWarsMath {
    fn default() -> Self {
        Self::new()
    }
}

impl StarWarsMath {
    pub fn new() -> Self {
        Self {
            prom_str: vec![0; PROM_STEPS],
            prom_mas: vec![0; PROM_STEPS],
            prom_am: vec![0; PROM_STEPS],
            mpa: 0,
            bic: 0,
            a: 0,
            b: 0,
            c: 0,
            acc: 0,
            math_run: false,
            prng_replay: Vec::new(),
            prng_replay_pos: 0,
            busy_cycles: 0,
            divisor: 0,
            dividend: 0,
            dvd_shift: 0,
            quotient_shift: 0,
            prng: 0,
        }
    }

    /// Decode the four 1K×4 mathbox PROMs (the `user2` region, 4 × 0x400 bytes)
    /// into the pre-split strobe/address/mode tables. Must be called once after
    /// ROM load, before the processor is used. Matches `starwars_mproc_init`.
    // `0x0000 + cnt` is kept for visual alignment with the other plane offsets.
    #[allow(clippy::identity_op)]
    pub fn load_proms(&mut self, user2: &[u8]) {
        let g = |off: usize| -> u16 { *user2.get(off).unwrap_or(&0) as u16 };
        for cnt in 0..PROM_STEPS {
            // Reassemble the 16-bit microword from the four 4-bit PROM planes.
            let val = (g(0x0c00 + cnt) & 0x000f)          // LS nibble
                | ((g(0x0800 + cnt) << 4) & 0x00f0)
                | ((g(0x0400 + cnt) << 8) & 0x0f00)
                | ((g(0x0000 + cnt) << 12) & 0xf000); // MS nibble

            self.prom_str[cnt] = ((val >> 8) & 0x00ff) as u8;
            self.prom_mas[cnt] = (val & 0x007f) as u8;
            self.prom_am[cnt] = ((val >> 7) & 0x0001) as u8;
        }
    }

    /// `MATH_RUN` flag, read by the CPU at IN1 bit 7.
    pub fn math_run(&self) -> bool {
        self.math_run
    }

    /// Quotient high byte ($4700 read).
    pub fn div_reh_r(&self) -> u8 {
        ((self.quotient_shift & 0xff00) >> 8) as u8
    }

    /// Quotient low byte ($4701 read).
    pub fn div_rel_r(&self) -> u8 {
        (self.quotient_shift & 0x00ff) as u8
    }

    /// Pseudo-random number read ($4703). Advances the 23-bit LFSR one byte's
    /// worth and returns bits 15:8 (the only bits wired to the CPU data bus).
    pub fn prng_r(&mut self) -> u8 {
        // Diagnostic replay takes precedence while it lasts; once exhausted
        // the real LFSR resumes, so a short recording degrades into normal
        // behaviour rather than repeating or stalling.
        if let Some(&v) = self.prng_replay.get(self.prng_replay_pos) {
            self.prng_replay_pos += 1;
            return v;
        }
        for _ in 0..8 {
            self.clock_prng();
        }
        ((self.prng >> 8) & 0xff) as u8
    }

    /// Install a recorded `$4703` sequence for comparison runs. See
    /// [`prng_replay`](Self::prng_replay). Returns the number of values
    /// installed; an empty slice clears any replay.
    pub fn set_prng_replay(&mut self, values: &[u8]) -> usize {
        self.prng_replay = values.to_vec();
        self.prng_replay_pos = 0;
        self.prng_replay.len()
    }

    /// How many replayed values have been consumed, so a caller can report
    /// when a recording ran out mid-run rather than silently drifting.
    pub fn prng_replay_consumed(&self) -> (usize, usize) {
        (self.prng_replay_pos, self.prng_replay.len())
    }

    /// Clock the 23-bit LFSR once. Polynomial x^23 + x^5 + 1 (taps at bits 22 and
    /// 4); the feedback bit is inverted so the register is self-starting from 0.
    fn clock_prng(&mut self) {
        let fb = (((self.prng >> 22) ^ (self.prng >> 4)) & 1) ^ 1;
        self.prng = ((self.prng << 1) | fb) & 0x7f_ffff;
    }

    /// Handle a write to the matrix/divider register file ($4700–$4707).
    ///
    /// `mathram` is the shared $5000–$5FFF RAM (`MATH_RAM_LEN` bytes); the matrix
    /// processor reads and writes it while running.
    pub fn math_w(&mut self, offset: u8, data: u8, mathram: &mut [u8]) {
        match offset & 0x07 {
            0 => {
                // mw0: set start address and run the microprogram.
                self.mpa = (data as u16) << 2;
                self.run_mproc(mathram);
            }
            1 => self.bic = (self.bic & 0x00ff) | (((data as u16) & 0x01) << 8),
            2 => self.bic = (self.bic & 0x0100) | data as u16,
            4 => {
                // dvsrh: latch divisor high, snapshot dividend, clear quotient.
                self.divisor = (self.divisor & 0x00ff) | ((data as u16) << 8);
                self.dvd_shift = self.dividend;
                self.quotient_shift = 0;
            }
            5 => {
                // dvsrl: latch divisor low and run the restoring division.
                self.divisor = (self.divisor & 0xff00) | data as u16;
                self.run_divide();
            }
            6 => self.dividend = (self.dividend & 0x00ff) | ((data as u16) << 8),
            7 => self.dividend = (self.dividend & 0xff00) | data as u16,
            _ => {}
        }
    }

    /// 15-step restoring division producing `quotient_shift`. Reproduces the
    /// schematic's exact behavior, including the "wrong" results the hardware
    /// gives when `divisor < 2*dividend` or `divisor > 0x8000`.
    fn run_divide(&mut self) {
        let comp = self.divisor ^ 0xffff;
        for _ in 1..16 {
            self.quotient_shift <<= 1;
            let trial = self.dvd_shift as i32 + comp as i32 + 1;
            if trial & 0x10000 != 0 {
                self.quotient_shift |= 1;
                self.dvd_shift = self.dvd_shift.wrapping_add(comp).wrapping_add(1) << 1;
            } else {
                self.dvd_shift <<= 1;
            }
        }
    }

    /// Run the matrix processor microprogram from the current `mpa` until a
    /// `M_HALT` strobe (or the runaway guard) stops it. Mirrors `run_mproc`.
    fn run_mproc(&mut self, mathram: &mut [u8]) {
        debug_assert_eq!(mathram.len(), MATH_RAM_LEN);

        let mut m_stop = M_STOP_LIMIT;
        let mut mptime: u32 = 0;
        self.math_run = true;

        while m_stop > 0 {
            // Each step of the matrix processor takes five clock cycles.
            mptime += 5;

            let mpa = self.mpa as usize & (PROM_STEPS - 1);
            let strobe = self.prom_str[mpa];
            let am = self.prom_am[mpa];
            let mas = self.prom_mas[mpa] as u16;

            // Construct the Math RAM word address for this step.
            let ma = if am == 0 {
                (mas & 3) | ((self.bic & 0x01ff) << 2) // BIC-relative
            } else {
                mas // direct
            };
            let ma_byte = (ma << 1) as usize;
            let ramword = (mathram[ma_byte + 1] as u16) | ((mathram[ma_byte] as u16) << 8);

            if strobe & CLEAR_ACC != 0 {
                self.acc = 0;
            }
            if strobe & LAC != 0 {
                self.acc = (ramword as i32) << 16;
            }
            if strobe & READ_ACC != 0 {
                mathram[ma_byte + 1] = ((self.acc >> 16) & 0xff) as u8;
                mathram[ma_byte] = ((self.acc >> 24) & 0xff) as u8;
            }
            if strobe & M_HALT != 0 {
                m_stop = 1; // decremented to 0 at the bottom of the loop
            }
            if strobe & INC_BIC != 0 {
                self.bic = (self.bic + 1) & 0x1ff;
            }
            if strobe & LDC != 0 {
                self.c = ramword as i16;

                // Serial subtract-multiply-accumulate: ACC += (A - B) * C.
                let diff = (self.a as i32).wrapping_sub(self.b as i32) << 1;
                let prod = diff.wrapping_mul(self.c as i32) << 1;
                self.acc = self.acc.wrapping_add(prod);

                // A and B are sign-extended by the 74LS384 after multiplying.
                self.a = if (self.a as u16) & 0x8000 != 0 { -1 } else { 0 };
                self.b = if (self.b as u16) & 0x8000 != 0 { -1 } else { 0 };

                // The multiply-add holds the sequencer for 33 extra cycles.
                mptime += 33;
            }
            if strobe & LDB != 0 {
                self.b = ramword as i16;
            }
            if strobe & LDA != 0 {
                self.a = ramword as i16;
            }

            // Advance the PROM counter; the top two bits are not part of the
            // counter, so each of the four pages wraps independently.
            let tmp = self.mpa.wrapping_add(1);
            self.mpa = (self.mpa & 0x0300) | (tmp & 0x00ff);

            m_stop -= 1;
        }

        // The hardware holds MATH_RUN for `mptime` master clocks; the main CPU
        // runs at master/8, so convert to CPU cycles for the countdown.
        self.busy_cycles = mptime.div_ceil(8);
        if self.busy_cycles == 0 {
            self.math_run = false;
        }
    }

    /// Advance the `MATH_RUN` busy countdown by one CPU cycle. The board calls
    /// this once per main-CPU cycle so the flag clears after the hardware's
    /// computation delay.
    pub fn tick(&mut self) {
        if self.busy_cycles > 0 {
            self.busy_cycles -= 1;
            if self.busy_cycles == 0 {
                self.math_run = false;
            }
        }
    }

    /// Reset dynamic state (the decoded PROMs are configuration and are kept).
    pub fn reset(&mut self) {
        self.mpa = 0;
        self.bic = 0;
        self.a = 0;
        self.b = 0;
        self.c = 0;
        self.acc = 0;
        self.math_run = false;
        self.busy_cycles = 0;
        self.divisor = 0;
        self.dividend = 0;
        self.dvd_shift = 0;
        self.quotient_shift = 0;
        self.prng = 0;
    }
}

impl Debuggable for StarWarsMath {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "MPA",
                value: self.mpa as u64,
                width: 16,
            },
            DebugRegister {
                name: "BIC",
                value: self.bic as u64,
                width: 16,
            },
            DebugRegister {
                name: "ACC",
                value: self.acc as u32 as u64,
                width: 32,
            },
            DebugRegister {
                name: "RUN",
                value: self.math_run as u64,
                width: 1,
            },
        ]
    }
}

impl super::Device for StarWarsMath {
    fn name(&self) -> &'static str {
        "SW-MATRIX"
    }

    fn reset(&mut self) {
        self.reset();
    }

    fn tick(&mut self) {
        self.tick();
    }
}

impl Saveable for StarWarsMath {
    fn save_state(&self, w: &mut StateWriter) {
        // The decoded PROMs are reconstructed from ROM at load; only dynamic
        // state is serialized.
        w.write_u16_le(self.mpa);
        w.write_u16_le(self.bic);
        w.write_u16_le(self.a as u16);
        w.write_u16_le(self.b as u16);
        w.write_u16_le(self.c as u16);
        w.write_u32_le(self.acc as u32);
        w.write_bool(self.math_run);
        w.write_u32_le(self.busy_cycles);
        w.write_u16_le(self.divisor);
        w.write_u16_le(self.dividend);
        w.write_u16_le(self.dvd_shift);
        w.write_u16_le(self.quotient_shift);
        w.write_u32_le(self.prng);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        self.mpa = r.read_u16_le()?;
        self.bic = r.read_u16_le()?;
        self.a = r.read_u16_le()? as i16;
        self.b = r.read_u16_le()? as i16;
        self.c = r.read_u16_le()? as i16;
        self.acc = r.read_u32_le()? as i32;
        self.math_run = r.read_bool()?;
        self.busy_cycles = r.read_u32_le()?;
        self.divisor = r.read_u16_le()?;
        self.dividend = r.read_u16_le()?;
        self.dvd_shift = r.read_u16_le()?;
        self.quotient_shift = r.read_u16_le()?;
        self.prng = r.read_u32_le()? & 0x7f_ffff;
        Ok(())
    }
}
