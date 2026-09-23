//! A board's transcription: parts, nets, and the pins between them.
//!
//! The model is deliberately small. A part has a designator, a kind, a typed
//! value and the pins its symbol draws. A net has a name and the
//! `designator.pin` endpoints on it. Everything else in this file is either
//! provenance or an annotation that records why a reading looks wrong when it
//! is not.
//!
//! Two properties are what make this different from the netlistsvg JSON it
//! replaces, and both come from `docs/designs/schematic-transcription.md`:
//!
//! - **Values are numbers, not display strings.** The unit lives in the key
//!   (`kohms`, `pf`, `uf`), so the file can be written the way the drawing
//!   writes the value while the loader still hands out farads and ohms.
//!   Nothing ever parses a value back out of a label.
//! - **A pin that goes nowhere is declared.** A pin the symbol draws is on a
//!   net or it is listed in `nc` with a reason. A pin that is not in `pins` at
//!   all is a different statement: the symbol does not draw it. Those two
//!   states are separate facts on this board and the format keeps them apart.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

/// What a part is. The kind decides which value key it carries and what its
/// pins are called when the transcription does not name them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Kind {
    /// Resistor.
    R,
    /// Capacitor.
    C,
    /// Inductor.
    L,
    /// Diode, including LEDs.
    D,
    /// Transistor, bipolar or FET.
    Q,
    /// Integrated circuit.
    U,
    /// Crystal or resonator.
    X,
    /// Potentiometer or trimmer.
    #[serde(rename = "POT")]
    Pot,
    /// Connector, test point, or anything else whose only job is to be a
    /// place where a net leaves the sheet.
    J,
}

impl Kind {
    /// The pins a symbol of this kind draws when the transcription does not
    /// say. Two-terminal parts get `a` and `b` in that order, which is also
    /// the order the generated drawing reads them in, so the transcriber
    /// chooses the flow by choosing which end is `a`.
    fn default_pins(self) -> Option<&'static [&'static str]> {
        match self {
            Kind::R | Kind::C | Kind::L | Kind::X => Some(&["a", "b"]),
            Kind::D => Some(&["a", "k"]),
            Kind::Q => Some(&["b", "c", "e"]),
            Kind::Pot => Some(&["a", "w", "b"]),
            // An IC's pins are the whole point of transcribing it, and a
            // connector's count is a property of the part rather than of its
            // class. Neither gets a default.
            Kind::U | Kind::J => None,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Kind::R => "R",
            Kind::C => "C",
            Kind::L => "L",
            Kind::D => "D",
            Kind::Q => "Q",
            Kind::U => "U",
            Kind::X => "X",
            Kind::Pot => "POT",
            Kind::J => "J",
        };
        f.write_str(s)
    }
}

/// A part's value, normalized to SI base units at load.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Ohms.
    Ohms(f64),
    /// Farads.
    Farads(f64),
    /// Henries.
    Henries(f64),
    /// Hertz.
    Hertz(f64),
    /// A part number rather than a quantity, for the parts whose behavior is
    /// the part rather than a number: ICs, and the transistors and diodes
    /// whose type is legible on the drawing.
    Device(String),
    /// No value: a diode or transistor whose part number the drawing does not
    /// give, or a connector.
    None,
}

impl Value {
    /// The value as a number in SI base units, for a part that carries one.
    pub fn quantity(&self) -> Option<f64> {
        match *self {
            Value::Ohms(v) | Value::Farads(v) | Value::Henries(v) | Value::Hertz(v) => Some(v),
            Value::Device(_) | Value::None => None,
        }
    }

    /// The value as the drawing would print it: `3.3k`, `680p`, `2.2u`.
    /// Presentation only. Nothing reads this back.
    pub fn label(&self) -> String {
        match self {
            Value::Ohms(v) => engineering(*v),
            Value::Farads(v) => format!("{}F", engineering(*v)),
            Value::Henries(v) => format!("{}H", engineering(*v)),
            Value::Hertz(v) => format!("{}Hz", engineering(*v)),
            Value::Device(d) => d.clone(),
            Value::None => String::new(),
        }
    }
}

/// Format a quantity with an engineering prefix, three significant digits at
/// most, trailing zeros trimmed: `3.3k`, `680p`, `47n`, `2.2M`.
fn engineering(v: f64) -> String {
    const PREFIXES: [(f64, &str); 7] = [
        (1e12, "T"),
        (1e9, "G"),
        (1e6, "M"),
        (1e3, "k"),
        (1.0, ""),
        (1e-3, "m"),
        (1e-6, "u"),
    ];
    if v == 0.0 {
        return "0".to_string();
    }
    // Below a micro the table would need `n` and `p`, which the loop's lower
    // bound does not reach; handle the whole decade run in one place instead.
    let (scale, prefix) = PREFIXES
        .iter()
        .copied()
        .find(|&(scale, _)| v.abs() >= scale)
        .unwrap_or_else(|| {
            if v.abs() >= 1e-9 {
                (1e-9, "n")
            } else {
                (1e-12, "p")
            }
        });
    let scaled = v / scale;
    let mut s = format!("{scaled:.3}");
    if s.contains('.') {
        s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    }
    format!("{s}{prefix}")
}

/// A pin the symbol draws that reaches nothing, with the reason it is a fact
/// rather than an omission.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoConnect {
    /// The pin, named as `pins` names it.
    pub pin: String,
    /// Why the drawing leaves it open. Required: an undocumented `nc` is a
    /// transcription that gave up, and this board has two of them that invert
    /// a voice if read the other way.
    pub why: String,
}

/// How completely a part has been read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Read {
    /// Every pin the symbol draws is in `pins`, so a pin that is absent is a
    /// pin the symbol does not draw. That is a claim about the drawing, and
    /// on this board it is load-bearing: `U7`'s missing output pin is what
    /// says the board reads that 555 at its capacitor.
    #[default]
    Full,
    /// The pin list is itself incomplete, so an absent pin says nothing. A
    /// part is partial whether it says so or not once it has an `unread` pin.
    Partial,
}

/// A pin the symbol draws whose net has not been read yet.
///
/// This is the third state, and it is deliberately uncomfortable. Decision 3
/// says a drawn pin is wired or it is `nc`, and that is the rule for a
/// finished transcription. Getting there takes more than one sitting, and the
/// alternative to saying "not read yet" out loud is leaving the part out,
/// which is the failure this whole design exists to stop: `C94` was invisible
/// for six passes precisely because nothing recorded that it had never been
/// looked at.
///
/// So an unread pin is a to-do item and not a suppression. It carries a
/// reason, it makes its part `Partial`, and everything that reports on a
/// transcription counts it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Unread {
    /// The pin, named as `pins` names it.
    pub pin: String,
    /// What it would take to settle it.
    pub why: String,
}

