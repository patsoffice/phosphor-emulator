//! Yamaha YM2151 (OPM) — 8-voice, 4-operator FM synthesiser.
//!
//! A faithful integer port of the OPM synthesis path from MAME's `ymfm`
//! reference core (`3rdparty/ymfm/src/ymfm_*`), restricted to the YM2151 (no
//! SSG-EG, depress, reverb, or rhythm). The die-extracted log-sine / exponent /
//! envelope-increment / detune / phase-step tables live in [`tables`].
//!
//! The chip is clocked at the FM rate (input clock / 64 ≈ 55.9 kHz for the
//! 3.579545 MHz Atari System 1 part). Per FM sample the engine advances the
//! envelope counter, the LFO and noise generators, every operator's phase and
//! envelope, then sums the eight channels through their selected algorithm.
//!
//! The register/timer/IRQ port keeps the shape established by the original stub:
//! [`Ym2151::write`]/[`Ym2151::read`] for the address/data/status ports, and the
//! two interval timers (A/B) that drive the chip's IRQ line — many Atari sound
//! programs idle in a timer-A IRQ loop. [`Ym2151::tick`] advances the timers and
//! accumulates FM samples; [`Ym2151::drain_audio`] resamples them to the host
//! rate.

mod tables;

use crate::core::save_state::{SaveError, Saveable, StateReader, StateWriter};
use tables::*;

// ---------------------------------------------------------------------------
// Timer / control register layout (drives the IRQ line)
// ---------------------------------------------------------------------------

const CTRL_LOAD_A: u8 = 0x01;
const CTRL_LOAD_B: u8 = 0x02;
const CTRL_IRQEN_A: u8 = 0x04;
const CTRL_IRQEN_B: u8 = 0x08;
const CTRL_RESET_A: u8 = 0x10;
const CTRL_RESET_B: u8 = 0x20;

const STATUS_TIMER_A: u8 = 0x01;
const STATUS_TIMER_B: u8 = 0x02;

const REG_TIMER_A_HI: usize = 0x10;
const REG_TIMER_A_LO: usize = 0x11;
const REG_TIMER_B: usize = 0x12;
const REG_CONTROL: usize = 0x14;

// ---------------------------------------------------------------------------
// FM constants
// ---------------------------------------------------------------------------

/// Envelope-generator states (ymfm `EG_*`); indices into the per-state rate set.
const EG_ATTACK: u8 = 1;
const EG_DECAY: u8 = 2;
const EG_SUSTAIN: u8 = 3;
const EG_RELEASE: u8 = 4;

/// Above this envelope attenuation the operator contributes nothing.
const EG_QUIET: u16 = 0x380;

/// Chip clocks per FM sample (the OPM divides its input clock by 64).
const CLOCKS_PER_SAMPLE: u32 = 64;

/// Per-algorithm operator wiring, indexed by the 3-bit CONNECT field. Encoding
/// (ymfm `ALGORITHM(op2in, op3in, op4in, op1out, op2out, op3out)`): bit0 = op2
/// input source, bits1-3 = op3 input, bits4-6 = op4 input (indices into the
/// `opout` scratch), bits7-9 = include op1/op2/op3 in the channel sum.
const ALGORITHM_OPS: [u16; 8] = [
    0x035, // 0: O1->O2->O3->O4                          (1,2,3, 0,0,0)
    0x03a, // 1: (O1+O2)->O3->O4                         (0,5,3, 0,0,0)
    0x064, // 2: (O1+(O2->O3))->O4                       (0,2,6, 0,0,0)
    0x071, // 3: ((O1->O2)+O3)->O4                       (1,0,7, 0,0,0)
    0x131, // 4: (O1->O2)+(O3->O4)                       (1,0,3, 0,1,0)
    0x313, // 5: (O1->O2)+(O1->O3)+(O1->O4)              (1,1,1, 0,1,1)
    0x301, // 6: (O1->O2)+O3+O4                          (1,0,0, 0,1,1)
    0x380, // 7: O1+O2+O3+O4                             (0,0,0, 1,1,1)
];

/// Detune-2 coarse delta in 1/64-semitone units (ymfm `s_detune2_delta`):
/// `(cents*64+50)/100` for 0/600/781/950 cents.
const DETUNE2_DELTA: [i32; 4] = [
    0,
    (600 * 64 + 50) / 100,
    (781 * 64 + 50) / 100,
    (950 * 64 + 50) / 100,
];

// ---------------------------------------------------------------------------
// Math primitives (ymfm `ymfm_fm.ipp`)
// ---------------------------------------------------------------------------

/// Absolute sine as a 4.8 log attenuation for a 10-bit phase; the second half of
/// the curve mirrors the first.
fn abs_sin_attenuation(input: u32) -> u32 {
    let input = if input & 0x100 != 0 { !input } else { input };
    SIN_TABLE[(input & 0xff) as usize] as u32
}

