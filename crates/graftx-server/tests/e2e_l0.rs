//! End-to-end Level Zero test: the client completes the handshake, creates a
//! Level Zero context, allocates a 1 MiB device buffer in that context, then
//! frees the buffer, all against a server whose session has a registered
//! [`L0Backend`]. The stub backend tracks object lifetimes in generational
//! handle tables but performs no real driver work.

use std::thread;

use graftx_server::{L0Backend, Session};
use graftx_transport::{loopback, Transport};

#[test]
fn handshake_ctx_create_mem_alloc_then_mem_free() {
    let (mut client, mut server) = loopback();

    // Server: register the Level Zero backend, then handle exactly four
    // requests (the handshake Hello, context-create, mem-alloc, and mem-free),
    // and exit.
    let server_thread = thread::spawn(move || {
        let mut session = Session::new(0x0000_0E00);
        session.register(Box::new(L0Backend::new()));
        for _ in 0..4 {
            let frame = server.recv().expect("server recv");
            let reply = session.handle(&frame).expect("server handle");
            server.send(&reply).expect("server send");
        }
    });

    // Client: complete the handshake first.
    let welcome = graftx_client::handshake(&mut client).expect("handshake");
    assert_eq!(welcome.session_id, 0x0000_0E00);

    // Create a Level Zero context.
    let ctx = graftx_client::l0::context_create(&mut client, 2, 2).expect("context create");
    // The context must name an `L0Context` (server kind 50).
    assert_eq!(ctx.kind(), 50);

    // Allocate a 1 MiB device buffer in that context.
    let ptr = graftx_client::l0::mem_alloc_device(&mut client, ctx, 1 << 20, 3, 3)
        .expect("mem alloc device");
    // The device pointer must name an `L0DeviceMem` (server kind 51).
    assert_eq!(ptr.kind(), 51);

    // Free the device buffer; the backend acks with an empty body and the
    // client surfaces a unit `Ok(())`.
    let freed: Result<(), graftx_client::ClientError> =
        graftx_client::l0::mem_free(&mut client, ptr, 4, 4);
    assert!(freed.is_ok());

    server_thread.join().expect("join server");
}
