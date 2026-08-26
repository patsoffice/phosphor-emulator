//! Minimal Zilog Z80 DMA controller — just enough for the Nintendo Mario Bros.
//! (and dkong3) sprite-list transfer.
//!
//! Mario Bros. wires a Z80 DMA chip at I/O port 0x00. Each frame the game
//! programs it for a memory-to-memory block copy (work RAM → sprite RAM) and
//! triggers the transfer through the LS259 `DMA SET` line (mainlatch Q5 → the
//! DMA `RDY` input). The real chip has a complex register-file protocol with
//! variable-length "follow byte" sequences; this implements that decoder
//! faithfully (mirroring MAME's `z80dma.cpp` register layout) but models only
//! the memory-to-memory transfer path — the only mode the Nintendo drivers use.
//!
//! Reference: MAME `src/devices/machine/z80dma.cpp`, Zilog Z80 DMA datasheet.

use phosphor_macros::Saveable;

/// A resolved memory-to-memory block transfer, produced by
/// [`Z80Dma::take_transfer`] once the chip is loaded, enabled and ready.
///
/// The chip transfers `count + 1` bytes total (the block-length register holds
/// "bytes minus one").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DmaTransfer {
    pub src: u16,
    pub dst: u16,
    pub count: u16,
    pub src_step: i16,
    pub dst_step: i16,
}

/// Destination for the next "follow" byte after a base-register write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Follow {
    PortAAddrL,
    PortAAddrH,
    BlockLenL,
    BlockLenH,
    PortBAddrL,
    PortBAddrH,
    /// Consumed but unused (port timing, mask/match, interrupt control, …).
    Scratch,
}

// Z80 DMA WR6 command bytes (subset used by the Nintendo drivers).
const CMD_RESET: u8 = 0xc3;
const CMD_LOAD: u8 = 0xcf;
const CMD_CONTINUE: u8 = 0xd3;
const CMD_DISABLE_DMA: u8 = 0x83;
const CMD_ENABLE_DMA: u8 = 0x87;
const CMD_FORCE_READY: u8 = 0xb3;

/// Minimal Z80 DMA controller (memory-to-memory only).
#[derive(Saveable)]
#[save_version(1)]
#[save_tlv]
pub struct Z80Dma {
    /// Base registers WR0..=WR6.
    #[save(id = 1)]
    wr: [u8; 7],

    // Programmable "follow" registers we actually use.
    #[save(id = 2)]
    porta_l: u8,
    #[save(id = 3)]
    porta_h: u8,
    #[save(id = 4)]
    portb_l: u8,
    #[save(id = 5)]
    portb_h: u8,
    #[save(id = 6)]
    blocklen_l: u8,
    #[save(id = 7)]
    blocklen_h: u8,

    // Follow-byte routing state machine: transient mid-program state, so a load
    // lands between byte sequences rather than part way through one.
    #[save_skip]
    follow: [Follow; 6],
    #[save_skip(default)]
    num_follow: u8,
    #[save_skip(default)]
    cur_follow: u8,

    // Transfer state latched by COMMAND_LOAD / COMMAND_CONTINUE.
    #[save(id = 8)]
    address_a: u16,
    #[save(id = 9)]
    address_b: u16,
    #[save(id = 10)]
    count: u16,
    #[save(id = 11)]
    armed: bool,

    #[save(id = 12)]
    enabled: bool,
    #[save(id = 13)]
    force_ready: bool,
    #[save(id = 14)]
    rdy_line: bool,
}

impl Default for Z80Dma {
    fn default() -> Self {
        Self::new()
    }
}

