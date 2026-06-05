//! End-to-end WebGPU test: the client completes the handshake, requests a
//! logical device, creates a 4 KiB buffer on that device, then destroys the
//! buffer, all against a server whose session has a registered
//! [`WebGpuBackend`]. The stub backend tracks object lifetimes in generational
//! handle tables but performs no real driver work.

use std::thread;

use graftx_server::{Session, WebGpuBackend};
use graftx_transport::{loopback, Transport};

#[test]
fn handshake_request_device_create_buffer_then_destroy_buffer() {
    let (mut client, mut server) = loopback();

    // Server: register the WebGPU backend, then handle exactly four requests
    // (the handshake Hello, request-device, create-buffer, and destroy-buffer),
    // and exit.
    let server_thread = thread::spawn(move || {
        let mut session = Session::new(0x00_BE_EF_00);
        session.register(Box::new(WebGpuBackend::new()));
        for _ in 0..4 {
            let frame = server.recv().expect("server recv");
            let reply = session.handle(&frame).expect("server handle");
            server.send(&reply).expect("server send");
        }
    });

    // Client: complete the handshake first.
    let welcome = graftx_client::handshake(&mut client).expect("handshake");
    assert_eq!(welcome.session_id, 0x00_BE_EF_00);

    // Request a logical device.
    let device = graftx_client::wgpu::request_device(&mut client, 2, 2).expect("request device");
    // The device must name a `WgpuDevice` (server kind 70).
    assert_eq!(device.kind(), 70);

    // Create a 4 KiB buffer on that device with no usage flags.
    let buffer = graftx_client::wgpu::create_buffer(&mut client, device, 4096, 0, 3, 3)
        .expect("create buffer");
    // The buffer must name a `WgpuBuffer` (server kind 71).
    assert_eq!(buffer.kind(), 71);

    // Destroy the buffer; the backend acks with an empty body and the client
    // surfaces a unit `Ok(())`.
    let destroyed: Result<(), graftx_client::ClientError> =
        graftx_client::wgpu::destroy_buffer(&mut client, buffer, 4, 4);
    assert!(destroyed.is_ok());

    server_thread.join().expect("join server");
}
