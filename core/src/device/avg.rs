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
//! The generator is a sequencer, not an instruction decoder. A 256×4-bit PROM
//! holds the next-state table; a latch holds the current state plus the halt
//! flag. Every clock the PROM is addressed by
//! `(halt ^ 1) << 7 | op << 4 | state` and its low nibble becomes the next
//! state. Bit 3 of the state (ST3) gates dispatch: states `8`–`F` run handlers
//! `0`–`7`, states `0`–`7` are idle waits that still cost a clock. Handlers
//! `0`–`3` latch DVY, the opcode, DVX and the intensity from vector memory;
//! handlers `4`–`7` are strobe0–strobe3 (normalize, binary scale, color/branch,
//! draw).
//!
//! [`Avg::execute`] runs that loop directly, so per-instruction timing falls
//! out of the PROM rather than being assumed: the sequencer is clocked at
//! master/8, each state costs 8 master-clock cycles, and strobe3 adds the beam
//! time of the vector it draws. The total is reported by
//! [`Avg::run_cycles`] and is what a board converts into its VG_HALT window.
//!
//! [`Avg::load_state_prom`] installs the game's own copy of that table; a
//! built-in default stands in until it does.
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
//! Instruction length is a consequence of how many latch states the PROM walks
//! through, not a property decoded up front: VCTR (op 0) runs latch0–latch3 and
//! so consumes 4 bytes, SVEC (op 2) runs latch1 and latch3 only and packs DVX
//! and int_latch into the low byte of its single word, and every other opcode
//! runs latch1 + latch0 for 2 bytes.
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

    /// The byte (word, on Quantum) the address counter is currently presenting
    /// to the latches — refreshed before every dispatched state.
    data: u16,
    /// 3-bit opcode latched by handler 1 (0 VCTR, 1 HALT, 2 SVEC, 3 STAT,
    /// 4 CNTR, 5 JSR, 6 RTS, 7 JMP). It also selects the PROM row, so it
    /// steers the rest of the instruction's state sequence.
    op: u8,
    /// X delta (13-bit Tempest/Star Wars, 12-bit Quantum).
    dvx: u16,
    /// Y delta / operand (13-bit).
    dvy: u16,
    /// DVY bit 12 — selects scale vs color/intensity in the STAT strobe.
    dvy12: u8,
    /// Intensity latch (4-bit) from handler 3.
    int_latch: u8,
    /// Vector timer, loaded by normalization and binary scaling and consumed
    /// (and cleared) by strobe3, where it sets the beam's travel time.
    timer: u16,

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

    /// The halt flag, set by strobe3 on a HALT opcode and cleared by
    /// [`go`](Self::go). It is bit 4 of the PROM address, so a halted
    /// sequencer parks in the table's all-zero lower half.
    halted: bool,

    /// Master-clock cycles between the GO write and the halt becoming visible.
    /// We walk the vector list in one go, but the hardware takes real time to
    /// do it and holds VG_HALT low meanwhile — games poll that flag, so a
    /// generator that is instantly done lets them do more work per frame than
    /// the hardware would. The board converts this to its own clock and
    /// reports "busy" for that long.
    run_cycles: u32,

    /// The 256×4 next-state PROM (low nibbles), initially the built-in
    /// [`default_state_prom`] and replaced by the game's own via
    /// [`load_state_prom`](Self::load_state_prom).
    state_prom: [u8; 0x100],
    /// Sequencer state latch: bits 3:0 the state, bit 4 the halt flag as it
    /// stood when the current PROM lookup was addressed.
    state_latch: u8,
    /// Set by strobe2 when a branch targets address 0 — the frame delimiter
    /// for the games whose vector list loops forever instead of halting.
    frame_done: bool,

    /// Accumulated display list for the current frame.
    display_list: Vec<VectorLine>,
}

/// Master-clock cycles per state-machine iteration, independent of drawing.
/// The sequencer is clocked at a fixed rate and every state costs the same;
/// only the *number* of states varies, and that comes from the state PROM.
const AVG_CYCLES_PER_STATE: u32 = 8;

