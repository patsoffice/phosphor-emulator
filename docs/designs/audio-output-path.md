# Design: Audio Output Path

> **Status: implemented.** All five phases are built; each section carries an
> "as built" note where the implementation departed from the proposal. This
> document covers the shared path every sound device
> takes from its native clock to the host audio device: decimation, buffering,
> rate negotiation, and clock synchronisation. It does not change how any chip
> is synthesised. Tracked in beads epic
> `phosphor-emulator-audio-output-path-oe0b`.

## Context

The sound chips in this workspace are modelled at the clock level. POKEY steps a
4-bit, a 5-bit, a 9-bit and a 17-bit LFSR once per 1.79 MHz tick and derives its
channel dividers from them. The Namco WSG walks its waveform ROM from a phase
accumulator clocked at the CPU rate. The AY-8910 runs its tone counters, noise
LFSR and envelope at chip clock / 8. The TMS5220 runs an LPC lattice filter at
its own 8 kHz frame rate. Each of these is a faithful model producing a signal at
the rate the hardware produces it.

Every one of them then converts to the host rate through a single shared path:

```text
chip synthesis            AudioResampler                  frontend
──────────────            ──────────────                  ────────
1.79 MHz (POKEY)   ─┐
3.07 MHz (WSG)     ─┼─►  box-average N samples  ──►  Vec  ──►  Mutex<VecDeque>  ──►  SDL callback
2.00 MHz (AY)      ─┤    emit the mean               drain(..n)     8192 cap
8.14 kHz (TMS)     ─┘                                              drop-oldest on overflow
```

That path has one significant defect and four smaller ones, all of which are
cheapest to address together because they touch the same two files.

Where it landed, for a reader arriving after the fact:

```text
chip synthesis        AudioResampler                       frontend
──────────────        ──────────────                       ────────
1.79 MHz (POKEY) ─┐   box to 4× out ─► 101-tap FIR ─►  SampleRing ─► SPSC ring ─► SDL callback
3.07 MHz (WSG)   ─┼─► 1 add/cycle       per output      O(1) drain    lock-free    8192 cap
2.00 MHz (AY)    ─┤                                                       ▲        drop-newest
8.14 kHz (TMS)   ─┘                                                       │
                      output rate negotiated from the device ─────────────┤
                      frame period trimmed from ring fill ────────────────┘
```

### The significant defect: box-filter decimation

`AudioResampler::tick` (`core/src/audio/mod.rs:124`) accumulates input samples
and emits their arithmetic mean each time a Bresenham phase accumulator crosses.
Averaging N consecutive samples and taking one is a box filter of length N used
as a decimation filter.

A length-N box filter has magnitude response `|sin(πfN/fs) / (N sin(πf/fs))|`.
Its first sidelobe is about 13 dB below the passband and the sidelobes decay at
only 6 dB per octave. For the decimation ratios in use — roughly 40 for POKEY at
1.79 MHz, roughly 70 for a 3.07 MHz Namco board — that means the stopband never
usefully arrives.

This matters more here than it would for most signals. These chips emit square
waves and LFSR noise, which carry substantial energy in high harmonics: a square
wave's Nth harmonic falls off only as 1/N. Everything above the 22.05 kHz output
Nyquist folds back into the audible band, and because the fold-back is a
reflection rather than a shift, it lands at frequencies unrelated to the
fundamental. The audible result is inharmonic grit that rises with pitch —
worst on exactly the bright, high-register effects these boards use most.

So the current path spends real effort synthesising the right signal and then
discards a meaningful part of that fidelity in the last step.

### The four smaller defects

1. **No clock synchronisation.** Video is paced against `frame_rate_hz` off the
   host monotonic clock (`frontend/src/emulator.rs:1203`); audio is consumed off
   the sound card's crystal. These differ by tens of ppm on real hardware.
   Nothing reconciles them, so the ring either fills — dropping its oldest
   samples at `emulator.rs:916` — or drains, holding the last sample at
   `audio.rs:41`. Both are audible, and both recur on a period set by drift rate
   rather than by anything happening in the game.

2. **A mutex on the real-time thread.** The SDL callback locks a
   `Mutex<VecDeque<i16>>` shared with the emulator thread (`audio.rs:26`). If the
   emulator holds it when the callback fires, the audio thread blocks and
   underruns. The drain-then-release structure narrows the window; it cannot
   remove it.

