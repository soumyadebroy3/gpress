//! Threads for independent pieces of work, with the standard library only.
//!
//! Results never depend on the number of threads: work is cut into the same pieces whatever the count, and
//! the results are put together in their original order. A `Pool` is shared by everything one job does
//! (the streams of a PDF and the segments of each stream), so the job never runs more threads than asked.
//! A thread the system refuses (thread or process limits) is no loss: the calling thread does the work.

use std::cell::Cell;
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
    /// Callers waiting for the slot they lent (see `map`), and slots handed to them.
    lent: Mutex<(usize, usize)>,
    back: Condvar,
}

impl Pool {
    /// A pool for `threads` threads in all (at least 1: the caller).
    pub fn new(threads: usize) -> Arc<Pool> {
        let threads = threads.max(1);
        Arc::new(Pool {
            free: AtomicUsize::new(threads - 1),
            threads,
            refused: AtomicBool::new(false),
            lent: Mutex::new((0, 0)),
            back: Condvar::new(),
        })
    }

    fn take(&self) -> bool {
        !self.refused.load(Ordering::Relaxed) && self.take_free()
    }

    /// Up to `most` free slots at once (none after a refused thread).
    fn take_up_to(&self, most: usize) -> usize {
        if self.refused.load(Ordering::Relaxed) {
            return 0;
        }
        let mut free = self.free.load(Ordering::Acquire);
        while free > 0 && most > 0 {
            let got = free.min(most);
            match self.free.compare_exchange_weak(free, free - got, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return got,
                Err(now) => free = now,
            }
        }
        0
    }

    /// A free slot, if there is one (also after a refused thread).
    fn take_free(&self) -> bool {
        // One slot less, unless none is free (a plain compare-and-swap loop).
        let mut free = self.free.load(Ordering::Acquire);
        while free > 0 {
            match self.free.compare_exchange_weak(free, free - 1, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return true,
                Err(now) => free = now,
            }
        }
        false
    }

    fn give(&self) {
        self.free.fetch_add(1, Ordering::AcqRel);
    }

    /// Gives a slot back: to a caller waiting for the one it lent first, else to the pool.
    fn release(&self) {
        let mut lent = self.lent.lock().unwrap_or_else(|e| e.into_inner());
        if lent.0 > 0 {
            lent.0 -= 1;
            lent.1 += 1;
            self.back.notify_one();
        } else {
            self.give();
        }
    }

    /// Takes back the slot a caller lent: a free one, else the next one given back. (While a caller waits
    /// here no slot is free, since `release` hands every slot to the waiting callers first.)
    fn take_back(&self) {
        #[cfg(test)]
        jitter::pause();
        let mut lent = self.lent.lock().unwrap_or_else(|e| e.into_inner());
        if self.take_free() {
            return;
        }
        lent.0 += 1;
        while lent.1 == 0 {
            lent = self.back.wait(lent).unwrap_or_else(|e| e.into_inner());
        }
        lent.1 -= 1;
    }

    /// A helper thread running `work`, if a slot is free and the system gives a thread.
    fn helper<'scope>(&'scope self, s: &'scope Scope<'scope, '_>, work: impl FnOnce() + Send + 'scope) -> bool {
        self.take() && self.start(s, work)
    }

    /// A helper thread running `work` on a slot already taken, if the system gives a thread (else the slot
    /// goes back).
    fn start<'scope>(&'scope self, s: &'scope Scope<'scope, '_>, work: impl FnOnce() + Send + 'scope) -> bool {
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

/// The default thread count: every core the machine offers, at most `api::MAX_THREADS` (what a caller may
/// ask for; memory grows with the threads, each working on a decoded stream of its own).
pub fn default_threads() -> usize {
    threads_for(std::thread::available_parallelism().ok().map(|n| n.get()))
}

/// The thread count for a machine that offers `cores` (unknown: 1), within 1 to `api::MAX_THREADS`.
fn threads_for(cores: Option<usize>) -> usize {
    cores.unwrap_or(1).clamp(1, crate::api::MAX_THREADS)
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
        self.0.release();
    }
}

/// Takes back the slot a `map` caller lent once its helpers are done, also when a piece panicked.
struct Lend<'p>(&'p Pool, Cell<bool>);

impl Drop for Lend<'_> {
    fn drop(&mut self) {
        if self.1.get() {
            self.0.take_back();
        }
    }
}

