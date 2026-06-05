#![forbid(unsafe_op_in_unsafe_fn)]
//! GraftX protocol codec micro-benchmark.
//!
//! Encodes and decodes a representative Vulkan frame (a create-instance
//! request) a configurable number of times, measures the wall-clock cost with
//! [`graftx_obs::Timer`], and prints the resulting throughput in frames per
//! second and the average cost per round-trip in nanoseconds.
//!
//! Run it with an explicit iteration count, e.g. `graftx-bench 1000`, or with
//! no argument to use [`DEFAULT_ITERATIONS`].

use std::process::ExitCode;

use graftx_obs::Timer;
use graftx_protocol::vk::CreateInstanceRequest;
use graftx_protocol::ProtocolError;

/// Iteration count used when no positive count is supplied on the command line.
const DEFAULT_ITERATIONS: u64 = 100_000;

/// The representative frame whose codec we measure.
///
/// A create-instance request is the first message a client sends, so it is a
/// natural stand-in for "a small, typical Vulkan frame".
fn representative_frame() -> CreateInstanceRequest {
    // Vulkan 1.3.0 packed as the standard `VK_MAKE_API_VERSION(0, 1, 3, 0)`.
    CreateInstanceRequest {
        app_api_version: 0x0040_3000,
    }
}

/// Encode `frame`, decode the bytes back, and return the decoded value.
///
/// This is the unit of work the benchmark times. It is pure: it allocates a
/// fresh buffer, performs one encode and one decode, and returns the round-trip
/// result so callers (and tests) can assert it matches the input.
fn round_trip(frame: &CreateInstanceRequest) -> Result<CreateInstanceRequest, ProtocolError> {
    let mut buf = Vec::new();
    frame.encode(&mut buf);
    CreateInstanceRequest::decode(&buf)
}

/// Result of timing `iterations` round-trips of the representative frame.
#[derive(Debug, Clone, Copy)]
struct BenchReport {
    /// Number of encode/decode round-trips performed.
    iterations: u64,
    /// Total elapsed nanoseconds across all round-trips.
    elapsed_nanos: u128,
}

impl BenchReport {
    /// Average cost of a single round-trip in nanoseconds.
    ///
    /// Returns `0.0` when no iterations ran, avoiding a divide-by-zero.
    fn nanos_per_op(&self) -> f64 {
        if self.iterations == 0 {
            return 0.0;
        }
        self.elapsed_nanos as f64 / self.iterations as f64
    }

    /// Throughput in round-trips per second.
    ///
    /// Returns `0.0` when no time elapsed, avoiding a divide-by-zero.
    fn frames_per_sec(&self) -> f64 {
        if self.elapsed_nanos == 0 {
            return 0.0;
        }
        const NANOS_PER_SEC: f64 = 1_000_000_000.0;
        self.iterations as f64 * NANOS_PER_SEC / self.elapsed_nanos as f64
    }
}

/// Time `iterations` round-trips of the representative frame.
///
/// Propagates the first decode error, if any, so a codec regression surfaces as
/// a failed run rather than misleading timings.
fn bench(iterations: u64) -> Result<BenchReport, ProtocolError> {
    let frame = representative_frame();
    let timer = Timer::start();
    for _ in 0..iterations {
        // `core::hint::black_box` keeps the optimizer from eliding the work.
        let decoded = round_trip(&frame)?;
        let _ = std::hint::black_box(decoded);
    }
    let elapsed_nanos = timer.elapsed().as_nanos();
    Ok(BenchReport {
        iterations,
        elapsed_nanos,
    })
}

/// Parse the iteration count from the first CLI argument.
///
/// A missing or unparseable argument falls back to [`DEFAULT_ITERATIONS`]; the
/// chosen count is returned alongside a flag noting whether the fallback fired
/// so the caller can warn the user.
fn parse_iterations(arg: Option<&str>) -> (u64, bool) {
    match arg {
        Some(raw) => match raw.parse::<u64>() {
            Ok(n) => (n, false),
            Err(_) => (DEFAULT_ITERATIONS, true),
        },
        None => (DEFAULT_ITERATIONS, false),
    }
}

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    let (iterations, fell_back) = parse_iterations(arg.as_deref());
    if fell_back {
        eprintln!(
            "graftx-bench: could not parse iteration count; using default {DEFAULT_ITERATIONS}"
        );
    }

    match bench(iterations) {
        Ok(report) => {
            println!(
                "graftx-bench: {} round-trips in {:.3} ms",
                report.iterations,
                report.elapsed_nanos as f64 / 1_000_000.0,
            );
            println!("  {:.2} frames/sec", report.frames_per_sec());
            println!("  {:.2} ns/op", report.nanos_per_op());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("graftx-bench: codec round-trip failed: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_the_frame() {
        let frame = representative_frame();
        let decoded = round_trip(&frame).expect("representative frame must round-trip");
        assert_eq!(decoded, frame);
    }

    #[test]
    fn round_trip_preserves_arbitrary_versions() {
        for app_api_version in [0u32, 1, 0x0040_3000, u32::MAX] {
            let frame = CreateInstanceRequest { app_api_version };
            let decoded = round_trip(&frame).expect("frame must round-trip");
            assert_eq!(decoded, frame);
        }
    }

    #[test]
    fn bench_reports_the_requested_iteration_count() {
        let report = bench(16).expect("bench must succeed");
        assert_eq!(report.iterations, 16);
    }

    #[test]
    fn zero_iterations_yields_finite_metrics() {
        let report = bench(0).expect("zero-iteration bench must succeed");
        assert_eq!(report.iterations, 0);
        assert_eq!(report.nanos_per_op(), 0.0);
        assert_eq!(report.frames_per_sec(), 0.0);
    }

    #[test]
    fn parse_iterations_reads_a_valid_count() {
        assert_eq!(parse_iterations(Some("1000")), (1000, false));
    }

    #[test]
    fn parse_iterations_falls_back_on_garbage() {
        assert_eq!(
            parse_iterations(Some("not-a-number")),
            (DEFAULT_ITERATIONS, true)
        );
    }

    #[test]
    fn parse_iterations_defaults_when_absent() {
        assert_eq!(parse_iterations(None), (DEFAULT_ITERATIONS, false));
    }
}