/// One section of a multi-section part: one of an `LM324`'s four amplifiers,
/// one of a `74123`'s two one-shots.
///
/// Presentation, like `block`. The netlist holds one `U19` with the real pin
/// numbers on it, because `U19` is one chip and pin 6 is pin 6. Sections only
/// say how to draw it, and a fourteen-pin rectangle with wires leaving all
/// four sides is not how this board is drawn or how the prose reads it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Section {
    /// The suffix the drawing uses: `a`, `b`, `c`, `d`.
    pub name: String,
    /// The package pins this section owns.
    pub pins: Vec<String>,
    /// What the section does, for the label: `integrator`, `Schmitt`.
    pub role: Option<String>,
    /// Which subcircuit this section belongs to, where the package spans
    /// several.
    ///
    /// Shared packages are the norm on this board rather than the exception:
    /// one `MB4391` runs the medium explosion on one channel and the cannon on
    /// the other, one `74123` is the medium explosion and the shot, one
    /// `4016B` gates four different voices. So the unit a drawing is cut along
    /// is the section and not the part. Falls back to the part's own `group`.
    pub group: Option<String>,
}

/// One part on the sheet.
#[derive(Debug, Clone)]
pub struct Part {
    /// The designator as the drawing prints it: `R143`, `U19`, `C88`.
    pub designator: String,
    /// What the part is.
    pub kind: Kind,
    /// The value, in SI base units, or the part number.
    pub value: Value,
    /// Set when the part number is inferred from the package and the era
    /// rather than printed on the drawing.
    ///
    /// Every op-amp on these two sheets is an unlabeled 14-pin quad, and
    /// `LM324` is the 1982 part for that job. That is a good inference and it
    /// is still an inference: the swing limits the shot's whole pitch range
    /// rests on come from the part class, so a reader has to be able to tell
    /// which parts were read off the drawing and which were reasoned about.
    /// Decision 7 asks for exactly this line, between "we genuinely do not
    /// know" and "we did not check", and a part number that looks read when it
    /// was guessed erases it.
    pub device_inferred: bool,
    /// The pins the symbol draws, in the order the drawing reads them.
    pub pins: Vec<String>,
    /// The pins that drive rather than listen. Presentation for the generated
    /// drawing, and the only thing that lets it lay a stage out left to right.
    pub outputs: Vec<String>,
    /// Pins drawn and connected to nothing.
    pub nc: Vec<NoConnect>,
    /// Pins drawn whose net has not been read yet.
    pub unread: Vec<Unread>,
    /// How completely this part has been read.
    pub read: Read,
    /// What the drawing prints, where this transcription deliberately records
    /// something else.
    ///
    /// The rarest and most dangerous kind of entry in a transcription: a place
    /// where the reader concluded the drawing is wrong. `U9`'s fourth section
    /// has its output labeled pin 11, where the identical section of the
    /// identical part `U10` two sheets-halves away is labeled 14 and pin 11 on
    /// a 14-pin quad is the negative supply, which cannot drive anything. So
    /// the transcription says 14 and this says why the drawing disagrees.
    ///
    /// Every such departure is a place a later reader will otherwise
    /// rediscover as a contradiction and "fix" back. Requires a `note`.
    pub drawing_says: Option<String>,
    /// How a multi-section part is drawn. Presentation.
    pub sections: Vec<Section>,
    /// Which subcircuit this part belongs to: one of the board's voices, a
    /// power supply, a mix bus.
    ///
    /// A whole board is one file, and a drawing is of one stage, so something
    /// has to bridge those. `netlist svg --group shot` cuts the group out and
    /// draws it, and a net crossing the group's edge becomes a port of the
    /// excerpt. That rule is what lets a board be transcribed once and drawn
    /// in pieces, instead of one file per picture, which is the duplication
    /// this design exists to remove.
    pub group: Option<String>,
    /// Which block of the drawing this part belongs to. Parts sharing a block
    /// become one box in the generated SVG. Presentation, per decision 4:
    /// grouping is how the picture reads, not a fact about the board.
    pub block: Option<String>,
    /// Anything about this part a later reader would otherwise have to
    /// re-derive: an ambiguous value, a settled argument, a rotated symbol.
    pub note: Option<String>,
}

impl Part {
    /// `R143 3.3k`, for a drawing label or a report line.
    pub fn label(&self) -> String {
        let value = self.value.label();
        if value.is_empty() {
            self.designator.clone()
        } else {
            format!("{} {}", self.designator, value)
        }
    }

    /// Whether the symbol draws this pin at all.
    pub fn draws(&self, pin: &str) -> bool {
        self.pins.iter().any(|p| p == pin)
    }
}

/// One net: a name and the pins on it.
#[derive(Debug, Clone)]
pub struct Net {
    /// The name the drawing gives it, or the name the transcription gives it
    /// when the drawing does not: `node X`, `PC0`, `+12V`.
    pub name: String,
    /// The `designator.pin` endpoints, in reading order.
    pub on: Vec<Endpoint>,
    /// A supply or ground. Rails are drawn as a separate stub per connection
    /// rather than as one node, because a rail with thirty endpoints is a
    /// hairball that hides the circuit.
    pub rail: bool,
    /// Set when the net leaves the excerpt: `input` or `output`. A port is
    /// allowed to have one endpoint, where an interior net is not.
    pub port: Option<Direction>,
    /// Why this net looks wrong and is not. The battleship's slow oscillator
    /// drives a follower and nothing else, traced three times; that is a
    /// finding about the board rather than a missing wire, and it belongs
    /// beside the net rather than in a suppression list.
    pub note: Option<String>,
}

/// Which way a port faces, from the excerpt's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Into the excerpt.
    Input,
    /// Out of the excerpt.
    Output,
}

impl Direction {
    /// The spelling netlistsvg wants.
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::Input => "input",
            Direction::Output => "output",
        }
    }
}

/// One end of a wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// The part's designator.
    pub part: String,
    /// The pin, named as that part's `pins` names it.
    pub pin: String,
}

impl fmt::Display for Endpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.part, self.pin)
    }
}

/// Where the reading came from, and how good it is.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Board {
    /// The board, as something a person would say: `Zaxxon sound`.
    pub name: String,
    /// The assembly number printed on the drawing.
    pub assembly: Option<String>,
    /// Which scan, and where it came from.
    pub source: Option<String>,
    /// Which sheets of it.
    pub sheets: Option<String>,
    /// The resolution it was read at. On this board 150 dpi separates signal
    /// names and does not separate 2.2K from 2.2M, so the number is part of
    /// the reading's provenance rather than trivia.
    pub dpi: Option<u32>,
    /// What this file covers, when it is an excerpt rather than a whole board.
    pub excerpt: Option<String>,
    /// The prose that carries the argument behind each reading. The netlist
    /// carries the result; this points at the reasoning.
    pub prose: Option<String>,
}

