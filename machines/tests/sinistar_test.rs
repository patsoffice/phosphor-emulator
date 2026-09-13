//! Integration tests for Sinistar: the boot smoke test plus the machine-trait
//! surface (native landscape raster under a declared ROT270, inputs, render
//! sizing). Save-state round-trips are covered by the shared harness in
//! `save_state_tests.rs`.

use phosphor_core::core::machine::{InputConfigurable, MachineCore, Orientation, Renderable};
use phosphor_machines::SinistarSystem;
use phosphor_machines::williams;

#[test]
fn renders_native_landscape_under_a_declared_rot270() {
    let sys = SinistarSystem::new();
    // The board raster is 292x240 landscape and Sinistar renders it as such;
    // the turned cabinet is a declared orientation the frontend applies, not a
    // rotation baked into the pixels. So the height here is the line count.
    assert_eq!(sys.display_size(), (292, 240));
    assert_eq!(sys.orientation(), Orientation::ROT270);
    // Still presented portrait, which is what the aspect describes.
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

    // Render the final frame into the native buffer without panicking.
    let (w, h) = sys.display_size();
    let mut buf = vec![0u8; (w * h * 3) as usize];
    sys.render_frame(&mut buf);
}
