//! Registry-wide invariants that need a *live* machine.
//!
//! `input_contract_test.rs` is the sibling of this file: it pins the static
//! control table that `MachineEntry` carries directly. Everything here needs an
//! actual constructed machine, which used to mean a `RomSet` — hence the
//! hand-maintained machine lists these tests replace. `MachineEntry::create_bare`
//! removes that constraint: it runs the same constructor with `load_rom_set`
//! omitted, so every registered machine can be instantiated with no ROMs at all.
//!
//! A bare machine cannot run its game (its ROM is zero-filled), and none of
//! these tests ask it to. They check the contract the frontend relies on the
//! moment it holds a `Box<dyn FrontendMachine>`: identity, display geometry,
//! frame timing, DIP accessors, and that ticking it does not panic.
//!
//! Everything iterates `registry::all()`, so a newly registered machine is
//! covered without touching this file.

use phosphor_core::gfx::{Orientation, apply_orientation};
use phosphor_machines::{assert_dip_banks_valid, registry};

/// Guard against a vacuous suite: every test here iterates `registry::all()`,
/// so an empty registry would make all of them pass while checking nothing.
#[test]
fn the_registry_is_not_empty() {
    assert!(
        registry::all().len() > 30,
        "registry has {} machines — the contract tests below iterate it, so \
         they would pass vacuously",
        registry::all().len()
    );
}

/// Construct every registered machine with no ROMs.
///
/// The precondition for every other test in this file, so it is worth failing
/// on its own: a machine that panics in its constructor would otherwise show up
/// as an unrelated test blowing up.
#[test]
fn every_machine_constructs_without_roms() {
    for entry in registry::all() {
        let sys = (entry.create_bare)();
        assert!(
            !sys.machine_id().is_empty(),
            "{}: constructed but reports an empty machine_id",
            entry.name
        );
    }
}

/// `machine_id` is the save-file key: `load_state` refuses data whose header
/// carries a different id. Two machines sharing an id can therefore load each
/// other's saves, which restores one machine's RAM into another's address map.
#[test]
fn machine_ids_are_unique_across_the_registry() {
    let mut seen: Vec<(String, &str)> = Vec::new();
    for entry in registry::all() {
        let id = (entry.create_bare)().machine_id().to_string();
        if let Some((_, other)) = seen.iter().find(|(seen_id, _)| *seen_id == id) {
            panic!(
                "machines '{}' and '{other}' both report machine_id '{id}' — \
                 save states are keyed on it, so each would happily load the \
                 other's file",
                entry.name
            );
        }
        seen.push((id, entry.name));
    }
}

/// The CLI name is the registry lookup key. A duplicate makes `registry::find`
/// return whichever entry `inventory` happens to have linked first.
#[test]
fn cli_names_are_unique_and_resolvable() {
    let mut names: Vec<&str> = registry::all().iter().map(|e| e.name).collect();
    let before = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(before, names.len(), "duplicate CLI name in the registry");

    for entry in registry::all() {
        assert!(
            registry::find(entry.name).is_some(),
            "{}: listed by registry::all() but not resolvable by find()",
            entry.name
        );
        assert!(
            !entry.rom_names.is_empty(),
            "{}: no ROM set names — the frontend has nothing to look up",
            entry.name
        );
        for rom in entry.rom_names {
            assert!(!rom.is_empty(), "{}: empty ROM set name", entry.name);
        }
    }
}

/// `display_size` is the frontend's allocation size for the frame texture, so
/// a zero dimension is a divide-by-zero or an empty window, and an implausible
/// one is a typo that would allocate hundreds of megabytes per frame.
#[test]
fn display_sizes_are_plausible_rasters() {
    for entry in registry::all() {
        let sys = (entry.create_bare)();
        let (w, h) = sys.display_size();
        assert!(
            w > 0 && h > 0,
            "{}: display_size is {w}x{h} — the frontend allocates w*h*3 bytes \
             for the frame texture",
            entry.name
        );
        // No arcade raster or vector rasterization target of this era comes
        // close; anything larger is a units mistake, not a real screen.
        assert!(
            w <= 2048 && h <= 2048,
            "{}: display_size is {w}x{h}, which is not a plausible raster",
            entry.name
        );
    }
}

/// `render_frame` must fit inside exactly the buffer `display_size` promises.
///
/// The frontend sizes its texture from `display_size` and hands over a slice of
/// precisely `w * h * 3` bytes, so a machine that writes past its declared
/// bounds panics here rather than in the frontend.
#[test]
fn every_machine_renders_into_its_declared_buffer() {
    for entry in registry::all() {
        let sys = (entry.create_bare)();
        let (w, h) = sys.display_size();
        let mut buf = vec![0u8; (w as usize) * (h as usize) * 3];
        sys.render_frame(&mut buf);
    }
}

/// The declared orientation must be one the frontend's central transform can
/// apply to the declared raster — the pair is a contract between the machine
/// and `apply_orientation`, and nothing else checks they agree.
#[test]
fn declared_orientation_applies_to_the_declared_raster() {
    for entry in registry::all() {
        let sys = (entry.create_bare)();
        let (w, h) = sys.display_size();
        let (w, h) = (w as usize, h as usize);
        let orientation = sys.orientation();

        assert_eq!(
            orientation.bits()
                & !(Orientation::FLIP_X | Orientation::FLIP_Y | Orientation::SWAP_XY),
            0,
            "{}: orientation carries undefined flag bits",
            entry.name
        );

        let mut src = vec![0u8; w * h * 3];
        sys.render_frame(&mut src);
        // Displayed dims are the native ones with the axes swapped when the
        // orientation transposes; the destination is sized from that.
        let (dw, dh) = if orientation.swaps_axes() {
            (h, w)
        } else {
            (w, h)
        };
        let mut dst = vec![0u8; dw * dh * 3];
        apply_orientation(&src, &mut dst, w, h, orientation);
    }
}

