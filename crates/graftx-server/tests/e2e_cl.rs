//! End-to-end OpenCL test: the client completes the handshake, creates a
//! context, allocates a buffer in that context, then releases the buffer, all
//! against a server whose session has a registered [`ClBackend`]. The stub
//! backend mints generational handles but performs no real OpenCL work.

use std::thread;

use graftx_server::{ClBackend, Session};
use graftx_transport::{loopback, Transport};

#[test]
fn handshake_create_context_create_buffer_then_release() {
    let (mut client, mut server) = loopback();

    // Server: register the OpenCL backend, then handle exactly four requests
    // (the handshake Hello, create-context, create-buffer, and release-buffer),
    // and exit.
    let server_thread = thread::spawn(move || {
        let mut session = Session::new(0x0040_0000);
        session.register(Box::new(ClBackend::new()));
        for _ in 0..4 {
            let frame = server.recv().expect("server recv");
            let reply = session.handle(&frame).expect("server handle");
            server.send(&reply).expect("server send");
        }
    });

    // Client: complete the handshake first.
    let welcome = graftx_client::handshake(&mut client).expect("handshake");
    assert_eq!(welcome.session_id, 0x0040_0000);

    // Create a context, allocate a buffer in it, then release the buffer.
    let context = graftx_client::cl::create_context(&mut client, 1, 1).expect("create context");
    // The context must name an OpenCL context (server kind 40).
    assert_eq!(context.kind(), 40);

    let mem =
        graftx_client::cl::create_buffer(&mut client, context, 4096, 2, 2).expect("create buffer");
    // The buffer must name an OpenCL memory buffer (server kind 41).
    assert_eq!(mem.kind(), 41);

    let released = graftx_client::cl::release_buffer(&mut client, mem, 3, 3);
    assert!(released.is_ok(), "release_buffer must return Ok(())");
    released.expect("release buffer");

    server_thread.join().expect("join server");
}
