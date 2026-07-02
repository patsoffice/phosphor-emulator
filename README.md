# Phosphor Emulator

> Phosphor retro CPU emulator framework

[![Rust](https://img.shields.io/badge/rust-1.85%2B-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Tests](https://img.shields.io/badge/tests-2674%20passing-brightgreen.svg)](core/tests/)

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

ROMs are matched by CRC32 checksum, so any MAME ROM naming convention works. All three Joust label variants are supported: Green (parent), Yellow, and Red.

**Controls:**

| Key              | Action                                        |
| ---------------- | --------------------------------------------- |
| Arrows           | P1 Move                                       |
| Space            | P1 Fire / Flap                                |
| Left Ctrl        | P1 Flap                                       |
| Left Shift       | P1 Jump                                       |
| I / K / J / L    | P1 Fire Up / Down / Left / Right (Robotron)   |
| Z / X / C        | Fire Left / Center / Right (Missile Command)  |
| 1                | P1 Start                                      |
| W / A / S / D    | P2 Move                                       |
| E                | P2 Fire / Jump                                |
| 2                | P2 Start                                      |
| 5                | Insert Coin                                   |
| Mouse            | Trackball (Crystal Castles, Missile Command)  |
| F1               | Toggle Debug Panel                            |
| F2 / F3 / F4     | Step Instruction / Step Cycle / Continue      |
| F5               | Reset Machine                                 |
| F6 / F7          | Quick Save / Quick Load                       |
| F9               | Toggle Throttle                               |
| F10              | Toggle Debug Overlay                          |
| F11              | Toggle Mouse Grab                             |
| F12              | Screenshot                                    |
| Tab              | Toggle Input Bindings Panel                   |
| \`               | Toggle DIP Switches Panel                     |
| Escape           | Quit                                          |

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
- Peripheral devices (MC6821 PIA, AY-8910, POKEY, Namco WSG, Z80 CTC, Williams SC1/SC2 blitter, DVG, Atari AVG vector generator, Mathbox coprocessor, I8257 DMA, MC1408 DAC, 74LS259 latch, SSIO sound board, CMOS RAM, MOS 6532 RIOT, GI ER2055 EAROM)

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
- **RoadRunnerSystem** — Road Runner on the shared Atari System 1 board (slapstic 108 + speech-equipped sound board [VIA6522 + TMS5220 stub] + ADC0809 analog joystick with IRQ2 + per-band motion-object banking)
- **TempestSystem** — Atari color vector arcade (M6502 + AVG vector generator + Mathbox coprocessor + 2×POKEY + ER2055 EAROM + spinner)
- **QuantumSystem** — Atari color vector arcade (MC68000 + Quantum-variant AVG + 2×POKEY + X2212 NVRAM + trackball)
- **MissileCommandSystem** — Atari raster arcade (M6502 + POKEY + bitmap video)
- **PacmanSystem** — Pac-Man on shared Namco Pac board (Z80 + WSG + tile/sprite video)
- **MsPacmanSystem** — Ms. Pac-Man on shared Namco Pac board (auxiliary decode latch + ROM encryption)
- **RobotronSystem** — Williams twin-stick arcade (M6809 + blitter + PIAs)
- **SatansHollowSystem** — Satan's Hollow on shared MCR II board (Z80 + SSIO + CTC + tile dirty tracking)
- **QbertSystem** — Q*Bert on shared Gottlieb System 80 board (I8088 + M6502 sound + RIOT + DAC)
- **GalagaSystem** — Galaga on shared Namco Galaga board (3×Z80 + WSG + 05XX starfield generator)
- **GridleeSystem** — Videa arcade (M6809 + bitmap video + trackball — freely distributable ROMs)
- Simple6502System, Simple6800System, Simple6809System, SimpleZ80System, Simple68000System (test harnesses)

### Macros Crate (`phosphor-macros`)

Proc macro crate providing `#[derive(BusDebug)]`, `#[derive(DebugTrace)]`, and `#[derive(MemoryRegion)]`. `BusDebug` auto-generates bus-level debug discovery, device register writes, watchpoint routing, and device reset dispatch from struct annotations (`#[debug_cpu(...)]`, `#[debug_device(...)]`, `#[debug_map(...)]`). When `#[debug_cpu]` omits explicit read/write methods, debug memory access is auto-routed through the matching `#[debug_map]` field's `AddressSpace16` backing store. `DebugTrace` generates the event-tracing capability from a `#[debug_events]`-annotated `DebugTraceBuffer` field. `MemoryRegion` generates `From<Region> for u8` and SCREAMING_SNAKE_CASE `u8` constants from `#[repr(u8)]` region enums.

### Frontend Crate (`phosphor-frontend`)

SDL2 + egui windowed frontend — external dependencies: SDL2, zip, egui:

- **Machine-agnostic** — operates entirely through the `FrontendMachine` trait object, no hardware-specific knowledge
- **ROM path resolution** — loads from MAME ZIP files, rompath directories, or extracted loose files
- SDL2 window with GPU-scaled texture rendering (VSync frame timing)
- **Debug panel** (F1 or `--debug`) — egui side panel showing all CPU and device registers, step/cycle/continue controls (works on both 16-bit and 24-bit-bus machines, including the MC68000 games Food Fight and Quantum)
- Keyboard, game controller, and mouse input bound from each machine's typed `InputConfigurable` controls; rebindable in the settings panel (F12) and persisted per machine
- Quick save/load (F6/F7), debug overlay with FPS and machine stats (F10), mouse grab for trackball games (F11)

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

### Tools (`tools/`)

Standalone command-line utilities built on the core crates.

- **`phosphor-disasm`** (`tools/disasm`) — disassembles a ROM with any per-CPU disassembler, in three modes: a raw file, a member of a `.zip`/directory ROM set, or a machine's named code region (CPU + origin auto-resolved from the disasm registry). Depends only on `phosphor-core`/`phosphor-machines` (no SDL2). See [docs/disassembler.md](docs/disassembler.md).

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
├── tools/                       # standalone CLIs built on the core crates
│   └── disasm/                  #   phosphor-disasm — per-CPU ROM disassembler
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

The `Bus` trait connects CPUs to their board's address space using associated types for address and data width — compile-time polymorphism with no vtable overhead. Each board struct implements `Bus` to wire memory regions, I/O devices, interrupt lines, and bus arbitration (halt/DMA) together.

- **`BusMasterComponent`** — anything that drives the bus (CPUs, DMA controllers)
- **`Device`** — uniform interface for peripherals (PIAs, sound chips, timers): register read/write, tick, reset, plus debug inspection and save/load via supertraits
- **`AddressSpace16` / `AddressSpace32`** — address decoding (page-table for 16-bit boards, sorted sparse ranges for 32-bit) with backing memory for side-effect-free debug reads, watchpoints, and bank switching
- **`BusDebug`** — auto-derived via `#[derive(BusDebug)]`, layers debug access on top for the frontend's register inspector, memory viewer, and device discovery

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

### Peripherals

- TMS5220 speech synthesizer (Star Wars)

### Games

- Radar Scope (Nintendo TKG-04)
- Star Wars (Atari: 2×M6809 + AVG + Mathbox)
- Reactor (Gottlieb System 80: I8088 + M6502 sound + RIOT + trackball)
- Mad Planets (Gottlieb System 80: I8088 + M6502 sound + RIOT + spinner)
- Battlezone (Atari: M6502 + DVG + Mathbox + POKEY)
- Space Duel (Atari: M6502 + AVG + POKEY)
- Space Fury (Sega G80: Z80 + vector generator)
- Zaxxon (Sega G80: Z80 + tilemap/sprite video)
- Congo Bongo (Sega G80: Z80 + tilemap/sprite video)

### Frontend

- Migrate from SDL2 to SDL3

## License

This project is licensed under the [MIT License](LICENSE).

This is a learning/reference implementation. Not affiliated with any hardware manufacturer.

See [CONTRIBUTING.md](CONTRIBUTING.md) for design decisions, troubleshooting, and contribution guidelines.
