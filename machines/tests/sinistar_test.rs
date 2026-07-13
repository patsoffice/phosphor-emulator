//! Integration tests for Sinistar: the boot smoke test plus the machine-trait
//! surface (portrait ROT270 display, inputs, render sizing). Save-state
//! round-trips are covered by the shared harness in `save_state_tests.rs`.

use phosphor_core::core::machine::{InputConfigurable, MachineCore, Renderable};
use phosphor_machines::SinistarSystem;
use phosphor_machines::williams;

#[test]
fn display_is_portrait_after_rot270() {
    let sys = SinistarSystem::new();
    // The board raster is 292x240 landscape; Sinistar presents it rotated.
    assert_eq!(sys.display_size(), (240, 292));
    assert_eq!(sys.display_aspect(), Some((3, 4)));
}

#[test]
fn render_frame_has_correct_size() {
    let sys = SinistarSystem::new();
    let (w, h) = sys.display_size();
    let mut buf = vec![0u8; (w * h * 3) as usize];
    sys.render_frame(&mut buf); // must not panic
}

#[test]
fn input_controls_all_labeled() {
    let sys = SinistarSystem::new();
    let controls = sys.input_controls();
    // fire, bomb, p1/p2 start, coin, advance, auto_up, up/down/left/right
    assert_eq!(controls.len(), 11);
    for c in controls {
        assert!(
            !c.label.is_empty(),
            "control {} has an empty label",
            c.stable_name
        );
        assert!(!c.stable_name.is_empty());
    }
}

#[test]
fn boots_and_runs_frames_without_panicking() {
    let mut sys = SinistarSystem::new();
    sys.reset();

    let frames = 120u64;
    for _ in 0..frames {
        sys.run_frame();
    }

    // Timing advanced by exactly one frame's worth of cycles each frame.
    assert_eq!(
        sys.board.clock(),
        frames * williams::TIMING.cycles_per_frame(),
        "clock should advance one frame of cycles per run_frame"
    );

    // Render the final frame into the portrait buffer without panicking.
    let (w, h) = sys.display_size();
    let mut buf = vec![0u8; (w * h * 3) as usize];
    sys.render_frame(&mut buf);
}
