//! The coverage catalog: every discrete or board-level analog audio path in the
//! project, with a status for each.
//!
//! The catalog is data (`targets.toml`) rather than code because its job is to
//! be *reviewed*. A list generated from the adapters that exist can only ever
//! describe what is already modelled, which is exactly the set that needs no
//! plan; the paths worth tracking are the ones with no adapter, and nothing in
//! the source knows about those.
//!
//! What keeps it honest is the tests below rather than the file itself. A
//! catalog nobody checks drifts from the tree within one commit, and a stale
//! inventory is worse than none: it answers the question wrongly instead of
//! sending you to look.

use serde::Deserialize;

const SCHEMA: u32 = 1;

/// The catalog as parsed. Embedded at compile time so `sndcmp targets` works
/// from any directory and the tests cannot pick up a different copy.
const SOURCE: &str = include_str!("../targets.toml");

/// How far along one audio path is.
///
/// The ordering matters: [`Status::Validated`] is the only one that claims
/// somebody checked the model against the drawing, and
/// `a_validated_target_keeps_its_evidence` holds it to that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    /// Nobody has checked what this board's analog audio path is.
    ///
    /// THE OTHER SIX STATUSES ARE ABOUT MODELLING; THIS ONE IS ABOUT REVIEW,
    /// and they are different axes. A board where a shared coupling stage is
    /// applied and no drawing has ever been read is *technically* `partial`,
    /// but calling it that claims a review that did not happen, and a status
    /// handed out on a guess is the failure this whole file exists to prevent.
    ///
    /// So an entry starts here and leaves only when someone reads the board's
    /// drawing. `notes` is required and must say what the code does today, so
    /// the row carries the evidence for its own claim rather than an adjective.
    Unexamined,
    /// The board has an analog path and we model none of it.
    Missing,
    /// Some of the path is modelled.
    Partial,
    /// A device exists but nothing has ever compared it against a reference.
    ImplementedUnvalidated,
    /// Compared against a reference and the residuals recorded, but not signed
    /// off against the schematic.
    ImplementedNeedsValidation,
    /// Compared and schematic-reviewed.
    Validated,
    /// Deliberately not being worked on. Requires `reason`.
    Blocked,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Unexamined => "unexamined",
            Status::Missing => "missing",
            Status::Partial => "partial",
            Status::ImplementedUnvalidated => "implemented-unvalidated",
            Status::ImplementedNeedsValidation => "implemented-needs-validation",
            Status::Validated => "validated",
            Status::Blocked => "blocked",
        }
    }
}

/// One audio path.
#[derive(Debug, Clone, Deserialize)]
pub struct Entry {
    /// Stable id for the path. Not the same thing as `adapter`: a path can be
    /// catalogued long before anything can drive it.
    pub id: String,
    /// Registry names of the machines this path belongs to.
    pub machines: Vec<String>,
    /// The device implementing it, if one exists.
    #[serde(default)]
    pub device: Option<String>,
    /// The `sndcmp` target id, where an adapter is registered.
    #[serde(default)]
    pub adapter: Option<String>,
    pub status: Status,
    /// Scenario ids that must exist.
    #[serde(default)]
    pub scenarios: Vec<String>,
    #[serde(default)]
    pub notes: Option<String>,
    /// Required when `status` is `blocked`.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Catalog {
    pub schema: u32,
    #[serde(default, rename = "target")]
    pub targets: Vec<Entry>,
}

impl Catalog {
    /// Parse the catalog compiled into this binary.
    pub fn load() -> Result<Self, String> {
        let c: Catalog =
            toml::from_str(SOURCE).map_err(|e| format!("parsing targets.toml: {e}"))?;
        if c.schema != SCHEMA {
            return Err(format!(
                "targets.toml schema {} but this build understands {SCHEMA}",
                c.schema
            ));
        }
        Ok(c)
    }

    pub fn find(&self, id: &str) -> Option<&Entry> {
        self.targets.iter().find(|t| t.id == id)
    }
}

