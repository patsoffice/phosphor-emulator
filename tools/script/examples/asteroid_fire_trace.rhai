// How long does Asteroids actually hold its fire lines?
//
// Both fire scenarios hold their enable for the whole run, and both say in their
// own comments that this is a modelled limitation rather than the board's
// behaviour: the old chirp model was driven off the enable LEVEL and truncated
// the moment the line dropped. That model is gone, and the question it was
// working around is now answerable — the voice runs from the 555's reset pin and
// two analog switches, so a released line is a real event with a real shape.
//
// The answer decides what the scenarios should do, because "held" and "pulsed"
// are different sounds: held, the pitch capacitor fills to the source
// transistor's saturation and the voice settles into a steady low buzz that
// never stops.
//
//   cargo run -p phosphor-script -- tools/script/examples/asteroid_fire_trace.rhai \
//       --machine asteroid --roms ~/ws/mame-runtime/roms
//
// `m` is pre-bound by `--machine asteroid --rompath <roms>`.

// The whole 74LS259, not just line 4. The ship's fire and the saucer's are
// separate lines, and the game may well drive others in the same frame; watching
// one address cannot tell "the game holds this line" from "the game rewrites the
// latch every frame for its own reasons".
for a in 0..8 {
    m.watch(0x3c00 + a, "write");
}

m.run_frames(10);
m.input("coin", true);
m.run_frames(2);
m.input("coin", false);

m.run_frames(3);
m.input("p1_start", true);
m.run_frames(2);
m.input("p1_start", false);

// Asteroids runs a power-on self test; play does not start immediately.
m.run_frames(600);
m.hits();

// Tap fire for two frames, which is about as short as a player can make it, and
// watch what the game does with the line afterwards. If the hardware wanted the
// line held for the length of the sound, the game would have to hold it here.
print("--- fire tapped for 2 frames ---");
m.input("fire", true);
m.run_frames(2);
m.input("fire", false);

for i in 0..40 {
    m.run_frames(1);
    let h = m.hits();
    if h.len() > 0 {
        let parts = [];
        for e in h {
            parts.push(`${e.addr - 0x3c00}=${e.value}`);
        }
        print(`frame +${i}: ${parts}`);
    }
}

// And held, in case the game gates on the button rather than on its own timer.
print("--- fire held for 40 frames ---");
m.input("fire", true);
for i in 0..40 {
    m.run_frames(1);
    let h = m.hits();
    if h.len() > 0 {
        let parts = [];
        for e in h {
            parts.push(`${e.addr - 0x3c00}=${e.value}`);
        }
        print(`frame +${i}: ${parts}`);
    }
}
m.input("fire", false);
