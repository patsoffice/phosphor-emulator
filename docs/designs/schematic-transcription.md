# Design: Schematic Transcription

> **Status: rungs 1, 2, 3 and 5 done, rung 4 cut, rung 6 unstarted.** The
> kill criterion is settled and its two questions disagreed; see the bottom of
> this file. The probe that followed the cut, generating *derived* constants
> rather than values, passed its own kill criterion; see "Derived constants". Make a board's transcription a single piece of data
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

> **Substantially done: 370 parts across 308 symbols, about forty short.**
> `docs/schematics/netlists/zaxxon-sound.toml`. Sheet 11 whole; on sheet 12 the
> PPI and all twelve gates, the player ship level and its opto-tuned engine,
> both missiles, the laser, both tone filters, the supply, the mute, all seven
> MB4391 channels, all eleven mix legs and the path to the speaker. What is
> missing is listed at the top of that file and every unread pin carries its
> reason, so the transcription states its own gaps.
>
> **What the reading found, beyond confirming the prose.** These are the
> return on transcribing at pin level rather than as blocks:
>
> - **The drawing is wrong on a pin number.** `U9`'s fourth-section output is
>   labeled pin 11, which is the negative supply on a 14-pin quad. `U10`'s
>   identical section says 14, and `U11`'s pin 11 is drawn going to ground two
>   sheet-halves away. Both earlier transcriptions copied the 11 through.
> - **The drawing uses one designator twice.** Two parts are labeled `R127`.
>   `R129` is missing from the run and the device already calls the 47k one
>   that. The loader refuses duplicates, so the file could not load until it
>   was settled.
> - **`LM324` is nowhere on the drawing.** It is a part-class inference, and it
>   supplies the output swing that sets the shot's whole pitch range.
> - **Crossings without junction dots are this drawing's commonest trap**, and
>   there are at least three: `R10`/`R11` over the player ship ladder,
>   `R96`/`R70` over the oscillators' pin-5 dividers, and `C30`/`C39` under the
>   tone filters' nodes. Each is worth a factor or a whole topology.
> - **The explosion and missile envelope followers tap a midpoint**, not the
>   storage capacitor, so what reaches the VCA is `(5 V + V_cap) / 2`.
> - **+6 V is an unregulated divider** that every op-amp reference on both
>   sheets shares behind 500 ohms.
> - **All seven `RO` capacitors** are now in the file, and the lint returns all
>   seven.
>
> **Two format changes the reading forced**, both recorded in decision 3's
> amendment and in the loader: a drawn pin may be `unread` with a reason, and
> a part may carry `also` for the rest of a run drawn as one symbol, because
> this board's bypass capacitors are 63 parts on three symbols.

### 3. Lints

Cheap, and each one catches an error this board actually had:

| lint | what it would have caught |
|---|---|
| net with fewer than two endpoints | a part transcribed with one end floating |
| pin neither on a net nor marked `nc` | a pin read but never placed |
| part in the netlist that no device node references | **`C94` and every other `RO` capacitor** |
| part referenced by the device but absent from the netlist | a constant with no part behind it |
| two parts with the same designator | a copy-paste during transcription |
| device constant and sheet disagree about a value | a value corrected in one file and not the other |

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
>
> **Row six came later**, after rung 5, and is `value-disagrees`. The same trick
> one step further: `lint` already read a constant's *name* to ask which parts
> the device models, and reading the literal beside it holds the two files'
> values together. Against the board today it compares 84 of the device's 111
> readable constants and reports none, which is the first evidence that the
> transcription and the device agree about more than their part lists.
>
> It is not rung 4 returning. Nothing is generated, and a constant whose value
> is an *expression* is skipped rather than guessed at, which is the same line
> decision 5 drew between inputs and derivations. Three things it declines to
> check are named in the code, each because the alternative is a false positive
> and a lint that cries wolf is a lint somebody turns off. The coverage count
> prints whether or not anything disagreed, because "nothing was compared" and
> "nothing disagreed" otherwise read the same.

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