/// Waveform sample: the 4.8 attenuation plus a sign bit (set in the lower half).
fn waveform(phase: u32) -> u32 {
    abs_sin_attenuation(phase) | (((phase >> 9) & 1) << 15)
}

/// 5.8 fixed-point log attenuation → 13-bit linear volume.
fn attenuation_to_volume(input: u32) -> u32 {
    POWER_TABLE[(input & 0xff) as usize] as u32 >> (input >> 8)
}

/// 4-bit envelope increment for a 6-bit rate and 3-bit step index.
fn attenuation_increment(rate: u32, index: u32) -> u32 {
    (INCREMENT_TABLE[rate as usize] >> (4 * index)) & 0xf
}

/// Signed phase displacement for a 3-bit detune and 5-bit key code.
fn detune_adjustment(detune: u32, keycode: u32) -> i32 {
    let result = DETUNE[(keycode * 4 + (detune & 3)) as usize] as i32;
    if detune & 4 != 0 { -result } else { result }
}

/// OPM block/key-code/key-fraction (13-bit `block_freq`) plus a signed delta →
/// 0.10 phase step (the "fnum"); handles the gappy key code and octave wrap.
fn opm_key_code_to_phase_step(block_freq: u32, delta: i32) -> u32 {
    let mut block = (block_freq >> 10) & 7;
    // the 4-bit key code maps 12 notes over 16 slots: multiply by 3/4
    let adjusted_code = ((block_freq >> 6) & 0xf) - ((block_freq >> 8) & 3);
    let mut eff_freq = ((adjusted_code << 6) | (block_freq & 0x3f)) as i32 + delta;

    if eff_freq as u32 >= 768 {
        if eff_freq < 0 {
            eff_freq += 768;
            if block == 0 {
                return PHASE_STEP[0] >> 7;
            }
            block -= 1;
        } else {
            eff_freq -= 768;
            if eff_freq >= 768 {
                block += 1;
                eff_freq -= 768;
            }
            if block >= 7 {
                return PHASE_STEP[767];
            }
            block += 1;
        }
    }
    PHASE_STEP[eff_freq as usize] >> (block ^ 7)
}

// ---------------------------------------------------------------------------
// Operator state
// ---------------------------------------------------------------------------

/// One of the 32 FM operators. Holds only the dynamic state; all tuning is
/// decoded from the register file each sample.
#[derive(Clone, Copy)]
struct Operator {
    /// 10.10 phase accumulator; the waveform index is `phase >> 10`.
    phase: u32,
    /// Envelope attenuation, 0 (loud) … 0x3ff (silent).
    env_attenuation: u16,
    /// Current [`EG_ATTACK`]…[`EG_RELEASE`] state.
    env_state: u8,
    /// Last applied key state, for edge detection.
    key_state: bool,
}

impl Operator {
    const fn new() -> Self {
        Self {
            phase: 0,
            env_attenuation: 0x3ff,
            env_state: EG_RELEASE,
            key_state: false,
        }
    }
}

// ---------------------------------------------------------------------------
// Chip
// ---------------------------------------------------------------------------

/// Full YM2151 (OPM) with FM synthesis, timers, and IRQ.
pub struct Ym2151 {
    regs: [u8; 256],
    address: u8,

    // Timers (chip clocks) + status flags.
    timer_a: u32,
    timer_b: u32,
    status: u8,

    // FM voice state.
    ops: [Operator; 32],
    /// Two-sample op-1 feedback history per channel.
    feedback: [[i32; 2]; 8],
    /// This sample's op-1 value per channel (folds into next sample's feedback).
    feedback_in: [i32; 8],
    /// Pending key state per operator (set by register 0x08).
    keyon: [bool; 32],

    // LFO + noise.
    env_counter: u32,
    lfo_counter: u32,
    lfo_am: u8,
    noise_lfsr: u32,
    noise_counter: u8,
    noise_state: u8,
    /// LFO noise waveform, written one step ahead of the read position.
    lfo_noise_wave: [i16; 256],

    // Audio output: FM samples generated at the native rate, resampled on drain.
    clock_acc: u32,
    native: Vec<f32>,
    resample_pos: f64,
    input_clock: u32,
    sample_rate: u32,
}

impl Ym2151 {
    pub fn new() -> Self {
        Self::with_clock(3_579_545, 44_100)
    }

    /// Construct with a specific input clock and host sample rate.
    pub fn with_clock(input_clock: u32, sample_rate: u32) -> Self {
        let mut ym = Self {
            regs: [0; 256],
            address: 0,
            timer_a: 0,
            timer_b: 0,
            status: 0,
            ops: [Operator::new(); 32],
            feedback: [[0; 2]; 8],
            feedback_in: [0; 8],
            keyon: [false; 32],
            env_counter: 0,
            lfo_counter: 0,
            lfo_am: 0,
            noise_lfsr: 1,
            noise_counter: 0,
            noise_state: 0,
            lfo_noise_wave: [0; 256],
            clock_acc: 0,
            native: Vec::new(),
            resample_pos: 0.0,
            input_clock,
            sample_rate,
        };
        ym.reset();
        ym
    }

