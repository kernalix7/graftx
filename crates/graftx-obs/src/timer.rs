//! Lightweight latency measurement helpers.
//!
//! [`Timer`] wraps a single [`std::time::Instant`] start point and reports the
//! [`elapsed`](Timer::elapsed) [`Duration`](std::time::Duration) since it was
//! started. [`time_call`] is a convenience wrapper that runs a closure and
//! returns both its result and the wall-clock time it took.
//!
//! These helpers are best-effort timers built on the monotonic clock; they make
//! no guarantees about resolution beyond what [`Instant`](std::time::Instant)
//! provides.

use std::time::{Duration, Instant};

/// A monotonic stopwatch anchored at a single start instant.
///
/// Construct with [`Timer::start`] and read the elapsed time with
/// [`elapsed`](Timer::elapsed). Because it is backed by
/// [`Instant`](std::time::Instant), successive reads never decrease.
#[derive(Debug, Clone, Copy)]
pub struct Timer {
    start: Instant,
}

impl Timer {
    /// Start a new timer anchored at the current instant.
    pub fn start() -> Timer {
        Timer {
            start: Instant::now(),
        }
    }

    /// Return the [`Duration`] elapsed since the timer was started.
    ///
    /// The underlying clock is monotonic, so repeated calls return values that
    /// never decrease.
    pub fn elapsed(&self) -> Duration {
        self.start.elapsed()
    }
}

/// Run `f`, returning its result alongside the wall-clock time it took.
///
/// The closure is invoked exactly once; the returned [`Duration`] measures the
/// span between just before and just after the call using the monotonic clock.
pub fn time_call<T, F: FnOnce() -> T>(f: F) -> (T, Duration) {
    let timer = Timer::start();
    let value = f();
    (value, timer.elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_is_monotonic_non_decreasing() {
        let timer = Timer::start();
        let first = timer.elapsed();
        let second = timer.elapsed();
        assert!(second >= first);
    }

    #[test]
    fn time_call_returns_closure_value() {
        let (value, _duration) = time_call(|| 42);
        assert_eq!(value, 42);
    }

    #[test]
    fn time_call_yields_a_duration() {
        let (value, duration) = time_call(|| "ok");
        assert_eq!(value, "ok");
        assert!(duration >= Duration::ZERO);
    }
}
