//! End-to-end OpenGL test: the client completes the handshake, creates a GL
//! context, makes it current, then generates a buffer in that context, all
//! against a server whose session has a registered [`GlBackend`]. The stub
//! backend mints generational handles but performs no real GL work.

use std::thread;

use graftx_server::{GlBackend, Session};
use graftx_transport::{loopback, Transport};

#[test]
fn handshake_create_context_make_current_then_gen_buffer() {
    let (mut client, mut server) = loopback();

    // Server: register the OpenGL backend, then handle exactly four requests
    // (the handshake Hello, create-context, make-current, and gen-buffer), and
    // exit.
    let server_thread = thread::spawn(move || {
        let mut session = Session::new(0x0061_0000);
        session.register(Box::new(GlBackend::new()));
        for _ in 0..4 {
            let frame = server.recv().expect("server recv");
            let reply = session.handle(&frame).expect("server handle");
            server.send(&reply).expect("server send");
        }
    });

    // Client: complete the handshake first.
    let welcome = graftx_client::handshake(&mut client).expect("handshake");
    assert_eq!(welcome.session_id, 0x0061_0000);

    // Create a context, make it current, then generate a buffer in it.
    let context = graftx_client::gl::create_context(&mut client, 2, 2).expect("create context");
    // The context must name a GL context (server kind 10).
    assert_eq!(context.kind(), 10);

    graftx_client::gl::make_current(&mut client, context, 3, 3).expect("make current");

    let buffer = graftx_client::gl::gen_buffer(&mut client, context, 4, 4).expect("gen buffer");
    // The buffer must name a GL buffer (server kind 11).
    assert_eq!(buffer.kind(), 11);

    server_thread.join().expect("join server");
}
