//! `DebugSession`: a booted machine plus read-first inspection accessors.
//!
//! Wraps the shared [`phosphor_harness::Harness`] (boot + frame stepping) and
//! layers on the read-only "observe + drive" surface the Rhai bindings expose:
//! memory read, CPU pc/regs, disassemble, `run_frames`/`step`, inputs by stable
//! name, and screenshot-to-PNG. This is the only genuinely new code in v1;
//! everything above it is binding.
//!
//! Every accessor routes through the machine's existing debug traits, exactly
//! as the `disasm trace` cycle loop and the frontend's `debug_ui` already do:
//! reads go through [`MachineDebug::debug_bus`] → [`BusDebug::read`];
//! `pc`/`regs`/`disasm` go through [`BusDebug::cpus`] →
//! [`DebugCpu`]/[`Debuggable`]. Machines without debug support yield
//! `None`/empty, so scripts can check.

use std::collections::HashMap;
use std::path::Path;

use phosphor_core::core::debug_hang::{HangDetector, HangReport};
use phosphor_core::core::debug_trace::DebugEvent;
use phosphor_core::core::machine::{
    DipSwitchBank, FrontendMachine, InputEvent, InputId, Orientation,
};
use phosphor_core::core::watchpoint::{WatchpointCondition, WatchpointHit, WatchpointKind};
use phosphor_harness::movie::{Movie, rom_digest};
use phosphor_harness::{Harness, load_rom_set};
use phosphor_machines::registry;

/// A booted machine plus the read-first inspection state layered on top of it.
pub struct DebugSession {
    harness: Harness,
    /// `stable_name` → `InputId`, indexed once from `input_controls()` so
    /// scripts can drive controls by their stable string name.
    input_ids: HashMap<String, InputId>,
    /// Reused RGB24 render buffer (`w * h * 3`), so repeated `screenshot`s
    /// don't reallocate.
    fb: Vec<u8>,
    /// Watchpoint hits drained from the machine after each frame/step, kept
    /// here so a whole run's worth survives the 64-entry machine-side queue.
    hits: Vec<WatchpointHit>,
    /// Bus events drained from the machine's trace ring after each frame/step,
    /// so a whole run survives the ring's finite capacity.
    events: Vec<DebugEvent>,
    /// Accumulated audio, `Some` once a script calls `capture_audio`. The ring
    /// is drained every frame either way; this decides whether to keep it.
    audio: Option<Vec<i16>>,
    /// Per-frame PC-sampling hang detector, `Some` once a script enables it.
    hang: Option<HangDetector>,
    /// Hang reports collected while running frames.
    hang_reports: Vec<HangReport>,
    /// Where this session's ROMs came from, when it was opened by name. `None`
    /// for a session wrapping an already-booted machine (the in-frontend
    /// console), which has no path to re-derive a ROM digest from.
    rom_path: Option<String>,
}

impl DebugSession {
    /// Boot `machine_name` from the ROM set at `rom_path` (via the shared
    /// [`Harness`]) and index its input controls.
    pub fn open(machine_name: &str, rom_path: &str) -> Result<Self, String> {
        let harness = Harness::build(machine_name, rom_path, None, None, &[], &[])?;
        let mut session = Self::wrap(harness);
        session.rom_path = Some(rom_path.to_string());
        Ok(session)
    }

    /// Wrap a booted [`Harness`], building the stable-name → `InputId` index.
    pub(crate) fn wrap(mut harness: Harness) -> Self {
        let input_ids = harness
            .machine_mut()
            .input_controls()
            .iter()
            .map(|c| (c.stable_name.to_string(), c.id))
            .collect();
        Self {
            harness,
            input_ids,
            fb: Vec::new(),
            hits: Vec::new(),
            events: Vec::new(),
            audio: None,
            hang: None,
            hang_reports: Vec::new(),
            rom_path: None,
        }
    }

    /// Wrap an already-booted machine (the in-frontend console binds its *live*
    /// machine this way, then drives it through the session).
    pub fn from_machine(machine: Box<dyn FrontendMachine>) -> Self {
        Self::wrap(Harness::from_machine(machine))
    }

    /// Consume the session and hand the machine back — how the frontend reclaims
    /// its machine after the run loop ends.
    pub fn into_machine(self) -> Box<dyn FrontendMachine> {
        self.harness.into_machine()
    }

    /// Shared access to the wrapped machine, for a host driving it directly
    /// (rendering, audio, input) alongside the scripted surface.
    pub fn machine(&self) -> &dyn FrontendMachine {
        self.harness.machine()
    }

    /// Mutable access to the wrapped machine, for a host driving it directly.
    pub fn machine_mut(&mut self) -> &mut dyn FrontendMachine {
        self.harness.machine_mut()
    }

