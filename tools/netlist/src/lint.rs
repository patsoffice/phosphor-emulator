//! Cheap checks over a transcription. Each one catches an error this board
//! actually had.
//!
//! See `docs/designs/schematic-transcription.md`, rung 3. The headline is
//! `not-modeled`: "which parts on this sheet does the device not model" is the
//! question nobody could ask, and `C94` is what that cost.
//!
//! Three of the five checks the design lists are not here, because the loader
//! already refuses a file that breaks them: a pin that is neither wired, `nc`
//! nor `unread`, a duplicate designator, and a pin on two nets at once. A rule
//! the format enforces is better than a rule a tool reports, so `lint` names
//! them as enforced rather than re-implementing them.

use crate::netlist::{Netlist, Part, Value};
use std::collections::{BTreeMap, BTreeSet};

/// How much a finding wants doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// The transcription is wrong or incomplete.
    Problem,
    /// Something the transcription already explains. It still prints, because
    /// a finding that disappears when annotated is a suppression, and the
    /// point of the annotation is that the next reader sees the explanation
    /// rather than re-deriving it.
    Explained,
    /// Not a defect. The parts-not-modeled list is this: a gap between the
    /// drawing and the device is a question, and sometimes the answer is that
    /// the approximation is fine.
    Observation,
}

impl Severity {
    /// The tag that starts the line.
    pub fn tag(self) -> &'static str {
        match self {
            Severity::Problem => "problem ",
            Severity::Explained => "explained",
            Severity::Observation => "note    ",
        }
    }
}

/// One thing a lint has to say.
#[derive(Debug, Clone)]
pub struct Finding {
    /// Which check produced it.
    pub lint: &'static str,
    /// How much it wants doing.
    pub severity: Severity,
    /// What it is about.
    pub subject: String,
    /// What is wrong, or what was noticed.
    pub detail: String,
}

/// What the device declares it models, derived rather than maintained.
///
/// This was specified as a stopgap that rung 4 would retire by generating the
/// constants. **Rung 4 was cut, so it is permanent**, and that is fine for the
/// reason it was acceptable as a stopgap: the device's own constant *names* are
/// the declaration, which is weaker than a manifest in one way and stronger in
/// another, because it cannot go stale. It is the code.
///
/// Constant names only, never comments. On this board that distinction is the
/// whole result: `zaxxon_sound.rs` mentions `C94` in a doc comment, in a
/// sentence saying it is *not* modeled. A check that read comments would count
/// that as coverage and report nothing.
#[derive(Debug, Clone, Default)]
pub struct DeviceParts {
    /// The designators the device's constant names carry.
    pub named: BTreeSet<String>,
    /// Constants that name exactly one designator and give a plain number, by
    /// designator. These are the ones whose value can be held against the
    /// sheet's.
    pub values: BTreeMap<String, Constant>,
    /// Constants naming more than one designator, which are not checked. See
    /// [`value_disagrees`] for why, and why saying so out loud matters.
    pub compound: Vec<String>,
}

/// One device constant, as written.
#[derive(Debug, Clone)]
pub struct Constant {
    /// The constant's name, so a finding can point at the line to change.
    pub name: String,
    /// Its literal, in whatever unit the source writes it in.
    pub value: f64,
}

impl DeviceParts {
    /// Read a device source file and collect the designators its constants
    /// name. `const R156` contributes `R156`; `const R145_R146` contributes
    /// both, which is how this device writes a pair it has already summed.
    ///
    /// The literal is collected too, where there is one to collect. A constant
    /// whose right-hand side is an expression rather than a number is a
    /// *derived* quantity, and this deliberately does not try to evaluate one:
    /// `shot_pitch_r` is two resistors in parallel and comparing it against
    /// either of them would be nonsense.
    pub fn from_source(source: &str) -> DeviceParts {
        let mut named = BTreeSet::new();
        let mut values: BTreeMap<String, Constant> = BTreeMap::new();
        let mut compound = Vec::new();
        for line in source.lines() {
            let line = line.trim_start();
            let Some(rest) = line.strip_prefix("const ") else {
                continue;
            };
            let Some((name, tail)) = rest.split_once(':') else {
                continue;
            };
            let name = name.trim();
            let mut here: Vec<&str> = Vec::new();
            for token in name.split('_') {
                if is_designator(token) {
                    named.insert(token.to_string());
                    here.push(token);
                }
            }
            let literal = tail
                .split_once('=')
                .and_then(|(_, rhs)| numeric_literal(rhs));
            match (here.as_slice(), literal) {
                ([one], Some(value)) => {
                    values.entry((*one).to_string()).or_insert(Constant {
                        name: name.to_string(),
                        value,
                    });
                }
                ([_, _, ..], Some(_)) => compound.push(name.to_string()),
                _ => {}
            }
        }
        DeviceParts {
            named,
            values,
            compound,
        }
    }
}

