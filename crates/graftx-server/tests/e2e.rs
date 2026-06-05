//! End-to-end M0 test: client and server exchange a handshake and a no-op
//! round-trip over the in-process loopback transport.

use std::thread;

use graftx_server::Session;
use graftx_transport::{loopback, Transport};

#[test]
fn handshake_then_noop_roundtrip() {
    let (mut client, mut server) = loopback();

    // Server: handle exactly two requests (Hello, then Noop), then exit.
    let server_thread = thread::spawn(move || {
        let mut session = Session::new(0xABCD_1234);
        for _ in 0..2 {
            let frame = server.recv().expect("server recv");
            let reply = session.handle(&frame).expect("server handle");
            server.send(&reply).expect("server send");
        }
        session.session_id
    });

    // Client: handshake, then a no-op.
    let welcome = graftx_client::handshake(&mut client).expect("handshake");
    assert_eq!(welcome.session_id, 0xABCD_1234);
    assert_eq!(welcome.proto_major, graftx_protocol::PROTOCOL_MAJOR);
    assert_eq!(
        welcome.max_frame_body,
        graftx_protocol::DEFAULT_MAX_FRAME_BODY
    );

    graftx_client::noop(&mut client, 2, 2).expect("noop round-trip");

    let id = server_thread.join().expect("join server");
    assert_eq!(id, 0xABCD_1234);
}