    /// Advance `n` whole frames through the harness, draining watchpoint hits
    /// and trace events after each (so neither overflows the machine's finite
    /// queues across the run) and sampling the hang detector once per frame.
    pub fn run_frames(&mut self, n: u64) {
        for _ in 0..n {
            self.harness.run_frame();
            self.drain_audio();
            self.drain_watchpoint_hits();
            self.drain_trace_events();
            self.sample_hang();
        }
    }

    /// Start accumulating audio, discarding anything captured so far.
    ///
    /// Scripts drive real input on a schedule, which is the only way to reach a
    /// sound a game only makes during play — a Donkey Kong footstep needs a
    /// coin, a start and a held joystick, none of which a device-level capture
    /// can produce.
    pub fn capture_audio(&mut self) {
        self.audio = Some(Vec::new());
    }

    /// Stop accumulating and write what was captured as a 16-bit mono WAV.
    pub fn write_audio(&mut self, path: &str) -> Result<usize, String> {
        let samples = self
            .audio
            .take()
            .ok_or_else(|| "capture_audio() was never called".to_string())?;
        let rate = self.harness.machine().audio_sample_rate();
        write_wav_16(path, &samples, rate).map_err(|e| format!("writing {path}: {e}"))?;
        Ok(samples.len())
    }

    /// Drain the machine's audio into the capture buffer.
    ///
    /// The ring has to be drained every frame whether or not anyone asked for
    /// audio, or it overruns and the capture develops gaps; when no capture is
    /// active the samples are simply dropped.
    fn drain_audio(&mut self) {
        let rate = self.harness.machine().audio_sample_rate().max(1) as usize;
        let mut chunk = vec![0i16; rate];
        loop {
            let n = self.harness.machine_mut().fill_audio(&mut chunk);
            if n == 0 {
                break;
            }
            if let Some(buf) = &mut self.audio {
                buf.extend_from_slice(&chunk[..n]);
            }
        }
    }

    /// Load a recorded input movie and replay it from power-on.
    ///
    /// The machine is reset first: a movie carries no save state, so it is only
    /// meaningful from there. Subsequent `run_frames` deliver the recording —
    /// no extra call is needed, because [`run_frames`](Self::run_frames) already
    /// goes through the harness, which already applies a bound movie.
    ///
    /// This is the cheap way to sample a recorded session at several points: one
    /// boot, then `run_frames` between screenshots, instead of `disasm replay
    /// --frames N` re-running from frame 0 for every sample.
    ///
    /// Two checks before anything is reset. A movie for another machine would
    /// bind against a control table it was never recorded against; a movie for
    /// this machine but a different ROM dump would bind fine and then silently
    /// diverge, which is the whole reason the digest exists. The digest check is
    /// skipped only when the session wraps a machine someone else booted, since
    /// there is then no ROM path to re-derive it from.
    pub fn load_movie(&mut self, path: &str) -> Result<(), String> {
        let bytes = std::fs::read(path).map_err(|e| format!("reading movie {path}: {e}"))?;
        let movie = Movie::decode(&bytes).map_err(|e| format!("reading movie {path}: {e}"))?;

        let id = self.harness.machine().machine_id().to_string();
        if movie.header.machine != id {
            return Err(format!(
                "movie {path} was recorded for '{}', but this session is running '{id}'",
                movie.header.machine
            ));
        }
        if let Some(rp) = self.rom_path.clone()
            && let Some(entry) = registry::find(&id)
        {
            let set = load_rom_set(&rp, entry.rom_names)
                .map_err(|e| format!("loading ROM set {rp}: {e}"))?;
            if rom_digest(&set) != movie.header.rom_digest {
                return Err(format!(
                    "movie {path} was recorded against a different ROM set than {rp}"
                ));
            }
        }

        self.harness.reset();
        self.harness
            .bind_movie(movie)
            .map_err(|e| format!("binding movie {path}: {e}"))?;
        // Observations collected before the reset describe a run that no longer
        // exists; keeping them would silently mix two timelines.
        self.hits.clear();
        self.events.clear();
        self.hang_reports.clear();
        Ok(())
    }

    /// Advance a single clock cycle via `debug_tick()`, returning the bitmask of
    /// CPUs that reached an instruction boundary (bit 0 = CPU 0, …). Machines
    /// without debug support (`cycles_per_frame == 0`) return `0`.
    ///
    /// Drains watchpoint hits and trace events, but does not sample the hang
    /// detector — hang detection is frame-granular (one PC sample per frame).
    pub fn step(&mut self) -> u32 {
        let mask = self.harness.machine_mut().debug_tick();
        self.drain_watchpoint_hits();
        self.drain_trace_events();
        mask
    }