/// A whole transcription, loaded and checked.
#[derive(Debug, Clone)]
pub struct Netlist {
    /// Provenance.
    pub board: Board,
    /// Every part on the sheet, including the ones no device models.
    pub parts: Vec<Part>,
    /// Every net.
    pub nets: Vec<Net>,
}

impl Netlist {
    /// The part with this designator.
    pub fn part(&self, designator: &str) -> Option<&Part> {
        self.parts.iter().find(|p| p.designator == designator)
    }

    /// The net this pin is on, if it is on one.
    pub fn net_of(&self, designator: &str, pin: &str) -> Option<&Net> {
        self.nets
            .iter()
            .find(|n| n.on.iter().any(|e| e.part == designator && e.pin == pin))
    }

    /// Every group named by a part or one of its sections, in the order the
    /// file first mentions them.
    pub fn groups(&self) -> Vec<&str> {
        let mut seen: Vec<&str> = Vec::new();
        let named = self.parts.iter().flat_map(|part| {
            std::iter::once(part.group.as_deref())
                .chain(part.sections.iter().map(|s| s.group.as_deref()))
        });
        for group in named.flatten() {
            if !seen.contains(&group) {
                seen.push(group);
            }
        }
        seen
    }

    /// The pins this group owns: every pin of a part in it, plus the pins of
    /// any section in it on a part that is not.
    fn pins_in(&self, group: &str) -> Vec<(String, String)> {
        let mut pins = Vec::new();
        for part in &self.parts {
            if part.group.as_deref() == Some(group) {
                for pin in &part.pins {
                    pins.push((part.designator.clone(), pin.clone()));
                }
                continue;
            }
            for section in &part.sections {
                if section.group.as_deref() == Some(group) {
                    for pin in &section.pins {
                        pins.push((part.designator.clone(), pin.clone()));
                    }
                }
            }
        }
        pins
    }

    /// Cut one subcircuit out of a whole-board transcription, as its own
    /// netlist, so it can be drawn on its own.
    ///
    /// **A net that crosses the group's edge becomes a port**, and that is the
    /// whole trick. An excerpt's boundary is not something anyone has to
    /// declare: it is wherever a wire leaves, which the board already knows.
    /// The port faces out if the group drives the net and in otherwise.
    ///
    /// Rails stay rails, since a supply is not a signal leaving the excerpt.
    /// Endpoints outside the group are dropped from each net, so the excerpt
    /// never names a part it does not contain.
    pub fn subset(&self, group: &str) -> Netlist {
        let owned = self.pins_in(group);
        let inside =
            |designator: &str, pin: &str| owned.iter().any(|(d, p)| d == designator && p == pin);

        // A part joins the excerpt if any of its pins does, and brings only
        // those. A package shared between two voices appears in both drawings,
        // each time as the section that belongs there.
        let mut parts: Vec<Part> = Vec::new();
        for part in &self.parts {
            let whole = part.group.as_deref() == Some(group);
            let pins: Vec<String> = part
                .pins
                .iter()
                .filter(|pin| inside(&part.designator, pin))
                .cloned()
                .collect();
            if pins.is_empty() {
                continue;
            }
            parts.push(Part {
                pins,
                sections: part
                    .sections
                    .iter()
                    .filter(|s| whole || s.group.as_deref() == Some(group))
                    .cloned()
                    .collect(),
                nc: part
                    .nc
                    .iter()
                    .filter(|nc| inside(&part.designator, &nc.pin))
                    .cloned()
                    .collect(),
                unread: part
                    .unread
                    .iter()
                    .filter(|u| inside(&part.designator, &u.pin))
                    .cloned()
                    .collect(),
                ..part.clone()
            });
        }

        let mut nets = Vec::new();
        for net in &self.nets {
            let kept: Vec<Endpoint> = net
                .on
                .iter()
                .filter(|e| inside(&e.part, &e.pin))
                .cloned()
                .collect();
            if kept.is_empty() {
                continue;
            }
            let leaves = kept.len() < net.on.len();
            let port = if net.rail {
                net.port
            } else if leaves {
                // The group drives it if any pin of ours on it is an output.
                let driven_here = kept.iter().any(|e| {
                    self.part(&e.part)
                        .is_some_and(|p| p.outputs.contains(&e.pin))
                });
                Some(if driven_here {
                    Direction::Output
                } else {
                    Direction::Input
                })
            } else {
                net.port
            };
            nets.push(Net {
                name: net.name.clone(),
                on: kept,
                rail: net.rail,
                port,
                note: net.note.clone(),
            });
        }

        Netlist {
            board: Board {
                excerpt: Some(match &self.board.excerpt {
                    Some(excerpt) => format!("{excerpt}, {group}"),
                    None => group.to_string(),
                }),
                ..self.board.clone()
            },
            parts,
            nets,
        }
    }

    /// Read a transcription, checking it as far as the format itself can.
    ///
    /// Every problem is reported, not just the first: a file with 200 parts
    /// in it wants one list of what is wrong with it rather than 200 runs.
    pub fn load(path: &Path) -> Result<Netlist, LoadError> {
        let text = std::fs::read_to_string(path).map_err(|e| LoadError {
            path: path.display().to_string(),
            problems: vec![format!("cannot read: {e}")],
        })?;
        Netlist::parse(&text).map_err(|problems| LoadError {
            path: path.display().to_string(),
            problems,
        })
    }

    /// Read a transcription from TOML text.
    pub fn parse(text: &str) -> Result<Netlist, Vec<String>> {
        let raw: RawDoc = toml::from_str(text).map_err(|e| vec![e.to_string()])?;

        let mut problems = Vec::new();
        let mut parts = Vec::new();
        for raw_part in &raw.parts {
            match raw_part.resolve() {
                Ok(part) => parts.push(part),
                Err(why) => problems.push(why),
            }
        }
        let nets: Vec<Net> = raw.nets.iter().map(RawNet::resolve).collect();

        let netlist = Netlist {
            board: raw.board,
            parts,
            nets,
        };
        netlist.check(&mut problems);
        if problems.is_empty() {
            Ok(netlist)
        } else {
            Err(problems)
        }
    }

