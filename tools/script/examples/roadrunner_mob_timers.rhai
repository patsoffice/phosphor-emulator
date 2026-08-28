// Where does Road Runner put its motion-object TIMER entries, and would they
// draw on screen?
//
//   cargo run -p phosphor-script -- run tools/script/examples/roadrunner_mob_timers.rhai
//
// WHY THIS EXISTS. An Atari System 1 motion-object entry whose word[1] is
// $FFFF is what the scanline-interrupt comparator watches for. Whether the
// motion-object renderer ALSO draws that entry was an open question
// (phosphor-emulator-h52k): MAME draws it, we used not to, and MAME is no
// authority here because its own suppression mask is declared and then never
// installed. The answer that settled it is structural rather than measured, and
// this script is the measurement that failed to settle it, kept because the
// numbers are worth having and because the second half is a lesson.
//
// WHAT IT SHOWS. Every timer entry the real game uses sits at X 504, which is
// -8: one tile width off the left edge, so it draws nothing whichever way the
// hardware behaves. That looks like the game deliberately hiding an entry it
// knows will be drawn.
//
// WHAT THE CONTROL SHOWS, AND IT IS THE POINT. X 504 is not special to timer
// entries at all: it is where Road Runner parks every dormant sprite. Over one
// recorded game, all 984 timer-entry samples sit at 504 and so do 10,706
// ordinary ones, against 430 distinct X values among the sprites that are
// actually placed. So the parking is a general convention, the timer entries
// inherit it from the same clear routine, and the first reading was wrong. The
// real game cannot discriminate between the two behaviours, because it never
// puts a timer where it would be seen.
//
// Run it against a machine whose cartridge has no scanline-interrupt hardware,
// marble for instance, and it finds no timer entries at all, which is the other
// half of the picture: the flag means something only to a cartridge that has
// the comparator, while the renderer is on the motherboard and serves both.

let m = open("roadrunner", "/home/pjl/ws/mame-runtime/roms");
m.load_movie("/home/pjl/ws/phosphor-emulator/harness/tests/golden/movies/roadrunner-1787708652.phmi");

// Motion-object RAM: 8 banks of 64 entries, each entry's four words living in
// four separate $40-word planes.
fn word(m, wi) {
    let a = 0xA02000 + wi * 2;
    (m.read(0, a) << 8) | m.read(0, a + 1)
}

// The drawn position, straight out of draw_mo_entry in atari_system1.rs.
fn drawn_x(w2) {
    let x = (w2 >> 5) & 0x1FF;
    if x >= 336 { x -= 512; }
    x
}
fn drawn_y(w0) {
    let h = (w0 & 0x0F) + 1;
    let y = (-(((w0 >> 5) & 0x1FF)) - 256 - h * 8) & 0x1FF;
    if y >= 240 { y -= 512; }
    y
}
// The scanline the timer names, which is what the entry is actually for.
fn irq_line(w0) {
    let h = (w0 & 0x0F) + 1;
    (256 - ((w0 >> 5) & 0x1FF) - h * 8 - 1) & 0x1FF
}

let seen = #{};
let onscreen = 0;
let offscreen = 0;
let timer_x = #{};
let other_x = #{};

let done = 0;
while done < 1290 {
    m.run_frames(10);
    done += 10;
    for bank in 0..8 {
        let base = bank * 256;
        for e in 0..64 {
            let w1 = word(m, base + 0x40 + e);
            let w2 = word(m, base + 0x80 + e);

            // The control: tally X for timer and non-timer entries alike.
            let xk = "" + ((w2 >> 5) & 0x1FF);
            if w1 == 0xFFFF {
                if timer_x.contains(xk) { timer_x[xk] += 1; } else { timer_x[xk] = 1; }
            } else {
                if other_x.contains(xk) { other_x[xk] += 1; } else { other_x[xk] = 1; }
                continue;
            }

            let w0 = word(m, base + e);
            let key = "" + bank + ":" + e + ":" + w0 + ":" + w2;
            if seen.contains(key) { continue; }
            seen[key] = true;

            let h = (w0 & 0x0F) + 1;
            let x = drawn_x(w2);
            let y = drawn_y(w0);
            let visible = x > -8 && x < 336 && y > -(h * 8) && y < 240;
            if visible { onscreen += 1; } else { offscreen += 1; }
            let where_ = "off screen";
            if visible { where_ = "ON SCREEN"; }
            let msg = "frame " + done + " bank " + bank + " entry " + e;
            msg += ": w0=" + w0 + " w2=" + w2 + " h=" + h;
            msg += " irq_line=" + irq_line(w0);
            msg += " draws at (" + x + "," + y + ") " + where_;
            print(msg);
        }
    }
}

print("");
print("distinct timer entries: " + seen.len()
    + "; would draw on screen: " + onscreen
    + "; off screen: " + offscreen);
print("timer-entry X values: " + timer_x);
let parked = 0;
if other_x.contains("504") { parked = other_x["504"]; }
print("ordinary entries parked at the same X 504: " + parked
    + ", across " + other_x.len() + " distinct X values");
