//! Threads for independent pieces of work, with the standard library only.
//!
//! Results never depend on the number of threads: work is cut into the same pieces whatever the count, and
//! the results are put together in their original order. A `Pool` is shared by everything one job does
//! (the streams of a PDF and the segments of each stream), so the job never runs more threads than asked.
//! A thread the system refuses (thread or process limits) is no loss: the calling thread does the work.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{Scope, ScopedJoinHandle};

/// The threads one job may use at the same time. The calling thread always works; the pool counts the
/// others that may join it.
#[derive(Debug)]
pub struct Pool {
    free: AtomicUsize,
    threads: usize,
    /// Set when the system refused a thread: the job goes on with the threads it has.
    refused: AtomicBool,
}

impl Pool {
    /// A pool for `threads` threads in all (at least 1: the caller).
    pub fn new(threads: usize) -> Arc<Pool> {
        let threads = threads.max(1);
        Arc::new(Pool { free: AtomicUsize::new(threads - 1), threads, refused: AtomicBool::new(false) })
    }

    fn take(&self) -> bool {
        !self.refused.load(Ordering::Relaxed)
            && self.free.fetch_update(Ordering::AcqRel, Ordering::Acquire, |f| f.checked_sub(1)).is_ok()
    }

    fn give(&self) {
        self.free.fetch_add(1, Ordering::AcqRel);
    }

    /// A helper thread running `work`, if a slot is free and the system gives a thread.
    fn helper<'scope>(&'scope self, s: &'scope Scope<'scope, '_>, work: impl FnOnce() + Send + 'scope) -> bool {
        if !self.take() {
            return false;
        }
        let slot = Slot(self);
        // Refused: the closure, slot included, is dropped, so the slot goes back.
        let ok = spawn(s, move || {
            let _slot = slot;
            work();
        })
        .is_some();
        if !ok {
            self.refused.store(true, Ordering::Relaxed);
        }
        ok
    }
}

/// The default thread count: every core the machine offers.
pub fn default_threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).max(1)
}

/// Starts `f` on a new thread of scope `s`. None when the system refuses one (thread or process limits);
/// the caller then does the work itself.
pub fn spawn<'scope, T: Send + 'scope>(
    s: &'scope Scope<'scope, '_>,
    f: impl FnOnce() -> T + Send + 'scope,
) -> Option<ScopedJoinHandle<'scope, T>> {
    #[cfg(test)]
    if tests::refuse_spawn() {
        return None;
    }
    std::thread::Builder::new().spawn_scoped(s, f).ok()
}

/// Gives a pool slot back when a helper thread ends, even if its work panicked.
struct Slot<'p>(&'p Pool);

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.0.give();
    }
}