    pub fn reset(&mut self) {
        self.regs = [0; 256];
        // OPM powers up with both output channels enabled.
        for ch in 0..8 {
            self.regs[0x20 + ch] = 0xc0;
        }
        self.address = 0;
        self.timer_a = 0;
        self.timer_b = 0;
        self.status = 0;
        self.ops = [Operator::new(); 32];
        self.feedback = [[0; 2]; 8];
        self.feedback_in = [0; 8];
        self.keyon = [false; 32];
        self.env_counter = 0;
        self.lfo_counter = 0;
        self.lfo_am = 0;
        self.noise_lfsr = 1;
        self.noise_counter = 0;
        self.noise_state = 0;
        self.lfo_noise_wave = [0; 256];
        self.clock_acc = 0;
        self.native.clear();
        self.resample_pos = 0.0;
    }

    // -- register accessors -------------------------------------------------

    /// Extract `count` bits at `start` of register `reg + extra`.
    #[inline]
    fn bits(&self, reg: usize, start: u32, count: u32, extra: usize) -> u32 {
        ((self.regs[reg + extra] as u32) >> start) & ((1 << count) - 1)
    }

    // system
    fn lfo_reset(&self) -> bool {
        self.bits(0x01, 1, 1, 0) != 0
    }
    fn noise_frequency(&self) -> u32 {
        self.bits(0x0f, 0, 5, 0) ^ 0x1f
    }
    fn noise_enable(&self) -> bool {
        self.bits(0x0f, 7, 1, 0) != 0
    }
    fn lfo_rate(&self) -> u32 {
        self.regs[0x18] as u32
    }
    fn lfo_am_depth(&self) -> u32 {
        self.bits(0x19, 0, 7, 0)
    }
    fn lfo_pm_depth(&self) -> u32 {
        self.bits(0x1a, 0, 7, 0)
    }
    fn lfo_waveform(&self) -> usize {
        self.bits(0x1b, 0, 2, 0) as usize
    }

    // per-channel (extra = channel)
    fn ch_output_any(&self, ch: usize) -> u32 {
        self.bits(0x20, 6, 2, ch)
    }
    fn ch_output_0(&self, ch: usize) -> bool {
        self.bits(0x20, 6, 1, ch) != 0
    }
    fn ch_output_1(&self, ch: usize) -> bool {
        self.bits(0x20, 7, 1, ch) != 0
    }
    fn ch_feedback(&self, ch: usize) -> u32 {
        self.bits(0x20, 3, 3, ch)
    }
    fn ch_algorithm(&self, ch: usize) -> usize {
        self.bits(0x20, 0, 3, ch) as usize
    }
    fn ch_block_freq(&self, ch: usize) -> u32 {
        (self.bits(0x28, 0, 7, ch) << 6) | self.bits(0x30, 2, 6, ch)
    }
    fn ch_lfo_pm_sens(&self, ch: usize) -> u32 {
        self.bits(0x38, 4, 3, ch)
    }
    fn ch_lfo_am_sens(&self, ch: usize) -> u32 {
        self.bits(0x38, 0, 2, ch)
    }

    // per-operator (extra = opnum)
    fn op_detune(&self, op: usize) -> u32 {
        self.bits(0x40, 4, 3, op)
    }
    fn op_multiple(&self, op: usize) -> u32 {
        self.bits(0x40, 0, 4, op)
    }
    fn op_total_level(&self, op: usize) -> u32 {
        self.bits(0x60, 0, 7, op)
    }
    fn op_ksr(&self, op: usize) -> u32 {
        self.bits(0x80, 6, 2, op)
    }
    fn op_attack_rate(&self, op: usize) -> u32 {
        self.bits(0x80, 0, 5, op)
    }
    fn op_lfo_am_enable(&self, op: usize) -> bool {
        self.bits(0xa0, 7, 1, op) != 0
    }
    fn op_decay_rate(&self, op: usize) -> u32 {
        self.bits(0xa0, 0, 5, op)
    }
    fn op_detune2(&self, op: usize) -> usize {
        self.bits(0xc0, 6, 2, op) as usize
    }
    fn op_sustain_rate(&self, op: usize) -> u32 {
        self.bits(0xc0, 0, 5, op)
    }
    fn op_sustain_level(&self, op: usize) -> u32 {
        self.bits(0xe0, 4, 4, op)
    }
    fn op_release_rate(&self, op: usize) -> u32 {
        self.bits(0xe0, 0, 4, op)
    }

    /// Register operator index for a channel's algorithm slot (O1,O2,O3,O4).
    /// OPM interleaves operators as `ch + {0,16,8,24}` (ymfm `operator_map`).
    #[inline]
    fn op_index(ch: usize, slot: usize) -> usize {
        ch + [0, 16, 8, 24][slot]
    }

