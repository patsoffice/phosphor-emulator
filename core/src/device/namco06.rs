use phosphor_macros::Saveable;

/// Namco 06XX custom chip — bus arbiter and NMI timer.
///
/// Multiplexes access to up to 4 custom I/O chips (51XX, 53XX, etc.)
/// and generates periodic NMI to the controlling CPU based on a
/// programmable clock divider.
///
/// Control register (written at SEL=1):
///   bits 0-3: chip select (active high, one per custom chip)
///   bit 4:    R/W direction (1 = read from chips, 0 = write to chips)
///   bits 5-7: clock divider (0 = timer stopped, else divides by 1<<N)
#[derive(Saveable)]
#[save_version(1)]
pub struct Namco06 {
    control: u8,
    nmi_pending: bool,
    timer_counter: u32,
    timer_period: u32,
    timer_running: bool,
    read_stretch: bool,
    timer_state: bool,
    /// Countdown for chip_select assertion delay. When the timer toggles to
    /// the active phase, chip_select waits this many CPU cycles before
    /// asserting. Compensates for MAME's timeslice scheduling which naturally
    /// gives the Z80 time to process its NMI and write data before the MCU
    /// processes its IRQ.
    chip_select_delay: u32,
    /// CPU cycles per 06XX base clock tick (typically 64 = CPU_CLK / 06XX_CLK).
    #[save_skip]
    base_divisor: u32,
}

impl Namco06 {
    pub fn new(base_divisor: u32) -> Self {
        Self {
            control: 0,
            nmi_pending: false,
            timer_counter: 0,
            timer_period: 0,
            timer_running: false,
            read_stretch: false,
            timer_state: false,
            chip_select_delay: 0,
            base_divisor,
        }
    }

    /// Read the control register.
    pub fn ctrl_read(&self) -> u8 {
        self.control
    }

    /// Write the control register. Starts or stops the NMI timer based on
    /// the clock divider bits (5-7). `cpu_clock` is the current CPU cycle
    /// count, used to align the initial delay to the next 06XX base clock tick.
    pub fn ctrl_write(&mut self, data: u8, cpu_clock: u64) {
        self.control = data;
        let num_shifts = (data >> 5) & 7;

        if num_shifts == 0 {
            // Divider zero: stop timer. Reset timer_state and clear NMI,
            // matching MAME's ctrl_w_sync which resets m_timer_state=false
            // and calls set_nmi(CLEAR_LINE).
            self.timer_running = false;
            self.timer_state = false;
            self.nmi_pending = false;
        } else {
            // Compute timer half-period in CPU cycles.
            // MAME: attotime::from_hz(clock() / divisor) / 2
            // = (1 << num_shifts) * base_divisor / 2 CPU cycles per toggle.
            let half_period = (self.base_divisor * (1 << num_shifts)) / 2;
            self.timer_period = half_period;

            // Initial delay: align to the next 06XX base clock tick.
            // MAME: from_ticks(total_ticks + 1, clock()) - now
            // This always advances to the NEXT clock edge (1-64 CPU cycles).
            let base = self.base_divisor as u64;
            let initial_delay = (base - (cpu_clock % base)) as u32;
            self.timer_counter = initial_delay;
            self.timer_running = true;

            if data & 0x10 != 0 {
                // Read mode: suppress the first NMI pulse.
                self.nmi_pending = false;
                self.read_stretch = true;
            } else {
                self.read_stretch = false;
            }
        }
    }

    /// Returns true if chip N (0-3) is selected.
    pub fn chip_select(&self, n: u8) -> bool {
        self.control & (1 << n) != 0
    }

    /// Returns true if the control register is in read mode (bit 4 set).
    pub fn is_read_mode(&self) -> bool {
        self.control & 0x10 != 0
    }