/// Upper bound on states per [`Avg::execute`] call, so a vector list that
/// neither halts nor branches to zero cannot spin forever. Real lists run a
/// few thousand instructions of 3–8 states each.
const MAX_STATES: u32 = 500_000;

/// The state each opcode's sequence runs through, in order, starting from the
/// idle state 0. States 8–F dispatch handlers 0–7 — 8 latch0, 9 latch1,
/// A latch2, B latch3, C strobe0, D strobe1, E strobe2, F strobe3 — and
/// states 0–7 are idle waits. Ops 0–4 drop back to idle at the end; the branch
/// ops 5–7 chain straight into the next instruction's latch1.
const STATE_CHAINS: [&[u8]; 8] = [
    &[0, 9, 8, 0xB, 0xA, 0xC, 0xD, 0xF], // VCTR
    &[0, 9, 8, 0xF],                     // HALT
    &[0, 9, 0xB, 0xC, 0xD, 0xF],         // SVEC
    &[0, 9, 8, 7, 6, 5, 0xE],            // STAT
    &[0, 9, 8, 0xC, 0xF],                // CNTR
    &[0, 9, 8, 0xC, 0xD, 0xE],           // JSR
    &[0, 9, 8, 0xD, 0xE],                // RTS
    &[0, 9, 8, 0xE],                     // JMP
];