    // -- ports --------------------------------------------------------------

    /// Read the status register (both port offsets return it). The BUSY bit is
    /// always clear here as register writes complete instantly.
    pub fn read(&self, _offset: u16) -> u8 {
        self.status
    }

    /// Write the address port (even offset) or the data port (odd offset).
    pub fn write(&mut self, offset: u16, data: u8) {
        if offset & 1 == 0 {
            self.address = data;
        } else {
            self.write_reg(self.address, data);
        }
    }

    fn write_reg(&mut self, reg: u8, data: u8) {
        let prev = self.regs[reg as usize];
        // The LFO AM/PM depths share register 0x19; PM is shadowed into 0x1a.
        if reg == 0x19 {
            self.regs[(0x19 + (data >> 7)) as usize] = data;
        } else if reg != 0x1a {
            self.regs[reg as usize] = data;
        }

        match reg as usize {
            0x08 => {
                // Key on/off: low 3 bits select the channel, bits 3-6 the four
                // operators (in O1..O4 slot order).
                let ch = (data & 7) as usize;
                let opmask = (data >> 3) & 0xf;
                for slot in 0..4 {
                    self.keyon[Self::op_index(ch, slot)] = opmask & (1 << slot) != 0;
                }
            }
            REG_CONTROL => {
                if data & CTRL_RESET_A != 0 {
                    self.status &= !STATUS_TIMER_A;
                }
                if data & CTRL_RESET_B != 0 {
                    self.status &= !STATUS_TIMER_B;
                }
                if data & CTRL_LOAD_A != 0 && prev & CTRL_LOAD_A == 0 {
                    self.timer_a = self.timer_a_period();
                }
                if data & CTRL_LOAD_B != 0 && prev & CTRL_LOAD_B == 0 {
                    self.timer_b = self.timer_b_period();
                }
            }
            _ => {}
        }
    }

    fn timer_a_period(&self) -> u32 {
        let ta = ((self.regs[REG_TIMER_A_HI] as u32) << 2) | (self.regs[REG_TIMER_A_LO] as u32 & 3);
        64 * (1024 - ta)
    }

    fn timer_b_period(&self) -> u32 {
        1024 * (256 - self.regs[REG_TIMER_B] as u32)
    }

    // -- FM synthesis -------------------------------------------------------

    /// Advance and sample the LFO + noise generators, returning the raw signed
    /// PM value for this sample (and latching the AM value in `self.lfo_am`).
    fn clock_noise_and_lfo(&mut self) -> i32 {
        let freq = self.noise_frequency();
        for _ in 0..2 {
            self.noise_lfsr <<= 1;
            self.noise_lfsr |= ((self.noise_lfsr >> 17) ^ (self.noise_lfsr >> 14) ^ 1) & 1;
            let old = self.noise_counter;
            self.noise_counter = self.noise_counter.wrapping_add(1);
            if old as u32 >= freq {
                self.noise_counter = 0;
                self.noise_state = ((self.noise_lfsr >> 17) & 1) as u8;
            }
        }

        let rate = self.lfo_rate();
        self.lfo_counter = self
            .lfo_counter
            .wrapping_add((0x10 | (rate & 0xf)) << (rate >> 4));
        if self.lfo_reset() {
            self.lfo_counter = 0;
        }

        let lfo = ((self.lfo_counter >> 22) & 0xff) as usize;
        let lfo_noise = (self.noise_lfsr >> 17) & 0xff;
        self.lfo_noise_wave[(lfo + 1) & 0xff] = (lfo_noise | (lfo_noise << 8)) as i16;

        let ampm = self.lfo_value(lfo) as i32;
        self.lfo_am = (((ampm & 0xff) * self.lfo_am_depth() as i32) >> 7) as u8;
        ((ampm >> 8) * self.lfo_pm_depth() as i32) >> 7
    }

    /// AM in the low byte, signed PM in the high byte, for the selected waveform.
    fn lfo_value(&self, lfo: usize) -> i16 {
        let index = lfo as u32;
        let (am, pm): (u8, u8) = match self.lfo_waveform() {
            0 => ((index ^ 0xff) as u8, index as u8), // sawtooth
            1 => {
                let am = if index & 0x80 != 0 { 0 } else { 0xff };
                (am, am ^ 0x80) // square
            }
            2 => {
                let am = if index & 0x80 != 0 {
                    (index << 1) as u8
                } else {
                    ((index ^ 0xff) << 1) as u8
                };
                let pm = if index & 0x40 != 0 { am } else { !am };
                (am, pm) // triangle
            }
            _ => return self.lfo_noise_wave[lfo], // noise (dynamic)
        };
        ((am as u16) | ((pm as u16) << 8)) as i16
    }

    /// AM attenuation offset for a channel, scaled by its AM sensitivity.
    fn lfo_am_offset(&self, ch: usize) -> u32 {
        let sens = self.ch_lfo_am_sens(ch);
        if sens == 0 {
            0
        } else {
            (self.lfo_am as u32) << (sens - 1)
        }
    }

