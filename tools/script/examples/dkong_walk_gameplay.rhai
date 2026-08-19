// Capture Donkey Kong's walk as the game actually produces it, twice: once
// walking and once standing still, on an otherwise identical schedule.
//
// Two runs because gameplay audio is not isolated. The music plays throughout
// — the "silence" before the first footstep measures -19 dBFS — so a single
// gameplay capture describes the tune, not the effect. Two captures that differ
// only in whether the stick is held put the footsteps in one and not the other,
// and everything else in both.
//
// Every earlier walk capture drove the 74LS259 sound latch directly, which
// measured a stimulus someone invented rather than the one the game emits. A
// watchpoint (dkong_walk_trace.rhai) shows the game pulses the walk line 3
// frames on / 9 off while Mario moves — a cadence no hand-written timeline had
// reproduced, and one whose 50 ms gap interacts with the effect's own decay.
// Driving real input and letting the game do the rest makes the trigger pattern
// authentic by construction, and the same schedule applies to MAME through its
// input ports.
//
//   cargo run --release -p phosphor-script -- run \
//       tools/script/examples/dkong_walk_gameplay.rhai \
//       --machine dkong --rompath ~/ws/mame-runtime/roms
//   disasm audiodiff /tmp/dk_play_walk.wav /tmp/dk_play_still.wav
//
// The diff of those two is the walk. Compare each against its MAME counterpart
// captured on the same schedule.

// `m` is pre-bound by `--machine dkong --rompath <roms>`, but each run needs a
// machine of its own from power-on, so open them explicitly instead.
const ROMS = "/home/pjl/ws/mame-runtime/roms";

// Frame schedule, shared by both runs and by the MAME side. Keeping it in one
// place is the whole point — the rig this replaces had its timeline in three
// files with nothing checking them against each other.
const COIN_AT = 10;
const START_AT = 15;
const PLAY_AT = 1000;
const SETTLE_FRAMES = 30;
const WALK_FRAMES = 250;
// Short on purpose. The two runs are identical until the stick is held, but
// afterwards their game states have separated — and a barrel falls on a Mario
// who stayed put, so the standing run dies while the walking one does not. That
// divergence is game state, not the effect, and it swamped the footsteps within
// a second of release. 15 frames is several times the footstep's 24 ms decay
// and short enough that nothing else has happened yet.
const TAIL_FRAMES = 15;

fn run_once(roms, hold_right, out) {
    let m = open("dkong", roms);

    m.run_frames(global::COIN_AT);
    m.input("coin", true);
    m.run_frames(2);
    m.input("coin", false);

    m.run_frames(global::START_AT - global::COIN_AT - 2);
    m.input("p1_start", true);
    m.run_frames(2);
    m.input("p1_start", false);

    // Reach gameplay. Nothing before this is of interest, and capturing it would
    // put the intro music in the measurement window.
    m.run_frames(global::PLAY_AT - global::START_AT - 2);

    // Capture starts here so frame PLAY_AT is sample 0 in both runs — that
    // shared origin is what lets the two be differenced without alignment.
    m.capture_audio();

    // Screenshot at the capture origin. Comparing audio between two emulators
    // is meaningless unless they are at the same point in the game — if the
    // music is at a different bar, a spectral comparison describes the offset
    // rather than the sound path. This is the check that says so.
    if hold_right {
        m.screenshot("/tmp/ours_play_f1000.png");
    }

    m.run_frames(global::SETTLE_FRAMES);

    if hold_right {
        m.input("p1_right", true);
    }
    m.run_frames(global::WALK_FRAMES);
    if hold_right {
        m.input("p1_right", false);
    }

    m.run_frames(global::TAIL_FRAMES);
    let n = m.write_audio(out);
    print(`${out}: ${n} samples (right ${if hold_right { "held" } else { "released" }})`);
}

run_once(global::ROMS, true, "/tmp/dk_play_walk.wav");
run_once(global::ROMS, false, "/tmp/dk_play_still.wav");
