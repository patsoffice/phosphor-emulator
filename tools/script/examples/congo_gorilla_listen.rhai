// Play Congo Bongo into its gorilla's roars and write the audio, to compare
// with a recording of a real board.
//
// The game fires the gorilla itself about four seconds after the start button:
// five roars 0.40 s apart, each a PB1 pulse only microseconds long on the sound
// board's PPI, with nothing else playing. So this is the gorilla in isolation,
// through the whole machine: the sound CPU's own writes, the edge caught on the
// write, and the voice built from the drawing.
//
//   cargo run -p phosphor-script -- run --machine congobongo \
//       --rompath ~/ws/mame-runtime/roms \
//       tools/script/examples/congo_gorilla_listen.rhai
//
// The roars start about 1.9 s into the capture. Against the MAME sample set's
// gorilla.wav (progettosnaps congo.zip, a recording of a real board, resampled
// to 44.1 kHz):
//
//   disasm audiodiff /tmp/congo_gorilla_listen.wav gorilla-44k.wav \
//       --range 1.88:2.28 --range-b 0.0:0.40
//
// The fade matches (the U15 gain law was fitted to that recording); the pitch
// runs about 2.2 times low at the median, which phosphor-emulator-7z54 records
// as open.
//
// `m` is pre-bound by `--machine congobongo --rompath <roms>`.

m.run_frames(180);
m.input("coin1", true);
m.run_frames(12);
m.input("coin1", false);
m.run_frames(108);
m.input("p1_start", true);
m.run_frames(12);
m.input("p1_start", false);

// Two seconds into the game, before the first roar.
m.run_frames(120);
m.capture_audio();
m.run_frames(360);

m.write_audio("/tmp/congo_gorilla_listen.wav");
print("wrote /tmp/congo_gorilla_listen.wav");
