//! End-to-end CUDA test: the client completes the handshake, creates a CUDA
//! context, allocates a 1 MiB device buffer in that context, then frees the
//! buffer, all against a server whose session has a registered
//! [`CudaBackend`]. The stub backend tracks object lifetimes in generational
//! handle tables but performs no real driver work.

use std::thread;

use graftx_server::{CudaBackend, Session};
use graftx_transport::{loopback, Transport};

#[test]
fn handshake_ctx_create_mem_alloc_then_mem_free() {
    let (mut client, mut server) = loopback();

    // Server: register the CUDA backend, then handle exactly four requests
    // (the handshake Hello, ctx-create, mem-alloc, and mem-free), and exit.
    let server_thread = thread::spawn(move || {
        let mut session = Session::new(0x00CA_FE00);
        session.register(Box::new(CudaBackend::new()));
        for _ in 0..4 {
            let frame = server.recv().expect("server recv");
            let reply = session.handle(&frame).expect("server handle");
            server.send(&reply).expect("server send");
        }
    });

    // Client: complete the handshake first.
    let welcome = graftx_client::handshake(&mut client).expect("handshake");
    assert_eq!(welcome.session_id, 0x00CA_FE00);

    // Create a context on device 0.
    let ctx = graftx_client::cuda::ctx_create(&mut client, 0, 2, 2).expect("ctx create");
    // The context must name a `CudaContext` (server kind 20).
    assert_eq!(ctx.kind(), 20);

    // Allocate a 1 MiB device buffer in that context.
    let dptr = graftx_client::cuda::mem_alloc(&mut client, ctx, 1 << 20, 3, 3).expect("mem alloc");
    // The device pointer must name a `CudaDevicePtr` (server kind 21).
    assert_eq!(dptr.kind(), 21);

    // Free the device buffer; the backend acks with an empty body and the
    // client surfaces a unit `Ok(())`.
    let freed: Result<(), graftx_client::ClientError> =
        graftx_client::cuda::mem_free(&mut client, dptr, 4, 4);
    assert!(freed.is_ok());

    server_thread.join().expect("join server");
}
