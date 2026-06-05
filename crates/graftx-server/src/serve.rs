//! Transport-driven serve loop.
//!
//! Drives a [`Session`](crate::Session) over any [`Transport`]: receive a
//! frame, hand it to the session, send back the reply. A clean peer close
//! (`UnexpectedEof`/`BrokenPipe`) ends the loop without error; any other
//! transport failure propagates. [`serve_tcp`] layers this over a
//! [`std::net::TcpListener`], framing each accepted socket with
//! [`StreamTransport`](graftx_transport::StreamTransport) and serving it with a
//! fresh session.

use std::io;
use std::net::{TcpListener, TcpStream};

use graftx_obs::ObsRegistry;
use graftx_protocol as proto;
use graftx_transport::{StreamTransport, Transport};

use crate::Session;

/// Map an API-namespace byte (the high byte of an opcode, as returned by
/// [`proto::opcode_api`]) to a stable, human-readable name.
///
/// The returned `&'static str` is used as the registry key in
/// [`serve_with_metrics`], so it is intentionally a fixed identifier per API
/// rather than a per-call label. Any byte outside the known [`proto::ApiId`]
/// range maps to `"unknown"`.
fn api_name(api: u8) -> &'static str {
    match api {
        0x00 => "core",
        0x01 => "vulkan",
        0x02 => "opengl",
        0x03 => "cuda",
        0x04 => "opencl",
        0x05 => "hip",
        0x06 => "level_zero",
        0x07 => "video",
        0x08 => "webgpu",
        0x09 => "optix",
        0x0A => "sycl",
        0x0B => "amf",
        _ => "unknown",
    }
}

/// Serve one session over a transport until the peer closes or an error occurs.
///
/// Each iteration receives one frame and dispatches it to
/// [`Session::handle`]. On a successful handle the reply is sent back. If the
/// session reports a [`ProtocolError`](graftx_protocol::ProtocolError) the
/// session is stopped cleanly (`Ok(())`); a later milestone will instead send
/// an error frame to the peer. A `recv` that fails with `UnexpectedEof` or
/// `BrokenPipe` is treated as a clean peer close and returns `Ok(())`; any
/// other `recv`/`send` error propagates.
pub fn serve<T: Transport>(transport: &mut T, session: &mut Session) -> io::Result<()> {
    loop {
        let frame = match transport.recv() {
            Ok(frame) => frame,
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
                ) =>
            {
                return Ok(());
            }
            Err(err) => return Err(err),
        };

        match session.handle(&frame) {
            Ok(reply) => transport.send(&reply)?,
            // A protocol-level error ends the session cleanly for now; a future
            // milestone will send a structured error frame before stopping.
            Err(_protocol_error) => return Ok(()),
        }
    }
}

/// Serve one session over a transport, recording per-API metrics into `registry`.
///
/// Behaves exactly like [`serve`] — same clean-close and error-propagation
/// semantics — but for every frame that the session handles successfully it
/// records one call into `registry`, keyed by the API name of the request's
/// opcode (via [`api_name`] over [`proto::opcode_api`]). The recorded
/// `bytes_out` is the inbound request frame length and `bytes_in` is the reply
/// frame length, matching the [`ObsRegistry`] convention where "out"/"in" are
/// relative to the forwarded call rather than the socket.
///
/// A frame whose header cannot be decoded is still forwarded to
/// [`Session::handle`] (which surfaces the same protocol error as [`serve`]);
/// no metric is recorded for it, since its API namespace is unknown.
pub fn serve_with_metrics<T: Transport>(
    transport: &mut T,
    session: &mut Session,
    registry: &ObsRegistry,
) -> io::Result<()> {
    loop {
        let frame = match transport.recv() {
            Ok(frame) => frame,
            Err(err)
                if matches!(
                    err.kind(),
                    io::ErrorKind::UnexpectedEof | io::ErrorKind::BrokenPipe
                ) =>
            {
                return Ok(());
            }
            Err(err) => return Err(err),
        };

        // Peek the opcode's API namespace before handling so a successful
        // handle can be attributed. A malformed header has no usable opcode, so
        // it is left unattributed and the error surfaces from `handle` below.
        let api = proto::decode_frame(&frame)
            .ok()
            .map(|(h, _)| api_name(proto::opcode_api(h.opcode)));

        match session.handle(&frame) {
            Ok(reply) => {
                transport.send(&reply)?;
                if let Some(api) = api {
                    registry.record(api, frame.len() as u64, reply.len() as u64);
                }
            }
            // A protocol-level error ends the session cleanly for now; a future
            // milestone will send a structured error frame before stopping.
            Err(_protocol_error) => return Ok(()),
        }
    }
}

