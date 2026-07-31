//! Shared `#[cfg(test)]` machine stub, used by both the `session` and
//! `rhai_api` unit tests so the binding path is exercised without ROM files.
//!
//! A trivial [`MachineCore::run_frame`] bumps a counter, [`handle_input`]
//! records events, a tiny [`BusDebug`] returns seeded bytes, and a single
//! [`DebugCpu`] reports a fixed PC/registers and a one-byte `NOP` disassembly.
//! The debug surface can be switched off (`has_debug = false`) to exercise the
//! "no debug support → None/empty" paths.

use std::cell::RefCell;
use std::rc::Rc;

use phosphor_core::core::debug::{BusDebug, DebugCpu, DebugRegister, Debuggable};
use phosphor_core::core::debug_trace::DebugTrace;
use phosphor_core::core::machine::{
    AudioSource, DipSwitches, FrontendMachine, InputConfigurable, InputControl, InputEvent,
    InputId, InputKind, MachineCore, MachineDebug, Nvram, Profilable, Renderable, SaveState,
};
use phosphor_core::cpu::disasm::DisassembledInstruction;
use phosphor_harness::Harness;

use crate::session::DebugSession;

/// The stub's single control, so tests can assert the resolved `InputId`.
pub const COIN_ID: InputId = InputId(7);

/// What the stub records so a test can prove a call reached the machine.
#[derive(Default)]
pub struct Recorder {
    pub frames: u32,
    pub inputs: Vec<InputEvent>,
}

/// A tiny CPU: fixed PC, two registers, and a one-byte `NOP` disassembly.
struct StubCpu;

impl Debuggable for StubCpu {
    fn debug_registers(&self) -> Vec<DebugRegister> {
        vec![
            DebugRegister {
                name: "A",
                value: 0x42,
                width: 8,
            },
            DebugRegister {
                name: "PC",
                value: 0x1234,
                width: 16,
            },
        ]
    }
}

impl DebugCpu for StubCpu {
    fn debug_pc(&self) -> u32 {
        0x1234
    }
    fn debug_at_instruction_boundary(&self) -> bool {
        true
    }
    fn debug_disassemble(&self, _addr: u32, _bytes: &[u8]) -> DisassembledInstruction {
        DisassembledInstruction {
            mnemonic: "NOP",
            operands: String::new(),
            byte_len: 1,
            bytes: [0; 10],
            target_addr: None,
        }
    }
}

/// A tiny bus: one CPU, a seeded read (`addr` low byte + 1), and a poke store
/// so a poked byte reads back (a poke overrides the seed at that address).
struct StubBus {
    cpu: StubCpu,
    poked: std::collections::HashMap<u32, u8>,
}

impl BusDebug for StubBus {
    fn devices(&self) -> Vec<(&str, &dyn Debuggable)> {
        Vec::new()
    }
    fn cpus(&self) -> Vec<(&str, &dyn DebugCpu)> {
        vec![("cpu0", &self.cpu)]
    }
    fn read(&self, _cpu_index: usize, addr: u32) -> Option<u8> {
        Some(
            self.poked
                .get(&addr)
                .copied()
                .unwrap_or((addr as u8).wrapping_add(1)),
        )
    }
    fn write(&mut self, _cpu_index: usize, addr: u32, data: u8) {
        self.poked.insert(addr, data);
    }
}

/// A minimal `FrontendMachine`: bumps a frame counter, records inputs, paints a
/// solid framebuffer, and optionally exposes the debug bus.
struct StubMachine {
    rec: Rc<RefCell<Recorder>>,
    bus: StubBus,
    has_debug: bool,
}

const CONTROLS: &[InputControl] = &[InputControl {
    id: COIN_ID,
    stable_name: "coin",
    label: "Coin",
    kind: InputKind::Button,
    player: None,
    default_bindings: &[],
}];

impl MachineCore for StubMachine {
    fn run_frame(&mut self) {
        self.rec.borrow_mut().frames += 1;
    }
    fn reset(&mut self) {}
    fn machine_id(&self) -> &str {
        "stub"
    }
}
impl Renderable for StubMachine {
    fn display_size(&self) -> (u32, u32) {
        (4, 3)
    }
    fn render_frame(&self, buffer: &mut [u8]) {
        buffer.fill(0xAB);
    }
}
impl AudioSource for StubMachine {}
impl InputConfigurable for StubMachine {
    fn input_controls(&self) -> &'static [InputControl] {
        CONTROLS
    }
    fn handle_input(&mut self, event: InputEvent) {
        self.rec.borrow_mut().inputs.push(event);
    }
}
impl MachineDebug for StubMachine {
    fn debug_bus(&self) -> Option<&dyn BusDebug> {
        self.has_debug.then_some(&self.bus as &dyn BusDebug)
    }
    fn debug_bus_mut(&mut self) -> Option<&mut dyn BusDebug> {
        self.has_debug.then_some(&mut self.bus as &mut dyn BusDebug)
    }
    fn cycles_per_frame(&self) -> u64 {
        if self.has_debug { 100 } else { 0 }
    }
    fn debug_tick(&mut self) -> u32 {
        if self.has_debug { 0b1 } else { 0 }
    }
}
impl DebugTrace for StubMachine {}
impl DipSwitches for StubMachine {}
impl SaveState for StubMachine {}
impl Nvram for StubMachine {}
impl Profilable for StubMachine {}

/// Build the stub machine and the recorder that observes it.
pub fn stub_machine(has_debug: bool) -> (Box<dyn FrontendMachine>, Rc<RefCell<Recorder>>) {
    let rec = Rc::new(RefCell::new(Recorder::default()));
    let machine = StubMachine {
        rec: Rc::clone(&rec),
        bus: StubBus {
            cpu: StubCpu,
            poked: std::collections::HashMap::new(),
        },
        has_debug,
    };
    (Box::new(machine), rec)
}

/// Wrap the stub machine in a [`DebugSession`] (via the harness `from_machine`
/// seam), returning the recorder for assertions.
pub fn stub_session(has_debug: bool) -> (DebugSession, Rc<RefCell<Recorder>>) {
    let (machine, rec) = stub_machine(has_debug);
    (DebugSession::wrap(Harness::from_machine(machine)), rec)
}