> **Done, and the first question's answer is that the approximation is sound.**
> `netlist solve <file> --group shot`, in `tools/netlist/src/solve.rs`, with the
> board's answers pinned in `tools/netlist/tests/zaxxon_shot_test.rs`.
>
> **The instrument.** Replace every capacitor by a current source, solve the
> resistive network once per capacitor, and the result is the matrix of
> transresistances between capacitor ports. Times the capacitances, its
> eigenvalues are the network's time constants: exactly `k` of them for `k`
> capacitors, rather than one per node. Scaled by the square roots of the
> capacitances the matrix is symmetric, so a Jacobi sweep is enough and nothing
> can fail to converge, which is what decision 6 asked of this rung. The
> **eigenvector** is the part that answers the question, because it says how
> much of each mode appears at each node. That is the difference between "these
> two capacitors each have a time constant" and "this network has two modes and
> both nodes take part in both".
>
> **The answer.** Three numbers in `zaxxon_sound.rs` are derived by treating one
> capacitor as a short or as an open while reading the other. Solving both at
> once:
>
> | | the device, one RC at a time | the network, solved | out by |
> |---|---|---|---|
> | `shot_pitch_r`, `C88` at node X | 26.3 ms | **25.87 ms** | 1.7 % |
> | `shot_vca_release_r`, `C89` at node Y | 759.9 ms | **776.7 ms** | 2.2 % |
> | `shot_pitch_from_y`, Y's share of X | 0.560 | **0.579** | 3.4 % |
>
> The third row is the one nothing could have produced by hand: it is not a
> divider ratio the solver computed, it is how far node X moves in the slow
> mode, and it lands on the divider ratio the device uses.
>
> **The reasoning behind each is confirmed too, not just the answer.**
> `shot_pitch_r` drops `R148` on the argument that `C89` holds node Y at an AC
> ground at 4 Hz; in the fast mode node Y moves **4 %** of what node X does, so
> it does. `shot_vca_release_r` keeps `R145`/`R146` on the argument that `C88`
> is an open at 0.2 Hz, so node X is a divider rather than a ground; in the slow
> mode node X moves 58 % of node Y, so it is. Both superseded readings are
> refuted by the same run: 43 ms is 66 % off the fast pole and 468 ms is 40 %
> off the slow one, which corroborates two corrections that had only ever been
> argued from a recording.
>
> **So the shot's octave is not here**, and that is the result. A door closed is
> what this rung was built to be able to produce, and it is the first time this
> voice's residual has been narrowed by ruling something out rather than by
> sweeping a constant.
>
> **The second question got a narrower answer than it asked.** With the 555
> parked at its loaded output high the solver puts node A at **2.22 V**, against
> the device's traced 2.07 V and the board's required 0.5 V. So the resistive
> reading is right to a couple of percent and decision 6's table reads
> disagrees/agrees the other way round from its worked example: it is the fourth
> row, a part property or something outside the passive network. Which of those
> it is needs the 555's duty against `C91`'s smoothing, and that is a transient
> rather than an operating point. The solver does DC and modes and does not
> pretend to the third.
>
> **What it declines to model is printed rather than assumed.** Every pin of
> every part that is not an R, C or L is an open circuit, and the run lists all
> 41 of them for the shot before it prints a number, because a time constant
> computed with the wrong pin open is a wrong answer that looks like a right
> one. A node that reaches no rail through any resistor is named and excluded
> rather than quietly grounded, and `C94` is exactly that: its only other end is
> the `MB4391`'s rolloff pin, so the honest answer about the part this epic
> exists because of is that the solver has nothing to say. That is decision 7's
> line drawn by the tool instead of by a reader.

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
>
> **Checked, and the paragraph above is wrong about the one part that
> mattered.** nltool 0.14 (nixpkgs `mame` 0.289's `tools` output, cached, runs
> netlist files) has the `74123`, the `555`, the `4016B` as `CD4016_DIP` and the
> `MM5837`. Its `LM324` is not a macromodel. Driven low and swept across loads,
> both `OPAMP(.., "LM324")` and `LM324_DIP` sit at 0.0930 V sinking 8 uA and
> 0.1168 V sinking 24 mA: a fixed floor behind about one ohm. The real part
> sinks 12 to 50 uA at 200 mV. So `OPAMP_V_LOW` would come out of nltool as a
> model parameter, the same kind of inference as ours, and never as an output.
> Two more: its `74139` aborts on instantiation, on the same truth-table check
> that cuts `list-devices` short, and it has no `MB4391` at all rather than a
> guessed one.
>
> **The candidates narrow by arithmetic before any tool runs.** Node A takes
> about 0.216 of the 555's output, so 0.5 V needs a 555 high of about 2.3 V,
> which a 555 sourcing 3 mA does not do: its loaded output high is out. What
> survives is the op-amp's sink into the 555's control pin, a 3.33 kOhm load
> returned to 8 V. The device clamps that pin at 0.1 V, where its 555 runs fast
> and node A averages rather than peaks; a real `LM324` cannot sink 2.4 mA at
> 0.1 V, so the real 555 runs slower. Which way that moves node A is not known,
> and answering it properly needs a transistor-level `LM324`, which means
> ngspice after all.
>
> **Filed as `phosphor-emulator-kfby.4` and deferred**, with the cheapest next
> step written into it: read the datasheet's output voltage at 2.4 mA sink, set
> it as that stage's floor in the device, and compare the shot against the
> recording before building anything.