    /// Effective 6-bit envelope rate for an operator's current state.
    fn eg_rate(&self, op: usize, ch: usize, state: u8) -> u32 {
        let keycode = (self.ch_block_freq(ch) >> 8) & 0x1f;
        let ksr = keycode >> (self.op_ksr(op) ^ 3);
        let raw = match state {
            EG_ATTACK => self.op_attack_rate(op) * 2,
            EG_DECAY => self.op_decay_rate(op) * 2,
            EG_SUSTAIN => self.op_sustain_rate(op) * 2,
            _ => self.op_release_rate(op) * 4 + 2,
        };
        if raw == 0 { 0 } else { (raw + ksr).min(63) }
    }

    /// Target attenuation for the decay→sustain transition (D1L; 15 ⇒ −∞).
    fn eg_sustain(&self, op: usize) -> u16 {
        let mut s = self.op_sustain_level(op);
        s |= (s + 1) & 0x10;
        (s << 5) as u16
    }

    /// 0.10 phase step for an operator this sample (detune-2 + PM, detune-1, MUL).
    fn compute_phase_step(&self, op: usize, ch: usize, lfo_raw_pm: i32) -> u32 {
        let block_freq = self.ch_block_freq(ch);
        let keycode = (block_freq >> 8) & 0x1f;

        let mut delta = DETUNE2_DELTA[self.op_detune2(op)];
        let pms = self.ch_lfo_pm_sens(ch);
        if pms != 0 {
            if pms < 6 {
                delta += lfo_raw_pm >> (6 - pms);
            } else {
                delta += lfo_raw_pm << (pms - 5);
            }
        }

        let mut step = opm_key_code_to_phase_step(block_freq, delta);
        step = step.wrapping_add(detune_adjustment(self.op_detune(op), keycode) as u32);
        let mul = self.op_multiple(op) * 2;
        let mul = if mul == 0 { 1 } else { mul };
        step.wrapping_mul(mul) >> 1
    }

    /// Effective attenuation including LFO AM and total level.
    fn envelope_attenuation(&self, op: usize, am_offset: u32) -> u32 {
        let mut result = self.ops[op].env_attenuation as u32;
        if self.op_lfo_am_enable(op) {
            result += am_offset;
        }
        result += self.op_total_level(op) << 3;
        result.min(0x3ff)
    }

    /// 14-bit signed operator output for a modulated phase.
    fn compute_volume(&self, op: usize, phase: u32, am_offset: u32) -> i32 {
        if self.ops[op].env_attenuation > EG_QUIET {
            return 0;
        }
        let sin_atten = waveform(phase & 0x3ff);
        let env = self.envelope_attenuation(op, am_offset) << 2;
        let result = attenuation_to_volume((sin_atten & 0x7fff) + env) as i32;
        if sin_atten & 0x8000 != 0 {
            -result
        } else {
            result
        }
    }

    /// 11-bit signed noise output (channel 7, operator 4 only).
    fn compute_noise_volume(&self, op: usize, am_offset: u32) -> i32 {
        let result = ((self.envelope_attenuation(op, am_offset) ^ 0x3ff) << 1) as i32;
        if self.noise_state & 1 != 0 {
            -result
        } else {
            result
        }
    }

    /// Apply a key on/off edge: reset to attack (phase too) or start release.
    fn clock_keystate(&mut self, op: usize, ch: usize) {
        let keyon = self.keyon[op];
        if keyon == self.ops[op].key_state {
            return;
        }
        self.ops[op].key_state = keyon;
        if keyon {
            if self.ops[op].env_state != EG_ATTACK {
                self.ops[op].env_state = EG_ATTACK;
                self.ops[op].phase = 0;
                // Attack rate ≥ 62 jumps straight to full volume.
                if self.eg_rate(op, ch, EG_ATTACK) >= 62 {
                    self.ops[op].env_attenuation = 0;
                }
            }
        } else if self.ops[op].env_state < EG_RELEASE {
            self.ops[op].env_state = EG_RELEASE;
        }
    }

    /// Advance one operator's envelope (on an envelope cycle) and phase.
    fn clock_operator(&mut self, op: usize, ch: usize, lfo_raw_pm: i32) {
        self.clock_keystate(op, ch);

        if self.env_counter & 3 == 0 {
            self.clock_envelope(op, ch, self.env_counter >> 2);
        }

        let step = self.compute_phase_step(op, ch, lfo_raw_pm);
        self.ops[op].phase = self.ops[op].phase.wrapping_add(step);
    }