/// A Rust numeric literal, or nothing if the right-hand side is an expression.
///
/// Nothing is the common case and the important one: most of what a device
/// holds is computed, and a check that guessed at an expression would be worse
/// than no check.
fn numeric_literal(rhs: &str) -> Option<f64> {
    let rhs = rhs.split("//").next()?.trim().trim_end_matches(';').trim();
    let rhs = rhs.trim_end_matches("f64").trim_end_matches("f32");
    let cleaned: String = rhs.chars().filter(|c| *c != '_').collect();
    cleaned.parse::<f64>().ok()
}

/// `R156`, `C94`, `U19`: a reference-designator prefix then digits, which is
/// how every designator on these sheets is written. `OPAMP` and `SWING` are
/// not, and neither is `CANNON`, so a compound constant name contributes only
/// the parts of it that are parts.
///
/// **The prefix has to be a known one**, and that is not fussiness. The first
/// run of the `no-part` check against a whole board reported `MB4391`,
/// `MM5837` and `K74123`, which are part numbers, and `V5`, `V6` and `V12`,
/// which are rail voltages. All six match "letters then digits" and none is a
/// designator. Six false positives against one real finding is the ratio at
/// which a check stops being read, so the rule is the prefix list a schematic
/// actually uses rather than the shape of the token.
fn is_designator(token: &str) -> bool {
    const PREFIXES: [&str; 12] = [
        "R", "C", "L", "D", "Q", "U", "X", "J", "P", "VR", "RP", "PC",
    ];
    let letters = token.chars().take_while(|c| c.is_ascii_uppercase()).count();
    let digits = token.len() - letters;
    PREFIXES.contains(&&token[..letters])
        && digits >= 1
        && token[letters..].chars().all(|c| c.is_ascii_digit())
}

/// Whether a part carries a quantity the device could hold as a constant.
///
/// An IC, a diode or a transistor has no value for a constant to be, so its
/// absence from the device's constants says nothing: the device models what
/// `U19` *does*. Only a part with a number can be missing a number.
fn carries_a_quantity(part: &Part) -> bool {
    matches!(
        part.value,
        Value::Ohms(_) | Value::Farads(_) | Value::Henries(_) | Value::Hertz(_)
    )
}

/// Run every lint.
pub fn run(netlist: &Netlist, device: Option<&DeviceParts>) -> Vec<Finding> {
    let mut findings = Vec::new();
    floating_nets(netlist, &mut findings);
    unread_pins(netlist, &mut findings);
    inferred_devices(netlist, &mut findings);
    departures(netlist, &mut findings);
    if let Some(device) = device {
        not_modeled(netlist, device, &mut findings);
        value_disagrees(netlist, device, &mut findings);
        no_part(netlist, device, &mut findings);
    }
    findings
}

