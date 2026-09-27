// Replay the golden Burger Time session and write its audio, to hear or diff
// what the Sound I/O board's mix does to real play.
//
// The board does not sum its two AY-3-8910s: 9F's channel A goes through its
// own band-pass and comes back inverted (docs/schematics/btime-audio-output.md).
// A change to that mix moves this file and not the golden frames, so this is
// the check that the change reached the speaker.
//
//   cargo run --release -p phosphor-script -- run --machine burgertime \
//       --rompath ~/ws/mame-runtime/roms \
//       tools/script/examples/burgertime_listen.rhai
//
// Run from the repository root, since the movie path is relative to it. To
// compare two builds, run each and diff the two files:
//
//   disasm audiodiff a.wav b.wav
//
// `m` is pre-bound by `--machine burgertime --rompath <roms>`.

m.load_movie("harness/tests/golden/movies/burgertime-1787707125.phmi");
m.capture_audio();
m.run_frames(4324);

m.write_audio("/tmp/burgertime_listen.wav");
print("wrote /tmp/burgertime_listen.wav");
