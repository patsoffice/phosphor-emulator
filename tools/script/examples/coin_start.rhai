// coin_start.rhai — an imperative input timeline.
//
// Generalizes the hard-coded machines/examples/asteroid_capture.rs: instead of
// baking an input schedule into a Rust fn, drive it from a script. Insert a
// coin, start a one-player game, let it run, then snapshot.
//
// Control names are per-machine stable names (see a machine's input_controls).
// The names below are galaga's:
//
//   phosphor-script run examples/coin_start.rhai --machine galaga <rompath>

// A pre-bound machine boots cold — `open()` loads no factory NVRAM — so galaga
// runs its ~3000-frame power-on self-test before it reaches the coin-ready
// attract screen. Warm up past it, then drive the timeline.
m.run_frames(3100);

m.input("coin1", true);    m.run_frames(8); m.input("coin1", false);
m.input("p1_start", true); m.run_frames(4); m.input("p1_start", false);

m.run_frames(400);         // into "PLAYER 1 / STAGE 1"
m.screenshot("coin_start.png");
print("frames = " + m.frame_count());
