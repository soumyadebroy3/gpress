//! The wall clock, where the platform has one. The engine decides nothing from time: the debug timings
//! say how long things took, and a `Deadline` makes a job stop and say so, never change its result.

use std::time::{Duration, Instant};

/// Now, or None where the platform has no clock (WebAssembly without a host clock, where `Instant::now`
/// would panic).
#[cfg(not(target_arch = "wasm32"))]
pub fn now() -> Option<Instant> {
    Some(Instant::now())
}

#[cfg(target_arch = "wasm32")]
pub fn now() -> Option<Instant> {
    None
}

/// The time since `start` (zero without a clock), for the debug timings.
pub fn since(start: Option<Instant>) -> Duration {
    match (start, now()) {
        (Some(s), Some(n)) => n.saturating_duration_since(s),
        _ => Duration::ZERO,
    }
}

/// A point in time a job must not pass. The engine checks it between streams, segments and parts: once it
/// has passed, the rest of the work is skipped as cheaply as possible and the job reports that it ran out
/// of time instead of returning a result. A job that finishes gives the same bytes with or without one.
/// Without a clock every deadline is `NONE`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Deadline(Option<Instant>);

impl Deadline {
    /// No limit.
    pub const NONE: Deadline = Deadline(None);

    /// `budget` from now (no limit without a clock, or when the sum doesn't fit the clock).
    pub fn after(budget: Duration) -> Deadline {
        Deadline(now().and_then(|t| t.checked_add(budget)))
    }

    /// Whether the deadline has passed. Never true for `NONE`, and once true, true for good.
    pub fn passed(self) -> bool {
        match (self.0, now()) {
            (Some(t), Some(n)) => n >= t,
            _ => false,
        }
    }

    /// Whether there is a deadline at all.
    pub fn is_set(self) -> bool {
        self.0.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deadline_passes_once_and_none_never_does() {
        assert!(!Deadline::NONE.passed());
        assert!(!Deadline::NONE.is_set());
        let d = Deadline::after(Duration::from_millis(20));
        assert!(d.is_set());
        std::thread::sleep(Duration::from_millis(30));
        assert!(d.passed());
        let far = Deadline::after(Duration::from_secs(3600));
        assert!(!far.passed());
        // A budget past the end of the clock means no limit, not a wrong one.
        assert!(!Deadline::after(Duration::MAX).is_set());
    }
}