## The second board

Everything above was built against one schematic, so none of it was known to
generalize. Lunar Lander's audio output is the first test, at
`docs/schematics/netlists/llander-audio.toml`, and it was chosen because
`phosphor-emulator-b72s` names three open residuals that are all
passive-network questions and because the first of them **cannot be reached by
comparison at all**: the throttle's volume law is wrong in our model and wrong
in the reference netlist the same way, so the two agree to 0.15 percentage
points at every setting and only the drawing disagrees.

### The format needed one new concept, and it is not a `drive`

Zaxxon never made the format express a **switched** element. Its `4016B` gates
were opaque parts the solver opened and nothing downstream cared, because every
question on that board was about a network that does not change. This board's
whole question is what happens when three `4066` sections close in eight
combinations, because the same three resistors set the thrust volume *and* the
noise low-pass corner.

So a part may now carry `switches`: a section naming the two pins it joins, the
pin that controls it, and an optional on-resistance. A scenario states the
control net's voltage the way it states any other, and the solver closes the
switch above a threshold. **A switch is deliberately not a `drive`**: holding
the node at a voltage would throw away the resistance the closed leg puts into
the network, which is the thing being asked about. Which switches were closed
prints with the answer rather than with the setup, because a switched network
is a different network per scenario.

### What it found, and the prose it was drafted from was mostly right

The reading order that worked on Zaxxon worked again: draft from the prose,
then check part by part against the scan. Everything
`llander-audio-output.md` established held. What it did not have:

- **`C14` and `C28` are on the sheet and in none of the prose and none of the
  device.** Two supply bypass capacitors. Both are electrically inert for the
  audio, and the solver says so ("both ends held, so it has no voltage to vary")
  rather than being silent about them.
- **`R100`'s lead crosses the 6 kHz gate's output with no junction dot** and
  lands on the 3 kHz gate's. Settled at 600 percent, where the two real
  junctions are unmistakable blobs and the crossing has none. Read the other
  way, both pull-ups land on one gate and the other output floats. That is the
  third board-topology crossing this project has found and the rule that
  catches them is now two for two.
- **`R7` pin 4 is +22 V and pin 11 is ground**, and an `LM324` has one supply
  pair for all four sections. The prose lists "whether the `LM324` sections
  share it" as not established; the package settles it, and the headroom
  argument that says this board does not clip rests on it.