/// `f(i)` for every `i` in `0..n`, results in index order. Indices are started in the order given by
/// `order` (all of `0..n`, e.g. the biggest piece first); without a pool, or with nothing free in it, the
/// calling thread does everything. Every free thread joins at the start; later, a pool slot released by
/// another part of the job is picked up at the next piece.
pub fn map<R: Send>(pool: Option<&Pool>, n: usize, order: Option<&[usize]>, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    debug_assert!(order.is_none_or(|o| o.len() == n));
    let at = |k: usize| order.map_or(k, |o| o[k]);
    // One thread (as in a browser build): plain sequential work, no thread machinery at all.
    let pool = match pool {
        Some(p) if n > 1 && p.threads > 1 => p,
        _ => {
            let mut out: Vec<Option<R>> = (0..n).map(|_| None).collect();
            for k in 0..n {
                let i = at(k);
                out[i] = Some(f(i));
            }
            return out.into_iter().map(|r| r.expect("every piece is done")).collect();
        }
    };
    let next = AtomicUsize::new(0);
    let results: Vec<Mutex<Option<R>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let run = |k: usize| {
        let i = at(k);
        let r = f(i);
        *results[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
    };
    let help = || loop {
        let k = next.fetch_add(1, Ordering::Relaxed);
        if k >= n {
            break;
        }
        run(k);
    };
    std::thread::scope(|s| {
        // As many helpers as the pool gives now (the caller takes a piece too).
        let mut helpers = 0;
        while helpers + 1 < n && pool.helper(s, help) {
            helpers += 1;
        }
        loop {
            let k = next.fetch_add(1, Ordering::Relaxed);
            if k >= n {
                break;
            }
            // A slot given back since then: someone else can take what is left after this piece.
            if k + 1 < n {
                pool.helper(s, help);
            }
            run(k);
        }
    });
    results
        .into_iter()
        .map(|m| m.into_inner().unwrap_or_else(|e| e.into_inner()).expect("every piece is done"))
        .collect()
}

// ---------------------------------------------------------------- identical inputs done once

enum State<V> {
    Pending,
    Done(V),
    /// The thread computing it panicked: whoever waits computes it again.
    Failed,
}

struct Entry<V> {
    state: Mutex<State<V>>,
    ready: Condvar,
}

/// Remembers results by key so that identical work is done once, also when two threads ask at the same time
/// (the second waits for the first). The caller checks that a remembered result fits its input; `get`
/// takes that check, and a result that fails it is computed again.
pub struct Memo<K, V> {
    map: Mutex<HashMap<K, Arc<Entry<V>>>>,
}

impl<K: std::hash::Hash + Eq + Clone, V: Clone> Memo<K, V> {
    pub fn new() -> Memo<K, V> {
        Memo { map: Mutex::new(HashMap::new()) }
    }

    /// The remembered value for `key` when `fits` accepts it, else `compute()` (remembered for next time).
    pub fn get(&self, key: K, fits: impl Fn(&V) -> bool, compute: impl FnOnce() -> V) -> V {
        let (entry, mine) = {
            let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
            match map.get(&key) {
                Some(e) => (e.clone(), false),
                None => {
                    let e = Arc::new(Entry { state: Mutex::new(State::Pending), ready: Condvar::new() });
                    map.insert(key.clone(), e.clone());
                    (e, true)
                }
            }
        };
        if !mine {
            let mut st = entry.state.lock().unwrap_or_else(|e| e.into_inner());
            while matches!(*st, State::Pending) {
                st = entry.ready.wait(st).unwrap_or_else(|e| e.into_inner());
            }
            if let State::Done(v) = &*st {
                if fits(v) {
                    return v.clone();
                }
            }
            drop(st);
            return compute();
        }
        // Marks the entry failed if `compute` panics, so nobody waits forever.
        struct Guard<'e, V> {
            e: &'e Entry<V>,
            done: bool,
        }
        impl<V> Drop for Guard<'_, V> {
            fn drop(&mut self) {
                if !self.done {
                    *self.e.state.lock().unwrap_or_else(|e| e.into_inner()) = State::Failed;
                    self.e.ready.notify_all();
                }
            }
        }
        let mut g = Guard { e: &entry, done: false };
        let v = compute();
        *entry.state.lock().unwrap_or_else(|e| e.into_inner()) = State::Done(v.clone());
        g.done = true;
        entry.ready.notify_all();
        v
    }
}

impl<K: std::hash::Hash + Eq + Clone, V: Clone> Default for Memo<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

