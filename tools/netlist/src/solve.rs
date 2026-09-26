//! Nodal analysis over a transcription's passive subnetwork.
//!
//! Rung 5 of `docs/designs/schematic-transcription.md`, the cheap oracle. The
//! design's decision 6 wants three things to compare rather than two: the
//! board, the device, and a solver reading the same netlist the transcription
//! holds. That third column is what tells a misread value from a modeling
//! approximation, which is the epic's third consequence and the one nothing
//! else addresses.
//!
//! Two things it computes, and they answer different questions.
//!
//! - **The operating point.** Capacitors open, every resistor in, every rail
//!   and every driven pin held. This is what bounds a node: node A's peak is
//!   its DC value with the 555 parked high, and whether that is 2.2 V or 0.5 V
//!   says whether a residual is in the reading or in a part property.
//! - **The natural time constants.** One per capacitor in the analyzed
//!   network, computed from the whole network at once rather than one RC at a
//!   time. This is the instrument for the question the design names first: the
//!   device writes node X as a superposition of two one-pole sections, and the
//!   prose justifies that with a pole separation nothing has ever checked.
//!
//! **Nothing here is a runtime.** `discrete-sound-framework.md` rules out a
//! solver during emulation and that stays ruled out; this is a command run by
//! hand against a file.
//!
//! # What it models, and what it declines to
//!
//! Resistors, capacitors and inductors, which is the whole of it. Every other
//! part is an open circuit, and that is a modeling claim rather than an
//! omission: an op-amp's input really is open to within a few nanoamps, and a
//! 555's discharge pin really is not. So the solver **reports every pin it
//! opened**, by part, and a reader who sees `U18.7` in that list knows the
//! answer is about the network between triggers and not about the oscillator.
//!
//! The same goes for the network's edges. A node that reaches no rail through
//! any resistor cannot be solved for and is not quietly grounded: it is named,
//! with the capacitors that were dropped along with it. `C94` on the MB4391's
//! rolloff pin is the example that matters, because the part at the other end
//! of it has no datasheet, so "the solver can say nothing here" is the correct
//! and useful answer rather than a gap.

use crate::netlist::{Kind, Netlist, Value};
use std::collections::{BTreeMap, BTreeSet};

/// What the solver is told that the netlist cannot say: how many volts a rail
/// carries, and what any part it does not model is holding its pins at.
#[derive(Debug, Clone)]
pub struct Setup {
    /// Rail voltages by net name, overriding what the name itself implies.
    pub rails: BTreeMap<String, f64>,
    /// Nets held by something the solver does not model: an op-amp output, a
    /// logic output, a 555's pin 3. The netlist knows the junction and cannot
    /// know the voltage, so this is where a scenario is stated.
    pub drives: BTreeMap<String, f64>,
    /// Volts above which an analog switch's control counts as closed.
    ///
    /// A `4066` on a 5 V supply driven from TTL, which is every switch this
    /// has met, so half the logic rail is the default. It is a knob rather
    /// than a constant because the number belongs to the scenario and not to
    /// the drawing.
    pub close_above: f64,
    /// Solve every op-amp section whose transcription marks its `+`, `-` and
    /// output as an ideal op-amp: the output supplies whatever current holds
    /// its two inputs equal. Off by default, when every op-amp pin is open.
    ///
    /// Off is right for the modes, which are a property of the passive network
    /// and which this cannot compute with an op-amp in it, and wrong for any
    /// stage with feedback, whose gain is the feedback network's. `ac` is where
    /// this matters.
    pub ideal_op_amps: bool,
}

impl Default for Setup {
    fn default() -> Setup {
        Setup {
            rails: BTreeMap::new(),
            drives: BTreeMap::new(),
            close_above: 2.5,
            ideal_op_amps: false,
        }
    }
}

/// A complex number, for the small-signal solve. Written here rather than
/// taken from a crate, for the reason the arithmetic section gives.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Cx {
    /// Real part.
    pub re: f64,
    /// Imaginary part.
    pub im: f64,
}

impl Cx {
    /// A complex number from its parts.
    pub const fn new(re: f64, im: f64) -> Cx {
        Cx { re, im }
    }

    /// The magnitude.
    pub fn abs(self) -> f64 {
        self.re.hypot(self.im)
    }

    /// The phase, in degrees.
    pub fn degrees(self) -> f64 {
        self.im.atan2(self.re).to_degrees()
    }
}

impl std::ops::Add for Cx {
    type Output = Cx;
    fn add(self, o: Cx) -> Cx {
        Cx::new(self.re + o.re, self.im + o.im)
    }
}

impl std::ops::Sub for Cx {
    type Output = Cx;
    fn sub(self, o: Cx) -> Cx {
        Cx::new(self.re - o.re, self.im - o.im)
    }
}

impl std::ops::Mul for Cx {
    type Output = Cx;
    fn mul(self, o: Cx) -> Cx {
        Cx::new(
            self.re * o.re - self.im * o.im,
            self.re * o.im + self.im * o.re,
        )
    }
}

impl std::ops::Div for Cx {
    type Output = Cx;
    fn div(self, o: Cx) -> Cx {
        let d = o.re * o.re + o.im * o.im;
        Cx::new(
            (self.re * o.re + self.im * o.im) / d,
            (self.im * o.re - self.re * o.im) / d,
        )
    }
}

impl std::ops::AddAssign for Cx {
    fn add_assign(&mut self, o: Cx) {
        *self = *self + o;
    }
}

impl std::ops::SubAssign for Cx {
    fn sub_assign(&mut self, o: Cx) {
        *self = *self - o;
    }
}

/// Every free node's response at one frequency, per volt of the source.
#[derive(Debug, Clone)]
pub struct AcPoint {
    /// Hertz.
    pub hz: f64,
    /// Each free node's complex response, in the order of `Network::free`.
    pub nodes: Vec<(String, Cx)>,
}

impl AcPoint {
    /// One node's response, if the node is free.
    pub fn at(&self, net: &str) -> Option<Cx> {
        self.nodes.iter().find(|(n, _)| n == net).map(|(_, v)| *v)
    }
}

/// An op-amp section solved as ideal: the terms its pins are on.
#[derive(Debug, Clone, Copy)]
struct Amp {
    plus: Term,
    minus: Term,
    /// Index into `free`: an output a drive holds is not an op-amp here.
    out: usize,
}

/// A voltage the solver was given rather than computed.
#[derive(Debug, Clone)]
pub struct Held {
    /// The net.
    pub net: String,
    /// What it is held at.
    pub volts: f64,
    /// Whether it is a rail or a stated drive.
    pub rail: bool,
}

/// One natural time constant of the passive network, with the node voltages
/// that move together in it.
///
/// A mode is the thing an independent-RC reading approximates. Two capacitors
/// in one network do not in general give one time constant each: they give two
/// modes, each of which lives on both nodes in some proportion. The shape is
/// that proportion, and it is what says whether "two independent RCs" is a
/// description of this network or a wish.
#[derive(Debug, Clone)]
pub struct Mode {
    /// Seconds.
    pub tau: f64,
    /// Node voltages in this mode, scaled so the largest is 1, largest first.
    /// A mode confined to one node has one entry near 1 and the rest near 0.
    pub shape: Vec<(String, f64)>,
    /// How the mode's stored energy divides between the capacitors, as
    /// fractions summing to 1, largest first.
    ///
    /// This is what a mode is *named* by. A mode belongs to the network rather
    /// than to any one capacitor, but where one capacitor holds nearly all of
    /// its energy, "the mode that lives in `C88`" is an exact description and
    /// needs no time constant to find it. Identifying a mode by where its
    /// time constant sits would be telling the solver the answer.
    pub energy: Vec<(String, f64)>,
}

/// What the solver did with one analog-switch section, and why.
///
/// Always reported, closed or open. A switch is the one element whose presence
/// in the network is a scenario's choice rather than the drawing's, so "which
/// switches were closed" is part of the answer rather than part of the setup.
#[derive(Debug, Clone)]
pub struct SwitchState {
    /// The part and section, as `P5.d`.
    pub section: String,
    /// The net its control pin is on.
    pub control: String,
    /// Whether the solver closed it.
    pub closed: bool,
    /// What it was closed with, in ohms, where it is closed.
    pub ohms: Option<f64>,
    /// The reason, in a few words.
    pub why: String,
}

