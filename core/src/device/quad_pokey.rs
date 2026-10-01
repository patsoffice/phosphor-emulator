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
//! Not every board ties all four. Major Havoc's (SP-252 sheet 10A) ties OUT1 to
//! OUT3 onto one node and gives OUT4 a node of its own, each into its own
//! amplifier. So each chip is assigned to an output node with
//! [`Self::set_nodes`], every node has its own load and its own stream, and by
//! default all four share node 0.
//!
//! Boards whose four chips each have their own resistor into a mixer, such as
//! Star Wars' sound board, are four [`Pokey`]s, not this.

use super::pokey::{OutputStage, Pokey, PokeyLoad};

/// Four POKEYs whose audio outputs are tied onto up to four nodes.
#[derive(phosphor_macros::Saveable)]
#[save_version(2)]
pub struct QuadPokey {
    chips: [Pokey; 4],
    /// One stream per node; only the first [`Self::node_count`] are ticked.
    resamplers: [crate::audio::AudioResampler<f32>; 4],
    /// Each node's load, when the board models it; `None` keeps the linear
    /// mix, the node's chips' volume levels summed and scaled so every channel
    /// of every chip on it at 15 is 1.0. Board configuration rather than state,
    /// as on [`Pokey`], and so is everything below.
    #[save_skip]
    output_stages: [Option<OutputStage>; 4],
    /// Which node each chip's output is tied to.
    #[save_skip]
    node_of: [usize; 4],
    /// Nodes in use: one more than the highest in `node_of`.
    #[save_skip]
    node_count: usize,
}

impl QuadPokey {
    /// Four chips on `master_clock_hz`, resampled to `output_sample_rate`, all
    /// tied to node 0.
    pub fn with_clock(master_clock_hz: u32, output_sample_rate: u32) -> Self {
        Self {
            chips: std::array::from_fn(|_| Pokey::with_clock(master_clock_hz, output_sample_rate)),
            resamplers: std::array::from_fn(|_| {
                crate::audio::AudioResampler::new(master_clock_hz as u64, output_sample_rate as u64)
            }),
            output_stages: [None, None, None, None],
            node_of: [0; 4],
            node_count: 1,
        }
    }

    /// Tie chip `n`'s output to node `nodes[n]`, 0 to 3. Nodes are numbered
    /// from 0 without gaps; a node no chip is tied to is a board error.
    pub fn set_nodes(&mut self, nodes: [usize; 4]) {
        let count = nodes.iter().max().map_or(1, |&m| m + 1);
        assert!(count <= 4, "a quad POKEY has at most four nodes: {nodes:?}");
        for node in 0..count {
            assert!(nodes.contains(&node), "node {node} has no chip: {nodes:?}");
        }
        self.node_of = nodes;
        self.node_count = count;
    }

    /// Nodes in use.
    pub fn node_count(&self) -> usize {
        self.node_count
    }

    /// How many chips are tied to `node`, for scaling its output with
    /// [`PokeyLoad::full_scale_of_chips`].
    pub fn chips_on(&self, node: usize) -> usize {
        self.node_of.iter().filter(|&&n| n == node).count()
    }

    /// Chip `n`, 0 to 3, for its registers, pot lines and interrupt.
    pub fn chip(&self, n: usize) -> &Pokey {
        &self.chips[n]
    }

    /// Chip `n`, 0 to 3, for its registers, pot lines and interrupt.
    pub fn chip_mut(&mut self, n: usize) -> &mut Pokey {
        &mut self.chips[n]
    }

    /// Model the load on node `node`. Its output is then what that load reads,
    /// in its own units (see [`PokeyLoad`]), for the devices of every chip tied
    /// to it together; divide by
    /// [`PokeyLoad::full_scale_of_chips`]`(self.chips_on(node))` to scale it.
    pub fn set_node_load(&mut self, node: usize, load: PokeyLoad) {
        self.output_stages[node] = Some(OutputStage::new(load, self.chips[0].master_clock_hz()));
    }

    /// [`Self::set_node_load`] on node 0, the only node when all four chips
    /// share one.
    pub fn set_output_load(&mut self, load: PokeyLoad) {
        self.set_node_load(0, load);
    }

    /// Advance all four chips one master clock and every node with them.
    pub fn tick(&mut self) {
        let mut mixed = [0.0f32; 4];
        let mut conductance = [0.0f64; 4];
        for (chip, &node) in self.chips.iter_mut().zip(&self.node_of) {
            let (m, g) = chip.clock();
            mixed[node] += m;
            conductance[node] += g;
        }
        for node in 0..self.node_count {
            let sample = match &mut self.output_stages[node] {
                Some(stage) => stage.step(conductance[node]) as f32,
                None => mixed[node] / (60.0 * self.chips_on(node) as f32),
            };
            self.resamplers[node].tick(sample);
        }
    }

    /// Take node `node`'s resampled output accumulated since the last call.
    pub fn drain_node(&mut self, node: usize) -> Vec<f32> {
        self.resamplers[node].drain_audio()
    }

    /// [`Self::drain_node`] on node 0, the only node when all four chips
    /// share one.
    pub fn drain_audio(&mut self) -> Vec<f32> {
        self.drain_node(0)
    }

    /// Reset all four chips and put every node back at rest.
    pub fn reset(&mut self) {
        for chip in &mut self.chips {
            chip.reset();
        }
        for (resampler, stage) in self.resamplers.iter_mut().zip(&mut self.output_stages) {
            resampler.reset();
            if let Some(stage) = stage {
                stage.rest();
            }
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

    /// Settled output of each node, with channel 1 of the listed chips
    /// volume-only at 15, chips 0 to 2 on node 0 and chip 3 on node 1, each
    /// node on 220 ohm to +5 V.
    fn split_drops(chips: &[usize]) -> (f32, f32) {
        let mut quad = QuadPokey::with_clock(CLOCK, RATE);
        quad.set_nodes([0, 0, 0, 1]);
        let load = PokeyLoad::PullUp(PokeyOutputNetwork {
            pullup_ohms: 220.0,
            supply_v: 5.0,
            load_farads: 0.0,
        });
        quad.set_node_load(0, load);
        quad.set_node_load(1, load);
        for &n in chips {
            let p = quad.chip_mut(n);
            p.write(0x0F, 0x03);
            p.write(0x01, 0x1F); // AUDC1: volume only, 15
        }
        for _ in 0..20_000 {
            quad.tick();
        }
        let a = *quad.drain_node(0).last().unwrap();
        let b = *quad.drain_node(1).last().unwrap();
        (a, b)
    }

    /// A chip on its own node does not load the others: chip 3 playing
    /// leaves node 0 exactly where chip 0 alone puts it, and node 1 carries
    /// chip 3 alone. With all four on one node, chip 3 would compress chip 0.
    #[test]
    fn a_chip_on_its_own_node_does_not_load_the_others() {
        let (alone, silent) = split_drops(&[0]);
        let (with_three, three) = split_drops(&[0, 3]);
        assert_eq!(silent, 0.0);
        assert!(alone > 0.05, "{alone}");
        assert!(
            (with_three - alone).abs() < 1e-6,
            "{with_three} against {alone}"
        );
        assert!((three - alone).abs() < 1e-6, "chip 3 on node 1: {three}");
    }

    /// Nodes must be numbered without gaps.
    #[test]
    #[should_panic(expected = "node 1 has no chip")]
    fn a_node_without_a_chip_is_rejected() {
        QuadPokey::with_clock(CLOCK, RATE).set_nodes([0, 0, 2, 2]);
    }

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