3. **Hardcoded output rate.** Devices construct their resampler against a literal
   or per-file `44_100` (`ay8910.rs:106`, `namco_wsg.rs:64`, `konami_sound.rs:64`,
   `ssio.rs:49`, `tms5220.rs:46`, `votrax_sc01.rs:59`,
   `machines/src/congo_bongo.rs:98`), and the frontend discards the spec SDL
   actually grants (`audio.rs:94`). On a device that opens at 48 kHz, the machine
   plays about 8% sharp.

4. **O(n) drain.** `fill_audio` uses `Vec::drain(..n)`, shifting the backlog down
   on every call (`audio/mod.rs:192`). Star Wars and I, Robot each carry four
   POKEYs, so they pay it four times per frame.

## Goals

1. Decimate with a filter whose stopband rejection is good enough that aliasing
   is inaudible — target 60 dB or better above the output Nyquist.
2. Keep the per-emulated-cycle cost essentially unchanged. Filter work must scale
   with output samples, not input cycles.
3. Keep `AudioResampler`'s public API, so no device or board changes.
4. Never lock, allocate, or syscall on the audio callback thread.
5. Track the host audio clock rather than assuming it matches the host monotonic
   clock.
6. Use whatever sample rate the audio device actually grants.
7. Preserve deterministic save/load. Resampler phase is part of machine state.

> Goal 7 turned out to have two pre-existing violations that the box filter's
> coarse output had been hiding. Star Wars never saved `audio_dc`, its DC
> blocker's two recursive state values; and the TMS5220's resampler was
> `#[save_skip]`ed alongside the variant and clock, though only the rates are
> configuration. Both surfaced in `save_state_tests` as soon as the output got
> finer-grained, and both are fixed.

## Non-goals

- Changing any chip's synthesis. The LFSRs, dividers, envelopes and lattice
  filters stay exactly as they are.
- Stereo, or per-machine mixing topology. The frontend contract stays mono `i16`
  drained through `AudioSource::fill_audio`.
- Board-level analog modelling. That is the discrete sound framework's job; see
  `docs/designs/discrete-sound-framework.md`.

## 1. Two-stage decimation — *implemented*

Filtering 1.79 MHz down to 44.1 kHz in one FIR stage would need a very long
filter to get a narrow transition band at that ratio, and would run per input
sample — unaffordable in the per-cycle hot path.

Split the ratio instead. The first stage is the existing box filter, which is
cheap (one add per input sample) and is a perfectly adequate anti-alias filter
when the target is still far above the final Nyquist. The second stage is a
proper windowed-sinc FIR running at the intermediate rate.

```text
1.79 MHz ──► box decimate ──► ~176 kHz ──► polyphase FIR ──► 44.1 kHz
             (1 add/sample)     (4× target)   (~64 taps, per output sample)
```

Choosing the intermediate rate at 4× the output rate means:

- The box filter's first null sits at `f_int`, well above the 22.05 kHz final
  Nyquist, so its poor stopband is harmless — everything it fails to reject is
  still inside the intermediate band and gets a second chance.
- The FIR only needs to reject above 22.05 kHz from a 176.4 kHz input, which is
  a relaxed transition band and therefore a short filter.
- The FIR runs at output rate — about 44,100 evaluations per second per device,
  each roughly 64 multiply-adds. That is on the order of 3 M MAC/s per device,
  against the ~50 M cycles/s of emulation the machine is already doing, and
  crucially it does not touch the per-emulated-cycle path at all.

Implementation notes:

- Design the FIR once at build time as a `const` table. A Kaiser window with
  β ≈ 8 over 64 taps gives roughly 80 dB stopband, comfortably past the 60 dB
  goal.
- Use a polyphase decomposition so only the taps contributing to each output
  sample are evaluated, rather than filtering at the intermediate rate and
  throwing samples away.
- The existing Bresenham accumulator already handles arbitrary non-integer
  ratios; it stays, moved to the boundary between the two stages.

### The upsampling path must survive

`AudioResampler` is not only a decimator. The TMS5220 produces 8,135 Hz and must
reach 44.1 kHz, and `core/src/audio/mod.rs:253` pins that case specifically — its
comment records that a downsample-only resampler was the cause of a "slow/choppy
speech" bug.

As built, stage one is not skipped for the upsampling case: its Bresenham
accumulator holds the input up to the intermediate rate as before, and the FIR
then smooths that zero-order hold rather than interpolating from the source
directly. This keeps one code path for both directions, and the ZOH images the
FIR cannot remove are the ones that were already there, so the path does not
regress. `resampler_upsamples_to_full_output_count` is unchanged and still
passes.