/// A value the sheet and the device both carry, and disagree about.
///
/// The drift class, caught at the value level. `zaxxon_sound.rs` writes
/// `const R156: f64 = 33_000.0` and the transcription writes `kohms = 33`, and
/// until this check nothing held the two together: `lint` read the constant's
/// *name* to ask which parts the device models and never looked at the number
/// beside it.
///
/// **This is not what rung 4 was.** Codegen was cut because it supplies inputs
/// and the drift it was tested against was in a derived figure. That verdict
/// stands and this does not reverse it: nothing is generated, the device keeps
/// its hand-written constants, and a derived quantity is skipped rather than
/// guessed at. What this adds is that the inputs can no longer disagree
/// silently, which is an hour of work for the cheap part of the same benefit.
///
/// Three things it deliberately declines to do, each because the alternative is
/// a false positive, and a lint that cries wolf is a lint somebody turns off.
///
/// - **A constant naming several designators is not checked.** `R145_R146` is a
///   series sum the device has already folded; comparing it against either part
///   would report a disagreement that is not one. They are counted and named
///   instead, so the gap in the coverage is visible rather than assumed away.
/// - **A constant whose value is an expression is not checked**, for the same
///   reason one step up: `shot_pitch_r` is two resistors in parallel and is not
///   any part's value.
/// - **Units are assumed to be SI on both sides.** The transcription guarantees
///   it and this device happens to honor it. A device that held kilohms would
///   make every row of this report at once, which is a loud failure rather than
///   a quiet one, and that is the property worth having.
fn value_disagrees(netlist: &Netlist, device: &DeviceParts, findings: &mut Vec<Finding>) {
    let mut compared = 0;
    for part in &netlist.parts {
        let Some(sheet) = part.value.quantity() else {
            continue;
        };
        // A run drawn as one symbol shares its value, so a constant naming any
        // designator in the run is a claim about this number.
        for designator in std::iter::once(&part.designator).chain(part.also.iter()) {
            let Some(constant) = device.values.get(designator) else {
                continue;
            };
            compared += 1;
            if (constant.value - sheet).abs() <= sheet.abs() * 1e-6 {
                continue;
            }
            // The ratio is the useful number: a factor of ten is a decimal
            // point somebody read differently, and a few percent is a
            // different part.
            let ratio = if sheet == 0.0 {
                f64::INFINITY
            } else {
                constant.value / sheet
            };
            findings.push(Finding {
                lint: "value-disagrees",
                severity: Severity::Problem,
                subject: designator.clone(),
                detail: format!(
                    "the sheet says {} and `{}` holds {}, a factor of {ratio:.4}. One of the \
                     two has been changed without the other",
                    part.value.label(),
                    constant.name,
                    constant.value,
                ),
            });
        }
    }
    // Always, even at zero. A silent check and a passing check read the same
    // in a report, and this one is cheap enough to be believed for the wrong
    // reason: it compares only where both sides name the same part, so a
    // transcription that is an excerpt has far fewer rows than the device has
    // constants.
    findings.push(Finding {
        lint: "value-disagrees",
        severity: Severity::Observation,
        subject: "(coverage)".to_string(),
        detail: format!(
            "{compared} values on this sheet were held against a device constant, out of {} \
             constants carrying a literal",
            device.values.len()
        ),
    });
}

/// A net with fewer than two endpoints connects nothing to nothing.
///
/// A port is exempt: it leaves the excerpt, so one endpoint inside is the
/// whole point of it. A net the transcription has annotated still reports,
/// downgraded, because the board really does have a stage that drives a
/// follower and nothing else and the next reader should be told that rather
/// than find a silent gap.
fn floating_nets(netlist: &Netlist, findings: &mut Vec<Finding>) {
    for net in &netlist.nets {
        if net.port.is_some() || net.on.len() >= 2 {
            continue;
        }
        let (severity, detail) = match &net.note {
            Some(note) => (
                Severity::Explained,
                format!(
                    "{} endpoint(s) and not a port, which the transcription explains: {note}",
                    net.on.len()
                ),
            ),
            None => (
                Severity::Problem,
                format!(
                    "{} endpoint(s) and not a port. A net has to reach two things, or leave \
                     the excerpt as a port, or say why not.",
                    net.on.len()
                ),
            ),
        };
        findings.push(Finding {
            lint: "floating-net",
            severity,
            subject: format!("net `{}`", net.name),
            detail,
        });
    }
}

/// Pins the transcription admits it has not read. Not a defect, but the whole
/// value of writing them down is that something says them out loud.
fn unread_pins(netlist: &Netlist, findings: &mut Vec<Finding>) {
    for part in &netlist.parts {
        for unread in &part.unread {
            findings.push(Finding {
                lint: "unread-pin",
                severity: Severity::Problem,
                subject: format!("{}.{}", part.designator, unread.pin),
                detail: format!("drawn but not read: {}", unread.why),
            });
        }
    }
}

/// Places the transcription deliberately contradicts the drawing.
///
/// The highest-risk lines in any transcription, and the ones most likely to be
/// "corrected" back by a later reader who rediscovers the contradiction and
/// not the argument. There should be very few, and every one should be worth
/// reading before trusting the file.
fn departures(netlist: &Netlist, findings: &mut Vec<Finding>) {
    for part in &netlist.parts {
        let Some(drawn) = &part.drawing_says else {
            continue;
        };
        findings.push(Finding {
            lint: "departs-from-drawing",
            severity: Severity::Explained,
            subject: part.designator.clone(),
            detail: match &part.note {
                Some(note) => format!("the drawing says {drawn}. {}", first_sentence(note)),
                None => format!("the drawing says {drawn}"),
            },
        });
    }
}

