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
    shared across boards has no pin numbers by design; grouped blocks need
    subgraphs, which netlistsvg has no concept of. Mermaid also follows the
    reader's theme, where a committed SVG carries its own background.

  What decides it is the reading and not the circuit. A netlist is worth its
  file only where the transcription recorded pins, and a real circuit read off
  sheet labels has nothing for one to draw:
  [`williams-video-clock.md`](williams-video-clock.md) is that case, and says so
  under its confidence heading.

  The choice is per diagram and not per file, and one file can want both.
  [`qbert-object-enable.md`](qbert-object-enable.md) is the case: a netlist for
  the gate, where counting eight NAND inputs off the drawing is the whole
  argument, and a block diagram for the pipeline downstream of it, where the
  stages are the point and the pin numbers are already in the tables.
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
- [`qbert-sound-output.md`](qbert-sound-output.md) — Q*Bert's Sound/Speech A6
  output stage. Two of its apparent gaps were already modelled; the real one is
  that the DAC-to-speech balance is two trimmers with two coupling capacitors
  ahead of the sum, where the model has one constant and one capacitor behind it.
- [`pacman-audio-output.md`](pacman-audio-output.md) — Pac-Man multiplies sample
  by volume in ANALOG, through two switched resistor networks, and neither is an
  exact binary ladder. Also a filter whose corner moves with the volume code, and
  two speakers where the emulator is mono. None of it modelled. Includes which
  Pac-Man scan to use and which one is cut mid-component.
- [`llander-audio-output.md`](llander-audio-output.md) — Lunar Lander's four
  sounds, and the thing a netlist comparison cannot see: the three resistors that
  set the thrust volume are the same three that set the noise filter's corner, so
  quieter thrust is darker thrust. Also derives the 89.5 Hz / Q 7.6 band-pass
  from its six component values, which is what confirms the reference's two magic
  numbers are the circuit rather than a fit.
- [`dkongjr-sound-sources.md`](dkongjr-sound-sources.md), what generates Donkey
  Kong Jr.'s effect tones. Four voices off five 74LS629 VCO halves, a 4020 tap
  mux and a 16-bit LFSR, sharing not one source with the 555s the emulator plays
  for it today. Three of the four are transcribed as netlists. The one thing
  still missing is a frequency law for the LS629, which its datasheet does not
  give.
