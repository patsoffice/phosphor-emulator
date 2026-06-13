use std::collections::HashMap;

use phosphor_core::core::machine::InputButton;
use sdl2::controller::{Axis, Button};
use sdl2::keyboard::Scancode;

/// Maps SDL scancodes to machine button IDs.
pub struct KeyMap {
    map: HashMap<Scancode, u8>,
}

impl KeyMap {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    /// Bind a scancode to a machine button ID.
    pub fn bind(&mut self, scancode: Scancode, button_id: u8) {
        self.map.insert(scancode, button_id);
    }

    /// Look up the machine button ID for a scancode.
    pub fn get(&self, scancode: Scancode) -> Option<u8> {
        self.map.get(&scancode).copied()
    }
}

/// Build a default key map for a machine's input buttons.
/// Uses name-based matching: common button names across machines
/// get consistent default bindings without game-specific knowledge.
pub fn default_key_map(buttons: &[InputButton]) -> KeyMap {
    let mut km = KeyMap::new();

    for button in buttons {
        // Exact name match first, then substring fallback for combo names
        // like "Jump L / P1 Start".
        let scancode = match button.name {
            // Player 1
            "P1 Left" => Some(Scancode::Left),
            "P1 Right" => Some(Scancode::Right),
            "P1 Up" => Some(Scancode::Up),
            "P1 Down" => Some(Scancode::Down),
            "P1 Flap" => Some(Scancode::LCtrl),
            "P1 Jump" => Some(Scancode::LShift),
            "P1 Fire" => Some(Scancode::Space),
            "P1 Start" => Some(Scancode::Num1),

            // Player 2
            "P2 Left" => Some(Scancode::A),
            "P2 Right" => Some(Scancode::D),
            "P2 Up" => Some(Scancode::W),
            "P2 Down" => Some(Scancode::S),
            "P2 Flap" => Some(Scancode::W),
            "P2 Jump" => Some(Scancode::E),
            "P2 Fire" => Some(Scancode::E),
            "P2 Start" => Some(Scancode::Num2),

            // Fire stick (Robotron twin-stick)
            "P1 Fire Up" => Some(Scancode::I),
            "P1 Fire Down" => Some(Scancode::K),
            "P1 Fire Left" => Some(Scancode::J),
            "P1 Fire Right" => Some(Scancode::L),

            // Missile Command fire buttons
            "Fire Left" => Some(Scancode::Z),
            "Fire Center" => Some(Scancode::X),
            "Fire Right" => Some(Scancode::C),

            // Food Fight
            "P1 Throw" => Some(Scancode::Space),
            "P2 Throw" => Some(Scancode::RShift),
            "Self-Test" => Some(Scancode::T),

            // System
            "Coin" => Some(Scancode::Num5),
            "Service" => Some(Scancode::Num6),

            // Substring fallback for combo-named buttons
            name if name.contains("P1 Start") => Some(Scancode::Num1),
            name if name.contains("P2 Start") => Some(Scancode::Num2),
            name if name.contains("Coin") => Some(Scancode::Num5),

            _ => None,
        };

        if let Some(sc) = scancode {
            km.bind(sc, button.id);
        }

        // Secondary bindings: combo buttons that serve dual roles (e.g. a jump
        // button that also acts as P1 Start) get both a system key and an
        // action key so players can use them during gameplay.
        if button.name.contains("Jump") {
            if button.name.contains(" L") {
                km.bind(Scancode::Space, button.id);
            } else if button.name.contains(" R") {
                km.bind(Scancode::LShift, button.id);
            }
        }
    }

    km
}

// ---------------------------------------------------------------------------
// Game controller mapping
// ---------------------------------------------------------------------------

/// Stick-to-digital axis mapping: when the stick passes the deadzone threshold,
/// the negative/positive button IDs are pressed.
struct StickMapping {
    neg_id: u8, // Button ID for negative axis (left/up)
    pos_id: u8, // Button ID for positive axis (right/down)
}

/// Maps SDL2 game controller buttons and axes to machine button IDs.
pub struct ControllerMap {
    buttons: HashMap<Button, u8>,
    left_x: Option<StickMapping>,
    left_y: Option<StickMapping>,
    right_x: Option<StickMapping>,
    right_y: Option<StickMapping>,
}

/// Deadzone threshold for analog sticks (±10000 of ±32768 range, ~30%).
const STICK_DEADZONE: i16 = 10_000;

impl ControllerMap {
    pub fn new() -> Self {
        Self {
            buttons: HashMap::new(),
            left_x: None,
            left_y: None,
            right_x: None,
            right_y: None,
        }
    }

    /// Bind a controller button to a machine button ID.
    pub fn bind_button(&mut self, button: Button, button_id: u8) {
        self.buttons.insert(button, button_id);
    }

    /// Look up the machine button ID for a controller button.
    pub fn get_button(&self, button: Button) -> Option<u8> {
        self.buttons.get(&button).copied()
    }

    /// Convert an axis value (-32768..32767) to digital press/release events.
    /// Returns up to 2 (button_id, pressed) pairs to pass to machine.set_input().
    pub fn axis_to_digital(&self, axis: Axis, value: i16) -> ArrayVec<(u8, bool)> {
        let mapping = match axis {
            Axis::LeftX => self.left_x.as_ref(),
            Axis::LeftY => self.left_y.as_ref(),
            Axis::RightX => self.right_x.as_ref(),
            Axis::RightY => self.right_y.as_ref(),
            _ => None,
        };

        let mut events = ArrayVec::new();
        if let Some(m) = mapping {
            events.push((m.neg_id, value < -STICK_DEADZONE));
            events.push((m.pos_id, value > STICK_DEADZONE));
        }
        events
    }
}