    /// Move any queued watchpoint hits out of the machine and into `self.hits`.
    fn drain_watchpoint_hits(&mut self) {
        let machine = self.harness.machine_mut();
        while let Some(hit) = machine.take_watchpoint_hit() {
            self.hits.push(hit);
        }
    }

    /// Copy any recorded trace events out of the machine's ring into
    /// `self.events` and clear the ring. No-op when tracing is disabled.
    fn drain_trace_events(&mut self) {
        let machine = self.harness.machine_mut();
        if machine.trace_enabled() {
            let batch = machine.trace_events().to_vec();
            machine.clear_trace_events();
            self.events.extend(batch);
        }
    }

    /// Sample each CPU's PC once and feed the hang detector, collecting reports.
    fn sample_hang(&mut self) {
        if self.hang.is_none() {
            return;
        }
        let snaps: Vec<(usize, u32)> = match self.harness.machine_mut().debug_bus() {
            Some(bus) => bus
                .cpus()
                .iter()
                .enumerate()
                .map(|(i, (_, cpu))| (i, cpu.debug_pc()))
                .collect(),
            None => return,
        };
        let detector = self.hang.as_mut().unwrap();
        for (i, pc) in snaps {
            if let Some(report) = detector.observe(i, pc) {
                self.hang_reports.push(report);
            }
        }
    }

    /// Side-effect-free memory read from `cpu`'s address space. `None` for an
    /// unmapped/I-O address, an out-of-range CPU, or a machine without debug
    /// support.
    pub fn read(&mut self, cpu: usize, addr: u32) -> Option<u8> {
        self.harness.machine_mut().debug_bus()?.read(cpu, addr)
    }

    /// Program counter of `cpu`. `None` for an out-of-range CPU or a machine
    /// without debug support.
    pub fn pc(&mut self, cpu: usize) -> Option<u32> {
        let bus = self.harness.machine_mut().debug_bus()?;
        bus.cpus().get(cpu).map(|(_, c)| c.debug_pc())
    }

    /// Registers of `cpu` as `(name, value)` pairs. Empty for an out-of-range
    /// CPU or a machine without debug support.
    pub fn regs(&mut self, cpu: usize) -> Vec<(String, u64)> {
        let Some(bus) = self.harness.machine_mut().debug_bus() else {
            return Vec::new();
        };
        let cpus = bus.cpus();
        let Some((_, c)) = cpus.get(cpu) else {
            return Vec::new();
        };
        c.debug_registers()
            .into_iter()
            .map(|r| (r.name.to_string(), r.value))
            .collect()
    }

