# CLAUDE.md

## Project: Phosphor Emulator

Cycle-accurate retro CPU emulator framework in Rust with arcade machine support.

### Read-Only Inspection (sak)

A global pre-tool-use hook redirects common shell read commands to `sak` (Swiss Army Knife for LLMs), a read-only inspection tool. When you reach for a shell CLI to *look at* something, use the `sak` equivalent — the hook blocks the raw command otherwise. Mutating operations (`cargo build`, `git commit`, writing files) are unaffected.

- `ls` / `find` / `**` globs → `sak fs glob '<pattern>'`
- `cat` / `sed -n` / `head` / `tail` → prefer the Read tool; else `sak fs read <f> -n <lo>-<hi>` or `sak fs head|tail <f> [n]`
- `grep` / `rg` → `sak fs grep <pattern> <path>` (pass `-` to read stdin)
- `cut` / `awk '{print $n}'` → `sak fs cut -d <delim> -f <n>`
- `tree` / `ls -R` / `stat` / `wc` → `sak fs tree [path]` / `sak fs stat <path…>` / `sak fs wc`
- `git status|log|diff|blame|show` → `sak git status|log|diff|blame|show` (flags: `--staged`, `--name-only`, `--stat`)
- `jq` on `*.json` → `sak json query|keys|grep|diff|…`
- reading `Cargo.toml` / YAML / plist → `sak config query|keys|grep|diff|convert`
- `sha256sum` / `shasum` / `b3sum` → `sak hash sha256|sha1|md5|blake3 <file>`

- Discover flags with `sak <domain> <command> --help`.
- To drop a tool's stderr noise (e.g. nix's "dirty tree" warning), redirect with `2>/dev/null` — **don't** pipe through `grep -v`, the hook blocks `grep`.
- Escape hatch when a sak path genuinely won't work: prefix the command with `SAK_HOOK_BYPASS=1`.

### Structural Code Mods (ast-grep)

`sak` is read-only; for *modifying* code across many sites, prefer `ast-grep`
(in the Nix dev shell) over hand-rolled `perl`/`sed` regexes. It matches on the
Rust AST via metavariables, so it won't fire inside comments/strings and
handles brace/bracket balancing for you — far safer than text regex for
refactors like "rewrite this call pattern" or "delete every block of this shape".

- Preview matches (read-only): `ast-grep run -p '<pattern>' -l rust <paths>`
- Rewrite (prints a diff; review it): `ast-grep run -p '<pattern>' -r '<rewrite>' -l rust <paths>`
- Apply in place once the diff looks right: add `-U` (update all).
- Metavariables are `$NAME` (single node) and `$$$NAME` (variadic), e.g.
  `ast-grep run -p 'foo($A, $B)' -r 'bar($A, $B)' -l rust`.

Use the harness `Edit` tool (with `replace_all`) for literal single-file
substitutions; reach for `ast-grep` when the change is pattern-shaped or spans
many files. Always re-run `cargo build`/`test` after a codemod.

### Build & Test

```bash
cargo build                                                    # Build entire workspace
cargo test -p phosphor-core                                    # Test CPU/device changes
cargo test -p phosphor-machines                                # Test machine/board changes
cargo test -p phosphor-macros                                  # Test proc macro changes
cargo test -p phosphor-frontend                                # Test frontend changes
cargo test -p phosphor-cpu-validation                          # CPU validation (slow — only after CPU changes)
cargo test m6809_alu_shift_test                                # Run specific test category
cargo clippy --all-features --all-targets                      # Check code quality
cargo clippy --all-features --all-targets --allow-dirty --fix  # Auto-fix clippy warnings
cargo fmt                                                      # Format code
cargo run --package phosphor-frontend -- joust /path/to/roms --scale 3
cargo run -p phosphor-disasm --bin disasm -- machine --machine mariobros --region sound /path/to/roms  # Disassemble a ROM
cargo run --release -p phosphor-bench -- --roms /path/to/roms   # Benchmark emulation throughput
```

- Before/after numbers for any performance change come from `phosphor-bench`, not from the interactive profiler — it reports the fastest of N repetitions, which is the stable estimator for a deterministic workload. Always `--release`; a debug build measures a different program.

- Test the crate you changed; also test downstream crates when changing `phosphor-core` or `phosphor-macros`
- `cargo clippy` must pass with no warnings
- `cargo fmt` must pass with no warnings

### Workspace Crates

| Crate                      | Purpose                                   | Dependencies                                          |
|----------------------------|-------------------------------------------|-------------------------------------------------------|
| `phosphor-core`            | CPU implementations, Bus trait, devices   | phosphor-macros                                       |
| `phosphor-macros`          | Proc macros                               | syn, quote, proc-macro2                               |
| `phosphor-machines`        | Arcade/system board implementations       | phosphor-core, phosphor-macros, inventory             |
| `phosphor-harness`         | Headless boot harness + ROM-path resolver | phosphor-core, phosphor-machines, zip                 |
| `phosphor-frontend`        | SDL2 display, audio, input, debug UI      | phosphor-core, phosphor-machines, phosphor-harness, phosphor-script, sdl2, egui, gl |
| `phosphor-cpu-validation`  | Test vector generation & validation       | phosphor-core, serde, serde_json, rand, flate2        |
| `phosphor-disasm`          | Standalone ROM disassembler CLI           | phosphor-core, phosphor-machines, phosphor-harness, clap |
| `phosphor-script`          | Rhai scripting over a booted machine      | phosphor-core, phosphor-machines, phosphor-harness, rhai |
| `phosphor-bench`           | Headless emulation throughput benchmark   | phosphor-harness, clap                                |
| `cross-validation`         | C++ cross-validate against ref emulators  | (non-Cargo, uses Makefile)                            |

