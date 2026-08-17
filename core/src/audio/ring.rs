//! A bounded circular sample buffer.
//!
//! Audio output is a producer/consumer queue: the emulator pushes samples as it
//! runs and the host drains a slice of them once per frame. A `Vec` drained
//! with `drain(..n)` gets that job done but shifts every remaining element down
//! on each call, so the cost of a drain scales with the backlog that was *not*
//! consumed. [`SampleRing`] is the same queue with head/tail indices, so a push
//! is O(1) and a drain is O(number of samples actually taken).
//!
//! Capacity grows by doubling up to [`SampleRing::MAX_CAPACITY`]. Past that the
//! ring drops its oldest samples and counts the loss in
//! [`SampleRing::overruns`] — a producer that outruns its consumer for seconds
//! at a time is a bug, and the counter is there so it is visible rather than an
//! unbounded allocation.

/// Bounded circular buffer of audio samples, oldest-first.
#[derive(Debug, Clone)]
pub struct SampleRing<T: Copy> {
    /// Backing storage. `buf.len()` is the capacity and is always a power of
    /// two, so wrapping is a mask rather than a modulo.
    buf: Vec<T>,
    /// Index of the oldest sample.
    head: usize,
    /// Number of samples currently held.
    len: usize,
    /// Samples dropped because the ring was full at `MAX_CAPACITY`.
    overruns: u64,
}

impl<T: Copy + Default> Default for SampleRing<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy + Default> SampleRing<T> {
    /// Initial capacity, in samples. At 44.1 kHz and ~60 Hz this is roughly
    /// five frames of audio, so the steady state never reallocates.
    pub const DEFAULT_CAPACITY: usize = 4096;

    /// Ceiling on growth, in samples — about three seconds at 44.1 kHz. A
    /// consumer that has not drained in three seconds is not coming back, so
    /// beyond this the ring drops oldest rather than growing without bound.
    pub const MAX_CAPACITY: usize = 1 << 17;

    /// Create an empty ring with [`Self::DEFAULT_CAPACITY`].
    pub fn new() -> Self {
        Self::with_capacity(Self::DEFAULT_CAPACITY)
    }

    /// Create an empty ring holding at least `capacity` samples. The actual
    /// capacity is rounded up to a power of two and clamped to
    /// [`Self::MAX_CAPACITY`].
    pub fn with_capacity(capacity: usize) -> Self {
        // Clamp before rounding: `next_power_of_two` overflows near `usize::MAX`,
        // and `MAX_CAPACITY` is itself a power of two so the order is equivalent.
        let capacity = capacity.clamp(1, Self::MAX_CAPACITY).next_power_of_two();
        Self {
            buf: vec![T::default(); capacity],
            head: 0,
            len: 0,
            overruns: 0,
        }
    }

    /// Number of samples currently held.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the ring holds no samples.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Current capacity in samples.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Total samples dropped because the ring was full at
    /// [`Self::MAX_CAPACITY`]. Non-zero means the consumer is not keeping up.
    #[inline]
    pub fn overruns(&self) -> u64 {
        self.overruns
    }

    /// Append one sample, growing the ring if it is full and under
    /// [`Self::MAX_CAPACITY`], otherwise dropping the oldest sample.
    #[inline]
    pub fn push(&mut self, sample: T) {
        if self.len == self.buf.len() {
            if self.buf.len() < Self::MAX_CAPACITY {
                self.grow();
            } else {
                // Full at the ceiling: drop the oldest to make room.
                self.head = (self.head + 1) & (self.buf.len() - 1);
                self.len -= 1;
                self.overruns += 1;
            }
        }
        let tail = (self.head + self.len) & (self.buf.len() - 1);
        self.buf[tail] = sample;
        self.len += 1;
    }

    /// Copy up to `out.len()` samples, oldest first, into `out` and remove
    /// them. Returns the number written.
    pub fn pop_front_into(&mut self, out: &mut [T]) -> usize {
        let n = out.len().min(self.len);
        let mask = self.buf.len() - 1;
        // The samples may wrap the end of the backing store, in which case they
        // are two contiguous runs rather than one.
        let first = n.min(self.buf.len() - self.head);
        out[..first].copy_from_slice(&self.buf[self.head..self.head + first]);
        if first < n {
            out[first..n].copy_from_slice(&self.buf[..n - first]);
        }
        self.head = (self.head + n) & mask;
        self.len -= n;
        n
    }

