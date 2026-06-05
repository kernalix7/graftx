//! End-to-end video test: the client completes the handshake, creates a decode
//! session for codec 0 at 1920x1080, decodes two frames, then destroys the
//! session, all against a server whose session has a registered
//! [`VideoBackend`]. The stub backend returns ascending frame indices.

use std::thread;

use graftx_server::{Session, VideoBackend};
use graftx_transport::{loopback, Transport};

#[test]
fn handshake_create_session_decode_two_frames_then_destroy() {
    let (mut client, mut server) = loopback();

    // Server: register the video backend, then handle exactly five requests
    // (the handshake Hello, create-decode-session, two decode-frame calls, and
    // destroy-session), and exit.
    let server_thread = thread::spawn(move || {
        let mut session = Session::new(0x00_5D_E0_00);
        session.register(Box::new(VideoBackend::new()));
        for _ in 0..5 {
            let frame = server.recv().expect("server recv");
            let reply = session.handle(&frame).expect("server handle");
            server.send(&reply).expect("server send");
        }
    });

    // Client: complete the handshake first.
    let welcome = graftx_client::handshake(&mut client).expect("handshake");
    assert_eq!(welcome.session_id, 0x00_5D_E0_00);

    // Create a decode session for codec 0 at 1920x1080.
    let session = graftx_client::video::create_decode_session(&mut client, 0, 1920, 1080, 2, 2)
        .expect("create decode session");
    // The session handle must name a video session (server kind 60).
    assert_eq!(session.kind(), 60);

    // Decode two frames; indices ascend from 0.
    let first = graftx_client::video::decode_frame(&mut client, session, 1024, 3, 3)
        .expect("decode frame 1");
    assert_eq!(first, 0);
    let second = graftx_client::video::decode_frame(&mut client, session, 1024, 4, 4)
        .expect("decode frame 2");
    assert_eq!(second, 1);

    // Destroy the session; the ack maps to `Ok(())`.
    let destroyed = graftx_client::video::destroy_session(&mut client, session, 5, 5);
    assert_eq!(destroyed.expect("destroy session"), ());

    server_thread.join().expect("join server");
}