/// A fast 64-bit hash of bytes (for finding identical inputs; matches are always checked).
pub fn hash64(d: &[u8]) -> u64 {
    const M: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut h = (d.len() as u64).wrapping_mul(M) ^ 0x2545_F491_4F6C_DD1D;
    let mut chunks = d.chunks_exact(8);
    for c in &mut chunks {
        let v = u64::from_le_bytes(c.try_into().unwrap());
        h = (h ^ v).wrapping_mul(M).rotate_left(29);
    }
    for &b in chunks.remainder() {
        h = (h ^ b as u64).wrapping_mul(M).rotate_left(29);
    }
    h ^= h >> 32;
    h = h.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    h ^ (h >> 32)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        /// Threads this thread may still start before `spawn` acts as if the system refused (tests only).
        static SPAWNS_LEFT: Cell<usize> = const { Cell::new(usize::MAX) };
    }

    /// While it lives, `spawn` called from this thread succeeds `n` more times, then fails like a system at
    /// its thread limit.
    pub(crate) struct RefuseSpawns;

    impl RefuseSpawns {
        pub(crate) fn after(n: usize) -> RefuseSpawns {
            SPAWNS_LEFT.with(|c| c.set(n));
            RefuseSpawns
        }
    }

    impl Drop for RefuseSpawns {
        fn drop(&mut self) {
            SPAWNS_LEFT.with(|c| c.set(usize::MAX));
        }
    }

    pub(super) fn refuse_spawn() -> bool {
        SPAWNS_LEFT.with(|c| match c.get() {
            0 => true,
            usize::MAX => false,
            n => {
                c.set(n - 1);
                false
            }
        })
    }

    #[test]
    fn a_refused_thread_is_no_loss() {
        let want: Vec<usize> = (0..60).map(|i| (0..5).map(|j| i * 10 + j).sum()).collect();
        for allowed in [0usize, 1, 2] {
            let _refuse = RefuseSpawns::after(allowed);
            // Flat work: only this thread asks for helpers, and with three slots and fewer threads allowed one
            // of its asks is refused for certain, whatever the timing.
            let pool = Pool::new(4);
            let got = map(Some(&pool), 60, None, |i| (0..5).map(|j| i * 10 + j).sum::<usize>());
            assert_eq!(got, want, "{allowed} threads allowed");
            // Every slot is back; the refusal stops further tries for the job.
            assert_eq!(pool.free.load(Ordering::SeqCst), 3);
            assert!(pool.refused.load(Ordering::SeqCst), "{allowed} threads allowed");
            // Nested work: the helpers ask for threads too (the hook counts this thread's asks only), so
            // whether a refusal gets recorded depends on timing; the results and the slots don't.
            let pool = Pool::new(4);
            let got = map(Some(&pool), 60, None, |i| map(Some(&pool), 5, None, |j| i * 10 + j).iter().sum::<usize>());
            assert_eq!(got, want, "{allowed} threads allowed, nested");
            assert_eq!(pool.free.load(Ordering::SeqCst), 3);
        }
        // The same through the scope: a refused spawn returns None, and the scope still ends.
        let _refuse = RefuseSpawns::after(0);
        std::thread::scope(|s| assert!(spawn(s, || 1).is_none()));
    }

    #[test]
    fn every_free_thread_joins_at_the_start() {
        // Each piece waits until 4 run at once (or 5 s): with 3 free slots, all 4 must start together.
        let pool = Pool::new(4);
        let running = Mutex::new((0usize, 0usize));
        let all_in = Condvar::new();
        map(Some(&pool), 4, None, |_| {
            let mut g = running.lock().unwrap();
            g.0 += 1;
            g.1 = g.1.max(g.0);
            all_in.notify_all();
            let (mut g, _) = all_in.wait_timeout_while(g, std::time::Duration::from_secs(5), |g| g.0 < 4).unwrap();
            g.0 -= 1;
        });
        assert_eq!(running.lock().unwrap().1, 4);
        assert_eq!(pool.free.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn map_keeps_order_with_any_thread_count() {
        let want: Vec<u64> = (0..200u64).map(|i| i * i + 7).collect();
        for threads in [1, 2, 3, 8] {
            let pool = Pool::new(threads);
            let order: Vec<usize> = (0..200).rev().collect();
            let got = map(Some(&pool), 200, Some(&order), |i| (i as u64) * (i as u64) + 7);
            assert_eq!(got, want);
            assert_eq!(map(Some(&pool), 200, None, |i| (i as u64) * (i as u64) + 7), want);
            // Every slot is back after the work.
            assert_eq!(pool.free.load(Ordering::SeqCst), threads - 1);
        }
        assert_eq!(map(None, 3, None, |i| i), vec![0, 1, 2]);
        assert!(map(None, 0, None, |i| i).is_empty());
    }

    #[test]
    fn nested_maps_share_the_pool() {
        let pool = Pool::new(3);
        let got = map(Some(&pool), 5, None, |i| map(Some(&pool), 4, None, |j| i * 10 + j).iter().sum::<usize>());
        assert_eq!(got, (0..5).map(|i| (0..4).map(|j| i * 10 + j).sum::<usize>()).collect::<Vec<_>>());
        assert_eq!(pool.free.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_panic_gives_the_slot_back() {
        let pool = Pool::new(2);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            map(Some(&pool), 10, None, |i| {
                if i == 7 {
                    panic!("test panic");
                }
                i
            })
        }));
        assert!(r.is_err());
        assert_eq!(pool.free.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn memo_computes_once_and_checks_fit() {
        let memo: Memo<u32, u64> = Memo::new();
        let calls = AtomicUsize::new(0);
        let pool = Pool::new(4);
        let got = map(Some(&pool), 40, None, |i| {
            memo.get(
                (i % 4) as u32,
                |_| true,
                || {
                    calls.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    (i % 4) as u64 * 100
                },
            )
        });
        assert_eq!(got, (0..40).map(|i| (i % 4) as u64 * 100).collect::<Vec<_>>());
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        // A remembered value that doesn't fit is not used.
        assert_eq!(memo.get(1, |_| false, || 5), 5);
    }

    #[test]
    fn memo_survives_a_panicking_computation() {
        let memo: Memo<u32, u32> = Memo::new();
        let r =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| memo.get(1, |_| true, || panic!("test panic"))));
        assert!(r.is_err());
        assert_eq!(memo.get(1, |_| true, || 9), 9);
    }

    #[test]
    fn hash_differs_for_small_changes() {
        let a = vec![0u8; 1000];
        let mut b = a.clone();
        b[999] = 1;
        assert_ne!(hash64(&a), hash64(&b));
        assert_ne!(hash64(&a[..999]), hash64(&a));
        assert_eq!(hash64(b"abc"), hash64(b"abc"));
    }
}