    /// The checks the format itself requires, as opposed to the lints that ask
    /// questions about the circuit. These are referential integrity: without
    /// them an endpoint can name a part that is not there and the file still
    /// looks like a netlist.
    ///
    /// The one that is not pure bookkeeping is the last: a drawn pin is on a
    /// net or it is `nc` with a reason, and there is no third state. That is
    /// decision 3, and it is what makes "you did not transcribe this" a
    /// question the file can be asked.
    fn check(&self, problems: &mut Vec<String>) {
        let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
        for part in &self.parts {
            *seen.entry(part.designator.as_str()).or_default() += 1;
        }
        for (designator, count) in &seen {
            if *count > 1 {
                problems.push(format!("{designator}: {count} parts share this designator"));
            }
        }

        let mut net_names: BTreeMap<&str, usize> = BTreeMap::new();
        for net in &self.nets {
            *net_names.entry(net.name.as_str()).or_default() += 1;
        }
        for (name, count) in &net_names {
            if *count > 1 {
                problems.push(format!("net `{name}`: declared {count} times"));
            }
        }

        // Every endpoint has to name a part that exists and a pin that part
        // draws, and no pin may be on two nets.
        let mut placed: BTreeMap<(String, String), Vec<&str>> = BTreeMap::new();
        for net in &self.nets {
            for endpoint in &net.on {
                match self.part(&endpoint.part) {
                    None => problems.push(format!(
                        "net `{}`: {endpoint} names a part that is not in this file",
                        net.name
                    )),
                    Some(part) if !part.draws(&endpoint.pin) => problems.push(format!(
                        "net `{}`: {endpoint} names a pin {} does not draw (it draws {})",
                        net.name,
                        part.designator,
                        part.pins.join(", ")
                    )),
                    Some(_) => {}
                }
                placed
                    .entry((endpoint.part.clone(), endpoint.pin.clone()))
                    .or_default()
                    .push(&net.name);
            }
        }
        for ((designator, pin), nets) in &placed {
            if nets.len() > 1 {
                problems.push(format!(
                    "{designator}.{pin}: on {} nets at once ({})",
                    nets.len(),
                    nets.join(", ")
                ));
            }
        }

        for part in &self.parts {
            for nc in &part.nc {
                if !part.draws(&nc.pin) {
                    problems.push(format!(
                        "{}: nc names pin {}, which the symbol does not draw",
                        part.designator, nc.pin
                    ));
                }
                if placed.contains_key(&(part.designator.clone(), nc.pin.clone())) {
                    problems.push(format!(
                        "{}.{}: marked nc and also on a net",
                        part.designator, nc.pin
                    ));
                }
            }
            for output in &part.outputs {
                if !part.draws(output) {
                    problems.push(format!(
                        "{}: out names pin {output}, which the symbol does not draw",
                        part.designator
                    ));
                }
            }
            if part.drawing_says.is_some() && part.note.is_none() {
                problems.push(format!(
                    "{}: drawing_says records a departure from the drawing and there is no \
                     `note` giving the reason. Contradicting the drawing is the one thing \
                     a transcription must always justify.",
                    part.designator
                ));
            }
            if part.device_inferred && !matches!(part.value, Value::Device(_)) {
                problems.push(format!(
                    "{}: device_inferred is set and there is no `device` to be inferred",
                    part.designator
                ));
            }
            if !part.sections.is_empty() && part.block.is_some() {
                problems.push(format!(
                    "{}: has sections and a block. A section is already a box in the \
                     drawing, so the two cannot both decide where this part goes.",
                    part.designator
                ));
            }
            let mut sectioned: Vec<&String> = Vec::new();
            for section in &part.sections {
                for pin in &section.pins {
                    if !part.draws(pin) {
                        problems.push(format!(
                            "{}: section {} names pin {pin}, which the symbol does not draw",
                            part.designator, section.name
                        ));
                    }
                    if sectioned.contains(&pin) {
                        problems.push(format!(
                            "{}.{pin}: in more than one section",
                            part.designator
                        ));
                    }
                    sectioned.push(pin);
                }
            }
            for unread in &part.unread {
                if !part.draws(&unread.pin) {
                    problems.push(format!(
                        "{}: unread names pin {}, which the symbol does not draw",
                        part.designator, unread.pin
                    ));
                }
                if placed.contains_key(&(part.designator.clone(), unread.pin.clone())) {
                    problems.push(format!(
                        "{}.{}: marked unread and also on a net",
                        part.designator, unread.pin
                    ));
                }
                if part.nc.iter().any(|nc| nc.pin == unread.pin) {
                    problems.push(format!(
                        "{}.{}: marked both nc and unread. Those are opposite claims: \
                         one says the drawing leaves it open, the other says nobody has \
                         looked.",
                        part.designator, unread.pin
                    ));
                }
            }
            for pin in &part.pins {
                let on_net = placed.contains_key(&(part.designator.clone(), pin.clone()));
                let no_connect = part.nc.iter().any(|nc| &nc.pin == pin);
                let not_read = part.unread.iter().any(|u| &u.pin == pin);
                if !on_net && !no_connect && !not_read {
                    problems.push(format!(
                        "{}.{pin}: drawn, but on no net, not marked nc, and not marked \
                         unread. A pin the symbol draws is wired, deliberately open, or \
                         admittedly not read yet; if the symbol does not draw it, leave \
                         it out of `pins`.",
                        part.designator
                    ));
                }
            }
        }
    }
}

/// A transcription that would not load, with every reason at once.
#[derive(Debug)]
pub struct LoadError {
    /// The file.
    pub path: String,
    /// What is wrong with it.
    pub problems: Vec<String>,
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "{}: {} problems", self.path, self.problems.len())?;
        for problem in &self.problems {
            writeln!(f, "  {problem}")?;
        }
        Ok(())
    }
}

impl std::error::Error for LoadError {}

// ---------------------------------------------------------------------------
// The on-disk shape. Separate from the loaded shape so that the file can name
// a value in the unit the drawing prints and the rest of the program can deal
// in farads.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawDoc {
    board: Board,
    #[serde(default)]
    parts: Vec<RawPart>,
    #[serde(default)]
    nets: Vec<RawNet>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPart {
    #[serde(rename = "ref")]
    designator: String,
    kind: Kind,
    device: Option<String>,
    #[serde(default)]
    device_inferred: bool,

    ohms: Option<f64>,
    kohms: Option<f64>,
    mohms: Option<f64>,
    pf: Option<f64>,
    nf: Option<f64>,
    uf: Option<f64>,
    uh: Option<f64>,
    mh: Option<f64>,
    hz: Option<f64>,
    khz: Option<f64>,
    mhz: Option<f64>,

    pins: Option<Vec<String>>,
    #[serde(default)]
    out: Vec<String>,
    #[serde(default)]
    nc: Vec<NoConnect>,
    #[serde(default)]
    unread: Vec<Unread>,
    #[serde(default)]
    read: Read,
    drawing_says: Option<String>,
    #[serde(default)]
    sections: Vec<Section>,
    group: Option<String>,
    block: Option<String>,
    note: Option<String>,
}

