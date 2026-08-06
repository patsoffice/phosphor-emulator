//! Analog input conditioning.
//!
//! Machines receive raw [`InputEvent`](super::machine::InputEvent)s — relative
//! motion, absolute deflection, held direction keys — but the hardware reads
//! something narrower: a wrapping counter a trackball game samples as a small
//! signed delta, or a clamped position an ADC digitizes. The shaping between
//! the two is the same handful of state machines repeated across every analog
//! cabinet, so it lives here.
//!
//! These are conditioners, not chips: they implement `Saveable` (their
//! accumulators are machine state that must survive a save) but deliberately
//! not `Device`, which would demand a chip designation and `Debuggable` that
//! none of them have.
//!
//! Machines keep their own "push to the ADC / POKEY" call after mutating one of
//! these. *When* the hardware samples a new value is machine-specific timing,
//! and the component must not own it.

use phosphor_macros::Saveable;

/// How a [`RelativeCounter`] moves its counter toward pending motion.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DrainPolicy {
    /// At most ±1 per call, remainder stays pending. For machines that drain
    /// from a cycle divider rather than once per frame, where the divider rate
    /// *is* the speed control.
    Unit,
    /// At most ±`max_step` per call, remainder retained — motion is never lost,
    /// only rate-limited, so a fast flick keeps rolling after the input stops.
    ClampCarry { max_step: i32 },
    /// At most ±`max_step` per call, remainder discarded — the counter stops
    /// when the physical device stops. Games reading the counter as a signed
    /// 4-bit delta need this: a larger step aliases into a stall or a reversal.
    ClampDrop { max_step: i32 },
}

/// A trackball or spinner axis: relative motion and held direction keys
/// condition a wrapping counter the game reads as a small signed delta.
#[derive(Saveable)]
#[save_version(1)]
pub struct RelativeCounter {
    counter: u8,
    pending: i32,
    neg_held: bool,
    pos_held: bool,
    /// Continuous stand-in for the held-key flags, fed by a stick deflection.
    velocity: f32,
    #[save_skip]
    mask: u8,
    #[save_skip]
    key_step: i32,
    #[save_skip]
    invert: bool,
    #[save_skip]
    policy: DrainPolicy,
}

impl RelativeCounter {
    /// `mask` is the counter width the game reads (`0xFF` for a full 8-bit
    /// counter, `0x0F` for a 4-bit one). `invert` negates motion at apply time,
    /// for cabinets wired to read an axis backwards.
    pub const fn new(mask: u8, key_step: i32, invert: bool, policy: DrainPolicy) -> Self {
        Self {
            counter: 0,
            pending: 0,
            neg_held: false,
            pos_held: false,
            velocity: 0.0,
            mask,
            key_step,
            invert,
            policy,
        }
    }

    /// Feed a relative delta in pointing-device units. Accumulates until
    /// [`update`](Self::update) drains it.
    pub fn add_delta(&mut self, delta: f32) {
        self.pending += delta as i32;
    }

    /// Set a held direction key. `pos` selects the positive end of the axis.
    pub fn set_held(&mut self, pos: bool, held: bool) {
        if pos {
            self.pos_held = held;
        } else {
            self.neg_held = held;
        }
    }

    /// Treat an absolute deflection (`-1.0..=1.0`) as a velocity rather than a
    /// position: a trackball has no center to return to, so a deflected stick
    /// should keep rolling the ball. This is the same fixed-step-per-update
    /// mechanism the direction keys use, with a continuous magnitude.
    pub fn set_velocity(&mut self, value: f32) {
        self.velocity = value;
    }

    /// Apply held keys, then drain per the policy. Call once per frame, or per
    /// cycle-divider tick for [`DrainPolicy::Unit`].
    pub fn update(&mut self) {
        match self.policy {
            // Keys move the counter directly and the pending drain is separate,
            // so a held key and buffered motion can both land in one call.
            DrainPolicy::Unit => {
                let mut step = 0;
                if self.neg_held {
                    step -= 1;
                }
                if self.pos_held {
                    step += 1;
                }
                step += self.velocity_step();
                self.apply(step);

                let drained = self.pending.signum();
                self.pending -= drained;
                self.apply(drained);
            }
            // Keys feed the accumulator, which is then rate-limited as one.
            DrainPolicy::ClampCarry { max_step } => {
                self.accumulate_keys();
                let step = self.pending.clamp(-max_step, max_step);
                self.pending -= step;
                self.apply(step);
            }
            DrainPolicy::ClampDrop { max_step } => {
                self.accumulate_keys();
                let step = self.pending.clamp(-max_step, max_step);
                self.pending = 0;
                self.apply(step);
            }
        }
    }

    fn accumulate_keys(&mut self) {
        if self.pos_held {
            self.pending += self.key_step;
        }
        if self.neg_held {
            self.pending -= self.key_step;
        }
        self.pending += self.velocity_step() * self.key_step;
    }

