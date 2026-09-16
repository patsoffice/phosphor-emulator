// Does Toobin' drive either of the two video behaviors this board approximates?
//
//   cargo run --release -p phosphor-script -- run tools/script/examples/toobin_vscroll_hblank.rhai
//
// WHY THIS EXISTS. phosphor-emulator-jg18.2 lists two things toobin.rs does not
// model: the VSCROLL counter restart the memory map documents on FF8700, and the
// real horizontal porch and sync split behind `in_hblank`, which assumes the 512
// visible dots of a 640-dot line come first. Both would be read off SP-320
// sheets 14 and 5. Before reading a sheet, find out whether the program can tell:
// a behavior the game never exercises is not worth a model, and machines/CLAUDE.md
// warns that a video constant added on a guess moves every pixel the wrong way.
//
// Same shape as the object-priority survey in that issue, which closed half the
// layer-priority question by finding the hardware path present and unused.
//
// WHAT IT SHOWS, over the recorded session covering attract, a coin and play.
//
// 1. VSCROLL. 2935 writes in 3000 frames and NOT ONE IS ZERO, so the map's
//    "RESTART VSCROLL COUNTER (IF 0 DURING HBLANK)" condition is never met. All
//    1136 writes in the shorter run land at FF8700 exactly, never on one of the
//    mirrors A0-A5 leave undecoded, which is also why the narrow decode in
//    `mask_addr` has never bitten.
//
// 2. HBLANK. The status port is read 4639 times, and the hblank flag reads low in
//    90 of those values, so a program that tested it could see it. NONE OF THE
//    FIVE READERS TESTS IT. Three do `BTST #4` on the high byte, which is the
//    service switch, and two do `BTST #5`, which is the sound-command-pending
//    flag; the game takes its timing from the scanline interrupt instead and does
//    not test the vblank flag either. So the porch approximation is invisible to
//    this program.
//
// THE VALUE OF THE SECOND ONE IS THE DISTINCTION IT DRAWS. "The bit is read" and
// "the bit is used" are different claims, and only the disassembly separates them.
// Counting reads alone would have said the approximation was observable 90 times.
//
// Neither result is a proof for every screen the game has. Both are strong: one
// is a clean zero over 3000 frames, the other is every reader in the program's
// hot paths.

let m = open("toobin", "/Users/pjl/ws/mame-runtime/roms");
m.load_movie("/Users/pjl/ws/phosphor-emulator/harness/tests/golden/movies/toobin-1789430195.phmi");

// Both registers sit in the control block, whose A0 through A5 are not decoded,
// so each answers across a 64-byte span and every one of them has to be watched.
const VSCROLL = 0xFF8700;
const STATUS = 0xFF9000;
for i in 0..0x40 {
    m.watch(VSCROLL + i, "write");
    m.watch(STATUS + i, "read");
}

let frames = 1200;
let writes = 0;
let zero_writes = 0;
let write_addrs = #{};
let reads = 0;
let hblank_low = 0;
let reader_pcs = #{};

for f in 0..frames {
    m.run_frames(1);
    for h in m.hits() {
        if h.kind == "write" {
            writes += 1;
            if h.value == 0 {
                zero_writes += 1;
            }
            let k = "" + h.addr;
            if write_addrs.contains(k) { write_addrs[k] += 1 } else { write_addrs[k] = 1 }
        } else {
            reads += 1;
            // The status word is active low; bit 15 is the hblank flag.
            if (h.value & 0x8000) == 0 {
                hblank_low += 1;
            }
            let k = "" + h.pc;
            if reader_pcs.contains(k) { reader_pcs[k] += 1 } else { reader_pcs[k] = 1 }
        }
    }
}

print("over " + frames + " frames of recorded play");
print("");
print("VSCROLL writes              " + writes);
print("  of those, value zero      " + zero_writes + "   (the restart condition)");
print("  distinct addresses        " + write_addrs);
print("");
print("status-port reads           " + reads);
print("  hblank flag low in value  " + hblank_low);
print("  distinct reading PCs      " + reader_pcs.len());
print("");
print("what each reader tests, which is the part that settles it:");
for pc in reader_pcs.keys() {
    print("  " + pc + " x" + reader_pcs[pc] + "   " + m.disasm(0, parse_int(pc)));
}
