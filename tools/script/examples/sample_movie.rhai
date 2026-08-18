// Sample a recorded input movie at several frames in ONE boot.
//
// `disasm replay --frames N` re-runs from frame 0 for every sample, so picking a
// golden-pin frame by trial and error costs a whole replay per guess. Binding a
// movie to a session instead boots once and walks forward through it: four
// samples of a 4324-frame Burger Time recording take about seven seconds this
// way, against ~11,600 frames replayed if each sample were its own
// `disasm replay`.
//
//   cargo run -p phosphor-script -- run tools/script/examples/sample_movie.rhai
//
// Edit the paths and the `want` list for the machine and movie you are pinning.
// `run_frames` after `load_movie` delivers the recording — there is no separate
// "play" call, because the session's frame loop already applies a bound movie.

let m = open("burgertime", "/home/pjl/ws/mame-runtime/roms");
m.load_movie("/home/pjl/.config/phosphor/movies/burgertime-1787015412.phmi");

// Frames to look at. `load_movie` resets to power-on, so these are absolute
// frame numbers from reset — the same numbers a `frames.toml` entry pins.
let want = [1500, 2500, 3300, 4324];

let prev = 0;
for f in want {
    m.run_frames(f - prev);
    prev = f;
    m.screenshot("/tmp/bt_" + f + ".png");
    print("sampled frame " + m.frame_count());
}
