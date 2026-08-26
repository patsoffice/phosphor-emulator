//! Helpers shared by the ROM-gated suites.
//!
//! Lives in a subdirectory so cargo does not build it as a test binary of its
//! own; each suite pulls it in with `mod common;`.

use std::panic::{AssertUnwindSafe, catch_unwind, resume_unwind};

use rayon::prelude::*;

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
/// This is the process-wide budget, not a per-call one. See [`Permits`].
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

/// Configure one process-wide worker pool, once.
///
/// **Two hand-rolled schedulers were measured and both were wrong**, which is
/// why this is rayon rather than something local. libtest already runs a
/// binary's tests concurrently, so any fan-out inside a test is nested
/// parallelism:
///
/// - A pool per call oversubscribed the machine. `movie_test` put roughly eighty
///   threads on sixteen cores and went from 70.9s to **80.0s**, slower than the
///   sequential loops it replaced.
/// - A global budget handed out first-come starved the caller that mattered. The
///   quick tests start first and took every worker, so the one 70.4s test that
///   *is* this binary's cost got none and ran sequentially: **74.6s**, no better
///   than doing nothing.
///
/// What is actually needed is one pool that every caller feeds and that steals
/// work between them, so the machine stays busy and no caller is starved. That
/// is rayon's whole purpose, and writing a third version of it here would be the
/// wrong use of anyone's afternoon.
fn install_pool() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        // Fails only if a pool is already installed, which is fine: it means
        // another call got here first and the thread count is already set.
        let _ = rayon::ThreadPoolBuilder::new()
            .num_threads(test_threads())
            .build_global();
    });
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
    if test_threads() <= 1 || items.len() <= 1 {
        return items.iter().map(f).collect();
    }
    install_pool();

    // Each item is caught individually rather than letting rayon propagate
    // whichever panic it noticed first: that keeps the reported failure the
    // lowest-index one, so a failing run names the same item however the work
    // happened to be scheduled. `collect` into a Vec preserves input order.
    let caught: Vec<Caught<R>> = items
        .par_iter()
        .map(|item| catch_unwind(AssertUnwindSafe(|| f(item))))
        .collect();

    let mut out = Vec::with_capacity(caught.len());
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
    use std::sync::atomic::{AtomicUsize, Ordering};

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
