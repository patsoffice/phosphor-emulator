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
use std::collections::BTreeSet;

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
/// A stopgap, and the design says so: rung 4 generates the constants from the
/// netlist, and the set of designators they carry becomes a by-product of that
/// rather than something extracted here. Until then the device's own constant
/// *names* are the declaration, which is weaker than a manifest in one way and
/// stronger in another: it cannot go stale, because it is the code.
///
/// Constant names only, never comments. On this board that distinction is the
/// whole result: `zaxxon_sound.rs` mentions `C94` in a doc comment, in a
/// sentence saying it is *not* modeled. A check that read comments would count
/// that as coverage and report nothing.
#[derive(Debug, Clone, Default)]
pub struct DeviceParts {
    /// The designators the device's constant names carry.
    pub named: BTreeSet<String>,
}

impl DeviceParts {
    /// Read a device source file and collect the designators its constants
    /// name. `const R156` contributes `R156`; `const R145_R146` contributes
    /// both, which is how this device writes a pair it has already summed.
    pub fn from_source(source: &str) -> DeviceParts {
        let mut named = BTreeSet::new();
        for line in source.lines() {
            let line = line.trim_start();
            let Some(rest) = line.strip_prefix("const ") else {
                continue;
            };
            let Some(name) = rest.split(':').next() else {
                continue;
            };
            for token in name.split('_') {
                if is_designator(token) {
                    named.insert(token.to_string());
                }
            }
        }
        DeviceParts { named }
    }
}

/// `R156`, `C94`, `U19`: one to three letters then digits, which is how every
/// designator on these sheets is written. `OPAMP` and `SWING` are not, and
/// neither is `CANNON`, so a compound constant name contributes only the parts
/// of it that are parts.
fn is_designator(token: &str) -> bool {
    let letters = token.chars().take_while(|c| c.is_ascii_uppercase()).count();
    let digits = token.len() - letters;
    (1..=3).contains(&letters) && digits >= 1 && token[letters..].chars().all(|c| c.is_ascii_digit())
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
    if let Some(device) = device {
        not_modeled(netlist, device, &mut findings);
        no_part(netlist, device, &mut findings);
    }
    findings
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
    for part in &netlist.parts {
        if !carries_a_quantity(part) || device.named.contains(&part.designator) {
            continue;
        }
        let detail = match &part.note {
            Some(note) => format!(
                "{} is on the sheet and no device constant names it. The transcription says: {}",
                part.label(),
                first_sentence(note)
            ),
            None => format!(
                "{} is on the sheet and no device constant names it",
                part.label()
            ),
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
    for designator in &device.named {
        if netlist.part(designator).is_none() {
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
        for no in ["OPAMP", "SWING", "CANNON", "REF", "R", "123", "Rx1", "HZ"] {
            assert!(!is_designator(no), "{no}");
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