/// Part numbers that were reasoned about rather than read.
///
/// Decision 7 asks for the line between "we genuinely do not know" and "we did
/// not check" to be visible, and this is the first place it bites: every
/// op-amp on these sheets is an unlabeled 14-pin quad, and the output swing
/// that sets the shot's whole pitch range is a property of the part class
/// somebody picked. That is a good inference resting on nothing printed, and a
/// reader deciding whether to trust a rate needs to see it.
fn inferred_devices(netlist: &Netlist, findings: &mut Vec<Finding>) {
    for part in &netlist.parts {
        if !part.device_inferred {
            continue;
        }
        findings.push(Finding {
            lint: "inferred-device",
            severity: Severity::Observation,
            subject: part.designator.clone(),
            detail: format!(
                "{} is not printed on the drawing; it is inferred from the package",
                part.value.label()
            ),
        });
    }
}

/// **The one that matters.** A part on the sheet that no device constant
/// names.
///
/// This is the question six passes over this board could not ask, and `C94`,
/// 680 pF on the pin the drawing labels `RO`, is what it cost: present on
/// every `MB4391` channel, in none of the prose, the drawing or the device,
/// and found only by cropping that region of the sheet for an unrelated
/// reason.
///
/// Every row is a question rather than a defect. A gap here is either a part
/// the device should model, or an approximation someone decided was fine, and
/// this cannot tell which. Saying that the gap exists is the entire job.
fn not_modeled(netlist: &Netlist, device: &DeviceParts, findings: &mut Vec<Finding>) {
    // A device that names no part at all is a different statement from a
    // device that misses ten, and the rows below cannot tell them apart: both
    // come out as a list. Lunar Lander is the case that forced this. Its
    // device holds eight constants, none named for a part, because it was
    // built from a reference emulator's measured levels rather than from the
    // drawing. Every row below is then true and the headline is that the
    // device does not model this board's parts at all.
    if device.named.is_empty() {
        findings.push(Finding {
            lint: "not-modeled",
            severity: Severity::Observation,
            subject: "(all)".to_string(),
            detail: "the device names no designator in any constant, so it does not model \
                     this board part by part at all. Every row below follows from that one \
                     fact rather than being a separate gap"
                .to_string(),
        });
    }
    for part in &netlist.parts {
        if !carries_a_quantity(part) {
            continue;
        }
        // A run drawn as one symbol is still a run of parts, so ask about
        // every designator it carries and report the whole run in one line.
        let unmodeled: Vec<&String> = std::iter::once(&part.designator)
            .chain(part.also.iter())
            .filter(|designator| !device.named.contains(*designator))
            .collect();
        if unmodeled.is_empty() {
            continue;
        }
        let what = if part.also.is_empty() {
            format!("{} is on the sheet", part.label())
        } else {
            format!(
                "{} and {} more like it are on the sheet ({} of the run unmodeled)",
                part.label(),
                part.also.len(),
                unmodeled.len()
            )
        };
        let detail = match &part.note {
            Some(note) => format!(
                "{what} and no device constant names them. The transcription says: {}",
                first_sentence(note)
            ),
            None => format!("{what} and no device constant names them"),
        };
        findings.push(Finding {
            lint: "not-modeled",
            severity: Severity::Observation,
            subject: part.designator.clone(),
            detail,
        });
    }
}

/// A designator the device names that is not on this sheet: a constant with no
/// part behind it.
///
/// Only meaningful over a whole board. Against an excerpt it would report
/// every part of every other voice, so it does not run there and says so.
fn no_part(netlist: &Netlist, device: &DeviceParts, findings: &mut Vec<Finding>) {
    if netlist.board.excerpt.is_some() {
        findings.push(Finding {
            lint: "no-part",
            severity: Severity::Observation,
            subject: "(skipped)".to_string(),
            detail: "this transcription is an excerpt, so a designator the device names and \
                     this file lacks is expected rather than a finding. Run it against a \
                     whole-board netlist."
                .to_string(),
        });
        return;
    }
    let on_sheet: BTreeSet<&str> = netlist
        .parts
        .iter()
        .flat_map(|part| {
            std::iter::once(part.designator.as_str()).chain(part.also.iter().map(String::as_str))
        })
        .collect();
    for designator in &device.named {
        if !on_sheet.contains(designator.as_str()) {
            findings.push(Finding {
                lint: "no-part",
                severity: Severity::Problem,
                subject: designator.clone(),
                detail: "a device constant names this and no part on the sheet has that \
                         designator"
                    .to_string(),
            });
        }
    }
}

