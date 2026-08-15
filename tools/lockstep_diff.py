#!/usr/bin/env python3
"""Find the first instruction where a phosphor M6809 trace and MAME's disagree.

Takes `disasm trace --cpu 0:regs` output and a MAME trace captured by
tools/mame_esb_lockstep.lua, reduces both to (cycle, pc, d), and reports the
first row that differs -- or confirms the whole compared window agrees.

    tools/lockstep_diff.py ours.txt mame_raw.txt

Only the common prefix is compared: MAME overruns the frame limit slightly
because its exit takes effect at the next scheduler boundary, so it normally
has more instructions than we do. A clean result therefore means "no
divergence anywhere in our run", not "both runs are the same length".

MAME's totalcycles leads ours by a constant reset offset, so the cycle column
is compared as a delta from each side's first instruction -- a real timing
divergence still shows up, a different starting bias does not.
"""

import re
import sys

OURS_RE = re.compile(
    r"cyc (\d+)\s+cpu0 pc=([0-9A-F]{4}).*?A=([0-9A-F]{2}) B=([0-9A-F]{2})"
)
MAME_RE = re.compile(r"^(\d+) ([0-9A-F]{4}) ([0-9A-F]{4}):")


def ours_rows(path):
    """(cycle, pc, d) from `disasm trace --cpu 0:regs` lines."""
    with open(path) as f:
        for line in f:
            m = OURS_RE.search(line)
            if m:
                cyc, pc, a, b = m.groups()
                yield int(cyc), int(pc, 16), (int(a, 16) << 8) | int(b, 16)


def mame_rows(path):
    """(cycle, pc, d) from `tracelog "%d %04X "` lines."""
    with open(path) as f:
        for line in f:
            m = MAME_RE.match(line)
            if m:
                cyc, d, pc = m.groups()
                yield int(cyc), int(pc, 16), int(d, 16)


def main():
    if len(sys.argv) != 3:
        sys.exit(f"usage: {sys.argv[0]} <ours.txt> <mame_raw.txt>")

    ours = list(ours_rows(sys.argv[1]))
    mame = list(mame_rows(sys.argv[2]))
    if not ours or not mame:
        sys.exit("one of the traces parsed to zero instructions -- check the formats")

    print(f"ours {len(ours)} instructions, mame {len(mame)} instructions")
    o0, m0 = ours[0][0], mame[0][0]
    print(f"cycle origin: ours {o0}, mame {m0} (offset {m0 - o0})")

    n = min(len(ours), len(mame))
    for i in range(n):
        oc, opc, od = ours[i]
        mc, mpc, md = mame[i]
        if opc != mpc or od != md or (oc - o0) != (mc - m0):
            print(f"\nfirst divergence at instruction {i}")
            for j in range(max(0, i - 3), min(n, i + 3)):
                oc, opc, od = ours[j]
                mc, mpc, md = mame[j]
                mark = "<<<" if j == i else "   "
                print(
                    f"  {j:8d} ours cyc={oc - o0:<10d} pc={opc:04X} D={od:04X}"
                    f"   mame cyc={mc - m0:<10d} pc={mpc:04X} D={md:04X} {mark}"
                )
            return 1

    print(f"\nno divergence in the first {n} instructions (pc, D and cycle delta agree)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
