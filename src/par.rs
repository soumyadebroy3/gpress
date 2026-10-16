//! Threads for independent pieces of work, with the standard library only.
//!
//! Results never depend on the number of threads: work is cut into the same pieces whatever the count, and
//! the results are put together in their original order. A `Pool` is shared by everything one job does
//! (the streams of a PDF and the segments of each stream), so the job never runs more threads than asked.
//! A thread the system refuses (thread or process limits) is no loss: the calling thread does the work.

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{Scope, ScopedJoinHandle};

/// The most threads a pool runs, whatever it is asked for.
pub const MAX_THREADS: usize = 64;

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
    /// A pool for `threads` threads in all (at least 1: the caller, at most [`MAX_THREADS`]).
    pub fn new(threads: usize) -> Arc<Pool> {
        let threads = threads.clamp(1, MAX_THREADS);
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

/// The default thread count: what the machine offers, at most 8.
pub fn default_threads() -> usize {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).clamp(1, 8)
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
/// job never runs more threads than its pool has. Inside a [`Memo`]'s compute the calling thread does
/// everything, in order: no helpers, nothing lent.
pub fn map<R: Send>(pool: Option<&Pool>, n: usize, order: Option<&[usize]>, f: impl Fn(usize) -> R + Sync) -> Vec<R> {
    debug_assert!(order.is_none_or(|o| o.len() == n));
    let at = |k: usize| order.map_or(k, |o| o[k]);
    // One thread (as in a browser build), or a memo's compute: plain sequential work, no thread machinery.
    let pool = match pool {
        Some(p) if n > 1 && p.threads > 1 && !COMPUTING.with(Cell::get) => p,
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

thread_local! {
    /// Set while this thread computes a memo entry that other threads may wait for.
    static COMPUTING: Cell<bool> = const { Cell::new(false) };
}

/// `compute()` with this thread marked as computing a memo entry, and the mark restored after (also when it
/// panics). While marked, every `map` runs on this thread alone: it never waits for a pool slot, so whoever
/// waits for the entry is sure to get it. (Without the rule, a compute could lend its slot at the end of a
/// map and wait for one back, while every slot went to pieces that wait for its entry.)
fn computing<R>(compute: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            COMPUTING.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(COMPUTING.with(|c| c.replace(true)));
    compute()
}

enum State<V> {
    Pending,
    Done(V),
    /// The thread computing it panicked, or its value is not kept: whoever waits computes it on their own.
    Failed,
}

struct Entry<V> {
    state: Mutex<State<V>>,
    ready: Condvar,
}

/// What a kept value costs besides its own weight (its key, its entry and their places in the map and the
/// queue); a bounded memo counts it in its budget.
pub const ENTRY_COST: usize = 256;

/// What a memo has done, and what it holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoStats {
    /// Values given from the memo.
    pub hits: usize,
    /// Values computed (also those computed again after a failed fit check).
    pub computed: usize,
    /// Values kept now, and their weight ([`ENTRY_COST`] each included).
    pub kept: usize,
    pub bytes: usize,
    /// The most the kept values ever weighed.
    pub peak: usize,
}

/// Remembers results by key so that identical work is done once, also when two threads ask at the same time
/// (the second waits for the first). The caller checks that a remembered result fits its input; `get`
/// takes that check, and a result that fails it is computed again. A bounded memo keeps values up to a
/// budget, the oldest dropped first.
///
/// A compute runs its `map`s on its own thread (see [`map`]), so a thread computing an entry never waits for
/// a pool slot, and everyone waiting for the entry gets it in finite time.
pub struct Memo<K, V> {
    kept: Mutex<Kept<K, V>>,
    /// The most the kept values may weigh in all; `usize::MAX`: no bound.
    budget: usize,
    weigh: fn(&V) -> usize,
    hits: AtomicUsize,
    computed: AtomicUsize,
}

struct Kept<K, V> {
    /// Every entry being computed or kept.
    map: HashMap<K, Arc<Entry<V>>>,
    /// The kept entries, oldest first, with their weight.
    order: VecDeque<(K, Arc<Entry<V>>, usize)>,
    bytes: usize,
    peak: usize,
}

impl<K: std::hash::Hash + Eq + Clone, V: Clone> Memo<K, V> {
    /// A memo that keeps every value while it lives.
    pub fn new() -> Memo<K, V> {
        Memo::bounded(usize::MAX, |_| 0)
    }

    /// A memo that keeps values while they weigh at most `budget` in all: `weigh(v)` each, plus
    /// [`ENTRY_COST`]. Values are kept in the order they are finished and the oldest go first; a value
    /// heavier than the whole budget is not kept.
    pub fn bounded(budget: usize, weigh: fn(&V) -> usize) -> Memo<K, V> {
        Memo {
            kept: Mutex::new(Kept { map: HashMap::new(), order: VecDeque::new(), bytes: 0, peak: 0 }),
            budget,
            weigh,
            hits: AtomicUsize::new(0),
            computed: AtomicUsize::new(0),
        }
    }

    /// The remembered value for `key` when `fits` accepts it, else `compute()` (remembered for next time).
    pub fn get(&self, key: K, fits: impl Fn(&V) -> bool, compute: impl FnOnce() -> V) -> V {
        self.get_kept(key, fits, || (compute(), true))
    }

    /// As `get`, where `compute` also says whether its value may be kept. A value not kept is never given
    /// again: whoever waited for it computes on their own.
    pub fn get_kept(&self, key: K, fits: impl Fn(&V) -> bool, compute: impl FnOnce() -> (V, bool)) -> V {
        let (entry, mine) = {
            let mut kept = self.lock();
            match kept.map.get(&key) {
                Some(e) => (e.clone(), false),
                None => {
                    let e = Arc::new(Entry { state: Mutex::new(State::Pending), ready: Condvar::new() });
                    kept.map.insert(key.clone(), e.clone());
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
                    self.hits.fetch_add(1, Ordering::Relaxed);
                    return v.clone();
                }
            }
            drop(st);
            self.computed.fetch_add(1, Ordering::Relaxed);
            return compute().0;
        }
        // Drops the entry if `compute` panics or its value is not kept, so nobody waits forever.
        struct Guard<'m, K: std::hash::Hash + Eq + Clone, V: Clone> {
            memo: &'m Memo<K, V>,
            key: &'m K,
            entry: &'m Arc<Entry<V>>,
            done: bool,
        }
        impl<K: std::hash::Hash + Eq + Clone, V: Clone> Drop for Guard<'_, K, V> {
            fn drop(&mut self) {
                if !self.done {
                    self.memo.forget(self.key, self.entry);
                    *self.entry.state.lock().unwrap_or_else(|e| e.into_inner()) = State::Failed;
                    self.entry.ready.notify_all();
                }
            }
        }
        let mut g = Guard { memo: self, key: &key, entry: &entry, done: false };
        let (v, keep) = computing(compute);
        self.computed.fetch_add(1, Ordering::Relaxed);
        if keep {
            *entry.state.lock().unwrap_or_else(|e| e.into_inner()) = State::Done(v.clone());
            g.done = true;
            entry.ready.notify_all();
        }
        drop(g);
        if keep {
            self.keep(key, entry, (self.weigh)(&v).saturating_add(ENTRY_COST));
        }
        v
    }

    /// What this memo has done, and what it holds.
    pub fn stats(&self) -> MemoStats {
        let kept = self.lock();
        MemoStats {
            hits: self.hits.load(Ordering::Relaxed),
            computed: self.computed.load(Ordering::Relaxed),
            kept: kept.order.len(),
            bytes: kept.bytes,
            peak: kept.peak,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Kept<K, V>> {
        self.kept.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Takes `entry` out of the map, unless the key has another entry by now.
    fn forget(&self, key: &K, entry: &Arc<Entry<V>>) {
        let mut kept = self.lock();
        if kept.map.get(key).is_some_and(|e| Arc::ptr_eq(e, entry)) {
            kept.map.remove(key);
        }
    }

    /// Keeps a finished entry of `weight`, and drops the oldest ones while the kept weigh more than the budget.
    fn keep(&self, key: K, entry: Arc<Entry<V>>, weight: usize) {
        if weight > self.budget {
            self.forget(&key, &entry);
            return;
        }
        let mut kept = self.lock();
        let kept = &mut *kept;
        kept.order.push_back((key, entry, weight));
        kept.bytes = kept.bytes.saturating_add(weight);
        while kept.bytes > self.budget {
            let Some((k, e, w)) = kept.order.pop_front() else { break };
            kept.bytes -= w;
            if kept.map.get(&k).is_some_and(|x| Arc::ptr_eq(x, &e)) {
                kept.map.remove(&k);
            }
        }
        kept.peak = kept.peak.max(kept.bytes);
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
    fn a_pool_never_has_more_than_max_threads() {
        assert_eq!(Pool::new(usize::MAX).threads, MAX_THREADS);
        assert_eq!(Pool::new(MAX_THREADS + 1).free.load(Ordering::SeqCst), MAX_THREADS - 1);
        assert_eq!(Pool::new(0).threads, 1);
        assert_eq!(Pool::new(5).threads, 5);
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
    fn a_bounded_memo_keeps_its_budget_oldest_first() {
        let budget = 3 * (100 + ENTRY_COST);
        let memo: Memo<u8, Vec<u8>> = Memo::bounded(budget, |v| v.len());
        let made = Mutex::new(Vec::new());
        let ask = |k: u8| {
            memo.get(
                k,
                |v| v[0] == k,
                || {
                    made.lock().unwrap().push(k);
                    vec![k; 100]
                },
            )
        };
        for k in 1..=5 {
            assert_eq!(ask(k), vec![k; 100]);
        }
        // 1 and 2 went to keep the budget. A hit doesn't make a value younger: 3 and 4 go for 2 and 1, and
        // 5 stays.
        for k in [5, 4, 3, 2, 1, 5] {
            assert_eq!(ask(k), vec![k; 100]);
        }
        assert_eq!(*made.lock().unwrap(), [1, 2, 3, 4, 5, 2, 1]);
        let st = memo.stats();
        assert_eq!((st.hits, st.computed, st.kept, st.bytes, st.peak), (4, 7, 3, budget, budget));
    }

    #[test]
    fn many_tiny_values_count_their_entries() {
        let memo: Memo<u32, u8> = Memo::bounded(4096, |_| 1);
        for k in 0..1000u32 {
            assert_eq!(memo.get(k, |_| true, || k as u8), k as u8);
        }
        let st = memo.stats();
        assert_eq!(st.kept, 4096 / (1 + ENTRY_COST), "{st:?}");
        assert!(st.peak <= 4096, "{st:?}");
        // The newest are kept.
        assert_eq!(memo.get(999, |_| true, || 0), 999u32 as u8);
        assert_eq!(memo.get(0, |_| true, || 7), 7);
    }

    #[test]
    fn a_value_not_kept_is_never_given() {
        let memo: Memo<u8, u32> = Memo::bounded(1 << 20, |_| 4);
        assert_eq!(memo.get_kept(1, |_| true, || (5, false)), 5);
        assert_eq!(memo.get(1, |_| true, || 6), 6);
        assert_eq!(memo.get(1, |_| true, || 7), 6);
        let st = memo.stats();
        assert_eq!((st.hits, st.computed, st.kept, st.bytes), (1, 2, 1, 4 + ENTRY_COST));
        // A value heavier than the whole budget is not kept either.
        let small: Memo<u8, Vec<u8>> = Memo::bounded(100 + ENTRY_COST, |v| v.len());
        assert_eq!(small.get(1, |_| true, || vec![1; 101]), vec![1; 101]);
        assert_eq!(small.get(1, |_| true, || vec![2; 100]), vec![2; 100]);
        assert_eq!(small.get(1, |_| true, || vec![3; 1]), vec![2; 100]);
        assert_eq!(small.stats().kept, 1);
        // Whoever waited for a value that is not kept computes on their own.
        let t0 = std::time::Instant::now();
        let holders = |k: u8| memo.kept.lock().unwrap().map.get(&k).map_or(0, Arc::strong_count);
        std::thread::scope(|s| {
            let owner = s.spawn(|| {
                memo.get_kept(
                    2,
                    |_| true,
                    || {
                        // Until the other thread waits for this entry (the map, this thread and the waiter).
                        while holders(2) < 3 {
                            assert!(t0.elapsed().as_secs() < 10, "nobody waited");
                            std::thread::sleep(std::time::Duration::from_millis(1));
                        }
                        (8, false)
                    },
                )
            });
            while holders(2) == 0 {
                assert!(t0.elapsed().as_secs() < 10, "the owner never started");
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            assert_eq!(memo.get(2, |_| true, || 9), 9);
            assert_eq!(owner.join().unwrap(), 8);
        });
        // Neither 8 nor 9 was kept.
        assert_eq!(memo.get(2, |_| true, || 10), 10);
        assert_eq!(memo.get(2, |_| true, || 11), 10);
    }

    #[test]
    fn a_compute_runs_its_maps_alone() {
        let pool = Pool::new(4);
        let memo: Memo<u8, usize> = Memo::new();
        let me = std::thread::current().id();
        let want: usize = (0..8).map(|i| (0..3).map(|j| i * 10 + j).sum::<usize>()).sum();
        let got = memo.get(
            1,
            |_| true,
            || {
                map(Some(&pool), 8, None, |i| {
                    // On the computing thread, with every slot still free: no helper started, nothing lent.
                    assert_eq!(std::thread::current().id(), me);
                    assert_eq!(pool.free.load(Ordering::SeqCst), 3);
                    assert_eq!(*pool.lent.lock().unwrap(), (0, 0));
                    map(Some(&pool), 3, None, |j| i * 10 + j).iter().sum::<usize>()
                })
                .iter()
                .sum()
            },
        );
        assert_eq!(got, want);
        // The mark is gone afterwards, also after a compute that panicked.
        assert!(!COMPUTING.with(Cell::get));
        let r =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| memo.get(2, |_| true, || panic!("test panic"))));
        assert!(r.is_err());
        assert!(!COMPUTING.with(Cell::get));
        all_back(&pool);
    }

    /// A tree of small maps (3, 2, 3 and 2 pieces): each piece asks `memo` for one key once its own nested
    /// map is done. The key's compute runs a map of 3 slow pieces.
    fn tree(pool: &Pool, memo: &Memo<u8, usize>, depth: usize) -> usize {
        map(Some(pool), 2 + depth % 2, None, |j| {
            let below = if depth == 0 { j } else { tree(pool, memo, depth - 1) };
            let v = memo.get(
                0,
                |_| true,
                || {
                    map(Some(pool), 3, None, |k| {
                        std::thread::sleep(std::time::Duration::from_micros(100));
                        k
                    })
                    .iter()
                    .sum::<usize>()
                },
            );
            below + v
        })
        .iter()
        .sum()
    }

    #[test]
    fn no_deadlock_with_a_shared_entry() {
        // The key's value is never kept, so it is computed again and again while the small maps leave slots
        // free, and their callers lend theirs and wait for one back. Without the compute rule a compute would
        // lend its slot at the end of its map too; the slots it frees go first to callers waiting for one,
        // which then ask for its entry, and it would wait for a slot forever.
        let want = tree(&Pool::new(1), &Memo::new(), 3);
        for threads in [2, 3, 4] {
            for seed in 1..=8 {
                let pool = Pool::new(threads);
                let memo: Arc<Memo<u8, usize>> = Arc::new(Memo::bounded(0, |_| 0));
                let (p, m) = (pool.clone(), memo.clone());
                let (tx, rx) = std::sync::mpsc::channel();
                let got = jitter::with(seed, || {
                    std::thread::spawn(move || {
                        let _ = tx.send(tree(&p, &m, 3));
                    });
                    // A watchdog: a deadlock fails the test instead of hanging it.
                    rx.recv_timeout(std::time::Duration::from_secs(30)).expect("deadlock: no result in 30 s")
                });
                assert_eq!(got, want, "{threads} threads, seed {seed}");
                all_back(&pool);
                let st = memo.stats();
                assert_eq!((st.kept, st.peak), (0, 0));
                // 3 + 3 x 2 + 3 x 2 x 3 + 3 x 2 x 3 x 2 pieces asked.
                assert!(st.computed >= 1 && st.hits + st.computed == 63, "{st:?}");
            }
        }
    }

    #[test]
    fn a_compute_never_waits_for_a_slot() {
        // An order of events that deadlocks without the compute rule, forced step by step. The compute's map
        // lends its slot at its end while its helper still works. Three other parts of the job then hold
        // every slot and wait for the entry: two take the free slots, and one lent its slot and waits for one
        // back, so it is first in line for the slot the compute's helper frees, and the compute would wait for
        // a slot forever. With the rule the compute has no helper and lends nothing: everyone gets the value.
        use std::sync::atomic::AtomicBool;
        use std::time::{Duration, Instant};
        fn wait(ok: impl Fn() -> bool, most: Duration) {
            let t0 = Instant::now();
            while !ok() && t0.elapsed() < most {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        struct Scene {
            pool: Arc<Pool>,
            memo: Memo<u8, usize>,
            started: AtomicBool,
            go: AtomicBool,
            done: AtomicBool,
        }
        let sc = Arc::new(Scene {
            pool: Pool::new(3),
            memo: Memo::new(),
            started: AtomicBool::new(false),
            go: AtomicBool::new(false),
            done: AtomicBool::new(false),
        });
        let (tx, rx) = std::sync::mpsc::channel();
        // The compute: a map of two pieces. A helper's piece waits for the others to take their places.
        let (s, t) = (sc.clone(), tx.clone());
        std::thread::spawn(move || {
            let me = std::thread::current().id();
            let v = s.memo.get(
                0,
                |_| true,
                || {
                    map(Some(&s.pool), 2, None, |c| {
                        if std::thread::current().id() == me {
                            wait(|| s.started.load(Ordering::SeqCst), Duration::from_millis(300));
                        } else {
                            s.started.store(true, Ordering::SeqCst);
                            wait(|| s.go.load(Ordering::SeqCst), Duration::from_secs(5));
                        }
                        c + 1
                    })
                    .iter()
                    .sum()
                },
            );
            s.done.store(true, Ordering::SeqCst);
            let _ = t.send(v);
        });
        // A part of the job on a slot taken for it: it asks for the entry, then gives the slot back. One that
        // lent its slot first takes one back.
        let part = |lent: bool| {
            let (s, t) = (sc.clone(), tx.clone());
            std::thread::spawn(move || {
                if lent {
                    s.pool.take_back();
                }
                let v = s.memo.get(0, |_| true, || 0);
                s.pool.release();
                let _ = t.send(v);
            });
        };
        // Once the compute's map has lent its slot (or the compute is done, running alone): one part takes a
        // slot, another takes the other free one, the first lends its own, a third part takes that, and the
        // first waits for a slot back.
        let free = || sc.pool.free.load(Ordering::SeqCst);
        wait(
            || sc.done.load(Ordering::SeqCst) || (sc.started.load(Ordering::SeqCst) && free() == 2),
            Duration::from_secs(5),
        );
        assert!(sc.pool.take());
        assert!(sc.pool.take());
        part(false);
        sc.pool.release();
        assert!(sc.pool.take());
        part(false);
        part(true);
        wait(|| sc.done.load(Ordering::SeqCst) || *sc.pool.lent.lock().unwrap() == (1, 0), Duration::from_secs(5));
        sc.go.store(true, Ordering::SeqCst);
        // A watchdog: a deadlock fails the test instead of hanging it.
        for _ in 0..4 {
            assert_eq!(rx.recv_timeout(Duration::from_secs(20)).expect("deadlock: no result in 20 s"), 3);
        }
        all_back(&sc.pool);
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
