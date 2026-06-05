//! A thread-safe registry of per-API [`CallStats`].
//!
//! [`ObsRegistry`] aggregates [`CallStats`] keyed by a `&'static str` API name
//! behind a [`Mutex`]. It is `Send + Sync` and intended to be shared across
//! worker threads (typically via an [`Arc`](std::sync::Arc)).
//!
//! A panic while the lock is held poisons the [`Mutex`]; rather than propagate
//! that as an `unwrap`/`expect` panic on every later access, the registry
//! recovers the guard with [`PoisonError::into_inner`] so that one misbehaving
//! thread cannot wedge metrics collection for the rest of the process. The
//! stored counters already saturate, so a partially-applied update is at worst
//! a slightly stale count, never undefined behaviour.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::{Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::Duration;

use crate::{CallStats, LatencyHist};

/// Backing storage for the process-global [`ObsRegistry`].
///
/// Initialized on first access by [`global`]; see that function for why a
/// single shared registry is the intended default for most callers.
static GLOBAL: OnceLock<ObsRegistry> = OnceLock::new();

/// Return the process-global default [`ObsRegistry`].
///
/// The registry is created lazily on first call and lives for the rest of the
/// process; every later call returns a reference to that same instance, so
/// callers can record statistics from anywhere without threading a handle
/// through their own APIs. [`ObsRegistry`] is `Send + Sync` (its state lives
/// behind a [`Mutex`]), so the shared reference is safe to use from any thread.
pub fn global() -> &'static ObsRegistry {
    GLOBAL.get_or_init(ObsRegistry::new)
}

/// Record a single call against the process-global registry.
///
/// Convenience wrapper over [`global().record`](ObsRegistry::record) for the
/// common case where callers use the shared default registry rather than their
/// own instance.
pub fn record(api: &'static str, bytes_out: u64, bytes_in: u64) {
    global().record(api, bytes_out, bytes_in);
}

/// A thread-safe collection of [`CallStats`] keyed by API name.
///
/// Records are merged in place, so the registry only ever grows one entry per
/// distinct `&'static str` API name regardless of call volume.
///
/// Latencies are tracked separately in their own [`LatencyHist`] map so that
/// the call/byte counters and the latency histograms stay independent: a call
/// can be counted via [`record`](Self::record) without an observed duration,
/// and a duration can be folded in via [`record_latency`](Self::record_latency)
/// without touching the byte counters.
#[derive(Debug, Default)]
pub struct ObsRegistry {
    inner: Mutex<HashMap<&'static str, CallStats>>,
    latency: Mutex<HashMap<&'static str, LatencyHist>>,
}