/// `f(i)` for every `i` in `0..n`, results in index order. Indices are started in the order given by
/// `order` (all of `0..n`, e.g. the biggest piece first); without a pool, or with nothing free in it, the
/// calling thread does everything. Every free thread joins at the start; later, a pool slot released by
/// another part of the job is picked up at the next piece. A caller with nothing left to start lends its
/// slot to the rest of the job while its helpers finish, and takes a slot back before it goes on, so the
/// job never runs more threads than its pool has.
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
    let done = AtomicUsize::new(0);
    let results: Vec<Mutex<Option<R>>> = (0..n).map(|_| Mutex::new(None)).collect();
    let run = |k: usize| {
        #[cfg(test)]
        jitter::pause();
        let i = at(k);
        let r = f(i);
        *results[i].lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
        done.fetch_add(1, Ordering::Release);
    };
    let help = || loop {
        let k = next.fetch_add(1, Ordering::Relaxed);
        if k >= n {
            break;
        }
        run(k);
    };
    let lend = Lend(pool, Cell::new(false));
    std::thread::scope(|s| {
        // As many helpers as the pool gives now (the caller takes a piece too). Their slots are all taken
        // before the first helper starts, so a nested map in a helper's piece finds none free: no slot can be
        // lent back to this map while it has pieces left to start, and a map started on the whole pool (the
        // streams of a file) never has more pieces in flight than the pool has threads.
        let mut slots = pool.take_up_to(n - 1);
        while slots > 0 {
            slots -= 1;
            if !pool.start(s, help) {
                // The system refused a thread: the slots not used go back too.
                (0..slots).for_each(|_| pool.release());
                break;
            }
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
        // Nothing left to start. While the helpers finish their pieces, the caller's slot works elsewhere in
        // the job (the scope waits for them here, and `lend` takes a slot back after it).
        if done.load(Ordering::Acquire) < n {
            lend.1.set(true);
            pool.release();
        }
    });
    drop(lend);
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
    let (chunks, rest) = d.as_chunks::<8>();
    for c in chunks {
        let v = u64::from_le_bytes(*c);
        h = (h ^ v).wrapping_mul(M).rotate_left(29);
    }
    for &b in rest {
        h = (h ^ b as u64).wrapping_mul(M).rotate_left(29);
    }
    h ^= h >> 32;
    h = h.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    h ^ (h >> 32)
}

/// Random pauses in the pool, so that tests see many orders of events (tests only; nothing in a release
/// build).
#[cfg(test)]
pub(crate) mod jitter {
    use std::hash::{DefaultHasher, Hash, Hasher};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    static SEED: AtomicU64 = AtomicU64::new(0);
    static COUNT: AtomicU64 = AtomicU64::new(0);
    static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