/// A closed switch whose part gives no on-resistance still has to be a number.
///
/// A milliohm rather than a zero, because merging two nodes is a different
/// piece of machinery and this keeps a switch an ordinary branch. Against the
/// kilohms these switches feed it is seven orders down, which is an ideal
/// switch to any precision the drawing supports, and every report says when
/// this stood in for a real figure.
const IDEAL_SWITCH_OHMS: f64 = 1e-3;

/// The passive network the solver extracted, and everything it assumed to get
/// there.
#[derive(Debug, Clone)]
pub struct Network {
    /// Nets held at a voltage.
    pub held: Vec<Held>,
    /// What each analog-switch section was set to.
    pub switches: Vec<SwitchState>,
    /// The unknown nodes, in matrix order.
    pub free: Vec<String>,
    /// Every pin the solver treated as an open circuit, as `U19.12`, sorted.
    /// A reader checks their own question against this list.
    pub opened: Vec<String>,
    /// What the solver left out, and why. Each entry is a sentence.
    pub excluded: Vec<String>,
    /// The op-amp sections solved as ideal, as `R7.b: + +5V, - R7b summing
    /// node, out band-pass out`. Empty unless the setup asked for them.
    pub op_amps: Vec<String>,
    /// Conductance branches, in siemens.
    resistive: Vec<Branch>,
    /// Capacitance branches, in farads.
    capacitive: Vec<Branch>,
    /// The ideal op-amps, in the order of `op_amps`.
    amps: Vec<Amp>,
}

/// One end of a branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Term {
    /// A net held at a known voltage: index into `held`.
    Held(usize),
    /// An unknown node: index into `free`.
    Free(usize),
}

/// A two-terminal element, reduced to a number and two nodes.
#[derive(Debug, Clone)]
struct Branch {
    /// The designator, for reports.
    part: String,
    a: Term,
    b: Term,
    /// Siemens or farads, by which list it is in.
    value: f64,
}