- **The two corner figures in the prose are both low, and increasingly so as
  the throttle falls.** The prose computes 71 Hz at full throttle and 10.6 Hz
  at throttle 1 from the switched resistance alone. Solving the network gives
  **73.1 Hz and 13.9 Hz**, because the hand calculation omits the `4066`'s
  on-resistance and, far more, `R22` and `R26`'s 48.2 kOhm path to +5 V loading
  the same node. The error is 3 percent at full throttle and **31 percent** at
  throttle 1.
- **And it confirms the finding the board was picked for.** From the netlist
  alone, with no arithmetic about which resistors are in parallel, throttle 1
  lands at **0.193** of full against the 0.143 a linear control would give. The
  prose derived 0.192 and 2.6 dB by hand. That is decision 6's second row:
  the reading is right and the approximation is wrong.

### One thing does not generalize, and it is a result

`lint --device` learns which parts a device models by reading its constants'
*names*. On Zaxxon that is 125 designators and the check is the epic's headline.
On Lunar Lander it returns **zero**: `llander_sound.rs` holds eight constants
and not one is named for a part, because that device was built from a reference
emulator's measured levels rather than from the drawing. Every part on the sheet
comes back unmodeled, which is true and is one fact rather than 23.

That is not a bug in the lint and it is not fixable by a better parser. It says
the device-side half of the pipeline assumes a device built part by part from a
schematic, and the fleet contains devices that are not. `not-modeled` now says
so in one line when a device names nothing, so the twenty-three rows are read
as the consequence they are. Any fleet migration has to expect both kinds, and
for the second kind the netlist's value is the solver rather than the lints.

## Derived constants

`phosphor-emulator-kfby.2`. Rung 4 was cut because codegen supplies inputs and
the errors lived in derivations. Rung 5 then made derivations available: the
solver produces `shot_pitch_r`'s quantity with no argument about which resistor
sits at an AC ground, and that argument had been wrong twice. So the probe asked
whether the solver's outputs could replace the hand derivations on one voice,
the shot, where three constants and two of the voice's known-wrong readings
live.

`netlist derive docs/schematics/netlists/zaxxon-sound.derive.toml` solves the
shot's network and writes `machines/src/zaxxon_sound_derived.rs`, which the
device takes as a module. `zaxxon_derive_test.rs` fails while that file differs
from what the spec writes, which is decision 4's guard made a test rather than a
habit: a generated file nobody regenerates is a fourth copy with a header
claiming otherwise.

### Two decisions the probe had to make first

**The output is a time constant, not a resistance.** The device took an R and a
C separately (`rc_high_pass(.., shot_pitch_r(), C88)`), so a generator could
have emitted `tau / C88` and changed nothing downstream. That was rejected: a
mode belongs to the network, not to `C88`, and an "R" computed backwards from it
is a number no part on the sheet has, handed to a builder that multiplies it
straight back. The builder already stored `tau` in all three node kinds, and
`rc_envelope` already took one, so it gained `low_pass_tau` and `high_pass_tau`,
and the device consumes what the generator emits without transforming it.

**A mode is named by its capacitor, never by its time constant.** The rung 5
tests found the fast mode as the one nearest 26 ms. A generator that did that
would be told its answer, and the kill criterion below would test nothing. So
`Mode` now carries how its stored energy divides between the capacitors (exact:
the squared components of the symmetrized eigenvector), and a spec asks for "the
mode of `C88`". The generator refuses a capacitor holding half or less of every
mode's energy, since the network then has no mode that is that capacitor's and a
one-pole section keyed to it is not something the solver can vouch for. On the
shot each mode keeps **97.7 %** of its energy in its own capacitor, which is the
separation argument measured rather than asserted.

### Kill criterion: settled, and both answers are yes

Both questions test one thing: whether the generator derives or transcribes.

