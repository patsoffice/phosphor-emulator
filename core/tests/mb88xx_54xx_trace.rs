//! This core running the real Namco 54XX firmware, traced in the same format
//! as `cross-validation/trace_54xx`, so the two can be diffed line for line.
//!
//! The per-opcode vectors in `validate_mb88xx` cannot reach interrupt entry,
//! the input ports or timing, and both of the bugs that made Galaga's
//! explosion wrong lived there. This is how they were found: run both cores on
//! the same firmware with the same command stream and look for the first line
//! where they disagree.
//!
//! ```text
//! ROM54=54xx.bin TRACE_OUT=ours.txt CMDS="30 40 00 02 DF" \
//!     cargo test -p phosphor-core --test mb88xx_54xx_trace
//! cross-validation/bin/trace_54xx 54xx.bin 24000 30 40 00 02 DF > ref.txt
//! diff <(cut -d' ' -f2-6 ours.txt) <(cut -d' ' -f2-6 ref.txt)
//! ```
//!
//! **Compare fields 2 onward, not the whole line.** The leading number counts
//! machine cycles and the two harnesses schedule on it differently; everything
//! after it is CPU state and must match exactly.
//!
//! Skips unless `ROM54` names a 54XX image, because the firmware is not
//! redistributable.

use phosphor_core::device::namco54::Namco54Lle;

#[test]
fn trace() {
    let Ok(rom_path) = std::env::var("ROM54") else {
        eprintln!("skipping: set ROM54=/path/to/54xx.bin to trace");
        return;
    };
    let cycles: usize = std::env::var("CYCLES")
        .unwrap_or_else(|_| "12000".into())
        .parse()
        .unwrap();
    let spacing: usize = std::env::var("CMD_SPACING")
        .unwrap_or_else(|_| "400".into())
        .parse()
        .unwrap();
    let hold: usize = std::env::var("CS_HOLD")
        .unwrap_or_else(|_| "40".into())
        .parse()
        .unwrap();
    let cmds: Vec<u8> = std::env::var("CMDS")
        .unwrap_or_default()
        .split_whitespace()
        .map(|s| u8::from_str_radix(s, 16).unwrap())
        .collect();

    let out_path = std::env::var("TRACE_OUT").unwrap_or_else(|_| "54xx-trace.txt".into());
    let mut out = String::new();

    let rom = std::fs::read(rom_path).unwrap();
    let mut c = Namco54Lle::new();
    c.load_rom(&rom);

    let mut next = 0usize;
    let mut next_at = 200usize;
    let mut cs_until: Option<usize> = None;

    for cycle in 0..cycles {
        if next < cmds.len() && cycle == next_at {
            // The device holds and releases the interrupt line itself, the way
            // the board does; `hold` is only here to keep the two harnesses'
            // command schedules comparable.
            c.write(cmds[next]);
            cs_until = Some(cycle + hold);
            next_at = cycle + spacing;
            out.push_str(&format!("{cycle} CMD {:02X}\n", cmds[next]));
            next += 1;
        }
        if cs_until == Some(cycle) {
            cs_until = None;
        }

        // Log at instruction starts only. The reference harness steps whole
        // instructions where this steps machine cycles, so logging every cycle
        // makes a two-cycle instruction look like a divergence.
        let boundary = c.mcu.at_instruction_boundary();
        let addr = ((c.mcu.pa as u16) << 6) | (c.mcu.pc as u16 & 0x3F);
        let (a, y, st) = (c.mcu.a, c.mcu.y, c.mcu.st);
        let before = c.channels();
        c.tick();
        let after = c.channels();
        if !boundary {
            continue;
        }

        let mut note = String::new();
        for (i, (b, a2)) in before.iter().zip(&after).enumerate() {
            if b != a2 {
                note.push_str(&format!(" ch{}<-{:X}", i + 1, a2));
            }
        }
        out.push_str(&format!(
            "{cycle} {addr:04X} A={a:X} Y={y:X} ST={st} PIO={:X} IRQ={}{note}\n",
            c.mcu.pio, c.mcu.irq_pin
        ));
    }
    std::fs::write(&out_path, out).unwrap();
    eprintln!("channels: {:X?}", c.channels());
}
