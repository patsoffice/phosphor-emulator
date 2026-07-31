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

use phosphor_core::core::machine::{InputEvent, InputId, Orientation};
use phosphor_core::core::watchpoint::{WatchpointCondition, WatchpointHit, WatchpointKind};
use phosphor_harness::Harness;

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
}

impl DebugSession {
    /// Boot `machine_name` from the ROM set at `rom_path` (via the shared
    /// [`Harness`]) and index its input controls.
    pub fn open(machine_name: &str, rom_path: &str) -> Result<Self, String> {
        let harness = Harness::build(machine_name, rom_path, None, None, &[])?;
        Ok(Self::wrap(harness))
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
        }
    }

    /// Advance `n` whole frames through the harness, draining watchpoint hits
    /// after each so a hot address doesn't overflow the machine's 64-entry
    /// queue across the run.
    pub fn run_frames(&mut self, n: u64) {
        for _ in 0..n {
            self.harness.run_frame();
            self.drain_watchpoint_hits();
        }
    }

    /// Advance a single clock cycle via `debug_tick()`, returning the bitmask of
    /// CPUs that reached an instruction boundary (bit 0 = CPU 0, …). Machines
    /// without debug support (`cycles_per_frame == 0`) return `0`.
    pub fn step(&mut self) -> u32 {
        let mask = self.harness.machine_mut().debug_tick();
        self.drain_watchpoint_hits();
        mask
    }

    /// Move any queued watchpoint hits out of the machine and into `self.hits`.
    fn drain_watchpoint_hits(&mut self) {
        let machine = self.harness.machine_mut();
        while let Some(hit) = machine.take_watchpoint_hit() {
            self.hits.push(hit);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{COIN_ID, stub_session as session};

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
