//! End-to-end HIP test: the client completes the handshake, allocates a 1 MiB
//! device buffer, creates a stream, then frees the buffer, all against a server
//! whose session has a registered [`HipBackend`]. The stub backend tracks
//! object lifetimes in generational handle tables but performs no real driver
//! work.

use std::thread;

use graftx_server::{HipBackend, Session};
use graftx_transport::{loopback, Transport};

#[test]
fn handshake_malloc_stream_create_then_free() {
    let (mut client, mut server) = loopback();

    // Server: register the HIP backend, then handle exactly four requests
    // (the handshake Hello, malloc, stream-create, and free), and exit.
    let server_thread = thread::spawn(move || {
        let mut session = Session::new(0x00B1_AC00);
        session.register(Box::new(HipBackend::new()));
        for _ in 0..4 {
            let frame = server.recv().expect("server recv");
            let reply = session.handle(&frame).expect("server handle");
            server.send(&reply).expect("server send");
        }
    });

    // Client: complete the handshake first.
    let welcome = graftx_client::handshake(&mut client).expect("handshake");
    assert_eq!(welcome.session_id, 0x00B1_AC00);

    // Allocate a 1 MiB device buffer.
    let dptr = graftx_client::hip::malloc(&mut client, 1 << 20, 1, 1).expect("malloc");
    // The device pointer must name a `HipDevicePtr` (server kind 30).
    assert_eq!(dptr.kind(), 30);

    // Create a stream.
    let stream = graftx_client::hip::stream_create(&mut client, 2, 2).expect("stream create");
    // The stream must name a `HipStream` (server kind 31).
    assert_eq!(stream.kind(), 31);

    // Free the device buffer; the backend acks with an empty body and the
    // client surfaces a unit `Ok(())`.
    let freed: Result<(), graftx_client::ClientError> =
        graftx_client::hip::free(&mut client, dptr, 3, 3);
    assert!(freed.is_ok());

    server_thread.join().expect("join server");
}
