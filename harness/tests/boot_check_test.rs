//! ROM-gated boot checks: does each machine actually reach its game loop?
//!
//! These are the assertions that used to live as `println!` verdicts in
//! `machines/examples/*_boot_check.rs`, where nothing ran them. The examples
//! stay as the interactive front end — thumbnails, per-frame traces, a tunable
//! frame count for bring-up — and the pass/fail statements live here.
//!
//! They are in `phosphor-harness` rather than `phosphor-machines` because this
//! crate already depends on machines, already owns the resolve-ROMs-and-boot
//! sequence, and already exports the [`roms_dir`] convention that the disasm
//! and script end-to-end tests use. Putting them under `machines/tests/` would
//! need a `machines -> harness` dev-dependency cycle for no gain.
//!
//! # Gating
//!
//! No ROM directory (`PHOSPHOR_ROMS`, else `~/ws/mame-runtime/roms`) → every
//! test here skips, so CI stays green without ROMs. A machine whose own ROM
//! set is missing from an otherwise-present directory skips individually, so a
//! partial collection still runs what it can.
//!
//! [`roms_dir`]: phosphor_harness::roms_dir

use std::path::{Path, PathBuf};

use phosphor_core::core::machine::FrontendMachine;
use phosphor_harness::{Harness, load_rom_set, roms_dir};
use phosphor_machines::registry;
use phosphor_machines::rom_loader::RomSet;

/// Resolve the ROM directory, or announce the skip and return `None`.
fn roms() -> Option<PathBuf> {
    let dir = roms_dir();
    if dir.is_none() {
        eprintln!("skipping: no ROM dir (set PHOSPHOR_ROMS or ~/ws/mame-runtime/roms)");
    }
    dir
}

/// Load a machine's ROM set, or `None` if this collection cannot supply it.
///
/// Every [`RomLoadError`] variant means the same thing here — the directory
/// does not hold the dump this machine wants, whether it is absent, the wrong
/// size, or a revision whose CRCs do not match. A local collection often has
/// *a* ZIP for a game but not the revision a machine's ROM table names (a
/// `dkongjr.zip` next to the `dkongjr2` set, say), so that has to read as a
/// skip and not a failure. Everything past the ROM load is the machine's own
/// behavior and is asserted normally.
///
/// [`RomLoadError`]: phosphor_machines::rom_loader::RomLoadError
fn rom_set(dir: &Path, machine: &str) -> Option<RomSet> {
    let entry = registry::find(machine).unwrap_or_else(|| panic!("{machine} is not registered"));
    if !entry
        .rom_names
        .iter()
        .any(|n| dir.join(format!("{n}.zip")).exists())
    {
        eprintln!("skipping {machine}: no ROM set in {}", dir.display());
        return None;
    }
    match load_rom_set(dir.to_str().unwrap(), entry.rom_names) {
        Ok(set) => Some(set),
        Err(e) => {
            eprintln!("skipping {machine}: {e}");
            None
        }
    }
}

/// Boot a registered machine from `dir`, or `None` if this collection cannot
/// supply its ROMs.
fn boot(dir: &Path, machine: &str) -> Option<Harness> {
    let entry = registry::find(machine).unwrap_or_else(|| panic!("{machine} is not registered"));
    let set = rom_set(dir, machine)?;
    let mut built = match (entry.create)(&set) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("skipping {machine}: {e}");
            return None;
        }
    };
    built.reset();
    Some(Harness::from_machine(built))
}

/// Every CPU's program counter, in the order the debug bus reports them.
fn program_counters(machine: &dyn FrontendMachine) -> Vec<u32> {
    machine
        .debug_bus()
        .map(|bus| bus.cpus().iter().map(|(_, cpu)| cpu.debug_pc()).collect())
        .unwrap_or_default()
}

/// Whether the machine is putting anything on the screen: a lit pixel for a
/// raster machine, a display-list entry for a vector one.
fn is_drawing(machine: &dyn FrontendMachine) -> bool {
    if let Some(list) = machine.vector_display_list() {
        return !list.is_empty();
    }
    let (w, h) = machine.display_size();
    let mut buf = vec![0u8; (w as usize) * (h as usize) * 3];
    machine.render_frame(&mut buf);
    buf.iter().any(|&b| b != 0)
}

/// Frames to give a machine to get something on screen. Generous because the
/// slowest here (Road Runner self-initializing a blank EEPROM, Star Wars
/// clearing its RAM test) need thousands; the loop exits as soon as the
/// machine draws, so fast booters cost only what they use.
const BOOT_BUDGET: usize = 3000;

// ---------------------------------------------------------------------------
// Registry-driven
// ---------------------------------------------------------------------------

