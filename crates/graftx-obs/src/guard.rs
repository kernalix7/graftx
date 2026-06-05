//! RAII helper that records a single call on drop.
//!
//! [`CallGuard`] ties the lifecycle of one forwarded GraftX call to a scope: it
//! enters a [`call_span`](crate::call_span) for the duration of the guard and,
//! when dropped, flushes exactly one [`record`](crate::ObsRegistry::record) to
//! the borrowed [`ObsRegistry`]. Byte counts default to zero and are set with
//! [`set_bytes`](CallGuard::set_bytes) once known.

use tracing::span::EnteredSpan;

use crate::{call_span, ObsRegistry};

/// Scope guard that records one call against an [`ObsRegistry`] on drop.
///
/// Construct with [`begin`]; the associated tracing span is entered for the
/// guard's lifetime and exited when the guard is dropped, at which point a
/// single call is recorded with the most recently set byte counts.
#[derive(Debug)]
pub struct CallGuard<'a> {
    registry: &'a ObsRegistry,
    api: &'static str,
    bytes_out: u64,
    bytes_in: u64,
    // Held for its `Drop`, which exits the span; kept last so it is dropped
    // after the registry record runs.
    _span: EnteredSpan,
}

impl<'a> CallGuard<'a> {
    /// Update the byte counts that will be recorded when the guard is dropped.
    ///
    /// Each call overwrites the previous values; only the final values are
    /// recorded, and the guard always records exactly one call regardless of
    /// how many times this is invoked.
    pub fn set_bytes(&mut self, bytes_out: u64, bytes_in: u64) {
        self.bytes_out = bytes_out;
        self.bytes_in = bytes_in;
    }
}

impl Drop for CallGuard<'_> {
    fn drop(&mut self) {
        self.registry
            .record(self.api, self.bytes_out, self.bytes_in);
    }
}

/// Begin a guarded call against `registry`, entering its tracing span.
///
/// The span is created via [`call_span`] and entered immediately; byte counts
/// start at zero and can be updated with [`CallGuard::set_bytes`] before the
/// guard is dropped.
pub fn begin<'a>(registry: &'a ObsRegistry, api: &'static str, opcode: u32) -> CallGuard<'a> {
    let span = call_span(api, opcode).entered();
    CallGuard {
        registry,
        api,
        bytes_out: 0,
        bytes_in: 0,
        _span: span,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_records_one_call_with_set_bytes() {
        let registry = ObsRegistry::new();
        {
            let mut guard = begin(&registry, "vulkan", 0x10);
            guard.set_bytes(10, 20);
        }

        let snapshot = registry.snapshot();
        assert_eq!(
            snapshot["vulkan"],
            crate::CallStats {
                calls: 1,
                bytes_out: 10,
                bytes_in: 20,
            }
        );
    }

    #[test]
    fn guard_without_set_bytes_records_zero_bytes() {
        let registry = ObsRegistry::new();
        {
            let _guard = begin(&registry, "vulkan", 0x10);
        }

        let snapshot = registry.snapshot();
        assert_eq!(
            snapshot["vulkan"],
            crate::CallStats {
                calls: 1,
                bytes_out: 0,
                bytes_in: 0,
            }
        );
    }

    #[test]
    fn two_guards_in_sequence_record_two_calls() {
        let registry = ObsRegistry::new();
        {
            let mut guard = begin(&registry, "vulkan", 0x10);
            guard.set_bytes(10, 20);
        }
        {
            let mut guard = begin(&registry, "vulkan", 0x11);
            guard.set_bytes(3, 4);
        }

        let snapshot = registry.snapshot();
        assert_eq!(
            snapshot["vulkan"],
            crate::CallStats {
                calls: 2,
                bytes_out: 13,
                bytes_in: 24,
            }
        );
    }

    #[test]
    fn set_bytes_overwrites_and_records_once() {
        let registry = ObsRegistry::new();
        {
            let mut guard = begin(&registry, "vulkan", 0x10);
            guard.set_bytes(1, 1);
            guard.set_bytes(10, 20);
        }

        let snapshot = registry.snapshot();
        assert_eq!(
            snapshot["vulkan"],
            crate::CallStats {
                calls: 1,
                bytes_out: 10,
                bytes_in: 20,
            }
        );
    }

    #[test]
    fn guard_span_entry_does_not_panic_under_subscriber() {
        let subscriber = tracing_subscriber::FmtSubscriber::builder()
            .with_max_level(tracing::Level::INFO)
            .with_test_writer()
            .finish();
        tracing::subscriber::with_default(subscriber, || {
            let registry = ObsRegistry::new();
            {
                let mut guard = begin(&registry, "vulkan", 0x42);
                tracing::info!("inside guarded call");
                guard.set_bytes(7, 8);
            }
            let snapshot = registry.snapshot();
            assert_eq!(
                snapshot["vulkan"],
                crate::CallStats {
                    calls: 1,
                    bytes_out: 7,
                    bytes_in: 8,
                }
            );
        });
    }
}
