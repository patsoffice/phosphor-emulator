//! Shared `#[cfg(test)]` machine stub, used by both the `session` and
//! `rhai_api` unit tests so the binding path is exercised without ROM files.
//!
//! A trivial [`MachineCore::run_frame`] bumps a counter, [`handle_input`]
//! records events, a tiny [`BusDebug`] returns seeded bytes, and a single
//! [`DebugCpu`] reports a fixed PC/registers and a one-byte `NOP` disassembly.
//! The debug surface can be switched off (`has_debug = false`) to exercise the
//! "no debug support → None/empty" paths.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use phosphor_core::core::debug::{BusDebug, DebugCpu, DebugRegister, Debuggable};
use phosphor_core::core::debug_trace::{DebugEvent, DebugEventKind, DebugTrace};
use phosphor_core::core::machine::{
    AudioSource, DipApplyTiming, DipChoice, DipOption, DipSwitchBank, DipSwitches, FrontendMachine,
    InputConfigurable, InputControl, InputEvent, InputId, InputKind, MachineCore, MachineDebug,
    Nvram, Profilable, Renderable, SaveState,
};
use phosphor_core::core::save_state::SaveError;
use phosphor_core::core::watchpoint::{
    DebugAccessSource, WatchpointHit, WatchpointKind, WatchpointPhase,
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

/// A tiny bus: one CPU, a seeded read (`addr` low byte + 1), a poke store so a
/// poked byte reads back, and a minimal write-watchpoint so the watchpoint
/// binding can be exercised. The value condition is ignored (its evaluation is
/// core's job, tested there) — any write to a watched address queues a hit.
struct StubBus {
    cpu: StubCpu,
    poked: std::collections::HashMap<u32, u8>,
    watched_writes: Vec<u32>,
    hits: VecDeque<WatchpointHit>,
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
    fn write(&mut self, cpu_index: usize, addr: u32, data: u8) {
        self.poked.insert(addr, data);
        if self.watched_writes.contains(&addr) {
            self.hits.push_back(WatchpointHit {
                cpu_index,
                source: DebugAccessSource::Frontend,
                cycle: 0,
                pc: None,
                addr,
                kind: WatchpointKind::Write,
                phase: WatchpointPhase::Before,
                value: u32::from(data),
                width: 1,
                region: None,
                device: None,
            });
        }
    }
    fn set_watchpoint_cond(
        &mut self,
        _cpu_index: usize,
        addr: u32,
        kind: WatchpointKind,
        _condition: phosphor_core::core::watchpoint::WatchpointCondition,
    ) {
        if kind == WatchpointKind::Write {
            self.watched_writes.push(addr);
        }
    }
    fn take_watchpoint_hit(&mut self) -> Option<WatchpointHit> {
        self.hits.pop_front()
    }
    fn clear_all_watchpoints(&mut self) {
        self.watched_writes.clear();
    }
}

/// A minimal `FrontendMachine`: bumps a frame counter, records inputs, paints a
/// solid framebuffer, optionally exposes the debug bus, and (when tracing is on)
/// records one bus event per frame.
struct StubMachine {
    rec: Rc<RefCell<Recorder>>,
    bus: StubBus,
    has_debug: bool,
    trace_on: bool,
    events: Vec<DebugEvent>,
    dip: u8,
}

/// A tiny DIP bank so the DIP-editing binding can be exercised.
const STUB_DIP_BANKS: &[DipSwitchBank] = &[DipSwitchBank {
    name: "TEST",
    options: &[
        DipOption {
            name: "Lives",
            mask: 0x03,
            apply: DipApplyTiming::Immediate,
            choices: &[
                DipChoice {
                    label: "3",
                    value: 0x00,
                },
                DipChoice {
                    label: "5",
                    value: 0x01,
                },
            ],
        },
        DipOption {
            name: "Bonus",
            mask: 0x0C,
            apply: DipApplyTiming::OnReset,
            choices: &[
                DipChoice {
                    label: "Low",
                    value: 0x00,
                },
                DipChoice {
                    label: "High",
                    value: 0x04,
                },
            ],
        },
    ],
}];

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
        let frame = {
            let mut rec = self.rec.borrow_mut();
            rec.frames += 1;
            u64::from(rec.frames)
        };
        if self.trace_on {
            self.events.push(DebugEvent {
                cpu_index: Some(0),
                addr: Some(0x1234),
                value: Some(0x99),
                width: 1,
                region: Some("test-ram"),
                ..DebugEvent::new(
                    frame,
                    DebugAccessSource::Cpu(0),
                    DebugEventKind::MemoryWrite,
                )
            });
        }
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
impl DebugTrace for StubMachine {
    fn set_trace_enabled(&mut self, enabled: bool) {
        self.trace_on = enabled;
    }
    fn trace_enabled(&self) -> bool {
        self.trace_on
    }
    fn trace_events(&mut self) -> &[DebugEvent] {
        &self.events
    }
    fn clear_trace_events(&mut self) {
        self.events.clear();
    }
}
impl DipSwitches for StubMachine {
    fn dip_banks(&self) -> &'static [DipSwitchBank] {
        STUB_DIP_BANKS
    }
    fn dip_bank_value(&self, bank: usize) -> u8 {
        if bank == 0 { self.dip } else { 0 }
    }
    fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        if bank == 0 {
            self.dip = value;
        }
    }
    // set_dip_option uses the default (mask-merge) impl.
}
impl SaveState for StubMachine {
    /// Snapshot is the poke store, encoded as 5-byte (addr LE + value) records.
    fn save_state(&self) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(self.bus.poked.len() * 5);
        for (addr, value) in &self.bus.poked {
            out.extend_from_slice(&addr.to_le_bytes());
            out.push(*value);
        }
        Some(out)
    }
    fn load_state(&mut self, data: &[u8]) -> Result<(), SaveError> {
        if !data.len().is_multiple_of(5) {
            return Err(SaveError::InvalidFormat(
                "stub state must be 5-byte records".into(),
            ));
        }
        self.bus.poked.clear();
        for chunk in data.chunks_exact(5) {
            let addr = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            self.bus.poked.insert(addr, chunk[4]);
        }
        Ok(())
    }
}
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
            watched_writes: Vec::new(),
            hits: VecDeque::new(),
        },
        has_debug,
        trace_on: false,
        events: Vec::new(),
        dip: 0,
    };
    (Box::new(machine), rec)
}

/// Wrap the stub machine in a [`DebugSession`] (via the harness `from_machine`
/// seam), returning the recorder for assertions.
pub fn stub_session(has_debug: bool) -> (DebugSession, Rc<RefCell<Recorder>>) {
    let (machine, rec) = stub_machine(has_debug);
    (DebugSession::wrap(Harness::from_machine(machine)), rec)
}
