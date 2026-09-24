//! Derived constants for a device, solved from a transcription and written out
//! as Rust.
//!
//! `phosphor-emulator-kfby.2`, the probe that followed rung 4's cut. Rung 4 was
//! value codegen and it was cut because values are inputs: the drift it was
//! tested against lived in a derived figure. This generates the derivations
//! instead. A device that writes `C88 * (R145_R146 || R147)` has made a
//! judgment about which resistor sits at an AC ground, and on Zaxxon's shot
//! that judgment was wrong twice. The solver makes no such judgment: it
//! solves the network the transcription holds.
//!
//! # What a spec says, and what it must not say
//!
//! A spec names a group, the drives a scenario needs, and for each constant
//! **which capacitor's mode** it wants and, for a share, which two nodes. It
//! never names a time constant, a resistor, or which parts are in a path. A
//! mode is found by where its energy is stored, so nothing in the spec can
//! steer the answer toward a number somebody already believed.
//!
//! A mode is only claimed for a capacitor that holds more than half of its
//! energy. Below that the network has no mode that belongs to that capacitor,
//! and a device writing it as one one-pole section would be an approximation
//! this cannot vouch for, so the generator refuses rather than picking the
//! nearest.
//!
//! # What it does not produce
//!
//! Anything that is not a linear passive network. An op-amp's output swing, a
//! 555's duty, a diode's clamp and the `MB4391`'s rolloff are part properties
//! or nonlinear behavior, and the solver opens every pin of every part that is
//! not an R, C or L. Those constants stay hand-written in the device.
//!
//! # Units
//!
//! Seconds and plain ratios, because that is what a device's filter chain
//! consumes. Not a resistance for a chosen capacitor: a mode is a property of
//! the network, and an "R" computed as `tau / C` would be a number no part on
//! the sheet has, handed to a builder that multiplies it straight back.

use crate::netlist::Netlist;
use crate::solve::{Mode, Network, Setup};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

/// A derive spec, as written on disk.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    /// The transcription, relative to the spec.
    pub netlist: String,
    /// The Rust file to write, relative to the spec.
    pub out: String,
    /// The device module the output belongs to, for the header.
    pub device: String,
    /// Each network to solve, and what to take from it.
    #[serde(default)]
    pub scenarios: Vec<Scenario>,
}

/// One solve: a group, the drives it needs, and the constants read out of it.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scenario {
    /// The parts' `group`.
    pub group: String,
    /// Nets held by parts the solver does not model, as in `netlist solve
    /// --drive`.
    #[serde(default)]
    pub drives: BTreeMap<String, f64>,
    /// Time constants.
    #[serde(default)]
    pub tau: Vec<Tau>,
    /// How far one node moves per volt of another, within one mode.
    #[serde(default)]
    pub share: Vec<Share>,
}

/// A constant that is one mode's time constant.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tau {
    /// The Rust constant.
    pub name: String,
    /// The capacitor whose mode it is.
    pub mode_of: String,
}

/// A constant that is one node's displacement per volt of another, in one
/// mode. This is the quantity a device writes as a divider ratio when it has
/// judged that the capacitor between them is open.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Share {
    /// The Rust constant.
    pub name: String,
    /// The capacitor whose mode it is.
    pub mode_of: String,
    /// The node that follows.
    pub node: String,
    /// The node it follows.
    pub per: String,
}

/// The output of a run: where it goes, and what it says.
#[derive(Debug, Clone)]
pub struct Generated {
    /// The file the spec names.
    pub out: PathBuf,
    /// Its contents.
    pub text: String,
}

/// The mode that stores most of its energy in `capacitor`, and how much.
fn mode_of<'a>(modes: &'a [Mode], capacitor: &str) -> Result<(&'a Mode, f64), String> {
    let stored = |mode: &Mode| {
        mode.energy
            .iter()
            .find(|(c, _)| c == capacitor)
            .map_or(0.0, |(_, e)| *e)
    };
    let mode = modes
        .iter()
        .max_by(|a, b| stored(a).total_cmp(&stored(b)))
        .ok_or_else(|| "the network has no modes".to_string())?;
    let share = stored(mode);
    if share == 0.0 {
        return Err(format!(
            "{capacitor} is not in the solved network; `netlist solve` says why"
        ));
    }
    if share <= 0.5 {
        return Err(format!(
            "{capacitor} holds at most {:.1} % of any mode's energy, so the network has no mode \
             that is its own, and a one-pole section keyed to it is not something this can vouch for",
            share * 100.0
        ));
    }
    Ok((mode, share))
}

