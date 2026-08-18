//! Emit a MAME-playable Lua script from a recorded input movie.
//!
//! # Why not `.inp`
//!
//! MAME's own input recording is port-bit-level and only replays reliably
//! against the exact build that wrote it, so a committed conversion would rot on
//! every MAME upgrade. A Lua script driving named `ioport` fields has neither
//! problem, and is readable in review.
//!
//! # Why this needs almost no per-machine knowledge
//!
//! Verified against MAME 0.287: port *tags* vary wildly between drivers
//! (`:IN0`, `:P1`, `:SYSTEM`, `:DSW1`, `:1820`, `:F60000`) but field *names* are
//! conventional — `P1 Up`, `P1 Button 1`, `Coin 1`, `1 Player Start` — across
//! every driver checked. So the emitted script resolves a field by name across
//! all ports and ignores the tag, and the name itself is derived from our own
//! [`InputKind`] plus player number rather than a hand-written table per
//! machine. All 15 of Burger Time's controls map this way.
//!
//! # Digital only, deliberately
//!
//! Analog cannot transfer, and not for want of encoding effort: Marble
//! Madness's MAME fields are `Trackball X`/`Trackball Y` with `is_analog` set
//! and mask 255 — an *absolute* 8-bit port value — while a movie carries the
//! stream of relative deltas from which our `RelativeCounter` derives that
//! value. Those are different models. Analog records are reported and skipped
//! rather than approximated.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use phosphor_core::core::machine::{ActionRole, Direction, InputControl, InputKind};
use phosphor_harness::movie::{Movie, MovieRecord};

/// Controls whose MAME field name the generic rule gets wrong.
///
/// Keyed by our `stable_name`. Small by construction — the rule below covers
/// directions, primary buttons, coins, starts and service, which is nearly
/// everything. `tilt` is the motivating case: it is
/// `Action(ActionRole::Secondary)` with no player, so the rule would render it
/// `P1 Button 2` where MAME has a literal `Tilt` field.
const OVERRIDES: &[(&str, &str)] = &[
    ("tilt", "Tilt"),
    ("service", "Service 1"),
    ("service1", "Service 1"),
    ("test", "Service Mode"),
];

/// The slot number trailing a stable name — `coin2` -> 2, `coin` -> 1.
///
/// Coin slots and start buttons are numbered by *slot*, not by player, and our
/// tables reflect that: burgertime declares both `coin1` and `coin2` with
/// `player: None`, because a coin slot does not belong to a player. Reading the
/// number off the name is therefore more faithful than reading `player`, which
/// mapped both slots onto MAME's `Coin 1`.
fn slot_suffix(stable_name: &str) -> Option<u8> {
    let tail: String = stable_name
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect();
    tail.chars().rev().collect::<String>().parse().ok()
}

/// The MAME `ioport` field name a control corresponds to, or `None` when it
/// cannot be expressed — which today means analog, and any bespoke button whose
/// meaning is machine-specific.
pub fn mame_field(c: &InputControl) -> Option<String> {
    if let Some((_, field)) = OVERRIDES.iter().find(|(name, _)| *name == c.stable_name) {
        return Some((*field).to_string());
    }
    let player = c.player.unwrap_or(1);
    Some(match c.kind {
        InputKind::DigitalDirection { direction } => {
            let d = match direction {
                Direction::Up => "Up",
                Direction::Down => "Down",
                Direction::Left => "Left",
                Direction::Right => "Right",
            };
            format!("P{player} {d}")
        }
        InputKind::Action(role) => {
            let n = match role {
                ActionRole::Primary => 1,
                ActionRole::Secondary => 2,
                ActionRole::Tertiary => 3,
            };
            format!("P{player} Button {n}")
        }
        // Numbered by slot, not player — see `slot_suffix`.
        InputKind::Coin => format!("Coin {}", slot_suffix(c.stable_name).unwrap_or(1)),
        InputKind::Start => {
            let n = slot_suffix(c.stable_name).unwrap_or(player);
            if n == 1 {
                "1 Player Start".to_string()
            } else {
                format!("{n} Players Start")
            }
        }
        InputKind::Service => "Service 1".to_string(),
        // A bespoke button's meaning is machine-specific, so guessing a MAME
        // field for it would be worse than saying it is unmapped.
        InputKind::Button => return None,
        InputKind::AnalogAxis { .. } => return None,
    })
}

