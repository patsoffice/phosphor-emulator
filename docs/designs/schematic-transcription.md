# Design: Schematic Transcription

> **Status: rungs 1 and 3 done, rung 4 cut, rung 2 open.** The kill criterion
> is settled and its two questions disagreed; see the bottom of this file.
> Make a board's transcription a single piece of data
> rather than three prose copies of itself: a pin-level netlist per board, lints
> over it, generated Rust constants, and an offline solver that can tell a
> misreading from a modeling approximation. Scoped to a probe on one board with
> a stated kill criterion, because the fleet migration is the expensive part and
> should not start until the probe reports.
>
> Sibling of `discrete-sound-fidelity.md` (tooling, closed) and
> `discrete-sound-framework.md` (the runtime). Neither of those non-goals is
> reversed here: this is about the transcription workflow, not the runtime.

## Context

Zaxxon's sound board is the most heavily read schematic in this repo. Six passes
took it from nothing to twelve modeled voices, and `phosphor-emulator-uy54`
closed with residuals rather than with the voices right. The residuals are
interesting; the *way* they resisted is more interesting, and it is a tooling
problem rather than a circuit one.

**The transcription of that board exists in three places and none of them is
authoritative.**

1. `docs/schematics/zaxxon-discrete-sound.md`, 2400 lines of prose, carries
   every value and every junction as English.
