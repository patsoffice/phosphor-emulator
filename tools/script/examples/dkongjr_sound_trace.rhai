// What does Donkey Kong Jr. actually write to its two sound latches, and for
// how long?
//
// The board's four effect voices are one-shots on 74LS123s, and an LS123 fires
// on an edge, not on a level. Two things follow that a schematic transcription
// alone does not settle, and both change what the model should be:
//
//   1. Which edge. `docs/schematics/dkongjr-sound-sources.md` names the latch
//      bit and the one-shot but not the pin the bit arrives at, and 74LS123
//      pin 9 is negative-edge triggered where pin 10 is positive-edge. A voice
//      triggered on the wrong edge still makes a sound of the right length and
//      pitch at the right rate, so it measures fine and is wrong.
//   2. Whether a hold means anything. If the game asserts a bit for the length
//      of an effect, the one-shot's width is irrelevant and its retrigger
//      behaviour is the whole voice; if it pulses, the width is the voice.
//
// Driven from the recorded movie rather than synthetic input, because a
// hand-driven session only reaches the effects the driver happens to trigger:
// holding right and up in one produced 214 walk assertions and not one jump or
// climb, which reads as "the game never does that" and is really "Junior was
// not standing on a vine".
//
//   cargo run -p phosphor-script -- run tools/script/examples/dkongjr_sound_trace.rhai
//
// The movie has no falling in it. That voice's trigger stays unmeasured here.
//
// NOTE ON THE ROM PATH. The recording was made against the `dkongjr2` dump, and
// the registry resolves the `dkongjr` dump first, so the movie's ROM digest
// does not match a session opened on the usual collection and `load_movie`
// refuses it. Point `ROMS` below at a directory where the second revision is
// what resolves. The refusal is the check working: the two dumps are different
// code and a movie bound to the wrong one diverges silently.
const ROMS = "/home/pjl/ws/mame-runtime/roms";

let m = open("dkongjr", ROMS);
m.load_movie("/home/pjl/.config/phosphor/movies/dkongjr-1787707396.phmi");

// Both latches, whole. The 6H latch at 0x7d00 carries walking (bit 0), jump
// (bit 1), climbing (bit 2) and the walking-pitch select (bit 7); the 5H latch
// at 0x7d80 carries falling on bit 1 and unrelated video signals elsewhere.
// Watching only the bits of interest could not tell "the game holds this line"
// from "the game rewrites the whole latch every frame", and this board does the
// latter: every frame it rewrites 0x7d00-0x7d06 whether anything changed or not.
for a in 0..8 {
    m.watch(0x7d00 + a, "write");
}
for a in 0..8 {
    m.watch(0x7d80 + a, "write");
}

// Per-bit run-length accounting. A count of assertions says how often a voice
// fires; the run lengths say whether the game pulses or holds, which is the
// question. Tracked on the latch's *state* rather than on writes, so the
// per-frame rewrite of an unchanged bit does not read as an event.
let names = ["walk", "jump", "climb", "6H.3", "6H.4", "6H.5", "6H.6", "pitch",
             "5H.0", "fall", "5H.2", "5H.3", "5H.4", "5H.5", "5H.6", "5H.7"];
let state = [];
let runs = [];
let started = [];
let events = [];
for i in 0..16 {
    state.push(0);
    runs.push([]);
    started.push(0);
    events.push(0);
}

let frames = 3000;
for f in 0..frames {
    m.run_frames(1);
    for e in m.hits() {
        let idx = e.addr - 0x7d00;
        if e.addr >= 0x7d80 {
            idx = e.addr - 0x7d80 + 8;
        }
        let v = e.value;
        if v != state[idx] {
            if v == 1 {
                started[idx] = f;
                events[idx] += 1;
            } else {
                runs[idx].push(f - started[idx]);
            }
            state[idx] = v;
        }
    }
}

print(`frames: ${frames}`);
for i in 0..16 {
    if events[i] > 0 {
        let lens = runs[i];
        let lo = 9999;
        let hi = 0;
        let sum = 0;
        for l in lens {
            if l < lo { lo = l; }
            if l > hi { hi = l; }
            sum += l;
        }
        let n = lens.len();
        if n == 0 { lo = 0; }
        print(`${names[i]}: ${events[i]} assertions, held ${lo}-${hi} frames`);
    }
}