impl Network {
    /// Extract the passive network a transcription describes.
    ///
    /// The order of business is: decide what is held, reduce every resistor,
    /// capacitor and inductor to a branch, find which nodes a rail can
    /// actually reach through those branches, and throw the rest away out
    /// loud.
    pub fn build(netlist: &Netlist, setup: &Setup) -> Result<Network, Vec<String>> {
        let mut errors = Vec::new();
        let mut excluded = Vec::new();

        // A net is held if it is a rail whose voltage is known, or if the
        // scenario names it. A drive wins: the whole point of one is to say
        // that something the solver does not model is holding this node.
        let mut held: Vec<Held> = Vec::new();
        let mut held_of: BTreeMap<&str, usize> = BTreeMap::new();
        let mut unvalued_rails: BTreeSet<&str> = BTreeSet::new();
        for net in &netlist.nets {
            let name = net.name.as_str();
            let stated = setup.drives.get(name).copied();
            let rail = setup
                .rails
                .get(name)
                .copied()
                .or_else(|| if net.rail { rail_volts(name) } else { None });
            let (volts, is_rail) = match (stated, rail) {
                (Some(v), _) => (v, false),
                (None, Some(v)) => (v, true),
                (None, None) => {
                    if net.rail {
                        unvalued_rails.insert(name);
                    }
                    continue;
                }
            };
            held_of.insert(name, held.len());
            held.push(Held {
                net: net.name.clone(),
                volts,
                rail: is_rail,
            });
        }
        for name in &setup.drives {
            if netlist.nets.iter().all(|net| &net.name != name.0) {
                errors.push(format!(
                    "--drive names `{}`, which is not a net in this file",
                    name.0
                ));
            }
        }

        // Analog switches, before anything else, because a closed one is a
        // branch and therefore changes which nodes are reachable at all.
        let mut switches: Vec<SwitchState> = Vec::new();
        let mut closed_pins: BTreeSet<(&str, &str)> = BTreeSet::new();
        let mut switch_branches: Vec<(&str, [&str; 2], f64)> = Vec::new();
        for part in &netlist.parts {
            for switch in &part.switches {
                let section = format!("{}.{}", part.designator, switch.name);
                let control = netlist.net_of(&part.designator, &switch.control);
                let held_at = control
                    .and_then(|net| held_of.get(net.name.as_str()))
                    .map(|i| held[*i].volts);
                let control_name = control.map_or("(not on a net)", |net| net.name.as_str());
                let (closed, why) = match held_at {
                    Some(volts) if volts >= setup.close_above => {
                        (true, format!("{control_name} is held at {volts} V"))
                    }
                    Some(volts) => (false, format!("{control_name} is held at {volts} V")),
                    // Nobody said what the control is doing, so the honest
                    // reading is that the switch state is not known. Open is
                    // the safe default and the report says it was a default.
                    None => (
                        false,
                        format!("nothing states {control_name}, so the state is unknown"),
                    ),
                };
                let ohms = closed.then(|| switch.ohms.unwrap_or(IDEAL_SWITCH_OHMS));
                if closed {
                    let ends: Vec<&str> = switch
                        .pins
                        .iter()
                        .filter_map(|pin| {
                            closed_pins.insert((part.designator.as_str(), pin.as_str()));
                            netlist
                                .net_of(&part.designator, pin)
                                .map(|net| net.name.as_str())
                        })
                        .collect();
                    if let [a, b] = ends[..] {
                        switch_branches.push((
                            part.designator.as_str(),
                            [a, b],
                            1.0 / ohms.expect("closed switches carry a resistance"),
                        ));
                    }
                }
                switches.push(SwitchState {
                    section,
                    control: control_name.to_string(),
                    closed,
                    ohms,
                    why,
                });
            }
        }

        // Op-amp sections the transcription marks, when the setup asks for them
        // to be ideal. Their three pins are connected rather than open, so they
        // are set aside before the open pins are collected; one that cannot be
        // solved is put back below, and says why.
        let mut amp_specs: Vec<(String, [&str; 3], [String; 3])> = Vec::new();
        let mut amp_pins: BTreeSet<(&str, &str)> = BTreeSet::new();
        if setup.ideal_op_amps {
            for part in &netlist.parts {
                for section in &part.sections {
                    let Some(pins) = &section.op_amp else {
                        continue;
                    };
                    let label = format!("{}.{}", part.designator, section.name);
                    let roles = [&pins.plus, &pins.minus, &pins.out];
                    let nets: Option<Vec<&str>> = roles
                        .iter()
                        .map(|pin| {
                            netlist
                                .net_of(&part.designator, pin)
                                .map(|net| net.name.as_str())
                        })
                        .collect();
                    let Some(nets) = nets else {
                        excluded.push(format!(
                            "{label}: an op-amp pin is on no net, so the section stays open"
                        ));
                        continue;
                    };
                    for pin in roles {
                        amp_pins.insert((part.designator.as_str(), pin.as_str()));
                    }
                    amp_specs.push((
                        label,
                        [nets[0], nets[1], nets[2]],
                        roles.map(|pin| format!("{}.{pin}", part.designator)),
                    ));
                }
            }
        }

        // Every pin of a part the solver does not model. Collected before the
        // branches, because a part is open as a whole: an `LM324` contributes
        // fourteen open pins and no branch.
        let mut opened: BTreeSet<String> = BTreeSet::new();
        let mut endpoints: Vec<(Kind, &str, [&str; 2], f64)> = Vec::new();
        for (designator, nets, siemens) in switch_branches {
            endpoints.push((Kind::R, designator, nets, siemens));
        }
        for part in &netlist.parts {
            let quantity = part.value.quantity();
            let passive = matches!(part.kind, Kind::R | Kind::C | Kind::L)
                && part.pins.len() == 2
                && quantity.is_some_and(|q| q > 0.0);
            if !passive {
                for pin in &part.pins {
                    // A pin a closed switch is carrying is connected, not
                    // open, and saying otherwise in the report would send a
                    // reader looking for a fault that is not there.
                    if closed_pins.contains(&(part.designator.as_str(), pin.as_str())) {
                        continue;
                    }
                    // Likewise an ideal op-amp's pins.
                    if amp_pins.contains(&(part.designator.as_str(), pin.as_str())) {
                        continue;
                    }
                    opened.insert(format!("{}.{pin}", part.designator));
                }
                if matches!(part.kind, Kind::R | Kind::C | Kind::L) {
                    excluded.push(format!(
                        "{}: a {} the solver cannot reduce to one value across two pins, \
                         so it is an open circuit here",
                        part.designator, part.kind
                    ));
                }
                continue;
            }
            if !part.also.is_empty() {
                excluded.push(format!(
                    "{}: one symbol standing for {} parts, counted once. The run is drawn \
                     between one pair of nets and the solver has no way to place the rest",
                    part.designator,
                    1 + part.also.len()
                ));
            }
            let value = match (part.kind, &part.value) {
                // A resistance enters as its reciprocal, so a branch is always
                // "value between two nodes" and the matrix build is one loop.
                (Kind::R, Value::Ohms(ohms)) => 1.0 / ohms,
                (Kind::C, Value::Farads(farads)) => *farads,
                (Kind::L, Value::Henries(_)) => {
                    // An inductor is a short at DC and a second state variable
                    // in the modes, and this board has none. Saying so beats
                    // pretending either half is implemented.
                    excluded.push(format!(
                        "{}: inductors are not implemented, so it is an open circuit here",
                        part.designator
                    ));
                    for pin in &part.pins {
                        opened.insert(format!("{}.{pin}", part.designator));
                    }
                    continue;
                }
                _ => continue,
            };
            let mut nets = [""; 2];
            let mut wired = true;
            for (slot, pin) in part.pins.iter().enumerate() {
                match netlist.net_of(&part.designator, pin) {
                    Some(net) => nets[slot] = net.name.as_str(),
                    None => {
                        wired = false;
                        let why = part
                            .unread
                            .iter()
                            .find(|u| &u.pin == pin)
                            .map(|_| "not read yet")
                            .unwrap_or("drawn and open");
                        excluded.push(format!(
                            "{}: {}.{pin} is {why}, so no current can flow in the part",
                            part.designator, part.designator
                        ));
                    }
                }
            }
            if wired {
                endpoints.push((part.kind, part.designator.as_str(), nets, value));
            }
        }

        // Which nodes a held net can reach through a *resistor*. A node that
        // cannot is not solvable and is not silently grounded: its DC voltage
        // is undetermined and the charge on any capacitor reaching it is
        // frozen, both of which are facts about the drawing rather than
        // shortcomings of the arithmetic.
        let mut reach = Reach::new();
        for held_net in held_of.keys() {
            reach.tie_to_rails(held_net);
        }
        for (kind, _, nets, _) in &endpoints {
            if matches!(kind, Kind::R) {
                reach.join(nets[0], nets[1]);
            }
        }
        // An ideal op-amp's output is a source: it can supply the current its
        // node needs, the way a rail can.
        for (_, [_, _, out], _) in &amp_specs {
            reach.tie_to_rails(out);
        }

        let mut free: Vec<String> = Vec::new();
        let mut free_of: BTreeMap<&str, usize> = BTreeMap::new();
        for net in &netlist.nets {
            let name = net.name.as_str();
            if held_of.contains_key(name) || !reach.on_rails(name) {
                continue;
            }
            free_of.insert(name, free.len());
            free.push(net.name.clone());
        }

        let term = |name: &str| -> Option<Term> {
            held_of
                .get(name)
                .map(|i| Term::Held(*i))
                .or_else(|| free_of.get(name).map(|i| Term::Free(*i)))
        };

        let mut amps = Vec::new();
        let mut op_amps = Vec::new();
        for (label, [plus, minus, out], pins) in amp_specs {
            let dropped = match (term(plus), term(minus), term(out)) {
                (_, _, Some(Term::Held(_))) => Some(format!(
                    "{label}: its output `{out}` is driven, and the drive stands in for the \
                     op-amp, so its pins are open"
                )),
                (Some(Term::Held(_)), Some(Term::Held(_)), _) => Some(format!(
                    "{label}: both inputs are held, which leaves the op-amp nothing to set, \
                     so its pins are open"
                )),
                (Some(p), Some(m), Some(Term::Free(o))) => {
                    amps.push(Amp {
                        plus: p,
                        minus: m,
                        out: o,
                    });
                    op_amps.push(format!("{label}: + {plus}, - {minus}, out {out}"));
                    None
                }
                _ => Some(format!(
                    "{label}: an input reaches no held net through any resistor, so it \
                     cannot be solved as an op-amp and its pins are open"
                )),
            };
            if let Some(why) = dropped {
                excluded.push(why);
                opened.extend(pins);
            }
        }

        let mut resistive = Vec::new();
        let mut capacitive = Vec::new();
        for (kind, designator, nets, value) in endpoints {
            let (Some(a), Some(b)) = (term(nets[0]), term(nets[1])) else {
                let stranded = if term(nets[0]).is_none() {
                    nets[0]
                } else {
                    nets[1]
                };
                excluded.push(format!(
                    "{designator}: `{stranded}` reaches no held net through any resistor, \
                     so there is no current path to solve for"
                ));
                continue;
            };
            if a == b {
                excluded.push(format!(
                    "{designator}: both ends on `{}`, so it carries nothing",
                    nets[0]
                ));
                continue;
            }
            let branch = Branch {
                part: designator.to_string(),
                a,
                b,
                value,
            };
            match kind {
                Kind::R => resistive.push(branch),
                Kind::C => {
                    if matches!((a, b), (Term::Held(_), Term::Held(_))) {
                        excluded.push(format!(
                            "{designator}: both ends held, so it has no voltage to vary and \
                             contributes no mode"
                        ));
                        continue;
                    }
                    capacitive.push(branch);
                }
                _ => {}
            }
        }

        // A rail whose name gives no voltage is only a problem if a passive
        // part is actually wired to it. Plenty of boards name a rail the
        // solver never has to reach.
        let referenced: BTreeSet<&str> = netlist
            .nets
            .iter()
            .filter(|net| {
                unvalued_rails.contains(net.name.as_str())
                    && net.on.iter().any(|e| {
                        netlist
                            .part(&e.part)
                            .is_some_and(|p| matches!(p.kind, Kind::R | Kind::C | Kind::L))
                    })
            })
            .map(|net| net.name.as_str())
            .collect();
        for name in referenced {
            errors.push(format!(
                "rail `{name}` carries passive parts and its name does not give a voltage. \
                 Pass --rail '{name}=<volts>'"
            ));
        }

        if free.is_empty() && errors.is_empty() {
            errors.push(
                "no unknown nodes: every net is either held or unreachable from a rail \
                 through a resistor"
                    .to_string(),
            );
        }
        if !errors.is_empty() {
            return Err(errors);
        }

        excluded.sort();
        excluded.dedup();
        Ok(Network {
            held,
            switches,
            free,
            opened: opened.into_iter().collect(),
            excluded,
            op_amps,
            resistive,
            capacitive,
            amps,
        })
    }

