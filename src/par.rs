//! Threads for independent pieces of work, with the standard library only.
//!
//! Results never depend on the number of threads: work is cut into the same pieces whatever the count, and
//! the results are put together in their original order. A `Pool` is shared by everything one job does
//! (the streams of a PDF and the segments of each stream), so the job never runs more threads than asked.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

/// The threads one job may use at the same time. The calling thread always works; the pool counts the
/// others that may join it.
#[derive(Debug)]
pub struct Pool {
    free: AtomicUsize,
    threads: usize,
}

impl Pool {
    /// A pool for `threads` threads in all (at least 1: the caller).
    pub fn new(threads: usize) -> Arc<Pool> {
        let threads = threads.max(1);
        Arc::new(Pool { free: AtomicUsize::new(threads - 1), threads })
    }

    fn take(&self) -> bool {
        self.free.fetch_update(Ordering::AcqRel, Ordering::Acquire, |f| f.checked_sub(1)).is_ok()
    }

    fn give(&self) {
        self.free.fetch_add(1, Ordering::AcqRel);
    }
}

/// The default thread count: what the machine offers, at most 8.
pub fn default_threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8)
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
/// calling thread does everything. Free threads join while work is left, so a pool slot released by
/// another part of the job is picked up at the next piece.
pub fn map<R: Send>(pool: Option<&Pool>, n: usize, order: Option<&[usize]>, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    debug_assert!(order.map_or(true, |o| o.len() == n));
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
    std::thread::scope(|s| loop {
        let k = next.fetch_add(1, Ordering::Relaxed);
        if k >= n {
            break;
        }
        // Someone else can take what is left after this piece.
        if k + 1 < n && pool.take() {
            let slot = Slot(pool);
            s.spawn(move || {
                let _slot = slot;
                help();
            });
        }
        run(k);
    });
    results.into_iter().map(|m| m.into_inner().unwrap_or_else(|e| e.into_inner()).expect("every piece is done")).collect()
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
mod tests {
    use super::*;

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
            memo.get((i % 4) as u32, |_| true, || {
                calls.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(2));
                (i % 4) as u64 * 100
            })
        });
        assert_eq!(got, (0..40).map(|i| (i % 4) as u64 * 100).collect::<Vec<_>>());
        assert_eq!(calls.load(Ordering::SeqCst), 4);
        // A remembered value that doesn't fit is not used.
        assert_eq!(memo.get(1, |_| false, || 5), 5);
    }

    #[test]
    fn memo_survives_a_panicking_computation() {
        let memo: Memo<u32, u32> = Memo::new();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| memo.get(1, |_| true, || panic!("test panic"))));
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