2. `docs/schematics/zaxxon-*.json` looks like a netlist and is not one. It is
   [netlistsvg](https://github.com/nturley/netlistsvg) input, written to render
   an SVG. Values live inside display strings (`"type": "R143 3k3 / R144 560"`),
   cells are block-level lumps rather than parts, and nets are bare integers.
   Nothing can compute from it.
3. `machines/src/zaxxon_sound.rs` has the values as `const R156: f64 = 33_000.0`
   and the *junctions* only implicitly, in how each node happens to be wired in
   code.

Three consequences, all of them observed rather than hypothesized.

**Omissions are invisible.** `C94`, 680 pF on the pin the drawing labels `RO`,
is in none of the three. So are `C75`, `C77`, `C86`, and the same part on every
other `MB4391` channel. Six passes did not notice, and the file had actively
talked itself out of the class: it called them "the part's own rolloff pin and
not in the signal path". The capacitor was found by cropping that region of the
sheet for an unrelated reason. **There is no artifact against which "you did not
transcribe this part" is a question anyone can ask.**

The reading itself is since corroborated on a second board: another Sega sound
board of the same era carries an `MB4391` with 680 pF from its `RO` pin to
ground, the same part on the same pin. So the kill criterion below tests the
lint rather than the capacitor.

**Copies drift.** The battleship and laser rates appeared in the prose and again
in a doc comment as 122.5, 322.6 and 579.4 Hz. Each is its current value times
`10.4/10.0`, the span change from splitting the op-amps' rails, made in a third
place. The device had been emitting 117.6 Hz the whole time, asserted by its own
test. Two hand-copied numbers went stale against a constant neither of them
mentioned.

**A residual cannot be attributed.** The shot's head sits an octave above the
board's. The possible causes are a misread value, a misread junction, a missing
part, a part property the drawing does not give, or the modeling approximation
itself. A pass swept five constants, closed five doors, and could not reach the
sixth, because there is no way to ask which class the error is in. The framework
represents a node as a filter chain rather than solving the network, and for
this voice node X is a superposition of two one-pole sections standing in for a
genuinely two-capacitor network. The file justifies that with "the two poles are
26 ms and 760 ms, twenty-nine to one, which is exactly the separation that lets
a two-capacitor network be written as two independent RCs". **That is an
assertion and nothing has ever checked it**, and the head is exactly where the
two capacitors interact.

### Why this is not the framework's non-goal

`discrete-sound-framework.md` says "not a general-purpose SPICE simulator" and
"not a text netlist parser in v1". Both are about the **runtime**, and both
remain correct: nodal iteration at 48 kHz across eleven voices and two CPUs is
not going to happen, and the filter-chain approach is fine once something can
establish that it is fine. Nothing here runs during emulation.

## Goals

- One authoritative, pin-level netlist per board, in a format that can be
  computed from.
- "Which parts on the sheet does the device not model" becomes a query.
- Value constants stop being hand-copied.
- A residual can be attributed to a reading error or to a modeling
  approximation, rather than to neither.

## Non-Goals

- Not a runtime solver. See above.
- Not a schematic capture tool. Redrawing boards in KiCad produces a prettier
  drawing, and the drawing is not what is scarce. The reading is.
- Not a replacement for the prose. `zaxxon-discrete-sound.md` carries the
  *argument* for each reading, which is the part that has repeatedly saved a
  later pass. The netlist carries the result. The prose should reference it
  instead of restating it.
- Not the fleet migration, yet. `targets.toml` has 10 `missing`, 10 `partial`,
  7 `implemented-needs-validation` and 2 `implemented-unvalidated` rows, so this
  amortizes well, but none of that starts before the probe reports.

## Decisions

### 1. The netlist is parts and nets, at pin level

A part has a designator, a kind, a typed value, and a pin list. A net has a name
and the `designator.pin` endpoints on it. That is the whole model.

Pin level is the point. Every expensive error on this board was a pin-level
fact that a block diagram cannot express: `R96` landing on the integrator's pin
6 rather than on the divider that feeds pin 5 (worth a factor of four in the
audible rate), `U10`(12,13,14) tapping the Schmitt's hysteresis node rather than
its output, `R92` leaving an op-amp and arriving at a node that is already a
follower's own output, and the `Qbar` versus `Q` reading that inverts a voice.

### 2. Values are typed, not display strings

`{ "kind": "R", "ohms": 33000 }`, not `"R156 33K"`. Anything that has to be
parsed back out of a label will be, wrongly, eventually. This is the single
difference that makes the file computable and is why the existing netlistsvg
JSON cannot simply be adopted.

### 3. Pins that go nowhere are declared, not absent

> **Amended by rung 1, which found a third state rather than two.** A drawn pin
> is also allowed to be `unread`, with a reason, and that makes its part
> partial. The two-state rule is right for a *finished* transcription and
> unreachable during one: the shot voice has `R160`, `D11` and `Q7`'s emitter
> whose junctions the prose never established, and the only alternatives to
> saying so were to guess them or to leave the parts out. Leaving parts out is
> the exact failure this design exists to stop. So `unread` is a to-do item
> that every report counts, not a suppression, and "we did not check" stays
> separable from "we genuinely do not know" the way decision 7 asks.

The drawing has several, and they are load-bearing facts rather than omissions:
`U18`'s `Q` on pin 13 is drawn and connects to nothing, and reading it as the
driver would invert the shot. `U7`'s pin 3 is absent from the symbol entirely,
which is what says that 555 is a ramp generator read at its capacitor. `PB0-PB3`
on the PPI are drawn and go nowhere.

So a pin is on a net, or it is marked `nc` with a note, and the lint rejects the
third case. **"Absent from the symbol" and "drawn and unconnected" are different
states and the format must keep them apart.**

### 4. The SVGs are generated from the netlist, not maintained beside it

Otherwise this becomes a fourth copy, which is the failure it exists to fix. The
existing `*.json` files become build products and the `*.svg` keep working.
Block-level grouping for the picture is presentation, expressed as an annotation
on parts rather than as a separate hand-built file.

### 5. Codegen covers values, and that is the smallest of the three payoffs

> **Withdrawn. Codegen is not being built.** The kill criterion asked whether
> it would have caught the 122.5 Hz drift and the answer, traced rather than
> assumed, is no. This decision's own last sentence is why. See the kill
> criterion below for the working.

Generated `const R156: f64 = 33_000.0` kills the drift class, which is real:
it is what let 122.5 Hz survive a span change. But it does not touch the
**derived** quantities, and those are where this board's real errors lived.
`shot_hz_per_volt()` computes from `R156`, `R159`, `C92` and a Schmitt window;
`shot_pitch_r()` encodes a judgement about which resistor sits at an AC ground
at the frequency in question. Codegen gives the inputs, not the derivation.

Ranked by what they would have bought on this board: **lints first, oracle
second, codegen third.** Plan accordingly, and do not let codegen lead because
it is the easiest to picture.

The claim in the first sentence is the one that did not survive contact. The
drift was never in a *value*: `battleship_hz()` computed from its part
constants the whole time and its test asserted the right answer throughout.
What went stale was two Hz figures hand-copied into prose, against a change to
`opamp_span`, which is an inferred part property rather than anything a netlist
holds. Generated part constants sit outside that chain at both ends.

### 6. The oracle is offline and staged, and the cheap rung may be enough

Given a transcribed netlist there are three things to compare rather than two,
and the third breaks the tie that stopped the last pass:

| solver vs board | device vs solver | conclusion |
|---|---|---|
| agrees | agrees | done; the residual is the cabinet or the recording |
| agrees | **disagrees** | **the reading is right and the approximation is wrong** |
| **disagrees** | agrees | **the reading is wrong**: value, junction, or missing part |
| disagrees | disagrees | both, or a part property the drawing does not give |

Worked example, from the pass that could not finish. Our node A peaks at
**2.07 V**, traced out of the running device; the board's has to peak near
**0.5 V**. If the solver also says 2.07, the reading is right and the gap is a
part property, and since the peak is bounded by `R153` against `R155`'s share of
the 555's own output high, that is a *specific* pair of parts to re-read at
400 dpi. If the solver says 0.5, the reading is wrong and a diff of the netlist
against the device says where. Either is a next step. "Swept five constants,
nothing moved" is not.

**Two rungs, and the first is small.** The highest-value question is narrow: is
the one-pole superposition on nodes X and Y valid? A nodal solver over just the
passive network, driven by the same stimulus, answers exactly that. It is linear
algebra on a handful of nodes, needs no external dependency, cannot fail to
converge, and requires modeling no digital part. Build that first.

Full SPICE export is the expensive rung. It buys the part properties: an
`LM324` macromodel knows it can only sink 12 to 50 uA at 200 mV, so running
`U19`(1,2,3) against the 555's pin-5 divider as its real load produces
`OPAMP_V_LOW` as an **output** rather than as the part-class inference it is
today. The same goes for the 555's loaded output high, which is the other term
in the bound above. It also costs: `74123`, `74LS139` and `4016B` have to become
behavioral sources, and relaxation oscillators with idealized comparators are
where ngspice convergence gets unpleasant.

### 7. Some unknowns survive the oracle, and saying which is a result

The `MB4391` has no datasheet on the open web and no SPICE model; MAME had to
guess it too, which is why its Border Line netlist passes `RO` into the model
and never uses it. The `MCD-725H`'s curve is the same wall. The oracle does not
dissolve these. What it does is separate **"we genuinely do not know"** from
**"we did not check"**, which are mixed together in the same `INVENTED` list
today.

## The ladder

Each rung is a child issue. The probe is rungs 1 to 3.

### 1. Format and loader

Schema, a loader, and the round trip to netlistsvg so the existing SVGs keep
building. Small. The decision point: if the format cannot express something the
Zaxxon sheets contain, better to find out before 200 parts are typed into it.

> **Done.** `tools/netlist`, with
> `docs/schematics/netlists/zaxxon-sound.toml` as the probe: 42
> parts, 32 nets, and `docs/schematics/zaxxon-shot-oscillator.json` generated
> from it. Four things the decision point turned up, which is what it was for.
>
> - **Multi-section parts needed an answer and got one.** An `LM324` is one
>   part with one set of pin numbers and four boxes on the drawing. Sections
>   are presentation beside `block`, so `U19.6` stays the fact and `U19b` stays
>   the picture.
> - **A third pin state exists.** See the amendment to decision 3.
> - **"Same SVG out" is the wrong target and the round trip is still good.**
>   The generated drawing has the same parts, values and pin numbers and is
>   busier, because the hand-built file bought its legibility by lumping parts
>   into blocks. Two of those lumps were *wrong*: `R148`/`C89` and `R155`/`C91`
>   were drawn in series where the prose's own node table has each pair going
>   to ground. A drawing that cannot be held against the table is how that
>   survived, so an exact match would have been a bad outcome.
> - **The value keys carry their unit** (`pf`, `kohms`, `uf`), so `C88` is
>   written `uf = 0.047` the way the drawing prints it and read back as farads.
>   The unit is in the key rather than in a string, which is decision 2 without
>   making the file unreadable.

### 2. Transcribe Zaxxon sheets 11 and 12

About 200 parts, at pin level, including parts the device does not model. That
last clause is the whole point and is what makes rung 3 able to find `C94`.

This board is chosen because it was read six times and its reading is fresh, so
a disagreement between the netlist and the device is most likely to be a real
finding rather than a transcription slip in the new file.

### 3. Lints

Cheap, and each one catches an error this board actually had:

| lint | what it would have caught |
|---|---|
| net with fewer than two endpoints | a part transcribed with one end floating |
| pin neither on a net nor marked `nc` | a pin read but never placed |
| part in the netlist that no device node references | **`C94` and every other `RO` capacitor** |
| part referenced by the device but absent from the netlist | a constant with no part behind it |
| two parts with the same designator | a copy-paste during transcription |

The third row is the one that matters. It is the question nobody could ask.

> **Done.** `netlist lint <file> --device <source>`. Against the shot voice the
> third row returns ten parts and `C94` is one of them, found without being told
> the `RO` capacitors exist. The other nine are all real: `C87`/`R142`, the
> one-shot's timing network; `R157`/`R158`, the divider the device folds into a
> factor of 0.5; `R160`/`R163`, `Q7`'s base drive, which the device treats as an
> ideal switch; `C93`, the VCA coupling; and `R203`/`R204`, the mix leg the
> device holds as one weight.
>
> **The device's part list comes from its constants' names and never from its
> comments**, and on this board that is the entire result. `zaxxon_sound.rs`
> mentions `C94` exactly once, in a doc comment saying it is *not* modeled. A
> check that read comments would have counted that as coverage and reported
> nothing.
>
> Rows two and five are not implemented, deliberately: the loader refuses a file
> that breaks them, along with a pin on two nets. A rule the format carries beats
> a rule a tool reports, so `lint` names them as enforced instead.
>
> Row four declines to run against an excerpt, where it would otherwise report
> every part of every other voice.

### 4. Codegen

> **Cut. Not being built.** The kill criterion asked whether this would have
> caught the 122.5 Hz drift and the answer is no, for the reason decision 5
> already gave: codegen supplies inputs, and the drift was in a derived figure
> copied into prose. See the kill criterion.
>
> One consequence to carry: rung 3's device part list was specified as a
> stopgap that codegen would retire. It is now permanent, so it has to stay
> honest on its own. It derives from constant names rather than a hand-kept
> manifest, which is what makes that acceptable: it cannot go stale, because it
> *is* the code.

Generated constants for one board, and `zaxxon_sound.rs` wired to them. See
decision 5 for what this does and does not buy.

### 5. Passive-network solver

The cheap oracle rung. Nodal analysis over the passive subnetwork, same stimulus
as the scenario, compared against both the device's traced node and the
reference recording. First question to point it at: node X's two-capacitor
superposition.

### 6. SPICE export

The expensive rung, and optional. Only worth starting if rung 5 answers its
question and a part-property question is still blocking a board.

> **Cheaper than costed, and the target should change.** Decision 6 prices this
> rung as "`74123`, `74LS139` and `4016B` have to become behavioral sources",
> which is true of ngspice and not of `nltool`, the offline netlist runner that
> ships with the reference emulator this project already reads. Its device
> library has all three as parts, and an `LM324` built on a real op-amp
> macromodel, which is exactly what decision 6 wanted in order to make
> `OPAMP_V_LOW` an output rather than a part-class inference. Its source format
> is parts and nets at pin level, so emitting it from a transcription is
> mechanical: `RES(R133, RES_K(51))`, `CAP(C26, CAP_P(680))`,
> `NET_C(IC20.14, C26.1)`, and an explicit `NC_()` for a drawn-and-open pin
> that independently arrives at decision 3.
>
> Two limits. Conversion only runs inward, from SPICE and EDA formats, so this
> means we emit rather than that anything reads ours. And the `MB4391` is a
> guessed substitution there too, so the part with no datasheet stays the part
> with no datasheet.
>
> Nothing about this touches the runtime non-goal: source out, the tool run by
> hand, nothing linked.

## Kill criterion

**After rungs 2 and 3, check two things.** Do the lints flag `C94` and its
siblings without being told about them? Would the codegen design have caught the
122.5 Hz drift? If either answer is no, close the epic rather than let it
linger. Both are questions about this specific board, both have known right
answers, and neither takes more than an afternoon to settle.

### Settled, and the two answers disagree

**One: yes.** `netlist lint --device machines/src/zaxxon_sound.rs` over the shot
voice returns `C94` in a list of ten, knowing nothing about `RO` pins. Nine of
the other rows are real gaps too. This is the answer the criterion wanted.

**Two: no**, and it was traced rather than assumed. `battleship_hz()` already
computes from `R93`, `R96`, `C58`, `R98` and `R99`, and its own test asserted
117.6 Hz throughout. The chain that broke was: `opamp_span` moved, the computed
rate moved with it, and two Hz figures that had been hand-copied into prose did
not. Codegen generates part values from the netlist. It does not generate
`opamp_span`, which is an inferred part property rather than a value on any
drawing, and it does not generate prose. It is absent from every link.

### What that failure means, which is not what the criterion assumed

The criterion treated its two questions as interchangeable tests of the same
idea. They are not. The first asks whether a netlist can be queried, which is
the epic's thesis. The second asks whether *codegen* would have caught a
specific drift, and decision 5 had already worked out that codegen supplies
inputs and not derivations, and ranked it third of three for that reason. The
question tested the weakest rung against a failure mode outside the netlist's
reach, and a no was close to predetermined.

So the verdict is **cut rung 4, keep the epic**. The evidence kills codegen
specifically and says nothing against rungs 2, 5 and 6. Reading it as a kill of
the whole ladder would discard a passing first answer on the strength of a
second question that was measuring something else.

The general lesson is worth more than the specific one: a kill criterion with
two questions joined by "if either answer is no" needs both questions to be
tests of the same thing, and these were not written that way.

### What still guards the honest failure mode

The failure mode is that the netlist becomes a fourth copy rather than replacing
three. Decision 4 and rung 4 were named as the two things preventing it, and
rung 4 is now gone, so the guard rests entirely on decision 4: the `.json` is
generated and `render.sh` regenerates it. That is holding for the one file that
exists. It is worth re-checking when the fleet migration starts, because a
hand-edited generated file is exactly how this would fail quietly.
