// What does Mario Bros. actually do with its three discrete trigger lines?
//
// The schematic settles the hardware and leaves the game unanswered.
// `docs/schematics/mario-sound-sources.md` reads the two footstep inputs as
// `7C00H(WR)` and `7C80H(WR)`: the address decode ANDed with the write strobe,
// straight into a 74123's B input with no latch between. The data byte never
// reaches the circuit, so "how long is it held" is not a question that exists
// for them. What the drawing cannot say is HOW OFTEN the game writes, and the
// 74123 is retriggerable with a diode-fed pulse of about 32 ms, so a strobe
// every two frames is one continuous tone and a strobe every sixteen is a train
// of separate clicks. Those are different sounds from identical components.
//
// The skid at 0x7F07 bit 0 is the opposite: a real level, inverted, whose 74123
// fires on the edge. A held line is therefore ONE 55 ms burst rather than a
// sustained skid, so a game that sustains one must be making repeated edges.
//
// Driven from the recorded movie rather than synthetic input, per this method's
// own lesson: hand-driving Donkey Kong Jr. produced 214 walk assertions and not
// one jump, which reads as "the game never does that" and really meant the
// character was not standing on a vine.
//
//   cargo run -p phosphor-script -- run tools/script/examples/mario_sound_trace.rhai
//
// WHAT IT FOUND, over 2500 frames of the committed recording:
//
//   walk1(7C00): 186 writes, gaps 1-232 frames, mean 7, <=2f 124, >=8f 21
//   walk2(7C80): no writes
//   skid(7F07):  2385 writes, 33 assertions, held 3-3 frames
//
// THE SKID'S THREE-FRAME PULSE IS CONFIRMED, not assumed. Thirty-three
// assertions and every one of them exactly three frames, which is the 50 ms both
// `scenarios/mariobros/skid.toml` and `drive_mario_single.lua` already use. The
// guess this trace existed to check was right.
//
// THE FOOTSTEPS COME IN BURSTS, and that is new. Two thirds of the gaps are one
// or two frames, against a 32 ms one-shot that is about two frames wide, so
// while Mario walks the one-shot is continuously retriggered and the voice is a
// sustained tone. The single-strobe scenario is still a correct fixture for one
// isolated footstep, which is what it claims to be, but no scenario covers the
// sound the game actually makes while walking.
//
// LUIGI IS NEVER HEARD. 0x7C80 is not written once in this recording, which is
// single-player, so walk2's discipline is unmeasured exactly as Donkey Kong
// Jr.'s falling voice was. Do not read "no writes" as "the game never does it".
const ROMS = "/Users/pjl/ws/mame-runtime/roms";

let m = open("mariobros", ROMS);
m.load_movie("harness/tests/golden/movies/mariobros-1787708126.phmi");

// Strobes: EVERY write is an event, including a rewrite of the same value,
// which is the opposite of the usual case and the reason these are counted
// rather than diffed.
m.watch(0x7c00, "write");
m.watch(0x7c80, "write");
// Level: the state is what matters, so the changes are the events. The game
// rewrites this port almost every frame whether or not the bit moves.
m.watch(0x7f07, "write");

let frames = 2500;
let names = ["walk1(7C00)", "walk2(7C80)"];
let strobe_frames = [[], []];
let skid_state = -1;
let skid_events = 0;
let skid_started = 0;
let skid_runs = [];
let skid_writes = 0;

for f in 0..frames {
    m.run_frames(1);
    for e in m.hits() {
        if e.addr == 0x7c00 { strobe_frames[0].push(f); }
        if e.addr == 0x7c80 { strobe_frames[1].push(f); }
        if e.addr == 0x7f07 {
            skid_writes += 1;
            let bit = e.value & 1;
            if bit != skid_state {
                if bit == 1 { skid_started = f; skid_events += 1; }
                else if skid_state == 1 { skid_runs.push(f - skid_started); }
                skid_state = bit;
            }
        }
    }
}

print(`frames: ${frames}`);
for i in 0..2 {
    let fs = strobe_frames[i];
    let n = fs.len();
    if n == 0 { print(`${names[i]}: no writes`); }
    else {
        // The gaps between consecutive strobes are the voice, because the
        // one-shot is retriggerable and about two frames wide.
        let lo = 99999; let hi = 0; let sum = 0; let gaps = 0;
        let under3 = 0; let over8 = 0;
        for k in 1..n {
            let g = fs[k] - fs[k-1];
            if g < lo { lo = g; }
            if g > hi { hi = g; }
            sum += g; gaps += 1;
            if g <= 2 { under3 += 1; }
            if g >= 8 { over8 += 1; }
        }
        if gaps == 0 { print(`${names[i]}: ${n} write(s), only one`); }
        else {
            print(`${names[i]}: ${n} writes, gaps ${lo}-${hi} frames, mean ${sum/gaps}, <=2f ${under3}, >=8f ${over8}`);
        }
    }
}
print(`skid(7F07): ${skid_writes} writes, ${skid_events} assertions`);
let n = skid_runs.len();
if n > 0 {
    let lo = 99999; let hi = 0; let sum = 0;
    for l in skid_runs { if l < lo { lo = l; } if l > hi { hi = l; } sum += l; }
    print(`  held ${lo}-${hi} frames, mean ${sum/n} over ${n} completed holds`);
}