    /// Remove and return every held sample, oldest first.
    pub fn drain_all(&mut self) -> Vec<T> {
        let mut out = vec![T::default(); self.len];
        self.pop_front_into(&mut out);
        out
    }

    /// Discard all held samples and the overrun count.
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
        self.overruns = 0;
    }

    /// Double the capacity, re-linearising the contents so `head` is 0 again.
    #[cold]
    fn grow(&mut self) {
        let mut next = vec![T::default(); (self.buf.len() * 2).min(Self::MAX_CAPACITY)];
        let first = self.len.min(self.buf.len() - self.head);
        next[..first].copy_from_slice(&self.buf[self.head..self.head + first]);
        next[first..self.len].copy_from_slice(&self.buf[..self.len - first]);
        self.buf = next;
        self.head = 0;
    }
}

impl<T: Copy + Default> Extend<T> for SampleRing<T> {
    fn extend<I: IntoIterator<Item = T>>(&mut self, iter: I) {
        for sample in iter {
            self.push(sample);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_drain_preserve_order() {
        let mut r = SampleRing::<i16>::with_capacity(4);
        r.extend([1, 2, 3]);
        assert_eq!(r.len(), 3);
        assert_eq!(r.drain_all(), vec![1, 2, 3]);
        assert!(r.is_empty());
    }

    #[test]
    fn pop_front_into_takes_a_prefix() {
        let mut r = SampleRing::<i16>::with_capacity(8);
        r.extend([10, 20, 30, 40]);

        let mut out = [0i16; 2];
        assert_eq!(r.pop_front_into(&mut out), 2);
        assert_eq!(out, [10, 20]);
        assert_eq!(r.len(), 2);

        let mut rest = [0i16; 8];
        assert_eq!(r.pop_front_into(&mut rest), 2);
        assert_eq!(&rest[..2], &[30, 40]);
        assert_eq!(r.pop_front_into(&mut rest), 0);
    }

    #[test]
    fn contents_survive_wrapping_the_backing_store() {
        // Fill, drain part, refill: the live samples now straddle the end of
        // the buffer, which is the case a naive slice copy gets wrong.
        let mut r = SampleRing::<i16>::with_capacity(4);
        r.extend([1, 2, 3, 4]);
        let mut out = [0i16; 3];
        r.pop_front_into(&mut out);
        r.extend([5, 6, 7]);
        assert_eq!(r.capacity(), 4, "should not have grown");
        assert_eq!(r.drain_all(), vec![4, 5, 6, 7]);
    }

    #[test]
    fn growth_doubles_and_keeps_every_sample() {
        let mut r = SampleRing::<i16>::with_capacity(2);
        r.extend(0..1000);
        assert!(r.capacity() >= 1000);
        assert_eq!(r.overruns(), 0);
        assert_eq!(r.drain_all(), (0..1000).collect::<Vec<i16>>());
    }

    #[test]
    fn growth_relinearises_a_wrapped_ring() {
        let mut r = SampleRing::<i16>::with_capacity(4);
        r.extend([1, 2, 3, 4]);
        let mut out = [0i16; 2];
        r.pop_front_into(&mut out);
        r.extend([5, 6, 7, 8]); // wraps, then grows on the last push
        assert_eq!(r.drain_all(), vec![3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn overrun_at_the_ceiling_drops_oldest_and_counts() {
        let cap = SampleRing::<i16>::MAX_CAPACITY;
        let mut r = SampleRing::<i16>::with_capacity(cap);
        for i in 0..cap {
            r.push(i as i16);
        }
        assert_eq!(r.overruns(), 0);

        r.push(-1);
        assert_eq!(r.len(), cap, "capacity is the ceiling");
        assert_eq!(r.overruns(), 1);

        let held = r.drain_all();
        assert_eq!(held[0], 1, "oldest sample was the one dropped");
        assert_eq!(*held.last().unwrap(), -1);
    }

    #[test]
    fn with_capacity_rounds_up_and_clamps() {
        assert_eq!(SampleRing::<i16>::with_capacity(0).capacity(), 1);
        assert_eq!(SampleRing::<i16>::with_capacity(100).capacity(), 128);
        assert_eq!(
            SampleRing::<i16>::with_capacity(usize::MAX / 2).capacity(),
            SampleRing::<i16>::MAX_CAPACITY
        );
    }

    #[test]
    fn clear_empties_the_ring() {
        let mut r = SampleRing::<f32>::with_capacity(4);
        r.extend([1.0, 2.0]);
        r.clear();
        assert!(r.is_empty());
        assert_eq!(r.overruns(), 0);
    }
}
