//! Every board's `TimingConfig` really does follow from the crystals it
//! declares.
//!
//! `TimingConfig` stores leaf rates: a CPU clock in hertz and a scanline in
//! whole CPU cycles. Where those come from is a comment. On the boards whose
//! CPU and video clocks sit on *different* crystals, `cycles_per_scanline` is
//! not a hardware constant at all but a conversion between two oscillators,
//! rounded to a whole number of cycles by hand, differently in each file, and
//! checked by nothing.
//!
//! Every registered machine now also declares a [`ClockTree`], and this file is
//! what makes that declaration load-bearing: the stored leaves have to agree
//! with the declared crystals, and each board has to state the rounding error
//! it accepts rather than leaving it in prose.
//!
//! Everything iterates `registry::all()`, so a newly registered machine is
//! covered without touching this file. It is a sibling of
//! `machine_contract_test.rs`, which pins the rest of what a live machine owes
//! the frontend.

use phosphor_core::core::ClockDomainName as Clk;
use phosphor_core::core::machine::ClockDeclaration;
use phosphor_machines::registry;

/// Guard against a vacuous suite: every test here iterates `registry::all()`,
/// so an empty registry would make all of them pass while checking nothing.
#[test]
fn the_registry_is_not_empty() {
    assert!(
        registry::all().len() > 30,
        "registry has {} machines, and the checks below iterate it, so they \
         would pass vacuously",
        registry::all().len()
    );
}

/// Machines whose scanline count is not derived from a dot clock, with the
/// reason it cannot be.
///
/// This list is the whole reason `every_raster_board_derives_its_scanline`
/// is not vacuous: without it, a board could quietly skip
/// `ClockTree::set_raster` and be checked for nothing. Adding to it means
/// stating out loud that a board's scanline count rests on nothing.
const NO_RASTER_DERIVATION: &[(&str, &str)] = &[
    (
        "joust",
        "Williams gen-1 documents no dot clock; its 64 cycles per scanline come \
         from an approximate 15.6 kHz horizontal rate",
    ),
    ("robotron", "same Williams board as joust"),
    ("sinistar", "same Williams board as joust"),
];

fn declaration(entry: &registry::MachineEntry) -> ClockDeclaration {
    (entry.create_bare)()
        .clock_declaration()
        .unwrap_or_else(|| panic!("{}: declares no clock tree", entry.name))
}

/// The precondition for everything below, and worth failing on its own: a
/// machine whose crystals live only in a comment is exactly what this suite
/// exists to catch, so `None` is never an acceptable answer.
#[test]
fn every_machine_declares_a_clock_tree() {
    for entry in registry::all() {
        let decl = declaration(entry);
        assert!(
            !decl.tree.is_empty(),
            "{}: declares an empty clock tree",
            entry.name
        );
        assert!(
            decl.tree.step_domain().is_some(),
            "{}: declares no stepping domain, so no ratio in its tree means \
             anything",
            entry.name
        );
        let cpu = decl
            .tree
            .find(Clk::Cpu)
            .unwrap_or_else(|| panic!("{}: declares no CPU domain", entry.name));
        assert_eq!(
            decl.tree.step_domain(),
            Some(cpu),
            "{}: the frame loop counts in CPU cycles, so the CPU must be the \
             stepping domain",
            entry.name
        );
    }
}

/// `TIMING.cpu_clock_hz` is the crystal divided down, to the nearest hertz.
///
/// Compared against the exact rational rather than a rounded `hz()`, because
/// two boards do not land on a whole hertz: Atari System 1's 14.318181 MHz
/// colourburst crystal halves to 7159090.5 Hz, and Gottlieb's sound 6502 to
/// 894886.25 Hz. Asking for equality with a rounded accessor would make those
/// boards' agreement with their own crystals depend on which way the accessor
/// happened to round.
#[test]
fn declared_cpu_rate_matches_timing() {
    for entry in registry::all() {
        let decl = declaration(entry);
        let cpu = decl.tree.find(Clk::Cpu).expect("CPU domain");
        let (num, den) = decl.tree.hz_exact(cpu);
        let stored = decl.timing.cpu_clock_hz as u128 * den;
        let diff = stored.abs_diff(num);
        assert!(
            // Within half a hertz: `stored` is the correctly rounded form.
            diff * 2 <= den,
            "{}: TIMING.cpu_clock_hz is {}, but the declared crystals give \
             {num}/{den} Hz ({:.3} Hz)",
            entry.name,
            decl.timing.cpu_clock_hz,
            num as f64 / den as f64,
        );
    }
}