    /// The nodal equations at angular frequency `omega`, with the held nets at
    /// the voltages given, over the free nodes and then one output current per
    /// ideal op-amp.
    ///
    /// A resistor is its conductance and a capacitor `j omega C`, so at zero a
    /// capacitor is open and this is the operating point. Each op-amp adds its
    /// output current to the output node's equation and one row saying its two
    /// inputs are equal.
    fn assemble(&self, omega: f64, held: &[f64]) -> (Vec<Vec<Cx>>, Vec<Cx>) {
        let n = self.free.len();
        let size = n + self.amps.len();
        let mut a = vec![vec![Cx::default(); size]; size];
        let mut rhs = vec![Cx::default(); size];
        let admittances = self
            .resistive
            .iter()
            .map(|b| (b, Cx::new(b.value, 0.0)))
            .chain(
                self.capacitive
                    .iter()
                    .map(|b| (b, Cx::new(0.0, omega * b.value))),
            );
        for (branch, y) in admittances {
            match (branch.a, branch.b) {
                (Term::Free(p), Term::Free(m)) => {
                    a[p][p] += y;
                    a[m][m] += y;
                    a[p][m] -= y;
                    a[m][p] -= y;
                }
                (Term::Free(p), Term::Held(k)) | (Term::Held(k), Term::Free(p)) => {
                    a[p][p] += y;
                    rhs[p] += y * Cx::new(held[k], 0.0);
                }
                (Term::Held(_), Term::Held(_)) => {}
            }
        }
        for (k, amp) in self.amps.iter().enumerate() {
            let row = n + k;
            a[amp.out][row] -= Cx::new(1.0, 0.0);
            for (term, sign) in [(amp.plus, 1.0), (amp.minus, -1.0)] {
                match term {
                    Term::Free(p) => a[row][p] += Cx::new(sign, 0.0),
                    Term::Held(h) => rhs[row] -= Cx::new(sign * held[h], 0.0),
                }
            }
        }
        (a, rhs)
    }

    /// Every free node's small-signal response per volt of `source`, which
    /// must be a held net, at each frequency. Every other held net is at AC
    /// ground, which is what a rail or a stated drive is to a small signal.
    ///
    /// This is the question every leg on these boards is: how much of a drive
    /// reaches a node at a frequency, through the coupling capacitors and,
    /// with `ideal_op_amps`, across the stages with feedback.
    pub fn ac(&self, source: &str, hz: &[f64]) -> Result<Vec<AcPoint>, String> {
        let Some(k) = self.held.iter().position(|h| h.net == source) else {
            return Err(format!(
                "`{source}` is not held, and an AC source is a rail or a drive: \
                 pass --drive '{source}=<volts>'"
            ));
        };
        let mut unit = vec![0.0; self.held.len()];
        unit[k] = 1.0;
        hz.iter()
            .map(|&f| {
                let (mut a, mut b) = self.assemble(std::f64::consts::TAU * f, &unit);
                solve_complex(&mut a, &mut b).map_err(|e| format!("at {f} Hz: {e}"))?;
                Ok(AcPoint {
                    hz: f,
                    nodes: self.free.iter().cloned().zip(b).collect(),
                })
            })
            .collect()
    }

    /// The conductance matrix over the unknown nodes, and the current each one
    /// receives from the held nets. Capacitors are open, which is what makes
    /// this the operating point rather than a moment in a waveform.
    fn conductances(&self) -> (Vec<Vec<f64>>, Vec<f64>) {
        let n = self.free.len();
        let mut g = vec![vec![0.0; n]; n];
        let mut i = vec![0.0; n];
        for branch in &self.resistive {
            match (branch.a, branch.b) {
                (Term::Free(p), Term::Free(m)) => {
                    g[p][p] += branch.value;
                    g[m][m] += branch.value;
                    g[p][m] -= branch.value;
                    g[m][p] -= branch.value;
                }
                (Term::Free(p), Term::Held(k)) | (Term::Held(k), Term::Free(p)) => {
                    g[p][p] += branch.value;
                    i[p] += branch.value * self.held[k].volts;
                }
                (Term::Held(_), Term::Held(_)) => {}
            }
        }
        (g, i)
    }

    /// The operating point: every unknown node's DC voltage.
    pub fn dc(&self) -> Result<Vec<(String, f64)>, String> {
        if !self.amps.is_empty() {
            let volts: Vec<f64> = self.held.iter().map(|h| h.volts).collect();
            let (mut a, mut b) = self.assemble(0.0, &volts);
            solve_complex(&mut a, &mut b)?;
            return Ok(self
                .free
                .iter()
                .cloned()
                .zip(b.iter().map(|v| v.re))
                .collect());
        }
        let (mut g, i) = self.conductances();
        let mut rhs = vec![i];
        solve_columns(&mut g, &mut rhs)?;
        Ok(self
            .free
            .iter()
            .cloned()
            .zip(rhs[0].iter().copied())
            .collect())
    }

    /// The natural time constants of the passive network, one per capacitor,
    /// with the node voltages that move together in each.
    ///
    /// The formulation is the one that gives exactly as many modes as there
    /// are capacitors, rather than as many as there are nodes. Replace each
    /// capacitor by a current source and solve the resistive network for the
    /// voltage that appears across every capacitor per unit current in every
    /// other: that k by k matrix of transresistances, times the capacitances,
    /// has the time constants as its eigenvalues. It is symmetric under the
    /// obvious scaling, so a Jacobi sweep is enough and nothing can fail to
    /// converge.
    ///
    /// The eigenvector is the part an independent-RC reading throws away. It
    /// says how much of each mode appears at each node, which is the
    /// difference between "these two capacitors each have a time constant" and
    /// "this network has two modes that both nodes take part in".
    pub fn modes(&self) -> Result<Vec<Mode>, String> {
        if !self.amps.is_empty() {
            return Err(
                "modes are a property of the passive network, and this one has ideal op-amps \
                 in it; solve without them for the modes"
                    .to_string(),
            );
        }
        let k = self.capacitive.len();
        if k == 0 {
            return Ok(Vec::new());
        }
        let n = self.free.len();
        let (mut g, _) = self.conductances();

        // Each capacitor's port, as a column of injected current.
        let mut ports = vec![vec![0.0; k]; n];
        for (j, branch) in self.capacitive.iter().enumerate() {
            if let Term::Free(p) = branch.a {
                ports[p][j] += 1.0;
            }
            if let Term::Free(m) = branch.b {
                ports[m][j] -= 1.0;
            }
        }
        // `solve_columns` wants a row per right-hand side, so transpose in and
        // back out. k is the number of capacitors on one voice; this is not a
        // place that wants a matrix library.
        let mut rhs: Vec<Vec<f64>> = (0..k)
            .map(|j| (0..n).map(|p| ports[p][j]).collect())
            .collect();
        solve_columns(&mut g, &mut rhs)?;

        // R[i][j]: volts across capacitor i per amp into capacitor j.
        let mut r = vec![vec![0.0; k]; k];
        for (i, row) in r.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                let mut v = 0.0;
                if let Term::Free(p) = self.capacitive[i].a {
                    v += rhs[j][p];
                }
                if let Term::Free(m) = self.capacitive[i].b {
                    v -= rhs[j][m];
                }
                *cell = v;
            }
        }

        // Symmetrize by the square roots of the capacitances: the eigenvalues
        // of `R * C` are those of `sqrt(C) * R * sqrt(C)`, and the second is
        // symmetric positive definite, so they are real and positive.
        let root: Vec<f64> = self.capacitive.iter().map(|b| b.value.sqrt()).collect();
        let mut s = vec![vec![0.0; k]; k];
        for (i, row) in s.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = root[i] * r[i][j] * root[j];
            }
        }
        let (taus, vectors) = jacobi(s);

        let mut modes: Vec<Mode> = Vec::new();
        for (m, tau) in taus.iter().copied().enumerate() {
            // Capacitor voltages in this mode, then the node voltages they
            // imply: the same solve, driven by this mode's currents.
            let current: Vec<f64> = (0..k).map(|j| root[j] * vectors[j][m]).collect();
            let mut shape: Vec<(String, f64)> = self
                .free
                .iter()
                .enumerate()
                .map(|(p, name)| {
                    let v: f64 = (0..k).map(|j| rhs[j][p] * current[j]).sum();
                    (name.clone(), v)
                })
                .collect();
            let peak = shape
                .iter()
                .map(|(_, v)| v.abs())
                .fold(0.0_f64, f64::max)
                .max(f64::MIN_POSITIVE);
            // Sign is arbitrary in an eigenvector, so fix it: the largest
            // component is positive, which makes two runs comparable.
            let sign = shape
                .iter()
                .max_by(|a, b| a.1.abs().total_cmp(&b.1.abs()))
                .map_or(1.0, |(_, v)| if *v < 0.0 { -1.0 } else { 1.0 });
            for entry in &mut shape {
                entry.1 = entry.1 * sign / peak;
            }
            shape.sort_by(|a, b| b.1.abs().total_cmp(&a.1.abs()));
            // The symmetrized eigenvector is `sqrt(C)` times the capacitor
            // voltages, so its squared components are each capacitor's
            // `C v^2`, and Jacobi keeps the vectors at unit length: the
            // squares are already the energy fractions.
            let mut energy: Vec<(String, f64)> = self
                .capacitive
                .iter()
                .enumerate()
                .map(|(j, branch)| (branch.part.clone(), vectors[j][m] * vectors[j][m]))
                .collect();
            energy.sort_by(|a, b| b.1.total_cmp(&a.1));
            modes.push(Mode { tau, shape, energy });
        }
        modes.sort_by(|a, b| b.tau.total_cmp(&a.tau));
        Ok(modes)
    }

    /// Which capacitors are in the analyzed network, for a report.
    pub fn capacitors(&self) -> Vec<String> {
        self.capacitive.iter().map(|b| b.part.clone()).collect()
    }

    /// Which resistors are in the analyzed network, for a report.
    pub fn resistors(&self) -> Vec<String> {
        self.resistive.iter().map(|b| b.part.clone()).collect()
    }
}