impl RawPart {
    fn resolve(&self) -> Result<Part, String> {
        let value = self.value()?;
        let pins = match (&self.pins, self.kind.default_pins()) {
            (Some(pins), _) if pins.is_empty() => {
                return Err(format!("{}: `pins` is empty", self.designator));
            }
            (Some(pins), _) => pins.clone(),
            (None, Some(default)) => default.iter().map(|p| (*p).to_string()).collect(),
            (None, None) => {
                return Err(format!(
                    "{}: kind {} has no default pins, so `pins` is required",
                    self.designator, self.kind
                ));
            }
        };
        let mut seen = pins.clone();
        seen.sort();
        seen.dedup();
        if seen.len() != pins.len() {
            return Err(format!("{}: `pins` repeats a pin", self.designator));
        }
        Ok(Part {
            designator: self.designator.clone(),
            kind: self.kind,
            value,
            device_inferred: self.device_inferred,
            pins,
            outputs: self.out.clone(),
            nc: self.nc.clone(),
            unread: self.unread.clone(),
            // A part with an unread pin is partial whether it says so or not.
            // Deriving it here means the two can never disagree.
            read: if self.unread.is_empty() {
                self.read
            } else {
                Read::Partial
            },
            drawing_says: self.drawing_says.clone(),
            sections: self.sections.clone(),
            group: self.group.clone(),
            block: self.block.clone(),
            note: self.note.clone(),
        })
    }

    /// Exactly one value key, and it has to be one this kind takes. A
    /// capacitor with `kohms` on it is a copy-paste, and a resistor with no
    /// value at all is a part somebody meant to come back to.
    fn value(&self) -> Result<Value, String> {
        let given: Vec<(&str, f64, Value)> = [
            ("ohms", self.ohms, 1.0, Value::Ohms as fn(f64) -> Value),
            ("kohms", self.kohms, 1e3, Value::Ohms),
            ("mohms", self.mohms, 1e6, Value::Ohms),
            ("pf", self.pf, 1e-12, Value::Farads),
            ("nf", self.nf, 1e-9, Value::Farads),
            ("uf", self.uf, 1e-6, Value::Farads),
            ("uh", self.uh, 1e-6, Value::Henries),
            ("mh", self.mh, 1e-3, Value::Henries),
            ("hz", self.hz, 1.0, Value::Hertz),
            ("khz", self.khz, 1e3, Value::Hertz),
            ("mhz", self.mhz, 1e6, Value::Hertz),
        ]
        .into_iter()
        .filter_map(|(key, value, scale, wrap)| value.map(|v| (key, v, wrap(v * scale))))
        .collect();

        if given.len() > 1 {
            let keys: Vec<&str> = given.iter().map(|(k, _, _)| *k).collect();
            return Err(format!(
                "{}: more than one value key ({})",
                self.designator,
                keys.join(", ")
            ));
        }

        let expected = match self.kind {
            Kind::R | Kind::Pot => Some("ohms"),
            Kind::C => Some("farads"),
            Kind::L => Some("henries"),
            Kind::X => Some("hertz"),
            Kind::D | Kind::Q | Kind::U | Kind::J => None,
        };

        match (given.into_iter().next(), expected) {
            (Some((key, _, value)), Some(want)) => {
                let matches = matches!(
                    (&value, want),
                    (Value::Ohms(_), "ohms")
                        | (Value::Farads(_), "farads")
                        | (Value::Henries(_), "henries")
                        | (Value::Hertz(_), "hertz")
                );
                if matches {
                    Ok(value)
                } else {
                    Err(format!(
                        "{}: kind {} takes a value in {want}, and carries `{key}`",
                        self.designator, self.kind
                    ))
                }
            }
            (Some((key, _, _)), None) => Err(format!(
                "{}: kind {} takes no quantity, and carries `{key}`",
                self.designator, self.kind
            )),
            (None, Some(want)) => Err(format!(
                "{}: kind {} needs a value in {want}",
                self.designator, self.kind
            )),
            (None, None) => match (&self.device, self.kind) {
                (Some(device), _) => Ok(Value::Device(device.clone())),
                // An IC with no part number is not a transcription of
                // anything: its whole behavior is its type.
                (None, Kind::U) => Err(format!(
                    "{}: an IC needs `device`, the part number on the can",
                    self.designator
                )),
                (None, _) => Ok(Value::None),
            },
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawNet {
    name: String,
    #[serde(default)]
    on: Vec<String>,
    #[serde(default)]
    rail: bool,
    port: Option<Direction>,
    note: Option<String>,
}

impl RawNet {
    fn resolve(&self) -> Net {
        Net {
            name: self.name.clone(),
            on: self.on.iter().map(|s| parse_endpoint(s)).collect(),
            rail: self.rail,
            port: self.port,
            note: self.note.clone(),
        }
    }
}

/// `R143.b` into its two halves. Split at the last dot, because an IC pin is
/// never a dotted name and a designator never contains one, but a future
/// hierarchical name might.
fn parse_endpoint(text: &str) -> Endpoint {
    match text.rsplit_once('.') {
        Some((part, pin)) => Endpoint {
            part: part.to_string(),
            pin: pin.to_string(),
        },
        // A malformed endpoint becomes a part that does not exist, which the
        // integrity check reports by name. Failing here instead would cost the
        // "every problem at once" property for no gain.
        None => Endpoint {
            part: text.to_string(),
            pin: String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHAPER: &str = r#"
[board]
name = "test"

[[parts]]
ref = "R143"
kind = "R"
kohms = 3.3

[[parts]]
ref = "R144"
kind = "R"
ohms = 560

[[nets]]
name = "+5V"
rail = true
on = ["R143.a"]

[[nets]]
name = "shaper"
on = ["R143.b", "R144.b"]

[[nets]]
name = "Qbar"
port = "input"
on = ["R144.a"]
"#;

    #[test]
    fn a_minimal_transcription_loads() {
        let netlist = Netlist::parse(SHAPER).expect("should load");
        assert_eq!(netlist.parts.len(), 2);
        assert_eq!(netlist.part("R143").unwrap().value, Value::Ohms(3300.0));
        assert_eq!(netlist.part("R144").unwrap().value, Value::Ohms(560.0));
        assert_eq!(netlist.net_of("R144", "b").unwrap().name, "shaper");
    }

    #[test]
    fn two_terminal_parts_get_their_pins_by_default() {
        let netlist = Netlist::parse(SHAPER).expect("should load");
        assert_eq!(netlist.part("R143").unwrap().pins, vec!["a", "b"]);
    }

    /// The unit is in the key, so the file reads the way the drawing does and
    /// the loader still hands out SI. This is decision 2: nothing parses a
    /// value back out of a label.
    /// Read one part's value out of a transcription, wired up enough to load.
    fn value_of(body: &str) -> f64 {
        let text = format!(
            "[board]\nname = \"t\"\n\n[[parts]]\nref = \"P1\"\n{body}\n\
             \n[[nets]]\nname = \"n1\"\nport = \"input\"\non = [\"P1.a\"]\n\
             \n[[nets]]\nname = \"n2\"\nport = \"input\"\non = [\"P1.b\"]\n"
        );
        Netlist::parse(&text)
            .expect("should load")
            .part("P1")
            .unwrap()
            .value
            .quantity()
            .expect("should carry a quantity")
    }

    #[test]
    fn values_normalize_to_si_from_whichever_unit_the_drawing_prints() {
        // Scaling by a decade is not exact in binary, so these compare to a
        // relative tolerance rather than bit for bit. The point of the test is
        // that the unit in the key is applied, not that 47e-9 round trips
        // through a multiply unchanged.
        let cases = [
            ("kind = \"C\"\npf = 680", 680e-12),
            ("kind = \"C\"\nuf = 0.047", 47e-9),
            ("kind = \"C\"\nnf = 47", 47e-9),
            ("kind = \"R\"\nmohms = 2.2", 2.2e6),
            ("kind = \"R\"\nkohms = 3.3", 3300.0),
            ("kind = \"R\"\nohms = 51000", 51000.0),
        ];
        for (body, want) in cases {
            let got = value_of(body);
            assert!(
                (got - want).abs() <= want.abs() * 1e-12,
                "{body}: got {got}, want {want}"
            );
        }
    }

    /// 0.047 uF and 0.47 uF differ by ten and the drawing writes `047UF` with
    /// no decimal point. Whatever the file says, it says as a number.
    #[test]
    fn the_two_readings_of_c88_are_different_numbers() {
        let tight = value_of("kind = \"C\"\nuf = 0.047");
        let loose = value_of("kind = \"C\"\nuf = 0.47");
        assert!((tight * 1e9 - 47.0).abs() < 1e-6, "{tight}");
        assert!((loose * 1e9 - 470.0).abs() < 1e-6, "{loose}");
    }

    #[test]
    fn a_capacitor_carrying_a_resistance_is_rejected() {
        let text = "[board]\nname = \"t\"\n\n[[parts]]\nref = \"C1\"\nkind = \"C\"\nkohms = 3.3\n";
        let problems = Netlist::parse(text).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("farads")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_resistor_with_no_value_is_rejected() {
        let text = "[board]\nname = \"t\"\n\n[[parts]]\nref = \"R1\"\nkind = \"R\"\n";
        let problems = Netlist::parse(text).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("needs a value")),
            "{problems:?}"
        );
    }

    #[test]
    fn two_value_keys_on_one_part_are_rejected() {
        let text = "[board]\nname = \"t\"\n\n[[parts]]\nref = \"R1\"\nkind = \"R\"\nohms = 51\nkohms = 51\n";
        let problems = Netlist::parse(text).unwrap_err();
        assert!(
            problems
                .iter()
                .any(|p| p.contains("more than one value key")),
            "{problems:?}"
        );
    }

    #[test]
    fn an_ic_without_a_part_number_is_rejected() {
        let text =
            "[board]\nname = \"t\"\n\n[[parts]]\nref = \"U1\"\nkind = \"U\"\npins = [\"1\"]\n";
        let problems = Netlist::parse(text).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("device")),
            "{problems:?}"
        );
    }

    #[test]
    fn an_ic_without_pins_is_rejected() {
        let text =
            "[board]\nname = \"t\"\n\n[[parts]]\nref = \"U1\"\nkind = \"U\"\ndevice = \"555\"\n";
        let problems = Netlist::parse(text).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("`pins` is required")),
            "{problems:?}"
        );
    }