    /// Advance the timer by one CPU cycle. Call every CPU cycle.
    ///
    /// The NMI output is a **level signal** that follows the two-phase timer,
    /// matching MAME's `nmi_generate` callback which calls `set_nmi(ASSERT)`
    /// on the falling edge (timer_state true) and `set_nmi(CLEAR)` on the
    /// rising edge. The board code uses the Z80's edge detector to convert
    /// this level into discrete NMI events.
    pub fn tick(&mut self) {
        if !self.timer_running {
            return;
        }

        // Count down chip_select delay from previous toggle
        self.chip_select_delay = self.chip_select_delay.saturating_sub(1);

        self.timer_counter = self.timer_counter.saturating_sub(1);
        if self.timer_counter == 0 {
            self.timer_counter = self.timer_period;
            self.timer_state = !self.timer_state;

            // Drive NMI output level on every toggle:
            //   falling edge (timer_state true):  ASSERT (unless read_stretch)
            //   rising edge  (timer_state false): CLEAR
            // This matches MAME's nmi_generate exactly.
            self.nmi_pending = self.timer_state && !self.read_stretch;
            self.read_stretch = false;

            // Delay chip_select assertion by one 06XX tick after NMI fires.
            // In MAME, both signals fire simultaneously but the timeslice
            // scheduler gives the Z80 priority, so it processes NMI and writes
            // command data before the MCU processes its IRQ. In our per-cycle
            // model, the MCU wins the race. This delay gives the Z80 one
            // 06XX tick (~64 CPU cycles) of head start.
            if self.timer_state {
                self.chip_select_delay = self.base_divisor;
            }
        }
    }

    /// Returns the current NMI output level (true = asserted).
    ///
    /// This is a level signal, not consumed on read — the Z80's rising-edge
    /// detector handles edge detection. The board code should only propagate
    /// this level to the CPU when it is not halted, matching MAME's
    /// `set_nmi()` which skips suspended CPUs.
    pub fn nmi_output(&self) -> bool {
        self.nmi_pending
    }

    /// Returns true if chip N is selected AND timer is in the active phase
    /// AND the chip_select propagation delay has elapsed.
    pub fn chip_select_active(&self, n: u8) -> bool {
        self.control & (1 << n) != 0 && self.timer_state && self.chip_select_delay == 0
    }

    // Debug accessors
    pub fn timer_running(&self) -> bool {
        self.timer_running
    }
    pub fn timer_counter(&self) -> u32 {
        self.timer_counter
    }
    pub fn timer_period(&self) -> u32 {
        self.timer_period
    }
    pub fn timer_state(&self) -> bool {
        self.timer_state
    }
    pub fn read_stretch(&self) -> bool {
        self.read_stretch
    }

    pub fn reset(&mut self) {
        self.control = 0;
        self.nmi_pending = false;
        self.timer_counter = 0;
        self.timer_period = 0;
        self.timer_running = false;
        self.read_stretch = false;
        self.timer_state = false;
        self.chip_select_delay = 0;
    }
}

impl super::Device for Namco06 {
    fn name(&self) -> &'static str {
        "Namco 06XX"
    }
    fn reset(&mut self) {
        self.reset();
    }
}

use crate::core::debug::{DebugRegister, Debuggable};