    fn clock_envelope(&mut self, op: usize, ch: usize, env_counter: u32) {
        // State transitions, evaluated before applying the increment.
        if self.ops[op].env_state == EG_ATTACK && self.ops[op].env_attenuation == 0 {
            self.ops[op].env_state = EG_DECAY;
        }
        if self.ops[op].env_state == EG_DECAY && self.ops[op].env_attenuation >= self.eg_sustain(op)
        {
            self.ops[op].env_state = EG_SUSTAIN;
        }

        let state = self.ops[op].env_state;
        let rate = self.eg_rate(op, ch, state);
        let rate_shift = rate >> 2;
        let counter = env_counter << rate_shift;
        if counter & 0x7ff != 0 {
            return;
        }
        let shift = if rate_shift <= 11 { 11 } else { rate_shift };
        let increment = attenuation_increment(rate, (counter >> shift) & 7);

        if state == EG_ATTACK {
            if rate < 62 {
                let a = self.ops[op].env_attenuation as i32;
                let delta = (!a).wrapping_mul(increment as i32) >> 4;
                self.ops[op].env_attenuation = (a + delta) as u16;
            }
        } else {
            let a = self.ops[op].env_attenuation + increment as u16;
            self.ops[op].env_attenuation = if a >= 0x400 { 0x3ff } else { a };
        }
    }

    /// Compute one channel's signed sample through its algorithm + feedback.
    fn output_channel(&mut self, ch: usize) -> i32 {
        let am_offset = self.lfo_am_offset(ch);

        // Operator 1, with optional self-feedback from the last two samples.
        let feedback = self.ch_feedback(ch);
        let opmod = if feedback != 0 {
            (self.feedback[ch][0] + self.feedback[ch][1]) >> (10 - feedback)
        } else {
            0
        };
        let op0 = Self::op_index(ch, 0);
        let p = (self.ops[op0].phase >> 10).wrapping_add(opmod as u32);
        let op1value = self.compute_volume(op0, p, am_offset);
        self.feedback_in[ch] = op1value;

        if self.ch_output_any(ch) == 0 {
            return 0;
        }

        let algo = ALGORITHM_OPS[self.ch_algorithm(ch)];
        let mut opout = [0i32; 8];
        opout[1] = op1value;

        let op2 = Self::op_index(ch, 1);
        let opmod = opout[(algo & 1) as usize] >> 1;
        let p = (self.ops[op2].phase >> 10).wrapping_add(opmod as u32);
        opout[2] = self.compute_volume(op2, p, am_offset);
        opout[5] = opout[1] + opout[2];

        let op3 = Self::op_index(ch, 2);
        let opmod = opout[((algo >> 1) & 7) as usize] >> 1;
        let p = (self.ops[op3].phase >> 10).wrapping_add(opmod as u32);
        opout[3] = self.compute_volume(op3, p, am_offset);
        opout[6] = opout[1] + opout[3];
        opout[7] = opout[2] + opout[3];

        let op4 = Self::op_index(ch, 3);
        let mut result = if self.noise_enable() && ch == 7 {
            self.compute_noise_volume(op4, am_offset)
        } else {
            let opmod = opout[((algo >> 4) & 7) as usize] >> 1;
            let p = (self.ops[op4].phase >> 10).wrapping_add(opmod as u32);
            self.compute_volume(op4, p, am_offset)
        };

        // rshift = 0, clipmax = 32767 for the OPM output stage.
        const CLIPMAX: i32 = 32767;
        if algo & 0x080 != 0 {
            result = (result + opout[1]).clamp(-CLIPMAX - 1, CLIPMAX);
        }
        if algo & 0x100 != 0 {
            result = (result + opout[2]).clamp(-CLIPMAX - 1, CLIPMAX);
        }
        if algo & 0x200 != 0 {
            result = (result + opout[3]).clamp(-CLIPMAX - 1, CLIPMAX);
        }
        result
    }

    /// Generate one native-rate FM sample (mono sum of all channels).
    fn generate_sample(&mut self) -> i32 {
        // Envelope counter advances with a ÷3 divider (skip every 4th value).
        self.env_counter = self.env_counter.wrapping_add(1);
        if self.env_counter & 3 == 3 {
            self.env_counter = self.env_counter.wrapping_add(1);
        }

        let lfo_raw_pm = self.clock_noise_and_lfo();

        for ch in 0..8 {
            self.feedback[ch][0] = self.feedback[ch][1];
            self.feedback[ch][1] = self.feedback_in[ch];
            for slot in 0..4 {
                self.clock_operator(Self::op_index(ch, slot), ch, lfo_raw_pm);
            }
        }

        let mut mono = 0i32;
        for ch in 0..8 {
            let sample = self.output_channel(ch);
            if self.ch_output_0(ch) || self.ch_output_1(ch) {
                mono += sample;
            }
        }
        mono
    }

    // -- clocking + output --------------------------------------------------

