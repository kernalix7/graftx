#![forbid(unsafe_op_in_unsafe_fn)]
//! GraftX in-process load generator.
//!
//! Drives many forwarded GPU calls through a real client/server pair connected
//! by an in-process [`graftx_transport::loopback`] channel, with no hypervisor
//! or GPU involved. A server [`Session`](graftx_server::Session) carrying a
//! [`VulkanBackend`](graftx_server::VulkanBackend) runs on its own thread and
//! records every handled frame into an [`ObsRegistry`](graftx_obs::ObsRegistry)
//! via [`serve_with_metrics`](graftx_server::serve_with_metrics); the client
//! thread runs the handshake and then `N` iterations of `vkCreateInstance`
//! followed by `vkEnumeratePhysicalDevices`.
//!
//! On completion the binary prints the registry's per-API report and the
//! measured throughput in operations per second (timed with
//! [`Timer`](graftx_obs::Timer)). Run it with an explicit op count, e.g.
//! `graftx-loadtest 1000`, or with no argument to use [`DEFAULT_OPS`].

use std::process::ExitCode;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use graftx_client::{Client, ClientError};
use graftx_obs::{CallStats, ObsRegistry, Timer};
use graftx_server::{serve_with_metrics, Session, VulkanBackend};
use graftx_transport::loopback;

/// Op count used when no positive count is supplied on the command line.
const DEFAULT_OPS: usize = 10_000;

/// Server-assigned session id used for the in-process loopback session.
const SESSION_ID: u64 = 0x6C6F_6164; // "load"

/// Vulkan API version the load client advertises (`VK_MAKE_API_VERSION(0, 1, 3, 0)`).
const APP_API_VERSION: u32 = 0x0040_3000;

/// Outcome of a load run: the aggregate call statistics and how long the client
/// loop took to issue them.
#[derive(Debug, Clone)]
struct LoadReport {
    /// Per-API statistics recorded by the server.
    registry: Arc<ObsRegistry>,
    /// Aggregate statistics across every API.
    total: CallStats,
    /// Wall-clock time spent in the client loop (handshake plus all ops).
    elapsed: Duration,
}

impl LoadReport {
    /// Throughput in operations per second over the measured elapsed time.
    ///
    /// "Operations" counts every recorded call (handshake plus the per-iteration
    /// Vulkan calls), matching [`CallStats::calls`]. Returns `0.0` when no time
    /// elapsed, avoiding a divide-by-zero.
    fn ops_per_sec(&self) -> f64 {
        let nanos = self.elapsed.as_nanos();
        if nanos == 0 {
            return 0.0;
        }
        const NANOS_PER_SEC: f64 = 1_000_000_000.0;
        self.total.calls as f64 * NANOS_PER_SEC / nanos as f64
    }
}

/// Drive `n` iterations of the Vulkan op pair through an in-process loopback
/// server, recording every handled call into `registry`.
///
/// Spins up a server thread that registers a [`VulkanBackend`] on a fresh
/// [`Session`] and serves the loopback endpoint with
/// [`serve_with_metrics`], then on the calling thread connects a [`Client`]
/// (running the handshake) and issues `n` rounds of `vkCreateInstance` +
/// `vkEnumeratePhysicalDevices`. Dropping the client closes the loopback, which
/// ends the server loop cleanly so its thread can be joined.
///
/// Returns the measured client-loop [`Duration`] on success. Any client-side
/// [`ClientError`] (including a server thread that failed before the loop
/// completed) is propagated.
fn drive(n: usize, registry: &Arc<ObsRegistry>) -> Result<Duration, ClientError> {
    let (client_end, server_end) = loopback();

    let server_registry = Arc::clone(registry);
    let server = thread::spawn(move || {
        let mut session = Session::new(SESSION_ID);
        session.register(Box::new(VulkanBackend::new()));
        let mut transport = server_end;
        serve_with_metrics(&mut transport, &mut session, &server_registry)
    });

    // Run the client loop, capturing any error so the server thread is always
    // joined before returning (dropping the client closes the loopback).
    let timer = Timer::start();
    let result = run_client(n, client_end);
    let elapsed = timer.elapsed();

    // The client has been dropped by `run_client`, closing its loopback end, so
    // the server loop has returned; joining it surfaces any transport error.
    let serve_result = server.join();

    result?;
    match serve_result {
        Ok(Ok(())) => Ok(elapsed),
        Ok(Err(err)) => Err(ClientError::Transport(err)),
        Err(_panic) => Err(ClientError::Transport(std::io::Error::other(
            "load server thread panicked",
        ))),
    }
}

/// Connect a [`Client`] over `client_end` and issue `n` rounds of the Vulkan op
/// pair, dropping the client (and so closing the loopback) when finished.
///
/// Each round issues `vkCreateInstance` and then
/// `vkEnumeratePhysicalDevices` against the returned instance, discarding the
/// results: the load generator only cares that the round-trip completed.
fn run_client(n: usize, client_end: graftx_transport::Loopback) -> Result<(), ClientError> {
    let mut client = Client::connect(client_end)?;
    for _ in 0..n {
        let instance = client.vk_create_instance(APP_API_VERSION)?;
        let _devices = client.vk_enumerate_physical_devices(instance)?;
    }
    // Dropping `client` here closes the loopback so the server loop ends.
    Ok(())
}