/// `+12V` is twelve volts and `GND` is none. A rail's name is the only place a
/// transcription states its voltage, and every rail on this board is named
/// this way; anything else has to be given with `--rail`.
fn rail_volts(name: &str) -> Option<f64> {
    let name = name.trim();
    if name.eq_ignore_ascii_case("gnd") || name.eq_ignore_ascii_case("ground") {
        return Some(0.0);
    }
    let digits = name.strip_suffix(['V', 'v'])?;
    let digits = digits.strip_prefix('+').unwrap_or(digits);
    digits.parse::<f64>().ok()
}

/// Which nets a rail can reach through resistors, as a union-find with every
/// held net tied together at the root.
struct Reach {
    parent: BTreeMap<String, String>,
}

/// The name of the single set every held net joins. A net cannot be called
/// this, because a rail is a net and no drawing labels one with a sentence.
const RAILS: &str = "\u{0}rails";

impl Reach {
    fn new() -> Reach {
        Reach {
            parent: BTreeMap::new(),
        }
    }

    fn find(&mut self, net: &str) -> String {
        let mut cursor = net.to_string();
        while let Some(up) = self.parent.get(&cursor) {
            if up == &cursor {
                break;
            }
            cursor = up.clone();
        }
        cursor
    }

    fn join(&mut self, a: &str, b: &str) {
        let (ra, rb) = (self.find(a), self.find(b));
        if ra == rb {
            return;
        }
        // The rails' set is always the root, so `on_rails` is one lookup.
        let (from, to) = if rb == RAILS { (ra, rb) } else { (rb, ra) };
        self.parent.insert(from, to);
    }

    fn tie_to_rails(&mut self, net: &str) {
        self.parent.entry(RAILS.to_string()).or_insert(RAILS.into());
        self.join(net, RAILS);
    }

    fn on_rails(&mut self, net: &str) -> bool {
        self.find(net) == RAILS
    }
}

// ---------------------------------------------------------------------------
// The arithmetic. Dense, small, and deliberately dependency free: the design
// says this rung is linear algebra on a handful of nodes and needs no external
// crate, and a voice's network is a dozen nodes at most.
// ---------------------------------------------------------------------------

/// Solve `a x = b` in place for several right-hand sides, by Gaussian
/// elimination with partial pivoting. `b` is one row per right-hand side.
fn solve_columns(a: &mut [Vec<f64>], b: &mut [Vec<f64>]) -> Result<(), String> {
    let n = a.len();
    for column in 0..n {
        let pivot = (column..n)
            .max_by(|&i, &j| a[i][column].abs().total_cmp(&a[j][column].abs()))
            .expect("the range is not empty");
        if a[pivot][column].abs() < 1e-30 {
            return Err(format!(
                "the conductance matrix is singular at node {column}, which means some node \
                 has no resistive path to a rail. That is a reading to check rather than \
                 an arithmetic failure"
            ));
        }
        a.swap(pivot, column);
        for rhs in b.iter_mut() {
            rhs.swap(pivot, column);
        }
        // The pivot row is read by every row below it and cannot be borrowed
        // alongside them, so take a copy. One row per column, on a matrix the
        // size of a voice.
        let pivot_row = a[column].clone();
        for row in column + 1..n {
            let factor = a[row][column] / pivot_row[column];
            if factor == 0.0 {
                continue;
            }
            for (cell, above) in a[row].iter_mut().zip(&pivot_row).skip(column) {
                *cell -= factor * above;
            }
            for rhs in b.iter_mut() {
                rhs[row] -= factor * rhs[column];
            }
        }
    }
    for rhs in b.iter_mut() {
        for row in (0..n).rev() {
            let mut sum = rhs[row];
            for k in row + 1..n {
                sum -= a[row][k] * rhs[k];
            }
            rhs[row] = sum / a[row][row];
        }
    }
    Ok(())
}

/// Solve `a x = b` in place over the complex numbers, by Gaussian elimination
/// with partial pivoting on magnitude. The small-signal counterpart of
/// `solve_columns`, for one right-hand side.
fn solve_complex(a: &mut [Vec<Cx>], b: &mut [Cx]) -> Result<(), String> {
    let n = a.len();
    for column in 0..n {
        let pivot = (column..n)
            .max_by(|&i, &j| a[i][column].abs().total_cmp(&a[j][column].abs()))
            .expect("the range is not empty");
        if a[pivot][column].abs() < 1e-30 {
            return Err(format!(
                "the system is singular at unknown {column}: some node has no path to a \
                 held net, or an op-amp has nothing its output can move"
            ));
        }
        a.swap(pivot, column);
        b.swap(pivot, column);
        let pivot_row = a[column].clone();
        for row in column + 1..n {
            let factor = a[row][column] / pivot_row[column];
            if factor == Cx::default() {
                continue;
            }
            for (cell, above) in a[row].iter_mut().zip(&pivot_row).skip(column) {
                *cell -= factor * *above;
            }
            let lead = b[column];
            b[row] -= factor * lead;
        }
    }
    for row in (0..n).rev() {
        let mut sum = b[row];
        for k in row + 1..n {
            sum -= a[row][k] * b[k];
        }
        b[row] = sum / a[row][row];
    }
    Ok(())
}