    /// Advance the timers and FM engine by `cycles` chip clocks.
    pub fn tick(&mut self, cycles: u32) {
        let ctrl = self.regs[REG_CONTROL];
        if ctrl & CTRL_LOAD_A != 0 {
            if self.timer_a <= cycles {
                self.status |= STATUS_TIMER_A;
                self.timer_a = self.timer_a_period();
            } else {
                self.timer_a -= cycles;
            }
        }
        if ctrl & CTRL_LOAD_B != 0 {
            if self.timer_b <= cycles {
                self.status |= STATUS_TIMER_B;
                self.timer_b = self.timer_b_period();
            } else {
                self.timer_b -= cycles;
            }
        }

        // Generate FM samples every 64 chip clocks.
        self.clock_acc += cycles;
        while self.clock_acc >= CLOCKS_PER_SAMPLE {
            self.clock_acc -= CLOCKS_PER_SAMPLE;
            let sample = self.generate_sample();
            // Eight channels each clipped to ±32767; normalise that full-scale
            // sum to [-1, 1].
            self.native.push(sample as f32 / 262_144.0);
        }
    }

    /// The IRQ line: asserted while a timer flag is set and its enable bit is on.
    pub fn irq(&self) -> bool {
        let ctrl = self.regs[REG_CONTROL];
        (self.status & STATUS_TIMER_A != 0 && ctrl & CTRL_IRQEN_A != 0)
            || (self.status & STATUS_TIMER_B != 0 && ctrl & CTRL_IRQEN_B != 0)
    }

    /// Drain accumulated FM audio, resampled from the native FM rate
    /// (`input_clock / 64`) to the host sample rate by linear interpolation.
    pub fn drain_audio(&mut self) -> Vec<f32> {
        let native_rate = (self.input_clock / CLOCKS_PER_SAMPLE) as f64;
        let step = native_rate / self.sample_rate as f64;
        let mut out = Vec::new();
        // Keep one source sample of history so interpolation can look ahead.
        while (self.resample_pos as usize) + 1 < self.native.len() {
            let i = self.resample_pos as usize;
            let frac = self.resample_pos - i as f64;
            let s = self.native[i] as f64 * (1.0 - frac) + self.native[i + 1] as f64 * frac;
            out.push(s as f32);
            self.resample_pos += step;
        }
        // Drop the consumed source samples, carrying the fractional position.
        let consumed = self.resample_pos as usize;
        if consumed > 0 {
            self.native.drain(0..consumed);
            self.resample_pos -= consumed as f64;
        }
        out
    }
}

impl Default for Ym2151 {
    fn default() -> Self {
        Self::new()
    }
}

impl Saveable for Ym2151 {
    fn save_state(&self, w: &mut StateWriter) {
        w.write_bytes(&self.regs);
        w.write_u8(self.address);
        w.write_u32_le(self.timer_a);
        w.write_u32_le(self.timer_b);
        w.write_u8(self.status);
        for op in &self.ops {
            w.write_u32_le(op.phase);
            w.write_u16_le(op.env_attenuation);
            w.write_u8(op.env_state);
            w.write_bool(op.key_state);
        }
        for ch in 0..8 {
            w.write_i32_le(self.feedback[ch][0]);
            w.write_i32_le(self.feedback[ch][1]);
            w.write_i32_le(self.feedback_in[ch]);
        }
        for &k in &self.keyon {
            w.write_bool(k);
        }
        w.write_u32_le(self.env_counter);
        w.write_u32_le(self.lfo_counter);
        w.write_u8(self.lfo_am);
        w.write_u32_le(self.noise_lfsr);
        w.write_u8(self.noise_counter);
        w.write_u8(self.noise_state);
    }

