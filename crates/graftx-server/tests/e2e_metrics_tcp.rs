//! End-to-end metrics test over a real TCP loopback socket.
//!
//! A `TcpListener` binds an ephemeral `127.0.0.1` port; a server thread accepts
//! exactly one connection, frames it with a [`StreamTransport`], and serves a
//! session with both a [`VulkanBackend`] and a [`GlBackend`] registered via
//! [`graftx_server::serve_with_metrics`], recording per-API call counts into a
//! shared [`ObsRegistry`]. The registry is handed to the server thread behind an
//! [`Arc`] so the main thread can inspect it after the join. The client
//! completes the handshake, creates a Vulkan instance, and creates an OpenGL
//! context over the same connection, then drops to end the serve loop. After the
//! server joins with `Ok`, the recorded snapshot must show calls for both the
//! "core" (handshake) and "vulkan" namespaces.

use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use graftx_obs::ObsRegistry;
use graftx_server::{GlBackend, Session, VulkanBackend};
use graftx_transport::StreamTransport;

#[test]
fn tcp_metrics_record_core_and_vulkan_across_backends() {
    let session_id: u64 = 0x00C0_FFEE;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
    let addr = listener.local_addr().expect("local addr");

    // The registry is shared between the serving thread and this thread; the
    // server records into it while serving and we read its snapshot after join.
    let registry = Arc::new(ObsRegistry::new());
    let server_registry = Arc::clone(&registry);

    // Server: accept one connection, frame it, and serve a fresh session with
    // both a Vulkan and an OpenGL backend registered, recording per-API metrics
    // until the client disconnects.
    let server_thread = thread::spawn(move || -> io::Result<()> {
        let (stream, _peer) = listener.accept().expect("server accept");
        let mut transport = StreamTransport::new(stream);
        let mut session = Session::new(session_id);
        session.register(Box::new(VulkanBackend::new()));
        session.register(Box::new(GlBackend::new()));
        graftx_server::serve_with_metrics(&mut transport, &mut session, &server_registry)
    });

    // Client: connect over TCP and frame the stream the same way.
    let client_stream = TcpStream::connect(addr).expect("client connect");
    let mut client = StreamTransport::new(client_stream);

    // Core traffic: the handshake is a single HELLO in the core namespace.
    let welcome = graftx_client::handshake(&mut client).expect("handshake");
    assert_eq!(welcome.session_id, session_id);

    // Vulkan traffic: create an instance.
    let _instance =
        graftx_client::vk::create_instance(&mut client, 0, 2, 2).expect("create instance");

    // OpenGL traffic: create a context over the same connection.
    let _context = graftx_client::gl::create_context(&mut client, 3, 3).expect("create context");

    // Drop the client stream to close the connection, ending the serve loop.
    drop(client);
    let result = server_thread.join().expect("join server thread");
    assert!(result.is_ok(), "server serve returned an error: {result:?}");

    // The registry must have attributed the handshake to "core" and the
    // create-instance to "vulkan".
    let snapshot = registry.snapshot();
    assert!(
        snapshot.contains_key("core"),
        "core namespace must be recorded, got {snapshot:?}"
    );
    assert!(
        snapshot.contains_key("vulkan"),
        "vulkan namespace must be recorded, got {snapshot:?}"
    );
    assert!(snapshot["core"].calls >= 1, "handshake recorded for core");
    assert_eq!(snapshot["vulkan"].calls, 1, "one create_instance recorded");
    assert_eq!(snapshot["opengl"].calls, 1, "one create_context recorded");

    // Byte counters are populated for every recorded call.
    assert!(snapshot["core"].bytes_out > 0);
    assert!(snapshot["core"].bytes_in > 0);
    assert!(snapshot["vulkan"].bytes_out > 0);
    assert!(snapshot["vulkan"].bytes_in > 0);
}