    /// Decision 3. `U21`'s `Q` on pin 13 is drawn and reaches nothing, and
    /// reading it as the driver inverts the whole voice. A pin the symbol
    /// draws and the file forgets is the error this rejects.
    #[test]
    fn a_drawn_pin_is_wired_or_declared_dead_and_there_is_no_third_state() {
        let base = "[board]\nname = \"t\"\n\n[[parts]]\nref = \"U21\"\nkind = \"U\"\n\
                    device = \"74123\"\npins = [\"4\", \"13\"]\n\
                    \n[[nets]]\nname = \"Qbar\"\nport = \"output\"\non = [\"U21.4\"]\n";

        let problems = Netlist::parse(base).unwrap_err();
        assert!(
            problems
                .iter()
                .any(|p| p.contains("U21.13") && p.contains("not marked nc")),
            "{problems:?}"
        );

        let declared = format!(
            "{base}\n[[parts.nc]]\npin = \"13\"\nwhy = \"Q is drawn and connects to nothing\"\n"
        );
        // `[[parts.nc]]` has to sit under its part, so rebuild rather than append.
        let declared = declared.replace(
            "pins = [\"4\", \"13\"]\n",
            "pins = [\"4\", \"13\"]\nnc = [{ pin = \"13\", why = \"Q is drawn and reaches nothing\" }]\n",
        );
        let declared = declared.split("\n[[parts.nc]]").next().unwrap().to_string();
        Netlist::parse(&declared).expect("a declared nc should load");
    }

    /// The third pin state. A drawn pin whose net nobody has read yet is a
    /// to-do item that the file states rather than an omission it hides.
    #[test]
    fn a_pin_can_be_admitted_unread_and_that_makes_its_part_partial() {
        let text = "[board]\nname = \"t\"\n\n[[parts]]\nref = \"Q7\"\nkind = \"Q\"\n\
                    device = \"C1684\"\n\
                    unread = [{ pin = \"e\", why = \"the emitter return is not on the crop\" }]\n\
                    \n[[nets]]\nname = \"base\"\nport = \"input\"\non = [\"Q7.b\"]\n\
                    \n[[nets]]\nname = \"collector\"\nport = \"output\"\non = [\"Q7.c\"]\n";
        let netlist = Netlist::parse(text).expect("should load");
        let q7 = netlist.part("Q7").unwrap();
        assert_eq!(q7.unread.len(), 1);
        assert_eq!(q7.read, Read::Partial, "an unread pin makes a part partial");
    }

