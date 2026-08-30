# Circuit excerpts

Small transcriptions of the specific sub-circuit a behaviour was derived from,
so a claim in a design doc or a code comment can be checked in seconds instead
of by re-downloading a scanned manual and hunting for the chip.

One file per behaviour, not per board. These are excerpts, deliberately: nobody
is transcribing an arcade motherboard, and a transcription nobody needs is a
transcription nobody checks.

## What goes in one

- **Provenance.** Which drawing, which manual, which page, which scan, and when
  it was read. Include which scans are unusable and why, because the next reader
  will otherwise repeat the search.
- **A diagram** of the signal flow, which is the part people actually read.
  Which kind it is depends on what the excerpt is:
  - **A netlist**, `<name>.json`, rendered to the SVG committed beside it by
    [`render.sh`](render.sh), when the excerpt transcribes specific chips: real
    refs, real pin numbers, real nets. `netlistsvg` auto-places and auto-routes,
    so the source is nets and never coordinates, and every port is labeled with
    both its pin number and its net. The drawing then states the net table
    instead of paraphrasing it.
  - **A mermaid diagram** when it is not, which is most of them. An architecture
    shared across boards has no pin numbers by design; a derivation carries
    values and conclusions rather than nets; grouped blocks need subgraphs,
    which netlistsvg has no concept of. Mermaid also follows the reader's theme,
    where a committed SVG carries its own background.
- **A net table**, `net -> ref.pin`, for the connections the behaviour turns on.
- **What it establishes**, stated as conclusions.
- **What it does NOT establish**, which is the half that keeps the format
  honest: inference, unread wires, and illegible labels named as such rather
  than written in the same confident voice as the rest.

## Why this shape

Considered and rejected:

- **KiCad** is the right tool for real schematics and its files are text and
  diffable, but its schematic side has no scripting API: the IPC API is PCB
  only as of KiCad 10, so authoring means emitting `.kicad_sch` S-expressions
  with no auto-placement and no auto-routing. Every symbol position and every
  wire segment would be a hand-computed coordinate in the diff, and parts like
  the 8T97 would need a symbol drawn before they could appear at all. That is a
  project rather than a note. Nothing here needs a netlist that can be
  simulated or fabricated either, which is the half KiCad would be earning.
- **A parallel machine-readable file** (TOML beside the prose) was the first
  instinct and is not here, because there is no consumer. Nothing can check a
  transcription against the emulator automatically: the emulator models
  behaviour, not gates. A file format with no reader is machinery, and this
  repository has enough of that already. If a consumer ever appears, the net
  tables are regular enough to lift.
- **Mermaid alone** cannot carry pin numbers legibly, which is what the
  netlists are for. Where one is used the net table is still the record, and
  the two are not checked against each other: both are written by hand from the
  same reading, so a netlist is a second transcription that can be wrong in a
  second way. Generating the table from the netlist would fix that and has not
  been done.

The honest limitation: these are hand transcriptions and can be wrong. The
mitigation is provenance plus a diagram someone can hold against the scan, not
a test. Nothing here is verified by machine, and the "does not establish"
section is there so that is obvious.

## Index

- [`williams-video-counter.md`](williams-video-counter.md) — what `$CB00` reads
  on a Williams gen-1 board, and why the counter aliases rather than saturates.
- [`williams-video-clock.md`](williams-video-clock.md) — the 12 MHz crystal and
  the chain that makes a scanline exactly 64 CPU cycles.
- [`sprite-list-scan.md`](sprite-list-scan.md) — whether a sprite circuit reads
  its object list as the beam scans or from a copy taken at vblank. Nine boards,
  and the answer is the same on all of them.
- [`qbert-object-enable.md`](qbert-object-enable.md), what enables one Gottlieb
  System 80 object on one line: why an enable and not a clip is what keeps a
  parked object off the screen, and what `sy_raw - 13` is made of.
- [`mcr-video-timing.md`](mcr-video-timing.md) — a negative result: MCR II's
  H and V counters and both blanking decodes are inside custom LSIs, so the
  blanking phase is on no drawing. Read it before hunting for one.