    /// Disassemble one instruction at `addr` in `cpu`'s address space,
    /// formatted as `"MNEMONIC operands"`. `None` for an out-of-range CPU or a
    /// machine without debug support.
    pub fn disasm(&mut self, cpu: usize, addr: u32) -> Option<String> {
        let bus = self.harness.machine_mut().debug_bus()?;
        let cpus = bus.cpus();
        let (_, c) = cpus.get(cpu)?;
        // Read a full instruction's worth of bytes (10 covers the longest
        // supported encoding), exactly as `debug_ui::disassemble_from` does.
        let mut bytes = [0u8; 10];
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = bus.read(cpu, addr.wrapping_add(i as u32)).unwrap_or(0);
        }
        Some(format!("{}", c.debug_disassemble(addr, &bytes)))
    }

    /// Poke `data` into `cpu`'s address space (a debugger write). Returns
    /// `false` for a machine without debug support (nothing written). Backed
    /// RAM takes the value; I/O and unmapped addresses are ignored by the
    /// board's write, exactly as a memory-viewer poke would be.
    ///
    /// This is the one write in the otherwise read-first surface: a poke is an
    /// explicit debug operation, distinct from the legitimate machine inputs
    /// [`input`](Self::input) drives.
    pub fn poke(&mut self, cpu: usize, addr: u32, data: u8) -> bool {
        match self.harness.machine_mut().debug_bus_mut() {
            Some(bus) => {
                bus.poke(cpu, addr, data);
                true
            }
            None => false,
        }
    }

    /// Number of CPUs exposed by the machine's debug bus (0 if none).
    pub fn cpu_count(&mut self) -> usize {
        self.harness
            .machine_mut()
            .debug_bus()
            .map_or(0, |bus| bus.cpus().len())
    }

    /// Set a watchpoint at `addr` on **every** CPU, returning the number of CPUs
    /// watched.
    ///
    /// Watching all CPUs is the deliberate default: watchpoints are scoped per
    /// CPU, and on multi-CPU boards a video/scroll register is often written by
    /// a sub-CPU, not the main one — so a single-CPU watch silently catches
    /// nothing. Each [`WatchpointHit`] carries its `cpu_index` so a script can
    /// still tell which CPU fired. Use [`watch_cpu`](Self::watch_cpu) to target
    /// one CPU deliberately.
    pub fn watch(&mut self, addr: u32, kind: WatchpointKind, cond: WatchpointCondition) -> usize {
        let n = self.cpu_count();
        for cpu in 0..n {
            self.harness
                .machine_mut()
                .set_watchpoint_cond(cpu, addr, kind, cond);
        }
        n
    }

    /// Set a watchpoint at `addr` on a single CPU's address space.
    pub fn watch_cpu(
        &mut self,
        cpu: usize,
        addr: u32,
        kind: WatchpointKind,
        cond: WatchpointCondition,
    ) {
        self.harness
            .machine_mut()
            .set_watchpoint_cond(cpu, addr, kind, cond);
    }

    /// Clear every watchpoint across all CPUs. Already-collected hits are kept.
    pub fn clear_watchpoints(&mut self) {
        self.harness.machine_mut().clear_all_watchpoints();
    }

    /// Drain and return every watchpoint hit collected so far (also sweeps any
    /// still queued in the machine). Leaves the accumulator empty.
    pub fn take_hits(&mut self) -> Vec<WatchpointHit> {
        self.drain_watchpoint_hits();
        std::mem::take(&mut self.hits)
    }

    /// Enable or disable bus-event recording. Enabling starts from a clean
    /// slate (any stale ring contents and the accumulator are dropped).
    ///
    /// Event trace is CPU-agnostic and region-tagged, and resolves mirrors —
    /// making it a better instrument than watchpoints for "which registers are
    /// written, and when within a frame". Note: bus-event tracing is wired for
    /// `AddressSpace16/32` boards; a machine without it records nothing.
    pub fn set_trace(&mut self, enabled: bool) {
        let machine = self.harness.machine_mut();
        machine.set_trace_enabled(enabled);
        machine.clear_trace_events();
        self.events.clear();
    }

    /// Whether bus-event recording is enabled.
    pub fn trace_enabled(&mut self) -> bool {
        self.harness.machine_mut().trace_enabled()
    }

    /// Drain and return every trace event collected so far (also sweeps the
    /// machine's current ring). Leaves the accumulator empty.
    pub fn take_events(&mut self) -> Vec<DebugEvent> {
        self.drain_trace_events();
        std::mem::take(&mut self.events)
    }

    /// Enable per-frame hang detection with the given PC `window` (bytes) and
    /// frame `threshold`. `run_frames` then samples each CPU's PC per frame; a
    /// CPU stuck within `window` for `threshold` frames produces a report.
    pub fn detect_hangs(&mut self, window: u32, threshold: u32) {
        self.hang = Some(HangDetector::with_params(window, threshold));
        self.hang_reports.clear();
    }

    /// Drain and return every hang report collected so far.
    pub fn take_hangs(&mut self) -> Vec<HangReport> {
        std::mem::take(&mut self.hang_reports)
    }

    /// Snapshot complete machine state, or `None` if unsupported. Pair with
    /// [`load_state`](Self::load_state) to branch a run and restore.
    pub fn save_state(&self) -> Option<Vec<u8>> {
        self.harness.machine().save_state()
    }

    /// Restore a snapshot taken by [`save_state`](Self::save_state).
    pub fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        self.harness
            .machine_mut()
            .load_state(data)
            .map_err(|e| e.to_string())
    }

    /// Reset the machine to power-on and zero the frame counter, clearing the
    /// watchpoint/event/hang accumulators (stale after a reset).
    pub fn reset(&mut self) {
        self.harness.reset();
        self.hits.clear();
        self.events.clear();
        self.hang_reports.clear();
        if let Some(detector) = self.hang.as_mut() {
            detector.reset();
        }
    }

    /// Static DIP-switch bank metadata (names, options, choices) — how a script
    /// discovers what it can set. Empty for a machine without DIP switches.
    pub fn dip_banks(&self) -> &'static [DipSwitchBank] {
        self.harness.machine().dip_banks()
    }

    /// Live byte value of DIP bank `bank` (0 if out of range).
    pub fn dip_bank_value(&self, bank: usize) -> u8 {
        self.harness.machine().dip_bank_value(bank)
    }

    /// Replace the whole live byte of DIP bank `bank`.
    pub fn set_dip_bank_value(&mut self, bank: usize, value: u8) {
        self.harness.machine_mut().set_dip_bank_value(bank, value);
    }

    /// Set one option within a bank, masking `value` into the bank byte.
    pub fn set_dip_option(&mut self, bank: usize, option: usize, value: u8) {
        self.harness
            .machine_mut()
            .set_dip_option(bank, option, value);
    }

    /// Set a DIP option by its name to the choice with the given label. Returns
    /// `false` if no such option/choice exists — the ergonomic path for sweeping
    /// configs (`set_dip("Difficulty", "Hard")`). Note: an option whose `apply`
    /// timing is `OnReset` only takes effect after [`reset`](Self::reset).
    pub fn set_dip_by_name(&mut self, option: &str, choice: &str) -> bool {
        let banks = self.harness.machine().dip_banks();
        for (bi, bank) in banks.iter().enumerate() {
            for (oi, opt) in bank.options.iter().enumerate() {
                if opt.name == option
                    && let Some(ch) = opt.choices.iter().find(|c| c.label == choice)
                {
                    let value = ch.value;
                    self.harness.machine_mut().set_dip_option(bi, oi, value);
                    return true;
                }
            }
        }
        false
    }

    /// Apply an *immediate* button edge to the control named `name`. Unknown
    /// names are ignored. Distinct from the harness's *scheduled* presses: this
    /// fires now, letting a script drive a timeline imperatively
    /// (`input("coin", true); run_frames(8); input("coin", false)`).
    pub fn input(&mut self, name: &str, pressed: bool) {
        if let Some(&id) = self.input_ids.get(name) {
            self.harness
                .machine_mut()
                .handle_input(InputEvent::Button { id, pressed });
        }
    }

    /// Apply an *immediate* absolute analog value (`-1.0..=1.0`) to the control
    /// named `name`. Unknown names are ignored.
    pub fn input_axis(&mut self, name: &str, value: f32) {
        if let Some(&id) = self.input_ids.get(name) {
            self.harness
                .machine_mut()
                .handle_input(InputEvent::Absolute { id, value });
        }
    }

    /// Apply an *immediate* relative motion delta (pointing-device units) to
    /// the control named `name`. Unknown names are ignored.
    ///
    /// This is the only way to drive a trackball or spinner from a script.
    /// Those machines accumulate `Relative` deltas into a wrapping counter and
    /// ignore `Absolute` entirely, so a script that reaches for
    /// [`input_axis`](Self::input_axis) on Marble Madness or Crystal Castles
    /// silently does nothing. Deltas accumulate until the machine drains them,
    /// so sustained motion means one call per frame, not one large call.
    pub fn input_relative(&mut self, name: &str, delta: f32) {
        if let Some(&id) = self.input_ids.get(name) {
            self.harness
                .machine_mut()
                .handle_input(InputEvent::Relative { id, delta });
        }
    }

    /// Render the current frame and write it to `path` as an 8-bit RGB PNG.
    ///
    /// Mirrors `disasm frameshot`: render the native buffer, then apply the
    /// machine's declared orientation centrally (so the PNG matches what the
    /// cabinet shows). Raster-only — vector machines render through
    /// `vector_display_list`, the same limitation frameshot has.
    pub fn screenshot(&mut self, path: &str) -> Result<(), String> {
        let (nw, nh) = self.harness.machine().display_size();
        self.fb.resize(nw as usize * nh as usize * 3, 0);

        let machine = self.harness.machine_mut();
        machine.render_frame(&mut self.fb);
        let orient = machine.orientation();

        if orient == Orientation::NORMAL {
            write_png(Path::new(path), &self.fb, nw, nh)
        } else {
            let (dw, dh) = if orient.swaps_axes() {
                (nh, nw)
            } else {
                (nw, nh)
            };
            let mut oriented = vec![0u8; dw as usize * dh as usize * 3];
            phosphor_core::gfx::apply_orientation(
                &self.fb,
                &mut oriented,
                nw as usize,
                nh as usize,
                orient,
            );
            write_png(Path::new(path), &oriented, dw, dh)
        }
        .map_err(|e| format!("writing {path}: {e}"))
    }

    /// Number of frames run so far via [`run_frames`](Self::run_frames).
    pub fn frame_count(&self) -> u64 {
        self.harness.frame_count() as u64
    }

    /// The machine's short identifier (`MachineCore::machine_id`).
    pub fn machine_id(&self) -> String {
        self.harness.machine().machine_id().to_string()
    }

    /// Native display size `(width, height)` in pixels.
    pub fn display_size(&self) -> (u32, u32) {
        self.harness.machine().display_size()
    }
}