### As built

- 101 taps, not the estimated 64. The Kaiser order estimate
  `N ≈ (A − 8) / (2.285·Δω)` puts 80 dB across a 17.4→26.7 kHz transition at
  about 100; a 95-tap trial measured 75.6 dB at the stopband edge.
- Measured end to end (`content_above_the_output_nyquist_does_not_fold_back`):
  a 30 kHz tone folds to 14.1 kHz at **91 dB** below an equal in-band tone,
  against the 60 dB goal.
- The dot product sums into four lanes rather than one running total. Float
  addition is not associative, so a single accumulator serialises the
  multiply-adds and blocks vectorisation — worth about 3× on the filter.
  Folding the filter about its centre to halve the multiplies was tried and
  measured within noise, because the reversed access costs more vectorisation
  than it saves multiplies.
- Cost, `phosphor-bench` at 600 frames × 5 reps: starwars 3.573 → 3.964 ms/f,
  tempest 1.108 → 1.308, galaga 0.712 → 0.771, qbert 2.768 → 2.973. That is
  7–18% of emulation time on the audio-heavy machines, all still 5–21×
  realtime. Goal 2 holds in the sense that matters — filter work scales with
  output samples — but note that most of the measured cost is stage one now
  firing 4× as often, not the filter itself. If this needs to come down, that
  is where to look, not at the tap count.

### Aliasing this does not remove

Stage one still aliases around multiples of the intermediate rate: input content
at `f_int − δ` folds to `δ`, and stage two cannot undo it. The box's own
response is the only protection there, and it has the right shape — about
−45 dB for content folding to 1 kHz, weakening to about −19 dB for content
folding to 18 kHz. Low-frequency aliases, the audible ones, are the
well-protected case. Raising [`DECIMATION`] would deepen this at the cost of a
proportionally longer filter.

## 2. Output ring — *implemented*

Replace the resampler's output `Vec` with a ring. Both `tick` and `fill_audio`
become O(1) amortised with no memmove.

Capacity should be a small multiple of a frame's worth of samples — at 44.1 kHz
and ~60 Hz that is ~735 per frame, so 4096 is ample. Overrun should be
observable rather than silent: expose a counter the profiler can read, because
a persistently overrunning device is a bug worth seeing.

As built (`core/src/audio/ring.rs`), `SampleRing<T>` starts at that 4096 and
**grows by doubling to a 131072-sample ceiling** rather than being strictly
fixed. The reason is that the resampler runs on the emulator thread, not the
audio callback — the no-allocation constraint of goal 4 applies to §4's
transport, not here — and several callers legitimately accumulate a full second
before draining (the headless harness, and the resampler's own
sample-count tests). Past the ceiling the ring drops oldest and counts it in
`overruns()`, so the diagnostic this section asks for still exists; it just
reports a consumer that has stopped draining for three seconds rather than for
a tenth of one.

The same `SampleRing` replaced the `audio_buffer: Vec<i16>` mix buffer in the
ten machines that sum several chips before `fill_audio`, which had the identical
front-of-`Vec` drain one level further down the path.

## 3. Rate negotiation — *implemented*

`Pokey::with_clock(clock_hz, sample_rate)` already takes the output rate as a
parameter. Generalise that shape to every device, and thread the real rate from
SDL back through machine construction.

The ordering problem: devices are constructed when the machine is built, but the
granted rate is not known until the audio device is opened, and the frontend
opens audio using `machine.audio_sample_rate()` — which the machine only knows
after its devices exist.

Break the cycle by opening the audio device first with a preferred rate, then
constructing the machine with the granted rate. The registry's `create` already
takes a `RomSet`; adding an audio-config parameter is a mechanical change across
factories. Alternatively, keep construction as-is and add a
`set_output_sample_rate(u32)` that propagates to each device's resampler —
`AudioResampler::set_input_rate` already demonstrates in-place retuning, and the
same phase-folding logic applies to the output side.

The second option is smaller and is the recommended starting point; revisit if
a device turns out to need its rate at construction time.

### As built: neither option, a third

Both options above put the rate in the *call graph* — either a parameter on
every machine factory, or a `set_output_sample_rate` every machine forwards to
its devices. Both mean about forty machines carrying a value that can only ever
have one answer per run, because there is one host audio device.