/// Every machine whose ROMs are present must run its CPU and put something on
/// screen.
///
/// This is the registry-driven half: a newly registered machine is covered as
/// soon as its ROM set is in the directory, with no edit here. It is a coarse
/// verdict — "reached the attract mode" rather than "drew the right thing",
/// which is the golden-frame epic's job — but it is the one that catches a
/// change that stops a machine booting at all.
#[test]
fn every_machine_with_roms_boots_and_draws() {
    let Some(dir) = roms() else { return };

    let mut checked = Vec::new();
    let mut skipped = Vec::new();
    for entry in registry::all() {
        let Some(mut harness) = boot(&dir, entry.name) else {
            skipped.push(entry.name);
            continue;
        };

        let reset_pcs = program_counters(harness.machine());
        let mut drew_at = None;
        for frame in 1..=BOOT_BUDGET {
            harness.run_frame();
            if is_drawing(harness.machine()) {
                drew_at = Some(frame);
                break;
            }
        }
        assert!(
            drew_at.is_some(),
            "{}: drew nothing in {BOOT_BUDGET} frames — it never reached its \
             attract mode",
            entry.name
        );

        let pcs = program_counters(harness.machine());
        assert!(
            !pcs.is_empty(),
            "{}: exposes no CPUs through its debug bus, so this test cannot \
             tell a booted machine from a wedged one",
            entry.name
        );
        // Per machine rather than per CPU: a second CPU legitimately sits at
        // its reset vector until the main CPU releases it (Xevious holds its
        // sub and sound Z80s that way, which its own check below covers).
        assert!(
            pcs.iter().zip(&reset_pcs).any(|(now, reset)| now != reset),
            "{}: no CPU left its reset PC in {} frames ({reset_pcs:02X?} -> \
             {pcs:02X?})",
            entry.name,
            drew_at.unwrap()
        );

        checked.push(entry.name);
    }

    eprintln!(
        "booted {} machine(s); skipped {} with no ROM set: {skipped:?}",
        checked.len(),
        skipped.len()
    );
    assert!(
        !checked.is_empty(),
        "the ROM directory {} exists but holds no registered machine's set — \
         this test would otherwise pass having checked nothing",
        dir.display()
    );
}

// ---------------------------------------------------------------------------
// Promoted from machines/examples/*_boot_check.rs
// ---------------------------------------------------------------------------

/// Frames of attract mode the vector games run before their display list is
/// judged, and the trailing window it must stay non-empty across.
const VECTOR_FRAMES: usize = 2000;
const VECTOR_TAIL: usize = 60;

/// A non-empty, frame-to-frame stable AVG display list means the dual-6809
/// boot completed, the Matrix Processor is feeding 3-D geometry, and the
/// vector generator is drawing it. A list that collapses mid-window means the
/// boot got partway and stalled — which a single end-of-run sample would miss.
fn assert_vector_display_is_live(dir: &Path, machine: &str) {
    let Some(mut harness) = boot(dir, machine) else {
        return;
    };

    let mut tail = Vec::with_capacity(VECTOR_TAIL);
    for frame in 0..VECTOR_FRAMES {
        harness.run_frame();
        if VECTOR_FRAMES - frame <= VECTOR_TAIL {
            let n = harness
                .machine()
                .vector_display_list()
                .map_or(0, |l| l.len());
            tail.push(n);
        }
    }

    let min = tail.iter().copied().min().unwrap_or(0);
    assert!(
        min > 0,
        "{machine}: the vector display list was empty on at least one of the \
         last {VECTOR_TAIL} frames (per-frame counts: min {min}, max {})",
        tail.iter().copied().max().unwrap_or(0)
    );

    // A live list also has to be drawing something, not just carrying
    // zero-intensity moves.
    let vectors = harness.machine().vector_display_list().unwrap_or(&[]);
    assert!(
        vectors.iter().any(|v| v.intensity > 0),
        "{machine}: {} vectors on the final frame but none is lit",
        vectors.len()
    );
}

#[test]
fn star_wars_boots_into_a_live_vector_display() {
    let Some(dir) = roms() else { return };
    assert_vector_display_is_live(&dir, "starwars");
}

/// ESB runs the Star Wars board plus a slapstic banking the $8000-$9FFF window
/// and a second banked ROM at $A000-$FFFF, so a live display here additionally
/// means the boot cleared the slapstic protection.
#[test]
fn empire_strikes_back_boots_past_its_slapstic() {
    let Some(dir) = roms() else { return };
    assert_vector_display_is_live(&dir, "esb");
}

