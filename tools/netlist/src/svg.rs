//! Generate netlistsvg input from a transcription.
//!
//! This is what keeps the netlist from becoming a fourth copy of the board.
//! The `docs/schematics/*.json` files were hand-built beside the prose and the
//! Rust constants; from here they are build products, and `render.sh` turns
//! them into the SVGs the documents embed.
//!
//! Everything in this module is presentation. A drawing has to decide things a
//! netlist does not know: which end of a resistor is upstream, which parts
//! belong in one box, and where a quad op-amp's four sections go. Those come
//! from the `block`, `section` and `out` annotations, which is decision 4:
//! grouping is how the picture reads, not a fact about the board. Nothing here
//! can change what the netlist says is connected to what.

use crate::netlist::{Kind, Net, Netlist, Part};
use serde_json::{Map, Value as Json, json};
use std::collections::BTreeMap;

/// One box in the generated drawing: a block of parts, one section of a
/// multi-section part, or a part on its own.
struct Cell<'a> {
    /// The key in the JSON. Invisible in the render, but it orders the file.
    id: String,
    /// The label netlistsvg prints on the box.
    label: String,
    /// The part pins this box owns.
    members: Vec<(&'a Part, Vec<String>)>,
}

impl<'a> Cell<'a> {
    /// Whether this box owns a given pin.
    fn owns(&self, designator: &str, pin: &str) -> bool {
        self.members
            .iter()
            .any(|(part, pins)| part.designator == designator && pins.iter().any(|p| p == pin))
    }

    /// The pins of this box that sit on a net, with the part that draws each.
    fn pins_on(&self, net: &Net) -> Vec<(&'a Part, String)> {
        let mut found = Vec::new();
        for (part, pins) in &self.members {
            for endpoint in &net.on {
                if endpoint.part == part.designator && pins.contains(&endpoint.pin) {
                    found.push((*part, endpoint.pin.clone()));
                }
            }
        }
        found
    }
}

/// Build the netlistsvg JSON for a transcription.
pub fn render(netlist: &Netlist) -> Json {
    let cells = build_cells(netlist);
    let bits = assign_bits(netlist);

    let mut ports = Map::new();
    for net in &netlist.nets {
        let Some(direction) = net.port else { continue };
        // A rail is drawn as a stub per connection rather than as a node, so
        // it has no single bit to hang a module port on.
        if net.rail {
            continue;
        }
        ports.insert(
            net.name.clone(),
            json!({ "direction": direction.as_str(), "bits": [bits[&net.name].clone()] }),
        );
    }

    let mut json_cells = Map::new();
    for cell in &cells {
        let mut directions = Map::new();
        let mut connections = Map::new();
        for net in &netlist.nets {
            let pins = cell.pins_on(net);
            if pins.is_empty() {
                continue;
            }
            // A net with every endpoint inside this box is internal to it: two
            // resistors drawn in series as one block have a junction between
            // them, and drawing it as a port would put a stub on the box for a
            // wire that never leaves.
            //
            // A port is the exception and not a special case: it leaves the
            // excerpt by definition, so it leaves the box however few
            // endpoints it has inside. Without this an op-amp section whose
            // three pins all go to the excerpt boundary draws with no pins at
            // all.
            let interior = net.port.is_none()
                && !net.rail
                && net.on.iter().all(|e| cell.owns(&e.part, &e.pin));
            if interior {
                continue;
            }
            let name = port_label(net, &pins);
            directions.insert(name.clone(), json!(direction_of(cell, net, netlist)));
            connections.insert(name, json!([bits[&net.name].clone()]));
        }
        // A box with no stubs is a part none of whose pins have been placed
        // yet. There is nothing to draw and a floating rectangle would read as
        // a part that connects to nothing, which is a different claim. It
        // stays in the netlist, where `netlist show` counts it.
        if connections.is_empty() {
            continue;
        }
        json_cells.insert(
            cell.id.clone(),
            json!({
                "type": cell.label,
                "port_directions": Json::Object(directions),
                "connections": Json::Object(connections),
                "attributes": {},
            }),
        );
    }

    let title = match &netlist.board.excerpt {
        Some(excerpt) => format!("{}: {excerpt}", netlist.board.name),
        None => netlist.board.name.clone(),
    };
    json!({
        "modules": {
            title: {
                "ports": Json::Object(ports),
                "cells": Json::Object(json_cells),
            }
        }
    })
}