So the rate lives in one place instead:
`phosphor_core::audio::host_sample_rate()` / `set_host_sample_rate()`, a
process-wide value defaulting to 44_100. Devices read it when they construct
their resamplers. About thirty hardcoded `44_100`s — device constructors,
per-file `const OUTPUT_SAMPLE_RATE`, and every machine's `audio_sample_rate()` —
now go through it. A per-file `const` becomes a small `fn`, since a `const`
cannot call one.

The ordering problem is answered by the first option's insight, just without the
parameter: the frontend opens a throwaway playback device to learn the granted
rate, calls `set_host_sample_rate`, and *then* builds the machine.
`emulator::init_sdl` exists so `main` can bring SDL up before construction and
hand the context to `run` — the alternative was an SDL init/quit/init cycle,
which is a worse thing to depend on.

The cost of this shape is one ordering rule: set the rate before building a
machine, because a device that already exists keeps the rate it was built with.
That rule is obeyed at exactly one call site, and `set_host_sample_rate`
documents it. Retuning a *live* machine's output rate is a different problem,
and §5 needs it — see the note there.

Acceptance is `machines/tests/audio_rate_test.rs`, in its own test binary
because the value is process-wide: at 48 kHz every registered machine reports
48 kHz, and a second of frames from a POKEY machine and a WSG machine each
produces a second of samples. Verified against a real device too — asking for
48_000 grants it, and the machine is built for it.

## 4. Lock-free transport — *implemented*

Replace `Arc<Mutex<VecDeque<i16>>>` with a single-producer / single-consumer ring
over a fixed `[i16; N]` and two atomic indices — the emulator thread owns the
write index, the callback owns the read index, and neither ever blocks.

Roughly 60 lines and no new dependency. The existing fade-in/fade-out ramp and
hold-last-sample-on-underrun behaviour are preserved exactly; only the transport
underneath them changes.

This is also a precondition for section 5, which needs to read fill level from
the emulator thread without taking a lock.

### As built

- The slots are `AtomicI16`, not a raw `[i16; N]` behind an `UnsafeCell`. A
  relaxed 16-bit load or store is a plain machine load or store, and the
  release/acquire pair on the indices is what actually publishes the samples —
  so the whole ring is safe Rust at no cost. Goal 4 (never lock, allocate or
  syscall on the callback) holds by construction, and the callback now pops
  straight into SDL's output buffer, so even its scratch `Vec` is gone.
- **A full ring turns away the newest samples, not the oldest.** The old
  `VecDeque` evicted from the front; a producer cannot safely move an index the
  consumer owns. Latency is unaffected — both leave the ring full — and only
  the choice of which samples are lost differs.
- Playback waits for the ring to reach the setpoint before the device is
  resumed. Starting on a nearly-empty ring guarantees underruns until the
  emulator gets ahead; ~90 ms of startup delay buys a margin against frame-time
  jitter.
- Both directions are counted: `dropped()` (producer could not fit) and
  `starved()` (consumer asked for more than the ring held). §5 is what makes
  these fault indications rather than the steady state.

## 5. Clock synchronisation — *implemented*

With a lock-free ring, the emulator can cheaply read how full it is, and that
number is a direct measurement of the phase between the two clocks. Steer the
resampler's output rate to hold it near a setpoint:

```text
  error   = fill_level - target_fill          (target ≈ half the ring)
  trim    = clamp(-Kp * error, -0.005, +0.005)
  rate    = nominal_rate * (1 + trim)
```

A ±0.5% authority is far more than the tens of ppm of real drift, so the loop
has ample headroom, and a 0.5% pitch deviation is well under the ~1% threshold
where pitch change becomes noticeable. Choose `Kp` for a time constant of
several seconds: this must correct drift, not chase per-frame jitter, and a fast
loop would modulate pitch audibly.

`AudioResampler::set_input_rate` already supports retuning without discarding
phase or buffered output, which is exactly the primitive this needs.

The result is that dropped samples and underruns become genuine fault
indications rather than the normal steady state — which is what makes the
overrun counter from section 2 worth having.

### As built: trim the frame period, not the sample rate

The control law is exactly as above. What it steers is not.

Trimming the resampler's output rate makes the machine emit `N·(1+trim)`
samples per emulated frame. The same audio is then spread over more samples and
played back at the card's fixed rate, so the machine is **detuned** by up to
0.5% — about 8.6 cents, which is audible on sustained tones. It also needs every
device's resampler retuned in step, which is precisely the per-machine
propagation §3 avoided.

Trimming the *frame period* instead reaches the same place from the other side:
the machine still emits exactly `N` samples per frame, but frames happen
slightly more or less often in wall-clock time, so audio is produced faster or
slower without a single sample being altered. **Pitch stays exact**, and what
moves is emulation speed, by at most 0.5%.