impl Debuggable for Namco06 {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "CTRL",
                value: self.control as u64,
                width: 8,
            },
            DebugRegister {
                name: "NMI",
                value: self.nmi_pending as u64,
                width: 1,
            },
            DebugRegister {
                name: "TIMER",
                value: self.timer_counter as u64,
                width: 16,
            },
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 06XX on the Namco boards divides a 1.536 MHz clock, which is 64
    /// CPU cycles per base tick at 3.072 MHz.
    const DIV: u32 = 64;

    fn chip() -> Namco06 {
        Namco06::new(DIV)
    }

    // --- The control register --------------------------------------------

    #[test]
    fn the_control_register_reads_back_what_was_written() {
        let mut c = chip();
        c.ctrl_write(0x5B, 0);
        assert_eq!(c.ctrl_read(), 0x5B);
    }

    #[test]
    fn the_low_four_bits_select_chips_independently() {
        let mut c = chip();
        for n in 0..4u8 {
            c.ctrl_write(1 << n, 0);
            for m in 0..4u8 {
                assert_eq!(c.chip_select(m), m == n, "select {n}, asked about {m}");
            }
        }
        // More than one at a time is legal: the 06XX broadcasts to each
        // selected chip rather than arbitrating between them.
        c.ctrl_write(0b1010, 0);
        assert!(c.chip_select(1) && c.chip_select(3));
        assert!(!c.chip_select(0) && !c.chip_select(2));
    }

    #[test]
    fn bit_four_is_the_read_direction() {
        let mut c = chip();
        c.ctrl_write(0x01, 0);
        assert!(!c.is_read_mode());
        c.ctrl_write(0x11, 0);
        assert!(c.is_read_mode());
    }

    // --- The timer -------------------------------------------------------

    #[test]
    fn a_zero_divider_stops_the_timer_and_drops_nmi() {
        // The divider field doubles as the run control, so writing zero to it
        // is how a board turns the NMI source off. It has to clear the output
        // as well: a timer stopped with NMI still asserted would leave the
        // CPU taking an interrupt that nothing will ever release.
        let mut c = chip();
        c.ctrl_write(0x20, 0);
        for _ in 0..DIV * 4 {
            c.tick();
        }
        assert!(c.timer_running());

        c.ctrl_write(0x00, 0);
        assert!(!c.timer_running());
        assert!(!c.nmi_output(), "stopping the timer left NMI asserted");
        assert!(!c.timer_state());
    }

    #[test]
    fn a_stopped_timer_does_not_advance() {
        let mut c = chip();
        c.ctrl_write(0x00, 0);
        let before = c.timer_counter();
        for _ in 0..1000 {
            c.tick();
        }
        assert_eq!(c.timer_counter(), before);
        assert!(!c.nmi_output());
    }

    #[test]
    fn the_divider_field_sets_the_half_period() {
        // Each step of the field doubles the period, and the stored value is a
        // half period because the output toggles rather than pulsing.
        let mut c = chip();
        for shifts in 1..8u8 {
            c.ctrl_write(shifts << 5, 0);
            assert_eq!(
                c.timer_period(),
                (DIV * (1 << shifts)) / 2,
                "divider field {shifts}"
            );
        }
    }

    #[test]
    fn the_first_tick_is_aligned_to_the_next_base_clock_edge() {
        // The 06XX counts its own clock, not the CPU's, so a control write
        // part way through a base period waits only for the remainder. Writing
        // exactly on an edge waits a whole period rather than firing at once.
        let mut c = chip();
        c.ctrl_write(0x20, 0);
        assert_eq!(c.timer_counter(), DIV, "on an edge, a full period");
        c.ctrl_write(0x20, 1);
        assert_eq!(c.timer_counter(), DIV - 1, "one cycle in, one less to wait");
        c.ctrl_write(0x20, u64::from(DIV) - 1);
        assert_eq!(c.timer_counter(), 1, "one cycle short of the edge");
    }

    #[test]
    fn the_nmi_output_follows_the_two_phase_timer() {
        // It is a level, not a pulse: asserted through the active half of the
        // cycle and released through the other. The board's edge detector is
        // what turns it into discrete interrupts.
        let mut c = chip();
        c.ctrl_write(0x20, 0);
        let half = c.timer_period();

        // The alignment tick, then the first toggle into the active phase.
        for _ in 0..DIV {
            c.tick();
        }
        assert!(c.timer_state(), "the first toggle enters the active phase");
        assert!(c.nmi_output());

        for _ in 0..half {
            c.tick();
        }
        assert!(!c.timer_state(), "the second toggle leaves it");
        assert!(!c.nmi_output(), "NMI is released on the other phase");

        for _ in 0..half {
            c.tick();
        }
        assert!(c.nmi_output(), "and asserted again on the next");
    }

    #[test]
    fn read_mode_suppresses_only_the_first_nmi() {
        // Setting up a read arms the timer without wanting the interrupt that
        // the first toggle would otherwise raise. Suppressing every one of
        // them instead would stop the transfer after a single byte.
        let mut c = chip();
        c.ctrl_write(0x30, 0); // read mode, divider 1
        assert!(c.read_stretch());
        let half = c.timer_period();

        for _ in 0..DIV {
            c.tick();
        }
        assert!(c.timer_state(), "the timer still toggled");
        assert!(!c.nmi_output(), "but the first NMI was suppressed");
        assert!(!c.read_stretch(), "and the suppression is spent");

        for _ in 0..half * 2 {
            c.tick();
        }
        assert!(c.nmi_output(), "the second one is not suppressed");
    }

    #[test]
    fn chip_select_waits_a_base_tick_behind_the_nmi() {
        // Both fire together on the board. Our per-cycle model would let the
        // MCU see its select before the Z80 has serviced the NMI and written
        // the command, so the select is held off for one 06XX tick to give the
        // Z80 the head start the real timeslice scheduler gives it.
        let mut c = chip();
        c.ctrl_write(0x41, 0); // chip 0 selected, divider field 2
        assert_eq!(c.timer_period(), DIV * 2, "a half period of two base ticks");

        for _ in 0..DIV {
            c.tick();
        }
        assert!(c.nmi_output(), "NMI is up");
        assert!(!c.chip_select_active(0), "the select is still held off");

        for _ in 0..DIV {
            c.tick();
        }
        assert!(c.chip_select_active(0), "and arrives a base tick later");
        // A chip that was never selected stays unselected throughout.
        assert!(!c.chip_select_active(1));
    }

    #[test]
    fn chip_select_is_inactive_through_the_idle_phase() {
        let mut c = chip();
        c.ctrl_write(0x41, 0);
        for _ in 0..DIV * 2 {
            c.tick();
        }
        assert!(c.chip_select_active(0));
        for _ in 0..DIV * 2 {
            c.tick();
        }
        assert!(
            !c.chip_select_active(0),
            "the select follows the active phase, not the selection bit alone"
        );
    }

    #[test]
    fn at_the_fastest_divider_the_head_start_consumes_the_whole_active_phase() {
        // THIS DOCUMENTS CURRENT BEHAVIOR AND IS NOT AN ENDORSEMENT OF IT.
        //
        // The chip-select head start is one base tick, and at divider field 1
        // the whole active phase is also one base tick, so the delay expires on
        // the same cycle the phase ends and `chip_select_active` is never true.
        // A board that drove the 06XX at its fastest divider would therefore
        // never see a select at all.
        //
        // That is a property of the compensation rather than of the part: on
        // hardware the select and the NMI assert together, and the delay exists
        // only because our per-cycle model would otherwise let the MCU win a
        // race the real timeslice scheduler gives to the Z80.
        //
        // **No game reaches this setting**, so the starved phase is unreachable
        // and this stays as it is; see phosphor-emulator-hszj for the survey.
        // The 06XX is on the Galaga board alone, so the machines are Galaga, Dig
        // Dug and Xevious, and across their committed movies (8,115 frames of
        // attract, a coin and real play) all 16,934 control writes carried a
        // divider field of 0, 3, 5, 6 or 7. Fields 1 and 2 were never written.
        // The values are the same handful every time: 0x10 to stop the timer,
        // 0x71 for the 51XX at field 3, 0xD2 for the 53XX at field 6.
        //
        // Field 3 is therefore the fastest divider any game uses, and there the
        // active phase is four base ticks against a one-tick head start, so 192
        // of its 256 cycles carry a select.
        let mut c = chip();
        c.ctrl_write(0x21, 0); // chip 0, divider field 1
        assert_eq!(c.timer_period(), DIV, "the active phase is one base tick");
        for _ in 0..DIV * 4 {
            c.tick();
            assert!(
                !c.chip_select_active(0),
                "a select appeared at the fastest divider, so this test is \
                 stale and the behavior it documents has changed"
            );
        }
        // The NMI still works; it is only the select that is starved.
        assert!(c.timer_running());
    }

    // --- Reset ------------------------------------------------------------

    #[test]
    fn reset_stops_the_timer_and_clears_every_output() {
        let mut c = chip();
        c.ctrl_write(0x3F, 0);
        for _ in 0..DIV * 3 {
            c.tick();
        }
        assert!(c.timer_running());

        c.reset();
        assert_eq!(c.ctrl_read(), 0);
        assert!(!c.timer_running());
        assert!(!c.nmi_output());
        assert!(!c.timer_state());
        assert!(!c.read_stretch());
        assert_eq!(c.timer_counter(), 0);
        assert_eq!(c.timer_period(), 0);
        for n in 0..4u8 {
            assert!(!c.chip_select(n), "chip {n} still selected after reset");
        }
    }
}