/// Eigenvalues and eigenvectors of a symmetric matrix, by cyclic Jacobi.
///
/// Returns the eigenvalues and a matrix whose `[i][m]` is component `i` of
/// eigenvector `m`. Jacobi is chosen over anything faster for the reason the
/// design gives for this whole rung: on a symmetric matrix it cannot fail to
/// converge, so a result is never a question about the solver.
fn jacobi(mut a: Vec<Vec<f64>>) -> (Vec<f64>, Vec<Vec<f64>>) {
    let n = a.len();
    let mut v = vec![vec![0.0; n]; n];
    for (i, row) in v.iter_mut().enumerate() {
        row[i] = 1.0;
    }
    for _ in 0..100 {
        let off = (0..n)
            .flat_map(|i| (i + 1..n).map(move |j| (i, j)))
            .map(|(i, j)| a[i][j] * a[i][j])
            .sum::<f64>();
        let scale = (0..n).map(|i| a[i][i] * a[i][i]).sum::<f64>();
        if off <= scale * 1e-30 {
            break;
        }
        for p in 0..n {
            for q in p + 1..n {
                if a[p][q].abs() < 1e-300 {
                    continue;
                }
                let theta = (a[q][q] - a[p][p]) / (2.0 * a[p][q]);
                let t = theta.signum() / (theta.abs() + (theta * theta + 1.0).sqrt());
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                // Rotate columns p and q, then rows p and q. The two rows have
                // to be borrowed apart, which `p < q` makes a clean split.
                for row in a.iter_mut() {
                    let (akp, akq) = (row[p], row[q]);
                    row[p] = c * akp - s * akq;
                    row[q] = s * akp + c * akq;
                }
                let (upper, lower) = a.split_at_mut(q);
                for (apk, aqk) in upper[p].iter_mut().zip(lower[0].iter_mut()) {
                    let (x, y) = (*apk, *aqk);
                    *apk = c * x - s * y;
                    *aqk = s * x + c * y;
                }
                for row in v.iter_mut() {
                    let (vp, vq) = (row[p], row[q]);
                    row[p] = c * vp - s * vq;
                    row[q] = s * vp + c * vq;
                }
            }
        }
    }
    ((0..n).map(|i| a[i][i]).collect(), v)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A divider and a capacitor: the textbook case, where the one-RC reading
    /// and the network agree exactly because there is only one capacitor.
    const DIVIDER: &str = r#"
[board]
name = "t"

[[parts]]
ref = "R1"
kind = "R"
kohms = 10

[[parts]]
ref = "R2"
kind = "R"
kohms = 10

[[parts]]
ref = "C1"
kind = "C"
uf = 1.0

[[nets]]
name = "+12V"
rail = true
on = ["R1.a"]

[[nets]]
name = "mid"
on = ["R1.b", "R2.a", "C1.a"]

[[nets]]
name = "GND"
rail = true
on = ["R2.b", "C1.b"]
"#;

    fn network(text: &str, setup: &Setup) -> Network {
        let netlist = Netlist::parse(text).expect("should load");
        Network::build(&netlist, setup).expect("should extract")
    }

    #[test]
    fn a_divider_solves_to_half_its_rail() {
        let net = network(DIVIDER, &Setup::default());
        let dc = net.dc().expect("should solve");
        let mid = dc.iter().find(|(n, _)| n == "mid").unwrap().1;
        assert!((mid - 6.0).abs() < 1e-9, "{mid}");
    }

    #[test]
    fn one_capacitor_gives_one_mode_at_its_thevenin_resistance() {
        let net = network(DIVIDER, &Setup::default());
        let modes = net.modes().expect("should solve");
        assert_eq!(modes.len(), 1);
        // 10k in parallel with 10k, times 1 uF.
        assert!((modes[0].tau - 5e-3).abs() < 1e-12, "{}", modes[0].tau);
        assert!((modes[0].shape[0].1 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn a_rails_voltage_comes_from_its_name_and_an_override_wins() {
        let net = network(DIVIDER, &Setup::default());
        let twelve = net.held.iter().find(|h| h.net == "+12V").unwrap();
        assert!((twelve.volts - 12.0).abs() < 1e-12);

        let setup = Setup {
            rails: BTreeMap::from([("+12V".to_string(), 11.5)]),
            ..Setup::default()
        };
        let net = network(DIVIDER, &setup);
        let dc = net.dc().expect("should solve");
        let mid = dc.iter().find(|(n, _)| n == "mid").unwrap().1;
        assert!((mid - 5.75).abs() < 1e-9, "{mid}");
    }

    /// Two RC sections that barely interact: the fast one's source impedance
    /// is a thousandth of the slow one's, so the poles are the products
    /// anybody would write down. This is the case the shot's prose asserts it
    /// is in, stated as a test so that the shot's answer is read against a
    /// known one.
    const TWO_STAGE: &str = r#"
[board]
name = "t"

[[parts]]
ref = "R1"
kind = "R"
ohms = 1000

[[parts]]
ref = "C1"
kind = "C"
uf = 1.0

[[parts]]
ref = "R2"
kind = "R"
mohms = 1.0

[[parts]]
ref = "C2"
kind = "C"
uf = 1.0

[[nets]]
name = "+5V"
rail = true
on = ["R1.a"]

[[nets]]
name = "first"
on = ["R1.b", "C1.a", "R2.a"]

[[nets]]
name = "second"
on = ["R2.b", "C2.a"]

[[nets]]
name = "GND"
rail = true
on = ["C1.b", "C2.b"]
"#;

    #[test]
    fn well_separated_sections_give_the_poles_each_section_would_alone() {
        let net = network(TWO_STAGE, &Setup::default());
        let modes = net.modes().expect("should solve");
        assert_eq!(modes.len(), 2);
        // 1 M times 1 uF, and 1 k times 1 uF, to a thousandth.
        assert!((modes[0].tau - 1.0).abs() < 2e-3, "{}", modes[0].tau);
        assert!((modes[1].tau - 1e-3).abs() < 2e-6, "{}", modes[1].tau);
    }

    /// The property the shot's question is about. A mode is not "a capacitor's
    /// time constant": it is a motion of the whole network, and the shape says
    /// how much of it appears at each node. Here the slow mode is confined to
    /// the second node because the first is held stiff by a 1 k source.
    #[test]
    fn a_modes_shape_says_which_nodes_take_part_in_it() {
        let net = network(TWO_STAGE, &Setup::default());
        let modes = net.modes().expect("should solve");
        let slow = &modes[0];
        assert_eq!(slow.shape[0].0, "second");
        let first = slow.shape.iter().find(|(n, _)| n == "first").unwrap().1;
        assert!(
            first.abs() < 0.01,
            "the slow mode barely moves `first`: {first}"
        );
    }

    /// Two equal sections with no separation at all: the modes are emphatically
    /// not one per capacitor, and an independent-RC reading of this network
    /// would be wrong by a factor rather than by a percent. This is the
    /// negative control for the shot's assertion.
    const UNSEPARATED: &str = r#"
[board]
name = "t"

[[parts]]
ref = "R1"
kind = "R"
kohms = 1

[[parts]]
ref = "C1"
kind = "C"
uf = 1.0

[[parts]]
ref = "R2"
kind = "R"
kohms = 1

[[parts]]
ref = "C2"
kind = "C"
uf = 1.0

[[nets]]
name = "+5V"
rail = true
on = ["R1.a"]

[[nets]]
name = "first"
on = ["R1.b", "C1.a", "R2.a"]

[[nets]]
name = "second"
on = ["R2.b", "C2.a"]

[[nets]]
name = "GND"
rail = true
on = ["C1.b", "C2.b"]
"#;

    #[test]
    fn sections_that_load_each_other_do_not_give_one_pole_each() {
        let net = network(UNSEPARATED, &Setup::default());
        let modes = net.modes().expect("should solve");
        // Each section alone reads 1 ms. The network gives 2.618 ms and
        // 0.382 ms, the golden ratio squared either side.
        assert!((modes[0].tau - 2.618e-3).abs() < 1e-6, "{}", modes[0].tau);
        assert!((modes[1].tau - 0.382e-3).abs() < 1e-6, "{}", modes[1].tau);
        // And both nodes take part in both modes.
        let second = modes[0]
            .shape
            .iter()
            .find(|(n, _)| n == "second")
            .unwrap()
            .1;
        assert!(second.abs() > 0.5, "{second}");
    }

    /// Every pin of a part the solver does not model is reported as opened,
    /// because "the answer assumed this pin was open" is the first thing a
    /// reader has to be able to check.
    #[test]
    fn a_part_the_solver_does_not_model_has_every_pin_reported_open() {
        let text = r#"
[board]
name = "t"

[[parts]]
ref = "U1"
kind = "U"
device = "LM324"
pins = ["1", "2", "3"]
out = ["1"]

[[parts]]
ref = "R1"
kind = "R"
kohms = 10

[[parts]]
ref = "R2"
kind = "R"
kohms = 10

[[nets]]
name = "+12V"
rail = true
on = ["R1.a"]

[[nets]]
name = "mid"
on = ["R1.b", "R2.a", "U1.3"]

[[nets]]
name = "GND"
rail = true
on = ["R2.b"]

[[nets]]
name = "out"
port = "output"
on = ["U1.1", "U1.2"]
"#;
        let net = network(text, &Setup::default());
        assert!(net.opened.contains(&"U1.3".to_string()), "{:?}", net.opened);
        let dc = net.dc().expect("should solve");
        let mid = dc.iter().find(|(n, _)| n == "mid").unwrap().1;
        assert!(
            (mid - 6.0).abs() < 1e-9,
            "an open input draws nothing: {mid}"
        );
    }

    /// A node that reaches no rail through a resistor is named rather than
    /// grounded, and the capacitor on it is dropped out loud. `C94` on the
    /// MB4391's rolloff pin is this exact shape, and the honest answer there
    /// is that the solver has nothing to say.
    #[test]
    fn a_node_with_no_resistive_path_is_excluded_and_said_so() {
        let text = r#"
[board]
name = "t"

[[parts]]
ref = "U1"
kind = "U"
device = "MB4391"
pins = ["14"]

[[parts]]
ref = "C94"
kind = "C"
pf = 680

[[parts]]
ref = "R1"
kind = "R"
kohms = 10

[[parts]]
ref = "R2"
kind = "R"
kohms = 10

[[nets]]
name = "+12V"
rail = true
on = ["R1.a"]

[[nets]]
name = "mid"
on = ["R1.b", "R2.a"]

[[nets]]
name = "GND"
rail = true
on = ["R2.b", "C94.b"]

[[nets]]
name = "U1 RO"
on = ["U1.14", "C94.a"]
"#;
        let net = network(text, &Setup::default());
        assert!(!net.free.contains(&"U1 RO".to_string()), "{:?}", net.free);
        assert!(
            net.excluded.iter().any(|e| e.contains("C94")),
            "{:?}",
            net.excluded
        );
        assert!(net.modes().expect("should solve").is_empty());
    }

    /// A drive is how a scenario says what the parts the solver does not model
    /// are holding. It makes its net a held one, which is what lets a voice's
    /// operating point be asked for at a stated moment.
    #[test]
    fn a_drive_holds_a_net_and_the_rest_solves_around_it() {
        let text = r#"
[board]
name = "t"

[[parts]]
ref = "U1"
kind = "U"
device = "555"
pins = ["3"]
out = ["3"]

[[parts]]
ref = "R153"
kind = "R"
kohms = 2.7

[[parts]]
ref = "R155"
kind = "R"
ohms = 820

[[nets]]
name = "U1 out"
on = ["U1.3", "R153.a"]

[[nets]]
name = "node A"
on = ["R153.b", "R155.a"]

[[nets]]
name = "GND"
rail = true
on = ["R155.b"]
"#;
        let setup = Setup {
            drives: BTreeMap::from([("U1 out".to_string(), 10.3)]),
            ..Setup::default()
        };
        let net = network(text, &setup);
        let dc = net.dc().expect("should solve");
        let a = dc.iter().find(|(n, _)| n == "node A").unwrap().1;
        // 820 / (2700 + 820) of 10.3 V.
        assert!((a - 10.3 * 820.0 / 3520.0).abs() < 1e-9, "{a}");
    }

    /// Two legs switched onto one node by two sections of a `4066`, which is
    /// Lunar Lander's throttle in miniature. The point is that the SAME
    /// resistors set the divider and the corner, so closing a second one has
    /// to move both.
    const SWITCHED: &str = r#"
[board]
name = "t"

[[parts]]
ref = "P5"
kind = "U"
device = "4066"
pins = ["3", "4", "5", "6", "8", "9"]

[[parts.switches]]
name = "b"
pins = ["4", "3"]
control = "5"
ohms = 80

[[parts.switches]]
name = "c"
pins = ["8", "9"]
control = "6"
ohms = 80

[[parts]]
ref = "R20"
kind = "R"
kohms = 8.2

[[parts]]
ref = "R18"
kind = "R"
kohms = 15

[[parts]]
ref = "C15"
kind = "C"
uf = 1.0

[[nets]]
name = "source"
port = "input"
on = ["P5.4", "P5.8"]

[[nets]]
name = "R20 leg"
on = ["P5.3", "R20.a"]

[[nets]]
name = "R18 leg"
on = ["P5.9", "R18.a"]

[[nets]]
name = "common node"
on = ["R20.b", "R18.b", "C15.a"]

[[nets]]
name = "+5V"
rail = true
on = ["C15.b"]

[[nets]]
name = "AUD1"
port = "input"
on = ["P5.5"]

[[nets]]
name = "AUD0"
port = "input"
on = ["P5.6"]
"#;

    fn throttle(drives: &[(&str, f64)]) -> Network {
        let setup = Setup {
            drives: drives
                .iter()
                .map(|(net, v)| ((*net).to_string(), *v))
                .collect(),
            ..Setup::default()
        };
        network(SWITCHED, &setup)
    }

    /// The decision point this board was picked for. A switch is not a drive:
    /// holding the node would throw away the resistance the closed leg puts in
    /// the network, which is the whole question.
    /// A closed switch is a branch and an open one is nothing, which is
    /// counted rather than inferred from the node list. Both legs' nodes stay
    /// reachable either way, because `R20` still joins its leg to the common
    /// node whether or not anything drives it; what changes is how many
    /// branches carry current.
    #[test]
    fn a_closed_switch_is_a_branch_in_the_network_and_an_open_one_is_not() {
        let switches = |n: &Network| n.resistors().iter().filter(|r| *r == "P5").count();

        let one = throttle(&[("source", 3.8), ("AUD0", 5.0), ("AUD1", 0.0)]);
        assert_eq!(switches(&one), 1, "{:?}", one.resistors());

        let both = throttle(&[("source", 3.8), ("AUD0", 5.0), ("AUD1", 5.0)]);
        assert_eq!(switches(&both), 2, "{:?}", both.resistors());
    }

    /// With every switch open this fixture's legs reach no rail through any
    /// resistor, so there is nothing to solve and the solver says so rather
    /// than returning an empty answer. The real board does not do this, because
    /// its common node also reaches +5 V through `R22` and `R26`; the fixture
    /// leaves those out, which makes it the clean case for the rule.
    #[test]
    fn a_network_reachable_only_through_open_switches_has_nothing_to_solve() {
        let netlist = Netlist::parse(SWITCHED).expect("should load");
        let setup = Setup {
            drives: BTreeMap::from([
                ("source".to_string(), 3.8),
                ("AUD0".to_string(), 0.0),
                ("AUD1".to_string(), 0.0),
            ]),
            ..Setup::default()
        };
        let errors = Network::build(&netlist, &setup).unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("no unknown nodes")),
            "{errors:?}"
        );
    }

    /// The finding itself: closing a second leg moves the corner **and** the
    /// divider together, because they are the same resistors. A model with a
    /// fixed corner and a linear volume is wrong about one or the other at
    /// every setting but full.
    #[test]
    fn closing_a_second_leg_moves_the_corner_and_the_level_together() {
        let quiet = throttle(&[("source", 3.8), ("AUD0", 5.0), ("AUD1", 0.0)]);
        let loud = throttle(&[("source", 3.8), ("AUD0", 5.0), ("AUD1", 5.0)]);

        let tau = |n: &Network| n.modes().expect("should solve")[0].tau;
        let (slow, fast) = (tau(&quiet), tau(&loud));
        // 15.08k * 1uF against (15.08k || 8.28k) * 1uF.
        assert!((slow - 15.08e-3).abs() < 1e-5, "{slow}");
        assert!((fast - 5.345e-3).abs() < 1e-5, "{fast}");
        assert!(
            slow / fast > 2.8,
            "the corner moves by nearly three to one across one bit: {slow} vs {fast}"
        );
    }

    /// Every switch is reported whichever way it went, because which ones were
    /// closed is part of the answer rather than part of the setup.
    #[test]
    fn switch_states_are_reported_closed_or_open() {
        let net = throttle(&[("source", 3.8), ("AUD0", 5.0), ("AUD1", 0.0)]);
        assert_eq!(net.switches.len(), 2);
        let closed = net.switches.iter().find(|s| s.section == "P5.c").unwrap();
        assert!(closed.closed);
        assert_eq!(closed.ohms, Some(80.0));
        assert!(closed.why.contains("AUD0"), "{}", closed.why);
        let open = net.switches.iter().find(|s| s.section == "P5.b").unwrap();
        assert!(!open.closed);
    }

    /// A control nobody stated is not a closed switch and not silently an open
    /// one either: it is an unknown, defaulted to open and said out loud.
    #[test]
    fn a_switch_whose_control_nothing_states_is_open_and_says_it_is_a_default() {
        let net = throttle(&[("source", 3.8), ("AUD0", 5.0)]);
        let unknown = net.switches.iter().find(|s| s.section == "P5.b").unwrap();
        assert!(!unknown.closed);
        assert!(unknown.why.contains("unknown"), "{}", unknown.why);
    }

    /// A pin a closed switch is carrying is connected, and reporting it open
    /// would send a reader looking for a fault that is not there.
    #[test]
    fn a_closed_switchs_pins_are_not_reported_open() {
        let net = throttle(&[("source", 3.8), ("AUD0", 5.0), ("AUD1", 0.0)]);
        assert!(
            !net.opened.contains(&"P5.8".to_string()),
            "{:?}",
            net.opened
        );
        assert!(
            !net.opened.contains(&"P5.9".to_string()),
            "{:?}",
            net.opened
        );
        // The open section's pins still are.
        assert!(net.opened.contains(&"P5.3".to_string()), "{:?}", net.opened);
    }

    #[test]
    fn a_drive_naming_no_net_is_an_error_rather_than_a_silent_nothing() {
        let netlist = Netlist::parse(DIVIDER).expect("should load");
        let setup = Setup {
            drives: BTreeMap::from([("node Q".to_string(), 1.0)]),
            ..Setup::default()
        };
        let errors = Network::build(&netlist, &setup).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("node Q")), "{errors:?}");
    }

    #[test]
    fn a_rail_whose_name_gives_no_voltage_is_an_error_naming_the_flag_to_fix_it() {
        let text = r#"
[board]
name = "t"

[[parts]]
ref = "R1"
kind = "R"
kohms = 10

[[parts]]
ref = "R2"
kind = "R"
kohms = 10

[[nets]]
name = "VBB"
rail = true
on = ["R1.a"]

[[nets]]
name = "mid"
on = ["R1.b", "R2.a"]

[[nets]]
name = "GND"
rail = true
on = ["R2.b"]
"#;
        let netlist = Netlist::parse(text).expect("should load");
        let errors = Network::build(&netlist, &Setup::default()).unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("--rail 'VBB=")),
            "{errors:?}"
        );
    }

    /// A resistor from a driven net into a capacitor to ground: the one
    /// response everybody knows, at its corner.
    #[test]
    fn an_rc_is_three_decibels_down_and_forty_five_degrees_late_at_its_corner() {
        let text = r#"
[board]
name = "t"

[[parts]]
ref = "R1"
kind = "R"
kohms = 10

[[parts]]
ref = "C1"
kind = "C"
uf = 0.1

[[nets]]
name = "in"
on = ["R1.a"]

[[nets]]
name = "out"
on = ["R1.b", "C1.a"]

[[nets]]
name = "GND"
rail = true
on = ["C1.b"]
"#;
        let setup = Setup {
            drives: BTreeMap::from([("in".to_string(), 0.0)]),
            ..Setup::default()
        };
        let net = network(text, &setup);
        let corner = 1.0 / (std::f64::consts::TAU * 10e3 * 0.1e-6);
        let points = net.ac("in", &[0.0, corner]).expect("should solve");
        let out = |i: usize| points[i].at("out").unwrap();
        assert!((out(0).abs() - 1.0).abs() < 1e-9, "{:?}", out(0));
        assert!(
            (out(1).abs() - std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-9,
            "{:?}",
            out(1)
        );
        assert!(
            (out(1).degrees() + 45.0).abs() < 1e-6,
            "{}",
            out(1).degrees()
        );
    }

    /// An inverting stage, `R2` from the output back to `-` and `C1` across
    /// it, with `+` on a rail. Lunar Lander's summing amp is exactly this
    /// shape, and `C1` is its `C27`: the stage is `-R2/R1` at DC and falls
    /// through `1/(2 pi R2 C1)`.
    const INVERTER: &str = r#"
[board]
name = "t"

[[parts]]
ref = "U1"
kind = "U"
device = "LM324"
pins = ["5", "6", "7"]
out = ["7"]

[[parts.sections]]
name = "b"
pins = ["5", "6", "7"]
op_amp = { plus = "5", minus = "6", out = "7" }

[[parts]]
ref = "R1"
kind = "R"
kohms = 10

[[parts]]
ref = "R2"
kind = "R"
kohms = 20

[[parts]]
ref = "C1"
kind = "C"
uf = 0.1

[[nets]]
name = "in"
on = ["R1.a"]

[[nets]]
name = "sum"
on = ["R1.b", "R2.a", "C1.a", "U1.6"]

[[nets]]
name = "out"
on = ["U1.7", "R2.b", "C1.b"]

[[nets]]
name = "+5V"
rail = true
on = ["U1.5"]
"#;

    fn inverter(ideal: bool, drive_out: bool) -> Network {
        let mut drives = BTreeMap::from([("in".to_string(), 6.0)]);
        if drive_out {
            drives.insert("out".to_string(), 1.0);
        }
        network(
            INVERTER,
            &Setup {
                drives,
                ideal_op_amps: ideal,
                ..Setup::default()
            },
        )
    }

    #[test]
    fn an_ideal_inverting_stage_holds_its_input_at_the_rail_and_gains_minus_two() {
        let net = inverter(true, false);
        assert_eq!(net.op_amps.len(), 1, "{:?}", net.excluded);
        assert!(
            !net.opened.contains(&"U1.6".to_string()),
            "{:?}",
            net.opened
        );
        let dc = net.dc().expect("should solve");
        let at = |n: &str| dc.iter().find(|(name, _)| name == n).unwrap().1;
        // The virtual ground sits on +5 V, and 1 V above it at the input is
        // 2 V below it at the output.
        assert!((at("sum") - 5.0).abs() < 1e-9, "{}", at("sum"));
        assert!((at("out") - 3.0).abs() < 1e-9, "{}", at("out"));
    }

    #[test]
    fn a_capacitor_across_the_feedback_makes_the_stage_a_low_pass() {
        let net = inverter(true, false);
        let corner = 1.0 / (std::f64::consts::TAU * 20e3 * 0.1e-6);
        let points = net.ac("in", &[1.0, corner, 10.0 * corner]).expect("solves");
        let gain = |i: usize| points[i].at("out").unwrap().abs();
        assert!((gain(0) - 2.0).abs() < 1e-3, "{}", gain(0));
        assert!(
            (gain(1) - 2.0 * std::f64::consts::FRAC_1_SQRT_2).abs() < 1e-9,
            "{}",
            gain(1)
        );
        assert!((gain(2) - 2.0 / 101f64.sqrt()).abs() < 1e-9, "{}", gain(2));
    }

    /// Without the option the same section is open pins, as it always was,
    /// and the answer is the passive network's.
    #[test]
    fn op_amps_are_open_unless_the_setup_asks_for_them_ideal() {
        let net = inverter(false, false);
        assert!(net.op_amps.is_empty());
        assert!(net.opened.contains(&"U1.6".to_string()), "{:?}", net.opened);
    }

    /// A driven output means the scenario has said what the op-amp does, so
    /// the drive wins and the section stays open, out loud.
    #[test]
    fn a_driven_output_stands_in_for_the_op_amp() {
        let net = inverter(true, true);
        assert!(net.op_amps.is_empty());
        assert!(
            net.excluded
                .iter()
                .any(|e| e.contains("U1.b") && e.contains("driven")),
            "{:?}",
            net.excluded
        );
        assert!(net.opened.contains(&"U1.6".to_string()), "{:?}", net.opened);
    }

    #[test]
    fn modes_are_refused_with_an_op_amp_in_the_network() {
        let err = inverter(true, false).modes().unwrap_err();
        assert!(err.contains("passive"), "{err}");
    }

    #[test]
    fn an_ac_source_must_be_held() {
        let err = inverter(true, false).ac("sum", &[100.0]).unwrap_err();
        assert!(err.contains("not held"), "{err}");
    }

    #[test]
    fn rail_names_read_as_voltages() {
        assert_eq!(rail_volts("+12V"), Some(12.0));
        assert_eq!(rail_volts("+5V"), Some(5.0));
        assert_eq!(rail_volts("-5V"), Some(-5.0));
        assert_eq!(rail_volts("GND"), Some(0.0));
        assert_eq!(rail_volts("+6V"), Some(6.0));
        assert_eq!(rail_volts("VBB"), None);
    }
}