impl Z80Dma {
    pub fn new() -> Self {
        Self {
            wr: [0; 7],
            porta_l: 0,
            porta_h: 0,
            portb_l: 0,
            portb_h: 0,
            blocklen_l: 0,
            blocklen_h: 0,
            follow: [Follow::Scratch; 6],
            num_follow: 0,
            cur_follow: 0,
            address_a: 0,
            address_b: 0,
            count: 0,
            armed: false,
            enabled: false,
            force_ready: false,
            rdy_line: false,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::new();
    }

    fn push_follow(&mut self, target: Follow) {
        if (self.num_follow as usize) < self.follow.len() {
            self.follow[self.num_follow as usize] = target;
            self.num_follow += 1;
        }
    }

    /// Write a byte to the DMA register file (I/O port 0x00 write).
    pub fn write(&mut self, data: u8) {
        if self.num_follow == 0 {
            self.write_base_register(data);
            self.cur_follow = 0;
        } else {
            let target = self.follow[self.cur_follow as usize];
            match target {
                Follow::PortAAddrL => self.porta_l = data,
                Follow::PortAAddrH => self.porta_h = data,
                Follow::BlockLenL => self.blocklen_l = data,
                Follow::BlockLenH => self.blocklen_h = data,
                Follow::PortBAddrL => self.portb_l = data,
                Follow::PortBAddrH => self.portb_h = data,
                Follow::Scratch => {}
            }
            self.cur_follow += 1;
            if self.cur_follow >= self.num_follow {
                self.num_follow = 0;
            }
        }
    }

    fn write_base_register(&mut self, data: u8) {
        if data & 0x87 == 0x00 {
            // WR2 — port B configuration
            self.wr[2] = data;
            if data & 0x40 != 0 {
                self.push_follow(Follow::Scratch); // port B timing
            }
        } else if data & 0x87 == 0x04 {
            // WR1 — port A configuration
            self.wr[1] = data;
            if data & 0x40 != 0 {
                self.push_follow(Follow::Scratch); // port A timing
            }
        } else if data & 0x80 == 0x00 {
            // WR0 — transfer direction/mode + port A address + block length
            self.wr[0] = data;
            if data & 0x08 != 0 {
                self.push_follow(Follow::PortAAddrL);
            }
            if data & 0x10 != 0 {
                self.push_follow(Follow::PortAAddrH);
            }
            if data & 0x20 != 0 {
                self.push_follow(Follow::BlockLenL);
            }
            if data & 0x40 != 0 {
                self.push_follow(Follow::BlockLenH);
            }
        } else if data & 0x83 == 0x80 {
            // WR3 — interrupt/mask/match (bit 6 also enables the DMA)
            self.wr[3] = data;
            if data & 0x08 != 0 {
                self.push_follow(Follow::Scratch); // mask byte
            }
            if data & 0x10 != 0 {
                self.push_follow(Follow::Scratch); // match byte
            }
            if data & 0x40 != 0 {
                self.enabled = true;
            }
        } else if data & 0x83 == 0x81 {
            // WR4 — operating mode + port B address + interrupt control
            self.wr[4] = data;
            if data & 0x04 != 0 {
                self.push_follow(Follow::PortBAddrL);
            }
            if data & 0x08 != 0 {
                self.push_follow(Follow::PortBAddrH);
            }
            if data & 0x10 != 0 {
                self.push_follow(Follow::Scratch); // interrupt control
            }
        } else if data & 0xc7 == 0x82 {
            // WR5 — ready/auto-restart configuration
            self.wr[5] = data;
        } else if data & 0x83 == 0x83 {
            // WR6 — command
            self.wr[6] = data;
            self.command(data);
        }
    }

    fn command(&mut self, cmd: u8) {
        match cmd {
            CMD_RESET => {
                self.enabled = false;
                self.force_ready = false;
                self.armed = false;
            }
            CMD_LOAD => {
                self.address_a = u16::from_le_bytes([self.porta_l, self.porta_h]);
                self.address_b = u16::from_le_bytes([self.portb_l, self.portb_h]);
                self.count = u16::from_le_bytes([self.blocklen_l, self.blocklen_h]);
                self.force_ready = false;
                self.armed = true;
            }
            CMD_CONTINUE => {
                self.count = u16::from_le_bytes([self.blocklen_l, self.blocklen_h]);
                self.armed = true;
            }
            CMD_ENABLE_DMA => self.enabled = true,
            CMD_DISABLE_DMA => self.enabled = false,
            CMD_FORCE_READY => self.force_ready = true,
            _ => {}
        }
    }

    /// Register read (I/O port 0x00 read). Minimal status byte.
    pub fn read(&self) -> u8 {
        0
    }

    /// Drive the external `RDY` line (Mario: LS259 Q5 → DMA RDY).
    pub fn set_rdy(&mut self, active: bool) {
        self.rdy_line = active;
    }

    fn ready_active_high(&self) -> bool {
        (self.wr[5] >> 3) & 0x01 != 0
    }

    fn is_ready(&self) -> bool {
        self.force_ready || (self.rdy_line == self.ready_active_high())
    }

    fn porta_is_source(&self) -> bool {
        (self.wr[0] >> 2) & 0x01 != 0
    }

    /// Address step for a port given its WR1/WR2 configuration byte:
    /// fixed (bit 5) → 0, else increment (bit 4) → +1, else decrement → -1.
    fn port_step(cfg: u8) -> i16 {
        if cfg & 0x20 != 0 {
            0
        } else if cfg & 0x10 != 0 {
            1
        } else {
            -1
        }
    }

    /// True when a transfer is loaded, the DMA is enabled, and the ready line
    /// is asserted — i.e. the block copy should run now.
    pub fn transfer_pending(&self) -> bool {
        self.enabled && self.armed && self.is_ready()
    }

    /// Consume the pending transfer, returning its resolved parameters. The
    /// DMA disables itself at end-of-block (matching one-shot byte/continuous
    /// behavior), so the caller performs the copy exactly once.
    pub fn take_transfer(&mut self) -> Option<DmaTransfer> {
        if !self.transfer_pending() {
            return None;
        }
        let porta_src = self.porta_is_source();
        let (src, dst, src_cfg, dst_cfg) = if porta_src {
            (self.address_a, self.address_b, self.wr[1], self.wr[2])
        } else {
            (self.address_b, self.address_a, self.wr[2], self.wr[1])
        };
        self.armed = false;
        self.enabled = false;
        Some(DmaTransfer {
            src,
            dst,
            count: self.count,
            src_step: Self::port_step(src_cfg),
            dst_step: Self::port_step(dst_cfg),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::save_state::{Saveable as _, StateReader, StateWriter};
    /// Program a memory-to-memory transfer: port A (source) 0x6900 incrementing,
    /// port B (dest) 0x7000 incrementing, length 4 bytes (block-len reg = 3),
    /// active-high ready, then LOAD + ENABLE.
    fn program_mem_to_mem(dma: &mut Z80Dma, src: u16, dst: u16, len_minus_1: u16) {
        // WR0: transfer (D1D0=01), port A is source (bit2=1),
        //      follow with port A addr L/H (bits 3,4) and block len L/H (bits 5,6).
        dma.write(0b0111_1101);
        dma.write((src & 0xff) as u8);
        dma.write((src >> 8) as u8);
        dma.write((len_minus_1 & 0xff) as u8);
        dma.write((len_minus_1 >> 8) as u8);

        // WR1: port A is memory (bit3=0), address increments (bit4=1). base 0x04.
        dma.write(0b0001_0100);
        // WR2: port B is memory, address increments. base 0x00.
        dma.write(0b0001_0000);
        // WR4: continuous mode (bits 6,5 = 01), port B addr L/H follow (bits 2,3). base 0x81.
        dma.write(0b1010_1101);
        dma.write((dst & 0xff) as u8);
        dma.write((dst >> 8) as u8);
        // WR5: ready active high (bit3=1). base 0x82.
        dma.write(0b1000_1010);

        dma.write(CMD_LOAD);
        dma.write(CMD_ENABLE_DMA);
    }

    #[test]
    fn decodes_and_resolves_mem_to_mem_transfer() {
        let mut dma = Z80Dma::new();
        program_mem_to_mem(&mut dma, 0x6900, 0x7000, 0x003);

        assert!(!dma.transfer_pending(), "not ready until RDY asserted");
        dma.set_rdy(true);
        assert!(dma.transfer_pending());

        let t = dma.take_transfer().expect("transfer resolved");
        assert_eq!(
            t,
            DmaTransfer {
                src: 0x6900,
                dst: 0x7000,
                count: 0x0003,
                src_step: 1,
                dst_step: 1,
            }
        );
        // One-shot: disabled after taking the block.
        assert!(!dma.transfer_pending());
        assert!(dma.take_transfer().is_none());
    }

    #[test]
    fn executes_block_copy_against_memory() {
        let mut mem = vec![0u8; 0x10000];
        for (i, b) in mem.iter_mut().enumerate().take(0x6905).skip(0x6900) {
            *b = (0xA0 + (i - 0x6900)) as u8;
        }

        let mut dma = Z80Dma::new();
        program_mem_to_mem(&mut dma, 0x6900, 0x7000, 0x0004); // 5 bytes
        dma.set_rdy(true);

        let t = dma.take_transfer().unwrap();
        let mut src = t.src;
        let mut dst = t.dst;
        for _ in 0..=t.count {
            let v = mem[src as usize];
            mem[dst as usize] = v;
            src = src.wrapping_add_signed(t.src_step);
            dst = dst.wrapping_add_signed(t.dst_step);
        }

        assert_eq!(&mem[0x7000..0x7005], &[0xA0, 0xA1, 0xA2, 0xA3, 0xA4]);
    }

    #[test]
    fn disable_command_blocks_transfer() {
        let mut dma = Z80Dma::new();
        program_mem_to_mem(&mut dma, 0x6900, 0x7000, 0x003);
        dma.write(CMD_DISABLE_DMA);
        dma.set_rdy(true);
        assert!(!dma.transfer_pending());
    }

    #[test]
    fn save_load_round_trip() {
        let mut dma = Z80Dma::new();
        program_mem_to_mem(&mut dma, 0x1234, 0x5678, 0x000A);

        let mut w = StateWriter::new();
        dma.save_state(&mut w);
        let bytes = w.into_vec();

        let mut dma2 = Z80Dma::new();
        let mut r = StateReader::new(&bytes);
        dma2.load_state(&mut r).unwrap();

        dma2.set_rdy(true);
        let t = dma2.take_transfer().unwrap();
        assert_eq!(t.src, 0x1234);
        assert_eq!(t.dst, 0x5678);
        assert_eq!(t.count, 0x000A);
    }
}
