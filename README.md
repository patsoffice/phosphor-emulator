# Phosphor Emulator

> Phosphor retro CPU emulator framework

[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-3076%20passing-brightgreen.svg)](core/tests/)

A modular emulator framework for retro CPUs, designed for extensibility and educational purposes. Features a trait-based architecture that allows easy addition of new CPUs, peripherals, and complete systems.

This is purely a project for fun and experimentation as a cycle-accurate emulator. It is not intended to be a replacement for [MAME](https://www.mamedev.org/), and it relies heavily on the hard work the MAME team has put in over the years (their documentation, source, and decades of preservation work are an invaluable reference).

## Quick Start

### Prerequisites

- Rust 1.85+ (2024 edition)
- Cargo
- SDL2 (`brew install sdl2` on macOS)

### Build and Test

```bash
# Clone and build
git clone <repository-url>
cd phosphor-emulator
cargo build

# Run all tests
cargo test

# Expected output:
#   test result: ok. XXXX passed; 0 failed
```

### Running the Emulator

```bash
# MAME-style rompath (directory containing joust.zip)
cargo run --package phosphor-frontend -- joust /path/to/roms

# Direct ZIP file
cargo run --package phosphor-frontend -- joust /path/to/joust.zip

# Extracted ROM directory (backward compatible)
cargo run --package phosphor-frontend -- joust /path/to/extracted/roms

# Start with debug panel open (paused at first instruction)
cargo run --package phosphor-frontend -- joust /path/to/roms --debug
```

ROMs are matched by CRC32 checksum, so any MAME ROM naming convention works.

**Controls:**

| Key              | Action                                        |
| ---------------- | --------------------------------------------- |
| Arrows           | P1 Move                                       |
| Left Shift       | P1 Button 1                                   |
| Space            | P1 Button 2                                   |
| Left Ctrl        | P1 Button 3                                   |
| I / K / J / L    | P1 Fire Up / Down / Left / Right (twin-stick) |
| 1                | P1 Start                                      |
| W / A / S / D    | P2 Move                                       |
| Right Shift      | P2 Button 1                                   |
| 2                | P2 Start                                      |
| 5                | Insert Coin                                   |
| 6                | Service                                       |
| Mouse            | Trackball (Crystal Castles, Missile Command)  |
| F1               | Toggle Debug Panel                            |
| F3               | Reset Machine (reset button)                  |
| Shift + F3       | Hard Reset (power cycle, rebuild from ROM)    |
| F5               | Pause / Resume                                |
| Shift + F5       | Pause and advance one frame (autorepeats)     |
| F6 / F7          | Quick Save / Quick Load                       |
| F8               | Pause / Run toggle (Debug Panel)              |
| Shift + F8/F9/F10 | Step Cycle / Instruction / Frame (Debug Panel) |
| F10              | Toggle Throttle                               |
| F11              | Toggle Debug Overlay                          |
| Shift + F11      | Toggle Profiler                               |
| F12              | Screenshot                                    |
| Shift + F12      | Record Input Movie                            |
| Scroll Lock      | Toggle Mouse Grab                             |
| Tab              | Toggle Input Bindings Panel                   |
| \`               | Toggle DIP Switches Panel                     |
| Ctrl + \`        | Toggle Script Console                         |
| ?                | Toggle Key Legend                             |
| Escape           | Quit                                          |

Buttons are numbered by rank rather than named per game, so the same key does
the most-used action everywhere: Button 1 is whatever the game's main action is
(fire, jump, flap), Button 2 the secondary one, Button 3 a third if the cabinet
had one. On a gamepad they are A, B and X. Only Button 1 differs between
players; a machine with more than one button for player 2 gives them the same
keys as player 1. A game with fewer buttons leaves the higher ones unbound, and
individual machines add their own keys on top where the cabinet had something
that does not fit the ladder.

The function keys follow MAME's layout, so muscle memory carries over. Where
MAME has a feature this emulator does not, the key is left free rather than
reused: F2 (MAME's service switch, which is a machine input rather than an
emulator one; ours is on 6), F4 (decoded-graphics viewer,
available here as `--gfxview`), Shift + F4 (rewind) and F9 (frameskip). F8
through Shift + F10 carry the debugger instead of MAME's frameskip, which is
the one deliberate divergence.

Every key above is rebindable in the settings panel (Tab), and `?` lists the
live bindings — emulator, debugger, and game — so the table is a starting
point rather than the authority.

Game controllers are auto-mapped (D-pad, left stick, face buttons, right stick for twin-stick games). Place a [gamecontrollerdb.txt](https://github.com/mdqinc/SDL_GameControllerDB) in the working directory or `~/.config/phosphor/` for broader controller support.

> `.cargo/config.toml` sets the Homebrew library path for aarch64-apple-darwin automatically, so no manual `LIBRARY_PATH` is needed.

### Disassembling ROMs

The `disasm` tool dumps a ROM through any of the per-CPU disassemblers without launching the emulator — handy for inspecting sound/CPU ROMs while debugging.

```bash
# Disassemble a known machine's code region (CPU + origin auto-resolved)
cargo run -p phosphor-disasm --bin disasm -- machine --machine mariobros --region sound /path/to/roms

# A raw, extracted ROM file with an explicit CPU
cargo run -p phosphor-disasm --bin disasm -- raw --cpu z80 --org 0 program.bin
```

See [docs/disassembler.md](docs/disassembler.md) for all three modes (`raw`, `rom`, `machine`) and a debugging workflow.

### Viewing GFX ROMs

Decode a machine's charset/sprite tile ROMs into a picture — the machine's palette applied — to check bit-plane layout and colors. Two paths share one compositor:

**Interactive viewer** (a window, in the frontend). Works for **any tile/sprite machine** for free — it shows the sheets the running machine already decoded (Pac-Man, Galaxian, Galaga, Dig Dug, Q*bert, BurgerTime, the Namco/Nintendo/Sega/Galaxian families, …):

```bash
# Browse a machine's GFX sheets; ←/→ cycle, +/- zoom, 0 refit, Esc quits
cargo run -p phosphor-frontend -- pacman /path/to/roms --gfxview

# Open on a specific sheet (default: the first one)
cargo run -p phosphor-frontend -- dkong /path/to/roms --gfxview --gfx-region sprites
```

The machine is booted for a moment first so palette-RAM-driven colors are populated. Vector/bitmap-framebuffer machines (Asteroids, I, Robot's 3-D, Crystal Castles) have no tile sheets and report so.

**PNG sheet export** (offline, in the `disasm` tool — no running machine, CI-friendly). This is the **bring-up** path: validate a new machine's bit-plane layout + PROM palette by diffing a sheet against a MAME GFX dump *before* the scanline renderer works. It needs a `GfxRegion` registered for the machine (currently Donkey Kong, Mario Bros., Congo Bongo):

```bash
# List a machine's registered GFX regions (no ROMs needed)
cargo run -p phosphor-disasm --bin disasm -- gfxview --machine congobongo

# Export a region to a PNG tile/sprite sheet
cargo run -p phosphor-disasm --bin disasm -- gfxview --machine congobongo --region sprites \
    /path/to/roms --scale 2 --cols 8 -o congo_sprites.png
```

Both paths color pixels at **pen group 0** — per-tile color codes aren't known without live video RAM, so a machine whose color code 0 is unused (e.g. Crystal Castles) isn't shown.

## Workspace Architecture

This project uses a **workspace structure** to separate reusable components from system implementations:

### Core Crate (`phosphor-core`)

Contains all reusable components — zero external dependencies:

- CPU implementations (M6800, M6809, M6502, Z80, I8035, I8088, M68000)
- Bus abstractions (Bus trait, BusMasterComponent)
- Machine traits — `MachineCore` (frame execution contract) plus capability traits (`Renderable`, `AudioSource`, `InputConfigurable`, `MachineDebug`, `SaveState`, `Nvram`, `Profilable`), bundled into the object-safe `FrontendMachine` for frontend use
- Device trait (common interface for all peripherals: reset, read/write, tick)
- Debug traits (Debuggable, DebugCpu, BusDebug) for interactive inspection and device register writes
- Address spaces — `AddressSpace16` (page-table dispatch for 16-bit boards) and `AddressSpace32` (sparse range decode for 68000-class machines), sharing backing memory, side-effect-free debug reads, watchpoints, region introspection, and bank switching
- Audio utilities (AudioResampler, AudioResamplerF32 — Bresenham box-filter downsampling from CPU clock to output rate)
- ClockDivider (Bresenham fractional clock divider for cross-domain ticking)
- DirtyBitset (fixed-capacity dirty-tracking bitset with O(1) bulk invalidation for tile/scanline change tracking)
- GFX utilities (GfxCache pre-decoded tile/sprite pixels, ROM decoders for Pac-Man/DK/MCR families, cache-friendly blocked rotation, sprite clipping, tilemap rendering)
- Peripheral devices (MC6821 PIA, AY-8910, POKEY, TMS5220 LPC speech, HC55516 CVSD speech, Namco WSG, Z80 CTC, Williams SC1/SC2 blitter, DVG, Atari AVG vector generator, Mathbox coprocessor, Star Wars Matrix Processor, I8257 DMA, MC1408 DAC, 74LS259 latch, SSIO sound board, CMOS RAM, MOS 6532 RIOT, GI ER2055 EAROM, X2212 NOVRAM, ADC0809)

### Machines Crate (`phosphor-machines`)

Complete system implementations that wire core components together:

- **AsteroidsSystem** — Atari vector arcade (M6502 + DVG + 1024×1024 vector display)
- **IrobotSystem** — Atari I, Robot, the first 3D-polygon arcade game (M6809 + AM2901 microcoded mathbox + TTL polygon rasterizer + alphanumeric overlay + 4×POKEY + ADC0809 analog stick + X2212 NVRAM)
- **DkongSystem** — Donkey Kong on shared TKG-04 board (Z80 + I8035 + I8257 DMA + tile/sprite video)
- **DkongJrSystem** — Donkey Kong Junior on shared TKG-04 board (24KB ROM, gfx bank, different sound I/O)
- **JoustSystem** — Williams arcade board (M6809 + 48KB video RAM + two PIAs + blitter + CMOS + 12KB ROM)
- **CrystalCastlesSystem** — Atari arcade (M6502 + 2×POKEY + bitmap video + sprites + trackball)
- **FoodFightSystem** — Atari arcade (MC68000 + 3×POKEY + tilemap/sprite video + analog sticks + X2212 NVRAM)
- **MarbleSystem** — Marble Madness on the shared Atari System 1 board (MC68010 + M6502 sound + Slapstic protection + POKEY + YM2151 OPM FM synthesis + playfield/motion-object video with priority/translucency + dual trackballs + 2804 EEPROM)
- **RoadRunnerSystem** — Road Runner on the shared Atari System 1 board (slapstic 108 + speech-equipped sound board [VIA6522 + TMS5220 LPC speech] + ADC0809 analog joystick with IRQ2 + per-band motion-object banking)
- **TempestSystem** — Atari color vector arcade (M6502 + AVG vector generator + Mathbox coprocessor + 2×POKEY + ER2055 EAROM + spinner)
- **QuantumSystem** — Atari color vector arcade (MC68000 + Quantum-variant AVG + 2×POKEY + X2212 NVRAM + trackball)
- **StarWarsSystem** — Atari Star Wars color vector cockpit game (dual MC6809E + Star Wars-variant AVG + Matrix Processor 3D-math coprocessor with hardware divider & PRNG + 4×POKEY + TMS5220 LPC speech via MOS 6532 RIOT + ADC0809 flight yoke + X2212 NVRAM), and The Empire Strikes Back on the same board (Slapstic-101 address-sequence bank switching + interleaved bank 2)
- **MissileCommandSystem** — Atari raster arcade (M6502 + POKEY + bitmap video)
- **PacmanSystem** — Pac-Man on shared Namco Pac board (Z80 + WSG + tile/sprite video)
- **MsPacmanSystem** — Ms. Pac-Man on shared Namco Pac board (auxiliary decode latch + ROM encryption)
- **RobotronSystem** — Williams twin-stick arcade (M6809 + blitter + PIAs)
- **SinistarSystem** — Sinistar on the shared Williams board (M6809 + M6800 sound + blitter with window-clip + HC55516 CVSD speech for the "sini-scream"/"I hunger" + 49-way joystick + 4KB work RAM + ROT270 portrait display)
- **SatansHollowSystem** — Satan's Hollow on shared MCR II board (Z80 + SSIO + CTC + tile dirty tracking)
- **QbertSystem** — Q*Bert on shared Gottlieb System 80 board (I8088 + M6502 sound + RIOT + DAC)
- **GalagaSystem** — Galaga on shared Namco Galaga board (3×Z80 + WSG + 05XX starfield generator)
- **XeviousSystem** — Xevious on the shared Namco Galaga board (3×Z80 + WSG melodic/effect sound, 54XX explosion channel stubbed; 06XX/51XX I/O + HLE 50XX score/protection; three-layer scrolling video — ROM-lookup background, foreground text, 3bpp sprites — with direct DIP reads)
- **GridleeSystem** — Videa arcade (M6809 + bitmap video + trackball — freely distributable ROMs)
- **BurgertimeSystem** — Data East BurgerTime on the shared btime board (DECO CPU-7 encrypted M6502 + 3bpp planar char/sprite/background video with X/Y-swap sprite RAM + inverted BGR palette + ROT270 portrait display + a second M6502 driving two AY-3-8910 PSGs)
- **DocastleSystem** — Universal Mr. Do's Castle / Do! Run Run / Mr. Do's Wild Ride on the shared docastle board (two Z80s in cycle-accurate lockstep through a WAIT-gated bidirectional latch + NMI handshake, 4×SN76489A with READY-driven WAIT stalls, 2×TMS1025 input mux with a one-read select pipeline, 4bpp tilemap/sprite video with pen-15 sprite masking, per-variant memory maps and DIP tables)
- Simple6502System, Simple6800System, Simple6809System, SimpleZ80System, Simple68000System (test harnesses)

### Macros Crate (`phosphor-macros`)

Proc macro crate providing `#[derive(BusDebug)]`, `#[derive(DebugTrace)]`, and `#[derive(MemoryRegion)]`. `BusDebug` auto-generates bus-level debug discovery, device register writes, watchpoint routing, and device reset dispatch from struct annotations (`#[debug_cpu(...)]`, `#[debug_device(...)]`, `#[debug_map(...)]`). When `#[debug_cpu]` omits explicit read/write methods, debug memory access is auto-routed through the matching `#[debug_map]` field's `AddressSpace16` backing store. `DebugTrace` generates the event-tracing capability from a `#[debug_events]`-annotated `DebugTraceBuffer` field. `MemoryRegion` generates `From<Region> for u8` and SCREAMING_SNAKE_CASE `u8` constants from `#[repr(u8)]` region enums.

### Frontend Crate (`phosphor-frontend`)

SDL2 + egui windowed frontend — external dependencies: SDL2, zip, egui:

- **Machine-agnostic** — operates entirely through the `FrontendMachine` trait object, no hardware-specific knowledge
- **ROM path resolution** — loads from MAME ZIP files, rompath directories, or extracted loose files
- SDL2 window with GPU-scaled texture rendering (VSync frame timing)
- **Debug panel** (F1 or `--debug`) — egui side panel showing all CPU and device registers, step/cycle/continue controls (works on both 16-bit and 24-bit-bus machines, including the MC68000 games Food Fight and Quantum)
- Keyboard, game controller, and mouse input bound from each machine's typed `InputConfigurable` controls; rebindable in the settings panel (Tab) and persisted per machine
- **Display panel** (Shift+`` ` ``) — brightness, focus and halation, applied live while the picture is in front of you. 1.0 is what was measured off the tube rather than the middle of a slider; see [Vector Displays](#vector-displays)
- **Vector rendering** — the display list is drawn on the GPU as a swept beam with its real spot size and faceplate glow, at window resolution rather than at the generator's coordinate resolution
- Quick save/load (F6/F7), debug overlay with FPS and machine stats (F11), mouse grab for trackball games (Scroll Lock)

### CPU Validation Crate (`phosphor-cpu-validation`)

[SingleStepTests](https://github.com/SingleStepTests/65x02)-style test infrastructure for validating CPU implementations against randomized test vectors with cycle-by-cycle bus traces. Cross-validates against independent reference emulators to catch flag, timing, and behavioral bugs.

- **M6809** — 266 opcodes, 266,000 test vectors, cross-validated against [elmerucr/MC6809](https://github.com/elmerucr/MC6809) and [mame4all](https://github.com/ValveSoftware/steamlink-sdk/tree/master/examples/mame4all) M6809. See [cpu-validation/README_6809.md](cpu-validation/README_6809.md).
- **M6800** — 192 opcodes, 192,000 test vectors, cross-validated against [mame4all](https://github.com/ValveSoftware/steamlink-sdk/tree/master/examples/mame4all) M6800. See [cpu-validation/README_6800.md](cpu-validation/README_6800.md).
- **M6502** — 151 opcodes, 1,510,000 test vectors, validated against [SingleStepTests/65x02](https://github.com/SingleStepTests/65x02) with cycle-by-cycle bus traces. See [cpu-validation/README_6502.md](cpu-validation/README_6502.md).
- **Z80** — 1604 opcodes, 1,604,000 test vectors, validated against [SingleStepTests/z80](https://github.com/SingleStepTests/z80) with full register/flag/timing verification. See [cpu-validation/README_z80.md](cpu-validation/README_z80.md).
- **I8035** — 229 opcodes, 229,000 test vectors, cross-validated against [mame4all](https://github.com/ValveSoftware/steamlink-sdk/tree/master/examples/mame4all) MCS-48. See [cpu-validation/README_i8035.md](cpu-validation/README_i8035.md).
- **I8088** — 279 opcodes, 2,577,000 test vectors, validated against [SingleStepTests/8088](https://github.com/SingleStepTests/8088) with full register/flag/memory verification. See [cpu-validation/README_i8088.md](cpu-validation/README_i8088.md).
- **M68000** — complete instruction set (74 mnemonics), 1,000,058 test vectors (every file, every vector incl. address-error aborts), validated against [SingleStepTests/680x0](https://github.com/SingleStepTests/680x0) with register/flag/memory (state-only) verification. See [core/src/cpu/m68000/README.md](core/src/cpu/m68000/README.md).

### Cross-Validation (`cross-validation/`)

C++ harnesses that validate phosphor-core's test vectors against independent reference emulators. Compares registers, memory, and cycle counts.

- **M6809** — 266,000/266,000 tests pass (100%) vs elmerucr/MC6809; 261,601/266,000 (98.3%) vs mame4all
- **M6800** — 191,996/192,000 tests pass (99.998%) vs mame4all
- **M6502** — 1,510,000/1,510,000 tests pass (100%) — via SingleStepTests/65x02 reference vectors
- **Z80** — 1,604,000/1,604,000 tests pass (100%) — via SingleStepTests/z80 reference vectors
- **I8035** — 221,000/225,000 tests pass (98.2%) vs mame4all (4 ANLD opcodes excluded due to known MAME bug)
- **I8088** — 2,577,000/2,577,000 tests pass (100%) — via SingleStepTests/8088 reference vectors

### Golden Frames (`harness/tests/golden/`)

Frame-level regression testing: what each machine is supposed to *look like*.
Every registered machine has a pinned frame — a machine name, a frame count, a
prose description of the picture, a SHA-256 of the rendered RGB frame (plus the
vector display list for the vector games), and a committed reference PNG.

```bash
# Compare every machine against its pinned frame (needs ROMs)
cargo test -p phosphor-harness --test golden_frame_test -- --ignored

# Recapture after an intended rendering change, then review the image diff
PHOSPHOR_GOLDEN_UPDATE=1 cargo test -p phosphor-harness --test golden_frame_test -- --ignored

# One machine at a time, for bisecting or a single refresh
PHOSPHOR_GOLDEN_ONLY=galaga cargo test -p phosphor-harness --test golden_frame_test -- --ignored
```

The machines are emulated on every core: one board's frame cannot affect
another's, so the sweep fans out and the results are put back in registry order.
`PHOSPHOR_TEST_THREADS=1` forces it sequential again, which is the first thing to
try if a suite ever disagrees with itself between runs. The same applies to
`audio_sanity_test`.

Catches the class the boot check cannot: a swapped palette entry, a sprite
drawn one line high, a scroll latch read from the wrong register. On a mismatch
the actual frame is written to `harness/tests/golden/actual/` for `disasm
imgdiff`. See [docs/designs/frame-regression.md](docs/designs/frame-regression.md).

> **The frame comparison is currently `#[ignore]`d** while the vector renderer is
> being reworked, so it does not run unless asked for by name. The rest of the
> file still runs: the pins are checked against the reference PNGs, every
> registered machine is checked for having one, and a hand-edited hash still
> fails without ROMs. Whether the comparison is re-enabled, narrowed to the
> raster machines, or retired is tracked as `phosphor-emulator-gr27`; the vector
> machines may be better served by the display-list hash alone, which is
> renderer-independent.

### Tools (`tools/`)

Standalone command-line utilities built on the core crates.

- **`phosphor-disasm`** (`tools/disasm`) — disassembles a ROM with any per-CPU disassembler, in three modes: a raw file, a member of a `.zip`/directory ROM set, or a machine's named code region (CPU + origin auto-resolved from the disasm registry). Depends only on `phosphor-core`/`phosphor-machines` (no SDL2). See [docs/disassembler.md](docs/disassembler.md).
- **`phosphor-script`** (`tools/script`) — Rhai scripting over a booted machine (`DebugSession`), shared by the frontend's interactive console and the headless script runner.
- **`phosphor-bench`** (`tools/bench`) — headless throughput benchmark: boots a machine, runs a fixed number of frames with no window and no throttle, and reports per-frame cost split into emulation, render, and audio. Exists so performance changes are measurable rather than asserted.

```bash
# Representative board set: pacman, galaga, tempest, marble, joust
cargo run --release -p phosphor-bench -- --roms /path/to/roms

# One machine, more frames, past the power-on self-test
cargo run --release -p phosphor-bench -- --machine galaga --frames 2000 --warmup 3000
```

Reports the **fastest** of N repetitions rather than the mean: emulation is
deterministic, so run-to-run variation is host noise, and noise only ever adds
time. The trailing `+/-` is how far the slowest repetition lagged — a large
value means the machine was busy and the number deserves a re-run.

## Project Structure

```text
phosphor-emulator/
├── core/                        # phosphor-core — zero external dependencies
│   └── src/
│       ├── core/                #   Bus, MachineCore/FrontendMachine, AddressSpace16/32, ClockDivider, debug traits
│       ├── cpu/                 #   M6800, M6809, M6502, Z80, I8035, I8088, M68000
│       ├── device/              #   PIA, AY-8910, POKEY, WSG, Z80 CTC, blitter, DVG, DMA, RIOT, SSIO, ...
│       ├── audio/               #   Resampler utilities
│       └── gfx/                 #   Tile/sprite decode, rotation, tilemap rendering
├── machines/                    # phosphor-machines — arcade board implementations
│   └── src/                     #   Shared boards (Williams, TKG-04, Namco Pac, MCR II, Gottlieb)
│                                #   + per-game wiring (Joust, Robotron, Pac-Man, DK, Q*Bert, ...)
├── macros/                      # phosphor-macros — #[derive(BusDebug)], #[derive(MemoryRegion)]
├── frontend/                    # phosphor-frontend — SDL2 + egui windowed emulator
│   └── src/                     #   Main loop, video, audio, input, debug panel, overlay
├── cpu-validation/              # phosphor-cpu-validation — test vector generation & validation
│   ├── src/bin/                 #   Test generators (M6809, M6800, I8035)
│   ├── tests/                   #   Single-step validators (M6809, M6800, M6502, Z80, I8088)
│   └── test_data/               #   Generated vectors + SingleStepTests submodules
├── harness/                     # phosphor-harness — headless boot + ROM-path resolver
├── tools/                       # standalone CLIs built on the core crates
│   ├── bench/                   #   phosphor-bench — headless throughput benchmark
│   ├── disasm/                  #   phosphor-disasm — per-CPU ROM disassembler
│   └── script/                  #   phosphor-script — Rhai scripting over a booted machine
└── cross-validation/            # C++ harnesses validating against reference emulators
```

## How It Works

### Execution Model

Each CPU is a **cycle-accurate state machine**. A call to `tick()` advances exactly **one CPU cycle**, performing a single bus read or write just like the real hardware. All CPUs follow the same `Fetch → Execute → Fetch` pattern, with CPU-specific states for prefixed opcodes, halt/wait modes, and interrupt sequencing.

**Example: M6809 executing `LDA #$42`** (opcode 0x86):

```text
Cycle 0 (Fetch):  Read 0x86 from memory[PC=0] → PC=1, state=Execute(0x86, 0)
Cycle 1 (Exec 0): Read 0x42 from memory[PC=1] → A=0x42, PC=2, state=Fetch
Cycle 2 (Fetch):  Read next opcode...
```

### Architecture

The `Bus` trait connects CPUs to their board's address space using associated types for address and data width. Each board struct implements `Bus` to wire memory regions, I/O devices, interrupt lines, and bus arbitration (halt/DMA) together.

Dispatch is *static* everywhere: every machine keeps its CPU state in one field and its bus state in another, so `cpu.execute_cycle(&mut bus, ..)` borrow-checks natively at a concrete type and the whole cycle monomorphises — no vtable, and no `unsafe` anywhere on the CPU↔bus path. Getting there removed a raw-pointer `&mut dyn Bus` reborrow (`bus_split!`) from every board; measured at 5–20% less emulation time per frame depending on how much of a frame is CPU bus cycles. See [docs/designs/concrete-bus-dispatch.md](docs/designs/concrete-bus-dispatch.md) for the shape and the results.

- **`BusMasterComponent`** — anything that drives the bus (CPUs, DMA controllers)
- **`Device`** — uniform interface for peripherals (PIAs, sound chips, timers): register read/write, tick, reset, plus debug inspection and save/load via supertraits
- **`AddressSpace16` / `AddressSpace32`** — address decoding (page-table for 16-bit boards, sorted sparse ranges for 32-bit) with backing memory for side-effect-free debug reads, watchpoints, and bank switching
- **`BusDebug`** — auto-derived via `#[derive(BusDebug)]`, layers debug access on top for the frontend's register inspector, memory viewer, and device discovery

### Vector Displays

The vector machines (Asteroids, Lunar Lander, Tempest, Quantum, Star Wars) do
not have a framebuffer. Their generators emit a list of line segments, and what
reaches the eye is a beam sweeping the glass. Both renderers — the OpenGL path
the frontend draws with, and the CPU rasterizer behind screenshots and the debug
panel — model that beam rather than drawing one-pixel lines, from figures taken
off the tube rather than tuned by eye:

- **Spot size.** The Atari colour XY monitors are 19 inch shadow-mask tubes. The
  mask pitch is about 0.6 mm and the focused spot about 0.7 mm, on a viewable
  area whose long axis is about 360 mm. The beam is drawn with that profile, so
  a vector has real width, soft edges, and round ends.
- **Brightness from dwell.** Light per unit of length is beam current times how
  long the beam spent there; the intensity code only supplies the current. The
  reference is the beam's own top speed, `0x1FF * 255 >> 4`, which works out at
  8.05 cycles per unit along an axis and `1/sqrt(2)` of that on a diagonal.
  Measured across all three AVG machines, the observed floor is 5.7 and the 5th
  percentile is 8.0 — the same deflection hardware in each.
- **Bright vertices.** Between one vector and the next the deflection DACs hold
  their last value, so the beam stands still on a single point while the
  sequencer fetches the next instruction: about 64 cycles on one spot against
  about 8 for a unit of moving line. That is where the bright corners of a
  vector picture come from.
- **Halation.** Light steeper than the critical angle cannot leave the faceplate,
  so it reflects back, crosses the glass again and re-emerges at
  `2*t*tan(asin(1/n))` — about 19 mm on an 11 mm faceplate, or 5% of the tube's
  long axis. That broad glow is the GL path's; the CPU rasterizer leaves it off,
  since compositing a full-frame blur costs several times the beam sweep and the
  GPU does it for nothing.

The one figure with no derivation behind it is what *fraction* of a spot's light
halates, which depends on phosphor isotropy and glass coatings we have no numbers
for. It is labelled as such in the source, and it is the knob most worth turning.

A generator's coordinates are not pixels: a unit is 1/65536 of the position
accumulator, so how many units a picture spans is decided by whatever scale
values a game's programmers picked. `Renderable::vector_field_size` reports that
extent separately from `display_size`, which is the resolution to rasterize into,
so a machine's rendered detail is not capped by its data's numeric range.

### Testing CPUs

A `TestBus` harness lets you exercise any CPU in isolation — load machine code, tick cycle-by-cycle, and assert results. Example with the M6809:

```rust
let mut cpu = M6809::new();
let mut bus = TestBus::new();

bus.load(0, &[0x86, 0x42, 0x97, 0x10]);  // LDA #$42; STA $10

for _ in 0..5 {
    cpu.tick_with_bus(&mut bus, BusMaster::Cpu(0));
}

assert_eq!(cpu.a, 0x42);
assert_eq!(bus.memory[0x10], 0x42);
```

## Future

### Games

- Radar Scope (Nintendo TKG-04)
- Reactor (Gottlieb System 80: I8088 + M6502 sound + RIOT + trackball)
- Mad Planets (Gottlieb System 80: I8088 + M6502 sound + RIOT + spinner)
- Battlezone (Atari: M6502 + DVG + Mathbox + POKEY)
- Space Duel (Atari: M6502 + AVG + POKEY)
- Space Fury (Sega G80: Z80 + vector generator)
- Zaxxon (Sega G80: Z80 + tilemap/sprite video)
 
### Frontend

- Migrate from SDL2 to SDL3

## License

This project is licensed under the [MIT License](LICENSE).

This is a learning/reference implementation. Not affiliated with any hardware manufacturer.

See [CONTRIBUTING.md](CONTRIBUTING.md) for design decisions, troubleshooting, and contribution guidelines.
