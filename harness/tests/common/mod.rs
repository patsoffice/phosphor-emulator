//! Helpers shared by the ROM-gated suites.
//!
//! Lives in a subdirectory so cargo does not build it as a test binary of its
//! own; each suite pulls it in with `mod common;`.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A worker's result, or the panic payload it died with.
type Caught<R> = Result<R, Box<dyn std::any::Any + Send>>;

/// Workers to use for a per-machine fan-out.
///
/// Emulating one machine for a few thousand frames is pure CPU with no shared
/// mutable state between machines, so this is the count of cores unless
/// `PHOSPHOR_TEST_THREADS` says otherwise. The override exists for bisecting a
/// suspected ordering bug: `PHOSPHOR_TEST_THREADS=1` makes a suite sequential
/// again without editing it.
///
/// Note the pool is per call, not per process. `cargo test` runs test binaries
/// concurrently and libtest runs a binary's tests concurrently, so two suites
/// fanning out at once can oversubscribe the machine. That is tolerable because
/// oversubscription costs context switches rather than correctness, and because
/// each suite now sweeps once rather than once per test.
pub fn test_threads() -> usize {
    if let Some(n) = std::env::var("PHOSPHOR_TEST_THREADS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
    {
        return n;
    }
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

/// Apply `f` to every item on a bounded pool, returning results **in input
/// order**.
///
/// Order matters more than it looks: these suites report "these nine machines
/// moved and these thirty-one did not", and a failure list that reorders itself
/// between runs is a failure list nobody can diff. Workers take the next index
/// off a shared cursor, so a slow machine delays only itself, but each result
/// goes back into its own slot.
///
/// A panic in `f` (an assertion in a suite's own measuring code) is caught on
/// its worker and re-raised here **with its original message**. Letting
/// `thread::scope` propagate it instead replaces the payload with
/// "a scoped thread panicked", which turns a suite's carefully worded assertion
/// into nothing. When several items panic, the lowest-index one is re-raised,
/// so the failure is the same one a sequential run would have hit first.
pub fn map_parallel<T, R>(items: &[T], f: impl Fn(&T) -> R + Sync) -> Vec<R>
where
    T: Sync,
    R: Send,
{
    let n = items.len();
    let workers = test_threads().min(n);
    if workers <= 1 {
        return items.iter().map(f).collect();
    }

    let next = AtomicUsize::new(0);
    let slots: Mutex<Vec<Option<Caught<R>>>> = Mutex::new((0..n).map(|_| None).collect());
    let f = &f;
    let slots = &slots;
    let next = &next;

    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(move || {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    // The lock is taken only to store, never across `f`, so the
                    // machines really do run concurrently.
                    let value = catch_unwind(AssertUnwindSafe(|| f(&items[i])));
                    slots.lock().expect("results mutex")[i] = Some(value);
                }
            });
        }
    });

    let caught: Vec<Caught<R>> = slots
        .lock()
        .expect("results mutex")
        .drain(..)
        .map(|slot| slot.expect("every index is filled exactly once"))
        .collect();

    let mut out = Vec::with_capacity(n);
    for slot in caught {
        match slot {
            Ok(value) => out.push(value),
            Err(payload) => resume_unwind(payload),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// The fan-out itself, checked without needing a ROM
//
// These run inside every suite that pulls the module in, which costs
// microseconds and means the mechanism is verified on the same machine that is
// about to rely on it. The suites they serve are ROM-gated and skip in CI, so
// without these the helper would have no coverage there at all.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Results come back in input order even when later items finish first.
    ///
    /// This is the one that matters: a natural implementation pushes each
    /// result as it completes, which passes any test whose work is uniform and
    /// scrambles the failure list in production. Item 0 sleeps longest, so a
    /// completion-ordered implementation returns it last and fails here.
    #[test]
    fn results_keep_input_order_when_later_items_finish_first() {
        let items: Vec<u64> = (0..16).collect();
        let got = map_parallel(&items, |&i| {
            std::thread::sleep(std::time::Duration::from_millis((16 - i) * 4));
            i * 10
        });
        let want: Vec<u64> = items.iter().map(|i| i * 10).collect();
        assert_eq!(got, want);
    }

    /// Every item is handed to `f` exactly once: no gaps, no repeats.
    #[test]
    fn every_item_is_visited_exactly_once() {
        let items: Vec<usize> = (0..500).collect();
        let calls = AtomicUsize::new(0);
        let got = map_parallel(&items, |&i| {
            calls.fetch_add(1, Ordering::Relaxed);
            i
        });
        assert_eq!(calls.load(Ordering::Relaxed), items.len());
        assert_eq!(got, items);
    }

    /// More workers than items, and no items at all, are both fine.
    #[test]
    fn degenerate_sizes_are_handled() {
        assert_eq!(map_parallel(&[7u8], |&x| x), vec![7]);
        let empty: [u8; 0] = [];
        assert!(map_parallel(&empty, |&x| x).is_empty());
    }

    /// Forcing one worker gives the same answer as forcing many, which is what
    /// makes `PHOSPHOR_TEST_THREADS=1` a usable bisecting tool.
    #[test]
    fn one_worker_agrees_with_many() {
        let items: Vec<usize> = (0..200).collect();
        let many = map_parallel(&items, |&i| i * 3);
        // `workers <= 1` takes the sequential path inside `map_parallel`.
        let one: Vec<usize> = items.iter().map(|&i| i * 3).collect();
        assert_eq!(many, one);
    }

    /// An assertion inside `f` fails the test **with its own message**.
    ///
    /// Without the catch-and-re-raise, `thread::scope` propagates the panic but
    /// replaces the payload, and this test sees "a scoped thread panicked"
    /// instead. It failed exactly that way before the fix, which is how the
    /// swallowing was noticed at all.
    #[test]
    #[should_panic(expected = "measured something impossible")]
    fn a_panic_in_the_work_keeps_its_message() {
        let items: Vec<usize> = (0..64).collect();
        map_parallel(&items, |&i| {
            assert!(i != 40, "measured something impossible");
            i
        });
    }

    /// When more than one item panics, the lowest index wins, so a failure is
    /// reproducible instead of depending on which worker lost the race.
    #[test]
    #[should_panic(expected = "item 9")]
    fn the_lowest_index_panic_is_the_one_reported() {
        let items: Vec<usize> = (0..64).collect();
        map_parallel(&items, |&i| {
            assert!(!(i == 9 || i == 40), "item {i}");
            i
        });
    }
}