    /// `nc` and `unread` are opposite claims: one says the drawing leaves the
    /// pin open, the other says nobody has looked. A pin cannot be both.
    #[test]
    fn a_pin_cannot_be_both_dead_and_unread() {
        let text = "[board]\nname = \"t\"\n\n[[parts]]\nref = \"U1\"\nkind = \"U\"\n\
                    device = \"555\"\npins = [\"3\"]\n\
                    nc = [{ pin = \"3\", why = \"open\" }]\n\
                    unread = [{ pin = \"3\", why = \"not looked at\" }]\n";
        let problems = Netlist::parse(text).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("both nc and unread")),
            "{problems:?}"
        );
    }

    #[test]
    fn an_unread_pin_that_is_actually_wired_is_rejected() {
        let text = "[board]\nname = \"t\"\n\n[[parts]]\nref = \"R1\"\nkind = \"R\"\nohms = 1\n\
                    unread = [{ pin = \"b\", why = \"not looked at\" }]\n\
                    \n[[nets]]\nname = \"a\"\nport = \"input\"\non = [\"R1.a\", \"R1.b\"]\n";
        let problems = Netlist::parse(text).unwrap_err();
        assert!(
            problems
                .iter()
                .any(|p| p.contains("marked unread and also on a net")),
            "{problems:?}"
        );
    }

    #[test]
    fn contradicting_the_drawing_without_a_reason_is_rejected() {
        let text = "[board]\nname = \"t\"\n\n[[parts]]\nref = \"U9\"\nkind = \"U\"\n\
                    device = \"LM324\"\npins = [\"14\"]\n\
                    drawing_says = \"this output is labeled pin 11\"\n\
                    \n[[nets]]\nname = \"n\"\nport = \"output\"\non = [\"U9.14\"]\n";
        let problems = Netlist::parse(text).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("must always justify")),
            "{problems:?}"
        );
    }

    /// An `nc` with no reason is a transcription that gave up, so the field is
    /// not optional. This checks the schema rejects it rather than defaulting.
    #[test]
    fn an_nc_without_a_reason_is_rejected() {
        let text = "[board]\nname = \"t\"\n\n[[parts]]\nref = \"U1\"\nkind = \"U\"\n\
                    device = \"555\"\npins = [\"3\"]\nnc = [{ pin = \"3\" }]\n";
        let problems = Netlist::parse(text).unwrap_err();
        assert!(problems.iter().any(|p| p.contains("why")), "{problems:?}");
    }

    #[test]
    fn an_nc_pin_that_is_also_wired_is_rejected() {
        let text = "[board]\nname = \"t\"\n\n[[parts]]\nref = \"U1\"\nkind = \"U\"\n\
                    device = \"555\"\npins = [\"3\"]\nnc = [{ pin = \"3\", why = \"open\" }]\n\
                    \n[[nets]]\nname = \"n\"\nport = \"output\"\non = [\"U1.3\"]\n";
        let problems = Netlist::parse(text).unwrap_err();
        assert!(
            problems
                .iter()
                .any(|p| p.contains("marked nc and also on a net")),
            "{problems:?}"
        );
    }

    /// A pin absent from `pins` is a different statement from a dead pin:
    /// `U7`'s output pin is not on the drawing at all, which is what says that
    /// 555 is read at its capacitor. Naming one on a net is the error.
    #[test]
    fn a_net_cannot_reach_a_pin_the_symbol_does_not_draw() {
        let text = "[board]\nname = \"t\"\n\n[[parts]]\nref = \"U7\"\nkind = \"U\"\n\
                    device = \"555\"\npins = [\"2\", \"6\"]\n\
                    \n[[nets]]\nname = \"ramp\"\nport = \"output\"\non = [\"U7.2\", \"U7.6\"]\n\
                    \n[[nets]]\nname = \"out\"\nport = \"output\"\non = [\"U7.3\"]\n";
        let problems = Netlist::parse(text).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("does not draw")),
            "{problems:?}"
        );
    }

    #[test]
    fn a_duplicate_designator_is_rejected() {
        let text = "[board]\nname = \"t\"\n\n[[parts]]\nref = \"R1\"\nkind = \"R\"\nohms = 1\n\
                    \n[[parts]]\nref = \"R1\"\nkind = \"R\"\nohms = 2\n";
        let problems = Netlist::parse(text).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("share this designator")),
            "{problems:?}"
        );
    }

    #[test]
    fn an_endpoint_naming_a_part_that_is_not_here_is_rejected() {
        let text = "[board]\nname = \"t\"\n\n[[parts]]\nref = \"R1\"\nkind = \"R\"\nohms = 1\n\
                    \n[[nets]]\nname = \"a\"\nport = \"input\"\non = [\"R1.a\", \"R9.b\"]\n\
                    \n[[nets]]\nname = \"b\"\nport = \"input\"\non = [\"R1.b\"]\n";
        let problems = Netlist::parse(text).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("not in this file")),
            "{problems:?}"
        );
    }

    #[test]
    fn one_pin_on_two_nets_is_rejected() {
        let text = "[board]\nname = \"t\"\n\n[[parts]]\nref = \"R1\"\nkind = \"R\"\nohms = 1\n\
                    \n[[nets]]\nname = \"a\"\nport = \"input\"\non = [\"R1.a\"]\n\
                    \n[[nets]]\nname = \"b\"\nport = \"input\"\non = [\"R1.a\", \"R1.b\"]\n";
        let problems = Netlist::parse(text).unwrap_err();
        assert!(
            problems.iter().any(|p| p.contains("on 2 nets at once")),
            "{problems:?}"
        );
    }

    /// A misspelled key that deserializes to nothing is how a transcription
    /// silently loses a part. `deny_unknown_fields` is load-bearing.
    #[test]
    fn an_unknown_key_is_rejected_rather_than_ignored() {
        let text = "[board]\nname = \"t\"\n\n[[parts]]\nref = \"R1\"\nkind = \"R\"\nkohm = 3.3\n";
        let problems = Netlist::parse(text).unwrap_err();
        assert!(problems.iter().any(|p| p.contains("kohm")), "{problems:?}");
    }

    const TWO_GROUPS: &str = r#"
[board]
name = "t"

[[parts]]
ref = "U1"
kind = "U"
device = "555"
pins = ["3"]
out = ["3"]
group = "oscillator"

# A three-terminal part so that the oscillator can reach ground and the mix
# from different pins, which is what makes this fixture legal: one pin sits on
# one net.
[[parts]]
ref = "R1"
kind = "R"
kohms = 10
pins = ["a", "b", "c"]
group = "oscillator"

[[parts]]
ref = "R2"
kind = "R"
kohms = 51
group = "mix"

