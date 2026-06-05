//! A tiny fixed-bucket latency histogram.
//!
//! [`LatencyHist`] sorts call durations into power-of-two microsecond buckets
//! and counts how many observations fall into each. The buckets are upper-bound
//! exclusive: bucket `i` (for `i < `[`BUCKETS`]` - 1`) counts durations whose
//! whole-microsecond value is strictly less than `2^i` microseconds, and the
//! final bucket is an open-ended overflow bucket for everything at or above the
//! largest tracked bound.
//!
//! The upper bounds run from `1us` (`2^0`) through `1024us` (`2^10`), giving
//! twelve buckets once the open-ended overflow bucket (`>=1024us`) is counted.
//! All counts use saturating arithmetic so a long-lived histogram cannot panic
//! on overflow; like the rest of `graftx-obs` the numbers are best-effort.

use std::time::Duration;

/// Number of histogram buckets, including the open-ended overflow bucket.
pub const BUCKETS: usize = 12;

/// Exclusive upper bounds, in whole microseconds, for each non-overflow bucket.
///
/// Bucket `i` counts durations with `d.as_micros() < UPPER_BOUNDS_US[i]`; the
/// final bucket (index `BUCKETS - 1`) has no entry here and absorbs everything
/// at or above the last bound (`>= 1024us`).
const UPPER_BOUNDS_US: [u128; BUCKETS - 1] = [1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024];

/// A latency histogram over power-of-two microsecond buckets.
///
/// Construct with [`LatencyHist::new`] (or [`Default`]), feed it durations with
/// [`record`](LatencyHist::record), and read it back via
/// [`count`](LatencyHist::count), [`bucket_counts`](LatencyHist::bucket_counts),
/// or the Markdown [`report`](LatencyHist::report).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LatencyHist {
    counts: [u64; BUCKETS],
}

impl LatencyHist {
    /// Create an empty histogram with all bucket counts at zero.
    pub fn new() -> LatencyHist {
        LatencyHist {
            counts: [0; BUCKETS],
        }
    }

    /// Record a single observed duration into its bucket.
    ///
    /// The duration is placed in the first bucket whose exclusive upper bound it
    /// falls under; durations at or above the largest tracked bound land in the
    /// open-ended overflow bucket. The matched bucket's count saturates at
    /// [`u64::MAX`] rather than wrapping or panicking.
    pub fn record(&mut self, d: Duration) {
        let micros = d.as_micros();
        let index = Self::bucket_index(micros);
        self.counts[index] = self.counts[index].saturating_add(1);
    }

    /// Return the index of the bucket that a whole-microsecond value belongs to.
    fn bucket_index(micros: u128) -> usize {
        for (i, &bound) in UPPER_BOUNDS_US.iter().enumerate() {
            if micros < bound {
                return i;
            }
        }
        BUCKETS - 1
    }

    /// Total number of observations recorded across all buckets.
    ///
    /// Computed with saturating addition, so it clamps at [`u64::MAX`].
    pub fn count(&self) -> u64 {
        self.counts
            .iter()
            .fold(0u64, |acc, &c| acc.saturating_add(c))
    }

    /// Borrow the raw per-bucket counts, lowest bound first.
    ///
    /// Index `i` (for `i < `[`BUCKETS`]` - 1`) is the count for durations under
    /// `UPPER_BOUNDS_US[i]` microseconds; the last index is the overflow bucket.
    pub fn bucket_counts(&self) -> &[u64; BUCKETS] {
        &self.counts
    }

    /// Render the non-empty buckets as a small Markdown table.
    ///
    /// The table has two columns, the bucket's exclusive upper bound and its
    /// count; the overflow bucket is shown with a `>=` lower bound. Buckets with
    /// a zero count are omitted to keep the report compact. When no observations
    /// have been recorded the table body is empty (header only).
    pub fn report(&self) -> String {
        let mut out = String::from("| bucket | count |\n| --- | --- |\n");
        for (i, &count) in self.counts.iter().enumerate() {
            if count == 0 {
                continue;
            }
            if i < UPPER_BOUNDS_US.len() {
                let bound = UPPER_BOUNDS_US[i];
                out.push_str(&format!("| <{bound}us | {count} |\n"));
            } else {
                // Overflow bucket: open-ended at the largest tracked bound.
                let last = UPPER_BOUNDS_US[UPPER_BOUNDS_US.len() - 1];
                out.push_str(&format!("| >={last}us | {count} |\n"));
            }
        }
        out
    }
}

impl Default for LatencyHist {
    fn default() -> LatencyHist {
        LatencyHist::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_and_default_start_empty() {
        let from_new = LatencyHist::new();
        let from_default = LatencyHist::default();
        assert_eq!(from_new, from_default);
        assert_eq!(from_new.count(), 0);
        assert_eq!(*from_new.bucket_counts(), [0u64; BUCKETS]);
    }

    #[test]
    fn record_places_durations_in_expected_buckets() {
        let mut hist = LatencyHist::new();
        // 0us -> bucket 0 (< 1us).
        hist.record(Duration::from_micros(0));
        // 1us -> bucket 1 (< 2us), since bucket 0 is strictly < 1us.
        hist.record(Duration::from_micros(1));
        // 3us -> bucket 2 (< 4us).
        hist.record(Duration::from_micros(3));
        // 1000us -> bucket 10 (< 1024us).
        hist.record(Duration::from_micros(1000));

        let counts = hist.bucket_counts();
        assert_eq!(counts[0], 1);
        assert_eq!(counts[1], 1);
        assert_eq!(counts[2], 1);
        assert_eq!(counts[10], 1);
        assert_eq!(hist.count(), 4);
    }

    #[test]
    fn at_or_above_largest_bound_lands_in_overflow_bucket() {
        let mut hist = LatencyHist::new();
        // Exactly the largest tracked bound (1024us) is not < 1024, so overflow.
        hist.record(Duration::from_micros(1024));
        // Just over a second is well past the largest bound.
        hist.record(Duration::from_secs(1));
        let counts = hist.bucket_counts();
        assert_eq!(counts[BUCKETS - 1], 2);
        assert_eq!(hist.count(), 2);
    }

    #[test]
    fn boundary_is_upper_bound_exclusive() {
        let mut hist = LatencyHist::new();
        // 2us is the bound for bucket 1, so it must fall into bucket 2.
        hist.record(Duration::from_micros(2));
        let counts = hist.bucket_counts();
        assert_eq!(counts[1], 0);
        assert_eq!(counts[2], 1);
    }

    #[test]
    fn record_saturates_on_overflow() {
        let mut hist = LatencyHist::new();
        hist.counts[0] = u64::MAX;
        hist.record(Duration::from_micros(0));
        assert_eq!(hist.bucket_counts()[0], u64::MAX);
    }

    #[test]
    fn report_lists_non_empty_buckets_with_known_rows() {
        let mut hist = LatencyHist::new();
        hist.record(Duration::from_micros(3)); // bucket 2 (< 4us)
        hist.record(Duration::from_micros(3));
        hist.record(Duration::from_secs(1)); // overflow bucket

        let report = hist.report();
        assert!(report.starts_with("| bucket | count |\n| --- | --- |\n"));
        assert!(report.contains("| <4us | 2 |"));
        assert!(report.contains("| >=1024us | 1 |"));
        // Empty buckets are omitted.
        assert!(!report.contains("<1us"));
    }

    #[test]
    fn report_on_empty_histogram_is_header_only() {
        let hist = LatencyHist::new();
        assert_eq!(hist.report(), "| bucket | count |\n| --- | --- |\n");
    }
}