impl ObsRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Lock the inner map, recovering the guard if the lock is poisoned.
    ///
    /// See the [module docs](self) for why a poisoned lock is tolerated rather
    /// than turned into a panic.
    fn lock(&self) -> MutexGuard<'_, HashMap<&'static str, CallStats>> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Lock the per-API latency map, recovering the guard if it is poisoned.
    ///
    /// Mirrors [`lock`](Self::lock) for the separate latency histogram map; see
    /// the [module docs](self) for why a poisoned lock is tolerated rather than
    /// turned into a panic.
    fn lock_latency(&self) -> MutexGuard<'_, HashMap<&'static str, LatencyHist>> {
        self.latency.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Record a single call against `api`, creating its entry if needed.
    ///
    /// Byte counts are accumulated with saturating arithmetic by
    /// [`CallStats::record`].
    pub fn record(&self, api: &'static str, bytes_out: u64, bytes_in: u64) {
        self.lock()
            .entry(api)
            .or_default()
            .record(bytes_out, bytes_in);
    }

    /// Return a point-in-time copy of the per-API statistics.
    pub fn snapshot(&self) -> HashMap<&'static str, CallStats> {
        self.lock().clone()
    }

    /// Return a point-in-time copy of the per-API statistics sorted by API name.
    ///
    /// Unlike [`snapshot`](Self::snapshot), the entries are returned in a
    /// deterministic order (ascending by `&'static str` API name), which makes
    /// the result suitable for stable display or comparison.
    pub fn snapshot_sorted(&self) -> Vec<(&'static str, CallStats)> {
        let guard = self.lock();
        let mut rows: Vec<(&'static str, CallStats)> =
            guard.iter().map(|(api, stats)| (*api, *stats)).collect();
        // Drop the guard before sorting so the lock is held only as long as the
        // snapshot of rows takes to copy out.
        drop(guard);
        rows.sort_by_key(|(api, _)| *api);
        rows
    }

    /// Clear all recorded per-API statistics, leaving the registry empty.
    ///
    /// After this returns, [`snapshot`](Self::snapshot) is empty and
    /// [`total`](Self::total) is [`CallStats::default`]. Like every other
    /// accessor, this recovers from a poisoned lock rather than panicking.
    pub fn reset(&self) {
        self.lock().clear();
    }

    /// Merge every per-API entry into a single aggregate [`CallStats`].
    ///
    /// All fields saturate at [`u64::MAX`] via [`CallStats::merge`].
    pub fn total(&self) -> CallStats {
        let guard = self.lock();
        let mut total = CallStats::default();
        for stats in guard.values() {
            total.merge(stats);
        }
        total
    }

    /// Render the current statistics as a Markdown table.
    ///
    /// The table has columns `| API | Calls | Bytes Out | Bytes In |`, one row
    /// per API sorted by name for deterministic output, followed by a `TOTAL`
    /// row aggregating every API via [`total`](Self::total). Counts are written
    /// as plain integers.
    pub fn report(&self) -> String {
        let rows = self.snapshot_sorted();

        let mut total = CallStats::default();
        for (_, stats) in &rows {
            total.merge(stats);
        }

        let mut out = String::new();
        out.push_str("| API | Calls | Bytes Out | Bytes In |\n");
        out.push_str("| --- | --- | --- | --- |\n");
        for (api, stats) in &rows {
            // Writing into a `String` is infallible, so the result is ignored.
            let _ = writeln!(
                out,
                "| {} | {} | {} | {} |",
                api, stats.calls, stats.bytes_out, stats.bytes_in
            );
        }
        let _ = writeln!(
            out,
            "| TOTAL | {} | {} | {} |",
            total.calls, total.bytes_out, total.bytes_in
        );
        out
    }

    /// Record a single observed latency for `api`, creating its entry if needed.
    ///
    /// The duration is folded into a per-API [`LatencyHist`], whose bucket
    /// counts saturate at [`u64::MAX`] via [`LatencyHist::record`]. This is
    /// independent of [`record`](Self::record): tracking a latency does not
    /// touch the call/byte counters.
    pub fn record_latency(&self, api: &'static str, d: Duration) {
        self.lock_latency().entry(api).or_default().record(d);
    }

    /// Render the per-API latency histograms as Markdown.
    ///
    /// Each API is shown as a `### <api>` heading followed by its
    /// [`LatencyHist::report`] table; APIs are listed in ascending name order
    /// for deterministic output. When no latencies have been recorded the
    /// result is the empty string.
    pub fn latency_report(&self) -> String {
        let guard = self.lock_latency();
        let mut rows: Vec<(&'static str, LatencyHist)> =
            guard.iter().map(|(api, hist)| (*api, *hist)).collect();
        // Drop the guard before sorting and formatting so the lock is held only
        // as long as the snapshot of histograms takes to copy out.
        drop(guard);
        rows.sort_by_key(|(api, _)| *api);

        let mut out = String::new();
        for (api, hist) in &rows {
            // Writing into a `String` is infallible, so the result is ignored.
            let _ = writeln!(out, "### {api}");
            out.push_str(&hist.report());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn snapshot_reports_per_api_counts() {
        let registry = ObsRegistry::new();
        registry.record("NtCreateFile", 10, 20);
        registry.record("NtCreateFile", 5, 7);
        registry.record("NtClose", 1, 0);

        let snapshot = registry.snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(
            snapshot["NtCreateFile"],
            CallStats {
                calls: 2,
                bytes_out: 15,
                bytes_in: 27,
            }
        );
        assert_eq!(
            snapshot["NtClose"],
            CallStats {
                calls: 1,
                bytes_out: 1,
                bytes_in: 0,
            }
        );
    }

    #[test]
    fn snapshot_sorted_is_ordered_and_matches_snapshot() {
        let registry = ObsRegistry::new();
        registry.record("NtCreateFile", 10, 20);
        registry.record("NtCreateFile", 5, 7);
        registry.record("NtClose", 1, 0);
        registry.record("NtReadFile", 3, 9);

        let sorted = registry.snapshot_sorted();

        // Entries are ascending by API name.
        let names: Vec<&'static str> = sorted.iter().map(|(api, _)| *api).collect();
        assert_eq!(names, ["NtClose", "NtCreateFile", "NtReadFile"]);

        // The sorted view holds exactly the same entries as the unordered one.
        let snapshot = registry.snapshot();
        assert_eq!(sorted.len(), snapshot.len());
        for (api, stats) in &sorted {
            assert_eq!(snapshot[api], *stats);
        }
        assert_eq!(
            sorted,
            vec![
                (
                    "NtClose",
                    CallStats {
                        calls: 1,
                        bytes_out: 1,
                        bytes_in: 0,
                    }
                ),
                (
                    "NtCreateFile",
                    CallStats {
                        calls: 2,
                        bytes_out: 15,
                        bytes_in: 27,
                    }
                ),
                (
                    "NtReadFile",
                    CallStats {
                        calls: 1,
                        bytes_out: 3,
                        bytes_in: 9,
                    }
                ),
            ]
        );
    }

    #[test]
    fn snapshot_sorted_of_empty_registry_is_empty() {
        let registry = ObsRegistry::new();
        assert!(registry.snapshot_sorted().is_empty());
    }

    #[test]
    fn reset_clears_all_recorded_stats() {
        let registry = ObsRegistry::new();
        registry.record("NtCreateFile", 10, 20);
        registry.record("NtClose", 1, 3);
        assert!(!registry.snapshot().is_empty());

        registry.reset();

        assert!(registry.snapshot().is_empty());
        assert!(registry.snapshot_sorted().is_empty());
        assert_eq!(registry.total(), CallStats::default());

        // The registry is still usable after a reset.
        registry.record("NtReadFile", 4, 5);
        assert_eq!(
            registry.snapshot()["NtReadFile"],
            CallStats {
                calls: 1,
                bytes_out: 4,
                bytes_in: 5,
            }
        );
    }

    #[test]
    fn reset_of_empty_registry_is_a_noop() {
        let registry = ObsRegistry::new();
        registry.reset();
        assert!(registry.snapshot().is_empty());
        assert_eq!(registry.total(), CallStats::default());
    }

    #[test]
    fn total_merges_all_apis() {
        let registry = ObsRegistry::new();
        registry.record("NtCreateFile", 10, 20);
        registry.record("NtCreateFile", 5, 7);
        registry.record("NtClose", 1, 3);

        assert_eq!(
            registry.total(),
            CallStats {
                calls: 3,
                bytes_out: 16,
                bytes_in: 30,
            }
        );
    }

    #[test]
    fn total_of_empty_registry_is_default() {
        let registry = ObsRegistry::new();
        assert_eq!(registry.total(), CallStats::default());
        assert!(registry.snapshot().is_empty());
    }

    #[test]
    fn concurrent_records_aggregate_exactly() {
        const THREADS: u64 = 8;
        const CALLS_PER_THREAD: u64 = 1_000;

        let registry = Arc::new(ObsRegistry::new());
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let registry = Arc::clone(&registry);
                thread::spawn(move || {
                    for _ in 0..CALLS_PER_THREAD {
                        registry.record("NtCreateFile", 2, 3);
                        registry.record("NtClose", 1, 1);
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("worker thread panicked");
        }

        let expected_per_api = THREADS * CALLS_PER_THREAD;
        let snapshot = registry.snapshot();
        assert_eq!(snapshot["NtCreateFile"].calls, expected_per_api);
        assert_eq!(snapshot["NtCreateFile"].bytes_out, expected_per_api * 2);
        assert_eq!(snapshot["NtCreateFile"].bytes_in, expected_per_api * 3);
        assert_eq!(snapshot["NtClose"].calls, expected_per_api);

        let total = registry.total();
        assert_eq!(total.calls, expected_per_api * 2);
        assert_eq!(total.bytes_out, expected_per_api * 2 + expected_per_api);
        assert_eq!(total.bytes_in, expected_per_api * 3 + expected_per_api);
    }

    #[test]
    fn report_renders_sorted_table_with_total() {
        let registry = ObsRegistry::new();
        registry.record("NtCreateFile", 10, 20);
        registry.record("NtCreateFile", 5, 7);
        registry.record("NtClose", 1, 3);

        let report = registry.report();

        assert!(
            report.contains("| API | Calls | Bytes Out | Bytes In |"),
            "report is missing the header:\n{report}"
        );
        assert!(
            report.contains("| NtCreateFile | 2 | 15 | 27 |"),
            "report is missing the NtCreateFile row:\n{report}"
        );
        assert!(
            report.contains("| NtClose | 1 | 1 | 3 |"),
            "report is missing the NtClose row:\n{report}"
        );
        assert!(
            report.contains("| TOTAL | 3 | 16 | 30 |"),
            "report is missing the TOTAL row:\n{report}"
        );

        // Rows are sorted by API name and the TOTAL row comes last.
        let ntclose = report.find("| NtClose |").expect("NtClose row present");
        let ntcreate = report
            .find("| NtCreateFile |")
            .expect("NtCreateFile row present");
        let total = report.find("| TOTAL |").expect("TOTAL row present");
        assert!(
            ntclose < ntcreate,
            "rows are not sorted by API name:\n{report}"
        );
        assert!(ntcreate < total, "TOTAL row is not last:\n{report}");
    }

    #[test]
    fn report_of_empty_registry_has_header_and_zero_total() {
        let registry = ObsRegistry::new();
        let report = registry.report();

        assert!(
            report.contains("| API | Calls | Bytes Out | Bytes In |"),
            "empty report is missing the header:\n{report}"
        );
        assert!(
            report.contains("| TOTAL | 0 | 0 | 0 |"),
            "empty report is missing a zeroed TOTAL row:\n{report}"
        );
    }

    #[test]
    fn record_recovers_from_poisoned_lock() {
        let registry = Arc::new(ObsRegistry::new());
        registry.record("NtCreateFile", 1, 1);

        // Poison the lock by panicking while holding the inner guard.
        let poisoner = Arc::clone(&registry);
        let result = thread::spawn(move || {
            let _guard = poisoner.inner.lock().expect("lock not yet poisoned");
            panic!("intentional poison");
        })
        .join();
        assert!(result.is_err(), "poisoning thread should have panicked");

        // The registry stays usable despite the poisoned lock.
        registry.record("NtCreateFile", 4, 6);
        let snapshot = registry.snapshot();
        assert_eq!(
            snapshot["NtCreateFile"],
            CallStats {
                calls: 2,
                bytes_out: 5,
                bytes_in: 7,
            }
        );
        assert_eq!(registry.total().calls, 2);
    }

    #[test]
    fn global_returns_a_stable_reference() {
        let first = global();
        let second = global();
        assert!(
            std::ptr::eq(first, second),
            "global() should return the same instance on every call"
        );
    }

    #[test]
    fn record_updates_the_global_registry() {
        // The global registry is shared across tests, so key off an API name
        // unique to this test and assert only on that row rather than totals.
        const API: &str = "global_test::record_updates_the_global_registry";

        record(API, 10, 20);
        record(API, 5, 7);

        assert_eq!(
            global().snapshot()[API],
            CallStats {
                calls: 2,
                bytes_out: 15,
                bytes_in: 27,
            }
        );
    }

    #[test]
    fn record_latency_tracks_per_api_histograms() {
        let registry = ObsRegistry::new();
        // NtCreateFile: two observations, both in the < 4us bucket.
        registry.record_latency("NtCreateFile", Duration::from_micros(3));
        registry.record_latency("NtCreateFile", Duration::from_micros(3));
        // NtClose: one observation in the overflow (>= 1024us) bucket.
        registry.record_latency("NtClose", Duration::from_secs(1));

        let report = registry.latency_report();
        assert!(
            report.contains("### NtCreateFile"),
            "latency report is missing the NtCreateFile heading:\n{report}"
        );
        assert!(
            report.contains("### NtClose"),
            "latency report is missing the NtClose heading:\n{report}"
        );
        assert!(
            report.contains("| <4us | 2 |"),
            "latency report is missing the NtCreateFile bucket row:\n{report}"
        );
        assert!(
            report.contains("| >=1024us | 1 |"),
            "latency report is missing the NtClose overflow row:\n{report}"
        );

        // APIs are listed in ascending name order.
        let ntclose = report.find("### NtClose").expect("NtClose heading present");
        let ntcreate = report
            .find("### NtCreateFile")
            .expect("NtCreateFile heading present");
        assert!(
            ntclose < ntcreate,
            "latency report APIs are not sorted by name:\n{report}"
        );
    }

    #[test]
    fn latency_report_of_empty_registry_is_empty() {
        let registry = ObsRegistry::new();
        assert!(registry.latency_report().is_empty());
    }

    #[test]
    fn record_latency_is_independent_of_record() {
        let registry = ObsRegistry::new();
        registry.record_latency("NtReadFile", Duration::from_micros(0));

        // Recording a latency must not create a CallStats entry.
        assert!(
            registry.snapshot().is_empty(),
            "record_latency should not touch the call/byte counters"
        );
        assert_eq!(registry.total(), CallStats::default());

        // Recording a call must not create a latency entry.
        let other = ObsRegistry::new();
        other.record("NtWriteFile", 1, 2);
        assert!(
            other.latency_report().is_empty(),
            "record should not touch the latency histograms"
        );
    }

    #[test]
    fn record_latency_recovers_from_poisoned_lock() {
        let registry = Arc::new(ObsRegistry::new());
        registry.record_latency("NtCreateFile", Duration::from_micros(3));

        // Poison the latency lock by panicking while holding its guard.
        let poisoner = Arc::clone(&registry);
        let result = thread::spawn(move || {
            let _guard = poisoner.latency.lock().expect("lock not yet poisoned");
            panic!("intentional poison");
        })
        .join();
        assert!(result.is_err(), "poisoning thread should have panicked");

        // The registry stays usable despite the poisoned latency lock.
        registry.record_latency("NtCreateFile", Duration::from_micros(3));
        let report = registry.latency_report();
        assert!(
            report.contains("| <4us | 2 |"),
            "latency report should reflect both observations:\n{report}"
        );
    }

    #[test]
    fn concurrent_record_latency_aggregates_exactly() {
        const THREADS: u64 = 8;
        const CALLS_PER_THREAD: u64 = 1_000;

        let registry = Arc::new(ObsRegistry::new());
        let handles: Vec<_> = (0..THREADS)
            .map(|_| {
                let registry = Arc::clone(&registry);
                thread::spawn(move || {
                    for _ in 0..CALLS_PER_THREAD {
                        // 3us falls in the < 4us bucket.
                        registry.record_latency("NtCreateFile", Duration::from_micros(3));
                    }
                })
            })
            .collect();

        for handle in handles {
            handle.join().expect("worker thread panicked");
        }

        let expected = THREADS * CALLS_PER_THREAD;
        let report = registry.latency_report();
        assert!(
            report.contains(&format!("| <4us | {expected} |")),
            "latency report should reflect all observations:\n{report}"
        );
    }

    #[test]
    fn record_and_global_record_target_the_same_registry() {
        // Unique per-test API name keeps this independent of other tests'
        // writes to the shared global registry.
        const API: &str = "global_test::record_and_global_record_target_the_same_registry";

        record(API, 1, 2);
        global().record(API, 3, 4);

        assert_eq!(
            global().snapshot()[API],
            CallStats {
                calls: 2,
                bytes_out: 4,
                bytes_in: 6,
            }
        );
    }
}