/// Write an RGB24 buffer as an 8-bit RGB PNG. Inlined (like disasm's
/// `gfxsheet::write_png`) rather than depending on the frontend for ~10 lines.
fn write_png(path: &Path, rgb24: &[u8], width: u32, height: u32) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let writer = std::io::BufWriter::new(file);
    let mut encoder = png::Encoder::new(writer, width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    let mut png_writer = encoder.write_header().map_err(std::io::Error::other)?;
    png_writer
        .write_image_data(rgb24)
        .map_err(std::io::Error::other)?;
    png_writer.finish().map_err(std::io::Error::other)?;
    Ok(())
}

/// Write 16-bit mono PCM as a WAV.
///
/// Hand-rolled rather than shared with `disasm`: this crate does not depend on
/// that binary, and the header is fourteen fields.
fn write_wav_16(path: &str, samples: &[i16], rate: u32) -> std::io::Result<()> {
    use std::io::Write;
    let data_len = (samples.len() * 2) as u32;
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(b"RIFF")?;
    f.write_all(&(36 + data_len).to_le_bytes())?;
    f.write_all(b"WAVE")?;
    f.write_all(b"fmt ")?;
    f.write_all(&16u32.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&1u16.to_le_bytes())?;
    f.write_all(&rate.to_le_bytes())?;
    f.write_all(&(rate * 2).to_le_bytes())?;
    f.write_all(&2u16.to_le_bytes())?;
    f.write_all(&16u16.to_le_bytes())?;
    f.write_all(b"data")?;
    f.write_all(&data_len.to_le_bytes())?;
    for s in samples {
        f.write_all(&s.to_le_bytes())?;
    }
    f.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{COIN_ID, stub_machine, stub_session as session};

    /// `Harness::run_frame` applies scheduled input and advances the frame
    /// counter itself, so a caller that drives the machine cycle-by-cycle
    /// (`debug_tick` — the debugger and `disasm trace --cpu`/`--break-pc`)
    /// bypasses both and used to silently drop every scheduled edge. The two
    /// halves are public for exactly that caller; this pins that using them
    /// delivers the same edges, on the same frames, as `run_frame`.
    #[test]
    fn cycle_stepped_driving_applies_the_same_scheduled_input_as_run_frame() {
        let deliveries = |cycle_stepped: bool| {
            let (machine, rec) = stub_machine(true);
            let mut h = Harness::from_machine(machine);
            h.schedule_motion(InputId(7), 2, 3, 1.5);
            for _ in 0..6 {
                if cycle_stepped {
                    // What the cycle loop does in place of `run_frame`.
                    h.apply_scheduled_input();
                    h.machine_mut().debug_tick();
                    h.advance_frame();
                } else {
                    h.run_frame();
                }
            }
            let frame = h.frame_count();
            let motions = rec
                .borrow()
                .inputs
                .iter()
                .filter(|e| matches!(e, InputEvent::Relative { .. }))
                .count();
            (frame, motions)
        };

        // The motion is scheduled for frames 2, 3 and 4.
        assert_eq!(deliveries(false), (6, 3), "frame-loop baseline");
        assert_eq!(deliveries(true), (6, 3), "cycle-stepped driving");
    }

    /// A movie bound through the session replays via `run_frames`, with no extra
    /// call — the property that makes this binding ~20 lines rather than a
    /// parallel playback path.
    #[test]
    fn a_bound_movie_is_delivered_by_run_frames() {
        use phosphor_harness::MovieRecorder;

        let (machine, rec) = stub_machine(true);
        let controls = machine.input_controls();
        let mut r = MovieRecorder::new("stub", [0u8; 32], controls, Vec::new(), None);
        for frame in 0..6 {
            if frame == 2 || frame == 4 {
                r.push_event(InputEvent::Button {
                    id: COIN_ID,
                    pressed: frame == 2,
                });
            }
            r.advance_frame();
        }
        let movie = r.finish();

        let dir = std::env::temp_dir().join(format!("phosphor-rhai-movie-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("stub.phmi");
        std::fs::write(&path, movie.encode()).expect("write movie");

        let mut s = DebugSession::from_machine(machine);
        s.load_movie(path.to_str().unwrap()).expect("load_movie");
        s.run_frames(6);

        let buttons = rec
            .borrow()
            .inputs
            .iter()
            .filter(|e| matches!(e, InputEvent::Button { .. }))
            .count();
        assert_eq!(buttons, 2, "the movie's presses were not delivered");
        assert_eq!(s.frame_count(), 6);
        let _ = std::fs::remove_file(&path);
    }

    /// A movie for a different machine is refused *before* the reset, so a
    /// mistyped path cannot quietly destroy the session it was aimed at.
    #[test]
    fn load_movie_refuses_another_machines_recording() {
        use phosphor_harness::MovieRecorder;

        let (machine, _rec) = stub_machine(true);
        let r = MovieRecorder::new(
            "not_the_stub",
            [0u8; 32],
            machine.input_controls(),
            Vec::new(),
            None,
        );
        let dir = std::env::temp_dir().join(format!("phosphor-rhai-movie-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        let path = dir.join("wrong.phmi");
        std::fs::write(&path, r.finish().encode()).expect("write movie");

        let mut s = DebugSession::from_machine(machine);
        s.run_frames(3);
        let err = s
            .load_movie(path.to_str().unwrap())
            .expect_err("must refuse");
        assert!(err.contains("not_the_stub"), "unhelpful error: {err}");
        assert_eq!(s.frame_count(), 3, "the session was reset despite refusing");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn run_frames_advances_machine_and_frame_count() {
        let (mut s, rec) = session(true);
        assert_eq!(s.frame_count(), 0);
        s.run_frames(3);
        assert_eq!(s.frame_count(), 3);
        assert_eq!(rec.borrow().frames, 3);
    }

    #[test]
    fn read_returns_seeded_bytes() {
        let (mut s, _) = session(true);
        assert_eq!(s.read(0, 0x10), Some(0x11));
        assert_eq!(s.read(0, 0x00), Some(0x01));
    }

    #[test]
    fn poke_writes_back_and_read_reflects_it() {
        let (mut s, _) = session(true);
        assert_eq!(s.read(0, 0x20), Some(0x21)); // seed before poke
        assert!(s.poke(0, 0x20, 0xEE));
        assert_eq!(s.read(0, 0x20), Some(0xEE)); // poked value overrides seed
    }

    #[test]
    fn poke_without_debug_support_returns_false() {
        let (mut s, _) = session(false);
        assert!(!s.poke(0, 0x20, 0xEE));
    }

    #[test]
    fn watchpoint_hit_collected_on_write() {
        let (mut s, _) = session(true);
        let n = s.watch(0x40, WatchpointKind::Write, WatchpointCondition::Always);
        assert_eq!(n, 1); // the stub exposes one CPU
        s.poke(0, 0x40, 0x99); // write to the watched address queues a hit
        let hits = s.take_hits();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].addr, 0x40);
        assert_eq!(hits[0].value, 0x99);
        assert_eq!(hits[0].cpu_index, 0);
        assert_eq!(hits[0].kind, WatchpointKind::Write);
        // Draining leaves the accumulator empty.
        assert!(s.take_hits().is_empty());
    }

    #[test]
    fn clear_watchpoints_stops_hits() {
        let (mut s, _) = session(true);
        s.watch(0x40, WatchpointKind::Write, WatchpointCondition::Always);
        s.clear_watchpoints();
        s.poke(0, 0x40, 0x99);
        assert!(s.take_hits().is_empty());
    }

    #[test]
    fn cpu_count_reflects_debug_support() {
        let (mut s, _) = session(true);
        assert_eq!(s.cpu_count(), 1);
        let (mut nod, _) = session(false);
        assert_eq!(nod.cpu_count(), 0);
    }

    #[test]
    fn trace_events_collected_across_frames() {
        let (mut s, _) = session(true);
        s.set_trace(true);
        assert!(s.trace_enabled());
        s.run_frames(3); // the stub records one bus event per frame while tracing
        let events = s.take_events();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].addr, Some(0x1234));
        assert_eq!(events[0].value, Some(0x99));
        assert_eq!(events[0].region, Some("test-ram"));
        // Draining leaves the accumulator empty.
        assert!(s.take_events().is_empty());
    }

    #[test]
    fn trace_disabled_records_nothing() {
        let (mut s, _) = session(true);
        s.run_frames(3);
        assert!(!s.trace_enabled());
        assert!(s.take_events().is_empty());
    }

    #[test]
    fn hang_detector_reports_stuck_cpu() {
        // The stub CPU's PC is fixed at 0x1234 → a perpetual hang.
        let (mut s, _) = session(true);
        s.detect_hangs(8, 3);
        s.run_frames(5);
        let hangs = s.take_hangs();
        assert_eq!(hangs.len(), 1); // reports once at threshold, then stays quiet
        assert_eq!(hangs[0].cpu_index, 0);
        assert_eq!(hangs[0].pc, 0x1234);
        assert!(hangs[0].frames_stuck >= 3);
        assert!(s.take_hangs().is_empty());
    }

    #[test]
    fn save_and_load_state_round_trip() {
        let (mut s, _) = session(true);
        s.poke(0, 0x50, 0xAA);
        assert_eq!(s.read(0, 0x50), Some(0xAA));
        let snap = s.save_state().expect("stub supports save state");
        s.poke(0, 0x50, 0xBB);
        assert_eq!(s.read(0, 0x50), Some(0xBB));
        s.load_state(&snap).unwrap();
        assert_eq!(s.read(0, 0x50), Some(0xAA)); // restored to the snapshot
    }

    #[test]
    fn load_state_rejects_bad_data() {
        let (mut s, _) = session(true);
        assert!(s.load_state(&[1, 2, 3]).is_err()); // not a whole number of records
    }

    #[test]
    fn reset_zeroes_frames_and_clears_accumulators() {
        let (mut s, _) = session(true);
        s.set_trace(true);
        s.run_frames(3);
        assert_eq!(s.frame_count(), 3);
        s.reset();
        assert_eq!(s.frame_count(), 0);
        assert!(s.take_events().is_empty());
    }

    #[test]
    fn dip_editing() {
        let (mut s, _) = session(true);

        let banks = s.dip_banks();
        assert_eq!(banks.len(), 1);
        assert_eq!(banks[0].name, "TEST");
        assert_eq!(banks[0].options[0].name, "Lives");

        s.set_dip_bank_value(0, 0x05);
        assert_eq!(s.dip_bank_value(0), 0x05);

        // Masked option write: Bonus (mask 0x0C) := 0x04 leaves the Lives bits.
        s.set_dip_option(0, 1, 0x04);
        assert_eq!(s.dip_bank_value(0) & 0x0C, 0x04);
        assert_eq!(s.dip_bank_value(0) & 0x03, 0x01);

        // By-name setter.
        assert!(s.set_dip_by_name("Lives", "5"));
        assert_eq!(s.dip_bank_value(0) & 0x03, 0x01);
        assert!(!s.set_dip_by_name("Nonexistent", "x"));
        assert!(!s.set_dip_by_name("Lives", "99")); // unknown choice label
    }

    #[test]
    fn pc_regs_and_disasm_route_through_debug_traits() {
        let (mut s, _) = session(true);
        assert_eq!(s.pc(0), Some(0x1234));
        assert_eq!(
            s.regs(0),
            vec![("A".to_string(), 0x42), ("PC".to_string(), 0x1234)]
        );
        assert_eq!(s.disasm(0, 0x1000).as_deref(), Some("NOP"));
    }

    #[test]
    fn input_reaches_handle_input_by_stable_name() {
        let (mut s, rec) = session(true);
        s.input("coin", true);
        s.input("coin", false);
        s.input("nonexistent", true); // ignored, no panic
        s.input_axis("coin", 0.5);
        s.input_relative("coin", -3.0);
        s.input_relative("nonexistent", 1.0); // ignored, no panic
        assert_eq!(
            rec.borrow().inputs,
            vec![
                InputEvent::Button {
                    id: COIN_ID,
                    pressed: true
                },
                InputEvent::Button {
                    id: COIN_ID,
                    pressed: false
                },
                InputEvent::Absolute {
                    id: COIN_ID,
                    value: 0.5
                },
                InputEvent::Relative {
                    id: COIN_ID,
                    delta: -3.0
                },
            ]
        );
    }

    /// Scheduled motion is a `phosphor-harness` feature, but the only stub
    /// `FrontendMachine` in the workspace lives here — duplicating 300 lines of
    /// double into `harness` for one test would cost more than it buys.
    #[test]
    fn scheduled_motion_feeds_one_delta_per_frame() {
        let (machine, rec) = crate::test_support::stub_machine(true);
        let mut harness = phosphor_harness::Harness::from_machine(machine);
        harness.schedule_motion(COIN_ID, 2, 3, -1.5);
        for _ in 0..6 {
            harness.run_frame();
        }
        assert_eq!(
            rec.borrow().inputs,
            vec![
                InputEvent::Relative {
                    id: COIN_ID,
                    delta: -1.5
                };
                3
            ]
        );
    }

    #[test]
    fn step_returns_boundary_mask() {
        let (mut s, _) = session(true);
        assert_eq!(s.step(), 0b1);
    }

    #[test]
    fn no_debug_support_yields_none_and_empty() {
        let (mut s, _) = session(false);
        assert_eq!(s.read(0, 0x10), None);
        assert_eq!(s.pc(0), None);
        assert!(s.regs(0).is_empty());
        assert_eq!(s.disasm(0, 0x1000), None);
        assert_eq!(s.step(), 0);
    }

    #[test]
    fn out_of_range_cpu_yields_none_and_empty() {
        let (mut s, _) = session(true);
        assert_eq!(s.pc(9), None);
        assert!(s.regs(9).is_empty());
        assert_eq!(s.disasm(9, 0x1000), None);
    }

    #[test]
    fn identity_accessors() {
        let (s, _) = session(true);
        assert_eq!(s.machine_id(), "stub");
        assert_eq!(s.display_size(), (4, 3));
    }

    #[test]
    fn screenshot_writes_a_valid_png() {
        let (mut s, _) = session(true);
        let path = std::env::temp_dir().join("phosphor_script_session_shot.png");
        let _ = std::fs::remove_file(&path);
        s.screenshot(path.to_str().unwrap()).unwrap();

        let data = std::fs::read(&path).unwrap();
        assert_eq!(&data[..8], b"\x89PNG\r\n\x1a\n", "PNG magic");
        std::fs::remove_file(&path).unwrap();
    }
}