/// Decide the boxes. A part's `section` annotations split it into one box per
/// section, which is how a quad op-amp is drawn and how the prose already
/// writes it (`U19`(5,6,7) rather than a fourteen-pin rectangle). Otherwise a
/// `block` groups parts into one box, and a part with neither is its own.
fn build_cells(netlist: &Netlist) -> Vec<Cell<'_>> {
    let mut cells: Vec<Cell<'_>> = Vec::new();
    let mut blocks: BTreeMap<&str, usize> = BTreeMap::new();

    for part in &netlist.parts {
        if !part.sections.is_empty() {
            let mut claimed: Vec<String> = Vec::new();
            for section in &part.sections {
                cells.push(Cell {
                    id: format!("{}{}", part.designator.to_lowercase(), section.name),
                    label: section_label(part, &section.name, section.role.as_deref()),
                    members: vec![(part, section.pins.clone())],
                });
                claimed.extend(section.pins.iter().cloned());
            }
            // Pins outside every section belong to the package rather than to
            // a section: supply pins, mostly. They get a box of their own so a
            // rail stays visible rather than being hidden inside a section it
            // does not belong to.
            let leftover: Vec<String> = part
                .pins
                .iter()
                .filter(|pin| !claimed.contains(pin))
                .filter(|pin| !part.nc.iter().any(|nc| &&nc.pin == pin))
                .cloned()
                .collect();
            if !leftover.is_empty() {
                cells.push(Cell {
                    id: part.designator.to_lowercase(),
                    label: part.label(),
                    members: vec![(part, leftover)],
                });
            }
            continue;
        }

        let pins: Vec<String> = part
            .pins
            .iter()
            .filter(|pin| !part.nc.iter().any(|nc| &&nc.pin == pin))
            .cloned()
            .collect();

        match part.block.as_deref() {
            Some(block) => match blocks.get(block) {
                Some(&index) => {
                    let cell: &mut Cell<'_> = &mut cells[index];
                    cell.label = format!("{} / {}", cell.label, part.label());
                    cell.members.push((part, pins));
                }
                None => {
                    blocks.insert(block, cells.len());
                    cells.push(Cell {
                        id: slug(block),
                        label: part.label(),
                        members: vec![(part, pins)],
                    });
                }
            },
            None => cells.push(Cell {
                id: part.designator.to_lowercase(),
                label: part.label(),
                members: vec![(part, pins)],
            }),
        }
    }
    cells
}

/// `U19b integrator`, or `U19b LM324` where the section has no stated role.
///
/// The role displaces the part number rather than joining it. Both would be
/// more informative and neither fits: `render.sh` pads the viewBox by a fixed
/// six pixels per character and a long label on the leftmost box loses its
/// start. What a section *does* is the thing a reader of this drawing needs,
/// and the part number is one line away in the parts table.
fn section_label(part: &Part, section: &str, role: Option<&str>) -> String {
    match (role, part.value.label().as_str()) {
        (Some(role), _) => format!("{}{section} {role}", part.designator),
        (None, "") => format!("{}{section}", part.designator),
        (None, device) => format!("{}{section} {device}", part.designator),
    }
}

