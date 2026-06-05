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
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::CallStats;

/// A thread-safe collection of [`CallStats`] keyed by API name.
///
/// Records are merged in place, so the registry only ever grows one entry per
/// distinct `&'static str` API name regardless of call volume.
#[derive(Debug, Default)]
pub struct ObsRegistry {
    inner: Mutex<HashMap<&'static str, CallStats>>,
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
        let guard = self.lock();

        let mut rows: Vec<(&'static str, CallStats)> =
            guard.iter().map(|(api, stats)| (*api, *stats)).collect();
        rows.sort_by_key(|(api, _)| *api);

        let mut total = CallStats::default();
        for (_, stats) in &rows {
            total.merge(stats);
        }
        // Drop the guard before formatting so the lock is held only as long as
        // the snapshot of rows takes to copy out.
        drop(guard);

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
}