/// The sequencer table a fresh [`Avg`] starts with, built from
/// [`STATE_CHAINS`]: entry `0x80 | op << 4 | state` is the state that follows
/// `state` while the generator runs, and the halted half (below `0x80`) is all
/// zeros, which parks it.
///
/// [`Avg::load_state_prom`] replaces this with the game's own PROM. The games'
/// PROMs differ only in which idle states pad an opcode out — the state counts,
/// and so the timing, are the same — so a machine that has not wired its PROM
/// up still sequences and times correctly.
fn default_state_prom() -> [u8; 0x100] {
    let mut prom = [0u8; 0x100];
    for (op, chain) in STATE_CHAINS.iter().enumerate() {
        let row = 0x80 | (op << 4);
        for pair in chain.windows(2) {
            prom[row | usize::from(pair[0])] = pair[1];
        }
        let last = usize::from(chain[chain.len() - 1]);
        prom[row | last] = if op >= 5 { 9 } else { 0 };
    }
    prom
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
            data: 0,
            op: 0,
            dvx: 0,
            dvy: 0,
            dvy12: 0,
            int_latch: 0,
            timer: 0,
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
            run_cycles: 0,
            state_prom: default_state_prom(),
            state_latch: 0,
            frame_done: false,
            display_list: Vec::with_capacity(2048),
        }
    }

    /// Trigger AVG execution (CPU writes to AVG GO register).
    ///
    /// Only the address counter and the halt flag are reset. The state latch
    /// keeps the state it parked in, so the first lookup of the new run still
    /// comes from the halted half of the PROM and costs one idle state before
    /// the first opcode is latched — exactly as the hardware does.
    pub fn go(&mut self) {
        self.pc = 0;
        self.sp = 0;
        self.halted = false;
        self.run_cycles = 0;
    }

    /// Returns true if the AVG has halted.
    pub fn is_halted(&self) -> bool {
        self.halted
    }

    /// Master-clock cycles the last [`execute`](Self::execute) spent between
    /// the GO write and raising the halt — the window over which the hardware
    /// holds VG_HALT low and a polling game has to wait.
    pub fn run_cycles(&self) -> u32 {
        self.run_cycles
    }

    /// Load the game's 256×4 AVG state PROM — the sequencer's next-state
    /// table, and therefore the source of instruction timing. Only the low
    /// nibble of each byte is the next-state field. A short image leaves the
    /// remaining entries at their [`default_state_prom`] values.
    pub fn load_state_prom(&mut self, data: &[u8]) {
        for (entry, byte) in self.state_prom.iter_mut().zip(data) {
            *entry = byte & 0x0F;
        }
    }

    /// Address the next-state PROM: the halt flag (inverted) picks the half,
    /// the opcode picks the row, and the latched state picks the column.
    fn state_addr(&self) -> usize {
        usize::from(((self.state_latch >> 4) ^ 1) & 1) << 7
            | usize::from(self.op & 7) << 4
            | usize::from(self.state_latch & 0x0F)
    }

    // The opcode's individual bits each steer part of the datapath, so the
    // strobes test them rather than the opcode as a whole.
    fn op0(&self) -> bool {
        self.op & 1 != 0
    }
    fn op1(&self) -> bool {
        self.op & 2 != 0
    }
    fn op2(&self) -> bool {
        self.op & 4 != 0
    }

    /// Present the byte (Quantum: word) at the address counter to the latches.
    ///
    /// Tempest addresses vector memory as `pc ^ 1`, Star Wars in native order,
    /// and Quantum reads the big-endian word the counter is inside.
    fn update_databus(&mut self, vmem: &[u8]) {
        let byte = |i: usize| u16::from(vmem.get(i).copied().unwrap_or(0));
        self.data = match self.variant {
            AvgVariant::Tempest => byte(usize::from(self.pc) ^ 1),
            AvgVariant::StarWars => byte(usize::from(self.pc)),
            AvgVariant::Quantum => Self::read_word_be(vmem, self.pc & !1),
        };
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
        self.state_latch = 0;
        self.timer = 0;
        self.op = 0;
        self.dvx = 0;
        self.dvy = 0;
        self.dvy12 = 0;
        self.int_latch = 0;
        self.data = 0;
        self.frame_done = false;
        self.run_cycles = 0;
        self.halted = true;
        self.has_prev = false;
        self.xpos = self.xcenter;
        self.ypos = self.ycenter;
        self.prev_x = self.xcenter;
        self.prev_y = self.ycenter;
        self.display_list.clear();
    }

    /// Run the sequencer until the vector list halts or branches to address 0.
    ///
    /// `vmem` is the combined vector RAM + ROM (8 KB for Tempest).
    /// `color_ram` is the 16-entry color RAM for Tempest/Quantum color lookup.
    ///
    /// Each iteration is one state: look the next state up in the PROM, run
    /// its handler if ST3 is set, and charge [`AVG_CYCLES_PER_STATE`] plus
    /// whatever beam time the handler consumed. That accumulated count is what
    /// [`run_cycles`](Self::run_cycles) reports, sampled at the moment the
    /// halt flag is raised.
    ///
    /// Returns true if a frame was completed (a branch to address 0). Games
    /// whose list ends in HALT return false; their frame is delimited by the
    /// halt instead.
    pub fn execute(&mut self, vmem: &[u8], color_ram: &[u8; 16]) -> bool {
        self.frame_done = false;
        let mut cycles = 0u32;

        for _ in 0..MAX_STATES {
            self.state_latch = (self.state_latch & 0x10) | self.state_prom[self.state_addr()];

            // ST3 gates dispatch: states 8-F run handlers 0-7, states 0-7 are
            // idle waits that still cost the sequencer a clock.
            if self.state_latch & 8 != 0 {
                self.update_databus(vmem);
                cycles += self.dispatch(self.state_latch & 7, color_ram);
            }

            // The halt only becomes visible once the CPU has had the cycles
            // the generator spent getting here, so sample the count on the
            // state that raises it rather than at the end of the run.
            if self.halted && self.state_latch & 0x10 == 0 {
                self.run_cycles = cycles;
            }

            self.state_latch = (u8::from(self.halted) << 4) | (self.state_latch & 0x0F);
            cycles += AVG_CYCLES_PER_STATE;

            if self.halted {
                // Parked in the PROM's zero half: nothing more happens until
                // the next GO.
                return false;
            }
            if self.frame_done {
                // Tempest and Quantum never halt — their list loops forever
                // and the branch back to address 0 delimits the frame. Stop
                // there and report done so the board can flush and re-trigger.
                self.halted = true;
                self.run_cycles = cycles;
                return true;
            }
        }
        false
    }

    /// Run the handler for a dispatched state. Handlers 0-3 latch operands off
    /// the data bus; 4-7 are strobe0-strobe3. The return value is the beam
    /// time the handler consumed, in master-clock cycles.
    fn dispatch(&mut self, handler: u8, color_ram: &[u8; 16]) -> u32 {
        match handler {
            0 => self.latch0(),
            1 => self.latch1(),
            2 => self.latch2(),
            3 => self.latch3(),
            4 => self.strobe0(),
            5 => self.strobe1(),
            6 => self.strobe2(),
            _ => self.strobe3(color_ram),
        }
        .max(0) as u32
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

    // -----------------------------------------------------------------------
    // State handlers
    //
    // Handlers 0-3 latch operands off the data bus and clock the address
    // counter; 4-7 are strobe0-strobe3. Each returns the beam time it
    // consumed, in master-clock cycles (only strobe3 ever charges any).
    // -----------------------------------------------------------------------

    /// Handler 0 (latch0): low byte of DVY.
    ///
    /// Quantum decodes whole words in latch1/latch3, so for it this state only
    /// clocks the address counter.
    fn latch0(&mut self) -> i32 {
        if self.variant != AvgVariant::Quantum {
            self.dvy = (self.dvy & 0x1F00) | self.data;
        }
        self.pc = self.pc.wrapping_add(1);
        0
    }

    /// Handler 1 (latch1): the opcode, DVY bit 12, and the high bits of DVY.
    ///
    /// This is where an instruction begins — the opcode it latches selects the
    /// PROM row that sequences the rest of it.
    fn latch1(&mut self) -> i32 {
        if self.variant == AvgVariant::Quantum {
            self.dvy = self.data & 0x1FFF;
            self.dvy12 = ((self.data >> 12) & 1) as u8;
            self.op = (self.data >> 13) as u8;
        } else {
            self.dvy12 = ((self.data >> 4) & 1) as u8;
            self.op = (self.data >> 5) as u8;
            self.dvy = (u16::from(self.dvy12) << 12) | ((self.data & 0xF) << 8);
        }
        self.int_latch = 0;
        self.dvx = 0;
        self.pc = self.pc.wrapping_add(1);
        0
    }

    /// Handler 2 (latch2): low byte of DVX (Quantum: address counter only).
    fn latch2(&mut self) -> i32 {
        if self.variant != AvgVariant::Quantum {
            self.dvx = (self.dvx & 0x1F00) | self.data;
        }
        self.pc = self.pc.wrapping_add(1);
        0
    }

    /// Handler 3 (latch3): the intensity latch and the high bits of DVX.
    fn latch3(&mut self) -> i32 {
        if self.variant == AvgVariant::Quantum {
            self.int_latch = (self.data >> 12) as u8;
            self.dvx = self.data & 0xFFF;
        } else {
            self.int_latch = (self.data >> 4) as u8;
            self.dvx = (u16::from(self.int_latch & 1) << 12)
                | ((self.data & 0xF) << 8)
                | (self.dvx & 0xFF);
        }
        self.pc = self.pc.wrapping_add(1);
        0
    }

    /// Handler 4 (strobe0): push the return address on a JSR, otherwise
    /// normalize DVX/DVY — shift both axes together until EITHER is normalized
    /// (sign bit differs from the next bit down), loading the timer as it goes.
    ///
    /// Normalization keeps deflection speed roughly constant: the X/Y DACs use
    /// only bits 3-12, so the low three bits must not carry information. The
    /// circuit does not special-case dvx = dvy = 0, in which case it shifts
    /// forever; the count is cut off after 16.
    fn strobe0(&mut self) -> i32 {
        if self.op0() {
            self.stack[(self.sp & 3) as usize] = self.pc;
            return 0;
        }

        if self.variant == AvgVariant::Quantum {
            // Quantum normalizes to 12 bits (sign at bit 11).
            let mut i = 0;
            while (((self.dvy ^ (self.dvy << 1)) & 0x800) == 0)
                && (((self.dvx ^ (self.dvx << 1)) & 0x800) == 0)
                && (i < 16)
            {
                self.dvy = (self.dvy << 1) & 0xFFF;
                self.dvx = (self.dvx << 1) & 0xFFF;
                self.timer >>= 1;
                self.timer |= 0x2000;
                i += 1;
            }
            return 0;
        }

        let op1_bit: u16 = if self.op1() { 0x80 } else { 0 };
        let mut i = 0;
        while (((self.dvy ^ (self.dvy << 1)) & 0x1000) == 0)
            && (((self.dvx ^ (self.dvx << 1)) & 0x1000) == 0)
            && (i < 16)
        {
            self.dvy = (self.dvy & 0x1000) | ((self.dvy << 1) & 0x1FFF);
            self.dvx = (self.dvx & 0x1000) | ((self.dvx << 1) & 0x1FFF);
            self.timer >>= 1;
            self.timer |= 0x4000 | op1_bit;
            i += 1;
        }
        // SVEC counts in the timer's low byte only.
        if self.op1() {
            self.timer &= 0xFF;
        }
        0
    }

    /// Handler 5 (strobe1): binary-scale the timer, or — on the branch opcodes,
    /// which reach this state instead — move the stack pointer.
    fn strobe1(&mut self) -> i32 {
        if self.op2() {
            // JSR/CNTR push, RTS/JMP pop. Only the opcodes whose PROM row
            // actually visits this state see the adjustment.
            self.sp = if self.op1() {
                self.sp.wrapping_sub(1) & 0xF
            } else {
                self.sp.wrapping_add(1) & 0xF
            };
            return 0;
        }

        if self.variant == AvgVariant::Quantum {
            for _ in 0..self.bin_scale {
                self.timer >>= 1;
                self.timer |= 0x2000;
            }
            return 0;
        }

        let op1_bit: u16 = if self.op1() { 0x80 } else { 0 };
        for _ in 0..self.bin_scale {
            self.timer >>= 1;
            self.timer |= 0x4000 | op1_bit;
        }
        if self.op1() {
            self.timer &= 0xFF;
        }
        0
    }

    /// Handler 6 (strobe2): the STAT latches (scale, or color/intensity) and
    /// the branches — JSR/JMP load the address counter, RTS pops it.
    fn strobe2(&mut self) -> i32 {
        if !self.op2() && self.dvy12 == 0 {
            match self.variant {
                // Star Wars latches an 8-bit intensity and a 4-bit color index
                // together, with no 0x800 select bit.
                AvgVariant::StarWars => {
                    self.intensity = (self.dvy & 0xFF) as u8;
                    self.color = ((self.dvy >> 8) & 0xF) as u8;
                }
                // Tempest picks color or intensity on DVY bit 11.
                AvgVariant::Tempest => {
                    if self.dvy & 0x800 != 0 {
                        self.color = (self.dvy & 0xF) as u8;
                    } else {
                        self.intensity = ((self.dvy >> 4) & 0xF) as u8;
                    }
                }
                // Quantum latches color and intensity together, gated on bit 11.
                AvgVariant::Quantum => {
                    if self.dvy & 0x800 != 0 {
                        self.color = (self.dvy & 0xF) as u8;
                        self.intensity = ((self.dvy >> 4) & 0xF) as u8;
                    }
                }
            }
        }

        if self.op2() {
            if self.op0() {
                self.pc = self.dvy << 1;
                // A branch to address 0 restarts the list. Games whose vector
                // list loops forever use it as the frame delimiter.
                if self.dvy == 0 {
                    self.frame_done = true;
                }
            } else {
                self.pc = self.stack[(self.sp & 3) as usize];
            }
        } else if self.dvy12 != 0 {
            self.scale = (self.dvy & 0xFF) as u8;
            self.bin_scale = ((self.dvy >> 8) & 7) as u8;
        }
        0
    }

    /// Handler 7 (strobe3): raise the halt flag on HALT, run the beam for the
    /// timer's remaining count, and emit the resulting point.
    ///
    /// The count is the beam's travel time, so it is also what this state
    /// charges the sequencer — the reason a screen full of long vectors holds
    /// VG_HALT low far longer than the state count alone implies.
    fn strobe3(&mut self, color_ram: &[u8; 16]) -> i32 {
        self.halted = self.op0();

        // CNTR re-centers the beam; the timer still has to run down first.
        if self.op2() {
            let cycles = self.timer_countdown(false);
            self.timer = 0;
            self.xpos = self.xcenter;
            self.ypos = self.ycenter;
            self.add_point(self.xpos, self.ypos, 0, [0, 0, 0]);
            return cycles;
        }
        if self.op0() {
            return 0;
        }

        let cycles = self.timer_countdown(self.op1());
        self.timer = 0;
        match self.variant {
            AvgVariant::Tempest => self.draw_tempest(cycles, color_ram),
            AvgVariant::Quantum => self.draw_quantum(cycles, color_ram),
            AvgVariant::StarWars => self.draw_starwars(cycles),
        }
        cycles
    }

    /// Cycles left on the vector timer: it counts up to its terminal value, so
    /// the remaining count is the distance to it. A short vector (SVEC) counts
    /// in the low byte only; Quantum's timer is 14-bit rather than 15-bit.
    fn timer_countdown(&self, is_short: bool) -> i32 {
        if is_short {
            0x100 - i32::from(self.timer & 0xFF)
        } else if self.variant == AvgVariant::Quantum {
            0x4000 - i32::from(self.timer)
        } else {
            0x8000 - i32::from(self.timer)
        }
    }

    /// Advance the beam by the normalized deltas over `cycles` of travel.
    /// The DACs take the upper 10 bits of the delta (`shift` = 3 for the
    /// 13-bit variants, 2 for Quantum's 12-bit), XOR in the sign, and center
    /// the result on 0.
    fn deflect(&self, cycles: i32, shift: u32) -> (i32, i32) {
        let scale_factor: i32 = i32::from(self.scale) ^ 0xFF;
        let dx = ((i32::from(self.dvx >> shift) ^ i32::from(self.xdac_xor)) - 0x200)
            .wrapping_mul(cycles)
            .wrapping_mul(scale_factor)
            >> 4;
        let dy = ((i32::from(self.dvy >> shift) ^ i32::from(self.ydac_xor)) - 0x200)
            .wrapping_mul(cycles)
            .wrapping_mul(scale_factor)
            >> 4;
        (dx, dy)
    }

    /// Beam position after flipping (set by the game's hardware register).
    fn flipped(&self) -> (i32, i32) {
        let mut x = self.xpos;
        let mut y = self.ypos;
        if self.flip_x {
            x += (self.xcenter - x) << 1;
        }
        if self.flip_y {
            y += (self.ycenter - y) << 1;
        }
        (x, y)
    }

    /// Tempest's strobe3 draw: 13-bit DAC and a 16-entry color RAM lookup.
    fn draw_tempest(&mut self, cycles: i32, color_ram: &[u8; 16]) {
        let (dx, dy) = self.deflect(cycles, 3);
        self.xpos = self.xpos.wrapping_add(dx);
        self.ypos = self.ypos.wrapping_sub(dy);

        // Color RAM holds the four active bits inverted in its low nibble.
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

        // int_latch bits 3:1 == 001 is the DATEA signal, selecting the
        // intensity the STAT strobe stored; otherwise those bits are the
        // intensity directly.
        let eff_intensity = if (self.int_latch >> 1) == 1 {
            self.intensity
        } else {
            self.int_latch & 0xE
        };

        let (x, y) = self.flipped();
        self.add_point(x, y, eff_intensity, [r, g, b]);
    }

    /// Quantum's strobe3 draw: 12-bit DAC and Quantum's color weights —
    /// r = bit3·0xCE, g = bit1·0xAA + bit0·0x54, b = bit2·0xCE.
    fn draw_quantum(&mut self, cycles: i32, color_ram: &[u8; 16]) {
        self.dvx &= 0xFFF;
        self.dvy &= 0xFFF;
        let (dx, dy) = self.deflect(cycles, 2);
        self.xpos = self.xpos.wrapping_add(dx);
        self.ypos = self.ypos.wrapping_sub(dy);

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

        // int_latch == 2 (DATEA) selects the stored STAT intensity.
        let eff_intensity = if self.int_latch == 2 {
            self.intensity
        } else {
            self.int_latch
        };

        // Emit directly in screen space. The Quantum machine presents this as a
        // pre-rotated portrait display (display_size already portrait,
        // Orientation::NORMAL), so no transpose is applied here even though the
        // generator's outputs are wired to a rotated monitor.
        let (x, y) = self.flipped();
        self.add_point(x, y, eff_intensity, [r, g, b]);
    }

    /// Star Wars' strobe3 draw: the same 13-bit position math as Tempest, but a
    /// `color111` color (no color RAM) and a combined intensity.
    fn draw_starwars(&mut self, cycles: i32) {
        let (dx, dy) = self.deflect(cycles, 3);
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

        // Hardware brightness is `((int_latch >> 1) * intensity) >> 3`, an
        // 8-bit value. The renderer's LUT is 4-bit, so shift down by a further
        // 4 (total >> 7) and clamp — preserving relative brightness.
        let brightness = (u32::from(self.int_latch >> 1) * u32::from(self.intensity)) >> 3;
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
        // The sequencer parks between runs, and where it parked decides how
        // the next GO starts. Everything else the handlers latch (op, dvx,
        // dvy, timer, …) is rebuilt from vector memory within a single run.
        w.write_u8(self.state_latch);
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
        self.state_latch = r.read_u8()?;
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

    // --- Sequencer timing ------------------------------------------------

    #[test]
    fn run_cycles_charge_eight_per_sequencer_state() {
        // A bare HALT walks latch1, latch0, strobe3. The halt is sampled on
        // the state that raises it, so it costs the two states before it.
        let vmem = build_vmem(&[0x00, 0x20]); // HALT
        let mut avg = Avg::new(1024, 1024);
        avg.go();
        avg.execute(&vmem, &default_color_ram());
        assert!(avg.is_halted());
        assert_eq!(avg.run_cycles(), 2 * 8);
    }

    #[test]
    fn a_parked_sequencer_costs_an_idle_state_on_the_next_go() {
        let vmem = build_vmem(&[0x00, 0x20]); // HALT
        let mut avg = Avg::new(1024, 1024);
        avg.go();
        avg.execute(&vmem, &default_color_ram());
        let first = avg.run_cycles();

        avg.go();
        avg.execute(&vmem, &default_color_ram());
        // GO clears the halt flag but not the state latch, so the second run
        // still addresses the PROM's parked (halted) half once before it
        // latches an opcode — one idle state the first run never paid for.
        assert_eq!(avg.run_cycles(), first + 8);
    }

    #[test]
    fn a_loaded_prom_replaces_the_built_in_sequence() {
        // Splice an extra idle state into HALT's chain: 9 -> 8 -> 3 -> F
        // instead of 9 -> 8 -> F. The generator should take exactly one state
        // longer to raise the halt.
        let mut prom = default_state_prom();
        prom[0x98] = 3; // after latch0, wait
        prom[0x93] = 0xF; // then strobe3

        let vmem = build_vmem(&[0x00, 0x20]); // HALT
        let mut avg = Avg::new(1024, 1024);
        avg.load_state_prom(&prom);
        avg.go();
        avg.execute(&vmem, &default_color_ram());
        assert!(avg.is_halted());
        assert_eq!(avg.run_cycles(), 3 * 8);
    }

    #[test]
    fn drawing_charges_the_beam_its_travel_time() {
        // VCTR (DVY = DVX = 0x200) then HALT. Normalization shifts twice,
        // leaving timer = 0x6000, so the beam runs 0x8000 - 0x6000 cycles.
        // The two instructions walk 11 states, 10 of them before the halt.
        let vmem = build_vmem(&[
            0x00, 0x02, 0x00, 0x82, // VCTR: DVY=0x200, DVX=0x200, intensity=8
            0x00, 0x20, // HALT
        ]);
        let mut avg = Avg::new(1024, 1024);
        avg.go();
        avg.execute(&vmem, &default_color_ram());
        assert_eq!(avg.run_cycles(), 10 * 8 + 0x2000);
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