    /// Direction a deflected stick is rolling, as a unit step. Deliberately
    /// coarse — the magnitude only decides whether the stick counts as held.
    fn velocity_step(&self) -> i32 {
        if self.velocity > 0.25 {
            1
        } else if self.velocity < -0.25 {
            -1
        } else {
            0
        }
    }

    fn apply(&mut self, step: i32) {
        let step = if self.invert { -step } else { step };
        self.counter = (self.counter as i32).wrapping_add(step) as u8 & self.mask;
    }

    /// The value the game reads.
    pub fn counter(&self) -> u8 {
        self.counter
    }

    /// Overwrite the counter, for machines that expose it as writable state.
    pub fn set_counter(&mut self, value: u8) {
        self.counter = value & self.mask;
    }

    /// Clear pending motion and held keys, leaving the counter alone — it is
    /// hardware state, not input state, and a trackball has no rest position.
    pub fn release_all(&mut self) {
        self.pending = 0;
        self.neg_held = false;
        self.pos_held = false;
        self.velocity = 0.0;
    }
}

/// An analog axis range. `center` need not be the midpoint: I, Robot's channels
/// are deliberately asymmetric about their rest position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AxisRange {
    pub min: i32,
    pub center: i32,
    pub max: i32,
}

impl AxisRange {
    pub const fn new(min: i32, center: i32, max: i32) -> Self {
        Self { min, center, max }
    }

    /// A range centered on the midpoint of `min..=max`.
    pub const fn symmetric(min: i32, max: i32) -> Self {
        Self {
            min,
            center: (min + max) / 2,
            max,
        }
    }
}

/// A self-centering analog stick or yoke axis.
#[derive(Saveable)]
#[save_version(1)]
pub struct AnalogAxis {
    position: i32,
    neg_held: bool,
    pos_held: bool,
    #[save_skip]
    range: AxisRange,
}

impl AnalogAxis {
    pub const fn new(range: AxisRange) -> Self {
        Self {
            position: range.center,
            neg_held: false,
            pos_held: false,
            range,
        }
    }

    /// Absolute deflection in `-1.0..=1.0`, scaled independently either side of
    /// center so an asymmetric range maps correctly at both extremes.
    pub fn set_absolute(&mut self, value: f32) {
        let value = value.clamp(-1.0, 1.0);
        let span = if value >= 0.0 {
            (self.range.max - self.range.center) as f32
        } else {
            (self.range.center - self.range.min) as f32
        };
        self.position = self.range.center + (value * span).round() as i32;
        self.clamp();
    }

    /// Relative nudge, clamped to the range.
    pub fn move_relative(&mut self, delta: f32) {
        self.position += delta.round() as i32;
        self.clamp();
    }

    /// Held direction key: full deflection while held, spring-centered on
    /// release. `pos` selects the positive end of the axis.
    ///
    /// Note this assigns absolutely, so releasing a key re-centers the axis
    /// even if relative motion had moved it. That is existing behavior across
    /// every machine doing this, preserved deliberately.
    pub fn set_held(&mut self, pos: bool, held: bool) {
        if pos {
            self.pos_held = held;
        } else {
            self.neg_held = held;
        }
        self.position = if self.neg_held {
            self.range.min
        } else if self.pos_held {
            self.range.max
        } else {
            self.range.center
        };
    }

    fn clamp(&mut self) {
        self.position = self.position.clamp(self.range.min, self.range.max);
    }

    pub fn position(&self) -> i32 {
        self.position
    }

    /// Place the axis at an exact position, clamped to the range.
    ///
    /// For restoring a saved position, and for machines whose absolute mapping
    /// is not [`set_absolute`](Self::set_absolute)'s — Star Wars scales both
    /// directions by its *upper* half-span even though its center is not the
    /// midpoint, so it computes its own value and assigns it here.
    pub fn set_position(&mut self, position: i32) {
        self.position = position;
        self.clamp();
    }

    /// `min + max - position`, for cabinets wired to read the axis reversed.
    pub fn reversed(&self) -> i32 {
        self.range.min + self.range.max - self.position
    }