- Never create circular dependencies between crates

### Nix Dev Environment

The repo ships a `flake.nix` that pins the toolchain (cargo, rustc, clang, SDL2, pkg-config, libGL, plus Wayland on Linux) and sets `CC`/`CXX` and `LD_LIBRARY_PATH`. This is the source of truth for the build environment — prefer it over a system Rust/SDL2 install.

- **Enter the shell:** `nix develop`, or just `cd` into the repo if you use direnv (`.envrc` runs `use flake`; run `direnv allow` once).
- **Run one command without entering:** `nix develop -c cargo test` (etc.).
- **`nix-shell` still works** — `shell.nix` is a flake-compat shim that re-exports the same dev shell for anyone without flakes enabled.
- **Bump pinned deps:** `nix flake update` (rewrites `flake.lock`); commit the lockfile. Don't hand-edit a nixpkgs URL/sha.
- `nix develop -c …` prints `warning: Git tree '…' is dirty` to **stderr** when the tree has uncommitted changes — strip with `2>/dev/null`, not a `grep` filter.

### SDL2 Dependency

- The Nix dev shell provides SDL2. Outside Nix, `phosphor-frontend` requires it via `brew install sdl2`.
- `.cargo/config.toml` sets the Homebrew library path for aarch64-apple-darwin automatically
- Core and machines crates have no external C dependencies (only Rust crates)

### Testing Requirements

- Every new instruction must have integration tests
- Test both A and B register variants where applicable
- Include edge cases: zero, overflow, sign boundary (0x7F/0x80), carry propagation
- Use each CPU's flag enum in assertions (e.g. `CcFlag::X as u8` for M68xx), never raw hex values

#### Registry-driven suites

Some tests iterate `registry::all()`, so a newly registered machine is covered without editing them — don't add a per-machine row where one of these already applies. Each carries a `the_registry_is_not_empty` guard so the file can't pass vacuously.

- `machines/tests/input_contract_test.rs` — the static control table on `MachineEntry`
- `machines/tests/machine_contract_test.rs` — identity, display geometry, frame rate, DIP tables, and that `run_frame`/`reset` don't panic, on a live machine built with `MachineEntry::create_bare` (no ROMs; see `RomSet::blank`)
- `machines/tests/save_state_tests.rs` — save/load round trip, ROM-less
- `harness/tests/boot_check_test.rs`, `harness/tests/save_state_rom_test.rs` — the same ground on machines booted from real ROMs. ROM-gated: they skip without `PHOSPHOR_ROMS` (or `~/ws/mame-runtime/roms`), and skip per machine for a ROM set the collection can't supply. Run them after any change that could affect boot; CI cannot.

### CPU Validation

```bash
# Self-validation
cargo test -p phosphor-cpu-validation

# Cross-validation (against reference emulators)
cd cross-validation && make
./bin/validate_m6809 ../cpu-validation/test_data/m6809/*.json
./bin/validate_m6800 ../cpu-validation/test_data/m6800/*.json
./bin/validate_i8035 ../cpu-validation/test_data/i8035/*.json
./bin/validate_mb88xx ../cpu-validation/test_data/mb88xx/*.json
```

- If cross-validation differs from datasheet for timings, use the datasheet values
- Any changes to the CPUs must run the cross-validation script

### Issue Tracking (beads)

Local issues are tracked with `br` (beads). They live in `.beads/` and are committed to git.

```bash
RUST_LOG=error br list                          # Show open issues (RUST_LOG=error quiets log noise)
br ready --json                                 # Actionable issues (not blocked/deferred)
br show <id>                                     # Issue details
br create "Title" -p 2 --type bug               # Create (types: feature, bug, task, chore)
br update <id> --status in_progress             # Claim work
br close <id> --reason "explanation"            # Close with a descriptive reason
br sync --flush-only                            # Export to issues.jsonl for committing
```

- Priority: 0 = critical … 4 = backlog. Statuses: `open`, `in_progress`, `deferred`, `closed`.
- `br` never auto-commits — run `br sync --flush-only`, then commit `.beads/` yourself.
- Check `br ready --json` at the start of a session to see what's actionable.

### Commit Style

- Prefix: `feat:`, `fix:`, `refactor:`, `test:`, `docs:`
- Summary line under 80 chars with counts where relevant
- Body: each logical change on its own `-` bullet
- Summarize what was added/changed and why, not just file names

### Design Priorities

1. **Correctness** - Cycle-accurate hardware matching
2. **Clarity** - Educational, maintainable code
3. **Performance** - Fast enough for real-time

### README

- Keep roadmap checkboxes current
- Update CPU-specific READMEs when adding instructions or changing opcode counts