/// Render the catalog as the project-wide plan.
pub fn render(catalog: &Catalog) -> String {
    let mut out = String::new();
    let width = catalog
        .targets
        .iter()
        .map(|t| t.id.len())
        .max()
        .unwrap_or(0)
        .max(2);

    out.push_str("Discrete and board-level analog audio paths.\n\n");
    for t in &catalog.targets {
        let adapter = match &t.adapter {
            Some(_) => "adapter",
            None => "-      ",
        };
        let scenarios = if t.scenarios.is_empty() {
            "no scenarios".to_string()
        } else {
            format!("{} scenario(s)", t.scenarios.len())
        };
        out.push_str(&format!(
            "  {:width$}  {:<28}  {adapter}  {scenarios}\n",
            t.id,
            t.status.as_str(),
            width = width
        ));
        out.push_str(&format!(
            "  {:width$}  {}\n",
            "",
            t.machines.join(", "),
            width = width
        ));
    }

    let mut counts: Vec<(&str, usize)> = Vec::new();
    for s in [
        Status::Unexamined,
        Status::Missing,
        Status::Partial,
        Status::ImplementedUnvalidated,
        Status::ImplementedNeedsValidation,
        Status::Validated,
        Status::Blocked,
    ] {
        let n = catalog.targets.iter().filter(|t| t.status == s).count();
        if n > 0 {
            counts.push((s.as_str(), n));
        }
    }
    out.push_str("\n  ");
    out.push_str(
        &counts
            .iter()
            .map(|(s, n)| format!("{n} {s}"))
            .collect::<Vec<_>>()
            .join(", "),
    );
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    fn catalog() -> Catalog {
        Catalog::load().expect("targets.toml parses")
    }

    fn scenario_root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scenarios")
    }

    #[test]
    fn the_catalog_parses_and_is_not_empty() {
        let c = catalog();
        assert!(
            c.targets.len() > 5,
            "the catalog has {} entries, which is fewer than the project has \
             discrete devices; it has probably lost its contents",
            c.targets.len()
        );
    }

    #[test]
    fn ids_are_unique() {
        let c = catalog();
        let mut seen = BTreeSet::new();
        for t in &c.targets {
            assert!(seen.insert(t.id.clone()), "duplicate catalog id {:?}", t.id);
        }
    }

    /// Every scenario a catalog entry claims must exist on disk.
    ///
    /// The failure this catches is a rename: moving or deleting a scenario
    /// leaves the catalog asserting coverage that is no longer there, which is
    /// the specific way an inventory becomes a liability.
    #[test]
    fn every_catalogued_scenario_exists() {
        let c = catalog();
        for t in &c.targets {
            for id in &t.scenarios {
                let path = scenario_root().join(format!("{id}.toml"));
                assert!(
                    path.exists(),
                    "{} lists scenario {id:?}, but {} does not exist",
                    t.id,
                    path.display()
                );
            }
        }
    }

    /// Every registered adapter must appear in the catalog.
    ///
    /// This is the direction that actually prevents drift: adding a target to
    /// `src/targets/` without cataloguing it is how a path gets modelled and
    /// then forgotten about at the plan level.
    #[test]
    fn every_registered_adapter_is_catalogued() {
        let c = catalog();
        let catalogued: BTreeSet<&str> = c
            .targets
            .iter()
            .filter_map(|t| t.adapter.as_deref())
            .collect();
        for spec in crate::target::all() {
            assert!(
                catalogued.contains(spec.id),
                "sndcmp target {:?} is registered but not in targets.toml",
                spec.id
            );
        }
    }

    /// And every adapter a catalog entry names must be registered.
    #[test]
    fn every_catalogued_adapter_is_registered() {
        let c = catalog();
        for t in &c.targets {
            if let Some(a) = &t.adapter {
                assert!(
                    crate::target::find(a).is_some(),
                    "{} names adapter {a:?}, which is not registered",
                    t.id
                );
            }
        }
    }

    /// A scenario's own target must be the adapter its catalog entry claims.
    ///
    /// Without this the two halves can disagree silently: an entry can list a
    /// scenario that drives a different board entirely, and the coverage it
    /// reports is then for something else.
    #[test]
    fn catalogued_scenarios_drive_the_entrys_adapter() {
        let c = catalog();
        for t in &c.targets {
            let Some(adapter) = &t.adapter else {
                assert!(
                    t.scenarios.is_empty(),
                    "{} lists scenarios but no adapter to run them",
                    t.id
                );
                continue;
            };
            for id in &t.scenarios {
                let path = scenario_root().join(format!("{id}.toml"));
                let sc = crate::scenario::Scenario::load(&path)
                    .unwrap_or_else(|e| panic!("{}: loading {id}: {e:?}", t.id));
                assert_eq!(
                    &sc.target, adapter,
                    "{} lists {id}, but that scenario drives {:?}",
                    t.id, sc.target
                );
            }
        }
    }

    /// Statuses have to mean something, so each one carries the evidence its
    /// name claims.
    #[test]
    fn a_status_carries_the_evidence_it_claims() {
        let c = catalog();
        for t in &c.targets {
            match t.status {
                Status::Blocked => {
                    assert!(t.reason.is_some(), "{} is blocked without a reason", t.id)
                }
                // `unexamined` is the one status that claims NOTHING has been
                // reviewed, so the thing it must carry is a description of the
                // unreviewed state. Without that it is an empty row that makes
                // the machine look accounted for.
                //
                // It must also not claim a comparison. An adapter or a scenario
                // means somebody drove this path, which is incompatible with
                // nobody having looked at it, and would let a row sit here
                // while quietly holding evidence it should have been promoted
                // on.
                Status::Unexamined => {
                    assert!(
                        t.notes.is_some(),
                        "{} is unexamined without notes; say what the code does today",
                        t.id
                    );
                    assert!(
                        t.adapter.is_none() && t.scenarios.is_empty(),
                        "{} is unexamined but has an adapter or scenarios, which means \
                         somebody did look at it",
                        t.id
                    );
                }
                Status::Missing => assert!(
                    t.device.is_none(),
                    "{} is `missing` but names a device, {:?}",
                    t.id,
                    t.device
                ),
                // Anything claiming an implementation must say where it is.
                Status::ImplementedUnvalidated
                | Status::ImplementedNeedsValidation
                | Status::Validated => {
                    assert!(
                        t.device.is_some(),
                        "{} claims an implementation but names no device",
                        t.id
                    );
                }
                Status::Partial => {}
            }
        }
    }

    /// `validated` and `implemented-needs-validation` both claim a comparison
    /// happened, and a comparison needs something to have been compared.
    ///
    /// This is the check that stops the status ladder being climbed by editing
    /// one word. Promoting an entry means adding scenarios, which is a visible
    /// change to the data rather than to an adjective.
    #[test]
    fn a_validated_target_keeps_its_evidence() {
        let c = catalog();
        for t in &c.targets {
            if matches!(
                t.status,
                Status::Validated | Status::ImplementedNeedsValidation
            ) {
                assert!(
                    t.adapter.is_some() && !t.scenarios.is_empty(),
                    "{} claims {:?} but has {} scenario(s) and {} adapter; a \
                     comparison needs both",
                    t.id,
                    t.status.as_str(),
                    t.scenarios.len(),
                    if t.adapter.is_some() { "an" } else { "no" }
                );
            }
        }
    }

    /// Every machine named must be one the registry actually has.
    ///
    /// Catches a renamed machine, and catches a typo that would otherwise make
    /// a path look covered while naming nothing.
    #[test]
    fn every_named_machine_is_registered() {
        let known: BTreeSet<&str> = phosphor_machines::registry::all()
            .iter()
            .map(|m| m.name)
            .collect();
        for t in &catalog().targets {
            for m in &t.machines {
                assert!(
                    known.contains(m.as_str()),
                    "{} names machine {m:?}, which is not in the registry",
                    t.id
                );
            }
        }
    }

    /// And the direction that makes the catalog an INVENTORY rather than a
    /// list of things somebody happened to look at.
    ///
    /// `every_named_machine_is_registered` above stops the catalog naming a
    /// machine that does not exist. It cannot stop the far more likely failure,
    /// which is a machine nobody has ever examined: the sound chip is emulated,
    /// its tests pass, the board around it is a wire, and there is no row saying
    /// so because nobody wrote one. That gap is invisible by construction.
    ///
    /// So every registered machine must appear in some entry, and a machine
    /// nobody has looked at gets an `unexamined` row saying what the code does
    /// today. That is a reviewed claim; an absent row is not, and the two are
    /// indistinguishable without this check.
    ///
    /// WHAT THIS CHECK DOES NOT CATCH: a machine can appear in one entry while a
    /// DIFFERENT path on the same board goes unrecorded. Galaga and Xevious were
    /// exactly that case, catalogued for their 54XX explosion circuit while
    /// nothing said anything about their output stage, and they satisfied this
    /// test throughout. Per-machine presence is the weakest useful invariant,
    /// not a completeness proof.
    #[test]
    fn every_registered_machine_is_catalogued() {
        let c = catalog();
        let catalogued: BTreeSet<&str> = c
            .targets
            .iter()
            .flat_map(|t| t.machines.iter().map(String::as_str))
            .collect();
        let missing: Vec<&str> = phosphor_machines::registry::all()
            .iter()
            .map(|m| m.name)
            .filter(|n| !catalogued.contains(n))
            .collect();
        assert!(
            missing.is_empty(),
            "{} registered machine(s) appear in no catalog entry, so nobody has \
             recorded whether they have an analog audio path:\n  {}",
            missing.len(),
            missing.join("\n  ")
        );
    }

    #[test]
    fn it_renders() {
        let text = render(&catalog());
        assert!(text.contains("atari-system1-audio"), "{text}");
        assert!(text.contains("missing"), "{text}");
    }
}