/// Fixed-capacity array for axis-to-digital conversion results (max 2 events).
pub struct ArrayVec<T> {
    items: [Option<T>; 2],
    len: usize,
}

impl<T: Copy> ArrayVec<T> {
    fn new() -> Self {
        Self {
            items: [None; 2],
            len: 0,
        }
    }

    fn push(&mut self, item: T) {
        if self.len < 2 {
            self.items[self.len] = Some(item);
            self.len += 1;
        }
    }
}

impl<T: Copy> Iterator for ArrayVec<T> {
    type Item = T;
    fn next(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        // Shift items forward
        let item = self.items[0];
        self.items[0] = self.items[1];
        self.items[1] = None;
        self.len -= 1;
        item
    }
}

/// Build a default controller map for a machine's input buttons.
/// Uses name-based matching, analogous to `default_key_map()`.
pub fn default_controller_map(buttons: &[InputButton]) -> ControllerMap {
    let mut cm = ControllerMap::new();

    let name_map: HashMap<&str, u8> = buttons.iter().map(|b| (b.name, b.id)).collect();

    // Face button A → primary action (fire/flap/jump)
    for name in ["P1 Fire", "P1 Flap", "P1 Jump", "Fire Center"] {
        if let Some(&id) = name_map.get(name) {
            cm.bind_button(Button::A, id);
            break;
        }
    }

    // Missile Command: three fire buttons on X/A/B
    if let Some(&id) = name_map.get("Fire Left") {
        cm.bind_button(Button::X, id);
    }
    if let Some(&id) = name_map.get("Fire Right") {
        cm.bind_button(Button::B, id);
    }

    // Asteroids: hyperspace on B, thrust on right trigger area (Y)
    if let Some(&id) = name_map.get("Hyperspace") {
        cm.bind_button(Button::B, id);
    }
    if let Some(&id) = name_map.get("Thrust") {
        cm.bind_button(Button::Y, id);
    }

    // System buttons (exact match first, then substring fallback for combo names)
    let find_by_name = |exact: &str| -> Option<u8> {
        name_map.get(exact).copied().or_else(|| {
            buttons
                .iter()
                .find(|b| b.name.contains(exact))
                .map(|b| b.id)
        })
    };
    if let Some(id) = find_by_name("Coin") {
        cm.bind_button(Button::Back, id);
    }
    if let Some(id) = find_by_name("P1 Start") {
        cm.bind_button(Button::Start, id);
    }

    // D-pad → P1 directions
    if let Some(&left) = name_map.get("P1 Left") {
        cm.bind_button(Button::DPadLeft, left);
    }
    if let Some(&right) = name_map.get("P1 Right") {
        cm.bind_button(Button::DPadRight, right);
    }
    if let Some(&up) = name_map.get("P1 Up") {
        cm.bind_button(Button::DPadUp, up);
    }
    if let Some(&down) = name_map.get("P1 Down") {
        cm.bind_button(Button::DPadDown, down);
    }

    // Left stick → P1 directions
    if let (Some(&left), Some(&right)) = (name_map.get("P1 Left"), name_map.get("P1 Right")) {
        cm.left_x = Some(StickMapping {
            neg_id: left,
            pos_id: right,
        });
    }
    if let (Some(&up), Some(&down)) = (name_map.get("P1 Up"), name_map.get("P1 Down")) {
        cm.left_y = Some(StickMapping {
            neg_id: up,
            pos_id: down,
        });
    }

    // Right stick → fire directions (Robotron twin-stick)
    if let (Some(&left), Some(&right)) =
        (name_map.get("P1 Fire Left"), name_map.get("P1 Fire Right"))
    {
        cm.right_x = Some(StickMapping {
            neg_id: left,
            pos_id: right,
        });
    }
    if let (Some(&up), Some(&down)) = (name_map.get("P1 Fire Up"), name_map.get("P1 Fire Down")) {
        cm.right_y = Some(StickMapping {
            neg_id: up,
            pos_id: down,
        });
    }

    cm
}

/// Map a mouse button to a machine fire button ID, using name-based lookup.
/// For trackball games: left click = primary fire, right/middle = secondary.
pub fn mouse_button_to_input(
    buttons: &[InputButton],
    mouse_btn: sdl2::mouse::MouseButton,
) -> Option<u8> {
    use sdl2::mouse::MouseButton;

    match mouse_btn {
        MouseButton::Left => {
            // Try "Fire Center" (Missile Command), "P1 Fire" (generic),
            // then "Jump L" (Crystal Castles).
            buttons
                .iter()
                .find(|b| b.name == "Fire Center")
                .or_else(|| buttons.iter().find(|b| b.name == "P1 Fire"))
                .or_else(|| buttons.iter().find(|b| b.name.contains("Jump L")))
                .map(|b| b.id)
        }
        MouseButton::Right => buttons
            .iter()
            .find(|b| b.name == "Fire Right")
            .or_else(|| buttons.iter().find(|b| b.name.contains("Jump R")))
            .map(|b| b.id),
        MouseButton::Middle => buttons.iter().find(|b| b.name == "Fire Left").map(|b| b.id),
        _ => None,
    }
}
