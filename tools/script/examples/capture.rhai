// capture.rhai — a frameshot equivalent, used for end-to-end validation.
//
// Boots the pre-bound machine `m`, advances into the attract sequence, writes a
// PNG of the current frame, and prints the main CPU's program counter. Because
// both this and `disasm frameshot` call render_frame after the same frame
// count, their PNGs must match — see the .6 validation task.
//
//   phosphor-script run examples/capture.rhai --machine galaga <rompath>

m.run_frames(3100);
m.screenshot("out.png");
print("machine = " + m.id());
print("frames  = " + m.frame_count());
print("pc0     = " + m.pc(0));