/// Which side of the box a port goes on.
///
/// netlistsvg puts inputs left and outputs right and lets ELK flow the graph
/// between them, so this decides how readable the drawing is and nothing else.
///
/// A pin the transcription declares in `out` drives its net, and that is the
/// reading. Where nothing is declared, which is every passive, the fallback is
/// pin order: a two-terminal part's `a` end is its upstream end. That is the
/// one convention the format asks a transcriber to hold to, and it is why
/// `pins` is ordered rather than a set.
fn direction_of(cell: &Cell<'_>, net: &Net, netlist: &Netlist) -> &'static str {
    let mine = cell.pins_on(net);
    if mine
        .iter()
        .any(|(part, pin)| part.outputs.iter().any(|o| o == pin))
    {
        return "output";
    }
    // A rail feeds the box; nothing drives a supply.
    if net.rail {
        return "input";
    }
    let driven_elsewhere = net.on.iter().any(|endpoint| {
        netlist
            .part(&endpoint.part)
            .is_some_and(|part| part.outputs.contains(&endpoint.pin))
    });
    if driven_elsewhere {
        return "input";
    }
    if mine
        .iter()
        .all(|(part, pin)| part.pins.first().is_some_and(|first| first == pin))
    {
        "input"
    } else {
        "output"
    }
}

/// The text on the stub. The net name always, plus the pin numbers where the
/// part has pins with identities: on an IC the pin number is the whole reason
/// the drawing exists, and on a resistor `a` and `b` are noise.
fn port_label(net: &Net, pins: &[(&Part, String)]) -> String {
    let identified: Vec<String> = pins
        .iter()
        .filter(|(part, _)| matches!(part.kind, Kind::U | Kind::Q | Kind::Pot | Kind::J))
        .map(|(_, pin)| pin.clone())
        .collect();
    if identified.is_empty() {
        net.name.clone()
    } else {
        format!("{} ({})", net.name, identified.join(","))
    }
}

/// Net names to netlistsvg bits.
///
/// A rail becomes a constant rather than a node. netlistsvg draws a constant
/// as its own little source next to whatever uses it, so `+12V` appears as a
/// stub on each part that touches it instead of as one node with thirty wires
/// converging on it. That is the difference between a drawing and a hairball,
/// and it is how the hand-built files did it too.
fn assign_bits(netlist: &Netlist) -> BTreeMap<String, Json> {
    let mut bits = BTreeMap::new();
    let mut next = 2;
    for net in &netlist.nets {
        let bit = if net.rail {
            let ground = net.name.eq_ignore_ascii_case("gnd")
                || net.name.eq_ignore_ascii_case("agnd")
                || net.name.eq_ignore_ascii_case("ground");
            json!(if ground { "0" } else { "1" })
        } else {
            let bit = json!(next);
            next += 1;
            bit
        };
        bits.insert(net.name.clone(), bit);
    }
    bits
}

/// A block name into something usable as a JSON key.
fn slug(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::netlist::Netlist;

    fn module(json: &Json) -> &Map<String, Json> {
        json["modules"]
            .as_object()
            .unwrap()
            .values()
            .next()
            .unwrap()
            .as_object()
            .unwrap()
    }

    fn cells(json: &Json) -> &Map<String, Json> {
        module(json)["cells"].as_object().unwrap()
    }

    const SERIES: &str = r#"
[board]
name = "t"

[[parts]]
ref = "R145"
kind = "R"
kohms = 270
block = "divider"

[[parts]]
ref = "R146"
kind = "R"
mohms = 1
block = "divider"

[[parts]]
ref = "C88"
kind = "C"
uf = 0.047

[[nets]]
name = "+12V"
rail = true
on = ["R145.a"]

[[nets]]
name = "R145/R146 junction"
on = ["R145.b", "R146.a"]

[[nets]]
name = "node X"
port = "output"
on = ["R146.b", "C88.b"]

[[nets]]
name = "shaper"
port = "input"
on = ["C88.a"]
"#;

    #[test]
    fn a_block_becomes_one_box_labeled_with_its_parts() {
        let netlist = Netlist::parse(SERIES).unwrap();
        let json = render(&netlist);
        assert_eq!(
            cells(&json)["divider"]["type"],
            json!("R145 270k / R146 1M")
        );
    }

    /// Two resistors drawn in series as one block have a junction between
    /// them. It is a real net and it is not a port on that box.
    #[test]
    fn a_net_internal_to_a_block_is_not_drawn_as_a_port() {
        let netlist = Netlist::parse(SERIES).unwrap();
        let json = render(&netlist);
        let ports = cells(&json)["divider"]["connections"].as_object().unwrap();
        assert!(!ports.keys().any(|k| k.contains("junction")), "{ports:?}");
        assert_eq!(ports.len(), 2, "{ports:?}");
    }

    #[test]
    fn a_rail_is_a_constant_rather_than_a_node() {
        let netlist = Netlist::parse(SERIES).unwrap();
        let json = render(&netlist);
        let connections = cells(&json)["divider"]["connections"].as_object().unwrap();
        assert_eq!(connections["+12V"], json!(["1"]));
    }

    #[test]
    fn ground_is_a_constant_too_and_a_different_one() {
        let text = "[board]\nname = \"t\"\n\n[[parts]]\nref = \"R1\"\nkind = \"R\"\nohms = 820\n\
                    \n[[nets]]\nname = \"node A\"\nport = \"input\"\non = [\"R1.a\"]\n\
                    \n[[nets]]\nname = \"GND\"\nrail = true\non = [\"R1.b\"]\n";
        let netlist = Netlist::parse(text).unwrap();
        let json = render(&netlist);
        assert_eq!(cells(&json)["r1"]["connections"]["GND"], json!(["0"]));
    }

    #[test]
    fn a_port_net_reaches_the_module_boundary() {
        let netlist = Netlist::parse(SERIES).unwrap();
        let json = render(&netlist);
        let ports = module(&json)["ports"].as_object().unwrap();
        assert_eq!(ports["node X"]["direction"], json!("output"));
        assert_eq!(ports["shaper"]["direction"], json!("input"));
    }

    /// The `a` end of a two-terminal part is its upstream end, which is the
    /// one convention the format asks a transcriber to hold to.
    #[test]
    fn pin_order_decides_flow_for_a_passive() {
        let netlist = Netlist::parse(SERIES).unwrap();
        let json = render(&netlist);
        let directions = cells(&json)["c88"]["port_directions"].as_object().unwrap();
        assert_eq!(directions["shaper"], json!("input"));
        assert_eq!(directions["node X"], json!("output"));
    }

    const QUAD: &str = r#"
[board]
name = "t"

[[parts]]
ref = "U19"
kind = "U"
device = "LM324"
pins = ["1", "2", "3", "5", "6", "7"]
out = ["1", "7"]

[[parts.sections]]
name = "a"
pins = ["1", "2", "3"]
role = "x-5.89"

[[parts.sections]]
name = "b"
pins = ["5", "6", "7"]
role = "integrator"

[[parts]]
ref = "R150"
kind = "R"
kohms = 33

[[nets]]
name = "+6V"
rail = true
on = ["U19.3"]

[[nets]]
name = "U19a in"
port = "input"
on = ["U19.2", "R150.b"]

[[nets]]
name = "U19a out"
on = ["U19.1", "R150.a"]

[[nets]]
name = "summing node"
port = "input"
on = ["U19.6"]

[[nets]]
name = "pin 5"
port = "input"
on = ["U19.5"]

[[nets]]
name = "U19b out"
port = "output"
on = ["U19.7"]
"#;

    /// A quad op-amp is drawn as its sections, which is how the board draws it
    /// and how the prose writes it. The netlist still holds one part with the
    /// real pin numbers on it: sections are presentation, like blocks.
    #[test]
    fn a_sectioned_part_is_drawn_once_per_section() {
        let netlist = Netlist::parse(QUAD).unwrap();
        let json = render(&netlist);
        let cells = cells(&json);
        assert!(cells.contains_key("u19a"), "{:?}", cells.keys());
        assert!(cells.contains_key("u19b"), "{:?}", cells.keys());
        assert_eq!(cells["u19b"]["type"], json!("U19b integrator"));
    }

    #[test]
    fn a_sections_pins_stay_on_the_section_that_draws_them() {
        let netlist = Netlist::parse(QUAD).unwrap();
        let json = render(&netlist);
        let a = cells(&json)["u19a"]["connections"].as_object().unwrap();
        let b = cells(&json)["u19b"]["connections"].as_object().unwrap();
        assert!(a.keys().any(|k| k.starts_with("+6V")), "{a:?}");
        assert!(!b.keys().any(|k| k.starts_with("+6V")), "{b:?}");
        assert!(b.keys().any(|k| k.starts_with("summing node")), "{b:?}");
    }

    /// An IC's pin number is the whole reason the drawing exists, so it goes
    /// on the stub. A resistor's `a` and `b` are noise and do not.
    #[test]
    fn an_ic_stub_carries_its_pin_number_and_a_passive_stub_does_not() {
        let netlist = Netlist::parse(QUAD).unwrap();
        let json = render(&netlist);
        let u19a = cells(&json)["u19a"]["connections"].as_object().unwrap();
        assert!(
            u19a.contains_key("summing node (6)") || u19a.contains_key("U19a in (2)"),
            "{u19a:?}"
        );
        let r150 = cells(&json)["r150"]["connections"].as_object().unwrap();
        assert!(r150.contains_key("U19a out"), "{r150:?}");
    }

    /// A declared output drives its net, so every other box on that net
    /// listens, whatever their pin order says.
    #[test]
    fn a_declared_output_makes_every_other_box_on_the_net_an_input() {
        let netlist = Netlist::parse(QUAD).unwrap();
        let json = render(&netlist);
        let u19a = cells(&json)["u19a"]["port_directions"].as_object().unwrap();
        assert_eq!(u19a["U19a out (1)"], json!("output"));
        let r150 = cells(&json)["r150"]["port_directions"].as_object().unwrap();
        assert_eq!(r150["U19a out"], json!("input"));
    }

    #[test]
    fn every_cell_has_matching_direction_and_connection_keys() {
        for text in [SERIES, QUAD] {
            let netlist = Netlist::parse(text).unwrap();
            let json = render(&netlist);
            for (id, cell) in cells(&json) {
                let directions = cell["port_directions"].as_object().unwrap();
                let connections = cell["connections"].as_object().unwrap();
                assert_eq!(
                    directions.keys().collect::<Vec<_>>(),
                    connections.keys().collect::<Vec<_>>(),
                    "{id}"
                );
            }
        }
    }
}