**Does it agree with the hand derivation where that is believed right?** Yes,
to within the coupling the one-capacitor reading discards:

| | hand, one capacitor at a time | generated | out by |
|---|---|---|---|
| `SHOT_C88_TAU` | 26.30 ms | **25.74 ms** | 2.1 % |
| `SHOT_C89_TAU` | 759.9 ms | **776.7 ms** | 2.2 % |
| `SHOT_C89_X_PER_Y` | 0.560 | **0.579** | 3.4 % |

The fast mode is 25.74 ms rather than rung 5's 25.87 because the spec holds
`Qbar` at 5 V, as rung 5's pinned tests do; its prose quoted the undriven run.
No part moved.

**Does it reproduce the two corrections without being told them?** Yes, and it
was asked more sharply than "is the answer not 43". Each superseded reading was
a judgment that one capacitor is a short or an open, so the same spec was run
on the two networks those judgments describe:

| | old hand reading | generated, on the network it assumed | generated, as drawn |
|---|---|---|---|
| `C88`'s mode | 42.73 ms | 42.75 ms, `C89` removed | **25.74 ms** |
| `C89`'s mode | 467.5 ms | 467.7 ms, `C88` shorted | **776.7 ms** |

A generator handed an answer would print the same number in all three columns.
This one prints each wrong answer on the wrong network and the right one on the
drawing, so the number is coming from the network. The residue in the middle
column is the 479 ohms the shaper node puts in series, which the hand formula
omits.

### What it found on the way

- **The superseded 468 ms reading was still asserted.** A device test computed
  `C89 * (R147 || R148)` and pinned it at 0.468 under the heading "And the
  decay", and the comment at the envelope's call site still gave the release as
  `R147` in parallel with `R148`. Both outlived the correction they contradicted,
  which is "copies drift" again, and both went with the derivation.
- **`nothing_saturates` was passing on phase.** Wiring in a fast corner 2 %
  shorter made the every-voice mix clip, with the shot's own level unchanged.
  Sweeping the trigger time showed the mix at `OUTPUT_GAIN` 4.3 already clipped
  at six of eight nearby trigger times, *before* this change; the one the test
  used happened to miss. The test now sweeps ten, and `OUTPUT_GAIN` is 3.8, by
  that constant's own rule that the conservative bound is the one to keep. That
  is a separate commit, ahead of the probe.
- **`lint --device` needed one rule it did not have**: only a name made of
  nothing but designators is a part's value. `SHOT_C88_TAU` says the device
  models `C88` and says nothing about its farads. Value coverage went from 102
  of 105 literals to 101 of 101: the three dropped from the denominator
  (`PC1_LED_VF`, `CANNON_R_Q6_ON`, `U11_GAIN`) name parts with no quantity and
  were never compared, and the one comparison lost is `C88`'s, which is
  inherent, because the device no longer holds that value and cannot disagree
  with the sheet about it. The lint also follows a `#[path]` module now, or
  `C88` would have been reported unmodeled on the device that models it most
  exactly.

### What it does not cover, said again

The solver handles linear passive networks. `opamp_span`, `OPAMP_V_LOW`, the
555's duty and the `MB4391`'s rolloff are part properties or nonlinear, and it
produces none of them; those are rung 6 or they stay inferences. On the shot
itself, `D10`'s clamp (`shot_vca_rest_v`) and the attack through it stay
hand-written for the same reason. **The 122.5 Hz drift lived in `opamp_span`,
and nothing here would have caught it.**

Whether to extend this past the shot is a separate decision. The probe says the
mechanism works where a voice's derivation is a passive network the
transcription holds; it does not say how many voices on this board or the fleet
are that shape.

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

The criterion asks for `C94` *and its siblings*, and rung 2 has since settled
that half too: all seven `RO` capacitors on the board, `C33`, `C42`, `C52`,
`C75`, `C77`, `C86` and `C94`, come back from the same query with nothing in
the tool knowing that MB4391s have rolloff pins.

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