/// Xevious holds its sub and sound Z80s in reset until the main CPU releases
/// them, which it only does after clearing the lengthy power-on RAM test — and
/// that test only passes if the 50XX start-up protection handshake succeeded.
/// So "the sub CPUs are running" is the readable signal that the protection
/// device is answering correctly.
#[test]
fn xevious_releases_its_sub_cpus_after_the_50xx_handshake() {
    let Some(dir) = roms() else { return };
    let Some(rom_set) = rom_set(&dir, "xevious") else {
        return;
    };

    let mut sys = phosphor_machines::xevious::XeviousSystem::new();
    sys.load_rom_set(&rom_set).expect("xevious ROM load");
    use phosphor_core::core::machine::MachineCore;
    sys.reset();

    const FRAMES: u32 = 800;
    let mut released_at = None;
    for frame in 1..=FRAMES {
        sys.run_frame();
        if released_at.is_none() && sys.sub_released() {
            released_at = Some(frame);
        }
    }

    assert!(
        released_at.is_some(),
        "xevious: sub/sound CPUs never came out of reset in {FRAMES} frames — \
         the main CPU is still in its self-test, which is what a failed 50XX \
         handshake looks like (main PC {:#06X})",
        sys.main_pc()
    );
    assert!(
        sys.sub_released(),
        "xevious: sub/sound CPUs were released at frame {} but are back in \
         reset by frame {FRAMES}",
        released_at.unwrap()
    );

    let (fg, bg) = sys.video_ram_nonzero();
    assert!(
        fg > 0 && bg > 0,
        "xevious: video RAM is empty after {FRAMES} frames (fg {fg}, bg {bg}) \
         — the game reached its loop but drew nothing"
    );
}

/// Atari System 1: the 68010 must leave its reset vector, stay inside mapped
/// space rather than running away, and populate video RAM. Road Runner
/// additionally self-initializes a blank EEPROM and hand-shakes with its sound
/// CPU during boot, hence the larger frame count.
fn assert_atari_system1_booted(
    machine: &str,
    reset_pc: u32,
    pc: u32,
    clock: u64,
    video: (usize, usize, usize),
    frames: u32,
) {
    assert_ne!(
        pc, reset_pc,
        "{machine}: the 68010 is still at its reset vector after {frames} frames"
    );
    assert!(
        pc < 0x0100_0000,
        "{machine}: the 68010 ran away to {pc:#010X}, outside the board's \
         mapped space"
    );
    assert!(
        clock > 0,
        "{machine}: the CPU clock did not advance in {frames} frames"
    );
    let (palette, alpha, playfield) = video;
    assert!(
        palette > 0,
        "{machine}: palette RAM is untouched after {frames} frames"
    );
    assert!(
        alpha > 0 || playfield > 0,
        "{machine}: neither the alpha nor the playfield layer has any content \
         after {frames} frames"
    );
}

#[test]
fn marble_madness_boots_its_68010_and_fills_video_ram() {
    let Some(dir) = roms() else { return };
    let Some(rom_set) = rom_set(&dir, "marble") else {
        return;
    };
    use phosphor_core::core::machine::MachineCore;

    let mut sys = phosphor_machines::MarbleSystem::new();
    sys.load_rom_set(&rom_set).expect("marble ROM load");
    sys.reset();
    let reset_pc = sys.get_cpu_state().pc;

    const FRAMES: u32 = 600;
    for _ in 0..FRAMES {
        sys.run_frame();
    }
    assert_atari_system1_booted(
        "marble",
        reset_pc,
        sys.get_cpu_state().pc,
        sys.clock(),
        sys.video_ram_stats(),
        FRAMES,
    );
}

#[test]
fn road_runner_boots_its_68010_and_fills_video_ram() {
    let Some(dir) = roms() else { return };
    let Some(rom_set) = rom_set(&dir, "roadrunner") else {
        return;
    };
    use phosphor_core::core::machine::MachineCore;

    let mut sys = phosphor_machines::RoadRunnerSystem::new();
    sys.load_rom_set(&rom_set).expect("roadrunner ROM load");
    sys.reset();
    let reset_pc = sys.get_cpu_state().pc;

    const FRAMES: u32 = 3000;
    for _ in 0..FRAMES {
        sys.run_frame();
    }
    assert_atari_system1_booted(
        "roadrunner",
        reset_pc,
        sys.get_cpu_state().pc,
        sys.clock(),
        sys.video_ram_stats(),
        FRAMES,
    );
}

/// The Galaxian family shares one video engine with per-game GFX banking, so a
/// blank frame on any one of them points at that game's bank wiring rather
/// than at the shared renderer.
#[test]
fn the_galaxian_family_draws_a_populated_frame() {
    let Some(dir) = roms() else { return };

    for machine in ["galaxian", "mooncrst", "pisces", "uniwars"] {
        let Some(mut harness) = boot(&dir, machine) else {
            continue;
        };
        // ~3 seconds: past the RAM check and into the attract intro.
        for _ in 0..180 {
            harness.run_frame();
        }

        let (w, h) = harness.machine().display_size();
        let mut buf = vec![0u8; (w as usize) * (h as usize) * 3];
        harness.machine().render_frame(&mut buf);

        let lit = buf.chunks(3).filter(|p| p != &[0, 0, 0]).count();
        let total = (w as usize) * (h as usize);
        assert!(
            lit > 0,
            "{machine}: the frame is entirely black after 180 frames"
        );
        // A frame that is *entirely* lit is as wrong as an empty one: it means
        // the tilemap decoded to a solid colour rather than to glyphs.
        assert!(
            lit < total,
            "{machine}: every one of {total} pixels is lit after 180 frames"
        );
    }
}
