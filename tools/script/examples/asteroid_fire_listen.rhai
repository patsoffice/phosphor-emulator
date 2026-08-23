// Fire a handful of shots in real gameplay and write the audio, for an ear.
//
// The scenario captures under tools/sound-compare/ are one voice on a synthetic
// timeline, which is what a measurement wants and not what a listener wants: a
// shot in Asteroids arrives over the thump track, sometimes over thrust, and at
// whatever rate the player taps. Both fire voices were rebuilt from the
// schematic and every number agrees with the board above 150 Hz, so what is left
// is the question no metric answers.
//
//   cargo run -p phosphor-script -- run --machine asteroid \
//       --rompath ~/ws/mame-runtime/roms \
//       tools/script/examples/asteroid_fire_listen.rhai
//
// `m` is pre-bound by `--machine asteroid --rompath <roms>`.

m.run_frames(10);
m.input("coin", true);
m.run_frames(2);
m.input("coin", false);

m.run_frames(3);
m.input("p1_start", true);
m.run_frames(2);
m.input("p1_start", false);

// Past the power-on self test and into play before anything is recorded.
m.run_frames(600);

m.capture_audio();

// Six shots at three spacings. The game holds its fire line for fourteen frames
// off its own timer, so a tap and a hold sound the same and only the gap between
// shots varies. The last pair overlaps a thrust burst, because a fire on its own
// is the easy case.
for gap in [30, 30, 18, 18, 10, 10] {
    m.input("fire", true);
    m.run_frames(2);
    m.input("fire", false);
    m.run_frames(gap);
}

m.input("thrust", true);
for i in 0..4 {
    m.input("fire", true);
    m.run_frames(2);
    m.input("fire", false);
    m.run_frames(20);
}
m.input("thrust", false);
m.run_frames(60);

m.write_audio("/tmp/asteroid_fire_listen.wav");
print("wrote /tmp/asteroid_fire_listen.wav");
