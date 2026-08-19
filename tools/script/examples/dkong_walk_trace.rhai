// What does Donkey Kong actually write to its walk latch while Mario walks?
//
// The discrete walk model assumes every transition of the 74LS259 line at
// 0x7d00 is one footstep. That was inferred from a synthetic capture holding
// the line for two seconds, which is not how the game drives it — and the walk
// sounds wrong in gameplay while measuring well on that synthetic capture. This
// prints the real pattern so the model can be built against it.
//
//   cargo run -p phosphor-script -- tools/script/examples/dkong_walk_trace.rhai \
//       --machine dkong --roms ~/ws/mame-runtime/roms
//
// Boot schedule is the one that reaches playable gameplay quickly: coin at
// frame 10, start at 15, play begins around 1000. Holding right walks Mario to
// the edge of the screen, which takes roughly 250 frames.

// `m` is pre-bound by `--machine dkong --rompath <roms>`.

// Watch the whole 74LS259 sound latch, not just the walk line. If the footstep
// alternates between two sounds, either the game varies what it writes here or
// a second line is involved; watching one address cannot tell those apart.
// `watch_changed` would miss a rewrite of the same value, and whether the game
// rewrites every frame is part of what we want to see.
for a in 0..8 {
    m.watch(0x7d00 + a, "write");
}

m.run_frames(10);
m.input("coin", true);
m.run_frames(2);
m.input("coin", false);

m.run_frames(3);
m.input("p1_start",true);
m.run_frames(2);
m.input("p1_start",false);

// Run to gameplay, discarding anything the attract/intro wrote.
m.run_frames(980);
m.hits();

print("--- holding right for 250 frames ---");
m.input("p1_right", true);

// Sample per frame so each hit can be attributed to a frame, which is what
// gives the toggle *rate* rather than just the count.
let total = 0;
for i in 0..250 {
    m.run_frames(1);
    let h = m.hits();
    if h.len() > 0 {
        total += h.len();
        let parts = [];
        for e in h {
            // Address matters as much as value: which line was written is what
            // distinguishes "the game alternates" from "one line, one sound".
            parts.push(`${e.addr - 0x7d00}=${e.value}`);
        }
        print(`frame ${1000 + i}: ${parts}`);
    }
}
m.input("p1_right", false);
print(`total writes while walking: ${total}`);

// A short tail: does anything fire on release?
print("--- 30 frames after release ---");
for i in 0..30 {
    m.run_frames(1);
    let h = m.hits();
    if h.len() > 0 {
        print(`frame ${1250 + i}: ${h.len()} write(s)`);
    }
}