    fn load_state(&mut self, r: &mut StateReader) -> Result<(), SaveError> {
        r.read_bytes_into(&mut self.regs)?;
        self.address = r.read_u8()?;
        self.timer_a = r.read_u32_le()?;
        self.timer_b = r.read_u32_le()?;
        self.status = r.read_u8()?;
        for op in &mut self.ops {
            op.phase = r.read_u32_le()?;
            op.env_attenuation = r.read_u16_le()?;
            op.env_state = r.read_u8()?;
            op.key_state = r.read_bool()?;
        }
        for ch in 0..8 {
            self.feedback[ch][0] = r.read_i32_le()?;
            self.feedback[ch][1] = r.read_i32_le()?;
            self.feedback_in[ch] = r.read_i32_le()?;
        }
        for k in &mut self.keyon {
            *k = r.read_bool()?;
        }
        self.env_counter = r.read_u32_le()?;
        self.lfo_counter = r.read_u32_le()?;
        self.lfo_am = r.read_u8()?;
        self.noise_lfsr = r.read_u32_le()?;
        self.noise_counter = r.read_u8()?;
        self.noise_state = r.read_u8()?;
        // Transient resampler buffers are not saved.
        self.lfo_noise_wave = [0; 256];
        self.clock_acc = 0;
        self.native.clear();
        self.resample_pos = 0.0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poke(ym: &mut Ym2151, reg: u8, data: u8) {
        ym.write(0, reg); // address port
        ym.write(1, data); // data port
    }

    // -- timers / IRQ (unchanged behaviour from the original stub) -----------

    #[test]
    fn status_starts_clear_and_not_busy() {
        let ym = Ym2151::new();
        assert_eq!(ym.read(0), 0);
        assert!(!ym.irq());
    }

    #[test]
    fn timer_a_overflow_sets_flag_and_irq() {
        let mut ym = Ym2151::new();
        poke(&mut ym, 0x10, 0xFF);
        poke(&mut ym, 0x11, 0x03); // A = 1023 → period 64
        poke(&mut ym, 0x14, CTRL_LOAD_A | CTRL_IRQEN_A);
        assert!(!ym.irq());
        ym.tick(64);
        assert_eq!(ym.read(0) & STATUS_TIMER_A, STATUS_TIMER_A);
        assert!(ym.irq());
        poke(&mut ym, 0x14, CTRL_LOAD_A | CTRL_IRQEN_A | CTRL_RESET_A);
        assert!(!ym.irq(), "reset clears the flag and IRQ");
    }

    #[test]
    fn timer_a_irq_masked_without_enable() {
        let mut ym = Ym2151::new();
        poke(&mut ym, 0x10, 0xFF);
        poke(&mut ym, 0x11, 0x03);
        poke(&mut ym, 0x14, CTRL_LOAD_A); // load, no IRQ enable
        ym.tick(64);
        assert_eq!(ym.read(0) & STATUS_TIMER_A, STATUS_TIMER_A);
        assert!(!ym.irq());
    }

    // -- FM synthesis --------------------------------------------------------

    /// Program channel 0 with a single audible carrier (algorithm 7, only
    /// operator O1 loud) and key it on.
    fn single_carrier_note(ym: &mut Ym2151) {
        poke(ym, 0x20, 0xc7); // ch0: both outputs, feedback 0, algorithm 7
        poke(ym, 0x28, 0x4a); // key code (a mid note)
        // O1 (opnum 0): MUL 1, TL 0 (loud), AR 31 (fast), RR 15, D1L 0 (full sustain)
        poke(ym, 0x40, 0x01);
        poke(ym, 0x60, 0x00);
        poke(ym, 0x80, 0x1f);
        poke(ym, 0xe0, 0x0f);
        // Silence the other three carriers of algorithm 7 (TL 127).
        for op in [16u8, 8, 24] {
            poke(ym, 0x40 + op, 0x01);
            poke(ym, 0x60 + op, 0x7f);
            poke(ym, 0x80 + op, 0x1f);
            poke(ym, 0xe0 + op, 0x0f);
        }
        poke(ym, 0x08, 0x78); // key on all four operators of channel 0
    }

    #[test]
    fn fm_note_produces_oscillating_audio() {
        let mut ym = Ym2151::new();
        single_carrier_note(&mut ym);

        let samples: Vec<i32> = (0..2000).map(|_| ym.generate_sample()).collect();

        let peak = samples.iter().map(|s| s.abs()).max().unwrap();
        assert!(peak > 1000, "expected audible output, peak was {peak}");
        // A waveform swings both ways, unlike silence or DC.
        assert!(samples.iter().any(|&s| s > 100), "no positive excursion");
        assert!(samples.iter().any(|&s| s < -100), "no negative excursion");
    }

    #[test]
    fn envelope_attacks_then_release_decays_to_silence() {
        let mut ym = Ym2151::new();
        single_carrier_note(&mut ym);

        // Let the attack settle, then measure the sustained peak.
        let sustained: i32 = (0..1500).map(|_| ym.generate_sample().abs()).max().unwrap();
        assert!(sustained > 1000, "note should sustain audibly: {sustained}");

        // Key off: the release rate of 15 drives attenuation to maximum.
        poke(&mut ym, 0x08, 0x00);
        for _ in 0..20_000 {
            ym.generate_sample();
        }
        let after: i32 = (0..500).map(|_| ym.generate_sample().abs()).max().unwrap();
        assert!(after < 50, "released note should fall silent, got {after}");
    }

    #[test]
    fn silent_chip_outputs_nothing() {
        let mut ym = Ym2151::new();
        let peak: i32 = (0..1000).map(|_| ym.generate_sample().abs()).max().unwrap();
        assert_eq!(peak, 0, "no keyed note → silence");
    }

    #[test]
    fn save_load_round_trips() {
        let mut ym = Ym2151::new();
        single_carrier_note(&mut ym);
        for _ in 0..500 {
            ym.generate_sample();
        }

        let mut w = StateWriter::new();
        ym.save_state(&mut w);
        let bytes = w.into_vec();

        let mut ym2 = Ym2151::new();
        let mut r = StateReader::new(&bytes);
        ym2.load_state(&mut r).unwrap();

        // Both chips advance identically from the restored state.
        let a: Vec<i32> = (0..200).map(|_| ym.generate_sample()).collect();
        let b: Vec<i32> = (0..200).map(|_| ym2.generate_sample()).collect();
        assert_eq!(a, b, "restored chip diverges");
    }
}
