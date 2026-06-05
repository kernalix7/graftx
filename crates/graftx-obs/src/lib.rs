#![forbid(unsafe_op_in_unsafe_fn)]
//! GraftX observability.
//!
//! Lightweight building blocks for instrumenting GraftX call traffic: a
//! [`CallStats`] accumulator for per-API call/byte counters and a
//! [`call_span`] helper that names a span for a single forwarded call.
//!
//! The counters use saturating arithmetic so a long-lived server cannot panic
//! on overflow; statistics are best-effort and clamping at `u64::MAX` is the
//! intended behaviour rather than wrapping or aborting.
//!
//! For aggregating statistics across many APIs and threads, see the
//! [`ObsRegistry`] in the [`registry`] module.

mod registry;

pub use registry::ObsRegistry;

/// Accumulated statistics for a stream of GraftX calls.
///
/// All mutating operations saturate at [`u64::MAX`]; they never wrap or panic.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CallStats {
    /// Number of calls observed.
    pub calls: u64,
    /// Total bytes sent (request/arguments) across those calls.
    pub bytes_out: u64,
    /// Total bytes received (response/results) across those calls.
    pub bytes_in: u64,
}

impl CallStats {
    /// Record a single call, adding its outbound and inbound byte counts.
    ///
    /// Increments [`calls`](Self::calls) by one and adds the byte counts,
    /// saturating each field at [`u64::MAX`].
    pub fn record(&mut self, bytes_out: u64, bytes_in: u64) {
        self.calls = self.calls.saturating_add(1);
        self.bytes_out = self.bytes_out.saturating_add(bytes_out);
        self.bytes_in = self.bytes_in.saturating_add(bytes_in);
    }

    /// Fold another set of statistics into this one, saturating each field.
    pub fn merge(&mut self, other: &CallStats) {
        self.calls = self.calls.saturating_add(other.calls);
        self.bytes_out = self.bytes_out.saturating_add(other.bytes_out);
        self.bytes_in = self.bytes_in.saturating_add(other.bytes_in);
    }
}

/// Create an `info`-level span describing a single GraftX call.
///
/// The span is named `graftx.call` and carries the API name and opcode as
/// fields; entering it scopes any nested events to this call.
pub fn call_span(api: &str, opcode: u32) -> tracing::Span {
    tracing::info_span!("graftx.call", api, opcode)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_accumulates_calls_and_bytes() {
        let mut stats = CallStats::default();
        stats.record(10, 20);
        stats.record(5, 7);
        assert_eq!(
            stats,
            CallStats {
                calls: 2,
                bytes_out: 15,
                bytes_in: 27,
            }
        );
    }

    #[test]
    fn record_saturates_on_overflow() {
        let mut stats = CallStats {
            calls: u64::MAX,
            bytes_out: u64::MAX,
            bytes_in: u64::MAX - 1,
        };
        stats.record(1, 5);
        assert_eq!(
            stats,
            CallStats {
                calls: u64::MAX,
                bytes_out: u64::MAX,
                bytes_in: u64::MAX,
            }
        );
    }

    #[test]
    fn merge_sums_fields() {
        let mut total = CallStats {
            calls: 3,
            bytes_out: 100,
            bytes_in: 200,
        };
        let other = CallStats {
            calls: 4,
            bytes_out: 50,
            bytes_in: 60,
        };
        total.merge(&other);
        assert_eq!(
            total,
            CallStats {
                calls: 7,
                bytes_out: 150,
                bytes_in: 260,
            }
        );
    }

    #[test]
    fn merge_saturates_on_overflow() {
        let mut total = CallStats {
            calls: u64::MAX,
            bytes_out: u64::MAX - 2,
            bytes_in: 1,
        };
        let other = CallStats {
            calls: 1,
            bytes_out: 10,
            bytes_in: u64::MAX,
        };
        total.merge(&other);
        assert_eq!(
            total,
            CallStats {
                calls: u64::MAX,
                bytes_out: u64::MAX,
                bytes_in: u64::MAX,
            }
        );
    }

    #[test]
    fn call_span_constructs_and_enters_under_subscriber() {
        let subscriber = tracing_subscriber::FmtSubscriber::builder()
            .with_max_level(tracing::Level::INFO)
            .with_test_writer()
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let span = call_span("NtCreateFile", 0x42);
            let _guard = span.enter();
            tracing::info!("inside call span");
        });
    }
}