/// A node's entry in a mode's shape.
fn at(mode: &Mode, net: &str) -> Result<f64, String> {
    mode.shape
        .iter()
        .find(|(name, _)| name == net)
        .map(|(_, v)| *v)
        .ok_or_else(|| format!("`{net}` is not a free node of the solved network"))
}

/// The scenario's drives, as the header and doc comments say them.
fn held(scenario: &Scenario) -> String {
    if scenario.drives.is_empty() {
        return "nothing driven".to_string();
    }
    scenario
        .drives
        .iter()
        .map(|(net, volts)| format!("`{net}` held at {volts} V"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A time constant for a trailing comment.
fn human(tau: f64) -> String {
    if tau >= 1.0 {
        format!("{tau:.4} s")
    } else {
        format!("{:.3} ms", tau * 1e3)
    }
}

/// Read a spec from disk.
pub fn load_spec(path: &Path) -> Result<Spec, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Load a spec and generate its file. Every problem is collected rather than
/// the first one returned, the way the loader does it.
pub fn generate(spec_path: &Path) -> Result<Generated, Vec<String>> {
    let spec = load_spec(spec_path).map_err(|e| vec![e])?;
    let base = spec_path.parent().unwrap_or(Path::new("."));
    let netlist = Netlist::load(&base.join(&spec.netlist)).map_err(|e| vec![e.to_string()])?;
    let text = render(&spec, spec_path, &netlist)?;
    Ok(Generated {
        out: base.join(&spec.out),
        text,
    })
}

/// Solve every scenario and write the Rust.
pub fn render(spec: &Spec, spec_path: &Path, netlist: &Netlist) -> Result<String, Vec<String>> {
    let mut errors = Vec::new();
    let mut out = String::new();
    // The file name only: the text must not depend on where it was run from,
    // or the check that it is current fails for a path spelled differently.
    let spec_name = spec_path.file_name().map_or_else(
        || spec_path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    writeln!(
        out,
        "//! Constants for `{}`, solved from `{}`
//! by `netlist derive` from `{spec_name}` beside it.
//!
//! **Generated. Do not edit.** Change the transcription or the spec and run
//! `netlist derive` on the spec again. A test in `tools/netlist` fails while
//! this file differs from what that writes.
//!
//! Each value comes from the whole passive network solved at once, not from a
//! judgment about which capacitor is a short or an open. The solver treats
//! every pin of a part that is not an R, C or L as open; `netlist solve` with
//! the same group and drives lists them.",
        spec.device, spec.netlist
    )
    .unwrap();

    for scenario in &spec.scenarios {
        let subset = netlist.subset(&scenario.group);
        if subset.parts.is_empty() {
            errors.push(format!("no parts in group `{}`", scenario.group));
            continue;
        }
        let setup = Setup {
            drives: scenario.drives.clone(),
            ..Setup::default()
        };
        let network = match Network::build(&subset, &setup) {
            Ok(network) => network,
            Err(problems) => {
                errors.extend(problems);
                continue;
            }
        };
        let modes = match network.modes() {
            Ok(modes) => modes,
            Err(problem) => {
                errors.push(problem);
                continue;
            }
        };
        let context = format!("group `{}`, {}", scenario.group, held(scenario));

        for tau in &scenario.tau {
            match mode_of(&modes, &tau.mode_of) {
                Ok((mode, energy)) => {
                    writeln!(
                        out,
                        "
/// The time constant of the mode that stores {:.1} % of its energy in
/// `{}`: {context}.
pub(super) const {}: f64 = {:.5e}; // {}",
                        energy * 100.0,
                        tau.mode_of,
                        tau.name,
                        mode.tau,
                        human(mode.tau)
                    )
                    .unwrap();
                }
                Err(e) => errors.push(format!("{}: {e}", tau.name)),
            }
        }

        for share in &scenario.share {
            let value = mode_of(&modes, &share.mode_of).and_then(|(mode, energy)| {
                let node = at(mode, &share.node)?;
                let per = at(mode, &share.per)?;
                if per.abs() < 1e-3 {
                    return Err(format!(
                        "`{}` does not move in this mode, so nothing is a share of it",
                        share.per
                    ));
                }
                Ok((node / per, energy))
            });
            match value {
                Ok((ratio, energy)) => {
                    writeln!(
                        out,
                        "
/// How far `{}` moves per volt of `{}`, in the mode that stores
/// {:.1} % of its energy in `{}`: {context}.
pub(super) const {}: f64 = {ratio:.5};",
                        share.node,
                        share.per,
                        energy * 100.0,
                        share.mode_of,
                        share.name,
                    )
                    .unwrap();
                }
                Err(e) => errors.push(format!("{}: {e}", share.name)),
            }
        }
    }

    if errors.is_empty() {
        Ok(out)
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two capacitors joined by a resistor, far apart in value, so each has a
    /// mode of its own and the generator can name both.
    const PAIR: &str = r#"
[board]
name = "t"

[[parts]]
ref = "R1"
kind = "R"
group = "g"
kohms = 10

[[parts]]
ref = "R2"
kind = "R"
group = "g"
kohms = 1000

[[parts]]
ref = "C1"
kind = "C"
group = "g"
uf = 1.0

[[parts]]
ref = "C2"
kind = "C"
group = "g"
uf = 100.0

[[nets]]
name = "+12V"
rail = true
on = ["R1.a"]

[[nets]]
name = "fast"
on = ["R1.b", "R2.a", "C1.a"]

[[nets]]
name = "slow"
on = ["R2.b", "C2.a"]

[[nets]]
name = "GND"
rail = true
on = ["C1.b", "C2.b"]
"#;

    fn spec(tau: &[(&str, &str)], share: &[(&str, &str, &str, &str)]) -> Spec {
        Spec {
            netlist: "t.toml".into(),
            out: "t.rs".into(),
            device: "t.rs".into(),
            scenarios: vec![Scenario {
                group: "g".into(),
                drives: BTreeMap::new(),
                tau: tau
                    .iter()
                    .map(|(name, c)| Tau {
                        name: (*name).into(),
                        mode_of: (*c).into(),
                    })
                    .collect(),
                share: share
                    .iter()
                    .map(|(name, c, node, per)| Share {
                        name: (*name).into(),
                        mode_of: (*c).into(),
                        node: (*node).into(),
                        per: (*per).into(),
                    })
                    .collect(),
            }],
        }
    }

    /// The fast capacitor's mode is `C1` against `R1` with `C2` a short, and
    /// the slow one is `C2` against `R1 + R2` with `C1` an open. Neither of
    /// those is typed anywhere here: the spec names the capacitors only.
    #[test]
    fn each_capacitor_names_its_own_mode() {
        let netlist = Netlist::parse(PAIR).unwrap();
        let text = render(
            &spec(&[("FAST", "C1"), ("SLOW", "C2")], &[]),
            Path::new("t.derive.toml"),
            &netlist,
        )
        .unwrap();
        let value = |name: &str| -> f64 {
            let line = text
                .lines()
                .find(|l| l.contains(&format!("const {name}:")))
                .unwrap();
            let rhs = line.split('=').nth(1).unwrap();
            rhs.split(';').next().unwrap().trim().parse().unwrap()
        };
        assert!((value("FAST") - 10e3 * 1e-6).abs() / 10e-3 < 0.02);
        assert!((value("SLOW") - 1010e3 * 100e-6).abs() / 101.0 < 0.02);
    }

    /// Asking for a node that does not move is refused, not divided by zero.
    #[test]
    fn a_share_of_a_node_that_does_not_move_is_refused() {
        let netlist = Netlist::parse(PAIR).unwrap();
        let errors = render(
            &spec(&[], &[("X", "C1", "fast", "+12V")]),
            Path::new("t.derive.toml"),
            &netlist,
        )
        .unwrap_err();
        assert!(errors[0].contains("not a free node"), "{errors:?}");
    }

    /// A capacitor that is not in the network is named as such.
    #[test]
    fn a_capacitor_the_network_lacks_is_refused() {
        let netlist = Netlist::parse(PAIR).unwrap();
        let errors = render(
            &spec(&[("T", "C9")], &[]),
            Path::new("t.derive.toml"),
            &netlist,
        )
        .unwrap_err();
        assert!(
            errors[0].contains("C9 is not in the solved network"),
            "{errors:?}"
        );
    }
}