[[parts]]
ref = "R3"
kind = "R"
kohms = 8.2
group = "mix"

[[nets]]
name = "GND"
rail = true
on = ["R1.b", "R3.b"]

[[nets]]
name = "U1 out"
on = ["U1.3", "R1.a"]

[[nets]]
name = "leg"
on = ["R1.c", "R2.a"]

[[nets]]
name = "sum"
port = "output"
on = ["R2.b", "R3.a"]
"#;

    #[test]
    fn a_group_is_cut_out_with_only_its_own_parts() {
        let netlist = Netlist::parse(TWO_GROUPS).expect("should load");
        let mix = netlist.subset("mix");
        assert_eq!(mix.parts.len(), 2);
        assert!(mix.part("R2").is_some());
        assert!(mix.part("R1").is_none(), "a part outside the group is gone");
        for net in &mix.nets {
            for endpoint in &net.on {
                assert!(
                    mix.part(&endpoint.part).is_some(),
                    "excerpt names {endpoint}, which it does not contain"
                );
            }
        }
    }

    /// The boundary of an excerpt is not something anyone declares. It is
    /// wherever a wire leaves, and the board already knows that.
    #[test]
    fn a_net_crossing_the_groups_edge_becomes_a_port() {
        let netlist = Netlist::parse(TWO_GROUPS).expect("should load");

        let mix = netlist.subset("mix");
        let leg = mix.nets.iter().find(|n| n.name == "leg").unwrap();
        assert_eq!(leg.port, Some(Direction::Input), "the mix receives the leg");

        // The same net crosses the other way too, so it is a port on both
        // sides. Its direction there falls back to `input`, because the pin on
        // it is a plain resistor end and nothing declares a driver: direction
        // is layout, and the fallback is the documented one.
        let oscillator = netlist.subset("oscillator");
        let leg = oscillator.nets.iter().find(|n| n.name == "leg").unwrap();
        assert_eq!(leg.port, Some(Direction::Input));
        assert_eq!(leg.on.len(), 1, "only the oscillator's own end of it");
    }

    #[test]
    fn a_rail_stays_a_rail_rather_than_becoming_a_port() {
        let netlist = Netlist::parse(TWO_GROUPS).expect("should load");
        let mix = netlist.subset("mix");
        let gnd = mix.nets.iter().find(|n| n.name == "GND").unwrap();
        assert!(gnd.rail);
        assert_eq!(gnd.port, None);
        assert_eq!(gnd.on.len(), 1, "only the endpoint inside the group");
    }

    /// A net wholly inside a group keeps whatever the board said it was, so an
    /// interior wire does not sprout a port just because a group was cut.
    #[test]
    fn a_net_wholly_inside_a_group_is_unchanged() {
        let netlist = Netlist::parse(TWO_GROUPS).expect("should load");
        let oscillator = netlist.subset("oscillator");
        let out = oscillator.nets.iter().find(|n| n.name == "U1 out").unwrap();
        assert_eq!(out.port, None);
        assert_eq!(out.on.len(), 2);
    }

    /// One `MB4391` runs the medium explosion on one channel and the cannon
    /// on the other, so the unit a drawing is cut along is the section rather
    /// than the part. The package appears in both drawings, each time as the
    /// half that belongs there.
    const SHARED_PACKAGE: &str = r#"
[board]
name = "t"

[[parts]]
ref = "U13"
kind = "U"
device = "MB4391"
pins = ["1", "2", "5", "6"]
out = ["1", "5"]

[[parts.sections]]
name = "a"
pins = ["1", "2"]
role = "VCA"
group = "cannon"

[[parts.sections]]
name = "b"
pins = ["5", "6"]
role = "VCA"
group = "medium explosion"

[[parts]]
ref = "R200"
kind = "R"
kohms = 47
group = "cannon"

[[parts]]
ref = "R197"
kind = "R"
kohms = 8.2
group = "medium explosion"

[[nets]]
name = "cannon in"
port = "input"
on = ["U13.2"]

[[nets]]
name = "cannon out"
on = ["U13.1", "R200.a"]

[[nets]]
name = "medium in"
port = "input"
on = ["U13.6"]

[[nets]]
name = "medium out"
on = ["U13.5", "R197.a"]

[[nets]]
name = "SJ"
port = "output"
on = ["R200.b", "R197.b"]
"#;

    #[test]
    fn a_package_shared_between_voices_appears_in_both_with_only_its_own_half() {
        let netlist = Netlist::parse(SHARED_PACKAGE).expect("should load");

        let cannon = netlist.subset("cannon");
        let u13 = cannon.part("U13").expect("the package is in the cannon");
        assert_eq!(u13.pins, vec!["1", "2"], "only channel A's pins");
        assert_eq!(u13.sections.len(), 1);
        assert_eq!(u13.sections[0].name, "a");
        assert!(
            cannon.part("R197").is_none(),
            "the other voice's leg is not here"
        );

        let medium = netlist.subset("medium explosion");
        let u13 = medium.part("U13").expect("and in the medium explosion");
        assert_eq!(u13.pins, vec!["5", "6"], "only channel B's pins");
        assert_eq!(u13.sections[0].name, "b");

        // Neither excerpt may name a pin it does not contain.
        for subset in [&cannon, &medium] {
            for net in &subset.nets {
                for endpoint in &net.on {
                    let part = subset.part(&endpoint.part).expect("part is present");
                    assert!(
                        part.draws(&endpoint.pin),
                        "excerpt names {endpoint}, which its copy of the part does not draw"
                    );
                }
            }
        }
    }

    #[test]
    fn a_section_group_is_listed_alongside_a_part_group() {
        let netlist = Netlist::parse(SHARED_PACKAGE).expect("should load");
        let groups = netlist.groups();
        assert!(groups.contains(&"cannon"), "{groups:?}");
        assert!(groups.contains(&"medium explosion"), "{groups:?}");
    }

    #[test]
    fn groups_are_listed_in_the_order_the_file_mentions_them() {
        let netlist = Netlist::parse(TWO_GROUPS).expect("should load");
        assert_eq!(netlist.groups(), vec!["oscillator", "mix"]);
    }

    #[test]
    fn engineering_notation_reads_like_the_drawing() {
        assert_eq!(engineering(3300.0), "3.3k");
        assert_eq!(engineering(33000.0), "33k");
        assert_eq!(engineering(820.0), "820");
        assert_eq!(engineering(2.2e6), "2.2M");
        assert_eq!(engineering(680e-12), "680p");
        assert_eq!(engineering(47e-9), "47n");
        assert_eq!(engineering(2.2e-6), "2.2u");
        assert_eq!(engineering(15e-6), "15u");
    }
}