/// The first sentence of a note, for a one-line report.
fn first_sentence(note: &str) -> String {
    let flat = note.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.find(". ") {
        Some(end) => flat[..=end].trim().to_string(),
        None => flat,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netlist::Netlist;

    fn findings_for(text: &str, device: Option<&str>) -> Vec<Finding> {
        let netlist = Netlist::parse(text).expect("should load");
        let parts = device.map(DeviceParts::from_source);
        run(&netlist, parts.as_ref())
    }

    #[test]
    fn a_designator_is_letters_then_digits() {
        for yes in ["R156", "C94", "U19", "D11", "Q7", "R203"] {
            assert!(is_designator(yes), "{yes}");
        }
        for yes in ["VR1", "RP2", "PC1"] {
            assert!(is_designator(yes), "{yes}");
        }
        for no in ["OPAMP", "SWING", "CANNON", "REF", "R", "123", "Rx1", "HZ"] {
            assert!(!is_designator(no), "{no}");
        }
        // The six the first whole-board run of `no-part` reported. Three are
        // part numbers and three are rail voltages, and every one of them is
        // "letters then digits".
        for no in ["MB4391", "MM5837", "K74123", "V5", "V6", "V12"] {
            assert!(!is_designator(no), "{no} is not a designator");
        }
    }

    #[test]
    fn a_compound_constant_name_contributes_every_part_in_it() {
        let device = DeviceParts::from_source(
            "const R145_R146: f64 = 1_270_000.0;\n\
             const U12_CANNON_REF: f64 = 6.0;\n\
             const OPAMP_SWING: f64 = 10.4;\n",
        );
        assert!(device.named.contains("R145"));
        assert!(device.named.contains("R146"));
        assert!(device.named.contains("U12"));
        assert!(!device.named.contains("CANNON"));
        assert!(!device.named.contains("SWING"));
    }

    /// A two-part netlist and a device, wired enough to load, for the value
    /// checks below.
    fn one_resistor(ohms: &str) -> String {
        format!(
            "[board]\nname = \"t\"\n\n\
             [[parts]]\nref = \"R156\"\nkind = \"R\"\n{ohms}\n\n\
             [[nets]]\nname = \"a\"\nport = \"input\"\non = [\"R156.a\"]\n\n\
             [[nets]]\nname = \"b\"\nport = \"output\"\non = [\"R156.b\"]\n"
        )
    }

    fn disagreements(netlist: &str, device: &str) -> Vec<Finding> {
        findings_for(netlist, Some(device))
            .into_iter()
            .filter(|f| f.lint == "value-disagrees" && f.severity == Severity::Problem)
            .collect()
    }

    /// The drift class at the value level: the sheet was re-read and corrected
    /// and the device kept the old number, or the reverse. Nothing held these
    /// two together before.
    #[test]
    fn a_value_the_device_and_the_sheet_disagree_about_is_reported() {
        let found = disagreements(&one_resistor("kohms = 33"), "const R156: f64 = 47_000.0;\n");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].subject, "R156");
        assert!(found[0].detail.contains("1.4242"), "{}", found[0].detail);
    }

    /// The same number written the way each file writes it. `kohms = 33` and
    /// `33_000.0` are the same resistance and neither is a string anything
    /// parses, which is decision 2 paying off on both sides at once.
    #[test]
    fn the_same_value_in_different_units_does_not_report() {
        assert!(
            disagreements(&one_resistor("kohms = 33"), "const R156: f64 = 33_000.0;\n").is_empty()
        );
        assert!(
            disagreements(&one_resistor("ohms = 33000"), "const R156: f64 = 33e3;\n").is_empty()
        );
    }

    /// The decimal-point case, which is the one this board actually had to
    /// settle by cropping a label at 600 percent. A factor of ten between the
    /// two files is exactly what this has to catch.
    #[test]
    fn a_decimal_point_read_differently_shows_up_as_a_factor_of_ten() {
        let netlist = "[board]\nname = \"t\"\n\n\
             [[parts]]\nref = \"C88\"\nkind = \"C\"\nuf = 0.047\n\n\
             [[nets]]\nname = \"a\"\nport = \"input\"\non = [\"C88.a\"]\n\n\
             [[nets]]\nname = \"b\"\nport = \"output\"\non = [\"C88.b\"]\n";
        let found = disagreements(netlist, "const C88: f64 = 0.47e-6;\n");
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].detail.contains("10.0000"), "{}", found[0].detail);
    }

    /// A derived constant is not any part's value, so it is skipped rather
    /// than guessed at. `shot_pitch_r` is two resistors in parallel, and a
    /// check that compared it against `R147` would report a disagreement that
    /// is not one, which is how a lint gets turned off.
    #[test]
    fn a_constant_whose_value_is_an_expression_is_not_compared() {
        let device = "const R156: f64 = 33_000.0;\n\
                      const R156_SHARE: f64 = R156 / (R156 + R159);\n";
        let parts = DeviceParts::from_source(device);
        assert!(parts.values.contains_key("R156"));
        assert_eq!(parts.values["R156"].value, 33_000.0);
        assert!(disagreements(&one_resistor("kohms = 33"), device).is_empty());
    }

    /// A constant naming two parts is a pair the device has already folded, so
    /// comparing it against either one would be nonsense. It is counted and
    /// named instead, so that the hole in the coverage is visible.
    #[test]
    fn a_constant_naming_several_parts_is_counted_rather_than_compared() {
        let device = "const R145_R146: f64 = 1_270_000.0;\n";
        let parts = DeviceParts::from_source(device);
        assert!(parts.values.is_empty(), "{:?}", parts.values);
        assert_eq!(parts.compound, vec!["R145_R146"]);

        let netlist = "[board]\nname = \"t\"\n\n\
             [[parts]]\nref = \"R145\"\nkind = \"R\"\nkohms = 270\n\n\
             [[parts]]\nref = \"R146\"\nkind = \"R\"\nmohms = 1\n\n\
             [[nets]]\nname = \"a\"\nport = \"input\"\non = [\"R145.a\", \"R146.a\"]\n\n\
             [[nets]]\nname = \"b\"\nport = \"output\"\non = [\"R145.b\", \"R146.b\"]\n";
        assert!(disagreements(netlist, device).is_empty());
    }

    /// Coverage prints whether or not anything disagreed, because a check that
    /// compared nothing and a check that found nothing read the same way.
    #[test]
    fn coverage_is_reported_even_when_everything_agrees() {
        let findings = findings_for(
            &one_resistor("kohms = 33"),
            Some("const R156: f64 = 33_000.0;\n"),
        );
        let coverage = findings
            .iter()
            .find(|f| f.lint == "value-disagrees" && f.subject == "(coverage)")
            .expect("coverage always reports");
        assert!(
            coverage.detail.starts_with("1 value"),
            "{}",
            coverage.detail
        );
    }

    /// The result that decides the epic's kill criterion. `C94` is named by no
    /// constant, and the only place the device mentions it is a doc comment
    /// saying it is absent. A check that read comments would call that
    /// coverage and report nothing.
    #[test]
    fn a_part_mentioned_only_in_a_comment_is_still_not_modeled() {
        let device = "/// `C94` on `U16` ch A is the shot's. None of them is here.\n\
                      const R156: f64 = 33_000.0;\n";
        let netlist = "[board]\nname = \"t\"\n\n\
             [[parts]]\nref = \"R156\"\nkind = \"R\"\nkohms = 33\n\n\
             [[parts]]\nref = \"C94\"\nkind = \"C\"\npf = 680\n\n\
             [[nets]]\nname = \"a\"\nport = \"input\"\non = [\"R156.a\", \"C94.a\"]\n\n\
             [[nets]]\nname = \"b\"\nport = \"output\"\non = [\"R156.b\", \"C94.b\"]\n";
        let findings = findings_for(netlist, Some(device));
        let flagged: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.lint == "not-modeled")
            .collect();
        assert_eq!(flagged.len(), 1, "{flagged:?}");
        assert_eq!(flagged[0].subject, "C94");
    }

    /// An IC has no value for a constant to be, so its absence says nothing
    /// about whether the device models it.
    /// A board's bypass capacitors are drawn once and labeled with the whole
    /// run. They are still separate parts, so the query has to ask about all
    /// of them, and it has to say so in one line rather than sixty.
    #[test]
    fn a_run_drawn_as_one_symbol_is_asked_about_as_a_run() {
        let netlist = "[board]\nname = \"t\"\n\n\
             [[parts]]\nref = \"C139\"\nkind = \"C\"\nuf = 0.047\n\
             also = [\"C141\", \"C142\", \"C143\"]\n\n\
             [[nets]]\nname = \"a\"\nport = \"input\"\non = [\"C139.a\"]\n\n\
             [[nets]]\nname = \"b\"\nport = \"output\"\non = [\"C139.b\"]\n";
        let findings = findings_for(netlist, Some("const C142: f64 = 1.0;\n"));
        let flagged: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.lint == "not-modeled")
            .collect();
        assert_eq!(flagged.len(), 1, "one line, not four");
        assert!(
            flagged[0].detail.contains("3 more like it"),
            "{:?}",
            flagged[0]
        );
        assert!(
            flagged[0].detail.contains("3 of the run unmodeled"),
            "C142 is named by a constant, so only three of the four are: {:?}",
            flagged[0]
        );
    }

    /// The other direction: a designator riding along on a shared symbol still
    /// counts as present, so the device naming it is not a missing part.
    #[test]
    fn a_designator_in_a_run_counts_as_being_on_the_sheet() {
        let netlist = "[board]\nname = \"t\"\n\n\
             [[parts]]\nref = \"C139\"\nkind = \"C\"\nuf = 0.047\n\
             also = [\"C141\"]\n\n\
             [[nets]]\nname = \"a\"\nport = \"input\"\non = [\"C139.a\"]\n\n\
             [[nets]]\nname = \"b\"\nport = \"output\"\non = [\"C139.b\"]\n";
        let findings = findings_for(netlist, Some("const C141: f64 = 1.0;\n"));
        assert!(
            !findings.iter().any(|f| f.lint == "no-part"),
            "{findings:?}"
        );
    }

    #[test]
    fn a_part_with_no_quantity_is_not_reported_as_unmodeled() {
        let netlist = "[board]\nname = \"t\"\n\n\
             [[parts]]\nref = \"U19\"\nkind = \"U\"\ndevice = \"LM324\"\npins = [\"1\"]\n\n\
             [[parts]]\nref = \"Q7\"\nkind = \"Q\"\ndevice = \"C1684\"\n\n\
             [[nets]]\nname = \"a\"\nport = \"input\"\non = [\"U19.1\", \"Q7.b\"]\n\n\
             [[nets]]\nname = \"b\"\nport = \"output\"\non = [\"Q7.c\", \"Q7.e\"]\n";
        let findings = findings_for(netlist, Some("const R1: f64 = 1.0;\n"));
        assert!(
            !findings.iter().any(|f| f.lint == "not-modeled"),
            "{findings:?}"
        );
    }

    #[test]
    fn a_net_reaching_one_thing_is_a_problem() {
        let netlist = "[board]\nname = \"t\"\n\n\
             [[parts]]\nref = \"R1\"\nkind = \"R\"\nohms = 1\n\n\
             [[nets]]\nname = \"lonely\"\non = [\"R1.a\"]\n\n\
             [[nets]]\nname = \"b\"\nport = \"output\"\non = [\"R1.b\"]\n";
        let findings = findings_for(netlist, None);
        let floating: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.lint == "floating-net")
            .collect();
        assert_eq!(floating.len(), 1);
        assert_eq!(floating[0].severity, Severity::Problem);
    }

    /// The battleship's slow oscillator drives a follower and nothing else,
    /// traced three times. That is a finding about the board, so the
    /// annotation downgrades it and keeps printing it rather than hiding it.
    #[test]
    fn an_annotated_dead_end_is_downgraded_and_still_printed() {
        let netlist = "[board]\nname = \"t\"\n\n\
             [[parts]]\nref = \"R1\"\nkind = \"R\"\nohms = 1\n\n\
             [[nets]]\nname = \"lonely\"\non = [\"R1.a\"]\n\
             note = \"drives a follower and nothing else, traced three times\"\n\n\
             [[nets]]\nname = \"b\"\nport = \"output\"\non = [\"R1.b\"]\n";
        let findings = findings_for(netlist, None);
        let floating: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.lint == "floating-net")
            .collect();
        assert_eq!(floating.len(), 1, "still reported");
        assert_eq!(floating[0].severity, Severity::Explained);
        assert!(floating[0].detail.contains("traced three times"));
    }

    #[test]
    fn a_port_with_one_endpoint_is_not_floating() {
        let netlist = "[board]\nname = \"t\"\n\n\
             [[parts]]\nref = \"R1\"\nkind = \"R\"\nohms = 1\n\n\
             [[nets]]\nname = \"in\"\nport = \"input\"\non = [\"R1.a\"]\n\n\
             [[nets]]\nname = \"out\"\nport = \"output\"\non = [\"R1.b\"]\n";
        let findings = findings_for(netlist, None);
        assert!(
            !findings.iter().any(|f| f.lint == "floating-net"),
            "{findings:?}"
        );
    }

    /// Over an excerpt the check would report every part of every other voice,
    /// so it declines and says why rather than producing noise.
    #[test]
    fn the_no_part_check_declines_to_run_against_an_excerpt() {
        let netlist = "[board]\nname = \"t\"\nexcerpt = \"one voice\"\n\n\
             [[parts]]\nref = \"R1\"\nkind = \"R\"\nohms = 1\n\n\
             [[nets]]\nname = \"a\"\nport = \"input\"\non = [\"R1.a\"]\n\n\
             [[nets]]\nname = \"b\"\nport = \"output\"\non = [\"R1.b\"]\n";
        let findings = findings_for(netlist, Some("const R99: f64 = 1.0;\n"));
        let no_part: Vec<&Finding> = findings.iter().filter(|f| f.lint == "no-part").collect();
        assert_eq!(no_part.len(), 1);
        assert_eq!(no_part[0].subject, "(skipped)");
    }

    #[test]
    fn a_constant_with_no_part_behind_it_is_a_problem_on_a_whole_board() {
        let netlist = "[board]\nname = \"t\"\n\n\
             [[parts]]\nref = \"R1\"\nkind = \"R\"\nohms = 1\n\n\
             [[nets]]\nname = \"a\"\nport = \"input\"\non = [\"R1.a\"]\n\n\
             [[nets]]\nname = \"b\"\nport = \"output\"\non = [\"R1.b\"]\n";
        let findings = findings_for(netlist, Some("const R99: f64 = 1.0;\n"));
        let no_part: Vec<&Finding> = findings.iter().filter(|f| f.lint == "no-part").collect();
        assert_eq!(no_part.len(), 1);
        assert_eq!(no_part[0].subject, "R99");
        assert_eq!(no_part[0].severity, Severity::Problem);
    }

    /// A part number that was reasoned about has to look different from one
    /// that was read, or the swing limits a voice's pitch rests on look like
    /// facts off the drawing.
    #[test]
    fn an_inferred_part_number_is_reported_as_an_inference() {
        let netlist = "[board]\nname = \"t\"\n\n\
             [[parts]]\nref = \"U19\"\nkind = \"U\"\ndevice = \"LM324\"\n\
             device_inferred = true\npins = [\"1\", \"2\"]\n\n\
             [[parts]]\nref = \"U18\"\nkind = \"U\"\ndevice = \"555\"\npins = [\"3\"]\n\n\
             [[nets]]\nname = \"a\"\nport = \"input\"\non = [\"U19.1\", \"U18.3\"]\n\n\
             [[nets]]\nname = \"b\"\nport = \"output\"\non = [\"U19.2\"]\n";
        let findings = findings_for(netlist, None);
        let inferred: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.lint == "inferred-device")
            .collect();
        assert_eq!(inferred.len(), 1, "only the unlabeled one");
        assert_eq!(inferred[0].subject, "U19");
    }

    /// Contradicting the drawing is the one thing a transcription must always
    /// justify, so the reason travels with the finding.
    #[test]
    fn a_departure_from_the_drawing_is_reported_with_its_reason() {
        let netlist = "[board]\nname = \"t\"\n\n\
             [[parts]]\nref = \"U9\"\nkind = \"U\"\ndevice = \"LM324\"\n\
             device_inferred = true\npins = [\"12\", \"13\", \"14\"]\n\
             drawing_says = \"the fourth section's output is labeled pin 11\"\n\
             note = \"Pin 11 is the negative supply on a 14-pin quad and cannot drive.\"\n\n\
             [[nets]]\nname = \"a\"\nport = \"input\"\non = [\"U9.12\", \"U9.13\"]\n\n\
             [[nets]]\nname = \"b\"\nport = \"output\"\non = [\"U9.14\"]\n";
        let findings = findings_for(netlist, None);
        let departures: Vec<&Finding> = findings
            .iter()
            .filter(|f| f.lint == "departs-from-drawing")
            .collect();
        assert_eq!(departures.len(), 1);
        assert!(
            departures[0].detail.contains("pin 11"),
            "{:?}",
            departures[0]
        );
        assert!(
            departures[0].detail.contains("negative supply"),
            "the reason travels with it: {:?}",
            departures[0]
        );
    }

    #[test]
    fn an_unread_pin_is_reported_as_outstanding_work() {
        let netlist = "[board]\nname = \"t\"\n\n\
             [[parts]]\nref = \"Q7\"\nkind = \"Q\"\ndevice = \"C1684\"\n\
             unread = [{ pin = \"e\", why = \"the emitter return is off the crop\" }]\n\n\
             [[nets]]\nname = \"a\"\nport = \"input\"\non = [\"Q7.b\"]\n\n\
             [[nets]]\nname = \"b\"\nport = \"output\"\non = [\"Q7.c\"]\n";
        let findings = findings_for(netlist, None);
        let unread: Vec<&Finding> = findings.iter().filter(|f| f.lint == "unread-pin").collect();
        assert_eq!(unread.len(), 1);
        assert_eq!(unread[0].subject, "Q7.e");
    }
}