    pub fn release_all(&mut self) {
        self.neg_held = false;
        self.pos_held = false;
        self.position = self.range.center;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit_counter() -> RelativeCounter {
        RelativeCounter::new(0xFF, 1, false, DrainPolicy::Unit)
    }

    #[test]
    fn unit_drains_one_per_call_and_keeps_the_remainder() {
        let mut c = unit_counter();
        c.add_delta(3.0);
        for expected in [1, 2, 3, 3] {
            c.update();
            assert_eq!(c.counter(), expected);
        }
    }

    #[test]
    fn unit_applies_a_held_key_and_pending_motion_in_the_same_call() {
        // ccastles moves the counter directly from the key and drains the
        // accumulator separately, so both can land in one tick.
        let mut c = unit_counter();
        c.set_held(true, true);
        c.add_delta(5.0);
        c.update();
        assert_eq!(c.counter(), 2);
    }

    #[test]
    fn clamp_carry_rate_limits_but_never_loses_motion() {
        let mut c = RelativeCounter::new(0xFF, 1, false, DrainPolicy::ClampCarry { max_step: 4 });
        c.add_delta(10.0);
        c.update();
        assert_eq!(c.counter(), 4);
        c.update();
        assert_eq!(c.counter(), 8);
        c.update();
        assert_eq!(c.counter(), 10, "the remainder must still arrive");
        c.update();
        assert_eq!(c.counter(), 10, "and then stop");
    }

    #[test]
    fn clamp_drop_discards_the_remainder() {
        let mut c = RelativeCounter::new(0x0F, 3, false, DrainPolicy::ClampDrop { max_step: 7 });
        c.add_delta(100.0);
        c.update();
        assert_eq!(c.counter(), 7);
        c.update();
        assert_eq!(c.counter(), 7, "excess is dropped, not carried");
    }

    #[test]
    fn mask_wraps_at_the_declared_width() {
        let mut c = RelativeCounter::new(0x0F, 1, false, DrainPolicy::ClampDrop { max_step: 7 });
        c.add_delta(7.0);
        c.update();
        c.add_delta(7.0);
        c.update();
        // 14 stays inside 4 bits; one more step wraps.
        assert_eq!(c.counter(), 14);
        c.add_delta(4.0);
        c.update();
        assert_eq!(c.counter(), 2);
    }

    #[test]
    fn invert_negates_at_apply_time() {
        let mut a = RelativeCounter::new(0xFF, 1, false, DrainPolicy::ClampCarry { max_step: 8 });
        let mut b = RelativeCounter::new(0xFF, 1, true, DrainPolicy::ClampCarry { max_step: 8 });
        a.add_delta(5.0);
        b.add_delta(5.0);
        a.update();
        b.update();
        assert_eq!(a.counter(), 5);
        assert_eq!(b.counter(), 251);
    }

    #[test]
    fn velocity_rolls_the_counter_like_a_held_key() {
        let mut c = RelativeCounter::new(0xFF, 2, false, DrainPolicy::ClampCarry { max_step: 8 });
        c.set_velocity(0.9);
        c.update();
        c.update();
        assert_eq!(c.counter(), 4, "two updates at one key_step each");

        // Inside the dead band a deflection is not motion.
        let mut c = RelativeCounter::new(0xFF, 2, false, DrainPolicy::ClampCarry { max_step: 8 });
        c.set_velocity(0.1);
        c.update();
        assert_eq!(c.counter(), 0);
    }

    #[test]
    fn release_all_clears_input_but_keeps_the_counter() {
        let mut c = unit_counter();
        c.add_delta(9.0);
        c.update();
        c.set_held(true, true);
        c.set_velocity(1.0);
        c.release_all();
        let held = c.counter();
        c.update();
        assert_eq!(c.counter(), held, "nothing left to drain or hold");
    }

    #[test]
    fn analog_axis_springs_back_to_center() {
        let mut a = AnalogAxis::new(AxisRange::symmetric(0x10, 0xF0));
        assert_eq!(a.position(), 0x80);
        a.set_held(false, true);
        assert_eq!(a.position(), 0x10);
        a.set_held(false, false);
        assert_eq!(a.position(), 0x80);
    }

    #[test]
    fn analog_axis_scales_asymmetrically_about_center() {
        // I, Robot's channels are not centered on their midpoint.
        let mut a = AnalogAxis::new(AxisRange::new(0x00, 0x40, 0xFF));
        a.set_absolute(1.0);
        assert_eq!(a.position(), 0xFF);
        a.set_absolute(-1.0);
        assert_eq!(a.position(), 0x00);
        // Rounded, not truncated: half of the 0xBF upper span is 95.5.
        a.set_absolute(0.5);
        assert_eq!(a.position(), 0x40 + 96);
        a.set_absolute(-0.5);
        assert_eq!(a.position(), 0x40 - 32);
    }

    #[test]
    fn analog_axis_relative_motion_clamps_to_the_range() {
        let mut a = AnalogAxis::new(AxisRange::symmetric(0x10, 0xF0));
        a.move_relative(1000.0);
        assert_eq!(a.position(), 0xF0);
        a.move_relative(-10_000.0);
        assert_eq!(a.position(), 0x10);
    }

    #[test]
    fn analog_axis_reversed_mirrors_about_the_range() {
        let mut a = AnalogAxis::new(AxisRange::symmetric(0x10, 0xF0));
        a.set_held(true, true);
        assert_eq!(a.position(), 0xF0);
        assert_eq!(a.reversed(), 0x10);
    }
}