/// A board with raster hardware derives its scanline from its dot clock, and
/// the conversion lands inside the error it declares.
#[test]
fn declared_raster_reproduces_cycles_per_scanline() {
    for entry in registry::all() {
        let decl = declaration(entry);
        let Some(raster) = decl.tree.raster() else {
            continue;
        };
        let (cycles, ppm) = decl.tree.cycles_per_scanline(raster.video, raster.htotal);
        assert_eq!(
            cycles,
            decl.timing.cycles_per_scanline,
            "{}: TIMING says {} cycles per scanline, but {} dot clocks at {} Hz \
             against a {} Hz CPU is {cycles}",
            entry.name,
            decl.timing.cycles_per_scanline,
            raster.htotal,
            decl.tree.hz(raster.video),
            decl.tree
                .hz(decl.tree.step_domain().expect("stepping domain")),
        );
        assert!(
            ppm.abs() <= raster.tolerance_ppm,
            "{}: converting {} dot clocks into {cycles} CPU cycles is off by \
             {ppm} ppm, past the {} ppm the board declares",
            entry.name,
            raster.htotal,
            raster.tolerance_ppm,
        );
    }
}

/// The declared tolerance is the error the board actually has, not a bound
/// loose enough to hide one.
///
/// Without this, a board could declare 10000 ppm and the check above would
/// wave anything through. It also means tightening a divider *fails* rather
/// than silently improving: the stale bound has to be brought down to match.
#[test]
fn declared_tolerance_is_tight() {
    for entry in registry::all() {
        let decl = declaration(entry);
        let Some(raster) = decl.tree.raster() else {
            continue;
        };
        let (_, ppm) = decl.tree.cycles_per_scanline(raster.video, raster.htotal);
        if ppm == 0 {
            assert_eq!(
                raster.tolerance_ppm, 0,
                "{}: the conversion is exact, so the board should declare 0 ppm \
                 rather than {}",
                entry.name, raster.tolerance_ppm,
            );
            continue;
        }
        assert!(
            raster.tolerance_ppm <= 2 * ppm.abs(),
            "{}: the board declares {} ppm but its actual error is {ppm} ppm. \
             Declare the error you have (rounded up by at most a factor of \
             two), so a tightened divider shows up here instead of passing \
             silently",
            entry.name,
            raster.tolerance_ppm,
        );
    }
}

/// A board that draws a raster must say where its scanline count comes from.
///
/// Vector machines are exempt by construction: they have no dot clock, and
/// their `TIMING` runs the whole frame as a single "scanline". Everything else
/// either declares the derivation or appears in [`NO_RASTER_DERIVATION`] with a
/// reason.
#[test]
fn every_raster_board_derives_its_scanline() {
    for entry in registry::all() {
        let decl = declaration(entry);
        if decl.timing.total_scanlines <= 1 {
            continue; // vector board: no raster hardware at all
        }
        let excused = NO_RASTER_DERIVATION.iter().find(|(n, _)| *n == entry.name);
        match (decl.tree.raster(), excused) {
            (Some(_), None) => {}
            (Some(_), Some((_, why))) => panic!(
                "{}: declares a raster derivation but is still excused in \
                 NO_RASTER_DERIVATION as '{why}'. Remove the entry",
                entry.name
            ),
            (None, Some(_)) => {}
            (None, None) => panic!(
                "{}: draws {} scanlines but declares no dot clock to derive \
                 them from. Call ClockTree::set_raster in its clock_tree(), or \
                 add it to NO_RASTER_DERIVATION with the reason it cannot",
                entry.name, decl.timing.total_scanlines,
            ),
        }
    }
}

/// The three cross-crystal boards, pinned by name.
///
/// The registry-driven checks above would pass if all three quietly became
/// single-crystal boards with nothing to round. These are the cases the whole
/// design exists for, so pin the actual numbers: what the board runs, and how
/// wrong it is.
#[test]
fn the_cross_crystal_scanline_conversions_are_what_we_think() {
    let cases = [
        // (machine, cycles per scanline, ppm error in the video rate)
        ("docastle", 254, -125),
        ("mrdo", 261, 235),
        // Three crystals, but 384 dot clocks at 6 MHz is exactly 256 cycles at
        // 4 MHz, so unlike the two above this one has nothing to round.
        ("mariobros", 256, 0),
    ];
    for (name, expect_cycles, expect_ppm) in cases {
        let entry = registry::find(name).unwrap_or_else(|| panic!("{name} is registered"));
        let decl = declaration(entry);
        let raster = decl.tree.raster().expect("a declared raster derivation");
        let got = decl.tree.cycles_per_scanline(raster.video, raster.htotal);
        assert_eq!(
            got,
            (expect_cycles, expect_ppm),
            "{name}: crystals {:?} now give {got:?}",
            decl.tree.roots(),
        );
    }
}