/// Drive `n` ops through the in-process loopback server and return the
/// aggregate [`CallStats`] the server recorded.
///
/// This is the testable entry point: it owns a fresh [`ObsRegistry`] so it does
/// not touch the process-global one, runs [`drive`], and folds the per-API
/// statistics into a single total via [`ObsRegistry::total`]. A client or server
/// failure is reported by returning [`CallStats::default`] (an all-zero total),
/// which a caller can distinguish from a successful run by its zero call count.
///
/// `main` drives the load through [`drive`] directly so it can also render the
/// per-API report and throughput; this wrapper exists as the stable, easily
/// testable entry point, so it is only exercised by the unit tests.
#[cfg_attr(not(test), allow(dead_code))]
fn run_load(n: usize) -> CallStats {
    let registry = Arc::new(ObsRegistry::new());
    match drive(n, &registry) {
        Ok(_elapsed) => registry.total(),
        Err(_err) => CallStats::default(),
    }
}

/// Parse the op count from the first CLI argument.
///
/// A missing or unparseable argument falls back to [`DEFAULT_OPS`]; the chosen
/// count is returned alongside a flag noting whether the fallback fired so the
/// caller can warn the user.
fn parse_ops(arg: Option<&str>) -> (usize, bool) {
    match arg {
        Some(raw) => match raw.parse::<usize>() {
            Ok(n) => (n, false),
            Err(_) => (DEFAULT_OPS, true),
        },
        None => (DEFAULT_OPS, false),
    }
}

fn main() -> ExitCode {
    let arg = std::env::args().nth(1);
    let (ops, fell_back) = parse_ops(arg.as_deref());
    if fell_back {
        eprintln!("graftx-loadtest: could not parse op count; using default {DEFAULT_OPS}");
    }

    let registry = Arc::new(ObsRegistry::new());
    let elapsed = match drive(ops, &registry) {
        Ok(elapsed) => elapsed,
        Err(err) => {
            eprintln!("graftx-loadtest: load run failed: {err}");
            return ExitCode::FAILURE;
        }
    };

    let report = LoadReport {
        registry: Arc::clone(&registry),
        total: registry.total(),
        elapsed,
    };

    print!("{}", report.registry.report());
    println!(
        "graftx-loadtest: {} ops ({} iterations) in {:.3} ms",
        report.total.calls,
        ops,
        report.elapsed.as_nanos() as f64 / 1_000_000.0,
    );
    println!("  {:.2} ops/sec", report.ops_per_sec());
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each loop iteration records two Vulkan calls (create-instance and
    /// enumerate-physical-devices); the handshake records one core call.
    const CALLS_PER_ITERATION: u64 = 2;
    const HANDSHAKE_CALLS: u64 = 1;

    #[test]
    fn run_load_records_expected_call_count() {
        let n = 32usize;
        let total = run_load(n);
        let expected = HANDSHAKE_CALLS + CALLS_PER_ITERATION * n as u64;
        assert_eq!(
            total.calls, expected,
            "unexpected total call count: {total:?}"
        );
        // Every recorded call moved bytes in both directions.
        assert!(total.bytes_out > 0, "expected outbound bytes: {total:?}");
        assert!(total.bytes_in > 0, "expected inbound bytes: {total:?}");
    }

    #[test]
    fn run_load_with_zero_ops_records_only_the_handshake() {
        let total = run_load(0);
        assert_eq!(
            total.calls, HANDSHAKE_CALLS,
            "zero ops should still handshake"
        );
    }

    #[test]
    fn drive_attributes_calls_per_api() {
        let registry = Arc::new(ObsRegistry::new());
        let elapsed = drive(8, &registry).expect("load run must succeed");
        assert!(elapsed >= Duration::ZERO);

        let snapshot = registry.snapshot();
        // The handshake is core traffic; the per-iteration calls are Vulkan.
        assert_eq!(snapshot["core"].calls, HANDSHAKE_CALLS);
        assert_eq!(snapshot["vulkan"].calls, CALLS_PER_ITERATION * 8);
        // Only the two namespaces that saw traffic are present.
        assert_eq!(snapshot.len(), 2);
    }

    #[test]
    fn ops_per_sec_is_zero_when_no_time_elapsed() {
        let report = LoadReport {
            registry: Arc::new(ObsRegistry::new()),
            total: CallStats {
                calls: 100,
                bytes_out: 10,
                bytes_in: 20,
            },
            elapsed: Duration::ZERO,
        };
        assert_eq!(report.ops_per_sec(), 0.0);
    }

    #[test]
    fn ops_per_sec_scales_with_call_count() {
        let report = LoadReport {
            registry: Arc::new(ObsRegistry::new()),
            total: CallStats {
                calls: 2_000,
                bytes_out: 0,
                bytes_in: 0,
            },
            elapsed: Duration::from_secs(2),
        };
        // 2000 calls over 2 seconds is 1000 ops/sec.
        assert!((report.ops_per_sec() - 1_000.0).abs() < 1e-6);
    }

    #[test]
    fn parse_ops_reads_a_valid_count() {
        assert_eq!(parse_ops(Some("1000")), (1000, false));
    }

    #[test]
    fn parse_ops_falls_back_on_garbage() {
        assert_eq!(parse_ops(Some("not-a-number")), (DEFAULT_OPS, true));
    }

    #[test]
    fn parse_ops_defaults_when_absent() {
        assert_eq!(parse_ops(None), (DEFAULT_OPS, false));
    }
}
