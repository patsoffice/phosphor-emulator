# Factory CMOS fixtures

1 KB battery-RAM images loaded by the `nvram` field of `../frames.toml`, the
way `disasm frameshot --nvram` loads one.

Only the three Williams machines need one. From a cold boot they print
`FACTORY SETTINGS RESTORED` and never leave it — Joust still shows it at 10,000
frames — so a frame pinned without a fixture would be that message and would
guard almost nothing. With an initialised CMOS they reach their title screens
by frame 2400. See `phosphor-emulator-4waf`; when the cold boot is fixed, the
entries can drop `nvram` and be recaptured.

Each was dumped from this emulator after its own CMOS-init ran:

```
cargo run --release -p phosphor-disasm --bin disasm -- frameshot \
    --machine joust --frames 1200 --dump-nvram joust.nv $ROMS
```
