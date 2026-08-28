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
- **A mermaid diagram** of the signal flow. It renders on GitHub and is the part
  people actually read.
- **A net table**, `net -> ref.pin`, for the connections the behaviour turns on.
- **What it establishes**, stated as conclusions.
- **What it does NOT establish**, which is the half that keeps the format
  honest: inference, unread wires, and illegible labels named as such rather
  than written in the same confident voice as the rest.

## Why this shape

Considered and rejected:

- **KiCad** is the right tool for real schematics and its files are text and
  diffable, but drawing a board in it is a project rather than a note, and
  nothing here needs a netlist that can be simulated or fabricated.
- **A parallel machine-readable file** (TOML beside the prose) was the first
  instinct and is not here, because there is no consumer. Nothing can check a
  transcription against the emulator automatically: the emulator models
  behaviour, not gates. A file format with no reader is machinery, and this
  repository has enough of that already. If a consumer ever appears, the net
  tables are regular enough to lift.
- **Mermaid alone** cannot carry pin numbers legibly, so it is the diagram and
  the net table is the record.

The honest limitation: these are hand transcriptions and can be wrong. The
mitigation is provenance plus a diagram someone can hold against the scan, not
a test. Nothing here is verified by machine, and the "does not establish"
section is there so that is obvious.

## Index

- [`williams-video-counter.md`](williams-video-counter.md) — what `$CB00` reads
  on a Williams gen-1 board, and why the counter aliases rather than saturates.
- [`williams-video-clock.md`](williams-video-clock.md) — the 12 MHz crystal and
  the chain that makes a scanline exactly 64 CPU cycles.