/// Accept TCP connections on `addr` and serve each with a fresh session.
///
/// Binds a [`TcpListener`], then for every accepted [`TcpStream`] wraps it in a
/// [`StreamTransport`] and runs [`serve`] with a session produced by
/// `make_session`. A serve error on one connection is logged and ignored so the
/// listener keeps accepting subsequent connections; this function only returns
/// if accepting itself fails.
pub fn serve_tcp<F: FnMut() -> Session>(addr: &str, mut make_session: F) -> io::Result<()> {
    let listener = TcpListener::bind(addr)?;
    for stream in listener.incoming() {
        let stream: TcpStream = match stream {
            Ok(stream) => stream,
            Err(err) => {
                eprintln!("graftx-server: accept failed: {err}");
                continue;
            }
        };
        let mut transport = StreamTransport::new(stream);
        let mut session = make_session();
        if let Err(err) = serve(&mut transport, &mut session) {
            eprintln!("graftx-server: connection serve error: {err}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    use graftx_transport::loopback;

    #[test]
    fn api_name_maps_known_namespaces_and_falls_back() {
        assert_eq!(api_name(0x00), "core");
        assert_eq!(api_name(0x01), "vulkan");
        assert_eq!(api_name(0x0B), "amf");
        // Every defined ApiId byte resolves to a non-"unknown" label.
        for api in 0x00u8..=0x0B {
            assert_ne!(api_name(api), "unknown", "api {api:#04x} should be named");
        }
        // Bytes outside the defined range fall back.
        assert_eq!(api_name(0x0C), "unknown");
        assert_eq!(api_name(0xFF), "unknown");
    }

    #[test]
    fn serve_with_metrics_records_per_api_call_counts() {
        let (mut server_side, mut client_side) = loopback();
        let registry = Arc::new(ObsRegistry::new());

        let server_registry = Arc::clone(&registry);
        let server = thread::spawn(move || {
            let mut session = Session::new(0x51);
            session.register(Box::new(crate::VulkanBackend::new()));
            serve_with_metrics(&mut server_side, &mut session, &server_registry)
        });

        // Core traffic: one handshake (HELLO) plus two no-ops.
        let welcome = graftx_client::handshake(&mut client_side).expect("handshake");
        assert_eq!(welcome.session_id, 0x51);
        graftx_client::noop(&mut client_side, 2, 2).expect("first noop");
        graftx_client::noop(&mut client_side, 3, 3).expect("second noop");

        // Vulkan traffic: create an instance, then enumerate its devices.
        let instance =
            graftx_client::vk::create_instance(&mut client_side, 0, 4, 4).expect("create instance");
        let devices =
            graftx_client::vk::enumerate_physical_devices(&mut client_side, instance, 5, 5)
                .expect("enumerate devices");
        assert_eq!(devices.len(), 1);

        // Dropping the client closes the loopback, so the loop returns Ok(()).
        drop(client_side);
        let result = server.join().expect("server thread");
        assert!(result.is_ok());

        let snapshot = registry.snapshot();
        // HELLO + two NOOPs are all in the core namespace.
        assert_eq!(snapshot["core"].calls, 3);
        // create_instance + enumerate_physical_devices are both Vulkan.
        assert_eq!(snapshot["vulkan"].calls, 2);
        // Only the two namespaces that saw traffic are present.
        assert_eq!(snapshot.len(), 2);

        // Byte counters are populated for every recorded call.
        assert!(snapshot["core"].bytes_out > 0);
        assert!(snapshot["core"].bytes_in > 0);
        assert!(snapshot["vulkan"].bytes_out > 0);
        assert!(snapshot["vulkan"].bytes_in > 0);
    }

    #[test]
    fn serve_handles_frames_then_returns_on_peer_drop() {
        let (mut server_side, mut client_side) = loopback();

        let server = thread::spawn(move || {
            let mut session = Session::new(0x51);
            session.register(Box::new(crate::VulkanBackend::new()));
            serve(&mut server_side, &mut session)
        });

        // Client: handshake, then a noop round-trip against the served session.
        let welcome = graftx_client::handshake(&mut client_side).expect("handshake");
        assert_eq!(welcome.session_id, 0x51);
        graftx_client::noop(&mut client_side, 2, 2).expect("noop");

        // Dropping the client closes the loopback, so serve must return Ok(()).
        drop(client_side);
        let result = server.join().expect("server thread");
        assert!(result.is_ok());
    }
}