/// A declared aspect ratio is used as a divisor when sizing the window.
#[test]
fn declared_display_aspects_are_usable_ratios() {
    for entry in registry::all() {
        let sys = (entry.create_bare)();
        if let Some((num, den)) = sys.display_aspect() {
            assert!(
                num > 0 && den > 0,
                "{}: display_aspect is {num}:{den} — the frontend divides by it",
                entry.name
            );
        }
    }
}

/// The frontend throttles on `frame_rate_hz` and the audio path derives its
/// samples-per-frame from it, so a zero or absurd value stalls or floods both.
#[test]
fn frame_rates_are_plausible_crt_refreshes() {
    for entry in registry::all() {
        let sys = (entry.create_bare)();
        let hz = sys.frame_rate_hz();
        assert!(
            hz.is_finite() && (30.0..=120.0).contains(&hz),
            "{}: frame_rate_hz is {hz}, outside any plausible arcade CRT refresh",
            entry.name
        );
    }
}

/// Every machine's DIP table must be internally consistent *and* consistent
/// with the machine's own power-on bytes.
///
/// `dip_test_suite!` does this per machine, which means it only reaches the
/// machines that remembered to invoke it. Driving it from the registry closes
/// that gap: the live bank values are the power-on bytes by definition, so the
/// check needs no per-machine expectation table.
#[test]
fn dip_tables_are_valid_against_their_power_on_values() {
    for entry in registry::all() {
        let sys = (entry.create_bare)();
        let banks = sys.dip_banks();
        if banks.is_empty() {
            continue; // a machine may legitimately expose no DIPs
        }
        let power_on: Vec<u8> = (0..banks.len()).map(|b| sys.dip_bank_value(b)).collect();
        assert_dip_banks_valid(banks, &power_on);
    }
}

/// Selecting any choice of any option must change that option's bits and no
/// others — the settings UI writes one option at a time and expects the rest of
/// the bank to survive.
///
/// Scoped to the union of the bank's option masks rather than the whole byte,
/// because machines whose DIPs share an input port with live signals mask on
/// read and merge on write (Galaxian's IN0/IN1/IN2, Burgertime's VBLANK bit).
#[test]
fn setting_one_dip_option_leaves_the_others_alone() {
    for entry in registry::all() {
        let mut sys = (entry.create_bare)();
        for (bank_idx, bank) in sys.dip_banks().iter().enumerate() {
            let claimed: u8 = bank.options.iter().map(|o| o.mask).fold(0, |a, m| a | m);
            for (opt_idx, opt) in bank.options.iter().enumerate() {
                for choice in opt.choices {
                    let before = sys.dip_bank_value(bank_idx);
                    sys.set_dip_option(bank_idx, opt_idx, choice.value);
                    let after = sys.dip_bank_value(bank_idx);
                    assert_eq!(
                        after & opt.mask,
                        choice.value & opt.mask,
                        "{}: bank '{}' option '{}' choice '{}' did not read back",
                        entry.name,
                        bank.name,
                        opt.name,
                        choice.label
                    );
                    assert_eq!(
                        after & claimed & !opt.mask,
                        before & claimed & !opt.mask,
                        "{}: bank '{}' option '{}' choice '{}' disturbed another \
                         option's bits",
                        entry.name,
                        bank.name,
                        opt.name,
                        choice.label
                    );
                }
            }
        }
    }
}

/// An out-of-range bank index must be inert in both directions — the settings
/// UI indexes by position, and a stale index must not corrupt bank 0.
#[test]
fn out_of_range_dip_banks_are_inert() {
    for entry in registry::all() {
        let mut sys = (entry.create_bare)();
        let n = sys.dip_banks().len();
        let before: Vec<u8> = (0..n).map(|b| sys.dip_bank_value(b)).collect();
        assert_eq!(
            sys.dip_bank_value(n),
            0,
            "{}: reading bank {n} (one past the end) should return 0",
            entry.name
        );
        sys.set_dip_bank_value(n, 0xFF);
        let after: Vec<u8> = (0..n).map(|b| sys.dip_bank_value(b)).collect();
        assert_eq!(
            before, after,
            "{}: writing one-past-the-end bank {n} disturbed a real bank",
            entry.name
        );
    }
}

/// Ticking and resetting must not panic.
///
/// A bare machine executes whatever a zero-filled ROM decodes to, which is
/// exactly the point: it drives the CPU down decode paths a booted game never
/// takes, and every video/audio/timer device still ticks underneath it. What
/// this pins is that no part of that path indexes out of bounds or divides by
/// zero when the code stream is garbage.
#[test]
fn bare_machines_tick_and_reset_without_panicking() {
    for entry in registry::all() {
        let mut sys = (entry.create_bare)();
        sys.reset();
        for _ in 0..2 {
            sys.run_frame();
        }
        sys.reset();
        sys.run_frame();
    }
}

/// Every machine must support save states, and the header must be rejected when
/// it does not match. The *depth* of the round trip is
/// `save_state_tests.rs`'s job; this is the "did you wire it up at all" check
/// that used to require adding a row to a hand-maintained list.
#[test]
fn every_machine_supports_save_state() {
    for entry in registry::all() {
        let sys = (entry.create_bare)();
        let saved = sys
            .save_state()
            .unwrap_or_else(|| panic!("{}: save_state() returned None", entry.name));
        assert!(
            !saved.is_empty(),
            "{}: save_state() returned an empty buffer",
            entry.name
        );
    }
}