That is also a better answer to the problem this section names. The complaint
was that video is paced off the host monotonic clock while audio is consumed off
the sound card's crystal, with nothing reconciling them. Steering the frame
period makes video follow the audio clock, so the two are locked rather than
merely both corrected. And it lands in one place — the frontend's throttle — so
no machine or device is touched at all.

Gain and authority: `CLOCK_GAIN = 0.01` over a 4096-sample setpoint gives a time
constant of about 9 seconds at 44.1 kHz, slow enough that the loop cancels
drift without chasing per-frame jitter. Being proportional-only it holds a
standing offset proportional to the drift it cancels — 200 ppm settles about 82
samples off setpoint, negligible against a 4096-sample margin — so there is no
reason to add integral action and its wind-up.

The loop is engaged only once playback has started; while the ring is
prefilling, its low fill level is by design and not something to correct.

Two things the fault counters must *not* report, both learned by watching them:

- **Host warm-up.** For the first second or two after the window appears the
  emulator misses frame deadlines — shader compilation, font atlas upload, cold
  pages — and starves the ring by ~13k samples. That has nothing to do with
  clock drift, and a counter that latched it would read as a permanent fault.
  The frontend takes a baseline three seconds after playback starts and reports
  only what moves beyond it.
- **Running unthrottled.** Fast-forward legitimately produces faster than the
  card consumes, so the overrun is the expected consequence rather than a
  defect. The clock loop is skipped entirely in that mode, and the message says
  so.

Acceptance: `the_clock_loop_settles_against_a_mismatched_consumer` simulates
twenty minutes against a 200 ppm mismatch (an order of magnitude worse than a
real crystal) and asserts the ring neither fills nor empties. Confirmed on real
hardware over an unbroken 11 min 51 s, with fill oscillating between about 3650
and 4700 around the 4096 setpoint and neither counter moving. The phase called
for 30 minutes; the run was ended deliberately at that point, not by a fault.

## Phasing

Each phase is independently shippable and independently valuable.

| Phase | Work | Why this order |
|-------|------|----------------|
| 1 | Output ring (§2) — **done** | Smallest, touches only `AudioResampler` internals, no behaviour change |
| 2 | Two-stage decimation (§1) — **done** | The audible win; independent of everything else |
| 3 | Rate negotiation (§3) — **done** | Mechanical; needed before the control loop has a correct nominal rate |
| 4 | Lock-free transport (§4) — **done** | Removes the priority inversion; precondition for phase 5 |
| 5 | Clock synchronisation (§5) — **done** | Needs phases 3 and 4 in place |

## Testing

- **Stopband rejection.** Drive the resampler with a tone above the output
  Nyquist and assert the aliased energy in the output is at least 60 dB below a
  reference in-band tone. This is the test that would have caught the current
  behaviour, and it is the acceptance gate for phase 2.
- **Existing resampler tests stay green unchanged.** `core/src/audio/mod.rs`
  already covers sample counts in both directions, box averaging, upsample
  hold, drain semantics, reset, and save/load round trip. They encode the
  contract; phase 2 must not need them rewritten, only the averaging-specific
  assertions relaxed to tolerances.
- **Upsampling regression.** Keep `resampler_upsamples_to_full_output_count`
  (`mod.rs:253`) exactly as it is. It pins a real past bug.
- **No allocation on the callback.** Assert structurally by construction; a
  debug assertion on the callback path is a reasonable belt-and-braces.
- **Drift.** Run a machine headless for a simulated long session with a
  deliberately mismatched consumer rate and assert the control loop settles and
  holds without drops or underruns.
- **By ear.** `--record-wav` already exists (`frontend/src/emulator.rs:1277`).
  Capture the same machine and input sequence before and after phase 2 and
  compare. Galaga, Tempest and Q*bert are good candidates — bright effects,
  four-POKEY mixing, and speech respectively.

  *Done: all three sound correct after the change.* This was the gate the
  measurements could not stand in for — 91 dB of stopband rejection says the
  aliasing is gone, but only listening confirms nothing else broke on the way,
  and speech through the upsampling path (Q*bert) was the case most at risk
  since stage one still holds rather than interpolating.

## References

- `core/src/audio/mod.rs` — the resampler being replaced
- `frontend/src/audio.rs` — the SDL transport being replaced
- `docs/designs/discrete-sound-framework.md` — board-level analog paths, which
  feed into this path rather than changing it