    /// Runs `f` with random pauses in the pool (seed 0: none); one jitter test at a time.
    pub(crate) fn with<R>(seed: u64, f: impl FnOnce() -> R) -> R {
        let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                SEED.store(0, Ordering::SeqCst);
            }
        }
        let _off = Off;
        SEED.store(seed, Ordering::SeqCst);
        f()
    }

    /// With a seed set: a pause of 0 to 200 us, drawn from the seed, the thread and a counter.
    pub(super) fn pause() {
        let seed = SEED.load(Ordering::Relaxed);
        if seed == 0 {
            return;
        }
        let mut h = DefaultHasher::new();
        std::thread::current().id().hash(&mut h);
        let mut x = seed ^ h.finish() ^ COUNT.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        for _ in 0..2 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
        }
        std::thread::sleep(std::time::Duration::from_micros(x % 201));
    }
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
    fn the_default_is_every_core_within_the_limit() {
        use crate::api::MAX_THREADS;
        assert_eq!(threads_for(Some(4)), 4);
        assert_eq!(threads_for(Some(MAX_THREADS)), MAX_THREADS);
        assert_eq!(threads_for(Some(MAX_THREADS + 1)), MAX_THREADS);
        assert_eq!(threads_for(Some(1024)), MAX_THREADS);
        assert_eq!(threads_for(None), 1);
        assert_eq!(threads_for(Some(0)), 1);
        // On this machine: every core it offers, as many as the options accept.
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
        assert_eq!(default_threads(), cores.min(MAX_THREADS));
        assert!(crate::Options::new().threads(default_threads()).check().is_ok());
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

    /// Every slot is back in the pool and nobody waits for one.
    fn all_back(pool: &Pool) {
        assert_eq!(pool.free.load(Ordering::SeqCst), pool.threads - 1);
        assert_eq!(*pool.lent.lock().unwrap(), (0, 0));
    }

    /// `f`, counted as running while it runs; the most at once goes to `most`.
    fn counted<R>(now: &AtomicUsize, most: &AtomicUsize, f: impl FnOnce() -> R) -> R {
        most.fetch_max(now.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
        let r = f();
        now.fetch_sub(1, Ordering::SeqCst);
        r
    }

    #[test]
    fn nested_maps_never_run_more_than_the_pool() {
        let leaf = |i: usize, j: usize, k: usize| i * 100 + j * 10 + k;
        let want: Vec<usize> =
            (0..8).map(|i| (0..4).map(|j| (0..3).map(|k| leaf(i, j, k)).sum::<usize>()).sum()).collect();
        // Later outer pieces take longer, so free slots appear while nested maps still run: callers lend, and
        // some find every slot taken when they want theirs back.
        for threads in [1, 2, 3, 4, 5, 6, 8] {
            for seed in 1..=8 {
                let pool = Pool::new(threads);
                let (now, most) = (AtomicUsize::new(0), AtomicUsize::new(0));
                let got = jitter::with(seed, || {
                    map(Some(&pool), 8, None, |i| {
                        map(Some(&pool), 4, None, |j| {
                            map(Some(&pool), 3, None, |k| {
                                counted(&now, &most, || {
                                    std::thread::sleep(std::time::Duration::from_micros(25 * (i as u64 + 1)));
                                    leaf(i, j, k)
                                })
                            })
                            .iter()
                            .sum::<usize>()
                        })
                        .iter()
                        .sum::<usize>()
                    })
                });
                assert_eq!(got, want, "{threads} threads, seed {seed}");
                let most = most.load(Ordering::SeqCst);
                assert!(most <= threads, "{most} leaf pieces at once, {threads} threads, seed {seed}");
                all_back(&pool);
            }
        }
    }

    #[test]
    fn outer_pieces_in_flight_never_exceed_the_pool() {
        // A slot lent by a caller inside an outer piece never starts another outer piece: the outer map took
        // every free slot at its start, so a slot is free only once no outer piece is left to start. (Each
        // nested map is a quick piece and a slow one, so its caller often lends while its helper works.)
        for threads in [2, 3, 4, 6, 8] {
            for seed in 1..=8 {
                let pool = Pool::new(threads);
                let (now, most) = (AtomicUsize::new(0), AtomicUsize::new(0));
                let got = jitter::with(seed, || {
                    map(Some(&pool), 60, None, |i| {
                        counted(&now, &most, || {
                            map(Some(&pool), 2, None, |j| {
                                std::thread::sleep(std::time::Duration::from_micros(200 * j as u64));
                                i * (j + 1)
                            })
                            .iter()
                            .sum::<usize>()
                        })
                    })
                });
                assert_eq!(got, (0..60).map(|i| i * 3).collect::<Vec<_>>());
                let most = most.load(Ordering::SeqCst);
                assert!(most <= threads, "{most} outer pieces at once, {threads} threads, seed {seed}");
                all_back(&pool);
            }
        }
    }

    #[test]
    fn a_lender_is_first_in_line() {
        let pool = Pool::new(2);
        assert!(pool.take());
        let back = Arc::new(AtomicBool::new(false));
        let (p, b) = (pool.clone(), back.clone());
        std::thread::spawn(move || {
            p.take_back();
            b.store(true, Ordering::SeqCst);
        });
        let t0 = std::time::Instant::now();
        while pool.lent.lock().unwrap().0 != 1 {
            assert!(t0.elapsed().as_secs() < 10, "the lender never waited");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        // The slot given back goes to the waiting lender, not to the pool.
        pool.release();
        while !back.load(Ordering::SeqCst) {
            assert!(t0.elapsed().as_secs() < 10, "the lender never got the slot");
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert_eq!(pool.free.load(Ordering::SeqCst), 0);
        assert_eq!(*pool.lent.lock().unwrap(), (0, 0));
    }

    #[test]
    fn a_panic_under_a_lent_slot_gives_every_slot_back() {
        for threads in [2, 3, 4] {
            for seed in 1..=4 {
                let pool = Pool::new(threads);
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    jitter::with(seed, || {
                        map(Some(&pool), 6, None, |i| {
                            map(Some(&pool), 5, None, |j| {
                                // The slow piece panics, mostly on a helper after its caller has lent its slot.
                                if i == 3 && j == 0 {
                                    std::thread::sleep(std::time::Duration::from_millis(2));
                                    panic!("test panic");
                                }
                                i + j
                            })
                            .len()
                        })
                    })
                }));
                assert!(r.is_err());
                all_back(&pool);
            }
        }
    }

    #[test]
    fn a_refused_thread_under_nested_maps_is_no_loss() {
        let leaf = |i: usize, j: usize, k: usize| i * 100 + j * 10 + k;
        let want: Vec<usize> =
            (0..12).map(|i| (0..5).map(|j| (0..3).map(|k| leaf(i, j, k)).sum::<usize>()).sum()).collect();
        for allowed in [0, 1, 2] {
            for seed in 1..=4 {
                let _refuse = RefuseSpawns::after(allowed);
                let pool = Pool::new(4);
                let got = jitter::with(seed, || {
                    map(Some(&pool), 12, None, |i| {
                        map(Some(&pool), 5, None, |j| {
                            map(Some(&pool), 3, None, |k| leaf(i, j, k)).iter().sum::<usize>()
                        })
                        .iter()
                        .sum::<usize>()
                    })
                });
                assert_eq!(got, want, "{allowed} threads allowed, seed {seed}");
                all_back(&pool);
            }
        }
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
