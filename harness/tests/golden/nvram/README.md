# Factory CMOS fixtures

1 KB battery-RAM images loaded by the `nvram` field of `../frames.toml`, the
way `disasm frameshot --nvram` loads one.

Only the three Williams machines need one. With blank battery RAM the board
factory-resets, prints `FACTORY SETTINGS RESTORED` and holds there until an
operator presses the reset button — that is what the hardware does, not an
emulation bug — so a frame pinned without a fixture would be that message and
would guard almost nothing. With an initialised CMOS they reach their title
screens by frame 2400.

The other way to reach attract mode from blank CMOS is the operator's: let the
init finish, then reset the machine. `frames.toml` has no way to express a
mid-run reset, so the fixtures stand in for that button press.

Each was dumped from this emulator after its own CMOS-init ran:

```
cargo run --release -p phosphor-disasm --bin disasm -- frameshot \
    --machine joust --frames 1200 --dump-nvram joust.nv $ROMS
```