/// What a conversion could and could not carry, so the caller can report it
/// rather than let the difference pass silently.
pub struct Conversion {
    pub lua: String,
    /// Controls the movie touched that have no MAME field.
    pub unmapped: Vec<String>,
    /// Analog records dropped — always a faithful-replay caveat, never a detail.
    pub analog_dropped: usize,
    /// Digital edges written into the script.
    pub edges: usize,
}

fn lua_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Convert a movie into a MAME autoboot script.
///
/// `driver` is MAME's name for the game, which is not ours — our `burgertime`
/// is MAME's `btime`. The registry's first ROM-set name is that value, and the
/// caller passes it in.
pub fn to_lua(
    movie: &Movie,
    controls: &[InputControl],
    driver: &str,
    snap_at: Option<u32>,
) -> Conversion {
    // Movie control index -> MAME field name, resolved once.
    let mut field_for: Vec<Option<String>> = Vec::with_capacity(movie.header.controls.len());
    let mut unmapped = Vec::new();
    for name in &movie.header.controls {
        match controls.iter().find(|c| c.stable_name == *name) {
            Some(c) => {
                let f = mame_field(c);
                if f.is_none() {
                    unmapped.push(name.clone());
                }
                field_for.push(f);
            }
            None => {
                unmapped.push(name.clone());
                field_for.push(None);
            }
        }
    }

    // frame -> [(field, value)], in record order.
    let mut plan: BTreeMap<u32, Vec<(String, u8)>> = BTreeMap::new();
    let mut analog_dropped = 0usize;
    let mut edges = 0usize;
    for r in &movie.records {
        match r {
            MovieRecord::Button {
                frame,
                ctl,
                pressed,
            } => {
                if let Some(Some(field)) = field_for.get(usize::from(*ctl)) {
                    plan.entry(*frame)
                        .or_default()
                        .push((field.clone(), u8::from(*pressed)));
                    edges += 1;
                }
            }
            MovieRecord::Relative { .. } | MovieRecord::Absolute { .. } => analog_dropped += 1,
            // DIPs are set on MAME's command line or in its cfg, not per frame;
            // markers and release-all have no port-level equivalent.
            _ => {}
        }
    }

    let mut lua = String::new();
    let _ = writeln!(
        lua,
        "-- Generated by `disasm movie mame` from an input movie.\n\
         --\n\
         -- machine: {} (MAME driver: {driver})\n\
         -- frames:  {}\n\
         -- edges:   {edges} digital\n\
         --\n\
         -- Run:\n\
         --   mame {driver} -autoboot_script THIS_FILE \\\n\
         --        -video none -sound none -seconds_to_run <N> -nothrottle\n\
         --\n\
         -- Field names are resolved across every port by name, because port tags\n\
         -- differ between drivers while the names do not.",
        movie.header.machine, movie.header.frames,
    );
    if analog_dropped > 0 {
        let _ = writeln!(
            lua,
            "--\n\
             -- WARNING: {analog_dropped} analog record(s) were dropped. MAME's analog\n\
             -- ports carry an absolute value; a movie carries relative deltas. This\n\
             -- script therefore replays only the digital half of the session."
        );
    }
    if !unmapped.is_empty() {
        let _ = writeln!(lua, "-- NOTE: no MAME field for: {}", unmapped.join(", "));
    }

    lua.push_str("\nlocal plan = {\n");
    for (frame, acts) in &plan {
        let body: Vec<String> = acts
            .iter()
            .map(|(f, v)| format!("{{{}, {v}}}", lua_quote(f)))
            .collect();
        let _ = writeln!(lua, "  [{frame}] = {{{}}},", body.join(", "));
    }
    lua.push_str("}\n\n");

    lua.push_str(
        "-- Resolve every field once, by name, across all ports.\n\
         local fields = {}\n\
         for _, port in pairs(manager.machine.ioport.ports) do\n\
         \x20 for name, f in pairs(port.fields) do\n\
         \x20   if fields[name] == nil then fields[name] = f end\n\
         \x20 end\n\
         end\n\n\
         local missing = {}\n\
         for _, acts in pairs(plan) do\n\
         \x20 for _, a in ipairs(acts) do\n\
         \x20   if fields[a[1]] == nil then missing[a[1]] = true end\n\
         \x20 end\n\
         end\n\
         for name, _ in pairs(missing) do\n\
         \x20 print(\"movie: this driver has no field named \" .. name)\n\
         end\n\n\
         local frame = 0\n",
    );
    if let Some(at) = snap_at {
        let _ = writeln!(lua, "local snap_at = {at}");
    }
    // The subscription object must be kept alive: MAME unsubscribes when it is
    // collected, so registering without binding it silently does nothing — the
    // script runs, prints nothing, and the run looks like plain attract mode.
    lua.push_str(
        "movie_sub = emu.add_machine_frame_notifier(function ()\n\
         \x20 local acts = plan[frame]\n\
         \x20 if acts ~= nil then\n\
         \x20   for _, a in ipairs(acts) do\n\
         \x20     local f = fields[a[1]]\n\
         \x20     if f ~= nil then f:set_value(a[2]) end\n\
         \x20   end\n\
         \x20 end\n",
    );
    if snap_at.is_some() {
        lua.push_str(
            "\x20 if frame == snap_at then\n\
             \x20   manager.machine.video:snapshot()\n\
             \x20   print(\"movie: snapshot at frame \" .. frame)\n\
             \x20 end\n",
        );
    }
    lua.push_str("\x20 frame = frame + 1\nend)\n");

    Conversion {
        lua,
        unmapped,
        analog_dropped,
        edges,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phosphor_core::core::machine::InputId;

    fn ctl(stable: &'static str, kind: InputKind, player: Option<u8>) -> InputControl {
        InputControl {
            id: InputId(0),
            stable_name: stable,
            label: "",
            kind,
            player,
            default_bindings: &[],
        }
    }

    /// The names asserted here were read off MAME 0.287 itself, across dkong,
    /// btime, galaga and marble — they are the convention this design rests on.
    #[test]
    fn the_generic_rule_matches_mames_field_names() {
        let cases = [
            (
                ctl(
                    "p1_up",
                    InputKind::DigitalDirection {
                        direction: Direction::Up,
                    },
                    Some(1),
                ),
                "P1 Up",
            ),
            (
                ctl(
                    "p2_right",
                    InputKind::DigitalDirection {
                        direction: Direction::Right,
                    },
                    Some(2),
                ),
                "P2 Right",
            ),
            (
                ctl(
                    "p1_button1",
                    InputKind::Action(ActionRole::Primary),
                    Some(1),
                ),
                "P1 Button 1",
            ),
            // Both coin slots declare `player: None` in the real tables, which
            // is why the slot number comes from the name and not from `player`.
            (ctl("coin1", InputKind::Coin, None), "Coin 1"),
            (ctl("coin2", InputKind::Coin, None), "Coin 2"),
            (ctl("coin", InputKind::Coin, None), "Coin 1"),
            (ctl("start1", InputKind::Start, Some(1)), "1 Player Start"),
            (ctl("start2", InputKind::Start, Some(2)), "2 Players Start"),
            (ctl("start2", InputKind::Start, None), "2 Players Start"),
        ];
        for (c, expected) in cases {
            assert_eq!(
                mame_field(&c).as_deref(),
                Some(expected),
                "{}",
                c.stable_name
            );
        }
    }

    /// The override exists because the generic rule is wrong here, so pin both
    /// halves: what the rule would say, and what the override says instead.
    #[test]
    fn tilt_takes_the_override_not_the_action_rule() {
        let tilt = ctl("tilt", InputKind::Action(ActionRole::Secondary), None);
        assert_eq!(mame_field(&tilt).as_deref(), Some("Tilt"));
        // Without the override the rule would have produced this, which btime
        // has no field for.
        let same_kind_no_override =
            ctl("p1_shield", InputKind::Action(ActionRole::Secondary), None);
        assert_eq!(
            mame_field(&same_kind_no_override).as_deref(),
            Some("P1 Button 2")
        );
    }

    #[test]
    fn analog_and_bespoke_buttons_are_unmapped() {
        use phosphor_core::core::machine::AnalogAxisKind;
        let track = ctl(
            "p1_trackball_x",
            InputKind::AnalogAxis {
                axis: AnalogAxisKind::X,
            },
            Some(1),
        );
        assert_eq!(mame_field(&track), None);
        assert_eq!(mame_field(&ctl("weird", InputKind::Button, Some(1))), None);
    }
}
