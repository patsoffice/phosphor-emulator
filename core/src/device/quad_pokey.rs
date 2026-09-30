//! Four POKEYs whose audio outputs a board ties to one node.
//!
//! Some Atari boards carry four POKEYs, often as one quad package, and wire
//! the four `AUD` pins straight together rather than each through its own
//! resistor. I, Robot's CPU PCB is the case this was written for: SP-251 sheet
//! 4A shows the quad POKEY at 4E with its four audio pins joined by junction
//! dots onto one net, with one load after it.
//!
//! On such a node the chips do not add. Each chip's output is a set of
//! open-drain devices, so the node sees the conductance of every device that is
//! on across all four chips, against one pull-up and one capacitor. How far the
//! node falls, and how fast its capacitor follows, both depend on that total:
//! a loud chip compresses a quiet one and moves the corner for all of them. Four
//! [`Pokey`]s each running its own [`PokeyLoad`] cannot express that, since each
//! would see only its own devices.
//!
//! So this steps the four chips' sound generators, sums their conductance every
//! clock, runs one output stage on the sum, and resamples once. Registers, pot
//! lines and interrupts stay per chip: reach them through [`Self::chip`] and
//! [`Self::chip_mut`]. Only the shared audio output is new.
//!
//! Boards whose four chips each have their own resistor into a mixer, such as
//! Star Wars' sound board, are four [`Pokey`]s, not this.

use super::pokey::{OutputStage, Pokey, PokeyLoad};

/// Four POKEYs sharing one audio output node.
#[derive(phosphor_macros::Saveable)]
#[save_version(1)]
pub struct QuadPokey {
    chips: [Pokey; 4],
    resampler: crate::audio::AudioResampler<f32>,
    /// The node's load, when the board models it; `None` keeps the linear mix,
    /// the four chips' volume levels summed and scaled so every channel of
    /// every chip at 15 is 1.0. Board configuration rather than state, as on
    /// [`Pokey`].
    #[save_skip]
    output_stage: Option<OutputStage>,
}

impl QuadPokey {
    /// Four chips on `master_clock_hz`, resampled to `output_sample_rate`.
    pub fn with_clock(master_clock_hz: u32, output_sample_rate: u32) -> Self {
        Self {
            chips: std::array::from_fn(|_| Pokey::with_clock(master_clock_hz, output_sample_rate)),
            resampler: crate::audio::AudioResampler::new(
                master_clock_hz as u64,
                output_sample_rate as u64,
            ),
            output_stage: None,
        }
    }

    /// Chip `n`, 0 to 3, for its registers, pot lines and interrupt.
    pub fn chip(&self, n: usize) -> &Pokey {
        &self.chips[n]
    }

    /// Chip `n`, 0 to 3, for its registers, pot lines and interrupt.
    pub fn chip_mut(&mut self, n: usize) -> &mut Pokey {
        &mut self.chips[n]
    }

    /// Model the load on the shared node. The output is then what that load
    /// reads, in its own units (see [`PokeyLoad`]), for the devices of all
    /// four chips together; divide by
    /// [`PokeyLoad::full_scale_of_chips`]`(4)` to scale it.
    pub fn set_output_load(&mut self, load: PokeyLoad) {
        self.output_stage = Some(OutputStage::new(load, self.chips[0].master_clock_hz()));
    }

    /// Advance all four chips one master clock and the shared node with them.
    pub fn tick(&mut self) {
        let mut mixed = 0.0;
        let mut conductance = 0.0;
        for chip in &mut self.chips {
            let (m, g) = chip.clock();
            mixed += m;
            conductance += g;
        }
        let sample = match &mut self.output_stage {
            Some(stage) => stage.step(conductance) as f32,
            None => mixed / 240.0,
        };
        self.resampler.tick(sample);
    }

    /// Take the node's resampled output accumulated since the last call.
    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.resampler.drain_audio()
    }

    /// Reset all four chips and put the node back at rest.
    pub fn reset(&mut self) {
        for chip in &mut self.chips {
            chip.reset();
        }
        self.resampler.reset();
        if let Some(stage) = &mut self.output_stage {
            stage.rest();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::pokey::PokeyOutputNetwork;

    const CLOCK: u32 = 1_512_000;
    const RATE: u32 = 44_100;

    /// Program chip `n`'s channel 1 as a pure tone at volume 15.
    fn tone(p: &mut Pokey, audf: u8) {
        p.write(0x0F, 0x03); // SKCTL: out of reset
        p.write(0x00, audf); // AUDF1
        p.write(0x01, 0xAF); // AUDC1: pure tone, volume 15
    }

    /// Without a load the node is the linear mix, which is exactly the four
    /// chips averaged: this is what I, Robot did before the node was modeled,
    /// four `Pokey`s each drained and summed at a quarter. Checked against that
    /// computed the old way, so it exercises the split of `Pokey::tick`.
    #[test]
    fn unloaded_it_is_the_four_chips_averaged() {
        let mut quad = QuadPokey::with_clock(CLOCK, RATE);
        let mut four: [Pokey; 4] = std::array::from_fn(|_| Pokey::with_clock(CLOCK, RATE));
        for (n, audf) in [(0, 10), (1, 23), (2, 47), (3, 90)] {
            tone(quad.chip_mut(n), audf);
            tone(&mut four[n], audf);
        }
        for _ in 0..CLOCK / 10 {
            quad.tick();
            for p in &mut four {
                p.tick();
            }
        }
        let q = quad.drain_audio();
        let parts: Vec<Vec<f32>> = four.iter_mut().map(|p| p.drain_audio()).collect();
        assert_eq!(q.len(), parts[0].len());
        let mut nonzero = 0;
        for i in 0..q.len() {
            let avg = 0.25 * (parts[0][i] + parts[1][i] + parts[2][i] + parts[3][i]);
            assert!(
                (q[i] - avg).abs() < 1e-5,
                "sample {i}: {} against {avg}",
                q[i]
            );
            if avg.abs() > 0.01 {
                nonzero += 1;
            }
        }
        assert!(nonzero > q.len() / 4, "the tones never sounded");
    }

    /// Settled drop at the shared node with channel 1 of the listed chips
    /// volume-only at 15, against 220 ohm to +5 V.
    fn settled_drop(chips: &[usize]) -> f32 {
        let mut quad = QuadPokey::with_clock(CLOCK, RATE);
        quad.set_output_load(PokeyLoad::PullUp(PokeyOutputNetwork {
            pullup_ohms: 220.0,
            supply_v: 5.0,
            load_farads: 0.0,
        }));
        for &n in chips {
            let p = quad.chip_mut(n);
            p.write(0x0F, 0x03);
            p.write(0x01, 0x1F); // AUDC1: volume only, 15
        }
        for _ in 0..20_000 {
            quad.tick();
        }
        *quad.drain_audio().last().unwrap()
    }

    /// On a shared node one chip compresses another: two chips at 15 pull the
    /// node less than twice as far as one, by exactly what one node against
    /// the sum of both chips' devices predicts. Four separate loads, one per
    /// chip, would give exactly twice.
    #[test]
    fn chips_on_one_node_compress_each_other() {
        let one = settled_drop(&[0]) as f64;
        let two = settled_drop(&[0, 3]) as f64;
        let g = AUD_15;
        let g_up = 1.0 / 220.0;
        let expected = (2.0 * g / (g_up + 2.0 * g)) / (g / (g_up + g));
        assert!(
            (two / one - expected).abs() < 1e-4,
            "{two} / {one} against {expected}"
        );
        assert!(two / one < 1.95, "{two} / {one}");
    }

    /// One channel at volume 15: bits 0 to 3 of the data sheet's devices in
    /// parallel, from the same rows `Pokey` derives them from.
    const AUD_15: f64 = {
        let rows = [4.2, 3.4, 2.1, 1.2];
        let mut g = 0.0;
        let mut i = 0;
        while i < 4 {
            g += (4.75 - rows[i]) / (10_000.0 * rows[i]);
            i += 1;
        }
        g
    };

    /// Reset puts every chip and the node back at rest.
    #[test]
    fn reset_silences_the_node() {
        let mut quad = QuadPokey::with_clock(CLOCK, RATE);
        quad.set_output_load(PokeyLoad::PullUp(PokeyOutputNetwork {
            pullup_ohms: 220.0,
            supply_v: 5.0,
            load_farads: 0.22e-6,
        }));
        for n in 0..4 {
            tone(quad.chip_mut(n), 10);
        }
        for _ in 0..10_000 {
            quad.tick();
        }
        quad.reset();
        quad.drain_audio();
        for _ in 0..10_000 {
            quad.tick();
        }
        let after = quad.drain_audio();
        assert!(after.iter().all(|&s| s.abs() < 1e-6), "{after:?}");
    }
}
